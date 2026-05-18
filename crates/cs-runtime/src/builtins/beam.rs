//! BEAM-style actor + table + hot-reload primops, exposed to
//! Scheme as builtins. Behind the `actor` feature.
//!
//! See `docs/research/beam_runtime_spec.md` for the design and
//! `docs/milestones/beam-v1-exit.md` for what shipped.

#![cfg(feature = "actor")]

use std::sync::Arc;

use cs_core::{Number, Pair, SymbolTable, Value};

use cs_actor::{ActorPid, Payload};

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
        Value::Number(n) => num_to_sendable(n),
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
        Value::Procedure(_) => Err("to_sendable: procedures cannot cross actor boundaries".into()),
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
    let actor_ref = st.actors.spawn(move |actor| {
        // Hold a raw pointer to `actor` for the duration of the
        // body so the (self) / (raw-receive) Scheme builtins can
        // reach it via ACTOR_CTX. Safety: the pointer lives only
        // on this blocking thread, and the Guard clears it before
        // the closure returns (or unwinds), so a thread-pool
        // worker reused for a later actor never sees a stale ptr.
        // The Guard also zeros REDUCTIONS so a reused worker
        // doesn't inherit the prior actor's count.
        let ptr: *mut cs_actor::Actor = actor;
        ACTOR_CTX.with(|c| c.set(ptr));
        REDUCTIONS.with(|c| c.set(0));
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                ACTOR_CTX.with(|c| c.set(std::ptr::null_mut()));
                REDUCTIONS.with(|c| c.set(0));
            }
        }
        let _g = Guard;
        entry(actor, args);
    });
    Ok(actor_ref.pid())
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
    let sv = to_sendable_in(&args[1], syms)?;
    primop_send(pid, sv)?;
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

/// `(table-size name)` — returns the current cell count.
pub fn b_beam_table_size(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("table-size", args, 1)?;
    let name = value_to_str(&args[0], syms, "table-size")?;
    let n = primop_table_size(&name)?;
    Ok(Value::Number(Number::Fixnum(n as i64)))
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

/// `(self)` — return the calling actor's PID as a symbol.
/// Errors if called from outside an actor body.
pub fn b_beam_self(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("self", args, 0)?;
    let pid = with_current_actor(|a| a.self_ref().pid())
        .ok_or_else(|| "self: not inside an actor body".to_string())?;
    Ok(from_sendable(&SendableValue::Pid(pid), syms))
}

/// `(reductions)` — return the calling actor's current
/// reduction count. Erlang-flavored: a proxy for "work done"
/// the scheduler tracks per actor. B3's scheduler-swap half
/// (post-1.0) will use this as a yield-check threshold.
pub fn b_beam_reductions(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("reductions", args, 0)?;
    let n = REDUCTIONS.with(|c| c.get());
    Ok(Value::Number(Number::Fixnum(n as i64)))
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
        Value::Number(Number::Fixnum(n)) if *n >= 0 => *n as u64,
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
    Ok(Value::Number(Number::Fixnum(new as i64)))
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

/// `(raw-receive)` blocks until a message arrives;
/// `(raw-receive timeout-ms)` returns `#f` if the deadline
/// passes without one. System messages (Exit/Down) surface as
/// tagged lists the Scheme `(receive)` macro can pattern-match.
pub fn b_beam_raw_receive(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    let timeout_ms = match args.len() {
        0 => None,
        1 => match &args[0] {
            Value::Boolean(false) => None,
            Value::Number(Number::Fixnum(n)) if *n >= 0 => Some(*n as u64),
            other => {
                return Err(format!(
                    "raw-receive: timeout must be #f or a non-negative integer, got {}",
                    other.type_name()
                ))
            }
        },
        n => return Err(format!("raw-receive: expected 0 or 1 arguments, got {}", n)),
    };

    let outcome = with_current_actor(|a| primop_raw_receive(a, timeout_ms))
        .ok_or_else(|| "raw-receive: not inside an actor body".to_string())??;

    match outcome {
        Some(sv) => Ok(from_sendable(&sv, syms)),
        None => Ok(Value::Boolean(false)),
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
    Ok(Value::Number(Number::Fixnum(epoch as i64)))
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
        Value::Number(Number::Fixnum(n)) if *n >= 0 => *n as usize,
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
                Some(e) => Value::Number(Number::Fixnum(e as i64)),
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
        ("self", b_beam_self),
        ("raw-receive", b_beam_raw_receive),
        // reductions (B3 first half: cooperative-yield seam)
        ("reductions", b_beam_reductions),
        ("bump-reductions!", b_beam_bump_reductions),
        ("yield", b_beam_yield),
        // table
        ("make-table", b_beam_make_table),
        ("table-insert!", b_beam_table_insert),
        ("table-lookup", b_beam_table_lookup),
        ("table-delete!", b_beam_table_delete),
        ("table-size", b_beam_table_size),
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
