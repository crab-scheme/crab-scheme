# Consistency Layer — CRDT, Consensus, Leases, Fencing

Crates this spec creates: **`cs-crdt`**, **`cs-consensus`**.

Covers milestones **M05** (CRDT), **M06** (consensus), **M07** (leases
+ fencing). Detailed task lists live in `tasks/M05-crdt.md`,
`tasks/M06-consensus.md`, `tasks/M07-leases-and-fencing.md`.

## The dual-layer rule

Crab Scheme refuses to let the application mutate distributed state
without naming the consistency model:

- **CRDT path** — `(crdt/pn-counter …)` and `(define-mergeable-actor …)`.
  Mergeable, eventually-consistent, no quorum, no leader, always
  available. For "soft truth": presence, soft counters, collaborative
  state, metrics.
- **Consensus path** — `(define-replicated-actor … #:consistency
  'linearizable …)`. Linearizable, leader-replicated, quorum-required,
  CP under partition. For "official truth": balances, leases,
  membership, ordered transactional state.

These layers are *orthogonal*. A node hosts both kinds of actors
simultaneously. The choice is per-actor and visible in the source.

Reference: Akka explicitly splits Distributed Data (CRDT) from
Persistence Replicated Event Sourcing — same insight, different
naming. <https://doc.akka.io/libraries/akka-core/current/typed/distributed-data.html>

## M05 — CRDT layer (cs-crdt)

### Type catalog (v1)

| Type | Use | Invariants preserved | Anti-pattern |
|------|-----|----------------------|--------------|
| `G-Counter` | grow-only counter | sum of increments | decrements impossible |
| `PN-Counter` | counter | add + subtract | no upper/lower bound |
| `OR-Set` | observable-remove set | add wins concurrent with remove | tag GC requires causal stability |
| `OR-Map` / `Causal Map` | map of CRDTs | per-key OR-Set + embedded merge | no cross-key atomicity |
| `LWW-Register` | last-writer-wins | one converged value | concurrent writes silently dropped |
| `MV-Register` | multi-value register | all concurrent writes preserved | app picks resolution |
| `RGA-Text` (v1.1) | collaborative text | character-level merge | metadata overhead 16-32 B/char |

Default in the public Scheme API: **MV-Register**, not LWW. LWW
silently loses writes; we surface that decision rather than hide it.

Reference: Shapiro et al., "A comprehensive study of Convergent
and Commutative Replicated Data Types" —
<https://inria.hal.science/inria-00555588v1/document>

### State-based vs op-based vs delta-state

Crab adopts **delta-state CRDTs** (Almeida, Shoker, Baquero 2016)
as the wire format: each local mutation produces a small joinable
delta; deltas ship over an unreliable channel; missed deltas are
recovered by Merkle-based anti-entropy. This is what Akka, Ditto,
and Automerge converge on.

Reference: <https://arxiv.org/abs/1603.01529>

### Causality

- **Hybrid Logical Clocks (HLC)** — 64-bit `<physical_ms | logical>`
  pairs. Single source of timestamps across the runtime. Used by
  every CRDT write, by Raft commit timestamps, and by lease tokens.
  CockroachDB / YugabyteDB / MongoDB / Cassandra Accord all converge
  on HLC. Reference:
  <https://martinfowler.com/articles/patterns-of-distributed-systems/hybrid-clock.html>
- **Dotted Version Vectors (DVV)** — version vector + a single
  `<node,seq>` dot per event. Size bounded by replication factor,
  not by client count. Used as causal context on `OR-Set`, `OR-Map`,
  `MV-Register`. Reference (Almeida et al.):
  <https://gsd.di.uminho.pt/members/vff/dotted-version-vectors-2012.pdf>
- **Vector clocks** — never exposed to user code (don't scale with
  joins).
- **Lamport scalars** — internal use only (intra-process actor
  channel ordering).

### Anti-entropy + delta sync

Two layers reconcile replicas:

1. **Foreground push** — on every local mutation, push the delta
   to a small fanout set of peers over cs-net's `messages` channel.
2. **Background anti-entropy** — periodic pairwise Merkle-tree
   reconciliation. Mirror Cassandra's design: trees aren't kept
   resident; rebuilt on demand. Range mismatches cause descent.

### Tombstone GC

OR-Sets and OR-Maps accumulate tombstones; bounded growth requires
*causal stability* detection. v1 ships a `#:keep-tombstones-ms`
per-CRDT knob; aggressive GC is opt-in and requires that all
replicas have been observed within the window.

### Scheme surface

