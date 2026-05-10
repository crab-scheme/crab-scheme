//! Runtime-side JIT integration (M6 iter 6).
//!
//! Wires the cs-vm tier-up hook to the cs-jit-cranelift backend:
//!
//! 1. [`Runtime::install_jit`] sets up a per-runtime [`Lowerer`]
//!    and registers the tier-up hook on the calling thread.
//! 2. When a [`cs_vm::vm::VmClosure`]'s call counter crosses the
//!    threshold, the hook fires.
//! 3. The hook recovers the active runtime via the same thread-
//!    local that `Runtime::with_active` set on entry to eval.
//! 4. It tries to translate the closure's bytecode body via
//!    [`cs_vm::jit_translate::bytecode_to_rir`]; on success, it
//!    JIT-compiles via the lowerer and installs the native function
//!    pointer on the closure (so subsequent calls dispatch through
//!    `try_dispatch_jit`).
//! 5. Translation failures are silent — the closure simply stays on
//!    the bytecode VM.
//!
//! Iter 6 limitations:
//! - Self-recursion isn't supported yet (the hook can't recover the
//!   closure's bound symbol from the bytecode alone). Recursive
//!   functions like fib stay on the VM until a later iter wires the
//!   binding name through.
//! - The JIT only compiles closures whose bodies contain the
//!   subset the translator handles (Const + LoadVar(param) + fixnum
//!   primops + Branch/Jump/Return). Closures over env / set! / etc.
//!   stay on the VM.

use cs_jit_cranelift::Lowerer;
use cs_vm::jit_translate::bytecode_to_rir;
use cs_vm::vm::{install_tier_up_hook, VmClosure};

use crate::Runtime;

impl Runtime {
    /// Install the JIT for this runtime: build a [`Lowerer`] and
    /// register the tier-up hook on the calling thread.
    ///
    /// The hook lifetime is bound to the thread the runtime runs
    /// on. Calling this on a runtime that already has the JIT
    /// installed is a no-op (the lowerer is reused; the hook is
    /// re-registered idempotently).
    ///
    /// # Errors
    ///
    /// Returns the underlying [`cs_jit::JitError`] if the lowerer
    /// fails to initialize (e.g., unsupported host ISA).
    pub fn install_jit(&mut self) -> Result<(), cs_jit::JitError> {
        if self.jit_lowerer.is_none() {
            self.jit_lowerer = Some(Lowerer::new()?);
        }
        install_tier_up_hook(Some(jit_tier_up_hook));
        Ok(())
    }

    /// Whether the JIT has been installed on this runtime.
    pub fn jit_installed(&self) -> bool {
        self.jit_lowerer.is_some()
    }
}

/// Tier-up hook installed by [`Runtime::install_jit`]. Compiles
/// the closure's bytecode body via the bytecode→RIR translator and
/// the Cranelift lowerer; on success, stashes the native function
/// pointer on the closure.
///
/// Silent on failure: any unsupported opcode, env access, or
/// translation error leaves the closure on the bytecode VM.
fn jit_tier_up_hook(closure: &VmClosure) {
    // SAFETY: The hook fires only inside the closure-call dispatch,
    // which runs inside `Runtime::with_active` (set by `eval_str` /
    // `eval_str_via_vm`). The active back-pointer is the unique
    // mutable access for this call.
    let rt = match unsafe { Runtime::active() } {
        Some(rt) => rt,
        None => return,
    };
    let lowerer = match rt.jit_lowerer.as_mut() {
        Some(l) => l,
        None => return,
    };
    let lam = &closure.bc.lambdas[closure.lambda_idx];
    // Self-name flows from VmClosure::self_name (set by the Define
    // / Set call sites in cs-vm), letting the translator recognize
    // recursive `LoadVar(self) ... Call N` patterns.
    let rir = match bytecode_to_rir(lam, "anon-jit", closure.self_name()) {
        Ok(r) => r,
        Err(_) => return,
    };
    let ptr = match lowerer.compile_pure_fixnum(&rir) {
        Ok(p) => p,
        Err(_) => return,
    };
    closure.set_jit_ptr(ptr, lam.params.len() as u32);
    // Phase-2 ABI generalization: tell the dispatcher how to decode
    // the i64 return. Defaults to Fixnum; flip to Boolean when the
    // RIR's inferred return type says so.
    let rt_tag = match rir.return_type {
        cs_vm::RirType::Boolean => cs_vm::vm::JIT_RT_BOOLEAN,
        _ => cs_vm::vm::JIT_RT_FIXNUM,
    };
    closure.set_jit_return_type(rt_tag);
}
