# foundations-b worktree

Phase B of the SDK spec — the distributed substrate. See
`docs/research/sdk_spec/roadmap.md` for the full milestone graph
and the per-milestone task lists at
`docs/research/sdk_spec/tasks/M0{2,3,4}-*.md`.

Branch: `foundations-b` off `main`.

## What's here vs main

```
crates/
  cs-net/         <-- scaffold (M02 transport substrate)
  cs-distrib/     <-- scaffold (M02 transport, M04 membership)
  cs-discovery/   <-- scaffold (M03 pluggable providers)
```

Each crate has a stub `lib.rs` with the public API sketched per the
spec, plus a `Cargo.toml` wired into the workspace. Nothing
implements anything yet — the scaffolds exist so the workspace
builds with the new module layout and we can land each phase as a
focused PR without re-litigating crate boundaries.

This is the same approach `BEAM_WORKTREE.md` took for the
actor/table/supervisor/hotreload scaffolds that became the BEAM v1
runtime in `main`.

## Active task ladder

| Milestone | Iters | Status |
|-----------|-------|--------|
| M02 — Distributed actor transport | A-F (6) | A scaffolded; B-F pending |
| M03 — Discovery providers | A-E (5) | trait scaffolded; impls pending |
| M04 — Membership + failure detection | A-D (4) | scaffolded; impls pending |

Detailed sub-task breakdowns live at:
- `docs/research/sdk_spec/tasks/M02-cluster-substrate.md`
- `docs/research/sdk_spec/tasks/M03-discovery.md`
- `docs/research/sdk_spec/tasks/M04-membership.md`

## API surface locked in by the scaffolds

**cs-net** (`crates/cs-net/src/lib.rs`):
- `Channel { Control, Consensus, Messages, Workflow, Bulk, Observability }` — six logical traffic classes multiplexed over one transport.
- `Transport` trait with `send / peer_label / close`.
- `TransportConfig` with per-channel high-watermarks.
- `TransportError` with `PeerClosed / Backpressure / Handshake / Tls / Io / NotImplemented`.
- Module stubs: `sim::SimPair`, `tcp::TcpTransport`, `quic::QuicTransport` (feature-gated).

**cs-distrib** (`crates/cs-distrib/src/lib.rs`):
- `NodeId { name, host, epoch }` — `epoch` distinguishes restart incarnations.
- `DistribError` with epoch-mismatch + transport-wraps.
- `membership::{MemberState, PartitionPolicy, SbrConfig, Member}` — 8-state machine + 5 SBR strategies.
- `phi::PhiAccrualFailureDetector` — sliding-window heartbeat tracking (real distribution fit in M04 iter B).
- `gossip::{GossipConfig, GossipMessage}` — SWIM-shaped protocol envelope.

**cs-discovery** (`crates/cs-discovery/src/lib.rs`):
- `DiscoveryProvider` trait — one async `lookup(Lookup, Duration) -> Result<Resolved, DiscoveryError>`. Modeled on `akka.discovery.ServiceDiscovery`.
- `Lookup`, `Resolved`, `ResolvedTarget`, `Protocol`.
- `FirstSuccess` combinator with fall-through-on-error semantics.
- Concrete providers behind features: `static`, `file`, `dns`, `kubernetes`, `db-postgres`, `db-sqlite`, `etcd`, `consul`, `nomad`, `aws-cloudmap`, `gcp-instance-groups`, `azure-vmss`, `mdns`, `gossip`.

## Build + test (sanity)

```bash
cargo build -p cs-net -p cs-distrib -p cs-discovery
cargo test  -p cs-net -p cs-distrib -p cs-discovery
```

Current state: **18 tests pass** (3 cs-net + 11 cs-distrib + 4 cs-discovery). Full workspace `cargo build` succeeds — the new crate boundary doesn't break any existing consumer.

## Workflow

This worktree is the *scaffold* layer of Phase B. Each milestone
(M02, M03, M04) is then intended to ship as a focused follow-up
worktree (`worktree-m02-transport`, `worktree-m03-discovery`,
`worktree-m04-membership`) so the implementation work can land in
reviewable chunks without churning the crate boundaries again.
