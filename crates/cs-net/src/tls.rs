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

/// Shared QUIC transport tuning for the cluster. Sizes flow-control windows so
/// one channel's stream can't monopolize the connection-wide receive budget
/// and starve another: quinn's `receive_window` caps bytes *across all streams*
/// of a connection, and the docs note that keeping `stream_receive_window`
/// smaller than it stops a single stream from monopolizing receive buffers
/// "while still requiring data on other streams" — exactly the head-of-line
/// case where a `Bulk` flood must not delay `Control`. `send_fairness`
/// round-robins equal-priority streams (channel priority is set per-stream in
/// [`crate::quic`]).
#[cfg(feature = "quic")]
fn quic_transport_config() -> Arc<quinn::TransportConfig> {
    use quinn::VarInt;
    let mut tc = quinn::TransportConfig::default();
    tc.receive_window(VarInt::from_u32(16 * 1024 * 1024)); // 16 MiB, connection-wide
    tc.stream_receive_window(VarInt::from_u32(2 * 1024 * 1024)); // 2 MiB per stream
    tc.send_window(16 * 1024 * 1024); // match the peer's receive window
    tc.send_fairness(true);
    Arc::new(tc)
}

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
    let mut sc = quinn::ServerConfig::with_crypto(Arc::new(qsc));
    sc.transport_config(quic_transport_config());
    Ok(sc)
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
    let mut client = quinn::ClientConfig::new(Arc::new(qcc));
    client.transport_config(quic_transport_config());
    Ok(client)
}

/// Dev/test self-signed mTLS identity, shared process-wide. NOT for
/// production: a single self-signed cert is used as both the presented
/// identity *and* the trusted root on every node, so the mutual-TLS handshake
/// runs (encrypted + mutually authenticated at the transport) without external
/// cert management. Node identity proper is the cs-distrib `Hello` (NodeId)
/// exchanged after the TLS handshake, so distinct nodes stay distinguishable.
#[cfg(feature = "dev-certs")]
pub mod dev {
    use super::*;
    use std::sync::OnceLock;

    /// (cert DER, key DER) for the one shared self-signed `localhost` identity.
    fn identity() -> &'static (CertificateDer<'static>, Vec<u8>) {
        static ID: OnceLock<(CertificateDer<'static>, Vec<u8>)> = OnceLock::new();
        ID.get_or_init(|| {
            let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
                .expect("rcgen self-signed identity");
            let cert = CertificateDer::from(ck.cert.der().to_vec());
            (cert, ck.key_pair.serialize_der())
        })
    }

    fn cert() -> CertificateDer<'static> {
        identity().0.clone()
    }
    fn key() -> PrivateKeyDer<'static> {
        PrivateKeyDer::try_from(identity().1.clone()).expect("valid dev key der")
    }
    fn roots() -> RootCertStore {
        let mut r = RootCertStore::empty();
        r.add(cert()).expect("add dev root");
        r
    }

    /// TCP mTLS server config from the shared dev identity.
    pub fn server_config() -> Result<Arc<ServerConfig>, TransportError> {
        Ok(Arc::new(super::server_config(
            roots(),
            vec![cert()],
            key(),
        )?))
    }

    /// TCP mTLS client config from the shared dev identity.
    pub fn client_config() -> Result<Arc<ClientConfig>, TransportError> {
        Ok(Arc::new(super::client_config(
            roots(),
            vec![cert()],
            key(),
        )?))
    }

    /// QUIC mTLS server config from the shared dev identity.
    #[cfg(feature = "quic")]
    pub fn quic_server_config() -> Result<quinn::ServerConfig, TransportError> {
        super::quic_server_config(roots(), vec![cert()], key())
    }

    /// QUIC mTLS client config from the shared dev identity.
    #[cfg(feature = "quic")]
    pub fn quic_client_config() -> Result<quinn::ClientConfig, TransportError> {
        super::quic_client_config(roots(), vec![cert()], key())
    }
}
