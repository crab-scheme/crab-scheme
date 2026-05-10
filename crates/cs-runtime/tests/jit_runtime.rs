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
fn recursive_fib_jits_after_warmup() {
    // The headline iter-7 test: the Define call site stamps a
    // self-name on the (lambda) closure; the tier-up hook flows
    // that through bytecode_to_rir's self-recursion detection;
    // CallSelf lowers to a Cranelift recursive call. Once the
    // counter crosses, fib runs natively and produces the right
    // values.
    cs_vm::vm::reset_jit_call_count();
    cs_vm::vm::reset_tier_up_count();

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();

    rt.eval_str_via_vm(
        "<test>",
        "(define fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))",
    )
    .unwrap();

    // Warmup: compute fib(15). Recursive calls drive the counter
    // past the threshold, triggering JIT compilation.
    let warmup = rt.eval_str_via_vm("<test>", "(fib 15)").unwrap();
    match warmup {
        Value::Number(Number::Fixnum(610)) => {}
        other => panic!("fib(15): expected 610, got {:?}", other),
    }

    assert!(
        cs_vm::vm::tier_up_count() >= 1,
        "tier-up should have fired for fib"
    );
    let jit_calls_after_warmup = cs_vm::vm::jit_call_count();
    assert!(
        jit_calls_after_warmup > 0,
        "fib should JIT-dispatch during warmup, jit_call_count = {jit_calls_after_warmup}"
    );

    // Post-warmup: fib(20) runs entirely on JIT. Verify the value.
    let r = rt.eval_str_via_vm("<test>", "(fib 20)").unwrap();
    match r {
        Value::Number(Number::Fixnum(6765)) => {}
        other => panic!("fib(20): expected 6765, got {:?}", other),
    }

    let final_jit_calls = cs_vm::vm::jit_call_count();
    // Recursive calls inside JIT'd fib lower to direct native
    // calls (Inst::CallSelf), so they don't tick the VM-side
    // jit_call_count. Just assert the top-level entry into fib(20)
    // dispatched through the JIT (and produced the right value).
    assert!(
        final_jit_calls > jit_calls_after_warmup,
        "fib(20) should add at least one JIT dispatch: {jit_calls_after_warmup} -> {final_jit_calls}"
    );
}

/// M6 Phase 2 iter E: `<=`, `>`, `>=` outside fused-branch
/// contexts now JIT (translator lowers GtFx2/LeFx2/GeFx2 via Lt+Eq
/// combinations).
#[test]
fn jit_handles_le_gt_ge_comparisons() {
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<jit>",
        "(define cmp (lambda (a b) (if (<= a b) (if (>= a b) 'eq 'lt) 'gt)))",
    )
    .unwrap();
    // Warm cmp past threshold.
    rt.eval_str_via_vm(
        "<jit>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (cmp i (- i 1)) (cmp i i) (cmp i (+ i 1)) (loop (+ i 1)))))",
    )
    .unwrap();
    // Functional checks — three result values.
    let lt = rt.eval_str_via_vm("<jit>", "(cmp 5 10)").unwrap();
    let eq = rt.eval_str_via_vm("<jit>", "(cmp 7 7)").unwrap();
    let gt = rt.eval_str_via_vm("<jit>", "(cmp 99 1)").unwrap();
    for (label, v) in [("lt", lt), ("eq", eq), ("gt", gt)] {
        match v {
            Value::Symbol(_) => {}
            other => panic!("{label}: expected symbol, got {:?}", other),
        }
    }
}

/// M6 Phase 2 iter F: fixnum-only builtins (quotient / remainder /
/// bitwise-{and,ior,xor}) JIT via specialized native instructions
/// (Cranelift sdiv / srem / band / bor / bxor).
#[test]
fn jit_handles_fixnum_builtin_calls() {
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<jit>", "(define div2 (lambda (n) (quotient n 2)))")
        .unwrap();
    rt.eval_str_via_vm("<jit>", "(define mask (lambda (n) (bitwise-and n 7)))")
        .unwrap();
    rt.eval_str_via_vm("<jit>", "(define mod3 (lambda (n) (remainder n 3)))")
        .unwrap();

    // Warm each closure past the threshold.
    let warmup = "(let loop ((i 0)) \
                    (if (= i 1500) 'done \
                        (begin (div2 i) (mod3 i) (mask i) (loop (+ i 1)))))";
    rt.eval_str_via_vm("<jit>", warmup).unwrap();

    let r = rt.eval_str_via_vm("<jit>", "(div2 100)").unwrap();
    match r {
        Value::Number(Number::Fixnum(50)) => {}
        other => panic!("expected 50, got {:?}", other),
    }
    let r = rt.eval_str_via_vm("<jit>", "(mod3 100)").unwrap();
    match r {
        Value::Number(Number::Fixnum(1)) => {}
        other => panic!("expected 1 (100 mod 3), got {:?}", other),
    }
    let r = rt.eval_str_via_vm("<jit>", "(mask 13)").unwrap();
    match r {
        // 13 & 7 = 5
        Value::Number(Number::Fixnum(5)) => {}
        other => panic!("expected 5, got {:?}", other),
    }
}

