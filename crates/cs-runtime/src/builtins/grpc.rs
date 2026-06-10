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
//! request becomes a mailbox message to a Scheme **actor**. The
//! actor's `(raw-receive)` loop sees `('*grpc-request* <handle>)`,
//! reads the method path + request bytes, dispatches, and ships the
//! response back through a respond primop.
//!
//! ## Surface
//!
//! ```ignore
//! ; Start an h2c gRPC server. `addr` is "host:port" (port 0 lets
//! ; the OS pick). `handler-pid` is a spawned actor that will
//! ; receive one ('*grpc-request* h) message per unary call.
//! ; Returns an integer server handle.
//! (grpc-serve "127.0.0.1:2379" handler-pid)        => 1
//! (grpc-serve "127.0.0.1:0"    handler-pid 5000)   => 2   ; opt. reply-timeout-ms
//!
//! ; Stop the server. Idempotent.
//! (grpc-server-stop sid)
//!
//! ; --- inside the handler actor, on ('*grpc-request* h) ---
//! (grpc-request-path  h)   => "/etcdserverpb.KV/Range"   ; the :path
//! (grpc-request-bytes h)   => #u8(...)                    ; de-framed request protobuf
//!
//! ; Reply OK (status 0) with response protobuf bytes. Frames the
//! ; response + sends the grpc-status: 0 trailer. Consumes h.
//! (grpc-respond! h response-bytevector)
//!
//! ; Reply with a non-OK gRPC status + grpc-message. No payload.
//! ; e.g. 5 = NOT_FOUND, 13 = INTERNAL, 14 = UNAVAILABLE.
//! (grpc-respond-error! h 5 "key not found")
//! ```
//!
//! ## How streaming (.23) extends this
//!
//! Unary delivers one `('*grpc-request* h)` and expects one
//! `grpc-respond!`. A server-streaming / bidi RPC keeps the same
//! delivery but swaps the single reply for a stream: the actor
//! would call a `grpc-stream-send!` primop repeatedly (each pushes
//! one framed message onto an mpsc-backed response body) then
//! `grpc-stream-close!` to flush the trailers. The transport seam
//! for that already exists in `cs_web::grpc` (see its module docs);
//! only the response-body source changes.

#![cfg(feature = "grpc")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use cs_actor::{ActorPid, Payload};
use cs_core::{SymbolTable, Value};

use cs_web::grpc::{
    bind_grpc, serve_grpc, ArcGrpcHandler, BoxFuture, GrpcHandler, GrpcReply, GrpcRequest,
};
use cs_web::Bytes;

use crate::builtins::beam::SendableValue;

// ---------------------------------------------------------------
// Envelope: a gRPC request crossing from hyper's task to a Scheme
// actor's mailbox, plus the reply channel back.
// ---------------------------------------------------------------

/// What a Scheme handler ships back through the reply channel.
struct GrpcReplyMsg {
    status: u32,
    message: Bytes,
    error: Option<String>,
}

/// One in-flight unary gRPC request. Mirrors `cs_web::actor::WebMessage`:
/// the request data plus a `Mutex<Option<oneshot::Sender>>` so the
/// envelope can cross the `Arc<dyn Any + Send + Sync>` payload
/// boundary (oneshot senders are `Send` but not `Sync`).
pub struct GrpcMessage {
    path: String,
    message: Bytes,
    reply: Mutex<Option<tokio::sync::oneshot::Sender<GrpcReplyMsg>>>,
}

impl GrpcMessage {
    fn reply_with(&self, msg: GrpcReplyMsg) -> bool {
        if let Some(tx) = self.reply.lock().expect("grpc reply lock poisoned").take() {
            tx.send(msg).is_ok()
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------
// Handler: sends each request to a Scheme actor and awaits reply.
// ---------------------------------------------------------------

/// A [`GrpcHandler`] that forwards every unary call to a Scheme
/// actor's mailbox and waits up to `timeout` for the actor to
/// respond. Slow / dead actors map to gRPC status codes the client
/// understands (DEADLINE_EXCEEDED / UNAVAILABLE / INTERNAL) rather
/// than a torn stream.
struct ActorGrpcHandler {
    target: cs_actor::ActorRef,
    timeout: Duration,
}

impl GrpcHandler for ActorGrpcHandler {
    fn call(&self, req: GrpcRequest) -> BoxFuture<'static, GrpcReply> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let envelope = Arc::new(GrpcMessage {
            path: req.path,
            message: req.message,
            reply: Mutex::new(Some(tx)),
        });
        let payload: Payload = envelope;
        let send_result = self.target.send(payload);
        let timeout = self.timeout;
        Box::pin(async move {
            if let Err(e) = send_result {
                // 14 = UNAVAILABLE — the handler actor's mailbox is
                // closed (terminated).
                return GrpcReply::error(14, format!("grpc handler actor send failed: {e}"));
            }
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(r)) => GrpcReply {
                    status: r.status,
                    message: r.message,
                    error: r.error,
                },
                // 13 = INTERNAL — actor dropped the reply channel
                // without responding.
                Ok(Err(_)) => GrpcReply::error(13, "grpc handler dropped reply channel"),
                // 4 = DEADLINE_EXCEEDED.
                Err(_) => GrpcReply::error(4, "grpc handler reply timeout"),
            }
        })
    }
}

