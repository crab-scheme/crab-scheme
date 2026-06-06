# Green Threads — Tasks

> Status: **Draft**. Companion: `requirements.md`, `design.md`.

Seven tracks (P0–P7), each split into small, independently-mergeable iterations
with a specific acceptance gate. **Order is load-bearing (INV-1):** the
`spawn-source` default must not flip to green (P4) until cooperative async TCP
(P2) and the green yield hook (P3) land and parity (P6) is proven — a blocking
`tcp-recv` or a panicking `block_on` yield hook on a shared worker would wedge
co-located actors. P1 ships behind an explicit opt-in so it is testable in
isolation before any default changes.

Every iteration: `cargo fmt` + `clippy` clean on touched crates; full
`cs-runtime` actor + cooperative_sleep/receive suites stay green; commit per
iteration (no end-of-track bundles).

---

## Track P0 — Region-scope-across-suspend guard *(cross-cutting; do early)*

### P0.1 — Refuse to park inside `(with-region)`
- [ ] Record the TLS region-stack depth at green-body entry (`green_source_body`).
- [ ] In `pump_coroutine`, before each `Sleep`/`Recv`/`Io`/`Yield` await, assert
  current region-stack depth == entry depth; on mismatch, terminate the actor
  with `ExitReason::Error("cannot park inside (with-region): region scope would
  span a suspend")`.
- [ ] Document the latent equivalent in the existing per-message `drive_handler`.
- [ ] **Gate:** a green actor that opens `(with-region)` then `(raw-receive)`s
  terminates with the clear error (not corruption); a green actor that opens +
  closes a region *between* receives runs fine.

---

## Track P1 — Whole-body green driver *(G-A; behind opt-in)*

### P1.1 — Extract the shared pump loop
- [ ] Lift `drive_handler`'s loop body (`beam.rs:522–560`) into
  `async fn pump_coroutine(co, actor_ptr) -> Result<Value,String>`.
- [ ] Re-express `drive_handler` as a thin wrapper over `pump_coroutine`.
- [ ] **Gate:** existing `spawn-activation` + cooperative_sleep/receive tests
  stay green (pure refactor, no behavior change).

### P1.2 — `green_source_body` + `checkout_green_stack`
- [ ] Add `green_source_body(actor, source, entry, args)` (design §1.2): build
  `Runtime`, `eval_str_via_vm(source)`, `resolve_and_build_call`, run the whole
  `(entry 'a…)` call inside a coroutine that publishes `YIELDER`; pump it.
- [ ] Factor `resolve_and_build_call` out of `run_scheme_body` (`beam.rs:626–640`)
  for reuse by both paths.
- [ ] Enforce drop-order (`co` declared after `rt`/`actor`); add the same
  safety/drop-order doc block as `drive_handler` (`beam.rs:474–492`).
- [ ] **Gate:** unit test — a green actor whose body is
  `(let loop () (let ((m (raw-receive))) (send (sender m) (work m)) (loop)))`
  echoes 100 messages; assert all replies and that the body parked (worker
  released) between messages.

### P1.3 — Opt-in surface `(spawn-source-green …)`
- [ ] Add `primop_spawn_source_green` (mirror `primop_spawn_source`,
  `beam.rs:377`) dispatching to `spawn_local_activation(green_source_body…)`.
- [ ] Register the `spawn-source-green` builtin; leave `spawn-source` dedicated
  (default unchanged this track — INV-1).
- [ ] **Gate:** `CRABSCHEME_ACTOR_WORKERS=1` + 500 green echo actors via
  `spawn-source-green`, each pinged once, all 500 reply (multiplexing proven).

### P1.4 — `(sleep)` / timed `(raw-receive)` on the whole-body path
- [ ] Test that `(sleep)` and `(raw-receive timeout)` inside a green
  `spawn-source-green` body park cooperatively (reuse the `Sleep`/`Recv` arms;
  no new code expected — proves the §0.2 insight end-to-end).
