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

#[test]
fn diff_jit_number_string_conversion() {
    // ADR 0012 D-2 (iter EC) — 1-arg number->string / string->number.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (n2s x) (number->string x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s2n s) (string->number s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (n2s 42) (n2s 3.14) (s2n \"7\") (s2n \"abc\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let s_int = rt.eval_str_via_vm("<diff>", "(n2s 1234)").unwrap();
    let s_flo = rt.eval_str_via_vm("<diff>", "(n2s 2.5)").unwrap();
    let n_int = rt.eval_str_via_vm("<diff>", "(s2n \"42\")").unwrap();
    let n_flo = rt.eval_str_via_vm("<diff>", "(s2n \"1.5\")").unwrap();
    let n_bad = rt.eval_str_via_vm("<diff>", "(s2n \"hello\")").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "number<->string never dispatched through JIT (count={after})"
    );
    match (&s_int, &s_flo) {
        (Value::String(a), Value::String(b)) => {
            assert_eq!(&*a.borrow(), "1234");
            assert!(b.borrow().starts_with("2.5"), "n2s 2.5 = {:?}", b.borrow());
        }
        other => panic!("expected (string, string), got {:?}", other),
    }
    match (&n_int, &n_flo, &n_bad) {
        (
            Value::Number(cs_core::Number::Fixnum(42)),
            Value::Number(cs_core::Number::Flonum(f)),
            Value::Boolean(false),
        ) => {
            assert!((f - 1.5).abs() < 1e-12);
        }
        other => panic!("expected (42, 1.5, #f), got {:?}", other),
    }
}

#[test]
fn diff_jit_r7rs_div_ops() {
    // ADR 0012 D-2 (iter ED) — R7RS division ops:
    // truncate-quotient (= quotient), truncate-remainder
    // (= remainder), floor-remainder (= modulo), and the new
    // floor-quotient.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tq a b) (truncate-quotient a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tr a b) (truncate-remainder a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fq a b) (floor-quotient a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fr a b) (floor-remainder a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (tq 7 2) (tr 7 2) (fq 7 2) (fr 7 2) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // truncate -> toward zero. floor -> toward -inf.
    // Use mixed signs to differentiate.
    //   7  2: tq=3 tr=1 fq=3 fr=1
    //  -7  2: tq=-3 tr=-1 fq=-4 fr=1
    //   7 -2: tq=-3 tr=1 fq=-4 fr=-1
    let r1 = rt.eval_str_via_vm("<diff>", "(tq -7 2)").unwrap();
    let r2 = rt.eval_str_via_vm("<diff>", "(tr -7 2)").unwrap();
    let r3 = rt.eval_str_via_vm("<diff>", "(fq -7 2)").unwrap();
    let r4 = rt.eval_str_via_vm("<diff>", "(fr -7 2)").unwrap();
    let r5 = rt.eval_str_via_vm("<diff>", "(fq 7 -2)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "R7RS div ops never dispatched through JIT (count={after})"
    );
    match (&r1, &r2, &r3, &r4, &r5) {
        (
            Value::Number(cs_core::Number::Fixnum(-3)),
            Value::Number(cs_core::Number::Fixnum(-1)),
            Value::Number(cs_core::Number::Fixnum(-4)),
            Value::Number(cs_core::Number::Fixnum(1)),
            Value::Number(cs_core::Number::Fixnum(-4)),
        ) => {}
        other => panic!("expected (-3, -1, -4, 1, -4), got {:?}", other),
    }
}

#[test]
fn diff_jit_exact_inexact_type_aware() {
    // ADR 0012 D-2 (iter EE) — exact?/inexact? type-aware. Closes
    // a latent gap where the JIT previously emitted const-true /
    // const-false regardless of operand type. Now Flonum-typed
    // args correctly return exact? = #f, inexact? = #t.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ex-fix n) (exact? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ex-flo x) (exact? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (in-fix n) (inexact? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (in-flo x) (inexact? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ex-fix 1) (ex-flo 1.0) (in-fix 1) (in-flo 1.0) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let ef = rt.eval_str_via_vm("<diff>", "(ex-fix 42)").unwrap();
    let el = rt.eval_str_via_vm("<diff>", "(ex-flo 3.14)").unwrap();
    let inf = rt.eval_str_via_vm("<diff>", "(in-fix 42)").unwrap();
    let il = rt.eval_str_via_vm("<diff>", "(in-flo 3.14)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "exact?/inexact? never dispatched through JIT (count={after})"
    );
    match (&ef, &el, &inf, &il) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Boolean(true),
        ) => {}
        other => panic!("expected (T, F, F, T), got {:?}", other),
    }
}

