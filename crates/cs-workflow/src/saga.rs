//! Sagas — orchestrated compensating transactions.
//!
//! Garcia-Molina & Salem 1987. A saga is a sequence of steps, each
//! with an optional compensation. On step failure the engine runs
//! compensations for completed steps in reverse order.
//!
//! Scaffold — engine integration lands in M08 iter E.

/// One step of a saga. The forward action and an optional
/// compensation. Both shapes are intentionally opaque here — concrete
/// types thread through the Scheme expander.
#[derive(Debug, Clone)]
pub struct SagaStep {
    pub name: String,
    pub has_compensation: bool,
    /// Pivot step — the last non-compensatable step (e.g., "email
    /// sent"). Beyond the pivot, only forward-recovery is possible.
    pub is_pivot: bool,
}

/// How compensations are run on failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationMode {
    /// Compensate completed steps in reverse start order. Default —
    /// matches Garcia-Molina & Salem's original specification.
    Reverse,
    /// Run all compensations concurrently. Faster recovery; requires
    /// each compensation to be idempotent and independent.
    Parallel,
}

impl Default for CompensationMode {
    fn default() -> Self {
        CompensationMode::Reverse
    }
}

#[derive(Debug, Clone, Default)]
pub struct Saga {
    pub steps: Vec<SagaStep>,
    pub mode: CompensationMode,
}

impl Saga {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, step: SagaStep) {
        self.steps.push(step);
    }

    /// Index of the pivot step, if any. Beyond this, the saga is in
    /// forward-recovery-only territory.
    pub fn pivot(&self) -> Option<usize> {
        self.steps.iter().position(|s| s.is_pivot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mode_is_reverse() {
        let s = Saga::new();
        assert_eq!(s.mode, CompensationMode::Reverse);
    }

    #[test]
    fn pivot_detection() {
        let mut s = Saga::new();
        s.add(SagaStep {
            name: "reserve-flight".into(),
            has_compensation: true,
            is_pivot: false,
        });
        s.add(SagaStep {
            name: "reserve-hotel".into(),
            has_compensation: true,
            is_pivot: false,
        });
        s.add(SagaStep {
            name: "send-itinerary".into(),
            has_compensation: false,
            is_pivot: true,
        });
        assert_eq!(s.pivot(), Some(2));
    }
}
