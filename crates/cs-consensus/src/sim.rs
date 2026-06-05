//! Deterministic in-memory cluster harness for the consensus cores.
//!
//! Drives a set of [`SimNode`]s with full control over time (logical ticks)
//! and the network (message delivery order, drops, partitions) — no tokio, no
//! sockets, no wall clock. This is what lets a hairy protocol like Raft (and
//! later EPaxos) be tested for safety/liveness reproducibly: every run is a
//! pure function of the calls made.
//!
//! A node is anything that reacts to a timer tick or an inbound message by
//! producing addressed outbound messages. The harness owns the nodes, routes
//! their outputs through a FIFO network (subject to partitions), and pumps to
//! quiescence.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ReplicaId;

/// A node the [`Cluster`] can drive: it reacts to ticks and messages by
/// emitting addressed outbound messages `(to, msg)`.
pub trait SimNode {
    /// The protocol message type exchanged between replicas.
    type Msg: Clone;

    /// This node's identity.
    fn id(&self) -> ReplicaId;

    /// Advance one logical timer tick (elections, heartbeats).
    fn on_tick(&mut self) -> Vec<(ReplicaId, Self::Msg)>;

    /// Handle one inbound message from `from`.
    fn on_message(&mut self, from: ReplicaId, msg: Self::Msg) -> Vec<(ReplicaId, Self::Msg)>;
}

/// A controllable in-memory cluster of [`SimNode`]s.
#[derive(Debug)]
pub struct Cluster<N: SimNode> {
    nodes: BTreeMap<ReplicaId, N>,
    /// In-flight messages: `(from, to, msg)`, delivered FIFO.
    inflight: VecDeque<(ReplicaId, ReplicaId, N::Msg)>,
    /// Isolated replicas: messages to/from them are dropped (a partition).
    isolated: BTreeSet<ReplicaId>,
    /// Safety valve so a misbehaving protocol can't spin the harness forever.
    max_deliveries: usize,
}

impl<N: SimNode> Cluster<N> {
    /// Build a cluster from its nodes.
    pub fn new(nodes: impl IntoIterator<Item = N>) -> Self {
        let nodes = nodes.into_iter().map(|n| (n.id(), n)).collect();
        Cluster {
            nodes,
            inflight: VecDeque::new(),
            isolated: BTreeSet::new(),
            max_deliveries: 100_000,
        }
    }

    /// Replica ids, ascending.
    pub fn ids(&self) -> Vec<ReplicaId> {
        self.nodes.keys().copied().collect()
    }

    pub fn node(&self, id: ReplicaId) -> &N {
        self.nodes.get(&id).expect("unknown replica")
    }

    pub fn node_mut(&mut self, id: ReplicaId) -> &mut N {
        self.nodes.get_mut(&id).expect("unknown replica")
    }

    /// Partition `id` away from the rest of the cluster (drop its traffic).
    pub fn isolate(&mut self, id: ReplicaId) {
        self.isolated.insert(id);
    }

    /// Heal a previously [`isolate`](Self::isolate)d replica.
    pub fn heal(&mut self, id: ReplicaId) {
        self.isolated.remove(&id);
    }

    /// Whether a `(from, to)` link currently passes traffic.
    fn link_up(&self, from: ReplicaId, to: ReplicaId) -> bool {
        !self.isolated.contains(&from) && !self.isolated.contains(&to)
    }

    /// Enqueue outputs produced by `from`, dropping any across a partition or
    /// addressed to an unknown replica.
    fn enqueue(&mut self, from: ReplicaId, outs: Vec<(ReplicaId, N::Msg)>) {
        for (to, msg) in outs {
            if self.nodes.contains_key(&to) && self.link_up(from, to) {
                self.inflight.push_back((from, to, msg));
            }
        }
    }

    /// Run an action on one node (e.g. propose a command), routing whatever
    /// messages it emits. Returns the closure's value.
    pub fn act<R>(
        &mut self,
        id: ReplicaId,
        f: impl FnOnce(&mut N) -> (R, Vec<(ReplicaId, N::Msg)>),
    ) -> R {
        let (r, outs) = f(self.nodes.get_mut(&id).expect("unknown replica"));
        self.enqueue(id, outs);
        r
    }

    /// Tick every node once (ascending id order), routing their outputs.
    pub fn tick_all(&mut self) {
        for id in self.ids() {
            let outs = self.nodes.get_mut(&id).expect("node").on_tick();
            self.enqueue(id, outs);
        }
    }

    /// Deliver all currently in-flight messages to quiescence (messages beget
    /// messages until none remain), routing every reply. Returns the number
    /// of messages delivered.
    pub fn deliver_all(&mut self) -> usize {
        let mut delivered = 0;
        while let Some((from, to, msg)) = self.inflight.pop_front() {
            delivered += 1;
            assert!(
                delivered <= self.max_deliveries,
                "cluster did not quiesce within {} deliveries (protocol livelock?)",
                self.max_deliveries
            );
            // A node isolated *after* a message was enqueued still shouldn't
            // receive it.
            if !self.link_up(from, to) {
                continue;
            }
            let outs = self.nodes.get_mut(&to).expect("node").on_message(from, msg);
            self.enqueue(to, outs);
        }
        delivered
    }

    /// One full round: tick everyone, then settle all resulting traffic.
    pub fn step(&mut self) {
        self.tick_all();
        self.deliver_all();
    }

    /// Run `rounds` full [`step`](Self::step)s.
    pub fn run(&mut self, rounds: usize) {
        for _ in 0..rounds {
            self.step();
        }
    }
}
