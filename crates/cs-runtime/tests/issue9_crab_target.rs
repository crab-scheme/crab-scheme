//! #9 iter-6 — `(crab-target)` build-target identification proc.
//!
//! Returns a string distinguishing native / wasi-p1 / wasi-p2 builds
//! so Scheme code can `cond-expand` across the HTTP-server-shape
//! divergence (native uses the accept loop; wasip2 uses
//! `http-incoming-handler` — see ADR 0033).

use cs_core::WriteMode;
use cs_runtime::Runtime;

#[test]
fn crab_target_returns_a_known_string() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(crab-target)").unwrap();
    let s = rt.format_value(&v, WriteMode::Display);
    // On native, this is "native". On wasip1/wasip2 it'd be
    // "wasi-p1" / "wasi-p2". The test runs on the host that's
    // exercising it — assert membership rather than equality.
    assert!(
        matches!(s.as_str(), "native" | "wasi-p1" | "wasi-p2" | "wasi"),
        "(crab-target) returned an unexpected value: {s:?}"
    );
}

#[test]
fn crab_target_native_when_built_for_a_non_wasi_host() {
    // Tests run on the build host, which is non-wasi here. If this
    // ever runs under a wasi conformance runner, the assertion below
    // gates only on cfg, so the same source file is target-correct.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(crab-target)").unwrap();
    let s = rt.format_value(&v, WriteMode::Display);
    #[cfg(not(target_os = "wasi"))]
    assert_eq!(s, "native");
    #[cfg(all(target_os = "wasi", target_env = "p2"))]
    assert_eq!(s, "wasi-p2");
    #[cfg(all(target_os = "wasi", target_env = "p1"))]
    assert_eq!(s, "wasi-p1");
}

#[test]
fn crab_target_rejects_args() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(crab-target 'extra)")
        .expect_err("crab-target takes 0 args");
    assert!(
        format!("{err}").contains("crab-target"),
        "error should mention the proc name; got: {err}"
    );
}
