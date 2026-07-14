//! `LocalWorkerPool` — the engine behind `ActorSystem::spawn_local_activation`
//! (#30 iter-2a / ADR 0032).
//!
//! ## Why this exists
//!
//! A Scheme actor's heap is `Rc`-everywhere (`Value: !Send`). The
//! existing async path ([`crate::ActorSystem::spawn_async`]) requires
//! `Fut: Send`, so an actor that holds its heap **across** a mailbox
//! `await` cannot ride the multi-thread runtime. The fallback —
//! [`crate::ActorSystem::spawn_sync_body_on_task`] — uses
//! `block_in_place`, which pins one worker (≈ one OS thread) for the
//! actor's whole life and so is capped by `max_blocking_threads(4096)`.
//!
//! A pool of OS threads, each running a **current-thread** tokio runtime
//! hosting a [`LocalSet`], hosts `!Send` futures via
//! `tokio::task::spawn_local`. Many such futures multiplex onto one
//! worker thread, each parking (releasing the thread) when it `await`s an
//! empty mailbox. That breaks the 4096 ceiling for mailbox-bound actors.
//!
//! ## Thread-affinity, not migration
//!
//! An actor is pinned to the worker it was dispatched to, for life. There
//! is **no** work-stealing / migration — that needs `Send` actor heaps
//! (iter-2b). The win here is M:N multiplexing with affinity.
//!
//! ## The `Send` seam
//!
//! The closure handed to a worker ([`WorkerJob`]) crosses the dispatch
//! channel, so it must be `Send`. The `!Send` future it builds is created
//! **on** the worker thread (inside the `LocalSet` context) and never
//! crosses back — exactly mirroring how `spawn-source` actors build their
//! `Rc` heap on the spawned thread (`beam.rs::run_scheme_body`).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::task::LocalSet;

use crate::ActorPid;

// ---------- cs-845.4: worker-stall watchdog (diagnostic only) ----------
//
// Every `LocalWorkerPool` worker gets a cheap [`WorkerHeartbeat`]: a
// last-progress timestamp (millis since the pool's epoch `Instant`) plus the
// pid of whichever actor is currently inside `Coroutine::resume` on that
// worker. Two call sites bump it — the job-receive loop in [`worker_main`]
// and the resume/suspend transition in `cs-runtime`'s `pump_coroutine`
// (via the free functions below) — both already on the hot path so the
// added cost is one `Relaxed` store.
//
// A single background thread (spawned only when
// `CRABSCHEME_WORKER_WATCHDOG_MS` is set) polls every worker's heartbeat at
// `stall_ms / 2` and logs one warning per stall episode (heartbeat older
// than `stall_ms` while an actor is recorded as running) plus one recovery
// line when it clears. Diagnostic only: no preemption, no killing.

/// Epoch all workers' `last_progress_ms` timestamps are measured from.
/// Process-wide (not per-pool) so it never needs re-anchoring; a `OnceLock`
/// keeps `Instant::now()` off the hot path after the first touch.
fn watchdog_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    watchdog_epoch().elapsed().as_millis() as u64
}

/// Per-worker heartbeat state, diagnostic-only. `current_pid_*` uses
/// `u64::MAX` as a "no actor currently running" sentinel; the node/local_id
/// halves aren't updated atomically together, which is fine here — a
/// momentarily-mismatched pid pair only affects the watchdog's blame log,
/// never program behavior.
struct WorkerHeartbeat {
    last_progress_ms: AtomicU64,
    current_pid_node: AtomicU64,
    current_pid_local: AtomicU64,
}

const NO_PID: u64 = u64::MAX;

impl WorkerHeartbeat {
    fn new() -> Self {
        Self {
            last_progress_ms: AtomicU64::new(now_ms()),
            current_pid_node: AtomicU64::new(NO_PID),
            current_pid_local: AtomicU64::new(NO_PID),
        }
    }

    fn tick(&self) {
        self.last_progress_ms.store(now_ms(), Ordering::Relaxed);
    }

    fn set_running(&self, pid: ActorPid) {
        self.current_pid_node
            .store(pid.node as u64, Ordering::Relaxed);
        self.current_pid_local
            .store(pid.local_id, Ordering::Relaxed);
        self.tick();
    }

    fn clear_running(&self) {
        self.current_pid_node.store(NO_PID, Ordering::Relaxed);
        self.current_pid_local.store(NO_PID, Ordering::Relaxed);
    }

