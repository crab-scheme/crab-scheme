//! `cs-actor` — BEAM-style actor system for CrabScheme.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md`. This
//! crate deliberately stays narrow — it provides the floor that
//! cannot exist in Scheme (Tokio runtime, mailbox plumbing, PID
//! allocator, per-Heap activation, reduction-counting hook,
//! cross-thread Value transport) and exposes exactly four
//! primops to the language:
//!
//! | Primop | Rust shape | Scheme shape (set later by cs-runtime) |
//! |--------|-----------|----------------------------------------|
//! | spawn  | `ActorSystem::spawn` | `(spawn thunk)` |
//! | send   | `ActorRef::send`     | `(send pid value)` |
//! | recv   | `Actor::receive`     | `(raw-receive)` |
//! | self   | `Actor::self_ref`    | `(self)` |
//!
//! Everything else — pattern-matching `(receive (...) (after ...))`,
//! `(call pid msg)`, `(link)` / `(monitor)`, supervisors,
//! `(define-behavior)`, gen_server-style restart logic — lives
//! in a Scheme prelude written on top of those four primops.
//! Pattern matching, child-spec records, restart strategies are
//! all much cleaner as macros than as a Rust DSL.
//!
//! - One [`cs_runtime::Runtime`] per actor (matches BEAM's
//!   per-process-heap model). The Runtime is constructed inside
//!   the actor's spawned thread so its `Rc`-heavy internals never
//!   cross a thread boundary.
//! - Mailbox = `tokio::sync::mpsc::Receiver<Message>` (unbounded
//!   by default, matching BEAM; bounded variants for back-pressure
//!   are a flag, not a separate API).
//! - Reduction-based preemption: yield hook in the cs-vm bytecode
//!   dispatch loop lands in B3.
//!
//! ## Status
//!
//! Phase **B2** — spawn / send / receive work end-to-end. Each
//! actor runs on a tokio `spawn_blocking` thread; this scales to
//! ~thousands of actors per process (limited by `max_blocking_threads`,
//! default raised here to 4096). B3 switches to true async scheduling
//! with reduction-based preemption so the architecture scales to
//! the 100k-actor target in the spec.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use cs_actor::{ActorSystem, Message};
//!
//! let sys = ActorSystem::new();
//!
//! // Spawn a "pong" actor that drains every message it gets. The
//! // sync-body-on-task path (parallel-runtime C1.1+) runs each
//! // actor as a tokio task rather than an OS thread — no per-actor
//! // OS-thread ceiling.
//! let pong = sys.spawn_sync_body_on_task(|actor| {
//!     while let Some(msg) = actor.receive() {
//!         if let Message::User(_p) = msg {
//!             // (real ping/pong needs the sender's PID embedded in the
//!             // message; B2 omits that — see Actor::receive_user docs.)
//!         }
//!     }
//! });
//!
//! // Cast a message; payload is any Send+Sync Arc-wrapped value.
//! pong.send(Arc::new("hello".to_string())).unwrap();
//!
//! // Wait for all actors to finish before tearing down the system.
//! sys.shutdown();
//! ```

#![allow(dead_code)] // some types are public for B3+ consumers; trim later.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cs_table::{Mailbox as DurableMailbox, TableRegistry};
use dashmap::DashMap;
use rustc_hash::FxBuildHasher;
use thiserror::Error;
use tokio::sync::mpsc;

// ---------- Mailbox backing strategy ----------

/// Which mailbox implementation backs an actor's inbox.
///
/// - **Fast** — `tokio::sync::mpsc::UnboundedSender/Receiver`.
///   Sharded, lock-free hot path. ~10× faster than Durable
///   for send/receive throughput. The default for callers
///   that don't opt in to a specific kind.
///
/// - **Durable** — cs-table `OrderedSet` backed Mailbox.
///   Queue contents live in the same Mnesia-style table
///   fabric as ETS-style shared state, so:
///     * `(table-size '__mailbox:0.42)` reports queue depth
///     * future cs-table disk-spill applies automatically
///     * post-mortem inspection works via the standard
///       table-lookup / table-fold primops
///   Pays a ~4× throughput cost (RwLock<BTreeMap> vs sharded
///   mpsc) for those properties.
///
/// Pick per spawn via `spawn_with_kind` / the `_durable`
/// variants of the existing spawn API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxKind {
    Fast,
    Durable,
}

impl Default for MailboxKind {
    fn default() -> Self {
        MailboxKind::Fast
    }
}

/// Per-actor mailbox handle that hides the Fast/Durable
/// choice. Both backings expose the same surface (push,
/// try_pop, pop_or_wait, is_closed, close, len) so call
/// sites don't branch on the kind — only `new_fast` /
/// `new_durable` do.
#[derive(Clone)]
pub(crate) struct ActorMailbox {
    inner: Arc<MailboxBacking>,
}

enum MailboxBacking {
    /// tokio mpsc with a single-owner receiver behind a
    /// Mutex<Option<…>> so the receive side can be parked
    /// during async block_in_place wrap without
    /// the receiver vanishing under us.
    Fast {
        sender: mpsc::UnboundedSender<Message>,
        /// Receiver lives behind the Mutex so `Actor::receive`
        /// can take exclusive access (mpsc::UnboundedReceiver
        /// is `!Sync`). The Option allows close-without-drop.
        receiver: Mutex<Option<mpsc::UnboundedReceiver<Message>>>,
        /// Approximate pending count — tokio mpsc doesn't
        /// expose len() on the unbounded sender, so we track
        /// it here for the soft-cap mechanism.
        pending: AtomicUsize,
    },
    /// cs-table-backed FIFO. Multi-producer / single-consumer
    /// via internal Condvar; same Arc-handle pattern works
    /// for any number of senders.
    Durable(Arc<DurableMailbox>),
}

