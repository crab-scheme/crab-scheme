//! CrabScheme JIT abstraction layer.
//!
//! Defines the `JitBackend` trait that each backend (Cranelift in
//! `cs-jit-cranelift`, HolyJIT in M7's `cs-jit-holy`) implements, plus
//! the per-procedure tier-up state machine and the deopt scaffolding.
//!
//! See `.spec-workflow/specs/jit-cranelift/{requirements,design}.md`
//! and `docs/adr/0007-jit-design.md`.

use std::sync::atomic::{AtomicU32, Ordering};

/// Default call count threshold for tier-up. After this many invocations
/// of a procedure, the runtime asks the JIT to compile it.
///
/// 1024 matches V8 / SpiderMonkey ballpark warmup. Tunable per `Tier`.
pub const DEFAULT_TIER_THRESHOLD: u32 = 1024;

/// Maximum number of deopt events before a procedure stays on the VM
/// permanently. Prevents tier-up oscillation around the threshold.
pub const MAX_DEOPT_RETRIES: u32 = 3;

/// Per-procedure tier state. Lives on the `Procedure` value alongside
/// its dispatch entry.
#[derive(Debug)]
pub struct Tier {
    /// Number of times this procedure has been called. Reset to 0 on
    /// successful tier-up; used purely for the warmup heuristic.
    counter: AtomicU32,

    /// Number of times this procedure has deopted. After
    /// `MAX_DEOPT_RETRIES`, the procedure stays Cold permanently.
    deopt_count: AtomicU32,

    /// Threshold at which this procedure tiers up.
    threshold: u32,
}

impl Default for Tier {
    fn default() -> Self {
        Self::with_threshold(DEFAULT_TIER_THRESHOLD)
    }
}

impl Tier {
    /// Tier with a custom threshold (test / benchmark hook).
    pub fn with_threshold(threshold: u32) -> Self {
        Self {
            counter: AtomicU32::new(0),
            deopt_count: AtomicU32::new(0),
            threshold,
        }
    }

    /// Bump the call counter. Returns `true` if this call is the one
    /// that crossed the tier-up threshold (i.e. the runtime should
    /// trigger compilation).
    pub fn bump(&self) -> bool {
        // If we've burned our deopt budget, never tier up again.
        if self.deopt_count.load(Ordering::Relaxed) >= MAX_DEOPT_RETRIES {
            return false;
        }
        let prev = self.counter.fetch_add(1, Ordering::Relaxed);
        prev + 1 == self.threshold
    }

    /// Record a deopt event. Returns `true` if the procedure should
    /// stay on the VM permanently after this deopt.
    pub fn record_deopt(&self) -> bool {
        let prev = self.deopt_count.fetch_add(1, Ordering::Relaxed);
        // Reset the call counter so we don't immediately re-tier-up.
        self.counter.store(0, Ordering::Relaxed);
        prev + 1 >= MAX_DEOPT_RETRIES
    }

    /// Current call count.
    pub fn count(&self) -> u32 {
        self.counter.load(Ordering::Relaxed)
    }

    /// Current deopt count.
    pub fn deopts(&self) -> u32 {
        self.deopt_count.load(Ordering::Relaxed)
    }

    /// Has this procedure exceeded the deopt budget? If so, it stays
    /// on the VM forever for this runtime instance.
    pub fn is_blacklisted(&self) -> bool {
        self.deopt_count.load(Ordering::Relaxed) >= MAX_DEOPT_RETRIES
    }
}

/// Coarse runtime type recorded in deopt events. The next compile uses
/// the union of types observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedType {
    Fixnum,
    Flonum,
    Boolean,
    Character,
    Pair,
    Vector,
    String,
    ByteVector,
    Procedure,
    /// Catchall — disables type specialization for this slot.
    Other,
}

/// Aggregated type observations for a procedure. The JIT consults this
/// when (re)compiling to decide whether to specialize each slot to a
/// concrete type or fall back to the dynamic path.
#[derive(Debug, Default, Clone)]
pub struct TypeFeedback {
    pub args: Vec<Vec<ObservedType>>, // per-arg observed types
}

/// Errors that a JIT backend can surface during compilation.
#[derive(Debug)]
pub enum JitError {
    /// The backend doesn't support some IR construct.
    Unsupported(String),
    /// Code generation failed for an internal reason.
    Codegen(String),
    /// Memory allocation for executable code failed.
    OutOfMemory,
}

impl std::fmt::Display for JitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JitError::Unsupported(s) => write!(f, "unsupported: {}", s),
            JitError::Codegen(s) => write!(f, "codegen: {}", s),
            JitError::OutOfMemory => write!(f, "out of memory"),
        }
    }
}

impl std::error::Error for JitError {}

