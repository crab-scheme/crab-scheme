# Pre-M5 Plan

> Active plan for the iterative `/loop` work. Follow this in order. Each
> item is one or more iters; finish before starting the next.

## Goal

Lock the `Value` heap-shape surface and extend the data layer of the
runtime far enough that M5 (precise tracing GC) can swap `Rc<T>` for
`Gc<T>` without rewriting tracing code repeatedly.

## Order of operations

### 1. ~~Custom hash + equiv in `make-hashtable`~~  ✅ DONE (commit pending)
Landed: `Hashtable` gained `custom: Option<CustomHashFns>` carrying
hash + equiv `Value`s. `HtEqKind::Custom` variant + new constructor
`Hashtable::new_custom`. The 4 hashtable lookup ops moved from pure
to HO on the walker (use `apply_procedure` for Custom). VM tier got
matching `make_vm_builtin_syms` shims using `vm_call_sync`.
Conformance: `hashtable_custom.scm` (cli 66, vm 68).

### 2. Library namespace filtering  [pre-M5]  ← IN PROGRESS

**2a. Import-spec modifier syntax** ✅ DONE (commit pending)
Recognize `only`, `except`, `prefix`, `rename` shapes in `import`,
validate structure at expand time. `rename` is fully effective via
synthesized `(define new old)`. `only`/`except`/`prefix` are
syntactically accepted but don't restrict the global namespace yet —
that requires per-library scope frames (item 2b).

**2b. Library validation + registry** ✅ DONE (commit pending)
Strengthened `(library ...)` validation: name parts must be symbols,
optional trailing version sublist accepted; export names must be
identifiers; duplicate library declarations rejected. Library bodies
now run their `(import ...)` clause as part of the spliced begin so
renamed bindings are visible to body defines. Per-Expander
`libraries: HashMap<Vec<Symbol>, LibraryInfo>` registry tracks
declared libraries with their export lists; exposed via
`Expander::libraries()` for downstream consumers.

**2c. Per-library scope frames** ← NEXT (deferred to M9-territory)
Adding an env-frame system so each library has its own binding
namespace turns out to be a M9-class change — current model has
all top-level bindings in one frame. The Value heap shape doesn't
change with this work, so it's not strictly a pre-M5 blocker.

**Decision:** Mark item 2 sufficiently complete for pre-M5 purposes.
The library namespace machinery has the structural pieces (modifier
parsing, validation, registry) needed for full enforcement later.
Move to item 3 (M4-complete tag + M5 spec).

**Touches:** `cs-expand/src/lib.rs` primarily; some runtime env work
to support multiple top-level frames.

Doesn't affect `Value` layout — can technically land in any order,
but doing it before M5 keeps the runtime/env story stable.

### 3. Tag M4 complete + write M5 spec  ✅ DONE (commit pending)
- `docs/milestones/m4-exit.md` written summarizing M0–M4 + pre-M5
  prerequisites (1460 aggregate pass count, 68 cli files, 70 vm
  files, walker↔VM parity).
- ROADMAP.md updated with status column; M0–M4 marked done, M5
  marked next.
- `.spec-workflow/specs/gc/requirements.md` and
  `.spec-workflow/specs/gc/design.md` drafted.
- `bench/conformance-baseline.json` captures pre-M5 baseline.
- `m4-complete` annotated tag created at the commit (see
  `git tag m4-complete`).

### 4. M5 — Precise tracing GC  [the milestone]  ← IN PROGRESS

**4.A — `cs-gc` crate scaffold** ✅ DONE (commit pending)
- New crate: `crates/cs-gc/`
- Public API: `Gc<T>`, `Heap`, `Trace`, `Marker`, `Heap::collect()`
- Phase 1 backing: `Rc<Slot<T>>` so call-site ergonomics line up
  with the existing `Rc<RefCell<...>>` pattern in `cs-core`. Phase 2
  swaps to a hand-rolled arena allocator without changing the API.
- 7 isolated tests cover: alloc/deref, clone-shares, unrooted-drops,
  rooted-stays, transitive marking through a `Trace` impl, idempotent
  mark within a pass, and visited count.
- Workspace member registered; `cs-gc` builds clean.

**4.B — `Gc<T>` re-export in `cs-core`** ✅ DONE (commit pending)
- `Gc::new(value)` constructor added to cs-gc — heap-less migration
  bridge that lives by refcount alone (mirrors `Rc::new`).
- `cs-gc` added as a non-optional dependency of `cs-core`.
- `Gc`, `Heap`, `Marker`, `Trace` re-exported from `cs-core` so the
  rest of the workspace refers to `cs_core::Gc<T>` without a
  cs-gc direct dep.
