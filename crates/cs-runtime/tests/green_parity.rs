//! Link / monitor / trap-exit parity for whole-body **green** actors
//! (`spawn-source-green`), against the dedicated `spawn-source` path.
//!
//! These exercise the green-specific seams:
//! - a green actor's termination flows through `on_actor_termination` (so its
//!   monitors get `*down*`, its links get `*exit*`), exactly as the dedicated
//!   path does — the green future's clean return → `ExitReason::Normal` via
//!   `spawn_local_activation`'s wrapper;
//! - a green whole-body actor *receives* system messages (`*down*` / `*exit*`)
//!   in its own `(raw-receive)` loop, decoded by `driver_receive` +
//!   `process_received` just like the blocking `primop_raw_receive`.
//!
//! Reasons here are all `'normal` (no Scheme primitive exits with a custom Error
//! reason — abnormal exits come only from Rust panics, mapped to `Error` by the
//! shared `catch_unwind` wrapper). What we verify is the *delivery*, which is the
//! part that differs between the green and dedicated execution paths.

#![cfg(feature = "actor")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_send, primop_spawn, primop_spawn_source,
    primop_spawn_source_green, SendableValue,
};

mod common;
use common::wait_until;

fn sym(s: &str) -> SendableValue {
    SendableValue::Symbol(s.into())
}

/// `(tag . payload)` — the little protocol the bodies below pattern-match.
fn tagged(tag: &str, payload: SendableValue) -> SendableValue {
    SendableValue::Pair(Box::new(sym(tag)), Box::new(payload))
}

/// Register a collector proc that records the first `n` symbol markers it
/// receives into `out`. Spawn it with `primop_spawn(name, vec![])` so the
/// private `ActorPid` type is only ever inferred.
fn register_markers(name: &'static str, n: usize, out: Arc<Mutex<Vec<String>>>) {
    beam_state().procs.register(
        name,
        Arc::new(move |actor, _args| {
            for _ in 0..n {
                match primop_raw_receive(actor, None) {
                    Ok(Some(SendableValue::Symbol(s))) => out.lock().unwrap().push(s.to_string()),
                    Ok(Some(other)) => out.lock().unwrap().push(format!("{other:?}")),
                    _ => break,
                }
            }
        }),
    );
}

