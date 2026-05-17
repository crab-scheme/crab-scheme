# BEAM-style runtime — spec

> Status: research-stage draft (2026-05-17). Synthesizes parallel
> research on BEAM internals (process model, scheduler, message
> passing), ETS/Mnesia, hot code reload + function versioning, and
> OTP supervision into a forward-looking design for bolting these
> capabilities onto CrabScheme.

## Why this matters

CrabScheme today is a single-process, single-threaded R6RS implementation
with multi-tier execution (walker / VM / JIT / AOT). It's good at
running standalone Scheme programs. It's **not yet good at**:

- Running thousands of independent units of work without one bad
  iteration killing the whole process.
- Surviving a buggy function — the failure tears down everything.
- Updating code without restarting (no hot reload).
- Communicating between independent units with backpressure +
  isolation.
- Storing modest amounts of in-memory tabular data with concurrent
  access patterns (today: one hashtable per Scheme value, no shared
  catalog).

BEAM solves all five with one coherent design: per-process heaps,
preemptive scheduling, pure message passing, in-VM tables (ETS),
supervised restart hierarchies, and two-version code loading. The
operational story those add up to — "let it crash, then restart
the smallest correct unit" — is the reason WhatsApp runs ~300
bytes/connection at 2B users, Discord absorbs traffic spikes,
Heroku Router isolates tenant failures, and 25-year-old Erlang
systems still ship hot patches in production.

This spec proposes how to bring those five capabilities into
CrabScheme over the next ~12 months of work, organized into five
new crates that integrate with the existing Runtime / VM / JIT / AOT
stack.

## TL;DR architecture

```
                  ┌──────────────────────────────────────────────┐
                  │  Tokio multi-thread runtime (cs-actor)        │
                  │                                               │
   ┌──────────┐   │   ┌─────────┐  ┌─────────┐  ┌─────────┐      │
   │ Scheme   │   │   │  Actor  │  │  Actor  │  │  Actor  │ ...  │
   │ embedder │──▶│   │   #1    │  │   #2    │  │   #3    │      │
   └──────────┘   │   │ ┌─────┐ │  │ ┌─────┐ │  │ ┌─────┐ │      │
                  │   │ │ Run-│ │  │ │ Run-│ │  │ │ Run-│ │      │
                  │   │ │ time│ │  │ │ time│ │  │ │ time│ │      │
                  │   │ └─────┘ │  │ └─────┘ │  │ └─────┘ │      │
                  │   │ mailbox │  │ mailbox │  │ mailbox │      │
                  │   └─────────┘  └─────────┘  └─────────┘      │
                  │        │            │            │            │
                  └────────┼────────────┼────────────┼────────────┘
                           │            │            │
                  ┌────────┴────────────┴────────────┴────────────┐
                  │     cs-table — shared atomic tables (ETS-ish)  │
                  └────────────────────────────────────────────────┘
                           │            │            │
                  ┌────────┴────────────┴────────────┴────────────┐
                  │     cs-supervisor — restart trees + linking     │
                  └────────────────────────────────────────────────┘
                           │            │            │
                  ┌────────┴────────────┴────────────┴────────────┐
                  │     cs-hotreload — two-version code dispatch    │
                  └────────────────────────────────────────────────┘
                                  (optional)
                  ┌────────────────────────────────────────────────┐
                  │     cs-distrib — node-to-node actor handles     │
                  └────────────────────────────────────────────────┘
```

The load-bearing decision: **one Runtime per actor**, each with its
own `Rc<Frame>` env + private cs-gc heap. Actors talk only by sending
copies of Values through tokio `mpsc` channels. No shared mutable
Scheme state except through cs-table.

This mirrors BEAM's per-process-heap model exactly (each Erlang
process has a private 2.6 KB initial heap that grows independently)
and avoids the Tokio-actor pitfall — sharing a heap across actors
forces Arc<Mutex<…>> everywhere and dissolves the isolation that
makes "let it crash" work.

## Goals