    /// `Some(pid)` iff an actor is currently inside a `resume()` on this
    /// worker (i.e. the worker is doing actor work right now, not idling or
    /// cooperatively parked).
    fn running_pid(&self) -> Option<ActorPid> {
        let local = self.current_pid_local.load(Ordering::Relaxed);
        if local == NO_PID {
            return None;
        }
        let node = self.current_pid_node.load(Ordering::Relaxed) as u16;
        Some(ActorPid {
            node,
            local_id: local,
        })
    }

    fn age_ms(&self) -> u64 {
        now_ms().saturating_sub(self.last_progress_ms.load(Ordering::Relaxed))
    }
}

std::thread_local! {
    /// The heartbeat of whichever `LocalWorkerPool` worker owns the current
    /// thread, installed once by [`worker_main`]. `None` on any other
    /// thread (e.g. dedicated-actor threads, the test harness) — the
    /// heartbeat calls below become no-ops there.
    static CURRENT_HEARTBEAT: std::cell::RefCell<Option<Arc<WorkerHeartbeat>>> =
        const { std::cell::RefCell::new(None) };
}

/// Install `hb` as the current thread's worker heartbeat. Called once by
/// [`worker_main`] before entering its job-receive loop.
fn install_heartbeat(hb: Arc<WorkerHeartbeat>) {
    CURRENT_HEARTBEAT.with(|c| *c.borrow_mut() = Some(hb));
}

/// Record a cheap liveness tick for the current worker thread (job-receive
/// loop iteration). No-op off a `LocalWorkerPool` worker thread.
pub fn heartbeat_tick() {
    CURRENT_HEARTBEAT.with(|c| {
        if let Some(hb) = c.borrow().as_ref() {
            hb.tick();
        }
    });
}

/// Mark `pid` as the actor currently running (inside `resume()`) on this
/// worker, and tick. Call immediately before resuming its coroutine.
pub fn heartbeat_running(pid: ActorPid) {
    CURRENT_HEARTBEAT.with(|c| {
        if let Some(hb) = c.borrow().as_ref() {
            hb.set_running(pid);
        }
    });
}

/// Clear the "currently running" pid for this worker. Call right after a
/// `resume()` returns (suspend or completion) — the worker is no longer
/// doing CPU-bound actor work, so it should never be blamed for a stall
/// while cooperatively parked.
pub fn heartbeat_idle() {
    CURRENT_HEARTBEAT.with(|c| {
        if let Some(hb) = c.borrow().as_ref() {
            hb.clear_running();
        }
    });
}

/// A unit of work handed to a worker thread. It runs **on** the worker,
/// inside the `LocalSet` context, so it may call
/// `tokio::task::spawn_local` to launch a `!Send` future. The boxed
/// closure itself must be `Send` (it crosses the dispatch channel); the
/// future it spawns need not be.
type WorkerJob = Box<dyn FnOnce() + Send + 'static>;

struct Worker {
    /// `None` once the pool's `Drop` has closed this worker's channel.
    job_tx: Option<mpsc::UnboundedSender<WorkerJob>>,
    /// `None` once joined in `Drop`.
    handle: Option<JoinHandle<()>>,
    /// Shared with the watchdog thread (if any) and with this worker's own
    /// thread (installed via [`install_heartbeat`]).
    heartbeat: Arc<WorkerHeartbeat>,
}

/// A fixed pool of single-threaded tokio workers, each hosting a
/// `LocalSet` that can run `!Send` actor futures. Dispatch is
/// round-robin; an actor stays on its assigned worker for life.
pub struct LocalWorkerPool {
    workers: Vec<Worker>,
    cursor: AtomicUsize,
    /// Process-unique pool identity, tagged onto every stall event so
    /// observers (tests especially) can tell this pool's watchdog output
    /// apart from any other pool's in the same process.
    pool_id: u64,
    /// The stall watchdog (cs-845.4), only spawned when
    /// `CRABSCHEME_WORKER_WATCHDOG_MS` is set (or an explicit `stall_ms` is
    /// passed to [`Self::with_watchdog`]); `None` (zero cost beyond the
    /// heartbeat stores) otherwise.
    watchdog: Option<JoinHandle<()>>,
    watchdog_shutdown: Arc<AtomicBool>,
}

