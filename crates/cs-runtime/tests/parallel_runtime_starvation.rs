//! parallel-runtime spec C2.3 — starvation prevention test.
//!
//! Verifies that a CPU-bound actor running through the bytecode
//! interpreter (here simulated via direct `vm_tick_reductions`
//! calls) cannot starve another actor on the same `ActorSystem`.
//! Requires C2.1 (yield hook in dispatch loop) + C2.2 (cs-actor
//! tokio_yield_hook bridge) + C1.3 (worker_threads ≥ 2).
//!
//! How it works:
//!
//! - Spawn one actor (`hog`) that loops calling
//!   `cs_vm::vm::vm_tick_reductions()` 100k times — simulates a
//!   tight bytecode loop with no `(receive)` to yield naturally.
//! - Spawn another actor (`responder`) that blocks on `receive`
//!   and atomically marks "got it" when a message arrives.
//! - Send the message to the responder.
//! - Assert the responder marks "got it" within a 2-second
//!   budget. Without C2's yield mechanism, the hog would hold
//!   its worker thread for the full loop duration and the
//!   responder's wakeup would wait until the hog finishes.

#![cfg(feature = "actor")]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cs_actor::{ActorSystem, Message};

#[test]
fn cpu_bound_actor_does_not_starve_responder() {
    // Install the yield hook + small reduction budget so the
    // hog yields aggressively. The cs-actor side already
    // installs the hook per-actor in cs-runtime's primop_spawn,
    // but THIS test goes through cs-actor directly (no Scheme),
    // so we install the hook manually for both actor threads.
    //
    // Reduction budget shrinks from 2000 default → 50 so the
    // hog yields every 50 ticks (instead of waiting ~50ms of
    // bytecode for the default budget).
    let prev_budget = cs_vm::vm::reduction_budget();
    cs_vm::vm::reset_yield_count();

    let sys = ActorSystem::new();
    let responder_acked = Arc::new(AtomicBool::new(false));

    // Responder: waits for a message, sets the flag.
    let responder_acked_for_actor = responder_acked.clone();
    let responder = sys.spawn_sync_body_on_task(move |actor| {
        // Set budget + hook on this worker thread.
        let prev = cs_vm::vm::install_yield_hook(Some(cs_actor::tokio_yield_hook));
        cs_vm::vm::set_reduction_budget(50);
        if let Some(Message::User(_)) = actor.receive() {
            responder_acked_for_actor.store(true, Ordering::SeqCst);
        }
        cs_vm::vm::install_yield_hook(prev);
    });

    // Hog: tight loop hammering vm_tick_reductions. Without the
    // yield hook this would hold its worker for the full loop
    // duration (~hundreds of ms even in release). With the hook,
    // it yields every 50 ticks → ~2000 yields total → tokio
    // gets ample chance to schedule the responder.
    //
    // `yield_count` is per-thread, so the test's main thread
    // can't read the hog's counter directly. The hog snapshots
    // its own thread-local count into a shared atomic before
    // returning so the main thread can assert the hook fired.
    let hog_yields = Arc::new(AtomicU64::new(0));
    let hog_yields_for_actor = hog_yields.clone();
    sys.spawn_sync_body_on_task(move |_actor| {
        let prev = cs_vm::vm::install_yield_hook(Some(cs_actor::tokio_yield_hook));
        cs_vm::vm::set_reduction_budget(50);
        cs_vm::vm::reset_yield_count();
        for _ in 0..100_000 {
            cs_vm::vm::vm_tick_reductions();
        }
        // Snapshot this thread's yield count for the test to read.
        hog_yields_for_actor.store(cs_vm::vm::yield_count(), Ordering::SeqCst);
        cs_vm::vm::install_yield_hook(prev);
    });

    // Give both actors a moment to start, then send the message.
    // Tiny delay because spawn_sync_body_on_task doesn't sync-
    // ronously confirm the actor is ready to receive.
    std::thread::sleep(Duration::from_millis(20));
    responder
        .send(Arc::new(()) as _)
        .expect("send to responder");

    // Wait for the responder to ack, up to 2 seconds. If the hog
    // were starving the runtime, we wouldn't see the ack within
    // this window on a single-worker setup; with C1.3 + C2 we
    // expect it within ~10-100ms.
    let start = Instant::now();
    let timeout = Duration::from_secs(2);
    while !responder_acked.load(Ordering::SeqCst) {
        if start.elapsed() > timeout {
            let yields = cs_vm::vm::yield_count();
            panic!(
                "responder starved by CPU-bound actor: {:?} elapsed, \
                 yield_count = {} (preemption mechanism may not be firing)",
                start.elapsed(),
                yields
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let elapsed = start.elapsed();
    // Wait for the hog to finish so its yield-count snapshot is final.
    sys.wait_idle();
    let hog_yields_observed = hog_yields.load(Ordering::SeqCst);
    println!(
        "responder acked in {:?}; hog produced {} yields",
        elapsed, hog_yields_observed
    );
    // The hog ran 100_000 ticks with budget=50, so the hook
    // should have fired ~2000 times. Tolerate a wide range: any
    // value > 0 means the preemption seam fired (anything more
    // is performance characteristic, not correctness).
    assert!(
        hog_yields_observed > 0,
        "yield hook never fired on hog's thread — preemption isn't actually being exercised"
    );

    sys.shutdown();
    cs_vm::vm::set_reduction_budget(prev_budget);
}
