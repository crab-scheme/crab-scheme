//! cs-sandbox-wasm iter 6 — RTT measurements.
//!
//! Per ADR 0015 iter 6: collect timing numbers that inform the
//! iter-7 component-model migration go/no-go decision. The
//! current text protocol (--eval + stdout capture) has a known
//! cost; component-model with WIT-typed exports would replace
//! the argv + stdout dance with a typed function call.
//!
//! These are dev-time observability tests, not regression
//! gates: they print numbers and assert only loose upper bounds
//! to catch real perf regressions (>10x worse than current).
//!
//! Run with: `cargo test -p cs-sandbox-wasm --test iter6_bench -- --nocapture`
//! to see the numbers. They print regardless.

use std::path::PathBuf;
use std::time::Instant;

use cs_sandbox_wasm::{SandboxConfig, SandboxInstance};

fn binary_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("CRABSCHEME_WASM_PATH") {
        let p = PathBuf::from(env_path);
        return p.exists().then_some(p);
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip1/release/crabscheme.wasm");
    default.exists().then_some(default)
}

fn cfg() -> SandboxConfig {
    let mut c = SandboxConfig::hygiene();
    c.binary_path = binary_path();
    c
}

macro_rules! requires_wasm {
    () => {
        if binary_path().is_none() {
            eprintln!("skipping: crabscheme.wasm not found");
            return;
        }
    };
}

// ---- Cold-start: time from SandboxInstance::new to first eval result ----

#[test]
fn cold_start_includes_module_compile_plus_first_eval() {
    requires_wasm!();
    let t0 = Instant::now();
    let mut sb = SandboxInstance::new(cfg()).expect("construct");
    let after_new = t0.elapsed();
    let t1 = Instant::now();
    let result = sb.eval("(+ 1 2 3)").expect("eval");
    let first_eval = t1.elapsed();
    let total = t0.elapsed();
    assert_eq!(result, "6");
    eprintln!(
        "iter-6 cold-start | new={:.3}s eval#1={:.3}s total={:.3}s",
        after_new.as_secs_f64(),
        first_eval.as_secs_f64(),
        total.as_secs_f64()
    );
    // Loose guard: 30s for cold start. Real numbers are
    // typically much smaller; this just catches a 10x regression.
    assert!(
        total.as_secs_f64() < 30.0,
        "cold start regressed past 30s ceiling: {:.3}s",
        total.as_secs_f64()
    );
}

// ---- Warm RTT: median of N evals against same SandboxInstance ----

#[test]
fn warm_rtt_per_eval_under_repeated_calls() {
    requires_wasm!();
    let mut sb = SandboxInstance::new(cfg()).unwrap();
    // Discard one eval to warm any wasmtime caches.
    let _ = sb.eval("(+ 1 1)").unwrap();
    let n = 5usize;
    let mut times: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let r = sb.eval("(* 6 7)").unwrap();
        times.push(t0.elapsed().as_secs_f64());
        assert_eq!(r, "42");
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = times[n / 2];
    let min = times[0];
    let max = times[n - 1];
    eprintln!(
        "iter-6 warm-rtt | n={} median={:.3}s min={:.3}s max={:.3}s",
        n, median, min, max
    );
    // Each warm eval re-instantiates a fresh wasmtime Store +
    // _start (since the SandboxInstance reuses Engine + Module
    // but not Store across calls). Loose guard: 30s/eval
    // ceiling.
    assert!(
        median < 30.0,
        "warm RTT regressed past 30s/eval: {:.3}",
        median
    );
}

// ---- Per-instance setup cost (Engine + Module compile) ----

#[test]
fn per_instance_setup_cost() {
    requires_wasm!();
    let n = 3usize;
    let mut times: Vec<f64> = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let _sb = SandboxInstance::new(cfg()).unwrap();
        times.push(t0.elapsed().as_secs_f64());
    }
    let total: f64 = times.iter().sum();
    let avg = total / (n as f64);
    eprintln!(
        "iter-6 per-instance setup | n={} avg={:.3}s total={:.3}s",
        n, avg, total
    );
    // Each Engine + Module rebuild is expected to be the
    // dominant cost. Loose 30s/build ceiling.
    assert!(
        avg < 30.0,
        "per-instance setup regressed past 30s: {:.3}",
        avg
    );
}
