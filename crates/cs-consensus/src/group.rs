//! Raft group + replicated state machine.
//!
//! v1 backed by `openraft` (M06 iter A). The scaffold captures the
//! API surface so consumers (Scheme `define-replicated-actor`, the
//! lease state machine) can be written against it.

use std::collections::HashSet;

pub type ReplicaId = String;

/// Strength of the read path. `Linearizable` issues a ReadIndex query
/// against the leader; `Local` reads from any replica's current
/// state-machine view (stale, but cheap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsistencyLevel {
    Linearizable,
    Local,
}

#[derive(Debug, Clone)]
pub struct RaftGroupConfig {
    pub group_id: String,
    pub members: HashSet<ReplicaId>,
    pub snapshot_every_entries: u64,
    pub snapshot_every_bytes: u64,
}

impl RaftGroupConfig {
    pub fn new(group_id: impl Into<String>, members: impl IntoIterator<Item = ReplicaId>) -> Self {
        RaftGroupConfig {
            group_id: group_id.into(),
            members: members.into_iter().collect(),
            snapshot_every_entries: 10_000,
            snapshot_every_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A handle to a Raft-replicated state machine. Real impl wraps
/// `openraft::Raft<Config>` and exposes the submit / read / membership
/// APIs over cs-net's `consensus` channel. Scaffold contract: API
/// surface stable, behavior `NotImplemented`.
#[derive(Debug)]
pub struct RaftGroup {
    pub config: RaftGroupConfig,
}

impl RaftGroup {
    pub fn new(config: RaftGroupConfig) -> Self {
        RaftGroup { config }
    }

    /// Number of members; a quorum is `floor(n/2) + 1`.
    pub fn quorum(&self) -> usize {
        self.config.members.len() / 2 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_member_quorum_is_two() {
        let cfg = RaftGroupConfig::new("g1", ["a".to_string(), "b".to_string(), "c".to_string()]);
        let g = RaftGroup::new(cfg);
        assert_eq!(g.quorum(), 2);
    }

    #[test]
    fn five_member_quorum_is_three() {
        let cfg = RaftGroupConfig::new(
            "g1",
            ["a", "b", "c", "d", "e"].into_iter().map(String::from),
        );
        let g = RaftGroup::new(cfg);
        assert_eq!(g.quorum(), 3);
    }
}
