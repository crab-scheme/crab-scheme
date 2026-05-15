//! End-to-end M6 iter 5: bytecode → RIR → Cranelift → native exec.
//!
//! Hand-construct a `CompiledLambda` for fib, run it through
//! `cs_vm::jit_translate::bytecode_to_rir`, JIT-compile via
//! `cs_jit_cranelift::Lowerer`, then call the native function
//! pointer and assert it produces the right values. This is the
//! integration test that proves the bytecode→RIR translator
//! produces RIR the lowerer accepts.

use std::mem::transmute;
use std::rc::Rc;

use cs_core::{Number, SymbolTable, Value};
use cs_jit_cranelift::Lowerer;
use cs_vm::jit_translate::bytecode_to_rir;
use cs_vm::opcode::{CompiledLambda, Inst};

fn fib_lambda(syms: &mut SymbolTable) -> (CompiledLambda, cs_core::Symbol) {
    let n = syms.intern("n");
    let fib = syms.intern("fib");
    let body = vec![
        Inst::LoadVar(n),
        Inst::Const(Value::Number(Number::Fixnum(2))),
        Inst::LtFx2,
        Inst::JumpIfFalse(6),
        Inst::LoadVar(n),
        Inst::Return,
        Inst::LoadVar(fib),
        Inst::LoadVar(n),
        Inst::Const(Value::Number(Number::Fixnum(1))),
        Inst::SubFx2,
        Inst::Call(1),
        Inst::LoadVar(fib),
        Inst::LoadVar(n),
        Inst::Const(Value::Number(Number::Fixnum(2))),
        Inst::SubFx2,
        Inst::Call(1),
        Inst::AddFx2,
        Inst::Return,
    ];
    let len = body.len();
    let l = CompiledLambda {
        params: vec![n],
        rest: None,
        body: Rc::new(body),
        spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
        fast: None,
        profile: Default::default(),
    };
    (l, fib)
}

#[test]
fn fib_bytecode_translates_and_jits_natively() {
    let mut syms = SymbolTable::new();
    let (lam, fib_sym) = fib_lambda(&mut syms);
    let rir = bytecode_to_rir(&lam, "fib_jit", Some(fib_sym)).expect("translate");

    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer.compile_pure_fixnum(&rir).expect("compile");
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    assert_eq!(func(0), 0);
    assert_eq!(func(1), 1);
    assert_eq!(func(2), 1);
    assert_eq!(func(3), 2);
    assert_eq!(func(5), 5);
    assert_eq!(func(10), 55);
    assert_eq!(func(20), 6765);
}

#[test]
fn add_one_bytecode_translates_and_jits() {
    let mut syms = SymbolTable::new();
    let x = syms.intern("x");
    let body = vec![
        Inst::LoadVar(x),
        Inst::Const(Value::Number(Number::Fixnum(1))),
        Inst::AddFx2,
        Inst::Return,
    ];
    let len = body.len();
    let lam = CompiledLambda {
        params: vec![x],
        rest: None,
        body: Rc::new(body),
        spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
        fast: None,
        profile: Default::default(),
    };
    let rir = bytecode_to_rir(&lam, "addone", None).unwrap();
    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_pure_fixnum(&rir).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
    assert_eq!(func(0), 1);
    assert_eq!(func(41), 42);
    assert_eq!(func(-1), 0);
}

// ====================================================================
// Stage 3 iter 3.1 — uniform-NB skeleton tests.
//
// The body's ABI is i64 NanboxValue bits in / i64 NanboxValue bits out.
// These tests hand the JIT a hand-rolled RIR Function (skipping the
// translator, which only emits typed RIR for the specialized tier) and
// invoke the resulting function pointer with raw NB bits.

