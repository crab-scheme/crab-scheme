# Region Memory — Requirements

> Status: **CLOSED** (2026-05-17). All 6 iters shipped; see
> `docs/milestones/region-memory-exit.md` and
> `docs/adr/0016-region-types.md`.
> Spec slug: `region-memory`
> Roadmap slot: Layer 3 of the unified memory management
> architecture (ADR 0015).
> Predecessor: countable-memory spec
> (`.spec-workflow/specs/countable-memory/`), ADR 0014.
> Companion (later): `escape-analysis` spec (Layer 5).

This spec adds **region-based (arena) allocation** to CrabScheme
as a third reclamation mechanism alongside Rust ownership
(layer 1) and `Rc<T>`-backed `Gc<T>` (layer 2, countable-memory).
Regions are bounded-extent bump allocators: every allocation
made inside a region lives until the region's owner drops, at
which point all allocations free in one operation regardless of
internal refcounts.

## Why regions

Reference counting (layer 2) is correct and fast on the common
case but has three costs that regions sidestep:

1. **Per-allocation refcount overhead.** Every `cons` /
   `make-vector` increments and eventually decrements an
   atomic-shaped counter. For tight allocation loops
   (`(map f xs)` over a 10k list), this dominates.
2. **Refcount bookkeeping at every clone.** Each Value clone
   pays one or more counter bumps.
3. **Cycle handling.** Layer 2's mutation-site cycle detector
   plus the `Pair::break_*_cycle` machinery exists because RC
   alone leaks cycles. Regions are **cycle-free by
   construction**: a region is freed in one shot, so cycles
   within it are released atomically.

