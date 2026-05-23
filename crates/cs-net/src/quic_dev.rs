//! Dev-mode QUIC listen/connect, using the shared self-signed mTLS identity
//! from [`crate::tls::dev`]. QUIC mandates TLS 1.3, so this transport is always
//! encrypted + mutually authenticated.
//!
//! These wrap the quinn [`Endpoint`] (and keep it alive for the lifetime of its
//! connections — dropping an `Endpoint` closes them) so a consumer can drive
//! QUIC without naming any quinn types: it holds an opaque [`DevListener`] and
//! gets back [`QuicTransport`]s. Dev/test only — see [`crate::tls::dev`] on the
//! shared identity.

use std::net::SocketAddr;
use std::sync::OnceLock;

use quinn::Endpoint;

use crate::quic::QuicTransport;
use crate::{TransportConfig, TransportError};

/// A QUIC listener. Owns the server [`Endpoint`], keeping it (and its accepted
/// connections) alive for as long as the listener lives.
#[derive(Debug)]
pub struct DevListener {
    ep: Endpoint,
}

impl DevListener {
    /// The actual bound address (use `"127.0.0.1:0"` to pick an ephemeral port).
    pub fn local_addr(&self) -> Result<String, TransportError> {
        Ok(self.ep.local_addr()?.to_string())
    }

    /// Accept the next inbound QUIC connection, wrapping it as a transport
    /// (per-channel streams). Returns once the QUIC + mTLS handshake completes.
    pub async fn accept(&self, cfg: &TransportConfig) -> Result<QuicTransport, TransportError> {
        let incoming = self.ep.accept().await.ok_or(TransportError::PeerClosed)?;
        let conn = incoming
            .await
            .map_err(|e| TransportError::Tls(format!("quic accept: {e}")))?;
        let label = conn.remote_address().to_string();
        Ok(QuicTransport::from_connection(conn, label, cfg))
    }
}

/// Bind a dev QUIC listener (mTLS) on `addr`. Must be called inside a tokio
/// runtime context (the quinn endpoint spawns a driver task).
pub fn listen(addr: SocketAddr) -> Result<DevListener, TransportError> {
    let ep = Endpoint::server(crate::tls::dev::quic_server_config()?, addr)?;
    Ok(DevListener { ep })
}

/// A process-wide client endpoint (ephemeral local UDP port), kept alive so the
/// connections it opens stay up. `Endpoint` is cheaply cloneable and the clones
/// share one driver, so handing out clones is fine. Must first be reached from
/// within a tokio runtime context.
fn client_endpoint() -> Result<Endpoint, TransportError> {
    static EP: OnceLock<Endpoint> = OnceLock::new();
    if let Some(ep) = EP.get() {
        return Ok(ep.clone());
    }
    let ep = Endpoint::client("0.0.0.0:0".parse().expect("valid bind addr"))?;
    // A racing thread may have set it first; keep whichever won.
    let _ = EP.set(ep);
    Ok(EP.get().expect("client endpoint set").clone())
}

/// Connect to a dev QUIC listener at `addr` (mTLS; `server_name` must match the
/// dev cert SAN, i.e. `"localhost"`). Must be called inside a tokio runtime.
pub async fn connect(
    addr: SocketAddr,
    server_name: &str,
    peer_label: &str,
    cfg: &TransportConfig,
) -> Result<QuicTransport, TransportError> {
    let ep = client_endpoint()?;
    QuicTransport::connect(
        &ep,
        crate::tls::dev::quic_client_config()?,
        addr,
        server_name,
        peer_label,
        cfg,
    )
    .await
}
