# Technology Stack — CrabScheme

## Project Type

CrabScheme is a **programming-language implementation**: a compiler, runtime, JIT,
standard library, and CLI/REPL distributed as a single Rust workspace. It produces
both a binary (`crabscheme`) and an embeddable library (`cs-runtime`) consumable by
other Rust projects.

## Core Technologies

### Primary Language

- **Rust**, edition 2024 (or latest stable at project start), MSRV pinned in
  `rust-toolchain.toml` and bumped deliberately.
- HolyJIT historically required nightly Rust due to compiler-plugin internals; the
  workspace therefore supports a `nightly` channel toggle. The non-JIT crates must
  build on stable.
- **Build system**: Cargo with workspace inheritance for dependency unification.
- **Package management**: Cargo + `cargo-deny` for license/advisory enforcement.

### Key Dependencies / Libraries

| Crate                        | Purpose                                                               |
| ---------------------------- | --------------------------------------------------------------------- |
| `holyjit`                    | Primary JIT backend (meta-JIT specializing the Rust runtime)          |
| `cranelift-codegen`          | Fallback / verification JIT backend                                   |
| `cranelift-jit`              | JIT module for Cranelift                                              |
| `cranelift-frontend`         | IR builder for Cranelift fallback path                                |
| `logos`                      | Lexer generator (datum-level tokens)                                  |
| `chumsky` *(or hand-rolled)* | Parser combinators (Scheme grammar is small enough for either)        |
| `rug` or `num-bigint`        | Arbitrary-precision integer/rational arithmetic for numeric tower     |
| `num-rational`               | Exact rationals                                                       |
| `num-complex`                | Complex numbers (R6RS requires complex support)                       |
| `unicode-normalization`      | NFC/NFKC for symbols and strings                                      |
| `unicode-segmentation`       | Grapheme cluster handling                                             |
| `gc` or custom               | GC; see "GC Strategy" below                                           |
| `clap`                       | CLI argument parsing                                                  |
| `rustyline`                  | REPL line editing, history, multi-line input                          |
| `miette` or `ariadne`        | Diagnostic rendering (rustc-quality error messages)                   |
| `insta`                      | Snapshot testing for parser/expander/IR outputs                       |
| `proptest`                   | Property-based testing of numeric, string, list operations            |
| `criterion`                  | Benchmark harness                                                     |
| `tracing` + `tracing-subscriber` | Structured runtime tracing                                       |
| `serde`                      | (De)serialization for cached IR and source maps                       |
| `bincode` / `postcard`       | Compact binary format for compiled IR cache                           |

Versions are pinned in `Cargo.toml` with caret bounds and a committed `Cargo.lock`.

### Application Architecture

CrabScheme follows a **classic compiler pipeline with a tiered execution backend**:

```
                            +-------------+
                            | Source .scm |
                            +------+------+
                                   |
                                   v
                              +---------+
                              |  Lexer  |   cs-lex
                              +----+----+
                                   |
                                   v
                            +-----------+
                            |  Reader   |   cs-parse  (S-exprs → datum tree)
                            +-----+-----+
                                  |
                                  v
                       +------------------+
                       | Macro Expander   |   cs-expand
                       | (syntax-case,    |
                       |  hygienic)       |
                       +---------+--------+
                                 |
                                 v
                         +---------------+
                         | Core AST →    |   cs-ir
                         | High-level IR |
                         +-------+-------+
                                 |
                                 v
                +----------------+----------------+
                |                |                |
                v                v                v
         +-----------+   +-------------+   +-----------+
         |Tree-walker|   | Bytecode VM |   |  Rust IR  |
         | cs-runtime|   |  (warm)     |   | (cs-rir)  |
         +-----------+   +-------------+   +-----+-----+
              ^                ^                 |
              |                |                 v
              |                |       +-------------------+
              |                |       | JIT abstraction   |
              |                |       | (trait JitBackend)|
              |                |       +----+---------+----+
              |                |            |         |
              |                |            v         v
              |                |   +----------+  +-----------+
              |                |   | HolyJIT  |  | Cranelift |
              |                |   +----+-----+  +-----+-----+
              |                |        |              |
              +----------------+--------+--------------+
                              |
                              v
                       +------------+
                       | GC + Value |   cs-core
                       | Runtime    |
                       +------------+
```

