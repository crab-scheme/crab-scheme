//! Driving a [`RaftNode`] over cs-net transports and cs-actor.
//!
//! [`RaftDriver`] is the thin I/O shim around the deterministic core: it owns
//! the node plus one cs-net [`Transport`] per peer, encodes/decodes messages
//! ([`crate::codec`]) over the `Channel::Consensus` logical channel, and routes
//! the node's outputs. It stays synchronous (like cs-distrib's `Router`), so it
//! works over any transport — the in-memory Sim, TCP+mTLS, or QUIC.
//!
//! [`spawn_raft_actor`] runs a driver inside a cs-actor task: a timer drives
//! `tick`/`poll`, and client commands arrive through the actor's mailbox — so a
//! Raft group is just another set of actors in the runtime.

use std::collections::BTreeMap;

use cs_net::{Channel, Transport};

use crate::codec::{decode, decode_epaxos, encode, encode_epaxos};
use crate::epaxos::{EpaxosReplica, EpaxosStateMachine, Message as EpaxosMessage};
use crate::raft::{Index, Message, RaftNode};
use crate::{ReplicaId, StateMachine};

/// A Raft replica wired to its peers over cs-net.
pub struct RaftDriver<SM: StateMachine> {
    node: RaftNode<SM>,
    peers: BTreeMap<ReplicaId, Box<dyn Transport>>,
}

