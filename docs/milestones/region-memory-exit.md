# Region Memory — Exit Report

> Tagged at the merge commit of this report.
> Predecessor: countable-memory (`docs/milestones/countable-memory-exit.md`).
> Spec: `.spec-workflow/specs/region-memory/` (CLOSED).
> ADR: `docs/adr/0016-region-types.md` (builds on
> `docs/adr/0014-countable-memory.md` and `docs/adr/0015-unified-memory-management.md`).

This report closes all 6 iters of the region-memory spec. The
`cs_gc::Region` bump-arena primitive ships default-on
workspace-wide; layer 5 (escape analysis, separate spec) is
the next consumer that will exercise it from compiled code.

---

## Acceptance summary

| Gate | Spec § | Result |
|---|---|---|
| FR-1: `Region` bump-arena type | requirements.md | **✅** `cs_gc::Region` over `bumpalo::Bump`, unique `RegionId`, `!Send`/`!Sync`. |
| FR-2: `Gc::new_in(region, value)` constructor | requirements.md | **✅** `crates/cs-gc/src/rc_only.rs` impls `new_in` delegating to `Region::alloc`. `Pair::new_in` for the cs-core convenience wrapper. |
| FR-3: in-line refcount header (non-reclaiming) | requirements.md | **✅** `RegionSlot<T> { strong: Cell<u32>, _pad: u32, value: T }`. Bump-free reclaims regardless of count. Asserted by `region_strong_count_does_not_drive_reclamation`. |
| FR-4: `Gc::promote` + `Promote::promote_deep` | requirements.md | **✅** `cs_gc::Gc::promote` + `cs_core::Promote::promote_deep`. Pair / Vector / String / ByteVector / Hashtable / Promise / Port covered. |
| FR-5: debug-mode use-after-region-drop check | requirements.md | **✅** `LIVE_REGION_IDS` thread-local + `assert_region_live`. Panics with `"use-after-region-drop"` diagnostic in debug; no-op in release. |
| FR-6: zero release-mode overhead for unused regions | requirements.md | **✅** `#[cfg(feature = "regions")]`-gated everywhere; `#[cfg(debug_assertions)]` for the validity check. Cs-gc with `--no-default-features --features countable-memory` builds identically to the pre-regions binary. |
| FR-7: discriminated `Gc<T>` representation | requirements.md | **✅** `GcRepr<T>` two-arm enum (`Rc`, `Region { ptr, region_id }`). Inter-variant ptr_eq is false; intra-variant compares pointers. |
| FR-8: cycle detector skips region mutations | requirements.md | **✅** `b_set_car` / `b_set_cdr` / `b_vector_set` / `b_hashtable_set` guard `check_and_break` behind `!Gc::is_region(container)`. Verified by `crates/cs-runtime/tests/region_cycle.rs`. |
| NFR-1: per-allocation cost < 5 ns | requirements.md | **✅** 3.75 ns/alloc measured (10⁶ allocations, release). |
| NFR-2: bulk-free cost < 50 ms for 10⁶ allocations | requirements.md | **✅** 11.7 µs measured (≈ 4300× under target). |
| NFR-3: M5b conformance preserved | requirements.md | **✅** 117 cs-cli conformance files green workspace-wide. |
| NFR-4: WASM target stays green | requirements.md | **✅** `bumpalo` is `no_std`-compatible; the regions feature is purely additive. |
| NFR-5: bumpalo dep MIT/Apache | requirements.md | **✅** 3.x, dual-licensed. |
| NFR-6: AOT pipeline unaffected | requirements.md | **✅** cs-aot tests pass; emitted Rust source unchanged (regions never appear in AOT IR yet). |
| NFR-7: ADR 0016 ratifies | requirements.md | **✅** `docs/adr/0016-region-types.md` (iter 6). |

---

## What shipped per iter

### Iter 1 — bumpalo dep + Region/RegionId/RegionSlot

`bumpalo = "3"` added to `[workspace.dependencies]` (MIT/Apache,
mature). `crates/cs-gc/src/region.rs` (new, gated on `regions`
feature) defines:

- `pub struct RegionId(NonZeroU64)` — minted from a global atomic
  counter; `Copy + Eq + Hash + Debug`.
- `#[repr(C)] pub(crate) struct RegionSlot<T> { strong, _pad, value }`
  — the per-allocation header (8 bytes overhead before the
  payload).
- `pub struct Region { id, arena: Bump, _not_send }` — single-
  threaded bump arena; `Region::new`, `Region::id`,
  `Region::allocated_bytes`, `Region::alloc<T: 'static>` (the
  last returns `NonNull<RegionSlot<T>>` for layer-2-internal use).

Unit tests in the module cover unique ids, allocation address
distinctness, payload readability, monotone allocation
accounting, and bulk-free smoke.

