//! cs-opt iter 1 — framework tests.
//!
//! Verifies the trait, registry, and pipeline machinery without
//! shipping any actual pass implementations (those land in iter 2).
//! Uses minimal in-test pass impls to exercise each surface.

use std::sync::Arc;

use cs_core::SymbolTable;
use cs_opt::{
    is_valid_pass_name, Bucket, Pass, PassContext, PassPipeline, PassRegistry, PassStats,
    PipelineError, RegisterError,
};
use cs_rir::{Block, BlockId, Function, Term, Value};

/// Construct a minimal but well-formed Function: an entry block
/// with a trivial Return terminator. The `pass_verify` feature
/// (when enabled) checks that the entry block exists in
/// `func.blocks`, so the `Function::new(...)` default (empty
/// blocks vec) trips the verifier — this helper avoids that.
fn skeleton_func(name: &str) -> Function {
    let mut f = Function::new(name);
    f.blocks.push(Block {
        id: BlockId(0),
        params: Vec::new(),
        insts: Vec::new(),
        terminator: Term::Return(Value(0)),
    });
    f
}

// ---- Test passes (minimal impls used across the file) ----

/// Records its name in `ctx.stats.mutations` so tests can verify
/// it actually ran in pipeline order.
struct MarkerPass {
    name: &'static str,
    bucket: Bucket,
}

impl Pass for MarkerPass {
    fn name(&self) -> &'static str {
        self.name
    }
    fn bucket(&self) -> Bucket {
        self.bucket
    }
    fn run(&self, _func: &mut Function, ctx: &mut PassContext) {
        ctx.stats.record_mutations(self.name, 1);
    }
}

// ---- Bucket ----

#[test]
fn bucket_priorities_are_ordered() {
    assert!(Bucket::Early.priority() < Bucket::Default.priority());
    assert!(Bucket::Default.priority() < Bucket::Late.priority());
}

#[test]
fn bucket_default_is_default_variant() {
    assert_eq!(Bucket::default(), Bucket::Default);
}

// ---- Name validation ----

#[test]
fn valid_pass_names_are_accepted() {
    assert!(is_valid_pass_name("a"));
    assert!(is_valid_pass_name("constant-fold"));
    assert!(is_valid_pass_name("pass-1"));
    assert!(is_valid_pass_name("z9"));
}

#[test]
fn invalid_pass_names_are_rejected() {
    assert!(!is_valid_pass_name(""));
    assert!(!is_valid_pass_name("Constant-Fold")); // uppercase
    assert!(!is_valid_pass_name("1-pass")); // starts with digit
    assert!(!is_valid_pass_name("-pass")); // starts with hyphen
    assert!(!is_valid_pass_name("pass_name")); // underscore
    assert!(!is_valid_pass_name("pass!")); // !
    assert!(!is_valid_pass_name("two words"));
}

// ---- Registry ----

#[test]
fn registry_starts_empty() {
    let r = PassRegistry::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
    assert!(r.names().is_empty());
}

#[test]
fn registry_accepts_valid_pass() {
    let mut r = PassRegistry::new();
    let p = Arc::new(MarkerPass {
        name: "p1",
        bucket: Bucket::Default,
    });
    r.register(p).unwrap();
    assert_eq!(r.len(), 1);
    assert!(r.get("p1").is_some());
    assert!(r.get("missing").is_none());
}

#[test]
fn registry_rejects_invalid_name() {
    let mut r = PassRegistry::new();
    let bad = Arc::new(MarkerPass {
        name: "Bad-Name",
        bucket: Bucket::Default,
    });
    match r.register(bad) {
        Err(RegisterError::InvalidName(n)) => assert_eq!(n, "Bad-Name"),
        other => panic!("expected InvalidName, got {:?}", other),
    }
}

#[test]
fn registry_rejects_duplicate() {
    let mut r = PassRegistry::new();
    r.register(Arc::new(MarkerPass {
        name: "dup",
        bucket: Bucket::Default,
    }))
    .unwrap();
    let again = r.register(Arc::new(MarkerPass {
        name: "dup",
        bucket: Bucket::Default,
    }));
    match again {
        Err(RegisterError::Duplicate(n)) => assert_eq!(n, "dup"),
        other => panic!("expected Duplicate, got {:?}", other),
    }
}

