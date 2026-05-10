//! M6 iter 3 — tier-up state machine wired into cs-vm closure dispatch.
//!
//! Each [`cs_vm::vm::VmClosure`] carries a [`cs_jit::Tier`] counter
//! that bumps on every call. When the counter crosses the threshold,
//! the optional `VmTierUpHook` (installed via
//! [`cs_vm::vm::install_tier_up_hook`]) fires. These tests exercise
//! the wiring end-to-end via real Scheme programs.

use std::sync::atomic::{AtomicU64, Ordering};

use cs_runtime::Runtime;

#[test]
fn closure_threshold_cross_fires_tier_up_event() {
    cs_vm::vm::reset_tier_up_count();
    let mut rt = Runtime::new();
    // The default threshold is 1024 (cs_jit::DEFAULT_TIER_THRESHOLD).
    // We define a closure and call it just enough times to ensure a
    // tier-up. Loop iters > threshold guarantees the cross.
    rt.eval_str_via_vm("<test>", "(define f (lambda (x) (+ x 1)))")
        .unwrap();
    let prog = "(let loop ((i 0)) \
                  (if (= i 2048) 'done (begin (f i) (loop (+ i 1)))))";
    rt.eval_str_via_vm("<test>", prog).unwrap();
    // Two closures cross: `f` and the named-let `loop`. Both reach
    // the 1024th call inside the program.
    assert!(
        cs_vm::vm::tier_up_count() >= 2,
        "tier_up_count = {}",
        cs_vm::vm::tier_up_count()
    );
}

#[test]
fn cold_closure_below_threshold_does_not_fire_event() {
    cs_vm::vm::reset_tier_up_count();
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<test>", "(define cold (lambda () 1))")
        .unwrap();
    // Far below the threshold.
    rt.eval_str_via_vm("<test>", "(cold) (cold) (cold)")
        .unwrap();
    assert_eq!(cs_vm::vm::tier_up_count(), 0);
}

#[test]
fn install_tier_up_hook_fires_on_threshold_cross() {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    fn hook(_closure: &cs_vm::vm::VmClosure, _args: &[cs_core::Value]) {
        COUNT.fetch_add(1, Ordering::SeqCst);
    }
    COUNT.store(0, Ordering::SeqCst);
    cs_vm::vm::reset_tier_up_count();
    let prev = cs_vm::vm::install_tier_up_hook(Some(hook));

    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<test>", "(define f (lambda (x) x))")
        .unwrap();
    let prog = "(let loop ((i 0)) \
                  (if (= i 1500) 'done (begin (f i) (loop (+ i 1)))))";
    rt.eval_str_via_vm("<test>", prog).unwrap();

    cs_vm::vm::install_tier_up_hook(prev);
    let count = COUNT.load(Ordering::SeqCst);
    assert!(count >= 2, "user hook fired {count} times");
    assert_eq!(count, cs_vm::vm::tier_up_count());
}

#[test]
fn record_deopt_increments_thread_local_counter() {
    cs_vm::vm::reset_deopt_count();
    let tier = cs_jit::Tier::with_threshold(8);
    assert_eq!(cs_vm::vm::deopt_count(), 0);

    let blacklisted_after_first = cs_vm::vm::record_deopt(&tier);
    assert!(!blacklisted_after_first);
    assert_eq!(cs_vm::vm::deopt_count(), 1);

    // Three deopts is the budget; after the third the tier is
    // blacklisted and bump() never reports a threshold cross again.
    cs_vm::vm::record_deopt(&tier);
    let blacklisted = cs_vm::vm::record_deopt(&tier);
    assert!(blacklisted);
    assert_eq!(cs_vm::vm::deopt_count(), 3);
    assert!(tier.is_blacklisted());

    // Even after 1024+ bumps, a blacklisted tier never crosses.
    for _ in 0..2048 {
        assert!(!tier.bump());
    }
}
