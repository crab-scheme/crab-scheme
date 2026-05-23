# KV cache on consensus — throughput measurements (Raft vs EPaxos)

**Harness:** `lib/consensus/bench.scm` — `crabscheme run lib/consensus/bench.scm`.

## What is (and isn't) measured — read this first

These numbers are the **protocol-logic compute cost** of committing writes
through the deterministic in-process cluster simulator, executed by the
CrabScheme tree-walker. There is:

- **no real network** (messages are routed in-memory, zero latency),
- **no disk / fsync**,
- **no concurrency** (single-threaded, one write at a time),
- and the state machine + engine use **association lists (O(n) per op)**.

So this is **not** comparable to production distributed-KV throughput, and the
absolute writes/sec are not the point. Two things here *are* meaningful:

1. **Raft vs EPaxos on the identical harness** — same interpreter, same KV, same
   sim — a fair *relative* comparison of per-write protocol work.
2. **Round-trips per commit** (below) — the protocol property that actually
   governs real-world throughput/latency.

## Results (release CLI, tree-walker, M3-class macOS)

| N writes | Raft (distinct keys) | EPaxos (distinct, fast-path) | EPaxos (same key, conflicting) |
|---------:|---------------------:|-----------------------------:|-------------------------------:|
| 20 | 4189 w/s | 2127 w/s | 1790 w/s |
| 40 | 4074 w/s | 1455 w/s | 1070 w/s |
| 80 | 2747 w/s | 674 w/s | 450 w/s |

Two effects are visible:
- **Throughput falls with N** — the O(n) association-list state machine + log
  `append` + alist growth make the *sim* roughly O(N²). That's the draft's data
  representation, not the algorithms; a hash-table state machine removes it.
- **Raft is faster than EPaxos here, and EPaxos degrades under conflict.**

## Why Raft "wins" in this sim — and why that's misleading

EPaxos does **more compute per command** (dependency computation scans prior
commands, fast/slow-path bookkeeping, dependency-graph execution), so in a
**zero-latency compute simulation** that extra work makes it slower. EPaxos's
real advantage is **not** compute — it's the network:

| | round trips / commit | bottleneck |
|---|---|---|
| **Raft** | 1 (leader → followers → leader) | a single leader serializes all writes |
| **EPaxos fast path** (no conflict) | 1 (any replica → fast quorum → it) | **none** — every replica leads in parallel |
| **EPaxos slow path** (conflict) | 2 | the command's leader |

On a real network EPaxos turns "no single leader" into higher aggregate
throughput and lower tail latency *under low conflict* — exactly what our
sim cannot show because it has no latency to amortize and no parallelism.
Under high conflict EPaxos falls back to 2 round trips and loses that edge,
which our `conflicting` column mirrors directionally (450 vs 674 w/s).

## Context: what real consensus KV stores do (different measurement!)

Real systems measure **network + disk + concurrent clients**, so their numbers
are orders of magnitude higher and not comparable to the compute figures above
— included only for orientation:

