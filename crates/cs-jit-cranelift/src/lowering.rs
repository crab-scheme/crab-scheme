//! `cs_rir::Function` → Cranelift IR lowering.
//!
//! Iter 4b scope: pure-Fixnum function bodies with control flow
//! and self-recursion.
//!
//! - `LoadConst(Fixnum / Boolean / Null / Unspecified)`
//! - `Param`
//! - `Move`
//! - `Add`, `Sub`, `Mul`, `Lt`, `Eq` over i64
//! - `Term::Return`, `Term::Jump` (with block params),
//!   `Term::Branch`
//! - Multi-block functions (each RIR `BlockId` becomes a Cranelift
//!   `Block`; entry block carries the function-arg params, others
//!   carry IR-specified params).
//! - `Inst::CallSelf(dst, args)` — recursive self-call routed
//!   through `module.declare_func_in_func`. Lets fib / fact / any
//!   single-recursive function JIT end-to-end.
//!
//! Out of scope (planned for later iters):
//! - General `Call` (procedure-value callee with feedback)
//! - Closures / env access
//! - `DeoptCheck` (we currently trust the IR's type tags)
//! - Flonum / Boolean arithmetic (we lower booleans via i64
//!   representation 0/1 which is fine for `Lt`/`Eq` results but we
//!   don't yet specialize on `Type::Flonum`).
//!
//! Calling convention: every JITted function is exposed as
//! `extern "C" fn(i64, i64, ...) -> i64`. The runtime is responsible
//! for unboxing Scheme `Value::Number(Fixnum)` to i64 before the
//! call and re-boxing the i64 result. Richer ABIs (closure
//! self-reference, non-fixnum args) come once `Call` lands.

use std::collections::HashMap;

