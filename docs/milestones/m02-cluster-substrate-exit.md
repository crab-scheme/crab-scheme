# M02 — Distributed actor transport: exit report

**Branch:** `feat/sdk-cluster-substrate` (off `main`).
**Crates:** `cs-net` (transport), `cs-distrib` (cluster routing).
**Spec:** `docs/research/sdk_spec/tasks/M02-cluster-substrate.md`.

The cluster transport + routing substrate is implemented and thoroughly
tested. Everything that can be verified deterministically (without real
sockets/certs or a cross-milestone dependency) is done and covered;
**cs-net 20 + cs-distrib 33 = 53 tests**, all green.

## What shipped

| Iter | Deliverable | Where |
|------|-------------|-------|
| **A** | `DistPid` cluster identity (full `NodeId` + `local_id`) + self-describing wire codec | `cs-distrib::pid` |
| **B** | `Transport` trait (sync `send`/`try_recv`/`is_closed`/`close`); **Sim** transport (deterministic, in-memory); **TCP** transport (tokio, sync-over-async bridge) | `cs-net::{sim,tcp}` |
| **C** | Length-prefixed channel framing (`[channel][len][payload]`) + streaming `FrameDecoder` so one byte stream muxes all six logical channels | `cs-net::framing` |
| **D** | Handshake **protocol**: `Hello` (NodeId + atom-cache-size + session-token) + `evaluate_hello` — accept / quarantine on version, self-identity, or stale epoch | `cs-distrib::handshake` |
| **E** | `Router` (local vs remote dispatch per `DistPid`, epoch-checked) + `RemoteRef` (`ActorRef`-shaped `.send`) | `cs-distrib::router` |
| **F** | DOWN-on-disconnect: `monitor` + `detect_disconnects` fire `DownNotice{NoConnection}` once per monitored Pid on a dropped node | `cs-distrib::router` |

## Acceptance criteria

- ✅ **3-node cluster forms via Sim transport; ping/pong across all pairs** —
  `router::tests::three_node_cluster_ping_pong_all_pairs`.
- ✅ **Peer disconnect fires DOWN on monitored remote Pids** —
  `router::tests::disconnect_fires_down_for_monitored_remote_pid`.
- ◑ **`spawn-remote` returns a usable RemoteRef** — `RemoteRef` is done;
  shipping the *closure* to the remote node needs M12 (content-addressed
  codebase), so `spawn-remote` proper is deferred to that milestone.
- ✅ **mTLS required on production transports** — `TcpTransport::connect_tls`
  / `accept_tls` (rustls configs in `cs-net::tls`) encrypt + mutually
  authenticate; tested (encrypted round-trip + certless-client rejection).
  A **QUIC** transport (`cs-net::quic`, quinn) is also implemented: TLS 1.3
  is mandatory there, and each channel rides its own QUIC stream. The
  head-of-line-blocking isolation that buys is **measured and proven under
  packet loss** (≈110× lower control latency vs a single shared stream at 5%
  loss) — see `docs/milestones/m02-transport-benchmark.md` and the test
  `quic_per_stream_isolates_control_from_bulk_under_loss`.
- ◑ **Per-peer pooling; channels never starve** — per-channel isolation +
  watermark backpressure are implemented and tested (Sim); a connection
  *pool* manager (multiple conns per peer) is a follow-up.

### Encrypted transports — DONE

Both production transports are encrypted + mutually authenticated:
- **mTLS over TCP** — `cs-net::tls` config builders + `TcpTransport::{connect_tls, accept_tls}`.
- **QUIC** — `cs-net::quic` (quinn, `quic` feature): TLS 1.3 mandatory, one stream per channel (no HoL).

Cert *provisioning from files* (`--tls-cert` PEM loading) is the only TLS
piece left — the configs take parsed DER today; tests use rcgen.

## Remaining tail (follow-ups)

1. **`(spawn-remote …)`** — blocked on **M12** for closure transfer.
2. **cs-actor / Scheme binding** — map `RemoteRef` onto cs-actor `ActorRef`,
   serialize messages via cs-runtime's `SendableValue`, so `(send pid msg)`
   is unchanged in source. (The substrate carries opaque bytes today, which
   is what kept it deterministically testable in isolation.)
3. **Connection pooling** — multiple connections per peer.
4. **PEM cert loading** — `--tls-cert` file provisioning for the configs.

These are a cross-milestone dependency (1), runtime integration (2), or I/O
glue (3, 4) — none change the substrate's design, which is locked in + tested
(cs-net 22 default / 24 with `quic`, cs-distrib 33).