impl ActorMailbox {
    /// Construct a Fast mailbox (default — tokio mpsc).
    pub(crate) fn new_fast() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(MailboxBacking::Fast {
                sender: tx,
                receiver: Mutex::new(Some(rx)),
                pending: AtomicUsize::new(0),
            }),
        }
    }

    /// Construct a Durable mailbox backed by `cs-table` under
    /// the given system's `TableRegistry`. The table name
    /// uniquely identifies the mailbox (one per actor).
    pub(crate) fn new_durable(registry: TableRegistry, table_name: String) -> Self {
        let mb = DurableMailbox::create(registry, table_name)
            .expect("mailbox name collision — PID allocator misbehaved");
        Self {
            inner: Arc::new(MailboxBacking::Durable(Arc::new(mb))),
        }
    }

    pub fn kind(&self) -> MailboxKind {
        match &*self.inner {
            MailboxBacking::Fast { .. } => MailboxKind::Fast,
            MailboxBacking::Durable(_) => MailboxKind::Durable,
        }
    }

    /// Push a message. Returns `Err(())` if the mailbox is
    /// closed; otherwise `Ok(())`. Caller wraps the unit
    /// error into `ActorError::SendFailed`.
    pub fn push(&self, msg: Message) -> Result<(), ()> {
        match &*self.inner {
            MailboxBacking::Fast {
                sender, pending, ..
            } => {
                sender.send(msg).map_err(|_| ())?;
                pending.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            MailboxBacking::Durable(mb) => {
                let payload: cs_table::Payload = Arc::new(msg);
                mb.push(payload).map_err(|_| ())
            }
        }
    }

    /// Non-blocking pop. Returns `Some(msg)` on hit, `None`
    /// on empty. (Closed-state inspection lives in
    /// `is_closed`.)
    pub fn try_pop(&self) -> Option<Message> {
        match &*self.inner {
            MailboxBacking::Fast {
                receiver, pending, ..
            } => {
                let mut guard = receiver.lock().expect("receiver lock poisoned");
                let recv = guard.as_mut()?;
                match recv.try_recv() {
                    Ok(msg) => {
                        pending.fetch_sub(1, Ordering::Relaxed);
                        Some(msg)
                    }
                    Err(_) => None,
                }
            }
            MailboxBacking::Durable(mb) => mb.try_pop().ok().flatten().map(|p| {
                Arc::downcast::<Message>(p)
                    .map(|am| (*am).clone())
                    .expect("durable mailbox payload was not a Message")
            }),
        }
    }

    /// Blocking pop with optional timeout. `None` timeout =
    /// block until message OR mailbox closed-and-empty.
    /// Returns `None` on timeout OR closed-and-empty.
    pub fn pop_or_wait(&self, timeout: Option<Duration>) -> Option<Message> {
        match &*self.inner {
            MailboxBacking::Fast {
                receiver, pending, ..
            } => {
                let mut guard = receiver.lock().expect("receiver lock poisoned");
                let recv = guard.as_mut()?;
                let msg = match timeout {
                    None => recv.blocking_recv(),
                    Some(d) => {
                        // tokio UnboundedReceiver has no blocking
                        // recv_timeout. Use try_recv in a sleep loop
                        // — short timeouts only.
                        let deadline = std::time::Instant::now() + d;
                        loop {
                            match recv.try_recv() {
                                Ok(m) => break Some(m),
                                Err(mpsc::error::TryRecvError::Disconnected) => break None,
                                Err(mpsc::error::TryRecvError::Empty) => {
                                    if std::time::Instant::now() >= deadline {
                                        break None;
                                    }
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }
                    }
                };
                if msg.is_some() {
                    pending.fetch_sub(1, Ordering::Relaxed);
                }
                msg
            }
            MailboxBacking::Durable(mb) => mb.pop_or_wait(timeout).ok().flatten().map(|p| {
                Arc::downcast::<Message>(p)
                    .map(|am| (*am).clone())
                    .expect("durable mailbox payload was not a Message")
            }),
        }
    }

    /// Async pop. Used by `Actor::receive_async` so async-
    /// body actors don't call `blocking_recv` (which panics
    /// inside a multi_thread tokio runtime).
    ///
    /// For Fast: temporarily takes the UnboundedReceiver out
    /// of the Mutex, awaits one recv, then puts it back. The
    /// take-and-replace serializes concurrent recv attempts
    /// the same way `pop_or_wait` does — mpsc semantics
    /// disallow multiple concurrent receivers anyway.
    ///
    /// For Durable: delegates to `pop_or_wait` (Condvar
    /// blocking; safe under `block_in_place`).
    pub async fn pop_or_wait_async(&self) -> Option<Message> {
        match &*self.inner {
            MailboxBacking::Fast {
                receiver, pending, ..
            } => {
                let mut rx = {
                    let mut guard = receiver.lock().expect("receiver lock poisoned");
                    guard.take()?
                };
                let msg = rx.recv().await;
                // Put the receiver back so subsequent recvs
                // can re-take it. Drop case: if msg is None
                // (sender closed), the receiver still goes
                // back so try_receive can observe Disconnected.
                {
                    let mut guard = receiver.lock().expect("receiver lock poisoned");
                    *guard = Some(rx);
                }
                if msg.is_some() {
                    pending.fetch_sub(1, Ordering::Relaxed);
                }
                msg
            }
            MailboxBacking::Durable(mb) => mb.pop_or_wait(None).ok().flatten().map(|p| {
                Arc::downcast::<Message>(p)
                    .map(|am| (*am).clone())
                    .expect("durable mailbox payload was not a Message")
            }),
        }
    }

    /// Mark the mailbox closed. After close, `push` returns
    /// `Err`. Any parked receivers wake and observe the
    /// closed state.
    pub fn close(&self) {
        match &*self.inner {
            MailboxBacking::Fast { receiver, .. } => {
                // Drop the receiver so the sender's
                // UnboundedSender::send returns Err. Parked
                // blocking_recv calls wake with None.
                receiver.lock().expect("receiver lock poisoned").take();
            }
            MailboxBacking::Durable(mb) => mb.close(),
        }
    }

    pub fn is_closed(&self) -> bool {
        match &*self.inner {
            MailboxBacking::Fast {
                sender, receiver, ..
            } => sender.is_closed() || receiver.lock().map(|g| g.is_none()).unwrap_or(true),
            MailboxBacking::Durable(mb) => mb.is_closed(),
        }
    }

    pub fn len(&self) -> usize {
        match &*self.inner {
            MailboxBacking::Fast { pending, .. } => pending.load(Ordering::Relaxed),
            MailboxBacking::Durable(mb) => mb.len(),
        }
    }
}

/// Mirrors the shape of `tokio::sync::mpsc::error::TryRecvError`
/// so downstream callers that already match on
/// `Empty` / `Disconnected` keep working post-mailbox swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TryRecvError {
    #[error("mailbox is empty (no message available right now)")]
    Empty,
    #[error("mailbox is closed (sender side dropped)")]
    Disconnected,
}

// (unwrap_message helper removed — ActorMailbox::try_pop and
// pop_or_wait now do the downcast at the boundary.)

// ---------- Identifiers ----------

/// A process identifier. Equivalent of Erlang's pid.
///
/// Encoding: 64 bits = 16 node id + 48 local actor id. The node bits
/// are 0 for local-only mode; `cs-distrib` (post-v1) will populate
/// them with remote node identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorPid {
    pub node: u16,
    pub local_id: u64,
}

impl fmt::Display for ActorPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}.{}>", self.node, self.local_id)
    }
}

// ---------- Messages ----------

/// Why an actor terminated.
#[derive(Debug, Clone)]
pub enum ExitReason {
    /// Normal completion — actor's main function returned.
    Normal,
    /// `(exit pid 'kill)` — uncatchable.
    Killed,
    /// `(exit pid <reason>)` for some other reason.
    User(String),
    /// Actor's Scheme code raised an error or panicked.
    Error(String),
}

/// User payload type: any `Send + Sync + 'static` value, type-erased.
///
/// B2 deliberately decouples the actor system from `cs_core::Value`
/// because `Value` is `!Send` (Rc-everywhere). When Scheme integration
/// lands (post-B3), `send`/`receive` primops will wrap each message
/// in a Send-able envelope that deep-copies the Value into the
/// receiver's heap (matching BEAM's copy-on-send semantics — see
/// `docs/research/beam_runtime_spec.md` "Cycle reclamation across
/// actors"). For now, tests use any Arc-wrapped Send+Sync type.
pub type Payload = Arc<dyn std::any::Any + Send + Sync>;

/// A message in flight to an actor.
///
/// User messages carry an opaque [`Payload`]; system messages
/// carry supervision / hot-reload signals.
#[derive(Debug, Clone)]
pub enum Message {
    /// Ordinary message sent via [`ActorRef::send`].
    User(Payload),
    /// Link-propagated exit signal.
    Exit { from: ActorPid, reason: ExitReason },
    /// Monitor-fired DOWN message.
    Down {
        ref_id: u64,
        pid: ActorPid,
        reason: ExitReason,
    },
    // Future variants (B6+): SystemReload, SystemPing, ...
}

// ---------- Errors ----------

#[derive(Debug, Error)]
pub enum ActorError {
    #[error("actor {pid} not found")]
    NotFound { pid: ActorPid },
    #[error("send to {pid} failed: receiver dropped")]
    SendFailed { pid: ActorPid },
    #[error("send to {pid} failed: mailbox full ({cap} cap reached)")]
    MailboxFull { pid: ActorPid, cap: usize },
    #[error("call to {pid} timed out after {timeout_ms} ms")]
    CallTimeout { pid: ActorPid, timeout_ms: u64 },
    #[error("actor system shutting down")]
    Shutdown,
}

// ---------- Internal registry ----------

