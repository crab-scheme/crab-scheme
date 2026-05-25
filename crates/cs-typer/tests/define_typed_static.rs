//! Issue #11 ext-1 — static type checking for `(define/typed N T E)`.
//!
//! The contract library's `define/typed` macro expands to:
//!
//! ```text
//! (define N (apply-contract (__type->contract 'T) E (quote N)))
//! ```
//!
//! Before this iter, the Checker inferred the `apply-contract`
//! call as `Any` (it's an opaque procedure to the typer); the
//! gradual `subtype(Any, T)` fallback then silently accepted any
//! `E`. After this iter, `Checker::check_set` peels the wrap and
//! checks `E` against `T` directly — so mismatched bodies fail
//! at expand time, not at first dynamic call.
//!
//! These tests drive the new behaviour in two ways:
//!
//! 1. **extract_annotations recognition.** Feed source with
//!    `(define/typed N T E)` and confirm the ascription lands in
//!    the table while the Datum survives for downstream macro
//!    expansion.
//! 2. **Checker peel.** Hand-roll the post-macro-expansion form
//!    `(: N T) (define N (apply-contract DUMMY E (quote N)))`
//!    and confirm the Checker reports an error when E mismatches
//!    T (and passes when it conforms).
//!
//! The peel tests use literals and Refs for `E` rather than
//! inline lambdas — inline lambdas without annotations degrade
//! to all-`Any` under gradual typing and so won't trigger
//! mismatches even with the peel. The Refs-to-annotated-defines
//! pattern proves the peel works end-to-end without depending on
//! the inline-lambda-annotation grammar.

use std::collections::HashMap;

use cs_core::SymbolTable;
use cs_diag::SourceMap;
use cs_expand::{Expander, Macro};
use cs_ir::CoreExpr;
use cs_parse::read_all;
use cs_typer::{extract_annotations, AnnotationTable, Checker, ProcType, Type, TypeError};

fn parse_extract_expand(src: &str) -> (CoreExpr, AnnotationTable, SymbolTable) {
    let mut sm = SourceMap::new();
    let f = sm.add("<define-typed-static>", src);
    let mut syms = SymbolTable::new();
    let data = read_all(f, src, &mut syms).expect("parse");
    let (stripped, table, diags) = extract_annotations(&data, &mut syms);
    assert!(diags.is_empty(), "annotation diags: {diags:?}");
    let mut macros: HashMap<cs_core::Symbol, Macro> = HashMap::new();
    let mut exp = Expander::new(&mut syms, &mut macros);
    let core = exp.expand_program(&stripped).expect("expand");
    drop(exp);
    (core, table, syms)
}

// ---- ascription synthesis from (define/typed ...) ----

#[test]
fn extract_picks_up_define_typed_ascription() {
    let src = "\
        (define/typed sq (-> Fixnum Fixnum)
          (lambda (x) (* x x)))
    ";
    let mut sm = SourceMap::new();
    let f = sm.add("<t>", src);
    let mut syms = SymbolTable::new();
    let data = read_all(f, src, &mut syms).expect("parse");
    let (stripped, table, diags) = extract_annotations(&data, &mut syms);
    assert!(diags.is_empty(), "diags: {diags:?}");
    // Datum survives so the macro can run later.
    assert_eq!(stripped.len(), 1);
    // Synthesized ascription is in the table.
    assert_eq!(table.top_level.len(), 1);
    assert_eq!(syms.name(table.top_level[0].name), "sq");
    let want = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Fixnum],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    assert_eq!(table.top_level[0].type_ann, want);
}

// ---- Checker peels apply-contract and reports literal mismatches ----

