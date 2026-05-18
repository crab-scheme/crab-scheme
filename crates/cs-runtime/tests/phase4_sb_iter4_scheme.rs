//! Phase-4-sb iter 4 — Scheme builtins for the WASM-instance sandbox.
//!
//! Gated by the `sandbox` Cargo feature. Tests that require the
//! crabscheme.wasm binary use the same `requires_wasm!` skip
//! pattern as cs-sandbox-wasm/tests/iter15_protocol.rs; tests
//! that exercise only the surface (predicates, error paths)
//! don't need the binary.

#![cfg(feature = "sandbox")]

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn binary_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("CRABSCHEME_WASM_PATH") {
        let p = PathBuf::from(env_path);
        return p.exists().then_some(p);
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip1/release/crabscheme.wasm");
    default.exists().then_some(default)
}

macro_rules! requires_wasm {
    () => {
        if binary_path().is_none() {
            eprintln!("skipping: crabscheme.wasm not found");
            return;
        }
    };
}

fn make_sandbox_src(preset: &str) -> String {
    let path = binary_path().expect("binary path required by requires_wasm!");
    format!(
        "(make-wasm-sandbox '{} \"{}\")",
        preset,
        path.to_string_lossy()
    )
}

// ---- predicate (no binary needed) ----

#[test]
fn sandbox_predicate_false_for_non_sandbox_values() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(list (sandbox? 42) (sandbox? 'sym) (sandbox? '()) (sandbox? \"str\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f #f)");
}

// ---- error cases (no binary needed) ----

#[test]
fn make_wasm_sandbox_rejects_unknown_preset() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(make-wasm-sandbox 'wat-is-this)")
        .expect_err("unknown preset should fail");
    let s = format!("{}", err);
    assert!(s.contains("unknown preset"), "got: {}", s);
}

#[test]
fn make_wasm_sandbox_rejects_non_symbol_preset() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(make-wasm-sandbox 42)")
        .expect_err("non-symbol");
    let s = format!("{}", err);
    assert!(s.contains("symbol"), "got: {}", s);
}

#[test]
fn make_wasm_sandbox_arity_check() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(make-wasm-sandbox)").is_err());
    assert!(rt
        .eval_str("<t>", "(make-wasm-sandbox 'hygiene \"path\" 'extra-arg)")
        .is_err());
}

#[test]
fn sandbox_eval_rejects_non_sandbox() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(sandbox-eval 'not-a-sandbox \"(+ 1 2)\")")
        .expect_err("non-sandbox");
    let s = format!("{}", err);
    assert!(s.contains("not a sandbox"), "got: {}", s);
}

#[test]
fn sandbox_eval_rejects_non_string_expr() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();
    let err = rt
        .eval_str("<t>", "(sandbox-eval sb '(+ 1 2))")
        .expect_err("expr arg must be string");
    let s = format!("{}", err);
    assert!(s.contains("string"), "got: {}", s);
}

// ---- end-to-end with binary ----

#[test]
fn sandbox_eval_simple_arithmetic() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();
    let v = rt
        .eval_str("<t>", "(sandbox-eval sb \"(+ 1 2 3)\")")
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

#[test]
fn sandbox_predicate_true_for_real_sandbox() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();
    let v = rt.eval_str("<t>", "(sandbox? sb)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn sandbox_eval_all_three_presets() {
    requires_wasm!();
    let mut rt = Runtime::new();
    for preset in ["hygiene", "plugin", "adversarial"] {
        let src = format!("(sandbox-eval {} \"(* 6 7)\")", make_sandbox_src(preset));
        let v = rt.eval_str("<t>", &src).unwrap();
        assert_eq!(
            disp(&rt, &v),
            "42",
            "preset {} produced wrong result",
            preset
        );
    }
}

#[test]
fn reset_sandbox_returns_unspecified_and_preserves_function() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("plugin"));
    rt.eval_str("<t>", &setup).unwrap();
    rt.eval_str("<t>", "(reset-sandbox! sb)").unwrap();
    let v = rt
        .eval_str("<t>", "(sandbox-eval sb \"(- 100 58)\")")
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn sandbox_eval_isolates_from_host_environment() {
    // Host defines x = 99; sandbox doesn't see it because the
    // sandbox is a fresh process-space instance.
    requires_wasm!();
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define x 99)").unwrap();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();
    let result = rt.eval_str("<t>", "(sandbox-eval sb \"x\")");
    // Result is an error (x unbound in sandbox) — sandbox-eval
    // raises with "guest exit(...)" wrapping the diagnostic.
    assert!(result.is_err(), "host x leaked into sandbox: {:?}", result);
}