#[test]
fn green_termination_notifies_a_monitor() {
    // P6.1: a green actor's termination must fire its monitors' `*down*`, and a
    // green watcher must *receive* that `*down*` in its own loop. The watcher
    // arms the monitor while the target is still alive (confirmed via 'armed)
    // before we tell the target to exit — so there is no monitor-after-death
    // race.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-mon-col", 2, out.clone());
    let col = primop_spawn("test:green-mon-col", vec![]).expect("spawn collector");

    // Target: receive one message ('stop) then return → exits Normal.
    let target = primop_spawn_source_green(
        "(define (g) (raw-receive))".to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn target");

    // Watcher: get col, get the target pid, monitor it, confirm 'armed, then
    // loop until a `*down*` arrives and report 'got-down.
    let watcher = primop_spawn_source_green(
        r#"
        (define (watcher)
          (let ((col (cdr (raw-receive))))
            (let ((g (cdr (raw-receive))))
              (system-monitor! g)
              (send col 'armed)
              (let loop ()
                (let ((m (raw-receive)))
                  (if (and (pair? m) (eq? (car m) '*down*))
                      (send col 'got-down)
                      (loop)))))))
        "#
        .to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    primop_send(watcher, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(watcher, tagged("mon", SendableValue::Pid(target))).unwrap();

    wait_until(
        Duration::from_secs(10),
        "watcher never armed its monitor",
        || out.lock().unwrap().iter().any(|s| s == "armed"),
    );
    // Monitor is established on a live target — now let it exit.
    primop_send(target, sym("stop")).unwrap();

    wait_until(
        Duration::from_secs(10),
        "monitor never received *down*",
        || out.lock().unwrap().iter().any(|s| s == "got-down"),
    );
    let got = out.lock().unwrap().clone();
    assert_eq!(got, vec!["armed", "got-down"]);
}

#[test]
fn green_trap_exit_receives_linked_exit_from_dedicated() {
    // P6.2 (cross-path): a green, trap-exit actor linked to a DEDICATED
    // (`spawn-source`) actor must receive an `*exit*` message when the dedicated
    // one terminates — proving (a) the dedicated actor's termination notifies the
    // green link, and (b) the green whole-body loop decodes the `Exit` system
    // message via driver_receive/process_received. Links fire on Normal exits
    // too; a trap-exit actor gets it as a message rather than dying.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-link-col", 2, out.clone());
    let col = primop_spawn("test:green-link-col", vec![]).expect("spawn collector");

    // Dedicated peer: receive one message ('stop) then return → exits Normal.
    let peer = primop_spawn_source(
        "(define (x) (raw-receive))".to_string(),
        "x".to_string(),
        vec![],
    )
    .expect("spawn dedicated peer");

    // Green, trapping: get col, get peer pid, trap exits, link the peer, confirm
    // 'armed, then loop until an `*exit*` arrives and report 'got-exit.
    let g = primop_spawn_source_green(
        r#"
        (define (g)
          (system-trap-exit! #t)
          (let ((col (cdr (raw-receive))))
            (let ((x (cdr (raw-receive))))
              (system-link! x)
              (send col 'armed)
              (let loop ()
                (let ((m (raw-receive)))
                  (if (and (pair? m) (eq? (car m) '*exit*))
                      (send col 'got-exit)
                      (loop)))))))
        "#
        .to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn green trapper");

    primop_send(g, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(g, tagged("link", SendableValue::Pid(peer))).unwrap();

    wait_until(
        Duration::from_secs(10),
        "green actor never armed its link",
        || out.lock().unwrap().iter().any(|s| s == "armed"),
    );
    // Link is established on a live peer — now let the peer exit.
    primop_send(peer, sym("stop")).unwrap();

    wait_until(
        Duration::from_secs(10),
        "green link never received *exit*",
        || out.lock().unwrap().iter().any(|s| s == "got-exit"),
    );
    let got = out.lock().unwrap().clone();
    assert_eq!(got, vec!["armed", "got-exit"]);
}

#[test]
#[cfg(feature = "regions")]
fn green_park_inside_with_region_terminates_with_error() {
    // P0.1: the TLS region stack is shared by co-located actors, so suspending
    // with a `(with-region)` scope still open is unsound. The driver must refuse
    // it — the actor dies with an Error reason naming the violation. A monitor
    // observes the *down* reason. G parks at depth 0 first (fine) so the watcher
    // can arm its monitor before G trips the guard.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-region-col", 2, out.clone());
    let col = primop_spawn("test:green-region-col", vec![]).expect("spawn collector");

    let g = primop_spawn_source_green(
        r#"
        (define (g)
          (raw-receive)
          (with-region (lambda () (raw-receive))))
        "#
        .to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn g");

    let watcher = primop_spawn_source_green(
        r#"
        (define (watcher)
          (let ((col (cdr (raw-receive))))
            (let ((g (cdr (raw-receive))))
              (system-monitor! g)
              (send col 'armed)
              (let loop ()
                (let ((m (raw-receive)))
                  (if (and (pair? m) (eq? (car m) '*down*))
                      (send col (cadddr m))
                      (loop)))))))
        "#
        .to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    primop_send(watcher, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(watcher, tagged("mon", SendableValue::Pid(g))).unwrap();
    wait_until(Duration::from_secs(10), "watcher never armed", || {
        out.lock().unwrap().iter().any(|s| s == "armed")
    });
    // Trigger: G enters (with-region …) and tries to receive INSIDE it.
    primop_send(g, sym("go")).unwrap();

    wait_until(Duration::from_secs(10), "G never reported a *down*", || {
        out.lock().unwrap().len() >= 2
    });
    let reason = out
        .lock()
        .unwrap()
        .iter()
        .find(|s| *s != "armed")
        .cloned()
        .unwrap_or_default();
    assert!(
        reason.contains("with-region"),
        "G must die with the region-park error; got reason {reason:?}"
    );
}

#[test]
#[cfg(feature = "regions")]
fn green_region_closed_before_receive_runs_fine() {
    // The control: a region opened and CLOSED before the next receive does not
    // trip the guard — only a scope that *spans* a suspend does.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-region-ok-col", 1, out.clone());
    let col = primop_spawn("test:green-region-ok-col", vec![]).expect("spawn collector");

    let g = primop_spawn_source_green(
        r#"
        (define (g)
          (let ((col (cdr (raw-receive))))
            (with-region (lambda () 42))
            (raw-receive)
            (send col 'ok)))
        "#
        .to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn g");

    primop_send(g, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(g, sym("go")).unwrap();
    wait_until(
        Duration::from_secs(10),
        "green actor with a closed region did not run fine",
        || out.lock().unwrap().iter().any(|s| s == "ok"),
    );
}