/// Per-actor state shared between the registry (so other
/// actors can look it up by PID) and the `ActorRef` handle
/// returned to spawners. Holds the inbox sender plus all
/// supervision metadata (link set, monitor map, trap-exit
/// flag).
///
/// Mutated from several threads: the actor's worker (sets
/// trap_exit, updates monitor map), other actors' workers
/// (link/unlink, register monitors), the actor's terminator
/// (drops everything in cleanup). Per-field `Mutex` (not one
/// big lock) keeps contention low on the hot send path —
/// `inbox` reads have no lock at all.
pub(crate) struct ActorState {
    /// Per-actor mailbox. Either tokio-mpsc (Fast) or
    /// cs-table-backed (Durable) — chosen at spawn time
    /// via `MailboxKind`. The `ActorMailbox` wrapper hides
    /// the choice from call sites that only need
    /// push/try_pop/pop_or_wait.
    mailbox: ActorMailbox,
    /// Bidirectional link partners. When this actor dies it
    /// sends `Message::Exit { from: self.pid, reason }` to
    /// each linked actor; the receivers either die (default)
    /// or convert it to a regular `Exit` message they can
    /// pattern-match (if `trap_exit` is true).
    links: Mutex<HashSet<ActorPid>>,
    /// Watchers monitoring this actor. Key is the monitor
    /// ref_id (allocated by `Monitor::next_ref_id`); value is
    /// the watcher's PID. On termination, send each watcher a
    /// `Message::Down { ref_id, pid: self.pid, reason }`.
    monitored_by: Mutex<HashMap<u64, ActorPid>>,
    /// Whether this actor wants `Message::Exit` delivered as
    /// a regular message (trap-exit mode) vs. terminating
    /// the actor (default). Read by `Actor::receive` /
    /// receive_async to decide the disposition of incoming
    /// Exit messages.
    trap_exit: AtomicBool,
    /// Soft mailbox cap (audit fix #5). `0` = unlimited (the
    /// default, BEAM-style). When > 0, `send_with_cap`
    /// checks `pending` before delivering and returns
    /// `ActorError::MailboxFull` if at-or-above cap. This
    /// rejects rather than blocks — backpressure is the
    /// sender's policy decision (retry, drop, alert). True
    /// awaiting backpressure would need switching the channel
    /// type to `mpsc::Sender`; that's a heavier refactor.
    ///
    /// `ActorRef::send` (held directly, e.g., from Rust tests)
    /// bypasses the cap check for the hot path. Production
    /// senders should go through cs-runtime's `(send pid …)`
    /// primop, which routes through `send_with_cap`.
    soft_cap: AtomicUsize,
    /// In-flight message count (sent − received). Bumped by
    /// `send_with_cap`; decremented by `primop_raw_receive`
    /// after a successful recv. Approximate when used outside
    /// the cap-enforcement path because non-capped `ActorRef::send`
    /// bypasses the increment.
    pending: AtomicUsize,
}

impl ActorState {
    fn new(mailbox: ActorMailbox) -> Self {
        Self {
            mailbox,
            links: Mutex::new(HashSet::new()),
            monitored_by: Mutex::new(HashMap::new()),
            trap_exit: AtomicBool::new(false),
            soft_cap: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
        }
    }

    /// Internal raw push that delegates to the ActorMailbox.
    /// Used by both system-message paths (Exit, Down) and by
    /// `send_with_cap`. The backing kind (Fast vs Durable)
    /// is transparent here.
    fn push_raw(&self, pid: ActorPid, msg: Message) -> Result<(), ActorError> {
        self.mailbox
            .push(msg)
            .map_err(|_| ActorError::SendFailed { pid })
    }

