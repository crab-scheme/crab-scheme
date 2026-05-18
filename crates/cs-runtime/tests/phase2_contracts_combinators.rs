//! R6RS++ Phase 2B.5 — contract combinators: or/c, and/c, list/c,
//! any/c, none/c.
//!
//! Combinators are predicate-builders: they return one-arg
//! procedures returning a boolean, so they drop directly into the
//! existing `(-> dom rng)` form without grammar changes.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_contract() -> Runtime {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let mut rt = Runtime::new();
    rt.eval_str("<contract>", &src).expect("load contract.scm");
    rt
}

// ---- any/c, none/c ----

#[test]
fn any_c_accepts_anything() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(list (any/c 1) (any/c \"x\") (any/c 'sym) (any/c '()))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #t #t)");
}

#[test]
fn none_c_rejects_everything() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(list (none/c 1) (none/c \"x\") (none/c 'sym) (none/c '()))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f #f)");
}

// ---- or/c ----

#[test]
fn or_c_accepts_any_matching_alternative() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define num-or-str (or/c number? string?))
             (list (num-or-str 42) (num-or-str \"hi\") (num-or-str 'sym))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #f)");
}

#[test]
fn or_c_in_arrow_contract() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define id-num-or-str
               (apply-contract (-> (or/c number? string?) (or/c number? string?))
                               (lambda (x) x)
                               'id))
             (list (id-num-or-str 5) (id-num-or-str \"hi\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(5 hi)");
    let err = rt
        .eval_str("<t>", "(id-num-or-str 'sym)")
        .expect_err("sym matches neither number? nor string?");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

#[test]
fn or_c_empty_rejects_all() {
    let mut rt = load_contract();
    let v = rt.eval_str("<t>", "((or/c) 5)").unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- and/c ----

#[test]
fn and_c_requires_all_predicates() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define pos-num (and/c number? (lambda (x) (> x 0))))
             (list (pos-num 5) (pos-num -1) (pos-num \"hi\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f #f)");
}

#[test]
fn and_c_empty_accepts_all() {
    let mut rt = load_contract();
    let v = rt.eval_str("<t>", "((and/c) 5)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn and_c_in_arrow_contract() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define sqrt-of
               (apply-contract (-> (and/c number? (lambda (x) (>= x 0)))
                                   number?)
                               (lambda (x) (* x x))
                               'sq))
             (sqrt-of 3)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "9");
    let err = rt
        .eval_str("<t>", "(sqrt-of -1)")
        .expect_err("negative violates (>= x 0)");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- list/c ----

#[test]
fn list_c_checks_each_position() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define pair-ns (list/c number? string?))
             (list (pair-ns (list 1 \"a\"))
                   (pair-ns (list 1 2))
                   (pair-ns (list \"a\" 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f #f)");
}

#[test]
fn list_c_rejects_wrong_length() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define triple (list/c number? number? number?))
             (list (triple (list 1 2 3))
                   (triple (list 1 2))
                   (triple (list 1 2 3 4)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f #f)");
}

#[test]
fn list_c_rejects_non_list() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define pair (list/c number? number?))
             (list (pair 42) (pair \"hi\") (pair 'sym))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f)");
}

#[test]
fn list_c_empty_matches_empty_list() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define nil-only (list/c))
             (list (nil-only '()) (nil-only (list 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f)");
}

#[test]
fn list_c_in_arrow_contract() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define first-of-pair
               (apply-contract (-> (list/c number? string?) number?)
                               (lambda (p) (car p))
                               'first-of-pair))
             (first-of-pair (list 7 \"label\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "7");
    let err = rt
        .eval_str("<t>", "(first-of-pair (list 7 42))")
        .expect_err("second elem not a string");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- composition ----

#[test]
fn combinators_compose() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            // List of (number-or-string) pairs.
            "(define pair-mixed (list/c (or/c number? string?)
                                         (or/c number? string?)))
             (list (pair-mixed (list 1 \"a\"))
                   (pair-mixed (list \"a\" 2))
                   (pair-mixed (list 'sym 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #f)");
}