- [ ] **Gate:** two green actors on one worker; A sleeps 50 ms mid-body; B is
  serviced during A's sleep; A resumes after.

---

## Track P2 — Cooperative async TCP *(G-B; the gating subsystem)*

### P2.0 — Dependency & feature-gate audit
- [ ] Confirm/add `cs-runtime → cs-stdlib-net` dep (same direction the sleep hook
  uses for `cs-stdlib-time`).
- [ ] Decide the cfg gate for the install call, the `Io` pump arm, and
  `driver_tcp_recv` (net builtins are feature-gated); with the feature off,
  `CoYield::Io` must be unconstructable / unreachable.
- [ ] **Gate:** workspace builds with the net feature **off** and **on**, no
  warnings.

### P2.1 — `cs-stdlib-net` hook + std-stream accessor
- [ ] Add `install_async_recv(hook)` over a `OnceLock` (template:
  `cs-stdlib-time/src/lib.rs:143–159`) and `clone_tcp_std(id) ->
  Option<std::net::TcpStream>`.
- [ ] In `tcp_recv` (`lib.rs:297`), consult the hook before the blocking read;
  `Some(res)` → return it, `None` → unchanged blocking path. Preserve the EOF
  contract (empty bytevector on clean EOF).
- [ ] **Gate:** `cs-stdlib-net` unit tests green; with no hook installed,
  `tcp-recv` behaves exactly as before (blocking).

### P2.2 — `CoYield::Io` / `CoResume::Io` + the beam hook
- [ ] Extend `CoYield`/`CoResume` (`beam.rs:888`/`:901`) with `Io`.
- [ ] Add `cooperative_tcp_recv_hook` (suspend `Io` when `YIELDER` set, else
  `None`) and install it next to the sleep hook (`cs-runtime/src/lib.rs:310`).
- [ ] **Gate:** unit test — a green body calling `tcp-recv` on a socket with
  buffered data reads it via the cooperative path (assert the hook fired, e.g.
  via a counter), not the blocking path.

### P2.3 — `driver_tcp_recv` + per-worker tokio-stream cache
- [ ] Add the `Io` arm to `pump_coroutine` → `driver_tcp_recv(handle, max)`.
- [ ] `driver_tcp_recv`: per-worker `RefCell<HashMap<i64, tokio::net::TcpStream>>`;
  first read for a handle clones via `clone_tcp_std`, `set_nonblocking(true)`,
  `TcpStream::from_std`, caches; reads via `AsyncReadExt::read`; `n == 0` ⇒
  `Ok(empty)`.
- [ ] Evict the cache entry on EOF / error / `tcp-close`.
- [ ] **Gate:** two green conn actors on one worker, one parked in `tcp-recv`
  with no data; the other is served promptly (parked read released the worker).
  A full request/response round-trips over a real socket on the green path.

### P2.4 — Cooperative `tcp-send` *(in v1)*
- [ ] `CoYield::IoWrite { handle, bytes }` + `CoResume::IoWrite` + `cs_stdlib_net::
  install_async_send` + a beam `cooperative_tcp_send_hook`, mirroring P2.1–P2.3.
- [ ] Driver `Yield(IoWrite)` arm → `driver_tcp_send(handle, bytes)` via
  `AsyncWriteExt::write_all` on the same per-worker cached tokio stream; resume.
- [ ] **Gate:** a green pusher fanning out to a slow (backpressured) reader does
  not freeze co-located actors on its worker.

---

## Track P3 — Green yield hook *(G-C)*

### P3.1 — `CoYield::Yield` + `green_yield_hook`
- [ ] Add `CoYield::Yield`; add `green_yield_hook` (suspend `Yield` when
  `YIELDER` set); pump `Yield` arm → `tokio::task::yield_now().await`.
