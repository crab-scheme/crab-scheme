# M08 — Durable workflows (cs-workflow)

**Crates created:** `cs-workflow`.
**Effort:** 6-8 iters.
**Depends on:** M01 (effects), M12 (codebase DB for hash pinning).
**Optional dep:** M06 (cs-consensus::Kv as an HA journal backend).

## Goal

`(define-workflow …)` body runs deterministically over a journal;
activities are impure escape hatches; sagas compose with explicit
compensation; awakeables provide durable promises; long workflows
escape via `continue-as-new`.

## Acceptance

- Workflow survives worker crash: kill the worker mid-execution, observe completion on the next pickup.
- Replay produces byte-identical commands (verified by replay diff).
- Compile-time effect check rejects forbidden primitives inside `(define-workflow …)`.
- Sagas compensate in reverse order on step failure.
- `(continue-as-new …)` retires current run and starts a new one with carried-forward state.
- Awakeables: workflow suspends until `(resolve-awakeable! id v)` is called from anywhere in the cluster.

## Iters

### A — Journal abstraction + in-memory backend

- `trait Journal { append, read, snapshot }`.
- `JournalInMemory` + `JournalCsTable` (cs-table OrderedSet).
- **Code:** new `crates/cs-workflow/src/journal.rs`.

### B — `(define-workflow …)` form + replay loop

- Effect-check (M01) at expand time.
- Replay loop: re-execute body; for each command, look up in journal; return cached or execute new.
- **Code:** `crates/cs-workflow/src/replay.rs` + macros in `lib/workflow/prelude.scm`.

### C — Activities + retry policy

- `(define-activity …)` form. Activity workers (cs-actor pool) consume tasks.
- Retry: exponential backoff + jitter, configurable per activity.
- Four timeouts: start-to-close, schedule-to-start, schedule-to-close, heartbeat.

### D — Deterministic primitives

- `workflow-now`, `workflow-random`, `workflow-uuid`, `workflow-sleep`.
- All read from journal on replay.

### E — Sagas

- `(saga step₁ #:compensate c₁ step₂ #:compensate c₂ …)` form.
- Registers compensations as each step completes; on failure runs in reverse.

### F — Awakeables

- `(make-awakeable)` returns ID + suspends.
- `(resolve-awakeable! id v)` works from any actor on any node.
- Resolution journaled.

### G — Continue-as-new + child workflows

- `(continue-as-new wf-name input)` retires current; starts new with same ID.
- Child workflows via `(child-workflow wf-name input)`.

### H — Postgres / consensus journal backends

- `JournalPostgres` via cs-stdlib-postgres.
- `JournalCsConsensus` via cs-consensus::Kv (M06).

## Example

```scheme
(define-workflow process-order
  (lambda (order)
    (let* ((reserved (await (activity 'reserve-inventory order)))
           (charged  (await (activity 'charge-card order))))
      (await (signal 'ship-confirmation #:timeout (hours 24)))
      (await (activity 'fulfill order reserved charged))
      'done)))

(define-activity charge-card
  #:retry-policy default-retry
  #:start-to-close (minutes 5)
  (lambda (order) (stripe-charge order)))

(define-workflow book-trip
  (lambda (trip)
    (saga
      (step (await (activity 'reserve-flight trip))
            #:compensate (activity 'cancel-flight trip))
      (step (await (activity 'reserve-hotel trip))
            #:compensate (activity 'cancel-hotel trip))
      (step (await (activity 'charge-card trip))
            #:compensate (activity 'refund-card trip)))))

;; Start a workflow:
(start-workflow process-order #:input order-123 #:id "ord-123")
;; Survives process restart automatically.
```

## External refs

- Temporal workflow definition — <https://docs.temporal.io/workflow-definition>
- Temporal retry policy — <https://docs.temporal.io/encyclopedia/retry-policies>
- DBOS architecture — <https://docs.dbos.dev/architecture>
- Restate awakeables — <https://docs.restate.dev/develop/ts/awakeables/>
- Garcia-Molina & Salem sagas — <https://www.cs.cornell.edu/andru/cs711/2002fa/reading/sagas.pdf>
- Inngest steps — <https://www.inngest.com/docs/learn/inngest-steps>

## Code pointers

- `crates/cs-actor/src/lib.rs` — workflow worker pool.
- `crates/cs-channel/src/lib.rs` — signals.
- `crates/cs-hotreload/src/lib.rs` — version-aware dispatch.
- `crates/cs-table/src/lib.rs` — embedded journal backend.
- `crates/cs-codebase/` (M12) — hash pinning.
