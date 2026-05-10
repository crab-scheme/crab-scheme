# ADR 0009 — Parking HolyJIT (M7 Evaluation Report)

**Status:** Accepted (M7 reframed as evaluation per the ROADMAP fallback)
**Date:** 2026-05-10
**Context:** M7 — HolyJIT Backend
**Predecessor:** M6 (`docs/milestones/m6-exit.md`, Cranelift JIT shipped)

## Decision

**Park the HolyJIT backend integration; proceed to M8 (continuations) with Cranelift as the sole JIT for the foreseeable future.** Document the feasibility findings here and adjust the ROADMAP to reflect the parked state.

This invokes the explicit fallback the ROADMAP set out for M7:

> **Fallback:** If HolyJIT integration proves infeasible despite reasonable upstream effort, this milestone is reframed as "evaluation report" — the project ships with Cranelift as the primary JIT and HolyJIT integration is parked with a clear postmortem ADR.

## Findings

### 1. HolyJIT is unmaintained.

- **Repository**: <https://github.com/nbp/holyjit>
- **Last code commit**: `2018-08-19` ("Add a Nix expression to remove extra nix-shell arguments.")
- **Time since last activity**: ~7 years and 9 months (as of 2026-05-10).
- **GitHub `updated_at`**: refreshed by metadata only; the source tree is unchanged.
- **No crates.io release**: HolyJIT was never published as a stable crate.

A 7-year-stale Rust codebase, on a language and ecosystem that has changed substantially over that period, is effectively a fork-target rather than a dependency. Integrating it would require:

- Updating to a current `nightly` toolchain (HolyJIT used Rust 2018 nightly with `proc-macro` features that have evolved).
- Rebuilding HolyJIT's compiler-plugin/proc-macro layer — `#[jit]` annotation processing was its core mechanism, and that area of the toolchain is what changed most.
- Likely rewriting parts of the codegen against current `cranelift-codegen` (HolyJIT predates the crates.io split).
- Resolving any `#![feature(...)]` flags that have been stabilized, removed, or renamed.

### 2. The original M7 design assumed an active HolyJIT.

The ROADMAP entry for M7 budgets "contribution time to nbp/holyjit" — that budget was based on the project being a typical somewhat-stale OSS dep. With 7+ years of stagnation, the contribution required would be a fork-and-modernize project, not contributions to an upstream that exists.

### 3. Cranelift covers the JIT need.

M6 shipped Cranelift as the primary JIT and met the `(fib 30)` perf gate by ~25× margin (`docs/milestones/m6-exit.md`, `bench/m6-fib30-baseline.md`). The remaining JIT work for the project — broader instruction lowering, deopt trampoline, OSR, jit-dump — sits in the M6 Phase 2 deferred list and is more impactful than a second backend.

### 4. The `JitBackend` trait absorbs the design intent.

M6's `JitBackend` trait makes the runtime backend-agnostic. If a viable second JIT backend appears later (a different research project, a HolyJIT successor, a maintained fork of HolyJIT), it can land as a peer crate without disturbing the runtime. The trait was designed for exactly this; M7's parked state doesn't compromise it.

## Considered alternatives

### A. Fork HolyJIT and modernize it ourselves.

**Rejected.** Modernizing 7-year-stale Rust nightly + proc-macro infrastructure is a multi-month effort that doesn't advance the language's feature set; it's pure infrastructure work for a research-grade backend whose value-add over Cranelift is unclear. The "meta-JIT" model HolyJIT advocates is interesting but its claimed benefits over Cranelift's IR-level JIT haven't been demonstrated for Scheme-shaped workloads.

### B. Skip the second backend entirely; merge M7 deliverables into M6 Phase 2.

**Considered.** M7's main artifact would have been "the runtime selects between two JITs at runtime via `--jit=holy`." With no second backend, that flag is meaningless. The substantive work (lowering, deopt, perf) is already in the M6 Phase 2 backlog; nothing here gets lost by parking M7.

### C. Shop for a different "second backend" research project.

**Maybe later.** Candidates include LLVM via `llvm-sys`, custom FFI to `mir-rs` (Linux-perf JIT, not Rust JIT), or building a hand-rolled tracing JIT on top of `cs-rir`. None of these match the M7 spec, all of them are sizable engineering efforts. A future ADR can revisit if a compelling option appears.

## Consequences

- M7 milestone is **parked**, not closed-clean. We're not tagging `m7-complete` because no implementation shipped; the milestone is reframed as "evaluation: parked".
- ROADMAP M7 entry updates to reference this ADR with the parked status.
- The `cs-jit-holy` crate is **not** added to the workspace.
- Future iters proceed to M8 (continuations) per the original ROADMAP order.
- If/when HolyJIT or a comparable peer backend becomes viable, a successor ADR (`0010-...` or later) can re-enter the M7 design space; this ADR documents what was true at the time of decision.

## References

- `ROADMAP.md` — M7 fallback clause that this decision invokes.
- `docs/milestones/m6-exit.md` — Cranelift JIT exit report.
- `docs/adr/0007-jit-design.md` — JitBackend trait design (the seam a future second backend would slot into).
- <https://github.com/nbp/holyjit> — upstream repository (stale).
