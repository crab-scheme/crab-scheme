//! End-to-end test: emit Rust source, compile with rustc, run, and
//! assert the runtime behavior matches what we'd compute by hand.
//!
//! Iter 1 of M10 Track A — exercises the full AOT pipeline at the
//! granularity of a single function. Iter 3's whole-program glue
//! wires this into a cargo project + main entry point; this test
//! exercises just the rustc invocation on the emitted source.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use cs_aot::{emit, emit_with, nb_helpers_source, EmitMode};
use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};

/// Build a runnable test binary from the emitted source.
///
/// Returns the binary path. The binary, when run with `cargo run
/// -- ARGS...`, calls the AOT'd function with the parsed i64 args
/// and prints the i64 result to stdout.
fn build_aot_binary(emitted: &str, fn_name: &str, n_params: usize) -> PathBuf {
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-test-{fn_name}-{}", std::process::id()));
    fs::create_dir_all(&tmpdir).expect("create tmpdir");

    // Write the AOT'd source + a small main that parses args and
    // calls the function.
    let mut src = String::from("#![allow(unused)]\n");
    src.push_str(emitted);
    src.push('\n');
    src.push_str("fn main() {\n");
    src.push_str("    let args: Vec<String> = std::env::args().collect();\n");
    let mut call_args = String::new();
    // RC3 iter 2.12 — every AOT'd fn takes __self_handle as first
    // arg. The test main passes 0 (unused here since these tests
    // exercise simple numeric kernels with no inner closures).
    call_args.push_str("0i64");
    for i in 0..n_params {
        call_args.push_str(", ");
        call_args.push_str(&format!("args[{}].parse::<i64>().unwrap()", i + 1));
    }
    src.push_str(&format!("    let result: i64 = {fn_name}({call_args});\n"));
    src.push_str("    println!(\"{}\", result);\n");
    src.push_str("}\n");

    let src_path = tmpdir.join("main.rs");
    fs::write(&src_path, &src).expect("write src");

    let bin_path = tmpdir.join("aot_bin");
    let status = Command::new("rustc")
        .arg("--edition=2021")
        .arg("-O")
        .arg(&src_path)
        .arg("-o")
        .arg(&bin_path)
        .status()
        .expect("rustc executes");
    assert!(
        status.success(),
        "rustc failed on AOT'd source:\n---\n{src}---"
    );
    bin_path
}

fn run_aot_binary(bin: &PathBuf, args: &[i64]) -> i64 {
    let mut cmd = Command::new(bin);
    for a in args {
        cmd.arg(a.to_string());
    }
    let out = cmd.output().expect("binary executes");
    assert!(out.status.success(), "binary exited non-zero: {out:?}");
    let s = String::from_utf8(out.stdout).expect("stdout is utf8");
    s.trim().parse::<i64>().expect("stdout parses as i64")
}

#[test]
fn aot_sq_runs_correctly() {
    // (define (sq x) (* x x))
    let mut f = Function::new("sq");
    f.params.push((Value(0), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![Inst::Mul(Value(1), Value(0), Value(0))],
        terminator: Term::Return(Value(1)),
    });

    let src = emit(&f).unwrap();
    let bin = build_aot_binary(&src, "sq", 1);

    assert_eq!(run_aot_binary(&bin, &[0]), 0);
    assert_eq!(run_aot_binary(&bin, &[7]), 49);
    assert_eq!(run_aot_binary(&bin, &[-3]), 9);
    assert_eq!(run_aot_binary(&bin, &[12345]), 152399025);
}

