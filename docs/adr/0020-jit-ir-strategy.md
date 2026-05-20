# ADR 0020: JIT IR Strategy — Backend Consolidation, Tail-Conv Selection, and RIR Optimization Pipeline

> Status: Accepted (incremental); roadmap
> Date: 2026-05-20
> Depends on: ADR 0007 (JIT design), ADR 0011 (NanBox ABI), ADR 0012 (uniform-NB tier), ADR 0019 (bounce trampoline)
> Closes: issue #47 (map-style bodies lose JIT coverage). Roadmap follow-ups: #50 (retire the pure-fixnum tier), #51 (RIR unboxing + escape analysis)

## Context

Crab Scheme's JIT pipeline is:

```
Scheme → expand → bytecode (cs-vm) → RIR (cs-rir) → Cranelift CLIF → native
```

`cs-rir` (crate `cs-rir`) is the unifying JIT intermediate representation:
basic blocks with block parameters (close to SSA), an instruction set
(`Inst::Call`, `CallSelf`, `CallGeneral`, `Cons`, `Car`, arithmetic, etc.),
and terminators (`Term::Return`, `Jump`, `Branch`). Every JIT backend
consumes the same `RirFunction`; the pass framework in `cs-opt` and the
effect/type inference in `cs-typer` both operate on this IR.

### Current state: two divergent Cranelift backends

There are presently two backends that lower the same RIR:

1. **uniform-NB** (modern, default): NanboxValue `i64` ABI, inline type
   checks, proper inline-cache (IC) dispatch for cross-function calls.
   Correct on cross-function calls. Uses an outer `CallConv::SystemV`
   trampoline + an inner body function; the inner body's calling convention
   is chosen per body (see below).

2. **pure-fixnum / "specialized"** (legacy): raw `i64` ABI, faster for
   pure fixnum arithmetic — `fib(38)` runs ~65× over the VM, `tak` and
   `ack` similarly — but **miscompiles cross-function calls** (issue #19):
   a `CallGeneral` clobbers the register state so a subsequent
   `Car`/`Cdr`/`Cons`/`CallSelf` reads garbage and returns `Null` where
   the caller expects a pair. Self-recursive bodies that never use
   `Call`/`CallGeneral` (e.g. `binary-trees`, `alloc-stress`) compile
   correctly on this tier.

Nearly every JIT correctness bug this cycle traces to the two backends
disagreeing: issue #19 and #47 are both caused by the legacy tier's
cross-call miscompile; ADR 0019 was needed because tail calls fought the
host stack before a trampoline existed; allocation remains under-optimized
(alloc-stress ~3×, nqueens ~1.7×) because the RIR carries little dataflow
information.

The routing guard in `cs-runtime/src/jit.rs` (`rir_has_cross_function_call`)
was added as the immediate, no-regression fix for issue #19: when uniform-NB
declines a body that contains a `Call`/`CallGeneral`, the fallback is to the
VM rather than the miscompiling legacy tier. This guard is correct and
remains in place; it means that any body with a cross-function call that
uniform-NB declines gets no JIT at all, which is the wrong asymptote.

### The problem this ADR addresses

Three interrelated tensions:

1. **Two backends, one IR**: bug risk scales with divergence. Every
   optimization applied to uniform-NB must be separately verified (or
   re-applied) on the legacy tier. Maintaining two code generators for the
   same IR is technical debt without a performance justification once
   uniform-NB matches the legacy tier on pure-arithmetic benchmarks.

2. **`CallConv::Tail` is not always the right choice**: ADR 0019 uses
   `return_call` for tail-position `CallSelf`, which requires
   `CallConv::Tail` on the inner body. Tail convention makes every register
   caller-save (more spills, larger frames). For bodies whose self-recursion
   is *non-tail* — where `return_call` is never emitted — using Tail conv
   unnecessarily raises the per-frame cost and lowers the host-stack depth
   ceiling, causing overflow on moderately deep data recursion.

3. **The RIR optimization pipeline is underutilized**: `cs-opt` and
   `cs-typer` exist but the IR does not yet carry enough dataflow
   information for high-value optimizations: SSA-based unboxing (keeping
   fixnums and flonums in registers across the RIR instead of
   NanBox-tagged i64 round-trips) and escape analysis (stack or region
   allocation for non-escaping `cons`/closure allocations). These are
   where an investment in the IR strategy pays the largest performance
   dividend.

## Decision

Three interlocking strategies, documented together because they share the
same motivation (one correct, optimizing IR) and because progress on each
unlocks the next.

---

### Strategy A — Consolidate to one Cranelift backend

The RIR is already the unifying IR; the correct long-term state is one
backend (uniform-NB) that handles all body shapes correctly, with the
legacy pure-fixnum tier retired. The RIR's `Inst` type and `cs-rir`'s
verification pass (`verify_function`) are the shared contract; any future
backend (a future `cs-jit-holy`, AOT, etc.) should consume the same IR.

**Precondition for retirement**: uniform-NB must first match the legacy
tier's performance on pure-arithmetic self-recursion benchmarks (`fib`,
`tak`, `ack`) before the legacy tier is removed. Until that threshold is
met, both tiers coexist. This mirrors the approach taken by mature engine
projects: CPython's uop IR (PEP 744) and WebKit B3 both maintain one
optimizing IR for all tiers, not multiple IRs that can diverge.

