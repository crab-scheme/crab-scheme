//! Leases + fencing tokens.
//!
//! Spec: `docs/research/sdk_spec/consistency.md` § M07. The hardest
//! distributed primitive to get right — Martin Kleppmann's "leases
//! alone are unsafe" critique. The fix is **monotonic fencing
//! tokens** issued atomically with the lease grant.
//!
//! Scaffold — real implementation in M07 iter A. The Raft group
//! whose state machine maps `lease-name -> (holder, token, deadline)`
//! issues tokens monotonically per resource.

/// A monotonically-increasing per-resource integer. Every lease grant
/// for the same resource name returns a strictly higher value than the
/// previous grant. Used as the **safety primitive** for protected
/// writes — protected resources reject calls whose token is below the
/// highest seen.
///
/// The deadline (`Lease::deadline_hlc`) is advisory; the token is
/// the actual safety mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct FencingToken(u64);

impl FencingToken {
    pub const fn from_raw(raw: u64) -> Self {
        FencingToken(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Knobs for `Lease::acquire`.
#[derive(Debug, Clone)]
pub struct LeaseConfig {
    pub ttl_ms: u64,
    pub renew_interval_ms: u64,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        // 30s TTL with 10s renew is the Kubernetes Lease default —
        // mature operational shape, copy.
        LeaseConfig {
            ttl_ms: 30_000,
            renew_interval_ms: 10_000,
        }
    }
}

/// A held lease. Contains the fencing token to attach to protected
/// writes and the deadline (HLC) at which the holder should renew.
#[derive(Debug, Clone)]
pub struct Lease {
    pub resource: String,
    pub holder: String,
    pub token: FencingToken,
    pub deadline_hlc_raw: u64,
}

impl Lease {
    pub fn new(
        resource: impl Into<String>,
        holder: impl Into<String>,
        token: FencingToken,
    ) -> Self {
        Lease {
            resource: resource.into(),
            holder: holder.into(),
            token,
            deadline_hlc_raw: 0,
        }
    }

    pub fn token(&self) -> FencingToken {
        self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fencing_tokens_are_orderable() {
        let a = FencingToken::from_raw(42);
        let b = FencingToken::from_raw(43);
        assert!(a < b);
        // Equality + hash sanity.
        assert_eq!(a, FencingToken::from_raw(42));
    }

    #[test]
    fn default_lease_config_matches_k8s_shape() {
        let c = LeaseConfig::default();
        assert_eq!(c.ttl_ms, 30_000);
        assert_eq!(c.renew_interval_ms, 10_000);
        // Renew interval is comfortably less than TTL — leaves room
        // for one missed renewal without lease loss.
        assert!(c.renew_interval_ms * 2 < c.ttl_ms);
    }

    #[test]
    fn lease_carries_token() {
        let lease = Lease::new("email-sender", "worker-7", FencingToken::from_raw(42));
        assert_eq!(lease.token().raw(), 42);
        assert_eq!(lease.holder, "worker-7");
    }
}
