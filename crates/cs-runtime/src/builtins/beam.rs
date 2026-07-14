//! BEAM-style actor + table + hot-reload primops, exposed to
//! Scheme as builtins. Behind the `actor` feature.
//!
//! See `docs/research/beam_runtime_spec.md` for the design and
//! `docs/milestones/beam-v1-exit.md` for what shipped.

#![cfg(feature = "actor")]

use std::sync::Arc;

use cs_core::{Number, Pair, SymbolTable, Value};

use cs_actor::{ActorPid, Payload};

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};

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
    // Channel handles ride as the tagged pair `(channel <id>)`
    // through the existing Pair/Symbol/Fixnum surface — no
    // dedicated variant needed. cs-runtime/builtins/channel.rs
    // recognizes the shape on inspection and looks the ID up in
    // the process-global ChannelRegistry.
    /// cs-845.1 same-worker fast-send path: a marker for a real
    /// (already deep-cloned) `Value` parked in this OS thread's
    /// `SAME_WORKER_MSGS` side-table, keyed by the receiver's PID.
    /// `recv` is the receiving actor's PID (the queue key); `seq` is
    /// a per-receiver monotonic stamp used only to sanity-check FIFO
    /// order on the pop side. Only ever produced by
    /// [`try_send_same_worker`] and only ever consumed by
    /// [`from_sendable`] running on the *same* worker thread (the
    /// sender checked colocation before picking this path) — both
    /// fields are plain `Copy` integers, so this satisfies `Payload`'s
    /// `Send`/`Sync` bound, but the referenced `Value` lives in a
    /// thread-local and is meaningless off the thread that produced it.
    Local {
        recv: ActorPid,
        seq: u64,
    },
}

/// Project a Scheme `Value` onto a `SendableValue`. Symbols
/// cross as their names (re-interned in the destination's
/// SymbolTable on the other side). Procedures, ports, promises,
/// and hashtables can't cross — see the SendableValue doc for
/// the design call.
pub fn to_sendable_in(v: &Value, syms: &SymbolTable) -> Result<SendableValue, String> {
    match v {
        Value::Null => Ok(SendableValue::Null),
        Value::Unspecified => Ok(SendableValue::Unspecified),
        Value::Eof => Ok(SendableValue::Eof),
        Value::Boolean(b) => Ok(SendableValue::Boolean(*b)),
        Value::Character(c) => Ok(SendableValue::Character(*c)),
        nv @ (Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)) => {
            let n = nv.as_number().unwrap();
            num_to_sendable(&n)
        }
        Value::String(s) => Ok(SendableValue::String(s.borrow().clone())),
        Value::Symbol(s) => Ok(SendableValue::Symbol(syms.name(*s).to_string())),
        // Identifiers cross as their name (mark dropped). Marks
        // are a per-process lexical-hygiene concept that doesn't
        // survive a cross-actor boundary -- a fresh expansion in
        // the destination would assign new marks anyway.
        Value::Identifier { name, .. } => Ok(SendableValue::Symbol(syms.name(*name).to_string())),
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
        Value::ByteVector(bv) => Ok(SendableValue::ByteVector(bv.borrow().clone())),
        // Procedures own an Rc<Env> (the closure capture environment),
        // and Rc is !Send. Two consumers funnel through this arm and
        // hit the same wall for different reasons:
        //
        // - **Cross-actor `(send pid …)`**: as named. The receiver's
        //   heap is a different Rc graph; even with a hypothetical
        //   Send-wrapped Rc the closure env points at the *sender's*
        //   bindings, which the receiver cannot resolve. Rehydration
        //   needs the receiver to re-evaluate the lambda against its
        //   own definition environment — outside the scope of this
        //   value-projection helper.
        //
        // - **`load-module!`**: less obviously a "cross-actor" use,
        //   but it goes through the same `SendableValue` projection
        //   (cs-hotreload's `Export = Arc<dyn Any + Send + Sync>` is
        //   the registry's storage shape). The BEAM-style "module
        //   exports are code" model needs procedures here; today the
        //   gap blocks #29 (JIT-invalidation on hot reload) because
        //   no procedure can reach the version registry in the first
        //   place. See ADR 0034 for the deferral analysis + the two
        //   paths past this wall (Send heaps vs. per-actor
        //   rehydration).
        Value::Procedure(_) => Err(
            "to_sendable: procedures cannot cross actor boundaries (also blocks procedures \
             as `load-module!` exports — see ADR 0034 for the architectural prerequisite)"
                .into(),
        ),
        Value::Hashtable(_) => {
            Err("to_sendable: hashtables are per-actor; use cs-table for shared state".into())
        }
        Value::Port(_) => Err("to_sendable: ports cannot cross actor boundaries".into()),
        Value::Promise(_) => Err("to_sendable: promises cannot cross actor boundaries".into()),
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
        SendableValue::Fixnum(n) => Value::Fixnum(*n),
        SendableValue::Flonum(f) => Value::from_number(Number::from_f64(*f)),
        SendableValue::BigInt(d) => {
            // Round-trip via the decimal string. Parser failure
            // is impossible: we produced this string in
            // num_to_sendable from a valid bigint.
            Value::from_number(Number::parse_decimal_integer(d).expect("bigint round-trip"))
        }
        SendableValue::String(s) => Value::string(s.clone()),
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
        SendableValue::Local { recv, seq } => take_same_worker_msg(*recv, *seq, syms),
    }
}

// ---------- cs-845.1: same-worker fast-send path ----------
//
// `to_sendable_in`/`from_sendable` round-trip through a fully separate
// `SendableValue` tree so a message can cross an OS-thread boundary (the
// mailbox `Payload` is `Arc<dyn Any + Send + Sync>`, and `Value`'s `Rc`s
// are `!Send`). When sender and receiver are actors pinned to the *same*
// `LocalWorkerPool` worker (see `cs_actor::ActorSystem::local_worker_of` /
// `cs_actor::current_worker_id`), that whole projection is unnecessary
// work: nothing ever crosses threads, so a same-thread deep clone of the
// `Value` tree (fresh `Gc`/`RefCell` allocations, so mutating the
// original after send can't leak into the receiver's copy) is both
// sufficient and one tree-walk instead of two.
//
// Symbols are the one wrinkle: `Symbol` ids are private to a
// `SymbolTable`, so an id from the sender's table isn't valid in the
// receiver's — *unless* both tables are `SymbolTable::with_base` layers
// over the identical `Rc`-shared base, which is exactly what every
// `spawn_local_activation` actor on a given worker gets (built via
// `Runtime::from_image(&worker_runtime_image())`, the same thread-local
// image for the worker's whole lifetime). Base ids (`syms.is_base`) are
// therefore safe to copy verbatim; a symbol interned into an actor's
// *private* extension table isn't, and we don't have receiver-side
// access to intern it correctly at send time. When the clone hits such a
// symbol it bails out (`Ok(None)`) and the caller falls back to the
// general `to_sendable_in`/`from_sendable` path, unchanged.
/// Per-receiver side-queue for the same-worker fast path. `next_seq` stamps
/// each parked message with a monotonic id (FIFO sanity check on pop);
/// `msgs` holds the `(seq, Value)` pairs in send order. Popped front-first on
/// receive — mailbox marker order matches queue push order on this one thread,
/// so FIFO relative to cross-worker messages is preserved.
#[derive(Default)]
struct SameWorkerQueue {
    next_seq: u64,
    msgs: std::collections::VecDeque<(u64, Value)>,
}

std::thread_local! {
    static SAME_WORKER_MSGS: std::cell::RefCell<std::collections::HashMap<ActorPid, SameWorkerQueue>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// RAII cleanup for one actor's same-worker fast-path side-queue, keyed by the
/// actor's own PID (the receiver key). Registered at the top of every actor
/// body that runs on a `LocalWorkerPool` worker ([`activation_body`] and
/// [`green_source_body`]), so its `Drop` runs on *every* exit path — normal
/// return, a Scheme-level error, a panic-unwind, or the future being dropped on
/// shutdown. Dropping removes the actor's whole queue, so any fast-path
/// messages parked for it but never received (the actor died with `Local` mail
/// still queued) are freed instead of leaking for the worker's lifetime. It
/// only touches the thread-local map — never `rt`/`actor` heap — so its drop
/// order relative to `co`/`rt`/`actor` is immaterial.
struct SameWorkerGuard(ActorPid);

impl Drop for SameWorkerGuard {
    fn drop(&mut self) {
        SAME_WORKER_MSGS.with(|m| {
            m.borrow_mut().remove(&self.0);
        });
    }
}

/// Process-wide count of sends that took the same-worker fast path.
/// Telemetry for tests + perf analysis (a Relaxed add is negligible next
/// to the tree clone it accounts for).
static SAME_WORKER_FAST_SENDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many `(send ...)`s have taken the same-worker fast path so far.
pub fn same_worker_fast_send_count() -> u64 {
    SAME_WORKER_FAST_SENDS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pull a same-worker message back out for receiver `recv`. Only called from
/// `from_sendable` for a `SendableValue::Local`, which — per the module doc
/// above — is only ever handed to a receiver running on the same OS thread that
/// produced it, so the front of `recv`'s queue is normally the matching message
/// (`seq` confirms FIFO). Any deviation is an *unforeseen* edge, not a
/// correctness invariant we can abort the whole worker thread over: we
/// degrade-and-log (a benign opaque `same-worker-message-lost` symbol) so the
/// blast radius is one dropped/garbled message, never a worker-thread panic.
fn take_same_worker_msg(recv: ActorPid, seq: u64, syms: &mut SymbolTable) -> Value {
    let popped = SAME_WORKER_MSGS.with(|m| {
        m.borrow_mut()
            .get_mut(&recv)
            .and_then(|q| q.msgs.pop_front())
    });
    match popped {
        Some((got_seq, v)) => {
            if got_seq != seq {
                eprintln!(
                    "cs-845.1: same-worker message for {recv} out of order (marker seq {seq}, \
                     queue front seq {got_seq}); FIFO invariant violated — delivering front anyway"
                );
            }
            v
        }
        None => {
            eprintln!(
                "cs-845.1: same-worker message for {recv} (seq {seq}) missing on receive \
                 (guard/ordering bug in the fast-send path); dropping message"
            );
            Value::Symbol(syms.intern("same-worker-message-lost"))
        }
    }
}

/// Deep-clone `v` into a fresh `Value` tree suitable for handing directly
/// to a colocated receiver, reusing symbol ids where — and only where —
/// that's sound (see the module doc above).
///
/// - `Ok(Some(clone))`: fast path is safe, here's the clone.
/// - `Ok(None)`: `v` contains a non-base (per-actor-private) symbol;
///   caller should fall back to `to_sendable_in`/`from_sendable`.
/// - `Err(_)`: `v` contains something that can never cross an actor
///   boundary at all (procedure/hashtable/port/promise) — same rejects as
///   `to_sendable_in`.
fn deep_clone_same_worker(v: &Value, syms: &SymbolTable) -> Result<Option<Value>, String> {
    macro_rules! recur {
        ($e:expr) => {
            match deep_clone_same_worker($e, syms)? {
                Some(cloned) => cloned,
                None => return Ok(None),
            }
        };
    }
    Ok(Some(match v {
        Value::Null => Value::Null,
        Value::Unspecified => Value::Unspecified,
        Value::Eof => Value::Eof,
        Value::Boolean(b) => Value::Boolean(*b),
        Value::Character(c) => Value::Character(*c),
        Value::Fixnum(n) => Value::Fixnum(*n),
        Value::Flonum(f) => Value::Flonum(*f),
        // Immutable (no interior mutability) boxed numbers — an `Rc`
        // clone is a legitimate deep-clone-equivalent, same as sharing
        // any other immutable value.
        Value::BigNumber(b) => Value::BigNumber(b.clone()),
        Value::Rational(r) => Value::Rational(r.clone()),
        Value::String(s) => Value::string(s.borrow().clone()),
        Value::Symbol(sym) => {
            if syms.is_base(*sym) {
                Value::Symbol(*sym)
            } else {
                return Ok(None);
            }
        }
        Value::Identifier { name, mark } => {
            if syms.is_base(*name) {
                // Mirrors `to_sendable_in`: mark doesn't survive the
                // boundary, identifiers degrade to plain symbols.
                let _ = mark;
                Value::Symbol(*name)
            } else {
                return Ok(None);
            }
        }
        Value::Pair(p) => {
            let car = recur!(&p.car.borrow());
            let cdr = recur!(&p.cdr.borrow());
            Value::Pair(Pair::new(car, cdr))
        }
        Value::Vector(items) => {
            let mut cloned = Vec::with_capacity(items.borrow().len());
            for e in items.borrow().iter() {
                cloned.push(recur!(e));
            }
            Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(cloned)))
        }
        Value::ByteVector(bv) => Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(
            bv.borrow().clone(),
        ))),
        Value::Procedure(_) => {
            return Err(
                "to_sendable: procedures cannot cross actor boundaries (also blocks procedures \
                 as `load-module!` exports — see ADR 0034 for the architectural prerequisite)"
                    .into(),
            )
        }
        Value::Hashtable(_) => {
            return Err(
                "to_sendable: hashtables are per-actor; use cs-table for shared state".into(),
            )
        }
        Value::Port(_) => return Err("to_sendable: ports cannot cross actor boundaries".into()),
        Value::Promise(_) => {
            return Err("to_sendable: promises cannot cross actor boundaries".into())
        }
    }))
}

/// Try the same-worker fast path for `(send pid v)`: if `target_worker` is
/// `Some` and matches the current thread's `LocalWorkerPool` worker id,
/// deep-clone `v` directly and reserve a per-receiver FIFO `seq`. Returns
/// `Ok(Some((recv, seq, cloned)))` — the caller sends the `Local { recv, seq }`
/// marker and then, *only on send success*, commits `cloned` into `recv`'s
/// side-queue via [`commit_same_worker_msg`] (so a failed send never parks an
/// orphan value). Returns `Ok(None)` whenever the fast path doesn't apply
/// (different/unknown worker, or a non-base symbol in the tree) so the caller
/// can fall back to `to_sendable_in` unchanged. `Err` propagates a genuine
/// not-sendable rejection (procedure/hashtable/port/promise).
fn try_send_same_worker(
    v: &Value,
    syms: &SymbolTable,
    recv: ActorPid,
    target_worker: Option<usize>,
) -> Result<Option<(ActorPid, u64, Value)>, String> {
    let same_worker = matches!(
        (target_worker, cs_actor::current_worker_id()),
        (Some(a), Some(b)) if a == b
    );
    if !same_worker {
        return Ok(None);
    }
    let Some(cloned) = deep_clone_same_worker(v, syms)? else {
        return Ok(None);
    };
    // Reserve the seq now so the marker and the (later) committed queue entry
    // agree. A reserved-but-never-committed seq (send failed) just leaves a
    // harmless gap; the pop side matches on the front entry, not on a dense id.
    let seq = SAME_WORKER_MSGS.with(|m| {
        let mut m = m.borrow_mut();
        let q = m.entry(recv).or_default();
        let seq = q.next_seq;
        q.next_seq += 1;
        seq
    });
    Ok(Some((recv, seq, cloned)))
}

