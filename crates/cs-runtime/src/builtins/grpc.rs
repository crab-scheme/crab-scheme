//! gRPC (h2c) server primops exposed to Scheme. Behind the `grpc`
//! feature.
//!
//! This is the Scheme-facing wiring for cs-web's cleartext HTTP/2
//! gRPC transport ([`cs_web::grpc`]). It is the substrate the etcd
//! v3 services (KV / Watch / Lease) bind onto: gRPC *framing* and
//! *trailers* are handled in Rust (hyper); gRPC *semantics* —
//! method dispatch, protobuf encode/decode, leader redirects — stay
//! in Scheme.
//!
//! ## Dispatch model: the actor bridge
//!
//! The cs-runtime evaluator is `!Send`, so a Scheme procedure can't
//! be called directly from hyper's multi-thread tokio task. We use
//! the same bridge cs-web uses for dynamic HTTP handlers: each gRPC
//! call/message becomes a mailbox message to a Scheme **actor**. The
//! actor's `(raw-receive)` loop sees `('*grpc-request* <handle>)`,
//! reads the method path + request bytes, dispatches, and drives the
//! response through the respond/stream primops.
//!
//! ## Surface
//!
//! ```ignore
//! ; Start an h2c gRPC server. `addr` is "host:port" (port 0 lets
//! ; the OS pick). `handler-pid` is a spawned actor that receives a
//! ; ('*grpc-request* h) per call. Returns an integer server handle.
//! (grpc-serve "127.0.0.1:2379" handler-pid)        => 1
//! (grpc-serve "127.0.0.1:0"    handler-pid 5000)   => 2  ; opt arg accepted (unused)
//!
//! ; Start a TLS / mutual-TLS gRPC server (cw-u4a.21). Same handler
//! ; contract; the socket is rustls-terminated. With require-client-cert?
//! ; = #t it is mTLS (a client without a cert chaining to ca-pem is
//! ; rejected at the TLS layer); with #f it is plain server TLS.
//! (grpc-serve-tls "127.0.0.1:2379" handler-pid
//!                 "server.crt" "server.key" "ca.crt" #t)  => 3
//!
//! ; Stop a server (h2c or TLS). Idempotent.
//! (grpc-server-stop sid)
//!
//! ; --- inside the handler actor ---
//! ; ('*grpc-request*    h)       first/only client message of a call
//! ; ('*grpc-stream-msg* h bytes) a subsequent client-streamed message (bidi)
//! ; ('*grpc-stream-end* h)       the client half-closed the request stream
//! (grpc-request-path  h)   => "/etcdserverpb.KV/Range"   ; the :path
//! (grpc-request-bytes h)   => #u8(...)                    ; FIRST request message
//! ; The verified mTLS client identity (SAN/CN), or #f over h2c / no cert.
//! (grpc-request-peer-identity h) => "etcd-client" | #f
//!
//! ; UNARY: one response message + grpc-status:0 trailer; consumes h.
//! (grpc-respond! h response-bytevector)
//! ; UNARY error: a non-OK grpc-status + grpc-message; consumes h.
//! (grpc-respond-error! h 5 "key not found")
//!
//! ; STREAMING: queue one response message (does NOT consume h) ->
//! ; returns #t, or #f if the client already hung up.
//! (grpc-stream-send! h response-bytevector)        => #t|#f
//! ; STREAMING: end the response stream with trailers; consumes h.
//! (grpc-stream-close! h)                  ; status 0
//! (grpc-stream-close! h 14 "no leader")   ; status + grpc-message
//! ```

#![cfg(feature = "grpc")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use cs_actor::{ActorPid, Payload};
use cs_core::{SymbolTable, Value};

use cs_web::grpc::{
    bind_grpc, grpc_server_tls_config, serve_grpc, serve_grpc_tls, ArcGrpcHandler, GrpcHandler,
    GrpcRequest, GrpcResponseSink,
};
use cs_web::Bytes;

use crate::builtins::beam::SendableValue;

// ---------------------------------------------------------------
// Envelopes: a gRPC call event crossing from hyper's task to a
// Scheme actor's mailbox.
// ---------------------------------------------------------------

