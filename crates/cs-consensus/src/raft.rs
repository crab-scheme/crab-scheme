//! Raft consensus core (leader election + log replication + commit/apply).
//!
//! A deterministic, I/O-free implementation of the Raft protocol (Ongaro &
//! Ousterhout, <https://raft.github.io/raft.pdf>). [`RaftNode`] is a pure
//! state machine: feed it a logical [`on_tick`](RaftNode::on_tick), an
//! inbound [`on_message`](RaftNode::on_message), or a client
//! [`propose`](RaftNode::propose); it mutates state and returns the messages
//! to send. No threads, clocks, or sockets — those live in the cs-net driver.
//!
//! This module covers the core (terms, elections, log matching, majority
//! commit, apply to the [`StateMachine`]). ReadIndex reads, snapshots, and
//! joint-consensus membership change build on it in sibling commits.

use std::collections::{BTreeMap, BTreeSet};

use crate::sim::SimNode;
use crate::{ReplicaId, StateMachine};

/// A Raft term — a logical election epoch, monotonically increasing.
pub type Term = u64;
/// A 1-based log index (`0` means "before the first entry").
pub type Index = u64;

/// What a log entry carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryPayload {
    /// A client command, applied to the [`StateMachine`].
    Command(Vec<u8>),
    /// A leader's no-op, appended on election so the leader can commit
    /// entries from prior terms via one of its own (Raft §5.4.2).
    Noop,
}

/// One replicated log entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub term: Term,
    pub index: Index,
    pub payload: EntryPayload,
}

/// Raft RPC messages exchanged between replicas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Candidate → peers: request a vote for `term`.
    RequestVote {
        term: Term,
        candidate: ReplicaId,
        last_log_index: Index,
        last_log_term: Term,
    },
    /// Voter → candidate.
    RequestVoteResp { term: Term, granted: bool },
    /// Leader → follower: heartbeat / replicate entries. `read_seq` is the
    /// leader's monotonic heartbeat counter, echoed back to confirm leadership
    /// for ReadIndex reads (Raft §6.4) without appending to the log.
    AppendEntries {
        term: Term,
        leader: ReplicaId,
        prev_log_index: Index,
        prev_log_term: Term,
        entries: Vec<Entry>,
        leader_commit: Index,
        read_seq: u64,
    },
    /// Follower → leader. `match_index` is the highest index now known to
    /// match the leader; `conflict_index` hints where to back up on failure;
    /// `read_seq` echoes the heartbeat's counter for ReadIndex confirmation.
    AppendEntriesResp {
        term: Term,
        success: bool,
        match_index: Index,
        conflict_index: Index,
        read_seq: u64,
    },
    /// Leader → follower that has fallen behind the leader's compacted log:
    /// ship the snapshot so it can catch up. The follower replies with an
    /// `AppendEntriesResp` (`match_index = last_included_index`).
    InstallSnapshot {
        term: Term,
        leader: ReplicaId,
        last_included_index: Index,
        last_included_term: Term,
        data: Vec<u8>,
        read_seq: u64,
    },
}

/// A node's current role in its term.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Follower,
    Candidate,
    Leader,
}

/// Outcome of a linearizable [`read`](RaftNode::read), delivered via
/// [`take_ready_reads`](RaftNode::take_ready_reads).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadResult {
    /// The query result from the leader's committed state.
    Value(Vec<u8>),
    /// This node was not (or stopped being) the leader; retry elsewhere.
    NotLeader,
}

/// A linearizable read awaiting leadership confirmation (ReadIndex).
#[derive(Clone, Debug)]
struct PendingRead {
    req_id: u64,
    query: Vec<u8>,
    /// Commit index captured when the read was issued; the read must observe
    /// at least this much applied state.
    read_index: Index,
    /// The read can be served once a quorum has confirmed a heartbeat with at
    /// least this `read_seq`.
    min_seq: u64,
}

/// Timer/tuning knobs, in logical ticks.
#[derive(Clone, Debug)]
pub struct Config {
    /// Election timeout is chosen uniformly in `[min, max]` ticks, re-rolled
    /// each cycle so simultaneous candidates desynchronize (avoids split
    /// votes). `max > min`.
    pub election_timeout_min: u32,
    pub election_timeout_max: u32,
    /// Leader heartbeat period; must be `< election_timeout_min` so a healthy
    /// leader keeps followers from timing out.
    pub heartbeat_interval: u32,
    /// Compact the log into a snapshot once this many *applied* entries have
    /// accumulated past the current snapshot base. `0` disables compaction.
    pub snapshot_threshold: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            election_timeout_min: 5,
            election_timeout_max: 10,
            heartbeat_interval: 1,
            snapshot_threshold: 0,
        }
    }
}

