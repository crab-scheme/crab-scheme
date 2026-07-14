//! cs-845.6 — repro for the actor-JIT starvation hypothesis.
//!
//! JIT tiering is force-disabled for all actor bodies by default (see
//! `cs_vm::vm::set_jit_enabled(false)` call sites in
//! `crates/cs-runtime/src/builtins/beam.rs`) because a prior perf branch
//! (perf/actor-vm-jit) found a JIT-tiered CPU-bound actor could starve a
//! co-located peer on a shared `LocalSet` worker. This test forces JIT back
//! on for actor bodies via `CRABSCHEME_ACTOR_JIT=1` (cs-845.6's new gate) and
//! checks whether a JIT-tiered tail-loop actor still lets a co-located peer
//! run to completion within a timeout, on a single forced worker
//! (`CRABSCHEME_ACTOR_LOCAL_WORKERS=1`).
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
fn jit_enabled_actor_tail_loop_does_not_starve_peer() {
    // Must be set before any actor code runs (beam.rs OnceLock reads it once).
    std::env::set_var("CRABSCHEME_ACTOR_JIT", "1");
    std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    beam_state().procs.register(
        "test:jit-starve-collector",
        Arc::new({
            let order = order.clone();
            move |actor, _args| {
                if let Ok(Some(SendableValue::Symbol(s))) = primop_raw_receive(actor, None) {
                    order.lock().unwrap().push(s.to_string());
                }
            }
        }),
    );
    let col = primop_spawn("test:jit-starve-collector", vec![]).expect("spawn collector");

    // Actor A: a tight self-tail-call loop, far past the JIT tier-up
    // threshold (1024 self-calls), with NO receive/sleep inside — the only
    // thing that can release the worker mid-loop is the reduction-tick →
    // yield-hook path this test is probing.
    let source_a = r#"
        (define (busy-loop i n)
          (if (< i n) (busy-loop (+ i 1) n) i))
        (define (start) (busy-loop 0 200000000))
        "#
    .to_string();
    primop_spawn_source_green(source_a, "start".into(), vec![]).expect("spawn busy actor");

    // Actor B: co-located on the same single worker, sends its marker
    // immediately on start.
    let source_b = format!(
        r#"
        (define (start) (send (string->symbol "<pid:{col}>") 'ping-ok))
        "#
    );
    primop_spawn_source_green(source_b, "start".into(), vec![]).expect("spawn ping actor");

    // If actor A's JIT-compiled loop starves the shared worker, B never
    // gets to run and this times out.
    wait_until(
        Duration::from_secs(10),
        "co-located peer never ran while a JIT-tiered tail loop was busy \
         (actor-JIT starvation reproduced)",
        || !order.lock().unwrap().is_empty(),
    );

    assert_eq!(
        order.lock().unwrap().first().map(String::as_str),
        Some("ping-ok")
    );
}
