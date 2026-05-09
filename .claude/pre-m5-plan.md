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

### 3. Tag M4 complete + write M5 spec  [milestone gate]
Retroactively tag the current commit `m4-complete` and write
`docs/milestones/m4-exit.md`. Create the M5 spec under
`.spec-workflow/specs/gc/`.

### 4. M5 — Precise tracing GC  [the milestone]
Per ROADMAP.md:
- New `cs-gc` crate
- Swap `Rc<T>` → `Gc<T>` in `Value`
- Per-Runtime root set
- VM stack as root set
- Stop-the-world mark-and-sweep first; generational copying as
  follow-up
- 24-hour fuzz with leak detector
- p99 GC pause < 1ms on stdlib load

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
