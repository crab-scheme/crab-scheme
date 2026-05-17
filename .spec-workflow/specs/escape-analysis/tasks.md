# Escape Analysis — Tasks

> Companion: `requirements.md`, `design.md`.
> Format mirrors the countable-memory / region-memory specs.

Depends on the `region-memory` spec being CLOSED (iters 1–3
shipped: `cs_gc::Region`, `Gc::new_in`, debug-mode validity).

7 iters, each a single commit per the per-iter commit policy.

---

## Iter 1 — `AllocEffect` + `EscapeKind` types

- [x] 1. Add `AllocEffect`, `EscapeKind` to cs-typer
  - File: `crates/cs-typer/src/effect.rs` (new),
    `crates/cs-typer/src/lib.rs`
  - Implement `AllocEffect { allocates, escapes, may_cycle }`
    and `EscapeKind { Local, Region, Heap, Unknown }` per
    design.md §"Component 1".
  - Implement `AllocEffect::PURE`, `AllocEffect::join`,
    `EscapeKind::join` (lattice).
  - Add `mod effect; pub use effect::{AllocEffect, EscapeKind};`
    to lib.rs.
  - Purpose: foundational types for effect inference.
  - _Leverage: existing cs-typer module structure (similar to
    Type)._
  - _Requirements: FR-1_
  - _Prompt: Role: Rust type-system author with PL-effect
    background | Task: Implement `AllocEffect` and `EscapeKind`
    in `crates/cs-typer/src/effect.rs` per design.md
    §"Component 1", with the lattice ordering
    `Local ⊑ Region ⊑ Unknown ⊑ Heap` | Restrictions: pure
    data types; no inferencer logic yet; const PURE; join
    operations associative + commutative + idempotent (assert
    via test) | Success: 8 unit tests in `effect.rs` covering
    PURE constant, lattice ordering, join idempotence, and
    several concrete combinations._

---

## Iter 2 — Per-primitive effect table

- [ ] 2. Add `EFFECT_TABLE` for built-in procedures
  - File: `crates/cs-typer/src/effect.rs` (extend)
  - Add a static map `EFFECT_TABLE: HashMap<&str, AllocEffect>`
    covering the ~80 primitives in cs-runtime's builtins:
    - `cons`, `make-vector`, `make-string`, `make-bytevector`,
      `make-hashtable`, `vector`, `list` → allocates,
      EscapeKind::Region (default), may_cycle=false.
    - `set-car!`, `set-cdr!`, `vector-set!`, `hashtable-set!`,
      `set!` → may_cycle=true, EscapeKind::Heap.
    - `car`, `cdr`, `vector-ref`, `string-ref`,
      `hashtable-ref` → PURE.
    - `+`, `-`, `*`, `<`, `=`, etc. (arithmetic, comparison) → PURE.
    - `apply`, `call/cc`, `dynamic-wind` → EscapeKind::Unknown
      (conservative).
  - Add a `pub fn primitive_effect(name: &str) -> AllocEffect`
    accessor.
  - Purpose: bootstrap the inferencer with known primitive
    effects.
  - _Leverage: cs-typer's existing primop type table
    (`crates/cs-typer/src/builtins.rs`) for the list of
    primitives to cover._
  - _Requirements: FR-1_
  - _Prompt: Role: Rust developer + Scheme primitive
    knowledge | Task: Add `EFFECT_TABLE` to
    `crates/cs-typer/src/effect.rs` covering the primitives
    enumerated in `crates/cs-typer/src/builtins.rs`. Each
    primitive gets a hand-curated `AllocEffect`. Add
    `primitive_effect(name)` accessor that returns
    `AllocEffect::PURE` for unknown primitives (safe default)
    | Restrictions: do not duplicate the builtins.rs type
    table; cross-reference for consistency; never mark
    something as Local that allocates (allocation implies at
    least Region) | Success: a unit test verifies the table
    covers at least the 30 most common primitives, and a
    spot-check (`primitive_effect("cons").allocates`,
    `primitive_effect("car").allocates == false`) returns
    expected values._

---

## Iter 3 — Effect inference for core expression shapes

