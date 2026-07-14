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
//!
//! ; Route a Scheme actor as a dynamic handler. The actor's
//! ; receive loop sees `('*web-request* <handle>)` and replies
//! ; via `(web-respond! handle status body)`.
//! (web-route-actor! sid 'GET "/dynamic" actor-pid 2000)
//!
//! ; Inside an actor body, decode a received web request.
//! (web-request-method handle)              => 'GET
//! (web-request-path   handle)              => "/dynamic"
//! (web-request-body   handle)              => "..."
//! (web-request-header handle "x-token")    => "secret" or #f
//! (web-respond!       handle 200 "ok")     => unspec
//! ```

#![cfg(feature = "web")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use cs_actor::{ActorPid, Payload};
use cs_core::{SymbolTable, Value};

use cs_web::actor::{ActorHandler, WebMessage};
use cs_web::handler::service_fn;
use cs_web::{response, ArcService, Method, Router, ServerConfig, StatusCode};

use crate::builtins::beam::SendableValue;

// ---------------------------------------------------------------
// Slab: global registry of in-flight server builders / handles.
// ---------------------------------------------------------------

/// One slot in the server registry. Servers progress through two
/// states: `Building` (mutable while the user registers routes,
/// owns a Router and a list of layers) and `Running` (immutable,
/// owns the tokio JoinHandle so `web-server-stop` can abort).
/// One layer the user has installed on a building server. Each
/// variant maps to a built-in cs-web Layer; `start` builds them
/// outside-in (first-pushed wraps the outermost). The variant set
/// is closed because Scheme can't supply arbitrary Layer impls
/// without a Value-typed bridge for !Send Scheme procs inside
/// tokio tasks — Scheme-defined custom middleware is an actor
/// chain, not a Layer.
enum LayerSpec {
    Trace,
    RequestId,
    CatchPanic,
    Timeout(std::time::Duration),
    /// Scheme actor acting as a Layer. On each request the
    /// actor receives `('*web-request* h)` and decides via
    /// `web-respond!` (short-circuit) or `web-continue!`
    /// (pass to the inner service). `Duration` is the decision
    /// timeout — exceeding it returns 504 without reaching the
    /// inner service.
    ActorLayer(ActorPid, std::time::Duration),
}

enum Slot {
    Building {
        addr: SocketAddr,
        router: Router,
        access_log: Option<cs_web::table::AccessLog>,
        layers: Vec<LayerSpec>,
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
    /// Pending request envelopes. When a cs-actor receives a
    /// `WebMessage` payload, the bridge interns it here and
    /// hands the Scheme actor an opaque `(*web-request* handle)`
    /// pair. `web-respond!` consumes the slot.
    requests: HashMap<i64, Arc<WebMessage>>,
    next_request_id: i64,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next_id: 1,
            slots: HashMap::new(),
            tables: cs_table::TableRegistry::new(),
            requests: HashMap::new(),
            next_request_id: 1,
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
        Value::Fixnum(n) => Ok(*n),
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
            layers: Vec::new(),
        },
    );
    Ok(id)
}

fn primop_layer_push(sid: i64, who: &'static str, spec: LayerSpec) -> Result<(), String> {
    with_slot(who, sid, |slot| match slot {
        Slot::Building { layers, .. } => {
            layers.push(spec);
            Ok(())
        }
        _ => Err(format!(
            "{}: server #{} already started or stopped",
            who, sid
        )),
    })
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
            layers,
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
        // Apply user-installed layers in reverse-push order so
        // the first `web-layer-*!` call ends up the OUTERMOST
        // wrapper (matches the cs_web::Stack semantics).
        let mut stack = cs_web::Stack::new();
        for spec in layers {
            stack = match spec {
                LayerSpec::Trace => stack.push(cs_web::Trace),
                LayerSpec::RequestId => stack.push(cs_web::RequestId::new()),
                LayerSpec::CatchPanic => stack.push(cs_web::CatchPanic),
                LayerSpec::Timeout(d) => stack.push(cs_web::Timeout::new(d)),
                LayerSpec::ActorLayer(pid, d) => {
                    let actor_ref = match crate::builtins::beam::lookup_pid(pid) {
                        Some(r) => r,
                        None => {
                            return Err(format!(
                                "web-server-start: layer actor {} not found (terminated?)",
                                pid
                            ));
                        }
                    };
                    stack.push(cs_web::actor::actor_layer(actor_ref, d))
                }
            };
        }
        service = stack.wrap(service);
        // Always-on outermost CatchPanic so a panicking handler
        // (or middleware) never crashes the connection task. A
        // user-installed CatchPanic stacks INSIDE this one —
        // double-wrap is harmless because a panic in a higher
        // layer is also caught.
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
    Ok(Value::Fixnum(id))
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
    Ok(Value::Fixnum(n as i64))
}

