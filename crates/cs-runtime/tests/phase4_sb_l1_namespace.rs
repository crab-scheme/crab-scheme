//! Phase-4-sb L1.2 — `(make-namespace ...)` mutable namespace.
//!
//! Per ADR 0015: same vector record shape as the L1.1 snapshot
//! environment but mutable. `namespace-set-variable-value!` and
//! `namespace-undefine-variable!` mutate the namespace; mutations
//! are visible to subsequent `eval` against the same namespace.
//! `eval` against a mutable namespace doesn't raise on `set!`
//! (the L1.1 immutability check passes through to normal frame
//! semantics).

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- construction ----

#[test]
fn make_namespace_returns_environment_value() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(environment? (make-namespace '(rnrs base)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn make_namespace_supports_same_libraries_as_environment() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(list (environment? (make-namespace '(rnrs base)))
                   (environment? (make-namespace '(rnrs lists)))
                   (environment? (make-namespace '(rnrs))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #t)");
}

// ---- eval against a mutable namespace ----

#[test]
fn eval_against_namespace_works() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(eval '(+ 10 20) (make-namespace '(rnrs base)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "30");
}

#[test]
fn set_inside_eval_against_namespace_does_not_raise() {
    let mut rt = Runtime::new();
    // Snapshot env (`environment`) would raise &assertion here.
    // Mutable namespace lets the set! through (mutates the local
    // per-eval frame; write-back to namespace is future work).
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(begin (set! + 99) 'ok) (make-namespace '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "ok");
}

// ---- namespace-set-variable-value! ----

#[test]
fn namespace_set_overwrites_existing_binding() {
    let mut rt = Runtime::new();
    // The base env has `+`; overwrite it with a constant. Then
    // subsequent eval picks up the overwrite.
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base)))
             (namespace-set-variable-value! ns '+ 'replaced)
             (eval '+ ns)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "replaced");
}

#[test]
fn namespace_set_adds_new_binding_not_in_base() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base)))
             (namespace-set-variable-value! ns 'host-secret 42)
             (eval 'host-secret ns)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn namespace_set_visible_to_subsequent_evals() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base)))
             (namespace-set-variable-value! ns 'config 'before)
             (define seen-first (eval 'config ns))
             (namespace-set-variable-value! ns 'config 'after)
             (define seen-second (eval 'config ns))
             (list seen-first seen-second)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(before after)");
}

// ---- namespace-undefine-variable! ----

#[test]
fn namespace_undefine_removes_binding() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        "(define ns (make-namespace '(rnrs base)))
         (namespace-set-variable-value! ns 'tmp 99)",
    )
    .unwrap();
    // Before undefine: visible.
    let v = rt.eval_str("<t>", "(eval 'tmp ns)").unwrap();
    assert_eq!(disp(&rt, &v), "99");
    // After undefine: unbound.
    rt.eval_str("<t>", "(namespace-undefine-variable! ns 'tmp)")
        .unwrap();
    let err = rt
        .eval_str("<t>", "(eval 'tmp ns)")
        .expect_err("undefined after undefine");
    let s = format!("{}", err);
    assert!(
        s.contains("tmp") || s.contains("unbound") || s.contains("undefined"),
        "got: {}",
        s
    );
}

#[test]
fn namespace_undefine_of_nonexistent_is_noop() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base)))
             (namespace-undefine-variable! ns 'never-existed)
             'ok",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "ok");
}

#[test]
fn namespace_undefine_then_redefine_works() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base)))
             (namespace-set-variable-value! ns 'x 1)
             (namespace-undefine-variable! ns 'x)
             (namespace-set-variable-value! ns 'x 2)
             (eval 'x ns)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "2");
}

// ---- mutation rejected on snapshot env from L1.1 ----

#[test]
fn snapshot_environment_rejects_namespace_set() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(define e (environment '(rnrs base)))
             (namespace-set-variable-value! e 'x 1)",
        )
        .expect_err("snapshot env should reject set");
    let s = format!("{}", err);
    assert!(
        s.contains("immutable") || s.contains("make-namespace"),
        "got: {}",
        s
    );
}

#[test]
fn snapshot_environment_still_raises_assertion_on_set_inside_eval() {
    // L1.1 behavior preserved: even with L1.2 shipped, an
    // immutable env still raises on set!.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((assertion-violation? c) 'caught))
               (eval '(set! + 5) (environment '(rnrs base))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

// ---- error cases ----

#[test]
fn namespace_set_rejects_non_namespace() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(namespace-set-variable-value! 42 'x 1)")
        .expect_err("non-namespace arg");
    let s = format!("{}", err);
    assert!(s.contains("not a namespace"), "got: {}", s);
}

#[test]
fn namespace_set_rejects_non_symbol_name() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(namespace-set-variable-value! (make-namespace '(rnrs base)) 42 'val)",
        )
        .expect_err("non-symbol name");
    let s = format!("{}", err);
    assert!(s.contains("symbol"), "got: {}", s);
}

#[test]
fn namespace_set_wrong_arity_errors() {
    let mut rt = Runtime::new();
    assert!(rt
        .eval_str("<t>", "(namespace-set-variable-value!)")
        .is_err());
    assert!(rt
        .eval_str(
            "<t>",
            "(namespace-set-variable-value! (make-namespace '(rnrs base)) 'x)"
        )
        .is_err());
}