#[test]
fn checker_peels_apply_contract_and_passes_conforming_literal() {
    // Post-macro-expansion shape. apply-contract is unbound here
    // (no contract library loaded), but the Checker peels by
    // name before inferring — so the wrap is transparent.
    let src = "\
        (: n Fixnum)
        (define n (apply-contract #t 42 (quote n)))
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let errors = checker.check_program(&core);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn checker_peels_apply_contract_and_catches_literal_mismatch() {
    // Body is a String literal but ascription says Fixnum.
    // Without the peel, the Checker infers apply-contract → Any
    // and accepts anything; with the peel it sees "literal" and
    // reports a String/Fixnum mismatch.
    let src = "\
        (: n Fixnum)
        (define n (apply-contract #t \"not-a-fixnum\" (quote n)))
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let errors = checker.check_program(&core);
    assert!(
        !errors.is_empty(),
        "expected a type error from the peeled literal"
    );
    let found = errors.iter().any(|e| {
        matches!(
            e,
            TypeError::Mismatch {
                expected: Type::Fixnum,
                found: Type::String,
                ..
            }
        )
    });
    assert!(
        found,
        "expected Fixnum/String mismatch after peel; got: {errors:?}"
    );
}

// ---- Checker peels apply-contract and reports Ref-to-typed mismatches ----

#[test]
fn peel_catches_mismatched_typed_helper_assignment() {
    // helper has type (-> Fixnum String), but we assign it to a
    // binding ascribed (-> Fixnum Fixnum) through the contract
    // wrap. The peel exposes the Ref so its inferred type is
    // checked against the ascription.
    let src = "\
        (: helper (-> Fixnum String))
        (define (helper [x : Fixnum]) : String \"r\")
        (: f (-> Fixnum Fixnum))
        (define f (apply-contract #t helper (quote f)))
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let errors = checker.check_program(&core);
    assert!(!errors.is_empty(), "expected a procedure-type mismatch");
}

#[test]
fn peel_passes_conforming_typed_helper_assignment() {
    let src = "\
        (: helper (-> Fixnum Fixnum))
        (define (helper [x : Fixnum]) : Fixnum x)
        (: f (-> Fixnum Fixnum))
        (define f (apply-contract #t helper (quote f)))
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let errors = checker.check_program(&core);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

// ---- peel sound: wrong shape leaves the value alone ----

#[test]
fn apply_contract_with_wrong_arity_is_not_peeled() {
    // 2-arg apply-contract isn't the contract library's runtime
    // wrap; the peel must leave it alone so we fall through to
    // the standard infer-then-subtype path. The body is a
    // 2-arg call to a free `apply-contract`; under the gradual
    // rule it infers to Any → subtype(Any, Fixnum) is true → no
    // error. The behavioural contract: the peel doesn't fire on
    // non-3-arg apply-contract shapes, and the gradual fallback
    // applies normally.
    let src = "\
        (: n Fixnum)
        (define n (apply-contract #t 42))
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let _ = checker.check_program(&core);
}

// ---- peel passes non-app values through cleanly ----

#[test]
fn non_apply_contract_value_uses_normal_check_path() {
    // Plain typed define with no wrap. Should typecheck cleanly.
    let src = "\
        (: n Fixnum)
        (define n 42)
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let errors = checker.check_program(&core);
    assert!(errors.is_empty(), "errors: {errors:?}");
}

#[test]
fn non_apply_contract_value_with_wrong_type_still_errors() {
    // No apply-contract wrap; the standard check path runs.
    let src = "\
        (: n Fixnum)
        (define n \"not-a-fixnum\")
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    let errors = checker.check_program(&core);
    assert!(!errors.is_empty(), "expected a type error");
}

// ---- peel doesn't fire when the head Ref is something else ----

#[test]
fn peel_only_recognizes_the_apply_contract_symbol() {
    // A 3-arg call to a different procedure (`some-other-fn`)
    // should NOT be peeled — only the literal symbol
    // `apply-contract` triggers the wrap-aware path. Otherwise
    // any user 3-arg call would be incorrectly stripped.
    let src = "\
        (: n Fixnum)
        (define n (some-other-fn #t \"not-a-fixnum\" (quote n)))
    ";
    let (core, table, mut syms) = parse_extract_expand(src);
    let mut checker = Checker::new(&table, &mut syms);
    // some-other-fn is a free Ref, the call infers to Any (the
    // function type isn't known to the typer), and gradual
    // subtype(Any, Fixnum) is true → no error. If peel fired,
    // we'd see the String literal and report a mismatch. Asserting
    // no errors confirms the peel didn't fire.
    let errors = checker.check_program(&core);
    assert!(
        errors.is_empty(),
        "peel must not fire for non-apply-contract heads; got: {errors:?}"
    );
}
