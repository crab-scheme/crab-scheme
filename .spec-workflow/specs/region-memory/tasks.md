# Region Memory — Tasks

> Companion: `requirements.md`, `design.md`.
> Format mirrors the countable-memory / foundation spec tasks:
> per-task file paths, leverage hooks, prompt scaffolds, exit
> criteria.
> Sequenced per design.md §"Migration plan" iters 1–6.

The work is split into **6 iters**. Each lands behind the
`regions` feature flag until iter 6 flips the default. Each
iter is a single commit per the per-iter commit policy.

---

## Iter 1 — `cs_gc::Region` arena type

- [ ] 1. Add `bumpalo` to the workspace `Cargo.toml`
  - File: `Cargo.toml`, `crates/cs-gc/Cargo.toml`
  - Add `bumpalo = "3"` under `[workspace.dependencies]`.
  - Add `bumpalo = { workspace = true, optional = true }` to
    `crates/cs-gc/Cargo.toml`.
  - Add `[features] regions = ["dep:bumpalo"]`.
  - Purpose: provide the underlying bump-arena dependency.
  - _Leverage: bumpalo (mature crate, MIT/Apache, in many
    Rust projects)._
  - _Requirements: FR-1, NFR-5_
  - _Prompt: Role: Rust workspace maintainer | Task: Add
    bumpalo 3.x to workspace.dependencies and to cs-gc as an
    optional dep gated on a new `regions` feature | Restrictions: pin
    the version in workspace; do not add to cs-gc default
    features; do not bump MSRV | Success: `cargo build -p cs-gc --features regions`
    pulls in bumpalo._

- [ ] 2. Implement `cs_gc::Region`, `RegionId`, `RegionSlot<T>`
  - File: `crates/cs-gc/src/region.rs` (new),
    `crates/cs-gc/src/lib.rs`
  - Create `crates/cs-gc/src/region.rs` per design.md
    §"Component 1" and §"Component 3":
    - `pub struct RegionId(NonZeroU64)` with `Debug`, `Clone`,
      `Copy`, `PartialEq`, `Eq`, `Hash`.
    - Global atomic counter to mint fresh ids.
    - `pub struct Region { id: RegionId, arena: Bump,
      _not_send: PhantomData<*const ()> }`.
    - `Region::new`, `Region::id`, `Region::allocated_bytes`,
      `Region::alloc<T: 'static>(&self, value: T) -> Gc<T>`.
    - `#[repr(C)] struct RegionSlot<T> { strong: Cell<u32>, _pad:
      u32, value: T }`.
  - Gate the whole module on `#[cfg(feature = "regions")]`.
  - Wire `mod region;` and `pub use region::{Region, RegionId};`
    in `crates/cs-gc/src/lib.rs` under the feature.
  - Purpose: stand up the arena owner and per-allocation
    header.
  - _Leverage: `bumpalo::Bump` for the underlying arena;
    `std::sync::atomic::AtomicU64` for the id counter._
  - _Requirements: FR-1, FR-3, FR-5_
  - _Prompt: Role: Rust systems engineer with bumpalo
    familiarity | Task: Implement `cs_gc::Region` and
    `cs_gc::RegionId` per design.md §"Component 1" and
    §"Component 3", with `Region::alloc` returning a
    `Gc<T>` whose underlying `GcRepr` is the Region arm
    (introduced in iter 2; for now stub it as a TODO).
    | Restrictions: gate on `#[cfg(feature = "regions")]`; no
    `unsafe` outside the `Region::alloc` body; the global id
    counter must be lock-free (Atomic only) | Success: `cargo
    build -p cs-gc --features regions` builds; 3 standalone
    unit tests in `region.rs` cover Region::new uniqueness,
    Region::alloc returning distinct addresses,
    Region::allocated_bytes growing monotonically._

---

## Iter 2 — `Gc<T>` discriminated union