Each tier is a separate crate so it can be tested, benchmarked, and replaced in
isolation. The **JitBackend trait** is the seam that lets HolyJIT and Cranelift
coexist, and is the linchpin of risk mitigation for HolyJIT's experimental status.

### Data Storage

CrabScheme is not a stateful service, but it does persist artifacts:

- **Compiled IR cache**: `~/.cache/crabscheme/ir/<hash>.cir` — postcard-encoded core
  IR keyed by source content hash. Speeds up REPL warm starts and incremental compile.
- **Library FASL cache**: precompiled R6RS libraries cached as IR alongside source.
- **Bench history**: JSON in `bench/history.json` (committed for trend tracking).
- **No databases.** No external services required at runtime.

### External Integrations

None at runtime. CrabScheme is self-contained. Build-time integrations:

- **GitHub Actions** for CI (lint, test, conformance, bench, release).
- **GitHub Releases** for binary distribution.
- **crates.io** for library publication of `cs-runtime` and supporting crates.
- **GitHub Pages** for the conformance dashboard.

## Development Environment

### Build & Development Tools

- **Build system**: `cargo` workspace; `cargo xtask` for repo-local automation
  (running conformance suites, generating release artifacts, publishing dashboards).
- **Workflow**: `cargo watch -x test` for inner loop; `cargo nextest` for parallel
  test execution; `cargo insta review` for snapshot management.
- **REPL development**: `cargo run -p cs-cli -- repl` for interactive testing.

### Code Quality Tools

| Concern             | Tool                                                  |
| ------------------- | ----------------------------------------------------- |
| Static analysis     | `clippy` with `-D warnings` in CI                     |
| Formatting          | `rustfmt` with project-tuned `rustfmt.toml`           |
| License/advisories  | `cargo-deny`                                          |
| Unsafe-code review  | `cargo-geiger` reports tracked per-release            |
| Unit tests          | Built-in `cargo test`                                 |
| Property tests      | `proptest`                                            |
| Snapshot tests      | `insta`                                               |
| Conformance tests   | Custom harness in `crates/cs-test` driving R6RS suite |
| Differential tests  | Custom harness comparing all execution tiers          |
| Benchmarks          | `criterion`                                           |
| Coverage            | `cargo-llvm-cov`, target ≥ 80% line, ≥ 70% branch     |
| Fuzz testing        | `cargo-fuzz` on lexer, parser, reader, numeric ops    |
| Documentation       | `rustdoc` with `--deny warnings` in CI                |

### Version Control & Collaboration

- **VCS**: Git on GitHub.
- **Branching**: trunk-based with short-lived feature branches; release branches cut
  from trunk for stabilization (`release/0.x`).
- **Code review**: GitHub PRs, ≥ 1 approving review required, CI green required,
  squash-merge with linked ADR for architecturally significant changes.
- **ADRs**: in `docs/adr/NNNN-*.md`, numbered, immutable once merged.

## Deployment & Distribution

- **Targets**: Linux x86_64, Linux aarch64, macOS x86_64, macOS arm64, Windows x86_64.
  WASM (wasm32-wasi) is a stretch goal for M10+.
- **Distribution**:
  - GitHub Releases (static-linked binary tarballs, one per target triple)
  - crates.io (`cs-runtime`, `cs-cli` as `crabscheme` binary)
  - Homebrew tap (post-1.0)
  - Nix flake (in-tree `flake.nix`)
- **Installation requirements**: glibc ≥ 2.31 (or musl static), no other runtime deps.
- **Updates**: standard package-manager flows; no auto-update mechanism in the binary.

## Technical Requirements & Constraints

### Performance Requirements

