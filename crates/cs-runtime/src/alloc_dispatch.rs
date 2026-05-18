//! Lifetime-aware allocation dispatch (escape-analysis spec
//! iter 6, layer 5 of the unified memory architecture).
//!
//! Each allocation primitive that the typer can classify
//! (`cons`, `make-vector`, `make-string`, `make-bytevector`,
//! `make-hashtable`, `vector`, `list`) gets a `*_dispatch`
//! wrapper that accepts a [`cs_rir::Lifetime`] and routes to
//! the right allocator:
//!
//! - `Lifetime::Region(_)` — `Pair::new_in(current_region, …)`
//!   via the region-scope stack from `crate::regions`. Errors
//!   if no region is in scope.
//! - `Lifetime::Rc` — the existing `Pair::new(…)` (global Rc
//!   heap). Same as the un-dispatched builtin.
//! - `Lifetime::Traced` — falls back to `Rc` until the
//!   `tracing-revival` spec lands.
//! - `Lifetime::Stack` — treated as `Region` (no stack-alloc
//!   yet); errors with the same diagnostic if no region scope.
//!
//! The undecorated builtins (`b_cons`, etc. in
//! `crates/cs-runtime/src/builtins/mod.rs`) stay unchanged
//! and continue to default to Rc; they're what unannotated
//! Scheme code paths exercise. The dispatch wrappers are
//! consumed by the bytecode VM (when iter 6's opcode
//! extensions land) and by AOT-emitted code (similar).
//!
//! Gated on `feature = "regions"` (forwarded from cs-gc).

#![cfg(feature = "regions")]

use std::cell::RefCell;

use cs_core::{Hashtable, HtEqKind, Pair, Value};
use cs_gc::Gc;
use cs_rir::Lifetime;

use crate::regions::current_region;

/// Error returned when a `Region`/`Stack` lifetime is
/// requested without an enclosing region scope. Programs
/// that reach this state are typer-bug-class — the inferencer
/// must enter a `RegionScope` before emitting a
/// `Lifetime::Region(_)` allocation.
///
/// Uses [`region_lookup_diagnostic`] to tailor the hint:
/// from non-actor context the user most likely missed
/// `(with-region …)`; from an actor task the task-local
/// stack may have been lost across a yield boundary (see
/// parallel-runtime C3.2 partial — full migration is gated
/// on `cs_gc::Region: Send`).
fn no_region_err(op: &'static str) -> String {
    use crate::regions::{region_lookup_diagnostic, RegionLookupDiagnostic};
    let hint = match region_lookup_diagnostic() {
        RegionLookupDiagnostic::NoTlsScope => "no `(with-region …)` in scope on this thread",
        RegionLookupDiagnostic::NoTaskScope => {
            "inside an actor body but the task-local region stack is empty — \
             either the actor's body opened no `(with-region …)`, or it did \
             open one and the region was lost across an await/yield boundary \
             (parallel-runtime C3.2 known limitation, gated on \
             `cs_gc::Region: Send`)"
        }
    };
    format!("{op}: Lifetime::Region(_) without enclosing RegionScope — {hint}")
}

/// `(cons car cdr)` lifetime-aware. Allocates a [`Pair`] in
/// the tier dictated by `lifetime`.
pub fn cons_in(lifetime: Lifetime, car: Value, cdr: Value) -> Result<Value, String> {
    let p = match lifetime {
        Lifetime::Region(_) | Lifetime::Stack => {
            let region = current_region().ok_or_else(|| no_region_err("cons"))?;
            Pair::new_in(&region, car, cdr)
        }
        Lifetime::Rc | Lifetime::Traced => Pair::new(car, cdr),
    };
    Ok(Value::Pair(p))
}

