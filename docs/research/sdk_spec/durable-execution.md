# Durable Execution — workflows, activities, sagas, replay

Crate this spec creates: **`cs-workflow`**.

Covers milestone **M08**. Detailed task list in
`tasks/M08-durable-execution.md`.

## Architectural choice

Crab Scheme adopts the **journal-replay** model: every workflow has
an append-only event history; the workflow function is a
deterministic fold over that history. On recovery the function
re-runs from the top; for each command the runtime looks up the
matching event in history and returns the previously-recorded
result instead of re-executing.

This is the Temporal/Cadence/Inngest/DBOS/Restate consensus
design. <https://docs.temporal.io/workflow-definition>

Crab's distinctive moves on top of that consensus:

- **Static determinism enforcement.** `cs-expand` refuses to expand
  a `(define-workflow …)` body whose effect set (M01) contains
  `net`, `wall-clock`, `random`, or `io`. The expander error
  points at the offending form. No "non-determinism error at
  replay" — we catch it at compile time.
- **Awakeables as a primitive.** Borrowed from Restate. A workflow
  can suspend on a durable promise that any external system can
  later resolve. Cleaner than signal-poll-loops.
- **Virtual objects via `cs-actor` + durability flag.** Don't ship
  a separate "durable object" primitive — extend `cs-actor` with
  `#:durable #:key …`. Per-key single-writer is the existing actor
  semantics.
- **HLC-based timers.** Workflow timers commit at HLC
  timestamps (see consistency.md). Same logical clock as Raft
  commit timestamps, same as CRDT writes.

## Goals

| # | Goal | Acceptance |
|---|------|------------|
| W1 | A workflow that crashes mid-execution resumes from the last completed step on the next worker pickup. | E2E test: kill a worker mid-saga, observe completion on a new worker. |
| W2 | A workflow re-execution produces byte-identical commands given the same input + journal. | Replay diff test; mismatch raises `&non-deterministic` at compile time. |
| W3 | Activity calls retry with exponential-backoff + jitter; non-retryable conditions short-circuit. | Property test: inject transient failures, observe retry pattern. |
| W4 | Sagas compensate in reverse order on step failure. | E2E test: book-trip example; force step 3 failure, observe C2 then C1. |
| W5 | Hot-reload of workflow code is safe — workflows pinned to the old code hash continue running on the old code until they finish. | Code-pinning test with cs-codebase (M12). |
| W6 | Long workflows survive history bloat via `(continue-as-new …)`. | Soak test: 1M-iteration loop continues-as-new every 50k iters. |

## Programming model

### The two roles

- **Workflow function** — orchestrates. Must be deterministic.
  Allowed: workflow primitives (`await`, `activity`, `timer`,
  `signal`, `awakeable`, `workflow-now`, `workflow-random`,
  `workflow-uuid`), pure Scheme forms.
  Forbidden: I/O, wall-clock, system random, mutable global state,
  hash-table iteration with non-deterministic order, references to
  bindings whose effect set includes `net`/`io`/`wall-clock`.
- **Activity function** — does the side-effecting work. May call
  the network, the LLM, the filesystem, anything. Retried on
  failure per the retry policy.

### Surface API

```scheme
;; Workflow definition. Body runs deterministically.
(define-workflow (process-order order)
  (let* ((reserved (await (activity 'reserve-inventory order)))
         (charged  (await (activity 'charge-card order
                                   #:retry-policy fast-retry))))
    (await (signal 'ship-confirmation
                   #:timeout (hours 24)))
    (await (activity 'fulfill order reserved charged))
    'done))

;; Activity definition. May be impure.
(define-activity (charge-card order)
  #:retry-policy default-retry
  #:start-to-close (minutes 5)
  #:heartbeat (seconds 30)
  (stripe-charge order))

;; Saga with compensations.
(define-workflow (book-trip trip)
  (saga
    (step (await (activity 'reserve-flight trip))
          #:compensate (activity 'cancel-flight trip))
    (step (await (activity 'reserve-hotel trip))
          #:compensate (activity 'cancel-hotel trip))
    (step (await (activity 'charge-card trip))
          #:compensate (activity 'refund-card trip))
    ;; pivot point — no compensation, can't unsend
    (step (await (activity 'send-itinerary trip)))))

;; Awakeable — durable promise resolved by external system
(define-workflow (approval-flow request)
  (let ((awk (make-awakeable)))
    (await (activity 'notify-approver
                     (request-approver request)
                     (awakeable-id awk)))
    (await awk)))                  ; suspends durably until resolved

;; Elsewhere (from any node):
(resolve-awakeable! id 'approved)

;; Deterministic primitives:
(workflow-now)                     ; HLC timestamp from journal
(workflow-random)                  ; PRNG seeded from journal
(workflow-uuid)                    ; UUID v7 from journal
(workflow-sleep (minutes 5))       ; durable timer

;; Escape hatch — journal once, replay returns same result:
(side-effect 'session-id (lambda () (host-uuid)))

;; Long workflows escape history bloat:
(continue-as-new process-order new-state)
```

