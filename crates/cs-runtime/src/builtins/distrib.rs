//! Cross-node cluster transport primops, exposed to Scheme. Behind the
//! `distrib` feature.
//!
//! These let a consensus engine written in Scheme (`lib/consensus/*.scm`)
//! exchange messages between *nodes*, not just between in-process actors:
//!
//! ```scheme
//!   (node-make NAME)          ; create a node (a cs-distrib Router) named NAME
//!   (node-link! A B)          ; connect nodes A and B with a sim transport
//!   (node-send FROM TO MSG)   ; route MSG from node FROM to node TO
//!   (node-poll NODE)          ; drain + decode the messages delivered to NODE
//! ```
//!
//! A message crosses as *data*: a Scheme value -> [`SendableValue`] -> a
//! compact self-describing byte frame ([`encode_sendable`]) carried by cs-net,
//! decoded back on arrival. cs-distrib's [`Router`] frames each as
//! `DistPid ‖ payload`; we run one replica per node (`local_id`
//! [`REPLICA_LOCAL_ID`]), so a node is addressed by name and a self-send loops
//! back through the router's own inbox (so even control messages flow over the
//! one uniform path).
//!
//! Why a process-global node registry (like `BeamState`)? A `Router` owns
//! transports and is not a Scheme value; keeping Routers in a name-keyed
//! registry means the builtins take string names and no Rust handle ever
//! crosses into Scheme — the same discipline that lets actors on different
//! threads share the cluster. The Router API is synchronous (`send` / `poll`
//! / `recv_local`), so no async runtime is needed at this boundary; the sim
//! transport is fully in-memory and deterministic, and tcp/quic implement the
//! same `Transport` trait for real sockets.

#![cfg(feature = "distrib")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::{Pair, SymbolTable, Value};

use cs_distrib::handshake::{evaluate_hello, HandshakeOutcome, Hello};
use cs_distrib::pid::DistPid;
use cs_distrib::router::Router;
use cs_distrib::NodeId;
use cs_net::sim::SimPair;
use cs_net::tcp::TcpTransport;
use cs_net::{Channel, Transport, TransportConfig};
use tokio::net::{TcpListener, TcpStream};

use super::beam::{beam_state, SendableValue};

// All single-process nodes share one host/epoch; a node is identified by name.
const NODE_HOST: &str = "local";
const NODE_EPOCH: u64 = 0;
// One replica actor per node, at a fixed local id, so node addressing is by
// name (the engine routes by node id; the local id is constant).
const REPLICA_LOCAL_ID: u64 = 1;

/// Process-wide registry of nodes (Routers) keyed by name. Lazily created.
pub struct DistribState {
    pub nodes: Mutex<HashMap<String, Arc<Router>>>,
}

static DISTRIB: OnceLock<DistribState> = OnceLock::new();

fn distrib_state() -> &'static DistribState {
    DISTRIB.get_or_init(|| DistribState {
        nodes: Mutex::new(HashMap::new()),
    })
}

fn node_id(name: &str) -> NodeId {
    NodeId::new(name, NODE_HOST, NODE_EPOCH)
}

fn lookup_router(name: &str, who: &str) -> Result<Arc<Router>, String> {
    distrib_state()
        .nodes
        .lock()
        .expect("nodes registry poisoned")
        .get(name)
        .cloned()
        .ok_or_else(|| format!("{who}: no node named {name:?} (call (node-make {name:?}) first)"))
}

//
// Rust-callable primops.
//

/// Create a node (Router) named `name`. Errors if one already exists.
pub fn primop_node_make(name: &str) -> Result<(), String> {
    let mut nodes = distrib_state()
        .nodes
        .lock()
        .expect("nodes registry poisoned");
    if nodes.contains_key(name) {
        return Err(format!("node-make: node {name:?} already exists"));
    }
    nodes.insert(name.to_string(), Arc::new(Router::new(node_id(name))));
    Ok(())
}

/// Connect nodes `a` and `b` with a bidirectional in-memory sim transport.
pub fn primop_node_link_sim(a: &str, b: &str) -> Result<(), String> {
    let ra = lookup_router(a, "node-link!")?;
    let rb = lookup_router(b, "node-link!")?;
    // SimPair("a","b").into_endpoints() -> (a-end, b-end); whatever a-end
    // sends, b-end receives. So a's router routes to b via the a-end.
    let (ea, eb) = SimPair::new(a, b).into_endpoints();
    ra.add_peer(node_id(b), Box::new(ea) as Box<dyn Transport>);
    rb.add_peer(node_id(a), Box::new(eb) as Box<dyn Transport>);
    Ok(())
}

/// Route `msg` from node `from` to node `to`. A self-send (`from == to`)
/// loops back through `from`'s own inbox.
pub fn primop_node_send(from: &str, to: &str, msg: &SendableValue) -> Result<(), String> {
    let router = lookup_router(from, "node-send")?;
    let target = DistPid::new(node_id(to), REPLICA_LOCAL_ID);
    let mut bytes = Vec::new();
    encode_sendable(msg, &mut bytes)?;
    router
        .send(&target, &bytes)
        .map_err(|e| format!("node-send: {from} -> {to}: {e}"))
}

/// Pump `node`'s transports and return every message now delivered to it,
/// decoded back into [`SendableValue`]s (in delivery order).
pub fn primop_node_poll(node: &str) -> Result<Vec<SendableValue>, String> {
    let router = lookup_router(node, "node-poll")?;
    router.poll().map_err(|e| format!("node-poll: {e}"))?;
    let mut out = Vec::new();
    while let Some((_target, payload)) = router.recv_local() {
        let (sv, _consumed) = decode_sendable(&payload)?;
        out.push(sv);
    }
    Ok(out)
}

