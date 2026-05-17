//! B7 acceptance E2E: counter-state-migration 1.0 -> 2.0.
//!
//! Spec lines 519-520:
//!   B7 | cs-hotreload advanced — JIT invalidation, state migration
//!      | callback, code-change ergonomics |
//!      | Counter-state-migration E2E: 1.0 -> 2.0 with added field
//!
//! What this test proves end-to-end:
//!
//! 1. `(load-module! 'counter v1-exports)` registers a module
//!    whose `"init-state"` export is a fixnum (the v1 state
//!    shape).
//!
//! 2. A counter actor reads its initial state via
//!    `(lookup-code 'counter "init-state")`, handles `'inc` /
//!    `'get` messages, and watches `"schema-version"` for code
//!    changes.
//!
//! 3. The driver thread calls `(load-module! 'counter v2-exports)`
//!    promoting the prior version to "old" and installing v2.
//!    v2's `"init-state"` is `(fixnum . metadata-fixnum)` — the
//!    same counter value plus a new field.
//!
//! 4. The actor notices the schema bump on its next message,
//!    runs a registered migration `(fn old-state) -> new-state`
//!    that produces the v2-shaped state. The migration is what
//!    `define-state-migration` from `lib/beam/prelude.scm` would
//!    register in a fully-prelude-driven flow.
//!
//! 5. Both versions remain in the registry. `(lookup-code-old
//!    'counter "init-state")` still returns the v1 shape;
//!    `(lookup-code ...)` returns v2. `(code-soft-purge!
//!    'counter 0)` drops the old.
//!
//! What this test does NOT prove (post-1.0 per the spec):
//!   - JIT-compiled function code is invalidated on reload.
//!     Cranelift's safepoint emitter is still maturing; the spec
//!     defers JIT-invalidation to a later iteration.

#![cfg(feature = "actor")]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use cs_runtime::builtins::beam::{beam_state, payload_to_sendable, primop_send, SendableValue};
use cs_runtime::Runtime;

/// The migration function. In the prelude-driven flow this is
/// what `(define-state-migration counter ((from-version 1) state)
/// (cons state 0))` registers. Pulled out as a Rust function
/// for the v1-prototype E2E.
fn migrate_v1_to_v2(old: &SendableValue) -> SendableValue {
    // v1 state: an integer. v2 state: (cons integer metadata=0).
    SendableValue::Pair(Box::new(old.clone()), Box::new(SendableValue::Fixnum(0)))
}

