//! Phase-4-sb L1.4 — first-class environment passthrough.
//!
//! Environments returned from `(environment ...)` / `(make-namespace ...)`
//! are ordinary first-class values: bind them, pass them to
//! procedures, store them in collections, compare them. The
//! L1.1/L1.2/L1.3 substrate already supports this because env
//! values are just Vector records — this file documents and
//! tests the property.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- bind + reuse across multiple evals ----

#[test]
fn env_can_be_bound_and_reused() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define e (environment '(rnrs base)))
             (list (eval '(+ 1 2) e)
                   (eval '(* 3 4) e)
                   (eval '(- 10 7) e))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(3 12 3)");
}

// ---- pass env as a procedure argument ----

#[test]
fn env_can_be_passed_to_a_user_procedure() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define (run-in env expr) (eval expr env))
             (run-in (environment '(rnrs base)) '(+ 100 23))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "123");
}

#[test]
fn env_can_be_curried_through_higher_order_proc() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define (eval-with env) (lambda (expr) (eval expr env)))
             (define run (eval-with (environment '(rnrs base))))
             (list (run '(+ 1 2)) (run '(* 3 4)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(3 12)");
}

// ---- store env in collections ----

#[test]
fn env_can_be_stored_in_a_list() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define envs (list (environment '(rnrs base))
                                 (environment '(rnrs base) '(rnrs lists))))
             (list (eval '(+ 1 2) (car envs))
                   (eval '(for-all positive? '(1 2 3)) (cadr envs)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(3 #t)");
}

#[test]
fn env_can_be_stored_in_a_vector() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define envs (vector (environment '(rnrs base))
                                   (make-namespace '(rnrs base))))
             (namespace-set-variable-value! (vector-ref envs 1) 'secret 42)
             (list (eval '(+ 1 2) (vector-ref envs 0))
                   (eval 'secret (vector-ref envs 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(3 42)");
}

// ---- distinct envs are distinct objects ----

#[test]
fn two_environments_are_not_eq() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eq? (environment '(rnrs base)) (environment '(rnrs base)))",
        )
        .unwrap();
    // Each call to (environment ...) builds a fresh Vector
    // record; they're not eq? to each other.
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn same_env_bound_to_two_names_is_eq_to_itself() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define e (environment '(rnrs base)))
             (define f e)
             (eq? e f)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- snapshot semantics survive passthrough ----

#[test]
fn snapshot_env_passed_through_proc_keeps_immutability() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define (try-mutate env)
               (guard (c ((assertion-violation? c) 'caught))
                 (eval '(set! + 99) env)))
             (try-mutate (environment '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

// ---- mutable namespace shared across closure captures ----

#[test]
fn namespace_shared_between_closures_sees_each_others_writes() {
    let mut rt = Runtime::new();
    // Two closures capture the SAME namespace; one writes, the
    // other reads. The mutation should be visible.
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base)))
             (define (writer v) (namespace-set-variable-value! ns 'shared v))
             (define (reader) (eval 'shared ns))
             (writer 'first)
             (define a (reader))
             (writer 'second)
             (define b (reader))
             (list a b)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(first second)");
}

// ---- env value type is observable ----

#[test]
fn vector_predicate_recognizes_env_as_vector() {
    let mut rt = Runtime::new();
    // Underneath, env is a Vector record. (vector? env) returns
    // #t — implementation detail, but useful for ad-hoc
    // introspection.
    let v = rt
        .eval_str(
            "<t>",
            "(list (vector? (environment '(rnrs base)))
                   (vector? (make-namespace '(rnrs base))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t)");
}