use cranelift_codegen::ir::{
    types::{F64, I64},
    AbiParam, Function as ClifFunction, InstBuilder, Signature, StackSlotData, StackSlotKind,
    UserFuncName,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use cs_jit::JitError;
#[cfg(test)]
use cs_rir::Block;
use cs_rir::{Const, Function as RirFunction, Inst, Term, Value as RirValue};

use crate::ic::IcTable;

/// Owns a Cranelift `JITModule` and emits one native function per
/// `compile_pure_fixnum` call.
///
/// One module per backend instance is fine for iter 2 (we never
/// re-define the same function); iter 3 may want a per-module
/// finalize pattern for tier-up.
pub struct Lowerer {
    module: JITModule,
    ctx: Context,
    func_ctx: FunctionBuilderContext,
    next_id: u64,
    /// User-stack-maps harvested from the most recently compiled
    /// inner function. Keyed by code offset (PC from buffer start);
    /// value is the list of SP-relative offsets that hold Gc<Value>
    /// raw handles at that safepoint. Read once per compile by
    /// `compile_pure_fixnum`'s caller, then overwritten by the next
    /// compile. ADR 0012 D-2 (iter BL).
    pub last_inner_stack_maps: HashMap<u32, Vec<i32>>,
    /// Base address of the most-recently-compiled inner function
    /// after `finalize_definitions`. Combined with
    /// `last_inner_stack_maps` keys (code offsets), this gives the
    /// absolute PC of each safepoint at runtime. Set by
    /// `compile_pure_fixnum` (iter BM).
    pub last_inner_base: *const u8,
    /// FuncId of the imported `vm_env_lookup_fixnum` helper.
    /// `Inst::EnvLookup` lowers to a Cranelift call against this.
    env_lookup_func: cranelift_module::FuncId,
    /// FuncId of the imported `vm_env_lookup_any` helper.
    /// `Inst::EnvLookupAny` lowers to a Cranelift call against
    /// this. Used by iter BU's translator when a free-var load
    /// flows to a `CallGeneral` callee position — the binding's
    /// shape is unknown at compile time, so we fetch a Gc handle.
    env_lookup_any_func: cranelift_module::FuncId,
    /// FuncId of the imported `vm_env_set_fixnum` helper.
    /// `Inst::EnvSet` lowers to a Cranelift call against this.
    env_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_pair(car, car_tag, cdr, cdr_tag) -> i64`.
    /// `Inst::Cons` lowers to a Cranelift call against this.
    alloc_pair_func: cranelift_module::FuncId,
    /// FuncId of `vm_pair_car(pair) -> i64`. Reserved for `car`
    /// lowering.
    #[allow(dead_code)]
    pair_car_func: cranelift_module::FuncId,
    /// FuncId of `vm_pair_cdr(pair) -> i64`. Reserved for `cdr`
    /// lowering.
    #[allow(dead_code)]
    pair_cdr_func: cranelift_module::FuncId,
    /// FuncId of `vm_pair_p(v) -> i64`. `Inst::PairP` lowers to a
    /// Cranelift call against this. Helper consumes the Any-tagged
    /// operand box.
    pair_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_null_p(v) -> i64`. `Inst::NullP` lowers to a
    /// Cranelift call against this.
    null_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_clone(v) -> i64`. `Inst::AnyClone`
    /// lowers to a Cranelift call against this. Helper does not
    /// consume the operand box.
    value_clone_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_drop(v) -> ()`. `Inst::AnyDrop` lowers
    /// to a Cranelift call against this. Helper consumes (drops)
    /// the operand box.
    value_drop_func: cranelift_module::FuncId,
    /// FuncId of `vm_box_typed(i, tag) -> i64`. `Inst::BoxTyped`
    /// lowers to a Cranelift call against this. Tag passes as i64
    /// (Cranelift has no u8 ABI param).
    box_typed_func: cranelift_module::FuncId,
    /// FuncId of `vm_unbox_fixnum(r) -> i64`. `Inst::AnyToFix`
    /// lowers to a Cranelift call against this. Helper consumes
    /// the Any-tagged box and panics on non-Fixnum runtime values.
    unbox_fixnum_func: cranelift_module::FuncId,
    /// FuncId of `vm_unbox_boolean(r) -> i64`. `Inst::AnyToBool`
    /// lowers to this. Consumes the box; returns 0/1.
    unbox_boolean_func: cranelift_module::FuncId,
    /// FuncId of `vm_unbox_flonum(r) -> i64`. `Inst::AnyToFlo`
    /// lowers to this. Consumes the box; returns the f64 bit
    /// pattern.
    unbox_flonum_func: cranelift_module::FuncId,
    /// FuncId of `vm_eq_any(a, b) -> i64`. `Inst::EqAny` lowers
    /// to this. Consumes both boxes; returns 0/1.
    eq_any_func: cranelift_module::FuncId,
    /// FuncId of `vm_any_truthy(r) -> i64`. `Inst::AnyTruthy`
    /// lowers to this. Consumes the box; returns 0 iff inner is
    /// `Boolean(false)`.
    any_truthy_func: cranelift_module::FuncId,
    /// FuncId of `vm_call_general(callee: i64, args_ptr: *const i64,
    /// n_args: usize) -> i64`. `Inst::CallGeneral` lowers to a
    /// Cranelift call against this — the slow-path miss handler for
    /// non-self, non-builtin closure calls (ADR 0012 D-1, iter BU).
    /// `usize` is modeled as `i64` in the Cranelift signature; the
    /// helper truncates internally.
    call_general_func: cranelift_module::FuncId,
    /// FuncId of `vm_closure_id_peek(callee: i64) -> u32`. Used by
    /// the IC hot path (ADR 0012 D-1, iter BY) to read a callee's
    /// closure id without consuming the Gc handle.
    closure_id_peek_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_vector_gc(n, fill) -> i64`.
    /// `Inst::VecAlloc` lowers to this (ADR 0012 D-2, iter BV).
    alloc_vector_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_ref_gc(vec, idx) -> i64`.
    vector_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_set_gc(vec, idx, x) -> i64`.
    vector_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_length_gc(vec) -> i64`. Returns raw
    /// Fixnum-shape i64 (NOT a Gc handle).
    vector_length_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_p_gc(v) -> i64`. Returns 0/1.
    vector_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_string_gc(n, fill) -> i64`. `Inst::StrAlloc`
    /// lowers to this (ADR 0012 D-2, iter BX). `fill` is a
    /// Fixnum-shape codepoint i64 (Character ABI), not a Gc handle.
    alloc_string_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ref_gc(s, idx) -> i64`. Returns a
    /// Fixnum-shape codepoint i64 (Character ABI carrier), NOT a
    /// Gc handle — so no stack-map declaration.
    string_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_length_gc(s) -> i64`. Returns raw
    /// Fixnum-shape i64 (char count, not byte count).
    string_length_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_p_gc(v) -> i64`. Returns 0/1.
    string_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_eq_gc(a, b) -> i64`. Returns 0/1.
    string_eq_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_closure(lambda_idx) -> i64`. Returns a
    /// fresh `Gc<Value::Procedure>` raw handle. ADR 0012 D-2 (iter BZ).
    make_closure_func: cranelift_module::FuncId,
    /// FuncId of `vm_length_gc(lst) -> i64`. Returns raw Fixnum-shape
    /// spine count. ADR 0012 D-2 (iter CA).
    length_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_p_gc(v) -> i64`. Returns 0/1. ADR 0012 D-2
    /// (iter CA).
    list_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_reverse_gc(lst) -> i64`. Returns a fresh Gc
    /// handle to a reversed list. ADR 0012 D-2 (iter CB).
    reverse_func: cranelift_module::FuncId,
    /// FuncId of `vm_memq_gc(item, lst) -> i64`. Returns a Gc
    /// handle (matched sublist or `#f`). ADR 0012 D-2 (iter CC).
    memq_func: cranelift_module::FuncId,
    /// FuncId of `vm_assq_gc(key, alist) -> i64`. Returns a Gc
    /// handle (matched `(k . v)` pair or `#f`). ADR 0012 D-2
    /// (iter CD).
    assq_func: cranelift_module::FuncId,
    /// FuncId of `vm_set_car_gc(p, v) -> i64`. Returns Gc(Unspecified).
    /// ADR 0012 D-2 (iter CE).
    set_car_func: cranelift_module::FuncId,
    /// FuncId of `vm_set_cdr_gc(p, v) -> i64`. ADR 0012 D-2 (iter CE).
    set_cdr_func: cranelift_module::FuncId,
    /// FuncId of `vm_memv_gc(item, lst) -> i64`. eqv?-flavored memq.
    /// ADR 0012 D-2 (iter CG).
    memv_func: cranelift_module::FuncId,
    /// FuncId of `vm_assv_gc(key, alist) -> i64`. eqv?-flavored assq.
    /// ADR 0012 D-2 (iter CG).
    assv_func: cranelift_module::FuncId,
    /// Per-module inline-cache slot storage. Indices into this
    /// table identify call sites; the slot's address is intended
    /// to be baked into JIT bodies as a constant pointer (ADR
    /// 0012 D-1; design in `docs/research/jit_inline_cache.md`).
    /// iter BR ships the table empty — call-site lowering (iter
    /// BS+) is what allocates and references entries. Exposed
    /// via [`Lowerer::ic_table_mut`] so future codegen can
    /// reserve slots without reaching into private fields.
    ic_table: IcTable,
}

impl Lowerer {
    /// Build a fresh lowerer, using the host ISA.
    pub fn new() -> Result<Self, JitError> {
        let mut flag_builder = settings::builder();
        // Use the default optimization level. Iter 2 doesn't need
        // tuning; iter 6+ (the perf iter) revisits.
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|e| JitError::Codegen(format!("flag use_colocated_libcalls: {e}")))?;
        flag_builder
            .set("is_pic", "false")
            .map_err(|e| JitError::Codegen(format!("flag is_pic: {e}")))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|e| JitError::Codegen(format!("native isa: {e}")))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| JitError::Codegen(format!("isa finish: {e}")))?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        // Register runtime helpers that JITted code calls. The
        // address comes from cs-vm's `extern "C"` function; the
        // linker resolves the symbol across workspace crates via
        // `#[no_mangle]`.
        builder.symbol(
            "vm_env_lookup_fixnum",
            cs_vm::vm::vm_env_lookup_fixnum as *const u8,
        );
        builder.symbol(
            "vm_env_lookup_any",
            cs_vm::vm::vm_env_lookup_any as *const u8,
        );
        builder.symbol(
            "vm_env_set_fixnum",
            cs_vm::vm::vm_env_set_fixnum as *const u8,
        );
        // ADR 0011 D-5 — heap-pointer ABI helpers. Pure-additive
        // imports today (no translator path uses them yet); subsequent
        // iters wire `cons` / `car` / `cdr` lowering through these
        // and unlock end-to-end Pair-returning JIT bodies.
        // ADR 0012 D-2 (iter BJ) — every Any-flavored runtime helper
        // now resolves to the `*_gc` variant. The symbol names stay
        // unchanged so we don't have to thread renames through every
        // declare_function / declare_func_in_func site; only the
        // resolved function address changes. The Box-flavored helpers
        // (vm_alloc_pair etc.) remain defined in cs-vm but become
        // unreachable from JIT'd code after this commit.
        builder.symbol("vm_alloc_pair", cs_vm::vm::vm_alloc_pair_gc as *const u8);
        builder.symbol("vm_pair_car", cs_vm::vm::vm_pair_car_gc as *const u8);
        builder.symbol("vm_pair_cdr", cs_vm::vm::vm_pair_cdr_gc as *const u8);
        builder.symbol("vm_pair_p", cs_vm::vm::vm_pair_p_gc as *const u8);
        builder.symbol("vm_null_p", cs_vm::vm::vm_null_p_gc as *const u8);
        builder.symbol("vm_value_clone", cs_vm::vm::vm_value_clone_gc as *const u8);
        builder.symbol("vm_value_drop", cs_vm::vm::vm_value_drop_gc as *const u8);
        builder.symbol("vm_box_typed", cs_vm::vm::vm_box_typed_gc as *const u8);
        builder.symbol(
            "vm_unbox_fixnum",
            cs_vm::vm::vm_unbox_fixnum_gc as *const u8,
        );
        builder.symbol(
            "vm_unbox_boolean",
            cs_vm::vm::vm_unbox_boolean_gc as *const u8,
        );
        builder.symbol(
            "vm_unbox_flonum",
            cs_vm::vm::vm_unbox_flonum_gc as *const u8,
        );
        builder.symbol("vm_eq_any", cs_vm::vm::vm_eq_any_gc as *const u8);
        builder.symbol("vm_any_truthy", cs_vm::vm::vm_any_truthy_gc as *const u8);
        // ADR 0012 D-1 (iter BU) — slow-path general Call. Lowered
        // from `Inst::CallGeneral` whenever the bytecode's call
        // target is neither `self` nor a known builtin.
        builder.symbol("vm_call_general", cs_vm::vm::vm_call_general as *const u8);
        builder.symbol(
            "vm_closure_id_peek",
            cs_vm::vm::vm_closure_id_peek as *const u8,
        );
        builder.symbol(
            "vm_alloc_vector_gc",
            cs_vm::vm::vm_alloc_vector_gc as *const u8,
        );
        builder.symbol("vm_vector_ref_gc", cs_vm::vm::vm_vector_ref_gc as *const u8);
        builder.symbol("vm_vector_set_gc", cs_vm::vm::vm_vector_set_gc as *const u8);
        builder.symbol(
            "vm_vector_length_gc",
            cs_vm::vm::vm_vector_length_gc as *const u8,
        );
        builder.symbol("vm_vector_p_gc", cs_vm::vm::vm_vector_p_gc as *const u8);
        // ADR 0012 D-2 (iter BX) — string op helpers.
        builder.symbol(
            "vm_alloc_string_gc",
            cs_vm::vm::vm_alloc_string_gc as *const u8,
        );
        builder.symbol("vm_string_ref_gc", cs_vm::vm::vm_string_ref_gc as *const u8);
        builder.symbol(
            "vm_string_length_gc",
            cs_vm::vm::vm_string_length_gc as *const u8,
        );
        builder.symbol("vm_string_p_gc", cs_vm::vm::vm_string_p_gc as *const u8);
        builder.symbol("vm_string_eq_gc", cs_vm::vm::vm_string_eq_gc as *const u8);
        // ADR 0012 D-2 (iter BZ) — lambda creation in JIT bodies.
        builder.symbol("vm_make_closure", cs_vm::vm::vm_make_closure as *const u8);
        // ADR 0012 D-2 (iter CA) — list ops.
        builder.symbol("vm_length_gc", cs_vm::vm::vm_length_gc as *const u8);
        builder.symbol("vm_list_p_gc", cs_vm::vm::vm_list_p_gc as *const u8);
        // ADR 0012 D-2 (iter CB) — reverse.
        builder.symbol("vm_reverse_gc", cs_vm::vm::vm_reverse_gc as *const u8);
        // ADR 0012 D-2 (iter CC) — memq.
        builder.symbol("vm_memq_gc", cs_vm::vm::vm_memq_gc as *const u8);
        // ADR 0012 D-2 (iter CD) — assq.
        builder.symbol("vm_assq_gc", cs_vm::vm::vm_assq_gc as *const u8);
        // ADR 0012 D-2 (iter CE) — pair mutation.
        builder.symbol("vm_set_car_gc", cs_vm::vm::vm_set_car_gc as *const u8);
        builder.symbol("vm_set_cdr_gc", cs_vm::vm::vm_set_cdr_gc as *const u8);
        // ADR 0012 D-2 (iter CG) — memv / assv (eqv?-flavored search).
        builder.symbol("vm_memv_gc", cs_vm::vm::vm_memv_gc as *const u8);
        builder.symbol("vm_assv_gc", cs_vm::vm::vm_assv_gc as *const u8);
        let mut module = JITModule::new(builder);

        // Import vm_env_lookup_fixnum: extern "C" fn(i64) -> i64.
        let mut env_lookup_sig = module.make_signature();
        env_lookup_sig.params.push(AbiParam::new(I64));
        env_lookup_sig.returns.push(AbiParam::new(I64));
        let env_lookup_func = module
            .declare_function(
                "vm_env_lookup_fixnum",
                cranelift_module::Linkage::Import,
                &env_lookup_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function env_lookup: {e}")))?;

        // Import vm_env_lookup_any: extern "C" fn(i64) -> i64 (same
        // shape as vm_env_lookup_fixnum).
        let env_lookup_any_func = module
            .declare_function(
                "vm_env_lookup_any",
                cranelift_module::Linkage::Import,
                &env_lookup_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function env_lookup_any: {e}")))?;

        // Import vm_env_set_fixnum: extern "C" fn(i64, i64) -> ().
        let mut env_set_sig = module.make_signature();
        env_set_sig.params.push(AbiParam::new(I64));
        env_set_sig.params.push(AbiParam::new(I64));
        let env_set_func = module
            .declare_function(
                "vm_env_set_fixnum",
                cranelift_module::Linkage::Import,
                &env_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function env_set: {e}")))?;

        // Heap-pointer ABI helpers (ADR 0011 D-5). Imported so the
        // module knows about them; no JIT body calls them yet.
        // vm_alloc_pair(car: i64, car_tag: u8, cdr: i64, cdr_tag: u8) -> i64.
        // The u8 tag args are passed as i64 (Cranelift doesn't have
        // a direct u8 ABI param; the helper's Rust signature truncates
        // to u8).
        let mut alloc_pair_sig = module.make_signature();
        alloc_pair_sig.params.push(AbiParam::new(I64)); // car
        alloc_pair_sig.params.push(AbiParam::new(I64)); // car_tag
        alloc_pair_sig.params.push(AbiParam::new(I64)); // cdr
        alloc_pair_sig.params.push(AbiParam::new(I64)); // cdr_tag
        alloc_pair_sig.returns.push(AbiParam::new(I64));
        let alloc_pair_func = module
            .declare_function(
                "vm_alloc_pair",
                cranelift_module::Linkage::Import,
                &alloc_pair_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_alloc_pair: {e}")))?;

        // vm_pair_car(pair: i64) -> i64 and vm_pair_cdr same shape.
        let mut pair_accessor_sig = module.make_signature();
        pair_accessor_sig.params.push(AbiParam::new(I64));
        pair_accessor_sig.returns.push(AbiParam::new(I64));
        let pair_car_func = module
            .declare_function(
                "vm_pair_car",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_pair_car: {e}")))?;
        let pair_cdr_func = module
            .declare_function(
                "vm_pair_cdr",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_pair_cdr: {e}")))?;

        // vm_pair_p / vm_null_p — same shape as the accessors: a
        // single Any-tagged i64 in, an i64 (0/1) out. Both consume
        // the input box.
        let pair_p_func = module
            .declare_function(
                "vm_pair_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_pair_p: {e}")))?;
        let null_p_func = module
            .declare_function(
                "vm_null_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_null_p: {e}")))?;

        // vm_value_clone — same shape as the accessors (i64 → i64).
        let value_clone_func = module
            .declare_function(
                "vm_value_clone",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_clone: {e}")))?;

        // vm_value_drop — i64 → ().
        let mut value_drop_sig = module.make_signature();
        value_drop_sig.params.push(AbiParam::new(I64));
        let value_drop_func = module
            .declare_function(
                "vm_value_drop",
                cranelift_module::Linkage::Import,
                &value_drop_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_drop: {e}")))?;

        // vm_box_typed(i, tag) — (i64, i64) → i64.
        let mut box_typed_sig = module.make_signature();
        box_typed_sig.params.push(AbiParam::new(I64));
        box_typed_sig.params.push(AbiParam::new(I64));
        box_typed_sig.returns.push(AbiParam::new(I64));
        let box_typed_func = module
            .declare_function(
                "vm_box_typed",
                cranelift_module::Linkage::Import,
                &box_typed_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_box_typed: {e}")))?;

        // vm_unbox_fixnum — same shape as the accessors (i64 -> i64).
        let unbox_fixnum_func = module
            .declare_function(
                "vm_unbox_fixnum",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_unbox_fixnum: {e}")))?;

        // Same shape (i64 -> i64) for boolean and flonum unbox.
        let unbox_boolean_func = module
            .declare_function(
                "vm_unbox_boolean",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_unbox_boolean: {e}")))?;
        let unbox_flonum_func = module
            .declare_function(
                "vm_unbox_flonum",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_unbox_flonum: {e}")))?;

        // vm_eq_any(a, b) -> i64. Same shape as the existing
        // `box_typed_sig` (i64, i64) -> i64.
        let eq_any_func = module
            .declare_function(
                "vm_eq_any",
                cranelift_module::Linkage::Import,
                &box_typed_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_eq_any: {e}")))?;

        // vm_any_truthy(r) -> i64 — same shape as accessors.
        let any_truthy_func = module
            .declare_function(
                "vm_any_truthy",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_any_truthy: {e}")))?;

        // vm_call_general(callee: i64, args_ptr: i64, n_args: i64) -> i64.
        // The Rust signature takes `*const i64` + `usize`; Cranelift
        // models pointers and `usize` as `i64` on the 64-bit hosts we
        // support, and the helper's Rust prologue casts back.
        let mut call_general_sig = module.make_signature();
        call_general_sig.params.push(AbiParam::new(I64)); // callee Gc handle
        call_general_sig.params.push(AbiParam::new(I64)); // args buffer pointer
        call_general_sig.params.push(AbiParam::new(I64)); // n_args
                                                          // ADR 0012 D-1 (iter BY) — IC slot pointer for miss-handler
                                                          // update. Null when the caller hasn't allocated a slot.
        call_general_sig.params.push(AbiParam::new(I64));
        call_general_sig.returns.push(AbiParam::new(I64));
        let call_general_func = module
            .declare_function(
                "vm_call_general",
                cranelift_module::Linkage::Import,
                &call_general_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_call_general: {e}")))?;

        // vm_closure_id_peek(i64) -> i32 (u32 zero-extends).
        let mut closure_id_peek_sig = module.make_signature();
        closure_id_peek_sig.params.push(AbiParam::new(I64));
        closure_id_peek_sig
            .returns
            .push(AbiParam::new(cranelift_codegen::ir::types::I32));
        let closure_id_peek_func = module
            .declare_function(
                "vm_closure_id_peek",
                cranelift_module::Linkage::Import,
                &closure_id_peek_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_closure_id_peek: {e}")))?;

        // ADR 0012 D-2 (iter BV) — vector op helpers.
        // vm_alloc_vector_gc(n: i64, fill: i64) -> i64
        let mut alloc_vector_sig = module.make_signature();
        alloc_vector_sig.params.push(AbiParam::new(I64));
        alloc_vector_sig.params.push(AbiParam::new(I64));
        alloc_vector_sig.returns.push(AbiParam::new(I64));
        let alloc_vector_func = module
            .declare_function(
                "vm_alloc_vector_gc",
                cranelift_module::Linkage::Import,
                &alloc_vector_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_alloc_vector_gc: {e}")))?;

        // vm_vector_ref_gc(vec: i64, idx: i64) -> i64
        let mut vector_ref_sig = module.make_signature();
        vector_ref_sig.params.push(AbiParam::new(I64));
        vector_ref_sig.params.push(AbiParam::new(I64));
        vector_ref_sig.returns.push(AbiParam::new(I64));
        let vector_ref_func = module
            .declare_function(
                "vm_vector_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_ref_gc: {e}")))?;

        // vm_vector_set_gc(vec: i64, idx: i64, x: i64) -> i64
        let mut vector_set_sig = module.make_signature();
        vector_set_sig.params.push(AbiParam::new(I64));
        vector_set_sig.params.push(AbiParam::new(I64));
        vector_set_sig.params.push(AbiParam::new(I64));
        vector_set_sig.returns.push(AbiParam::new(I64));
        let vector_set_func = module
            .declare_function(
                "vm_vector_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_set_gc: {e}")))?;

        // vm_vector_length_gc(vec: i64) -> i64
        // vm_vector_p_gc(v: i64) -> i64 — same shape.
        let vector_length_func = module
            .declare_function(
                "vm_vector_length_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_length_gc: {e}")))?;
        let vector_p_func = module
            .declare_function(
                "vm_vector_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter BX) — string op helpers. Signatures:
        // vm_alloc_string_gc(n: i64, fill: i64) -> i64 — reuses
        //   the vector_ref_sig shape (two i64 in, one i64 out).
        // vm_string_ref_gc(s: i64, idx: i64) -> i64 — same shape.
        // vm_string_eq_gc(a: i64, b: i64) -> i64 — same shape.
        // vm_string_length_gc(s: i64) -> i64 — pair_accessor_sig.
        // vm_string_p_gc(v: i64) -> i64 — pair_accessor_sig.
        let alloc_string_func = module
            .declare_function(
                "vm_alloc_string_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_alloc_string_gc: {e}")))?;
        let string_ref_func = module
            .declare_function(
                "vm_string_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ref_gc: {e}")))?;
        let string_length_func = module
            .declare_function(
                "vm_string_length_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_length_gc: {e}")))?;
        let string_p_func = module
            .declare_function(
                "vm_string_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_p_gc: {e}")))?;
        let string_eq_func = module
            .declare_function(
                "vm_string_eq_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_eq_gc: {e}")))?;

        // ADR 0012 D-2 (iter BZ) — vm_make_closure(lambda_idx: i64) -> i64.
        // Same shape as pair_accessor_sig (one i64 in, one i64 out).
        let make_closure_func = module
            .declare_function(
                "vm_make_closure",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_make_closure: {e}")))?;

        // ADR 0012 D-2 (iter CA) — vm_length_gc / vm_list_p_gc.
        // Both share pair_accessor_sig (one i64 in, one i64 out).
        let length_func = module
            .declare_function(
                "vm_length_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_length_gc: {e}")))?;
        let list_p_func = module
            .declare_function(
                "vm_list_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_list_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter CB) — vm_reverse_gc(lst: i64) -> i64.
        let reverse_func = module
            .declare_function(
                "vm_reverse_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_reverse_gc: {e}")))?;

        // ADR 0012 D-2 (iter CC) — vm_memq_gc(item, lst) -> i64.
        // Two i64 in, one i64 out (same shape as vector_ref_sig).
        let memq_func = module
            .declare_function(
                "vm_memq_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_memq_gc: {e}")))?;

        // ADR 0012 D-2 (iter CD) — vm_assq_gc(key, alist) -> i64.
        // Same shape as vm_memq_gc.
        let assq_func = module
            .declare_function(
                "vm_assq_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_assq_gc: {e}")))?;

        // ADR 0012 D-2 (iter CE) — vm_set_car_gc / vm_set_cdr_gc.
        // Two i64 in, one i64 out — vector_ref_sig shape.
        let set_car_func = module
            .declare_function(
                "vm_set_car_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_set_car_gc: {e}")))?;
        let set_cdr_func = module
            .declare_function(
                "vm_set_cdr_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_set_cdr_gc: {e}")))?;

        // ADR 0012 D-2 (iter CG) — vm_memv_gc / vm_assv_gc.
        // Same shape as memq/assq.
        let memv_func = module
            .declare_function(
                "vm_memv_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_memv_gc: {e}")))?;
        let assv_func = module
            .declare_function(
                "vm_assv_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_assv_gc: {e}")))?;

        let ctx = module.make_context();
        Ok(Self {
            module,
            ctx,
            func_ctx: FunctionBuilderContext::new(),
            next_id: 0,
            last_inner_stack_maps: HashMap::new(),
            last_inner_base: std::ptr::null(),
            env_lookup_func,
            env_lookup_any_func,
            env_set_func,
            alloc_pair_func,
            pair_car_func,
            pair_cdr_func,
            pair_p_func,
            null_p_func,
            value_clone_func,
            value_drop_func,
            box_typed_func,
            unbox_fixnum_func,
            unbox_boolean_func,
            unbox_flonum_func,
            eq_any_func,
            any_truthy_func,
            call_general_func,
            closure_id_peek_func,
            alloc_vector_func,
            vector_ref_func,
            vector_set_func,
            vector_length_func,
            vector_p_func,
            alloc_string_func,
            string_ref_func,
            string_length_func,
            string_p_func,
            string_eq_func,
            make_closure_func,
            length_func,
            list_p_func,
            reverse_func,
            memq_func,
            assq_func,
            set_car_func,
            set_cdr_func,
            memv_func,
            assv_func,
            // iter BR: empty IC table. Iter BS+ will reserve a
            // slot per Inst::Call as lowering walks the RIR.
            ic_table: IcTable::new(0),
        })
    }

    /// Mutable handle to the per-module IC table. Used by future
    /// call-site lowering (iter BS+) to reserve a slot per
    /// `Inst::Call`; iter BR exposes the accessor without any
    /// in-tree caller so the shape can settle before codegen
    /// piles on. See `docs/research/jit_inline_cache.md` §3.1.
    pub fn ic_table_mut(&mut self) -> &mut IcTable {
        &mut self.ic_table
    }

    /// Immutable view of the IC table. Tests and diagnostics
    /// only — the lowering hot path uses `ic_table_mut`.
    pub fn ic_table(&self) -> &IcTable {
        &self.ic_table
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Compile a pure-fixnum RIR `Function` into a callable
    /// `extern "C" fn(i64, ..., i64) -> i64`. Returns the function
    /// pointer; the lowerer retains ownership of the underlying
    /// memory mapping.
    ///
    /// Iter 4 supports multi-block functions with `Branch` and
    /// `Jump` terminators. Block parameters carry per-edge values.
    /// `Call` and `DeoptCheck` are still Unsupported (iter 4b+).
    pub fn compile_pure_fixnum(&mut self, rir: &RirFunction) -> Result<*const u8, JitError> {
        if rir.blocks.is_empty() {
            return Err(JitError::Codegen("function has no blocks".into()));
        }

        // Wrapper pattern (ADR 0011 D-7): every JIT'd function is
        // emitted as a pair of Cranelift functions:
        //
        //   outer  — CallConv::SystemV. The runtime transmutes its
        //            pointer as `extern "C" fn(i64,...) -> i64` and
        //            dispatches through it. Body is a one-instruction
        //            trampoline: `return inner(args...)`.
        //   inner  — CallConv::Tail. Hosts the real RIR-derived body.
        //            `Inst::CallSelf` in tail position lowers to
        //            Cranelift `return_call` against `inner`'s
        //            FuncRef, reusing the caller's frame. Without
        //            this, deeply recursive bodies (`let loop` over
        //            millions of iters) burned host stack on every
        //            iteration.
        //
        // Tail position detection is done at the end of each block:
        //   (a) Direct      — last inst is `CallSelf(dst, args)` and
        //                     terminator is `Return(dst)`.
        //   (b) Through join — last inst is `CallSelf(dst, args)` and
        //                     terminator is `Jump(target, [dst])` to
        //                     a block whose only content is
        //                     `Return(p)` where p is its first param.
        //                     The let-loop pattern lowers via (b).
        let outer_sig = {
            let mut sig = Signature::new(CallConv::SystemV);
            for _ in &rir.params {
                sig.params.push(AbiParam::new(I64));
            }
            sig.returns.push(AbiParam::new(I64));
            sig
        };
        let inner_sig = {
            let mut sig = Signature::new(CallConv::Tail);
            for _ in &rir.params {
                sig.params.push(AbiParam::new(I64));
            }
            sig.returns.push(AbiParam::new(I64));
            sig
        };

        let outer_seq = self.fresh_id();
        let inner_seq = self.fresh_id();
        let outer_module_name = format!("{}#{}.outer", rir.name, outer_seq);
        let inner_module_name = format!("{}#{}.inner", rir.name, inner_seq);
        let outer_id = self
            .module
            .declare_function(&outer_module_name, Linkage::Local, &outer_sig)
            .map_err(|e| {
                JitError::Codegen(format!("declare_function {}: {e}", outer_module_name))
            })?;
        let inner_id = self
            .module
            .declare_function(&inner_module_name, Linkage::Local, &inner_sig)
            .map_err(|e| {
                JitError::Codegen(format!("declare_function {}: {e}", inner_module_name))
            })?;

        // Compile inner first — the actual body. inner's CallSelf
        // points back at inner via its own FuncRef so tail recursion
        // self-loops on the same Tail-conv function.
        self.compile_inner_body(rir, inner_id, inner_seq, &inner_sig)?;
        // Compile outer — a plain SystemV trampoline that calls inner.
        self.compile_outer_trampoline(rir, outer_id, outer_seq, &outer_sig, inner_id)?;
        self.module
            .finalize_definitions()
            .map_err(|e| JitError::Codegen(format!("finalize_definitions: {e}")))?;
        // ADR 0012 D-2 (iter BM) — capture the inner function's
        // runtime base address. The caller (tier-up hook) uses this
        // alongside `last_inner_stack_maps` to construct a
        // `JitStackMaps` keyed by the inner base for the closure.
        self.last_inner_base = self.module.get_finalized_function(inner_id);
        Ok(self.module.get_finalized_function(outer_id))
    }

    fn compile_inner_body(
        &mut self,
        rir: &RirFunction,
        inner_id: cranelift_module::FuncId,
        inner_seq: u64,
        inner_sig: &Signature,
    ) -> Result<(), JitError> {
        let func_name = UserFuncName::user(0, inner_seq as u32);
        let mut clif = ClifFunction::with_name_signature(func_name, inner_sig.clone());
        let mut value_map: HashMap<RirValue, cranelift_codegen::ir::Value> = HashMap::new();
        {
            let mut builder = FunctionBuilder::new(&mut clif, &mut self.func_ctx);
            let self_fnref = self.module.declare_func_in_func(inner_id, builder.func);
            let env_lookup_fnref = self
                .module
                .declare_func_in_func(self.env_lookup_func, builder.func);
            let env_lookup_any_fnref = self
                .module
                .declare_func_in_func(self.env_lookup_any_func, builder.func);
            let env_set_fnref = self
                .module
                .declare_func_in_func(self.env_set_func, builder.func);
            let alloc_pair_fnref = self
                .module
                .declare_func_in_func(self.alloc_pair_func, builder.func);
            let pair_car_fnref = self
                .module
                .declare_func_in_func(self.pair_car_func, builder.func);
            let pair_cdr_fnref = self
                .module
                .declare_func_in_func(self.pair_cdr_func, builder.func);
            let pair_p_fnref = self
                .module
                .declare_func_in_func(self.pair_p_func, builder.func);
            let null_p_fnref = self
                .module
                .declare_func_in_func(self.null_p_func, builder.func);
            let value_clone_fnref = self
                .module
                .declare_func_in_func(self.value_clone_func, builder.func);
            let value_drop_fnref = self
                .module
                .declare_func_in_func(self.value_drop_func, builder.func);
            let box_typed_fnref = self
                .module
                .declare_func_in_func(self.box_typed_func, builder.func);
            let unbox_fixnum_fnref = self
                .module
                .declare_func_in_func(self.unbox_fixnum_func, builder.func);
            let unbox_boolean_fnref = self
                .module
                .declare_func_in_func(self.unbox_boolean_func, builder.func);
            let unbox_flonum_fnref = self
                .module
                .declare_func_in_func(self.unbox_flonum_func, builder.func);
            let eq_any_fnref = self
                .module
                .declare_func_in_func(self.eq_any_func, builder.func);
            let any_truthy_fnref = self
                .module
                .declare_func_in_func(self.any_truthy_func, builder.func);
            let call_general_fnref = self
                .module
                .declare_func_in_func(self.call_general_func, builder.func);
            let closure_id_peek_fnref = self
                .module
                .declare_func_in_func(self.closure_id_peek_func, builder.func);
            // iter BV — vector ops.
            let alloc_vector_fnref = self
                .module
                .declare_func_in_func(self.alloc_vector_func, builder.func);
            let vector_ref_fnref = self
                .module
                .declare_func_in_func(self.vector_ref_func, builder.func);
            let vector_set_fnref = self
                .module
                .declare_func_in_func(self.vector_set_func, builder.func);
            let vector_length_fnref = self
                .module
                .declare_func_in_func(self.vector_length_func, builder.func);
            let vector_p_fnref = self
                .module
                .declare_func_in_func(self.vector_p_func, builder.func);
            // iter BX — string ops.
            let alloc_string_fnref = self
                .module
                .declare_func_in_func(self.alloc_string_func, builder.func);
            let string_ref_fnref = self
                .module
                .declare_func_in_func(self.string_ref_func, builder.func);
            let string_length_fnref = self
                .module
                .declare_func_in_func(self.string_length_func, builder.func);
            let string_p_fnref = self
                .module
                .declare_func_in_func(self.string_p_func, builder.func);
            let string_eq_fnref = self
                .module
                .declare_func_in_func(self.string_eq_func, builder.func);
            // iter BZ — lambda creation.
            let make_closure_fnref = self
                .module
                .declare_func_in_func(self.make_closure_func, builder.func);
            // iter CA — list ops.
            let length_fnref = self
                .module
                .declare_func_in_func(self.length_func, builder.func);
            let list_p_fnref = self
                .module
                .declare_func_in_func(self.list_p_func, builder.func);
            // iter CB — reverse.
            let reverse_fnref = self
                .module
                .declare_func_in_func(self.reverse_func, builder.func);
            // iter CC — memq.
            let memq_fnref = self
                .module
                .declare_func_in_func(self.memq_func, builder.func);
            // iter CD — assq.
            let assq_fnref = self
                .module
                .declare_func_in_func(self.assq_func, builder.func);
            // iter CE — pair mutation.
            let set_car_fnref = self
                .module
                .declare_func_in_func(self.set_car_func, builder.func);
            let set_cdr_fnref = self
                .module
                .declare_func_in_func(self.set_cdr_func, builder.func);
            // iter CG — memv / assv.
            let memv_fnref = self
                .module
                .declare_func_in_func(self.memv_func, builder.func);
            let assv_fnref = self
                .module
                .declare_func_in_func(self.assv_func, builder.func);

            let mut block_map: HashMap<cs_rir::BlockId, cranelift_codegen::ir::Block> =
                HashMap::with_capacity(rir.blocks.len());
            for rir_block in &rir.blocks {
                let cb = builder.create_block();
                if rir_block.id == rir.entry {
                    builder.append_block_params_for_function_params(cb);
                } else {
                    for _ in &rir_block.params {
                        builder.append_block_param(cb, I64);
                    }
                }
                block_map.insert(rir_block.id, cb);
            }
            let entry_clif = *block_map
                .get(&rir.entry)
                .ok_or_else(|| JitError::Codegen("entry block not in block map".into()))?;
            builder.switch_to_block(entry_clif);
            let entry_params = builder.block_params(entry_clif).to_vec();
            if entry_params.len() != rir.params.len() {
                return Err(JitError::Codegen(format!(
                    "entry param count mismatch: rir={} clif={}",
                    rir.params.len(),
                    entry_params.len()
                )));
            }
            for ((rir_v, ty), clif_v) in rir.params.iter().zip(entry_params.iter()) {
                value_map.insert(*rir_v, *clif_v);
                // ADR 0012 D-2 (iter BK) — Any-typed params arrive as
                // Gc<Value> raw handles. Mark them for stack-map
                // tracking so Cranelift spills them around each call
                // (every call is an implicit safepoint).
                if *ty == cs_rir::Type::Any {
                    builder.declare_value_needs_stack_map(*clif_v);
                }
            }

            for rir_block in &rir.blocks {
                let cb = *block_map.get(&rir_block.id).unwrap();
                builder.switch_to_block(cb);
                if rir_block.id != rir.entry {
                    let bps = builder.block_params(cb).to_vec();
                    for ((rir_v, ty), clif_v) in rir_block.params.iter().zip(bps.iter()) {
                        value_map.insert(*rir_v, *clif_v);
                        // Mark Any-typed join-block params for stack
                        // maps too — they hold Gc<Value> handles
                        // flowing in from predecessor Jumps.
                        if *ty == cs_rir::Type::Any {
                            builder.declare_value_needs_stack_map(*clif_v);
                        }
                    }
                }

                // Tail-CallSelf detection.
                let tail = detect_tail_call_self(rir, rir_block);
                let body_insts = if tail.is_some() {
                    &rir_block.insts[..rir_block.insts.len() - 1]
                } else {
                    &rir_block.insts[..]
                };
                for inst in body_insts {
                    lower_inst(
                        &mut builder,
                        &mut value_map,
                        self_fnref,
                        env_lookup_fnref,
                        env_lookup_any_fnref,
                        env_set_fnref,
                        alloc_pair_fnref,
                        pair_car_fnref,
                        pair_cdr_fnref,
                        pair_p_fnref,
                        null_p_fnref,
                        value_clone_fnref,
                        value_drop_fnref,
                        box_typed_fnref,
                        unbox_fixnum_fnref,
                        unbox_boolean_fnref,
                        unbox_flonum_fnref,
                        eq_any_fnref,
                        any_truthy_fnref,
                        call_general_fnref,
                        closure_id_peek_fnref,
                        alloc_vector_fnref,
                        vector_ref_fnref,
                        vector_set_fnref,
                        vector_length_fnref,
                        vector_p_fnref,
                        alloc_string_fnref,
                        string_ref_fnref,
                        string_length_fnref,
                        string_p_fnref,
                        string_eq_fnref,
                        make_closure_fnref,
                        length_fnref,
                        list_p_fnref,
                        reverse_fnref,
                        memq_fnref,
                        assq_fnref,
                        set_car_fnref,
                        set_cdr_fnref,
                        memv_fnref,
                        assv_fnref,
                        inst,
                    )?;
                }
                if let Some(args) = tail {
                    let cargs: Vec<cranelift_codegen::ir::Value> = args
                        .iter()
                        .map(|a| lookup(&value_map, *a))
                        .collect::<Result<_, _>>()?;
                    builder.ins().return_call(self_fnref, &cargs);
                } else {
                    lower_terminator(&mut builder, &block_map, &value_map, &rir_block.terminator)?;
                }
            }
            builder.seal_all_blocks();
            builder.finalize();
        }
        self.ctx.func = clif;
        self.module
            .define_function(inner_id, &mut self.ctx)
            .map_err(|e| JitError::Codegen(format!("define_function inner: {e}")))?;
        // ADR 0012 D-2 (iter BL) — harvest user_stack_maps before
        // clear_context drops the compiled output. Each entry is
        // (code_offset, _padding, UserStackMap); we flatten the
        // map's entries into `(pc_offset, [sp_offset])` pairs and
        // store in `self.last_inner_stack_maps` keyed by no key
        // (caller reads-then-clears after each compile). The full
        // per-closure registry plumbing lives in iter BM.
        let mut maps: HashMap<u32, Vec<i32>> = HashMap::new();
        if let Some(compiled) = self.ctx.compiled_code() {
            for (code_offset, _padding, sm) in compiled.buffer.user_stack_maps() {
                let mut offsets: Vec<i32> = Vec::new();
                for (_ty, sp_off) in sm.entries() {
                    offsets.push(sp_off as i32);
                }
                if !offsets.is_empty() {
                    maps.insert(*code_offset, offsets);
                }
            }
        }
        self.last_inner_stack_maps = maps;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }

    fn compile_outer_trampoline(
        &mut self,
        rir: &RirFunction,
        outer_id: cranelift_module::FuncId,
        outer_seq: u64,
        outer_sig: &Signature,
        inner_id: cranelift_module::FuncId,
    ) -> Result<(), JitError> {
        let func_name = UserFuncName::user(0, outer_seq as u32);
        let mut clif = ClifFunction::with_name_signature(func_name, outer_sig.clone());
        {
            let mut builder = FunctionBuilder::new(&mut clif, &mut self.func_ctx);
            let inner_fnref = self.module.declare_func_in_func(inner_id, builder.func);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);
            let args_v: Vec<cranelift_codegen::ir::Value> = builder.block_params(entry).to_vec();
            let _ = rir;
            let inst = builder.ins().call(inner_fnref, &args_v);
            let results = builder.inst_results(inst).to_vec();
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "trampoline expected 1 result, got {}",
                    results.len()
                )));
            }
            builder.ins().return_(&[results[0]]);
            builder.finalize();
        }
        self.ctx.func = clif;
        self.module
            .define_function(outer_id, &mut self.ctx)
            .map_err(|e| JitError::Codegen(format!("define_function outer: {e}")))?;
        self.module.clear_context(&mut self.ctx);
        Ok(())
    }

    /// Drain references to internal state. Used by tests that want
    /// to ensure module isolation between calls.
    #[doc(hidden)]
    pub fn module(&self) -> &JITModule {
        &self.module
    }
}