pub fn b_web_layer_trace(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-layer-trace!", args, 1)?;
    let sid = value_to_i64(&args[0], "web-layer-trace!")?;
    primop_layer_push(sid, "web-layer-trace!", LayerSpec::Trace)?;
    Ok(Value::Unspecified)
}

pub fn b_web_layer_request_id(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-layer-request-id!", args, 1)?;
    let sid = value_to_i64(&args[0], "web-layer-request-id!")?;
    primop_layer_push(sid, "web-layer-request-id!", LayerSpec::RequestId)?;
    Ok(Value::Unspecified)
}

pub fn b_web_layer_catch_panic(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-layer-catch-panic!", args, 1)?;
    let sid = value_to_i64(&args[0], "web-layer-catch-panic!")?;
    primop_layer_push(sid, "web-layer-catch-panic!", LayerSpec::CatchPanic)?;
    Ok(Value::Unspecified)
}

/// Push an actor-backed Layer onto the building stack. Scheme:
/// `(web-layer-actor! sid pid [timeout-ms])`. Timeout defaults
/// to 30 s.
pub fn b_web_layer_actor(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 && args.len() != 3 {
        return Err(format!(
            "web-layer-actor!: expected 2 or 3 arguments, got {}",
            args.len()
        ));
    }
    let sid = value_to_i64(&args[0], "web-layer-actor!")?;
    let pid = value_to_pid(&args[1], syms, "web-layer-actor!")?;
    let timeout_ms = if args.len() == 3 {
        let n = value_to_i64(&args[2], "web-layer-actor!")?;
        if n < 0 {
            return Err("web-layer-actor!: timeout-ms must be non-negative".into());
        }
        n as u64
    } else {
        30_000
    };
    primop_layer_push(
        sid,
        "web-layer-actor!",
        LayerSpec::ActorLayer(pid, std::time::Duration::from_millis(timeout_ms)),
    )?;
    Ok(Value::Unspecified)
}

/// `(web-continue! handle)` — signal a layer actor's decision
/// to pass the request through to the inner service. Consumes
/// the slab entry; subsequent inspection / respond against the
/// same handle errors. Returns `#f` (without erroring) if the
/// handle was a handler envelope rather than a layer envelope —
/// makes it safe to call optimistically in middleware that
/// could be invoked in either context.
pub fn primop_continue(handle: i64) -> Result<bool, String> {
    let msg = take_request(handle)?;
    Ok(msg.signal_continue())
}

pub fn b_web_continue(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-continue!", args, 1)?;
    let h = value_to_i64(&args[0], "web-continue!")?;
    let ok = primop_continue(h)?;
    Ok(Value::Boolean(ok))
}

