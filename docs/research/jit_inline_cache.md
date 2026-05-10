# JIT Inline Cache Design for CrabScheme

> Research synthesis + concrete proposal for the next iter sequence.
> Companion to ADR 0011 (D-4 is the relevant decision).
> Reviewed against V8/JSC/SpiderMonkey/HotSpot/LuaJIT and the
> Hölzle–Chambers–Ungar PIC paper (1991).

---

## 1. Survey of existing approaches

### 1.1 Self / Strongtalk — the canonical PIC

Polymorphic inline caches originate with Hölzle, Chambers, and Ungar,
*"Optimizing dynamically-typed object-oriented languages with polymorphic
inline caches,"* ECOOP 1991 (LNCS 512), extending the monomorphic IC of
Deutsch & Schiffman, *"Efficient implementation of the Smalltalk-80
system,"* POPL 1984. The model still drives every production engine:

- Cache lives **at the call site**, not on the receiver.
- Each entry is a `(type, code-pointer)` pair.
- Misses grow the cache rather than evicting, becoming a fall-through
  linear search of up to `MAX_POLY` entries.
- Past `MAX_POLY` the site goes **megamorphic** and reverts to a
  generic runtime lookup.

Empirical baseline cited since the Smalltalk-80 paper: ~1/3 of call
sites stay unlinked; of linked ones, **~90% monomorphic, ~9%
polymorphic, ~1% megamorphic**. Recent Ruby data reports **~98%
monomorphism** in large benchmarks; Larose 2024 on SOM AST caches
finds 2-entry caches optimal. Lesson: optimize the monomorphic case
hard; make polymorphic correct-not-fast; let megamorphic cost be
obvious so we instrument it.

### 1.2 V8 (Ignition, Sparkplug, Maglev, TurboFan)

V8 separates cache from call site. Each `JSFunction` owns a
**FeedbackVector** — a heap array of slots, one per IC-bearing
bytecode (`v8/src/objects/feedback-vector.h`). A call slot packs
`SpeculationMode` (1 bit) + `CallFeedbackContent` (1 bit) + `CallCount`
(30 bits) alongside a *weak* pointer to the target (GC reclaims dead
targets). States transition `UNINITIALIZED → MONOMORPHIC →
POLYMORPHIC → MEGAMORPHIC`, with `MAX_POLY = 4` for property loads.
The `MegamorphicSentinel` is a singleton meaning "go through the
generic hash-table lookup." V8 does **not** patch code — the feedback
vector is a side table read inline. TurboFan and Maglev consume the
feedback at compile time and inline guarded direct calls, deopting
on guard failure. Modern trade-off: no SMC, simpler GC, one extra
load per IC check.

### 1.3 JavaScriptCore (LLInt, Baseline, DFG, FTL)

JSC attaches a `StructureStubInfo`
(`Source/JavaScriptCore/bytecode/StructureStubInfo.h`) to each
caching opcode. Stub generation runs through
`PolymorphicAccess::regenerate` and `AccessCase::generateImpl`
(`Source/JavaScriptCore/bytecode/PolymorphicAccess.cpp`). The
Baseline JIT actively repatches stub code via
`Source/JavaScriptCore/jit/Repatch.cpp` (`tryCacheGetBy`,
`operationGetByIdOptimize`). Cases chain as a list of type-guard
stubs ending in the slow path. Watchpoints invalidate stubs when
prototype chains mutate — Scheme has no prototype chain, so this
machinery is unnecessary for us.

### 1.4 SpiderMonkey CacheIR (Baseline, Warp/Ion)

SpiderMonkey's **CacheIR** (Mooij 2018, Lima et al. OOPSLA 2023,
doi:10.1145/3617651.3622979) is the modern engineering win: ICs
are encoded as a small linear bytecode shared across tiers. Each
IC-bearing site has a **linked list of stubs**; each stub is
generated from a CacheIR program plus a side **stub-data** table.
Same compiled native code is reused across stubs that differ only
in data (shape pointers, slot offsets, function pointers). Misses
extend the list. Warp consumes the same CacheIR to do its
specializations, unifying the feedback format across tiers. Heavy
upfront engineering (CacheIR interpreter, register allocator,
stub linker) — not appropriate for a young runtime, but the right
endpoint.

