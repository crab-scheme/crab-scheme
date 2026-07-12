//! Integration tests for the `scalar-replace-cons` optimizer pass
//! (#28). The pass is seeded as default-on in `Runtime::new`, so a
//! JIT-compiled function whose transient `cons` cells are read back
//! only by `car`/`cdr` and never escape runs without allocating those
//! pairs at all.
//!
//! Reach (see the pass module docs + the #28 PR): SRA fires on
//! **directly-consumed** conses — the cons result flows straight into
//! `car`/`cdr`/`pair?`/`null?` as an SSA temp. A pair bound with `let`
//! on the JIT tier is held in an env slot (the JIT path does not demote
//! env locals to SSA — that's #51b, and demoting under deopt is
//! unsound), so it is *not* eliminated; it still computes correctly.
//! These tests verify both: elimination where it applies, and
//! correctness everywhere (including across a deopt).

use cs_runtime::Runtime;

/// Directly-consumed transient conses: each `(cons a b)` result feeds a
/// single `car`/`cdr` with no intervening binding. `cc(a,b) = a + b`.
/// This is the scalar-replaceable shape — both conses are eliminated
/// when `cc` is JIT-compiled.
const CC_DEF: &str = "(define (cc a b) (+ (car (cons a b)) (cdr (cons a b))))";

/// Drive `cc` past the tier-up threshold (1024) so the next call runs
/// JIT-compiled, with SRA having eliminated its conses.
const CC_WARM: &str = "(let loop ((i 0)) (if (= i 2000) 'done (begin (cc 3 4) (loop (+ i 1)))))";

/// Self-recursive driver whose body builds two **directly-consumed**
/// transient conses per iteration — `(car (cons n 1))` and
/// `(cdr (cons n 1))`, equal to `n` and `1`. Self-recursion tiers the
/// function up on its own, so the conses live in the very function we
/// warm and measure (no separate driver-loop whose JIT timing would
/// confound the allocation count). `sumcc(n,0)` = sum_{i=1..n}(i+1).
const SUMCC_DEF: &str = "(define (sumcc n acc) \
     (if (= n 0) \
         acc \
         (sumcc (- n 1) (+ acc (car (cons n 1)) (cdr (cons n 1))))))";

/// A `let`-bound transient pair. Correct on every tier, but NOT
/// eliminated on the JIT tier (the binding lives in an env slot).
const SUMPAIRS: &str = "(define (sumpairs n acc) \
     (if (= n 0) \
         acc \
         (let ((p (cons n (* n 2)))) \
           (sumpairs (- n 1) (+ acc (car p) (cdr p))))))";

fn as_i64(v: &cs_core::Value) -> i64 {
    match v {
        nv @ (cs_core::Value::Fixnum(_)
        | cs_core::Value::Flonum(_)
        | cs_core::Value::BigNumber(_)
        | cs_core::Value::Rational(_)) => {
            let n = nv.as_number().unwrap();
            n.to_f64() as i64
        }
        other => panic!("expected a number, got {other:?}"),
    }
}

fn as_f64(v: &cs_core::Value) -> f64 {
    match v {
        nv @ (cs_core::Value::Fixnum(_)
        | cs_core::Value::Flonum(_)
        | cs_core::Value::BigNumber(_)
        | cs_core::Value::Rational(_)) => {
            let n = nv.as_number().unwrap();
            n.to_f64()
        }
        other => panic!("expected a number, got {other:?}"),
    }
}

