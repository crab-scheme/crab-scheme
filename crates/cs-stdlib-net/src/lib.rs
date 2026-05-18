//! CrabScheme stdlib module: `(crab net …)`.
//!
//! TCP / UDP / DNS via `std::net`. Iter 9 of the `stdlib-modules`
//! spec.
//!
//! Sockets are exposed as opaque **fixnum handles** that index into
//! a process-global slab — same approach used by `cs-actor` for
//! actor pids and `cs-stdlib-metrics` for the metric registry.
//! Each `tcp-connect` / `tcp-listen` / `udp-bind` registers a slot
//! and returns the fixnum; subsequent ops pass the fixnum back.
//! `close` drops the slot. Typed socket values (with `socket?`
//! predicate, Drop semantics) land when `Value::Opaque` does.
//!
//! All operations are synchronous and block the calling thread.
//! For concurrency, drive them from BEAM actors.
//!
//! ## Registered procedures
//!
//! ### DNS
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `dns-resolve` | string | list of strings | Each entry is `ip:port` if a port was given, else `ip`. |
//!
//! ### TCP
//!
//! | Scheme name | Args | Returns |
//! |---|---|---|
//! | `tcp-connect`   | host port              | socket handle |
//! | `tcp-listen`    | host port              | listener handle |
//! | `tcp-accept`    | listener-handle        | socket handle |
//! | `tcp-send`      | socket-handle bv       | unspec |
//! | `tcp-recv`      | socket-handle max-len  | bytevector (≤ max-len; empty on clean EOF) |
//! | `tcp-close`     | handle                 | unspec |
//!
//! ### UDP
//!
//! | Scheme name | Args | Returns |
//! |---|---|---|
//! | `udp-bind`      | host port               | socket handle |
//! | `udp-send-to`   | handle bv host port     | unspec |
//! | `udp-recv-from` | handle max-len          | (bv source-host source-port) |
//! | `udp-close`     | handle                  | unspec |

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

// ----- handle registry -----

enum Slot {
    Tcp(TcpStream),
    TcpListener(TcpListener),
    Udp(UdpSocket),
}

struct Registry {
    next_id: i64,
    slots: HashMap<i64, Slot>,
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
        .map_err(|e| FfiError::HostFailure(format!("net: registry poisoned: {}", e)))
}

fn insert(slot: Slot) -> Result<i64, FfiError> {
    let mut r = lock()?;
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, slot);
    Ok(id)
}

fn remove(id: i64) -> Result<Slot, FfiError> {
    let mut r = lock()?;
    r.slots
        .remove(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("net: handle {} not registered", id)))
}

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("dns-resolve", dns_resolve),
        UntypedProc::new("tcp-connect", tcp_connect),
        UntypedProc::new("tcp-listen", tcp_listen),
        UntypedProc::new("tcp-accept", tcp_accept),
        UntypedProc::new("tcp-send", tcp_send),
        UntypedProc::new("tcp-recv", tcp_recv),
        UntypedProc::new("tcp-close", net_close),
        UntypedProc::new("udp-bind", udp_bind),
        UntypedProc::new("udp-send-to", udp_send_to),
        UntypedProc::new("udp-recv-from", udp_recv_from),
        UntypedProc::new("udp-close", net_close),
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
        Some(Value::Number(n)) => Ok(n.to_f64() as i64),
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

fn io_fail(name: &str, e: std::io::Error) -> FfiError {
    FfiError::HostFailure(format!("{}: {}", name, e))
}

// ----- DNS -----

fn dns_resolve(args: &[Value]) -> Result<Value, FfiError> {
    let host = expect_string("dns-resolve", args, 0)?;
    // `to_socket_addrs` accepts either "host:port" or bare "host"
    // (the latter needs a synthetic port; std requires one). Try
    // bare first; if that fails, append ":0".
    let addrs = if host.contains(':') {
        host.to_socket_addrs()
    } else {
        format!("{}:0", host).to_socket_addrs()
    };
    let iter = addrs.map_err(|e| FfiError::HostFailure(format!("dns-resolve: {}: {}", host, e)))?;
    let strings: Vec<Value> = iter
        .map(|sa| {
            // Strip the dummy `:0` if we added it; otherwise keep
            // the host:port shape so callers can distinguish IPv4
            // vs IPv6 mapped addresses with explicit ports.
            if host.contains(':') {
                string_value(sa.to_string())
            } else {
                string_value(sa.ip().to_string())
            }
        })
        .collect();
    Ok(Value::list(strings))
}

// ----- TCP -----

fn tcp_connect(args: &[Value]) -> Result<Value, FfiError> {
    let host = expect_string("tcp-connect", args, 0)?;
    let port = expect_fixnum("tcp-connect", args, 1)?;
    let addr = format!("{}:{}", host, port);
    let s = TcpStream::connect(&addr).map_err(|e| io_fail("tcp-connect", e))?;
    Ok(Value::fixnum(insert(Slot::Tcp(s))?))
}

fn tcp_listen(args: &[Value]) -> Result<Value, FfiError> {
    let host = expect_string("tcp-listen", args, 0)?;
    let port = expect_fixnum("tcp-listen", args, 1)?;
    let addr = format!("{}:{}", host, port);
    let l = TcpListener::bind(&addr).map_err(|e| io_fail("tcp-listen", e))?;
    Ok(Value::fixnum(insert(Slot::TcpListener(l))?))
}

