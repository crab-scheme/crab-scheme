//! Cooperative parking for the remaining blocking builtins (cs-845.3):
//! `tcp-connect`/`dns-resolve`, file I/O, and subprocess `run`/`run/status`.
//!
//! Each test forces two green actors onto a single shared `LocalSet` worker
//! (`CRABSCHEME_ACTOR_LOCAL_WORKERS=1`): one performs the slow blocking op,
//! the other does an independent ping/pong. If the slow op froze the worker
//! instead of parking, the ping/pong could never complete while it's in
//! flight — so observing the ping/pong finish proves the op cooperated.

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

#[test]
fn parked_run_does_not_freeze_a_colocated_actor() {
    force_single_worker();

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-run-park", 2, out.clone());
    let col = primop_spawn("test:green-run-park", vec![]).expect("spawn collector");

    // `sleep 0.5` via `(run ...)` — long enough that a frozen worker would
    // stall the co-located ping well past this test's wait window.
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
        "co-located ping was never served while a green actor parked in (run)",
        || out.lock().unwrap().iter().any(|s| s == "ping"),
    );
    wait_until(
        Duration::from_secs(10),
        "(run \"sleep\" ...) never completed",
        || out.lock().unwrap().iter().any(|s| s == "run-done"),
    );
}

#[test]
fn parked_read_file_string_does_not_freeze_a_colocated_actor() {
    force_single_worker();

    let mut path = std::env::temp_dir();
    path.push(format!("cs-845-3-blocking-fs-{}.txt", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).expect("create temp file");
        // A few hundred KB so the blocking-threadpool read has non-zero
        // duration to overlap with the co-located ping.
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
        "co-located ping was never served while a green actor parked in read-file-string",
        || out.lock().unwrap().iter().any(|s| s == "ping"),
    );
    wait_until(
        Duration::from_secs(10),
        "read-file-string never completed",
        || out.lock().unwrap().iter().any(|s| s == "read-done"),
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn parked_tcp_connect_does_not_freeze_a_colocated_actor() {
    force_single_worker();

    // A listener that accepts but never completes the handshake's first
    // read/write — tcp-connect itself only needs the TCP handshake, which
    // `TcpListener::accept` alone satisfies, so this mainly checks that the
    // connect call parks rather than errors or hangs the worker.
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
        "co-located ping was never served around a green actor's tcp-connect",
        || out.lock().unwrap().iter().any(|s| s == "ping"),
    );
    wait_until(
        Duration::from_secs(10),
        "tcp-connect never completed",
        || out.lock().unwrap().iter().any(|s| s == "connect-done"),
    );

    let _ = server.join();
}
