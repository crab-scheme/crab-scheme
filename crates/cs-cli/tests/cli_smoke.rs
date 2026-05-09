//! End-to-end smoke tests driving the `crabscheme` binary.

use std::process::Command;

fn cli() -> Command {
    let bin = env!("CARGO_BIN_EXE_crabscheme");
    Command::new(bin)
}

fn run_eval(expr: &str) -> (String, String, i32) {
    let out = cli().args(["-e", expr]).output().expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let code = out.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

#[test]
fn eval_addition() {
    let (out, _, code) = run_eval("(+ 1 2)");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "3");
}

#[test]
fn eval_define_and_call() {
    let (out, _, code) = run_eval("(define (sq x) (* x x)) (sq 9)");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "81");
}

#[test]
fn eval_factorial() {
    let (out, _, code) = run_eval("(define (f n) (if (= n 0) 1 (* n (f (- n 1))))) (f 12)");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "479001600");
}

#[test]
fn eval_tail_call_iterative() {
    let (out, _, code) =
        run_eval("(define (loop n acc) (if (= n 0) acc (loop (- n 1) (+ acc 1)))) (loop 100000 0)");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "100000");
}

#[test]
fn eval_undefined_variable_error() {
    let (_, err, code) = run_eval("(foo 1 2)");
    assert_eq!(code, 2);
    assert!(err.contains("undefined"), "stderr: {}", err);
}

fn workspace_path(rel: &str) -> String {
    // CARGO_MANIFEST_DIR points at crates/cs-cli; go up two levels to the workspace root.
    let manifest = env!("CARGO_MANIFEST_DIR");
    format!("{}/../../{}", manifest, rel)
}

#[test]
fn run_factorial_file() {
    let out = cli()
        .args(["run", &workspace_path("examples/factorial.scm")])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "479001600");
}

#[test]
fn run_fibonacci_file() {
    let out = cli()
        .args(["run", &workspace_path("examples/fibonacci.scm")])
        .output()
        .expect("spawn");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "6765");
}

#[test]
fn run_missing_file_exits_one() {
    let out = cli()
        .args(["run", "/no/such/file.scm"])
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn eval_via_vm_tier() {
    // Same expression on the VM tier. Result should match.
    let out = cli()
        .args(["--tier", "vm", "-e", "(+ 1 2 3 4 5)"])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "15");
}

#[test]
fn run_factorial_via_vm() {
    let out = cli()
        .args([
            "--tier",
            "vm",
            "run",
            &workspace_path("examples/factorial.scm"),
        ])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "479001600");
}

use std::io::Write;
use std::process::Stdio;

fn run_repl_session(stdin_text: &str, args: &[&str]) -> (String, String, i32) {
    let mut child = cli()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn repl");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_text.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn repl_evaluates_expression() {
    let (out, _err, code) = run_repl_session("(+ 2 3)\n:quit\n", &["repl"]);
    assert_eq!(code, 0);
    assert!(out.contains('5'), "stdout: {:?}", out);
}

#[test]
fn repl_tier_switch() {
    let (out, _err, code) = run_repl_session(
        ":tier vm\n(* 7 7)\n:tier walker\n(* 8 8)\n:quit\n",
        &["repl"],
    );
    assert_eq!(code, 0);
    assert!(out.contains("tier: vm"), "stdout: {:?}", out);
    assert!(out.contains("49"), "stdout: {:?}", out);
    assert!(out.contains("tier: walker"), "stdout: {:?}", out);
    assert!(out.contains("64"), "stdout: {:?}", out);
}

#[test]
fn repl_starts_in_vm_tier_when_flag_passed() {
    let (out, _err, code) = run_repl_session("(* 11 11)\n:quit\n", &["--tier", "vm", "repl"]);
    assert_eq!(code, 0);
    // banner should mention "(vm)"
    assert!(out.contains("(vm)"), "stdout: {:?}", out);
    assert!(out.contains("121"), "stdout: {:?}", out);
}

