//! AOT **level 3** (toolchain-free object backend) end-to-end.
//!
//! Compiles a self-recursive function to a native binary via the
//! cranelift-object backend + the system `cc`, linking against the prebuilt
//! `libcs_aot_rt.a` archive — **no rustc/cargo at AOT time** — and checks
//! the binary runs and returns the right answer.
//!
//! Forces the object path with `CRABSCHEME_AOT_FORCE_OBJECT=1` so it
//! exercises L3 even on machines that have a Rust toolchain (where the CLI
//! would otherwise pick L1). Skips gracefully when `cc` isn't available.
//!
//! The shared Cranelift lowering is already validated by `jit_conformance`
//! and `diff_aot_vs_jit`; this harness specifically covers the object-emit
//! + `cc`-link + run chain that's unique to L3.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root resolves")
        .to_path_buf()
}

fn cc_available() -> bool {
    Command::new("cc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build `pkg` (release) if `artifact` is missing. Returns false on build
/// failure so the caller can skip rather than hard-fail.
fn ensure_built(root: &Path, artifact: &Path, pkg: &str) -> bool {
    if artifact.exists() {
        return true;
    }
    let ok = Command::new("cargo")
        .current_dir(root)
        .args(["build", "--release", "-p", pkg])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok && artifact.exists()
}

/// Resolve (crabscheme binary, archive) for the release profile, building
/// either if needed. `None` means the test should skip (cc/build absent).
fn setup() -> Option<(PathBuf, PathBuf)> {
    if !cc_available() {
        eprintln!("aot_object_l3: skipping — no `cc` on PATH");
        return None;
    }
    let root = workspace_root();
    let bin = root.join("target/release/crabscheme");
    let archive = root.join("target/release/libcs_aot_rt.a");
    if !ensure_built(&root, &bin, "cs-cli") {
        eprintln!("aot_object_l3: skipping — could not build cs-cli");
        return None;
    }
    if !ensure_built(&root, &archive, "cs-aot-rt") {
        eprintln!("aot_object_l3: skipping — could not build cs-aot-rt archive");
        return None;
    }
    Some((bin, archive))
}

/// Compile `src`'s `entry` to a native binary via the L3 object path, run it
/// with `args`, and return trimmed stdout.
fn run_via_object(bin: &Path, src: &str, entry: &str, args: &[&str]) -> String {
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("cs-aot-l3-{entry}-{pid}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmpdir");
    let src_path = tmp.join("input.scm");
    std::fs::write(&src_path, src).expect("write src");
    let out_bin = tmp.join(entry);

    let out = Command::new(bin)
        .env("CRABSCHEME_AOT_FORCE_OBJECT", "1")
        .arg("aot")
        .arg(&src_path)
        .arg("--entry")
        .arg(entry)
        .arg("-o")
        .arg(&out_bin)
        .arg("--build")
        .output()
        .expect("crabscheme aot executes");
    assert!(
        out.status.success(),
        "crabscheme aot (L3) failed for `{entry}`:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let run = Command::new(&out_bin)
        .args(args)
        .output()
        .expect("L3 binary executes");
    assert!(
        run.status.success(),
        "L3 binary failed for `{entry}` {args:?}:\nstderr:\n{}",
        String::from_utf8_lossy(&run.stderr),
    );
    String::from_utf8_lossy(&run.stdout).trim().to_string()
}

#[test]
fn l3_fib_25_runs_without_toolchain() {
    let Some((bin, _archive)) = setup() else {
        return;
    };
    let got = run_via_object(
        &bin,
        "(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))",
        "fib",
        &["25"],
    );
    assert_eq!(got, "75025");
}

#[test]
fn l3_ack_3_6_runs_without_toolchain() {
    let Some((bin, _archive)) = setup() else {
        return;
    };
    let got = run_via_object(
        &bin,
        "(define (ack m n) (if (= m 0) (+ n 1) (if (= n 0) (ack (- m 1) 1) (ack (- m 1) (ack m (- n 1))))))",
        "ack",
        &["3", "6"],
    );
    assert_eq!(got, "509");
}

#[test]
fn l3_declines_cross_function_program() {
    let Some((bin, _archive)) = setup() else {
        return;
    };
    let pid = std::process::id();
    let tmp = std::env::temp_dir().join(format!("cs-aot-l3-decline-{pid}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("create tmpdir");
    let src_path = tmp.join("cross.scm");
    std::fs::write(
        &src_path,
        "(define (helper n) (* n 2))\n(define (main n) (+ (helper n) 1))\n",
    )
    .expect("write src");

    let out = Command::new(&bin)
        .env("CRABSCHEME_AOT_FORCE_OBJECT", "1")
        .arg("aot")
        .arg(&src_path)
        .arg("--entry")
        .arg("main")
        .arg("-o")
        .arg(tmp.join("main"))
        .arg("--build")
        .output()
        .expect("crabscheme aot executes");
    // Cross-function programs aren't linkable by L3 yet — must decline with
    // a non-zero exit and a pointer to L1, not silently emit a broken binary.
    assert_eq!(out.status.code(), Some(4), "expected exit 4 (decline)");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("calls other functions"),
        "decline message missing; stderr:\n{stderr}"
    );
}
