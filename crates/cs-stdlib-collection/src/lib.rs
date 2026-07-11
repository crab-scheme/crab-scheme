//! CrabScheme stdlib module: `(crab collection)` — FIFO queue, hash
//! set, priority heap. Iter 12 of the `stdlib-modules` spec.
//!
//! R6RS already covers ordered associations (lists, alists,
//! hashtables) and vectors; this module fills in the data
//! structures every CrabScheme program ends up wanting on top:
//!
//! - `queue` (FIFO, `VecDeque`-backed)
//! - `set` (unordered uniqueness, hashes string elements)
//! - `heap` (priority queue over `BinaryHeap` of fixnums, max-heap;
//!   pair with negation for min-heap)
//!
//! All three return fixnum handles (same slab pattern as
//! cs-stdlib-net). Typed values land with `Value::Opaque`.
//!
//! Hash set + heap restrictions for this iter:
//! - `set-add!` accepts only strings (hashable as `String`).
//!   General-Value sets need a stable hash over Scheme values,
//!   tracked for a follow-up alongside `equal-hash`.
//! - `heap-push!` accepts only fixnums (ordered as i64). General-
//!   Value priority queues need a comparator argument.
//!
//! These restrictions cover the 80% case (string sets, fixnum
//! priority queues) without inviting the full hash/comparator
//! design discussion.
//!
//! ## Registered procedures
//!
//! ### Queue (FIFO)
//!
//! | Name | Args | Returns |
//! |---|---|---|
//! | `queue-new`     | —          | handle |
//! | `queue-push!`   | handle val | unspec |
//! | `queue-pop!`    | handle     | val or #f when empty |
//! | `queue-peek`    | handle     | val or #f when empty |
//! | `queue-length`  | handle     | fixnum |
//! | `queue-empty?`  | handle     | boolean |
//!
//! ### Set (string-keyed)
//!
//! | Name | Args | Returns |
//! |---|---|---|
//! | `set-new`        | —              | handle |
//! | `set-add!`       | handle string  | unspec |
//! | `set-remove!`    | handle string  | boolean (true if present) |
//! | `set-contains?`  | handle string  | boolean |
//! | `set-size`       | handle         | fixnum |
//! | `set->list`      | handle         | list of strings (order unspecified) |
//!
//! ### Heap (max-heap of fixnums)
//!
//! | Name | Args | Returns |
//! |---|---|---|
//! | `heap-new`     | —          | handle |
//! | `heap-push!`   | handle fixnum | unspec |
//! | `heap-pop!`    | handle     | fixnum (max) or #f when empty |
//! | `heap-peek`    | handle     | fixnum or #f when empty |
//! | `heap-length`  | handle     | fixnum |

use std::cell::RefCell;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

enum Slot {
    Queue(VecDeque<Value>),
    Set(HashSet<String>),
    Heap(BinaryHeap<i64>),
}

struct Registry {
    next_id: i64,
    slots: HashMap<i64, Slot>,
}

// Thread-local registry — the queue stores Value, which contains
// Gc (Rc-backed) and so isn't Sync. CrabScheme runs single-
// threaded; pinning the registry to the runtime thread is both
// correct and avoids the Mutex/Sync ceremony.
thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry {
        next_id: 1,
        slots: HashMap::new(),
    });
}

fn with_registry<F, R>(f: F) -> R
where
    F: FnOnce(&mut Registry) -> R,
{
    REGISTRY.with(|cell| f(&mut cell.borrow_mut()))
}

fn insert(slot: Slot) -> i64 {
    with_registry(|r| {
        let id = r.next_id;
        r.next_id += 1;
        r.slots.insert(id, slot);
        id
    })
}

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("queue-new", queue_new),
        UntypedProc::new("queue-push!", queue_push),
        UntypedProc::new("queue-pop!", queue_pop),
        UntypedProc::new("queue-peek", queue_peek),
        UntypedProc::new("queue-length", queue_length),
        UntypedProc::new("queue-empty?", queue_empty_p),
        UntypedProc::new("set-new", set_new),
        UntypedProc::new("set-add!", set_add),
        UntypedProc::new("set-remove!", set_remove),
        UntypedProc::new("set-contains?", set_contains_p),
        UntypedProc::new("set-size", set_size),
        UntypedProc::new("set->list", set_to_list),
        UntypedProc::new("set-union", set_union),
        UntypedProc::new("set-intersection", set_intersection),
        UntypedProc::new("set-difference", set_difference),
        UntypedProc::new("set-subset?", set_subset_p),
        UntypedProc::new("heap-new", heap_new),
        UntypedProc::new("heap-push!", heap_push),
        UntypedProc::new("heap-pop!", heap_pop),
        UntypedProc::new("heap-peek", heap_peek),
        UntypedProc::new("heap-length", heap_length),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn expect_no_args(name: &str, args: &[Value]) -> Result<(), FfiError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(arity(name, "0", args.len()))
    }
}

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Number(cs_core::Number::Fixnum(v))) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::string(s)
}

