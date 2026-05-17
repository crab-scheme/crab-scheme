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
//! ```
//! use std::sync::Arc;
//! use cs_actor::{ActorSystem, Message};
//!
//! let sys = ActorSystem::new();
//!
//! // Spawn a "pong" actor that drains every message it gets.
//! let pong = sys.spawn(|actor| {
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

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;
use thiserror::Error;
use tokio::sync::mpsc;

// Re-export the tokio mpsc error type used in [`Actor::try_receive`]'s
// signature so downstream crates can match on it without depending on
// tokio themselves.
pub use tokio::sync::mpsc::error::TryRecvError;

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
    #[error("call to {pid} timed out after {timeout_ms} ms")]
    CallTimeout { pid: ActorPid, timeout_ms: u64 },
    #[error("actor system shutting down")]
    Shutdown,
}

// ---------- Internal registry ----------

/// Process-wide registry mapping PID → mailbox sender. Cloned into
/// every `ActorRef` so cross-actor `send` calls can look up the
/// receiver without going through the ActorSystem.
type Registry = Arc<Mutex<FxHashMap<ActorPid, mpsc::UnboundedSender<Message>>>>;

// ---------- Public handle ----------

/// Cheap, cloneable handle for sending messages to an actor.
///
/// Cloning is `Arc::clone` on the underlying sender + a PID copy —
/// cheap to pass around.
#[derive(Clone)]
pub struct ActorRef {
    pid: ActorPid,
    inbox: mpsc::UnboundedSender<Message>,
}

impl ActorRef {
    pub fn pid(&self) -> ActorPid {
        self.pid
    }

    /// Fire-and-forget cast. Returns `Err` only if the receiver
    /// has been dropped (actor terminated).
    pub fn send(&self, payload: Payload) -> Result<(), ActorError> {
        self.inbox
            .send(Message::User(payload))
            .map_err(|_| ActorError::SendFailed { pid: self.pid })
    }

    /// Send a pre-built system Message. Used internally by the
    /// supervisor / link mechanisms.
    pub fn send_raw(&self, msg: Message) -> Result<(), ActorError> {
        self.inbox
            .send(msg)
            .map_err(|_| ActorError::SendFailed { pid: self.pid })
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
    inbox: mpsc::UnboundedReceiver<Message>,
    /// Cloned from the system so the actor can spawn children or
    /// look up sibling actors by PID.
    system: ActorSystemRef,
}

impl Actor {
    pub fn pid(&self) -> ActorPid {
        self.pid
    }

    /// Blocking receive — returns the next message in the mailbox,
    /// or `None` if all senders have been dropped (i.e., we're the
    /// last reference, system is shutting down, and nothing else
    /// can ever send us a message).
    ///
    /// In B2 this is a true OS-thread block via tokio's
    /// `blocking_recv`. B3 swaps this for a cooperative async-aware
    /// receive that yields to the tokio scheduler so other actors
    /// can run on the same worker.
    pub fn receive(&mut self) -> Option<Message> {
        self.inbox.blocking_recv()
    }

    /// Non-blocking receive — returns immediately. `Ok(msg)` if a
    /// message was available, `Err(empty)` if the mailbox is empty
    /// but the channel is still open, `Err(disconnected)` if all
    /// senders have dropped.
    pub fn try_receive(&mut self) -> Result<Message, mpsc::error::TryRecvError> {
        self.inbox.try_recv()
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
    handle: tokio::runtime::Handle,
}

impl ActorSystemRef {
    fn next_pid(&self) -> ActorPid {
        ActorPid {
            node: 0,
            local_id: self.next_local_id.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn lookup(&self, pid: ActorPid) -> Option<ActorRef> {
        let reg = self.registry.lock().ok()?;
        reg.get(&pid).cloned().map(|inbox| ActorRef { pid, inbox })
    }

    fn deregister(&self, pid: ActorPid) {
        if let Ok(mut reg) = self.registry.lock() {
            reg.remove(&pid);
        }
    }
}

impl ActorSystem {
    /// Create a new system with default settings.
    ///
    /// B2 defaults:
    /// - 1 tokio worker thread (spec: "single-threaded tokio runtime")
    /// - Up to 4096 blocking threads (each spawned actor consumes one).
    ///   B3 reverses this: many worker threads + async actors that
    ///   share workers via cooperative yield.
    pub fn new() -> Self {
        let tokio_rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(4096)
            .thread_name("cs-actor-blk")
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let handle = tokio_rt.handle().clone();
        let inner = ActorSystemRef {
            registry: Arc::new(Mutex::new(FxHashMap::default())),
            next_local_id: Arc::new(AtomicU64::new(1)),
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
    pub fn spawn<F>(&self, body: F) -> ActorRef
    where
        F: FnOnce(&mut Actor) + Send + 'static,
    {
        let pid = self.inner.next_pid();
        let (tx, rx) = mpsc::unbounded_channel::<Message>();
        // Register the sender so anyone with the PID can find us.
        self.inner
            .registry
            .lock()
            .expect("registry poisoned")
            .insert(pid, tx.clone());

        let system_for_actor = self.inner.clone();
        let inner_for_cleanup = self.inner.clone();
        let pid_for_cleanup = pid;

        self.inner.handle.spawn_blocking(move || {
            let mut actor = Actor {
                pid,
                inbox: rx,
                system: system_for_actor,
            };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut actor)));
            // Deregister so further sends to this PID error out.
            inner_for_cleanup.deregister(pid_for_cleanup);
            if let Err(payload) = result {
                let msg = panic_message(&payload);
                eprintln!("cs-actor {pid_for_cleanup}: panicked: {msg}");
                // B5 will deliver this as Message::Exit to linked
                // actors; B2 just logs.
            }
        });

        ActorRef { pid, inbox: tx }
    }

    /// Look up an actor by PID. `None` if the actor has terminated
    /// or never existed.
    pub fn lookup(&self, pid: ActorPid) -> Option<ActorRef> {
        self.inner.lookup(pid)
    }

    /// Total actors currently registered. Useful for tests + tools.
    pub fn live_actor_count(&self) -> usize {
        self.inner.registry.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Block until the registry drains (all spawned actors have
    /// terminated). Used by tests + by graceful shutdown.
    pub fn wait_idle(&self) {
        loop {
            if self.live_actor_count() == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Shut the system down. Drops all actors, drops the tokio
    /// runtime, waits for all blocking threads to finish their
    /// current iteration.
    pub fn shutdown(self) {
        // Drop the registry's senders so any blocking_recv() calls
        // wake up with None.
        self.inner.registry.lock().expect("poisoned").clear();
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
}