```scheme
;; Atomic counters
(define likes (crdt/pn-counter 'post:123:likes))
(crdt/inc! likes 1)
(crdt/inc! likes -1)
(crdt/value likes)              ; ⇒ integer

;; Sets
(define online (crdt/or-set 'online-users))
(crdt/add! online 'alice)
(crdt/remove! online 'alice)
(crdt/contains? online 'alice)

;; Causal maps
(define presence (crdt/causal-map 'session-state))
(crdt/map-set! presence 'alice (crdt/lww-register 'editing-doc-42))
(crdt/map-value presence)       ; ⇒ ((alice . editing-doc-42))

;; Mergeable actor (CRDT-state actor; see runtime-kernel.md):
(define-mergeable-actor user-presence
  #:state (crdt/causal-map)
  #:on-message
    (lambda (state msg)
      (case (car msg)
        ((join)  (crdt/map-set! state (cadr msg) 'online))
        ((leave) (crdt/map-remove! state (cadr msg))))))

;; Optional consistency level on writes:
(crdt/update! likes inc! #:write 'majority #:timeout 2000)
```

### v1 minimum

- Types: G-Counter, PN-Counter, OR-Set, OR-Map (Causal Map),
  LWW-Register, MV-Register.
- HLC clock + DVV causal context.
- Delta-state wire format + gossip push + Merkle anti-entropy.
- `define-mergeable-actor` form.
- Defer: RGA-text, BoundedCounter, Yjs interop, cross-region anti-entropy.

### Code pointers

- `crates/cs-table/src/lib.rs` — shared atomic tables; CRDT state
  may be stored in cs-table for queryability.
- `crates/cs-channel/src/lib.rs` — gossip transport reuses
  watermark/backpressure semantics.
- `crates/cs-runtime/src/builtins/mod.rs` — register `crdt/*`
  primops here.

### Open issues