- [ ] 3. Implement `infer_effect` for CoreExpr
  - File: `crates/cs-typer/src/effect.rs` (extend),
    `crates/cs-typer/src/infer.rs` (call site)
  - Add `pub fn infer_effect(expr: &CoreExpr, env: &TypeEnv) -> AllocEffect`.
  - Walk the AST bottom-up per design.md §"Component 2"
    rules. Each shape combines its sub-expressions' effects
    via `AllocEffect::join`.
  - For App: look up the function's effect (primitive_effect
    for built-ins; PURE default for user functions in iter 3 —
    inter-procedural is iter 5).
  - For Lambda: compute body effect; if escapes ≥ Region OR
    closure-over-env detected, propagate Heap.
  - Purpose: assign effects to every expression.
  - _Leverage: cs-typer's existing AST walk in
    `infer.rs`._
  - _Requirements: FR-1, FR-2, FR-7_
  - _Prompt: Role: Rust developer with type-system
    implementation experience | Task: Implement
    `infer_effect(expr, env) -> AllocEffect` per design.md
    §"Component 2" rules. The function does a bottom-up walk
    of CoreExpr; for each shape, derive the effect from
    sub-expressions and the shape itself. Letrec / Define
    propagate Heap escape; Lambda's effect depends on its
    body; App joins arg effects with the function's effect
    | Restrictions: do not recurse infinitely on letrec
    (use a visited set for Letrec resolution); for unknown
    user functions, default to AllocEffect with
    escapes=Unknown rather than PURE | Success: 30
    snapshot tests in `crates/cs-typer/tests/effects.rs`
    covering the rule table; each test inputs a Scheme
    expression and asserts the expected AllocEffect._

- [ ] 4. May-cycle detection
  - File: `crates/cs-typer/src/effect.rs` (extend)
  - For Letrec, Set, Define rules: when the RHS contains a
    Lambda that captures the LHS name, set `may_cycle = true`.
  - For `set-car!` / `set-cdr!` calls: when both args reduce
    to the same binding (per cs-typer's value-tracking), set
    `may_cycle = true`.
  - Purpose: identify cyclic bindings so layer 4 (tracing)
    knows when to trigger.
  - _Leverage: cs-typer's existing free-var analysis._
  - _Requirements: FR-7_
  - _Prompt: Role: Rust developer with PL theory background
    | Task: Extend `infer_effect` to detect potential
    cycles per the rules above, setting `may_cycle = true`
    | Restrictions: conservative — may_cycle can be
    over-approximated (false-positive is OK; false-negative
    is a correctness bug); don't trigger on simple Lambda
    without self-reference | Success: 5 may-cycle tests in
    `effects.rs` covering (define (f) f), (letrec ((g g)) g),
    mutual recursion via set!, (set-cdr! x x), and a control
    case (define (g) 1)._

---

## Iter 4 — `Lifetime` tag in cs-rir + `rir_bridge`

- [ ] 5. Add `Lifetime` to `cs-rir::Type`
  - File: `crates/cs-rir/src/types.rs`,
    `crates/cs-rir/src/lib.rs`
  - Add `Lifetime` enum per design.md §"Component 3".
  - Add `RegionTag(u32)` struct.
  - Modify `cs_rir::Type` (or add a parallel `TypedExpr`
    structure) to carry `lifetime: Lifetime`.
  - Default constructors set `lifetime = Lifetime::Rc`.
  - Purpose: thread lifetime down to the IR layer.
  - _Leverage: cs-rir's existing Type extensibility (it's
    been extended before)._
  - _Requirements: FR-2_
  - _Prompt: Role: Rust developer | Task: Add `Lifetime`
    enum + RegionTag struct to cs-rir; extend the Type
    representation to carry a Lifetime field; provide
    Default = Lifetime::Rc | Restrictions: keep cs-rir's
    public API backward-compatible (all existing
    construction sites default to Rc); add a Lifetime
    accessor on whichever Type variants benefit (Pair,
    Vector, Hashtable, Closure) | Success: cargo build -p
    cs-rir green; cargo test -p cs-rir 0 failures._

