//! EPaxos (Egalitarian Paxos) consensus core.
//!
//! Leaderless consensus (Moraru, Andersen, Kaminsky — *There Is More
//! Consensus in Egalitarian Parliaments*, SOSP'13,
//! <https://www.cs.cmu.edu/~dga/papers/epaxos-sosp2013.pdf>). Unlike Raft
//! there is no single leader: any replica is the *command leader* for a
//! command it receives. Each command occupies an instance `(replica, slot)`
//! and carries a dependency set (the interfering commands it must execute
//! after) plus a sequence number that breaks ties inside dependency cycles.
//!
//! - **PreAccept** — the command leader assigns an instance, computes deps +
//!   seq from its own log, and asks a fast quorum.
//! - **Fast path** — if the whole fast quorum returns the leader's deps + seq
//!   unchanged, commit immediately (one round trip, no leader bottleneck).
//! - **Slow path** — otherwise take the union of deps / max of seq and run an
//!   explicit Accept round to a majority, then commit.
//! - **Execute** — committed commands are executed in dependency order: build
//!   the dependency graph, find strongly-connected components (Tarjan), run
//!   them in reverse-topological order, and within a component by `(seq,
//!   instance)`. Because every replica commits the *same* deps + seq per
//!   instance, every replica executes interfering commands in the same order.
//!
//! Like [`crate::raft`] this is a deterministic, I/O-free core driven by the
//! [`crate::sim`] harness. Explicit-prepare **recovery** of a failed command
//! leader is deferred (documented); ballots are carried but always 0 here.

use std::collections::{BTreeMap, BTreeSet};

use crate::sim::SimNode;
use crate::ReplicaId;

/// A monotonically increasing per-replica slot.
pub type Slot = u64;
/// A dependency sequence number (orders commands within a cycle).
pub type Seq = u64;
/// A ballot number (for recovery; always 0 in this core).
pub type Ballot = u64;

/// An instance of the command log: the `slot`-th command led by `replica`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instance {
    pub replica: ReplicaId,
    pub slot: Slot,
}

/// A replicated state machine for EPaxos: it defines command *interference*
/// (which commands must be ordered) and executes committed commands.
pub trait EpaxosStateMachine {
    /// Whether commands `a` and `b` conflict and must be ordered relative to
    /// each other. Non-interfering (commuting) commands need no ordering and
    /// can both take the fast path with empty deps.
    fn interferes(&self, a: &[u8], b: &[u8]) -> bool;

    /// Execute one committed command (in dependency order), returning an
    /// opaque result.
    fn execute(&mut self, command: &[u8]) -> Vec<u8>;
}

/// Lifecycle of an instance on a replica.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    PreAccepted,
    Accepted,
    Committed,
    Executed,
}

/// A replica's record of one instance.
#[derive(Clone, Debug)]
struct InstanceState {
    command: Vec<u8>,
    seq: Seq,
    deps: BTreeSet<Instance>,
    status: Status,
    ballot: Ballot,
}

/// EPaxos protocol messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    PreAccept {
        instance: Instance,
        ballot: Ballot,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    },
    PreAcceptReply {
        instance: Instance,
        ballot: Ballot,
        seq: Seq,
        deps: BTreeSet<Instance>,
    },
    Accept {
        instance: Instance,
        ballot: Ballot,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    },
    AcceptReply {
        instance: Instance,
        ballot: Ballot,
    },
    Commit {
        instance: Instance,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    },
}

type Out = (ReplicaId, Message);

/// Per-instance leader bookkeeping while collecting replies.
#[derive(Debug)]
struct Collect {
    command: Vec<u8>,
    init_seq: Seq,
    init_deps: BTreeSet<Instance>,
    /// Replies received so far (the leader counts itself).
    replies: usize,
    /// Replies that matched the leader's initial seq+deps exactly.
    agreed: usize,
    union_seq: Seq,
    union_deps: BTreeSet<Instance>,
    accepting: bool,
    accept_oks: usize,
}

/// A single EPaxos replica.
#[derive(Debug)]
pub struct EpaxosReplica<SM: EpaxosStateMachine> {
    id: ReplicaId,
    replicas: Vec<ReplicaId>,
    next_slot: Slot,
    cmds: BTreeMap<Instance, InstanceState>,
    collects: BTreeMap<Instance, Collect>,
    sm: SM,
}

