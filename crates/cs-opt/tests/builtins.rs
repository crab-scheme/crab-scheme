//! cs-opt iter 2 — tests for the three shipped builtin passes.
//!
//! Construct minimal `Function`s by hand to exercise each pass'
//! behavior. The framework's traversal + stats are already
//! covered by `framework.rs`; these tests focus on what each
//! pass actually DOES to RIR.

use std::sync::Arc;

use cs_core::SymbolTable;
use cs_opt::{
    register_builtins, Bucket, Pass, PassContext, PassPipeline, PassRegistry, PassStats,
    BUILTIN_NAMES,
};
use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};

// ---- Helpers ----

fn empty_func(name: &str) -> Function {
    let mut f = Function::new(name);
    f.blocks.push(Block {
        id: BlockId(0),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    f.return_type = Type::Fixnum;
    f
}

fn run_pass(pass: &dyn Pass, func: &mut Function) -> PassStats {
    let syms = SymbolTable::new();
    let mut stats = PassStats::default();
    let mut ctx = PassContext {
        syms: &syms,
        typer_hints: None,
        stats: &mut stats,
    };
    pass.run(func, &mut ctx);
    stats
}

// ---- register_builtins ----

#[test]
fn register_builtins_succeeds_on_fresh_registry() {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    assert_eq!(r.len(), BUILTIN_NAMES.len());
    for name in BUILTIN_NAMES {
        assert!(r.get(name).is_some(), "missing: {}", name);
    }
}

#[test]
fn register_builtins_twice_is_rejected_as_duplicate() {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    // Second call hits the duplicate path on the first pass.
    assert!(register_builtins(&mut r).is_err());
}

#[test]
fn builtin_passes_have_expected_buckets() {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    assert_eq!(r.get("constant-fold").unwrap().bucket(), Bucket::Early);
    assert_eq!(r.get("dead-block-elim").unwrap().bucket(), Bucket::Default);
    assert_eq!(r.get("inst-stats").unwrap().bucket(), Bucket::Late);
}

// ---- constant-fold ----

fn const_fold() -> Arc<dyn Pass> {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    r.get("constant-fold").unwrap()
}

#[test]
fn constant_fold_folds_add_of_two_consts() {
    // LoadConst v0 = 2; LoadConst v1 = 3; Add v2 = v0 + v1.
    let mut f = empty_func("fold-add");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(2)),
        Inst::LoadConst(Value(1), Const::Fixnum(3)),
        Inst::Add(Value(2), Value(0), Value(1)),
    ];
    f.blocks[0].terminator = Term::Return(Value(2));
    let stats = run_pass(&*const_fold(), &mut f);
    assert_eq!(stats.mutations("constant-fold"), 1);
    // The Add should now be a LoadConst of 5.
    match &f.blocks[0].insts[2] {
        Inst::LoadConst(Value(2), Const::Fixnum(5)) => (),
        other => panic!("expected LoadConst 5, got {:?}", other),
    }
}

#[test]
fn constant_fold_chains_through_added_consts() {
    // Newly-folded consts feed further folds within the same block.
    let mut f = empty_func("fold-chain");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(2)),
        Inst::LoadConst(Value(1), Const::Fixnum(3)),
        Inst::Add(Value(2), Value(0), Value(1)), // -> 5
        Inst::LoadConst(Value(3), Const::Fixnum(4)),
        Inst::Mul(Value(4), Value(2), Value(3)), // -> 20 (after chain)
    ];
    f.blocks[0].terminator = Term::Return(Value(4));
    let stats = run_pass(&*const_fold(), &mut f);
    assert_eq!(stats.mutations("constant-fold"), 2);
    match &f.blocks[0].insts[4] {
        Inst::LoadConst(Value(4), Const::Fixnum(20)) => (),
        other => panic!("expected LoadConst 20, got {:?}", other),
    }
}

#[test]
fn constant_fold_skips_when_operand_is_not_const() {
    // v0 is a block param (no LoadConst), so Add isn't foldable.
    let mut f = empty_func("no-fold");
    f.blocks[0].params = vec![(Value(0), Type::Fixnum)];
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(1), Const::Fixnum(3)),
        Inst::Add(Value(2), Value(0), Value(1)),
    ];
    f.blocks[0].terminator = Term::Return(Value(2));
    let stats = run_pass(&*const_fold(), &mut f);
    assert_eq!(stats.mutations("constant-fold"), 0);
    // Add remains.
    assert!(matches!(f.blocks[0].insts[1], Inst::Add(_, _, _)));
}

