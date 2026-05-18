//! Deep promotion of region-allocated values to Rc storage.
//!
//! Layer 3 (region-memory) needs an "escape hatch": when a
//! value provably outlives its region, the region's bulk-free
//! would orphan still-reachable handles. `Promote::promote_deep`
//! traverses a `Value` tree and replaces every Region-backed
//! `Gc<T>` it finds with a fresh Rc-backed equivalent — same
//! payload, same structural shape, just relocated to the global
//! heap.
//!
//! # When this is invoked
//!
//! - **Layer 5 (escape analysis, forthcoming)** — at allocation
//!   sites the compiler proves escape, the lowered code emits
//!   the allocation as Rc directly (no promotion needed). Where
//!   escape is conditional, the runtime calls `promote_deep` on
//!   the value before crossing the region boundary.
//! - **Manual region users** — when explicitly handing a value
//!   out of a region's scope, call `promote_deep` first.
//!
//! # Shape preservation
//!
//! Promotion preserves payload values bit-for-bit but allocates
//! fresh `Gc<T>` handles. Pointer identity (`eq?`, `Gc::ptr_eq`)
//! does NOT survive promotion — that's intrinsic: the new
//! handle lives at a different address. Programs relying on
//! object identity across a region boundary must `promote_deep`
//! before they record the identity.

#![cfg(feature = "regions")]

use std::cell::RefCell;

use crate::{Hashtable, Pair, Port, Promise, PromiseState, Value};

/// Deep-promote a value to Rc storage. Recursively descends
/// through every heap-bearing variant, promoting any Region-
/// backed `Gc<T>` it encounters. No-op for fully-Rc values.
///
/// Leaf variants (`Null`, `Number`, `Boolean`, …) and
/// `Procedure` (`Rc<dyn Procedure>` is already global) are
/// passed through unchanged.
pub trait Promote {
    fn promote_deep(&mut self);
}

/// Return a fresh, fully-Rc-backed copy of `v` — never
/// mutates the source. Use this when you need the result
/// to be safe across a region drop AND the caller might
/// hold parallel references to the source value (e.g., the
/// VM's value-passing chain).
///
/// `promote_deep` only mutates the handles it can reach
/// from `self`. If the VM has independently cloned the same
/// `Gc` elsewhere, that clone still dangles after region
/// drop. `to_rc_deep` sidesteps this by building a brand-new
/// value tree where every `Gc` is freshly Rc-allocated,
/// regardless of what the source's handles look like.
pub fn to_rc_deep(v: &Value) -> Value {
    match v {
        Value::Pair(p) => {
            let car = to_rc_deep(&p.car());
            let cdr = to_rc_deep(&p.cdr());
            Value::Pair(Pair::new(car, cdr))
        }
        Value::Vector(g) => {
            let cloned: Vec<Value> = g.borrow().iter().map(to_rc_deep).collect();
            Value::Vector(cs_gc::Gc::new(RefCell::new(cloned)))
        }
        Value::String(g) => {
            let cloned: String = g.borrow().clone();
            Value::String(cs_gc::Gc::new(RefCell::new(cloned)))
        }
        Value::ByteVector(g) => {
            let cloned: Vec<u8> = g.borrow().clone();
            Value::ByteVector(cs_gc::Gc::new(RefCell::new(cloned)))
        }
        Value::Hashtable(h) => {
            let items: Vec<(Value, Value)> = h
                .items
                .borrow()
                .iter()
                .map(|(k, val)| (to_rc_deep(k), to_rc_deep(val)))
                .collect();
            let new_ht = match (&h.eq_kind, &h.custom) {
                (crate::HtEqKind::Custom, Some(cf)) => {
                    Hashtable::new_custom(to_rc_deep(&cf.hash), to_rc_deep(&cf.equiv))
                }
                (kind, _) => Hashtable::new(*kind),
            };
            *new_ht.items.borrow_mut() = items;
            Value::Hashtable(new_ht)
        }
        Value::Promise(p) => {
            let new_state = match &*p.state.borrow() {
                PromiseState::Pending(v) => PromiseState::Pending(to_rc_deep(v)),
                PromiseState::Forced(v) => PromiseState::Forced(to_rc_deep(v)),
            };
            Value::Promise(cs_gc::Gc::new(Promise {
                state: RefCell::new(new_state),
            }))
        }
        Value::Port(_) => {
            // Ports hold OS resources (file handles, etc.) —
            // can't safely deep-clone. Caller must not
            // return Ports from a region scope OR must
            // ensure the port outlives the region (typically
            // by allocating it via global Rc to start with,
            // which is what `(open-input-file)` etc. already
            // do — Port construction routes to Gc::new not
            // Gc::new_in).
            v.clone()
        }
        // Leaves and Procedure (already Rc<dyn>): just clone.
        // Identifier is leaf-shaped (Symbol + u64 mark).
        Value::Null
        | Value::Unspecified
        | Value::Eof
        | Value::Boolean(_)
        | Value::Character(_)
        | Value::Symbol(_)
        | Value::Identifier { .. }
        | Value::Number(_)
        | Value::Procedure(_) => v.clone(),
    }
}