#[test]
fn raised_condition_renders_as_error_msg() {
    // (error "msg" irritants...) on either tier should render as
    // "error: msg (irritants...)" rather than the raw list shape.
    let (_, err, code) = run_eval(r#"(error "bad thing" 42 "extra")"#);
    assert_eq!(code, 2);
    assert!(
        err.contains(r#"error: bad thing (42 "extra")"#),
        "stderr: {:?}",
        err
    );
}

#[test]
fn assertion_renders_friendly_message() {
    let (_, err, code) = run_eval("(assert (= 1 2))");
    assert_eq!(code, 2);
    assert!(err.contains("assertion failed"), "stderr: {:?}", err);
    // No raw list-shape leakage into the user-facing message.
    assert!(
        !err.contains(r#"("error""#),
        "raw condition shape leaked into stderr: {:?}",
        err
    );
}

#[test]
fn vm_raised_renders_as_error_msg() {
    let out = cli()
        .args(["--tier", "vm", "-e", r#"(error "x")"#])
        .output()
        .expect("spawn");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2));
    assert!(err.contains("error: x"), "stderr: {:?}", err);
    assert!(!err.contains("__raised__"), "stderr: {:?}", err);
}

/// Run examples/metacircular.scm — a metacircular Scheme evaluator that
/// runs three small programs (factorial 10, sum 1..100, mutable counter)
/// through itself. Stresses closures, env lookup, multi-body lambdas, and
/// the apply primitive — a good integration test on both tiers.
fn assert_metacircular_output(out: &[u8]) {
    let s = String::from_utf8_lossy(out);
    assert!(s.contains("metacircular: 3628800"), "stdout: {:?}", s);
    assert!(
        s.contains("metacircular sum 1..100: 5050"),
        "stdout: {:?}",
        s
    );
    assert!(
        s.contains("metacircular counter (3 calls): 3"),
        "stdout: {:?}",
        s
    );
}

#[test]
fn run_metacircular_walker() {
    let out = cli()
        .args(["run", &workspace_path("examples/metacircular.scm")])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_metacircular_output(&out.stdout);
}

#[test]
fn run_metacircular_vm() {
    let out = cli()
        .args([
            "--tier",
            "vm",
            "run",
            &workspace_path("examples/metacircular.scm"),
        ])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_metacircular_output(&out.stdout);
}

#[test]
fn features_builtin_returns_advertised_symbols() {
    let (out, _, code) = run_eval("(features)");
    assert_eq!(code, 0);
    let s = out.trim();
    // Must contain the four known features as symbols (no quoting).
    for f in ["crabscheme", "r6rs-subset", "r7rs-subset", "exact-closed"] {
        assert!(s.contains(f), "missing feature {} in {:?}", f, s);
    }
}

#[test]
fn version_builtin_returns_string() {
    let (out, _, code) = run_eval("(crabscheme-version)");
    assert_eq!(code, 0);
    // Should be a quoted string (write mode).
    assert!(out.contains("\"0.0"), "stdout: {:?}", out);
}

#[test]
fn walker_runtime_error_includes_call_stack_backtrace() {
    // Same fixture as the VM test, but on the walker tier. The walker
    // pushes App spans onto ctx.call_stack regardless of tail-call
    // status, so even tail-call chains contribute backtrace entries
    // (the VM only shows non-tail Frames).
    let bt_test = std::env::temp_dir().join("crabscheme_walker_bt_smoke.scm");
    std::fs::write(
        &bt_test,
        "(define (deep-error)\n  (foo 1 2)\n  'unreachable)\n\
         (define (middle)\n  (deep-error)\n  'unreachable)\n\
         (define (outer)\n  (middle)\n  'unreachable)\n\
         (outer)\n",
    )
    .unwrap();
    let out = cli()
        .args(["run", bt_test.to_str().unwrap()])
        .output()
        .expect("walker");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("undefined variable: foo"), "{}", err);
    assert!(
        err.contains("called from [1]"),
        "innermost-frame note missing: {}",
        err
    );
    let count = err.matches("called from").count();
    assert!(
        count >= 2,
        "expected ≥2 backtrace lines, got {}: {}",
        count,
        err
    );
    let _ = std::fs::remove_file(&bt_test);
}

#[test]
fn vm_runtime_error_includes_call_stack_backtrace() {
    // Three nested non-tail calls; deepest one references an undefined
    // variable. The VM walks frames at error time and emits one note per
    // outer frame.
    let bt_test = std::env::temp_dir().join("crabscheme_bt_smoke.scm");
    std::fs::write(
        &bt_test,
        "(define (deep-error)\n  (foo 1 2)\n  'unreachable)\n\
         (define (middle)\n  (deep-error)\n  'unreachable)\n\
         (define (outer)\n  (middle)\n  'unreachable)\n\
         (outer)\n",
    )
    .unwrap();
    let out = cli()
        .args(["--tier", "vm", "run", bt_test.to_str().unwrap()])
        .output()
        .expect("vm");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("undefined variable: foo"), "{}", err);
    assert!(
        err.contains("called from [1]"),
        "innermost-frame note missing: {}",
        err
    );
    // The outer two frames produce two more `called from` notes.
    let count = err.matches("called from").count();
    assert!(
        count >= 2,
        "expected ≥2 backtrace lines, got {}: {}",
        count,
        err
    );
    let _ = std::fs::remove_file(&bt_test);
}

#[test]
fn color_never_produces_plain_text() {
    // --color never: no ANSI escape codes regardless of TTY.
    let out = cli()
        .args(["--color", "never", "-e", "(foo 1 2)"])
        .output()
        .expect("spawn");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("undefined variable: foo"), "{}", err);
    assert!(
        !err.contains("\x1b["),
        "stderr unexpectedly has escape codes: {:?}",
        err
    );
}

