//! Verification tests for the five honesty gaps in
//! `docs/milestones/beam-v1-exit.md`:
//!
//! 1. procedure-version hot reload (re-register in
//!    ProcedureRegistry; old running actor + new spawns)
//! 2. JIT-tier integration — call beam builtins from
//!    JIT-compiled code
//! 3. modest soak: N actors x M ops without deadlock
//! 4. throughput sanity bench — record spawn / send /
//!    table-insert per-op latencies
//!
//! The prelude-macro gap (#1 in the exit-doc list) is verified
//! separately in `beam_prelude_macros.rs` because loading
//! lib/beam/prelude.scm runs through a different code path
//! (expander + record-type registry seeding).

#![cfg(feature = "actor")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cs_runtime::builtins::beam::{
    beam_state, payload_to_sendable, primop_send, primop_spawn, SendableValue,
};
use cs_runtime::Runtime;

// ============================================================
// 1. Procedure-version hot reload.
// ============================================================
//
// Claim being verified: re-registering a procedure under the
// same name in ProcedureRegistry replaces the entry, future
// spawns use the new version, and an already-running actor
// keeps running the version it was spawned with (the Arc is
// captured into the closure at spawn time).

#[test]
fn procedure_reregistration_swaps_for_future_spawns() {
    let observed: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    // v1 of the procedure pushes "v1" and exits.
    let obs_v1 = observed.clone();
    beam_state().procs.register(
        "test:proc-hotswap",
        Arc::new(move |_actor, _args| {
            obs_v1.lock().unwrap().push("v1");
        }),
    );

    primop_spawn("test:proc-hotswap", vec![]).expect("spawn v1");

    // Re-register v2 (different behavior) under the same name.
    let obs_v2 = observed.clone();
    beam_state().procs.register(
        "test:proc-hotswap",
        Arc::new(move |_actor, _args| {
            obs_v2.lock().unwrap().push("v2");
        }),
    );

    primop_spawn("test:proc-hotswap", vec![]).expect("spawn v2");

    // Wait for both actors to finish — the spawn order isn't
    // guaranteed to match the completion order, so we wait for
    // a v1 AND a v2 to land. Up to 1 second.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let v = observed.lock().unwrap().clone();
        let has_v1 = v.iter().any(|s| *s == "v1");
        let has_v2 = v.iter().any(|s| *s == "v2");
        if has_v1 && has_v2 {
            break;
        }
        if Instant::now() >= deadline {
            panic!("proc-hotswap timeout, observed: {:?}", v);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn already_running_actor_keeps_its_original_version() {
    // Spawn an actor that waits for a message before exiting.
    // While it's blocked, re-register the procedure with a new
    // body. Verify the *running* actor still runs the original
    // body's code (i.e., when it wakes up, it does what v1 said
    // to do, not what v2 says).
    let observed: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let obs_v1 = observed.clone();
    beam_state().procs.register(
        "test:proc-running-pin",
        Arc::new(move |actor, _args| {
            // Block until we receive a message, then push "v1".
            let _ = actor.receive();
            obs_v1.lock().unwrap().push("v1");
        }),
    );

    let pid = primop_spawn("test:proc-running-pin", vec![]).expect("spawn v1");

    // Swap to v2 before the actor wakes up.
    let obs_v2 = observed.clone();
    beam_state().procs.register(
        "test:proc-running-pin",
        Arc::new(move |actor, _args| {
            let _ = actor.receive();
            obs_v2.lock().unwrap().push("v2");
        }),
    );

    // Wake the already-running v1 actor.
    primop_send(pid, SendableValue::Symbol("go".into())).expect("send");

    // Spawn a v2 actor and wake it too.
    let pid_v2 = primop_spawn("test:proc-running-pin", vec![]).expect("spawn v2");
    primop_send(pid_v2, SendableValue::Symbol("go".into())).expect("send v2");

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let v = observed.lock().unwrap().clone();
        if v.len() >= 2 {
            // Both actors finished. The running-v1 actor must
            // have pushed "v1" (NOT "v2") despite the re-registration.
            assert!(
                v.iter().any(|s| *s == "v1"),
                "running v1 actor should have pushed v1, observed: {:?}",
                v
            );
            assert!(
                v.iter().any(|s| *s == "v2"),
                "spawned-after-swap actor should have pushed v2, observed: {:?}",
                v
            );
            return;
        }
        if Instant::now() >= deadline {
            panic!("running-pin timeout, observed: {:?}", v);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ============================================================
// 2. JIT-tier integration.
// ============================================================
//
// What this test proves: beam builtins are callable through the
// bytecode VM tier inside a Runtime that has the JIT installed.
// Tier-up of the calling function is attempted (tier_up_count
// bumps), the function returns correct results, and side
// effects on the BeamState (the table) are observable.
//
// What this test does NOT prove: that JIT-compiled native code
// dispatches the beam builtins directly. As of this iter, the
// JIT lowerer doesn't have a code path for Syms-shape builtins
// (which beam_syms_builtins() uses for SymbolTable access), so
// even after tier-up the populate function continues to invoke
// table-insert! via the bytecode dispatch path. That's a
// cs-jit-cranelift extension, not a beam-wiring issue — the
// builtins are correctly registered against both walker and VM
// tiers, which is what cs-runtime's installation API covers.

#[test]
fn beam_builtin_callable_via_vm_with_jit_installed() {
    cs_vm::vm::reset_tier_up_count();
    cs_vm::vm::reset_jit_call_count();

    let mut rt = Runtime::new();
    rt.install_jit().expect("install_jit");

    // Set up a table the recursive function will hammer.
    rt.eval_str_via_vm("<t>", "(make-table 'jit-test-table 'set)")
        .unwrap();

    rt.eval_str_via_vm(
        "<t>",
        "(define populate
           (lambda (n)
             (if (= n 0)
                 'done
                 (begin
                   (table-insert! 'jit-test-table n n)
                   (populate (- n 1))))))",
    )
    .unwrap();

    // Several thousand recursive self-calls — past the tier-up
    // threshold (the existing fib JIT tests show it's in the
    // ~1000-call range).
    let warmup = rt
        .eval_str_via_vm("<t>", "(populate 3000)")
        .expect("populate warmup");
    assert_eq!(
        rt.format_value(&warmup, cs_core::WriteMode::Display),
        "done"
    );

    // Run again — second pass with whatever the tier-up logic
    // produced (compiled or fell-back-to-bytecode).
    rt.eval_str_via_vm("<t>", "(populate 1000)")
        .expect("populate post-warmup");

    // Tier-up is attempted: the runtime's tier-up hook fires
    // even when codegen falls back. (jit_call_count > 0 is the
    // stronger claim — true once cs-jit-cranelift learns to
    // emit calls into Syms-shape builtins; tracked as part of
    // #107.)
    assert!(
        cs_vm::vm::tier_up_count() >= 1,
        "tier-up should have been attempted at least once"
    );

    // Correctness via the bytecode-VM path is what we're
    // primarily asserting:
    let v = rt
        .eval_str_via_vm("<t>", "(table-lookup 'jit-test-table 1)")
        .unwrap();
    assert_eq!(rt.format_value(&v, cs_core::WriteMode::Display), "1");

    // Total size: populate writes keys 3000..=1 then 1000..=1.
    // Set semantics → 3000 distinct entries.
    let sz = rt
        .eval_str_via_vm("<t>", "(table-size 'jit-test-table)")
        .unwrap();
    assert_eq!(rt.format_value(&sz, cs_core::WriteMode::Display), "3000");
}

// ============================================================
// 3. Modest soak — N actors x M ops, no deadlock, bounded
//    latency.
// ============================================================
//
// Spec's B3 acceptance ("1000 actors x 10M ops, p99 < 50 ms")
// requires the work-stealing scheduler (#107). With the
// current spawn-blocking model the realistic ceiling is much
// lower — tokio's max_blocking_threads default is bumped to
// 4096 in cs-actor::ActorSystem::new, but each spawn is an OS
// thread.
//
// This soak runs `N_ACTORS x N_MSGS_EACH` round-trips and
// records wall time + p99. It's a CONFIDENCE test: the system
// doesn't deadlock, every message is delivered, and per-op
// latency stays within a generous envelope. It's not a
// substitute for the spec acceptance.

const SOAK_ACTORS: usize = 100;
const SOAK_MSGS_PER_ACTOR: usize = 20;

#[test]
fn soak_n_actors_m_messages_no_deadlock() {
    let total = SOAK_ACTORS * SOAK_MSGS_PER_ACTOR;
    let completed = Arc::new(AtomicU64::new(0));
    let observed_latencies: Arc<Mutex<Vec<Duration>>> =
        Arc::new(Mutex::new(Vec::with_capacity(total)));

    // Each actor drains its mailbox; for every message, records
    // the time-since-spawn-start and bumps the completion
    // counter. Exits when it sees a 'stop signal.
    let completed_clone = completed.clone();
    let latencies_clone = observed_latencies.clone();
    beam_state().procs.register(
        "test:soak-worker",
        Arc::new(move |actor, _args| loop {
            let msg = match actor.receive() {
                Some(cs_actor::Message::User(p)) => match payload_to_sendable(&p) {
                    Some(s) => s,
                    None => continue,
                },
                Some(_) => continue,
                None => return,
            };
            match &msg {
                SendableValue::Symbol(s) if s == "stop" => return,
                SendableValue::Pair(head, tail) => {
                    // (cons send-time payload)
                    if let SendableValue::Fixnum(ts_ns) = head.as_ref() {
                        let latency = elapsed_since_ns(*ts_ns as u128);
                        latencies_clone.lock().unwrap().push(latency);
                        completed_clone.fetch_add(1, Ordering::Relaxed);
                    }
                    let _ = tail;
                }
                _ => {}
            }
        }),
    );

    // Spawn the soak workers.
    let mut pids = Vec::with_capacity(SOAK_ACTORS);
    for _ in 0..SOAK_ACTORS {
        pids.push(primop_spawn("test:soak-worker", vec![]).expect("spawn"));
    }

    // Send M messages to each worker.
    let start = Instant::now();
    for msg_ix in 0..SOAK_MSGS_PER_ACTOR {
        for pid in &pids {
            let ts = now_ns() as i64;
            let msg = SendableValue::Pair(
                Box::new(SendableValue::Fixnum(ts)),
                Box::new(SendableValue::Fixnum(msg_ix as i64)),
            );
            primop_send(*pid, msg).expect("send");
        }
    }

    // Wait for all messages to land.
    let deadline = Instant::now() + Duration::from_secs(10);
    while completed.load(Ordering::Relaxed) < total as u64 {
        if Instant::now() >= deadline {
            panic!(
                "soak timeout: {} of {} completed",
                completed.load(Ordering::Relaxed),
                total
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let elapsed = start.elapsed();

    // Tell workers to exit.
    for pid in &pids {
        let _ = primop_send(*pid, SendableValue::Symbol("stop".into()));
    }

    // Stats.
    let mut lats = observed_latencies.lock().unwrap().clone();
    lats.sort_unstable();
    let p50 = lats[lats.len() / 2];
    let p99 = lats[(lats.len() as f64 * 0.99) as usize];
    let max = *lats.last().unwrap();
    let throughput_msg_per_sec = total as f64 / elapsed.as_secs_f64();

    eprintln!(
        "soak: {} actors x {} msgs = {} total in {:?}; throughput {:.0} msg/s; latency p50 {:?} p99 {:?} max {:?}",
        SOAK_ACTORS, SOAK_MSGS_PER_ACTOR, total, elapsed, throughput_msg_per_sec, p50, p99, max
    );

    // Confidence assertions: nothing pathological. These are
    // generous bounds to avoid CI flakiness on shared hardware.
    assert_eq!(
        completed.load(Ordering::Relaxed),
        total as u64,
        "every message must be delivered"
    );
    assert!(
        p99 < Duration::from_millis(500),
        "p99 latency {:?} exceeded 500ms — investigate scheduler / mailbox contention",
        p99
    );
}

// ============================================================
// 4. Throughput sanity bench.
// ============================================================
//
// Records per-op latency for the three hot primops so future
// regressions show up. Not a criterion bench (no statistical
// rigor) — a record of headline numbers so anyone reading the
// test output sees the order of magnitude.

#[test]
fn bench_record_spawn_send_table_insert() {
    const N: usize = 1000;

    // Trivial body — exit immediately.
    beam_state()
        .procs
        .register("test:bench-noop", Arc::new(|_actor, _args| {}));

    // spawn
    let t = Instant::now();
    let mut pids = Vec::with_capacity(N);
    for _ in 0..N {
        pids.push(primop_spawn("test:bench-noop", vec![]).expect("spawn"));
    }
    let spawn_total = t.elapsed();

    // send (to a still-alive draining actor; reuse the first
    // pid spammed below)
    beam_state().procs.register(
        "test:bench-drain",
        Arc::new(|actor, _args| while actor.receive().is_some() {}),
    );
    let sink = primop_spawn("test:bench-drain", vec![]).expect("spawn sink");
    let t = Instant::now();
    for _ in 0..N {
        primop_send(sink, SendableValue::Fixnum(0)).expect("send");
    }
    let send_total = t.elapsed();

    // table-insert via the primop
    use cs_runtime::builtins::beam::primop_make_table;
    use cs_runtime::builtins::beam::primop_table_insert;
    primop_make_table("test:bench-table", "set").expect("make-table");
    let t = Instant::now();
    for i in 0..N {
        primop_table_insert(
            "test:bench-table",
            SendableValue::Fixnum(i as i64),
            SendableValue::Fixnum(0),
        )
        .expect("insert");
    }
    let insert_total = t.elapsed();

    eprintln!(
        "bench: spawn {:?}/op, send {:?}/op, table-insert {:?}/op (N={})",
        spawn_total / N as u32,
        send_total / N as u32,
        insert_total / N as u32,
        N
    );

    // sink is an ActorPid (Copy); the actor would exit when
    // the registry's sender for it is dropped. Nothing to do
    // explicitly — the test process tearing down its
    // ActorSystem will drain everything.
    let _ = sink;
}

// ============================================================
// Helpers.
// ============================================================

fn now_ns() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn elapsed_since_ns(ts_ns: u128) -> Duration {
    let now = now_ns();
    Duration::from_nanos((now.saturating_sub(ts_ns)) as u64)
}
