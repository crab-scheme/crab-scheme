//! R6RS++ Phase 1.5 Iter C — syntax-case template instantiator
//! stamps non-pvar identifiers with per-form-evaluation marks.
//!
//! After this iter, a `(syntax T)` template inside a syntax-case
//! body emits `(make-identifier 'foo __sc-mark__)` rather than
//! `(quote foo)` for each non-pvar identifier `foo` in `T`. The
//! `__sc-mark__` binding is a fresh `(fresh-mark!)` call per
//! syntax-case form invocation, so two runs of the same
//! macro-defining-syntax produce identifiers that compare
//! unequal under R6RS hygiene predicates (which Iter 1.5.D
//! wires up).
//!
//! Iter 1.5.C tests verify the value-shape changes only:
//! - non-pvar identifiers in templates produce `Value::Identifier`
//! - pvar substitutions retain their original kind (Symbol if
//!   the user supplied a bare symbol)
//! - two `(syntax foo)` calls inside the same syntax-case body
//!   share a mark
//! - two distinct syntax-case form evaluations produce identifiers
//!   with different marks
//! - standalone `(syntax foo)` outside any syntax-case gets mark=0
//!
//! The bound-identifier=? side of the story (comparing
//! identifiers by (name, mark)) lands in 1.5.D.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- value-shape verification via `identifier?` ----

#[test]
fn template_introduced_identifier_is_identifier() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (identifier? (syntax foo))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn pvar_substitution_retains_symbol_kind() {
    // The user supplies a bare symbol 'user-sym as the pvar
    // value. Inside the template, `(syntax x)` returns the pvar
    // value -- which is still a bare Symbol (pvars don't gain
    // marks from the template-introduction mechanism).
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'user-sym ()
               (x (symbol? (syntax x))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- mark sharing across (syntax T) calls within one form ----

#[test]
fn two_syntax_forms_in_one_clause_share_mark() {
    // `(eq? a b)` on two identifiers compares name+mark per
    // Phase 1.5 Iter A's variant semantics. Two `(syntax foo)`
    // inside the same syntax-case body must share a mark, so
    // eq? returns #t.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (eq? (syntax foo) (syntax foo))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn two_syntax_forms_with_different_names_compare_unequal() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (eq? (syntax foo) (syntax bar))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- mark distinction across syntax-case form evaluations ----

#[test]
fn two_distinct_syntax_case_forms_produce_different_marks() {
    // The eq? hits when both invocations share the same mark.
    // Distinct form-evaluations should generate distinct marks
    // so the eq? returns #f.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(let ((a (syntax-case 'whatever () (_ (syntax intro))))
                   (b (syntax-case 'whatever () (_ (syntax intro)))))
               (eq? a b))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn syntax_case_inside_function_gets_fresh_mark_per_call() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define (intro)
               (syntax-case 'whatever () (_ (syntax foo))))
             (eq? (intro) (intro))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- standalone (syntax T) uses mark=0 ----

#[test]
fn standalone_syntax_forms_share_mark_zero() {
    // Outside any syntax-case body, `(syntax T)` falls back to
    // mark=0 -- two standalone calls produce equal identifiers.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(eq? (syntax foo) (syntax foo))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- syntax->datum strips marks ----

#[test]
fn syntax_to_datum_strips_identifier_to_symbol() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (symbol? (syntax->datum (syntax foo)))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn syntax_to_datum_name_matches_template_symbol() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case 'whatever ()
               (_ (eq? (syntax->datum (syntax my-name)) 'my-name)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- display still hides marks ----

#[test]
fn template_identifier_displays_as_bare_name() {
    // The mark is observable via eq?/bound-identifier=?, but
    // write/display still print the bare name -- so existing
    // tests that assert on stringified output keep working.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(syntax-case '(arg) ()
               ((x) (syntax (wrapper x))))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(wrapper arg)");
}

// ---- raw builtins ----

#[test]
fn fresh_mark_returns_distinct_fixnums() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(eq? (fresh-mark!) (fresh-mark!))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn make_identifier_builds_identifier_value() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(identifier? (make-identifier 'foo 42))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn make_identifier_with_same_name_and_mark_eq() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eq? (make-identifier 'foo 7) (make-identifier 'foo 7))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn make_identifier_with_different_marks_not_eq() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eq? (make-identifier 'foo 7) (make-identifier 'foo 8))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn identifier_vs_symbol_not_eq() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(eq? 'foo (make-identifier 'foo 0))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}
