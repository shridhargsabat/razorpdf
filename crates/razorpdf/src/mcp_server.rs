//! MCP JSON-RPC 2.0 server over stdio (newline-delimited messages).
//!
//! Speaks the MCP stdio transport: one JSON-RPC object per line on stdin,
//! one response line per request on stdout. Methods: `initialize`, `ping`,
//! `tools/list`, `tools/call` with tools `parse_pdf`, `inspect_pdf_layout`,
//! `extract_pdf_tables`, `get_pdf_toc`, `search_pdf_chunks`,
//! `read_pdf_page_range`, `render_pdf_region`, `extract_pdf_schema`,
//! `diff_pdfs`, and `extract_pdf_annotations_and_forms`. Tool failures return
//! `isError` results per MCP convention; malformed traffic gets JSON-RPC
//! protocol errors.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use anyhow::Result;
use lru::LruCache;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use razorpdf_core::{
    assembler, emitter, extractor, geometry, search, triage,
    vlm::{HttpVlmProvider, MockVlmProvider, NoVlmProvider},
    BBox, GroundedChunk, ParsedDocument, ParserConfig,
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
            },
            {
                "name": "get_pdf_toc",
                "description": "Document outline: page count, nested heading tree with page numbers and chunk ids, plus per-page complexity scores.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."}
                    }
                }
            },
            {
                "name": "search_pdf_chunks",
                "description": "BM25 + phrase-match retrieval over grounded chunks; returns chunk text with page numbers and bounding boxes.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "query"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."},
                        "query": {"type": "string", "description": "Search query (terms + exact-phrase boost)."},
                        "top_k": {"type": "integer", "description": "Max hits to return (default 5)."}
                    }
                }
            },
            {
                "name": "read_pdf_page_range",
                "description": "Read Markdown plus grounded chunks for pages start_page..=end_page (1-based, inclusive).",
                "inputSchema": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."},
                        "start_page": {"type": "integer", "description": "First page, 1-based (default 1)."},
                        "end_page": {"type": "integer", "description": "Last page, 1-based inclusive (default: last page)."}
                    }
                }
            },
            {
                "name": "render_pdf_region",
                "description": "Crop a page region to WebP (padded, optionally highlighted) and return it as an MCP image block.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "page", "bbox"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."},
                        "page": {"type": "integer", "description": "Page number, 1-based."},
                        "bbox": {"type": "array", "description": "Region [x0, y0, x1, y1] in PDF points (top-left origin)."},
                        "highlight": {"type": "boolean", "description": "Draw the citation highlight + border (default true)."}
                    }
                }
            },
            {
                "name": "extract_pdf_schema",
                "description": "Extract structured JSON matching a JSON Schema via AcroForm, inline/spatial key-value, and table-column matching, with per-field bbox provenance.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path", "schema"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."},
                        "schema": {"type": "object", "description": "JSON Schema object with properties (and optional required)."}
                    }
                }
            },
            {
                "name": "diff_pdfs",
                "description": "Compare two PDF revisions block-by-block (Unchanged/Modified/Added/Removed) with bounding boxes and a Markdown redline.",
                "inputSchema": {
                    "type": "object",
                    "required": ["old_path", "new_path"],
                    "properties": {
                        "old_path": {"type": "string", "description": "Absolute path to the old revision PDF."},
                        "new_path": {"type": "string", "description": "Absolute path to the new revision PDF."}
                    }
                }
            },
            {
                "name": "extract_pdf_annotations_and_forms",
                "description": "List hyperlink annotations (/URI), AcroForm field values, and linked footnotes with page numbers and bounding boxes.",
                "inputSchema": {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path to the input PDF."}
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
    if name == "render_pdf_region" {
        return match tool_render_pdf_region(&args).await {
            Ok(b64) => success_response(
                id,
                json!({"content": [{"type": "image", "data": b64, "mimeType": "image/webp"}]}),
            ),
            Err(text) => success_response(id, text_content(text, true)),
        };
    }
    let outcome = match name {
        "parse_pdf" => tool_parse_pdf(&args).await,
        "inspect_pdf_layout" => tool_inspect_pdf_layout(&args).await,
        "extract_pdf_tables" => tool_extract_pdf_tables(&args).await,
        "get_pdf_toc" => tool_get_pdf_toc(&args).await,
        "search_pdf_chunks" => tool_search_pdf_chunks(&args).await,
        "read_pdf_page_range" => tool_read_pdf_page_range(&args).await,
        "extract_pdf_schema" => tool_extract_pdf_schema(&args).await,
        "diff_pdfs" => tool_diff_pdfs(&args).await,
        "extract_pdf_annotations_and_forms" => tool_extract_pdf_annotations_and_forms(&args).await,
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

/// Parsed-document cache size (distinct PDFs held in memory).
const DOC_CACHE_CAP: usize = 16;

struct CachedDoc {
    mtime: SystemTime,
    doc: Arc<ParsedDocument>,
}

static DOC_CACHE: OnceLock<Mutex<LruCache<PathBuf, CachedDoc>>> = OnceLock::new();

fn doc_cache() -> &'static Mutex<LruCache<PathBuf, CachedDoc>> {
    DOC_CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(DOC_CACHE_CAP).expect("nonzero cache cap"),
        ))
    })
}