fn slot_mut<F, R>(name: &str, id: i64, f: F) -> Result<R, FfiError>
where
    F: FnOnce(&mut Slot) -> Result<R, FfiError>,
{
    with_registry(|r| {
        let slot = r
            .slots
            .get_mut(&id)
            .ok_or_else(|| FfiError::HostFailure(format!("{}: bad handle {}", name, id)))?;
        f(slot)
    })
}

fn bad_kind(name: &str, id: i64, expected: &str) -> FfiError {
    FfiError::HostFailure(format!("{}: handle {} is not a {}", name, id, expected))
}

// ----- queue -----

fn queue_new(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("queue-new", args)?;
    Ok(Value::fixnum(insert(Slot::Queue(VecDeque::new()))))
}

fn queue_push(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("queue-push!", args, 0)?;
    let val = args
        .get(1)
        .cloned()
        .ok_or(arity("queue-push!", ">= 2", args.len()))?;
    slot_mut("queue-push!", id, |s| match s {
        Slot::Queue(q) => {
            q.push_back(val);
            Ok(Value::Unspecified)
        }
        _ => Err(bad_kind("queue-push!", id, "queue")),
    })
}

fn queue_pop(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("queue-pop!", args, 0)?;
    slot_mut("queue-pop!", id, |s| match s {
        Slot::Queue(q) => Ok(q.pop_front().unwrap_or(Value::Boolean(false))),
        _ => Err(bad_kind("queue-pop!", id, "queue")),
    })
}

fn queue_peek(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("queue-peek", args, 0)?;
    slot_mut("queue-peek", id, |s| match s {
        Slot::Queue(q) => Ok(q.front().cloned().unwrap_or(Value::Boolean(false))),
        _ => Err(bad_kind("queue-peek", id, "queue")),
    })
}

fn queue_length(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("queue-length", args, 0)?;
    slot_mut("queue-length", id, |s| match s {
        Slot::Queue(q) => Ok(Value::fixnum(q.len() as i64)),
        _ => Err(bad_kind("queue-length", id, "queue")),
    })
}

fn queue_empty_p(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("queue-empty?", args, 0)?;
    slot_mut("queue-empty?", id, |s| match s {
        Slot::Queue(q) => Ok(Value::Boolean(q.is_empty())),
        _ => Err(bad_kind("queue-empty?", id, "queue")),
    })
}

// ----- set -----

fn set_new(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("set-new", args)?;
    Ok(Value::fixnum(insert(Slot::Set(HashSet::new()))))
}

fn set_add(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("set-add!", args, 0)?;
    let elem = expect_string("set-add!", args, 1)?;
    slot_mut("set-add!", id, |s| match s {
        Slot::Set(set) => {
            set.insert(elem);
            Ok(Value::Unspecified)
        }
        _ => Err(bad_kind("set-add!", id, "set")),
    })
}

fn set_remove(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("set-remove!", args, 0)?;
    let elem = expect_string("set-remove!", args, 1)?;
    slot_mut("set-remove!", id, |s| match s {
        Slot::Set(set) => Ok(Value::Boolean(set.remove(&elem))),
        _ => Err(bad_kind("set-remove!", id, "set")),
    })
}

fn set_contains_p(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("set-contains?", args, 0)?;
    let elem = expect_string("set-contains?", args, 1)?;
    slot_mut("set-contains?", id, |s| match s {
        Slot::Set(set) => Ok(Value::Boolean(set.contains(&elem))),
        _ => Err(bad_kind("set-contains?", id, "set")),
    })
}

