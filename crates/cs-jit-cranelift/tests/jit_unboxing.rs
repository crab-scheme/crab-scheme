//! ADR 0020 Strategy C — execution tests for speculative Fixnum
//! unboxing in the uniform-NB tier.
//!
//! These build a hand-constructed RIR whose params are `Type::Fixnum`,
//! compile it via `compile_uniform_nb`, then call the native pointer
//! and check the NB-encoded result. The body's arithmetic lowers
//! through the unboxed raw-i64 lane (no per-op tag check / re-encode);
//! a correct result end-to-end proves the unbox-at-entry, raw arith,
//! and box-at-return path agree with the boxed semantics.

use std::mem::transmute;

use cs_jit_cranelift::Lowerer;
use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};
use cs_vm::vm::NanboxValue;

/// `(define (ss a b) (+ (* a a) (* b b)))` with both params Fixnum.
/// Every operand is in the raw lane: the two `Mul`s and the `Add` all
/// take the unboxed path; the result is boxed once at `Return`.
fn sum_of_squares() -> Function {
    let mut f = Function::new("ss_unbox");
    f.params.push((Value(0), Type::Fixnum)); // a
    f.params.push((Value(1), Type::Fixnum)); // b
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::Mul(Value(2), Value(0), Value(0)), // a*a
            Inst::Mul(Value(3), Value(1), Value(1)), // b*b
            Inst::Add(Value(4), Value(2), Value(3)), // +
        ],
        terminator: Term::Return(Value(4)),
    });
    f
}

fn call2(ptr: *const u8, a: i64, b: i64) -> cs_core::Value {
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };
    let av = NanboxValue::fixnum(a).into_raw();
    let bv = NanboxValue::fixnum(b).into_raw();
    let r = func(av, bv);
    unsafe { NanboxValue(r).to_value() }
}

/// Call a 1-arg uniform-NB outer trampoline: encode the arg as an NB
/// Fixnum, invoke, decode the NB result.
fn call1(ptr: *const u8, a: i64) -> cs_core::Value {
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
    let r = func(NanboxValue::fixnum(a).into_raw());
    unsafe { NanboxValue(r).to_value() }
}

fn as_fixnum(v: cs_core::Value, ctx: &str) -> i64 {
    match v {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => n,
        other => panic!("{ctx}: expected Fixnum, got {other:?}"),
    }
}

/// `(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))`
/// in the exact shape the bytecode→RIR translator emits: the two arms
/// `Jump` to a join block with a Fixnum param, then `Return(param)`.
/// The two `CallSelf`s are non-tail (results feed `Add`). This is the
/// raw-Fixnum self-call ABI target (#50): pre-iter-3 `compile_uniform_nb`
/// rejected it ("non-tail CallSelf"); now it compiles and recursion
/// passes raw i64 end-to-end, boxing only at the trampoline perimeter.
fn fib_join() -> Function {
    let mut f = Function::new("fib_raw_abi");
    f.params.push((Value(0), Type::Fixnum)); // n
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(2)),
            Inst::Lt(Value(2), Value(0), Value(1)),
        ],
        terminator: Term::Branch(Value(2), BlockId(1), BlockId(2), Vec::new()),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![],
        terminator: Term::Jump(BlockId(3), vec![Value(0)]),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(4), Const::Fixnum(1)),
            Inst::Sub(Value(5), Value(0), Value(4)),
            Inst::CallSelf(Value(6), vec![Value(5)]),
            Inst::LoadConst(Value(7), Const::Fixnum(2)),
            Inst::Sub(Value(8), Value(0), Value(7)),
            Inst::CallSelf(Value(9), vec![Value(8)]),
            Inst::Add(Value(10), Value(6), Value(9)),
        ],
        terminator: Term::Jump(BlockId(3), vec![Value(10)]),
    });
    f.blocks.push(Block {
        id: BlockId(3),
        params: vec![(Value(3), Type::Fixnum)],
        insts: vec![],
        terminator: Term::Return(Value(3)),
    });
    f
}

#[test]
fn raw_abi_fib_compiles_and_runs_on_uniform_nb() {
    let f = fib_join();
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    // Pre-iter-3 this returned Err(Unsupported "non-tail CallSelf"); the
    // raw-Fixnum self-call ABI now admits it on uniform-NB.
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("fib must compile on uniform-NB via the raw-Fixnum ABI");
    // Deep recursion (fib(20) makes 13529 self-calls) exercises the raw
    // call boundary repeatedly; results must match the boxed semantics.
    for (n, expected) in [(0, 0), (1, 1), (2, 1), (5, 5), (10, 55), (20, 6765)] {
        assert_eq!(as_fixnum(call1(ptr, n), &format!("fib({n})")), expected);
    }
}

