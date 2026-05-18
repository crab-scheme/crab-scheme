//! Phase 4 iter 2 — `list-of/c` and `vector-of/c` combinators.
//!
//! The variadic-element counterparts to Phase 2B.5's `list/c`.
//! Used by the cs-typer contract lowering when a typed signature
//! involves `(Listof T)` or `(Vectorof T)`.

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

// ---- list-of/c ----

#[test]
fn list_of_c_accepts_uniform_list() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define numbers (list-of/c number?))
             (list (numbers '())
                   (numbers (list 1 2 3))
                   (numbers (list 1.0 2 3.5)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #t)");
}

#[test]
fn list_of_c_rejects_mixed_list() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define numbers (list-of/c number?))
             (list (numbers (list 1 2 'oops))
                   (numbers (list 'a 'b))
                   (numbers (list \"hi\" 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f)");
}

#[test]
fn list_of_c_rejects_non_list() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define numbers (list-of/c number?))
             (list (numbers 42)
                   (numbers \"not a list\")
                   (numbers 'sym))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f)");
}

#[test]
fn list_of_c_in_arrow_contract() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define sum-numbers
               (apply-contract (-> (list-of/c number?) number?)
                               (lambda (xs)
                                 (let loop ((rest xs) (acc 0))
                                   (if (null? rest)
                                       acc
                                       (loop (cdr rest) (+ acc (car rest))))))
                               'sum-numbers))
             (sum-numbers (list 1 2 3 4 5))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "15");
    let err = rt
        .eval_str("<t>", "(sum-numbers (list 1 'bad 3))")
        .expect_err("mixed list violates list-of/c");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- vector-of/c ----

#[test]
fn vector_of_c_accepts_uniform_vector() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define strs (vector-of/c string?))
             (list (strs (vector))
                   (strs (vector \"a\" \"b\" \"c\")))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t)");
}

#[test]
fn vector_of_c_rejects_mixed_vector() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define strs (vector-of/c string?))
             (list (strs (vector \"a\" 2 \"c\"))
                   (strs (vector 1 2 3))
                   (strs (vector 'a 'b)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f)");
}

#[test]
fn vector_of_c_rejects_non_vector() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define strs (vector-of/c string?))
             (list (strs (list \"a\" \"b\"))
                   (strs 42)
                   (strs \"hi\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#f #f #f)");
}

#[test]
fn vector_of_c_in_arrow_contract() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define first-string
               (apply-contract (-> (vector-of/c string?) string?)
                               (lambda (v) (vector-ref v 0))
                               'first-string))
             (first-string (vector \"hello\" \"world\"))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "hello");
    let err = rt
        .eval_str("<t>", "(first-string (vector 1 2))")
        .expect_err("number vector violates vector-of/c");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- composition ----

#[test]
fn list_of_c_composes_with_or_c() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define list-of-num-or-str
               (list-of/c (or/c number? string?)))
             (list (list-of-num-or-str (list 1 \"a\" 2 \"b\"))
                   (list-of-num-or-str (list 1 2 3))
                   (list-of-num-or-str (list 'sym 1)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #f)");
}

#[test]
fn list_of_c_can_nest_list_of_c() {
    let mut rt = load_contract();
    let v = rt
        .eval_str(
            "<t>",
            "(define matrix-like
               (list-of/c (list-of/c number?)))
             (list (matrix-like (list (list 1 2) (list 3 4)))
                   (matrix-like (list (list 1 2) (list 'bad 4))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #f)");
}