#[test]
fn uniform_nb_add_two_fixnums() {
    use cs_rir::{Block, BlockId, Const as RirConst, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    // Function: (fn (a b) (+ a b)) — all NB-typed.
    let mut f = Function::new("add_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::Add(
            cs_rir::Value(2),
            cs_rir::Value(0),
            cs_rir::Value(1),
        )],
        terminator: Term::Return(cs_rir::Value(2)),
    });
    // Suppress unused warning on the imported variants we don't use here.
    let _ = RirConst::Fixnum(0);

    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer.compile_uniform_nb(&f).expect("compile_uniform_nb");
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    // Fast path: both Fixnum NBs.
    let a = NanboxValue::fixnum(40).into_raw();
    let b = NanboxValue::fixnum(2).into_raw();
    let r = func(a, b);
    let v = unsafe { NanboxValue(r).to_value() };
    match v {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 42),
        other => panic!("expected Fixnum(42), got {:?}", other),
    }
}

#[test]
fn uniform_nb_loadconst_plus_param() {
    use cs_rir::{Block, BlockId, Const as RirConst, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    // Function: (fn (x) (+ x 1)) — LoadConst Fixnum + Add via NB.
    let mut f = Function::new("plus_one_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(1), RirConst::Fixnum(1)),
            RirInst::Add(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
        ],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer.compile_uniform_nb(&f).expect("compile_uniform_nb");
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    let x = NanboxValue::fixnum(41).into_raw();
    let r = func(x);
    let v = unsafe { NanboxValue(r).to_value() };
    match v {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 42),
        other => panic!("expected Fixnum(42), got {:?}", other),
    }
}

#[test]
fn uniform_nb_sub_two_fixnums() {
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("sub_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::Sub(
            cs_rir::Value(2),
            cs_rir::Value(0),
            cs_rir::Value(1),
        )],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let r = func(
        NanboxValue::fixnum(100).into_raw(),
        NanboxValue::fixnum(58).into_raw(),
    );
    match unsafe { NanboxValue(r).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 42),
        other => panic!("expected Fixnum(42), got {:?}", other),
    }
}

#[test]
fn uniform_nb_mul_two_fixnums() {
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("mul_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::Mul(
            cs_rir::Value(2),
            cs_rir::Value(0),
            cs_rir::Value(1),
        )],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let r = func(
        NanboxValue::fixnum(6).into_raw(),
        NanboxValue::fixnum(7).into_raw(),
    );
    match unsafe { NanboxValue(r).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 42),
        other => panic!("expected Fixnum(42), got {:?}", other),
    }
}

#[test]
fn uniform_nb_lt_returns_boolean_nb() {
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("lt_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::Lt(
            cs_rir::Value(2),
            cs_rir::Value(0),
            cs_rir::Value(1),
        )],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let r_true = func(
        NanboxValue::fixnum(1).into_raw(),
        NanboxValue::fixnum(2).into_raw(),
    );
    match unsafe { NanboxValue(r_true).to_value() } {
        cs_core::Value::Boolean(b) => assert!(b),
        other => panic!("expected Boolean(true), got {:?}", other),
    }
    let r_false = func(
        NanboxValue::fixnum(2).into_raw(),
        NanboxValue::fixnum(2).into_raw(),
    );
    match unsafe { NanboxValue(r_false).to_value() } {
        cs_core::Value::Boolean(b) => assert!(!b),
        other => panic!("expected Boolean(false), got {:?}", other),
    }
}

#[test]
fn uniform_nb_branch_clamp() {
    // Multi-block clamp: if x < 10 then x else x * 2.
    use cs_rir::{Block, BlockId, Const as RirConst, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("clamp_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(1), RirConst::Fixnum(10)),
            RirInst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
        ],
        terminator: Term::Branch(cs_rir::Value(2), BlockId(1), BlockId(2)),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![],
        terminator: Term::Return(cs_rir::Value(0)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(3), RirConst::Fixnum(2)),
            RirInst::Mul(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
        ],
        terminator: Term::Return(cs_rir::Value(4)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    // x=5 < 10 → return x = 5.
    let r5 = func(NanboxValue::fixnum(5).into_raw());
    match unsafe { NanboxValue(r5).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 5),
        other => panic!("expected Fixnum(5), got {:?}", other),
    }
    // x=15 ≥ 10 → return x*2 = 30.
    let r15 = func(NanboxValue::fixnum(15).into_raw());
    match unsafe { NanboxValue(r15).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 30),
        other => panic!("expected Fixnum(30), got {:?}", other),
    }
}