- **etcd (Raft), 3-node cloud cluster:** **>30,000 writes/sec** modern
  (historically ~5,000/sec); "limited by network IO and disk IO latency."
  [etcd performance docs](https://etcd.io/docs/v3.5/op-guide/performance/),
  [benchmarks](https://etcd.io/docs/v3.6/benchmarks/etcd-3-demo-benchmarks/)
- **Multi-Paxos / Raft, WAN synchronous:** ~**28,000 req/sec**; **EPaxos at 2%
  conflict ≈ 2.7× Multi-Paxos throughput**, with the advantage shrinking as
  conflict rises (the same conflict-sensitivity our numbers show).
  [Performance Comparison of Paxos and Raft](https://www.diva-portal.org/smash/get/diva2:1471222/FULLTEXT01.pdf),
  [Reproducing EPaxos (Princeton)](https://medium.com/princeton-systems-course/reproducing-epaxos-by-ang-li-and-robin-qiu-b5a8fc5262a2)

The literature's headline — *EPaxos beats leader-based consensus under low
conflict, loses the edge under high conflict* — is a **network/round-trip**
result. Our compute sim can't reproduce the win (no latency) but does reproduce
the conflict-sensitivity.

## Latency model — where EPaxos's advantage actually shows

`lib/consensus/latency-sim.scm` — `crabscheme run lib/consensus/latency-sim.scm`.

EPaxos's win is a **round-trip** effect, invisible in the zero-latency compute
bench. This sim **measures** the number of sequential message rounds to commit a
write (BFS over the real engines' message-causality graph), then expresses
commit latency as `rounds × L` (one-way network delay). Raft has no
forward/notify messages in our engine, so for a non-leader origin we measure the
leader's round-trip commit (2 rounds) and *add* the two hops Raft genuinely
needs — forward (origin→leader) and notify (leader→origin).

Measured: **both** engines commit in **2 message rounds** (one round trip) — but
Raft only when the write originates at the leader; **EPaxos achieves it from any
origin** (any replica leads its own command).

| write origin (3-node) | Raft | EPaxos |
|---|---|---|
| leader | 2L | 2L |
| follower-1 | 4L | 2L |
| follower-2 | 4L | 2L |
| **mean (uniform origins)** | **3.33L** | **2L** |

At a WAN `L = 50 ms`: **mean commit latency Raft ≈ 167 ms vs EPaxos 100 ms** (~40%
lower). Throughput axis: Raft's single leader coordinates *all* M writes (a
bottleneck); EPaxos spreads coordination ~M/3 across replicas (≈3× headroom at
low conflict) — matching the literature's "no leader bottleneck" / 2.7×-at-2%-
conflict result. Under conflict EPaxos falls to 2 round trips and loses the edge.

## Hash-table state machine (#2)

`bench.scm` also runs an O(1) **hash-table** state machine (R6RS mutable
`make-hashtable`, a fresh table per replica) beside the pure alist. A mutable
hashtable isn't a pure value (Article II), so it's **benchmark-only** — the
library KV (`kv-cache.scm`) stays pure-alist; this just isolates SM cost from
engine cost. Release numbers:

| N | Raft alist → ht | EPaxos alist → ht |
|--:|--|--|
| 20 | 5141 → 6321 w/s (+23%) | 2470 → 2693 (+9%) |
| 40 | 4119 → 5027 w/s (+22%) | 1274 → 1586 (+24%) |
| 80 | 2716 → 3576 w/s (+32%) | 629 → 708 (+13%) |

The hash-table SM helps Raft's distinct-key path most; EPaxos benefits least
because its cost is the **dependency machinery**, not the SM.

## Pure persistent-map state machine (#1 — `lib/consensus/pmap.scm`)

CrabScheme ships only *mutable* R6RS hashtables, so the hash-table SM above
violates Article II (a pure value). `pmap.scm` adds a **persistent (immutable)
ordered map** — a treap balanced by `equal-hash` priorities — giving an
**O(log n) PURE** state machine: each replica keeps its own immutable snapshot,
no mutation. The library KV (`kv-cache.scm`, `epaxos-kv.scm`) now uses it.

Release numbers (pure pmap SM), with the engine's unbounded-`aset` leak fixed:

| N | Raft (distinct, pmap) | EPaxos (distinct, pmap) |
|--:|--:|--:|
| 20 | 2894 w/s | 1984 w/s |
| 40 | 2549 w/s | 1450 w/s |
| 80 | 2256 w/s | 969 w/s |
| 160 | 1884 w/s | 590 w/s |

Reading the scaling: **Raft is nearly flat** (2894→1884 over an 8× N increase —
the per-write cost barely grows), so its remaining list-`append` log is not the
bottleneck in range. **EPaxos still falls ~O(N²)** (1984→590) because
`deps-and-seq` must compare each new command against every prior one — that scan
is **inherent** to EPaxos's general interference model (a key-indexed variant
would flatten the *non-conflicting* case, at the cost of the generic
`interferes?` predicate). pmap ≈ alist at these N (treap overhead vs alist's
O(n)); pmap pulls ahead as N grows, and stays pure — the point.

Also fixed here: node state and the cluster map used a **shadow-cons** alist that
**grew unbounded over a node's lifetime** (a real leak for a long-running
replica); both now do a proper non-growing replace (`aset`/`cluster-set`), a
small constant cost at low N for bounded memory at scale.

## Actor-driven cluster (real concurrency) — `spawn-source`

The benchmarks above run the engines through the deterministic in-memory
*simulator* (one thread, explicit `settle`). The same pure engine now also runs
as **real parallel actors**, each replica on its own OS thread, coordinating
over live mailboxes — no sim:

- `lib/consensus/raft-actor-body.scm` — the replica actor body: it `(raw-receive)`s
  messages and `(send)`s the engine's outputs to peer actors, pumping the exact
  pure transitions from `raft.scm`.
- `lib/consensus/raft-cluster.scm` — `crabscheme run lib/consensus/raft-cluster.scm`
  spawns 3 replicas, elects a leader, replicates 3 writes, and asserts every
  replica's state machine converged (commit index `(3 3 3)`, `user:1 = alice`
  on a/b/c). Verified stable across repeated runs.

This is unblocked by **`spawn-source`** (cs-runtime), the bridge that lets a
Scheme procedure *be* an actor body. A Scheme `Value` is `Rc`-based and so
`!Send`, but an actor body must be `Send` (it runs on a worker of the
multi-thread runtime), so `(spawn (lambda …))` is impossible; `spawn-source`
instead ships the body as source + an entry name and rebuilds it on the actor's
own thread. Consensus *logic stays Scheme* (Constitution Article I) while
honoring the runtime's thread model. PIDs round-trip through messages as
printable symbols, so replicas address each other directly.

## Cross-node cluster over cs-net

The actor cluster above shares one process's mailboxes. The next step out is a
real cluster transport: replicas that exchange messages between **nodes**, each
Raft RPC serialized, framed, and routed — the cs-net / cs-distrib stack.

cs-runtime's `distrib` feature exposes builtins driving cs-distrib's
synchronous `Router` over a cs-net `Transport`:

- `(node-make NAME)` — a node (Router) named NAME.
- `(node-link! A B)` — connect two nodes with the in-memory **sim** transport.
- `(node-listen NODE ADDR)` / `(node-connect NODE PEER-ADDR)` — plaintext **TCP**.
- `(node-listen-tls …)` / `(node-connect-tls …)` — **TCP + mutual TLS**: a real
  TLS 1.3 handshake (both nodes present + verify a cert; all traffic encrypted)
  before any consensus traffic.
- `(node-listen-quic …)` / `(node-connect-quic …)` — **QUIC**: TLS 1.3 mandatory
  (always encrypted + mutually authenticated) and one stream per logical
  `Channel`, so a stalled `Bulk` transfer can't head-of-line-block `Control`.
- A length-prefixed cs-distrib `Hello` handshake (over the `Control` channel,
  so it is identical across all four transports) exchanges NodeIds after any TLS
  handshake; socket I/O runs on cs-actor's tokio runtime.
- `(node-peer-count NODE)` — registered peers (socket peers register
  asynchronously on the accepting side, so a bootstrap waits on this).
- `(node-send FROM TO MSG)` — MSG crosses as data: Scheme value →
  `SendableValue` → a compact byte frame → `Router.send` (framed `DistPid ‖
  payload`).
- `(node-poll NODE)` — pump NODE's transports and return the decoded messages.

mTLS/QUIC use cs-net's shared self-signed **dev** identity (one cert as identity
+ root on every node, behind cs-net's `dev-certs` feature) — enough to run the
real handshake in-process; a production cluster loads per-node certs from a CA.
Node identity proper stays the cs-distrib `Hello` (NodeId), so distinct nodes
remain distinguishable regardless of the shared transport cert.

The replica body `raft-net-body.scm` is transport-agnostic (only
`node-send`/`node-poll`), so the **same** 3 replicas run over every transport:

| transport | demo | stress |
|---|---|---|
| sim (in-memory) | `raft-net.scm` | 12/12 |
| real TCP | `raft-net-tcp.scm` | 10/10 |
| TCP + mutual TLS | `raft-net-tls.scm` | 8/8 |
| QUIC (TLS 1.3) | `raft-net-quic.scm` | 8/8 |

Each elects a leader, replicates 3 writes, and converges (commit `(3 3 3)`,
`user:1 = alice` on a/b/c). Rust tests prove each transport's socket round-trip
directly: `two_nodes_send_and_poll_over_{real_tcp,mtls,quic}` (+ sim + codec) —
8 distrib tests.

## Remaining for a production benchmark

1. ~~Model network latency~~ — done (`latency-sim.scm`).
2. ~~Hash-table state machine~~ / ~~pure O(log n) state machine~~ — done
   (`pmap.scm`, Article II intact).
3. **EPaxos dependency index** — key-bucket `cmds` to flatten the
   non-conflicting `deps-and-seq` scan (couples to key-based interference).
4. **Run over real transport** — done: ~~actor-driven in-process~~
   (`raft-cluster.scm`), ~~cross-node over sim~~ (`raft-net.scm`), ~~real TCP~~
   (`raft-net-tcp.scm`), ~~mutual TLS~~ (`raft-net-tls.scm`), and ~~QUIC~~
   (`raft-net-quic.scm`). Remaining for production: load **per-node certs from a
   CA** instead of the shared dev self-signed identity.
5. **JIT tier** instead of the tree-walker for the protocol compute.
