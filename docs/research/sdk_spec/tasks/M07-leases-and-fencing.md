# M07 — Leases + fencing tokens

**Crates extended:** `cs-consensus` (lease state machine).
**Effort:** 1-2 iters.
**Depends on:** M06.

## Goal

Atomic lease grants with strictly-monotonic fencing tokens.
Protected primitives accept `#:fence <token>` and reject stale tokens.
The single hardest distributed primitive to get right — Kleppmann's
critique resolved.

## Acceptance

- `(lease-acquire! 'name #:ttl-ms N)` returns a `(lease-id, token, deadline-hlc)` triple.
- Tokens are monotonic per resource across all grants (in time, not per-leaseholder).
- `(replicated-actor-call! … #:fence stale-token)` rejects with `&fenced`.
- E2E test: deliberately partition a leaseholder, advance time past TTL, observe successor's fence rejecting old holder's writes.

## Iters

### A — Lease state machine

- Replicated actor whose state is `(map lease-name → (holder, token, deadline-hlc))`.
- Commands: `acquire`, `renew`, `release`, `expire-stale`.
- Token bump is monotonic per lease name.
- **Code:** `crates/cs-consensus/src/lease.rs`.

### B — `#:fence` keyword + propagation

- Extend `replicated-actor-call!` argument parser.
- State machine checks `token >= current-highest-token-seen` per resource.
- Reject with `(error '&fenced ...)`.
- **Code:** `cs-runtime` builtin update.

## Example

```scheme
(define lease
  (lease-acquire! 'email-sender #:ttl-ms 30000))
(lease-token lease)     ; ⇒ 42

;; Fence-protected write:
(replicated-actor-call! shard-3
                        '(update key val)
                        #:fence (lease-token lease))

;; If we slept past TTL and lost the lease:
(define new-lease (lease-acquire! 'email-sender #:ttl-ms 30000))
(lease-token new-lease) ; ⇒ 43

;; Old leaseholder's stale write now fails:
(replicated-actor-call! shard-3
                        '(update key val)
                        #:fence 42)     ; ⇒ &fenced
```

## External refs

- Kleppmann distributed locking — <https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>
- Chubby — Burrows, OSDI 2006
- Spanner TrueTime — <https://docs.cloud.google.com/spanner/docs/true-time-external-consistency>

## Code pointers

- `crates/cs-consensus/src/group.rs` — Raft group infra from M06.
- `crates/cs-runtime/src/builtins/mod.rs` — `replicated-actor-call!` builtin.
