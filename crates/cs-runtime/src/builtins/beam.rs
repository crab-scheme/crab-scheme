//! BEAM-style actor + table + hot-reload primops, exposed to
//! Scheme as builtins. Behind the `actor` feature.
//!
//! See `docs/research/beam_runtime_spec.md`. This module is the
//! glue layer between the three Rust crates (cs-actor / cs-table /
//! cs-hotreload) and the user-facing Scheme surface that
//! `lib/beam/prelude.scm` builds on.
//!
//! ## What's here
//!
//! - [`SendableValue`] — the subset of `cs_core::Value` that can
//!   cross an actor boundary. Procedures, GC-managed cell types,
//!   and ports are *not* representable: per BEAM's copy-on-send
//!   model, every cross-actor value gets deep-cloned into the
//!   receiver's heap. Sharing a `Rc<Slot<T>>` across threads
//!   wouldn't be safe (Rc is `!Send`) and wouldn't match the
//!   spec's "send-copies semantics" choice.
//! - [`to_sendable`] / [`from_sendable`] — the boundary
//!   conversions. Source / sink for the `Payload` type-erased
//!   `Arc<dyn Any + Send + Sync>` used by cs-actor and cs-table.
//! - [`actor_table_primops`] / [`hotreload_primops`] —
//!   higher-order builtin entries to plug into
//!   `pure_builtins()` / `higher_order_builtins()` in `mod.rs`.
//!
//! ## What's *not* here (yet)
//!
//! - `(spawn thunk)` with a real Scheme thunk. The closure
//!   captures `Rc` references and Symbols local to its source
//!   Runtime, so it can't cross to a child actor's Runtime as-is.
//!   The first cut will require the spawned procedure to be a
//!   *top-level name* the child Runtime can resolve by re-load:
//!   `(spawn 'my-mod:my-proc)`. That keeps thunk-transport out
//!   of the boundary and matches BEAM's `spawn(Mod, Fun, Args)`.
//!   Implementation lands when this module joins the dispatch
//!   layer; design is sketched below.
//! - Selective receive at the Rust level — that's done in
//!   `lib/beam/prelude.scm`'s `(receive ...)` macro on top of
//!   the `raw-receive` primop. The Rust side stays a plain
//!   blocking dequeue.

#![cfg(feature = "actor")]

use std::sync::Arc;

use cs_core::{Number, Pair, SymbolTable, Value};

use cs_actor::{ActorPid, Payload};

// ============================================================
// SendableValue — the cross-actor value subset.
// ============================================================

/// A subset of `cs_core::Value` safe to ship across actor
/// boundaries.
///
/// Why a separate type instead of just `Value`?
///
/// 1. `Value` holds `Rc<Slot<T>>` for its GC-managed variants
///    (Pair, Vector, String, Hashtable, ...). `Rc` is `!Send`;
///    sending one across threads is UB.
///
/// 2. The interned `Symbol(u32)` IDs are per-`SymbolTable` and
///    therefore per-actor. Symbol "foo" might be `Symbol(42)` in
///    actor A and `Symbol(99)` in actor B. The boundary has to
///    carry the *name*, not the local ID.
///
/// 3. Procedures (`Rc<dyn Procedure>`) close over the source
///    Runtime's environment. Re-creating them in the receiver is
///    a bigger problem than this struct solves; see the module
///    docs on `(spawn 'name)`.
///
/// The encoding is `Send + Sync + 'static` so it can ride
/// straight inside a `cs_actor::Payload`.
#[derive(Debug, Clone, PartialEq)]
pub enum SendableValue {
    Null,
    Unspecified,
    Eof,
    Boolean(bool),
    Character(char),
    Fixnum(i64),
    Flonum(f64),
    /// Bigints fall through to their decimal-string serialization
    /// for now. We can revisit if profiling shows this is hot.
    BigInt(String),
    String(String),
    /// Symbols cross as their name; the receiver re-interns into
    /// its own SymbolTable on arrival.
    Symbol(String),
    Pair(Box<SendableValue>, Box<SendableValue>),
    Vector(Vec<SendableValue>),
    ByteVector(Vec<u8>),
    /// An actor PID is the canonical sendable handle.
    Pid(ActorPid),
}

