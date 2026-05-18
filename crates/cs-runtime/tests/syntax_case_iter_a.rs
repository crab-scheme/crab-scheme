//! R6RS++ §12 (#118) Iter A — syntax-case foundation surface.
//!
//! Tests pin the public contract for the six identifier/datum/temp
//! builtins. Because first-class SyntaxObjects don't exist yet,
//! several semantically-distinct cases are degenerate today (e.g.
//! `bound-identifier=?` and `free-identifier=?` collapse to
//! symbol-eq); those cases are marked `#[ignore]` with a pointer to
//! the iter that will lift the limitation.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- identifier? ----

#[test]
fn identifier_p_true_for_symbol() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(identifier? 'foo)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn identifier_p_false_for_other_types() {
    let mut rt = Runtime::new();
    for src in &[
        "(identifier? 1)",
        "(identifier? \"foo\")",
        "(identifier? #t)",
        "(identifier? '())",
        "(identifier? '(a))",
    ] {
        let v = rt.eval_str("<t>", src).unwrap();
        assert_eq!(disp(&rt, &v), "#f", "for: {}", src);
    }
}

#[test]
fn identifier_p_arity_check() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(identifier?)").is_err());
    assert!(rt.eval_str("<t>", "(identifier? 'a 'b)").is_err());
}

// ---- syntax->datum ----

#[test]
fn syntax_to_datum_identity_today() {
    // Today no marks exist, so syntax->datum is an identity. The
    // test pins the API; future iters that introduce mark stripping
    // can specialize without breaking this assertion (a marked
    // identifier stripped to its underlying symbol still equals the
    // raw symbol input).
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(syntax->datum '(1 2 3))").unwrap();
    assert_eq!(disp(&rt, &v), "(1 2 3)");
    let v = rt.eval_str("<t>", "(syntax->datum 'foo)").unwrap();
    assert_eq!(disp(&rt, &v), "foo");
    let v = rt.eval_str("<t>", "(syntax->datum 42)").unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

// ---- datum->syntax ----

#[test]
fn datum_to_syntax_returns_datum() {
    // datum->syntax takes (template-id datum) and today is an
    // identity on the datum arg. Once marks land, the result will
    // be a syntax-wrapped datum that carries the template-id's
    // lexical context.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(datum->syntax 'ctx '(a b c))").unwrap();
    assert_eq!(disp(&rt, &v), "(a b c)");
}

#[test]
fn datum_to_syntax_requires_identifier_context() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(datum->syntax 42 'sym)")
        .expect_err("non-identifier context should fail");
    assert!(format!("{}", err).contains("datum->syntax"), "got: {}", err);
}

// ---- bound-identifier=? / free-identifier=? ----

#[test]
fn bound_id_eq_true_for_same_symbol() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo 'foo)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn bound_id_eq_false_for_different_symbols() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? 'foo 'bar)")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn free_id_eq_true_for_same_symbol() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(free-identifier=? 'foo 'foo)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn id_eq_predicates_reject_non_identifiers() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(bound-identifier=? 1 2)").is_err());
    assert!(rt
        .eval_str("<t>", "(free-identifier=? \"a\" \"b\")")
        .is_err());
}

#[test]
#[ignore = "blocked on SyntaxObject Value variant (post-1.0 track)"]
fn bound_id_eq_distinguishes_marked_identifiers() {
    // Two identifiers with the same readable name but introduced
    // via different macro call sites should compare unequal under
    // R6RS bound-identifier=?. Today they collapse to symbol-eq
    // because Value::Symbol doesn't carry per-call marks.
    //
    // The fix requires adding a `Value::Identifier { name, mark }`
    // variant that the syntax-case template instantiator stamps
    // with a fresh per-expansion mark; bound-identifier=? then
    // compares both fields. The migration touches ~45 files that
    // currently `match` on `Value::Symbol(_)` -- tracked as a
    // post-1.0 task in the plan doc's Iter E status section.
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"
        (define-syntax mark-a
          (syntax-rules () ((_ id) (quote id))))
        (define-syntax mark-b
          (syntax-rules () ((_ id) (quote id))))
        "#,
    )
    .unwrap();
    let v = rt
        .eval_str("<t>", "(bound-identifier=? (mark-a x) (mark-b x))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f", "marks should distinguish");
}

// ---- generate-temporaries ----

#[test]
fn generate_temporaries_returns_n_fresh_identifiers() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(length (generate-temporaries '(a b c d)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "4");
}

#[test]
fn generate_temporaries_each_temp_is_identifier() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", "(map identifier? (generate-temporaries '(a b c)))")
        .unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #t)");
}

#[test]
fn generate_temporaries_yields_distinct_names() {
    // Two separate calls should never produce overlapping names.
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            r#"
        (let ((a (generate-temporaries '(x y z)))
              (b (generate-temporaries '(p q r))))
          ;; Every name in `a` should be distinct from every name in `b`.
          (let loop ((xs a))
            (if (null? xs)
                #t
                (if (member (car xs) b)
                    #f
                    (loop (cdr xs))))))
        "#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn generate_temporaries_empty_list_yields_empty() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(generate-temporaries '())").unwrap();
    assert_eq!(disp(&rt, &v), "()");
}

#[test]
fn generate_temporaries_rejects_non_list() {
    let mut rt = Runtime::new();
    assert!(rt.eval_str("<t>", "(generate-temporaries 42)").is_err());
}
