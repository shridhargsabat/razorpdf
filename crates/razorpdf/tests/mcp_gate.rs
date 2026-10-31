//! Phase 5 verification gate: spawn `razorpdf mcp`, drive JSON-RPC, assert.

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
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture_pdf(name: &str) -> String {
    let bytes = match name {
        "single" => razorpdf_core::test_fixtures::single_col_clean_pdf(),
        "bordered" => razorpdf_core::test_fixtures::bordered_table_pdf(),
        "messy" => razorpdf_core::test_fixtures::messy_overlap_table_pdf(),
        _ => panic!("unknown fixture {name}"),
    };
    let dir = std::env::temp_dir().join("razorpdf-mcp-gate");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{name}.pdf"));
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

/// Gate 5: full MCP round-trip over stdio.
#[test]
fn gate5_mcp_parse_pdf_roundtrip() {
    let single = fixture_pdf("single");
    let mut mcp = McpClient::spawn();

    let init = mcp.request(
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "gate5", "version": "0.0.0"},
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "razorpdf");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    mcp.notify("notifications/initialized");

    let listed = mcp.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();
    assert!(names.contains(&"parse_pdf"));
    assert!(names.contains(&"inspect_pdf_layout"));
    assert!(names.contains(&"extract_pdf_tables"));

    // parse_pdf returns structured Markdown + chunks.
    let parsed = mcp.request(
        "tools/call",
        serde_json::json!({"name": "parse_pdf", "arguments": {"path": single}}),
    );
    let doc: serde_json::Value =
        serde_json::from_str(tool_text(&parsed)).expect("parse_pdf JSON payload");
    assert_eq!(doc["total_pages"], 1);
    assert_eq!(doc["fast_path_pages"], 1);
    assert_eq!(doc["vlm_fallback_regions"], 0);
    assert_eq!(doc["chunks"].as_array().expect("chunks").len(), 17);
    assert!(doc["markdown"]
        .as_str()
        .expect("markdown")
        .contains("# RazorPDF Benchmark Document"));

    // inspect_pdf_layout reports real per-page stats.
    let inspected = mcp.request(
        "tools/call",
        serde_json::json!({"name": "inspect_pdf_layout", "arguments": {"path": single}}),
    );
    let layout: serde_json::Value =
        serde_json::from_str(tool_text(&inspected)).expect("inspect JSON payload");
    assert_eq!(layout["pages"][0]["spans"], 17);
    assert_eq!(layout["pages"][0]["verdict"], "fast");

    // extract_pdf_tables returns the deterministic grid.
    let bordered = fixture_pdf("bordered");
    let tables = mcp.request(
        "tools/call",
        serde_json::json!({"name": "extract_pdf_tables", "arguments": {"path": bordered}}),
    );
    let extracted: serde_json::Value =
        serde_json::from_str(tool_text(&tables)).expect("tables JSON payload");
    let tables = extracted["tables"].as_array().expect("tables array");
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0]["source"], "deterministic");
    assert_eq!(
        tables[0]["rows"][0],
        serde_json::json!(["Name", "Qty", "Price"])
    );

    // Messy tables resolve through the mock provider on request.
    let messy = fixture_pdf("messy");
    let mock_tables = mcp.request(
        "tools/call",
        serde_json::json!({"name": "extract_pdf_tables", "arguments": {"path": messy, "mock_vlm": true}}),
    );
    let mocked: serde_json::Value =
        serde_json::from_str(tool_text(&mock_tables)).expect("mock tables JSON");
    let tables = mocked["tables"].as_array().expect("tables array");
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0]["source"], "vlm");
    assert!(tables[0]["markdown"]
        .as_str()
        .expect("markdown")
        .contains("mock_col_a"));

    // Unknown tools and methods fail cleanly (no hangs, no panics).
    let unknown_tool = mcp.request(
        "tools/call",
        serde_json::json!({"name": "nope", "arguments": {}}),
    );
    assert!(unknown_tool.get("error").is_some());
    let unknown_method = mcp.request("nope/method", serde_json::json!({}));
    assert_eq!(unknown_method["error"]["code"], -32601);
}