pub fn b_web_layer_timeout(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-layer-timeout!", args, 2)?;
    let sid = value_to_i64(&args[0], "web-layer-timeout!")?;
    let ms = value_to_i64(&args[1], "web-layer-timeout!")?;
    if ms < 0 {
        return Err("web-layer-timeout!: timeout-ms must be non-negative".into());
    }
    primop_layer_push(
        sid,
        "web-layer-timeout!",
        LayerSpec::Timeout(std::time::Duration::from_millis(ms as u64)),
    )?;
    Ok(Value::Unspecified)
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
    let g = cs_gc::Gc::new(std::cell::RefCell::new(bound.into()));
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

// ---------------------------------------------------------------
// WebMessage payload bridge — exposes Scheme-defined dynamic
// handlers.
//
// When a cs-actor receives a `Message::User(payload)` where the
// payload is a `WebMessage`, the bridge slabs the envelope and
// returns a tagged pair `('*web-request* <handle>)` that Scheme
// pattern-matches the same way it matches `('*exit* …)` /
// `('*down* …)`. The Scheme handler then reads request data and
// ships its response via the `web-request-*` / `web-respond!`
// primops below.
// ---------------------------------------------------------------

/// Called by `crate::builtins::beam::message_to_sendable` when a
/// User payload's primary `SendableValue` downcast fails. If the
/// payload is in fact a [`WebMessage`], stash the envelope in the
/// request slab and return the tagged pair Scheme will receive.
///
/// Returning `None` lets the caller fall through to the
/// `*opaque-payload*` placeholder for genuinely-foreign payloads.
pub fn try_intern_web_request(payload: &Payload) -> Option<SendableValue> {
    let msg: Arc<WebMessage> = Arc::clone(payload).downcast::<WebMessage>().ok()?;
    let mut reg = lock();
    let id = reg.next_request_id;
    reg.next_request_id += 1;
    reg.requests.insert(id, msg);
    Some(SendableValue::Pair(
        Box::new(SendableValue::Symbol("*web-request*".into())),
        Box::new(SendableValue::Pair(
            Box::new(SendableValue::Fixnum(id)),
            Box::new(SendableValue::Null),
        )),
    ))
}

/// Read-only access to a request slot. Returns Err if the handle
/// has already been responded to or never existed.
fn with_request<R>(who: &str, handle: i64, f: impl FnOnce(&WebMessage) -> R) -> Result<R, String> {
    let reg = lock();
    let msg = reg.requests.get(&handle).ok_or_else(|| {
        format!(
            "{}: web request #{} not found (already responded?)",
            who, handle
        )
    })?;
    Ok(f(msg.as_ref()))
}

/// Take a request slot out of the slab for the respond path.
fn take_request(handle: i64) -> Result<Arc<WebMessage>, String> {
    lock().requests.remove(&handle).ok_or_else(|| {
        format!(
            "web-respond!: web request #{} not found (already responded?)",
            handle
        )
    })
}

pub fn primop_request_method(handle: i64) -> Result<String, String> {
    with_request("web-request-method", handle, |m| m.req.method().to_string())
}

pub fn primop_request_path(handle: i64) -> Result<String, String> {
    with_request("web-request-path", handle, |m| {
        m.req.uri().path().to_string()
    })
}

pub fn primop_request_body(handle: i64) -> Result<String, String> {
    with_request("web-request-body", handle, |m| {
        String::from_utf8_lossy(m.req.body()).into_owned()
    })
}

/// Parse the request URI's query string into an ordered list of
/// `(name . value)` pairs. URL-decodes both. Empty / missing
/// query → empty list. Values without `=` decode to the empty
/// string. Order is preserved so `(assv ...)` returns the first
/// match when a key repeats.
pub fn primop_request_params(handle: i64) -> Result<Vec<(String, String)>, String> {
    with_request("web-request-params", handle, |m| {
        m.req.uri().query().map(parse_query).unwrap_or_default()
    })
}

/// Single named query param. `None` if the query string is
/// missing or doesn't contain `name`. Returns the FIRST match if
/// the key repeats — same convention as `(assv name (params))`.
pub fn primop_request_param(handle: i64, name: &str) -> Result<Option<String>, String> {
    with_request("web-request-param", handle, |m| {
        let q = m.req.uri().query()?;
        parse_query(q)
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    })
}

/// Every request header as an alist of `(name . value)` pairs.
/// Header names lowercased (http::HeaderName is case-insensitive
/// internally). Multi-valued headers appear once per value.
pub fn primop_request_headers(handle: i64) -> Result<Vec<(String, String)>, String> {
    with_request("web-request-headers", handle, |m| {
        m.req
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect()
    })
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|kv| match kv.find('=') {
            Some(i) => (url_decode(&kv[..i]), url_decode(&kv[i + 1..])),
            None => (url_decode(kv), String::new()),
        })
        .collect()
}

/// Basic percent decoder. `+` → space (form-encoded convention),
/// `%XX` → byte XX. Falls back to literal bytes on malformed
/// escapes.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn primop_request_header(handle: i64, name: &str) -> Result<Option<String>, String> {
    with_request("web-request-header", handle, |m| {
        m.req
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok().map(|s| s.to_string()))
    })
}