### 1.5 HotSpot C2

`CompiledIC` lives at the call site as a small patched sequence
(`hotspot/src/share/vm/code/compiledIC.cpp`). States: `Clean →
Monomorphic → Megamorphic` — no general polymorphic mode; C2
supports **bimorphic** inlining only (two receivers, two inlined
targets), after which the call goes megamorphic via a `VtableStub`.
Empirically (Shipilëv, *"JVM Anatomy Quark #16"*): mono optimized
325 ns/op, megamorphic via VtableStub 1070 ns/op — **3× cost** but
far better than the interpreter (~11.5 µs/op).

### 1.6 LuaJIT 2.x and Deegen

LuaJIT is tracing, not IC-based; dispatch specialization happens via
trace guards, not call-site caches. The Deegen baseline JIT for Lua
(Liu, 2023) is the most relevant prior art for a Scheme-style call
IC: a **direct-call mode** that caches on function-object identity,
transitioning to a **closure-call mode** that caches on function
prototype when a factory pattern is detected. Each stub chains into
an SMC region; misses JIT-compile and link a new stub at the head
of the chain.

### 1.7 Summary

| Engine            | Cache location           | Patching               | Max poly |
|-------------------|--------------------------|------------------------|---------:|
| Self / Strongtalk | Inline at call site      | SMC                    | ~4–8     |
| V8 Ignition/Maglev| Side table (FeedbackVec) | None — re-read inline  | 4        |
| JSC Baseline      | StructureStubInfo + SMC  | Code patching          | ~8       |
| SpiderMonkey      | CacheIR stub chain       | List extension         | ~8       |
| HotSpot C2        | Inline at call site      | SMC, GC-coordinated    | 2 (bi)   |
| Deegen baseline   | SMC region + side meta   | List extension via SMC | ~4       |

---

## 2. Trade-off matrix

| Axis                  | Per-call-site slot       | Per-closure JIT ptr   | Side feedback vector       |
|-----------------------|--------------------------|-----------------------|----------------------------|
| Hot-path cost         | 1 load + cmp + branch    | 0 (already in closure)| 1 load + cmp               |
| Cold-path cost        | Compile stub, patch SMC  | None                  | None                       |
| Memory per site       | ~1 cache line (64 B)     | 0 extra               | 1 slot/site in heap array  |
| Self-modifying code   | Yes                      | No                    | No                         |
| GC interaction        | Keep stubs alive         | Closure is a root     | Scan vector                |
| Polymorphic extension | Chain stubs              | Hard (1 ptr/closure)  | Grow polymorphic array     |
| Multi-thread safety   | Atomic patch needed      | `Cell` already !Send  | Atomic slot write          |
| Cranelift fit         | Awkward (no SMC API)     | Easy                  | Easy                       |

For CrabScheme, **per-call-site slots stored in a side table with the
slot's address baked into the JIT body** fits best: V8-style hot path
without needing Cranelift to support SMC. Slot *address* is constant;
slot *contents* mutate freely.

---

## 3. Recommendation for CrabScheme

**Decision:** per-call-site monomorphic-first IC stored in a side
table owned by the JIT module, addressed by a pointer baked into the
compiled body. Polymorphic chain via runtime helper; megamorphic via
`vm_call_sync`.

### 3.1 Where the slot lives

A `Box<[IcSlot]>` inside the `Lowerer` (the cs-jit-cranelift module
state) plus a per-site index. The slot's *address* (`&boxed[i]`) is
known at lowering time and baked into the JIT body as a constant
pointer load. Matches V8's "address constant, contents mutate" model
and sidesteps SMC.

