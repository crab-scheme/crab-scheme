//! `cs-actor` — BEAM-style actor system for CrabScheme.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md`:
//! - One [`cs_runtime::Runtime`] per actor (matches BEAM's per-process-heap
//!   model; sidesteps a multi-month Arc-everywhere refactor of the
//!   existing Rc-based Runtime).
//! - Tokio multi-thread runtime for actor scheduling.
//! - Mailbox = `tokio::sync::mpsc::Receiver<Message>` (unbounded by
//!   default, matching BEAM semantics; opt-in bounded variant for
//!   back-pressured actors).
//! - Reduction-based preemption via a yield hook in the cs-vm
//!   bytecode dispatch loop (lands in B3, not B2).
//!
//! ## Status
//!
//! Phase **B2** scaffold only. Spawn / send / receive are stubs.
//! See `docs/research/beam_runtime_spec.md#phased-rollout` for the
//! full B1-B8 trajectory.

#![allow(dead_code)] // pre-stub state; populated through B2-B3

use std::fmt;

use thiserror::Error;

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

/// Handle for sending messages to an actor.
///
/// `Clone`-able: cheap to pass around. Internally an `Arc` over the
/// mpsc sender + the pid.
#[derive(Clone)]
pub struct ActorRef {
    pid: ActorPid,
    // inbox: tokio::sync::mpsc::UnboundedSender<Message>,
    // populated in B2 implementation
}

impl ActorRef {
    pub fn pid(&self) -> ActorPid {
        self.pid
    }
}

/// Why an actor terminated.
#[derive(Debug, Clone)]
pub enum ExitReason {
    /// Normal completion — actor's main function returned.
    Normal,
    /// Caller invoked `(exit pid 'kill)` — uncatchable.
    Killed,
    /// Caller invoked `(exit pid <reason>)` for some other reason.
    User(String),
    /// Actor's Scheme code raised an error or panicked.
    Error(String),
}

/// A message in flight to an actor.
///
/// User messages carry a Scheme [`Value`]; system messages carry
/// supervision / hot-reload signals.
#[derive(Debug, Clone)]
pub enum Message {
    /// Ordinary message sent via `(send pid value)` or `(call pid value)`.
    User(cs_core::Value),
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

#[derive(Debug, Error)]
pub enum ActorError {
    #[error("actor {pid} not found")]
    NotFound { pid: ActorPid },
    #[error("send to {pid} failed: {reason}")]
    SendFailed { pid: ActorPid, reason: String },
    #[error("call to {pid} timed out after {timeout_ms} ms")]
    CallTimeout { pid: ActorPid, timeout_ms: u64 },
    #[error("actor system not running")]
    NoSystem,
}

/// The actor system itself. Owns the tokio runtime + the pid → mailbox
/// registry.
///
/// B2: single-threaded; B3: multi-thread work-stealing.
pub struct ActorSystem {
    // tokio_rt: tokio::runtime::Runtime,
    // registry: dashmap::DashMap<ActorPid, ActorRef>,
    // next_local_id: std::sync::atomic::AtomicU64,
    // populated in B2 implementation
}

impl ActorSystem {
    /// Create a new actor system with default settings.
    ///
    /// B2 stub: returns an `ActorSystem` whose `spawn` always errors with
    /// `NoSystem`. The real implementation builds a tokio runtime + an
    /// empty registry.
    pub fn new() -> Self {
        Self {}
    }

    /// Spawn an actor running `body`, returning its handle.
    ///
    /// B2 stub.
    pub fn spawn<F>(&self, _body: F) -> Result<ActorRef, ActorError>
    where
        F: FnOnce(&mut cs_runtime::Runtime) + Send + 'static,
    {
        Err(ActorError::NoSystem)
    }
}

impl Default for ActorSystem {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_displays() {
        let p = ActorPid {
            node: 0,
            local_id: 42,
        };
        assert_eq!(p.to_string(), "<0.42>");
    }

    #[test]
    fn stub_spawn_errors() {
        let sys = ActorSystem::new();
        let r = sys.spawn(|_rt| ());
        assert!(matches!(r, Err(ActorError::NoSystem)));
    }
}
