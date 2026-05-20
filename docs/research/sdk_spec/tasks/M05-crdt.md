# M05 — CRDT layer (cs-crdt)

**Crates created:** `cs-crdt`.
**Effort:** 4-5 iters.
**Depends on:** M02 (gossip transport via cs-net).

## Goal

State-based + delta-state CRDTs with HLC timestamps + DVV causal
context, anti-entropy via gossip + Merkle, exposed to Scheme as
first-class values.

## Acceptance

- Six v1 types: G-Counter, PN-Counter, OR-Set, OR-Map, LWW-Register, MV-Register.
- Delta-state wire format; gossip on cs-net `messages` channel.
- HLC clock + DVV causal context primitives.
- `define-mergeable-actor` form.
- Conformance: 3-node test, mutations on each node, observe convergence within `<10×fanout` periods.
- Memory: tombstones don't grow unbounded under typical load (causal stability window honored).

## Iters

### A — HLC + DVV runtime primitives

- `(hlc-now)`, `(hlc-before? a b)`, `(hlc-concurrent? a b)`.
- `(dvv-mk)`, `(dvv-tick dvv node)`, `(dvv-merge a b)`.
- **Code:** new `crates/cs-crdt/src/clock.rs`.

### B — State-based CRDT primitives (G, PN, OR-Set)

- Generic `CrdtState` trait + concrete impls.
- `(crdt/pn-counter name)`, `(crdt/inc! c)`, `(crdt/value c)`.
- **Code:** `crates/cs-crdt/src/{counter,set,map}.rs`.

### C — OR-Map, LWW-Register, MV-Register

- Causal map keyed by anything `equal?`-comparable.
- LWW with HLC timestamp + tiebreaker (NodeId).
- MV with DVV causal context.

### D — Delta-state + anti-entropy

- Each CRDT op produces a delta; ship delta over gossip.
- Background Merkle reconciliation on idle interval.
- Tombstone GC with `#:keep-tombstones-ms` knob.
- **Code:** `crates/cs-crdt/src/sync.rs`.

### E — `define-mergeable-actor` form

- Lower into an actor whose state is a CRDT + a delta-publish hook.
- All mutations from `#:on-message` produce deltas → gossip.
- **Code:** `lib/beam/prelude.scm` for the macro; cs-runtime builtin for actual mutation.

## Example

```scheme
;; Simple counter:
(define likes (crdt/pn-counter 'post:123:likes))
(crdt/inc! likes)
(crdt/value likes)      ; ⇒ N

;; Mergeable actor for presence:
(define-mergeable-actor user-presence
  #:state (crdt/causal-map)
  #:on-message
    (lambda (state msg)
      (case (car msg)
        ((join)  (crdt/map-set! state (cadr msg) 'online))
        ((leave) (crdt/map-remove! state (cadr msg))))))

(define presence-pid (spawn user-presence))
(send presence-pid '(join alice))    ; gossips to peers
```

## External refs

- Shapiro et al. CRDT survey — <https://inria.hal.science/inria-00555588v1/document>
- Almeida et al. Delta CRDTs — <https://arxiv.org/abs/1603.01529>
- DVV paper — <https://gsd.di.uminho.pt/members/vff/dotted-version-vectors-2012.pdf>
- HLC pattern — <https://martinfowler.com/articles/patterns-of-distributed-systems/hybrid-clock.html>
- Automerge (reference impl) — <https://github.com/automerge/automerge>
- Akka Distributed Data — <https://doc.akka.io/libraries/akka-core/current/typed/distributed-data.html>

## Code pointers

- `crates/cs-table/src/lib.rs` — backing for CRDT state if needed.
- `crates/cs-net/src/lib.rs` — gossip transport (M02).
- `crates/cs-channel/src/lib.rs` — backpressure on the gossip channel.