**Not per-closure:** one closure can be called from many sites;
identity-caching on the closure forgets call-site locality (site A
sees only `f`, site B alternates `f`/`g`). Per-call-site is what the
literature universally recommends.

**Not per-receiver:** the per-closure `VmClosure::jit_ptr` is already
the *direct* JIT pointer (cs-vm/src/vm.rs:740). The IC is specifically
the call-site cache *of* that pointer so the site can short-circuit
without re-reading the closure struct.

### 3.2 Cache key — `u32` closure id

| Candidate            | Pros                       | Cons                              |
|----------------------|----------------------------|-----------------------------------|
| Raw `*const VmClosure` | Free                     | UAF on closure drop               |
| **`u32` closure id** | Stable, compact, int cmp   | Need per-runtime ID registry      |
| Symbol id            | Already exists             | Top-level redefinition breaks it  |
| Lambda-idx + env hash| Captures alpha-equivalence | Hash on hot path too slow         |

The `u32` id wins for the same reason V8 uses weak-pointer-with-cell:
stable, cheap to compare, cleared on drop. Registry is a
`Vec<Weak<VmClosure>>` we scan at GC checkpoints. One increment +
map insert per closure construction — amortized negligible.

### 3.3 Cache value — `(jit_ptr, arity, param_types)`

Same shape as `VmClosure::set_jit_ptr` (cs-vm/src/vm.rs:834) so
dispatcher and IC share decode logic. The arity ride-along lets the
IC fast path skip the closure-struct load. Caching the packed
`param_types` (one `u32`, same encoding as the `JIT_RT_*` tag space
at cs-vm/src/vm.rs:794-824) encodes the guard in-line.

### 3.4 Dispatch sequence in JIT'd code

For `(call f a b)` where `f` is neither `SelfRef` nor a known builtin:

```text
; pseudo-Cranelift CLIF
v_cls_id    = call vm_closure_id_of(f)      ; u32 id
v_slot_ptr  = iconst <const addr of IcSlot> ; baked in
v_cached    = load.i32 v_slot_ptr+0
v_hit       = icmp eq v_cls_id, v_cached
brif v_hit, ic_hit, ic_miss

ic_hit:
v_jit_ptr   = load.i64 v_slot_ptr+8
v_result    = call_indirect <sig>, v_jit_ptr, a, b
jump cont(v_result)

ic_miss:
v_result    = call vm_jit_call_helper(callee, slot_ptr, a, b)
jump cont(v_result)

cont(v_phi):
```

Hot path: **3 instructions before the indirect call** — load,
compare, conditional branch. On x86_64/ARM64 the predictor handles
this with ~5% overhead over a direct call (measured in V8/JSC).

`vm_jit_call_helper` is the new runtime function: (a) looks up the
live closure by id, (b) ensures it has a JIT pointer (fires
tier-up if not), (c) checks arg tags match the slot's signature
(or boxes Any), (d) updates the IC slot (or chains), and (e)
dispatches via `vm_call_sync`.

### 3.5 Integration with existing type-feedback ABI

The IC slot stores `param_types: u32` (same packed encoding as
`VmClosure::jit_param_types`). On hit, the JIT body was already
compiled assuming those param types — the type guard is **implicit
in the closure-id check** because two different specializations of
the same source get different ids (closure id is per-`VmClosure`,
and recompile with new signature bumps a generation, which changes
the effective id). No double-guarding.

For the **Any** ABI lane (ADR 0011 D-3), the slot stores `JIT_RT_ANY`
in every param slot and the helper boxes arbitrary values on miss.

### 3.6 Polymorphic strategy

Start **monomorphic-only** for the first iter. Inline 1 entry; misses
go to `vm_jit_call_helper`. The helper logs miss events on the slot;
once miss_count exceeds `IC_POLY_PROMOTE_THRESHOLD = 16` it flips
the slot to **polymorphic mode**: a separate
`Vec<(id, jit_ptr, arity, param_types)>` chained off the slot.