/// `(define (sum n acc) (if (= n 0) acc (sum (- n 1) (+ acc n))))` — a
/// *tail*-recursive Fixnum accumulator. The self-call is in tail
/// position, so the raw ABI lowers it to `return_call` (CallConv::Tail)
/// with raw args, returning raw. Exercises the tail self-call path of
/// the raw ABI (distinct from fib's non-tail `call`).
fn sum_tail() -> Function {
    let mut f = Function::new("sum_raw_abi");
    f.params.push((Value(0), Type::Fixnum)); // n
    f.params.push((Value(1), Type::Fixnum)); // acc
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(2), Const::Fixnum(0)),
            Inst::Eq(Value(3), Value(0), Value(2)),
        ],
        terminator: Term::Branch(Value(3), BlockId(1), BlockId(2), Vec::new()),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![],
        terminator: Term::Return(Value(1)), // acc
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(4), Const::Fixnum(1)),
            Inst::Sub(Value(5), Value(0), Value(4)), // n-1
            Inst::Add(Value(6), Value(1), Value(0)), // acc+n
            Inst::CallSelf(Value(7), vec![Value(5), Value(6)]),
        ],
        terminator: Term::Return(Value(7)),
    });
    f
}

#[test]
fn raw_abi_tail_sum_compiles_and_runs_on_uniform_nb() {
    let f = sum_tail();
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("tail-recursive sum must compile on uniform-NB via the raw ABI");
    // sum(n, 0) = n + (n-1) + ... + 1. Large n exercises deep tail
    // recursion in constant host stack (return_call).
    for (n, expected) in [(0, 0), (1, 1), (10, 55), (100, 5050), (1000, 500500)] {
        assert_eq!(as_fixnum(call2(ptr, n, 0), &format!("sum({n})")), expected);
    }
}

#[test]
fn unboxed_sum_of_squares_is_correct() {
    let f = sum_of_squares();
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("sum-of-squares must compile on uniform-NB");
    for (a, b) in [(3, 4), (0, 0), (123, 456), (-7, 5), (1000, 2000)] {
        let v = call2(ptr, a, b);
        let expected = a * a + b * b;
        match v {
            cs_core::Value::Number(cs_core::Number::Fixnum(n)) => {
                assert_eq!(n, expected, "ss({a},{b})");
            }
            other => panic!("ss({a},{b}): expected Fixnum({expected}), got {other:?}"),
        }
    }
}

#[test]
fn unboxed_mul_overflow_requests_deopt() {
    // (sq x) = x*x. A large x overflows the 47-bit Fixnum range, so the
    // unboxed Mul must set the deopt sentinel (cleared here via
    // jit_take_deopt). The returned placeholder is discarded by the
    // dispatcher in production; here we just assert the sentinel fired.
    let mut f = Function::new("sq_unbox");
    f.params.push((Value(0), Type::Fixnum)); // x
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![Inst::Mul(Value(1), Value(0), Value(0))],
        terminator: Term::Return(Value(1)),
    });
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer.compile_uniform_nb(&f).expect("compile");
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    // Small input: no overflow, sentinel stays clear, result correct.
    let _ = cs_vm::vm::jit_take_deopt();
    let small = func(NanboxValue::fixnum(1000).into_raw());
    assert_eq!(
        cs_vm::vm::jit_take_deopt(),
        0,
        "no deopt expected for sq(1000)"
    );
    match unsafe { NanboxValue(small).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 1_000_000),
        other => panic!("expected Fixnum(1000000), got {other:?}"),
    }

    // Overflowing input: 10_000_000^2 = 1e14 > 47-bit max (~7.04e13).
    let _ = func(NanboxValue::fixnum(10_000_000).into_raw());
    assert_eq!(
        cs_vm::vm::jit_take_deopt(),
        cs_vm::vm::DEOPT_REASON_FIXNUM_OVERFLOW,
        "overflowing unboxed Mul must request a deopt"
    );
}

#[test]
fn not_boolean_compiles_and_flips_on_uniform_nb() {
    // `(define (f n) (if (not (< n 5)) 1 0))` — exercises uniform-NB's
    // NotBoolean lowering (#50: needed so tak's `(if (not (< y x)) ...)`
    // leaves the pure-fixnum tier). f(n) = (n >= 5) ? 1 : 0.
    let mut f = Function::new("not_bool");
    f.params.push((Value(0), Type::Fixnum)); // n
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(5)),
            Inst::Lt(Value(2), Value(0), Value(1)), // n < 5
            Inst::NotBoolean(Value(3), Value(2)),   // not (n < 5)
        ],
        terminator: Term::Branch(Value(3), BlockId(1), BlockId(2), Vec::new()),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![Inst::LoadConst(Value(4), Const::Fixnum(1))],
        terminator: Term::Return(Value(4)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![Inst::LoadConst(Value(5), Const::Fixnum(0))],
        terminator: Term::Return(Value(5)),
    });
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("NotBoolean body must compile on uniform-NB");
    for (n, expected) in [(0, 0), (4, 0), (5, 1), (10, 1)] {
        assert_eq!(as_fixnum(call1(ptr, n), &format!("f({n})")), expected);
    }
}
