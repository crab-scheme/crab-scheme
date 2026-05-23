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
use std::sync::{Arc, Mutex, OnceLock};

use cs_core::{Pair, SymbolTable, Value};

use cs_distrib::pid::DistPid;
use cs_distrib::router::Router;
use cs_distrib::NodeId;
use cs_net::sim::SimPair;
use cs_net::Transport;

use super::beam::{from_sendable, to_sendable_in, SendableValue};

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
    let msg = to_sendable_in(&args[2], syms)?;
    primop_node_send(&from, &to, &msg)?;
    Ok(Value::Unspecified)
}

/// `(node-poll NODE)` — returns a list of the messages delivered to NODE.
pub fn b_node_poll(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("node-poll: expected (node-poll NODE)".into());
    }
    let node = name_of(&args[0], syms, "node-poll")?;
    let msgs = primop_node_poll(&node)?;
    let mut list = Value::Null;
    for sv in msgs.iter().rev() {
        list = Value::Pair(Pair::new(from_sendable(sv, syms), list));
    }
    Ok(list)
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
        ("node-send", b_node_send),
        ("node-poll", b_node_poll),
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
}
