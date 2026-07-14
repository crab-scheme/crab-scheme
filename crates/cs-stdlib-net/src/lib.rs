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

// ----- cooperative async I/O hook (inverted dependency) -----
//
// cs-stdlib-net can't depend on the actor layer, so cs-runtime installs a hook
// (when the `actor` layer is present) that lets a coroutine driver service a
// socket read by *parking* its green worker instead of blocking it. Same shape
// as `cs_stdlib_time::install_cooperative_sleep`.

/// `(handle, max_len) -> Some(result)` if the cooperative path handled the read
/// (the hook decides, based on whether a coroutine driver is active on this
/// thread), or `None` to fall through to the blocking read. `Ok(empty)` on a
/// clean EOF, matching the blocking path's contract.
type AsyncRecvHook = fn(i64, usize) -> Option<Result<Vec<u8>, String>>;
static ASYNC_RECV: OnceLock<AsyncRecvHook> = OnceLock::new();

/// Install the cooperative async-recv hook (idempotent; first wins). Called by
/// cs-runtime at startup when the actor layer is built.
pub fn install_async_recv(hook: AsyncRecvHook) {
    let _ = ASYNC_RECV.set(hook);
}

/// `(handle, bytes) -> Some(result)` if the cooperative path handled the write
/// (parked the green worker), or `None` to fall through to the blocking write.
type AsyncSendHook = fn(i64, &[u8]) -> Option<Result<(), String>>;
static ASYNC_SEND: OnceLock<AsyncSendHook> = OnceLock::new();

/// Install the cooperative async-send hook (idempotent; first wins).
pub fn install_async_send(hook: AsyncSendHook) {
    let _ = ASYNC_SEND.set(hook);
}

/// Cooperative-blocking hook for `dns-resolve` / `tcp-connect` (cs-845.3):
/// these do real (possibly slow) DNS + connect syscalls that, run inline on
/// a green actor's shared worker, would freeze every co-tenant actor. Same
/// generic erased-closure shape as `cs_ffi::blocking` — see that module's
/// docs. `None` (hook not installed, e.g. no actor layer) means the caller
/// runs the op as a plain blocking call.
static COOPERATIVE_BLOCKING: OnceLock<cs_ffi::blocking::BlockingHook> = OnceLock::new();

/// Install the cooperative-blocking hook (idempotent; first wins). Called by
/// cs-runtime at startup when the actor layer is built.
pub fn install_cooperative_blocking(hook: cs_ffi::blocking::BlockingHook) {
    let _ = COOPERATIVE_BLOCKING.set(hook);
}

