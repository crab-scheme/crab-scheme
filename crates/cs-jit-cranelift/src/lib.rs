//! CrabScheme Cranelift JIT backend.
//!
//! Lowers `cs_rir::Function` to Cranelift `clif` IR and produces
//! native code via `cranelift-jit`.
//!
//! M6 iter 1 (this iter) ships only the backend scaffolding —
//! a struct that implements `JitBackend` but rejects every IR with
//! `JitError::Unsupported("not yet implemented")`. Iter 2 adds the
//! `LoadConst` and arithmetic lowerings; subsequent iters fill in
//! the rest of the IR per the schedule in `design.md`.
//!
//! See `.spec-workflow/specs/jit-cranelift/{requirements,design}.md`
//! and `docs/adr/0007-jit-design.md`.

use cs_jit::{JitBackend, JitError, JitFn, TypeFeedback};

/// Cranelift-backed JIT backend.
#[derive(Default)]
pub struct CraneliftBackend {
    next_cookie: u64,
    // Iter 2 wires in the JIT module + isa builder here:
    // module: cranelift_jit::JITModule,
    // ctx: cranelift_codegen::Context,
}

impl JitBackend for CraneliftBackend {
    fn name(&self) -> &str {
        "cranelift"
    }

    fn compile(&mut self, _rir: &cs_rir::Function) -> Result<JitFn, JitError> {
        // Iter 1 stub: every compile request is rejected. The runtime
        // falls back to the VM dispatch path. This lets us land the
        // trait + tier-up state machine without blocking on Cranelift
        // version pinning + lowering work.
        Err(JitError::Unsupported(
            "cranelift lowering scaffolded; instruction lowering lands in iter 2".into(),
        ))
    }

    fn dump_native(&self, _jf: &JitFn) -> Vec<u8> {
        Vec::new()
    }
}

impl CraneliftBackend {
    /// Allocate a fresh compilation cookie. Used by future lowering
    /// code to tag emitted functions.
    pub fn next_cookie(&mut self) -> u64 {
        let c = self.next_cookie;
        self.next_cookie += 1;
        c
    }

    /// Construct a placeholder `JitFn` for testing the tier-up
    /// plumbing without actually running native code. The runtime
    /// must check `JitFn::backend == "cranelift-stub"` and route to
    /// the VM path before this gets exercised in production.
    pub fn fake_compiled(&mut self) -> JitFn {
        JitFn {
            backend: "cranelift-stub",
            cookie: self.next_cookie(),
            feedback: TypeFeedback::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_cranelift() {
        let b = CraneliftBackend::default();
        assert_eq!(b.name(), "cranelift");
    }

    #[test]
    fn compile_returns_unsupported_for_now() {
        let mut b = CraneliftBackend::default();
        let f = cs_rir::Function::new("test");
        match b.compile(&f) {
            Err(JitError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn fake_compiled_assigns_distinct_cookies() {
        let mut b = CraneliftBackend::default();
        let a = b.fake_compiled();
        let c = b.fake_compiled();
        assert_ne!(a.cookie, c.cookie);
        assert_eq!(a.backend, "cranelift-stub");
    }

    #[test]
    fn dump_native_empty_for_stub() {
        let b = CraneliftBackend::default();
        let jf = JitFn {
            backend: "cranelift",
            cookie: 0,
            feedback: TypeFeedback::default(),
        };
        assert!(b.dump_native(&jf).is_empty());
    }
}
