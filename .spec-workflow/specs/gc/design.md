# M5 GC — Design

> Status: **CLOSED** at `m5-complete` tag. Exit report:
> `docs/milestones/m5-exit.md`.
> Phase 1 (mark-sweep + Rc-backed slots) shipped per this design.
> Phase 2 (arena swap, generational copying) is tracked as a
> post-M5 perf-track follow-up.
> Companion: `requirements.md`.

## Overview

Replace `Rc<RefCell<...>>` heap pointers with a hand-rolled precise
tracing GC. Phase 1 is mark-and-sweep stop-the-world; Phase 2 (post
M5 exit) upgrades to generational copying.

## Components

### `cs-gc` crate

New workspace crate. Public API:

```rust
pub struct Heap { /* roots, freelists, header table */ }

pub struct Gc<T> { /* opaque; mirrors Rc<T> ergonomics */ }

impl<T> Heap {
    pub fn alloc<T: Trace>(&mut self, v: T) -> Gc<T>;
    pub fn collect(&mut self);
}

pub trait Trace {
    fn trace(&self, marker: &mut Marker);
}
```

`Gc<T>` derefs to `&T` and is `Clone` (cloning is a fast no-op pointer
copy; reachability is determined at GC time, not refcount-time).

### Per-Runtime `Heap`

The `Runtime` owns a `Heap`. The walker, VM, and any future JIT all
allocate through `Runtime::heap_mut()`. A future multi-runtime story
keeps heaps disjoint (no cross-heap pointers).

### Rooting strategy

**Precise rooting via a roots vector**, not conservative stack scan.

Roots:
1. The runtime top-level `Frame` chain — `Heap::add_root(frame)` at
   construction.
2. The VM's value stack — `vm.stack` is reachable via a borrow during
   `collect()` (collector pauses execution).
3. The VM frame stack — same.
4. `pending_values` channel — held in a `RefCell<Option<Vec<Value>>>`;
   trace if `Some`.
5. `COND_PARENTS` and `BUILTIN_ERR_IRRITANT` thread-locals — register
   their cell pointers as roots when the Runtime is created.
6. `call/cc`-captured continuations alive on the heap — themselves
   root candidates because their frames must persist.

### Trace impls

Each heap-allocated `Value` variant implements `Trace`:
- `Pair { car, cdr }` traces both cells.
- `Vector(RefCell<Vec<Value>>)` traces every element.
- `String(RefCell<String>)` no-op (no heap pointers inside).
- `ByteVector(RefCell<Vec<u8>>)` no-op.
- `Hashtable.items` traces every (k, v) pair, plus the `custom`
  hash/equiv slots (themselves `Value`s).
- `Port` variants trace their state's referenced strings/bytevectors.
- `Procedure` variants trace closure environments.
- `Record { tag: Symbol, fields: Vec<Value> }` traces the field vec.

`Symbol` itself is just a `u32` index into the symbol table; the
table holds `Rc<str>` entries that are immortal once interned. No
trace needed.

### Algorithm — Phase 1 (mark-sweep)

1. **Mark**: starting from roots, walk reachable graph, set the mark
   bit on each visited header.
2. **Sweep**: walk every allocated header; reset mark bit if set,
   else free.

Triggered when allocation count since last GC exceeds a configurable
threshold (default: 4096 allocations).

### Algorithm — Phase 2 (generational copying, post-M5-exit)

- Two semispaces, bump allocator.
- Young gen survives → tenured.
- Write barrier on `set-car!`/`set-cdr!`/`vector-set!` etc. to
  maintain the remembered set.

Phase 2 is its own follow-up — the M5 exit gate is achievable with
mark-sweep alone.

## Migration plan

This is a bottom-up swap and the diff is large. Plan:

1. **Step A**: write `cs-gc` crate with `Heap`, `Gc<T>`, `Trace`,
   stop-the-world `collect()`. Drop-in compatible with `Rc<T>`
   ergonomically. Tests in isolation.

2. **Step B**: introduce a `cfg`-gated alias in `cs-core`:
   `pub type Heap = Rc<...>` becomes `pub type Heap = Gc<...>` behind
   `feature = "gc"`. Land the feature flag, no behavior change yet.

3. **Step C**: update each heap-bearing variant in `Value` to use
   `Gc<T>` directly under the flag. Update accessors. Run conformance
   under both flags; verify parity.

4. **Step D**: remove the flag — `Gc<T>` is the only path. Delete
   `Rc` imports from `value.rs`.

5. **Step E**: rooting audit. For every `unsafe` use within the GC,
   document why it's needed. Property-test the rooting set with random
   allocator sequences.

6. **Step F**: fuzz target. 1-hour nightly runs, escalating to 24h
   cumulative before declaring M5 exit.

## Open questions

1. Should `Gc<T>` be `Copy` for cheaper cloning? Probably yes —
   it's just a pointer + a generation tag.
2. Do we need a finalizer story for `FileOutput` ports? Today
   `close-port` flushes; under GC we want the same on collection.
3. How do we handle `Continuation { id: u64 }` — id is just an index
   into a sidecar table, so no trace needed. Confirm this stays true.

These get resolved as we implement Step A.

## File-level diff scope (estimate)

| Crate | LOC change |
|---|---|
| `cs-gc` (new) | ~600 |
| `cs-core/src/value.rs` | ~200 (variant rewrites) |
| `cs-runtime/src/eval.rs` | ~100 (heap-arg threading) |
| `cs-vm/src/vm.rs` | ~200 (heap-arg threading + rooting) |
| `cs-cli` | ~30 (Runtime construction) |
| Tests | ~150 (fuzz target + benches) |

---

## Tasks

A `tasks.md` will follow once Step A is in flight. The shape will mirror
the foundation spec's per-task format with file paths, leverage tags,
prompt scaffolds, and exit criteria per item.
