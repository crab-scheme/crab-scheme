//! R6RS++ Phase 2A.4 — expand-time error pinpointing (issue #33).
//!
//! When a `define-syntax-parser` call doesn't match, the expander now
//! reports *why* and *where* — naming the offending sub-form / the
//! missing or duplicated option — instead of a flat
//! "no matching rule for macro 'X'". The offending sub-form's source
//! span rides on the `ExpandError` for tooling (LSP); these tests
//! assert on the human-readable reason that surfaces through
//! `eval_str`.

use cs_runtime::Runtime;

fn expect_err(src_defs: &str, call: &str) -> String {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", src_defs).unwrap();
    let err = rt
        .eval_str("<t>", call)
        .expect_err("call should fail to expand/eval");
    format!("{}", err)
}

// ---- combinator path (backtracking matcher) ----

#[test]
fn or_nonmatch_names_the_offending_shape() {
    let s = expect_err(
        r#"(define-syntax-parser first-of
             ((_ (~or (a) (a b))) a))"#,
        "(first-of 5)",
    );
    // points at the atom `5` that matched neither list alternative
    assert!(s.contains("list"), "want 'list' in: {}", s);
    assert!(
        !s.contains("no matching rule"),
        "should be pinpointed: {}",
        s
    );
}

#[test]
fn once_missing_required_option_is_named() {
    let s = expect_err(
        r#"(define-syntax-parser kvpair
             ((_ (~or (~once #:a a) (~once #:b b)) ...) (list a b)))"#,
        "(kvpair #:a 1)",
    );
    // names the missing required option
    assert!(s.contains("#:b"), "want '#:b' named in: {}", s);
    assert!(
        s.contains("required") || s.contains("missing") || s.contains("once"),
        "want a cardinality reason in: {}",
        s
    );
}

#[test]
fn once_duplicate_option_is_named() {
    let s = expect_err(
        r#"(define-syntax-parser kvpair
             ((_ (~or (~once #:a a) (~once #:b b)) ...) (list a b)))"#,
        "(kvpair #:a 1 #:a 3 #:b 2)",
    );
    assert!(s.contains("#:a"), "want '#:a' named in: {}", s);
    assert!(
        s.contains("once") || s.contains("only") || s.contains("at most"),
        "want a cardinality reason in: {}",
        s
    );
}

#[test]
fn keyword_literal_mismatch_is_named() {
    // An unknown keyword in the flagship EH-ellipsis pattern: should
    // name one of the expected literals (`#:a` or `#:b`) rather than
    // a generic no-match.
    let s = expect_err(
        r#"(define-syntax-parser kvpair
             ((_ (~or (~once #:a a) (~once #:b b)) ...) (list a b)))"#,
        "(kvpair #:zzz 1 #:b 2)",
    );
    assert!(s.contains("expected"), "want 'expected' reason in: {}", s);
    assert!(
        s.contains("#:a") || s.contains("#:b"),
        "want a known keyword in: {}",
        s
    );
    assert!(
        !s.contains("no matching rule"),
        "should be pinpointed: {}",
        s
    );
}

// ---- syntax-rules-desugared path (no combinators) ----

#[test]
fn extra_argument_is_pinpointed() {
    let s = expect_err(
        r#"(define-syntax-parser my-double
             ((_ x) (* 2 x)))"#,
        "(my-double 1 2)",
    );
    assert!(
        s.contains("extra") || s.contains("unexpected") || s.contains("got 2"),
        "want an arity/extra-form reason in: {}",
        s
    );
    assert!(
        !s.contains("no matching rule"),
        "should be pinpointed: {}",
        s
    );
}

#[test]
fn too_few_arguments_is_pinpointed() {
    let s = expect_err(
        r#"(define-syntax-parser pair2
             ((_ a b) (list a b)))"#,
        "(pair2 1)",
    );
    assert!(
        s.contains("missing") || s.contains("too few") || s.contains("expected"),
        "want a too-few reason in: {}",
        s
    );
}

#[test]
fn multi_clause_reports_best_partial_match() {
    // The 2-arg clause is the "closest" to a 2-arg call with a bad
    // nested shape; the diagnostic should reflect the furthest progress
    // rather than the first clause's failure.
    let s = expect_err(
        r#"(define-syntax-parser shape
             ((_ ()) 'empty)
             ((_ (a b)) (list a b)))"#,
        "(shape (1 2 3))",
    );
    // (1 2 3) is a list but not a 2-element one
    assert!(
        !s.contains("no matching rule"),
        "should be pinpointed: {}",
        s
    );
}