/// cw-gx4: like `primop_node_send` but routes over an explicit cs-net channel
/// `ch` (one Raft group → one channel), so groups don't serialize on Messages.
pub fn primop_node_send_ch(
    from: &str,
    to: &str,
    ch: u8,
    msg: &SendableValue,
) -> Result<(), String> {
    let router = lookup_router(from, "node-send-ch")?;
    let target = DistPid::new(node_id(to), REPLICA_LOCAL_ID);
    let mut bytes = Vec::new();
    encode_sendable(msg, &mut bytes)?;
    router
        .send_ch(&target, &bytes, ch)
        .map_err(|e| format!("node-send-ch: {from} -> {to} ch{ch}: {e}"))
}

/// cw-sei: send a `Value` directly (no intermediate `SendableValue` tree) over
/// channel `ch`. Fuses projection + byte-encode into one read-only walk.
pub fn primop_node_send_ch_value(
    from: &str,
    to: &str,
    ch: u8,
    v: &Value,
    syms: &SymbolTable,
) -> Result<(), String> {
    let router = lookup_router(from, "node-send-ch")?;
    let target = DistPid::new(node_id(to), REPLICA_LOCAL_ID);
    let mut bytes = Vec::new();
    encode_value_in(v, syms, &mut bytes)?;
    router
        .send_ch(&target, &bytes, ch)
        .map_err(|e| format!("node-send-ch: {from} -> {to} ch{ch}: {e}"))
}

/// cw-sei: `primop_node_send` direct-from-`Value` variant.
pub fn primop_node_send_value(
    from: &str,
    to: &str,
    v: &Value,
    syms: &SymbolTable,
) -> Result<(), String> {
    let router = lookup_router(from, "node-send")?;
    let target = DistPid::new(node_id(to), REPLICA_LOCAL_ID);
    let mut bytes = Vec::new();
    encode_value_in(v, syms, &mut bytes)?;
    router
        .send(&target, &bytes)
        .map_err(|e| format!("node-send: {from} -> {to}: {e}"))
}

/// cw-gx4: pump `node`'s transports and return only the messages delivered on
/// shard channel `ch`. A per-group poller calls this with its own channel so
/// independent groups drain in parallel.
pub fn primop_node_poll_ch(node: &str, ch: u8) -> Result<Vec<SendableValue>, String> {
    let router = lookup_router(node, "node-poll-ch")?;
    router
        .poll_channel(ch)
        .map_err(|e| format!("node-poll-ch: {e}"))?;
    let mut out = Vec::new();
    while let Some((_target, payload)) = router.recv_local_channel(ch) {
        let (sv, _consumed) = decode_sendable(&payload)?;
        out.push(sv);
    }
    Ok(out)
}

/// Number of peers currently registered on `node`. A cluster bootstrap waits
/// on this because TCP peers are added asynchronously on the accepting side.
pub fn primop_node_peer_count(node: &str) -> Result<usize, String> {
    Ok(lookup_router(node, "node-peer-count")?.peer_count())
}

/// `(node-peers NODE)` -> list of connected peer-name strings.
pub fn b_node_peers(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("node-peers: expected (node-peers NODE)".into());
    }
    let node = name_of(&args[0], syms, "node-peers")?;
    let labels = lookup_router(&node, "node-peers")?.peer_labels();
    Ok(Value::list(labels.into_iter().map(Value::string)))
}

/// Prune peers whose transport has closed (a crashed or departed node) and
/// return how many were dropped (also fires any pending DOWN monitors). Call
/// periodically so `node-peer-count` reflects reality — `node-poll` does not
/// prune on its own — which lets reconnection logic notice a gap and re-dial a
/// peer that has come back.
pub fn primop_node_detect_disconnects(node: &str) -> Result<usize, String> {
    Ok(lookup_router(node, "node-detect-disconnects")?.detect_disconnects())
}

//
// TCP transport — real cross-process / cross-machine sockets (plaintext or
// mutual-TLS). node-link! is the in-memory sim transport; these connect nodes
// over actual TCP. Socket I/O runs on cs-actor's tokio runtime (the one the
// cluster already uses) via ActorSystem::runtime_handle — no second runtime.
// Once a connection is up the routing/serialization is identical to the sim
// path, so the consensus engine and the (node-send/node-poll) builtins are
// unchanged.
//

/// Plaintext TCP vs. mutual-TLS (and below, QUIC, which is always mTLS).
#[derive(Clone, Copy)]
enum Security {
    Plain,
    Mtls,
}