pub fn primop_respond(handle: i64, status: u16, body: String) -> Result<(), String> {
    let msg = take_request(handle)?;
    let status = StatusCode::from_u16(status)
        .map_err(|e| format!("web-respond!: invalid status {}: {}", status, e))?;
    let resp = response(status, body);
    if !msg.reply_with(resp) {
        return Err("web-respond!: reply channel already consumed".into());
    }
    Ok(())
}

fn primop_route_actor(
    sid: i64,
    method: Method,
    path: String,
    pid: ActorPid,
    timeout_ms: u64,
) -> Result<(), String> {
    let actor_ref = crate::builtins::beam::lookup_pid(pid)
        .ok_or_else(|| format!("web-route-actor!: actor {} not found (terminated?)", pid))?;
    let svc: ArcService =
        ActorHandler::new(actor_ref, std::time::Duration::from_millis(timeout_ms)).into_service();
    with_slot("web-route-actor!", sid, |slot| match slot {
        Slot::Building { router, .. } => {
            let r = std::mem::replace(router, Router::new());
            *router = r.route(method, &path, svc);
            Ok(())
        }
        _ => Err(format!(
            "web-route-actor!: server #{} already started or stopped",
            sid
        )),
    })
}

// ---------------------------------------------------------------
// Scheme glue for the request bridge.
// ---------------------------------------------------------------

fn parse_pid_symbol(name: &str) -> Option<ActorPid> {
    let inner = name.strip_prefix("<pid:<")?.strip_suffix(">>")?;
    let (n, l) = inner.split_once('.')?;
    Some(ActorPid {
        node: n.parse().ok()?,
        local_id: l.parse().ok()?,
    })
}

fn value_to_pid(v: &Value, syms: &SymbolTable, who: &str) -> Result<ActorPid, String> {
    match v {
        Value::Symbol(s) => {
            let name = syms.name(*s);
            parse_pid_symbol(name).ok_or_else(|| {
                format!(
                    "{}: expected a PID symbol like <pid:<n.m>>, got '{}'",
                    who, name
                )
            })
        }
        other => Err(format!(
            "{}: expected a PID symbol, got {}",
            who,
            other.type_name()
        )),
    }
}

pub fn b_web_request_method(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-method", args, 1)?;
    let h = value_to_i64(&args[0], "web-request-method")?;
    let m = primop_request_method(h)?;
    Ok(Value::Symbol(syms.intern(&m)))
}

pub fn b_web_request_path(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-path", args, 1)?;
    let h = value_to_i64(&args[0], "web-request-path")?;
    let p = primop_request_path(h)?;
    let g = cs_gc::Gc::new(std::cell::RefCell::new(p.into()));
    Ok(Value::String(g))
}

pub fn b_web_request_body(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-body", args, 1)?;
    let h = value_to_i64(&args[0], "web-request-body")?;
    let b = primop_request_body(h)?;
    let g = cs_gc::Gc::new(std::cell::RefCell::new(b.into()));
    Ok(Value::String(g))
}

fn alist_to_scheme(pairs: Vec<(String, String)>) -> Value {
    let mut acc = Value::Null;
    for (k, v) in pairs.into_iter().rev() {
        let k_g = cs_gc::Gc::new(std::cell::RefCell::new(k.into()));
        let v_g = cs_gc::Gc::new(std::cell::RefCell::new(v.into()));
        let pair = Value::Pair(cs_core::Pair::new(Value::String(k_g), Value::String(v_g)));
        acc = Value::Pair(cs_core::Pair::new(pair, acc));
    }
    acc
}

pub fn b_web_request_params(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-params", args, 1)?;
    let h = value_to_i64(&args[0], "web-request-params")?;
    let pairs = primop_request_params(h)?;
    Ok(alist_to_scheme(pairs))
}

pub fn b_web_request_param(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-param", args, 2)?;
    let h = value_to_i64(&args[0], "web-request-param")?;
    let name = value_to_str(&args[1], syms, "web-request-param")?;
    match primop_request_param(h, &name)? {
        Some(v) => {
            let g = cs_gc::Gc::new(std::cell::RefCell::new(v.into()));
            Ok(Value::String(g))
        }
        None => Ok(Value::Boolean(false)),
    }
}

pub fn b_web_request_headers(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-headers", args, 1)?;
    let h = value_to_i64(&args[0], "web-request-headers")?;
    let pairs = primop_request_headers(h)?;
    Ok(alist_to_scheme(pairs))
}