#[test]
fn aot_add3_runs_correctly() {
    // (define (add3 a b c) (+ (+ a b) c))
    let mut f = Function::new("add3");
    f.params.push((Value(0), Type::Fixnum));
    f.params.push((Value(1), Type::Fixnum));
    f.params.push((Value(2), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::Add(Value(3), Value(0), Value(1)),
            Inst::Add(Value(4), Value(3), Value(2)),
        ],
        terminator: Term::Return(Value(4)),
    });

    let src = emit(&f).unwrap();
    let bin = build_aot_binary(&src, "add3", 3);

    assert_eq!(run_aot_binary(&bin, &[1, 2, 3]), 6);
    assert_eq!(run_aot_binary(&bin, &[10, 20, 30]), 60);
    assert_eq!(run_aot_binary(&bin, &[-5, 5, 100]), 100);
}

#[test]
fn aot_arith_chain_with_const() {
    // (define (q x) (+ (* x 3) 7))
    let mut f = Function::new("q");
    f.params.push((Value(0), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(3)),
            Inst::Mul(Value(2), Value(0), Value(1)),
            Inst::LoadConst(Value(3), Const::Fixnum(7)),
            Inst::Add(Value(4), Value(2), Value(3)),
        ],
        terminator: Term::Return(Value(4)),
    });

    let src = emit(&f).unwrap();
    let bin = build_aot_binary(&src, "q", 1);

    assert_eq!(run_aot_binary(&bin, &[0]), 7);
    assert_eq!(run_aot_binary(&bin, &[10]), 37);
    assert_eq!(run_aot_binary(&bin, &[-2]), 1);
}

#[test]
fn aot_abs_via_branch_runs_correctly() {
    // (define (abs x) (if (< x 0) (- 0 x) x))
    // RIR shape (loop+match):
    //   block 0: v1=0; v2=v0<v1; Branch(v2, then=1, else=2)
    //   block 1 (then): v3=0; v4=v3-v0; Return v4
    //   block 2 (else): Return v0
    let mut f = Function::new("abs");
    f.params.push((Value(0), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(0)),
            Inst::Lt(Value(2), Value(0), Value(1)),
        ],
        terminator: Term::Branch(Value(2), BlockId(1), BlockId(2)),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(3), Const::Fixnum(0)),
            Inst::Sub(Value(4), Value(3), Value(0)),
        ],
        terminator: Term::Return(Value(4)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![],
        terminator: Term::Return(Value(0)),
    });

    let src = emit(&f).unwrap();
    let bin = build_aot_binary(&src, "abs", 1);

    assert_eq!(run_aot_binary(&bin, &[0]), 0);
    assert_eq!(run_aot_binary(&bin, &[5]), 5);
    assert_eq!(run_aot_binary(&bin, &[-5]), 5);
    assert_eq!(run_aot_binary(&bin, &[-12345]), 12345);
    assert_eq!(run_aot_binary(&bin, &[i64::MAX]), i64::MAX);
}

#[test]
fn aot_iterative_sum_loop_via_jump() {
    // Iterative sum 1..n via a back-edge Jump with block params.
    //   (define (sum n)
    //     (let loop ((i 1) (acc 0))
    //       (if (> i n) acc (loop (+ i 1) (+ acc i)))))
    //
    // RIR (using Lt-with-swapped-args since RIR has Lt but not Gt;
    // condition is (i <= n), which we encode as NOT (n < i) — but
    // RIR also has no Not, so we encode "i > n" as Lt(n, i) and put
    // the exit on the then-branch.
    //
    //   block 0 (entry):
    //     v1=1; v2=0;
    //     Jump(block 1, [v1, v2])
    //   block 1 (loop, params i=v3, acc=v4):
    //     v5 = Lt(v0_n, v3_i)         ; n < i, i.e. i > n
    //     Branch(v5, then=2, else=3)
    //   block 2 (exit):
    //     Return v4
    //   block 3 (body):
    //     v6 = 1
    //     v7 = Add(v3_i, v6)         ; i+1
    //     v8 = Add(v4_acc, v3_i)     ; acc+i
    //     Jump(block 1, [v7, v8])
    let mut f = Function::new("sum");
    f.params.push((Value(0), Type::Fixnum)); // n
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(1)),
            Inst::LoadConst(Value(2), Const::Fixnum(0)),
        ],
        terminator: Term::Jump(BlockId(1), vec![Value(1), Value(2)]),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![(Value(3), Type::Fixnum), (Value(4), Type::Fixnum)],
        insts: vec![Inst::Lt(Value(5), Value(0), Value(3))],
        terminator: Term::Branch(Value(5), BlockId(2), BlockId(3)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![],
        terminator: Term::Return(Value(4)),
    });
    f.blocks.push(Block {
        id: BlockId(3),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(6), Const::Fixnum(1)),
            Inst::Add(Value(7), Value(3), Value(6)),
            Inst::Add(Value(8), Value(4), Value(3)),
        ],
        terminator: Term::Jump(BlockId(1), vec![Value(7), Value(8)]),
    });

    let src = emit(&f).unwrap();
    let bin = build_aot_binary(&src, "sum", 1);

    assert_eq!(run_aot_binary(&bin, &[0]), 0);
    assert_eq!(run_aot_binary(&bin, &[1]), 1);
    assert_eq!(run_aot_binary(&bin, &[10]), 55);
    assert_eq!(run_aot_binary(&bin, &[100]), 5050);
    assert_eq!(run_aot_binary(&bin, &[1000]), 500500);
}