- [ ] Green body installs `green_yield_hook` (not `tokio_yield_hook`) via
  `cs_vm::vm::install_yield_hook`, under the same RAII `Guard` discipline as
  `run_actor_body` (`beam.rs:804,817–838`).
- [ ] **Gate:** a CPU-bound green actor (tight loop, no receive) co-located with
  a responder; a message to the responder is processed within a bounded time
  (no `block_on` panic, no starvation) — the S4 adversarial test.

---

## Track P4 — Default flip + crab-cache migration *(G-D; GATED on P2+P3+P6)*

### P4.0 — Flip surface + escape hatch *(decision locked: flip to green)*
- [ ] Make green the `spawn-source` default; add `spawn-source-dedicated`;
  `spawn-source-green` becomes an alias of `spawn-source`. Record in an ADR
  (next number after 0034).
- [ ] Add `CRABSCHEME_ACTOR_DEFAULT=dedicated|green` read in
  `primop_spawn_source` (`beam.rs:377`).
- [ ] **Gate:** ADR merged; env override flips the model with no code change.

### P4.1 — In-tree caller audit
- [ ] Audit every `spawn-source` caller (cs-web, examples, tests) for
  "must own a thread" actors (blocking syscalls / fsync / sole-drainer roles);
  pin those to dedicated.
- [ ] **Gate:** workspace test suite green under the new default.

### P4.2 — crab-cache migration *(design §4.3; only after P2 lands — INV-1)*
- [ ] conn (`node.scm:78`, `node-cluster.scm:131`), pusher (`conn.scm:163`),
  broker (`node.scm:44`, `node-cluster.scm:106`) → green.
- [ ] shard (`node.scm:60`, `node-cluster.scm:87`) → **dedicated** (INV-2);
  peer-poller (`node-cluster.scm:96`) → **dedicated** (INV-3).
- [ ] **Gate:** `bench/single-node.sh` (conformance), `bench/cluster.sh
  failover`, `bench/crash-recovery.sh` all green; durable SET throughput ≥ Stage 1
  (`/tmp/cc-vsredis-final.md`) — shards still fsync in parallel.

---

## Track P5 — Stack sizing & RSS *(G-E)*

### P5.1 — Green stack class
- [ ] Add `GREEN_STACK_BYTES` (256–512 KiB) + `checkout_green_stack` /
  `checkin_green_stack` over a separate (or size-tagged) pool; re-evaluate
  `STACK_POOL_CAP` (`beam.rs:917`) for the held-for-life model.
- [ ] **Gate:** green echo + conn tests pass on the smaller stack.

### P5.2 — RSS measurement + scale finding *(done — `docs/measurements/green-threads-scale.md`)*
- [x] Measured per-green-actor RSS: **~826 KiB/actor** (N=2000), dominated by the
  per-actor `Runtime` (not the stack). 10k≈8 GiB / 50k≈40 GiB / 100k≈80 GiB.
- [x] **Corrects the spec's assumption:** the coroutine stack is *not* the RSS
  lever — VM-tier bodies stay shallow (a 2 M-deep non-tail recursion ran on the
  1 MiB green stack with no overflow → green bodies are effectively stack-safe;
  `GREEN_STACK_BYTES` is only a virtual-footprint knob). Real lever = a **shared
  Runtime** (next milestone); hard ceiling = `vm.max_map_count` (~65k).
- [x] Guard-page: unreachable via Scheme recursion on the VM tier (heap-allocated
  call frames); corosensei's guard page remains a backstop for native recursion.

### Shared-Runtime *(new milestone — designed in `docs/measurements/green-threads-scale.md`)*
- [ ] Split `Runtime` → per-worker immutable `RuntimeImage` (`Rc`-shared builtins
  env + bundled libs + base symbols) + per-actor `RuntimeInstance` (child overlay
  env for defines + per-actor mutable state).
- [ ] Walls: `DefineGlobal`-at-root → per-actor define boundary; shareable
  `SymbolTable`; per-actor isolation of macros/pinned + const-folder base/overlay
  awareness. **Gate:** < ~50 KiB/actor overlay; full suite green.