| Operation                          | Target                                  |
| ---------------------------------- | --------------------------------------- |
| Cold start (`crabscheme -e '(+ 1 2)'`) | < 50 ms wall-clock                  |
| REPL prompt latency                | < 5 ms after startup                    |
| Tree-walker `(fib 30)`             | within 5× of CPython (sanity floor)     |
| Bytecode VM `(fib 30)`             | within 1.5× of CPython                  |
| JIT `(fib 30)`                     | within 1.2× of `gcc -O2` C equivalent   |
| Macro expansion of stdlib          | < 200 ms full cold                      |
| Embed-and-eval round trip          | < 200 µs for trivial expressions        |

### Compatibility Requirements

- **Platform support**: tier 1 = Linux x86_64/aarch64, macOS x86_64/arm64; tier 2 =
  Windows x86_64; tier 3 = WASM.
- **Rust MSRV**: pinned, bumped deliberately; non-JIT crates build on stable.
- **Standards compliance**:
  - **R6RS** (primary target — IEEE-style spec adherence).
  - **R7RS-small** compatibility shim.
  - **Unicode 15+** for string operations.
  - **IEEE 754** for inexact reals.
  - **SRFI** subset (1, 13, 14, 19, 27, 41, 69 prioritized).

### Security & Compliance

- **Threat model**: CrabScheme runs untrusted Scheme code only when explicitly
  invoked by the user. Sandboxing is **not** a default, but a future opt-in via a
  capability-restricted runtime constructor (`Runtime::sandboxed()`).
- **Supply chain**: `cargo-deny` enforces an allowlist of license types and bans
  yanked / advisory-flagged crates. Lockfile committed and audited per release.
- **Memory safety**: `unsafe` blocks justified inline with a `// SAFETY:` comment
  and reviewed individually. JIT-emitted code lives in W^X pages.
- **Compliance**: project does not handle PII; compliance is limited to OSS license
  cleanliness (Apache-2.0 / MIT dual-licensed).

### Scalability & Reliability

- CrabScheme is a single-process tool; "scaling" maps to handling large programs and
  long-running REPL sessions.
- **Memory**: GC must keep working-set bounded; pathological programs (deep
  recursion, large `quasiquote` trees) profiled and tested.
- **Long-running stability**: 24-hour fuzz run with no crashes is a release gate.
- **Reliability**: `panic!` in the runtime is a bug; user-facing errors are always
  Scheme conditions or structured `Result` types at the embed boundary.

## Technical Decisions & Rationale

### Decision Log

1. **Rust over OCaml/Haskell as host language.**
   *Rationale*: HolyJIT requires Rust; Rust gives us mature tooling, predictable
   performance, no GC by default (we control GC entirely), and a strong embedding
   story.
   *Alternatives considered*: OCaml (used by Larceny generation tooling and many
   academic Schemes) — rejected because HolyJIT integration would be impossible.

2. **HolyJIT-primary, Cranelift-fallback JIT strategy.**
   *Rationale*: HolyJIT's "annotate Rust functions, get JIT" model is exactly what we
   want for a meta-JIT specializing the interpreter to a program. But HolyJIT is
   experimental and has been mostly dormant; betting the project on it is reckless.
   Cranelift is mature, maintained, and has a clean JIT API. Both backends sit
   behind a `JitBackend` trait so we can ship with whichever works and switch
   freely. We will contribute fixes to HolyJIT as we encounter them.
   *Alternatives considered*: LLVM (heavy, hard to embed at runtime), libgccjit
   (license incompatibility), in-house JIT (years of work).

3. **Tiered execution: tree-walker → bytecode → JIT.**
   *Rationale*: A tree-walker is the easiest correctness baseline and ships
   immediately. A bytecode VM is the canonical warm-tier with mature implementation
   patterns. JIT specializes the hottest code. Each tier is a forcing function for
   the IR design.
   *Alternatives considered*: skip bytecode and go directly to JIT — rejected
   because warm-tier coverage of edge cases (continuations, dynamic-wind) is too
   risky to test only at the JIT layer.

