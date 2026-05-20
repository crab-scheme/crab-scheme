//! Phi-accrual failure detector (Hayashibara et al., SRDS 2004).
//!
//! Spec: `docs/research/sdk_spec/distributed.md` § M04, iter B.
//!
//! Tracks a sliding window of inter-arrival times for heartbeats from
//! one peer and exposes a continuous suspicion level `phi`. Higher
//! phi = more suspect. The membership layer applies a threshold
//! (`suspect=8`, `down=12`) to decide when to mark a peer unreachable.
//!
//! Scaffold only — concrete normal-distribution fit and CDF
//! computation deferred to M04 iter B.

use std::collections::VecDeque;

/// Default sliding-window size in samples.
pub const DEFAULT_WINDOW_SIZE: usize = 200;

/// Default minimum standard deviation, in milliseconds. Avoids
/// divide-by-zero on perfectly periodic peers.
pub const DEFAULT_MIN_STDDEV_MS: f64 = 100.0;

/// Default "acceptable heartbeat pause" — a free pass that absorbs
/// GC pauses, scheduler delays, NTP jumps, hypervisor freezes.
pub const DEFAULT_ACCEPTABLE_PAUSE_MS: u64 = 3_000;

/// Default suspect threshold. One false positive per ~10^8 sample
/// windows. (Akka default.)
pub const DEFAULT_SUSPECT_THRESHOLD: f64 = 8.0;

/// Default "definitely dead" threshold, set higher to require a
/// longer streak of missed heartbeats before eviction.
pub const DEFAULT_DOWN_THRESHOLD: f64 = 12.0;

/// Per-peer phi-accrual failure detector.
///
/// Scaffold — `phi()` always returns 0.0 in this stub; the
/// implementation lands in M04 iter B. The `min_stddev_ms` and
/// `acceptable_pause_ms` fields are configured here so the API is
/// stable, but they're not read until iter B's distribution fit
/// uses them.
#[derive(Debug, Clone)]
#[allow(dead_code)] // M04 iter B consumes min_stddev_ms / acceptable_pause_ms
pub struct PhiAccrualFailureDetector {
    window_size: usize,
    min_stddev_ms: f64,
    acceptable_pause_ms: u64,
    /// Most-recent heartbeat arrival times. Cleared on `reset`.
    arrivals_ms: VecDeque<u64>,
    last_arrival_ms: Option<u64>,
}

impl PhiAccrualFailureDetector {
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW_SIZE)
    }

    pub fn with_window(window_size: usize) -> Self {
        PhiAccrualFailureDetector {
            window_size,
            min_stddev_ms: DEFAULT_MIN_STDDEV_MS,
            acceptable_pause_ms: DEFAULT_ACCEPTABLE_PAUSE_MS,
            arrivals_ms: VecDeque::with_capacity(window_size),
            last_arrival_ms: None,
        }
    }

    /// Record a received heartbeat (or any traffic — every message
    /// resets the timer).
    pub fn heartbeat(&mut self, now_ms: u64) {
        if let Some(prev) = self.last_arrival_ms {
            let delta = now_ms.saturating_sub(prev);
            self.arrivals_ms.push_back(delta);
            while self.arrivals_ms.len() > self.window_size {
                self.arrivals_ms.pop_front();
            }
        }
        self.last_arrival_ms = Some(now_ms);
    }

    /// Current suspicion level, queried at `now_ms`. Implementation
    /// stubbed in this scaffold — always returns 0.0.
    pub fn phi(&self, _now_ms: u64) -> f64 {
        // M04 iter B: fit Normal(μ, σ) over `arrivals_ms`, compute
        // d = now_ms - last_arrival_ms, phi = -log10(1 - CDF(d, μ, σ)).
        0.0
    }

    /// Convenience: `phi < suspect_threshold`. Always true in this stub.
    pub fn is_available(&self, now_ms: u64) -> bool {
        self.phi(now_ms) < DEFAULT_SUSPECT_THRESHOLD
    }

    /// Reset the window (e.g. after a peer reconnects with a new epoch).
    pub fn reset(&mut self) {
        self.arrivals_ms.clear();
        self.last_arrival_ms = None;
    }
}

impl Default for PhiAccrualFailureDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_records_arrival_delta() {
        let mut d = PhiAccrualFailureDetector::new();
        d.heartbeat(1_000);
        // First sample has no predecessor — only `last_arrival_ms`
        // updates.
        assert_eq!(d.arrivals_ms.len(), 0);
        d.heartbeat(2_000);
        assert_eq!(d.arrivals_ms.len(), 1);
        assert_eq!(d.arrivals_ms[0], 1_000);
    }

    #[test]
    fn window_size_caps_samples() {
        let mut d = PhiAccrualFailureDetector::with_window(3);
        for i in 0..10 {
            d.heartbeat(i * 1000);
        }
        // 9 deltas were generated, but the window keeps only 3.
        assert_eq!(d.arrivals_ms.len(), 3);
    }

    #[test]
    fn reset_clears_state() {
        let mut d = PhiAccrualFailureDetector::new();
        d.heartbeat(0);
        d.heartbeat(1000);
        d.reset();
        assert!(d.arrivals_ms.is_empty());
        assert_eq!(d.last_arrival_ms, None);
    }

    #[test]
    fn stub_phi_is_always_zero_and_available() {
        // Scaffold contract: phi() is 0.0 until M04 iter B implements
        // the real distribution fit. Don't change without flipping
        // the implementation in the same iter.
        let d = PhiAccrualFailureDetector::new();
        assert_eq!(d.phi(1000), 0.0);
        assert!(d.is_available(1000));
    }
}
