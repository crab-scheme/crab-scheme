# Distributed Runtime — discovery, transport, membership, sharding

Crates this spec creates: **`cs-distrib`**, **`cs-discovery`**, **`cs-net`**.

This document covers M02 (transport), M03 (discovery), M04
(membership + failure detection), and the sharding/placement
sub-section of the agentic runtime. The detailed task lists live
in `tasks/M02-cluster-substrate.md`, `tasks/M03-discovery.md`, and
`tasks/M04-membership.md`.

## Goals

| # | Goal | Acceptance |
|---|------|------------|
| D1 | An actor on node A can `(send pid msg)` to a pid on node B with the same syntactic form as a local send. | E2E test: 3-node cluster, ping/pong across all pairs. |
| D2 | Failed peers are detected within p99 ≤ 5 s on a 30-node cluster. | Soak test with deliberate kills; phi-accrual + SWIM heartbeats. |
| D3 | A node restart re-joins the cluster automatically via the configured discovery provider. | k8s + DNS-SRV + static-seeds all in CI. |
| D4 | Network partitions don't silently corrupt state. | "Keep majority" SBR drains minority; manual override is available. |
| D5 | Sharded entities migrate during cluster topology change without losing in-flight work (drop-and-recreate with persistence). | E2E test: rebalance a 64-shard counter actor; verify counts. |
| D6 | Distribution is observable — every remote send carries a trace context, every membership transition is a structured event. | OTLP exports verified. |

## Node identity

A **Node ID** is a triple `(name, host, epoch)`:

- `name` — the cluster-local name (`api-1`, `worker-7`).
- `host` — the network endpoint hint (resolved by discovery, not authoritative).
- `epoch` — a 64-bit monotonic counter, bumped on every node restart. The epoch is what distinguishes a freshly-restarted node from its previous incarnation — any Pid carrying the old epoch is rejected by the new instance.

A **Pid** is then `(node-id, local-id)` where `local-id` is the in-process actor id from `cs-actor`. Pids are self-describing — a remote Pid sent over the wire is sufficient to route a reply back without consulting any directory.

Scheme surface:

```scheme
(node-self)              ; → #<node api-1@cluster epoch=42>
(node-id-name nid)       ; → 'api-1
(node-id-epoch nid)      ; → 42
(pid-node pid)           ; → #<node …> for any Pid (local or remote)
```

## M02 — Distributed actor transport (cs-distrib)

### Architecture

```
                    ┌──────────────────────────────┐
                    │ Scheme: (send pid msg)        │
                    └──────────────┬───────────────┘
                                   │
                    ┌──────────────┴───────────────┐
                    │ ActorRef trait                │
                    │  - LocalRef (cs-actor)        │
                    │  - RemoteRef (cs-distrib)     │
                    └──────────────┬───────────────┘
                                   │ if remote
                    ┌──────────────┴───────────────┐
                    │ Router (per-peer connection)  │
                    │  + logical channel demux      │
                    └──────────────┬───────────────┘
                                   │
                    ┌──────────────┴───────────────┐
                    │ cs-net Transport              │
                    │  TCP+TLS / QUIC / Sim         │
                    └──────────────────────────────┘
```

### Pid encoding (wire format)

```
PID := <NodeId> <LocalId>
NodeId := <name:string> <epoch:u64> <addr-hint:string>
LocalId := <u64>
```

Atoms (symbols) get a per-connection atom-cache (BEAM-style) — most messages carry symbol-heavy payloads (record tags, message tuple heads) and the cache pays for itself by ~3 messages. The cache is bounded (default 4096 entries) and uses a Bloom filter for cache miss avoidance.

### Logical channels over one transport

One transport connection per peer pair, multiplexed into logical channels:

| Channel | Priority | Reordering tolerated? |
|---------|----------|-----------------------|
| `control` | highest | no |
| `consensus` | high | no |
| `messages` | normal | yes (per-Pid order preserved) |
| `workflow` | normal | yes |
| `bulk` | low (background) | yes |
| `observability` | low | yes |

Backpressure is per-channel. A stalled bulk transfer can't choke control or consensus traffic. With QUIC each logical channel maps to a QUIC stream; with TCP they share a single byte stream demuxed by frame header.

