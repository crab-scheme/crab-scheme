//! cs-845.8: an uncaught **Scheme-level** error in an actor body must chain to
//! links/monitors as `ExitReason::Error`, exactly as a Rust panic already does.
//! Before this fix, a Scheme error was logged (`eprintln!`) and the actor exited
//! `'normal` — silent (to supervision) death.
//!
//! Covers all four actor-body kinds that can run Scheme code:
//! - `spawn-source` (dedicated, `run_scheme_body` / `scheme_source_entry`)
//! - `spawn-source-green` (whole-body coroutine, `green_source_body`)
//! - `spawn-activation` (framework-driven per-message handler, `activation_body`)
//! - a normal-exit control case (no regression: a body that returns cleanly
//!   still reports `'normal`, on both the dedicated and green paths).

#![cfg(feature = "actor")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_send, primop_spawn, primop_spawn_activation,
    primop_spawn_source_dedicated, primop_spawn_source_green, SendableValue,
};

mod common;
use common::wait_until;

fn sym(s: &str) -> SendableValue {
    SendableValue::Symbol(s.into())
}

fn tagged(tag: &str, payload: SendableValue) -> SendableValue {
    SendableValue::Pair(Box::new(sym(tag)), Box::new(payload))
}

/// Register a collector proc that records the first `n` values it receives
/// (rendered via `{:?}` for symbols so tests can substring-match error
/// payloads) into `out`.
fn register_collector(name: &'static str, n: usize, out: Arc<Mutex<Vec<String>>>) {
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

/// Watcher body shared by the monitor tests: forward `col`, then `target`,
/// then arm a monitor, report `'armed`, and on `*down*` report the reason
/// symbol (car'd out of `(*down* ref-id pid reason)`).
const MONITOR_WATCHER_SRC: &str = r#"
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
"#;

/// Watcher body for the link/trap-exit tests: same shape but arms a
/// `system-link!` after setting trap-exit, and matches `*exit*`.
const LINK_WATCHER_SRC: &str = r#"
(define (watcher)
  (system-trap-exit! #t)
  (let ((col (cdr (raw-receive))))
    (let ((x (cdr (raw-receive))))
      (system-link! x)
      (send col 'armed)
      (let loop ()
        (let ((m (raw-receive)))
          (if (and (pair? m) (eq? (car m) '*exit*))
              (send col (caddr m))
              (loop)))))))
"#;

fn arm_and_report(
    watcher: cs_actor::ActorPid,
    col: cs_actor::ActorPid,
    target: cs_actor::ActorPid,
    out: &Arc<Mutex<Vec<String>>>,
) {
    primop_send(watcher, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(watcher, tagged("mon", SendableValue::Pid(target))).unwrap();
    wait_until(Duration::from_secs(10), "watcher never armed", || {
        out.lock().unwrap().iter().any(|s| s == "armed")
    });
}

fn wait_for_second_report(out: &Arc<Mutex<Vec<String>>>, what: &str) -> String {
    wait_until(Duration::from_secs(10), what, || {
        out.lock().unwrap().len() >= 2
    });
    out.lock()
        .unwrap()
        .iter()
        .find(|s| *s != "armed")
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------
// (a) linked actor dies when its link errors — dedicated + green
// ---------------------------------------------------------------------

#[test]
fn dedicated_scheme_error_chains_error_exit_to_link() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-link-dedicated-col", 2, out.clone());
    let col = primop_spawn("test:cs845-link-dedicated-col", vec![]).expect("spawn collector");

    // Dedicated peer: raises an uncaught error (unbound variable) on 'stop.
    let peer = primop_spawn_source_dedicated(
        "(define (x) (raw-receive) (this-is-not-defined))".to_string(),
        "x".to_string(),
        vec![],
    )
    .expect("spawn dedicated peer");

    let watcher =
        primop_spawn_source_green(LINK_WATCHER_SRC.to_string(), "watcher".to_string(), vec![])
            .expect("spawn watcher");

    arm_and_report(watcher, col, peer, &out);
    primop_send(peer, sym("stop")).unwrap();

    let reason = wait_for_second_report(&out, "link never received *exit* for the errored peer");
    assert!(
        reason.starts_with("error:"),
        "expected an error: exit reason, got {reason:?}"
    );
}

#[test]
fn green_scheme_error_chains_error_exit_to_link() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-link-green-col", 2, out.clone());
    let col = primop_spawn("test:cs845-link-green-col", vec![]).expect("spawn collector");

    // Green peer: raises an uncaught error on 'stop.
    let peer = primop_spawn_source_green(
        "(define (x) (raw-receive) (this-is-not-defined))".to_string(),
        "x".to_string(),
        vec![],
    )
    .expect("spawn green peer");

    let watcher =
        primop_spawn_source_green(LINK_WATCHER_SRC.to_string(), "watcher".to_string(), vec![])
            .expect("spawn watcher");

    arm_and_report(watcher, col, peer, &out);
    primop_send(peer, sym("stop")).unwrap();

    let reason = wait_for_second_report(&out, "link never received *exit* for the errored peer");
    assert!(
        reason.starts_with("error:"),
        "expected an error: exit reason, got {reason:?}"
    );
}

// ---------------------------------------------------------------------
// (b) monitor receives DOWN with Error reason + message
// ---------------------------------------------------------------------

#[test]
fn dedicated_scheme_error_delivers_down_with_error_reason() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-mon-dedicated-col", 2, out.clone());
    let col = primop_spawn("test:cs845-mon-dedicated-col", vec![]).expect("spawn collector");

    let target = primop_spawn_source_dedicated(
        "(define (g) (raw-receive) (car 5))".to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn target");

    let watcher = primop_spawn_source_green(
        MONITOR_WATCHER_SRC.to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    arm_and_report(watcher, col, target, &out);
    primop_send(target, sym("stop")).unwrap();

    let reason =
        wait_for_second_report(&out, "monitor never received *down* for the errored target");
    assert!(
        reason.starts_with("error:"),
        "expected an error: DOWN reason, got {reason:?}"
    );
}

#[test]
fn green_scheme_error_delivers_down_with_error_reason() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-mon-green-col", 2, out.clone());
    let col = primop_spawn("test:cs845-mon-green-col", vec![]).expect("spawn collector");

    let target = primop_spawn_source_green(
        "(define (g) (raw-receive) (car 5))".to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn target");

    let watcher = primop_spawn_source_green(
        MONITOR_WATCHER_SRC.to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    arm_and_report(watcher, col, target, &out);
    primop_send(target, sym("stop")).unwrap();

    let reason =
        wait_for_second_report(&out, "monitor never received *down* for the errored target");
    assert!(
        reason.starts_with("error:"),
        "expected an error: DOWN reason, got {reason:?}"
    );
}

#[test]
fn activation_handler_error_delivers_down_with_error_reason() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-mon-activation-col", 2, out.clone());
    let col = primop_spawn("test:cs845-mon-activation-col", vec![]).expect("spawn collector");

    // Activation handler: raises on the 2nd message ('boom).
    let source = r#"
        (define (handler msg)
          (cond
            ((eq? msg 'boom) (car 5))
            (else #t)))
    "#;
    let target =
        primop_spawn_activation(source.to_string(), "handler".to_string()).expect("spawn target");

    let watcher = primop_spawn_source_green(
        MONITOR_WATCHER_SRC.to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    arm_and_report(watcher, col, target, &out);
    primop_send(target, sym("boom")).unwrap();

    let reason = wait_for_second_report(
        &out,
        "monitor never received *down* for the errored activation",
    );
    assert!(
        reason.starts_with("error:"),
        "expected an error: DOWN reason, got {reason:?}"
    );
}

// ---------------------------------------------------------------------
// (d) Normal exits are unaffected (control cases)
// ---------------------------------------------------------------------

#[test]
fn dedicated_normal_exit_still_reports_normal() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-normal-dedicated-col", 2, out.clone());
    let col = primop_spawn("test:cs845-normal-dedicated-col", vec![]).expect("spawn collector");

    let target = primop_spawn_source_dedicated(
        "(define (g) (raw-receive))".to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn target");

    let watcher = primop_spawn_source_green(
        MONITOR_WATCHER_SRC.to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    arm_and_report(watcher, col, target, &out);
    primop_send(target, sym("stop")).unwrap();

    let reason =
        wait_for_second_report(&out, "monitor never received *down* for the normal target");
    assert_eq!(reason, "normal");
}

// ---------------------------------------------------------------------
// (a') activation × link — links tested on all three Scheme paths
// ---------------------------------------------------------------------

#[test]
fn activation_handler_error_chains_error_exit_to_link() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-link-activation-col", 2, out.clone());
    let col = primop_spawn("test:cs845-link-activation-col", vec![]).expect("spawn collector");

    // Activation handler: raises on 'boom.
    let source = r#"
        (define (handler msg)
          (cond
            ((eq? msg 'boom) (car 5))
            (else #t)))
    "#;
    let peer =
        primop_spawn_activation(source.to_string(), "handler".to_string()).expect("spawn peer");

    let watcher =
        primop_spawn_source_green(LINK_WATCHER_SRC.to_string(), "watcher".to_string(), vec![])
            .expect("spawn watcher");

    arm_and_report(watcher, col, peer, &out);
    primop_send(peer, sym("boom")).unwrap();

    let reason = wait_for_second_report(
        &out,
        "link never received *exit* for the errored activation",
    );
    assert!(
        reason.starts_with("error:"),
        "expected an error: exit reason, got {reason:?}"
    );
}

// ---------------------------------------------------------------------
// load/resolve-phase errors (invalid source / missing entry) also chain
// ---------------------------------------------------------------------

/// Register a Rust-proc watcher: receives a Pid, arms a monitor on it, then
/// waits for the `*down*` and records its reason string. Records "noproc" if
/// the target already died before the monitor could be armed (the caller
/// retries — the target errors at *spawn* here, so there is an inherent race
/// that a Scheme-level watcher cannot win reliably).
fn register_load_watcher(name: &'static str, out: Arc<Mutex<Vec<String>>>) {
    beam_state().procs.register(
        name,
        Arc::new(move |actor, _args| {
            let target = match primop_raw_receive(actor, None) {
                Ok(Some(SendableValue::Pid(p))) => p,
                _ => return,
            };
            if actor.monitor(target).is_err() {
                out.lock().unwrap().push("noproc".into());
                return;
            }
            loop {
                match primop_raw_receive(actor, Some(10_000)) {
                    Ok(Some(msg)) => {
                        // (*down* ref-id pid reason) — walk to the 4th element.
                        let rendered = format!("{msg:?}");
                        if rendered.contains("*down*") {
                            out.lock().unwrap().push(rendered);
                            return;
                        }
                    }
                    _ => return,
                }
            }
        }),
    );
}

#[test]
fn green_load_phase_error_delivers_down_with_error_reason() {
    // Invalid source: the green body never runs — eval_str_via_vm_cached fails
    // at load. That must still be an Error exit (previously: silent Normal).
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_load_watcher("test:cs845-load-green-watcher", out.clone());

    for attempt in 0..5 {
        let watcher = primop_spawn("test:cs845-load-green-watcher", vec![]).expect("spawn watcher");
        let target =
            primop_spawn_source_green("(this is not scheme".to_string(), "g".to_string(), vec![])
                .expect("spawn target");
        primop_send(watcher, SendableValue::Pid(target)).unwrap();

        wait_until(
            Duration::from_secs(10),
            "load-phase watcher never reported",
            || !out.lock().unwrap().is_empty(),
        );
        let got = out.lock().unwrap().pop().unwrap();
        if got == "noproc" {
            // Lost the arming race (target died before monitor) — retry.
            assert!(attempt < 4, "monitor lost the arming race 5 times in a row");
            continue;
        }
        assert!(
            got.contains("error:"),
            "expected an error: DOWN reason for the load-phase failure, got {got:?}"
        );
        return;
    }
}

#[test]
fn green_missing_entry_delivers_down_with_error_reason() {
    // Valid source but no such entry procedure: resolve_and_build_call fails.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_load_watcher("test:cs845-entry-green-watcher", out.clone());

    for attempt in 0..5 {
        let watcher =
            primop_spawn("test:cs845-entry-green-watcher", vec![]).expect("spawn watcher");
        let target = primop_spawn_source_green(
            "(define (g) 1)".to_string(),
            "no-such-entry".to_string(),
            vec![],
        )
        .expect("spawn target");
        primop_send(watcher, SendableValue::Pid(target)).unwrap();

        wait_until(
            Duration::from_secs(10),
            "missing-entry watcher never reported",
            || !out.lock().unwrap().is_empty(),
        );
        let got = out.lock().unwrap().pop().unwrap();
        if got == "noproc" {
            assert!(attempt < 4, "monitor lost the arming race 5 times in a row");
            continue;
        }
        assert!(
            got.contains("error:"),
            "expected an error: DOWN reason for the missing entry, got {got:?}"
        );
        return;
    }
}

#[test]
fn green_normal_exit_still_reports_normal() {
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_collector("test:cs845-normal-green-col", 2, out.clone());
    let col = primop_spawn("test:cs845-normal-green-col", vec![]).expect("spawn collector");

    let target = primop_spawn_source_green(
        "(define (g) (raw-receive))".to_string(),
        "g".to_string(),
        vec![],
    )
    .expect("spawn target");

    let watcher = primop_spawn_source_green(
        MONITOR_WATCHER_SRC.to_string(),
        "watcher".to_string(),
        vec![],
    )
    .expect("spawn watcher");

    arm_and_report(watcher, col, target, &out);
    primop_send(target, sym("stop")).unwrap();

    let reason =
        wait_for_second_report(&out, "monitor never received *down* for the normal target");
    assert_eq!(reason, "normal");
}
