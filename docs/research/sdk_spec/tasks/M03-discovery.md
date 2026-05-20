# M03 — Discovery providers (cs-discovery)

**Crates created:** `cs-discovery`.
**Effort:** 3-5 iters (one per provider family).
**Depends on:** M02.

## Goal

A pluggable `DiscoveryProvider` trait with concrete implementations
behind feature flags, plus a `first-success` combinator and a cluster
bootstrap that resolves seed-node ambiguity via lowest-address rule.

## Acceptance

- `(cluster #:discovery (first-success P1 P2 …))` forms a cluster from any one of the listed providers.
- 3-node cluster formation in CI via: Static, DNS-SRV, Kubernetes Endpoints (kind cluster), DbBacked (Postgres).
- Bootstrap with lowest-address tie-break for cold cluster formation.
- Provider failures (DNS down, K8s API 500) fall through to the next provider.

## Iters

### A — DiscoveryProvider trait + Static + Dns

- `crates/cs-discovery/src/lib.rs`: `trait DiscoveryProvider` with async `members()` method.
- Static (config-derived) + Dns (A/AAAA + SRV via `hickory-resolver`).
- `first-success` combinator: try in order, return first non-empty.

### B — FileBased + DbBacked (Postgres/SQLite)

- File watcher (`notify` crate) for FileBased.
- `cs-stdlib-postgres` + `cs-stdlib-sqlite` integration for DB.

### C — Kubernetes (kube crate)

- Watch Endpoints / EndpointSlices by labelSelector.
- Behind `discovery-k8s` feature.

### D — Cluster Bootstrap

- Poll all providers until `required-contact-point-nr` reachable.
- Probe each contact for existing-cluster status.
- Lowest-address election on cold start; `new-cluster-wait` debounce.

### E — Cloud + mDNS + Gossip (feature-gated, optional)

- AWS Cloud Map / GCP / Azure VMSS / mDNS / Gossip-from-known-peer.

## Example

```scheme
(cluster
  #:name 'prod
  #:discovery
  (first-success
    (k8s-api #:namespace "prod"
             #:label-selector "app=checkout")
    (dns-srv "_crab._tcp.checkout.internal")
    (file "/etc/crab/peers.edn")
    (db-postgres
      #:table "cluster_members"
      #:dsn (getenv "DATABASE_URL"))))
```

## External refs

- Akka Discovery — <https://doc.akka.io/libraries/akka-core/current/discovery/index.html>
- Akka Cluster Bootstrap details — <https://doc.akka.io/libraries/akka-management/current/bootstrap/details.html>
- Consul API — <https://developer.hashicorp.com/consul/api-docs>
- kube crate (Kubernetes Rust client) — <https://kube.rs/>

## Code pointers

- `crates/cs-stdlib-net/` — existing DNS/HTTP/TLS.
- `crates/cs-stdlib-fs/` — file watching (notify).
- `crates/cs-stdlib-postgres/` — DB-backed if shipped.