- [ ] 6. `rir_bridge` propagates effect → lifetime
  - File: `crates/cs-typer/src/rir_bridge.rs`
  - At each cs-typer Type → cs-rir Type lowering point,
    consult the AnnotationTable for the corresponding
    expression's AllocEffect; map to Lifetime per design.md
    §"Component 4" table.
  - Mint a new `RegionTag` per function scope that needs one
    (the inferencer notes the scope boundary).
  - Purpose: lower effects to lifetime tags in the IR.
  - _Leverage: existing rir_bridge structure._
  - _Requirements: FR-2_
  - _Prompt: Role: Rust developer with compiler lowering
    experience | Task: Extend `rir_bridge` to consult the
    AllocEffect annotation per expression and set the
    cs-rir Type's Lifetime field per the design.md mapping
    table | Restrictions: missing annotations default to
    Lifetime::Rc; never emit a Lifetime::Region without
    a valid RegionTag in scope | Success: `cargo test -p
    cs-typer` green; a new snapshot test in
    `crates/cs-typer/tests/rir_lifetime.rs` verifies the
    propagation for 5 representative expressions._

---

## Iter 5 — Region-scope stack in cs-runtime

**Note**: depends on `region-memory` spec iters 1–3.

- [ ] 7. Add `crates/cs-runtime/src/regions.rs`
  - File: `crates/cs-runtime/src/regions.rs` (new),
    `crates/cs-runtime/src/lib.rs`
  - Implement `REGION_STACK` thread-local + `RegionScope`
    + `current_region` per design.md §"Component 5".
  - Gate on `feature = "regions"` (forwarded from cs-gc).
  - Purpose: maintain per-thread region context for
    lifetime-aware allocation.
  - _Leverage: cs-gc's Region from region-memory spec._
  - _Requirements: FR-4_
  - _Prompt: Role: Rust developer | Task: Implement the
    region-scope stack per design.md §"Component 5". Use
    thread_local! for REGION_STACK; RegionScope is RAII
    | Restrictions: gate on `feature = "regions"`; the stack
    discipline is LIFO; cloning a Rc<Region> is OK because
    refcount keeps the region alive while any RegionScope
    holds it | Success: cargo build green; a unit test
    creates a region, enters a scope, asserts
    current_region returns it, exits, asserts current_region
    returns None._

---

## Iter 6 — Allocation dispatch wiring

- [ ] 8. Lifetime-aware allocation dispatch in cs-runtime
  - File: `crates/cs-runtime/src/builtins/mod.rs`
  - For each of `b_cons`, `b_make_vector`, `b_make_string`,
    `b_make_bytevector`, `b_make_hashtable`, `b_vector`,
    `b_list`: add a `*_dispatch` wrapper that takes an
    optional `Lifetime` (passed via thread-local or extra
    arg) and routes accordingly per design.md §"Component 6".
  - Keep the existing entry points (no lifetime arg)
    defaulting to `Lifetime::Rc` so existing callers don't
    change.
  - Purpose: enable lifetime-driven allocation in the walker
    tier.
  - _Leverage: region-memory's `Pair::new_in` from
    `cs_core`._
  - _Requirements: FR-3, FR-4_
  - _Prompt: Role: Rust developer doing a mechanical
    extension | Task: Add lifetime-aware dispatch wrappers
    for each allocation builtin per design.md §"Component 6"
    | Restrictions: do NOT change the existing entry
    points' behaviour; the dispatch wrapper is additive;
    on Lifetime::Region without a region in scope, return
    a clear error rather than panic | Success: cargo test
    --workspace --release 0 failures; a new integration
    test exercises a region-allocated cons via the dispatch
    wrapper._

- [ ] 9. cs-vm bytecode + opcode extensions
  - File: `crates/cs-vm/src/vm.rs`,
    `crates/cs-rir/src/inst.rs` (the bytecode opcode set)
  - Add new bytecode instructions per design.md
    §"Component 7": `AllocPairRegion`, `AllocPairTraced`,
    `OpenRegion`, `CloseRegion`, etc.
  - Update the cs-rir → cs-vm translator to emit these
    opcodes based on the RIR expression's Lifetime.
  - Update the VM dispatcher to route to the right helper
    (e.g., `vm_alloc_pair_region_gc` for `AllocPairRegion`).
  - Purpose: wire lifetime dispatch into the bytecode path.
  - _Leverage: existing AllocPair opcode + dispatcher._
  - _Requirements: FR-3_
  - _Prompt: Role: Rust developer with cs-vm bytecode
    familiarity | Task: Add new lifetime-aware bytecode
    opcodes per design.md §"Component 7"; update the
    bytecode→RIR translator to emit them; update the VM
    dispatcher | Restrictions: existing bytecodes stay
    backward-compatible; new opcodes are additive; the
    opcode encoding stays stable (no changes to existing
    opcode numbers) | Success: cargo test --workspace
    --release 0 failures; new integration test verifies
    a Scheme `(map f xs)` workload generates `AllocPairRegion`
    opcodes (inspected via the disassembler)._

