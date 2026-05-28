//! CrabScheme stdlib module: `(crab http server)`.
//!
//! Back-end split (#9 iter-3 client; iter-5 wires the wasi server).
//! The native build uses `tiny_http` (sync; the accept loop blocks —
//! drive concurrency from BEAM actors). The `wasm32-wasip2` build
//! presents the same proc names, but they raise `HostFailure` until
//! iter-5 binds the `wasi:http/incoming-handler` world (see ADR 0033).
//! Implementations live in [`server_native`] / [`server_wasi_stub`].

#[cfg(not(target_os = "wasi"))]
mod server_native;
#[cfg(not(target_os = "wasi"))]
pub(crate) use server_native::procs;

#[cfg(target_os = "wasi")]
mod server_wasi_stub;
#[cfg(target_os = "wasi")]
pub(crate) use server_wasi_stub::procs;