#[test]
fn register_error_display() {
    let e1 = RegisterError::InvalidName("X".into());
    let e2 = RegisterError::Duplicate("y".into());
    assert!(format!("{}", e1).contains("invalid"));
    assert!(format!("{}", e2).contains("already registered"));
}

#[test]
fn global_registry_is_singleton() {
    // Just verify two get_or_init calls return the same lock; we
    // don't mutate it to avoid cross-test contamination.
    let r1 = PassRegistry::global() as *const _;
    let r2 = PassRegistry::global() as *const _;
    assert_eq!(r1, r2);
}

// ---- Pipeline construction ----

#[test]
fn empty_pipeline_is_empty() {
    let p = PassPipeline::empty();
    assert!(p.is_empty());
    assert_eq!(p.len(), 0);
    assert!(p.names().is_empty());
}

#[test]
fn pipeline_resolves_known_names() {
    let mut r = PassRegistry::new();
    r.register(Arc::new(MarkerPass {
        name: "a",
        bucket: Bucket::Default,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "b",
        bucket: Bucket::Default,
    }))
    .unwrap();
    let p = PassPipeline::from_names(&r, &["a", "b"]).unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p.names(), vec!["a", "b"]);
}

#[test]
fn pipeline_reports_all_unknown_names() {
    let r = PassRegistry::new();
    let err = PassPipeline::from_names(&r, &["nope", "also-nope"]).unwrap_err();
    match err {
        PipelineError::UnknownPasses(names) => {
            assert_eq!(names.len(), 2);
            assert!(names.contains(&"nope".to_string()));
            assert!(names.contains(&"also-nope".to_string()));
        }
    }
}

#[test]
fn pipeline_sorts_by_bucket_priority() {
    let mut r = PassRegistry::new();
    r.register(Arc::new(MarkerPass {
        name: "late",
        bucket: Bucket::Late,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "early",
        bucket: Bucket::Early,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "default",
        bucket: Bucket::Default,
    }))
    .unwrap();
    // Names given in non-priority order; pipeline sorts.
    let p = PassPipeline::from_names(&r, &["late", "default", "early"]).unwrap();
    assert_eq!(p.names(), vec!["early", "default", "late"]);
}

#[test]
fn pipeline_preserves_registration_order_within_bucket() {
    let mut r = PassRegistry::new();
    r.register(Arc::new(MarkerPass {
        name: "a-default",
        bucket: Bucket::Default,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "b-default",
        bucket: Bucket::Default,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "c-default",
        bucket: Bucket::Default,
    }))
    .unwrap();
    let p = PassPipeline::from_names(&r, &["c-default", "a-default", "b-default"]).unwrap();
    // Stable sort: same priority preserves input order.
    assert_eq!(p.names(), vec!["c-default", "a-default", "b-default"]);
}

#[test]
fn pipeline_error_display_lists_unknown_names() {
    let r = PassRegistry::new();
    let err = PassPipeline::from_names(&r, &["x", "y"]).unwrap_err();
    let s = format!("{}", err);
    assert!(s.contains("unknown"));
    assert!(s.contains("x"));
    assert!(s.contains("y"));
}

// ---- Pipeline execution ----

#[test]
fn pipeline_runs_passes_in_order() {
    let mut r = PassRegistry::new();
    r.register(Arc::new(MarkerPass {
        name: "p-early",
        bucket: Bucket::Early,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "p-default",
        bucket: Bucket::Default,
    }))
    .unwrap();
    r.register(Arc::new(MarkerPass {
        name: "p-late",
        bucket: Bucket::Late,
    }))
    .unwrap();
    let p = PassPipeline::from_names(&r, &["p-late", "p-default", "p-early"]).unwrap();
    let mut func = skeleton_func("test");
    let syms = SymbolTable::new();
    let mut stats = PassStats::default();
    let mut ctx = PassContext {
        syms: &syms,
        typer_hints: None,
        stats: &mut stats,
    };
    p.run(&mut func, &mut ctx);
    // Each pass recorded one run.
    assert_eq!(stats.runs_for("p-early"), 1);
    assert_eq!(stats.runs_for("p-default"), 1);
    assert_eq!(stats.runs_for("p-late"), 1);
    // Each marker pass also recorded one mutation.
    assert_eq!(stats.mutations_for("p-early"), 1);
    assert_eq!(stats.mutations_for("p-default"), 1);
    assert_eq!(stats.mutations_for("p-late"), 1);
}

