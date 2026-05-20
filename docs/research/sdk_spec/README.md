# Crab Scheme — Full SDK Spec

> Status: research-stage draft (2026-05-19). Synthesizes the
> maintainer's *Distributed Runtime & Agentic Systems Design*
> document with research on Erlang/OTP, Akka Cluster, Riak Core,
> Automerge/Yjs, Raft/VR/EPaxos, Temporal/DBOS/Cadence/Restate,
> Anthropic Claude Agent SDK, LangGraph, AutoGen, and DSPy.

## One-page summary

Crab Scheme is a Scheme/Lisp dialect on top of a Rust runtime
(`cs-runtime`) with a multi-tier execution stack (walker → VM →
Cranelift JIT → AOT). It already has BEAM-style actors, channels,
hot reload, content-aware sandboxing, and a 30-crate stdlib.

This spec extends it into a **distributed, durable, agent-native
operating system disguised as a Lisp**: clustering across nodes,
CRDT-merged eventually-consistent state, consensus-backed
authoritative state, durable workflows, content-addressed code,
and first-class AI agent orchestration.

The work is organized into **12 milestones**, M01 through M12, with
explicit dependencies and three parallelizable tracks
(distributed/consistency, durable execution, agentic). Each
milestone has a dedicated task list in `tasks/M*.md` with concrete
sub-iterations, acceptance criteria, code pointers to the existing
codebase, and external references.

## Reading order

If you want the vision and the scope:

1. [`overview.md`](overview.md) — vision, philosophy, goals, non-goals
2. [`architecture.md`](architecture.md) — diagram + crate map
3. [`roadmap.md`](roadmap.md) — milestones M01..M12 + dependency graph

If you want the per-subsystem deep dives:

4. [`language.md`](language.md) — effect annotations, hot upgrade forms
5. [`runtime-kernel.md`](runtime-kernel.md) — actors, supervision, registries
6. [`distributed.md`](distributed.md) — discovery, transport, membership, sharding
7. [`consistency.md`](consistency.md) — CRDT + consensus + leases
8. [`durable-execution.md`](durable-execution.md) — workflows, activities, sagas
9. [`agentic.md`](agentic.md) — models, tools, agents, memory, policies, evals
10. [`security.md`](security.md) — capabilities, mTLS, effect permissions
11. [`operations.md`](operations.md) — observability, simulation, schema evolution

If you want the implementation backlog:

12. [`tasks/`](tasks/) — one file per milestone, ordered sub-iterations
13. [`references.md`](references.md) — external research citations

## How to use this spec

- Each milestone in `roadmap.md` and `tasks/M*.md` is intended to be
  the **launch brief for a new implementation worktree** (parallel
  to how `BEAM_WORKTREE.md` launched the actor work that's now in
  `cs-actor`, `cs-channel`, `cs-hotreload`).
- Per-subsystem docs (`runtime-kernel.md`, `distributed.md`, etc.)
  are the *design constants* — what shape do the APIs take, what
  invariants must hold, what's explicitly out of scope for v1.
- The roadmap is sequenced so that **M01-M04** unlock the
  distributed substrate (and can be done as one logical chunk),
  **M05-M07** add consistency primitives on top, **M08** adds
  durable workflows, and **M09-M11** layer the agentic runtime.
  **M12** (content-addressed code) is the latest but unlocks
  the durability story for the others.

## What's deliberately *not* in this spec

- Replacing relational databases.
- Transparent WAN-scale magic (cross-region clusters in v1).
- Implicit distributed consistency (the language asks the user
  to *choose* CRDT vs consensus per actor).
- Hiding network boundaries (`send-remote` is a different verb
  from `send-local`).
- Pretending failures don't exist (supervision is the failure
  story; durable workflows are the recovery story).
