//! Tests for the `(drop-sandbox! s)` Scheme builtin.
//!
//! The L2 sandbox docs (`docs/user/sandboxing.md`) listed
//! "no drop-sandbox!" as a known gap: the thread-local
//! `SANDBOXES` vector grew monotonically and never released its
//! `wasmtime::Engine`/`Module` cache entries. This file pins:
//!
//! 1. Drop is callable on a sandbox value.
//! 2. After drop, `sandbox-eval` / `reset-sandbox!` raise a
//!    "sandbox has been dropped" error.
//! 3. After drop, the `sandbox?` predicate still returns `#t`
//!    (the Scheme value is unchanged — only the underlying
//!    instance was released).
//! 4. Drop is idempotent (a second drop on the same value is a
//!    no-op).
//! 5. Drop on a non-sandbox argument raises a type error.
//! 6. Drop with the wrong arity raises an arity error.
//!
//! Mirrors the layout of `phase4_sb_iter4_scheme.rs`: tests that
//! exercise only the surface (predicates, error paths) skip the
//! binary requirement; the end-to-end test gates on
//! `requires_wasm!`.

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

// ---- error cases (no binary needed) ----

#[test]
fn drop_sandbox_rejects_non_sandbox() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(drop-sandbox! 'not-a-sandbox)")
        .expect_err("non-sandbox argument should fail");
    let s = format!("{}", err);
    assert!(s.contains("not a sandbox"), "got: {}", s);
}

#[test]
fn drop_sandbox_arity_check() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(drop-sandbox!)").is_err());
    assert!(rt.eval_str("<t>", "(drop-sandbox! 'a 'b)").is_err());
}

// ---- end-to-end with binary ----

#[test]
fn drop_sandbox_releases_instance_and_subsequent_eval_errors() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();

    // Baseline: eval works before drop.
    let pre = rt
        .eval_str("<t>", "(sandbox-eval sb \"(+ 1 2 3)\")")
        .unwrap();
    assert_eq!(disp(&rt, &pre), "6");

    // Drop returns Unspecified.
    let dropped = rt.eval_str("<t>", "(drop-sandbox! sb)").unwrap();
    assert_eq!(disp(&rt, &dropped), "");

    // Subsequent eval surfaces the "dropped" error.
    let err = rt
        .eval_str("<t>", "(sandbox-eval sb \"(+ 1 2 3)\")")
        .expect_err("eval after drop should error");
    let s = format!("{}", err);
    assert!(s.contains("dropped"), "got: {}", s);

    // reset-sandbox! after drop also errors.
    let err = rt
        .eval_str("<t>", "(reset-sandbox! sb)")
        .expect_err("reset after drop should error");
    let s = format!("{}", err);
    assert!(s.contains("dropped"), "got: {}", s);
}

#[test]
fn dropped_sandbox_value_still_satisfies_predicate() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();
    rt.eval_str("<t>", "(drop-sandbox! sb)").unwrap();

    // The Scheme value `#('__sandbox__ id)` is unchanged — only the
    // underlying SandboxInstance was released. `sandbox?` is a
    // shape check, not a liveness check, so it stays #t.
    let v = rt.eval_str("<t>", "(sandbox? sb)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn drop_sandbox_is_idempotent() {
    requires_wasm!();
    let mut rt = Runtime::new();
    let setup = format!("(define sb {})", make_sandbox_src("hygiene"));
    rt.eval_str("<t>", &setup).unwrap();

    // Two drops back-to-back; second is a no-op.
    rt.eval_str("<t>", "(drop-sandbox! sb)").unwrap();
    let v = rt.eval_str("<t>", "(drop-sandbox! sb)").unwrap();
    assert_eq!(disp(&rt, &v), "");
}
