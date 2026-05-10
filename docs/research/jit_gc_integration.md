# CrabScheme JIT-GC Integration — Design Proposal

> Status: **Research / Proposed** for the milestone following M6 Phase 4.
> Companions: ADR 0006 (GC design), ADR 0011 (JIT boxed-value ABI).
> Context: at commit `c04d70e`, M6 Phase 4 iter AV's `AnyClone`/`AnyDrop`
> linear-typing model is the current bandaid — known fragile under
> multi-use joins, loops, and recursive Any-arg calls.

## 1. Survey of Existing Approaches

JIT-allocated values escaping the GC's view is a classical hazard.
Solutions cluster into three families.

### 1.1 Precise stack maps — HotSpot, Cranelift

**HotSpot**'s JIT (C1, C2) emits one *oop map* per safepoint listing
every register and stack slot holding a GC `oop` at that PC. JIT'd
code polls a protected page at loop back-edges, calls, and returns;
the GC `mprotect`s the page, the polling thread takes a SIGSEGV, and
the signal handler runs the GC with the oop maps as the precise root
set. See HotSpot glossary entries for "oop map" and "safepoint"
(openjdk.org/groups/hotspot/docs/HotSpotGlossary.html); Ragozin's
"Safepoints in HotSpot JVM" (2012) is the canonical walkthrough.

**Cranelift's user-stack-maps API** (Fitzgen, "New Stack Maps for
Wasmtime and Cranelift", 2024-09-10) ports this to Rust. The
frontend marks values:

```rust
builder.declare_value_needs_stack_map(v);   // an SSA Value
builder.declare_var_needs_stack_map(var);   // a FunctionBuilder Variable
```

