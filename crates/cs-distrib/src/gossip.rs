//! SWIM-style gossip + suspicion subprotocol.
//!
//! Spec: `docs/research/sdk_spec/distributed.md` § M04, iter C.
//! Reference: SWIM paper — <https://www.cs.cornell.edu/projects/Quicksilver/public_pdfs/SWIM.pdf>.
//!
//! Each protocol period (default 1 s) we pick a random peer and send
//! `ping`. If no `ack` returns, we ask `k=3` other peers to `ping-req`
//! the suspect indirectly (filters one-hop network glitches). On
//! continued silence the peer goes `suspect`; a `suspect` can refute
//! by gossiping a fresh `alive` claim within the suspicion timeout.
//! Membership deltas piggy-back on every ping/ack.
//!
//! Scaffold — actual protocol implementation deferred to M04 iter C.

use std::time::Duration;

/// Knobs for the gossip protocol.
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Protocol period: how often this node initiates a probe.
    pub period: Duration,
    /// Direct-ping timeout before falling back to indirect probes.
    pub ping_timeout: Duration,
    /// Indirect-probe fanout (the `k` in SWIM's "k peers ping-req").
    pub indirect_fanout: usize,
    /// Maximum time a node may sit in `Suspect` before being marked
    /// `Down`. Tuned per cluster size.
    pub suspect_timeout: Duration,
}

impl Default for GossipConfig {
    fn default() -> Self {
        GossipConfig {
            period: Duration::from_secs(1),
            ping_timeout: Duration::from_millis(500),
            indirect_fanout: 3,
            // (5 + log10(N)) × period; for ~30 nodes that's ~6.5 s.
            suspect_timeout: Duration::from_secs(6),
        }
    }
}

/// The protocol's message types. Wire-encoded onto cs-net's
/// `control` logical channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GossipMessage {
    Ping {
        seq: u32,
    },
    Ack {
        seq: u32,
    },
    /// "I tried to ping `target` and it didn't answer; please ping
    /// it on my behalf."
    PingReq {
        seq: u32,
        target: String,
    },
    /// A node refuting a suspect claim about itself.
    Alive {
        node: String,
        incarnation: u64,
    },
    /// A node reporting another node as suspect (piggy-backed).
    Suspect {
        node: String,
        incarnation: u64,
    },
    /// A node reporting another node as confirmed-dead (piggy-backed).
    Confirm {
        node: String,
        incarnation: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_swim_paper_shape() {
        let c = GossipConfig::default();
        assert_eq!(c.period, Duration::from_secs(1));
        assert_eq!(c.indirect_fanout, 3);
        // Half-second ping timeout — leaves room for one period of
        // jitter without triggering an indirect probe.
        assert!(c.ping_timeout < c.period);
    }

    #[test]
    fn message_variants_have_distinct_shapes() {
        // Refuting suspicion bumps the incarnation; suspect and
        // confirm carry the incarnation they are reporting against.
        let alive = GossipMessage::Alive {
            node: "n1".into(),
            incarnation: 5,
        };
        let suspect = GossipMessage::Suspect {
            node: "n1".into(),
            incarnation: 4,
        };
        assert_ne!(alive, suspect);
    }
}