#[test]
fn constant_fold_skips_on_overflow() {
    // i64::MAX + 1 overflows; checked_add returns None, pass skips.
    let mut f = empty_func("overflow");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(i64::MAX)),
        Inst::LoadConst(Value(1), Const::Fixnum(1)),
        Inst::Add(Value(2), Value(0), Value(1)),
    ];
    f.blocks[0].terminator = Term::Return(Value(2));
    let stats = run_pass(&*const_fold(), &mut f);
    assert_eq!(stats.mutations("constant-fold"), 0);
    assert!(matches!(f.blocks[0].insts[2], Inst::Add(_, _, _)));
}

#[test]
fn constant_fold_handles_sub_and_mul() {
    let mut f = empty_func("sub-mul");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(10)),
        Inst::LoadConst(Value(1), Const::Fixnum(3)),
        Inst::Sub(Value(2), Value(0), Value(1)), // 7
        Inst::Mul(Value(3), Value(0), Value(1)), // 30
    ];
    f.blocks[0].terminator = Term::Return(Value(3));
    let stats = run_pass(&*const_fold(), &mut f);
    assert_eq!(stats.mutations("constant-fold"), 2);
    assert!(matches!(
        f.blocks[0].insts[2],
        Inst::LoadConst(_, Const::Fixnum(7))
    ));
    assert!(matches!(
        f.blocks[0].insts[3],
        Inst::LoadConst(_, Const::Fixnum(30))
    ));
}

#[test]
fn constant_fold_does_not_fold_div() {
    // Div skipped because Fixnum/Fixnum may need a Rational.
    let mut f = empty_func("no-div-fold");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(10)),
        Inst::LoadConst(Value(1), Const::Fixnum(5)),
        Inst::Div(Value(2), Value(0), Value(1)),
    ];
    f.blocks[0].terminator = Term::Return(Value(2));
    let stats = run_pass(&*const_fold(), &mut f);
    assert_eq!(stats.mutations("constant-fold"), 0);
}

#[test]
fn constant_fold_stops_at_block_boundary() {
    // v0 = LoadConst 5 in block 0; v1 = block-param of block 1
    // referencing the value (passed via Jump). The pass doesn't
    // follow inter-block flow.
    let mut f = empty_func("two-block");
    f.blocks[0].insts = vec![Inst::LoadConst(Value(0), Const::Fixnum(5))];
    f.blocks[0].terminator = Term::Jump(BlockId(1), vec![Value(0)]);
    f.blocks.push(Block {
        id: BlockId(1),
        params: vec![(Value(1), Type::Fixnum)],
        insts: vec![Inst::Add(Value(2), Value(1), Value(1))],
        terminator: Term::Return(Value(2)),
    });
    let stats = run_pass(&*const_fold(), &mut f);
    // v1 is a block-param; the pass doesn't track it.
    assert_eq!(stats.mutations("constant-fold"), 0);
}

// ---- dead-block-elim ----

fn dead_block() -> Arc<dyn Pass> {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    r.get("dead-block-elim").unwrap()
}

#[test]
fn dead_block_elim_keeps_entry_only() {
    let mut f = empty_func("just-entry");
    let stats = run_pass(&*dead_block(), &mut f);
    assert_eq!(stats.mutations("dead-block-elim"), 0);
    assert_eq!(f.blocks.len(), 1);
}