impl LocalWorkerPool {
    /// Build a pool of `n_workers` threads (clamped to ≥ 1). Each thread
    /// stands up its own current-thread runtime + `LocalSet` and parks
    /// waiting for jobs. Watchdog config comes from
    /// `CRABSCHEME_WORKER_WATCHDOG_MS` (default OFF).
    pub fn new(n_workers: usize) -> Self {
        Self::with_watchdog(n_workers, stall_ms_from_env())
    }

    /// [`Self::new`] with an explicit watchdog threshold instead of the env
    /// var — `Some(stall_ms)` enables the stall watchdog, `None` leaves it
    /// off. Lets tests configure the watchdog without racy process-global
    /// `set_var` calls.
    pub fn with_watchdog(n_workers: usize, stall_ms: Option<u64>) -> Self {
        static POOL_IDS: AtomicU64 = AtomicU64::new(0);
        let pool_id = POOL_IDS.fetch_add(1, Ordering::Relaxed);
        let n = n_workers.max(1);
        let mut workers = Vec::with_capacity(n);
        for i in 0..n {
            let (job_tx, job_rx) = mpsc::unbounded_channel::<WorkerJob>();
            let heartbeat = Arc::new(WorkerHeartbeat::new());
            let worker_heartbeat = Arc::clone(&heartbeat);
            let handle = std::thread::Builder::new()
                .name(format!("cs-actor-local-{i}"))
                // cw-m9c (G1): a green actor body runs inside its own
                // corosensei coroutine stack (GREEN_STACK_BYTES, beam.rs —
                // that's the real lever for a `(receive)`-ing a large flat
                // list, since `to_sendable_in`/`from_sendable` recurse one
                // Rust stack frame per cons cell). This is just the shallow
                // outer OS-thread stack the LocalSet dispatch loop itself
                // runs on; a small bump over the 2 MiB default is cheap
                // (lazily-committed virtual) defense-in-depth.
                .stack_size(16 * 1024 * 1024)
                .spawn(move || worker_main(job_rx, worker_heartbeat))
                .expect("spawn cs-actor local worker thread");
            workers.push(Worker {
                job_tx: Some(job_tx),
                handle: Some(handle),
                heartbeat,
            });
        }
        let watchdog_shutdown = Arc::new(AtomicBool::new(false));
        let watchdog = stall_ms.map(|stall_ms| {
            let heartbeats: Vec<Arc<WorkerHeartbeat>> =
                workers.iter().map(|w| Arc::clone(&w.heartbeat)).collect();
            let shutdown = Arc::clone(&watchdog_shutdown);
            std::thread::Builder::new()
                .name("cs-actor-worker-watchdog".to_string())
                .spawn(move || watchdog_main(pool_id, heartbeats, stall_ms, shutdown))
                .expect("spawn cs-actor worker watchdog thread")
        });
        Self {
            workers,
            cursor: AtomicUsize::new(0),
            pool_id,
            watchdog,
            watchdog_shutdown,
        }
    }

    /// This pool's process-unique identity — matches the `pool_id` on stall
    /// events emitted by this pool's watchdog.
    pub fn pool_id(&self) -> u64 {
        self.pool_id
    }

    /// Hand `job` to the next worker (round-robin). The job runs on that
    /// worker's thread, inside its `LocalSet`, so it may `spawn_local` a
    /// `!Send` future. Returns `false` if the chosen worker has already
    /// shut down (its channel is closed) — the caller should treat that
    /// as the actor never having started.
    pub fn dispatch(&self, job: WorkerJob) -> bool {
        let n = self.workers.len();
        // `% n` with n ≥ 1 (guaranteed by `new`) is always valid.
        let idx = self.cursor.fetch_add(1, Ordering::Relaxed) % n;
        match &self.workers[idx].job_tx {
            Some(tx) => tx.send(job).is_ok(),
            None => false,
        }
    }

    /// Number of worker threads in the pool.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }
}

impl Drop for LocalWorkerPool {
    fn drop(&mut self) {
        // Close every dispatch channel first so each worker's recv loop
        // sees `None` and its `block_on` returns; then join so the
        // threads (and their runtimes, and any still-parked actor
        // futures) are torn down before we return.
        for w in &mut self.workers {
            w.job_tx = None;
        }
        for w in &mut self.workers {
            if let Some(h) = w.handle.take() {
                let _ = h.join();
            }
        }
        // Signal the watchdog (if any) and join it so no thread outlives
        // the pool.
        self.watchdog_shutdown.store(true, Ordering::Relaxed);
        if let Some(h) = self.watchdog.take() {
            let _ = h.join();
        }
    }
}

