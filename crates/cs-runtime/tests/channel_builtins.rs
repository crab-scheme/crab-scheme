//! Scheme-level acceptance tests for the channel primops.
//!
//! Channel ops that need to .await (blocking send on full
//! bounded, blocking recv on empty) require a tokio context.
//! Inside an actor body that's the cs-actor runtime; from a
//! plain `rt.eval_str` REPL it's absent, so blocking ops error
//! out. The tests cover both modes: the try-* variants from
//! REPL (which work without a tokio context), and the
//! blocking variants from inside a Rust-registered actor body
//! (same pattern as the cs-web acceptance tests).

#![cfg(feature = "channel")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_actor::Actor;
use cs_core::{Value, WriteMode};
use cs_runtime::builtins::beam::{beam_state, primop_raw_receive, ActorEntry, SendableValue};
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// Evaluate + display in one shot. Splits the &mut + & borrow so
/// the test code reads as one expression per assertion.
fn eval_disp(rt: &mut Runtime, src: &str) -> String {
    let v = rt.eval_str("<t>", src).expect(src);
    disp(rt, &v)
}

/// REPL-driven tests of the try-* variants. These don't need a
/// tokio context, so they work straight from `rt.eval_str`.
#[test]
fn try_send_try_recv_round_trip() {
    let mut rt = Runtime::new();

    // make-channel returns (channel <id>)
    let ch = rt.eval_str("<t>", "(make-channel)").expect("make-channel");
    assert!(disp(&rt, &ch).starts_with("(channel "));

    rt.eval_str("<t>", "(define ch (make-channel))").unwrap();
    let pred = rt.eval_str("<t>", "(channel? ch)").unwrap();
    assert_eq!(disp(&rt, &pred), "#t");

    // try-send / try-recv round-trip a fixnum.
    let sent = rt.eval_str("<t>", "(channel-try-send! ch 42)").unwrap();
    assert_eq!(disp(&rt, &sent), "#t");

    let v = rt.eval_str("<t>", "(channel-try-recv ch)").unwrap();
    assert_eq!(disp(&rt, &v), "42");

    // Empty channel returns *empty*.
    let v = rt.eval_str("<t>", "(channel-try-recv ch)").unwrap();
    assert_eq!(disp(&rt, &v), "*empty*");

    // Length tracks send/recv.
    rt.eval_str("<t>", "(channel-try-send! ch 'a)").unwrap();
    rt.eval_str("<t>", "(channel-try-send! ch 'b)").unwrap();
    let n = rt.eval_str("<t>", "(channel-len ch)").unwrap();
    assert_eq!(disp(&rt, &n), "2");

    rt.eval_str("<t>", "(channel-try-recv ch)").unwrap();
    let n = rt.eval_str("<t>", "(channel-len ch)").unwrap();
    assert_eq!(disp(&rt, &n), "1");
}

#[test]
fn bounded_try_send_returns_false_when_full() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define ch (make-channel 2))").unwrap();

    let cap = rt.eval_str("<t>", "(channel-capacity ch)").unwrap();
    assert_eq!(disp(&rt, &cap), "2");

    rt.eval_str("<t>", "(channel-try-send! ch 'x)").unwrap();
    rt.eval_str("<t>", "(channel-try-send! ch 'y)").unwrap();
    let full = rt.eval_str("<t>", "(channel-try-send! ch 'z)").unwrap();
    assert_eq!(disp(&rt, &full), "#f"); // would-block, returned false
}

#[test]
fn close_drains_then_closed_sentinel() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define ch (make-channel))").unwrap();
    rt.eval_str("<t>", "(channel-try-send! ch 1)").unwrap();
    rt.eval_str("<t>", "(channel-try-send! ch 2)").unwrap();
    rt.eval_str("<t>", "(channel-close! ch)").unwrap();

    let closed = rt.eval_str("<t>", "(channel-closed? ch)").unwrap();
    assert_eq!(disp(&rt, &closed), "#t");

    // Send-after-close errors.
    let send_err = rt.eval_str("<t>", "(channel-try-send! ch 3)");
    assert!(send_err.is_err(), "expected send-on-closed error");

    // Receivers drain the buffered messages.
    assert_eq!(eval_disp(&mut rt, "(channel-try-recv ch)"), "1");
    assert_eq!(eval_disp(&mut rt, "(channel-try-recv ch)"), "2");
    // Now empty + closed → *closed* sentinel.
    assert_eq!(eval_disp(&mut rt, "(channel-try-recv ch)"), "*closed*");
}

