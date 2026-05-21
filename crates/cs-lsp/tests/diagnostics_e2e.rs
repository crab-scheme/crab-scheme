//! Phase 1 exit-gate test: drive the real `crabscheme-lsp` binary
//! through a proper LSP handshake, open a file with a syntax error, and
//! assert the server publishes a matching diagnostic. This is the
//! "open a .scm file, introduce a syntax error, see it inline" gate,
//! exercised over the actual stdio transport.
//!
//! A reader thread + `recv_timeout` keep the test from hanging if a
//! regression stops the server from publishing (it fails instead).

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

/// Read one LSP message (Content-Length header + body) from `r`.
fn read_message<R: BufRead>(r: &mut R) -> Option<String> {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[test]
fn did_open_broken_file_publishes_diagnostic() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_crabscheme-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crabscheme-lsp");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");

    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        while let Some(m) = read_message(&mut r) {
            if tx.send(m).is_err() {
                break;
            }
        }
    });

    let mut send = |body: &str| {
        stdin.write_all(frame(body).as_bytes()).unwrap();
        stdin.flush().unwrap();
    };

    send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#);
    // Proper handshake: wait for the initialize response before sending
    // notifications (tower-lsp drops notifications received pre-init).
    rx.recv_timeout(Duration::from_secs(10))
        .expect("initialize response");

    send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
    send(
        r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///t.scm","languageId":"scheme","version":1,"text":"(+ 1 2"}}}"#,
    );

    // Drain until publishDiagnostics (the initialized logMessage precedes it).
    let mut publish = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(m) if m.contains("publishDiagnostics") => {
                publish = Some(m);
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    send(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#);
    send(r#"{"jsonrpc":"2.0","method":"exit","params":null}"#);
    drop(stdin);
    let _ = child.wait();
    let _ = reader.join();

    let publish = publish.expect("server should publish a diagnostic for a broken file");
    assert!(publish.contains("unclosed list"), "got: {publish}");
    assert!(
        publish.contains("\"severity\":1"),
        "expected Error severity (1); got: {publish}"
    );
    assert!(
        publish.contains("file:///t.scm"),
        "wrong uri; got: {publish}"
    );
}
