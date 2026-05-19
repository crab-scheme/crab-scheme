//! Tower-style web server primops exposed to Scheme. Behind the
//! `web` feature.
//!
//! Surface (all in the default top-level environment when the
//! feature is enabled):
//!
//! ```ignore
//! ; Returns an integer server handle.
//! (web-server-create "127.0.0.1:8080") => 1
//!
//! ; Register a static route. Status defaults to 200. Method is
//! ; a symbol; path is a string.
//! (web-route-static! sid 'GET "/health" "ok")
//! (web-route-static! sid 'GET "/teapot" "tea" 418)
//!
//! ; Graft a cdylib plugin's routes onto the server.
//! (web-route-module! sid "/path/to/libplugin.dylib")
//!
//! ; Install an access-log layer writing to a cs-table OrderedSet
//! ; (created if absent). The table is inspectable via the
//! ; existing (table-fold ...) primops.
//! (web-access-log! sid "access")
//!
//! ; Start serving in a background tokio task. Returns
//! ; immediately. Subsequent web-route-static!/web-access-log!
//! ; calls are NOT honored after start — register first.
//! (web-server-start sid)
//!
//! ; Stop the server. Idempotent.
//! (web-server-stop sid)
//! ```
//!
//! Scope: the v1 surface supports static-body routes and module
//! plugins. Scheme-defined dynamic handlers need a Value-typed
//! payload bridge for `cs_web::actor::WebMessage` — that lives in
//! cs-actor's payload layer and is a follow-up. Today's
//! workaround: write the dynamic handler in Rust (a module
//! plugin) and mount it via `web-route-module!`.

#![cfg(feature = "web")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use cs_core::{SymbolTable, Value};

use cs_web::handler::service_fn;
use cs_web::{response, ArcService, Method, Router, ServerConfig, StatusCode};

// ---------------------------------------------------------------
// Slab: global registry of in-flight server builders / handles.
// ---------------------------------------------------------------

/// One slot in the server registry. Servers progress through two
/// states: `Building` (mutable while the user registers routes,
/// owns a Router and a list of layers) and `Running` (immutable,
/// owns the tokio JoinHandle so `web-server-stop` can abort).
enum Slot {
    Building {
        addr: SocketAddr,
        router: Router,
        access_log: Option<cs_web::table::AccessLog>,
    },
    Running {
        shutdown_tx: tokio::sync::oneshot::Sender<()>,
        join: tokio::task::JoinHandle<()>,
    },
    // Closed slots stay registered so a double-stop is a no-op
    // rather than a `slot not found` error.
    Stopped,
}

struct Registry {
    next_id: i64,
    slots: HashMap<i64, Slot>,
    /// Per-table fabric for access-log targets. Servers that
    /// install an access log share this so a Scheme caller can
    /// later read it with `(table-fold ...)`.
    tables: cs_table::TableRegistry,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next_id: 1,
            slots: HashMap::new(),
            tables: cs_table::TableRegistry::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().expect("cs-web server registry poisoned")
}

// ---------------------------------------------------------------
// Background tokio runtime. cs-web servers need to run on tokio,
// but cs-runtime itself is `!Send` — the Scheme caller can't sit
// on a tokio task. We own a dedicated multi-thread runtime that
// hosts all cs-web servers across a single CrabScheme process.
// ---------------------------------------------------------------

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("cs-web")
            .build()
            .expect("build cs-web tokio runtime")
    })
}

// ---------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------

fn check_arity(who: &str, args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        let noun = if expected == 1 {
            "argument"
        } else {
            "arguments"
        };
        Err(format!(
            "{}: expected {} {}, got {}",
            who,
            expected,
            noun,
            args.len()
        ))
    }
}

fn value_to_str(v: &Value, syms: &SymbolTable, who: &str) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(syms.name(*s).to_string()),
        Value::String(g) => Ok(g.borrow().clone()),
        other => Err(format!(
            "{}: expected symbol or string, got {}",
            who,
            other.type_name()
        )),
    }
}

fn value_to_i64(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Number(cs_core::Number::Fixnum(n)) => Ok(*n),
        other => Err(format!(
            "{}: expected fixnum, got {}",
            who,
            other.type_name()
        )),
    }
}