/// One stall-episode transition, mirrored into the [`stall_events`] hook
/// alongside the eprintln. Observers prefer this over scraping stderr —
/// it's exact and race-free.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct StallEvent {
    /// Which pool's watchdog emitted this (see [`LocalWorkerPool::pool_id`]).
    pub pool_id: u64,
    /// Worker index within that pool.
    pub worker: usize,
    /// The blamed / recovering actor. `None` only on the recovery emitted
    /// after the stalled actor already finished its `resume()` (running pid
    /// cleared before the watchdog's next poll saw the fresh heartbeat).
    pub pid: Option<ActorPid>,
    /// Heartbeat age at emission. 0 on a pid-less recovery — there is no
    /// meaningful "stall age" once the worker is idle again.
    pub age_ms: u64,
    /// `false` = stall warning, `true` = recovery.
    pub recovered: bool,
}

/// Observation hook for watchdog stall/recovery events (cs-845.4). Hidden,
/// stability-exempt API: exists so tests (including cs-runtime integration
/// tests, which can't see a `#[cfg(test)]` hook across crates) can assert on
/// watchdog behavior without scraping stderr. Recording only happens on
/// stall-episode transitions (rare), never on the hot path.
#[doc(hidden)]
pub mod stall_events {
    use super::StallEvent;
    use std::collections::VecDeque;
    use std::sync::{Mutex, OnceLock};

    /// Bounded so a long-lived process with the watchdog enabled can't grow
    /// the sink without bound; observers snapshot promptly in practice.
    const CAP: usize = 1024;

    fn sink() -> &'static Mutex<VecDeque<StallEvent>> {
        static SINK: OnceLock<Mutex<VecDeque<StallEvent>>> = OnceLock::new();
        SINK.get_or_init(|| Mutex::new(VecDeque::new()))
    }

    pub(super) fn record(ev: StallEvent) {
        let mut q = sink().lock().unwrap();
        if q.len() >= CAP {
            q.pop_front();
        }
        q.push_back(ev);
    }

    /// A copy of every event currently retained (process-wide, all pools).
    /// Non-destructive so concurrent observers can't steal each other's
    /// events — filter by `pool_id` and/or `pid` for your own.
    pub fn snapshot() -> Vec<StallEvent> {
        sink().lock().unwrap().iter().cloned().collect()
    }
}

/// Read `CRABSCHEME_WORKER_WATCHDOG_MS`; `Some(stall_ms)` enables the
/// watchdog, `None` (unset, empty, or unparseable) leaves it off — the
/// default is OFF.
fn stall_ms_from_env() -> Option<u64> {
    std::env::var("CRABSCHEME_WORKER_WATCHDOG_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
}

/// The watchdog thread's body: poll every worker's heartbeat every
/// `stall_ms / 2` and log a warning the moment a worker with a currently-
/// running actor goes stale, then a recovery line once it ticks again.
/// Diagnostic only — never touches the worker, never preempts.
fn watchdog_main(
    pool_id: u64,
    heartbeats: Vec<Arc<WorkerHeartbeat>>,
    stall_ms: u64,
    shutdown: Arc<AtomicBool>,
) {
    let poll_every = Duration::from_millis((stall_ms / 2).max(1));
    // Per-worker "already warned, awaiting recovery" flag — local to this
    // thread, so no extra atomics on the hot path.
    let mut stalled = vec![false; heartbeats.len()];
    while !shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(poll_every);
        for (idx, hb) in heartbeats.iter().enumerate() {
            let Some(pid) = hb.running_pid() else {
                // No actor currently in `resume()` on this worker: either
                // idle or cooperatively parked — never a stall to blame.
                if stalled[idx] {
                    stalled[idx] = false;
                    eprintln!("cs-actor watchdog: worker {idx} recovered");
                    stall_events::record(StallEvent {
                        pool_id,
                        worker: idx,
                        pid: None,
                        age_ms: 0,
                        recovered: true,
                    });
                }
                continue;
            };
            let age = hb.age_ms();
            if age >= stall_ms {
                if !stalled[idx] {
                    stalled[idx] = true;
                    eprintln!(
                        "cs-actor watchdog: worker {idx} appears stalled ({age}ms since last \
                         progress, threshold {stall_ms}ms) — actor {pid} may be blocking the \
                         worker non-cooperatively"
                    );
                    stall_events::record(StallEvent {
                        pool_id,
                        worker: idx,
                        pid: Some(pid),
                        age_ms: age,
                        recovered: false,
                    });
                }
            } else if stalled[idx] {
                stalled[idx] = false;
                eprintln!("cs-actor watchdog: worker {idx} recovered (actor {pid})");
                stall_events::record(StallEvent {
                    pool_id,
                    worker: idx,
                    pid: Some(pid),
                    age_ms: age,
                    recovered: true,
                });
            }
        }
    }
}

