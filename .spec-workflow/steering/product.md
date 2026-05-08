# Product Overview — CrabScheme

## Product Purpose

**CrabScheme** is a complete, production-quality implementation of R6RS Scheme written in
Rust, with a multi-tier execution strategy that culminates in JIT compilation via
[HolyJIT](https://github.com/nbp/holyjit). It exists to give the Scheme community a
modern, embeddable, tooling-friendly implementation that does not compromise on
language conformance, performance, or developer experience — and to demonstrate that
HolyJIT's meta-JIT design is viable for a real, dynamic, garbage-collected language.

Crucially, CrabScheme is built from the bottom up to be **fully verified against the
R6RS specification, conformance test suites, and reference implementations** (Larceny,
Chez, Racket-R6RS) before any optimization work is undertaken. Correctness is the
load-bearing wall; speed is the trim.

## Target Users

1. **Scheme practitioners** who want a maintained, fast R6RS implementation that runs
   anywhere Rust runs (Linux, macOS, Windows, BSD, WASM) and can be embedded in larger
   Rust applications as a library.
2. **Language implementors and researchers** studying real-world meta-JIT design,
   hygienic macro expansion, and modern continuation handling. CrabScheme is designed
   to be readable and modifiable as a study artifact.
3. **Embedded scripting use cases** where R6RS's simplicity, formal grounding, and
   sandboxability are preferable to Lua or JavaScript.
4. **Educators** teaching compilers, type systems, and language design who want a
   Rust-native target their students can hack on.

## Key Features

1. **Full R6RS Conformance.** Numeric tower (exact/inexact, bignums, rationals,
   complex), hygienic macros (`syntax-rules`, `syntax-case`), records, libraries,
   conditions/exceptions, dynamic-wind, mandatory tail-call elimination, full
   first-class continuations (`call/cc`).
2. **R7RS-small compatibility layer** so existing R7RS code runs without churn.
3. **Tiered execution.** Tree-walking interpreter (correctness baseline) → bytecode
   VM (warm code) → HolyJIT-compiled native (hot code). Each tier is independently
   exercisable and observable.
4. **HolyJIT-powered runtime JIT** with a Cranelift fallback backend behind the same
   internal interface, so the JIT layer is swappable and verifiable.
5. **AOT transpilation path.** Scheme → Rust source code compilable by `rustc` for
   single-binary distribution of Scheme programs. The same IR feeds both JIT and AOT.
6. **CLI + REPL** with multi-line editing, history, completion, source-mapped error
   messages with rustc-quality diagnostics, and a built-in stepping debugger.
7. **Embeddable runtime** exposed as a Rust library crate (`cs-runtime`) with an
   ergonomic Rust ↔ Scheme value bridge.
8. **Conformance harness** that runs the Larceny R6RS test suite, the Racket R6RS
   tests, and a property-testing layer continuously in CI.

## Business Objectives

- Deliver a Scheme implementation that passes ≥99% of recognized R6RS conformance
  tests within 18 months of project start.
- Demonstrate HolyJIT viability with a non-trivial language as a forcing function for
  the HolyJIT project itself (and contribute fixes upstream).
- Produce documentation good enough that a competent Rust engineer with no Scheme
  background can understand the implementation end to end in under a week.
- Ship a single static binary (`crabscheme`) usable on Linux/macOS/Windows.

## Success Metrics

| Metric                                     | Target                                       |
| ------------------------------------------ | -------------------------------------------- |
| R6RS conformance test pass rate            | ≥ 99% by M9                                  |
| Larceny test suite pass rate               | ≥ 95% by M9                                  |
| JIT speedup over interpreter (gabriel)     | ≥ 10× geomean by M8                          |
| Cold-start REPL latency                    | < 50 ms                                      |
| Embedded `eval` from Rust round-trip       | < 200 µs for trivial expressions             |
| Lines-of-code per crate                    | ≤ 5,000 (forces modularity)                  |
| Property-test cases per release            | ≥ 10,000 cases on each numeric/string op    |
| Reproducible build (rustc + lockfile)      | byte-identical artifacts on Linux x86_64     |

## Product Principles

1. **Correctness first, performance second.** We pass the spec before we optimize.
   Every optimization must be guarded by a correctness test that fails without it.
2. **Layered backends, all switchable.** The same Scheme program must run identically
   under the tree-walker, the bytecode VM, and the JIT. Differential testing across
   tiers is a continuous CI gate.
3. **Pragmatic about JIT.** HolyJIT is the primary target, but its experimental
   status is acknowledged. The JIT layer is abstracted behind a trait so a Cranelift
   backend can carry the project if HolyJIT integration stalls.
4. **Verified at every layer.** Property tests, golden tests, conformance suites, and
   where applicable formal-reference cross-checks (e.g., comparing `(read)` outputs
   against Larceny on a corpus of real Scheme code).
5. **Diagnostics are a feature, not an afterthought.** Errors include source spans,
   suggestions, and an explanation link. Macro expansion preserves provenance.
6. **Embeddable and unsurprising.** No global state. The runtime is a value
   constructed by the host. Multiple isolated runtimes can coexist in one process.
7. **Documentation is code.** Every public item has rustdoc. Architecture decisions
   are recorded as ADRs in-tree.

## Monitoring & Visibility

CrabScheme is a CLI/library project; "monitoring" maps to developer-facing
introspection rather than dashboards.

- **CLI tracing**: `crabscheme --trace=expand,compile,jit run foo.scm` emits a
  structured event log of pipeline stages.
- **Conformance dashboard**: CI publishes a static HTML report of which R6RS tests
  pass/fail/skip per commit, hosted on the project's GitHub Pages.
- **JIT introspection**: `(jit-dump <proc>)` REPL primitive emits the IR,
  Rust-flavored IR, and final native disassembly for any compiled procedure.
- **Performance regression tracking**: a benchmark suite (Gabriel benchmarks +
  custom) runs nightly with results stored in `bench/history.json`.

## Future Vision

CrabScheme's R6RS core is the foundation; the long-horizon roadmap extends in three
directions without compromising spec conformance.

### Potential Enhancements

- **R7RS-large adoption**: implement R7RS-large libraries (red, tangerine, orange
  editions) as opt-in modules.
- **WASM target**: compile CrabScheme itself to WASM so Scheme programs run in
  browsers, with the JIT degrading to a portable bytecode VM.
- **Typed Scheme bridge**: optional gradual typing layer compatible with Typed
  Racket's type annotations, leveraging Rust's type system at compile time.
- **Distributed runtime**: serializable continuations over network boundaries
  (mirroring Termite Scheme), enabled by CrabScheme's first-class continuation
  representation.
- **Formal verification of core**: machine-checked semantics for the evaluation core
  in Coq or Lean, with extracted reference interpreter cross-checked against the
  Rust implementation in CI.