### Iter 2 — `Gc<T>` GcRepr discriminated union

`crates/cs-gc/src/rc_only.rs` refactored from
`pub struct Gc<T>(Rc<T>)` to:

```rust
enum GcRepr<T: ?Sized> {
    Rc(Rc<T>),
    #[cfg(feature = "regions")]
    Region { ptr: NonNull<RegionSlot<T>>, region_id: RegionId },
}
pub struct Gc<T: ?Sized>(GcRepr<T>);
```

Every public method (`Clone`, `Deref`, `PartialEq`, `Debug`,
`ptr_eq`, `as_addr`, `strong_count`, `downgrade`,
`into_raw_jit` / `from_raw_jit` / `raw_incref`, `Drop`)
dispatches on `GcRepr`. The Region arm uses
`std::ptr::addr_eq` for pointer comparisons (avoids the
fat-pointer comparison warning for `?Sized` T) and a
`ManuallyDrop + ptr::read` pattern in `into_raw_jit` to move
the inner repr out without running Drop. `downgrade` is bound
to `T: Sized` (RawWeak::new requires it); the Region arm
panics in debug and returns a never-upgrading `Weak` in
release (no use case yet for Weak references into a region).

Added `is_region(&Self) -> bool` discriminator (iter 9 in
the original ordering, landed here as part of the union).

### Iter 3 — `Gc::new_in` + debug-mode validity

`region.rs` added a thread-local `LIVE_REGION_IDS:
RefCell<HashSet<RegionId>>` populated by `Region::new` /
`Region::Drop`. Helpers `assert_region_live(region_id)`
(panics under `#[cfg(debug_assertions)]`) and
`is_region_live(region_id)` (used in release by `Gc::Drop`
to skip a stale slot decrement) wire into every Region-arm
operation in `rc_only.rs`: `Clone`, `Deref`, `strong_count`
all assert; `Drop` checks and short-circuits.

`Gc::new_in(region: &Region, value: T)` is the public
constructor that delegates to `Region::alloc` and wraps the
result in `GcRepr::Region`.

8 integration tests in `crates/cs-gc/tests/region.rs` cover
basic alloc/lifetime, refcount semantics (clone bumps;
drop-to-zero does NOT reclaim), bulk-free behaviour,
cross-region distinguishability, the bumpalo no-Drop
contract, the debug-mode panic on use-after-region-drop,
plus 2 release-only perf microbenches.

### Iter 4 — `Gc::promote` + Promote trait

`cs_gc::Gc::promote(this: &mut Self)` (T: Clone) — if the
current variant is Region, deep-clone the payload and
replace `self.0` with a fresh `GcRepr::Rc(Rc::new(cloned))`.
Rc arm is a no-op.

`cs_core::Promote::promote_deep` (new module
`crates/cs-core/src/promote.rs`) walks `Value`'s heap-
bearing variants recursively, promoting every Region-backed
`Gc<T>` it finds via the type's regular Rc constructor
(`Pair::new`, `Hashtable::new`, etc.). Required adding
`Clone` derives to the leaf Port-state structs
(`FileOutputState`, `StringInputState`,
`ByteVectorInputState`) so the Port path can clone its
contents.

Forwarded `regions` feature through cs-core and cs-runtime
manifests so consumers can opt in transparently.

7 integration tests in `crates/cs-runtime/tests/region_promote.rs`
cover single-level Pair promotion (survives region drop),
two-level deep promotion (inner Pair too), Vector with
nested region Pair, String/ByteVector leaf payload clones,
Hashtable items recursion, mixed Region+Rc handling, and
leaf-value passthrough.

### Iter 5 — Cycle-detector region skip

`b_set_car`, `b_set_cdr`, `b_vector_set`, and
`b_hashtable_set` (all 3 paths within the last) now guard
their `cs_gc::cycle::check_and_break(container, …)` call
behind `#[cfg(feature = "regions")] if Gc::is_region(c) {
return Ok(Value::Unspecified); }`. Region cycles reclaim
via the region's bulk free regardless of internal back-
edges; running the detector wastes CPU and could falsely
refuse to break a benign cycle.

4 integration tests in `crates/cs-runtime/tests/region_cycle.rs`
verify the skip:
`region_pair_self_cycle_skips_detector` —
`cycle_detection_count` stays at baseline after a self-
cycling region Pair. `region_vector_self_ref_skips_detector`
+ `region_hashtable_self_ref_skips_detector` — same for
vectors and hashtables. The contrasting
`rc_pair_self_cycle_fires_detector` confirms detection
still works for the Rc path.

### Iter 6 — Flip default-on + ADR 0016 + this report