/// A new call: the first request message + the sink driving the
/// response. The sink (`Clone + Send + Sync`) is stashed in the
/// registry so the respond/stream primops can reach it by handle.
struct GrpcBeginMsg {
    call_id: i64,
    path: String,
    message: Bytes,
    /// The verified mTLS peer identity for this call, or `None` on the
    /// h2c (cleartext) path / a TLS connection with no client cert.
    /// Carried from the transport so the bridge can expose it via
    /// `grpc-request-peer-identity`.
    peer_identity: Option<Arc<str>>,
    /// The call's gRPC metadata (HTTP/2 headers), lowercase keys. Carried
    /// from the transport so the bridge can expose it via
    /// `grpc-request-metadata` (the etcd-Auth `token` hook, `.26`).
    metadata: HashMap<String, String>,
    sink: GrpcResponseSink,
}

/// A subsequent client-streamed request message (bidi).
struct GrpcStreamMsg {
    call_id: i64,
    message: Bytes,
}

/// The client half-closed the request stream.
struct GrpcStreamEnd {
    call_id: i64,
}

/// One in-flight call's response state, keyed by the handle Scheme
/// sees. `grpc-respond!` / `grpc-respond-error!` / `grpc-stream-close!`
/// consume the entry; `grpc-stream-send!` leaves it in place.
struct StreamSlot {
    path: String,
    first_message: Bytes,
    peer_identity: Option<Arc<str>>,
    metadata: HashMap<String, String>,
    sink: GrpcResponseSink,
}

// ---------------------------------------------------------------
// Handler: forwards every call event to a Scheme actor's mailbox.
// ---------------------------------------------------------------

/// A [`GrpcHandler`] that forwards every call event to a Scheme
/// actor's mailbox. The Scheme side drives the response asynchronously
/// through the sink (no server-side reply timeout — gRPC deadlines are
/// client-driven; a dead handler simply never produces frames and the
/// client's deadline fires).
struct ActorGrpcHandler {
    target: cs_actor::ActorRef,
}

impl GrpcHandler for ActorGrpcHandler {
    fn begin(&self, call_id: u64, req: GrpcRequest, sink: GrpcResponseSink) {
        let envelope = Arc::new(GrpcBeginMsg {
            call_id: call_id as i64,
            path: req.path,
            message: req.message,
            peer_identity: req.peer_identity,
            metadata: req.metadata,
            sink: sink.clone(),
        });
        let payload: Payload = envelope;
        if self.target.send(payload).is_err() {
            // 14 = UNAVAILABLE — the handler actor's mailbox is closed.
            sink.close(14, Some("grpc handler actor unavailable".into()));
        }
    }

    fn client_message(&self, call_id: u64, message: Bytes) {
        let envelope = Arc::new(GrpcStreamMsg {
            call_id: call_id as i64,
            message,
        });
        let _ = self.target.send(envelope as Payload);
    }

    fn client_end(&self, call_id: u64) {
        let envelope = Arc::new(GrpcStreamEnd {
            call_id: call_id as i64,
        });
        let _ = self.target.send(envelope as Payload);
    }
}

// ---------------------------------------------------------------
// Registry: running servers + the in-flight call slab.
// ---------------------------------------------------------------

enum Slot {
    Running {
        shutdown_tx: tokio::sync::oneshot::Sender<()>,
        join: tokio::task::JoinHandle<()>,
    },
    // Closed slots stay registered so a double-stop is a no-op.
    Stopped,
}

struct Registry {
    next_id: i64,
    slots: HashMap<i64, Slot>,
    /// In-flight calls, keyed by the transport-assigned `call_id`
    /// (which is the handle Scheme sees in `('*grpc-request* h)`).
    requests: HashMap<i64, StreamSlot>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next_id: 1,
            slots: HashMap::new(),
            requests: HashMap::new(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    registry().lock().expect("cs-grpc registry poisoned")
}

// ---------------------------------------------------------------
// Background tokio runtime. Shared with the same rationale as
// cs-web: the Scheme caller is `!Send` and can't sit on a tokio
// task, so gRPC servers run on a dedicated multi-thread runtime.
// ---------------------------------------------------------------

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("cs-grpc")
            .build()
            .expect("build cs-grpc tokio runtime")
    })
}

