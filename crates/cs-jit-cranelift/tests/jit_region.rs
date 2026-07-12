//! #51 escape analysis — execution tests for the uniform-NB
//! `Inst::ConsRegion` lowering (the layer-3 region-allocating cons).
//!
//! The whole file is gated on `regions`: without it, `ConsRegion` is
//! not in the uniform-NB prewalk's supported set (declines → VM) and
//! the `vm_alloc_pair_region` helper isn't compiled.
//!
//! These bare cs-jit tests run with no region scope established (no
//! cs-runtime resolver), so `vm_alloc_pair_region_gc` takes its
//! documented Rc-heap fallback. That still proves the JIT lowering is
//! correct end-to-end: a pair is allocated and its slots read back.
//! The region-allocation path (with a live `RegionScope`) is covered
//! by the cs-runtime integration test.
#![cfg(feature = "regions")]

use std::mem::transmute;

use cs_jit_cranelift::Lowerer;
use cs_rir::{Block, BlockId, Function, Inst, Term, Type, Value};
use cs_vm::vm::NanboxValue;

fn call2(ptr: *const u8, a: i64, b: i64) -> cs_core::Value {
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };
    let av = NanboxValue::fixnum(a).into_raw();
    let bv = NanboxValue::fixnum(b).into_raw();
    let r = func(av, bv);
    unsafe { NanboxValue(r).to_value() }
}

fn as_fixnum(v: cs_core::Value, ctx: &str) -> i64 {
    match v {
        cs_core::Value::Fixnum(n) => n,
        other => panic!("{ctx}: expected Fixnum, got {other:?}"),
    }
}

/// A 2-param body that builds `(cons-region a b)` and reads one slot
/// back: `accessor` is `Inst::Car` or `Inst::Cdr` applied to the pair.
fn cons_region_then(accessor: fn(Value, Value) -> Inst) -> Function {
    let mut f = Function::new("cons_region_accessor");
    f.params.push((Value(0), Type::Fixnum)); // a
    f.params.push((Value(1), Type::Fixnum)); // b
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::ConsRegion(Value(2), Value(0), 0, Value(1), 0),
            accessor(Value(3), Value(2)),
        ],
        terminator: Term::Return(Value(3)),
    });
    f
}

#[test]
fn cons_region_car_round_trips_on_uniform_nb() {
    // (car (cons-region a b)) == a. Proves the ConsRegion arm lowers,
    // the prewalk accepts it, and the allocated pair is readable.
    let f = cons_region_then(Inst::Car);
    let mut lowerer = Lowerer::new().expect("lowerer");
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("compile_uniform_nb accepts ConsRegion under `regions`");
    assert_eq!(as_fixnum(call2(ptr, 7, 9), "car"), 7);
    assert_eq!(as_fixnum(call2(ptr, -3, 100), "car"), -3);
}

#[test]
fn cons_region_cdr_round_trips_on_uniform_nb() {
    // (cdr (cons-region a b)) == b.
    let f = cons_region_then(Inst::Cdr);
    let mut lowerer = Lowerer::new().expect("lowerer");
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("compile_uniform_nb accepts ConsRegion under `regions`");
    assert_eq!(as_fixnum(call2(ptr, 7, 9), "cdr"), 9);
    assert_eq!(as_fixnum(call2(ptr, -3, 100), "cdr"), 100);
}
