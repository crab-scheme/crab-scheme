# M06 — Consensus engine (cs-consensus)

**Crates created:** `cs-consensus`.
**Effort:** 5-7 iters (Raft is hairy).
**Depends on:** M02 (transport).
**Engine:** `openraft`.

## Goal

`(define-replicated-actor …)` form lowered to an openraft Raft
group with linearizable reads via ReadIndex, joint-consensus
membership change, snapshot/restore.

## Acceptance

- 3-node Raft group: deposit/withdraw on account-balance survives 1-node-down + 2-node-partition (CP).
- Membership change (add/remove node) via joint consensus; no two-disjoint-majorities path.
- Snapshots flow over cs-net `bulk` channel; restore on new replica.
- Determinism enforcement on `#:state-machine` bodies (effect-set check rejects `io`/`net`/`wall-clock`/`random`).

## Iters

### A — openraft integration scaffold

- `cs-consensus::RaftGroup` wrapping `openraft::Raft<Config>` for our `Config` type.
- Plumb `RaftNetwork`, `RaftLogStorage`, `RaftStateMachine` traits through cs-net + cs-table.
- **Code:** new `crates/cs-consensus/src/group.rs`.

### B — `define-replicated-actor` macro

- Expands to a `RaftGroup` registration + a wrapper actor that dispatches `(replicated-actor-call! …)`.
- State machine body validated via M01 effect-check.
- **Code:** `lib/beam/prelude.scm` + new cs-runtime builtin.

### C — ReadIndex for linearizable reads

- `(replicated-actor-read! a)` issues a ReadIndex query; returns leader's committed value.
- **Code:** `cs-consensus::read_index`.

### D — Joint-consensus membership change

- `add-replica!`, `remove-replica!` primitives.
- Transitional `C_old,new` config required for commits during change.

### E — Snapshot + restore

- Serialize Scheme state via `cs-runtime`'s fasl-like format (extend if not present).
- Snapshot trigger: every N entries or M bytes (configurable).
- Stream over cs-net `bulk` channel.

### F — Conformance + chaos tests

- 3-node group, deliberate kills + partitions, observe correct CP behavior.
- Property test: any sequence of `(deposit …) | (withdraw …)` yields a serializable history.

## Example

```scheme
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

;; Commits via majority quorum:
(replicated-actor-call! account-balance '(deposit 100))   ; ⇒ 100
(replicated-actor-call! account-balance '(withdraw 30))   ; ⇒ 70

;; Linearizable read:
(replicated-actor-read! account-balance)                  ; ⇒ 70
```

## External refs

- Raft paper — <https://raft.github.io/raft.pdf>
- openraft — <https://github.com/databendlabs/openraft>
- openraft getting started — <https://docs.rs/openraft/latest/openraft/docs/getting_started/>
- Joint consensus (CockroachDB) — <https://www.cockroachlabs.com/blog/joint-consensus-raft/>
- Embedded openraft case (Danube) — <https://dev-state.com/posts/migrate_danube_etcd_to_raft/>

## Code pointers

- `crates/cs-net/src/lib.rs` — `consensus` logical channel.
- `crates/cs-table/src/lib.rs` — Raft log backing.
- `crates/cs-runtime/src/lib.rs` — fasl-like serializer (extend).
