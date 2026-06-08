# CrabScheme

R6RS-flavored Scheme implementation in Rust with four execution tiers
(tree-walker, bytecode VM, Cranelift JIT, Rust-source AOT) and a
`wasm32-wasip1` target.

```
$ crabscheme -e '(letrec ((fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))) (fib 25))'
75025
```

## Status

**1.0 Release Candidate 7** on `main`. ROADMAP milestones M0–M10 all
tagged; R6RS++ Phases 1–4 closed (typed boundaries, optimizer plugins,
L1+L2 sandboxing, custom `#!lang` readers, syntax-case + SyntaxObject);
BEAM v1 closed (actors + hot reload + distributed transport); SDK
foundations M01 (effects) and M02 (cluster substrate) shipped; LSP +
MCP servers shipped for editor + AI-agent integration. Tagged
releases at
[releases](https://github.com/crab-scheme/crab-scheme/releases).

| Surface | State |
|---|---|
| R6RS conformance | **100%** on the 117-fixture corpus (2,464 assertions) |
| WASM conformance | **100%** — 0pp gap to native; both `wasm32-wasip1` and `wasm32-wasip2` build under CI |
| Workspace tests  | 0 failures across 40+ test executables |
| JIT perf gates   | All three ADR-0013 gates **MET** — 1.4–2.4× faster than Chez/Guile/Gambit on the microbench geomean |
| AOT pipeline     | `crabscheme aot prog.scm --build` → standalone native binary (level-3 toolchain-free via cranelift-object, level-1 cargo project) |
| Open issues      | 3 (all explicitly post-1.0 per [ADR 0034](docs/adr/0034-post-1.0-open-issue-landscape.md)) |

## What works

### Language

- **Lexer + reader + R6RS-flavored expander.** `syntax-rules`,
  `syntax-case` with full R6RS hygiene primitives (`bound-identifier=?`,
  `free-identifier=?`, mark-aware identifier comparison),
  `define-syntax-parser` (Racket-style combinators with `~or`,
  `~optional`, `~once`, ellipsis-head cardinality), `let`, `let*`,
  `letrec`, `do`, `case`, `cond`, `guard`, `quasiquote`,
  `define-record-type`, submodules, continuation marks (tail-safe).
- **Full numeric tower:** fixnum, bignum, rational, flonum, with
  auto-promote on overflow; NaN-boxed compact representation in the
  VM/JIT tiers.
- **Strings, characters, vectors, bytevectors, hashtables** (eq/eqv/
  equal); ports (string-in, string-out, file-in/out, binary,
  transcoded); promises, parameters, conditions.
- **Typed boundaries** — `define/typed` static type-check + automatic
  contract generation at library exports + intra-library elision
  (ADRs 0021–0025). See [`docs/user/types.md`](docs/user/types.md).
- **Custom `#!lang` readers** — define a language as a Scheme procedure
  that consumes a port and returns a datum (parse-time reader
  protocol, all 4 R6RS++ Phase 4 deliverables closed).

### Execution

- **Four execution tiers** sharing the `cs_rir` IR — tree-walker → VM
  bytecode → Cranelift JIT (uniform-NB ABI, type-feedback
  specialization, inline caches) → AOT (cranelift-object level-3
  toolchain-free, or level-1 cargo-project).
- **Optimizer plugins** — register Scheme-callable passes via
  `install-optimizer-pass!`; passes operate on shared `cs_rir`
  benefiting both JIT and AOT (ADR 0014). See
  [`docs/user/optimizer-plugins.md`](docs/user/optimizer-plugins.md).
- **Both ahead-of-time levels** — `crabscheme aot prog.scm --build`
  produces a native binary with no Rust toolchain required (level 3,
  cranelift-object → system `cc`) or via cargo (level 1). See
  [`docs/user/aot.md`](docs/user/aot.md).

### Concurrency, distribution, hot reload

- **BEAM-style actors** — `(spawn thunk)` / `(send pid msg)` /
  `(receive ...)`, supervision trees + worker pools in pure Scheme
  (`lib/beam/prelude.scm`).
- **Hot reload** — two-version code dispatch (`current` / `old` +
  `code-soft-purge!` / `code-purge!`), state-migration callbacks; the
  `beam_counter_migration` E2E exercises v1 → v2 with added fields.
- **Distributed transport** — `cs-net` (Sim, TCP, QUIC with mTLS) +
  `cs-distrib` (DistPid, Router, RemoteRef, DOWN propagation).
- **Consensus library** — full Raft (election / replication / commit /
  ReadIndex / snapshots / joint-membership) + EPaxos in pure Scheme
  on top of the transport substrate (`lib/consensus/`).
- **Reduction-tick preemption** — actors yield cooperatively even
  inside JIT-compiled hot loops (ADR 0031).
- See [`docs/user/actors.md`](docs/user/actors.md).

### Sandboxing

- **L1 — immutable environments** — `(environment '(rnrs base) ...)`
  returns a frozen binding snapshot; pair with `eval` to restrict the
  import set of untrusted code.
- **L2 — WASM-instance sandboxes** — `(make-wasm-sandbox)` spawns a
  real wasmtime instance hosting `crabscheme.wasm`; fuel + epoch +
  wall-clock limits; three threat-model presets (`hygiene` / `plugin` /
  `adversarial`). L1+L2 compose for defense-in-depth.
- See [`docs/user/sandboxing.md`](docs/user/sandboxing.md).

### Editor + AI-agent integration

- **LSP server** (cs-lsp) — diagnostics, symbols, hover, definition,
  references, completion, signature help, format, workspace-symbol,
  rename, semanticTokens. VS Code scaffold in `crabscheme-vscode/`.
- **MCP server** (crabscheme-mcp) — 7 tools (cs_diagnostics,
  cs_symbols, cs_definition, cs_references, cs_hover, cs_format,
  cs_workspace_symbols), validated against MCP 2025-06-18.
- Shared harness so CLI + LSP + MCP cannot drift in their reasoning.
- See [`docs/user/lsp.md`](docs/user/lsp.md) and
  [`docs/user/mcp.md`](docs/user/mcp.md).

### Standard library + portability

- **R6RS standard library** (M9): `(rnrs)`, `(rnrs base)`,
  `(rnrs lists)`, `(rnrs sorting)`, `(rnrs hashtables)`, `(rnrs io
  ports)`, `(rnrs records)`, `(rnrs enums)`, plus prioritized SRFIs.
- **26-module `(crab …)` stdlib** — path, fs, os, process, string,
  format, regex, time, random, uuid, json, csv, toml, url, hash,
  compress, deflate, archive, log, metrics, net, http, websocket,
  collection, math, tty, signal, meta, base. WASM-safe subset of 21
  modules built under `wasm-stdlib` for wasip1; `wasm-stdlib-full`
  adds net/http/websocket on wasip2.
- **`wasm32-wasip1`** — 2.2 MB `crabscheme.wasm` runs under wasmtime;
  conformance matches native to the byte.
- **`wasm32-wasip2`** — `wasi:sockets-0.2` + `wasi:http-0.2`
  incoming-handler scaffold; component-model-aware (ADR 0033).
- **Rust FFI** — trait-based (WASM-portable) + dynamic-library
  (`libloading`, native-only). See `crates/cs-ffi*` +
  [`docs/ffi-limitations.md`](docs/ffi-limitations.md).
- **First-class `call/cc`** on the walker + VM tiers.

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

# AOT — compile a Scheme source file to a standalone native binary.
echo '(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))' > fact.scm
crabscheme aot fact.scm --build
./fact-aot/target/release/fact 10        # → 3628800
```

See [`docs/user/aot.md`](docs/user/aot.md) for the AOT supported-
construct list, CLI flags (`--entry NAME`, `--emit-rir`,
`--emit-rust-source`), and the "how to extend" walkthrough.

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

### Microbench (median of 3, seconds — refreshed 2026-05-16 on Apple M-series)

```
benchmark        walker     vm        jit       chez      guile     gambit    rust-O
fib              0.438      0.025     0.011     0.037     0.043     0.039     0.010
tak              0.044      0.016     0.008     0.035     0.021     0.014     0.009
ack              0.093      0.021     0.010     0.036     0.021     0.017     0.009
nqueens          0.100      0.033     0.027     0.040     0.020     0.019     0.010
mandelbrot       0.360      0.088     0.043     0.055     0.044     0.049     0.013
spectral-norm    0.292      0.089     0.021     0.048     0.026     0.028     0.010
binary-trees     0.134      0.056     0.019     ERR       ERR       0.024     0.011
alloc-stress     0.120      0.033     0.020     0.034     0.019     0.017     0.011
```

CrabScheme JIT beats Chez on all 8 benches (geomean ~2.5×),
matches or beats Guile on 7 of 8, and matches Gambit-interp on
all of them. See
[`docs/adr/0013-perf-gate-reframe.md`](docs/adr/0013-perf-gate-reframe.md)
for the three ADR-0013 perf gates and `bench/microbench/run.sh` for
the harness.

### AOT (fib(40), best-of-3)

| Binary                          | fib(40) | Notes                                      |
|---------------------------------|--------:|--------------------------------------------|
| Reference `rustc -O fib.rs`     |  0.14 s | Hand-written Rust baseline                 |
| cs-aot **RawI64** ABI           |  0.14 s | Self-contained, no runtime dep             |
| cs-aot **Nb** ABI (post-RC2)    |  0.34 s | ~2.4× of rustc-O; NB Fixnum fast-path inlined |

See
[`docs/measurements/2026-05-16-rc2-aot-nb-inline.md`](docs/measurements/2026-05-16-rc2-aot-nb-inline.md)
for the AOT NB inline-fast-path numbers.

### AOT microbench coverage (post-RC2)

`bench/aot-comparison.sh` runs `crabscheme aot` on each microbench
.scm file and reports pass / fail / blocker per fixture:

| Bench           | AOT?         | Blocker if not                              |
|-----------------|--------------|---------------------------------------------|
| fib             | ✅ 0.03s @ N=35 | —                                        |
| ack             | ✅ <0.01s @ (3,6) | —                                      |
| tak             | ❌           | `EnvLookupAny` (multi-block let; iter O)    |
| nqueens         | ❌           | `MakeClosure` (nested lambdas; iter K)      |
| mandelbrot      | ❌           | `MakeClosure`                               |
| spectral-norm   | ❌           | demote edge case (iter P)                   |
| binary-trees    | ❌           | `MakeClosure`                               |
| alloc-stress    | ❌           | `MakeClosure`                               |

2 / 8 microbenches AOT cleanly today. The 6 that don't surface
the exact RIR `Inst` variant cs-aot doesn't yet handle. See
[`docs/measurements/2026-05-16-rc2-aot-coverage.md`](docs/measurements/2026-05-16-rc2-aot-coverage.md)
for the per-blocker iter map and
[`docs/user/aot.md`](docs/user/aot.md) for the supported-construct
list.

## Documentation

### User guides

- **[`docs/user/types.md`](docs/user/types.md)** — typed boundaries:
  `define/typed`, library auto-contracting, intra-library elision.
- **[`docs/user/aot.md`](docs/user/aot.md)** — AOT user guide
  (level 1 cargo, level 3 toolchain-free).
- **[`docs/user/cache-builtins.md`](docs/user/cache-builtins.md)** — native
  byte-cache / store builtins (group-commit `store-flush-wal`, RESP-bulk
  framing, fused `conn-serve-gets`) for high-throughput caches like crab-cache.
- **[`docs/user/lsp.md`](docs/user/lsp.md)** — LSP + headless CLI for
  editors and agents.
- **[`docs/user/mcp.md`](docs/user/mcp.md)** — MCP server for
  AI-agent integration (Claude / ChatGPT / any MCP client).
- **[`docs/user/actors.md`](docs/user/actors.md)** — BEAM-style
  actors, supervision, hot reload, distributed transport.
- **[`docs/user/optimizer-plugins.md`](docs/user/optimizer-plugins.md)**
  — register Scheme-callable passes operating on shared `cs_rir`.
- **[`docs/user/sandboxing.md`](docs/user/sandboxing.md)** — L1
  environment sandboxing + L2 WASM-instance sandboxing.

### Project history & internals

- **[ROADMAP.md](ROADMAP.md)** — milestone plan + RC posture.
- **[CONSTITUTION.md](CONSTITUTION.md)** — design philosophy
  ("Rust is the machine; Scheme is the logic").
- **[CONTRIBUTING.md](CONTRIBUTING.md)** — dev workflow, dev env,
  test discipline, file map.
- **[docs/milestones/](docs/milestones/)** — per-milestone exit
  reports. Notable: `m6-phase6-exit.md` (Phase 6 JIT close),
  `m10-trackW-exit.md` (WASM ship), `m10-trackA-exit.md` (AOT ship),
  `beam-v1-exit.md` (BEAM v1 ship), `stdlib-modules-exit.md` (26
  modules), `wasip2-networking-exit.md` (wasi:sockets + wasi:http).
- **[docs/adr/](docs/adr/)** — architecture decisions (0001–0034).
  Notable: ADR 0013 (perf-gate reframe), ADR 0014 (optimizer plugins),
  ADR 0015 (sandboxing L1+L2), ADR 0033 (wasip2 networking), ADR 0034
  (post-1.0 deferral landscape).
- **[docs/measurements/](docs/measurements/)** — perf + conformance
  measurement snapshots.
- **[docs/ffi-limitations.md](docs/ffi-limitations.md)** — FFI surface
  status and known gaps.

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