- [ ] 3. Refactor `Gc<T>` to wrap `GcRepr<T>`
  - File: `crates/cs-gc/src/rc_only.rs`
  - Under `#[cfg(feature = "regions")]`, change `pub struct
    Gc<T: ?Sized>(Rc<T>)` to
    `pub struct Gc<T: ?Sized>(GcRepr<T>)` with the enum:
    ```rust
    enum GcRepr<T: ?Sized> {
        Rc(Rc<T>),
        Region {
            ptr: NonNull<RegionSlot<T>>,
            region_id: RegionId,
        },
    }
    ```
  - Reimplement every existing method (Clone, Deref, PartialEq,
    Debug, ptr_eq, as_addr, strong_count, downgrade, new,
    into_raw_jit, from_raw_jit, raw_incref) to dispatch on
    `GcRepr` per design.md §"Component 5" table.
  - Under `#[cfg(not(feature = "regions"))]` the existing
    Rc-only struct stays.
  - Purpose: enable both backings behind the same API.
  - _Leverage: iter 1's RegionSlot and RegionId._
  - _Requirements: FR-2, FR-3, NFR-4_
  - _Prompt: Role: Rust systems engineer comfortable with
    discriminated unions and unsafe pointer arithmetic |
    Task: Refactor `Gc<T>` to the GcRepr variant per
    design.md §"Component 2" + §"Component 5", preserving
    byte-for-byte semantics on the Rc arm and implementing
    the new Region arm. The strong count for Region values
    is `(*ptr.as_ptr()).strong.get() as usize`. The JIT
    raw-handle ABI for Region values increments strong via
    `slot.strong.set(slot.strong.get() + 1)` then returns
    `ptr.as_ptr() as *const ()` | Restrictions: no behaviour
    change for existing Rc-only callers; `unsafe` is confined
    to the Region arm methods; the discriminator branch must
    not affect Rc-arm hot-path performance measurably (profile
    after) | Success: `cargo test -p cs-gc --features regions`
    runs 20 existing rc_only tests green; `cargo test -p cs-gc`
    (no feature) runs 13 tracing tests green._

---

## Iter 3 — `Gc::new_in` + debug-mode validity

- [ ] 4. Implement `Gc::new_in(region, v)` + thread-local
  region tracking
  - File: `crates/cs-gc/src/rc_only.rs`,
    `crates/cs-gc/src/region.rs`
  - Add `Gc::new_in(region: &Region, value: T) -> Gc<T>` that
    delegates to `Region::alloc`.
  - Add `LIVE_REGION_IDS: RefCell<HashSet<RegionId>>` thread-
    local in `region.rs`.
  - `Region::new` inserts its id; `Region::Drop` removes it.
  - Add a `#[cfg(debug_assertions)]`-gated check in every
    Region-arm `Gc<T>` method that requires the region to be
    live (deref, clone, etc.). Panic with
    `"cs_gc::Gc<T>: region {id:?} dropped while handle outstanding"`.
  - Purpose: enable region allocation; catch use-after-region-
    drop in dev builds.
  - _Leverage: std `RefCell`, `HashSet`._
  - _Requirements: FR-2, FR-5_
  - _Prompt: Role: Rust developer with thread-local
    diagnostic patterns | Task: Implement `Gc::new_in`
    delegating to `Region::alloc`, and the LIVE_REGION_IDS
    thread-local with debug-mode validity checks per design.md
    §"Component 6". The check fires on every Region-arm method
    that dereferences `ptr` | Restrictions: zero release-mode
    cost (use `#[cfg(debug_assertions)]`); the panic message
    must include the RegionId for diagnostic value;
    LIVE_REGION_IDS uses `with`-pattern, not `static mut` |
    Success: a unit test in `region.rs` creates a region,
    allocates a Gc, drops the region, accesses the Gc → panics
    with the expected diagnostic in debug builds._

