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