/// Detect whether `block`'s last instruction is a tail-position
/// `Inst::CallSelf`. Two shapes are recognized:
///
/// (a) Direct: last inst is `CallSelf(dst, args)` and terminator is
///     `Term::Return(dst)`.
/// (b) Through trivial join: last inst is `CallSelf(dst, args)` and
///     terminator is `Term::Jump(target, [dst])` where target is a
///     block whose only content is `Term::Return(p)` for p its first
///     param. The let-loop / let-named pattern always lowers via (b).
///
/// Returns the args slice if a tail call was detected (caller emits
/// `return_call(self_fnref, args)` and skips the regular terminator),
/// or `None` otherwise.
fn detect_tail_call_self<'a>(
    rir: &'a RirFunction,
    block: &'a cs_rir::Block,
) -> Option<&'a [RirValue]> {
    let last = block.insts.last()?;
    let (dst, args) = match last {
        Inst::CallSelf(dst, args) => (dst, args.as_slice()),
        _ => return None,
    };
    match &block.terminator {
        Term::Return(ret_v) if ret_v == dst => Some(args),
        Term::Jump(target, jump_args) if jump_args.len() == 1 && jump_args[0] == *dst => {
            let target_block = rir.blocks.iter().find(|b| b.id == *target)?;
            if !target_block.insts.is_empty() {
                return None;
            }
            match (&target_block.terminator, target_block.params.first()) {
                (Term::Return(rv), Some((p, _))) if rv == p => Some(args),
                _ => None,
            }
        }
        _ => None,
    }
}

