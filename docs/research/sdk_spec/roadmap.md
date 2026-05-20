# Roadmap — Milestones M01..M12

Twelve milestones organized into four phases. Each milestone is
intended to become its own implementation worktree, with the
detailed task list in `tasks/M*.md` serving as the launch brief.

## Phase ordering

```text
        ┌───────────────────────────────────────────────────┐
        │  Phase A: Foundations (sequential)                 │
        │  ───────────────────────────────────                │
        │  M01  Effects + hot-upgrade form syntax            │
        │  M12  Content-addressed codebase DB                │
        │       (numbered out-of-order; unlocks durable      │
        │        replay safety + version-pinning. Run        │
        │        immediately after M01.)                      │
        └────────────────────┬──────────────────────────────┘
                             │
        ┌────────────────────┴──────────────────────────────┐
        │  Phase B: Distributed substrate (sequential)        │
        │  ────────────────────────────────────────────       │
        │  M02  Distributed actor transport (cs-distrib)      │
        │  M03  Discovery providers (cs-discovery)            │
        │  M04  Membership + failure detection                │
        └────────────────────┬──────────────────────────────┘
                             │
        ┌────────────────────┴──────────────────────────────┐
        │  Phase C: Consistency (parallelizable internally)   │
        │  ──────────────────────────────────────────         │
        │  M05  CRDT layer (cs-crdt)        ─┐                 │
        │  M06  Consensus engine (cs-consensus)  ─┐            │
        │  M07  Leases + fencing tokens          ─┴ depends on M06 │
        └────────────────────┬──────────────────────────────┘
                             │
        ┌────────────────────┴──────────────────────────────┐
        │  Phase D: Durable + agentic (parallelizable)        │
        │  ──────────────────────────────────────────         │
        │  M08  Durable workflows (cs-workflow)               │
        │  M09  Agentic runtime — models + tools              │
        │  M10  Capabilities + policy DSL (cs-cap)            │
        │  M11  Agentic runtime — agents + memory + evals     │
        └───────────────────────────────────────────────────┘
```

## Dependency graph

```text
M01 ─→ M12 ─→ M02 ─→ M03 ─→ M04 ─┬─→ M05 ─→ ...
                                  ├─→ M06 ─→ M07 ─→ ...
                                  │
                                  └─→ M08 ─→ M09 ─→ M10 ─→ M11
```

- M01 → everything (effect annotations are the type-system anchor).
- M12 → M08 (durable replay needs content-addressed code).
- M02 → M03/M04/M05/M06 (network transport is the substrate).
- M03 + M04 → M02 (distributed transport needs discovery + membership to actually form a cluster).
- M06 → M07 (leases need consensus).
- M08 → M09 → M11 (agents are durable workflows over models).
- M10 → M11 (agent policies are capabilities; agents may not ship without).

## Parallelism opportunities

After M04 lands, **M05, M06, and M08 can run in parallel** on
separate worktrees:

- M05 (CRDT) only depends on cs-net for anti-entropy gossip
- M06 (Consensus) only depends on cs-net for Raft RPCs
- M08 (Workflow) only depends on cs-codebase (M12) + cs-table (shipped)

Similarly, after M09 lands, **M10 and M11 can be split** — M10
ships the policy/capability DSL, M11 ships the actual agent
runtime that consumes it. The policy DSL is also useful
standalone for hardening `cs-distrib` and `cs-workflow`.

## Milestone summaries

### M01 — Foundations: effects + hot-upgrade form syntax

**Goal:** add `#:effects` annotation to define forms and a
`define-state-migration` form. Make `cs-expand` aware so it
can later forbid network effects inside workflows (M08) and
require migration definitions for hot-upgrade jumps.

**Deliverables:**
- `(define foo #:effects '(net io) (lambda ...))` parses and
  records the effect set in the IR.
- `(define-state-migration v1->v2 (lambda (s) ...))` form
  shipped (cs-hotreload already has a Rust-side hook).
- Effect-set algebra (union, subset, deny-list, allow-list)
  in `cs-rir`.
- Conformance: a `#:effects '(net)` body inside a workflow body
  fails to expand once M08 ships (gated test).

**Estimated effort:** 1-2 iters.

**Code pointers:**
- `crates/cs-expand/src/lib.rs` — expander entry point
- `crates/cs-rir/src/lib.rs` — IR pass framework
- `crates/cs-hotreload/src/lib.rs` — existing migration hook
- `lib/beam/prelude.scm` — `define-state-migration` macro lives here

---

### M12 — Content-addressed codebase DB (cs-codebase)

**Goal:** every binding has a hash; namespaces map names to
hashes; the dependency graph is queryable; workflows pin hashes
so durable replay (M08) is safe across hot upgrades.

