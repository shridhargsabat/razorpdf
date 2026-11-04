//! Wire-level MCP tests: every tool driven over real stdio JSON-RPC.
//! Expected values mirror the in-process unit tests in `mcp_server.rs`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Live MCP child with line-oriented JSON-RPC; kills the child on drop.
struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_razorpdf"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn razorpdf mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .expect("write request");
        self.stdin.flush().expect("flush");
        let mut out = String::new();
        self.stdout.read_line(&mut out).expect("read response");
        let response: serde_json::Value = serde_json::from_str(&out).expect("response is JSON");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id);
        response
    }

    fn notify(&mut self, method: &str) {
        let line = serde_json::json!({"jsonrpc": "2.0", "method": method});
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .expect("write notification");
        self.stdin.flush().expect("flush");
    }

    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        )
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write `bytes` to a uniquely-tagged temp PDF; returns its path.
/// Tags must be unique per test (tests run in parallel).
fn fixture_pdf(tag: &str, bytes: Vec<u8>) -> String {
    let dir = std::env::temp_dir().join("razorpdf-mcp-wire");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{tag}.pdf"));
    std::fs::write(&path, bytes).expect("write fixture");
    path.to_string_lossy().into_owned()
}

/// Tool-result text payload (MCP `content[0].text`).
fn tool_text(response: &serde_json::Value) -> &str {
    assert!(
        response.get("error").is_none(),
        "unexpected protocol error: {response}"
    );
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
}

/// First content block (for non-text results like images).
fn first_block(response: &serde_json::Value) -> &serde_json::Value {
    assert!(
        response.get("error").is_none(),
        "unexpected protocol error: {response}"
    );
    &response["result"]["content"][0]
}

/// MCP `isError` tool-results flag (distinct from JSON-RPC `error`).
fn is_tool_error(response: &serde_json::Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
}

#[test]
fn wire_toc_lists_headings_and_scores() {
    let path = fixture_pdf("wire-toc", razorpdf_core::test_fixtures::single_col_clean_pdf());
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool("get_pdf_toc", serde_json::json!({"path": path}));
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    assert_eq!(parsed["total_pages"], 1);
    assert!(parsed["toc"][0]["title"]
        .as_str()
        .unwrap()
        .contains("RazorPDF Benchmark"));
    assert_eq!(parsed["toc"][0]["level"], 1);
    assert_eq!(parsed["toc"][0]["page"], 1);
    assert!(!parsed["toc"][0]["chunk_id"].as_str().unwrap().is_empty());
    assert_eq!(parsed["page_scores"].as_array().expect("scores").len(), 1);
}

#[test]
fn wire_search_finds_term_with_bbox() {
    let path = fixture_pdf(
        "wire-search",
        razorpdf_core::test_fixtures::single_col_clean_pdf(),
    );
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool(
        "search_pdf_chunks",
        serde_json::json!({"path": path, "query": "Benchmark", "top_k": 3}),
    );
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    let hits = parsed["hits"].as_array().expect("hits");
    assert!(!hits.is_empty());
    assert!(hits[0]["markdown"].as_str().unwrap().contains("Benchmark"));
    assert!(hits[0].get("bbox").is_some());
    assert_eq!(hits[0]["page"], 1);
    assert!(hits.len() <= 3);

    // Blank queries are tool errors, not crashes or empty successes.
    let blank = mcp.call_tool(
        "search_pdf_chunks",
        serde_json::json!({"path": path, "query": "  "}),
    );
    assert!(is_tool_error(&blank));
}

#[test]
fn wire_page_range_slices_first_page() {
    let path = fixture_pdf("wire-range", razorpdf_core::test_fixtures::split_paragraph_pdf());
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool(
        "read_pdf_page_range",
        serde_json::json!({"path": path, "start_page": 1, "end_page": 1}),
    );
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    assert_eq!(parsed["total_pages"], 2);
    let chunks = parsed["chunks"].as_array().expect("chunks");
    assert!(!chunks.is_empty());
    for chunk in chunks {
        assert_eq!(chunk["page_index"], 0);
    }
    assert!(!parsed["markdown"].as_str().unwrap().is_empty());
}

#[test]
fn wire_render_region_returns_webp_image() {
    let path = fixture_pdf("wire-render", razorpdf_core::test_fixtures::bordered_table_pdf());
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool(
        "render_pdf_region",
        serde_json::json!({"path": path, "page": 1, "bbox": [0.0, 0.0, 612.0, 792.0], "highlight": true}),
    );
    let block = first_block(&out);
    assert_eq!(block["type"], "image");
    assert_eq!(block["mimeType"], "image/webp");
    assert!(block["data"].as_str().unwrap().starts_with("UklGR")); // base64 RIFF
}

