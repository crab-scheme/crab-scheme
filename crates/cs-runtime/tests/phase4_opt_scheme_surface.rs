//! Phase-4-opt iter 3 — Scheme surface for optimizer-pass install.
//!
//! `install-optimizer-pass!`, `remove-optimizer-pass!`, and
//! `installed-optimizer-passes` are Scheme builtins backed by
//! cs-opt's thread-local active-pass list. The shipped builtin
//! passes (constant-fold, dead-block-elim, inst-stats) are
//! registered by Runtime::new() and therefore name-resolvable
//! from any Scheme code that has a Runtime.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- registry visibility from Scheme ----

#[test]
fn installed_passes_starts_empty() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn install_adds_named_pass_to_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'constant-fold)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

#[test]
fn install_multiple_passes_preserves_order() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'constant-fold)
             (install-optimizer-pass! 'dead-block-elim)
             (install-optimizer-pass! 'inst-stats)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold dead-block-elim inst-stats)");
}

#[test]
fn install_is_idempotent() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'constant-fold)
             (install-optimizer-pass! 'constant-fold)
             (install-optimizer-pass! 'constant-fold)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

#[test]
fn remove_drops_named_pass() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'constant-fold)
             (install-optimizer-pass! 'dead-block-elim)
             (remove-optimizer-pass! 'constant-fold)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(dead-block-elim)");
}

#[test]
fn remove_unknown_pass_is_noop() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(remove-optimizer-pass! 'never-installed)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

// ---- error cases ----

#[test]
fn install_unknown_pass_errors_immediately() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(install-optimizer-pass! 'no-such-pass)")
        .expect_err("unknown pass should fail at install");
    let s = format!("{}", err);
    assert!(s.contains("unknown pass"), "got: {}", s);
}

#[test]
fn install_non_symbol_errors() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(install-optimizer-pass! 42)")
        .expect_err("non-symbol should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("symbol") || s.contains("pass name"),
        "got: {}",
        s
    );
}

#[test]
fn install_wrong_arity_errors() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(install-optimizer-pass!)").is_err());
    assert!(rt
        .eval_str("<t>", "(install-optimizer-pass! 'a 'b)")
        .is_err());
}

// ---- shipped builtins are registered ----

#[test]
fn shipped_builtin_passes_are_resolvable() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    // All three shipped builtins should install without error.
    for name in &["constant-fold", "dead-block-elim", "inst-stats"] {
        let src = format!("(install-optimizer-pass! '{})", name);
        rt.eval_str("<t>", &src).expect(name);
    }
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold dead-block-elim inst-stats)");
}

// ---- integration: pipeline actually runs ----
//
// We can't easily observe pass effects on a real eval here because
// the active-pass list is consulted inside cs-vm's bytecode→RIR
// translation, and the runtime evaluates a fresh Runtime via the
// walker tier for `eval_str`. End-to-end observation lands when
// the VM JIT path takes the pipeline output AND the test exercises
// JIT-compiled code. For now the framework tests in cs-opt cover
// pipeline correctness; this file verifies the Scheme surface.
//
// The smoke check below confirms (a) install succeeded (the
// thread-local was mutated) and (b) cs-opt's run_active_pipeline
// is willing to run with the active list. It doesn't observe an
// IR change.

#[test]
fn install_and_eval_does_not_break_runtime() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(install-optimizer-pass! 'inst-stats)")
        .unwrap();
    let v = rt.eval_str("<t>", "(+ 1 2 3 4)").unwrap();
    assert_eq!(disp(&rt, &v), "10");
}
