//! Awakeables — durable promises (Restate's primitive).
//!
//! A workflow can `make-awakeable`, suspend on it, and any actor on
//! any node can `(resolve-awakeable! id value)` to lift it back into
//! execution. Cleaner than signal-poll-loops.
//!
//! Scaffold — `Awakeable` value type only. Engine + cross-node
//! resolution land in M08 iter F.

/// Unique identifier for an awakeable. Stable across the cluster so
/// callers can pass it to external systems that resolve it later
/// (webhook callbacks, human approvals).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AwakeableId(pub String);

impl AwakeableId {
    pub fn new(s: impl Into<String>) -> Self {
        AwakeableId(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A handle to a durable promise. Engine integration: the workflow's
/// `(await awk)` suspends the journal at this point; later, when
/// `resolve-awakeable!` fires (from any actor on any node), the
/// journal records the resolution and the workflow resumes.
#[derive(Debug, Clone)]
pub struct Awakeable {
    pub id: AwakeableId,
    /// Optional descriptive payload (e.g. URL of the external system
    /// to call back). Not interpreted by the engine.
    pub note: Option<String>,
}

impl Awakeable {
    pub fn new(id: impl Into<String>) -> Self {
        Awakeable {
            id: AwakeableId::new(id),
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awakeable_id_round_trips() {
        let a = AwakeableId::new("awk-7");
        assert_eq!(a.as_str(), "awk-7");
    }

    #[test]
    fn awakeable_carries_note() {
        let mut a = Awakeable::new("awk-1");
        a.note = Some("waiting on approval webhook".into());
        assert!(a.note.is_some());
    }
}