/// Convert a Scheme `Value` to its sendable representation. Fails
/// for types that can't safely cross actor boundaries
/// (procedures, ports, promises, hashtables — the latter is
/// representable in principle but the v1 design treats it as a
/// per-actor concept; share state via `cs-table` instead).
pub fn to_sendable(v: &Value) -> Result<SendableValue, String> {
    match v {
        Value::Null => Ok(SendableValue::Null),
        Value::Unspecified => Ok(SendableValue::Unspecified),
        Value::Eof => Ok(SendableValue::Eof),
        Value::Boolean(b) => Ok(SendableValue::Boolean(*b)),
        Value::Character(c) => Ok(SendableValue::Character(*c)),
        Value::Number(n) => num_to_sendable(n),
        Value::String(s) => Ok(SendableValue::String(s.borrow().clone())),
        // Symbol -> string requires a SymbolTable to resolve the
        // u32 to its name; the caller supplies one via the
        // `to_sendable_in` variant below. The bare `to_sendable`
        // can't see one, so it rejects symbols. Callers in this
        // module use the `_in` variant.
        Value::Symbol(_) => {
            Err("to_sendable: symbol requires SymbolTable; use to_sendable_in".into())
        }
        Value::Pair(p) => {
            let head = to_sendable(&p.car.borrow())?;
            let tail = to_sendable(&p.cdr.borrow())?;
            Ok(SendableValue::Pair(Box::new(head), Box::new(tail)))
        }
        Value::Vector(v) => {
            let items: Result<Vec<_>, _> = v.borrow().iter().map(to_sendable).collect();
            Ok(SendableValue::Vector(items?))
        }
        Value::ByteVector(bv) => Ok(SendableValue::ByteVector(bv.borrow().clone())),
        Value::Procedure(_) => Err("to_sendable: procedures cannot cross actor boundaries".into()),
        Value::Hashtable(_) => {
            Err("to_sendable: hashtables are per-actor; use cs-table for shared state".into())
        }
        Value::Port(_) => Err("to_sendable: ports cannot cross actor boundaries".into()),
        Value::Promise(_) => Err("to_sendable: promises cannot cross actor boundaries".into()),
    }
}

/// Like [`to_sendable`] but resolves Symbols against the source
/// SymbolTable so they cross as names. Recursive callers must
/// keep using this variant so nested symbols inside pairs /
/// vectors are also resolved.
pub fn to_sendable_in(v: &Value, syms: &SymbolTable) -> Result<SendableValue, String> {
    match v {
        Value::Symbol(s) => Ok(SendableValue::Symbol(syms.name(*s).to_string())),
        Value::Pair(p) => {
            let head = to_sendable_in(&p.car.borrow(), syms)?;
            let tail = to_sendable_in(&p.cdr.borrow(), syms)?;
            Ok(SendableValue::Pair(Box::new(head), Box::new(tail)))
        }
        Value::Vector(v) => {
            let items: Result<Vec<_>, _> =
                v.borrow().iter().map(|e| to_sendable_in(e, syms)).collect();
            Ok(SendableValue::Vector(items?))
        }
        other => to_sendable(other),
    }
}

/// Rebuild a Scheme `Value` from a sendable representation in
/// the destination actor's environment. Re-interns symbols
/// against the destination SymbolTable.
pub fn from_sendable(s: &SendableValue, syms: &mut SymbolTable) -> Value {
    match s {
        SendableValue::Null => Value::Null,
        SendableValue::Unspecified => Value::Unspecified,
        SendableValue::Eof => Value::Eof,
        SendableValue::Boolean(b) => Value::Boolean(*b),
        SendableValue::Character(c) => Value::Character(*c),
        SendableValue::Fixnum(n) => Value::Number(Number::Fixnum(*n)),
        SendableValue::Flonum(f) => Value::Number(Number::from_f64(*f)),
        SendableValue::BigInt(d) => {
            // Round-trip via the decimal string. Parser failure
            // is impossible: we produced this string in
            // num_to_sendable from a valid bigint.
            Value::Number(Number::parse_decimal_integer(d).expect("bigint round-trip"))
        }
        SendableValue::String(s) => {
            Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.clone())))
        }
        SendableValue::Symbol(name) => Value::Symbol(syms.intern(name)),
        SendableValue::Pair(car, cdr) => {
            let car_v = from_sendable(car, syms);
            let cdr_v = from_sendable(cdr, syms);
            Value::Pair(Pair::new(car_v, cdr_v))
        }
        SendableValue::Vector(items) => {
            let rebuilt: Vec<Value> = items.iter().map(|e| from_sendable(e, syms)).collect();
            Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(rebuilt)))
        }
        SendableValue::ByteVector(bytes) => {
            Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(bytes.clone())))
        }
        SendableValue::Pid(_pid) => {
            // PIDs are represented in Scheme by a symbol of the
            // form `<node.local>`. Once cs-typer gains a real
            // `Pid` type variant we'll replace this with
            // `Value::Pid(pid)`. For now the symbolic form is
            // stable and printable.
            let s = format!("<pid:{}>", _pid);
            Value::Symbol(syms.intern(&s))
        }
    }
}

