//! parallel-runtime spec C4.2 — `CycleChildren` impl coverage
//! for Pair / Vector / Hashtable / Promise.
//!
//! Each impl should emit exactly the child heap addresses that
//! the Bacon-Rajan trial-deletion walk needs to decrement. Leaf
//! values (numbers, symbols, booleans) MUST NOT yield addresses.

#![cfg(feature = "tracing-cycle-collector")]

use std::cell::RefCell;
use std::rc::Rc;

use cs_core::{Hashtable, HtEqKind, Pair, Promise, Value};
use cs_gc::cycle_registry::CycleChildren;
use cs_gc::Gc;

/// Collect every address emitted by the impl into a sorted Vec
/// so tests can compare against an expected set.
fn collected_addrs<T: CycleChildren + ?Sized>(node: &T) -> Vec<usize> {
    let mut out = Vec::new();
    node.cycle_children(&mut |a| out.push(a));
    out.sort();
    out
}

#[test]
fn pair_with_two_leaves_yields_nothing() {
    // (cons 1 2) — neither slot is a heap container, so the
    // BR walk should see no children.
    let p = Pair::new(
        Value::Number(cs_core::Number::Fixnum(1)),
        Value::Number(cs_core::Number::Fixnum(2)),
    );
    assert_eq!(collected_addrs(&*p), Vec::<usize>::new());
}

