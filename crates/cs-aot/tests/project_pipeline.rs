//! End-to-end project-emit + cargo-build + run test for M10 Track A
//! iter 3. Validates the whole-program glue: a `cs-rir::Function`
//! list goes in, a self-contained native binary comes out, the
//! binary runs and produces the expected output.
//!
//! Self-recursive `factorial` is the canary because it exercises
//! Branch, Eq, Mul, Sub, LoadConst, and CallSelf in one tight loop.
//! If this test passes, the AOT pipeline can handle any other
//! numeric kernel built from the iter-3 supported set.

use std::path::PathBuf;
use std::process::Command;

use cs_aot::project::{emit_project, ProjectOptions};
use cs_aot::EmitMode;
use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};

/// (define (factorial n)
///   (if (= n 0) 1 (* n (factorial (- n 1)))))
///
/// RIR (loop+match shape, all SSA):
///   block 0:
///     v1 = 0
///     v2 = Eq(v0, v1)              ; n == 0
///     Branch(v2, then=1, else=2)
///   block 1 (base):
///     v3 = 1
///     Return v3
///   block 2 (recurse):
///     v4 = 1
///     v5 = Sub(v0, v4)             ; n - 1
///     v6 = CallSelf(v5)            ; factorial(n - 1)
///     v7 = Mul(v0, v6)             ; n * factorial(n - 1)
///     Return v7
fn factorial_function() -> Function {
    let mut f = Function::new("factorial");
    f.params.push((Value(0), Type::Fixnum));
    f.return_type = Type::Fixnum;
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(0)),
            Inst::Eq(Value(2), Value(0), Value(1)),
        ],
        terminator: Term::Branch(Value(2), BlockId(1), BlockId(2)),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![Inst::LoadConst(Value(3), Const::Fixnum(1))],
        terminator: Term::Return(Value(3)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(4), Const::Fixnum(1)),
            Inst::Sub(Value(5), Value(0), Value(4)),
            Inst::CallSelf(Value(6), vec![Value(5)]),
            Inst::Mul(Value(7), Value(0), Value(6)),
        ],
        terminator: Term::Return(Value(7)),
    });
    f
}

fn cs_vm_workspace_path() -> PathBuf {
    let cs_aot_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cs_aot_manifest
        .parent()
        .expect("cs-aot crate has crates/ parent")
        .join("cs-vm")
}

fn workspace_target_dir() -> PathBuf {
    let cs_aot_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    cs_aot_manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root resolves")
        .join("target/aot-project-tests")
}

/// Build the emitted project via cargo and return the binary path.
/// Shares CARGO_TARGET_DIR across tests so cs-vm + deps cache.
fn cargo_build(project_dir: &PathBuf, package_name: &str) -> PathBuf {
    let target_dir = workspace_target_dir();
    let output = Command::new("cargo")
        .current_dir(project_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg(package_name)
        .arg("--offline")
        .output()
        .expect("cargo executes");
    assert!(
        output.status.success(),
        "cargo build failed for {project_dir:?}:\n--- stderr ---\n{}\n--- stdout ---\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    target_dir.join(format!("release/{package_name}"))
}

fn run_with_arg(bin: &PathBuf, n: i64) -> i64 {
    let out = Command::new(bin)
        .arg(n.to_string())
        .output()
        .expect("binary executes");
    assert!(
        out.status.success(),
        "binary exited non-zero for n={n}: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("stdout utf8")
        .trim()
        .parse::<i64>()
        .expect("stdout parses as i64")
}

#[test]
fn factorial_nb_compiles_and_runs() {
    // Unique tmp dir keyed off the test name so concurrent test
    // runs don't trample each other (same trick as iter 2b's NB
    // tests; the silent stale-artifact bug was painful enough to
    // codify here too).
    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-project-factorial-nb-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);

    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: "aot_factorial_nb".to_string(),
        entry_fn_name: "factorial".to_string(),
        cs_vm_path: Some(cs_vm_workspace_path()),
    };

    let emitted =
        emit_project(&[factorial_function()], &tmpdir, &opts).expect("emit_project succeeds");

    // Sanity: the project dir exists with the expected layout.
    assert!(emitted.project_dir.join("Cargo.toml").exists());
    assert!(emitted.project_dir.join("src/main.rs").exists());

    let bin = cargo_build(&emitted.project_dir, &opts.package_name);

    // Self-evidently correct factorials.
    assert_eq!(run_with_arg(&bin, 0), 1);
    assert_eq!(run_with_arg(&bin, 1), 1);
    assert_eq!(run_with_arg(&bin, 5), 120);
    assert_eq!(run_with_arg(&bin, 10), 3628800);
    // 12! = 479001600 — still fits in NB's 47-bit Fixnum range
    // (max ≈ 1.4e14). Going higher would tag-overflow into
    // GC-allocated Fixnum, which the iter-3 emitter doesn't yet
    // handle in the main shim's `as_fixnum().expect(...)`.
    assert_eq!(run_with_arg(&bin, 12), 479001600);
}

#[test]
fn factorial_rawi64_compiles_and_runs() {
    // RawI64 mode lower-bound coverage: same kernel, no cs-vm dep.
    // Confirms the project emitter handles the unsafe-free path
    // and that CallSelf threads i64 through Rust's extern "C" ABI.
    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-project-factorial-raw-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);

    let opts = ProjectOptions {
        mode: EmitMode::RawI64,
        package_name: "aot_factorial_raw".to_string(),
        entry_fn_name: "factorial".to_string(),
        cs_vm_path: None,
    };

    let emitted =
        emit_project(&[factorial_function()], &tmpdir, &opts).expect("emit_project succeeds");
    let bin = cargo_build(&emitted.project_dir, &opts.package_name);

    assert_eq!(run_with_arg(&bin, 0), 1);
    assert_eq!(run_with_arg(&bin, 5), 120);
    assert_eq!(run_with_arg(&bin, 10), 3628800);
}
