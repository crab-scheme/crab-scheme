# ADR 0012 — JIT Feature Completion: IC + GC Integration

> Status: **D-2 (GC integration) Landed** as of M6 Phase 4 iter BS.
> D-1 (Inline cache) — Proposed for M6 Phase 5.
> Companion research:
> - `docs/research/jit_inline_cache.md`
> - `docs/research/jit_gc_integration.md`

## Implementation status (M6 Phase 4 iters BD-BS)

| Iter | Deliverable                                                  | Status   |
| ---- | ------------------------------------------------------------ | -------- |
| BD   | Reserve `JIT_RT_GC = 16`; add `Gc::{into,from}_raw_jit`       | ✓ shipped |
| BE   | `JitStackMaps` registry + `scan_frame`                       | ✓ shipped |
| BF   | `declare_value_needs_stack_map` on Cons result                | ✓ shipped |
| BG   | Add `vm_alloc_pair_gc` + `value_to_gc_i64` helpers           | ✓ shipped |
| BH   | Six sibling Gc helpers (car/cdr/pair?/null?/clone/drop)      | ✓ shipped |
| BI   | Six more Gc helpers (box_typed/unbox_*/truthy/eq_any)         | ✓ shipped |
| BJ   | Atomic Box→Gc switch in dispatcher + all Cranelift call sites| ✓ shipped |
| BK   | Stack-map declarations on all Any-producing sites             | ✓ shipped |
| BL   | Harvest `user_stack_maps` post-compile                       | ✓ shipped |
| BM   | Thread maps to per-VmClosure storage                          | ✓ shipped |
| BN   | Per-thread active-JIT-frames TLS list (RAII JitFrameGuard)   | ✓ shipped |
| BO   | Route JIT allocations through `Heap::alloc` via TLS pointer   | ✓ shipped |
| BP   | `Runtime::with_active` installs Heap in JIT TLS              | ✓ shipped |
| BQ   | Verify GC-after-JIT survival + reclamation tests              | ✓ shipped |
| BS   | Close GC-during-JIT gap (refcount-only soundness)            | ✓ shipped |

**GC integration scope shipped**: JIT-allocated `Gc<Value>`s register
with the Runtime's Heap. The tracing GC sees them as weak-ref slots
and can trace through Value::Trace from walker-tier roots. A manual
`collect()` after a JIT body returns correctly preserves
walker-reachable values and reclaims unreachable ones.

**GC-during-JIT soundness (closed in iter BS)**: `Heap::collect()`
firing *while a JIT body is mid-execution* (e.g. from auto-collect
inside `vm_alloc_pair_gc → Heap::alloc`) is sound under the Phase-1
`cs-gc` design without explicit JIT-stack-map root scanning. The
argument is refcount-driven: every `Gc::into_raw_jit` handle sitting
in a JIT spill slot contributes a strong count of 1 to the underlying
`Rc<Slot<T>>`. Phase-1 `Heap::collect`'s sweep retains a slot iff its
`Weak::strong_count() > 0`, so it cannot reclaim any allocation that
is referenced by a live JIT stack slot. Linear-consumption helpers
(`vm_pair_car_gc`, `vm_value_drop_gc`, etc.) decrement the count when
the JIT body transfers ownership *out* of the spill slot, at which
point the slot is dead from the body's perspective; the body never
reads it again, so the sweep's reclamation is correct.

The iter BS stress test
(`diff_jit_collect_during_jit_body_keeps_live_pairs` in
`crates/cs-runtime/tests/jit_differential.rs`) exercises this under
maximum pressure: `Heap::set_auto_collect(true)` plus
`Heap::set_threshold(1)` makes every JIT-body allocation trigger a
full collect cycle, and the body's two-Cons sum still computes
correctly. Public APIs `has_active_jit_frames`,
`active_jit_frame_count`, and `scan_all_active_conservatively` are
exported from `cs-vm::jit_stackmap` for introspection and as hooks
for a future precise scanner.

**Why precise root scanning was deferred (and not needed for Phase
1)**: a conservative-by-PC scanner that reads every recorded spill
slot without knowing which PC the frame is actually paused at would
read dangling pointers in the consume-on-use ABI cases (the slot's
strong count has already dropped to 0). Precise scanning requires
either inline-assembly FP-chain walking or signal-handler safepoint
polling — both platform-specific and out of scope while the refcount
invariant supplies soundness. When Phase 2 (arena / compacting GC)
lands, allocations need to be *moved*, which requires precise root
locations: a future iter adds the FP-chain walker then.

## Context

