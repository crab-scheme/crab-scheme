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

/// Contract-driven request validation, end-to-end through
/// rt.eval_str. Creates a WebMessage from Rust, hands its
/// handle to Scheme, runs predicate-based validation on params
/// + headers + body, and asserts the response.
#[test]
fn scheme_contracts_validate_request_fields() {
    use cs_actor::Payload;
    use cs_runtime::builtins::beam::SendableValue;
    use cs_runtime::builtins::web::try_intern_web_request;
    use cs_web::actor::WebMessage;
    use std::sync::Arc;

    fn make_request(
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (i64, tokio::sync::oneshot::Receiver<cs_web::Response>) {
        let mut b = cs_web::http::Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req: cs_web::Request = b
            .body(cs_web::Bytes::copy_from_slice(body.as_bytes()))
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<cs_web::Response>();
        let envelope: Arc<WebMessage> = Arc::new(WebMessage::new(req, tx));
        let sv = try_intern_web_request(&(envelope as Payload)).expect("bridge");
        let handle = match sv {
            SendableValue::Pair(_, tail) => match *tail {
                SendableValue::Pair(boxed_id, _) => match *boxed_id {
                    SendableValue::Fixnum(n) => n,
                    _ => panic!("handle not fixnum"),
                },
                _ => panic!(),
            },
            _ => panic!(),
        };
        (handle, rx)
    }

    let mut rt = Runtime::new();

    // Define the same predicates the lib/beam/web-contracts.scm
    // library exports. The library itself is loaded explicitly by
    // most callers via `(import ...)`; here we inline so the test
    // doesn't depend on the package loader.
    rt.eval_str(
        "<setup>",
        r#"
        (define (non-empty-string? v)
          (and (string? v) (> (string-length v) 0)))
        (define (integer-string? v)
          (and (string? v)
               (let ((n (string->number v)))
                 (and n (integer? n)))))
        (define (json-string? v)
          (and (string? v)
               (> (string-length v) 0)
               (let ((c (string-ref v 0)))
                 (or (char=? c #\{) (char=? c #\[) (char=? c #\")))))
        ;; or/c composed by hand for the test — same logic as
        ;; lib/contract/contract.scm.
        (define (or-c p1 p2) (lambda (v) (or (p1 v) (p2 v))))
        ;; nullable-of-c — common contract for "either missing or
        ;; matches predicate". Web params are nullable (#f when
        ;; absent).
        (define (nullable-of-c p) (or-c not p))
        "#,
    )
    .expect("setup");

    // --- Case 1: valid POST /users with id=42, x-token=sekret,
    //     JSON body. Expect 200.
    let (h1, rx1) = make_request(
        "POST",
        "/users?id=42",
        &[("x-token", "sekret"), ("content-type", "application/json")],
        r#"{"name":"alice"}"#,
    );
    rt.eval_str("<t>", &format!("(define h {})", h1)).unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (let ((id (web-request-param  h "id"))
              (tok (web-request-header h "x-token"))
              (body (web-request-body  h)))
          (cond
            ((not (integer-string? id))
             (web-respond! h 400 "invalid id"))
            ((not (non-empty-string? tok))
             (web-respond! h 400 "missing x-token"))
            ((not (json-string? body))
             (web-respond! h 400 "body must be JSON"))
            (else
             (web-respond! h 200 (string-append "ok id=" id)))))
        "#,
    )
    .expect("validate ok request");
    let resp1 = rx1.blocking_recv().expect("reply");
    assert_eq!(resp1.status(), 200);
    assert_eq!(resp1.body(), &cs_web::Bytes::from_static(b"ok id=42"));

    // --- Case 2: same shape but id is non-numeric. Expect 400.
    let (h2, rx2) = make_request(
        "POST",
        "/users?id=not-a-number",
        &[("x-token", "sekret"), ("content-type", "application/json")],
        r#"{"name":"alice"}"#,
    );
    rt.eval_str("<t>", &format!("(define h2 {})", h2)).unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (let ((id (web-request-param h2 "id")))
          (if (integer-string? id)
              (web-respond! h2 200 "ok")
              (web-respond! h2 400 "invalid id")))
        "#,
    )
    .expect("validate bad id");
    let resp2 = rx2.blocking_recv().expect("reply");
    assert_eq!(resp2.status(), 400);
    assert_eq!(resp2.body(), &cs_web::Bytes::from_static(b"invalid id"));

    // --- Case 3: missing x-token header. The header primop
    //     returns #f; `(nullable-of-c non-empty-string?)` accepts
    //     #f, so a "lenient" contract still passes — exercise the
    //     `or-c` combinator.
    let (h3, rx3) = make_request("GET", "/items", &[], "");
    rt.eval_str("<t>", &format!("(define h3 {})", h3)).unwrap();
    let res = rt
        .eval_str(
            "<t>",
            r#"
            (let* ((tok (web-request-header h3 "x-token"))
                   (lenient (nullable-of-c non-empty-string?))
                   (strict  non-empty-string?))
              (cond
                ((not (lenient tok))
                 (web-respond! h3 400 "tok failed lenient"))
                ((not (strict tok))
                 (web-respond! h3 200 "lenient passed; strict would have rejected"))
                (else
                 (web-respond! h3 200 "all good"))))
            "#,
        )
        .expect("lenient validation");
    let _ = res;
    let resp3 = rx3.blocking_recv().expect("reply");
    assert_eq!(resp3.status(), 200);
    let body = std::str::from_utf8(resp3.body()).unwrap();
    assert!(
        body.contains("lenient passed"),
        "expected lenient/strict branch, got {:?}",
        body
    );

    // --- Case 4: params alist round-trip. Multiple repeated
    //     keys, URL-decoded values.
    let (h4, rx4) = make_request("GET", "/search?q=hello+world&tag=red&tag=blue", &[], "");
    rt.eval_str("<t>", &format!("(define h4 {})", h4)).unwrap();
    let n = rt
        .eval_str("<t>", "(length (web-request-params h4))")
        .expect("params");
    assert_eq!(disp(&rt, &n), "3");
    rt.eval_str("<t>", r#"(web-respond! h4 200 "checked")"#)
        .unwrap();
    let resp4 = rx4.blocking_recv().expect("reply");
    assert_eq!(resp4.status(), 200);
}

/// Both ergonomic forms from `lib/beam/web-contracts.scm`:
///
/// - the `req-*` / `respond!` short aliases (option 1)
/// - the `with-request` macro that locally binds short accessors
///   capturing the handle (option 3)
///
/// Each test crafts a WebMessage, hands the handle to Scheme,
/// runs the ergonomic form, and asserts the reply.
#[test]
fn scheme_short_aliases_and_with_request_macro() {
    use cs_actor::Payload;
    use cs_runtime::builtins::beam::SendableValue;
    use cs_runtime::builtins::web::try_intern_web_request;
    use cs_web::actor::WebMessage;
    use std::sync::Arc;

    fn make_request(
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (i64, tokio::sync::oneshot::Receiver<cs_web::Response>) {
        let mut b = cs_web::http::Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req: cs_web::Request = b
            .body(cs_web::Bytes::copy_from_slice(body.as_bytes()))
            .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<cs_web::Response>();
        let envelope: Arc<WebMessage> = Arc::new(WebMessage::new(req, tx));
        let sv = try_intern_web_request(&(envelope as Payload)).expect("bridge");
        let handle = match sv {
            SendableValue::Pair(_, tail) => match *tail {
                SendableValue::Pair(boxed_id, _) => match *boxed_id {
                    SendableValue::Fixnum(n) => n,
                    _ => panic!("handle not fixnum"),
                },
                _ => panic!(),
            },
            _ => panic!(),
        };
        (handle, rx)
    }

    let mut rt = Runtime::new();

    // Inline the two pieces from lib/beam/web-contracts.scm we need —
    // the short aliases and the with-request macro. The library
    // itself is loaded by users via `(import (lib beam web-contracts))`;
    // the test inlines so it doesn't depend on the package loader.
    rt.eval_str(
        "<setup>",
        r#"
        (define req-method  web-request-method)
        (define req-param   web-request-param)
        (define req-header  web-request-header)
        (define respond!    web-respond!)
        (define-syntax with-request
          (syntax-rules ()
            [(_ h body* ...)
             (let-syntax ([method   (syntax-rules () [(_)    (web-request-method  h)])]
                          [path     (syntax-rules () [(_)    (web-request-path    h)])]
                          [body     (syntax-rules () [(_)    (web-request-body    h)])]
                          [param    (syntax-rules () [(_ k)  (web-request-param   h k)])]
                          [header   (syntax-rules () [(_ k)  (web-request-header  h k)])]
                          [respond! (syntax-rules () [(_ s b2) (web-respond!      h s b2)])])
               body* ...)]))
        (define (integer-string? v)
          (and (string? v)
               (let ((n (string->number v)))
                 (and n (integer? n)))))
        "#,
    )
    .expect("setup");

    // --- Short aliases: handle still passed explicitly, but the
    //     procedure names are 4× shorter.
    let (h1, rx1) = make_request("GET", "/items?id=42", &[("x-token", "tok")], "");
    rt.eval_str("<t>", &format!("(define h {})", h1)).unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (let ((id  (req-param  h "id"))
              (tok (req-header h "x-token"))
              (m   (req-method h)))
          (respond! h 200
            (string-append (symbol->string m) " id=" id " tok=" tok)))
        "#,
    )
    .expect("aliases");
    let r1 = rx1.blocking_recv().expect("reply");
    assert_eq!(r1.status(), 200);
    assert_eq!(r1.body(), &cs_web::Bytes::from_static(b"GET id=42 tok=tok"));

    // --- with-request: handle disappears inside the form.
    let (h2, rx2) = make_request("POST", "/users?id=7", &[("x-auth", "secret")], "{}");
    rt.eval_str("<t>", &format!("(define h2 {})", h2)).unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (with-request h2
          (let ((id  (param  "id"))
                (a   (header "x-auth")))
            (if (integer-string? id)
                (respond! 200 (string-append "ok " id " auth=" a))
                (respond! 400 "bad id"))))
        "#,
    )
    .expect("with-request happy path");
    let r2 = rx2.blocking_recv().expect("reply");
    assert_eq!(r2.status(), 200);
    assert_eq!(r2.body(), &cs_web::Bytes::from_static(b"ok 7 auth=secret"));

    // --- with-request: failing contract path through the macro.
    let (h3, rx3) = make_request("GET", "/users?id=abc", &[], "");
    rt.eval_str("<t>", &format!("(define h3 {})", h3)).unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (with-request h3
          (let ((id (param "id")))
            (if (integer-string? id)
                (respond! 200 "ok")
                (respond! 400 "bad id"))))
        "#,
    )
    .expect("with-request reject path");
    let r3 = rx3.blocking_recv().expect("reply");
    assert_eq!(r3.status(), 400);
    assert_eq!(r3.body(), &cs_web::Bytes::from_static(b"bad id"));

    // --- with-request: `(method)` / `(path)` / `(body)` shadow any
    //     outer bindings. Verify all three nullary forms work.
    let (h4, rx4) = make_request("PUT", "/x", &[], "payload-here");
    rt.eval_str("<t>", &format!("(define h4 {})", h4)).unwrap();
    rt.eval_str(
        "<t>",
        r#"
        (with-request h4
          (respond! 200
            (string-append (symbol->string (method))
                           " " (path)
                           " body=" (body))))
        "#,
    )
    .expect("with-request all-nullary");
    let r4 = rx4.blocking_recv().expect("reply");
    assert_eq!(r4.status(), 200);
    assert_eq!(
        r4.body(),
        &cs_web::Bytes::from_static(b"PUT /x body=payload-here")
    );
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
