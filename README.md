# CrabScheme

R6RS-flavored Scheme implementation in Rust with four execution tiers
(tree-walker, bytecode VM, Cranelift JIT, Rust-source AOT) and a
`wasm32-wasip1` target.

```
$ crabscheme -e '(letrec ((fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))) (fib 25))'
75025
```

## Status

**1.0 Release Candidate 1** tagged at commit
`17632e7` (`git tag -l 1.0-*`). All ROADMAP milestones M0–M10 are
complete and tagged. See
[`docs/measurements/2026-05-16-1.0-rc-readiness.md`](docs/measurements/2026-05-16-1.0-rc-readiness.md)
for the full RC sign-off framing and gate-by-gate posture.

| Surface          | State                                                       |
|------------------|-------------------------------------------------------------|
| R6RS conformance | **100%** on the 117-fixture corpus (2,464 assertions)       |
| WASM conformance | **100%** — 0pp gap to native via `bench/wasm-conformance.sh`|
| Workspace tests  | 0 failures across 24 test executables                       |
| JIT perf gates   | All three ADR-0013 gates **MET** — ~11.6× walker geomean    |
| AOT pipeline     | fib(40) RawI64 ABI matches `rustc -O` to the centisecond    |

## What works

- **Lexer + reader + R6RS-flavored expander.** `syntax-rules` with
  hygienic binder renaming; `let`, `let*`, `letrec`, `do`, `case`,
  `cond`, `guard`, `quasiquote`, `define-record-type`.
- **Full numeric tower:** fixnum, bignum, rational, flonum, with
  auto-promote on overflow; NaN-boxed compact representation in the
  VM/JIT tiers.
- **Strings, characters, vectors, bytevectors, hashtables** (eq/eqv/
  equal); ports (string-in, string-out, file-in/out, binary,
  transcoded); promises, parameters, conditions.
- **Four execution tiers:**
  - Tree-walking interpreter with proper tail-call elimination.
  - Bytecode VM with stack machine + TCE + const-folded globals.
  - Cranelift JIT with type-feedback specialization, NB-typed slow
    paths, and inline caches. Beats Chez/Guile/Gambit-interp geomean.
  - AOT compiler: `cs_rir::Function` → Rust source → cargo project →
    standalone native binary. RawI64 ABI matches hand-written Rust;
    Nb ABI within 2× of `rustc -O` on fib post-RC2 fast-path inlining.
- **WASM target.** `cargo build --target wasm32-wasip1 -p cs-cli
  --no-default-features` produces a 2.2 MB `crabscheme.wasm` that runs
  under wasmtime. Conformance matches native to the byte.
- **First-class call/cc** on the VM tier (M8).
- **Rust FFI** with two flavors: trait-based (WASM-portable) and
  dynamic-library (`libloading`, native-only). See `crates/cs-ffi*`.
- **R6RS standard library foundation** (M9): `(rnrs)`, `(rnrs base)`,
  `(rnrs lists)`, `(rnrs sorting)`, `(rnrs hashtables)`, `(rnrs io
  ports)`, `(rnrs records)`, `(rnrs enums)`, plus prioritized SRFIs.

## Quickstart

```bash
# Evaluate an expression.
cargo run --release -- -e '(* 6 7)'

# Run a Scheme source file.
cargo run --release -- run examples/factorial.scm

# Pick a tier explicitly.
cargo run --release -- --tier walker run program.scm
cargo run --release -- --tier vm     run program.scm
cargo run --release -- --tier vm-jit run program.scm   # default

# Interactive REPL.
cargo run --release -- repl

# WASM build + run.
cargo build --target wasm32-wasip1 --release -p cs-cli --no-default-features
wasmtime run --dir=. target/wasm32-wasip1/release/crabscheme.wasm run program.scm
```

### REPL commands

| Command            | Effect                                          |
|--------------------|-------------------------------------------------|
| `:help`            | List commands                                   |
| `:quit`            | Exit (also `^D`)                                |
| `:tier walker\|vm\|vm-jit` | Switch execution tier                    |
| `:time <expr>`     | Evaluate and print wall-clock time              |
| `:load <path>`     | Load and run a Scheme file in this session      |
| `:reset`           | Reinitialize the runtime, dropping definitions  |

## Architecture

```
                ┌────────────────────────────┐
   source ──▶  │ cs-lex → cs-parse → cs-expand│
                └─────────────┬──────────────┘
                              │ CoreExpr
              ┌───────────────┼───────────────────┐
              ▼               ▼                   ▼
       cs-runtime          cs-vm           cs-rir + cs-jit-cranelift
       (tree-walker)    (bytecode VM)        (Cranelift JIT, native)
                              │
                              └──▶ cs-aot ─▶ Rust source ─▶ rustc ─▶ static binary
```

