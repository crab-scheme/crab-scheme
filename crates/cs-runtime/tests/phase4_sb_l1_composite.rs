//! Phase-4-sb L1.3 — composite environment construction.
//!
//! `(environment '(rnrs base) '(rnrs lists))` resolves to the
//! UNION of the two library export sets. Previously L1.1/L1.2
//! aliased both to the same RNRS_BASE_EXPORTS list, so the union
//! property was trivially satisfied but `(rnrs lists)`-specific
//! names like `for-all` weren't actually distinct. L1.3 split
//! the lists; this file verifies the split.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

// ---- (rnrs base) alone does NOT include (rnrs lists) procs ----

#[test]
fn base_alone_does_not_include_for_all() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(eval '(for-all positive? '(1 2 3)) (environment '(rnrs base)))",
        )
        .expect_err("for-all is in (rnrs lists), not (rnrs base)");
    let s = format!("{}", err);
    assert!(
        s.contains("for-all") || s.contains("unbound") || s.contains("undefined"),
        "got: {}",
        s
    );
}

#[test]
fn base_alone_does_not_include_fold_left() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str(
            "<t>",
            "(eval '(fold-left + 0 '(1 2 3)) (environment '(rnrs base)))",
        )
        .expect_err("fold-left is in (rnrs lists)");
    let s = format!("{}", err);
    assert!(
        s.contains("fold-left") || s.contains("unbound") || s.contains("undefined"),
        "got: {}",
        s
    );
}

// ---- (rnrs lists) alone DOES include them ----

#[test]
fn lists_alone_provides_for_all() {
    let mut rt = Runtime::new();
    let v = rt.eval_str(
        "<t>",
        "(eval '(for-all (lambda (x) #t) '(1 2 3)) (environment '(rnrs lists)))",
    );
    // for-all needs a predicate procedure — but (rnrs lists)
    // alone doesn't include `positive?` or `lambda`-builtins.
    // Lambda is a special form (handled by expander), so the
    // expression should at least parse + expand. The predicate
    // closure call may fail if its body uses a base-only proc;
    // here we use a constant-#t closure so the call is self-
    // contained.
    let v = v.unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

// ---- composite ((rnrs base) + (rnrs lists)) provides both ----

#[test]
fn composite_provides_base_arithmetic_and_lists_for_all() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(for-all positive? '(1 2 3))
                    (environment '(rnrs base) '(rnrs lists)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

#[test]
fn composite_provides_fold_left_with_base_arithmetic() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(fold-left + 0 '(1 2 3 4 5))
                    (environment '(rnrs base) '(rnrs lists)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "15");
}

#[test]
fn composite_provides_filter_with_base_predicate() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(filter odd? '(1 2 3 4 5))
                    (environment '(rnrs base) '(rnrs lists)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(1 3 5)");
}

// ---- order of specs doesn't matter; dedupe across overlaps ----

#[test]
fn composite_spec_order_does_not_matter() {
    let mut rt = Runtime::new();
    let v1 = rt
        .eval_str(
            "<t>",
            "(eval '(for-all positive? '(1 2))
                    (environment '(rnrs base) '(rnrs lists)))",
        )
        .unwrap();
    let v2 = rt
        .eval_str(
            "<t>",
            "(eval '(for-all positive? '(1 2))
                    (environment '(rnrs lists) '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v1), disp(&rt, &v2));
    assert_eq!(disp(&rt, &v1), "#t");
}

#[test]
fn repeated_spec_is_a_noop() {
    let mut rt = Runtime::new();
    // Dedupe makes repeating (rnrs base) idempotent.
    let v = rt
        .eval_str(
            "<t>",
            "(eval '(+ 1 2) (environment '(rnrs base) '(rnrs base) '(rnrs base)))",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "3");
}

// ---- make-namespace also takes multiple specs ----

#[test]
fn make_namespace_supports_composite_construction() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(define ns (make-namespace '(rnrs base) '(rnrs lists)))
             (eval '(fold-left + 0 '(10 20 30)) ns)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "60");
}
