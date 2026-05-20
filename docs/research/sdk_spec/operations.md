# Operations — observability, simulation, backpressure, schema evolution

Crates this spec creates/extends: **`cs-sim`** (deterministic simulation),
**`cs-stdlib-otel`** (OpenTelemetry export), **`cs-stdlib-wal`**
(append-only log shared with audit).

## Observability

OpenTelemetry-shaped throughout. Every primitive that crosses an
interesting boundary emits a span:

| Layer | Span kind |
|-------|-----------|
| `(send remote-pid msg)` | producer / consumer |
| `(activity 'name args)` | internal (workflow) |
| `(tool-call 'name args)` | internal (agent) |
| `(replicated-actor-call! ...)` | internal (consensus) |
| `(crdt/update! ...)` | internal (crdt) |
| HTTP / DB calls | client |
| Tool handlers | server |

Trace context is propagated:
- through actor messages (in a hidden header on the cs-actor `Message` envelope),
- through remote sends (in the cs-distrib protocol header),
- through workflow journal entries (recorded with the span).

The replay engine **restores** the original trace context — so a
workflow that ran 6 weeks ago and is replayed today appears in the
trace timeline at its original timestamps.

Scheme surface:

```scheme
(otel/set-exporter! (otlp-exporter "https://otel.internal:4317"))

;; Manual spans:
(otel/span 'process-order
  #:attrs '((order-id . 123))
  (lambda ()
    ...))
```

Reference: OTel for LLMs — <https://openobserve.ai/blog/opentelemetry-for-llms/>

## Simulation

`cs-sim` ships:

- A deterministic in-memory transport for cs-net (`Transport::Sim`).
- A virtual clock that advances on quiescence.
- A deterministic PRNG seed for `(workflow-random)`.
- A replayable trace format.

Test:

```scheme
(sim-cluster
  #:nodes 3
  #:seed 42
  #:scenario
  (lambda ()
    (spawn-on 'node-a (lambda () ...))
    (sim-advance-time (seconds 60))
    (sim-partition '(node-a) '(node-b node-c))
    (sim-advance-time (seconds 30))
    (sim-heal)))
```

Produces byte-identical message traces given the same seed. Used
for:
- Testing partition behavior.
- Reproducing distributed-system bugs.
- Regression-testing replay safety.

References:
- FoundationDB simulation testing — <https://apple.github.io/foundationdb/testing.html>
- TigerBeetle deterministic simulation — <https://tigerbeetle.com/blog/2023-07-11-we-put-a-distributed-database-in-the-browser>

## Backpressure

Three places impose backpressure:

1. **Per-actor bounded mailbox.**
   `(spawn thunk #:mailbox-bound 10000)` — sender blocks (or fails
   with `'send-pressure`) when mailbox is full. Already in cs-actor.
2. **Per-channel watermarks.** Existing in cs-channel.
3. **Per-peer transport queue.** New in cs-net: per-peer queue
   depth + per-channel quotas. Backpressure surfaces to the calling
   actor.

Observability hooks: every backpressure event emits a span tagged
with the queue and the depth.

## Schema evolution

Workflow code, replicated actor state, and CRDT shapes all evolve
across deploys. Three coordinated mechanisms:

1. **Code pinning (cs-codebase, M12).** Workflows pin the hash;
   replay always uses the pinned hash.
2. **State migrations (cs-hotreload).** `define-state-migration`
   converts v1 state to v2 on hot upgrade.
3. **CRDT compatibility.** CRDT types are designed to be
   forward/backward-compatible — new replica versions accept old
   wire formats. Breaking changes require a coordinated cluster-wide
   transition.

For breaking changes in any of the above, the recommended
operational story:

- Deploy v2 behind a feature flag that's off by default.
- Wait for all in-flight v1 workflows to drain (or migrate via M12
  pin-mode).
- Flip the flag.

## Upgrade tooling (CLI)

```bash
crabscheme upgrade --drain --timeout 300s
  # SIGTERM, wait for all activities to complete, snapshot, exit

crabscheme rolling-restart --batch-size 2 --wait-healthy 60s
  # restart 2 nodes at a time, wait for membership reconvergence

crabscheme replay-workflow <run-id>
  # deterministically re-run from the journal
```

## Code pointers

- `crates/cs-runtime/src/trace.rs` — existing trace hook (extend with OTel).
- `crates/cs-actor/src/lib.rs` — `Message` envelope (add trace context).
- `crates/cs-channel/src/lib.rs` — existing backpressure semantics.
- `crates/cs-table/src/lib.rs` — durable mailbox; queue depth queryable.

## External references

- OTel docs — <https://opentelemetry.io/docs/>
- OTel for LLMs — <https://openobserve.ai/blog/opentelemetry-for-llms/>
- OTel for AI agents — <https://zylos.ai/research/2026-02-28-opentelemetry-ai-agent-observability>
- FoundationDB sim testing — <https://apple.github.io/foundationdb/testing.html>
- TigerBeetle sim testing — <https://tigerbeetle.com/blog/2023-07-11-we-put-a-distributed-database-in-the-browser>