/// Commit a deep-cloned fast-path message into receiver `recv`'s side-queue
/// after its `Local { recv, seq }` marker was successfully delivered. Bumps the
/// fast-send telemetry counter. Kept separate from [`try_send_same_worker`] so
/// the parking happens strictly *after* `primop_send` succeeds.
fn commit_same_worker_msg(recv: ActorPid, seq: u64, cloned: Value) {
    SAME_WORKER_MSGS.with(|m| {
        m.borrow_mut()
            .entry(recv)
            .or_default()
            .msgs
            .push_back((seq, cloned));
    });
    SAME_WORKER_FAST_SENDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn num_to_sendable(n: &Number) -> Result<SendableValue, String> {
    match n {
        Number::Fixnum(i) => Ok(SendableValue::Fixnum(*i)),
        Number::Flonum(f) => Ok(SendableValue::Flonum(*f)),
        Number::Big(b) => Ok(SendableValue::BigInt(b.to_str_radix(10))),
        Number::Rat(_) => Err("to_sendable: rationals not yet supported across actors".into()),
    }
}

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

/// Map a SendableValue to a cs-table Key. cs-table's Key enum
/// is intentionally narrow (Fixnum / String / Bytes); other
/// SendableValue variants don't have sensible Hash + Ord, so
/// they're rejected at the boundary.
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

//
// These functions are the Rust-callable shape of the BEAM
// primops. The Scheme-builtin wrappers further down convert
// Value <-> SendableValue at the boundary and call into these.

/// `(spawn 'name args...)` — look up the named entry in the
/// procedure registry and spawn an actor that runs it.
///
/// The spawned thread installs an [`ActorContext`] for its
/// lifetime so per-actor Scheme builtins (`(self)`,
/// `(raw-receive)`) can find the current [`cs_actor::Actor`]
/// without it being plumbed through every call.
pub fn primop_spawn(name: &str, args: Vec<SendableValue>) -> Result<ActorPid, String> {
    let st = beam_state();
    let entry = st
        .procs
        .lookup(name)
        .ok_or_else(|| format!("spawn: no procedure registered under {:?}", name))?;
    // parallel-runtime C1.2 + C3.2 (partial): use spawn_sync_
    // body_on_task so the actor runs as a tokio task. The full
    // C3.2 wiring of `REGION_STACK_TASK.scope(...)` is BLOCKED
    // on making `cs_gc::Region` Send — its Rc-internals can't
    // cross an `await` point on the multi_thread runtime. The
    // task-local infrastructure exists (C3.1's dual-stack) and
    // its tests pass synthetically, but the actor's body can't
    // .scope() over an Rc-bearing stack without Region: Send.
    //
    // Today's behavior: an actor's `(with-region …)` still
    // rides the TLS path. Works correctly as long as the body
    // doesn't yield between `enter` and `Drop` (the common
    // case — yield budget is 2000 ops, region scopes are
    // typically much shorter). Migration WITH an open region
    // scope is a documented limitation; filed as a separate
    // gap. The non-actor REPL path is unaffected.
    let actor_ref = st
        .actors
        .spawn_sync_body_on_task(move |actor| run_actor_body(actor, entry, args));
    Ok(actor_ref.pid())
}

/// Whether `(spawn-source …)` defaults to a dedicated thread rather than green.
/// Reads `CRABSCHEME_ACTOR_DEFAULT` once (`dedicated` → true; anything else /
/// unset → green, the default). The escape hatch makes the flip reversible
/// without a code change. Set the env before the first spawn (it is cached).
fn spawn_source_default_is_dedicated() -> bool {
    static DEDICATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEDICATED.get_or_init(|| {
        std::env::var("CRABSCHEME_ACTOR_DEFAULT")
            .map(|v| v.eq_ignore_ascii_case("dedicated"))
            .unwrap_or(false)
    })
}

/// `(spawn-source SOURCE ENTRY args...)` — spawn an actor whose body is a
/// *Scheme* procedure, not a pre-registered Rust closure.
///
/// **Green by default** (whole-body coroutine on the parking `LocalSet` pool —
/// parks on `(receive)`/`(sleep)`/`(tcp-recv)`, no thread-per-actor ceiling). Use
/// [`primop_spawn_source_dedicated`] (`spawn-source-dedicated`) for an actor that
/// must own an OS thread — one doing blocking work that has no cooperative
/// counterpart (a long blocking `fsync`, a sole-drainer poll loop). Override the
/// default globally with `CRABSCHEME_ACTOR_DEFAULT=dedicated`.
pub fn primop_spawn_source(
    source: String,
    entry: String,
    args: Vec<SendableValue>,
) -> Result<ActorPid, String> {
    if spawn_source_default_is_dedicated() {
        primop_spawn_source_dedicated(source, entry, args)
    } else {
        primop_spawn_source_green(source, entry, args)
    }
}

/// Dedicated-thread `spawn-source`: the body runs via `block_in_place` on its own
/// worker thread (the original `spawn-source` semantics, now opt-in).
///
/// Why source + a name instead of a closure? A Scheme `Value::Procedure` is
/// `Rc`-based and therefore `!Send`; a `block_in_place` body must be
/// `Send + 'static` (it runs on a *different* thread than the spawner). So a
/// closure can't cross the boundary. Instead we capture only `String`s (Send),
/// and on the actor's own worker thread build a fresh per-actor
/// [`crate::Runtime`], load `source` (every `Rc` Value stays thread-local),
/// resolve `entry`, and apply it. `args` cross as data (`SendableValue`), exactly
/// like mailbox messages.
pub fn primop_spawn_source_dedicated(
    source: String,
    entry: String,
    args: Vec<SendableValue>,
) -> Result<ActorPid, String> {
    let st = beam_state();
    let body = scheme_source_entry(source, entry);
    let actor_ref = st
        .actors
        .spawn_sync_body_on_task(move |actor| run_actor_body(actor, body, args));
    Ok(actor_ref.pid())
}

/// `(spawn-source-green SOURCE ENTRY args...)` — like [`primop_spawn_source`]
/// but runs the body on the parking `LocalSet` worker pool (green) instead of a
/// dedicated `block_in_place` thread. The actor **parks** (releases its worker)
/// on `(receive)` / `(raw-receive)` / `(sleep)`, and many such actors multiplex
/// onto each worker — no thread-per-actor `max_blocking_threads` ceiling.
///
/// Unlike `spawn-activation` (framework-driven, one `(handler msg)` call per
/// message), this runs an arbitrary free-form body with its own receive loop —
/// the shape every `spawn-source` actor (and all of crab-cache) uses — via
/// [`green_source_body`].
///
/// M1: an explicit opt-in sibling of `spawn-source` (default stays dedicated;
/// INV-1 — cooperative async TCP must land before the default can flip).
pub fn primop_spawn_source_green(
    source: String,
    entry: String,
    args: Vec<SendableValue>,
) -> Result<ActorPid, String> {
    let st = beam_state();
    let actor_ref = st
        .actors
        .spawn_local_activation(move |actor| green_source_body(actor, source, entry, args));
    Ok(actor_ref.pid())
}

/// `(spawn-activation SOURCE HANDLER)` — spawn a Scheme actor on the
/// `LocalSet` worker pool (#30 iter-2a / ADR 0032) so it **parks** (releases
/// its worker) while waiting on an empty mailbox instead of pinning an OS
/// thread. This is what breaks the `max_blocking_threads(4096)` ceiling for
/// mailbox-bound actors that `spawn-source` (blocking `(receive)` loop +
/// `block_in_place`) is bound by.
///
/// `SOURCE` is loaded into a fresh per-actor [`crate::Runtime`] on the worker
/// thread (every `Rc` Value stays thread-local, exactly like `spawn-source`).
/// `HANDLER` names a top-level unary procedure `(handler msg) -> continue?`
/// that the framework calls once per delivered message; it returns `#f` to
/// stop the actor, any other value to continue. Per-actor state lives in the
/// handler's own (mutable) top-level bindings — the Runtime persists on the
/// worker across activations, so state survives the parking await.
///
/// The "top-level loop receive parks; mid-stack `(raw-receive)` blocks" seam
/// of ADR 0032 falls out for free: the **framework** owns the parking
/// `receive_async().await`, while a `(raw-receive)` *inside* a handler still
/// blocks via `ACTOR_CTX` (the existing primop, unchanged).
pub fn primop_spawn_activation(source: String, handler: String) -> Result<ActorPid, String> {
    let st = beam_state();
    let actor_ref = st
        .actors
        .spawn_local_activation(move |actor| activation_body(actor, source, handler));
    Ok(actor_ref.pid())
}

/// The framework-driven activation loop, run on a `LocalSet` worker thread.
/// Builds the per-actor Runtime once, resolves the handler, then loops:
/// park on `receive_async().await`, decode the message to a `Value`, and
/// invoke the handler with `ACTOR_CTX` pointing at this actor for the
/// duration of that synchronous call only.
async fn activation_body(mut actor: cs_actor::Actor, source: String, handler: String) {
    // cs-845.1: free any same-worker fast-path mail still queued for this
    // actor on every exit path (return / error / panic-unwind / shutdown drop).
    let _sw_guard = SameWorkerGuard(actor.pid());
    // cs-tds: see the matching comment in `green_source_body` — this
    // future is pinned to one actor-dedicated `LocalWorkerPool` worker
    // thread for its whole life, so disabling JIT tiering here needs no
    // restore.
    cs_vm::vm::set_jit_enabled(false);
    // Shared-Runtime: overlay this worker's shared base (builtins + bundled libs)
    // instead of a full Runtime::new() per actor (same lever as green_source_body).
    let mut rt = crate::Runtime::from_image(&worker_runtime_image());
    // VM bytecode tier for the handler (see run_scheme_body re: no JIT). The
    // handler is loaded as a VM closure; `apply_value` (below, per message)
    // delegates VM closures to the VM caller, so each invocation runs on the VM.
    // Cached per source per worker — activation actors sharing a handler body
    // reuse the compiled bytecode (closures share the cached code chunks).
    if let Err(d) = rt.eval_str_via_vm_cached("<spawn-activation>", &source) {
        eprintln!("spawn-activation: loading actor source failed: {d:?}");
        return;
    }
    let handler_proc = match rt.lookup(&handler) {
        Some(v @ Value::Procedure(_)) => v,
        Some(other) => {
            eprintln!(
                "spawn-activation: top-level `{handler}` is {}, not a procedure",
                other.type_name()
            );
            return;
        }
        None => {
            eprintln!("spawn-activation: no top-level `{handler}` defined in the source");
            return;
        }
    };
    while let Some(msg) = actor.receive_async().await {
        let sv = message_to_sendable(msg);
        let msg_val = rt.sendable_to_value(&sv);
        // Run the handler on a stackful coroutine so a `(sleep)` inside it
        // suspends the synchronous evaluator and the driver parks on a tokio
        // timer — co-located actors run meanwhile — instead of blocking the
        // worker. `drive_handler` installs/clears ACTOR_CTX + the reduction
        // budget around every resume, so actors sharing a worker thread never
        // see each other's context (the guarantee the old per-call
        // `ActivationCtx` gave, now spanning the suspend points too).
        let outcome = drive_handler(&mut actor, &mut rt, &handler_proc, msg_val).await;
        match outcome {
            Ok(Value::Boolean(false)) => break, // handler asked to stop
            Ok(_) => {}
            Err(e) => {
                eprintln!("spawn-activation: handler `{handler}` raised: {e}");
                break;
            }
        }
    }
}

/// Drive one activation-handler invocation on a stackful coroutine, so a
/// `(sleep)`/`(sleep-ms)` deep inside the *synchronous* handler can suspend the
/// whole evaluator and hand control back here. We then park on a tokio timer —
/// releasing the LocalSet worker so co-located actors run — and resume the
/// handler exactly where it slept. Handlers that never sleep just run to
/// `Return` on the first resume (one stack switch in + out).
///
/// ## Safety / aliasing
///
/// The coroutine closure must be `'static`, so it captures `rt`/`actor` as raw
/// pointers (the exact discipline of [`ACTOR_CTX`] / `with_current_actor`). This
/// is sound for the same reason: one worker thread, one handler at a time, and
/// control *strictly alternates* — while the coroutine runs, this driver is
/// parked inside `resume()` (it never touches `rt`/`actor`); while the coroutine
/// is suspended at the `.await`, its frame is frozen and this driver only awaits
/// a timer. So the pointers are never dereferenced concurrently and never cross
/// a thread. `rt`/`actor` are borrowed from `activation_body` for strictly
/// longer than every coroutine we build here.
///
/// ## Drop order
///
/// `co` is built here and moved into `pump_coroutine`, whose frame is awaited
/// from this one — so `co` stays deeper than `rt`/`actor` in `activation_body`.
/// On shutdown, dropping the suspended `activation_body` future drops
/// innermost-first: `co` (corosensei force-unwinds its frozen Scheme stack,
/// running `Rc` destructors that touch `rt`'s heap) drops *before* `rt`/`actor`.
/// Do not hoist `co` or the stack pool above `rt`/`actor`.
async fn drive_handler(
    actor: &mut cs_actor::Actor,
    rt: &mut crate::Runtime,
    handler: &Value,
    msg: Value,
) -> Result<Value, String> {
    let rt_ptr: *mut crate::Runtime = rt;
    let actor_ptr: *mut cs_actor::Actor = actor;
    let handler = handler.clone();

    let co: Coroutine<CoResume, CoYield, Result<Value, String>, DefaultStack> =
        Coroutine::with_stack(
            checkout_stack(StackClass::Activation),
            move |yielder, _first: CoResume| {
                // Publish the yielder FIRST, before the handler can suspend: on the
                // first resume the driver can't pre-install it (the closure hasn't
                // run yet), so the closure seeds it here.
                YIELDER.with(|y| y.set(yielder as *const _));
                // Sound: see fn doc — the driver is parked in `resume()` right now.
                let rt = unsafe { &mut *rt_ptr };
                rt.apply_value(&handler, &[msg])
            },
        );

    // The suspend/resume loop is shared with the whole-body green driver
    // (`green_source_body`) — the only difference is what the closure above runs.
    pump_coroutine(co, actor_ptr, StackClass::Activation).await
}

/// Drive a coroutine-hosted Scheme computation to completion, servicing each
/// cooperative suspend by parking on the matching async primitive (releasing the
/// LocalSet worker so co-located actors run) and resuming the coroutine with the
/// result. Shared by the per-message [`drive_handler`] (one handler invocation)
/// and the whole-body `green_source_body` (an entire free-form actor body) — the
/// machinery is identical; only the coroutine closure differs.
///
/// Takes `co` by value: on `Return` it reclaims the stack via `into_stack`, and
/// keeping `co` in this (innermost) frame preserves the drop-order guarantee its
/// callers rely on — see [`drive_handler`]'s safety doc.
///
/// ## Safety
///
/// `actor_ptr` must point at the actor that owns `co` and outlive it. Sound for
/// the same reason as elsewhere: one single-threaded worker; control strictly
/// alternates (this driver is parked in `resume()` while the coroutine runs, and
/// the coroutine is frozen while this driver `.await`s); and `ACTOR_CTX` /
/// `YIELDER` are cleared before every `.await`, so a co-located actor never
/// observes our stale pointers.
async fn pump_coroutine(
    mut co: Coroutine<CoResume, CoYield, Result<Value, String>, DefaultStack>,
    actor_ptr: *mut cs_actor::Actor,
    stack_class: StackClass,
) -> Result<Value, String> {
    // Enable reduction-budget preemption on this worker: when the cs-vm yield
    // hook fires mid-evaluation it suspends the running coroutine (CoYield::Yield),
    // so a CPU-bound body releases its shared worker instead of monopolizing it.
    // Installed once per worker thread (idempotent); LocalSet workers are
    // green-dedicated and the hook is a no-op whenever no coroutine is active
    // (YIELDER null), so it needs no teardown. See [`green_yield_hook`].
    ensure_green_yield_hook();
    // Clear ACTOR_CTX / YIELDER on *every* exit from this driver — including a
    // panic unwinding out of `co.resume` below (corosensei propagates a coroutine
    // panic through `resume`, which then unwinds this frame past the inline
    // clear). Without this, a panicking actor would leave its (freed) pointers
    // installed for the next actor to run on this shared worker. Mirrors the RAII
    // Guard `run_actor_body` uses on the dedicated path (P6.1 parity).
    struct ClearCtx;
    impl Drop for ClearCtx {
        fn drop(&mut self) {
            ACTOR_CTX.with(|c| c.set(std::ptr::null_mut()));
            YIELDER.with(|y| y.set(std::ptr::null()));
        }
    }
    let _clear_ctx = ClearCtx;
    // Region-park guard baseline (P0.1): the depth of the shared TLS region stack
    // when this body/handler started. Every suspend must return to this depth —
    // see the check after each resume below.
    #[cfg(feature = "regions")]
    let region_entry_depth = crate::regions::region_stack_depth();
    // The yielder pointer is stable for the coroutine's life; cache it after
    // the first resume so we can re-publish it before every later resume (a
    // co-located actor that ran during our suspend clobbered the thread-local).
    let mut cached_yielder: *const Yielder<CoResume, CoYield> = std::ptr::null();
    // The value handed to the next `resume`: ignored on the first resume; `Woke`
    // after a sleep; the mailbox result after a receive.
    let mut resume_input = CoResume::Woke;

    loop {
        // Install OUR actor's context for the duration of this resume only.
        ACTOR_CTX.with(|c| c.set(actor_ptr));
        YIELDER.with(|y| y.set(cached_yielder));
        REDUCTIONS.with(|c| c.set(0));

        let result = co.resume(resume_input);

        // Capture the yielder the closure published on the first resume.
        if cached_yielder.is_null() {
            cached_yielder = YIELDER.with(|y| y.get());
        }
        // Clear context BEFORE any await: while we're parked, the LocalSet may
        // run a co-located actor, and it must never observe our stale pointers.
        ACTOR_CTX.with(|c| c.set(std::ptr::null_mut()));
        YIELDER.with(|y| y.set(std::ptr::null()));

        // Region-park guard (P0.1): the TLS region stack is shared by every actor
        // co-located on this worker, so suspending (any Yield arm awaits) with an
        // extra `(with-region)` scope still open would interleave with a peer's
        // regions and corrupt the stack. Refuse it loudly → ExitReason::Error via
        // the wrapper; `co`'s RegionScope drops run as it unwinds, and ClearCtx
        // restores the thread-locals. (Save/restore-around-suspend is the proper
        // fix and a tracked follow-up; it needs the blocked region-as-task-local
        // work — see primop_spawn's note. crab-cache's green actors hold no region
        // across a receive, so this only guards genuine misuse.)
        #[cfg(feature = "regions")]
        if matches!(result, CoroutineResult::Yield(_)) {
            let depth = crate::regions::region_stack_depth();
            assert!(
                depth == region_entry_depth,
                "cannot park inside (with-region): a region scope ({depth} deep, entry \
                 {region_entry_depth}) would span a suspend on a shared worker"
            );
        }

        match result {
            CoroutineResult::Yield(CoYield::Sleep(dur)) => {
                tokio::time::sleep(dur).await;
                resume_input = CoResume::Woke;
            }
            CoroutineResult::Yield(CoYield::Recv { timeout }) => {
                // Park on the async mailbox (releasing the worker) and process
                // the message exactly as the blocking path would, then resume
                // the handler with the result. `actor_ptr` is sound here for the
                // same reason as elsewhere: the coroutine is suspended (not
                // touching the actor) while we await, ACTOR_CTX is cleared, and
                // this driver is single-threaded.
                let act = unsafe { &mut *actor_ptr };
                resume_input = CoResume::Received(driver_receive(act, timeout).await);
            }
            CoroutineResult::Yield(CoYield::Yield) => {
                // Reduction-budget preemption: hand the worker back to the
                // scheduler for one tick so a co-located actor runs, then resume
                // the body exactly where the hook fired.
                tokio::task::yield_now().await;
                resume_input = CoResume::Woke;
            }
            #[cfg(feature = "stdlib-net")]
            CoroutineResult::Yield(CoYield::Io { handle, max }) => {
                // Cooperative socket read: await on the async stream (releasing
                // the worker) instead of blocking it, then resume with the bytes.
                resume_input = CoResume::Io(driver_tcp_recv(handle, max).await);
            }
            #[cfg(feature = "stdlib-net")]
            CoroutineResult::Yield(CoYield::IoWrite { handle, bytes }) => {
                resume_input = CoResume::IoWrite(driver_tcp_send(handle, bytes).await);
            }
            CoroutineResult::Return(outcome) => {
                // `into_stack` asserts the coroutine is done — only valid here.
                checkin_stack(co.into_stack(), stack_class);
                return outcome;
            }
        }
    }
}

std::thread_local! {
    /// This LocalSet worker's shared [`crate::RuntimeImage`] — the immutable
    /// builtins/bundled-libs base that every green actor on this worker overlays
    /// via [`crate::Runtime::from_image`]. Built once on first green spawn here;
    /// `Rc`-based + thread-local, so it never crosses a thread (same isolation as
    /// the per-actor Runtimes). This is the shared-Runtime memory lever.
    static WORKER_RUNTIME_IMAGE: std::cell::OnceCell<std::rc::Rc<crate::RuntimeImage>> =
        const { std::cell::OnceCell::new() };
}

/// This worker's shared `RuntimeImage`, building it on first use.
fn worker_runtime_image() -> std::rc::Rc<crate::RuntimeImage> {
    WORKER_RUNTIME_IMAGE.with(|c| {
        c.get_or_init(|| std::rc::Rc::new(crate::RuntimeImage::build()))
            .clone()
    })
}

/// Whole-body green actor driver — the free-form analog of [`activation_body`]
/// and the green analog of [`run_scheme_body`]. Runs an entire `spawn-source`
/// body (with its *own* `(receive)`/`(raw-receive)`/`(sleep)` loop) inside a
/// stackful coroutine on a `LocalSet` worker, so every cooperative suspend point
/// the body hits **parks** (releasing the worker for co-located actors) instead
/// of blocking the worker thread.
///
/// No body or receive/sleep-primop changes are needed: the body's own
/// `(raw-receive)` / `(sleep)` already route through the `YIELDER`-gated
/// cooperative hooks ([`cooperative_raw_receive`] / [`cooperative_sleep_hook`]),
/// and this driver — via [`pump_coroutine`] — publishes that `YIELDER`. The
/// difference from [`drive_handler`] is only *what the coroutine runs*: a whole
/// body that loops until the actor exits, rather than one handler invocation.
///
/// ## Drop order
///
/// `rt`/`actor` live in this frame; `co` is built here and moved into
/// [`pump_coroutine`], whose frame is awaited from this one — so `co` stays
/// innermost and force-unwinds (running `Rc` dtors that touch `rt`'s heap)
/// *before* `rt`/`actor` drop on teardown. Do not hoist `co` above `rt`/`actor`.
/// See [`drive_handler`]'s safety doc for the full single-thread aliasing
/// argument (identical here — one worker, strictly-alternating control).
async fn green_source_body(
    mut actor: cs_actor::Actor,
    source: String,
    entry: String,
    args: Vec<SendableValue>,
) {
    // cs-845.1: free any same-worker fast-path mail still queued for this
    // actor on every exit path (return / error / panic-unwind / shutdown drop).
    let _sw_guard = SameWorkerGuard(actor.pid());
    // cs-tds: this whole future lives on one `LocalWorkerPool` worker
    // thread for its entire life (tokio pins `spawn_local` tasks to the
    // thread that owns their `LocalSet`), and that pool is dedicated to
    // actor bodies — so disabling JIT tiering here is a once-per-task,
    // no-restore-needed equivalent of disabling it once at worker-thread
    // startup. See `cs_vm::vm::set_jit_enabled` doc.
    cs_vm::vm::set_jit_enabled(false);
    // Shared-Runtime: a cheap per-actor Runtime overlaying this worker's shared
    // base (builtins + bundled libs), instead of a full `Runtime::new()` per
    // actor. The body's defines land in the per-actor overlay env; builtins /
    // libraries resolve through to the shared base. This is the green-threads
    // memory lever (N actors → one base + N small overlays).
    let mut rt = crate::Runtime::from_image(&worker_runtime_image());
    // Load on the VM tier in the driver frame (no YIELDER installed yet:
    // top-level `(define …)`s don't park). Cached per source per worker — actors
    // running the same body reuse the compiled bytecode (sharing its code
    // chunks); only the closures + overlay bindings are per-actor. Mirrors
    // `run_scheme_body` otherwise.
    if let Err(d) = rt.eval_str_via_vm_cached("<spawn-source-green>", &source) {
        eprintln!("spawn-source(green): loading actor source failed: {d:?}");
        return;
    }
    let call = match resolve_and_build_call(&rt, &entry, &args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("spawn-source(green): {e}");
            return;
        }
    };

    let rt_ptr: *mut crate::Runtime = &mut rt;
    let actor_ptr: *mut cs_actor::Actor = &mut actor;
    // `co` declared after `rt`/`actor` (drop-order — see fn doc). The closure
    // seeds the yielder, then runs the WHOLE body; its (raw-receive)/(sleep)
    // suspend back to `pump_coroutine`, which lives until the body returns.
    let co: Coroutine<CoResume, CoYield, Result<Value, String>, DefaultStack> =
        Coroutine::with_stack(
            checkout_stack(StackClass::Green),
            move |yielder, _first: CoResume| {
                YIELDER.with(|y| y.set(yielder as *const _));
                // Sound: the driver is parked in `resume()` right now (see fn doc).
                let rt = unsafe { &mut *rt_ptr };
                rt.eval_str_via_vm("<spawn-source-green-body>", &call)
                    .map_err(|d| format!("actor body raised: {d:?}"))
            },
        );

    // Termination parity with the dedicated path (`scheme_source_entry`) and the
    // activation path (`activation_body`): a *Scheme-level* error (the body
    // returned `Err` — including a trap-exit `Err` from `(raw-receive)`) is
    // surfaced loudly and the actor exits cleanly → `ExitReason::Normal` via
    // `spawn_local_activation`'s wrapper. A *Rust panic* propagates out of this
    // future and that wrapper's `catch_unwind` maps it to `ExitReason::Error`,
    // which `on_actor_termination` then chains to linked actors / monitors —
    // identical to the dedicated `spawn_async` path. (No Scheme primitive exits
    // with a custom Error reason; abnormal exits come only from panics.) The
    // `pump_coroutine` ClearCtx guard keeps that panic path hygienic.
    if let Err(e) = pump_coroutine(co, actor_ptr, StackClass::Green).await {
        eprintln!("spawn-source(green): actor `{entry}` terminated: {e}");
    }
}