impl Promote for Value {
    fn promote_deep(&mut self) {
        match self {
            Value::Pair(p) => {
                if cs_gc::Gc::is_region(p) {
                    // Extract contents via the safe accessors
                    // (these handle the weak-tombstone case
                    // already), recursively promote, then
                    // re-allocate as Rc-backed.
                    let mut car = p.car();
                    let mut cdr = p.cdr();
                    car.promote_deep();
                    cdr.promote_deep();
                    *p = Pair::new(car, cdr);
                } else {
                    // Already Rc; still descend in case inner
                    // values contain Region handles.
                    //
                    // Use the safe accessors + setters (not
                    // borrow_mut) so a concurrent borrow from
                    // the VM's value-passing chain doesn't
                    // RefCell-panic. Reads via car()/cdr() are
                    // borrow-and-release; writes via set_car/
                    // set_cdr re-borrow briefly.
                    let mut car = p.car();
                    let mut cdr = p.cdr();
                    car.promote_deep();
                    cdr.promote_deep();
                    p.set_car(car);
                    p.set_cdr(cdr);
                }
            }
            Value::Vector(v) => {
                if cs_gc::Gc::is_region(v) {
                    let mut cloned: Vec<Value> = v.borrow().clone();
                    for elem in cloned.iter_mut() {
                        elem.promote_deep();
                    }
                    *v = cs_gc::Gc::new(RefCell::new(cloned));
                } else {
                    // Same borrow-discipline rationale as the
                    // Pair Rc-arm above: snapshot then restore
                    // via interior swap, so descent into inner
                    // values doesn't re-enter the borrow.
                    let mut snapshot: Vec<Value> = v.borrow().clone();
                    for elem in snapshot.iter_mut() {
                        elem.promote_deep();
                    }
                    *v.borrow_mut() = snapshot;
                }
            }
            Value::String(s) => {
                if cs_gc::Gc::is_region(s) {
                    let cloned: String = s.borrow().clone();
                    *s = cs_gc::Gc::new(RefCell::new(cloned));
                }
                // String contents are leaf — no descent needed.
            }
            Value::ByteVector(b) => {
                if cs_gc::Gc::is_region(b) {
                    let cloned: Vec<u8> = b.borrow().clone();
                    *b = cs_gc::Gc::new(RefCell::new(cloned));
                }
                // ByteVector contents are leaf.
            }
            Value::Hashtable(h) => {
                if cs_gc::Gc::is_region(h) {
                    // Build fresh Rc-backed hashtable with
                    // the same eq_kind and (recursively-
                    // promoted) items + custom fns.
                    let mut items: Vec<(Value, Value)> = h.items.borrow().clone();
                    for (k, val) in items.iter_mut() {
                        k.promote_deep();
                        val.promote_deep();
                    }
                    let new_ht = match (&h.eq_kind, &h.custom) {
                        (crate::HtEqKind::Custom, Some(cf)) => {
                            let mut hash = cf.hash.clone();
                            let mut equiv = cf.equiv.clone();
                            hash.promote_deep();
                            equiv.promote_deep();
                            Hashtable::new_custom(hash, equiv)
                        }
                        (kind, _) => Hashtable::new(*kind),
                    };
                    *new_ht.items.borrow_mut() = items;
                    *h = new_ht;
                } else {
                    // Rc-backed; descend in case items contain
                    // region handles. Snapshot-then-restore
                    // pattern (same rationale as Pair/Vector
                    // Rc-arms) to avoid borrow_mut held across
                    // recursive promote_deep that may re-enter.
                    let mut items: Vec<(Value, Value)> = h.items.borrow().clone();
                    for (k, val) in items.iter_mut() {
                        k.promote_deep();
                        val.promote_deep();
                    }
                    *h.items.borrow_mut() = items;
                    // (custom fns are typically Rc Procedures
                    //  already; no work needed)
                }
            }
            Value::Promise(p) => {
                if cs_gc::Gc::is_region(p) {
                    // Snapshot state, recursively promote
                    // whatever Value is inside, then build a
                    // fresh Rc-backed Promise.
                    let new_state = match &*p.state.borrow() {
                        PromiseState::Pending(v) => {
                            let mut v = v.clone();
                            v.promote_deep();
                            PromiseState::Pending(v)
                        }
                        PromiseState::Forced(v) => {
                            let mut v = v.clone();
                            v.promote_deep();
                            PromiseState::Forced(v)
                        }
                    };
                    *p = cs_gc::Gc::new(Promise {
                        state: RefCell::new(new_state),
                    });
                } else {
                    let mut state = p.state.borrow_mut();
                    match &mut *state {
                        PromiseState::Pending(v) | PromiseState::Forced(v) => {
                            v.promote_deep();
                        }
                    }
                }
            }
            Value::Port(p) => {
                if cs_gc::Gc::is_region(p) {
                    // Port variants are leaf (no Value
                    // children); we just need to re-allocate
                    // the outer Gc as Rc. The content can be
                    // moved through a fresh clone since each
                    // Port-state struct is owned content.
                    let new_port = match &**p {
                        Port::StringInput(s) => {
                            Port::StringInput(RefCell::new(s.borrow().clone()))
                        }
                        Port::StringOutput(s) => {
                            Port::StringOutput(RefCell::new(s.borrow().clone()))
                        }
                        Port::ByteVectorInput(b) => {
                            Port::ByteVectorInput(RefCell::new(b.borrow().clone()))
                        }
                        Port::ByteVectorOutput(b) => {
                            Port::ByteVectorOutput(RefCell::new(b.borrow().clone()))
                        }
                        Port::FileOutput(f) => Port::FileOutput(RefCell::new(f.borrow().clone())),
                    };
                    *p = cs_gc::Gc::new(new_port);
                }
                // Rc-backed: nothing to descend into.
            }
            // Leaves: no heap pointers.
            // Identifier is leaf (Symbol + u64 mark).
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::Number(_)
            // Procedure is Rc<dyn>; already global heap.
            | Value::Procedure(_) => {}
        }
    }
}