Region-allocated values give up flexibility (you can't extend a
value's lifetime beyond its region) in exchange for predictable,
bulk-free reclamation. The trade-off pays off when escape
analysis (layer 5) can prove the lifetime bound — which is the
common case for intermediate values in functional pipelines.

The worktree name `region-memory` signals intent: memory whose
liveness is *region-bounded*, not refcount-counted.

---

## Functional requirements

### FR-1. `cs_gc::Region` arena type

Introduce a new public type `cs_gc::Region` that owns a bump
allocator. Allocations made through the region's API live as
long as the region itself; when the region's owner drops, all
allocations free in one operation.

Public API (under `feature = "regions"`):
```rust
pub struct Region {
    /// Opaque per-region identity (used to validate Gc<T> belongs
    /// to this region in debug builds).
    id: RegionId,
    /// Bump arena.
    arena: Bump,
}

impl Region {
    pub fn new() -> Self;
    pub fn id(&self) -> RegionId;
    pub fn alloc<T: 'static>(&self, value: T) -> Gc<T>;
    pub fn allocated_bytes(&self) -> usize;
}

impl Drop for Region {
    fn drop(&mut self) {
        // Bulk-free all allocations made through this region.
    }
}
```

**Acceptance**: `crates/cs-gc/src/region.rs` exposes
`Region::new`, `Region::alloc`, `Region::id`,
`Region::allocated_bytes`. A unit test allocates 10⁶ small
`Gc<i64>` values through one region, asserts peak RSS within
bound, and verifies the region drops all allocations in one
operation.

### FR-2. Region-aware `Gc<T>` constructor

Extend `cs_gc::Gc<T>` with a region-aware allocation path:
```rust
impl<T: 'static> Gc<T> {
    /// Allocate `value` in `region`. The returned `Gc<T>`
    /// participates in the region's bulk-free; cloning bumps a
    /// region-local refcount, dropping decrements; both are
    /// no-ops once the region drops.
    pub fn new_in(region: &Region, value: T) -> Self;
}
```

`Gc<T>` becomes a discriminated union under the hood:
```rust
enum GcRepr<T> {
    /// Global Rc<T> (layer 2, countable-memory).
    Rc(Rc<T>),
    /// Region-local: pointer into a region arena + region id.
    Region {
        ptr: NonNull<T>,
        region_id: RegionId,
    },
}
```

Operations on `Gc<T>` (clone, deref, ptr_eq, as_addr,
into_raw_jit, from_raw_jit, raw_incref, downgrade,
strong_count) handle both variants transparently.

**Acceptance**: `Gc::new_in(region, v)` returns a `Gc<T>` whose
deref / clone / Debug behaviour is observationally identical to
`Gc::new(v)` for the duration of the region's life.
`Gc::strong_count` returns the region-local count for region-
allocated values; `Gc::as_addr` returns a stable identity that
survives Rc → Region distinction.

### FR-3. Region-local refcounting

Region allocations carry an in-line refcount header
(per-allocation) for two reasons:

1. The detector machinery from countable-memory expects every
   `Gc<T>` to have a meaningful `strong_count`; region
   allocations must report one too.
2. Some downstream code (JIT raw-handle ABI per ADR 0012 D-2)
   borrows handles via `raw_incref` / `from_raw_jit` —
   region-local refcount lets that work correctly across the
   borrow.

Layout per region allocation:
```text
| u32 strong | u32 padding |  T payload  |
              ↑ aligned to alignof(T)
```

The strong count exists for ABI compatibility but does NOT
trigger reclamation — the region's bulk-free runs when the
region drops, irrespective of the local count.

**Acceptance**: `Gc::strong_count` on a region-allocated value
returns 1 after `Gc::new_in`, increments on clone, decrements
on drop, and is unobservable from program semantics (the value
stays alive even when count hits 0, until the region drops).

### FR-4. Copy-on-promote for escaping values

When a region-allocated `Gc<T>` is about to outlive its region,
the runtime promotes it to a global `Rc<T>` by deep-copying the
value into a fresh Rc allocation. The original region pointer
becomes a no-op after promotion (subsequent reads via the
in-flight `Gc<T>` go through the promoted Rc).

Escape detection is the responsibility of layer 5 (escape
analysis); this layer provides the *mechanism*:
```rust
impl<T: 'static + Clone> Gc<T> {
    /// Promote a region-allocated `Gc<T>` to a global Rc-backed
    /// allocation by deep-cloning the value. No-op for already-
    /// Rc-backed `Gc<T>`. Used by the runtime when a value's
    /// lifetime extends beyond its allocating region (e.g., it
    /// gets stored in a longer-lived data structure).
    pub fn promote(this: &mut Self);
}
```

For values with internal references (`Pair`, `Vector`,
`Hashtable`, etc.) promotion is recursive — internal references
to other region-allocated values are themselves promoted.

**Acceptance**: a test allocates a `Gc<Pair>` in a region, calls
`Gc::promote`, then drops the region; subsequent reads of the
promoted pair (and its inner values) return correct data.

### FR-5. Region-aware Drop discipline

Dropping a `Region` releases all its allocations regardless of
their refcount state. Outstanding `Gc<T>` handles that pointed
into the dropped region become **invalid** — any subsequent
access is a use-after-free.

To make this safe:

- Debug builds validate region membership on every Gc operation
  (deref, clone, etc.) by checking the embedded `RegionId`. If
  the region has dropped (its id is no longer alive), the
  access panics with a clear diagnostic.
- Release builds skip the check; correctness relies on layer 5
  proving no escape, or on explicit `Gc::promote` calls.

**Acceptance**: a debug-build test that drops a region while
holding a `Gc<T>` handle into it, then accesses the handle,
panics with a "region dropped" diagnostic. A release-build
benchmark confirms no per-access overhead.

### FR-6. Conformance parity with countable-memory

All 117 cs-cli conformance tests passing under countable-memory
must still pass when this spec lands — even though no Scheme
code yet uses regions explicitly. Region support is purely
additive; the default allocation path stays `Gc::new`
(Rc-backed).

**Acceptance**: `cargo test --workspace --release` produces
0 failures. The countable-memory exit report's 117/117 holds.

### FR-7. JIT raw-handle ABI preservation

The JIT (M6) and AOT (M10) spill live `Gc<Value>` handles to
the host stack as raw `i64` words via `Gc::into_raw_jit` /
`Gc::from_raw_jit` / `Gc::raw_incref` (ADR 0012 D-2). The new
`Gc<T>` representation must preserve byte-compatibility of
this ABI for Rc-backed values, and define a clear semantics
for region-backed values: a JIT-spilled region handle keeps
the region's allocation alive (via the region-local refcount)
until the JIT-emitted code releases it AND the region drops.

**Acceptance**: existing M6/M10 differential parity tests
(`crates/cs-vm/tests/jit_*`, `crates/cs-aot/tests/*`) stay
green. A new test verifies that JIT-emitted code interacting
with region-allocated Gc handles operates correctly across
region drop boundaries.

### FR-8. Region-local cycle handling

Cycles **entirely contained within one region** require no
special handling — region drop releases them. The countable-
memory cycle detector should skip cycle-check on
region-allocated mutation sites (the cycle is benign and will
reclaim via region drop).

**Acceptance**: a test that creates a region, builds a cyclic
pair structure with `set-cdr!` inside the region, drops the
region — the cycle counter records the detection but the break
is skipped (region handles reclamation). After region drop,
all cycle members are freed (verified via Drop sentinel).

---

## Non-functional requirements

### NFR-1. Allocation latency

Region allocation should be **at least 5× faster** than
`Gc::new` for small payloads (a single i64 / small Pair).
Measured via a microbenchmark allocating 10⁶ values; region
path < 5ns per allocation in release mode, Gc::new path < 25ns.

### NFR-2. Bulk-free latency

Region drop reclaiming N allocations should run in O(N) but
with much lower per-element constant than `Rc::drop`. Target:
region drop reclaims 10⁶ allocations in < 50ms (vs. ~500ms for
the equivalent Rc-drop chain).

### NFR-3. Memory overhead per allocation

Region allocations should be **at most 16 bytes overhead** per
small payload (in-line refcount + alignment). Compare to
Rc-backed which is 24 bytes (Rc header).

### NFR-4. Public API stability of `cs_core::Value`

`Value`'s variant set and inner pointer types stay unchanged.
`Value::Pair(Gc<Pair>)` etc. continue to compile. The fact
that the `Gc<T>` has two internal representations is invisible
at the variant level.

### NFR-5. Safe API surface

No `unsafe` exposed to non-cs-gc / non-cs-runtime crates.
Region internals (the bump arena, the inline header layout)
use `unsafe` internally but expose a fully-safe API. Continues
the iter-7.1.x precedent of contained `unsafe`.

### NFR-6. WASM target stays green

WASM (M10 Track W) builds with region support disabled by
default (no behaviour change). `cargo build --target
wasm32-unknown-unknown -p cs-runtime --no-default-features
--features ffi-trait` continues to succeed. Enabling regions
on WASM requires no special porting (bump arena is just heap
bytes).

### NFR-7. ADR

A new ADR (`docs/adr/0016-region-types.md`) ratifies:
- The single-region single-thread design for v1
- The copy-on-promote mechanism
- The debug-mode region validation
- The Rc + region duality in `Gc<T>`

---

## Out of scope (deferred)

| Item | Why excluded |
|---|---|
| Multi-region per Gc (region polymorphism) | Cyclone-style region polymorphism is a major type-system extension; v1 ships one region per Gc. |
| Multi-threaded regions | CrabScheme is single-runtime/single-thread today; cross-thread regions defer. |
| Region polymorphism in cs-typer | Layer 5 (escape-analysis) covers basic effect inference; full region kinds are out of scope here. |
| Automatic region inference (no explicit `Gc::new_in`) | Belongs to layer 5 (escape-analysis); this spec provides the runtime mechanism only. |
| Region nesting (sub-regions inside regions) | Nested regions complicate the bump arena; v1 ships flat regions only. |

---

## Risks

1. **Region-escape correctness without escape analysis.** Until
   layer 5 ships, region usage is manual (`Gc::new_in` at
   developer-chosen sites). A misjudged region scope causes
   debug-build panics (good) or release-build UB (bad).
   *Mitigation*: ship debug-mode region validation
   (FR-5); document that release-mode region usage requires
   layer 5 OR manual proofs.

2. **`Gc<T>` discriminated union slows the hot path.** Every
   clone / deref now does a one-bit branch.
   *Mitigation*: profile after FR-2; expect <1% overhead per
   the same shape as `Cow<T>`-style enums in stdlib.

3. **Copy-on-promote for deeply-nested data is expensive.**
   Promoting a `Gc<Pair>` whose cdr is itself
   region-allocated requires recursive promotion.
   *Mitigation*: layer 5 should rarely trigger promotion (only
   for values escaping the region); benchmark the worst case
   (deep list) to confirm bounded cost.

4. **Cycle detection still fires on region-allocated values.**
   Layer 2's cycle counter machinery doesn't know about regions
   yet; it'd report false positives.
   *Mitigation*: FR-8 — detector skips region-allocated
   mutation sites.

5. **JIT integration risk.** The raw-handle ABI carries an
   opaque pointer; for region-allocated values, the pointer
   addresses an arena-local slot whose interpretation changes
   after region drop. Without careful spill-slot semantics,
   the JIT could read stale memory.
   *Mitigation*: FR-7 plus a JIT-specific differential test
   that exercises region-allocated values across method
   boundaries.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `cs_gc::Region` + `Region::alloc` + `Region::id` shipped | `crates/cs-gc/src/region.rs` |
| `Gc::new_in(region, v)` constructor | `crates/cs-gc/src/rc_only.rs` (or new file) |
| Region-local refcount header layout | per FR-3 |
| `Gc::promote` for escape-to-rc | FR-4 |
| Debug-mode region-validity check | FR-5 |
| 117 cs-cli conformance + workspace 0 failures | FR-6 |
| JIT differential tests green | FR-7 |
| Cycle detector skips region mutations | FR-8 |
| Allocation latency ≥ 5× faster than `Gc::new` | NFR-1 |
| Bulk-free 10⁶ allocations in < 50ms | NFR-2 |
| Per-allocation overhead ≤ 16 bytes | NFR-3 |
| `cs_core::Value` API unchanged | NFR-4 |
| `unsafe` contained inside cs-gc | NFR-5 |
| WASM build green | NFR-6 |
| ADR 0016 written | NFR-7 |
