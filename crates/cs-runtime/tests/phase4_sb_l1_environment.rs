//! Phase-4-sb L1.1 — `(environment ...)` immutable snapshot.
//!
//! Per ADR 0015: `(environment '(rnrs base))` returns an immutable
//! snapshot environment. `eval` consulted with it uses a restricted
//! frame; identifiers outside the import set are unbound;
//! `(eval '(set! ...) env)` raises `&assertion`.
//!
//! L1.1 supports only the `(rnrs base)` library; other import
//! specs error at construction. L1.3 follow-up adds composite
//! construction.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- construction ----

#[test]
fn environment_returns_environment_value() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(environment? (environment '(rnrs base)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn top_level_env_sentinel_is_not_environment_value() {
    let mut rt = Runtime::new();
    // interaction-environment returns the legacy sentinel; not
    // a real env record for predicate purposes.
    let v = rt
        .eval_str("<t>", "(environment? (interaction-environment))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn environment_predicate_false_for_non_env_values() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(list (environment? 42) (environment? 'sym) (environment? '()))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f)");
}

// ---- eval against a restricted env ----

#[test]
fn eval_against_restricted_env_runs_in_set() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(eval '(+ 1 2 3) (environment '(rnrs base)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "6");
}

#[test]
fn eval_against_restricted_env_finds_list_ops() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(map car '((1 2) (3 4) (5 6))) (environment '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 3 5)");
}

#[test]
fn eval_against_restricted_env_handles_lambda() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eval '((lambda (x) (* x x)) 7) (environment '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "49");
}

// ---- unbound identifiers outside the import set ----

#[test]
fn name_not_in_rnrs_base_is_unbound_inside_env() {
    let mut rt = Runtime::new();
    // `hashtable?` is registered globally (via `(rnrs hashtables)`-
    // equivalent builtins) but NOT in our `(rnrs base)` export
    // list. Eval against the restricted env should report it as
    // an unbound identifier.
    let err = rt
        .eval_str("<t>", "(eval '(hashtable? 42) (environment '(rnrs base)))")
        .expect_err("hashtable? not in rnrs base");
    let s = format!("{}", err);
    assert!(
        s.contains("hashtable?") || s.contains("unbound") || s.contains("undefined"),
        "got: {}",
        s
    );
}

// ---- immutability: set! raises &assertion ----

#[test]
fn set_against_env_binding_raises_assertion() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((assertion-violation? c) 'caught-assertion))
               (eval '(set! + 5) (environment '(rnrs base))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught-assertion");
}

#[test]
fn set_via_lambda_inside_restricted_env_also_raises() {
    let mut rt = Runtime::new();
    // The set! goes through a lambda body but still targets the
    // env binding `car`; should still raise.
    let v = rt
        .eval_str(
            "<t>",
            "(guard (c ((assertion-violation? c) 'caught-set!))
               (eval '((lambda () (set! car #f))) (environment '(rnrs base))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught-set!");
}

// ---- snapshot semantics ----

#[test]
fn env_snapshot_does_not_see_host_redefines_after_construction() {
    let mut rt = Runtime::new();
    // Construct env, save it.
    rt.eval_str("<t>", "(define e (environment '(rnrs base)))")
        .unwrap();
    // Now redefine `+` at the host top-level to multiplication.
    rt.eval_str("<t>", "(define + *)").unwrap();
    // Eval against the saved env still sees the ORIGINAL +.
    let v = rt.eval_str("<t>", "(eval '(+ 2 3) e)").unwrap();
    assert_eq!(disp(&rt, &v), "5"); // not 6, which would mean it saw the * redefine
}

// ---- error cases ----

#[test]
fn unknown_library_errors_at_construction() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(environment '(unknown library))")
        .expect_err("unknown library should fail");
    let s = format!("{}", err);
    assert!(s.contains("unknown library"), "got: {}", s);
}

#[test]
fn import_spec_must_be_proper_list_of_symbols() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(environment '(rnrs 42))")
        .expect_err("non-symbol part in spec should fail");
    let s = format!("{}", err);
    assert!(
        s.contains("symbol") || s.contains("import-spec"),
        "got: {}",
        s
    );
}

// ---- composition with eval/environment inside the env ----

#[test]
fn restricted_env_can_itself_call_eval_environment() {
    let mut rt = Runtime::new();
    // Both `eval` and `environment` are in our (rnrs base)
    // export list, so a guest can construct further restricted
    // envs and run inside them. Defense-in-depth substrate.
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(eval '(+ 10 20) (environment '(rnrs base)))
                    (environment '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "30");
}