impl<SM: EpaxosStateMachine> EpaxosReplica<SM> {
    /// Create a replica in the `replicas` set (which must contain `id`).
    pub fn new(id: ReplicaId, replicas: Vec<ReplicaId>, sm: SM) -> Self {
        let mut replicas = replicas;
        replicas.sort_unstable();
        replicas.dedup();
        assert!(replicas.contains(&id), "replica set must include self");
        EpaxosReplica {
            id,
            replicas,
            next_slot: 0,
            cmds: BTreeMap::new(),
            collects: BTreeMap::new(),
            sm,
        }
    }

    pub fn id(&self) -> ReplicaId {
        self.id
    }
    pub fn sm(&self) -> &SM {
        &self.sm
    }

    fn peers(&self) -> impl Iterator<Item = ReplicaId> + '_ {
        let me = self.id;
        self.replicas.iter().copied().filter(move |r| *r != me)
    }
    fn slow_quorum(&self) -> usize {
        self.replicas.len() / 2 + 1
    }
    /// EPaxos fast-path quorum: `F + ⌊(F+1)/2⌋` for `N = 2F+1`.
    fn fast_quorum(&self) -> usize {
        let n = self.replicas.len();
        let f = (n - 1) / 2;
        f + f.div_ceil(2)
    }

    fn status_of(&self, i: &Instance) -> Option<Status> {
        self.cmds.get(i).map(|s| s.status)
    }

    /// Compute `(seq, deps)` for `command`, attributing interference against
    /// every instance already known (except `exclude`).
    fn deps_and_seq(&self, command: &[u8], exclude: Instance) -> (Seq, BTreeSet<Instance>) {
        let mut deps = BTreeSet::new();
        let mut max_seq = 0;
        for (inst, st) in &self.cmds {
            if *inst == exclude {
                continue;
            }
            if self.sm.interferes(command, &st.command) {
                deps.insert(*inst);
                max_seq = max_seq.max(st.seq);
            }
        }
        (max_seq + 1, deps)
    }

    /// Record (insert or overwrite) an instance's state.
    fn record(
        &mut self,
        inst: Instance,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
        status: Status,
        ballot: Ballot,
    ) {
        let entry = self.cmds.entry(inst).or_insert_with(|| InstanceState {
            command: command.clone(),
            seq,
            deps: deps.clone(),
            status,
            ballot,
        });
        // Don't regress a more-advanced status (e.g. a late PreAccept after a
        // Commit).
        if status >= entry.status {
            entry.command = command;
            entry.seq = seq;
            entry.deps = deps;
            entry.status = status;
            entry.ballot = ballot;
        }
    }

    // ---- public driving API ----

    /// Lead a new `command`: assign an instance, compute deps + seq, and start
    /// the PreAccept round. Returns the messages to send.
    pub fn propose(&mut self, command: Vec<u8>) -> Vec<Out> {
        let inst = Instance {
            replica: self.id,
            slot: self.next_slot,
        };
        self.next_slot += 1;
        let (seq, deps) = self.deps_and_seq(&command, inst);
        self.record(
            inst,
            command.clone(),
            seq,
            deps.clone(),
            Status::PreAccepted,
            0,
        );
        self.collects.insert(
            inst,
            Collect {
                command: command.clone(),
                init_seq: seq,
                init_deps: deps.clone(),
                replies: 1, // self
                agreed: 1,  // self trivially agrees
                union_seq: seq,
                union_deps: deps.clone(),
                accepting: false,
                accept_oks: 1,
            },
        );
        let msg = Message::PreAccept {
            instance: inst,
            ballot: 0,
            command,
            seq,
            deps,
        };
        self.peers().map(|p| (p, msg.clone())).collect()
    }

    /// Handle one inbound protocol message.
    pub fn on_message(&mut self, from: ReplicaId, msg: Message) -> Vec<Out> {
        match msg {
            Message::PreAccept {
                instance,
                ballot,
                command,
                seq,
                deps,
            } => self.on_preaccept(from, instance, ballot, command, seq, deps),
            Message::PreAcceptReply {
                instance,
                ballot,
                seq,
                deps,
            } => self.on_preaccept_reply(instance, ballot, seq, deps),
            Message::Accept {
                instance,
                ballot,
                command,
                seq,
                deps,
            } => self.on_accept(from, instance, ballot, command, seq, deps),
            Message::AcceptReply { instance, ballot } => self.on_accept_reply(instance, ballot),
            Message::Commit {
                instance,
                command,
                seq,
                deps,
            } => self.on_commit(instance, command, seq, deps),
        }
    }

    fn on_preaccept(
        &mut self,
        from: ReplicaId,
        inst: Instance,
        ballot: Ballot,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    ) -> Vec<Out> {
        // Augment the leader's deps/seq with our own interfering instances.
        let (local_seq, local_deps) = self.deps_and_seq(&command, inst);
        let merged_seq = seq.max(local_seq);
        let mut merged_deps = deps;
        merged_deps.extend(local_deps);
        self.record(
            inst,
            command,
            merged_seq,
            merged_deps.clone(),
            Status::PreAccepted,
            ballot,
        );
        vec![(
            from,
            Message::PreAcceptReply {
                instance: inst,
                ballot,
                seq: merged_seq,
                deps: merged_deps,
            },
        )]
    }

    fn on_preaccept_reply(
        &mut self,
        inst: Instance,
        _ballot: Ballot,
        seq: Seq,
        deps: BTreeSet<Instance>,
    ) -> Vec<Out> {
        let Some(c) = self.collects.get_mut(&inst) else {
            return Vec::new(); // already decided / not the leader
        };
        if c.accepting {
            return Vec::new(); // already in the slow path
        }
        c.replies += 1;
        if seq == c.init_seq && deps == c.init_deps {
            c.agreed += 1;
        }
        c.union_seq = c.union_seq.max(seq);
        c.union_deps.extend(deps);

        let fq = self.fast_quorum();
        let sq = self.slow_quorum();
        let c = self.collects.get(&inst).expect("collect");
        if c.replies < fq {
            return Vec::new(); // keep collecting
        }
        if c.agreed >= fq {
            // Fast path: the whole fast quorum agreed with the leader.
            let (command, seq, deps) = (c.command.clone(), c.init_seq, c.init_deps.clone());
            return self.commit_as_leader(inst, command, seq, deps);
        }
        // Slow path: run an explicit Accept with the union.
        if c.replies >= sq {
            let (command, seq, deps) = (c.command.clone(), c.union_seq, c.union_deps.clone());
            let c = self.collects.get_mut(&inst).expect("collect");
            c.accepting = true;
            c.accept_oks = 1; // self
            self.record(
                inst,
                command.clone(),
                seq,
                deps.clone(),
                Status::Accepted,
                0,
            );
            let msg = Message::Accept {
                instance: inst,
                ballot: 0,
                command,
                seq,
                deps,
            };
            return self.peers().map(|p| (p, msg.clone())).collect();
        }
        Vec::new()
    }

    fn on_accept(
        &mut self,
        from: ReplicaId,
        inst: Instance,
        ballot: Ballot,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    ) -> Vec<Out> {
        self.record(inst, command, seq, deps, Status::Accepted, ballot);
        vec![(
            from,
            Message::AcceptReply {
                instance: inst,
                ballot,
            },
        )]
    }

    fn on_accept_reply(&mut self, inst: Instance, _ballot: Ballot) -> Vec<Out> {
        let sq = self.slow_quorum();
        let ready = {
            let Some(c) = self.collects.get_mut(&inst) else {
                return Vec::new();
            };
            if !c.accepting {
                return Vec::new();
            }
            c.accept_oks += 1;
            if c.accept_oks < sq {
                return Vec::new();
            }
            (c.command.clone(), c.union_seq, c.union_deps.clone())
        };
        self.commit_as_leader(inst, ready.0, ready.1, ready.2)
    }

    fn commit_as_leader(
        &mut self,
        inst: Instance,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    ) -> Vec<Out> {
        self.collects.remove(&inst);
        self.record(
            inst,
            command.clone(),
            seq,
            deps.clone(),
            Status::Committed,
            0,
        );
        self.execute_committed();
        let msg = Message::Commit {
            instance: inst,
            command,
            seq,
            deps,
        };
        self.peers().map(|p| (p, msg.clone())).collect()
    }

    fn on_commit(
        &mut self,
        inst: Instance,
        command: Vec<u8>,
        seq: Seq,
        deps: BTreeSet<Instance>,
    ) -> Vec<Out> {
        self.record(inst, command, seq, deps, Status::Committed, 0);
        self.execute_committed();
        Vec::new()
    }

    // ---- execution: dependency-graph order ----

    /// Execute every instance that is now executable: committed, with all
    /// transitive dependencies committed too. Components are run in
    /// reverse-topological order; ties inside a strongly-connected component
    /// break by `(seq, instance)`. Deterministic across replicas because the
    /// committed `(seq, deps)` are identical everywhere.
    fn execute_committed(&mut self) {
        // The candidate set: committed-but-not-executed instances all of whose
        // transitive deps are committed (else they must wait). Computed as a
        // fixpoint: drop any instance with a dep that isn't committed, or whose
        // dep was dropped, until stable.
        let mut eligible: BTreeSet<Instance> = self
            .cmds
            .iter()
            .filter(|(_, s)| s.status == Status::Committed)
            .map(|(i, _)| *i)
            .collect();
        loop {
            let mut removed = false;
            let snapshot: Vec<Instance> = eligible.iter().copied().collect();
            for inst in snapshot {
                let deps = &self.cmds[&inst].deps;
                let blocked = deps.iter().any(|d| match self.status_of(d) {
                    Some(Status::Executed) => false,          // already done
                    Some(_) if eligible.contains(d) => false, // in this batch
                    _ => true, // not committed / not eligible → blocks
                });
                if blocked {
                    eligible.remove(&inst);
                    removed = true;
                }
            }
            if !removed {
                break;
            }
        }
        if eligible.is_empty() {
            return;
        }

        // Tarjan SCC over the eligible subgraph (edges instance → dep-in-set).
        let order = tarjan_scc(&eligible, |i| {
            self.cmds[&i]
                .deps
                .iter()
                .copied()
                .filter(|d| eligible.contains(d))
                .collect()
        });
        // `order` lists SCCs in reverse-topological order (dependencies first),
        // which is exactly execution order. Within an SCC, sort by (seq, inst).
        for mut scc in order {
            scc.sort_by_key(|i| (self.cmds[i].seq, *i));
            for inst in scc {
                if self.cmds[&inst].status == Status::Executed {
                    continue;
                }
                let cmd = self.cmds[&inst].command.clone();
                self.sm.execute(&cmd);
                self.cmds.get_mut(&inst).expect("inst").status = Status::Executed;
            }
        }
    }
}

