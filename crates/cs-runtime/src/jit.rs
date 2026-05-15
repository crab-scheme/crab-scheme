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
use cs_vm::jit_translate::bytecode_to_rir_with_hints;
use cs_vm::vm::{install_tier_up_hook, VmClosure};
use cs_vm::RirType;

use cs_core::{Number, Value};

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
fn jit_tier_up_hook(closure: &VmClosure, args: &[Value]) {
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
    // Type-feedback signature: derive each param's type from the
    // value passed at the triggering call. The translator uses this
    // to seed value_types so flonum-arg bodies dispatch through
    // FlonumAdd/Mul/etc. directly. Args that don't match any of our
    // immediate types (heap-pointer Values) fall back to Fixnum,
    // and the dispatcher type-guard will reject them anyway.
    let param_hints: Vec<RirType> = args
        .iter()
        .map(|v| match v {
            Value::Number(Number::Fixnum(_)) => RirType::Fixnum,
            Value::Number(Number::Flonum(_)) => RirType::Flonum,
            Value::Boolean(_) => RirType::Boolean,
            Value::Character(_) => RirType::Character,
            // Heap-pointer Values (Pair, Vector, String, ...) hint
            // as Type::Any so the translator accepts them and the
            // dispatcher boxes them via value_to_any_i64. The JIT
            // body must consume each Any-tagged i64 linearly (via
            // car/cdr/pair?/null?/return) — multi-use of the same
            // RirValue isn't yet supported on the Any lane.
            Value::Pair(_)
            | Value::Vector(_)
            | Value::String(_)
            | Value::ByteVector(_)
            | Value::Hashtable(_)
            | Value::Port(_)
            | Value::Promise(_)
            | Value::Symbol(_)
            | Value::Null
            | Value::Procedure(_) => RirType::Any,
            // Unspecified / Eof we leave as Fixnum for now — they
            // can't be passed to any of the Any-consumers we
            // currently lower, so they'll deopt at the type guard
            // and tier back down to bytecode.
            _ => RirType::Fixnum,
        })
        .collect();
    let param_tags: Vec<u8> = param_hints
        .iter()
        .map(|t| match t {
            RirType::Boolean => cs_vm::vm::JIT_RT_BOOLEAN,
            RirType::Character => cs_vm::vm::JIT_RT_CHARACTER,
            RirType::Flonum => cs_vm::vm::JIT_RT_FLONUM,
            RirType::Any => cs_vm::vm::JIT_RT_ANY,
            _ => cs_vm::vm::JIT_RT_FIXNUM,
        })
        .collect();
    // Self-name flows from VmClosure::self_name (set by the Define
    // / Set call sites in cs-vm), letting the translator recognize
    // recursive `LoadVar(self) ... Call N` patterns.
    let rir = match bytecode_to_rir_with_hints(
        lam,
        "anon-jit",
        closure.self_name(),
        Some(&param_hints),
    ) {
        Ok(r) => r,
        Err(_) => return,
    };
    // ADR 0012 D-1 (closure-env capture fix) — record whether the
    // body builds a nested closure. If so, `try_dispatch_jit` must
    // install a params-bound invocation-frame env on JIT_CALLER_ENV
    // (not the bare definition env) so `vm_make_closure` captures
    // this invocation's parameters. Computed before `compile_*`
    // since the lowerer may drain parts of the RIR.
    let builds_closures = rir.builds_closures();
    // Stage 3 iter 3.6 status: the uniform-NB baseline tier is
    // fully built (see `Lowerer::compile_uniform_nb`) but NOT yet
    // wired as the default tier-up target. Two prerequisites remain:
    //   1. Non-tail `CallSelf` would emit a regular Cranelift `call`
    //      and burn host stack on deep recursions. The lowerer
    //      rejects those bodies via an upfront check, but
    //      compounding that with the specialized tier's coverage
    //      story needs careful sequencing.
    //   2. The GC stack-map walker expects `Gc<Value>` raw handles
    //      at marked SSA slots; uniform-NB bodies carry direct
    //      `NB_TAG_PAIR` payloads (raw `Gc<Pair>` ptrs). A `collect()`
    //      mid-body crashes when the walker reads the slot. Fix is
    //      to teach the walker about NB encoding (or hold off marking
    //      pointer-tagged NBs as roots until then).
    // Both items are tracked as follow-ups. Keep specialized as the
    // sole tier for now — uniform-NB is callable from tests/benches
    // via `compile_uniform_nb` directly.
    let (ptr, return_tag_override) = match lowerer.compile_pure_fixnum(&rir) {
        Ok(p) => (p, None),
        Err(_) => return,
    };
    closure.set_jit_ptr(ptr, lam.params.len() as u32);
    closure.set_jit_param_types(&param_tags);
    closure.set_jit_needs_frame_env(builds_closures);
    // ADR 0012 D-2 (iter BM) — install the harvested stack-map
    // registry on the closure. Empty record-set is fine (means no
    // call inside the body kept a Gc handle live across it). The
    // GC scanner (iter BN) will use these maps to walk JIT frames.
    if !lowerer.last_inner_stack_maps.is_empty() {
        let mut maps = cs_vm::jit_stackmap::JitStackMaps::new(lowerer.last_inner_base);
        for (pc, offsets) in lowerer.last_inner_stack_maps.drain() {
            maps.insert(pc, offsets);
        }
        closure.set_jit_stack_maps(std::rc::Rc::new(maps));
    }
    // For the uniform-NB tier the return type tag is `JIT_RT_NB`
    // (the boundary passes the i64 through unchanged). For the
    // specialized tier, decode per `rir.return_type` as before.
    let rt_tag = match return_tag_override {
        Some(t) => t,
        None => match rir.return_type {
            RirType::Boolean => cs_vm::vm::JIT_RT_BOOLEAN,
            RirType::Character => cs_vm::vm::JIT_RT_CHARACTER,
            RirType::Flonum => cs_vm::vm::JIT_RT_FLONUM,
            RirType::Null => cs_vm::vm::JIT_RT_NULL,
            RirType::Symbol => cs_vm::vm::JIT_RT_SYMBOL,
            RirType::Any => cs_vm::vm::JIT_RT_ANY,
            _ => cs_vm::vm::JIT_RT_FIXNUM,
        },
    };
    closure.set_jit_return_type(rt_tag);
}
