//! M6 iter 6 — Runtime JIT integration end-to-end.
//!
//! Hot VM closures, after their tier counter crosses, get JIT-
//! compiled and dispatched through native code on subsequent calls.
//! The bytecode body remains as the deopt fallback.

use cs_core::{Number, Value};
use cs_runtime::Runtime;

#[test]
fn install_jit_succeeds_and_marks_runtime() {
    let mut rt = Runtime::new();
    assert!(!rt.jit_installed());
    rt.install_jit().expect("install_jit");
    assert!(rt.jit_installed());
    // Idempotent.
    rt.install_jit().unwrap();
    assert!(rt.jit_installed());
}

#[test]
fn hot_arithmetic_closure_dispatches_through_jit() {
    cs_vm::vm::reset_jit_call_count();
    cs_vm::vm::reset_tier_up_count();

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();

    // Warm the arith closure past the threshold.
    rt.eval_str_via_vm("<test>", "(define addone (lambda (x) (+ x 1)))")
        .unwrap();
    let prog = "(let loop ((i 0)) \
                  (if (= i 1500) 'done (begin (addone i) (loop (+ i 1)))))";
    rt.eval_str_via_vm("<test>", prog).unwrap();

    // The tier-up hook should have JITted addone (and the named-let
    // loop), so jit_call_count > 0 by the time the loop finishes.
    assert!(
        cs_vm::vm::tier_up_count() >= 1,
        "tier_up_count = {}",
        cs_vm::vm::tier_up_count()
    );
    let jit_calls_during_warmup = cs_vm::vm::jit_call_count();
    assert!(
        jit_calls_during_warmup > 0,
        "expected JIT dispatch during warmup, jit_call_count = {jit_calls_during_warmup}"
    );

    // Functional correctness: the JITted addone returns the right
    // value when called fresh.
    let r = rt.eval_str_via_vm("<test>", "(addone 41)").unwrap();
    match r {
        Value::Number(Number::Fixnum(42)) => {}
        other => panic!("expected 42, got {:?}", other),
    }

    // Make a few extra calls and verify jit_call_count grew.
    let before = cs_vm::vm::jit_call_count();
    for _ in 0..10 {
        rt.eval_str_via_vm("<test>", "(addone 100)").unwrap();
    }
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > before,
        "jit_call_count should grow on subsequent calls: before={before} after={after}"
    );
}

#[test]
fn cold_closure_runs_correctly_without_jit() {
    // Without install_jit, no JIT dispatch happens.
    cs_vm::vm::reset_jit_call_count();
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<test>", "(define f (lambda (x) (* x 2)))")
        .unwrap();
    let r = rt.eval_str_via_vm("<test>", "(f 21)").unwrap();
    match r {
        Value::Number(Number::Fixnum(42)) => {}
        other => panic!("expected 42, got {:?}", other),
    }
    assert_eq!(cs_vm::vm::jit_call_count(), 0);
}

#[test]
fn unsupported_closure_stays_on_vm_silently() {
    // A closure with non-fixnum / env-access body translates fail
    // and should silently stay on the VM.
    cs_vm::vm::reset_jit_call_count();
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();

    // String concat — translator rejects (non-fixnum primop in body).
    rt.eval_str_via_vm("<test>", "(define g (lambda (s) (string-append s \"!\")))")
        .unwrap();
    let prog = "(let loop ((i 0)) \
                  (if (= i 1500) 'done (begin (g \"hi\") (loop (+ i 1)))))";
    rt.eval_str_via_vm("<test>", prog).unwrap();

    // Tier-up fired (the loop ticked > threshold) but g never
    // JIT-dispatched because the translator rejected it. The loop
    // closure itself probably does JIT (pure-fixnum). So
    // jit_call_count may or may not be 0; what matters is g still
    // works.
    let r = rt.eval_str_via_vm("<test>", "(g \"hello\")").unwrap();
    match r {
        Value::String(s) => assert_eq!(*s.borrow(), "hello!"),
        other => panic!("expected string, got {:?}", other),
    }
}