type Out = (ReplicaId, Message);

/// A single Raft replica.
#[derive(Debug)]
pub struct RaftNode<SM: StateMachine> {
    id: ReplicaId,
    /// Full voter set (including self), ascending. Single-config for now.
    voters: Vec<ReplicaId>,
    cfg: Config,

    // ---- persistent state (would be fsync'd in production) ----
    current_term: Term,
    voted_for: Option<ReplicaId>,
    log: Vec<Entry>,
    /// Index/term of the last entry folded into the latest snapshot; the live
    /// `log` holds only entries after `base_index`.
    base_index: Index,
    base_term: Term,
    /// Serialized state machine as of `base_index` — shipped verbatim in
    /// `InstallSnapshot` (it must reflect `base_index`, not the latest apply).
    snapshot_data: Vec<u8>,

    // ---- volatile state ----
    role: Role,
    leader_id: Option<ReplicaId>,
    commit_index: Index,
    last_applied: Index,

    // ---- candidate state ----
    votes: BTreeSet<ReplicaId>,

    // ---- leader state ----
    next_index: BTreeMap<ReplicaId, Index>,
    match_index: BTreeMap<ReplicaId, Index>,

    // ---- ReadIndex (linearizable reads) ----
    /// Monotonic heartbeat counter; bumped on each broadcast.
    read_seq: u64,
    /// Highest `read_seq` each follower has echoed (leadership confirmation).
    acked_seq: BTreeMap<ReplicaId, u64>,
    pending_reads: Vec<PendingRead>,
    ready_reads: Vec<(u64, ReadResult)>,

    // ---- timers (logical ticks) ----
    election_elapsed: u32,
    heartbeat_elapsed: u32,
    election_timeout: u32,
    rng: u64,

    sm: SM,
}

impl<SM: StateMachine> RaftNode<SM> {
    /// Create a follower in `voters` (which must contain `id`).
    pub fn new(id: ReplicaId, voters: Vec<ReplicaId>, cfg: Config, sm: SM) -> Self {
        assert!(voters.contains(&id), "voter set must include self");
        let mut voters = voters;
        voters.sort_unstable();
        voters.dedup();
        let mut node = RaftNode {
            id,
            voters,
            cfg,
            current_term: 0,
            voted_for: None,
            log: Vec::new(),
            base_index: 0,
            base_term: 0,
            snapshot_data: Vec::new(),
            role: Role::Follower,
            leader_id: None,
            commit_index: 0,
            last_applied: 0,
            votes: BTreeSet::new(),
            next_index: BTreeMap::new(),
            match_index: BTreeMap::new(),
            read_seq: 0,
            acked_seq: BTreeMap::new(),
            pending_reads: Vec::new(),
            ready_reads: Vec::new(),
            election_elapsed: 0,
            heartbeat_elapsed: 0,
            election_timeout: 0,
            // Seed the PRNG per replica so timeouts differ across nodes.
            rng: id.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            sm,
        };
        node.reset_election_timer();
        node
    }

    // ---- accessors ----
    pub fn id(&self) -> ReplicaId {
        self.id
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn is_leader(&self) -> bool {
        self.role == Role::Leader
    }
    pub fn leader(&self) -> Option<ReplicaId> {
        self.leader_id
    }
    pub fn current_term(&self) -> Term {
        self.current_term
    }
    pub fn commit_index(&self) -> Index {
        self.commit_index
    }
    pub fn last_applied(&self) -> Index {
        self.last_applied
    }
    pub fn sm(&self) -> &SM {
        &self.sm
    }

    // ---- log helpers (1-based; `base_index` entries are compacted away into
    // the latest snapshot, so live entries are `base_index+1 ..= last`) ----
    pub fn last_log_index(&self) -> Index {
        self.log.last().map(|e| e.index).unwrap_or(self.base_index)
    }
    fn last_log_term(&self) -> Term {
        self.log.last().map(|e| e.term).unwrap_or(self.base_term)
    }
    /// Term of the entry at `index`, or `None` if it's unknown (beyond the log)
    /// or already compacted (`index < base_index`). `base_index` itself is the
    /// snapshot's last-included term.
    fn term_at(&self, index: Index) -> Option<Term> {
        if index == 0 {
            return Some(0);
        }
        if index == self.base_index {
            return Some(self.base_term);
        }
        if index < self.base_index {
            return None; // compacted
        }
        self.log
            .get((index - self.base_index - 1) as usize)
            .map(|e| e.term)
    }
    fn entry_at(&self, index: Index) -> Option<&Entry> {
        if index <= self.base_index {
            return None; // zero or compacted
        }
        self.log.get((index - self.base_index - 1) as usize)
    }

    fn peers(&self) -> impl Iterator<Item = ReplicaId> + '_ {
        let me = self.id;
        self.voters.iter().copied().filter(move |v| *v != me)
    }
    fn majority(&self) -> usize {
        self.voters.len() / 2 + 1
    }
    /// Does `acked` (a set of replica ids) constitute a quorum of voters?
    fn is_quorum(&self, acked: &BTreeSet<ReplicaId>) -> bool {
        let n = self.voters.iter().filter(|v| acked.contains(v)).count();
        n >= self.majority()
    }

