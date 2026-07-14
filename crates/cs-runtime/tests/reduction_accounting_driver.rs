//! cs-845.7 — discriminating driver-level test for per-actor reduction
//! accounting, exercising the REAL green-actor driver (`pump_coroutine`).
//!
//! Unlike `reduction_accounting.rs`'s white-box protocol replay, this test
//! fails under the old shared-countdown code and passes under the
//! per-actor save/restore. Scheme of the experiment:
//!
//! 1. One `LocalWorkerPool` worker; a huge reduction budget (1M) is set on
//!    that worker thread so instruction-count imprecision can't matter.
//! 2. A green actor B runs through `pump_coroutine`, does a little work,
//!    and parks on `(raw-receive)` — the driver saves B's remaining
//!    countdown (~1M) into B's own `Actor::reduction_slice`.
//! 3. While B is parked, a raw job on the worker thread POISONS the shared
//!    thread_local countdown to 5 (simulating a co-tenant that burned its
//!    slice down to almost nothing) and resets the worker's yield counter.
//! 4. B is woken and runs a loop far bigger than 5 ticks but far smaller
//!    than its ~1M saved slice.
//! 5. A final job reads the worker's `yield_count`.
//!
//! - NEW (per-actor) code: `pump_coroutine` restores B's OWN saved
//!   countdown over the poison on resume → B never exhausts → 0 yields.
//! - OLD (shared) code: B resumes with the poisoned 5 remaining, exhausts
//!   within a handful of instructions → the yield hook fires → ≥ 1 yield.
//!
//! This is in its own test file (own process) deliberately: the worker's
//! `VM_YIELD_COUNT` is a per-thread cell shared by every co-tenant, so a
//! CPU-bound actor from a sibling test in the same binary would pollute
//! the count between our reset and read.
//!
//! Ordering is deterministic because everything on the worker is
//! single-threaded and cooperative: `run_on_local_worker` jobs interleave
//! with — never preempt — actor task polls, and the test blocks on each
//! job's completion channel before taking the next step.

#![cfg(feature = "actor")]

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_send, primop_spawn, primop_spawn_source_green,
    SendableValue,
};

mod common;
use common::wait_until;

fn sym(s: &str) -> SendableValue {
    SendableValue::Symbol(s.into())
}

fn tagged(tag: &str, payload: SendableValue) -> SendableValue {
    SendableValue::Pair(Box::new(sym(tag)), Box::new(payload))
}

/// Run `job` on THE single green worker thread and block until it has
/// executed there.
fn on_worker<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let ok = beam_state().actors.run_on_local_worker(move || {
        let _ = tx.send(job());
    });
    assert!(ok, "local worker pool has shut down");
    rx.recv_timeout(Duration::from_secs(10))
        .expect("worker job never ran")
}

#[test]
fn resumed_actor_restores_its_own_slice_not_a_cotenants_leftover() {
    // NOTE: `set_var` is process-global and unsynchronized; safe here only
    // because this file contains a single test and the pool reads the var
    // once, on first green spawn. Do not add tests to this file that race
    // with it.
    std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    let out_for_actor = out.clone();
    // Collector runs on a DEDICATED thread (primop_spawn ->
    // spawn_sync_body_on_task), never on the green worker — it must not
    // touch the worker's thread-locals.
    beam_state().procs.register(
        "test:cs845-driver-collector",
        Arc::new(move |actor, _args| {
            for _ in 0..2 {
                match primop_raw_receive(actor, None) {
                    Ok(Some(SendableValue::Symbol(s))) => {
                        out_for_actor.lock().unwrap().push(s.to_string())
                    }
                    Ok(Some(other)) => out_for_actor.lock().unwrap().push(format!("{other:?}")),
                    _ => break,
                }
            }
        }),
    );
    let col = primop_spawn("test:cs845-driver-collector", vec![]).expect("spawn collector");

    // Spawn B first: the pool is built lazily on first green spawn, and
    // `run_on_local_worker` needs it to exist. B parks immediately on its
    // first (raw-receive) — before any budget knob matters.
    let b = primop_spawn_source_green(
        r#"
        (define (b)
          (let ((col (cdr (raw-receive))))
            (send col 'parked)
            (raw-receive)
            (let loop ((i 0)) (if (< i 5000) (loop (+ i 1)) 'x))
            (send col 'done)))
        "#
        .to_string(),
        "b".to_string(),
        vec![],
    )
    .expect("spawn green actor B");

    // Step 1: huge budget on the worker, so B's post-park saved slice is
    // ~1M no matter how many instructions loading + the marker send cost.
    let prev_budget = on_worker(|| {
        let prev = cs_vm::vm::reduction_budget();
        cs_vm::vm::set_reduction_budget(1_000_000);
        prev
    });

    // Step 2: hand B its collector; B sends 'parked then parks again.
    // When the driver saves B's slice at that park it is ~1M-minus-a-bit.
    primop_send(b, tagged("col", SendableValue::Pid(col))).unwrap();
    wait_until(Duration::from_secs(10), "B never parked", || {
        out.lock().unwrap().iter().any(|s| s == "parked")
    });

    // Step 3: B is parked (its poll ended — jobs can't preempt a poll, so
    // reaching this job proves the park). Poison the shared countdown to 5,
    // as a nearly-exhausted co-tenant would leave it, and zero the yield
    // counter.
    on_worker(|| {
        cs_vm::vm::set_ticks_remaining(5);
        cs_vm::vm::reset_yield_count();
    });

    // Step 4: wake B. It loops 5000 iterations — thousands of VM ticks,
    // far more than the poisoned 5, far less than its ~1M saved slice.
    primop_send(b, sym("go")).unwrap();
    wait_until(Duration::from_secs(10), "B never finished", || {
        out.lock().unwrap().iter().any(|s| s == "done")
    });

    // Step 5: read the worker's yield count and restore the budget.
    // (Budget restored inside the same job so no later green work runs on
    // a 1M budget.)
    let yields = on_worker(move || {
        let y = cs_vm::vm::yield_count();
        cs_vm::vm::set_reduction_budget(prev_budget);
        y
    });

    assert_eq!(
        yields, 0,
        "actor B must resume with its OWN saved reduction slice (~1M \
         remaining); {yields} yield(s) means it inherited the poisoned \
         co-tenant leftover (5 remaining) from the shared thread_local — \
         the pre-cs-845.7 shared-countdown bug"
    );
}