/// Exchange `Hello`s over an established transport's `Control` channel,
/// returning the accepted peer's `NodeId`. Works for ANY transport — sim, TCP,
/// mTLS-over-TCP, QUIC — because it only uses the `Transport` trait, so the
/// node-identity handshake is independent of how the bytes are carried (and of
/// any TLS handshake the transport already did). Both sides send then poll, so
/// it is symmetric for connector and acceptor.
async fn handshake_over_transport(t: &dyn Transport, local: &NodeId) -> Result<NodeId, String> {
    t.send(Channel::Control, &Hello::new(local.clone(), 0).encode_vec())
        .map_err(|e| format!("handshake send: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match t
            .try_recv(Channel::Control)
            .map_err(|e| format!("handshake recv: {e}"))?
        {
            Some(bytes) => {
                let peer = Hello::decode(&bytes).map_err(|e| format!("handshake decode: {e}"))?;
                return match evaluate_hello(local, &peer, 0, None) {
                    HandshakeOutcome::Accepted { peer, .. } => Ok(peer),
                    HandshakeOutcome::Quarantine { reason } => {
                        Err(format!("handshake quarantine: {reason}"))
                    }
                };
            }
            None => {
                if std::time::Instant::now() > deadline {
                    return Err("handshake timed out waiting for peer Hello".into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        }
    }
}

/// Wrap an accepted TCP stream per the security mode into a boxed transport.
/// `name` is the local node's name (its mTLS server cert is per-node).
async fn wrap_accepted(
    stream: TcpStream,
    sec: Security,
    cfg: &TransportConfig,
    name: &str,
) -> Result<Box<dyn Transport>, String> {
    let _ = stream.set_nodelay(true);
    match sec {
        Security::Plain => Ok(Box::new(TcpTransport::from_stream(stream, "peer", cfg))),
        Security::Mtls => {
            let sc =
                cs_net::tls::dev::server_config(name).map_err(|e| format!("mtls config: {e}"))?;
            let t = TcpTransport::accept_tls(stream, "peer", cfg, sc)
                .await
                .map_err(|e| format!("mtls accept: {e}"))?;
            Ok(Box::new(t))
        }
    }
}

/// Bind `node` to a TCP `addr` and accept inbound connections forever (on the
/// cluster runtime), running the identity handshake + registering each as a
/// peer. Returns the actual bound address (so `(node-listen NODE "127.0.0.1:0")`
/// can publish the chosen port).
fn listen_impl(node: &str, addr: &str, sec: Security) -> Result<String, String> {
    let router = lookup_router(node, "node-listen")?;
    let local = node_id(node);
    let handle = beam_state().actors.runtime_handle();

    let addr_owned = addr.to_string();
    let listener = handle
        .block_on(async move { TcpListener::bind(&addr_owned).await })
        .map_err(|e| format!("node-listen: bind {addr}: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("node-listen: local_addr: {e}"))?
        .to_string();

    let cfg = TransportConfig::default();
    handle.spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => match wrap_accepted(stream, sec, &cfg, &local.name).await {
                    Ok(t) => match handshake_over_transport(t.as_ref(), &local).await {
                        Ok(peer) => router.add_peer(peer, t),
                        Err(e) => eprintln!("node-listen {}: {e}", local.label()),
                    },
                    Err(e) => eprintln!("node-listen {}: {e}", local.label()),
                },
                Err(e) => {
                    eprintln!("node-listen {}: accept: {e}", local.label());
                    break;
                }
            }
        }
    });
    Ok(bound)
}

/// Connect `node` to a peer listening at `peer_addr`, handshake, register the
/// peer. Synchronous from the caller's view. Call from the cluster bootstrap
/// (main) thread, not an actor body.
fn connect_impl(node: &str, peer_addr: &str, sec: Security) -> Result<(), String> {
    let router = lookup_router(node, "node-connect")?;
    let local = node_id(node);
    let handle = beam_state().actors.runtime_handle();
    let cfg = TransportConfig::default();
    let peer_addr_owned = peer_addr.to_string();

    handle.block_on(async move {
        let t: Box<dyn Transport> = match sec {
            Security::Plain => {
                let stream = TcpStream::connect(&peer_addr_owned)
                    .await
                    .map_err(|e| format!("node-connect: connect {peer_addr_owned}: {e}"))?;
                let _ = stream.set_nodelay(true);
                Box::new(TcpTransport::from_stream(stream, "peer", &cfg))
            }
            Security::Mtls => {
                let cc = cs_net::tls::dev::client_config(&local.name)
                    .map_err(|e| format!("mtls config: {e}"))?;
                // server_name "localhost" matches the dev cert SAN.
                let t = TcpTransport::connect_tls(&peer_addr_owned, "localhost", "peer", &cfg, cc)
                    .await
                    .map_err(|e| format!("mtls connect {peer_addr_owned}: {e}"))?;
                Box::new(t)
            }
        };
        let peer = handshake_over_transport(t.as_ref(), &local).await?;
        router.add_peer(peer, t);
        Ok::<(), String>(())
    })
}

/// `(node-listen NODE ADDR)` — plaintext TCP.
pub fn primop_node_listen(node: &str, addr: &str) -> Result<String, String> {
    listen_impl(node, addr, Security::Plain)
}

/// `(node-connect NODE PEER-ADDR)` — plaintext TCP.
pub fn primop_node_connect(node: &str, peer_addr: &str) -> Result<(), String> {
    connect_impl(node, peer_addr, Security::Plain)
}

/// `(node-listen-tls NODE ADDR)` — TCP with mutual TLS (dev identity).
pub fn primop_node_listen_tls(node: &str, addr: &str) -> Result<String, String> {
    listen_impl(node, addr, Security::Mtls)
}

/// `(node-connect-tls NODE PEER-ADDR)` — TCP with mutual TLS (dev identity).
pub fn primop_node_connect_tls(node: &str, peer_addr: &str) -> Result<(), String> {
    connect_impl(node, peer_addr, Security::Mtls)
}

//
// QUIC transport — always mTLS (TLS 1.3), one stream per logical channel (so a
// Bulk transfer can't head-of-line-block Control). Same identity handshake +
// routing as the others; only the byte pipe differs. quinn lives entirely
// behind cs-net::quic_dev, so cs-runtime never names a quinn type.
//

/// `(node-listen-quic NODE ADDR)` — listen for QUIC peers; returns the addr.
pub fn primop_node_listen_quic(node: &str, addr: &str) -> Result<String, String> {
    let router = lookup_router(node, "node-listen-quic")?;
    let local = node_id(node);
    let handle = beam_state().actors.runtime_handle();
    let bind: SocketAddr = addr
        .parse()
        .map_err(|e| format!("node-listen-quic: bad addr {addr}: {e}"))?;

    // Endpoint::server spawns a driver task, so build it inside the runtime.
    let listener = {
        let _g = handle.enter();
        cs_net::quic_dev::listen(bind, &local.name).map_err(|e| format!("node-listen-quic: {e}"))?
    };
    let bound = listener
        .local_addr()
        .map_err(|e| format!("node-listen-quic: {e}"))?;

    let cfg = TransportConfig::default();
    handle.spawn(async move {
        loop {
            match listener.accept(&cfg).await {
                Ok(t) => {
                    let t: Box<dyn Transport> = Box::new(t);
                    match handshake_over_transport(t.as_ref(), &local).await {
                        Ok(peer) => router.add_peer(peer, t),
                        Err(e) => eprintln!("node-listen-quic {}: {e}", local.label()),
                    }
                }
                Err(e) => {
                    eprintln!("node-listen-quic {}: accept: {e}", local.label());
                    break;
                }
            }
        }
    });
    Ok(bound)
}

/// `(node-connect-quic NODE PEER-ADDR)` — connect NODE to a QUIC peer (mTLS).
pub fn primop_node_connect_quic(node: &str, peer_addr: &str) -> Result<(), String> {
    let router = lookup_router(node, "node-connect-quic")?;
    let local = node_id(node);
    let handle = beam_state().actors.runtime_handle();
    let cfg = TransportConfig::default();
    let bind: SocketAddr = peer_addr
        .parse()
        .map_err(|e| format!("node-connect-quic: bad addr {peer_addr}: {e}"))?;

    handle.block_on(async move {
        // server_name "localhost" matches the dev cert SAN.
        let t = cs_net::quic_dev::connect(bind, "localhost", "peer", &cfg, &local.name)
            .await
            .map_err(|e| format!("node-connect-quic {peer_addr}: {e}"))?;
        let t: Box<dyn Transport> = Box::new(t);
        let peer = handshake_over_transport(t.as_ref(), &local).await?;
        router.add_peer(peer, t);
        Ok::<(), String>(())
    })
}

//
// Scheme-builtin wrappers (Value <-> SendableValue at the boundary).
//

fn name_of(v: &Value, syms: &SymbolTable, who: &str) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(syms.name(*s).to_string()),
        Value::String(s) => Ok(s.borrow().clone()),
        other => Err(format!(
            "{who}: expected a node name (symbol or string), got {}",
            other.type_name()
        )),
    }
}

