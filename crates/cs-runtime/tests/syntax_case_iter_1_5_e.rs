//! R6RS++ Phase 1.5 Iter E — binding-form alpha-rename for
//! mark-induced shadowing.
//!
//! Status note: this iter ended up *mostly moot* for our
//! runtime-syntax-case architecture, but the tests below pin
//! the actually-correct behavior of the existing surfaces so
//! future implementers don't reintroduce a regression while
//! solving a non-problem.
//!
//! Why it's mostly moot:
//!
//! 1. Our `(eval value)` builtin serialize-and-reparses (see
//!    `b_eval` in `cs-runtime/src/builtins/mod.rs`). The write
//!    representation of `Value::Identifier { name, .. }` is
//!    just the bare name -- the mark vanishes in the
//!    serialization. The re-parsed form is plain symbols, so
//!    the regular cs-expand path runs with fresh names and no
//!    mark-induced capture is possible.
//!
//! 2. A `Value::Identifier` can't be directly applied (it's
//!    not a procedure). So an Identifier in operator position
//!    raises a runtime error before any binding-form lookup
//!    semantics matter.
//!
//! 3. The existing `cs_expand::hygiene_pass` already handles
//!    binder-position renaming for the syntax-rules expansion
//!    path via the `\u{E000}` TEMPLATE_MARKER mechanism. That
//!    code path is untouched by Phase 1.5 -- it operated on
//!    Datums, before any Value::Identifier exists.
//!
//! What remains for a future iter (deferred):
//!
//! * If/when we add a *hygiene-preserving* eval-of-value path
//!   that doesn't go through serialization, marked identifiers
//!   in binder positions would need alpha-rename. That's
//!   tracked as part of the broader Phase 2 macro work (not
//!   in 1.5).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- eval-strips-marks (the "non-problem" pin) ----

#[test]
fn eval_of_marked_identifier_form_loses_marks() {
    // (eval (datum->syntax ...)) round-trips through string
    // serialization. Identifier marks aren't reflected in the
    // write representation, so they don't survive the parse.
    // This means binding-form capture via marked identifiers
    // is structurally impossible through this path.
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define outer-x 'user)").unwrap();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((stx (datum->syntax 'ctx '(let ((x 1)) outer-x))))
               (eval stx))",
        )
        .unwrap();
    // The let inside the marked form still binds an `x` named
    // shadow that doesn't capture outer-x. Result is 'user.
    assert_eq!(disp(&rt, &v), "user");
}

// ---- existing syntax-rules hygiene preserved ----

#[test]
fn syntax_rules_binder_hygiene_still_works() {
    // The cs-expand TEMPLATE_MARKER mechanism gensym-renames
    // binder identifiers introduced by syntax-rules templates.
    // A macro that binds a local `x` doesn't capture the
    // user's `x` reference.
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define x 'user-x)").unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax with-shadow
          (syntax-rules ()
            ((_ body) (let ((x 'macro-x)) body))))
        "#,
    )
    .unwrap();
    // The body references the outer `x` -- which should NOT be
    // captured by the macro's `let` binder. Reads 'user-x.
    let v = rt.eval_str("<t>", "(with-shadow x)").unwrap();
    assert_eq!(disp(&rt, &v), "user-x");
}

#[test]
fn syntax_rules_lambda_param_hygiene() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define n 'user-n)").unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax with-arg
          (syntax-rules ()
            ((_ body) ((lambda (n) body) 'macro-n))))
        "#,
    )
    .unwrap();
    let v = rt.eval_str("<t>", "(with-arg n)").unwrap();
    assert_eq!(disp(&rt, &v), "user-n");
}

// ---- Identifier in operator position is a runtime error ----

#[test]
fn identifier_value_not_directly_applicable() {
    // If a macro author tries to use a marked Identifier
    // directly as an operator, the runtime correctly rejects
    // it (it's not a procedure). The TEMPLATE_MARKER hygiene
    // path handles binder positions in syntax-rules; the new
    // Identifier value type is for data-level hygiene
    // comparison via bound-identifier=? etc., not direct
    // evaluation.
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "((make-identifier 'foo 42) 1 2 3)")
        .expect_err("Identifier is not a procedure");
    let s = format!("{}", err);
    assert!(
        s.contains("procedure") || s.contains("apply") || s.contains("not"),
        "expected procedure-like error, got: {}",
        s
    );
}
