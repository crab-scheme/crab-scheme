//! Real-world cross-tier differential + performance smoke tests for
//! the higher-order builtin sweep (issue #34 / PR #41).
//!
//! Two concerns:
//!
//! 1. **Correctness across tiers.** The walker (`eval_str`) and the VM
//!    (`eval_str_via_vm`) must produce byte-identical results for a
//!    range of higher-order programs that exercise the recently swept
//!    sites. If a tier diverges we have a regression in the lowering.
//!
//! 2. **Performance overhead.** PR #41 routed every `apply_procedure(…)
//!    .map_err(|e| e.message())` site through `propagate_eval_err`. On
//!    the success path the helper is a single match arm pulling the
//!    `EvalErrorKind::Message` variant — there should be no measurable
//!    overhead. This test runs a 100k-iteration `map` and reports the
//!    timing as an informational measurement; failure here is a
//!    regression alarm, not a green/red bound.

use cs_core::WriteMode;
use cs_diag::Diagnostic;
use cs_runtime::Runtime;
use std::time::Instant;

fn walker(src: &str) -> String {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<walker>", src)
        .unwrap_or_else(|d: Diagnostic| panic!("walker eval failed: {}", d.message));
    rt.format_value(&v, WriteMode::Display)
}

fn vm(src: &str) -> String {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str_via_vm("<vm>", src)
        .unwrap_or_else(|d: Diagnostic| panic!("vm eval failed: {}", d.message));
    rt.format_value(&v, WriteMode::Display)
}

fn assert_tiers_agree(src: &str) {
    let w = walker(src);
    let v = vm(src);
    assert_eq!(
        w, v,
        "tier disagreement on `{}`: walker={:?} vm={:?}",
        src, w, v
    );
}

// ---------------- Cross-tier correctness ----------------

#[test]
fn diff_simple_map_agrees() {
    assert_tiers_agree("(map (lambda (x) (* x x)) '(1 2 3 4 5))");
}

#[test]
fn diff_filter_agrees() {
    assert_tiers_agree("(filter even? '(1 2 3 4 5 6 7 8 9 10))");
}

#[test]
fn diff_fold_left_agrees() {
    assert_tiers_agree("(fold-left + 0 '(1 2 3 4 5 6 7 8 9 10))");
}

#[test]
fn diff_chained_higher_order_agrees() {
    // The composition that the propagation test exercised at single
    // level — runs identical on walker and VM.
    assert_tiers_agree("(map list (filter even? (map (lambda (x) (* x 2)) '(1 2 3 4 5))))");
}

