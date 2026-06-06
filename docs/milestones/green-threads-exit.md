# green-threads — M1/M2 + scale exit report

> Status: **complete and review-ready.** Spec: `.spec-workflow/specs/green-threads/`.
> Predecessor: the parallel-runtime spec (which delivered green *activation*
> handlers). This milestone makes free-form `spawn-source` bodies — the shape
> every real actor app uses — run green, then drives per-actor memory down far
> enough that 50–100k concurrent actors is practical.

## The problem

Every `spawn-source` actor body (its own `(receive)`/`(tcp-recv)` loop — all of
crab-cache) ran on a **dedicated OS thread** (`block_in_place`): capped at
`max_blocking_threads` (4096), ~1–2 MiB of committed stack + scheduler overhead
each, so memory and the ceiling scaled with *connection count*, not work. The
green machinery the parallel-runtime work shipped (coroutine driver, cooperative
park) was reachable only by the framework-driven `spawn-activation` handler, not
free-form bodies.

## Track summary

### M1 — green execution (opt-in) — **complete** (PR #119)

A free-form body runs as a stackful coroutine on the parking `LocalSet` pool and
parks (releases its worker) on `(receive)`/`(raw-receive)`/`(sleep)`.

- `pump_coroutine` — the shared suspend/resume loop, extracted from
  `drive_handler` (so the per-message and whole-body drivers are one mechanism).