/// Tarjan's strongly-connected-components over `nodes`, with `succ(n)` giving
/// the out-edges (kept within `nodes`). Returns SCCs in reverse-topological
/// order: a component appears before any component that depends on it.
fn tarjan_scc(
    nodes: &BTreeSet<Instance>,
    succ: impl Fn(Instance) -> Vec<Instance>,
) -> Vec<Vec<Instance>> {
    #[derive(Clone, Copy)]
    struct NodeInfo {
        index: u32,
        lowlink: u32,
        on_stack: bool,
    }
    let mut info: BTreeMap<Instance, NodeInfo> = BTreeMap::new();
    let mut stack: Vec<Instance> = Vec::new();
    let mut sccs: Vec<Vec<Instance>> = Vec::new();
    let mut next_index: u32 = 0;

    // Iterative Tarjan (avoids recursion / stack overflow on long chains).
    // Each frame tracks the node and an index into its successor list.
    for &start in nodes {
        if info.contains_key(&start) {
            continue;
        }
        let mut call: Vec<(Instance, usize, Vec<Instance>)> = vec![(start, 0, succ(start))];
        while let Some((v, i, succs)) = call.last().cloned() {
            if i == 0 {
                info.insert(
                    v,
                    NodeInfo {
                        index: next_index,
                        lowlink: next_index,
                        on_stack: true,
                    },
                );
                next_index += 1;
                stack.push(v);
            }
            if i < succs.len() {
                // Advance this frame's cursor.
                call.last_mut().unwrap().1 += 1;
                let w = succs[i];
                match info.get(&w) {
                    None => {
                        call.push((w, 0, succ(w)));
                    }
                    Some(wi) if wi.on_stack => {
                        let w_index = wi.index;
                        let vi = info.get_mut(&v).unwrap();
                        vi.lowlink = vi.lowlink.min(w_index);
                    }
                    Some(_) => {}
                }
            } else {
                // Done with v: if it's an SCC root, pop the component.
                let v_info = info[&v];
                if v_info.lowlink == v_info.index {
                    let mut comp = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        info.get_mut(&w).unwrap().on_stack = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    sccs.push(comp);
                }
                call.pop();
                if let Some((parent, _, _)) = call.last() {
                    let parent = *parent;
                    let v_low = info[&v].lowlink;
                    let pi = info.get_mut(&parent).unwrap();
                    pi.lowlink = pi.lowlink.min(v_low);
                }
            }
        }
    }
    // Tarjan emits SCCs in reverse-topological order already.
    sccs
}