**Deliverables:**
- `cs-codebase` crate with `Hash::of(&CoreExpr)`, `Namespace`,
  `DepGraph`, `Migration` types.
- `(hash-of foo)` and `(pin-workflow! 'name #abc123)` primops.
- Storage adapter (sled / rocksdb / postgres feature-gated).
- Workflow integration: `cs-workflow` records the current
  hash of the workflow body in its history; replay refuses if
  the hash isn't reachable.

**Estimated effort:** 3-4 iters.

**Code pointers:**
- `crates/cs-rir/src/lib.rs` — canonical AST shape for hashing
- `crates/cs-pkg/src/lib.rs` — existing manifest + lockfile, similar storage shape
- `crates/cs-hotreload/src/lib.rs` — existing version tracker
- External: <https://www.unison-lang.org/docs/the-big-idea/>

---

### M02 — Distributed actor transport (cs-distrib)

**Goal:** an actor on node A can `(send pid msg)` to a `pid`
hosted on node B. `(spawn-remote 'node-b worker-fn)` returns
a `RemoteRef` indistinguishable from a local `ActorRef` for
message-sending purposes.

**Deliverables:**
- `cs-distrib` crate with `NodeId`, `RemoteRef`, `Router`,
  `Handshake` (mTLS via rustls).
- `cs-net` crate with `Transport::Tcp` (and a stub for QUIC).
- Per-peer connection pooling; logical-channel multiplexing
  (control / messages / bulk / consensus / workflow / obs).
- Cross-node `(send pid msg)` works under unit tests with
  the in-memory `Transport::Sim`.
- Failures (peer down, transport closed) surface as
  monitor messages, not panics.

**Estimated effort:** 4-6 iters.

**Code pointers:**
- `crates/cs-actor/src/lib.rs` — `ActorRef` trait; `RemoteRef`
  must implement the same surface.
- `crates/cs-web/` — existing tokio + hyper transport stack;
  patterns for connection mgmt + TLS apply here.
- External: Erlang distribution protocol — <https://www.erlang.org/doc/apps/erts/erl_dist_protocol.html>
- External: Akka Cluster — <https://doc.akka.io/docs/akka/current/typed/cluster.html>

---

### M03 — Discovery providers (cs-discovery)

**Goal:** a Scheme `(cluster #:discovery ...)` form can chain
multiple providers (Static / DNS / Kubernetes / Postgres / etcd
/ Consul / Cloud Map / mDNS / Gossip) with first-success
semantics.

**Deliverables:**
- `cs-discovery` crate with `DiscoveryProvider` trait.
- Concrete providers behind feature flags so binary size stays
  small for embedders.
- `first-success` combinator: try providers in order, return
  the first that yields a non-empty member set.
- Conformance: 3-node cluster forms via each provider in CI
  (k8s tested via kind / minikube).

**Estimated effort:** 3-5 iters (one per provider family).

**Code pointers:**
- `crates/cs-stdlib-net/` — existing DNS / HTTP plumbing
- `crates/cs-stdlib-postgres` — existing Postgres client (if shipped) for db-backed
- External: Akka Discovery — <https://doc.akka.io/docs/akka-management/current/discovery/index.html>
- External: HashiCorp Consul API — <https://developer.hashicorp.com/consul/api-docs>

---

### M04 — Membership + failure detection

**Goal:** the cluster knows who's up, who's down, who's
suspect. Failed peers are detected within `~5s` p99 on a 30-node
cluster. Network partitions don't silently corrupt state.

**Deliverables:**
- Phi accrual failure detector in `cs-distrib`.
- Membership state machine: `joining → weakly-up → up →
  leaving → exiting → down → removed → quarantined`.
- Partition policies: `pause-minority`, `keep-majority`,
  `manual-recovery`, `isolate-region`.
- Gossip protocol (SWIM-family) running on the `control`
  logical channel.
- Conformance: kill a node, watch membership reconverge.

**Estimated effort:** 3-4 iters.

**Code pointers:**
- `crates/cs-distrib/src/` (created in M02) — extend with
  `PhiAccrual`, `Member`, `Gossip` modules.
- External: phi accrual paper — Hayashibara et al., "The φ
  Accrual Failure Detector", SRDS 2004.
- External: SWIM — Das, Gupta, Motivala, "SWIM: Scalable
  Weakly-consistent Infection-style Process Group Membership
  Protocol", DSN 2002.

---

### M05 — CRDT layer (cs-crdt)

**Goal:** a Scheme programmer can declare distributed state as a
CRDT and the runtime takes care of merging across replicas. The
state isn't authoritative — for that, use M06.

