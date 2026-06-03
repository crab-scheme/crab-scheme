# #30 iter-2a — LocalSet parking actors — exit report

> Status: SHIPPED 2026-06-03 · branch `feat/actor-localset-2a`
> ADR: [`0032-work-stealing-scheduler-scoping.md`](../adr/0032-work-stealing-scheduler-scoping.md)

## What this delivered

The one unblocked half of #30: break the `max_blocking_threads(4096)`
ceiling for mailbox-bound Scheme actors by multiplexing them M:N onto a
small pool of worker threads with **thread-affinity** (park/resume on the
same thread; **no** migration — that needs `Send` heaps, iter-2b).

Two layers:

1. **Engine — `cs-actor`** (`src/local_pool.rs`, commit `99acc03`).
   `LocalWorkerPool`: N OS threads, each a current-thread tokio runtime
   hosting a `LocalSet`. `ActorSystem::spawn_local_activation` dispatches an
   actor (round-robin, pinned for life) and `spawn_local`s its future on the
   chosen worker. Unlike `spawn_async` it drops the `Fut: Send` bound, so a
   `!Send` `Rc`-heap survives across a mailbox `await`; unlike
   `spawn_sync_body_on_task` it parks instead of pinning a thread via
   `block_in_place`. The dispatched job captures only `Send` data and builds
   the `!Send` future *on* the worker — the same trick `spawn-source` uses to
   build its heap on the spawned thread.

2. **Scheme surface — `cs-runtime`** (`builtins/beam.rs`, commit `257671e`).
   `(spawn-activation SOURCE HANDLER)`: loads `SOURCE` into a fresh per-actor
   `Runtime` on the worker, resolves `HANDLER` (a unary
   `(handler msg) -> continue?`), then runs the framework-owned loop — park
   on `receive_async().await`, decode the message, call the handler with
   `ACTOR_CTX` scoped to that one synchronous call. `#f` stops the actor;
   per-actor state lives in the handler's own persistent top-level bindings
   (the `Runtime` survives across activations). Adds two thin actor-gated
   `Runtime` methods: `apply_value` (walker-tier apply) + `sendable_to_value`.

## Key design decisions

- **The `(receive)`-parks semantics seam falls out for free.** ADR 0032
  required "only the top-level loop receive parks; mid-stack receive keeps
  blocking." That is exactly *who owns the loop*: the framework owns the
  parking `receive_async`, while a `(raw-receive)` *inside* a handler still
  blocks via the unchanged `ACTOR_CTX` primop. **No change to `raw-receive`.**
- **No yield hook on this path.** A synchronous handler can't cooperatively
  yield mid-call (the sync-VM constraint), so mid-handler preemption is out
  of scope; the win is parking *between* messages. A CPU-bound handler holds
  its worker until it returns (documented limitation).
- **State persists in the handler's closure, not threaded through Rust.**
  The per-actor `Runtime` lives on the worker across activations, so the
  handler keeps state in its own mutable top-level bindings — the minimal
  API that still gives a real parking actor.
- **`ACTOR_CTX` is scoped per activation**, never held across the `await`,
  so actors sharing a worker thread never see each other's context (a
  per-activation refinement of `run_actor_body`'s whole-body guard).

## Evidence

- `cs-actor`: 3 `local_pool` unit tests + 3 integration tests
  (`tests/local_activation.rs`), incl. **`exceeds_blocking_thread_ceiling`
  — 5000 mailbox-bound `!Send`-heap actors run on the small pool** (0.03s),
  plus persistent-state and ping/pong. 25 lib + doctest green; clippy-clean.
- `cs-runtime --features actor`: `spawn_activation_accumulates_persistent_state`
  (sum 1..=5 across 5 separate parking activations → 15) +
  `multiple_activation_actors_multiplex_on_the_pool` (8 concurrent activation
  actors). 96 lib + all integration green; default (no-`actor`) build
  compiles (new methods gated); clippy-clean.

## What remains deferred (unchanged walls)

- **iter-2b — `Send` actor heaps.** The prerequisite for *true*
  work-stealing migration. A multi-month GC / value-representation project
  (`Value: !Send`, `Rc` everywhere). Blocked.
- **iter-2c — actual work-stealing.** Only meaningful after 2b; then ride
  tokio (per ADR 0032's verdict).
- **#29 — JIT-invalidation on hot reload.** Still blocked: `to_sendable_in`
  rejects `Value::Procedure`, so no procedure reaches the version registry
  (ADR 0034).

The practical ceiling for parking actors is now **per-actor `Runtime`
memory**, not OS threads — exactly the limit ADR 0032 anticipated.