#[test]
fn pair_with_two_heap_children_yields_both() {
    // (cons (cons 1 2) (cons 3 4)) — both car and cdr point at
    // heap pairs.
    let a = Pair::new(
        Value::Number(cs_core::Number::Fixnum(1)),
        Value::Number(cs_core::Number::Fixnum(2)),
    );
    let b = Pair::new(
        Value::Number(cs_core::Number::Fixnum(3)),
        Value::Number(cs_core::Number::Fixnum(4)),
    );
    let outer = Pair::new(Value::Pair(a.clone()), Value::Pair(b.clone()));

    let got = collected_addrs(&*outer);
    let mut expected = vec![Gc::as_addr(&a), Gc::as_addr(&b)];
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn pair_self_cycle_yields_self_addr_twice() {
    // (define p (cons … …)) ; (set-car! p p) ; (set-cdr! p p)
    // — both slots point at the same pair. The walk emits the
    // address twice; BR's mark_gray dedups via the registry's
    // candidate set, so double-emission is fine.
    let p = Pair::new(Value::Unspecified, Value::Unspecified);
    p.set_car(Value::Pair(p.clone()));
    p.set_cdr(Value::Pair(p.clone()));

    let got = collected_addrs(&*p);
    assert_eq!(got, vec![Gc::as_addr(&p), Gc::as_addr(&p)]);
}

#[test]
fn vector_emits_each_heap_element_once() {
    // #(<pair> 7 <pair> #t <pair>) — only the three pairs are
    // heap-allocated.
    let p1 = Pair::new(Value::Boolean(true), Value::Null);
    let p2 = Pair::new(Value::Boolean(false), Value::Null);
    let p3 = Pair::new(Value::Boolean(true), Value::Null);

    let v: Gc<RefCell<Vec<Value>>> = Gc::new(RefCell::new(vec![
        Value::Pair(p1.clone()),
        Value::Number(cs_core::Number::Fixnum(7)),
        Value::Pair(p2.clone()),
        Value::Boolean(true),
        Value::Pair(p3.clone()),
    ]));

    let got = collected_addrs(&*v);
    let mut expected = vec![Gc::as_addr(&p1), Gc::as_addr(&p2), Gc::as_addr(&p3)];
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn hashtable_walks_keys_values_and_custom_fns() {
    // Build a hashtable with two pair-keyed pair entries plus
    // user-supplied (hash, equiv) procedures. The walk must
    // emit all six heap addrs (2 keys + 2 vals + 2 custom).
    let kp1 = Pair::new(Value::Boolean(true), Value::Null);
    let vp1 = Pair::new(Value::Boolean(false), Value::Null);
    let kp2 = Pair::new(Value::Number(cs_core::Number::Fixnum(1)), Value::Null);
    let vp2 = Pair::new(Value::Number(cs_core::Number::Fixnum(2)), Value::Null);

    // Custom hash/equiv slots — point at heap pairs as stand-ins
    // for procedures (the trait emits the Value heap_addr; what
    // the slot semantically holds is irrelevant for the walk).
    let hash_proc = Pair::new(Value::Boolean(true), Value::Null);
    let equiv_proc = Pair::new(Value::Boolean(true), Value::Null);

    let ht = Hashtable {
        items: RefCell::new(vec![
            (Value::Pair(kp1.clone()), Value::Pair(vp1.clone())),
            (Value::Pair(kp2.clone()), Value::Pair(vp2.clone())),
        ]),
        eq_kind: HtEqKind::Custom,
        custom: Some(cs_core::CustomHashFns {
            hash: Value::Pair(hash_proc.clone()),
            equiv: Value::Pair(equiv_proc.clone()),
        }),
    };

    let got = collected_addrs(&ht);
    let mut expected = vec![
        Gc::as_addr(&kp1),
        Gc::as_addr(&vp1),
        Gc::as_addr(&kp2),
        Gc::as_addr(&vp2),
        Gc::as_addr(&hash_proc),
        Gc::as_addr(&equiv_proc),
    ];
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn hashtable_no_custom_emits_only_kv() {
    let kp = Pair::new(Value::Boolean(true), Value::Null);
    let vp = Pair::new(Value::Boolean(false), Value::Null);
    let ht = Hashtable {
        items: RefCell::new(vec![(Value::Pair(kp.clone()), Value::Pair(vp.clone()))]),
        eq_kind: HtEqKind::Eq,
        custom: None,
    };
    let got = collected_addrs(&ht);
    let mut expected = vec![Gc::as_addr(&kp), Gc::as_addr(&vp)];
    expected.sort();
    assert_eq!(got, expected);
}

#[test]
fn promise_pending_walks_its_thunk() {
    let thunk_holder = Pair::new(Value::Boolean(true), Value::Null);
    let p = Promise::pending(Value::Pair(thunk_holder.clone()));
    assert_eq!(collected_addrs(&*p), vec![Gc::as_addr(&thunk_holder)]);
}

#[test]
fn promise_forced_walks_its_value() {
    // Build a promise then mutate state to Forced.
    let inner = Pair::new(Value::Number(cs_core::Number::Fixnum(99)), Value::Null);
    let p = Promise::pending(Value::Unspecified);
    *p.state.borrow_mut() = cs_core::PromiseState::Forced(Value::Pair(inner.clone()));
    assert_eq!(collected_addrs(&*p), vec![Gc::as_addr(&inner)]);
}

#[test]
fn promise_with_leaf_payload_emits_nothing() {
    let p = Promise::pending(Value::Number(cs_core::Number::Fixnum(0)));
    assert_eq!(collected_addrs(&*p), Vec::<usize>::new());
}

/// Sanity: heap_addr returns Some for all heap variants and
/// None for all leaf variants — this is the foundation the C4.2
/// walk relies on.
#[test]
fn heap_addr_distinguishes_heap_vs_leaf() {
    // Heap variants.
    let p = Pair::new(Value::Null, Value::Null);
    assert!(Value::Pair(p).heap_addr().is_some());
    let v: Gc<RefCell<Vec<Value>>> = Gc::new(RefCell::new(Vec::new()));
    assert!(Value::Vector(v).heap_addr().is_some());

    // Leaf variants.
    assert!(Value::Null.heap_addr().is_none());
    assert!(Value::Unspecified.heap_addr().is_none());
    assert!(Value::Eof.heap_addr().is_none());
    assert!(Value::Boolean(true).heap_addr().is_none());
    assert!(Value::Character('a').heap_addr().is_none());
    assert!(Value::Number(cs_core::Number::Fixnum(0))
        .heap_addr()
        .is_none());
}

/// `RefCell<T>` and `Vec<T>` blanket impls in cs-gc let
/// container storage participate in the walk. Round-trip:
/// vector → RefCell → Vec → Value → heap_addr.
#[test]
fn vec_blanket_impl_forwards_through_values() {
    let p1 = Pair::new(Value::Null, Value::Null);
    let p2 = Pair::new(Value::Boolean(true), Value::Null);
    let storage: Vec<Value> = vec![Value::Pair(p1.clone()), Value::Pair(p2.clone())];
    // Call cycle_children on the Vec<Value> directly — the
    // blanket impl in cs-gc walks each item.
    let mut got = Vec::new();
    storage.cycle_children(&mut |a| got.push(a));
    got.sort();
    let mut expected = vec![Gc::as_addr(&p1), Gc::as_addr(&p2)];
    expected.sort();
    assert_eq!(got, expected);

    // Keep Rcs alive past the assert.
    let _ = (p1, p2);
}

// Suppress unused-import warning for Rc when the test file
// otherwise doesn't reach into it. Used implicitly by the Gc
// strong-count machinery.
#[allow(dead_code)]
fn _force_rc_use() -> Rc<()> {
    Rc::new(())
}
