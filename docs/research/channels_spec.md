# Channels — spec

> Status: research-stage draft (2026-05-19). First-class MPMC
> channels for CrabScheme: typed FIFOs with optional buffering,
> blocking send/recv with backpressure, close semantics, and a
> `select` form that waits on the first-ready of several
> operations.

## Why this matters

CrabScheme already has two communication primitives:

- **Actor mailbox** — one per cs-actor, MPSC (any number of
  senders, exactly one receiver = the owning actor). Tied to
  actor lifetime. Not first-class (you can pass a PID around,
  but the mailbox itself isn't a Scheme value you can hand
  to N consumers).
- **cs-table** — shared K/V store (set + ordered-set). Concurrent
  read/write, but no FIFO ordering, no blocking semantics, no
  backpressure, no close signal.

Several common patterns don't fit either:

| Pattern             | Mailbox | cs-table | Channels |
|---------------------|:-------:|:--------:|:--------:|
| Worker pool (N consumers share a queue) |   no    |   no     |   yes    |
| Fan-out (1 producer → N consumers, each gets one msg) |   no    |   no     |   yes    |
| Fan-in (N producers → 1 consumer with order preserved) |   ok\*  |   no     |   yes    |
| Bounded backpressure (blocking send) |   no    |   no     |   yes    |
| Wait on first-ready of several inputs |   no    |   no     |   yes    |
| Explicit "no more messages" signal |   no    |   no     |   yes    |

\* Mailbox handles fan-in but only if the consumer IS an actor;
sending to a non-actor consumer requires routing through one.

Channels fill the gap. They're first-class Scheme values (so
you can put a channel in a hash-table, send one through another
channel, return one from a procedure), can cross actor
boundaries as `SendableValue`, and ride on the same tokio
runtime + reduction scheduler the rest of the BEAM-style
runtime uses.

## TL;DR architecture

```
                  ┌─────────────────────────────────────────┐
                  │  Scheme surface                         │
                  │                                         │
                  │  (make-channel [n])                     │
                  │  (channel-send!     ch v)               │
                  │  (channel-try-send! ch v)               │
                  │  (channel-recv      ch [timeout-ms])    │
                  │  (channel-try-recv  ch)                 │
                  │  (channel-close!    ch)                 │
                  │  (channel-closed?   ch)                 │
                  │  (channel-len       ch)                 │
                  │  (channel-capacity  ch)                 │
                  │                                         │
                  │  (select                                │
                  │    [(recv ch1) v body...]               │
                  │    [(send! ch2 val) body...]            │
                  │    [(after ms) body...])                │
                  └────────────┬────────────────────────────┘
                               │ primops in cs-runtime
                  ┌────────────▼────────────────────────────┐
                  │  cs-channel (new crate, ~600 LoC)       │
                  │                                         │
                  │  Channel { id, kind, depth, closed }    │
                  │    kind: Buffered(tokio::mpsc::N)       │
                  │          Unbuffered(tokio::mpsc::1)     │
                  │          Broadcast(tokio::broadcast)    │
                  │                                         │
                  │  ChannelRegistry { id → Arc<Channel> }  │
                  │                                         │
                  │  Send/recv go through tokio's mpsc.     │
                  │  Close is a soft-flag + sender drop.    │
                  │  Select is a tokio::select! macro       │
                  │  emitted by the cs-runtime primop.      │
                  └─────────────────────────────────────────┘
```

The point of view: a Channel is just a tokio mpsc wrapped in a
process-wide registry, indexed by `ChannelId` (i64), so
`SendableValue::Channel(ChannelId)` can cross actor boundaries
without dragging non-Send Scheme state. The registry holds the
actual sender/receiver halves — sender + receiver are kept in
the same registry slot so any actor that learns the ID can
drive either side.

## Goals

1. **First-class** — a channel is a Scheme value you can store,
   pass, return.
2. **Crosses actor boundaries** via `SendableValue::Channel`.
3. **Three flavors**: unbounded (default), bounded (capacity N),
   unbuffered (synchronous rendezvous, capacity 0).
4. **Blocking send + recv with backpressure** on bounded
   channels.
5. **Try-variants** for non-blocking probing.
6. **Close semantics**: senders after close → error; receivers
   drain pending then see `*closed*`.
7. **Select**: a primop that waits on the first-ready of several
   send/recv/timeout clauses.