// ---------------------------------------------------------------
// Helpers (host-fn shape).
// ---------------------------------------------------------------

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

/// R7RS truthiness: only `#f` is false; everything else (including the
/// symbol `#t`) is true. Used for the `require-client-cert?` flag.
fn value_to_bool(v: &Value) -> bool {
    !matches!(v, Value::Boolean(false))
}

fn value_to_bytes(v: &Value, who: &str) -> Result<Bytes, String> {
    match v {
        Value::ByteVector(b) => Ok(Bytes::copy_from_slice(&b.borrow())),
        // A string is accepted as a convenience (UTF-8 bytes).
        Value::String(g) => Ok(Bytes::from(g.borrow().clone().into_bytes())),
        other => Err(format!(
            "{}: expected bytevector, got {}",
            who,
            other.type_name()
        )),
    }
}

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

// ---------------------------------------------------------------
// Server lifecycle.
// ---------------------------------------------------------------

fn primop_serve(addr: &str, pid: ActorPid) -> Result<(i64, String), String> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| format!("grpc-serve: invalid addr {:?}: {}", addr, e))?;
    let actor_ref = crate::builtins::beam::lookup_pid(pid)
        .ok_or_else(|| format!("grpc-serve: handler actor {} not found (terminated?)", pid))?;
    let handler: ArcGrpcHandler = Arc::new(ActorGrpcHandler { target: actor_ref });

    // Bind synchronously so the Scheme caller sees bind errors
    // immediately (e.g. address in use), not from a detached task.
    let (listener, bound) = rt()
        .block_on(async { bind_grpc(addr).await })
        .map_err(|e| format!("grpc-serve: bind: {}", e))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let join = rt().spawn(async move {
        let _ = serve_grpc(
            listener,
            handler,
            Some(async move {
                let _ = shutdown_rx.await;
            }),
        )
        .await;
    });

    let mut reg = lock();
    let id = reg.next_id;
    reg.next_id += 1;
    reg.slots.insert(id, Slot::Running { shutdown_tx, join });
    Ok((id, bound.to_string()))
}

/// Start a TLS/mTLS gRPC server (cw-u4a.21). Same actor-bridge handler
/// as [`primop_serve`], but the socket is rustls-terminated and — when
/// `require_client_cert` — every connection must present a client cert
/// chaining to `ca_pem` (require-and-verify). The verified peer identity
/// is exposed per call via `grpc-request-peer-identity`.
#[allow(clippy::too_many_arguments)]
fn primop_serve_tls(
    addr: &str,
    pid: ActorPid,
    cert_pem: &str,
    key_pem: &str,
    ca_pem: &str,
    require_client_cert: bool,
) -> Result<(i64, String), String> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| format!("grpc-serve-tls: invalid addr {:?}: {}", addr, e))?;
    let actor_ref = crate::builtins::beam::lookup_pid(pid).ok_or_else(|| {
        format!(
            "grpc-serve-tls: handler actor {} not found (terminated?)",
            pid
        )
    })?;
    let handler: ArcGrpcHandler = Arc::new(ActorGrpcHandler { target: actor_ref });

    // Build the rustls server config (load PEMs + build the client-cert
    // verifier) synchronously, so bad paths / certs / a missing CA fail
    // the Scheme caller immediately rather than from a detached task.
    let tls_config = grpc_server_tls_config(cert_pem, key_pem, ca_pem, require_client_cert)
        .map_err(|e| format!("grpc-serve-tls: {}", e))?;

    let (listener, bound) = rt()
        .block_on(async { bind_grpc(addr).await })
        .map_err(|e| format!("grpc-serve-tls: bind: {}", e))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let join = rt().spawn(async move {
        let _ = serve_grpc_tls(
            listener,
            tls_config,
            handler,
            Some(async move {
                let _ = shutdown_rx.await;
            }),
        )
        .await;
    });

    let mut reg = lock();
    let id = reg.next_id;
    reg.next_id += 1;
    reg.slots.insert(id, Slot::Running { shutdown_tx, join });
    Ok((id, bound.to_string()))
}

