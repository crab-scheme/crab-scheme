# Green Threads — Design

> Status: **Draft**. Companion: `requirements.md`, `tasks.md`.
> All `file:line` pointers are against the tree at `e0ddcb0`
> (branch `perf/actor-vm-jit`). Line numbers will drift; symbol names are stable.

## 0. Architecture recap & reuse map

The whole design is **"run the free-form body on the same coroutine machinery
the activation handler already uses, generalized from one-message to whole-body,
plus one new suspend reason for socket reads."** Almost everything exists.

### 0.1 Machinery already shipped (reused as-is)

| Component | Location | Role here |
|---|---|---|
| `drive_handler` (per-message coroutine driver) | `cs-runtime/src/builtins/beam.rs:493` | Generalized into a shared `pump` loop (§1) |
| `driver_receive` (async mailbox pop + `process_received`) | `beam.rs:568` | Unchanged; the `Recv` arm calls it |
| `process_received` (trap-exit enforcement) | `beam.rs:1046` | Unchanged |
| `YIELDER` thread-local (`*const Yielder`) | `beam.rs:870` | Unchanged; the body's hooks read it |
| `cooperative_sleep_hook` | `beam.rs:1613` | Unchanged — already suspends a free-form body |
| `cooperative_raw_receive` | `beam.rs:1699` | Unchanged — already suspends a free-form body |
| `CoYield` / `CoResume` enums | `beam.rs:888` / `:901` | **Extended** with `Io` (§2) and `Yield` (§3) |
| `STACK_POOL` + `checkout_stack`/`checkin_stack` | `beam.rs:880`, `:919+` | Reused; add a green stack class (§5) |
| `LocalWorkerPool` / `spawn_local_activation` | `cs-actor/src/local_pool.rs`, `lib.rs:1349` | Reused verbatim to dispatch the whole-body future |
| cooperative-sleep install pattern | `cs-stdlib-time/src/lib.rs:143–159`, installed at `cs-runtime/src/lib.rs:310` | The exact template for the async-TCP hook (§2) |

### 0.2 The key insight

`cooperative_raw_receive` (`beam.rs:1699`) and `cooperative_sleep_hook`
(`beam.rs:1613`) already check "is a `YIELDER` installed on this thread?" and, if
so, **suspend the synchronous evaluator** back to the driver. They do **not**
care whether the code above them is a framework handler or a free-form body.

So: **if we run an entire `spawn-source` body inside a coroutine that publishes a
`YIELDER`, the body's own `(raw-receive)` / `(sleep)` calls — wherever they are
in its loop — already park cooperatively.** No body edits, no primop edits. The
only genuinely new suspend reason is the blocking socket read (§2), because
`tcp-recv` lives in `cs-stdlib-net` and has no `YIELDER` awareness yet.

## 1. G-A — Whole-body coroutine driver

### 1.1 What exists

`activation_body` (`beam.rs:422`) builds a per-actor `Runtime`, resolves the
handler, then loops: `actor.receive_async().await` → `drive_handler(&mut actor,
&mut rt, &handler, msg)` once per message. `drive_handler` (`beam.rs:493`) builds
a coroutine that runs **one** `rt.apply_value(&handler, &[msg])`, then pumps it
(`beam.rs:522–560`): install `ACTOR_CTX`/`YIELDER`/`REDUCTIONS`, `resume`, clear
context before any await, and service `Sleep`/`Recv`/`Return`.

The dedicated free-form path is `run_scheme_body` (`beam.rs:616`): build a
`Runtime`, `eval_str_via_vm(source)` to load, resolve `entry`, then
`eval_str_via_vm("(entry 'a0 …)")` to run the body to completion **on the calling
(dedicated) thread**, blocking on every `(raw-receive)`.

### 1.2 What changes

**Refactor the pump loop out of `drive_handler`** into a shared async helper so
both the per-message driver and the new whole-body driver use identical
suspend/resume/clear-context logic:

