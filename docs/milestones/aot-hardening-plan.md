# AOT Hardening Plan — Post-1.0-rc2

> Status: **Open** as of 2026-05-16. Predecessor: 1.0-rc2 (`m10-aot-complete` + rc2 iters A-T).
> Estimated duration: 2-4 months across six phases.
> Spec slug: `aot-hardening` (extends M10 Track A).
>
> **Target outcome:** AOT graduates from "works for self-recursive numeric
> kernels" to "compiles arbitrary R6RS-foundation Scheme programs into
> standalone binaries that are observably equivalent to the VM/JIT
> tiers, distributable through standard package managers, and within
> 1.5× of `rustc -O` on the supported hot-path workloads."

## Why an "AOT hardening" phase exists

RC2 closed the AOT pipeline end-to-end: `crabscheme aot prog.scm
--build` produces a standalone binary. But the resulting binary is
useful only for the narrow program shape RC2 supports today: a
self-recursive numeric kernel with one `(define (f n) ...)`
extracted by `--entry`. Six concrete user-facing gaps make the
RC2 AOT path "demo-ware" rather than "production-ready":

1. **Distribution**: AOT'd projects depend on cs-vm by path = `<dev tree>/crates/cs-vm`. A release-installed `crabscheme` binary can't actually invoke AOT — the path resolves nowhere. Users who download a 1.0-rc1/rc2 release tarball can run `crabscheme aot ...` only if they ALSO have the source tree.

2. **Coverage**: 6 of 8 microbenches fail with specific blocker Insts (`MakeClosure`, `EnvLookupAny`, demote edge case). Real Scheme programs hit these constantly.

3. **Correctness**: cs-aot has its own unit tests but no differential coverage against the JIT or VM. A subtle codegen bug — e.g., a wrong sign-extension in the NB fast path — could produce silently-wrong results that conformance tests don't catch (because conformance doesn't run on AOT'd binaries).

4. **Diagnostics**: when AOT fails, the user sees `Inst::EnvLookupAny not yet supported (iter 1)`. That's accurate but not actionable — the user doesn't know which Scheme construct produced that Inst or how to rewrite their program.

5. **Performance**: AOT-NB is 2-3× slower than `rustc -O` on fib. The remaining gap is dynamic tag checks the JIT defers to a per-function type guard; AOT has no equivalent type-feedback channel.

6. **Hardening**: AOT'd binaries don't yet handle stack overflow, OOM, or shared symbol-table state cleanly. Cross-compilation isn't tested.

Each phase below targets one of those gaps with explicit exit criteria.

## Scope: six phases

```
Phase 1 — Distribution                  (~2 weeks)
        ↓
Phase 2 — Coverage                      (~6 weeks)
        ↓ ──── (Phase 3 can start in parallel after 2.1)
Phase 3 — Correctness                   (~4 weeks)
        ↓
Phase 4 — Diagnostics & UX              (~3 weeks)
        ↓
Phase 5 — Performance                   (~4 weeks)
        ↓
Phase 6 — Hardening                     (~3 weeks)
```

The sequencing reflects user impact: distribution unblocks adoption;
coverage unblocks real programs; correctness ensures the programs
work; diagnostics help users when they don't; performance is the
ongoing optimization arc; hardening handles the long tail of
edge cases.

## Phase 1 — Distribution

**Target gap:** AOT'd projects can't build from a release-installed
binary because `cs-vm = { path = "../cs-vm" }` doesn't resolve.

**Approach:** Publish the cs-* crates to crates.io with stable
APIs. Update the emitted Cargo.toml to depend on the published
versions. Maintain a `--dev-tree` flag for in-repo dev-loop use.

| Iter | Scope | Effort |
|------|-------|--------|
| 1.1 | Stabilize public APIs in cs-core, cs-diag, cs-rir, cs-vm, cs-aot. Identify unintentional public surface; mark internal items `pub(crate)`. | 1 week |
| 1.2 | Publish cs-core, cs-diag, cs-rir to crates.io as 0.1.0. These have no in-repo cyclic deps. | 2 days |
| 1.3 | Publish cs-vm as 0.1.0. Pins versions of cs-core/cs-diag/cs-rir. | 2 days |
| 1.4 | Publish cs-aot as 0.1.0. Pins cs-vm. | 1 day |
| 1.5 | `cs_aot::project::emit_project` now emits `cs-vm = "0.1"` instead of `path = ...`. Add a `cs_vm_path` override for dev-tree usage. | 2 days |
| 1.6 | CI test: extract a release tarball, install `crabscheme` to PATH, run `crabscheme aot foo.scm --build`, verify the produced binary works. | 2 days |
| 1.7 | Update `docs/user/aot.md` + README — "install from a release tarball" workflow. | 1 day |