- `green_source_body` + `(spawn-source-green)` — the whole-body driver. **No body
  or primop changes**: the body's own `(raw-receive)`/`(sleep)` already route
  through the `YIELDER`-gated cooperative hooks, which the coroutine publishes
  (the spec's key insight, §0.2).
- `green_yield_hook` (`CoYield::Yield`) — a CPU-bound green body yields its shared
  worker via the cs-vm reduction-budget hook. This is the **green replacement for
  `cs_actor::tokio_yield_hook`**, which `block_on`s and so *panics* on a
  current-thread LocalSet worker.
- Link/monitor/trap-exit/panic parity (`ClearCtx` keeps the panic path hygienic);
  region-park guard (refuse to suspend with a `(with-region)` scope open — the
  shared TLS region stack would interleave).

### M2 — cooperative I/O + the flip — **complete** (PR #119)

- **Cooperative async `tcp-recv` + `tcp-send`** (the gating subsystem):
  `CoYield::Io`/`IoWrite` + `cs-stdlib-net` `install_async_recv`/`install_async_send`
  hooks (inverted dependency, like the sleep hook) + a per-worker tokio-stream
  cache. recv and send **ship together** — `try_clone` + `set_nonblocking` flips
  `O_NONBLOCK` on the shared file description, so a green conn's blocking
  `write_all` would `WouldBlock`.
- Green stack class (`GREEN_STACK_BYTES` = 1 MiB).
- **`(spawn-source)` flipped to green by default** + `(spawn-source-dedicated)`
  opt-in + `CRABSCHEME_ACTOR_DEFAULT` env hatch (ADR 0035).
- **crab-cache migrated** (PR #1): conn/pusher/broker green, shards + peer-poller
  dedicated (blocking fsync / Raft tick-clock). All gates green — conformance,
  crash-recovery (10k durable SETs survive `kill -9`), failover (no acked-write
  loss), linearizability — at Stage-1 throughput.

### Scale — shared Runtime + shared body compilation — **complete** (PRs #120, #121)

Measurement showed 50–100k was memory-bound (~826 KiB/actor), not stack- or
thread-bound. Two layered levers brought it down ~11–20×:

- **#120 — shared Runtime.** Each worker builds one immutable base (builtins +
  bundled libs); each green actor overlays it (`Runtime::from_image`): a
  `child_define_root` env (defines isolated, builtins resolve through the base) +
  a **base+extension `SymbolTable`** (`Rc` base + small per-actor extension —
  keeps `intern(&mut self)`, so **no 33-file `&mut SymbolTable` rewrite**).
  826 → ~118 KiB/actor.
- **#121 — shared body compilation.** Split the eval pipeline
  (`compile_program_via_vm` + `run_bytecode`; `cs_vm::run` borrows `&Bytecode`),
  then a per-worker per-source cache: actors running the *same* body reuse the
  compiled bytecode (sharing its code chunks) instead of re-compiling.
  118 → ~40 KiB (trivial) / ~72 KiB (200-define body).

## Discoveries surfaced during implementation

### The stack is not the RSS lever; the per-actor `Runtime` was

The spec assumed `GREEN_STACK_BYTES` tuning would gate scale. Measurement showed
otherwise: mmap is lazily committed (RSS = touched pages), and **VM-tier green
bodies stay shallow on the native stack** — a 2 M-deep non-tail recursion ran on
the 1 MiB stack with no overflow (the bytecode VM heap-allocates call frames). So
green bodies are effectively stack-overflow-safe via normal Scheme recursion, and
the stack size is only a *virtual*-footprint knob. The real cost was the
per-actor `Runtime` (~826 KiB), which redirected the whole scale effort.

### Measure before refining — the macros clone was negligible

Asked to do a macros base/overlay refinement, an empty-`macros` probe measured
the macros clone at **~1–2 KiB/actor** — a dead end. The residual was the **syms
clone** (~110 KiB). Pivoted to sharing the symbol interner instead (the actual
lever). (Same discipline caught the stack assumption above.)

### `tokio_yield_hook` panics on a current-thread worker

The existing reduction-yield hook does `Handle::block_on(yield_now())` — sound
under `block_in_place`, but `block_on` re-entrancy *panics* on a current-thread
LocalSet worker. The green path needs a coroutine-suspending yield hook
(`CoYield::Yield`), the only way to release a current-thread worker cooperatively.

### Neither path auto-chains a Scheme-level error

`b_beam_raw_receive` surfaces a trap-exit `Err` as an ordinary Scheme error, and
`scheme_source_entry`/`activation_body` log it and exit `Normal`; only a Rust
**panic** → `ExitReason::Error` (chained to links). Green matches — important so
the green path didn't silently diverge.

### The hard mmap ceiling is `vm.max_map_count`

One mmap per live coroutine stack → ~65k concurrent green actors on a
default-`sysctl` Linux before `mmap` fails, independent of stack size. 100k needs
the operator to raise `vm.max_map_count` (documented in ADR 0035).

## Scale measurements (idle-actor RSS probe, N=2000)

| layer | Δ/actor | 50k | 100k |
|---|--:|--:|--:|
| original (full `Runtime::new` per actor) | ~826 KiB | ~40 GiB | ~80 GiB |
| + shared base env (#120) | ~230 KiB | ~11.5 GiB | ~23 GiB |
| + shared base syms (#120) | ~118 KiB | ~5.9 GiB | ~11.8 GiB |
| + shared body compilation (#121) | **~40–72 KiB** | **~2–3.6 GiB** | **~4–7 GiB** |

(~72 KiB is a 200-`define` body; ~40 KiB a trivial one — the body-dependent part
is now shared.) Full method + per-N tables: `docs/measurements/green-threads-scale.md`.

## Decisions (recorded)

- **Green is the default** (`spawn-source`), with an explicit `spawn-source-dedicated`
  opt-in and an env hatch — ADR 0035.
- **Cooperative `tcp-send` shipped in v1** (forced by the shared-fd `O_NONBLOCK`).
- **Shared syms via base+extension**, not a shared `Rc<RefCell>` interner — avoids
  rewriting the 33-file `&mut SymbolTable` surface.

## Test coverage

New: `tests/green_parity.rs` (4 — link/monitor/trap-exit cross-path + region-park
guard), `tests/green_tcp.rs` (2 — real-socket round-trip + parked-recv doesn't
freeze a peer), green cases in `tests/cooperative_sleep.rs`, beam unit tests
(whole-body loop, multiplex), cs-vm define-boundary (2), cs-core base+extension
SymbolTable (1). Regression held throughout: cs-vm 65, cs-core symbol 3,
jit_differential 247, vm_conformance 74, beam 35, the cooperative/green suites,
and **crab-cache** conformance/crash-recovery/failover/linearizability.

## Follow-ups (not blocking)

- **`spawn-activation` could adopt `Runtime::from_image` + the body cache** too
  (same shared-base win for the framework-handler path; currently `Runtime::new`).
- **100k on Linux** needs `vm.max_map_count` raised (one VMA per coroutine stack).
- The remaining ~40–72 KiB/actor floor (coroutine-stack touched pages + per-actor
  closures/bindings) is genuinely per-actor; reducing it would need a different
  execution model.
- The PR stack (#118→#121) is based on the still-open #115→#117 chain; bases
  retarget to `main` as the chain merges.

## Conclusion

Free-form actor bodies are green by default, park on receive/sleep/socket-I/O,
and cost ~40–72 KiB each instead of an OS thread + ~826 KiB. The 4096-thread
ceiling is gone; **50k actors fit in ~2–3.6 GiB and 100k in ~4–7 GiB**, validated
end-to-end on crab-cache at no throughput loss. Shipped as a reviewable stack:
green execution (#119), shared Runtime (#120), shared body compilation (#121),
plus the crab-cache migration (#1).
