//! Phase 2A.5 (#32) — macro-expansion smoke tests for the beam
//! libraries migrated from `syntax-rules` to `define-syntax-parser`.
//!
//! No prior test loaded these libs from disk — the web/channel suites
//! inline their own copies of the macros — so `channels.scm`,
//! `web-server.scm`, and `web-contracts.scm` migrations were unverified
//! end to end.
//!
//! We can't `eval_str` `web-*.scm` wholesale: they contain helper
//! functions written `(define (f a . rest) ...)`, a dotted-rest shape
//! cs-expand's define-shape parser rejects ("bad function form"). That
//! is a pre-existing limitation unrelated to #32 (the helpers are
//! untouched by it). So instead we extract just the migrated
//! `(define-syntax-parser ...)` forms from each file and expand them:
//! defining the macros, then defining functions that *use* each
//! keyword macro. Expansion is where the syntax-rules→define-syntax-
//! parser migration could go wrong (a malformed `#:literals` clause, a
//! literal that no longer matches); the templates reference runtime
//! primops only as free variables, so no feature or live server is
//! needed.

use cs_runtime::Runtime;

/// Extract every top-level `(define-syntax-parser ...)` form from
/// Scheme source, balancing `()`/`[]` while skipping `;` line comments
/// and `"..."` strings. This pulls the migrated macros out of a file
/// whose other forms cs-expand can't load (see module docs).
fn parser_macros(src: &str) -> String {
    const MARKER: &str = "(define-syntax-parser";
    let bytes = src.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        // Byte-wise match: `i` may land mid-UTF-8 (lib comments use
        // em-dashes), so slicing `src` as a str here would panic. The
        // captured `start..j` slice is always paren-delimited (ASCII),
        // so it stays on char boundaries.
        if bytes[i..].starts_with(MARKER.as_bytes()) {
            let start = i;
            let mut depth = 0i32;
            let mut in_str = false;
            let mut j = i;
            while j < bytes.len() {
                let c = bytes[j];
                if in_str {
                    if c == b'"' {
                        in_str = false;
                    }
                } else if c == b';' {
                    while j < bytes.len() && bytes[j] != b'\n' {
                        j += 1;
                    }
                    continue;
                } else if c == b'"' {
                    in_str = true;
                } else if c == b'(' || c == b'[' {
                    depth += 1;
                } else if c == b')' || c == b']' {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                j += 1;
            }
            out.push_str(&src[start..j]);
            out.push('\n');
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn read_lib(rel: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

/// Load a lib's migrated macros into a fresh runtime.
fn rt_with_macros(rel: &str) -> Runtime {
    let macros = parser_macros(&read_lib(rel));
    assert!(
        macros.contains("define-syntax-parser"),
        "no parser macros extracted from {rel}"
    );
    let mut rt = Runtime::new();
    rt.eval_str("<macros>", &macros)
        .unwrap_or_else(|e| panic!("define {rel} macros: {e}"));
    rt
}

/// `channels.scm`: `with-channel`, plus `select`/`select-biased`
/// fanning out into `select-build` (the `recv`/`send!`/`after`/`else`
/// literals). Uses sit in never-called thunks so the channel primops
/// (which need a tokio/actor context) are referenced but not run.
#[test]
fn channels_macros_expand() {
    let mut rt = rt_with_macros("lib/beam/channels.scm");
    rt.eval_str(
        "<t>",
        "(define (use-wc) (with-channel (c (make-channel 4)) (channel-send! c 7) (channel-recv c)))",
    )
    .expect("with-channel expands");
    rt.eval_str(
        "<t>",
        "(define (use-select c) \
           (select [(recv c) x (list 'got x)] \
                   [(send! c 1) 'sent] \
                   [(after 10) 'timeout] \
                   [else 'empty]))",
    )
    .expect("select / select-build (recv send! after else) expand");
    rt.eval_str(
        "<t>",
        "(define (use-biased c) (select-biased [(recv c) x x] [else 'none]))",
    )
    .expect("select-biased expands");
}

/// `web-contracts.scm`: `check-request`, `with-request` (inner
/// `let-syntax` locals kept as `syntax-rules`), and
/// `with-validated-request` (the `#:param`/`#:header`/`#:body` keyword
/// literals, both arities).
#[test]
fn web_contracts_macros_expand() {
    let mut rt = rt_with_macros("lib/beam/web-contracts.scm");
    rt.eval_str(
        "<t>",
        "(define (h4 h) (check-request h ([id 5 number?]) 'ok))",
    )
    .expect("check-request expands");
    rt.eval_str(
        "<t>",
        "(define (h1 h) (with-request h (list (method) (path) (param \"id\"))))",
    )
    .expect("with-request expands (inner let-syntax intact)");
    rt.eval_str(
        "<t>",
        "(define (h2 h) \
           (with-validated-request h \
             #:param  ([id string?]) \
             #:header ([t string?]) \
             #:body   string? \
             (lambda (id t body) (list id t body))))",
    )
    .expect("with-validated-request (with #:body) expands");
    rt.eval_str(
        "<t>",
        "(define (h3 h) \
           (with-validated-request h \
             #:param  ([id string?]) \
             #:header ([t string?]) \
             (lambda (id t) (list id t))))",
    )
    .expect("with-validated-request (no #:body) expands");
}

/// `web-server.scm`: `define-handler` (the `middleware` literal) and
/// `define-server` fanning out into `server-action`/`server-mw` (their
/// `route`/`access-log`/`middleware`/`request-id`/`timeout` literals).
#[test]
fn web_server_macros_expand() {
    let mut rt = rt_with_macros("lib/beam/web-server.scm");
    // Stub the Rust primops the macros expand into, so a top-level
    // define-server expands AND runs server-action/server-mw dispatch
    // without the web feature or a live socket. (`define-server` is a
    // top-level form — it expands to a `define` and runs
    // web-server-create.)
    rt.eval_str(
        "<stub>",
        "(define web-server-create     (lambda args 'sid)) \
         (define web-access-log!       (lambda args 'ok)) \
         (define web-route-static!     (lambda args 'ok)) \
         (define web-route-actor!      (lambda args 'ok)) \
         (define web-layer-request-id! (lambda args 'ok)) \
         (define web-layer-trace!      (lambda args 'ok)) \
         (define web-layer-timeout!    (lambda args 'ok)) \
         (define run-middleware-chain  (lambda args 'ok)) \
         (define some-pid 'pid)",
    )
    .expect("primop stubs");

    rt.eval_str("<t>", "(define-handler my-h (middleware) (lambda (h) h))")
        .expect("define-handler (middleware literal) expands");
    rt.eval_str(
        "<t>",
        "(define-server app \"127.0.0.1:0\" \
           (middleware request-id trace (timeout 5000)) \
           (access-log \"acc\") \
           (route 'GET  \"/health\" (static \"ok\")) \
           (route 'POST \"/users\"  some-pid))",
    )
    .expect("define-server / server-action / server-mw expand + dispatch");
    let v = rt
        .eval_str("<t>", "app")
        .expect("app bound by define-server");
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "sid");
}
