//! Distributed actor identity + wire codec (SDK M02.A).
//!
//! cs-actor's local `ActorPid` is `{ node: u16, local_id: u64 }` — a
//! node-local routing index that means nothing off-box. A [`DistPid`]
//! carries the full [`NodeId`] (including the restart `epoch`) so it routes
//! across the cluster and a message addressed to a *stale incarnation* (a
//! Pid minted before the target node restarted) is detectable: the router
//! compares the Pid's epoch against the live connection's NodeId epoch and
//! rejects mismatches (see [`crate::DistribError::EpochMismatch`]).
//!
//! Wire format (big-endian), self-describing so the peer needs no prior
//! knowledge of the sender's node:
//!
//! ```text
//! [name_len:u16][name][host_len:u16][host][epoch:u64][local_id:u64]
//! ```
//!
//! An atom-cache that replaces the repeated NodeId with a small integer is
//! a later optimization (negotiated in the M02.D handshake — see the
//! `atom-cache-size` field); v1 inlines the NodeId for simplicity.

use crate::{DistribError, NodeId};

/// A cluster-wide actor identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DistPid {
    /// The node hosting the actor (name@host#epoch).
    pub node: NodeId,
    /// The actor's node-local id (matches cs-actor `ActorPid::local_id`).
    pub local_id: u64,
}

impl DistPid {
    pub fn new(node: NodeId, local_id: u64) -> Self {
        DistPid { node, local_id }
    }

    /// Append this Pid's wire encoding to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        put_str(out, &self.node.name);
        put_str(out, &self.node.host);
        out.extend_from_slice(&self.node.epoch.to_be_bytes());
        out.extend_from_slice(&self.local_id.to_be_bytes());
    }

    /// Encode to a fresh `Vec`.
    pub fn encode_vec(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }

    /// Decode a Pid from the front of `bytes`, returning it plus the number
    /// of bytes consumed (so a decoder can read a Pid then a payload from
    /// one frame).
    pub fn decode(bytes: &[u8]) -> Result<(DistPid, usize), DistribError> {
        let mut cur = Cursor { bytes, pos: 0 };
        let name = cur.get_str()?;
        let host = cur.get_str()?;
        let epoch = cur.get_u64()?;
        let local_id = cur.get_u64()?;
        Ok((
            DistPid::new(NodeId::new(name, host, epoch), local_id),
            cur.pos,
        ))
    }
}

impl std::fmt::Display for DistPid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}.{}>", self.node, self.local_id)
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Minimal big-endian read cursor with bounds checks.
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
            .ok_or_else(|| DistribError::Decode(format!("truncated: need {n} more bytes")))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn get_u16(&mut self) -> Result<u16, DistribError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn get_u64(&mut self) -> Result<u64, DistribError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes(b.try_into().expect("8 bytes")))
    }

    fn get_str(&mut self) -> Result<String, DistribError> {
        let len = self.get_u16()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec())
            .map_err(|e| DistribError::Decode(format!("non-utf8 str: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(name: &str, host: &str, epoch: u64, local: u64) -> DistPid {
        DistPid::new(NodeId::new(name, host, epoch), local)
    }

    #[test]
    fn round_trip() {
        let p = pid("worker", "10.0.0.4:7001", 42, 12345);
        let bytes = p.encode_vec();
        let (decoded, consumed) = DistPid::decode(&bytes).unwrap();
        assert_eq!(decoded, p);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn decode_reports_consumed_so_payload_follows() {
        let p = pid("a", "h", 1, 7);
        let mut frame = p.encode_vec();
        frame.extend_from_slice(b"the-payload");
        let (decoded, consumed) = DistPid::decode(&frame).unwrap();
        assert_eq!(decoded, p);
        assert_eq!(&frame[consumed..], b"the-payload");
    }

    #[test]
    fn epoch_distinguishes_incarnations() {
        // Same name@host, different epoch → different Pid (stale-Pid
        // detection relies on this).
        let a = pid("n", "h", 1, 5);
        let b = pid("n", "h", 2, 5);
        assert_ne!(a, b);
        assert_ne!(a.encode_vec(), b.encode_vec());
    }

    #[test]
    fn truncated_input_is_an_error() {
        let bytes = pid("worker", "host", 1, 2).encode_vec();
        // Drop the last byte → decode must fail, not panic.
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(
            DistPid::decode(truncated),
            Err(DistribError::Decode(_))
        ));
        // Empty input too.
        assert!(matches!(DistPid::decode(&[]), Err(DistribError::Decode(_))));
    }

    #[test]
    fn empty_name_and_host_round_trip() {
        let p = pid("", "", 0, 0);
        let (decoded, _) = DistPid::decode(&p.encode_vec()).unwrap();
        assert_eq!(decoded, p);
    }
}