- [ ] 5. Integration tests for region lifetime
  - File: `crates/cs-gc/tests/region.rs` (new)
  - Cover:
    - `region_alloc_basic_lifetime` — Region::new, alloc 10
      i64 Gc, region drop, observe Drop sentinel.
    - `region_clone_bumps_strong_count` — clone twice, check
      strong=3, drop two clones, check strong=1.
    - `region_strong_count_does_not_drive_reclamation` —
      drop all Gc clones while region still alive, verify
      payload still accessible (region holds it).
    - `region_drop_releases_outstanding_handles_debug` —
      `#[cfg(debug_assertions)]`-gated panic test.
    - `region_alloc_microbench` — 10⁶ allocations, asserts
      per-alloc latency < 5ns (NFR-1).
    - `region_bulk_free_microbench` — 10⁶ allocations,
      asserts region drop < 50ms (NFR-2).
  - Purpose: lock in FR-3, FR-5, NFR-1, NFR-2.
  - _Leverage: existing cs-gc test patterns; std::time for
    perf measurement._
  - _Requirements: FR-1, FR-3, FR-5, NFR-1, NFR-2_
  - _Prompt: Role: Rust QA engineer | Task: Write 6
    integration tests in `crates/cs-gc/tests/region.rs`
    covering FR-1/3/5 and NFR-1/2 per the list. Perf tests
    use `std::time::Instant`; skip on debug builds (cfg-gated)
    | Restrictions: tests pass on both debug and release
    where applicable; no flaky tests (use generous bounds
    for perf assertions, see countable-memory's
    cycle_collect_timing for the cfg(debug_assertions)
    pattern) | Success: all 6 tests green under
    `cargo test -p cs-gc --features regions --test region
    --release`._

---

## Iter 4 — `Gc::promote` for escape-to-Rc

- [ ] 6. Implement `Gc::promote` for `T: Clone`
  - File: `crates/cs-gc/src/rc_only.rs`
  - Add `Gc::promote(this: &mut Self)` per design.md
    §"Component 7":
    ```rust
    impl<T: 'static + Clone> Gc<T> {
        pub fn promote(this: &mut Self) {
            if let GcRepr::Region { ptr, .. } = &this.0 {
                let cloned = unsafe { (*ptr.as_ptr()).value.clone() };
                this.0 = GcRepr::Rc(Rc::new(cloned));
            }
            // Rc arm: no-op.
        }
    }
    ```
  - For deep promote of values with internal Gc references,
    add a `Promote` trait (next task).
  - Purpose: support escape-to-Rc for layer-5-detected escapes.
  - _Leverage: existing Clone bounds; the Rc arm machinery._
  - _Requirements: FR-4_
  - _Prompt: Role: Rust systems engineer | Task: Add
    `Gc::promote(this: &mut Self)` per design.md §"Component 7"
    | Restrictions: only modify if the current variant is
    Region; Rc arm is a no-op; the clone happens through the
    standard `T: Clone` bound | Success: a test promotes a
    `Gc<i64>` allocated in a region, drops the region, reads
    the value — should still return the original i64 (deep
    cloned into Rc heap)._

- [ ] 7. `Promote` trait for deep promotion of cs-core types
  - File: `crates/cs-core/src/value.rs`
  - Add a `Promote` trait (also gated on `regions`):
    ```rust
    pub trait Promote {
        fn promote_deep(&mut self);
    }

    impl Promote for Value {
        fn promote_deep(&mut self) {
            match self {
                Value::Pair(p) => {
                    cs_gc::Gc::promote(p);
                    p.car.borrow_mut().promote_deep();
                    p.cdr.borrow_mut().promote_deep();
                }
                Value::Vector(v) => { /* similar */ }
                /* other Gc-bearing variants */
                _ => {}
            }
        }
    }
    ```
  - Purpose: deep-promote a Value tree from region to Rc.
  - _Leverage: existing CycleVisit walk patterns; the Pair
    accessors._
  - _Requirements: FR-4_
  - _Prompt: Role: Rust developer doing a mechanical trait
    impl across cs-core types | Task: Add the `Promote` trait
    and impls for each heap-bearing Value variant per the task
    description | Restrictions: deep-promote recursively
    through containers; handle Hashtable's items vec; do not
    touch Procedure (Rc<dyn> is already global) | Success: a
    test allocates a `Pair` in a region, set-cdr! it to
    another region-allocated Pair, calls `promote_deep`,
    drops the region — the resulting Pair tree is intact in
    Rc storage._