/// Parse once per (path, mtime); shared by the inspection tools.
///
/// Always assembles with [`NoVlmProvider`] (fast path + raw-text
/// fallbacks): deterministic, offline, and keyed purely by path+mtime.
/// `parse_pdf` keeps its own direct assembly (VLM options would poison
/// a shared key).
async fn cached_document(path: &str) -> Result<Arc<ParsedDocument>, String> {
    let fs_path = PathBuf::from(path);
    let mtime = std::fs::metadata(&fs_path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("stat {path}: {e}"))?;
    {
        let mut cache = doc_cache().lock().await;
        if let Some(hit) = cache.get(&fs_path) {
            if hit.mtime == mtime {
                return Ok(Arc::clone(&hit.doc));
            }
        }
    }
    let bytes = read_pdf(path)?;
    let config = ParserConfig::default();
    let doc = assembler::assemble_document(&bytes, path, &config, Arc::new(NoVlmProvider))
        .await
        .map_err(|e| e.to_string())?;
    let doc = Arc::new(doc);
    let _ = doc_cache().lock().await.put(
        fs_path,
        CachedDoc {
            mtime,
            doc: Arc::clone(&doc),
        },
    );
    Ok(doc)
}

/// BM25-ranked chunk indices (highest first) with an exact-phrase boost.
fn bm25_rank(chunks: &[GroundedChunk], query: &str, top_k: usize) -> Vec<(usize, f32)> {
    search::bm25_rank(chunks, query, top_k)
}

struct TocHeading {
    level: u8,
    title: String,
    page: usize,
    chunk_id: String,
}

/// Parse a `#`-style heading from chunk markdown (level clamped H1..H4).
fn parse_heading(markdown: &str) -> Option<(u8, String)> {
    let line = markdown.lines().next()?.trim();
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let title = line[hashes..].trim().to_string();
    if title.is_empty() {
        return None;
    }
    Some((hashes.min(4) as u8, title))
}