8. **Cooperative with the reduction scheduler**: every
   send/recv/select call ticks reductions so a CPU-bound actor
   doing tight channel ops still yields.
9. **Integrates with the cs-web actor-chain pattern**: a
   middleware actor can `select` between a request mailbox and
   a config channel.
10. **Performance**: send/recv on an uncontended Fast channel
    should be < 1 μs per call.

## Non-goals (this spec)

- **Persistent channels** that survive process restart. The
  durable backing pattern (analogous to `MailboxKind::Durable`
  for actor mailboxes) is post-1.0 follow-up.
- **Distributed channels** across nodes. Same line as
  `cs-distrib` in the BEAM spec — defer to v2.
- **Statically typed channels**. cs-typer could narrow channel
  payload types later; v1 channels carry `SendableValue`.
- **Channel select with priority biasing** beyond what
  `tokio::select!` provides (`biased`).
- **`for v := range ch`-style loop sugar**. Easy to write as a
  macro on top of `channel-recv` — separate library work.

## Success criteria

A channel implementation ships when:

1. The Scheme surface above works end-to-end: an actor sends N
   messages to a channel, M consumer actors collectively
   receive all N (no drops, no duplicates).
2. Bounded send blocks when the channel is full and unblocks
   when a receiver drains.
3. Unbuffered send blocks until a receiver is ready (rendezvous).
4. `(channel-close! ch)` causes subsequent `channel-recv`s to
   return `*closed*` AFTER draining any in-flight messages.
5. `select` picks one ready clause per call; if multiple are
   ready, picks fairly (round-robin or random — `tokio::select!`
   without `biased` is pseudo-random).
6. Send/recv tick the reduction counter — a tight
   send-loop yields cooperatively under load.
7. Microbench: uncontended send + recv pair < 1 μs at p50 on
   the dev host.
8. cs-runtime primops + tests: 15+ acceptance tests covering
   the cases above plus error paths (recv from never-sent
   channel, send to closed channel, select with no ready clauses
   and no `after`, etc.).

## Rust / Scheme split

The split mirrors cs-actor and cs-table:

- **Rust side** owns the runtime mechanics: the tokio mpsc
  wrappers, the channel registry, the select implementation
  (a primop that does a `tokio::select!` against the resolved
  channel halves).
- **Scheme side** is the API: `make-channel`, `channel-send!`,
  `channel-recv`, the `select` macro. The macro expands to a
  series of `channel-try-*` + `channel-recv-timeout` calls
  inside a Scheme dispatcher loop — OR (preferred) a single
  `channel-select` primop call that takes a list of clauses
  and returns the winning index + value.

Channel handles in Scheme are `(channel <id-fixnum>)` tagged
pairs — same convention as `('*web-request* h)` from cs-web,
or PIDs (`<pid:<n.m>>` symbols). Channels can be `eq?`-compared
by ID.

## Crate breakdown

### `cs-channel` (~600 LoC)

Single crate, no deps except tokio + cs-table for the optional
Durable backing.

```rust
pub struct Channel {
    pub id: ChannelId,
    kind: ChannelKind,
    capacity: Option<usize>,   // None for unbounded
    depth: AtomicU64,          // current in-flight count
    closed: AtomicBool,
}

enum ChannelKind {
    Buffered  { tx: mpsc::Sender<SendableValue>, rx: Mutex<mpsc::Receiver<SendableValue>> },
    Unbuffered { tx: mpsc::Sender<SendableValue>, rx: Mutex<mpsc::Receiver<SendableValue>> }, // capacity=1
    Broadcast  { tx: broadcast::Sender<SendableValue> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(u64);

pub struct ChannelRegistry {
    next_id: AtomicU64,
    chans:   DashMap<ChannelId, Arc<Channel>>,
}

impl ChannelRegistry {
    pub fn create(&self, capacity: Option<usize>) -> ChannelId;
    pub fn lookup(&self, id: ChannelId) -> Option<Arc<Channel>>;
    pub fn drop_channel(&self, id: ChannelId);

    pub async fn send(&self, id: ChannelId, v: SendableValue) -> Result<(), ChannelError>;
    pub fn try_send(&self, id: ChannelId, v: SendableValue) -> Result<bool, ChannelError>;
    pub async fn recv(&self, id: ChannelId) -> Result<Option<SendableValue>, ChannelError>;
    pub fn try_recv(&self, id: ChannelId) -> Result<Option<SendableValue>, ChannelError>;
    pub async fn recv_timeout(&self, id: ChannelId, ms: u64) -> Result<Option<SendableValue>, ChannelError>;

    pub fn close(&self, id: ChannelId) -> bool;
    pub fn is_closed(&self, id: ChannelId) -> bool;
    pub fn len(&self, id: ChannelId) -> usize;
}
```

