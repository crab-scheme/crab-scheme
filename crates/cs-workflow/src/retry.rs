//! Retry policy — exponential backoff with jitter.
//!
//! Spec: `docs/research/sdk_spec/durable-execution.md` § Retry policy.
//! Defaults match Temporal: 1s × 2.0^n, capped at 100s, unlimited
//! attempts.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub initial_interval: Duration,
    pub backoff_coefficient: f64,
    pub max_interval: Duration,
    /// `None` means unlimited.
    pub max_attempts: Option<u32>,
    /// Names of condition types that should NOT be retried (e.g.
    /// `"&authentication"`, `"&invalid-input"`). Matching against
    /// the Scheme-side condition tag.
    pub non_retryable: Vec<String>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            initial_interval: Duration::from_secs(1),
            backoff_coefficient: 2.0,
            max_interval: Duration::from_secs(100),
            max_attempts: None,
            non_retryable: Vec::new(),
        }
    }
}

impl RetryPolicy {
    /// Compute the delay before attempt N (1-indexed). Jitter is the
    /// caller's responsibility — typically half-jitter on top of the
    /// computed delay (i.e. final = rand(0.5, 1.0) × delay). The pure
    /// function makes this trivially testable.
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return self.initial_interval;
        }
        let nanos = self.initial_interval.as_nanos() as f64
            * self.backoff_coefficient.powi((attempt - 1) as i32);
        let capped = nanos.min(self.max_interval.as_nanos() as f64);
        Duration::from_nanos(capped as u64)
    }

    pub fn should_retry(&self, attempt: u32, condition_tag: &str) -> bool {
        if self.non_retryable.iter().any(|t| t == condition_tag) {
            return false;
        }
        match self.max_attempts {
            Some(max) => attempt < max,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_temporal_defaults() {
        let p = RetryPolicy::default();
        assert_eq!(p.initial_interval, Duration::from_secs(1));
        assert_eq!(p.backoff_coefficient, 2.0);
        assert_eq!(p.max_interval, Duration::from_secs(100));
        assert!(p.max_attempts.is_none());
    }

    #[test]
    fn backoff_grows_exponentially_to_cap() {
        let p = RetryPolicy::default();
        let d1 = p.delay_for_attempt(1);
        let d2 = p.delay_for_attempt(2);
        let d3 = p.delay_for_attempt(3);
        assert_eq!(d1, Duration::from_secs(1));
        assert_eq!(d2, Duration::from_secs(2));
        assert_eq!(d3, Duration::from_secs(4));
        // Cap kicks in eventually.
        let dN = p.delay_for_attempt(20);
        assert_eq!(dN, p.max_interval);
    }

    #[test]
    fn non_retryable_short_circuits() {
        let mut p = RetryPolicy::default();
        p.non_retryable.push("&authentication".into());
        assert!(!p.should_retry(1, "&authentication"));
        assert!(p.should_retry(1, "&network"));
    }

    #[test]
    fn max_attempts_bounds_retry() {
        let mut p = RetryPolicy::default();
        p.max_attempts = Some(3);
        assert!(p.should_retry(1, "&x"));
        assert!(p.should_retry(2, "&x"));
        assert!(!p.should_retry(3, "&x"));
    }
}
