//! Cooperative `(sleep)` on a shared `spawn-activation` LocalSet worker.
//!
//! These tests force a **single** worker thread
//! (`CRABSCHEME_ACTOR_LOCAL_WORKERS=1`) so every activation actor is
//! co-located on one thread. That makes the cooperative property directly
//! observable: while one actor sleeps, a co-located peer must keep running.
//! If `(sleep)` blocked the worker (the pre-coroutine behavior) the peer
//! could not run until the sleeper woke, and these assertions would fail.
//!
//! Each test isolates its observations through its own collector actor, so the
//! tests are robust to sharing the single worker when run in parallel.

#![cfg(feature = "actor")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_send, primop_spawn, primop_spawn_activation,
    SendableValue,
};

mod common;
use common::wait_until;

/// Force the shared LocalSet pool to a single worker so activation actors
/// co-locate. Must run before the pool is first built (its first
/// `spawn-activation`); every test calls it first, and the value is constant,
/// so the once-built pool always has exactly one worker for this process.
fn force_single_worker() {
    std::env::set_var("CRABSCHEME_ACTOR_LOCAL_WORKERS", "1");
}

fn sym(s: &str) -> SendableValue {
    SendableValue::Symbol(s.into())
}

/// `(tag . payload)` — the small protocol the handlers below pattern-match.
fn tagged(tag: &str, payload: SendableValue) -> SendableValue {
    SendableValue::Pair(Box::new(sym(tag)), Box::new(payload))
}

/// Register a collector proc that records the first `n` markers it receives, in
/// arrival order, into `out`. Spawn it with `primop_spawn(name, vec![])` at the
/// call site (so the private `ActorPid` type is only ever inferred, never named).
fn register_order_collector(name: &'static str, n: usize, out: Arc<Mutex<Vec<String>>>) {
    beam_state().procs.register(
        name,
        Arc::new(move |actor, _args| {
            for _ in 0..n {
                match primop_raw_receive(actor, None) {
                    Ok(Some(sv)) => out.lock().unwrap().push(render(&sv)),
                    _ => break,
                }
            }
        }),
    );
}

/// Render a received marker to a stable string for assertions.
fn render(sv: &SendableValue) -> String {
    match sv {
        SendableValue::Symbol(s) => s.to_string(),
        other => format!("{other:?}"),
    }
}

#[test]
fn cooperative_sleep_lets_colocated_actor_progress() {
    force_single_worker();

    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:coop-order", 3, order.clone());
    let col = primop_spawn("test:coop-order", vec![]).expect("spawn collector");

    // A: on `go`, mark `a-start`, sleep, then mark `a-done`.
    let a = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go) (send col 'a-start) (sleep-ms 200) (send col 'a-done) #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn A");

    // B: on `go`, mark `b-done` and stop.
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

    // A goes first; wait until it has recorded `a-start` — i.e. it is now
    // parked in `(sleep-ms 200)`. Only THEN release B.
    primop_send(a, sym("go")).unwrap();
    wait_until(Duration::from_secs(2), "A never recorded a-start", || {
        order.lock().unwrap().iter().any(|s| s == "a-start")
    });

    // B is co-located on the same single worker. If the sleeping A blocked the
    // worker, B could not run until A woke (200 ms), so `b-done` would land
    // AFTER `a-done`. Cooperative sleep lets B run during A's nap.
    primop_send(b, sym("go")).unwrap();

    wait_until(Duration::from_secs(3), "did not collect 3 markers", || {
        order.lock().unwrap().len() >= 3
    });

    let got = order.lock().unwrap().clone();
    assert_eq!(
        got,
        vec!["a-start", "b-done", "a-done"],
        "b-done must arrive during A's sleep (between a-start and a-done)"
    );
}