/// `(node-make NAME)`
pub fn b_node_make(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("node-make: expected (node-make NAME)".into());
    }
    primop_node_make(&name_of(&args[0], syms, "node-make")?)?;
    Ok(Value::Unspecified)
}

/// `(node-link! A B)` — bidirectional sim transport between two nodes.
pub fn b_node_link(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-link!: expected (node-link! A B)".into());
    }
    let a = name_of(&args[0], syms, "node-link!")?;
    let b = name_of(&args[1], syms, "node-link!")?;
    primop_node_link_sim(&a, &b)?;
    Ok(Value::Unspecified)
}

/// `(node-send FROM TO MSG)`
pub fn b_node_send(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err("node-send: expected (node-send FROM TO MSG)".into());
    }
    let from = name_of(&args[0], syms, "node-send")?;
    let to = name_of(&args[1], syms, "node-send")?;
    // cw-sei: encode straight from the Value, skipping the SendableValue tree.
    primop_node_send_value(&from, &to, &args[2], syms)?;
    Ok(Value::Unspecified)
}

fn chan_of(v: &Value, who: &str) -> Result<u8, String> {
    match v {
        Value::Number(cs_core::Number::Fixnum(n)) if (0..=5).contains(n) => Ok(*n as u8),
        _ => Err(format!("{who}: channel must be an integer 0..5")),
    }
}

/// `(node-send-ch FROM TO CH MSG)` — cw-gx4: send over cs-net channel CH.
pub fn b_node_send_ch(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 {
        return Err("node-send-ch: expected (node-send-ch FROM TO CH MSG)".into());
    }
    let from = name_of(&args[0], syms, "node-send-ch")?;
    let to = name_of(&args[1], syms, "node-send-ch")?;
    let ch = chan_of(&args[2], "node-send-ch")?;
    // cw-sei: encode straight from the Value, skipping the SendableValue tree.
    primop_node_send_ch_value(&from, &to, ch, &args[3], syms)?;
    Ok(Value::Unspecified)
}

/// `(node-poll-ch NODE CH)` — cw-gx4: drain only channel CH's inbox.
pub fn b_node_poll_ch(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-poll-ch: expected (node-poll-ch NODE CH)".into());
    }
    let node = name_of(&args[0], syms, "node-poll-ch")?;
    let ch = chan_of(&args[1], "node-poll-ch")?;
    // cw-sei: decode straight into Values, skipping the SendableValue tree.
    let msgs = primop_node_poll_ch_value(&node, ch, syms)?;
    let mut list = Value::Null;
    for v in msgs.into_iter().rev() {
        list = Value::Pair(Pair::new(v, list));
    }
    Ok(list)
}

/// `(node-poll-ch-wait NODE CH TIMEOUT-MS)` — like node-poll-ch, but blocks up
/// to TIMEOUT-MS for inbound traffic when the channel is empty. Dedicated-thread
/// actors only.
pub fn b_node_poll_ch_wait(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err("node-poll-ch-wait: expected (node-poll-ch-wait NODE CH TIMEOUT-MS)".into());
    }
    let node = name_of(&args[0], syms, "node-poll-ch-wait")?;
    let ch = chan_of(&args[1], "node-poll-ch-wait")?;
    let timeout_ms = match &args[2] {
        Value::Number(cs_core::Number::Fixnum(n)) if *n >= 0 => *n as u64,
        _ => return Err("node-poll-ch-wait: TIMEOUT-MS must be a non-negative integer".into()),
    };
    let msgs = primop_node_poll_ch_wait_value(&node, ch, timeout_ms, syms)?;
    let mut list = Value::Null;
    for v in msgs.into_iter().rev() {
        list = Value::Pair(Pair::new(v, list));
    }
    Ok(list)
}

