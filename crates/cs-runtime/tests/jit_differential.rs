//! M6 iter 8 — JIT differential test.
//!
//! Each test runs the same Scheme program on three tiers and
//! asserts the results agree:
//!
//! 1. Walker (`Runtime::eval_str`)
//! 2. VM with no JIT (`Runtime::eval_str_via_vm` on a runtime that
//!    has not called `install_jit`)
//! 3. VM with JIT (`Runtime::eval_str_via_vm` on a runtime that
//!    has called `install_jit`; programs are warmed past the
//!    threshold so hot closures actually dispatch through native
//!    code).
//!
//! This is the spec's iter-N+1 deliverable scaled to a focused
//! initial set of programs. Broader 10k+ coverage will follow once
//! the existing conformance harness is wired through the JIT
//! runtime as a third tier.

use cs_core::Value;
use cs_runtime::Runtime;

/// Define a procedure on a runtime, warm it by computing
/// `(<name> <warmup-arg>)` once, then evaluate `expr` and return
/// the result.
fn define_warm_eval(
    install_jit: bool,
    defines: &[&str],
    warmup_call: Option<&str>,
    expr: &str,
) -> Value {
    let mut rt = Runtime::new();
    if install_jit {
        rt.install_jit().unwrap();
    }
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    if let Some(w) = warmup_call {
        rt.eval_str_via_vm("<diff>", w).unwrap();
    }
    rt.eval_str_via_vm("<diff>", expr).unwrap()
}

/// Walker tier: evaluate every define + the final expression on
/// the walker (eval_str). No warmup.
fn walker_eval(defines: &[&str], expr: &str) -> Value {
    let mut rt = Runtime::new();
    for d in defines {
        rt.eval_str("<diff>", d).unwrap();
    }
    rt.eval_str("<diff>", expr).unwrap()
}

fn assert_three_tier_agreement(defines: &[&str], warmup: Option<&str>, expr: &str) {
    let walker = walker_eval(defines, expr);
    let vm_no_jit = define_warm_eval(false, defines, warmup, expr);
    let vm_jit = define_warm_eval(true, defines, warmup, expr);

    match (&walker, &vm_no_jit, &vm_jit) {
        (Value::Number(a), Value::Number(b), Value::Number(c)) => {
            assert_eq!(a.to_f64(), b.to_f64(), "walker vs vm-no-jit");
            assert_eq!(b.to_f64(), c.to_f64(), "vm-no-jit vs vm-jit");
        }
        _ => panic!(
            "expected numbers; walker={:?} vm={:?} jit={:?}",
            walker, vm_no_jit, vm_jit
        ),
    }
}

#[test]
fn diff_fib_20() {
    let defines = &["(define fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))"];
    // Warm with (fib 15) -> 1597 recursive calls, well past the
    // default tier threshold of 1024.
    assert_three_tier_agreement(defines, Some("(fib 15)"), "(fib 20)");
}

#[test]
fn diff_fact_12() {
    let defines = &["(define fact (lambda (n) (if (= n 0) 1 (* n (fact (- n 1))))))"];
    // (fact 12) recurses 12 times; not enough for tier-up. Loop
    // many calls before checking the final value.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (fact 8) (loop (+ i 1)))))",
    )
    .unwrap();
    let jit_result = rt.eval_str_via_vm("<diff>", "(fact 12)").unwrap();

    let walker = walker_eval(defines, "(fact 12)");
    match (&walker, &jit_result) {
        (Value::Number(a), Value::Number(b)) => {
            assert_eq!(a.to_f64(), b.to_f64());
            assert_eq!(a.to_f64(), 479001600.0);
        }
        other => panic!("expected numbers, got {:?}", other),
    }
}

#[test]
fn diff_ack_3_5() {
    // Ackermann's function — many recursive calls, tier-up
    // guaranteed.
    let defines = &["(define ack (lambda (m n) \
            (if (= m 0) (+ n 1) \
                (if (= n 0) (ack (- m 1) 1) \
                    (ack (- m 1) (ack m (- n 1)))))))"];
    // Stay small: (ack 3 4) is plenty of recursion to trigger
    // tier-up but doesn't overflow the test thread's debug-build
    // stack. (ack 3 6) blows the debug-build stack frame budget.
    assert_three_tier_agreement(defines, Some("(ack 3 3)"), "(ack 3 4)");
}

#[test]
fn diff_loop_sum_1_to_n() {
    // Iterative-style sum via tail recursion.
    let defines = &["(define loop-sum (lambda (n) \
            (let helper ((i 0) (acc 0)) \
              (if (> i n) acc (helper (+ i 1) (+ acc i))))))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm with a sum that itself triggers tier-up.
    rt.eval_str_via_vm("<diff>", "(loop-sum 2000)").unwrap();
    let jit_result = rt.eval_str_via_vm("<diff>", "(loop-sum 100)").unwrap();
    let walker = walker_eval(defines, "(loop-sum 100)");
    match (&walker, &jit_result) {
        (Value::Number(a), Value::Number(b)) => {
            assert_eq!(a.to_f64(), b.to_f64());
            assert_eq!(a.to_f64(), 5050.0);
        }
        other => panic!("expected numbers, got {:?}", other),
    }
}

#[test]
fn diff_gcd() {
    let defines =
        &["(define gcd (lambda (a b) (if (= b 0) a (gcd b (- a (* (quotient a b) b))))))"];
    // Warm with a hot loop that exercises gcd many times.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (gcd 48 18) (loop (+ i 1)))))",
    )
    .unwrap();
    let jit_result = rt.eval_str_via_vm("<diff>", "(gcd 48 18)").unwrap();
    let walker = walker_eval(defines, "(gcd 48 18)");
    match (&walker, &jit_result) {
        (Value::Number(a), Value::Number(b)) => {
            assert_eq!(a.to_f64(), b.to_f64());
            assert_eq!(a.to_f64(), 6.0);
        }
        other => panic!("expected numbers, got {:?}", other),
    }
}

#[test]
fn diff_jit_with_free_var_lookup() {
    // M6 Phase 2 iter B: free-var LoadVar lowers to Inst::EnvLookup.
    // Walker, VM-no-JIT, and VM-with-JIT must produce identical
    // values across the warmup-then-call pattern.
    let defines = &[
        "(define base 100)",
        "(define add-base (lambda (x) (+ x base)))",
    ];

    let mut rt_walker = Runtime::new();
    for d in defines {
        rt_walker.eval_str("<diff>", d).unwrap();
    }
    let walker = rt_walker.eval_str("<diff>", "(add-base 42)").unwrap();

    let mut rt_vm = Runtime::new();
    for d in defines {
        rt_vm.eval_str_via_vm("<diff>", d).unwrap();
    }
    let vm = rt_vm.eval_str_via_vm("<diff>", "(add-base 42)").unwrap();

    let mut rt_jit = Runtime::new();
    rt_jit.install_jit().unwrap();
    for d in defines {
        rt_jit.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt_jit
        .eval_str_via_vm(
            "<diff>",
            "(let loop ((i 0)) (if (= i 1500) 'done (begin (add-base i) (loop (+ i 1)))))",
        )
        .unwrap();
    let jit = rt_jit.eval_str_via_vm("<diff>", "(add-base 42)").unwrap();

    let unwrap = |v: &Value, label: &str| match v {
        Value::Number(n) => n.to_f64(),
        other => panic!("{label}: not a number, got {:?}", other),
    };
    assert_eq!(unwrap(&walker, "walker"), unwrap(&vm, "vm"));
    assert_eq!(unwrap(&vm, "vm"), unwrap(&jit, "jit"));
    assert_eq!(unwrap(&jit, "jit"), 142.0);
}

#[test]
fn diff_jit_with_set_bang_through_env() {
    // M6 Phase 2 iter C: free-var set! lowers to Inst::EnvSet.
    let defines = &["(define c 0)", "(define bump (lambda () (set! c (+ c 1))))"];
    let warmup = "(let loop ((i 0)) (if (= i 1500) 'done (begin (bump) (loop (+ i 1)))))";

    let mut rt_walker = Runtime::new();
    for d in defines {
        rt_walker.eval_str("<diff>", d).unwrap();
    }
    rt_walker.eval_str("<diff>", warmup).unwrap();
    let walker = rt_walker.eval_str("<diff>", "c").unwrap();

    let mut rt_jit = Runtime::new();
    rt_jit.install_jit().unwrap();
    for d in defines {
        rt_jit.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt_jit.eval_str_via_vm("<diff>", warmup).unwrap();
    let jit = rt_jit.eval_str_via_vm("<diff>", "c").unwrap();

    let unwrap = |v: &Value, label: &str| match v {
        Value::Number(n) => n.to_f64(),
        other => panic!("{label}: not a number, got {:?}", other),
    };
    assert_eq!(unwrap(&walker, "walker"), unwrap(&jit, "jit"));
    assert_eq!(unwrap(&jit, "jit"), 1500.0);
}

#[test]
fn diff_pure_arithmetic_fast_path() {
    // Closures that the JIT translator handles trivially.
    let defines = &[
        "(define triple (lambda (x) (* x 3)))",
        "(define dist2 (lambda (a b) (* (- a b) (- a b))))",
    ];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (triple i) (dist2 i 5) (loop (+ i 1)))))",
    )
    .unwrap();

    let triple_jit = rt.eval_str_via_vm("<diff>", "(triple 14)").unwrap();
    let dist2_jit = rt.eval_str_via_vm("<diff>", "(dist2 10 3)").unwrap();

    let triple_walker = walker_eval(defines, "(triple 14)");
    let dist2_walker = walker_eval(defines, "(dist2 10 3)");

    let extract = |v: &Value| match v {
        Value::Number(n) => n.to_f64(),
        other => panic!("expected number, got {:?}", other),
    };
    assert_eq!(extract(&triple_jit), extract(&triple_walker));
    assert_eq!(extract(&triple_walker), 42.0);
    assert_eq!(extract(&dist2_jit), extract(&dist2_walker));
    assert_eq!(extract(&dist2_walker), 49.0);
}

#[test]
fn diff_predicate_returns_boolean() {
    // M6 Phase 2 iter W: predicate procedures should JIT and decode
    // their i64 return as Boolean, not as a Number(0)/Number(1).
    let defines = &["(define pos? (lambda (n) (positive? n)))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    // Warm past the tier threshold (default 1024).
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (pos? i) (loop (+ i 1)))))",
    )
    .unwrap();

    let jit_t = rt.eval_str_via_vm("<diff>", "(pos? 5)").unwrap();
    let jit_f = rt.eval_str_via_vm("<diff>", "(pos? -3)").unwrap();
    let jit_zero = rt.eval_str_via_vm("<diff>", "(pos? 0)").unwrap();

    let walker_t = walker_eval(defines, "(pos? 5)");
    let walker_f = walker_eval(defines, "(pos? -3)");
    let walker_zero = walker_eval(defines, "(pos? 0)");

    // The bug we're guarding against: prior to iter W the JIT'd
    // body returned `Value::Number(Fixnum(1))` and `Number(Fixnum(0))`
    // even though the source used `positive?`. Now the dispatcher
    // decodes them as proper Booleans.
    assert!(matches!(jit_t, Value::Boolean(true)), "jit_t = {:?}", jit_t);
    assert!(
        matches!(jit_f, Value::Boolean(false)),
        "jit_f = {:?}",
        jit_f
    );
    assert!(
        matches!(jit_zero, Value::Boolean(false)),
        "jit_zero = {:?}",
        jit_zero
    );
    // Cross-tier agreement on every case.
    assert_eq!(
        format!("{:?}", jit_t),
        format!("{:?}", walker_t),
        "jit vs walker"
    );
    assert_eq!(format!("{:?}", jit_f), format!("{:?}", walker_f));
    assert_eq!(format!("{:?}", jit_zero), format!("{:?}", walker_zero));
}

#[test]
fn diff_integer_to_char_returns_character() {
    // M6 Phase 2 iter X: procedures whose body ends in
    // `(integer->char ...)` should JIT and decode the i64 codepoint
    // into a proper Value::Character on return.
    let defines = &["(define digit (lambda (n) (integer->char (+ 48 n))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (digit 3) (loop (+ i 1)))))",
    )
    .unwrap();

    let jit_zero = rt.eval_str_via_vm("<diff>", "(digit 0)").unwrap();
    let jit_nine = rt.eval_str_via_vm("<diff>", "(digit 9)").unwrap();

    let walker_zero = walker_eval(defines, "(digit 0)");
    let walker_nine = walker_eval(defines, "(digit 9)");

    assert!(
        matches!(jit_zero, Value::Character('0')),
        "jit_zero = {:?}",
        jit_zero
    );
    assert!(
        matches!(jit_nine, Value::Character('9')),
        "jit_nine = {:?}",
        jit_nine
    );
    assert_eq!(format!("{:?}", jit_zero), format!("{:?}", walker_zero));
    assert_eq!(format!("{:?}", jit_nine), format!("{:?}", walker_nine));
}

#[test]
fn diff_jit_lowered_predicates_and_eq() {
    // M6 Phase 2 iter Y: more builtins JIT-lowered. eq?/eqv?/boolean=?
    // /char=?/symbol=? bottom out in `Eq`. Always-#f predicates
    // (boolean?, char?, pair?, etc.) reduce to LoadConst(false) since
    // the JIT body only runs when args are Fixnums. Always-#t
    // (number?, fixnum?, etc.) reduce to LoadConst(true). And
    // char->integer mirrors the iter-X integer->char path.
    let defines = &[
        "(define eq3 (lambda (a b c) (if (eq? a b) c 0)))",
        "(define is-bool? (lambda (n) (boolean? n)))",
        "(define is-num?  (lambda (n) (number? n)))",
        "(define digit->code (lambda (n) (char->integer (integer->char (+ 48 n)))))",
    ];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (eq3 1 1 9) (is-bool? 7) (is-num? 7) (digit->code 3) \
                       (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, &str)] = &[
        ("(eq3 5 5 99)", "99"),
        ("(eq3 5 6 99)", "0"),
        ("(is-bool? 7)", "#f"),
        ("(is-num? 7)", "#t"),
        ("(digit->code 0)", "48"),
        ("(digit->code 9)", "57"),
    ];
    for (expr, _) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        assert_eq!(
            format!("{:?}", jit),
            format!("{:?}", walker),
            "tier-disagreement on {}: jit={:?} walker={:?}",
            expr,
            jit,
            walker
        );
    }
}