```rust
// beam.rs — new, extracted verbatim from drive_handler:522–560
async fn pump_coroutine(
    co: &mut Coroutine<CoResume, CoYield, Result<Value, String>, DefaultStack>,
    actor_ptr: *mut cs_actor::Actor,
) -> Result<Value, String> {
    let mut cached_yielder: *const Yielder<CoResume, CoYield> = std::ptr::null();
    let mut resume_input = CoResume::Woke;
    loop {
        ACTOR_CTX.with(|c| c.set(actor_ptr));
        YIELDER.with(|y| y.set(cached_yielder));
        REDUCTIONS.with(|c| c.set(0));
        let result = co.resume(resume_input);
        if cached_yielder.is_null() { cached_yielder = YIELDER.with(|y| y.get()); }
        ACTOR_CTX.with(|c| c.set(std::ptr::null_mut()));   // INV-4: clear before await
        YIELDER.with(|y| y.set(std::ptr::null()));
        match result {
            CoroutineResult::Yield(CoYield::Sleep(d)) => {
                tokio::time::sleep(d).await; resume_input = CoResume::Woke;
            }
            CoroutineResult::Yield(CoYield::Recv { timeout }) => {
                let act = unsafe { &mut *actor_ptr };
                resume_input = CoResume::Received(driver_receive(act, timeout).await);
            }
            CoroutineResult::Yield(CoYield::Io { handle, max }) =>   // §2 (NEW)
                resume_input = CoResume::Io(driver_tcp_recv(handle, max).await),
            CoroutineResult::Yield(CoYield::Yield) =>                // §3 (NEW)
                { tokio::task::yield_now().await; resume_input = CoResume::Woke; }
            CoroutineResult::Return(outcome) => {
                checkin_stack(co.into_stack()); return outcome;
            }
        }
    }
}
```

`drive_handler` becomes a thin wrapper: build the one-shot handler coroutine,
`pump_coroutine(&mut co, actor_ptr).await`.

**Add the whole-body green driver** — the free-form analog of `activation_body`,
the green analog of `run_scheme_body`:

```rust
// beam.rs — NEW
async fn green_source_body(
    mut actor: cs_actor::Actor, source: String, entry: String, args: Vec<SendableValue>,
) {
    let mut rt = crate::Runtime::new();
    // Load on the VM tier in the driver frame (no YIELDER yet — top-level
    // (define …) does not park). Mirrors run_scheme_body:624.
    if let Err(d) = rt.eval_str_via_vm("<green-source>", &source) {
        eprintln!("spawn-source(green): load failed: {d:?}"); return;
    }
    // Resolve + render the call exactly like run_scheme_body:626–640.
    let call = match resolve_and_build_call(&rt, &entry, &args) {
        Ok(c) => c, Err(e) => { eprintln!("spawn-source(green): {e}"); return; }
    };
    let rt_ptr: *mut crate::Runtime = &mut rt;
    // `co` is declared AFTER `rt`/`actor`, so it drops FIRST: corosensei
    // force-unwinds the frozen Scheme stack (running Rc dtors that touch rt's
    // heap) before rt drops. DO NOT hoist `co` above `rt`. (beam.rs:486–492.)
    let mut co: Coroutine<CoResume, CoYield, Result<Value, String>, DefaultStack> =
        Coroutine::with_stack(checkout_green_stack(), move |yielder, _first| {
            YIELDER.with(|y| y.set(yielder as *const _));
            let rt = unsafe { &mut *rt_ptr };
            rt.eval_str_via_vm("<green-body>", &call)   // the whole body runs here;
                                                        // its (raw-receive)/(sleep)/
                                                        // (tcp-recv) suspend cooperatively
        });
    let outcome = pump_coroutine(&mut co, &mut actor as *mut _).await;
    let reason = match outcome {
        Ok(_) => cs_actor::ExitReason::Normal,
        Err(e) => cs_actor::ExitReason::Error(e),       // §6 — termination parity
    };
    // on_actor_termination is driven by spawn_local_activation's wrapper
    // (lib.rs:1389–1398); returning here ends the future with `reason` surfaced
    // there. (See §6 for the panic path.)
    let _ = reason; // wired through the dispatch wrapper, not returned directly
}
```

**Dispatch** (`primop_spawn_source`, `beam.rs:377`) selects green vs dedicated
(§4). The green arm calls `spawn_local_activation` (`lib.rs:1349`) with
`move |actor| green_source_body(actor, source, entry, args)`. The dedicated arm
is today's `spawn_sync_body_on_task` (`beam.rs:386`), unchanged.

### 1.3 Soundness

Identical to `drive_handler`'s argument (`beam.rs:474–492`): single worker
thread, strictly alternating control, `rt`/`actor` borrowed for strictly longer
than `co`. The **only** new lifetime fact is that `co` now lives for the *whole
actor life* (the body loops until the connection closes), not one message — so
`rt` and `actor` must live in `green_source_body`'s frame above `co` (they do).

## 2. G-B — Cooperative async TCP (the gating subsystem)

### 2.1 What exists

`(tcp-recv sock max)` → `tcp_recv` (`cs-stdlib-net/src/lib.rs:297`) clones the
`std::net::TcpStream` out of the global `Mutex<Registry>` (`lib.rs:67`, slots:
`HashMap<i64, Slot>`), releases the lock, then **blocks** on
`stream.read(&mut buf)` (`lib.rs:327`). On a dedicated thread that is correct; on
a shared green worker it freezes every co-located actor.