#[test]
fn uniform_nb_jump_with_block_param() {
    // (fn (x) (let join ([v (if (< x 0) (- 0 x) x)]) v))
    use cs_rir::{Block, BlockId, Const as RirConst, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("abs_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(1), RirConst::Fixnum(0)),
            RirInst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
        ],
        terminator: Term::Branch(cs_rir::Value(2), BlockId(1), BlockId(2)),
    });
    // Negative branch: 0 - x.
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(3), RirConst::Fixnum(0)),
            RirInst::Sub(cs_rir::Value(4), cs_rir::Value(3), cs_rir::Value(0)),
        ],
        terminator: Term::Jump(BlockId(3), vec![cs_rir::Value(4)]),
    });
    // Positive branch: pass x through.
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![],
        terminator: Term::Jump(BlockId(3), vec![cs_rir::Value(0)]),
    });
    // Join.
    f.blocks.push(Block {
        id: BlockId(3),
        params: vec![(cs_rir::Value(5), Type::Any)],
        insts: vec![],
        terminator: Term::Return(cs_rir::Value(5)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    let r_neg = func(NanboxValue::fixnum(-7).into_raw());
    match unsafe { NanboxValue(r_neg).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 7),
        other => panic!("expected Fixnum(7), got {:?}", other),
    }
    let r_pos = func(NanboxValue::fixnum(5).into_raw());
    match unsafe { NanboxValue(r_pos).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 5),
        other => panic!("expected Fixnum(5), got {:?}", other),
    }
    let r_zero = func(NanboxValue::fixnum(0).into_raw());
    match unsafe { NanboxValue(r_zero).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 0),
        other => panic!("expected Fixnum(0), got {:?}", other),
    }
}

#[test]
fn uniform_nb_mul_overflow_falls_to_helper() {
    // (* a b) where a*b overflows 47-bit Fixnum. The fast path's
    // overflow check fires and the slow helper does the BigInt math
    // (or oversized Fixnum encode via Gc<Value>). Either way, no
    // panic, and we get a Number-typed result.
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::{NanboxValue, NB_FIXNUM_MAX};

    let mut f = Function::new("mul_overflow");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::Mul(
            cs_rir::Value(2),
            cs_rir::Value(0),
            cs_rir::Value(1),
        )],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let r = func(
        NanboxValue::fixnum(NB_FIXNUM_MAX).into_raw(),
        NanboxValue::fixnum(2).into_raw(),
    );
    // Result is a Number — Fixnum (oversized, wrapped) or Big.
    match unsafe { NanboxValue(r).to_value() } {
        cs_core::Value::Number(_) => { /* ok */ }
        other => panic!("expected Number, got {:?}", other),
    }
}

#[test]
fn uniform_nb_add_mixed_fixnum_flonum_slow_path() {
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    // (fn (a b) (+ a b)) — call with Fixnum + Flonum. Should go via
    // the runtime helper's slow path and return a Flonum.
    let mut f = Function::new("add_mixed");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::Add(
            cs_rir::Value(2),
            cs_rir::Value(0),
            cs_rir::Value(1),
        )],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    let ptr = lowerer.compile_uniform_nb(&f).expect("compile_uniform_nb");
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let a = NanboxValue::fixnum(1).into_raw();
    let b = NanboxValue::flonum(2.5).into_raw();
    let r = func(a, b);
    let v = unsafe { NanboxValue(r).to_value() };
    match v {
        cs_core::Value::Number(cs_core::Number::Flonum(f)) => {
            assert!((f - 3.5).abs() < 1e-9);
        }
        other => panic!("expected Flonum(3.5), got {:?}", other),
    }
}
