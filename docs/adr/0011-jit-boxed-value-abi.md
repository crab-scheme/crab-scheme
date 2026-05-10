# ADR 0011 — JIT Boxed-Value ABI

> Status: **Proposed** for the milestone after M6 Phase 2.
> Companion: `docs/jit-detailed-plan.md` (end-state B items 5–10).
> Predecessor: ADR 0007 (JIT architecture, i64-only ABI).
> Predecessor: M6 Phase 2 exit (`docs/milestones/m6-phase2-exit.md`).

## Context

M6 Phase 2 closed with a four-tag immediate-value ABI: every JIT'd
function takes and returns `i64`, with the dispatcher boxing/unboxing
based on per-param and per-return type tags stored on the closure.
This works cleanly for Fixnum / Boolean / Character / Flonum bodies
but hits a hard wall at heap-pointer types (Pair, Vector, String,
Procedure, Hashtable, Port, Symbol, BigInt, Rational).

Bodies that touch heap-pointer values today fall through to bytecode:

- `(cons x y)` — allocation produces a Pair; can't be returned via i64.
- `(list a b c)` — same.
- `(define (f x) (lambda (y) (+ x y)))` — body creates a closure.
- `(other-proc x)` — calling another non-self closure needs to dispatch
  on a procedure value the JIT can't carry across the i64 boundary.
- `(vector-ref v i)` — `v` is a heap pointer the type guard rejects.

These cases are roughly half of real Scheme programs. The Phase 2
scoreboard ("typed numerics fast path") hits a ceiling here. To move
forward we need to commit to a calling convention that can carry
arbitrary `Value`s across the JIT boundary.

This ADR picks the convention. It does **not** add code — it ratifies
the design choices that the next 8–10 implementation iters will assume.

## Decisions

### D-1. Pass `Value` by tagged 64-bit word, with per-position metadata

JIT'd functions accept and return `i64` words whose meaning is decoded
via a *tag side channel* — the per-param and per-return tags already
on `VmClosure` (4 bits per slot, 8 slots max). The Phase 2 ABI
generalizes rather than gets replaced.

For each tag, the i64 word's interpretation:

| Tag         | i64 carries                              | Phase 2? |
|-------------|------------------------------------------|----------|
| `Fixnum`    | the i64 directly                         | ✅       |
| `Boolean`   | 0/1                                      | ✅       |
| `Character` | u32 codepoint (zero-extended)            | ✅       |
| `Flonum`    | f64 bit pattern (`f64::to_bits`)         | ✅       |
| **`Pair`**  | `Rc<Pair>::into_raw() as i64`            | new      |
| **`Vec`**   | `Gc<RefCell<Vec<Value>>>::into_raw() as i64` | new  |
| **`Str`**   | same shape as Vec                        | new      |
| **`BV`**    | same                                     | new      |
| **`Proc`**  | `Rc<dyn Procedure>::into_raw() as i64`   | new      |
| **`Sym`**   | `Symbol(u32)` zero-extended              | new      |
| **`Big`**   | heap-pointer to `BigInt`                 | new      |
| **`Rat`**   | heap-pointer to `BigRational`            | new      |
| **`Hash`**  | heap-pointer to `Hashtable`              | new      |
| **`Port`**  | heap-pointer to `Port`                   | new      |
| **`Any`**   | full `Value` reference (boxed; see D-3)  | new      |

The tag space stays at 4 bits (16 slots, currently 4 used). This is
"per-call ABI" — at one call to one closure we know exactly what the
operands are. Polymorphic call sites use `Any` (D-3).

**Alternatives considered:**

- **NaN-boxing the entire `Value` into one i64.** Pack the tag into
  the high bits of the f64, with payload in the low 51 bits. 51 bits
  is enough for fixnums up to 2^51 and 32-bit-aligned heap pointers
  on 64-bit OSes (with sign-extension). Used by SpiderMonkey, V8
  smis, Lua's tagged unions.
  - *Pro:* one ABI handles every type uniformly, no per-slot tag
    machinery, dispatch and unboxing are one mask + branch.
  - *Con:* loses the bit-identical i64 path for fixnums (we'd be
    masking on every read), bumps complexity in the runtime
    (existing Value enum stays — JIT would need its own NaN-box
    layout that the dispatcher converts to/from), and depends on
    pointer alignment guarantees that vary by allocator. Phase 2's
    perf wins came partly from raw i64 fixnum ops; NaN-boxing
    would erode that.
  - *Decision:* deferred. Worth revisiting if the per-slot tags
    become a bottleneck — they currently are not.