| # | Goal | Measured by |
|---|------|-------------|
| G1 | Spawn 100k actors in a single process, each with its own Scheme env, with steady-state RSS < 1 GB. | Soak test in `bench/realworld/` (Phase F-style). |
| G2 | An actor that crashes (panic, OOM, infinite loop caught by reduction limit) does NOT kill the host process. | Conformance test that spawns a panicking actor and verifies the rest of the system runs. |
| G3 | Hot-reload a function definition while a long-running actor is mid-call, with explicit state migration. | E2E test: start counter actor at v1, hot-load v2 with extra field, observe migrated state. |
| G4 | Provide an in-memory KV table (cs-table) that supports concurrent reads from N actors + serialized writes. | t3g-table-contention bench; measure throughput under N=1..16 readers. |
| G5 | OTP-style supervision: a supervisor with N children, with restart policies (`permanent` / `transient` / `temporary`), restart intensity limits, and `one_for_one` / `one_for_all` / `rest_for_one` strategies. | Conformance: kill a child, verify the right restart policy fires; saturate restarts, verify supervisor escalates. |

## Non-goals (this spec)

- **Distributed Erlang** in v1. Single-node only. The `cs-distrib`
  block in the diagram is sketched but deferred to a follow-up.
- **Full OTP behaviors library** (gen_event, gen_statem, sys,
  release_handler). Start with the core (`gen_server`-equivalent +
  `supervisor`-equivalent), let users build the rest on top.
- **Distributed transactions** (Mnesia-class). cs-table is in-memory
  + single-node + non-transactional in v1.
- **Replacing the existing single-threaded `crabscheme run` mode**.
  The actor system is additive: `crabscheme run foo.scm` continues
  to work without spawning the tokio runtime; `crabscheme run-actor
  foo.scm` (or equivalent) opts in.
- **Native code hot-reload for AOT-compiled binaries**. AOT is by
  definition a single-shot compile; hot reload is a JIT/VM-tier
  feature.

## What we measure (success criteria)

- **Spawn cost**: median time to `(spawn (lambda () ...))`. Target:
  < 50 µs (BEAM is 3–17 µs; we're heavier per the per-actor Runtime
  but should be in the same order of magnitude).
- **Spawn footprint**: bytes RSS per actor. Target: < 16 KB
  (BEAM's 2.6 KB is from 30 years of arena tuning; we'll be 4–8×
  heavier early on).
- **Message latency**: round-trip `(call other-actor msg)`. Target:
  < 10 µs uncontended, < 100 µs at 1k concurrent actors.
- **Crash isolation**: 99th percentile latency for surviving actors
  when one actor crashes per second. Target: no observable spike.
- **Hot reload pause**: wall time the running actor is paused
  during `(reload-module! mod)`. Target: < 10 ms for a small module.

## Crate breakdown

### `cs-actor` (~3-4 KLoC)

Core actor primitive + tokio integration. Owns the runtime.

**Responsibilities**:
- Construct + manage tokio `Runtime` (multi-thread, work-stealing).
- Per-actor `Actor` struct holding (a) its own CrabScheme `Runtime`
  (the existing one, single-threaded — see "Why per-actor Runtime"
  below), (b) a tokio `mpsc::Receiver<Message>` for the mailbox,
  (c) a kill switch, (d) the supervisor link.
- Spawn API: `spawn(thunk) -> ActorRef`, `spawn_link`,
  `spawn_monitor`.
- Message send: `send(actor_ref, value)` (cast, fire-and-forget)
  and `call(actor_ref, value, timeout) -> value` (sync RPC).
- Reduction-based preemption: every N bytecode ops the VM yields to
  tokio's scheduler. Prevents one CPU-bound actor from blocking a
  worker thread for arbitrary time.