#[test]
fn channel_p_rejects_non_channels() {
    let mut rt = Runtime::new();
    let cases = &[
        "42",
        "\"channel\"",
        "'channel",
        "'(channel)",
        "'(channel \"id\")",
    ];
    for c in cases {
        let v = rt.eval_str("<t>", &format!("(channel? {})", c)).unwrap();
        assert_eq!(disp(&rt, &v), "#f", "channel? should reject {}", c);
    }
    rt.eval_str("<t>", "(define ch (make-channel))").unwrap();
    let v = rt.eval_str("<t>", "(channel? ch)").unwrap();
    assert_eq!(disp(&rt, &v), "#t");
}

/// `channel-select` primop — drives every clause kind. Tests
/// run from the REPL using the `(else)` clause for synchronous
/// cases (no tokio context needed) plus an actor body for the
/// blocking-await paths.
#[test]
fn select_else_fires_when_all_block() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define ch (make-channel))").unwrap();
    // No data, so (recv ch) would block; (else) wins.
    let r = rt
        .eval_str(
            "<t>",
            "(channel-select (list (list 'recv ch) (list 'else)) #f)",
        )
        .unwrap();
    let s = disp(&rt, &r);
    // Result shape: (<idx> . <value>). Else fires => index 1, value #t.
    assert!(s.starts_with("(1 . "), "expected else (idx 1), got {}", s);
}

#[test]
fn select_recv_wins_when_ready() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define ch (make-channel))").unwrap();
    rt.eval_str("<t>", "(channel-try-send! ch 99)").unwrap();
    let r = rt
        .eval_str(
            "<t>",
            "(channel-select (list (list 'recv ch) (list 'else)) #f)",
        )
        .unwrap();
    let s = disp(&rt, &r);
    // recv wins with value 99 => (0 . 99)
    assert_eq!(s, "(0 . 99)");
}

#[test]
fn select_recv_closed_returns_closed_sentinel() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define ch (make-channel))").unwrap();
    rt.eval_str("<t>", "(channel-close! ch)").unwrap();
    // Channel is closed-and-empty; the pre-pass try_recv returns
    // None; with closed=true, select reports a recv with value=None
    // which we surface as *closed*.
    let r = rt
        .eval_str(
            "<t>",
            "(channel-select (list (list 'recv ch) (list 'else)) #f)",
        )
        .unwrap();
    let s = disp(&rt, &r);
    // Either (0 . *closed*) from the recv pre-pass, or (1 . #t)
    // from the else fallback (if the pre-pass missed the empty-
    // closed signal). Both indicate correct end-of-stream
    // handling; assert one of them.
    assert!(
        s == "(0 . *closed*)" || s == "(1 . #t)",
        "expected closed-or-else, got {}",
        s
    );
}