fn value_to_u16(v: &Value, who: &str) -> Result<u16, String> {
    let n = value_to_i64(v, who)?;
    if !(0..=u16::MAX as i64).contains(&n) {
        return Err(format!("{}: status {} out of range 0..65535", who, n));
    }
    Ok(n as u16)
}

fn method_from_symbol(s: &str) -> Result<Method, String> {
    match s.to_ascii_uppercase().as_str() {
        "GET" => Ok(Method::GET),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "DELETE" => Ok(Method::DELETE),
        "PATCH" => Ok(Method::PATCH),
        "HEAD" => Ok(Method::HEAD),
        "OPTIONS" => Ok(Method::OPTIONS),
        other => Err(format!("unknown HTTP method '{}'", other)),
    }
}

fn with_slot<R>(
    who: &str,
    sid: i64,
    f: impl FnOnce(&mut Slot) -> Result<R, String>,
) -> Result<R, String> {
    let mut reg = lock();
    let slot = reg
        .slots
        .get_mut(&sid)
        .ok_or_else(|| format!("{}: server #{} not found", who, sid))?;
    f(slot)
}

fn static_service(status: u16, body: String) -> ArcService {
    let body: cs_web::Bytes = body.into();
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    service_fn(move |_| {
        let body = body.clone();
        async move { response(status, body) }
    })
}

// ---------------------------------------------------------------
// Primops (host-fn shape)
// ---------------------------------------------------------------

fn primop_server_create(addr: &str) -> Result<i64, String> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| format!("web-server-create: invalid addr {:?}: {}", addr, e))?;
    let mut reg = lock();
    let id = reg.next_id;
    reg.next_id += 1;
    reg.slots.insert(
        id,
        Slot::Building {
            addr,
            router: Router::new(),
            access_log: None,
        },
    );
    Ok(id)
}

fn primop_route_static(
    sid: i64,
    method: Method,
    path: String,
    body: String,
    status: u16,
) -> Result<(), String> {
    with_slot("web-route-static!", sid, |slot| match slot {
        Slot::Building { router, .. } => {
            // Take + put — `route` consumes the router by value.
            let r = std::mem::replace(router, Router::new());
            *router = r.route(method, &path, static_service(status, body));
            Ok(())
        }
        _ => Err(format!(
            "web-route-static!: server #{} already started or stopped",
            sid
        )),
    })
}

#[cfg(feature = "web-modules")]
fn primop_route_module(sid: i64, path: String) -> Result<usize, String> {
    let module = unsafe { cs_web::Module::load(&path) }
        .map_err(|e| format!("web-route-module!: load {}: {}", path, e))?;
    let mut sink = cs_web::RouteSink::new();
    module.register_into(&mut sink);
    let n = sink.len();
    with_slot("web-route-module!", sid, |slot| match slot {
        Slot::Building { router, .. } => {
            let r = std::mem::replace(router, Router::new());
            *router = r.add_sink(sink);
            // We deliberately leak the Module here so the
            // dylib stays mapped for the server's lifetime —
            // routes hold fn pointers into it. A Drop hook on
            // Stopped would be the principled fix; the leak is
            // acceptable because users never reload modules in
            // typical web-server scenarios.
            std::mem::forget(module);
            Ok(n)
        }
        _ => Err(format!(
            "web-route-module!: server #{} already started or stopped",
            sid
        )),
    })
}

fn primop_access_log(sid: i64, table_name: &str) -> Result<(), String> {
    let tables = lock().tables.clone();
    let log = cs_web::table::AccessLog::new(tables, table_name)
        .map_err(|e| format!("web-access-log!: {}", e))?;
    with_slot("web-access-log!", sid, |slot| match slot {
        Slot::Building { access_log, .. } => {
            *access_log = Some(log);
            Ok(())
        }
        _ => Err(format!(
            "web-access-log!: server #{} already started or stopped",
            sid
        )),
    })
}