#[test]
fn self_is_correct_after_waking() {
    force_single_worker();

    // A reports `(self)` both before sleeping and after waking; B reports its
    // own marker in between (during A's nap). If ACTOR_CTX is correctly
    // re-installed for A on resume, A's two self-reports are identical — even
    // though B clobbered the shared thread-local while A slept.
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:coop-self", 3, order.clone());
    let col = primop_spawn("test:coop-self", vec![]).expect("spawn collector");

    let a = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go) (send col (self)) (sleep-ms 150) (send col (self)) #f)
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
            ((eq? msg 'go) (send col 'b) #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn B");

    primop_send(a, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(b, tagged("col", SendableValue::Pid(col))).unwrap();

    // A goes first, reports self #1, then parks in `(sleep-ms 150)`.
    primop_send(a, sym("go")).unwrap();
    wait_until(
        Duration::from_secs(2),
        "A never reported its first self",
        || !order.lock().unwrap().is_empty(),
    );
    // While A sleeps, B runs and overwrites the shared ACTOR_CTX thread-local.
    primop_send(b, sym("go")).unwrap();

    wait_until(Duration::from_secs(3), "did not collect 3 markers", || {
        order.lock().unwrap().len() >= 3
    });

    let got = order.lock().unwrap().clone();
    assert_eq!(
        got[1], "b",
        "B should report during A's sleep (between A's two self reports)"
    );
    assert_eq!(
        got[0], got[2],
        "(self) must be A's pid both before sleeping and after waking — proving \
         ACTOR_CTX is correctly re-installed for A on resume despite B running mid-nap"
    );
    assert!(
        got[0].contains("pid"),
        "A's self should render as a pid, got {:?}",
        got[0]
    );
}

#[test]
fn many_colocated_sleepers_all_wake() {
    force_single_worker();

    const N: usize = 20;
    let done = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:coop-many", N, done.clone());
    let col = primop_spawn("test:coop-many", vec![]).expect("spawn collector");

    let src = r#"
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'go))
             (sleep-ms 100)
             (send (cdr msg) 'done)
             #f)
            (else #t)))
    "#;

    let pids: Vec<_> = (0..N)
        .map(|_| primop_spawn_activation(src.to_string(), "handler".to_string()).expect("spawn"))
        .collect();

    // Release them all; each parks in `(sleep-ms 100)`. If sleeps serialized on
    // the single worker this would take >= N*100ms = 2s; cooperatively they
    // overlap and all wake within a few hundred ms.
    let start = Instant::now();
    for p in &pids {
        primop_send(*p, tagged("go", SendableValue::Pid(col))).unwrap();
    }

    wait_until(Duration::from_secs(5), "not all sleepers woke", || {
        done.lock().unwrap().len() >= N
    });
    let elapsed = start.elapsed();

    assert_eq!(done.lock().unwrap().len(), N);
    assert!(
        elapsed < Duration::from_millis(1500),
        "all {N} co-located sleepers should overlap (took {elapsed:?}; serial blocking would be ~{}ms)",
        N * 100
    );
}

#[test]
fn deep_nontail_handler_runs_on_coroutine_stack() {
    force_single_worker();

    // Each activation handler now runs on a coroutine stack rather than the
    // worker thread's stack. A non-tail recursion (every `+` keeps a pending
    // frame on the host stack) must still have real headroom — proving the
    // stack is the 2 MiB we sized, not corosensei's 4 KiB minimum. `sum 1..=200`
    // is ~200 deep non-tail (a 4 KiB stack would hold only a handful) = 20100.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:coop-deep", 1, out.clone());
    let col = primop_spawn("test:coop-deep", vec![]).expect("spawn collector");

    let a = primop_spawn_activation(
        r#"
        (define col #f)
        (define (sum n) (if (= n 0) 0 (+ n (sum (- n 1)))))
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go) (send col (sum 200)) #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn A");

    primop_send(a, tagged("col", SendableValue::Pid(col))).unwrap();
    primop_send(a, sym("go")).unwrap();

    wait_until(
        Duration::from_secs(3),
        "deep handler never reported",
        || !out.lock().unwrap().is_empty(),
    );
    assert!(
        out.lock().unwrap()[0].contains("20100"),
        "deep non-tail recursion result; got {:?}",
        out.lock().unwrap()[0]
    );
}

#[test]
fn peer_error_does_not_disturb_a_sleeper() {
    force_single_worker();

    // A sleeps; while it naps, a co-located C raises an error (its handler
    // returns Err, so that actor terminates). A must still wake and finish — a
    // peer dying mid-nap must not corrupt the shared worker, the stack pool, or
    // A's suspended coroutine.
    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_order_collector("test:coop-err", 2, out.clone());
    let col = primop_spawn("test:coop-err", vec![]).expect("spawn collector");

    let a = primop_spawn_activation(
        r#"
        (define col #f)
        (define (handler msg)
          (cond
            ((and (pair? msg) (eq? (car msg) 'col)) (set! col (cdr msg)) #t)
            ((eq? msg 'go) (send col 'a-start) (sleep-ms 200) (send col 'a-done) #f)
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn A");

    let c = primop_spawn_activation(
        r#"
        (define (handler msg)
          (cond
            ((eq? msg 'go) (error "boom from C"))
            (else #t)))
        "#
        .to_string(),
        "handler".to_string(),
    )
    .expect("spawn C");

    primop_send(a, tagged("col", SendableValue::Pid(col))).unwrap();

    primop_send(a, sym("go")).unwrap();
    wait_until(Duration::from_secs(2), "A never started", || {
        out.lock().unwrap().iter().any(|s| s == "a-start")
    });
    // C errors out on the same worker while A is parked in its sleep.
    primop_send(c, sym("go")).unwrap();

    wait_until(
        Duration::from_secs(3),
        "A never finished after a peer errored mid-nap",
        || out.lock().unwrap().iter().any(|s| s == "a-done"),
    );
    let got = out.lock().unwrap().clone();
    assert!(
        got.contains(&"a-start".to_string()) && got.contains(&"a-done".to_string()),
        "A must complete despite a co-located peer erroring during its sleep; got {got:?}"
    );
}
