//! End-to-end test that the example plugin registers and runs
//! through cs-opt's pipeline machinery without requiring any
//! cs-opt source modifications.

use cs_core::SymbolTable;
use cs_opt::{register_builtins, Pass, PassContext, PassPipeline, PassRegistry, PassStats};
use cs_opt_example::{install, NoOpCounter};
use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Value};

#[test]
fn example_plugin_registers_alongside_builtins() {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    install(&mut r).unwrap();
    assert!(r.get("no-op-counter").is_some());
    assert!(r.get("constant-fold").is_some());
}

#[test]
fn example_plugin_runs_in_pipeline_with_builtins() {
    let mut r = PassRegistry::new();
    register_builtins(&mut r).unwrap();
    install(&mut r).unwrap();

    let p = PassPipeline::from_names(&r, &["constant-fold", "dead-block-elim", "no-op-counter"])
        .unwrap();
    // no-op-counter's Bucket::Late puts it after the two builtins.
    assert_eq!(
        p.names(),
        vec!["constant-fold", "dead-block-elim", "no-op-counter"]
    );

    let mut f = Function::new("e2e");
    f.blocks.push(Block {
        id: BlockId(0),
        params: Vec::new(),
        insts: vec![
            Inst::LoadConst(Value(0), Const::Fixnum(2)),
            Inst::LoadConst(Value(1), Const::Fixnum(3)),
            Inst::Add(Value(2), Value(0), Value(1)),
        ],
        terminator: Term::Return(Value(2)),
    });

    let syms = SymbolTable::new();
    let mut stats = PassStats::default();
    let mut ctx = PassContext {
        syms: &syms,
        typer_hints: None,
        stats: &mut stats,
    };
    p.run(&mut f, &mut ctx);

    // constant-fold collapsed Add.
    assert_eq!(stats.mutations_for("constant-fold"), 1);
    // no-op-counter recorded the post-fold inst count.
    assert_eq!(stats.mutations_for("no-op-counter"), 3);
}

#[test]
fn example_plugin_name_is_valid() {
    assert!(cs_opt::is_valid_pass_name(NoOpCounter.name()));
}

#[test]
fn install_into_global_then_lookup() {
    // Use a private isolated registry so this test doesn't
    // contaminate the global with state other tests in the
    // workspace might assume isn't there. install_global is
    // tested separately in the runtime-integration suite where
    // a fresh-process-per-test discipline gives clean isolation.
    let mut r = PassRegistry::new();
    install(&mut r).unwrap();
    let pass = r.get("no-op-counter").expect("missing after install");
    assert_eq!(pass.name(), "no-op-counter");
    assert_eq!(pass.bucket(), cs_opt::Bucket::Late);
}