/// The async side of a cooperative `(raw-receive)`: pop a message from the
/// mailbox — parking on the async mailbox (cancel-safe) so co-located actors run
/// — then apply the same trap-exit + system-message processing as the blocking
/// [`primop_raw_receive`]. `timeout`: `None` = block, `Some(0)` = try-once,
/// `Some(ms)` = wait up to `ms` then report a timeout.
async fn driver_receive(
    actor: &mut cs_actor::Actor,
    timeout: Option<u64>,
) -> Result<Option<SendableValue>, String> {
    let msg = match timeout {
        None => match actor.receive_async().await {
            Some(m) => m,
            None => return Err("raw-receive: mailbox closed".into()),
        },
        Some(0) => match actor.try_receive() {
            Ok(m) => m,
            Err(cs_actor::TryRecvError::Empty) => return Ok(None),
            Err(cs_actor::TryRecvError::Disconnected) => {
                return Err("raw-receive: mailbox closed".into())
            }
        },
        Some(ms) => {
            // `receive_async` is cancel-safe (cs-actor issue #60), so dropping it
            // when the timer fires neither loses a message nor wedges the mailbox.
            match tokio::time::timeout(Duration::from_millis(ms), actor.receive_async()).await {
                Ok(Some(m)) => m,
                Ok(None) => return Err("raw-receive: mailbox closed".into()),
                Err(_elapsed) => return Ok(None),
            }
        }
    };
    process_received(actor, msg)
}

#[cfg(feature = "stdlib-net")]
std::thread_local! {
    /// Per-worker cache of tokio streams for cooperative socket I/O, keyed by the
    /// cs-stdlib-net handle id. A long-lived green conn reuses its stream across
    /// reads/writes rather than re-cloning the fd + re-registering with the
    /// reactor each call. Evicted on EOF / error (the next op rebuilds from the
    /// std stream cs-stdlib-net still holds).
    static TOKIO_TCP: std::cell::RefCell<std::collections::HashMap<i64, tokio::net::TcpStream>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Take this handle's cached tokio stream, or build one from the std stream
/// cs-stdlib-net holds (dup'd, set non-blocking, adopted onto the current
/// reactor). The caller reinserts it on success; on EOF/error it stays evicted.
#[cfg(feature = "stdlib-net")]
fn take_tokio_tcp(handle: i64) -> Result<tokio::net::TcpStream, String> {
    if let Some(s) = TOKIO_TCP.with(|m| m.borrow_mut().remove(&handle)) {
        return Ok(s);
    }
    let std_stream = cs_stdlib_net::clone_tcp_std(handle)
        .ok_or_else(|| format!("handle {handle} is not a live TCP socket"))?;
    std_stream
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking failed: {e}"))?;
    tokio::net::TcpStream::from_std(std_stream).map_err(|e| format!("tokio adopt failed: {e}"))
}

/// Driver side of a cooperative `(tcp-recv handle max)`: read up to `max` bytes,
/// awaiting (releasing the worker) instead of blocking. `Ok(empty)` on a clean
/// EOF — matching the blocking path. The conn actor owns its socket and is
/// suspended here, so no peer touches this handle's cache entry meanwhile.
#[cfg(feature = "stdlib-net")]
async fn driver_tcp_recv(handle: i64, max: usize) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let mut stream = take_tokio_tcp(handle).map_err(|e| format!("tcp-recv: {e}"))?;
    let mut buf = vec![0u8; max];
    match stream.read(&mut buf).await {
        Ok(0) => Ok(Vec::new()), // clean EOF — leave the stream evicted
        Ok(n) => {
            buf.truncate(n);
            TOKIO_TCP.with(|m| m.borrow_mut().insert(handle, stream));
            Ok(buf)
        }
        Err(e) => Err(format!("tcp-recv: {e}")), // evict on error; next op rebuilds
    }
}

/// Driver side of a cooperative `(tcp-send handle bytes)`: write all of `bytes`,
/// awaiting instead of blocking. Required for green conns: the cooperative recv
/// put the shared fd in non-blocking mode, so the blocking `write_all` path
/// would hit `WouldBlock`.
#[cfg(feature = "stdlib-net")]
async fn driver_tcp_send(handle: i64, bytes: Vec<u8>) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut stream = take_tokio_tcp(handle).map_err(|e| format!("tcp-send: {e}"))?;
    let result = async {
        stream
            .write_all(&bytes)
            .await
            .map_err(|e| format!("tcp-send: {e}"))?;
        stream.flush().await.map_err(|e| format!("tcp-send: {e}"))?;
        Ok(())
    }
    .await;
    if result.is_ok() {
        TOKIO_TCP.with(|m| m.borrow_mut().insert(handle, stream));
    }
    result
}

/// Build a `Send + Sync` [`ActorEntry`] that runs a Scheme body. The closure
/// captures only `String`s — never an `Rc`-based Scheme value — so it crosses
/// onto the actor's worker thread cleanly. The `Rc` graph is created *inside*
/// the closure, on that thread, and never escapes it.
fn scheme_source_entry(source: String, entry: String) -> ActorEntry {
    Arc::new(move |_actor, args| {
        if let Err(e) = run_scheme_body(&source, &entry, &args) {
            // The actor terminates (the body returned/aborted). Surface the
            // reason loudly rather than dying silently — Article VI.
            eprintln!("spawn-source: actor `{entry}` terminated: {e}");
        }
    })
}

/// On the actor's worker thread: stand up a fresh Runtime, load the body
/// source, resolve `entry` to a procedure, and call it with `args`. The
/// `(self)` / `(raw-receive)` / `(send …)` builtins the body uses reach this
/// actor through `ACTOR_CTX`, which [`run_actor_body`] installed before
/// invoking us.
fn run_scheme_body(source: &str, entry: &str, args: &[SendableValue]) -> Result<(), String> {
    let mut rt = crate::Runtime::new();
    // Run the actor body on the VM bytecode tier, not the walker — the VM is
    // ~2.8× faster than the tree-walker on this kind of code (RESP parse,
    // command dispatch, store ops, Raft logic). JIT is intentionally NOT
    // installed: a microbench showed it adds only ~5% over the VM tier, and it
    // hung the cache's concurrent write path (Raft propose/apply) under load
    // (`-c 50` SET); VM-only resolves that and is faster across the board.
    rt.eval_str_via_vm("<spawn-source>", source)
        .map_err(|d| format!("loading actor source failed: {d:?}"))?;
    let call = resolve_and_build_call(&rt, entry, args)?;
    rt.eval_str_via_vm("<spawn-call>", &call)
        .map_err(|d| format!("actor body raised: {d:?}"))?;
    Ok(())
}

/// Resolve `entry` to a top-level procedure in `rt` and render the call
/// expression `(entry 'a0 'a1 ...)` that runs it. Shared by the dedicated
/// [`run_scheme_body`] and the green [`green_source_body`].
///
/// Each argument is rendered as a quoted external datum, so this needs only the
/// public eval/lookup surface (no mutable `SymbolTable` access) and reuses the
/// reader's own interning — symbols/strings/lists round-trip exactly. Errors if
/// `entry` is missing or is bound to a non-procedure.
fn resolve_and_build_call(
    rt: &crate::Runtime,
    entry: &str,
    args: &[SendableValue],
) -> Result<String, String> {
    match rt.lookup(entry) {
        Some(Value::Procedure(_)) => {}
        Some(other) => {
            return Err(format!(
                "top-level `{entry}` is {}, not a procedure",
                other.type_name()
            ));
        }
        None => return Err(format!("no top-level `{entry}` defined in the source")),
    }
    build_call_expr(entry, args)
}

/// Render `(entry 'arg0 'arg1 ...)`. Each argument is `quote`d so that a
/// symbol stays a symbol and a list stays a list (rather than being read as a
/// call). Nested data inside an argument needs no further quoting — it is
/// already under the outer `quote`.
fn build_call_expr(entry: &str, args: &[SendableValue]) -> Result<String, String> {
    let mut s = String::from("(");
    s.push_str(entry);
    for a in args {
        s.push_str(" '");
        sendable_to_datum(a, &mut s)?;
    }
    s.push(')');
    Ok(s)
}

/// Write a [`SendableValue`] as its Scheme external (read) representation.
/// The result is a *datum* (no leading quote); callers that need it to
/// self-evaluate add the `quote`. Matches the reader so a round-trip through
/// `eval_str` reconstructs the same value.
fn sendable_to_datum(s: &SendableValue, out: &mut String) -> Result<(), String> {
    match s {
        SendableValue::Null => out.push_str("()"),
        SendableValue::Boolean(b) => out.push_str(if *b { "#t" } else { "#f" }),
        SendableValue::Fixnum(n) => out.push_str(&n.to_string()),
        SendableValue::Flonum(f) => out.push_str(&flonum_datum(*f)),
        SendableValue::BigInt(d) => out.push_str(d), // decimal string reads as a bignum
        SendableValue::Character(c) => out.push_str(&char_datum(*c)),
        SendableValue::String(st) => string_datum(st, out),
        // Inside the outer quote this becomes a symbol again.
        SendableValue::Symbol(name) => out.push_str(name),
        SendableValue::Pair(car, cdr) => {
            out.push('(');
            sendable_to_datum(car, out)?;
            let mut rest = cdr.as_ref();
            loop {
                match rest {
                    SendableValue::Null => break,
                    SendableValue::Pair(h, t) => {
                        out.push(' ');
                        sendable_to_datum(h, out)?;
                        rest = t.as_ref();
                    }
                    other => {
                        out.push_str(" . ");
                        sendable_to_datum(other, out)?;
                        break;
                    }
                }
            }
            out.push(')');
        }
        SendableValue::Vector(items) => {
            out.push_str("#(");
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                sendable_to_datum(it, out)?;
            }
            out.push(')');
        }
        SendableValue::ByteVector(bytes) => {
            out.push_str("#vu8(");
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(&b.to_string());
            }
            out.push(')');
        }
        // A PID passed as a *spawn argument* is uncommon (peers normally
        // arrive via messages, which use `from_sendable` directly). Reject it
        // rather than emit a token whose symbol-readability we can't depend
        // on — keeps the bridge honest about what it guarantees.
        SendableValue::Pid(_) => {
            return Err(
                "a PID cannot be a spawn-source argument; send it as a message instead".into(),
            );
        }
        SendableValue::Unspecified => {
            return Err("the unspecified value cannot be a spawn-source argument".into());
        }
        // cs-845.1: `Local` is an internal same-worker-mailbox handle,
        // produced only by `try_send_same_worker` and consumed only by
        // `from_sendable`. It should never reach datum serialization
        // (spawn-source args always go through `to_sendable_in`, not the
        // fast-send path).
        SendableValue::Local { .. } => {
            return Err(
                "internal error: a same-worker message handle escaped into datum serialization"
                    .into(),
            );
        }
        SendableValue::Eof => {
            return Err("the eof object cannot be a spawn-source argument".into());
        }
    }
    Ok(())
}