- **Pass `*const Value` (Rc-managed pointer) for everything.** Every
  JIT arg is a heap pointer to a full Value enum.
  - *Pro:* single ABI, no special-casing immediates.
  - *Con:* allocates one Value per call boundary even for a fixnum.
    Kills the typed-immediate perf wins from Phase 2. Out.

- **Pass `Value` by registers (multi-word ABI).** Use the SystemV ABI's
  multi-register return for a 2-word `(tag, payload)` pair.
  - *Pro:* tags travel with the value, no per-slot state on the closure.
  - *Con:* Cranelift signature complexity, ABI varies across platforms,
    every existing Phase 2 ptr/i64 path needs reshaping. The per-slot
    tags we already have do this job for free.
  - *Decision:* deferred. Could revisit if multiple JIT callers (not
    just the dispatcher) need to interop.

### D-2. Heap pointers carry an Rc bump on entry, drop on return

Every heap-pointer arg the dispatcher passes increments the source
`Rc`/`Gc` count before transmuting to i64; the JIT body's return
hands its result back as a fresh refcount the dispatcher then either
boxes into a `Value` (re-Rc's the same allocation) or drops.

This is the conservative path that mirrors how the existing `Value`
clone discipline works: every `Rc<T>` clone is a refcount bump, every
drop a decrement. We extend that to the JIT boundary.

**Rationale:**
- The JIT body can't reason about lifetime relationships on its own.
  Bumping at the boundary makes it safe to forget about ownership
  inside the body.
- The cost is one atomic increment per heap-pointer arg/return —
  negligible vs. the cost of running a non-leaf body.
- Matches the FFI convention from ADR 0008 D-3 (Pinned<'rt> bump on
  cross-boundary).

**Alternatives considered:**
- **Borrowed pointers (no bump).** Rely on the dispatcher to keep
  the source alive across the call. Saves the increment.
  - *Con:* if the JIT body stores the pointer somewhere reachable
    across calls (a closure capture, a hash table value), the borrow
    rule no longer holds. Too easy to violate.
  - *Decision:* rejected. We pay the bump.
- **Shared static storage for very common values.** `Null`, `True`,
  `False`, `Unspecified`, small Fixnums could be statically allocated
  with refcount = u32::MAX (effectively immortal). The bump still
  fires but is cheap because the increment never trips the drop.
  - *Decision:* nice-to-have follow-up; not required for the ADR.

### D-3. Polymorphic slots use `Any` — i64 carries `*const Value`

A call site that the type-feedback loop can't pin to one shape uses
the `Any` tag, which means the i64 is `Box::into_raw(Box<Value>)`.
The body receives a heap-allocated `Value` (full enum, all variants),
matches on it, and returns a fresh `Box<Value>` on the way out.

**Rationale:**
- Avoids combinatorial explosion: a 2-arg function with 4 possible
  arg types per slot is 16 specializations; with 12 types it's 144.
- Type feedback drives the common case toward monomorphic `Fixnum`
  / `Flonum` / etc. specializations; `Any` is the deopt-friendly
  fallback when feedback says the call site is megamorphic.
- The cost (one allocation per `Any` arg) is bounded — programs that
  hit `Any` extensively are not the JIT's target audience.

**Alternatives considered:**
- **No `Any` slot — always specialize.** Force every call site to
  a specific signature; deopt to bytecode for any type that doesn't
  match. *Con:* aggressively-polymorphic code (e.g. a generic
  list-processing function called with mixed types) would never JIT.
  Rejected.

### D-4. General `Call` lowering uses an inline cache + runtime helper

Lowering `Inst::Call(callee, args)` (where `callee` isn't a known
builtin or `SelfRef`) emits:

1. A monomorphic inline cache slot: stores the last-observed callee
   identity (a stable u32 closure-id from a per-runtime registry).
2. At call time:
   - Compare the live callee's id to the cached id.
   - If match: jump to the cached jit_ptr (compiled with the
     observed signature).
   - If mismatch: call `vm_jit_call_helper(callee_id, args...)` which
     dispatches polymorphically (via `vm_call_sync` or the JIT'd
     ptr if available); the result returns through the helper's
     i64 ABI.

The IC self-heals on monomorphism — first call records identity,
subsequent calls take the fast path.

**Rationale:**
- Matches the proven pattern from V8/SpiderMonkey/Hotspot: inline
  caches are the canonical answer to dynamic dispatch.
- Keeps the JIT body small (one compare + branch + indirect call).
- The slow path stays correct via the existing dispatch infra.

**Alternatives considered:**
- **No IC — always go through the helper.** Simpler but every call
  costs an extra function-call overhead. With monomorphic call sites
  being the common case, the IC pays for itself.
- **Polymorphic IC (PIC).** Cache 2–4 identities at one site. Out
  of scope for the first iter; can extend later if needed.

### D-5. Allocation lowering goes through extern "C" runtime helpers

Lowering `cons`, `list`, `vector`, `make-vector`, `make-string`,
`make-bytevector`, `string-ref`, etc. emits Cranelift calls to
`extern "C"` helpers in `cs-vm` (or `cs-runtime`):

```rust
extern "C" fn vm_alloc_pair(car: i64, car_tag: u8, cdr: i64, cdr_tag: u8) -> i64;
extern "C" fn vm_pair_car(pair: i64) -> i64;  // returns Any-tagged
extern "C" fn vm_pair_cdr(pair: i64) -> i64;
extern "C" fn vm_alloc_vector(n: u32) -> i64;
extern "C" fn vm_vector_ref(vec: i64, idx: i64) -> i64;
// ... etc.
```

The JIT body calls these via the Cranelift `call` opcode. The
helpers internally use `cs-core::Value` / `Pair` / `Vector` exactly
as the rest of the runtime does — no new allocator, no shadow heap.

**Rationale:**
- Matches the existing `vm_env_lookup_fixnum` / `vm_env_set_fixnum`
  helper pattern from M6 Phase 1.
- Keeps allocation policy under the runtime's control (GC pauses,
  region allocation, future arena schemes can swap in without JIT
  changes).
