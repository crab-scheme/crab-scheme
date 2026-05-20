# M04 — Membership + failure detection

**Crates extended:** `cs-distrib` (membership module).
**Effort:** 3-4 iters.
**Depends on:** M02, M03.

## Goal

The cluster tracks who's `up / joining / weakly-up / leaving /
exiting / down / removed / quarantined`. Failed peers detected p99
≤ 5 s on a 30-node cluster. Split-brain handled by configurable SBR.

## Acceptance

- Membership state machine implements all 8 states.
- Phi-accrual failure detector with per-peer sliding window.
- SWIM-style gossip with suspicion subprotocol.
- `keep-majority` SBR strategy + `static-quorum` + `manual-recovery`.
- Quarantine state on epoch mismatch.
- All visible to Scheme via a `(cluster-events)` channel.

## Iters

### A — Membership state machine

- `enum MemberState`; transitions via leader.
- Leader = lowest-address `up` member (or external lease in v2).
- **Code:** `crates/cs-distrib/src/membership.rs` (new module).

### B — Phi-accrual failure detector

- Sliding window (~200 samples) of inter-heartbeat-arrival times.
- φ = -log10(1 - CDF(d, μ, σ)).
- Tunable thresholds: `suspect=8`, `down=12`; `acceptable-pause=3s`.
- **Code:** `crates/cs-distrib/src/phi.rs`.

### C — SWIM gossip + suspicion

- Period (1s) ping + indirect-ping (k=3) on cs-net `control` channel.
- Suspicion subprotocol: suspect can refute within timeout.
- Piggyback membership deltas on every ping/ack.
- **Code:** `crates/cs-distrib/src/gossip.rs`.

### D — SBR strategies + cluster-events

- `keep-majority`, `static-quorum #:size N`, `keep-oldest`, `manual-recovery`, `isolate-region`.
- Stability window (default 20s) before SBR decides.
- `(cluster-events)` returns a channel of events `('member-up ...)`.
- **Code:** `crates/cs-distrib/src/sbr.rs`.

## Example

```scheme
(cluster
  #:name 'prod
  ...
  #:failure-detector '(phi-accrual #:suspect 8 #:down 12)
  #:partition-policy '(keep-majority #:stable-after 20))

;; Watch cluster events:
(define evs (cluster-events))
(spawn (lambda ()
  (loop (match (channel-receive evs)
          (('member-up node) (log-info "up:" node))
          (('member-down node) (log-warn "down:" node))
          (('partition us them) (handle-partition us them))))))
```

## External refs

- SWIM paper — <https://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf>
- Lifeguard paper — <https://ar5iv.labs.arxiv.org/html/1707.00788>
- Phi-accrual paper — <https://www.researchgate.net/publication/29682135_The_ph_accrual_failure_detector>
- HashiCorp memberlist (reference impl) — <https://github.com/hashicorp/memberlist>
- Akka SBR — <https://doc.akka.io/libraries/akka-core/current/split-brain-resolver.html>

## Code pointers

- `crates/cs-distrib/src/` — created in M02; extend here.
- `crates/cs-table/src/lib.rs` — for membership table backing.