Reference: head-of-line blocking analysis [Akka Remoting limitations](https://doc.akka.io/libraries/akka-core/current/typed/cluster.html) and [QUIC HoL discussion](https://calendar.perfplanet.com/2020/head-of-line-blocking-in-quic-and-http-3-the-details/).

### Handshake

mTLS via rustls. Both sides present certificates issued by the cluster CA. The certificate's CN is the node name; SAN entries contain the node's external addresses. On TLS success the peers exchange:

```
CLIENT_HELLO := { node-id, protocol-version, supported-channels, atom-cache-size }
SERVER_HELLO := { node-id, accepted-channels, atom-cache-size, session-token }
```

Session token is a random 32-byte value used to detect "the connection seems alive but the other side restarted" — if the same logical Node ID presents a different session token, the connection is dropped and the membership layer is notified the peer's epoch changed.

### Scheme surface for remote ops

```scheme
;; Spawn a worker on another node:
(define remote-pid
  (spawn-remote 'worker-7@cluster
    (lambda ()
      (loop
        (match (receive)
          (('work job) (process-job job)))))))

;; Send (uniform syntax, local or remote):
(send remote-pid '(work order-123))

;; Monitor (uniform):
(define ref (monitor remote-pid))
(receive
  (('down ref pid reason) (handle-failure pid reason)))

;; Optional: explicit "no auto-failover" send:
(send-remote 'worker-7@cluster 'chat-router '(message "hi"))
```

`spawn-remote` requires the remote node to have the function's hash in its codebase DB (M12). If it doesn't, the receiving node fetches the AST closure over the `bulk` channel. The transfer is content-addressed so it's cached on subsequent calls.

### Failure propagation

A remote peer going unreachable surfaces as DOWN messages for every monitored Pid on that peer, with reason `'noconnection`. Links propagate exits the same way as local links. The membership layer (M04) is the authoritative source for "peer up/down"; the per-Pid monitors are downstream of that.

### Open questions / pitfalls

- **Full-mesh vs hub-and-spoke.** BEAM is full-mesh; at >70 nodes this is O(N²) connections. v1 ships full-mesh with a documented soft cap of 64 nodes. A hub-and-spoke variant (regional brokers) is a v2 concern.
- **Auto-link removal vs auto-link survival.** When a peer disconnects briefly, do existing links flap? v1 chooses: links to a remote peer that briefly disconnects produce one DOWN message, and the user explicitly re-establishes. No "quiet mode."

## M03 — Discovery providers (cs-discovery)

### Trait

```rust
trait DiscoveryProvider: Send + Sync {
    async fn members(&self, query: &Lookup) -> Result<Resolved, Error>;
    fn name(&self) -> &str;
}

struct Lookup { service_name: String, port_name: Option<String>, protocol: Option<Protocol> }
struct Resolved { service_name: String, targets: Vec<ResolvedTarget> }
struct ResolvedTarget { host: String, port: Option<u16>, ip: Option<IpAddr> }
```

Modeled on [Akka ServiceDiscovery](https://doc.akka.io/japi/akka-core/current/akka/discovery/ServiceDiscovery.html). One method, opaque inputs, opaque outputs — provider authors do all the work behind the trait.

### Provider matrix

| Provider | Crate / dep | Used when |
|----------|-------------|-----------|
| `Static` | none | dev / static deploys |
| `Dns` (A/AAAA + SRV) | `hickory-resolver` | DNS-based service mesh |
| `Kubernetes` | `kube` crate | K8s deploys |
| `FileBased` | none | Nomad-style file-backed |
| `DbBacked` (Postgres/SQLite) | `sqlx` (feature) | self-hosted, no other system |
| `Etcd` | `etcd-client` | etcd cluster |
| `Consul` | `consulrs` | HashiCorp stack |
| `Nomad` | reuse FileBased | with Nomad supervisor |
| `AwsCloudMap` | `aws-sdk-servicediscovery` | AWS deploys |
| `GcpInstanceGroups` | `google-cloud-compute` | GCP deploys |
| `AzureVmss` | `azure_mgmt_compute` | Azure deploys |
| `Mdns` | `mdns-sd` | LAN dev |
| `Gossip` | reuse cs-distrib | bootstrap from one known peer |

All behind feature flags. The default binary only ships `Static + Dns + Kubernetes + FileBased + DbBacked + Mdns + Gossip` (eight provider implementations, ~50KB binary overhead). Heavy cloud SDKs (AWS, GCP, Azure) are opt-in features.

### First-success combinator

```scheme
(cluster
  #:name 'prod
  #:discovery
  (first-success
    (k8s-api #:namespace "prod"
             #:label-selector "app=api")
    (dns-srv "_crab._tcp.api.internal")
    (file "/etc/crab/peers.edn")
    (db-postgres
      #:table "cluster_members"
      #:dsn (getenv "DATABASE_URL"))))
```

Semantics: try providers in declaration order, return the first that yields ≥1 result within the per-provider timeout. Useful for "in K8s, use K8s API; on dev laptop, fall back to mDNS; in CI, use a static file."

### Bootstrap flow

Adapted from [Akka Cluster Bootstrap](https://doc.akka.io/libraries/akka-management/current/bootstrap/details.html):

1. Poll discovery until `required-contact-point-nr` (default 3 for single-region, configurable) reachable addresses are returned consecutively across `stable-margin` polls (default 5s).
2. Each candidate exposes an HTTP `GET /bootstrap/seed-nodes` endpoint returning its current cluster view.
3. If any candidate already belongs to a cluster, the local node joins that cluster.
4. Else, after `new-cluster-wait` (default 30s) of nobody being in a cluster, the node with the **lowest address** self-elects to form a new cluster and others join it.

This rules out the split-brain-on-bootstrap problem of "every node forms its own cluster simultaneously."

## M04 — Membership + failure detection

### Membership state machine

```
                           joining ─────┐
                              │          │
                          (gossip)       │
                              │     (timeout)
                              ▼          │
                          weakly-up      │
                              │          ▼
                           (leader      down ─────► removed
                            promotes)     ▲           ▲
                              │           │           │
                              ▼           │           │
                              up ─────► leaving ──► exiting
                              │
                              └─ (suspect) ── quarantined
```

States:

- `joining` — node has started, has not yet been seen by the leader as up.
- `weakly-up` — visible to peers, eligible to receive non-quorum messages.
- `up` — full member, counted in quorums.
- `leaving` — node has requested graceful shutdown; leader is removing.
- `exiting` — leader has decided to remove; node should refuse new traffic.
- `down` — node is unreachable, failure detector tripped.
- `removed` — terminal, node id is no longer part of the cluster.
- `quarantined` — special case: peer reconnected but with a different epoch.

### Phi-accrual failure detector

For each peer maintain a sliding window of ~200 inter-arrival times of heartbeats (or piggy-backed ordinary traffic). On query at time `t`:

```
φ(t) = -log10( 1 - F(t - last_heartbeat_time) )
```

where `F` is the CDF of the empirically fitted normal `(μ, σ)`. Default threshold `phi-suspect = 8` (one false positive per ~10^8 windows). `phi-down = 12` for "definitely dead" with reduced sensitivity. The detector ships a `acceptable-heartbeat-pause` of 3s — a "free pass" interval that absorbs GC pauses (per [Akka's design](https://doc.akka.io/libraries/akka-core/current/typed/failure-detector.html); [Hayashibara paper](https://www.researchgate.net/publication/29682135_The_ph_accrual_failure_detector)).

### Gossip (SWIM-style)

Build on [`hashicorp/memberlist`](https://github.com/hashicorp/memberlist)'s design without taking it as a dependency (Go-native). Implement in Rust on top of cs-net:

- Every protocol period (default 1s) pick a random peer, send `ping`.
- If no `ack` within timeout (default 500ms), pick `k=3` other peers and ask them to `ping-req` the target.
- If none of the indirect probes succeed, mark the target `suspect`.
- The suspect can refute by gossiping a fresh `alive` claim within `suspicion-timeout` (default `(5 + log10(N)) × protocol-period`).
- If no refutation, mark `dead`. Disseminate the dead event piggy-backed on subsequent ping/ack.

Optionally add **Lifeguard** (HashiCorp 2017): a node that's been slow processing its own queue downweights its own suspicion votes. Adds another ~200 LoC.

### Partition policies (SBR)

```scheme
(cluster
  ...
  #:partition-policy 'keep-majority)
```

- `'keep-majority` — partition with strict majority of `up` members survives; others self-down.
- `'static-quorum` `#:size 3` — partition with ≥3 reachable survives.
- `'keep-oldest` — partition containing the oldest member survives (useful when a singleton lives there).
- `'manual-recovery` — neither side downs; admin intervenes.
- `'isolate-region` — region-aware: each region runs independently; reconvergence on heal.

All deciders defer for `stable-after` (default 20s) before acting. If instability lasts `stable-after + down-all-when-unstable` (default 1m), all nodes self-down — safety net.

### Open questions / pitfalls

- **Quarantine semantics.** When a peer's epoch changes mid-connection (it restarted), every Pid carrying the old epoch is invalid. The connection is dropped, all monitors fire DOWN, the membership entry is marked `quarantined`. Re-handshake with the new epoch is required before new Pids exchange.
- **Asymmetric reachability.** A sees B as down, B sees A as up. Per Akka's design, gossip every node's full unreachability set, not just per-node liveness. Convergence is when reachability across all pairs is mutually consistent.
- **Bootstrap with `stable-after` window.** Don't make decisions in the first 20s of a node's life — it's still learning the cluster shape.

## Sharding & placement (used by cs-agent and cs-workflow)

### Placement modes

```scheme
;; (1) Stateless, hash-based — no coordinator
(send-to-shard 'user:123
               #:strategy 'hrw  ; rendezvous hashing
               '(update-profile ...))

;; (2) Coordinator-based — sticky assignments via a singleton coordinator
(define-shard-region user-region
  #:strategy 'least-shard       ; Akka's allocation strategy
  #:shards 256
  #:entity-id (lambda (msg) (cdr (assq 'user-id msg)))
  #:behavior user-actor-fn)
```

### Hashing

- **Rendezvous (HRW)** is the default — for `n` cluster members and key `k`, compute `hash(k, member_i)` for all `i`, route to the max. O(n) per lookup, but `n ≤ 64` means it's cheap. No coordinator needed; placements recompute on every membership change but can defer migration if the current owner is still reachable.
- **Consistent hashing** is opt-in for very large clusters where O(n) lookup hurts.

### Migration during rebalance

Three modes, user picks per `define-shard-region`:

- `'drop-and-recreate` (default) — stop entity on old node, start fresh on new (rehydrating from persistence). Cheap, lossy if state isn't durable.
- `'stream-state` — old owner serializes entity state, sends over `bulk` channel, new owner deserializes. Requires `(define-serializable ...)` on the actor state.
- `'forward-during-handoff` — old owner forwards messages to new while drain completes. Simple but ordering-sensitive.

### Open questions

- **Singletons.** A cluster singleton (e.g., the workflow scheduler) is a degenerate sharded entity with shard count = 1. v1 implements via `define-shard-region #:shards 1` plus a lease (M07). Akka's `ClusterSingletonManager` is a useful reference but its split-brain handling depends on SBR.

## Code pointers

- `crates/cs-actor/src/lib.rs` — `ActorRef` trait; `RemoteRef` must implement the same surface so `(send …)` is uniform.
- `crates/cs-web/src/` — existing tokio + hyper + rustls stack; transport patterns transfer.
- `crates/cs-stdlib-net/` — existing DNS / HTTP plumbing.
- `crates/cs-channel/src/lib.rs` — backpressure semantics (watermarks); reuse for logical channels.
- `crates/cs-table/src/lib.rs` — shared atomic tables; membership state lives in a cs-table.
- `lib/beam/prelude.scm` — supervisor + behavior macros; extend with `(spawn-remote …)`.

## External references

- Erlang Distribution Protocol — <https://www.erlang.org/doc/apps/erts/erl_dist_protocol.html>
- Akka Cluster Spec — <https://doc.akka.io/docs/akka/current/typed/cluster-concepts.html>
- Akka Cluster Sharding — <https://doc.akka.io/libraries/akka-core/current/typed/cluster-sharding.html>
- Akka Discovery — <https://doc.akka.io/libraries/akka-core/current/discovery/index.html>
- Cluster Bootstrap details — <https://doc.akka.io/libraries/akka-management/current/bootstrap/details.html>
- Phi-accrual paper — Hayashibara et al., SRDS 2004
- SWIM paper — <https://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf>
- Lifeguard paper — <https://ar5iv.labs.arxiv.org/html/1707.00788>
- HashiCorp memberlist — <https://github.com/hashicorp/memberlist>
- Rendezvous Hashing — <https://en.wikipedia.org/wiki/Rendezvous_hashing>
- Riak Core handoff — <https://riak.com/posts/technical/understanding-riak_core-handoff/index.html>
- QUIC vs TCP+TLS — <https://arxiv.org/pdf/1906.07415>
- quinn (Rust QUIC) — <https://github.com/quinn-rs/quinn>
- Damian Gryski on hashing tradeoffs — <https://dgryski.medium.com/consistent-hashing-algorithmic-tradeoffs-ef6b8e2fcae8>
