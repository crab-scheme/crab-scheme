# R6RS++ Phase 4 sandboxing — L2 arc, exit report

> Status: **L2 (WASM-instance sandbox) complete. All required
> ADR-0015 iters shipped (1, 1.5, 4, 5, 6); iter 7 explicitly
> declined per iter-6 measurements; iter 8 (stretch) deferred.
> Sandboxing as a whole — L1 + L2 — is now substantively
> complete; only optional stretch items remain.**
> Branch: `r6rs-extensions`.
> ADR: `docs/adr/0015-sandboxing.md` (Accepted 2026-05-18).
> Predecessor: L1 status doc (`docs/milestones/r6rs-extensions-p4-sb-l1-status.md`).

## What L2 is

ADR 0015 §"Decision" layer 2 (L2): a real capability-based
isolation boundary built on wasmtime + WASI. The host crabscheme
spawns a wasmtime Instance of the no-default-features
`crabscheme.wasm` binary, hands it the source via a stdin/stdout-
adjacent text protocol, constrains its WASI capabilities (no
filesystem unless mapped, no network, configurable memory + fuel
limits). IS a security boundary; adversarial Scheme inside the
sandbox cannot read host memory, write host files, or spin
forever (fuel runs out).

## What shipped

### Iter 1 — cs-sandbox-wasm crate skeleton + wasmtime smoke
Commit: `bbfafc9`.

New `crates/cs-sandbox-wasm` with the full type surface:
- `SandboxConfig` — 9 fields per the ADR design
- Three named presets: `hygiene()` / `plugin()` / `adversarial()`
- `SandboxError` — 8 variants mirroring the wire-protocol
  failure kinds
- `SandboxInstance` — placeholder eval stub in iter 1
- `SandboxRuntime` (mod runtime) — `Engine` + per-config setup
  honoring `consume_fuel` + `epoch_interruption` + the memory
  limit via `StoreLimitsBuilder`
- `verify_wasmtime_integration` smoke helper that compiles an
  inline WAT `add` module — proves the Engine/Module/Linker/
  Store/Instance dance works against wasmtime 36.x

Wasmtime pinned to 36.x LTS per ADR Q1 resolution. 15 tests in
`iter1_smoke.rs`.

### Iter 1.5 — crabscheme.wasm protocol integration
Commit: `0cc3550`.

Wire the actual crabscheme.wasm binary through the sandbox
surface. End-to-end working: real wasmtime embedding + real
WASI capability constraints + real Scheme eval inside a
sandboxed instance of crabscheme.

- `SandboxRuntime` caches a compiled `Module` from
  `config.binary_path` (lazy; absent path = eval errors cleanly)
- `eval_via_protocol`: builds a fresh `WasiP1Ctx` per call with
  argv `["crabscheme", "--eval", expr_source]`, sets up
  `MemoryInputPipe` for stdin + two `MemoryOutputPipe` for
  stdout/stderr, preopens `allow_paths` as guest mounts
- Linker carries preview1 sync imports
  (`preview1::add_to_linker_sync`)
- Trap classification: wasmtime's `I32Exit(0)` = normal WASI
  exit (return stdout); `I32Exit(n>0)` = `GuestRaised`;
  fuel-text in trap = `FuelExhausted`; memory-text = `MemoryExhausted`

10 integration tests in `iter15_protocol.rs` covering arithmetic,
list construction, string round-trip, all 3 presets,
filesystem-isolation, fuel-exhaustion (infinite loop doesn't
hang), reset, and the clear "no binary configured" error.

### Iter 4 — Scheme builtins for sandbox
Commit: `c9f180d`.

`cs-runtime` gains a new `sandbox` feature gating
`cs-sandbox-wasm`. Four Scheme builtins:

- `(make-wasm-sandbox preset [binary-path])` — preset is a
  symbol (`'hygiene` / `'plugin` / `'adversarial`); binary-path
  optional (falls back to `CRABSCHEME_WASM_PATH` env)
- `(sandbox? v)` — predicate
- `(sandbox-eval s expr-source)` — runs the source string in
  the sandbox; returns the printed result as a Scheme string
- `(reset-sandbox! s)` — rebuilds the runtime

Implementation: thread-local registry keyed by `u32` id; Scheme
value is `#('__sandbox__ id)`. 11 tests in
`tests/phase4_sb_iter4_scheme.rs` (gated on
`#![cfg(feature = "sandbox")]`).

### Iter 5 — L1 inside L2 defense-in-depth
Commit: `14fb511`.