In poly mode, JIT'd code still does the inline mono check first (V8
keeps the most-recently-hit entry inline), falling through to the
helper which probes the chain. `MAX_POLY = 4` (V8's number; the
empirical literature consensus is the long tail past 4 is
megamorphic anyway).

Past `MAX_POLY`, the slot transitions to **megamorphic**: helper
unconditionally goes through `vm_call_sync` and stops adding
entries. Per-slot counter exposed via `jit-status` so we can
diagnose mega sites.

### 3.7 Invalidation

Three cases:

1. **Closure dropped.** Walk `Weak<VmClosure>` registry on tier-up
   / GC checkpoints; clear any slot whose `cached_id` no longer
   resolves.
2. **JIT pointer cleared for recompile.** `clear_jit_for_recompile`
   (cs-vm/src/vm.rs:897) bumps a generation counter; the live
   closure's id changes; IC slots cached on old id miss and rebuild.
3. **Top-level redefinition** (`(define f ...)` twice): new closure,
   new id, old IC entries miss and re-resolve. Matches Guile/Racket
   REPL semantics.

**Multi-threading:** out of scope (ADR 0011 §Negative). Single-thread
`Cell<u32>` and `Cell<*const u8>` sufficient.

---

## 4. Data structure sketches

```rust
// cs-vm/src/vm.rs

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClosureId(pub u32);

pub struct VmClosure {
    // ... existing fields (lambda_idx, env, bc, tier, jit_ptr, ...) ...
    /// Stable identity for IC comparison. Bumped on
    /// clear_jit_for_recompile so IC slots cached on the old id miss
    /// and rebuild.
    id: Cell<ClosureId>,
}

// cs-jit/src/ic.rs (new module)

#[repr(C)]                       // ABI-stable for JIT loads
pub struct IcSlot {
    pub cached_id:       Cell<u32>,        // u32::MAX = uninitialized
    pub jit_ptr:         Cell<*const u8>,
    pub jit_arity:       Cell<u32>,
    pub jit_param_types: Cell<u32>,
    pub miss_count:      Cell<u32>,
    pub state:           Cell<u8>,         // 0=mono, 1=poly, 2=mega
}

pub const IC_STATE_MONO: u8 = 0;
pub const IC_STATE_POLY: u8 = 1;
pub const IC_STATE_MEGA: u8 = 2;
pub const IC_POLY_PROMOTE_THRESHOLD: u32 = 16;
pub const IC_MAX_POLY: usize = 4;

pub struct PolyChain {
    pub entries: Vec<(u32, *const u8, u32, u32)>, // (id, jit_ptr, arity, param_tags)
}
```

```rust
// cs-vm/src/vm.rs — the helper

/// IC miss handler. Updates `slot` based on the live closure, then
/// dispatches via existing infrastructure.
///
/// SAFETY: `closure_any` is an Any-tagged Box<Value> from the JIT
/// (ADR 0011 D-3). `slot_ptr` is the constant address baked into the
/// JIT body's code.
#[no_mangle]
pub unsafe extern "C" fn vm_jit_call_helper(
    closure_any: i64,
    slot_ptr:    i64,
    a0: i64, a1: i64, a2: i64, a3: i64,
    a4: i64, a5: i64, a6: i64, a7: i64,
) -> i64 {
    let value: Box<Value> = unsafe { Box::from_raw(closure_any as *mut Value) };
    let slot: &IcSlot     = unsafe { &*(slot_ptr as *const IcSlot) };
    let proc_ = match *value {
        Value::Procedure(p) => p,
        _ => panic!("vm_jit_call_helper: callee not a procedure"),
    };
    if let Some(closure) = proc_.as_any().downcast_ref::<VmClosure>() {
        update_ic_slot(slot, closure);
        let args = collect_args(slot.jit_arity.get() as usize,
                                [a0,a1,a2,a3,a4,a5,a6,a7],
                                slot.jit_param_types.get());
        return value_to_any_i64(
            vm_call_sync(&Value::Procedure(proc_), &args, syms_thread_local())
                .expect("call failed")
        );
    }
    // Non-VmClosure procedures: bypass IC, route to sync directly.
    value_to_any_i64(
        vm_call_sync(&Value::Procedure(proc_), &collect_args(/*…*/),
                     syms_thread_local()).expect("call failed")
    )
}
```

