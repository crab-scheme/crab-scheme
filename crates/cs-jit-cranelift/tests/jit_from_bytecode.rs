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
        terminator: Term::Branch(cs_rir::Value(2), BlockId(1), BlockId(2), Vec::new()),
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
        terminator: Term::Branch(cs_rir::Value(2), BlockId(1), BlockId(2), Vec::new()),
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
fn uniform_nb_cons_car_cdr_roundtrip() {
    // (fn (a b) (car (cons a b))) — must return a.
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("cons_car_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            // tags are baked at translate time for the specialized
            // tier; uniform-NB ignores them.
            RirInst::Cons(
                cs_rir::Value(2),
                cs_rir::Value(0),
                cs_vm::vm::JIT_RT_ANY,
                cs_rir::Value(1),
                cs_vm::vm::JIT_RT_ANY,
            ),
            RirInst::Car(cs_rir::Value(3), cs_rir::Value(2)),
        ],
        terminator: Term::Return(cs_rir::Value(3)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let a_nb = NanboxValue::fixnum(7).into_raw();
    let b_nb = NanboxValue::fixnum(13).into_raw();
    let r = func(a_nb, b_nb);
    match unsafe { NanboxValue(r).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 7),
        other => panic!("expected Fixnum(7) from (car (cons 7 13)), got {:?}", other),
    }
}

#[test]
fn uniform_nb_cdr_returns_cdr() {
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("cons_cdr_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.params.push((cs_rir::Value(1), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::Cons(
                cs_rir::Value(2),
                cs_rir::Value(0),
                cs_vm::vm::JIT_RT_ANY,
                cs_rir::Value(1),
                cs_vm::vm::JIT_RT_ANY,
            ),
            RirInst::Cdr(cs_rir::Value(3), cs_rir::Value(2)),
        ],
        terminator: Term::Return(cs_rir::Value(3)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };

    let r = func(
        NanboxValue::fixnum(7).into_raw(),
        NanboxValue::fixnum(13).into_raw(),
    );
    match unsafe { NanboxValue(r).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 13),
        other => panic!("expected Fixnum(13), got {:?}", other),
    }
}

#[test]
fn uniform_nb_pair_p_yes_and_no() {
    // (fn (x) (pair? x)) — for a Pair input it returns #t,
    // for a Fixnum it returns #f.
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("pair_p_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::PairP(cs_rir::Value(1), cs_rir::Value(0))],
        terminator: Term::Return(cs_rir::Value(1)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    // Build a Pair through the runtime helper.
    let car_nb = NanboxValue::fixnum(1).into_raw();
    let cdr_nb = NanboxValue::fixnum(2).into_raw();
    let pair_nb = unsafe {
        cs_vm::vm::vm_alloc_pair_gc(car_nb, cs_vm::vm::JIT_RT_ANY, cdr_nb, cs_vm::vm::JIT_RT_ANY)
    };
    let r_pair = func(pair_nb);
    match unsafe { NanboxValue(r_pair).to_value() } {
        cs_core::Value::Boolean(b) => assert!(b),
        other => panic!("expected Boolean(true) for pair, got {:?}", other),
    }

    // Fixnum is not a pair.
    let r_fix = func(NanboxValue::fixnum(42).into_raw());
    match unsafe { NanboxValue(r_fix).to_value() } {
        cs_core::Value::Boolean(b) => assert!(!b),
        other => panic!("expected Boolean(false) for fixnum, got {:?}", other),
    }
}

#[test]
fn uniform_nb_null_p_yes_and_no() {
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("null_p_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![RirInst::NullP(cs_rir::Value(1), cs_rir::Value(0))],
        terminator: Term::Return(cs_rir::Value(1)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    // '() is null.
    let r_null = func(NanboxValue::NULL.into_raw());
    match unsafe { NanboxValue(r_null).to_value() } {
        cs_core::Value::Boolean(b) => assert!(b),
        other => panic!("expected #t for null, got {:?}", other),
    }
    // Fixnum is not null.
    let r_fix = func(NanboxValue::fixnum(0).into_raw());
    match unsafe { NanboxValue(r_fix).to_value() } {
        cs_core::Value::Boolean(b) => assert!(!b),
        other => panic!("expected #f for fixnum 0, got {:?}", other),
    }
}

#[test]
fn uniform_nb_any_clone_preserves_pair() {
    // (fn (x) (car (clone x))) — clone increfs, then car extracts
    // the first slot. Result should match the pair's car.
    use cs_rir::{Block, BlockId, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("clone_car_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::AnyClone(cs_rir::Value(1), cs_rir::Value(0)),
            RirInst::Car(cs_rir::Value(2), cs_rir::Value(1)),
            // Drop the original `x` (the clone got consumed by Car).
            RirInst::AnyDrop(cs_rir::Value(0)),
        ],
        terminator: Term::Return(cs_rir::Value(2)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    let pair_nb = unsafe {
        cs_vm::vm::vm_alloc_pair_gc(
            NanboxValue::fixnum(99).into_raw(),
            cs_vm::vm::JIT_RT_ANY,
            NanboxValue::fixnum(100).into_raw(),
            cs_vm::vm::JIT_RT_ANY,
        )
    };
    let r = func(pair_nb);
    match unsafe { NanboxValue(r).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 99),
        other => panic!("expected Fixnum(99), got {:?}", other),
    }
}

#[test]
fn uniform_nb_call_self_countdown() {
    // (define (count-down n) (if (< n 1) n (count-down (- n 1))))
    // For n >= 0, recurses until n=0, returns 0.
    use cs_rir::{Block, BlockId, Const as RirConst, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("count_down_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    // Block 0: compute (n < 1), branch.
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(1), RirConst::Fixnum(1)),
            RirInst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
        ],
        terminator: Term::Branch(cs_rir::Value(2), BlockId(1), BlockId(2), Vec::new()),
    });
    // Block 1: base case, return n.
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![],
        terminator: Term::Return(cs_rir::Value(0)),
    });
    // Block 2: recurse with n - 1.
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(3), RirConst::Fixnum(1)),
            RirInst::Sub(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
            RirInst::CallSelf(cs_rir::Value(5), vec![cs_rir::Value(4)]),
        ],
        terminator: Term::Return(cs_rir::Value(5)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer.compile_uniform_nb(&f).unwrap();
    let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };

    // n = 0 returns 0 (base case immediately).
    let r0 = func(NanboxValue::fixnum(0).into_raw());
    match unsafe { NanboxValue(r0).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 0),
        other => panic!("expected Fixnum(0), got {:?}", other),
    }
    // n = 10 recurses 10× and returns 0.
    let r10 = func(NanboxValue::fixnum(10).into_raw());
    match unsafe { NanboxValue(r10).to_value() } {
        cs_core::Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(n, 0),
        other => panic!("expected Fixnum(0), got {:?}", other),
    }
}

