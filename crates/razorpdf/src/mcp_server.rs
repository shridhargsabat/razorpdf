//! MCP JSON-RPC 2.0 server over stdio (newline-delimited messages).
//!
//! Speaks the MCP stdio transport: one JSON-RPC object per line on stdin,
//! one response line per request on stdout. Methods: `initialize`, `ping`,
//! `tools/list`, `tools/call` with tools `parse_pdf`, `inspect_pdf_layout`,
//! and `extract_pdf_tables`. Tool failures return `isError` results per MCP
//! convention; malformed traffic gets JSON-RPC protocol errors.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use razorpdf_core::{
    assembler, extractor, geometry, triage,
    vlm::{HttpVlmProvider, MockVlmProvider, NoVlmProvider},
    ParserConfig,
};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "razorpdf";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Start serving MCP tools over stdin/stdout; returns on EOF.
pub async fn run() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_message(&line).await {
            let mut out = serde_json::to_string(&response)?;
            out.push('\n');
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

/// Handle one raw line; `None` for notifications (no response).
async fn handle_message(line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Some(error_response(Value::Null, -32700, "Parse error")),
    };
    // JSON-RPC notification (no id): acknowledge silently.
    msg.get("id")?;
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    if msg.get("jsonrpc") != Some(&json!("2.0")) || method.is_empty() {
        return Some(error_response(id, -32600, "Invalid Request"));
    }
    match method {
        "initialize" => Some(success_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            }),
        )),
        "ping" => Some(success_response(id, json!({}))),
        "tools/list" => Some(success_response(id, tools_list())),
        "tools/call" => Some(tools_call(id, msg.get("params")).await),
        _ => Some(error_response(id, -32601, "Method not found")),
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// MCP `content` wrapper for tool results.
fn text_content(text: String, is_error: bool) -> Value {
    if is_error {
        json!({"content": [{"type": "text", "text": text}], "isError": true})
    } else {
        json!({"content": [{"type": "text", "text": text}]})
    }
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "parse_pdf",
                "description": "Parse a PDF into Markdown plus grounded RAG chunks with page bounding boxes.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."},
                        "vlm_url": {"type": "string", "description": "OpenAI-compatible vision endpoint for messy regions (optional)."},
                        "mock_vlm": {"type": "boolean", "description": "Resolve VLM regions with the deterministic offline mock (default false)."}
                    }
                }
            },
            {
                "name": "inspect_pdf_layout",
                "description": "Report per-page layout stats, complexity scores, and triage regions without full parsing.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."}
                    }
                }
            },
            {
                "name": "extract_pdf_tables",
                "description": "Extract tables as structured rows: deterministic vector grids plus VLM-resolved (or raw-text) borderless regions.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."},
                        "mock_vlm": {"type": "boolean", "description": "Resolve borderless regions with the offline mock (default false)."}
                    }
                }
            }
        ]
    })
}

async fn tools_call(id: Value, params: Option<&Value>) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or(Value::Null);
    let outcome = match name {
        "parse_pdf" => tool_parse_pdf(&args).await,
        "inspect_pdf_layout" => tool_inspect_pdf_layout(&args).await,
        "extract_pdf_tables" => tool_extract_pdf_tables(&args).await,
        _ => return error_response(id, -32602, "Unknown tool"),
    };
    match outcome {
        Ok(text) => success_response(id, text_content(text, false)),
        Err(text) => success_response(id, text_content(text, true)),
    }
}

