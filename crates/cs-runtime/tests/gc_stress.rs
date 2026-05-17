#![cfg(not(feature = "countable-memory"))]

//! Stress test: run a non-trivial Scheme workload while interleaving
//! `Runtime::collect()` calls. The goal isn't perf measurement (Phase 1
//! is Rc-backed; the collect cycle is mostly bookkeeping) — it's a
//! correctness gate that confirms the root traversal doesn't trip on
//! any of the heap shapes the runtime actually allocates.
//!
//! A failure here means tracing is wrong: we mis-marked something and
//! a subsequent program access either panicked (BorrowMut conflict
//! during trace) or returned stale data. So far Phase 1's `Gc<T>` is
//! Rc-backed, so the only failure mode is a panic during trace, not
//! a use-after-free.

use cs_runtime::Runtime;

#[test]
fn collect_between_long_chains_of_evaluations() {
    let mut rt = Runtime::new();
    rt.eval_str("<p>", "(define total 0)").unwrap();
    for i in 1..=200 {
        let src = format!("(set! total (+ total {}))", i);
        rt.eval_str("<p>", &src).unwrap();
        if i % 25 == 0 {
            rt.collect();
        }
    }
    let v = rt.eval_str("<p>", "total").unwrap();
    let s = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(s, "20100"); // 1+2+…+200
}

#[test]
fn collect_between_string_and_vector_workload() {
    let mut rt = Runtime::new();
    let prog = r#"
      (define (range n)
        (let loop ((i n) (acc '()))
          (if (= i 0) acc (loop (- i 1) (cons i acc)))))
      (define (build-strs n)
        (map number->string (range n)))
      (define strs (build-strs 50))
      (define vec (list->vector strs))
      (define joined (apply string-append strs))
    "#;
    rt.eval_str("<p>", prog).unwrap();
    rt.collect();
    rt.collect(); // idempotent
    let len = rt.eval_str("<p>", "(string-length joined)").unwrap();
    let len_s = rt.format_value(&len, cs_core::WriteMode::Display);
    // strs = ["50", "49", ..., "1"]; lengths: 11 two-digit (50..10) +
    // 9 single-digit. 11*2 + 9 = 31. Wait — list->vector and then
    // apply over strs. range 50 → '(1 2 ... 50). map number->string
    // gives "1" "2" .. "50". String lengths: "1".."9" = 9*1 = 9;
    // "10".."50" = 41*2 = 82. Total = 91.
    assert_eq!(len_s, "91");

    let veclen = rt.eval_str("<p>", "(vector-length vec)").unwrap();
    let veclen_s = rt.format_value(&veclen, cs_core::WriteMode::Display);
    assert_eq!(veclen_s, "50");
}

#[test]
fn collect_during_hashtable_workload() {
    let mut rt = Runtime::new();
    let prog = r#"
      (define ht (make-eq-hashtable))
      (define (fill! n)
        (let loop ((i 0))
          (if (= i n)
              'done
              (begin
                (hashtable-set! ht (string->symbol (number->string i)) i)
                (loop (+ i 1))))))
      (fill! 100)
    "#;
    rt.eval_str("<p>", prog).unwrap();
    rt.collect();
    let size = rt.eval_str("<p>", "(hashtable-size ht)").unwrap();
    let s = rt.format_value(&size, cs_core::WriteMode::Display);
    assert_eq!(s, "100");
    let v = rt
        .eval_str(
            "<p>",
            "(hashtable-ref ht (string->symbol (number->string 42)) #f)",
        )
        .unwrap();
    let s = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(s, "42");
}

#[test]
fn collect_with_closures_capturing_data() {
    let mut rt = Runtime::new();
    let prog = r#"
      (define (make-counter)
        (let ((n 0))
          (lambda ()
            (set! n (+ n 1))
            n)))
      (define c1 (make-counter))
      (define c2 (make-counter))
    "#;
    rt.eval_str("<p>", prog).unwrap();
    rt.collect();
    // Closure-captured cell should still be reachable via top-frame
    // binding to c1 / c2.
    rt.eval_str("<p>", "(c1)").unwrap();
    rt.eval_str("<p>", "(c1)").unwrap();
    rt.eval_str("<p>", "(c1)").unwrap();
    rt.collect();
    let v = rt.eval_str("<p>", "(c1)").unwrap();
    let s = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(s, "4");
    let v2 = rt.eval_str("<p>", "(c2)").unwrap();
    let s2 = rt.format_value(&v2, cs_core::WriteMode::Display);
    // c2 is independent of c1; first call returns 1.
    assert_eq!(s2, "1");
}

#[test]
fn collect_does_not_disturb_vm_tier_program() {
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<p>", "(define (f x) (* x x))").unwrap();
    rt.collect();
    let v = rt.eval_str_via_vm("<p>", "(f 7)").unwrap();
    let s = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(s, "49");
    rt.collect();
    let v2 = rt.eval_str_via_vm("<p>", "(f 12)").unwrap();
    let s2 = rt.format_value(&v2, cs_core::WriteMode::Display);
    assert_eq!(s2, "144");
}