**Receiver mutex** — tokio's `mpsc::Receiver` is `!Sync` (one
receiver per channel by design). Wrapping it in `Mutex` lets
the registry expose recv to any actor that asks. Lock is held
only across the await; the recv future polls cooperatively.

**Send is `async`** for the blocking path. `try_send` is sync
and returns `Ok(false)` when the channel is at capacity.

### `cs-runtime/src/builtins/channel.rs` (~400 LoC)

Behind the new `channel` feature. Implies `actor` (shares the
tokio runtime, scheduler, and `SendableValue` boundary). Exposes:

- `(make-channel)` → unbounded
- `(make-channel capacity)` → bounded
- `(make-channel 0)` → unbuffered
- `(channel-send! ch v)`
- `(channel-try-send! ch v)`
- `(channel-recv ch)` / `(channel-recv ch timeout-ms)`
- `(channel-try-recv ch)`
- `(channel-close! ch)`
- `(channel-closed? ch)`
- `(channel-len ch)`
- `(channel-capacity ch)`
- `(channel-select clauses)` — the primop the macro expands to

Channel IDs surface as `(channel <id>)` pairs (analog to the
`*web-request*` tagged pair). The `channel?` predicate
recognizes them.

### `lib/beam/channels.scm` (~200 LoC, no new Rust)

Patterns + macros built on the primops:

- `(select ...)` macro — expands to `channel-select` with the
  clause list parsed into the primop's expected shape.
- `(channel-for-each proc ch)` — drain a channel until close,
  calling `proc` per value.
- `(make-worker-pool n consumer-body)` — spawn `n` actors all
  receiving from one shared channel; returns the channel ID
  and the list of worker PIDs.
- `(make-pipeline stage-procs)` — chain channels through a
  sequence of stages (each stage consumes from one channel,
  produces to the next).
- `(fan-out src-ch n)` — spawn `n` broadcast consumers from
  one source channel.

## Scheme-facing surface

### Special forms / primops

```scheme
; ---- Create -------------------------------------------------

; Unbounded — sends never block, depth grows without limit.
(make-channel)              ; => (channel 1)

; Bounded — sends block when depth == capacity.
(make-channel 100)          ; => (channel 2)

; Unbuffered — send blocks until a receiver is ready (rendezvous).
(make-channel 0)            ; => (channel 3)

; ---- Send / recv --------------------------------------------

(channel-send!     ch v)            ; blocking; returns unspec
(channel-try-send! ch v)            ; non-blocking; #t / #f
(channel-recv      ch)              ; blocking; returns v or *closed*
(channel-recv      ch timeout-ms)   ; blocking with timeout; *timeout*
(channel-try-recv  ch)              ; non-blocking; v / *empty* / *closed*

; ---- Lifecycle ----------------------------------------------

(channel-close!    ch)              ; signal no more sends
(channel-closed?   ch)              ; predicate
(channel-len       ch)              ; current depth
(channel-capacity  ch)              ; #f for unbounded, n otherwise
(channel?          v)               ; type predicate

; ---- Select -------------------------------------------------

(select
  [(recv ch1)    v   (display "got from ch1: ") (display v)]
  [(recv ch2)    v   (display "got from ch2: ") (display v)]
  [(send! ch3 v3)    (display "sent to ch3")]
  [(after 1000)      (display "1s timeout")]
  [else              (display "would-block-on-all")])
```

Sentinels (`*closed*`, `*timeout*`, `*empty*`) follow the
existing convention from cs-web (`*web-request*`) and cs-actor
(`*exit*`, `*down*`, `*timeout*`). They're symbols, eq?-
comparable, no quoting tricks.

### Library helpers

