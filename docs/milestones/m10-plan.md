# M10 Plan — AOT Compiler + WASM Target

> Status: **Open** as of 2026-05-16. Predecessor: M6 Phase 6 (`m6-phase6-complete`).
> Spec slugs: `aot`, `wasm` (two parallel tracks per the ROADMAP).
> Estimated duration: 3-6 months across both tracks.

## Why M10 exists

The ROADMAP describes M10 as two parallel stretch deliverables:

- **`aot`**: Scheme → Rust source → static binary. Zero-startup, LTO-able native distribution.
- **`wasm`**: CrabScheme on `wasm32-wasip1` (and eventually browser); bytecode-VM-only build (no JIT in WASM since runtime native codegen isn't allowed).

Both tracks expand the runtime's reach beyond "tool you run on a Unix-style machine with a JIT".

## Track scope

### Track W (`wasm`) — CrabScheme on WASM

**Deliverable:** `crabscheme run program.scm` runs under `wasmtime` (and eventually in a browser) producing correct results on the conformance test corpus.

**Design constraints:**
- WASM has no runtime native codegen → ship bytecode-VM only.
- WASM has no `dlopen` → drop `cs-ffi` (the dynamic-library loader).
- Stdlib + builtins recompile to WASM unchanged (pure Rust, no host syscalls beyond stdio).

**Iter plan:**

| Iter | Scope | Risk | Effort |
|------|-------|------|--------|
| W1   | Feature-flag scaffolding — make `cs-jit*` + `cs-ffi*` optional in `cs-runtime` and `cs-cli`. Verify native build with features ON/OFF still passes tests. | Low — cargo plumbing | 1-2 iters |
| W2   | `cargo build --target wasm32-wasip1 -p cs-cli --no-default-features --features wasm` produces a `.wasm` binary. | Medium — may surface unportable code (Box::into_raw casts, etc.) | 1-3 iters |
| W3   | `wasmtime crabscheme.wasm run program.scm` executes correctly on a corpus subset. | Medium — file I/O via WASI; stdout buffering | 1-2 iters |
| W4   | Conformance run on WASM — pass rate within 2pp of native per the ROADMAP exit gate. Closeout doc. | Low — measurement + bookkeeping | 1 iter |

**Exit criteria:** `wasmtime` runs CrabScheme programs correctly; conformance pass rate ≥ (native rate − 2pp).

### Track A (`aot`) — Scheme → Rust source → static binary

**Deliverable:** `crabscheme aot program.scm -o program` produces a self-contained executable that runs identically to JIT.

**Design constraints:**
- Reuse `cs-rir` — the same IR the JIT consumes. AOT emits Rust source instead of native code; the cs-runtime stays linked into the AOT'd binary.
- The AOT path should NOT require recompiling the runtime per program — the runtime is a fixed dependency.
- Generated Rust source must be `rustc`-clean (no warnings) so AOT'd binaries don't surface noise to end users.

**Iter plan:**

| Iter | Scope | Risk | Effort |
|------|-------|------|--------|
| A1   | `cs-aot` crate skeleton — given a `cs-rir::Function`, emit Rust source for that one function. Verify compile + behavior matches JIT on a few small RIR fixtures. | Medium — first design of source emission | 2-3 iters |
| A2   | Broader RIR coverage — handle the same `Inst` variants the JIT handles. Tier with the JIT lowering for parity. | Medium-high — many variants | 4-6 iters |
| A3   | Whole-program glue — Bytecode → RIR per lambda → Rust source per lambda + main entry → cargo project skeleton → `cargo build --release` → static binary. | High — cross-cutting cargo + linking + main-entry-point work | 3-4 iters |
| A4   | Closeout — a non-trivial Scheme program (e.g. the `fib` or `nqueens` benchmark) compiles to a static binary, runs identically, with bench numbers comparable to JIT. | Low — measurement | 1 iter |

**Exit criteria:** A non-trivial Scheme program compiles to a static binary that runs correctly. Bench numbers should be within 2× of JIT (likely faster post-rustc LTO).

## Recommended ordering — WASM first, then AOT

**Reasons to start with WASM:**

1. **Smaller scope.** WASM is a port (existing functionality, new target). AOT is new functionality (a new compiler path).
2. **Forces good factoring.** WASM exposes any hidden assumptions in the runtime (libc, dlopen, native codegen). Fixing those benefits both tracks.
3. **Cargo feature plumbing is dual-use.** W1's feature-flag work (making `cs-jit*` and `cs-ffi*` optional) is also useful for AOT (the AOT'd binary may want to disable JIT so it's purely Rust source).
4. **Lower up-front design cost.** WASM is mostly cargo config + portability fixes. AOT needs new architectural decisions (per-procedure vs whole-program; how Rust source maps to RIR; how generated code interfaces with cs-runtime).

After WASM closes (≥ W3), AOT iters can start in parallel or sequence.

## Out of scope for M10

- **JIT in WASM.** WASM doesn't support runtime native codegen. The ROADMAP explicitly excludes this.
- **Browser-side WASM (`wasm32-unknown-unknown`).** The ROADMAP scopes this for after WASI works. Browser will need different stdio handling (no WASI; needs `console.log` or DOM bindings).
- **AOT cross-compilation.** First iter targets the host triple only. Cross-compile lands later if motivated.
- **Generated-Rust performance optimization.** First-iter AOT emits straightforward Rust; relying on `rustc -O` + LTO for optimization. A future iter may add AOT-specific peephole optimizations.

## Tracking

Each track gets:
- A separate iter log committed to milestone docs (`docs/milestones/m10-trackW-*.md`, `docs/milestones/m10-trackA-*.md`).
- Per-iter commits with measurement attached.
- Exit summary at track close.

M10 as a whole gets:
- This plan doc.
- Exit doc at close (`docs/milestones/m10-exit.md`).
- Tag `m10-complete` when both tracks meet their exit criteria. Partial close (e.g. `m10-wasm-complete` if only WASM ships) is acceptable per the ROADMAP convention.

## Starting point: Track W iter 1

Track W iter 1 begins immediately:

1. Audit `cs-runtime` and `cs-cli` for `cs-jit*` / `cs-ffi*` dep references.
2. Add `[features]` to `cs-runtime`:
   - `jit` — enables `cs-jit*` deps and `Runtime::install_jit`.
   - `ffi` — enables `cs-ffi*` deps and the `Runtime::load_shared_library` path.
   - `default = ["jit", "ffi"]` — native builds unchanged.
3. Move `cs-jit*` / `cs-ffi*` references behind `#[cfg(feature = "...")]`.
4. Add `[features]` to `cs-cli` mirroring those.
5. Verify `cargo test --release` passes with default features.
6. Verify `cargo test --release -p cs-cli --no-default-features --features minimal` (where `minimal` is a new feature set excluding jit + ffi) passes.

Once W1 closes, W2 starts: actually attempt the WASM build.
