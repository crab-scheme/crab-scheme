# Actors, supervision, and hot reload

CrabScheme ships a BEAM-style concurrency model with isolated actor
mailboxes, supervision trees, and live code reload. The Rust substrate
(`cs-actor`, `cs-hotreload`, `cs-distrib`) exposes a small primop
surface; the user-facing library lives in
[`lib/beam/prelude.scm`](../../lib/beam/prelude.scm) and implements
supervisors, worker pools, monitors, and behaviors in pure Scheme on
top of those primops.

Design philosophy: *"Rust is the machine; Scheme is the logic"* — the
runtime is the actor substrate; everything you'd want to *configure*
(supervision strategy, restart policy, hot-reload semantics) is
expressed as Scheme code you can read.

> **What's behind a wall today**: module exports are **data-only**.
> Procedures cannot be stored as module exports because they hold
> `Rc<Env>` which is `!Send`. See [ADR 0034][adr34] for the prereq
> work that unblocks procedure-bearing modules. Until then, hot reload
> is for state, not code; you reload data shapes and migration
> callbacks. Real BEAM-style "swap a function definition while it's
> running" needs the `!Send` heap project first.

[adr34]: ../adr/0034-post-1.0-open-issue-landscape.md

## Quickstart — spawn / send / receive

```scheme
(import (rnrs) (lib beam prelude))

; The actor body is referenced by registered name (so it survives
; serialization to a remote node — see Distribution below).
(define (echo-once)
  (let ((msg (raw-receive)))
    ; raw-receive blocks until a message arrives.
    (display msg) (newline)))

; Register the body once at startup.
(register-actor! 'echo-once echo-once)

(define pid (spawn 'echo-once))
(send pid 'hello)
; ⇒ prints "hello"
```

The seven primop surface area (all single-arity, all sync):