#[test]
fn empty_pipeline_run_is_a_noop() {
    let p = PassPipeline::empty();
    let mut func = skeleton_func("noop");
    let syms = SymbolTable::new();
    let mut stats = PassStats::default();
    let mut ctx = PassContext {
        syms: &syms,
        typer_hints: None,
        stats: &mut stats,
    };
    p.run(&mut func, &mut ctx);
    assert!(stats.runs.is_empty());
    assert!(stats.mutations.is_empty());
}

#[test]
fn pipeline_run_twice_accumulates_stats() {
    let mut r = PassRegistry::new();
    r.register(Arc::new(MarkerPass {
        name: "p1",
        bucket: Bucket::Default,
    }))
    .unwrap();
    let p = PassPipeline::from_names(&r, &["p1"]).unwrap();
    let mut func = skeleton_func("t");
    let syms = SymbolTable::new();
    let mut stats = PassStats::default();
    let mut ctx = PassContext {
        syms: &syms,
        typer_hints: None,
        stats: &mut stats,
    };
    p.run(&mut func, &mut ctx);
    p.run(&mut func, &mut ctx);
    assert_eq!(stats.runs_for("p1"), 2);
    assert_eq!(stats.mutations_for("p1"), 2);
}

// ---- PassStats methods ----

#[test]
fn pass_stats_returns_zero_for_unknown_pass() {
    let stats = PassStats::default();
    assert_eq!(stats.runs_for("never-ran"), 0);
    assert_eq!(stats.mutations_for("never-ran"), 0);
}

// ---- Verifier (iter 4) ----
//
// These two tests only run under the `pass_verify` feature.
// `cargo test -p cs-opt --features pass_verify` exercises them;
// the default test run skips them via cfg.

#[cfg(feature = "pass_verify")]
mod verifier_attribution {
    use super::*;
    use cs_rir::Term;

    /// A buggy pass that breaks RIR by clobbering func.entry to a
    /// non-existent BlockId. The verifier should panic with the
    /// pass's name attributed.
    struct BuggyPass;
    impl Pass for BuggyPass {
        fn name(&self) -> &'static str {
            "buggy"
        }
        fn run(&self, func: &mut cs_rir::Function, _ctx: &mut PassContext) {
            func.entry = BlockId(9999);
        }
    }

    /// A pass that breaks RIR by making a Jump target dangle.
    struct DanglerPass;
    impl Pass for DanglerPass {
        fn name(&self) -> &'static str {
            "dangler"
        }
        fn run(&self, func: &mut cs_rir::Function, _ctx: &mut PassContext) {
            func.blocks[0].terminator = Term::Jump(BlockId(9999), vec![]);
        }
    }

    #[test]
    #[should_panic(expected = "buggy")]
    fn verifier_attributes_missing_entry_to_pass_name() {
        let mut r = PassRegistry::new();
        r.register(Arc::new(BuggyPass)).unwrap();
        let p = PassPipeline::from_names(&r, &["buggy"]).unwrap();
        let mut func = skeleton_func("t");
        let syms = SymbolTable::new();
        let mut stats = PassStats::default();
        let mut ctx = PassContext {
            syms: &syms,
            typer_hints: None,
            stats: &mut stats,
        };
        p.run(&mut func, &mut ctx);
    }

    #[test]
    #[should_panic(expected = "dangler")]
    fn verifier_attributes_dangling_target_to_pass_name() {
        let mut r = PassRegistry::new();
        r.register(Arc::new(DanglerPass)).unwrap();
        let p = PassPipeline::from_names(&r, &["dangler"]).unwrap();
        let mut func = skeleton_func("t");
        let syms = SymbolTable::new();
        let mut stats = PassStats::default();
        let mut ctx = PassContext {
            syms: &syms,
            typer_hints: None,
            stats: &mut stats,
        };
        p.run(&mut func, &mut ctx);
    }
}