### Retry policy

```scheme
(define-retry-policy default-retry
  #:initial-interval (seconds 1)
  #:backoff-coefficient 2.0
  #:max-interval (seconds 100)
  #:max-attempts #f                ; unlimited
  #:non-retryable '(&authentication &invalid-input))

(define-retry-policy fast-retry
  #:initial-interval (millis 100)
  #:backoff-coefficient 1.5
  #:max-interval (seconds 5)
  #:max-attempts 5)
```

Default matches Temporal: `1s × 2.0^n, capped at 100s, unlimited
attempts`. Per-activity overrides allowed.

Reference: <https://docs.temporal.io/encyclopedia/retry-policies>

### Timeouts

Four orthogonal timeouts per activity (Temporal convention):

- `start-to-close` — max time for one attempt.
- `schedule-to-start` — max time from schedule to worker pickup.
- `schedule-to-close` — total time across all retries.
- `heartbeat-timeout` — long-running activities must heartbeat
  every N seconds or be considered dead.

## Engine internals

### Event history per execution

```
WorkflowExecution = {
  workflow-id, run-id,
  workflow-fn-hash,         ; pinned via cs-codebase (M12)
  input,
  history: [Event, …],      ; append-only
  state: Pending | Running | Completed(value) | Failed(condition),
}

Event = ActivityScheduled(seq, name, args, retry-policy)
      | ActivityCompleted(seq, value)
      | ActivityFailed(seq, condition, attempt)
      | TimerStarted(seq, fire-at)
      | TimerFired(seq)
      | SignalReceived(seq, name, value)
      | AwakeableCreated(seq, id)
      | AwakeableResolved(seq, id, value)
      | SideEffectRecorded(seq, key, value)
      | ChildWorkflowStarted(seq, child-run-id)
      | ChildWorkflowCompleted(seq, value)
      | WorkflowCompleted(value)
      | WorkflowFailed(condition)
```

### Sticky worker cache (v1.1)

After a worker handles a workflow's decision task, keep the live
state cached and prefer to route subsequent tasks back to the same
worker. Cache hit = O(1) resume; cache miss = full replay
(O(history)). This is what Cadence and Temporal do to keep
warm-path latency low.

### Storage

Pluggable journal backend behind a trait:

```rust
trait Journal: Send + Sync {
    async fn append(&self, run_id: &str, events: &[Event]) -> Result<()>;
    async fn read(&self, run_id: &str) -> Result<Vec<Event>>;
    async fn snapshot(&self, run_id: &str, snapshot: Snapshot) -> Result<()>;
    async fn read_snapshot(&self, run_id: &str) -> Result<Option<Snapshot>>;
}
```

Concrete implementations (feature-gated):

- `JournalInMemory` — dev, tests.
- `JournalCsTable` — embedded; uses cs-table OrderedSet for the log.
- `JournalCsConsensus` — self-hosted highly-available; uses
  `cs-consensus::Kv`.
- `JournalPostgres` — production; uses `cs-stdlib-postgres`.
- `JournalSqlite` — laptop dev.

### Determinism enforcement

Two layers:

1. **Compile-time** (M01 effect annotations + cs-expand). A
   `(define-workflow …)` body's effect set is checked against the
   forbidden set `{net, io, wall-clock, random}`. Violations are
   compile errors with a precise pointer to the offending form.
2. **Replay-time** — on each replay, recorded commands must match
   re-executed commands by `(seq, kind, args-hash)`. Mismatch
   raises `&non-deterministic` (a subcondition of `&assertion`)
   and freezes the workflow until code is patched.

Both layers exist because some non-determinism (e.g., closing over
a mutable Scheme parameter that changes between executions) can
only be detected at replay.

## Versioning

When workflow code changes, in-flight workflows must continue to
run on the old code. Two strategies are first-class:

### Code pinning (preferred)

