# Escape Analysis — Design

> Status: **CLOSED** (2026-05-17). See
> `docs/milestones/escape-analysis-exit.md` and
> `docs/adr/0017-escape-analysis.md`.
> Companion: `requirements.md`, `tasks.md`.

## Overview

Extend cs-typer with effect inference that classifies each
expression's allocation behaviour (`AllocEffect` →
`EscapeKind` + `may_cycle`). Lower these effects to cs-rir as
a `Lifetime` tag on every typed expression's result. The
runtime (cs-runtime, cs-vm, cs-aot) dispatches allocation
constructors on the `Lifetime` tag — `Region(_)` allocations
go through `Gc::new_in` (from the `region-memory` spec),
`Traced` allocations go through the cfg-gated tracing path
(from the `tracing-revival` spec), and the default `Rc`
allocations go through `Gc::new`.

Start conservatively: everything is `Rc` unless the typer can
prove tighter. Tighten over time via benchmark-driven
improvements.

## Steering document alignment

### Technical standards (`steering/tech.md`)

cs-typer is described in `tech.md` §"Key Features" as an
optional/gradual type system. This spec extends its inference
output with effect annotations — purely additive on the
existing type-inference machinery.

The five-layer architecture from ADR 0015:

| Layer | Implementation | Status |
|---|---|---|
| 1: Ownership | Rust borrows | shipped |
| 2: RC | countable-memory | shipped |
| 3: Regions | `region-memory` spec | spec drafted |
| 4: Tracing | `tracing-revival` spec | spec drafted |
| **5: Analysis** | **this spec** | **drafted** |

### Project structure (`steering/structure.md`)

- cs-typer gains an `effect` module
  (`crates/cs-typer/src/effect.rs`).
- cs-rir's `Type` extends with `Lifetime`
  (`crates/cs-rir/src/types.rs`).
- cs-typer's `rir_bridge` propagates effect → lifetime
  (`crates/cs-typer/src/rir_bridge.rs`).
- cs-runtime gets a region-scope-stack
  (`crates/cs-runtime/src/regions.rs`, depends on the
  `region-memory` spec landing first).
- cs-runtime / cs-vm allocation sites gain lifetime-aware
  dispatch (existing `b_cons` etc. updated; new
  `b_cons_in_region`-style entry points).

## Code reuse analysis

### Existing components to leverage

- **`cs-typer::infer`**: the bottom-up type inferencer. Effect
  inference extends `infer` to also return an `AllocEffect`.
- **`cs-typer::check`**: top-down checker; carries effects
  forward through Lambda / Letrec / Let bindings.
- **`cs-typer::rir_bridge`**: lowers `cs-typer::Type` to
  `cs-rir::Type`. Extended to set the `Lifetime` field.
- **`cs-rir::Type`**: already a structured type with
  primitive / pointer / function variants. Adding a
  `Lifetime` field is a unary extension.
- **`cs-runtime::Pair::new` / `Vector::new` / etc.**: existing
  constructors. Extended with new `Pair::new_in(region, ...)`
  variants that match the `region-memory` spec's
  `Gc::new_in`.
- **`cs_gc::Region`**: provided by the `region-memory` spec
  (FR-1 there).

### Integration points

- **cs-cli** `crabscheme check`: already runs cs-typer.
  Extended to dump effect annotations alongside types when
  `--show-effects` is passed.
- **cs-aot**: uses cs-rir as IR input. After this spec, AOT-
  emitted code dispatches allocations on `Lifetime` tags. The
  emitted Cargo.toml still pins `cs-vm` etc.
- **cs-vm**: bytecode dispatch reads lifetime tags from the
  bytecode instructions (new opcodes
  `AllocPairRegion(reg_idx)` etc.) and routes accordingly.

## Architecture

### Modular design

- **`cs-typer::effect`**: new module owning `AllocEffect`,
  `EscapeKind`, `EffectInfer` (the inferencer). ~400 LOC.