fn primop_server_start(sid: i64) -> Result<String, String> {
    // Take the Building state out of the slot so we can move the
    // Router into the tokio task. Bind synchronously so the
    // Scheme caller sees errors immediately rather than from a
    // background task it can't observe.
    let (addr, service) = {
        let mut reg = lock();
        let slot = reg
            .slots
            .get_mut(&sid)
            .ok_or_else(|| format!("web-server-start: server #{} not found", sid))?;
        let Slot::Building {
            addr,
            router,
            access_log,
        } = std::mem::replace(slot, Slot::Stopped)
        else {
            // Restore and report.
            return Err(format!(
                "web-server-start: server #{} already started or stopped",
                sid
            ));
        };
        let mut service: ArcService = router.into_service();
        if let Some(log) = access_log {
            service = cs_web::Stack::new().push(log).wrap(service);
        }
        // Always wrap with CatchPanic so a panicking handler
        // never crashes the connection task.
        service = cs_web::Stack::new().push(cs_web::CatchPanic).wrap(service);
        (addr, service)
    };

    let cfg = ServerConfig {
        addr,
        request_timeout: None,
    };
    let (listener, bound) = rt()
        .block_on(async { cs_web::bind(&cfg).await })
        .map_err(|e| format!("web-server-start: bind: {}", e))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let join = rt().spawn(async move {
        let _ = cs_web::serve(
            listener,
            service,
            Some(async move {
                let _ = shutdown_rx.await;
            }),
        )
        .await;
    });

    lock()
        .slots
        .insert(sid, Slot::Running { shutdown_tx, join });
    Ok(bound.to_string())
}

fn primop_server_stop(sid: i64) -> Result<(), String> {
    let mut reg = lock();
    let slot = reg
        .slots
        .get_mut(&sid)
        .ok_or_else(|| format!("web-server-stop: server #{} not found", sid))?;
    match std::mem::replace(slot, Slot::Stopped) {
        Slot::Running { shutdown_tx, join } => {
            // Signal shutdown; ignore send error (receiver already
            // gone means the server task already finished).
            let _ = shutdown_tx.send(());
            join.abort();
            Ok(())
        }
        Slot::Building { .. } | Slot::Stopped => Ok(()),
    }
}

// ---------------------------------------------------------------
// Scheme glue (host-fn shape `fn(&[Value], &mut SymbolTable) -> ...`).
// ---------------------------------------------------------------

pub fn b_web_server_create(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-server-create", args, 1)?;
    let addr = value_to_str(&args[0], syms, "web-server-create")?;
    let id = primop_server_create(&addr)?;
    Ok(Value::Number(cs_core::Number::Fixnum(id)))
}

pub fn b_web_route_static(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 && args.len() != 5 {
        return Err(format!(
            "web-route-static!: expected 4 or 5 arguments, got {}",
            args.len()
        ));
    }
    let sid = value_to_i64(&args[0], "web-route-static!")?;
    let method_name = value_to_str(&args[1], syms, "web-route-static!")?;
    let method =
        method_from_symbol(&method_name).map_err(|e| format!("web-route-static!: {}", e))?;
    let path = value_to_str(&args[2], syms, "web-route-static!")?;
    let body = value_to_str(&args[3], syms, "web-route-static!")?;
    let status = if args.len() == 5 {
        value_to_u16(&args[4], "web-route-static!")?
    } else {
        200
    };
    primop_route_static(sid, method, path, body, status)?;
    Ok(Value::Unspecified)
}

#[cfg(feature = "web-modules")]
pub fn b_web_route_module(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-route-module!", args, 2)?;
    let sid = value_to_i64(&args[0], "web-route-module!")?;
    let path = value_to_str(&args[1], syms, "web-route-module!")?;
    let n = primop_route_module(sid, path)?;
    Ok(Value::Number(cs_core::Number::Fixnum(n as i64)))
}

pub fn b_web_access_log(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-access-log!", args, 2)?;
    let sid = value_to_i64(&args[0], "web-access-log!")?;
    let table = value_to_str(&args[1], syms, "web-access-log!")?;
    primop_access_log(sid, &table)?;
    Ok(Value::Unspecified)
}

pub fn b_web_server_start(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-server-start", args, 1)?;
    let sid = value_to_i64(&args[0], "web-server-start")?;
    let bound = primop_server_start(sid)?;
    // Return the bound address as a string — useful when the
    // caller passed `127.0.0.1:0` to let the OS pick a port.
    let g = cs_gc::Gc::new(std::cell::RefCell::new(bound));
    Ok(Value::String(g))
}

pub fn b_web_server_stop(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-server-stop", args, 1)?;
    let sid = value_to_i64(&args[0], "web-server-stop")?;
    primop_server_stop(sid)?;
    Ok(Value::Unspecified)
}

// ---------------------------------------------------------------
// Registration entry point.
// ---------------------------------------------------------------

