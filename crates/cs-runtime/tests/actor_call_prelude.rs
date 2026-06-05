//! The auto-loaded `(crab actor)` synchronous-RPC prelude: `call` plus
//! its server-side helpers. The full send/raw-receive round-trip is
//! covered end-to-end by crab-cache's cluster/failover gates; here we
//! pin that the bundled library auto-loads and binds correct procedures
//! (a stale, never-shipped draft lived in lib/beam/prelude.scm).
#![cfg(all(feature = "actor", feature = "bundled-scheme"))]

use cs_runtime::Runtime;

/// `call` is global at startup — no `(import …)`, no hand-rolled
/// `(send target (cons (self) msg)) (raw-receive)`.
#[test]
fn crab_actor_call_is_autoloaded() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(procedure? call)").expect("eval");
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "#t");
}

/// The request shape is `(sender . payload)`; the server-side helpers
/// destructure and answer it. These are pure (no scheduler needed).
#[test]
fn crab_actor_request_helpers() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(list (call-sender (cons 'p 'm)) (call-message (cons 'p 'm)) (procedure? call-reply))",
        )
        .expect("eval");
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "(p m #t)");
}