#[test]
fn counter_state_migration_v1_to_v2() {
    // -----------------------------------------------------------
    // 1. Load counter@v1.
    // -----------------------------------------------------------
    let mut driver = Runtime::new();
    driver
        .eval_str(
            "<t>",
            r#"
            (load-module! 'beam-e2e-counter
              '(("init-state"     . 0)
                ("schema-version" . 1)))
        "#,
        )
        .expect("load v1");

    // -----------------------------------------------------------
    // 2. Register a counter actor body. The body runs a Scheme
    //    Runtime that re-uses the process-wide BeamState (the
    //    OnceLock makes BeamState a singleton, so every Runtime
    //    sees the same loaded modules and actor mailboxes).
    //
    //    Protocol the actor implements:
    //      ('get reply-to)  -> reply with current counter value
    //      ('inc)           -> bump counter
    //      ('migrate-check) -> compare schema versions; if the
    //                          loaded module advanced past the
    //                          actor's local epoch, run the
    //                          migration in-place and adopt the
    //                          new state shape.
    //      ('snapshot reply-to) -> reply with the *full* state
    //                              (so the test can verify
    //                              shape).
    // -----------------------------------------------------------

    let result: Arc<Mutex<Option<SendableValue>>> = Arc::new(Mutex::new(None));
    let result_clone = result.clone();

    beam_state().procs.register(
        "test:e2e-counter",
        Arc::new(move |actor, _args| {
            let mut rt = cs_runtime::Runtime::new();

            // Read initial state from the registry.
            let init = rt
                .eval_str("<t>", r#"(lookup-code 'beam-e2e-counter "init-state")"#)
                .expect("init-state lookup");
            let mut state = cs_runtime::builtins::beam::to_sendable_in(&init, rt.symbols())
                .expect("encode init");

            let mut local_schema: i64 = 1;

            loop {
                let msg = match actor.receive() {
                    Some(cs_actor::Message::User(p)) => {
                        payload_to_sendable(&p).expect("our payloads are SendableValue")
                    }
                    Some(_) => continue,
                    None => break, // mailbox closed
                };

                // Decode (tag . rest)
                let (tag, rest) = match &msg {
                    SendableValue::Pair(h, t) => (h.as_ref().clone(), t.as_ref().clone()),
                    SendableValue::Symbol(s) => {
                        (SendableValue::Symbol(s.clone()), SendableValue::Null)
                    }
                    _ => continue,
                };
                let tag_name = match &tag {
                    SendableValue::Symbol(s) => s.clone(),
                    _ => continue,
                };

                match tag_name.as_str() {
                    "inc" => {
                        state = bump_counter(&state);
                    }
                    "get" => {
                        if let SendableValue::Pair(reply_to, _) = rest {
                            if let SendableValue::Pid(pid) = *reply_to {
                                let _ = primop_send(pid, counter_value(&state));
                            }
                        }
                    }
                    "snapshot" => {
                        if let SendableValue::Pair(reply_to, _) = rest {
                            if let SendableValue::Pid(pid) = *reply_to {
                                let _ = primop_send(pid, state.clone());
                            }
                        }
                    }
                    "migrate-check" => {
                        // Check whether the loaded module has
                        // advanced past our local schema.
                        let sv = rt
                            .eval_str("<t>", r#"(lookup-code 'beam-e2e-counter "schema-version")"#)
                            .expect("schema-version lookup");
                        let new_schema = match &sv {
                            cs_core::Value::Number(cs_core::Number::Fixnum(n)) => *n,
                            _ => local_schema,
                        };
                        if new_schema > local_schema {
                            // Promote state through every step.
                            // For this test only v1 -> v2 is
                            // defined; in real usage the
                            // migration table would have entries
                            // for each transition.
                            for from in local_schema..new_schema {
                                if from == 1 {
                                    state = migrate_v1_to_v2(&state);
                                }
                            }
                            local_schema = new_schema;
                        }
                        // Hand the new (epoch, state) back to the
                        // test via the shared mutex.
                        *result_clone.lock().unwrap() = Some(SendableValue::Pair(
                            Box::new(SendableValue::Fixnum(local_schema)),
                            Box::new(state.clone()),
                        ));
                    }
                    "stop" => break,
                    _ => {}
                }
            }
        }),
    );

    // -----------------------------------------------------------
    // 3. Spawn the actor + send increment messages.
    // -----------------------------------------------------------
    let pid_val = driver
        .eval_str("<t>", "(spawn 'test:e2e-counter)")
        .expect("spawn counter");
    let pid_display = driver.format_value(&pid_val, cs_core::WriteMode::Display);
    let target_pid = parse_pid_symbol(&pid_display);

    // 3 increments
    for _ in 0..3 {
        primop_send(target_pid, SendableValue::Symbol("inc".into())).expect("send inc");
    }

    // migrate-check while still on v1 — state should be 3,
    // schema-version 1. Wait for it.
    wait_for_result(&result, Duration::from_secs(1), || {
        primop_send(target_pid, SendableValue::Symbol("migrate-check".into()))
            .expect("send migrate-check");
    });
    {
        let snapshot = result.lock().unwrap().take().unwrap();
        let (epoch, state) = decode_pair(&snapshot);
        assert_eq!(epoch, 1, "still on v1 schema");
        assert_eq!(state, SendableValue::Fixnum(3), "3 after 3 increments");
    }

    // -----------------------------------------------------------
    // 4. Reload counter@v2. v2's "init-state" exists but the
    //    running actor doesn't read it on reload — it migrates
    //    its own state in place.
    // -----------------------------------------------------------
    driver
        .eval_str(
            "<t>",
            r#"
            (load-module! 'beam-e2e-counter
              '(("init-state"     . (0 . 0))
                ("schema-version" . 2)))
        "#,
        )
        .expect("load v2");

    // (code-versions ...) should report (1 2)
    let versions = driver
        .eval_str("<t>", "(code-versions 'beam-e2e-counter)")
        .unwrap();
    assert_eq!(
        driver.format_value(&versions, cs_core::WriteMode::Display),
        "(1 2)",
        "two-version registry shows old=1 current=2"
    );

    // The actor's local epoch is still 1 until it processes the
    // next migrate-check.
    wait_for_result(&result, Duration::from_secs(1), || {
        primop_send(target_pid, SendableValue::Symbol("migrate-check".into()))
            .expect("send migrate-check after reload");
    });
    {
        let snapshot = result.lock().unwrap().take().unwrap();
        let (epoch, state) = decode_pair(&snapshot);
        assert_eq!(epoch, 2, "actor adopted v2 schema");
        // State went from Fixnum(3) to (3 . 0)
        assert_eq!(
            state,
            SendableValue::Pair(
                Box::new(SendableValue::Fixnum(3)),
                Box::new(SendableValue::Fixnum(0)),
            ),
            "v1->v2 migration wrapped 3 into (3 . 0)"
        );
    }

    // -----------------------------------------------------------
    // 5. Old version still visible until purged.
    // -----------------------------------------------------------
    let old_init = driver
        .eval_str("<t>", r#"(lookup-code-old 'beam-e2e-counter "init-state")"#)
        .unwrap();
    assert_eq!(
        driver.format_value(&old_init, cs_core::WriteMode::Display),
        "0",
        "old version still holds v1 init-state"
    );

    driver
        .eval_str("<t>", "(code-soft-purge! 'beam-e2e-counter 0)")
        .expect("soft-purge with 0 holders");

    let old_init_after_purge = driver
        .eval_str("<t>", r#"(lookup-code-old 'beam-e2e-counter "init-state")"#)
        .unwrap();
    assert_eq!(
        driver.format_value(&old_init_after_purge, cs_core::WriteMode::Display),
        "#f",
        "old version dropped after soft-purge"
    );

    // Tidy up so a re-run of the test under cargo's parallel
    // executor on the same process doesn't see stale state.
    driver
        .eval_str("<t>", "(code-unload! 'beam-e2e-counter)")
        .ok();
    primop_send(target_pid, SendableValue::Symbol("stop".into())).ok();
}

// ============================================================
// Helpers.
// ============================================================

fn parse_pid_symbol(name: &str) -> cs_actor::ActorPid {
    let inner = name
        .strip_prefix("<pid:<")
        .and_then(|s| s.strip_suffix(">>"))
        .unwrap();
    let (n, l) = inner.split_once('.').unwrap();
    cs_actor::ActorPid {
        node: n.parse().unwrap(),
        local_id: l.parse().unwrap(),
    }
}

fn decode_pair(sv: &SendableValue) -> (i64, SendableValue) {
    if let SendableValue::Pair(head, tail) = sv {
        if let SendableValue::Fixnum(n) = head.as_ref() {
            return (*n, tail.as_ref().clone());
        }
    }
    panic!("expected (fixnum . state) pair, got {:?}", sv)
}

fn bump_counter(state: &SendableValue) -> SendableValue {
    match state {
        SendableValue::Fixnum(n) => SendableValue::Fixnum(n + 1),
        SendableValue::Pair(head, tail) => SendableValue::Pair(
            Box::new(match head.as_ref() {
                SendableValue::Fixnum(n) => SendableValue::Fixnum(n + 1),
                other => other.clone(),
            }),
            tail.clone(),
        ),
        other => other.clone(),
    }
}

fn counter_value(state: &SendableValue) -> SendableValue {
    match state {
        SendableValue::Fixnum(n) => SendableValue::Fixnum(*n),
        SendableValue::Pair(head, _) => head.as_ref().clone(),
        other => other.clone(),
    }
}

fn wait_for_result<F: FnOnce()>(
    result: &Arc<Mutex<Option<SendableValue>>>,
    timeout: Duration,
    trigger: F,
) {
    *result.lock().unwrap() = None;
    trigger();
    let deadline = std::time::Instant::now() + timeout;
    while result.lock().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            panic!(
                "counter actor did not produce a snapshot within {:?}",
                timeout
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