| Crate                 | Purpose                                                  |
|-----------------------|----------------------------------------------------------|
| `cs-core`             | Universal `Value`, `Symbol`, numeric tower, eq/eqv/equal |
| `cs-diag`             | Spans, source map, diagnostic rendering                  |
| `cs-lex` / `cs-parse` | Tokenizer + reader producing `Datum`                     |
| `cs-ir`               | `CoreExpr` — post-expansion AST                          |
| `cs-expand`           | R6RS `syntax-rules` macro expander                       |
| `cs-runtime`          | Tree-walking interpreter, environments, builtins         |
| `cs-vm`               | Stack-based bytecode VM with NB-typed values             |
| `cs-gc`               | Precise tracing GC (`Gc<T>` smart pointer)               |
| `cs-rir`              | RIR — the JIT/AOT-shared regional IR                     |
| `cs-jit`              | JIT trait + tier abstraction                             |
| `cs-jit-cranelift`    | Cranelift backend (native codegen)                       |
| `cs-aot`              | AOT compiler: RIR → Rust source → static binary          |
| `cs-ffi` / `cs-ffi-*` | Trait FFI + libloading FFI + macros + example plugins    |
| `cs-cli`              | `crabscheme` binary (REPL, `-e`, `run`)                  |

All tiers dispatch to the same `cs-core::Value`; the VM and JIT share
a NaN-boxed compact representation (`cs_vm::vm::NanboxValue`). AOT-NB
binaries link `cs-vm` for runtime helpers; AOT-RawI64 binaries are
fully self-contained.

## Testing

```bash
# Full workspace test suite (24 test executables).
cargo test --workspace --release

# Native conformance — 117 fixtures, 117/117 pass.
cargo test -p cs-cli --test conformance

# WASM conformance — 117 fixtures, 2,464/0/0 via wasmtime.
bash bench/wasm-conformance.sh

# AOT pipeline — factorial + fib compile to standalone binaries.
cargo test -p cs-aot

# Microbench cross-implementation perf table.
bash bench/microbench/run.sh
```

## Performance

### Microbench (median of 3, seconds)

```
benchmark        walker     vm        jit       chez      guile     gambit    rust-O
fib              0.752      0.022     0.008     0.040     0.054     0.044     0.008
tak              0.042      0.015     0.008     0.031     0.020     0.011     0.008
ack              0.091      0.020     0.008     0.032     0.019     0.016     0.008
nqueens          0.098      0.029     0.023     0.033     0.019     0.016     0.009
mandelbrot       0.297      0.070     0.015     0.036     0.031     0.038     0.008
spectral-norm    0.259      0.081     0.017     0.036     0.021     0.028     0.008
binary-trees     0.125      0.049     0.016     ERR       ERR       0.019     0.009
alloc-stress    0.116      0.033     0.020     0.032     0.019     0.017     0.012
```

CrabScheme JIT beats Chez (~2.6× geomean), Guile (~1.4×) and
Gambit-interp (~1.4×) across the suite. See
[`docs/adr/0013-perf-gate-reframe.md`](docs/adr/0013-perf-gate-reframe.md)
for the three ADR-0013 perf gates and `bench/microbench/run.sh` for
the harness.

### AOT (fib(40), best-of-3)

| Binary                          | fib(40) | Notes                                      |
|---------------------------------|--------:|--------------------------------------------|
| Reference `rustc -O fib.rs`     |  0.15 s | Hand-written Rust baseline                 |
| cs-aot **RawI64** ABI           |  0.14 s | Self-contained, no runtime dep             |
| cs-aot **Nb** ABI (post-RC2)    |  0.29 s | 1.93× of rustc-O; NB Fixnum fast-path inlined |

See
[`docs/measurements/2026-05-16-rc2-aot-nb-inline.md`](docs/measurements/2026-05-16-rc2-aot-nb-inline.md)
for the AOT NB inline-fast-path numbers.

## Documentation

- **[ROADMAP.md](ROADMAP.md)** — milestone plan + RC posture.
- **[docs/milestones/](docs/milestones/)** — per-milestone exit reports.
  Notable: `m6-phase6-exit.md` (Phase 6 JIT close), `m10-trackW-exit.md`
  (WASM ship), `m10-trackA-exit.md` (AOT ship).
- **[docs/adr/](docs/adr/)** — architecture decisions. Notable: ADR
  0013 (perf-gate reframe), ADR 0009 (HolyJIT parked).
- **[docs/measurements/](docs/measurements/)** — perf + conformance
  measurement snapshots.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms
or conditions.