    // ---- PRNG (xorshift64) ----
    fn next_rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
    fn reset_election_timer(&mut self) {
        let span = (self.cfg.election_timeout_max - self.cfg.election_timeout_min + 1) as u64;
        let r = (self.next_rand() % span) as u32;
        self.election_timeout = self.cfg.election_timeout_min + r;
        self.election_elapsed = 0;
    }

    // ---- public driving API ----

    /// Advance one logical tick. Followers/candidates may start an election;
    /// a leader may emit heartbeats.
    pub fn on_tick(&mut self) -> Vec<Out> {
        match self.role {
            Role::Leader => {
                self.heartbeat_elapsed += 1;
                if self.heartbeat_elapsed >= self.cfg.heartbeat_interval {
                    self.heartbeat_elapsed = 0;
                    return self.broadcast_append();
                }
                Vec::new()
            }
            Role::Follower | Role::Candidate => {
                self.election_elapsed += 1;
                if self.election_elapsed >= self.election_timeout {
                    self.start_election()
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Submit a client command. Returns `(Some(index), msgs)` if this node is
    /// the leader (the entry was appended and replication started), else
    /// `(None, [])`.
    pub fn propose(&mut self, command: Vec<u8>) -> (Option<Index>, Vec<Out>) {
        if self.role != Role::Leader {
            return (None, Vec::new());
        }
        let index = self.append_local(EntryPayload::Command(command));
        let outs = self.broadcast_append();
        (Some(index), outs)
    }

    /// Issue a linearizable read (ReadIndex, Raft §6.4). The leader captures
    /// its current commit index and confirms — via a quorum-acked heartbeat —
    /// that it is still the leader before answering, so the read reflects
    /// every previously-committed write and never a stale value.
    ///
    /// The result arrives via [`take_ready_reads`](Self::take_ready_reads):
    /// immediately as [`ReadResult::NotLeader`] if this node isn't the leader
    /// (or single-node), otherwise once leadership is confirmed. The returned
    /// messages are the confirming heartbeats to send.
    pub fn read(&mut self, req_id: u64, query: Vec<u8>) -> Vec<Out> {
        if self.role != Role::Leader {
            self.ready_reads.push((req_id, ReadResult::NotLeader));
            return Vec::new();
        }
        let outs = self.broadcast_append(); // bumps read_seq
        self.pending_reads.push(PendingRead {
            req_id,
            query,
            read_index: self.commit_index,
            min_seq: self.read_seq,
        });
        self.serve_reads(); // single-node leader can answer right away
        outs
    }

    /// Drain reads that have completed since the last call.
    pub fn take_ready_reads(&mut self) -> Vec<(u64, ReadResult)> {
        std::mem::take(&mut self.ready_reads)
    }

    /// Handle one inbound message.
    pub fn on_message(&mut self, from: ReplicaId, msg: Message) -> Vec<Out> {
        match msg {
            Message::RequestVote {
                term,
                candidate,
                last_log_index,
                last_log_term,
            } => self.handle_request_vote(term, candidate, last_log_index, last_log_term),
            Message::RequestVoteResp { term, granted } => {
                self.handle_request_vote_resp(from, term, granted)
            }
            Message::AppendEntries {
                term,
                leader,
                prev_log_index,
                prev_log_term,
                entries,
                leader_commit,
                read_seq,
            } => self.handle_append_entries(
                term,
                leader,
                prev_log_index,
                prev_log_term,
                entries,
                leader_commit,
                read_seq,
            ),
            Message::AppendEntriesResp {
                term,
                success,
                match_index,
                conflict_index,
                read_seq,
            } => self.handle_append_entries_resp(
                from,
                term,
                success,
                match_index,
                conflict_index,
                read_seq,
            ),
            Message::InstallSnapshot {
                term,
                leader,
                last_included_index,
                last_included_term,
                data,
                read_seq,
            } => self.handle_install_snapshot(
                term,
                leader,
                last_included_index,
                last_included_term,
                data,
                read_seq,
            ),
        }
    }

    // ---- role transitions ----

    fn become_follower(&mut self, term: Term, leader: Option<ReplicaId>) {
        self.role = Role::Follower;
        self.current_term = term;
        self.leader_id = leader;
        self.votes.clear();
        // Reads can only be served by a leader; fail any in flight so the
        // caller retries on the new one.
        for pr in std::mem::take(&mut self.pending_reads) {
            self.ready_reads.push((pr.req_id, ReadResult::NotLeader));
        }
        self.acked_seq.clear();
        self.reset_election_timer();
    }

    fn start_election(&mut self) -> Vec<Out> {
        self.role = Role::Candidate;
        self.current_term += 1;
        self.voted_for = Some(self.id);
        self.leader_id = None;
        self.votes.clear();
        self.votes.insert(self.id);
        self.reset_election_timer();

        // Single-node cluster: an instant majority.
        if self.is_quorum(&self.votes) {
            return self.become_leader();
        }
        let (lli, llt) = (self.last_log_index(), self.last_log_term());
        let msg = Message::RequestVote {
            term: self.current_term,
            candidate: self.id,
            last_log_index: lli,
            last_log_term: llt,
        };
        self.peers().map(|p| (p, msg.clone())).collect()
    }

    fn become_leader(&mut self) -> Vec<Out> {
        self.role = Role::Leader;
        self.leader_id = Some(self.id);
        self.heartbeat_elapsed = 0;
        let next = self.last_log_index() + 1;
        self.next_index.clear();
        self.match_index.clear();
        for p in self.peers().collect::<Vec<_>>() {
            self.next_index.insert(p, next);
            self.match_index.insert(p, 0);
        }
        // Commit barrier for this term (Raft §5.4.2).
        self.append_local(EntryPayload::Noop);
        // A single-node leader can commit its own entries immediately.
        self.maybe_advance_commit();
        self.broadcast_append()
    }

    // ---- log mutation ----

    fn append_local(&mut self, payload: EntryPayload) -> Index {
        let index = self.last_log_index() + 1;
        self.log.push(Entry {
            term: self.current_term,
            index,
            payload,
        });
        if let Some(m) = self.match_index.get_mut(&self.id) {
            *m = index;
        }
        index
    }

    // ---- RequestVote ----

    fn handle_request_vote(
        &mut self,
        term: Term,
        candidate: ReplicaId,
        cand_last_index: Index,
        cand_last_term: Term,
    ) -> Vec<Out> {
        if term > self.current_term {
            self.become_follower(term, None);
            self.voted_for = None;
        }
        let mut granted = false;
        if term == self.current_term {
            let log_ok =
                (cand_last_term, cand_last_index) >= (self.last_log_term(), self.last_log_index());
            let can_vote = self.voted_for.is_none() || self.voted_for == Some(candidate);
            if log_ok && can_vote {
                granted = true;
                self.voted_for = Some(candidate);
                self.reset_election_timer();
            }
        }
        vec![(
            candidate,
            Message::RequestVoteResp {
                term: self.current_term,
                granted,
            },
        )]
    }

    fn handle_request_vote_resp(&mut self, from: ReplicaId, term: Term, granted: bool) -> Vec<Out> {
        if term > self.current_term {
            self.become_follower(term, None);
            self.voted_for = None;
            return Vec::new();
        }
        if self.role != Role::Candidate || term != self.current_term {
            return Vec::new();
        }
        if granted {
            self.votes.insert(from);
            if self.is_quorum(&self.votes) {
                return self.become_leader();
            }
        }
        Vec::new()
    }

    // ---- AppendEntries (follower side) ----

    #[allow(clippy::too_many_arguments)]
    fn handle_append_entries(
        &mut self,
        term: Term,
        leader: ReplicaId,
        prev_log_index: Index,
        prev_log_term: Term,
        entries: Vec<Entry>,
        leader_commit: Index,
        read_seq: u64,
    ) -> Vec<Out> {
        if term < self.current_term {
            return vec![(
                leader,
                Message::AppendEntriesResp {
                    term: self.current_term,
                    success: false,
                    match_index: 0,
                    conflict_index: 0,
                    read_seq,
                },
            )];
        }
        // Valid leader for this term (or a newer one): (re)sync as follower.
        self.become_follower(term, Some(leader));

        // Log-matching check at prev_log_index.
        let prev_ok = match self.term_at(prev_log_index) {
            Some(t) => t == prev_log_term,
            None => false, // we don't have prev_log_index yet
        };
        if !prev_ok {
            // Back the leader up: jump to just past what we have if too short,
            // else to the start of our conflicting term's run.
            let conflict_index = if self.last_log_index() < prev_log_index {
                self.last_log_index() + 1
            } else {
                prev_log_index
            };
            return vec![(
                leader,
                Message::AppendEntriesResp {
                    term: self.current_term,
                    success: false,
                    match_index: 0,
                    conflict_index,
                    read_seq,
                },
            )];
        }

        // The highest index this AppendEntries lets us match the leader on.
        let match_index = prev_log_index + entries.len() as Index;
        // Splice in entries, truncating on the first conflict.
        for e in entries {
            if e.index <= self.base_index {
                continue; // already folded into our snapshot
            }
            match self.term_at(e.index) {
                Some(t) if t == e.term => {} // already have it; skip
                Some(_) => {
                    // Conflict: drop this entry and everything after it.
                    self.log.truncate((e.index - self.base_index - 1) as usize);
                    self.log.push(e);
                }
                None => self.log.push(e),
            }
        }

        // Advance commit to the leader's, bounded by what we now hold.
        if leader_commit > self.commit_index {
            self.commit_index = leader_commit.min(self.last_log_index());
            self.apply_committed();
        }
        vec![(
            leader,
            Message::AppendEntriesResp {
                term: self.current_term,
                success: true,
                match_index,
                conflict_index: 0,
                read_seq,
            },
        )]
    }

    // ---- InstallSnapshot (follower side) ----

    #[allow(clippy::too_many_arguments)]
    fn handle_install_snapshot(
        &mut self,
        term: Term,
        leader: ReplicaId,
        last_included_index: Index,
        last_included_term: Term,
        data: Vec<u8>,
        read_seq: u64,
    ) -> Vec<Out> {
        let ack = |this: &Self, success, match_index| {
            vec![(
                leader,
                Message::AppendEntriesResp {
                    term: this.current_term,
                    success,
                    match_index,
                    conflict_index: 0,
                    read_seq,
                },
            )]
        };
        if term < self.current_term {
            return ack(self, false, 0);
        }
        self.become_follower(term, Some(leader));
        // Stale / already-covered snapshot: ack our current position.
        if last_included_index <= self.base_index {
            return ack(self, true, self.last_log_index());
        }
        // Keep a consistent suffix if we have a matching entry at the snapshot
        // boundary; otherwise the whole log is superseded.
        if self.term_at(last_included_index) == Some(last_included_term) {
            self.log.retain(|e| e.index > last_included_index);
        } else {
            self.log.clear();
        }
        self.sm.restore(&data);
        self.snapshot_data = data;
        self.base_index = last_included_index;
        self.base_term = last_included_term;
        self.commit_index = self.commit_index.max(last_included_index);
        self.last_applied = self.last_applied.max(last_included_index);
        ack(self, true, last_included_index)
    }

    // ---- AppendEntries (leader side) ----

    fn handle_append_entries_resp(
        &mut self,
        from: ReplicaId,
        term: Term,
        success: bool,
        match_index: Index,
        conflict_index: Index,
        read_seq: u64,
    ) -> Vec<Out> {
        if term > self.current_term {
            self.become_follower(term, None);
            self.voted_for = None;
            return Vec::new();
        }
        if self.role != Role::Leader || term != self.current_term {
            return Vec::new();
        }
        // Any in-term response (success or not) confirms this follower still
        // sees us as leader as of `read_seq` — that's the ReadIndex barrier.
        let prev = self.acked_seq.get(&from).copied().unwrap_or(0);
        self.acked_seq.insert(from, read_seq.max(prev));
        if success {
            self.match_index.insert(from, match_index);
            self.next_index.insert(from, match_index + 1);
            self.maybe_advance_commit();
            self.serve_reads();
            Vec::new()
        } else {
            // Back up next_index toward the follower's hint and retry.
            let ni = self.next_index.get(&from).copied().unwrap_or(1);
            let backed = if conflict_index > 0 {
                conflict_index.min(ni.saturating_sub(1)).max(1)
            } else {
                ni.saturating_sub(1).max(1)
            };
            self.next_index.insert(from, backed);
            self.serve_reads();
            vec![(from, self.append_for(from))]
        }
    }

    /// Build the message to send to `peer` from its `next_index`: an
    /// `InstallSnapshot` if it has fallen behind the compacted log, else an
    /// `AppendEntries` carrying the missing suffix.
    fn append_for(&self, peer: ReplicaId) -> Message {
        let next = self.next_index.get(&peer).copied().unwrap_or(1);
        // The entry just before `next` is gone (compacted) → ship the snapshot.
        if next <= self.base_index {
            return Message::InstallSnapshot {
                term: self.current_term,
                leader: self.id,
                last_included_index: self.base_index,
                last_included_term: self.base_term,
                data: self.snapshot_data.clone(),
                read_seq: self.read_seq,
            };
        }
        let prev_log_index = next - 1;
        let prev_log_term = self.term_at(prev_log_index).unwrap_or(0);
        let entries: Vec<Entry> = self
            .log
            .iter()
            .filter(|e| e.index >= next)
            .cloned()
            .collect();
        Message::AppendEntries {
            term: self.current_term,
            leader: self.id,
            prev_log_index,
            prev_log_term,
            entries,
            leader_commit: self.commit_index,
            read_seq: self.read_seq,
        }
    }

    fn broadcast_append(&mut self) -> Vec<Out> {
        // Each broadcast carries a higher read_seq; quorum echoes confirm
        // leadership and let pending ReadIndex reads be served.
        self.read_seq += 1;
        self.peers()
            .collect::<Vec<_>>()
            .into_iter()
            .map(|p| (p, self.append_for(p)))
            .collect()
    }

    /// Leader: advance `commit_index` to the highest index replicated on a
    /// quorum *and* belonging to the current term (Raft §5.4.2), then apply.
    fn maybe_advance_commit(&mut self) {
        if self.role != Role::Leader {
            return;
        }
        let mut advanced = false;
        for n in (self.commit_index + 1)..=self.last_log_index() {
            if self.term_at(n) != Some(self.current_term) {
                continue;
            }
            let mut acked: BTreeSet<ReplicaId> = BTreeSet::new();
            acked.insert(self.id); // leader has it
            for p in self.peers() {
                if self.match_index.get(&p).copied().unwrap_or(0) >= n {
                    acked.insert(p);
                }
            }
            if self.is_quorum(&acked) {
                self.commit_index = n;
                advanced = true;
            }
        }
        if advanced {
            self.apply_committed();
        }
    }

    fn apply_committed(&mut self) {
        while self.last_applied < self.commit_index {
            self.last_applied += 1;
            let idx = self.last_applied;
            if let Some(entry) = self.entry_at(idx) {
                if let EntryPayload::Command(cmd) = &entry.payload {
                    let cmd = cmd.clone();
                    self.sm.apply(&cmd);
                }
            }
        }
        // Newly-applied state may satisfy a pending read's read_index.
        self.serve_reads();
        self.maybe_compact();
    }

    /// Fold every applied entry into a snapshot and drop it from the live log.
    /// The snapshot blob is captured from the state machine *now*, while it
    /// reflects exactly `last_applied`. No-op if nothing new was applied.
    pub fn compact(&mut self) {
        if self.last_applied <= self.base_index {
            return;
        }
        let new_base = self.last_applied;
        let new_base_term = self.term_at(new_base).expect("applied entry is present");
        self.snapshot_data = self.sm.snapshot();
        self.log.retain(|e| e.index > new_base);
        self.base_index = new_base;
        self.base_term = new_base_term;
    }

    /// Compact once the configured number of applied entries has accumulated
    /// past the snapshot base.
    fn maybe_compact(&mut self) {
        let thr = self.cfg.snapshot_threshold;
        if thr > 0 && self.last_applied.saturating_sub(self.base_index) >= thr {
            self.compact();
        }
    }

    /// Snapshot bookkeeping accessor (for tests/metrics): the highest index
    /// folded into a snapshot.
    pub fn snapshot_index(&self) -> Index {
        self.base_index
    }

    /// Highest `read_seq` a quorum of voters has confirmed (the leader counts
    /// itself at its current `read_seq`). Mirrors commit-index advancement but
    /// over heartbeat acknowledgements instead of match indices.
    fn confirmed_read_seq(&self) -> u64 {
        let mut seqs: Vec<u64> = self
            .voters
            .iter()
            .map(|v| {
                if *v == self.id {
                    self.read_seq
                } else {
                    self.acked_seq.get(v).copied().unwrap_or(0)
                }
            })
            .collect();
        seqs.sort_unstable_by(|a, b| b.cmp(a)); // descending
        seqs[self.majority() - 1]
    }

    /// Serve every pending read whose leadership barrier is confirmed and
    /// whose `read_index` has been applied.
    fn serve_reads(&mut self) {
        if self.role != Role::Leader {
            return;
        }
        let confirmed = self.confirmed_read_seq();
        let applied = self.last_applied;
        let mut still_pending = Vec::new();
        for pr in std::mem::take(&mut self.pending_reads) {
            if pr.min_seq <= confirmed && pr.read_index <= applied {
                let val = self.sm.query(&pr.query);
                self.ready_reads.push((pr.req_id, ReadResult::Value(val)));
            } else {
                still_pending.push(pr);
            }
        }
        self.pending_reads = still_pending;
    }
}

impl<SM: StateMachine> SimNode for RaftNode<SM> {
    type Msg = Message;
    fn id(&self) -> ReplicaId {
        self.id
    }
    fn on_tick(&mut self) -> Vec<(ReplicaId, Message)> {
        RaftNode::on_tick(self)
    }
    fn on_message(&mut self, from: ReplicaId, msg: Message) -> Vec<(ReplicaId, Message)> {
        RaftNode::on_message(self, from, msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Cluster;

    /// A trivial deterministic state machine: sums i64 commands.
    #[derive(Default, Debug)]
    struct SumSm {
        total: i64,
        applied: Vec<i64>,
    }
    impl StateMachine for SumSm {
        fn apply(&mut self, command: &[u8]) -> Vec<u8> {
            let v = i64::from_le_bytes(command.try_into().expect("i64"));
            self.total += v;
            self.applied.push(v);
            self.total.to_le_bytes().to_vec()
        }
        fn query(&self, _query: &[u8]) -> Vec<u8> {
            self.total.to_le_bytes().to_vec()
        }
        fn snapshot(&self) -> Vec<u8> {
            self.total.to_le_bytes().to_vec()
        }
        fn restore(&mut self, snapshot: &[u8]) {
            self.total = i64::from_le_bytes(snapshot.try_into().expect("i64"));
            self.applied.clear();
        }
    }

    fn cmd(v: i64) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    fn cluster(n: u64) -> Cluster<RaftNode<SumSm>> {
        cluster_with(n, Config::default())
    }

    fn cluster_with(n: u64, cfg: Config) -> Cluster<RaftNode<SumSm>> {
        let ids: Vec<ReplicaId> = (0..n).map(ReplicaId).collect();
        let nodes = ids
            .iter()
            .map(|id| RaftNode::new(*id, ids.clone(), cfg.clone(), SumSm::default()));
        Cluster::new(nodes)
    }

    /// Tick/settle until exactly one leader exists, or panic after `budget`.
    fn elect(c: &mut Cluster<RaftNode<SumSm>>, budget: usize) -> ReplicaId {
        for _ in 0..budget {
            c.step();
            let leaders: Vec<ReplicaId> = c
                .ids()
                .into_iter()
                .filter(|id| c.node(*id).is_leader())
                .collect();
            if leaders.len() == 1 {
                return leaders[0];
            }
        }
        panic!("no unique leader within budget");
    }

    fn propose(c: &mut Cluster<RaftNode<SumSm>>, leader: ReplicaId, v: i64) -> Option<Index> {
        let idx = c.act(leader, |n| n.propose(cmd(v)));
        c.deliver_all();
        idx
    }

    #[test]
    fn single_node_elects_itself() {
        let mut c = cluster(1);
        let leader = elect(&mut c, 20);
        assert_eq!(leader, ReplicaId(0));
        assert!(c.node(ReplicaId(0)).is_leader());
    }

    #[test]
    fn three_nodes_one_leader_at_top_term() {
        let mut c = cluster(3);
        let leader = elect(&mut c, 50);
        let term = c.node(leader).current_term();
        // Exactly one leader, and everyone agrees who at the top term.
        let leaders = c
            .ids()
            .into_iter()
            .filter(|id| c.node(*id).is_leader())
            .count();
        assert_eq!(leaders, 1);
        for id in c.ids() {
            if c.node(id).current_term() == term && id != leader {
                assert_ne!(c.node(id).role(), Role::Leader);
            }
        }
    }

    #[test]
    fn replication_commits_on_majority() {
        let mut c = cluster(3);
        let leader = elect(&mut c, 50);
        propose(&mut c, leader, 100);
        propose(&mut c, leader, 30);
        // A couple of heartbeat rounds let followers learn the commit index.
        c.run(3);
        for id in c.ids() {
            assert_eq!(c.node(id).sm().total, 130, "replica {id} state");
        }
    }

    #[test]
    fn follower_catches_up_after_isolation() {
        let mut c = cluster(3);
        let leader = elect(&mut c, 50);
        let follower = c.ids().into_iter().find(|id| *id != leader).unwrap();

        c.isolate(follower);
        for v in [10, 20, 30] {
            propose(&mut c, leader, v);
        }
        c.run(3);
        // The isolated follower missed everything.
        assert_eq!(c.node(follower).sm().total, 0);

        c.heal(follower);
        c.run(8); // heartbeats carry the missing suffix + commit index
        assert_eq!(c.node(follower).sm().total, 60, "healed follower caught up");
    }

    #[test]
    fn leader_failure_triggers_reelection_and_progress() {
        let mut c = cluster(3);
        let old = elect(&mut c, 50);
        propose(&mut c, old, 5);
        c.run(3);

        // Old leader dies.
        c.isolate(old);
        let new = {
            // The remaining two must elect a new leader.
            let mut found = None;
            for _ in 0..80 {
                c.step();
                let ls: Vec<ReplicaId> = c
                    .ids()
                    .into_iter()
                    .filter(|id| *id != old && c.node(*id).is_leader())
                    .collect();
                if ls.len() == 1 {
                    found = Some(ls[0]);
                    break;
                }
            }
            found.expect("new leader elected among survivors")
        };
        assert_ne!(new, old);
        propose(&mut c, new, 7);
        c.run(4);
        // The two survivors agree on 5 + 7.
        for id in c.ids().into_iter().filter(|id| *id != old) {
            assert_eq!(c.node(id).sm().total, 12, "survivor {id}");
        }
    }

    #[test]
    fn minority_partition_cannot_commit() {
        let mut c = cluster(5);
        let leader = elect(&mut c, 60);
        // Commit something with the full cluster.
        propose(&mut c, leader, 1);
        c.run(3);

        // Cut the leader off with one ally → a 2-node minority.
        let ally = c.ids().into_iter().find(|id| *id != leader).unwrap();
        c.isolate(leader);
        c.isolate(ally);

        // Old leader (now in the minority) cannot commit new proposals.
        let before = c.node(leader).commit_index();
        for _ in 0..10 {
            c.act(leader, |n| n.propose(cmd(99)));
            c.deliver_all();
            c.step();
        }
        assert_eq!(
            c.node(leader).commit_index(),
            before,
            "minority leader must not advance commit"
        );
        assert_eq!(c.node(leader).sm().total, 1, "no minority commit applied");

        // The 3-node majority elects a leader and commits.
        let majority: Vec<ReplicaId> = c
            .ids()
            .into_iter()
            .filter(|id| *id != leader && *id != ally)
            .collect();
        let mut new = None;
        for _ in 0..80 {
            c.step();
            let ls: Vec<ReplicaId> = majority
                .iter()
                .copied()
                .filter(|id| c.node(*id).is_leader())
                .collect();
            if ls.len() == 1 {
                new = Some(ls[0]);
                break;
            }
        }
        let new = new.expect("majority elects a leader");
        propose(&mut c, new, 40);
        c.run(4);
        for id in &majority {
            assert_eq!(c.node(*id).sm().total, 41, "majority replica {id}");
        }
    }

    /// Issue a ReadIndex read on `leader`, settle the confirming heartbeats,
    /// and return its result.
    fn read(c: &mut Cluster<RaftNode<SumSm>>, leader: ReplicaId, req: u64) -> ReadResult {
        c.act(leader, |n| ((), n.read(req, Vec::new())));
        c.deliver_all();
        c.node_mut(leader)
            .take_ready_reads()
            .into_iter()
            .find(|(id, _)| *id == req)
            .map(|(_, r)| r)
            .expect("read completed")
    }

    #[test]
    fn linearizable_read_reflects_committed_writes() {
        let mut c = cluster(3);
        let leader = elect(&mut c, 50);
        propose(&mut c, leader, 100);
        propose(&mut c, leader, 30);
        c.run(2);
        // ReadIndex returns the leader's committed value after quorum confirms
        // leadership — never a stale read.
        assert_eq!(
            read(&mut c, leader, 1),
            ReadResult::Value(130i64.to_le_bytes().to_vec())
        );
    }

    #[test]
    fn read_on_follower_reports_not_leader() {
        let mut c = cluster(3);
        let leader = elect(&mut c, 50);
        let follower = c.ids().into_iter().find(|id| *id != leader).unwrap();
        c.act(follower, |n| ((), n.read(7, Vec::new())));
        assert_eq!(
            c.node_mut(follower).take_ready_reads(),
            vec![(7, ReadResult::NotLeader)]
        );
    }

    #[test]
    fn lagging_follower_recovers_via_snapshot() {
        // Threshold 3: the leader compacts aggressively. A follower isolated
        // from the start falls behind the compacted prefix and must be caught
        // up with an InstallSnapshot, not an AppendEntries.
        let cfg = Config {
            snapshot_threshold: 3,
            ..Config::default()
        };
        let mut c = cluster_with(3, cfg);
        let leader = elect(&mut c, 50);
        let follower = c.ids().into_iter().find(|id| *id != leader).unwrap();

        c.isolate(follower);
        for v in [1, 2, 3, 4, 5] {
            propose(&mut c, leader, v);
        }
        c.run(3);
        assert!(
            c.node(leader).snapshot_index() > 0,
            "leader should have compacted its log"
        );
        assert_eq!(
            c.node(follower).sm().total,
            0,
            "isolated follower missed all"
        );

        // Heal: the only way to catch up is via the snapshot.
        c.heal(follower);
        c.run(10);
        assert_eq!(
            c.node(follower).sm().total,
            15,
            "follower restored from snapshot + tail"
        );
        assert!(
            c.node(follower).snapshot_index() > 0,
            "follower installed the snapshot"
        );
    }
}