fn tcp_accept(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tcp-accept", args, 0)?;
    let r = lock()?;
    let listener = match r.slots.get(&id) {
        Some(Slot::TcpListener(l)) => l.try_clone().map_err(|e| io_fail("tcp-accept", e))?,
        Some(_) => {
            return Err(FfiError::HostFailure(format!(
                "tcp-accept: handle {} is not a TCP listener",
                id
            )))
        }
        None => {
            return Err(FfiError::HostFailure(format!(
                "tcp-accept: bad handle {}",
                id
            )))
        }
    };
    drop(r);
    let (stream, _peer) = listener.accept().map_err(|e| io_fail("tcp-accept", e))?;
    Ok(Value::fixnum(insert(Slot::Tcp(stream))?))
}

fn tcp_send(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tcp-send", args, 0)?;
    let payload = expect_bv("tcp-send", args, 1)?;
    let mut r = lock()?;
    let stream = match r.slots.get_mut(&id) {
        Some(Slot::Tcp(s)) => s,
        Some(_) => {
            return Err(FfiError::HostFailure(format!(
                "tcp-send: handle {} is not a TCP socket",
                id
            )))
        }
        None => {
            return Err(FfiError::HostFailure(format!(
                "tcp-send: bad handle {}",
                id
            )))
        }
    };
    stream
        .write_all(&payload)
        .map_err(|e| io_fail("tcp-send", e))?;
    stream.flush().map_err(|e| io_fail("tcp-send", e))?;
    Ok(Value::Unspecified)
}

fn tcp_recv(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tcp-recv", args, 0)?;
    let max_len = expect_fixnum("tcp-recv", args, 1)?;
    if max_len <= 0 {
        return Err(FfiError::HostFailure(
            "tcp-recv: max-len must be positive".into(),
        ));
    }
    let mut buf = vec![0u8; max_len as usize];
    let mut r = lock()?;
    let stream = match r.slots.get_mut(&id) {
        Some(Slot::Tcp(s)) => s,
        Some(_) => {
            return Err(FfiError::HostFailure(format!(
                "tcp-recv: handle {} is not a TCP socket",
                id
            )))
        }
        None => {
            return Err(FfiError::HostFailure(format!(
                "tcp-recv: bad handle {}",
                id
            )))
        }
    };
    let n = stream.read(&mut buf).map_err(|e| io_fail("tcp-recv", e))?;
    buf.truncate(n);
    Ok(bv_value(buf))
}

fn net_close(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("close", args, 0)?;
    let _ = remove(id)?; // Drop runs here, closes the underlying socket.
    Ok(Value::Unspecified)
}

// ----- UDP -----

fn udp_bind(args: &[Value]) -> Result<Value, FfiError> {
    let host = expect_string("udp-bind", args, 0)?;
    let port = expect_fixnum("udp-bind", args, 1)?;
    let addr = format!("{}:{}", host, port);
    let s = UdpSocket::bind(&addr).map_err(|e| io_fail("udp-bind", e))?;
    Ok(Value::fixnum(insert(Slot::Udp(s))?))
}

fn udp_send_to(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("udp-send-to", args, 0)?;
    let payload = expect_bv("udp-send-to", args, 1)?;
    let host = expect_string("udp-send-to", args, 2)?;
    let port = expect_fixnum("udp-send-to", args, 3)?;
    let dst = format!("{}:{}", host, port);
    let r = lock()?;
    let sock = match r.slots.get(&id) {
        Some(Slot::Udp(s)) => s.try_clone().map_err(|e| io_fail("udp-send-to", e))?,
        Some(_) => {
            return Err(FfiError::HostFailure(format!(
                "udp-send-to: handle {} is not a UDP socket",
                id
            )))
        }
        None => {
            return Err(FfiError::HostFailure(format!(
                "udp-send-to: bad handle {}",
                id
            )))
        }
    };
    drop(r);
    sock.send_to(&payload, &dst)
        .map_err(|e| io_fail("udp-send-to", e))?;
    Ok(Value::Unspecified)
}

fn udp_recv_from(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("udp-recv-from", args, 0)?;
    let max_len = expect_fixnum("udp-recv-from", args, 1)?;
    if max_len <= 0 {
        return Err(FfiError::HostFailure(
            "udp-recv-from: max-len must be positive".into(),
        ));
    }
    let r = lock()?;
    let sock = match r.slots.get(&id) {
        Some(Slot::Udp(s)) => s.try_clone().map_err(|e| io_fail("udp-recv-from", e))?,
        Some(_) => {
            return Err(FfiError::HostFailure(format!(
                "udp-recv-from: handle {} is not a UDP socket",
                id
            )))
        }
        None => {
            return Err(FfiError::HostFailure(format!(
                "udp-recv-from: bad handle {}",
                id
            )))
        }
    };
    drop(r);
    let mut buf = vec![0u8; max_len as usize];
    let (n, src) = sock
        .recv_from(&mut buf)
        .map_err(|e| io_fail("udp-recv-from", e))?;
    buf.truncate(n);
    Ok(Value::list(vec![
        bv_value(buf),
        string_value(src.ip().to_string()),
        Value::fixnum(src.port() as i64),
    ]))
}
