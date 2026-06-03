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
//! Phase 3.2 broadened the corpus from the original 4 numeric
//! kernels to the full RC3-supported surface: cross-procedure
//! calls, mutual recursion, multi-block `cond`, heap vectors,
//! list/string builtins (generic dispatch), and the flonum surface.
//! Each new case was verified to build AND match the JIT — `--explain`
//! alone over-reports (it checks RIR shape, not that the emitted
//! project builds + runs; see the `#[ignore]`'d `diff_free_var_read`).
//! Entries return Fixnum or Flonum so
//! the JIT-oracle renderer can match the AOT binary's stdout exactly;
//! string/list *results* are exercised by feeding them into a
//! Fixnum-returning entry (e.g. `(string-length (string-append …))`)
//! rather than printed directly.

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
fn run_via_aot(src: &str, entry: &str, cli_args: &[&str], multi: bool) -> String {
    // Unique dir PER CALL. Many cases share the entry name `f`, so a
    // dir keyed only on (entry, pid) collides across parallel test
    // threads — two cases write the same `input.scm`/`proj` and clobber
    // each other (which silently ran one case's binary for another's
    // assertion). The atomic sequence guarantees distinct dirs.
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let pid = std::process::id();
    let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-diff-{entry}-{pid}-{uniq}"));
    let _ = std::fs::remove_dir_all(&tmpdir);
    std::fs::create_dir_all(&tmpdir).expect("create tmpdir");
    let src_path = tmpdir.join("input.scm");
    std::fs::write(&src_path, src).expect("write src");

    let proj_dir = tmpdir.join("proj");
    let crabscheme = crabscheme_bin();
    let mut cmd = Command::new(&crabscheme);
    cmd.arg("aot")
        .arg(&src_path)
        .arg("-o")
        .arg(&proj_dir)
        .arg("--build");
    if multi {
        // `--multi` populates the compiler's globals from the runtime
        // builtins (so `(display …)`, `string-append`, etc. fold to
        // constants) and resolves cross-procedure references, instead
        // of leaving them as unresolved env captures. Single `--entry`
        // mode can't compile programs that touch builtins / other
        // defines / free variables — the captured values change the
        // emitted fn signature and the entry shim hits an arg
        // mismatch. So anything beyond a self-contained numeric kernel
        // goes through `--multi`, the user-recommended path for it.
        cmd.arg("--multi");
    } else {
        cmd.arg("--entry").arg(entry);
    }
    let out = cmd.output().expect("crabscheme aot executes");
    assert!(
        out.status.success(),
        "crabscheme aot{} failed for entry `{entry}`:\nstderr:\n{}",
        if multi { " --multi" } else { "" },
        String::from_utf8_lossy(&out.stderr)
    );

    // `--entry` names the binary `sanitize_pkg_name(entry)` and takes
    // just the args. `--multi` names it after the source basename
    // (`input`) and dispatches on `<binary> <fn> <args…>`.
    let (bin_name, run_args): (String, Vec<&str>) = if multi {
        let mut a = vec![entry];
        a.extend_from_slice(cli_args);
        ("input".to_string(), a)
    } else {
        (entry.replace('-', "_"), cli_args.to_vec())
    };
    let bin_path = proj_dir.join("target/release").join(&bin_name);
    let run = Command::new(&bin_path)
        .args(&run_args)
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
        // Match the AOT binary's flonum fast path
        // (`println!("{}", f64::from_bits(...))`), which uses Rust's
        // f64 Display — e.g. 5.0 renders as "5", 1.5 as "1.5".
        Value::Number(cs_core::Number::Flonum(x)) => format!("{x}"),
        other => panic!("diff_aot_vs_jit: unsupported Value variant {other:?}"),
    }
}

/// Differential assertion via single-`--entry` AOT mode (the default
/// path; works for self-contained numeric kernels).
fn assert_diff(src: &str, entry: &str, cli_args: &[&str], expected: &str) {
    assert_diff_mode(src, entry, cli_args, expected, false);
}

/// Differential assertion via `--multi` AOT mode — required for
/// programs that reference builtins, other top-level defines, or free
/// variables (single `--entry` mode can't compile those today).
fn assert_diff_multi(src: &str, entry: &str, cli_args: &[&str], expected: &str) {
    assert_diff_mode(src, entry, cli_args, expected, true);
}

