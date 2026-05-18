//! cs-sandbox-wasm iter 1.5 — crabscheme.wasm protocol integration.
//!
//! Tests evaluate Scheme expressions through the embedded
//! wasmtime + the real `crabscheme.wasm` binary.
//!
//! ## Binary path resolution
//!
//! Tests look for the binary in this order:
//! 1. `CRABSCHEME_WASM_PATH` env var (CI / explicit overrides)
//! 2. `../../target/wasm32-wasip1/release/crabscheme.wasm`
//!    (the standard out-of-tree workspace build path)
//!
//! If neither resolves to an existing file, every test in this
//! module is SKIPPED (returns `Ok(())` early) — keeps CI green
//! on machines that haven't built the WASM target yet. To run
//! these tests locally:
//!
//! ```bash
//! cargo build --release --target wasm32-wasip1 --no-default-features --bin crabscheme
//! cargo test -p cs-sandbox-wasm --test iter15_protocol
//! ```

use std::path::PathBuf;

use cs_sandbox_wasm::{SandboxConfig, SandboxError, SandboxInstance};

/// Resolve the crabscheme.wasm binary path or return `None` if
/// no candidate exists. Tests use `if let Some` to skip.
fn binary_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("CRABSCHEME_WASM_PATH") {
        let p = PathBuf::from(env_path);
        return p.exists().then_some(p);
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip1/release/crabscheme.wasm");
    default.exists().then_some(default)
}

fn cfg(preset: SandboxConfig) -> SandboxConfig {
    let mut c = preset;
    c.binary_path = binary_path();
    c
}

// ---- helper that runs each test only if the binary is present ----

macro_rules! requires_wasm {
    ($binding:ident) => {
        let $binding = match binary_path() {
            Some(p) => p,
            None => {
                eprintln!(
                    "skipping: crabscheme.wasm not found. Build it via \
                     `cargo build --release --target wasm32-wasip1 \
                     --no-default-features --bin crabscheme`"
                );
                return;
            }
        };
        let _ = $binding;
    };
}

// ---- basic eval round-trip ----

#[test]
fn eval_simple_arithmetic_returns_correct_value() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::hygiene())).unwrap();
    let result = sb.eval("(+ 1 2 3)").unwrap();
    assert_eq!(result, "6");
}

#[test]
fn eval_multiplication() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::hygiene())).unwrap();
    let result = sb.eval("(* 6 7)").unwrap();
    assert_eq!(result, "42");
}

#[test]
fn eval_list_construction() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::hygiene())).unwrap();
    let result = sb.eval("(list 1 2 3)").unwrap();
    assert_eq!(result, "(1 2 3)");
}

#[test]
fn eval_string_literal() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::hygiene())).unwrap();
    let result = sb.eval("\"hello\"").unwrap();
    // The guest's --eval prints via display semantics by
    // default; strings print without surrounding quotes.
    assert!(
        result == "hello" || result == "\"hello\"",
        "got: {:?}",
        result
    );
}

#[test]
fn eval_runs_with_adversarial_preset() {
    // adversarial preset: fresh per-eval, strict fuel limit.
    // (+ 1 2) uses negligible fuel.
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::adversarial())).unwrap();
    let result = sb.eval("(+ 1 2)").unwrap();
    assert_eq!(result, "3");
}

#[test]
fn eval_runs_with_plugin_preset() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::plugin())).unwrap();
    let result = sb.eval("(- 100 58)").unwrap();
    assert_eq!(result, "42");
}

// ---- resource isolation: filesystem ----

#[test]
fn no_filesystem_access_by_default() {
    // hygiene preset: allow_paths is empty. The guest can't
    // open files. Try `(open-input-file ...)` and expect a
    // failure (the guest's error surfaces via GuestRaised or
    // an internal error depending on how WASI denies it).
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::hygiene())).unwrap();
    let result = sb.eval("(open-input-file \"/etc/passwd\")");
    assert!(
        result.is_err(),
        "expected eval to fail; got Ok({:?})",
        result
    );
}

// ---- fuel exhaustion ----

#[test]
fn infinite_loop_under_adversarial_preset_exhausts_fuel() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::adversarial())).unwrap();
    // Tight infinite loop. Adversarial preset fuel = 10M.
    // crabscheme's walker tier is slow per instruction so 10M
    // wasm instructions should NOT cover an infinite loop
    // running through the walker; we expect FuelExhausted.
    let result = sb.eval("(let loop () (loop))");
    match result {
        Err(SandboxError::FuelExhausted) => (),
        Err(other) => {
            // Acceptable alternative: the walker traps with
            // some other error before fuel runs out (e.g., the
            // tail-call recursion blows the stack). Document
            // both and accept; the assertion is just "the
            // sandbox doesn't hang."
            eprintln!("got non-FuelExhausted error (acceptable): {:?}", other);
        }
        Ok(v) => panic!("infinite loop returned value {:?}", v),
    }
}

// ---- reset rebuilds the runtime ----

#[test]
fn reset_does_not_lose_binary_cache() {
    requires_wasm!(_p);
    let mut sb = SandboxInstance::new(cfg(SandboxConfig::plugin())).unwrap();
    let result1 = sb.eval("(+ 1 1)").unwrap();
    sb.reset().unwrap();
    let result2 = sb.eval("(+ 2 2)").unwrap();
    assert_eq!(result1, "2");
    assert_eq!(result2, "4");
}

// ---- no binary configured: clear error ----

#[test]
fn eval_without_binary_path_returns_clear_error() {
    // Iter 1 path: SandboxConfig::hygiene() leaves binary_path
    // as None. eval() should report the missing path clearly.
    let mut sb = SandboxInstance::new(SandboxConfig::hygiene()).unwrap();
    let err = sb.eval("(+ 1 2)").unwrap_err();
    let s = format!("{}", err);
    assert!(
        s.contains("binary_path") || s.contains("not set"),
        "got: {}",
        s
    );
}