/// `(make-vector n fill)` lifetime-aware.
pub fn make_vector_in(lifetime: Lifetime, n: usize, fill: Value) -> Result<Value, String> {
    let v: Vec<Value> = vec![fill; n];
    let g: Gc<RefCell<Vec<Value>>> = match lifetime {
        Lifetime::Region(_) | Lifetime::Stack => {
            let region = current_region().ok_or_else(|| no_region_err("make-vector"))?;
            Gc::new_in(&region, RefCell::new(v))
        }
        Lifetime::Rc | Lifetime::Traced => Gc::new(RefCell::new(v)),
    };
    Ok(Value::Vector(g))
}

/// `(vector …)` lifetime-aware variadic.
pub fn vector_in(lifetime: Lifetime, elems: Vec<Value>) -> Result<Value, String> {
    let g: Gc<RefCell<Vec<Value>>> = match lifetime {
        Lifetime::Region(_) | Lifetime::Stack => {
            let region = current_region().ok_or_else(|| no_region_err("vector"))?;
            Gc::new_in(&region, RefCell::new(elems))
        }
        Lifetime::Rc | Lifetime::Traced => Gc::new(RefCell::new(elems)),
    };
    Ok(Value::Vector(g))
}

/// `(make-string n fill)` lifetime-aware.
pub fn make_string_in(lifetime: Lifetime, n: usize, fill: char) -> Result<Value, String> {
    let s: String = std::iter::repeat(fill).take(n).collect();
    let g: Gc<RefCell<String>> = match lifetime {
        Lifetime::Region(_) | Lifetime::Stack => {
            let region = current_region().ok_or_else(|| no_region_err("make-string"))?;
            Gc::new_in(&region, RefCell::new(s))
        }
        Lifetime::Rc | Lifetime::Traced => Gc::new(RefCell::new(s)),
    };
    Ok(Value::String(g))
}

/// `(make-bytevector n fill)` lifetime-aware.
pub fn make_bytevector_in(lifetime: Lifetime, n: usize, fill: u8) -> Result<Value, String> {
    let v: Vec<u8> = vec![fill; n];
    let g: Gc<RefCell<Vec<u8>>> = match lifetime {
        Lifetime::Region(_) | Lifetime::Stack => {
            let region = current_region().ok_or_else(|| no_region_err("make-bytevector"))?;
            Gc::new_in(&region, RefCell::new(v))
        }
        Lifetime::Rc | Lifetime::Traced => Gc::new(RefCell::new(v)),
    };
    Ok(Value::ByteVector(g))
}

/// `(make-hashtable eq-kind)` lifetime-aware.
pub fn make_hashtable_in(lifetime: Lifetime, eq_kind: HtEqKind) -> Result<Value, String> {
    let ht = Hashtable {
        items: RefCell::new(Vec::new()),
        eq_kind,
        custom: None,
    };
    let g: Gc<Hashtable> = match lifetime {
        Lifetime::Region(_) | Lifetime::Stack => {
            let region = current_region().ok_or_else(|| no_region_err("make-hashtable"))?;
            Gc::new_in(&region, ht)
        }
        Lifetime::Rc | Lifetime::Traced => Gc::new(ht),
    };
    Ok(Value::Hashtable(g))
}

