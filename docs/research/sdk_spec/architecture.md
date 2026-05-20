# Architecture — Consolidated Diagram + Crate Map

## Full architecture

```text
Crab Scheme
├─ Language ───────────────────────── (mostly shipped)
│  ├─ Scheme core (R6RS+)              cs-lex / cs-parse / cs-expand / cs-ir / cs-rir
│  ├─ macros (syntax-case, parser)     cs-expand
│  ├─ modules + libraries              cs-runtime + cs-pkg
│  ├─ effect annotations               🚧 M01 (new in cs-runtime + cs-expand)
│  └─ hot upgrade forms                cs-hotreload (+ new define-state-migration macro)
│
├─ Codebase DB ───────────────────── 🚧 M12 (new: cs-codebase)
│  ├─ content-addressed ASTs           cs-codebase::Hash
│  ├─ dependency graph                 cs-codebase::DepGraph
│  ├─ namespace ↔ hash binding         cs-codebase::Namespace
│  ├─ type/effect signatures           cs-codebase::Signature
│  ├─ docs/tests as values             cs-codebase::Note + cs-codebase::Test
│  └─ migration graph                  cs-codebase::Migration
│
├─ Runtime Kernel ────────────────── (shipped + M01 extensions)
│  ├─ actors                           cs-actor::ActorSystem / ActorRef
│  ├─ mailboxes                        cs-actor::Mailbox (Fast | Durable)
│  ├─ supervision                      cs-supervisor + lib/beam/prelude.scm
│  ├─ monitors / links                 cs-actor + Scheme prelude
│  ├─ registries (pid + pg)            cs-actor::Registry + cs-table
│  └─ tracing hooks                    cs-runtime::trace + OpenTelemetry
│
├─ Distributed Runtime ───────────── 🚧 M02 (new: cs-distrib)
│  ├─ node identity                    cs-distrib::NodeId
│  ├─ secure handshake                 cs-distrib::Handshake (mTLS / Noise)
│  ├─ remote actor refs                cs-distrib::RemoteRef
│  ├─ remote spawn                     cs-distrib::spawn_remote
│  ├─ remote message routing           cs-distrib::Router
│  ├─ code transfer                    cs-distrib::CodeFetch (uses cs-codebase)
│  └─ tracing hooks                    cs-distrib::trace_remote_send
│
├─ Discovery System ──────────────── 🚧 M03 (new: cs-discovery)
│  ├─ static seeds                     cs-discovery::Static
│  ├─ DNS A / SRV                      cs-discovery::Dns
│  ├─ Kubernetes API                   cs-discovery::Kubernetes
│  ├─ file-based                       cs-discovery::FileBased
│  ├─ Postgres / MySQL / SQLite        cs-discovery::DbBacked
│  ├─ etcd / Consul / Nomad            cs-discovery::Etcd / Consul / Nomad
│  ├─ AWS Cloud Map / GCP / Azure VMSS cs-discovery::Cloud
│  ├─ mDNS                             cs-discovery::Mdns
│  └─ gossip exchange                  cs-discovery::Gossip
│
├─ Networking Layer ──────────────── 🚧 M02 (new: cs-net)
│  ├─ TCP + TLS                        cs-net::Transport::Tcp
│  ├─ QUIC                             cs-net::Transport::Quic (quinn)
│  ├─ WebSocket                        cs-net::Transport::WebSocket (tokio-tungstenite)
│  ├─ Unix sockets                     cs-net::Transport::Unix
│  ├─ in-memory simulation transport   cs-net::Transport::Sim
│  └─ logical channels                 cs-net::Channel (control | msgs | bulk | consensus)
│
├─ Membership & Failure Detection ── 🚧 M04 (in cs-distrib)
│  ├─ membership states                cs-distrib::MemberState (joining/up/leaving/...)
│  ├─ phi accrual failure detector     cs-distrib::PhiAccrual
│  ├─ partition policies               cs-distrib::PartitionPolicy
│  └─ gossip protocol                  cs-distrib::Gossip (SWIM/HyParView family)
│
├─ Consistency Layer ─────────────── 🚧 M05–M07 (new: cs-crdt, cs-consensus)
│  ├─ CRDT primitives                  cs-crdt::{GCounter,PNCounter,ORSet,ORMap,LWWReg,MVReg,Text,CausalMap}
│  ├─ delta sync + anti-entropy        cs-crdt::Sync
│  ├─ replicated actors                cs-consensus::ReplicatedActor
│  ├─ consensus engine                 cs-consensus::Raft (openraft) + future VR/EPaxos
│  ├─ leases                           cs-consensus::Lease
│  └─ fencing tokens                   cs-consensus::FenceToken (monotonic, attached to lease)
│
├─ Durable Execution ─────────────── 🚧 M08 (new: cs-workflow)
│  ├─ workflows                        cs-workflow::Workflow
│  ├─ activities                       cs-workflow::Activity
│  ├─ timers                           cs-workflow::Timer
│  ├─ signals + queries                cs-workflow::Signal / Query
│  ├─ retries                          cs-workflow::RetryPolicy
│  ├─ sagas                            cs-workflow::Saga
│  └─ replay engine                    cs-workflow::Replay
│
├─ Agentic Runtime ───────────────── 🚧 M09–M11 (new: cs-agent)
│  ├─ models                           cs-agent::Model (provider + endpoint + config)
│  ├─ tools                            cs-agent::Tool (schema + handler + effects)
│  ├─ agents                           cs-agent::Agent (model + tools + memory + policies)
│  ├─ memory                           cs-agent::Memory::{Vector,Episodic,Crdt,Consensus,Cache}
│  ├─ policies                         cs-cap::Policy + cs-agent::AgentPolicy
│  ├─ evals                            cs-agent::Eval + cs-agent::Dataset
│  └─ traces                           cs-agent::Trace (OpenTelemetry-compatible)
│
├─ Storage ───────────────────────── (mostly shipped + extensions)
│  ├─ append-only logs                 cs-stdlib-* (new: cs-stdlib-wal)
│  ├─ snapshots                        cs-codebase + cs-workflow (shared serializer)
│  ├─ local tables                     cs-table (ETS-style)
│  ├─ CRDT tables                      cs-crdt + cs-table integration
│  └─ consensus-backed KV              cs-consensus::Kv
│
├─ Operations ────────────────────── (partial; 🚧 M11)
│  ├─ observability                    cs-runtime::trace + OpenTelemetry SDK (cs-stdlib-otel new)
│  ├─ simulation                       cs-net::Transport::Sim + cs-sim crate
│  ├─ backpressure                     cs-actor::BoundedMailbox + cs-channel watermarks
│  ├─ schema evolution                 cs-codebase::Migration (uses cs-pkg manifests)
│  └─ upgrade tooling                  CLI: crabscheme upgrade / drain / rolling-restart
│
└─ Security ──────────────────────── 🚧 M10 (new: cs-cap)
   ├─ capabilities                     cs-cap::Capability (ocap-style, unforgeable tokens)
   ├─ mTLS                             cs-distrib::Handshake (rustls)
   ├─ effect permissions               cs-cap::EffectGrant (declarative)
   ├─ code-hash allowlists             cs-codebase + cs-cap interlock
   └─ audit logs                       cs-cap::AuditLog (append-only, queryable)
```

