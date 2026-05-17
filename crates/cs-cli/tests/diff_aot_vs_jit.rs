//! RC3 Phase 3 iter 3.1: differential test harness — AOT-compiled
//! programs must produce the same answer as the JIT tier.
//!
//! For each (source, entry, args, expected) tuple in the table:
//! 1. Compile the source to a bytecode → RIR → native binary via
//!    the crabscheme aot CLI (the same path users hit).
//! 2. Run the binary with `args`, capture stdout.
//! 3. Independently, run the same `(entry args)` invocation through
//!    cs_runtime's eval_str_via_vm (JIT tier when feature is on).
//! 4. Assert all three values agree: AOT-output == JIT-output ==
//!    expected.
//!
//! Treats the JIT (well-tested, conformance-validated) as the oracle.
//! AOT silently producing a different result would indicate a real
//! codegen bug — exactly what this harness exists to catch.
//!
//! Today this is restricted to the AOT-compatible subset
//! (self-recursive Fixnum kernels + simple let-bindings). The
//! coverage expands automatically as Phase 2 iters land more
//! supported Insts.

use std::path::PathBuf;
use std::process::Command;

use cs_core::Value;
use cs_runtime::Runtime;

/// Resolve the crabscheme binary built by the workspace's release
/// profile. The harness assumes the binary was already built (CI's
/// `cargo build --release` step covers this; locally devs can `cargo
/// build --release -p cs-cli` before running these tests).
fn crabscheme_bin() -> PathBuf {
    let crate_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = crate_manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root resolves");
    let bin = workspace_root.join("target/release/crabscheme");
    if !bin.exists() {
        // Build it. The test would otherwise fail with a confusing
        // "no such binary"; building once is cheap if cached.
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args(["build", "--release", "-p", "cs-cli"])
            .status()
            .expect("cargo executes");
        assert!(status.success(), "cargo build of cs-cli failed");
    }
    bin
}

/// Run `crabscheme aot <src_file> --entry <entry> --build`, then
/// invoke the resulting binary with `cli_args` and return stdout.
fn run_via_aot(src: &str, entry: &str, cli_args: &[&str]) -> String {
    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-diff-{entry}-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);
    std::fs::create_dir_all(&tmpdir).expect("create tmpdir");
    let src_path = tmpdir.join("input.scm");
    std::fs::write(&src_path, src).expect("write src");

    let proj_dir = tmpdir.join("proj");
    let crabscheme = crabscheme_bin();
    let out = Command::new(&crabscheme)
        .arg("aot")
        .arg(&src_path)
        .arg("--entry")
        .arg(entry)
        .arg("-o")
        .arg(&proj_dir)
        .arg("--build")
        .output()
        .expect("crabscheme aot executes");
    assert!(
        out.status.success(),
        "crabscheme aot failed for entry `{entry}`:\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The `aot --build` step's emitted binary lives at
    // `<proj_dir>/target/release/<sanitized package name>`. The CLI
    // sets package_name = sanitize_pkg_name(entry).
    let bin_name = entry.replace('-', "_");
    let bin_path = proj_dir.join("target/release").join(&bin_name);
    let run = Command::new(&bin_path)
        .args(cli_args)
        .output()
        .expect("AOT binary executes");
    assert!(
        run.status.success(),
        "AOT binary failed for entry `{entry}` args {cli_args:?}:\nstderr:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8_lossy(&run.stdout).trim().to_string()
}

/// Run the equivalent `(entry args...)` invocation through the JIT
/// tier as the oracle. Returns the rendered i64 result.
fn run_via_jit(src: &str, entry: &str, cli_args: &[&str]) -> String {
    let mut rt = Runtime::new();
    #[cfg(feature = "jit")]
    rt.install_jit().expect("install_jit");

    // Evaluate the definitions first.
    rt.eval_str_via_vm("<diff-src>", src)
        .expect("eval src defs");

    // Build a `(entry arg1 arg2 ...)` call expr and eval it.
    let call_expr = format!(
        "({entry} {})",
        cli_args.iter().copied().collect::<Vec<_>>().join(" "),
    );
    let v = rt
        .eval_str_via_vm("<diff-call>", &call_expr)
        .expect("eval call");
    render_value(&v)
}

/// Render a Value as the same text the AOT'd binary's `println!`
/// produces. Today AOT only supports Fixnum-returning entries via
/// the Nb shim, so this only needs to handle the integer case.
fn render_value(v: &Value) -> String {
    match v {
        Value::Number(cs_core::Number::Fixnum(n)) => n.to_string(),
        other => panic!("diff_aot_vs_jit: unsupported Value variant {other:?}"),
    }
}

/// The core differential assertion: AOT output, JIT output, and the
/// `expected` value all agree.
fn assert_diff(src: &str, entry: &str, cli_args: &[&str], expected: &str) {
    let aot = run_via_aot(src, entry, cli_args);
    let jit = run_via_jit(src, entry, cli_args);
    assert_eq!(
        jit,
        expected,
        "JIT (oracle) disagreed with expected for `({entry} {})` — \
         test case may be wrong:\n  jit={jit:?}\n  expected={expected:?}",
        cli_args.iter().copied().collect::<Vec<_>>().join(" "),
    );
    assert_eq!(
        aot,
        jit,
        "AOT diverged from JIT oracle for `({entry} {})`:\n  aot={aot:?}\n  jit={jit:?}",
        cli_args.iter().copied().collect::<Vec<_>>().join(" "),
    );
}

#[test]
fn diff_fact_10() {
    assert_diff(
        "(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))",
        "fact",
        &["10"],
        "3628800",
    );
}

#[test]
fn diff_fib_25() {
    assert_diff(
        "(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))",
        "fib",
        &["25"],
        "75025",
    );
}

#[test]
fn diff_ack_3_6() {
    assert_diff(
        "(define (ack m n) (if (= m 0) (+ n 1) (if (= n 0) (ack (- m 1) 1) (ack (- m 1) (ack m (- n 1))))))",
        "ack",
        &["3", "6"],
        "509",
    );
}

#[test]
fn diff_let_binding() {
    // Exercises RC3 iter 2.5's multi-block demote (let in a multi-
    // block function). If AOT's demote diverges from the VM/JIT's
    // env-based semantics, this catches it.
    assert_diff(
        "(define (h n) (let ((doubled (* n 2))) (if (< doubled 100) doubled (* doubled 2))))",
        "h",
        &["50"],
        "200",
    );
}
