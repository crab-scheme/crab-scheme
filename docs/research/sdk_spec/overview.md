# Overview — Vision, Philosophy, Goals

## Vision

Crab Scheme is a **distributed, durable, agent-native Lisp runtime**.

It combines:

- Scheme/Lisp ergonomics
- Rust systems performance and safety
- Erlang-style actors and supervision
- CRDT-based eventually consistent state
- Consensus-backed authoritative state
- Durable workflow replay
- Content-addressed code and versioning
- Deterministic distributed simulation
- First-class AI agent orchestration

It is not just a programming language. It is a distributed
operating system disguised as a Lisp.

## Core philosophy

```text
Actors handle concurrency.
Supervisors handle failure.
Discovery finds nodes.
Networking connects them.
CRDTs handle mergeable truth.
Consensus handles official truth.
Durable workflows handle time.
Events handle memory.
Content-addressed code handles versioning.
Capabilities handle trust.
Policies handle boundaries.
Agents handle judgment.
Tools handle action.
Evals handle quality.
Simulation handles confidence.
```

Every line in that list maps to a concrete subsystem with a
dedicated section of this spec and a milestone in the roadmap.

## Primary goals

| # | Goal | Where it's specified | How it's measured |
|---|------|----------------------|-------------------|
| G1 | Distributed-first runtime | `distributed.md` | A 3-node cluster handles a workload one node was running; one node can crash without taking down the workload. |
| G2 | Failure-aware semantics | `runtime-kernel.md` § Supervision | A panicking actor never kills the host process; supervisor restart policies are observable. |
| G3 | Durable long-running computation | `durable-execution.md` | Workflows survive process restart; replay produces the same result; sagas compensate on failure. |
| G4 | Safe hot upgrades | `language.md` § hot upgrade + `runtime-kernel.md` § hotreload | An actor mid-call can be upgraded without losing in-flight work; old workflows continue using pinned code hashes. |
| G5 | Explicit consistency semantics | `consistency.md` | The user picks `crdt` (eventually consistent) vs `replicated-actor` (linearizable consensus) per use case; no implicit consistency tier. |
| G6 | Agent-native orchestration | `agentic.md` | Agents are supervised actors; tool calls are typed and policy-checked; durable agent workflows survive restart. |
| G7 | Deterministic replay and simulation | `operations.md` § simulation | The simulation transport produces byte-identical message traces given the same seed. |
| G8 | Strong observability | `operations.md` § observability | Every actor, every workflow, every tool call, every consensus decision is traced and queryable. |
| G9 | Cloud-native networking and discovery | `distributed.md` § discovery | Discovery providers for static, DNS, k8s, postgres, etcd, consul, mDNS, gossip. |
| G10 | Version-safe code evolution | `language.md` § codebase DB | Old workflows pinned to old code hashes; new code is a new hash; schema evolution is explicit. |

## Non-goals

- **Replacing relational databases.** Crab tables (`cs-table`) are
  for in-memory shared state, not analytical workloads. Use Postgres
  / SQLite for the latter; Crab integrates via `cs-stdlib-postgres`.
- **Transparent WAN-scale magic.** v1 targets a single LAN /
  cloud-region cluster. Cross-region replication is a v2 concern
  with explicit topology declarations.
- **Implicit distributed consistency.** The language refuses to
  let a developer mutate distributed state without naming the
  consistency model. `(crdt/pn-counter ...)` vs `(replicated-actor
  ... #:consistency 'linearizable)` are syntactically different.
- **Hiding network boundaries.** `send-local` and `send-remote` are
  separate primitives. Remote operations carry observable failure
  modes.
- **Pretending failures do not exist.** Supervisors, monitors,
  links, durable workflows, sagas, leases with fencing tokens —
  the entire stack is built around the assumption that things break.

## Design tensions and the resolutions this spec adopts

### Tension 1: Lisp dynamism vs durable replay determinism

Workflows must replay deterministically. But Lisp loves dynamic
dispatch, `(eval ...)`, hot reload, and macro expansion at runtime.

**Resolution.** Workflows compile to a sub-language (`crab-workflow`)
that statically forbids non-deterministic primops (no `random`, no
wall-clock `current-time`, no direct network I/O). The forbidden
forms are detected at expand time, not at runtime. Activities are
the escape hatch for impure operations. See `durable-execution.md`.

### Tension 2: CRDT vs consensus

Some state genuinely is mergeable (presence, soft counters,
collaborative documents). Some state genuinely is not (account
balances, exclusive locks). The same language has to serve both.

**Resolution.** CRDT and consensus are *separate, orthogonal*
subsystems with different APIs (`crdt/*` vs `replicated-actor`).
A "soft" use case picks the CRDT; a "hard" use case picks
consensus. The language never silently chooses for the user.
See `consistency.md`.

### Tension 3: Agentic flexibility vs production safety

LLM agents are useful when they have agency. But agency without
guardrails ships data exfiltration / financial loss / arbitrary
RCE in the supply chain.

**Resolution.** Every tool call goes through a policy gate.
Policies are first-class Scheme values. Production deploys can
deny-by-default with explicit allowlists. Human approval is a
durable workflow primitive (`(await-human ...)`), not an out-of-band
hack. Capabilities flow through the call graph. See `agentic.md` §
policies + `security.md`.

### Tension 4: Hot reload vs durable replay

Hot reload changes function definitions in place. Durable replay
needs the *exact* function that was running when the workflow
started, even months later.

**Resolution.** Code is content-addressed. Workflow histories
pin code hashes. A hot-loaded v2 doesn't replace the v1 hash; it
adds a new one and updates the "current" pointer. Replay of a
v1 workflow continues to fetch the v1 code by hash. See
`language.md` § codebase DB.

## What "v1" means in this spec

"v1" of the SDK means: **the system can run a production agent
fleet across a 3-to-30-node cluster, with durable workflows,
CRDT presence, Raft-backed leases, and supervised agents calling
typed tools through a policy layer**.

It does not mean: cross-region replication, geo-distributed
consensus, transparent WAN clustering, custom hardware support.
Those land in v2.

## Status

| Layer | v1 status (target) |
|-------|--------------------|
| Language core (R6RS+) | ✅ shipped (in `main`) |
| Macros (syntax-case, syntax-parse) | ✅ shipped |
| Contracts + typed boundaries | ✅ shipped |
| Single-node actor runtime (cs-actor) | ✅ shipped |
| Channels (cs-channel) | ✅ shipped |
| Hot reload (cs-hotreload) | ✅ shipped (single-node) |
| Sandboxing (L1 + L2) | ✅ shipped |
| Stdlib (~30 crates) | ✅ shipped |
| JIT (Cranelift) | ✅ shipped |
| AOT | ✅ shipped (numeric kernels) |
| Content-addressed codebase DB | 🚧 M12 |
| Distributed actor substrate (cs-distrib) | 🚧 M02 |
| Discovery providers (cs-discovery) | 🚧 M03 |
| Membership + failure detection | 🚧 M04 |
| CRDT layer (cs-crdt) | 🚧 M05 |
| Consensus (cs-consensus) | 🚧 M06 |
| Leases + fencing | 🚧 M07 |
| Durable workflows (cs-workflow) | 🚧 M08 |
| Agentic runtime (cs-agent) | 🚧 M09–M11 |
| Capability + policy DSL (cs-cap) | 🚧 M10 |