fn lower_terminator(
    b: &mut FunctionBuilder,
    block_map: &HashMap<cs_rir::BlockId, cranelift_codegen::ir::Block>,
    map: &HashMap<RirValue, cranelift_codegen::ir::Value>,
    term: &Term,
) -> Result<(), JitError> {
    match term {
        Term::Return(v) => {
            let cv = lookup(map, *v)?;
            b.ins().return_(&[cv]);
        }
        Term::Jump(target, args) => {
            let tb = *block_map
                .get(target)
                .ok_or_else(|| JitError::Codegen(format!("unknown jump target {:?}", target)))?;
            let cargs: Vec<cranelift_codegen::ir::BlockArg> = args
                .iter()
                .map(|a| lookup(map, *a).map(cranelift_codegen::ir::BlockArg::Value))
                .collect::<Result<_, _>>()?;
            b.ins().jump(tb, &cargs);
        }
        Term::Branch(cond, then_b, else_b) => {
            let cv = lookup(map, *cond)?;
            let tb = *block_map
                .get(then_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown then target {:?}", then_b)))?;
            let eb = *block_map
                .get(else_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown else target {:?}", else_b)))?;
            b.ins().brif(cv, tb, &[], eb, &[]);
        }
    }
    Ok(())
}

fn lower_inst(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    self_fnref: cranelift_codegen::ir::FuncRef,
    env_lookup_fnref: cranelift_codegen::ir::FuncRef,
    env_lookup_any_fnref: cranelift_codegen::ir::FuncRef,
    env_set_fnref: cranelift_codegen::ir::FuncRef,
    alloc_pair_fnref: cranelift_codegen::ir::FuncRef,
    pair_car_fnref: cranelift_codegen::ir::FuncRef,
    pair_cdr_fnref: cranelift_codegen::ir::FuncRef,
    pair_p_fnref: cranelift_codegen::ir::FuncRef,
    null_p_fnref: cranelift_codegen::ir::FuncRef,
    value_clone_fnref: cranelift_codegen::ir::FuncRef,
    value_drop_fnref: cranelift_codegen::ir::FuncRef,
    box_typed_fnref: cranelift_codegen::ir::FuncRef,
    unbox_fixnum_fnref: cranelift_codegen::ir::FuncRef,
    unbox_boolean_fnref: cranelift_codegen::ir::FuncRef,
    unbox_flonum_fnref: cranelift_codegen::ir::FuncRef,
    eq_any_fnref: cranelift_codegen::ir::FuncRef,
    any_truthy_fnref: cranelift_codegen::ir::FuncRef,
    call_general_fnref: cranelift_codegen::ir::FuncRef,
    closure_id_peek_fnref: cranelift_codegen::ir::FuncRef,
    alloc_vector_fnref: cranelift_codegen::ir::FuncRef,
    vector_ref_fnref: cranelift_codegen::ir::FuncRef,
    vector_set_fnref: cranelift_codegen::ir::FuncRef,
    vector_length_fnref: cranelift_codegen::ir::FuncRef,
    vector_p_fnref: cranelift_codegen::ir::FuncRef,
    alloc_string_fnref: cranelift_codegen::ir::FuncRef,
    string_ref_fnref: cranelift_codegen::ir::FuncRef,
    string_length_fnref: cranelift_codegen::ir::FuncRef,
    string_p_fnref: cranelift_codegen::ir::FuncRef,
    string_eq_fnref: cranelift_codegen::ir::FuncRef,
    make_closure_fnref: cranelift_codegen::ir::FuncRef,
    length_fnref: cranelift_codegen::ir::FuncRef,
    list_p_fnref: cranelift_codegen::ir::FuncRef,
    reverse_fnref: cranelift_codegen::ir::FuncRef,
    memq_fnref: cranelift_codegen::ir::FuncRef,
    assq_fnref: cranelift_codegen::ir::FuncRef,
    set_car_fnref: cranelift_codegen::ir::FuncRef,
    set_cdr_fnref: cranelift_codegen::ir::FuncRef,
    memv_fnref: cranelift_codegen::ir::FuncRef,
    assv_fnref: cranelift_codegen::ir::FuncRef,
    inst: &Inst,
) -> Result<(), JitError> {
    match inst {
        Inst::LoadConst(dst, c) => {
            let v = match c {
                Const::Fixnum(n) => b.ins().iconst(I64, *n),
                Const::Boolean(true) => b.ins().iconst(I64, 1),
                Const::Boolean(false) => b.ins().iconst(I64, 0),
                Const::Character(c) => b.ins().iconst(I64, *c as u32 as i64),
                Const::Flonum(f) => {
                    // Encode the f64 directly as the i64 bit pattern;
                    // the decoder reads back via `f64::from_bits`.
                    b.ins().iconst(I64, f.to_bits() as i64)
                }
                Const::Null => b.ins().iconst(I64, 0),
                Const::Unspecified => b.ins().iconst(I64, 0),
                Const::Symbol(id) => b.ins().iconst(I64, *id as i64),
                other => {
                    return Err(JitError::Unsupported(format!(
                        "LoadConst {:?} not in iter-2 scope",
                        other
                    )));
                }
            };
            map.insert(*dst, v);
        }
        Inst::Add(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().iadd(l, r))?,
        Inst::Sub(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().isub(l, r))?,
        Inst::Mul(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().imul(l, r))?,
        Inst::Quotient(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().sdiv(l, r))?
        }
        Inst::Remainder(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().srem(l, r))?
        }
        Inst::BitAnd(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().band(l, r))?
        }
        Inst::BitOr(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().bor(l, r))?,
        Inst::BitXor(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().bxor(l, r))?
        }
        Inst::AbsFixnum(dst, src) => {
            let s = lookup(map, *src)?;
            let v = b.ins().iabs(s);
            map.insert(*dst, v);
        }
        Inst::MaxFixnum(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().smax(l, r))?
        }
        Inst::MinFixnum(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().smin(l, r))?
        }
        Inst::Lt(dst, lhs, rhs) => {
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            // icmp returns an i8 0/1; widen to i64 to match the
            // calling convention.
            let cmp = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                l,
                r,
            );
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::Eq(dst, lhs, rhs) => {
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            let cmp = b
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, l, r);
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::Move(dst, src) => {
            let v = lookup(map, *src)?;
            map.insert(*dst, v);
        }
        Inst::IntCharBitcast(dst, src) => {
            // Same i64 carrying the codepoint; only the dst's
            // type tag changes for the return-type post-pass.
            let v = lookup(map, *src)?;
            map.insert(*dst, v);
        }
        Inst::FixToFlo(dst, src) => {
            // Convert the Fixnum i64 to f64, then bitcast the f64
            // back to i64 so the value still rides the i64 ABI lane.
            // The dispatcher decodes via `f64::from_bits` based on
            // the Flonum return-type tag.
            let v = lookup(map, *src)?;
            let f = b.ins().fcvt_from_sint(F64, v);
            let bits = b
                .ins()
                .bitcast(I64, cranelift_codegen::ir::MemFlags::new(), f);
            map.insert(*dst, bits);
        }
        Inst::Cons(dst, car, car_tag, cdr, cdr_tag) => {
            // Lowers to a call against vm_alloc_pair(car, car_tag,
            // cdr, cdr_tag). Tags are baked at translate time;
            // operands are whatever values the JIT body computed.
            // Result is Any-tagged (the i64 carries
            // Box::into_raw(Box<Value::Pair(_)>)).
            let car_v = lookup(map, *car)?;
            let cdr_v = lookup(map, *cdr)?;
            let car_t = b.ins().iconst(I64, *car_tag as i64);
            let cdr_t = b.ins().iconst(I64, *cdr_tag as i64);
            let inst_ref = b
                .ins()
                .call(alloc_pair_fnref, &[car_v, car_t, cdr_v, cdr_t]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Cons expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            // ADR 0012 D-2 — declare this SSA value a stack-map root.
            // Cranelift's frontend will spill it to a known SP-offset
            // slot before each call (every `call` is a safepoint) and
            // reload after. The metadata is harvested at
            // `define_function` time and stored in a per-closure
            // `JitStackMaps` (wired in iter BG when the GC scanner
            // consumes the registry). Today (BF) this is metadata-
            // only — the helper still allocates Box<Value>; semantics
            // unchanged. Risk localized to Cranelift's spill
            // discipline; M6 Phase 4 test suite is the canary.
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Car(dst, src) => {
            // Lowers to vm_pair_car_gc(pair_i64) -> i64. The runtime
            // helper consumes the Gc<Value> handle, returns the car
            // as a fresh Gc<Value> handle. ADR 0012 D-2 (iter BK) —
            // mark the dst as a stack-map root so Cranelift spills
            // it around subsequent calls.
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(pair_car_fnref, &[v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Car expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Cdr(dst, src) => {
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(pair_cdr_fnref, &[v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Cdr expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::PairP(dst, src) => {
            // Lowers to vm_pair_p(any) -> i64. The helper consumes
            // the box; result is 0/1, decoded as Boolean by the
            // dispatcher when PairP's dst flows to the function's
            // return value.
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(pair_p_fnref, &[v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "PairP expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::NullP(dst, src) => {
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(null_p_fnref, &[v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "NullP expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::AnyClone(dst, src) => {
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(value_clone_fnref, &[v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "AnyClone expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::AnyDrop(src) => {
            let v = lookup(map, *src)?;
            b.ins().call(value_drop_fnref, &[v]);
        }
        Inst::BoxTyped(dst, src, tag) => {
            // Box a typed i64 (Fixnum/Boolean/Character/Flonum/...)
            // into an Any-tagged Gc<Value> handle. Tag passes as i64.
            let v = lookup(map, *src)?;
            let t = b.ins().iconst(I64, *tag as i64);
            let inst_ref = b.ins().call(box_typed_fnref, &[v, t]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BoxTyped expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::AnyToFix(dst, src) => {
            // Lowers to vm_unbox_fixnum — consumes the box.
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(unbox_fixnum_fnref, &[v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "AnyToFix expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::AnyToBool(dst, src) => {
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(unbox_boolean_fnref, &[v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "AnyToBool expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::AnyToFlo(dst, src) => {
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(unbox_flonum_fnref, &[v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "AnyToFlo expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::EqAny(dst, lhs, rhs) => {
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            let inst_ref = b.ins().call(eq_any_fnref, &[l, r]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "EqAny expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::AnyTruthy(dst, src) => {
            let v = lookup(map, *src)?;
            let inst_ref = b.ins().call(any_truthy_fnref, &[v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "AnyTruthy expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::FlonumAdd(dst, lhs, rhs) => {
            fbinop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().fadd(l, r))?
        }
        Inst::FlonumSub(dst, lhs, rhs) => {
            fbinop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().fsub(l, r))?
        }
        Inst::FlonumMul(dst, lhs, rhs) => {
            fbinop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().fmul(l, r))?
        }
        Inst::FlonumDiv(dst, lhs, rhs) => {
            fbinop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().fdiv(l, r))?
        }
        Inst::FlonumLt(dst, lhs, rhs) => {
            // IEEE-754 less-than: NaN compares unordered, so we use
            // `LessThan` (the strict/ordered form). NaN < x is false
            // on either side, matching R6RS flonum semantics.
            let l_i = lookup(map, *lhs)?;
            let r_i = lookup(map, *rhs)?;
            let mf = cranelift_codegen::ir::MemFlags::new();
            let l_f = b.ins().bitcast(F64, mf, l_i);
            let r_f = b.ins().bitcast(F64, mf, r_i);
            let cmp = b.ins().fcmp(
                cranelift_codegen::ir::condcodes::FloatCC::LessThan,
                l_f,
                r_f,
            );
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::FlonumEq(dst, lhs, rhs) => {
            // IEEE-754 equality: NaN ≠ NaN. `Equal` is the ordered
            // (no-NaN-match) form.
            let l_i = lookup(map, *lhs)?;
            let r_i = lookup(map, *rhs)?;
            let mf = cranelift_codegen::ir::MemFlags::new();
            let l_f = b.ins().bitcast(F64, mf, l_i);
            let r_f = b.ins().bitcast(F64, mf, r_i);
            let cmp = b
                .ins()
                .fcmp(cranelift_codegen::ir::condcodes::FloatCC::Equal, l_f, r_f);
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::FlonumSqrt(dst, src) => funary(b, map, *dst, *src, |b, x| b.ins().sqrt(x))?,
        Inst::FlonumAbs(dst, src) => funary(b, map, *dst, *src, |b, x| b.ins().fabs(x))?,
        Inst::FlonumMax(dst, lhs, rhs) => {
            fbinop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().fmax(l, r))?
        }
        Inst::FlonumMin(dst, lhs, rhs) => {
            fbinop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().fmin(l, r))?
        }
        Inst::FlonumFloor(dst, src) => funary(b, map, *dst, *src, |b, x| b.ins().floor(x))?,
        Inst::FlonumCeil(dst, src) => funary(b, map, *dst, *src, |b, x| b.ins().ceil(x))?,
        Inst::FlonumTrunc(dst, src) => funary(b, map, *dst, *src, |b, x| b.ins().trunc(x))?,
        Inst::FlonumRound(dst, src) => funary(b, map, *dst, *src, |b, x| b.ins().nearest(x))?,
        Inst::Param(_, _) => {
            // Param entries are populated from the entry block's
            // appended params before lower_inst runs.
            return Err(JitError::Codegen(
                "Inst::Param appears in block body — must be entry-only".into(),
            ));
        }
        Inst::CallSelf(dst, args) => {
            let cargs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
            let inst_ref = b.ins().call(self_fnref, &cargs);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "CallSelf expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::EnvLookup(dst, sym) => {
            // Pass the symbol id as i64; the helper reads
            // JIT_CALLER_ENV from a thread-local that the runtime
            // dispatch site set before the JIT call.
            let sym_v = b.ins().iconst(I64, *sym as i64);
            let inst_ref = b.ins().call(env_lookup_fnref, &[sym_v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "EnvLookup expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::EnvLookupAny(dst, sym) => {
            // Same shape as EnvLookup but the result is a fresh
            // Gc<Value> handle (Any-tagged), so it needs to be a
            // stack-map root across subsequent safepoints.
            let sym_v = b.ins().iconst(I64, *sym as i64);
            let inst_ref = b.ins().call(env_lookup_any_fnref, &[sym_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "EnvLookupAny expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::EnvSet(sym, value) => {
            let val = lookup(map, *value)?;
            let sym_v = b.ins().iconst(I64, *sym as i64);
            b.ins().call(env_set_fnref, &[sym_v, val]);
        }
        Inst::CallGeneral(dst, callee, args) => {
            // ADR 0012 D-1 (iter BY) — IC hot path. For each
            // CallGeneral site we allocate a fresh IcSlot via
            // Box::leak (stable process-lifetime address). The
            // emitted code checks the slot's cached_closure_id
            // against the callee's live id; on match it dispatches
            // through cached_jit_ptr via call_indirect; on miss it
            // falls through to vm_call_general which also updates
            // the slot.
            let callee_v = lookup(map, *callee)?;
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;

            // Args buffer for the miss path (vm_call_general expects
            // an i64[] + count). At least 8 bytes so stack_addr is
            // always valid even at n == 0.
            let buf_bytes = std::cmp::max(8u32, (n as u32) * 8);
            let buf_slot = b.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                buf_bytes,
                3,
            ));
            for (i, av) in arg_vs.iter().enumerate() {
                let _ = b.ins().stack_store(*av, buf_slot, (i as i32) * 8);
            }
            let buf_addr = b.ins().stack_addr(I64, buf_slot, 0);
            let n_args_v = b.ins().iconst(I64, n as i64);

            // Allocate per-site IC slot. Box::leak gives a stable
            // process-lifetime address; the i64 baked into the JIT
            // body's constant pool is that address.
            let ic_slot: &'static crate::ic::IcSlot = Box::leak(Box::new(crate::ic::IcSlot::new()));
            let slot_ptr_const = ic_slot as *const crate::ic::IcSlot as i64;
            let slot_addr_v = b.ins().iconst(I64, slot_ptr_const);

            // Peek closure id from callee (doesn't consume).
            let peek_inst = b.ins().call(closure_id_peek_fnref, &[callee_v]);
            let peeked_id_i32 = {
                let rs = b.inst_results(peek_inst);
                if rs.len() != 1 {
                    return Err(JitError::Codegen(
                        "vm_closure_id_peek expected 1 result".into(),
                    ));
                }
                rs[0]
            };
            // Load cached_closure_id (u32 at offset 0 of IcSlot —
            // see #[repr(C)] layout invariant in cs-jit-cranelift/ic.rs).
            let cached_id_i32 = b.ins().load(
                cranelift_codegen::ir::types::I32,
                cranelift_codegen::ir::MemFlags::new(),
                slot_addr_v,
                0,
            );
            // Hit iff cached != 0 AND peeked == cached.
            let id_match = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::Equal,
                peeked_id_i32,
                cached_id_i32,
            );
            let zero32 = b.ins().iconst(cranelift_codegen::ir::types::I32, 0);
            let cached_nonzero = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                cached_id_i32,
                zero32,
            );
            let take_hit = b.ins().band(id_match, cached_nonzero);

            let hit_block = b.create_block();
            let miss_block = b.create_block();
            let join_block = b.create_block();
            // Join block has one block-param: the i64 result Gc handle.
            b.append_block_param(join_block, I64);

            b.ins().brif(take_hit, hit_block, &[], miss_block, &[]);

            // ===== Hit block =====
            b.switch_to_block(hit_block);
            b.seal_block(hit_block);
            // Drop the callee handle — the cached jit_ptr's body
            // doesn't consume callee (it consumes args only); we own
            // the strong ref. Use vm_value_drop_gc.
            b.ins().call(value_drop_fnref, &[callee_v]);
            // Load cached_jit_ptr (offset_of cached_jit_ptr inside
            // IcSlot, #[repr(C)]). Field order:
            //   cached_closure_id  AtomicU32  offset 0, 4 bytes
            //   cached_jit_ptr     AtomicPtr  offset 8 (alignment-padded
            //                                  on 64-bit; 4 bytes pad
            //                                  follow cached_closure_id)
            //   cached_arity       AtomicU32  offset 16
            //   cached_param_types AtomicU32  offset 20
            //   miss_count         AtomicU32  offset 24
            let cached_jit_ptr_v =
                b.ins()
                    .load(I64, cranelift_codegen::ir::MemFlags::new(), slot_addr_v, 8);
            // call_indirect with a signature matching args.len().
            let mut hit_sig = b.func.import_signature({
                let mut s = cranelift_codegen::ir::Signature::new(
                    cranelift_codegen::isa::CallConv::SystemV,
                );
                for _ in 0..n {
                    s.params.push(AbiParam::new(I64));
                }
                s.returns.push(AbiParam::new(I64));
                s
            });
            let _ = &mut hit_sig;
            let hit_inst = b.ins().call_indirect(hit_sig, cached_jit_ptr_v, &arg_vs);
            let hit_result = {
                let rs = b.inst_results(hit_inst);
                if rs.len() != 1 {
                    return Err(JitError::Codegen(
                        "IC hit call_indirect expected 1 result".into(),
                    ));
                }
                rs[0]
            };
            b.ins().jump(
                join_block,
                &[cranelift_codegen::ir::BlockArg::Value(hit_result)],
            );

            // ===== Miss block =====
            b.switch_to_block(miss_block);
            b.seal_block(miss_block);
            let miss_inst = b.ins().call(
                call_general_fnref,
                &[callee_v, buf_addr, n_args_v, slot_addr_v],
            );
            let miss_result = {
                let rs = b.inst_results(miss_inst);
                if rs.len() != 1 {
                    return Err(JitError::Codegen(
                        "vm_call_general expected 1 result".into(),
                    ));
                }
                rs[0]
            };
            b.ins().jump(
                join_block,
                &[cranelift_codegen::ir::BlockArg::Value(miss_result)],
            );

            // ===== Join =====
            b.switch_to_block(join_block);
            b.seal_block(join_block);
            let result = b.block_params(join_block)[0];
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecAlloc(dst, n_op, fill) => {
            // ADR 0012 D-2 (iter BV) — vm_alloc_vector_gc(n, fill).
            let n_v = lookup(map, *n_op)?;
            let f_v = lookup(map, *fill)?;
            let inst_ref = b.ins().call(alloc_vector_fnref, &[n_v, f_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecAlloc expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecRef(dst, vec, idx) => {
            let v_v = lookup(map, *vec)?;
            let i_v = lookup(map, *idx)?;
            let inst_ref = b.ins().call(vector_ref_fnref, &[v_v, i_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecRef expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecSet(dst, vec, idx, x) => {
            let v_v = lookup(map, *vec)?;
            let i_v = lookup(map, *idx)?;
            let x_v = lookup(map, *x)?;
            let inst_ref = b.ins().call(vector_set_fnref, &[v_v, i_v, x_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecSet expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecLength(dst, vec) => {
            // Returns raw Fixnum-shape i64; no stack-map declaration.
            let v_v = lookup(map, *vec)?;
            let inst_ref = b.ins().call(vector_length_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecLength expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::VecP(dst, src) => {
            // Consume-on-use predicate; returns 0/1 (Boolean).
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(vector_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StrAlloc(dst, n_op, fill) => {
            // ADR 0012 D-2 (iter BX) — vm_alloc_string_gc(n, fill).
            // `fill` is a Fixnum-shape codepoint (NOT a Gc handle);
            // the result is a fresh Gc<Value::String> handle.
            let n_v = lookup(map, *n_op)?;
            let f_v = lookup(map, *fill)?;
            let inst_ref = b.ins().call(alloc_string_fnref, &[n_v, f_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrAlloc expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrRef(dst, s, idx) => {
            // Returns a Fixnum-shape codepoint i64 (Character ABI
            // carrier); NOT a Gc handle — no stack-map declaration.
            let s_v = lookup(map, *s)?;
            let i_v = lookup(map, *idx)?;
            let inst_ref = b.ins().call(string_ref_fnref, &[s_v, i_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrRef expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StrLength(dst, s) => {
            // Returns raw Fixnum-shape i64; no stack-map declaration.
            let s_v = lookup(map, *s)?;
            let inst_ref = b.ins().call(string_length_fnref, &[s_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrLength expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StrP(dst, src) => {
            // Consume-on-use predicate; returns 0/1 (Boolean).
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(string_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StrEq(dst, a, b_op) => {
            // Consume both; returns 0/1 (Boolean).
            let a_v = lookup(map, *a)?;
            let b_v = lookup(map, *b_op)?;
            let inst_ref = b.ins().call(string_eq_fnref, &[a_v, b_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrEq expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::MakeClosure(dst, lambda_idx) => {
            // ADR 0012 D-2 (iter BZ) — emit a call to
            // `vm_make_closure(lambda_idx)`. The helper reads the
            // enclosing closure's env+bc from JIT TLS, builds a
            // VmClosure, and returns a fresh Gc<Value::Procedure>
            // raw handle (refcount = 1). Mark the result as
            // stack-map-tracked so it spills correctly across
            // subsequent calls.
            let idx_v = b.ins().iconst(I64, *lambda_idx as i64);
            let inst_ref = b.ins().call(make_closure_fnref, &[idx_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "MakeClosure expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Length(dst, lst) => {
            // ADR 0012 D-2 (iter CA) — vm_length_gc consumes the
            // operand and returns a raw Fixnum-shape spine count.
            // No stack-map declaration (result is not a Gc handle).
            let v_v = lookup(map, *lst)?;
            let inst_ref = b.ins().call(length_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Length expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::ListP(dst, src) => {
            // ADR 0012 D-2 (iter CA) — vm_list_p_gc consumes the
            // operand; returns 0/1 (Boolean).
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(list_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::Reverse(dst, lst) => {
            // ADR 0012 D-2 (iter CB) — vm_reverse_gc consumes the
            // operand and returns a fresh Gc<Value> handle. Mark
            // the result for stack-map tracking so subsequent calls
            // can safely interleave.
            let v_v = lookup(map, *lst)?;
            let inst_ref = b.ins().call(reverse_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Reverse expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Memq(dst, item, lst) => {
            // ADR 0012 D-2 (iter CC) — vm_memq_gc consumes both
            // operands and returns a Gc handle (matched sublist or
            // boolean #f). Mark for stack-map tracking.
            let item_v = lookup(map, *item)?;
            let lst_v = lookup(map, *lst)?;
            let inst_ref = b.ins().call(memq_fnref, &[item_v, lst_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Memq expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Assq(dst, key, alist) => {
            // ADR 0012 D-2 (iter CD) — vm_assq_gc consumes both
            // operands and returns a Gc handle (matched `(k . v)`
            // pair or boolean #f). Mark for stack-map tracking.
            let key_v = lookup(map, *key)?;
            let alist_v = lookup(map, *alist)?;
            let inst_ref = b.ins().call(assq_fnref, &[key_v, alist_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Assq expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::SetCar(dst, pair, val) => {
            // ADR 0012 D-2 (iter CE) — vm_set_car_gc consumes both
            // operands, mutates pair.car, returns Gc(Unspecified).
            let p_v = lookup(map, *pair)?;
            let v_v = lookup(map, *val)?;
            let inst_ref = b.ins().call(set_car_fnref, &[p_v, v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "SetCar expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::SetCdr(dst, pair, val) => {
            // ADR 0012 D-2 (iter CE) — vm_set_cdr_gc; mirrors SetCar.
            let p_v = lookup(map, *pair)?;
            let v_v = lookup(map, *val)?;
            let inst_ref = b.ins().call(set_cdr_fnref, &[p_v, v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "SetCdr expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Memv(dst, item, lst) => {
            // ADR 0012 D-2 (iter CG) — vm_memv_gc; mirrors Memq.
            let item_v = lookup(map, *item)?;
            let lst_v = lookup(map, *lst)?;
            let inst_ref = b.ins().call(memv_fnref, &[item_v, lst_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Memv expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Assv(dst, key, alist) => {
            // ADR 0012 D-2 (iter CG) — vm_assv_gc; mirrors Assq.
            let key_v = lookup(map, *key)?;
            let alist_v = lookup(map, *alist)?;
            let inst_ref = b.ins().call(assv_fnref, &[key_v, alist_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Assv expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Call(_, _, _) | Inst::DeoptCheck(_, _) => {
            return Err(JitError::Unsupported(format!(
                "{:?} not in iter-4b scope",
                inst
            )));
        }
    }
    Ok(())
}

fn binop(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    dst: RirValue,
    lhs: RirValue,
    rhs: RirValue,
    op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    let l = lookup(map, lhs)?;
    let r = lookup(map, rhs)?;
    let v = op(b, l, r);
    map.insert(dst, v);
    Ok(())
}

/// Flonum unary op: bitcast i64 carrier to f64, run the op, bitcast
/// back. Same semantics-on-bits as `fbinop`.
fn funary(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    dst: RirValue,
    src: RirValue,
    op: impl FnOnce(&mut FunctionBuilder, cranelift_codegen::ir::Value) -> cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    let s_i = lookup(map, src)?;
    let mf = cranelift_codegen::ir::MemFlags::new();
    let s_f = b.ins().bitcast(F64, mf, s_i);
    let v_f = op(b, s_f);
    let v_i = b.ins().bitcast(I64, mf, v_f);
    map.insert(dst, v_i);
    Ok(())
}

/// Flonum binop: bitcast both i64 carriers to f64, run the op,
/// bitcast the f64 result back to i64. Cranelift's optimizer folds
/// the redundant bitcasts when the producers are also f64-typed.
fn fbinop(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    dst: RirValue,
    lhs: RirValue,
    rhs: RirValue,
    op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    let l_i = lookup(map, lhs)?;
    let r_i = lookup(map, rhs)?;
    let mf = cranelift_codegen::ir::MemFlags::new();
    let l_f = b.ins().bitcast(F64, mf, l_i);
    let r_f = b.ins().bitcast(F64, mf, r_i);
    let v_f = op(b, l_f, r_f);
    let v_i = b.ins().bitcast(I64, mf, v_f);
    map.insert(dst, v_i);
    Ok(())
}

fn lookup(
    map: &HashMap<RirValue, cranelift_codegen::ir::Value>,
    v: RirValue,
) -> Result<cranelift_codegen::ir::Value, JitError> {
    map.get(&v)
        .copied()
        .ok_or_else(|| JitError::Codegen(format!("undefined RIR value {:?}", v)))
}

#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod testing {
    use super::*;

    /// Build a minimal `f(a, b) = a + b` RIR Function for tests.
    pub fn add_two_fixnums() -> RirFunction {
        let mut f = RirFunction::new("add_two");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(1), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![cs_rir::Inst::Add(
                cs_rir::Value(2),
                cs_rir::Value(0),
                cs_rir::Value(1),
            )],
            terminator: cs_rir::Term::Return(cs_rir::Value(2)),
        });
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::transmute;

    #[test]
    fn lower_add_two_fixnums_runs_natively() {
        let mut lowerer = Lowerer::new().expect("Lowerer::new");
        let f = testing::add_two_fixnums();
        let ptr = lowerer
            .compile_pure_fixnum(&f)
            .expect("compile_pure_fixnum");
        // SAFETY: ptr is the address of a finalized native
        // function with the (i64, i64) -> i64 signature we declared.
        let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(2, 3), 5);
        assert_eq!(func(-7, 7), 0);
        assert_eq!(func(i64::MAX - 1, 1), i64::MAX);
    }

    #[test]
    fn lower_const_plus_param_returns_const_when_arg_zero() {
        // f(x) = x + 100
        let mut f = RirFunction::new("addconst");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(100)),
                Inst::Add(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Return(cs_rir::Value(2)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(0), 100);
        assert_eq!(func(42), 142);
    }

    #[test]
    fn lower_lt_returns_one_or_zero() {
        // f(x, y) = if x < y then 1 else 0  (encoded as Lt + Return)
        let mut f = RirFunction::new("lt");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(1), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![Inst::Lt(
                cs_rir::Value(2),
                cs_rir::Value(0),
                cs_rir::Value(1),
            )],
            terminator: Term::Return(cs_rir::Value(2)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(1, 2), 1);
        assert_eq!(func(2, 1), 0);
        assert_eq!(func(5, 5), 0);
    }

    #[test]
    fn lower_mul_and_sub_chain() {
        // f(a, b, c) = (a + b) * (a - c)
        let mut f = RirFunction::new("mulsub");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(1), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(2), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Add(cs_rir::Value(3), cs_rir::Value(0), cs_rir::Value(1)),
                Inst::Sub(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(2)),
                Inst::Mul(cs_rir::Value(5), cs_rir::Value(3), cs_rir::Value(4)),
            ],
            terminator: Term::Return(cs_rir::Value(5)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64, i64, i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(5, 3, 1), (5 + 3) * (5 - 1)); // 8 * 4 = 32
        assert_eq!(func(0, 0, 0), 0);
    }

    #[test]
    fn lower_branch_two_arm_returns() {
        // f(x) = if x < 10 then x else x*2
        // entry:  cond = x < 10; brif cond, then, else
        // then:   return x
        // else:   t = x*2; return t
        let mut f = RirFunction::new("clamp");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(10)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(cs_rir::Value(2), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(cs_rir::Value(0)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(3), Const::Fixnum(2)),
                Inst::Mul(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
            ],
            terminator: Term::Return(cs_rir::Value(4)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(5), 5);
        assert_eq!(func(9), 9);
        assert_eq!(func(10), 20);
        assert_eq!(func(100), 200);
    }

    #[test]
    fn lower_jump_with_block_param() {
        // f(x) = let join_arg = (if x < 0 then -x else x) in join_arg + 1
        // entry: cond = x < 0; brif cond, neg, pos
        // neg:   t = 0 - x; jump join(t)
        // pos:   jump join(x)
        // join(p): r = p + 1; return r
        let mut f = RirFunction::new("absplus1");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(0)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(cs_rir::Value(2), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(3), Const::Fixnum(0)),
                Inst::Sub(cs_rir::Value(4), cs_rir::Value(3), cs_rir::Value(0)),
            ],
            terminator: Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(4)]),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![],
            terminator: Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(0)]),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(3),
            params: vec![(cs_rir::Value(5), cs_rir::Type::Fixnum)],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(6), Const::Fixnum(1)),
                Inst::Add(cs_rir::Value(7), cs_rir::Value(5), cs_rir::Value(6)),
            ],
            terminator: Term::Return(cs_rir::Value(7)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(5), 6);
        assert_eq!(func(-5), 6);
        assert_eq!(func(0), 1);
        assert_eq!(func(-100), 101);
    }

    #[test]
    fn lower_self_recursive_fib() {
        // fib(n) = if n < 2 then n else fib(n-1) + fib(n-2)
        //
        // entry: cond = n < 2; brif cond, base, rec
        // base:  return n
        // rec:   one = 1; n_minus_1 = n - one
        //        a = call_self(n_minus_1)
        //        two = 2; n_minus_2 = n - two
        //        b = call_self(n_minus_2)
        //        s = a + b; return s
        let mut f = RirFunction::new("fib");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(2)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(cs_rir::Value(2), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(cs_rir::Value(0)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(3), Const::Fixnum(1)),
                Inst::Sub(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
                Inst::CallSelf(cs_rir::Value(5), vec![cs_rir::Value(4)]),
                Inst::LoadConst(cs_rir::Value(6), Const::Fixnum(2)),
                Inst::Sub(cs_rir::Value(7), cs_rir::Value(0), cs_rir::Value(6)),
                Inst::CallSelf(cs_rir::Value(8), vec![cs_rir::Value(7)]),
                Inst::Add(cs_rir::Value(9), cs_rir::Value(5), cs_rir::Value(8)),
            ],
            terminator: Term::Return(cs_rir::Value(9)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(0), 0);
        assert_eq!(func(1), 1);
        assert_eq!(func(2), 1);
        assert_eq!(func(5), 5);
        assert_eq!(func(10), 55);
        assert_eq!(func(20), 6765);
    }

    #[test]
    fn empty_function_rejected() {
        let f = RirFunction::new("empty");
        let mut lowerer = Lowerer::new().unwrap();
        match lowerer.compile_pure_fixnum(&f) {
            Err(JitError::Codegen(msg)) => assert!(msg.contains("no blocks")),
            other => panic!("expected Codegen error, got {:?}", other),
        }
    }

    /// ADR 0012 D-2 (iter BL): compiling a body that keeps a Gc
    /// handle live across a call should populate
    /// `Lowerer::last_inner_stack_maps`. Cranelift's stack-map
    /// machinery only records slots that are live AT a safepoint
    /// (not just declared elsewhere) — so a body that does Cons +
    /// immediate Return produces zero records (no call in between).
    ///
    /// We force one entry by doing two Cons calls: the first Cons's
    /// result is live across the second's call site, so its slot
    /// gets recorded.
    #[test]
    fn cons_body_produces_stack_maps() {
        use cs_rir::Type;
        // f(a, b) = let p1 = cons(a, b) in let p2 = cons(a, b) in
        //           car(p1)  ;; consumes p1 — but it was live across
        //                      the second cons call.
        let mut f = RirFunction::new("two-cons-then-car");
        f.params.push((cs_rir::Value(0), Type::Fixnum));
        f.params.push((cs_rir::Value(1), Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Cons(cs_rir::Value(2), cs_rir::Value(0), 0, cs_rir::Value(1), 0),
                Inst::Cons(cs_rir::Value(3), cs_rir::Value(0), 0, cs_rir::Value(1), 0),
                // Consume p2 first so p1 (Value(2)) is what flows to
                // Return — and p1 was live across the second Cons's
                // call site.
                Inst::AnyDrop(cs_rir::Value(3)),
                Inst::Car(cs_rir::Value(4), cs_rir::Value(2)),
            ],
            terminator: Term::Return(cs_rir::Value(4)),
        });
        f.return_type = Type::Any;
        let mut lowerer = Lowerer::new().unwrap();
        lowerer
            .compile_pure_fixnum(&f)
            .expect("compile_pure_fixnum should succeed for two-cons body");
        assert!(
            !lowerer.last_inner_stack_maps.is_empty(),
            "expected at least one stack-map record, got {}",
            lowerer.last_inner_stack_maps.len()
        );
        for (pc, offsets) in &lowerer.last_inner_stack_maps {
            assert!(
                !offsets.is_empty(),
                "stack-map record at PC {} has no slot offsets",
                pc
            );
        }
    }
}