| Primop | What |
|---|---|
| `(spawn 'name arg ...)` | Spawn an actor body registered under `'name`. Returns a PID. |
| `(spawn-source "source-text")` | Spawn from a Scheme source string. Useful for dynamic actors. |
| `(spawn-activation "source" 'handler)` | Spawn a **parking** actor on the LocalSet pool: a framework loop calls `(handler msg)` per message, releasing its worker between messages. Scales past the 4096 thread-per-actor ceiling — see [Two scheduling models](#two-scheduling-models-blocking-vs-parking). |
| `(send pid msg)` | Send `msg` (any `SendableValue` shape — no procedures) to `pid`. |
| `(self)` | This actor's PID. |
| `(raw-receive)` | Block until a message arrives. |
| `(reductions)` / `(bump-reductions! n)` / `(yield)` | Cooperative-yield seam. JIT-compiled hot loops also tick reductions automatically (ADR 0031). |
| Supervision: `(system-link! pid)` / `(system-monitor! pid)` / `(system-trap-exit! #t)` / etc. | Low-level supervision primops; the higher-level `link` / `monitor` / `trap-exit!` wrappers live in `lib/beam/prelude.scm`. |

## Two scheduling models: blocking vs. parking

`(spawn 'name)` and `(spawn-source …)` run an actor body that owns its
own `(receive)` loop. That body **blocks** its worker thread while waiting
for a message (`block_in_place`), so each live actor consumes one OS
thread — capped at 4096 per process. Fine for hundreds of actors; a wall
for hundreds of thousands.

`(spawn-activation "source" 'handler)` instead hands the receive loop to
the runtime: it calls `(handler msg)` once per delivered message and
**parks** (releases the worker thread) while the mailbox is empty, so many
mailbox-bound actors multiplex onto a small pool of worker threads. This
breaks the 4096 ceiling for idle / mailbox-bound actors (#30 iter-2a, ADR
0032). The practical limit becomes memory — each actor still has its own
`Runtime` — not threads.

```scheme
; handler is a unary (handler msg) -> continue? procedure.
; Return #f to stop the actor; any other value keeps it alive.
; Per-actor state lives in the handler's own (mutable) bindings —
; the Runtime persists across activations, so state survives the park.
(define source "
  (define total 0)
  (define (handler msg)
    (cond ((eq? msg 'stop) #f)
          (else (set! total (+ total msg)) #t)))")
(define pid (spawn-activation source 'handler))
(send pid 5) (send pid 7) (send pid 'stop)  ; total accumulates across parks
```

**Semantics seam** (ADR 0032): only the framework-owned top-level receive
parks. A `(raw-receive)` *inside* a handler still blocks — the synchronous
VM cannot suspend mid-call — so write actors you want to scale in the
activation shape (one message per call, state in the handler). A CPU-bound
handler holds its worker until it returns; the win is parking *between*
messages, not mid-handler preemption. Free migration between workers
(true work-stealing) needs `Send` actor heaps and stays deferred (iter-2b,
ADR 0032 / 0034).

## Supervision trees

The supervision API is pure Scheme over the primops:

```scheme
(import (rnrs) (lib beam prelude))

; A child spec: (id thunk restart-policy)
(define worker-spec
  (list 'worker-1
        (lambda () (worker-loop))
        'permanent))

; Make a supervisor with `one-for-one` strategy:
(define sup
  (make-supervisor 'my-sup
                   (list worker-spec)
                   'one-for-one
                   5      ; max restarts
                   60))   ; period (seconds)

; Children are started + linked at supervisor startup.
(supervisor-which-children (self))
```

Restart strategies (`make-supervisor` argument):

- `one-for-one` — restart only the failed child.
- `one-for-all` — restart all children if any fails.
- `rest-for-one` — restart the failed child and every later child in the spec list.

Restart policies (per-child):

- `permanent` — always restart (workers).
- `transient` — restart only on abnormal exit (worker pool).
- `temporary` — never restart (one-shot jobs).

If the supervisor's restart count crosses `max-restarts` within
`period`, the supervisor itself fails and propagates upward — same
shape as Erlang/OTP.

## Monitors and linking

```scheme
; Linked actors crash together unless trap-exit is on:
(define worker (spawn 'work))
(link worker)
(trap-exit! #t)
(raw-receive)
; ⇒ (exit-signal worker reason)  ; when worker dies

; Monitors are one-way: the monitoring actor gets a 'DOWN message,
; but the monitored actor is unaffected.
(define ref (monitor worker))
(raw-receive)
; ⇒ (DOWN ref worker reason)
```

## Behaviors

The `lib/beam/prelude.scm` exports `behavior-loop` and `call` for
implementing OTP-style `gen_server`-like patterns:

```scheme
(define (echo-server)
  (behavior-loop
    initial-state                 ; state seed
    (lambda (state call)          ; call handler — synchronous reply
      (values state (cdr call)))
    (lambda (state cast)          ; cast handler — fire-and-forget
      state)
    (lambda (state info)          ; info handler — non-call/cast
      state)
    (lambda (state reason)        ; terminate handler
      (display reason) (newline))
    #f))                           ; cc-fn (code-change) — for hot reload

; Synchronous call (returns the reply):
(define result (call pid '(get-state)))
```

## Tables

For shared state without ETS, use the table primops:

```scheme
(define t (make-table 'my-table 'set))    ; 'set | 'bag | 'ordered-set
(table-insert! t 'key 'value)
(table-lookup t 'key)             ; ⇒ ((key value))
(table-delete! t 'key)
(table-size t)
```

Tables are process-global; concurrent access is serialized through a
single owning actor (the "table writer" started by
`start-table-writer` in `lib/beam/prelude.scm:431+`). Use
`table-tx` for atomic multi-op transactions.

## Hot reload — data version migration

The two-version dispatch table holds an `old` and `current` version
per module. Loading a third version pushes `old` out.

```scheme
; v1 — initial load.
(load-module! 'counter '(("init-state"     . 0)
                          ("schema-version" . 1)))

(define s (lookup-code 'counter "init-state"))   ; ⇒ 0

; ... actors use the state, then a new version lands ...

; v2 — counter state grows a metadata field.
(load-module! 'counter '(("init-state"     . (0 . 0))   ; (counter . metadata)
                          ("schema-version" . 2)))

(lookup-code     'counter "init-state")          ; ⇒ (0 . 0)
(lookup-code-old 'counter "init-state")          ; ⇒ 0
(code-versions   'counter)                       ; ⇒ (1 . 2)
```

When an actor notices a schema bump, it runs a registered migration
to lift its in-memory state from v1 → v2. The full pattern is in the
`beam_counter_migration` integration test.

```scheme
(code-soft-purge! 'counter holder-count)
; ⇒ drops `old` if `holder-count` is 0 (Scheme tracks who's pinned to v1).

(code-purge! 'counter)
; ⇒ force-drop `old` unconditionally (asserts no one is still using v1).
```

### Hot-reload limits (and what's deferred)

- **Procedure exports**: blocked on the `!Send` heap project; see
  [ADR 0034](../adr/0034-post-1.0-open-issue-landscape.md). For now,
  load-module! exports must be `SendableValue`-shaped (no
  `Rc<Env>`-bearing procedures).
- **JIT-body invalidation on reload**: not relevant today (no
  procedures in the registry → no JIT body to invalidate). Tracked
  as #29 once procedures land.

## Distribution

The transport substrate is shipped under the `distrib` feature:

| Crate | What |
|---|---|
| `cs-net` | Sim + TCP + QUIC (with mTLS) transports + framing |
| `cs-distrib` | `DistPid`, `Router`, `RemoteRef`, handshake, DOWN propagation |

A multi-node Raft + EPaxos consensus library on top of this lives at
[`lib/consensus/`](../../lib/consensus/). It runs the consensus core
as deterministic Scheme on top of actor mailboxes — a worked example
of "Scheme is the logic, Rust is the machine."

> **What's behind a wall**: spawn-remote on a closure is impossible
> today (closure-spawn requires the closure to cross the network,
> which means the env crosses a `!Send` boundary). The `spawn-source`
> primop is the practical workaround: ship the source text and have
> the remote node `eval` it locally. The SDK M02 milestone proved
> this end-to-end with the actor-bridged Raft over `cs-net`.

## See also

- [`docs/adr/0034-post-1.0-open-issue-landscape.md`](../adr/0034-post-1.0-open-issue-landscape.md)
  — the architectural walls (procedure-bearing modules + free
  actor migration + automatic yield) all rooted in `!Send`
  `Rc`-everywhere `Value`.
- [`docs/adr/0031-jit-reduction-tick-preemption.md`](../adr/0031-jit-reduction-tick-preemption.md)
  — how JIT-compiled hot loops cooperatively yield.
- [`docs/adr/0032-work-stealing-scheduler-scoping.md`](../adr/0032-work-stealing-scheduler-scoping.md)
  — current scheduling model + the post-1.0 work-stealing analysis.
- [`docs/milestones/beam-v1-exit.md`](../milestones/beam-v1-exit.md)
  — BEAM v1 exit report (8 phases).
- [`lib/beam/prelude.scm`](../../lib/beam/prelude.scm) — the
  supervision-tree, worker-pool, and behavior library in pure Scheme.
- [`lib/consensus/`](../../lib/consensus/) — Raft + EPaxos consensus
  in pure Scheme (proof of concept).
- `crates/cs-runtime/tests/beam_counter_migration.rs` — full v1 → v2
  state-migration E2E.
