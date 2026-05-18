//! Shared helpers for the beam_* integration tests. Placed under
//! `tests/common/` (rather than `tests/common.rs`) so cargo
//! doesn't treat it as a standalone test target.

#![cfg(feature = "actor")]
#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Poll `pred` until it returns true or `timeout` elapses. Panics
/// with `msg` on timeout. Replaces the deadline-loop boilerplate
/// duplicated across the beam tests.
pub fn wait_until<F: FnMut() -> bool>(timeout: Duration, msg: &str, mut pred: F) {
    let deadline = Instant::now() + timeout;
    while !pred() {
        if Instant::now() >= deadline {
            panic!("{msg} (timeout after {:?})", timeout);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