**Exit criteria:**
- Anyone with curl + `rustup default stable` can:
  ```bash
  curl -fsSL https://github.com/crab-scheme/crab-scheme/releases/download/1.x/crabscheme-1.x-linux-x86_64.tar.gz | tar -xz
  ./crabscheme aot fact.scm --build
  ./fact-aot/target/release/fact 10
  # → 3628800
  ```
- No path = references in emitted Cargo.toml.

## Phase 2 — Coverage

**Target gap:** 6 / 8 microbenches fail. Real programs use closures,
free variables, multi-block lets.

**Approach:** Land the RC2 backlog's heavy iters K, U, W, plus a
clean fix for the multi-block demote edge case (O/P from RC2).

| Iter | Scope | Effort |
|------|-------|--------|
| 2.1 | **cs-vm Procedure-heap public API** — `pub fn alloc_procedure(...)` and `pub fn install_proc_table(...)` so AOT'd binaries can register their lambdas. | 1.5 weeks |
| 2.2 | **MakeClosure lowering** — for non-capturing closures, emit `vm_alloc_procedure_from_aot_fn(fn_ptr)`. Capturing closures defer to 2.4. | 1 week |
| 2.3 | **General `Call` / `CallGeneral` lowering** — read closure tag, dispatch through proc_table. | 1 week |
| 2.4 | **Closure capture via cs-vm Env public API** — `pub fn install_jit_caller_env(...)` so AOT can construct + install the env before invoking a closure. Captured locals live in the env. | 1 week |
| 2.5 | **Multi-block demote** — fix the iter O attempt. Per-block alias map; rejoin at branch points; bail on EnvSet in non-block-0. Unblocks tak-shape recursive programs. | 1 week |
| 2.6 | **spectral-norm demote edge case** — fix the "v17 used before defined" bug discovered in RC2 iter J. | 3 days |
| 2.7 | **Top-level program synthesis** — the bytecode at `bc.insts` (the top-level forms, not bc.lambdas[]) becomes a synthesized `__main__` lambda that runs side effects. `crabscheme aot prog.scm --build` works on full programs, not just `--entry name`. | 1 week |
| 2.8 | **Mutual-recursion support** — top-level defines that call each other. Requires the proc_table to be populated before any of them run; thread via a startup hook. | 1 week |