### 2.2 What changes — mirror the cooperative-sleep hook exactly

**(a) `cs-stdlib-net`: an installable async-recv hook** (template:
`cs-stdlib-time/src/lib.rs:143–159`).

```rust
// cs-stdlib-net/src/lib.rs — NEW
// Returns Some(result) if a cooperative driver handled the read (green path);
// None to fall through to the blocking read (dedicated / non-actor) — unchanged.
static ASYNC_RECV: OnceLock<fn(i64, usize) -> Option<Result<Vec<u8>, String>>> = OnceLock::new();
pub fn install_async_recv(hook: fn(i64, usize) -> Option<Result<Vec<u8>, String>>) {
    let _ = ASYNC_RECV.set(hook);
}
// Hand the driver a clone of the std stream for `id` so it can build a tokio
// stream for the async read. No tokio types cross the crate boundary.
pub fn clone_tcp_std(id: i64) -> Option<std::net::TcpStream> {
    let r = registry().lock().ok()?;
    match r.slots.get(&id) { Some(Slot::Tcp(s)) => s.try_clone().ok(), _ => None }
}
```

In `tcp_recv`, before the blocking read (`lib.rs:~305`):

```rust
if let Some(hook) = ASYNC_RECV.get() {
    if let Some(res) = hook(id, max_len as usize) {     // cooperative path took it
        return res.map(bv_value).map_err(FfiError::HostFailure);
    }
}
// …unchanged blocking read (lib.rs:305–329)…
```

**(b) `cs-runtime`/beam: the hook + the driver-side read.**

```rust
// beam.rs — the installed hook. Runs DEEP inside tcp_recv, inside the coroutine.
fn cooperative_tcp_recv_hook(handle: i64, max: usize) -> Option<Result<Vec<u8>, String>> {
    let yielder = YIELDER.with(|c| c.get());
    if yielder.is_null() { return None; }               // dedicated/non-actor → blocking
    match unsafe { (*yielder).suspend(CoYield::Io { handle, max }) } {
        CoResume::Io(res) => Some(res),
        _ => Some(Err("tcp-recv: internal error (resumed without bytes)".into())),
    }
}

// beam.rs — the driver side (called from pump_coroutine's Io arm). Owns a
// per-worker fd→tokio-stream cache so a long-lived conn doesn't re-clone+re-
// register on every read.
thread_local! { static TOKIO_TCP: RefCell<HashMap<i64, tokio::net::TcpStream>> = …; }
async fn driver_tcp_recv(handle: i64, max: usize) -> Result<Vec<u8>, String> {
    // get-or-build the tokio stream for this handle
    //   first time: cs_stdlib_net::clone_tcp_std(handle) → set_nonblocking(true)
    //               → tokio::net::TcpStream::from_std → cache
    // then: let n = stream.read(&mut buf).await;  buf.truncate(n);  Ok(buf)
    //   n == 0 ⇒ clean EOF ⇒ Ok(empty)  (preserves tcp_recv's EOF contract, lib.rs:328)
}
```

**(c) `CoYield` / `CoResume` extension** (`beam.rs:888` / `:901`):

```rust
enum CoYield  { Sleep(Duration), Recv { timeout: Option<u64> },
                Io { handle: i64, max: usize },   // NEW
                Yield }                            // §3
enum CoResume { Woke, Received(Result<Option<SendableValue>, String>),
                Io(Result<Vec<u8>, String>) }      // NEW
```

**(d) Install at startup** next to the sleep hook (`cs-runtime/src/lib.rs:310`):

```rust
#[cfg(feature = "<net>")]
cs_stdlib_net::install_async_recv(builtins::beam::cooperative_tcp_recv_hook);
```

This also establishes the `cs-runtime → cs-stdlib-net` dependency the driver
needs for `clone_tcp_std` — the same direction the sleep hook already uses for
`cs-stdlib-time`. **Verify the dep + the feature gate** (Task P2.0): the net
builtins are feature-gated, so the install call, the `Io` pump arm, and
`driver_tcp_recv` are all `#[cfg]`-gated; with the feature off, `CoYield::Io`
never arises.

### 2.3 Scope notes

- **`tcp-send` is cooperative in v1 (decision locked).** Symmetric with the
  recv path: `CoYield::IoWrite { handle, bytes }` + `cs_stdlib_net::install_async_send`
  consulted at the top of `tcp_send` (`lib.rs:~275`); the driver writes via
  `AsyncWriteExt::write_all` on the same per-worker cached tokio stream, then
  resumes. Covers the heavy pub/sub `pusher` writing to a slow reader without
  freezing co-located actors. (`tcp-send` currently blocks at `lib.rs:291`.)