/// `(node-poll NODE)` — returns a list of the messages delivered to NODE.
pub fn b_node_poll(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("node-poll: expected (node-poll NODE)".into());
    }
    let node = name_of(&args[0], syms, "node-poll")?;
    // cw-sei: decode straight into Values, skipping the SendableValue tree.
    let msgs = primop_node_poll_value(&node, syms)?;
    let mut list = Value::Null;
    for v in msgs.into_iter().rev() {
        list = Value::Pair(Pair::new(v, list));
    }
    Ok(list)
}

/// `(node-listen NODE ADDR)` — listen for TCP peers; returns the bound addr.
/// Use `"127.0.0.1:0"` to bind an ephemeral port and read back the choice.
pub fn b_node_listen(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-listen: expected (node-listen NODE ADDR)".into());
    }
    let node = name_of(&args[0], syms, "node-listen")?;
    let addr = name_of(&args[1], syms, "node-listen")?;
    let bound = primop_node_listen(&node, &addr)?;
    Ok(Value::String(cs_core::Gc::new(std::cell::RefCell::new(
        bound,
    ))))
}

/// `(node-connect NODE PEER-ADDR)` — connect NODE to a peer over TCP.
pub fn b_node_connect(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-connect: expected (node-connect NODE PEER-ADDR)".into());
    }
    let node = name_of(&args[0], syms, "node-connect")?;
    let addr = name_of(&args[1], syms, "node-connect")?;
    primop_node_connect(&node, &addr)?;
    Ok(Value::Unspecified)
}

/// `(node-listen-tls NODE ADDR)` — listen for mTLS TCP peers; returns the addr.
pub fn b_node_listen_tls(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-listen-tls: expected (node-listen-tls NODE ADDR)".into());
    }
    let node = name_of(&args[0], syms, "node-listen-tls")?;
    let addr = name_of(&args[1], syms, "node-listen-tls")?;
    let bound = primop_node_listen_tls(&node, &addr)?;
    Ok(Value::String(cs_core::Gc::new(std::cell::RefCell::new(
        bound,
    ))))
}

/// `(node-connect-tls NODE PEER-ADDR)` — connect NODE to a peer with mutual TLS.
pub fn b_node_connect_tls(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-connect-tls: expected (node-connect-tls NODE PEER-ADDR)".into());
    }
    let node = name_of(&args[0], syms, "node-connect-tls")?;
    let addr = name_of(&args[1], syms, "node-connect-tls")?;
    primop_node_connect_tls(&node, &addr)?;
    Ok(Value::Unspecified)
}

/// `(node-listen-quic NODE ADDR)` — listen for QUIC peers; returns the addr.
pub fn b_node_listen_quic(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-listen-quic: expected (node-listen-quic NODE ADDR)".into());
    }
    let node = name_of(&args[0], syms, "node-listen-quic")?;
    let addr = name_of(&args[1], syms, "node-listen-quic")?;
    let bound = primop_node_listen_quic(&node, &addr)?;
    Ok(Value::String(cs_core::Gc::new(std::cell::RefCell::new(
        bound,
    ))))
}

/// `(node-connect-quic NODE PEER-ADDR)` — connect NODE to a QUIC peer (mTLS).
pub fn b_node_connect_quic(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("node-connect-quic: expected (node-connect-quic NODE PEER-ADDR)".into());
    }
    let node = name_of(&args[0], syms, "node-connect-quic")?;
    let addr = name_of(&args[1], syms, "node-connect-quic")?;
    primop_node_connect_quic(&node, &addr)?;
    Ok(Value::Unspecified)
}

/// `(node-peer-count NODE)` — how many peers NODE has registered.
pub fn b_node_peer_count(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("node-peer-count: expected (node-peer-count NODE)".into());
    }
    let node = name_of(&args[0], syms, "node-peer-count")?;
    Ok(Value::Number(cs_core::Number::Fixnum(
        primop_node_peer_count(&node)? as i64,
    )))
}

/// `(node-detect-disconnects NODE)` — drop peers whose link has closed and
/// return how many were dropped. Call periodically (e.g. from a poll loop) so
/// `node-peer-count` drops when a peer dies and a reconnector can re-dial.
pub fn b_node_detect_disconnects(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("node-detect-disconnects: expected (node-detect-disconnects NODE)".into());
    }
    let node = name_of(&args[0], syms, "node-detect-disconnects")?;
    Ok(Value::Number(cs_core::Number::Fixnum(
        primop_node_detect_disconnects(&node)? as i64,
    )))
}

/// The Scheme-facing distrib builtins, in the `(name, fn)` shape the
/// registration loops accept. Merged into cs-runtime's walker + VM env when
/// the `distrib` feature is on.
pub fn distrib_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    vec![
        ("node-make", b_node_make),
        ("node-link!", b_node_link),
        ("node-listen", b_node_listen),
        ("node-connect", b_node_connect),
        ("node-listen-tls", b_node_listen_tls),
        ("node-connect-tls", b_node_connect_tls),
        ("node-listen-quic", b_node_listen_quic),
        ("node-connect-quic", b_node_connect_quic),
        ("node-peer-count", b_node_peer_count),
        ("node-peers", b_node_peers),
        ("node-detect-disconnects", b_node_detect_disconnects),
        ("node-send", b_node_send),
        ("node-poll", b_node_poll),
        ("node-send-ch", b_node_send_ch),
        ("node-poll-ch", b_node_poll_ch),
        ("node-poll-ch-wait", b_node_poll_ch_wait),
    ]
}

//
// Self-describing byte codec for SendableValue. A tag byte then the payload;
// big-endian, length-prefixed strings/bytes. Mirrors cs-distrib's DistPid
// codec style. Keeps the wire format independent of the in-process Value
// representation so it survives a real (tcp/quic) hop unchanged.
//

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