**Deliverables:**
- `cs-crdt` crate with state-based CRDTs: G-Counter, PN-Counter,
  OR-Set, OR-Map, LWW-Register, MV-Register, RGA text CRDT,
  CausalMap.
- Delta-state extension for low-bandwidth sync (only deltas
  travel over the network, not full state).
- Anti-entropy gossip on cs-net's `messages` channel.
- Tombstone GC: timed compaction with a lower bound the user
  controls (e.g., `#:keep-tombstones-ms 60000`).
- Scheme surface: `(define c (crdt/pn-counter 'post:123:likes))`,
  `(crdt-inc! c 1)`, `(crdt-value c)`, etc.
- Vector clocks / version vectors for causal ordering.

**Estimated effort:** 4-5 iters.

**Code pointers:**
- `crates/cs-table/` — existing shared-state primitive;
  cs-crdt sits adjacent.
- `crates/cs-net/` — gossip transport.
- External: Shapiro et al., "Conflict-Free Replicated Data
  Types" — <https://hal.inria.fr/inria-00609399v1/document>
- External: Automerge — <https://automerge.org>
- External: Akka Distributed Data — <https://doc.akka.io/docs/akka/current/typed/distributed-data.html>

---

### M06 — Consensus engine (cs-consensus)

**Goal:** linearizable, leader-elected, log-replicated state on
top of cs-net. Used by M07 (leases), M11 (some agent state),
the cluster's own membership ground truth, and any
`(replicated-actor ...)`.

**Deliverables:**
- `cs-consensus` crate wrapping `openraft` as the v1 engine.
- Per-group log: `RaftGroup::new(group_id, members) → Handle`.
- Snapshotting: take/install snapshots on the `bulk` channel.
- Joint consensus for membership changes.
- Scheme surface: `(replicated-actor name #:replicas 3
  #:consistency 'linearizable ...)` desugars to a Raft group.
- Conformance: 3-node Raft group survives 1 down + 2 partition
  scenarios.

**Estimated effort:** 5-7 iters (Raft is hairy).

**Code pointers:**
- External: `openraft` crate — <https://crates.io/crates/openraft>
- External: Diego Ongaro's Raft thesis — <https://raft.github.io/raft.pdf>
- External: etcd Raft library — <https://github.com/etcd-io/raft>

---

### M07 — Leases + fencing tokens

**Goal:** when one worker must hold an exclusive lease (e.g.,
"only one node sends email"), the system enforces the
"highest fencing token wins" invariant so stale workers cannot
corrupt protected state.

**Deliverables:**
- `cs-consensus::Lease` API: `(lease-acquire! 'name #:ttl-ms ...)`,
  `(lease-renew! l)`, `(lease-release! l)`.
- Monotonic fencing tokens attached to each lease grant.
- Protected primitives accept fencing tokens; reject stale.
- Scheme idioms in `lib/beam/leases.scm`.
- Conformance: deliberately partition a leaseholder, confirm
  it can't write after token is bumped.

**Estimated effort:** 1-2 iters (built on M06).

**Code pointers:**
- External: Martin Kleppmann, "How to do distributed locking" —
  <https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>
- External: Burrows, "The Chubby Lock Service", OSDI 2006.

---

### M08 — Durable workflows (cs-workflow)

**Goal:** `(define-workflow ...)` bodies survive process crashes,
replay deterministically, and compose via sagas with explicit
compensation. Non-deterministic operations are statically forbidden
inside workflow bodies; activities are the escape hatch.

**Deliverables:**
- `cs-workflow` crate: `Workflow`, `Activity`, `Timer`, `Signal`,
  `Query`, `RetryPolicy`, `Saga`, `Replay`.
- History storage adapter (cs-table for embedded; postgres for
  prod via cs-stdlib-postgres; cs-consensus::Kv for self-hosted).
- `cs-expand` integration: `(define-workflow ...)` runs an
  effect-set check (M01) and rejects non-deterministic primops.
- Activity workers: independent processes/actors that pick up
  activity tasks, run them, report results back to the history.
- `(await-human ...)` as a workflow primitive (blocks until a
  durable signal of the right shape arrives).
- Scheme idioms in `lib/workflow/prelude.scm`.

**Estimated effort:** 6-8 iters.

**Code pointers:**
- `crates/cs-actor/` — actor model is the substrate
- `crates/cs-channel/` — signals are a channel
- `crates/cs-codebase/` (M12) — replay needs version pinning
- External: Temporal docs — <https://docs.temporal.io/concepts>
- External: DBOS docs — <https://docs.dbos.dev/>
- External: Cadence original paper — "Workflow Orchestration at Uber"

---

