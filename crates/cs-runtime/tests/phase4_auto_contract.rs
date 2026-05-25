//! Issue #11 ext-2 — library-export auto-contracting at runtime.
//!
//! When a library declares `(export NAME ...)` and `NAME` has an
//! ascription either inside the library body or at the
//! top-level above it, cs-runtime's `eval_data_in_env`
//! pipeline now injects a `(set! NAME (apply-contract ... NAME 'NAME))`
//! immediately after the binding's `define`. The wrap fires at
//! every untyped call into the library, producing a clear
//! `&contract-violation` instead of the silent type error the
//! callee would otherwise encounter.
//!
//! The ascription uses cs-typer's type-annotation grammar
//! (`Fixnum`, `Flonum`, `(-> T1 T2)`, `(Listof T)`, `(->* doms rest rng)`, …) —
//! the auto-contract pass lowers these to runtime contracts
//! (`integer?`, `real?`, `(-> integer? integer?)`, etc.) when
//! it injects the wrap.
//!
//! Coverage:
//! - ascription INSIDE the library body — most common form
//! - ascription OUTSIDE the library, at file scope — fallback
//! - mixed ascribed + unascribed exports (only ascribed wrap)
//! - untyped library is unchanged (no wrap, no contract import)
//! - variadic tail (`(->*)`) lowers to `(->* ...)` contract
//!
//! The wrap is opt-in via the user library importing the
//! contract library — without that import the injected
//! `apply-contract` would be unbound. Each test loads
//! contract.scm explicitly.

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn load_contract(rt: &mut Runtime) {
    let contract_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let src = std::fs::read_to_string(&contract_path).unwrap();
    rt.eval_str("<contract>", &src).unwrap();
}

#[test]
fn ascribed_export_wraps_with_contract_at_runtime() {
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    // Define a library whose export `f` is ascribed but NOT
    // wrapped via `define/typed`. The auto-contract pass should
    // inject a wrap after the define so calls from outside the
    // library go through the contract check.
    rt.eval_str(
        "<lib>",
        "(library (svc) \
           (export f) \
           (import (rnrs)) \
           (: f (-> Fixnum Fixnum)) \
           (define (f x) (* x 2)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (svc))").unwrap();
    // Good call: f(5) → 10.
    let v = rt.eval_str("<call>", "(f 5)").unwrap();
    assert_eq!(disp(&rt, &v), "10");
    // Bad call: f('not-a-fixnum) should produce a contract
    // violation, not a silent type error in `*`.
    let v = rt
        .eval_str(
            "<bad>",
            "(guard (c ((contract-violation? c) 'caught)) (f 'oops))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

#[test]
fn untyped_library_is_unchanged() {
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    // No ascription — no wrap injected. The library should
    // behave exactly as it did pre-ext-2.
    rt.eval_str(
        "<lib>",
        "(library (svc) \
           (export f) \
           (import (rnrs)) \
           (define (f x) (* x 2)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (svc))").unwrap();
    let v = rt.eval_str("<call>", "(f 7)").unwrap();
    assert_eq!(disp(&rt, &v), "14");
}

#[test]
fn unascribed_export_is_unwrapped_even_in_typed_library() {
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    // Only `f` is ascribed; `g` exports unwrapped.
    rt.eval_str(
        "<lib>",
        "(library (svc) \
           (export f g) \
           (import (rnrs)) \
           (: f (-> Fixnum Fixnum)) \
           (define (f x) (* x 2)) \
           (define (g x) x))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (svc))").unwrap();
    // g accepts any value — no contract.
    let v = rt.eval_str("<g>", "(g 'whatever)").unwrap();
    assert_eq!(disp(&rt, &v), "whatever");
    // f rejects non-integers via the wrap.
    let v = rt
        .eval_str(
            "<f-bad>",
            "(guard (c ((contract-violation? c) 'caught)) (f 'oops))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

#[test]
fn unrelated_internal_ascription_doesnt_wrap_export() {
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    // `helper` is ascribed but NOT exported; `f` is exported
    // but NOT ascribed. Neither should be auto-wrapped.
    rt.eval_str(
        "<lib>",
        "(library (svc) \
           (export f) \
           (import (rnrs)) \
           (: helper (-> Fixnum Fixnum)) \
           (define (helper x) x) \
           (define (f x) (helper x)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (svc))").unwrap();
    // f accepts anything (no wrap).
    let v = rt.eval_str("<call>", "(f 'whatever)").unwrap();
    assert_eq!(disp(&rt, &v), "whatever");
}

#[test]
fn outside_library_ascription_falls_through_as_fallback() {
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    // Ascription at the file's top level, BEFORE the library
    // declaration. extract_annotations records it; the library
    // body has no local ascription; the auto-contract pass
    // falls back to the table.
    rt.eval_str(
        "<top>",
        "(: f (-> Fixnum Fixnum)) \
         (library (svc) \
           (export f) \
           (import (rnrs)) \
           (define (f x) (* x 2)))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (svc))").unwrap();
    let v = rt
        .eval_str(
            "<bad>",
            "(guard (c ((contract-violation? c) 'caught)) (f 'oops))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}

#[test]
fn ascription_form_no_longer_errors_in_runtime_path() {
    // Before ext-2, `(: x Fixnum)` would fail at expand time as
    // an unbound `:` reference. After ext-2, `extract_annotations`
    // strips it from the data stream. This test confirms the
    // sky doesn't fall when a user writes the form outside any
    // library context.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(: x Fixnum) (define x 42) x").unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn variadic_export_lowers_to_arrow_star_wrap() {
    let mut rt = Runtime::new();
    load_contract(&mut rt);
    // (->* (Fixnum) Fixnum Fixnum) — first arg Fixnum mandatory,
    // rest are Fixnum, result is Fixnum. The contract lowers to
    // (->* (integer?) integer? integer?). Using the explicit
    // `(lambda (tag . xs) …)` shape so we don't depend on cs-expand
    // accepting the `(define (f a . r) …)` sugar (the existing
    // phase4_define_typed.rs tests use this same idiom).
    rt.eval_str(
        "<lib>",
        "(library (svc) \
           (export tag-sum) \
           (import (rnrs)) \
           (: tag-sum (->* (Fixnum) Fixnum Fixnum)) \
           (define tag-sum (lambda (tag . xs) (+ tag (apply + xs)))))",
    )
    .unwrap();
    rt.eval_str("<use>", "(import (svc))").unwrap();
    let v = rt.eval_str("<good>", "(tag-sum 1 2 3 4)").unwrap();
    assert_eq!(disp(&rt, &v), "10");
    // First arg wrong → contract violation on the mandatory
    // arg.
    let v = rt
        .eval_str(
            "<bad>",
            "(guard (c ((contract-violation? c) 'caught)) (tag-sum 'oops 1 2))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "caught");
}