- **`cs-rir::lifetime`**: small extension to RIR types.
  ~80 LOC.
- **`cs-typer::rir_bridge`** extension: ~50 LOC added.
- **`cs-runtime::regions`** (depends on region-memory spec):
  the region-scope-stack. ~80 LOC.
- **`cs-runtime::builtins/mod.rs`** / **`cs-vm::vm.rs`** /
  **`cs-aot`**: lifetime-aware dispatch wrappers. ~200 LOC
  across the workspace.

```mermaid
graph TD
    A[Scheme source] --> B[cs-expand → cs-ir]
    B --> C[cs-typer::infer]
    C --> D[AllocEffect per expression]
    D --> E[cs-typer::rir_bridge]
    E --> F[cs-rir::Type with Lifetime tag]
    F --> G{Compile target}
    G -- VM --> H[cs-vm bytecode with<br/>AllocPair{Region,Rc,Traced}]
    G -- AOT --> I[Rust source emitting<br/>Pair::new / new_in / new_traced]
    G -- Walker --> J[cs-runtime builtins<br/>dispatch on Lifetime]
    H --> K[Layer 3/2/4 allocator]
    I --> K
    J --> K
```

## Components and interfaces

### Component 1 — `AllocEffect` + `EscapeKind`

- **Purpose**: capture per-expression allocation behaviour.
- **Interfaces** (in `crates/cs-typer/src/effect.rs`):
  ```rust
  #[derive(Clone, Debug, PartialEq, Eq, Hash)]
  pub struct AllocEffect {
      pub allocates: bool,
      pub escapes: EscapeKind,
      pub may_cycle: bool,
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub enum EscapeKind {
      Local,
      Region,
      Heap,
      Unknown,
  }

  impl AllocEffect {
      pub const PURE: AllocEffect = AllocEffect {
          allocates: false,
          escapes: EscapeKind::Local,
          may_cycle: false,
      };

      pub fn join(self, other: AllocEffect) -> AllocEffect {
          AllocEffect {
              allocates: self.allocates || other.allocates,
              escapes: self.escapes.join(other.escapes),
              may_cycle: self.may_cycle || other.may_cycle,
          }
      }
  }

  impl EscapeKind {
      pub fn join(self, other: EscapeKind) -> EscapeKind {
          use EscapeKind::*;
          match (self, other) {
              (Heap, _) | (_, Heap) => Heap,
              (Unknown, _) | (_, Unknown) => Unknown,
              (Region, _) | (_, Region) => Region,
              (Local, Local) => Local,
          }
      }
  }
  ```
- The lattice ordering: `Local ⊑ Region ⊑ Unknown ⊑ Heap`.
  `Heap` is the most pessimistic; `Local` the most
  optimistic.
- **Dependencies**: none (pure data).

### Component 2 — Effect inference rules

- **Purpose**: derive `AllocEffect` per cs-ir expression
  shape.
- **Rules** (a representative subset):

  | Expr shape | Effect |
  |---|---|
  | Constant, Symbol, primitive ref | PURE |
  | `(cons a b)` where neither a nor b escape | { allocates, escapes=Region, may_cycle: false } |
  | `(cons a b)` where result is stored in a heap binding | { allocates, escapes=Heap, may_cycle: false } |
  | `(set-cdr! x v)` where v is reachable from x | { allocates: effect(v).allocates, may_cycle: true, escapes: effect(v).escapes } |
  | `(lambda ...)` whose closure escapes | { allocates, escapes=Heap, may_cycle if env-binding loop detected } |
  | `(let ([x e]) body)` | effect(e).join(effect(body)) — but the inner let env doesn't escape |
  | Call `(f args)` where f's known effect = ... | join(effects of args, f's effect) |

- **Algorithm**: bottom-up traversal of cs-ir AST. For each
  expression, compute `AllocEffect` from sub-expressions plus
  the expression's own behaviour. Walks the same shape as
  `cs-typer::infer`.