## Crate map (deltas from `main`)

### Already in `main`

```
cs-actor          single-node actors, mailboxes, PIDs
cs-aot            AOT compiler
cs-channel        MPMC + broadcast + select + rendezvous channels
cs-core           Value, Symbol, SymbolTable, common types
cs-diag           diagnostics
cs-expand         macro expander (syntax-rules + syntax-case)
cs-ffi            FFI host procedure trait
cs-gc             countable-memory + region memory
cs-hotreload      two-version code dispatch
cs-ir             core IR
cs-jit            JIT trait + interfaces
cs-jit-cranelift  Cranelift JIT backend
cs-lex            lexer
cs-opt            optimizer pass framework + plugins
cs-parse          parser
cs-pkg            package manifest + lockfile + resolver
cs-rir            regularized IR (post-expand)
cs-runtime        host VM, builtins, contracts, conditions, parameters
cs-sandbox-wasm   L2 wasmtime-based sandbox
cs-stdlib-*       ~30 stdlib crates
cs-supervisor     supervisor primitives (one_for_one, one_for_all, rest_for_one)
cs-table          ETS-style shared atomic tables + ordered_set + Mailbox
cs-vm             bytecode VM
cs-web            web framework (HTTP/1, HTTP/2, HTTP/3, contracts, layers)
```