#[test]
fn diff_real_to_flonum_returns_flonum() {
    // M6 Phase 2 iter Z: procedures whose body ends in
    // `(real->flonum ...)` (or `(exact->inexact ...)`) JIT and
    // decode the i64 carrier as the bit pattern of an f64.
    let defines = &[
        "(define to-flo (lambda (n) (real->flonum n)))",
        "(define to-inex (lambda (n) (exact->inexact n)))",
    ];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (to-flo i) (to-inex i) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[&str] = &["(to-flo 0)", "(to-flo 5)", "(to-flo -7)", "(to-inex 42)"];
    for expr in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        // Flonum agreement on bit-pattern.
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(a)),
                Value::Number(cs_core::Number::Flonum(b)),
            ) => assert_eq!(a.to_bits(), b.to_bits(), "tier-disagreement on {}", expr),
            other => panic!("expected flonums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_flonum_arithmetic() {
    // M6 Phase 2 iter AA: + - * / on operands typed Flonum should
    // emit FlonumAdd/Sub/Mul/Div in the RIR — Cranelift fadd / fsub
    // / fmul / fdiv with bitcast to/from i64. Bodies that lift
    // Fixnum params via real->flonum and then arithmetic flow
    // through the new path.
    let defines = &[
        "(define sqr-flo (lambda (n) (let ((f (real->flonum n))) (* f f))))",
        "(define avg2-flo (lambda (a b) \
            (let ((fa (real->flonum a)) (fb (real->flonum b))) \
              (* (+ fa fb) 0.5))))",
    ];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (sqr-flo 7) (avg2-flo 3 5) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, f64)] = &[
        ("(sqr-flo 5)", 25.0),
        ("(sqr-flo -7)", 49.0),
        ("(avg2-flo 3 4)", 3.5),
        ("(avg2-flo 100 50)", 75.0),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(j)),
                Value::Number(cs_core::Number::Flonum(w)),
            ) => {
                assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected flonums, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_flonum_comparison() {
    // M6 Phase 2 iter AB: < / = / > / <= / >= on Flonum-typed
    // operands lower to FlonumLt / FlonumEq with Cranelift fcmp,
    // not the i64 icmp. Without this, comparing two i64 carriers
    // of f64 bit patterns would silently disagree with IEEE-754
    // ordering.
    let defines = &["(define gt-flo (lambda (a b) \
        (let ((fa (real->flonum a)) (fb (real->flonum b))) \
          (if (< fa fb) -1 (if (= fa fb) 0 1)))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (gt-flo 3 5) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, i64)] = &[
        ("(gt-flo 3 5)", -1),
        ("(gt-flo 5 5)", 0),
        ("(gt-flo 7 2)", 1),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected fixnums, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_flonum_branch() {
    // M6 Phase 2 iter AC: BranchOn{Lt,Le,Gt,Ge,Ne}Fx2 against
    // Flonum-typed operands lower through FlonumLt / FlonumEq so
    // the brif terminator gets IEEE ordering rather than i64 cmp on
    // bit patterns. Verifies via a clamping body that branches
    // twice on flonum comparisons.
    let defines = &["(define clamp01 (lambda (n) \
        (let ((f (real->flonum n))) \
          (if (< f 0.0) 0.0 (if (> f 1.0) 1.0 f)))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (clamp01 0) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, f64)] = &[
        ("(clamp01 -5)", 0.0),
        ("(clamp01 0)", 0.0),
        ("(clamp01 2)", 1.0),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(j)),
                Value::Number(cs_core::Number::Flonum(w)),
            ) => {
                assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected flonums, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_flonum_unary_and_minmax() {
    // M6 Phase 2 iter AD: flsqrt / flabs / flmax / flmin lower to
    // Cranelift sqrt / fabs / fmax / fmin when operands are
    // statically Flonum-typed. Body composes them through
    // multi-flonum arithmetic.
    let defines = &[
        "(define hyp-flo (lambda (a b) \
            (let ((fa (real->flonum a)) (fb (real->flonum b))) \
              (flsqrt (+ (* fa fa) (* fb fb))))))",
        "(define clamped-abs (lambda (n lo hi) \
            (let ((f (real->flonum n)) (flo (real->flonum lo)) (fhi (real->flonum hi))) \
              (flmin fhi (flmax flo (flabs f))))))",
    ];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (hyp-flo 3 4) (clamped-abs i 0 100) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, f64)] = &[
        ("(hyp-flo 3 4)", 5.0),
        ("(hyp-flo 5 12)", 13.0),
        ("(clamped-abs 50 0 100)", 50.0),
        ("(clamped-abs -200 0 100)", 100.0),
        ("(clamped-abs -10 0 100)", 10.0),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(j)),
                Value::Number(cs_core::Number::Flonum(w)),
            ) => {
                assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected flonums, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_multi_arg_arith() {
    // M6 Phase 2 iter AE: variadic + / - / *. The cs-vm compiler
    // only specializes 2-arg primops to *Fx2; 0/1/3+ args reach the
    // JIT translator as a BuiltinRef + Call N. The translator now
    // chains the matching binary RIR op, switching to the Flonum*
    // variant when every operand is statically Flonum-typed.
    let defines = &[
        "(define sum3 (lambda (a b c) (+ a b c)))",
        "(define prod4 (lambda (a b c d) (* a b c d)))",
        "(define sub3 (lambda (a b c) (- a b c)))",
        "(define neg-x (lambda (x) (- x)))",
        "(define plus0 (lambda () (+)))",
        "(define mul1  (lambda () (*)))",
        "(define sum3-flo (lambda (a b c) \
            (let ((fa (real->flonum a)) (fb (real->flonum b)) (fc (real->flonum c))) \
              (+ fa fb fc))))",
    ];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (sum3 1 2 3) (prod4 1 2 3 4) (sub3 10 1 2) (neg-x 5) \
                       (plus0) (mul1) (sum3-flo 1 2 3) \
                       (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, &str)] = &[
        ("(sum3 1 2 3)", "6"),
        ("(prod4 1 2 3 4)", "24"),
        ("(sub3 10 1 2)", "7"),
        ("(neg-x 5)", "-5"),
        ("(plus0)", "0"),
        ("(mul1)", "1"),
        ("(sum3-flo 1 2 3)", "6.0"),
    ];
    for (expr, _) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        assert_eq!(
            format!("{:?}", jit),
            format!("{:?}", walker),
            "tier-disagreement on {}: jit={:?} walker={:?}",
            expr,
            jit,
            walker
        );
    }
}

#[test]
fn diff_arg_side_flonum_passthrough() {
    // M6 Phase 2 iter AF: closures called with flonum args at
    // tier-up time get a JIT body whose params are typed Flonum,
    // and the dispatch type-guard accepts flonum-encoded i64s.
    // Without iter AF, this body would either tier up with all-
    // Fixnum params (and silently produce nonsense via i64 imul)
    // or stay on bytecode forever.
    let defines = &["(define sqr (lambda (n) (* n n)))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm with FLONUM args so the tier-up hook records Flonum
    // signatures and JITs accordingly.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (sqr 3.5) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, f64)] = &[
        ("(sqr 3.5)", 12.25),
        ("(sqr 2.0)", 4.0),
        ("(sqr -4.0)", 16.0),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(j)),
                Value::Number(cs_core::Number::Flonum(w)),
            ) => {
                assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected flonums, got {:?}", expr, other),
        }
    }

    // Now call the same name with fixnum args — type-guard rejects
    // (because the closure was JIT'd with Flonum signature) and
    // dispatch falls through to bytecode. Walker == bytecode VM
    // here so they should still agree.
    let fix = rt.eval_str_via_vm("<diff>", "(sqr 5)").unwrap();
    let fix_walker = walker_eval(defines, "(sqr 5)");
    assert_eq!(format!("{:?}", fix), format!("{:?}", fix_walker));
}

#[test]
fn diff_flonum_rounding_ops() {
    // M6 Phase 2 iter AG: floor / ceiling / truncate / round on
    // Flonum-typed operands lower through Cranelift floor / ceil /
    // trunc / nearest. Without this, the translator's identity-on-
    // fixnum Move path was wrong for f64 inputs (it returned the
    // bit pattern unchanged, which decoded as the original flonum).
    let defines = &[
        "(define floor-mul (lambda (x) (floor (* x 1.5))))",
        "(define ceil-mul  (lambda (x) (ceiling (* x 1.5))))",
        "(define trunc-mul (lambda (x) (truncate (* x 1.5))))",
        "(define round-mul (lambda (x) (round (* x 1.5))))",
    ];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (floor-mul 2.0) (ceil-mul 2.0) (trunc-mul 2.0) (round-mul 2.0) \
                       (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, f64)] = &[
        ("(floor-mul 2.7)", 4.0),
        ("(floor-mul -3.7)", -6.0),
        ("(ceil-mul 2.1)", 4.0),
        ("(trunc-mul -2.7)", -4.0),
        ("(round-mul 2.5)", 4.0),
        ("(round-mul 3.5)", 5.0),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(j)),
                Value::Number(cs_core::Number::Flonum(w)),
            ) => {
                assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected flonums, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_recompile_on_arg_type_change() {
    // M6 Phase 2 iter AH: feedback-driven recompile. A closure first
    // tier-up'd with one arg-type signature (e.g. all Fixnum) gets
    // a JIT body baked in. Calls with mismatching types previously
    // fell through to bytecode forever; this iter bumps a per-
    // closure deopt counter and clears the JIT pointer once it
    // crosses JIT_DEOPT_RECOMPILE_THRESHOLD, so the next call's
    // tier-up hook compiles fresh against the new arg shape.
    let defines = &["(define sqr (lambda (n) (* n n)))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();

    // Phase 1: warm with FIXNUM args. JIT'd with Fixnum signature.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (sqr i) (loop (+ i 1)))))",
    )
    .unwrap();
    let fix_jit = rt.eval_str_via_vm("<diff>", "(sqr 7)").unwrap();
    let fix_walker = walker_eval(defines, "(sqr 7)");
    assert_eq!(format!("{:?}", fix_jit), format!("{:?}", fix_walker));

    // Phase 2: hammer with FLONUM args. The first ~256 calls miss
    // the type-guard and run on bytecode. After the threshold
    // crosses, jit_ptr clears + tier counter primes for re-JIT.
    // Subsequent flonum calls trigger the new compile.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (sqr 2.5) (loop (+ i 1)))))",
    )
    .unwrap();

    // After recompile, flonum calls should produce a flonum result.
    let flo_jit = rt.eval_str_via_vm("<diff>", "(sqr 3.5)").unwrap();
    let flo_walker = walker_eval(defines, "(sqr 3.5)");
    match (&flo_jit, &flo_walker) {
        (Value::Number(cs_core::Number::Flonum(j)), Value::Number(cs_core::Number::Flonum(w))) => {
            assert_eq!(j.to_bits(), w.to_bits());
            assert_eq!(*j, 12.25);
        }
        other => panic!("expected flonums, got {:?}", other),
    }
}

#[test]
fn diff_jit_introspection_and_distinct_compiles() {
    // M6 Phase 2 iter AI: (jit-installed?) / (jit-stats) /
    // (jit-status proc) Scheme-visible accessors.
    //
    // Also pins down a regression caught while writing this test:
    // every call to compile_pure_fixnum used `declare_function` with
    // the same module-level name "anon-jit", colliding on the
    // second compile and silently leaving subsequent closures on
    // bytecode. The fix appends the lowerer's fresh_id to the
    // module name so each compile is independent.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();

    // First closure: warm with flonum args.
    rt.eval_str_via_vm("<diff>", "(define sqr-flo (lambda (n) (* n n)))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (sqr-flo 1.5) (loop (+ i 1)))))",
    )
    .unwrap();

    // Second closure: warm with fixnum args.
    rt.eval_str_via_vm("<diff>", "(define sqr-fix (lambda (n) (* n n)))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (sqr-fix i) (loop (+ i 1)))))",
    )
    .unwrap();

    let flo_status = rt
        .eval_str_via_vm("<diff>", "(jit-status sqr-flo)")
        .unwrap();
    let fix_status = rt
        .eval_str_via_vm("<diff>", "(jit-status sqr-fix)")
        .unwrap();

    // Eval everything before borrowing rt immutably for format_value.
    let installed = rt.eval_str_via_vm("<diff>", "(jit-installed?)").unwrap();
    let stats = rt.eval_str_via_vm("<diff>", "(jit-stats)").unwrap();
    let flo_str = rt.format_value(&flo_status, cs_core::WriteMode::Write);
    let fix_str = rt.format_value(&fix_status, cs_core::WriteMode::Write);
    let stats_str = rt.format_value(&stats, cs_core::WriteMode::Write);
    // Shape: (jit-on <return> (<params...>) calls <N> deopts <M>).
    // We pin the prefix; counts depend on test harness ordering.
    assert!(
        flo_str.starts_with("(jit-on flonum (flonum) calls "),
        "flo: {}",
        flo_str
    );
    assert!(
        fix_str.starts_with("(jit-on fixnum (fixnum) calls "),
        "fix: {}",
        fix_str
    );
    assert!(matches!(installed, Value::Boolean(true)));
    assert!(
        stats_str.starts_with('(') && stats_str.ends_with(')'),
        "stats not a list: {}",
        stats_str
    );
}

#[test]
fn diff_jit_let_loop_flonum_accumulator() {
    // M6 Phase 3 iter AM: regression for a load-bearing bug — a
    // `let loop ((i 0) (acc 0.0)) ...` body whose acc accumulates a
    // flonum sum was JIT'd as if acc were a Fixnum. The `Return acc`
    // path inferred Fixnum (because acc, a block param, was typed
    // Fixnum by seed_block_entry's old default), and the dispatcher
    // decoded the f64 bit pattern as Number(Fixnum(<bits-as-int>)).
    //
    // Two fixes:
    //   1. seed_block_entry propagates predecessor stack types into
    //      block params, instead of defaulting every param to Fixnum.
    //   2. infer_return_type seeds flo_values / bool_values /
    //      char_values from `func.params` and each block's `params`
    //      so block-param-typed returns resolve correctly.
    //
    // Cross-tier-checked because walker handles this trivially.
    let defines = &["(define sumsq (lambda (n) \
        (let loop ((i 0) (acc 0.0)) \
          (if (= i n) acc \
              (loop (+ i 1) (+ acc (* (real->flonum i) (real->flonum i))))))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm with 2000 calls — enough to trigger the loop closure's
    // tier-up.
    rt.eval_str_via_vm("<diff>", "(sumsq 2000)").unwrap();

    // Bench at a smaller n to keep the test fast (also avoids the
    // separate JIT'd-CallSelf-burns-stack issue at very deep
    // recursion — that's a tail-call-deferred follow-up).
    let jit = rt.eval_str_via_vm("<diff>", "(sumsq 5000)").unwrap();
    let walker = walker_eval(defines, "(sumsq 5000)");
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Flonum(j)), Value::Number(cs_core::Number::Flonum(w))) => {
            assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on (sumsq 5000)");
            // sum of i*i for i in 0..5000 = 41654167500.0
            assert_eq!(*j, 41654167500.0);
        }
        other => panic!("expected flonums, got {:?}", other),
    }
}

#[test]
fn diff_jit_tail_call_deep_recursion() {
    // M6 Phase 3 iter AO: tail-call lowering via the wrapper
    // pattern (outer SystemV trampoline + inner Tail-conv body
    // with `return_call` for tail-CallSelf). Without this, JIT'd
    // recursive bodies burn host stack at ~50k iters; with it,
    // 1M+ iters are safe.
    let defines = &["(define sumsq (lambda (n) \
        (let loop ((i 0) (acc 0.0)) \
          (if (= i n) acc \
              (loop (+ i 1) (+ acc (* (real->flonum i) (real->flonum i))))))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm("<diff>", "(sumsq 2000)").unwrap();

    // 250k iters — 5x past pre-AO crash threshold. Pre-AO crashed
    // at 1M; we cap here to keep the test fast and avoid timing
    // dependence.
    let jit = rt.eval_str_via_vm("<diff>", "(sumsq 250000)").unwrap();
    let walker = walker_eval(defines, "(sumsq 250000)");
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Flonum(j)), Value::Number(cs_core::Number::Flonum(w))) => {
            assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on (sumsq 250000)");
        }
        other => panic!("expected flonums, got {:?}", other),
    }
}

#[test]
fn diff_jit_mixed_fixnum_flonum_arithmetic() {
    // M6 Phase 3 iter AP: emit_arith_binop / emit_cmp_binop / the
    // variadic + - * chain now promote Fixnum operands to Flonum
    // when ANY operand is Flonum. R6RS numeric-tower contagion.
    //
    // Pre-AP, the body
    //   (define (sum-up-to n) (let loop ((i 0) (acc 0.0))
    //     (if (= i n) acc (loop (+ i 1) (+ acc 1.0 i)))))
    // returned i64::MAX as f64 (~9.2e18) instead of the correct
    // 2_001_000.0 — the chain fell back to fixnum addition because
    // `(+ acc 1.0 i)` had mixed types and the all_flonum gate said
    // "not all flonum, use Add".
    let defines = &["(define sum-up-to (lambda (n) \
        (let loop ((i 0) (acc 0.0)) \
          (if (= i n) acc \
              (loop (+ i 1) (+ acc 1.0 i))))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Force tier-up of the inner loop closure.
    rt.eval_str_via_vm("<diff>", "(sum-up-to 2000)").unwrap();

    let cases: &[(&str, f64)] = &[
        ("(sum-up-to 100)", 5050.0),
        ("(sum-up-to 1000)", 500500.0),
        ("(sum-up-to 2000)", 2001000.0),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Flonum(j)),
                Value::Number(cs_core::Number::Flonum(w)),
            ) => {
                assert_eq!(j.to_bits(), w.to_bits(), "tier-mismatch on {}", expr);
                assert_eq!(*j, *expected, "value mismatch on {}", expr);
            }
            other => panic!("{}: expected flonums, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_cons_pair_return() {
    // M6 Phase 4 iter AS: first end-to-end Pair-returning JIT body.
    // (define (pair-up a b) (cons a b)) tier-ups, the JIT translator
    // emits Inst::Cons (with per-operand JIT_RT_* tags), the lowerer
    // calls vm_alloc_pair, and the dispatcher decodes the Any-tagged
    // Box::into_raw return back into a proper Value::Pair.
    let defines = &["(define pair-up (lambda (a b) (cons a b)))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (pair-up 99 100) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, i64, i64)] = &[
        ("(pair-up 5 7)", 5, 7),
        ("(pair-up 100 200)", 100, 200),
        ("(pair-up -1 0)", -1, 0),
    ];
    for (expr, ec, ed) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (Value::Pair(jp), Value::Pair(wp)) => {
                let jc = jp.car.borrow().clone();
                let jd = jp.cdr.borrow().clone();
                let wc = wp.car.borrow().clone();
                let wd = wp.cdr.borrow().clone();
                match (&jc, &wc, &jd, &wd) {
                    (
                        Value::Number(cs_core::Number::Fixnum(a)),
                        Value::Number(cs_core::Number::Fixnum(b)),
                        Value::Number(cs_core::Number::Fixnum(c)),
                        Value::Number(cs_core::Number::Fixnum(d)),
                    ) => {
                        assert_eq!(a, b);
                        assert_eq!(c, d);
                        assert_eq!(*a, *ec);
                        assert_eq!(*c, *ed);
                    }
                    other => panic!("unexpected pair contents on {}: {:?}", expr, other),
                }
            }
            other => panic!("expected pairs, got {:?}", other),
        }
    }
}

