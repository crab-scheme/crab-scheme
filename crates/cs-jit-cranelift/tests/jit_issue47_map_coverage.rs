//! Regression tests for issue #47 — recover JIT coverage for
//! map-style cross-function bodies.
//!
//! Before the fix, the uniform-NB tier rejected *any* non-tail
//! `CallSelf` (its inner used `CallConv::Tail`, whose larger per-frame
//! cost overflows deep pure-arithmetic recursion like `tak`). Map-style
//! helpers — `(define (mp lst) (if (null? lst) '() (cons (f (car lst))
//! (mp (cdr lst)))))` — recurse non-tail to *data* depth and also make
//! a sibling call (`CallGeneral`), so they were rejected here and then
//! routed to the VM (the legacy SystemV tier miscompiles cross-function
//! calls, issue #19), losing JIT coverage entirely.
//!
//! The fix admits a non-tail `CallSelf` on uniform-NB when the body
//! also has a cross-function call, and selects the inner calling
//! convention by need: `CallConv::Tail` only when a tail-position
//! self-call will emit `return_call`, else `CallConv::SystemV` (smaller
//! frames → higher host-stack ceiling, matching the legacy tier).
//!
//! These tests assert at the tier boundary: a map-style RIR body
//! *compiles* on uniform-NB (`Ok`). A pure non-tail self-recursive body
//! with no cross-call also compiles now (#50) — it has no tail self-call,
//! so the inner uses CallConv::SystemV and incurs no host-stack hazard,
//! letting it leave the legacy pure-fixnum tier.

use cs_jit_cranelift::Lowerer;
use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};

/// `(define (f n g) (if (< n 1) 0 (+ (g n) (f (- n 1) g))))`
///
/// The recursive `(f (- n 1) g)` is **non-tail** (an operand of `+`)
/// and the body makes a sibling call `(g n)` (`CallGeneral`). This is
/// the minimal #47 shape. No tail self-call ⇒ the inner picks
/// `CallConv::SystemV` (the higher-ceiling branch).
fn map_style_nontail_self_plus_crosscall() -> Function {
    let mut f = Function::new("f_map_nontail");
    f.params.push((Value(0), Type::Any)); // n
    f.params.push((Value(1), Type::Any)); // g
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(2), Const::Fixnum(1)),
            Inst::Lt(Value(3), Value(0), Value(2)),
        ],
        terminator: Term::Branch(Value(3), BlockId(1), BlockId(2), Vec::new()),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![Inst::LoadConst(Value(4), Const::Fixnum(0))],
        terminator: Term::Return(Value(4)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            // (g n) — cross-function call; callee is param `g`.
            Inst::CallGeneral(Value(5), Value(1), vec![Value(0)]),
            Inst::LoadConst(Value(6), Const::Fixnum(1)),
            Inst::Sub(Value(7), Value(0), Value(6)),
            // (f (- n 1) g) — NON-TAIL self-call (Add follows).
            Inst::CallSelf(Value(8), vec![Value(7), Value(1)]),
            Inst::Add(Value(9), Value(5), Value(8)),
        ],
        terminator: Term::Return(Value(9)),
    });
    f
}

/// `(define (loop n g) (if (< n 1) (g 0) (loop (- n 1) g)))`
///
/// The recursive `(loop (- n 1) g)` is in **tail** position and there
/// is a tail cross-call `(g 0)`. The tail self-call ⇒ the inner picks
/// `CallConv::Tail` and emits `return_call`; the tail cross-call lowers
/// to the ADR-0019 bounce. Exercises the Tail-conv branch of the fix.
fn tail_self_plus_tail_crosscall() -> Function {
    let mut f = Function::new("f_loop_tail");
    f.params.push((Value(0), Type::Any)); // n
    f.params.push((Value(1), Type::Any)); // g
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(2), Const::Fixnum(1)),
            Inst::Lt(Value(3), Value(0), Value(2)),
        ],
        terminator: Term::Branch(Value(3), BlockId(1), BlockId(2), Vec::new()),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(4), Const::Fixnum(0)),
            // (g 0) — tail-position cross-call → bounce.
            Inst::CallGeneral(Value(5), Value(1), vec![Value(4)]),
        ],
        terminator: Term::Return(Value(5)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(6), Const::Fixnum(1)),
            Inst::Sub(Value(7), Value(0), Value(6)),
            // (loop (- n 1) g) — tail self-call → return_call.
            Inst::CallSelf(Value(8), vec![Value(7), Value(1)]),
        ],
        terminator: Term::Return(Value(8)),
    });
    f
}

/// `(define (f n) (if (< n 1) 0 (+ (f (- n 1)) 1)))`
///
/// Non-tail self-call, **no** cross-function call. Must stay rejected so
/// `tak`-style pure-arithmetic recursion keeps routing to the
/// specialized (SystemV) tier rather than paying NB overhead here.
fn nontail_self_no_crosscall() -> Function {
    let mut f = Function::new("f_nontail_pure");
    f.params.push((Value(0), Type::Any)); // n
    f.entry = BlockId(0);
    f.blocks.push(Block {
        id: BlockId(0),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(1), Const::Fixnum(1)),
            Inst::Lt(Value(2), Value(0), Value(1)),
        ],
        terminator: Term::Branch(Value(2), BlockId(1), BlockId(2), Vec::new()),
    });
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![],
        insts: vec![Inst::LoadConst(Value(3), Const::Fixnum(0))],
        terminator: Term::Return(Value(3)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: vec![],
        insts: vec![
            Inst::LoadConst(Value(4), Const::Fixnum(1)),
            Inst::Sub(Value(5), Value(0), Value(4)),
            Inst::CallSelf(Value(6), vec![Value(5)]),
            Inst::LoadConst(Value(7), Const::Fixnum(1)),
            Inst::Add(Value(8), Value(6), Value(7)),
        ],
        terminator: Term::Return(Value(8)),
    });
    f
}

#[test]
fn map_style_body_now_compiles_on_uniform_nb() {
    let f = map_style_nontail_self_plus_crosscall();
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    // Pre-fix this returned Err(Unsupported "non-tail CallSelf"); the
    // body then fell to the VM. The cross-call exception now admits it.
    lowerer
        .compile_uniform_nb(&f)
        .expect("map-style body must compile on uniform-NB (issue #47)");
}

#[test]
fn tail_self_with_cross_call_compiles_on_uniform_nb() {
    let f = tail_self_plus_tail_crosscall();
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    // The Tail-conv branch: a tail self-call emits `return_call` (legal
    // only under CallConv::Tail) while the tail cross-call bounces.
    lowerer
        .compile_uniform_nb(&f)
        .expect("tail-self + cross-call body must compile on uniform-NB");
}

#[test]
fn pure_nontail_self_now_compiles_on_uniform_nb() {
    let f = nontail_self_no_crosscall();
    let mut lowerer = Lowerer::new().expect("Lowerer::new");
    // #50 — a pure non-tail self-recursive body with no cross-call has no
    // *tail* self-call, so the inner uses CallConv::SystemV (small frames,
    // the same host-stack ceiling as the legacy pure-fixnum tier). It is
    // therefore admitted on uniform-NB now instead of routing to the
    // pure-fixnum tier — the precondition for retiring that tier. (Earlier
    // this asserted rejection; the SystemV-conv selection from #47 made
    // the host-stack hazard a non-issue for non-tail-only bodies.)
    lowerer
        .compile_uniform_nb(&f)
        .expect("pure non-tail self-recursion must compile on uniform-NB (#50)");
}