**Exit criteria:**
- 8 / 8 microbenches AOT cleanly via `bench/aot-comparison.sh`.
- A non-trivial `.scm` (e.g., `(let* ((a 1) (b (lambda (x) (+ x a)))) (display (b 5)))`) AOTs and prints correctly.
- The list of "what doesn't work" in `docs/user/aot.md` shrinks to documented post-1.0 work (FFI in AOT'd binaries, multi-shot call/cc, etc.).

## Phase 3 — Correctness

**Target gap:** No differential testing — AOT could be silently
wrong on inputs unit tests don't cover.

**Approach:** Treat the JIT (already well-tested) as the oracle.
For every program AOT can compile, AOT'd output must match JIT'd
output bytewise.

| Iter | Scope | Effort |
|------|-------|--------|
| 3.1 | **Differential test harness** — given a Scheme program + an input, run on (walker, VM, JIT, AOT) and assert all four outputs match. Cache builds. | 1 week |
| 3.2 | **Wire conformance fixtures through differential harness** — for each `tests/conformance/foundation/*.scm` that AOT can compile (single-define files with self-recursion), the AOT output must match the other tiers. | 1 week |
| 3.3 | **Proptest harness for AOT-compatible RIR** — generate random `cs_rir::Function`s within the supported Inst set; verify (a) cs-aot emits Rust source that parses, (b) the resulting binary returns the same as the JIT'd function. | 1 week |
| 3.4 | **Cross-platform smoke** — release workflow builds AOT'd fact.scm + fib.scm on each target and checks output matches. Catches platform-specific codegen bugs. | 3 days |
| 3.5 | **Nightly fuzz** — small-program fuzzer that mutates known-good Scheme programs and feeds them to AOT. Failures upload the input + the divergent output. | 4 days |

**Exit criteria:**
- 100% of AOT-compatible conformance fixtures match other tiers
  byte-for-byte.
- Proptest harness runs 10k cases on every PR via CI; nightly
  fuzz job has accumulated >100 hours of fuzz time with 0 failed
  cases (failures get filed as bugs and fixed).

## Phase 4 — Diagnostics & UX

**Target gap:** When AOT fails, users see internal Inst variant
names rather than actionable next steps.

| Iter | Scope | Effort |
|------|-------|--------|
| 4.1 | **Inst → user-meaningful description table** — each `UnsupportedInst("X")` maps to "your program uses Y (Scheme construct)"; link to the relevant section of `docs/user/aot.md`. | 4 days |
| 4.2 | **Source-span propagation through AOT** — `cs_rir::Function` already carries spans; thread them through `emit_project`'s diagnostics so "Inst::MakeClosure" surfaces with the `(lambda ...)` source location that produced it. | 1 week |
| 4.3 | **--explain CLI flag** — `crabscheme aot --explain prog.scm` runs the pipeline up to RIR translation, then prints a human-readable summary: how many top-level defines, what each one will lower to, what's supported, what's not. Doesn't emit a project. | 4 days |
| 4.4 | **Workaround suggestions** — for common failures, suggest an equivalent that does compile (e.g., "rewrite `(map f xs)` as a `(define (apply-map f xs) ...)` that takes f as a parameter, then AOT apply-map separately"). | 3 days |
| 4.5 | **`crabscheme aot --doctor`** — diagnostic command that runs each supported-Inst test program and reports OK / NOT INSTALLED / WRONG OUTPUT. Useful for verifying a release-installed binary works on the user's platform. | 3 days |
| 4.6 | **Cargo-build progress streaming** — instead of a multi-second silence while cargo compiles cs-vm, surface cargo's progress output so the user knows it's working. | 2 days |

**Exit criteria:**
- Every `UnsupportedInst` error includes a doc link + workaround suggestion.
- Source spans reach the user (`crabscheme aot prog.scm` shows "line 12: this `(lambda ...)` produces MakeClosure").
- `--doctor` exits 0 on a freshly-installed crabscheme and lists what works.

## Phase 5 — Performance

**Target gap:** AOT-NB is 2-3× slower than `rustc -O` on fib due
to dynamic tag checks the JIT defers to per-function type guards.

| Iter | Scope | Effort |
|------|-------|--------|
| 5.1 | **Type-feedback profile collection** — run the program once on the VM tier with profiling enabled; emit a `.aot-profile.json` per Function with observed operand types. | 1 week |
| 5.2 | **AOT consumes profile** — when both operands of an arith op are observed-Fixnum in the profile, emit `wrapping_add` directly (skip the tag check + helper). | 1 week |
| 5.3 | **Profile-guided NB → RawI64 promotion** — if every operand AND result of a function is observed-Fixnum, emit the function under RawI64 ABI (the iter-G self-contained mode). Caller must encode/decode at the boundary. | 1.5 weeks |
| 5.4 | **Inline more runtime helpers** — `vm_pair_p`, `vm_null_p`, etc. are short enough to inline in the emitted source rather than call. Mirrors what `rustc -O` would do across crate boundaries with LTO. | 4 days |
| 5.5 | **AOT-specific bench panel** — extend `bench/microbench/run.sh` with an AOT column for the benches AOT can compile. Track AOT performance over time. | 3 days |
| 5.6 | **LTO + opt-level=3 in emitted Cargo.toml** — current emission opts out; flip on. | 1 day |

**Exit criteria:**
- AOT-Nb fib(40) within 1.5× of `rustc -O fib.rs` (today: 2.4×).
- AOT beats JIT on cold-start workloads (no warm-up, no IC pollution).
- Microbench harness shows AOT column for the 8 benches.

## Phase 6 — Hardening

**Target gap:** AOT'd binaries don't handle the long tail of
"things go wrong" cases gracefully.

| Iter | Scope | Effort |
|------|-------|--------|
| 6.1 | **Stack overflow handling** — AOT'd binary catches host-stack overflow in deep recursion, prints a Scheme-level diagnostic, exits cleanly. Today: SEGV. | 3 days |
| 6.2 | **OOM handling** — heap allocation failure (Gc::alloc returning Err) propagates to a Scheme-level condition, doesn't abort. | 3 days |
| 6.3 | **Multi-procedure binary** — `crabscheme aot prog.scm` should emit a single binary that exposes multiple top-level procedures via CLI args (`./prog <fn> <args...>`), not just one `--entry`. | 1 week |
| 6.4 | **Cross-compile** — `crabscheme aot --target wasm32-wasip1 prog.scm` emits a project with the target set; user runs `cargo build --target=...`. Tested in release workflow. | 4 days |
| 6.5 | **AOT'd binary `--version`** — embeds the crabscheme version that produced it. Helps users + bug reports correlate. | 2 days |
| 6.6 | **JIT-as-oracle correctness check** — `crabscheme aot --verify prog.scm` AOTs the program, also JIT-runs it on a sample input, and warns if the two differ. Cheap insurance. | 4 days |
| 6.7 | **Signal handling** — Ctrl-C on a long-running AOT'd binary exits cleanly, not via panic. | 2 days |

**Exit criteria:**
- Stack overflow + OOM produce Scheme conditions, not SEGV/abort.
- `--target` cross-compilation works for the 4 release targets.
- All AOT'd binaries report `--version` matching the crabscheme that built them.
- `--verify` finds zero divergences on the conformance corpus.

## What this phase explicitly DOES NOT do

- **FFI in AOT'd binaries.** Today FFI assumes a long-running runtime; AOT'd binaries are short-lived. Future work, separate plan.
- **Multi-shot call/cc in AOT'd binaries.** Inherits the M8 walker-tier deferral.
- **Browser WASM (`wasm32-unknown-unknown`)**. Separate stretch (no WASI; needs JS-bound stdio).
- **Verified core (M11)**. Independent stretch.

## Sequencing decisions to revisit

- Phase 1 (distribution) before Phase 2 (coverage). Rationale: a
  release-installed binary that fails to AOT is a worse first
  impression than one that AOTs only fib. Users will hit Phase 1
  before they hit any Phase 2 gap.
- Phase 3 (correctness) starts in parallel with Phase 2 after iter
  2.1 lands. Rationale: differential testing needs MakeClosure
  support to cover the conformance fixtures that use closures;
  starting at 2.1 means the harness is ready as 2.4 / 2.7 land.
- Phase 5 (perf) AFTER Phase 4 (diagnostics). Rationale: most
  users hit "doesn't compile" before "compiles but is slow."
  Diagnostics unblock more users.

## Exit posture for the whole plan

A user can:

```bash
# Install
curl -fsSL .../crabscheme-1.x.tar.gz | tar -xz

# AOT any well-formed Scheme program
./crabscheme aot my-program.scm --build
./my-program-aot/target/release/my-program <args>

# Get help when it doesn't work
./crabscheme aot --explain my-program.scm
./crabscheme aot --doctor

# Verify correctness
./crabscheme aot --verify my-program.scm

# Cross-compile
./crabscheme aot --target wasm32-wasip1 my-program.scm
```

And the resulting binary:
- Returns the same answer as the walker / VM / JIT tiers on every
  conformance fixture.
- Runs within 1.5× of `rustc -O` of an equivalent hand-written
  Rust program on hot numeric paths.
- Survives stack overflow, OOM, and Ctrl-C without SEGV.
- Reports its provenance via `--version`.

That's "robust" — AOT graduates from a milestone-Track deliverable
to a production-grade tool users can rely on.

## Tracking

This plan gets its own exit report (`aot-hardening-exit.md`) when
all six phases close. Per-phase exit reports (`aot-hardening-
phaseN-exit.md`) land as each one finishes. Per-iter commits
follow the RC2 pattern (`AOT-hardening phase N iter M: ...`).

A tag of `aot-hardening-complete` lands at the end. The 1.0 GA
tag decision is independent — could ship 1.0 GA with AOT in its
current state (RC2) and defer this whole plan to a 1.1 hardening
release, OR ship 1.0 GA only after AOT hardening completes. That's
a release-strategy call, not a technical block.
