//! Headless CLI exit-gate test (Phase 6 iter 6.H1): run the real
//! `crabscheme-lsp <subcommand>` binary and assert its JSON output and
//! exit codes — the contract a coding harness shells out to.

use std::path::Path;
use std::process::Command;

/// Run `crabscheme-lsp ARGS`; return `(stdout, exit_code)`.
fn run(args: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_crabscheme-lsp"))
        .args(args)
        .output()
        .expect("spawn crabscheme-lsp");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn write(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p.display().to_string()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "cs-lsp-cli-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn check_valid_then_invalid() {
    let dir = temp_dir("check");
    let ok = write(&dir, "ok.scm", "(define x 1)");
    let (out, code) = run(&["check", &ok]);
    assert_eq!(code, 0, "valid file should exit 0");
    assert_eq!(out.trim(), "[]", "valid file should report no diagnostics");

    let bad = write(&dir, "bad.scm", "(+ 1 2");
    let (out, code) = run(&["check", &bad]);
    assert_eq!(code, 1, "file with diagnostics should exit 1");
    assert!(out.contains("\"severity\": \"error\""), "out: {out}");
    assert!(out.contains("\"line\": 1"), "should be 1-based: {out}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn symbols_def_refs_hover_fmt() {
    let dir = temp_dir("feat");
    let f = write(&dir, "a.scm", "(define (f x) x)\n(f 1)\n(f 2)");

    let (sym, _) = run(&["symbols", &f]);
    assert!(sym.contains("\"name\": \"f\""), "symbols: {sym}");
    assert!(sym.contains("\"kind\": \"function\""), "symbols: {sym}");

    // Definition of the use of `f` at line 2 col 2 → its define on line 1.
    let (def, _) = run(&["def", &f, "--line", "2", "--col", "2"]);
    assert!(def.contains("\"line\": 1"), "def: {def}");

    // References (declaration + two uses) → three locations.
    let (refs, _) = run(&["refs", &f, "--line", "2", "--col", "2"]);
    assert_eq!(refs.matches("\"start\"").count(), 3, "refs: {refs}");

    // Hover on a builtin.
    let g = write(&dir, "b.scm", "(cons 1 2)");
    let (hov, _) = run(&["hover", &g, "--line", "1", "--col", "2"]);
    assert!(hov.contains("cons"), "hover: {hov}");

    // Format reindents and prints to stdout (no --write).
    let messy = write(&dir, "m.scm", "(define (g x)\n(* x\n2))");
    let (fmt, code) = run(&["fmt", &messy]);
    assert_eq!(code, 0);
    assert!(fmt.contains("    2))"), "fmt did not reindent: {fmt:?}");
    // Without --write the file is untouched.
    assert_eq!(
        std::fs::read_to_string(&messy).unwrap(),
        "(define (g x)\n(* x\n2))",
        "fmt without --write must not modify the file"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn fmt_write_rewrites_file() {
    let dir = temp_dir("fmtw");
    let messy = write(&dir, "m.scm", "(a\n(b))");
    let (_, code) = run(&["fmt", &messy, "--write"]);
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&messy).unwrap(),
        "(a\n  (b))",
        "--write should reindent the file in place"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn workspace_symbols_across_files() {
    let dir = temp_dir("ws");
    write(&dir, "a.scm", "(define (alpha x) x)");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    write(&dir, "sub/b.scm", "(define beta 2)");

    let root = dir.display().to_string();
    let (all, code) = run(&["workspace-symbols", &root, "--query", "alpha"]);
    assert_eq!(code, 0);
    assert!(all.contains("\"name\": \"alpha\""), "ws: {all}");
    assert!(
        !all.contains("\"name\": \"beta\""),
        "query should filter: {all}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_file_exits_2() {
    let (_, code) = run(&["check", "/no/such/file.scm"]);
    assert_eq!(code, 2, "missing file should exit 2");
}