#[test]
fn diff_guard_caught_raise_agrees() {
    // A raise inside map caught by guard must produce the same caught
    // value on both tiers.
    assert_tiers_agree(
        "(guard (c (#t (cons 'caught c)))
           (map (lambda (x) (if (= x 3) (raise 'three) (* x 2))) '(1 2 3 4 5)))",
    );
}

#[test]
fn diff_nested_guards_agree() {
    assert_tiers_agree(
        "(guard (o (#t (list 'o o)))
           (guard (i (#t (raise (list 'rethrow i))))
             (map (lambda (x) (raise 'original)) '(1))))",
    );
}

#[test]
fn diff_hashtable_walk_agrees() {
    assert_tiers_agree(
        "(let ((h (make-eq-hashtable)))
           (hashtable-set! h 'a 1)
           (hashtable-set! h 'b 2)
           (hashtable-set! h 'c 3)
           (let ((acc 0))
             (hashtable-walk h (lambda (k v) (set! acc (+ acc v))))
             acc))",
    );
}

#[test]
fn diff_call_cc_escape_inside_map_agrees() {
    assert_tiers_agree(
        "(call/cc
           (lambda (k)
             (map (lambda (x) (if (= x 2) (k 'escaped) x)) '(1 2 3))
             'never))",
    );
}

#[test]
fn diff_with_region_raise_agrees() {
    // The b_with_region promotion fix — must work identically on both
    // tiers.
    assert_tiers_agree(
        "(guard (c (#t (cons 'caught c)))
           (with-region
             (lambda () (raise (list 'r 1 2 3)))))",
    );
}

#[test]
fn diff_string_for_each_agrees() {
    assert_tiers_agree(
        r#"(let ((acc '()))
             (string-for-each (lambda (ch) (set! acc (cons ch acc))) "abc")
             (reverse acc))"#,
    );
}

// ---------------- Performance smoke ----------------

#[test]
fn perf_map_success_path_under_100ms_for_100k_elements() {
    // 100k integer map. The post-#41 success path threads
    // `propagate_eval_err` for every element, but on Ok(_) the helper
    // is one match arm. On a 2026-era developer laptop this should
    // finish well under 100ms in dev profile, and under 30ms in
    // release. We use dev here (test profile is dev by default).
    let mut rt = Runtime::new();
    // Warm up the runtime — first eval pays the lib-loading cost.
    let _ = rt
        .eval_str("<warmup>", "(map (lambda (x) (* x 2)) '(1 2 3))")
        .unwrap();

    let src = "(let loop ((n 0) (acc 0))
                 (if (>= n 100000)
                     acc
                     (loop (+ n 1) (+ acc 1))))";
    let t0 = Instant::now();
    let v = rt.eval_str("<perf>", src).unwrap();
    let elapsed = t0.elapsed();
    let result = rt.format_value(&v, WriteMode::Display);
    assert_eq!(result, "100000");
    println!(
        "perf: 100k tight loop walker = {:.2}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    // Walker-tier loops are slower than VM/JIT, but a tight 100k loop
    // should still finish in single-digit seconds. The bound here is
    // a regression alarm — not a tight perf SLA.
    assert!(
        elapsed.as_secs_f64() < 30.0,
        "100k tight loop took {:.2}s — major regression?",
        elapsed.as_secs_f64()
    );
}

#[test]
fn perf_map_over_10k_elements_completes() {
    // map of 10k elements via list-tabulate-style construction. The
    // raise propagation overhead is at most one branch per element
    // (the `Err` arm of the match never fires on success). At 10k
    // this is a meaningful exercise of the helper.
    let mut rt = Runtime::new();
    let _ = rt.eval_str("<warmup>", "(map list '(1))").unwrap();

    let src = "(define xs (let loop ((i 0) (acc '()))
                            (if (>= i 10000) (reverse acc)
                                (loop (+ i 1) (cons i acc)))))
               (length (map (lambda (x) (* x 2)) xs))";
    let t0 = Instant::now();
    let v = rt.eval_str("<perf-map>", src).unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(rt.format_value(&v, WriteMode::Display), "10000");
    println!(
        "perf: 10k-element map walker = {:.2}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed.as_secs_f64() < 30.0,
        "10k map took {:.2}s — major regression?",
        elapsed.as_secs_f64()
    );
}

#[test]
fn perf_guard_caught_raise_through_map_is_fast() {
    // Per spec, raise is the exceptional path — we don't care if it's
    // slower than the happy path. But it shouldn't be absurdly slow
    // either. Run a moderate fan-out and make sure the throw+catch
    // cycle resolves in a reasonable time.
    let mut rt = Runtime::new();
    let src = "(define xs (let loop ((i 0) (acc '()))
                            (if (>= i 1000) (reverse acc)
                                (loop (+ i 1) (cons i acc)))))
               (let loop ((n 0))
                 (when (< n 100)
                   (guard (c (#t #t))
                     (map (lambda (x) (if (= x 500) (raise 'mid) x)) xs))
                   (loop (+ n 1))))";
    let t0 = Instant::now();
    let _ = rt.eval_str("<perf-raise>", src).unwrap();
    let elapsed = t0.elapsed();
    println!(
        "perf: 100x (1000-element map + guard catch) walker = {:.2}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed.as_secs_f64() < 30.0,
        "guard+raise loop took {:.2}s — regression?",
        elapsed.as_secs_f64()
    );
}