Lowering sketch is in §3.4. The Cranelift side imports
`vm_jit_call_helper` and `vm_closure_id_from_any` symbols at module
construction (same `JITBuilder::symbol` pattern as the existing
`vm_alloc_pair` etc. at cs-jit-cranelift/src/lowering.rs:138-146)
and allocates one `IcSlot` index per `Inst::Call` encountered.

---

## 5. Iter-by-iter implementation plan

**iter AT-1 — IC slot infrastructure + closure-id registry.**
Files: `crates/cs-jit/src/ic.rs` (new), `crates/cs-vm/src/vm.rs`
(extend `VmClosure` with `id: Cell<ClosureId>`, add registry on
`Runtime`). Stamp id at `VmClosure::new`. No JIT code changes.
Deliverable: closure-id round-trip test; `IcSlot::new` / `::reset`
unit tests; `size_of::<IcSlot>() == 64` assertion.

**iter AT-2 — `vm_jit_call_helper` + monomorphic slot update.**
Files: `crates/cs-vm/src/vm.rs`. Implement `vm_jit_call_helper`,
`update_ic_slot`, `vm_closure_id_from_any`. Logic: first call records
`(id, jit_ptr, arity, param_types)`; mismatch bumps miss_count.
Poly + mega still stubbed (asserts).
Deliverable: helper unit test driving slot through cold→warm states
without JIT involvement.

**iter AT-3 — `Inst::Call` lowering, monomorphic path.**
Files: `crates/cs-vm/src/jit_translate.rs` (replace the
`StackEntry::Value(_)` arm at line 1102 — currently
`Unsupported("Call with non-builtin non-self callee not yet supported")` —
with an `Inst::Call(dst, callee, args)` emission);
`crates/cs-jit-cranelift/src/lowering.rs` (lower `Inst::Call` via
the sequence in §3.4; the current `Inst::Call(_,_,_)` arm at line 964
is the site to replace). Import the two new helper symbols. Allocate
IC slots per Call into the `Lowerer`'s slot table.
Deliverable: first end-to-end JIT'd body that calls another closure.
Test: `(define (g x) (+ x 1)) (define (f x) (g x))` where `f` tiers
up and `(g x)` dispatches through the IC.

**iter AT-4 — Polymorphic chain.**
Files: `crates/cs-jit/src/ic.rs`, `crates/cs-vm/src/vm.rs`.
Implement `PolyChain`, mono→poly transition (miss_count >
`IC_POLY_PROMOTE_THRESHOLD`), helper chain walk. JIT'd code
unchanged — inline mono check still the fast path; polymorphism
only changes the helper's miss handling.
Deliverable: test with two callees alternating at one site
(`(if cond f g)` pattern); verify second-class chains in; verify
mono path stays for the most-recent entry.

**iter AT-5 — Megamorphic fallback + invalidation + scoreboard.**
Files: same. Implement transition to `IC_STATE_MEGA` past
`IC_MAX_POLY` and the unconditional `vm_call_sync` route. Implement
`clear_ic_for_closure` (or generation-bump approach) called from
`VmClosure::clear_jit_for_recompile`. Add `jit-status` reporting:
per-slot state, miss counts, total IC slot count.
Deliverable: benchmark — Gabriel `puzzle` / `boyer` (highly
polymorphic dispatch) at least matches bytecode VM; microbench shows
5×+ speedup on a monomorphic indirect-call hot loop. Exit report.

---

## 6. Open questions / deferred

- **Tail-call IC.** `Inst::Call` in tail position should use
  `return_call_indirect` on the hit path. Defer to iter AT-6; the
  wrapper pattern from ADR 0011 D-7 already covers `CallSelf`.