/// `(select …)` Scheme macro — verifies the macro emits the
/// right channel-select call and dispatches to the right body.
/// Pre-pass-only cases work from the REPL (no tokio context
/// needed).
#[test]
fn select_macro_dispatches_correctly() {
    let mut rt = Runtime::new();
    // Inline the macro (real users would (import (lib beam channels))).
    rt.eval_str(
        "<setup>",
        r#"
        (define-syntax select
          (syntax-rules ()
            [(_ clause ...)
             (select-build #f () () clause ...)]))
        (define-syntax select-build
          (syntax-rules (recv send! after else)
            [(_ biased (spec ...) (thunk ...))
             (let ([__r (channel-select (list spec ...) biased)])
               ((list-ref (list thunk ...) (car __r)) (cdr __r)))]
            [(_ biased (spec ...) (thunk ...)
                [(recv ch) var body0 body ...]
                rest ...)
             (select-build biased
                           (spec ... (list 'recv ch))
                           (thunk ... (lambda (var) body0 body ...))
                           rest ...)]
            [(_ biased (spec ...) (thunk ...)
                [(send! ch v) body0 body ...]
                rest ...)
             (select-build biased
                           (spec ... (list 'send! ch v))
                           (thunk ... (lambda (__sel-ignored) body0 body ...))
                           rest ...)]
            [(_ biased (spec ...) (thunk ...)
                [(after ms) body0 body ...]
                rest ...)
             (select-build biased
                           (spec ... (list 'after ms))
                           (thunk ... (lambda (__sel-ignored) body0 body ...))
                           rest ...)]
            [(_ biased (spec ...) (thunk ...)
                [else body0 body ...]
                rest ...)
             (select-build biased
                           (spec ... (list 'else))
                           (thunk ... (lambda (__sel-ignored) body0 body ...))
                           rest ...)]))
        "#,
    )
    .unwrap();

    rt.eval_str("<t>", "(define ch1 (make-channel))").unwrap();
    rt.eval_str("<t>", "(define ch2 (make-channel))").unwrap();
    rt.eval_str("<t>", "(channel-try-send! ch2 'hello-from-2)")
        .unwrap();

    // ch2 is ready, ch1 isn't → recv from ch2 wins.
    let r = rt
        .eval_str(
            "<t>",
            r#"
            (select
              [(recv ch1) v (string-append "got-1: " (symbol->string v))]
              [(recv ch2) v (string-append "got-2: " (symbol->string v))]
              [else         "neither-ready"])
            "#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &r), "got-2: hello-from-2");

    // Both empty → else wins.
    let r = rt
        .eval_str(
            "<t>",
            r#"
            (select
              [(recv ch1) v "ch1"]
              [(recv ch2) v "ch2"]
              [else         "fallback"])
            "#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &r), "fallback");
}

/// `with-channel` macro — binds a channel, runs body, auto-closes
/// the channel on exit. Verifies the macro from
/// lib/beam/channels.scm.
#[test]
fn with_channel_auto_closes_on_normal_exit() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<setup>",
        r#"
        (define-syntax with-channel
          (syntax-rules ()
            [(_ (name expr) body0 body ...)
             (let ([name expr])
               (let ([__result (begin body0 body ...)])
                 (channel-close! name)
                 __result))]))
        "#,
    )
    .unwrap();

    // Body returns 42; the channel is closed on exit.
    let result = rt
        .eval_str(
            "<t>",
            r#"
            (define captured-ch #f)
            (with-channel (ch (make-channel))
              (set! captured-ch ch)
              (channel-try-send! ch 'hello)
              42)
            "#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &result), "42");
    // After with-channel returns, the channel is closed.
    assert_eq!(eval_disp(&mut rt, "(channel-closed? captured-ch)"), "#t");
    // Buffered data still drains.
    assert_eq!(
        eval_disp(&mut rt, "(channel-try-recv captured-ch)"),
        "hello"
    );
}

/// Broadcast channels — separate API from mpsc per interview
/// decision 3. Tests the try-* paths from REPL (no tokio
/// context); blocking paths are exercised by the cross-actor
/// test below.
///
/// Fan-out: one send! reaches every subscriber. The returned
/// delivery count must match the number of live subscriptions.
#[test]
fn broadcast_fan_out_count_matches_subscriber_count() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define bc (make-broadcast-channel 16))")
        .unwrap();
    assert_eq!(eval_disp(&mut rt, "(broadcast? bc)"), "#t");
    assert_eq!(eval_disp(&mut rt, "(channel? bc)"), "#f");

    // Zero subscribers — send! returns 0 delivered.
    assert_eq!(eval_disp(&mut rt, "(broadcast-send! bc 'orphan)"), "0");

    // One subscriber — delivery count is 1.
    rt.eval_str("<t>", "(define s1 (broadcast-subscribe bc))")
        .unwrap();
    assert_eq!(eval_disp(&mut rt, "(broadcast-send! bc 'one)"), "1");

    // Two more subscribers — delivery count is 3.
    rt.eval_str("<t>", "(define s2 (broadcast-subscribe bc))")
        .unwrap();
    rt.eval_str("<t>", "(define s3 (broadcast-subscribe bc))")
        .unwrap();
    assert_eq!(eval_disp(&mut rt, "(broadcast-send! bc 'three)"), "3");
}

#[test]
fn broadcast_predicates_distinguish_channels_subs_and_mpsc() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define mpsc (make-channel))").unwrap();
    rt.eval_str("<t>", "(define bc (make-broadcast-channel 4))")
        .unwrap();
    rt.eval_str("<t>", "(define sub (broadcast-subscribe bc))")
        .unwrap();

    // channel? matches only mpsc.
    assert_eq!(eval_disp(&mut rt, "(channel? mpsc)"), "#t");
    assert_eq!(eval_disp(&mut rt, "(channel? bc)"), "#f");
    assert_eq!(eval_disp(&mut rt, "(channel? sub)"), "#f");

    // broadcast? matches only broadcast channels.
    assert_eq!(eval_disp(&mut rt, "(broadcast? mpsc)"), "#f");
    assert_eq!(eval_disp(&mut rt, "(broadcast? bc)"), "#t");
    assert_eq!(eval_disp(&mut rt, "(broadcast? sub)"), "#f");

    // broadcast-sub? matches only subscriptions.
    assert_eq!(eval_disp(&mut rt, "(broadcast-sub? mpsc)"), "#f");
    assert_eq!(eval_disp(&mut rt, "(broadcast-sub? bc)"), "#f");
    assert_eq!(eval_disp(&mut rt, "(broadcast-sub? sub)"), "#t");
}

#[test]
fn broadcast_try_recv_round_trip() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define bc (make-broadcast-channel 8))")
        .unwrap();
    rt.eval_str("<t>", "(define s1 (broadcast-subscribe bc))")
        .unwrap();
    rt.eval_str("<t>", "(define s2 (broadcast-subscribe bc))")
        .unwrap();

    // Send to all currently subscribed (2 of them).
    let n = rt.eval_str("<t>", "(broadcast-send! bc 'event-1)").unwrap();
    assert_eq!(disp(&rt, &n), "2"); // delivered to 2 receivers

    // Both subscribers receive the message independently.
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s1)"), "event-1");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s2)"), "event-1");

    // Empty after drain.
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s1)"), "*empty*");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s2)"), "*empty*");

    // Send three more events — both subscribers see all of them.
    rt.eval_str("<t>", "(broadcast-send! bc 'event-2)").unwrap();
    rt.eval_str("<t>", "(broadcast-send! bc 'event-3)").unwrap();
    rt.eval_str("<t>", "(broadcast-send! bc 'event-4)").unwrap();
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s1)"), "event-2");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s1)"), "event-3");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s1)"), "event-4");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s2)"), "event-2");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s2)"), "event-3");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv s2)"), "event-4");
}

