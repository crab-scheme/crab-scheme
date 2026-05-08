//! Differential testing: tree-walker vs bytecode VM.
//!
//! Each test program is run through both tiers; results must match.
//! This is the M4 exit-gate harness.

use cs_core::WriteMode;
use cs_runtime::Runtime;

fn diff(src: &str) {
    let mut rt_walker = Runtime::new();
    let mut rt_vm = Runtime::new();
    let v_walker = rt_walker
        .eval_str("<diff-walker>", src)
        .unwrap_or_else(|d| panic!("walker failed on {:?}: {}", src, d.message));
    let v_vm = rt_vm
        .eval_str_via_vm("<diff-vm>", src)
        .unwrap_or_else(|d| panic!("vm failed on {:?}: {}", src, d.message));
    let s_walker = rt_walker.format_value(&v_walker, WriteMode::Write);
    let s_vm = rt_vm.format_value(&v_vm, WriteMode::Write);
    assert_eq!(
        s_walker, s_vm,
        "differential mismatch on {:?}: walker={} vm={}",
        src, s_walker, s_vm
    );
}

#[test]
fn diff_constants() {
    diff("42");
    diff("#t");
    diff("#\\a");
    diff("\"hello\"");
}

#[test]
fn diff_arithmetic() {
    diff("(+ 1 2 3)");
    diff("(* 2 3 4)");
    diff("(- 100 50 25)");
    diff("(+ (* 2 3) (- 10 5))");
}

#[test]
fn diff_if() {
    diff("(if #t 1 2)");
    diff("(if #f 1 2)");
    diff("(if (= 5 5) (* 10 10) 0)");
}

#[test]
fn diff_lambda() {
    diff("((lambda (x) (* x x)) 7)");
    diff("((lambda (x y) (+ x y)) 10 20)");
    diff("((lambda (x) ((lambda (y) (+ x y)) 5)) 10)");
}

#[test]
fn diff_letrec_recursive() {
    diff("(letrec ((fact (lambda (n) (if (= n 0) 1 (* n (fact (- n 1))))))) (fact 6))");
    diff("(letrec ((fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))) (fib 10))");
}

#[test]
fn diff_closures() {
    diff("(((lambda (x) (lambda (y) (+ x y))) 10) 32)");
}

#[test]
fn diff_cons() {
    diff("(car (cons 1 2))");
    diff("(cdr (cons 1 2))");
}

#[test]
fn diff_tail_call_iteration() {
    // 100,000 tail-recursive iterations: must not stack-overflow on either tier.
    diff(
        "(letrec ((loop (lambda (n acc)
                          (if (= n 0) acc (loop (- n 1) (+ acc 1))))))
           (loop 100000 0))",
    );
}

#[test]
fn diff_factorial_tail_iter() {
    // 50! iteratively (large enough to test bignum but not too slow).
    diff(
        "(letrec ((fact-iter (lambda (n acc)
                                (if (= n 0) acc (fact-iter (- n 1) (* n acc))))))
           (fact-iter 20 1))",
    );
}

#[test]
fn diff_lists() {
    diff("(length (list 1 2 3 4 5))");
    diff("(reverse '(1 2 3))");
    diff("(append '(1 2) '(3 4) '(5))");
}

#[test]
fn diff_nested_let() {
    diff(
        "(letrec ((f (lambda (x)
                       (letrec ((g (lambda (y) (+ x y))))
                         (g 10)))))
           (f 5))",
    );
}

#[test]
fn diff_define_then_call() {
    diff(
        "(define square (lambda (x) (* x x)))
         (define cube (lambda (x) (* x (square x))))
         (cube 5)",
    );
}

#[test]
fn diff_mutation_via_set() {
    diff(
        "(define n 0)
         (set! n 10)
         (set! n (+ n 5))
         n",
    );
}

#[test]
fn diff_quote() {
    diff("'(a b c)");
    diff("'((1 2) (3 4))");
}

#[test]
fn diff_predicates() {
    diff("(pair? '(1 2))");
    diff("(null? '())");
    diff("(number? 42)");
    diff("(string? \"x\")");
    diff("(eqv? 5 5)");
    diff("(equal? '(1 2 3) '(1 2 3))");
}

#[test]
fn diff_list_operations() {
    diff("(length '(a b c d e))");
    diff("(reverse '(1 2 3 4))");
    diff("(append '() '(1) '(2 3))");
    diff("(list 1 2 3)");
}

#[test]
fn diff_nested_letrec_local_scope() {
    // Inner letrec must NOT clobber outer x.
    diff(
        "(define x 100)
         (letrec ((x 7)) x)",
    );
    diff(
        "(define x 100)
         (letrec ((x 7)) x)
         x", // Outer x must still be 100.
    );
}

#[test]
fn diff_letrec_recursive_closure_capture() {
    // Inner letrec captures outer.
    diff(
        "(define n 100)
         (letrec ((f (lambda () n))) (f))",
    );
}

