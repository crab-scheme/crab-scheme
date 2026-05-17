//! `cs-supervisor` — OTP supervision trees.
//!
//! Per the spec at `docs/research/beam_runtime_spec.md`:
//! - Strategies: `one_for_one`, `one_for_all`, `rest_for_one`. Defer
//!   `simple_one_for_one` (worker pool case is covered by
//!   `one_for_one` + dynamic add).
//! - Restart intensity: `{max_restarts, period_seconds}` (default
//!   `{1, 5}` matches OTP — aggressive, prevents cascades).
//! - Shutdown: `BrutalKill | Timeout(ms) | Infinity`.
//!
//! ## Status
//!
//! Phase **B5** scaffold only.

#![allow(dead_code)]

use cs_actor::ActorPid;

#[derive(Debug, Clone, Copy)]
pub enum Strategy {
    OneForOne,
    OneForAll,
    RestForOne,
}

#[derive(Debug, Clone, Copy)]
pub enum Restart {
    /// Always restart, even on normal exit.
    Permanent,
    /// Restart only on abnormal exit.
    Transient,
    /// Never restart.
    Temporary,
}

#[derive(Debug, Clone, Copy)]
pub enum Shutdown {
    BrutalKill,
    Timeout(u64),
    Infinity,
}

/// Child role: a worker or a sub-supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildType {
    Worker,
    Supervisor,
}

/// Description of one child managed by a supervisor.
#[derive(Debug, Clone)]
pub struct ChildSpec {
    pub id: String,
    pub restart: Restart,
    pub shutdown: Shutdown,
    pub child_type: ChildType,
}

/// Handle to a running supervisor.
///
/// B5 stub.
pub struct SupervisorRef {
    pid: ActorPid,
}

impl SupervisorRef {
    pub fn pid(&self) -> ActorPid {
        self.pid
    }
}