- [ ] 8. Promote tests
  - File: `crates/cs-runtime/tests/region_promote.rs` (new)
  - Cover:
    - Single-level: region-allocated Pair → promote → drop
      region → access works.
    - Two-level: Pair with Pair-valued cdr → promote_deep →
      drop region → traversal works.
    - Mixed: region Pair with Rc-allocated inner → promote
      → mixed-mode handled correctly.
  - _Leverage: cs-runtime's Runtime + region helpers._
  - _Requirements: FR-4_
  - _Prompt: Role: Rust QA engineer | Task: Write 3 tests
    in `crates/cs-runtime/tests/region_promote.rs` per the
    list; cfg-gated to `regions` feature | Restrictions:
    use the public Runtime + Region API only; no internal
    fields | Success: `cargo test -p cs-runtime --features
    regions --test region_promote` green._

---

## Iter 5 — Cycle-detector integration

- [ ] 9. `Gc::is_region(&self) -> bool` accessor
  - File: `crates/cs-gc/src/rc_only.rs`
  - Add a simple discriminator query:
    ```rust
    impl<T: ?Sized> Gc<T> {
        pub fn is_region(this: &Self) -> bool {
            matches!(this.0, GcRepr::Region { .. })
        }
    }
    ```
  - Purpose: let cs-runtime skip cycle detection on region-
    allocated mutations.
  - _Leverage: GcRepr enum._
  - _Requirements: FR-8_
  - _Prompt: Role: Rust developer | Task: Add `Gc::is_region`
    discriminator accessor under `#[cfg(feature = "regions")]`
    | Restrictions: zero cost on the Rc arm; the method takes
    `&Self` to match the existing accessor naming convention
    (Gc::ptr_eq style) | Success: cargo build green; test
    asserts `Gc::is_region(Gc::new(0))` is false and
    `Gc::is_region(Gc::new_in(&Region::new(), 0))` is true._

- [ ] 10. Skip cycle detection on region-allocated mutations
  - File: `crates/cs-runtime/src/builtins/mod.rs`
  - In `b_set_car` and `b_set_cdr`, guard the
    `check_and_break` call:
    ```rust
    #[cfg(all(feature = "countable-memory", feature = "regions"))]
    if cs_gc::Gc::is_region(p) {
        // Region-allocated cycle reclaims via region drop.
        return Ok(Value::Unspecified);
    }
    #[cfg(feature = "countable-memory")]
    cs_gc::cycle::check_and_break(p, |p| { ... });
    ```
  - Same for the VM tier's `vm_set_car_gc` / `vm_set_cdr_gc`
    helpers if they're wired to check_and_break (they aren't
    today, per iter 7.1.x.z note).
  - Purpose: avoid false-positive cycle break attempts on
    region-allocated values (FR-8).
  - _Leverage: existing cycle-detector wiring._
  - _Requirements: FR-8_
  - _Prompt: Role: Rust developer | Task: Modify b_set_car
    and b_set_cdr in crates/cs-runtime/src/builtins/mod.rs
    to skip the cycle detector when the mutated pair is
    region-allocated. Use the new `Gc::is_region` accessor
    | Restrictions: only skip cycle detection; the set_car /
    set_cdr mutation itself still runs | Success: a new test
    in crates/cs-runtime/tests/region_cycle.rs builds a
    cyclic Pair in a region via set-cdr!, asserts no cycle
    detection fires (cycle_detection_count stays 0), drops
    the region — no leak (verified via a Drop sentinel
    counter)._

---

## Iter 6 — Flip default + ADR 0016 + exit report