#[test]
fn wire_schema_extraction_returns_data_and_provenance() {
    let path = fixture_pdf(
        "wire-schema",
        razorpdf_core::test_fixtures::single_col_clean_pdf(),
    );
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"title": {"type": "string", "title": "Document Title"}}
    });
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool(
        "extract_pdf_schema",
        serde_json::json!({"path": path, "schema": schema}),
    );
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    assert!(parsed.get("data").is_some());
    assert!(parsed.get("provenance").is_some());
    assert!(parsed.get("overall_confidence").is_some());

    // Missing `schema` arg is a tool error, not a crash.
    let missing = mcp.call_tool("extract_pdf_schema", serde_json::json!({"path": path}));
    assert!(is_tool_error(&missing));
}

#[test]
fn wire_diff_identical_pdfs_all_unchanged() {
    let old = fixture_pdf(
        "wire-diff-old",
        razorpdf_core::test_fixtures::single_col_clean_pdf(),
    );
    let new = fixture_pdf(
        "wire-diff-new",
        razorpdf_core::test_fixtures::single_col_clean_pdf(),
    );
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool("diff_pdfs", serde_json::json!({"old_path": old, "new_path": new}));
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    assert!(parsed["unchanged"].as_u64().unwrap() > 0);
    assert_eq!(parsed["modified"], 0);
    assert_eq!(parsed["added"], 0);
    assert_eq!(parsed["removed"], 0);
    assert!(parsed.get("markdown_diff").is_some());
    assert_eq!(parsed["old_path"], old);
    assert_eq!(parsed["new_path"], new);
}

#[test]
fn wire_annotations_lists_links_forms_footnotes() {
    let path = fixture_pdf(
        "wire-annots",
        razorpdf_core::test_fixtures::single_col_clean_pdf(),
    );
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool(
        "extract_pdf_annotations_and_forms",
        serde_json::json!({"path": path}),
    );
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    assert!(parsed.get("links").is_some());
    assert!(parsed.get("form_fields").is_some());
    assert!(parsed.get("footnotes").is_some());
}

#[test]
fn wire_tools_list_carries_complete_annotations() {
    let mut mcp = McpClient::spawn();
    let listed = mcp.request("tools/list", serde_json::json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    // Non-vacuous: the loop below must cover all ten tools.
    assert_eq!(tools.len(), 10);
    for tool in tools {
        let name = tool["name"].as_str().expect("tool name");
        let ann = tool.get("annotations").unwrap_or_else(|| panic!("{name} advertises no annotations"));
        for hint in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
            assert!(
                ann.get(hint).and_then(serde_json::Value::as_bool).is_some(),
                "{name} missing explicit boolean {hint}"
            );
        }
        assert_eq!(ann["readOnlyHint"], true, "{name} must be read-only");
        assert_eq!(ann["destructiveHint"], false, "{name} must be non-destructive");
        assert_eq!(ann["idempotentHint"], true, "{name} must be idempotent");
        assert_eq!(ann["openWorldHint"], name == "parse_pdf", "{name}");
    }
}

#[test]
fn wire_ping_and_notification_keep_session_alive() {
    let mut mcp = McpClient::spawn();
    let pong = mcp.request("ping", serde_json::json!({}));
    assert_eq!(pong["result"], serde_json::json!({}));
    // Notifications get no reply; the next request must still work.
    mcp.notify("notifications/initialized");
    let listed = mcp.request("tools/list", serde_json::json!({}));
    assert_eq!(listed["result"]["tools"].as_array().expect("tools").len(), 10);
}

#[test]
fn wire_parse_real_invoice_sample() {
    let invoice = format!(
        "{}/../../samples/invoice.pdf",
        env!("CARGO_MANIFEST_DIR")
    );
    assert!(std::path::Path::new(&invoice).exists(), "missing {invoice}");
    let mut mcp = McpClient::spawn();
    let out = mcp.call_tool("parse_pdf", serde_json::json!({"path": invoice}));
    assert!(!is_tool_error(&out), "{}", tool_text(&out));
    let parsed: serde_json::Value = serde_json::from_str(tool_text(&out)).expect("json");
    assert_eq!(parsed["total_pages"], 1);
    assert_eq!(parsed["fast_path_pages"], 1);
    assert_eq!(parsed["chunks"].as_array().expect("chunks").len(), 6);
    assert!(parsed["markdown"].as_str().unwrap().contains("TAX INVOICE"));
}
