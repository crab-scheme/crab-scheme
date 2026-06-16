//! Per-node message router + remote actor reference (SDK M02.E).
//!
//! A [`Router`] owns one node's view of the cluster: a transport to each
//! peer (keyed by `name@host`, with the peer's live restart epoch) plus a
//! local inbox. [`Router::send`] decides local vs remote per [`DistPid`]:
//!
//! - **local** (`pid.node == self`) → delivered straight to the local inbox
//!   (loopback);
//! - **remote** → the Pid + payload are framed and written to the peer's
//!   `Messages` channel; a stale-epoch target is rejected with
//!   [`DistribError::EpochMismatch`], an unknown one with `NoTransport`.
//!
//! [`Router::poll`] drains every peer transport's `Messages` channel,
//! decodes `(DistPid, payload)`, and delivers to the local inbox — so a
//! `(send pid msg)` on node A lands in node B's inbox. [`RemoteRef`] is the
//! `ActorRef`-shaped handle (`.send(payload)`); the cs-actor / Scheme
//! `(send pid msg)` binding wraps it (later integration).
//!
//! The byte payload is opaque here — message *encoding* (cs-runtime's
//! `SendableValue`) layers on top. This keeps the transport + routing
//! mechanics deterministically testable on their own.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use cs_net::{Channel, Transport, TransportError};

use crate::{DistPid, DistribError, NodeId};

/// A connected peer: its live identity + the transport to reach it.
struct Peer {
    node: NodeId,
    transport: Box<dyn Transport>,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer").field("node", &self.node).finish()
    }
}

/// Why a monitored remote Pid went DOWN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownReason {
    /// The transport to the Pid's node dropped (Erlang's `noconnection`).
    NoConnection,
}

impl DownReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            DownReason::NoConnection => "noconnection",
        }
    }
}

impl std::fmt::Display for DownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A DOWN notification: `watcher` was monitoring `monitored`, which is now
/// unreachable. Delivered to the watcher's local actor as the equivalent of
/// `('down ref pid reason)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownNotice {
    pub watcher: DistPid,
    pub monitored: DistPid,
    pub reason: DownReason,
}

/// A registered monitor: `watcher` (local) watches `target` (remote).
#[derive(Debug, Clone)]
struct Monitor {
    watcher: DistPid,
    target: DistPid,
}

/// One node's message router.
#[derive(Debug)]
pub struct Router {
    node: NodeId,
    /// Peers keyed by `name@host` (epoch-independent) so a restarted peer
    /// replaces its predecessor and stale-epoch sends are caught.
    peers: Mutex<HashMap<String, Peer>>,
    /// Inbound messages destined for local actors on this node (the default
    /// `Messages` channel — back-compat for the single-poller path).
    inbox: Mutex<VecDeque<(DistPid, Vec<u8>)>>,
    /// cw-gx4: per-cs-net-channel inboxes, so independent consumers (e.g. one
    /// peer-poller PER Raft group) can drain in parallel instead of serializing
    /// on one channel. Indexed by `Channel as usize` (0..6); only the
    /// shard channels {1,3,4,5} (Consensus/Workflow/Bulk/Observability) are used
    /// — Control=0 is reserved for handshake/gossip, Messages=2 stays on `inbox`.
    chan_inboxes: [Mutex<VecDeque<(DistPid, Vec<u8>)>>; 6],
    /// Active monitors of remote Pids (fire DOWN on disconnect).
    monitors: Mutex<Vec<Monitor>>,
    /// DOWN notices ready for local delivery.
    down_inbox: Mutex<VecDeque<DownNotice>>,
}

/// cw-gx4: the cs-net channels usable for per-group Raft traffic (NOT Control=0,
/// NOT Messages=2 which is the back-compat default). 4-way parallelism.
pub const SHARD_CHANNELS: [u8; 4] = [1, 3, 4, 5];

fn channel_of(ch: u8) -> Channel {
    Channel::ALL[(ch as usize) % Channel::ALL.len()]
}

