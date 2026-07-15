//! Cooperative `tcp-recv` / `tcp-send` on the whole-body green path.
//!
//! A green `spawn-source-green` actor that blocks on `(tcp-recv)` must **park**
//! (releasing its LocalSet worker for co-located actors) rather than freezing
//! the worker — and the read/write must still round-trip correctly over a real
//! socket. `(tcp-connect)` itself is not cooperative (instant on localhost), only
//! the read/write of an established connection are (the cache's hot path).

#![cfg(all(feature = "actor", feature = "stdlib-net"))]

use std::io::{Read, Write};
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
                    Ok(Some(SendableValue::SymbolId { name, .. })) => {
                        out.lock().unwrap().push(name)
                    }
                    Ok(Some(other)) => out.lock().unwrap().push(format!("{other:?}")),
                    _ => break,
                }
            }
        }),
    );
}

#[test]
fn green_conn_round_trips_over_a_real_socket() {
    force_single_worker();

    // A std server: accept one connection, send "ping", read the reply.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        sock.write_all(b"ping").expect("server write");
        let mut buf = [0u8; 64];
        let n = sock.read(&mut buf).expect("server read");
        String::from_utf8_lossy(&buf[..n]).to_string()
    });

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-tcp-rt", 1, out.clone());
    let col = primop_spawn("test:green-tcp-rt", vec![]).expect("spawn collector");

    // Green conn: connect, cooperatively recv "ping", cooperatively send "pong".
    let body = format!(
        r#"
        (define (conn)
          (let ((col (cdr (raw-receive))))
            (let ((h (tcp-connect "127.0.0.1" {port})))
              (let ((req (tcp-recv h 64)))
                (if (string=? (utf8->string req) "ping")
                    (begin (tcp-send h (string->utf8 "pong")) (send col 'recv-ok))
                    (send col 'recv-bad))))))
        "#
    );
    let g = primop_spawn_source_green(body, "conn".to_string(), vec![]).expect("spawn green conn");
    primop_send(g, tagged("col", SendableValue::Pid(col))).unwrap();

    wait_until(
        Duration::from_secs(10),
        "green conn never round-tripped",
        || !out.lock().unwrap().is_empty(),
    );
    assert_eq!(
        out.lock().unwrap()[0],
        "recv-ok",
        "green conn should have read \"ping\""
    );
    let reply = server.join().expect("server thread");
    assert_eq!(
        reply, "pong",
        "server should have read the green conn's cooperative send"
    );
}

#[test]
fn parked_tcp_recv_does_not_freeze_a_colocated_actor() {
    force_single_worker();

    // Two green conns share the single worker. A connects to a server that NEVER
    // sends, so A parks indefinitely in (tcp-recv). B connects to a server that
    // sends immediately. If A's parked recv blocked the worker, B could never be
    // serviced; cooperative parking lets B complete promptly.
    let la = TcpListener::bind("127.0.0.1:0").expect("bind A");
    let pa = la.local_addr().unwrap().port();
    let lb = TcpListener::bind("127.0.0.1:0").expect("bind B");
    let pb = lb.local_addr().unwrap().port();

    let out = Arc::new(Mutex::new(Vec::<String>::new()));
    register_markers("test:green-tcp-park", 1, out.clone());
    let col = primop_spawn("test:green-tcp-park", vec![]).expect("spawn collector");

    // Server A: accept and hold the connection open, never sending.
    let server_a = std::thread::spawn(move || {
        let (_sock, _) = la.accept().expect("accept A");
        std::thread::sleep(Duration::from_secs(3)); // hold the socket past the assert
    });
    // Server B: accept and send "go".
    let server_b = std::thread::spawn(move || {
        let (mut sock, _) = lb.accept().expect("accept B");
        sock.write_all(b"go").expect("server B write");
    });

    let conn_body = |port: u16, marker: &str| {
        format!(
            r#"
            (define (conn)
              (let ((col (cdr (raw-receive))))
                (let ((h (tcp-connect "127.0.0.1" {port})))
                  (let ((m (tcp-recv h 64)))
                    (if (> (bytevector-length m) 0) (send col '{marker}) (send col 'empty))))))
            "#
        )
    };

    let a = primop_spawn_source_green(conn_body(pa, "a-served"), "conn".to_string(), vec![])
        .expect("spawn A");
    primop_send(a, tagged("col", SendableValue::Pid(col))).unwrap();
    let b = primop_spawn_source_green(conn_body(pb, "b-served"), "conn".to_string(), vec![])
        .expect("spawn B");
    primop_send(b, tagged("col", SendableValue::Pid(col))).unwrap();

    // B must be served while A is parked in its read — proving A released the
    // shared worker. (A never gets data, so it would never report 'a-served.)
    wait_until(
        Duration::from_secs(10),
        "co-located B was never served while A parked in tcp-recv",
        || out.lock().unwrap().iter().any(|s| s == "b-served"),
    );
    server_b.join().expect("server B thread");
    let _ = server_a.join();
}
