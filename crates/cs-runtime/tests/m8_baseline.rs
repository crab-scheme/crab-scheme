//! M8 iter 1 — baseline measurements for first-class continuations.
//!
//! Documents what call/cc currently handles (escape-only patterns
//! per the foundation milestone) and what it doesn't (re-invocation
//! after the call/cc returns; multiple invocations; coroutine
//! patterns). Subsequent M8 iters flip the latter group from #[ignore]
//! to passing.

use cs_core::{Number, Value};
use cs_runtime::Runtime;

fn run(prog: &str) -> Result<Value, String> {
    let mut rt = Runtime::new();
    rt.eval_str("<m8>", prog).map_err(|d| d.message)
}

// ---- Diagnostic-shape baseline (M8 iter 2) ----

#[test]
fn after_extent_invocation_emits_clear_diagnostic() {
    // Until the M8 driver-loop refactor lands, invoking a saved
    // continuation outside its dynamic extent must produce a
    // diagnostic that names the limitation rather than the generic
    // "uncaught escape" message users were previously seeing.
    let mut rt = Runtime::new();
    let prog = "(define saved #f) \
                (call/cc (lambda (k) (set! saved k) 10)) \
                (saved 100)";
    let err = rt.eval_str("<m8>", prog).expect_err("should error");
    let msg = err.message;
    assert!(
        msg.contains("outside its dynamic extent") && msg.contains("M8"),
        "diagnostic should name the M8 limitation, got: {msg}"
    );
}

#[test]
fn after_extent_invocation_emits_clear_diagnostic_vm() {
    let mut rt = Runtime::new();
    let prog = "(define saved #f) \
                (call/cc (lambda (k) (set! saved k) 10)) \
                (saved 100)";
    let err = rt.eval_str_via_vm("<m8>", prog).expect_err("should error");
    let msg = err.message;
    assert!(
        msg.contains("outside its dynamic extent") && msg.contains("M8"),
        "VM diagnostic should name the M8 limitation, got: {msg}"
    );
}

// ---- Already passes (escape-only baseline) ----

#[test]
fn baseline_escape_returns_directly() {
    let r = run("(call/cc (lambda (k) 42))").unwrap();
    match r {
        Value::Number(Number::Fixnum(42)) => {}
        other => panic!("expected 42, got {:?}", other),
    }
}

#[test]
fn baseline_escape_invoked_inside_extent() {
    let r = run("(+ 1 (call/cc (lambda (k) (k 10))))").unwrap();
    match r {
        Value::Number(Number::Fixnum(11)) => {}
        other => panic!("expected 11, got {:?}", other),
    }
}

#[test]
fn baseline_escape_bypasses_outer_arithmetic() {
    let r = run("(call/cc (lambda (k) (+ 1 (k 99))))").unwrap();
    match r {
        Value::Number(Number::Fixnum(99)) => {}
        other => panic!("expected 99, got {:?}", other),
    }
}

// ---- Below: ignored until M8 lands first-class semantics ----

#[test]
#[ignore = "M8: re-invocation after call/cc returns is not yet supported"]
fn m8_reinvocation_after_extent() {
    // Save the continuation, escape with one value, then invoke
    // the saved continuation with a different value. Should re-run
    // the surrounding context.
    let r = run("(define saved #f) \
         (define result1 (+ 1 (call/cc (lambda (k) (set! saved k) 10)))) \
         (if (< result1 50) (saved 100) 'done)")
    .unwrap();
    // After re-invocation, result1 = 1 + 100 = 101, then the if
    // branch is 'done because 101 >= 50.
    match r {
        Value::Symbol(_) => {}
        other => panic!("expected 'done after reinvocation, got {:?}", other),
    }
}

#[test]
#[ignore = "M8: multi-invocation continuations not yet supported"]
fn m8_multiple_invocations() {
    // The classic counter-via-call/cc pattern: each invocation of
    // the saved continuation re-runs the body and bumps the count.
    let r = run("(define count 0) \
         (define saved #f) \
         (call/cc (lambda (k) (set! saved k) #f)) \
         (set! count (+ count 1)) \
         (if (< count 3) (saved #f) count)")
    .unwrap();
    match r {
        Value::Number(Number::Fixnum(3)) => {}
        other => panic!("expected 3, got {:?}", other),
    }
}

#[test]
#[ignore = "M8: dynamic-wind shared-prefix not yet implemented"]
fn m8_dynamic_wind_through_continuation() {
    // dynamic-wind's after thunk should run when a continuation
    // invocation crosses the wind boundary outward.
    let r = run("(define log '()) \
         (define (push x) (set! log (cons x log))) \
         (define saved #f) \
         (define (body) \
           (call/cc (lambda (k) (set! saved k))) \
           (push 'body)) \
         (dynamic-wind \
           (lambda () (push 'before)) \
           body \
           (lambda () (push 'after))) \
         (if (memv 'after log) (reverse log) (saved #f))")
    .unwrap();
    // Expected log on first run: (before body after).
    // After re-invocation of saved: (before body after before body
    // after) — wind reruns its before+after for re-entry.
    match r {
        Value::Pair(_) => {}
        other => panic!("expected list, got {:?}", other),
    }
}