/// Encode a [`SendableValue`] onto `out`. Errors on a PID (PIDs are
/// node-local handles; address peers by node name instead).
pub fn encode_sendable(v: &SendableValue, out: &mut Vec<u8>) -> Result<(), String> {
    match v {
        SendableValue::Null => out.push(0),
        SendableValue::Unspecified => out.push(1),
        SendableValue::Eof => out.push(2),
        SendableValue::Boolean(b) => {
            out.push(3);
            out.push(u8::from(*b));
        }
        SendableValue::Character(c) => {
            out.push(4);
            out.extend_from_slice(&(*c as u32).to_be_bytes());
        }
        SendableValue::Fixnum(n) => {
            out.push(5);
            out.extend_from_slice(&n.to_be_bytes());
        }
        SendableValue::Flonum(f) => {
            out.push(6);
            out.extend_from_slice(&f.to_bits().to_be_bytes());
        }
        SendableValue::BigInt(s) => {
            out.push(7);
            put_bytes(out, s.as_bytes());
        }
        SendableValue::String(s) => {
            out.push(8);
            put_bytes(out, s.as_bytes());
        }
        SendableValue::Symbol(s) => {
            out.push(9);
            put_bytes(out, s.as_bytes());
        }
        SendableValue::Pair(a, b) => {
            out.push(10);
            encode_sendable(a, out)?;
            encode_sendable(b, out)?;
        }
        SendableValue::Vector(items) => {
            out.push(11);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            for it in items {
                encode_sendable(it, out)?;
            }
        }
        SendableValue::ByteVector(bytes) => {
            out.push(12);
            put_bytes(out, bytes);
        }
        SendableValue::Pid(_) => {
            return Err(
                "node-send: a PID cannot cross nodes; address the peer by node name".into(),
            );
        }
    }
    Ok(())
}

/// cw-sei: encode a `Value` DIRECTLY to the same wire bytes as
/// `encode_sendable(&to_sendable_in(v))`, WITHOUT allocating the intermediate
/// `SendableValue` tree. The send path (node-send/-ch) is a hot Raft path; the
/// profile showed `to_sendable_in` (18k samples) + its `drop_in_place` churn is
/// pure overhead — this fuses the projection and the byte-encode into one
/// read-only walk of the `Value`. Tag bytes MUST match `encode_sendable`.
pub fn encode_value_in(v: &Value, syms: &SymbolTable, out: &mut Vec<u8>) -> Result<(), String> {
    match v {
        Value::Null => out.push(0),
        Value::Unspecified => out.push(1),
        Value::Eof => out.push(2),
        Value::Boolean(b) => {
            out.push(3);
            out.push(u8::from(*b));
        }
        Value::Character(c) => {
            out.push(4);
            out.extend_from_slice(&(*c as u32).to_be_bytes());
        }
        Value::Number(n) => match n {
            cs_core::Number::Fixnum(i) => {
                out.push(5);
                out.extend_from_slice(&i.to_be_bytes());
            }
            cs_core::Number::Flonum(f) => {
                out.push(6);
                out.extend_from_slice(&f.to_bits().to_be_bytes());
            }
            cs_core::Number::Big(b) => {
                out.push(7);
                put_bytes(out, b.to_str_radix(10).as_bytes());
            }
            cs_core::Number::Rat(_) => {
                return Err("node-send: rationals not yet supported across actors".into());
            }
        },
        Value::String(s) => {
            out.push(8);
            put_bytes(out, s.borrow().as_bytes());
        }
        Value::Symbol(s) => {
            out.push(9);
            put_bytes(out, syms.name(*s).as_bytes());
        }
        Value::Identifier { name, .. } => {
            out.push(9);
            put_bytes(out, syms.name(*name).as_bytes());
        }
        Value::Pair(p) => {
            out.push(10);
            encode_value_in(&p.car.borrow(), syms, out)?;
            encode_value_in(&p.cdr.borrow(), syms, out)?;
        }
        Value::Vector(items) => {
            out.push(11);
            let items = items.borrow();
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            for it in items.iter() {
                encode_value_in(it, syms, out)?;
            }
        }
        Value::ByteVector(bytes) => {
            out.push(12);
            put_bytes(out, &bytes.borrow());
        }
        Value::Procedure(_) => {
            return Err("node-send: procedures cannot cross actor boundaries".into());
        }
        Value::Hashtable(_) => {
            return Err(
                "node-send: hashtables are per-actor; use cs-table for shared state".into(),
            );
        }
        Value::Port(_) => return Err("node-send: ports cannot cross actor boundaries".into()),
        Value::Promise(_) => return Err("node-send: promises cannot cross actor boundaries".into()),
    }
    Ok(())
}

/// Decode one [`SendableValue`] from the front of `bytes`, returning it and
/// the number of bytes consumed.
pub fn decode_sendable(bytes: &[u8]) -> Result<(SendableValue, usize), String> {
    let mut c = Dec { b: bytes, pos: 0 };
    let v = dec(&mut c)?;
    Ok((v, c.pos))
}

struct Dec<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Dec<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| format!("decode: truncated (need {n} more bytes)"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    fn len_bytes(&mut self) -> Result<Vec<u8>, String> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> Result<String, String> {
        String::from_utf8(self.len_bytes()?).map_err(|e| format!("decode: non-utf8 string: {e}"))
    }
}

fn dec(c: &mut Dec) -> Result<SendableValue, String> {
    match c.u8()? {
        0 => Ok(SendableValue::Null),
        1 => Ok(SendableValue::Unspecified),
        2 => Ok(SendableValue::Eof),
        3 => Ok(SendableValue::Boolean(c.u8()? != 0)),
        4 => {
            let n = c.u32()?;
            Ok(SendableValue::Character(
                char::from_u32(n).ok_or_else(|| format!("decode: bad char {n}"))?,
            ))
        }
        5 => Ok(SendableValue::Fixnum(c.i64()?)),
        6 => Ok(SendableValue::Flonum(f64::from_bits(c.u64()?))),
        7 => Ok(SendableValue::BigInt(c.string()?)),
        8 => Ok(SendableValue::String(c.string()?)),
        9 => Ok(SendableValue::Symbol(c.string()?)),
        10 => {
            let a = dec(c)?;
            let b = dec(c)?;
            Ok(SendableValue::Pair(Box::new(a), Box::new(b)))
        }
        11 => {
            let n = c.u32()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(dec(c)?);
            }
            Ok(SendableValue::Vector(v))
        }
        12 => Ok(SendableValue::ByteVector(c.len_bytes()?)),
        other => Err(format!("decode: unknown tag {other}")),
    }
}

