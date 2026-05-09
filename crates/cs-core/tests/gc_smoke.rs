//! Smoke tests for the `Gc<T>` re-export from cs-gc.
//!
//! These don't exercise the GC machinery deeply (cs-gc has its own
//! tests for that). The goal is to confirm the API shape works for
//! the patterns `Value`'s heap variants need: Clone, Deref, Eq, and
//! interior mutability via `RefCell`.

use std::cell::RefCell;

use cs_core::{Gc, Trace};

#[derive(Debug)]
struct Pair {
    car: i64,
    cdr: i64,
}

impl Trace for Pair {
    fn trace(&self, _marker: &mut cs_core::Marker) {
        // Leaf — no Gc<T> children.
    }
}

#[test]
fn gc_drop_in_for_rc_in_simple_record() {
    let p = Gc::new(Pair { car: 1, cdr: 2 });
    let q = p.clone();
    assert_eq!(p.car, 1);
    assert_eq!(q.cdr, 2);
    assert!(Gc::ptr_eq(&p, &q));
}

#[test]
fn gc_with_refcell_for_mutable_payload() {
    // Mirrors the `Rc<RefCell<String>>` pattern used for Value::String.
    let s: Gc<RefCell<String>> = Gc::new(RefCell::new("hello".to_string()));
    let t = s.clone();
    s.borrow_mut().push_str(" world");
    // Mutation visible via the clone (shared cell).
    assert_eq!(*t.borrow(), "hello world");
}

#[test]
fn gc_with_refcell_vec_for_vector_value() {
    // Mirrors the `Rc<RefCell<Vec<Value>>>` pattern used for
    // Value::Vector.
    let v: Gc<RefCell<Vec<i64>>> = Gc::new(RefCell::new(vec![1, 2, 3]));
    v.borrow_mut().push(4);
    assert_eq!(v.borrow().len(), 4);
    let v2 = v.clone();
    v2.borrow_mut().push(5);
    assert_eq!(v.borrow().len(), 5);
}

#[test]
fn ptr_eq_matches_pointer_identity() {
    let a = Gc::new(Pair { car: 1, cdr: 2 });
    let b = Gc::new(Pair { car: 1, cdr: 2 }); // same payload, distinct allocation
    assert!(!Gc::ptr_eq(&a, &b));
    let c = a.clone();
    assert!(Gc::ptr_eq(&a, &c));
}