// ---------------------------------------------------------------
// Registry: running servers + the request slab.
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
    /// In-flight request envelopes, keyed by the handle Scheme sees
    /// in `('*grpc-request* <handle>)`. `grpc-respond!` /
    /// `grpc-respond-error!` consume the entry.
    requests: HashMap<i64, Arc<GrpcMessage>>,
    next_request_id: i64,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next_id: 1,
            slots: HashMap::new(),
            requests: HashMap::new(),
            next_request_id: 1,
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
// (Kept separate from cs-web's so enabling `grpc` without `web`
// still works.)
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
        Value::Number(cs_core::Number::Fixnum(n)) => Ok(*n),
        other => Err(format!(
            "{}: expected fixnum, got {}",
            who,
            other.type_name()
        )),
    }
}

fn value_to_bytes(v: &Value, who: &str) -> Result<Bytes, String> {
    match v {
        Value::ByteVector(b) => Ok(Bytes::copy_from_slice(&b.borrow())),
        // A string is accepted as a convenience (UTF-8 bytes) — gRPC
        // payloads are usually bytevectors, but an echo of text is
        // handy.
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

fn primop_serve(addr: &str, pid: ActorPid, timeout_ms: u64) -> Result<(i64, String), String> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| format!("grpc-serve: invalid addr {:?}: {}", addr, e))?;
    let actor_ref = crate::builtins::beam::lookup_pid(pid)
        .ok_or_else(|| format!("grpc-serve: handler actor {} not found (terminated?)", pid))?;
    let handler: ArcGrpcHandler = Arc::new(ActorGrpcHandler {
        target: actor_ref,
        timeout: Duration::from_millis(timeout_ms),
    });

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
// Request bridge: intern an incoming GrpcMessage, expose its data,
// consume it on respond.
// ---------------------------------------------------------------

/// Called by `beam::message_to_sendable` when a User payload isn't
/// a plain `SendableValue`. If it's a [`GrpcMessage`], stash it and
/// return the tagged pair `('*grpc-request* <handle>)` the Scheme
/// actor pattern-matches. Returns `None` for other payloads so the
/// caller can keep trying (e.g. the web bridge).
pub fn try_intern_grpc_request(payload: &Payload) -> Option<SendableValue> {
    let msg: Arc<GrpcMessage> = Arc::clone(payload).downcast::<GrpcMessage>().ok()?;
    let mut reg = lock();
    let id = reg.next_request_id;
    reg.next_request_id += 1;
    reg.requests.insert(id, msg);
    Some(SendableValue::Pair(
        Box::new(SendableValue::Symbol("*grpc-request*".into())),
        Box::new(SendableValue::Pair(
            Box::new(SendableValue::Fixnum(id)),
            Box::new(SendableValue::Null),
        )),
    ))
}

fn with_request<R>(who: &str, handle: i64, f: impl FnOnce(&GrpcMessage) -> R) -> Result<R, String> {
    let reg = lock();
    let msg = reg.requests.get(&handle).ok_or_else(|| {
        format!(
            "{}: grpc request #{} not found (already responded?)",
            who, handle
        )
    })?;
    Ok(f(msg.as_ref()))
}

fn take_request(who: &str, handle: i64) -> Result<Arc<GrpcMessage>, String> {
    lock().requests.remove(&handle).ok_or_else(|| {
        format!(
            "{}: grpc request #{} not found (already responded?)",
            who, handle
        )
    })
}

fn primop_request_path(handle: i64) -> Result<String, String> {
    with_request("grpc-request-path", handle, |m| m.path.clone())
}

fn primop_request_bytes(handle: i64) -> Result<Vec<u8>, String> {
    with_request("grpc-request-bytes", handle, |m| m.message.to_vec())
}

fn primop_respond(handle: i64, message: Bytes) -> Result<(), String> {
    let msg = take_request("grpc-respond!", handle)?;
    if !msg.reply_with(GrpcReplyMsg {
        status: 0,
        message,
        error: None,
    }) {
        return Err("grpc-respond!: reply channel already consumed".into());
    }
    Ok(())
}

fn primop_respond_error(handle: i64, status: u32, message: String) -> Result<(), String> {
    let msg = take_request("grpc-respond-error!", handle)?;
    if !msg.reply_with(GrpcReplyMsg {
        status,
        message: Bytes::new(),
        error: Some(message),
    }) {
        return Err("grpc-respond-error!: reply channel already consumed".into());
    }
    Ok(())
}

// ---------------------------------------------------------------
// Scheme glue (host-fn shape `fn(&[Value], &mut SymbolTable) -> …`).
// ---------------------------------------------------------------

fn bytevector_value(bytes: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(bytes)))
}