fn set_size(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("set-size", args, 0)?;
    slot_mut("set-size", id, |s| match s {
        Slot::Set(set) => Ok(Value::fixnum(set.len() as i64)),
        _ => Err(bad_kind("set-size", id, "set")),
    })
}

fn set_to_list(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("set->list", args, 0)?;
    slot_mut("set->list", id, |s| match s {
        Slot::Set(set) => Ok(Value::list(
            set.iter()
                .map(|s| string_value(s.clone()))
                .collect::<Vec<_>>(),
        )),
        _ => Err(bad_kind("set->list", id, "set")),
    })
}

/// Clone the `HashSet` behind a set handle (read-only).
fn read_set(r: &Registry, name: &str, id: i64) -> Result<HashSet<String>, FfiError> {
    match r.slots.get(&id) {
        Some(Slot::Set(s)) => Ok(s.clone()),
        Some(_) => Err(bad_kind(name, id, "set")),
        None => Err(FfiError::HostFailure(format!(
            "{}: bad handle {}",
            name, id
        ))),
    }
}

/// Build a fresh set slot from `elems`, returning its handle. Done inside
/// an existing `with_registry` borrow (can't call `insert`, which re-borrows).
fn insert_set(r: &mut Registry, elems: HashSet<String>) -> Value {
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, Slot::Set(elems));
    Value::fixnum(id)
}

/// Shared body for the binary set-combining ops.
fn set_binop(
    name: &str,
    args: &[Value],
    combine: impl FnOnce(&HashSet<String>, &HashSet<String>) -> HashSet<String>,
) -> Result<Value, FfiError> {
    let a = expect_fixnum(name, args, 0)?;
    let b = expect_fixnum(name, args, 1)?;
    with_registry(|r| {
        let sa = read_set(r, name, a)?;
        let sb = read_set(r, name, b)?;
        Ok(insert_set(r, combine(&sa, &sb)))
    })
}

fn set_union(args: &[Value]) -> Result<Value, FfiError> {
    set_binop("set-union", args, |a, b| a.union(b).cloned().collect())
}

fn set_intersection(args: &[Value]) -> Result<Value, FfiError> {
    set_binop("set-intersection", args, |a, b| {
        a.intersection(b).cloned().collect()
    })
}

fn set_difference(args: &[Value]) -> Result<Value, FfiError> {
    set_binop("set-difference", args, |a, b| {
        a.difference(b).cloned().collect()
    })
}

fn set_subset_p(args: &[Value]) -> Result<Value, FfiError> {
    let a = expect_fixnum("set-subset?", args, 0)?;
    let b = expect_fixnum("set-subset?", args, 1)?;
    with_registry(|r| {
        let sa = read_set(r, "set-subset?", a)?;
        let sb = read_set(r, "set-subset?", b)?;
        Ok(Value::Boolean(sa.is_subset(&sb)))
    })
}

// ----- heap (max-heap of fixnums) -----

fn heap_new(args: &[Value]) -> Result<Value, FfiError> {
    expect_no_args("heap-new", args)?;
    Ok(Value::fixnum(insert(Slot::Heap(BinaryHeap::new()))))
}

fn heap_push(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("heap-push!", args, 0)?;
    let val = expect_fixnum("heap-push!", args, 1)?;
    slot_mut("heap-push!", id, |s| match s {
        Slot::Heap(h) => {
            h.push(val);
            Ok(Value::Unspecified)
        }
        _ => Err(bad_kind("heap-push!", id, "heap")),
    })
}

fn heap_pop(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("heap-pop!", args, 0)?;
    slot_mut("heap-pop!", id, |s| match s {
        Slot::Heap(h) => Ok(h.pop().map_or(Value::Boolean(false), Value::fixnum)),
        _ => Err(bad_kind("heap-pop!", id, "heap")),
    })
}

fn heap_peek(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("heap-peek", args, 0)?;
    slot_mut("heap-peek", id, |s| match s {
        Slot::Heap(h) => Ok(h
            .peek()
            .map_or(Value::Boolean(false), |v| Value::fixnum(*v))),
        _ => Err(bad_kind("heap-peek", id, "heap")),
    })
}

fn heap_length(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("heap-length", args, 0)?;
    slot_mut("heap-length", id, |s| match s {
        Slot::Heap(h) => Ok(Value::fixnum(h.len() as i64)),
        _ => Err(bad_kind("heap-length", id, "heap")),
    })
}
