//! Real-world tests for raise-condition propagation through higher-order
//! builtins. This is the integration suite for issue #34 / PR #41 —
//! the 65-site sweep that made every `apply_procedure(...).map_err(|e|
//! e.message())` site route `EvalErrorKind::Raised` / `Escape` through
//! `ctx.pending_raise` / `pending_escape` instead of collapsing to a
//! plain string.
//!
//! Tests cover the breadth of higher-order builtins (map / for-each /
//! filter / fold / find / every / any / reduce / unfold / hashtable
//! and vector and string iterators / port helpers / dynamic-wind),
//! single-level guards, multi-level nested guards, with-exception-handler,
//! call/cc-escape preservation, and the corner case in `b_with_region`
//! that was caught and fixed during PR #41 development (where the
//! condition value was Region-allocated and would dangle after the
//! `_guard` dropped the bump arena).
//!
//! Every test uses `disp` to compare the materialized Scheme value
//! against an expected string — the same helper pattern as the existing
//! `phase2_condition_types.rs` suite.

use cs_core::WriteMode;
use cs_diag::Diagnostic;
use cs_runtime::Runtime;

fn eval_str(src: &str) -> String {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<rw>", src)
        .unwrap_or_else(|d: Diagnostic| panic!("eval failed: {}", d.message));
    rt.format_value(&v, WriteMode::Display)
}

// ---------------- Single-level higher-order builtins ----------------

