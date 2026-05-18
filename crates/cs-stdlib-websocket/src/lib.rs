//! CrabScheme stdlib module: `(crab websocket)`.
//!
//! Synchronous WebSocket client via `tungstenite`. Iter 10 of the
//! `stdlib-modules` spec.
//!
//! Connections are exposed as fixnum handles indexing a
//! process-global slab (same pattern as `cs-stdlib-net`).
//! Blocking calls; for concurrent connections, drive from BEAM
//! actors.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns |
//! |---|---|---|
//! | `ws-connect`     | url                | handle |
//! | `ws-send-text`   | handle string      | unspec |
//! | `ws-send-binary` | handle bv          | unspec |
//! | `ws-recv`        | handle             | (kind . payload) — kind is `"text"`, `"binary"`, `"ping"`, `"pong"`, `"close"`; payload is string for text/close, bytevector for the rest |
//! | `ws-close`       | handle             | unspec |

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::{Pair, Value};
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Conn = WebSocket<MaybeTlsStream<TcpStream>>;

struct Registry {
    next_id: i64,
    slots: HashMap<i64, Conn>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| {
        Mutex::new(Registry {
            next_id: 1,
            slots: HashMap::new(),
        })
    })
}

fn lock() -> Result<std::sync::MutexGuard<'static, Registry>, FfiError> {
    registry()
        .lock()
        .map_err(|e| FfiError::HostFailure(format!("ws: registry poisoned: {}", e)))
}

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("ws-connect", ws_connect),
        UntypedProc::new("ws-send-text", ws_send_text),
        UntypedProc::new("ws-send-binary", ws_send_binary),
        UntypedProc::new("ws-recv", ws_recv),
        UntypedProc::new("ws-close", ws_close),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_fixnum(name: &str, args: &[Value], idx: usize) -> Result<i64, FfiError> {
    match args.get(idx) {
        Some(Value::Number(cs_core::Number::Fixnum(v))) => Ok(*v),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "fixnum",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn expect_bv(name: &str, args: &[Value], idx: usize) -> Result<Vec<u8>, FfiError> {
    match args.get(idx) {
        Some(Value::ByteVector(bv)) => Ok(bv.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "bytevector",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

fn pair(k: &str, v: Value) -> Value {
    Value::Pair(Pair::new(string_value(k.to_string()), v))
}

fn ws_fail(op: &str, e: tungstenite::Error) -> FfiError {
    FfiError::HostFailure(format!("ws-{}: {}", op, e))
}

// ----- procedures -----

fn ws_connect(args: &[Value]) -> Result<Value, FfiError> {
    let url = expect_string("ws-connect", args, 0)?;
    let (socket, _resp) = tungstenite::connect(&url).map_err(|e| ws_fail("connect", e))?;
    let mut r = lock()?;
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, socket);
    Ok(Value::fixnum(id))
}

fn ws_send_text(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("ws-send-text", args, 0)?;
    let text = expect_string("ws-send-text", args, 1)?;
    let mut r = lock()?;
    let sock = r
        .slots
        .get_mut(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("ws-send-text: bad handle {}", id)))?;
    sock.send(Message::Text(text))
        .map_err(|e| ws_fail("send-text", e))?;
    Ok(Value::Unspecified)
}

fn ws_send_binary(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("ws-send-binary", args, 0)?;
    let payload = expect_bv("ws-send-binary", args, 1)?;
    let mut r = lock()?;
    let sock = r
        .slots
        .get_mut(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("ws-send-binary: bad handle {}", id)))?;
    sock.send(Message::Binary(payload))
        .map_err(|e| ws_fail("send-binary", e))?;
    Ok(Value::Unspecified)
}

fn ws_recv(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("ws-recv", args, 0)?;
    let mut r = lock()?;
    let sock = r
        .slots
        .get_mut(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("ws-recv: bad handle {}", id)))?;
    let msg = sock.read().map_err(|e| ws_fail("recv", e))?;
    Ok(match msg {
        Message::Text(s) => pair("text", string_value(s)),
        Message::Binary(b) => pair("binary", bv_value(b)),
        Message::Ping(b) => pair("ping", bv_value(b)),
        Message::Pong(b) => pair("pong", bv_value(b)),
        Message::Close(frame) => {
            let reason = frame.map(|f| f.reason.into_owned()).unwrap_or_default();
            pair("close", string_value(reason))
        }
        Message::Frame(_) => pair("binary", bv_value(Vec::new())),
    })
}

fn ws_close(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("ws-close", args, 0)?;
    let mut r = lock()?;
    let sock = r
        .slots
        .remove(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("ws-close: bad handle {}", id)))?;
    // Best-effort close handshake; drop the socket either way.
    let mut sock = sock;
    let _ = sock.close(None);
    Ok(Value::Unspecified)
}
