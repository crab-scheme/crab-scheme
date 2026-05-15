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