    /// Send with soft-cap enforcement. Returns `Err(MailboxFull)`
    /// when `soft_cap > 0` and `pending >= soft_cap`; otherwise
    /// behaves like the raw push. Increments `pending` on
    /// success — the receiver decrements after a successful recv.
    pub(crate) fn send_with_cap(&self, pid: ActorPid, msg: Message) -> Result<(), ActorError> {
        let cap = self.soft_cap.load(Ordering::Relaxed);
        if cap > 0 && self.pending.load(Ordering::Relaxed) >= cap {
            return Err(ActorError::MailboxFull { pid, cap });
        }
        self.push_raw(pid, msg)?;
        self.pending.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Called by `primop_raw_receive` after a successful
    /// dequeue. Saturating-decrements `pending`; we tolerate
    /// transient underflow if `send_with_cap` racing with
    /// `ActorRef::send` (which bypasses pending) skewed the
    /// count.
    pub(crate) fn note_received(&self) {
        let prev = self.pending.load(Ordering::Relaxed);
        if prev > 0 {
            // Best-effort decrement. Concurrent decrements
            // are fine because we're saturating-subtracting.
            self.pending.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

/// Process-wide registry mapping PID → per-actor state.
/// Cloned `Arc`s into every `ActorRef` so cross-actor `send`
/// calls bypass the registry entirely on the hot path.
///
/// **Pre-rev:** `Arc<Mutex<FxHashMap<PID, Sender>>>`.
/// Profiled (see `examples/perf_spawn_echo.rs`) and found
/// contention here dropped spawn rate ~10× at N=1M live
/// actors: the spawner thread inserts while the worker pool
/// concurrently `deregister`s, all serialized through one
/// mutex.
///
/// **Now:** `DashMap` of `Arc<ActorState>`. Sharded on key
/// hash internally (default 64 shards, each with its own
/// lock). Concurrent insert + deregister hit different shards
/// almost always. `dashmap = "6"` is already a workspace dep
/// (cs-table / cs-runtime / cs-hotreload), so no new
/// transitive cost.
type Registry = Arc<DashMap<ActorPid, Arc<ActorState>, FxBuildHasher>>;

// ---------- Public handle ----------

/// Cheap, cloneable handle for sending messages to an actor.
///
/// Cloning is `Arc::clone` on the underlying sender + a PID copy —
/// cheap to pass around.
#[derive(Clone)]
pub struct ActorRef {
    pid: ActorPid,
    /// Direct handle on the actor's mailbox. Hides the Fast
    /// vs Durable choice behind the same push API.
    mailbox: ActorMailbox,
}

impl ActorRef {
    pub fn pid(&self) -> ActorPid {
        self.pid
    }

    /// Fire-and-forget cast. Returns `Err` only if the
    /// mailbox is closed (actor terminated).
    pub fn send(&self, payload: Payload) -> Result<(), ActorError> {
        self.send_raw(Message::User(payload))
    }

    /// Send a pre-built system Message. Used internally by the
    /// supervisor / link mechanisms.
    pub fn send_raw(&self, msg: Message) -> Result<(), ActorError> {
        self.mailbox
            .push(msg)
            .map_err(|_| ActorError::SendFailed { pid: self.pid })
    }

    /// Which kind of mailbox backs this actor.
    pub fn mailbox_kind(&self) -> MailboxKind {
        self.mailbox.kind()
    }
}

impl fmt::Debug for ActorRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ActorRef({})", self.pid)
    }
}

// ---------- Actor (the thing the body closure sees) ----------

/// Each actor's view of the world: its own PID, its mailbox, and
/// (B3+) hooks for yielding back to the scheduler.
pub struct Actor {
    pid: ActorPid,
    /// Same handle as `ActorState.mailbox` and the holders of
    /// `ActorRef`. The mailbox is multi-producer /
    /// single-consumer; this side owns the consumer.
    mailbox: ActorMailbox,
    /// Cloned from the system so the actor can spawn children or
    /// look up sibling actors by PID.
    system: ActorSystemRef,
    /// Shared per-actor state: trap_exit flag, link set,
    /// monitor map. Held as `Arc` so supervision primops
    /// (link / monitor / trap_exit) reach the same cells the
    /// terminator reads from in `spawn_async`'s cleanup path.
    pub(crate) state: Arc<ActorState>,
}

impl Actor {
    pub fn pid(&self) -> ActorPid {
        self.pid
    }

    /// Blocking receive — returns the next message in the
    /// mailbox, or `None` once the mailbox is closed AND
    /// empty. Uses the cs-table Mailbox's Condvar wait, so
    /// blocks the OS thread; callers running inside a tokio
    /// async block should wrap in `block_in_place`
    /// (`spawn_sync_body_on_task` does this automatically).
    pub fn receive(&mut self) -> Option<Message> {
        self.recv_with_timeout(None)
    }

    /// Async receive. For Fast-backed actors this awaits
    /// tokio mpsc's `recv()` natively (no thread block).
    /// For Durable-backed actors the cs-table Mailbox uses
    /// a Condvar, so this falls back to a blocking wait —
    /// callers expecting non-blocking semantics with a
    /// durable mailbox should wrap their body in
    /// `spawn_sync_body_on_task` (which uses
    /// `block_in_place`).
    pub async fn receive_async(&mut self) -> Option<Message> {
        self.mailbox.pop_or_wait_async().await
    }

    /// Non-blocking receive — returns immediately. `Ok(msg)`
    /// if a message was available, `Err(TryRecvError::Empty)`
    /// if the mailbox is empty but the actor is still alive,
    /// `Err(TryRecvError::Disconnected)` if the mailbox is
    /// closed and drained.
    pub fn try_receive(&mut self) -> Result<Message, TryRecvError> {
        match self.mailbox.try_pop() {
            Some(msg) => Ok(msg),
            None if self.mailbox.is_closed() => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Recv with optional timeout. `None` = block forever
    /// until message or close. Returns `None` on timeout or
    /// closed+empty mailbox.
    fn recv_with_timeout(&mut self, timeout: Option<Duration>) -> Option<Message> {
        self.mailbox.pop_or_wait(timeout)
    }

    /// Which kind of mailbox backs this actor.
    pub fn mailbox_kind(&self) -> MailboxKind {
        self.mailbox.kind()
    }

    // Note: there's no `receive_user` skip-system-messages helper.
    // The Scheme `(receive ...)` macro layered on top of `receive`
    // does selective-receive with pattern matching — discarding
    // system messages (or trapping them) is a policy decision
    // expressed at the Scheme level, not baked into the primop.

    /// Return a handle for sending to this actor (for replying to
    /// the actor that just messaged us, or handing out our PID
    /// elsewhere).
    pub fn self_ref(&self) -> ActorRef {
        // The system's registry already holds a sender for us.
        self.system.lookup(self.pid).expect("self lookup failed")
    }

    /// Look up another actor by PID.
    pub fn lookup(&self, pid: ActorPid) -> Option<ActorRef> {
        self.system.lookup(pid)
    }

    // ---- Supervision (BEAM-style link / monitor / trap_exit) ----

    /// Bidirectionally link this actor to `target`. When
    /// either dies, the other gets `Message::Exit { from,
    /// reason }`. By default the receiver treats a non-Normal
    /// Exit as a fatal signal and terminates; calling
    /// [`Self::trap_exit`] flips it to a regular deliverable
    /// message instead.
    ///
    /// Idempotent (linking twice is the same as linking once).
    /// Returns `Err(NotFound)` if `target` doesn't exist —
    /// matches BEAM behavior of immediately delivering an
    /// `*exit*` for already-dead links.
    pub fn link(&self, target: ActorPid) -> Result<(), ActorError> {
        if target == self.pid {
            // Self-link: BEAM treats this as a no-op (you
            // can't supervise yourself in a useful way).
            return Ok(());
        }
        let target_state = self
            .system
            .registry
            .get(&target)
            .map(|e| Arc::clone(e.value()))
            .ok_or(ActorError::NotFound { pid: target })?;
        // Insert into both link sets. Order doesn't matter
        // because we use HashSet (no duplicates).
        self.state
            .links
            .lock()
            .expect("links lock poisoned")
            .insert(target);
        target_state
            .links
            .lock()
            .expect("links lock poisoned")
            .insert(self.pid);
        Ok(())
    }

    /// Tear down a link previously created via [`Self::link`].
    /// No-op if no link existed. Doesn't error on missing
    /// target — by the time you decide to unlink, the target
    /// may already be gone, and that's fine.
    pub fn unlink(&self, target: ActorPid) {
        if target == self.pid {
            return;
        }
        self.state
            .links
            .lock()
            .expect("links lock poisoned")
            .remove(&target);
        if let Some(entry) = self.system.registry.get(&target) {
            entry
                .value()
                .links
                .lock()
                .expect("links lock poisoned")
                .remove(&self.pid);
        }
    }

    /// One-way monitor: when `target` dies, this actor gets
    /// `Message::Down { ref_id, pid: target, reason }`.
    /// Returns a `ref_id` the caller passes to
    /// [`Self::demonitor`] to cancel. Unlike [`link`], the
    /// dying side never receives anything — monitor is
    /// asymmetric.
    ///
    /// Returns `Err(NotFound)` for non-existent target. (BEAM
    /// instead sends an immediate Down with reason = noproc;
    /// we may add that later.)
    pub fn monitor(&self, target: ActorPid) -> Result<u64, ActorError> {
        let target_state = self
            .system
            .registry
            .get(&target)
            .map(|e| Arc::clone(e.value()))
            .ok_or(ActorError::NotFound { pid: target })?;
        let ref_id = self.system.next_monitor_ref();
        target_state
            .monitored_by
            .lock()
            .expect("monitored_by lock poisoned")
            .insert(ref_id, self.pid);
        Ok(ref_id)
    }

    /// Cancel a monitor previously created via [`Self::monitor`].
    /// `target` is the actor being monitored; `ref_id` is the
    /// value [`monitor`] returned. Silent no-op if either is
    /// already gone.
    pub fn demonitor(&self, target: ActorPid, ref_id: u64) {
        if let Some(entry) = self.system.registry.get(&target) {
            entry
                .value()
                .monitored_by
                .lock()
                .expect("monitored_by lock poisoned")
                .remove(&ref_id);
        }
    }

    /// Set / clear trap-exit mode. When enabled, incoming
    /// `Message::Exit` is delivered to the actor's mailbox
    /// like any other message (the actor can pattern-match
    /// it). When disabled (default), a non-Normal Exit
    /// terminates the receiving actor.
    ///
    /// Returns the previous value.
    pub fn trap_exit(&self, enabled: bool) -> bool {
        self.state.trap_exit.swap(enabled, Ordering::SeqCst)
    }

    /// Read the current trap-exit setting without changing it.
    pub fn is_trapping_exits(&self) -> bool {
        self.state.trap_exit.load(Ordering::Relaxed)
    }

    // ---- Bounded mailbox (audit fix #5) ----

    /// Set the soft cap on this actor's mailbox. `0`
    /// (default) disables enforcement; positive `n` means
    /// `send_with_cap`-routed sends to this actor return
    /// `Err(MailboxFull)` when at-or-above N pending.
    /// Returns the previous cap.
    pub fn set_mailbox_cap(&self, cap: usize) -> usize {
        self.state.soft_cap.swap(cap, Ordering::Relaxed)
    }

    /// Read the configured cap (0 = unlimited).
    pub fn mailbox_cap(&self) -> usize {
        self.state.soft_cap.load(Ordering::Relaxed)
    }

    /// Current approximate pending count. May be off if
    /// some senders used the raw `ActorRef::send` path
    /// instead of routing through `send_with_cap`.
    pub fn mailbox_pending(&self) -> usize {
        self.state.pending_count()
    }

    /// Send `payload` to `target` via the cap-checked path.
    /// Used by cs-runtime's `(send pid …)` Scheme primop.
    pub fn send_with_cap_to(&self, target: ActorPid, payload: Payload) -> Result<(), ActorError> {
        let target_state = self
            .system
            .registry
            .get(&target)
            .map(|e| Arc::clone(e.value()))
            .ok_or(ActorError::SendFailed { pid: target })?;
        target_state.send_with_cap(target, Message::User(payload))
    }

    /// Decrement this actor's pending counter after a
    /// successful receive. cs-runtime's `primop_raw_receive`
    /// calls this so the mailbox cap reflects in-flight
    /// vs. consumed messages.
    pub fn note_received(&self) {
        self.state.note_received();
    }
}

// ---------- Actor system ----------

/// The actor system — owns the tokio runtime, the registry, and
/// the next-PID counter.
pub struct ActorSystem {
    tokio_rt: tokio::runtime::Runtime,
    inner: ActorSystemRef,
}

#[derive(Clone)]
struct ActorSystemRef {
    registry: Registry,
    next_local_id: Arc<AtomicU64>,
    /// Monotonic counter for monitor ref_ids. Independent
    /// from PIDs so callers can't confuse the two.
    next_monitor_ref_id: Arc<AtomicU64>,
    handle: tokio::runtime::Handle,
    /// Per-system cs-table fabric that backs each actor's
    /// mailbox. Each spawn allocates a fresh table named
    /// `__mailbox:<pid>` here; the Mailbox struct owns the
    /// table's lifecycle (`Drop` removes it).
    tables: TableRegistry,
    /// Signaled by `on_actor_termination` whenever an actor
    /// finishes. `wait_idle` blocks on the Condvar and
    /// rechecks `registry.is_empty()` instead of busy-
    /// polling (audit fix #9). The Mutex<()> is just a
    /// dummy required by Condvar's API; the source of truth
    /// is `registry.is_empty()`.
    idle_notify: Arc<(Mutex<()>, Condvar)>,
}

impl ActorSystemRef {
    /// Build a fresh mailbox for `pid` with the requested
    /// backing. Fast skips the cs-table allocation entirely
    /// (cheaper); Durable allocates a `__mailbox:<pid>` table
    /// in the system fabric so the queue is inspectable via
    /// standard table-lookup primops.
    fn build_mailbox(&self, pid: ActorPid, kind: MailboxKind) -> ActorMailbox {
        match kind {
            MailboxKind::Fast => ActorMailbox::new_fast(),
            MailboxKind::Durable => {
                let name = format!("__mailbox:{}.{}", pid.node, pid.local_id);
                ActorMailbox::new_durable(self.tables.clone(), name)
            }
        }
    }
}

impl ActorSystemRef {
    fn next_pid(&self) -> ActorPid {
        ActorPid {
            node: 0,
            local_id: self.next_local_id.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn lookup(&self, pid: ActorPid) -> Option<ActorRef> {
        // DashMap::get returns a Ref guard scoped to one shard;
        // clone the ActorMailbox handle out before the guard drops.
        self.registry.get(&pid).map(|entry| ActorRef {
            pid,
            mailbox: entry.value().mailbox.clone(),
        })
    }

    fn next_monitor_ref(&self) -> u64 {
        self.next_monitor_ref_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a new actor's state in the registry. Called
    /// by the spawn paths before the actor body runs.
    fn register_actor(&self, pid: ActorPid, state: Arc<ActorState>) {
        self.registry.insert(pid, state);
    }

    /// Called from spawn_async's cleanup path when an actor
    /// terminates. Walks the link set + monitor set and
    /// delivers `Exit` / `Down` messages to the survivors,
    /// then removes the entry from the registry.
    ///
    /// `reason` is the actor's termination reason — Normal
    /// for clean returns, Error("…") for panics. Links
    /// receive Exit; if a linked actor is NOT trap_exit,
    /// the Exit reason is propagated by also re-delivering
    /// to that actor's links (cascading dies).
    fn on_actor_termination(&self, pid: ActorPid, reason: ExitReason) {
        // Pull the dying actor's state out so we have a
        // stable view of its link/monitor sets even after
        // removing it from the registry.
        let dying = self.registry.remove(&pid).map(|(_, state)| state);
        let Some(dying) = dying else {
            return;
        };

        // Notify monitors (one-way: dying side only emits).
        // Hold the lock briefly to snapshot, then send
        // outside the lock so a downstream send that blocks
        // doesn't keep the lock.
        let monitors_snapshot: Vec<(u64, ActorPid)> = dying
            .monitored_by
            .lock()
            .expect("monitored_by lock poisoned")
            .iter()
            .map(|(&r, &p)| (r, p))
            .collect();
        for (ref_id, watcher) in monitors_snapshot {
            if let Some(entry) = self.registry.get(&watcher) {
                let _ = entry.value().push_raw(
                    watcher,
                    Message::Down {
                        ref_id,
                        pid,
                        reason: reason.clone(),
                    },
                );
            }
        }

        // Notify links (bidirectional, propagating).
        let links_snapshot: Vec<ActorPid> = dying
            .links
            .lock()
            .expect("links lock poisoned")
            .iter()
            .copied()
            .collect();
        for linked in links_snapshot {
            // Remove the back-pointer from the survivor's
            // link set so a future link-cycle break doesn't
            // re-notify.
            if let Some(entry) = self.registry.get(&linked) {
                entry
                    .value()
                    .links
                    .lock()
                    .expect("links lock poisoned")
                    .remove(&pid);
                let _ = entry.value().push_raw(
                    linked,
                    Message::Exit {
                        from: pid,
                        reason: reason.clone(),
                    },
                );
                // The survivor's receive_async decides whether
                // to terminate or trap. We don't escalate
                // here — propagation rides the Message::Exit
                // chain naturally as receivers handle them.
            }
        }

        // Audit fix #9: signal anyone parked in wait_idle.
        // The Condvar guards the registry-empty condition;
        // wakers re-check `registry.is_empty()` before
        // returning so spurious wakeups are harmless.
        if self.registry.is_empty() {
            let (lock, cv) = &*self.idle_notify;
            let _g = lock.lock().expect("idle_notify lock poisoned");
            cv.notify_all();
        }
    }
}

impl ActorSystem {
    /// Create a new system with default settings.
    ///
    /// Defaults (parallel-runtime spec C1.3):
    /// - Worker threads = `std::thread::available_parallelism()`
    ///   (host's logical core count, min 2). Honors the
    ///   `CRABSCHEME_ACTOR_WORKERS` env var (numeric, or
    ///   "physical" — not yet wired since `available_parallelism`
    ///   already returns logical cores; physical-only would need
    ///   the `num_cpus` crate).
    /// - Up to 4096 blocking threads cap (legacy safety net for
    ///   any caller still using `spawn` / `spawn_blocking`; the
    ///   `spawn_async` and `spawn_sync_body_on_task` paths don't
    ///   consume this budget once C2's yield hook fully releases
    ///   workers between reductions).
    ///
    /// Pre-C1.3 default was `worker_threads(1)` — a single async
    /// worker. Now M workers multiplex N async tasks, so the C2
    /// yield-hook preemption has somewhere to migrate the
    /// currently-yielded actor.
    pub fn new() -> Self {
        let workers = std::env::var("CRABSCHEME_ACTOR_WORKERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(2)
            });
        let tokio_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .max_blocking_threads(4096)
            .thread_name("cs-actor-blk")
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let handle = tokio_rt.handle().clone();
        let inner = ActorSystemRef {
            registry: Arc::new(DashMap::with_hasher(FxBuildHasher::default())),
            next_local_id: Arc::new(AtomicU64::new(1)),
            next_monitor_ref_id: Arc::new(AtomicU64::new(1)),
            tables: TableRegistry::new(),
            idle_notify: Arc::new((Mutex::new(()), Condvar::new())),
            handle,
        };
        Self { tokio_rt, inner }
    }

    /// Spawn an actor running `body`, returning its handle.
    ///
    /// `body` receives a mutable [`Actor`] reference; from inside,
    /// the closure can call `receive()`, `self_ref()`, `lookup()`,
    /// spawn child actors via the captured system, etc.
    ///
    /// The closure returns when the actor finishes (normal exit).
    /// Panic inside `body` is captured and (B5) propagated as an
    /// `ExitReason::Error` to linked actors; B2 just logs it to
    /// stderr.
    ///
    /// **Deprecated (parallel-runtime C1.4).** This path uses
    /// `spawn_blocking`, which dedicates one OS thread per
    /// actor and hits the 4096-actor ceiling from
    /// `max_blocking_threads(4096)`. New code should use:
    ///
    /// - [`Self::spawn_sync_body_on_task`] for an
    ///   identically-shaped sync `FnOnce(&mut Actor)` body
    ///   that runs as a tokio task (no thread-per-actor
    ///   ceiling), or
    /// - [`Self::spawn_async`] for native async bodies.
    ///
    /// Existing call sites work unchanged and won't be
    /// removed in 1.0, but the API is no longer the
    /// recommended path.
    #[deprecated(
        since = "1.0.0",
        note = "use spawn_sync_body_on_task (sync body, no thread-per-actor ceiling) \
                or spawn_async (native async body) — see parallel-runtime spec C1.4"
    )]
    pub fn spawn<F>(&self, body: F) -> ActorRef
    where
        F: FnOnce(&mut Actor) + Send + 'static,
    {
        self.spawn_with_kind(MailboxKind::default(), body)
    }

    /// Like [`spawn`] but lets the caller pick the mailbox
    /// backing. Most users want `Fast` (the default); pick
    /// `Durable` for inspectable / persistable queues at the
    /// cost of ~4× send throughput.
    pub fn spawn_with_kind<F>(&self, kind: MailboxKind, body: F) -> ActorRef
    where
        F: FnOnce(&mut Actor) + Send + 'static,
    {
        let pid = self.inner.next_pid();
        let mailbox = self.inner.build_mailbox(pid, kind);
        let state = Arc::new(ActorState::new(mailbox.clone()));
        self.inner.register_actor(pid, Arc::clone(&state));

        let system_for_actor = self.inner.clone();
        let inner_for_cleanup = self.inner.clone();
        let state_for_actor = Arc::clone(&state);
        let mailbox_for_actor = mailbox.clone();
        let pid_for_cleanup = pid;

        self.inner.handle.spawn_blocking(move || {
            let mut actor = Actor {
                pid,
                mailbox: mailbox_for_actor,
                system: system_for_actor,
                state: state_for_actor,
            };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut actor)));
            let reason = match &result {
                Ok(()) => ExitReason::Normal,
                Err(payload) => {
                    let msg = panic_message(payload);
                    eprintln!("cs-actor {pid_for_cleanup}: panicked: {msg}");
                    ExitReason::Error(msg)
                }
            };
            inner_for_cleanup.on_actor_termination(pid_for_cleanup, reason);
        });

        ActorRef { pid, mailbox }
    }

    /// Spawn an actor whose body is a sync closure, but run that
    /// closure as a tokio task (not via `spawn_blocking`).
    /// Internally wraps the body in `block_in_place` so synchronous
    /// blocking calls (like the bytecode interpreter's
    /// `actor.receive()`) work correctly without parking the
    /// underlying tokio worker thread permanently.
    ///
    /// This is the bridge between the existing sync-body world and
    /// the async-task scheduler. Callers that already have an
    /// async body should use [`spawn_async`] directly.
    ///
    /// Embedders without tokio-specific knowledge call this from
    /// cs-runtime — no `tokio::*` types appear in their code.
    pub fn spawn_sync_body_on_task<F>(&self, body: F) -> ActorRef
    where
        F: FnOnce(&mut Actor) + Send + 'static,
    {
        self.spawn_sync_body_on_task_with_kind(MailboxKind::default(), body)
    }

    /// Like [`spawn_sync_body_on_task`] but with an explicit
    /// mailbox backing. Use `Durable` when the actor's queue
    /// needs to survive crashes / be inspectable from
    /// outside.
    pub fn spawn_sync_body_on_task_with_kind<F>(&self, kind: MailboxKind, body: F) -> ActorRef
    where
        F: FnOnce(&mut Actor) + Send + 'static,
    {
        self.spawn_async_with_kind(kind, move |mut actor| async move {
            tokio::task::block_in_place(move || {
                body(&mut actor);
            });
        })
    }

    /// Async-body counterpart of [`spawn`] (parallel-runtime spec
    /// C1.1). `body` is an async closure that takes an owned
    /// `Actor` and returns a `Future`; the future runs as a tokio
    /// task (not `spawn_blocking`), so M worker threads can
    /// multiplex N ≫ M actors instead of one OS thread per actor.
    ///
    /// The 4096-actor ceiling from `max_blocking_threads(4096)`
    /// does not apply to this path. The practical ceiling is
    /// memory (each task carries a per-actor Runtime).
    ///
    /// Panic capture: tokio tasks panicking surface via
    /// `JoinHandle`; we install a panic handler at task entry
    /// that mirrors the sync path's behavior (deregister + log).
    pub fn spawn_async<F, Fut>(&self, body: F) -> ActorRef
    where
        F: FnOnce(Actor) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.spawn_async_with_kind(MailboxKind::default(), body)
    }

