//! Phase 5 exit-gate test: drive the real `crabscheme-lsp` binary over
//! stdio and assert the two exit criteria plus the supporting features.
//!
//! Exit criterion 1 — open a `.scm` with messy indentation and request
//! `textDocument/formatting`; the server returns a TextEdit that
//! reindents it. Exit criterion 2 — `workspace/symbol` finds `define`s
//! across files in the workspace root. Also covers `rename` (5.6) and
//! `semanticTokens/full` (5.1).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
}

fn read_message<R: BufRead>(r: &mut R) -> Option<String> {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some(v) = t.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// A fresh temp directory, unique per process + nanosecond.
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "cs-lsp-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Spawn the server; return (child, stdin, channel of stdout messages).
fn spawn() -> (
    std::process::Child,
    std::process::ChildStdin,
    mpsc::Receiver<String>,
) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_crabscheme-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn crabscheme-lsp");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        while let Some(m) = read_message(&mut r) {
            if tx.send(m).is_err() {
                break;
            }
        }
    });
    (child, stdin, rx)
}

/// Wait for the response carrying json-rpc `id` (skipping notifications).
fn recv_id(rx: &mpsc::Receiver<String>, id: i32) -> String {
    let needle = format!("\"id\":{id}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(m) if m.contains(&needle) => return m,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    panic!("no response with id {id}");
}

#[test]
fn formatting_rename_and_semantic_tokens_over_stdio() {
    let (mut child, mut stdin, rx) = spawn();
    let mut send = |body: &str| {
        stdin.write_all(frame(body).as_bytes()).unwrap();
        stdin.flush().unwrap();
    };

    send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#);
    let init = recv_id(&rx, 1);
    // Capabilities advertise the Phase 5 providers.
    assert!(
        init.contains("\"documentFormattingProvider\""),
        "no formatting cap: {init}"
    );
    assert!(
        init.contains("\"semanticTokensProvider\""),
        "no semantic-tokens cap: {init}"
    );
    assert!(init.contains("\"renameProvider\""), "no rename cap: {init}");
    send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);

    // Messy indentation: nested forms flush-left.
    send(
        r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///t.scm","languageId":"scheme","version":1,"text":"(define (f x)\n(+ x\n1))"}}}"#,
    );

    // Exit criterion 1: formatting reindents (the `1))` moves to col 4).
    send(
        r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///t.scm"},"options":{"tabSize":2,"insertSpaces":true}}}"#,
    );
    let fmt = recv_id(&rx, 2);
    assert!(
        fmt.contains("\"newText\""),
        "formatting not a TextEdit: {fmt}"
    );
    assert!(
        fmt.contains("    1))"),
        "formatting did not reindent: {fmt}"
    );

    // semanticTokens/full returns a non-empty delta-encoded array.
    send(
        r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/semanticTokens/full","params":{"textDocument":{"uri":"file:///t.scm"}}}"#,
    );
    let sem = recv_id(&rx, 3);
    assert!(sem.contains("\"data\":["), "no semantic tokens data: {sem}");
    assert!(!sem.contains("\"data\":[]"), "semantic tokens empty: {sem}");

    // rename `f` (its define, line 0 char 9) → WorkspaceEdit using "g".
    send(
        r#"{"jsonrpc":"2.0","id":4,"method":"textDocument/rename","params":{"textDocument":{"uri":"file:///t.scm"},"position":{"line":0,"character":9},"newName":"g"}}"#,
    );
    let ren = recv_id(&rx, 4);
    assert!(
        ren.contains("\"newText\":\"g\""),
        "rename did not produce edits: {ren}"
    );

    send(r#"{"jsonrpc":"2.0","id":9,"method":"shutdown","params":null}"#);
    send(r#"{"jsonrpc":"2.0","method":"exit","params":null}"#);
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn workspace_symbol_over_stdio() {
    let dir = temp_dir("ws-e2e");
    std::fs::write(dir.join("a.scm"), "(define (alpha x) x)").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/b.scm"), "(define beta 2)").unwrap();
    let root_uri = format!("file://{}", dir.display());

    let (mut child, mut stdin, rx) = spawn();
    let mut send = |body: &str| {
        stdin.write_all(frame(body).as_bytes()).unwrap();
        stdin.flush().unwrap();
    };

    send(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"rootUri":"{root_uri}","capabilities":{{}}}}}}"#
    ));
    let init = recv_id(&rx, 1);
    assert!(
        init.contains("\"workspaceSymbolProvider\""),
        "no workspace-symbol cap: {init}"
    );
    send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);

    // Exit criterion 2: workspace/symbol finds the cross-file define.
    send(r#"{"jsonrpc":"2.0","id":2,"method":"workspace/symbol","params":{"query":"alpha"}}"#);
    let ws = recv_id(&rx, 2);
    assert!(
        ws.contains("\"name\":\"alpha\""),
        "workspace/symbol did not find alpha: {ws}"
    );
    // The query filters out non-matching defines.
    assert!(
        !ws.contains("\"name\":\"beta\""),
        "query should have excluded beta: {ws}"
    );

    // An empty query returns every define across the project.
    send(r#"{"jsonrpc":"2.0","id":3,"method":"workspace/symbol","params":{"query":""}}"#);
    let all = recv_id(&rx, 3);
    assert!(all.contains("\"name\":\"alpha\""), "missing alpha: {all}");
    assert!(all.contains("\"name\":\"beta\""), "missing beta: {all}");

    send(r#"{"jsonrpc":"2.0","id":9,"method":"shutdown","params":null}"#);
    send(r#"{"jsonrpc":"2.0","method":"exit","params":null}"#);
    drop(stdin);
    let _ = child.wait();
    std::fs::remove_dir_all(&dir).ok();
}
