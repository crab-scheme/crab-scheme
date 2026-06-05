# M06 — Consensus engines (cs-consensus): exit report

**Crate:** `cs-consensus`. **Branch:** `feat/sdk-consensus`.
**Builds on:** M02 (cs-net transport + `Channel::Consensus`), cs-actor.
**Spec:** `docs/research/sdk_spec/tasks/M06-consensus.md`,
`docs/research/sdk_spec/consistency.md`.

Two replication ("orchestration") engines built **on top of our own
networking and actors** — homegrown, not openraft. Both are deterministic,
I/O-free cores driven by a shared cluster simulator, then wired over cs-net.

## Deviations from the M06 spec (intentional)

- **Homegrown, not openraft.** The spec named `openraft`; the request was to
  build *on top of our networking and actors*. openraft brings its own
  network/storage abstractions, so a from-scratch core over cs-net /
  cs-actor (the same deterministic, Sim-tested style as M02) was the right
  fit. The `Channel::Consensus` logical channel the spec reserved is exactly
  what the driver uses.
- **EPaxos included.** The spec deferred EPaxos; it was explicitly requested
  alongside Raft, so a leaderless EPaxos core ships too.

## Architecture: deterministic core + thin I/O shim

Each engine's protocol is a **pure synchronous state machine**: it consumes a
logical tick / inbound message / client proposal and returns the messages to
send (state observed via accessors). No clocks, sockets, or tasks in the core.
The whole protocol is exercised by `sim::Cluster` — logical ticks, FIFO
delivery, `isolate`/`heal` partitions — with zero wall-clock flakiness. The
networking/actor layer is a thin pump (`RaftDriver` / `EpaxosDriver`,
`spawn_raft_actor`).

## Raft — `raft.rs` (near-production)

| Capability | Notes |
|---|---|
| Leader election | terms, randomized timeouts, RequestVote, learners don't campaign |
| Log replication | AppendEntries, log-matching + conflict back-off |
| Commit / apply | majority + current-term safety rule (§5.4.2); no-op commit barrier on election |
| Linearizable reads | ReadIndex (§6.4) — quorum-confirmed heartbeat barrier, no log append |
| Log compaction | snapshots + `InstallSnapshot`; base-relative log |
| Membership change | **joint consensus** `C_old,new` → `C_new`; quorum of both halves |

## EPaxos — `epaxos.rs` (core)

Leaderless: any replica leads a command in instance `(replica, slot)`.
PreAccept computes deps (interfering instances) + seq; **fast path** commits
in one round when the fast quorum returns the leader's deps/seq unchanged;
otherwise the **slow path** runs Accept over the union deps / max seq to a
majority. **Execution** is in dependency order: a fixpoint selects executable
committed instances, iterative **Tarjan SCC** runs components in
reverse-topological order, ties broken by `(seq, instance)`. Because every
replica commits identical `(seq, deps)`, all replicas execute interfering
commands in the same order.

## On top of our networking and actors

- `codec.rs` — hand-rolled wire codecs for every Raft **and** EPaxos message
  (no serde, matching cs-net/cs-distrib style).
- `driver.rs` — `RaftDriver` / `EpaxosDriver`: own a node + one cs-net
  `Transport` per peer, encode/route over `Channel::Consensus`, drain inbound.
  Work over any transport (Sim / TCP+mTLS / QUIC).
- `spawn_raft_actor` — runs a driver inside a **cs-actor** task: a timer
  drives tick/poll; `RaftCommand`s arrive via the mailbox. (Same pattern
  applies to EPaxos.)

## Test coverage (20 tests, clippy-clean)

- **Raft:** single/3-node election; majority commit; follower catch-up;
  leader-failover re-election; 5-node minority-partition cannot commit (CP);
  ReadIndex (value + NotLeader-on-follower); snapshot recovery of a
  from-start-isolated follower; add-a-node via joint consensus.
- **EPaxos:** non-interfering commands commute; concurrent interfering
  commands get one consistent order everywhere; dependency-chain causal
  order; 5-node consistency.
- **Networking/actors:** Raft agreement over the cs-net Sim transport; a Raft
  group running as cs-actor actors converges; EPaxos consistent order over
  cs-net. Codec round-trip + truncation tests.

## Deferred (documented)

- **EPaxos explicit-prepare recovery** of a failed command leader (ballots are
  carried but always 0). The happy path + fast/slow/execution are complete.
- **Raft:** lease-based reads (ReadIndex ships); pre-vote; leader step-down
  when removed from `C_new`.
- **Scheme surface** (`define-replicated-actor`, `replicated-actor-call!/read!`)
  + determinism effect-check on `#:state-machine` bodies — runtime
  integration, layered on these engines next.
- **Snapshot format versioning** for cross-upgrade replay.
