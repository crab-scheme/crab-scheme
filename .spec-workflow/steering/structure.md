# Project Structure — CrabScheme

## Directory Organization

CrabScheme is a Cargo workspace. Each crate has a single, well-defined responsibility
and a public-API surface narrow enough to fit on one screen.

```
crabscheme/
├── Cargo.toml                  # Workspace root, dependency unification
├── Cargo.lock                  # Committed
├── rust-toolchain.toml         # Pinned MSRV / channel
├── rustfmt.toml                # Project formatting
├── deny.toml                   # cargo-deny config
├── flake.nix                   # Nix dev shell + build
├── README.md
├── LICENSE-APACHE
├── LICENSE-MIT
├── CHANGELOG.md
├── ROADMAP.md                  # Top-level milestones M0..M10
│
├── .spec-workflow/             # Spec-workflow steering + per-feature specs
│   ├── steering/               # product.md, tech.md, structure.md (this file)
│   └── specs/                  # foundation, expander, vm, jit, …
│
├── crates/
│   ├── cs-core/                # Value types, symbols, numeric tower
│   ├── cs-lex/                 # Lexer (logos) + token types + source spans
│   ├── cs-parse/               # Reader: tokens → datum tree
│   ├── cs-expand/              # Hygienic macro expander, core-form recognizer
│   ├── cs-ir/                  # Core IR (post-expansion AST → SSA-ish IR)
│   ├── cs-rir/                 # Rust-flavored backend IR consumed by JIT backends
│   ├── cs-jit/                 # JitBackend trait + dispatch logic
│   ├── cs-jit-holy/            # HolyJIT backend implementation (feature-gated)
│   ├── cs-jit-cranelift/       # Cranelift backend implementation
│   ├── cs-vm/                  # Bytecode VM (warm tier)
│   ├── cs-runtime/             # Runtime: GC, eval loop, builtins, embed API
│   ├── cs-stdlib/              # R6RS standard libraries (Scheme + Rust impls)
│   ├── cs-diag/                # Diagnostic rendering, source maps, error types
│   ├── cs-cli/                 # `crabscheme` binary
│   ├── cs-repl/                # REPL implementation (rustyline-based)
│   ├── cs-test/                # Conformance harness + differential testing
│   └── cs-aot/                 # AOT Scheme→Rust source emitter
│
├── stdlib/                     # Scheme source for R6RS standard libraries
│   ├── rnrs/
│   │   ├── base.scm
│   │   ├── lists.scm
│   │   ├── records.scm
│   │   ├── conditions.scm
│   │   └── …
│   └── srfi/
│       ├── 1.scm
│       └── …
│
├── tests/                      # Workspace-level integration tests
│   ├── conformance/            # Curated test vectors per spec section
│   ├── differential/           # Cross-tier differential test corpus
│   └── golden/                 # Snapshot inputs + expected outputs
│
├── bench/                      # Benchmarks driven by criterion
│   ├── gabriel/                # Gabriel benchmarks (boyer, dynamic, etc.)
│   ├── micro/                  # Targeted micros (call overhead, alloc, etc.)
│   └── history.json            # Committed perf history
│
├── docs/
│   ├── adr/                    # Architecture Decision Records (numbered, immutable)
│   ├── architecture.md         # Big-picture diagram + walkthrough
│   ├── jit-design.md           # JIT layer deep dive
│   ├── gc-design.md            # GC deep dive
│   ├── embedding.md            # Rust ↔ Scheme bridge guide
│   └── conformance.md          # Conformance methodology
│
├── examples/                   # Embedding examples (Rust crates that depend on cs-runtime)
│   ├── minimal-eval/
│   ├── sandboxed-runtime/
│   └── plugin-host/
│
├── xtask/                      # `cargo xtask`: project-local automation
│   └── src/
│       ├── conformance.rs
│       ├── publish.rs
│       └── bench_report.rs
│
├── third_party/                # Vendored R6RS test suites (license-permitting)
│   ├── larceny-tests/
│   └── racket-r6rs-tests/
│
└── .github/
    └── workflows/              # CI: lint, test, conformance, bench, release
```