Added `regions` to the default feature set of every
workspace crate that exposes the feature (cs-gc, cs-core,
cs-vm, cs-runtime) plus the workspace's pinned cs-runtime
feature list. The default Scheme allocation path remains
Rc-backed — regions activate only via explicit
`Gc::new_in` (or future layer-5 dispatch). Opt out with
`--no-default-features --features countable-memory`.

ADR 0016 (`docs/adr/0016-region-types.md`) ratifies the
single-region-per-Gc design, copy-on-promote semantics,
debug-mode validity model, and Rc/Region duality in
`GcRepr<T>`.

This report closes the spec.

---

## Performance numbers

Per the iter-3 microbenches, measured on the development
machine in release mode (10⁶ allocations of `i64`):

| Metric | Target | Actual | Headroom |
|---|---|---|---|
| Per-allocation latency | < 5 ns | **3.75 ns** | 25 % under |
| Bulk-free for 10⁶ allocations | < 50 ms | **11.7 µs** | ≈ 4300× under |

The bulk-free measurement is the dominant win: ten million
`Rc::drop` chains take well into the millisecond range; one
`Bump::drop` is a single buffer free.

The cycle-detector skip is a CPU win whose magnitude depends
on workload — it eliminates `O(reachable subgraph)` work
per mutation when the container is region-allocated. No
Scheme benchmark today actually exercises this (regions
aren't dispatched to from Scheme yet), so we don't have a
percentage.

---

## What this enables (and what stays deferred)

**Enables**:
- `escape-analysis` spec (layer 5). cs-typer's effect
  inferencer can emit `Gc::new_in(region, …)` at every
  allocation site it proves bounded; no further runtime
  primitive needed.
- The `tracing-revival` spec (layer 4) if a concrete cycle
  workload demands tracing — though regions reduce the
  motivation since they handle bounded-lifetime values
  natively.
- Manual region use by hand-tuned hot loops (subject to the
  same release-mode-UB caveat that layer 5 will eliminate).

**Stays deferred**:
- Layer-5 escape analysis itself (separate spec, not yet
  started). Until it lands, the production allocation path
  is still 100 % Rc-backed; the region machinery sits
  unused except by manual `Gc::new_in` callers.
- Region polymorphism (a `Gc<T>` that migrates between
  regions). Out of scope for v1; `Gc::promote` handles the
  only direction (region → Rc) that mattered.
- Multi-threaded regions. `Region: !Send` by construction
  via a `PhantomData<*const ()>` marker; revisit if
  cs-runtime grows real thread support.
- Per-allocation Drop in regions. `bumpalo::Bump` does not
  run Drop on its allocations — values with non-trivial
  cleanup paths must use Rc or arrange cleanup separately.

---

## Known regressions / open issues

None landed in this spec. The pre-existing
`jit_differential::diff_jit_fixnum_constants` SIGTRAP
(cfg-gated since countable-memory iter 7.1) and the
pre-existing `jit_conformance` stack overflow on the
worktree machine are unrelated to region memory.

---

## File map

New files:
- `crates/cs-gc/src/region.rs` (361 LOC) — Region, RegionId, RegionSlot, LIVE_REGION_IDS.
- `crates/cs-gc/tests/region.rs` (216 LOC) — 8 unit + 2 perf integration tests.
- `crates/cs-core/src/promote.rs` (199 LOC) — Promote trait + Value impl.
- `crates/cs-runtime/tests/region_promote.rs` (212 LOC) — 7 deep-promote tests.
- `crates/cs-runtime/tests/region_cycle.rs` (162 LOC) — 4 cycle-skip tests.
- `docs/adr/0016-region-types.md` — this iter's ADR.
- `docs/milestones/region-memory-exit.md` — this report.

Modified files:
- `Cargo.toml` — bumpalo dep, cs-runtime workspace pin.
- `crates/cs-gc/Cargo.toml` — `regions` feature default-on.
- `crates/cs-gc/src/lib.rs` — re-export Region/RegionId.
- `crates/cs-gc/src/rc_only.rs` — GcRepr union, new_in, promote, is_region, raw-handle Region arm, Drop, debug-mode validity wiring.
- `crates/cs-core/Cargo.toml` — `regions` feature forwarding, default-on.
- `crates/cs-core/src/lib.rs` — pub use promote::Promote.
- `crates/cs-core/src/value.rs` — Pair::new_in, Clone derives on Port-state.
- `crates/cs-vm/Cargo.toml` — `regions` feature forwarding, default-on.
- `crates/cs-runtime/Cargo.toml` — `regions` feature forwarding, default-on.
- `crates/cs-runtime/src/builtins/mod.rs` — `Gc::is_region` guard on 4 mutation builtins (5 callsites).
- `.spec-workflow/specs/region-memory/{requirements,design,tasks}.md` — marked CLOSED.