/// Nest a flat heading list into a children-tree outline.
fn nest_headings(headings: Vec<TocHeading>) -> Vec<Value> {
    let mut root: Vec<Value> = Vec::new();
    let mut stack: Vec<(u8, Vec<usize>)> = Vec::new();
    for h in &headings {
        while stack.last().is_some_and(|(level, _)| *level >= h.level) {
            stack.pop();
        }
        let item = json!({
            "level": h.level,
            "title": h.title,
            "page": h.page,
            "chunk_id": h.chunk_id,
            "children": [],
        });
        match stack.last() {
            Some((_, path)) => {
                let mut target = &mut root;
                for &i in path {
                    target = target[i]["children"]
                        .as_array_mut()
                        .expect("children array");
                }
                target.push(item);
                let mut new_path = path.clone();
                new_path.push(target.len() - 1);
                stack.push((h.level, new_path));
            }
            None => {
                root.push(item);
                stack.push((h.level, vec![root.len() - 1]));
            }
        }
    }
    root
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
        let lines =
            geometry::merge_spans_to_lines_with_threshold(&page.spans, config.line_merge_overlap)
                .map_err(|e| e.to_string())?;
        let median_font = geometry::body_font_size(&lines);
        let tables = geometry::detect_all_tables(
            &page.vectors,
            &page.spans,
            &lines,
            median_font,
            &page.filled_rects,
        );
        let verdict = triage::score_page(page, &config).map_err(|e| e.to_string())?;
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
        let lines =
            geometry::merge_spans_to_lines_with_threshold(&page.spans, config.line_merge_overlap)
                .map_err(|e| e.to_string())?;
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
        let verdict = triage::score_page(page, &config).map_err(|e| e.to_string())?;
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
                    // Currently unreachable (canvas is Some whenever a mock
                    // table region exists), but if that invariant ever
                    // drifts, keep the array shape with an explicit
                    // unresolved marker instead of silently dropping the entry.
                    tables_out.push(json!({
                        "page_index": page.page_index,
                        "bbox": region.content_bbox,
                        "source": "unresolved",
                        "rows": Value::Null,
                        "has_header": Value::Null,
                        "markdown": Value::Null,
                        "raw_text": region_raw_text(page, region.content_bbox),
                    }));
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
                        reason: region.reason,
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

async fn tool_get_pdf_toc(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let doc = cached_document(&path).await?;
    let headings: Vec<TocHeading> = doc
        .chunks
        .iter()
        .filter_map(|c| {
            parse_heading(&c.markdown).map(|(level, title)| TocHeading {
                level,
                title,
                page: c.page_index + 1,
                chunk_id: c.chunk_id.clone(),
            })
        })
        .collect();
    let toc = nest_headings(headings);
    let bytes = read_pdf(&path)?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, &path).map_err(|e| e.to_string())?;
    let mut page_scores = Vec::new();
    for page in &pages {
        let verdict = triage::score_page(page, &config).map_err(|e| e.to_string())?;
        page_scores.push(json!({
            "page": page.page_index + 1,
            "score": verdict.score,
            "verdict": if verdict.fallback_regions.is_empty() { "fast" } else { "vlm" },
        }));
    }
    serde_json::to_string_pretty(&json!({
        "source_path": doc.source_path.clone(),
        "total_pages": doc.total_pages,
        "fast_path_pages": doc.fast_path_pages,
        "vlm_fallback_regions": doc.vlm_fallback_regions,
        "elapsed_ms": doc.elapsed_ms,
        "toc": toc,
        "page_scores": page_scores,
    }))
    .map_err(|e| e.to_string())
}

