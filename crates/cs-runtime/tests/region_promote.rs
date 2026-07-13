//! Integration tests for `Promote::promote_deep` (region-memory
//! iter 4). Exercises FR-4: deep-promote a Value subtree from
//! region storage into Rc storage so the value survives the
//! region's drop.
//!
//! Build with `--features regions`.

#![cfg(feature = "regions")]

use std::cell::RefCell;

use cs_core::{Hashtable, HtEqKind, Number, Pair, Promote, Symbol, Value};
use cs_gc::{Gc, Region};

fn n(v: i64) -> Value {
    Value::Fixnum(v)
}

/// Helper: build a region-allocated Pair. The caller can then
/// drop the region first to verify use-after-region behavior.
fn alloc_pair_in_region(region: &Region, car: Value, cdr: Value) -> Gc<Pair> {
    Pair::new_in(region, car, cdr)
}

#[test]
fn single_level_pair_promotion_survives_region_drop() {
    let mut v = {
        let region = Region::new();
        let p = alloc_pair_in_region(&region, n(1), n(2));
        assert!(Gc::is_region(&p));
        let mut v = Value::Pair(p);
        v.promote_deep();
        // After promotion, the outer Pair is Rc-backed.
        if let Value::Pair(ref pp) = v {
            assert!(!Gc::is_region(pp));
        }
        v
        // region drops here.
    };
    // Touch v after region drop — must succeed.
    if let Value::Pair(p) = &v {
        assert!(matches!(
            p.car(),
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
        ));
        assert!(matches!(
            p.cdr(),
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
        ));
    } else {
        panic!("expected Pair");
    }
    // Run promote_deep again — idempotent no-op.
    v.promote_deep();
    if let Value::Pair(p) = &v {
        assert!(matches!(
            p.car(),
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
        ));
        assert!(matches!(
            p.cdr(),
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
        ));
    } else {
        unreachable!();
    }
}

#[test]
fn two_level_pair_promotion_descends() {
    let v = {
        let region = Region::new();
        let inner = alloc_pair_in_region(&region, n(10), n(20));
        let outer = alloc_pair_in_region(&region, n(0), Value::Pair(inner));
        assert!(Gc::is_region(&outer));
        let mut v = Value::Pair(outer);
        v.promote_deep();
        // Verify both levels are now Rc-backed.
        if let Value::Pair(p_outer) = &v {
            assert!(!Gc::is_region(p_outer));
            if let Value::Pair(p_inner) = p_outer.cdr() {
                assert!(!Gc::is_region(&p_inner));
            } else {
                panic!("expected nested Pair on cdr");
            }
        }
        v
        // region drops here.
    };
    // Walk the tree post-region-drop.
    if let Value::Pair(p_outer) = &v {
        assert!(matches!(
            p_outer.car(),
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
        ));
        if let Value::Pair(p_inner) = p_outer.cdr() {
            assert!(matches!(
                p_inner.car(),
                Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
            ));
            assert!(matches!(
                p_inner.cdr(),
                Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
            ));
        } else {
            panic!();
        }
    } else {
        unreachable!();
    }
}

#[test]
fn mixed_region_and_rc_handled() {
    // Outer Pair in a region, inner Pair already in Rc.
    let rc_inner = Pair::new(n(100), n(200));
    assert!(!Gc::is_region(&rc_inner));
    let v = {
        let region = Region::new();
        let outer = alloc_pair_in_region(&region, n(0), Value::Pair(rc_inner.clone()));
        let mut v = Value::Pair(outer);
        v.promote_deep();
        v
        // region drops here.
    };
    // Outer is now Rc, inner was already Rc.
    if let Value::Pair(p) = &v {
        assert!(!Gc::is_region(p));
        if let Value::Pair(inner) = p.cdr() {
            assert!(!Gc::is_region(&inner));
            // The original rc_inner and the cdr now point at
            // distinct allocations (promote always re-alloc's
            // the outer; inner was cloned for the new outer's
            // car). Identity isn't preserved across promote.
            assert!(matches!(
                inner.car(),
                Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
            ));
        }
    }
}

#[test]
fn vector_promotion_descends_into_elements() {
    let v = {
        let region = Region::new();
        let inner_pair = alloc_pair_in_region(&region, n(1), n(2));
        let elems = vec![n(7), Value::Pair(inner_pair), Value::Boolean(true)];
        let region_vec: Gc<RefCell<Vec<Value>>> = Gc::new_in(&region, RefCell::new(elems));
        assert!(Gc::is_region(&region_vec));
        let mut v = Value::Vector(region_vec);
        v.promote_deep();
        v
        // region drops here.
    };
    if let Value::Vector(vec_gc) = &v {
        assert!(!Gc::is_region(vec_gc));
        let borrowed = vec_gc.borrow();
        assert_eq!(borrowed.len(), 3);
        if let Value::Pair(p) = &borrowed[1] {
            assert!(!Gc::is_region(p));
            assert!(matches!(
                p.car(),
                Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
            ));
            assert!(matches!(
                p.cdr(),
                Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
            ));
        } else {
            panic!("expected Pair at index 1");
        }
    } else {
        unreachable!();
    }
}

#[test]
fn string_promotion_clones_payload() {
    let v = {
        let region = Region::new();
        let s: Gc<RefCell<cs_core::CsStr>> =
            Gc::new_in(&region, RefCell::new(cs_core::CsStr::new("hello world")));
        assert!(Gc::is_region(&s));
        let mut v = Value::String(s);
        v.promote_deep();
        v
    };
    if let Value::String(s) = &v {
        assert!(!Gc::is_region(s));
        assert_eq!(&*s.borrow(), "hello world");
    }
}

#[test]
fn hashtable_promotion_descends_into_items() {
    let v = {
        let region = Region::new();
        let inner = alloc_pair_in_region(&region, n(42), n(43));
        let ht = Hashtable::from_parts(
            vec![
                (Value::Symbol(Symbol(1)), Value::Pair(inner)),
                (n(0), Value::Boolean(false)),
            ],
            HtEqKind::Eqv,
            None,
            std::collections::HashMap::new(),
        );
        let h: Gc<Hashtable> = Gc::new_in(&region, ht);
        assert!(Gc::is_region(&h));
        let mut v = Value::Hashtable(h);
        v.promote_deep();
        v
    };
    if let Value::Hashtable(h) = &v {
        assert!(!Gc::is_region(h));
        let items = h.items.borrow();
        assert_eq!(items.len(), 2);
        if let Value::Pair(p) = &items[0].1 {
            assert!(!Gc::is_region(p));
            assert!(matches!(
                p.car(),
                Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
            ));
        } else {
            panic!();
        }
    }
}

#[test]
fn leaf_values_are_unchanged_by_promote() {
    let originals = vec![
        Value::Null,
        Value::Unspecified,
        Value::Eof,
        Value::Boolean(true),
        n(123),
        Value::Character('x'),
    ];
    for mut v in originals {
        let before = format!("{v:?}");
        v.promote_deep();
        let after = format!("{v:?}");
        assert_eq!(before, after, "leaf value mutated by promote_deep");
    }
}