- Scheduler integration: ordinary actors run on tokio's default
  worker pool; actors that explicitly call FFI or do blocking I/O
  go through `spawn_blocking` (BEAM's "dirty scheduler" analogue).

**Key types**:
```rust
pub struct ActorRef {
    pid: ActorPid,
    inbox: tokio::sync::mpsc::Sender<Message>,
}

pub struct ActorPid {
    node: NodeId,         // 0 for local; future-proof for cs-distrib
    local_id: u64,
}

pub struct Actor {
    pid: ActorPid,
    runtime: cs_runtime::Runtime,
    inbox_rx: tokio::sync::mpsc::Receiver<Message>,
    links: Vec<ActorPid>,
    monitors: Vec<ActorPid>,
    state: ActorState,
}

pub enum ActorState { Running, Waiting, Trapping }

pub enum Message {
    User(Value),
    Exit { from: ActorPid, reason: ExitReason },
    Down { ref_id: u64, pid: ActorPid, reason: ExitReason },
    SystemReload { /* hot-reload payload */ },
    SystemPing(/* heartbeat / liveness */),
}
```

**Why per-actor Runtime**: the existing `cs_runtime::Runtime` is
~thoroughly Rc-based (Frames, Env, Pinned values, GC handles).
Refactoring to Arc-everything would be a multi-month undertaking
and would penalize the single-threaded fast path. Instead, every
actor gets its own Runtime. They don't share Scheme state; the
only cross-actor communication is message-passing (which copies
Values) and cs-table (which lives outside any actor's heap).

This matches BEAM exactly. Actors aren't lightweight in the
2.6KB-Erlang-process sense — each carries ~50-100 KB of Runtime
state — but they're cheap enough to spawn thousands, which is the
right target for CrabScheme's expected workloads.

**Integration with cs-vm**: the bytecode interpreter loop needs a
yield hook. Every N opcodes (say 4096), check whether the current
fiber's tokio task budget is exhausted; if so, `tokio::task::yield_now().await`.
Requires the VM dispatch loop to be `async fn` when running under
an actor. The existing synchronous entry point stays for
`crabscheme run`.

**Integration with cs-gc**: each actor has its own Heap (per
Phase A architecture). Global counters become per-actor counters
(reverting the Phase B/F static `AtomicU64` to per-Heap counters,
or stratifying by actor-id). This is a real backwards-incompat
that needs careful migration — flag in the rollout plan below.

### `cs-table` (~1.5 KLoC)

In-memory tables, ETS-shaped (`set` and `ordered_set` only in v1).

**Responsibilities**:
- Concurrent hash/btree-backed tables, addressable by name (a Symbol).
- Atomic Value insert / lookup / delete.
- Snapshot iteration (a la `ets:tab2list` but lazy).

**Design choices** (informed by the ETS research):
- v1 skips the `protected` / `public` / `private` access model. All
  tables are effectively public; cross-actor isolation comes from
  process boundaries, not table ACLs.
- v1 implements `set` (DashMap) and `ordered_set` (concurrent
  skiplist or RwLock<BTreeMap>). `bag` and `duplicate_bag` deferred
  — usable but not load-bearing for the common KV pattern.
- Value copying on read (BEAM semantics): the returned Value is a
  deep clone, so the caller can mutate it without races. This is
  cheap for Fixnum/Symbol; for Pair / Vector / String it costs an
  actual deep walk. Document as a hot-path consideration.
- No transactions / no Mnesia features. If users need multi-key
  atomicity, they wrap a single actor as the gatekeeper for the
  table (the "writer process" pattern that's idiomatic OTP).

**Key API**:
```scheme
(make-table 'users 'set)              ; create
(table-insert! 'users key value)
(table-lookup 'users key)             ; → value or #f
(table-delete! 'users key)
(table-size 'users)                   ; → integer
(table-fold 'users acc f)             ; lazy iteration
(table-list-names)                    ; → '(users sessions ...)
```

### `cs-supervisor` (~1 KLoC)

OTP `supervisor` behavior, Scheme-flavored.

**Responsibilities**:
- Child specs (`make-child-spec` returning a record with id, start
  MFA, restart policy, shutdown timeout, type).
- Strategies: `one_for_one`, `one_for_all`, `rest_for_one`. Defer
  `simple_one_for_one` (worker-pool case can be built on
  `one_for_one` + dynamic add).
- Restart intensity: `{max_restarts, period_seconds}` (default
  `{1, 5}`, matching OTP — aggressive, prevents cascades).
- Shutdown: `'brutal_kill | <integer-ms> | 'infinity`.
- Link semantics: supervisor traps exits so a child crash doesn't
  propagate to it; instead it pattern-matches the `'EXIT` and
  decides per the strategy.

**Scheme API**:
```scheme
(make-supervisor
  'my-tree
  '((id . worker-a)
    (start . (worker-a-module start))
    (restart . permanent)
    (shutdown . 5000)
    (type . worker))
  '((id . worker-b) ...)
  #:strategy 'one-for-one
  #:intensity 3
  #:period 10)

(supervisor-start sup-pid)
(supervisor-which-children sup-pid)
(supervisor-terminate-child sup-pid 'worker-a)
(supervisor-restart-child sup-pid 'worker-a)
```

### `cs-hotreload` (~1.5-2 KLoC)

Two-version code loading + per-call-site version dispatch.

**Responsibilities**:
- Module concept (today CrabScheme has loose top-level defines;
  cs-hotreload formalizes them into versioned modules).
- Version table: per-module, holds (old_version, current_version)
  function pointers. New load promotes current → old and writes
  the new version.
- Per-call-site dispatch: "remote" calls (fully-qualified
  `(my-module my-fn args)`) go through the export table and see the
  current version; "local" calls (lexically inside the module
  body) bind to the version they entered with — the same
  asymmetry BEAM uses, for the same reason: an actor mid-call can
  finish in the old code without inconsistency.
- Purge: `(code-soft-purge! 'my-module)` checks whether any actor
  still has the old version on its call stack; if yes, returns
  `#f`; if no, drops the old code and reclaims its literals.
  `(code-purge! 'my-module)` force-purges, killing offending
  actors.
- State migration: actors register a `code-change` callback. When
  a supervisor receives `(reload-module! 'my-module)`, it iterates
  its children, suspends each, calls their `code-change` callback
  with `(old-version, new-version, state)`, resumes.

**The hard parts** (called out in the research):
1. **JIT specialization vs hot reload** — Cranelift inlines callees
   on tier-up. A hot reload that changes an inlined callee leaves
   the inlined JIT code stale. Options: invalidate JIT on reload
   (simplest, costs warm-up), keep a dispatch table the JIT calls
   through (slower steady-state, no invalidation), or precise
   deoptimization (Cranelift doesn't yet ship this).
   
   **v1 pick**: invalidate the JIT for any code path that calls
   the reloaded module (transitive closure via the export table).
   The JIT re-warms on next call. Document the latency cliff.

2. **GC + old code literals** — old code holds references to its
   constant pool (Symbols, interned Strings). Those must stay
   alive while any actor still runs old code.
   
   **v1 pick**: tag each Heap allocation with a code-version
   epoch; on `code-purge`, scan all actor heaps and only free
   epoch X literals when no actor's call stack references epoch X.

3. **State migration ergonomics** — Erlang's `code_change/3` is
   famously awkward. We can do better with a Scheme syntax:
   `(define-state-migration my-module (from-version state) ...)`
   that the system invokes for each affected actor.

### `cs-distrib` (~3 KLoC, deferred to v2)

Node-to-node actor handles. Tokio's TCP framework + a binary
encoding for Values (`cs-core` has serialization primitives we can
reuse). EPMD-equivalent: simple service discovery via a known port.

Deferred because: single-node BEAM gets you very far. The hard
problems (net splits, distributed transactions, global locks)
deserve their own design pass.

## Scheme-facing surface

### Special forms / primops

```scheme
; Actor lifecycle
(spawn (lambda () body))            ; → ActorRef
(spawn-link  (lambda () body))      ; bidirectional link
(spawn-monitor (lambda () body))    ; (values ref pid)
(self)                              ; → my own PID
(exit pid 'reason)                  ; signal pid

; Messaging
(send pid value)                    ; cast (fire-and-forget)
(call pid value [timeout-ms])       ; sync RPC, returns reply
(receive
  ((pattern1) action1)
  ((pattern2) action2)
  (after timeout-ms timeout-action))

; Linking + monitoring
(link pid)
(unlink pid)
(monitor pid)                       ; → ref
(demonitor ref)
(process-flag 'trap-exit #t)        ; convert exits to messages

; Supervision (cs-supervisor)
(make-supervisor name children #:strategy s #:intensity n #:period p)
(supervisor-start sup)
(supervisor-which-children sup)
(supervisor-terminate-child sup child-id)

; Tables (cs-table)
(make-table name type)              ; type ∈ '(set ordered-set)
(table-insert! name key value)
(table-lookup name key)
(table-delete! name key)
(table-fold name acc f)

; Hot reload (cs-hotreload)
(load-module! 'my-module "path/to/mod.scm")
(reload-module! 'my-module "path/to/mod.scm")
(code-soft-purge! 'my-module)       ; → #t if safe, #f if blocked
(code-purge! 'my-module)            ; force, kills holders
(code-versions 'my-module)          ; → (old current)

(define-state-migration my-module
  ((from-version "1.0") state)
  ; produce migrated state for 2.0
  (cons 'new-field state))

; Reductions / yielding (mostly invisible to users)
(yield)                             ; manual hand-back to scheduler
```

### Behavior-style helper (`gen-server` analogue)

```scheme
(define-behavior counter
  #:init (lambda (initial-value) initial-value)
  #:handle-call
    (lambda (msg state)
      (case msg
        ((get) (values state state))
        ((inc) (values 'ok (+ state 1)))))
  #:handle-cast
    (lambda (msg state)
      (case msg
        ((reset) 0)
        (else state)))
  #:handle-info
    (lambda (msg state) state)
  #:terminate
    (lambda (reason state) #void)
  #:code-change
    (lambda (from-version state extra)
      (values 'ok state)))

(define c (counter-start 0))
(counter-call c 'get)                ; → 0
(counter-cast c 'inc)
(counter-call c 'get)                ; → 1
```

Macroexpands to a `spawn` + `receive` loop that dispatches per the
callbacks. The macro lives in a Scheme prelude (no special form
needed in the core language).

## Integration with existing CrabScheme

### Runtime + GC changes

**Backwards-incompatible** (will require an ADR):

- The Phase A/B/F global atomic counters in cs-gc must split into
  per-Heap counters. Each actor has its own Heap; cumulative byte
  / alloc counters become per-actor, accessible via the harness's
  per-actor `(gc-stats)` returned alist.
- `cs_gc::Heap` becomes `Send` (it's currently `!Send` because of
  the `Rc<Slot<T>>` inside). The Send-ness is conditional: a Heap
  is Send only while no Scheme code is actively running on it
  (i.e., between tokio task yields). The tokio scheduler enforces
  this by only handing a Heap to a worker thread when the
  associated actor is runnable.
- `cs_runtime::Runtime::active()` (the thread-local "current
  runtime" accessor used by builtins) becomes
  per-tokio-task-local, not process-global.

**Non-incompatible additions**:
- Bytecode dispatch loop gains a yield-check hook. The standalone
  `crabscheme run` path has the hook compiled out; actor-mode
  builds the hook in.
- New primops (spawn, send, etc.) live in cs-actor and are
  registered via the existing `pure_builtins()` /
  `higher_order_builtins()` mechanism cs-runtime already has.

### JIT (cs-jit-cranelift) changes

- Yield-check insertion: every N basic blocks the JIT inserts a
  call to a `__check_yield` runtime helper. Costs ~2 ns per check
  + the dispatcher overhead when a yield actually fires.
- Stack maps for hot reload: Cranelift's `enable_safepoints`
  setting + a custom safepoint emitter. We need precise root info
  so a paused actor's stack can be walked by `code-change`. This
  is meaningful work — cranelift's safepoint support is still
  evolving as of 0.131.
- JIT invalidation on reload: a new method on `Lowerer` that drops
  the JIT cache for a given module. Cranelift's JITModule supports
  freeing modules; we'd need to teach our tier-up logic to
  rebuild on next call.

### AOT (cs-aot) changes

AOT-emitted code is fundamentally single-shot — once compiled to a
Rust source and built, it can't be hot-reloaded. Two implications:

1. **AOT and hot-reload are exclusive per binary**. The harness
   picks at build time: either `crabscheme aot --hot-reload-safe`
   (emits dispatch-table code that goes through the export table,
   slower but reload-capable) or `crabscheme aot --no-reload`
   (current behavior, full direct-call elision).

2. **AOT-emitted code can be a child of a reload-capable
   supervisor**. The AOT-compiled module is treated as "version
   frozen at build time"; if someone calls `(reload-module! ...)`
   on it, the call fails with a clear error.

### Typer (cs-typer) changes

- New `Type` variants: `Pid`, `MonitorRef`, `Table`, `Supervisor`.
- Primops for spawn/send/call get typed signatures so users get
  arity / type errors at typecheck time.
- The behavior macros expand to typed code; `define-behavior`
  registers the callback signatures so call sites can be checked.

### docs/research + docs/adr

- This doc → `docs/research/beam_runtime_spec.md` (you're reading
  it).
- An ADR per major design call:
  - ADR-XX: One Runtime per actor (vs Arc-everything).
  - ADR-XX+1: Tokio mpsc for mailboxes (vs custom MPSC).
  - ADR-XX+2: cs-table as ETS-shaped but only `set` + `ordered_set`
    in v1.
  - ADR-XX+3: JIT-invalidation on reload (vs dispatch-table or
    deoptimization).

## Phased rollout

| Phase | Deliverable | Acceptance |
|-------|-------------|------------|
| **B1** | Per-Heap gc-stats migration (revert Phase B/F globals to per-Heap), threading audit | Existing bench/realworld results unchanged; cs-gc test that two Heaps maintain independent byte counters |
| **B2** | `cs-actor` skeleton — spawn / send / receive, single-threaded tokio runtime (one worker), no preemption | Smoke test: spawn 1000 actors, each prints its pid; full message round-trip across 100 actors |
| **B3** | Reduction-based preemption, work-stealing scheduler, yield hooks in VM | Soak test: 1000 actors each doing 10M ops, no one starves; latency p99 < 50 ms |
| **B4** | `cs-table` — set + ordered_set tables; the missing piece for actors that need shared state | t3g-table-contention bench at N=1..16 readers; correctness against Chez-style golden |
| **B5** | `cs-supervisor` — child specs, restart strategies, intensity limits, link/monitor | Conformance tests for each restart strategy + intensity cap |
| **B6** | `cs-hotreload` minimum — module loading, two-version table, basic per-call-site dispatch (no JIT invalidation yet) | E2E test: hot-reload a function, verify old in-flight calls finish, new calls hit new version |
| **B7** | `cs-hotreload` advanced — JIT invalidation, state migration callback, code-change ergonomics | Counter-state-migration E2E: 1.0 → 2.0 with added field |
| **B8** | `define-behavior` macro + standard library helpers | Port a small Erlang OTP example (a key-value-store gen_server) and verify shape matches |
| **B9** | Distributed actors (`cs-distrib`) — deferred until B1-B8 prove out | (Out of scope for v1) |

Each phase is 2-4 weeks. v1 (B1-B8) is ~6 months of focused work.

## Open questions

1. **Reduction counting unit** — Erlang uses "function calls + iter
   steps" as the unit. We could use bytecode-op count, or
   instructions-retired, or a real timer. Each has tradeoffs:
   - Bytecode-op: cheap to count, easy to tune, but JIT-translated
     functions skip the count → starvation risk on hot paths.
   - Instruction-retired: portable accuracy, but per-yield-check
     overhead is non-trivial (~5 ns).
   - Real timer: easiest tuning but `Instant::now()` is too slow
     for per-op checking.
   **Recommendation**: bytecode-op count + a JIT-side
   instruction-counter that compiles to a single decrement +
   branch. Re-check after benchmarking B3.

2. **Mailbox unbounded vs bounded** — Erlang mailboxes are
   unbounded (a process can be flooded). Tokio's `mpsc` is bounded
   by default. Unbounded matches BEAM semantics; bounded gives
   backpressure but breaks the "send is never blocking" invariant.
   **Recommendation**: unbounded by default, with an opt-in
   `(spawn-bounded N (lambda () ...))` for back-pressured actors.

3. **PID encoding** — BEAM packs `node + serial + id` into a
   tagged integer. CrabScheme's NB encoding has tag space — we
   could use one of the unused tag bits for `Pid`. Cheap pointer
   compares but limits actor count to 2^48 or so. Probably
   acceptable.

4. **What to do when an actor's Heap exceeds a threshold** — BEAM
   schedules GC on the offending process and only that process.
   With per-actor Heaps we get that for free. But we'd need a
   `(set-actor-heap-limit! pid bytes)` knob so users can cap
   runaway actors.

5. **Tokio vs custom executor** — Tokio is the obvious choice but
   it's an ~~enormous~~ large dep. An alternative is `smol` (much
   smaller) or a hand-rolled executor (tighter coupling to our
   reduction-counting model). For v1 — tokio. For v2 — measure and
   reconsider.

6. **How does this interact with the JIT?** — The JIT was designed
   for single-thread, no-preemption execution. The yield-check
   insertion is straightforward; what's NOT straightforward is
   that some JIT'd functions assume they own the Heap exclusively
   for their duration. Preemption invalidates that. Either:
   - JIT compiles yield-safe (every call site is a potential yield
     point — costs perf), or
   - JIT-compiled code runs without preemption and we count it as
     one "reduction" per entire call.
   **Recommendation**: option 2 for short JIT'd functions,
   option 1 for long ones (e.g., anything with a loop bigger than
   N iterations). Tier-up logic flags long functions for the
   yield-safe path.

7. **Hot reload + AOT** — fundamentally incompatible (AOT is a
   build artifact, not reloadable). The `--hot-reload-safe`
   compromise above is honest but ugly. Alternative: only allow
   hot reload of modules loaded via the VM/JIT tier, never AOT.
   Document this loudly. Users who want both write the
   reloadable parts as plain Scheme + the perf-critical
   non-reloadable parts as AOT.

8. **Cycle reclamation across actors** — cs-gc's mark-sweep only
   sees one Heap at a time. If actor A sends a Value to actor B
   that closes over a Heap-A reference, you have a cross-heap
   pointer that GC can't reclaim. The Erlang fix: send-copies
   semantics — every message is a deep clone, so there are never
   cross-heap pointers. **Recommendation**: enforce send-copies
   for v1; revisit only when profiling shows it's a hot path.

## References

- BEAM book — https://blog.stenmans.org/theBeamBook/
- Erlang ETS docs — https://www.erlang.org/doc/apps/stdlib/ets.html
- Erlang code-loading — https://www.erlang.org/doc/system/code_loading.html
- OTP design principles — https://www.erlang.org/doc/system/sup_princ.html
- gen_server module — https://www.erlang.org/doc/apps/stdlib/gen_server.html
- gen_statem module — https://www.erlang.org/doc/apps/stdlib/gen_statem.html
- Mnesia overview — https://www.erlang.org/doc/apps/mnesia/mnesia_overview.html
- "Deep Diving Into the Erlang Scheduler" (AppSignal, 2024)
- "Hot Reloading Code in Erlang" (malloc.dog, 2026)
- Ractor (Tokio-based actor lib) — https://slawlor.github.io/ractor/
- Pony reference capabilities —
  https://tutorial.ponylang.org/capabilities/reference-capabilities.html
- WhatsApp 2B-user scaling notes —
  https://scalewithchintan.com/blog/whatsapp-erlang-architecture-2-billion-users

## What the work-rate looks like

If this spec is greenlit, ~6 months of focused engineering for
v1 (B1-B8). The biggest risks:

1. **Per-actor Runtime ergonomics** — every cross-actor primop has
   to crisply distinguish "my Runtime" from "their Runtime."
   Likely 2-3 false starts on the API shape.
2. **Hot reload + JIT** — Cranelift's safepoint support is
   evolving. May require pinning to a specific cranelift version
   or contributing upstream.
3. **GC + threading** — moving the cs-gc Heap from `!Send` to
   `Send (only between yields)` is a real refactor. Audit-heavy.

Compared to the perf follow-ups already in the task list
(heap-rooting migration, inline pair storage, bump allocator), the
BEAM work is more **net new** and less **finish existing**. Both
matter; this spec is for when the team is ready to invest in
runtime breadth, not depth.
