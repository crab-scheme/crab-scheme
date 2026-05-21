//! MCP exit-gate test (Phase 6 iter 6.H2): drive the real
//! `crabscheme-mcp` binary over newline-delimited JSON-RPC stdio through
//! the MCP lifecycle (initialize → initialized → tools/list → tools/call)
//! — the contract a coding harness like Claude speaks.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn mcp_lifecycle_and_tool_calls() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_crabscheme-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crabscheme-mcp");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    // Reader thread: MCP messages are newline-delimited, one per line.
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut send = |body: &str| {
        stdin.write_all(body.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };

    // Read the response carrying json-rpc `id` (in-order here, but match
    // to be robust), or fail on timeout.
    let recv_id = |rx: &mpsc::Receiver<String>, id: i64| -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(line) => {
                    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
                        return v;
                    }
                }
                Err(_) => break,
            }
        }
        panic!("no response with id {id}");
    };

    // 1. initialize — server echoes a known protocol version + its info.
    send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
    );
    let init = recv_id(&rx, 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "crabscheme-mcp");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // 2. initialized notification — no response expected.
    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // 3. tools/list — all seven tools.
    send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
    let list = recv_id(&rx, 2);
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 7, "{list}");

    // 4. tools/call cs_diagnostics on inline broken source.
    send(
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"cs_diagnostics","arguments":{"text":"(+ 1 2"}}}"#,
    );
    let diag = recv_id(&rx, 3);
    assert_eq!(diag["result"]["isError"], false, "{diag}");
    let text = diag["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"severity\": \"error\""), "{text}");

    // 5. tools/call cs_format returns reindented source.
    send(
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"cs_format","arguments":{"text":"(a\n(b))"}}}"#,
    );
    let fmt = recv_id(&rx, 4);
    assert_eq!(
        fmt["result"]["content"][0]["text"].as_str().unwrap(),
        "(a\n  (b))",
        "{fmt}"
    );

    // 6. ping → empty result.
    send(r#"{"jsonrpc":"2.0","id":5,"method":"ping","params":{}}"#);
    let pong = recv_id(&rx, 5);
    assert!(pong["result"].is_object(), "{pong}");

    drop(stdin);
    let _ = child.wait();
    let _ = reader.join();
}
