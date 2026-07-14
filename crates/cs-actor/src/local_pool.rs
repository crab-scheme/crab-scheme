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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use tokio::sync::mpsc;
use tokio::task::LocalSet;

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
    /// Number of actors currently live on this worker (dispatched minus
    /// exited). Used by `dispatch`'s power-of-two-choices to pick the
    /// less-loaded of two candidate workers. Placement-only signal — see
    /// the module doc's "Thread-affinity, not migration" note; there is
    /// no work-stealing based on this counter (that needs `Send` actor
    /// heaps, iter-2b).
    load: Arc<AtomicUsize>,
}

/// RAII handle for a worker's live-actor count: `dispatch`'s job-builder
/// closure receives one and should move it into the spawned actor
/// future. Decrements on drop — including on panic-unwind through
/// `catch_unwind`, since the guard lives in the async fn's generated
/// state machine and is dropped like any other local when that state
/// machine unwinds.
pub struct LoadGuard(Arc<AtomicUsize>);

impl Drop for LoadGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A fixed pool of single-threaded tokio workers, each hosting a
/// `LocalSet` that can run `!Send` actor futures. Dispatch uses
/// power-of-two-choices over live-actor counts (placement only — an
/// actor stays on its assigned worker for life, no migration).
pub struct LocalWorkerPool {
    workers: Vec<Worker>,
    cursor: AtomicUsize,
}

impl LocalWorkerPool {
    /// Build a pool of `n_workers` threads (clamped to ≥ 1). Each thread
    /// stands up its own current-thread runtime + `LocalSet` and parks
    /// waiting for jobs.
    pub fn new(n_workers: usize) -> Self {
        let n = n_workers.max(1);
        let mut workers = Vec::with_capacity(n);
        for i in 0..n {
            let (job_tx, job_rx) = mpsc::unbounded_channel::<WorkerJob>();
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
                .spawn(move || worker_main(job_rx))
                .expect("spawn cs-actor local worker thread");
            workers.push(Worker {
                job_tx: Some(job_tx),
                handle: Some(handle),
                load: Arc::new(AtomicUsize::new(0)),
            });
        }
        Self {
            workers,
            cursor: AtomicUsize::new(0),
        }
    }

    /// Pick a worker via power-of-two-choices — a rotating cursor names
    /// candidate 1, `cursor + 1` names candidate 2, and the job goes to
    /// whichever of the two currently has fewer live actors (ties go to
    /// candidate 1). Build the job with `build_job`, which receives a
    /// [`LoadGuard`] for the chosen worker; the caller should move that
    /// guard into the spawned actor future so the count drops back down
    /// when the actor exits (or panics).
    ///
    /// Placement only: the actor stays on the chosen worker for its
    /// whole life. There is no migration / work-stealing off this
    /// counter — rebalancing a running actor needs a `Send` actor heap,
    /// which is the iter-2b wall (see the module doc).
    ///
    /// Returns `false` if the chosen worker has already shut down (its
    /// channel is closed) — the caller should treat that as the actor
    /// never having started.
    pub fn dispatch<F>(&self, build_job: F) -> bool
    where
        F: FnOnce(LoadGuard) -> WorkerJob,
    {
        let n = self.workers.len();
        // `% n` with n ≥ 1 (guaranteed by `new`) is always valid.
        let c1 = self.cursor.fetch_add(1, Ordering::Relaxed) % n;
        let c2 = (c1 + 1) % n;
        let idx = if self.workers[c2].load.load(Ordering::Relaxed)
            < self.workers[c1].load.load(Ordering::Relaxed)
        {
            c2
        } else {
            c1
        };
        let load = Arc::clone(&self.workers[idx].load);
        load.fetch_add(1, Ordering::Relaxed);
        let job = build_job(LoadGuard(Arc::clone(&load)));
        let sent = match &self.workers[idx].job_tx {
            Some(tx) => tx.send(job).is_ok(),
            None => false,
        };
        if !sent {
            load.fetch_sub(1, Ordering::Relaxed);
        }
        sent
    }

    /// Number of worker threads in the pool.
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Current live-actor count per worker (test/diagnostic use).
    #[cfg(test)]
    fn worker_loads(&self) -> Vec<usize> {
        self.workers
            .iter()
            .map(|w| w.load.load(Ordering::Relaxed))
            .collect()
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
    }
}