fn num_to_sendable(n: &Number) -> Result<SendableValue, String> {
    match n {
        Number::Fixnum(i) => Ok(SendableValue::Fixnum(*i)),
        Number::Flonum(f) => Ok(SendableValue::Flonum(*f)),
        Number::Big(b) => Ok(SendableValue::BigInt(b.to_str_radix(10))),
        Number::Rat(_) => Err("to_sendable: rationals not yet supported across actors".into()),
    }
}

// ============================================================
// Payload wrappers.
// ============================================================
//
// cs-actor `Payload` and cs-table value cells are both
// `Arc<dyn Any + Send + Sync>`. We carry a SendableValue inside
// the Arc; downcast at the receiver to pull it back out.

/// Wrap a SendableValue as a cs-actor `Payload`.
pub fn payload_of(s: SendableValue) -> Payload {
    Arc::new(s)
}

/// Unwrap a cs-actor `Payload` back to a SendableValue.
/// Returns `None` if the payload was produced by some other code
/// path with a different inner type (e.g., a Rust-side actor
/// embedding raw `String` payloads in a test).
pub fn payload_to_sendable(p: &Payload) -> Option<SendableValue> {
    p.downcast_ref::<SendableValue>().cloned()
}

// ============================================================
// Cross-actor key conversion for cs-table.
// ============================================================
//
// cs-table's `Key` enum is intentionally narrow (Fixnum / String
// / Bytes). Map a SendableValue → Key for the small set of
// values that have sensible Hash + Ord; reject everything else.

pub fn key_of(s: &SendableValue) -> Result<cs_table::Key, String> {
    match s {
        SendableValue::Fixnum(i) => Ok(cs_table::Key::Fixnum(*i)),
        SendableValue::String(s) => Ok(cs_table::Key::String(s.clone())),
        SendableValue::Symbol(name) => Ok(cs_table::Key::String(name.clone())),
        SendableValue::ByteVector(b) => Ok(cs_table::Key::Bytes(b.clone())),
        other => Err(format!(
            "cs-table key must be fixnum / string / symbol / bytevector; got {:?}",
            other
        )),
    }
}

// ============================================================
// BeamState — the global Actor/Table/HotReload state.
// ============================================================
//
// Per the spec, the BEAM-style primops are process-global:
// PIDs, tables, and module versions are identifiers that any
// actor in the system can hand to any other actor. We hold the
// three Rust subsystems in a single shared state and register
// it once at runtime startup.
//
// Thread safety: ActorSystem owns a tokio Runtime that handles
// its own internal locking; TableRegistry and VersionRegistry
// are both Arc + DashMap internally. We expose them as
// Arc-shared handles so any actor's body closure can clone +
// capture them.

use cs_actor::{ActorRef, ActorSystem, ExitReason, Message};
use cs_hotreload::VersionRegistry;
use cs_table::{TableRegistry, TableType};
use std::sync::OnceLock;
use std::time::Duration;

/// Process-wide singleton holding the ActorSystem, TableRegistry,
/// and VersionRegistry. Lazily initialised on first access.
///
/// Why a singleton instead of per-Runtime state? PIDs are
/// globally meaningful — actor A in Runtime R1 should be able to
/// send to actor B in Runtime R2. Stuffing the state into each
/// Runtime would force cross-Runtime PID resolution. A single
/// process-wide system matches BEAM's model (one VM, many
/// processes).
pub struct BeamState {
    pub actors: ActorSystem,
    pub tables: TableRegistry,
    pub versions: VersionRegistry,
    pub procs: ProcedureRegistry,
}