#[test]
fn dead_block_elim_removes_unreachable() {
    let mut f = empty_func("with-dead");
    // entry returns directly. Block 1 is unreachable.
    f.blocks.push(Block {
        id: BlockId(1),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    f.blocks.push(Block {
        id: BlockId(2),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    let stats = run_pass(&*dead_block(), &mut f);
    assert_eq!(stats.mutations("dead-block-elim"), 2);
    assert_eq!(f.blocks.len(), 1);
    assert_eq!(f.blocks[0].id, BlockId(0));
}

#[test]
fn dead_block_elim_follows_jump_to_keep_target() {
    let mut f = empty_func("follow-jump");
    f.blocks[0].terminator = Term::Jump(BlockId(1), vec![]);
    f.blocks.push(Block {
        id: BlockId(1),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    let stats = run_pass(&*dead_block(), &mut f);
    assert_eq!(stats.mutations("dead-block-elim"), 0);
    assert_eq!(f.blocks.len(), 2);
}

#[test]
fn dead_block_elim_follows_both_branch_arms() {
    let mut f = empty_func("follow-branch");
    f.blocks[0].insts = vec![Inst::LoadConst(Value(0), Const::Boolean(true))];
    f.blocks[0].terminator = Term::Branch(Value(0), BlockId(1), BlockId(2), vec![]);
    for id in [BlockId(1), BlockId(2), BlockId(3)] {
        f.blocks.push(Block {
            id,
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Term::Return(Value(0)),
        });
    }
    // Block 3 is unreachable; 1 and 2 stay.
    let stats = run_pass(&*dead_block(), &mut f);
    assert_eq!(stats.mutations("dead-block-elim"), 1);
    assert_eq!(f.blocks.len(), 3);
    assert!(f.blocks.iter().any(|b| b.id == BlockId(1)));
    assert!(f.blocks.iter().any(|b| b.id == BlockId(2)));
    assert!(!f.blocks.iter().any(|b| b.id == BlockId(3)));
}

#[test]
fn dead_block_elim_preserves_entry_at_index_zero() {
    // cs-aot relies on `func.blocks[0]` for the entry. Verify
    // retain doesn't shuffle the entry off index 0 even when
    // other blocks get dropped.
    let mut f = empty_func("entry-position");
    f.blocks.push(Block {
        id: BlockId(1),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    f.blocks[0].terminator = Term::Jump(BlockId(2), vec![]);
    f.blocks.push(Block {
        id: BlockId(2),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    let _ = run_pass(&*dead_block(), &mut f);
    // Block 1 was dead; blocks 0 and 2 survive. Entry block id
    // is still at vec index 0.
    assert_eq!(f.blocks[0].id, f.entry);
}

// ---- inst-stats ----

fn inst_stats() -> Arc<dyn Pass> {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    r.get("inst-stats").unwrap()
}

#[test]
fn inst_stats_counts_total_insts_and_blocks() {
    let mut f = empty_func("counted");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(1)),
        Inst::LoadConst(Value(1), Const::Fixnum(2)),
        Inst::Add(Value(2), Value(0), Value(1)),
    ];
    f.blocks[0].terminator = Term::Return(Value(2));
    f.blocks.push(Block {
        id: BlockId(1),
        params: Vec::new(),
        insts: vec![Inst::LoadConst(Value(3), Const::Fixnum(99))],
        terminator: Term::Return(Value(3)),
    });
    let stats = run_pass(&*inst_stats(), &mut f);
    assert_eq!(stats.mutations("inst-stats"), 4);
    assert_eq!(stats.mutations("inst-stats:blocks"), 2);
}

#[test]
fn inst_stats_does_not_mutate_function() {
    let mut f = empty_func("immutable-by-stats");
    f.blocks[0].insts = vec![Inst::LoadConst(Value(0), Const::Fixnum(7))];
    let before = format!("{:?}", f);
    let _ = run_pass(&*inst_stats(), &mut f);
    let after = format!("{:?}", f);
    assert_eq!(before, after);
}

// ---- end-to-end pipeline of all three ----

#[test]
fn full_builtin_pipeline_runs_in_bucket_order() {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    let p =
        PassPipeline::from_names(&r, &["dead-block-elim", "inst-stats", "constant-fold"]).unwrap();
    // Bucket ordering: Early (constant-fold) before Default
    // (dead-block-elim) before Late (inst-stats).
    assert_eq!(
        p.names(),
        vec!["constant-fold", "dead-block-elim", "inst-stats"]
    );

    let mut f = empty_func("e2e");
    f.blocks[0].insts = vec![
        Inst::LoadConst(Value(0), Const::Fixnum(10)),
        Inst::LoadConst(Value(1), Const::Fixnum(20)),
        Inst::Add(Value(2), Value(0), Value(1)),
    ];
    f.blocks[0].terminator = Term::Return(Value(2));
    // Add an unreachable block too.
    f.blocks.push(Block {
        id: BlockId(99),
        params: Vec::new(),
        insts: vec![Inst::LoadConst(Value(99), Const::Fixnum(0))],
        terminator: Term::Return(Value(99)),
    });

    let syms = SymbolTable::new();
    let mut stats = PassStats::default();
    let mut ctx = PassContext {
        syms: &syms,
        typer_hints: None,
        stats: &mut stats,
    };
    p.run(&mut f, &mut ctx);

    // constant-fold folded the Add.
    assert_eq!(stats.mutations("constant-fold"), 1);
    assert!(matches!(
        f.blocks[0].insts[2],
        Inst::LoadConst(_, Const::Fixnum(30))
    ));
    // dead-block-elim dropped the unreachable block.
    assert_eq!(stats.mutations("dead-block-elim"), 1);
    assert_eq!(f.blocks.len(), 1);
    // inst-stats saw the cleaned-up IR.
    assert_eq!(stats.mutations("inst-stats"), 3);
    assert_eq!(stats.mutations("inst-stats:blocks"), 1);

    // Every pass ran once.
    assert_eq!(stats.runs("constant-fold"), 1);
    assert_eq!(stats.runs("dead-block-elim"), 1);
    assert_eq!(stats.runs("inst-stats"), 1);
}