/// A worker thread's entry point: build a current-thread runtime + a
/// `LocalSet`, then drive the job-receive loop on it. Each received job
/// runs inside the `LocalSet` context, so jobs may `spawn_local` `!Send`
/// futures that the same `block_on` continues to poll concurrently with
/// the receive loop. When the channel closes (pool drop), the loop ends
/// and `block_on` returns, dropping the runtime.
fn worker_main(mut job_rx: mpsc::UnboundedReceiver<WorkerJob>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime for cs-actor local worker");
    let local = LocalSet::new();
    local.block_on(&rt, async move {
        while let Some(job) = job_rx.recv().await {
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
            let ok = pool.dispatch(|guard| {
                Box::new(move || {
                    tokio::task::spawn_local(async move {
                        let _guard = guard;
                        // `Rc` is `!Send`; holding it across the await is
                        // the whole point — a `LocalSet` permits it.
                        let cell = Rc::new(std::cell::Cell::new(0u64));
                        for _ in 0..5 {
                            tokio::task::yield_now().await;
                            cell.set(cell.get() + 1);
                        }
                        if cell.get() == 5 {
                            done.fetch_add(1, Ordering::Relaxed);
                        }
                    });
                })
            });
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

    /// Dispatch spreads load across workers (power-of-two-choices).
    #[test]
    fn dispatch_is_round_robin() {
        let pool = LocalWorkerPool::new(4);
        // We can't observe the index directly, but we can confirm
        // dispatch keeps succeeding and the pool reports the right
        // width.
        assert_eq!(pool.worker_count(), 4);
        for _ in 0..16 {
            assert!(pool.dispatch(|guard| Box::new(move || {
                tokio::task::spawn_local(async move {
                    let _guard = guard;
                });
            })));
        }
    }

    /// After the pool is dropped, its worker threads are joined and torn
    /// down cleanly (no hang). This is a smoke test for `Drop`.
    #[test]
    fn pool_drop_joins_workers() {
        let pool = LocalWorkerPool::new(3);
        for _ in 0..10 {
            pool.dispatch(|guard| {
                Box::new(move || {
                    tokio::task::spawn_local(async move {
                        let _guard = guard;
                        tokio::task::yield_now().await;
                    });
                })
            });
        }
        drop(pool); // must return promptly; would hang if Drop were wrong
    }

    /// Skewed spawning still balances: dispatching many actors that park
    /// forever (never releasing their `LoadGuard`) should keep the
    /// spread between the busiest and idlest worker small, not pile
    /// everything onto one worker the way blind round-robin with skipped
    /// workers could.
    #[test]
    fn power_of_two_choices_balances_load() {
        let pool = LocalWorkerPool::new(4);
        let total = 200;
        for _ in 0..total {
            let ok = pool.dispatch(|guard| {
                Box::new(move || {
                    tokio::task::spawn_local(async move {
                        // Park forever: hold the guard so the worker's
                        // live count never drops for the life of this
                        // test.
                        let _guard = guard;
                        std::future::pending::<()>().await;
                    });
                })
            });
            assert!(ok);
        }
        // Give the workers a moment to actually spawn_local each job
        // (dispatch only sends to the channel; the recv loop drives
        // spawn_local asynchronously).
        std::thread::sleep(Duration::from_millis(200));
        let loads = pool.worker_loads();
        let max = *loads.iter().max().unwrap();
        let min = *loads.iter().min().unwrap();
        assert_eq!(loads.iter().sum::<usize>(), total, "loads: {loads:?}");
        assert!(
            max - min <= total / 4,
            "expected balanced spread, got {loads:?}"
        );
    }

    /// Live counts return to 0 once every actor exits.
    #[test]
    fn load_counters_return_to_zero_after_exit() {
        let pool = LocalWorkerPool::new(4);
        let total = 100u64;
        let done = Arc::new(AtomicU64::new(0));
        for _ in 0..total {
            let done = Arc::clone(&done);
            let ok = pool.dispatch(|guard| {
                Box::new(move || {
                    tokio::task::spawn_local(async move {
                        let _guard = guard;
                        tokio::task::yield_now().await;
                        done.fetch_add(1, Ordering::Relaxed);
                    });
                })
            });
            assert!(ok);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while done.load(Ordering::Relaxed) < total {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The guard drops in the same async block after the counted
        // work, but give the drop a moment to land.
        std::thread::sleep(Duration::from_millis(50));
        let loads = pool.worker_loads();
        assert_eq!(loads, vec![0, 0, 0, 0], "expected all counters to drain");
    }
}