fn primop_server_stop(sid: i64) -> Result<(), String> {
    let mut reg = lock();
    let slot = reg
        .slots
        .get_mut(&sid)
        .ok_or_else(|| format!("grpc-server-stop: server #{} not found", sid))?;
    match std::mem::replace(slot, Slot::Stopped) {
        Slot::Running { shutdown_tx, join } => {
            let _ = shutdown_tx.send(());
            join.abort();
            Ok(())
        }
        Slot::Stopped => Ok(()),
    }
}

// ---------------------------------------------------------------
// Request bridge: intern an incoming call event, expose its data,
// drive the response.
// ---------------------------------------------------------------

/// Called by `beam::message_to_sendable` when a User payload isn't a
/// plain `SendableValue`. Recognises the three gRPC call envelopes and
/// returns the tagged pair the Scheme actor pattern-matches. A
/// [`GrpcBeginMsg`] also stashes the call's [`StreamSlot`]. Returns
/// `None` for other payloads so the caller can keep trying.
pub fn try_intern_grpc_request(payload: &Payload) -> Option<SendableValue> {
    if let Ok(begin) = Arc::clone(payload).downcast::<GrpcBeginMsg>() {
        let mut reg = lock();
        reg.requests.insert(
            begin.call_id,
            StreamSlot {
                path: begin.path.clone(),
                first_message: begin.message.clone(),
                peer_identity: begin.peer_identity.clone(),
                metadata: begin.metadata.clone(),
                sink: begin.sink.clone(),
            },
        );
        return Some(tagged("*grpc-request*", begin.call_id, None));
    }
    if let Ok(m) = Arc::clone(payload).downcast::<GrpcStreamMsg>() {
        return Some(tagged(
            "*grpc-stream-msg*",
            m.call_id,
            Some(m.message.to_vec()),
        ));
    }
    if let Ok(e) = Arc::clone(payload).downcast::<GrpcStreamEnd>() {
        return Some(tagged("*grpc-stream-end*", e.call_id, None));
    }
    None
}

/// Build `(tag handle)` or `(tag handle #u8(bytes))`.
fn tagged(tag: &str, handle: i64, bytes: Option<Vec<u8>>) -> SendableValue {
    let tail = match bytes {
        Some(b) => SendableValue::Pair(
            Box::new(SendableValue::Fixnum(handle)),
            Box::new(SendableValue::Pair(
                Box::new(SendableValue::ByteVector(b)),
                Box::new(SendableValue::Null),
            )),
        ),
        None => SendableValue::Pair(
            Box::new(SendableValue::Fixnum(handle)),
            Box::new(SendableValue::Null),
        ),
    };
    SendableValue::Pair(Box::new(SendableValue::Symbol(tag.into())), Box::new(tail))
}

/// Read a field of a live slot without consuming it.
fn with_slot<R>(who: &str, handle: i64, f: impl FnOnce(&StreamSlot) -> R) -> Result<R, String> {
    let reg = lock();
    let slot = reg.requests.get(&handle).ok_or_else(|| {
        format!(
            "{}: grpc call #{} not found (already responded/closed?)",
            who, handle
        )
    })?;
    Ok(f(slot))
}

/// Remove and return a call's sink (ending the call's lifecycle).
fn take_slot(who: &str, handle: i64) -> Result<GrpcResponseSink, String> {
    lock()
        .requests
        .remove(&handle)
        .map(|s| s.sink)
        .ok_or_else(|| {
            format!(
                "{}: grpc call #{} not found (already responded/closed?)",
                who, handle
            )
        })
}

fn primop_request_path(handle: i64) -> Result<String, String> {
    with_slot("grpc-request-path", handle, |s| s.path.clone())
}