- **`tcp-accept` stays blocking/dedicated.** The accept loop (`node.scm:78`,
  `node-cluster.scm:131`) is **one** actor that spawns one conn per accept — keep
  it dedicated (single thread). The multiplexed hot path is per-connection
  `tcp-recv`, which is what we make cooperative.
- **wasm:** `tcp-recv`/`tcp-listen`/`tcp-accept` already error on `wasm32-wasi`
  (`lib.rs:210,234`); the hook is native-only and changes nothing there.

## 3. G-C — Non-`block_on` green yield hook

### 3.1 What exists / why it breaks green

`run_actor_body` (`beam.rs:804`) installs `cs_actor::tokio_yield_hook`
(`lib.rs:1482`) as cs-vm's yield hook; the bytecode dispatch loop calls it every
N reductions. That hook does `Handle::current().block_on(yield_now())`
(`lib.rs:1489`) — sound under `block_in_place` on the multi-thread runtime, but
on a **current-thread LocalSet worker it panics** (`block_on` re-entrancy). So a
CPU-bound green actor with no receive/sleep/io would either panic (if it kept
that hook) or monopolize its worker (if it had none).

### 3.2 What changes

Add a **coroutine-suspending** yield hook for the green path:

```rust
enum CoYield { …, Yield }   // (already shown in §2c)

// beam.rs — installed by green_source_body's coroutine in place of tokio_yield_hook
fn green_yield_hook() {
    let y = YIELDER.with(|c| c.get());
    if !y.is_null() { unsafe { (*y).suspend(CoYield::Yield); } }   // resumes with Woke
}
```

The green body installs `green_yield_hook` via `cs_vm::vm::install_yield_hook`
(the same install point `run_actor_body` uses, `beam.rs:804`), guarded by the
same RAII `Guard` (`beam.rs:817–838`) so a pooled worker reused by other code
doesn't inherit it. `pump_coroutine`'s `Yield` arm (`§1.2`) does
`tokio::task::yield_now().await`, releasing the worker for one tick.

**Fallback if scope must shrink:** install *no* yield hook on the green path and
document the limitation (green CPU-bound actors yield only at receive/sleep/io
points). We prefer the hook — the suspend machinery is already there and S4
requires no-starvation.

## 4. G-D — Default flip, opt-in, crab-cache migration

### 4.1 Surface decision *(locked: flip the default)*

`(spawn-source …)` **becomes green by default.** Introduce
**`(spawn-source-dedicated …)`** for thread-owning actors (blocking fsync /
sole-drainer roles — INV-2/INV-3). This requires auditing every in-tree
`spawn-source` caller (cs-web, examples, tests) for "must own a thread" actors
and pinning them to the dedicated variant (Task P4.1).

Ship an **escape hatch**: `CRABSCHEME_ACTOR_DEFAULT=dedicated|green` read in
`primop_spawn_source` (`beam.rs:377`) so the default is reversible without code
changes (S5). During M1 the green path is reachable only via the explicit
`spawn-source-green` builtin (default stays dedicated); the flip lands in M2,
*after* cooperative TCP (INV-1), at which point `spawn-source-green` becomes an
alias of the now-green `spawn-source`.

### 4.2 Dispatch

`primop_spawn_source` (`beam.rs:377`) branches on the chosen model:
green → `spawn_local_activation(green_source_body…)`; dedicated → today's
`spawn_sync_body_on_task(run_actor_body…)` (`beam.rs:386`). `primop_spawn`
(`beam.rs:335`, the Rust-closure variant) stays dedicated (its bodies are
arbitrary Rust; not in scope).

### 4.3 crab-cache migration map (≈ 5 spawn-site edits; **INV-1: only after §2**)

| Site | Actor | Model | Why |
|---|---|---|---|
| `src/node.scm:78`, `node-cluster.scm:131` | **conn** | **green** | I/O-bound, one per client → the scale win |
| `src/server/conn.scm:163` | **pusher** | **green** | per-subscriber fan-out, I/O-bound |
| `src/node.scm:44`, `node-cluster.scm:106` | **broker** (pub/sub) | **green** | message-routing, mailbox-bound |
| `src/node.scm:60`, `node-cluster.scm:87` | **shard** | **dedicated** | **INV-2** blocking RocksDB fsync |
| `src/node-cluster.scm:96` | **peer-poller** | **dedicated** | **INV-3** Raft clock + sole drainer |