fn arg_path(args: &Value) -> Result<String, String> {
    args.get("path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "missing required argument: path".to_string())
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn read_pdf(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("read {path}: {e}"))
}

async fn tool_parse_pdf(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let bytes = read_pdf(&path)?;
    let mut config = ParserConfig::default();
    if let Some(url) = args.get("vlm_url").and_then(Value::as_str) {
        config.vlm.url = Some(url.to_string());
    }
    let doc = if arg_bool(args, "mock_vlm") {
        assembler::assemble_document(&bytes, &path, &config, Arc::new(MockVlmProvider::new()))
            .await
            .map_err(|e| e.to_string())?
    } else if config.vlm.url.is_some() || std::env::var("RAZORPDF_VLM_URL").is_ok() {
        let http = HttpVlmProvider::new(config.vlm.clone()).map_err(|e| e.to_string())?;
        assembler::assemble_document(&bytes, &path, &config, Arc::new(http))
            .await
            .map_err(|e| e.to_string())?
    } else {
        assembler::assemble_document(&bytes, &path, &config, Arc::new(NoVlmProvider))
            .await
            .map_err(|e| e.to_string())?
    };
    serde_json::to_string_pretty(&json!({
        "source_path": doc.source_path,
        "total_pages": doc.total_pages,
        "fast_path_pages": doc.fast_path_pages,
        "vlm_fallback_regions": doc.vlm_fallback_regions,
        "elapsed_ms": doc.elapsed_ms,
        "markdown": doc.markdown,
        "chunks": doc.chunks,
    }))
    .map_err(|e| e.to_string())
}

async fn tool_inspect_pdf_layout(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let bytes = read_pdf(&path)?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, &path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for page in &pages {
        let lines = geometry::merge_spans_to_lines(&page.spans);
        let median_font = geometry::body_font_size(&lines);
        let tables = geometry::detect_all_tables(
            &page.vectors,
            &page.spans,
            &lines,
            median_font,
            &page.filled_rects,
        );
        let verdict = triage::score_page(page, &config);
        out.push(json!({
            "page_index": page.page_index,
            "width": page.width,
            "height": page.height,
            "spans": page.spans.len(),
            "vectors": page.vectors.len(),
            "lines": lines.len(),
            "tables": tables.len(),
            "median_font_size": geometry::median_font_size(&lines),
            "complexity_score": verdict.score,
            "verdict": if verdict.fallback_regions.is_empty() { "fast" } else { "vlm" },
            "fallback_regions": verdict.fallback_regions.iter().map(|r| json!({
                "reason": format!("{:?}", r.reason),
                "score": r.score,
                "bbox": r.content_bbox,
            })).collect::<Vec<_>>(),
        }));
    }
    serde_json::to_string_pretty(&json!({"source_path": path, "pages": out}))
        .map_err(|e| e.to_string())
}

async fn tool_extract_pdf_tables(args: &Value) -> Result<String, String> {
    use razorpdf_core::vlm::{cropper, VlmProvider, VlmRequest};

    let path = arg_path(args)?;
    let bytes = read_pdf(&path)?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, &path).map_err(|e| e.to_string())?;
    let use_mock = arg_bool(args, "mock_vlm");
    let mock = MockVlmProvider::new();
    let mut tables_out = Vec::new();

    for page in &pages {
        let lines = geometry::merge_spans_to_lines(&page.spans);
        let median_font = geometry::body_font_size(&lines);
        for table in geometry::detect_all_tables(
            &page.vectors,
            &page.spans,
            &lines,
            median_font,
            &page.filled_rects,
        ) {
            tables_out.push(json!({
                "page_index": page.page_index,
                "bbox": table.bbox,
                "source": "deterministic",
                "method": table.source.as_str(),
                "rows": table.rows,
                "has_header": table.has_header,
                "markdown": geometry::table_grid::detected_table_to_markdown(&table),
            }));
        }
        let verdict = triage::score_page(page, &config);
        let needs_canvas = use_mock
            && verdict.fallback_regions.iter().any(|r| {
                matches!(
                    r.reason,
                    razorpdf_core::TriageReason::BorderlessIrregularTable
                        | razorpdf_core::TriageReason::OverlappingSpans
                )
            });
        let mut canvas = None;
        if needs_canvas {
            canvas = Some(cropper::render_synthetic_canvas(
                page,
                config.vlm.render_dpi,
            ));
        }
        for region in &verdict.fallback_regions {
            let is_table = matches!(
                region.reason,
                razorpdf_core::TriageReason::BorderlessIrregularTable
                    | razorpdf_core::TriageReason::OverlappingSpans
            );
            if !is_table {
                continue;
            }
            if use_mock {
                // Defensive: canvas is Some whenever needs_canvas held, but a
                // mock region must never panic the server if that drifts.
                let Some(canvas) = canvas.as_ref() else {
                    continue;
                };
                let uri = cropper::crop_region_to_data_uri(
                    canvas,
                    &region.bbox,
                    page.width,
                    page.height,
                    config.vlm.webp_quality,
                )
                .map_err(|e| e.to_string())?;
                let response = mock
                    .complete_region(&VlmRequest {
                        image_data_uri: uri,
                        page_index: page.page_index,
                        region: region.content_bbox,
                        reason: region.reason.clone(),
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                tables_out.push(json!({
                    "page_index": page.page_index,
                    "bbox": region.content_bbox,
                    "source": "vlm",
                    "rows": Value::Null,
                    "has_header": Value::Null,
                    "markdown": response.markdown,
                }));
            } else {
                let raw = region_raw_text(page, region.content_bbox);
                tables_out.push(json!({
                    "page_index": page.page_index,
                    "bbox": region.content_bbox,
                    "source": "unresolved",
                    "rows": Value::Null,
                    "has_header": Value::Null,
                    "markdown": Value::Null,
                    "raw_text": raw,
                }));
            }
        }
    }
    serde_json::to_string_pretty(&json!({"source_path": path, "tables": tables_out}))
        .map_err(|e| e.to_string())
}

/// Raw span texts inside a bbox, in reading-ish order.
fn region_raw_text(page: &razorpdf_core::RawPageElements, content: razorpdf_core::BBox) -> String {
    let mut hits: Vec<&razorpdf_core::TextSpan> = page
        .spans
        .iter()
        .filter(|s| {
            let (cx, cy) = s.bbox.centroid();
            content.contains_point(cx, cy)
        })
        .collect();
    hits.sort_by(|a, b| {
        a.bbox
            .y0
            .partial_cmp(&b.bbox.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.bbox
                    .x0
                    .partial_cmp(&b.bbox.x0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    hits.iter()
        .map(|s| s.text.trim())
        .collect::<Vec<_>>()
        .join(" ")
}