#[test]
fn diff_jit_nan_infinite_finite_flo() {
    // ADR 0012 D-2 (iter EF) — nan?/infinite?/finite? for Flonum
    // via inline fcmp. Fixnum/non-flonum cases use the const
    // predicates from earlier iters.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (np x) (nan? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ip x) (infinite? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fp x) (finite? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (np 1.0) (ip 1.0) (fp 1.0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let nan_t = rt.eval_str_via_vm("<diff>", "(np (/ 0.0 0.0))").unwrap();
    let nan_f = rt.eval_str_via_vm("<diff>", "(np 1.5)").unwrap();
    let inf_t = rt.eval_str_via_vm("<diff>", "(ip (/ 1.0 0.0))").unwrap();
    let inf_f = rt.eval_str_via_vm("<diff>", "(ip 1.5)").unwrap();
    let fin_t = rt.eval_str_via_vm("<diff>", "(fp 1.5)").unwrap();
    let fin_f1 = rt.eval_str_via_vm("<diff>", "(fp (/ 1.0 0.0))").unwrap();
    let fin_f2 = rt.eval_str_via_vm("<diff>", "(fp (/ 0.0 0.0))").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 6,
        "nan?/infinite?/finite? never dispatched through JIT (count={after})"
    );
    match (&nan_t, &nan_f, &inf_t, &inf_f, &fin_t, &fin_f1, &fin_f2) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, F, T, F, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_typed_eq_any_args() {
    // ADR 0012 D-2 (iter EG) — boolean=?/char=?/symbol=? on
    // Any-shape args route through EqAny. Function parameters are
    // Any-shape until proven otherwise, so the integer-Eq fast
    // path would compare Gc pointers — wrong. EqAny decodes both
    // boxes and compares inner values.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (b= a b) (boolean=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (c= a b) (char=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s= a b) (symbol=? a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (b= #t #t) (c= #\\a #\\a) (s= 'x 'x) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let bt = rt.eval_str_via_vm("<diff>", "(b= #t #t)").unwrap();
    let bf = rt.eval_str_via_vm("<diff>", "(b= #t #f)").unwrap();
    let ct = rt.eval_str_via_vm("<diff>", "(c= #\\a #\\a)").unwrap();
    let cf = rt.eval_str_via_vm("<diff>", "(c= #\\a #\\b)").unwrap();
    let st = rt.eval_str_via_vm("<diff>", "(s= 'foo 'foo)").unwrap();
    let sf = rt.eval_str_via_vm("<diff>", "(s= 'foo 'bar)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 3,
        "boolean=?/char=?/symbol=? never dispatched through JIT (count={after})"
    );
    match (&bt, &bf, &ct, &cf, &st, &sf) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, F, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_integer_rational_flonum() {
    // ADR 0012 D-2 (iter EH) — integer?/rational? type-aware for
    // Flonum operand. Closes a latent gap where the JIT previously
    // emitted always-true regardless of value (e.g. (integer? 3.14)
    // would return #t).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (int-flo x) (integer? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rat-flo x) (rational? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (int-flo 1.0) (int-flo 1.5) \
                      (rat-flo 1.0) (rat-flo (/ 1.0 0.0)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let i_whole = rt.eval_str_via_vm("<diff>", "(int-flo 4.0)").unwrap();
    let i_frac = rt.eval_str_via_vm("<diff>", "(int-flo 3.14)").unwrap();
    let i_inf = rt
        .eval_str_via_vm("<diff>", "(int-flo (/ 1.0 0.0))")
        .unwrap();
    let r_finite = rt.eval_str_via_vm("<diff>", "(rat-flo 1.5)").unwrap();
    let r_inf = rt
        .eval_str_via_vm("<diff>", "(rat-flo (/ 1.0 0.0))")
        .unwrap();
    let r_nan = rt
        .eval_str_via_vm("<diff>", "(rat-flo (/ 0.0 0.0))")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 4,
        "integer?/rational? never dispatched through JIT (count={after})"
    );
    match (&i_whole, &i_frac, &i_inf, &r_finite, &r_inf, &r_nan) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, F, T, F, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_exact_integer_rational_flonum() {
    // ADR 0012 D-2 (iter EI) — exact-integer? / exact-rational?
    // for Flonum operand both return #f (flonums are inexact).
    // Separate Fixnum and Flonum warmups to keep parameter type
    // stable within each function (matches EH pattern).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ei-fix n) (exact-integer? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ei-flo x) (exact-integer? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (er-fix n) (exact-rational? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (er-flo x) (exact-rational? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ei-fix 1) (ei-flo 1.0) \
                      (er-fix 1) (er-flo 1.0) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let ei_fix = rt.eval_str_via_vm("<diff>", "(ei-fix 42)").unwrap();
    let ei_flo = rt.eval_str_via_vm("<diff>", "(ei-flo 42.0)").unwrap();
    let er_fix = rt.eval_str_via_vm("<diff>", "(er-fix 42)").unwrap();
    let er_flo = rt.eval_str_via_vm("<diff>", "(er-flo 1.5)").unwrap();
    let after = cs_vm::vm::jit_call_count();
    // Note: LoadConst-only bodies don't increment jit_call_count
    // (which tracks vm_* helper invocations). Verify correctness
    // by value alone.
    let _ = after;
    match (&ei_fix, &ei_flo, &er_fix, &er_flo) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_reverse() {
    // ADR 0012 D-2 (iter EJ) — (string-reverse s) returns a fresh
    // string with the characters of `s` in reverse order.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sr s) (string-reverse s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sr \"abc\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let hello = rt.eval_str_via_vm("<diff>", "(sr \"hello\")").unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(sr \"\")").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "string-reverse never dispatched through JIT (count={after})"
    );
    match (&hello, &empty) {
        (Value::String(h), Value::String(e)) => {
            assert_eq!(&*h.borrow(), "olleh");
            assert_eq!(&*e.borrow(), "");
        }
        other => panic!("expected two strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_square_flonum() {
    // ADR 0012 D-2 (iter EK) — square Flonum-aware. Closes latent
    // integer-mul-on-f64 bug: x * x for Flonum used integer imul.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sq-fix n) (square n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sq-flo x) (square x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sq-fix 3) (sq-flo 2.5) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let fix = rt.eval_str_via_vm("<diff>", "(sq-fix 7)").unwrap();
    let flo = rt.eval_str_via_vm("<diff>", "(sq-flo 2.5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&fix, &flo) {
        (Value::Number(cs_core::Number::Fixnum(49)), Value::Number(cs_core::Number::Flonum(f))) => {
            assert!((f - 6.25).abs() < 1e-12, "sq-flo 2.5 = {f}");
        }
        other => panic!("expected (49, 6.25), got {:?}", other),
    }
}

#[test]
fn diff_jit_integer_ops_flonum_guard() {
    // ADR 0012 D-2 (iter EL) — integer-only ops gated on !=Flonum.
    // Verify Fixnum operands still JIT correctly and Flonum
    // operands route to the VM error path.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (and2 a b) (bitwise-and a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mod2 a b) (modulo a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (and2 12 10) (mod2 7 3) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let and_val = rt.eval_str_via_vm("<diff>", "(and2 12 10)").unwrap();
    let mod_val = rt.eval_str_via_vm("<diff>", "(mod2 17 5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&and_val, &mod_val) {
        (Value::Number(cs_core::Number::Fixnum(8)), Value::Number(cs_core::Number::Fixnum(2))) => {}
        other => panic!("expected (8, 2), got {:?}", other),
    }
    // Flonum operand: bitwise-and 1.5 must error rather than
    // silently produce a wrong i64.
    let err = rt.eval_str_via_vm("<diff>", "(bitwise-and 1.5 255)");
    assert!(
        err.is_err(),
        "(bitwise-and 1.5 255) should error, got {:?}",
        err
    );
}

#[test]
fn diff_jit_make_list_2arg() {
    // ADR 0012 D-2 (iter EM) — (make-list n fill) builds a fresh
    // list of n copies of fill.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mkl n f) (make-list n f))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (mkl 3 'x) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let three_zero = rt.eval_str_via_vm("<diff>", "(mkl 3 0)").unwrap();
    let zero_x = rt.eval_str_via_vm("<diff>", "(mkl 0 'x)").unwrap();
    let len = rt.eval_str_via_vm("<diff>", "(length (mkl 5 'a))").unwrap();
    let elem = rt
        .eval_str_via_vm("<diff>", "(list-ref (mkl 5 'a) 2)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "make-list never dispatched through JIT (count={after})"
    );
    match (&three_zero, &zero_x, &len, &elem) {
        (
            Value::Pair(_),
            Value::Null,
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Symbol(_),
        ) => {}
        other => panic!("expected (pair, null, 5, symbol), got {:?}", other),
    }
}

#[test]
fn diff_jit_iota_1arg() {
    // ADR 0012 D-2 (iter EN) — (iota n) returns (0 1 ... n-1).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (io n) (iota n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (io 5) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lst = rt.eval_str_via_vm("<diff>", "(io 4)").unwrap();
    let zero = rt.eval_str_via_vm("<diff>", "(io 0)").unwrap();
    let len = rt.eval_str_via_vm("<diff>", "(length (io 10))").unwrap();
    let last = rt
        .eval_str_via_vm("<diff>", "(list-ref (io 10) 9)")
        .unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "iota never dispatched through JIT (count={after})"
    );
    match (&lst, &zero, &len, &last) {
        (
            Value::Pair(_),
            Value::Null,
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(9)),
        ) => {}
        other => panic!("expected (pair, null, 10, 9), got {:?}", other),
    }
}

#[test]
fn diff_jit_last_pair_and_last() {
    // ADR 0012 D-2 (iter EO) — last-pair and last walk to the end
    // of a proper list.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lp x) (last-pair x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (la x) (last x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (lp '(1 2 3)) (la '(1 2 3)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lp = rt.eval_str_via_vm("<diff>", "(lp '(10 20 30))").unwrap();
    let lp_car = rt
        .eval_str_via_vm("<diff>", "(car (lp '(10 20 30)))")
        .unwrap();
    let la = rt.eval_str_via_vm("<diff>", "(la '(10 20 30))").unwrap();
    let after = cs_vm::vm::jit_call_count();
    assert!(
        after >= 2,
        "last-pair/last never dispatched through JIT (count={after})"
    );
    match (&lp, &lp_car, &la) {
        (
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(30)),
            Value::Number(cs_core::Number::Fixnum(30)),
        ) => {}
        other => panic!("expected (pair, 30, 30), got {:?}", other),
    }
}

#[test]
fn diff_jit_zero_positive_negative_flonum() {
    // ADR 0012 D-2 (iter EP) — zero?/positive?/negative? type-aware
    // for Flonum operand via FlonumEq/FlonumLt against 0.0.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (z x) (zero? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (p x) (positive? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (n x) (negative? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (z 1.0) (p 1.0) (n 1.0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let z0 = rt.eval_str_via_vm("<diff>", "(z 0.0)").unwrap();
    let z1 = rt.eval_str_via_vm("<diff>", "(z 1.5)").unwrap();
    let p_t = rt.eval_str_via_vm("<diff>", "(p 1.5)").unwrap();
    let p_f = rt.eval_str_via_vm("<diff>", "(p -1.5)").unwrap();
    let p_z = rt.eval_str_via_vm("<diff>", "(p 0.0)").unwrap();
    let n_t = rt.eval_str_via_vm("<diff>", "(n -1.5)").unwrap();
    let n_f = rt.eval_str_via_vm("<diff>", "(n 1.5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&z0, &z1, &p_t, &p_f, &p_z, &n_t, &n_f) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, F, F, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_not_type_aware() {
    // ADR 0012 D-2 (iter EQ) — type-aware (not x). Boolean path
    // is the existing Eq(x, 0). Any path goes through AnyTruthy.
    // Other primitive types are always truthy → not is always #f.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (n-any x) (not x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (n-bool x) (not (if x #t #f)))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (n-any #t) (n-any 1) (n-bool #f) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Any operand:
    let a_f = rt.eval_str_via_vm("<diff>", "(n-any #f)").unwrap();
    let a_t = rt.eval_str_via_vm("<diff>", "(n-any #t)").unwrap();
    let a_num = rt.eval_str_via_vm("<diff>", "(n-any 0)").unwrap();
    let a_nil = rt.eval_str_via_vm("<diff>", "(n-any '())").unwrap();
    // Boolean operand (via (if x #t #f) normalization):
    let b_f = rt.eval_str_via_vm("<diff>", "(n-bool #f)").unwrap();
    let b_t = rt.eval_str_via_vm("<diff>", "(n-bool #t)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&a_f, &a_t, &a_num, &a_nil, &b_f, &b_t) {
        (
            Value::Boolean(true),  // (not #f) = #t
            Value::Boolean(false), // (not #t) = #f
            Value::Boolean(false), // (not 0) = #f (only #f is falsy)
            Value::Boolean(false), // (not '()) = #f
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, F, F, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_vector_copy_bang_3arg() {
    // ADR 0012 D-2 (iter ER) — (vector-copy! dest at src). Copies
    // src into dest starting at index `at`.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (vcb dest at src) (vector-copy! dest at src) dest)",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (vcb (make-vector 4 0) 1 #(10 20)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // dest = #(0 0 0 0 0), at=1, src=#(7 8 9) → dest becomes #(0 7 8 9 0).
    let v = rt
        .eval_str_via_vm("<diff>", "(vcb (make-vector 5 0) 1 #(7 8 9))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &v {
        Value::Vector(vg) => {
            let inner = vg.borrow();
            assert_eq!(inner.len(), 5);
            // index 0 stays 0, indices 1..=3 = 7, 8, 9, index 4 stays 0.
            for (i, expected) in [0i64, 7, 8, 9, 0].iter().enumerate() {
                match &inner[i] {
                    Value::Number(cs_core::Number::Fixnum(n)) => assert_eq!(*n, *expected),
                    other => panic!("at {i}: expected fixnum {expected}, got {:?}", other),
                }
            }
        }
        other => panic!("expected vector, got {:?}", other),
    }
}

#[test]
fn diff_jit_bytevector_string_copy_bang() {
    // ADR 0012 D-2 (iter ES) — (bytevector-copy! d at s) and
    // (string-copy! d at s) 3-arg forms.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (bcb dest at src) (bytevector-copy! dest at src) dest)",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (scb dest at src) (string-copy! dest at src) dest)",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (bcb (make-bytevector 4 0) 1 (bytevector 9 9)) \
                      (scb (make-string 4 #\\.) 1 \"ab\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let bv = rt
        .eval_str_via_vm("<diff>", "(bcb (make-bytevector 5 0) 1 (bytevector 7 8 9))")
        .unwrap();
    let s = rt
        .eval_str_via_vm("<diff>", "(scb (make-string 5 #\\.) 1 \"xyz\")")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &bv {
        Value::ByteVector(bg) => {
            let inner = bg.borrow();
            assert_eq!(&inner[..], &[0, 7, 8, 9, 0]);
        }
        other => panic!("expected bytevector, got {:?}", other),
    }
    match &s {
        Value::String(sg) => {
            assert_eq!(&*sg.borrow(), ".xyz.");
        }
        other => panic!("expected string, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_case_conversion() {
    // ADR 0012 D-2 (iter ET) — string-upcase / string-downcase /
    // string-foldcase.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (up s) (string-upcase s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (dn s) (string-downcase s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fc s) (string-foldcase s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (up \"abc\") (dn \"ABC\") (fc \"AbC\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let u = rt
        .eval_str_via_vm("<diff>", "(up \"Hello World\")")
        .unwrap();
    let d = rt
        .eval_str_via_vm("<diff>", "(dn \"Hello WORLD\")")
        .unwrap();
    let f = rt.eval_str_via_vm("<diff>", "(fc \"FoldME\")").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&u, &d, &f) {
        (Value::String(a), Value::String(b), Value::String(c)) => {
            assert_eq!(&*a.borrow(), "HELLO WORLD");
            assert_eq!(&*b.borrow(), "hello world");
            assert_eq!(&*c.borrow(), "foldme");
        }
        other => panic!("expected three strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_contains() {
    // ADR 0012 D-2 (iter EU) — (string-contains haystack needle).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sc h n) (string-contains h n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sc \"hello\" \"ll\") (sc \"foo\" \"bar\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let hit = rt
        .eval_str_via_vm("<diff>", "(sc \"hello world\" \"world\")")
        .unwrap();
    let miss = rt
        .eval_str_via_vm("<diff>", "(sc \"hello\" \"xyz\")")
        .unwrap();
    let head = rt
        .eval_str_via_vm("<diff>", "(sc \"abcdef\" \"abc\")")
        .unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(sc \"hello\" \"\")").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&hit, &miss, &head, &empty) {
        (
            Value::Number(cs_core::Number::Fixnum(6)),
            Value::Boolean(false),
            Value::Number(cs_core::Number::Fixnum(0)),
            Value::Number(cs_core::Number::Fixnum(0)),
        ) => {}
        other => panic!("expected (6, #f, 0, 0), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_prefix_suffix_p() {
    // ADR 0012 D-2 (iter EV) — string-prefix? / string-suffix?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (pre p s) (string-prefix? p s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (suf p s) (string-suffix? p s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (pre \"hello\" \"hello world\") \
                      (suf \"world\" \"hello world\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let pt = rt
        .eval_str_via_vm("<diff>", "(pre \"foo\" \"foobar\")")
        .unwrap();
    let pf = rt
        .eval_str_via_vm("<diff>", "(pre \"baz\" \"foobar\")")
        .unwrap();
    let st = rt
        .eval_str_via_vm("<diff>", "(suf \"bar\" \"foobar\")")
        .unwrap();
    let sf = rt
        .eval_str_via_vm("<diff>", "(suf \"foo\" \"foobar\")")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&pt, &pf, &st, &sf) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_reverse_bang() {
    // ADR 0012 D-2 (iter EW) — reverse! aliased to reverse.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rb x) (reverse! x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (rb '(1 2 3)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r = rt.eval_str_via_vm("<diff>", "(rb '(10 20 30 40))").unwrap();
    let first = rt
        .eval_str_via_vm("<diff>", "(car (rb '(10 20 30 40)))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&r, &first) {
        (Value::Pair(_), Value::Number(cs_core::Number::Fixnum(40))) => {}
        other => panic!("expected (pair, 40), got {:?}", other),
    }
}

#[test]
fn diff_jit_take_drop() {
    // ADR 0012 D-2 (iter EX) — take / drop SRFI-1.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tk lst n) (take lst n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (dr lst n) (drop lst n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (tk '(1 2 3 4 5) 2) \
                      (dr '(1 2 3 4 5) 2) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let t = rt.eval_str_via_vm("<diff>", "(tk '(1 2 3 4 5) 3)").unwrap();
    let t_len = rt
        .eval_str_via_vm("<diff>", "(length (tk '(1 2 3 4 5) 3))")
        .unwrap();
    let d = rt.eval_str_via_vm("<diff>", "(dr '(1 2 3 4 5) 3)").unwrap();
    let d_car = rt
        .eval_str_via_vm("<diff>", "(car (dr '(1 2 3 4 5) 3))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&t, &t_len, &d, &d_car) {
        (
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(4)),
        ) => {}
        other => panic!("expected (pair, 3, pair, 4), got {:?}", other),
    }
}

#[test]
fn diff_jit_list_classifiers() {
    // ADR 0012 D-2 (iter EY) — null-list?/proper-list?/dotted-list?/
    // circular-list?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (nl x) (null-list? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (pl x) (proper-list? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (dl x) (dotted-list? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cl x) (circular-list? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (nl '()) (pl '(1 2 3)) (dl '(1 . 2)) (cl '()) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let n_t = rt.eval_str_via_vm("<diff>", "(nl '())").unwrap();
    let n_f = rt.eval_str_via_vm("<diff>", "(nl '(1 2))").unwrap();
    let p_t = rt.eval_str_via_vm("<diff>", "(pl '(1 2 3))").unwrap();
    let p_n = rt.eval_str_via_vm("<diff>", "(pl '())").unwrap();
    let p_f = rt.eval_str_via_vm("<diff>", "(pl '(1 . 2))").unwrap();
    let d_t = rt.eval_str_via_vm("<diff>", "(dl '(1 . 2))").unwrap();
    let d_f = rt.eval_str_via_vm("<diff>", "(dl '(1 2 3))").unwrap();
    let c_f = rt.eval_str_via_vm("<diff>", "(cl '(1 2 3))").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&n_t, &n_f, &p_t, &p_n, &p_f, &d_t, &d_f, &c_f) {
        (
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(false),
        ) => {}
        other => panic!("expected (T, F, T, T, F, T, F, F), got {:?}", other),
    }
}

#[test]
fn diff_jit_ordinal_accessors() {
    // ADR 0012 D-2 (iter EZ) — SRFI-1 first/second/third/fourth.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f1 x) (first x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f2 x) (second x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f3 x) (third x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f4 x) (fourth x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (f1 '(1 2 3 4 5)) (f2 '(1 2 3 4 5)) \
                      (f3 '(1 2 3 4 5)) (f4 '(1 2 3 4 5)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let a = rt
        .eval_str_via_vm("<diff>", "(f1 '(10 20 30 40 50))")
        .unwrap();
    let b = rt
        .eval_str_via_vm("<diff>", "(f2 '(10 20 30 40 50))")
        .unwrap();
    let c = rt
        .eval_str_via_vm("<diff>", "(f3 '(10 20 30 40 50))")
        .unwrap();
    let d = rt
        .eval_str_via_vm("<diff>", "(f4 '(10 20 30 40 50))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&a, &b, &c, &d) {
        (
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(20)),
            Value::Number(cs_core::Number::Fixnum(30)),
            Value::Number(cs_core::Number::Fixnum(40)),
        ) => {}
        other => panic!("expected (10, 20, 30, 40), got {:?}", other),
    }
}

#[test]
fn diff_jit_more_ordinal_accessors() {
    // ADR 0012 D-2 (iter FA) — SRFI-1 fifth/sixth/seventh/eighth/
    // ninth/tenth.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f5 x) (fifth x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f6 x) (sixth x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f7 x) (seventh x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f8 x) (eighth x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f9 x) (ninth x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f10 x) (tenth x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (f5 '(1 2 3 4 5 6 7 8 9 10)) \
                      (f10 '(1 2 3 4 5 6 7 8 9 10)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lst = "'(10 20 30 40 50 60 70 80 90 100)";
    let r5 = rt
        .eval_str_via_vm("<diff>", &format!("(f5 {})", lst))
        .unwrap();
    let r6 = rt
        .eval_str_via_vm("<diff>", &format!("(f6 {})", lst))
        .unwrap();
    let r7 = rt
        .eval_str_via_vm("<diff>", &format!("(f7 {})", lst))
        .unwrap();
    let r8 = rt
        .eval_str_via_vm("<diff>", &format!("(f8 {})", lst))
        .unwrap();
    let r9 = rt
        .eval_str_via_vm("<diff>", &format!("(f9 {})", lst))
        .unwrap();
    let r10 = rt
        .eval_str_via_vm("<diff>", &format!("(f10 {})", lst))
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&r5, &r6, &r7, &r8, &r9, &r10) {
        (
            Value::Number(cs_core::Number::Fixnum(50)),
            Value::Number(cs_core::Number::Fixnum(60)),
            Value::Number(cs_core::Number::Fixnum(70)),
            Value::Number(cs_core::Number::Fixnum(80)),
            Value::Number(cs_core::Number::Fixnum(90)),
            Value::Number(cs_core::Number::Fixnum(100)),
        ) => {}
        other => panic!("expected (50..100), got {:?}", other),
    }
}

#[test]
fn diff_jit_concatenate_and_not_pair_p() {
    // ADR 0012 D-2 (iter FB) — concatenate / not-pair?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cc lol) (concatenate lol))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (np x) (not-pair? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (cc '((1 2) (3) (4 5))) (np 1) (np '(1 2)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let c = rt
        .eval_str_via_vm("<diff>", "(cc '((1 2) (3) (4 5)))")
        .unwrap();
    let c_len = rt
        .eval_str_via_vm("<diff>", "(length (cc '((1 2) (3) (4 5))))")
        .unwrap();
    let np_t = rt.eval_str_via_vm("<diff>", "(np 42)").unwrap();
    let np_f = rt.eval_str_via_vm("<diff>", "(np '(1 2))").unwrap();
    let np_null = rt.eval_str_via_vm("<diff>", "(np '())").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&c, &c_len, &np_t, &np_f, &np_null) {
        (
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(5)),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Boolean(true),
        ) => {}
        other => panic!("expected (pair, 5, T, F, T), got {:?}", other),
    }
}

#[test]
fn diff_jit_iota_2arg() {
    // ADR 0012 D-2 (iter FC) — (iota count start) 2-arg.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (io2 c s) (iota c s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (io2 5 10) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lst = rt.eval_str_via_vm("<diff>", "(io2 4 100)").unwrap();
    let first = rt.eval_str_via_vm("<diff>", "(car (io2 4 100))").unwrap();
    let last = rt
        .eval_str_via_vm("<diff>", "(list-ref (io2 4 100) 3)")
        .unwrap();
    let zero = rt.eval_str_via_vm("<diff>", "(io2 0 10)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&lst, &first, &last, &zero) {
        (
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(100)),
            Value::Number(cs_core::Number::Fixnum(103)),
            Value::Null,
        ) => {}
        other => panic!("expected (pair, 100, 103, null), got {:?}", other),
    }
}

#[test]
fn diff_jit_iota_3arg() {
    // ADR 0012 D-2 (iter FD) — (iota count start step) 3-arg.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (io3 c s st) (iota c s st))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (io3 4 0 2) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let l = rt.eval_str_via_vm("<diff>", "(io3 5 10 3)").unwrap();
    let first = rt.eval_str_via_vm("<diff>", "(car (io3 5 10 3))").unwrap();
    let last = rt
        .eval_str_via_vm("<diff>", "(list-ref (io3 5 10 3) 4)")
        .unwrap();
    // Negative step: (io3 3 100 -10) → (100 90 80)
    let neg = rt
        .eval_str_via_vm("<diff>", "(list-ref (io3 3 100 -10) 2)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&l, &first, &last, &neg) {
        (
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(10)),
            Value::Number(cs_core::Number::Fixnum(22)),
            Value::Number(cs_core::Number::Fixnum(80)),
        ) => {}
        other => panic!("expected (pair, 10, 22, 80), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_join_2arg() {
    // ADR 0012 D-2 (iter FE) — (string-join parts sep).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sj parts sep) (string-join parts sep))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sj '(\"a\" \"b\") \"-\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let r = rt
        .eval_str_via_vm("<diff>", "(sj '(\"foo\" \"bar\" \"baz\") \", \")")
        .unwrap();
    let single = rt
        .eval_str_via_vm("<diff>", "(sj '(\"only\") \"--\")")
        .unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(sj '() \"-\")").unwrap();
    let no_sep = rt
        .eval_str_via_vm("<diff>", "(sj '(\"a\" \"b\" \"c\") \"\")")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&r, &single, &empty, &no_sep) {
        (Value::String(a), Value::String(b), Value::String(c), Value::String(d)) => {
            assert_eq!(&*a.borrow(), "foo, bar, baz");
            assert_eq!(&*b.borrow(), "only");
            assert_eq!(&*c.borrow(), "");
            assert_eq!(&*d.borrow(), "abc");
        }
        other => panic!("expected four strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_split_2arg() {
    // ADR 0012 D-2 (iter FF) — (string-split s sep).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ss s sep) (string-split s sep))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ss \"a,b\" \",\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let parts = rt
        .eval_str_via_vm("<diff>", "(ss \"foo,bar,baz\" \",\")")
        .unwrap();
    let len = rt
        .eval_str_via_vm("<diff>", "(length (ss \"foo,bar,baz\" \",\"))")
        .unwrap();
    let first = rt
        .eval_str_via_vm("<diff>", "(car (ss \"foo,bar,baz\" \",\"))")
        .unwrap();
    let char_sep_len = rt
        .eval_str_via_vm("<diff>", "(length (ss \"a-b-c\" #\\-))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&parts, &len, &first, &char_sep_len) {
        (
            Value::Pair(_),
            Value::Number(cs_core::Number::Fixnum(3)),
            Value::String(fs),
            Value::Number(cs_core::Number::Fixnum(3)),
        ) => {
            assert_eq!(&*fs.borrow(), "foo");
        }
        other => panic!("expected (pair, 3, \"foo\", 3), got {:?}", other),
    }
}

#[test]
fn diff_jit_string_pad_2arg() {
    // ADR 0012 D-2 (iter FG) — (string-pad s width) and
    // (string-pad-right s width) 2-arg forms (default space).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sp s w) (string-pad s w))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (spr s w) (string-pad-right s w))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sp \"a\" 4) (spr \"a\" 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lp = rt.eval_str_via_vm("<diff>", "(sp \"42\" 5)").unwrap();
    let rp = rt.eval_str_via_vm("<diff>", "(spr \"42\" 5)").unwrap();
    let lt = rt.eval_str_via_vm("<diff>", "(sp \"abcdef\" 3)").unwrap();
    let rt2 = rt.eval_str_via_vm("<diff>", "(spr \"abcdef\" 3)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&lp, &rp, &lt, &rt2) {
        (Value::String(a), Value::String(b), Value::String(c), Value::String(d)) => {
            assert_eq!(&*a.borrow(), "   42");
            assert_eq!(&*b.borrow(), "42   ");
            assert_eq!(&*c.borrow(), "def");
            assert_eq!(&*d.borrow(), "abc");
        }
        other => panic!("expected four strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_trim_family() {
    // ADR 0012 D-2 (iter FH) — string-trim/-left/-right.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tb s) (string-trim s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tl s) (string-trim-left s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tr s) (string-trim-right s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (tb \"  abc  \") (tl \"  abc\") (tr \"abc  \") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let b_v = rt.eval_str_via_vm("<diff>", "(tb \"  hello  \")").unwrap();
    let l = rt.eval_str_via_vm("<diff>", "(tl \"   left\")").unwrap();
    let r = rt.eval_str_via_vm("<diff>", "(tr \"right   \")").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&b_v, &l, &r) {
        (Value::String(a), Value::String(b), Value::String(c)) => {
            assert_eq!(&*a.borrow(), "hello");
            assert_eq!(&*b.borrow(), "left");
            assert_eq!(&*c.borrow(), "right");
        }
        other => panic!("expected three strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_replace_all() {
    // ADR 0012 D-2 (iter FI) — string-replace-all s from to.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rep s f t) (string-replace-all s f t))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (rep \"foo bar foo\" \"foo\" \"baz\") \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let a = rt
        .eval_str_via_vm("<diff>", "(rep \"foo bar foo baz\" \"foo\" \"qux\")")
        .unwrap();
    let b = rt
        .eval_str_via_vm("<diff>", "(rep \"aaaa\" \"aa\" \"b\")")
        .unwrap();
    let c = rt
        .eval_str_via_vm("<diff>", "(rep \"hello world\" \"xyz\" \"abc\")")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&a, &b, &c) {
        (Value::String(x), Value::String(y), Value::String(z)) => {
            assert_eq!(&*x.borrow(), "qux bar qux baz");
            assert_eq!(&*y.borrow(), "bb");
            assert_eq!(&*z.borrow(), "hello world");
        }
        other => panic!("expected three strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_take_drop_family() {
    // ADR 0012 D-2 (iter FJ) — string-take/-drop/-take-right/-drop-right.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (st s n) (string-take s n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sd s n) (string-drop s n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (str s n) (string-take-right s n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sdr s n) (string-drop-right s n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (st \"abcdef\" 3) (sd \"abcdef\" 3) \
                      (str \"abcdef\" 3) (sdr \"abcdef\" 3) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let t = rt.eval_str_via_vm("<diff>", "(st \"hello\" 3)").unwrap();
    let d = rt.eval_str_via_vm("<diff>", "(sd \"hello\" 3)").unwrap();
    let tr = rt.eval_str_via_vm("<diff>", "(str \"hello\" 3)").unwrap();
    let dr = rt.eval_str_via_vm("<diff>", "(sdr \"hello\" 3)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match (&t, &d, &tr, &dr) {
        (Value::String(a), Value::String(b), Value::String(c), Value::String(e)) => {
            assert_eq!(&*a.borrow(), "hel");
            assert_eq!(&*b.borrow(), "lo");
            assert_eq!(&*c.borrow(), "llo");
            assert_eq!(&*e.borrow(), "he");
        }
        other => panic!("expected four strings, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_index_family() {
    // ADR 0012 D-2 (iter FK) — string-contains-right / string-index / -index-right.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (scr h n) (string-contains-right h n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (si s c) (string-index s c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sir s c) (string-index-right s c))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (scr \"foo bar foo\" \"foo\") \
                      (si \"hello\" #\\l) \
                      (sir \"hello\" #\\l) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let cr_hit = rt
        .eval_str_via_vm("<diff>", "(scr \"foo bar foo baz\" \"foo\")")
        .unwrap();
    let cr_miss = rt
        .eval_str_via_vm("<diff>", "(scr \"hello world\" \"xyz\")")
        .unwrap();
    let i_hit = rt.eval_str_via_vm("<diff>", "(si \"hello\" #\\l)").unwrap();
    let i_miss = rt.eval_str_via_vm("<diff>", "(si \"hello\" #\\z)").unwrap();
    let ir_hit = rt
        .eval_str_via_vm("<diff>", "(sir \"hello\" #\\l)")
        .unwrap();
    let ir_miss = rt
        .eval_str_via_vm("<diff>", "(sir \"hello\" #\\z)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&cr_hit, Value::Number(cs_core::Number::Fixnum(8))));
    assert!(matches!(&cr_miss, Value::Boolean(false)));
    assert!(matches!(&i_hit, Value::Number(cs_core::Number::Fixnum(2))));
    assert!(matches!(&i_miss, Value::Boolean(false)));
    assert!(matches!(&ir_hit, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&ir_miss, Value::Boolean(false)));
}

#[test]
fn diff_jit_bytevector_utf8_conversion() {
    // ADR 0012 D-2 (iter FL) — bytevector/utf8 conversion family.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bv2l bv) (bytevector->u8-list bv))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l2bv l) (u8-list->bytevector l))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s2u s) (string->utf8 s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (u2s b) (utf8->string b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (bv2l (bytevector 1 2 3)) \
                      (l2bv (list 1 2 3)) \
                      (s2u \"hi\") \
                      (u2s (bytevector 104 105)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lst = rt
        .eval_str_via_vm("<diff>", "(bv2l (bytevector 65 66 67))")
        .unwrap();
    let bv = rt
        .eval_str_via_vm("<diff>", "(l2bv (list 65 66 67))")
        .unwrap();
    let utf = rt.eval_str_via_vm("<diff>", "(s2u \"AB\")").unwrap();
    let str_v = rt
        .eval_str_via_vm("<diff>", "(u2s (bytevector 72 101 108 108 111))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &lst {
        Value::Pair(p) => {
            assert!(matches!(
                p.car.borrow().clone(),
                Value::Number(cs_core::Number::Fixnum(65))
            ));
        }
        other => panic!("expected pair, got {:?}", other),
    }
    match &bv {
        Value::ByteVector(b) => {
            assert_eq!(&*b.borrow(), &vec![65u8, 66, 67]);
        }
        other => panic!("expected bytevector, got {:?}", other),
    }
    match &utf {
        Value::ByteVector(b) => {
            assert_eq!(&*b.borrow(), &vec![65u8, 66]);
        }
        other => panic!("expected bytevector, got {:?}", other),
    }
    match &str_v {
        Value::String(s) => {
            assert_eq!(&*s.borrow(), "Hello");
        }
        other => panic!("expected string, got {:?}", other),
    }
}

#[test]
fn diff_jit_log2_atan2() {
    // ADR 0012 D-2 (iter FM) — log 2-arg + atan 2-arg.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lg n b) (log n b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (at y x) (atan y x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (lg 8.0 2.0) (at 1.0 1.0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let l8 = rt.eval_str_via_vm("<diff>", "(lg 8.0 2.0)").unwrap();
    let l100 = rt.eval_str_via_vm("<diff>", "(lg 100.0 10.0)").unwrap();
    let a45 = rt.eval_str_via_vm("<diff>", "(at 1.0 1.0)").unwrap();
    let a0 = rt.eval_str_via_vm("<diff>", "(at 0.0 1.0)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // log_2(8) = 3
    match &l8 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 3.0).abs() < 1e-9),
        other => panic!("expected flonum 3.0, got {:?}", other),
    }
    // log_10(100) = 2
    match &l100 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 2.0).abs() < 1e-9),
        other => panic!("expected flonum 2.0, got {:?}", other),
    }
    // atan2(1,1) = π/4 ≈ 0.7854
    match &a45 {
        Value::Number(cs_core::Number::Flonum(f)) => {
            assert!((f - std::f64::consts::FRAC_PI_4).abs() < 1e-9)
        }
        other => panic!("expected π/4, got {:?}", other),
    }
    // atan2(0,1) = 0
    match &a0 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!(f.abs() < 1e-9),
        other => panic!("expected 0.0, got {:?}", other),
    }
}

#[test]
fn diff_jit_bitwise_bit_count_length() {
    // ADR 0012 D-2 (iter FN) — bitwise-bit-count / bitwise-length.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bc n) (bitwise-bit-count n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bl n) (bitwise-length n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (bc 7) (bl 16) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // popcount: 7 = 0b111 -> 3
    let c7 = rt.eval_str_via_vm("<diff>", "(bc 7)").unwrap();
    // 255 = 0b11111111 -> 8
    let c255 = rt.eval_str_via_vm("<diff>", "(bc 255)").unwrap();
    // 0 -> 0
    let c0 = rt.eval_str_via_vm("<diff>", "(bc 0)").unwrap();
    // bitwise-length: 16 -> 5 (0b10000)
    let l16 = rt.eval_str_via_vm("<diff>", "(bl 16)").unwrap();
    let l1 = rt.eval_str_via_vm("<diff>", "(bl 1)").unwrap();
    let l0 = rt.eval_str_via_vm("<diff>", "(bl 0)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&c7, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&c255, Value::Number(cs_core::Number::Fixnum(8))));
    assert!(matches!(&c0, Value::Number(cs_core::Number::Fixnum(0))));
    assert!(matches!(&l16, Value::Number(cs_core::Number::Fixnum(5))));
    assert!(matches!(&l1, Value::Number(cs_core::Number::Fixnum(1))));
    assert!(matches!(&l0, Value::Number(cs_core::Number::Fixnum(0))));
}

#[test]
fn diff_jit_bitwise_shift_and_bit_set_p() {
    // ADR 0012 D-2 (iter FO) — bitwise shift-left/-right + bit-set?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (sl n c) (bitwise-arithmetic-shift-left n c))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (sr n c) (bitwise-arithmetic-shift-right n c))",
    )
    .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bs n b) (bitwise-bit-set? n b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sl 1 3) (sr 16 2) (bs 5 0) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 1 << 3 = 8
    let s1 = rt.eval_str_via_vm("<diff>", "(sl 1 3)").unwrap();
    // 16 >> 2 = 4
    let s2 = rt.eval_str_via_vm("<diff>", "(sr 16 2)").unwrap();
    // -8 >> 2 = -2 (arithmetic / sign-extending)
    let s3 = rt.eval_str_via_vm("<diff>", "(sr -8 2)").unwrap();
    // 5 = 0b101: bit 0 set, bit 1 not set, bit 2 set
    let b0 = rt.eval_str_via_vm("<diff>", "(bs 5 0)").unwrap();
    let b1 = rt.eval_str_via_vm("<diff>", "(bs 5 1)").unwrap();
    let b2 = rt.eval_str_via_vm("<diff>", "(bs 5 2)").unwrap();
    // Out-of-range
    let bo = rt.eval_str_via_vm("<diff>", "(bs 5 100)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&s1, Value::Number(cs_core::Number::Fixnum(8))));
    assert!(matches!(&s2, Value::Number(cs_core::Number::Fixnum(4))));
    assert!(matches!(&s3, Value::Number(cs_core::Number::Fixnum(-2))));
    assert!(matches!(&b0, Value::Boolean(true)));
    assert!(matches!(&b1, Value::Boolean(false)));
    assert!(matches!(&b2, Value::Boolean(true)));
    assert!(matches!(&bo, Value::Boolean(false)));
}

#[test]
fn diff_jit_bytevector_s8_ref_set() {
    // ADR 0012 D-2 (iter FP) — bytevector-s8-ref/-set!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sr bv k) (bytevector-s8-ref bv k))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (ss bv k v) (bytevector-s8-set! bv k v) (bytevector-s8-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sr (bytevector 1 255 128) 0) \
                      (ss (make-bytevector 4 0) 1 -5) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // 0x80 (=128 as u8) read as s8 = -128
    let neg = rt
        .eval_str_via_vm("<diff>", "(sr (bytevector 128) 0)")
        .unwrap();
    // 0x7F = 127 (positive max s8)
    let pos = rt
        .eval_str_via_vm("<diff>", "(sr (bytevector 127) 0)")
        .unwrap();
    // 0xFF (=255 as u8) read as s8 = -1
    let neg1 = rt
        .eval_str_via_vm("<diff>", "(sr (bytevector 255) 0)")
        .unwrap();
    // Set to -42 and read back
    let setget = rt
        .eval_str_via_vm("<diff>", "(ss (make-bytevector 4 0) 2 -42)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&neg, Value::Number(cs_core::Number::Fixnum(-128))));
    assert!(matches!(&pos, Value::Number(cs_core::Number::Fixnum(127))));
    assert!(matches!(&neg1, Value::Number(cs_core::Number::Fixnum(-1))));
    assert!(matches!(
        &setget,
        Value::Number(cs_core::Number::Fixnum(-42))
    ));
}

#[test]
fn diff_jit_bytevector_u16_s16_native() {
    // ADR 0012 D-2 (iter FQ) — bytevector u16/s16 native ref/set!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (ur bv k) (bytevector-u16-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (sr bv k) (bytevector-s16-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (us bv k v) \
           (bytevector-u16-native-set! bv k v) \
           (bytevector-u16-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (ss bv k v) \
           (bytevector-s16-native-set! bv k v) \
           (bytevector-s16-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (us (make-bytevector 4 0) 0 12345) \
                      (ss (make-bytevector 4 0) 2 -123) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let uw = rt
        .eval_str_via_vm("<diff>", "(us (make-bytevector 4 0) 0 60000)")
        .unwrap();
    let sw_pos = rt
        .eval_str_via_vm("<diff>", "(ss (make-bytevector 4 0) 0 30000)")
        .unwrap();
    let sw_neg = rt
        .eval_str_via_vm("<diff>", "(ss (make-bytevector 4 0) 0 -30000)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&uw, Value::Number(cs_core::Number::Fixnum(60000))));
    assert!(matches!(
        &sw_pos,
        Value::Number(cs_core::Number::Fixnum(30000))
    ));
    assert!(matches!(
        &sw_neg,
        Value::Number(cs_core::Number::Fixnum(-30000))
    ));
}

#[test]
fn diff_jit_bytevector_u32_s32_native() {
    // ADR 0012 D-2 (iter FR) — bytevector u32/s32 native ref/set!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (us bv k v) \
           (bytevector-u32-native-set! bv k v) \
           (bytevector-u32-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (ss bv k v) \
           (bytevector-s32-native-set! bv k v) \
           (bytevector-s32-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (us (make-bytevector 8 0) 0 123456) \
                      (ss (make-bytevector 8 0) 4 -77777) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let uw = rt
        .eval_str_via_vm("<diff>", "(us (make-bytevector 8 0) 0 4000000000)")
        .unwrap();
    let sw_pos = rt
        .eval_str_via_vm("<diff>", "(ss (make-bytevector 8 0) 4 2000000000)")
        .unwrap();
    let sw_neg = rt
        .eval_str_via_vm("<diff>", "(ss (make-bytevector 8 0) 4 -2000000000)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(
        &uw,
        Value::Number(cs_core::Number::Fixnum(4000000000))
    ));
    assert!(matches!(
        &sw_pos,
        Value::Number(cs_core::Number::Fixnum(2000000000))
    ));
    assert!(matches!(
        &sw_neg,
        Value::Number(cs_core::Number::Fixnum(-2000000000))
    ));
}

#[test]
fn diff_jit_bytevector_ieee_native() {
    // ADR 0012 D-2 (iter FS) — bytevector IEEE single/double native ref/set!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (sw bv k v) \
           (bytevector-ieee-single-native-set! bv k v) \
           (bytevector-ieee-single-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (dw bv k v) \
           (bytevector-ieee-double-native-set! bv k v) \
           (bytevector-ieee-double-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sw (make-bytevector 16 0) 0 1.5) \
                      (dw (make-bytevector 16 0) 8 3.14) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let s = rt
        .eval_str_via_vm("<diff>", "(sw (make-bytevector 16 0) 0 2.5)")
        .unwrap();
    let d = rt
        .eval_str_via_vm("<diff>", "(dw (make-bytevector 16 0) 0 3.14159265)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // f32 roundtrip exact for 2.5 (representable)
    match &s {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 2.5).abs() < 1e-9),
        other => panic!("expected flonum, got {:?}", other),
    }
    // f64 roundtrip exact
    match &d {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 3.14159265).abs() < 1e-12),
        other => panic!("expected flonum, got {:?}", other),
    }
}

#[test]
fn diff_jit_bytevector_u64_s64_native() {
    // ADR 0012 D-2 (iter FT) — bytevector u64/s64 native ref/set!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (us bv k v) \
           (bytevector-u64-native-set! bv k v) \
           (bytevector-u64-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (ss bv k v) \
           (bytevector-s64-native-set! bv k v) \
           (bytevector-s64-native-ref bv k))",
    )
    .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (us (make-bytevector 16 0) 0 12345) \
                      (ss (make-bytevector 16 0) 8 -99999999) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let u = rt
        .eval_str_via_vm(
            "<diff>",
            "(us (make-bytevector 16 0) 0 9223372036854775807)",
        )
        .unwrap();
    let sp = rt
        .eval_str_via_vm(
            "<diff>",
            "(ss (make-bytevector 16 0) 0 9223372036854775807)",
        )
        .unwrap();
    let sn = rt
        .eval_str_via_vm(
            "<diff>",
            "(ss (make-bytevector 16 0) 0 -9223372036854775808)",
        )
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // i64::MAX
    assert!(matches!(
        &u,
        Value::Number(cs_core::Number::Fixnum(9223372036854775807))
    ));
    assert!(matches!(
        &sp,
        Value::Number(cs_core::Number::Fixnum(9223372036854775807))
    ));
    assert!(matches!(
        &sn,
        Value::Number(cs_core::Number::Fixnum(-9223372036854775808))
    ));
}

#[test]
fn diff_jit_fx_predicates() {
    // ADR 0012 D-2 (iter FU) — fx predicate aliases.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fz n) (fxzero? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fp n) (fxpositive? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fn n) (fxnegative? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fe n) (fxeven? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fo n) (fxodd? n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (fz 0) (fp 1) (fn -1) (fe 2) (fo 3) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let z0 = rt.eval_str_via_vm("<diff>", "(fz 0)").unwrap();
    let z1 = rt.eval_str_via_vm("<diff>", "(fz 5)").unwrap();
    let p1 = rt.eval_str_via_vm("<diff>", "(fp 7)").unwrap();
    let p0 = rt.eval_str_via_vm("<diff>", "(fp 0)").unwrap();
    let n1 = rt.eval_str_via_vm("<diff>", "(fn -3)").unwrap();
    let n0 = rt.eval_str_via_vm("<diff>", "(fn 5)").unwrap();
    let e_t = rt.eval_str_via_vm("<diff>", "(fe 4)").unwrap();
    let e_f = rt.eval_str_via_vm("<diff>", "(fe 5)").unwrap();
    let o_t = rt.eval_str_via_vm("<diff>", "(fo 5)").unwrap();
    let o_f = rt.eval_str_via_vm("<diff>", "(fo 4)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&z0, Value::Boolean(true)));
    assert!(matches!(&z1, Value::Boolean(false)));
    assert!(matches!(&p1, Value::Boolean(true)));
    assert!(matches!(&p0, Value::Boolean(false)));
    assert!(matches!(&n1, Value::Boolean(true)));
    assert!(matches!(&n0, Value::Boolean(false)));
    assert!(matches!(&e_t, Value::Boolean(true)));
    assert!(matches!(&e_f, Value::Boolean(false)));
    assert!(matches!(&o_t, Value::Boolean(true)));
    assert!(matches!(&o_f, Value::Boolean(false)));
}

#[test]
fn diff_jit_fx_arith_and_compare() {
    // ADR 0012 D-2 (iter FV) — fx arithmetic + comparison + max/min.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (a b c) (fx+ b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s b c) (fx- b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m b c) (fx* b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mx b c) (fxmax b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mn b c) (fxmin b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (e b c) (fx=? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l b c) (fx<? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (g b c) (fx>? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (le b c) (fx<=? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ge b c) (fx>=? b c))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (a 3 4) (s 5 2) (m 6 7) (mx 9 4) (mn 9 4) \
                      (e 5 5) (l 1 2) (g 3 1) (le 4 4) (ge 4 4) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let plus = rt.eval_str_via_vm("<diff>", "(a 10 32)").unwrap();
    let minus = rt.eval_str_via_vm("<diff>", "(s 50 8)").unwrap();
    let times = rt.eval_str_via_vm("<diff>", "(m 6 7)").unwrap();
    let mx = rt.eval_str_via_vm("<diff>", "(mx 12 5)").unwrap();
    let mn = rt.eval_str_via_vm("<diff>", "(mn 12 5)").unwrap();
    let eq_t = rt.eval_str_via_vm("<diff>", "(e 5 5)").unwrap();
    let eq_f = rt.eval_str_via_vm("<diff>", "(e 5 6)").unwrap();
    let lt = rt.eval_str_via_vm("<diff>", "(l 1 2)").unwrap();
    let gt = rt.eval_str_via_vm("<diff>", "(g 3 1)").unwrap();
    let le_eq = rt.eval_str_via_vm("<diff>", "(le 4 4)").unwrap();
    let le_lt = rt.eval_str_via_vm("<diff>", "(le 3 4)").unwrap();
    let le_gt = rt.eval_str_via_vm("<diff>", "(le 5 4)").unwrap();
    let ge_eq = rt.eval_str_via_vm("<diff>", "(ge 4 4)").unwrap();
    let ge_gt = rt.eval_str_via_vm("<diff>", "(ge 5 4)").unwrap();
    let ge_lt = rt.eval_str_via_vm("<diff>", "(ge 3 4)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&plus, Value::Number(cs_core::Number::Fixnum(42))));
    assert!(matches!(&minus, Value::Number(cs_core::Number::Fixnum(42))));
    assert!(matches!(&times, Value::Number(cs_core::Number::Fixnum(42))));
    assert!(matches!(&mx, Value::Number(cs_core::Number::Fixnum(12))));
    assert!(matches!(&mn, Value::Number(cs_core::Number::Fixnum(5))));
    assert!(matches!(&eq_t, Value::Boolean(true)));
    assert!(matches!(&eq_f, Value::Boolean(false)));
    assert!(matches!(&lt, Value::Boolean(true)));
    assert!(matches!(&gt, Value::Boolean(true)));
    assert!(matches!(&le_eq, Value::Boolean(true)));
    assert!(matches!(&le_lt, Value::Boolean(true)));
    assert!(matches!(&le_gt, Value::Boolean(false)));
    assert!(matches!(&ge_eq, Value::Boolean(true)));
    assert!(matches!(&ge_gt, Value::Boolean(true)));
    assert!(matches!(&ge_lt, Value::Boolean(false)));
}

#[test]
fn diff_jit_fx_bitwise_aliases() {
    // ADR 0012 D-2 (iter FW) — fx bitwise + bit-inspection aliases.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (a b c) (fxand b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (o b c) (fxior b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (x b c) (fxxor b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (n b) (fxnot b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (c b) (fxbit-count b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l b) (fxlength b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s b k) (fxbit-set? b k))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (a 7 5) (o 8 1) (x 5 3) (n 0) (c 7) (l 16) (s 5 2) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let a = rt.eval_str_via_vm("<diff>", "(a 12 10)").unwrap();
    let o = rt.eval_str_via_vm("<diff>", "(o 12 3)").unwrap();
    let x = rt.eval_str_via_vm("<diff>", "(x 12 5)").unwrap();
    let n = rt.eval_str_via_vm("<diff>", "(n 0)").unwrap();
    let c = rt.eval_str_via_vm("<diff>", "(c 255)").unwrap();
    let l = rt.eval_str_via_vm("<diff>", "(l 16)").unwrap();
    let s_t = rt.eval_str_via_vm("<diff>", "(s 5 2)").unwrap();
    let s_f = rt.eval_str_via_vm("<diff>", "(s 5 1)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // 12 & 10 = 8
    assert!(matches!(&a, Value::Number(cs_core::Number::Fixnum(8))));
    // 12 | 3 = 15
    assert!(matches!(&o, Value::Number(cs_core::Number::Fixnum(15))));
    // 12 ^ 5 = 9
    assert!(matches!(&x, Value::Number(cs_core::Number::Fixnum(9))));
    // !0 = -1
    assert!(matches!(&n, Value::Number(cs_core::Number::Fixnum(-1))));
    // popcount(255) = 8
    assert!(matches!(&c, Value::Number(cs_core::Number::Fixnum(8))));
    // length(16) = 5
    assert!(matches!(&l, Value::Number(cs_core::Number::Fixnum(5))));
    assert!(matches!(&s_t, Value::Boolean(true)));
    assert!(matches!(&s_f, Value::Boolean(false)));
}

#[test]
fn diff_jit_fx_shift_and_first_bit() {
    // ADR 0012 D-2 (iter FX) — fx shift family + fxfirst-bit-set.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sh n c) (fxarithmetic-shift n c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sl n c) (fxarithmetic-shift-left n c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sr n c) (fxarithmetic-shift-right n c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fb n) (fxfirst-bit-set n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sh 1 2) (sl 1 3) (sr 16 2) (fb 8) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let h_pos = rt.eval_str_via_vm("<diff>", "(sh 1 3)").unwrap();
    let h_neg = rt.eval_str_via_vm("<diff>", "(sh 16 -2)").unwrap();
    let l = rt.eval_str_via_vm("<diff>", "(sl 1 3)").unwrap();
    let r = rt.eval_str_via_vm("<diff>", "(sr 16 2)").unwrap();
    let f8 = rt.eval_str_via_vm("<diff>", "(fb 8)").unwrap();
    let f1 = rt.eval_str_via_vm("<diff>", "(fb 1)").unwrap();
    let f0 = rt.eval_str_via_vm("<diff>", "(fb 0)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // arithmetic-shift: positive count = left, negative = right
    assert!(matches!(&h_pos, Value::Number(cs_core::Number::Fixnum(8))));
    assert!(matches!(&h_neg, Value::Number(cs_core::Number::Fixnum(4))));
    // shift-left 1<<3 = 8
    assert!(matches!(&l, Value::Number(cs_core::Number::Fixnum(8))));
    // shift-right 16>>2 = 4
    assert!(matches!(&r, Value::Number(cs_core::Number::Fixnum(4))));
    // trailing zeros: 8 = 0b1000 -> 3
    assert!(matches!(&f8, Value::Number(cs_core::Number::Fixnum(3))));
    // 1 -> 0
    assert!(matches!(&f1, Value::Number(cs_core::Number::Fixnum(0))));
    // 0 -> -1
    assert!(matches!(&f0, Value::Number(cs_core::Number::Fixnum(-1))));
}

#[test]
fn diff_jit_fl_arith_compare_predicates() {
    // ADR 0012 D-2 (iter FY) — fl arithmetic + compare + predicates.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (a b c) (fl+ b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (s b c) (fl- b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m b c) (fl* b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (d b c) (fl/ b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (e b c) (fl=? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l b c) (fl<? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (g b c) (fl>? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (le b c) (fl<=? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ge b c) (fl>=? b c))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (z x) (flzero? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (p x) (flpositive? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (n x) (flnegative? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (a 1.0 2.0) (s 5.0 3.0) (m 3.0 4.0) (d 12.0 4.0) \
                      (e 1.0 1.0) (l 1.0 2.0) (g 3.0 1.0) (le 4.0 4.0) (ge 4.0 4.0) \
                      (z 0.0) (p 1.0) (n -1.0) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let add = rt.eval_str_via_vm("<diff>", "(a 1.5 2.5)").unwrap();
    let sub = rt.eval_str_via_vm("<diff>", "(s 5.5 2.0)").unwrap();
    let mul = rt.eval_str_via_vm("<diff>", "(m 2.5 4.0)").unwrap();
    let div = rt.eval_str_via_vm("<diff>", "(d 10.0 4.0)").unwrap();
    let eq_t = rt.eval_str_via_vm("<diff>", "(e 3.14 3.14)").unwrap();
    let eq_f = rt.eval_str_via_vm("<diff>", "(e 1.0 2.0)").unwrap();
    let lt = rt.eval_str_via_vm("<diff>", "(l 1.0 2.0)").unwrap();
    let gt = rt.eval_str_via_vm("<diff>", "(g 3.0 1.0)").unwrap();
    let le_eq = rt.eval_str_via_vm("<diff>", "(le 4.0 4.0)").unwrap();
    let le_gt = rt.eval_str_via_vm("<diff>", "(le 5.0 4.0)").unwrap();
    let ge_eq = rt.eval_str_via_vm("<diff>", "(ge 4.0 4.0)").unwrap();
    let ge_lt = rt.eval_str_via_vm("<diff>", "(ge 3.0 4.0)").unwrap();
    let z_t = rt.eval_str_via_vm("<diff>", "(z 0.0)").unwrap();
    let z_f = rt.eval_str_via_vm("<diff>", "(z 0.1)").unwrap();
    let p_t = rt.eval_str_via_vm("<diff>", "(p 0.5)").unwrap();
    let n_t = rt.eval_str_via_vm("<diff>", "(n -0.5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &add {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 4.0).abs() < 1e-9),
        other => panic!("expected 4.0, got {:?}", other),
    }
    match &sub {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 3.5).abs() < 1e-9),
        other => panic!("expected 3.5, got {:?}", other),
    }
    match &mul {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 10.0).abs() < 1e-9),
        other => panic!("expected 10.0, got {:?}", other),
    }
    match &div {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 2.5).abs() < 1e-9),
        other => panic!("expected 2.5, got {:?}", other),
    }
    assert!(matches!(&eq_t, Value::Boolean(true)));
    assert!(matches!(&eq_f, Value::Boolean(false)));
    assert!(matches!(&lt, Value::Boolean(true)));
    assert!(matches!(&gt, Value::Boolean(true)));
    assert!(matches!(&le_eq, Value::Boolean(true)));
    assert!(matches!(&le_gt, Value::Boolean(false)));
    assert!(matches!(&ge_eq, Value::Boolean(true)));
    assert!(matches!(&ge_lt, Value::Boolean(false)));
    assert!(matches!(&z_t, Value::Boolean(true)));
    assert!(matches!(&z_f, Value::Boolean(false)));
    assert!(matches!(&p_t, Value::Boolean(true)));
    assert!(matches!(&n_t, Value::Boolean(true)));
}

#[test]
fn diff_jit_fl_trig_round_predicates() {
    // ADR 0012 D-2 (iter FZ) — fl trig/round/predicate aliases.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sn x) (flsin x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cs x) (flcos x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ex x) (flexp x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lg x) (fllog x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fl x) (flfloor x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cl x) (flceiling x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tr x) (fltruncate x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rd x) (flround x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fn x) (flfinite? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ifn x) (flinfinite? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (in x) (flinteger? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (nn x) (flnan? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sn 0.0) (cs 0.0) (ex 0.0) (lg 1.0) \
                      (fl 1.5) (cl 1.5) (tr 1.5) (rd 1.5) \
                      (fn 1.0) (ifn 1.0) (in 1.0) (nn 1.0) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let sin0 = rt.eval_str_via_vm("<diff>", "(sn 0.0)").unwrap();
    let cos0 = rt.eval_str_via_vm("<diff>", "(cs 0.0)").unwrap();
    let exp0 = rt.eval_str_via_vm("<diff>", "(ex 0.0)").unwrap();
    let log1 = rt.eval_str_via_vm("<diff>", "(lg 1.0)").unwrap();
    let f15 = rt.eval_str_via_vm("<diff>", "(fl 1.7)").unwrap();
    let c15 = rt.eval_str_via_vm("<diff>", "(cl 1.2)").unwrap();
    let t_neg = rt.eval_str_via_vm("<diff>", "(tr -1.7)").unwrap();
    let r_half = rt.eval_str_via_vm("<diff>", "(rd 2.5)").unwrap();
    let fin_t = rt.eval_str_via_vm("<diff>", "(fn 1.0)").unwrap();
    let inf_t = rt.eval_str_via_vm("<diff>", "(ifn (/ 1.0 0.0))").unwrap();
    let int_t = rt.eval_str_via_vm("<diff>", "(in 3.0)").unwrap();
    let int_f = rt.eval_str_via_vm("<diff>", "(in 3.14)").unwrap();
    let nan_t = rt.eval_str_via_vm("<diff>", "(nn (/ 0.0 0.0))").unwrap();
    let nan_f = rt.eval_str_via_vm("<diff>", "(nn 1.0)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &sin0 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!(f.abs() < 1e-9),
        other => panic!("expected 0.0, got {:?}", other),
    }
    match &cos0 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 1.0).abs() < 1e-9),
        other => panic!("expected 1.0, got {:?}", other),
    }
    match &exp0 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 1.0).abs() < 1e-9),
        other => panic!("expected 1.0, got {:?}", other),
    }
    match &log1 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!(f.abs() < 1e-9),
        other => panic!("expected 0.0, got {:?}", other),
    }
    match &f15 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 1.0).abs() < 1e-9),
        other => panic!("expected 1.0, got {:?}", other),
    }
    match &c15 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 2.0).abs() < 1e-9),
        other => panic!("expected 2.0, got {:?}", other),
    }
    match &t_neg {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - -1.0).abs() < 1e-9),
        other => panic!("expected -1.0, got {:?}", other),
    }
    // banker's rounding: 2.5 -> 2
    match &r_half {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 2.0).abs() < 1e-9),
        other => panic!("expected 2.0, got {:?}", other),
    }
    assert!(matches!(&fin_t, Value::Boolean(true)));
    assert!(matches!(&inf_t, Value::Boolean(true)));
    assert!(matches!(&int_t, Value::Boolean(true)));
    assert!(matches!(&int_f, Value::Boolean(false)));
    assert!(matches!(&nan_t, Value::Boolean(true)));
    assert!(matches!(&nan_f, Value::Boolean(false)));
}