// ---------------------------------------------------------------------
// NB-mode end-to-end tests (iter 2b)
//
// NB mode's emitted source references `cs_vm::vm::vm_value_*_nb`.
// We can't link cs-vm with bare `rustc` (it has external crate deps:
// num-bigint, num-rational, etc.), so we build via cargo with a
// tmp project that declares cs-vm as a path dependency. Slower than
// the RawI64 tests but proves the end-to-end NB ABI pipeline.
// ---------------------------------------------------------------------

/// Compile an NB-mode emitted snippet into a runnable binary by
/// spawning a tmp cargo project that links cs-vm.
///
/// The wrapper main encodes each argv arg as an NB Fixnum, calls the
/// AOT'd function (which now operates in NB), decodes the result as
/// a fixnum, and prints it. Round-tripping through cs-vm's actual
/// NanboxValue encode/decode is intentional — it's what catches ABI
/// mismatches between the emitter's `const_to_rust_nb` bit-layout
/// and the runtime's actual NB shape.
fn build_aot_binary_nb(emitted: &str, fn_name: &str, n_params: usize) -> PathBuf {
    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-nb-{fn_name}-{pid}"));
    let _ = fs::remove_dir_all(&tmpdir); // start clean to avoid stale artifacts
    fs::create_dir_all(tmpdir.join("src")).expect("create src");

    // Resolve cs-vm's absolute path relative to cs-aot's manifest.
    // CARGO_MANIFEST_DIR is the cs-aot crate root at test compile time.
    let cs_aot_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cs_vm_path = cs_aot_manifest_dir
        .parent()
        .expect("cs-aot has parent dir (crates/)")
        .join("cs-vm");
    assert!(
        cs_vm_path.exists(),
        "expected cs-vm at {}",
        cs_vm_path.display()
    );

    // Package + bin name must be unique per test, otherwise tests
    // sharing CARGO_TARGET_DIR overwrite each other's `aot_bin` and
    // we end up reading the wrong binary (race + stale-artifact bug
    // that produced silent test failures in the iter-2b NB tests).
    let pkg_name = format!("aot_nb_test_{fn_name}");
    let bin_name = format!("aot_bin_{fn_name}");
    let cargo_toml = format!(
        r#"[package]
name = "{pkg_name}"
version = "0.0.1"
edition = "2021"

[dependencies]
cs-vm = {{ path = "{cs_vm_path}" }}

[[bin]]
name = "{bin_name}"
path = "src/main.rs"

[profile.release]
opt-level = 3
"#,
        cs_vm_path = cs_vm_path.display(),
    );
    fs::write(tmpdir.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let mut src = String::from("#![allow(unused, unused_unsafe)]\n");
    // NB-mode emitted source references `nb_*_inline` helpers; the
    // caller (this test scaffold, and project.rs for project builds)
    // is required to inject them once per translation unit.
    src.push_str(nb_helpers_source());
    src.push_str(emitted);
    src.push('\n');
    src.push_str("use cs_vm::vm::NanboxValue;\n");
    src.push_str("fn main() {\n");
    src.push_str("    let args: Vec<String> = std::env::args().collect();\n");
    // RC3 iter 2.12 — every AOT'd fn takes __self_handle: i64 first.
    let mut call_args = String::from("0i64");
    for i in 0..n_params {
        call_args.push_str(", ");
        call_args.push_str(&format!(
            "NanboxValue::fixnum(args[{}].parse::<i64>().unwrap()).into_raw()",
            i + 1
        ));
    }
    src.push_str(&format!(
        "    let result_nb: i64 = {fn_name}({call_args});\n"
    ));
    src.push_str("    let nb = NanboxValue(result_nb);\n");
    src.push_str("    let n = nb.as_fixnum().expect(\"NB result is a fixnum\");\n");
    src.push_str("    println!(\"{}\", n);\n");
    src.push_str("}\n");
    fs::write(tmpdir.join("src/main.rs"), &src).expect("write main.rs");

    // Share the workspace target dir so cs-vm + deps don't get
    // rebuilt for every NB test. We construct the target path
    // relative to cs-aot's manifest, pointing at the workspace root.
    let workspace_root = cs_aot_manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("cs-aot is two levels below workspace root");
    let target_dir = workspace_root.join("target/aot-nb-tests");

    let output = Command::new("cargo")
        .current_dir(&tmpdir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg(&bin_name)
        .arg("--offline")
        .output()
        .expect("cargo executes");
    assert!(
        output.status.success(),
        "cargo build failed on NB AOT'd source:\n--- src ---\n{}\n--- stderr ---\n{}\n--- stdout ---\n{}",
        src,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );

    target_dir.join(format!("release/{bin_name}"))
}

#[test]
fn aot_sq_nb_runs_correctly() {
    // Same RIR as aot_sq_runs_correctly, but emitted under NB ABI.
    // (* x x) lowers to nb_mul_inline(v0, v0); the helper inlines
    // the Fixnum fast path and falls back to vm_value_mul_nb on a
    // tag miss. Result is an NB Fixnum.
    let mut f = Function::new("sq_nb");
    f.params.push((Value(0), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![Inst::Mul(Value(1), Value(0), Value(0))],
        terminator: Term::Return(Value(1)),
    });

    let src = emit_with(EmitMode::Nb, &f).unwrap();
    assert!(
        src.contains("nb_mul_inline"),
        "NB emit should call nb_mul_inline helper: {src}"
    );
    let bin = build_aot_binary_nb(&src, "sq_nb", 1);

    assert_eq!(run_aot_binary(&bin, &[0]), 0);
    assert_eq!(run_aot_binary(&bin, &[7]), 49);
    assert_eq!(run_aot_binary(&bin, &[-3]), 9);
    assert_eq!(run_aot_binary(&bin, &[12345]), 152399025);
}

/// Flonum kernel canary for RC2 iter C: `(define (energy x y)
/// (+ (* x x) (* y y)))` lowered via FlonumMul + FlonumAdd. Proves
/// the bitcast-f64-and-go pattern works end-to-end through the Nb
/// ABI scaffold + the f64 NB encoding.
fn build_aot_binary_flonum_nb(emitted: &str, fn_name: &str) -> PathBuf {
    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-nb-flonum-{fn_name}-{pid}"));
    let _ = fs::remove_dir_all(&tmpdir);
    fs::create_dir_all(tmpdir.join("src")).expect("create src");

    let cs_aot_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cs_vm_path = cs_aot_manifest_dir.parent().expect("crates/").join("cs-vm");

    let pkg_name = format!("aot_nb_flonum_{fn_name}");
    let bin_name = format!("aot_bin_{fn_name}");
    let cargo_toml = format!(
        r#"[package]
name = "{pkg_name}"
version = "0.0.1"
edition = "2021"

[dependencies]
cs-vm = {{ path = "{cs_vm_path}" }}

[[bin]]
name = "{bin_name}"
path = "src/main.rs"

[profile.release]
opt-level = 3
"#,
        cs_vm_path = cs_vm_path.display(),
    );
    fs::write(tmpdir.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

    let mut src = String::from("#![allow(unused, unused_unsafe)]\n");
    src.push_str(nb_helpers_source());
    src.push_str(emitted);
    src.push('\n');
    src.push_str("use cs_vm::vm::NanboxValue;\n");
    src.push_str("fn main() {\n");
    src.push_str("    let args: Vec<String> = std::env::args().collect();\n");
    src.push_str("    let x = NanboxValue::flonum(args[1].parse::<f64>().unwrap()).into_raw();\n");
    src.push_str("    let y = NanboxValue::flonum(args[2].parse::<f64>().unwrap()).into_raw();\n");
    // RC3 iter 2.12 — every AOT'd fn takes __self_handle: i64 first.
    src.push_str(&format!("    let r: i64 = {fn_name}(0i64, x, y);\n"));
    src.push_str("    let f = NanboxValue(r).as_flonum().expect(\"result is a Flonum\");\n");
    src.push_str("    println!(\"{}\", f);\n");
    src.push_str("}\n");
    fs::write(tmpdir.join("src/main.rs"), &src).expect("write main.rs");

    let workspace_root = cs_aot_manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root");
    let target_dir = workspace_root.join("target/aot-nb-tests");
    let output = Command::new("cargo")
        .current_dir(&tmpdir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg(&bin_name)
        .arg("--offline")
        .output()
        .expect("cargo executes");
    assert!(
        output.status.success(),
        "cargo build failed for flonum NB AOT:\n--- src ---\n{}\n--- stderr ---\n{}\n--- stdout ---\n{}",
        src,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    target_dir.join(format!("release/{bin_name}"))
}

#[test]
fn aot_energy_flonum_nb() {
    // (define (energy x y) (+ (* x x) (* y y))) — Flonum.
    // Exercises FlonumMul + FlonumAdd via the Nb-mode bitcast-and-
    // go path. Result: energy(3.0, 4.0) = 25.0.
    let mut f = Function::new("energy");
    f.params.push((Value(0), Type::Flonum));
    f.params.push((Value(1), Type::Flonum));
    f.return_type = Type::Flonum;
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::FlonumMul(Value(2), Value(0), Value(0)), // x²
            Inst::FlonumMul(Value(3), Value(1), Value(1)), // y²
            Inst::FlonumAdd(Value(4), Value(2), Value(3)), // x² + y²
        ],
        terminator: Term::Return(Value(4)),
    });

    let src = emit_with(EmitMode::Nb, &f).unwrap();
    let bin = build_aot_binary_flonum_nb(&src, "energy");

    let run = |x: f64, y: f64| -> f64 {
        let out = Command::new(&bin)
            .arg(format!("{x}"))
            .arg(format!("{y}"))
            .output()
            .expect("binary executes");
        assert!(out.status.success(), "energy binary failed: {out:?}");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .parse::<f64>()
            .expect("f64 parse")
    };

    assert_eq!(run(3.0, 4.0), 25.0);
    assert_eq!(run(0.0, 0.0), 0.0);
    assert_eq!(run(1.5, 2.5), 1.5 * 1.5 + 2.5 * 2.5);
    assert_eq!(run(-3.0, 4.0), 25.0);
}

#[test]
fn aot_distance_flonum_nb() {
    // (define (distance x y) (sqrt (+ (* x x) (* y y))))
    // RC2 iter D — exercises FlonumSqrt on top of iter C's
    // FlonumMul + FlonumAdd. Pythagorean distance is the
    // textbook canary; (3.0, 4.0) → 5.0 is exact in f64.
    let mut f = Function::new("distance");
    f.params.push((Value(0), Type::Flonum));
    f.params.push((Value(1), Type::Flonum));
    f.return_type = Type::Flonum;
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::FlonumMul(Value(2), Value(0), Value(0)),
            Inst::FlonumMul(Value(3), Value(1), Value(1)),
            Inst::FlonumAdd(Value(4), Value(2), Value(3)),
            Inst::FlonumSqrt(Value(5), Value(4)),
        ],
        terminator: Term::Return(Value(5)),
    });

    let src = emit_with(EmitMode::Nb, &f).unwrap();
    let bin = build_aot_binary_flonum_nb(&src, "distance");

    let run = |x: f64, y: f64| -> f64 {
        let out = Command::new(&bin)
            .arg(format!("{x}"))
            .arg(format!("{y}"))
            .output()
            .expect("binary executes");
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .parse::<f64>()
            .expect("f64 parse")
    };

    assert_eq!(run(3.0, 4.0), 5.0);
    assert_eq!(run(0.0, 0.0), 0.0);
    assert_eq!(run(5.0, 12.0), 13.0);
    assert_eq!(run(-3.0, -4.0), 5.0);
}

