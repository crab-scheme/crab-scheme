//! Durability + commit-notification tests for the `rocksdb-log` feature.
//!
//! These exercise the public seam the crab-cache Scheme layer relies on:
//! a `RocksLogStore`-backed `RaftNode` that survives a restart, the
//! persist-before-ack ordering invariant, and the driver's commit→ack bridge
//! (`Applied` to the proposer, `Committed` to a registered observer).
//!
//! Only built/run with `--features rocksdb-log`.
#![cfg(feature = "rocksdb-log")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_consensus::raft::{Config, Index, RaftNode};
use cs_consensus::sim::Cluster;
use cs_consensus::{Applied, Committed, RaftDriver, RaftLogStore, RocksLogStore};
use cs_consensus::{ReplicaId, StateMachine};
use tempfile::TempDir;

/// Sums i64 commands; the apply result is the running total (so an `Applied`
/// reply carries something we can assert on). `restore` reloads the total.
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

/// Drive a peer-less single-node driver until it has applied `target` commands
/// (single-node Raft commits immediately, but election takes a few ticks).
fn drive_until_applied(d: &mut RaftDriver<SumSm, RocksLogStore>, target: u64, budget: usize) {
    for _ in 0..budget {
        d.tick();
        d.poll();
        if d.node().last_applied() >= target {
            return;
        }
    }
    panic!(
        "node did not apply {} within budget (applied={})",
        target,
        d.node().last_applied()
    );
}

/// (a) Crash-recovery at the node level: a single-node group commits writes
/// durably, the process "crashes" (store dropped), and `restore_from` brings a
/// fresh node back at the same term/commit/applied with the state intact.
#[test]
fn node_resumes_from_durable_store_after_crash() {
    let dir = TempDir::new().unwrap();
    let id = ReplicaId(0);
    let voters = vec![id];

    let (committed_term, committed_commit, total_before) = {
        let store = RocksLogStore::open(dir.path()).unwrap();
        let node = RaftNode::new_with_store(
            id,
            voters.clone(),
            Config::default(),
            SumSm::default(),
            store,
        );
        let mut d = RaftDriver::new(node);
        // Elect (single-node) + commit two client writes durably.
        drive_until_applied(&mut d, 1, 50); // Noop committed after election
        d.propose(cmd(100));
        d.propose(cmd(23));
        // Single-node commits within propose; a few ticks settle bookkeeping.
        for _ in 0..5 {
            d.tick();
        }
        let n = d.node();
        assert_eq!(n.sm().total, 123, "state before crash");
        (n.current_term(), n.commit_index(), n.sm().total)
        // store dropped here → simulated kill -9
    };
    assert!(
        committed_commit >= 3,
        "expected Noop + 2 commands committed"
    );

    // Restart: reopen the SAME path and resume.
    let store = RocksLogStore::open(dir.path()).unwrap();
    let resumed = RaftNode::restore_from(id, voters, Config::default(), SumSm::default(), store);
    assert_eq!(resumed.current_term(), committed_term, "term recovered");
    assert_eq!(
        resumed.commit_index(),
        committed_commit,
        "commit index recovered"
    );
    assert_eq!(
        resumed.last_applied(),
        committed_commit,
        "replay to applied"
    );
    assert_eq!(
        resumed.sm().total,
        total_before,
        "state machine replayed from durable log"
    );
}

/// (b) Persist-before-ack: by the time `propose` returns the assigned index,
/// the entry is already durable in the store — re-opening the store at the same
/// path (without the in-flight node) shows the entry on disk. We also assert the
/// hard-state (commit) was synced no later than the ack.
#[test]
fn entry_is_durable_before_propose_returns() {
    let dir = TempDir::new().unwrap();
    let id = ReplicaId(0);
    let voters = vec![id];

    let store = RocksLogStore::open(dir.path()).unwrap();
    let node = RaftNode::new_with_store(id, voters, Config::default(), SumSm::default(), store);
    let mut d = RaftDriver::new(node);
    drive_until_applied(&mut d, 1, 50); // become leader

    // Propose; capture the index the ack would carry.
    let idx = d.propose(cmd(42)).expect("leader assigned an index");

    // Open a SECOND read-only handle to the same DB path is not allowed by
    // RocksDB (single-process lock), so instead assert against the live store:
    // the entry must be present in the store the instant propose returned.
    let st = d.node().store();
    assert_eq!(st.last_index(), idx, "entry persisted before ack");
    let persisted = st.entry(idx).expect("entry on disk before ack");
    assert_eq!(
        persisted.payload,
        cs_consensus::raft::EntryPayload::Command(cmd(42))
    );
    // Single-node commits within propose → hard-state commit must already cover
    // the entry (synced before the ack path).
    assert!(
        st.load_hard_state().commit_index >= idx,
        "hard-state commit synced before ack"
    );
}