/// A worker thread's entry point: build a current-thread runtime + a
/// `LocalSet`, then drive the job-receive loop on it. Each received job
/// runs inside the `LocalSet` context, so jobs may `spawn_local` `!Send`
/// futures that the same `block_on` continues to poll concurrently with
/// the receive loop. When the channel closes (pool drop), the loop ends
/// and `block_on` returns, dropping the runtime.
fn worker_main(mut job_rx: mpsc::UnboundedReceiver<WorkerJob>, heartbeat: Arc<WorkerHeartbeat>) {
    install_heartbeat(heartbeat);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime for cs-actor local worker");
    let local = LocalSet::new();
    local.block_on(&rt, async move {
        while let Some(job) = job_rx.recv().await {
            // Choke point 1 (cs-845.4): a received job is the coarsest
            // liveness signal — proves the receive loop itself isn't
            // wedged before the job even starts running its own body.
            heartbeat_tick();
            job();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::time::Duration;

    /// A pool with 2 workers runs `!Send` futures (each holds an `Rc`
    /// across an `await`) and they all complete. The `Rc` makes the
    /// future `!Send`, so this would not compile on `spawn_async` — it
    /// proves the `LocalSet` hosting works.
    #[test]
    fn local_pool_runs_non_send_futures() {
        let pool = LocalWorkerPool::new(2);
        assert_eq!(pool.worker_count(), 2);
        let done = Arc::new(AtomicU64::new(0));
        let total = 200u64;
        for _ in 0..total {
            let done = Arc::clone(&done);
            let ok = pool.dispatch(Box::new(move || {
                tokio::task::spawn_local(async move {
                    // `Rc` is `!Send`; holding it across the await is the
                    // whole point — a `LocalSet` permits it.
                    let cell = Rc::new(std::cell::Cell::new(0u64));
                    for _ in 0..5 {
                        tokio::task::yield_now().await;
                        cell.set(cell.get() + 1);
                    }
                    if cell.get() == 5 {
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }));
            assert!(ok, "dispatch should succeed while the pool is alive");
        }
        // Spin until every future has completed (or time out). The pool's
        // workers drive the futures on their own threads.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while done.load(Ordering::Relaxed) < total {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out: only {}/{} futures completed",
                done.load(Ordering::Relaxed),
                total
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(done.load(Ordering::Relaxed), total);
    }

    /// Dispatch round-robins across workers (cursor advances).
    #[test]
    fn dispatch_is_round_robin() {
        let pool = LocalWorkerPool::new(4);
        // Four dispatches should touch four distinct workers; we can't
        // observe the index directly, but we can confirm the cursor math
        // by dispatching a no-op many times without panicking and that
        // the pool reports the right width.
        assert_eq!(pool.worker_count(), 4);
        for _ in 0..16 {
            assert!(pool.dispatch(Box::new(|| {
                tokio::task::spawn_local(async {});
            })));
        }
    }

    /// After the pool is dropped, its worker threads are joined and torn
    /// down cleanly (no hang). This is a smoke test for `Drop`.
    #[test]
    fn pool_drop_joins_workers() {
        let pool = LocalWorkerPool::new(3);
        for _ in 0..10 {
            pool.dispatch(Box::new(|| {
                tokio::task::spawn_local(async {
                    tokio::task::yield_now().await;
                });
            }));
        }
        drop(pool); // must return promptly; would hang if Drop were wrong
    }

    // ---------- cs-845.4: worker-stall watchdog ----------
    //
    // These tests configure the watchdog via the explicit `with_watchdog`
    // constructor (no racy process-global `set_var`) and read the shared
    // `stall_events` sink via non-destructive `snapshot()`, filtering on
    // their own pool's `pool_id` — so they are safe to run in parallel with
    // each other and with any other pool-constructing test in this binary.

    /// This pool's events recorded so far.
    fn my_events(pool: &LocalWorkerPool) -> Vec<StallEvent> {
        stall_events::snapshot()
            .into_iter()
            .filter(|e| e.pool_id == pool.pool_id())
            .collect()
    }

    /// A worker whose job closure directly simulates a non-cooperative
    /// blocking op: it marks itself "running" (exactly as
    /// `pump_coroutine` does before `co.resume`) and then calls
    /// `std::thread::sleep` *synchronously in the job closure* — no
    /// `.await`, so this really does freeze the worker thread the way an
    /// un-hooked blocking builtin would, not just a slow cooperative task.
    fn dispatch_blocking_job(pool: &LocalWorkerPool, pid: ActorPid, blocked_for: Duration) {
        let ok = pool.dispatch(Box::new(move || {
            heartbeat_running(pid);
            std::thread::sleep(blocked_for);
            heartbeat_idle();
        }));
        assert!(ok, "dispatch should succeed while the pool is alive");
    }

    /// (a) A worker blocked non-cooperatively for longer than `stall_ms`
    /// produces a stall event naming the blamed pid.
    #[test]
    fn watchdog_reports_a_genuine_stall() {
        let pool = LocalWorkerPool::with_watchdog(1, Some(100));
        let pid = ActorPid {
            node: 0,
            local_id: 42,
        };
        dispatch_blocking_job(&pool, pid, Duration::from_millis(500));

        // Poll for the stall event instead of a fixed sleep: the watchdog
        // ticks every stall_ms/2 = 50ms, so it should show up well inside
        // the 500ms block.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events = my_events(&pool);
            if events.iter().any(|e| !e.recovered) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(pool);

        let stall = events
            .iter()
            .find(|e| !e.recovered)
            .expect("expected a stall event while the worker was blocked");
        assert_eq!(stall.pid, Some(pid));
    }

    /// (b) Normal cooperative operation (jobs that yield/await, never
    /// blocking the OS thread) never trips the watchdog.
    #[test]
    fn watchdog_stays_quiet_during_cooperative_work() {
        let pool = LocalWorkerPool::with_watchdog(1, Some(200));
        let pid = ActorPid {
            node: 0,
            local_id: 7,
        };
        for _ in 0..20 {
            let ok = pool.dispatch(Box::new(move || {
                tokio::task::spawn_local(async move {
                    heartbeat_running(pid);
                    tokio::task::yield_now().await;
                    heartbeat_idle();
                });
            }));
            assert!(ok);
        }
        // Give the watchdog several poll cycles (poll_every = 100ms) to have
        // had a chance to (wrongly) fire.
        std::thread::sleep(Duration::from_millis(600));
        let events = my_events(&pool);
        drop(pool);

        assert!(
            events.iter().all(|e| e.recovered),
            "cooperative work must never produce a stall warning: {events:?}"
        );
    }

    /// (c) After a stall clears (the blocking op returns), the watchdog
    /// reports recovery.
    #[test]
    fn watchdog_reports_recovery_after_stall_clears() {
        let pool = LocalWorkerPool::with_watchdog(1, Some(100));
        let pid = ActorPid {
            node: 0,
            local_id: 99,
        };
        dispatch_blocking_job(&pool, pid, Duration::from_millis(300));

        // Wait long enough to see both the stall and its recovery: the
        // block lasts 300ms, threshold is 100ms, poll every 50ms. Events
        // arrive in emission order within the sink, so a recovery at a
        // later index than a stall really did follow it.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saw_stall = false;
        let mut saw_recovery = false;
        while Instant::now() < deadline && !(saw_stall && saw_recovery) {
            saw_stall = false;
            saw_recovery = false;
            for e in my_events(&pool) {
                if e.recovered {
                    saw_recovery = saw_recovery || saw_stall; // recovery must follow a stall
                } else {
                    saw_stall = true;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(pool);

        assert!(saw_stall, "expected a stall event");
        assert!(
            saw_recovery,
            "expected a recovery event after the stall cleared"
        );
    }
}
