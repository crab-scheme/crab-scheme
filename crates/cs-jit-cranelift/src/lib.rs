//! CrabScheme Cranelift JIT backend.
//!
//! Lowers `cs_rir::Function` to Cranelift `clif` IR and produces
//! native code via `cranelift-jit`.
//!
//! M6 iter 2 (this iter) lowers single-block, pure-Fixnum functions
//! (`LoadConst` + `Add`/`Sub`/`Mul`/`Lt`/`Eq` + `Param` + `Move` +
//! `Term::Return`). Iter 3+ adds Branch/Jump/Call.
//!
//! See `.spec-workflow/specs/jit-cranelift/{requirements,design}.md`
//! and `docs/adr/0007-jit-design.md`.

pub mod ic;
pub mod lowering;

pub use ic::{IcSlot, IcTable};
pub use lowering::Lowerer;

use std::collections::HashMap;

use cs_jit::{JitBackend, JitError, JitFn, TypeFeedback};

/// Cranelift-backed JIT backend.
///
/// Owns a single [`Lowerer`] (which owns the underlying Cranelift
/// `JITModule`); each `compile` populates the module with one more
/// function and stashes the resulting native pointer keyed by the
/// returned [`JitFn::cookie`]. The runtime's tier-up code consults
/// the cookie to retrieve the pointer for invocation.
pub struct CraneliftBackend {
    lowerer: Option<Lowerer>,
    next_cookie: u64,
    /// `JitFn::cookie` -> finalized native pointer. Populated by
    /// [`compile`]; consumed by the runtime via [`native_ptr`].
    finalized: HashMap<u64, *const u8>,
}

// SAFETY: The native pointers in `finalized` are owned by the
// `Lowerer`'s `JITModule`. Their lifetime is tied to the backend
// instance and the JIT memory mapping — both single-threaded.
// Mark Send so this can live on a Runtime that is itself Send.
unsafe impl Send for CraneliftBackend {}

impl Default for CraneliftBackend {
    fn default() -> Self {
        Self {
            lowerer: None,
            next_cookie: 0,
            finalized: HashMap::new(),
        }
    }
}

impl JitBackend for CraneliftBackend {
    fn name(&self) -> &str {
        "cranelift"
    }

    fn compile(&mut self, rir: &cs_rir::Function) -> Result<JitFn, JitError> {
        if self.lowerer.is_none() {
            self.lowerer = Some(Lowerer::new()?);
        }
        let lowerer = self.lowerer.as_mut().unwrap();
        let ptr = lowerer.compile_pure_fixnum(rir)?;
        let cookie = self.next_cookie;
        self.next_cookie += 1;
        self.finalized.insert(cookie, ptr);
        Ok(JitFn {
            backend: "cranelift",
            cookie,
            feedback: TypeFeedback::default(),
        })
    }

    fn dump_native(&self, _jf: &JitFn) -> Vec<u8> {
        // Iter 8 wires this to actual disassembly (`(jit-dump <proc>)`
        // REPL primitive). Iter 2 returns empty bytes; the runtime
        // doesn't expose dump until then.
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

    /// Look up the finalized native pointer for a `JitFn` cookie
    /// previously returned by [`compile`].
    pub fn native_ptr(&self, jf: &JitFn) -> Option<*const u8> {
        self.finalized.get(&jf.cookie).copied()
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
    fn compile_pure_fixnum_via_jit_backend_trait() {
        let mut b = CraneliftBackend::default();
        let f = lowering::testing::add_two_fixnums();
        let jf = b.compile(&f).expect("compile via JitBackend");
        assert_eq!(jf.backend, "cranelift");
        let ptr = b.native_ptr(&jf).expect("native_ptr present");
        // SAFETY: ptr is the address of a finalized native function
        // with the (i64, i64) -> i64 signature we declared.
        let func: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(ptr) };
        assert_eq!(func(40, 2), 42);
    }

    #[test]
    fn compile_multi_block_with_branch_runs_natively() {
        // Use the lowering test fixture: a 3-block function with a
        // Branch terminator. Verifies the JitBackend trait path
        // accepts multi-block IR (iter 4 wired this in).
        let mut b = CraneliftBackend::default();
        let mut f = cs_rir::Function::new("clamp");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(cs_rir::Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                cs_rir::Inst::LoadConst(cs_rir::Value(1), cs_rir::Const::Fixnum(10)),
                cs_rir::Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: cs_rir::Term::Branch(
                cs_rir::Value(2),
                cs_rir::BlockId(1),
                cs_rir::BlockId(2),
            ),
        });
        f.blocks.push(cs_rir::Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: cs_rir::Term::Return(cs_rir::Value(0)),
        });
        f.blocks.push(cs_rir::Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![
                cs_rir::Inst::LoadConst(cs_rir::Value(3), cs_rir::Const::Fixnum(2)),
                cs_rir::Inst::Mul(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
            ],
            terminator: cs_rir::Term::Return(cs_rir::Value(4)),
        });
        let jf = b.compile(&f).expect("compile multi-block");
        let ptr = b.native_ptr(&jf).expect("native_ptr present");
        let func: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
        assert_eq!(func(5), 5);
        assert_eq!(func(15), 30);
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
