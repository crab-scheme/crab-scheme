# Runtime Kernel — actors, supervision, registries (existing + extensions)

Crates: **`cs-actor`** (shipped), **`cs-supervisor`** (shipped),
**`cs-hotreload`** (shipped). This document covers what's already in `main`
and the extensions needed for the distributed runtime (M02-M04).

## Already shipped

The BEAM-style actor runtime in `main`:

| Crate | Surface |
|-------|---------|
| `cs-actor` | `ActorSystem`, `ActorRef`, `Mailbox` (Fast \| Durable), PID allocator, registry, reduction-counted preemption, work-stealing scheduler |
| `cs-supervisor` | `one_for_one`, `one_for_all`, `rest_for_one` strategies, restart intensity limits |
| `cs-hotreload` | two-version dispatch, state migration via registered functions |
| `cs-table` | ETS-style shared atomic tables, OrderedSet, durable mailbox backing |
| `cs-channel` | MPMC, broadcast, rendezvous channels, `(select …)` |

Scheme surface in `lib/beam/prelude.scm`:

- `(spawn thunk)` / `(self)` / `(send pid msg)` / `(receive …)`
- `(monitor pid)` / `(demonitor ref)` (single-node)
- `(link pid)` / `(unlink pid)`
- `(define-behavior …)` (gen_server-shaped)
- `(supervisor …)` form with child specs
- `(register name pid)` / `(whereis name)` / `(unregister name)`
- `(pg-join group pid)` / `(pg-broadcast group msg)` / `(pg-leave group)`
- `(define-state-migration v1->v2 thunk)`

Reference: `docs/research/beam_runtime_spec.md` (the shipped design).

## Extensions for distributed runtime (M02-M04)

### `ActorRef` trait abstraction

Today `ActorRef` is a concrete `cs-actor::LocalActorRef`. M02 adds:

```rust
trait ActorRef: Send + Sync {
    fn send(&self, msg: Message) -> Result<(), SendError>;
    fn pid(&self) -> Pid;
    fn is_local(&self) -> bool;
}
struct LocalActorRef { /* cs-actor */ }
struct RemoteActorRef { /* cs-distrib */ }
```

Scheme code calling `(send pid msg)` doesn't change — the dispatch
picks `Local` or `Remote` based on whether `(pid-node pid)` is
`(node-self)`.

### Distributed monitor/link

Local monitors send `{down, ref, pid, reason}` immediately when the
target dies. Distributed monitors hook into the membership layer
(M04): when a peer transitions to `down` state, every monitored
remote Pid on that peer fires DOWN with reason `'noconnection`.

### Distributed registry (process groups)

Local `pg-broadcast` iterates a `cs-table::OrderedSet`. For
distributed pg, the registry becomes a **CRDT OR-Set** keyed by
group name (M05). Adding/removing PIDs is local; gossip
propagates; broadcasts iterate the per-node slice.

### Code pointers

- `crates/cs-actor/src/lib.rs` — single-file crate (~3k lines); `ActorRef`, `Mailbox`, `ActorSystem`.
- `crates/cs-supervisor/src/lib.rs` — supervisor primitives.
- `crates/cs-channel/src/lib.rs` — channels (used as signal transport for workflows in M08).
- `crates/cs-hotreload/src/lib.rs` — version-aware dispatch.
- `lib/beam/prelude.scm` — Scheme-level macros for behaviors, supervisors, monitors.

## Open issues for v1 distributed extension

- **PID encoding includes node-id + epoch.** Today PIDs are local. M02 must extend the PID type to carry node-id and rev/epoch so a stale PID from a restarted peer is identifiable.
- **Per-Pid monitor ledger.** Currently per-actor monitors live in the actor's own state. Distributed monitors need a node-level ledger so that "this peer just went down" can fan out DOWN messages to N actors per peer departure.
- **Backpressure for remote sends.** Local sends use `tokio::sync::mpsc` watermarks. Remote sends must impose backpressure when the per-peer transport queue fills, surfaced as `'send-pressure` to the caller.

See `distributed.md` for the M02+ deep dive.
