# ADR 0032: Work-stealing scheduler (#30 second half) — scoping, tokio-vs-custom, and deferral

> Status: Accepted (deferral; design captured). **iter-2a SHIPPED 2026-06-03**
> (parking activation actors on per-worker `LocalSet`s — see the Decision
> section + `docs/milestones/m30-iter2a-localset-exit.md`); iter-2b/2c remain
> deferred on `Send` heaps.
> Date: 2026-05-26
> Authors: crab-scheme contributors

## Context

Issue #30 ("B3: work-stealing scheduler + auto-yield hook — second
half") has two parts:

1. **Automatic yield** (preempt without explicit `(yield)`).
2. **Work-stealing scheduler** across worker threads.

Scoping the issue against the actual cs-actor code (rather than its
prose) reshaped both parts.

### Part 1 is mostly done; the real gap was JIT-only

VM-level reduction preemption already existed: `vm_tick_reductions()`
runs every dispatch-loop instruction and fires the installed
`VM_YIELD_HOOK` at the budget (default 2000). The actual gap was that
**JIT-compiled code bypasses the dispatch loop and never ticked**, so a
hot tier-up'd actor loop stopped preempting. That gap is closed by **ADR
0031 / #30 iter-1** (a reduction tick at the JIT tail-self back-edge).
This ADR is about Part 2.

### Current actor scheduling (the baseline)