impl<SM: EpaxosStateMachine> SimNode for EpaxosReplica<SM> {
    type Msg = Message;
    fn id(&self) -> ReplicaId {
        self.id
    }
    fn on_tick(&mut self) -> Vec<(ReplicaId, Message)> {
        Vec::new() // no timers in the core (recovery is deferred)
    }
    fn on_message(&mut self, from: ReplicaId, msg: Message) -> Vec<(ReplicaId, Message)> {
        EpaxosReplica::on_message(self, from, msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::Cluster;

    /// Commands are `[key, value]`; two commands interfere iff same key.
    /// The SM records the execution order so tests can compare across replicas.
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

    fn cluster(n: u64) -> Cluster<EpaxosReplica<KvSm>> {
        let ids: Vec<ReplicaId> = (0..n).map(ReplicaId).collect();
        Cluster::new(
            ids.iter()
                .map(|id| EpaxosReplica::new(*id, ids.clone(), KvSm::default())),
        )
    }

    fn cmd(key: u8, val: u8) -> Vec<u8> {
        vec![key, val]
    }

    fn executed(c: &Cluster<EpaxosReplica<KvSm>>, id: ReplicaId) -> Vec<Vec<u8>> {
        c.node(id).sm().executed.clone()
    }

    #[test]
    fn non_interfering_commands_commit_and_execute_everywhere() {
        let mut c = cluster(3);
        // Different keys → no interference → both fast-path.
        c.act(ReplicaId(0), |n| ((), n.propose(cmd(1, 10))));
        c.act(ReplicaId(1), |n| ((), n.propose(cmd(2, 20))));
        c.deliver_all();
        for id in c.ids() {
            let ex = executed(&c, id);
            assert_eq!(ex.len(), 2, "replica {id} executed both");
            assert!(ex.contains(&cmd(1, 10)) && ex.contains(&cmd(2, 20)));
        }
    }

    #[test]
    fn concurrent_interfering_commands_execute_in_one_order_everywhere() {
        let mut c = cluster(3);
        // Same key, proposed concurrently at two replicas: neither sees the
        // other on PreAccept → slow path → mutual deps → one SCC, ordered by
        // (seq, instance) identically on every replica.
        c.act(ReplicaId(0), |n| ((), n.propose(cmd(7, 100))));
        c.act(ReplicaId(1), |n| ((), n.propose(cmd(7, 200))));
        c.deliver_all();

        let order0 = executed(&c, ReplicaId(0));
        assert_eq!(order0.len(), 2, "both interfering commands executed");
        for id in c.ids() {
            assert_eq!(
                executed(&c, id),
                order0,
                "replica {id} agrees on execution order"
            );
        }
    }

    #[test]
    fn dependency_chain_executes_in_causal_order() {
        let mut c = cluster(3);
        // Three same-key commands proposed one after another (each settles
        // before the next), forming a dep chain a → b → c.
        c.act(ReplicaId(0), |n| ((), n.propose(cmd(5, 1))));
        c.deliver_all();
        c.act(ReplicaId(1), |n| ((), n.propose(cmd(5, 2))));
        c.deliver_all();
        c.act(ReplicaId(2), |n| ((), n.propose(cmd(5, 3))));
        c.deliver_all();

        let expected = vec![cmd(5, 1), cmd(5, 2), cmd(5, 3)];
        for id in c.ids() {
            assert_eq!(executed(&c, id), expected, "replica {id} causal order");
        }
    }

    #[test]
    fn five_node_cluster_orders_interfering_commands_consistently() {
        let mut c = cluster(5);
        // Three concurrent same-key commands across a 5-node cluster.
        c.act(ReplicaId(0), |n| ((), n.propose(cmd(3, 1))));
        c.act(ReplicaId(2), |n| ((), n.propose(cmd(3, 2))));
        c.act(ReplicaId(4), |n| ((), n.propose(cmd(3, 3))));
        c.deliver_all();

        let order0 = executed(&c, ReplicaId(0));
        assert_eq!(order0.len(), 3, "all three executed");
        for id in c.ids() {
            assert_eq!(executed(&c, id), order0, "replica {id} consistent order");
        }
    }
}