/// The core differential assertion: AOT output, JIT output, and the
/// `expected` value all agree.
fn assert_diff_mode(src: &str, entry: &str, cli_args: &[&str], expected: &str, multi: bool) {
    let aot = run_via_aot(src, entry, cli_args, multi);
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

// ---- Phase 3.2: broad differential corpus ----
// Each case below exercises a distinct slice of the now-supported
// surface. Numeric/flonum/vector/control-flow cases build lean
// (cs-vm only); the string/list cases link cs-runtime for generic
// builtin dispatch and so cost a heavier emitted-project build.

#[test]
fn diff_cross_proc_call() {
    // f calls a *different* top-level define (Inst::Call), not just
    // itself. g(x)=x+1; f(n)=g(n)+g(2n)=3n+2. f(7)=23.
    assert_diff_multi(
        "(define (g x) (+ x 1)) (define (f n) (+ (g n) (g (* n 2))))",
        "f",
        &["7"],
        "23",
    );
}

#[test]
fn diff_mutual_recursion() {
    // e/o call each other; even n reaches e(0)=100. e(10)=100.
    assert_diff_multi(
        "(define (e n) (if (= n 0) 100 (o (- n 1)))) (define (o n) (if (= n 0) 200 (e (- n 1))))",
        "e",
        &["10"],
        "100",
    );
}

// Reading a top-level *value* binding (not a procedure) isn't AOT-able
// yet: `base` becomes an env capture, so single `--entry` hits an
// arg-mismatch and `--multi` skips the capturing fn. Same env-install
// gap as `set!`-on-globals and closure capture. Cross-procedure
// *function* references (see `diff_cross_proc_call`) DO work. `--explain`
// reports this "compatible" (it checks RIR shape, not that the emitted
// project builds + runs) — a known over-report, hence `#[ignore]`.
#[test]
#[ignore = "AOT can't read a global value binding yet (becomes a capture); env-install gap"]
fn diff_free_var_read() {
    assert_diff_multi(
        "(define base 1000) (define (f n) (+ base n))",
        "f",
        &["23"],
        "1023",
    );
}

#[test]
fn diff_vector_ops() {
    // Heap vector alloc + set + ref. v[0]=n; returns v[0]+7. f(35)=42.
    assert_diff_multi(
        "(define (f n) (let ((v (make-vector 3 0))) (vector-set! v 0 n) (+ (vector-ref v 0) 7)))",
        "f",
        &["35"],
        "42",
    );
}

#[test]
fn diff_cond_multiblock() {
    // Multi-block CFG beyond a single let — cond with 3 arms.
    // n=5 → middle arm (* n n) = 25.
    assert_diff_multi(
        "(define (f n) (cond ((< n 0) 0) ((< n 10) (* n n)) (else (+ n 100))))",
        "f",
        &["5"],
        "25",
    );
}

#[test]
fn diff_string_builtins() {
    // Generic builtin dispatch: string-append + number->string +
    // string-length, returning a Fixnum. "ab" ++ "7" = "ab7", len 3.
    assert_diff_multi(
        "(define (f n) (string-length (string-append \"ab\" (number->string n))))",
        "f",
        &["7"],
        "3",
    );
}

#[test]
fn diff_list_builtins() {
    // list + reverse + length via generic dispatch. The list always
    // has 4 elements, so length is 4 regardless of n.
    assert_diff_multi(
        "(define (f n) (length (reverse (list 1 2 3 n))))",
        "f",
        &["9"],
        "4",
    );
}

#[test]
fn diff_flonum_sqrt() {
    // Flonum Inst surface: exact->inexact + Mul + FlonumSqrt.
    // sqrt(n*n) = n as a flonum; n=5 → 5.0 → "5".
    assert_diff_multi(
        "(define (f n) (sqrt (exact->inexact (* n n))))",
        "f",
        &["5"],
        "5",
    );
}

// ---- closures (compile via --multi; single --entry doesn't enumerate
// nested lambdas, so MakeClosure surfaces there — see docs/user/aot.md) ----

#[test]
fn diff_closure_let_bound() {
    // A let-bound lambda, called in place. f(5) = 5 * 2 = 10.
    assert_diff_multi(
        "(define (f x) (let ((dbl (lambda (y) (* y 2)))) (dbl x)))",
        "f",
        &["5"],
        "10",
    );
}

#[test]
fn diff_closure_returned() {
    // A procedure that returns a closure over its arg, then calls it:
    // ((adder 10) 5) = 15. Exercises capture-by-value across the
    // MakeClosure → vm_alloc_aot_procedure_with_captures path.
    assert_diff_multi(
        "(define (adder n) (lambda (x) (+ x n))) (define (test) ((adder 10) 5))",
        "test",
        &[],
        "15",
    );
}

#[test]
fn diff_closure_higher_order_arg() {
    // A lambda passed as an argument and applied:
    // (apply-g (lambda (x) (+ x 100)) 5) = 105.
    assert_diff_multi(
        "(define (apply-g g v) (g v)) (define (test) (apply-g (lambda (x) (+ x 100)) 5))",
        "test",
        &[],
        "105",
    );
}