#[test]
fn jit_eliminates_directly_consumed_cons_allocations() {
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<sra>", SUMCC_DEF).unwrap();
    // Warm: `sumcc` tiers itself up via self-recursion during this call.
    rt.eval_str_via_vm("<sra>", "(sumcc 100000 0)").unwrap();

    // Measure allocations over a fresh, now-JIT-compiled run. Each of
    // the `iters` iterations builds two transient conses — 2*iters
    // pairs' worth if not eliminated.
    let iters: i64 = 200_000;
    let before = cs_gc::alloc_telemetry::alloc_count_total();
    let result = rt
        .eval_str_via_vm("<sra>", &format!("(sumcc {iters} 0)"))
        .unwrap();
    let after = cs_gc::alloc_telemetry::alloc_count_total();
    let allocs = after - before;

    // sum over i=1..iters of (i + 1) = iters*(iters+1)/2 + iters.
    let expected = iters * (iters + 1) / 2 + iters;
    assert_eq!(as_i64(&result), expected, "result correctness");

    // Without SRA each iteration heap-allocates two conses, i.e.
    // 2*iters allocations. With SRA the per-iteration conses are gone;
    // the only residue is incidental runtime allocation (cycle-detector
    // / telemetry bookkeeping plus the brief JIT tier-up transient) —
    // observed at a few thousand and machine/profile-dependent, but
    // always well under one allocation per iteration. So `allocs <
    // iters` both proves the conses were eliminated (the un-optimized
    // run can't get below 2*iters) and tolerates the incidental noise.
    let ceiling = iters as u64;
    assert!(
        allocs < ceiling,
        "expected directly-consumed conses eliminated: {allocs} allocations across \
         {iters} iterations (ceiling {ceiling}). scalar-replace-cons did not fire."
    );
}

#[test]
fn sra_is_correct_across_a_deopt() {
    // Warm `cc` with fixnums so it JIT-compiles and specializes to a
    // fixnum fast path (the conses are SRA-eliminated). Then call it
    // with flonum args: the fixnum guards fail and the call deopts back
    // to the VM mid-function. Because SRA *removed* the pair values
    // (rather than merely promoting them), this exercises whether deopt
    // reconstruction is still correct without those values.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<sra>", CC_DEF).unwrap();
    rt.eval_str_via_vm("<sra>", CC_WARM).unwrap();

    // Fixnum path (JIT, SRA active): cc(30, 12) = 42.
    let fixnum = rt.eval_str_via_vm("<sra>", "(cc 30 12)").unwrap();
    assert_eq!(as_i64(&fixnum), 42, "fixnum fast path");

    // Flonum path: forces a guard failure → deopt → VM resumes.
    // cc(1.5, 2.5) = 4.0.
    let flonum = rt.eval_str_via_vm("<sra>", "(cc 1.5 2.5)").unwrap();
    assert!(
        (as_f64(&flonum) - 4.0).abs() < 1e-9,
        "deopt path produced {}, expected 4.0",
        as_f64(&flonum)
    );
}

#[test]
fn sra_preserves_semantics_across_tiers() {
    // The let-bound shape: not eliminated on JIT, but must compute the
    // same result on the walker, the VM (no JIT), and the VM+JIT tiers.
    let prog = "(sumpairs 5000 0)";
    // sum over i=1..5000 of (i + 2i) = 3 * 5000*5001/2.
    let want = 3 * (5000 * 5001 / 2);

    let mut w = Runtime::new();
    w.eval_str("<sra>", SUMPAIRS).unwrap();
    assert_eq!(as_i64(&w.eval_str("<sra>", prog).unwrap()), want, "walker");

    let mut v = Runtime::new();
    v.eval_str_via_vm("<sra>", SUMPAIRS).unwrap();
    assert_eq!(
        as_i64(&v.eval_str_via_vm("<sra>", prog).unwrap()),
        want,
        "vm-no-jit"
    );

    let mut j = Runtime::new();
    j.install_jit().unwrap();
    j.eval_str_via_vm("<sra>", SUMPAIRS).unwrap();
    j.eval_str_via_vm("<sra>", "(sumpairs 100000 0)").unwrap(); // warm
    assert_eq!(
        as_i64(&j.eval_str_via_vm("<sra>", prog).unwrap()),
        want,
        "vm-jit"
    );
}