```scheme
(import (lib beam channels))

; Drain until close.
(channel-for-each
  (lambda (item) (process item))
  inbox)

; Worker pool — N actors share one channel.
(define-values (jobs workers)
  (make-worker-pool 8
    (lambda (job)
      (display "worker ") (display (self)) (display ": ") (display job))))
; Use:
(channel-send! jobs 'work-1)
(channel-send! jobs 'work-2)
...

; Pipeline of stages.
(define out
  (make-pipeline
    (list (lambda (in) ; stage 1 — read URL, fetch HTML
            (let loop ()
              (let ([url (channel-recv in)])
                (if (eq? url '*closed*) #f
                    (begin (channel-send! ... (fetch url))
                           (loop))))))
          (lambda (in) ; stage 2 — extract links
            ...)
          (lambda (in) ; stage 3 — write to db
            ...))))
```

## Semantics

### Blocking rules

| Channel kind | Send blocks when                | Recv blocks when             |
|--------------|---------------------------------|------------------------------|
| unbounded    | never                           | empty                        |
| bounded(n)   | depth == n                      | empty                        |
| unbuffered   | no receiver currently waiting   | no sender currently waiting  |

`channel-try-send!` / `channel-try-recv` never block; they
return `#f` / `*empty*` instead.

### Close

```scheme
(define ch (make-channel 10))
(channel-send! ch 'a)
(channel-send! ch 'b)
(channel-close! ch)
(channel-recv ch)       ; => 'a
(channel-recv ch)       ; => 'b
(channel-recv ch)       ; => *closed*
(channel-send! ch 'c)   ; => error: send on closed channel
(channel-closed? ch)    ; => #t
```

A closed channel still delivers buffered messages — close ≠
discard. Senders after close raise `&channel-closed` (a
condition subtype of `&error`).

Closing an already-closed channel is a no-op (returns `#f`).

### Select

Resolves the first ready clause. `tokio::select!` underneath
uses pseudo-random fairness by default. Clauses:

- `(recv ch) var body...` — body sees `var` bound to the
  received value or sentinel.
- `(send! ch val) body...` — body runs after a successful send.
- `(after ms) body...` — body runs if no other clause was
  ready within `ms`.
- `else body...` — body runs if EVERY non-`after`, non-`else`
  clause would block. Mutually exclusive with `(after ...)`.

Exactly one clause's body runs per `(select ...)` call. The
form returns that clause's body's value.

### Reduction cooperation

Every send / recv / try-send / try-recv / select call ticks
the reduction counter inside an actor body via the same
`vm_tick_reductions` hook the bytecode dispatch loop uses
(parallel-runtime C4.5). A tight channel-pump loop yields
cooperatively at the 2000-reduction budget.

`channel-recv` blocking semantics use tokio's `recv().await`,
which already cooperatively yields when the channel is empty
— no busy spin.

### Errors

Conditions emitted (new subtypes of `&error`):

- `&channel-closed`         — send on a closed channel
- `&channel-not-found`      — operation on a dropped channel ID
- `&channel-invalid-arg`    — non-channel value, negative capacity, etc.

`channel-recv` on a closed channel returns `*closed*`, NOT
raise — receivers iterate naturally to the end.

## Integration with existing CrabScheme

### Runtime + GC changes

None to cs-gc or cs-core. `SendableValue` gains a new variant:

```rust
pub enum SendableValue {
    ...existing...
    Channel(ChannelId),
}
```

Conversion in cs-runtime/src/builtins/beam.rs's
`to_sendable_in` / `from_sendable` learns this variant.
Channels carried across actor boundaries reach the destination
as the same `(channel <id>)` Scheme tagged pair.

### VM / JIT / AOT changes

cs-vm: the bytecode dispatch loop's `vm_tick_reductions` hook
already supports plug-in tick sources. Channel send/recv just
calls into it. No new opcodes.

cs-jit-cranelift: no changes — channel primops are
HostProcedure calls, not lowered to native IR.

cs-aot: out of scope. AOT compiles numeric kernels; channel
ops are not in the lowered subset.

cs-typer: no changes for v1. Future: a `(Channelof T)` type
that narrows the contract on send/recv values. Tracked
post-1.0.

### cs-actor

cs-actor doesn't need to know about channels — channels live
in their own registry and use their own tokio handles.
Actors interact with channels through the cs-runtime primops,
which look up the channel via the registry.

The actor body's `(receive)` and a channel's `(channel-recv)`
are independent — an actor can do both. Both tick reductions.

For the `(receive)` form to wait on EITHER its mailbox OR a
channel, we'd need a richer `(receive)` syntax (see "Open
questions" below). For v1, actors that want both compose
manually: poll mailbox with `(receive (after 0) ...)` then
poll channel with `(channel-try-recv ch)`, sleep briefly if
both empty.