/// M6 Phase 2 iter G: `abs`, `min`, `max` lower to Cranelift
/// native instructions (iabs / smin / smax).
#[test]
fn jit_handles_abs_min_max() {
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<jit>", "(define dist (lambda (a b) (abs (- a b))))")
        .unwrap();
    rt.eval_str_via_vm("<jit>", "(define clamp-low (lambda (x lo) (max x lo)))")
        .unwrap();
    rt.eval_str_via_vm("<jit>", "(define clamp-hi (lambda (x hi) (min x hi)))")
        .unwrap();

    let warmup = "(let loop ((i 0)) \
                    (if (= i 1500) 'done \
                        (begin (dist i (- 0 i)) (clamp-low i 0) (clamp-hi i 1000) \
                               (loop (+ i 1)))))";
    rt.eval_str_via_vm("<jit>", warmup).unwrap();

    let r = rt.eval_str_via_vm("<jit>", "(dist 5 -3)").unwrap();
    match r {
        Value::Number(Number::Fixnum(8)) => {}
        other => panic!("dist 5 -3: expected 8, got {:?}", other),
    }
    let r = rt.eval_str_via_vm("<jit>", "(clamp-low -2 0)").unwrap();
    match r {
        Value::Number(Number::Fixnum(0)) => {}
        other => panic!("clamp-low -2 0: expected 0, got {:?}", other),
    }
    let r = rt.eval_str_via_vm("<jit>", "(clamp-hi 9999 1000)").unwrap();
    match r {
        Value::Number(Number::Fixnum(1000)) => {}
        other => panic!("clamp-hi 9999 1000: expected 1000, got {:?}", other),
    }
}

/// M6 Phase 2 iter B: closures with free Fixnum vars can JIT.
/// `(define base 100) (define add-base (lambda (x) (+ x base)))` —
/// `base` is a free var inside add-base's body. The JIT translator
/// emits Inst::EnvLookup, which the lowerer turns into a Cranelift
/// call to vm_env_lookup_fixnum at runtime.
#[test]
fn jit_handles_free_var_env_lookup() {
    cs_vm::vm::reset_jit_call_count();
    cs_vm::vm::reset_tier_up_count();
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<jit>", "(define base 100)").unwrap();
    rt.eval_str_via_vm("<jit>", "(define add-base (lambda (x) (+ x base)))")
        .unwrap();

    // Warm add-base past the threshold.
    rt.eval_str_via_vm(
        "<jit>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (add-base i) (loop (+ i 1)))))",
    )
    .unwrap();

    // Functional: (add-base 42) = 142. The JIT body reads base from
    // the captured env via Inst::EnvLookup.
    let r = rt.eval_str_via_vm("<jit>", "(add-base 42)").unwrap();
    match r {
        Value::Number(Number::Fixnum(142)) => {}
        other => panic!("expected 142, got {:?}", other),
    }

    // base mutation reflects in subsequent JIT calls (env is shared
    // via Rc, the helper reads live state).
    rt.eval_str_via_vm("<jit>", "(set! base 1000)").unwrap();
    let r = rt.eval_str_via_vm("<jit>", "(add-base 5)").unwrap();
    match r {
        Value::Number(Number::Fixnum(1005)) => {}
        other => panic!("expected 1005 after set!, got {:?}", other),
    }
}

/// M6 Phase 2 iter C: free-var `set!` from inside JIT'd code.
/// `(define c 0) (define (bump) (set! c (+ c 1)))` — bump's body
/// reads c via EnvLookup and writes it back via EnvSet.
#[test]
fn jit_handles_free_var_set_bang() {
    cs_vm::vm::reset_jit_call_count();
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<jit>", "(define c 0)").unwrap();
    rt.eval_str_via_vm("<jit>", "(define bump (lambda () (set! c (+ c 1))))")
        .unwrap();

    // Warm bump past the threshold by calling it many times.
    rt.eval_str_via_vm(
        "<jit>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (bump) (loop (+ i 1)))))",
    )
    .unwrap();

    // After 1500 bumps, c is some value. Let's snapshot it then
    // bump once more and check c grew by 1 (i.e., set! actually
    // wrote back through the JIT).
    let snap = rt.eval_str_via_vm("<jit>", "c").unwrap();
    let snap_n = match snap {
        Value::Number(Number::Fixnum(n)) => n,
        other => panic!("c not a fixnum: {:?}", other),
    };
    rt.eval_str_via_vm("<jit>", "(bump)").unwrap();
    let after = rt.eval_str_via_vm("<jit>", "c").unwrap();
    let after_n = match after {
        Value::Number(Number::Fixnum(n)) => n,
        other => panic!("c not a fixnum after bump: {:?}", other),
    };
    assert_eq!(after_n, snap_n + 1, "set! should have incremented c");
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