/// (b') Stronger persist-before-ack across a real reopen: write through the
/// store, drop it (closing the DB + releasing the lock), reopen, and confirm
/// the last appended entry survived — i.e. the append fsynced before returning.
#[test]
fn appended_entry_survives_reopen() {
    let dir = TempDir::new().unwrap();
    let last = {
        let mut store = RocksLogStore::open(dir.path()).unwrap();
        store.append(&[cs_consensus::raft::Entry {
            term: 1,
            index: 1,
            payload: cs_consensus::raft::EntryPayload::Command(cmd(7)),
        }]);
        store.save_hard_state(cs_consensus::HardState {
            term: 1,
            voted_for: Some(ReplicaId(0)),
            commit_index: 1,
        });
        store.last_index()
    };
    assert_eq!(last, 1);
    let store = RocksLogStore::open(dir.path()).unwrap();
    assert_eq!(store.last_index(), 1, "synced append survived reopen");
    assert_eq!(store.load_hard_state().commit_index, 1);
    assert_eq!(
        store.entry(1).unwrap().payload,
        cs_consensus::raft::EntryPayload::Command(cmd(7))
    );
}

/// (c) Notify-on-commit: a registered observer receives a `Committed` message
/// per applied index, and the proposer receives an `Applied` (with its req_id +
/// result) for the index it proposed — over a RocksDB-backed node.
#[test]
fn driver_notifies_observer_and_proposer_on_commit() {
    // ActorSystem owns its own tokio runtime; keep the test thread outside any
    // runtime so shutdown() (which drops that runtime) doesn't panic.
    let system = cs_actor::ActorSystem::new();

    // Two collectors: one for Committed (observer), one for Applied (proposer).
    let committed: Arc<Mutex<Vec<Committed>>> = Arc::new(Mutex::new(Vec::new()));
    let applied: Arc<Mutex<Vec<Applied>>> = Arc::new(Mutex::new(Vec::new()));

    let committed_c = committed.clone();
    let observer = system.spawn_async(move |mut actor| async move {
        loop {
            match actor.receive_async().await {
                Some(cs_actor::Message::User(p)) => {
                    if let Some(c) = p.downcast_ref::<Committed>() {
                        committed_c.lock().unwrap().push(c.clone());
                    }
                }
                Some(_) => {}
                None => return,
            }
        }
    });

    let applied_c = applied.clone();
    let proposer = system.spawn_async(move |mut actor| async move {
        loop {
            match actor.receive_async().await {
                Some(cs_actor::Message::User(p)) => {
                    if let Some(a) = p.downcast_ref::<Applied>() {
                        applied_c.lock().unwrap().push(a.clone());
                    }
                }
                Some(_) => {}
                None => return,
            }
        }
    });

    let dir = TempDir::new().unwrap();
    let id = ReplicaId(0);
    let store = RocksLogStore::open(dir.path()).unwrap();
    let node = RaftNode::new_with_store(id, vec![id], Config::default(), SumSm::default(), store);
    let mut d = RaftDriver::new(node);
    d.set_observer(observer);

    // Elect, then propose with a reply target + req_id.
    drive_until_applied(&mut d, 1, 50);
    let idx = d
        .propose_with_reply(cmd(55), proposer, 7)
        .expect("leader assigned an index");
    // Drive so the command commits + applies + notifications fire.
    for _ in 0..5 {
        d.tick();
        d.poll();
    }

    // Give the actor runtime a moment to drain the mailboxes.
    let mut ok = false;
    for _ in 0..200 {
        std::thread::sleep(Duration::from_millis(5));
        if applied.lock().unwrap().iter().any(|a| a.req_id == 7)
            && committed.lock().unwrap().iter().any(|c| c.index == idx)
        {
            ok = true;
            break;
        }
    }
    assert!(ok, "observer + proposer received their notifications");

    // The proposer's Applied carries the SM result for index `idx` (total=55).
    let a = applied
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.req_id == 7)
        .cloned()
        .unwrap();
    assert_eq!(a.result, 55i64.to_le_bytes().to_vec());

    // The observer saw the command at `idx` (and the earlier Noop at index 1).
    let cs = committed.lock().unwrap();
    assert!(
        cs.iter().any(|c| c.index == idx && c.command == cmd(55)),
        "observer saw the committed command"
    );
    assert!(
        cs.iter().any(|c| c.index == 1 && c.command.is_empty()),
        "observer saw the leader's Noop (empty command) at index 1"
    );

    drop(cs);
    system.shutdown();
}

