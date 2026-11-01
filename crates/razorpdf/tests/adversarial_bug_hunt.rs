//! MCP stdio edge cases: malformed input must yield JSON-RPC errors,
//! never a crash or a hang. Spawns the real `razorpdf mcp` binary.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
}

impl Mcp {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_razorpdf"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn razorpdf mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
        }
    }

    fn ask(&mut self, req: &str) -> serde_json::Value {
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
        let line = self
            .lines
            .next()
            .expect("server responds")
            .expect("read line");
        serde_json::from_str(&line).expect("response is JSON")
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn tmp_pdf(name: &str, bytes: &[u8]) -> String {
    let path = std::env::temp_dir().join(format!("razorpdf-mcp-{name}"));
    std::fs::write(&path, bytes).expect("write fixture");
    path.to_string_lossy().into_owned()
}

fn valid_pdf() -> Vec<u8> {
    razorpdf_core::test_fixtures::single_col_clean_pdf()
}

#[test]
fn mcp_initialize_and_list() {
    let mut mcp = Mcp::spawn();
    let init = mcp.ask(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
    assert_eq!(init["result"]["serverInfo"]["name"], "razorpdf");
    let list = mcp.ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"parse_pdf"));
}

#[test]
fn mcp_malformed_input_yields_rpc_errors() {
    let mut mcp = Mcp::spawn();
    let parse_err = mcp.ask("this is not json");
    assert_eq!(parse_err["error"]["code"], -32700);
    let no_method = mcp.ask(r#"{"jsonrpc":"2.0","id":7,"method":"nope","params":{}}"#);
    assert_eq!(no_method["error"]["code"], -32601);
    let no_tool =
        mcp.ask(r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"nope_tool"}}"#);
    assert_eq!(no_tool["error"]["code"], -32602);
    let bad_version = mcp.ask(r#"{"jsonrpc":"1.0","id":9,"method":"ping","params":{}}"#);
    assert_eq!(bad_version["error"]["code"], -32600);
}

#[test]
fn mcp_missing_and_bad_files_are_tool_errors_not_crashes() {
    let mut mcp = Mcp::spawn();
    // Missing `path` argument.
    let missing = mcp.ask(
        r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"parse_pdf","arguments":{}}}"#,
    );
    assert_eq!(missing["result"]["isError"], true);
    assert!(missing["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("missing required argument: path"));
    // Nonexistent file.
    let gone = mcp.ask(
        r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"parse_pdf","arguments":{"path":"/definitely/not/here.pdf"}}}"#,
    );
    assert_eq!(gone["result"]["isError"], true);
    // Garbage bytes: clean tool error, server stays alive.
    let junk = tmp_pdf("junk.pdf", b"not a pdf at all");
    let bad = mcp.ask(&format!(
        r#"{{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{{"name":"parse_pdf","arguments":{{"path":"{junk}"}}}}}}"#
    ));
    assert_eq!(bad["result"]["isError"], true);
    // Server still alive after all that: valid parse works.
    let good = tmp_pdf("good.pdf", &valid_pdf());
    let ok = mcp.ask(&format!(
        r#"{{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{{"name":"parse_pdf","arguments":{{"path":"{good}"}}}}}}"#
    ));
    assert!(ok["result"].get("isError").is_none());
    assert!(ok["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("RazorPDF Benchmark Document"));
}
