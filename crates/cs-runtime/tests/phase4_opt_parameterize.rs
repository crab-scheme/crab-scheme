//! Cluster F — `with-active-optimizer-passes` lexical-scoping
//! tests. Verifies that the Rust-side scoped guard (cs_opt::
//! with_scoped_active_passes) combined with the Scheme builtin
//! gives parameterize-like behavior:
//!
//! - Inside the body, `(installed-optimizer-passes)` returns
//!   the scoped list.
//! - install! / remove! inside the body mutate the SCOPED list,
//!   not the outer one.
//! - On normal return, the outer list is restored.
//! - On `raise` inside the body, the outer list is still restored
//!   (RAII guard fires on unwind).
//! - Nesting works: inner scope replaces outer; restored on exit.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- baseline visibility ----

#[test]
fn body_sees_scoped_pass_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-active-optimizer-passes '(constant-fold)
               (lambda () (installed-optimizer-passes)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

#[test]
fn outer_list_restored_after_normal_return() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'inst-stats)
             (with-active-optimizer-passes '(constant-fold)
               (lambda () 'inner-done))
             (installed-optimizer-passes)",
        )
        .unwrap();
    // Outer install! set the list to '(inst-stats); the with-
    // call ran its body with '(constant-fold), then restored.
    // After the call the OUTER list must be back to (inst-stats).
    assert_eq!(disp(&rt, &v), "(inst-stats)");
}

// ---- mutation inside body is scoped ----

#[test]
fn install_inside_body_does_not_leak_to_outer() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-active-optimizer-passes '(constant-fold)
               (lambda ()
                 (install-optimizer-pass! 'dead-block-elim)
                 (installed-optimizer-passes)))",
        )
        .unwrap();
    // Inside the body, the install! added to the SCOPED list.
    assert_eq!(disp(&rt, &v), "(constant-fold dead-block-elim)");
    // Outside the body, the install! is gone — outer list still
    // empty because the with- call restored it.
    let v2 = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v2), "()");
}

#[test]
fn remove_inside_body_does_not_leak_to_outer() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'inst-stats)
             (with-active-optimizer-passes '(constant-fold inst-stats)
               (lambda ()
                 (remove-optimizer-pass! 'inst-stats)
                 (installed-optimizer-passes)))",
        )
        .unwrap();
    // Inside: scope started with (constant-fold inst-stats),
    // remove! dropped inst-stats → (constant-fold).
    assert_eq!(disp(&rt, &v), "(constant-fold)");
    // Outside: the outer install! is preserved — the with-call's
    // remove! affected only the scoped list.
    let v2 = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v2), "(inst-stats)");
}

// ---- nesting ----

#[test]
fn nested_scopes_replace_then_restore() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(with-active-optimizer-passes '(constant-fold)
               (lambda ()
                 (let ((outer-view (installed-optimizer-passes))
                       (inner-view
                         (with-active-optimizer-passes '(dead-block-elim)
                           (lambda () (installed-optimizer-passes))))
                       (back-view (installed-optimizer-passes)))
                   (list outer-view inner-view back-view))))",
        )
        .unwrap();
    // outer-view: the outer with's scope before inner = (constant-fold)
    // inner-view: inner with's scope while running = (dead-block-elim)
    // back-view: after inner returned, outer scope restored = (constant-fold)
    assert_eq!(
        disp(&rt, &v),
        "((constant-fold) (dead-block-elim) (constant-fold))"
    );
}

// ---- unwind safety (RAII restore on raise) ----