### cs-web

A handler / layer actor can use channels for:

- Rate limiting — a token-bucket channel that emits one token
  every `1/rate-per-second` seconds; handler blocks on
  `(channel-recv tokens)` before processing.
- Pub/sub — a broadcast channel that fans out events to every
  connected SSE / WebSocket client.
- Backpressure — a bounded channel between request reception
  and a slow downstream worker; reaching capacity returns 503.

No cs-web changes required. The integration is pure Scheme.

### cs-runtime feature gates

```toml
# cs-runtime/Cargo.toml
channel = ["actor", "dep:cs-channel"]
```

`channel` implies `actor` because cs-channel needs the same
tokio runtime as cs-actor — sharing it avoids spinning up a
second multi-thread runtime per process.

cs-cli grows `channel = ["cs-runtime/channel"]` mirroring
existing feature passthrough.

## Phased rollout

| Phase | Deliverable                                                | Acceptance |
|-------|------------------------------------------------------------|-----------|
| CH-A  | cs-channel crate, Channel struct, registry, unbounded only | unit: send/recv round trip, len, drop |
| CH-B  | Bounded channels, blocking send                            | unit: bounded send blocks, unblocks on recv |
| CH-C  | Unbuffered channels (rendezvous)                           | unit: unbuffered send waits for recv |
| CH-D  | Close semantics + try-send / try-recv                      | unit: drain-then-closed, send-after-close errors |
| CH-E  | cs-runtime primops + Scheme builtins                       | acceptance: rt.eval_str round trips |
| CH-F  | `channel-select` primop                                    | unit: 3-way select, after clause, else clause |
| CH-G  | `(select ...)` macro in lib/beam/channels.scm              | acceptance: select drives a fan-in test |
| CH-H  | SendableValue bridge — channels cross actor boundaries     | acceptance: A creates ch, sends to B via PID, B recvs from ch |
| CH-I  | `lib/beam/channels.scm` helpers (for-each, worker-pool, pipeline) | acceptance: worker pool processes N jobs across M workers |
| CH-J  | Microbench                                                  | uncontended send+recv < 1 μs at p50 |
| CH-K  | Documentation + ADR                                         | docs/adr/NNNN-channels.md describes the design decisions |

Estimated effort: ~2 weeks, similar shape to the cs-actor B1–B8
arc. Phases A-D are crate-local; E-H wire to cs-runtime; I-K
are library + docs.

## Alternatives considered

### Build channels on cs-table

cs-table has ordered storage and we already have a
mailbox-backed-by-cs-table path (`MailboxKind::Durable`).
Channels could ride the same fabric.

**Why we didn't**: cs-table is optimized for K/V lookup, not
FIFO drain. The OrderedSet `pop_first_ordered` we ship is the
existing primitive — usable but inefficient at high
throughput, since each pop takes the RwLock-write path through
a BTreeMap. tokio mpsc is purpose-built for FIFO, uses an
amortized-O(1) ring + signal pair.

We could offer a `ChannelKind::Durable` backing later (post-
1.0, parallel to mailbox Durable) for cases that need queue
introspection from outside the process.

### Make channels actors

A channel could be implemented as a "channel actor" that owns
its buffer and responds to send/recv messages.

**Why we didn't**: every send/recv would pay the actor
mailbox round-trip cost (envelope alloc, mailbox push, actor
wakeup, deserialize), which is ~70 μs (we measured in the
ActorLayer bench). tokio mpsc round-trip is < 1 μs. The 70×
overhead would make channels uneconomical for any high-
throughput pattern (worker pools especially).

### Go-style typed channels via cs-typer

cs-typer could enforce `(Channelof Integer)` at compile time.

**Why we didn't (v1)**: useful but orthogonal. Ship dynamic
channels first; layer the type narrowing on top once the
runtime semantics are stable.

### Async/await in Scheme syntax

We could add `async`/`await` as a Scheme syntactic concept
and let `channel-recv` be the canonical awaitable.

**Why we didn't**: actors + reductions already give us
cooperative scheduling without changing the language surface.
Adding async/await would introduce a second concurrency
mechanism, fragment the patterns library, and pull cs-runtime
toward a "two-color function" world that conflicts with our
"every Scheme proc is just a proc" stance.

## Locked decisions