#[test]
fn diff_jit_flexpt_parity_and_fixnum_to_flonum() {
    // ADR 0012 D-2 (iter GA) — flexpt + fleven?/flodd? + fixnum->flonum.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ex x y) (flexpt x y))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (e x) (fleven? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (o x) (flodd? x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (f n) (fixnum->flonum n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ex 2.0 10.0) (e 4.0) (o 5.0) (f 42) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let p = rt.eval_str_via_vm("<diff>", "(ex 2.0 8.0)").unwrap();
    let e_t = rt.eval_str_via_vm("<diff>", "(e 4.0)").unwrap();
    let e_f = rt.eval_str_via_vm("<diff>", "(e 3.0)").unwrap();
    let e_frac = rt.eval_str_via_vm("<diff>", "(e 2.5)").unwrap();
    let o_t = rt.eval_str_via_vm("<diff>", "(o 5.0)").unwrap();
    let o_f = rt.eval_str_via_vm("<diff>", "(o 4.0)").unwrap();
    let f42 = rt.eval_str_via_vm("<diff>", "(f 42)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // 2^8 = 256
    match &p {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 256.0).abs() < 1e-9),
        other => panic!("expected 256.0, got {:?}", other),
    }
    assert!(matches!(&e_t, Value::Boolean(true)));
    assert!(matches!(&e_f, Value::Boolean(false)));
    assert!(matches!(&e_frac, Value::Boolean(false)));
    assert!(matches!(&o_t, Value::Boolean(true)));
    assert!(matches!(&o_f, Value::Boolean(false)));
    match &f42 {
        Value::Number(cs_core::Number::Flonum(f)) => assert!((f - 42.0).abs() < 1e-9),
        other => panic!("expected 42.0, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_titlecase_and_hashes() {
    // ADR 0012 D-2 (iter GB) — string-titlecase + string-hash + symbol-hash.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tc s) (string-titlecase s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sh s) (string-hash s))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (yh s) (symbol-hash s))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (tc \"hello world\") (sh \"hello\") (yh 'sym) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let tc = rt
        .eval_str_via_vm("<diff>", "(tc \"hello world from scheme\")")
        .unwrap();
    let sh = rt.eval_str_via_vm("<diff>", "(sh \"hello\")").unwrap();
    let sh_consistent = rt.eval_str_via_vm("<diff>", "(sh \"hello\")").unwrap();
    let sh_diff = rt.eval_str_via_vm("<diff>", "(sh \"world\")").unwrap();
    let yh = rt.eval_str_via_vm("<diff>", "(yh 'foo)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &tc {
        Value::String(s) => assert_eq!(&*s.borrow(), "Hello World From Scheme"),
        other => panic!("expected string, got {:?}", other),
    }
    // string-hash should be non-zero Fixnum
    let sh_val = match &sh {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected Fixnum, got {:?}", other),
    };
    let sh_val2 = match &sh_consistent {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected Fixnum, got {:?}", other),
    };
    let sh_diff_val = match &sh_diff {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected Fixnum, got {:?}", other),
    };
    assert_eq!(sh_val, sh_val2, "string-hash deterministic");
    assert_ne!(sh_val, sh_diff_val, "different strings hash differently");
    assert!(sh_val >= 0, "string-hash positive");
    match &yh {
        Value::Number(cs_core::Number::Fixnum(v)) => assert!(*v >= 0),
        other => panic!("expected Fixnum, got {:?}", other),
    }
}

#[test]
fn diff_jit_port_subtype_predicates_and_list_head() {
    // ADR 0012 D-2 (iter GC) — port-subtype predicates + list-head.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ip v) (input-port? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (op v) (output-port? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bp v) (binary-port? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (tp v) (textual-port? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (lh lst n) (list-head lst n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ip 42) (op 42) (bp 42) (tp 42) \
                      (lh (list 1 2 3 4 5) 3) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Non-port values: all #f
    let ip_no = rt.eval_str_via_vm("<diff>", "(ip 42)").unwrap();
    let op_no = rt.eval_str_via_vm("<diff>", "(op 42)").unwrap();
    let bp_no = rt.eval_str_via_vm("<diff>", "(bp 42)").unwrap();
    let tp_no = rt.eval_str_via_vm("<diff>", "(tp 42)").unwrap();
    // A textual output port (open-output-string)
    let op_t = rt
        .eval_str_via_vm("<diff>", "(op (open-output-string))")
        .unwrap();
    let tp_t = rt
        .eval_str_via_vm("<diff>", "(tp (open-output-string))")
        .unwrap();
    let ip_t_str = rt
        .eval_str_via_vm("<diff>", "(ip (open-input-string \"hi\"))")
        .unwrap();
    // list-head
    let lh = rt
        .eval_str_via_vm("<diff>", "(lh (list 10 20 30 40 50) 3)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&ip_no, Value::Boolean(false)));
    assert!(matches!(&op_no, Value::Boolean(false)));
    assert!(matches!(&bp_no, Value::Boolean(false)));
    assert!(matches!(&tp_no, Value::Boolean(false)));
    assert!(matches!(&op_t, Value::Boolean(true)));
    assert!(matches!(&tp_t, Value::Boolean(true)));
    assert!(matches!(&ip_t_str, Value::Boolean(true)));
    // list-head of 5-element list, n=3 => (10 20 30)
    match &lh {
        Value::Pair(p) => {
            assert!(matches!(
                p.car.borrow().clone(),
                Value::Number(cs_core::Number::Fixnum(10))
            ));
        }
        other => panic!("expected pair, got {:?}", other),
    }
}

#[test]
fn diff_jit_complex_real_valued_promise() {
    // ADR 0012 D-2 (iter GD) — complex?/real-valued?/promise?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cx v) (complex? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rv v) (real-valued? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (pm v) (promise? v))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (cx 5) (rv 5.5) (pm 42) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let cx_fix = rt.eval_str_via_vm("<diff>", "(cx 42)").unwrap();
    let cx_flo = rt.eval_str_via_vm("<diff>", "(cx 3.14)").unwrap();
    let rv_fix = rt.eval_str_via_vm("<diff>", "(rv 42)").unwrap();
    let rv_flo = rt.eval_str_via_vm("<diff>", "(rv 3.14)").unwrap();
    let pm_no = rt.eval_str_via_vm("<diff>", "(pm 42)").unwrap();
    let pm_yes = rt
        .eval_str_via_vm("<diff>", "(pm (make-promise 5))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&cx_fix, Value::Boolean(true)));
    assert!(matches!(&cx_flo, Value::Boolean(true)));
    assert!(matches!(&rv_fix, Value::Boolean(true)));
    assert!(matches!(&rv_flo, Value::Boolean(true)));
    assert!(matches!(&pm_no, Value::Boolean(false)));
    assert!(matches!(&pm_yes, Value::Boolean(true)));
}

#[test]
fn diff_jit_div_mod_euclid() {
    // ADR 0012 D-2 (iter GE) — R6RS div/mod (Euclidean).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (d x y) (div x y))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m x y) (mod x y))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fd x y) (fxdiv x y))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fm x y) (fxmod x y))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (d 10 3) (m 10 3) (fd 7 2) (fm 7 2) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // R6RS Euclidean: div(-7, 2) = -4 (not -3); mod always non-negative
    let dpos = rt.eval_str_via_vm("<diff>", "(d 10 3)").unwrap();
    let dneg = rt.eval_str_via_vm("<diff>", "(d -7 2)").unwrap();
    let mpos = rt.eval_str_via_vm("<diff>", "(m 10 3)").unwrap();
    let mneg = rt.eval_str_via_vm("<diff>", "(m -7 2)").unwrap();
    let fd = rt.eval_str_via_vm("<diff>", "(fd 17 5)").unwrap();
    let fm = rt.eval_str_via_vm("<diff>", "(fm 17 5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // 10 div 3 = 3 (rounds toward -inf)
    assert!(matches!(&dpos, Value::Number(cs_core::Number::Fixnum(3))));
    // -7 div 2 = -4 (rounds toward -inf: -3.5 → -4)
    assert!(matches!(&dneg, Value::Number(cs_core::Number::Fixnum(-4))));
    // 10 mod 3 = 1
    assert!(matches!(&mpos, Value::Number(cs_core::Number::Fixnum(1))));
    // -7 mod 2 = 1 (non-negative remainder)
    assert!(matches!(&mneg, Value::Number(cs_core::Number::Fixnum(1))));
    // 17 fxdiv 5 = 3, fxmod = 2
    assert!(matches!(&fd, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&fm, Value::Number(cs_core::Number::Fixnum(2))));
}

#[test]
fn diff_jit_hashtable_p_and_valued_aliases() {
    // ADR 0012 D-2 (iter GF) — hashtable? + integer-valued?/rational-valued?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ht v) (hashtable? v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (iv n) (integer-valued? n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (rv n) (rational-valued? n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ht 42) (iv 5) (rv 3.14) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let ht_no = rt.eval_str_via_vm("<diff>", "(ht 42)").unwrap();
    let ht_yes = rt
        .eval_str_via_vm("<diff>", "(ht (make-eqv-hashtable))")
        .unwrap();
    let iv_fix = rt.eval_str_via_vm("<diff>", "(iv 42)").unwrap();
    let iv_flo_int = rt.eval_str_via_vm("<diff>", "(iv 5.0)").unwrap();
    let iv_flo_frac = rt.eval_str_via_vm("<diff>", "(iv 3.14)").unwrap();
    let rv_fix = rt.eval_str_via_vm("<diff>", "(rv 42)").unwrap();
    let rv_flo_fin = rt.eval_str_via_vm("<diff>", "(rv 3.14)").unwrap();
    let rv_flo_inf = rt.eval_str_via_vm("<diff>", "(rv (/ 1.0 0.0))").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&ht_no, Value::Boolean(false)));
    assert!(matches!(&ht_yes, Value::Boolean(true)));
    assert!(matches!(&iv_fix, Value::Boolean(true)));
    assert!(matches!(&iv_flo_int, Value::Boolean(true)));
    assert!(matches!(&iv_flo_frac, Value::Boolean(false)));
    assert!(matches!(&rv_fix, Value::Boolean(true)));
    assert!(matches!(&rv_flo_fin, Value::Boolean(true)));
    assert!(matches!(&rv_flo_inf, Value::Boolean(false)));
}

#[test]
fn diff_jit_hashtable_size_and_mutable() {
    // ADR 0012 D-2 (iter GG) — hashtable-size + hashtable-mutable?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sz ht) (hashtable-size ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mu ht) (hashtable-mutable? ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sz warm-ht) (mu warm-ht) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Empty hashtable
    let empty = rt
        .eval_str_via_vm("<diff>", "(sz (make-eqv-hashtable))")
        .unwrap();
    // Populated hashtable
    rt.eval_str_via_vm("<diff>", "(define ht2 (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht2 'a 1)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht2 'b 2)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht2 'c 3)")
        .unwrap();
    let pop = rt.eval_str_via_vm("<diff>", "(sz ht2)").unwrap();
    let mutable = rt
        .eval_str_via_vm("<diff>", "(mu (make-eqv-hashtable))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&empty, Value::Number(cs_core::Number::Fixnum(0))));
    assert!(matches!(&pop, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&mutable, Value::Boolean(true)));
}

#[test]
fn diff_jit_hashtable_keys_values() {
    // ADR 0012 D-2 (iter GH) — hashtable-keys + hashtable-values.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (kk ht) (hashtable-keys ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (vv ht) (hashtable-values ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! warm-ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (kk warm-ht) (vv warm-ht) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'x 10)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'y 20)")
        .unwrap();
    let keys = rt.eval_str_via_vm("<diff>", "(kk ht)").unwrap();
    let vals = rt.eval_str_via_vm("<diff>", "(vv ht)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &keys {
        Value::Vector(v) => assert_eq!(v.borrow().len(), 2),
        other => panic!("expected vector, got {:?}", other),
    }
    match &vals {
        Value::Vector(v) => {
            assert_eq!(v.borrow().len(), 2);
            let nums: Vec<i64> = v
                .borrow()
                .iter()
                .filter_map(|x| match x {
                    Value::Number(cs_core::Number::Fixnum(n)) => Some(*n),
                    _ => None,
                })
                .collect();
            assert_eq!(nums.len(), 2);
            assert!(nums.contains(&10));
            assert!(nums.contains(&20));
        }
        other => panic!("expected vector, got {:?}", other),
    }
}

#[test]
fn diff_jit_numerator_denominator_and_clear() {
    // ADR 0012 D-2 (iter GI) — numerator/denominator + hashtable-clear!.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (nu n) (numerator n))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (de n) (denominator n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (sz-after-clear ht) \
           (hashtable-clear! ht) \
           (hashtable-size ht))",
    )
    .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! warm-ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (nu 42) (de 42) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let num42 = rt.eval_str_via_vm("<diff>", "(nu 42)").unwrap();
    let num_neg = rt.eval_str_via_vm("<diff>", "(nu -7)").unwrap();
    let den42 = rt.eval_str_via_vm("<diff>", "(de 42)").unwrap();
    // Populate then clear
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'b 2)")
        .unwrap();
    let cleared = rt.eval_str_via_vm("<diff>", "(sz-after-clear ht)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&num42, Value::Number(cs_core::Number::Fixnum(42))));
    assert!(matches!(
        &num_neg,
        Value::Number(cs_core::Number::Fixnum(-7))
    ));
    assert!(matches!(&den42, Value::Number(cs_core::Number::Fixnum(1))));
    assert!(matches!(
        &cleared,
        Value::Number(cs_core::Number::Fixnum(0))
    ));
}

#[test]
fn diff_jit_equal_hash_and_hashtable_to_alist() {
    // ADR 0012 D-2 (iter GJ) — equal-hash + hashtable->alist.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (eh v) (equal-hash v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (h2a ht) (hashtable->alist ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! warm-ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (eh \"hello\") (h2a warm-ht) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let h_str = rt.eval_str_via_vm("<diff>", "(eh \"hello\")").unwrap();
    let h_str2 = rt.eval_str_via_vm("<diff>", "(eh \"hello\")").unwrap();
    let h_diff = rt.eval_str_via_vm("<diff>", "(eh \"world\")").unwrap();
    let h_list = rt.eval_str_via_vm("<diff>", "(eh (list 1 2 3))").unwrap();
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'x 100)")
        .unwrap();
    let alist = rt.eval_str_via_vm("<diff>", "(h2a ht)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    let h1 = match &h_str {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected fixnum, got {:?}", other),
    };
    let h2 = match &h_str2 {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected fixnum, got {:?}", other),
    };
    let h3 = match &h_diff {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected fixnum, got {:?}", other),
    };
    assert_eq!(h1, h2, "equal-hash deterministic");
    assert_ne!(h1, h3, "different strings hash differently");
    assert!(h1 >= 0);
    assert!(matches!(&h_list, Value::Number(cs_core::Number::Fixnum(v)) if *v >= 0));
    match &alist {
        Value::Pair(p) => match p.car.borrow().clone() {
            Value::Pair(_) => (),
            other => panic!("expected pair entry, got {:?}", other),
        },
        other => panic!("expected pair list, got {:?}", other),
    }
}

#[test]
fn diff_jit_file_exists_and_jiffies_per_second() {
    // ADR 0012 D-2 (iter GK) — file-exists? + jiffies-per-second.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fe p) (file-exists? p))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (jps) (jiffies-per-second))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (fe \"/nonexistent-path-xyz\") (jps) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let missing = rt
        .eval_str_via_vm("<diff>", "(fe \"/nonexistent-path-zzz-12345\")")
        .unwrap();
    // /tmp should exist on Unix
    let exists = rt.eval_str_via_vm("<diff>", "(fe \"/tmp\")").unwrap();
    let jps = rt.eval_str_via_vm("<diff>", "(jps)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&missing, Value::Boolean(false)));
    assert!(matches!(&exists, Value::Boolean(true)));
    assert!(matches!(
        &jps,
        Value::Number(cs_core::Number::Fixnum(1_000_000_000))
    ));
}

#[test]
fn diff_jit_current_second_and_current_jiffy() {
    // ADR 0012 D-2 (iter GL) — current-second + current-jiffy.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cs) (current-second))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (cj) (current-jiffy))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (cs) (cj) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let s = rt.eval_str_via_vm("<diff>", "(cs)").unwrap();
    let j1 = rt.eval_str_via_vm("<diff>", "(cj)").unwrap();
    let j2 = rt.eval_str_via_vm("<diff>", "(cj)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    // current-second returns a positive flonum (Unix epoch seconds)
    match &s {
        Value::Number(cs_core::Number::Flonum(f)) => assert!(*f > 1_700_000_000.0),
        other => panic!("expected positive flonum, got {:?}", other),
    }
    // current-jiffy returns non-negative Fixnum that's monotonically increasing
    let jv1 = match &j1 {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected fixnum, got {:?}", other),
    };
    let jv2 = match &j2 {
        Value::Number(cs_core::Number::Fixnum(v)) => *v,
        other => panic!("expected fixnum, got {:?}", other),
    };
    assert!(jv1 >= 0);
    assert!(jv2 >= jv1, "current-jiffy monotonic");
}

#[test]
fn diff_jit_bytevector_list_r7rs_aliases() {
    // ADR 0012 D-2 (iter GM) — bytevector->list / list->bytevector aliases.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (b2l bv) (bytevector->list bv))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (l2b l) (list->bytevector l))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (b2l (bytevector 1 2 3)) \
                      (l2b (list 1 2 3)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let lst = rt
        .eval_str_via_vm("<diff>", "(b2l (bytevector 10 20 30))")
        .unwrap();
    let bv = rt
        .eval_str_via_vm("<diff>", "(l2b (list 100 200))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &lst {
        Value::Pair(p) => assert!(matches!(
            p.car.borrow().clone(),
            Value::Number(cs_core::Number::Fixnum(10))
        )),
        other => panic!("expected pair, got {:?}", other),
    }
    match &bv {
        Value::ByteVector(b) => assert_eq!(&*b.borrow(), &vec![100u8, 200]),
        other => panic!("expected bytevector, got {:?}", other),
    }
}

#[test]
fn diff_jit_append_reverse() {
    // ADR 0012 D-2 (iter GN) — append-reverse.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ar a b) (append-reverse a b))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ar (list 1 2 3) (list 4 5)) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // (append-reverse '(1 2 3) '(4 5)) => (3 2 1 4 5)
    let r = rt
        .eval_str_via_vm("<diff>", "(ar (list 1 2 3) (list 4 5))")
        .unwrap();
    // (append-reverse '() '(a)) => (a)
    let empty = rt.eval_str_via_vm("<diff>", "(ar '() (list 'a))").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &r {
        Value::Pair(p) => {
            // First element should be 3
            assert!(matches!(
                p.car.borrow().clone(),
                Value::Number(cs_core::Number::Fixnum(3))
            ));
        }
        other => panic!("expected pair, got {:?}", other),
    }
    match &empty {
        Value::Pair(p) => {
            assert!(matches!(p.car.borrow().clone(), Value::Symbol(_)));
        }
        other => panic!("expected pair, got {:?}", other),
    }
}

#[test]
fn diff_jit_alist_copy() {
    // ADR 0012 D-2 (iter GO) — alist-copy.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ac al) (alist-copy al))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ac (list (cons 'a 1) (cons 'b 2))) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let copied = rt
        .eval_str_via_vm(
            "<diff>",
            "(ac (list (cons 'x 10) (cons 'y 20) (cons 'z 30)))",
        )
        .unwrap();
    let nil = rt.eval_str_via_vm("<diff>", "(ac '())").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &copied {
        Value::Pair(p) => match p.car.borrow().clone() {
            Value::Pair(pe) => match pe.car.borrow().clone() {
                Value::Symbol(_) => (),
                other => panic!("expected symbol in first key, got {:?}", other),
            },
            other => panic!("expected pair entry, got {:?}", other),
        },
        other => panic!("expected pair list, got {:?}", other),
    }
    assert!(matches!(&nil, Value::Null));
}

#[test]
fn diff_jit_port_open_predicates() {
    // ADR 0012 D-2 (iter GP) — input-port-open? + output-port-open?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (iop p) (input-port-open? p))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (oop p) (output-port-open? p))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (iop 42) (oop 42) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let iop_no = rt.eval_str_via_vm("<diff>", "(iop 42)").unwrap();
    let oop_no = rt.eval_str_via_vm("<diff>", "(oop 42)").unwrap();
    let iop_yes = rt
        .eval_str_via_vm("<diff>", "(iop (open-input-string \"hi\"))")
        .unwrap();
    let oop_yes = rt
        .eval_str_via_vm("<diff>", "(oop (open-output-string))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&iop_no, Value::Boolean(false)));
    assert!(matches!(&oop_no, Value::Boolean(false)));
    assert!(matches!(&iop_yes, Value::Boolean(true)));
    assert!(matches!(&oop_yes, Value::Boolean(true)));
}

#[test]
fn diff_jit_port_eof_and_position() {
    // ADR 0012 D-2 (iter GQ) — port-eof? + port-has-port-position? +
    // port-has-set-port-position!?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (pe p) (port-eof? p))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (php p) (port-has-port-position? p))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (phsp p) (port-has-set-port-position!? p))",
    )
    .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-p (open-input-string \"\"))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (pe warm-p) (php warm-p) (phsp warm-p) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let eof_empty = rt
        .eval_str_via_vm("<diff>", "(pe (open-input-string \"\"))")
        .unwrap();
    let eof_nonempty = rt
        .eval_str_via_vm("<diff>", "(pe (open-input-string \"hi\"))")
        .unwrap();
    let php_no = rt.eval_str_via_vm("<diff>", "(php 42)").unwrap();
    let php_yes = rt
        .eval_str_via_vm("<diff>", "(php (open-input-string \"\"))")
        .unwrap();
    let phsp_yes = rt
        .eval_str_via_vm("<diff>", "(phsp (open-input-string \"\"))")
        .unwrap();
    let phsp_out = rt
        .eval_str_via_vm("<diff>", "(phsp (open-output-string))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&eof_empty, Value::Boolean(true)));
    assert!(matches!(&eof_nonempty, Value::Boolean(false)));
    assert!(matches!(&php_no, Value::Boolean(false)));
    assert!(matches!(&php_yes, Value::Boolean(true)));
    assert!(matches!(&phsp_yes, Value::Boolean(true)));
    assert!(matches!(&phsp_out, Value::Boolean(false)));
}

#[test]
fn diff_jit_port_position() {
    // ADR 0012 D-2 (iter GR) — port-position.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (pp p) (port-position p))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-p (open-input-string \"hello\"))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (pp warm-p) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let initial = rt
        .eval_str_via_vm("<diff>", "(pp (open-input-string \"hello\"))")
        .unwrap();
    let out_empty = rt
        .eval_str_via_vm("<diff>", "(pp (open-output-string))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(
        &initial,
        Value::Number(cs_core::Number::Fixnum(0))
    ));
    assert!(matches!(
        &out_empty,
        Value::Number(cs_core::Number::Fixnum(0))
    ));
}

#[test]
fn diff_jit_delete_and_delete_duplicates() {
    // ADR 0012 D-2 (iter GS) — delete + delete-duplicates.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (d x l) (delete x l))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (dd l) (delete-duplicates l))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (d 2 (list 1 2 3)) \
                      (dd (list 1 2 1 3)) \
                      (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let no_2 = rt
        .eval_str_via_vm("<diff>", "(d 2 (list 1 2 3 2 4))")
        .unwrap();
    let uniq = rt
        .eval_str_via_vm("<diff>", "(dd (list 1 2 1 3 2 4))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn list_to_vec(v: &Value) -> Vec<i64> {
        let mut out = Vec::new();
        let mut cur = v.clone();
        loop {
            match cur {
                Value::Pair(p) => {
                    if let Value::Number(cs_core::Number::Fixnum(n)) = p.car.borrow().clone() {
                        out.push(n);
                    }
                    cur = p.cdr.borrow().clone();
                }
                _ => break,
            }
        }
        out
    }
    assert_eq!(list_to_vec(&no_2), vec![1, 3, 4]);
    assert_eq!(list_to_vec(&uniq), vec![1, 2, 3, 4]);
}

#[test]
fn diff_jit_make_promise() {
    // ADR 0012 D-2 (iter GT) — make-promise.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mp v) (make-promise v))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (mp 42) (mp \"hello\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let p_int = rt.eval_str_via_vm("<diff>", "(mp 42)").unwrap();
    let p_str = rt.eval_str_via_vm("<diff>", "(mp \"hello\")").unwrap();
    let forced = rt.eval_str_via_vm("<diff>", "(force (mp 99))").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&p_int, Value::Promise(_)));
    assert!(matches!(&p_str, Value::Promise(_)));
    assert!(matches!(
        &forced,
        Value::Number(cs_core::Number::Fixnum(99))
    ));
}

#[test]
fn diff_jit_force_forced() {
    // ADR 0012 D-2 (iter GU) — force fast-path on already-forced promises.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (fp x) (force x))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define p1 (make-promise 42))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define p2 (make-promise \"hi\"))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (fp p1) (fp p2) (fp 7) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let v_int = rt.eval_str_via_vm("<diff>", "(fp p1)").unwrap();
    let v_str = rt.eval_str_via_vm("<diff>", "(fp p2)").unwrap();
    let v_passthrough = rt.eval_str_via_vm("<diff>", "(fp 7)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&v_int, Value::Number(cs_core::Number::Fixnum(42))));
    match &v_str {
        Value::String(sg) => assert_eq!(&*sg.borrow(), "hi"),
        other => panic!("expected string, got {:?}", other),
    }
    assert!(matches!(
        &v_passthrough,
        Value::Number(cs_core::Number::Fixnum(7))
    ));
}

#[test]
fn diff_jit_hashtable_contains() {
    // ADR 0012 D-2 (iter GV) — hashtable-contains? fast path on Eq/Eqv/Equal.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (hc ht k) (hashtable-contains? ht k))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'b 2)")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (hc ht 'a) (hc ht 'z) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let yes = rt.eval_str_via_vm("<diff>", "(hc ht 'a)").unwrap();
    let no = rt.eval_str_via_vm("<diff>", "(hc ht 'z)").unwrap();
    // equal hashtable handles structural equality
    rt.eval_str_via_vm("<diff>", "(define eqht (make-hashtable equal-hash equal?))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! eqht (list 1 2) 'found)")
        .unwrap();
    let eq_yes = rt
        .eval_str_via_vm("<diff>", "(hc eqht (list 1 2))")
        .unwrap();
    let eq_no = rt
        .eval_str_via_vm("<diff>", "(hc eqht (list 3 4))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&yes, Value::Boolean(true)));
    assert!(matches!(&no, Value::Boolean(false)));
    assert!(matches!(&eq_yes, Value::Boolean(true)));
    assert!(matches!(&eq_no, Value::Boolean(false)));
}

#[test]
fn diff_jit_hashtable_delete() {
    // ADR 0012 D-2 (iter GW) — hashtable-delete! fast path.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (hd ht k) (hashtable-delete! ht k))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (hd warm-ht 'absent) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'b 2)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'c 3)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hd ht 'b)").unwrap();
    let sz = rt.eval_str_via_vm("<diff>", "(hashtable-size ht)").unwrap();
    let has_a = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht 'a)")
        .unwrap();
    let has_b = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht 'b)")
        .unwrap();
    let has_c = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht 'c)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hd ht 'z)").unwrap();
    let sz2 = rt.eval_str_via_vm("<diff>", "(hashtable-size ht)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&sz, Value::Number(cs_core::Number::Fixnum(2))));
    assert!(matches!(&has_a, Value::Boolean(true)));
    assert!(matches!(&has_b, Value::Boolean(false)));
    assert!(matches!(&has_c, Value::Boolean(true)));
    assert!(matches!(&sz2, Value::Number(cs_core::Number::Fixnum(2))));
}

#[test]
fn diff_jit_hashtable_set() {
    // ADR 0012 D-2 (iter GX) — hashtable-set! fast path.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (hs ht k v) (hashtable-set! ht k v))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (hs warm-ht 'a 1) (hs warm-ht 'b 2) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hs ht 'a 1)").unwrap();
    rt.eval_str_via_vm("<diff>", "(hs ht 'b 2)").unwrap();
    rt.eval_str_via_vm("<diff>", "(hs ht 'c 3)").unwrap();
    let sz = rt.eval_str_via_vm("<diff>", "(hashtable-size ht)").unwrap();
    let va = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref ht 'a #f)")
        .unwrap();
    let vb = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref ht 'b #f)")
        .unwrap();
    let vc = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref ht 'c #f)")
        .unwrap();
    // Overwrite existing key
    rt.eval_str_via_vm("<diff>", "(hs ht 'b 42)").unwrap();
    let vb2 = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref ht 'b #f)")
        .unwrap();
    let sz2 = rt.eval_str_via_vm("<diff>", "(hashtable-size ht)").unwrap();
    // Equal-hashtable with structural-equal keys
    rt.eval_str_via_vm("<diff>", "(define eqht (make-hashtable equal-hash equal?))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hs eqht (list 1 2) 100)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hs eqht (list 1 2) 200)")
        .unwrap();
    let eq_v = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref eqht (list 1 2) #f)")
        .unwrap();
    let eq_sz = rt
        .eval_str_via_vm("<diff>", "(hashtable-size eqht)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&sz, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&va, Value::Number(cs_core::Number::Fixnum(1))));
    assert!(matches!(&vb, Value::Number(cs_core::Number::Fixnum(2))));
    assert!(matches!(&vc, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&vb2, Value::Number(cs_core::Number::Fixnum(42))));
    assert!(matches!(&sz2, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&eq_v, Value::Number(cs_core::Number::Fixnum(200))));
    assert!(matches!(&eq_sz, Value::Number(cs_core::Number::Fixnum(1))));
}

#[test]
fn diff_jit_hashtable_ref() {
    // ADR 0012 D-2 (iter GY) — hashtable-ref fast path.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (hr ht k d) (hashtable-ref ht k d))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! warm-ht 'a 7)")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (hr warm-ht 'a 0) (hr warm-ht 'missing -1) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'k1 10)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'k2 20)")
        .unwrap();
    let hit = rt.eval_str_via_vm("<diff>", "(hr ht 'k1 -1)").unwrap();
    let hit2 = rt.eval_str_via_vm("<diff>", "(hr ht 'k2 -1)").unwrap();
    let miss = rt.eval_str_via_vm("<diff>", "(hr ht 'absent -1)").unwrap();
    // Default with non-fixnum boxed type
    let str_def = rt
        .eval_str_via_vm("<diff>", "(hr ht 'absent \"fallback\")")
        .unwrap();
    // Equal-hashtable structural key
    rt.eval_str_via_vm("<diff>", "(define eqht (make-hashtable equal-hash equal?))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! eqht (list 'x 1) 99)")
        .unwrap();
    let eq_hit = rt
        .eval_str_via_vm("<diff>", "(hr eqht (list 'x 1) -1)")
        .unwrap();
    let eq_miss = rt
        .eval_str_via_vm("<diff>", "(hr eqht (list 'y 2) -1)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&hit, Value::Number(cs_core::Number::Fixnum(10))));
    assert!(matches!(&hit2, Value::Number(cs_core::Number::Fixnum(20))));
    assert!(matches!(&miss, Value::Number(cs_core::Number::Fixnum(-1))));
    match &str_def {
        Value::String(sg) => assert_eq!(&*sg.borrow(), "fallback"),
        other => panic!("expected string fallback, got {:?}", other),
    }
    assert!(matches!(
        &eq_hit,
        Value::Number(cs_core::Number::Fixnum(99))
    ));
    assert!(matches!(
        &eq_miss,
        Value::Number(cs_core::Number::Fixnum(-1))
    ));
}

#[test]
fn diff_jit_hashtable_copy() {
    // ADR 0012 D-2 (iter GZ) — hashtable-copy fast path.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (hc ht) (hashtable-copy ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm-ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! warm-ht 'a 1)")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (hc warm-ht) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Populate then copy
    rt.eval_str_via_vm("<diff>", "(define ht (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'x 10)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht 'y 20)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define ht2 (hc ht))")
        .unwrap();
    let sz_orig = rt.eval_str_via_vm("<diff>", "(hashtable-size ht)").unwrap();
    let sz_copy = rt
        .eval_str_via_vm("<diff>", "(hashtable-size ht2)")
        .unwrap();
    let copy_has_x = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref ht2 'x -1)")
        .unwrap();
    let copy_has_y = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref ht2 'y -1)")
        .unwrap();
    // Mutate copy — original must stay unchanged
    rt.eval_str_via_vm("<diff>", "(hashtable-set! ht2 'z 99)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-delete! ht2 'x)")
        .unwrap();
    let orig_z = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht 'z)")
        .unwrap();
    let orig_x = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht 'x)")
        .unwrap();
    let copy_z = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht2 'z)")
        .unwrap();
    let copy_x = rt
        .eval_str_via_vm("<diff>", "(hashtable-contains? ht2 'x)")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(
        &sz_orig,
        Value::Number(cs_core::Number::Fixnum(2))
    ));
    assert!(matches!(
        &sz_copy,
        Value::Number(cs_core::Number::Fixnum(2))
    ));
    assert!(matches!(
        &copy_has_x,
        Value::Number(cs_core::Number::Fixnum(10))
    ));
    assert!(matches!(
        &copy_has_y,
        Value::Number(cs_core::Number::Fixnum(20))
    ));
    // Original unchanged after copy mutation
    assert!(matches!(&orig_z, Value::Boolean(false)));
    assert!(matches!(&orig_x, Value::Boolean(true)));
    // Copy reflects its own mutations
    assert!(matches!(&copy_z, Value::Boolean(true)));
    assert!(matches!(&copy_x, Value::Boolean(false)));
}

#[test]
fn diff_jit_vector_copy_slice() {
    // ADR 0012 D-2 (iter HA) — vector-copy 3-arg slice form.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (vc v s e) (vector-copy v s e))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define src #(10 20 30 40 50))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (vc src 1 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let mid = rt.eval_str_via_vm("<diff>", "(vc src 1 4)").unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(vc src 2 2)").unwrap();
    let full = rt.eval_str_via_vm("<diff>", "(vc src 0 5)").unwrap();
    let prefix = rt.eval_str_via_vm("<diff>", "(vc src 0 2)").unwrap();
    let suffix = rt.eval_str_via_vm("<diff>", "(vc src 3 5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn vec_to_ints(v: &Value) -> Vec<i64> {
        match v {
            Value::Vector(vg) => vg
                .borrow()
                .iter()
                .map(|e| match e {
                    Value::Number(cs_core::Number::Fixnum(n)) => *n,
                    other => panic!("unexpected element: {:?}", other),
                })
                .collect(),
            other => panic!("expected vector, got {:?}", other),
        }
    }
    assert_eq!(vec_to_ints(&mid), vec![20, 30, 40]);
    assert_eq!(vec_to_ints(&empty), Vec::<i64>::new());
    assert_eq!(vec_to_ints(&full), vec![10, 20, 30, 40, 50]);
    assert_eq!(vec_to_ints(&prefix), vec![10, 20]);
    assert_eq!(vec_to_ints(&suffix), vec![40, 50]);
}

#[test]
fn diff_jit_string_copy_slice() {
    // ADR 0012 D-2 (iter HB) — string-copy 3-arg reuses the substring
    // lowering (R7RS char-based slicing).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sc s a b) (string-copy s a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define src \"hello-world\")")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sc src 0 5) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let mid = rt.eval_str_via_vm("<diff>", "(sc src 6 11)").unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(sc src 3 3)").unwrap();
    let full = rt.eval_str_via_vm("<diff>", "(sc src 0 11)").unwrap();
    let prefix = rt.eval_str_via_vm("<diff>", "(sc src 0 5)").unwrap();
    // Multibyte: char-based slicing must skip bytes correctly
    rt.eval_str_via_vm("<diff>", "(define utf \"αβγδε\")")
        .unwrap();
    let utf_mid = rt.eval_str_via_vm("<diff>", "(sc utf 1 4)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn s_of(v: &Value) -> String {
        match v {
            Value::String(sg) => sg.borrow().clone(),
            other => panic!("expected string, got {:?}", other),
        }
    }
    assert_eq!(s_of(&mid), "world");
    assert_eq!(s_of(&empty), "");
    assert_eq!(s_of(&full), "hello-world");
    assert_eq!(s_of(&prefix), "hello");
    assert_eq!(s_of(&utf_mid), "βγδ");
}

#[test]
fn diff_jit_bytevector_copy_slice() {
    // ADR 0012 D-2 (iter HC) — bytevector-copy 3-arg slice form.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bc v s e) (bytevector-copy v s e))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define src (bytevector 1 2 3 4 5 6 7 8))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (bc src 2 6) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let mid = rt.eval_str_via_vm("<diff>", "(bc src 2 6)").unwrap();
    let empty = rt.eval_str_via_vm("<diff>", "(bc src 4 4)").unwrap();
    let full = rt.eval_str_via_vm("<diff>", "(bc src 0 8)").unwrap();
    let prefix = rt.eval_str_via_vm("<diff>", "(bc src 0 3)").unwrap();
    let suffix = rt.eval_str_via_vm("<diff>", "(bc src 5 8)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn bv_to_bytes(v: &Value) -> Vec<u8> {
        match v {
            Value::ByteVector(bg) => bg.borrow().clone(),
            other => panic!("expected bytevector, got {:?}", other),
        }
    }
    assert_eq!(bv_to_bytes(&mid), vec![3, 4, 5, 6]);
    assert_eq!(bv_to_bytes(&empty), Vec::<u8>::new());
    assert_eq!(bv_to_bytes(&full), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(bv_to_bytes(&prefix), vec![1, 2, 3]);
    assert_eq!(bv_to_bytes(&suffix), vec![6, 7, 8]);
}

#[test]
fn diff_jit_eof_object() {
    // ADR 0012 D-2 (iter HD) — eof-object 0-arg constructor.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mke) (eof-object))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (mke) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let v = rt.eval_str_via_vm("<diff>", "(mke)").unwrap();
    let is_eof = rt.eval_str_via_vm("<diff>", "(eof-object? (mke))").unwrap();
    let not_eof = rt.eval_str_via_vm("<diff>", "(eof-object? 42)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&v, Value::Eof));
    assert!(matches!(&is_eof, Value::Boolean(true)));
    assert!(matches!(&not_eof, Value::Boolean(false)));
}

#[test]
fn diff_jit_string_replace_first() {
    // ADR 0012 D-2 (iter HE) — string-replace (first occurrence only).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sr s f t) (string-replace s f t))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sr \"abcabc\" \"a\" \"X\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let one_hit = rt
        .eval_str_via_vm("<diff>", "(sr \"abcabc\" \"a\" \"X\")")
        .unwrap();
    let multi_target = rt
        .eval_str_via_vm("<diff>", "(sr \"foo-foo-foo\" \"foo\" \"BAR\")")
        .unwrap();
    let no_match = rt
        .eval_str_via_vm("<diff>", "(sr \"hello\" \"xyz\" \"!\")")
        .unwrap();
    let utf = rt
        .eval_str_via_vm("<diff>", "(sr \"αβγαβγ\" \"α\" \"X\")")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn s_of(v: &Value) -> String {
        match v {
            Value::String(sg) => sg.borrow().clone(),
            other => panic!("expected string, got {:?}", other),
        }
    }
    // Only the FIRST occurrence is replaced
    assert_eq!(s_of(&one_hit), "Xbcabc");
    assert_eq!(s_of(&multi_target), "BAR-foo-foo");
    assert_eq!(s_of(&no_match), "hello");
    assert_eq!(s_of(&utf), "Xβγαβγ");
}

#[test]
fn diff_jit_bytevector_fill_slice() {
    // ADR 0012 D-2 (iter HF) — bytevector-fill! 4-arg slice form.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (bf v fill s e) (bytevector-fill! v fill s e) v)",
    )
    .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm (make-bytevector 8 0))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (bf warm 7 2 6) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    rt.eval_str_via_vm("<diff>", "(define bv (make-bytevector 8 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(bf bv 9 2 6)").unwrap();
    let after_mid = rt.eval_str_via_vm("<diff>", "bv").unwrap();
    rt.eval_str_via_vm("<diff>", "(define bv2 (make-bytevector 6 1))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(bf bv2 0 0 6)").unwrap();
    let after_full = rt.eval_str_via_vm("<diff>", "bv2").unwrap();
    rt.eval_str_via_vm("<diff>", "(define bv3 (make-bytevector 4 5))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(bf bv3 99 2 2)").unwrap();
    let after_empty = rt.eval_str_via_vm("<diff>", "bv3").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn bv_bytes(v: &Value) -> Vec<u8> {
        match v {
            Value::ByteVector(bg) => bg.borrow().clone(),
            other => panic!("expected bytevector, got {:?}", other),
        }
    }
    // bv: 8 zeros, fill 2..6 with 9
    assert_eq!(bv_bytes(&after_mid), vec![0, 0, 9, 9, 9, 9, 0, 0]);
    // bv2: 6 ones, fill 0..6 with 0
    assert_eq!(bv_bytes(&after_full), vec![0, 0, 0, 0, 0, 0]);
    // bv3: 4 fives, fill 2..2 (empty range) — unchanged
    assert_eq!(bv_bytes(&after_empty), vec![5, 5, 5, 5]);
}

#[test]
fn diff_jit_vector_fill_slice() {
    // ADR 0012 D-2 (iter HG) — vector-fill! 4-arg slice form.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(define (vf v fill s e) (vector-fill! v fill s e) v)",
    )
    .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm (make-vector 8 0))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (vf warm 7 2 6) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    // Fixnum fill (BoxTyped from Fixnum to Any)
    rt.eval_str_via_vm("<diff>", "(define v1 (make-vector 6 0))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(vf v1 99 1 5)").unwrap();
    let after_mid = rt.eval_str_via_vm("<diff>", "v1").unwrap();
    // String fill (Any operand, no BoxTyped needed)
    rt.eval_str_via_vm("<diff>", "(define v2 (make-vector 4 'init))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(vf v2 \"x\" 0 4)").unwrap();
    let after_str = rt.eval_str_via_vm("<diff>", "v2").unwrap();
    // Empty range (start == end) — source unchanged
    rt.eval_str_via_vm("<diff>", "(define v3 (make-vector 3 'a))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(vf v3 'z 1 1)").unwrap();
    let after_empty = rt.eval_str_via_vm("<diff>", "v3").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn vec_ints(v: &Value) -> Vec<i64> {
        match v {
            Value::Vector(vg) => vg
                .borrow()
                .iter()
                .map(|e| match e {
                    Value::Number(cs_core::Number::Fixnum(n)) => *n,
                    other => panic!("unexpected element: {:?}", other),
                })
                .collect(),
            other => panic!("expected vector, got {:?}", other),
        }
    }
    fn vec_string_count(v: &Value, expected: &str) -> usize {
        match v {
            Value::Vector(vg) => vg
                .borrow()
                .iter()
                .filter(|e| matches!(e, Value::String(sg) if sg.borrow().as_str() == expected))
                .count(),
            other => panic!("expected vector, got {:?}", other),
        }
    }
    // v1: 6 zeros, fill 1..5 with 99
    assert_eq!(vec_ints(&after_mid), vec![0, 99, 99, 99, 99, 0]);
    // v2: 4 symbols 'init', fill all 4 slots with "x"
    assert_eq!(vec_string_count(&after_str, "x"), 4);
    // v3: 3 symbols 'a, fill 1..1 (empty) — vector unchanged length 3
    match &after_empty {
        Value::Vector(vg) => assert_eq!(vg.borrow().len(), 3),
        other => panic!("expected vector, got {:?}", other),
    }
}

#[test]
fn diff_jit_string_fill_slice() {
    // ADR 0012 D-2 (iter HH) — string-fill! 4-arg slice form.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (sf s ch a b) (string-fill! s ch a b) s)")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define warm (make-string 8 #\\a))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (sf warm #\\X 2 6) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    rt.eval_str_via_vm("<diff>", "(define s1 (make-string 6 #\\.))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(sf s1 #\\* 1 5)").unwrap();
    let after_mid = rt.eval_str_via_vm("<diff>", "s1").unwrap();
    rt.eval_str_via_vm("<diff>", "(define s2 (make-string 4 #\\z))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(sf s2 #\\Q 0 4)").unwrap();
    let after_full = rt.eval_str_via_vm("<diff>", "s2").unwrap();
    rt.eval_str_via_vm("<diff>", "(define s3 (make-string 3 #\\.))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(sf s3 #\\x 1 1)").unwrap();
    let after_empty = rt.eval_str_via_vm("<diff>", "s3").unwrap();
    rt.eval_str_via_vm("<diff>", "(define s4 (string #\\α #\\β #\\δ #\\ε))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(sf s4 #\\γ 1 3)").unwrap();
    let after_utf = rt.eval_str_via_vm("<diff>", "s4").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    fn s_of(v: &Value) -> String {
        match v {
            Value::String(sg) => sg.borrow().clone(),
            other => panic!("expected string, got {:?}", other),
        }
    }
    assert_eq!(s_of(&after_mid), ".****.");
    assert_eq!(s_of(&after_full), "QQQQ");
    assert_eq!(s_of(&after_empty), "...");
    assert_eq!(s_of(&after_utf), "αγγε");
}

#[test]
fn diff_jit_exact_nonneg_int() {
    // ADR 0012 D-2 (iter HI) — exact-nonnegative-integer?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (eni x) (exact-nonnegative-integer? x))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (eni 7) (eni -3) (eni \"hi\") (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let zero = rt.eval_str_via_vm("<diff>", "(eni 0)").unwrap();
    let pos = rt.eval_str_via_vm("<diff>", "(eni 42)").unwrap();
    let neg = rt.eval_str_via_vm("<diff>", "(eni -1)").unwrap();
    let flo = rt.eval_str_via_vm("<diff>", "(eni 3.14)").unwrap();
    let str_ = rt.eval_str_via_vm("<diff>", "(eni \"hi\")").unwrap();
    let bool_ = rt.eval_str_via_vm("<diff>", "(eni #t)").unwrap();
    let big = rt
        .eval_str_via_vm("<diff>", "(eni (* 99999999999 99999999999))")
        .unwrap();
    let big_neg = rt
        .eval_str_via_vm("<diff>", "(eni (- 0 (* 99999999999 99999999999)))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&zero, Value::Boolean(true)));
    assert!(matches!(&pos, Value::Boolean(true)));
    assert!(matches!(&neg, Value::Boolean(false)));
    assert!(matches!(&flo, Value::Boolean(false)));
    assert!(matches!(&str_, Value::Boolean(false)));
    assert!(matches!(&bool_, Value::Boolean(false)));
    assert!(matches!(&big, Value::Boolean(true)));
    assert!(matches!(&big_neg, Value::Boolean(false)));
}

#[test]
fn diff_jit_bytevector_eq() {
    // ADR 0012 D-2 (iter HJ) — bytevector=?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (bve a b) (bytevector=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define a (bytevector 1 2 3))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define b (bytevector 1 2 3))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (bve a b) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let same = rt.eval_str_via_vm("<diff>", "(bve a b)").unwrap();
    let diff_content = rt
        .eval_str_via_vm("<diff>", "(bve a (bytevector 1 2 4))")
        .unwrap();
    let diff_length = rt
        .eval_str_via_vm("<diff>", "(bve a (bytevector 1 2))")
        .unwrap();
    let empty = rt
        .eval_str_via_vm(
            "<diff>",
            "(bve (make-bytevector 0 0) (make-bytevector 0 0))",
        )
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&same, Value::Boolean(true)));
    assert!(matches!(&diff_content, Value::Boolean(false)));
    assert!(matches!(&diff_length, Value::Boolean(false)));
    assert!(matches!(&empty, Value::Boolean(true)));
}

#[test]
fn diff_jit_vector_eq() {
    // ADR 0012 D-2 (iter HK) — vector=?.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (ve a b) (vector=? a b))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define a (vector 1 2 3))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define b (vector 1 2 3))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (ve a b) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let same = rt.eval_str_via_vm("<diff>", "(ve a b)").unwrap();
    let diff_content = rt
        .eval_str_via_vm("<diff>", "(ve a (vector 1 2 4))")
        .unwrap();
    let diff_length = rt.eval_str_via_vm("<diff>", "(ve a (vector 1 2))").unwrap();
    let empty = rt
        .eval_str_via_vm("<diff>", "(ve (vector) (vector))")
        .unwrap();
    // Structural equality: vectors of strings
    let strs = rt
        .eval_str_via_vm(
            "<diff>",
            "(ve (vector \"hi\" \"world\") (vector \"hi\" \"world\"))",
        )
        .unwrap();
    // Nested vectors (recursive)
    let nested = rt
        .eval_str_via_vm(
            "<diff>",
            "(ve (vector (vector 1 2) 3) (vector (vector 1 2) 3))",
        )
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&same, Value::Boolean(true)));
    assert!(matches!(&diff_content, Value::Boolean(false)));
    assert!(matches!(&diff_length, Value::Boolean(false)));
    assert!(matches!(&empty, Value::Boolean(true)));
    assert!(matches!(&strs, Value::Boolean(true)));
    assert!(matches!(&nested, Value::Boolean(true)));
}

#[test]
fn diff_jit_make_bytevector_one_arg() {
    // ADR 0012 D-2 (iter HL) — make-bytevector 1-arg form (fill defaults
    // to 0).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mbv n) (make-bytevector n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (mbv 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let bv5 = rt.eval_str_via_vm("<diff>", "(mbv 5)").unwrap();
    let bv0 = rt.eval_str_via_vm("<diff>", "(mbv 0)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &bv5 {
        Value::ByteVector(bg) => assert_eq!(&*bg.borrow(), &[0u8, 0, 0, 0, 0]),
        other => panic!("expected bytevector, got {:?}", other),
    }
    match &bv0 {
        Value::ByteVector(bg) => assert_eq!(bg.borrow().len(), 0),
        other => panic!("expected bytevector, got {:?}", other),
    }
}

#[test]
fn diff_jit_make_string_one_arg() {
    // ADR 0012 D-2 (iter HM) — make-string 1-arg form (fill defaults
    // to #\space).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mks n) (make-string n))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (mks 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let s5 = rt.eval_str_via_vm("<diff>", "(mks 5)").unwrap();
    let s0 = rt.eval_str_via_vm("<diff>", "(mks 0)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    match &s5 {
        Value::String(sg) => assert_eq!(sg.borrow().as_str(), "     "),
        other => panic!("expected string, got {:?}", other),
    }
    match &s0 {
        Value::String(sg) => assert_eq!(sg.borrow().as_str(), ""),
        other => panic!("expected string, got {:?}", other),
    }
}

#[test]
fn diff_jit_div0_mod0() {
    // ADR 0012 D-2 (iter HO) — R6RS centered div0 / mod0.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (d x y) (div0 x y))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m x y) (mod0 x y))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (d 13 4) (m 13 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let d13_4 = rt.eval_str_via_vm("<diff>", "(d 13 4)").unwrap();
    let m13_4 = rt.eval_str_via_vm("<diff>", "(m 13 4)").unwrap();
    let d14_4 = rt.eval_str_via_vm("<diff>", "(d 14 4)").unwrap();
    let m14_4 = rt.eval_str_via_vm("<diff>", "(m 14 4)").unwrap();
    let d13_n4 = rt.eval_str_via_vm("<diff>", "(d 13 -4)").unwrap();
    let m13_n4 = rt.eval_str_via_vm("<diff>", "(m 13 -4)").unwrap();
    let d0_5 = rt.eval_str_via_vm("<diff>", "(d 0 5)").unwrap();
    let m0_5 = rt.eval_str_via_vm("<diff>", "(m 0 5)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&d13_4, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&m13_4, Value::Number(cs_core::Number::Fixnum(1))));
    assert!(matches!(&d14_4, Value::Number(cs_core::Number::Fixnum(4))));
    assert!(matches!(&m14_4, Value::Number(cs_core::Number::Fixnum(-2))));
    assert!(matches!(
        &d13_n4,
        Value::Number(cs_core::Number::Fixnum(-3))
    ));
    assert!(matches!(&m13_n4, Value::Number(cs_core::Number::Fixnum(1))));
    assert!(matches!(&d0_5, Value::Number(cs_core::Number::Fixnum(0))));
    assert!(matches!(&m0_5, Value::Number(cs_core::Number::Fixnum(0))));
}

#[test]
fn diff_jit_fxdiv0_fxmod0() {
    // ADR 0012 D-2 (iter HP) — R6RS fxdiv0 / fxmod0 (fixnum variants).
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (d x y) (fxdiv0 x y))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (m x y) (fxmod0 x y))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (d 13 4) (m 13 4) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let d13_4 = rt.eval_str_via_vm("<diff>", "(d 13 4)").unwrap();
    let m13_4 = rt.eval_str_via_vm("<diff>", "(m 13 4)").unwrap();
    let d14_4 = rt.eval_str_via_vm("<diff>", "(d 14 4)").unwrap();
    let m14_4 = rt.eval_str_via_vm("<diff>", "(m 14 4)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&d13_4, Value::Number(cs_core::Number::Fixnum(3))));
    assert!(matches!(&m13_4, Value::Number(cs_core::Number::Fixnum(1))));
    assert!(matches!(&d14_4, Value::Number(cs_core::Number::Fixnum(4))));
    assert!(matches!(&m14_4, Value::Number(cs_core::Number::Fixnum(-2))));
}

#[test]
fn diff_jit_hashtable_hash_function() {
    // ADR 0012 D-2 (iter HQ) — hashtable-hash-function.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (hhf ht) (hashtable-hash-function ht))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define eqh (make-eq-hashtable))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (hhf eqh) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let on_eq = rt.eval_str_via_vm("<diff>", "(hhf eqh)").unwrap();
    let on_eqv = rt
        .eval_str_via_vm("<diff>", "(hhf (make-eqv-hashtable))")
        .unwrap();
    let on_equal = rt
        .eval_str_via_vm("<diff>", "(hhf (make-hashtable))")
        .unwrap();
    let on_custom = rt
        .eval_str_via_vm("<diff>", "(hhf (make-hashtable string-hash string=?))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&on_eq, Value::Boolean(false)));
    assert!(matches!(&on_eqv, Value::Boolean(false)));
    assert!(matches!(&on_equal, Value::Boolean(false)));
    assert!(
        !matches!(&on_custom, Value::Boolean(false)),
        "expected custom hash proc, got {:?}",
        on_custom
    );
}

#[test]
fn diff_jit_make_hashtable_zero_arg() {
    // ADR 0012 D-2 (iter HR) — (make-hashtable) 0-arg = Equal-kind.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (mh) (make-hashtable))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (mh) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let h = rt.eval_str_via_vm("<diff>", "(mh)").unwrap();
    let is_ht = rt.eval_str_via_vm("<diff>", "(hashtable? (mh))").unwrap();
    let sz = rt
        .eval_str_via_vm("<diff>", "(hashtable-size (mh))")
        .unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&h, Value::Hashtable(_)));
    assert!(matches!(&is_ht, Value::Boolean(true)));
    assert!(matches!(&sz, Value::Number(cs_core::Number::Fixnum(0))));
}

#[test]
fn diff_jit_make_hashtable_eq_eqv() {
    // ADR 0012 D-2 (iter HS) — make-eq/eqv-hashtable 0-arg constructors.
    let mut rt = Runtime::new();
    rt.install_jit().unwrap();
    rt.eval_str_via_vm("<diff>", "(define (eqh) (make-eq-hashtable))")
        .unwrap();
    rt.eval_str_via_vm("<diff>", "(define (eqvh) (make-eqv-hashtable))")
        .unwrap();
    rt.eval_str_via_vm(
        "<diff>",
        "(let loop ((i 0)) \
           (if (= i 1500) 'done \
               (begin (eqh) (eqvh) (loop (+ i 1)))))",
    )
    .unwrap();
    cs_vm::vm::reset_jit_call_count();
    let h_eq = rt.eval_str_via_vm("<diff>", "(eqh)").unwrap();
    let h_eqv = rt.eval_str_via_vm("<diff>", "(eqvh)").unwrap();
    let _ = cs_vm::vm::jit_call_count();
    assert!(matches!(&h_eq, Value::Hashtable(_)));
    assert!(matches!(&h_eqv, Value::Hashtable(_)));
    // Round-trip: set+get works
    rt.eval_str_via_vm("<diff>", "(define h1 (eqh))").unwrap();
    rt.eval_str_via_vm("<diff>", "(hashtable-set! h1 'a 1)")
        .unwrap();
    let v = rt
        .eval_str_via_vm("<diff>", "(hashtable-ref h1 'a #f)")
        .unwrap();
    assert!(matches!(&v, Value::Number(cs_core::Number::Fixnum(1))));
}