fn primop_request_bytes(handle: i64) -> Result<Vec<u8>, String> {
    with_slot("grpc-request-bytes", handle, |s| s.first_message.to_vec())
}

/// The verified mTLS peer identity for this call, or `None` when the
/// call arrived over cleartext h2c / a TLS connection with no client
/// cert. The string is the client leaf cert's SAN (DNS/URI/IP) or CN.
fn primop_request_peer_identity(handle: i64) -> Result<Option<String>, String> {
    with_slot("grpc-request-peer-identity", handle, |s| {
        s.peer_identity.as_deref().map(|id| id.to_string())
    })
}

/// The value of gRPC metadata header `key` (lowercased) for this call,
/// or `None` if the request carried no such header. gRPC metadata is
/// HTTP/2 headers, so this is how the `token` / `authorization` header
/// an auth client presents is read (etcd-Auth, `.26`).
fn primop_request_metadata(handle: i64, key: &str) -> Result<Option<String>, String> {
    let key = key.to_ascii_lowercase();
    with_slot("grpc-request-metadata", handle, |s| {
        s.metadata.get(&key).cloned()
    })
}

fn primop_respond(handle: i64, message: Bytes) -> Result<(), String> {
    let sink = take_slot("grpc-respond!", handle)?;
    sink.send_message(message);
    sink.close(0, None);
    Ok(())
}

fn primop_respond_error(handle: i64, status: u32, message: String) -> Result<(), String> {
    let sink = take_slot("grpc-respond-error!", handle)?;
    sink.close(status, Some(message));
    Ok(())
}

/// Queue one streaming response message. Returns whether the client is
/// still attached (false once it hangs up) so the handler can stop.
fn primop_stream_send(handle: i64, message: Bytes) -> Result<bool, String> {
    with_slot("grpc-stream-send!", handle, |s| {
        s.sink.send_message(message)
    })
}

fn primop_stream_close(handle: i64, status: u32, message: Option<String>) -> Result<(), String> {
    let sink = take_slot("grpc-stream-close!", handle)?;
    sink.close(status, message);
    Ok(())
}

// ---------------------------------------------------------------
// Scheme glue (host-fn shape `fn(&[Value], &mut SymbolTable) -> …`).
// ---------------------------------------------------------------

fn bytevector_value(bytes: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(bytes)))
}

fn string_value(s: String) -> Value {
    Value::String(cs_gc::Gc::new(std::cell::RefCell::new(s.into())))
}

pub fn b_grpc_serve(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 && args.len() != 3 {
        return Err(format!(
            "grpc-serve: expected 2 or 3 arguments (addr handler-pid [timeout-ms]), got {}",
            args.len()
        ));
    }
    let addr = value_to_str(&args[0], syms, "grpc-serve")?;
    let pid = value_to_pid(&args[1], syms, "grpc-serve")?;
    // The optional 3rd arg (legacy reply-timeout-ms) is accepted for
    // backward compatibility but unused — streaming has no single
    // reply to time out; gRPC deadlines are client-driven.
    if args.len() == 3 {
        let _ = value_to_i64(&args[2], "grpc-serve")?;
    }
    let (id, _bound) = primop_serve(&addr, pid)?;
    Ok(Value::Fixnum(id))
}