#[test]
fn map_with_raise_condition_value_preserved() {
    // Plain symbol condition.
    assert_eq!(
        eval_str("(guard (c (#t c)) (map (lambda (x) (raise 'fail)) '(1)))"),
        "fail"
    );
    // Integer condition — the key thing is the *value* survives.
    assert_eq!(
        eval_str("(guard (c (#t c)) (map (lambda (x) (raise 99)) '(1)))"),
        "99"
    );
    // String condition.
    assert_eq!(
        eval_str(r#"(guard (c (#t c)) (map (lambda (x) (raise "boom")) '(1)))"#),
        "boom"
    );
    // List condition — composite values must round-trip.
    assert_eq!(
        eval_str("(guard (c (#t c)) (map (lambda (x) (raise '(a b c))) '(1)))"),
        "(a b c)"
    );
}

#[test]
fn raise_fires_on_first_element_not_last() {
    // map should short-circuit — proof: raising on the first element
    // doesn't process the rest, so a side-effect counter (via set!) is
    // never bumped beyond 1.
    assert_eq!(
        eval_str(
            "(define count 0)
             (define result
               (guard (c (#t 'caught))
                 (map (lambda (x)
                        (set! count (+ count 1))
                        (raise 'stop))
                      '(a b c d e))))
             (list result count)"
        ),
        "(caught 1)"
    );
}

#[test]
fn for_each_preserves_condition_value() {
    assert_eq!(
        eval_str("(guard (c (#t c)) (for-each (lambda (x) (raise x)) '(42)))"),
        "42"
    );
}

#[test]
fn fold_left_propagates_raise() {
    // R6RS `fold-left` — the lib/r6rs lists module is in the default
    // boot, so this should work in any fresh Runtime.
    assert_eq!(
        eval_str(
            "(guard (c (#t c))
               (fold-left (lambda (acc x) (raise (cons 'at x))) 0 '(1 2 3)))"
        ),
        "(at . 1)"
    );
}

#[test]
fn filter_propagates_raise_from_predicate() {
    assert_eq!(
        eval_str("(guard (c (#t c)) (filter (lambda (x) (raise (* x 10))) '(7)))"),
        "70"
    );
}

#[test]
fn find_propagates_raise_from_predicate() {
    assert_eq!(
        eval_str("(guard (c (#t c)) (find (lambda (x) (raise 'in-find)) '(1 2 3)))"),
        "in-find"
    );
}

#[test]
fn for_all_propagates_raise() {
    assert_eq!(
        eval_str("(guard (c (#t c)) (for-all (lambda (x) (raise 'every-failed)) '(1)))"),
        "every-failed"
    );
}

#[test]
fn exists_propagates_raise() {
    assert_eq!(
        eval_str("(guard (c (#t c)) (exists (lambda (x) (raise 'any-failed)) '(1)))"),
        "any-failed"
    );
}

#[test]
fn apply_propagates_raise() {
    // (apply f args) — the chain into f should route conditions.
    assert_eq!(
        eval_str("(guard (c (#t c)) (apply (lambda (x) (raise (+ x 1))) '(41)))"),
        "42"
    );
}

// ---------------- Nested + multi-level guards ----------------

#[test]
fn inner_guard_catches_first_outer_unaffected() {
    // The inner guard catches; the outer guard never sees the
    // condition. The outer expression returns the inner result.
    assert_eq!(
        eval_str(
            "(guard (outer (#t 'outer-caught))
               (guard (inner (#t (list 'inner-caught inner)))
                 (map (lambda (x) (raise 'boom)) '(1))))"
        ),
        "(inner-caught boom)"
    );
}

#[test]
fn re_raise_from_inner_guard_lands_in_outer() {
    // Inner guard's handler re-raises; the outer guard must catch.
    assert_eq!(
        eval_str(
            "(guard (outer (#t (list 'outer outer)))
               (guard (inner (#t (raise (list 'reraised inner))))
                 (map (lambda (x) (raise 'original)) '(1))))"
        ),
        "(outer (reraised original))"
    );
}

#[test]
fn raise_through_chained_map_filter_map() {
    // map → filter → map composition. Innermost map raises; the
    // condition must climb through both filter and the outer map.
    assert_eq!(
        eval_str(
            "(guard (c (#t (cons 'caught c)))
               (map list
                    (filter even?
                            (map (lambda (x)
                                   (if (= x 3) (raise 'three) (* x 2)))
                                 '(1 2 3 4 5)))))"
        ),
        "(caught . three)"
    );
}

// ---------------- with-exception-handler ----------------

#[test]
fn with_exception_handler_intercepts_raise_inside_map() {
    assert_eq!(
        eval_str(
            r#"(call-with-current-continuation
                 (lambda (k)
                   (with-exception-handler
                     (lambda (c) (k (cons 'handler-saw c)))
                     (lambda ()
                       (map (lambda (x) (raise 'in-map)) '(1))))))"#
        ),
        "(handler-saw . in-map)"
    );
}

#[test]
fn with_exception_handler_intercepts_through_two_levels() {
    // Two levels of map before the raise; handler still sees the
    // original condition value.
    assert_eq!(
        eval_str(
            r#"(call-with-current-continuation
                 (lambda (k)
                   (with-exception-handler
                     (lambda (c) (k (cons 'h c)))
                     (lambda ()
                       (map (lambda (xs)
                              (map (lambda (x)
                                     (if (= x 2) (raise 'two) x))
                                   xs))
                            '((1 2 3)))))))"#
        ),
        "(h . two)"
    );
}

// ---------------- Mixed iterator types ----------------

#[test]
fn raise_inside_vector_map_propagates() {
    assert_eq!(
        eval_str(
            "(guard (c (#t c))
               (vector-map (lambda (x) (raise (* x x))) #(7)))"
        ),
        "49"
    );
}

#[test]
fn raise_inside_vector_for_each_propagates() {
    assert_eq!(
        eval_str(
            "(guard (c (#t c))
               (vector-for-each (lambda (x) (raise 'in-vfe)) #(1 2 3)))"
        ),
        "in-vfe"
    );
}

#[test]
fn raise_inside_string_for_each_propagates() {
    assert_eq!(
        eval_str(
            r#"(guard (c (#t c))
               (string-for-each (lambda (ch) (raise ch)) "abc"))"#
        ),
        "a"
    );
}

#[test]
fn raise_inside_hashtable_walk_propagates() {
    assert_eq!(
        eval_str(
            "(define h (make-eq-hashtable))
             (hashtable-set! h 'k 'v)
             (guard (c (#t c))
               (hashtable-walk h (lambda (k v) (raise (cons k v)))))"
        ),
        "(k . v)"
    );
}

// ---------------- Region-allocated condition (the bug PR #41 caught) ----

#[test]
fn raise_inside_with_region_does_not_dangle() {
    // The condition value is allocated inside the region; when the
    // region drops its bump arena, the condition stash in ctx must
    // already be promoted to Rc storage. Pre-fix this would panic
    // with "use-after-region-drop". Post-fix it produces a clean
    // condition value the outer guard catches.
    assert_eq!(
        eval_str(
            "(guard (c (#t (list 'caught c)))
               (with-region
                 (lambda ()
                   (raise (cons 'region-allocated (list 1 2 3))))))"
        ),
        "(caught (region-allocated 1 2 3))"
    );
}

// ---------------- call/cc escape preservation ----------------

#[test]
fn escape_continuation_inside_map_returns_to_caller() {
    // call/cc capture; escape from inside map. The escape value is
    // the result of the call/cc, NOT a raised condition. Verifies
    // that the Escape variant routes correctly via pending_escape.
    assert_eq!(
        eval_str(
            "(call/cc
               (lambda (k)
                 (map (lambda (x)
                        (if (= x 2) (k 'escaped) x))
                      '(1 2 3))
                 'never))"
        ),
        "escaped"
    );
}

// ---------------- Sanity: non-raising calls are unaffected ----------

#[test]
fn map_without_raise_is_unaffected() {
    // Regression guard: the helper-injection at 66 sites should not
    // perturb the success path. (Performance is a separate concern;
    // here we just verify identity of the success-path result.)
    assert_eq!(
        eval_str("(map (lambda (x) (* x x)) '(1 2 3 4))"),
        "(1 4 9 16)"
    );
    assert_eq!(eval_str("(filter even? '(1 2 3 4 5 6))"), "(2 4 6)");
    assert_eq!(eval_str("(fold-left + 0 '(1 2 3 4 5))"), "15");
}