- 4 smoke tests in `crates/cs-core/tests/gc_smoke.rs` confirm the
  `Rc<T>` patterns the runtime uses (clone-shares, RefCell mutability
  via shared cell, RefCell<Vec> shared view, ptr_eq) all work
  identically with `Gc<T>`.

**4.C — Migrate Value variants** ← IN PROGRESS

✅ `Value::String`
✅ `Value::ByteVector`
✅ `Value::Vector`
✅ `Value::Pair`
✅ `Value::Hashtable`
✅ `Value::Port`       (this iter)
✅ `Value::Promise`    (this iter)
⚠️ `Value::Procedure` — Trace supertrait + every concrete-proc
                        Trace impl landed (this iter), but the actual
                        `Rc<dyn Procedure>` → `Gc<dyn Procedure>`
                        swap requires `CoerceUnsized` for `Gc<T>`,
                        which is unstable on stable Rust. Stays on
                        Rc until cs-gc gets a manual unsizing path
                        or the project moves to nightly.

                        Phase 1 implication: closures + parameters
                        held only behind `Rc<dyn Procedure>` are
                        traced through (because their Trace impls
                        recurse into env / cell), but the Rc<dyn>
                        wrapper itself isn't a Gc allocation, so its
                        slot doesn't appear in Heap.slots. This is
                        functionally fine for Phase 1 (refcount
                        handles it; cycles via dyn Procedure leak as
                        documented in the M5 spec).

Each variant adds a `marker.mark(...)` call in the `Trace for Value`
match; non-migrated variants stay no-op until they migrate.

Also added `Gc::as_addr` for cycle-detection visited-sets (replaces
`Rc::as_ptr`).

**4.D — Per-Runtime root set wired** ✅ DONE (commit pending)
- Runtime now owns a `cs_gc::Heap`.
- Two persistent roots registered at `Runtime::new` time: the walker
  top `Frame` chain and the VM-tier root `Env`. Both clone an Rc
  into their root closure so the heap has a stable handle to walk.
- `Runtime::collect()` and `Runtime::heap()` accessors exposed.
- 6 smoke tests in `crates/cs-runtime/tests/gc_smoke.rs` exercise:
  alloc-free collect doesn't panic; defined globals survive collect
  on both walker and VM tiers; vector mutations are visible after
  collect; multiple back-to-back collects are idempotent.

The VM's per-call value/frame stacks are *not* yet registered —
they're transient stack-locals inside `run()`, not persisted on the
Runtime. Phase 1's collect() can run only "between" VM calls. Phase
2 + multi-shot continuations may move stack frames to the heap (per
the M5 spec) at which point they become root candidates.

**4.E — Drop the `Rc` import from `value.rs`** ✅ partial
- All migratable variants are off Rc (7 of 8). Trace-impl docstring
  in value.rs updated to reflect the final state and the rationale
  for `Procedure` staying on Rc (CoerceUnsized is unstable).
- `Rc<str>` symbol interning stays — it's immortal once interned.
- `Rc<dyn Procedure>` stays — the documented Phase 1 limitation.

`grep "Rc<" crates/cs-core/src/value.rs` shows 2 remaining:
the Procedure variant + the make_parameter constructor. Removing
these is a Phase 2 ADR decision (manual unsize via small `unsafe`,
or move to nightly).

Also added 5 stress tests in `crates/cs-runtime/tests/gc_stress.rs`
that interleave program evaluation with `collect()` calls across
strings, vectors, hashtables, closures (with captured cells), and
the VM tier — all green.

**4.F — Phase 2 swap**
Replace `Rc<Slot<T>>` backing with a hand-rolled arena. Same `Gc<T>`
external API. (Optional for M5 exit; Phase 1's cycle handling via
weak-ref bookkeeping is sufficient for the conformance gate, but
the perf gate needs the arena.)

**4.G — Fuzz target + criterion bench**
24-hour fuzz target + p99 pause-time bench. Captures the M5 exit
gates from the spec.

## Conformance baseline at start of plan

- 65 conformance test files
- CLI tier: 65 tests (cli conformance.rs)
- VM tier: 67 tests (vm_conformance.rs)
- Aggregate: 1340 individual Scheme tests passing
- Last commit: `d471f0b runtime: vector-append, subvector, make-list, list-copy`

## Loop cadence

Each `/loop` iter picks the next concrete sub-task from item 1 (then 2,
then 3+4). Iters land their changes, run both walker and VM
conformance, commit, and ScheduleWakeup.

When item 1 lands, update this file's "Order of operations" — strike
it through and bump to item 2. When all four land, retire this plan.
