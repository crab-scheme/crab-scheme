# Green Threads — Requirements

> Status: **Draft**.
> Companion: `design.md`, `tasks.md`.
> Predecessor: the **parallel-runtime** spec (tracks C1–C6, shipped). That work
> delivered green, parking actors for the *framework-driven* `spawn-activation`
> handler shape (`(handler msg) -> continue?`). This spec finishes the
> "N ≫ M actors on a shared pool" story for **free-form Scheme bodies** — the
> shape every `spawn-source` actor (and all of crab-cache) actually uses.

## 1. Background — the three spawn models today

| Surface | primop (`cs-runtime`) | `cs-actor` API | Execution | Ceiling |
|---|---|---|---|---|
| `(spawn name args…)` | `primop_spawn` `beam.rs:335` | `spawn_sync_body_on_task` `lib.rs:1171` | `block_in_place` **dedicated OS thread**; body via `run_actor_body` `beam.rs:789` | 4096 |
| `(spawn-source src entry args…)` | `primop_spawn_source` `beam.rs:377` | `spawn_sync_body_on_task` `lib.rs:1171` | `block_in_place` **dedicated OS thread**; body via `run_scheme_body` `beam.rs:616` (VM tier since `e0ddcb0`) | 4096 |
| `(spawn-activation src handler)` | `primop_spawn_activation` `beam.rs:409` | `spawn_local_activation` `lib.rs:1349` → `LocalWorkerPool` `local_pool.rs` | **green** (LocalSet); per-message `drive_handler` `beam.rs:493` on a corosensei coroutine | memory |

The green path already has everything that makes parking work: a coroutine
driver (`drive_handler`), an async mailbox pop (`driver_receive` `beam.rs:568`),
the `YIELDER` thread-local bridge (`beam.rs:870`), and the cooperative
`(sleep)`/`(raw-receive)` hooks (`cooperative_sleep_hook` `beam.rs:1613`,
`cooperative_raw_receive` `beam.rs:1699`). It is used **only** by the
framework-driven activation handler, which the framework calls once per message.

## 2. Problem

Every `spawn-source` body is a **free-form** Scheme procedure with its *own*
receive/loop structure — e.g. crab-cache's connection actor:

```scheme
(define (conn sock)                       ; src/server/conn.scm:174
  (let loop ((buf (make-bytevector 0 0)))
    (let ((chunk (tcp-recv sock RECV-MAX))); :177  ← BLOCKS the worker
      ...
      (loop rem))))
```

These bodies cannot use `spawn-activation` (which owns the loop). So they run on
the **dedicated-thread** path, which means:

1. **One OS thread per actor.** crab-cache holds one thread per live client
   connection. Past `max_blocking_threads(4096)` (`lib.rs:1047`) new actors
   silently fail to start.
2. **~1–2 MiB of committed stack + scheduler overhead per idle connection** —
   memory scales with *connection count*, not *work*.
3. The green machinery we already shipped is unreachable from these bodies.

The `block_in_place` model is *correct* (one thread, blocking calls are fine),
but it does not scale to the connection counts a cache is expected to hold.

## 3. Why now

Stage 1 (`e0ddcb0`, actor bodies on the VM tier) made per-op cost ~3× cheaper,
so per-op CPU is no longer the bottleneck — **connection scalability and
per-connection memory are the next ceiling.** The coroutine + cooperative-park
machinery built for cooperative-sleep / cooperative-`raw-receive` is exactly the
substrate a whole-body green driver needs; this spec spends that substrate.

## 4. Goals

- **G-A — Whole-body green execution.** A free-form `spawn-source` body runs on
  the LocalSet pool and **parks** (releases its worker) on `(receive)` /
  `(raw-receive)` / `(sleep)`, with **no body changes** and **no receive/sleep
  primop changes** (the existing `YIELDER`-gated hooks already suspend).
- **G-B — Cooperative socket reads.** A blocking `(tcp-recv)` on the green path
  cooperatively suspends instead of freezing every co-located actor on the
  worker. *(This is the gating subsystem — see INV-1.)*
- **G-C — Green by default.** `spawn-source` defaults to green; an explicit
  opt-in (`spawn-source-dedicated`, or equivalent) keeps a dedicated thread for
  actors that must own one.
- **G-D — Behavioral parity.** Links, monitors, `DOWN`, trap-exit, and
  panic-termination behave identically green vs dedicated; both converge on
  `on_actor_termination` (`lib.rs:1152` / `:1398`).
- **G-E — Bounded memory.** N green actors cost O(touched coroutine stack), not
  O(OS thread). 10k idle connections is a routine, not a ceiling-breaking, load.

## 5. Non-goals

- **Work-stealing across LocalSet workers.** Out of scope by ADR 0032
  (work-stealing-scheduler-scoping). Each green actor stays pinned to the worker
  it was dispatched to.
- **Making `cs_gc::Region` `Send`.** The parallel-runtime C3 wall
  (`beam.rs:343–356`). A green body that holds an open `(with-region)` scope
  *across a suspend* is **guarded** (§7 / design §7), not solved, here.