### M09 — Agentic runtime: models + tools

**Goal:** ship the lowest level of the agentic runtime — a way
to describe a model, a way to declare a typed tool, and a way to
call them. Agents come in M11; this milestone is the substrate.

**Deliverables:**
- `cs-agent` crate with `Model`, `ModelProvider`, `Tool`,
  `ToolSchema`, `ToolResult`.
- Providers (feature-gated): Anthropic, OpenAI, Bedrock, vLLM/Ollama (local).
- Scheme surface: `(define-model gpt #:provider 'openai
  #:model "gpt-5.5")`, `(register-tool! 'send-email #:schema
  '(object (to string) ...) #:effects '(net) #:handler ...)`.
- JSON Schema validation on tool inputs/outputs.
- Tool-call streaming + parallel tool calls (Anthropic-style).
- Audit-log integration with cs-cap (M10) — every tool call is
  recorded.

**Estimated effort:** 3-4 iters.

**Code pointers:**
- `crates/cs-stdlib-http/` — HTTP client for provider API calls
- `crates/cs-stdlib-json/` — JSON Schema validation
- `crates/cs-runtime/src/builtins/mod.rs` — register primops here
- External: Anthropic Claude SDK — <https://docs.claude.com/en/api/messages>
- External: OpenAI tool calling — <https://platform.openai.com/docs/guides/function-calling>

---

### M10 — Capabilities + policy DSL (cs-cap)

**Goal:** every privileged operation (network send, file I/O,
tool call, remote spawn, lease acquire, model call) goes through
a capability check + a policy evaluator. Production agents can
deny-by-default with explicit allowlists.

**Deliverables:**
- `cs-cap` crate: `Capability` (unforgeable token), `Policy`
  (predicate expressed as a Scheme value), `Audit`.
- Policy DSL: `(define-policy production-safety
  (deny tool-call #:when '(and ...)))`.
- Capability flow through call graph: child actors inherit
  parent's caps unless explicitly attenuated.
- mTLS certs in `cs-distrib` are capabilities.
- Workflows / agents declare required caps; runtime injects.
- Audit log: queryable, append-only, retention-policied.

**Estimated effort:** 3-4 iters.

**Code pointers:**
- `crates/cs-sandbox-wasm/` — existing L1+L2 sandbox (caps applied at L1 level)
- `crates/cs-runtime/src/builtins/mod.rs` — gate b_* primops with cap checks
- External: object-capability model — <http://erights.org/elib/capability/index.html>
- External: OPA policy engine — <https://www.openpolicyagent.org/docs/policy-language>

---

### M11 — Agentic runtime: agents + memory + evals + multi-agent

**Goal:** the full agentic story. An agent is a supervised actor
with a model, a tool list, a memory, and a policy. Agents can be
composed into teams. Long-running agents are durable workflows.
Evals are first-class tests.

**Deliverables:**
- `(define-agent ...)` form: model + tools + memory + policies +
  optional supervisor strategy.
- Memory subsystems: vector (pgvector / qdrant), episodic
  (event log), CRDT (shared scratch), consensus (authoritative),
  cache (in-RAM).
- `(define-agent-workflow ...)` — agents as durable workflows.
- `(await-human ...)` integration (from M08).
- Multi-agent coordination patterns:
  `supervisor`, `blackboard`, `debate`, `map-reduce`,
  `pipeline`, `human-in-the-loop`.
- `(define-eval ...)`: dataset + assertion + LLM-as-judge
  options.
- Trace export over OTLP (cs-stdlib-otel).

**Estimated effort:** 5-7 iters.

**Code pointers:**
- `crates/cs-actor/` — agent IS an actor
- `crates/cs-workflow/` (M08) — agent-workflow integration
- `crates/cs-cap/` (M10) — policy enforcement
- External: LangGraph — <https://langchain-ai.github.io/langgraph/>
- External: AutoGen — <https://microsoft.github.io/autogen/>
- External: DSPy — <https://dspy-docs.vercel.app/>
- External: Anthropic agent design — <https://www.anthropic.com/research/building-effective-agents>

---

## Out of scope for v1

The following are explicitly post-v1:

- **Cross-region clusters** (single LAN/cloud-region only).
- **EPaxos** consensus backend (Raft only; VR as a v2 backend).
- **Computer-use / browser agents** (provider has it; the
  spec doesn't bind to that surface in v1).
- **GPU-local model hosting** (we proxy to vLLM/Ollama; we don't
  embed inference).
- **Workflow visual editor / web UI** (CLI + log viewer only).
- **Native code hot-reload of AOT-compiled binaries** (already
  out-of-scope per BEAM spec; mentioned here for completeness).