#[test]
fn raise_inside_body_still_restores_outer_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    // Pre-install on outer scope.
    rt.eval_str("<t>", "(install-optimizer-pass! 'inst-stats)")
        .unwrap();
    // Body raises; with-call propagates the error. The Rust-side
    // RAII guard should still restore the prev list.
    let result = rt.eval_str(
        "<t>",
        "(with-active-optimizer-passes '(constant-fold)
           (lambda () (raise 'oops)))",
    );
    assert!(result.is_err(), "raise should propagate");
    // Outer list still intact?
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(
        disp(&rt, &v),
        "(inst-stats)",
        "outer list must survive raise-from-body"
    );
}

// ---- input validation ----

#[test]
fn rejects_unknown_pass_name_in_scope_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(with-active-optimizer-passes '(no-such-pass)
               (lambda () (installed-optimizer-passes)))",
        )
        .expect_err("unknown pass should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("unknown pass") || s.contains("no-such-pass"),
        "got: {}",
        s
    );
    // No state change — outer list still empty.
    let v = rt.eval_str("<t>", "(installed-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn rejects_non_symbol_in_scope_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(with-active-optimizer-passes '(42)
               (lambda () (installed-optimizer-passes)))",
        )
        .expect_err("non-symbol should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("symbol") || s.contains("pass name"),
        "got: {}",
        s
    );
}

#[test]
fn rejects_wrong_arity() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    assert!(rt
        .eval_str("<t>", "(with-active-optimizer-passes)")
        .is_err());
    assert!(rt
        .eval_str("<t>", "(with-active-optimizer-passes '())")
        .is_err());
    assert!(rt
        .eval_str(
            "<t>",
            "(with-active-optimizer-passes '() (lambda () 0) 'extra)"
        )
        .is_err());
}

// ---- active-optimizer-passes parameter-like procedure (ADR 0014 §5) ----
//
// These tests exercise the new `active-optimizer-passes` builtin
// whose getter/setter are backed by cs_opt::ACTIVE_PASSES. They
// verify that `parameterize` over this procedure gives the same
// lexical-scoping semantics as `with-active-optimizer-passes`.

#[test]
fn param_proc_getter_reads_empty_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(active-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn param_proc_setter_installs_pass_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(active-optimizer-passes '(constant-fold))")
        .unwrap();
    let v = rt.eval_str("<t>", "(active-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
    // cleanup
    cs_opt::clear_active_passes();
}

#[test]
fn parameterize_body_sees_scoped_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(parameterize ((active-optimizer-passes '(constant-fold)))
               (active-optimizer-passes))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(constant-fold)");
}

#[test]
fn parameterize_restores_outer_list_after_body() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    // Pre-install on outer scope via the setter.
    rt.eval_str("<t>", "(active-optimizer-passes '(inst-stats))")
        .unwrap();
    rt.eval_str(
        "<t>",
        "(parameterize ((active-optimizer-passes '(constant-fold)))
           'inner-done)",
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(active-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "(inst-stats)");
    cs_opt::clear_active_passes();
}

#[test]
fn parameterize_restores_empty_list_after_body() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        "(parameterize ((active-optimizer-passes '(constant-fold)))
           'inner-done)",
    )
    .unwrap();
    // Outer was empty; must be restored to empty.
    let v = rt.eval_str("<t>", "(active-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn parameterize_nested_scopes() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(parameterize ((active-optimizer-passes '(constant-fold)))
               (let ((outer (active-optimizer-passes))
                     (inner
                       (parameterize ((active-optimizer-passes '(dead-block-elim)))
                         (active-optimizer-passes)))
                     (back (active-optimizer-passes)))
                 (list outer inner back)))",
        )
        .unwrap();
    assert_eq!(
        disp(&rt, &v),
        "((constant-fold) (dead-block-elim) (constant-fold))"
    );
}

#[test]
fn param_proc_rejects_unknown_pass() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(active-optimizer-passes '(no-such-pass))")
        .expect_err("unknown pass should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("unknown pass") || s.contains("no-such-pass"),
        "got: {}",
        s
    );
    // State must not have changed.
    let v = rt.eval_str("<t>", "(active-optimizer-passes)").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn param_proc_rejects_non_symbol_in_list() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(active-optimizer-passes '(42))")
        .expect_err("non-symbol should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("symbol") || s.contains("pass name"),
        "got: {}",
        s
    );
}

#[test]
fn param_proc_rejects_wrong_arity() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    assert!(rt
        .eval_str("<t>", "(active-optimizer-passes '(constant-fold) 'extra)",)
        .is_err());
}
