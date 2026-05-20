//! `cs-workflow` — durable workflow engine for CrabScheme.
//!
//! Workflows are journal-replayed deterministic orchestrations;
//! activities are impure side-effecting workers; sagas compose with
//! explicit compensation; awakeables are durable promises (Restate's
//! contribution).
//!
//! Spec: `docs/research/sdk_spec/durable-execution.md` § M08; task list
//! `tasks/M08-durable-execution.md`.
//!
//! ## Status
//!
//! **Scaffold only.** Public type shapes: `Workflow`, `Activity`,
//! `RetryPolicy`, `Saga`, `Awakeable`, `Journal`, `Event`. Real
//! replay engine + storage backends ship in M08 iters A-H.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

use thiserror::Error;

pub mod awakeable;
pub mod journal;
pub mod retry;
pub mod saga;

pub use awakeable::{Awakeable, AwakeableId};
pub use journal::{Event, EventKind, Journal, JournalInMemory, RunId, Sequence, WorkflowId};
pub use retry::RetryPolicy;
pub use saga::{CompensationMode, Saga, SagaStep};

#[derive(Debug, Error)]
pub enum WorkflowError {
    /// Workflow code is non-deterministic — replay produced a
    /// different command than the journal records. The runtime
    /// freezes the workflow with this condition; the operator patches
    /// the workflow and resumes from the last good event.
    #[error("non-deterministic at event {at}: expected {expected}, got {got}")]
    NonDeterministic {
        at: u64,
        expected: String,
        got: String,
    },
    /// An activity exceeded its retry budget. The error chains the
    /// last underlying failure as a string.
    #[error("activity {name} failed after {attempts} attempts: {cause}")]
    ActivityExhausted {
        name: String,
        attempts: u32,
        cause: String,
    },
    /// Awakeable timed out before being resolved.
    #[error("awakeable {id:?} timed out after {ttl_ms}ms")]
    AwakeableTimeout { id: AwakeableId, ttl_ms: u64 },
    /// Journal storage failure (Postgres down, disk full, etc.).
    #[error("journal: {0}")]
    Journal(String),
    #[error("not implemented (cs-workflow scaffold; see docs/research/sdk_spec/tasks/M08-durable-execution.md)")]
    NotImplemented,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_deterministic_error_carries_event_index() {
        let e = WorkflowError::NonDeterministic {
            at: 17,
            expected: "ActivityScheduled(charge-card)".into(),
            got: "TimerStarted(60s)".into(),
        };
        let s = format!("{}", e);
        assert!(s.contains("17"));
        assert!(s.contains("charge-card"));
        assert!(s.contains("60s"));
    }
}