- **Inputs**: cs-ir AST, cs-typer's `TypeEnv`, knowledge of
  primitive effects (a static `EFFECT_TABLE` mapping built-in
  procedures to their effects).

- **Outputs**: `AnnotationTable` mapping each expression's
  span to its `AllocEffect`.

### Component 3 — `Lifetime` tag in cs-rir

- **Purpose**: carry the inferred effect down to the IR layer
  the VM / AOT consume.
- **Interfaces** (in `crates/cs-rir/src/types.rs`):
  ```rust
  #[derive(Clone, Debug, PartialEq, Eq, Hash)]
  pub enum Lifetime {
      Stack,
      Region(RegionTag),
      Rc,
      Traced,
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub struct RegionTag(pub u32);

  impl Lifetime {
      pub const fn default() -> Lifetime {
          Lifetime::Rc
      }
  }
  ```
- `RegionTag` is a per-function unique identifier for a
  region. Multiple expressions sharing a region get the same
  tag. Tags get materialized at IR-lowering time.

### Component 4 — `rir_bridge` effect propagation

- **Purpose**: translate `AllocEffect` to `Lifetime` and
  embed in the lowered RIR.
- **Mapping**:
  | AllocEffect | Lifetime |
  |---|---|
  | `escapes = Local` | `Stack` (if T is stack-friendly, else `Region(R0)`) |
  | `escapes = Region` AND `may_cycle = false` | `Region(R_n)` |
  | `escapes = Heap` AND `may_cycle = false` | `Rc` |
  | `may_cycle = true` (any escape) | `Traced` (if `tracing-revival` enables it) else `Rc` |
  | `escapes = Unknown` | `Rc` (conservative) |

### Component 5 — Region-scope stack in cs-runtime

- **Purpose**: maintain a per-thread "current region" for
  allocation dispatch.
- **Interfaces** (in `crates/cs-runtime/src/regions.rs`):
  ```rust
  thread_local! {
      static REGION_STACK: RefCell<Vec<Rc<cs_gc::Region>>> = RefCell::new(Vec::new());
  }

  pub struct RegionScope<'a> {
      _marker: PhantomData<&'a ()>,
  }

  impl<'a> RegionScope<'a> {
      pub fn enter(region: Rc<cs_gc::Region>) -> Self {
          REGION_STACK.with(|s| s.borrow_mut().push(region));
          RegionScope { _marker: PhantomData }
      }
  }

  impl<'a> Drop for RegionScope<'a> {
      fn drop(&mut self) {
          REGION_STACK.with(|s| { s.borrow_mut().pop(); });
      }
  }

  pub fn current_region() -> Option<Rc<cs_gc::Region>> {
      REGION_STACK.with(|s| s.borrow().last().cloned())
  }
  ```
- Walker eval enters a `RegionScope` for each expression whose
  IR-level `Lifetime` is `Region(_)`. VM dispatch does the
  same via opcodes `OpenRegion(tag)` / `CloseRegion(tag)`.

### Component 6 — Allocation dispatch in builtins

- **Purpose**: per-allocation site, pick the right
  constructor.
- **Interfaces** (in `crates/cs-runtime/src/builtins/mod.rs`):
  ```rust
  fn b_cons_dispatch(args: &[Value], lifetime: Lifetime) -> Result<Value, String> {
      if args.len() != 2 {
          return Err(arity_err("cons", "2", args.len()));
      }
      let car = args[0].clone();
      let cdr = args[1].clone();
      let pair_gc = match lifetime {
          Lifetime::Region(_) => {
              let region = crate::regions::current_region()
                  .ok_or_else(|| "cons: region tag without region scope".to_string())?;
              cs_core::Pair::new_in(&region, car, cdr)
          }
          Lifetime::Traced => /* tracing-revival spec */ cs_core::Pair::new(car, cdr),
          Lifetime::Stack => /* stack-alloc — future */ cs_core::Pair::new(car, cdr),
          Lifetime::Rc => cs_core::Pair::new(car, cdr),
      };
      Ok(Value::Pair(pair_gc))
  }
  ```