/// A flonum in a form the reader takes as a flonum (never as a fixnum):
/// always carries a `.`/exponent, with the R6RS spellings for the specials.
fn flonum_datum(f: f64) -> String {
    if f.is_nan() {
        return "+nan.0".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            "+inf.0".into()
        } else {
            "-inf.0".into()
        };
    }
    let s = format!("{f}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

/// A character literal (`#\a`, with names for the unprintables).
fn char_datum(c: char) -> String {
    match c {
        ' ' => "#\\space".into(),
        '\n' => "#\\newline".into(),
        '\t' => "#\\tab".into(),
        '\r' => "#\\return".into(),
        '\0' => "#\\nul".into(),
        other => format!("#\\{other}"),
    }
}

/// A string literal with the minimal escaping the reader needs.
fn string_datum(st: &str, out: &mut String) {
    out.push('"');
    for ch in st.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Sync inner of the actor body: installs the yield hook,
/// ACTOR_CTX, REDUCTIONS counter, runs `entry`, cleans up via
/// RAII Guard. Factored out so the spawn_async closure shape
/// stays readable.
fn run_actor_body(actor: &mut cs_actor::Actor, entry: ActorEntry, args: Vec<SendableValue>) {
    // Hold a raw pointer to `actor` for the body's duration so
    // the (self) / (raw-receive) Scheme builtins can reach it
    // via ACTOR_CTX. Safety: this pointer lives only on the
    // worker thread inside block_in_place; the Guard clears it
    // before the closure returns or unwinds.
    let ptr: *mut cs_actor::Actor = actor;
    ACTOR_CTX.with(|c| c.set(ptr));
    REDUCTIONS.with(|c| c.set(0));
    // parallel-runtime C2.2: install the reduction-yield hook
    // for this worker thread. cs-actor::tokio_yield_hook
    // routes through tokio::task::yield_now so CPU-bound actor
    // bodies release their worker cooperatively. Non-actor
    // contexts never reach here, so the hook stays None and
    // the dispatch loop's per-op tick is a pure counter no-op.
    let prev_hook = cs_vm::vm::install_yield_hook(Some(cs_actor::tokio_yield_hook));
    // cs-tds: actor bodies run VM-only — the JIT is deliberately dropped
    // for actors (perf/actor-vm-jit found it hung concurrent SET, with
    // only marginal gain elsewhere). Clearing this per-invocation lets
    // the Call/TailCall dispatch loop skip the tier-bump/jit_ptr checks
    // that only matter for JIT tiering; a pooled worker thread reused by
    // a non-actor caller has it restored to `true` by the Guard below.
    let prev_jit_enabled = cs_vm::vm::jit_enabled();
    cs_vm::vm::set_jit_enabled(false);
    // parallel-runtime C4.5: bridge the BR sweep's yield
    // hook to cs-vm's reduction counter. The same
    // per-iteration tick the bytecode dispatch loop uses
    // (cs_vm::vm::vm_tick_reductions) now also fires once
    // per candidate processed by cs_gc::cycle_collector,
    // so a long sweep on this tokio worker yields when the
    // reduction budget exhausts. Non-actor contexts (REPL,
    // crabscheme run) leave the hook unset → sweep runs at
    // full speed, same as pre-C4.5.
    #[cfg(feature = "tracing-cycle-collector")]
    let prev_sweep_hook =
        cs_gc::cycle_collector::install_sweep_yield_hook(Some(cs_vm::vm::vm_tick_reductions));
    struct Guard {
        prev_hook: Option<cs_vm::vm::VmYieldHook>,
        prev_jit_enabled: bool,
        #[cfg(feature = "tracing-cycle-collector")]
        prev_sweep_hook: Option<cs_gc::cycle_collector::SweepYieldHook>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            ACTOR_CTX.with(|c| c.set(std::ptr::null_mut()));
            REDUCTIONS.with(|c| c.set(0));
            // Restore the previous hook (typically None) so a
            // pooled worker thread reused by a non-actor caller
            // doesn't see our hook.
            cs_vm::vm::install_yield_hook(self.prev_hook);
            cs_vm::vm::set_jit_enabled(self.prev_jit_enabled);
            #[cfg(feature = "tracing-cycle-collector")]
            cs_gc::cycle_collector::install_sweep_yield_hook(self.prev_sweep_hook);
        }
    }
    let _g = Guard {
        prev_hook,
        prev_jit_enabled,
        #[cfg(feature = "tracing-cycle-collector")]
        prev_sweep_hook,
    };
    entry(actor, args);
}

// ActorContext: thread-local pointer to the currently-running
// Actor so per-actor Scheme builtins ((self), (raw-receive))
// can find their context without it being threaded through
// every builtin signature.
std::thread_local! {
    static ACTOR_CTX: std::cell::Cell<*mut cs_actor::Actor> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };

    /// Per-actor reduction counter (Erlang's "reductions" — a
    /// proxy for work done). Bumped by `(bump-reductions! N)`
    /// from Scheme; reset by `(yield)` after the cooperative
    /// hand-off. Read with `(reductions)`.
    ///
    /// B3's scheduler-swap half (post-1.0) will wire this into
    /// the bytecode dispatch loop's yield-check hook: when the
    /// counter exceeds a threshold, the dispatch loop calls
    /// `(yield)` automatically. For now the user opts in by
    /// calling `(yield)` from Scheme — which keeps the seam in
    /// place without changing dispatch-loop perf.
    static REDUCTIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Bridge from the deeply-nested `sleep` builtin up to the coroutine's
    /// `Yielder`. corosensei passes the `Yielder` to the coroutine closure as a
    /// parameter, but `(sleep)` runs hundreds of `eval` frames below that entry
    /// and can't receive it that way — so [`drive_handler`] publishes it here
    /// (same single-thread raw-pointer discipline as `ACTOR_CTX`). Non-null only
    /// while a coroutine-driven handler is on the stack; `(sleep)` reads it to
    /// decide cooperative-suspend vs. plain `thread::sleep`.
    static YIELDER: std::cell::Cell<*const Yielder<CoResume, CoYield>> =
        const { std::cell::Cell::new(std::ptr::null()) };

    /// Per-worker pool of recycled coroutine stacks (mmap-backed, with a guard
    /// page). A handler that never sleeps checks a stack out, runs to `Return`,
    /// and checks it back in within one `drive_handler` call; only a handler
    /// *suspended* in `(sleep)` holds its stack across the `.await`. So the
    /// live-stack count tracks concurrent sleepers, not total actors — and a
    /// shallow suspended handler only resides its touched pages, not the full
    /// reservation.
    static STACK_POOL: std::cell::RefCell<Vec<DefaultStack>> =
        const { std::cell::RefCell::new(Vec::new()) };

    /// Separate pool for whole-body **green** coroutine stacks ([`GREEN_STACK_BYTES`],
    /// smaller than the activation stack). Kept apart so checkin never hands a
    /// small green stack to a deep activation handler, and vice versa. Green
    /// actors are long-lived (held for life), so this pool mainly smooths
    /// spawn/die churn rather than per-message reuse.
    static GREEN_STACK_POOL: std::cell::RefCell<Vec<DefaultStack>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Which pool a coroutine stack belongs to (see [`checkout_stack`]).
#[derive(Clone, Copy)]
enum StackClass {
    /// Per-message activation handler — the larger [`ACTOR_STACK_BYTES`] stack.
    Activation,
    /// Whole-body green actor — the smaller [`GREEN_STACK_BYTES`] stack.
    Green,
}

/// What a coroutine-hosted activation handler asks its driver to do when it
/// suspends — the coroutine's `Yield` type. The driver performs the async
/// operation (releasing the worker so co-located actors run) and resumes the
/// handler with a matching [`CoResume`].
enum CoYield {
    /// `(sleep)`/`(sleep-ms)`: park me this long, then resume with `Woke`.
    Sleep(Duration),
    /// `(raw-receive)`: park on the async mailbox (no timeout / try-once /
    /// timeout per `timeout`), then resume with `Received(..)`.
    Recv { timeout: Option<u64> },
    /// Reduction-budget preemption (the cs-vm yield hook fired mid-evaluation):
    /// release the worker for one scheduler tick, then resume with `Woke`. Lets
    /// a CPU-bound body cooperatively yield to co-located actors instead of
    /// monopolizing its shared worker. See [`green_yield_hook`].
    Yield,
    /// `(tcp-recv handle max)`: read up to `max` bytes asynchronously (releasing
    /// the worker), then resume with `Io(..)`. See [`cooperative_tcp_recv_hook`].
    #[cfg(feature = "stdlib-net")]
    Io { handle: i64, max: usize },
    /// `(tcp-send handle bytes)`: write `bytes` asynchronously (releasing the
    /// worker), then resume with `IoWrite(..)`. See [`cooperative_tcp_send_hook`].
    #[cfg(feature = "stdlib-net")]
    IoWrite { handle: i64, bytes: Vec<u8> },
}

/// What the driver passes back *into* the coroutine on resume — the coroutine's
/// `Input` type. A `Sleep` resumes with `Woke`; a `Recv` resumes with the
/// driver's processed mailbox result (same shape as [`primop_raw_receive`]:
/// `Ok(Some)` = message, `Ok(None)` = timeout, `Err` = mailbox closed /
/// trap-exit termination).
enum CoResume {
    /// Resume value after a `Sleep` or a `Yield`, and the (ignored) value of the
    /// first resume.
    Woke,
    /// Resume value after a `Recv`.
    Received(Result<Option<SendableValue>, String>),
    /// Resume value after an `Io` read: the bytes read (`Ok(empty)` on clean EOF).
    #[cfg(feature = "stdlib-net")]
    Io(Result<Vec<u8>, String>),
    /// Resume value after an `IoWrite`: success or the write error.
    #[cfg(feature = "stdlib-net")]
    IoWrite(Result<(), String>),
}

std::thread_local! {
    /// Whether this worker thread has installed [`green_yield_hook`] yet. The
    /// hook is per-thread (cs-vm's yield hook is a thread-local), so we install
    /// it lazily on first use and leave it: LocalSet workers are green-dedicated,
    /// and the hook is a no-op when no coroutine is active.
    static GREEN_HOOK_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// cs-vm yield hook for coroutine-driven actors (green `spawn-source-green` and
/// framework `spawn-activation`). The bytecode dispatch loop calls this every
/// reduction-budget tick; if we are inside a coroutine (a [`YIELDER`] is set),
/// suspend with [`CoYield::Yield`] so the driver hands the worker back to the
/// scheduler for one tick — cooperative CPU preemption — then resume right where
/// the hook fired. Outside a coroutine (`YIELDER` null) it is a no-op.
///
/// This is the green replacement for [`cs_actor::tokio_yield_hook`], which
/// `block_on`s `yield_now` and so **panics** on a current-thread LocalSet worker
/// (`block_on` re-entrancy). Suspending the coroutine instead is the only way to
/// release a current-thread worker cooperatively. Function-pointer compatible
/// with `cs_vm::vm::VmYieldHook = fn()`.
fn green_yield_hook() {
    let yielder = YIELDER.with(|c| c.get());
    if !yielder.is_null() {
        // Sound: see [`cooperative_sleep_hook`] — we are executing inside the
        // coroutine whose `Yielder` this is, on its thread, and the driver
        // re-installs `YIELDER` before every resume.
        unsafe { (*yielder).suspend(CoYield::Yield) };
    }
}

/// Install [`green_yield_hook`] as this worker thread's cs-vm yield hook, once.
/// Idempotent and never torn down (see [`GREEN_HOOK_INSTALLED`]).
fn ensure_green_yield_hook() {
    GREEN_HOOK_INSTALLED.with(|installed| {
        if !installed.get() {
            cs_vm::vm::install_yield_hook(Some(green_yield_hook));
            installed.set(true);
        }
    });
}

/// Coroutine stack size. Matches the LocalSet worker thread's default 2 MiB
/// stack: the coroutine stack *replaces* the worker thread's stack while a
/// handler runs, so 2 MiB preserves the non-tail Scheme recursion headroom a
/// handler had before. mmap is lazily committed, so the virtual size is cheap.
const ACTOR_STACK_BYTES: usize = 2 * 1024 * 1024;

/// Whole-body green-actor coroutine stack size — smaller than the per-message
/// activation stack because a green conn body parks shallow (its loop + a
/// `(tcp-recv)`/`(receive)`), and a green actor holds its stack for its whole
/// life (so the count tracks live conns, not churn). mmap is lazily committed,
/// so this is a *virtual*-footprint lever, not the RSS lever (RSS = touched
/// pages; the per-actor `Runtime` dominates at scale — see the green-threads
/// P5.2 measurement). 1 MiB keeps generous non-tail headroom while halving the
/// per-conn reservation.
///
/// cw-m9c (G1): `to_sendable_in`/`from_sendable` (this file) recurse one Rust
/// stack frame per cons cell, so a green actor `(receive)`-ing a large flat
/// list — e.g. a gRPC conn actor receiving a `kv-range-ok` reply for a
/// >=5k-row LIST — needs stack proportional to the list length; 1 MiB
/// overflowed at ~4-5k rows. Bumped 64x (still a lazily-committed *virtual*
/// reservation — no RSS cost until touched, so this is headroom, not a new
/// steady-state memory cost per idle conn) to cover a full 100k-row LIST
/// reply with margin.
const GREEN_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Cap on recycled stacks retained per worker, per pool. Past this, checked-in
/// stacks are dropped — so a transient burst can't permanently pin many
/// fully-faulted stacks.
const STACK_POOL_CAP: usize = 64;

/// Take a coroutine stack of the given [`StackClass`] from this worker's pool, or
/// allocate one. Allocation only fails at OOM (mmap), where aborting is the right
/// call — mirrors the `expect` on worker-thread spawn in `cs_actor::local_pool`.
fn checkout_stack(class: StackClass) -> DefaultStack {
    match class {
        StackClass::Activation => STACK_POOL
            .with(|p| p.borrow_mut().pop())
            .unwrap_or_else(|| {
                DefaultStack::new(ACTOR_STACK_BYTES).expect("allocate activation coroutine stack")
            }),
        StackClass::Green => GREEN_STACK_POOL
            .with(|p| p.borrow_mut().pop())
            .unwrap_or_else(|| {
                DefaultStack::new(GREEN_STACK_BYTES).expect("allocate green coroutine stack")
            }),
    }
}

/// Return a finished coroutine's stack to its class's pool for reuse, up to
/// [`STACK_POOL_CAP`]; drop it past the cap.
fn checkin_stack(stack: DefaultStack, class: StackClass) {
    let pool = match class {
        StackClass::Activation => &STACK_POOL,
        StackClass::Green => &GREEN_STACK_POOL,
    };
    pool.with(|p| {
        let mut v = p.borrow_mut();
        if v.len() < STACK_POOL_CAP {
            v.push(stack);
        }
    });
}

/// Run `f` with a `&mut` reference to the currently-running
/// Actor. Returns `None` if we're not inside an actor body —
/// i.e., the call site is from top-level Scheme rather than a
/// spawned actor. Callers surface that as a Scheme error.
pub fn with_current_actor<R>(f: impl FnOnce(&mut cs_actor::Actor) -> R) -> Option<R> {
    ACTOR_CTX.with(|c| {
        let p = c.get();
        if p.is_null() {
            None
        } else {
            // Safety: ACTOR_CTX is set only by primop_spawn for the
            // lifetime of the body closure on this thread, and is
            // cleared by the Drop guard before the closure
            // returns/unwinds. The pointer never crosses thread
            // boundaries (thread_local). The Actor outlives every
            // borrow we take here.
            Some(f(unsafe { &mut *p }))
        }
    })
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
    process_received(actor, msg)
}

/// Trap-exit enforcement + `Message` -> `SendableValue`, shared by the blocking
/// [`primop_raw_receive`] and the cooperative [`driver_receive`] once a `Message`
/// has been popped. BEAM trap-exit: a non-`Normal` `Exit` delivered to a
/// non-trapping actor is surfaced as a Rust `Err` — the actor body's
/// `catch_unwind` turns that into the actor's exit reason, and any LINKED actors
/// get a chained `Exit` through `on_actor_termination`.
fn process_received(
    actor: &mut cs_actor::Actor,
    msg: Message,
) -> Result<Option<SendableValue>, String> {
    if let Message::Exit { from, reason } = &msg {
        if !actor.is_trapping_exits() && !matches!(reason, ExitReason::Normal) {
            return Err(format!(
                "linked actor {} exited abnormally: {}",
                from,
                exit_reason_str(reason)
            ));
        }
    }
    Ok(Some(message_to_sendable(msg)))
}

fn message_to_sendable(msg: Message) -> SendableValue {
    match msg {
        Message::User(payload) => {
            if let Some(sv) = payload_to_sendable(&payload) {
                return sv;
            }
            // cs-web sends `Arc<WebMessage>` payloads from hyper's
            // request task to a registered actor's mailbox. The
            // bridge stashes the envelope and returns a tagged
            // pair the Scheme handler pattern-matches.
            #[cfg(feature = "web")]
            if let Some(sv) = crate::builtins::web::try_intern_web_request(&payload) {
                return sv;
            }
            // cs-grpc sends gRPC call-event payloads (begin / stream-msg /
            // stream-end) from hyper's h2c request task to the registered
            // handler actor. Same bridge shape as the web one: stash + tagged
            // pair (`*grpc-request*` / `*grpc-stream-msg*` / `*grpc-stream-end*`).
            #[cfg(feature = "grpc")]
            if let Some(sv) = crate::builtins::grpc::try_intern_grpc_request(&payload) {
                return sv;
            }
            // Genuinely-foreign payload (Rust test fixture, third-
            // party plugin, ...). Wrap as a placeholder symbol so
            // Scheme pattern-match still has something to compare.
            SendableValue::Symbol("*opaque-payload*".into())
        }
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

//
// VersionRegistry exports are type-erased `Arc<dyn Any + Send + Sync>`;
// we wrap a SendableValue inside the Arc so the same Value <->
// boundary conversion the actor / table primops use applies. That
// means modules in v1 carry data exports (constants, lookup
// tables, behavior-state shapes); procedure exports stay in the
// ProcedureRegistry, which already supports "register again under
// the same name" as its hot-reload story.

/// Load (or re-load) a module. Returns the new current version's
/// epoch. The exports map keys are export names (e.g.,
/// "init-state", "max-value"); the values are SendableValues
/// produced by the Scheme caller.
pub fn primop_load_module(
    module: &str,
    exports: cs_hotreload::ExportsMap<String, SendableValue>,
) -> u32 {
    let typed: cs_hotreload::ExportsMap<String, cs_hotreload::Export> = exports
        .into_iter()
        .map(|(k, v)| (k, Arc::new(v) as cs_hotreload::Export))
        .collect();
    beam_state().versions.load(module, typed)
}

/// Look up an export in the **current** version. Returns `None`
/// if the module isn't loaded or the export doesn't exist.
pub fn primop_lookup_code(module: &str, name: &str) -> Option<SendableValue> {
    let export = beam_state().versions.lookup(module, name)?;
    export.downcast_ref::<SendableValue>().cloned()
}

/// Look up an export in the **old** (pre-reload) version. Used
/// by behavior actors that are still in-flight on the old code
/// when a reload lands.
pub fn primop_lookup_code_old(module: &str, name: &str) -> Option<SendableValue> {
    let export = beam_state().versions.lookup_old(module, name)?;
    export.downcast_ref::<SendableValue>().cloned()
}

pub fn primop_code_soft_purge(module: &str, holder_count: usize) -> Result<(), String> {
    beam_state()
        .versions
        .soft_purge(module, holder_count)
        .map_err(|e| format!("code-soft-purge!: {}", e))
}

pub fn primop_code_purge(module: &str) -> Result<(), String> {
    beam_state()
        .versions
        .purge(module)
        .map_err(|e| format!("code-purge!: {}", e))
}

/// `(old-epoch, current-epoch)` or `None` if the module isn't
/// loaded. Either component is `None` when that slot is empty
/// (e.g., a module loaded once has no old version).
pub fn primop_code_versions(module: &str) -> Option<(Option<u32>, Option<u32>)> {
    beam_state().versions.epochs(module)
}

pub fn primop_code_modules() -> Vec<String> {
    beam_state().versions.modules()
}

pub fn primop_code_unload(module: &str) {
    beam_state().versions.unload(module);
}

//
// Each builtin takes a slice of `Value` from the dispatcher,
// converts via `to_sendable_in` / `from_sendable`, calls the
// primop, then converts the result back. Type errors are
// returned as the "<who>: <message>" string the dispatcher
// converts to a proper R6RS condition.

/// Decode a PID from its symbol form. We carry PIDs across
/// the Scheme boundary as symbols named like `<pid:<node.local>>`
/// (see `from_sendable`). Parse the embedded `<node.local>`
/// piece back into an `ActorPid`.
fn parse_pid_symbol(name: &str) -> Option<ActorPid> {
    // Format: "<pid:<NODE.LOCAL>>"
    let inner = name.strip_prefix("<pid:<")?.strip_suffix(">>")?;
    let (n, l) = inner.split_once('.')?;
    Some(ActorPid {
        node: n.parse().ok()?,
        local_id: l.parse().ok()?,
    })
}

fn value_to_pid(v: &Value, syms: &SymbolTable, who: &str) -> Result<ActorPid, String> {
    match v {
        Value::Symbol(s) => {
            let name = syms.name(*s);
            parse_pid_symbol(name).ok_or_else(|| {
                format!(
                    "{}: expected a PID symbol like <pid:<n.m>>, got '{}'",
                    who, name
                )
            })
        }
        other => Err(format!(
            "{}: expected a PID symbol, got {}",
            who,
            other.type_name()
        )),
    }
}

fn check_arity(who: &str, args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        let noun = if expected == 1 {
            "argument"
        } else {
            "arguments"
        };
        Err(format!(
            "{}: expected {} {}, got {}",
            who,
            expected,
            noun,
            args.len()
        ))
    }
}

fn value_to_str<'a>(v: &'a Value, syms: &'a SymbolTable, who: &str) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(syms.name(*s).to_string()),
        Value::String(g) => Ok(g.borrow().clone()),
        other => Err(format!(
            "{}: expected symbol or string, got {}",
            who,
            other.type_name()
        )),
    }
}

/// `(send pid value)` — fire-and-forget cast.
pub fn b_beam_send(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("send", args, 2)?;
    let pid = value_to_pid(&args[0], syms, "send")?;
    // cs-845.1: try the same-worker fast path first — skips the
    // SendableValue projection/rebuild round-trip when sender and
    // receiver are colocated on the same LocalWorkerPool worker. Falls
    // back to the general to_sendable_in path unchanged otherwise.
    let target_worker = beam_state().actors.local_worker_of(pid);
    match try_send_same_worker(&args[1], syms, pid, target_worker)? {
        Some((recv, seq, cloned)) => {
            // Send the marker first; only park the value once the marker was
            // actually delivered. If `primop_send` errors (target just
            // terminated), we return without committing, so no orphan value is
            // left in the side-queue.
            primop_send(pid, SendableValue::Local { recv, seq })?;
            commit_same_worker_msg(recv, seq, cloned);
        }
        None => {
            let sv = to_sendable_in(&args[1], syms)?;
            primop_send(pid, sv)?;
        }
    }
    Ok(Value::Unspecified)
}

/// `(make-table name type)` — create a named table. `type` is
/// either the symbol `set` or `ordered-set`.
pub fn b_beam_make_table(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("make-table", args, 2)?;
    let name = value_to_str(&args[0], syms, "make-table")?;
    let ty = value_to_str(&args[1], syms, "make-table")?;
    primop_make_table(&name, &ty)?;
    Ok(Value::Unspecified)
}

/// `(table-insert! name key value)` — set the cell at `key` to
/// `value`. Overwrites any prior value (set / ordered_set
/// semantics).
pub fn b_beam_table_insert(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("table-insert!", args, 3)?;
    let name = value_to_str(&args[0], syms, "table-insert!")?;
    let key = to_sendable_in(&args[1], syms)?;
    let value = to_sendable_in(&args[2], syms)?;
    primop_table_insert(&name, key, value)?;
    Ok(Value::Unspecified)
}

/// `(table-lookup name key)` — returns the value or `#f`.
pub fn b_beam_table_lookup(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("table-lookup", args, 2)?;
    let name = value_to_str(&args[0], syms, "table-lookup")?;
    let key = to_sendable_in(&args[1], syms)?;
    match primop_table_lookup(&name, &key)? {
        Some(sv) => Ok(from_sendable(&sv, syms)),
        None => Ok(Value::Boolean(false)),
    }
}

/// `(table-delete! name key)` — returns `#t` if the key was
/// present (and is now removed), `#f` otherwise.
pub fn b_beam_table_delete(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("table-delete!", args, 2)?;
    let name = value_to_str(&args[0], syms, "table-delete!")?;
    let key = to_sendable_in(&args[1], syms)?;
    let removed = primop_table_delete(&name, &key)?;
    Ok(Value::Boolean(removed))
}

/// `(table-get-resp-bulk name key)` — look up a **bytevector** value in table
/// `name` and return its RESP *bulk* encoding (`$<len>\r\n<bytes>\r\n`) as a
/// fresh bytevector, or `#f` if the key is absent or its value isn't a
/// bytevector. The value bytes go straight from the stored table payload into
/// the framed output buffer — with NO intermediate Scheme bytevector and NO
/// separate encode pass, so it avoids both the `table-lookup` deep-clone
/// (`payload_to_sendable`'s `.cloned()`) and the Scheme `resp-encode`. A read
/// fast-path for byte-cache front-ends (crab-cache GET); semantics-free
/// (length-prefixed framing only).
pub fn b_beam_table_get_resp_bulk(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("table-get-resp-bulk", args, 2)?;
    let name = value_to_str(&args[0], syms, "table-get-resp-bulk")?;
    let key = to_sendable_in(&args[1], syms)?;
    let k = key_of(&key)?;
    let looked = beam_state()
        .tables
        .lookup(&name, &k)
        .map_err(|e| format!("table-get-resp-bulk: {}", e))?;
    // Borrow the stored payload's bytes (no clone) and frame them in one alloc.
    match looked
        .as_ref()
        .and_then(|p| p.downcast_ref::<SendableValue>())
    {
        Some(SendableValue::ByteVector(bytes)) => {
            let mut out = Vec::with_capacity(bytes.len() + 16);
            out.push(b'$');
            out.extend_from_slice(bytes.len().to_string().as_bytes());
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(bytes);
            out.extend_from_slice(b"\r\n");
            Ok(Value::ByteVector(cs_core::Gc::new(
                std::cell::RefCell::new(out),
            )))
        }
        _ => Ok(Value::Boolean(false)),
    }
}

// ---- native fused GET fast-path (crab-cache) ----
//
// CRC16-CCITT (XMODEM): polynomial 0x1021, init 0x0000, no input/output
// reflection, no final XOR — byte-for-byte the same as crab-cache's
// `crc16-bytes` (src/slotmap.scm) and Redis Cluster.
fn cc_crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in bytes {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// {hashtag}: if the key has a '{' followed later by a '}' with at least one
/// byte between them, hash ONLY that substring; otherwise hash the whole key.
/// Mirrors crab-cache `hashtag-bytes` (src/slotmap.scm) byte-for-byte.
fn cc_hashtag(key: &[u8]) -> &[u8] {
    if let Some(open) = key.iter().position(|&c| c == b'{') {
        // first '}' strictly after the '{'
        if let Some(rel) = key[open + 1..].iter().position(|&c| c == b'}') {
            let close = open + 1 + rel;
            if close > open + 1 {
                return &key[open + 1..close];
            }
        }
    }
    key
}

/// slot = CRC16(hashtag(key)) mod 16384. Mirrors crab-cache `key-slot`.
fn cc_slot(key: &[u8]) -> i64 {
    (cc_crc16(cc_hashtag(key)) as i64) % 16384
}

/// shard = (slot * nshards) / 16384. Mirrors crab-cache `slot->shard`.
fn cc_slot_to_shard(slot: i64, nshards: i64) -> i64 {
    (slot * nshards) / 16384
}

/// slot = CRC16(hashtag(key)) mod 16384; shard = (slot * nshards) / 16384.
/// Mirrors crab-cache `key-slot` + `slot->shard`.
fn cc_shard(key: &[u8], nshards: i64) -> i64 {
    cc_slot_to_shard(cc_slot(key), nshards)
}

/// Read a base-10 length (no sign — RESP bulk/array headers are non-negative)
/// from `buf[start..]` up to the next CRLF. Returns `(value, index-after-CRLF)`,
/// or `None` if the CRLF hasn't arrived yet (incomplete) or a non-digit byte
/// appears before it (malformed → treated as a non-servable frame → STOP).
fn cc_read_len(buf: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut i = start;
    let mut acc: usize = 0;
    let mut any = false;
    while i + 1 < buf.len() {
        let c = buf[i];
        if c == b'\r' && buf[i + 1] == b'\n' {
            return if any { Some((acc, i + 2)) } else { None };
        }
        if !c.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add((c - b'0') as usize)?;
        any = true;
        i += 1;
    }
    None
}

/// Parse a single `$<len>\r\n<bytes>\r\n` bulk at `pos`, returning the byte
/// slice and the index just past it, or `None` if it is incomplete / not a
/// bulk header. Mirrors resp.scm `parse-bulk` for the complete-frame case.
fn cc_parse_bulk(buf: &[u8], pos: usize) -> Option<(&[u8], usize)> {
    if pos >= buf.len() || buf[pos] != b'$' {
        return None;
    }
    let (len, data_start) = cc_read_len(buf, pos + 1)?;
    let end = data_start.checked_add(len)?;
    // need the value bytes AND the trailing CRLF
    if end + 2 > buf.len() {
        return None;
    }
    Some((&buf[data_start..end], end + 2))
}

/// A command's route — the Rust port of crab-cache `classify-route`
/// (src/router.scm). `Shard(i)` is the single shard owning all the command's
/// keys; the variants mirror router.scm's `'any`/`'all`/`'cluster`/`'crossslot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CcRoute {
    Any,
    All,
    Cluster,
    CrossSlot,
    Shard(i64),
}

/// Which operand indices are keys, or a symbolic route — the port of
/// router.scm `key-positions`. `name` must already be ASCII-upcased.
enum CcKeySpec {
    Any,
    All,
    Cluster,
    Indices(Vec<usize>),
}

fn cc_in(name: &[u8], set: &[&[u8]]) -> bool {
    set.iter().any(|s| *s == name)
}

fn cc_key_positions(name: &[u8], argc: usize) -> CcKeySpec {
    if cc_in(
        name,
        &[
            b"PING", b"ECHO", b"SELECT", b"COMMAND", b"INFO", b"QUIT", b"TICK",
        ],
    ) {
        CcKeySpec::Any
    } else if cc_in(name, &[b"DBSIZE", b"FLUSHALL", b"FLUSHDB", b"KEYS"]) {
        CcKeySpec::All
    } else if name == b"CLUSTER" {
        CcKeySpec::Cluster
    } else if cc_in(
        name,
        &[b"DEL", b"EXISTS", b"UNLINK", b"MGET", b"SMISMEMBER"],
    ) {
        CcKeySpec::Indices((0..argc).collect()) // every operand is a key
    } else if name == b"MSET" {
        CcKeySpec::Indices((0..argc).step_by(2).collect()) // even indices: key val key val …
    } else {
        CcKeySpec::Indices(vec![0]) // single key at operand 0
    }
}

/// Classify a command into its route. `verb` is the raw arg0 (upcased here);
/// `operands` are the post-verb bulk args. Byte-for-byte faithful to
/// router.scm `classify-route` so the native fast-path and the interpreted
/// fallback never disagree (asserted by the differential test
/// `test/native-classify-diff.scm`).
fn cc_classify(verb: &[u8], operands: &[&[u8]], nshards: i64) -> CcRoute {
    let name = verb.to_ascii_uppercase();
    match cc_key_positions(&name, operands.len()) {
        CcKeySpec::Any => CcRoute::Any,
        CcKeySpec::All => CcRoute::All,
        CcKeySpec::Cluster => CcRoute::Cluster,
        CcKeySpec::Indices(ps) => {
            let n = operands.len();
            let keys: Vec<&[u8]> = ps
                .into_iter()
                .filter(|&p| p < n)
                .map(|p| operands[p])
                .collect();
            if keys.is_empty() {
                CcRoute::Shard(0) // keyed command given no key -> shard 0 (arity-errs there)
            } else {
                let slot0 = cc_slot(keys[0]);
                if keys.iter().all(|k| cc_slot(k) == slot0) {
                    CcRoute::Shard(cc_slot_to_shard(slot0, nshards))
                } else {
                    CcRoute::CrossSlot
                }
            }
        }
    }
}

/// Append a RESP bulk `$<len>\r\n<bytes>\r\n` — byte-for-byte identical to
/// `table-get-resp-bulk` and resp.scm `(resp-encode (r-bulk …))`.
fn cc_append_bulk(out: &mut Vec<u8>, bytes: &[u8]) {
    out.reserve(bytes.len() + 16);
    out.push(b'$');
    out.extend_from_slice(bytes.len().to_string().as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
}

/// Append a RESP error `-<msg>\r\n` (msg includes the code word).
fn cc_append_err(out: &mut Vec<u8>, msg: &[u8]) {
    out.push(b'-');
    out.extend_from_slice(msg);
    out.extend_from_slice(b"\r\n");
}

/// Append the RESP reply for an 'any (stateless) verb — byte-for-byte identical
/// to conn.scm `stateless-reply` composed with resp.scm `resp-encode`. Only
/// ever called for the closed 'any set {PING ECHO SELECT COMMAND INFO QUIT
/// TICK} (cc_classify routes nothing else here); the catch-all mirrors
/// stateless-reply's `(else (r-ok))`.
fn cc_append_stateless(out: &mut Vec<u8>, verb: &[u8], operands: &[&[u8]]) {
    let name = verb.to_ascii_uppercase();
    match name.as_slice() {
        b"PING" => {
            if operands.is_empty() {
                out.extend_from_slice(b"+PONG\r\n"); // (r-simple "PONG")
            } else {
                cc_append_bulk(out, operands[0]); // (r-bulk (car operands))
            }
        }
        b"ECHO" => {
            if !operands.is_empty() {
                cc_append_bulk(out, operands[0]);
            } else {
                cc_append_err(out, b"ERR wrong number of arguments for 'echo' command");
            }
        }
        b"COMMAND" => out.extend_from_slice(b"*0\r\n"), // (r-array '())
        b"INFO" => cc_append_bulk(
            out,
            b"# Server\r\nredis_version:7.4.0-crabscheme\r\ncrab_cache:1\r\n",
        ),
        // SELECT, QUIT, TICK, and stateless-reply's `else` -> +OK
        _ => out.extend_from_slice(b"+OK\r\n"),
    }
}

/// `(conn-serve-gets data node-name nshards)` — serve the LEADING run of
/// locally-led GET *hits* from the RESP read buffer `data` entirely in Rust,
/// short-circuiting crab-cache's interpreted parse/dispatch/encode for the
/// common case.
///
/// For each frame from offset 0: if it is exactly `*2\r\n$3\r\nGET\r\n$K\r\n
/// <key>\r\n` (arg0 upcased == "GET", arity 1) AND this node leads the key's
/// slot (cc-shard-leader["<node>:<shard>"] == node-name) AND cc-str holds the
/// key as a ByteVector, append its bulk frame `$<len>\r\n<value>\r\n` (byte-for-
/// byte identical to `table-get-resp-bulk`) to the output. STOP at the FIRST
/// frame that is anything else — partial frame, non-GET, GET with arity != 1, a
/// non-locally-led slot, or a GET miss — and consume nothing past it.
///
/// Returns `(cons out-bytevector consumed)`: `out` = concatenated hit frames,
/// `consumed` = the byte offset where parsing stopped (0 if the first frame
/// isn't a servable GET-hit). The caller tcp-sends `out` and runs the existing
/// interpreted path on `data[consumed..]`, so SET / non-local / miss /
/// SUBSCRIBE / inline / partial all flow through unchanged.
pub fn b_beam_conn_serve_gets(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("conn-serve-gets", args, 3)?;
    let data = match &args[0] {
        Value::ByteVector(bv) => bv.borrow(),
        other => {
            return Err(format!(
                "conn-serve-gets: data must be a bytevector, got {}",
                other.type_name()
            ));
        }
    };
    let node = value_to_str(&args[1], syms, "conn-serve-gets")?;
    let nshards = match &args[2] {
        Value::Fixnum(n) => *n,
        other => {
            return Err(format!(
                "conn-serve-gets: nshards must be a fixnum, got {}",
                other.type_name()
            ));
        }
    };

    let buf: &[u8] = &data;
    let n = buf.len();
    let mut out: Vec<u8> = Vec::new();
    let mut consumed: usize = 0;
    let tables = &beam_state().tables;

    loop {
        let frame_start = consumed;
        // Must be a RESP array header `*…`. Anything else (inline, EOF, partial)
        // stops the native run and falls back to the interpreted path.
        if frame_start >= n || buf[frame_start] != b'*' {
            break;
        }
        let (argc, mut pos) = match cc_read_len(buf, frame_start + 1) {
            Some(x) => x,
            None => break, // incomplete or malformed multibulk header
        };
        // A GET is exactly two bulk args; any other arity is not our fast path.
        if argc != 2 {
            break;
        }
        // arg0 must be the verb "GET" (case-insensitive, exactly 3 bytes).
        let (verb, after_verb) = match cc_parse_bulk(buf, pos) {
            Some(x) => x,
            None => break,
        };
        if !verb.eq_ignore_ascii_case(b"GET") {
            break;
        }
        pos = after_verb;
        // arg1 = key.
        let (key, after_key) = match cc_parse_bulk(buf, pos) {
            Some(x) => x,
            None => break,
        };

        // Leadership: serve locally iff cc-shard-leader["<node>:<shard>"] == node.
        let shard = cc_shard(key, nshards);
        let qk = format!("{}:{}", node, shard);
        let leader = match tables.lookup("cc-shard-leader", &cs_table::Key::String(qk)) {
            Ok(l) => l,
            Err(_) => break, // table missing → let the interpreted path decide
        };
        let leads = matches!(
            leader.as_ref().and_then(|p| p.downcast_ref::<SendableValue>()),
            Some(SendableValue::Symbol(s)) if s == &node
        );
        if !leads {
            break; // not locally led → fall back (interpreted path emits MOVED)
        }

        // Hit only if cc-str holds the key as a ByteVector. A miss must fall
        // back to the shard (authoritative RocksDB read + warm-on-miss + TTL
        // lazy-expiry), so STOP rather than emit nil.
        let looked = match tables.lookup("cc-str", &cs_table::Key::Bytes(key.to_vec())) {
            Ok(v) => v,
            Err(_) => break,
        };
        match looked
            .as_ref()
            .and_then(|p| p.downcast_ref::<SendableValue>())
        {
            Some(SendableValue::ByteVector(bytes)) => {
                // byte-for-byte identical framing to table-get-resp-bulk
                out.reserve(bytes.len() + 16);
                out.push(b'$');
                out.extend_from_slice(bytes.len().to_string().as_bytes());
                out.extend_from_slice(b"\r\n");
                out.extend_from_slice(bytes);
                out.extend_from_slice(b"\r\n");
            }
            _ => break, // miss / non-bytevector → fall back to the shard
        }

        // This frame was fully served natively; advance.
        consumed = after_key;
    }

    let out_v = Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(out)));
    Ok(Value::Pair(Pair::new(
        out_v,
        Value::Fixnum(consumed as i64),
    )))
}