- [ ] 10. cs-aot emit extensions
  - File: `crates/cs-aot/src/project.rs` (or whichever
    emits Rust)
  - Update emit_project to generate lifetime-aware
    allocation calls in the Rust output. For
    `Lifetime::Region`, emit
    `Pair::new_in(&current_region, car, cdr)`.
  - Purpose: AOT-emitted binaries dispatch allocations on
    lifetime.
  - _Leverage: cs-aot's existing emit pipeline._
  - _Requirements: FR-3_
  - _Prompt: Role: Rust developer with cs-aot experience
    | Task: Extend cs-aot's project emit to generate
    lifetime-aware allocation per design.md §"Component 8"
    | Restrictions: preserve existing AOT binaries'
    behaviour (default to Lifetime::Rc); new lifetime
    emits are additive | Success: cs-aot tests green;
    a manual `crabscheme aot foo.scm --build` on a
    map-heavy program produces faster execution than
    the baseline (NFR-3 partial check)._

---

## Iter 7 — ADR 0017 + exit report + spec close

- [ ] 11. ADR 0017 + exit report + spec close
  - File: `docs/adr/0017-escape-analysis.md` (new),
    `docs/milestones/escape-analysis-exit.md` (new),
    spec files status update.
  - Write ADR 0017 per requirements.md NFR-5:
    - Effect system shape (AllocEffect, EscapeKind, lattice).
    - Conservative-by-default approach.
    - Lifetime tag propagation through cs-rir.
    - Region-scope-stack mechanism.
    - Inter-procedural punt to per-call-site for v1.
  - Write exit report per M5 / countable-memory style.
  - Mark spec status CLOSED.
  - Purpose: lock layer 5 of the unified architecture into
    project history.
  - _Leverage: ADR 0015 / ADR 0016 for style._
  - _Requirements: NFR-5_
  - _Prompt: Role: Rust + documentation author | Task:
    Write ADR 0017 ratifying the escape-analysis design,
    exit report in M5 style covering iters 1–7 + perf
    numbers (NFR-1: ≤ 20% typer latency; NFR-3: ≥ 30%
    map speedup), mark spec CLOSED | Restrictions: do not
    delete iter-1-6 implementation; document the
    inter-procedural and stack-alloc deferrals clearly
    | Success: ADR landed; exit report includes the
    measurements; spec marked CLOSED in all three files._

---

## Sequencing summary

| Iter | Title | Depends on | Default-on? |
|------|-------|------------|-------------|
| 1 | AllocEffect + EscapeKind types | — | yes (additive) |
| 2 | Per-primitive effect table | 1 | yes (additive) |
| 3 | Effect inference for CoreExpr | 1, 2 | yes |
| 4 | Lifetime in cs-rir + rir_bridge | 3 | yes |
| 5 | Region-scope stack | 4, region-memory spec iter 3+ | feature-gated |
| 6 | Allocation dispatch wiring | 5 | feature-gated |
| 7 | ADR 0017 + exit | all | yes |

Iters 1–4 are pure type-system additions (no behaviour
change). Iters 5–6 enable allocation dispatch only when
both `regions` and the new `escape-analysis` features are on.
Iter 7 just ships docs.

## What this spec enables

After this spec:
- The cs-typer effect inferencer tells layer 3 (regions) when
  to allocate where.
- The cs-typer may-cycle detector tells layer 4 (tracing)
  when a cycle's possible.
- Every allocation site in cs-runtime / cs-vm / cs-aot
  dispatches on the inferred lifetime, automatically picking
  the cheapest safe mechanism.
- The five-layer architecture of ADR 0015 is fully active.