impl BeamState {
    fn new() -> Self {
        Self {
            actors: ActorSystem::new(),
            tables: TableRegistry::new(),
            versions: VersionRegistry::new(),
            procs: ProcedureRegistry::default(),
        }
    }
}

static BEAM_STATE: OnceLock<BeamState> = OnceLock::new();

/// Access the process-wide BeamState, initialising on first call.
pub fn beam_state() -> &'static BeamState {
    BEAM_STATE.get_or_init(BeamState::new)
}

// ============================================================
// ProcedureRegistry — the answer to "how do thunks cross threads?"
// ============================================================
//
// BEAM solves this with `spawn(Mod, Fun, Args)`: the receiver
// loads the named module fresh and calls the function. Our v1
// equivalent: a process-wide registry mapping a Scheme-side
// symbol-name to a Rust closure that runs inside the spawned
// actor's blocking thread.
//
// The Scheme-side workflow:
//   (register-procedure! 'my-mod:start <rust-defined entry>)
//   (spawn 'my-mod:start arg1 arg2 ...)
//
// For now `register-procedure!` is only callable from Rust (tests,
// FFI). The follow-up iter teaches the registrar to compile a
// Scheme source-string into a thunk, opening up the registration
// to Scheme code. Until then, integration tests register their
// own procedures via [`ProcedureRegistry::register`] directly.

/// A procedure that can run inside a spawned actor's body.
///
/// Signature: takes the actor's environment + the decoded
/// arguments (already converted from SendableValue) and runs
/// the actor's main loop. The closure is `Send + Sync` so it can
/// be cloned into the spawned thread without lifetime gymnastics.
pub type ActorEntry = Arc<dyn Fn(&mut cs_actor::Actor, Vec<SendableValue>) + Send + Sync + 'static>;

#[derive(Default)]
pub struct ProcedureRegistry {
    entries: std::sync::Mutex<std::collections::HashMap<String, ActorEntry>>,
}

impl ProcedureRegistry {
    /// Register a procedure under a canonical name. Overwrites
    /// any prior registration (matches hot-reload semantics —
    /// later registrations replace earlier ones).
    pub fn register(&self, name: &str, entry: ActorEntry) {
        self.entries
            .lock()
            .expect("procedure registry poisoned")
            .insert(name.to_string(), entry);
    }

    /// Look up by name. Returns a clone of the Arc so the caller
    /// can hand it to the spawned thread.
    pub fn lookup(&self, name: &str) -> Option<ActorEntry> {
        self.entries
            .lock()
            .expect("procedure registry poisoned")
            .get(name)
            .cloned()
    }
}

// ============================================================
// Primop implementations (Rust side).
// ============================================================
//
// These functions are the Rust-callable shape of the BEAM
// primops. The Scheme-builtin wrappers further down convert
// Value <-> SendableValue at the boundary and call into these.

/// `(spawn 'name args...)` — look up the named entry in the
/// procedure registry and spawn an actor that runs it.
pub fn primop_spawn(name: &str, args: Vec<SendableValue>) -> Result<ActorPid, String> {
    let st = beam_state();
    let entry = st
        .procs
        .lookup(name)
        .ok_or_else(|| format!("spawn: no procedure registered under {:?}", name))?;
    let actor_ref = st.actors.spawn(move |actor| entry(actor, args));
    Ok(actor_ref.pid())
}

/// `(send pid v)` — deliver `v` to the actor identified by `pid`.
/// Fails only if the actor has terminated.
pub fn primop_send(pid: ActorPid, value: SendableValue) -> Result<(), String> {
    let st = beam_state();
    let actor_ref = st
        .actors
        .lookup(pid)
        .ok_or_else(|| format!("send: actor {} not found (terminated?)", pid))?;
    actor_ref
        .send(payload_of(value))
        .map_err(|e| format!("send to {}: {}", pid, e))
}

/// Look up an ActorRef given a PID. Returns None if the actor
/// is dead. Useful for the supervisor / behavior plumbing that
/// wants to construct fresh references.
pub fn lookup_pid(pid: ActorPid) -> Option<ActorRef> {
    beam_state().actors.lookup(pid)
}