    /// Like [`spawn_async`] but lets the caller pick the
    /// mailbox backing.
    pub fn spawn_async_with_kind<F, Fut>(&self, kind: MailboxKind, body: F) -> ActorRef
    where
        F: FnOnce(Actor) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let pid = self.inner.next_pid();
        let mailbox = self.inner.build_mailbox(pid, kind);
        let state = Arc::new(ActorState::new(mailbox.clone()));
        self.inner.register_actor(pid, Arc::clone(&state));

        let system_for_actor = self.inner.clone();
        let inner_for_cleanup = self.inner.clone();
        let state_for_actor = Arc::clone(&state);
        let mailbox_for_actor = mailbox.clone();
        let pid_for_cleanup = pid;

        self.inner.handle.spawn(async move {
            let actor = Actor {
                pid,
                mailbox: mailbox_for_actor,
                system: system_for_actor,
                state: state_for_actor,
            };
            // Wrap in AssertUnwindSafe + catch_unwind so a panic in
            // body() doesn't poison the whole runtime. Tokio's
            // default behavior aborts the task; we want the same
            // log-and-deregister semantics as `spawn`.
            let fut = std::panic::AssertUnwindSafe(body(actor));
            let result = futures::FutureExt::catch_unwind(fut).await;
            let reason = match &result {
                Ok(()) => ExitReason::Normal,
                Err(payload) => {
                    let msg = panic_message(payload);
                    eprintln!("cs-actor {pid_for_cleanup}: panicked: {msg}");
                    ExitReason::Error(msg)
                }
            };
            inner_for_cleanup.on_actor_termination(pid_for_cleanup, reason);
        });