/// `(conn-serve-batch data node-name nshards fast?)` — the native
/// decode→classify→local-dispatch loop (cc-5pw.2). A strict SUPERSET of
/// [`b_beam_conn_serve_gets`]: from offset 0 it parses each complete RESP array
/// frame, classifies it via [`cc_classify`] (≡ router.scm `classify-route`), and
/// serves entirely in Rust the closed allowlist of side-effect-free verbs — the
/// 'any stateless verbs (PING/ECHO/SELECT/COMMAND/INFO/QUIT/TICK) in BOTH
/// consistency modes, and GET *hits* (cc-str) only when `fast?` is true
/// (linearizable GET must take the ReadIndex shard path, cc-idc).
///
/// It STOPS — returning the bytes consumed so far — at the first frame that
/// needs the interpreter: any write/Raft op or other Shard-routed non-GET
/// (SET/DEL/INCR/MULTI/EXEC/SUBSCRIBE/PUBLISH/…), an 'all/'cluster route, a
/// cross-slot command, a non-local or no-leader GET, a cc-str miss, a
/// partial/non-array/`*0` frame, or an unknown verb (which classifies to a
/// Shard route and falls through — never guessed). Returns
/// `(cons out-bytevector consumed)`: the caller tcp-sends `out` and runs the
/// unchanged interpreted path on `data[consumed..]`.
pub fn b_beam_conn_serve_batch(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("conn-serve-batch", args, 4)?;
    let data = match &args[0] {
        Value::ByteVector(bv) => bv.borrow(),
        other => {
            return Err(format!(
                "conn-serve-batch: data must be a bytevector, got {}",
                other.type_name()
            ));
        }
    };
    let node = value_to_str(&args[1], syms, "conn-serve-batch")?;
    let nshards = match &args[2] {
        Value::Fixnum(n) => *n,
        other => {
            return Err(format!(
                "conn-serve-batch: nshards must be a fixnum, got {}",
                other.type_name()
            ));
        }
    };
    // Scheme truthiness: anything but #f means fast mode (serve GET hits locally).
    let fast = !matches!(&args[3], Value::Boolean(false));

    let buf: &[u8] = &data;
    let n = buf.len();
    let mut out: Vec<u8> = Vec::new();
    let mut consumed: usize = 0;
    let tables = &beam_state().tables;

    'frames: loop {
        let frame_start = consumed;
        // Must be a RESP array header `*…`. inline/EOF/partial → fallback.
        if frame_start >= n || buf[frame_start] != b'*' {
            break;
        }
        let (argc, mut pos) = match cc_read_len(buf, frame_start + 1) {
            Some(x) => x,
            None => break, // incomplete or malformed multibulk header
        };
        if argc == 0 {
            break; // `*0` frame → hand to the interpreted parser
        }
        // Parse every bulk arg of this frame (slices borrowed from `buf`).
        let mut frame_args: Vec<&[u8]> = Vec::with_capacity(argc);
        for _ in 0..argc {
            match cc_parse_bulk(buf, pos) {
                Some((b, next)) => {
                    frame_args.push(b);
                    pos = next;
                }
                None => break 'frames, // incomplete/malformed → consume nothing of this frame
            }
        }
        let verb = frame_args[0];
        let operands = &frame_args[1..];

        match cc_classify(verb, operands, nshards) {
            // 'any → a stateless reply computed purely from the operands.
            CcRoute::Any => cc_append_stateless(&mut out, verb, operands),
            // A single owning shard: only GET *hits* serve natively; every write
            // and every other read defers to the interpreter (Raft / ReadIndex).
            CcRoute::Shard(shard) => {
                if !fast || !verb.eq_ignore_ascii_case(b"GET") || operands.len() != 1 {
                    break;
                }
                let key = operands[0];
                let qk = format!("{}:{}", node, shard);
                let leader = match tables.lookup("cc-shard-leader", &cs_table::Key::String(qk)) {
                    Ok(l) => l,
                    Err(_) => break, // table missing → let the interpreted path decide
                };
                let leads = matches!(
                    leader.as_ref().and_then(|p| p.downcast_ref::<SendableValue>()),
                    Some(SendableValue::Symbol(s)) if s == &node
                );
                if !leads {
                    break; // not locally led → fall back (interpreted path emits MOVED)
                }
                let looked = match tables.lookup("cc-str", &cs_table::Key::Bytes(key.to_vec())) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                match looked
                    .as_ref()
                    .and_then(|p| p.downcast_ref::<SendableValue>())
                {
                    Some(SendableValue::ByteVector(bytes)) => cc_append_bulk(&mut out, bytes),
                    _ => break, // miss / non-bytevector → fall back to the shard (warms cc-str)
                }
            }
            // Fan-out / topology / cross-slot all need the interpreter (or cfg
            // the native path doesn't hold) → stop and fall back.
            CcRoute::All | CcRoute::Cluster | CcRoute::CrossSlot => break,
        }

        consumed = pos; // this frame was fully served natively; advance
    }

    let out_v = Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(out)));
    Ok(Value::Pair(Pair::new(
        out_v,
        Value::Fixnum(consumed as i64),
    )))
}

