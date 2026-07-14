//! cs-845.7 — per-actor reduction accounting.
//!
//! The cs-vm reduction-budget countdown (`VM_TICKS_REMAINING`) used to be a
//! single thread_local `Cell<u32>` shared by every green actor multiplexed
//! on one `LocalWorkerPool` worker. Because green actors take turns on the
//! same OS thread, whichever actor happened to be running when the
//! countdown reached some value left it there for whoever ran next —
//! e.g. actor A burning 1500 of its 2000-op slice before suspending via a
//! non-budget route (a `(raw-receive)`, not budget exhaustion) would leave
//! actor B only ~500 ops before B's *own* first yield, even though B never
//! spent any of its budget.
//!
//! The fix (`crates/cs-actor/src/lib.rs`'s `Actor::reduction_slice` +
//! `crates/cs-runtime/src/builtins/beam.rs`'s `pump_coroutine`) saves/
//! restores each actor's own countdown across every coroutine suspend, so
//! the shared thread_local never leaks between co-tenants.
//!
//! Two tests:
//! - `per_actor_slice_isolated_across_switch` is a white-box test of the
//!   exact save/restore protocol `pump_coroutine` follows (mirrors the
//!   existing `parallel_runtime_starvation.rs` style of exercising
//!   `cs_vm::vm` directly instead of spinning up real coroutines): it
//!   proves that following the protocol keeps actor B's countdown at a
//!   full budget regardless of what actor A left behind.
//! - `light_actor_not_starved_by_cpu_bound_neighbor` is an end-to-end
//!   green-actor fairness check: a CPU-bound actor loops indefinitely
//!   while a light "ping" actor replies to a batch of messages; the light
//!   actor must finish promptly, proving cooperative preemption still
//!   works after the accounting was moved off the shared cell.
//!
//! NEITHER test discriminates old-shared vs per-actor accounting on its
//! own (the first replays the protocol by hand; the second passes under
//! either scheme). The discriminating test — poisoned countdown observed
//! through the REAL driver — lives in `reduction_accounting_driver.rs`
//! (own binary, because it needs exclusive use of the worker's per-thread
//! yield counter). These two remain as protocol documentation and a
//! fairness regression guard.

#![cfg(feature = "actor")]

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

/// White-box: replay `pump_coroutine`'s exact save/restore contract with
/// two simulated actors (plain saved-slice cells, no real coroutines) and
/// confirm actor B never observes actor A's leftover countdown.
#[test]
fn per_actor_slice_isolated_across_switch() {
    let prev_budget = cs_vm::vm::reduction_budget();
    cs_vm::vm::set_reduction_budget(2000);

    // Two actors' saved slices, exactly what `Actor::reduction_slice`
    // holds: `None` until they've run their first slice.
    let mut actor_a_slice: Option<u32> = None;
    let actor_b_slice: Option<u32> = None;

    // --- Actor A's first (and only, for this test) slice ---
    // Load: fresh actor -> full budget.
    cs_vm::vm::set_ticks_remaining(
        actor_a_slice.unwrap_or_else(|| cs_vm::vm::reduction_budget().saturating_sub(1)),
    );
    // A burns 1500 ticks (well under its budget) then suspends via a
    // NON-exhaustion route (e.g. `(raw-receive)`) — its countdown is
    // saved as-is, not refilled.
    for _ in 0..1500 {
        cs_vm::vm::vm_tick_reductions();
    }
    let a_after_1500 = cs_vm::vm::ticks_remaining();
    assert!(
        a_after_1500 > 0 && a_after_1500 < 1000,
        "sanity: A should have burned most of its budget, got {a_after_1500} remaining"
    );
    actor_a_slice = Some(a_after_1500);

    // --- Actor B's first slice runs next on the SAME shared thread_local ---
    // Load: B is fresh (`None`) -> MUST get a full budget, not A's leftover.
    cs_vm::vm::set_ticks_remaining(
        actor_b_slice.unwrap_or_else(|| cs_vm::vm::reduction_budget().saturating_sub(1)),
    );
    let b_loaded = cs_vm::vm::ticks_remaining();

    cs_vm::vm::set_reduction_budget(prev_budget);

    assert_eq!(
        b_loaded,
        2000_u32.saturating_sub(1),
        "actor B must load its OWN full budget on its first slice, not A's \
         leftover ({a_after_1500}) still sitting in the shared thread_local \
         countdown — this is the exact bug cs-845.7 fixes"
    );
    // And actor A's own saved slice is untouched by B running.
    assert_eq!(actor_a_slice, Some(a_after_1500));
}