impl Router {
    pub fn new(node: NodeId) -> Self {
        Router {
            node,
            peers: Mutex::new(HashMap::new()),
            inbox: Mutex::new(VecDeque::new()),
            chan_inboxes: std::array::from_fn(|_| Mutex::new(VecDeque::new())),
            monitors: Mutex::new(Vec::new()),
            down_inbox: Mutex::new(VecDeque::new()),
        }
    }

    /// This router's node identity.
    pub fn node(&self) -> &NodeId {
        &self.node
    }

    /// Number of peers currently registered. Used to wait for a cluster's
    /// connections to be fully established before driving traffic (peers are
    /// added asynchronously on the accepting side of a TCP connection).
    pub fn peer_count(&self) -> usize {
        self.peers.lock().expect("peers poisoned").len()
    }

    /// Labels of all currently-registered peers (cw-lkq.6: lets the etcd
    /// MemberAdd handler resolve a joiner's node name from the mesh — the
    /// joiner dialed in under its real name).
    pub fn peer_labels(&self) -> Vec<String> {
        self.peers
            .lock()
            .expect("peers poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// Register (or replace) the transport to `peer`.
    pub fn add_peer(&self, peer: NodeId, transport: Box<dyn Transport>) {
        self.peers.lock().expect("peers poisoned").insert(
            peer.label(),
            Peer {
                node: peer,
                transport,
            },
        );
    }

    /// Route `payload` to `target`. Local targets loop back to the inbox;
    /// remote targets are framed `(pid ‖ payload)` onto the peer's
    /// `Messages` channel.
    pub fn send(&self, target: &DistPid, payload: &[u8]) -> Result<(), DistribError> {
        if target.node == self.node {
            self.deliver_local(target.clone(), payload.to_vec());
            return Ok(());
        }
        let peers = self.peers.lock().expect("peers poisoned");
        let peer = peers
            .get(&target.node.label())
            .ok_or_else(|| DistribError::NoTransport(target.node.label()))?;
        if peer.node.epoch != target.node.epoch {
            return Err(DistribError::EpochMismatch {
                expected: peer.node.epoch,
                got: target.node.epoch,
            });
        }
        let mut frame = target.encode_vec();
        frame.extend_from_slice(payload);
        peer.transport.send(Channel::Messages, &frame)?;
        Ok(())
    }

    /// cw-gx4: send on an explicit cs-net channel (one Raft group → one
    /// channel) so independent groups don't serialize on `Messages`. `ch` is a
    /// `Channel as u8` (use the {1,3,4,5} shard channels). Local targets are
    /// delivered to the matching per-channel inbox.
    pub fn send_ch(&self, target: &DistPid, payload: &[u8], ch: u8) -> Result<(), DistribError> {
        if target.node == self.node {
            self.deliver_local_ch(ch, target.clone(), payload.to_vec());
            return Ok(());
        }
        let peers = self.peers.lock().expect("peers poisoned");
        let peer = peers
            .get(&target.node.label())
            .ok_or_else(|| DistribError::NoTransport(target.node.label()))?;
        if peer.node.epoch != target.node.epoch {
            return Err(DistribError::EpochMismatch {
                expected: peer.node.epoch,
                got: target.node.epoch,
            });
        }
        let mut frame = target.encode_vec();
        frame.extend_from_slice(payload);
        peer.transport.send(channel_of(ch), &frame)?;
        Ok(())
    }

    /// Drain every peer's `Messages` channel into the local inbox. Returns
    /// the number of messages delivered. A closed peer is skipped (the
    /// failure detector / DOWN path handles disconnects).
    pub fn poll(&self) -> Result<usize, DistribError> {
        let peers = self.peers.lock().expect("peers poisoned");
        let mut delivered = 0;
        let mut inbound: Vec<(DistPid, Vec<u8>)> = Vec::new();
        for peer in peers.values() {
            loop {
                match peer.transport.try_recv(Channel::Messages) {
                    Ok(Some(frame)) => {
                        let (pid, consumed) = DistPid::decode(&frame)?;
                        inbound.push((pid, frame[consumed..].to_vec()));
                        delivered += 1;
                    }
                    Ok(None) | Err(TransportError::PeerClosed) => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        // cw-gx4: also drain the per-group shard channels into their own
        // inboxes, so a per-group poller can read just its channel.
        let mut chan_inbound: Vec<(u8, DistPid, Vec<u8>)> = Vec::new();
        for &ch in SHARD_CHANNELS.iter() {
            for peer in peers.values() {
                loop {
                    match peer.transport.try_recv(channel_of(ch)) {
                        Ok(Some(frame)) => {
                            let (pid, consumed) = DistPid::decode(&frame)?;
                            chan_inbound.push((ch, pid, frame[consumed..].to_vec()));
                            delivered += 1;
                        }
                        Ok(None) | Err(TransportError::PeerClosed) => break,
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
        drop(peers);
        for msg in inbound {
            self.deliver_local(msg.0, msg.1);
        }
        for (ch, pid, payload) in chan_inbound {
            self.deliver_local_ch(ch, pid, payload);
        }
        Ok(delivered)
    }

    fn deliver_local(&self, pid: DistPid, payload: Vec<u8>) {
        self.inbox
            .lock()
            .expect("inbox poisoned")
            .push_back((pid, payload));
    }

    fn deliver_local_ch(&self, ch: u8, pid: DistPid, payload: Vec<u8>) {
        self.chan_inboxes[(ch as usize) % self.chan_inboxes.len()]
            .lock()
            .expect("chan inbox poisoned")
            .push_back((pid, payload));
    }

    /// Pop the next inbound message for a local actor, if any. (Stands in
    /// for cs-actor mailbox delivery until that integration lands.)
    pub fn recv_local(&self) -> Option<(DistPid, Vec<u8>)> {
        self.inbox.lock().expect("inbox poisoned").pop_front()
    }

    /// cw-gx4: pop the next inbound message on a specific shard channel.
    pub fn recv_local_channel(&self, ch: u8) -> Option<(DistPid, Vec<u8>)> {
        self.chan_inboxes[(ch as usize) % self.chan_inboxes.len()]
            .lock()
            .expect("chan inbox poisoned")
            .pop_front()
    }

    /// Register that local `watcher` monitors remote `target`. If the
    /// transport to `target`'s node later drops, a [`DownNotice`] with
    /// reason [`DownReason::NoConnection`] is queued for `watcher`.
    pub fn monitor(&self, watcher: DistPid, target: DistPid) {
        self.monitors
            .lock()
            .expect("monitors poisoned")
            .push(Monitor { watcher, target });
    }

    /// Scan peers for dropped connections and fire DOWN for every monitor
    /// of a Pid on a now-closed node. Returns the number of DOWN notices
    /// fired. Closed peers + their monitors are removed (DOWN fires once).
    pub fn detect_disconnects(&self) -> usize {
        let mut peers = self.peers.lock().expect("peers poisoned");
        let down_nodes: Vec<String> = peers
            .iter()
            .filter(|(_, p)| p.transport.is_closed())
            .map(|(label, _)| label.clone())
            .collect();
        if down_nodes.is_empty() {
            return 0;
        }
        for label in &down_nodes {
            peers.remove(label);
        }
        drop(peers);

        let mut monitors = self.monitors.lock().expect("monitors poisoned");
        let mut down = self.down_inbox.lock().expect("down_inbox poisoned");
        let mut fired = 0;
        monitors.retain(|m| {
            if down_nodes.contains(&m.target.node.label()) {
                down.push_back(DownNotice {
                    watcher: m.watcher.clone(),
                    monitored: m.target.clone(),
                    reason: DownReason::NoConnection,
                });
                fired += 1;
                false // monitor consumed
            } else {
                true
            }
        });
        fired
    }

    /// Pop the next DOWN notice ready for local delivery, if any.
    pub fn recv_down(&self) -> Option<DownNotice> {
        self.down_inbox
            .lock()
            .expect("down_inbox poisoned")
            .pop_front()
    }

    /// Close the transport to `peer_label` (`name@host`), as on a graceful
    /// leave or a detected link failure. The drop is observed by the next
    /// [`Self::detect_disconnects`]. Returns whether a peer matched.
    pub fn disconnect_peer(&self, peer_label: &str) -> bool {
        let peers = self.peers.lock().expect("peers poisoned");
        match peers.get(peer_label) {
            Some(p) => {
                let _ = p.transport.close();
                true
            }
            None => false,
        }
    }
}

/// An `ActorRef`-shaped handle to a (possibly remote) actor. `send` routes
/// through the owning [`Router`] — local or remote is transparent, so
/// source-level `(send pid msg)` is unchanged.
#[derive(Debug, Clone)]
pub struct RemoteRef {
    router: Arc<Router>,
    pid: DistPid,
}

impl RemoteRef {
    pub fn new(router: Arc<Router>, pid: DistPid) -> Self {
        RemoteRef { router, pid }
    }

    pub fn pid(&self) -> &DistPid {
        &self.pid
    }

    /// Send an (opaque) payload to the referenced actor.
    pub fn send(&self, payload: &[u8]) -> Result<(), DistribError> {
        self.router.send(&self.pid, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_net::sim::SimPair;

    fn node(name: &str) -> NodeId {
        NodeId::new(name, "localhost:7000", 1)
    }

    /// Wire two routers together over a Sim transport pair.
    fn connect(a: &Router, b: &Router) {
        let (ea, eb) = SimPair::new(a.node().label(), b.node().label()).into_endpoints();
        a.add_peer(b.node().clone(), Box::new(ea));
        b.add_peer(a.node().clone(), Box::new(eb));
    }

    #[test]
    fn local_send_loops_back() {
        let r = Router::new(node("a"));
        let pid = DistPid::new(node("a"), 1);
        r.send(&pid, b"self-msg").unwrap();
        assert_eq!(r.recv_local(), Some((pid, b"self-msg".to_vec())));
    }

    #[test]
    fn remote_send_arrives_after_poll() {
        let a = Router::new(node("a"));
        let b = Router::new(node("b"));
        connect(&a, &b);
        let pid_on_b = DistPid::new(node("b"), 42);
        a.send(&pid_on_b, b"ping").unwrap();
        // Not delivered until the receiver polls its transports.
        assert_eq!(b.recv_local(), None);
        assert_eq!(b.poll().unwrap(), 1);
        assert_eq!(b.recv_local(), Some((pid_on_b, b"ping".to_vec())));
    }

    #[test]
    fn unknown_peer_is_no_transport() {
        let a = Router::new(node("a"));
        let pid_on_b = DistPid::new(node("b"), 1);
        assert!(matches!(
            a.send(&pid_on_b, b"x"),
            Err(DistribError::NoTransport(_))
        ));
    }

    #[test]
    fn stale_epoch_is_rejected() {
        let a = Router::new(node("a"));
        let b = Router::new(node("b")); // epoch 1
        connect(&a, &b);
        // A Pid minted for a *previous* incarnation (epoch 0).
        let stale = DistPid::new(NodeId::new("b", "localhost:7000", 0), 7);
        match a.send(&stale, b"x") {
            Err(DistribError::EpochMismatch { expected, got }) => {
                assert_eq!(expected, 1);
                assert_eq!(got, 0);
            }
            other => panic!("expected EpochMismatch, got {other:?}"),
        }
    }

    #[test]
    fn three_node_cluster_ping_pong_all_pairs() {
        // Acceptance: 3-node cluster forms via Sim transport; ping/pong
        // across all pairs.
        let nodes = ["a", "b", "c"];
        let routers: Vec<Arc<Router>> = nodes
            .iter()
            .map(|n| Arc::new(Router::new(node(n))))
            .collect();
        // Fully connect.
        for i in 0..routers.len() {
            for j in (i + 1)..routers.len() {
                connect(&routers[i], &routers[j]);
            }
        }
        // Every ordered pair (sender → receiver) exchanges ping/pong.
        for (si, sender) in routers.iter().enumerate() {
            for (ri, receiver) in routers.iter().enumerate() {
                if si == ri {
                    continue;
                }
                let pid_on_recv = DistPid::new(node(nodes[ri]), 100 + ri as u64);
                sender.send(&pid_on_recv, b"ping").unwrap();
                assert_eq!(receiver.poll().unwrap(), 1, "{}→{}", nodes[si], nodes[ri]);
                let (got_pid, payload) = receiver.recv_local().expect("ping delivered");
                assert_eq!(got_pid, pid_on_recv);
                assert_eq!(payload, b"ping");
                // Pong back to a pid on the sender.
                let pid_on_sender = DistPid::new(node(nodes[si]), 200 + si as u64);
                receiver.send(&pid_on_sender, b"pong").unwrap();
                assert_eq!(sender.poll().unwrap(), 1);
                assert_eq!(sender.recv_local(), Some((pid_on_sender, b"pong".to_vec())));
            }
        }
    }

    #[test]
    fn remote_ref_send_is_transparent() {
        let a = Arc::new(Router::new(node("a")));
        let b = Arc::new(Router::new(node("b")));
        connect(&a, &b);
        let r = RemoteRef::new(a.clone(), DistPid::new(node("b"), 9));
        r.send(b"via-ref").unwrap();
        b.poll().unwrap();
        assert_eq!(b.recv_local(), Some((r.pid().clone(), b"via-ref".to_vec())));
    }

    #[test]
    fn disconnect_fires_down_for_monitored_remote_pid() {
        let a = Router::new(node("a"));
        let b = Router::new(node("b"));
        connect(&a, &b);
        let remote = DistPid::new(node("b"), 77);
        let watcher = DistPid::new(node("a"), 1);
        a.monitor(watcher.clone(), remote.clone());
        // No disconnect yet → nothing fires.
        assert_eq!(a.detect_disconnects(), 0);
        assert_eq!(a.recv_down(), None);
        // Link drops.
        assert!(a.disconnect_peer(&node("b").label()));
        assert_eq!(a.detect_disconnects(), 1);
        assert_eq!(
            a.recv_down(),
            Some(DownNotice {
                watcher,
                monitored: remote,
                reason: DownReason::NoConnection,
            })
        );
        // Fires once: the monitor + peer are consumed.
        assert_eq!(a.detect_disconnects(), 0);
        assert_eq!(a.recv_down(), None);
    }

    #[test]
    fn disconnect_without_monitor_fires_nothing() {
        let a = Router::new(node("a"));
        let b = Router::new(node("b"));
        connect(&a, &b);
        a.disconnect_peer(&node("b").label());
        assert_eq!(a.detect_disconnects(), 0);
        assert_eq!(a.recv_down(), None);
    }

    #[test]
    fn down_only_for_the_disconnected_node() {
        let a = Router::new(node("a"));
        let b = Router::new(node("b"));
        let c = Router::new(node("c"));
        connect(&a, &b);
        connect(&a, &c);
        let on_b = DistPid::new(node("b"), 1);
        let on_c = DistPid::new(node("c"), 1);
        let w = DistPid::new(node("a"), 9);
        a.monitor(w.clone(), on_b.clone());
        a.monitor(w.clone(), on_c.clone());
        // Only B drops.
        a.disconnect_peer(&node("b").label());
        assert_eq!(a.detect_disconnects(), 1);
        let notice = a.recv_down().expect("one DOWN");
        assert_eq!(notice.monitored, on_b);
        assert_eq!(a.recv_down(), None);
        // C's monitor is untouched — still reachable.
        assert_eq!(a.send(&on_c, b"still-ok").is_ok(), true);
    }
}