fn string_value(s: String) -> Value {
    Value::String(cs_gc::Gc::new(std::cell::RefCell::new(s)))
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
    let timeout_ms = if args.len() == 3 {
        let n = value_to_i64(&args[2], "grpc-serve")?;
        if n < 0 {
            return Err("grpc-serve: timeout-ms must be non-negative".into());
        }
        n as u64
    } else {
        30_000
    };
    let (id, _bound) = primop_serve(&addr, pid, timeout_ms)?;
    Ok(Value::Number(cs_core::Number::Fixnum(id)))
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

pub fn grpc_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    vec![
        ("grpc-serve", b_grpc_serve),
        ("grpc-server-stop", b_grpc_server_stop),
        ("grpc-request-path", b_grpc_request_path),
        ("grpc-request-bytes", b_grpc_request_bytes),
        ("grpc-respond!", b_grpc_respond),
        ("grpc-respond-error!", b_grpc_respond_error),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a GrpcMessage envelope by hand and run it through the
    // bridge — no server needed. Proves the request slab + respond
    // path the Scheme actor uses.
    #[test]
    fn bridge_interns_and_responds() {
        let (tx, rx) = tokio::sync::oneshot::channel::<GrpcReplyMsg>();
        let envelope: Arc<GrpcMessage> = Arc::new(GrpcMessage {
            path: "/etcdserverpb.KV/Range".into(),
            message: Bytes::from_static(b"req-bytes"),
            reply: Mutex::new(Some(tx)),
        });
        let payload: Payload = envelope;

        let sv = try_intern_grpc_request(&payload).expect("bridge should match");
        let handle = match sv {
            SendableValue::Pair(head, tail) => {
                assert!(matches!(*head, SendableValue::Symbol(ref s) if s == "*grpc-request*"));
                match *tail {
                    SendableValue::Pair(boxed_id, _) => match *boxed_id {
                        SendableValue::Fixnum(n) => n,
                        _ => panic!("handle not fixnum"),
                    },
                    _ => panic!("tail not pair"),
                }
            }
            _ => panic!("not a pair"),
        };

        assert_eq!(
            primop_request_path(handle).unwrap(),
            "/etcdserverpb.KV/Range"
        );
        assert_eq!(primop_request_bytes(handle).unwrap(), b"req-bytes");

        // Respond OK; the slot is consumed.
        primop_respond(handle, Bytes::from_static(b"resp-bytes")).unwrap();
        let got = rx.blocking_recv().expect("reply");
        assert_eq!(got.status, 0);
        assert_eq!(&got.message[..], b"resp-bytes");

        // Second respond / inspect must error — slot was taken.
        assert!(primop_respond(handle, Bytes::new()).is_err());
        assert!(primop_request_path(handle).is_err());
    }

    #[test]
    fn bridge_error_reply() {
        let (tx, rx) = tokio::sync::oneshot::channel::<GrpcReplyMsg>();
        let envelope: Arc<GrpcMessage> = Arc::new(GrpcMessage {
            path: "/etcdserverpb.KV/Put".into(),
            message: Bytes::new(),
            reply: Mutex::new(Some(tx)),
        });
        let sv = try_intern_grpc_request(&(envelope as Payload)).expect("bridge");
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
        primop_respond_error(handle, 5, "key not found".into()).unwrap();
        let got = rx.blocking_recv().expect("reply");
        assert_eq!(got.status, 5);
        assert_eq!(got.error.as_deref(), Some("key not found"));
    }

    #[test]
    fn bridge_ignores_foreign_payload() {
        let payload: Payload = Arc::new("not-a-grpc-request".to_string());
        assert!(try_intern_grpc_request(&payload).is_none());
    }
}