/// Register a Rust-side collector proc that records the first `n` markers
/// it receives (mirrors `green_parity.rs`'s helper).
fn register_markers(name: &'static str, n: usize, out: Arc<Mutex<Vec<String>>>) {
    beam_state().procs.register(
        name,
        Arc::new(move |actor, _args| {
            for _ in 0..n {
                match primop_raw_receive(actor, None) {
                    Ok(Some(SendableValue::Symbol(s))) => out.lock().unwrap().push(s.to_string()),
                    Ok(Some(SendableValue::SymbolId { name, .. })) => {
                        out.lock().unwrap().push(name)
                    }
                    Ok(Some(other)) => out.lock().unwrap().push(format!("{other:?}")),
                    _ => break,
                }
            }
        }),
    );
}

/// Force a single `LocalWorkerPool` worker so the hot and light green
/// actors below are co-located on one OS thread — the exact scenario
/// where reduction-budget bleed between co-tenants would show up.
///
/// NOTE: `env::set_var` is process-global and unsynchronized; it is only
/// safe here because the pool reads the var ONCE (on the first green
/// spawn in this process) and every test in this binary sets the same
/// constant value before spawning. Mirrors `cooperative_sleep.rs`.
fn force_single_worker() {
    std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");
}

#[test]
fn light_actor_not_starved_by_cpu_bound_neighbor() {
    force_single_worker();

    const PONGS: usize = 20;
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:cs845-collector", PONGS, out.clone());
    let col = primop_spawn("test:cs845-collector", vec![]).expect("spawn collector");

    // Hot: a long tight self-recursive loop, entirely VM-interpreted
    // (green actor bodies run with JIT tiering disabled), so it ticks
    // reductions and yields every `budget` ops but otherwise never stops
    // running on its own.
    let _hot = primop_spawn_source_green(
        "(define (hot) (let loop ((i 0)) (if (< i 200000000) (loop (+ i 1)) 'done)))".to_string(),
        "hot".to_string(),
        vec![],
    )
    .expect("spawn hot actor");

    // Light: receives its collector pid, then replies 'pong PONGS times,
    // once per ping it receives.
    let light = primop_spawn_source_green(
        r#"
        (define (light)
          (let ((col (cdr (raw-receive))))
            (let loop ((i 0))
              (if (< i 20)
                  (begin
                    (raw-receive)
                    (send col 'pong)
                    (loop (+ i 1)))
                  'done))))
        "#
        .to_string(),
        "light".to_string(),
        vec![],
    )
    .expect("spawn light actor");

    primop_send(light, tagged("col", SendableValue::Pid(col))).unwrap();
    for _ in 0..PONGS {
        primop_send(light, sym("ping")).unwrap();
    }

    // With cooperative preemption intact, the light actor's replies must
    // all arrive quickly despite sharing its worker with a CPU-bound
    // neighbor. Pre-fix (shared thread_local budget) the mechanism still
    // yields periodically too — this is primarily a regression guard that
    // the accounting refactor didn't break fairness — but a tight bound
    // also catches a refactor that accidentally stops loading/restoring
    // the countdown (which would either hang or make yields wildly
    // irregular).
    wait_until(
        Duration::from_secs(10),
        "light actor's pongs never all arrived — CPU-bound neighbor may be \
         starving it",
        || out.lock().unwrap().len() == PONGS,
    );
    assert_eq!(out.lock().unwrap().len(), PONGS);
}