/// `(raw-receive timeout-ms)` — receive the next message. Pass
/// `None` for a blocking receive; `Some(0)` for try-once;
/// `Some(N)` for up to N ms wait.
///
/// Returns:
/// - `Ok(Some(sv))` — a user message
/// - `Ok(None)` — timeout fired (only with `Some(ms)`)
/// - `Err(reason)` — the actor's mailbox is closed (system
///   shutting down)
///
/// System messages (Exit/Down) are converted to tagged lists
/// the Scheme `(receive)` macro can pattern-match:
///   `(*exit* <from-pid-symbol> <reason-string>)`
///   `(*down* <ref-id> <pid-symbol> <reason-string>)`
pub fn primop_raw_receive(
    actor: &mut cs_actor::Actor,
    timeout_ms: Option<u64>,
) -> Result<Option<SendableValue>, String> {
    let msg_opt = match timeout_ms {
        None => actor.receive(),
        Some(0) => match actor.try_receive() {
            Ok(m) => Some(m),
            Err(cs_actor::TryRecvError::Empty) => return Ok(None),
            Err(cs_actor::TryRecvError::Disconnected) => {
                return Err("raw-receive: mailbox closed".into())
            }
        },
        Some(ms) => {
            // Spin-poll with a short sleep until deadline. Crude
            // but correct; B3's async-receive uses tokio's
            // mailbox-with-timeout primitive directly. For v1
            // this is fine since we're on a dedicated blocking
            // thread per actor.
            let deadline = std::time::Instant::now() + Duration::from_millis(ms);
            loop {
                match actor.try_receive() {
                    Ok(m) => break Some(m),
                    Err(cs_actor::TryRecvError::Disconnected) => {
                        return Err("raw-receive: mailbox closed".into())
                    }
                    Err(cs_actor::TryRecvError::Empty) => {
                        if std::time::Instant::now() >= deadline {
                            return Ok(None);
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }
    };

    let msg = match msg_opt {
        Some(m) => m,
        None => return Err("raw-receive: mailbox closed".into()),
    };

    Ok(Some(message_to_sendable(msg)))
}

fn message_to_sendable(msg: Message) -> SendableValue {
    match msg {
        Message::User(payload) => payload_to_sendable(&payload).unwrap_or_else(|| {
            // Payloads from Rust-side test fixtures that don't
            // use SendableValue come through as opaque-tagged
            // values. Wrap as a placeholder symbol so Scheme
            // pattern-match still has something to compare.
            SendableValue::Symbol("*opaque-payload*".into())
        }),
        Message::Exit { from, reason } => sendable_list(vec![
            SendableValue::Symbol("*exit*".into()),
            SendableValue::Pid(from),
            SendableValue::Symbol(exit_reason_str(&reason)),
        ]),
        Message::Down {
            ref_id,
            pid,
            reason,
        } => sendable_list(vec![
            SendableValue::Symbol("*down*".into()),
            SendableValue::Fixnum(ref_id as i64),
            SendableValue::Pid(pid),
            SendableValue::Symbol(exit_reason_str(&reason)),
        ]),
    }
}

fn exit_reason_str(r: &ExitReason) -> String {
    match r {
        ExitReason::Normal => "normal".into(),
        ExitReason::Killed => "killed".into(),
        ExitReason::User(s) => s.clone(),
        ExitReason::Error(s) => format!("error:{}", s),
    }
}

fn sendable_list(items: Vec<SendableValue>) -> SendableValue {
    let mut acc = SendableValue::Null;
    for item in items.into_iter().rev() {
        acc = SendableValue::Pair(Box::new(item), Box::new(acc));
    }
    acc
}

// ============================================================
// cs-table primop implementations.
// ============================================================

pub fn primop_make_table(name: &str, type_str: &str) -> Result<(), String> {
    let ty = match type_str {
        "set" => TableType::Set,
        "ordered-set" | "ordered_set" => TableType::OrderedSet,
        other => return Err(format!("make-table: unknown type {:?}", other)),
    };
    beam_state()
        .tables
        .create(name, ty)
        .map_err(|e| format!("make-table: {}", e))
}

pub fn primop_table_insert(
    name: &str,
    key: SendableValue,
    value: SendableValue,
) -> Result<(), String> {
    let k = key_of(&key)?;
    beam_state()
        .tables
        .insert(name, k, payload_of(value))
        .map_err(|e| format!("table-insert!: {}", e))
}

pub fn primop_table_lookup(
    name: &str,
    key: &SendableValue,
) -> Result<Option<SendableValue>, String> {
    let k = key_of(key)?;
    let p = beam_state()
        .tables
        .lookup(name, &k)
        .map_err(|e| format!("table-lookup: {}", e))?;
    Ok(p.and_then(|payload| payload_to_sendable(&payload)))
}

pub fn primop_table_delete(name: &str, key: &SendableValue) -> Result<bool, String> {
    let k = key_of(key)?;
    beam_state()
        .tables
        .delete(name, &k)
        .map_err(|e| format!("table-delete!: {}", e))
}

pub fn primop_table_size(name: &str) -> Result<usize, String> {
    beam_state()
        .tables
        .size(name)
        .map_err(|e| format!("table-size: {}", e))
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_atoms() {
        let mut syms = SymbolTable::new();

        let cases = [
            Value::Null,
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Character('λ'),
            Value::Number(Number::Fixnum(42)),
            Value::Number(Number::Flonum(3.14)),
            Value::Unspecified,
            Value::Eof,
        ];

        for v in &cases {
            let s = to_sendable_in(v, &syms).expect("encode");
            let back = from_sendable(&s, &mut syms);
            // Round-trip via the SendableValue projection:
            // re-encoding the rebuilt Value should hit the same
            // SendableValue (Value's PartialEq for Gc<RefCell<_>>
            // compares pointers, which won't match across rebuilds).
            let s2 = to_sendable_in(&back, &syms).expect("re-encode");
            assert_eq!(s, s2, "round-trip mismatch for {:?}", v);
        }
    }

    #[test]
    fn round_trip_string() {
        let mut syms = SymbolTable::new();
        let v = Value::String(cs_core::Gc::new(std::cell::RefCell::new("hello".into())));
        let s = to_sendable_in(&v, &syms).expect("encode");
        assert_eq!(s, SendableValue::String("hello".into()));
        let back = from_sendable(&s, &mut syms);
        if let Value::String(g) = &back {
            assert_eq!(*g.borrow(), "hello");
        } else {
            panic!("expected String");
        }
    }

    #[test]
    fn round_trip_symbol_reinterns() {
        // Symbol IDs are per-table. Sending "foo" across a
        // boundary should resolve to the destination's `foo`
        // symbol, NOT the source's u32.
        let mut src = SymbolTable::new();
        // Force the destination to allocate other symbols first
        // so its `foo` ends up at a different u32.
        let mut dst = SymbolTable::new();
        dst.intern("a");
        dst.intern("b");
        dst.intern("c");

        let src_foo = src.intern("foo");
        let v = Value::Symbol(src_foo);

        let s = to_sendable_in(&v, &src).expect("encode");
        assert_eq!(s, SendableValue::Symbol("foo".into()));

        let back = from_sendable(&s, &mut dst);
        if let Value::Symbol(dst_foo) = back {
            assert_eq!(dst.name(dst_foo), "foo");
            assert_ne!(src_foo.0, dst_foo.0, "interning offsets should differ");
        } else {
            panic!("expected Symbol");
        }
    }

    #[test]
    fn round_trip_pair_and_list() {
        let mut syms = SymbolTable::new();

        // (1 2 3) as a proper list = (1 . (2 . (3 . ())))
        let a = Pair::new(Value::Number(Number::Fixnum(3)), Value::Null);
        let b = Pair::new(Value::Number(Number::Fixnum(2)), Value::Pair(a));
        let lst = Value::Pair(Pair::new(Value::Number(Number::Fixnum(1)), Value::Pair(b)));

        let s = to_sendable_in(&lst, &syms).expect("encode");
        let back = from_sendable(&s, &mut syms);
        let s2 = to_sendable_in(&back, &syms).expect("re-encode");
        assert_eq!(s, s2);
    }

    #[test]
    fn round_trip_nested_with_symbols() {
        let mut src = SymbolTable::new();
        let mut dst = SymbolTable::new();

        let hello = src.intern("hello");
        let world = src.intern("world");
        // (hello . world)
        let v = Value::Pair(Pair::new(Value::Symbol(hello), Value::Symbol(world)));

        let s = to_sendable_in(&v, &src).expect("encode");
        let back = from_sendable(&s, &mut dst);
        let s2 = to_sendable_in(&back, &dst).expect("re-encode");
        assert_eq!(s, s2);
    }

    #[test]
    fn round_trip_vector() {
        let mut syms = SymbolTable::new();
        let v = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(vec![
            Value::Number(Number::Fixnum(1)),
            Value::Boolean(true),
            Value::Null,
        ])));
        let s = to_sendable_in(&v, &syms).expect("encode");
        let back = from_sendable(&s, &mut syms);
        let s2 = to_sendable_in(&back, &syms).expect("re-encode");
        assert_eq!(s, s2);
    }

    #[test]
    fn reject_unsendable_types() {
        let syms = SymbolTable::new();

        // Hashtables are per-actor in this design; share state via
        // cs-table instead. The encoder must reject them.
        use cs_core::{Hashtable, HtEqKind};
        let ht = Value::Hashtable(Hashtable::new(HtEqKind::Eq));
        match to_sendable_in(&ht, &syms) {
            Err(msg) => assert!(msg.contains("hashtable")),
            Ok(_) => panic!("hashtable should not be sendable"),
        }
    }

    #[test]
    fn payload_round_trip() {
        let mut syms = SymbolTable::new();
        let v = Value::Pair(Pair::new(
            Value::Number(Number::Fixnum(7)),
            Value::Boolean(false),
        ));
        let s = to_sendable_in(&v, &syms).expect("encode");
        let p = payload_of(s.clone());
        let s2 = payload_to_sendable(&p).expect("downcast");
        assert_eq!(s, s2);

        let back = from_sendable(&s2, &mut syms);
        let s3 = to_sendable_in(&back, &syms).expect("re-encode");
        assert_eq!(s, s3);
    }

    #[test]
    fn cs_table_key_conversion() {
        assert!(matches!(
            key_of(&SendableValue::Fixnum(7)),
            Ok(cs_table::Key::Fixnum(7))
        ));
        assert!(matches!(
            key_of(&SendableValue::String("k".into())),
            Ok(cs_table::Key::String(_))
        ));
        assert!(matches!(
            key_of(&SendableValue::Symbol("s".into())),
            Ok(cs_table::Key::String(_))
        ));
        assert!(matches!(
            key_of(&SendableValue::ByteVector(vec![1, 2, 3])),
            Ok(cs_table::Key::Bytes(_))
        ));
        // Booleans aren't sensible Hash + Ord keys for ETS.
        assert!(key_of(&SendableValue::Boolean(true)).is_err());
    }

    #[test]
    fn pid_round_trips_via_symbol() {
        // PIDs aren't a Value variant yet (waits on cs-typer's
        // Pid type). The interim encoding rebuilds them as a
        // printable symbol; verify the projection is stable.
        let mut syms = SymbolTable::new();
        let pid = ActorPid {
            node: 0,
            local_id: 17,
        };
        let s = SendableValue::Pid(pid);
        let v = from_sendable(&s, &mut syms);
        if let Value::Symbol(sym) = v {
            assert_eq!(syms.name(sym), "<pid:<0.17>>");
        } else {
            panic!("expected Symbol for PID");
        }
    }

    // ----------------------------------------------------------
    // Integration tests against the live BeamState singleton.
    //
    // Tests use unique procedure / table names so they don't
    // collide despite cargo running them in parallel.
    // ----------------------------------------------------------

    use std::sync::atomic::{AtomicI64, Ordering};

    #[test]
    fn spawn_send_receive_round_trip() {
        // Spawn an echo actor that receives one message and
        // stores it where the test thread can read it.
        let received: Arc<std::sync::Mutex<Option<SendableValue>>> =
            Arc::new(std::sync::Mutex::new(None));
        let received_clone = received.clone();

        beam_state().procs.register(
            "test:echo-once",
            Arc::new(move |actor, _args| {
                if let Ok(Some(msg)) = primop_raw_receive(actor, None) {
                    *received_clone.lock().unwrap() = Some(msg);
                }
            }),
        );

        let pid = primop_spawn("test:echo-once", vec![]).expect("spawn");
        primop_send(pid, SendableValue::Fixnum(42)).expect("send");

        // Wait for the echo actor to wake + record. 1s is plenty.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if received.lock().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("echo actor never received");
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(*received.lock().unwrap(), Some(SendableValue::Fixnum(42)));
    }

    #[test]
    fn ping_pong_two_actors() {
        // Pong: receive ('ping reply-to), send back ('pong)
        // Ping: spawn pong, send ('ping self), receive, store.

        let result: Arc<std::sync::Mutex<Option<SendableValue>>> =
            Arc::new(std::sync::Mutex::new(None));
        let result_clone = result.clone();

        beam_state().procs.register(
            "test:pong",
            Arc::new(move |actor, _args| {
                if let Ok(Some(msg)) = primop_raw_receive(actor, None) {
                    // Expected: (cons 'ping reply-pid)
                    if let SendableValue::Pair(head, tail) = msg {
                        if matches!(*head, SendableValue::Symbol(ref s) if s == "ping") {
                            if let SendableValue::Pid(reply) = *tail {
                                let _ = primop_send(reply, SendableValue::Symbol("pong".into()));
                            }
                        }
                    }
                }
            }),
        );

        beam_state().procs.register(
            "test:ping",
            Arc::new(move |actor, args| {
                let pong_pid = match args.first() {
                    Some(SendableValue::Pid(p)) => *p,
                    _ => return,
                };
                let my_pid = actor.self_ref().pid();
                let msg = SendableValue::Pair(
                    Box::new(SendableValue::Symbol("ping".into())),
                    Box::new(SendableValue::Pid(my_pid)),
                );
                let _ = primop_send(pong_pid, msg);
                if let Ok(Some(reply)) = primop_raw_receive(actor, Some(1000)) {
                    *result_clone.lock().unwrap() = Some(reply);
                }
            }),
        );

        let pong_pid = primop_spawn("test:pong", vec![]).expect("spawn pong");
        let _ping_pid =
            primop_spawn("test:ping", vec![SendableValue::Pid(pong_pid)]).expect("spawn ping");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if result.lock().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("ping/pong never completed");
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(
            *result.lock().unwrap(),
            Some(SendableValue::Symbol("pong".into()))
        );
    }

    #[test]
    fn raw_receive_timeout_returns_none() {
        // Actor that never gets a message; raw-receive with a
        // short timeout returns Ok(None).
        let outcome: Arc<AtomicI64> = Arc::new(AtomicI64::new(-1));
        let outcome_clone = outcome.clone();

        beam_state().procs.register(
            "test:timeout-actor",
            Arc::new(
                move |actor, _args| match primop_raw_receive(actor, Some(50)) {
                    Ok(None) => outcome_clone.store(0, Ordering::SeqCst),
                    Ok(Some(_)) => outcome_clone.store(1, Ordering::SeqCst),
                    Err(_) => outcome_clone.store(2, Ordering::SeqCst),
                },
            ),
        );

        let _pid = primop_spawn("test:timeout-actor", vec![]).expect("spawn");

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if outcome.load(Ordering::SeqCst) >= 0 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("timeout actor never finished");
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(outcome.load(Ordering::SeqCst), 0, "expected Ok(None)");
    }

    #[test]
    fn table_crud_round_trip() {
        let tn = "test:crud-table";
        primop_make_table(tn, "set").expect("make-table");
        primop_table_insert(
            tn,
            SendableValue::String("alice".into()),
            SendableValue::Fixnum(1),
        )
        .expect("insert");
        primop_table_insert(
            tn,
            SendableValue::String("bob".into()),
            SendableValue::Fixnum(2),
        )
        .expect("insert");

        assert_eq!(
            primop_table_lookup(tn, &SendableValue::String("alice".into())).unwrap(),
            Some(SendableValue::Fixnum(1))
        );
        assert_eq!(primop_table_size(tn).unwrap(), 2);

        let removed =
            primop_table_delete(tn, &SendableValue::String("alice".into())).expect("delete");
        assert!(removed);
        assert_eq!(primop_table_size(tn).unwrap(), 1);
        assert_eq!(
            primop_table_lookup(tn, &SendableValue::String("alice".into())).unwrap(),
            None
        );
    }

    #[test]
    fn make_table_twice_errors() {
        let tn = "test:dup-table";
        primop_make_table(tn, "set").expect("first create");
        let err = primop_make_table(tn, "set").expect_err("second should fail");
        assert!(err.contains("already exists"), "got: {}", err);
    }

    #[test]
    fn table_unknown_type_errors() {
        let err = primop_make_table("test:bad-type-table", "bag").expect_err("bag unsupported");
        assert!(err.contains("unknown type"), "got: {}", err);
    }
}