/// (a, multi-node) The durable store under real replication: a 3-node Sim
/// cluster — each replica RocksDB-backed — replicates commands, an isolated
/// follower misses them, then catches up via the log on heal. Finally one
/// replica's store is reopened to confirm the replicated log is on disk.
/// This exercises the AppendEntries splice/truncate paths through the store
/// seam (not just the single-node fast path).
#[test]
fn three_node_durable_cluster_replicates_and_survives_reopen() {
    let dirs: Vec<TempDir> = (0..3).map(|_| TempDir::new().unwrap()).collect();
    let ids: Vec<ReplicaId> = (0..3).map(ReplicaId).collect();

    let follower_path;
    {
        let nodes = ids.iter().enumerate().map(|(i, id)| {
            let store = RocksLogStore::open(dirs[i].path()).unwrap();
            RaftNode::new_with_store(*id, ids.clone(), Config::default(), SumSm::default(), store)
        });
        let mut c: Cluster<RaftNode<SumSm, RocksLogStore>> = Cluster::new(nodes);

        // Elect a leader.
        let mut leader = None;
        for _ in 0..60 {
            c.step();
            let ls: Vec<ReplicaId> = c
                .ids()
                .into_iter()
                .filter(|id| c.node(*id).is_leader())
                .collect();
            if ls.len() == 1 {
                leader = Some(ls[0]);
                break;
            }
        }
        let leader = leader.expect("a leader emerged");
        let follower = c.ids().into_iter().find(|id| *id != leader).unwrap();
        follower_path = dirs[follower.0 as usize].path().to_path_buf();

        // Isolate one follower, commit on the majority, confirm it lagged.
        c.isolate(follower);
        for v in [10i64, 20, 30] {
            c.act(leader, |n| n.propose(cmd(v)));
            c.deliver_all();
        }
        c.run(3);
        assert_eq!(c.node(follower).sm().total, 0, "isolated follower lagged");

        // Heal: the follower catches up via the replicated (durable) log.
        c.heal(follower);
        c.run(10);
        for id in c.ids() {
            assert_eq!(c.node(id).sm().total, 60, "replica {id} converged");
        }
        // The follower's durable store now holds the replicated tail.
        assert!(
            c.node(follower).store().last_index() >= 3,
            "follower persisted the replicated entries"
        );
        // cluster (and its stores) dropped here
    }

    // Reopen the (formerly-isolated) follower's store: the replicated log it
    // received over AppendEntries is durable across a restart.
    let store = RocksLogStore::open(&follower_path).unwrap();
    assert!(
        store.last_index() >= 3,
        "replicated log survived reopen (last_index={})",
        store.last_index()
    );
    let hs = store.load_hard_state();
    assert!(hs.term >= 1 && hs.commit_index >= 3, "hard-state persisted");
    // Spot-check a replicated command entry decodes.
    let some_cmd: Vec<Index> = (1..=store.last_index())
        .filter(|i| {
            matches!(
                store.entry(*i).map(|e| e.payload),
                Some(cs_consensus::raft::EntryPayload::Command(_))
            )
        })
        .collect();
    assert!(
        !some_cmd.is_empty(),
        "at least one durable Command entry present"
    );
}