        ActorRef { pid, mailbox }
    }

    /// **Perf-diagnostic only** — `spawn_async` minus the
    /// registry insert + deregister. The returned ActorRef
    /// is NOT findable via `lookup(pid)`; `send()` works only
    /// because the caller holds the ref directly. Used by
    /// `examples/perf_spawn_echo.rs` to isolate registry-
    /// contention cost from tokio-task overhead.
    #[doc(hidden)]
    pub fn spawn_async_unregistered<F, Fut>(&self, body: F) -> ActorRef
    where
        F: FnOnce(Actor) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let pid = self.inner.next_pid();
        // Perf-diagnostic only — always uses Fast backing
        // since the goal is to measure tokio-task overhead.
        let mailbox = self.inner.build_mailbox(pid, MailboxKind::Fast);
        let system_for_actor = self.inner.clone();
        // Even unregistered actors need their own state cell —
        // Actor's supervision methods read from it — but we
        // skip the registry insert so other actors can't
        // discover us by PID. Result: link/monitor of an
        // unregistered actor fails with NotFound, which is the
        // intent for perf-diagnostic isolation.
        let state = Arc::new(ActorState::new(mailbox.clone()));
        let mailbox_for_actor = mailbox.clone();
        self.inner.handle.spawn(async move {
            let actor = Actor {
                pid,
                mailbox: mailbox_for_actor,
                system: system_for_actor,
                state,
            };
            let fut = std::panic::AssertUnwindSafe(body(actor));
            let _ = futures::FutureExt::catch_unwind(fut).await;
        });
        ActorRef { pid, mailbox }
    }

    /// Look up an actor by PID. `None` if the actor has terminated
    /// or never existed.
    pub fn lookup(&self, pid: ActorPid) -> Option<ActorRef> {
        self.inner.lookup(pid)
    }

    /// Total actors currently registered. Useful for tests + tools.
    pub fn live_actor_count(&self) -> usize {
        self.inner.registry.len()
    }

    /// Block until the registry drains (all spawned actors have
    /// terminated). Used by tests + by graceful shutdown.
    pub fn wait_idle(&self) {
        // Audit fix #9: park on the system's Condvar rather
        // than busy-polling every 5ms. `on_actor_termination`
        // signals after each actor's cleanup, and the
        // re-check inside the wait loop handles spurious
        // wakeups + the race between `live_actor_count()`
        // and the actual decrement.
        let (lock, cv) = &*self.inner.idle_notify;
        loop {
            if self.live_actor_count() == 0 {
                return;
            }
            let guard = lock.lock().expect("idle_notify lock poisoned");
            // Brief upper bound so a missed notify (e.g.,
            // signal arrived between the registry check and
            // the lock acquire) doesn't deadlock the wait —
            // we recheck the registry every 250ms regardless.
            let (_g, _timeout) = cv
                .wait_timeout(guard, std::time::Duration::from_millis(250))
                .expect("idle_notify wait poisoned");
        }
    }

    /// Shut the system down. Drops all actors, drops the tokio
    /// runtime, waits for all blocking threads to finish their
    /// current iteration.
    pub fn shutdown(self) {
        // Drop the registry's senders so any blocking_recv() calls
        // wake up with None.
        self.inner.registry.clear();
        // Drop the runtime — this joins all worker + blocking
        // threads. Long-running actors that don't notice the
        // dropped senders will be aborted at runtime tear-down.
        drop(self.tokio_rt);
    }
}

impl Default for ActorSystem {
    fn default() -> Self {
        Self::new()
    }
}