- Each existing builtin (`b_cons`, `b_make_vector`,
  `b_make_string`, etc.) gets a `*_dispatch` variant; the
  existing entry point is preserved (default to `Lifetime::Rc`).
- The cs-rir → cs-vm translator emits new opcodes:
  `AllocPair[lifetime]` instead of `AllocPair`. The VM
  dispatcher routes to the right helper.

### Component 7 — cs-vm bytecode extensions

- **Purpose**: encode lifetime info in the bytecode.
- **New opcodes**:
  ```rust
  pub enum Inst {
      // existing opcodes...
      AllocPairRc(Reg, Reg, Reg),
      AllocPairRegion(Reg, Reg, Reg, RegionTag),
      AllocPairTraced(Reg, Reg, Reg),
      OpenRegion(RegionTag),
      CloseRegion(RegionTag),
      // similar for AllocVector, AllocClosure, etc.
  }
  ```
- The cs-rir → cs-vm translator picks the opcode based on the
  RIR expression's `Lifetime`.

### Component 8 — AOT emit extension

- **Purpose**: AOT-emitted Rust code dispatches allocations
  on lifetime.
- **Mechanism**: cs-aot's project emitter generates Rust
  source that, for each allocation site, emits a match on the
  lifetime tag. The emitted source uses cs-runtime's region
  scope (or the future stack-alloc primitives).

## Data models

### `AllocEffect`

```text
AllocEffect {
    allocates: bool,        // 1 bit (1 byte after alignment)
    escapes: EscapeKind,    // 1 byte (enum)
    may_cycle: bool,        // 1 byte
}
// total 4 bytes per annotation
```

### `Lifetime`

```text
Lifetime {
    Stack,                  // tag-only
    Region(RegionTag),      // tag + u32
    Rc,                     // tag-only
    Traced,                 // tag-only
}
// 8 bytes max (Rc by default is 1 byte tag)
```

### `RegionTag`

```text
RegionTag(u32) — per-function unique id; minted at IR-lowering time.
```

## Error handling

### Error scenarios

1. **Region tag without region in scope.**
   - **Scenario**: VM dispatches `AllocPairRegion(_, _, _, tag)`
     but the region-scope stack is empty.
   - **Handling**: runtime error "no current region for tag X".
   - **User impact**: indicates a typer / IR-lowering bug; the
     `OpenRegion` was missing.

