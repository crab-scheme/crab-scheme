//! Cooperative mid-handler `(raw-receive)` on a shared spawn-activation worker.
//!
//! Companion to `cooperative_sleep.rs`: the same coroutine machinery lets a
//! `(raw-receive)` *inside* a handler park on the async mailbox — releasing the
//! LocalSet worker so co-located actors run — instead of blocking it. These
//! tests force a single worker so actors co-locate and the property is directly
//! observable.

#![cfg(feature = "actor")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_send, primop_spawn, primop_spawn_activation,
    SendableValue,
};

mod common;
use common::wait_until;

fn force_single_worker() {
    std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");
}

fn sym(s: &str) -> SendableValue {
    SendableValue::Symbol(s.into())
}

fn tagged(tag: &str, payload: SendableValue) -> SendableValue {
    SendableValue::Pair(Box::new(sym(tag)), Box::new(payload))
}

/// Register a collector proc (dedicated thread) that records the first `n`
/// markers it receives, in arrival order, into `out`.
fn register_order_collector(name: &'static str, n: usize, out: Arc<Mutex<Vec<String>>>) {
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
fn cooperative_raw_receive_lets_colocated_actor_progress() {
    force_single_worker();

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:recv-order", 3, order.clone());
    let col = primop_spawn("test:recv-order", vec![]).expect("spawn collector");

    // A: on `go`, announce it is waiting, then block in a *mid-handler*
    // `(raw-receive)` until a `wake` arrives, then announce it got it.
    let a = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go)
             (send col 'a-waiting)
             (raw-receive)
             (send col 'a-got)
             #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn A");

    let b = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go) (send col 'b-done) #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn B");

    primop_send(a, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(b, tagged("col", SendableValue::Pid(col))).unwrap();

    // A goes first and parks in `(raw-receive)`.
    primop_send(a, sym("go")).unwrap();
    wait_until(Duration::from_secs(2), "A never announced waiting", || {
        order.lock().unwrap().iter().any(|s| s == "a-waiting")
    });

    // B is co-located. If A's `(raw-receive)` blocked the worker, B could not
    // run; cooperatively it runs while A is parked on the mailbox.
    primop_send(b, sym("go")).unwrap();
    wait_until(
        Duration::from_secs(2),
        "B did not run while A was parked",
        || order.lock().unwrap().iter().any(|s| s == "b-done"),
    );

    // Now wake A's mid-handler receive.
    primop_send(a, sym("wake")).unwrap();
    wait_until(
        Duration::from_secs(2),
        "A never woke from raw-receive",
        || order.lock().unwrap().len() >= 3,
    );

    let got = order.lock().unwrap().clone();
    assert_eq!(
        got,
        vec!["a-waiting", "b-done", "a-got"],
        "b-done must land while A is parked in (raw-receive) — between a-waiting and a-got"
    );
}

#[test]
fn cooperative_raw_receive_timeout_yields_to_peer() {
    force_single_worker();

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:recv-timeout", 3, order.clone());
    let col = primop_spawn("test:recv-timeout", vec![]).expect("spawn collector");

    // A: waits up to 150 ms for a message that never comes -> `'*timeout*`.
    // While it waits, a co-located B must still run.
    let a = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go)
             (send col 'a-wait)
             (let ((m (raw-receive 150)))
               (send col (if (eq? m '*timeout*) 'a-timeout 'a-other)))
             #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn A");

    let b = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go) (send col 'b-done) #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn B");

    primop_send(a, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(b, tagged("col", SendableValue::Pid(col))).unwrap();

    primop_send(a, sym("go")).unwrap();
    wait_until(Duration::from_secs(2), "A never announced waiting", || {
        order.lock().unwrap().iter().any(|s| s == "a-wait")
    });
    // B runs during A's timed wait.
    primop_send(b, sym("go")).unwrap();

    wait_until(Duration::from_secs(3), "did not collect 3 markers", || {
        order.lock().unwrap().len() >= 3
    });

    let got = order.lock().unwrap().clone();
    assert_eq!(
        got,
        vec!["a-wait", "b-done", "a-timeout"],
        "B runs during A's timed receive, and A's receive times out to '*timeout*'"
    );
}