`cranelift-frontend` runs liveness over marked values, inserts spills
before each call (every `call` is an implicit safepoint), reloads
after. The post-compile artifact is `UserStackMap` records — each a
list of `(ir::Type, offset_from_SP)` pairs at one PC. The runtime
walks frames at GC time, reads SP-relative offsets, marks each slot
as a GC root. The production consumer today is Wasmtime's wasm-gc
proposal (PR #1832). See `cranelift-codegen/src/ir/user_stack_maps.rs`.

### 1.2 Conservative scanning — JavaScriptCore Riptide, Boehm

**Riptide** (WebKit, r209827; Pizlo, "Introducing Riptide", 2017)
scans stack and registers conservatively: every word is tested
against live allocation bounds, and any in-range word is treated as
a root. False positives are rare in 64-bit address spaces. The JIT
pays nothing — heap pointers can sit in any register. Riptide uses
Steele's retreating-wavefront write barrier ("Multiprocessing
compactifying GC", CACM 1975), inlined at every heap store. The
permanent cost: no compaction, ever — moving an allocation would
require rewriting on-stack words it can't distinguish from integers.
Boehm-Demers-Weiser ("Garbage Collection in an Uncooperative
Environment", SPE 1988) is the canonical reference.

### 1.3 Tracing-aware allocation — LuaJIT 3.0, MMTk

**LuaJIT 3.0**'s proposed quad-color GC (luajit.org/New-Garbage-
Collector, ~2012) ties allocation to GC-owned arena pages; the trace
compiler bumps a pointer into the current arena. No stack maps —
Lua's FRAME records carry type info, so the GC walks frames using
the same metadata as the interpreter.

**MMTk** (Blackburn et al., "Oil and water? High performance GC in
Java with MMTk", ICSE 2004) is a language-independent GC framework
with a Rust `mmtk-core` crate. VMs implement `VMBinding`:
`VMObjectModel`, `VMScanning` (`scan_roots_in_mutator_thread`,
`scan_object`), `VMCollection`, and a custom `VMSlot`. The payoff:
multiple production-grade plans (Immix, GenImmix, MarkCompact) by
feature flag. Larose's SOM case study ("Adding GC to our Rust-based
interpreters with MMTk", 2025) documents ~6 months of refactoring;
rooting remains the binding's problem.

### 1.4 V8 Maglev/Orinoco — tier-aware composition

V8 emits precise stack maps from TurboFan and Maglev, plus a Handle
Scope (intrusive list of `Local<>`) for C++ glue. Each tier owns its
own stack maps; OSR/deopt reconstructs unboxed state from the deopt
metadata before handing control to the lower tier. The lesson for
CrabScheme: tier transitions need their own metadata. ADR 0011's
deopt sentinel handles the i64 boundary; what's missing is GC
visibility of in-progress values inside the JIT body.

### 1.5 Refcounting (current CrabScheme path)

CPython, Swift ARC, and CrabScheme's existing `AnyClone`/`AnyDrop`
all bookkeep per-pointer-write rather than per-PC. JIT'd code
inserts incref/decref at clone/drop points. Deutsch & Bobrow
(CACM 1976) is the classical deferred-RC paper; Blackburn & McKinley
("Ulterior reference counting", OOPSLA 2003) is the modern variant
powering MMTk's RC plan. The downside for CrabScheme: cycles leak
without a separate cycle collector, and every `AnyClone` allocates a
fresh `Box<Value>`.

## 2. Trade-off Matrix

Evaluating against CrabScheme's constraints — Cranelift-based JIT,
precise mark-sweep `cs-gc` already in place, i64-only ABI per
ADR 0011, `Gc<T>`-backed heap variants but `Box<Value>` slipping
through the JIT:

| Dimension | Precise (Cranelift stack maps) | Conservative (Boehm) | Refcount on Box (today) |
|---|---|---|---|
| Cycle collection | Works with `cs-gc` mark-sweep | Works (non-moving only) | No |
| Future compaction (ADR 0006 Phase 2) | Compatible | Blocks forever | Blocks (raw `Box`) |
| WASM target (M10) | Works | No host stack to scan | Works |
| Cranelift integration | First-class API | Bolt-on side tables | None |
| Per-call cost | 1 spill+reload per live ref | Zero | 1 alloc per AnyClone |
| Migration | Incremental, can co-exist | Single big-bang | n/a |
| Soundness today | Sound when wired | Sound but coarse | Unsound at joins/loops |
| ADR 0006 alignment | Matches "precise rooting" ✅ | Contradicts | Off-axis |

Decisive: **precise stack maps** is the only option compatible with
ADR 0006's precise-rooting decision, the WASM target, and the
Phase 2 arena plan. Conservative is excluded by WASM. Refcount-on-
Box is what's in place and known broken.

## 3. Recommendation

**Adopt Cranelift's user-stack-maps API as the primary mechanism for
tracking JIT-live `Gc<T>` references, and lift the JIT's allocation
contract from `Box::into_raw(Box<Value>)` to a raw-handle form of
`Gc<Value>`.** Keep ADR 0011's i64-only ABI; the i64 carries a `Gc`
handle whose slot is rooted by stack maps at every call.

Rationale:

1. **ADR 0006 already commits to precise rooting.** Wiring stack
   maps into `Heap`'s root walker is the straight-line completion
   of that decision, not a new bet.

2. **Cranelift's API is purpose-built.** `cranelift-frontend`
   inserts spills automatically; we don't write liveness ourselves.
   `UserStackMap::entries()` yields `(Type, SP-offset)` pairs we
   walk during `Heap::collect`.

3. **The leak is the bug, not the symptom.** Today every `AnyClone`
   allocates a fresh `Box<Value>`. Switching the carrier to a `Gc`
   handle makes a clone a refcount bump (`Rc::clone` on the inner
   slot) — no allocation — and a drop a `Rc::decrement_strong_count`.

4. **Migration is incremental.** Phase 1 `cs-gc` stays
   `Rc<Slot<T>>`-backed; cycle collection still goes through
   `Heap::collect`. The stack maps catch what the JIT body holds
   across calls; existing `Trace` impls catch reachability through
   Pairs/Vectors. `AnyClone`/`AnyDrop` can stay as a fallback during
   transition and be retired iter-by-iter.

5. **OSR and deopt stay clean.** ADR 0011's sentinel pattern is
   unchanged; the i64-carrying-pointer cases now point at GC slots
   the bytecode VM can pick up directly.

Trade-off: stack maps spill *every* live GC ref across *every* call
in the JIT body. For a tight `(map proc lst)` loop that's one spill
per iteration — what HotSpot pays. Cranelift's optimizer can
sometimes elide loop-invariant spills; we accept the cost otherwise.

## 4. API Sketches

Rust pseudocode against real CrabScheme types (`Gc<T>`, `Trace`,
`Heap`, `Marker`, `Value`).

### 4.1 `Gc<Value>` as the JIT carrier (`cs-gc`)

```rust
impl<T: Trace + 'static> Gc<T> {
    /// Hand off as a raw handle for ABI use; pair with from_raw_jit.
    pub fn into_raw_jit(this: Self) -> *const () {
        Rc::into_raw(this.inner) as *const ()
    }
    /// # Safety: ptr must be a live, owned handle.
    pub unsafe fn from_raw_jit(ptr: *const ()) -> Self {
        Gc { inner: Rc::from_raw(ptr as *const Slot<T>) }
    }
    /// Bump strong count without taking ownership (used by frontend
    /// when materialising a spill-slot view during scan_frame).
    pub unsafe fn raw_incref(ptr: *const ()) {
        Rc::increment_strong_count(ptr as *const Slot<T>);
    }
}
```

### 4.2 New runtime helpers (`cs-vm/src/vm.rs`)

```rust
pub const JIT_RT_GC: u8 = 0x10;   // new tag, slot reserved in ADR 0011 D-1

#[no_mangle]
pub unsafe extern "C" fn vm_alloc_pair_gc(
    car: i64, car_tag: u8, cdr: i64, cdr_tag: u8,
) -> i64 {
    let car_v = unsafe { i64_to_value(car, car_tag) };
    let cdr_v = unsafe { i64_to_value(cdr, cdr_tag) };
    let pair = Pair::new(car_v, cdr_v);                         // Gc<Pair>
    let v: Gc<Value> = Heap::current().alloc(Value::Pair(pair));
    Gc::into_raw_jit(v) as i64
}

#[no_mangle]
pub unsafe extern "C" fn vm_gc_clone(ptr: i64) -> i64 {
    Gc::<Value>::raw_incref(ptr as *const ());
    ptr   // a clone is the same address with one more strong count
}

#[no_mangle]
pub unsafe extern "C" fn vm_gc_drop(ptr: i64) {
    drop(unsafe { Gc::<Value>::from_raw_jit(ptr as *const ()) });
}
```

`vm_pair_car_gc` / `vm_pair_cdr_gc` / `vm_pair_p_gc` / `vm_null_p_gc`
follow the same shape (replacing the existing Box-based helpers at
`cs-vm/src/vm.rs:346-413`).

### 4.3 Lowering (`cs-jit-cranelift/src/lowering.rs`)

```rust
Inst::Cons(dst, car, _, cdr, _) => {
    let r = b.ins().call(alloc_pair_gc_fnref, &[/* ... */]);
    let v = b.inst_results(r)[0];
    b.declare_value_needs_stack_map(v);   // NEW — frontend handles spill/reload
    map.insert(*dst, v);
}
```

Every SSA value tagged as a heap-pointer type (`Type::Pair`,
`Type::Vector`, `Type::String`, `Type::Procedure`, `Type::Any` when
GC-backed, etc.) gets `declare_value_needs_stack_map` at definition.
The frontend spills around every `call` instruction automatically.

### 4.4 Stack-map root walker (`cs-vm/src/jit_runtime.rs`, new)

```rust
pub struct JitStackMaps {
    pub by_pc: HashMap<u32, UserStackMap>,
    pub base: *const u8,
}

/// Walk one JIT'd frame; mark each spilled GC ref.
pub unsafe fn scan_frame(
    frame_sp: *mut u8, return_pc: *const u8,
    maps: &JitStackMaps, marker: &mut Marker,
) {
    let pc_off = (return_pc as usize - maps.base as usize) as u32;
    let Some(sm) = maps.by_pc.get(&pc_off) else { return };
    for (ty, offset) in sm.entries() {
        debug_assert_eq!(ty, I64);
        let slot = frame_sp.add(offset as usize) as *const *const ();
        let handle = *slot;
        // Borrow without bumping refcount (frame owns the slot).
        let g = ManuallyDrop::new(Gc::<Value>::from_raw_jit(handle));
        marker.mark(&*g);
    }
}
```

`Heap::add_root` registers a closure per JIT-installed function; on
`collect()` it walks frames via FP-chain (Cranelift emits FP frames
with `unwind_info` on by default on x86_64 and ARM64).

### 4.5 Deopt path

Unchanged from ADR 0011 D-9: the sentinel (`0` for heap-pointer tags)
fires from a JIT helper, the dispatcher re-routes to bytecode. The
spilled slots are still live (stack-map-rooted); the dispatcher reads
them as `Gc<Value>` and hands them to the VM. No re-alloc, no
double-free.

## 5. Iter-by-Iter Plan

Five iters, each a clean exit gate. Names follow the M6 Phase 4 iter
convention (BB onwards, since BA — `c04d70e` — is HEAD).

### Iter BB — Reserve `JIT_RT_GC`, document the migration

- Add `JIT_RT_GC: u8 = 0x10` in `cs-vm::vm`. Unused this iter.
- Write ADR 0012 ratifying this design; cite ADR 0006 and 0011.
- Add `Gc::into_raw_jit` / `from_raw_jit` / `raw_incref` to `cs-gc`
  (cfg-gated `feature = "jit"`).

Deliverables: 1 ADR, 3 new `Gc` methods, 1 const. **Risk: low.**

### Iter BC — Stack-map registry + runtime helpers (no codegen yet)

- New `cs-vm/src/jit_runtime.rs`: `JitStackMaps`, `scan_frame`,
  the `*_gc` helper registry (parallel to today's Box helpers).
- Plumb a `JitFrameDescriptor` into `VmClosure` carrying the stack
  maps for this body.
- Extend `Heap::add_root` with a JIT-aware variant that walks the
  current host stack via FP chain (x86_64 + ARM64 gated;
  `cfg(any(target_arch = "x86_64", target_arch = "aarch64"))`).

Deliverables: 1 module, 1 root-walk integration, unit test for
`scan_frame` on a hand-crafted frame. **Risk: medium** — frame
walking is platform-specific. Mitigation: fallback to today's path
on unsupported targets.

### Iter BD — Cranelift stack-map plumbing for `Cons`

- Update `Inst::Cons` lowering at `lowering.rs:744`: call
  `vm_alloc_pair_gc`, mark result with
  `declare_value_needs_stack_map`.
- Wire `compiled_function.user_stack_maps()` into the `JitStackMaps`
  registry at JIT-install time.
- Differential test: same `(cons x y)` body via bytecode and JIT
  with interleaved `Heap::collect()` calls; equal results, zero
  leaks under valgrind.

Deliverables: 1 lowering site, 1 install-side hook, 1 differential
test. **Risk: high** — first use of `declare_value_needs_stack_map`,
spill discipline could surprise regalloc. Mitigation: keep the path
gated by a runtime flag for one iter; A/B against the M6 Phase 4
test suite.

### Iter BE — Extend to Car/Cdr/PairP/NullP; retire `AnyClone`/`AnyDrop` for Gc slots

- Lower `Car`, `Cdr`, `PairP`, `NullP` to `*_gc` helpers; mark
  results with `declare_value_needs_stack_map`.
- Translator stops emitting `AnyClone` for Gc-backed args
  (multi-use is now safe — stack map keeps the slot live).
- `AnyDrop` at return becomes `vm_gc_drop` for Gc-backed slots; for
  `JIT_RT_ANY` (still in use for the megamorphic case) the existing
  `Box::from_raw` path stays.

Deliverables: 4 lowering sites, translator changes, M6 Phase 4 tests
unchanged green. **Risk: medium** — multi-use semantics change.
Mitigation: keep `AnyClone`/`AnyDrop` reachable behind a build flag
for one iter so we can A/B.

### Iter BF — Cycle test, fuzz, arena-readiness sign-off

- Cycle test: `(define x (cons 1 #f)) (set-cdr! x x)` JIT'd, then
  `(gc-collect)`, then `(heap-live-slots)` drops to baseline. This
  is the test proving the leak-prone era is over.
- 24h fuzz with `gc_stress` extended to invoke JIT'd allocations.
- Document arena migration path: when ADR 0006 Phase 2 lands,
  stack-map slots become `*mut Header` and `scan_frame` is
  unchanged. Forward compatibility validated.

Deliverables: 1 cycle test, 1 fuzz extension, 1 milestone exit
report. **Risk: low** — everything hangs together.

## 6. Risk Callouts

1. **Cranelift's user-stack-maps API is recent (2024).** Production
   users are mostly Wasmtime wasm-gc. Pin the Cranelift version;
   track upstream issues like #1883 (skip the IR walk in
   `emit_stackmaps` when no r32/r64 are used).

2. **Frame-pointer walking is fragile across FFI.** ADR 0008's
   `Pinned<'rt>` host-side root anchor lets us stop scanning at the
   FFI boundary and resume on return. The walker must honor it.

3. **Phase 1 `cs-gc` can't break cycles through `Rc<dyn Procedure>`.**
   Acknowledged in ADR 0006. Stack maps don't change this; the
   long-term fix is ADR 0006 Phase 2 (arena swap).

4. **One extra `Gc<Value>` per JIT-returned heap value.** A
   `Heap::alloc` call where today there's a `Box::new`. Profile
   early; if hot, intern `Null`/`#t`/`#f` as global singletons
   (already planned in ADR 0011 D-2 footnote).

5. **Deopt with live spills.** The JIT body may have spilled refs
   when a helper deopts via sentinel. The dispatcher must read
   the spilled refs (still alive via the stack map) and reconstruct
   the bytecode-VM stack from them. Iter BE's differential test
   must cover "deopt with live GC refs in the frame".

## 7. References

### Internal
- `docs/adr/0006-gc-design.md`, `docs/adr/0007-jit-design.md`,
  `docs/adr/0008-ffi-design.md`, `docs/adr/0011-jit-boxed-value-abi.md`.
- `crates/cs-gc/src/lib.rs` — `Trace`, `Marker`, `Heap::add_root`.
- `crates/cs-rir/src/lib.rs` — `Inst::Cons`, `AnyClone`, `AnyDrop`,
  `BoxTyped`, `AnyToFix`.
- `crates/cs-vm/src/vm.rs` — `i64_to_value` (line 305), `value_to_any_i64`
  (line 329), `vm_alloc_pair` (line 346), `vm_value_clone` (line 402),
  `vm_value_drop` (line 411), `vm_box_typed` (line 422).
- `crates/cs-jit-cranelift/src/lowering.rs` — `Cons` (line 744),
  `AnyClone` (line 823), `AnyDrop` (line 835), `BoxTyped` (line 839).
- `crates/cs-core/src/value.rs` — `Value` enum (line 279), `Trace for
  Value` (line 321); all heap variants `Gc<T>` except `Procedure`.

### Cranelift / Wasmtime
- Fitzgen, N. "New Stack Maps for Wasmtime and Cranelift",
  bytecodealliance.org/articles/new-stack-maps-for-wasmtime
  (2024-09-10).
- `cranelift-codegen/src/ir/user_stack_maps.rs` — `UserStackMap`,
  `UserStackMapEntry`, `entries()`.
- `cranelift-frontend::FunctionBuilder::declare_value_needs_stack_map`
  and `declare_var_needs_stack_map`.
- Wasmtime PR #1832 — externref stack-map-based GC.

### Industrial GCs/JITs
- Pizlo, F. "Introducing Riptide: WebKit's Retreating Wavefront
  Concurrent Garbage Collector", WebKit blog, 2017-01.
- Steele, G. L. "Multiprocessing compactifying garbage collection",
  CACM 18:9, 1975.
- HotSpot glossary, openjdk.org/groups/hotspot/docs/HotSpotGlossary.html.
- Ragozin, A. "Safepoints in HotSpot JVM", 2012.
- V8 blog: "Trash talk: the Orinoco garbage collector" (2018),
  "Maglev — V8's Fastest Optimizing JIT" (2023).
- LuaJIT wiki, "New Garbage Collector" (Pall, c. 2012).

### Academic
- Blackburn, Cheng, McKinley. "Oil and water? High performance
  garbage collection in Java with MMTk", ICSE 2004.
- Blackburn, McKinley. "Ulterior reference counting: Fast garbage
  collection without a long wait", OOPSLA 2003.
- Deutsch, Bobrow. "An Efficient, Incremental, Automatic Garbage
  Collector", CACM 19:9, 1976.
- Hudson, Moss. "Incremental Collection of Mature Objects",
  IWMM 1992.
- Boehm, Weiser. "Garbage Collection in an Uncooperative
  Environment", SPE 18:9, 1988.
- Hölzle, U. "A Fast Write Barrier for Generational Garbage
  Collectors", OOPSLA workshop, 1993.

### Rust GC ecosystem
- Larose, O. "Adding garbage collection to our Rust-based
  interpreters with MMTk", octavelarose.github.io, 2025-01-30.
- Goregaokar, M. "A Tour of Safe Tracing GC Designs in Rust",
  manishearth.github.io, 2021-04-05.
- Turon, A. (boats), `shifgrethor` series — lifetime-bound roots
  in Rust.
- `mmtk-core` crate documentation, docs.rs/mmtk.
