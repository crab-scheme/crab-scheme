//! Cooperative parking for the remaining blocking builtins (cs-845.3):
//! `tcp-connect`/`dns-resolve`, file I/O, and subprocess `run`/`run/status`.
//!
//! Each test forces two green actors onto a single shared `LocalSet` worker
//! (`CRABSCHEME_ACTOR_LOCAL_WORKERS=1`): one performs the blocking op, the
//! other does an independent ping/pong.
//!
//! The `(run "sleep" ...)` test carries the discrimination: it asserts
//! ORDERING — the co-tenant's 'ping must be recorded BEFORE 'run-done.
//! With the cooperative hooks uninstalled (inline blocking), the slow-run
//! actor holds the worker for the whole 0.5s subprocess and 'run-done is
//! deterministically recorded first, so the assert fails. The fs and
//! tcp-connect tests are smoke-only: those ops are near-instant on a local
//! machine, so an ordering assert would be flaky — they just prove the
//! hooked ops still complete correctly from a green actor.

#![cfg(all(
    feature = "actor",
    feature = "stdlib-fs",
    feature = "stdlib-process",
    feature = "stdlib-net"
))]

use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_runtime::builtins::beam::{
    beam_state, primop_raw_receive, primop_send, primop_spawn, primop_spawn_source_green,
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

/// Spawn a green "ping" actor that immediately reports in — used as the
/// co-tenant witness that the shared worker is still servicing other actors
/// while the slow op is parked.
fn spawn_ping(col: cs_actor::ActorPid) {
    let body = r#"
        (define (ping)
          (let ((col (cdr (raw-receive))))
            (send col 'ping)))
        "#;
    let p = primop_spawn_source_green(body.to_string(), "ping".to_string(), vec![])
        .expect("spawn ping");
    primop_send(p, tagged("col", SendableValue::Pid(col))).unwrap();
}

/// The discriminating test: a green actor blocked ~0.5s in `(run "sleep" ...)`
/// must PARK (releasing the shared worker) so the co-located ping actor runs
/// and reports BEFORE the subprocess finishes. With the cooperative blocking
/// hooks uninstalled, the inline `wait_with_output` holds the worker for the
/// whole 0.5s and 'run-done is deterministically recorded first — this
/// ordering assert is what fails on the old inline-blocking path.
#[test]
fn parked_run_does_not_freeze_a_colocated_actor() {
    force_single_worker();

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-run-park", 2, out.clone());
    let col = primop_spawn("test:green-run-park", vec![]).expect("spawn collector");

    let body = r#"
        (define (slow-run)
          (let ((col (cdr (raw-receive))))
            (run "sleep" (list "0.5"))
            (send col 'run-done)))
        "#;
    let g = primop_spawn_source_green(body.to_string(), "slow-run".to_string(), vec![])
        .expect("spawn slow-run");
    primop_send(g, tagged("col", SendableValue::Pid(col))).unwrap();
    spawn_ping(col);

    wait_until(
        Duration::from_secs(10),
        "(run \"sleep\" ...) never completed",
        || out.lock().unwrap().iter().any(|s| s == "run-done"),
    );
    let markers = out.lock().unwrap().clone();
    let ping_idx = markers.iter().position(|s| s == "ping");
    let done_idx = markers.iter().position(|s| s == "run-done");
    assert!(
        ping_idx.is_some(),
        "co-located ping was never served: {markers:?}"
    );
    assert!(
        ping_idx < done_idx,
        "'ping must be recorded BEFORE 'run-done — inline (non-cooperative) \
         blocking in (run) would freeze the shared worker for the whole \
         subprocess and force run-done first; got {markers:?}"
    );
}

/// Smoke test only: a local file read is near-instant, so an ordering assert
/// against the ping would be flaky. This just proves `read-file-string` still
/// completes correctly from a green actor through the cooperative blocking
/// hook. The `(run ...)` test above carries the does-not-freeze discrimination.
#[test]
fn green_read_file_string_completes() {
    force_single_worker();

    let mut path = std::env::temp_dir();
    path.push(format!("cs-845-3-blocking-fs-{}.txt", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).expect("create temp file");
        let chunk = vec![b'x'; 1024];
        for _ in 0..512 {
            f.write_all(&chunk).expect("write temp file");
        }
    }
    let path_str = path.to_string_lossy().into_owned();

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-fs-park", 2, out.clone());
    let col = primop_spawn("test:green-fs-park", vec![]).expect("spawn collector");

    let body = format!(
        r#"
        (define (slow-read)
          (let ((col (cdr (raw-receive))))
            (read-file-string "{path_str}")
            (send col 'read-done)))
        "#
    );
    let g =
        primop_spawn_source_green(body, "slow-read".to_string(), vec![]).expect("spawn slow-read");
    primop_send(g, tagged("col", SendableValue::Pid(col))).unwrap();
    spawn_ping(col);

    wait_until(
        Duration::from_secs(10),
        "co-located ping was never served",
        || out.lock().unwrap().iter().any(|s| s == "ping"),
    );
    wait_until(
        Duration::from_secs(10),
        "read-file-string never completed",
        || out.lock().unwrap().iter().any(|s| s == "read-done"),
    );

    let _ = std::fs::remove_file(&path);
}

/// Smoke test only: a localhost TCP handshake is near-instant, so an ordering
/// assert against the ping would be flaky. This just proves `tcp-connect`
/// still completes correctly from a green actor through the cooperative
/// blocking hook. The `(run ...)` test above carries the does-not-freeze
/// discrimination.
#[test]
fn green_tcp_connect_completes() {
    force_single_worker();

    // tcp-connect only needs the TCP handshake, which `TcpListener::accept`
    // alone satisfies; the server just holds the socket open briefly.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (_sock, _) = listener.accept().expect("accept");
        std::thread::sleep(Duration::from_secs(2));
    });

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-connect-park", 2, out.clone());
    let col = primop_spawn("test:green-connect-park", vec![]).expect("spawn collector");

    let body = format!(
        r#"
        (define (slow-connect)
          (let ((col (cdr (raw-receive))))
            (tcp-connect "127.0.0.1" {port})
            (send col 'connect-done)))
        "#
    );
    let g = primop_spawn_source_green(body, "slow-connect".to_string(), vec![])
        .expect("spawn slow-connect");
    primop_send(g, tagged("col", SendableValue::Pid(col))).unwrap();
    spawn_ping(col);

    wait_until(
        Duration::from_secs(10),
        "co-located ping was never served",
        || out.lock().unwrap().iter().any(|s| s == "ping"),
    );
    wait_until(
        Duration::from_secs(10),
        "tcp-connect never completed",
        || out.lock().unwrap().iter().any(|s| s == "connect-done"),
    );

    let _ = server.join();
}
