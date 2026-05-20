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
