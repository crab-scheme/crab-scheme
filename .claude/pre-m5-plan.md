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

**4.G — Fuzz target + pause-time harness** ✅ partial (commit pending)
- `gc_fuzz.rs` (3 tests): hand-rolled deterministic LCG fuzzer
  that generates random Op sequences (define-list/string/vector/
  hashtable/counter, mutate, collect, read-length) and asserts no
  collect-during-trace panics. 16 seeds × 32 ops, 16 seeds × 16
  collect-after-each-step ops, plus one 256-op long run. Avoids
  proptest because tempfile→rustix→iconv is a Nix linker problem
  on this host; can swap to proptest later if the env supports it.
- `gc_timing.rs` (2 tests): records `collect()` durations on a
  modest heap (100 lists + 10 vectors + 10 hashtable-like
  structures) and on an empty Runtime. Asserts p99 < 10ms (loose
  Phase 1 bound; Phase 2 spec requires < 1ms). Phase 1 measured:
  p50 ≈ 2.3μs, p99 ≈ 4.3μs on this hardware — comfortably under
  the spec's 1ms threshold even before Phase 2.

What's still in 4.G but deferred:
- ✅ 24-hour fuzz CI workflow (`.github/workflows/m5-fuzz.yml`)
- ✅ ADR 0006 (`docs/adr/0006-gc-design.md`)
- ✅ Memory-baseline measurement (`gc_memory.rs`, this iter):
  factorial(200) → 4.38 MiB, 10k list → 6.08 MiB,
  10 fresh Runtimes → 4.33 MiB. All comfortably under the 80 MiB
  test ceiling. Captured in `bench/m5-phase1-baseline.json` for
  Phase 2 to compare against (≤ 1.2× target per M5 spec).
- Criterion-based bench harness (`bench/gc_pause.rs`) — only
  remaining 4.G item. The existing `gc_timing.rs` already records
  durations + p99; converting to criterion adds statistical analysis
  but no new coverage. Defer until Phase 2 makes the numbers
  meaningfully change.

## Conformance baseline at start of plan

- 65 conformance test files
- CLI tier: 65 tests (cli conformance.rs)
- VM tier: 67 tests (vm_conformance.rs)
- Aggregate: 1340 individual Scheme tests passing
- Last commit: `d471f0b runtime: vector-append, subvector, make-list, list-copy`

## R7RS conformance progress (post-M5)

Iters 117+ are filling R7RS gaps one per iter:
- 117 case-arrow `=>` form
- 118 r7rs port reads (read-string, char-ready?, read-u8, peek-u8, ...)
- 119 case-arrow else-clause
- 120 bytevector ops via `(u8-list->bytevector ...)` + open-input-string aliases
- 121 `#u8(...)` literal end-to-end (lex/parse/expand/eval)
- 122 `#\alarm`, `#\backspace`, `#\delete`, `#\escape` named chars + `#\xHH...`
- 123 string escapes `\v`, `\f`, `\|`, `\xHH;`, line continuation
- 124 `|pipe-quoted|` identifiers
- 125 R7RS port-output: write-string slicing, write-u8, write-bytevector,
  open-output-string/bytevector aliases, get-output-bytevector
- 126 R7RS file-error?, read-error? predicates + tagged conditions
  (open-input-file/open-output-file failures get &file-error tag)
- 127 wire &read-error tag into b_read on both walker and VM tiers
- 128 R7RS (exit) and (emergency-exit) — raise &exit-requested
  catchable condition with the value as a field; both tiers
- 129 R7RS port helpers: call-with-port, call-with-input-string,
  call-with-output-string. Both tiers via vm_call_sync shims
- 130 R7RS port management: close-input-port, close-output-port,
  flush-output-port, input-port-open?, output-port-open?
- 131 R7RS variadic eq predicates + list-set!: boolean=?, symbol=?,
  list-set!
- 132 R7RS vector-fill! optional start/end + new string-fill!
  with same R7RS arity
- 133 R7RS delay-force + iterative force on both tiers; make-promise
  now wraps a value as Forced (R7RS); delay/delay-force expansion
  uses internal __make-pending-promise to wrap a thunk
- 134 R7RS syntax-error special form: raises ExpandError::BadSyntax
  with message + irritants. Fires whenever the template is expanded —
  only "matched" branches in syntax-rules reach it.
- 135 R7RS string-copy + bytevector-copy with optional [start [end]];
  added missing string-set!
- 136 R7RS string->list, vector->list, string->vector, vector->string
  with optional [start [end]]
- 137 R7RS bytevector-fill! with optional [start [end]]
- 138 R7RS (string char ...) constructor — variadic char-collection
  to a new string
- 139 R7RS string-map / string-for-each multi-string forms; both
  tiers; VM error path now raises catchable conditions
- 140 R7RS char-ci=?, char-ci<?, char-ci<=?, char-ci>?, char-ci>=?
  + string-ci variants. Variadic, Unicode-aware via to_lowercase
  (this iter)

Current totals:
- 94 conformance test files (cli)
- VM tier: 96 tests
- Aggregate: 1917 individual Scheme tests passing

## Loop cadence

Each `/loop` iter picks the next concrete sub-task from item 1 (then 2,
then 3+4). Iters land their changes, run both walker and VM
conformance, commit, and ScheduleWakeup.

When item 1 lands, update this file's "Order of operations" — strike
it through and bump to item 2. When all four land, retire this plan.
