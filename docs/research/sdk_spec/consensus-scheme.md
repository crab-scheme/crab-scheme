# Consensus as a CrabScheme library

**Status:** design-draft + runnable pure core. **Supersedes** the Rust-crate
approach in `tasks/M06-consensus.md`. **Governed by** `CONSTITUTION.md`,
Article I (*the code is Scheme; Rust is the machine*).

## Why this exists

Consensus protocol logic — Raft's term/log/commit rules, EPaxos's dependency
ordering — is **pure dispatch**: it makes no system calls. By Article I it
belongs in CrabScheme (`lib/consensus/*.scm`), not in a Rust crate, exactly as
supervision moved from a `cs-supervisor` crate to `lib/beam/prelude.scm`. The
earlier Rust `cs-consensus` crate is retired; this spec replaces it.

## The Rust/Scheme boundary

| Layer | Where | What |
|-------|-------|------|
| Protocol logic | **Scheme** `lib/consensus/raft.scm` | election, log replication, commit, apply — pure `(state, input) → (state, outputs)` |
| Surface | **Scheme** `lib/consensus/*.scm` | `define-replicated-actor`, `replicated-actor-call!`, `-read!`, KV cache |
| Transport | **Rust** `cs-net` | `Channel::Consensus` framed bytes (Sim / TCP+mTLS / QUIC) |
| Actors / mailboxes | **Rust** `cs-actor` | `spawn` / `send` / `raw-receive` / `self` primops |

Only the bottom two rows are Rust — they touch sockets and threads. Everything
above is Scheme.

## Architecture: a pure core + a thin driver (Article II)

The engine is a set of **pure functions** over an immutable node value:

```
(make-raft id ids apply-fn sm0)         ; -> node
(raft-campaign node)                    ; -> (node' . outputs)   start an election
(raft-propose  node command)            ; -> (node' . outputs)   leader appends a command
(raft-tick     node)                    ; -> (node' . outputs)   leader heartbeat
(raft-step     node from message)       ; -> (node' . outputs)   handle one inbound message
```

`outputs` is a list of `(peer . message)`. No clocks, sockets, or mutation —
so the whole protocol is exercised by a **pure Scheme cluster simulator**
(`cluster-make` / `cluster-campaign` / `cluster-propose` / `cluster-settle`)
that routes messages to quiescence. This is runnable today and is the engine's
test (Articles III–IV).

The **networked driver** wraps the same pure step in an actor: a `spawn`ed loop
calls `raft-tick` on a timer and `raft-step` on each `raw-receive`, sending
outputs over the `cs-net` consensus channel. That driver + the
`define-replicated-actor` macro are a **design-draft** until the cluster
send/receive primops are wired from `cs-runtime` to `cs-actor`/`cs-net` — the
same status as `lib/beam/prelude.scm` today.

## Surface

```scheme
(define-replicated-actor kv-cache
  #:initial '()
  #:cluster '(node-a node-b node-c)
  #:consistency 'linearizable
  #:state-machine (lambda (state op) ...))   ; pure; determinism enforced (Article VIII)

(replicated-actor-call! kv-cache '(set "k" "v"))   ; commit on a majority
(replicated-actor-read! kv-cache)                  ; linearizable snapshot (ReadIndex)
```

## Scope

**This iteration (runnable):** the pure Raft core — election, log replication,
majority commit with the current-term rule, apply to a `(state, op) → state`
machine — plus the Scheme cluster simulator and a KV-cache self-test.

**Design-draft (needs primops):** the `define-replicated-actor` macro and the
actor/cluster driver.

**Deferred (documented):** ReadIndex/snapshots/joint-membership in Scheme (the
Rust prototype proved them; port as needed); EPaxos engine (`epaxos.scm`,
leaderless, dependency-graph execution); determinism effect-check on
`#:state-machine` bodies.

## Testing

The pure core is verified by a runnable Scheme self-test: a 3-node cluster
elects a leader, commits a sequence of writes, and every replica's state
machine converges to the same value. Wired into the conformance suite
(`crabscheme run` + `crates/cs-cli/tests/conformance.rs`).
