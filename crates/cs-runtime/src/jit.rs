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
use cs_vm::jit_translate::bytecode_to_rir_full;
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
/// Thread-local poison flag — set when any prior JIT compile on
/// this thread panicked. Once set, `jit_tier_up_hook` returns
/// early without touching the Lowerer. Rationale: a Cranelift
/// panic indicates our lowering produced IR that violates
/// Cranelift's invariants. Empirical (maze): the workload that
/// triggers the panic ALSO has functions whose uniform-NB
/// translation compiles "cleanly" but produces wrong native code
/// (negative vector indices) — those don't panic at compile time
/// but corrupt runtime state. The safest containment after seeing
/// a panic is to stop attempting JIT compilation on this thread
/// for the rest of the process. Functions stay on the bytecode VM
/// (which is correct by construction).
///
/// Set never reset within a process: a panic indicates a JIT bug
/// that won't be fixed at runtime. The runtime keeps running
/// correctly on the VM tier.
std::thread_local! {
    static JIT_POISONED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn jit_tier_up_hook(closure: &VmClosure, args: &[Value]) {
    if JIT_POISONED.with(|p| p.get()) {
        return;
    }
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
    // Phase 5.4: prefer typer-derived hints when the user
    // annotated this function. The Runtime's
    // `typer_hints_by_lambda_id` map (populated by
    // `install_typer_hints` after the typer ran) is keyed by the
    // lambda's process-wide unique id. When present, treat as
    // authoritative — annotations are the user's explicit promise
    // and beat observation, which is single-sample and can mis-
    // specialize on polymorphic call sites. Hints get padded with
    // Any to args.len() in case the annotation was partial.
    let lambda_id = lam.profile.lambda_id;
    let param_hints: Vec<RirType> = {
        let typer = rt.typer_hints_by_lambda_id.borrow();
        if let Some(h) = typer.get(&lambda_id) {
            let mut v = h.clone();
            v.resize(args.len(), RirType::Any);
            v
        } else {
            drop(typer);
            // Observation-based fallback: derive each param's type
            // from the value passed at the triggering call. Args
            // that don't match any of our immediate types
            // (heap-pointer Values) hint as Any.
            args.iter()
                .map(|v| match v {
                    Value::Number(Number::Fixnum(_)) => RirType::Fixnum,
                    Value::Number(Number::Flonum(_)) => RirType::Flonum,
                    Value::Boolean(_) => RirType::Boolean,
                    Value::Character(_) => RirType::Character,
                    // Heap-pointer Values hint as Any so the
                    // translator accepts them; the JIT body must
                    // consume each Any-tagged i64 linearly.
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
                    _ => RirType::Fixnum,
                })
                .collect()
        }
    };
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
    //
    // Phase 6 Stage A iter 2: pass `Some(&closure.env)` as the
    // caller_env so the translator's CallGeneral splice site can
    // resolve free-var callees to top-level VmClosure bindings and
    // attempt leaf-callee inlining. `inline_depth = 0` because this
    // is the top-level body of a JIT'd function.
    let rir = match bytecode_to_rir_full(
        lam,
        "anon-jit",
        closure.self_name(),
        Some(&param_hints),
        Some(&closure.env),
        0,
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
    // Stage 3 iter 3.7 — uniform-NB enabled as the default tier with
    // specialized as fallback. Bodies the uniform-NB tier rejects
    // (non-tail `CallSelf` — the rejection guard catches host-stack
    // hazards upfront) fall through to specialized. Bodies neither
    // tier handles stay on bytecode.
    //
    // Prereq review:
    //  1. Non-tail `CallSelf` host-stack overflow: mitigated by the
    //     upfront rejection in `compile_uniform_nb`; affected bodies
    //     route to specialized or bytecode.
    //  2. GC stack-map walker: `cs_vm::jit_stackmap::scan_frame` has
    //     no production consumer (verified). Marked slots are
    //     bookkeeping; refcounting via `Rc::into_raw_jit` keeps live
    //     NB payloads alive across `collect()` independently. Heap
    //     trace via `Bindings::Trace` walks NB-encoded slots through
    //     the `ManuallyDrop` borrow pattern.
    // Both `compile_*` paths can panic from inside Cranelift's
    // codegen when our lowering produces an IR shape Cranelift's
    // internal assertions reject (e.g., the v36 `remove_constant_
    // phis` pass's `entry block unknown` expect on patterns we
    // currently lower invalidly — maze/sboyer/t3a-tree-rewriter
    // etc.). Aborting the host process for a tier-up attempt is
    // never the right behavior: the bytecode VM is always a
    // correct fallback. `AssertUnwindSafe` is sound because we
    // treat any panic as "drop this JIT attempt entirely" — we
    // never observe the Lowerer state after a panic to draw
    // conclusions; we just stop trying to JIT this closure and
    // leave it on the VM.
    //
    // Critical: on a uniform-NB **panic**, we do NOT fall through
    // to `compile_pure_fixnum`. The two tiers cover overlapping
    // RIR subsets — a function uniform-NB rejected via panic
    // (rather than via a clean `Err`) is one whose RIR shape may
    // also trip `compile_pure_fixnum`'s codegen invariants in
    // ways that don't abort cleanly. Empirical: some maze
    // functions where uniform-NB panicked lowered through
    // `compile_pure_fixnum` without panic, but the resulting
    // native code computed bogus vector indices (negative
    // offsets) that crashed `vm_vector_ref_gc` at runtime. The
    // pre-panic-catch behavior was "process aborts → user never
    // sees this" which hid the pure-fixnum miscompile; with the
    // catch in place the runtime symptom surfaces. Safest: any
    // panic means abandon both tiers for this closure.
    //
    // A clean uniform-NB `Err` (the prewalk's `Unsupported` /
    // `Codegen` returns) still falls through to pure_fixnum since
    // those cases are by-design tier-routing, not codegen
    // failures.
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let uniform_result = catch_unwind(AssertUnwindSafe(|| lowerer.compile_uniform_nb(&rir)));
    let (ptr, return_tag_override) = match uniform_result {
        Ok(Ok(p)) => (p, Some(cs_vm::vm::JIT_RT_NB)),
        Err(_) => {
            // uniform-NB panicked — poison the JIT subsystem.
            // See the JIT_POISONED rationale above; the safest
            // response is to stop attempting JIT on this thread
            // entirely for the rest of the process.
            JIT_POISONED.with(|p| p.set(true));
            return;
        }
        Ok(Err(_)) => {
            // Clean rejection from uniform-NB — try the
            // specialized tier. pure_fixnum can also panic
            // (its codegen path overlaps cranelift's), so wrap
            // it the same way and poison on panic.
            let pure_result = catch_unwind(AssertUnwindSafe(|| lowerer.compile_pure_fixnum(&rir)));
            match pure_result {
                Ok(Ok(p)) => (p, None),
                Err(_) => {
                    JIT_POISONED.with(|p| p.set(true));
                    return;
                }
                Ok(Err(_)) => return,
            }
        }
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
    // Always compute the semantic return tag from `rir.return_type`
    // (what the body conceptually returns). For the specialized tier
    // this is also the ABI tag — the body emits raw i64 of that
    // shape. For uniform-NB the ABI tag is `JIT_RT_NB` (the body
    // emits a uniform NB i64 carrier) while the semantic tag still
    // describes what the body conceptually returns, so observability
    // surfaces like `jit-status` and `jit-introspection` can report
    // the user-visible type rather than the ABI carrier.
    let semantic_tag = match rir.return_type {
        RirType::Boolean => cs_vm::vm::JIT_RT_BOOLEAN,
        RirType::Character => cs_vm::vm::JIT_RT_CHARACTER,
        RirType::Flonum => cs_vm::vm::JIT_RT_FLONUM,
        RirType::Null => cs_vm::vm::JIT_RT_NULL,
        RirType::Symbol => cs_vm::vm::JIT_RT_SYMBOL,
        RirType::Any => cs_vm::vm::JIT_RT_ANY,
        _ => cs_vm::vm::JIT_RT_FIXNUM,
    };
    let rt_tag = return_tag_override.unwrap_or(semantic_tag);
    closure.set_jit_return_type(rt_tag);
    closure.set_jit_semantic_return_type(semantic_tag);
}
