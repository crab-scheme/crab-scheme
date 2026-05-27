# Changelog

All notable changes to CrabScheme are recorded here. The format is loosely
based on [Keep a Changelog](https://keepachangelog.com/).

Versioning tracks the release-candidate series toward 1.0. The Cargo workspace
version `1.0.0-rcN` corresponds to the git tag `1.0-rcN`, which drives the
release workflow (`.github/workflows/release.yml`). See
[`docs/RELEASING.md`](docs/RELEASING.md) for how a release candidate is cut.

## [1.0-rc7] — 2026-05-26

### Added
- **Distributed-actor SDK foundations.** `#:effects` declarations on
  `define`/`lambda`, inferred and checked by `crabscheme check` (#58); a
  cluster substrate — `cs-net` (Sim + TCP transports, framing) and
  `cs-distrib` (DistPid / Router / RemoteRef / handshake / DOWN) (#59);
  and a `cs-consensus` library — homegrown Raft (election / replication /
  ReadIndex / snapshots / joint membership) + EPaxos, over a persistent-map
  state machine (#64, #1).
- **`syntax-parse` combinators** for `define-syntax-parser`:
  `~or` / `~optional` / `~once` with ellipsis-head cardinality, via a
  backtracking matcher (#31); plus **expand-time error pinpointing** that
  blames the offending sub-form (#33).
- **`#!lang` custom-reader protocol** — parse-time reader dispatch, and
  honoring a language's `expander` (#71) and `base-env` (#70) exports
  (#69, #73, #75).
- **R6RS++ Phase 4 typed boundaries — closed** (#11): static
  `define/typed` checking, library-export auto-contracting, intra-library
  contract elision, and eta-elision verification (#76).
- **`#:literals` for `define-syntax-parser`** plus migration of all 27
  in-tree macros onto it (#32, #81), and **expand-time built-in
  syntax-class checks** (`id` / `number` / `string`) so definition-bodied
  macros (`define/contract`, `define/typed`, `define-record`) can validate
  their `name:id` argument — the body is emitted unwrapped (#82, ADR 0029).
- **scalar-replace non-escaping cons cells** — a default-on JIT
  optimization that eliminates directly-consumed transient pairs (#28,
  #79).
- **tail-safe continuation marks** — native frame-based marks; tail loops
  under `parameterize`-style marks now run in constant space (#36, #80).

### Changed
- Reader threads source spans through syntax data for sharper diagnostics
  (#72, #74).
- Project constitution doc (#63); CI moved to sccache + nextest (#65).
- Recorded 1.0-rc6 microbenchmark results (controlled rc5 A/B) (#78).

### Fixed
- **JIT silent miscompile (#83, ADR 0030).** An inner closure capturing a
  *loop-updated* variable of a tiered-up, loop-converted self-recursive
  procedure read that variable **frozen** at its value when the procedure
  crossed the tier-up threshold — returning silently-wrong results (integer
  and float) past ~1024 iterations. The loop back-edge now syncs updated
  loop vars into the frame env the closure captures.
- `cs-actor` `receive` made async cancel-safe (#62).
- Rebuilt the m5-fuzz harness for the cycle detector (#77).

## [1.0-rc6] — 2026-05-22

### Added
- **AOT level 3 — toolchain-free native codegen** (#57). `crabscheme aot
  --build` now produces a native binary on a host with only a C linker
  (`cc`) — no Rust toolchain. The JIT's Cranelift lowering is reused through
  a generic `Lowerer<M: Module>` to emit a relocatable object, linked against
  a new `cs-aot-rt` static archive (`libcs_aot_rt.a`, bundled in release
  tarballs). Auto-selected when cargo+rustc are absent (or
  `CRABSCHEME_AOT_FORCE_OBJECT=1`); `aot-doctor` self-tests both back-ends.
  Scope: a single self-contained (self-recursive) function.
- **AOT generic builtin dispatch** (#57). `crabscheme aot --multi --build`
  compiles programs using arbitrary stdlib builtins (strings, lists, I/O, …)
  to native binaries via `Inst::CallBuiltin` + `cs_runtime::aot_call_builtin`,
  not just numeric kernels. Adds `Const::String` through the pipeline.
- **AOT level 1 — installed-binary AOT** (#57). `build.rs` embeds the
  workspace sources (`bundled-aot-sources` feature) so a release-installed
  `crabscheme` can `aot --build` with no dev source tree.
- **Language Server Protocol server (`cs-lsp`)** (#55). Diagnostics, document
  symbols, hover, go-to-definition, find-references, highlight, completion,
  signature help, formatting, workspace symbols, rename, and semantic tokens;
  plus a headless JSON CLI and a `crabscheme-mcp` MCP server for coding
  agents, and a VS Code extension scaffold. Both server binaries ship in the
  release tarballs.

### Fixed
- Tier parity for higher-order builtins across walker / VM / JIT (#48, #56).
- gc-stats pause-time conformance assertions made tier-robust (#56).
- JIT cross-function map-style coverage (#47, #52); region/env
  use-after-free in cons-in-region under JIT (#51a, #54).

### Changed
- `cs-jit-cranelift`'s `Lowerer` is now generic over the Cranelift `Module`
  (`JITModule` for the in-process JIT, `ObjectModule` for AOT L3) —
  behavior-preserving for the JIT (jit_conformance + jit_differential green).

## [1.0-rc5] — 2026-05-20

The major feature wave.

### Added
- **R6RS++ extensions** (#12): `syntax-case` + full hygiene, contracts,
  records, `syntax-parse`, conditions, parameters, submodules, continuation
  marks, `#!lang` headers, typed boundaries, optimizer plugins, and
  sandboxing (L1 immutable environments + L2 WASM-instance).
- **BEAM-style runtime** (#2): `spawn`/`send`/`receive` actors, tables, and
  hot reload, with a Scheme supervision prelude.
- **Batteries-included stdlib** (#6, #7, #8): 26 `(crab …)` modules
  (path/fs/os/process/string/format/regex/json/csv/http/…), plus a WASM-safe
  subset.
- **`cs-web`**, a tower-style async web framework; **channels** (#25);
  **parallel runtime** (async actors + tiered memory management).
- **JIT**: proper tail calls (ADR 0019, #45), cross-function miscompile fix
  (#19, #46), speculative Fixnum unboxing, and uniform-NB as the sole tier
  (#50).
- Real-world benchmark suites; the SDK spec for distributed / durable /
  agentic CrabScheme.

## [1.0-rc4] — 2026-05-17

### Added
- Typer Phase 5 feeds param-type hints into AOT; AOT codegen optimizations:
  closure-elision, Fixnum fast paths, and direct-call elision for
  no-capture top-level functions.

## [1.0-rc3] — 2026-05-16

### Added
- AOT NB-correct lowering: all eight microbenchmarks AOT-compile and run
  correctly (flonum ops, `sqrt` on `Any`, `make-vector` NB length, symbols,
  truthiness).

## [1.0-rc2] — 2026-05-16

### Changed
- Early release-candidate stabilization on top of rc1.

## [1.0-rc1] — 2026-05-16

### Added
- First release candidate / prebuilt-binary release. Foundation milestones
  M0–M10: tree-walker + bytecode VM (M4: VM 2–3× the walker) + Cranelift JIT
  + the AOT pipeline; nan-boxing value representation; countable and region
  memory management; R7RS core conformance.

[1.0-rc7]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc7
[1.0-rc6]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc6
[1.0-rc5]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc5
[1.0-rc4]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc4
[1.0-rc3]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc3
[1.0-rc2]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc2
[1.0-rc1]: https://github.com/crab-scheme/crab-scheme/releases/tag/1.0-rc1
