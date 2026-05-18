# Escape Analysis — Requirements

> Status: **CLOSED** (2026-05-17, with cs-vm + cs-aot codegen
> deferred — see `docs/milestones/escape-analysis-exit.md`
> and `docs/adr/0017-escape-analysis.md`).
> Spec slug: `escape-analysis`
> Roadmap slot: Layer 5 of the unified memory management
> architecture (ADR 0015).
> Predecessor: `region-memory` spec (Layer 3 runtime).
> Companions: `countable-memory` spec (Layer 2 runtime,
> ADR 0014); `tracing-revival` spec (Layer 4 runtime).

This spec extends **cs-typer** with effect inference that
classifies every allocation site into one of four lifetime
categories — `Stack`, `Region(R)`, `Rc`, `Traced` — then
threads the classification through cs-rir so the runtime can
dispatch to the right allocation constructor without changing
call sites. This is the **conductor layer** that turns
CrabScheme's existing five mechanisms (ownership, RC, regions,
tracing, compiler analysis) into a unified system.

Without this spec, layer 3 (regions) is unused — users would
have to manually write `Gc::new_in(region, v)` everywhere, and
layer 4 (tracing) is a manual escape valve. With this spec,
the compiler picks the cheapest safe mechanism per allocation.

## Why escape analysis

The runtime mechanisms shipped or queued so far:
- **Ownership** (layer 1): used implicitly for Rust-internal
  state.
- **RC / `Gc<T>`** (layer 2): the default for all Scheme heap
  allocations.
- **Regions** (layer 3): available via `Gc::new_in` but only
  manually invoked.
- **Tracing** (layer 4): cfg-gated, no automatic trigger.

The right mechanism for a given allocation depends on
**lifetime** (bounded vs. unbounded) and **shape** (cyclic vs.
acyclic). The compiler can infer both from the program
structure when types are available.

Examples of what the analysis should achieve:

1. `(map f xs)` allocates an intermediate cons chain that
   doesn't escape `map`'s caller. Region-allocate.
2. `(let ([sym (string->symbol s)]) ...)` interns the symbol
   permanently. RC.
3. `(define (loop) (loop))` — the closure cycles through its
   env. Detect at type level; mark for traced reclamation OR
   refuse compilation if regions can't contain it.
4. `(vector 1 2 3)` immediately consumed inside the call.
   Stack-allocate (zero-cost) if the vector fits in a
   stack-allowable size.

These judgments require static type information — exactly what
cs-typer provides.

---

## Functional requirements

### FR-1. Effect annotation in cs-typer's Type system

Extend `cs-typer::Type` with an `effect` field that tracks per-
expression allocation behaviour:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AllocEffect {
    /// Does evaluation allocate heap memory?
    pub allocates: bool,
    /// May the allocation escape the current scope?
    pub escapes: EscapeKind,
    /// May the allocation form a cycle through this binding?
    pub may_cycle: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EscapeKind {
    /// Bounded to the current Rust stack frame.
    Local,
    /// Bounded to the current dynamic extent (e.g., let scope).
    Region,
    /// Escapes to a longer-lived holder (binding, return value).
    Heap,
    /// Conservative — unknown escape pattern.
    Unknown,
}
```

The inferencer determines these per-expression by walking the
typed IR and accumulating effects bottom-up.

**Acceptance**: `cs-typer::infer` reports the right
`AllocEffect` for at least 30 representative expression
shapes (sampled from the conformance suite); a snapshot test
in `crates/cs-typer/tests/effects.rs` codifies the expected
results.

### FR-2. Lifetime tag in cs-rir's Type

Extend `cs_rir::Type` with a lifetime tag:

```rust
pub enum Lifetime {
    /// Stack-allocatable; never escapes the producing call frame.
    Stack,
    /// Region-allocatable; lifetime bounded by a named region R.
    Region(RegionTag),
    /// Rc-managed; default if escape analysis can't prove tighter.
    Rc,
    /// Traced; only when cycles are statically certain.
    Traced,
}
```

`cs-rir::Type` becomes `(BaseType, Lifetime)`. The
`rir_bridge` module (cs-typer → cs-rir lowering) carries the
lifetime from the `AllocEffect` annotation.

**Acceptance**: `cs-rir::Type` has a `lifetime: Lifetime`
field accessor; every RIR type-construction path sets it
explicitly (default `Rc`); `rir_bridge` populates it from
the typer's effect inference.

### FR-3. Allocation dispatch in cs-runtime / cs-vm / cs-aot

At every Pair / Vector / Hashtable / etc. allocation site in
the runtime, dispatch on the lifetime tag:

```rust
fn alloc_pair(car: Value, cdr: Value, lifetime: Lifetime, region: Option<&Region>) -> Gc<Pair> {
    match lifetime {
        Lifetime::Stack => /* future: stack alloc */,
        Lifetime::Region(_) => {
            let r = region.expect("region tag without region available");
            Gc::new_in(r, Pair::raw_new(car, cdr))
        }
        Lifetime::Rc | Lifetime::Unknown => Pair::new(car, cdr),
        Lifetime::Traced => Gc::new_traced(Pair::raw_new(car, cdr)),
    }
}
```

The cs-rir → cs-vm and cs-rir → cs-aot translators emit
lifetime-aware allocation instructions.

**Acceptance**: a microbenchmark (`bench/escape_dispatch.rs`)
showing a `(map f xs)` workload where the intermediate cons
chain is region-allocated; perf improves over the all-Rc
baseline by ≥ 30% on a 10k-element list.

### FR-4. Region scope provision

For `Lifetime::Region(_)` to be useful, the runtime must have
a region in scope at the allocation site. The runtime
maintains a per-thread "current region" stack:

```rust
thread_local! {
    static REGION_STACK: RefCell<Vec<Rc<Region>>> = ...;
}

pub struct RegionScope<'a> {
    region: &'a Region,
    _guard: PhantomData<&'a ()>,
}