- `cargo` already ships `vm_alloc_*` symbols cross-crate via
  `#[no_mangle]` — Cranelift imports them with the existing
  `JITBuilder::symbol` mechanism.

**Alternatives considered:**
- **JIT directly emits allocation code.** Inline a fast-path
  bump-allocator. *Con:* needs intimate knowledge of the runtime's
  GC contract (write barriers, scan boundaries). Hard to keep in
  sync with `cs-gc` evolution. Out.

### D-6. Lambda creation in JIT body uses an extern helper

Lowering `Inst::Lambda(idx)` (closure construction) emits a call to
`extern "C" fn vm_make_closure(lambda_idx: u32, env_ptr: i64) -> i64`
which builds a fresh `VmClosure` with the captured env and returns a
heap-pointer-tagged i64.

The lambda body itself isn't lowered at this point — the result is a
closure that, on its own future call, may tier up and JIT.

**Rationale:**
- Closures are values that flow through programs; JIT'ing one's body
  is orthogonal to creating one.
- Matches the runtime's existing `VmClosure::new` semantics.

### D-7. Tail call uses Cranelift `tail_call`

Self-recursion (`CallSelf`) becomes `tail_call` instead of `call` for
tail positions, eliminating the stack frame.

For IC-monomorphic tail calls, when the cached `jit_ptr` matches, we
emit `tail_call` to that pointer; the helper-fallback path stays as
a regular call (it can't be in tail position because the helper
itself does work).

**Rationale:**
- Cranelift's `tail_call` is stable and works on x86_64 / ARM64.
- Without TCO, deep recursion (e.g. mutually recursive number-crunching)
  blows the native stack on JIT'd code that the walker handles fine.

### D-8. Type guards on heap-pointer args use a tag check

Today's per-param tag check works for immediates. Heap pointers need
an additional check that the tag matches the actual `Value` variant.
The dispatcher does this already (matching `Value::Pair(_)` etc.); we
extend it to the new tags.

For the JIT body, `Any`-tagged args may be of any variant — the body's
own match-on-Value dispatches polymorphically.

### D-9. Deopt path: returns a sentinel via the i64 ABI

When the JIT body needs to deopt mid-execution (overflow, type mismatch,
allocation failure), it returns a sentinel value the dispatcher
recognizes and re-dispatches through bytecode.

The sentinel: `i64::MIN_VALUE + 1` for fixnum slots (one shy of the
real `MIN_FIXNUM`, which programs do legitimately produce). For
heap-pointer tags, we use `0` (null pointer — never a legitimate
heap address).