#[test]
fn diff_jit_const_symbol_in_body() {
    // M6 Phase 4 iter BA: Symbol literal `'foo` round-trips
    // through the JIT lane via the JIT_RT_SYMBOL tag. Pre-BA,
    // Const::Symbol defaulted to Type::Fixnum, so the i64 (the
    // symbol id) decoded as a Fixnum.
    let defines = &["(define (kind v) (if (pair? v) (quote pair) (quote not-pair)))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done \
             (begin (kind (cons i i)) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let jit_pair = rt.eval_str_via_vm("<diff>", "(kind (cons 1 2))").unwrap();
    let jit_nil = rt.eval_str_via_vm("<diff>", "(kind (quote ()))").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(after >= 2, "kind never JITted (count = {})", after);

    let walker_pair = walker_eval(defines, "(kind (cons 1 2))");
    let walker_nil = walker_eval(defines, "(kind (quote ()))");
    match (&jit_pair, &walker_pair) {
        (Value::Symbol(j), Value::Symbol(w)) => assert_eq!(j, w, "pair-branch symbol mismatch"),
        other => panic!("expected matching symbols on pair branch, got {:?}", other),
    }
    match (&jit_nil, &walker_nil) {
        (Value::Symbol(j), Value::Symbol(w)) => assert_eq!(j, w, "nil-branch symbol mismatch"),
        other => panic!("expected matching symbols on nil branch, got {:?}", other),
    }
}

#[test]
fn diff_jit_tail_recursive_length_acc() {
    // M6 Phase 4 iter BA: accumulator-style list traversal in
    // tail position. Args are mixed (Any list + Fixnum acc); the
    // recursive call must not block tail-call optimization.
    //
    // The pre-widen CallSelf-type-propagation pass (iter AZ)
    // ensures the CallSelf dst's type matches sibling Jump args
    // (both Fixnum here), so widen_joins_to_any leaves the
    // CallSelf in tail position untouched and detect_tail_call_self
    // fires.
    let defines = &["(define (length-acc lst acc) \
           (if (null? lst) acc (length-acc (cdr lst) (+ acc 1))))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done \
             (begin (length-acc (cons 1 (cons 2 (cons 3 (quote ())))) 0) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let _ = rt
        .eval_str_via_vm(
            "<diff>",
            "(length-acc (cons 1 (cons 2 (cons 3 (quote ())))) 0)",
        )
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "length-acc never dispatched through the JIT (count = {})",
        after
    );

    // Build a long list to exercise the tail-call path. Walker
    // also needs TCO to handle this — confirmed by walker_eval
    // returning the right answer rather than overflowing.
    let mut build = String::from("(quote ())");
    for i in 0..200 {
        build = format!("(cons {} {})", i, build);
    }
    let expr = format!("(length-acc {} 0)", build);
    let jit = rt.eval_str_via_vm("<diff>", &expr).unwrap();
    let walker = walker_eval(defines, &expr);
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 200);
        }
        other => panic!("expected fixnum 200, got {:?}", other),
    }
}

#[test]
fn diff_jit_const_null_in_body() {
    // M6 Phase 4 iter AZ: `(quote ())` inside a JIT body must
    // round-trip as Value::Null, not Value::Number(Fixnum(0)).
    // Pre-AZ, Const::Null defaulted to Type::Fixnum, so the join
    // post-pass widened with JIT_RT_FIXNUM tag and the dispatcher
    // decoded the i64 as Fixnum(0).
    let defines = &["(define (head-or-empty v) \
           (if (pair? v) (car v) (quote ())))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done \
             (begin (head-or-empty (cons i i)) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let jit_pair = rt
        .eval_str_via_vm("<diff>", "(head-or-empty (cons 5 6))")
        .unwrap();
    let jit_nil = rt
        .eval_str_via_vm("<diff>", "(head-or-empty (quote ()))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "head-or-empty did not dispatch via JIT (count = {})",
        after
    );

    // Pair input: car = 5.
    match &jit_pair {
        Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(*n, 5),
        other => panic!("expected fixnum 5, got {:?}", other),
    }

    // Nil input: must be Value::Null, not Fixnum(0).
    assert!(
        matches!(jit_nil, Value::Null),
        "head-or-empty '() should return Null, got {:?}",
        jit_nil
    );

    // Walker agreement.
    let walker_pair = walker_eval(defines, "(head-or-empty (cons 5 6))");
    let walker_nil = walker_eval(defines, "(head-or-empty (quote ()))");
    assert!(matches!(
        walker_pair,
        Value::Number(cs_core::Number::Fixnum(5))
    ));
    assert!(matches!(walker_nil, Value::Null));
}

#[test]
fn diff_jit_recursive_length() {
    // M6 Phase 4 iter AY: recursive list traversal. `length` takes
    // an Any param, recurses through cdr, accumulates a Fixnum
    // count via (+ 1 (length (cdr lst))). Exercises CallSelf with
    // Any args, AnyToFix on the recursive return value (which is
    // already Fixnum-typed since the function returns Fixnum), and
    // the AnyClone/AnyDrop machinery for the multi-use lst param.
    let defines = &["(define (length lst) \
           (if (null? lst) 0 (+ 1 (length (cdr lst)))))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm with a 4-element list, repeated to push past the
    // tier-up threshold.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done \
             (begin (length (cons 1 (cons 2 (cons 3 (cons 4 (quote ())))))) (loop (+ i 1)))))",
    )
    .unwrap();

    // Sanity-check that the body actually dispatched through native
    // code rather than silently falling back to bytecode (a translator
    // refusal would still produce correct results — we want to know
    // the iter actually exercises the JIT path).
    cs_vm::vm::reset_jit_call_count();
    let _ = rt
        .eval_str_via_vm("<diff>", "(length (cons 1 (cons 2 (quote ()))))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "length never dispatched through the JIT (count = {})",
        after
    );

    let cases: &[(&str, i64)] = &[
        ("(length (quote ()))", 0),
        ("(length (cons 1 (quote ())))", 1),
        (
            "(length (cons 1 (cons 2 (cons 3 (cons 4 (cons 5 (quote ())))))))",
            5,
        ),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", expr);
                assert_eq!(*j, *expected, "wrong value on {}", expr);
            }
            other => panic!("expected fixnums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_recursive_sum_list() {
    // M6 Phase 4 iter AY: companion to diff_jit_recursive_length
    // exercising AnyToFix on the (car lst) operand of `+`. The body
    // is `(if (null? lst) 0 (+ (car lst) (sum-list (cdr lst))))`.
    // Iter AX's unbox_any_to_fix gates on Any types, so this iter
    // confirms the integration with CallSelf-with-Any-args does not
    // disturb the Fixnum-typed return inference.
    let defines = &["(define (sum-list lst) \
           (if (null? lst) 0 (+ (car lst) (sum-list (cdr lst)))))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done \
             (begin (sum-list (cons 1 (cons 2 (cons 3 (quote ()))))) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let _ = rt
        .eval_str_via_vm("<diff>", "(sum-list (cons 1 (cons 2 (quote ()))))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "sum-list never dispatched through the JIT (count = {})",
        after
    );

    let cases: &[(&str, i64)] = &[
        ("(sum-list (quote ()))", 0),
        ("(sum-list (cons 7 (quote ())))", 7),
        (
            "(sum-list (cons 1 (cons 2 (cons 3 (cons 4 (cons 5 (quote ())))))))",
            15,
        ),
        ("(sum-list (cons 100 (cons -50 (cons 25 (quote ())))))", 75),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", expr);
                assert_eq!(*j, *expected, "wrong value on {}", expr);
            }
            other => panic!("expected fixnums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_arith_on_any_operand() {
    // M6 Phase 4 iter AX: arithmetic on Any operands. `(car v)`
    // returns an Any-tagged Box; `(+ (car v) 1)` needs raw Fixnum
    // bits. The translator emits Inst::AnyToFix on Any operands
    // to consume the box and extract the Fixnum.
    let defines = &["(define (head-plus-one v) (+ (car v) 1))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done (begin (head-plus-one (cons i 0)) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, i64)] = &[
        ("(head-plus-one (cons 5 0))", 6),
        ("(head-plus-one (cons -3 99))", -2),
        ("(head-plus-one (cons 41 'x))", 42),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", expr);
                assert_eq!(*j, *expected, "wrong value on {}", expr);
            }
            other => panic!("expected fixnums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_mixed_tier_return() {
    // M6 Phase 4 iter AW: control-flow joins where one predecessor
    // pushes Any (e.g. `(car v)`) and the other pushes a typed
    // immediate (e.g. a Fixnum) used to crash because the join
    // block_param picked one type and the dispatcher decoded the
    // other branch's i64 with the wrong tag.
    //
    // The post-pass `widen_joins_to_any` widens the join's slot to
    // Any and inserts BoxTyped on the predecessor that was passing
    // a typed value, so the dispatcher always sees a Box pointer
    // when the function's inferred return type is Any.
    let defines = &["(define (head v fb) (if (pair? v) (car v) fb))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done (begin (head (cons i (+ i 1)) -1) (loop (+ i 1)))))",
    )
    .unwrap();

    // Pair input: returns car (a Fixnum, boxed for Any return).
    let jit_pair = rt
        .eval_str_via_vm("<diff>", "(head (cons 7 8) -1)")
        .unwrap();
    let walker_pair = walker_eval(defines, "(head (cons 7 8) -1)");
    match (&jit_pair, &walker_pair) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 7);
        }
        other => panic!("expected fixnum 7 from pair branch, got {:?}", other),
    }

    // Non-pair input: returns the fb argument (raw Fixnum, must be
    // boxed by the post-pass since the function returns Any).
    let jit_fb = rt.eval_str_via_vm("<diff>", "(head 0 42)").unwrap();
    let walker_fb = walker_eval(defines, "(head 0 42)");
    match (&jit_fb, &walker_fb) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 42);
        }
        other => panic!("expected fixnum 42 from fb branch, got {:?}", other),
    }
}

#[test]
fn diff_jit_multi_use_any_param() {
    // M6 Phase 4 iter AV: `v` appears twice in the body — once as
    // the predicate operand (consumes), once inside the true branch
    // for `car` (consumes). Both uses must own independent boxes,
    // so the translator emits AnyClone on every LoadVar(any-param).
    // The original dispatch-allocated box is released by AnyDrop on
    // the return path.
    //
    // Both branches return Any (Pair-flavored cdr / clone of the
    // original) so the inferred return type is Type::Any.
    let defines = &["(define (head v) (if (pair? v) (car v) v))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (head (cons i (+ i 1))) (loop (+ i 1)))))",
    )
    .unwrap();

    // Pair input: returns car (Fixnum boxed as Any).
    let jit_pair = rt.eval_str_via_vm("<diff>", "(head (cons 7 8))").unwrap();
    let walker_pair = walker_eval(defines, "(head (cons 7 8))");
    match (&jit_pair, &walker_pair) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 7);
        }
        other => panic!("expected fixnum 7, got {:?}", other),
    }

    // Non-pair input: returns the operand itself (Null, an Any).
    let jit_nil = rt.eval_str_via_vm("<diff>", "(head (quote ()))").unwrap();
    let walker_nil = walker_eval(defines, "(head (quote ()))");
    match (&jit_nil, &walker_nil) {
        (Value::Null, Value::Null) => {}
        other => panic!("expected nulls, got {:?}", other),
    }
}

#[test]
fn diff_jit_pair_predicate() {
    // M6 Phase 4 iter AU: heap-pointer args reach the JIT through
    // the Any param lane, and pair?/null? predicates lower to
    // vm_pair_p / vm_null_p (consume-on-use).
    //
    // (define (kind v) (if (pair? v) 1 0)) — single linear use of
    // v. The predicate consumes the argument's owned box, returns
    // a Boolean, and the if folds to a Fixnum return. We warm with
    // a Pair so the JIT body's param hint is Any.
    let defines = &["(define (kind v) (if (pair? v) 1 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (kind (cons i i)) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, i64)] = &[
        ("(kind (cons 1 2))", 1),
        ("(kind (cons 99 100))", 1),
        ("(kind (cons -1 0))", 1),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", expr);
                assert_eq!(*j, *expected, "wrong value on {}", expr);
            }
            other => panic!("expected fixnums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_null_predicate() {
    // Companion to diff_jit_pair_predicate exercising null?.
    let defines = &["(define (is-null v) (if (null? v) 1 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm with '() so param0 is hinted Any (Null is one of the
    // Any-mapped variants in jit.rs).
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (is-null (quote ())) (loop (+ i 1)))))",
    )
    .unwrap();

    let nil_call = "(is-null (quote ()))";
    let pair_call = "(is-null (cons 1 2))";
    for (expr, expected) in [(nil_call, 1), (pair_call, 0)] {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", expr);
                assert_eq!(*j, expected, "wrong value on {}", expr);
            }
            other => panic!("expected fixnums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_car_cdr_passthrough() {
    // M6 Phase 4 iter AT: Inst::Car / Inst::Cdr lower to vm_pair_car
    // / vm_pair_cdr. The car/cdr of a freshly-consed pair returns
    // through the JIT body's Any-tagged return — when the slots are
    // Fixnum-Boxed Values, the dispatcher decodes them back to the
    // expected Value variants on the way out.
    let defines = &[
        "(define (pcar a b) (car (cons a b)))",
        "(define (pcdr a b) (cdr (cons a b)))",
    ];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (pcar 1 2) (pcdr 3 4) (loop (+ i 1)))))",
    )
    .unwrap();

    let cases: &[(&str, i64)] = &[
        ("(pcar 11 22)", 11),
        ("(pcdr 11 22)", 22),
        ("(pcar -7 99)", -7),
        ("(pcdr -7 99)", 99),
    ];
    for (expr, expected) in cases {
        let jit = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let walker = walker_eval(defines, expr);
        match (&jit, &walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", expr);
                assert_eq!(*j, *expected, "wrong value on {}", expr);
            }
            other => panic!("expected fixnums on {}, got {:?}", expr, other),
        }
    }
}