At commit `859896a` (M6 Phase 4 iter BC), the JIT covers a usable
slice of Scheme but two architectural holes block the rest:

1. **No general Call.** Bodies that call any non-self closure refuse
   to JIT. Only `Inst::CallSelf` and inline-specialized builtins are
   wired. This is the single biggest unlock.
2. **Bypassing the GC.** `Inst::Cons` allocates `Box<Value>` via
   `Box::into_raw`. The mark-sweep `cs-gc` never sees these boxes.
   `Inst::AnyClone` / `Inst::AnyDrop` bandaid the lifetime through
   linear typing but fragility under joins, loops, and recursive
   Any-arg calls is documented and growing.

Both holes are well-studied in production JITs. Two research agents
surveyed the landscape; this ADR records the decisions, the rationale,
and the iter sequence that delivers them.

## Decisions

### D-1 — Inline cache shape

**Per-call-site monomorphic-first IC stored in a side table owned by
the JIT module**, addressed by a pointer baked into the compiled body
as a constant.

| Knob              | Choice                                        |
| ----------------- | --------------------------------------------- |
| Slot location     | Side table `Box<[IcSlot]>` in `Lowerer`       |
| Slot addressing   | Pointer constant baked into JIT body          |
| Cache key         | `u32` stable closure id (`ClosureId`)         |
| Cache value       | `(jit_ptr, arity, param_types: u32)`          |
| Polymorphism cap  | `MAX_POLY = 4`, then megamorphic              |
| Miss path         | Runtime helper → fall back to `vm_call_sync`  |
| Invalidation      | Generation counter; bumped on JIT recompile   |

**Why per-call-site:** the literature (Hölzle/Chambers/Ungar ECOOP
1991; V8, JSC, SpiderMonkey all production) is universal — call-site
locality dominates closure locality. Per-closure caching forgets
which site is calling, conflating sites that alternate vs sites that
stabilize.

**Why side table over self-modifying code:** Cranelift doesn't
expose runtime patching naturally. V8's FeedbackVector + SpiderMonkey
CacheIR both use side tables; the empirical cost is one extra load,
well below the dispatch-stub-jump cost SMC saves. Our deopt model
(today's `clear_jit_for_recompile` at cs-vm/src/vm.rs) already
assumes pointer-address stability, so this also stays coherent.

**Why `u32` ClosureId over raw `*const VmClosure`:** stable across
GC, cheap to compare, cleared on drop without dangling. Registry is a
`Vec<Weak<VmClosure>>` walked at GC checkpoints. Symbol-based keys
break under redefinition; lambda-idx + env hash is too slow on hot.

### D-2 — GC integration strategy

**Adopt Cranelift's user-stack-maps API** (`declare_value_needs_stack_map`,
`UserStackMap`) and **lift the JIT's allocation contract from
`Box::into_raw(Box<Value>)` to a raw-handle form of `Gc<Value>`.**

| Knob                  | Choice                                         |
| --------------------- | ---------------------------------------------- |
| Root tracking         | Cranelift user stack maps (precise)            |
| ABI carrier           | `Gc<Value>` raw handle (vs current `Box<Value>`) |
| Helper rename         | `vm_alloc_pair` → `vm_alloc_pair_gc`, etc.     |
| Reference protocol    | refcount via `Rc::increment_strong_count`      |
| New tag               | `JIT_RT_GC` — replaces `JIT_RT_ANY` semantics  |
| Frame walking         | FP-chain on x86_64 + aarch64 (unwind_info on)  |
| Migration             | Box helpers stay one iter for A/B safety       |