The `rir_has_cross_function_call` guard in `cs-runtime/src/jit.rs` is
the correct safety valve while consolidation is in progress: it ensures
that bodies with cross-function calls are never silently miscompiled by
the legacy tier. As uniform-NB's coverage expands, fewer bodies will
reach this guard.

---

### Strategy B — Use Cranelift's guaranteed tail calls selectively

`return_call` / `CallConv::Tail` is the correct mechanism for tail-position
`CallSelf` (and ultimately for replacing the ADR-0019 bounce trampoline for
tail-position `Call`/`CallGeneral` as well). Cranelift's tail calling
convention is mature: wasm tail calls are on by default for x86_64,
aarch64, and riscv64 as of 2024. However, Tail convention makes every
register caller-save, which implies more spills and a larger per-frame cost
(approximately 7% on some benchmarks).

The resolution is to select the inner calling convention **by need** rather
than applying Tail unconditionally:

- If any block in the body ends in a tail-position `CallSelf` (so that a
  `return_call` will be emitted), the inner body requires `CallConv::Tail`
  (a `return_call` is only legal under Tail convention).
- If no block has a tail-position `CallSelf`, no `return_call` is emitted,
  so `CallConv::SystemV` is both sufficient and preferable: smaller frames,
  more registers available, higher host-stack depth ceiling.

Cross-function tail calls (tail-position `Call`/`CallGeneral`) continue to
use the ADR-0019 bounce trampoline (set bounce slot, return placeholder),
which does not require Tail convention and therefore does not affect this
analysis.

---

### Strategy C — Grow the RIR optimization pipeline

The highest-value improvements to JIT throughput are not in the
Cranelift lowering pass but in the RIR passes that precede it. Two
specific targets, in priority order:

1. **SSA-based unboxing** (`cs-opt` pass, informed by `cs-typer`
   inference): when `cs-typer` proves that a value is always a fixnum or
   always a flonum across a body, eliminate the NanBox-tagged `i64`
   round-trips in the RIR (`BoxTyped` / `AnyToFix` / `AnyToFlo`). Keep the
   fixnum in a register across the whole body, and emit a single tag-check
   guard at the function entry instead of one per use site. This is where
   the ~65× improvement on `fib` (pure-fixnum tier, raw-i64 ABI) can be
   approached on uniform-NB (correct ABI, proper IC dispatch).

2. **Escape analysis → stack/region allocation** (`cs-opt` pass consuming
   lifetime information from `cs-rir::lifetime`): when a `Cons`/`MakeClosure`
   allocation is proven to not escape the current JIT frame, lower it to a
   stack allocation (or a `cs_gc::Region` bump pointer) rather than a
   full GC heap allocation. This directly targets the allocation long-tail
   (`alloc-stress` ~3×, `nqueens` ~1.7× over VM) that persists after the
   call-handling fixes.

