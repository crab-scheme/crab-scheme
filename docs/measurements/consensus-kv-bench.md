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

## To make this a fair throughput benchmark

1. **Model network latency** in the sim (per-message delay) — then EPaxos's
   fewer round trips / leaderless parallelism become visible.
2. **Hash-table state machine** (remove the O(n) alist) for honest absolute
   numbers.
3. **Run over the real cs-net transport** (Sim/TCP/QUIC) once the cluster
   send/recv primops are wired — measures the actual networked path.
4. **JIT tier** instead of the tree-walker for the protocol compute.
