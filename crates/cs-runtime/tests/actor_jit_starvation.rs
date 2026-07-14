//! cs-845.6 — repro for the actor-JIT starvation hypothesis.
//!
//! JIT tiering is force-disabled for all actor bodies by default (see
//! `cs_vm::vm::set_jit_enabled(false)` call sites in
//! `crates/cs-runtime/src/builtins/beam.rs`) because a prior perf branch
//! (perf/actor-vm-jit) found a JIT-tiered CPU-bound actor could starve a
//! co-located peer on a shared `LocalSet` worker. This test forces JIT back
//! on for actor bodies via `CRABSCHEME_ACTOR_JIT=1` (cs-845.6's new gate) and
//! checks whether a JIT-tiered tail-loop actor still lets co-located peers
//! run — both BEFORE and (crucially) well AFTER it has tiered up — on a
//! single forced worker (`CRABSCHEME_ACTOR_LOCAL_WORKERS=1`).
//!
//! Judge finding (exec-actorjit): a single immediate ping isn't sufficient —
//! the default reduction budget is 2000 ops, comfortably fewer than the ops
//! needed to reach the ~1024-self-call JIT tier-up threshold, so the first
//! tick (and the ping it delivers) can fire from the ordinary VM-tier
//! dispatch loop *before* the loop ever tiers up to native code. That proves
//! nothing about the JIT-side tick this bead added
//! (`vm_jit_tick_reductions` at the tail-self back-edge,
//! `crates/cs-jit-cranelift/src/lowering.rs`). So this test now sends a
//! SECOND ping only after a real-world delay long enough to guarantee the
//! busy loop has already tiered up and is running JIT-compiled machine code
//! — if the JIT-side tick were neutered, the busy loop (chosen large enough
//! to run far longer than this test's timeout even at native speed) would
//! never yield again and the second ping would time out.
//!
//! `CRABSCHEME_ACTOR_JIT` is read once into a `OnceLock` inside beam.rs, so
//! it must be set before the first actor body runs in this process. This
//! file contains exactly one test (a separate binary under cargo's
//! per-integration-test-file model) so there's no ordering race with other
//! tests that assume the default (JIT-off) behavior.

#![cfg(feature = "actor")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_spawn, primop_spawn_source_green, SendableValue,
};

mod common;
use common::wait_until;

#[test]
fn jit_enabled_actor_tail_loop_does_not_starve_peer_post_tier_up() {
    // Must be set before any actor code runs (beam.rs OnceLock reads it once).
    std::env::set_var("CRABSCHEME_ACTOR_JIT", "1");
    std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    beam_state().procs.register(
        "test:jit-starve-collector",
        Arc::new({
            let order = order.clone();
            move |actor, _args| {
                // Two markers: an early ping (may land pre-tier-up — not
                // load-bearing on its own) and a post-tier-up ping (the
                // actual assertion).
                for _ in 0..2 {
                    if let Ok(Some(SendableValue::Symbol(s))) = primop_raw_receive(actor, None) {
                        order.lock().unwrap().push(s.to_string());
                    } else {
                        break;
                    }
                }
            }
        }),
    );
    let col = primop_spawn("test:jit-starve-collector", vec![]).expect("spawn collector");

    // Actor A: a tight self-tail-call loop, far past the JIT tier-up
    // threshold (1024 self-calls), with NO receive/sleep inside — the only
    // thing that can release the worker mid-loop is the reduction-tick →
    // yield-hook path this test is probing. `n` is chosen so that even at a
    // generous 5e9 native iterations/sec (this loop body is a compare + add
    // + tail-call — nowhere near that fast in practice) running it to
    // completion untimed would take >10s, comfortably longer than either
    // wait below; a neutered post-tier-up tick would starve peer B2 for the
    // loop's entire (long) run, not just a brief window.
    let source_a = r#"
        (define (busy-loop i n)
          (if (< i n) (busy-loop (+ i 1) n) i))
        (define (start) (busy-loop 0 50000000000))
        "#
    .to_string();
    primop_spawn_source_green(source_a, "start".into(), vec![]).expect("spawn busy actor");

    // Actor B1: co-located, sends its marker immediately on start. This can
    // legitimately land via a pre-tier-up VM-tier tick — informational, not
    // the load-bearing assertion.
    let source_b1 =
        format!(r#"(define (start) (send (string->symbol "<pid:{col}>") 'ping-early))"#);
    primop_spawn_source_green(source_b1, "start".into(), vec![]).expect("spawn early ping actor");

    wait_until(
        Duration::from_secs(10),
        "co-located peer B1 never ran (unexpected even pre-tier-up)",
        || !order.lock().unwrap().is_empty(),
    );

    // Give the busy loop real wall-clock time to run FAR past the ~1024
    // self-call tier-up threshold and start executing JIT-compiled machine
    // code. 300ms is enormously conservative for reaching 1024 iterations of
    // a trivial tail loop under any tier (VM or JIT).
    std::thread::sleep(Duration::from_millis(300));

    // Actor B2: spawned only now. If the busy loop's *JIT-compiled* tail
    // loop never ticks post-tier-up, the worker is monopolized for the rest
    // of the ~5e10-iteration run and B2 never gets scheduled — this times
    // out. If the tick fires (as ADR 0031 + this bead's install_jit wiring
    // intends), B2 runs within one budget interval regardless of how much of
    // the busy loop remains.
    let source_b2 =
        format!(r#"(define (start) (send (string->symbol "<pid:{col}>") 'ping-post-tier-up))"#);
    primop_spawn_source_green(source_b2, "start".into(), vec![])
        .expect("spawn post-tier-up ping actor");

    wait_until(
        Duration::from_secs(10),
        "co-located peer B2 never ran while a JIT-tiered (post-tier-up) tail loop was busy \
         (actor-JIT post-tier-up starvation reproduced)",
        || order.lock().unwrap().len() >= 2,
    );

    let got = order.lock().unwrap().clone();
    assert_eq!(got, vec!["ping-early", "ping-post-tier-up"]);
}