(`src/shard.scm:39` already uses `spawn-activation` — unaffected.)

## 5. G-E — Stack sizing & RSS

### 5.1 The model change

In the per-message driver, only a handler **suspended in `(sleep)`** holds its
2 MiB stack across `.await`; a non-sleeping handler checks a stack out and back
in within one `drive_handler` call (`beam.rs:874–879`). In the **whole-body**
model the body is *always* parked (its loop blocks on receive/io), so **every
green actor holds its coroutine stack for its entire life.** N conns ⇒ N stacks.

mmap is lazily committed (`beam.rs:910–911`), so RSS = *touched* pages = the call
depth at the park point, not the full reservation. A conn parked at `(let loop …
(tcp-recv))` is shallow.

### 5.2 What changes

- Add `GREEN_STACK_BYTES` (e.g. **256–512 KiB**) distinct from the per-handler
  `ACTOR_STACK_BYTES = 2 MiB` (`beam.rs:912`), with `checkout_green_stack()` /
  `checkin_green_stack()` over a separate pool (or a size-tagged
  `STACK_POOL`). Conn bodies are shallow at the park; deep non-tail recursion
  inside a body is the risk to measure (guard page must fault cleanly).
- Re-evaluate `STACK_POOL_CAP = 64` (`beam.rs:917`): whole-body stacks are held
  for life, not recycled per call — the pool only smooths spawn/die churn, so the
  cap governs *burst* reuse, not steady-state residency.

### 5.3 Gate

Measure RSS at 1k and 10k concurrent shallow-parked green conns (S1). Add a test
that deep recursion in a green body overflows the guard page **cleanly** (process
abort with a clear message, never UB).

## 6. G-F — Link / monitor / DOWN / panic parity

### 6.1 What changes

- **Normal / error return.** `green_source_body` maps the coroutine outcome to
  `ExitReason::Normal` / `ExitReason::Error(e)` (`§1.2`); these flow through
  `spawn_local_activation`'s wrapper to `on_actor_termination`
  (`lib.rs:1389–1398`), the same sink the dedicated path uses (`lib.rs:1152`).
- **Panic.** The body runs inside corosensei; a panic on `resume` must be
  captured and turned into `ExitReason::Error` — parity with the dedicated
  `catch_unwind` (`lib.rs:1143`) and the green wrapper's `FutureExt::catch_unwind`
  (`lib.rs:1389`). Decide capture point: inside `pump_coroutine` around
  `co.resume`, or let it propagate to the wrapper's existing catch_unwind.
- **Trap-exit.** Already handled: the body's `(raw-receive)` → `driver_receive`
  → `process_received` (`beam.rs:1046`) raises an `Err` for a non-`Normal` `Exit`
  to a non-trapping actor; that `Err` becomes the coroutine's `Return(Err)` →
  `ExitReason::Error` → linked actors get a chained `Exit`.

### 6.2 Gate

Link a green actor ↔ a dedicated actor (both directions); kill one; assert the
other gets `Exit`/`DOWN`. A trap-exit green actor survives a linked crash.
Monitor a green actor from a dedicated one and vice-versa; assert `DOWN` on
termination.

## 7. Cross-cutting hazard — region scope across a suspend

(See requirements §7.) The whole-body driver **must refuse to suspend with an
open `(with-region)` scope.** Implementation: at each suspend point in
`pump_coroutine`, assert the current TLS region-stack depth equals the depth at
body entry; if not, return `ExitReason::Error("cannot park inside (with-region):
region scope would span a suspend")`. crab-cache's green candidates don't use
`(with-region)`, so this only guards misuse. Save/restore-around-suspend (snapshot
the suspending actor's region-stack tail, restore on resume) is the proper fix
and a tracked follow-up — it depends on the same region-stack-as-task-local work
the parallel-runtime C3 track left blocked on `Region: Send` (`beam.rs:343–356`).

## 8. Soundness recap

Every new raw-pointer use (`rt_ptr`, `actor_ptr`, `YIELDER`) repeats the existing
`drive_handler` discipline (`beam.rs:474–492`): one single-threaded worker;
control strictly alternates; context (`ACTOR_CTX`/`YIELDER`) cleared before every
`.await` (INV-4); pointees (`rt`/`actor`) outlive the coroutine by frame ordering.
The whole-body change extends the coroutine's *lifetime* (one message → whole
actor) but not its *thread* or its *aliasing rules*. The new `Io`/`Yield`
suspends are structurally identical to `Sleep`/`Recv`: suspend in the body,
service in the driver while the coroutine is frozen, resume with a result.
