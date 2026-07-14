//! cs-845.4: the worker-stall watchdog observed through the REAL green
//! driver (`pump_coroutine` in `builtins/beam.rs`), not the cs-actor unit
//! harness — these tests exercise the actual `heartbeat_running` /
//! `heartbeat_idle` call sites around `co.resume`.
//!
//! Two behaviors:
//! 1. A green actor **cooperatively parked** on `(raw-receive)` must NOT be
//!    blamed — `heartbeat_idle()` after each resume means a parked worker has
//!    no "currently running" pid for the watchdog to blame.
//! 2. A green actor stuck in a **genuinely blocking, un-hooked op** (here:
//!    opening a FIFO for reading with no writer — `open-input-file` blocks
//!    the whole worker thread) MUST be blamed by pid.
//!
//! The watchdog is enabled via `CRABSCHEME_WORKER_WATCHDOG_MS`, read once
//! when the process-global beam `ActorSystem` lazily builds its
//! `LocalWorkerPool`. Both tests set the same value before any green spawn
//! and are serialized by a shared mutex, so the env var is written before
//! any concurrent reader exists and never changes afterwards.

#![cfg(all(feature = "actor", unix))]

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use cs_actor::local_pool::stall_events;
use cs_actor::ActorPid;
use cs_runtime::builtins::beam::{primop_send, primop_spawn_source_green, SendableValue};

/// Serialize the two tests (they share the process-global beam actor system
/// and its single worker pool) and set the watchdog env var exactly once,
/// before the first green spawn can build the pool.
fn watchdog_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    static ENV: OnceLock<()> = OnceLock::new();
    let guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    ENV.get_or_init(|| {
        std::env::set_var("CRABSCHEME_WORKER_WATCHDOG_MS", "100");
        // One worker: the blocking test must freeze the SAME worker the
        // watchdog is scrutinizing, deterministically.
        std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");
    });
    guard
}

fn stall_blaming(pid: ActorPid) -> bool {
    stall_events::snapshot()
        .iter()
        .any(|e| !e.recovered && e.pid == Some(pid))
}

/// (1) A `(raw-receive)`-parked green actor is never blamed: parked =
/// `heartbeat_idle()` already ran, so the watchdog sees no running pid.
#[test]
fn parked_green_actor_is_not_blamed() {
    let _guard = watchdog_lock();

    let source = r#"
        (define (main)
          (raw-receive)
          'done)
    "#;
    let pid = primop_spawn_source_green(source.to_string(), "main".to_string(), vec![])
        .expect("spawn green actor");

    // 500ms parked = 5x the 100ms threshold, ~10 watchdog polls. If the
    // park were mis-accounted as "running", the watchdog would fire here.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !stall_blaming(pid),
        "a cooperatively parked green actor must not be blamed for a stall"
    );

    // Wake it so it exits cleanly.
    let _ = primop_send(pid, SendableValue::Symbol("stop".into()));
}

/// (2) A green actor frozen in a genuinely blocking op IS blamed. The op:
/// `(open-input-file)` on a FIFO with no writer — a plain blocking `open(2)`
/// with no cooperative hook, freezing the worker exactly like any un-hooked
/// blocking builtin would.
#[test]
fn blocking_green_actor_is_blamed_then_released() {
    let _guard = watchdog_lock();

    let dir = std::env::temp_dir().join(format!("cs-green-watchdog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let fifo = dir.join("stall.fifo");
    let _ = std::fs::remove_file(&fifo);
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed");

    let source = format!(
        r#"
        (define (main)
          (open-input-file "{}")
          'done)
    "#,
        fifo.display()
    );
    let pid =
        primop_spawn_source_green(source, "main".to_string(), vec![]).expect("spawn green actor");

    // The open blocks the worker; at threshold 100ms / poll 50ms a stall
    // event blaming our pid must appear well within the deadline.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stall_blaming(pid) {
        assert!(
            Instant::now() < deadline,
            "watchdog never blamed the blocking green actor {pid}; events: {:?}",
            stall_events::snapshot()
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Release the actor (opening the write side unblocks the reader's open)
    // so the shared worker is free again for any later test in this binary.
    let fifo_w = fifo.clone();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(fifo_w)
            .expect("open fifo for write");
        let _ = f.write_all(b"x");
    });
    writer.join().expect("writer thread");
    let _ = std::fs::remove_file(&fifo);
    let _ = std::fs::remove_dir(&dir);
}
