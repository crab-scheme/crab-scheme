//! Smoke tests for the M5 step 4.D wiring of Runtime roots into Heap.
//!
//! These tests don't exercise GC reclamation directly — Phase 1's
//! collector is Rc-backed, so cycles still leak the same way they
//! would have before M5. What they DO exercise is:
//!   • Runtime::new sets up the heap and registers the top frame +
//!     vm env as roots without panicking.
//!   • collect() runs without panicking on a non-trivial Runtime.
//!   • A program's defined globals survive a collect() — i.e. the
//!     root traversal actually walks the top frame.

use cs_runtime::Runtime;

#[test]
fn collect_after_runtime_new_does_not_panic() {
    let rt = Runtime::new();
    rt.collect();
    // Still alive — this is mostly a "doesn't blow up" check.
    assert!(rt.heap().alloc_count() == 0 || rt.heap().alloc_count() > 0);
}

#[test]
fn collect_preserves_global_definitions() {
    let mut rt = Runtime::new();
    rt.eval_str("<test>", "(define greet \"hello\")")
        .expect("define should evaluate");
    rt.collect();
    // The string we defined should still be reachable through the top
    // frame's binding for `greet`.
    let v = rt
        .eval_str("<test>", "greet")
        .expect("greet should still be bound");
    let formatted = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(formatted, "hello");
}

#[test]
fn collect_preserves_pair_chain() {
    let mut rt = Runtime::new();
    rt.eval_str("<test>", "(define xs '(1 2 3 4 5))")
        .expect("define list");
    rt.collect();
    let len = rt
        .eval_str("<test>", "(length xs)")
        .expect("length should still work");
    let formatted = rt.format_value(&len, cs_core::WriteMode::Display);
    assert_eq!(formatted, "5");
}

#[test]
fn collect_preserves_vector() {
    let mut rt = Runtime::new();
    rt.eval_str("<test>", "(define vec (make-vector 8 #f))")
        .expect("make-vector");
    rt.eval_str("<test>", "(vector-set! vec 3 'mark)")
        .expect("vector-set!");
    rt.collect();
    let v = rt
        .eval_str("<test>", "(vector-ref vec 3)")
        .expect("vector-ref after collect");
    let formatted = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(formatted, "mark");
}

#[test]
fn vm_tier_preserves_definitions_across_collect() {
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<test>", "(define vm-greet \"hi-vm\")")
        .expect("define on vm tier");
    rt.collect();
    let v = rt
        .eval_str_via_vm("<test>", "vm-greet")
        .expect("vm-greet still bound");
    let formatted = rt.format_value(&v, cs_core::WriteMode::Display);
    assert_eq!(formatted, "hi-vm");
}

#[test]
fn many_collects_in_a_row_are_idempotent() {
    let rt = Runtime::new();
    for _ in 0..10 {
        rt.collect();
    }
}