/// Compiled native function. Opaque holder for a function pointer plus
/// the metadata the runtime needs to reuse it across calls and to
/// support `(jit-dump <proc>)`.
///
/// The actual function-pointer + memory-management lifecycle is owned
/// by the backend that produced it. `JitFn` carries a backend tag so
/// the runtime can route `dump_native` calls back to the right
/// backend.
#[allow(dead_code)]
pub struct JitFn {
    /// Backend that produced this. Used by `dump_native`.
    pub backend: &'static str,
    /// Cookie the backend uses to identify this function internally.
    pub cookie: u64,
    /// Type feedback that drove this compilation.
    pub feedback: TypeFeedback,
}

impl std::fmt::Debug for JitFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitFn")
            .field("backend", &self.backend)
            .field("cookie", &self.cookie)
            .field("feedback", &self.feedback)
            .finish()
    }
}

/// Trait every JIT backend implements. M6 ships `cs-jit-cranelift`;
/// M7 adds `cs-jit-holy` as a peer.
pub trait JitBackend: Send {
    /// Human-readable backend name (`"cranelift"`, `"holyjit"`, etc.).
    fn name(&self) -> &str;

    /// Compile a `cs_rir::Function` into native code. Returns a
    /// `JitFn` holder.
    fn compile(&mut self, rir: &cs_rir::Function) -> Result<JitFn, JitError>;

    /// Return the disassembled native bytes for a previously-compiled
    /// `JitFn`. Used by `(jit-dump <proc>)`.
    fn dump_native(&self, jf: &JitFn) -> Vec<u8>;
}

/// No-op backend — accepts every IR, produces a `JitFn` that's never
/// actually executed (the runtime falls back to the VM). Used for
/// testing the tier-up plumbing without requiring a real codegen
/// backend in the workspace.
#[derive(Default)]
pub struct NoopBackend {
    next_cookie: u64,
}

impl JitBackend for NoopBackend {
    fn name(&self) -> &str {
        "noop"
    }

    fn compile(&mut self, _rir: &cs_rir::Function) -> Result<JitFn, JitError> {
        let c = self.next_cookie;
        self.next_cookie += 1;
        Ok(JitFn {
            backend: "noop",
            cookie: c,
            feedback: TypeFeedback::default(),
        })
    }

    fn dump_native(&self, _jf: &JitFn) -> Vec<u8> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_default_threshold() {
        let t = Tier::default();
        assert_eq!(t.threshold, DEFAULT_TIER_THRESHOLD);
        assert_eq!(t.count(), 0);
        assert_eq!(t.deopts(), 0);
        assert!(!t.is_blacklisted());
    }

    #[test]
    fn tier_bumps_to_threshold() {
        let t = Tier::with_threshold(4);
        assert!(!t.bump()); // 1
        assert!(!t.bump()); // 2
        assert!(!t.bump()); // 3
        assert!(t.bump()); // 4 — crosses threshold
                           // Past the threshold, bumps don't return true again unless we
                           // reset (after a deopt).
        assert!(!t.bump());
    }

    #[test]
    fn tier_blacklists_after_max_deopts() {
        let t = Tier::with_threshold(4);
        // First deopt — not yet blacklisted.
        assert!(!t.record_deopt());
        assert!(!t.is_blacklisted());
        // Second deopt.
        assert!(!t.record_deopt());
        assert!(!t.is_blacklisted());
        // Third deopt — blacklisted.
        assert!(t.record_deopt());
        assert!(t.is_blacklisted());
        // Once blacklisted, bump never reports tier-up.
        for _ in 0..1000 {
            assert!(!t.bump());
        }
    }

    #[test]
    fn deopt_resets_call_counter() {
        let t = Tier::with_threshold(4);
        for _ in 0..3 {
            t.bump();
        }
        assert_eq!(t.count(), 3);
        t.record_deopt();
        assert_eq!(t.count(), 0);
    }

    #[test]
    fn noop_backend_compiles_anything() {
        let mut b = NoopBackend::default();
        assert_eq!(b.name(), "noop");
        let f = cs_rir::Function::new("test");
        let jf = b.compile(&f).unwrap();
        assert_eq!(jf.backend, "noop");
        assert_eq!(b.dump_native(&jf), Vec::<u8>::new());
    }

    #[test]
    fn noop_backend_assigns_distinct_cookies() {
        let mut b = NoopBackend::default();
        let f = cs_rir::Function::new("test");
        let jf1 = b.compile(&f).unwrap();
        let jf2 = b.compile(&f).unwrap();
        assert_ne!(jf1.cookie, jf2.cookie);
    }

    #[test]
    fn jit_error_display() {
        let e = JitError::Unsupported("foo".into());
        assert!(format!("{}", e).contains("foo"));
        let e = JitError::Codegen("bar".into());
        assert!(format!("{}", e).contains("bar"));
        let e = JitError::OutOfMemory;
        assert!(format!("{}", e).contains("memory"));
    }
}
