//! CrabScheme stdlib module: `(crab http client + server)`.
//!
//! Synchronous HTTP via `ureq` (client) and `tiny_http` (server).
//! Iter 10 (client) + iter 11 (server) of the `stdlib-modules`
//! spec. Supersedes the example `cs-ffi-http` crate.
//!
//! See:
//! - [`client`] — `http-get` / `http-post` / etc.
//! - [`server`] — `http-server-bind` / `http-server-accept` /
//!   `http-respond` etc.

use std::sync::Arc;

use cs_ffi::host::HostProcedure;

mod client;
mod server;

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    let mut all = client::procs();
    all.extend(server::procs());
    all
}
