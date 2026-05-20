//! Event-sourced journal per workflow execution.
//!
//! Spec: `docs/research/sdk_spec/durable-execution.md` § Engine
//! internals. Storage backends are pluggable (`Journal` trait) —
//! `JournalInMemory` for tests, `JournalCsTable` for embedded,
//! `JournalCsConsensus` for HA, `JournalPostgres` for prod (all in
//! M08 iters B + H).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

pub type WorkflowId = String;
pub type RunId = String;
pub type Sequence = u64;

/// One event in a workflow's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: Sequence,
    pub kind: EventKind,
}

/// The variants recognized by the replay engine. Adding a new variant
/// is a journal-format change — bump the format version (M08 iter B)
/// and ship migration code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    WorkflowStarted { input: Vec<u8> },
    ActivityScheduled { name: String, args_hash: [u8; 32] },
    ActivityCompleted { value: Vec<u8> },
    ActivityFailed { reason: String, attempt: u32 },
    TimerStarted { fire_at_hlc: u64 },
    TimerFired,
    SignalReceived { name: String, value: Vec<u8> },
    AwakeableCreated { id: String },
    AwakeableResolved { id: String, value: Vec<u8> },
    SideEffectRecorded { key: String, value: Vec<u8> },
    ChildWorkflowStarted { child_run_id: RunId },
    ChildWorkflowCompleted { value: Vec<u8> },
    WorkflowCompleted { value: Vec<u8> },
    WorkflowFailed { reason: String },
}

/// Trait every storage backend implements.
pub trait Journal: Send + Sync + std::fmt::Debug {
    fn append(&self, run_id: &RunId, events: &[Event]) -> Result<(), JournalError>;
    fn read(&self, run_id: &RunId) -> Result<Vec<Event>, JournalError>;
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("storage: {0}")]
    Storage(String),
    #[error("run not found: {0}")]
    NotFound(String),
}

/// In-memory backend — for tests and the M08 iter A bring-up. Real
/// production runs use `cs-table` (embedded) or Postgres
/// (`cs-stdlib-postgres`).
#[derive(Debug, Default)]
pub struct JournalInMemory {
    inner: Mutex<HashMap<RunId, Vec<Event>>>,
}

impl JournalInMemory {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> MutexGuard<'_, HashMap<RunId, Vec<Event>>> {
        self.inner.lock().expect("journal mutex poisoned")
    }
}

impl Journal for JournalInMemory {
    fn append(&self, run_id: &RunId, events: &[Event]) -> Result<(), JournalError> {
        let mut m = self.map();
        m.entry(run_id.clone())
            .or_default()
            .extend_from_slice(events);
        Ok(())
    }

    fn read(&self, run_id: &RunId) -> Result<Vec<Event>, JournalError> {
        Ok(self.map().get(run_id).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event { seq, kind }
    }

    #[test]
    fn in_memory_append_then_read_round_trips() {
        let j = JournalInMemory::new();
        let run: RunId = "run-1".into();
        j.append(
            &run,
            &[
                ev(
                    0,
                    EventKind::WorkflowStarted {
                        input: b"hi".to_vec(),
                    },
                ),
                ev(
                    1,
                    EventKind::ActivityScheduled {
                        name: "charge-card".into(),
                        args_hash: [0; 32],
                    },
                ),
            ],
        )
        .unwrap();
        let read = j.read(&run).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].seq, 0);
        assert_eq!(read[1].seq, 1);
    }

    #[test]
    fn read_missing_run_returns_empty_not_error() {
        let j = JournalInMemory::new();
        let read = j.read(&"absent".to_string()).unwrap();
        assert!(read.is_empty());
    }
}