/// cw-sei: decode one wire value DIRECTLY into a `Value`, skipping the
/// intermediate `SendableValue` tree (the receive-side counterpart of
/// [`encode_value_in`]). Mirrors `dec` + `from_sendable` fused: the profile's
/// 52k `from_sendable` samples + tree-drop churn collapse into one walk that
/// builds the destination `Value` as it reads bytes. Tags MUST match `dec`.
fn decode_value(c: &mut Dec, syms: &mut SymbolTable) -> Result<Value, String> {
    use std::cell::RefCell;
    match c.u8()? {
        0 => Ok(Value::Null),
        1 => Ok(Value::Unspecified),
        2 => Ok(Value::Eof),
        3 => Ok(Value::Boolean(c.u8()? != 0)),
        4 => {
            let n = c.u32()?;
            Ok(Value::Character(
                char::from_u32(n).ok_or_else(|| format!("decode: bad char {n}"))?,
            ))
        }
        5 => Ok(Value::Number(cs_core::Number::Fixnum(c.i64()?))),
        6 => Ok(Value::Number(cs_core::Number::from_f64(f64::from_bits(
            c.u64()?,
        )))),
        7 => {
            let s = c.string()?;
            Ok(Value::Number(
                cs_core::Number::parse_decimal_integer(&s).expect("bigint round-trip"),
            ))
        }
        8 => Ok(Value::String(cs_core::Gc::new(RefCell::new(c.string()?)))),
        9 => Ok(Value::Symbol(syms.intern(&c.string()?))),
        10 => {
            let a = decode_value(c, syms)?;
            let b = decode_value(c, syms)?;
            Ok(Value::Pair(Pair::new(a, b)))
        }
        11 => {
            let n = c.u32()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(decode_value(c, syms)?);
            }
            Ok(Value::Vector(cs_core::Gc::new(RefCell::new(v))))
        }
        12 => Ok(Value::ByteVector(cs_core::Gc::new(RefCell::new(
            c.len_bytes()?,
        )))),
        other => Err(format!("decode: unknown tag {other}")),
    }
}

/// cw-sei: `primop_node_poll` returning `Value`s built directly from the wire
/// (no `SendableValue` round-trip).
pub fn primop_node_poll_value(node: &str, syms: &mut SymbolTable) -> Result<Vec<Value>, String> {
    let router = lookup_router(node, "node-poll")?;
    router.poll().map_err(|e| format!("node-poll: {e}"))?;
    let mut out = Vec::new();
    while let Some((_target, payload)) = router.recv_local() {
        let mut c = Dec {
            b: &payload,
            pos: 0,
        };
        out.push(decode_value(&mut c, syms)?);
    }
    Ok(out)
}

/// cw-sei: per-channel direct-to-`Value` poll (receive-side counterpart of
/// `primop_node_send_ch_value`).
pub fn primop_node_poll_ch_value(
    node: &str,
    ch: u8,
    syms: &mut SymbolTable,
) -> Result<Vec<Value>, String> {
    let router = lookup_router(node, "node-poll-ch")?;
    router
        .poll_channel(ch)
        .map_err(|e| format!("node-poll-ch: {e}"))?;
    let mut out = Vec::new();
    while let Some((_target, payload)) = router.recv_local_channel(ch) {
        let mut c = Dec {
            b: &payload,
            pos: 0,
        };
        out.push(decode_value(&mut c, syms)?);
    }
    Ok(out)
}