- [ ] 11. Flip `regions` to default-on workspace-wide
  - File: `crates/cs-gc/Cargo.toml`, `crates/cs-core/Cargo.toml`,
    `crates/cs-runtime/Cargo.toml`, `crates/cs-vm/Cargo.toml`,
    workspace `Cargo.toml`
  - Set `default = [..., "regions"]` in each crate.
  - Update workspace's `cs-runtime` declaration to include
    `regions` in its features list.
  - Verify `cargo test --workspace --release` green.
  - Purpose: ship regions as production default.
  - _Leverage: previous iters._
  - _Requirements: All FRs + NFR-4, NFR-6_
  - _Prompt: Role: Rust release engineer | Task: Flip the
    `regions` feature to default-on in every workspace crate's
    `[features]` block. Validate workspace + conformance + WASM
    builds | Restrictions: do not modify Scheme semantics; the
    default `Gc::new` path stays Rc-backed (regions only
    activate via explicit `Gc::new_in` or future layer-5
    dispatch) | Success: `cargo test --workspace --release`
    and `cargo build --target wasm32-unknown-unknown -p
    cs-runtime --no-default-features --features ffi-trait`
    both green; conformance 117/117 maintained._

- [ ] 12. ADR 0016 + exit report + spec close
  - File: `docs/adr/0016-region-types.md` (new),
    `docs/milestones/region-memory-exit.md` (new),
    `.spec-workflow/specs/region-memory/{requirements,design,tasks}.md`
    (status update)
  - Write `docs/adr/0016-region-types.md` ratifying the design
    per requirements.md NFR-7:
    - Single-region per Gc (no region polymorphism for v1).
    - Copy-on-promote mechanism.
    - Debug-mode region-validity check.
    - Rc + region duality in `GcRepr<T>`.
  - Write `docs/milestones/region-memory-exit.md` per the
    M5/countable-memory exit-report style — what shipped,
    perf numbers, deferred work, what depends on this
    (escape-analysis spec depends on Region availability).
  - Mark spec status `CLOSED` in the three spec files.
  - Purpose: lock the layered architecture's layer 3 into
    project history.
  - _Leverage: ADR 0014 / ADR 0015 for style;
    countable-memory exit report._
  - _Requirements: NFR-7_
  - _Prompt: Role: Rust + documentation author | Task:
    Write ADR 0016 ratifying the region-types design per
    requirements.md NFR-7, write the exit report in the M5
    exit-report style covering iters 1-6 + perf numbers,
    mark spec status CLOSED | Restrictions: do not delete
    any iter-1-5 implementation; do not introduce
    breaking API changes; the region module stays opt-in via
    explicit `Gc::new_in` (no automatic dispatch yet — that
    awaits the `escape-analysis` spec) | Success: ADR
    landed; exit report includes the NFR-1/2/3 measurements;
    spec marked CLOSED in all three files._

---

## Sequencing summary

| Iter | Title | Reversible? | Default-on? |
|------|-------|-------------|-------------|
| 1 | bumpalo dep + Region type | yes | no |
| 2 | Gc<T> GcRepr discriminated union | yes | no |
| 3 | Gc::new_in + debug-mode validity | yes | no |
| 4 | Gc::promote for escape-to-Rc | yes | no |
| 5 | Cycle-detector region skip | yes | no |
| 6 | Flip default-on + ADR 0016 + exit | yes via revert | **yes** |

Iters 1–5 are pure additions behind a flag. Iter 6 flips the
default but the feature can still be disabled.

## Rollback story

- Iters 1–5: revert the iter's commit.
- Iter 6: revert to restore the off-by-default.

No "point of no return" iter in this spec — regions are
purely additive infrastructure that layer 5 will later wire
into automatic allocation dispatch.

## What this spec enables

After this spec:
- `cs_gc::Region` is the primitive layer 5 will use for
  region-bounded allocation.
- `Gc::new_in(region, v)` is the constructor the cs-typer
  effect inferencer will emit at proven-bounded sites.
- `Gc::promote` is the escape hatch for values whose lifetime
  is dynamically longer than expected.
- The cycle detector cleanly excludes region-allocated values
  from its detection / break logic.

The `escape-analysis` spec depends on these primitives.