4. **GC strategy: reference-counting bootstrap, then a precise tracing collector.**
   *Rationale*: `Rc<RefCell<…>>` for M0–M2 lets us focus on language correctness.
   Cycles are handled by an opt-in cycle collector during this phase. M5 introduces
   a precise mark-region or generational tracing collector with explicit roots
   tracked by the runtime. The `JitBackend` trait emits stackmaps so JITted code
   participates in tracing.
   *Alternatives considered*: BDW conservative GC (forbids precise stack scanning,
   problematic with JITted Rust), full Bacon–Rajan from day one (overkill for M0).

5. **"Transpile to Rust" interpreted as a Rust-flavored IR (cs-rir), not Rust
   source.**
   *Rationale*: Generating Rust source and shelling out to `rustc` at runtime is
   too slow for JIT (multi-second compile latency). cs-rir is a small,
   Rust-shaped SSA IR that both the HolyJIT backend (via direct lowering) and the
   Cranelift backend (via translation to clif) can consume. An **AOT mode** does
   exist that emits Rust source for `rustc` consumption — that path produces
   shippable native binaries from Scheme programs and is a separate use case.
   *Alternatives considered*: emit Rust source for both AOT and JIT — rejected on
   latency grounds.

6. **Hand-rolled lexer with `logos`, hand-rolled recursive-descent parser.**
   *Rationale*: Scheme's grammar is small enough that a parser generator is
   overhead. Hand-rolling gives us perfect control over source spans, error
   recovery, and reader extensibility (`#;` datum comments, `#!r6rs`, custom
   reader macros).
   *Alternatives considered*: `chumsky`, `lalrpop`. We may revisit if the parser
   proves a maintenance burden.

7. **Conformance-driven development.**
   *Rationale*: The R6RS test suites from Larceny and Racket are large and
   well-curated. Wiring them into CI from M1 onward turns conformance into an
   always-visible scoreboard rather than a post-hoc validation.
   *Alternatives considered*: writing our own tests from scratch — rejected as
   wasteful and lower-quality.

8. **Differential testing across execution tiers.**
   *Rationale*: A single Scheme expression run under the tree-walker, bytecode VM,
   and JIT must produce the same result. Property tests generate expressions and
   diff outputs; any divergence is a bug in one of the tiers. This catches
   optimization bugs that conformance tests miss.

9. **No global state in `cs-runtime`.**
   *Rationale*: Embedders need multiple isolated runtimes per process (think:
   plugin sandboxes, multi-tenant servers). `Runtime` is a constructed value;
   builtins, ports, and GC roots are per-runtime.

## Known Limitations

- **HolyJIT maturity risk.** HolyJIT may require non-trivial upstream work to
  function on modern Rust. Our `JitBackend` abstraction lets us ship on Cranelift
  alone if needed; HolyJIT then becomes an opt-in alternative backend.
  *Mitigation*: spike HolyJIT integration in M3 to surface blockers early; budget
  upstream contribution time.

- **Numeric tower performance.** Arbitrary-precision arithmetic via `rug`/`num` is
  slower than native fixnum/flonum paths. The runtime tags small integers as
  fixnums and only allocates bignums when needed; non-trivial arithmetic-heavy
  benchmarks may still be slower than e.g. Chez Scheme.
  *Mitigation*: aggressive fixnum specialization in the JIT; benchmark-driven
  tuning post-M8.

- **WASM target deferred.** WASM precludes most JIT options. WASM support is
  bytecode-VM-only and lands no earlier than M10.
  *Mitigation*: keep the bytecode VM tier independent of the JIT crates so it can
  compile to WASM unchanged.

- **First-class continuations are expensive.** `call/cc` requires capturing the
  full Scheme stack. We will use one-shot continuations where possible and a
  segmented stack for general continuations; performance of programs that abuse
  `call/cc` is explicitly not a goal beyond correctness.

- **Initial GC is reference counting.** Reference cycles in long-running programs
  may leak until the M5 tracing collector lands. The early phase is suitable for
  language conformance work, not production deployment.
