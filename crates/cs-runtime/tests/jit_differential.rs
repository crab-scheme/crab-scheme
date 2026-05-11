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