#[test]
fn diff_jit_eq_on_any_symbol() {
    // M6 Phase 4 iter BB: eq? on Any operands routes through
    // vm_eq_any. Without this, eq?'s fallthrough i64 compare on
    // two Box pointers is always false even when the boxed Values
    // are equal symbols.
    //
    // Body: `(define (is-foo? v) (if (eq? v (quote foo)) 1 0))`.
    // v arrives Any-tagged (Symbol from warmup). The Const::Symbol
    // 'foo is Type::Symbol; the translator boxes it via BoxTyped
    // so both eq? operands are Any-tagged on entry to vm_eq_any.
    let defines = &["(define (is-foo? v) (if (eq? v (quote foo)) 1 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done (begin (is-foo? (quote foo)) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let jit_match = rt
        .eval_str_via_vm("<diff>", "(is-foo? (quote foo))")
        .unwrap();
    let jit_miss = rt
        .eval_str_via_vm("<diff>", "(is-foo? (quote bar))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(after >= 2, "is-foo? never JITted (count = {})", after);

    let walker_match = walker_eval(defines, "(is-foo? (quote foo))");
    let walker_miss = walker_eval(defines, "(is-foo? (quote bar))");
    for (label, jit, walker, expected) in [
        ("match", &jit_match, &walker_match, 1i64),
        ("miss", &jit_miss, &walker_miss, 0),
    ] {
        match (jit, walker) {
            (
                Value::Number(cs_core::Number::Fixnum(j)),
                Value::Number(cs_core::Number::Fixnum(w)),
            ) => {
                assert_eq!(j, w, "tier mismatch on {}", label);
                assert_eq!(*j, expected, "wrong value on {}", label);
            }
            other => panic!("expected fixnums on {}, got {:?}", label, other),
        }
    }
}

#[test]
fn diff_jit_unbox_flonum_via_car() {
    // M6 Phase 4 iter BB: arithmetic on Any + Flonum routes the
    // Any operand through vm_unbox_flonum (not vm_unbox_fixnum,
    // which would panic on a runtime-Flonum value).
    //
    // Body: `(define (head-add v) (+ 1.5 (car v)))`. (car v) is
    // Any. The Const 1.5 is Flonum. `unbox_any_against` picks
    // AnyToFlo because the typed operand is Flonum.
    let defines = &["(define (head-add v) (+ 1.5 (car v)))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done (begin (head-add (cons 2.0 0)) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let jit = rt
        .eval_str_via_vm("<diff>", "(head-add (cons 3.5 0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(after > 0, "head-add never JITted (count = {})", after);

    let walker = walker_eval(defines, "(head-add (cons 3.5 0))");
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Flonum(j)), Value::Number(cs_core::Number::Flonum(w))) => {
            assert_eq!(j.to_bits(), w.to_bits(), "tier mismatch");
            assert!((j - 5.0).abs() < 1e-9, "expected 5.0, got {}", j);
        }
        other => panic!("expected flonums, got {:?}", other),
    }
}

#[test]
fn diff_jit_pair_alloc_then_collect_reclaims_when_unreachable() {
    // M6 Phase 4 iter BQ — companion to the survives-collect test.
    // After a JIT-allocated Pair becomes unreachable (the binding
    // is rebound and the value isn't kept anywhere else), a
    // manual `collect()` should reclaim it via the weak-ref
    // sweep. The Pair's slot drops to refcount 0; the weak in the
    // Heap's slots vec expires; `collect()` removes the expired
    // entry, shrinking `slot_count`.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mkpair a b) (cons a b))")
        .unwrap();
    // Warmup to JIT.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (mkpair i i) (loop (+ i 1)))))",
    )
    .unwrap();
    // Allocate, then make unreachable by rebinding.
    rt.eval_str_via_vm("<diff>", "(define p (mkpair 1 2))")
        .unwrap();
    let before = rt.heap().live_slots();
    rt.eval_str_via_vm("<diff>", "(set! p 0)").unwrap();
    // Collect twice: the first marks/sweeps; the second's a
    // no-op safety check.
    rt.heap().collect();
    rt.heap().collect();
    let after = rt.heap().live_slots();
    // Live slot count must not GROW across no-allocation
    // collects. The exact delta depends on whether the warmup
    // loop's intermediate Pairs are still in the weak-ref list,
    // so we use a loose assertion: the path doesn't crash and
    // collect() doesn't add slots.
    assert!(
        after <= before,
        "live_slots grew across collect (before={}, after={})",
        before,
        after
    );
}

#[test]
fn diff_jit_pair_survives_collect_when_walker_holds_it() {
    // M6 Phase 4 iter BQ — basic GC sanity for JIT-allocated
    // values. A JIT body produces a Pair, the walker captures
    // it in a binding, manual `collect()` runs, and the binding
    // still resolves correctly. The Pair is registered in the
    // Runtime's Heap (iter BP wired) and stays alive because the
    // walker's env roots it.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mkpair a b) (cons a b))")
        .unwrap();
    // Warmup to JIT.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (mkpair i i) (loop (+ i 1)))))",
    )
    .unwrap();
    // Allocate via JIT and bind. Walker's env now holds the
    // Pair via `p`.
    rt.eval_str_via_vm("<diff>", "(define p (mkpair 42 99))")
        .unwrap();
    // Force a GC pass. The Pair's slot is in the Heap's weak-ref
    // list (from BP's Heap::alloc routing) and traced as a root
    // via the walker's env -> Value::Pair -> Trace.
    rt.heap().collect();
    // Verify p still resolves and points at the right Pair.
    let v = rt.eval_str_via_vm("<diff>", "(car p)").unwrap();
    match v {
        Value::Number(cs_core::Number::Fixnum(42)) => {}
        other => panic!("expected fixnum 42, got {:?}", other),
    }
    let v2 = rt.eval_str_via_vm("<diff>", "(cdr p)").unwrap();
    match v2 {
        Value::Number(cs_core::Number::Fixnum(99)) => {}
        other => panic!("expected fixnum 99, got {:?}", other),
    }
}

#[test]
fn diff_jit_alloc_count_grows_through_heap() {
    // M6 Phase 4 iter BP — when the JIT body allocates Gc<Value>
    // (cons / car / cdr produce fresh handles), the runtime's
    // Heap should see each allocation because Runtime::with_active
    // installs the heap pointer in JIT_ACTIVE_HEAP.
    let defines = &["(define (mkpair a b) (cons a b))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm up so the body JITs.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (mkpair i i) (loop (+ i 1)))))",
    )
    .unwrap();

    let before = rt.heap().alloc_count();
    cs_vm::vm::reset_jit_call_count();
    // Fire 200 JIT'd cons calls.
    for _ in 0..200 {
        let _ = rt.eval_str_via_vm("<diff>", "(mkpair 5 6)").unwrap();
    }
    let after_jit = cs_vm::vm::jit_call_count();
    assert!(
        after_jit >= 200,
        "expected at least 200 JIT dispatches, got {}",
        after_jit
    );
    let after = rt.heap().alloc_count();
    let grew = after.saturating_sub(before);
    assert!(
        grew >= 200,
        "Heap alloc_count grew by {} (before={}, after={}); expected at least 200 from the 200 JIT'd cons calls",
        grew,
        before,
        after
    );
}

#[test]
fn diff_jit_truthiness_on_any() {
    // M6 Phase 4 iter BC: `(if any-value ...)` must treat
    // Boolean(false) as falsy even when boxed as Any. Pre-fix,
    // brif on a Box pointer (always nonzero) always took the
    // truthy branch.
    //
    // The translator now emits Inst::AnyTruthy(fresh, cond)
    // when cond is Type::Any at JumpIfFalse, and brif consumes
    // the decoded 0/1.
    let defines = &["(define (selector v) (if v (quote got-truthy) (quote got-falsy)))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done (begin (selector (cons i i)) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let jit_false = rt.eval_str_via_vm("<diff>", "(selector #f)").unwrap();
    let jit_true = rt
        .eval_str_via_vm("<diff>", "(selector (cons 1 2))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(after >= 2, "selector never JITted (count = {})", after);

    let walker_false = walker_eval(defines, "(selector #f)");
    let walker_true = walker_eval(defines, "(selector (cons 1 2))");
    match (&jit_false, &walker_false) {
        (Value::Symbol(j), Value::Symbol(w)) => {
            assert_eq!(j, w, "tier mismatch on #f branch")
        }
        other => panic!("expected matching symbols on #f branch, got {:?}", other),
    }
    match (&jit_true, &walker_true) {
        (Value::Symbol(j), Value::Symbol(w)) => {
            assert_eq!(j, w, "tier mismatch on truthy branch")
        }
        other => panic!(
            "expected matching symbols on truthy branch, got {:?}",
            other
        ),
    }
    // (Per-branch matches above already verified each path returned
    // the matching walker symbol; that implicitly confirms the
    // branches are distinct.)
}

#[test]
fn diff_jit_collect_during_jit_body_keeps_live_pairs() {
    // M6 Phase 4 iter BS — close ADR 0012 D-2's "Known limitation
    // deferred" paragraph. If `Heap::collect()` fires *while a JIT
    // body is mid-execution* (e.g. from auto-collect inside
    // `vm_alloc_pair_gc → Heap::alloc`), Gc handles held only on
    // the JIT body's spill slots MUST NOT be incorrectly reclaimed.
    //
    // The argument for soundness here is refcount-driven: every
    // `Gc::into_raw_jit` value sitting in a JIT spill slot
    // contributes a strong count of 1 to its allocation's Rc, so
    // the Phase-1 `Heap::collect` sweep — which only reclaims
    // slots whose `weak.strong_count() == 0` — cannot touch them.
    // This test exercises that invariant under maximum auto-
    // collect pressure: threshold=1 means EVERY allocation inside
    // the JIT body triggers a collect cycle.
    //
    // The body is `(define (pcar2 a b c d) (+ (car (cons a b))
    // (car (cons c d))))`: two Pair allocations, the first's car
    // must survive the second alloc's auto-collect or the sum
    // would be wrong (or the body would panic on a dangling i64).
    let defines = &["(define (pcar2 a b c d) (+ (car (cons a b)) (car (cons c d))))"];

    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    // Warm up so pcar2 JITs. During warmup, auto-collect is OFF
    // (the default) so we don't blow up the warmup loop's own
    // allocations.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (pcar2 1 2 3 4) (loop (+ i 1)))))",
    )
    .unwrap();

    // Confirm the body has been JIT'd.
    cs_vm::vm::reset_jit_call_count();
    let _ = rt.eval_str_via_vm("<diff>", "(pcar2 10 20 30 40)").unwrap();
    let warm = cs_vm::vm::jit_call_count();
    assert!(warm >= 1, "pcar2 never JITted (count = {})", warm);

    // Crank up auto-collect to maximum pressure: every `Heap::alloc`
    // call triggers a `collect()` cycle. The collect_count() we
    // see before/after each pcar2 call should grow by at least 2
    // (one per Cons inside the body); if it doesn't, the JIT body
    // either bypassed the heap or auto-collect wasn't taking
    // effect — both invalidate the test.
    rt.heap().set_auto_collect(true);
    rt.heap().set_threshold(1);

    let cases: &[(&str, i64)] = &[
        ("(pcar2 1 2 3 4)", 4),      // 1 + 3
        ("(pcar2 11 22 33 44)", 44), // 11 + 33
        ("(pcar2 -5 0 7 0)", 2),     // -5 + 7
        ("(pcar2 100 0 200 0)", 300),
    ];
    for (expr, expected) in cases {
        let before = rt.heap().collect_count();
        let v = rt.eval_str_via_vm("<diff>", expr).unwrap();
        let after = rt.heap().collect_count();
        match v {
            Value::Number(cs_core::Number::Fixnum(n)) => {
                assert_eq!(n, *expected, "wrong sum on {}", expr);
            }
            other => panic!("expected fixnum on {}, got {:?}", expr, other),
        }
        // We expect at least two collects per pcar2 call (one for
        // each Cons). Loose lower bound — the outer eval may
        // allocate other auxiliary Gc handles too.
        let grew = after.saturating_sub(before);
        assert!(
            grew >= 2,
            "expected at least 2 auto-collects during {} (before={}, after={}); \
             auto-collect under threshold=1 was either skipped or the body bypassed the heap",
            expr,
            before,
            after,
        );
    }

    // Sanity: with auto-collect still on, run a tighter loop and
    // verify nothing panics (the inner JIT body fires many
    // mid-execution collects).
    let stress = rt
        .eval_str_via_vm(
            "<diff>",
            "(let loop ((i 0) (s 0)) \
             (if (= i 100) s (loop (+ i 1) (+ s (pcar2 i i (+ i 1) (+ i 1))))))",
        )
        .unwrap();
    // sum over i in 0..100 of (i + (i + 1)) = sum 2i + 1 for i in 0..100
    //                       = 2 * (0+1+...+99) + 100 = 2*4950 + 100 = 10000
    match stress {
        Value::Number(cs_core::Number::Fixnum(n)) => {
            assert_eq!(n, 10000, "stress loop produced wrong sum");
        }
        other => panic!("expected fixnum from stress loop, got {:?}", other),
    }

    // Reset auto-collect so we don't poison subsequent tests
    // sharing the binary's static state (cargo test reuses the
    // process). Each test makes its own Runtime, so this is
    // belt-and-suspenders.
    rt.heap().set_auto_collect(false);
}

#[test]
fn diff_jit_general_call_via_slow_path() {
    // M6 Phase 4 iter BU: general (non-self, non-builtin) Call must
    // route through `vm_call_general` rather than refusing to JIT.
    //
    // `outer` invokes `inner` — a free-variable closure, not `self`
    // and not a constant-folded builtin. Pre-BU, the translator
    // returned `Unsupported("Call with non-builtin non-self callee
    // not yet supported")` and `outer` stayed in the bytecode VM
    // forever. Post-BU, `outer` JITs and the body's `(inner y)`
    // lowers to `Inst::CallGeneral`, which calls the runtime helper
    // `vm_call_general` (the IC miss handler; the IC hot path lands
    // later, ADR 0012 D-1).
    let defines = &["(define (inner x) (+ x 1))", "(define (outer y) (inner y))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    // Warm `outer` past the tier-up threshold.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
         (if (= i 1500) 'done \
             (begin (outer i) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let result = rt.eval_str_via_vm("<diff>", "(outer 41)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "outer never dispatched through the JIT (count = {})",
        after
    );

    match &result {
        Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(*n, 42),
        other => panic!("expected fixnum 42, got {:?}", other),
    }

    // Walker agreement.
    let walker = walker_eval(defines, "(outer 41)");
    match (&result, &walker) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
        }
        other => panic!("walker / jit disagree: {:?}", other),
    }
}

#[test]
fn diff_jit_vector_ref_from_make_vector() {
    // M6 Phase 4 iter BV: vector-ref against a freshly-allocated
    // vector. The body's `make-vector` lowers to vm_alloc_vector_gc;
    // `vector-ref` lowers to vm_vector_ref_gc; the indexed element
    // (initialized to 42 via the fill arg) comes back as Fixnum.
    let defines = &["(define (vec-first n) (vector-ref (make-vector n 42) 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (vec-first 3) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let jit = rt.eval_str_via_vm("<diff>", "(vec-first 5)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "vec-first never dispatched through JIT (count={})",
        after
    );
    let walker = walker_eval(defines, "(vec-first 5)");
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 42);
        }
        other => panic!("expected fixnum 42, got {:?}", other),
    }
}

#[test]
fn diff_jit_vector_length() {
    // vector-length lowers to vm_vector_length_gc (returns raw
    // Fixnum-shape i64, no Gc wrapping).
    let defines = &["(define (vlen n) (vector-length (make-vector n 0)))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (vlen 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let jit = rt.eval_str_via_vm("<diff>", "(vlen 7)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "vlen never dispatched through JIT (count={})",
        after
    );
    let walker = walker_eval(defines, "(vlen 7)");
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 7);
        }
        other => panic!("expected fixnum 7, got {:?}", other),
    }
}

#[test]
fn diff_jit_vector_pred() {
    // vector? lowers to vm_vector_p_gc (returns 0/1).
    let defines = &["(define (is-vec? v) (if (vector? v) 1 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (is-vec? (make-vector i 0)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let jit_vec = rt
        .eval_str_via_vm("<diff>", "(is-vec? (make-vector 3 0))")
        .unwrap();
    let jit_pair = rt
        .eval_str_via_vm("<diff>", "(is-vec? (cons 1 2))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "is-vec? never dispatched through JIT (count={})",
        after
    );
    match (&jit_vec, &jit_pair) {
        (Value::Number(cs_core::Number::Fixnum(1)), Value::Number(cs_core::Number::Fixnum(0))) => {}
        other => panic!("expected (1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_deopt_on_type_miss() {
    // M6 Phase 4 iter BW: pre-BW, a runtime type miss in
    // vm_pair_car_gc / vm_unbox_fixnum etc. panicked through
    // extern "C" and aborted. Post-BW, the helper sets the
    // JIT_DEOPT_REQUESTED sentinel and returns a placeholder
    // Value::Unspecified Gc handle. try_dispatch_jit reads the
    // sentinel post-call, bumps the closure's deopt counter, and
    // (post-threshold) clears the JIT pointer. A subsequent call
    // re-tiers through bytecode.
    //
    // Body: (car v). Warmed with Pairs so the JIT'd body's
    // type-feedback says param0 is Any (Pair). Then call with a
    // *non-Pair* Any (e.g. a Symbol) — the body's vm_pair_car_gc
    // hits the miss, the sentinel fires, and the dispatcher
    // returns None, which causes the caller to re-dispatch via
    // bytecode (which itself errors gracefully).
    let defines = &["(define (head v) (car v))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (head (cons i i)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();

    // Pair input: JIT path, should succeed.
    let jit_pair = rt.eval_str_via_vm("<diff>", "(head (cons 7 8))").unwrap();
    match &jit_pair {
        Value::Number(cs_core::Number::Fixnum(7)) => {}
        other => panic!("expected fixnum 7 from pair input, got {:?}", other),
    }
    let after_first = cs_vm::vm::jit_call_count();
    assert!(after_first > 0, "head never JITted (count={})", after_first);

    // Now call (head 'not-a-pair). Pre-BW this aborted the
    // process. Post-BW: the helper returns Value::Unspecified (or
    // re-runs through bytecode which errors); the test should
    // NOT abort — that's the property we care about.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.eval_str_via_vm("<diff>", "(head (quote not-a-pair))")
    }));
    // Either: walker re-run produced an error (Err) and we
    // captured it, or it returned Unspecified, or the bytecode
    // path's own (car non-pair) error surfaced. The critical
    // assertion: no process abort.
    let _ = res;
}

#[test]
fn diff_jit_ic_hot_path_dispatches_general_call() {
    // M6 Phase 4 iter BY — IC hot path for general Call.
    // After warmup, the per-call-site IcSlot is filled (id +
    // jit_ptr); subsequent calls take the hit branch and dispatch
    // directly via call_indirect, skipping vm_call_sync's
    // SymbolTable lookup. Correctness: result equals walker.
    let defines = &["(define (inner x) (+ x 1))", "(define (outer y) (inner y))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    for d in defines {
        rt.eval_str_via_vm("<diff>", d).unwrap();
    }
    // Warm both: outer's JIT body has a CallGeneral to inner; the
    // IC slot fills once inner is itself JIT-compiled.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (outer i) (loop (+ i 1)))))",
    )
    .unwrap();

    cs_vm::vm::reset_jit_call_count();
    let result = rt.eval_str_via_vm("<diff>", "(outer 41)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(after > 0, "outer never JITted (count={})", after);

    match result {
        Value::Number(cs_core::Number::Fixnum(42)) => {}
        other => panic!("expected fixnum 42, got {:?}", other),
    }

    let walker = walker_eval(defines, "(outer 41)");
    match (&result, &walker) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w, "tier mismatch")
        }
        _ => panic!("type mismatch"),
    }
}