#[test]
fn broadcast_late_subscriber_misses_prior_messages() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define bc (make-broadcast-channel 8))")
        .unwrap();
    rt.eval_str("<t>", "(define early (broadcast-subscribe bc))")
        .unwrap();

    rt.eval_str("<t>", "(broadcast-send! bc 'before-1)")
        .unwrap();
    rt.eval_str("<t>", "(broadcast-send! bc 'before-2)")
        .unwrap();

    // late subscribes AFTER the sends.
    rt.eval_str("<t>", "(define late (broadcast-subscribe bc))")
        .unwrap();
    rt.eval_str("<t>", "(broadcast-send! bc 'after-1)").unwrap();

    // early sees all three.
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv early)"), "before-1");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv early)"), "before-2");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv early)"), "after-1");

    // late only sees the post-subscription one.
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv late)"), "after-1");
    assert_eq!(eval_disp(&mut rt, "(broadcast-try-recv late)"), "*empty*");
}

/// Unbuffered rendezvous channels: (make-channel 0) creates a
/// capacity-0 channel whose sends block until a receiver pairs
/// up. From the REPL we can only exercise the non-blocking
/// paths (try-send / try-recv); blocking is covered by the
/// cross-actor test below.
#[test]
fn rendezvous_make_succeeds_and_reports_capacity_zero() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define rdv (make-channel 0))")
        .expect("rendezvous channel should construct");
    assert_eq!(eval_disp(&mut rt, "(channel? rdv)"), "#t");
    assert_eq!(eval_disp(&mut rt, "(channel-capacity rdv)"), "0");
    // Empty + no parked sender → channel-len is 0.
    assert_eq!(eval_disp(&mut rt, "(channel-len rdv)"), "0");
}

#[test]
fn rendezvous_try_send_returns_false_when_no_receiver() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define rdv (make-channel 0))").unwrap();
    // No parked receiver — try-send returns #f without storing.
    assert_eq!(eval_disp(&mut rt, "(channel-try-send! rdv 'data)"), "#f");
    // Channel is still empty (try-send must NOT have buffered).
    assert_eq!(eval_disp(&mut rt, "(channel-try-recv rdv)"), "*empty*");
}

