//! Cluster handshake protocol (SDK M02.D).
//!
//! Before any `Messages` traffic flows, two nodes exchange a
//! `CLIENT_HELLO` / `SERVER_HELLO` on the `Control` channel, carrying each
//! side's [`NodeId`], a negotiated atom-cache size, and a per-connection
//! session token. [`evaluate_hello`] decides whether to **accept** the peer
//! or move it to a **quarantine** state (protocol-version mismatch, a
//! self-connection, or a *stale epoch* — a peer presenting an epoch older
//! than one we've already seen, i.e. a replayed/zombie incarnation).
//!
//! This module is the protocol + decision logic, which is deterministic and
//! unit-tested here. On production transports the Hello rides *inside* the
//! mTLS session (rustls handshake first, then this exchange over the
//! encrypted `Control` channel); that socket/TLS wiring lands with the TCP
//! transport in cs-net.

use crate::{DistribError, NodeId};

/// Bumped on any incompatible change to the Hello wire format or the
/// handshake sequence. Peers on different versions quarantine.
pub const HANDSHAKE_VERSION: u16 = 1;

/// A `CLIENT_HELLO` / `SERVER_HELLO` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// Protocol version the sender speaks.
    pub version: u16,
    /// The sender's full identity (name@host#epoch).
    pub node: NodeId,
    /// Largest atom-cache index the sender will use (0 = no atom cache;
    /// v1 inlines NodeIds — see [`crate::pid`]).
    pub atom_cache_size: u32,
    /// Random per-connection token, echoed to bind the two Hellos to one
    /// session and defeat cross-connection replay.
    pub session_token: u64,
}

impl Hello {
    pub fn new(node: NodeId, session_token: u64) -> Self {
        Hello {
            version: HANDSHAKE_VERSION,
            node,
            atom_cache_size: 0,
            session_token,
        }
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.version.to_be_bytes());
        put_str(out, &self.node.name);
        put_str(out, &self.node.host);
        out.extend_from_slice(&self.node.epoch.to_be_bytes());
        out.extend_from_slice(&self.atom_cache_size.to_be_bytes());
        out.extend_from_slice(&self.session_token.to_be_bytes());
    }

    pub fn encode_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Hello, DistribError> {
        let mut cur = Cursor { bytes, pos: 0 };
        let version = cur.get_u16()?;
        let name = cur.get_str()?;
        let host = cur.get_str()?;
        let epoch = cur.get_u64()?;
        let atom_cache_size = cur.get_u32()?;
        let session_token = cur.get_u64()?;
        Ok(Hello {
            version,
            node: NodeId::new(name, host, epoch),
            atom_cache_size,
            session_token,
        })
    }
}

/// The result of evaluating a peer's [`Hello`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeOutcome {
    /// Accept the connection. `atom_cache_size` is the negotiated minimum.
    Accepted {
        peer: NodeId,
        session_token: u64,
        atom_cache_size: u32,
    },
    /// Refuse + quarantine the peer; `reason` explains why.
    Quarantine { reason: String },
}