**Why precise stack maps over conservative scanning:** WASM target
(M10) forbids conservative scanning; ADR 0006 already commits to
precise rooting; Cranelift now offers exactly the API we need
(`UserStackMap` with `(ir::Type, SP-offset)` entries — Fitzgen, "New
Stack Maps for Wasmtime and Cranelift", 2024-09).

**Why `Gc<Value>` over keeping `Box<Value>` + side-mark:** the boxes
hold `Gc<Pair>` etc. internally, so we already pay one indirection;
hoisting the JIT carrier to `Gc<Value>` eliminates the Box layer
entirely and lets the GC trace through `Value::Pair(gc)` naturally
without a separate registry. Refcount via `Rc::increment_strong_count`
is what `AnyClone` already approximates — formalize it.

**Why not refcount-only (current):** cycles leak, AnyClone fragility,
no path to MMTk arena (Phase 2 of GC).

### D-3 — Ordering & layering

GC integration ships **before** the IC. Two reasons:

1. The IC slot contents (jit_ptr) need a stable lifetime. Today's
   `clear_jit_for_recompile` invalidates by pointer — the IC will
   too. The GC migration also reaches into `VmClosure` lifecycle, so
   doing it second creates merge churn against the IC's call-site
   tables.
2. AnyClone/AnyDrop fragility is currently *gated* by linear-typing
   discipline in the translator — adding general Call multiplies the
   non-linear cases (a callee's arg is the same Any value being held
   by the caller's continuation). Land GC first, then the IC inherits
   precise rooting for free.

## Non-decisions / deferred

- **MMTk integration.** ADR 0006 leaves the door open for MMTk; this
  ADR opts to use Cranelift stack maps with the existing mark-sweep
  `cs-gc` first. MMTk is a Phase 2 conversion.
- **Polymorphic chain implementation.** Recommendation is a linked
  list of `IcSlot` (V8/JSC style) but the data structure can be
  flat-array if profiling shows ≤ 4 entries dominates. Defer until iters
  BG-BH show empirical hit rate distributions.
- **Lambda creation in JIT bodies.** Needed for `(lambda (x) ...)`
  inside hot code; out of scope for this ADR. Comes after IC lands.

## Iter sequence

Eight iters across two milestones. Names continue M6 Phase 4's
convention (BD onward; BB and BC are committed).

### Phase 4 closeout — GC integration (BD–BG)

| Iter | Deliverable                                               | Risk   |
| ---- | --------------------------------------------------------- | ------ |
| BD   | Reserve `JIT_RT_GC = 0x10`; add `Gc::{into,from}_raw_jit` | Low    |
| BE   | `JitStackMaps` registry; `scan_frame`; FP-chain walker    | Medium |
| BF   | `Cons` lowered via `*_gc` helpers + stack-map plumbing    | High   |
| BG   | Extend `Car`/`Cdr`/`PairP`/`NullP`; retire `AnyClone`/`AnyDrop` for Gc slots | High |

After BG, `Inst::AnyClone`/`AnyDrop` stay only on the i64-tagged
immediate decoy path (currently unreachable post-BG). Mark them
deprecated; remove in a later cleanup.

### Phase 5 — Inline cache (BH–BL)

| Iter | Deliverable                                               | Risk   |
| ---- | --------------------------------------------------------- | ------ |
| BH   | `ClosureId` registry; `IcSlot` struct; reserve side table | Low    |
| BI   | Lowered call sequence (load-compare-call); helper miss path | High |
| BJ   | First end-to-end JIT'd non-self call (warmup test)         | High   |
| BK   | Polymorphic chain (2–4 entries); promotion at `miss_count > 16` | Medium |
| BL   | Megamorphic + invalidation + scoreboard (perf telemetry)   | Medium |

## Risks & mitigations

- **Cranelift stack maps are new (2024-09).** Mitigate by gating the
  `*_gc` path behind a runtime flag for one iter (BF); A/B against
  the existing 39-test M6 Phase 4 suite before flipping the default.
- **FP-chain walking is platform-specific.** x86_64 + aarch64
  validated; gate via `cfg(any(target_arch = "x86_64", target_arch
  = "aarch64"))`; fall back to today's path otherwise.
- **IC + GC interaction.** ClosureId registry holds `Weak<VmClosure>`;
  GC visits each on `collect()` and clears IC slots whose id is
  unreachable. Tested via a closure-redefinition stress test in BL.
- **Deopt path must read spill slots as `Gc<Value>`, not `Box<Value>`.**
  The dispatcher tier-down at the sentinel reads the IC slot using
  `ManuallyDrop<Gc<Value>>` to avoid double-free. Covered by the BG
  differential test.

## References

- Research IC: `/tmp/jit_research_ic.md` (2743 words, 2025-05-10).
- Research GC: `/tmp/jit_research_gc.md` (2559 words, 2025-05-10).
- Fitzgen, "New Stack Maps for Wasmtime and Cranelift", 2024-09-10.
- Hölzle, Chambers, Ungar, "Optimizing Dynamically-Typed
  Object-Oriented Languages With Polymorphic Inline Caches", ECOOP
  1991.
- Deutsch, Schiffman, "Efficient Implementation of the Smalltalk-80
  System", POPL 1984.
- Pizlo, "Introducing Riptide", WebKit blog, 2017.
- ADR 0006 — GC design (precise rooting).
- ADR 0007 — JIT design.
- ADR 0011 — JIT boxed-value ABI (D-3, D-4 superseded by D-1/D-2 here).