#[test]
fn aot_iterative_sum_nb_via_jump() {
    // Same iterative sum as the RawI64 variant — but emitted under
    // NB ABI. This proves loop+match + Jump-with-args + NB
    // arithmetic helpers all compose correctly. Catches any tag
    // smearing that would corrupt the back-edge param assignment.
    let mut f = Function::new("sum_nb");
    f.params.push((Value(0), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(1)),
            Inst::LoadConst(Value(2), Const::Fixnum(0)),
        ],
        terminator: Term::Jump(BlockId(1), vec![Value(1), Value(2)]),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![(Value(3), Type::Fixnum), (Value(4), Type::Fixnum)],
        insts: vec![Inst::Lt(Value(5), Value(0), Value(3))],
        terminator: Term::Branch(Value(5), BlockId(2), BlockId(3)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![],
        terminator: Term::Return(Value(4)),
    });
    f.blocks.push(Block {
        id: BlockId(3),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(6), Const::Fixnum(1)),
            Inst::Add(Value(7), Value(3), Value(6)),
            Inst::Add(Value(8), Value(4), Value(3)),
        ],
        terminator: Term::Jump(BlockId(1), vec![Value(7), Value(8)]),
    });

    let src = emit_with(EmitMode::Nb, &f).unwrap();
    let bin = build_aot_binary_nb(&src, "sum_nb", 1);

    assert_eq!(run_aot_binary(&bin, &[0]), 0);
    assert_eq!(run_aot_binary(&bin, &[1]), 1);
    assert_eq!(run_aot_binary(&bin, &[10]), 55);
    assert_eq!(run_aot_binary(&bin, &[100]), 5050);
}

#[test]
fn aot_wrapping_arith_matches_jit_semantics() {
    // Verify that emitted Add uses wrapping_add (not checked), so
    // overflow matches the JIT's underlying i64 ops rather than
    // panicking in debug mode.
    //
    // (define (saturating-overflow x) (+ x i64::MAX))
    // For x=1, JIT would wrap to i64::MIN.
    let mut f = Function::new("wraps");
    f.params.push((Value(0), Type::Fixnum));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(i64::MAX)),
            Inst::Add(Value(2), Value(0), Value(1)),
        ],
        terminator: Term::Return(Value(2)),
    });

    let src = emit(&f).unwrap();
    let bin = build_aot_binary(&src, "wraps", 1);

    // 1 + i64::MAX wraps to i64::MIN under two's-complement.
    assert_eq!(run_aot_binary(&bin, &[1]), i64::MIN);
    assert_eq!(run_aot_binary(&bin, &[0]), i64::MAX);
}
