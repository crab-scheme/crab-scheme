//! Per-type allocation histogram (cs-i6p.1), gated on
//! `feature = "alloc-histogram"` so the default hot path stays
//! a single thread-local add.
//!
//! Buckets: `pair` / `vector` / `string` / `closure` / `bignum`
//! / `other`. Classification is by `T`'s (monomorphized, hence
//! per-callsite-constant) `type_name`, matched once per
//! `Gc::new::<T>` call — cheap relative to the allocation
//! itself, and only compiled in when the feature is on.
//!
//! **`closure` and `bignum` are always 0.** `Value::Procedure`
//! is `Rc<Box<dyn Procedure>>` and `Value::BigNumber` is
//! `Rc<num_bigint::BigInt>` (see `cs-core/src/value.rs`) —
//! plain `Rc`, not `cs_gc::Gc<T>`. They never call
//! `record_alloc` in the first place (same gap the module doc
//! notes for regions), so there is no hook here to bump. The
//! buckets are kept in the enum/shape anyway so callers that
//! expect the full six-category shape (matching the categories
//! named in cs-i6p.1) don't have to special-case two missing
//! keys — they just always read zero, which is honest.

use std::any::type_name;
use std::sync::atomic::{AtomicU64, Ordering};

/// One `(count, bytes)` pair per bucket.
#[derive(Default)]
struct Bucket {
    count: AtomicU64,
    bytes: AtomicU64,
}

impl Bucket {
    const fn new() -> Self {
        Bucket {
            count: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    fn bump(&self, bytes: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.bytes.store(0, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (u64, u64) {
        (
            self.count.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
        )
    }
}

static PAIR: Bucket = Bucket::new();
static VECTOR: Bucket = Bucket::new();
static STRING: Bucket = Bucket::new();
static CLOSURE: Bucket = Bucket::new();
static BIGNUM: Bucket = Bucket::new();
static OTHER: Bucket = Bucket::new();

/// Classify `T` into one of the six histogram buckets by its
/// (monomorphized, compile-time-constant per callsite)
/// `type_name`. Matches against the concrete `Gc<T>` payload
/// types used by `cs-core::Value` — see the module doc for
/// exactly which `Value` variant maps to which `T`.
fn classify<T>() -> &'static Bucket {
    let name = type_name::<T>();
    if name.contains("Pair") {
        &PAIR
    } else if name.contains("CsStr") {
        &STRING
    } else if name.contains("Vec<") {
        // `RefCell<Vec<Value>>` (vectors) and `RefCell<Vec<u8>>`
        // (bytevectors) both land here — cs-i6p.1's requested
        // categories don't split them, and bytevectors are the
        // rarer case.
        &VECTOR
    } else {
        &OTHER
    }
}

/// Record one allocation of `T` (`size_of::<T>() +
/// RC_HEADER_BYTES` bytes) into its histogram bucket. Called
/// from `record_alloc<T>` only under `feature =
/// "alloc-histogram"`.
#[inline]
pub(crate) fn record<T>() {
    let bytes = std::mem::size_of::<T>() as u64 + (2 * std::mem::size_of::<usize>()) as u64;
    classify::<T>().bump(bytes);
}

/// Zero every bucket, including the always-zero `closure` /
/// `bignum` ones (kept for shape symmetry).
pub(crate) fn reset() {
    PAIR.reset();
    VECTOR.reset();
    STRING.reset();
    CLOSURE.reset();
    BIGNUM.reset();
    OTHER.reset();
}

/// `(count, bytes)` for each of the six buckets, in the fixed
/// order `pair, vector, string, closure, bignum, other`.
pub fn snapshot() -> [(&'static str, u64, u64); 6] {
    let (pc, pb) = PAIR.snapshot();
    let (vc, vb) = VECTOR.snapshot();
    let (sc, sb) = STRING.snapshot();
    let (cc, cb) = CLOSURE.snapshot();
    let (nc, nb) = BIGNUM.snapshot();
    let (oc, ob) = OTHER.snapshot();
    [
        ("pair", pc, pb),
        ("vector", vc, vb),
        ("string", sc, sb),
        ("closure", cc, cb),
        ("bignum", nc, nb),
        ("other", oc, ob),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gc;
    use std::cell::RefCell;

    struct FakePair {
        _a: i64,
        _b: i64,
    }

    // Every one of these needs an exact post-`reset()` bucket
    // value, so — like the exact-value tests in
    // `alloc_telemetry::tests` — each runs itself in an isolated
    // subprocess via `run_isolated` rather than inline, to avoid
    // racing every other Gc-allocating test in the binary (which
    // also bumps these same buckets whenever `alloc-histogram` is
    // enabled, since `record_alloc` calls `histogram::record`
    // unconditionally under the feature). See
    // `alloc_telemetry::run_isolated`'s doc for the full
    // rationale.

    #[test]
    fn other_bucket_bumps_for_unclassified_types() {
        crate::alloc_telemetry::run_isolated(
            "alloc_telemetry::histogram::tests::other_bucket_bumps_for_unclassified_types_isolated",
        );
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn other_bucket_bumps_for_unclassified_types_isolated() {
        reset();
        let _g: Gc<i64> = Gc::new(1);
        let (_, count, bytes) = snapshot()[5];
        assert_eq!(count, 1);
        assert!(bytes > 0);
    }

    #[test]
    fn vector_bucket_bumps_for_vec_payload() {
        crate::alloc_telemetry::run_isolated(
            "alloc_telemetry::histogram::tests::vector_bucket_bumps_for_vec_payload_isolated",
        );
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn vector_bucket_bumps_for_vec_payload_isolated() {
        reset();
        let _g: Gc<RefCell<Vec<i64>>> = Gc::new(RefCell::new(vec![1, 2, 3]));
        let (_, count, _) = snapshot()[1];
        assert_eq!(count, 1);
    }

    #[test]
    fn closure_and_bignum_buckets_stay_zero() {
        crate::alloc_telemetry::run_isolated(
            "alloc_telemetry::histogram::tests::closure_and_bignum_buckets_stay_zero_isolated",
        );
    }

    #[test]
    #[ignore = "run only via run_isolated, in its own subprocess"]
    fn closure_and_bignum_buckets_stay_zero_isolated() {
        reset();
        let _g: Gc<FakePair> = Gc::new(FakePair { _a: 1, _b: 2 });
        let snap = snapshot();
        assert_eq!(snap[3], ("closure", 0, 0));
        assert_eq!(snap[4], ("bignum", 0, 0));
    }
}