## Naming Conventions

### Files

- **Rust source files**: `snake_case.rs`. One pub-facing logical unit per file
  where reasonable; private helpers live alongside.
- **Test files**: `tests/*.rs` for integration tests; `#[cfg(test)] mod tests` for
  in-module unit tests; `*_proptest.rs` for property tests.
- **Scheme source**: `kebab-case.scm` (idiomatic Scheme convention).
- **ADRs**: `docs/adr/NNNN-short-slug.md` where `NNNN` is zero-padded sequential.
- **Specs**: `.spec-workflow/specs/<spec-slug>/{requirements,design,tasks}.md`.

### Code

- **Crate names**: `cs-<purpose>` lowercase kebab-case (`cs-runtime`, `cs-jit-holy`).
- **Modules**: `snake_case`.
- **Types/Traits**: `PascalCase` (`Value`, `JitBackend`, `Expander`).
- **Functions/methods**: `snake_case`.
- **Constants/statics**: `SCREAMING_SNAKE_CASE`.
- **Lifetimes**: short, lowercase: `'a`, `'src`, `'arena`. Avoid bare `'_` in
  pub APIs; spell them out.
- **Generics**: single uppercase or short PascalCase: `T`, `B: JitBackend`.
- **Scheme-side identifiers**: kebab-case per Scheme convention.

## Import Patterns

### Import Order

Inside a Rust file, group imports with blank lines between groups, in this order:

1. `std`, `core`, `alloc` imports.
2. External crate imports (alphabetical).
3. Workspace-internal crate imports (alphabetical).
4. `crate::` and `super::` imports.

`rustfmt` is configured to enforce this grouping.

### Crate Dependency Direction

Dependencies flow strictly downward; no crate depends on a crate above it in this
list. The workspace-level CI enforces this with `cargo-modules` graph checks.

```
                cs-cli      cs-repl
                   \         /
                    cs-runtime
                   /    |    \
              cs-vm  cs-jit  cs-stdlib
                |      |
              cs-ir  cs-rir
                  \    |
                cs-expand
                   |
                cs-parse
                   |
                cs-lex
                   |
                cs-core ─── cs-diag
```

`cs-test` and `cs-aot` are tools, not on this dependency core; they may depend on
any of the above.

### Module/Package Organization

- **Workspace dependencies** are declared once in the root `Cargo.toml`'s
  `[workspace.dependencies]` and inherited by member crates with
  `dep = { workspace = true }`.
- **Feature flags** live on the leaf crates that need them (`cs-jit-holy` carries
  the `holyjit` feature; `cs-runtime` re-exports an aggregated `jit-holy` feature
  that turns on the dependency chain).
- **No `pub use`-driven facades** spanning crate boundaries. Every public item is
  exported from exactly one place.

## Code Structure Patterns

### Module Organization Inside a Crate

```rust
// crate-root: lib.rs or mod.rs
//   1. crate-level docs (`//!`)
//   2. `mod` declarations (alphabetical within visibility groups)
//   3. `pub use` re-exports (the crate's public surface)
//   4. crate-wide `pub` types if unavoidable (kept minimal)
//
// per-module file
//   1. module docs (`//!`)
//   2. `use` imports (grouped per "Import Order")
//   3. constants
//   4. type/struct/enum/trait declarations
//   5. impls (one block per type, methods grouped logically)
//   6. free functions
//   7. `#[cfg(test)] mod tests`
```

### Function Organization

```rust
fn name(args) -> Result<T, Error> {
    // 1. argument validation / preconditions (`anyhow::ensure!`-style or returns)
    // 2. early-exit / fast-path
    // 3. core logic (small, focused; extract when nesting > 3)
    // 4. single explicit return value
}
```

### File Organization Principles

- **One conceptual unit per file.** A file may contain a primary type plus its
  tightly coupled helpers; it must not contain two unrelated public types.
- **Public API at the top, internals below.** A reader scanning a file should see
  what's exported before how it's implemented.
- **`#[cfg(test)] mod tests` at the bottom** of each source file for unit tests
  that exercise private items.