/// Blocking variant of [`primop_node_poll_ch_value`]: when channel `ch` has
/// nothing queued, wait up to `timeout_ms` for ANY inbound frame instead of
/// returning empty — so a poll loop needs no sleep and mesh hop latency is
/// delivery latency, not polling granularity (crab-watchstore cw-xq9).
/// BLOCKS the calling thread: use only from dedicated-thread actors
/// (`spawn-source-dedicated`), never a shared LocalSet worker.
pub fn primop_node_poll_ch_wait_value(
    node: &str,
    ch: u8,
    timeout_ms: u64,
    syms: &mut SymbolTable,
) -> Result<Vec<Value>, String> {
    let router = lookup_router(node, "node-poll-ch-wait")?;
    router
        .poll_channel_wait(ch, timeout_ms)
        .map_err(|e| format!("node-poll-ch-wait: {e}"))?;
    let mut out = Vec::new();
    while let Some((_target, payload)) = router.recv_local_channel(ch) {
        let mut c = Dec {
            b: &payload,
            pos: 0,
        };
        out.push(decode_value(&mut c, syms)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(v: &SendableValue) -> SendableValue {
        let mut bytes = Vec::new();
        encode_sendable(v, &mut bytes).expect("encode");
        let (back, consumed) = decode_sendable(&bytes).expect("decode");
        assert_eq!(consumed, bytes.len(), "decode must consume the whole frame");
        back
    }

    #[test]
    fn codec_round_trips_every_data_variant() {
        let cases = vec![
            SendableValue::Null,
            SendableValue::Unspecified,
            SendableValue::Eof,
            SendableValue::Boolean(true),
            SendableValue::Boolean(false),
            SendableValue::Character('λ'),
            SendableValue::Fixnum(-9_000_000_000),
            SendableValue::Flonum(3.5),
            SendableValue::BigInt("123456789012345678901234567890".into()),
            SendableValue::String("hi \"there\"".into()),
            SendableValue::Symbol("set".into()),
            SendableValue::ByteVector(vec![1, 2, 3, 255]),
            // (engine a (rv 1 a 0 0)) — a realistic Raft wire message
            SendableValue::Pair(
                Box::new(SendableValue::Symbol("engine".into())),
                Box::new(SendableValue::Pair(
                    Box::new(SendableValue::Symbol("a".into())),
                    Box::new(SendableValue::Pair(
                        Box::new(SendableValue::Vector(vec![
                            SendableValue::Symbol("rv".into()),
                            SendableValue::Fixnum(1),
                        ])),
                        Box::new(SendableValue::Null),
                    )),
                )),
            ),
        ];
        for c in &cases {
            assert_eq!(&round_trip(c), c);
        }
    }

    #[test]
    fn codec_rejects_pid() {
        let mut out = Vec::new();
        let pid = cs_actor::ActorPid {
            node: 0,
            local_id: 1,
        };
        assert!(encode_sendable(&SendableValue::Pid(pid), &mut out).is_err());
    }

    #[test]
    fn truncated_frame_errors_not_panics() {
        let mut bytes = Vec::new();
        encode_sendable(&SendableValue::String("abc".into()), &mut bytes).unwrap();
        assert!(decode_sendable(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode_sendable(&[]).is_err());
    }

    #[test]
    fn two_nodes_send_and_poll_over_sim_transport() {
        primop_node_make("codec-test-a").expect("make a");
        primop_node_make("codec-test-b").expect("make b");
        primop_node_link_sim("codec-test-a", "codec-test-b").expect("link");

        // a -> b
        let msg = SendableValue::Pair(
            Box::new(SendableValue::Symbol("ping".into())),
            Box::new(SendableValue::Fixnum(7)),
        );
        primop_node_send("codec-test-a", "codec-test-b", &msg).expect("send");

        // b receives exactly that, decoded
        let got = primop_node_poll("codec-test-b").expect("poll");
        assert_eq!(got, vec![msg]);
        // nothing left
        assert!(primop_node_poll("codec-test-b").expect("poll2").is_empty());
    }

    #[test]
    fn self_send_loops_back() {
        primop_node_make("loopback-node").expect("make");
        primop_node_send(
            "loopback-node",
            "loopback-node",
            &SendableValue::Symbol("campaign".into()),
        )
        .expect("self-send");
        let got = primop_node_poll("loopback-node").expect("poll");
        assert_eq!(got, vec![SendableValue::Symbol("campaign".into())]);
    }

    #[test]
    fn two_nodes_send_and_poll_over_real_tcp() {
        // The real-socket path: two nodes connected over loopback TCP, a
        // message serialized + framed + routed across the socket, decoded on
        // the far side. (Proves the cross-node path is genuinely over cs-net's
        // TCP transport, not only the in-memory sim.)
        primop_node_make("tcp-a").expect("make a");
        primop_node_make("tcp-b").expect("make b");

        // a listens on an ephemeral loopback port; b connects to it. One TCP
        // connection is full-duplex, so the single handshake registers the
        // peer on both routers.
        let addr = primop_node_listen("tcp-a", "127.0.0.1:0").expect("listen");
        primop_node_connect("tcp-b", &addr).expect("connect");

        // b -> a over the wire.
        let msg = SendableValue::Pair(
            Box::new(SendableValue::Symbol("engine".into())),
            Box::new(SendableValue::Fixnum(99)),
        );
        // a's accept loop registers b asynchronously; retry the poll until the
        // framed message has crossed and a's router has the peer + frame.
        primop_node_send("tcp-b", "tcp-a", &msg).expect("send");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = primop_node_poll("tcp-a").expect("poll");
            if !got.is_empty() {
                assert_eq!(got, vec![msg]);
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("message never arrived over TCP");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn two_nodes_send_and_poll_over_mtls() {
        // The encrypted + mutually authenticated path: same as the TCP test but
        // the connection runs a real TLS 1.3 mutual handshake (dev identity)
        // before any frame flows. Proves node-listen-tls / node-connect-tls.
        primop_node_make("mtls-a").expect("make a");
        primop_node_make("mtls-b").expect("make b");
        let addr = primop_node_listen_tls("mtls-a", "127.0.0.1:0").expect("listen-tls");
        primop_node_connect_tls("mtls-b", &addr).expect("connect-tls");

        let msg = SendableValue::Pair(
            Box::new(SendableValue::Symbol("engine".into())),
            Box::new(SendableValue::Fixnum(7)),
        );
        primop_node_send("mtls-b", "mtls-a", &msg).expect("send");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = primop_node_poll("mtls-a").expect("poll");
            if !got.is_empty() {
                assert_eq!(got, vec![msg]);
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("message never arrived over mTLS");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn two_nodes_send_and_poll_over_quic() {
        // QUIC (mandatory TLS 1.3, one stream per channel). Same identity
        // handshake + routing as the others; proves node-listen-quic /
        // node-connect-quic.
        primop_node_make("quic-a").expect("make a");
        primop_node_make("quic-b").expect("make b");
        let addr = primop_node_listen_quic("quic-a", "127.0.0.1:0").expect("listen-quic");
        primop_node_connect_quic("quic-b", &addr).expect("connect-quic");

        let msg = SendableValue::Pair(
            Box::new(SendableValue::Symbol("engine".into())),
            Box::new(SendableValue::Fixnum(11)),
        );
        primop_node_send("quic-b", "quic-a", &msg).expect("send");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = primop_node_poll("quic-a").expect("poll");
            if !got.is_empty() {
                assert_eq!(got, vec![msg]);
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("message never arrived over QUIC");
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