/// `(native-classify-route name operands nshards)` — expose [`cc_classify`] to
/// Scheme so the differential test can assert it agrees with router.scm
/// `classify-route` over a fuzz corpus. `name` is a bytevector (raw verb),
/// `operands` a list of bytevectors, `nshards` a fixnum. Returns the route as
/// router.scm does: the symbol `'any`/`'all`/`'cluster`/`'crossslot`, or the
/// shard index as a fixnum.
pub fn b_beam_native_classify_route(
    args: &[Value],
    syms: &mut SymbolTable,
) -> Result<Value, String> {
    check_arity("native-classify-route", args, 3)?;
    let verb: Vec<u8> = match &args[0] {
        Value::ByteVector(bv) => bv.borrow().clone(),
        other => {
            return Err(format!(
                "native-classify-route: name must be a bytevector, got {}",
                other.type_name()
            ));
        }
    };
    let ops_vals = proper_list(&args[1])
        .ok_or_else(|| "native-classify-route: operands must be a proper list".to_string())?;
    let mut ops: Vec<Vec<u8>> = Vec::with_capacity(ops_vals.len());
    for v in &ops_vals {
        match v {
            Value::ByteVector(bv) => ops.push(bv.borrow().clone()),
            other => {
                return Err(format!(
                    "native-classify-route: each operand must be a bytevector, got {}",
                    other.type_name()
                ));
            }
        }
    }
    let nshards = match &args[2] {
        Value::Fixnum(n) => *n,
        other => {
            return Err(format!(
                "native-classify-route: nshards must be a fixnum, got {}",
                other.type_name()
            ));
        }
    };
    let op_slices: Vec<&[u8]> = ops.iter().map(|v| v.as_slice()).collect();
    Ok(match cc_classify(&verb, &op_slices, nshards) {
        CcRoute::Any => Value::Symbol(syms.intern("any")),
        CcRoute::All => Value::Symbol(syms.intern("all")),
        CcRoute::Cluster => Value::Symbol(syms.intern("cluster")),
        CcRoute::CrossSlot => Value::Symbol(syms.intern("crossslot")),
        CcRoute::Shard(s) => Value::Fixnum(s),
    })
}

/// `(table-size name)` — returns the current cell count.
pub fn b_beam_table_size(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("table-size", args, 1)?;
    let name = value_to_str(&args[0], syms, "table-size")?;
    let n = primop_table_size(&name)?;
    Ok(Value::Fixnum(n as i64))
}

/// `(spawn name arg ...)` — look up the named procedure in the
/// process-wide ProcedureRegistry, spawn an actor that runs it
/// with the (deep-cloned) `arg ...` values, return its PID.
///
/// In v1 the registry only accepts entries pre-registered from
/// Rust (tests, FFI). A future iter wires
/// `(register-procedure! 'name (lambda (args) ...))` so Scheme
/// can register its own — that needs a Scheme-thunk -> Rust
/// closure compiler (the eval target's job, not this builtin).
pub fn b_beam_spawn(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.is_empty() {
        return Err("spawn: expected a procedure name and 0+ arguments".into());
    }
    let name = value_to_str(&args[0], syms, "spawn")?;
    let mut sendable_args = Vec::with_capacity(args.len() - 1);
    for a in &args[1..] {
        sendable_args.push(to_sendable_in(a, syms)?);
    }
    let pid = primop_spawn(&name, sendable_args)?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

/// `(spawn-source SOURCE ENTRY arg ...)` — spawn an actor running a Scheme
/// body. `SOURCE` is a string of Scheme loaded into a fresh per-actor Runtime
/// on the actor's own thread; `ENTRY` (symbol or string) names the top-level
/// procedure to run; `arg ...` are passed to it. Returns the new PID.
///
/// This is the Scheme-on-actor path that `(spawn name …)` cannot be: a raw
/// `(lambda …)` is `Rc`-based / `!Send` and so can't move onto the actor's
/// worker thread. See [`primop_spawn_source`].
pub fn b_beam_spawn_source(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("spawn-source: expected a SOURCE string, an ENTRY name, and 0+ args".into());
    }
    let source = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => {
            return Err(format!(
                "spawn-source: SOURCE must be a string, got {}",
                other.type_name()
            ));
        }
    };
    let entry = value_to_str(&args[1], syms, "spawn-source")?;
    let mut sendable_args = Vec::with_capacity(args.len() - 2);
    for a in &args[2..] {
        sendable_args.push(to_sendable_in(a, syms)?);
    }
    let pid = primop_spawn_source(source, entry, sendable_args)?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

/// `(spawn-source-green SOURCE ENTRY arg ...)` — like [`b_beam_spawn_source`]
/// but runs the body green (on the parking `LocalSet` pool) instead of a
/// dedicated thread, so the actor parks on receive/sleep and many actors share a
/// worker. M1 opt-in sibling of `spawn-source`. See [`primop_spawn_source_green`].
pub fn b_beam_spawn_source_green(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(
            "spawn-source-green: expected a SOURCE string, an ENTRY name, and 0+ args".into(),
        );
    }
    let source = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => {
            return Err(format!(
                "spawn-source-green: SOURCE must be a string, got {}",
                other.type_name()
            ));
        }
    };
    let entry = value_to_str(&args[1], syms, "spawn-source-green")?;
    let mut sendable_args = Vec::with_capacity(args.len() - 2);
    for a in &args[2..] {
        sendable_args.push(to_sendable_in(a, syms)?);
    }
    let pid = primop_spawn_source_green(source, entry, sendable_args)?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

/// `(spawn-source-dedicated SOURCE ENTRY arg ...)` — like [`b_beam_spawn_source`]
/// but always runs the body on its own dedicated OS thread (the pre-flip
/// `spawn-source` semantics). For actors doing blocking work with no cooperative
/// counterpart (long `fsync`, a sole-drainer poll loop). See
/// [`primop_spawn_source_dedicated`].
pub fn b_beam_spawn_source_dedicated(
    args: &[Value],
    syms: &mut SymbolTable,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(
            "spawn-source-dedicated: expected a SOURCE string, an ENTRY name, and 0+ args".into(),
        );
    }
    let source = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => {
            return Err(format!(
                "spawn-source-dedicated: SOURCE must be a string, got {}",
                other.type_name()
            ));
        }
    };
    let entry = value_to_str(&args[1], syms, "spawn-source-dedicated")?;
    let mut sendable_args = Vec::with_capacity(args.len() - 2);
    for a in &args[2..] {
        sendable_args.push(to_sendable_in(a, syms)?);
    }
    let pid = primop_spawn_source_dedicated(source, entry, sendable_args)?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

/// `(spawn-activation SOURCE HANDLER)` — spawn a Scheme actor on the parking
/// `LocalSet` worker pool. `SOURCE` is a Scheme string; `HANDLER` is a symbol
/// or string naming the top-level per-message handler `(handler msg) ->
/// continue?`. Returns the new actor's PID. See [`primop_spawn_activation`].
pub fn b_beam_spawn_activation(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("spawn-activation", args, 2)?;
    let source = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => {
            return Err(format!(
                "spawn-activation: SOURCE must be a string, got {}",
                other.type_name()
            ));
        }
    };
    let handler = value_to_str(&args[1], syms, "spawn-activation")?;
    let pid = primop_spawn_activation(source, handler)?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

/// `(self)` — return the calling actor's PID as a symbol.
/// Errors if called from outside an actor body.
pub fn b_beam_self(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("self", args, 0)?;
    let pid = with_current_actor(|a| a.self_ref().pid())
        .ok_or_else(|| "self: not inside an actor body".to_string())?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

// ---- Supervision primops (lib/beam/prelude.scm bridges these
// to the user-facing `(link)`, `(monitor)`, `(unlink)`,
// `(demonitor)`, `(trap-exit!)`) ----

/// `(system-link! pid)` — bidirectional link from calling
/// actor to `pid`. When either dies, the other gets a
/// `Message::Exit` (delivered as `'(*exit* <from> <reason>)`
/// to a trap-exit actor, fatal otherwise).
///
/// Returns `#t` on success, raises if not inside an actor or
/// target is dead.
pub fn b_beam_system_link(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("system-link!", args, 1)?;
    let target = value_to_pid(&args[0], syms, "system-link!")?;
    let res = with_current_actor(|a| a.link(target))
        .ok_or_else(|| "system-link!: not inside an actor body".to_string())?;
    res.map_err(|e| format!("system-link!: {e}"))?;
    Ok(Value::Boolean(true))
}

/// `(system-unlink! pid)` — tear down a link. Silent no-op if
/// no link existed. Returns `#t`.
pub fn b_beam_system_unlink(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("system-unlink!", args, 1)?;
    let target = value_to_pid(&args[0], syms, "system-unlink!")?;
    with_current_actor(|a| a.unlink(target))
        .ok_or_else(|| "system-unlink!: not inside an actor body".to_string())?;
    Ok(Value::Boolean(true))
}

/// `(system-monitor! pid)` — one-way monitor. Returns the
/// fresh ref_id as a fixnum the caller passes to
/// `(system-demonitor! pid ref)` to cancel. When `pid` dies,
/// the calling actor's mailbox receives
/// `'(*down* <ref-id> <pid> <reason>)`.
pub fn b_beam_system_monitor(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("system-monitor!", args, 1)?;
    let target = value_to_pid(&args[0], syms, "system-monitor!")?;
    let ref_id = with_current_actor(|a| a.monitor(target))
        .ok_or_else(|| "system-monitor!: not inside an actor body".to_string())?
        .map_err(|e| format!("system-monitor!: {e}"))?;
    Ok(Value::Fixnum(ref_id as i64))
}

/// `(system-demonitor! pid ref-id)` — cancel a prior monitor.
/// Silent no-op if either argument no longer matches. Returns
/// `#t`.
pub fn b_beam_system_demonitor(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("system-demonitor!", args, 2)?;
    let target = value_to_pid(&args[0], syms, "system-demonitor!")?;
    let ref_id = match &args[1] {
        Value::Fixnum(n) if *n >= 0 => *n as u64,
        other => {
            return Err(format!(
                "system-demonitor!: ref_id must be a non-negative integer, got {}",
                other.type_name()
            ))
        }
    };
    with_current_actor(|a| a.demonitor(target, ref_id))
        .ok_or_else(|| "system-demonitor!: not inside an actor body".to_string())?;
    Ok(Value::Boolean(true))
}

/// `(system-trap-exit! enabled?)` — set the calling actor's
/// trap-exit flag. When enabled, incoming `Exit` messages
/// arrive as `'(*exit* <from> <reason>)` user messages
/// instead of terminating the actor. Returns the previous
/// value (`#t` or `#f`).
pub fn b_beam_system_trap_exit(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("system-trap-exit!", args, 1)?;
    let enabled = match &args[0] {
        Value::Boolean(b) => *b,
        other => {
            return Err(format!(
                "system-trap-exit!: expected #t or #f, got {}",
                other.type_name()
            ))
        }
    };
    let prev = with_current_actor(|a| a.trap_exit(enabled))
        .ok_or_else(|| "system-trap-exit!: not inside an actor body".to_string())?;
    Ok(Value::Boolean(prev))
}

/// `(reductions)` — return the calling actor's current
/// reduction count. Erlang-flavored: a proxy for "work done"
/// the scheduler tracks per actor. B3's scheduler-swap half
/// (post-1.0) will use this as a yield-check threshold.
pub fn b_beam_reductions(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("reductions", args, 0)?;
    let n = REDUCTIONS.with(|c| c.get());
    Ok(Value::Fixnum(n as i64))
}

/// `(bump-reductions! n)` — add `n` to the calling actor's
/// reduction counter. Returns the new value.
///
/// User code calls this between logical work units; eventually
/// the bytecode dispatch loop's yield-check hook will do it
/// automatically (B3 second half, post-1.0).
pub fn b_beam_bump_reductions(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("bump-reductions!", args, 1)?;
    let n = match &args[0] {
        Value::Fixnum(n) if *n >= 0 => *n as u64,
        other => {
            return Err(format!(
                "bump-reductions!: expected non-negative integer, got {}",
                other.type_name()
            ))
        }
    };
    let new = REDUCTIONS.with(|c| {
        let v = c.get().saturating_add(n);
        c.set(v);
        v
    });
    Ok(Value::Fixnum(new as i64))
}

/// `(yield)` — cooperative hand-off. Calls
/// `std::thread::yield_now()` (asking the OS scheduler to give
/// another thread a chance) and resets this actor's reduction
/// counter to zero. Returns unspecified.
///
/// In the current spawn-blocking model, kernel preemption
/// already keeps actors fair across cores; `(yield)` is the
/// hook for opt-in cooperative behavior and for the post-1.0
/// scheduler-swap where worker threads juggle many actors via
/// reduction-counted slices.
pub fn b_beam_yield(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("yield", args, 0)?;
    // The reduction-count gate is conceptually per-actor; only
    // make the reset meaningful when we're actually inside an
    // actor body. Outside, `(yield)` is still legal (a no-op
    // hand-off) so user code can call it unconditionally.
    if ACTOR_CTX.with(|c| !c.get().is_null()) {
        REDUCTIONS.with(|c| c.set(0));
    }
    std::thread::yield_now();
    Ok(Value::Unspecified)
}

/// Sleep `ms` milliseconds, the right way for the calling context.
///
/// Inside a coroutine-driven `spawn-activation` handler — a [`YIELDER`] is
/// installed — this **suspends** the handler back to [`drive_handler`], which
/// parks on a tokio timer (so co-located actors on the shared LocalSet worker
/// keep running) and resumes the handler when the timer fires. `suspend`
/// returns `()` (the coroutine's `Input`) on resume.
///
/// Everywhere else — non-actor code (scripts, the main thread) and
/// dedicated-thread `spawn`/`spawn-source` actors (one `block_in_place` thread
/// each, where blocking starves no peer) — it is a plain `thread::sleep`, which
/// is correct.
///
/// (The crab-cache Raft-poller lesson is orthogonal: a poller's idle loop often
/// doubles as a tick clock + sole I/O drainer, so *any* real wait there slows
/// the protocol — that code keeps `(yield)` by choice, not because sleep can no
/// longer cooperate.)
/// Cooperative-sleep hook for cs-stdlib-time's `sleep-ms` (installed at runtime
/// startup) and the basis of beam's own `(sleep)`. If a coroutine driver is
/// active on this thread (a `YIELDER` is installed — i.e. we are inside a
/// `spawn-activation` handler), park the caller by suspending the coroutine
/// onto [`drive_handler`]'s timer and return `true`. Otherwise return `false`
/// so the caller does a normal blocking `thread::sleep`.
pub fn cooperative_sleep_hook(ms: u64) -> bool {
    let yielder = YIELDER.with(|c| c.get());
    if yielder.is_null() {
        false
    } else {
        // Sound: we're executing *inside* the coroutine whose `Yielder` this is
        // (it lives for the coroutine's life, on this thread), and
        // `drive_handler` re-installs `YIELDER` before every resume.
        unsafe { (*yielder).suspend(CoYield::Sleep(Duration::from_millis(ms))) };
        true
    }
}

fn do_sleep(ms: u64) {
    if ms == 0 {
        return;
    }
    if !cooperative_sleep_hook(ms) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// `(sleep-ms n)` — sleep for `n` milliseconds (non-negative integer).
/// Returns unspecified; a zero-duration call returns immediately.
///
/// Cooperative inside a `spawn-activation` handler (suspends the evaluator and
/// parks on a timer, so co-located actors run); a blocking `thread::sleep`
/// everywhere else. See [`do_sleep`].
pub fn b_beam_sleep_ms(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("sleep-ms", args, 1)?;
    let ms = match &args[0] {
        Value::Fixnum(n) if *n >= 0 => *n as u64,
        Value::Fixnum(n) => {
            return Err(format!(
                "sleep-ms: duration must be non-negative, got {}",
                n
            ))
        }
        other => {
            return Err(format!(
                "sleep-ms: expected non-negative integer, got {}",
                other.type_name()
            ))
        }
    };
    do_sleep(ms);
    Ok(Value::Unspecified)
}

/// `(sleep secs)` — sleep for `secs` seconds (fixnum or flonum,
/// non-negative). Fractional seconds are supported. Returns
/// unspecified. Zero-duration call returns immediately.
///
/// Same execution model as [`b_beam_sleep_ms`] (cooperative inside an
/// activation handler via [`do_sleep`], blocking elsewhere).
pub fn b_beam_sleep(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("sleep", args, 1)?;
    let secs_f: f64 = match &args[0] {
        Value::Fixnum(n) if *n >= 0 => *n as f64,
        Value::Fixnum(n) => return Err(format!("sleep: duration must be non-negative, got {}", n)),
        Value::Flonum(f) if *f >= 0.0 => *f,
        Value::Flonum(f) => return Err(format!("sleep: duration must be non-negative, got {}", f)),
        other => {
            return Err(format!(
                "sleep: expected non-negative number, got {}",
                other.type_name()
            ))
        }
    };
    if secs_f > 0.0 {
        do_sleep((secs_f * 1000.0).round() as u64);
    }
    Ok(Value::Unspecified)
}

/// Cooperative `(raw-receive)`: if a coroutine driver is active on this thread
/// (a [`YIELDER`] is installed — i.e. we are inside a `spawn-activation`
/// handler), suspend with a `Recv` request so [`drive_handler`] parks on the
/// async mailbox (letting co-located actors run) and resumes us with the result.
/// Returns `None` if there is no driver, so the caller falls back to the
/// blocking [`primop_raw_receive`] (correct for non-actor code and
/// dedicated-thread `spawn`/`spawn-source` actors, which own their thread).
fn cooperative_raw_receive(
    timeout_ms: Option<u64>,
) -> Option<Result<Option<SendableValue>, String>> {
    let yielder = YIELDER.with(|c| c.get());
    if yielder.is_null() {
        return None;
    }
    // Sound: see `cooperative_sleep_hook` — we're inside the coroutine whose
    // `Yielder` this is, on its thread, and the driver re-installs `YIELDER`
    // before resuming us.
    match unsafe {
        (*yielder).suspend(CoYield::Recv {
            timeout: timeout_ms,
        })
    } {
        CoResume::Received(result) => Some(result),
        // Any other resume value for a Recv is a driver bug.
        _ => Some(Err(
            "raw-receive: internal error (resumed without a message)".into(),
        )),
    }
}

/// Cooperative `(tcp-recv handle max)`: if a coroutine driver is active on this
/// thread (a [`YIELDER`] is installed), suspend with an [`CoYield::Io`] request so
/// the driver does the read on a tokio stream (releasing the worker) and resumes
/// us with the bytes. `None` when there is no driver, so cs-stdlib-net falls back
/// to its blocking read (dedicated thread / non-actor). Installed into
/// cs-stdlib-net via [`cs_stdlib_net::install_async_recv`].
#[cfg(feature = "stdlib-net")]
pub fn cooperative_tcp_recv_hook(handle: i64, max: usize) -> Option<Result<Vec<u8>, String>> {
    let yielder = YIELDER.with(|c| c.get());
    if yielder.is_null() {
        return None;
    }
    // Sound: see `cooperative_sleep_hook`.
    match unsafe { (*yielder).suspend(CoYield::Io { handle, max }) } {
        CoResume::Io(res) => Some(res),
        _ => Some(Err(
            "tcp-recv: internal error (resumed without bytes)".into()
        )),
    }
}

/// Cooperative `(tcp-send handle bytes)` — the write counterpart of
/// [`cooperative_tcp_recv_hook`]. Required on the green path: the cooperative
/// recv set the shared fd non-blocking, so a blocking `write_all` would
/// `WouldBlock`. Installed via [`cs_stdlib_net::install_async_send`].
#[cfg(feature = "stdlib-net")]
pub fn cooperative_tcp_send_hook(handle: i64, bytes: &[u8]) -> Option<Result<(), String>> {
    let yielder = YIELDER.with(|c| c.get());
    if yielder.is_null() {
        return None;
    }
    // Sound: see `cooperative_sleep_hook`. `bytes` is copied into the CoYield
    // because the slice can't outlive this call across the suspend.
    match unsafe {
        (*yielder).suspend(CoYield::IoWrite {
            handle,
            bytes: bytes.to_vec(),
        })
    } {
        CoResume::IoWrite(res) => Some(res),
        _ => Some(Err(
            "tcp-send: internal error (resumed without an ack)".into()
        )),
    }
}

/// `(raw-receive)` blocks until a message arrives;
/// `(raw-receive timeout-ms)` returns `'*timeout*` if the deadline
/// passes without one. System messages (Exit/Down) surface as
/// tagged lists the Scheme `(receive)` macro can pattern-match.
/// Cooperative inside an activation handler (see [`cooperative_raw_receive`]).
pub fn b_beam_raw_receive(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    let timeout_ms = match args.len() {
        0 => None,
        1 => match &args[0] {
            Value::Boolean(false) => None,
            Value::Fixnum(n) if *n >= 0 => Some(*n as u64),
            other => {
                return Err(format!(
                    "raw-receive: timeout must be #f or a non-negative integer, got {}",
                    other.type_name()
                ))
            }
        },
        n => return Err(format!("raw-receive: expected 0 or 1 arguments, got {}", n)),
    };

    let outcome = match cooperative_raw_receive(timeout_ms) {
        Some(result) => result,
        None => with_current_actor(|a| primop_raw_receive(a, timeout_ms))
            .ok_or_else(|| "raw-receive: not inside an actor body".to_string())?,
    }?;

    match outcome {
        Some(sv) => Ok(from_sendable(&sv, syms)),
        // Timeout: return the symbol `'*timeout*` instead of
        // `#f`. Pre-fix, raw-receive returned `#f` which
        // collided with a legitimate `(send pid #f)` payload —
        // the receiver couldn't distinguish "timed out" from
        // "got #f". The `*timeout*` symbol matches the
        // existing `*exit*` / `*down*` system-message tag
        // convention; the `(timeout? msg)` predicate in
        // lib/beam/prelude.scm normalizes the check.
        None => Ok(Value::Symbol(syms.intern("*timeout*"))),
    }
}

/// Walk a proper Scheme list, returning `None` if it isn't
/// proper (improper tail or atom encountered).
fn proper_list(v: &Value) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return Some(out),
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            _ => return None,
        }
    }
}