async fn tool_search_pdf_chunks(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if query.trim().is_empty() {
        return Err("missing required argument: query".to_string());
    }
    let top_k = args
        .get("top_k")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .clamp(1, 100) as usize;
    let doc = cached_document(&path).await?;
    let hits: Vec<Value> = bm25_rank(&doc.chunks, &query, top_k)
        .into_iter()
        .map(|(i, score)| {
            let chunk = &doc.chunks[i];
            json!({
                "chunk_id": chunk.chunk_id,
                "page": chunk.page_index + 1,
                "bbox": chunk.bbox,
                "section_path": chunk.breadcrumb,
                "score": score,
                "markdown": chunk.markdown,
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "source_path": doc.source_path.clone(),
        "query": query,
        "top_k": top_k,
        "hits": hits,
    }))
    .map_err(|e| e.to_string())
}

async fn tool_read_pdf_page_range(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let doc = cached_document(&path).await?;
    let total = doc.total_pages;
    let start = args
        .get("start_page")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end = args
        .get("end_page")
        .and_then(Value::as_u64)
        .unwrap_or(total as u64)
        .max(1) as usize;
    if start > end {
        return Err(format!("start_page {start} exceeds end_page {end}"));
    }
    if start > total {
        return Err(format!("start_page {start} out of range (1..={total})"));
    }
    let end = end.min(total);
    let (lo, hi) = (start - 1, end);
    let chunks: Vec<&GroundedChunk> = doc
        .chunks
        .iter()
        .filter(|c| c.page_index >= lo && c.page_index < hi)
        .collect();
    let markdown = chunks
        .iter()
        .map(|c| c.markdown.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::to_string_pretty(&json!({
        "source_path": doc.source_path.clone(),
        "start_page": start,
        "end_page": end,
        "total_pages": total,
        "markdown": markdown,
        "chunks": chunks,
    }))
    .map_err(|e| e.to_string())
}

fn parse_bbox_arg(value: Option<&Value>) -> Result<BBox, String> {
    let nums = value
        .and_then(Value::as_array)
        .ok_or_else(|| "missing required argument: bbox [x0, y0, x1, y1]".to_string())?;
    if nums.len() != 4 {
        return Err("bbox must be [x0, y0, x1, y1]".to_string());
    }
    let mut coords = [0.0f32; 4];
    for (i, v) in nums.iter().enumerate() {
        coords[i] =
            v.as_f64()
                .ok_or_else(|| "bbox coordinates must be numbers".to_string())? as f32;
    }
    if coords[0] >= coords[2] || coords[1] >= coords[3] {
        return Err("bbox must satisfy x0 < x1 and y0 < y1".to_string());
    }
    Ok(BBox::new(coords[0], coords[1], coords[2], coords[3]))
}

/// Padded context (points) around render_pdf_region crops.
const RENDER_PADDING_PT: f32 = 20.0;

async fn tool_render_pdf_region(args: &Value) -> Result<String, String> {
    use base64::Engine as _;
    use razorpdf_core::vlm::cropper;

    let path = arg_path(args)?;
    let page_no = args.get("page").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let bbox = parse_bbox_arg(args.get("bbox"))?;
    let highlight = args
        .get("highlight")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let bytes = read_pdf(&path)?;
    let config = ParserConfig::default();
    let pages = extractor::extract_document(&bytes, &path).map_err(|e| e.to_string())?;
    let page = pages
        .get(page_no - 1)
        .ok_or_else(|| format!("page {page_no} out of range (1..={})", pages.len()))?;
    let canvas = cropper::render_synthetic_canvas(page, config.vlm.render_dpi);
    let webp = if highlight {
        cropper::render_citation_overlay(
            &canvas,
            page.width,
            page.height,
            &bbox,
            RENDER_PADDING_PT,
            config.vlm.webp_quality,
        )
    } else {
        cropper::crop_region_to_webp(
            &canvas,
            &bbox,
            page.width,
            page.height,
            config.vlm.webp_quality,
        )
    }
    .map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&webp))
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

async fn tool_extract_pdf_schema(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let schema = args
        .get("schema")
        .filter(|s| s.is_object())
        .ok_or_else(|| "missing required argument: schema (JSON object)".to_string())?;
    let doc = cached_document(&path).await?;
    let result = extractor::extract_with_schema(&doc, schema).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

async fn tool_diff_pdfs(args: &Value) -> Result<String, String> {
    let old_path = args
        .get("old_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required argument: old_path".to_string())?;
    let new_path = args
        .get("new_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required argument: new_path".to_string())?;
    let old_doc = cached_document(old_path).await?;
    let new_doc = cached_document(new_path).await?;
    let diff = emitter::diff_documents(&old_doc, &new_doc);
    let markdown_diff = emitter::to_markdown_diff(&diff);
    let mut value = serde_json::to_value(&diff).map_err(|e| e.to_string())?;
    value["markdown_diff"] = Value::String(markdown_diff);
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

async fn tool_extract_pdf_annotations_and_forms(args: &Value) -> Result<String, String> {
    let path = arg_path(args)?;
    let doc = cached_document(&path).await?;
    // Links live on raw pages (assembler only borrows them for anchors).
    let bytes = read_pdf(&path)?;
    let pages = extractor::extract_document(&bytes, &path).map_err(|e| e.to_string())?;
    let mut links = Vec::new();
    for page in &pages {
        for link in &page.link_annotations {
            links.push(json!({
                "page": page.page_index + 1,
                "bbox": link.bbox,
                "uri": link.uri,
            }));
        }
    }
    serde_json::to_string_pretty(&json!({
        "source_path": doc.source_path.clone(),
        "links": links,
        "form_fields": doc.form_fields,
        "footnotes": doc.footnotes,
    }))
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture_file(tag: &str, bytes: Vec<u8>) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("razorpdf-mcp-{tag}-{}-{n}.pdf", std::process::id()));
        std::fs::write(&path, bytes).expect("fixture write");
        path.to_string_lossy().into_owned()
    }

    async fn call_tool(name: &str, args: Value) -> Value {
        tools_call(json!(1), Some(&json!({"name": name, "arguments": args}))).await
    }

    fn result_text(v: &Value) -> &str {
        v.get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|b| b.get("text"))
            .and_then(Value::as_str)
            .expect("text content")
    }

    fn is_error(v: &Value) -> bool {
        v.get("result")
            .and_then(|r| r.get("isError"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    #[test]
    fn list_advertises_ten_tools() {
        let list = tools_list();
        let names: Vec<&str> = list
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array")
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names.len(), 10);
        for expected in [
            "parse_pdf",
            "inspect_pdf_layout",
            "extract_pdf_tables",
            "get_pdf_toc",
            "search_pdf_chunks",
            "read_pdf_page_range",
            "render_pdf_region",
            "extract_pdf_schema",
            "diff_pdfs",
            "extract_pdf_annotations_and_forms",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }

    #[tokio::test]
    async fn parse_pdf_roundtrip() {
        let path = fixture_file(
            "parse",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let out = call_tool("parse_pdf", json!({"path": path})).await;
        assert!(!is_error(&out));
        assert!(result_text(&out).contains("RazorPDF Benchmark Document"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn inspect_layout_reports_pages() {
        let path = fixture_file(
            "inspect",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let out = call_tool("inspect_pdf_layout", json!({"path": path})).await;
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert_eq!(parsed["pages"][0]["spans"].as_u64().unwrap(), 17);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn extract_tables_finds_grid() {
        let path = fixture_file("tables", razorpdf_core::test_fixtures::bordered_table_pdf());
        let out = call_tool("extract_pdf_tables", json!({"path": path})).await;
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert!(!parsed["tables"].as_array().expect("array").is_empty());
        assert!(parsed["tables"][0].get("rows").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn toc_lists_headings_and_scores() {
        let path = fixture_file("toc", razorpdf_core::test_fixtures::single_col_clean_pdf());
        let out = call_tool("get_pdf_toc", json!({"path": path})).await;
        assert!(!is_error(&out), "{}", result_text(&out));
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert_eq!(parsed["total_pages"], 1);
        assert!(parsed["toc"][0]["title"]
            .as_str()
            .unwrap()
            .contains("RazorPDF Benchmark"));
        assert_eq!(parsed["toc"][0]["level"], 1);
        assert_eq!(parsed["toc"][0]["page"], 1);
        assert!(!parsed["toc"][0]["chunk_id"].as_str().unwrap().is_empty());
        assert_eq!(parsed["page_scores"].as_array().expect("scores").len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn search_finds_term_with_bbox() {
        let path = fixture_file(
            "search",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let out = call_tool(
            "search_pdf_chunks",
            json!({"path": path, "query": "Benchmark", "top_k": 3}),
        )
        .await;
        assert!(!is_error(&out), "{}", result_text(&out));
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        let hits = parsed["hits"].as_array().expect("hits");
        assert!(!hits.is_empty());
        assert!(hits[0]["markdown"].as_str().unwrap().contains("Benchmark"));
        assert!(hits[0].get("bbox").is_some());
        assert_eq!(hits[0]["page"], 1);
        assert!(hits.len() <= 3);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn page_range_slices_single_page() {
        let path = fixture_file("range", razorpdf_core::test_fixtures::split_paragraph_pdf());
        let out = call_tool(
            "read_pdf_page_range",
            json!({"path": path, "start_page": 1, "end_page": 1}),
        )
        .await;
        assert!(!is_error(&out), "{}", result_text(&out));
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert_eq!(parsed["total_pages"], 2);
        for chunk in parsed["chunks"].as_array().expect("chunks") {
            assert_eq!(chunk["page_index"], 0);
        }
        assert!(!parsed["markdown"].as_str().unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn render_region_returns_webp_image() {
        let path = fixture_file("render", razorpdf_core::test_fixtures::bordered_table_pdf());
        let out = call_tool(
            "render_pdf_region",
            json!({"path": path, "page": 1, "bbox": [0.0, 0.0, 612.0, 792.0], "highlight": true}),
        )
        .await;
        let block = out
            .get("result")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get(0))
            .expect("content");
        assert_eq!(block["type"], "image");
        assert_eq!(block["mimeType"], "image/webp");
        assert!(block["data"].as_str().unwrap().starts_with("UklGR")); // base64 RIFF
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let out = call_tool("nope", json!({})).await;
        assert_eq!(out["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn missing_path_is_error() {
        let out = call_tool("parse_pdf", json!({})).await;
        assert!(is_error(&out));
    }

    #[tokio::test]
    async fn blank_query_is_error() {
        let path = fixture_file("q", razorpdf_core::test_fixtures::single_col_clean_pdf());
        let out = call_tool("search_pdf_chunks", json!({"path": path, "query": "  "})).await;
        assert!(is_error(&out));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn schema_tool_returns_structured_result() {
        let path = fixture_file(
            "schema",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let schema = json!({
            "type": "object",
            "properties": {"title": {"type": "string", "title": "Document Title"}}
        });
        let out = call_tool(
            "extract_pdf_schema",
            json!({"path": path, "schema": schema}),
        )
        .await;
        assert!(!is_error(&out), "{}", result_text(&out));
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert!(parsed.get("data").is_some());
        assert!(parsed.get("provenance").is_some());
        assert!(parsed.get("overall_confidence").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn schema_tool_requires_schema_arg() {
        let path = fixture_file(
            "schema-err",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let out = call_tool("extract_pdf_schema", json!({"path": path})).await;
        assert!(is_error(&out));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn diff_tool_compares_revisions() {
        let old = fixture_file(
            "diff-old",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let new = fixture_file(
            "diff-new",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let out = call_tool("diff_pdfs", json!({"old_path": old, "new_path": new})).await;
        assert!(!is_error(&out), "{}", result_text(&out));
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert!(parsed["unchanged"].as_u64().unwrap() > 0);
        assert_eq!(parsed["modified"], 0);
        assert_eq!(parsed["added"], 0);
        assert_eq!(parsed["removed"], 0);
        assert!(parsed.get("markdown_diff").is_some());
        let _ = std::fs::remove_file(parsed["old_path"].as_str().unwrap());
        let _ = std::fs::remove_file(parsed["new_path"].as_str().unwrap());
    }

    #[tokio::test]
    async fn annotations_tool_lists_links_forms_footnotes() {
        let path = fixture_file(
            "annots",
            razorpdf_core::test_fixtures::single_col_clean_pdf(),
        );
        let out = call_tool("extract_pdf_annotations_and_forms", json!({"path": path})).await;
        assert!(!is_error(&out), "{}", result_text(&out));
        let parsed: Value = serde_json::from_str(result_text(&out)).expect("json");
        assert!(parsed.get("links").is_some());
        assert!(parsed.get("form_fields").is_some());
        assert!(parsed.get("footnotes").is_some());
        let _ = std::fs::remove_file(&path);
    }
}