### New crates this spec proposes

```
cs-codebase     Content-addressed AST DB. Hash::of(ast), Namespace,
                DepGraph, Migration. Sits below cs-runtime so the
                expander can resolve identifiers to hashes.

cs-distrib      Node-to-node actor handles, gossip, membership.
                Builds on cs-actor: RemoteRef implements the same
                ActorRef trait so `(send pid msg)` works uniformly.

cs-discovery    Pluggable discovery providers. Trait
                DiscoveryProvider { fn members() -> Vec<NodeId>; }.
                Concrete impls behind feature flags so binary size
                stays small for embedders who only need one.

cs-net          Transports (TCP+TLS, QUIC, WebSocket, Unix, Sim) +
                logical channel multiplexing. Used by cs-distrib +
                cs-consensus + cs-workflow for inter-node traffic.

cs-crdt         CRDT primitives. State-based + delta-state.
                Anti-entropy via gossip on top of cs-net.

cs-consensus    Raft (openraft) + leases + fencing tokens +
                replicated actors. Optional VR/EPaxos backends.

cs-workflow     Durable workflows. Activities, timers, signals,
                sagas, replay engine. Pluggable storage (cs-table /
                cs-consensus::Kv / postgres via cs-stdlib-postgres).

cs-agent        Models, tools, agents, memory, evals.
                Reuses cs-workflow for durable agent flows.
                Model providers behind features.

cs-cap          Capabilities, effect permissions, audit logs,
                policy DSL. Cross-cutting; used by cs-agent +
                cs-distrib + cs-workflow.

cs-sim          Deterministic simulation harness. In-memory
                transport (already in cs-net), virtual time,
                replayable seed.

cs-stdlib-wal   Append-only write-ahead-log primitive (shared
                across cs-workflow + cs-consensus + cs-codebase).

cs-stdlib-otel  OpenTelemetry tracing/metrics exporter.
```

## Build / dependency layers

```
                          cs-cap (capabilities + policy)
                                  ▲
                ┌─────────────────┼─────────────────┐
                │                 │                 │
            cs-agent        cs-workflow         cs-distrib
                │                 │                 │
        ┌───────┼────────┐        │         ┌───────┼────────┐
        │       │        │        │         │       │        │
   cs-stdlib*  cs-crdt cs-consensus cs-table cs-net cs-discovery cs-codebase
        │       │        │        │         │       │        │
        └───────┴────────┴────────┴─────────┴───────┴────────┘
                                  │
                              cs-runtime
                                  │
                  ┌───────────────┼───────────────┐
                  │               │               │
              cs-actor        cs-channel    cs-hotreload
                  │               │               │
              cs-supervisor   cs-table        cs-codebase
                  │
                cs-vm
                  │
              cs-jit-cranelift
                  │
                cs-aot
                  │
              cs-ir / cs-rir / cs-expand / cs-parse / cs-lex
                  │
                cs-core / cs-diag / cs-gc / cs-ffi
```

The dependency invariant: **lower layers never know about
upper layers**. `cs-runtime` does not depend on `cs-agent` or
`cs-workflow` — they depend on it. This lets a minimal embedder
(e.g., a WASM build) include only `cs-runtime` + the language
core and exclude all distributed/agentic code via feature flags.

## Process / OS layer

A Crab Scheme **node** is one OS process. Inside that process:

- One tokio multi-threaded runtime (already there for cs-actor).
- N actor threads (one `cs-runtime::Runtime` each).
- One discovery thread.
- One gossip thread.
- One consensus thread (if any replicated actors live here).
- One workflow worker pool.
- One agent worker pool (if any agents live here).
- One observability exporter thread (OTLP).

A **cluster** is N nodes connected over the cs-net transport,
sharing membership via gossip, and addressing each other by
`node-id@cluster-name` tuples.

Inter-node traffic is multiplexed over a single transport
connection per peer, with logical channels for:

- `control` — membership gossip, lease renewals, heartbeats
- `messages` — application actor sends
- `bulk` — code transfer, snapshot transfer
- `consensus` — Raft RPCs
- `workflow` — workflow history fan-out
- `observability` — distributed traces

This avoids head-of-line blocking inside one channel choking
another, while keeping connection count linear in cluster size.