/// `(load-module! 'name '(("export" . value) ...))` — register
/// (or re-register) a module's exports. Returns the new
/// current version's epoch as a fixnum.
pub fn b_beam_load_module(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("load-module!", args, 2)?;
    let module = value_to_str(&args[0], syms, "load-module!")?;
    let pairs = proper_list(&args[1])
        .ok_or_else(|| "load-module!: second arg must be an alist of (name . value)".to_string())?;
    let mut exports: cs_hotreload::ExportsMap<String, SendableValue> = Default::default();
    for entry in pairs {
        match entry {
            Value::Pair(p) => {
                let k = value_to_str(&p.car.borrow(), syms, "load-module!")?;
                let v = to_sendable_in(&p.cdr.borrow(), syms)?;
                exports.insert(k, v);
            }
            other => {
                return Err(format!(
                    "load-module!: alist entries must be pairs, got {}",
                    other.type_name()
                ))
            }
        }
    }
    let epoch = primop_load_module(&module, exports);
    Ok(Value::Fixnum(epoch as i64))
}

/// `(lookup-code 'module "export")` — current version. Returns
/// the export value or `#f` if missing.
pub fn b_beam_lookup_code(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("lookup-code", args, 2)?;
    let module = value_to_str(&args[0], syms, "lookup-code")?;
    let name = value_to_str(&args[1], syms, "lookup-code")?;
    match primop_lookup_code(&module, &name) {
        Some(sv) => Ok(from_sendable(&sv, syms)),
        None => Ok(Value::Boolean(false)),
    }
}

/// `(lookup-code-old 'module "export")` — pre-reload version.
/// Returns `#f` for modules without a prior version.
pub fn b_beam_lookup_code_old(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("lookup-code-old", args, 2)?;
    let module = value_to_str(&args[0], syms, "lookup-code-old")?;
    let name = value_to_str(&args[1], syms, "lookup-code-old")?;
    match primop_lookup_code_old(&module, &name) {
        Some(sv) => Ok(from_sendable(&sv, syms)),
        None => Ok(Value::Boolean(false)),
    }
}

/// `(code-soft-purge! 'module holder-count)` — drop the old
/// version if no actor is pinned to it. Raises with a clear
/// error if the count is non-zero.
pub fn b_beam_code_soft_purge(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("code-soft-purge!", args, 2)?;
    let module = value_to_str(&args[0], syms, "code-soft-purge!")?;
    let count = match &args[1] {
        Value::Fixnum(n) if *n >= 0 => *n as usize,
        other => {
            return Err(format!(
                "code-soft-purge!: holder-count must be a non-negative integer, got {}",
                other.type_name()
            ))
        }
    };
    primop_code_soft_purge(&module, count)?;
    Ok(Value::Unspecified)
}

/// `(code-purge! 'module)` — force-drop the old version.
pub fn b_beam_code_purge(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("code-purge!", args, 1)?;
    let module = value_to_str(&args[0], syms, "code-purge!")?;
    primop_code_purge(&module)?;
    Ok(Value::Unspecified)
}

/// `(code-versions 'module)` — returns `'(old-epoch current-epoch)`
/// where each side is a fixnum or `#f`. Returns `#f` if the
/// module isn't loaded.
pub fn b_beam_code_versions(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("code-versions", args, 1)?;
    let module = value_to_str(&args[0], syms, "code-versions")?;
    match primop_code_versions(&module) {
        Some((old, cur)) => {
            let epoch_v = |o: Option<u32>| match o {
                Some(e) => Value::Fixnum(e as i64),
                None => Value::Boolean(false),
            };
            let tail = Pair::new(epoch_v(cur), Value::Null);
            Ok(Value::Pair(Pair::new(epoch_v(old), Value::Pair(tail))))
        }
        None => Ok(Value::Boolean(false)),
    }
}

/// `(code-modules)` — proper list of loaded module names as
/// symbols.
pub fn b_beam_code_modules(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("code-modules", args, 0)?;
    let mods = primop_code_modules();
    let mut acc = Value::Null;
    for name in mods.into_iter().rev() {
        let sym = Value::Symbol(syms.intern(&name));
        acc = Value::Pair(Pair::new(sym, acc));
    }
    Ok(acc)
}

/// `(code-unload! 'module)` — drop both versions. Idempotent
/// for missing modules.
pub fn b_beam_code_unload(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("code-unload!", args, 1)?;
    let module = value_to_str(&args[0], syms, "code-unload!")?;
    primop_code_unload(&module);
    Ok(Value::Unspecified)
}

