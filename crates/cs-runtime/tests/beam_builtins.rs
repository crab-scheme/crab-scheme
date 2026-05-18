//! End-to-end tests for the BEAM-style builtins as exposed to
//! Scheme via the walker / VM tiers. See
//! `crates/cs-runtime/src/builtins/beam.rs`.

#![cfg(feature = "actor")]

use cs_core::WriteMode;
use cs_runtime::Runtime;

mod common;
use common::wait_until;

/// Render a Value to a Scheme-equivalent display string using
/// the runtime's symbol table. Bare `format!("{}", v)` doesn't
/// resolve symbol names (it has no SymbolTable handle), so it
/// prints `#<symbol#NNN>` instead of the symbol's actual name.
fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// Each test uses a unique table name so the process-wide
/// BeamState's TableRegistry doesn't collide across tests under
/// cargo's parallel execution.

#[test]
fn table_make_insert_lookup_via_scheme_walker() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", r#"(make-table 'beam-test-walker-1 'set)"#)
        .expect("make-table");
    rt.eval_str("<t>", r#"(table-insert! 'beam-test-walker-1 "alice" 100)"#)
        .expect("insert");
    let v = rt
        .eval_str("<t>", r#"(table-lookup 'beam-test-walker-1 "alice")"#)
        .expect("lookup");
    // Round-trip projects (100 -> SendableValue::Fixnum -> Value::Number).
    assert_eq!(disp(&rt, &v), "100");
}

#[test]
fn table_lookup_missing_returns_false() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", r#"(make-table 'beam-test-walker-2 'set)"#)
        .expect("make-table");
    let v = rt
        .eval_str("<t>", r#"(table-lookup 'beam-test-walker-2 "missing-key")"#)
        .expect("lookup");
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn table_delete_returns_bool() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", r#"(make-table 'beam-test-walker-3 'set)"#)
        .unwrap();
    rt.eval_str("<t>", r#"(table-insert! 'beam-test-walker-3 1 "one")"#)
        .unwrap();
    let hit = rt
        .eval_str("<t>", r#"(table-delete! 'beam-test-walker-3 1)"#)
        .unwrap();
    assert_eq!(disp(&rt, &hit), "#t");
    let miss = rt
        .eval_str("<t>", r#"(table-delete! 'beam-test-walker-3 1)"#)
        .unwrap();
    assert_eq!(disp(&rt, &miss), "#f");
}

#[test]
fn table_size_grows_and_shrinks() {
    let mut rt = Runtime::new();
    rt.eval_str("<t>", r#"(make-table 'beam-test-walker-4 'set)"#)
        .unwrap();
    rt.eval_str("<t>", r#"(table-insert! 'beam-test-walker-4 1 'a)"#)
        .unwrap();
    rt.eval_str("<t>", r#"(table-insert! 'beam-test-walker-4 2 'b)"#)
        .unwrap();
    rt.eval_str("<t>", r#"(table-insert! 'beam-test-walker-4 3 'c)"#)
        .unwrap();
    let n = rt
        .eval_str("<t>", r#"(table-size 'beam-test-walker-4)"#)
        .unwrap();
    assert_eq!(disp(&rt, &n), "3");
    rt.eval_str("<t>", r#"(table-delete! 'beam-test-walker-4 2)"#)
        .unwrap();
    let n2 = rt
        .eval_str("<t>", r#"(table-size 'beam-test-walker-4)"#)
        .unwrap();
    assert_eq!(disp(&rt, &n2), "2");
}

#[test]
fn table_via_vm_tier() {
    // Same surface, but go through the VM (bytecode) tier rather
    // than the walker. Confirms both registration loops in
    // cs-runtime/src/lib.rs wire the builtins.
    let mut rt = Runtime::new();
    rt.eval_str_via_vm("<t>", r#"(make-table 'beam-test-vm-1 'set)"#)
        .unwrap();
    rt.eval_str_via_vm("<t>", r#"(table-insert! 'beam-test-vm-1 "k" "v")"#)
        .unwrap();
    let v = rt
        .eval_str_via_vm("<t>", r#"(table-lookup 'beam-test-vm-1 "k")"#)
        .unwrap();
    assert_eq!(disp(&rt, &v), "v");
}

#[test]
fn spawn_unknown_proc_errors() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", r#"(spawn 'no-such-registered-proc)"#)
        .expect_err("spawn unknown should fail");
    let formatted = format!("{}", err);
    assert!(
        formatted.contains("no procedure registered")
            || formatted.contains("no-such-registered-proc"),
        "got: {}",
        formatted
    );
}

#[test]
fn send_rejects_non_pid_argument() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", r#"(send 42 'msg)"#)
        .expect_err("send to a fixnum should fail");
    let formatted = format!("{}", err);
    assert!(
        formatted.contains("send") || formatted.contains("PID"),
        "got: {}",
        formatted
    );
}

#[test]
fn make_table_rejects_unknown_type() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", r#"(make-table 'unused-name 'bag)"#)
        .expect_err("bag is unsupported");
    let formatted = format!("{}", err);
    assert!(formatted.contains("unknown type"), "got: {}", formatted);
}

#[test]
fn self_outside_actor_errors() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", r#"(self)"#)
        .expect_err("self from top-level should fail");
    let formatted = format!("{}", err);
    assert!(
        formatted.contains("not inside an actor"),
        "got: {}",
        formatted
    );
}

#[test]
fn raw_receive_outside_actor_errors() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", r#"(raw-receive)"#)
        .expect_err("raw-receive from top-level should fail");
    let formatted = format!("{}", err);
    assert!(
        formatted.contains("not inside an actor"),
        "got: {}",
        formatted
    );
}

#[test]
fn raw_receive_bad_timeout_errors() {
    use cs_runtime::builtins::beam::beam_state;
    use std::sync::Arc;
    use std::sync::Mutex;

    // Register a proc that tries (raw-receive 'oops) inside an
    // actor body — the bad timeout should surface as an error.
    let err_msg: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let err_clone = err_msg.clone();
    beam_state().procs.register(
        "test:bad-timeout-actor",
        Arc::new(move |_actor, _args| {
            // The Scheme body runs in *this* thread (the actor's
            // tokio blocking worker). We don't have a Runtime in
            // scope here; for the purposes of *this* assertion we
            // can drive the Scheme builtin via the with_current_actor
            // path indirectly by constructing a transient Runtime
            // pinned to this thread. That mirrors the real path that
            // an actor body running compiled Scheme would follow.
            let mut rt = cs_runtime::Runtime::new();
            let r = rt.eval_str("<t>", r#"(raw-receive 'oops)"#);
            *err_clone.lock().unwrap() = Some(format!("{:?}", r.err()));
        }),
    );

    let _pid = cs_runtime::builtins::beam::primop_spawn("test:bad-timeout-actor", vec![]).unwrap();
    wait_until(
        std::time::Duration::from_secs(1),
        "bad-timeout actor never finished",
        || err_msg.lock().unwrap().is_some(),
    );
    let s = err_msg.lock().unwrap().clone().unwrap();
    assert!(s.contains("timeout") || s.contains("must be"), "got: {}", s);
}

#[test]
fn all_scheme_actor_body_self_send_receive() {
    use cs_runtime::builtins::beam::{beam_state, primop_send, SendableValue};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    // The actor body is entirely Scheme: it calls (self) to get
    // its PID, receives one message, and writes its own PID and
    // the received message into a shared slot so the test can
    // verify all three primops (self, raw-receive, and the
    // surrounding ActorContext install) work end-to-end.
    let result: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let result_clone = result.clone();
    beam_state().procs.register(
        "test:scheme-body-self-receive",
        Arc::new(move |_actor, _args| {
            // A fresh Runtime per spawn (each actor has its own
            // Heap, matching the spec's per-actor Heap model).
            let mut rt = cs_runtime::Runtime::new();
            let me = rt.eval_str("<t>", "(self)").expect("self inside actor");
            let msg = rt
                .eval_str("<t>", "(raw-receive)")
                .expect("raw-receive inside actor");
            let me_s = rt.format_value(&me, cs_core::WriteMode::Display);
            let msg_s = rt.format_value(&msg, cs_core::WriteMode::Display);
            *result_clone.lock().unwrap() = Some((me_s, msg_s));
        }),
    );

    let mut driver = Runtime::new();
    let pid_val = driver
        .eval_str("<t>", "(spawn 'test:scheme-body-self-receive)")
        .expect("spawn");
    let pid_display = disp(&driver, &pid_val);

    // Use the Rust-side primop_send to deliver — we already
    // covered the Scheme send path in another test, and this
    // keeps the assertion focused on (self) + (raw-receive).
    let target_pid = {
        // Re-parse the symbol's name into ActorPid via the same
        // logic the (send) builtin uses.
        let inner = pid_display
            .strip_prefix("<pid:<")
            .and_then(|s| s.strip_suffix(">>"))
            .unwrap();
        let (n, l) = inner.split_once('.').unwrap();
        cs_actor::ActorPid {
            node: n.parse().unwrap(),
            local_id: l.parse().unwrap(),
        }
    };
    primop_send(target_pid, SendableValue::Symbol("payload".into())).unwrap();

    wait_until(
        Duration::from_secs(2),
        "scheme actor body never finished",
        || result.lock().unwrap().is_some(),
    );
    let (me_s, msg_s) = result.lock().unwrap().clone().unwrap();
    assert_eq!(me_s, pid_display, "(self) should match the spawn'd pid");
    assert_eq!(msg_s, "payload");
}

// ----------------------------------------------------------
// cs-hotreload builtins
// ----------------------------------------------------------

#[test]
fn load_module_and_lookup_via_scheme() {
    let mut rt = Runtime::new();
    let epoch = rt
        .eval_str(
            "<t>",
            r#"(load-module! 'beam-hotreload-test-1
                 '(("init" . 0)
                   ("max"  . 100)))"#,
        )
        .expect("load-module!");
    assert_eq!(disp(&rt, &epoch), "1");

    let init = rt
        .eval_str("<t>", r#"(lookup-code 'beam-hotreload-test-1 "init")"#)
        .unwrap();
    assert_eq!(disp(&rt, &init), "0");

    let max = rt
        .eval_str("<t>", r#"(lookup-code 'beam-hotreload-test-1 "max")"#)
        .unwrap();
    assert_eq!(disp(&rt, &max), "100");

    let miss = rt
        .eval_str("<t>", r#"(lookup-code 'beam-hotreload-test-1 "absent")"#)
        .unwrap();
    assert_eq!(disp(&rt, &miss), "#f");
}

#[test]
fn reload_module_demotes_to_old() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-2 '(("v" . 1)))"#,
    )
    .unwrap();
    let epoch2 = rt
        .eval_str(
            "<t>",
            r#"(load-module! 'beam-hotreload-test-2 '(("v" . 2)))"#,
        )
        .unwrap();
    assert_eq!(disp(&rt, &epoch2), "2");

    let cur = rt
        .eval_str("<t>", r#"(lookup-code 'beam-hotreload-test-2 "v")"#)
        .unwrap();
    assert_eq!(disp(&rt, &cur), "2");

    let old = rt
        .eval_str("<t>", r#"(lookup-code-old 'beam-hotreload-test-2 "v")"#)
        .unwrap();
    assert_eq!(disp(&rt, &old), "1");

    let versions = rt
        .eval_str("<t>", r#"(code-versions 'beam-hotreload-test-2)"#)
        .unwrap();
    assert_eq!(disp(&rt, &versions), "(1 2)");
}

#[test]
fn code_versions_for_missing_module_returns_false() {
    let mut rt = Runtime::new();
    let v = rt
        .eval_str("<t>", r#"(code-versions 'beam-no-such-module)"#)
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn soft_purge_blocked_by_holder_count() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-3 '(("x" . 1)))"#,
    )
    .unwrap();
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-3 '(("x" . 2)))"#,
    )
    .unwrap();
    // 3 holders pinned to old version => soft_purge refuses.
    let err = rt
        .eval_str("<t>", r#"(code-soft-purge! 'beam-hotreload-test-3 3)"#)
        .expect_err("blocked purge should error");
    let formatted = format!("{}", err);
    assert!(formatted.contains("still pinned"), "got: {}", formatted);

    // 0 holders => soft_purge succeeds; old slot is None afterward.
    rt.eval_str("<t>", r#"(code-soft-purge! 'beam-hotreload-test-3 0)"#)
        .expect("purge with 0 holders");
    let old = rt
        .eval_str("<t>", r#"(lookup-code-old 'beam-hotreload-test-3 "x")"#)
        .unwrap();
    assert_eq!(disp(&rt, &old), "#f");
}

#[test]
fn force_purge_removes_old_unconditionally() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-4 '(("x" . 1)))"#,
    )
    .unwrap();
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-4 '(("x" . 2)))"#,
    )
    .unwrap();
    rt.eval_str("<t>", r#"(code-purge! 'beam-hotreload-test-4)"#)
        .unwrap();
    let old = rt
        .eval_str("<t>", r#"(lookup-code-old 'beam-hotreload-test-4 "x")"#)
        .unwrap();
    assert_eq!(disp(&rt, &old), "#f");
    let cur = rt
        .eval_str("<t>", r#"(lookup-code 'beam-hotreload-test-4 "x")"#)
        .unwrap();
    assert_eq!(disp(&rt, &cur), "2");
}

#[test]
fn unload_drops_both_versions() {
    let mut rt = Runtime::new();
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-5 '(("x" . 7)))"#,
    )
    .unwrap();
    rt.eval_str("<t>", r#"(code-unload! 'beam-hotreload-test-5)"#)
        .unwrap();
    let v = rt
        .eval_str("<t>", r#"(code-versions 'beam-hotreload-test-5)"#)
        .unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn load_module_accepts_nested_sendable_values() {
    let mut rt = Runtime::new();
    // Carry a pair-shaped value as an export — the boundary
    // walks SendableValue::Pair correctly.
    rt.eval_str(
        "<t>",
        r#"(load-module! 'beam-hotreload-test-6 '(("p" . (alpha . beta))))"#,
    )
    .unwrap();
    let p = rt
        .eval_str("<t>", r#"(lookup-code 'beam-hotreload-test-6 "p")"#)
        .unwrap();
    assert_eq!(disp(&rt, &p), "(alpha . beta)");
}

// ----------------------------------------------------------
// Reductions / cooperative yield (B3 first half)
// ----------------------------------------------------------

#[test]
fn yield_at_top_level_is_noop() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(yield)").expect("yield outside actor");
    assert_eq!(disp(&rt, &v), "#<unspecified>");
}

#[test]
fn reductions_in_actor_increment_and_yield_resets() {
    use cs_runtime::builtins::beam::beam_state;
    use std::sync::Arc;
    use std::sync::Mutex;

    let snapshots: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let snapshots_clone = snapshots.clone();

    beam_state().procs.register(
        "test:reductions-actor",
        Arc::new(move |_actor, _args| {
            let mut rt = cs_runtime::Runtime::new();

            // Initial count is zero.
            let r0 = rt.eval_str("<t>", "(reductions)").unwrap();
            snapshots_clone.lock().unwrap().push(disp_to_i64(&rt, &r0));

            // Bump twice.
            rt.eval_str("<t>", "(bump-reductions! 10)").unwrap();
            rt.eval_str("<t>", "(bump-reductions! 7)").unwrap();
            let r17 = rt.eval_str("<t>", "(reductions)").unwrap();
            snapshots_clone.lock().unwrap().push(disp_to_i64(&rt, &r17));

            // Yield resets to zero.
            rt.eval_str("<t>", "(yield)").unwrap();
            let r_after_yield = rt.eval_str("<t>", "(reductions)").unwrap();
            snapshots_clone
                .lock()
                .unwrap()
                .push(disp_to_i64(&rt, &r_after_yield));
        }),
    );

    let _pid =
        cs_runtime::builtins::beam::primop_spawn("test:reductions-actor", vec![]).expect("spawn");

    wait_until(
        std::time::Duration::from_secs(1),
        "reductions actor never finished",
        || snapshots.lock().unwrap().len() >= 3,
    );
    let snaps = snapshots.lock().unwrap().clone();
    assert_eq!(snaps, vec![0, 17, 0]);
}

#[test]
fn bump_reductions_rejects_negative() {
    let mut rt = Runtime::new();
    let err = rt
        .eval_str("<t>", "(bump-reductions! -3)")
        .expect_err("negative bump should fail");
    let formatted = format!("{}", err);
    assert!(formatted.contains("non-negative"), "got: {}", formatted);
}

#[test]
fn reductions_at_top_level_is_zero() {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", "(reductions)").unwrap();
    // At top level (no actor body), reductions is the
    // thread-local default 0.
    assert_eq!(disp(&rt, &v), "0");
}

/// Convert a Scheme value back to an i64 via the display path.
/// The integration tests print integers as decimal text — this
/// is the cheapest way to assert numeric equality without
/// exposing more of the cs_core::Number API to the test crate.
fn disp_to_i64(rt: &Runtime, v: &cs_core::Value) -> i64 {
    disp(rt, v).parse::<i64>().expect("integer-printing value")
}

#[test]
fn spawn_registered_proc_round_trip() {
    use cs_runtime::builtins::beam::{beam_state, primop_send, primop_spawn, SendableValue};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    // Register an echo proc via the Rust-side API (the bridge
    // from Scheme is multi-iter scope). Then call (spawn ...)
    // from Scheme and verify the returned PID symbol is valid
    // for (send ...).
    let received: Arc<Mutex<Option<SendableValue>>> = Arc::new(Mutex::new(None));
    let received_clone = received.clone();
    beam_state().procs.register(
        "test:scheme-spawn-echo",
        Arc::new(
            move |actor: &mut cs_actor::Actor, _args: Vec<SendableValue>| {
                if let Some(msg) = actor.receive() {
                    if let cs_actor::Message::User(payload) = msg {
                        if let Some(sv) = cs_runtime::builtins::beam::payload_to_sendable(&payload)
                        {
                            *received_clone.lock().unwrap() = Some(sv);
                        }
                    }
                }
            },
        ),
    );

    let mut rt = Runtime::new();
    let pid_val = rt
        .eval_str("<t>", r#"(spawn 'test:scheme-spawn-echo)"#)
        .expect("spawn from scheme");
    // The PID surfaces as a symbol formatted like "<pid:<0.N>>".
    let pid_display = disp(&rt, &pid_val);
    assert!(
        pid_display.starts_with("<pid:<"),
        "expected PID symbol, got {}",
        pid_display
    );

    // Build a (send <pid-symbol> 'hello) form and eval it.
    // The PID symbol contains '<' '.' '>' which the lexer treats as
    // special — use (string->symbol ...) so it parses cleanly.
    let send_src = format!(r#"(send (string->symbol "{}") 'hello)"#, pid_display);
    rt.eval_str("<t>", &send_src).expect("send to spawned pid");

    wait_until(
        Duration::from_secs(1),
        "echo never received via Scheme path",
        || received.lock().unwrap().is_some(),
    );
    assert_eq!(
        *received.lock().unwrap(),
        Some(SendableValue::Symbol("hello".into()))
    );
}
