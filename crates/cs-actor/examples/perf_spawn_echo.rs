//! parallel-runtime M1 perf bench — raw cs-actor throughput.
//!
//! Measures spawn-N and echo-N at the cs-actor level, bypassing
//! the Scheme `(spawn name)` primop (which would require a
//! Rust-registered proc per the BeamState design — spec called
//! out as a future iter).
//!
//! Run with:
//!   cargo run --release --example perf_spawn_echo
//!   cargo run --release --example perf_spawn_echo -- spawn 1000000
//!   cargo run --release --example perf_spawn_echo -- echo  100000
//!
//! Default scales: spawn 1M, echo 100k.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::sync::atomic::AtomicBool;

use cs_actor::{ActorSystem, Message, Payload};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (bench, n) = if args.len() >= 3 {
        (args[1].as_str(), args[2].parse::<usize>().unwrap_or(0))
    } else {
        ("all", 0)
    };

    match bench {
        "spawn" => bench_spawn(n.max(1)),
        "echo" => bench_echo(n.max(1)),
        "starvation" => bench_starvation(n.max(1)),
        "all" => {
            bench_spawn(1_000_000);
            bench_echo(100_000);
            bench_starvation(10_000_000);
        }
        other => {
            eprintln!("unknown bench: {other}");
            std::process::exit(1);
        }
    }
}

/// Spawn N no-op actors as fast as possible. Each actor returns
/// immediately. Measures both the spawn rate and the time to
/// drain the runtime afterward (so the N-actor steady state is
/// observable).
fn bench_spawn(n: usize) {
    let sys = ActorSystem::new();
    println!("== spawn N={n} ==");

    let t0 = Instant::now();
    for _ in 0..n {
        sys.spawn_sync_body_on_task(|_actor| {
            // No-op body. The actor's mpsc receive returns None
            // when the system drops the sender; we just return
            // immediately, which is the fastest path.
        });
    }
    let spawn_elapsed = t0.elapsed();
    let spawn_rate = n as f64 / spawn_elapsed.as_secs_f64();
    println!(
        "  spawn:  {:>10} actors in {:>10.3}s → {:>12.0} actors/s",
        n,
        spawn_elapsed.as_secs_f64(),
        spawn_rate
    );

    let t1 = Instant::now();
    sys.wait_idle();
    let drain_elapsed = t1.elapsed();
    println!(
        "  drain:  wait_idle took {:>10.3}s",
        drain_elapsed.as_secs_f64()
    );

    let total = t0.elapsed();
    let throughput = n as f64 / total.as_secs_f64();
    println!(
        "  total:  {:>10.3}s → {:>12.0} actors/s (spawn+drain)",
        total.as_secs_f64(),
        throughput
    );

    sys.shutdown();
}

/// Starvation test: one CPU-bound actor in a tight loop +
/// one responder waiting on a message. Measures how long it
/// takes the responder to ack despite the hog. C2.1+C2.2's
/// cooperative-yield seam should keep this well under 100ms
/// even at huge loop counts.
fn bench_starvation(loop_count: usize) {
    use std::sync::atomic::Ordering;

    let sys = ActorSystem::new();
    println!("== starvation hog-loop={loop_count} ==");

    // Install the yield hook on this thread.
    let prev_budget = cs_vm::vm::reduction_budget();
    cs_vm::vm::set_reduction_budget(50);

    let acked = Arc::new(AtomicBool::new(false));
    let acked_for_actor = acked.clone();
    let responder = sys.spawn_sync_body_on_task(move |actor| {
        let prev = cs_vm::vm::install_yield_hook(Some(cs_actor::tokio_yield_hook));
        cs_vm::vm::set_reduction_budget(50);
        if let Some(Message::User(_)) = actor.receive() {
            acked_for_actor.store(true, Ordering::SeqCst);
        }
        cs_vm::vm::install_yield_hook(prev);
    });

    // Hog: hammer vm_tick_reductions. Without C2's yield this
    // would block its worker for the entire loop.
    sys.spawn_sync_body_on_task(move |_actor| {
        let prev = cs_vm::vm::install_yield_hook(Some(cs_actor::tokio_yield_hook));
        cs_vm::vm::set_reduction_budget(50);
        for _ in 0..loop_count {
            cs_vm::vm::vm_tick_reductions();
        }
        cs_vm::vm::install_yield_hook(prev);
    });

    // Small delay so both actors register.
    std::thread::sleep(Duration::from_millis(20));

    let t0 = Instant::now();
    responder
        .send(Arc::new(()) as Payload)
        .expect("send to responder");
    let deadline = t0 + Duration::from_secs(5);
    while !acked.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            panic!("responder starved by hog for >5s with hog-loop={loop_count}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let latency = t0.elapsed();
    println!(
        "  responder acked in {:>10.3}ms (hog still doing its {loop_count} ticks)",
        latency.as_secs_f64() * 1000.0
    );

    sys.wait_idle();
    let total = t0.elapsed();
    println!(
        "  total (incl. hog completion): {:>10.3}s",
        total.as_secs_f64()
    );

    sys.shutdown();
    cs_vm::vm::set_reduction_budget(prev_budget);
}

/// Two actors play ping-pong N times. Measures end-to-end
/// message throughput (2N messages crossing actor boundaries).
fn bench_echo(n: usize) {
    let sys = ActorSystem::new();
    println!("== echo N={n} (=> {} msgs round-trip) ==", n * 2);

    let received = Arc::new(AtomicU64::new(0));
    let received_clone = received.clone();

    // The echo actor receives N messages and increments the
    // shared counter for each. Returns when count reaches N.
    let echo = sys.spawn_sync_body_on_task(move |actor| {
        for _ in 0..n {
            match actor.receive() {
                Some(Message::User(_)) => {
                    received_clone.fetch_add(1, Ordering::Relaxed);
                }
                _ => break,
            }
        }
    });

    let t0 = Instant::now();
    for i in 0..n {
        let payload: Payload = Arc::new(i as u64);
        echo.send(payload).expect("send");
    }
    let send_elapsed = t0.elapsed();

    // Wait for the echo actor to drain.
    let deadline = t0 + Duration::from_secs(60);
    while received.load(Ordering::Relaxed) < n as u64 {
        if Instant::now() >= deadline {
            panic!(
                "echo actor didn't drain within 60s: received {} / {}",
                received.load(Ordering::Relaxed),
                n
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let total_elapsed = t0.elapsed();

    let send_rate = n as f64 / send_elapsed.as_secs_f64();
    let total_msgs = 2 * n;
    let total_rate = total_msgs as f64 / total_elapsed.as_secs_f64();

    println!(
        "  send:   {:>10} msgs in {:>10.3}s → {:>12.0} msgs/s (sender side only)",
        n,
        send_elapsed.as_secs_f64(),
        send_rate
    );
    println!(
        "  total:  {:>10} msgs in {:>10.3}s → {:>12.0} msgs/s (round-trip)",
        total_msgs,
        total_elapsed.as_secs_f64(),
        total_rate
    );

    sys.shutdown();
}