pub fn web_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    #[allow(unused_mut)]
    let mut v: Vec<(
        &'static str,
        fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
    )> = vec![
        ("web-server-create", b_web_server_create),
        ("web-route-static!", b_web_route_static),
        ("web-access-log!", b_web_access_log),
        ("web-server-start", b_web_server_start),
        ("web-server-stop", b_web_server_stop),
    ];
    #[cfg(feature = "web-modules")]
    {
        v.push(("web-route-module!", b_web_route_module));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // We round-trip via real HTTP rather than calling primops in
    // isolation — the goal is to prove the cs-runtime side is wired
    // end-to-end. Tests use ephemeral ports (`127.0.0.1:0`) and
    // read the bound address back from `web-server-start`.

    fn http_get_blocking(addr: &str, path: &str) -> (u16, Vec<u8>) {
        use std::io::{Read, Write};
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
        // Body starts after the first "\r\n\r\n".
        let body_start = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(buf.len());
        (status, buf[body_start..].to_vec())
    }

    #[test]
    fn static_route_round_trip() {
        let sid = primop_server_create("127.0.0.1:0").expect("create");
        primop_route_static(sid, Method::GET, "/ping".into(), "pong".into(), 200).expect("route");
        let bound = primop_server_start(sid).expect("start");
        // Give the runtime a moment to settle the accept loop.
        std::thread::sleep(Duration::from_millis(50));
        let (status, body) = http_get_blocking(&bound, "/ping");
        assert_eq!(status, 200);
        assert_eq!(body, b"pong");
        primop_server_stop(sid).expect("stop");
    }

    #[test]
    fn custom_status_propagates() {
        let sid = primop_server_create("127.0.0.1:0").expect("create");
        primop_route_static(sid, Method::GET, "/teapot".into(), "tea".into(), 418).expect("route");
        let bound = primop_server_start(sid).expect("start");
        std::thread::sleep(Duration::from_millis(50));
        let (status, body) = http_get_blocking(&bound, "/teapot");
        assert_eq!(status, 418);
        assert_eq!(body, b"tea");
        primop_server_stop(sid).expect("stop");
    }

    #[test]
    fn unknown_route_404() {
        let sid = primop_server_create("127.0.0.1:0").expect("create");
        primop_route_static(sid, Method::GET, "/known".into(), "hi".into(), 200).expect("route");
        let bound = primop_server_start(sid).expect("start");
        std::thread::sleep(Duration::from_millis(50));
        let (status, _) = http_get_blocking(&bound, "/missing");
        assert_eq!(status, 404);
        primop_server_stop(sid).expect("stop");
    }

    #[test]
    fn double_stop_is_idempotent() {
        let sid = primop_server_create("127.0.0.1:0").expect("create");
        primop_route_static(sid, Method::GET, "/x".into(), "y".into(), 200).expect("route");
        let _ = primop_server_start(sid).expect("start");
        primop_server_stop(sid).expect("stop1");
        primop_server_stop(sid).expect("stop2"); // idempotent
    }

    #[test]
    fn cant_register_routes_after_start() {
        let sid = primop_server_create("127.0.0.1:0").expect("create");
        let _ = primop_server_start(sid).expect("start");
        let res = primop_route_static(sid, Method::GET, "/late".into(), "no".into(), 200);
        assert!(res.is_err());
        primop_server_stop(sid).expect("stop");
    }

    #[test]
    fn access_log_records_requests() {
        let sid = primop_server_create("127.0.0.1:0").expect("create");
        primop_route_static(sid, Method::GET, "/a".into(), "a".into(), 200).expect("route");
        primop_route_static(sid, Method::GET, "/b".into(), "b".into(), 200).expect("route");
        primop_access_log(sid, "test-access-log").expect("access-log");
        let bound = primop_server_start(sid).expect("start");
        std::thread::sleep(Duration::from_millis(50));
        let _ = http_get_blocking(&bound, "/a");
        let _ = http_get_blocking(&bound, "/b");
        let _ = http_get_blocking(&bound, "/missing");
        std::thread::sleep(Duration::from_millis(50));
        primop_server_stop(sid).expect("stop");

        // Read back the access log via the shared table registry.
        let tables = lock().tables.clone();
        let size = tables.size("test-access-log").expect("size");
        assert_eq!(size, 3);
    }
}