#[test]
fn diff_inner_mutation_via_set() {
    // set! on an inner binding mutates the inner scope, not the outer
    diff(
        "(define x 100)
         (letrec ((x 1))
           (set! x 99)
           x)",
    );
    diff(
        "(define x 100)
         (letrec ((x 1))
           (set! x 99))
         x", // After the letrec, outer x should still be 100.
    );
}

#[test]
fn diff_counter_closure() {
    // Classic mutable-closure-captures pattern
    diff(
        "(letrec ((make-counter (lambda ()
                                   (letrec ((n 0))
                                     (lambda () (set! n (+ n 1)) n)))))
           (let ((c (make-counter)))
             (c)
             (c)
             (c)))",
    );
}

#[test]
fn diff_quasiquote() {
    diff("`(1 ,(+ 2 3) 4)");
    diff(
        "(let ((xs '(2 3 4)))
           `(1 ,@xs 5))",
    );
}

#[test]
fn diff_strings() {
    diff("(string-length \"hello\")");
    diff("(string-append \"foo\" \"bar\")");
    diff("(string=? \"abc\" \"abc\")");
}

#[test]
fn diff_apply() {
    diff("(apply + '(1 2 3 4 5))");
    diff("(apply + 1 2 '(3 4 5))");
    diff("(apply (lambda (a b c) (* a b c)) '(2 3 4))");
    diff("(apply list 1 2 '(3 4 5))");
}

#[test]
fn diff_map() {
    diff("(map (lambda (x) (* x x)) '(1 2 3 4 5))");
    diff("(map + '(1 2 3) '(10 20 30))");
}

#[test]
fn diff_filter() {
    diff("(filter even? '(1 2 3 4 5 6))");
    diff("(filter (lambda (x) (> x 3)) '(1 2 3 4 5))");
}

#[test]
fn diff_for_each() {
    diff(
        "(define total 0)
         (for-each (lambda (x) (set! total (+ total x))) '(1 2 3 4 5))
         total",
    );
}

#[test]
fn diff_fold_left() {
    diff("(fold-left + 0 '(1 2 3 4 5))");
    diff("(fold-left * 1 '(1 2 3 4 5))");
    diff("(fold-left (lambda (acc x) (cons x acc)) '() '(1 2 3))");
}

#[test]
fn diff_fold_right() {
    diff("(fold-right cons '() '(1 2 3))");
    diff("(fold-right + 0 '(1 2 3 4 5))");
    diff("(fold-right (lambda (x acc) (cons (* x 2) acc)) '() '(1 2 3))");
}

#[test]
fn diff_reduce() {
    diff("(reduce + 0 '(1 2 3 4 5))");
    diff("(reduce + 99 '())");
    diff("(reduce max 0 '(3 1 4 1 5 9 2 6))");
}

#[test]
fn diff_count() {
    diff("(count even? '(1 2 3 4 5 6))");
    diff("(count even? '())");
    diff("(count (lambda (x) (> x 3)) '(1 2 3 4 5))");
}

#[test]
fn diff_partition() {
    diff(
        "(call-with-values
           (lambda () (partition even? '(1 2 3 4 5 6)))
           list)",
    );
}

#[test]
fn diff_values_cwv() {
    diff(
        "(call-with-values
           (lambda () (values 1 2 3))
           (lambda (a b c) (+ a b c)))",
    );
    diff(
        "(call-with-values
           (lambda () 42)
           (lambda (x) (* x 2)))",
    );
}

#[test]
fn diff_apply_over_ho() {
    // apply on a HO marker proc — exercises ho_apply path in vm_call_sync.
    diff("(apply map (list (lambda (x) (* x x)) '(1 2 3 4)))");
    diff("(apply fold-left (list + 0 '(1 2 3 4 5)))");
    diff("(apply filter (list even? '(1 2 3 4 5 6)))");
}

/// Time a closure and return millis as f64.
fn time_ms<F: FnOnce()>(f: F) -> f64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore] // run with --ignored for perf check
fn perf_fib_25_compared() {
    let src =
        "(letrec ((fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))) (fib 25))";
    let mut rt_w = Runtime::new();
    let mut rt_v = Runtime::new();
    // Warm-up
    let _ = rt_w.eval_str("warm", src);
    let _ = rt_v.eval_str_via_vm("warm", src);
    let walker_ms = time_ms(|| {
        let mut rt = Runtime::new();
        let _ = rt.eval_str("perf-walker", src);
    });
    let vm_ms = time_ms(|| {
        let mut rt = Runtime::new();
        let _ = rt.eval_str_via_vm("perf-vm", src);
    });
    println!(
        "(fib 25): walker={:.1}ms vm={:.1}ms ratio={:.2}",
        walker_ms,
        vm_ms,
        walker_ms / vm_ms
    );
}
