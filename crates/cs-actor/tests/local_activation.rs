//! Integration tests for `ActorSystem::spawn_local_activation`
//! (#30 iter-2a / ADR 0032).
//!
//! The headline guarantee: **more mailbox-bound actors than the
//! `max_blocking_threads(4096)` ceiling can be live concurrently**,
//! because local-activation actors park (release their worker) on an
//! empty-mailbox `await` instead of pinning an OS thread via
//! `block_in_place`. Each actor here holds an `Rc` heap across the
//! mailbox `await`, so its body future is `!Send` — which only compiles
//! because the `LocalSet` worker pool hosts it.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cs_actor::{ActorSystem, Message, Payload};

fn cmd(s: &'static str) -> Payload {
    Arc::new(s)
}

fn as_cmd(msg: &Message) -> Option<&'static str> {
    match msg {
        Message::User(p) => p.downcast_ref::<&'static str>().copied(),
        _ => None,
    }
}

/// Spawn far more mailbox-bound actors than the 4096 blocking-thread
/// ceiling, hold them all parked on `receive_async`, then drive each to
/// completion. If the ceiling still applied, actors beyond 4096 could not
/// be live at once and their messages would never be processed (the test
/// would time out in `wait_idle`).
#[test]
fn exceeds_blocking_thread_ceiling() {
    let system = ActorSystem::new();
    // > max_blocking_threads(4096), with margin. Each actor is a cheap
    // parked tokio task, not an OS thread, so this is light.
    let n = 5000usize;
    let pings_seen = Arc::new(AtomicU64::new(0));

    let mut refs = Vec::with_capacity(n);
    for _ in 0..n {
        let pings_seen = Arc::clone(&pings_seen);
        let actor_ref = system.spawn_local_activation(move |mut actor| async move {
            // `Rc<Cell<_>>` is `!Send`. Holding it across every
            // `receive_async().await` below is the whole point: a
            // `LocalSet`-hosted future may keep a thread-pinned heap
            // across awaits; a `spawn_async` future could not.
            let count: Rc<Cell<u64>> = Rc::new(Cell::new(0));
            while let Some(msg) = actor.receive_async().await {
                match as_cmd(&msg) {
                    Some("ping") => count.set(count.get() + 1),
                    Some("stop") => break,
                    _ => {}
                }
            }
            pings_seen.fetch_add(count.get(), Ordering::Relaxed);
        });
        refs.push(actor_ref);
    }

    // All `n` actors are now spawned and (about to be) parked on an empty
    // mailbox. Send each one ping + a stop. Unbounded Fast mailboxes never
    // reject, so ordering vs. the actor reaching its await doesn't matter.
    for r in &refs {
        r.send(cmd("ping")).expect("send ping");
        r.send(cmd("stop")).expect("send stop");
    }

    // Block until every actor has processed its messages and terminated.
    system.wait_idle();
    assert_eq!(
        pings_seen.load(Ordering::Relaxed),
        n as u64,
        "every one of the {n} actors should have processed exactly one ping"
    );
    assert_eq!(system.live_actor_count(), 0, "all actors should be drained");
}

/// A local-activation actor keeps persistent `!Send` state across many
/// activations (parks between each), proving the heap survives the await
/// — the capability `spawn_async` lacks.
#[test]
fn persistent_non_send_state_across_activations() {
    let system = ActorSystem::new();
    let final_sum = Arc::new(AtomicU64::new(0));
    let final_sum_c = Arc::clone(&final_sum);

    let actor_ref = system.spawn_local_activation(move |mut actor| async move {
        // Persistent accumulator held across every parking await.
        let acc: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        while let Some(msg) = actor.receive_async().await {
            match as_cmd(&msg) {
                Some("inc") => acc.set(acc.get() + 1),
                Some("done") => break,
                _ => {}
            }
        }
        final_sum_c.store(acc.get(), Ordering::Relaxed);
    });

    for _ in 0..1000 {
        actor_ref.send(cmd("inc")).expect("send inc");
    }
    actor_ref.send(cmd("done")).expect("send done");
    system.wait_idle();
    assert_eq!(final_sum.load(Ordering::Relaxed), 1000);
}

/// Ping/pong between two local-activation actors completes promptly,
/// exercising the parking receive on both sides under the pool.
#[test]
fn ping_pong_round_trips() {
    let system = ActorSystem::new();
    let rounds = 200u64;
    let done = Arc::new(AtomicU64::new(0));
    let done_c = Arc::clone(&done);

    // Pong counts the pings it receives (via a shared atomic) and keeps a
    // thread-local `!Send` tally across each parking await.
    let pong = system.spawn_local_activation(move |mut actor| async move {
        let local: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        while let Some(msg) = actor.receive_async().await {
            match as_cmd(&msg) {
                Some("ping") => {
                    local.set(local.get() + 1);
                    done_c.fetch_add(1, Ordering::Relaxed);
                }
                Some("stop") => break,
                _ => {}
            }
        }
    });

    let pinger = system.spawn_local_activation(move |mut actor| async move {
        // Wait for a kickoff, then fire `rounds` pings at pong.
        while let Some(msg) = actor.receive_async().await {
            if as_cmd(&msg) == Some("go") {
                for _ in 0..rounds {
                    let _ = pong.send(cmd("ping"));
                }
                let _ = pong.send(cmd("stop"));
                break;
            }
        }
    });

    pinger.send(cmd("go")).expect("kick off");
    // Wait for the pong actor to see all rounds (and both actors to exit).
    let deadline = Instant::now() + Duration::from_secs(10);
    while done.load(Ordering::Relaxed) < rounds {
        assert!(Instant::now() < deadline, "ping/pong timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    system.wait_idle();
    assert_eq!(done.load(Ordering::Relaxed), rounds);
}