#[test]
fn color_always_emits_ansi_escapes() {
    // --color always: emits ANSI codes for severity/file/caret.
    let out = cli()
        .args(["--color", "always", "-e", "(foo 1 2)"])
        .output()
        .expect("spawn");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("undefined variable: foo"), "{}", err);
    // Bold (\x1b[1m) and red (\x1b[31m) must both appear.
    assert!(err.contains("\x1b[1m"), "missing bold escape: {:?}", err);
    assert!(err.contains("\x1b[31m"), "missing red escape: {:?}", err);
}

#[test]
fn color_auto_off_when_stderr_is_pipe() {
    // Default --color auto: stderr captured by Command is not a TTY,
    // so output should be plain (no ANSI codes).
    let out = cli().args(["-e", "(foo 1 2)"]).output().expect("spawn");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("undefined variable"), "{}", err);
    assert!(!err.contains("\x1b["), "{}", err);
}

#[test]
fn include_form_splices_a_file_inline() {
    // (include "path") at expand time reads the file's contents and
    // inlines them as if typed at that position. Verifies on both tiers.
    let lib = std::env::temp_dir().join("crabscheme_incl_lib_smoke.scm");
    let main = std::env::temp_dir().join("crabscheme_incl_main_smoke.scm");
    std::fs::write(&lib, "(define (sq x) (* x x))\n").unwrap();
    std::fs::write(
        &main,
        format!("(include {:?})\n(sq 9)\n", lib.to_str().unwrap()),
    )
    .unwrap();

    let walker_out = cli()
        .args(["run", main.to_str().unwrap()])
        .output()
        .expect("walker");
    assert!(
        walker_out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&walker_out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&walker_out.stdout).trim(), "81");

    let vm_out = cli()
        .args(["--tier", "vm", "run", main.to_str().unwrap()])
        .output()
        .expect("vm");
    assert!(
        vm_out.status.success(),
        "stderr: {:?}",
        String::from_utf8_lossy(&vm_out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&vm_out.stdout).trim(), "81");

    let _ = std::fs::remove_file(lib);
    let _ = std::fs::remove_file(main);
}

