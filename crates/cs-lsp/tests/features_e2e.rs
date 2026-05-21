//! Feature exit-gate test (Phases 2–4): drive the real `crabscheme-lsp`
//! binary over stdio and assert the responses for documentSymbol, hover,
//! definition, references, completion (a `let` snippet), and signature
//! help (`(cons obj1 obj2)`). Exercises the full request/response path.

use std::io::{BufRead, BufReader, Read, Write};
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

#[test]
fn document_symbol_and_hover_over_stdio() {
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

    // Find a response message with the given json-rpc id (skipping
    // notifications), or fail on timeout.
    let recv_id = |rx: &mpsc::Receiver<String>, id: i32| -> String {
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
    };

    send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#);
    recv_id(&rx, 1);
    send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
    send(
        r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///t.scm","languageId":"scheme","version":1,"text":"(define (f x) x)\n(f 1)\n(cons 1 2)"}}}"#,
    );

    // documentSymbol → outline contains f, kind Function (12).
    send(
        r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":"file:///t.scm"}}}"#,
    );
    let sym = recv_id(&rx, 2);
    assert!(sym.contains("\"name\":\"f\""), "outline missing f: {sym}");

    // hover on the use of f at line 1, char 1 → "defined at line 1".
    send(
        r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///t.scm"},"position":{"line":1,"character":1}}}"#,
    );
    let hov = recv_id(&rx, 3);
    assert!(hov.contains("defined at line 1"), "hover wrong: {hov}");

    // goto-definition on the use of f (line 1) → its define on line 0.
    send(
        r#"{"jsonrpc":"2.0","id":4,"method":"textDocument/definition","params":{"textDocument":{"uri":"file:///t.scm"},"position":{"line":1,"character":1}}}"#,
    );
    let def = recv_id(&rx, 4);
    assert!(def.contains("\"line\":0"), "definition wrong: {def}");

    // references of f (with declaration) → 2 Locations (define + use).
    send(
        r#"{"jsonrpc":"2.0","id":5,"method":"textDocument/references","params":{"textDocument":{"uri":"file:///t.scm"},"position":{"line":1,"character":1},"context":{"includeDeclaration":true}}}"#,
    );
    let refs = recv_id(&rx, 5);
    assert_eq!(
        refs.matches("\"range\"").count(),
        2,
        "references wrong: {refs}"
    );

    // Phase 4 exit criterion 1: completion offers a `let` snippet.
    send(
        r#"{"jsonrpc":"2.0","id":6,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///t.scm"},"position":{"line":2,"character":0}}}"#,
    );
    let comp = recv_id(&rx, 6);
    assert!(
        comp.contains("\"label\":\"let\""),
        "no let completion: {comp}"
    );
    assert!(
        comp.contains("\"insertTextFormat\":2"),
        "let completion not a snippet: {comp}"
    );

    // Phase 4 exit criterion 2: signature help for cons (line 2).
    send(
        r#"{"jsonrpc":"2.0","id":7,"method":"textDocument/signatureHelp","params":{"textDocument":{"uri":"file:///t.scm"},"position":{"line":2,"character":6}}}"#,
    );
    let sig = recv_id(&rx, 7);
    assert!(
        sig.contains("(cons obj1 obj2)"),
        "signature help wrong: {sig}"
    );

    send(r#"{"jsonrpc":"2.0","id":8,"method":"shutdown","params":null}"#);
    send(r#"{"jsonrpc":"2.0","method":"exit","params":null}"#);
    drop(stdin);
    let _ = child.wait();
    let _ = reader.join();
}