Guile's CPS-soup IR is a design reference for how a Scheme-specific IR can
carry enough information for both transformations without needing a separate
high-level IR phase.

---

## First landed increment (issue #47 fix, Strategy A + B step 1)

The concrete, already-implemented step that advances both A and B
simultaneously:

**File**: `crates/cs-jit-cranelift/src/lowering.rs`, `compile_uniform_nb`.

### What changed

**Eligibility prewalk amendment (Strategy A)**: the non-tail `CallSelf`
rejection now has an exception. Previously, any non-tail `CallSelf` caused
`compile_uniform_nb` to return `Err(Unsupported)`, routing the body through
the `rir_has_cross_function_call` guard to the VM with no JIT at all. The
amendment: when the body also contains at least one `Call` or `CallGeneral`
(`has_cross_call`), a non-tail `CallSelf` is admitted. The rationale is that
such "map-style" bodies (e.g. `(define (mp lst) (if (null? lst) '() (cons
(f (car lst)) (mp (cdr lst)))))`) cannot fall back to the legacy tier
(issue #19 miscompile) and recurse to *data* depth rather than the
exponential control depth of pure-arithmetic recursion like `tak`. Admitting
them to uniform-NB gives correct IC dispatch where before they had no JIT
coverage.

Pure non-tail self-recursion with no cross-function call (the `tak`/`fib`
shape) continues to be rejected by the prewalk and routes to the legacy
pure-fixnum tier — there is no performance regression on those benchmarks.

**Calling convention selection by need (Strategy B)**: after the prewalk
passes, a new helper `detect_uniform_nb_tail_self` is queried for every
block. `needs_tail_conv` is true iff at least one block ends in a
tail-position `CallSelf` (the condition that will emit `return_call`).
The inner body's calling convention is then:

```
let inner_conv = if needs_tail_conv { CallConv::Tail } else { CallConv::SystemV };
```

For the map-style bodies admitted by the prewalk amendment, no block ends
in a tail-position `CallSelf` (the self-call is an argument to `Cons`),
so `needs_tail_conv` is false and the inner picks `CallConv::SystemV`.
This gives the map-style body the same per-frame cost as the legacy tier
would have, matching (and in principle exceeding) its host-stack depth
ceiling. The per-frame cost for those bodies is also slightly *lower* than
it would be under Tail convention, because fewer registers are spilled on
each non-tail recursive call.

For bodies that do tail-recurse to themselves (and will emit `return_call`),
`needs_tail_conv` is true and the inner keeps `CallConv::Tail`, preserving
the tail-self-recursion optimization introduced in ADR 0019.

### Why this is the safe first step of Strategy A

The change moves map-style cross-function bodies onto uniform-NB without
retiring the legacy tier and without requiring uniform-NB to have closed the
performance gap on pure-arithmetic benchmarks. The `rir_has_cross_function_call`
guard in `cs-runtime/src/jit.rs` remains as protection for any body shapes
that uniform-NB still declines (e.g. `Inst` variants not yet handled by the
uniform-NB lowerer). The net effect is that JIT coverage expands — bodies
that previously ran in the VM now run JIT-compiled and correctly — without
any change to the legacy tier or to the bodies it currently handles.

### Tests added

`crates/cs-jit-cranelift/tests/jit_issue47_map_coverage.rs` (3 tests):

1. **`map_style_body_now_compiles_on_uniform_nb`**: a minimal map-style
   RIR body (non-tail `CallSelf` + `CallGeneral`) compiles on
   `compile_uniform_nb`. Pre-fix this returned `Err(Unsupported)`.

2. **`tail_self_with_cross_call_compiles_on_uniform_nb`**: a body whose
   self-call is tail-position (emits `return_call` under `CallConv::Tail`)
   and also has a tail-position `CallGeneral` (the ADR-0019 bounce path)
   compiles correctly on uniform-NB.

3. **`pure_nontail_self_still_rejected`**: a pure non-tail self-recursive
   body with no cross-function call is still rejected with
   `Err(Unsupported)`, asserting that `tak`-style recursion continues
   to route to the specialized tier.

## Consequences

* **Correctness** — map-style helpers (nboyer/sboyer term rewriters,
  `map`-like recursive helpers) now JIT on uniform-NB with correct IC
  dispatch. Previously they fell to the VM due to the `rir_has_cross_function_call`
  guard installed for issue #19.

* **Frame cost for non-tail bodies is slightly reduced** — selecting
  `CallConv::SystemV` for bodies without tail self-recursion means fewer
  caller-save register spills per non-tail recursive call. This is a minor
  performance improvement that also applies to any *new* bodies admitted to
  uniform-NB by future eligibility expansions.

* **Host-stack depth ceiling** — JIT-compiling non-tail recursion at all
  imposes a finite host-stack depth (each non-tail call leaves a native
  frame live). R6RS §3.5 requires only unbounded *tail* recursion;
  non-tail recursion running on a finite stack is conformant. This trade-off
  is already present for `tak` (routed to the legacy SystemV tier) and is
  accepted by the codebase. For map-style helpers, recursion depth is
  bounded by list/data depth, not by an integer argument, so
  pathologically deep lists could overflow where the VM's heap frames would
  not. `CallConv::SystemV` (chosen when there is no tail self-call) gives
  the maximum possible host-stack ceiling under current Cranelift ABIs,
  matching the legacy tier's ceiling.

* **Scope broadening** — the `has_cross_call` exception admits *all* bodies
  with a non-tail `CallSelf` and a cross-function call, not just `map`-shaped
  bodies. This is intentional: the invariant being enforced by the prewalk
  is that pure-arithmetic non-tail self-recursion without cross-calls stays
  on the legacy (SystemV, raw-i64) tier. Any body that also has a
  cross-function call must be on uniform-NB (the only tier with correct IC
  dispatch), so the broader admission is correct.

* **Open follow-up — pure-fixnum retirement (Strategy A completion)**:
  the legacy `compile_pure_fixnum` tier can be retired once uniform-NB
  matches or exceeds its performance on `fib`, `tak`, and `ack`. Until
  then, the two tiers coexist and the routing logic in
  `cs-runtime/src/jit.rs` remains authoritative. Tracked as issue #50.

* **Open follow-up — RIR optimization pipeline (Strategy C)**: SSA-based
  unboxing and escape analysis in `cs-opt`/`cs-typer` are the next
  high-value steps (tracked as issue #51); they are unblocked by having a
  single, correct backend to target.

## Alternatives considered

* **Admit all non-tail `CallSelf` unconditionally on uniform-NB** — would
  cover tak/fib as well as map-style bodies, but at the cost of a
  performance regression on pure-arithmetic benchmarks: uniform-NB's
  NanBox overhead is meaningful for tight fixnum loops. The `has_cross_call`
  condition precisely separates the two cases.

* **Lower non-tail `CallSelf` on the legacy tier when present alongside a
  cross-function call** — rejected because the legacy tier miscompiles
  cross-function calls (issue #19). This would silently return wrong results
  on nboyer/sboyer rather than falling to the VM.

* **Apply `CallConv::Tail` to all bodies** — simplest code, but imposes
  the Tail-conv spill overhead on every body even when `return_call` is
  never emitted. For map-style helpers the cost manifests as a lower
  host-stack ceiling for the very recursion pattern they embody.

* **Introduce a separate mid-tier IR** — rejected as premature: `cs-rir`
  already carries enough structure for SSA-based unboxing and escape
  analysis. A separate higher-level IR would add translation complexity
  without a corresponding benefit at this stage.

## References

* Cranelift `CallConv` documentation and tail-call ABI:
  <https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/tail-calls.md>
* Guile CPS-soup IR (a Scheme-specific SSA IR designed for unboxing and
  closure optimization): Andy Wingo, "A new concurrent parallel GC for
  Guile" / "CPS soup" blog series, 2015–2017.
* WebKit B3 (single optimizing IR for all tiers in a production JS engine):
  <https://webkit.org/docs/b3/>
* CPython copy-and-patch JIT with a single uop IR (PEP 744):
  <https://peps.python.org/pep-0744/>
