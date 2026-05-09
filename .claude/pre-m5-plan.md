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

### 2. Library namespace filtering  [pre-M5]  ← NEXT
The expander recognizes `library` / `import` shape but doesn't enforce
namespace boundaries. Add proper scope frames per library and filter
imports.

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
