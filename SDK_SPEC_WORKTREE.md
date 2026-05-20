# sdk-spec worktree

This worktree carries the **full language SDK spec** for Crab Scheme:
the multi-document design + roadmap that lifts the language from a
single-process Scheme with a BEAM-style actor runtime into a
distributed, durable, agent-native programming substrate.

Branch: `sdk-spec` off `main`.

## Where the spec lives

```
docs/research/sdk_spec/
├── README.md                # entry; reading order; one-page summary
├── overview.md              # vision, philosophy, design goals, non-goals
├── architecture.md          # consolidated architecture + crate map
├── language.md              # effects, hot upgrade forms, codebase DB
├── runtime-kernel.md        # actors, mailboxes, supervision, monitors
├── distributed.md           # discovery, networking, membership, clustering, sharding
├── consistency.md           # CRDT layer, consensus engine, leases, fencing
├── durable-execution.md     # workflows, activities, timers, signals, sagas, replay
├── agentic.md               # models, tools, agents, memory, policies, evals, multi-agent
├── security.md              # capabilities, mTLS, effect permissions, audit
├── operations.md            # observability, simulation, backpressure, schema evolution
├── roadmap.md               # M1..M12 with dependencies + parallel tracks
├── references.md            # external research citations
└── tasks/                   # one file per milestone: detailed task list
    ├── M01-foundations.md
    ├── M02-cluster-substrate.md
    ├── ...
    └── M12-content-addressed-code.md
```

## How the spec was built

- Input: the maintainer's consolidated *Distributed Runtime & Agentic
  Systems Design* document (vision, philosophy, architecture, primitive
  sketches across 10+ subsystems).
- Research: four parallel research passes — distributed actor systems
  (Erlang/OTP, Akka, Riak Core, phi accrual), CRDT + consensus
  (Automerge/Yjs, Riak Datatypes, Raft/VR/EPaxos), durable execution
  (Temporal, DBOS, Cadence, Inngest, Restate), agentic runtimes
  (Anthropic Claude Agent SDK, LangGraph, AutoGen, DSPy, multi-agent
  coordination, evals). See `references.md`.
- Synthesis: this worktree.

## What's already in main vs new in this spec

Already shipped (referenced via code pointers in the spec):

```
crates/cs-actor/         single-node actors, mailboxes, PIDs, registry
crates/cs-channel/       MPMC + broadcast + select + rendezvous channels
crates/cs-hotreload/     two-version code dispatch + state migration
crates/cs-table/         shared atomic tables (ETS-style)
crates/cs-sandbox-wasm/  L1 + L2 sandbox isolation
crates/cs-runtime/       host VM, parameters, contracts, conditions
crates/cs-vm/            bytecode VM
crates/cs-jit-cranelift/ Cranelift JIT
crates/cs-aot/           AOT compiler
crates/cs-opt/           optimizer plugin framework
crates/cs-pkg/           manifest + lockfile + resolver
crates/cs-expand/        macro expander incl. syntax-case
crates/cs-stdlib-*/      ~30 stdlib crates (fs, net, http, json, regex, ...)
```

New crates this spec proposes (sketched in `architecture.md`):

```
crates/cs-codebase/      content-addressed AST DB
crates/cs-discovery/     pluggable discovery providers
crates/cs-distrib/       node-to-node remote actors, gossip, membership
crates/cs-crdt/          CRDT primitives + delta sync
crates/cs-consensus/     Raft-based replicated logs + leases
crates/cs-workflow/      durable workflow engine + replay
crates/cs-agent/         models, tools, agents, evals, policies
crates/cs-cap/           capability tokens + policy DSL
```

## Workflow

This worktree is a writing-only branch — no Rust source changes, only
markdown. Drop the spec to `main` via a regular PR when each section
is complete enough to anchor the actual implementation work.

The detailed task lists in `docs/research/sdk_spec/tasks/M*.md` become
the implementation backlog. Each milestone is expected to live on its
own future worktree (`worktree-cluster-substrate`,
`worktree-crdt`, `worktree-workflow`, `worktree-agent`, etc.) so the
implementation phases can ship in parallel.
