//! Scheme-level acceptance tests for the cs-web primops.
//!
//! Drives `(web-server-create ...)`, `(web-route-static! ...)`,
//! `(web-server-start ...)`, `(web-server-stop ...)` via the
//! walker through the actual cs-runtime — proving the wiring
//! between Scheme and cs-web works the way users will see it.

#![cfg(feature = "web")]

use std::io::{Read, Write};
use std::time::Duration;

use cs_core::{Value, WriteMode};
use cs_runtime::Runtime;

fn disp(rt: &Runtime, v: &Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

/// Send a primitive HTTP/1.1 GET and return (status, body).
fn http_get(addr: &str, path: &str) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        path
    )
    .unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    let raw = String::from_utf8_lossy(&buf);
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body_start = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(buf.len());
    let body = String::from_utf8_lossy(&buf[body_start..]).to_string();
    (status, body)
}

/// Pull the bound address back out of the Scheme string result.
fn parse_addr(rt: &Runtime, v: &Value) -> String {
    disp(rt, v)
}

#[test]
fn scheme_starts_serves_and_stops_static_route() {
    let mut rt = Runtime::new();
    // Register the route while building.
    let sid_val = rt
        .eval_str("<t>", r#"(web-server-create "127.0.0.1:0")"#)
        .expect("create");
    let sid = disp(&rt, &sid_val);
    let route_expr = format!(
        r#"(web-route-static! {} 'GET "/scheme-hello" "hello from scheme")"#,
        sid
    );
    rt.eval_str("<t>", &route_expr).expect("route");
    let bound_val = rt
        .eval_str("<t>", &format!("(web-server-start {})", sid))
        .expect("start");
    let addr = parse_addr(&rt, &bound_val);

    // Settle the accept loop, then GET.
    std::thread::sleep(Duration::from_millis(50));
    let (status, body) = http_get(&addr, "/scheme-hello");
    assert_eq!(status, 200);
    assert_eq!(body, "hello from scheme");

    rt.eval_str("<t>", &format!("(web-server-stop {})", sid))
        .expect("stop");
}

#[test]
fn scheme_custom_status_route() {
    let mut rt = Runtime::new();
    let sid_val = rt
        .eval_str("<t>", r#"(web-server-create "127.0.0.1:0")"#)
        .expect("create");
    let sid = disp(&rt, &sid_val);
    rt.eval_str(
        "<t>",
        &format!(r#"(web-route-static! {} 'GET "/teapot" "tea" 418)"#, sid),
    )
    .expect("route");
    let bound_val = rt
        .eval_str("<t>", &format!("(web-server-start {})", sid))
        .expect("start");
    let addr = parse_addr(&rt, &bound_val);

    std::thread::sleep(Duration::from_millis(50));
    let (status, body) = http_get(&addr, "/teapot");
    assert_eq!(status, 418);
    assert_eq!(body, "tea");

    rt.eval_str("<t>", &format!("(web-server-stop {})", sid))
        .expect("stop");
}

#[test]
fn scheme_access_log_visible_via_table_primops() {
    let mut rt = Runtime::new();
    let sid_val = rt
        .eval_str("<t>", r#"(web-server-create "127.0.0.1:0")"#)
        .expect("create");
    let sid = disp(&rt, &sid_val);
    rt.eval_str(
        "<t>",
        &format!(r#"(web-route-static! {} 'GET "/a" "a")"#, sid),
    )
    .expect("route");
    rt.eval_str(
        "<t>",
        &format!(r#"(web-access-log! {} "scheme-access")"#, sid),
    )
    .expect("access-log");

    let bound_val = rt
        .eval_str("<t>", &format!("(web-server-start {})", sid))
        .expect("start");
    let addr = parse_addr(&rt, &bound_val);

    std::thread::sleep(Duration::from_millis(50));
    let _ = http_get(&addr, "/a");
    let _ = http_get(&addr, "/a");
    let _ = http_get(&addr, "/missing");
    // Give the access-log writes time to commit before we stop
    // (some run synchronously inside the connection handler, but
    // a small slack helps).
    std::thread::sleep(Duration::from_millis(50));
    rt.eval_str("<t>", &format!("(web-server-stop {})", sid))
        .expect("stop");

    // The access log lives in cs-web's own table registry, not
    // cs-runtime's `make-table` registry — they're separate
    // fabrics. The runtime test just verifies the primop didn't
    // error; cross-registry visibility from Scheme requires
    // exposing the cs-web table registry through `make-table`,
    // which is a follow-up.
    // Sanity: server stopped cleanly.
}

#[test]
fn scheme_404_for_unknown_route() {
    let mut rt = Runtime::new();
    let sid_val = rt
        .eval_str("<t>", r#"(web-server-create "127.0.0.1:0")"#)
        .expect("create");
    let sid = disp(&rt, &sid_val);
    rt.eval_str(
        "<t>",
        &format!(r#"(web-route-static! {} 'GET "/known" "hi")"#, sid),
    )
    .expect("route");
    let bound_val = rt
        .eval_str("<t>", &format!("(web-server-start {})", sid))
        .expect("start");
    let addr = parse_addr(&rt, &bound_val);

    std::thread::sleep(Duration::from_millis(50));
    let (status, _) = http_get(&addr, "/nope");
    assert_eq!(status, 404);

    rt.eval_str("<t>", &format!("(web-server-stop {})", sid))
        .expect("stop");
}

/// End-to-end: an actor body uses the bridge-driven primops to
/// serve a real HTTP request. The body is registered in beam's
/// procedure registry from Rust because there's no Scheme-facing
/// `(register-actor-body! ...)` primop today (separate from the
/// payload-bridge fix), but the body uses the SAME primops a
/// pure-Scheme actor would call — `(web-request-method req)`,
/// `(web-request-path req)`, `(web-respond! req status body)` —
/// just invoked through their Rust shapes. Proves the bridge,
/// the request slab, the inspector primops, and the response
/// path are all wired correctly.
#[test]
fn actor_handles_request_via_bridged_message() {
    use cs_actor::Actor;
    use cs_runtime::builtins::beam::{beam_state, primop_raw_receive, ActorEntry, SendableValue};
    use std::sync::Arc;

    // Body: receive one ('*web-request* handle), respond with the
    // request path uppercased.
    let body: ActorEntry = Arc::new(|actor: &mut Actor, _args: Vec<SendableValue>| {
        let msg = match primop_raw_receive(actor, Some(2000)) {
            Ok(Some(m)) => m,
            other => {
                eprintln!("actor: unexpected receive result {:?}", other);
                return;
            }
        };
        // Decode ('*web-request* <handle>).
        let handle = match msg {
            SendableValue::Pair(head, tail) => match (*head, *tail) {
                (SendableValue::Symbol(s), SendableValue::Pair(boxed_id, boxed_nil))
                    if s == "*web-request*" && matches!(*boxed_nil, SendableValue::Null) =>
                {
                    match *boxed_id {
                        SendableValue::Fixnum(n) => n,
                        _ => return,
                    }
                }
                _ => return,
            },
            _ => return,
        };
        let path = cs_runtime::builtins::web::primop_request_path(handle).unwrap_or_default();
        let _ = cs_runtime::builtins::web::primop_respond(handle, 200, path.to_uppercase());
    });
    beam_state().procs.register("test:web-uppercaser", body);

    let mut rt = Runtime::new();

    // Spawn the actor. PID comes back as a quoted symbol.
    let pid_val = rt
        .eval_str("<t>", r#"(spawn 'test:web-uppercaser)"#)
        .expect("spawn");
    let pid_str = disp(&rt, &pid_val);

    // Set up the server with that actor as the GET /scheme-handler
    // route handler.
    let sid_val = rt
        .eval_str("<t>", r#"(web-server-create "127.0.0.1:0")"#)
        .expect("create");
    let sid = disp(&rt, &sid_val);
    let route_expr = format!(
        "(web-route-actor! {} 'GET \"/scheme-handler\" '{} 2000)",
        sid, pid_str
    );
    rt.eval_str("<t>", &route_expr).expect("route");
    let bound_val = rt
        .eval_str("<t>", &format!("(web-server-start {})", sid))
        .expect("start");
    let addr = parse_addr(&rt, &bound_val);

    std::thread::sleep(Duration::from_millis(50));
    let (status, body_resp) = http_get(&addr, "/scheme-handler");
    assert_eq!(status, 200);
    assert_eq!(body_resp, "/SCHEME-HANDLER");

    rt.eval_str("<t>", &format!("(web-server-stop {})", sid))
        .expect("stop");
}

#[test]
fn scheme_route_after_start_is_error() {
    let mut rt = Runtime::new();
    let sid_val = rt
        .eval_str("<t>", r#"(web-server-create "127.0.0.1:0")"#)
        .expect("create");
    let sid = disp(&rt, &sid_val);
    rt.eval_str(
        "<t>",
        &format!(r#"(web-route-static! {} 'GET "/x" "y")"#, sid),
    )
    .expect("route");
    rt.eval_str("<t>", &format!("(web-server-start {})", sid))
        .expect("start");

    // Adding a route after start must error.
    let res = rt.eval_str(
        "<t>",
        &format!(r#"(web-route-static! {} 'GET "/late" "no")"#, sid),
    );
    assert!(res.is_err(), "expected error registering after start");

    rt.eval_str("<t>", &format!("(web-server-stop {})", sid))
        .expect("stop");
}
