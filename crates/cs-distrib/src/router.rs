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

/// One node's message router.
#[derive(Debug)]
pub struct Router {
    node: NodeId,
    /// Peers keyed by `name@host` (epoch-independent) so a restarted peer
    /// replaces its predecessor and stale-epoch sends are caught.
    peers: Mutex<HashMap<String, Peer>>,
    /// Inbound messages destined for local actors on this node.
    inbox: Mutex<VecDeque<(DistPid, Vec<u8>)>>,
}

impl Router {
    pub fn new(node: NodeId) -> Self {
        Router {
            node,
            peers: Mutex::new(HashMap::new()),
            inbox: Mutex::new(VecDeque::new()),
        }
    }

    /// This router's node identity.
    pub fn node(&self) -> &NodeId {
        &self.node
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
        drop(peers);
        for msg in inbound {
            self.deliver_local(msg.0, msg.1);
        }
        Ok(delivered)
    }

    fn deliver_local(&self, pid: DistPid, payload: Vec<u8>) {
        self.inbox
            .lock()
            .expect("inbox poisoned")
            .push_back((pid, payload));
    }

    /// Pop the next inbound message for a local actor, if any. (Stands in
    /// for cs-actor mailbox delivery until that integration lands.)
    pub fn recv_local(&self) -> Option<(DistPid, Vec<u8>)> {
        self.inbox.lock().expect("inbox poisoned").pop_front()
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
}