/// `(grpc-serve-tls addr handler-pid cert-pem key-pem ca-pem require-client-cert?)`
/// — start a TLS/mTLS gRPC server (cw-u4a.21) and return an integer
/// server handle (stop it with `grpc-server-stop`, same as the h2c path).
///
/// - `addr` — "host:port" to bind (port 0 lets the OS choose).
/// - `handler-pid` — the actor that receives `('*grpc-request* h)` etc.,
///   identical to `grpc-serve`.
/// - `cert-pem` / `key-pem` — this server's certificate chain + private
///   key PEM file paths (the leaf SAN should match the dialed endpoint,
///   e.g. `IP:127.0.0.1` / `DNS:localhost`).
/// - `ca-pem` — PEM bundle of CA(s) used to verify *client* certs.
/// - `require-client-cert?` — `#t` ⇒ mutual TLS (require-and-verify: a
///   connection without a client cert chaining to `ca-pem` is rejected
///   at the TLS layer); `#f` ⇒ plain server TLS (no client cert
///   requested, so `grpc-request-peer-identity` is always `#f`).
pub fn b_grpc_serve_tls(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 6 {
        return Err(format!(
            "grpc-serve-tls: expected 6 arguments \
             (addr handler-pid cert-pem key-pem ca-pem require-client-cert?), got {}",
            args.len()
        ));
    }
    let addr = value_to_str(&args[0], syms, "grpc-serve-tls")?;
    let pid = value_to_pid(&args[1], syms, "grpc-serve-tls")?;
    let cert_pem = value_to_str(&args[2], syms, "grpc-serve-tls")?;
    let key_pem = value_to_str(&args[3], syms, "grpc-serve-tls")?;
    let ca_pem = value_to_str(&args[4], syms, "grpc-serve-tls")?;
    let require_client_cert = value_to_bool(&args[5]);
    let (id, _bound) = primop_serve_tls(
        &addr,
        pid,
        &cert_pem,
        &key_pem,
        &ca_pem,
        require_client_cert,
    )?;
    Ok(Value::Fixnum(id))
}

pub fn b_grpc_server_stop(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "grpc-server-stop: expected 1 argument, got {}",
            args.len()
        ));
    }
    let sid = value_to_i64(&args[0], "grpc-server-stop")?;
    primop_server_stop(sid)?;
    Ok(Value::Unspecified)
}

pub fn b_grpc_request_path(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "grpc-request-path: expected 1 argument, got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-request-path")?;
    Ok(string_value(primop_request_path(h)?))
}

pub fn b_grpc_request_bytes(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "grpc-request-bytes: expected 1 argument, got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-request-bytes")?;
    Ok(bytevector_value(primop_request_bytes(h)?))
}

/// `(grpc-request-peer-identity h)` → the verified mTLS client identity
/// string for call `h` (the client leaf cert's SAN DNS/URI/IP, else its
/// Subject CN), or `#f` when the call came over cleartext h2c or a TLS
/// connection without a client certificate. This is the hook etcd Auth
/// (cw-u4a.26) maps to a user; `.21` only EXPOSES it.
pub fn b_grpc_request_peer_identity(
    args: &[Value],
    _syms: &mut SymbolTable,
) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "grpc-request-peer-identity: expected 1 argument, got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-request-peer-identity")?;
    Ok(match primop_request_peer_identity(h)? {
        Some(id) => string_value(id),
        None => Value::Boolean(false),
    })
}

/// `(grpc-request-metadata h key)` → the gRPC metadata header value
/// string for `key` (case-insensitive; gRPC normalises header names to
/// lowercase), or `#f` if the call carried no such header. This is the
/// lone interpreter gap etcd-Auth (cw-u4a.26) needed: the `token`
/// header is `(grpc-request-metadata h "token")`. Mirrors
/// `grpc-request-peer-identity` (the cert-CN identity hook).
pub fn b_grpc_request_metadata(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "grpc-request-metadata: expected 2 arguments (handle key), got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-request-metadata")?;
    let key = value_to_str(&args[1], syms, "grpc-request-metadata")?;
    Ok(match primop_request_metadata(h, &key)? {
        Some(v) => string_value(v),
        None => Value::Boolean(false),
    })
}

pub fn b_grpc_respond(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "grpc-respond!: expected 2 arguments (handle response-bytes), got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-respond!")?;
    let bytes = value_to_bytes(&args[1], "grpc-respond!")?;
    primop_respond(h, bytes)?;
    Ok(Value::Unspecified)
}

pub fn b_grpc_respond_error(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(format!(
            "grpc-respond-error!: expected 3 arguments (handle status message), got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-respond-error!")?;
    let status = value_to_i64(&args[1], "grpc-respond-error!")?;
    if !(0..=u32::MAX as i64).contains(&status) {
        return Err(format!(
            "grpc-respond-error!: status {} out of range 0..4294967295",
            status
        ));
    }
    let message = value_to_str(&args[2], syms, "grpc-respond-error!")?;
    primop_respond_error(h, status as u32, message)?;
    Ok(Value::Unspecified)
}

pub fn b_grpc_stream_send(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "grpc-stream-send!: expected 2 arguments (handle response-bytes), got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-stream-send!")?;
    let bytes = value_to_bytes(&args[1], "grpc-stream-send!")?;
    Ok(Value::Boolean(primop_stream_send(h, bytes)?))
}