/// Clone the underlying std `TcpStream` for socket handle `id`, so a cooperative
/// driver can build a tokio stream for the async read. `None` if `id` is not a
/// live TCP socket. No tokio types cross this crate boundary.
pub fn clone_tcp_std(id: i64) -> Option<std::net::TcpStream> {
    let r = lock().ok()?;
    match r.slots.get(&id) {
        Some(Slot::Tcp(s)) => s.try_clone().ok(),
        _ => None,
    }
}

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("dns-resolve", dns_resolve),
        UntypedProc::new("tcp-connect", tcp_connect),
        UntypedProc::new("tcp-listen", tcp_listen),
        UntypedProc::new("tcp-accept", tcp_accept),
        UntypedProc::new("tcp-send", tcp_send),
        UntypedProc::new("tcp-recv", tcp_recv),
        UntypedProc::new("tcp-close", tcp_close),
        UntypedProc::new("udp-bind", udp_bind),
        UntypedProc::new("udp-send-to", udp_send_to),
        UntypedProc::new("udp-recv-from", udp_recv_from),
        UntypedProc::new("udp-close", udp_close),
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
        Some(Value::Fixnum(v)) => Ok(*v),
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
    Value::string(s)
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
    let host_has_port = host.contains(':');
    let hook = COOPERATIVE_BLOCKING.get().copied();
    let lookup_host = host;
    let iter: Vec<std::net::SocketAddr> = cs_ffi::blocking::run_blocking(hook, move || {
        // `to_socket_addrs` accepts either "host:port" or bare "host" (the
        // latter needs a synthetic port; std requires one). Try bare first;
        // if that fails, append ":0".
        let addrs = if host_has_port {
            lookup_host.to_socket_addrs()
        } else {
            format!("{}:0", lookup_host).to_socket_addrs()
        };
        addrs
            .map(|it| it.collect::<Vec<_>>())
            .map_err(|e| format!("dns-resolve: {}: {}", lookup_host, e))
    })
    .map_err(FfiError::HostFailure)?;
    let strings: Vec<Value> = iter
        .into_iter()
        .map(|sa| {
            // Strip the dummy `:0` if we added it; otherwise keep
            // the host:port shape so callers can distinguish IPv4
            // vs IPv6 mapped addresses with explicit ports.
            if host_has_port {
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
    let hook = COOPERATIVE_BLOCKING.get().copied();
    let s = cs_ffi::blocking::run_blocking(hook, move || {
        TcpStream::connect(&addr).map_err(|e| format!("tcp-connect: {}", e))
    })
    .map_err(FfiError::HostFailure)?;
    Ok(Value::fixnum(insert(Slot::Tcp(s))?))
}

fn tcp_listen(args: &[Value]) -> Result<Value, FfiError> {
    // #9 iter-2 — wasi:sockets 0.2 (wasm32-wasip2) doesn't standardize
    // socket creation, so passive sockets can't be made portably. Raise
    // a clear error instead of letting a std::net call's wasi behavior
    // leak through. For HTTP servers, use the wasi:http/incoming-handler
    // shape that iter-5 exposes.
    #[cfg(target_os = "wasi")]
    {
        let _ = args;
        return Err(FfiError::HostFailure(
            "tcp-listen: not supported on wasm32-wasi — wasi:sockets 0.2 does \
             not standardize socket creation; use a host-driven accept loop \
             (e.g. wasi:http/incoming-handler for HTTP)"
                .into(),
        ));
    }
    #[cfg(not(target_os = "wasi"))]
    {
        let host = expect_string("tcp-listen", args, 0)?;
        let port = expect_fixnum("tcp-listen", args, 1)?;
        let addr = format!("{}:{}", host, port);
        let l = TcpListener::bind(&addr).map_err(|e| io_fail("tcp-listen", e))?;
        Ok(Value::fixnum(insert(Slot::TcpListener(l))?))
    }
}

fn tcp_accept(args: &[Value]) -> Result<Value, FfiError> {
    // #9 iter-2 — paired with `tcp-listen`'s wasi gate above. No
    // listener can be produced on wasi, so reject explicitly rather than
    // bottoming out as a "bad handle" message.
    #[cfg(target_os = "wasi")]
    {
        let _ = args;
        return Err(FfiError::HostFailure(
            "tcp-accept: not supported on wasm32-wasi (paired with \
             tcp-listen; see that error for the wasi:sockets 0.2 gap)"
                .into(),
        ));
    }
    #[cfg(not(target_os = "wasi"))]
    {
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
}

fn tcp_send(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tcp-send", args, 0)?;
    let payload = expect_bv("tcp-send", args, 1)?;
    // Cooperative path (see tcp_recv): a green driver writes by parking instead
    // of blocking the shared worker. None ⇒ no driver ⇒ blocking path below
    // (unchanged). This must shadow the blocking write for green conns: the
    // cooperative recv put this fd in nonblocking mode (shared file description),
    // so a blocking write_all here would hit WouldBlock.
    if let Some(hook) = ASYNC_SEND.get() {
        if let Some(res) = hook(id, &payload) {
            return res
                .map(|()| Value::Unspecified)
                .map_err(FfiError::HostFailure);
        }
    }
    // Clone the stream handle (a dup of the same fd) and RELEASE the registry
    // lock before the blocking write. Holding the global lock across the
    // syscall would serialize every socket in the process onto one mutex —
    // fatal for a concurrent server (e.g. redis-benchmark's many connections).
    // `tcp-accept` / the UDP ops already follow this clone-then-unlock shape.
    let mut stream = {
        let r = lock()?;
        match r.slots.get(&id) {
            Some(Slot::Tcp(s)) => s.try_clone().map_err(|e| io_fail("tcp-send", e))?,
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
    // Cooperative path: if a coroutine driver installed the async-recv hook and
    // it claims this read (we're inside a green driver on this thread), let it
    // park instead of blocking the shared worker. `None` ⇒ no driver ⇒ fall
    // through to the blocking read below (dedicated thread / non-actor —
    // unchanged). `Ok(empty)` from the hook is a clean EOF, same as below.
    if let Some(hook) = ASYNC_RECV.get() {
        if let Some(res) = hook(id, max_len as usize) {
            return res.map(bv_value).map_err(FfiError::HostFailure);
        }
    }
    let mut buf = vec![0u8; max_len as usize];
    // Clone + release the lock before the blocking read (see tcp-send): a
    // recv that blocks waiting for the next client request must not hold the
    // global registry mutex, or it freezes every other socket in the process.
    let mut stream = {
        let r = lock()?;
        match r.slots.get(&id) {
            Some(Slot::Tcp(s)) => s.try_clone().map_err(|e| io_fail("tcp-recv", e))?,
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
        }
    };
    let n = stream.read(&mut buf).map_err(|e| io_fail("tcp-recv", e))?;
    buf.truncate(n);
    Ok(bv_value(buf))
}

// Close a TCP handle (stream or listener). Refuses to close UDP
// handles so a typo like `(tcp-close udp-handle)` doesn't silently
// drop the wrong socket.
fn tcp_close(args: &[Value]) -> Result<Value, FfiError> {
    close_kind(
        "tcp-close",
        args,
        |s| matches!(s, Slot::Tcp(_) | Slot::TcpListener(_)),
        "tcp socket or listener",
    )
}

// Close a UDP handle. Refuses TCP/listener handles for the same
// reason as tcp_close.
fn udp_close(args: &[Value]) -> Result<Value, FfiError> {
    close_kind(
        "udp-close",
        args,
        |s| matches!(s, Slot::Udp(_)),
        "udp socket",
    )
}

fn close_kind(
    name: &str,
    args: &[Value],
    matches_kind: impl Fn(&Slot) -> bool,
    expected: &str,
) -> Result<Value, FfiError> {
    let id = expect_fixnum(name, args, 0)?;
    let mut r = lock()?;
    match r.slots.get(&id) {
        Some(s) if matches_kind(s) => {
            r.slots.remove(&id); // Drop here closes the underlying socket.
            Ok(Value::Unspecified)
        }
        Some(_) => Err(FfiError::HostFailure(format!(
            "{}: handle {} is not a {}",
            name, id, expected
        ))),
        None => Err(FfiError::HostFailure(format!(
            "{}: handle {} not registered",
            name, id
        ))),
    }
}

// ----- UDP -----

fn udp_bind(args: &[Value]) -> Result<Value, FfiError> {
    // #9 iter-2 — same wasi:sockets 0.2 gap as tcp-listen: wasi sockets
    // 0.2 doesn't standardize socket creation, so UdpSocket::bind on
    // wasm32-wasi is unreliable. Raise explicitly.
    #[cfg(target_os = "wasi")]
    {
        let _ = args;
        return Err(FfiError::HostFailure(
            "udp-bind: not supported on wasm32-wasi — wasi:sockets 0.2 does \
             not standardize socket creation"
                .into(),
        ));
    }
    #[cfg(not(target_os = "wasi"))]
    {
        let host = expect_string("udp-bind", args, 0)?;
        let port = expect_fixnum("udp-bind", args, 1)?;
        let addr = format!("{}:{}", host, port);
        let s = UdpSocket::bind(&addr).map_err(|e| io_fail("udp-bind", e))?;
        Ok(Value::fixnum(insert(Slot::Udp(s))?))
    }
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