## Code Organization Principles

1. **Single Responsibility.** Each crate, module, file, and function has one job.
   The crate dependency diagram is the authority on responsibilities.
2. **Modularity at the crate boundary.** Crates communicate through stable trait
   interfaces (`JitBackend`, `Reader`, `Expander`); replacing one crate's internals
   must not require touching another.
3. **Testability.** Pure transformations (lex, parse, expand, lower) are pure
   functions on owned input → owned output, trivially testable. Side-effecting
   layers (runtime, JIT, GC) expose hooks for in-test instrumentation.
4. **Consistency.** New code matches the patterns of nearby code; ADRs document
   any deliberate departures.
5. **No leaky abstractions.** A change to the macro expander must not require
   touching the JIT. A change to the JIT must not require touching the parser.
   The IR is the firewall.

## Module Boundaries

| Boundary                          | Direction     | Contract                                      |
| --------------------------------- | ------------- | --------------------------------------------- |
| `cs-lex` → `cs-parse`             | one-way       | `Token` stream + source spans                 |
| `cs-parse` → `cs-expand`          | one-way       | `Datum` tree                                  |
| `cs-expand` → `cs-ir`             | one-way       | `CoreExpr` (post-expansion AST)               |
| `cs-ir` → `cs-rir` / `cs-vm`      | one-way       | `Ir` (high-level SSA-ish)                     |
| `cs-rir` → `cs-jit`               | one-way       | `RustIr` consumed by any `JitBackend`         |
| `cs-jit` ↔ `cs-jit-holy`          | trait-impl    | `JitBackend` trait                            |
| `cs-jit` ↔ `cs-jit-cranelift`     | trait-impl    | `JitBackend` trait                            |
| `cs-runtime` → all execution tiers| orchestration | `Runtime` selects tier per call site          |
| `cs-runtime` → `cs-stdlib`        | data          | Stdlib loaded as Scheme source + Rust hooks   |
| `cs-cli`/`cs-repl` → `cs-runtime` | embed         | Public embed API (`Runtime`, `Value`, `eval`) |
| `cs-aot` → `cs-rir`               | consumer      | Reads cs-rir, emits Rust source               |

**Public vs internal**: every crate has a single `pub` surface; everything else is
`pub(crate)`. Workspace-internal items needed by another crate are exposed only by
adding to the public surface — there are no `pub(workspace)` shortcuts.

**Stable vs experimental**: experimental work lands behind a Cargo feature flag
(e.g. `experimental-tracing-jit`) until it stabilizes via ADR.

## Code Size Guidelines

These are guidelines, not rules. Reviewers flag deviations; ADRs document
deliberate exceptions.

- **Crate size**: ≤ 5,000 lines of Rust (excluding generated, tests). Triggers
  consideration of splitting.
- **File size**: ≤ 500 lines preferred, ≤ 1,000 hard ceiling.
- **Function size**: ≤ 60 lines preferred, ≤ 120 hard ceiling. Prefer extraction
  over nesting.
- **Cyclomatic complexity**: ≤ 10 per function (enforced via `clippy::cognitive_complexity` lint at `warn`).
- **Nesting depth**: ≤ 4 levels of `{}` inside a function body.
- **Public API surface per crate**: small enough that the rustdoc index page fits
  on one screen.

## Documentation Standards

- **Every public item** carries a rustdoc comment with at minimum: a one-line
  summary, an example (`# Examples`), and any panics/errors documented.
- **Every crate** has a crate-level `//!` doc explaining its purpose, position in
  the pipeline, and usage example.
- **Architecturally significant changes** require an ADR in `docs/adr/`. The
  template lives at `docs/adr/0000-template.md`.
- **Specs** live in `.spec-workflow/specs/` per the spec-workflow conventions
  (requirements.md, design.md, tasks.md).
- **In-line comments** explain *why*, not *what*. Comments referencing the R6RS
  spec cite the exact section number.
- **README files** for any sub-tree non-obvious to a newcomer (e.g.
  `crates/cs-jit/README.md` explains the JitBackend trait).