Every workflow execution pins the hash of the workflow function on
start (cs-codebase, M12). Replay always fetches that hash. Hot
reload (`cs-hotreload`) introduces a *new* hash; the live workflow
keeps running on the old hash until it finishes.

### Explicit versioning

For workflows that must outlive even pinned code (e.g., long-running
loops that should benefit from bug fixes mid-flight), expose a
Temporal-style patch primitive:

```scheme
(define-workflow (long-running-loop x)
  (when (workflow-patched? 'add-cache-step "v2")
    (await (activity 'cache-init)))
  (loop (await (activity 'do-step x))))
```

`workflow-patched?` returns `#t` only for runs that started after
`add-cache-step` was deployed; older runs see `#f` and skip the
new code. Patch markers are journaled.

Reference: <https://docs.temporal.io/concepts/what-is-a-version-id>

## Code pointers

- `crates/cs-actor/src/lib.rs` — actor system; the
  workflow-worker pool is built on it.
- `crates/cs-channel/src/lib.rs` — signals are channels.
- `crates/cs-hotreload/src/lib.rs` — version-aware dispatch.
- `crates/cs-table/src/lib.rs` — `OrderedSet` for embedded journal.
- `crates/cs-pkg/src/lib.rs` — manifest format (workflow code lives
  in packages too).
- `lib/beam/prelude.scm` — supervisor + behaviour macros; extend with
  `(define-workflow …)`, `(define-activity …)`, `(saga …)`.

## External references (consolidated)

### Temporal & friends
- Temporal workflow definition — <https://docs.temporal.io/workflow-definition>
- Temporal retry policy — <https://docs.temporal.io/encyclopedia/retry-policies>
- Temporal continue-as-new — <https://docs.temporal.io/workflow-execution/continue-as-new>
- Temporal signals/queries/updates — <https://docs.temporal.io/handling-messages>
- Temporal side effects — <https://docs.temporal.io/develop/go/side-effects>
- Temporal workflow execution limits — <https://docs.temporal.io/workflow-execution/limits>
- Temporal rules / TMPRL1100 — <https://github.com/temporalio/rules>
- Bitovi on replay testing — <https://www.bitovi.com/blog/replay-testing-to-avoid-non-determinism-in-temporal-workflows>

### DBOS
- DBOS architecture — <https://docs.dbos.dev/architecture>
- DBOS workflow tutorial — <https://docs.dbos.dev/python/tutorials/workflow-tutorial>
- DBOS vs Temporal — <https://www.tiarebalbi.com/en/blog/dbos-vs-temporal-postgres-durable-execution>

### Cadence
- Cadence workflows — <https://cadenceworkflow.io/docs/concepts/workflows>
- Cadence event handling — <https://cadenceworkflow.io/docs/concepts/events>

### Inngest
- Inngest steps — <https://www.inngest.com/docs/learn/inngest-steps>
- How Inngest executes functions — <https://www.inngest.com/docs/learn/how-functions-are-executed>
- Inngest SDK spec — <https://github.com/inngest/inngest/blob/main/docs/SDK_SPEC.md>

### Restate
- Restate key concepts — <https://docs.restate.dev/foundations/key-concepts>
- Restate engine first principles — <https://www.restate.dev/blog/building-a-modern-durable-execution-engine-from-first-principles>
- Restate Rust SDK — <https://docs.rs/restate-sdk/latest/restate_sdk/>
- Restate awakeables — <https://docs.restate.dev/develop/ts/awakeables/>

### Sagas
- Garcia-Molina & Salem 1987 — <https://www.cs.cornell.edu/andru/cs711/2002fa/reading/sagas.pdf>
- Saga orchestration vs choreography — <https://blog.bytebytego.com/p/saga-pattern-demystified-orchestration>
- Temporal saga patterns — <https://temporal.io/blog/mastering-saga-patterns-for-distributed-transactions-in-microservices>

### Event sourcing
- Azure pattern — <https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing>
- eventually-rs — <https://github.com/get-eventually/eventually-rs>
- primait/event_sourcing.rs — <https://github.com/primait/event_sourcing.rs>

### Continuations
- F# computation expressions — <https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/computation-expressions>
- Freer Monads (Kiselyov) — <https://okmij.org/ftp/Haskell/extensible/more.pdf>
- Resonate how it works — <https://docs.resonatehq.io/evaluate/how-it-works>

### Retry & jitter
- Exponential backoff + jitter — <https://layrs.me/course/hld/12-reliability-patterns/retry>
- Retries, backoff, jitter — <https://www.codereliant.io/p/retries-backoff-jitter>