impl<SM: StateMachine> std::fmt::Debug for RaftDriver<SM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaftDriver")
            .field("id", &self.node.id())
            .field("role", &self.node.role())
            .field("peers", &self.peers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl<SM: StateMachine> RaftDriver<SM> {
    pub fn new(node: RaftNode<SM>) -> Self {
        RaftDriver {
            node,
            peers: BTreeMap::new(),
        }
    }

    /// Register the transport reaching peer `id` (on `Channel::Consensus`).
    pub fn add_peer(&mut self, id: ReplicaId, transport: Box<dyn Transport>) {
        self.peers.insert(id, transport);
    }

    pub fn node(&self) -> &RaftNode<SM> {
        &self.node
    }
    pub fn node_mut(&mut self) -> &mut RaftNode<SM> {
        &mut self.node
    }

    /// Encode + send each outbound message to its peer's consensus channel.
    fn dispatch(&self, outs: Vec<(ReplicaId, Message)>) {
        for (to, msg) in outs {
            if let Some(t) = self.peers.get(&to) {
                let _ = t.send(Channel::Consensus, &encode(&msg));
            }
        }
    }

    /// Advance the logical clock once (elections / heartbeats).
    pub fn tick(&mut self) {
        let outs = self.node.on_tick();
        self.dispatch(outs);
    }

    /// Drain inbound consensus frames from every peer, feed them to the node,
    /// and route the replies. Returns how many messages were processed.
    pub fn poll(&mut self) -> usize {
        // Collect first (immutable borrow of peers), then process (mutable node).
        let mut inbound: Vec<(ReplicaId, Vec<u8>)> = Vec::new();
        for (id, t) in &self.peers {
            while let Ok(Some(frame)) = t.try_recv(Channel::Consensus) {
                inbound.push((*id, frame));
            }
        }
        let mut processed = 0;
        for (from, frame) in inbound {
            if let Ok(msg) = decode(&frame) {
                let outs = self.node.on_message(from, msg);
                self.dispatch(outs);
                processed += 1;
            }
        }
        processed
    }

    /// Submit a client command; returns the assigned log index if leader.
    pub fn propose(&mut self, command: Vec<u8>) -> Option<Index> {
        let (idx, outs) = self.node.propose(command);
        self.dispatch(outs);
        idx
    }

    /// Issue a linearizable read (result via `node().take_ready_reads`).
    pub fn read(&mut self, req_id: u64, query: Vec<u8>) {
        let outs = self.node.read(req_id, query);
        self.dispatch(outs);
    }
}

/// A command delivered to a [`spawn_raft_actor`] actor through its mailbox.
#[derive(Debug, Clone)]
pub enum RaftCommand {
    /// Submit a client command to the replicated log.
    Propose(Vec<u8>),
}

/// Run a [`RaftDriver`] as a cs-actor actor: a `tick_period` timer drives
/// `tick`/`poll`, and [`RaftCommand`]s arrive via the actor's mailbox. Returns
/// the actor handle; send it `RaftCommand`s with `ActorRef::send`.
pub fn spawn_raft_actor<SM>(
    system: &cs_actor::ActorSystem,
    mut driver: RaftDriver<SM>,
    tick_period: std::time::Duration,
) -> cs_actor::ActorRef
where
    SM: StateMachine + Send + 'static,
{
    system.spawn_async(move |mut actor| async move {
        let mut timer = tokio::time::interval(tick_period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            // Drain pending commands non-blockingly. (We deliberately don't
            // `select!` on `receive_async`: that future isn't cancel-safe, so
            // dropping it on the timer branch would wreck the mailbox.)
            loop {
                match actor.try_receive() {
                    Ok(cs_actor::Message::User(payload)) => {
                        if let Some(RaftCommand::Propose(c)) = payload.downcast_ref::<RaftCommand>()
                        {
                            driver.propose(c.clone());
                        }
                    }
                    Ok(_) => {} // exit/down signals: ignore for now
                    Err(cs_actor::TryRecvError::Empty) => break,
                    Err(cs_actor::TryRecvError::Disconnected) => return,
                }
            }
            driver.tick();
            driver.poll();
        }
    })
}

/// An EPaxos replica wired to its peers over cs-net (the EPaxos analogue of
/// [`RaftDriver`]). Same thin sync shim: encode/route over `Channel::Consensus`,
/// drain inbound, drive `propose`/`poll`.
pub struct EpaxosDriver<SM: EpaxosStateMachine> {
    node: EpaxosReplica<SM>,
    peers: BTreeMap<ReplicaId, Box<dyn Transport>>,
}

impl<SM: EpaxosStateMachine> std::fmt::Debug for EpaxosDriver<SM> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EpaxosDriver")
            .field("id", &self.node.id())
            .field("peers", &self.peers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl<SM: EpaxosStateMachine> EpaxosDriver<SM> {
    pub fn new(node: EpaxosReplica<SM>) -> Self {
        EpaxosDriver {
            node,
            peers: BTreeMap::new(),
        }
    }

    pub fn add_peer(&mut self, id: ReplicaId, transport: Box<dyn Transport>) {
        self.peers.insert(id, transport);
    }

    pub fn node(&self) -> &EpaxosReplica<SM> {
        &self.node
    }

    fn dispatch(&self, outs: Vec<(ReplicaId, EpaxosMessage)>) {
        for (to, msg) in outs {
            if let Some(t) = self.peers.get(&to) {
                let _ = t.send(Channel::Consensus, &encode_epaxos(&msg));
            }
        }
    }

    /// Lead a new command; routes the PreAccept round.
    pub fn propose(&mut self, command: Vec<u8>) {
        let outs = self.node.propose(command);
        self.dispatch(outs);
    }

    /// Drain inbound consensus frames, feed them to the replica, route replies.
    pub fn poll(&mut self) -> usize {
        let mut inbound: Vec<(ReplicaId, Vec<u8>)> = Vec::new();
        for (id, t) in &self.peers {
            while let Ok(Some(frame)) = t.try_recv(Channel::Consensus) {
                inbound.push((*id, frame));
            }
        }
        let mut processed = 0;
        for (from, frame) in inbound {
            if let Ok(msg) = decode_epaxos(&frame) {
                let outs = self.node.on_message(from, msg);
                self.dispatch(outs);
                processed += 1;
            }
        }
        processed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raft::Config;
    use cs_net::sim::SimPair;

    #[derive(Default, Debug)]
    struct SumSm {
        total: i64,
    }
    impl StateMachine for SumSm {
        fn apply(&mut self, command: &[u8]) -> Vec<u8> {
            self.total += i64::from_le_bytes(command.try_into().unwrap());
            self.total.to_le_bytes().to_vec()
        }
        fn query(&self, _q: &[u8]) -> Vec<u8> {
            self.total.to_le_bytes().to_vec()
        }
        fn snapshot(&self) -> Vec<u8> {
            self.total.to_le_bytes().to_vec()
        }
        fn restore(&mut self, s: &[u8]) {
            self.total = i64::from_le_bytes(s.try_into().unwrap());
        }
    }

    fn cmd(v: i64) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    /// Build 3 drivers fully meshed over cs-net Sim transports.
    fn meshed() -> BTreeMap<ReplicaId, RaftDriver<SumSm>> {
        let ids = [ReplicaId(0), ReplicaId(1), ReplicaId(2)];
        let voters = ids.to_vec();
        let mut drivers: BTreeMap<ReplicaId, RaftDriver<SumSm>> = ids
            .iter()
            .map(|id| {
                (
                    *id,
                    RaftDriver::new(RaftNode::new(
                        *id,
                        voters.clone(),
                        Config::default(),
                        SumSm::default(),
                    )),
                )
            })
            .collect();
        // One Sim pair per undirected edge.
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let (ea, eb) =
                    SimPair::new(format!("{}", ids[i].0), format!("{}", ids[j].0)).into_endpoints();
                drivers
                    .get_mut(&ids[i])
                    .unwrap()
                    .add_peer(ids[j], Box::new(ea));
                drivers
                    .get_mut(&ids[j])
                    .unwrap()
                    .add_peer(ids[i], Box::new(eb));
            }
        }
        drivers
    }

    fn step_all(drivers: &mut BTreeMap<ReplicaId, RaftDriver<SumSm>>) {
        let ids: Vec<ReplicaId> = drivers.keys().copied().collect();
        for id in &ids {
            drivers.get_mut(id).unwrap().tick();
        }
        // A few poll passes settle the request/response exchange.
        for _ in 0..4 {
            for id in &ids {
                drivers.get_mut(id).unwrap().poll();
            }
        }
    }

    #[test]
    fn raft_reaches_agreement_over_cs_net_sim_transport() {
        let mut drivers = meshed();
        let ids: Vec<ReplicaId> = drivers.keys().copied().collect();

        // Drive until a leader emerges.
        let mut leader = None;
        for _ in 0..60 {
            step_all(&mut drivers);
            let ls: Vec<ReplicaId> = ids
                .iter()
                .copied()
                .filter(|id| drivers[id].node().is_leader())
                .collect();
            if ls.len() == 1 {
                leader = Some(ls[0]);
                break;
            }
        }
        let leader = leader.expect("a leader emerged over cs-net");

        // Propose through the real transport path; let it replicate.
        drivers.get_mut(&leader).unwrap().propose(cmd(100));
        drivers.get_mut(&leader).unwrap().propose(cmd(23));
        for _ in 0..10 {
            step_all(&mut drivers);
        }
        for id in &ids {
            assert_eq!(
                drivers[id].node().sm().total,
                123,
                "replica {id} agreed over cs-net"
            );
        }
    }

    // ---- actor-driven path ----

    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct SharedSm {
        total: Arc<Mutex<i64>>,
    }
    impl StateMachine for SharedSm {
        fn apply(&mut self, command: &[u8]) -> Vec<u8> {
            let mut t = self.total.lock().unwrap();
            *t += i64::from_le_bytes(command.try_into().unwrap());
            t.to_le_bytes().to_vec()
        }
        fn query(&self, _q: &[u8]) -> Vec<u8> {
            self.total.lock().unwrap().to_le_bytes().to_vec()
        }
        fn snapshot(&self) -> Vec<u8> {
            self.total.lock().unwrap().to_le_bytes().to_vec()
        }
        fn restore(&mut self, s: &[u8]) {
            *self.total.lock().unwrap() = i64::from_le_bytes(s.try_into().unwrap());
        }
    }

    // Plain `#[test]`: the ActorSystem owns its own tokio runtime (actors run
    // there), so the test thread must stay *outside* a runtime — otherwise
    // `system.shutdown()` (which drops that runtime) panics.
    #[test]
    fn raft_group_runs_as_actors() {
        use std::time::Duration;
        let system = cs_actor::ActorSystem::new();
        let ids = [ReplicaId(0), ReplicaId(1), ReplicaId(2)];
        let voters = ids.to_vec();

        // Per-node observable state.
        let totals: BTreeMap<ReplicaId, Arc<Mutex<i64>>> = ids
            .iter()
            .map(|id| (*id, Arc::new(Mutex::new(0))))
            .collect();

        let mut drivers: BTreeMap<ReplicaId, RaftDriver<SharedSm>> = ids
            .iter()
            .map(|id| {
                let sm = SharedSm {
                    total: totals[id].clone(),
                };
                (
                    *id,
                    RaftDriver::new(RaftNode::new(*id, voters.clone(), Config::default(), sm)),
                )
            })
            .collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let (ea, eb) = SimPair::new("a", "b").into_endpoints();
                drivers
                    .get_mut(&ids[i])
                    .unwrap()
                    .add_peer(ids[j], Box::new(ea));
                drivers
                    .get_mut(&ids[j])
                    .unwrap()
                    .add_peer(ids[i], Box::new(eb));
            }
        }

        // Spawn each driver as an actor.
        let refs: Vec<cs_actor::ActorRef> = ids
            .iter()
            .map(|id| {
                let d = drivers.remove(id).unwrap();
                spawn_raft_actor(&system, d, Duration::from_millis(2))
            })
            .collect();

        // Let a leader settle first — a propose before then is a no-op on
        // every follower and would simply be dropped.
        std::thread::sleep(Duration::from_millis(300));
        // Propose to all (only the leader appends; followers no-op).
        for r in &refs {
            r.send(Arc::new(RaftCommand::Propose(cmd(42)))).unwrap();
        }

        // Wait for the cluster to converge.
        let mut ok = false;
        for _ in 0..400 {
            std::thread::sleep(Duration::from_millis(5));
            if ids.iter().all(|id| *totals[id].lock().unwrap() == 42) {
                ok = true;
                break;
            }
        }
        assert!(ok, "actor-driven Raft group did not converge to 42");
        system.shutdown();
    }

    // ---- EPaxos over cs-net ----

    use crate::epaxos::{EpaxosReplica, EpaxosStateMachine};

    /// Commands `[key, val]`; interfere iff same key. Records execution order.
    #[derive(Default, Debug)]
    struct KvSm {
        executed: Vec<Vec<u8>>,
    }
    impl EpaxosStateMachine for KvSm {
        fn interferes(&self, a: &[u8], b: &[u8]) -> bool {
            !a.is_empty() && !b.is_empty() && a[0] == b[0]
        }
        fn execute(&mut self, command: &[u8]) -> Vec<u8> {
            self.executed.push(command.to_vec());
            Vec::new()
        }
    }

    #[test]
    fn epaxos_consistent_order_over_cs_net_sim_transport() {
        let ids = [ReplicaId(0), ReplicaId(1), ReplicaId(2)];
        let replicas = ids.to_vec();
        let mut drivers: BTreeMap<ReplicaId, EpaxosDriver<KvSm>> = ids
            .iter()
            .map(|id| {
                (
                    *id,
                    EpaxosDriver::new(EpaxosReplica::new(*id, replicas.clone(), KvSm::default())),
                )
            })
            .collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let (ea, eb) = SimPair::new("a", "b").into_endpoints();
                drivers
                    .get_mut(&ids[i])
                    .unwrap()
                    .add_peer(ids[j], Box::new(ea));
                drivers
                    .get_mut(&ids[j])
                    .unwrap()
                    .add_peer(ids[i], Box::new(eb));
            }
        }
        // Two concurrent interfering commands (same key) via different leaders,
        // committed + executed over the real cs-net framed path.
        drivers.get_mut(&ids[0]).unwrap().propose(vec![9, 1]);
        drivers.get_mut(&ids[1]).unwrap().propose(vec![9, 2]);
        for _ in 0..20 {
            for id in &ids {
                drivers.get_mut(id).unwrap().poll();
            }
        }
        let order0 = drivers[&ids[0]].node().sm().executed.clone();
        assert_eq!(order0.len(), 2, "both interfering commands executed");
        for id in &ids {
            assert_eq!(
                drivers[id].node().sm().executed,
                order0,
                "replica {id} agrees on order over cs-net"
            );
        }
    }
}