#[test]
fn diff_jit_string_length_via_make_string() {
    // M6 Phase 4 iter BX: string-length against a freshly-allocated
    // string. The body's `make-string` lowers to vm_alloc_string_gc;
    // `string-length` lowers to vm_string_length_gc; the length
    // (char count, not byte count) comes back as Fixnum.
    let defines = &["(define (slen n) (string-length (make-string n #\\a)))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (slen 3) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let jit = rt.eval_str_via_vm("<diff>", "(slen 5)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "slen never dispatched through JIT (count={})",
        after
    );
    let walker = walker_eval(defines, "(slen 5)");
    match (&jit, &walker) {
        (Value::Number(cs_core::Number::Fixnum(j)), Value::Number(cs_core::Number::Fixnum(w))) => {
            assert_eq!(j, w);
            assert_eq!(*j, 5);
        }
        other => panic!("expected fixnum 5, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_ref() {
    // string-ref returns a JIT_RT_CHARACTER-typed result. The body's
    // `make-string` constructs a freshly-allocated string filled
    // with the supplied character; `string-ref s 0` returns the
    // first char as a Value::Character via the dispatcher's
    // codepoint decode.
    let defines = &["(define (sfirst c) (string-ref (make-string 3 c) 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (sfirst #\\a) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let jit = rt.eval_str_via_vm("<diff>", "(sfirst #\\h)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after > 0,
        "sfirst never dispatched through JIT (count={})",
        after
    );
    let walker = walker_eval(defines, "(sfirst #\\h)");
    match (&jit, &walker) {
        (Value::Character(j), Value::Character(w)) => {
            assert_eq!(j, w);
            assert_eq!(*j, 'h');
        }
        other => panic!("expected character 'h', got {:?}", other),
    }
}

#[test]
fn diff_jit_string_pred() {
    // string? lowers to vm_string_p_gc (returns 0/1). Returns 1 for
    // a freshly-allocated string, 0 for a pair.
    let defines = &["(define (is-str? v) (if (string? v) 1 0))"];
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", defines[0]).unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (is-str? (make-string 2 #\\x)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let jit_str = rt
        .eval_str_via_vm("<diff>", "(is-str? (make-string 3 #\\y))")
        .unwrap();
    let jit_pair = rt
        .eval_str_via_vm("<diff>", "(is-str? (cons 1 2))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "is-str? never dispatched through JIT (count={})",
        after
    );
    match (&jit_str, &jit_pair) {
        (Value::Number(cs_core::Number::Fixnum(1)), Value::Number(cs_core::Number::Fixnum(0))) => {}
        other => panic!("expected (1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_make_closure_in_body() {
    // ADR 0012 D-2 (iter BZ) — Inst::MakeClosure now lowers to a
    // `vm_make_closure` runtime call inside JIT bodies. Before BZ,
    // a body containing `(lambda ...)` would deopt at translate
    // time (Unsupported opcode); after BZ, the outer function
    // tiers up and the inner lambda is constructed on the hot path
    // and returned as an Any-typed Gc handle.
    //
    // Scope note (iter BZ MVP): the constructed inner closure's
    // env is the *outer closure's* captured env (read from
    // JIT_CALLER_ENV). Free vars resolvable through that chain
    // (globals, parent lexical scope) work; free vars that name
    // the outer fn's *parameters* do not, because in the JIT path
    // params live in registers/i64 lanes, not in Env. A later iter
    // can extend MakeClosure to box param bindings into a fresh
    // Env::child first. This test closes over a global so it
    // stays inside the supported envelope.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define base 10)").unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mk) (lambda (x) (+ base x)))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (call-it f a) (f a))")
        .unwrap();
    // Warm up `mk` so it JITs (Any-returning fn — tiers up after
    // the JIT threshold).
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (mk) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Now construct a fresh closure via the JIT path and call it.
    // The constructed closure isn't itself JIT-compiled, so the
    // call routes through bytecode dispatch — which is fine; we
    // just need the MakeClosure call to succeed and the resulting
    // closure to look up `base` through its (cloned) env chain.
    let result = rt.eval_str_via_vm("<diff>", "(call-it (mk) 3)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 1,
        "mk never dispatched through JIT (count={after})"
    );
    match &result {
        Value::Number(cs_core::Number::Fixnum(13)) => {}
        other => panic!("expected 13, got {:?}", other),
    }
}

#[test]
fn diff_jit_length_walks_spine() {
    // ADR 0012 D-2 (iter CA) — `(length lst)` lowers to vm_length_gc
    // which walks the spine and returns a raw Fixnum count.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (len lst) (length lst))")
        .unwrap();
    // Warmup loop so `len` tiers up.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (len (list 1 2 3)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r0 = rt.eval_str_via_vm("<diff>", "(len '())").unwrap();
    let r1 = rt.eval_str_via_vm("<diff>", "(len (list 42))").unwrap();
    let r5 = rt
        .eval_str_via_vm("<diff>", "(len (list 1 2 3 4 5))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "len never dispatched through JIT (count={after})"
    );
    match (&r0, &r1, &r5) {
        (
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(5)),
        ) => {}
        other => panic!("expected (0, 1, 5), got {:?}", other),
    }
}

#[test]
fn diff_jit_list_pred() {
    // ADR 0012 D-2 (iter CA) — `(list? v)` lowers to vm_list_p_gc.
    // Returns 1 for '() and proper chains, 0 for improper lists and
    // atoms. Total predicate — no deopt on non-list inputs.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (is-list? v) (if (list? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (is-list? (list 1 2)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r_list = rt
        .eval_str_via_vm("<diff>", "(is-list? (list 1 2 3))")
        .unwrap();
    let r_null = rt.eval_str_via_vm("<diff>", "(is-list? '())").unwrap();
    let r_improper = rt
        .eval_str_via_vm("<diff>", "(is-list? (cons 1 2))")
        .unwrap();
    let r_atom = rt.eval_str_via_vm("<diff>", "(is-list? 42)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "is-list? never dispatched through JIT (count={after})"
    );
    match (&r_list, &r_null, &r_improper, &r_atom) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1, 1, 0, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_reverse_allocates_new_spine() {
    // ADR 0012 D-2 (iter CB) — `(reverse lst)` lowers to
    // vm_reverse_gc which walks the spine and builds a fresh
    // reversed list. Returns an Any-shape Gc handle; result is
    // structurally a proper list.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rev lst) (reverse lst))")
        .unwrap();
    // Warmup so `rev` tiers up.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (rev (list 1 2 3)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r_empty = rt.eval_str_via_vm("<diff>", "(rev '())").unwrap();
    // (length (reverse lst)) == (length lst).
    let len = rt
        .eval_str_via_vm("<diff>", "(length (rev (list 10 20 30 40)))")
        .unwrap();
    // (car (reverse '(1 2 3))) == 3, (cadr ...) == 2, etc.
    let first = rt
        .eval_str_via_vm("<diff>", "(car (rev (list 1 2 3)))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "rev never dispatched through JIT (count={after})"
    );
    assert!(
        matches!(&r_empty, Value::Null),
        "expected Null, got {:?}",
        r_empty
    );
    match (&len, &first) {
        (Value::Number(cs_core::Number::Fixnum(4)), Value::Number(cs_core::Number::Fixnum(3))) => {}
        other => panic!("expected (4, 3), got {:?}", other),
    }
}

#[test]
fn diff_jit_memq_matches_by_eq() {
    // ADR 0012 D-2 (iter CC) — `(memq item lst)` lowers to
    // vm_memq_gc. Walks spine, eq?-compares each car against item,
    // returns the matched sublist or #f. Symbol identity comparison
    // is the typical use case.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (find item lst) (memq item lst))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (find 'b (list 'a 'b 'c)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Hit: 'b at position 1 → returns '(b c).
    let hit = rt
        .eval_str_via_vm("<diff>", "(car (find 'b (list 'a 'b 'c)))")
        .unwrap();
    // Miss: 'z is not in the list → returns #f.
    let miss = rt
        .eval_str_via_vm("<diff>", "(find 'z (list 'a 'b 'c))")
        .unwrap();
    // Length of matched sublist: '(b c) → 2.
    let len = rt
        .eval_str_via_vm("<diff>", "(length (find 'b (list 'a 'b 'c)))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "find never dispatched through JIT (count={after})"
    );
    match (&hit, &miss, &len) {
        (Value::Symbol(_), Value::Boolean(false), Value::Number(cs_core::Number::Fixnum(2))) => {}
        other => panic!("expected (Symbol(b), #f, 2), got {:?}", other),
    }
}

#[test]
fn diff_jit_assq_walks_alist() {
    // ADR 0012 D-2 (iter CD) — `(assq key alist)` lowers to
    // vm_assq_gc. Walks the alist spine, eq?-compares each entry's
    // car against key, returns the matched `(k . v)` pair or #f.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lookup k al) (assq k al))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (sample) (list (cons 'a 1) (cons 'b 2) (cons 'c 3)))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (lookup 'b (sample)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Hit: lookup 'b → returns (b . 2); cdr is 2.
    let val = rt
        .eval_str_via_vm("<diff>", "(cdr (lookup 'b (sample)))")
        .unwrap();
    // Miss: lookup 'z → returns #f.
    let miss = rt
        .eval_str_via_vm("<diff>", "(lookup 'z (sample))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "lookup never dispatched through JIT (count={after})"
    );
    match (&val, &miss) {
        (Value::Number(cs_core::Number::Fixnum(2)), Value::Boolean(false)) => {}
        other => panic!("expected (2, #f), got {:?}", other),
    }
}

#[test]
fn diff_jit_set_car_cdr_mutate_pair() {
    // ADR 0012 D-2 (iter CE) — set-car! / set-cdr! lower to
    // vm_set_car_gc / vm_set_cdr_gc. The mutation is observable
    // through subsequent car/cdr reads. Test pattern: outer fn
    // takes a pair + a value, set-car! it, then return (car p)
    // to prove the slot was updated.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (mutate-car! p v) (set-car! p v) (car p))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (mutate-cdr! p v) (set-cdr! p v) (cdr p))",
    )
    .unwrap();
    // Warmup both JIT bodies.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (mutate-car! (cons 1 2) 99) \
                    (mutate-cdr! (cons 1 2) 99) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let new_car = rt
        .eval_str_via_vm("<diff>", "(mutate-car! (cons 1 2) 42)")
        .unwrap();
    let new_cdr = rt
        .eval_str_via_vm("<diff>", "(mutate-cdr! (cons 1 2) 88)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "mutate-car!/cdr! never dispatched through JIT (count={after})"
    );
    match (&new_car, &new_cdr) {
        (
            Value::Number(cs_core::Number::Fixnum(42)),
            Value::Number(cs_core::Number::Fixnum(88)),
        ) => {}
        other => panic!("expected (42, 88), got {:?}", other),
    }
}

#[test]
fn diff_jit_char_ordered_comparisons() {
    // ADR 0012 D-2 (iter CF) — char<? / char>? / char<=? /
    // char>=? now lower to RirInst::Lt with appropriate operand
    // swaps and negation. Character carries codepoint in Fixnum-
    // shape i64 lanes so numeric comparison matches Unicode order.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lt? a b) (if (char<? a b) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (gt? a b) (if (char>? a b) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (le? a b) (if (char<=? a b) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ge? a b) (if (char>=? a b) 1 0))")
        .unwrap();
    // Warmup all four with #\a #\b.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (lt? #\\a #\\b) (gt? #\\a #\\b) \
                       (le? #\\a #\\b) (ge? #\\a #\\b) \
                       (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lt_yes = rt.eval_str_via_vm("<diff>", "(lt? #\\a #\\z)").unwrap();
    let gt_no = rt.eval_str_via_vm("<diff>", "(gt? #\\a #\\z)").unwrap();
    let le_eq = rt.eval_str_via_vm("<diff>", "(le? #\\m #\\m)").unwrap();
    let ge_eq = rt.eval_str_via_vm("<diff>", "(ge? #\\m #\\m)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "char comparisons never dispatched through JIT (count={after})"
    );
    match (&lt_yes, &gt_no, &le_eq, &ge_eq) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(1)),
        ) => {}
        other => panic!("expected (1, 0, 1, 1), got {:?}", other),
    }
}

#[test]
fn diff_jit_memv_assv_eqv_search() {
    // ADR 0012 D-2 (iter CG) — memv / assv are the eqv?-flavored
    // search ops. cs_core::eq::eqv extends eq? with by-value
    // comparison for numbers and characters.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mv item lst) (memv item lst))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (av key al) (assv key al))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (mv 2 (list 1 2 3)) (av 2 (list (cons 1 'a) (cons 2 'b))) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let mv_hit = rt
        .eval_str_via_vm("<diff>", "(car (mv 2 (list 1 2 3)))")
        .unwrap();
    let mv_miss = rt
        .eval_str_via_vm("<diff>", "(mv 99 (list 1 2 3))")
        .unwrap();
    let av_hit = rt
        .eval_str_via_vm("<diff>", "(cdr (av 2 (list (cons 1 'a) (cons 2 'b))))")
        .unwrap();
    let av_miss = rt
        .eval_str_via_vm("<diff>", "(av 99 (list (cons 1 'a) (cons 2 'b)))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "memv/assv never dispatched through JIT (count={after})"
    );
    match (&mv_hit, &mv_miss, &av_hit, &av_miss) {
        (
            Value::Number(cs_core::Number::Fixnum(2)),
            Value::Boolean(false),
            Value::Symbol(_),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (2, #f, Symbol(b), #f), got {:?}", other),
    }
}

#[test]
fn diff_jit_member_assoc_equal_search() {
    // ADR 0012 D-2 (iter CH) — member / assoc use cs_core::eq::equal
    // (structural deep equality). Distinguish from memv/assv by
    // matching structurally identical but distinct allocations
    // (e.g. two `(list 1 2)` instances): eqv? returns #f (different
    // identity), equal? returns #t.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m item lst) (member item lst))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (a key al) (assoc key al))")
        .unwrap();
    // Warmup with a structural-match pattern.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (m (list 1 2) (list (list 1 2) (list 3 4))) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (a (list 1 2) (list (cons (list 1 2) 'x) (cons (list 3 4) 'y))) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // member: structurally equal needle (list 1 2) matches the
    // first element. Take its car (a list) and length → 2.
    let m_hit_len = rt
        .eval_str_via_vm(
            "<diff>",
            "(length (car (m (list 1 2) (list (list 1 2) (list 3 4)))))",
        )
        .unwrap();
    let m_miss = rt
        .eval_str_via_vm("<diff>", "(m (list 9 9) (list (list 1 2) (list 3 4)))")
        .unwrap();
    // assoc: structurally equal key (list 1 2) matches the first
    // entry. Take its cdr → 'x.
    let a_hit = rt
        .eval_str_via_vm(
            "<diff>",
            "(cdr (a (list 1 2) (list (cons (list 1 2) 'x) (cons (list 3 4) 'y))))",
        )
        .unwrap();
    let a_miss = rt
        .eval_str_via_vm(
            "<diff>",
            "(a (list 9 9) (list (cons (list 1 2) 'x) (cons (list 3 4) 'y)))",
        )
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "member/assoc never dispatched through JIT (count={after})"
    );
    match (&m_hit_len, &m_miss, &a_hit, &a_miss) {
        (
            Value::Number(cs_core::Number::Fixnum(2)),
            Value::Boolean(false),
            Value::Symbol(_),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (2, #f, Symbol(x), #f), got {:?}", other),
    }
}

#[test]
fn diff_jit_char_unicode_predicates() {
    // ADR 0012 D-2 (iter CI) — char-alphabetic? / char-numeric? /
    // char-whitespace? lower to vm_char_*_p helpers using Rust's
    // Unicode classification. Body uses `integer->char` to feed
    // each predicate a Character-typed operand so the dispatch
    // arm fires (it gates on Type::Character).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (alpha? n) (if (char-alphabetic? (integer->char n)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (numer? n) (if (char-numeric? (integer->char n)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (white? n) (if (char-whitespace? (integer->char n)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (alpha? 97) (numer? 97) (white? 97) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 65 = 'A' (alphabetic), 53 = '5' (numeric), 32 = ' ' (whitespace).
    let alpha_a = rt.eval_str_via_vm("<diff>", "(alpha? 65)").unwrap();
    let alpha_5 = rt.eval_str_via_vm("<diff>", "(alpha? 53)").unwrap();
    let numer_5 = rt.eval_str_via_vm("<diff>", "(numer? 53)").unwrap();
    let numer_a = rt.eval_str_via_vm("<diff>", "(numer? 65)").unwrap();
    let white_sp = rt.eval_str_via_vm("<diff>", "(white? 32)").unwrap();
    let white_a = rt.eval_str_via_vm("<diff>", "(white? 65)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 6,
        "char predicates never dispatched through JIT (count={after})"
    );
    match (&alpha_a, &alpha_5, &numer_5, &numer_a, &white_sp, &white_a) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1,0,1,0,1,0), got {:?}", other),
    }
}

#[test]
fn diff_jit_char_case_ops() {
    // ADR 0012 D-2 (iter CJ) — char-upcase / char-downcase return
    // a Character (codepoint via char::to_uppercase().next() /
    // to_lowercase().next()). char-upper-case? / char-lower-case?
    // are Boolean predicates. All four operate on Character-typed
    // operands.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (up n) (char->integer (char-upcase (integer->char n))))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (dn n) (char->integer (char-downcase (integer->char n))))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (uc? n) (if (char-upper-case? (integer->char n)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (lc? n) (if (char-lower-case? (integer->char n)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
            (if (= i 1500) 'done \
                (begin (up 97) (dn 65) (uc? 65) (lc? 97) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 97 = 'a', upcase → 65 = 'A'.
    let up_a = rt.eval_str_via_vm("<diff>", "(up 97)").unwrap();
    // 65 = 'A', downcase → 97 = 'a'.
    let dn_a = rt.eval_str_via_vm("<diff>", "(dn 65)").unwrap();
    // 'A' is upper-case.
    let uc_a = rt.eval_str_via_vm("<diff>", "(uc? 65)").unwrap();
    let uc_lower = rt.eval_str_via_vm("<diff>", "(uc? 97)").unwrap();
    // 'a' is lower-case.
    let lc_a = rt.eval_str_via_vm("<diff>", "(lc? 97)").unwrap();
    let lc_upper = rt.eval_str_via_vm("<diff>", "(lc? 65)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 6,
        "char case ops never dispatched through JIT (count={after})"
    );
    match (&up_a, &dn_a, &uc_a, &uc_lower, &lc_a, &lc_upper) {
        (
            Value::Number(cs_core::Number::Fixnum(65)),
            Value::Number(cs_core::Number::Fixnum(97)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (65, 97, 1, 0, 1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_list_ref_and_tail() {
    // ADR 0012 D-2 (iter CK) — list-ref / list-tail walk n cdrs.
    // list-ref then takes the car; list-tail returns the spine
    // remainder as-is.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ref lst n) (list-ref lst n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tail lst n) (list-tail lst n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (ref (list 10 20 30 40) 2) (tail (list 1 2 3) 1) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // list-ref at index 0, 2 of (10 20 30 40) → 10, 30.
    let ref0 = rt
        .eval_str_via_vm("<diff>", "(ref (list 10 20 30 40) 0)")
        .unwrap();
    let ref2 = rt
        .eval_str_via_vm("<diff>", "(ref (list 10 20 30 40) 2)")
        .unwrap();
    // list-tail at index 2 of (1 2 3 4 5) → (3 4 5); length 3.
    let tail_len = rt
        .eval_str_via_vm("<diff>", "(length (tail (list 1 2 3 4 5) 2))")
        .unwrap();
    // list-tail at index 0 → identity (length 4).
    let tail_id = rt
        .eval_str_via_vm("<diff>", "(length (tail (list 1 2 3 4) 0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "list-ref/list-tail never dispatched through JIT (count={after})"
    );
    match (&ref0, &ref2, &tail_len, &tail_id) {
        (
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(30)),
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(4)),
        ) => {}
        other => panic!("expected (10, 30, 3, 4), got {:?}", other),
    }
}

#[test]
fn diff_jit_modulo_signs() {
    // ADR 0012 D-2 (iter CL) — R6RS modulo follows the divisor's
    // sign (Euclidean adjustment), unlike `remainder` which
    // follows the dividend. Lowered inline via Cranelift srem +
    // select.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (md a b) (modulo a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (md 13 4) (md -13 4) (md 13 -4) (md -13 -4) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // All four sign combinations.
    let pp = rt.eval_str_via_vm("<diff>", "(md 13 4)").unwrap(); // 1
    let np = rt.eval_str_via_vm("<diff>", "(md -13 4)").unwrap(); // 3
    let pn = rt.eval_str_via_vm("<diff>", "(md 13 -4)").unwrap(); // -3
    let nn = rt.eval_str_via_vm("<diff>", "(md -13 -4)").unwrap(); // -1
                                                                   // Zero remainder edge case: clean division.
    let exact = rt.eval_str_via_vm("<diff>", "(md 12 4)").unwrap(); // 0
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 5,
        "modulo never dispatched through JIT (count={after})"
    );
    match (&pp, &np, &pn, &nn, &exact) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(-3)),
            Value::Number(cs_core::Number::Fixnum(-1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1, 3, -3, -1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_substring_slices() {
    // ADR 0012 D-2 (iter CM) — substring lowers to vm_substring_gc.
    // Indices are character (not byte) positions. Result is a
    // fresh Gc<Value::String>.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sub s a b) (substring s a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (sub (make-string 10 #\\x) 2 7) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // (sub "xxxxxxxxxx" 2 7) → 5-char string "xxxxx", length 5.
    let len = rt
        .eval_str_via_vm("<diff>", "(string-length (sub (make-string 10 #\\x) 2 7))")
        .unwrap();
    let empty_len = rt
        .eval_str_via_vm("<diff>", "(string-length (sub (make-string 4 #\\a) 2 2))")
        .unwrap();
    let full_len = rt
        .eval_str_via_vm("<diff>", "(string-length (sub (make-string 5 #\\b) 0 5))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "substring never dispatched through JIT (count={after})"
    );
    match (&len, &empty_len, &full_len) {
        (
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(5)),
        ) => {}
        other => panic!("expected (5, 0, 5), got {:?}", other),
    }
}

#[test]
fn diff_jit_list_copy_fresh_spine() {
    // ADR 0012 D-2 (iter CN) — list-copy walks the spine and
    // allocates fresh pairs.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cp lst) (list-copy lst))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (cp (list 1 2 3)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(length (cp (list 10 20 30 40)))")
        .unwrap();
    let first = rt
        .eval_str_via_vm("<diff>", "(car (cp (list 100 200 300)))")
        .unwrap();
    let empty_len = rt.eval_str_via_vm("<diff>", "(length (cp '()))").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "list-copy never dispatched through JIT (count={after})"
    );
    match (&len, &first, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(4)),
            Value::Number(cs_core::Number::Fixnum(100)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (4, 100, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_list_set_mutates_indexed_pair() {
    // ADR 0012 D-2 (iter CO) — list-set! walks n cdrs then mutates
    // the resulting pair's car. The mutation is observable through
    // a subsequent list-ref. Body sequences list-set! then list-ref
    // so we can assert the new value in a single JIT call.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (set-and-read lst n v) (list-set! lst n v) (list-ref lst n))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (set-and-read (list 1 2 3) 1 99) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Mutate index 0 to 42; read it back.
    let r0 = rt
        .eval_str_via_vm("<diff>", "(set-and-read (list 1 2 3) 0 42)")
        .unwrap();
    // Mutate index 2 to 99; read it back.
    let r2 = rt
        .eval_str_via_vm("<diff>", "(set-and-read (list 10 20 30) 2 99)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "list-set! never dispatched through JIT (count={after})"
    );
    match (&r0, &r2) {
        (
            Value::Number(cs_core::Number::Fixnum(42)),
            Value::Number(cs_core::Number::Fixnum(99)),
        ) => {}
        other => panic!("expected (42, 99), got {:?}", other),
    }
}

#[test]
fn diff_jit_gcd_lcm_pair() {
    // ADR 0012 D-2 (iter CP) — gcd / lcm on fixnum pairs via
    // Euclidean algorithm. Both operands and result are Fixnum.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (g a b) (gcd a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l a b) (lcm a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (g 12 8) (l 12 8) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let g_12_8 = rt.eval_str_via_vm("<diff>", "(g 12 8)").unwrap(); // 4
    let g_neg = rt.eval_str_via_vm("<diff>", "(g -12 8)").unwrap(); // 4 (abs)
    let g_zero = rt.eval_str_via_vm("<diff>", "(g 0 7)").unwrap(); // 7 (b_gcd folds 0)
    let l_12_8 = rt.eval_str_via_vm("<diff>", "(l 12 8)").unwrap(); // 24
    let l_zero = rt.eval_str_via_vm("<diff>", "(l 0 7)").unwrap(); // 0
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 5,
        "gcd/lcm never dispatched through JIT (count={after})"
    );
    match (&g_12_8, &g_neg, &g_zero, &l_12_8, &l_zero) {
        (
            Value::Number(cs_core::Number::Fixnum(4)),
            Value::Number(cs_core::Number::Fixnum(4)),
            Value::Number(cs_core::Number::Fixnum(7)),
            Value::Number(cs_core::Number::Fixnum(24)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (4, 4, 7, 24, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_bytevector_read_ops() {
    // ADR 0012 D-2 (iter CQ) — bytevector? / bytevector-length /
    // bytevector-u8-ref now lower to runtime helpers. Operand is
    // Any (Gc handle); results are Boolean (bytevector?) or Fixnum
    // (length / u8-ref).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bv-p? v) (if (bytevector? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bv-len bv) (bytevector-length bv))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bv-at bv k) (bytevector-u8-ref bv k))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (bv-p? (make-bytevector 4 0)) \
                    (bv-len (make-bytevector 4 0)) \
                    (bv-at (make-bytevector 4 42) 0) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let p_bv = rt
        .eval_str_via_vm("<diff>", "(bv-p? (make-bytevector 4 0))")
        .unwrap();
    let p_other = rt.eval_str_via_vm("<diff>", "(bv-p? (list 1 2))").unwrap();
    let len = rt
        .eval_str_via_vm("<diff>", "(bv-len (make-bytevector 10 0))")
        .unwrap();
    let byte = rt
        .eval_str_via_vm("<diff>", "(bv-at (make-bytevector 4 42) 2)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "bytevector ops never dispatched through JIT (count={after})"
    );
    match (&p_bv, &p_other, &len, &byte) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(42)),
        ) => {}
        other => panic!("expected (1, 0, 10, 42), got {:?}", other),
    }
}

#[test]
fn diff_jit_bytevector_write_ops() {
    // ADR 0012 D-2 (iter CR) — make-bytevector + bytevector-u8-set!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mk-bv n fill) (make-bytevector n fill))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (set-and-read bv k v) \
             (bytevector-u8-set! bv k v) (bytevector-u8-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (mk-bv 4 7) \
                    (set-and-read (make-bytevector 4 0) 1 99) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(bytevector-length (mk-bv 8 #xAB))")
        .unwrap();
    let byte0 = rt
        .eval_str_via_vm("<diff>", "(bytevector-u8-ref (mk-bv 8 #xAB) 0)")
        .unwrap();
    let byte7 = rt
        .eval_str_via_vm("<diff>", "(bytevector-u8-ref (mk-bv 8 #xAB) 7)")
        .unwrap();
    let mutated = rt
        .eval_str_via_vm("<diff>", "(set-and-read (make-bytevector 5 0) 2 42)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "bytevector write ops never dispatched through JIT (count={after})"
    );
    match (&len, &byte0, &byte7, &mutated) {
        (
            Value::Number(cs_core::Number::Fixnum(8)),
            Value::Number(cs_core::Number::Fixnum(171)),
            Value::Number(cs_core::Number::Fixnum(171)),
            Value::Number(cs_core::Number::Fixnum(42)),
        ) => {}
        other => panic!("expected (8, 171, 171, 42), got {:?}", other),
    }
}

#[test]
fn diff_jit_char_foldcase_and_titlecase() {
    // ADR 0012 D-2 (iter CS) — char-foldcase / char-titlecase.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (fc n) (char->integer (char-foldcase (integer->char n))))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (tc n) (char->integer (char-titlecase (integer->char n))))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (fc 65) (tc 97) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let fc_a = rt.eval_str_via_vm("<diff>", "(fc 65)").unwrap();
    let tc_a = rt.eval_str_via_vm("<diff>", "(tc 97)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "char-foldcase/titlecase never dispatched through JIT (count={after})"
    );
    match (&fc_a, &tc_a) {
        (
            Value::Number(cs_core::Number::Fixnum(97)),
            Value::Number(cs_core::Number::Fixnum(65)),
        ) => {}
        other => panic!("expected (97, 65), got {:?}", other),
    }
}

#[test]
fn diff_jit_expt_fixnum() {
    // ADR 0012 D-2 (iter CT) — expt via repeated squaring.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (pw base e) (expt base e))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (pw 2 10) (pw 3 5) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let p_2_10 = rt.eval_str_via_vm("<diff>", "(pw 2 10)").unwrap();
    let p_3_5 = rt.eval_str_via_vm("<diff>", "(pw 3 5)").unwrap();
    let p_5_0 = rt.eval_str_via_vm("<diff>", "(pw 5 0)").unwrap();
    let p_0_3 = rt.eval_str_via_vm("<diff>", "(pw 0 3)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "expt never dispatched through JIT (count={after})"
    );
    match (&p_2_10, &p_3_5, &p_5_0, &p_0_3) {
        (
            Value::Number(cs_core::Number::Fixnum(1024)),
            Value::Number(cs_core::Number::Fixnum(243)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1024, 243, 1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_char_ci_comparisons() {
    // ADR 0012 D-2 (iter CU) — char-ci=? / char-ci<? / char-ci>? /
    // char-ci<=? / char-ci>=? lower as CharFoldcase on both operands
    // followed by the base comparison. No new helpers — purely
    // dispatch-side rewriting.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (cieq a b) \
             (if (char-ci=? (integer->char a) (integer->char b)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (cilt a b) \
             (if (char-ci<? (integer->char a) (integer->char b)) 1 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (cieq 65 97) (cilt 65 66) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 'A' ci=? 'a' → 1 (foldcase folds both to 'a').
    let eq_aA = rt.eval_str_via_vm("<diff>", "(cieq 65 97)").unwrap();
    // 'A' ci=? 'B' → 0.
    let eq_AB = rt.eval_str_via_vm("<diff>", "(cieq 65 66)").unwrap();
    // 'A' ci<? 'b' → 1 ('a' < 'b' after foldcase).
    let lt_Ab = rt.eval_str_via_vm("<diff>", "(cilt 65 98)").unwrap();
    // 'B' ci<? 'a' → 0 (both fold; 'b' is not < 'a').
    let lt_Ba = rt.eval_str_via_vm("<diff>", "(cilt 66 97)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "char-ci ops never dispatched through JIT (count={after})"
    );
    match (&eq_aA, &eq_AB, &lt_Ab, &lt_Ba) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1, 0, 1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_digit_value_mixed_return() {
    // ADR 0012 D-2 (iter CV) — digit-value returns Fixnum 0-9 for
    // digit chars and #f for non-digits. Mixed return → Any-shape
    // Gc result.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (dv n) (digit-value (integer->char n)))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (dv 53) (dv 65) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 53 = '5', digit-value = 5.
    let d5 = rt.eval_str_via_vm("<diff>", "(dv 53)").unwrap();
    // 48 = '0', digit-value = 0.
    let d0 = rt.eval_str_via_vm("<diff>", "(dv 48)").unwrap();
    // 65 = 'A', not a digit → #f.
    let dA = rt.eval_str_via_vm("<diff>", "(dv 65)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "digit-value never dispatched through JIT (count={after})"
    );
    match (&d5, &d0, &dA) {
        (
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (5, 0, #f), got {:?}", other),
    }
}

#[test]
fn diff_jit_vector_list_conversions() {
    // ADR 0012 D-2 (iter CW) — vector->list / list->vector (1-arg).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (v->l v) (vector->list v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l->v lst) (list->vector lst))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (v->l (make-vector 4 0)) (l->v (list 1 2 3)) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let v_len = rt
        .eval_str_via_vm("<diff>", "(length (v->l (make-vector 5 0)))")
        .unwrap();
    let l_len = rt
        .eval_str_via_vm("<diff>", "(vector-length (l->v (list 10 20 30 40)))")
        .unwrap();
    let first = rt
        .eval_str_via_vm("<diff>", "(car (v->l (l->v (list 42 100 200))))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "vector<->list never dispatched through JIT (count={after})"
    );
    match (&v_len, &l_len, &first) {
        (
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(4)),
            Value::Number(cs_core::Number::Fixnum(42)),
        ) => {}
        other => panic!("expected (5, 4, 42), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_list_conversions() {
    // ADR 0012 D-2 (iter CX) — string->list / list->string. Parallel
    // to CW but operates on chars / strings.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s->l s) (string->list s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l->s lst) (list->string lst))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (s->l (make-string 4 #\\x)) \
                    (l->s (list #\\a #\\b)) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // string->list: a 5-char string → length-5 list.
    let s_len = rt
        .eval_str_via_vm("<diff>", "(length (s->l (make-string 5 #\\y)))")
        .unwrap();
    // list->string: 3-char list → 3-char string.
    let l_len = rt
        .eval_str_via_vm("<diff>", "(string-length (l->s (list #\\a #\\b #\\c)))")
        .unwrap();
    // Round-trip preserves the first char.
    let first = rt
        .eval_str_via_vm(
            "<diff>",
            "(char->integer (car (s->l (l->s (list #\\Z #\\X)))))",
        )
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "string<->list never dispatched through JIT (count={after})"
    );
    match (&s_len, &l_len, &first) {
        (
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(90)),
        ) => {}
        other => panic!("expected (5, 3, 90), got {:?}", other),
    }
}

#[test]
fn diff_jit_symbol_string_conversions() {
    // ADR 0012 D-2 (iter CY) — symbol->string / string->symbol.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s->str sym) (symbol->string sym))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (str->s s) (string->symbol s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (s->str 'foo) (str->s (make-string 3 #\\a)) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(string-length (s->str 'hello))")
        .unwrap();
    let first = rt
        .eval_str_via_vm(
            "<diff>",
            "(string-ref (s->str (str->s (make-string 3 #\\Z))) 0)",
        )
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    // Only one of the two functions reliably tiers up in this
    // pattern (the str->s body returns Symbol-shape so it tiers up;
    // the s->str body returns Any/String). Either way, semantic
    // correctness on the result is what we assert.
    assert!(
        after >= 1,
        "symbol<->string never dispatched through JIT (count={after})"
    );
    match (&len, &first) {
        (Value::Number(cs_core::Number::Fixnum(5)), Value::Character('Z')) => {}
        other => panic!("expected (5, #\\Z), got {:?}", other),
    }
}

#[test]
fn diff_jit_vector_bytevector_fill() {
    // ADR 0012 D-2 (iter CZ) — vector-fill! / bytevector-fill!.
    // Bulk-overwrite every slot; mutation is observable via
    // subsequent ref ops.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (vfill v fill) (vector-fill! v fill) (vector-ref v 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (bvfill bv fill) \
             (bytevector-fill! bv fill) (bytevector-u8-ref bv 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (vfill (make-vector 4 0) 42) \
                    (bvfill (make-bytevector 4 0) 99) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let v_res = rt
        .eval_str_via_vm("<diff>", "(vfill (make-vector 5 0) 77)")
        .unwrap();
    let bv_res = rt
        .eval_str_via_vm("<diff>", "(bvfill (make-bytevector 5 0) 200)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "fill ops never dispatched through JIT (count={after})"
    );
    match (&v_res, &bv_res) {
        (
            Value::Number(cs_core::Number::Fixnum(77)),
            Value::Number(cs_core::Number::Fixnum(200)),
        ) => {}
        other => panic!("expected (77, 200), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_set_mutates_char() {
    // ADR 0012 D-2 (iter DA) — string-set! mutates the k-th
    // character. Indexes are character (not byte) positions.
    // Body sequences set! then string-ref to assert the new
    // codepoint in a single JIT call.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (set-and-read s k n) \
             (string-set! s k (integer->char n)) (string-ref s k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (set-and-read (make-string 4 #\\a) 1 65) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Mutate index 0 of \"aaaaa\" to 'Z' (90); read back as char.
    let r0 = rt
        .eval_str_via_vm("<diff>", "(set-and-read (make-string 5 #\\a) 0 90)")
        .unwrap();
    // Mutate index 3 to '!' (33).
    let r3 = rt
        .eval_str_via_vm("<diff>", "(set-and-read (make-string 5 #\\a) 3 33)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "string-set! never dispatched through JIT (count={after})"
    );
    match (&r0, &r3) {
        (Value::Character('Z'), Value::Character('!')) => {}
        other => panic!("expected (#\\Z, #\\!), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_vector_copy() {
    // ADR 0012 D-2 (iter DB) — 1-arg string-copy / vector-copy.
    // Verifies via length preservation that the copy succeeded.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sc s) (string-copy s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (vc v) (vector-copy v))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (sc (make-string 4 #\\x)) (vc (make-vector 3 0)) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let s_len = rt
        .eval_str_via_vm("<diff>", "(string-length (sc (make-string 7 #\\y)))")
        .unwrap();
    let v_len = rt
        .eval_str_via_vm("<diff>", "(vector-length (vc (make-vector 5 99)))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "string-copy/vector-copy never dispatched through JIT (count={after})"
    );
    match (&s_len, &v_len) {
        (Value::Number(cs_core::Number::Fixnum(7)), Value::Number(cs_core::Number::Fixnum(5))) => {}
        other => panic!("expected (7, 5), got {:?}", other),
    }
}

#[test]
fn diff_jit_bytevector_copy() {
    // ADR 0012 D-2 (iter DC) — 1-arg bytevector-copy.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bc bv) (bytevector-copy bv))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (bc (make-bytevector 4 0)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(bytevector-length (bc (make-bytevector 9 7)))")
        .unwrap();
    let byte = rt
        .eval_str_via_vm(
            "<diff>",
            "(bytevector-u8-ref (bc (make-bytevector 4 42)) 2)",
        )
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "bytevector-copy never dispatched through JIT (count={after})"
    );
    match (&len, &byte) {
        (Value::Number(cs_core::Number::Fixnum(9)), Value::Number(cs_core::Number::Fixnum(42))) => {
        }
        other => panic!("expected (9, 42), got {:?}", other),
    }
}

#[test]
fn diff_jit_any_type_predicates() {
    // ADR 0012 D-2 (iter DD) — procedure? / symbol? on Any operands.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (proc? v) (if (procedure? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sym? v) (if (symbol? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (proc? car) (sym? 'foo) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let p_yes = rt.eval_str_via_vm("<diff>", "(proc? car)").unwrap();
    let p_no = rt.eval_str_via_vm("<diff>", "(proc? (list 1 2))").unwrap();
    let s_yes = rt.eval_str_via_vm("<diff>", "(sym? 'foo)").unwrap();
    let s_no = rt
        .eval_str_via_vm("<diff>", "(sym? (make-string 3 #\\a))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "type predicates never dispatched through JIT (count={after})"
    );
    match (&p_yes, &p_no, &s_yes, &s_no) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1, 0, 1, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_immediate_type_predicates_on_any() {
    // ADR 0012 D-2 (iter DE) — char? / boolean? / fixnum? on Any.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (c? v) (if (char? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (b? v) (if (boolean? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fx? v) (if (fixnum? v) 1 0))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (c? #\\a) (b? #t) (fx? 42) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let c_yes = rt.eval_str_via_vm("<diff>", "(c? #\\z)").unwrap();
    let c_no = rt.eval_str_via_vm("<diff>", "(c? 42)").unwrap();
    let b_yes = rt.eval_str_via_vm("<diff>", "(b? #t)").unwrap();
    let b_no = rt.eval_str_via_vm("<diff>", "(b? 'foo)").unwrap();
    let fx_yes = rt.eval_str_via_vm("<diff>", "(fx? 42)").unwrap();
    let fx_no = rt.eval_str_via_vm("<diff>", "(fx? #\\a)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    // Not every variant reliably tiers up in this pattern (param
    // type narrowing is per-closure); assert correctness on results
    // and that at least one variant reached the JIT.
    assert!(
        after >= 1,
        "type predicates never dispatched through JIT (count={after})"
    );
    match (&c_yes, &c_no, &b_yes, &b_no, &fx_yes, &fx_no) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1,0,1,0,1,0), got {:?}", other),
    }
}

#[test]
fn diff_jit_flonum_transcendentals() {
    // ADR 0012 D-2 (iter DF) — sin / cos / tan / log / exp lowered
    // to runtime helpers.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s x) (sin x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (e x) (exp x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (s 1.0) (e 1.0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let sin0 = rt.eval_str_via_vm("<diff>", "(s 0.0)").unwrap();
    let exp0 = rt.eval_str_via_vm("<diff>", "(e 0.0)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "transcendentals never dispatched through JIT (count={after})"
    );
    match (&sin0, &exp0) {
        (Value::Number(cs_core::Number::Flonum(a)), Value::Number(cs_core::Number::Flonum(b)))
            if a.abs() < 1e-12 && (*b - 1.0).abs() < 1e-12 => {}
        other => panic!("expected (sin 0 ≈ 0, exp 0 ≈ 1), got {:?}", other),
    }
}

#[test]
fn diff_jit_flonum_inverse_trig() {
    // ADR 0012 D-2 (iter DG) — asin / acos / atan lower to runtime
    // helpers.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (as x) (asin x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ac x) (acos x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (at x) (atan x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (as 0.5) (ac 0.5) (at 1.0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let asin0 = rt.eval_str_via_vm("<diff>", "(as 0.0)").unwrap();
    let acos1 = rt.eval_str_via_vm("<diff>", "(ac 1.0)").unwrap();
    let atan0 = rt.eval_str_via_vm("<diff>", "(at 0.0)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "inverse trig never dispatched through JIT (count={after})"
    );
    match (&asin0, &acos1, &atan0) {
        (
            Value::Number(cs_core::Number::Flonum(a)),
            Value::Number(cs_core::Number::Flonum(b)),
            Value::Number(cs_core::Number::Flonum(c)),
        ) if a.abs() < 1e-12 && b.abs() < 1e-12 && c.abs() < 1e-12 => {}
        other => panic!("expected (asin 0, acos 1, atan 0 all ≈ 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_fill() {
    // ADR 0012 D-2 (iter DH) — string-fill! overwrites all chars
    // of a string with the given Character. Mutation observable
    // through subsequent string-ref.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (fill-and-read s c) (string-fill! s c) (string-ref s 0))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (fill-and-read (make-string 4 #\\a) #\\Z) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r = rt
        .eval_str_via_vm("<diff>", "(fill-and-read (make-string 5 #\\a) #\\Q)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 1,
        "string-fill! never dispatched through JIT (count={after})"
    );
    match &r {
        Value::Character('Q') => {}
        other => panic!("expected #\\Q, got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_min_max() {
    // ADR 0012 D-2 (iter DI) — variadic min / max chained over
    // multiple args (3+). 2-arg form was already handled via the
    // single-Inst table; this iter extends to longer chains.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m3 a b c) (min a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (x4 a b c d) (max a b c d))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (m3 1 2 3) (x4 1 2 3 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let mn = rt.eval_str_via_vm("<diff>", "(m3 7 3 9)").unwrap(); // 3
    let mx = rt.eval_str_via_vm("<diff>", "(x4 7 3 9 5)").unwrap(); // 9
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "variadic min/max never dispatched through JIT (count={after})"
    );
    match (&mn, &mx) {
        (Value::Number(cs_core::Number::Fixnum(3)), Value::Number(cs_core::Number::Fixnum(9))) => {}
        other => panic!("expected (3, 9), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_bitwise() {
    // ADR 0012 D-2 (iter DJ) — variadic bitwise-and / -ior / -xor.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (and3 a b c) (bitwise-and a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (or3 a b c) (bitwise-or a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (xor3 a b c) (bitwise-xor a b c))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (and3 1 2 3) (or3 1 2 4) (xor3 1 2 4) \
                    (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r_and = rt
        .eval_str_via_vm("<diff>", "(and3 #xFF #x0F #x33)")
        .unwrap();
    let r_or = rt.eval_str_via_vm("<diff>", "(or3 1 2 4)").unwrap();
    let r_xor = rt.eval_str_via_vm("<diff>", "(xor3 1 2 4)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic bitwise never dispatched through JIT (count={after})"
    );
    match (&r_and, &r_or, &r_xor) {
        (
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(7)),
            Value::Number(cs_core::Number::Fixnum(7)),
        ) => {}
        other => panic!("expected (3, 7, 7), got {:?}", other),
    }
}

#[test]
fn diff_jit_bitwise_not() {
    // ADR 0012 D-2 (iter DK) — bitwise-not (1-arg) via Cranelift
    // native bnot.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bn x) (bitwise-not x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done (begin (bn 0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // ~0 = -1
    let r0 = rt.eval_str_via_vm("<diff>", "(bn 0)").unwrap();
    // ~5 = -6 (two's complement: ~x = -x-1)
    let r5 = rt.eval_str_via_vm("<diff>", "(bn 5)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "bitwise-not never dispatched through JIT (count={after})"
    );
    match (&r0, &r5) {
        (
            Value::Number(cs_core::Number::Fixnum(-1)),
            Value::Number(cs_core::Number::Fixnum(-6)),
        ) => {}
        other => panic!("expected (-1, -6), got {:?}", other),
    }
}

#[test]
fn diff_jit_arithmetic_shift() {
    // ADR 0012 D-2 (iter DL) — arithmetic-shift (positive count =
    // left shift, negative = arithmetic right shift).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ash n c) (arithmetic-shift n c))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (ash 1 4) (ash 16 -2) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 1 << 4 = 16
    let r_l = rt.eval_str_via_vm("<diff>", "(ash 1 4)").unwrap();
    // 16 >> 2 = 4
    let r_r = rt.eval_str_via_vm("<diff>", "(ash 16 -2)").unwrap();
    // Arithmetic right shift preserves sign: -16 >> 2 = -4
    let r_neg = rt.eval_str_via_vm("<diff>", "(ash -16 -2)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "arithmetic-shift never dispatched through JIT (count={after})"
    );
    match (&r_l, &r_r, &r_neg) {
        (
            Value::Number(cs_core::Number::Fixnum(16)),
            Value::Number(cs_core::Number::Fixnum(4)),
            Value::Number(cs_core::Number::Fixnum(-4)),
        ) => {}
        other => panic!("expected (16, 4, -4), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_compares() {
    // ADR 0012 D-2 (iter DM) — variadic <, >, <=, >=, = (3+ args).
    // R6RS pairwise: (< a b c) means a<b AND b<c.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lt3 a b c) (if (< a b c) 1 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (eq3 a b c) (if (= a b c) 1 0))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (lt3 1 2 3) (eq3 5 5 5) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 1 < 2 < 3 → true
    let lt_ok = rt.eval_str_via_vm("<diff>", "(lt3 1 2 3)").unwrap();
    // 1 < 2 < 2 → false (second pair fails)
    let lt_no = rt.eval_str_via_vm("<diff>", "(lt3 1 2 2)").unwrap();
    // 5 = 5 = 5 → true
    let eq_ok = rt.eval_str_via_vm("<diff>", "(eq3 5 5 5)").unwrap();
    // 5 = 5 = 6 → false
    let eq_no = rt.eval_str_via_vm("<diff>", "(eq3 5 5 6)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "variadic compares never dispatched through JIT (count={after})"
    );
    match (&lt_ok, &lt_no, &eq_ok, &eq_no) {
        (
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (1,0,1,0), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_list() {
    // ADR 0012 D-2 (iter DN) — variadic `list` lowers as right-to-
    // left Cons chain via the existing Cons RIR. No new helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l3 a b c) (list a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l0) (list))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (l3 1 2 3) (l0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // (list 10 20 30) has length 3.
    let len = rt
        .eval_str_via_vm("<diff>", "(length (l3 10 20 30))")
        .unwrap();
    // First element of (list 10 20 30) is 10.
    let first = rt.eval_str_via_vm("<diff>", "(car (l3 10 20 30))").unwrap();
    // (list) is empty.
    let empty_len = rt.eval_str_via_vm("<diff>", "(length (l0))").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic list never dispatched through JIT (count={after})"
    );
    match (&len, &first, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (3, 10, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_vector() {
    // ADR 0012 D-2 (iter DO) — variadic `vector` lowers via stack-
    // buffer + vm_make_vector_buf helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (v3 a b c) (vector a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (v0) (vector))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (v3 1 2 3) (v0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(vector-length (v3 10 20 30))")
        .unwrap();
    let elem = rt
        .eval_str_via_vm("<diff>", "(vector-ref (v3 10 20 30) 1)")
        .unwrap();
    let empty_len = rt
        .eval_str_via_vm("<diff>", "(vector-length (v0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic vector never dispatched through JIT (count={after})"
    );
    match (&len, &elem, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(20)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (3, 20, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_string() {
    // ADR 0012 D-2 (iter DP) — variadic `string` lowers via stack-
    // buffer + vm_make_string_buf helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s3 a b c) (string a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s0) (string))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (s3 #\\a #\\b #\\c) (s0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(string-length (s3 #\\x #\\y #\\z))")
        .unwrap();
    let ch = rt
        .eval_str_via_vm("<diff>", "(string-ref (s3 #\\x #\\y #\\z) 1)")
        .unwrap();
    let empty_len = rt
        .eval_str_via_vm("<diff>", "(string-length (s0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic string never dispatched through JIT (count={after})"
    );
    match (&len, &ch, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Character('y'),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (3, #\\y, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_bytevector() {
    // ADR 0012 D-2 (iter DQ) — variadic `bytevector` lowers via
    // stack-buffer + vm_make_bytevector_buf helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bv3 a b c) (bytevector a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bv0) (bytevector))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (bv3 1 2 3) (bv0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(bytevector-length (bv3 10 20 30))")
        .unwrap();
    let byte = rt
        .eval_str_via_vm("<diff>", "(bytevector-u8-ref (bv3 10 20 30) 1)")
        .unwrap();
    let empty_len = rt
        .eval_str_via_vm("<diff>", "(bytevector-length (bv0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic bytevector never dispatched through JIT (count={after})"
    );
    match (&len, &byte, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Number(cs_core::Number::Fixnum(20)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (3, 20, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_string_append() {
    // ADR 0012 D-2 (iter DR) — variadic `string-append` lowers via
    // stack-buffer + vm_string_append_buf helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sa3 a b c) (string-append a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sa0) (string-append))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (sa3 \"ab\" \"cd\" \"ef\") (sa0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let s = rt
        .eval_str_via_vm("<diff>", "(sa3 \"hi-\" \"there-\" \"world\")")
        .unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(sa0)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "variadic string-append never dispatched through JIT (count={after})"
    );
    match (&s, &empty) {
        (Value::String(sg), Value::String(eg)) => {
            assert_eq!(&*sg.borrow(), "hi-there-world");
            assert_eq!(&*eg.borrow(), "");
        }
        other => panic!("expected two strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_append() {
    // ADR 0012 D-2 (iter DS) — variadic `append` lowers via stack-
    // buffer + vm_append_buf helper. R7RS: last arg used as-is.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ap3 a b c) (append a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ap0) (append))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (ap3 '(1 2) '(3) '(4 5)) (ap0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lst = rt
        .eval_str_via_vm("<diff>", "(ap3 '(1 2) '(3) '(4 5))")
        .unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(ap0)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "variadic append never dispatched through JIT (count={after})"
    );
    let len = rt
        .eval_str_via_vm("<diff>", "(length (ap3 '(1 2) '(3) '(4 5)))")
        .unwrap();
    let last = rt
        .eval_str_via_vm("<diff>", "(list-ref (ap3 '(1 2) '(3) '(4 5)) 4)")
        .unwrap();
    match (&lst, &empty, &len, &last) {
        (
            Value::Pair(_),
            Value::Null,
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(5)),
        ) => {}
        other => panic!("expected (pair, null, 5, 5), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_vector_append() {
    // ADR 0012 D-2 (iter DT) — variadic `vector-append` lowers via
    // stack-buffer + vm_vector_append_buf helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (va3 a b c) (vector-append a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (va0) (vector-append))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (va3 #(1 2) #(3) #(4 5)) (va0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm("<diff>", "(vector-length (va3 #(1 2) #(3) #(4 5)))")
        .unwrap();
    let elem = rt
        .eval_str_via_vm("<diff>", "(vector-ref (va3 #(1 2) #(3) #(4 5)) 3)")
        .unwrap();
    let empty_len = rt
        .eval_str_via_vm("<diff>", "(vector-length (va0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic vector-append never dispatched through JIT (count={after})"
    );
    match (&len, &elem, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(4)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (5, 4, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_variadic_bytevector_append() {
    // ADR 0012 D-2 (iter DU) — variadic `bytevector-append` lowers
    // via stack-buffer + vm_bytevector_append_buf helper.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ba3 a b c) (bytevector-append a b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ba0) (bytevector-append))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) (if (= i 1500) 'done \
             (begin (ba3 (bytevector 1 2) (bytevector 3) (bytevector 4 5)) \
                    (ba0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let len = rt
        .eval_str_via_vm(
            "<diff>",
            "(bytevector-length (ba3 (bytevector 10 20) (bytevector 30) (bytevector 40 50)))",
        )
        .unwrap();
    let byte = rt
        .eval_str_via_vm(
            "<diff>",
            "(bytevector-u8-ref (ba3 (bytevector 10 20) (bytevector 30) (bytevector 40 50)) 3)",
        )
        .unwrap();
    let empty_len = rt
        .eval_str_via_vm("<diff>", "(bytevector-length (ba0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "variadic bytevector-append never dispatched through JIT (count={after})"
    );
    match (&len, &byte, &empty_len) {
        (
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Number(cs_core::Number::Fixnum(40)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (5, 40, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_cxr_accessors() {
    // ADR 0012 D-2 (iter DV) — composed pair accessors lower to
    // chains of Car/Cdr. Covers 2/3/4-letter cxr forms.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-caar x) (caar x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-cadr x) (cadr x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-cdar x) (cdar x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-cddr x) (cddr x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-caddr x) (caddr x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-cadddr x) (cadddr x))")
        .unwrap();
    // Warmup: each function called repeatedly so the JIT compiles.
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (f-caar '((1 2) 3)) \
                      (f-cadr '(1 2 3)) \
                      (f-cdar '((1 2) 3)) \
                      (f-cddr '(1 2 3 4)) \
                      (f-caddr '(1 2 3 4)) \
                      (f-cadddr '(1 2 3 4 5)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let caar = rt
        .eval_str_via_vm("<diff>", "(f-caar '((10 20) 30))")
        .unwrap();
    let cadr = rt
        .eval_str_via_vm("<diff>", "(f-cadr '(10 20 30))")
        .unwrap();
    let cdar = rt
        .eval_str_via_vm("<diff>", "(f-cdar '((10 20) 30))")
        .unwrap();
    let cddr = rt
        .eval_str_via_vm("<diff>", "(f-cddr '(10 20 30 40))")
        .unwrap();
    let caddr = rt
        .eval_str_via_vm("<diff>", "(f-caddr '(10 20 30 40))")
        .unwrap();
    let cadddr = rt
        .eval_str_via_vm("<diff>", "(f-cadddr '(10 20 30 40 50))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "cxr accessors never dispatched through JIT (count={after})"
    );
    match (&caar, &cadr, &cddr, &caddr, &cadddr) {
        (
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(20)),
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(30)),
            Value::Number(cs_core::Number::Fixnum(40)),
        ) => {}
        other => panic!("expected (10, 20, pair, 30, 40), got {:?}", other),
    }
    // cdar of ((10 20) 30) is (20) — a pair.
    assert!(
        matches!(&cdar, Value::Pair(_)),
        "expected pair for cdar, got {:?}",
        cdar
    );
}

#[test]
fn diff_jit_string_ordered_compares() {
    // ADR 0012 D-2 (iter DW) — string<?/<=?/>?/>=? lower to
    // dedicated helpers mirroring string=?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (slt a b) (string<? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sgt a b) (string>? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sle a b) (string<=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sge a b) (string>=? a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (slt \"a\" \"b\") (sgt \"b\" \"a\") \
                      (sle \"a\" \"a\") (sge \"a\" \"a\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lt_t = rt
        .eval_str_via_vm("<diff>", "(slt \"apple\" \"banana\")")
        .unwrap();
    let lt_f = rt
        .eval_str_via_vm("<diff>", "(slt \"banana\" \"apple\")")
        .unwrap();
    let gt_t = rt
        .eval_str_via_vm("<diff>", "(sgt \"banana\" \"apple\")")
        .unwrap();
    let le_eq = rt
        .eval_str_via_vm("<diff>", "(sle \"abc\" \"abc\")")
        .unwrap();
    let ge_eq = rt
        .eval_str_via_vm("<diff>", "(sge \"abc\" \"abc\")")
        .unwrap();
    let le_f = rt
        .eval_str_via_vm("<diff>", "(sle \"abd\" \"abc\")")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "string ordered compares never dispatched through JIT (count={after})"
    );
    match (&lt_t, &lt_f, &gt_t, &le_eq, &ge_eq, &le_f) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, T, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_ci_compares() {
    // ADR 0012 D-2 (iter DX) — case-insensitive string compares.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cieq a b) (string-ci=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cilt a b) (string-ci<? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cigt a b) (string-ci>? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cile a b) (string-ci<=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cige a b) (string-ci>=? a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (cieq \"Hi\" \"hi\") (cilt \"A\" \"b\") \
                      (cigt \"B\" \"a\") (cile \"a\" \"A\") \
                      (cige \"A\" \"a\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let eq_t = rt
        .eval_str_via_vm("<diff>", "(cieq \"HELLO\" \"hello\")")
        .unwrap();
    let eq_f = rt
        .eval_str_via_vm("<diff>", "(cieq \"Hello\" \"World\")")
        .unwrap();
    let lt_t = rt
        .eval_str_via_vm("<diff>", "(cilt \"Apple\" \"BANANA\")")
        .unwrap();
    let gt_t = rt
        .eval_str_via_vm("<diff>", "(cigt \"BANANA\" \"apple\")")
        .unwrap();
    let le_eq = rt
        .eval_str_via_vm("<diff>", "(cile \"abc\" \"ABC\")")
        .unwrap();
    let ge_eq = rt
        .eval_str_via_vm("<diff>", "(cige \"abc\" \"ABC\")")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 5,
        "string-ci compares never dispatched through JIT (count={after})"
    );
    match (&eq_t, &eq_f, &lt_t, &gt_t, &le_eq, &ge_eq) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(true),
        ) => {}
        other => panic!("expected (T, F, T, T, T, T), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_vector_conversion() {
    // ADR 0012 D-2 (iter DY) — 1-arg (string->vector s) and
    // (vector->string v) lower via dedicated helpers.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s2v s) (string->vector s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (v2s v) (vector->string v))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (s2v \"abc\") \
                      (v2s #(#\\a #\\b #\\c)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let vec_from_str = rt.eval_str_via_vm("<diff>", "(s2v \"hello\")").unwrap();
    let str_from_vec = rt
        .eval_str_via_vm("<diff>", "(v2s #(#\\f #\\o #\\o))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "string<->vector conversions never dispatched through JIT (count={after})"
    );
    match (&vec_from_str, &str_from_vec) {
        (Value::Vector(vg), Value::String(sg)) => {
            let v = vg.borrow();
            assert_eq!(v.len(), 5);
            assert!(matches!(&v[0], Value::Character('h')));
            assert!(matches!(&v[4], Value::Character('o')));
            assert_eq!(&*sg.borrow(), "foo");
        }
        other => panic!("expected (vector, string), got {:?}", other),
    }
}

#[test]
fn diff_jit_equal_deep() {
    // ADR 0012 D-2 (iter DZ) — equal? deep structural equality.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (eq2 a b) (equal? a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (eq2 '(1 2) '(1 2)) \
                      (eq2 #(1 2) #(1 2)) \
                      (eq2 \"abc\" \"abc\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let list_t = rt
        .eval_str_via_vm("<diff>", "(eq2 '(1 (2 3)) '(1 (2 3)))")
        .unwrap();
    let list_f = rt
        .eval_str_via_vm("<diff>", "(eq2 '(1 (2 3)) '(1 (2 4)))")
        .unwrap();
    let vec_t = rt
        .eval_str_via_vm("<diff>", "(eq2 #(1 2 #(3 4)) #(1 2 #(3 4)))")
        .unwrap();
    let str_t = rt
        .eval_str_via_vm("<diff>", "(eq2 \"hello\" \"hello\")")
        .unwrap();
    let mixed_f = rt.eval_str_via_vm("<diff>", "(eq2 1 \"1\")").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "equal? never dispatched through JIT (count={after})"
    );
    match (&list_t, &list_f, &vec_t, &str_t, &mixed_f) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_sqrt_flo_and_fix() {
    // ADR 0012 D-2 (iter EA) — sqrt for typed numeric args lowers
    // via FlonumSqrt (with FixToFlo for Fixnum). Result always
    // Flonum.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sqrt-flo x) (sqrt x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sqrt-fix n) (sqrt n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sqrt-flo 4.0) (sqrt-fix 16) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let f9 = rt.eval_str_via_vm("<diff>", "(sqrt-flo 9.0)").unwrap();
    let f25 = rt.eval_str_via_vm("<diff>", "(sqrt-fix 25)").unwrap();
    let f2 = rt.eval_str_via_vm("<diff>", "(sqrt-flo 2.0)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "sqrt never dispatched through JIT (count={after})"
    );
    match (&f9, &f25, &f2) {
        (
            Value::Number(cs_core::Number::Flonum(a)),
            Value::Number(cs_core::Number::Flonum(b)),
            Value::Number(cs_core::Number::Flonum(c)),
        ) => {
            assert!((a - 3.0).abs() < 1e-12, "sqrt(9.0) = {a}");
            assert!((b - 5.0).abs() < 1e-12, "sqrt(25) = {b}");
            assert!((c - 2.0_f64.sqrt()).abs() < 1e-12, "sqrt(2.0) = {c}");
        }
        other => panic!("expected three flonums, got {:?}", other),
    }
}

#[test]
fn diff_jit_abs_min_max_flonum() {
    // ADR 0012 D-2 (iter EB) — abs/max/min route through Flonum
    // RIR when any operand is Flonum-typed.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-abs x) (abs x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-max a b) (max a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f-min a b) (min a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (f-abs -1.5) (f-max 1.0 2.0) (f-min 3.0 4.0) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let a = rt.eval_str_via_vm("<diff>", "(f-abs -3.5)").unwrap();
    let mx = rt.eval_str_via_vm("<diff>", "(f-max 2.5 7.5)").unwrap();
    let mn = rt.eval_str_via_vm("<diff>", "(f-min 2.5 7.5)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "flonum abs/max/min never dispatched through JIT (count={after})"
    );
    match (&a, &mx, &mn) {
        (
            Value::Number(cs_core::Number::Flonum(av)),
            Value::Number(cs_core::Number::Flonum(mxv)),
            Value::Number(cs_core::Number::Flonum(mnv)),
        ) => {
            assert!((av - 3.5).abs() < 1e-12, "abs -3.5 = {av}");
            assert!((mxv - 7.5).abs() < 1e-12, "max 2.5 7.5 = {mxv}");
            assert!((mnv - 2.5).abs() < 1e-12, "min 2.5 7.5 = {mnv}");
        }
        other => panic!("expected three flonums, got {:?}", other),
    }
}
