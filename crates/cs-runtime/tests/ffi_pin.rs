//! `Pinned<'rt>` rooting tests for M5b iter 4.
//!
//! Pinned values must survive GC cycles. The fuzz-style test
//! allocates 100k pairs while holding a pin and asserts the pin's
//! contents are intact at the end. RAII drop semantics ensure the
//! pin is released when the guard goes out of scope.

use cs_core::{Number, Value};
use cs_runtime::Runtime;

#[test]
fn pin_keeps_value_alive_across_explicit_collects() {
    let mut rt = Runtime::new();

    // Allocate a pair, pin it.
    let v = rt.eval_str("<pin>", "(cons 'tag 42)").unwrap();
    let p = rt.pin(v.clone());

    // Run several explicit collects.
    for _ in 0..10 {
        rt.collect();
    }

    // Pinned slot still holds the original pair.
    let stored = p.value();
    let s = rt.format_value(&stored, cs_core::WriteMode::Write);
    assert_eq!(s, "(tag . 42)");
}

#[test]
fn pin_drop_releases_root() {
    let mut rt = Runtime::new();
    assert_eq!(rt.pin_count(), 0);

    let v = rt.eval_str("<pin>", "(list 1 2 3)").unwrap();
    {
        let _p = rt.pin(v.clone());
        assert_eq!(rt.pin_count(), 1);
    }
    // Pinned drop ran -> count back to 0.
    assert_eq!(rt.pin_count(), 0);
}

#[test]
fn multiple_pins_have_distinct_ids_and_release_independently() {
    let mut rt = Runtime::new();
    let v1 = rt.eval_str("<pin>", "(list 1)").unwrap();
    let v2 = rt.eval_str("<pin>", "(list 2)").unwrap();
    let v3 = rt.eval_str("<pin>", "(list 3)").unwrap();

    let p1 = rt.pin(v1);
    let p2 = rt.pin(v2);
    let p3 = rt.pin(v3);
    assert_eq!(rt.pin_count(), 3);
    assert_ne!(p1.id(), p2.id());
    assert_ne!(p2.id(), p3.id());
    assert_ne!(p1.id(), p3.id());

    drop(p2);
    assert_eq!(rt.pin_count(), 2);

    drop(p1);
    drop(p3);
    assert_eq!(rt.pin_count(), 0);
}

#[test]
fn pin_survives_100k_intervening_allocations() {
    // The headline fuzz test from M5b spec FR-3:
    // hold a pin while allocating ~100k pairs and trigger collects
    // along the way. The pin's content must be unchanged at the
    // end.
    let mut rt = Runtime::new();
    let v = rt.eval_str("<pin>", "(cons 'pinned-marker 12345)").unwrap();
    let p = rt.pin(v);

    // Build 100 batches of 1000-element lists, collecting between
    // each batch. Each batch allocates 1000 pairs that go out of
    // scope and become eligible for sweep.
    for batch in 0..100 {
        let prog = format!(
            "(define junk-{batch} (let loop ((i 1000) (acc '())) \
                (if (= i 0) acc (loop (- i 1) (cons i acc))))) \
             (set! junk-{batch} '())",
        );
        rt.eval_str("<pin-fuzz>", &prog).unwrap();
        rt.collect();
    }

    // Pinned slot still holds the original tagged pair.
    let stored = p.value();
    let s = rt.format_value(&stored, cs_core::WriteMode::Write);
    assert_eq!(
        s, "(pinned-marker . 12345)",
        "pin contents drifted after 100k allocations"
    );
}

#[test]
fn pin_value_accessor_returns_correct_data() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<pin>", "42").unwrap();
    let p = rt.pin(v);
    match p.value() {
        Value::Number(Number::Fixnum(42)) => {}
        other => panic!("expected 42, got {:?}", other),
    }
}

#[test]
fn pin_then_evaluate_more_code() {
    // Realistic FFI use: host pins a value, then runs more Scheme
    // code that might allocate. The pin must survive.
    let mut rt = Runtime::new();
    let pinned_list = rt.eval_str("<pin>", "(list 'a 'b 'c)").unwrap();
    let p = rt.pin(pinned_list);

    // Run more code that allocates.
    rt.eval_str(
        "<pin>",
        "(define more (let loop ((i 100) (acc '())) \
            (if (= i 0) acc (loop (- i 1) (cons i acc)))))",
    )
    .unwrap();
    rt.collect();

    // Pinned value still intact.
    let s = rt.format_value(&p.value(), cs_core::WriteMode::Write);
    assert_eq!(s, "(a b c)");
}