/// The list of Scheme-facing BEAM builtins, in the
/// `(name, fn)` shape `syms_builtins()` accepts. cs-runtime
/// merges this into its `install_into` registration loop when
/// the `actor` feature is on.
pub fn beam_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    vec![
        // actor
        ("send", b_beam_send),
        ("spawn", b_beam_spawn),
        ("spawn-source", b_beam_spawn_source),
        ("spawn-source-green", b_beam_spawn_source_green),
        ("spawn-source-dedicated", b_beam_spawn_source_dedicated),
        ("spawn-activation", b_beam_spawn_activation),
        ("self", b_beam_self),
        ("raw-receive", b_beam_raw_receive),
        // reductions (B3 first half: cooperative-yield seam)
        ("reductions", b_beam_reductions),
        ("bump-reductions!", b_beam_bump_reductions),
        ("yield", b_beam_yield),
        // timed sleep (releases OS thread for the duration)
        ("sleep-ms", b_beam_sleep_ms),
        ("sleep", b_beam_sleep),
        // supervision (bridges to lib/beam/prelude.scm's
        // user-facing (link), (monitor), (unlink), (demonitor),
        // (trap-exit!) wrappers)
        ("system-link!", b_beam_system_link),
        ("system-unlink!", b_beam_system_unlink),
        ("system-monitor!", b_beam_system_monitor),
        ("system-demonitor!", b_beam_system_demonitor),
        ("system-trap-exit!", b_beam_system_trap_exit),
        // table
        ("make-table", b_beam_make_table),
        ("table-insert!", b_beam_table_insert),
        ("table-lookup", b_beam_table_lookup),
        ("table-delete!", b_beam_table_delete),
        ("table-size", b_beam_table_size),
        ("table-get-resp-bulk", b_beam_table_get_resp_bulk),
        ("conn-serve-gets", b_beam_conn_serve_gets),
        ("conn-serve-batch", b_beam_conn_serve_batch),
        ("native-classify-route", b_beam_native_classify_route),
        // hot-reload
        ("load-module!", b_beam_load_module),
        ("lookup-code", b_beam_lookup_code),
        ("lookup-code-old", b_beam_lookup_code_old),
        ("code-soft-purge!", b_beam_code_soft_purge),
        ("code-purge!", b_beam_code_purge),
        ("code-versions", b_beam_code_versions),
        ("code-modules", b_beam_code_modules),
        ("code-unload!", b_beam_code_unload),
    ]
}

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
            Value::Fixnum(42),
            Value::Flonum(3.14),
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
        let v = Value::string("hello");
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
        let a = Pair::new(Value::Fixnum(3), Value::Null);
        let b = Pair::new(Value::Fixnum(2), Value::Pair(a));
        let lst = Value::Pair(Pair::new(Value::Fixnum(1), Value::Pair(b)));

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
            Value::Fixnum(1),
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
        let v = Value::Pair(Pair::new(Value::Fixnum(7), Value::Boolean(false)));
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
    fn spawn_activation_accumulates_persistent_state() {
        // A parking activation actor (#30 iter-2a): the framework owns the
        // receive loop and calls `handler` once per message. The handler keeps
        // a running total in its own persistent top-level binding ACROSS
        // separate activations — proving the `!Send` Scheme heap survives the
        // `receive_async().await` between messages. On `stop` it forwards the
        // final total to a collector actor (a registered Rust proc) that
        // records it where the test thread can read it.
        let result: Arc<std::sync::Mutex<Option<SendableValue>>> =
            Arc::new(std::sync::Mutex::new(None));
        let result_clone = result.clone();
        beam_state().procs.register(
            "test:collect-total",
            Arc::new(move |actor, _args| {
                if let Ok(Some(msg)) = primop_raw_receive(actor, None) {
                    *result_clone.lock().unwrap() = Some(msg);
                }
            }),
        );
        let collector = primop_spawn("test:collect-total", vec![]).expect("spawn collector");

        let source = r#"
            (define collector #f)
            (define total 0)
            (define (handler msg)
              (cond
                ((and (pair? msg) (eq? (car msg) 'collector))
                 (set! collector (cdr msg)) #t)
                ((eq? msg 'stop)
                 (send collector total)
                 #f)
                (else
                 (set! total (+ total msg))
                 #t)))
        "#;
        let pid = primop_spawn_activation(source.to_string(), "handler".to_string())
            .expect("spawn-activation");

        // Tell the actor where to report, feed it 1..=5 (sum 15), then stop.
        // The Fast mailbox is FIFO, so the collector pid lands before the
        // numbers and `stop` lands last.
        primop_send(
            pid,
            SendableValue::Pair(
                Box::new(SendableValue::Symbol("collector".into())),
                Box::new(SendableValue::Pid(collector)),
            ),
        )
        .expect("send collector pid");
        for n in 1..=5 {
            primop_send(pid, SendableValue::Fixnum(n)).expect("send n");
        }
        primop_send(pid, SendableValue::Symbol("stop".into())).expect("send stop");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if result.lock().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("activation actor never reported its total");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(*result.lock().unwrap(), Some(SendableValue::Fixnum(15)));
    }

    #[test]
    fn multiple_activation_actors_multiplex_on_the_pool() {
        // Several parking activation actors coexist on the shared LocalSet
        // worker pool (more actors than workers) and all complete. Each is
        // told its id + the collector pid, replies `id * 10` on `go`, then
        // stops. The collector (a registered Rust proc) gathers all N replies.
        let n: i64 = 8;
        let got: Arc<std::sync::Mutex<Vec<SendableValue>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let got_clone = got.clone();
        beam_state().procs.register(
            "test:collect-n",
            Arc::new(move |actor, _args| {
                for _ in 0..n {
                    match primop_raw_receive(actor, None) {
                        Ok(Some(msg)) => got_clone.lock().unwrap().push(msg),
                        _ => break,
                    }
                }
            }),
        );
        let collector = primop_spawn("test:collect-n", vec![]).expect("spawn collector");

        let source = r#"
            (define collector #f)
            (define id #f)
            (define (handler msg)
              (cond
                ((and (pair? msg) (eq? (car msg) 'init))
                 (set! collector (cadr msg))
                 (set! id (caddr msg))
                 #t)
                ((eq? msg 'go)
                 (send collector (* id 10))
                 #f)
                (else #t)))
        "#;

        for i in 0..n {
            let pid = primop_spawn_activation(source.to_string(), "handler".to_string())
                .expect("spawn-activation");
            // (list 'init <collector-pid> i)
            let init = SendableValue::Pair(
                Box::new(SendableValue::Symbol("init".into())),
                Box::new(SendableValue::Pair(
                    Box::new(SendableValue::Pid(collector)),
                    Box::new(SendableValue::Pair(
                        Box::new(SendableValue::Fixnum(i)),
                        Box::new(SendableValue::Null),
                    )),
                )),
            );
            primop_send(pid, init).expect("send init");
            primop_send(pid, SendableValue::Symbol("go".into())).expect("send go");
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if got.lock().unwrap().len() == n as usize {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "only {} of {n} activation actors reported",
                    got.lock().unwrap().len()
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // Replies are id*10 for id in 0..n, in any arrival order — sum them.
        let sum: i64 = got
            .lock()
            .unwrap()
            .iter()
            .map(|v| match v {
                SendableValue::Fixnum(k) => *k,
                _ => panic!("unexpected reply {v:?}"),
            })
            .sum();
        assert_eq!(sum, (0..n).map(|i| i * 10).sum::<i64>());
    }

    #[test]
    fn spawn_source_datum_rendering() {
        // The arg bridge: a SendableValue must render to an external datum the
        // reader reconstructs identically. (build_call_expr quotes each arg.)
        fn d(s: &SendableValue) -> String {
            let mut out = String::new();
            sendable_to_datum(s, &mut out).expect("datum");
            out
        }
        assert_eq!(d(&SendableValue::Fixnum(42)), "42");
        assert_eq!(d(&SendableValue::Boolean(true)), "#t");
        assert_eq!(d(&SendableValue::Boolean(false)), "#f");
        assert_eq!(d(&SendableValue::Null), "()");
        assert_eq!(d(&SendableValue::Symbol("foo".into())), "foo");
        assert_eq!(d(&SendableValue::String("a\"b".into())), "\"a\\\"b\"");
        assert_eq!(d(&SendableValue::Flonum(1.0)), "1.0"); // never reads back as a fixnum
        assert_eq!(d(&SendableValue::Flonum(1.5)), "1.5");

        // proper list (set "k" 7)
        let lst = SendableValue::Pair(
            Box::new(SendableValue::Symbol("set".into())),
            Box::new(SendableValue::Pair(
                Box::new(SendableValue::String("k".into())),
                Box::new(SendableValue::Pair(
                    Box::new(SendableValue::Fixnum(7)),
                    Box::new(SendableValue::Null),
                )),
            )),
        );
        assert_eq!(d(&lst), "(set \"k\" 7)");

        // improper pair (a . b) and a vector
        let dotted = SendableValue::Pair(
            Box::new(SendableValue::Symbol("a".into())),
            Box::new(SendableValue::Symbol("b".into())),
        );
        assert_eq!(d(&dotted), "(a . b)");
        assert_eq!(
            d(&SendableValue::Vector(vec![
                SendableValue::Fixnum(1),
                SendableValue::Fixnum(2)
            ])),
            "#(1 2)"
        );

        // the call expr quotes each argument
        let call = build_call_expr(
            "main",
            &[SendableValue::Symbol("x".into()), SendableValue::Fixnum(3)],
        )
        .unwrap();
        assert_eq!(call, "(main 'x '3)");

        // values with no readable datum are rejected at the boundary
        let mut sink = String::new();
        assert!(sendable_to_datum(&SendableValue::Unspecified, &mut sink).is_err());
    }

    #[test]
    fn spawn_source_runs_scheme_body() {
        // First end-to-end proof that a *Scheme* procedure runs as an actor
        // body on its own worker thread (the !Send bridge). The body squares
        // the number it receives and writes the result into the process-global
        // table — the cross-thread channel the test thread reads. No PID
        // round-trip, no Rust-registered closure: the logic is Scheme.
        let table = "spawn-src-sq-test";
        primop_make_table(table, "set").expect("make table");

        let body = r#"
            (define (squarer key)
              (let ((n (raw-receive)))
                (table-insert! 'spawn-src-sq-test key (* n n))))
        "#;

        let pid = primop_spawn_source(
            body.to_string(),
            "squarer".to_string(),
            vec![SendableValue::String("sq".into())],
        )
        .expect("spawn-source");
        primop_send(pid, SendableValue::Fixnum(9)).expect("send");

        // Building a per-actor Runtime (full stdlib) then evaluating takes a
        // moment in debug; poll generously.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let got = primop_table_lookup(table, &SendableValue::String("sq".into()))
                .expect("table lookup");
            if let Some(v) = got {
                assert_eq!(v, SendableValue::Fixnum(81));
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("scheme actor body never wrote its result");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn spawn_source_vm_tier_helper_and_mutable_state() {
        // Stage-1 (VM+JIT actor bodies) guards two risks of the walker->VM
        // switch in `run_scheme_body`:
        // (1) cross-eval visibility — the body's top-level defs are loaded with
        //     one `eval_str_via_vm`, the `(run …)` entry call with a SECOND, and
        //     the entry must see the separately-defined `double` helper; `lookup`
        //     must find the entry in `vm_env` (walker `top` is empty on VM).
        // (2) the const-folding `set!` hazard — a mutable top-level (`acc`) the
        //     body `set!`s and re-reads must not be folded to a stale Const.
        let table = "spawn-src-vm-test";
        primop_make_table(table, "set").expect("make table");

        let body = r#"
            (define (double x) (* 2 x))
            (define acc 0)
            (define (run key)
              (set! acc (+ acc (double (raw-receive))))
              (set! acc (+ acc (double (raw-receive))))
              (table-insert! 'spawn-src-vm-test key acc))
        "#;

        let pid = primop_spawn_source(
            body.to_string(),
            "run".to_string(),
            vec![SendableValue::String("r".into())],
        )
        .expect("spawn-source");
        primop_send(pid, SendableValue::Fixnum(5)).expect("send 5");
        primop_send(pid, SendableValue::Fixnum(8)).expect("send 8");

        // acc = double(5) + double(8) = 10 + 16 = 26.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(v) =
                primop_table_lookup(table, &SendableValue::String("r".into())).expect("lookup")
            {
                assert_eq!(v, SendableValue::Fixnum(26));
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("vm-tier actor body never wrote its result");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn spawn_source_green_runs_whole_body_loop() {
        // P1.2 gate: a free-form green spawn-source body with its OWN
        // (raw-receive) loop runs on the LocalSet pool and parks between
        // receives — no body or primop change vs the dedicated path (the spec
        // §0.2 insight: the body's own (raw-receive) already routes through the
        // YIELDER-gated cooperative hook, which green_source_body publishes).
        // The body sums three received numbers in a self-recursive loop, then
        // writes the total to the global table (the cross-thread channel).
        let table = "spawn-src-green-loop-test";
        primop_make_table(table, "set").expect("make table");

        let body = r#"
            (define (summer key)
              (let loop ((remaining 3) (acc 0))
                (if (= remaining 0)
                    (table-insert! 'spawn-src-green-loop-test key acc)
                    (loop (- remaining 1) (+ acc (raw-receive))))))
        "#;

        let pid = primop_spawn_source_green(
            body.to_string(),
            "summer".to_string(),
            vec![SendableValue::String("s".into())],
        )
        .expect("spawn-source-green");
        primop_send(pid, SendableValue::Fixnum(10)).expect("send 10");
        primop_send(pid, SendableValue::Fixnum(20)).expect("send 20");
        primop_send(pid, SendableValue::Fixnum(12)).expect("send 12");

        // acc = 10 + 20 + 12 = 42
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(v) =
                primop_table_lookup(table, &SendableValue::String("s".into())).expect("lookup")
            {
                assert_eq!(v, SendableValue::Fixnum(42));
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("green whole-body actor never wrote its result");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn same_worker_send_deep_clones_not_aliases() {
        // cs-845.1: two spawn-source-green actors colocated on the same
        // LocalWorkerPool worker take the fast same-worker send path
        // (`try_send_same_worker`) instead of the SendableValue
        // projection round-trip. Prove it preserves copy semantics: the
        // sender mutates its list *after* sending it, and the receiver's
        // copy must be unaffected.
        let table = "same-worker-send-test";
        primop_make_table(table, "set").expect("make table");

        // The receiver just files whatever it's handed under the given
        // key — a LocalWorkerPool actor so it's eligible for colocation.
        let receiver_body = r#"
            (define (recv)
              (let loop ()
                (let ((msg (raw-receive)))
                  (table-insert! 'same-worker-send-test (car msg) (cdr msg))
                  (loop))))
        "#;
        let receiver =
            primop_spawn_source_green(receiver_body.to_string(), "recv".to_string(), vec![])
                .expect("spawn receiver");

        // The sender: waits to be handed the receiver's pid (a PID can't
        // ride as a spawn arg — the datum bridge rejects it — so it
        // arrives as a message, same as `ping_pong_two_actors` below),
        // then builds a mutable list, sends it, and mutates the ORIGINAL
        // *after* sending. If the fast path aliased instead of
        // deep-cloning, the receiver would observe the post-mutation
        // value (1 999 3) instead of the pre-send value (1 2 3). Also a
        // `spawn-source-green` (LocalWorkerPool) actor, so it's eligible
        // for colocation with `receiver`.
        // Fixnum table keys (1 = pre-mutation copy, 2 = post-mutation
        // original) keep the messages symbol-free, so the fast path never
        // bails on a per-actor extension symbol — letting the counter
        // assertion below prove the fast path really fired.
        let sender_body = r#"
            (define (run)
              (let ((rpid (raw-receive)))
                (let ((original (list 1 2 3)))
                  (send rpid (cons 1 original))
                  (set-car! (cdr original) 999)
                  (send rpid (cons 2 original)))))
        "#;

        // Find a sender colocated with `receiver` by pigeonhole: spawn
        // candidates (round-robin dispatched) until one lands on the same
        // worker. 512 candidates comfortably exceeds any real machine's
        // `available_parallelism()` worker count. Every candidate but the
        // chosen one just parks forever on its `raw-receive` — harmless,
        // torn down at process exit.
        let mut sender = None;
        for _ in 0..512 {
            let candidate =
                primop_spawn_source_green(sender_body.to_string(), "run".to_string(), vec![])
                    .expect("spawn candidate");
            if beam_state().actors.local_worker_of(candidate)
                == beam_state().actors.local_worker_of(receiver)
            {
                sender = Some(candidate);
                break;
            }
        }
        let sender = sender.expect("failed to find a worker-colocated sender candidate");
        let fast_sends_before = same_worker_fast_send_count();
        primop_send(sender, SendableValue::Pid(receiver)).expect("kick off sender");

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let copy = primop_table_lookup(table, &SendableValue::Fixnum(1)).expect("lookup copy");
            let mutated = primop_table_lookup(table, &SendableValue::Fixnum(2))
                .expect("lookup mutated-original");
            if let (Some(copy), Some(mutated)) = (copy, mutated) {
                let list123 = SendableValue::Pair(
                    Box::new(SendableValue::Fixnum(1)),
                    Box::new(SendableValue::Pair(
                        Box::new(SendableValue::Fixnum(2)),
                        Box::new(SendableValue::Pair(
                            Box::new(SendableValue::Fixnum(3)),
                            Box::new(SendableValue::Null),
                        )),
                    )),
                );
                assert_eq!(
                    copy, list123,
                    "receiver's copy must reflect the list as it was AT SEND TIME"
                );
                assert_ne!(
                    mutated, list123,
                    "sanity: the original really was mutated after sending"
                );
                // Both sends were colocated + symbol-free, so both must
                // have taken the fast path (>= tolerates other actor
                // tests bumping the process-wide counter concurrently).
                assert!(
                    same_worker_fast_send_count() >= fast_sends_before + 2,
                    "expected the colocated sends to take the same-worker fast path"
                );
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("same-worker send/receive never completed");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // cs-845.1 judge fixes: direct unit coverage of the fast-path internals
    // (no actor runtime needed — these exercise the thread-local side-queue,
    // the base-symbol gating, and the RAII guard on the calling thread).

    fn as_fixnum(v: &Value) -> i64 {
        match v {
            Value::Fixnum(n) => *n,
            other => panic!("expected fixnum, got {}", other.type_name()),
        }
    }

    #[test]
    fn deep_clone_bails_on_nested_non_base_symbol() {
        // A symbol that resolves through the shared Rc base is safe to copy
        // verbatim (`is_base` → true); one interned into a table's private
        // extension is not, and must force the whole clone to bail (`Ok(None)`)
        // even when buried inside a pair or vector.
        let base = std::rc::Rc::new({
            let mut t = SymbolTable::new();
            t.intern("shared-base-sym");
            t
        });
        let mut ext = SymbolTable::with_base(base);
        let base_sym = ext.intern("shared-base-sym"); // resolves in base
        let priv_sym = ext.intern("actor-private-sym"); // extension-only
        assert!(ext.is_base(base_sym));
        assert!(!ext.is_base(priv_sym));

        // Bare base symbol: fast path is safe.
        assert!(matches!(
            deep_clone_same_worker(&Value::Symbol(base_sym), &ext),
            Ok(Some(_))
        ));

        // Non-base symbol NESTED inside a pair: must bail.
        let nested_pair = Value::Pair(Pair::new(
            Value::Symbol(base_sym),
            Value::Pair(Pair::new(Value::Symbol(priv_sym), Value::Null)),
        ));
        assert!(matches!(
            deep_clone_same_worker(&nested_pair, &ext),
            Ok(None)
        ));

        // Non-base symbol NESTED inside a vector: must bail too.
        let nested_vec = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(vec![
            Value::Fixnum(1),
            Value::Symbol(priv_sym),
        ])));
        assert!(matches!(
            deep_clone_same_worker(&nested_vec, &ext),
            Ok(None)
        ));
    }

    #[test]
    fn same_worker_queue_is_fifo() {
        // Multiple fast-path messages parked for one receiver come back out in
        // send order (front-first), with the seq stamps matching the markers.
        let pid = ActorPid {
            node: 0,
            local_id: 0x5A5A_0001,
        };
        for n in [10_i64, 20, 30] {
            let seq = SAME_WORKER_MSGS.with(|m| {
                let mut m = m.borrow_mut();
                let q = m.entry(pid).or_default();
                let seq = q.next_seq;
                q.next_seq += 1;
                seq
            });
            commit_same_worker_msg(pid, seq, Value::Fixnum(n));
        }
        let mut syms = SymbolTable::new();
        assert_eq!(as_fixnum(&take_same_worker_msg(pid, 0, &mut syms)), 10);
        assert_eq!(as_fixnum(&take_same_worker_msg(pid, 1, &mut syms)), 20);
        assert_eq!(as_fixnum(&take_same_worker_msg(pid, 2, &mut syms)), 30);
        // Queue drained; a further pop degrades to the opaque lost-message
        // symbol rather than panicking.
        let lost = take_same_worker_msg(pid, 3, &mut syms);
        assert!(matches!(&lost, Value::Symbol(s) if syms.name(*s) == "same-worker-message-lost"));
        // Clean up so we don't leave a stray entry for other tests.
        SAME_WORKER_MSGS.with(|m| {
            m.borrow_mut().remove(&pid);
        });
    }

    #[test]
    fn guard_drops_pending_queue_on_actor_death() {
        // An actor that terminates with fast-path mail still queued must not
        // leak it: the RAII guard removes the actor's whole side-queue on drop.
        let pid = ActorPid {
            node: 0,
            local_id: 0x5A5A_0002,
        };
        commit_same_worker_msg(pid, 0, Value::Fixnum(7));
        assert!(SAME_WORKER_MSGS.with(|m| m.borrow().contains_key(&pid)));
        {
            let _guard = SameWorkerGuard(pid);
            // guard live: entry still present.
            assert!(SAME_WORKER_MSGS.with(|m| m.borrow().contains_key(&pid)));
        }
        // guard dropped: entry (and its pending value) gone.
        assert!(!SAME_WORKER_MSGS.with(|m| m.borrow().contains_key(&pid)));
    }

    #[test]
    fn green_source_actors_multiplex_on_the_pool() {
        // P1.3 gate: many whole-body green spawn-source actors coexist on the
        // shared LocalSet pool and all complete — multiplexing, not
        // thread-per-actor (these would blow the 4096 block_in_place ceiling on
        // the dedicated path at scale). Each receives (collector-pid . id),
        // replies id*10, then exits; a registered Rust collector gathers all N.
        let n: i64 = 50;
        let got: Arc<std::sync::Mutex<Vec<SendableValue>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let got_clone = got.clone();
        beam_state().procs.register(
            "test:collect-green",
            Arc::new(move |actor, _args| {
                for _ in 0..n {
                    match primop_raw_receive(actor, None) {
                        Ok(Some(msg)) => got_clone.lock().unwrap().push(msg),
                        _ => break,
                    }
                }
            }),
        );
        let collector = primop_spawn("test:collect-green", vec![]).expect("spawn collector");

        // PIDs can't be spawn args (rejected by the datum bridge) — deliver the
        // collector pid + id as the first message instead.
        let body = r#"
            (define (worker)
              (let ((msg (raw-receive)))
                (send (car msg) (* (cdr msg) 10))))
        "#;
        for i in 0..n {
            let pid = primop_spawn_source_green(body.to_string(), "worker".to_string(), vec![])
                .expect("spawn-source-green");
            let msg = SendableValue::Pair(
                Box::new(SendableValue::Pid(collector)),
                Box::new(SendableValue::Fixnum(i)),
            );
            primop_send(pid, msg).expect("send");
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            if got.lock().unwrap().len() == n as usize {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "only {} of {n} green source actors reported",
                    got.lock().unwrap().len()
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let sum: i64 = got
            .lock()
            .unwrap()
            .iter()
            .map(|v| match v {
                SendableValue::Fixnum(k) => *k,
                _ => panic!("unexpected reply {v:?}"),
            })
            .sum();
        assert_eq!(sum, (0..n).map(|i| i * 10).sum::<i64>());
    }

    #[test]
    fn spawn_source_vm_sees_bundled_library_prelude() {
        // Regression for the VM-tier actor bug: a spawn-source body runs on the
        // VM tier, and bundled Scheme libraries (loaded by `load_bundled_library`)
        // must be visible there too. Before the both-tier load, `(crab actor)`'s
        // `call` was walker-only, so a body referencing it raised "undefined
        // variable: call" — exactly how crab-cache's conn actor died. The body
        // guards the reference so a miss is a clear `'undefined` rather than a
        // 30 s timeout.
        let table = "spawn-src-bundled-test";
        primop_make_table(table, "set").expect("make table");
        let body = r#"
            (define (run key)
              (table-insert! 'spawn-src-bundled-test key
                (guard (e (#t 'undefined)) (if (procedure? call) 'ok 'not-proc))))
        "#;
        primop_spawn_source(
            body.to_string(),
            "run".to_string(),
            vec![SendableValue::String("c".into())],
        )
        .expect("spawn-source");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(v) =
                primop_table_lookup(table, &SendableValue::String("c".into())).expect("lookup")
            {
                assert_eq!(
                    v,
                    SendableValue::Symbol("ok".into()),
                    "`call` from (crab actor) must resolve to a procedure on the VM tier"
                );
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("bundled-library actor never reported");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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

    // ---- sleep-ms / sleep tests ----

    #[test]
    fn sleep_ms_zero_returns_immediately() {
        let mut syms = SymbolTable::new();
        let start = std::time::Instant::now();
        let v = b_beam_sleep_ms(&[Value::Fixnum(0)], &mut syms).expect("sleep-ms 0");
        assert!(matches!(v, Value::Unspecified));
        // Zero-sleep must not take more than a generous 50ms (scheduler jitter).
        assert!(start.elapsed().as_millis() < 50);
    }

    #[test]
    fn sleep_ms_waits_at_least_requested_duration() {
        let mut syms = SymbolTable::new();
        let start = std::time::Instant::now();
        b_beam_sleep_ms(&[Value::Fixnum(20)], &mut syms).expect("sleep-ms 20");
        // Relax lower bound to 15ms to tolerate OS timer granularity.
        assert!(
            start.elapsed().as_millis() >= 15,
            "elapsed {:?} < 15ms",
            start.elapsed()
        );
    }

    #[test]
    fn sleep_ms_negative_errors() {
        let mut syms = SymbolTable::new();
        let err =
            b_beam_sleep_ms(&[Value::Fixnum(-1)], &mut syms).expect_err("negative should error");
        assert!(err.contains("non-negative"), "got: {}", err);
    }

    #[test]
    fn sleep_ms_wrong_type_errors() {
        let mut syms = SymbolTable::new();
        let err =
            b_beam_sleep_ms(&[Value::Boolean(true)], &mut syms).expect_err("bool should error");
        assert!(err.contains("sleep-ms"), "got: {}", err);
    }

    #[test]
    fn sleep_zero_seconds_returns_immediately() {
        let mut syms = SymbolTable::new();
        let start = std::time::Instant::now();
        let v = b_beam_sleep(&[Value::Fixnum(0)], &mut syms).expect("sleep 0");
        assert!(matches!(v, Value::Unspecified));
        assert!(start.elapsed().as_millis() < 50);
    }

    #[test]
    fn sleep_zero_float_returns_immediately() {
        let mut syms = SymbolTable::new();
        let start = std::time::Instant::now();
        b_beam_sleep(&[Value::Flonum(0.0)], &mut syms).expect("sleep 0.0");
        assert!(start.elapsed().as_millis() < 50);
    }

    #[test]
    fn sleep_negative_errors() {
        let mut syms = SymbolTable::new();
        let err =
            b_beam_sleep(&[Value::Fixnum(-1)], &mut syms).expect_err("negative int should error");
        assert!(err.contains("non-negative"), "got: {}", err);
    }

    #[test]
    fn sleep_negative_float_errors() {
        let mut syms = SymbolTable::new();
        let err = b_beam_sleep(&[Value::Flonum(-0.5)], &mut syms)
            .expect_err("negative float should error");
        assert!(err.contains("non-negative"), "got: {}", err);
    }

    #[test]
    fn sleep_wrong_type_errors() {
        let mut syms = SymbolTable::new();
        let err = b_beam_sleep(&[Value::Null], &mut syms).expect_err("null should error");
        assert!(err.contains("sleep"), "got: {}", err);
    }

    #[test]
    fn sleep_without_coroutine_driver_blocks() {
        // Cooperative sleep is gated on a live coroutine driver (a `YIELDER`),
        // NOT on the thread name. With no driver installed — the case on any
        // bare thread, even one named like a LocalSet worker — `sleep`/`sleep-ms`
        // fall back to a plain blocking `thread::sleep` and succeed, rather than
        // erroring (the old F3 behavior). (The closure maps away the !Send Value
        // before returning across the thread boundary.)
        let res: Result<(), String> = std::thread::Builder::new()
            .name("cs-actor-local-0".to_string())
            .spawn(|| {
                let start = std::time::Instant::now();
                let mut syms = SymbolTable::new();
                b_beam_sleep_ms(&[Value::Fixnum(20)], &mut syms).map(|_| ())?;
                b_beam_sleep(&[Value::Flonum(0.02)], &mut syms).map(|_| ())?;
                assert!(
                    start.elapsed() >= Duration::from_millis(30),
                    "both sleeps should actually block when no driver is present"
                );
                Ok(())
            })
            .unwrap()
            .join()
            .unwrap();
        res.expect("sleep without a coroutine driver must just block + succeed");
    }

    #[test]
    fn sleep_ms_ok_on_dedicated_thread() {
        // A dedicated spawn-source / block_in_place actor (or non-actor code)
        // has no coroutine driver, so it block-sleeps fine.
        let res: Result<(), String> = std::thread::Builder::new()
            .name("tokio-runtime-worker".to_string())
            .spawn(|| {
                let mut syms = SymbolTable::new();
                b_beam_sleep_ms(&[Value::Fixnum(5)], &mut syms).map(|_| ())
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(res.is_ok());
    }

    // cc_classify must agree with crab-cache router.scm `classify-route`. (The
    // exhaustive fuzz lives in crab-cache test/native-classify-diff.scm; this
    // pins the index math — 'any/'all/'cluster, single-key, no-key, multi-key
    // same/cross slot, and MSET's even-index key selection.)
    #[test]
    fn cc_classify_matches_router_scm() {
        let ns = 3;
        // 'any stateless verbs (case-insensitive arg0).
        assert_eq!(cc_classify(b"PING", &[], ns), CcRoute::Any);
        assert_eq!(cc_classify(b"ping", &[b"hi"], ns), CcRoute::Any);
        assert_eq!(cc_classify(b"INFO", &[], ns), CcRoute::Any);
        // 'all fan-out.
        assert_eq!(cc_classify(b"DBSIZE", &[], ns), CcRoute::All);
        assert_eq!(cc_classify(b"KEYS", &[b"*"], ns), CcRoute::All);
        // 'cluster.
        assert_eq!(cc_classify(b"CLUSTER", &[b"SLOTS"], ns), CcRoute::Cluster);
        // Single-key commands route to one shard; SET == GET for the same key.
        let get = cc_classify(b"GET", &[b"foo"], ns);
        assert!(matches!(get, CcRoute::Shard(_)));
        assert_eq!(cc_classify(b"SET", &[b"foo", b"bar"], ns), get);
        // Unknown verb -> else branch -> single key at operand 0.
        assert!(matches!(
            cc_classify(b"WHATEVER", &[b"k"], ns),
            CcRoute::Shard(_)
        ));
        // Keyed command with NO key -> shard 0 (where it arity-errs).
        assert_eq!(cc_classify(b"GET", &[], ns), CcRoute::Shard(0));
        // Multi-key DEL: shared {hashtag} co-locates -> one shard; else CrossSlot.
        assert!(matches!(
            cc_classify(b"DEL", &[b"{t}1", b"{t}2"], ns),
            CcRoute::Shard(_)
        ));
        assert_eq!(
            cc_classify(b"DEL", &[b"{a}", b"{b}"], ns),
            CcRoute::CrossSlot
        );
        // MSET keys are the EVEN operands; the (cross-slot) values must not count.
        assert!(matches!(
            cc_classify(b"MSET", &[b"{t}1", b"{a}", b"{t}2", b"{b}"], ns),
            CcRoute::Shard(_)
        ));
        assert_eq!(
            cc_classify(b"MSET", &[b"{a}", b"v1", b"{b}", b"v2"], ns),
            CcRoute::CrossSlot
        );
    }
}