2. **Escape inference too aggressive.**
   - **Scenario**: a binding marked `EscapeKind::Region`
     actually escapes (e.g., gets stored in a longer-lived
     data structure via mutation the typer didn't see).
   - **Handling**: debug builds catch via the region-memory
     spec's region-validity check. Release builds: UB.
   - **Mitigation**: conservative default; tighten only with
     test coverage.

3. **`Traced` allocation without tracing feature.**
   - **Scenario**: a `Lifetime::Traced` allocation site runs
     under a runtime build without the `tracing-revival`
     spec's tracing-cycle-collector feature.
   - **Handling**: fall back to `Rc` (the tracing tag becomes
     equivalent to RC). Logged via a runtime warning.

## Testing strategy

### Unit testing

- `crates/cs-typer/tests/effects.rs` (new): 30+ snapshot
  tests covering:
  - Pure expressions (Constant, Ref) → PURE.
  - Simple `cons` not escaping → `EscapeKind::Region`.
  - `cons` returned from a function → `EscapeKind::Heap`.
  - `lambda` escapes via `define` → `may_cycle` if recursive.
  - `set!` / `set-car!` → conservative `EscapeKind::Heap`.

### Integration testing

- `crates/cs-runtime/tests/escape_dispatch.rs` (new):
  - region allocation actually fires on `(map f xs)` shapes.
  - `Lifetime::Rc` shapes don't use a region.
  - region-scope stack maintains LIFO discipline.

### Benchmarking

- `bench/escape_dispatch.rs` (new): 30%+ speedup on
  `(map f xs)` vs. all-Rc baseline (NFR-3).
- `bench/escape_analysis_time.rs` (new): ≤ 20% latency
  overhead on typer (NFR-1).

### End-to-end testing

- Conformance suite stays unchanged. Workspace 0 failures
  at every iter.

## Migration plan

The work is sequenced as 7 iters. Iter 1 lands the AllocEffect
type with default = PURE everywhere (no behaviour change).
Iters 2–6 progressively tighten the analysis. Iter 7 ships
ADR + exit report.

### Iter 1 — `AllocEffect` + `EscapeKind` types

Add types in `crates/cs-typer/src/effect.rs`; expose the
join lattice. Default inference returns PURE everywhere.

### Iter 2 — Per-primitive effect table

Populate `EFFECT_TABLE` with effects for built-in procedures
(`cons` allocates, `car`/`cdr` are pure, `set-car!`
mutates, etc.).

### Iter 3 — Effect inference for core expression shapes

Implement bottom-up effect inference in cs-typer for
Constant, Ref, Lambda, Let, If, App. Snapshot tests
verify expected effects for 30+ shapes.

### Iter 4 — `Lifetime` tag in cs-rir + `rir_bridge`

Add `Lifetime` to cs-rir; extend `rir_bridge` to set it from
the inferred effect. RIR-emitting code defaults to `Rc`.

### Iter 5 — Region-scope stack in cs-runtime

Add `crates/cs-runtime/src/regions.rs` with
`REGION_STACK` + `RegionScope` (depends on `region-memory`
iter 1–3 having shipped).

### Iter 6 — Allocation dispatch wiring

Modify `b_cons`, `b_make_vector`, etc. to dispatch on
lifetime. cs-vm gains new `AllocPair{Rc,Region,Traced}`
opcodes; cs-aot emit updated.

### Iter 7 — ADR 0017 + exit report

Write ADR 0017 + exit report. Spec marked CLOSED.

## File-level diff scope (estimate)

| Crate | LOC change |
|---|---|
| `cs-typer/src/effect.rs` (new) | +400 |
| `cs-typer/src/infer.rs` (effect propagation) | +150 |
| `cs-typer/src/check.rs` (effect threading) | +100 |
| `cs-typer/src/rir_bridge.rs` | +50 |
| `cs-rir/src/types.rs` (Lifetime tag) | +80 |
| `cs-runtime/src/regions.rs` (new) | +100 |
| `cs-runtime/src/builtins/mod.rs` (dispatch) | +150 |
| `cs-vm/src/vm.rs` (new opcodes + dispatch) | +200 |
| `cs-aot` (emit dispatch) | +80 |
| Tests + benches | +500 |
| `docs/adr/0017-escape-analysis.md` | +200 |

Net: ~+2010 LOC. Larger than countable-memory because layer 5
spans more crates.

## Open questions

1. **Inter-procedural analysis**: should `(define (f x) (cons x x))`'s
   effect propagate to all `(f y)` call sites? Per-function
   summaries (Tofte-Talpin style) are tractable; full
   whole-program escape is harder. v1 punts to per-call-site.
2. **Region polymorphism**: can `(define (map f xs) ...)`
   have an effect signature parameterized over the caller's
   region? Cyclone-style yes; we defer.
3. **Effect-driven JIT specialization**: should the JIT
   recompile hot functions when their effect summary
   changes? Defer.
4. **AOT lifetime tag verification**: should the AOT emitter
   include a runtime check that the lifetime tags match the
   actual escape pattern? Useful for the conservative-to-
   tight migration.

## Tasks

`tasks.md` covers iter-by-iter breakdown with file paths,
leverage hooks, prompt scaffolds, and exit criteria.