impl<'a> RegionScope<'a> {
    pub fn enter(region: &'a Region) -> Self {
        REGION_STACK.with(|s| s.borrow_mut().push(region));
        RegionScope { region, _guard: PhantomData }
    }
}

impl<'a> Drop for RegionScope<'a> {
    fn drop(&mut self) {
        REGION_STACK.with(|s| s.borrow_mut().pop());
    }
}

pub fn current_region() -> Option<Rc<Region>> {
    REGION_STACK.with(|s| s.borrow().last().cloned())
}
```

The typer-emitted RIR opens a region at the start of every
expression whose `AllocEffect` is `EscapeKind::Region`; closes
it at expression end.

**Acceptance**: a regression test showing that opening and
closing regions correctly bounds an allocation's lifetime.

### FR-5. Conservative-by-default analysis

The initial implementation marks **everything** as `Rc`
(matching today's behaviour). Only allocations the typer can
**prove** are non-escaping get region tags. As the analysis
matures, more expressions move from `Rc` to `Region(_)`.

The conservative default ensures correctness: a wrong tag
that's too tight (Region when it should be Rc) is a
correctness bug; a tag that's too loose (Rc when it could be
Region) is only a perf regression.

**Acceptance**: under the iter 1 / 2 implementation, all 117
conformance tests pass — meaning the conservative tagging
introduces no behaviour change. Subsequent iters tighten via
benchmark-driven progression.

### FR-6. Escape sites: bindings, returns, closures-over-env

Three escape sites need special handling:

1. **Bindings**: `(define x ...)` and `(set! x ...)` extend
   `x`'s lifetime to the enclosing frame's lifetime.
   `EscapeKind::Heap` for the RHS.
2. **Returns**: a function's return value escapes to the
   caller. If the caller's region is known (region
   polymorphism), the value can promote to that region.
   Otherwise `EscapeKind::Heap`.
3. **Closures over env**: a `(lambda (...) body)` captures
   `env`. If the closure escapes, `env` escapes. The
   transitive escape analysis propagates this.

**Acceptance**: a test suite (`crates/cs-typer/tests/escape_sites.rs`)
covers each case with at least 3 concrete Scheme programs and
verifies the inferencer assigns the expected `EscapeKind`.

### FR-7. May-cycle detection

The typer determines whether a binding could form a cycle:

- `(define f (lambda () (f)))`: `f`'s value contains a
  closure whose env references `f`. May-cycle = true.
- `(define x (cons 1 2))` then `(set-cdr! x x)`: `x` is
  mutated post-binding to form a cycle. The
  `set-cdr!` typer rule sees both args are the same binding
  and marks `x`'s effect with `may_cycle = true`.
- `(map f xs)`: no cycle possible.

The cycle annotation feeds the runtime's decision to use
`Lifetime::Rc` vs. `Lifetime::Traced` (the rare case where
tracing actually triggers).

**Acceptance**: 5 test cases exercising self-recursive
closures, letrec, mutual recursion via `set!`, and acyclic
controls; the inferencer reports the expected `may_cycle`.

### FR-8. Conformance + workspace tests stay green

All 117 cs-cli conformance tests and the full workspace test
suite stay 0-failure throughout the spec's iters. Each iter
that tightens the analysis must verify no test regresses.

**Acceptance**: `cargo test --workspace --release` returns
0 failures at every iter exit.

---

## Non-functional requirements

### NFR-1. Analysis pass latency

The escape analysis pass must add ≤ 20% to cs-typer's existing
inference latency. Measured via a benchmark over the
metacircular program (a non-trivial workload).

### NFR-2. Correctness over coverage

A conservative analysis (everything `Rc`) is acceptable as a
starting baseline. Coverage tightens over time. Wrong-direction
escape (Region when Rc would be correct) is a release-blocker
bug; wrong-direction conservatism (Rc when Region would be
correct) is a perf regression to track in a bench history.

### NFR-3. Per-allocation site cost

The runtime dispatch on `Lifetime` adds ≤ 1 cycle on the hot
path (single branch). Measured by comparing
`b_cons_with_lifetime_dispatch` to `b_cons_baseline`.

### NFR-4. WASM compatibility

The analysis pass runs at typecheck time; the WASM target's
runtime sees only the lifetime tags. WASM build stays green.

### NFR-5. ADR

A new ADR (`docs/adr/0017-escape-analysis.md`) ratifies:
- The effect system's shape (AllocEffect, EscapeKind).
- The conservative-by-default approach.
- The Lifetime tag's propagation through cs-rir.
- The region-scope-stack mechanism.

---

## Out of scope

| Item | Why excluded |
|---|---|
| Region polymorphism in the surface type system | Requires major cs-typer extension (region kinds, region variables). Defer to a successor spec. |
| Inter-procedural analysis | Per-function intra-procedural is the v1; whole-program analysis is a future iter. |
| Automatic tracing triggering | Belongs to `tracing-revival` spec; this spec only emits the `Lifetime::Traced` tag. |
| Stack allocation | The `Lifetime::Stack` tag is reserved but unused in v1; future iter wires it once the JIT supports stack-alloc'd Pairs. |
| Continuation-aware escape analysis | First-class continuations make escape analysis fundamentally harder; cs-vm's one-shot-by-default policy mitigates, but a fully sound analysis defers. |

---

## Risks

1. **Wrong escape inference orphans a value.** A binding marked
   `Region` when it actually escapes → use-after-region-drop.
   *Mitigation*: conservative default; debug-mode region-
   validity check from `region-memory` spec.

2. **Analysis fails to scale on large programs.** Whole-
   program traversal at typecheck time could slow compilation
   significantly.
   *Mitigation*: NFR-1 budget; per-function analysis bounds
   the work.

3. **Mutability changes lifetime mid-program.** `set!` /
   `set-car!` can re-bind a value into a longer-lived holder.
   *Mitigation*: the typer's `set!` rule conservatively
   marks mutated bindings as `EscapeKind::Heap` unless proven
   otherwise.

4. **Continuation re-entry breaks region scoping.** A captured
   `call/cc` continuation re-enters a region long after its
   official extent.
   *Mitigation*: one-shot continuations (cs-vm's default)
   don't re-enter. Multi-shot continuations promote their
   region to Rc at capture time.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `AllocEffect` + `EscapeKind` in cs-typer | `crates/cs-typer/src/types.rs` |
| `Lifetime` in cs-rir | `crates/cs-rir/src/types.rs` |
| `rir_bridge` propagates effect → lifetime | `crates/cs-typer/src/rir_bridge.rs` |
| Allocation dispatch (Pair / Vector / etc.) | `crates/cs-runtime/src/builtins/mod.rs`, `crates/cs-vm/src/vm.rs` |
| Region scope stack | `crates/cs-runtime/src/regions.rs` |
| 30+ snapshot test cases for `AllocEffect` | `crates/cs-typer/tests/effects.rs` |
| 5 may-cycle test cases | included in effects.rs |
| Conservative pass: 0 conformance regression | `cargo test --workspace --release` |
| Microbench: `(map f xs)` ≥ 30% faster | `bench/escape_dispatch.rs` |
| Analysis pass ≤ 20% latency overhead | `bench/escape_analysis_time.rs` |
| WASM build green | `cargo build --target wasm32-unknown-unknown` |
| ADR 0017 written | `docs/adr/0017-escape-analysis.md` |