The Scheme `(spawn thunk)` builtin uses `spawn_sync_body_on_task` →
`block_in_place(run_actor_body)`. So **every actor is effectively
thread-per-actor**: it runs its whole life synchronously inside
`block_in_place` on a tokio worker, and `(receive)` on an empty mailbox
calls `blocking_recv()`, *holding that thread*. `block_in_place` promotes
a replacement worker, so N live/blocked actors ≈ N OS threads, hard-capped
by `max_blocking_threads(4096)`. There is an async path
(`spawn_async` + `receive_async`) but only **Rust-native** async bodies
use it (cs-web's `spawn_handler_actor`); Scheme actors do not.

## Two constraints dominate any scheduler choice

1. **Actor heaps are `!Send`.** An actor's Scheme state is an
   `Rc`-everywhere heap (`Value: !Send`) pinned to its thread. Messages
   cross threads (`Payload = Arc<dyn Any + Send + Sync>`); the *heap*
   cannot. Real BEAM work-stealing migrates processes freely because each
   process heap is isolated and movable — crabscheme's `Rc` heaps are
   neither. **No scheduler — tokio's or a custom one — can migrate a
   running actor's heap to another thread** until actor heaps become
   `Send`/movable ("isolated heaps for actors", a separate large GC /
   value-representation project).

2. **The Scheme VM is synchronous.** `(receive)` blocks deep in a sync
   call stack; you cannot `.await` from there. So an actor can only *park*
   (release its worker while waiting) if a **framework-driven loop**
   owns the receive (the "activation model": await mailbox → run a sync
   Scheme handler per message). Arbitrary mid-stack `(receive)` cannot
   park. cs-web's async actors work only because their handler is
   **stateless per message** (request in, response out) — no persistent
   `!Send` heap is held across the await.

Together these mean: the near-term ceiling for *any* approach is **M:N
multiplexing with thread-affinity** (park and resume on the *same*
thread), not free migration/stealing.

## Tokio vs. custom — the comparison

### Option A — ride tokio's work-stealing

**Pros:** reuse a mature, tuned M:N scheduler (no scheduler code to
write/maintain); free I/O integration (cs-net/cs-web/timers already run
on it); smallest delta; lowest risk.

**Cons:** `handle.spawn` requires `Fut: Send`, so a stateful Scheme
actor's `!Send` heap can't be held across a mailbox `await` — forcing
either **per-worker `LocalSet`s** (single-thread executors that host
`!Send` futures; gives M:N + affinity, no migration) or per-activation
heap rehydration (loses cheap state) or Send heaps. tokio scheduling is
cooperative/await-based, not reduction-aware (our yield hook bolts
reduction preemption on via `yield_now`). Little control over BEAM
semantics (priorities, per-actor fuel). Migration still needs Send heaps.

### Option B — build a custom BEAM-style scheduler

**Pros:** full control — per-worker run queues, reduction-budget
preemption as a first-class concept (we already have the counter + iter-1
JIT tick), priorities, BEAM-faithful semantics; independent of tokio's
policy.

**Cons:** large, hard, bug-prone engineering (sync, fairness,
park/unpark, anti-thundering-herd, NUMA); **still** blocked from true
migration by `!Send` (so stealing is limited until Send heaps exist —
same prerequisite as Option A, *plus* a scheduler to write); duplicates
tokio and then needs separate I/O integration (or runs two schedulers);
high maintenance burden for a 1.0-era project.

### Verdict

Both options are gated by the same prerequisites (`!Send` heaps for
migration; activation model + `LocalSet`s for parking under a sync VM).
A custom scheduler adds large effort on top of those prerequisites
without removing them, and duplicates a battle-tested component. **If/when
the scheduler half is built, ride tokio.** The leverage is in the
heap-isolation work, not the scheduler.

## Decision

**Defer Part 2 (the work-stealing scheduler) — likely post-1.0** — and
record the staged path:

- **iter-2a — async Scheme actors (M:N, no migration): SHIPPED
  (2026-06-03).** A `LocalWorkerPool` (`cs-actor/src/local_pool.rs`) of
  single-threaded `LocalSet` workers + `ActorSystem::spawn_local_activation`
  host `!Send` actor futures; the Scheme `(spawn-activation SOURCE HANDLER)`
  builtin (`cs-runtime/src/builtins/beam.rs`) runs a framework-owned
  activation loop — park on `receive_async().await`, then call a per-message
  `(handler msg) -> continue?` with `ACTOR_CTX` scoped to that one call.
  `!Send` heaps survive the await with thread-affinity; the 4096-thread
  ceiling no longer bounds mailbox-bound actors (a Rust test runs 5000 on
  the small pool). **The semantics change landed as designed**: the
  framework owns the parking receive; a `(raw-receive)` *inside* a handler
  still blocks (sync VM can't suspend mid-call). No yield hook on this path
  — a CPU-bound handler holds its worker until it returns (mid-handler
  preemption is out of scope; the win is parking between messages). See
  `docs/milestones/m30-iter2a-localset-exit.md`.
- **iter-2b — isolated (`Send`) actor heaps:** the real prerequisite for
  *true* work-stealing migration, under tokio or a custom scheduler
  equally. A large GC / value-representation project.
- **iter-2c — actual work-stealing:** only meaningful after 2b; ride
  tokio per the verdict above.

iter-1 (ADR 0031) already shipped the contained, verified win (JIT actors
preempt), so deferring the scheduler does not leave actors able to starve
a worker via a hot JIT loop.

## Consequences
- The ADR itself captured the design + deferral; **iter-2a was
  subsequently built** (2026-06-03) — see the Decision section and the
  exit doc.
- The thread-per-actor model (4096 ceiling) still applies to the
  *blocking* paths (`spawn` / `spawn-source`, whose Scheme body owns a
  blocking `(receive)` loop). The new **parking** path
  (`spawn-activation`) is not bound by it — mailbox-bound activation
  actors multiplex onto the `LocalSet` pool; their practical limit is
  per-actor `Runtime` memory.
- iter-2b (`Send` actor heaps) + iter-2c (true work-stealing migration)
  remain deferred; a future contributor has the full constraint map and
  staged plan, so that work doesn't restart from scratch.

## References
- Issue #30 (second half); ADR 0031 (#30 iter-1, JIT reduction tick).
- `cs-actor/src/lib.rs` — `ActorSystem::new` (tokio multi-thread),
  `spawn_sync_body_on_task` / `spawn_async` / `receive_async`, the
  `Payload: Send + Sync` / `Value: !Send` split.
- `cs-runtime/src/builtins/beam.rs` — `run_actor_body`, the `(receive)`
  path (`ACTOR_CTX` → sync `blocking_recv`).
- `cs-web/src/actor.rs` — `spawn_handler_actor`, the existing
  (stateless-per-message) activation-model precedent.