The dispatcher checks the return; on sentinel, runs the call through
`vm_call_sync` and returns that result. Bumps the deopt counter on
the closure for feedback-driven recompile.

**Rationale:**
- No out-of-band signal (TLS flag, signal handler) needed for the
  common case.
- The sentinel is reserved at the ABI level so user programs can't
  accidentally produce it.

**Alternatives considered:**
- **TLS deopt flag.** Set a per-thread flag in the JIT body, check
  in the dispatcher. Cost: one extra TLS load per call. Marginal,
  but real. *Decision:* sentinel preferred; falls back to TLS if
  sentinel reservation proves problematic.

## Consequences

### Positive

- The JIT can finally cover non-leaf bodies (most real Scheme code).
- General `Call` + `apply` + Lambda creation + allocation all become
  reachable as separate iters with no further design churn.
- Tail-call optimization unlocks deep recursion at full JIT speed.
- The per-slot tag machinery from Phase 2 generalizes cleanly — no
  global ABI rewrite.

### Negative / risks

- Complexity of the runtime's helper surface. Each new lowered op
  (alloc, vector-ref, cons, etc.) needs a matching `extern "C"`
  helper that's careful with refcounts and the GC contract.
- IC machinery adds per-call-site state. Programs with a million
  call sites pay a million slots' worth of side-table memory. We
  cap at ~1 cache line per site; for typical hot loops this is
  negligible.
- The deopt sentinel reserves one bit pattern. If we ever need to
  return that exact i64 legitimately, we'll need to either widen
  the ABI (multi-word return) or pick a different sentinel.
- Multi-threading: the IC slots aren't atomic. Single-threaded use
  is fine; multi-threaded JIT would need atomic CAS on the IC slot
  or per-thread caches. Phase 2 was single-thread by construction;
  this ADR doesn't address multi-thread, just doesn't preclude it.

### Things that *don't* change

- The four-tag immediate ABI from Phase 2 (Fixnum/Boolean/Character/
  Flonum) keeps working unchanged. The new tags add to the encoding,
  they don't replace it.
- The `cs-rir` Function shape (params + blocks + insts + terminator)
  is unchanged. New ops are additions.
- The tier-up hook signature (`fn(&VmClosure, &[Value])`) is
  unchanged.
- The per-closure `jit_param_types` / `jit_return_type` tags stay 4
  bits each (16 slots; we use 4 today + 11 new = 15, leaving room
  for `Any` at slot 15 and one spare).
- `Runtime::install_jit` stays opt-in; the bytecode VM still works
  identically without it.

## Follow-ups

- [ ] iter AL: extend the `JIT_RT_*` tag space — add `JIT_RT_PAIR`,
  `JIT_RT_PROC`, `JIT_RT_ANY`, etc. as `pub const u8` in
  `cs-vm::vm`. No code paths use them yet; just reserves the
  encoding.
- [ ] iter AM: add `extern "C" fn vm_alloc_pair`, `vm_pair_car`,
  `vm_pair_cdr` in `cs-vm`. Wire them into `cs-jit-cranelift` via
  the existing `JITBuilder::symbol` pattern. No translator changes
  yet — just the helpers.
- [ ] iter AN: lower `cons` in `cs-jit-cranelift` to call
  `vm_alloc_pair`. First end-to-end Pair-returning JIT body.
- [ ] iter AO: monomorphic IC infrastructure — per-call-site slot in
  RIR + Cranelift, helper for the slow path.
- [ ] iter AP: lower general `Call` via the IC.
- [ ] iter AQ: tail-call lowering for `CallSelf` and IC-monomorphic
  calls.
- [ ] iter AR: lower `Lambda` via `vm_make_closure`.
- [ ] iter AS: Gabriel benchmark suite import — first real perf
  scoreboard.

## References

- `docs/milestones/m6-phase2-exit.md` — Phase 2 close report.
- `docs/jit-detailed-plan.md` — full deferred-items inventory.
- ADR 0007 — Phase 1 architecture (i64-only ABI).
- ADR 0008 — FFI convention (Pinned<'rt> bump pattern that D-2 mirrors).
- Cranelift's `tail_call` opcode — D-7.
- SpiderMonkey IC paper, Hotspot's adaptive optimization — D-4 prior art.