Resolved during the design interview (2026-05-19); the spec
above reflects these:

1. **Unified receive.** `(receive)` accepts channel clauses
   alongside mailbox patterns; one primitive for "wait on
   first-ready of N inputs." Cost: cs-actor's mailbox
   primitives need a poll/select shape so the receive runtime
   can await both. Deferred to a follow-up — the MVP keeps
   `(receive)` mailbox-only and channels use `(select)`.

2. **Explicit close.** `(channel-close!)` is the primary
   lifecycle API. A `(with-channel (ch ...) body)` macro
   handles the auto-close-on-scope-exit ergonomic case.
   Rejected: GC-tracked auto-reclaim (would need finalizer
   hooks; complexity not justified for v1).

3. **MPMC default, broadcast separate.** `(make-channel)`
   creates an MPMC queue (one message goes to one receiver,
   work-stealing). `(make-broadcast-channel)` is the pub/sub
   variant. Two constructors so the semantics is visible at
   the call site.

4. **Asymmetric send.** `channel-send!` blocks indefinitely;
   no 3-arg timeout form. Programs that want bounded send
   patience use `channel-try-send!` in a retry loop, or
   `(select [(send! …) …] [(after …) …])`. Deliberate
   asymmetry vs `channel-recv` (which does take a timeout).

5. **Select fairness — random default, biased opt-in.**
   `(select …)` picks pseudo-randomly among simultaneously-
   ready clauses (matches Go semantics; prevents one channel
   from starving others). `(select #:fair 'biased …)` opts
   into deterministic clause-order priority — useful when
   you actually want "always prefer ch1 if ready."

6. **REPL outside-actor → error.** Channel ops that need a
   tokio context (blocking `channel-send!`/`recv` on a
   bounded-full or empty channel) signal `&channel-no-context`
   when called outside an actor body. Wrap in `(spawn …)`
   or `(run-channel-task body)` to get a runtime. try-*
   variants work everywhere because they're sync.

## Implementation status (2026-05-19)

Feature-complete for 1.0. `cs-channel` crate +
`cs-runtime/builtins/channel.rs` + 20 acceptance tests green.

Shipped:

- CH-A + CH-D + CH-E + CH-H — unbounded + bounded channels,
  close semantics, cs-runtime primops, cross-actor delivery
  via the existing SendableValue surface (no dedicated variant;
  the `(channel <id>)` pair carries naturally).
- CH-F + CH-G — `channel-select` primop + `(select …)` Scheme
  macro with `recv` / `send!` / `after` / `else` clauses,
  random-fairness default + `select-biased` opt-in.
- CH-B' — unbuffered rendezvous channels (`(make-channel 0)`),
  custom Mutex+oneshot protocol giving true sender-knows-
  receiver-got-it semantics.
- Broadcast channels (decision 3) — `(make-broadcast-channel
  cap)` + `broadcast-subscribe` + `broadcast-send!` etc. on
  top of `tokio::sync::broadcast`.
- CH-I — `(with-channel …)` macro + library helpers
  (`channel-for-each`, `channel-drain-to-list`) in
  `lib/beam/channels.scm`.

Deferred follow-ups (post-1.0):

- Unified receive (decision 1) — single primitive over mailbox
  + channels; the current design uses separate APIs.
- CH-J — microbench suite (send/recv throughput, contention).

## References

- Go channels: <https://go.dev/ref/spec#Channel_types>
- Racket channels (sync/async): <https://docs.racket-lang.org/reference/sync.html>
- tokio mpsc: <https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html>
- Hoare 1978 — Communicating Sequential Processes
  <https://dl.acm.org/doi/10.1145/359576.359585>
- BEAM-style runtime spec (this repo) for the actor +
  reduction context: `docs/research/beam_runtime_spec.md`

## What the work-rate looks like

The CH-A–CH-D phases are crate-local and parallelize with the
existing parallel-runtime work — about 3 days. CH-E–CH-H are
the cs-runtime bridge and tests — about 4 days. CH-I–CH-K
(library + bench + docs) — about 3 days. Total ~10 working
days for a single contributor; less if parallelized with the
contracts / layers / web-server work that landed in this
branch.

If we want a "minimum viable channels" landing earlier, ship
just CH-A + CH-D + CH-E + CH-H (unbounded + close + bridge +
cross-actor) — that's a useful subset in ~4 days. Bounded,
unbuffered, and select can come in a follow-up.
