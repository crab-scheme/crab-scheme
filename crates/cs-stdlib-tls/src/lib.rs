//! CrabScheme stdlib module: `(crab tls …)`.
//!
//! A blocking TLS **client** built on rustls — the secure-transport
//! companion to `(crab net)`'s raw TCP. `tls-connect` performs the TCP
//! connect plus the TLS handshake (validating the server certificate
//! against the Mozilla CA bundle from `webpki-roots`) and returns an
//! opaque fixnum handle into a process-global slab, exactly like
//! `(crab net)`. `tls-send` / `tls-recv` move bytes over the encrypted
//! stream; `tls-close` drops it.
//!
//! All operations are synchronous and block the calling thread. For
//! concurrency, drive them from BEAM actors — same as `(crab net)`.
//!
//! Native-only: rustls + std::net don't apply to the wasm builds, so
//! this module is excluded from `wasm-stdlib` (like `(crab sql)`).
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns |
//! |---|---|---|
//! | `tls-connect` | host port             | tls handle |
//! | `tls-send`    | tls-handle bytevector | unspec |
//! | `tls-recv`    | tls-handle max-len    | bytevector (≤ max-len; empty on clean close) |
//! | `tls-close`   | tls-handle            | unspec |
//!
//! ```scheme
//! (import (crab tls))
//! (define h (tls-connect "example.com" 443))
//! (tls-send h (string->utf8 "GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n"))
//! (utf8->string (tls-recv h 4096))
//! (tls-close h)
//! ```

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("tls-connect", tls_connect),
        UntypedProc::new("tls-send", tls_send),
        UntypedProc::new("tls-recv", tls_recv),
        UntypedProc::new("tls-close", tls_close),
    ]
}

// ----- handle registry (mirror cs-stdlib-net's fixnum slab) -----

type TlsStream = StreamOwned<ClientConnection, TcpStream>;

struct Registry {
    next_id: i64,
    slots: HashMap<i64, TlsStream>,
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
        .map_err(|e| FfiError::HostFailure(format!("tls: registry poisoned: {}", e)))
}

fn insert(stream: TlsStream) -> Result<i64, FfiError> {
    let mut r = lock()?;
    let id = r.next_id;
    r.next_id += 1;
    r.slots.insert(id, stream);
    Ok(id)
}

// ----- shared client config (built once, then cached) -----

fn build_config() -> Result<ClientConfig, FfiError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Ok(
        ClientConfig::builder_with_provider(rustls::crypto::ring::default_provider().into())
            .with_safe_default_protocol_versions()
            .map_err(|e| FfiError::HostFailure(format!("tls: provider init: {}", e)))?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn client_config() -> Result<Arc<ClientConfig>, FfiError> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(c) = CFG.get() {
        return Ok(c.clone());
    }
    let cfg = Arc::new(build_config()?);
    // First writer wins; a racing thread's identical config is dropped.
    let _ = CFG.set(cfg.clone());
    Ok(cfg)
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

fn bv_value(b: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(b)))
}

fn io_fail(name: &str, e: std::io::Error) -> FfiError {
    FfiError::HostFailure(format!("{}: {}", name, e))
}

// ----- procedures -----

fn tls_connect(args: &[Value]) -> Result<Value, FfiError> {
    let host = expect_string("tls-connect", args, 0)?;
    let port = expect_fixnum("tls-connect", args, 1)?;
    let config = client_config()?;
    // SNI / certificate validation use `host` as the server name.
    let server_name = ServerName::try_from(host.clone()).map_err(|e| {
        FfiError::HostFailure(format!(
            "tls-connect: invalid server name {:?}: {}",
            host, e
        ))
    })?;
    let mut conn = ClientConnection::new(config, server_name)
        .map_err(|e| FfiError::HostFailure(format!("tls-connect: {}", e)))?;
    let addr = format!("{}:{}", host, port);
    let mut sock = TcpStream::connect(&addr).map_err(|e| io_fail("tls-connect", e))?;
    // Drive the handshake to completion so certificate / SNI / connection
    // errors surface here rather than on the first tls-send. On a blocking
    // socket each complete_io call runs a full flight; the progress guard
    // turns a peer that closes mid-handshake into a clear error instead of
    // a spin.
    while conn.is_handshaking() {
        let (rd, wr) = conn
            .complete_io(&mut sock)
            .map_err(|e| io_fail("tls-connect (handshake)", e))?;
        if rd == 0 && wr == 0 && conn.is_handshaking() {
            return Err(FfiError::HostFailure(
                "tls-connect: handshake stalled (peer closed the connection?)".into(),
            ));
        }
    }
    Ok(Value::fixnum(insert(StreamOwned::new(conn, sock))?))
}

fn tls_send(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tls-send", args, 0)?;
    let payload = expect_bv("tls-send", args, 1)?;
    let mut r = lock()?;
    let stream = r
        .slots
        .get_mut(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("tls-send: bad handle {}", id)))?;
    stream
        .write_all(&payload)
        .map_err(|e| io_fail("tls-send", e))?;
    stream.flush().map_err(|e| io_fail("tls-send", e))?;
    Ok(Value::Unspecified)
}

fn tls_recv(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tls-recv", args, 0)?;
    let max_len = expect_fixnum("tls-recv", args, 1)?;
    if max_len <= 0 {
        return Err(FfiError::HostFailure(
            "tls-recv: max-len must be positive".into(),
        ));
    }
    let mut buf = vec![0u8; max_len as usize];
    let mut r = lock()?;
    let stream = r
        .slots
        .get_mut(&id)
        .ok_or_else(|| FfiError::HostFailure(format!("tls-recv: bad handle {}", id)))?;
    // rustls returns Ok(0) on a clean close_notify — mirror tcp-recv's
    // empty-bytevector-means-EOF contract.
    let n = stream.read(&mut buf).map_err(|e| io_fail("tls-recv", e))?;
    buf.truncate(n);
    Ok(bv_value(buf))
}

fn tls_close(args: &[Value]) -> Result<Value, FfiError> {
    let id = expect_fixnum("tls-close", args, 0)?;
    let mut r = lock()?;
    if r.slots.remove(&id).is_some() {
        // Dropping the StreamOwned closes the underlying TCP socket.
        Ok(Value::Unspecified)
    } else {
        Err(FfiError::HostFailure(format!(
            "tls-close: handle {} not registered",
            id
        )))
    }
}