---

## Track P6 — Link / monitor / DOWN / panic parity *(G-F; builds on P1)*

### P6.1 — Termination mapping + panic capture
- [ ] `green_source_body` maps coroutine `Ok`/`Err` → `ExitReason::Normal`/`Error`
  to `on_actor_termination` (parity with `lib.rs:1152`/`:1398`).
- [ ] Capture a panic on `co.resume` → `ExitReason::Error` (parity with the
  dedicated `catch_unwind` `lib.rs:1143` / green wrapper `lib.rs:1389`).
- [ ] **Gate:** a green actor that `(error …)`s and one that panics each
  terminate with the right reason; a linked dedicated actor is notified.

### P6.2 — Link / monitor / trap-exit cross-path
- [ ] Tests: green↔dedicated link (both directions) delivers `Exit` on crash;
  green↔dedicated monitor delivers `DOWN`; a trap-exit green actor survives a
  linked crash (via `process_received`, `beam.rs:1046`).
- [ ] **Gate:** all cross-path link/monitor/trap-exit tests pass deterministically
  (10 runs).

---

## Track P7 — Validation & bench *(ships the spec)*

### P7.1 — Scale + soak
- [ ] Bench: **50k–100k** concurrent green conns, mixed GET/SET, sustained;
  assert no 4096-style failure, bounded RSS, throughput within noise of Stage 1.
- [ ] **Gate:** S1 + S3 met; soak (≥10 min) shows no leak / no stack-pool growth.

### P7.2 — Linearizability regression check
- [ ] Re-run `bench/linearizability.sh` green vs the pre-change baseline; confirm
  dup rate is **no worse** than the documented non-idempotent-replay baseline
  (this spec must not worsen it).
- [ ] **Gate:** S2 met; if dup rate moves, root-cause before ship.

### P7.3 — Docs + exit report
- [ ] Update `docs/user/` (actor model: green default, `spawn-source-dedicated`,
  the env hatch, the region-park guard).
- [ ] Write `docs/milestones/green-threads-exit.md`.
- [ ] **Gate:** docs review.

---

## Dependency graph

```
P0.1 ─────────────────────────┐ (guard; independent, land early)
                              ↓
P1.1 → P1.2 → P1.3 → P1.4 ────┼──────────────→ P6.1 → P6.2
                              │                   │
P2.0 → P2.1 → P2.2 → P2.3 ────┤(→ P2.4 opt)       │
                              │                   │
P3.1 ─────────────────────────┤                   │
                              │                   │
                              ▼                   ▼
                          P4.0 → P4.1 → P4.2 ←── (parity proven)
                              │
P5.1 → P5.2 ──────────────────┤
                              ▼
                    P7.1 → P7.2 → P7.3
```

INV-1 edge: **P4.2 (crab-cache green conns) must not land before P2.3.**

## Milestone gates

### M1 — Green execution, opt-in (P0 + P1 + P3 + P6)
`(spawn-source-green …)` runs free-form bodies green with full park/parity, CPU
yield, and the region guard. `spawn-source` default unchanged; TCP still blocking
(so green conns aren't yet useful for the cache — receive/sleep-bound green
actors are). Independently shippable PR.

### M2 — Cooperative TCP + flip (P2 + P5 + P4)
`tcp-recv` **and** `tcp-send` cooperative on the green path; green becomes the
`spawn-source` default (with the env hatch + `spawn-source-dedicated`);
crab-cache conn/pusher/broker green, shards/poller dedicated. INV-1 satisfied.
Independently shippable PR.

### M3 — Validation (P7)
Scale (10k conns), no-regression (throughput, linearizability), docs + exit
report. Ship.

Recommend landing M1 → M2 → M3 as three sequential PRs, mirroring the
parallel-runtime spec's three-milestone cadence.
