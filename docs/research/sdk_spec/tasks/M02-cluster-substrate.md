# M02 — Distributed actor transport (cs-distrib)

**Crates created:** `cs-distrib`, `cs-net`.
**Effort:** 4-6 iters.
**Depends on:** M01 (effect annotations for `agent`-tagged tools).
**Unblocks:** M03, M04, M05, M06, M08 (everything distributed).

## Goal

A `(send pid msg)` to a remote Pid routes over a per-peer transport
connection, decoded on the other side, delivered to the remote
actor's mailbox. Failures surface as DOWN messages via the
membership layer.

## Acceptance

- 3-node cluster forms via Sim transport; ping/pong across all pairs.
- `(spawn-remote 'other-node fn)` returns a usable RemoteRef.
- Peer disconnect fires DOWN on every monitored remote Pid.
- mTLS handshake required on production transports.
- Per-peer connection pooling; logical channels never starve each other.

## Iters

### A — NodeId + Pid encoding (1 iter)

- Extend `cs-actor::Pid` with `node_id`, `epoch`.
- Wire-encode/decode (CBOR or similar).
- **Code:** `crates/cs-actor/src/lib.rs` (extend Pid struct).

### B — cs-net Transport trait + Sim impl (1 iter)

- `Transport::Sim` for in-process multi-node tests; deterministic.
- `Transport::Tcp` (no TLS yet) for cross-process.
- **Code:** new `crates/cs-net/src/lib.rs`. Reuse cs-web's tokio + rustls setup.

### C — Logical channels + framing (1 iter)

- Multiplex `control / consensus / messages / workflow / bulk / observability` over one transport conn.
- Length-prefixed frames + channel tag byte.
- Per-channel watermark for backpressure.

### D — Handshake + mTLS (1 iter)

- rustls integration, cert from `--tls-cert` flag.
- `CLIENT_HELLO`/`SERVER_HELLO` with NodeId + atom-cache-size + session-token.
- Quarantine state on epoch mismatch.

### E — RemoteRef + Router (1 iter)

- `RemoteActorRef` implementing `ActorRef`.
- `Router` decides local vs remote dispatch per Pid.
- `(send pid msg)` is unchanged in source.

### F — `spawn-remote` + DOWN on disconnect (1 iter)

- `(spawn-remote 'node-b fn)` over `messages` channel.
- Remote node fetches function closure from cs-codebase (M12) over `bulk`.
- On disconnect, fire DOWN with reason `'noconnection` for monitored remote Pids.

## Example

```scheme
;; Node A:
(cluster #:name 'demo
         #:discovery (static '("node-b@localhost:7001"
                              "node-c@localhost:7002")))

(define remote-pid
  (spawn-remote 'node-b@localhost:7001
    (lambda ()
      (loop (match (receive)
              (('ping from) (send from 'pong)))))))

(monitor remote-pid)
(send remote-pid `(ping ,(self)))
(receive
  ('pong (display "got pong"))
  (('down ref pid reason) (display "remote died:") (display reason)))
```

## External refs

- Erlang Distribution Protocol — <https://www.erlang.org/doc/apps/erts/erl_dist_protocol.html>
- Akka Remoting / Cluster — <https://doc.akka.io/docs/akka/current/typed/cluster-concepts.html>
- quinn (QUIC) — <https://github.com/quinn-rs/quinn>
- rustls — <https://docs.rs/rustls/>

## Code pointers

- `crates/cs-actor/src/lib.rs` — `ActorRef`, `Pid`, `Message`.
- `crates/cs-web/src/` — tokio + rustls + multiplexing patterns.
- `crates/cs-channel/src/lib.rs` — backpressure semantics.