pub fn b_grpc_stream_close(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(format!(
            "grpc-stream-close!: expected 1 to 3 arguments (handle [status [message]]), got {}",
            args.len()
        ));
    }
    let h = value_to_i64(&args[0], "grpc-stream-close!")?;
    let status = if args.len() >= 2 {
        let s = value_to_i64(&args[1], "grpc-stream-close!")?;
        if !(0..=u32::MAX as i64).contains(&s) {
            return Err(format!(
                "grpc-stream-close!: status {} out of range 0..4294967295",
                s
            ));
        }
        s as u32
    } else {
        0
    };
    let message = if args.len() == 3 {
        Some(value_to_str(&args[2], syms, "grpc-stream-close!")?)
    } else {
        None
    };
    primop_stream_close(h, status, message)?;
    Ok(Value::Unspecified)
}

pub fn grpc_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    vec![
        ("grpc-serve", b_grpc_serve),
        ("grpc-serve-tls", b_grpc_serve_tls),
        ("grpc-server-stop", b_grpc_server_stop),
        ("grpc-request-path", b_grpc_request_path),
        ("grpc-request-bytes", b_grpc_request_bytes),
        ("grpc-request-peer-identity", b_grpc_request_peer_identity),
        ("grpc-request-metadata", b_grpc_request_metadata),
        ("grpc-respond!", b_grpc_respond),
        ("grpc-respond-error!", b_grpc_respond_error),
        ("grpc-stream-send!", b_grpc_stream_send),
        ("grpc-stream-close!", b_grpc_stream_close),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // The GrpcResponseSink is constructed by the transport (its inner
    // sender is private), so the Rust-side tests assert the tagged-pair
    // shapes the bridge hands Scheme — the bridge's contract. The full
    // begin/stream/respond round-trip is proven end-to-end by the
    // cs-web hyper-client integration test and the crab-watchstore
    // etcdctl proof.
    #[test]
    fn tagged_request_shape() {
        let sv = tagged("*grpc-request*", 7, None);
        match sv {
            SendableValue::Pair(head, tail) => {
                assert!(matches!(*head, SendableValue::Symbol(ref s) if s == "*grpc-request*"));
                match *tail {
                    SendableValue::Pair(id, rest) => {
                        assert!(matches!(*id, SendableValue::Fixnum(7)));
                        assert!(matches!(*rest, SendableValue::Null));
                    }
                    _ => panic!("tail not pair"),
                }
            }
            _ => panic!("not a pair"),
        }
    }

    #[test]
    fn tagged_stream_msg_shape() {
        let sv = tagged("*grpc-stream-msg*", 9, Some(vec![1, 2, 3]));
        match sv {
            SendableValue::Pair(head, tail) => {
                assert!(matches!(*head, SendableValue::Symbol(ref s) if s == "*grpc-stream-msg*"));
                match *tail {
                    SendableValue::Pair(id, rest) => {
                        assert!(matches!(*id, SendableValue::Fixnum(9)));
                        match *rest {
                            SendableValue::Pair(bv, nil) => {
                                assert!(
                                    matches!(*bv, SendableValue::ByteVector(ref b) if b == &[1,2,3])
                                );
                                assert!(matches!(*nil, SendableValue::Null));
                            }
                            _ => panic!("no bytevector"),
                        }
                    }
                    _ => panic!("tail not pair"),
                }
            }
            _ => panic!("not a pair"),
        }
    }

    #[test]
    fn bridge_ignores_foreign_payload() {
        let payload: Payload = Arc::new("not-a-grpc-request".to_string());
        assert!(try_intern_grpc_request(&payload).is_none());
    }
}