- Bounded counters (inventory, "never go below zero") cannot be a
  pure CRDT — they require either consensus or an escrow protocol
  (AntidoteDB's BoundedCounter). v1 documents the limit; v1.1 may
  add an escrow primitive.
- Tombstone GC across cluster topology change is subtle. Defer
  aggressive GC to v1.1.

## M06 — Consensus engine (cs-consensus)

### Engine choice

**openraft** — pure async, event-driven, pluggable storage and
network, tokio-native (cs-runtime already uses tokio), powers
Databend / CnosDB / RobustMQ.

Reference: <https://github.com/databendlabs/openraft> ·
<https://docs.rs/openraft/latest/openraft/docs/getting_started/>

Alternative considered: `tikv/raft-rs` (sync API), `etcd-io/raft`
(Go). openraft wins on async fit + clear traits.

### Replicated actor surface

```scheme
;; Define a replicated state machine. The state machine is a pure
;; function (state, op) → state. No (current-time), no (random),
;; no I/O — same determinism rules as workflows (see
;; durable-execution.md).
(define-replicated-actor account-balance
  #:initial 0
  #:cluster '(node-a node-b node-c)
  #:consistency 'linearizable
  #:state-machine
    (lambda (state op)
      (case (car op)
        ((deposit)  (+ state (cadr op)))
        ((withdraw) (if (>= state (cadr op))
                        (- state (cadr op))
                        state)))))

;; Submit a command — blocks until majority commit.
(replicated-actor-call! account-balance '(deposit 100))

;; Linearizable read via ReadIndex.
(replicated-actor-read! account-balance)
```

### Joint consensus for membership

Membership changes pass through a transitional `C_old,new`
configuration where commits require quorums in *both* old and new
configurations. Rules out the classic two-disjoint-majorities split
brain. Reference:
<https://www.cockroachlabs.com/blog/joint-consensus-raft/>

### Snapshot policy

- Snapshot every N committed entries (configurable, default 10000)
  OR every M bytes of log growth (default 64 MB).
- Snapshots flow over cs-net's `bulk` channel (low priority,
  doesn't choke message traffic).
- The snapshotter must serialize Scheme state — use cs-runtime's
  fasl-like format (the same used for AOT'd code modules).

### v1 minimum

- openraft-backed `replicated-actor` form.
- Linearizable reads via ReadIndex.
- Joint-consensus membership.
- Snapshot/restore.
- Defer: lease-based reads (v1.1 perf), VR backend, EPaxos.

### Open issues

- **Determinism enforcement.** Replicated state machines must be
  pure functions. v1 ships a static analyzer pass that refuses
  `#:state-machine` bodies referencing forbidden bindings
  (`current-time`, `random`, network I/O). Same analyzer reused
  by workflows. See language.md § effects.
- **Snapshot format stability.** Replays after upgrade must
  deserialize old snapshots. Pin the format with a version byte
  and ship migration code per format bump.

## M07 — Leases + fencing tokens

The single hardest primitive to get right.

### Why leases alone are unsafe

A leaseholder pause (GC, scheduler stall, swap) can extend past the
TTL. The holder believes it still owns the lease and writes, but
the lease has expired and a successor has begun writing too.
**Result: two concurrent writers, corruption.**

The fix is a **monotonically increasing fencing token** issued
*atomically with the lease grant*. Every protected operation
includes the token; the resource validates it monotonically and
rejects stale ones.

Reference (Kleppmann):
<https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>

### Required primitives

Three guarantees the runtime must expose:

1. **Strictly monotonic token issuance.** The lease-grant op
   returns `(lease-id, fencing-token, deadline-hlc)` where
   `fencing-token` is monotonic per-resource across all grants
   (in time, not just per-leaseholder).
2. **Fencing-aware writes.** Every `replicated-actor-call!` and
   every CRDT mutation may carry an optional `#:fence
   <token>`; the runtime rejects stale tokens with a
   `&fenced` condition.
3. **Deadline is advisory.** The HLC deadline tells the
   leaseholder when to renew. It is **not** the safety primitive;
   the token is.

### Scheme surface

```scheme
(define lease
  (lease-acquire! 'email-sender
                  #:ttl-ms 30000))

(lease-token lease)            ; ⇒ monotonic integer (e.g. 42)
(lease-deadline lease)         ; ⇒ HLC timestamp

(lease-renew! lease)           ; bumps deadline, same token
(lease-release! lease)         ; voluntary release

;; Fence-protected write to a replicated actor:
(replicated-actor-call! shard-3-actor
                        '(update key val)
                        #:fence (lease-token lease))
```

### Implementation

Leases are themselves a replicated actor — a Raft group whose state
machine is `{ lease-name → (current-holder, fencing-token,
deadline-hlc) }`. Acquire/renew/release are commands; grants happen
on the leader; tokens are bumped monotonically per-lease.

### v1 minimum

- Lease primitives + monotonic fencing tokens.
- `#:fence` keyword on replicated-actor-call! and on cluster
  membership writes.
- HLC-based TTL deadline (advisory).
- Defer: Spanner-style TrueTime-bounded uncertainty; v1 documents
  "deadline is hint, token is safety."

### Code pointers

- Built atop `cs-consensus::ReplicatedActor` (the lease state
  machine is itself a Raft group).
- `crates/cs-actor/src/lib.rs` — supervision plumbing for
  leaseholders.

### Open issues

- **Renewal races.** Two acquirers can race; serialization happens
  via the consensus log, not local locks.
- **Per-resource tokens.** Tokens are scoped per resource name.
  Reusing a token across resources is rejected by the runtime.
- **Document loudly: token, not deadline.** v1 docs must include
  the Kleppmann diagram and a worked example of the failure mode.

## External references (consolidated)

- Shapiro et al., CRDT survey — <https://inria.hal.science/inria-00555588v1/document>
- Almeida et al., Delta CRDTs — <https://arxiv.org/abs/1603.01529>
- Almeida et al., DVV — <https://gsd.di.uminho.pt/members/vff/dotted-version-vectors-2012.pdf>
- Automerge — <https://github.com/automerge/automerge>
- Yjs — <https://github.com/yjs/yjs>
- Riak Datatypes — <https://docs.riak.com/riak/kv/latest/developing/data-types/>
- AntidoteDB — <https://github.com/AntidoteDB/antidote>
- Akka Distributed Data — <https://doc.akka.io/libraries/akka-core/current/typed/distributed-data.html>
- HLC pattern — <https://martinfowler.com/articles/patterns-of-distributed-systems/hybrid-clock.html>
- Raft paper — <https://raft.github.io/raft.pdf>
- openraft — <https://github.com/databendlabs/openraft>
- CockroachDB joint consensus — <https://www.cockroachlabs.com/blog/joint-consensus-raft/>
- VR Revisited — <http://pmg.csail.mit.edu/papers/vr-revisited.pdf>
- EPaxos page — <http://efficient.github.io/epaxos/>
- Apache Cassandra Accord (CEP-15) — <https://cwiki.apache.org/confluence/display/CASSANDRA/CEP-15>
- Kleppmann on distributed locks — <https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>
- Spanner TrueTime — <https://docs.cloud.google.com/spanner/docs/true-time-external-consistency>
