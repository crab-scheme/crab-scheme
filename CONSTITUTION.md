# The CrabScheme Constitution

The durable, foundational rules for how CrabScheme is built. Specs, milestones,
and ADRs come and go; these articles govern all of them. When a decision
conflicts with an article, the article wins — or the article is amended first
(see *Amending*, below). `CONTRIBUTING.md` covers day-to-day workflow; this
covers the *laws*.

---

## Article I — The code is Scheme; Rust is the machine

**At the end of the day, the code is Scheme.** CrabScheme is a Scheme, and its
libraries, SDK, and application logic are written *in* CrabScheme, under
`lib/*.scm`. Rust exists only to build **the machine** — the lexer, parser,
runtime, GC, JIT/AOT, and the irreducible **primitives that touch the outside
world** (I/O, sockets, timers, threads, FFI) or that demand raw performance and
memory control. Everything expressible as pure logic over those primitives
**must** be Scheme.

**Litmus test for "Rust or Scheme?"**
- Does it make a system call, cross an FFI boundary, or need hand-tuned
  performance / memory layout? → **Rust primitive** (a crate).
- Is it policy, protocol, orchestration, or dispatch logic built *on* those
  primitives? → **Scheme library** (`lib/`).

**Corollaries.**
- A new *library* defaults to Scheme. A new *Rust crate* must justify itself as
  a primitive, not a convenience.
- Exposing a new primitive to Scheme is itself work; prefer composing existing
  primops over adding crates.

**Precedent.** `cs-supervisor` (a Rust crate) was *deleted*; supervision trees
live in `lib/beam/prelude.scm` as pure dispatch over four actor primops
(`spawn`, `send`, `raw-receive`, `self`). Consensus (Raft/EPaxos) is
`lib/consensus/*.scm`, **not** a Rust crate — the protocol is pure dispatch;
only the transport (`cs-net`) and actors (`cs-actor`) are Rust primitives.

---

## Article II — Pure cores, effects at the edges

Logic is written as **pure functions**: `(state, input) → (state, outputs)`.
I/O, clocks, randomness, and mutation are pushed to the boundary. *Decide* and
*do* are separate functions — never the same one. A pure core can be exercised
exhaustively and deterministically without sockets, threads, or a wall clock.

**Precedent.** The consensus engines are pure step machines (a tick or a
message returns the messages to send); they're driven by a deterministic
in-memory cluster simulator. The `cs-net` Sim transport lets the whole cluster
substrate be tested with no syscalls.

---

## Article III — Prove it; don't claim it

A performance or correctness claim is not true until it is **measured or
tested**. "This design avoids X" / "this is faster" must be backed by a
benchmark or a test that would fail if it weren't so. Loopback/ideal-case
numbers are not proof of a property that only matters under stress.

**Precedent.** "One QUIC stream per channel avoids head-of-line blocking" was
*aspirational* — invisible on lossless loopback. It became true only when a
5%-packet-loss benchmark and a regression test demonstrated it (~110× lower
control latency vs a single shared stream). Regressions are reported, not
hidden (a JIT change's fib slowdown was surfaced alongside its broad wins).

---

## Article IV — Tests are the contract

Write the test first, watch it fail, then make it pass (red → green). **Never
disable, weaken, or delete a test to get a green check.** Every bug fix ships a
regression test that *fails on the old code and passes on the new* — that test
is the proof the bug is real and fixed.

**Precedent.** The `cs-actor` cancel-safety fix added a reproduction test that
failed before the fix (mailbox wedged shut) and passed after. Commit hooks and
`--no-verify` are never bypassed; failing tests are fixed, not skipped.

---

## Article V — Reuse the machine; one concern per crate

Learn the existing primitives before adding new ones; build on `cs-net`,
`cs-actor`, `cs-table`, `cs-runtime` rather than reinventing. Each Rust crate
owns exactly **one** concern, with a boundary narrow enough to test in
isolation.

**Precedent.** Consensus rides the `cs-net` `Channel::Consensus` and `cs-actor`
rather than growing its own networking. The M02 transport is bytes-only, with
no `cs-actor`/`cs-runtime` coupling, so it's deterministically testable on its
own.

---

## Article VI — Honesty over green checkmarks

Report outcomes faithfully. State what failed (with output), what was skipped,
and what was deferred. Document **intentional deviations** from a spec and
**deferred** work in an exit report or ADR. Do not bypass safety mechanisms
(branch protection, required CI, commit hooks) to make work *look* finished;
unverified is not done.

**Precedent.** A PR was left to auto-merge on green rather than admin-bypassing
still-running CI. Deviations from the consensus spec (homegrown instead of the
named engine; an extra algorithm) were documented in the milestone exit report.

---

## Article VII — Incremental, green commits

Commit working code per iteration, not in end-of-session bundles. **Every
commit compiles, passes its tests, and is formatter- and linter-clean.** The
message explains *why*, not just *what*. Stacking small, green commits keeps the
history bisectable and the branch always shippable.

---

## Article VIII — Determinism is a feature, and it is enforced

Replicated and durable state machines must be **deterministic** — no
`current-time`, no `random`, no I/O — so the same command sequence yields the
same state on every replica and every replay. Where this matters, it is
*checked*, not merely documented (the effect system rejects forbidden effects
in such bodies).

**Precedent.** The SDK effect annotations (`#:effects`) and the determinism
contract on replicated/workflow state-machine bodies.

---

## Amending this constitution

These rules are meant to last, but they are not sacred. To change one: open a PR
that edits this file with the rationale, and — for a load-bearing shift (e.g.
moving the Rust/Scheme line) — record an ADR. An article stands until it is
amended; "we didn't have time" is not an exception to it, it is a reason to
descope the work, not the law.