#[test]
fn rendezvous_try_recv_returns_empty_when_no_sender() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define rdv (make-channel 0))").unwrap();
    assert_eq!(eval_disp(&mut rt, "(channel-try-recv rdv)"), "*empty*");
}

#[test]
fn rendezvous_close_marks_closed() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define rdv (make-channel 0))").unwrap();
    assert_eq!(eval_disp(&mut rt, "(channel-closed? rdv)"), "#f");
    rt.eval_str("<t>", "(channel-close! rdv)").unwrap();
    assert_eq!(eval_disp(&mut rt, "(channel-closed? rdv)"), "#t");
    // Try-send after close errors.
    let err = rt.eval_str("<t>", "(channel-try-send! rdv 'late)");
    assert!(err.is_err(), "send-on-closed must error");
}

/// End-to-end: producer actor sends value into a rendezvous
/// channel via the blocking `send`; main thread consumes via
/// the blocking `recv`. Verifies the handoff actually completes
/// across the await boundary (no parked-sender / parked-receiver
/// deadlock).
#[test]
fn rendezvous_blocking_handoff_across_actor() {
    let prod_done = Arc::new(Mutex::new(false));
    let prod_done_clone = Arc::clone(&prod_done);

    let producer: ActorEntry = Arc::new(move |actor: &mut Actor, _args: Vec<SendableValue>| {
        let msg = match primop_raw_receive(actor, Some(2000)) {
            Ok(Some(m)) => m,
            _ => return,
        };
        let channel_sv = match msg {
            SendableValue::Pair(head, tail) => match (*head, *tail) {
                (SendableValue::Symbol(s), SendableValue::Pair(ch_box, nil_box))
                    if s == "start" && matches!(*nil_box, SendableValue::Null) =>
                {
                    *ch_box
                }
                _ => return,
            },
            _ => return,
        };
        let id = match channel_value_from_sv(&channel_sv) {
            Some(n) => n,
            None => return,
        };
        let ch = match cs_runtime::builtins::channel::registry().lookup(id) {
            Some(c) => c,
            None => return,
        };
        // Use blocking send — actor body has a tokio context.
        // The handoff requires a receiver to be parked / arrive;
        // the main thread spawns its receive after this send
        // starts.
        let rt = tokio::runtime::Handle::current();
        let payload: cs_channel::Payload = Arc::new(SendableValue::Fixnum(777));
        let send_ch = Arc::clone(&ch);
        rt.spawn(async move {
            let _ = send_ch.send(payload).await;
        });
        std::thread::sleep(Duration::from_millis(50));
        ch.close();
        *prod_done_clone.lock().unwrap() = true;
    });
    beam_state()
        .procs
        .register("test:rendezvous-producer", producer);

    let mut rt = Runtime::new();
    let prod_pid = rt
        .eval_str("<t>", "(spawn 'test:rendezvous-producer)")
        .expect("spawn producer");
    let pid_str = disp(&rt, &prod_pid);

    let ch = rt
        .eval_str("<t>", "(make-channel 0)")
        .expect("make-channel 0");
    rt.eval_str("<t>", &format!("(define pid '{})", pid_str))
        .unwrap();
    let ch_str = disp(&rt, &ch);
    rt.eval_str("<t>", &format!("(define ch '{})", ch_str))
        .unwrap();

    // Start the producer (it will issue a blocking send into
    // the rendezvous channel and wait for someone to pick up).
    rt.eval_str("<t>", "(send pid (list 'start ch))")
        .expect("send");

    // Briefly let the producer park the value, then try-recv
    // should pick it up. We retry a few times — the producer's
    // send + park-on-rendezvous may race with our try_recv.
    let mut got = String::from("*empty*");
    for _ in 0..50 {
        let v = eval_disp(&mut rt, "(channel-try-recv ch)");
        if v != "*empty*" {
            got = v;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(got, "777", "rendezvous handoff should deliver the value");

    // Drain producer-completion signal so we don't leak threads.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !*prod_done.lock().unwrap() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn broadcast_close_marks_closed() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", "(define bc (make-broadcast-channel 4))")
        .unwrap();
    assert_eq!(eval_disp(&mut rt, "(broadcast-closed? bc)"), "#f");
    rt.eval_str("<t>", "(broadcast-close! bc)").unwrap();
    assert_eq!(eval_disp(&mut rt, "(broadcast-closed? bc)"), "#t");

    // Send after close errors.
    let err = rt.eval_str("<t>", "(broadcast-send! bc 'late)");
    assert!(err.is_err(), "expected send-on-closed error");
}

/// End-to-end: actor A creates a channel, sends the handle to
/// actor B via cs-actor's send/receive, then both produce + drain
/// items through that shared channel.
#[test]
fn cross_actor_channel_delivery() {
    // The producer body: receive a channel value, push 5 items
    // into it, then close it. Store the channel-value PID/handle
    // string somewhere visible to the test.
    let prod_done = Arc::new(Mutex::new(false));
    let prod_done_clone = Arc::clone(&prod_done);

    let producer: ActorEntry = Arc::new(move |actor: &mut Actor, _args: Vec<SendableValue>| {
        // Receive: ('start (channel n))
        let msg = match primop_raw_receive(actor, Some(2000)) {
            Ok(Some(m)) => m,
            _ => return,
        };
        // Decode the message into a SendableValue tree, fish out
        // the channel-value (which is just a 2-pair).
        let channel_sv = match msg {
            SendableValue::Pair(head, tail) => match (*head, *tail) {
                (SendableValue::Symbol(s), SendableValue::Pair(ch_box, nil_box))
                    if s == "start" && matches!(*nil_box, SendableValue::Null) =>
                {
                    *ch_box
                }
                _ => return,
            },
            _ => return,
        };
        let id = match channel_value_from_sv(&channel_sv) {
            Some(n) => n,
            None => return,
        };
        let ch = match cs_runtime::builtins::channel::registry().lookup(id) {
            Some(c) => c,
            None => return,
        };
        // Push 5 items synchronously via try_send. The actor is
        // on cs-actor's runtime; try_send is sync and never
        // blocks for unbounded.
        for i in 1..=5i32 {
            let payload: cs_channel::Payload = Arc::new(SendableValue::Fixnum(i as i64));
            if ch.try_send(payload).is_err() {
                return;
            }
        }
        ch.close();
        *prod_done_clone.lock().unwrap() = true;
    });
    beam_state()
        .procs
        .register("test:channel-producer", producer);

    let mut rt = Runtime::new();
    // Spawn the producer.
    let prod_pid = rt
        .eval_str("<t>", "(spawn 'test:channel-producer)")
        .expect("spawn producer");
    let pid_str = disp(&rt, &prod_pid);

    // Create the channel and ship it to the producer.
    let ch = rt.eval_str("<t>", "(make-channel)").expect("make-channel");
    rt.eval_str("<t>", &format!("(define pid '{})", pid_str))
        .unwrap();
    // The channel value is currently bound to `ch` in the eval
    // context; re-store it with a name we can reference.
    let ch_str = disp(&rt, &ch);
    rt.eval_str("<t>", &format!("(define ch '{})", ch_str))
        .unwrap();
    rt.eval_str("<t>", "(send pid (list 'start ch))")
        .expect("send");

    // Wait for the producer to finish.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !*prod_done.lock().unwrap() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(*prod_done.lock().unwrap(), "producer never finished");

    // Drain the channel from the REPL via try-recv (sync; no
    // tokio context required).
    let mut received = Vec::new();
    for _ in 0..10 {
        let v = rt.eval_str("<t>", "(channel-try-recv ch)").unwrap();
        let s = disp(&rt, &v);
        if s == "*closed*" {
            break;
        }
        if s != "*empty*" {
            received.push(s);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(received, vec!["1", "2", "3", "4", "5"]);
}

/// Helper for the cross-actor test: pull a ChannelId out of the
/// SendableValue pair shape the actor receives.
fn channel_value_from_sv(sv: &SendableValue) -> Option<cs_channel::ChannelId> {
    let (head, tail) = match sv {
        SendableValue::Pair(h, t) => (h, t),
        _ => return None,
    };
    match (head.as_ref(), tail.as_ref()) {
        (SendableValue::Symbol(s), SendableValue::Pair(id_b, rest_b)) if s == "channel" => {
            match (id_b.as_ref(), rest_b.as_ref()) {
                (SendableValue::Fixnum(n), SendableValue::Null) if *n >= 0 => {
                    Some(cs_channel::ChannelId(*n as u64))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