User's expression wrapped in `(eval 'USER (environment IMPORTS))`
before passing to the guest. Two-layer enforcement: L2 (WASI) is
the system boundary; L1 (namespace) is the lexical boundary. An
adversary has to break BOTH.

- `SandboxRuntime` carries `imports: Vec<String>` snapshotted
  at construction
- `build_l1_wrapped_eval(user, imports)` builds the wrap
- Default imports `["(rnrs base)"]` produces
  `(eval 'USER (environment '(rnrs base)))`

7 tests in `iter5_l1_inside_l2.rs` verifying: L1-allowed names
work; L1-restricted names fail (`hashtable?`, `for-all`);
composite imports unlock additional libs; the key
defense-in-depth claim (`open-input-file` blocked at L1
regardless of WASI capability); L1's `set!` immutability
propagates through L2.

### Iter 6 — RTT measurements + iter-7 decision
Commit: `ff992d3`.

Measured (release, Apple Silicon, wasmtime 36.x, hygiene preset,
2.5 MB crabscheme.wasm):

| Phase                          | Time      |
|--------------------------------|-----------|
| SandboxInstance::new compile   | ~1.0s     |
| First eval after construct     | ~1ms      |
| Warm eval RTT (subsequent)     | <1ms      |
| Per-instance setup avg         | ~0.9s     |

**Iter-7 decision: NO migration to component model.** Text
protocol RTT is sub-ms; component-model would replace argv +
stdout with WIT-typed calls but cannot reduce sub-ms RTT
meaningfully. The Module-compile cost (which IS the bottleneck)
isn't an ABI question — it's an instance-reuse optimization.

3 tests in `iter6_bench.rs` print numbers via `--nocapture` and
assert loose 30s ceilings against future regressions.

## What's deferred (residual)

### Iter 7 — component-model migration
**Explicitly declined per iter-6 measurements.** The ADR
specified iter 7 as conditional on iter 6 numbers; numbers say
no. If a future workload demands sub-ms per-eval RTT (currently
already there) or if the wasmtime LTS drops the preview1 API,
revisit.

### Iter 8 (stretch) — preloads + cross-eval continuations
Out of the loop scope. Tracked as residual:
- `SandboxConfig.preload: Vec<(name, source)>` — preloaded user
  libraries the guest sees alongside `(rnrs base)`
- Cross-eval continuation handles when `reuse_instance=true`
  (currently intra-eval only)
- `(drop-sandbox! s)` for explicit cleanup (currently
  thread-local leaks sandboxes for the program's lifetime)

### Engine + Module sharing across SandboxInstances
Not in any iter, but the obvious follow-up given iter-6
numbers. The ~1s per-sandbox compile cost dominates; a process-
wide compiled-module cache (keyed by binary-path content hash)
would amortize that across many sandboxes.

Plus `wasmtime::Module::serialize` for cached pre-compiled module
loading (skip Cranelift on subsequent runs across process
boundaries — useful for short-lived host processes).

## Test additions

| Suite                                         | New tests |
|-----------------------------------------------|-----------|
| cs-sandbox-wasm/tests/iter1_smoke.rs (1)      | 15        |
| cs-sandbox-wasm/tests/iter15_protocol.rs (1.5)| 10        |
| cs-sandbox-wasm/tests/iter5_l1_inside_l2.rs (5)|  7       |
| cs-sandbox-wasm/tests/iter6_bench.rs (6)      |  3        |
| cs-runtime/tests/phase4_sb_iter4_scheme.rs (4)| 11        |
| **Total L2**                                  | **46**    |

Plus 47 L1 tests (from L1.1–L1.4). **Sandboxing total: 93 new
tests.** All green. Workspace test sweep clean both with and
without `--features sandbox`.

## Cross-cutting

- **Binary build dependency.** L2 tests require
  `cargo build --release --target wasm32-wasip1 --no-default-features --bin crabscheme`
  to have produced `target/wasm32-wasip1/release/crabscheme.wasm`,
  or `CRABSCHEME_WASM_PATH` env override. Tests skip cleanly
  with an eprintln when neither path resolves.
- **Wasmtime 36.x LTS** is a sizable new dep (~30s clean build
  of wasmtime + wasmtime-wasi). Gated behind the `sandbox`
  feature in cs-runtime; default builds skip it.
- **No effect on ADR 0013 perf gates.** Sandboxing is opt-in;
  baseline JIT performance unchanged.

## Phase 4 deliverable status

After this arc, all 4 ADR-0015 deliverables that depend on L2
are present:

| Deliverable                  | Status |
|------------------------------|--------|
| Typed boundaries (typed-arc) | ✅      |
| Optimizer plugins            | ✅      |
| Sandboxing — L1 + L2         | ✅      |
| Custom readers (tracked #156)| ⬜     |

Phase 4 is now 3-of-4 done. Custom readers is the residual
Phase 4 deliverable; it's tracked as the post-1.0 follow-up to
the Phase 3C `#!lang` MVP.

## Cross-reference

- [[r6rs-extensions-p4-sb-l1-status]] — L1 arc status (sibling
  to this report)
- [[project_r6rspp_phase4_typed_arc]] — typed-boundaries arc
- [[project_r6rspp_phase4_optimizer_arc]] — optimizer-plugins
  arc (closed)
- ADR 0015 — the design this implements