- **Async DNS / async `connect` / cooperative UDP.** Out of scope. Cooperative
  `tcp-recv` **and** `tcp-send` (the cache's hot blocking calls) **are** in scope
  (decision locked); `connect` / DNS / UDP are not.
- **Changing the dedicated path.** `spawn` and opted-in dedicated `spawn-source`
  keep today's exact semantics.

## 6. Invariants & hard constraints

- **INV-1 (phase ordering is load-bearing).** The default must **not** flip to
  green (G-C) until cooperative async TCP (G-B) lands. A blocking `tcp-recv` on
  a shared worker freezes *every* co-located actor — flipping first would wedge
  the cache. Tasks enforce this ordering.
- **INV-2 (shards stay dedicated).** Actors that do **blocking `fsync`**
  (crab-cache shard actors, `node.scm:60`, `node-cluster.scm:87`) **must** keep
  a dedicated thread. Concurrent fsync *across OS threads* is what carries the
  matched-durability write throughput — durable SET currently **beats Redis**
  precisely because fsync parallelizes across dedicated threads. Greening shards
  would serialize fsync onto shared workers and collapse durable throughput.
- **INV-3 (poller stays dedicated).** crab-cache's `peer-poller`
  (`node-cluster.scm:96`) is the Raft tick-clock **and** sole network drainer;
  the cooperative-sleep findings showed it must own its thread.
- **INV-4 (single-thread soundness).** The raw-pointer `ACTOR_CTX` / `YIELDER` /
  `actor_ptr` discipline is sound **only** because a LocalSet worker is
  single-threaded and control strictly alternates (driver parked in `resume()`
  while the coroutine runs; coroutine frozen while the driver awaits). The design
  **must** preserve "clear `ACTOR_CTX` + `YIELDER` before every `.await`"
  (`beam.rs:534–537`) so a co-located actor never observes a stale pointer.

## 7. Known hazard to gate (not silently ship)

**Region scope across a suspend.** On a single-threaded worker the TLS region
stack (`REGION_STACK_TLS`) is shared by all co-located actors. If green actor A
suspends mid-`(with-region)` and co-located actor B runs and pushes *its* region
scope onto the same TLS stack, the stacks interleave → corruption. crab-cache's
green candidates (conn/pusher/broker) do **not** use `(with-region)`, so this
does not block the showcase — but the whole-body driver **must refuse to
suspend with an open region scope** (clear error), with save/restore-around-
suspend as a tracked follow-up. (The existing per-message `drive_handler` shares
this latent assumption; we make it explicit.)

## 8. Success criteria

- **S1 — Scale.** **50k–100k** concurrent idle green connections held with
  bounded, sub-linear-in-threads RSS (concrete budget set by the G5 measurement;
  pushes the green coroutine stack small — see design §5). This is impossible
  today (4096 thread ceiling); the target is chosen to prove the model scales an
  order of magnitude past it.
- **S2 — Correctness parity.** crab-cache conn/pusher/broker run green;
  shards/poller stay dedicated; the full gate suite stays green —
  `bench/single-node.sh` (conformance), `bench/cluster.sh failover` (no acked
  write lost), `bench/crash-recovery.sh` (10k durable SETs survive `kill -9`),
  `bench/linearizability.sh` (dup rate **no worse** than the documented
  pre-existing non-idempotent-replay baseline).
- **S3 — No throughput regression.** Relaxed + durable throughput stays within
  noise of Stage 1 (`/tmp/cc-vsredis-final.md`); tail latency under high
  connection counts should *improve*.
- **S4 — No new starvation.** A CPU-bound green actor cannot indefinitely freeze
  a co-located actor (G3 green yield hook), proven by an adversarial test.
- **S5 — Opt-out exists.** A documented escape hatch (env var) forces the old
  dedicated default for rollback without code changes.

## 9. Acceptance scenarios

1. **Whole-body park.** *Given* a green `spawn-source` actor whose body is
   `(let loop () (let ((m (raw-receive))) … (loop)))`, *when* it awaits an empty
   mailbox, *then* its worker is released and a co-located green actor runs;
   *when* a message arrives, *then* the body resumes at the `raw-receive` with
   the message — no body or primop change.
2. **Many on one worker.** *Given* `CRABSCHEME_ACTOR_WORKERS=1` and 500 green
   `spawn-source` echo actors, *when* each is pinged, *then* all 500 reply
   (proving multiplexing, not thread-per-actor).
3. **Cooperative read.** *Given* two green conn actors on one worker, one parked
   in `(tcp-recv)` with no data, *when* the other receives a request, *then* it
   is served promptly (the parked read released the worker).
4. **Durable regime intact.** *Given* crab-cache with green conns + dedicated
   shards, *when* `redis-benchmark` drives `appendfsync always` SET, *then*
   durable SET throughput stays ≥ Stage 1 (shards still fsync in parallel).
5. **Link parity.** *Given* a green actor linked to a dedicated actor, *when*
   either crashes, *then* the other receives the `Exit`/`DOWN` exactly as in the
   all-dedicated case; a trap-exit green actor survives a linked crash.
6. **No starvation.** *Given* a CPU-bound green actor (tight compute loop, no
   receive) co-located with a responder, *when* a message is sent to the
   responder, *then* it is processed within a bounded time (G3).