#[test]
fn uniform_nb_admits_nontail_callself_via_systemv() {
    // (define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
    // — two non-tail CallSelfs, here with an *Any* param (so the raw-
    // Fixnum ABI does not apply). #50: because the body has no *tail*
    // self-call, the inner uses CallConv::SystemV (small frames, the
    // same host-stack ceiling as the legacy pure-fixnum tier), so it is
    // admitted on uniform-NB instead of routing to that tier. (Earlier
    // this asserted rejection; the SystemV-conv selection removed the
    // host-stack hazard for non-tail-only bodies.) Recursion goes
    // through the NB lane (Any param), so verify correctness too.
    use cs_rir::{Block, BlockId, Const as RirConst, Function, Inst as RirInst, Term, Type};
    use cs_vm::vm::NanboxValue;

    let mut f = Function::new("fib_nb");
    f.params.push((cs_rir::Value(0), Type::Any));
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            RirInst::LoadConst(cs_rir::Value(1), RirConst::Fixnum(2)),
            RirInst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
        ],
        terminator: Term::Branch(cs_rir::Value(2), BlockId(1), BlockId(2), Vec::new()),
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
            RirInst::LoadConst(cs_rir::Value(3), RirConst::Fixnum(1)),
            RirInst::Sub(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
            RirInst::CallSelf(cs_rir::Value(5), vec![cs_rir::Value(4)]),
            RirInst::LoadConst(cs_rir::Value(6), RirConst::Fixnum(2)),
            RirInst::Sub(cs_rir::Value(7), cs_rir::Value(0), cs_rir::Value(6)),
            RirInst::CallSelf(cs_rir::Value(8), vec![cs_rir::Value(7)]),
            RirInst::Add(cs_rir::Value(9), cs_rir::Value(5), cs_rir::Value(8)),
        ],
        terminator: Term::Return(cs_rir::Value(9)),
    });

    let mut lowerer = Lowerer::new().unwrap();
    let ptr = lowerer
        .compile_uniform_nb(&f)
        .expect("non-tail CallSelf (no tail-self) must compile on uniform-NB via SystemV (#50)");
    let func: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
    for (n, expected) in [(0i64, 0i64), (1, 1), (2, 1), (7, 13), (10, 55)] {
        let r = func(NanboxValue::fixnum(n).into_raw());
        match unsafe { NanboxValue(r).to_value() } {
            cs_core::Value::Number(cs_core::Number::Fixnum(v)) => {
                assert_eq!(v, expected, "fib({n})")
            }
            other => panic!("fib({n}): expected Fixnum({expected}), got {other:?}"),
        }
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
