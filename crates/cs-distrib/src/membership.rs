//! Membership state machine. Spec: `docs/research/sdk_spec/distributed.md`
//! § M04, task list `tasks/M04-membership.md`.
//!
//! Scaffold — state transitions, leader-promotion logic, and SBR
//! policy evaluation are deferred to M04 iter A and iter D.

use crate::NodeId;

/// Per-node membership state. Transitions are leader-driven (the
/// lowest-address `Up` member promotes `Joining → WeaklyUp → Up`
/// and starts removal flows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemberState {
    /// Node has started, is gossiping itself, but the leader hasn't
    /// promoted it yet. Not counted in quorums.
    Joining,
    /// Visible to peers, eligible to receive non-quorum messages.
    WeaklyUp,
    /// Full member, counted in quorums.
    Up,
    /// Node requested graceful shutdown; leader is removing it.
    Leaving,
    /// Leader has decided to remove; node should refuse new traffic.
    Exiting,
    /// Failure detector tripped — node is unreachable.
    Down,
    /// Terminal — node ID no longer in the cluster.
    Removed,
    /// Peer reconnected but with a different epoch (it restarted).
    /// Treated as a brand-new node; the old identity is rejected.
    Quarantined,
}

impl MemberState {
    /// Whether this state counts toward quorum membership.
    pub const fn counts_for_quorum(self) -> bool {
        matches!(self, MemberState::Up)
    }
}

/// Partition-handling policy. Selected per cluster via
/// `(cluster #:partition-policy …)`. Spec § M04 iter D.
#[derive(Debug, Clone)]
pub enum PartitionPolicy {
    /// Partition with strict majority of `Up` members survives;
    /// others self-down. Default.
    KeepMajority,
    /// Partition with at least `size` reachable members survives.
    StaticQuorum { size: usize },
    /// Partition containing the oldest member (by join order)
    /// survives. Useful when a cluster singleton lives there.
    KeepOldest,
    /// Neither side downs; admin intervenes.
    ManualRecovery,
    /// Region-aware: each region runs independently; reconvergence
    /// on heal. Requires region tagging on every node.
    IsolateRegion,
}

impl Default for PartitionPolicy {
    fn default() -> Self {
        PartitionPolicy::KeepMajority
    }
}

/// Stable-window before SBR fires. Default 20 s; if instability
/// lasts `stable_after + down_all_when_unstable`, every node downs
/// itself as a safety net.
#[derive(Debug, Clone)]
pub struct SbrConfig {
    pub policy: PartitionPolicy,
    pub stable_after_ms: u64,
    pub down_all_when_unstable_ms: u64,
}

impl Default for SbrConfig {
    fn default() -> Self {
        SbrConfig {
            policy: PartitionPolicy::default(),
            stable_after_ms: 20_000,
            down_all_when_unstable_ms: 60_000,
        }
    }
}

/// A membership entry for one peer. The membership table is a
/// CRDT-merged map keyed by `NodeId`; entries gossip with the
/// rest of the cluster state. Implementation deferred to M04.
#[derive(Debug, Clone)]
pub struct Member {
    pub node: NodeId,
    pub state: MemberState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_up_counts_for_quorum() {
        assert!(MemberState::Up.counts_for_quorum());
        for s in [
            MemberState::Joining,
            MemberState::WeaklyUp,
            MemberState::Leaving,
            MemberState::Exiting,
            MemberState::Down,
            MemberState::Removed,
            MemberState::Quarantined,
        ] {
            assert!(!s.counts_for_quorum(), "{:?} should not count", s);
        }
    }

    #[test]
    fn default_sbr_keeps_majority() {
        let sbr = SbrConfig::default();
        assert!(matches!(sbr.policy, PartitionPolicy::KeepMajority));
        assert_eq!(sbr.stable_after_ms, 20_000);
    }
}
