//! End-to-end tests for the BEAM-style builtins as exposed to
//! Scheme via the walker / VM tiers. See
//! `crates/cs-runtime/src/builtins/beam.rs`.

#![cfg(feature = "actor")]

use cs_core::WriteMode;
use cs_runtime::Runtime;

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

    // Wait for the echo to land.
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while received.lock().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            // Sanity: confirm the spawn primop is wired by going
            // directly through it so the test fails for a
            // meaningful reason if the Scheme path silently
            // dropped the send.
            let pid = primop_spawn("test:scheme-spawn-echo", vec![]).unwrap();
            primop_send(pid, SendableValue::Symbol("backup".into())).unwrap();
            panic!("echo never received via Scheme path");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        *received.lock().unwrap(),
        Some(SendableValue::Symbol("hello".into()))
    );
}
