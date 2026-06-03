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
}

/// A fixed pool of single-threaded tokio workers, each hosting a
/// `LocalSet` that can run `!Send` actor futures. Dispatch is
/// round-robin; an actor stays on its assigned worker for life.
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
                .spawn(move || worker_main(job_rx))
                .expect("spawn cs-actor local worker thread");
            workers.push(Worker {
                job_tx: Some(job_tx),
                handle: Some(handle),
            });
        }
        Self {
            workers,
            cursor: AtomicUsize::new(0),
        }
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
}
