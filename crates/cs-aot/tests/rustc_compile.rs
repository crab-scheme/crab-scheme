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

use cs_aot::emit;
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
    for i in 0..n_params {
        if i > 0 {
            call_args.push_str(", ");
        }
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
