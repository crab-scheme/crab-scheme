//! mTLS configuration for the TCP transport (SDK M02.D).
//!
//! Builds rustls configs for **mutual** TLS: the server requires + verifies
//! a client certificate chaining to a trusted root, and the client verifies
//! the server the same way. Every node thus authenticates its peer, and all
//! `Channel` traffic is encrypted (TLS 1.3 / 1.2 via the ring provider).
//!
//! Wiring: [`crate::tcp::TcpTransport::connect_tls`] /
//! [`crate::tcp::TcpTransport::accept_tls`] wrap the `TcpStream` in a
//! `tokio_rustls` TLS stream, then hand it to the generic
//! `TcpTransport::from_stream` — so the [`crate::framing`] + cs-distrib
//! handshake run unchanged over the encrypted channel.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

use crate::TransportError;

/// Install the ring crypto provider as the process default. Idempotent —
/// safe to call from every config builder and from tests. rustls 0.23
/// requires a provider be installed before [`ServerConfig`] / [`ClientConfig`]
/// builders run.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Server-side mTLS config: present `cert_chain` / `key`, and **require** a
/// client certificate that chains to `roots`.
pub fn server_config(
    roots: RootCertStore,
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ServerConfig, TransportError> {
    install_crypto_provider();
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| TransportError::Tls(format!("client cert verifier: {e}")))?;
    ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(cert_chain, key)
        .map_err(|e| TransportError::Tls(format!("server identity: {e}")))
}

/// Client-side mTLS config: trust `roots` for the server, and present
/// `cert_chain` / `key` as this node's identity (the server requires it).
pub fn client_config(
    roots: RootCertStore,
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<ClientConfig, TransportError> {
    install_crypto_provider();
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(cert_chain, key)
        .map_err(|e| TransportError::Tls(format!("client identity: {e}")))
}

/// The cluster's QUIC ALPN protocol id (QUIC mandates ALPN).
#[cfg(feature = "quic")]
pub const QUIC_ALPN: &[u8] = b"crabscheme-cluster";

/// quinn server config for the QUIC transport: the same mTLS as
/// [`server_config`] (require + verify a client cert), wrapped for QUIC
/// (TLS 1.3, cluster ALPN).
#[cfg(feature = "quic")]
pub fn quic_server_config(
    roots: RootCertStore,
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::ServerConfig, TransportError> {
    let mut rc = server_config(roots, cert_chain, key)?;
    rc.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    let qsc = quinn::crypto::rustls::QuicServerConfig::try_from(rc)
        .map_err(|e| TransportError::Tls(format!("quic server config: {e}")))?;
    Ok(quinn::ServerConfig::with_crypto(Arc::new(qsc)))
}

/// quinn client config for the QUIC transport (mTLS + cluster ALPN).
#[cfg(feature = "quic")]
pub fn quic_client_config(
    roots: RootCertStore,
    cert_chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::ClientConfig, TransportError> {
    let mut cc = client_config(roots, cert_chain, key)?;
    cc.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    let qcc = quinn::crypto::rustls::QuicClientConfig::try_from(cc)
        .map_err(|e| TransportError::Tls(format!("quic client config: {e}")))?;
    Ok(quinn::ClientConfig::new(Arc::new(qcc)))
}
