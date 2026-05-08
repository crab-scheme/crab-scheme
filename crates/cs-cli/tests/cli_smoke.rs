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
