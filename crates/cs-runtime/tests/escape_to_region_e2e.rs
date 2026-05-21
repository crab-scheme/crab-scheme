//! #51 escape-to-region — end-to-end integration through the runtime
//! JIT pipeline.
//!
//! The cs-opt unit tests prove the analysis promotes the right
//! `Cons`es; the cs-jit-cranelift `jit_region` tests prove the
//! `ConsRegion` lowering runs. These tests close the loop: install the
//! pass via its Scheme builtin, warm a cons-heavy procedure past the
//! JIT tier-up threshold (so the pass runs during tier-up and its
//! `ConsRegion`s reach native code), then check results — both outside
//! a region (heap fallback) and inside `(with-region …)` (real bump
//! allocation). A wrong car/cdr or a region use-after-free would
//! corrupt the result.
//!
//! ## What the pass promotes (and why the test uses this shape)
//!
//! The analysis is conservative: a `Cons` is promoted only if it never
//! escapes — in particular, never stored into the local environment via
//! `EnvDefineLocal`. The bytecode→RIR translator materializes a `let`
//! binding into `EnvDefineLocal`, so a `(let ((p (cons …))) …)` temp is
//! treated as escaping and stays on the heap. That is exactly what
//! keeps the pass safe: a pair stored in a (retained) env frame can
//! outlive the region (see the `#[ignore]`d known-bug repro at the
//! bottom). The promotable shape is a cons that is consumed directly —
//! e.g. `(car (cons a b))` — and never bound. So the fixture below uses
//! the directly-consumed form, which the pass does promote.
//!
//! Gated on `regions`: `with-region` and the `ConsRegion` JIT lowering
//! are `regions`-only. cs-runtime enables `regions` by default.
#![cfg(feature = "regions")]

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// `(cc a b)` = `a + b`, computed via two directly-consumed temp pairs
/// — `(car (cons a b))` and `(cdr (cons a b))`. Each cons is used once
/// (by car / by cdr) and never bound, so `escape-to-region` promotes
/// both to `ConsRegion`. Result correctness depends on car/cdr reading
/// the right slots of the (region-allocated) pairs.
const CC_DEF: &str = "(define (cc a b) (+ (car (cons a b)) (cdr (cons a b))))";

/// Warm `cc` past the default tier-up threshold (1024) so the next call
/// runs JIT-compiled (with the pass having promoted its conses).
const CC_WARM: &str = "(let loop ((i 0)) (if (= i 1500) 'done (begin (cc 4 0) (loop (+ i 1)))))";

#[test]
fn escape_to_region_is_installable() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    let v = rt
        .eval_str(
            "<t>",
            "(install-optimizer-pass! 'escape-to-region)
             (installed-optimizer-passes)",
        )
        .unwrap();
    assert_eq!(disp(&rt, &v), "(escape-to-region)");
    cs_opt::clear_active_passes();
}

#[test]
fn escape_to_region_preserves_semantics_through_jit() {
    // Baseline: JIT on, pass OFF, no region. cc(30,12) = 42.
    let baseline = {
        cs_opt::clear_active_passes();
        let mut rt = Runtime::new();
        rt.install_jit().unwrap();
        rt.eval_str_via_vm("<t>", CC_DEF).unwrap();
        rt.eval_str_via_vm("<t>", CC_WARM).unwrap();
        let v = rt.eval_str_via_vm("<t>", "(cc 30 12)").unwrap();
        disp(&rt, &v)
    };
    assert_eq!(baseline, "42");

    // Pass ON, JIT on. Both conses are promoted to ConsRegion when cc
    // tiers up. Outside a region they fall back to the heap; inside
    // `(with-region …)` they bump-allocate. All must equal the baseline
    // — promotion + region allocation are semantics-preserving.
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<t>", "(install-optimizer-pass! 'escape-to-region)")
        .unwrap();
    rt.eval_str_via_vm("<t>", CC_DEF).unwrap();
    rt.eval_str_via_vm("<t>", CC_WARM).unwrap();

    let out_of_region = rt.eval_str_via_vm("<t>", "(cc 30 12)").unwrap();
    assert_eq!(disp(&rt, &out_of_region), baseline, "pass-on, no region");

    let in_region = rt
        .eval_str_via_vm("<t>", "(with-region (lambda () (cc 30 12)))")
        .unwrap();
    assert_eq!(
        disp(&rt, &in_region),
        baseline,
        "pass-on, inside with-region (region-allocated, consumed locally)"
    );

    // Bigger operands inside a region — more region bytes, still exact.
    // cc(1000, 234) = 1234.
    let bigger = rt
        .eval_str_via_vm("<t>", "(with-region (lambda () (cc 1000 234)))")
        .unwrap();
    assert_eq!(disp(&rt, &bigger), "1234");
    cs_opt::clear_active_passes();
}

#[test]
fn cons_in_region_correct_on_walker_and_vm() {
    // The explicit `cons-in-region` builtin (which lowers to ConsRegion
    // for JIT/AOT) is correct on the non-JIT tiers. car/cdr read the
    // region-allocated pair's slots; `with-region` deep-promotes the
    // returned fixnum. (45 . 12) -> 45 + 12 = 57.
    let prog = "(with-region (lambda () \
                  (+ (car (cons-in-region 45 12)) (cdr (cons-in-region 45 12)))))";

    let mut rt = Runtime::new();
    let walker = rt.eval_str("<t>", prog).unwrap();
    assert_eq!(disp(&rt, &walker), "57", "walker");

    let mut rt2 = Runtime::new();
    rt2.install_jit().unwrap();
    // No warm-up: a single call stays on the bytecode VM.
    let vm = rt2.eval_str_via_vm("<t>", prog).unwrap();
    assert_eq!(disp(&rt2, &vm), "57", "vm");
}

/// Regression test for the region/env-lifetime UAF fixed in #51a.
///
/// Before the fix: a `cons-in-region` pair *bound by `let`* is lowered
/// to `EnvDefineLocal`. For a JIT body without a frame env (no nested
/// closure), `EnvDefineLocal` wrote into the closure's *definition*
/// (global) env, so the region-allocated pair outlived its
/// `with-region` arena; once the arena was bulk-freed, the dangling
/// handle panicked `slot region_id was 0` at teardown
/// (`Bindings::drop` → `Gc::from_raw_jit_region`).
///
/// The fix (`Function::has_env_define_local` → `set_jit_needs_frame_env`)
/// gives any `EnvDefineLocal` body a per-call frame env, so the temp is
/// scoped to the invocation and dropped (region still live) on return.
#[test]
fn cons_in_region_jit_with_region_no_env_leak() {
    cs_opt::clear_active_passes();
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<t>",
        "(define (g a b) (let ((p (cons-in-region a b))) (+ (car p) (cdr p))))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<t>",
        "(with-region (lambda () \
           (let loop ((i 0)) (if (= i 1500) 'done (begin (g 4 4) (loop (+ i 1)))))))",
    )
    .unwrap();
    let r = rt
        .eval_str_via_vm("<t>", "(with-region (lambda () (g 50 50)))")
        .unwrap();
    assert_eq!(disp(&rt, &r), "100");
}