/// Decide whether to accept `hello` from a peer.
///
/// - Protocol-version mismatch → quarantine.
/// - The peer claims our own identity (name@host) → quarantine
///   (self/loopback or an impersonation).
/// - `known_peer_epoch = Some(e)` and the presented epoch is `< e` → the
///   peer is a stale/zombie incarnation → quarantine. A higher (restart) or
///   first-seen epoch is accepted.
/// - Otherwise accept, negotiating the atom-cache size down to the smaller
///   of the two sides.
pub fn evaluate_hello(
    local: &NodeId,
    hello: &Hello,
    local_atom_cache_size: u32,
    known_peer_epoch: Option<u64>,
) -> HandshakeOutcome {
    if hello.version != HANDSHAKE_VERSION {
        return HandshakeOutcome::Quarantine {
            reason: format!(
                "handshake version mismatch: local {HANDSHAKE_VERSION}, peer {}",
                hello.version
            ),
        };
    }
    if hello.node.name == local.name && hello.node.host == local.host {
        return HandshakeOutcome::Quarantine {
            reason: format!("peer presented our own identity {}", local.label()),
        };
    }
    if let Some(known) = known_peer_epoch {
        if hello.node.epoch < known {
            return HandshakeOutcome::Quarantine {
                reason: format!(
                    "stale peer epoch for {}: known #{known}, presented #{}",
                    hello.node.label(),
                    hello.node.epoch
                ),
            };
        }
    }
    HandshakeOutcome::Accepted {
        peer: hello.node.clone(),
        session_token: hello.session_token,
        atom_cache_size: local_atom_cache_size.min(hello.atom_cache_size),
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], DistribError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or_else(|| DistribError::Decode(format!("handshake truncated: need {n} bytes")))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
    fn get_u16(&mut self) -> Result<u16, DistribError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn get_u32(&mut self) -> Result<u32, DistribError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes(b.try_into().expect("4 bytes")))
    }
    fn get_u64(&mut self) -> Result<u64, DistribError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(b.try_into().expect("8 bytes")))
    }
    fn get_str(&mut self) -> Result<String, DistribError> {
        let len = self.get_u16()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|e| DistribError::Decode(format!("non-utf8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nid(name: &str, epoch: u64) -> NodeId {
        NodeId::new(name, "host:7000", epoch)
    }

    #[test]
    fn hello_round_trips() {
        let mut h = Hello::new(nid("b", 3), 0xDEAD_BEEF);
        h.atom_cache_size = 256;
        let decoded = Hello::decode(&h.encode_vec()).unwrap();
        assert_eq!(decoded, h);
    }

    #[test]
    fn truncated_hello_errors() {
        let bytes = Hello::new(nid("b", 1), 1).encode_vec();
        assert!(matches!(
            Hello::decode(&bytes[..bytes.len() - 1]),
            Err(DistribError::Decode(_))
        ));
    }

    #[test]
    fn accepts_compatible_peer() {
        let local = nid("a", 1);
        let hello = Hello::new(nid("b", 1), 99);
        match evaluate_hello(&local, &hello, 128, None) {
            HandshakeOutcome::Accepted {
                peer,
                session_token,
                atom_cache_size,
            } => {
                assert_eq!(peer, nid("b", 1));
                assert_eq!(session_token, 99);
                assert_eq!(atom_cache_size, 0); // min(128, 0)
            }
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn negotiates_atom_cache_to_min() {
        let local = nid("a", 1);
        let mut hello = Hello::new(nid("b", 1), 1);
        hello.atom_cache_size = 64;
        match evaluate_hello(&local, &hello, 128, None) {
            HandshakeOutcome::Accepted {
                atom_cache_size, ..
            } => assert_eq!(atom_cache_size, 64),
            other => panic!("expected Accepted, got {other:?}"),
        }
    }

    #[test]
    fn quarantines_version_mismatch() {
        let local = nid("a", 1);
        let mut hello = Hello::new(nid("b", 1), 1);
        hello.version = HANDSHAKE_VERSION + 1;
        assert!(matches!(
            evaluate_hello(&local, &hello, 0, None),
            HandshakeOutcome::Quarantine { .. }
        ));
    }

    #[test]
    fn quarantines_self_identity() {
        let local = nid("a", 1);
        // Peer claims to be us (same name@host).
        let hello = Hello::new(nid("a", 5), 1);
        match evaluate_hello(&local, &hello, 0, None) {
            HandshakeOutcome::Quarantine { reason } => assert!(reason.contains("our own identity")),
            other => panic!("expected Quarantine, got {other:?}"),
        }
    }

    #[test]
    fn quarantines_stale_epoch() {
        let local = nid("a", 1);
        // We last saw peer b at epoch 5; it now presents epoch 4 → stale.
        let hello = Hello::new(nid("b", 4), 1);
        match evaluate_hello(&local, &hello, 0, Some(5)) {
            HandshakeOutcome::Quarantine { reason } => assert!(reason.contains("stale peer epoch")),
            other => panic!("expected Quarantine, got {other:?}"),
        }
    }

    #[test]
    fn accepts_restart_with_higher_epoch() {
        let local = nid("a", 1);
        // Peer b restarted: known #5, now presents #6 → accept.
        let hello = Hello::new(nid("b", 6), 1);
        assert!(matches!(
            evaluate_hello(&local, &hello, 0, Some(5)),
            HandshakeOutcome::Accepted { .. }
        ));
    }
}