pub fn b_web_request_header(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("web-request-header", args, 2)?;
    let h = value_to_i64(&args[0], "web-request-header")?;
    let name = value_to_str(&args[1], syms, "web-request-header")?;
    match primop_request_header(h, &name)? {
        Some(v) => {
            let g = cs_gc::Gc::new(std::cell::RefCell::new(v.into()));
            Ok(Value::String(g))
        }
        None => Ok(Value::Boolean(false)),
    }
}

pub fn b_web_respond(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 && args.len() != 3 {
        return Err(format!(
            "web-respond!: expected 2 or 3 arguments, got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "web-respond!")?;
    let (status, body) = if args.len() == 3 {
        (
            value_to_u16(&args[1], "web-respond!")?,
            value_to_str(&args[2], syms, "web-respond!")?,
        )
    } else {
        (200, value_to_str(&args[1], syms, "web-respond!")?)
    };
    primop_respond(h, status, body)?;
    Ok(Value::Unspecified)
}

pub fn b_web_route_actor(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 && args.len() != 5 {
        return Err(format!(
            "web-route-actor!: expected 4 or 5 arguments, got {}",
            args.len()
        ));
    }
    let sid = value_to_i64(&args[0], "web-route-actor!")?;
    let method_name = value_to_str(&args[1], syms, "web-route-actor!")?;
    let method =
        method_from_symbol(&method_name).map_err(|e| format!("web-route-actor!: {}", e))?;
    let path = value_to_str(&args[2], syms, "web-route-actor!")?;
    let pid = value_to_pid(&args[3], syms, "web-route-actor!")?;
    let timeout_ms = if args.len() == 5 {
        let n = value_to_i64(&args[4], "web-route-actor!")?;
        if n < 0 {
            return Err("web-route-actor!: timeout-ms must be non-negative".into());
        }
        n as u64
    } else {
        30_000
    };
    primop_route_actor(sid, method, path, pid, timeout_ms)?;
    Ok(Value::Unspecified)
}

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
        ("web-route-actor!", b_web_route_actor),
        ("web-access-log!", b_web_access_log),
        ("web-layer-trace!", b_web_layer_trace),
        ("web-layer-request-id!", b_web_layer_request_id),
        ("web-layer-catch-panic!", b_web_layer_catch_panic),
        ("web-layer-timeout!", b_web_layer_timeout),
        ("web-layer-actor!", b_web_layer_actor),
        ("web-continue!", b_web_continue),
        ("web-server-start", b_web_server_start),
        ("web-server-stop", b_web_server_stop),
        ("web-request-method", b_web_request_method),
        ("web-request-path", b_web_request_path),
        ("web-request-body", b_web_request_body),
        ("web-request-header", b_web_request_header),
        ("web-request-headers", b_web_request_headers),
        ("web-request-param", b_web_request_param),
        ("web-request-params", b_web_request_params),
        ("web-respond!", b_web_respond),
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
    fn bridge_interns_web_message_payload() {
        // Build a WebMessage envelope by hand and run it through
        // the bridge — no need to spin up a server.
        let req: cs_web::Request = cs_web::http::Request::builder()
            .method(Method::POST)
            .uri("/things/42")
            .header("x-token", "secret")
            .body(cs_web::Bytes::from_static(b"payload"))
            .unwrap();

        let (tx, _rx) = tokio::sync::oneshot::channel::<cs_web::Response>();
        let envelope: Arc<WebMessage> = Arc::new(WebMessage::new(req, tx));
        let payload: Payload = envelope;

        // Bridge produces ('*web-request* <handle>) and registers
        // the envelope.
        let sv = try_intern_web_request(&payload).expect("bridge should match");
        let handle = match sv {
            SendableValue::Pair(head, tail) => {
                assert!(matches!(*head, SendableValue::Symbol(ref s) if s == "*web-request*"));
                match *tail {
                    SendableValue::Pair(boxed_id, boxed_nil) => {
                        assert!(matches!(*boxed_nil, SendableValue::Null));
                        match *boxed_id {
                            SendableValue::Fixnum(n) => n,
                            _ => panic!("handle was not a fixnum"),
                        }
                    }
                    _ => panic!("tag pair tail was not a pair"),
                }
            }
            _ => panic!("bridge did not return a pair"),
        };

        // Inspect the request via the same primops Scheme will use.
        assert_eq!(primop_request_method(handle).unwrap(), "POST");
        assert_eq!(primop_request_path(handle).unwrap(), "/things/42");
        assert_eq!(primop_request_body(handle).unwrap(), "payload");
        assert_eq!(
            primop_request_header(handle, "x-token").unwrap().as_deref(),
            Some("secret")
        );
        assert!(primop_request_header(handle, "missing").unwrap().is_none());

        // Respond. The slot is consumed.
        primop_respond(handle, 200, "ok".into()).unwrap();
        // Second respond / inspect must error — slot was taken.
        assert!(primop_respond(handle, 200, "again".into()).is_err());
        assert!(primop_request_method(handle).is_err());
    }

    #[test]
    fn params_headers_round_trip() {
        let req: cs_web::Request = cs_web::http::Request::builder()
            .method(Method::GET)
            .uri("/items?id=42&name=alice&tag=red&tag=blue&empty=")
            .header("x-token", "sekret")
            .header("accept", "application/json")
            .body(cs_web::Bytes::new())
            .unwrap();
        let (tx, _rx) = tokio::sync::oneshot::channel::<cs_web::Response>();
        let envelope: Arc<WebMessage> = Arc::new(WebMessage::new(req, tx));
        let payload: Payload = envelope;
        let sv = try_intern_web_request(&payload).expect("bridge");
        let handle = match sv {
            SendableValue::Pair(_, tail) => match *tail {
                SendableValue::Pair(boxed_id, _) => match *boxed_id {
                    SendableValue::Fixnum(n) => n,
                    _ => panic!("not fixnum"),
                },
                _ => panic!("tail not pair"),
            },
            _ => panic!("sv not pair"),
        };

        let params = primop_request_params(handle).unwrap();
        assert_eq!(
            params,
            vec![
                ("id".into(), "42".into()),
                ("name".into(), "alice".into()),
                ("tag".into(), "red".into()),
                ("tag".into(), "blue".into()),
                ("empty".into(), "".into()),
            ]
        );

        // Single-param lookup picks the first occurrence — matches
        // `(assv 'tag ...)` semantics in Scheme.
        assert_eq!(
            primop_request_param(handle, "id").unwrap().as_deref(),
            Some("42")
        );
        assert_eq!(
            primop_request_param(handle, "tag").unwrap().as_deref(),
            Some("red")
        );
        assert!(primop_request_param(handle, "missing").unwrap().is_none());

        let headers = primop_request_headers(handle).unwrap();
        assert!(headers.iter().any(|(k, v)| k == "x-token" && v == "sekret"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "accept" && v == "application/json"));

        primop_respond(handle, 200, "ok".into()).unwrap();
    }

    #[test]
    fn params_url_decode() {
        let req: cs_web::Request = cs_web::http::Request::builder()
            .method(Method::GET)
            // %20 → space, + → space, %2B → +, %3D → =
            .uri("/search?q=hello+world&filter=a%2Bb%3Dc&path=/a%20b")
            .body(cs_web::Bytes::new())
            .unwrap();
        let (tx, _rx) = tokio::sync::oneshot::channel::<cs_web::Response>();
        let envelope: Arc<WebMessage> = Arc::new(WebMessage::new(req, tx));
        let sv = try_intern_web_request(&(envelope as Payload)).expect("bridge");
        let handle = match sv {
            SendableValue::Pair(_, tail) => match *tail {
                SendableValue::Pair(boxed_id, _) => match *boxed_id {
                    SendableValue::Fixnum(n) => n,
                    _ => panic!(),
                },
                _ => panic!(),
            },
            _ => panic!(),
        };

        let params = primop_request_params(handle).unwrap();
        assert_eq!(
            params,
            vec![
                ("q".into(), "hello world".into()),
                ("filter".into(), "a+b=c".into()),
                ("path".into(), "/a b".into()),
            ]
        );

        primop_respond(handle, 200, "ok".into()).unwrap();
    }

    #[test]
    fn bridge_ignores_foreign_payload() {
        // Wrap a String in the payload — not a WebMessage. Bridge
        // returns None, leaving the *opaque-payload* path intact.
        let payload: Payload = Arc::new("not-a-web-request".to_string());
        assert!(try_intern_web_request(&payload).is_none());
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