- **Closure-call mode (Deegen-style).** When the callee is a factory
  closure (same lambda_idx, different env), cache on
  `(lambda_idx, env-fingerprint)` instead of closure id. Speculative
  win; ship mono first, measure.
- **CacheIR-style stub sharing.** Worth revisiting once we have
  multiple IC kinds (call, vector-ref, hashtable-ref). Until then the
  per-kind helper is lower-energy.

---

## References

- Deutsch, L. Peter; Schiffman, Allan M. *"Efficient Implementation of
  the Smalltalk-80 System."* POPL 1984. **Monomorphic IC origin.**
- Hölzle, Urs; Chambers, Craig; Ungar, David. *"Optimizing
  Dynamically-Typed Object-Oriented Languages with Polymorphic Inline
  Caches."* ECOOP 1991, LNCS 512. **PIC origin.**
- Mooij, Jan de. *"CacheIR: A new approach to Inline Caching in
  Firefox,"* Mozilla Hacks, 2018; Lima et al., *"CacheIR: The
  Benefits of a Structured Representation for Inline Caches,"*
  OOPSLA 2023 (doi:10.1145/3617651.3622979).
- Liu, "Building a Baseline JIT for Lua Automatically"
  (sillycross.github.io, 2023). **Deegen call-IC modes.**
- Shipilëv, Aleksey. *"JVM Anatomy Quark #16: Megamorphic Virtual
  Calls,"* shipilev.net, 2018. **HotSpot bimorphic perf data.**
- V8 source: `src/objects/feedback-vector.h`, `src/ic/ic.cc`,
  `src/ic/accessor-assembler.h` (github.com/v8/v8).
- JSC source: `Source/JavaScriptCore/bytecode/StructureStubInfo.h`,
  `Source/JavaScriptCore/bytecode/PolymorphicAccess.cpp`,
  `Source/JavaScriptCore/jit/Repatch.cpp`,
  `Source/JavaScriptCore/jit/JITInlineCacheGenerator.h`.
- HotSpot: `hotspot/src/share/vm/code/compiledIC.cpp`; OpenJDK Wiki,
  *"Overview of CompiledIC and CompiledStaticCall."*
- Lima, Caio. *"Inline Cache Implementation on JSC,"*
  caiolima.github.io, 2020.
- Henderson, Nathan. *"On Policy Decisions of Polymorphic Inline
  Caches in JavaScript Engines"* (MSc, U. Alberta, 2021).
- Wingo, Andy. *"Design Notes on Inline Caches in Guile,"*
  wingolog.org, 2018; *"Inline Cache Applications in Scheme,"* 2012.
- Larose, Octave. *"Inline caching in our AST interpreter,"* 2024.
  **2-entry IC optimal empirically for SOM.**
- Cranelift docs: `wasmtime/cranelift/docs/ir.md` — `call_indirect`,
  `return_call_indirect`, signature import.

CrabScheme source touch points:
- `crates/cs-vm/src/vm.rs:465-540` — `try_dispatch_jit` (IC short-circuits this path).
- `crates/cs-vm/src/vm.rs:731-775` — `VmClosure` struct (closure-id field site).
- `crates/cs-vm/src/vm.rs:794-824` — `JIT_RT_*` tags (IC param_types encoding).
- `crates/cs-vm/src/vm.rs:897` — `clear_jit_for_recompile` (gen bump site).
- `crates/cs-vm/src/jit_translate.rs:1102-1106` — current "Unsupported" arm that AT-3 replaces.
- `crates/cs-jit-cranelift/src/lowering.rs:929-943` — `Inst::CallSelf` lowering (template for `Inst::Call`).
- `crates/cs-jit-cranelift/src/lowering.rs:964-969` — current `Inst::Call(_,_,_) → Unsupported` site.
- ADR 0011 D-4 — ratified the IC approach this design operationalizes.