/// Reduction-yield bridge for cs-vm's `install_yield_hook`
/// (parallel-runtime spec C2.2). Calling this from a thread that's
/// currently inside a tokio runtime's `block_in_place` (i.e., an
/// actor body launched via `spawn_sync_body_on_task`) briefly
/// returns control to the tokio scheduler so the runtime can drain
/// queued tasks, then resumes.
///
/// Outside an actor context (no current tokio runtime) this is a
/// no-op — `Handle::try_current()` returns `Err` and we skip the
/// `block_on`.
///
/// Designed to be wired up as
/// `cs_vm::vm::install_yield_hook(Some(cs_actor::tokio_yield_hook))`
/// at the start of every actor body. Function pointer compatible
/// with `cs_vm::vm::VmYieldHook = fn()`.
pub fn tokio_yield_hook() {
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        // block_on of yield_now is sound from inside block_in_place
        // on a multi_thread runtime: block_in_place excused this
        // worker from its async duties; yield_now is a one-tick
        // yield that always returns Ready on the next poll. No
        // deadlock potential.
        h.block_on(tokio::task::yield_now());
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
#[allow(deprecated)] // legacy `spawn` (C1.4) still has tests
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex; // tests use Mutex for their own shared state

    #[test]
    fn pid_displays() {
        let p = ActorPid {
            node: 0,
            local_id: 42,
        };
        assert_eq!(p.to_string(), "<0.42>");
    }

    #[test]
    fn spawn_and_run_to_completion() {
        let sys = ActorSystem::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let _r = sys.spawn(move |_a| {
            c.fetch_add(1, Ordering::Relaxed);
        });
        sys.wait_idle();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        sys.shutdown();
    }

    #[test]
    fn one_thousand_actors_each_print_their_pid() {
        // Spec B2 acceptance: spawn 1000 actors each writing
        // observable evidence (here: pushing pid into a shared
        // Vec so we can assert uniqueness + count).
        let sys = ActorSystem::new();
        let collected: Arc<Mutex<Vec<ActorPid>>> = Arc::new(Mutex::new(Vec::with_capacity(1000)));
        for _ in 0..1000 {
            let c = collected.clone();
            sys.spawn(move |a| {
                c.lock().expect("collected poisoned").push(a.pid());
            });
        }
        sys.wait_idle();
        let v = collected.lock().expect("collected poisoned");
        assert_eq!(v.len(), 1000);
        // PIDs are unique (each spawn bumps the atomic counter).
        let mut sorted = v.clone();
        sorted.sort_by_key(|p| p.local_id);
        sorted.dedup_by_key(|p| p.local_id);
        assert_eq!(sorted.len(), 1000);
        sys.shutdown();
    }

    #[test]
    fn round_trip_message_across_two_actors() {
        let sys = ActorSystem::new();
        let received: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
        let received_for_pong = received.clone();

        // Pong: receives one message, stores it.
        let pong = sys.spawn(move |a| {
            if let Some(Message::User(p)) = a.receive() {
                if let Some(v) = p.downcast_ref::<i64>() {
                    *received_for_pong.lock().expect("poisoned") = Some(*v);
                }
            }
        });

        // Ping: sends one message to pong.
        sys.spawn(move |_a| {
            pong.send(Arc::new(42i64)).expect("send to pong");
        });

        sys.wait_idle();
        let got = received.lock().expect("poisoned").take();
        assert_eq!(got, Some(42));
        sys.shutdown();
    }

    #[test]
    fn hundred_actor_chain_round_trip() {
        // Spec B2 acceptance: full message round-trip across 100
        // actors. We build a relay chain: a1 → a2 → ... → a100 →
        // collector. Sending one message to a1 causes it to
        // cascade through all 100, with each actor incrementing
        // the payload.
        let sys = ActorSystem::new();
        let final_value: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
        let final_v_for_collector = final_value.clone();
        let collector = sys.spawn(move |a| {
            if let Some(Message::User(p)) = a.receive() {
                if let Some(i) = p.downcast_ref::<i64>() {
                    *final_v_for_collector.lock().expect("poisoned") = Some(*i);
                }
            }
        });

        let mut next: ActorRef = collector;
        for _ in 0..100 {
            let downstream = next.clone();
            next = sys.spawn(move |a| {
                if let Some(Message::User(p)) = a.receive() {
                    if let Some(i) = p.downcast_ref::<i64>() {
                        let _ = downstream.send(Arc::new(*i + 1));
                    }
                }
            });
        }
        // Kick off the chain.
        next.send(Arc::new(0i64)).expect("kickoff");

        // Wait until the collector finishes.
        sys.wait_idle();
        let got = final_value.lock().expect("poisoned").take();
        // 100 hops, each +1 → 100.
        assert_eq!(got, Some(100));
        sys.shutdown();
    }

    #[test]
    fn lookup_returns_none_after_actor_exits() {
        let sys = ActorSystem::new();
        let r = sys.spawn(|_a| {});
        let pid = r.pid();
        sys.wait_idle();
        // After idle, the actor is deregistered.
        assert!(sys.lookup(pid).is_none());
        sys.shutdown();
    }

    #[test]
    fn panic_in_actor_body_is_isolated() {
        let sys = ActorSystem::new();
        let _r = sys.spawn(|_a| {
            panic!("intentional");
        });
        sys.wait_idle();
        // Sibling actor still works.
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        sys.spawn(move |_a| {
            c.fetch_add(1, Ordering::Relaxed);
        });
        sys.wait_idle();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        sys.shutdown();
    }

    // ---- parallel-runtime spec C1.1 — async spawn + receive ----

    #[test]
    fn async_round_trip_one_thousand_actors() {
        // Acceptance for C1.1 from tasks.md: 1k actors via
        // spawn_async, each receives + replies once. Validates the
        // async path doesn't park OS threads per-actor and that
        // both spawn_async and receive_async work end-to-end.
        let sys = ActorSystem::new();
        let reply_count = Arc::new(AtomicUsize::new(0));

        // A router actor that sends `N` messages and collects replies.
        let n = 1000usize;
        let workers: Vec<ActorRef> = (0..n)
            .map(|_| {
                let reply_count = reply_count.clone();
                sys.spawn_async(move |mut actor| async move {
                    if let Some(Message::User(_)) = actor.receive_async().await {
                        reply_count.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();

        for w in &workers {
            w.send(Arc::new(())).expect("send to worker");
        }
        sys.wait_idle();
        assert_eq!(reply_count.load(Ordering::Relaxed), n);
        sys.shutdown();
    }

    #[test]
    fn async_panic_in_body_is_isolated() {
        // catch_unwind around the async body must mirror the sync
        // path: process keeps running, sibling actors unaffected.
        let sys = ActorSystem::new();
        sys.spawn_async(|_actor| async move {
            panic!("intentional async panic");
        });
        sys.wait_idle();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        sys.spawn_async(move |_actor| async move {
            c.fetch_add(1, Ordering::Relaxed);
        });
        sys.wait_idle();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        sys.shutdown();
    }

    // ---- Supervision (link / monitor / trap_exit) ----

    /// Bidirectional link: A's death delivers Message::Exit to B.
    /// With trap_exit on, B captures it instead of dying.
    #[test]
    fn link_propagates_exit_with_trap() {
        let sys = ActorSystem::new();
        // Channel for B to publish what it received.
        let (tx, rx) = std::sync::mpsc::channel::<Message>();

        // B: trap exits, link to A (passed via a oneshot for the
        // pid handshake), then receive.
        let (pid_tx, pid_rx) = std::sync::mpsc::sync_channel::<ActorPid>(1);
        // Gate A's completion until B has linked. Without
        // this, A's no-op body races to terminate before B
        // can resolve its registry entry.
        let (linked_tx, linked_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let b = sys.spawn_sync_body_on_task(move |actor| {
            actor.trap_exit(true);
            let a_pid = pid_rx.recv().expect("a_pid");
            actor.link(a_pid).expect("link");
            linked_tx.send(()).expect("linked signal");
            if let Some(msg) = actor.receive() {
                tx.send(msg).expect("forward to test thread");
            }
        });

        // A: wait for B to link, then exit normally. B should
        // receive Exit { from: A, reason: Normal } and forward.
        let a = sys.spawn_sync_body_on_task(move |_actor| {
            linked_rx.recv().expect("wait for link");
        });
        pid_tx.send(a.pid()).expect("pid handshake");

        let received = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("B should receive an Exit message");
        match received {
            Message::Exit { from, reason } => {
                assert_eq!(from, a.pid());
                assert!(matches!(reason, ExitReason::Normal));
            }
            other => panic!("expected Exit, got {other:?}"),
        }

        let _ = b; // keep alive past assert
        sys.shutdown();
    }

    /// One-way monitor: when target dies, watcher gets Message::Down
    /// with the matching ref_id.
    #[test]
    fn monitor_fires_down_on_target_death() {
        let sys = ActorSystem::new();
        let (tx, rx) = std::sync::mpsc::channel::<Message>();
        let (pid_tx, pid_rx) = std::sync::mpsc::sync_channel::<ActorPid>(1);
        // Gate target's exit until watcher has registered.
        let (monitored_tx, monitored_rx) = std::sync::mpsc::sync_channel::<()>(1);

        // Watcher: monitor target, then receive Down.
        sys.spawn_sync_body_on_task(move |actor| {
            let target = pid_rx.recv().expect("target_pid");
            let _ref = actor.monitor(target).expect("monitor");
            monitored_tx.send(()).expect("monitored signal");
            if let Some(msg) = actor.receive() {
                tx.send(msg).expect("forward to test thread");
            }
        });

        // Target: wait for watcher to register monitor, then exit.
        let target = sys.spawn_sync_body_on_task(move |_actor| {
            monitored_rx.recv().expect("wait for monitor");
        });
        pid_tx.send(target.pid()).expect("pid handshake");

        let received = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("watcher should receive a Down message");
        match received {
            Message::Down { pid, reason, .. } => {
                assert_eq!(pid, target.pid());
                assert!(matches!(reason, ExitReason::Normal));
            }
            other => panic!("expected Down, got {other:?}"),
        }

        sys.shutdown();
    }

    /// link to a dead PID returns NotFound. (BEAM would send an
    /// immediate Exit instead; we error explicitly for now —
    /// noproc-immediate-exit is a follow-up if needed.)
    #[test]
    fn link_to_dead_actor_errors() {
        let sys = ActorSystem::new();
        let dead = sys.spawn_sync_body_on_task(|_actor| {});
        sys.wait_idle();
        let dead_pid = dead.pid();
        let (errored, errored_for_actor) = (
            Arc::new(std::sync::Mutex::new(None::<ActorError>)),
            Arc::new(std::sync::Mutex::new(None::<ActorError>)),
        );
        let errored_clone = Arc::clone(&errored_for_actor);
        sys.spawn_sync_body_on_task(move |actor| {
            let err = actor.link(dead_pid).expect_err("link should fail");
            *errored_clone.lock().unwrap() = Some(err);
        });
        sys.wait_idle();
        let _ = errored;
        let err = errored_for_actor.lock().unwrap().take().expect("err");
        assert!(matches!(err, ActorError::NotFound { .. }));
        sys.shutdown();
    }

    // ---- Multi-kind mailbox backings ----

    /// Default kind is Fast (tokio mpsc).
    #[test]
    fn default_spawn_uses_fast_mailbox() {
        let sys = ActorSystem::new();
        let r = sys.spawn_sync_body_on_task(|_actor| {});
        assert_eq!(r.mailbox_kind(), MailboxKind::Fast);
        sys.wait_idle();
        sys.shutdown();
    }

    /// Explicit Durable spawn uses cs-table backing.
    #[test]
    fn durable_spawn_uses_durable_mailbox() {
        let sys = ActorSystem::new();
        let r = sys.spawn_sync_body_on_task_with_kind(MailboxKind::Durable, |_actor| {});
        assert_eq!(r.mailbox_kind(), MailboxKind::Durable);
        sys.wait_idle();
        sys.shutdown();
    }

    /// Echo round-trip works for both backings — same
    /// observable behavior, same code path other than
    /// the kind selector.
    #[test]
    fn echo_works_with_both_backings() {
        for kind in [MailboxKind::Fast, MailboxKind::Durable] {
            let sys = ActorSystem::new();
            let echo_count = Arc::new(AtomicUsize::new(0));
            let echo_count_for_body = Arc::clone(&echo_count);
            let target = sys.spawn_sync_body_on_task_with_kind(kind, move |actor| {
                for _ in 0..10 {
                    if let Some(Message::User(_)) = actor.receive() {
                        echo_count_for_body.fetch_add(1, Ordering::Relaxed);
                    } else {
                        break;
                    }
                }
            });
            for i in 0..10 {
                target.send(Arc::new(i as u64) as Payload).expect("send");
            }
            sys.wait_idle();
            assert_eq!(echo_count.load(Ordering::Relaxed), 10, "kind = {kind:?}");
            sys.shutdown();
        }
    }

    /// Audit fix #5: soft-cap mailbox rejects sends when
    /// the pending count reaches the cap. send_with_cap_to
    /// returns Err(MailboxFull). Note: ActorRef::send (direct)
    /// bypasses the cap intentionally — see ActorState
    /// docs for the design choice.
    #[test]
    fn mailbox_cap_rejects_when_full() {
        let sys = ActorSystem::new();
        let (pid_tx, pid_rx) = std::sync::mpsc::sync_channel::<ActorPid>(1);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let (start_tx, start_rx) = std::sync::mpsc::sync_channel::<()>(1);
        // Gate sender's first send until sleepy has applied
        // the cap — otherwise sender races sleepy and the
        // first 3 sends all see cap=0 → all accepted.
        let (cap_set_tx, cap_set_rx) = std::sync::mpsc::sync_channel::<()>(1);

        // Sleepy actor: set cap = 2, then block on `start_rx`
        // (NOT receive — we want the mailbox to fill from
        // outside while this body holds).
        let sleepy = sys.spawn_sync_body_on_task(move |actor| {
            actor.set_mailbox_cap(2);
            cap_set_tx.send(()).expect("cap_set signal");
            // Wait for the test thread to attempt its 3 sends
            // before consuming anything.
            start_rx.recv().expect("start signal");
            // Drain whatever's available + signal.
            while actor.try_receive().is_ok() {}
            done_tx.send(()).expect("done signal");
        });
        pid_tx.send(sleepy.pid()).expect("pid handshake");

        // Use a sender actor so we have access to send_with_cap_to.
        let target_pid = pid_rx.recv().expect("target_pid");
        cap_set_rx.recv().expect("wait for cap set");
        let (err_tx, err_rx) = std::sync::mpsc::sync_channel::<Option<ActorError>>(1);
        sys.spawn_sync_body_on_task(move |actor| {
            // Send 1 — accepted, pending=1.
            actor
                .send_with_cap_to(target_pid, Arc::new(1u64) as Payload)
                .expect("send 1");
            // Send 2 — accepted, pending=2.
            actor
                .send_with_cap_to(target_pid, Arc::new(2u64) as Payload)
                .expect("send 2");
            // Send 3 — at-cap → MailboxFull.
            let err = actor
                .send_with_cap_to(target_pid, Arc::new(3u64) as Payload)
                .err();
            err_tx.send(err).expect("err signal");
        });

        let err = err_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("sender result");
        match err {
            Some(ActorError::MailboxFull { cap, .. }) => {
                assert_eq!(cap, 2);
            }
            Some(other) => panic!("expected MailboxFull, got {other:?}"),
            None => panic!("expected the third send to fail"),
        }

        // Release the sleepy actor.
        start_tx.send(()).expect("unblock sleepy");
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("sleepy drain done");

        sys.shutdown();
    }

    /// Audit fix #9: wait_idle uses Condvar instead of
    /// busy-polling. Verify it actually returns after the
    /// expected number of actors complete.
    #[test]
    fn wait_idle_returns_when_all_actors_terminate() {
        let sys = ActorSystem::new();
        for _ in 0..32 {
            sys.spawn_sync_body_on_task(|_actor| {
                // No-op body.
            });
        }
        // wait_idle should return promptly without spinning.
        let t0 = std::time::Instant::now();
        sys.wait_idle();
        let elapsed = t0.elapsed();
        assert_eq!(sys.live_actor_count(), 0);
        // Sanity bound: shouldn't take more than a second.
        // Pre-fix this took up to (registry_drain_time + 5ms)
        // due to the 5ms sleep loop; post-fix the Condvar
        // notifies as each actor terminates.
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "wait_idle took {:?} — should be ≪ 1s for 32 no-op actors",
            elapsed
        );
        sys.shutdown();
    }

    /// demonitor before target's death suppresses the Down.
    /// Targets that die without active monitors emit nothing.
    #[test]
    fn demonitor_silences_down() {
        let sys = ActorSystem::new();
        let (tx, rx) = std::sync::mpsc::channel::<Message>();
        let (pid_tx, pid_rx) = std::sync::mpsc::sync_channel::<ActorPid>(1);
        let (demonitored_tx, demonitored_rx) = std::sync::mpsc::sync_channel::<()>(1);

        sys.spawn_sync_body_on_task(move |actor| {
            let target = pid_rx.recv().expect("target_pid");
            let ref_id = actor.monitor(target).expect("monitor");
            actor.demonitor(target, ref_id);
            demonitored_tx.send(()).expect("demonitored signal");
            // Use 50ms try_recv loop with timeout to confirm
            // no Down arrives — receive() would block forever.
            std::thread::sleep(std::time::Duration::from_millis(50));
            if let Ok(msg) = actor.try_receive() {
                tx.send(msg).expect("forward unexpected message");
            }
        });

        // Target waits for demonitor to complete before exiting.
        let target = sys.spawn_sync_body_on_task(move |_actor| {
            demonitored_rx.recv().expect("wait for demonitor");
        });
        pid_tx.send(target.pid()).expect("pid handshake");
        sys.wait_idle();

        // Watcher's try_receive should have failed — channel empty.
        assert!(
            rx.try_recv().is_err(),
            "demonitored watcher should not receive Down"
        );
        sys.shutdown();
    }
}