/// `(list …)` lifetime-aware. Builds the list pairs in the
/// requested tier (each pair allocates via the same dispatch).
pub fn list_in(lifetime: Lifetime, elems: Vec<Value>) -> Result<Value, String> {
    let mut acc = Value::Null;
    for e in elems.into_iter().rev() {
        acc = cons_in(lifetime, e, acc)?;
    }
    Ok(acc)
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use cs_gc::Region;
    use cs_rir::RegionTag;

    use crate::regions::RegionScope;

    use super::*;

    #[test]
    fn cons_rc_lifetime_uses_global_heap() {
        let v = cons_in(Lifetime::Rc, Value::Boolean(true), Value::Null).unwrap();
        if let Value::Pair(p) = &v {
            assert!(!Gc::is_region(p));
        } else {
            panic!();
        }
    }

    #[test]
    fn cons_region_lifetime_with_scope_uses_region() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        let v = cons_in(
            Lifetime::Region(RegionTag(0)),
            Value::Boolean(true),
            Value::Null,
        )
        .unwrap();
        if let Value::Pair(p) = &v {
            assert!(Gc::is_region(p), "expected region-backed Pair");
        }
    }

    #[test]
    fn cons_region_lifetime_without_scope_errors() {
        let err = cons_in(
            Lifetime::Region(RegionTag(0)),
            Value::Boolean(true),
            Value::Null,
        )
        .expect_err("expected error");
        assert!(err.contains("RegionScope"), "got: {err}");
        // C3.3 diagnostic: TLS-context error names the user's
        // most-likely cause (forgot `(with-region …)`).
        assert!(err.contains("with-region"), "got: {err}");
    }

    /// parallel-runtime C3.3 — inside an actor task with an
    /// empty task-local stack, the diagnostic should mention
    /// the actor-context cause, not the TLS one.
    #[cfg(feature = "actor")]
    #[test]
    fn cons_region_lifetime_inside_task_scope_errors_with_actor_hint() {
        use crate::regions::REGION_STACK_TASK;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let stack = std::cell::RefCell::new(Vec::new());
            REGION_STACK_TASK
                .scope(stack, async {
                    let err = cons_in(
                        Lifetime::Region(RegionTag(0)),
                        Value::Boolean(true),
                        Value::Null,
                    )
                    .expect_err("expected error");
                    assert!(
                        err.contains("actor body"),
                        "expected actor-context hint, got: {err}"
                    );
                    assert!(
                        err.contains("Region: Send"),
                        "expected Send-Region note, got: {err}"
                    );
                })
                .await;
        });
    }

    #[test]
    fn make_vector_region_lifetime_with_scope() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        let v = make_vector_in(Lifetime::Region(RegionTag(0)), 3, Value::Boolean(false)).unwrap();
        if let Value::Vector(gv) = &v {
            assert!(Gc::is_region(gv));
            assert_eq!(gv.borrow().len(), 3);
        }
    }

    #[test]
    fn make_string_region_lifetime() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        let v = make_string_in(Lifetime::Region(RegionTag(0)), 5, 'x').unwrap();
        if let Value::String(s) = &v {
            assert!(Gc::is_region(s));
            assert_eq!(&*s.borrow(), "xxxxx");
        }
    }

    #[test]
    fn make_bytevector_region_lifetime() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        let v = make_bytevector_in(Lifetime::Region(RegionTag(0)), 4, 0xAB).unwrap();
        if let Value::ByteVector(b) = &v {
            assert!(Gc::is_region(b));
            assert_eq!(&*b.borrow(), &[0xAB; 4]);
        }
    }

    #[test]
    fn make_hashtable_region_lifetime() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        let v = make_hashtable_in(Lifetime::Region(RegionTag(0)), HtEqKind::Eqv).unwrap();
        if let Value::Hashtable(h) = &v {
            assert!(Gc::is_region(h));
        }
    }

    #[test]
    fn list_region_lifetime_all_pairs_region_backed() {
        let r = Rc::new(Region::new());
        let _g = RegionScope::enter(Rc::clone(&r));
        let elems = vec![Value::Boolean(true), Value::Boolean(false), Value::Null];
        let v = list_in(Lifetime::Region(RegionTag(0)), elems).unwrap();
        // Walk the list, assert every pair is region-backed.
        let mut cur = v;
        let mut count = 0;
        loop {
            match cur {
                Value::Pair(p) => {
                    assert!(Gc::is_region(&p));
                    count += 1;
                    cur = p.cdr();
                }
                Value::Null => break,
                _ => panic!("expected pair or null"),
            }
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn traced_lifetime_falls_back_to_rc() {
        // Until tracing-revival ships.
        let v = cons_in(Lifetime::Traced, Value::Boolean(true), Value::Null).unwrap();
        if let Value::Pair(p) = &v {
            assert!(!Gc::is_region(p));
        }
    }
}
