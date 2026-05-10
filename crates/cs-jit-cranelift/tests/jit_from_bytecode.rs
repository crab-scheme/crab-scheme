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
    };
    let rir = bytecode_to_rir(&lam, "addone", None).unwrap();
    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_pure_fixnum(&rir).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
    assert_eq!(func(0), 1);
    assert_eq!(func(41), 42);
    assert_eq!(func(-1), 0);
}