#[test]
fn include_missing_file_reports_error() {
    let (_, err, code) = run_eval(r#"(include "/no/such/file.scm")"#);
    assert_eq!(code, 2);
    assert!(err.contains("include"), "stderr: {:?}", err);
    assert!(err.contains("cannot read"), "stderr: {:?}", err);
}

#[test]
fn repl_load_command_brings_definitions_into_scope() {
    // :load <path> reads a file and runs it in the current REPL session.
    // Definitions made by the loaded file should remain visible to
    // subsequent REPL input.
    let load_path = workspace_path("examples/factorial.scm");
    let session = format!(":load {}\n(define foo 41)\nfoo\n:quit\n", load_path);
    let (out, _err, code) = run_repl_session(&session, &["repl"]);
    assert_eq!(code, 0);
    // factorial.scm prints 479001600 and then the REPL reports it loaded.
    assert!(out.contains("479001600"), "{}", out);
    assert!(out.contains("; loaded"), "{}", out);
    // Definitions made AFTER the :load also work — confirms the REPL
    // didn't reset state.
    assert!(out.contains("41"), "{}", out);
}

#[test]
fn repl_load_missing_file_prints_error_continues() {
    let (_out, _err, code) = run_repl_session(":load /no/such/file.scm\n42\n:quit\n", &["repl"]);
    assert_eq!(code, 0);
    // Nothing else asserted — just that the REPL stayed alive past the
    // failed load and still evaluated the trailing 42.
}

#[test]
fn vm_arity_mismatch_has_source_span_and_no_dup_prefix() {
    // Arity mismatch: span on the offending call site + descriptive
    // expected/got message. No duplicate "+: +: ..." builtin prefix.
    let out = cli()
        .args(["--tier", "vm", "-e", "(define (sq x) (* x x)) (sq 1 2 3)"])
        .output()
        .expect("vm");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("arity mismatch"), "{}", err);
    assert!(err.contains("expected 1"), "{}", err);
    assert!(err.contains("got 3"), "{}", err);
    assert!(err.contains(":1:"), "should have span: {}", err);
}

#[test]
fn vm_builtin_type_error_has_no_doubled_prefix() {
    // Builtin error path used to render as "+: +: expected ..." because
    // the VM dispatch added one prefix while the builtin already had its
    // own. Now we strip the duplicate. Walker output is the reference.
    let walker = cli().args(["-e", r#"(+ 1 "two")"#]).output().unwrap();
    let walker_err = String::from_utf8_lossy(&walker.stderr).into_owned();
    let vm = cli()
        .args(["--tier", "vm", "-e", r#"(+ 1 "two")"#])
        .output()
        .unwrap();
    let vm_err = String::from_utf8_lossy(&vm.stderr).into_owned();
    // Both should contain the leading "+: expected ..." part.
    assert!(walker_err.contains("+: expected"), "{}", walker_err);
    assert!(vm_err.contains("+: expected"), "{}", vm_err);
    // Critical: the VM error must not include the "+: +:" double prefix
    // we used to produce.
    assert!(
        !vm_err.contains("+: +:"),
        "double prefix in VM error: {}",
        vm_err
    );
}

#[test]
fn vm_call_to_non_procedure_has_source_span() {
    let out = cli()
        .args(["--tier", "vm", "-e", "(42 1 2)"])
        .output()
        .expect("vm");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("non-procedure"), "{}", err);
    assert!(err.contains(":1:"), "should have span: {}", err);
}

#[test]
fn vm_undefined_variable_has_source_span() {
    // Both tiers should report a source location for an undefined-variable
    // error. The VM tier didn't carry source spans through the bytecode
    // before this iteration.
    let walker_out = cli().args(["-e", "(foo 1 2)"]).output().expect("walker");
    let walker_err = String::from_utf8_lossy(&walker_out.stderr).into_owned();
    assert!(
        walker_err.contains("undefined variable: foo"),
        "{}",
        walker_err
    );
    assert!(walker_err.contains(":1:"), "{}", walker_err);

    let vm_out = cli()
        .args(["--tier", "vm", "-e", "(foo 1 2)"])
        .output()
        .expect("vm");
    let vm_err = String::from_utf8_lossy(&vm_out.stderr).into_owned();
    assert!(vm_err.contains("undefined variable: foo"), "{}", vm_err);
    // Regression guard for the new bytecode-level span tracking.
    assert!(
        vm_err.contains(":1:"),
        "VM error should include line:col span: {}",
        vm_err
    );
}
