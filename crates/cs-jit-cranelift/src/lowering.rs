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
use std::collections::HashSet;

use cranelift_codegen::ir::condcodes::IntCC;
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
    /// FuncId of `vm_env_set_nb(sym, value) -> ()` (Stage 3 baseline).
    /// Same shape as `vm_env_set_fixnum` but accepts a raw
    /// `NanboxValue` and decodes internally.
    env_set_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_env_define_local_nb(sym, value) -> ()` (iter8).
    env_define_local_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_pair(car, car_tag, cdr, cdr_tag) -> i64`.
    /// `Inst::Cons` lowers to a Cranelift call against this.
    alloc_pair_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_pair_region(car, car_tag, cdr, cdr_tag)
    /// -> i64`. Layer-3 region-allocating counterpart;
    /// `Inst::ConsRegion` lowers to a call against this. Only
    /// declared when the `regions` feature is on (cs-vm's
    /// `vm_alloc_pair_region_gc` is itself cfg-gated on
    /// `regions + countable-memory`).
    #[cfg(feature = "regions")]
    alloc_pair_region_func: cranelift_module::FuncId,
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
    /// FuncId of `vm_value_add_nb(a: i64, b: i64) -> i64` (Stage 3 baseline).
    /// `Inst::Add` in the uniform-NB body lowers to a Cranelift call
    /// against this on a Fixnum tag-check miss.
    value_add_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_sub_nb(a, b) -> i64` (Stage 3 baseline). See
    /// [`Self::value_add_nb_func`].
    value_sub_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_mul_nb(a, b) -> i64` (Stage 3 baseline). See
    /// [`Self::value_add_nb_func`].
    value_mul_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_div_nb(a, b) -> i64` (Phase 5b iter7).
    /// No inline fast path — Fixnum/Fixnum can return Rational.
    /// `Inst::Div` lowering calls this unconditionally.
    value_div_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_lt_nb(a, b) -> i64` (Stage 3 baseline). See
    /// [`Self::value_add_nb_func`].
    value_lt_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_eq_nb(a, b) -> i64` (Stage 3 baseline). See
    /// [`Self::value_add_nb_func`].
    value_eq_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_le_nb(a, b) -> i64` (Stage 3 baseline).
    value_le_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_gt_nb(a, b) -> i64` (Stage 3 baseline).
    value_gt_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_value_ge_nb(a, b) -> i64` (Stage 3 baseline).
    value_ge_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_eq_any(a, b) -> i64`. `Inst::EqAny` lowers
    /// to this. Consumes both boxes; returns 0/1.
    eq_any_func: cranelift_module::FuncId,
    /// FuncId of `vm_equal_gc(a, b) -> i64`. `Inst::EqualAny` lowers
    /// to this. R7RS deep structural equality. ADR 0012 D-2 (iter DZ).
    equal_func: cranelift_module::FuncId,
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
    /// FuncId of `vm_ic_dispatch(callee: i64, args_ptr: *const i64,
    /// n_args: usize, jit_ptr: *const u8) -> i64`. The IC hot path
    /// (`Inst::CallGeneral` hit branch) calls this instead of a bare
    /// `call_indirect`: it installs the callee's env / bytecode /
    /// stack-map-frame TLS guards first (ADR 0012 D-1, iter JI).
    ic_dispatch_func: cranelift_module::FuncId,
    /// FuncId of `vm_jit_set_tailcall(callee: i64, args_ptr: *const i64,
    /// n: i64) -> i64`. A tail-position `Call`/`CallGeneral` lowers to a
    /// call against this (instead of the IC dispatch) + a `return`, so
    /// the dispatch trampoline re-runs the callee in constant stack
    /// (ADR 0019).
    set_tailcall_func: cranelift_module::FuncId,
    /// FuncId of `vm_jit_request_deopt(reason: i64) -> i64` (ADR 0020
    /// Strategy C). Called by speculatively-unboxed Fixnum arithmetic on
    /// 47-bit overflow to request a deopt to bytecode.
    request_deopt_func: cranelift_module::FuncId,
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
    /// FuncId of `vm_string_lt_gc(a, b) -> i64`. ADR 0012 D-2 (iter DW).
    string_lt_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_gt_gc(a, b) -> i64`. ADR 0012 D-2 (iter DW).
    string_gt_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_le_gc(a, b) -> i64`. ADR 0012 D-2 (iter DW).
    string_le_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ge_gc(a, b) -> i64`. ADR 0012 D-2 (iter DW).
    string_ge_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ci_eq_gc(a, b) -> i64`. ADR 0012 D-2 (iter DX).
    string_ci_eq_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ci_lt_gc(a, b) -> i64`. ADR 0012 D-2 (iter DX).
    string_ci_lt_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ci_gt_gc(a, b) -> i64`. ADR 0012 D-2 (iter DX).
    string_ci_gt_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ci_le_gc(a, b) -> i64`. ADR 0012 D-2 (iter DX).
    string_ci_le_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_ci_ge_gc(a, b) -> i64`. ADR 0012 D-2 (iter DX).
    string_ci_ge_func: cranelift_module::FuncId,
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
    /// FuncId of `vm_member_gc(item, lst) -> i64`. equal?-flavored memq.
    /// ADR 0012 D-2 (iter CH).
    member_func: cranelift_module::FuncId,
    /// FuncId of `vm_assoc_gc(key, alist) -> i64`. equal?-flavored assq.
    /// ADR 0012 D-2 (iter CH).
    assoc_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_tail_gc(lst, n) -> i64`. ADR 0012 D-2
    /// (iter CK).
    list_tail_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_ref_gc(lst, n) -> i64`. ADR 0012 D-2
    /// (iter CK).
    list_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_substring_gc(s, start, end) -> i64`. Returns
    /// a fresh Gc<Value::String>. ADR 0012 D-2 (iter CM).
    substring_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_copy_gc(lst) -> i64`. Returns fresh Gc.
    /// ADR 0012 D-2 (iter CN).
    list_copy_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_set_gc(lst, n, val) -> i64`. Returns
    /// Gc(Unspecified). ADR 0012 D-2 (iter CO).
    list_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_gcd_fx(a, b) -> i64`. Both operands Fixnum,
    /// result Fixnum. ADR 0012 D-2 (iter CP).
    gcd_func: cranelift_module::FuncId,
    /// FuncId of `vm_lcm_fx(a, b) -> i64`. Both operands Fixnum,
    /// result Fixnum. ADR 0012 D-2 (iter CP).
    lcm_func: cranelift_module::FuncId,
    /// FuncId of `vm_expt_fx(base, exp) -> i64`. Both Fixnum,
    /// result Fixnum (deopts on overflow / neg exp).
    /// ADR 0012 D-2 (iter CT).
    expt_func: cranelift_module::FuncId,
    /// FuncId of `vm_arith_shift_fx(n, count) -> i64`.
    /// ADR 0012 D-2 (iter DL).
    arith_shift_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_p_gc(v) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter CQ).
    bv_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_length_gc(bv) -> i64`. Returns raw
    /// Fixnum. ADR 0012 D-2 (iter CQ).
    bv_length_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u8_ref_gc(bv, k) -> i64`. Returns
    /// raw Fixnum (byte). ADR 0012 D-2 (iter CQ).
    bv_u8_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_bytevector_gc(n, fill) -> i64`. Returns
    /// a fresh Gc<Value::ByteVector>. ADR 0012 D-2 (iter CR).
    bv_alloc_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u8_set_gc(bv, k, val) -> i64`. Returns
    /// Gc(Unspecified). ADR 0012 D-2 (iter CR).
    bv_u8_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_fill_gc(vec, fill) -> i64`. ADR 0012 D-2
    /// (iter CZ).
    vec_fill_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_fill_gc(bv, fill) -> i64`. ADR 0012
    /// D-2 (iter CZ).
    bv_fill_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_set_gc(s, k, ch) -> i64`. ADR 0012 D-2
    /// (iter DA).
    str_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_fill_gc(s, ch) -> i64`. ADR 0012 D-2
    /// (iter DH).
    str_fill_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_vector_buf(buf, n) -> i64`. Variadic
    /// vector constructor. ADR 0012 D-2 (iter DO).
    make_vector_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_string_buf(buf, n) -> i64`. Variadic
    /// string constructor. ADR 0012 D-2 (iter DP).
    make_string_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_bytevector_buf(buf, n) -> i64`. Variadic
    /// bytevector constructor. ADR 0012 D-2 (iter DQ).
    make_bytevector_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_append_buf(buf, n) -> i64`. Variadic
    /// string concatenation. ADR 0012 D-2 (iter DR).
    string_append_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_append_buf(buf, n) -> i64`. Variadic list
    /// concatenation. ADR 0012 D-2 (iter DS).
    append_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_append_buf(buf, n) -> i64`. Variadic
    /// vector concatenation. ADR 0012 D-2 (iter DT).
    vector_append_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_append_buf(buf, n) -> i64`. Variadic
    /// bytevector concatenation. ADR 0012 D-2 (iter DU).
    bytevector_append_buf_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_copy_gc(s) -> i64`. ADR 0012 D-2
    /// (iter DB).
    str_copy_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_copy_gc(v) -> i64`. ADR 0012 D-2
    /// (iter DB).
    vec_copy_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_copy_gc(bv) -> i64`. ADR 0012 D-2
    /// (iter DC).
    bv_copy_func: cranelift_module::FuncId,
    /// FuncId of `vm_procedure_p_gc(v) -> i64`. ADR 0012 D-2 (iter DD).
    procedure_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_p_gc(v) -> i64`. ADR 0012 D-2 (iter DD).
    port_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_eof_p_gc(v) -> i64`. ADR 0012 D-2 (iter DD).
    eof_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_symbol_p_gc(v) -> i64`. ADR 0012 D-2 (iter DD).
    symbol_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_p_gc(v) -> i64`. ADR 0012 D-2 (iter DE).
    char_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_boolean_p_gc(v) -> i64`. ADR 0012 D-2 (iter DE).
    boolean_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_fixnum_p_gc(v) -> i64`. ADR 0012 D-2 (iter DE).
    fixnum_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_p_gc(v) -> i64`. ADR 0012 D-2 (iter DE).
    flonum_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_sin(x) -> i64`. ADR 0012 D-2 (iter DF).
    flonum_sin_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_is_integer(x_bits) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter EH).
    flonum_is_integer_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_cos(x) -> i64`. ADR 0012 D-2 (iter DF).
    flonum_cos_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_tan(x) -> i64`. ADR 0012 D-2 (iter DF).
    flonum_tan_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_log(x) -> i64`. ADR 0012 D-2 (iter DF).
    flonum_log_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_exp(x) -> i64`. ADR 0012 D-2 (iter DF).
    flonum_exp_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_asin(x) -> i64`. ADR 0012 D-2 (iter DG).
    flonum_asin_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_acos(x) -> i64`. ADR 0012 D-2 (iter DG).
    flonum_acos_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_atan(x) -> i64`. ADR 0012 D-2 (iter DG).
    flonum_atan_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_log2(n, base) -> i64`. ADR 0012 D-2 (iter FM).
    flonum_log2_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_atan2(y, x) -> i64`. ADR 0012 D-2 (iter FM).
    flonum_atan2_func: cranelift_module::FuncId,
    /// FuncId of `vm_flonum_expt(x, y) -> i64`. ADR 0012 D-2 (iter GA).
    flonum_expt_func: cranelift_module::FuncId,
    /// FuncId of `vm_fl_even_p(x) -> i64`. ADR 0012 D-2 (iter GA).
    fl_even_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_fl_odd_p(x) -> i64`. ADR 0012 D-2 (iter GA).
    fl_odd_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_titlecase_gc(s) -> i64`. ADR 0012 D-2
    /// (iter GB).
    string_titlecase_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_hash_gc(s) -> i64`. ADR 0012 D-2 (iter GB).
    string_hash_func: cranelift_module::FuncId,
    /// FuncId of `vm_symbol_hash_gc(s) -> i64`. ADR 0012 D-2 (iter GB).
    symbol_hash_func: cranelift_module::FuncId,
    /// FuncId of `vm_input_port_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GC).
    input_port_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_output_port_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GC).
    output_port_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_binary_port_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GC).
    binary_port_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_textual_port_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GC).
    textual_port_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_output_port_open_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GP).
    output_port_open_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_eof_p_gc(p) -> i64` (0/1). ADR 0012 D-2 (iter GQ).
    port_eof_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_has_set_port_position_p_gc(p) -> i64` (0/1).
    /// ADR 0012 D-2 (iter GQ).
    port_has_set_port_position_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_position_gc(p) -> i64` (Fixnum).
    /// ADR 0012 D-2 (iter GR).
    port_position_func: cranelift_module::FuncId,
    /// FuncId of `vm_promise_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GD).
    promise_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_div_euclid(x, y) -> i64`. ADR 0012 D-2 (iter GE).
    div_euclid_func: cranelift_module::FuncId,
    /// FuncId of `vm_div0(x, y) -> i64`. ADR 0012 D-2 (iter HO).
    div0_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_hash_function_gc(ht) -> i64`. ADR 0012 D-2 (iter HQ).
    hashtable_hash_function_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_hashtable_equal_gc() -> i64`. ADR 0012 D-2 (iter HR).
    make_hashtable_equal_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_hashtable_eq_gc() -> i64`. ADR 0012 D-2 (iter HS).
    make_hashtable_eq_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_hashtable_eqv_gc() -> i64`. ADR 0012 D-2 (iter HS).
    make_hashtable_eqv_func: cranelift_module::FuncId,
    /// FuncId of `vm_mod0(x, y) -> i64`. ADR 0012 D-2 (iter HO).
    mod0_func: cranelift_module::FuncId,
    /// FuncId of `vm_mod_euclid(x, y) -> i64`. ADR 0012 D-2 (iter GE).
    mod_euclid_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_p_gc(v) -> i64` (0/1). ADR 0012 D-2 (iter GF).
    hashtable_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_size_gc(ht) -> i64`. ADR 0012 D-2 (iter GG).
    hashtable_size_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_mutable_p_gc(ht) -> i64` (0/1).
    /// ADR 0012 D-2 (iter GG).
    hashtable_mutable_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_keys_gc(ht) -> i64`. ADR 0012 D-2 (iter GH).
    hashtable_keys_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_values_gc(ht) -> i64`. ADR 0012 D-2 (iter GH).
    hashtable_values_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_clear_gc(ht) -> i64`. ADR 0012 D-2 (iter GI).
    hashtable_clear_func: cranelift_module::FuncId,
    /// FuncId of `vm_equal_hash_gc(v) -> i64` (Fixnum). ADR 0012 D-2 (iter GJ).
    equal_hash_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_to_alist_gc(ht) -> i64`. ADR 0012 D-2 (iter GJ).
    hashtable_to_alist_func: cranelift_module::FuncId,
    /// FuncId of `vm_file_exists_p_gc(path) -> i64` (0/1). ADR 0012 D-2 (iter GK).
    file_exists_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_current_second() -> i64` (Flonum bits). ADR 0012 D-2 (iter GL).
    current_second_func: cranelift_module::FuncId,
    /// FuncId of `vm_current_jiffy() -> i64` (Fixnum). ADR 0012 D-2 (iter GL).
    current_jiffy_func: cranelift_module::FuncId,
    /// FuncId of `vm_append_reverse_gc(rev, tail) -> i64`. ADR 0012 D-2 (iter GN).
    append_reverse_func: cranelift_module::FuncId,
    /// FuncId of `vm_alist_copy_gc(lst) -> i64`. ADR 0012 D-2 (iter GO).
    alist_copy_func: cranelift_module::FuncId,
    /// FuncId of `vm_delete_gc(target, lst) -> i64`. ADR 0012 D-2 (iter GS).
    delete_func: cranelift_module::FuncId,
    /// FuncId of `vm_delete_duplicates_gc(lst) -> i64`. ADR 0012 D-2 (iter GS).
    delete_duplicates_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_promise_gc(v) -> i64`. ADR 0012 D-2 (iter GT).
    make_promise_func: cranelift_module::FuncId,
    /// FuncId of `vm_force_forced_gc(p) -> i64`. ADR 0012 D-2 (iter GU).
    force_forced_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_contains_p_gc(ht, k) -> i64`. ADR 0012 D-2 (iter GV).
    hashtable_contains_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_delete_gc(ht, k) -> i64`. ADR 0012 D-2 (iter GW).
    hashtable_delete_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_set_gc(ht, k, v) -> i64`. ADR 0012 D-2 (iter GX).
    hashtable_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_ref_gc(ht, k, d) -> i64`. ADR 0012 D-2 (iter GY).
    hashtable_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_hashtable_copy_gc(ht) -> i64`. ADR 0012 D-2 (iter GZ).
    hashtable_copy_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_copy_slice_gc(v, s, e) -> i64`. ADR 0012 D-2 (iter HA).
    vector_copy_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_copy_from_gc(v, s) -> i64`. ADR 0012 D-2 (iter HT).
    vector_copy_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_copy_from_gc(bv, s) -> i64`. ADR 0012 D-2 (iter HU).
    bytevector_copy_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_copy_from_gc(s, st) -> i64`. ADR 0012 D-2 (iter HV).
    string_copy_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_fill_from_gc(bv, f, st) -> i64`. ADR 0012 D-2 (iter IA).
    bytevector_fill_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_fill_from_gc(v, f, st) -> i64`. ADR 0012 D-2 (iter IB).
    vector_fill_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_fill_from_gc(s, ch, st) -> i64`. ADR 0012 D-2 (iter IC).
    string_fill_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_to_string_slice_gc(v, s, e) -> i64`. ADR 0012 D-2 (iter ID).
    vector_to_string_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_vector_slice_gc(s, st, e) -> i64`. ADR 0012 D-2 (iter IE).
    string_to_vector_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_to_list_slice_gc(v, s, e) -> i64`. ADR 0012 D-2 (iter IF).
    vector_to_list_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_list_slice_gc(s, st, e) -> i64`. ADR 0012 D-2 (iter IG).
    string_to_list_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_to_list_slice_gc(bv, st, e) -> i64`. ADR 0012 D-2 (iter IH).
    bytevector_to_list_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_copy_slice_gc(bv, s, e) -> i64`. ADR 0012 D-2 (iter HC).
    bytevector_copy_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_eof_object_gc() -> i64`. ADR 0012 D-2 (iter HD).
    eof_object_func: cranelift_module::FuncId,
    /// FuncId of `vm_bitwise_bit_count(n) -> i64`. ADR 0012 D-2 (iter FN).
    bitwise_bit_count_func: cranelift_module::FuncId,
    /// FuncId of `vm_bitwise_length(n) -> i64`. ADR 0012 D-2 (iter FN).
    bitwise_length_func: cranelift_module::FuncId,
    /// FuncId of `vm_bitwise_arith_shift_left(n, count) -> i64`.
    /// ADR 0012 D-2 (iter FO).
    bitwise_arith_shift_left_func: cranelift_module::FuncId,
    /// FuncId of `vm_bitwise_arith_shift_right(n, count) -> i64`.
    /// ADR 0012 D-2 (iter FO).
    bitwise_arith_shift_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_bitwise_bit_set_p(n, bit) -> i64` (raw 0/1).
    /// ADR 0012 D-2 (iter FO).
    bitwise_bit_set_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s8_ref_gc(bv, k) -> i64`. Sign-extended
    /// byte read. ADR 0012 D-2 (iter FP).
    bytevector_s8_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s8_set_gc(bv, k, v) -> i64`. Returns
    /// Unspecified Gc handle. ADR 0012 D-2 (iter FP).
    bytevector_s8_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u16_native_ref_gc`. ADR 0012 D-2
    /// (iter FQ).
    bytevector_u16_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s16_native_ref_gc`. ADR 0012 D-2
    /// (iter FQ).
    bytevector_s16_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u16_native_set_gc`. ADR 0012 D-2
    /// (iter FQ).
    bytevector_u16_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s16_native_set_gc`. ADR 0012 D-2
    /// (iter FQ).
    bytevector_s16_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u32_native_ref_gc`. ADR 0012 D-2
    /// (iter FR).
    bytevector_u32_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s32_native_ref_gc`. ADR 0012 D-2
    /// (iter FR).
    bytevector_s32_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u32_native_set_gc`. ADR 0012 D-2
    /// (iter FR).
    bytevector_u32_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s32_native_set_gc`. ADR 0012 D-2
    /// (iter FR).
    bytevector_s32_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_ieee_single_native_ref_gc`.
    /// ADR 0012 D-2 (iter FS).
    bytevector_ieee_single_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_ieee_double_native_ref_gc`.
    /// ADR 0012 D-2 (iter FS).
    bytevector_ieee_double_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_ieee_single_native_set_gc`.
    /// ADR 0012 D-2 (iter FS).
    bytevector_ieee_single_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_ieee_double_native_set_gc`.
    /// ADR 0012 D-2 (iter FS).
    bytevector_ieee_double_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u64_native_ref_gc`. ADR 0012 D-2
    /// (iter FT).
    bytevector_u64_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s64_native_ref_gc`. ADR 0012 D-2
    /// (iter FT).
    bytevector_s64_native_ref_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_u64_native_set_gc`. ADR 0012 D-2
    /// (iter FT).
    bytevector_u64_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_s64_native_set_gc`. ADR 0012 D-2
    /// (iter FT).
    bytevector_s64_native_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_fx_first_bit_set(n) -> i64`. ADR 0012 D-2
    /// (iter FX).
    fx_first_bit_set_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_alphabetic_p(c) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter CI).
    char_alphabetic_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_numeric_p(c) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter CI).
    char_numeric_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_whitespace_p(c) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter CI).
    char_whitespace_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_upcase(c) -> i64`. Returns a Character
    /// codepoint. ADR 0012 D-2 (iter CJ).
    char_upcase_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_downcase(c) -> i64`. ADR 0012 D-2 (iter CJ).
    char_downcase_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_upper_case_p(c) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter CJ).
    char_upper_case_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_lower_case_p(c) -> i64`. Returns 0/1.
    /// ADR 0012 D-2 (iter CJ).
    char_lower_case_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_foldcase(c) -> i64`. Returns Character.
    /// ADR 0012 D-2 (iter CS).
    char_foldcase_func: cranelift_module::FuncId,
    /// FuncId of `vm_char_titlecase(c) -> i64`. Returns Character.
    /// ADR 0012 D-2 (iter CS).
    char_titlecase_func: cranelift_module::FuncId,
    /// FuncId of `vm_digit_value(c) -> i64`. Returns Any Gc handle
    /// (Fixnum or Boolean). ADR 0012 D-2 (iter CV).
    digit_value_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_to_list_gc(v) -> i64`. Returns fresh
    /// list Gc handle. ADR 0012 D-2 (iter CW).
    vector_to_list_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_to_vector_gc(lst) -> i64`. Returns fresh
    /// vector Gc handle. ADR 0012 D-2 (iter CW).
    list_to_vector_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_list_gc(s) -> i64`. ADR 0012 D-2
    /// (iter CX).
    string_to_list_func: cranelift_module::FuncId,
    /// FuncId of `vm_list_to_string_gc(lst) -> i64`. ADR 0012 D-2
    /// (iter CX).
    list_to_string_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_vector_gc(s) -> i64`. ADR 0012 D-2
    /// (iter DY).
    string_to_vector_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_to_string_gc(v) -> i64`. ADR 0012 D-2
    /// (iter DY).
    vector_to_string_func: cranelift_module::FuncId,
    /// FuncId of `vm_number_to_string_gc(n) -> i64`. ADR 0012 D-2
    /// (iter EC).
    number_to_string_func: cranelift_module::FuncId,
    /// FuncId of `vm_number_to_string_radix_gc(n, radix) -> i64`.
    /// ADR 0012 D-2 (iter II).
    number_to_string_radix_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_number_gc(s) -> i64`. ADR 0012 D-2
    /// (iter EC).
    string_to_number_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_number_radix_gc(s, radix) -> i64`.
    /// ADR 0012 D-2 (iter IJ).
    string_to_number_radix_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_list_unspecified_gc(n) -> i64`.
    /// ADR 0012 D-2 (iter IK).
    make_list_unspec_func: cranelift_module::FuncId,
    /// FuncId of `vm_alloc_vector_unspec_gc(n) -> i64`.
    /// ADR 0012 D-2 (iter JE).
    make_vector_unspec_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_to_list_slice_from_gc(v, start) -> i64`.
    /// ADR 0012 D-2 (iter IL).
    vector_to_list_slice_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_list_slice_from_gc(s, start) -> i64`.
    /// ADR 0012 D-2 (iter IM).
    string_to_list_slice_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_to_list_slice_from_gc(bv, start) -> i64`.
    /// ADR 0012 D-2 (iter IN).
    bytevector_to_list_slice_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_to_string_slice_from_gc(v, start) -> i64`.
    /// ADR 0012 D-2 (iter IO).
    vector_to_string_slice_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_vector_slice_from_gc(s, start) -> i64`.
    /// ADR 0012 D-2 (iter IP).
    string_to_vector_slice_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_copy_bang_from_gc(dest, at, src, src_start) -> i64`.
    /// ADR 0012 D-2 (iter IQ).
    vector_copy_bang_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_copy_bang_from_gc(dest, at, src, src_start) -> i64`.
    /// ADR 0012 D-2 (iter IR).
    bytevector_copy_bang_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_copy_bang_from_gc(dest, at, src, src_start) -> i64`.
    /// ADR 0012 D-2 (iter IS).
    string_copy_bang_from_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_copy_bang_slice_gc(dest, at, src, src_start, src_end) -> i64`.
    /// ADR 0012 D-2 (iter IT).
    vector_copy_bang_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_copy_bang_slice_gc(dest, at, src, src_start, src_end) -> i64`.
    /// ADR 0012 D-2 (iter IU).
    bytevector_copy_bang_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_copy_bang_slice_gc(dest, at, src, src_start, src_end) -> i64`.
    /// ADR 0012 D-2 (iter IV).
    string_copy_bang_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_reverse_gc(s) -> i64`. ADR 0012 D-2
    /// (iter EJ).
    string_reverse_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_upcase_gc(s) -> i64`. ADR 0012 D-2 (iter ET).
    string_upcase_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_downcase_gc(s) -> i64`. ADR 0012 D-2 (iter ET).
    string_downcase_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_foldcase_gc(s) -> i64`. ADR 0012 D-2 (iter ET).
    string_foldcase_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_contains_gc(h, n) -> i64`. ADR 0012 D-2 (iter EU).
    string_contains_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_prefix_p_gc(pre, s) -> i64`. ADR 0012 D-2 (iter EV).
    string_prefix_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_suffix_p_gc(suf, s) -> i64`. ADR 0012 D-2 (iter EV).
    string_suffix_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_join_gc(parts, sep) -> i64`. ADR 0012 D-2 (iter FE).
    string_join_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_split_gc(s, sep) -> i64`. ADR 0012 D-2 (iter FF).
    string_split_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_pad_gc(s, w) -> i64`. ADR 0012 D-2 (iter FG).
    string_pad_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_pad_right_gc(s, w) -> i64`. ADR 0012 D-2 (iter FG).
    string_pad_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_trim_gc(s) -> i64`. ADR 0012 D-2 (iter FH).
    string_trim_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_trim_left_gc(s) -> i64`. ADR 0012 D-2 (iter FH).
    string_trim_left_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_trim_right_gc(s) -> i64`. ADR 0012 D-2 (iter FH).
    string_trim_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_replace_all_gc(s, from, to) -> i64`.
    /// ADR 0012 D-2 (iter FI).
    string_replace_all_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_replace_first_gc(s, from, to) -> i64`.
    /// ADR 0012 D-2 (iter HE).
    string_replace_first_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_fill_slice_gc(bv, fill, s, e) -> i64`.
    /// ADR 0012 D-2 (iter HF).
    bytevector_fill_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_fill_slice_gc(v, fill, s, e) -> i64`.
    /// ADR 0012 D-2 (iter HG).
    vector_fill_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_fill_slice_gc(s, ch, st, e) -> i64`.
    /// ADR 0012 D-2 (iter HH).
    string_fill_slice_func: cranelift_module::FuncId,
    /// FuncId of `vm_exact_nonneg_int_p_gc(x) -> i64`. ADR 0012 D-2 (iter HI).
    exact_nonneg_int_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_eq_p_gc(a, b) -> i64`. ADR 0012 D-2 (iter HJ).
    bytevector_eq_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_eq_p_gc(a, b) -> i64`. ADR 0012 D-2 (iter HK).
    vector_eq_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_take_gc(s, n) -> i64`. ADR 0012 D-2 (iter FJ).
    string_take_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_drop_gc(s, n) -> i64`. ADR 0012 D-2 (iter FJ).
    string_drop_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_take_right_gc(s, n) -> i64`.
    /// ADR 0012 D-2 (iter FJ).
    string_take_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_drop_right_gc(s, n) -> i64`.
    /// ADR 0012 D-2 (iter FJ).
    string_drop_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_contains_right_gc(h, n) -> i64`.
    /// ADR 0012 D-2 (iter FK).
    string_contains_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_index_gc(s, c) -> i64`. ADR 0012 D-2 (iter FK).
    string_index_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_index_right_gc(s, c) -> i64`.
    /// ADR 0012 D-2 (iter FK).
    string_index_right_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_to_u8_list_gc(bv) -> i64`.
    /// ADR 0012 D-2 (iter FL).
    bytevector_to_u8_list_func: cranelift_module::FuncId,
    /// FuncId of `vm_u8_list_to_bytevector_gc(lst) -> i64`.
    /// ADR 0012 D-2 (iter FL).
    u8_list_to_bytevector_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_utf8_gc(s) -> i64`. ADR 0012 D-2 (iter FL).
    string_to_utf8_func: cranelift_module::FuncId,
    /// FuncId of `vm_utf8_to_string_gc(bv) -> i64`. ADR 0012 D-2 (iter FL).
    utf8_to_string_func: cranelift_module::FuncId,
    /// FuncId of `vm_make_list_fill_gc(n, fill) -> i64`. ADR 0012
    /// D-2 (iter EM).
    make_list_fill_func: cranelift_module::FuncId,
    /// FuncId of `vm_iota_n_gc(n) -> i64`. ADR 0012 D-2 (iter EN).
    iota_n_func: cranelift_module::FuncId,
    /// FuncId of `vm_iota_ns_gc(count, start) -> i64`. ADR 0012 D-2 (iter FC).
    iota_ns_func: cranelift_module::FuncId,
    /// FuncId of `vm_iota_nss_gc(count, start, step) -> i64`. ADR 0012 D-2 (iter FD).
    iota_nss_func: cranelift_module::FuncId,
    /// FuncId of `vm_last_pair_gc(lst) -> i64`. ADR 0012 D-2 (iter EO).
    last_pair_func: cranelift_module::FuncId,
    /// FuncId of `vm_last_gc(lst) -> i64`. ADR 0012 D-2 (iter EO).
    last_func: cranelift_module::FuncId,
    /// FuncId of `vm_take_gc(lst, n) -> i64`. ADR 0012 D-2 (iter EX).
    take_func: cranelift_module::FuncId,
    /// FuncId of `vm_drop_gc(lst, n) -> i64`. ADR 0012 D-2 (iter EX).
    drop_func: cranelift_module::FuncId,
    /// FuncId of `vm_null_list_p_gc(v) -> i64`. ADR 0012 D-2 (iter EY).
    null_list_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_concatenate_gc(lists) -> i64`. ADR 0012 D-2 (iter FB).
    concatenate_func: cranelift_module::FuncId,
    /// FuncId of `vm_not_pair_p_gc(v) -> i64`. ADR 0012 D-2 (iter FB).
    not_pair_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_proper_list_p_gc(v) -> i64`. ADR 0012 D-2 (iter EY).
    proper_list_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_dotted_list_p_gc(v) -> i64`. ADR 0012 D-2 (iter EY).
    dotted_list_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_circular_list_p_gc(v) -> i64`. ADR 0012 D-2 (iter EY).
    circular_list_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_vector_copy_bang_gc(dest, at, src) -> i64`.
    /// ADR 0012 D-2 (iter ER).
    vector_copy_bang_func: cranelift_module::FuncId,
    /// FuncId of `vm_bytevector_copy_bang_gc(dest, at, src) -> i64`.
    /// ADR 0012 D-2 (iter ES).
    bytevector_copy_bang_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_copy_bang_gc(dest, at, src) -> i64`.
    /// ADR 0012 D-2 (iter ES).
    string_copy_bang_func: cranelift_module::FuncId,
    /// FuncId of `vm_symbol_to_string_gc(sym) -> i64`. ADR 0012 D-2
    /// (iter CY).
    symbol_to_string_func: cranelift_module::FuncId,
    /// FuncId of `vm_string_to_symbol_gc(s) -> i64`. ADR 0012 D-2
    /// (iter CY).
    string_to_symbol_func: cranelift_module::FuncId,
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
        // cranelift's x86_64 backend asserts frame pointers are
        // present when emitting tail calls (cranelift-codegen
        // 0.131.1 / isa/x64/inst/emit.rs:1874). Our self-recursive
        // arith paths (e.g., ack) can emit tail calls, so on x64
        // we MUST keep frame pointers. aarch64 doesn't have this
        // restriction but the flag is harmless there.
        flag_builder
            .set("preserve_frame_pointers", "true")
            .map_err(|e| JitError::Codegen(format!("flag preserve_frame_pointers: {e}")))?;
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
        // Stage 3 baseline-JIT env-set helper (iter 3.5): accepts a
        // raw NanboxValue, decodes internally.
        builder.symbol("vm_env_set_nb", cs_vm::vm::vm_env_set_nb as *const u8);
        builder.symbol(
            "vm_env_define_local_nb",
            cs_vm::vm::vm_env_define_local_nb as *const u8,
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
        // Layer 3 — Inst::ConsRegion calls this region-allocating
        // counterpart of vm_alloc_pair. The runtime helper resolves
        // the current region via the cs-runtime resolver hook;
        // falls back to Rc allocation if no region is in scope.
        #[cfg(feature = "regions")]
        builder.symbol(
            "vm_alloc_pair_region",
            cs_vm::vm::vm_alloc_pair_region_gc as *const u8,
        );
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
        // Stage 3 baseline-JIT NB-typed arithmetic helpers (iter 3.0).
        builder.symbol("vm_value_add_nb", cs_vm::vm::vm_value_add_nb as *const u8);
        builder.symbol("vm_value_sub_nb", cs_vm::vm::vm_value_sub_nb as *const u8);
        builder.symbol("vm_value_mul_nb", cs_vm::vm::vm_value_mul_nb as *const u8);
        // Phase 5b iter7 — Fixnum/Fixnum division can produce a
        // Rational so there's no inline fast path; always call the
        // helper.
        builder.symbol("vm_value_div_nb", cs_vm::vm::vm_value_div_nb as *const u8);
        builder.symbol("vm_value_lt_nb", cs_vm::vm::vm_value_lt_nb as *const u8);
        builder.symbol("vm_value_eq_nb", cs_vm::vm::vm_value_eq_nb as *const u8);
        builder.symbol("vm_value_le_nb", cs_vm::vm::vm_value_le_nb as *const u8);
        builder.symbol("vm_value_gt_nb", cs_vm::vm::vm_value_gt_nb as *const u8);
        builder.symbol("vm_value_ge_nb", cs_vm::vm::vm_value_ge_nb as *const u8);
        builder.symbol("vm_eq_any", cs_vm::vm::vm_eq_any_gc as *const u8);
        // ADR 0012 D-2 (iter DZ) — equal? deep structural equality.
        builder.symbol("vm_equal_gc", cs_vm::vm::vm_equal_gc as *const u8);
        builder.symbol("vm_any_truthy", cs_vm::vm::vm_any_truthy_gc as *const u8);
        // ADR 0012 D-1 (iter BU) — slow-path general Call. Lowered
        // from `Inst::CallGeneral` whenever the bytecode's call
        // target is neither `self` nor a known builtin.
        builder.symbol("vm_call_general", cs_vm::vm::vm_call_general as *const u8);
        // ADR 0012 D-1 (iter JI) — IC hot-path dispatch helper.
        builder.symbol("vm_ic_dispatch", cs_vm::vm::vm_ic_dispatch as *const u8);
        // ADR 0019 — proper-tail-call bounce. A JIT body in tail
        // position calls this to stash (callee, args) and returns a
        // placeholder; the dispatch trampoline re-runs it in constant
        // stack instead of recursing through the IC.
        builder.symbol(
            "vm_jit_set_tailcall",
            cs_vm::vm::vm_jit_set_tailcall as *const u8,
        );
        // ADR 0020 Strategy C — speculative Fixnum unboxing requests a
        // deopt (and continues with a benign placeholder) when an
        // unboxed Add/Sub/Mul overflows the 47-bit Fixnum range.
        builder.symbol(
            "vm_jit_request_deopt",
            cs_vm::vm::vm_jit_request_deopt as *const u8,
        );
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
        // ADR 0012 D-2 (iter DW) — ordered string comparisons.
        builder.symbol("vm_string_lt_gc", cs_vm::vm::vm_string_lt_gc as *const u8);
        builder.symbol("vm_string_gt_gc", cs_vm::vm::vm_string_gt_gc as *const u8);
        builder.symbol("vm_string_le_gc", cs_vm::vm::vm_string_le_gc as *const u8);
        builder.symbol("vm_string_ge_gc", cs_vm::vm::vm_string_ge_gc as *const u8);
        // ADR 0012 D-2 (iter DX) — string-ci comparisons.
        builder.symbol(
            "vm_string_ci_eq_gc",
            cs_vm::vm::vm_string_ci_eq_gc as *const u8,
        );
        builder.symbol(
            "vm_string_ci_lt_gc",
            cs_vm::vm::vm_string_ci_lt_gc as *const u8,
        );
        builder.symbol(
            "vm_string_ci_gt_gc",
            cs_vm::vm::vm_string_ci_gt_gc as *const u8,
        );
        builder.symbol(
            "vm_string_ci_le_gc",
            cs_vm::vm::vm_string_ci_le_gc as *const u8,
        );
        builder.symbol(
            "vm_string_ci_ge_gc",
            cs_vm::vm::vm_string_ci_ge_gc as *const u8,
        );
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
        // ADR 0012 D-2 (iter CH) — member / assoc (equal?-flavored search).
        builder.symbol("vm_member_gc", cs_vm::vm::vm_member_gc as *const u8);
        builder.symbol("vm_assoc_gc", cs_vm::vm::vm_assoc_gc as *const u8);
        // ADR 0012 D-2 (iter CK) — list-tail / list-ref.
        builder.symbol("vm_list_tail_gc", cs_vm::vm::vm_list_tail_gc as *const u8);
        builder.symbol("vm_list_ref_gc", cs_vm::vm::vm_list_ref_gc as *const u8);
        // ADR 0012 D-2 (iter CM) — substring.
        builder.symbol("vm_substring_gc", cs_vm::vm::vm_substring_gc as *const u8);
        // ADR 0012 D-2 (iter CN) — list-copy.
        builder.symbol("vm_list_copy_gc", cs_vm::vm::vm_list_copy_gc as *const u8);
        // ADR 0012 D-2 (iter CO) — list-set!.
        builder.symbol("vm_list_set_gc", cs_vm::vm::vm_list_set_gc as *const u8);
        // ADR 0012 D-2 (iter CP) — gcd / lcm.
        builder.symbol("vm_gcd_fx", cs_vm::vm::vm_gcd_fx as *const u8);
        builder.symbol("vm_lcm_fx", cs_vm::vm::vm_lcm_fx as *const u8);
        // ADR 0012 D-2 (iter CT) — expt.
        builder.symbol("vm_expt_fx", cs_vm::vm::vm_expt_fx as *const u8);
        // ADR 0012 D-2 (iter DL) — arithmetic-shift.
        builder.symbol(
            "vm_arith_shift_fx",
            cs_vm::vm::vm_arith_shift_fx as *const u8,
        );
        // ADR 0012 D-2 (iter CQ) — bytevector read ops.
        builder.symbol(
            "vm_bytevector_p_gc",
            cs_vm::vm::vm_bytevector_p_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_length_gc",
            cs_vm::vm::vm_bytevector_length_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_u8_ref_gc",
            cs_vm::vm::vm_bytevector_u8_ref_gc as *const u8,
        );
        // ADR 0012 D-2 (iter CR) — bytevector write ops.
        builder.symbol(
            "vm_alloc_bytevector_gc",
            cs_vm::vm::vm_alloc_bytevector_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_u8_set_gc",
            cs_vm::vm::vm_bytevector_u8_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter CZ) — bulk fill ops.
        builder.symbol(
            "vm_vector_fill_gc",
            cs_vm::vm::vm_vector_fill_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_fill_gc",
            cs_vm::vm::vm_bytevector_fill_gc as *const u8,
        );
        // ADR 0012 D-2 (iter DA) — string-set!.
        builder.symbol("vm_string_set_gc", cs_vm::vm::vm_string_set_gc as *const u8);
        // ADR 0012 D-2 (iter DH) — string-fill!.
        builder.symbol(
            "vm_string_fill_gc",
            cs_vm::vm::vm_string_fill_gc as *const u8,
        );
        // ADR 0012 D-2 (iter DO) — variadic vector.
        builder.symbol(
            "vm_make_vector_buf",
            cs_vm::vm::vm_make_vector_buf as *const u8,
        );
        // ADR 0012 D-2 (iter DP) — variadic string.
        builder.symbol(
            "vm_make_string_buf",
            cs_vm::vm::vm_make_string_buf as *const u8,
        );
        // ADR 0012 D-2 (iter DQ) — variadic bytevector.
        builder.symbol(
            "vm_make_bytevector_buf",
            cs_vm::vm::vm_make_bytevector_buf as *const u8,
        );
        // ADR 0012 D-2 (iter DR) — variadic string-append.
        builder.symbol(
            "vm_string_append_buf",
            cs_vm::vm::vm_string_append_buf as *const u8,
        );
        // ADR 0012 D-2 (iter DS) — variadic append (lists).
        builder.symbol("vm_append_buf", cs_vm::vm::vm_append_buf as *const u8);
        // ADR 0012 D-2 (iter DT) — variadic vector-append.
        builder.symbol(
            "vm_vector_append_buf",
            cs_vm::vm::vm_vector_append_buf as *const u8,
        );
        // ADR 0012 D-2 (iter DU) — variadic bytevector-append.
        builder.symbol(
            "vm_bytevector_append_buf",
            cs_vm::vm::vm_bytevector_append_buf as *const u8,
        );
        // ADR 0012 D-2 (iter DB) — string-copy / vector-copy.
        builder.symbol(
            "vm_string_copy_gc",
            cs_vm::vm::vm_string_copy_gc as *const u8,
        );
        builder.symbol(
            "vm_vector_copy_gc",
            cs_vm::vm::vm_vector_copy_gc as *const u8,
        );
        // ADR 0012 D-2 (iter DC) — bytevector-copy.
        builder.symbol(
            "vm_bytevector_copy_gc",
            cs_vm::vm::vm_bytevector_copy_gc as *const u8,
        );
        // ADR 0012 D-2 (iter DD) — type predicates on Any.
        builder.symbol(
            "vm_procedure_p_gc",
            cs_vm::vm::vm_procedure_p_gc as *const u8,
        );
        builder.symbol("vm_port_p_gc", cs_vm::vm::vm_port_p_gc as *const u8);
        builder.symbol("vm_eof_p_gc", cs_vm::vm::vm_eof_p_gc as *const u8);
        builder.symbol("vm_symbol_p_gc", cs_vm::vm::vm_symbol_p_gc as *const u8);
        // ADR 0012 D-2 (iter DE) — more type predicates on Any.
        builder.symbol("vm_char_p_gc", cs_vm::vm::vm_char_p_gc as *const u8);
        builder.symbol("vm_boolean_p_gc", cs_vm::vm::vm_boolean_p_gc as *const u8);
        builder.symbol("vm_fixnum_p_gc", cs_vm::vm::vm_fixnum_p_gc as *const u8);
        builder.symbol("vm_flonum_p_gc", cs_vm::vm::vm_flonum_p_gc as *const u8);
        // ADR 0012 D-2 (iter DF) — flonum transcendentals.
        builder.symbol("vm_flonum_sin", cs_vm::vm::vm_flonum_sin as *const u8);
        // ADR 0012 D-2 (iter EH) — Flonum integer? helper.
        builder.symbol(
            "vm_flonum_is_integer",
            cs_vm::vm::vm_flonum_is_integer as *const u8,
        );
        builder.symbol("vm_flonum_cos", cs_vm::vm::vm_flonum_cos as *const u8);
        builder.symbol("vm_flonum_tan", cs_vm::vm::vm_flonum_tan as *const u8);
        builder.symbol("vm_flonum_log", cs_vm::vm::vm_flonum_log as *const u8);
        builder.symbol("vm_flonum_exp", cs_vm::vm::vm_flonum_exp as *const u8);
        // ADR 0012 D-2 (iter DG) — inverse trig.
        builder.symbol("vm_flonum_asin", cs_vm::vm::vm_flonum_asin as *const u8);
        builder.symbol("vm_flonum_acos", cs_vm::vm::vm_flonum_acos as *const u8);
        builder.symbol("vm_flonum_atan", cs_vm::vm::vm_flonum_atan as *const u8);
        // ADR 0012 D-2 (iter FM) — log/atan 2-arg.
        builder.symbol("vm_flonum_log2", cs_vm::vm::vm_flonum_log2 as *const u8);
        builder.symbol("vm_flonum_atan2", cs_vm::vm::vm_flonum_atan2 as *const u8);
        // ADR 0012 D-2 (iter GA) — flexpt, fleven?, flodd?.
        builder.symbol("vm_flonum_expt", cs_vm::vm::vm_flonum_expt as *const u8);
        builder.symbol("vm_fl_even_p", cs_vm::vm::vm_fl_even_p as *const u8);
        builder.symbol("vm_fl_odd_p", cs_vm::vm::vm_fl_odd_p as *const u8);
        // ADR 0012 D-2 (iter GB) — string-titlecase / string-hash / symbol-hash.
        builder.symbol(
            "vm_string_titlecase_gc",
            cs_vm::vm::vm_string_titlecase_gc as *const u8,
        );
        builder.symbol(
            "vm_string_hash_gc",
            cs_vm::vm::vm_string_hash_gc as *const u8,
        );
        builder.symbol(
            "vm_symbol_hash_gc",
            cs_vm::vm::vm_symbol_hash_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GC) — port-subtype predicates.
        builder.symbol(
            "vm_input_port_p_gc",
            cs_vm::vm::vm_input_port_p_gc as *const u8,
        );
        builder.symbol(
            "vm_output_port_p_gc",
            cs_vm::vm::vm_output_port_p_gc as *const u8,
        );
        builder.symbol(
            "vm_binary_port_p_gc",
            cs_vm::vm::vm_binary_port_p_gc as *const u8,
        );
        builder.symbol(
            "vm_textual_port_p_gc",
            cs_vm::vm::vm_textual_port_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GP) — output-port-open?.
        builder.symbol(
            "vm_output_port_open_p_gc",
            cs_vm::vm::vm_output_port_open_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GQ) — port-eof? + port-has-set-port-position!?.
        builder.symbol("vm_port_eof_p_gc", cs_vm::vm::vm_port_eof_p_gc as *const u8);
        builder.symbol(
            "vm_port_has_set_port_position_p_gc",
            cs_vm::vm::vm_port_has_set_port_position_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GR) — port-position.
        builder.symbol(
            "vm_port_position_gc",
            cs_vm::vm::vm_port_position_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GD) — promise?.
        builder.symbol("vm_promise_p_gc", cs_vm::vm::vm_promise_p_gc as *const u8);
        // ADR 0012 D-2 (iter GE) — R6RS div / mod.
        builder.symbol("vm_div_euclid", cs_vm::vm::vm_div_euclid as *const u8);
        // ADR 0012 D-2 (iter HO) — div0 / mod0.
        builder.symbol("vm_div0", cs_vm::vm::vm_div0 as *const u8);
        builder.symbol("vm_mod0", cs_vm::vm::vm_mod0 as *const u8);
        // ADR 0012 D-2 (iter HQ) — hashtable-hash-function.
        builder.symbol(
            "vm_hashtable_hash_function_gc",
            cs_vm::vm::vm_hashtable_hash_function_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HR) — make-hashtable 0-arg.
        builder.symbol(
            "vm_make_hashtable_equal_gc",
            cs_vm::vm::vm_make_hashtable_equal_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HS) — make-eq/eqv-hashtable.
        builder.symbol(
            "vm_make_hashtable_eq_gc",
            cs_vm::vm::vm_make_hashtable_eq_gc as *const u8,
        );
        builder.symbol(
            "vm_make_hashtable_eqv_gc",
            cs_vm::vm::vm_make_hashtable_eqv_gc as *const u8,
        );
        builder.symbol("vm_mod_euclid", cs_vm::vm::vm_mod_euclid as *const u8);
        // ADR 0012 D-2 (iter GF) — hashtable?.
        builder.symbol(
            "vm_hashtable_p_gc",
            cs_vm::vm::vm_hashtable_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GG) — hashtable-size / hashtable-mutable?.
        builder.symbol(
            "vm_hashtable_size_gc",
            cs_vm::vm::vm_hashtable_size_gc as *const u8,
        );
        builder.symbol(
            "vm_hashtable_mutable_p_gc",
            cs_vm::vm::vm_hashtable_mutable_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GH) — hashtable-keys / hashtable-values.
        builder.symbol(
            "vm_hashtable_keys_gc",
            cs_vm::vm::vm_hashtable_keys_gc as *const u8,
        );
        builder.symbol(
            "vm_hashtable_values_gc",
            cs_vm::vm::vm_hashtable_values_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GI) — hashtable-clear!.
        builder.symbol(
            "vm_hashtable_clear_gc",
            cs_vm::vm::vm_hashtable_clear_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GJ) — equal-hash + hashtable->alist.
        builder.symbol("vm_equal_hash_gc", cs_vm::vm::vm_equal_hash_gc as *const u8);
        builder.symbol(
            "vm_hashtable_to_alist_gc",
            cs_vm::vm::vm_hashtable_to_alist_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GK) — file-exists?.
        builder.symbol(
            "vm_file_exists_p_gc",
            cs_vm::vm::vm_file_exists_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GL) — current-second / current-jiffy.
        builder.symbol(
            "vm_current_second",
            cs_vm::vm::vm_current_second as *const u8,
        );
        builder.symbol("vm_current_jiffy", cs_vm::vm::vm_current_jiffy as *const u8);
        // ADR 0012 D-2 (iter GN) — append-reverse.
        builder.symbol(
            "vm_append_reverse_gc",
            cs_vm::vm::vm_append_reverse_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GO) — alist-copy.
        builder.symbol("vm_alist_copy_gc", cs_vm::vm::vm_alist_copy_gc as *const u8);
        // ADR 0012 D-2 (iter GS) — delete + delete-duplicates.
        builder.symbol("vm_delete_gc", cs_vm::vm::vm_delete_gc as *const u8);
        builder.symbol(
            "vm_delete_duplicates_gc",
            cs_vm::vm::vm_delete_duplicates_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GT) — make-promise.
        builder.symbol(
            "vm_make_promise_gc",
            cs_vm::vm::vm_make_promise_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GU) — force fast-path.
        builder.symbol(
            "vm_force_forced_gc",
            cs_vm::vm::vm_force_forced_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GV) — hashtable-contains?.
        builder.symbol(
            "vm_hashtable_contains_p_gc",
            cs_vm::vm::vm_hashtable_contains_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GW) — hashtable-delete!.
        builder.symbol(
            "vm_hashtable_delete_gc",
            cs_vm::vm::vm_hashtable_delete_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GX) — hashtable-set!.
        builder.symbol(
            "vm_hashtable_set_gc",
            cs_vm::vm::vm_hashtable_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GY) — hashtable-ref.
        builder.symbol(
            "vm_hashtable_ref_gc",
            cs_vm::vm::vm_hashtable_ref_gc as *const u8,
        );
        // ADR 0012 D-2 (iter GZ) — hashtable-copy.
        builder.symbol(
            "vm_hashtable_copy_gc",
            cs_vm::vm::vm_hashtable_copy_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HA) — vector-copy 3-arg slice.
        builder.symbol(
            "vm_vector_copy_slice_gc",
            cs_vm::vm::vm_vector_copy_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HT) — vector-copy 2-arg slice-to-end.
        builder.symbol(
            "vm_vector_copy_from_gc",
            cs_vm::vm::vm_vector_copy_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HU) — bytevector-copy 2-arg slice-to-end.
        builder.symbol(
            "vm_bytevector_copy_from_gc",
            cs_vm::vm::vm_bytevector_copy_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HV) — string-copy 2-arg slice-to-end.
        builder.symbol(
            "vm_string_copy_from_gc",
            cs_vm::vm::vm_string_copy_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IA) — bytevector-fill! 3-arg fill-from.
        builder.symbol(
            "vm_bytevector_fill_from_gc",
            cs_vm::vm::vm_bytevector_fill_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IB) — vector-fill! 3-arg fill-from.
        builder.symbol(
            "vm_vector_fill_from_gc",
            cs_vm::vm::vm_vector_fill_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IC) — string-fill! 3-arg fill-from.
        builder.symbol(
            "vm_string_fill_from_gc",
            cs_vm::vm::vm_string_fill_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter ID) — vector->string 3-arg slice.
        builder.symbol(
            "vm_vector_to_string_slice_gc",
            cs_vm::vm::vm_vector_to_string_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IE) — string->vector 3-arg slice.
        builder.symbol(
            "vm_string_to_vector_slice_gc",
            cs_vm::vm::vm_string_to_vector_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IF) — vector->list 3-arg slice.
        builder.symbol(
            "vm_vector_to_list_slice_gc",
            cs_vm::vm::vm_vector_to_list_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IG) — string->list 3-arg slice.
        builder.symbol(
            "vm_string_to_list_slice_gc",
            cs_vm::vm::vm_string_to_list_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IH) — bytevector->list 3-arg slice.
        builder.symbol(
            "vm_bytevector_to_list_slice_gc",
            cs_vm::vm::vm_bytevector_to_list_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HC) — bytevector-copy 3-arg slice.
        builder.symbol(
            "vm_bytevector_copy_slice_gc",
            cs_vm::vm::vm_bytevector_copy_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HD) — eof-object constructor.
        builder.symbol("vm_eof_object_gc", cs_vm::vm::vm_eof_object_gc as *const u8);
        // ADR 0012 D-2 (iter FN) — bitwise-bit-count / -length.
        builder.symbol(
            "vm_bitwise_bit_count",
            cs_vm::vm::vm_bitwise_bit_count as *const u8,
        );
        builder.symbol(
            "vm_bitwise_length",
            cs_vm::vm::vm_bitwise_length as *const u8,
        );
        // ADR 0012 D-2 (iter FO).
        builder.symbol(
            "vm_bitwise_arith_shift_left",
            cs_vm::vm::vm_bitwise_arith_shift_left as *const u8,
        );
        builder.symbol(
            "vm_bitwise_arith_shift_right",
            cs_vm::vm::vm_bitwise_arith_shift_right as *const u8,
        );
        builder.symbol(
            "vm_bitwise_bit_set_p",
            cs_vm::vm::vm_bitwise_bit_set_p as *const u8,
        );
        // ADR 0012 D-2 (iter FP) — bytevector-s8-ref/-set!.
        builder.symbol(
            "vm_bytevector_s8_ref_gc",
            cs_vm::vm::vm_bytevector_s8_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s8_set_gc",
            cs_vm::vm::vm_bytevector_s8_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FQ).
        builder.symbol(
            "vm_bytevector_u16_native_ref_gc",
            cs_vm::vm::vm_bytevector_u16_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s16_native_ref_gc",
            cs_vm::vm::vm_bytevector_s16_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_u16_native_set_gc",
            cs_vm::vm::vm_bytevector_u16_native_set_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s16_native_set_gc",
            cs_vm::vm::vm_bytevector_s16_native_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FR).
        builder.symbol(
            "vm_bytevector_u32_native_ref_gc",
            cs_vm::vm::vm_bytevector_u32_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s32_native_ref_gc",
            cs_vm::vm::vm_bytevector_s32_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_u32_native_set_gc",
            cs_vm::vm::vm_bytevector_u32_native_set_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s32_native_set_gc",
            cs_vm::vm::vm_bytevector_s32_native_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FS).
        builder.symbol(
            "vm_bytevector_ieee_single_native_ref_gc",
            cs_vm::vm::vm_bytevector_ieee_single_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_ieee_double_native_ref_gc",
            cs_vm::vm::vm_bytevector_ieee_double_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_ieee_single_native_set_gc",
            cs_vm::vm::vm_bytevector_ieee_single_native_set_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_ieee_double_native_set_gc",
            cs_vm::vm::vm_bytevector_ieee_double_native_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FT) — bytevector u64/s64 native ref/set!.
        builder.symbol(
            "vm_bytevector_u64_native_ref_gc",
            cs_vm::vm::vm_bytevector_u64_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s64_native_ref_gc",
            cs_vm::vm::vm_bytevector_s64_native_ref_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_u64_native_set_gc",
            cs_vm::vm::vm_bytevector_u64_native_set_gc as *const u8,
        );
        builder.symbol(
            "vm_bytevector_s64_native_set_gc",
            cs_vm::vm::vm_bytevector_s64_native_set_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FX) — fxfirst-bit-set.
        builder.symbol(
            "vm_fx_first_bit_set",
            cs_vm::vm::vm_fx_first_bit_set as *const u8,
        );
        // ADR 0012 D-2 (iter CI) — char Unicode predicates.
        builder.symbol(
            "vm_char_alphabetic_p",
            cs_vm::vm::vm_char_alphabetic_p as *const u8,
        );
        builder.symbol(
            "vm_char_numeric_p",
            cs_vm::vm::vm_char_numeric_p as *const u8,
        );
        builder.symbol(
            "vm_char_whitespace_p",
            cs_vm::vm::vm_char_whitespace_p as *const u8,
        );
        // ADR 0012 D-2 (iter CJ) — char case ops.
        builder.symbol("vm_char_upcase", cs_vm::vm::vm_char_upcase as *const u8);
        builder.symbol("vm_char_downcase", cs_vm::vm::vm_char_downcase as *const u8);
        builder.symbol(
            "vm_char_upper_case_p",
            cs_vm::vm::vm_char_upper_case_p as *const u8,
        );
        builder.symbol(
            "vm_char_lower_case_p",
            cs_vm::vm::vm_char_lower_case_p as *const u8,
        );
        // ADR 0012 D-2 (iter CS) — char-foldcase / char-titlecase.
        builder.symbol("vm_char_foldcase", cs_vm::vm::vm_char_foldcase as *const u8);
        builder.symbol(
            "vm_char_titlecase",
            cs_vm::vm::vm_char_titlecase as *const u8,
        );
        // ADR 0012 D-2 (iter CV) — digit-value.
        builder.symbol("vm_digit_value", cs_vm::vm::vm_digit_value as *const u8);
        // ADR 0012 D-2 (iter CW) — vector->list / list->vector.
        builder.symbol(
            "vm_vector_to_list_gc",
            cs_vm::vm::vm_vector_to_list_gc as *const u8,
        );
        builder.symbol(
            "vm_list_to_vector_gc",
            cs_vm::vm::vm_list_to_vector_gc as *const u8,
        );
        // ADR 0012 D-2 (iter DY) — string<->vector.
        builder.symbol(
            "vm_string_to_vector_gc",
            cs_vm::vm::vm_string_to_vector_gc as *const u8,
        );
        builder.symbol(
            "vm_vector_to_string_gc",
            cs_vm::vm::vm_vector_to_string_gc as *const u8,
        );
        // ADR 0012 D-2 (iter EC) — number<->string.
        builder.symbol(
            "vm_number_to_string_gc",
            cs_vm::vm::vm_number_to_string_gc as *const u8,
        );
        // ADR 0012 D-2 (iter II) — number->string 2-arg radix.
        builder.symbol(
            "vm_number_to_string_radix_gc",
            cs_vm::vm::vm_number_to_string_radix_gc as *const u8,
        );
        builder.symbol(
            "vm_string_to_number_gc",
            cs_vm::vm::vm_string_to_number_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IJ) — string->number 2-arg radix.
        builder.symbol(
            "vm_string_to_number_radix_gc",
            cs_vm::vm::vm_string_to_number_radix_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IL) — vector->list 2-arg slice-from.
        builder.symbol(
            "vm_vector_to_list_slice_from_gc",
            cs_vm::vm::vm_vector_to_list_slice_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IM) — string->list 2-arg slice-from.
        builder.symbol(
            "vm_string_to_list_slice_from_gc",
            cs_vm::vm::vm_string_to_list_slice_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IN) — bytevector->list 2-arg slice-from.
        builder.symbol(
            "vm_bytevector_to_list_slice_from_gc",
            cs_vm::vm::vm_bytevector_to_list_slice_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IO) — vector->string 2-arg slice-from.
        builder.symbol(
            "vm_vector_to_string_slice_from_gc",
            cs_vm::vm::vm_vector_to_string_slice_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IP) — string->vector 2-arg slice-from.
        builder.symbol(
            "vm_string_to_vector_slice_from_gc",
            cs_vm::vm::vm_string_to_vector_slice_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IQ) — vector-copy! 4-arg.
        builder.symbol(
            "vm_vector_copy_bang_from_gc",
            cs_vm::vm::vm_vector_copy_bang_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IR) — bytevector-copy! 4-arg.
        builder.symbol(
            "vm_bytevector_copy_bang_from_gc",
            cs_vm::vm::vm_bytevector_copy_bang_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IS) — string-copy! 4-arg.
        builder.symbol(
            "vm_string_copy_bang_from_gc",
            cs_vm::vm::vm_string_copy_bang_from_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IT) — vector-copy! 5-arg.
        builder.symbol(
            "vm_vector_copy_bang_slice_gc",
            cs_vm::vm::vm_vector_copy_bang_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IU) — bytevector-copy! 5-arg.
        builder.symbol(
            "vm_bytevector_copy_bang_slice_gc",
            cs_vm::vm::vm_bytevector_copy_bang_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IV) — string-copy! 5-arg.
        builder.symbol(
            "vm_string_copy_bang_slice_gc",
            cs_vm::vm::vm_string_copy_bang_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter IK) — make-list 1-arg.
        builder.symbol(
            "vm_make_list_unspecified_gc",
            cs_vm::vm::vm_make_list_unspecified_gc as *const u8,
        );
        // ADR 0012 D-2 (iter JE) — make-vector 1-arg.
        builder.symbol(
            "vm_alloc_vector_unspec_gc",
            cs_vm::vm::vm_alloc_vector_unspec_gc as *const u8,
        );
        // ADR 0012 D-2 (iter EJ) — string-reverse.
        builder.symbol(
            "vm_string_reverse_gc",
            cs_vm::vm::vm_string_reverse_gc as *const u8,
        );
        // ADR 0012 D-2 (iter ET) — string case conversion.
        builder.symbol(
            "vm_string_upcase_gc",
            cs_vm::vm::vm_string_upcase_gc as *const u8,
        );
        builder.symbol(
            "vm_string_downcase_gc",
            cs_vm::vm::vm_string_downcase_gc as *const u8,
        );
        builder.symbol(
            "vm_string_foldcase_gc",
            cs_vm::vm::vm_string_foldcase_gc as *const u8,
        );
        // ADR 0012 D-2 (iter EU) — string-contains.
        builder.symbol(
            "vm_string_contains_gc",
            cs_vm::vm::vm_string_contains_gc as *const u8,
        );
        // ADR 0012 D-2 (iter EV) — string-prefix?/suffix?.
        builder.symbol(
            "vm_string_prefix_p_gc",
            cs_vm::vm::vm_string_prefix_p_gc as *const u8,
        );
        builder.symbol(
            "vm_string_suffix_p_gc",
            cs_vm::vm::vm_string_suffix_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FE) — string-join.
        builder.symbol(
            "vm_string_join_gc",
            cs_vm::vm::vm_string_join_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FF) — string-split.
        builder.symbol(
            "vm_string_split_gc",
            cs_vm::vm::vm_string_split_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FG) — string-pad / string-pad-right.
        builder.symbol("vm_string_pad_gc", cs_vm::vm::vm_string_pad_gc as *const u8);
        builder.symbol(
            "vm_string_pad_right_gc",
            cs_vm::vm::vm_string_pad_right_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FH) — string trim family.
        builder.symbol(
            "vm_string_trim_gc",
            cs_vm::vm::vm_string_trim_gc as *const u8,
        );
        builder.symbol(
            "vm_string_trim_left_gc",
            cs_vm::vm::vm_string_trim_left_gc as *const u8,
        );
        builder.symbol(
            "vm_string_trim_right_gc",
            cs_vm::vm::vm_string_trim_right_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FI) — string-replace-all.
        builder.symbol(
            "vm_string_replace_all_gc",
            cs_vm::vm::vm_string_replace_all_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HE) — string-replace (first occurrence).
        builder.symbol(
            "vm_string_replace_first_gc",
            cs_vm::vm::vm_string_replace_first_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HF) — bytevector-fill! 4-arg slice.
        builder.symbol(
            "vm_bytevector_fill_slice_gc",
            cs_vm::vm::vm_bytevector_fill_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HG) — vector-fill! 4-arg slice.
        builder.symbol(
            "vm_vector_fill_slice_gc",
            cs_vm::vm::vm_vector_fill_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HH) — string-fill! 4-arg slice.
        builder.symbol(
            "vm_string_fill_slice_gc",
            cs_vm::vm::vm_string_fill_slice_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HI) — exact-nonnegative-integer?.
        builder.symbol(
            "vm_exact_nonneg_int_p_gc",
            cs_vm::vm::vm_exact_nonneg_int_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HJ) — bytevector=?.
        builder.symbol(
            "vm_bytevector_eq_p_gc",
            cs_vm::vm::vm_bytevector_eq_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter HK) — vector=?.
        builder.symbol(
            "vm_vector_eq_p_gc",
            cs_vm::vm::vm_vector_eq_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FJ) — string-take/-drop/-take-right/-drop-right.
        builder.symbol(
            "vm_string_take_gc",
            cs_vm::vm::vm_string_take_gc as *const u8,
        );
        builder.symbol(
            "vm_string_drop_gc",
            cs_vm::vm::vm_string_drop_gc as *const u8,
        );
        builder.symbol(
            "vm_string_take_right_gc",
            cs_vm::vm::vm_string_take_right_gc as *const u8,
        );
        builder.symbol(
            "vm_string_drop_right_gc",
            cs_vm::vm::vm_string_drop_right_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FK).
        builder.symbol(
            "vm_string_contains_right_gc",
            cs_vm::vm::vm_string_contains_right_gc as *const u8,
        );
        builder.symbol(
            "vm_string_index_gc",
            cs_vm::vm::vm_string_index_gc as *const u8,
        );
        builder.symbol(
            "vm_string_index_right_gc",
            cs_vm::vm::vm_string_index_right_gc as *const u8,
        );
        // ADR 0012 D-2 (iter FL).
        builder.symbol(
            "vm_bytevector_to_u8_list_gc",
            cs_vm::vm::vm_bytevector_to_u8_list_gc as *const u8,
        );
        builder.symbol(
            "vm_u8_list_to_bytevector_gc",
            cs_vm::vm::vm_u8_list_to_bytevector_gc as *const u8,
        );
        builder.symbol(
            "vm_string_to_utf8_gc",
            cs_vm::vm::vm_string_to_utf8_gc as *const u8,
        );
        builder.symbol(
            "vm_utf8_to_string_gc",
            cs_vm::vm::vm_utf8_to_string_gc as *const u8,
        );
        // ADR 0012 D-2 (iter EM) — make-list 2-arg.
        builder.symbol(
            "vm_make_list_fill_gc",
            cs_vm::vm::vm_make_list_fill_gc as *const u8,
        );
        // ADR 0012 D-2 (iter EN) — iota 1-arg.
        builder.symbol("vm_iota_n_gc", cs_vm::vm::vm_iota_n_gc as *const u8);
        // ADR 0012 D-2 (iter FC) — iota 2-arg.
        builder.symbol("vm_iota_ns_gc", cs_vm::vm::vm_iota_ns_gc as *const u8);
        // ADR 0012 D-2 (iter FD) — iota 3-arg.
        builder.symbol("vm_iota_nss_gc", cs_vm::vm::vm_iota_nss_gc as *const u8);
        // ADR 0012 D-2 (iter EO) — last-pair / last.
        builder.symbol("vm_last_pair_gc", cs_vm::vm::vm_last_pair_gc as *const u8);
        builder.symbol("vm_last_gc", cs_vm::vm::vm_last_gc as *const u8);
        // ADR 0012 D-2 (iter EX) — take / drop.
        builder.symbol("vm_take_gc", cs_vm::vm::vm_take_gc as *const u8);
        builder.symbol("vm_drop_gc", cs_vm::vm::vm_drop_gc as *const u8);
        // ADR 0012 D-2 (iter FB) — concatenate / not-pair?.
        builder.symbol(
            "vm_concatenate_gc",
            cs_vm::vm::vm_concatenate_gc as *const u8,
        );
        builder.symbol("vm_not_pair_p_gc", cs_vm::vm::vm_not_pair_p_gc as *const u8);
        // ADR 0012 D-2 (iter EY) — SRFI-1 list classifiers.
        builder.symbol(
            "vm_null_list_p_gc",
            cs_vm::vm::vm_null_list_p_gc as *const u8,
        );
        builder.symbol(
            "vm_proper_list_p_gc",
            cs_vm::vm::vm_proper_list_p_gc as *const u8,
        );
        builder.symbol(
            "vm_dotted_list_p_gc",
            cs_vm::vm::vm_dotted_list_p_gc as *const u8,
        );
        builder.symbol(
            "vm_circular_list_p_gc",
            cs_vm::vm::vm_circular_list_p_gc as *const u8,
        );
        // ADR 0012 D-2 (iter ER) — vector-copy! 3-arg.
        builder.symbol(
            "vm_vector_copy_bang_gc",
            cs_vm::vm::vm_vector_copy_bang_gc as *const u8,
        );
        // ADR 0012 D-2 (iter ES) — bytevector-copy! / string-copy! 3-arg.
        builder.symbol(
            "vm_bytevector_copy_bang_gc",
            cs_vm::vm::vm_bytevector_copy_bang_gc as *const u8,
        );
        builder.symbol(
            "vm_string_copy_bang_gc",
            cs_vm::vm::vm_string_copy_bang_gc as *const u8,
        );
        // ADR 0012 D-2 (iter CX) — string<->list.
        builder.symbol(
            "vm_string_to_list_gc",
            cs_vm::vm::vm_string_to_list_gc as *const u8,
        );
        builder.symbol(
            "vm_list_to_string_gc",
            cs_vm::vm::vm_list_to_string_gc as *const u8,
        );
        // ADR 0012 D-2 (iter CY) — symbol<->string.
        builder.symbol(
            "vm_symbol_to_string_gc",
            cs_vm::vm::vm_symbol_to_string_gc as *const u8,
        );
        builder.symbol(
            "vm_string_to_symbol_gc",
            cs_vm::vm::vm_string_to_symbol_gc as *const u8,
        );
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
        let env_set_nb_func = module
            .declare_function(
                "vm_env_set_nb",
                cranelift_module::Linkage::Import,
                &env_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function env_set_nb: {e}")))?;
        let env_define_local_nb_func = module
            .declare_function(
                "vm_env_define_local_nb",
                cranelift_module::Linkage::Import,
                &env_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function env_define_local_nb: {e}")))?;

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

        // Layer 3 — region-allocating counterpart, same signature.
        // The symbol resolves to cs-vm's `vm_alloc_pair_region_gc`
        // (registered in the JIT builder above).
        #[cfg(feature = "regions")]
        let alloc_pair_region_func = {
            let mut sig = module.make_signature();
            sig.params.push(AbiParam::new(I64)); // car
            sig.params.push(AbiParam::new(I64)); // car_tag
            sig.params.push(AbiParam::new(I64)); // cdr
            sig.params.push(AbiParam::new(I64)); // cdr_tag
            sig.returns.push(AbiParam::new(I64));
            module
                .declare_function(
                    "vm_alloc_pair_region",
                    cranelift_module::Linkage::Import,
                    &sig,
                )
                .map_err(|e| {
                    JitError::Codegen(format!("declare_function vm_alloc_pair_region: {e}"))
                })?
        };

        // vm_pair_car(pair: i64) -> i64 and vm_pair_cdr same shape.
        let mut pair_accessor_sig = module.make_signature();
        pair_accessor_sig.params.push(AbiParam::new(I64));
        pair_accessor_sig.returns.push(AbiParam::new(I64));
        // ADR 0012 D-2 (iter GL) — 0-arg helpers returning one i64.
        let mut zero_arg_sig = module.make_signature();
        zero_arg_sig.returns.push(AbiParam::new(I64));
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

        // Stage 3 baseline-JIT NB-typed arith / cmp helpers (iter 3.0).
        // All share the (i64, i64) -> i64 signature `box_typed_sig` was
        // built for; lazily declared here so the JIT module can call
        // them once the iter-3.1 lowering wires them in.
        let mut nb_arith_sig = module.make_signature();
        nb_arith_sig.params.push(AbiParam::new(I64));
        nb_arith_sig.params.push(AbiParam::new(I64));
        nb_arith_sig.returns.push(AbiParam::new(I64));
        let value_add_nb_func = module
            .declare_function(
                "vm_value_add_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_add_nb: {e}")))?;
        let value_sub_nb_func = module
            .declare_function(
                "vm_value_sub_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_sub_nb: {e}")))?;
        let value_mul_nb_func = module
            .declare_function(
                "vm_value_mul_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_mul_nb: {e}")))?;
        let value_div_nb_func = module
            .declare_function(
                "vm_value_div_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_div_nb: {e}")))?;
        let value_lt_nb_func = module
            .declare_function(
                "vm_value_lt_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_lt_nb: {e}")))?;
        let value_eq_nb_func = module
            .declare_function(
                "vm_value_eq_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_eq_nb: {e}")))?;
        let value_le_nb_func = module
            .declare_function(
                "vm_value_le_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_le_nb: {e}")))?;
        let value_gt_nb_func = module
            .declare_function(
                "vm_value_gt_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_gt_nb: {e}")))?;
        let value_ge_nb_func = module
            .declare_function(
                "vm_value_ge_nb",
                cranelift_module::Linkage::Import,
                &nb_arith_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_value_ge_nb: {e}")))?;

        // vm_eq_any(a, b) -> i64. Same shape as the existing
        // `box_typed_sig` (i64, i64) -> i64.
        let eq_any_func = module
            .declare_function(
                "vm_eq_any",
                cranelift_module::Linkage::Import,
                &box_typed_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_eq_any: {e}")))?;

        // ADR 0012 D-2 (iter DZ) — vm_equal_gc(a, b) -> i64. Same shape.
        let equal_func = module
            .declare_function(
                "vm_equal_gc",
                cranelift_module::Linkage::Import,
                &box_typed_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_equal_gc: {e}")))?;

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

        // ADR 0012 D-1 (iter JI) — vm_ic_dispatch(callee: i64,
        // args_ptr: i64, n_args: i64, jit_ptr: i64) -> i64. The IC
        // hot path calls this on a slot hit instead of a bare
        // `call_indirect`: it installs the callee's env / bytecode /
        // stack-map-frame TLS guards before running the cached body.
        let mut ic_dispatch_sig = module.make_signature();
        ic_dispatch_sig.params.push(AbiParam::new(I64)); // callee Gc handle
        ic_dispatch_sig.params.push(AbiParam::new(I64)); // args buffer pointer
        ic_dispatch_sig.params.push(AbiParam::new(I64)); // n_args
        ic_dispatch_sig.params.push(AbiParam::new(I64)); // cached jit_ptr
        ic_dispatch_sig.params.push(AbiParam::new(I64)); // slot_ptr (for cached_param_types read)
        ic_dispatch_sig.returns.push(AbiParam::new(I64));
        let ic_dispatch_func = module
            .declare_function(
                "vm_ic_dispatch",
                cranelift_module::Linkage::Import,
                &ic_dispatch_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_ic_dispatch: {e}")))?;

        // ADR 0019 — vm_jit_set_tailcall(callee: i64, args_ptr: i64,
        // n: i64) -> i64. Stashes a proper tail call for the dispatch
        // trampoline; returns a placeholder the body discards.
        let mut set_tailcall_sig = module.make_signature();
        set_tailcall_sig.params.push(AbiParam::new(I64)); // callee NB handle
        set_tailcall_sig.params.push(AbiParam::new(I64)); // args buffer pointer
        set_tailcall_sig.params.push(AbiParam::new(I64)); // n_args
        set_tailcall_sig.returns.push(AbiParam::new(I64));
        let set_tailcall_func = module
            .declare_function(
                "vm_jit_set_tailcall",
                cranelift_module::Linkage::Import,
                &set_tailcall_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_jit_set_tailcall: {e}")))?;

        // ADR 0020 Strategy C — vm_jit_request_deopt(reason: i64) -> i64.
        // Sets the deopt sentinel on unboxed-Fixnum overflow; returns a
        // placeholder the body discards.
        let mut request_deopt_sig = module.make_signature();
        request_deopt_sig.params.push(AbiParam::new(I64)); // reason code
        request_deopt_sig.returns.push(AbiParam::new(I64));
        let request_deopt_func = module
            .declare_function(
                "vm_jit_request_deopt",
                cranelift_module::Linkage::Import,
                &request_deopt_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_jit_request_deopt: {e}"))
            })?;

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
        // 4-arg helpers (4 i64 in, 1 i64 out). ADR 0012 D-2 (iter HF).
        let mut four_arg_sig = module.make_signature();
        four_arg_sig.params.push(AbiParam::new(I64));
        four_arg_sig.params.push(AbiParam::new(I64));
        four_arg_sig.params.push(AbiParam::new(I64));
        four_arg_sig.params.push(AbiParam::new(I64));
        four_arg_sig.returns.push(AbiParam::new(I64));
        // 5-arg helpers (5 i64 in, 1 i64 out). ADR 0012 D-2 (iter IT).
        let mut five_arg_sig = module.make_signature();
        five_arg_sig.params.push(AbiParam::new(I64));
        five_arg_sig.params.push(AbiParam::new(I64));
        five_arg_sig.params.push(AbiParam::new(I64));
        five_arg_sig.params.push(AbiParam::new(I64));
        five_arg_sig.params.push(AbiParam::new(I64));
        five_arg_sig.returns.push(AbiParam::new(I64));
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

        // ADR 0012 D-2 (iter DW) — ordered string comparisons.
        let string_lt_func = module
            .declare_function(
                "vm_string_lt_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_lt_gc: {e}")))?;
        let string_gt_func = module
            .declare_function(
                "vm_string_gt_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_gt_gc: {e}")))?;
        let string_le_func = module
            .declare_function(
                "vm_string_le_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_le_gc: {e}")))?;
        let string_ge_func = module
            .declare_function(
                "vm_string_ge_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ge_gc: {e}")))?;

        // ADR 0012 D-2 (iter DX) — string-ci comparisons.
        let string_ci_eq_func = module
            .declare_function(
                "vm_string_ci_eq_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ci_eq_gc: {e}")))?;
        let string_ci_lt_func = module
            .declare_function(
                "vm_string_ci_lt_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ci_lt_gc: {e}")))?;
        let string_ci_gt_func = module
            .declare_function(
                "vm_string_ci_gt_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ci_gt_gc: {e}")))?;
        let string_ci_le_func = module
            .declare_function(
                "vm_string_ci_le_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ci_le_gc: {e}")))?;
        let string_ci_ge_func = module
            .declare_function(
                "vm_string_ci_ge_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_ci_ge_gc: {e}")))?;

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

        // ADR 0012 D-2 (iter CH) — vm_member_gc / vm_assoc_gc.
        let member_func = module
            .declare_function(
                "vm_member_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_member_gc: {e}")))?;
        let assoc_func = module
            .declare_function(
                "vm_assoc_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_assoc_gc: {e}")))?;

        // ADR 0012 D-2 (iter CK) — vm_list_tail_gc / vm_list_ref_gc.
        // Same shape as memq/assq (two i64 in, one out).
        let list_tail_func = module
            .declare_function(
                "vm_list_tail_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_list_tail_gc: {e}")))?;
        let list_ref_func = module
            .declare_function(
                "vm_list_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_list_ref_gc: {e}")))?;

        // ADR 0012 D-2 (iter CM) — vm_substring_gc(s, start, end) -> i64.
        // Three i64 in, one out — same shape as vector_set_sig.
        let substring_func = module
            .declare_function(
                "vm_substring_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_substring_gc: {e}")))?;

        // ADR 0012 D-2 (iter CN) — vm_list_copy_gc(lst) -> i64.
        let list_copy_func = module
            .declare_function(
                "vm_list_copy_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_list_copy_gc: {e}")))?;

        // ADR 0012 D-2 (iter CO) — vm_list_set_gc(lst, n, val) -> i64.
        // Three i64 in, one out — same shape as vector_set_sig.
        let list_set_func = module
            .declare_function(
                "vm_list_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_list_set_gc: {e}")))?;

        // ADR 0012 D-2 (iter CP) — vm_gcd_fx / vm_lcm_fx. Two i64 in,
        // one out — same shape as vector_ref_sig.
        let gcd_func = module
            .declare_function(
                "vm_gcd_fx",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_gcd_fx: {e}")))?;
        let lcm_func = module
            .declare_function(
                "vm_lcm_fx",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_lcm_fx: {e}")))?;

        // ADR 0012 D-2 (iter CT) — vm_expt_fx(base, exp) -> i64.
        let expt_func = module
            .declare_function(
                "vm_expt_fx",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_expt_fx: {e}")))?;

        // ADR 0012 D-2 (iter DL) — vm_arith_shift_fx(n, count) -> i64.
        let arith_shift_func = module
            .declare_function(
                "vm_arith_shift_fx",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_arith_shift_fx: {e}")))?;

        // ADR 0012 D-2 (iter CQ) — bytevector read ops.
        let bv_p_func = module
            .declare_function(
                "vm_bytevector_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_bytevector_p_gc: {e}")))?;
        let bv_length_func = module
            .declare_function(
                "vm_bytevector_length_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_length_gc: {e}"))
            })?;
        let bv_u8_ref_func = module
            .declare_function(
                "vm_bytevector_u8_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_u8_ref_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter CR) — bytevector write ops.
        let bv_alloc_func = module
            .declare_function(
                "vm_alloc_bytevector_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_alloc_bytevector_gc: {e}"))
            })?;
        let bv_u8_set_func = module
            .declare_function(
                "vm_bytevector_u8_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_u8_set_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter CZ) — vm_vector_fill_gc / vm_bytevector_fill_gc.
        // Two i64 in, one out — vector_ref_sig shape.
        let vec_fill_func = module
            .declare_function(
                "vm_vector_fill_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_fill_gc: {e}")))?;
        let bv_fill_func = module
            .declare_function(
                "vm_bytevector_fill_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_fill_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter DA) — vm_string_set_gc(s, k, ch) -> i64.
        let str_set_func = module
            .declare_function(
                "vm_string_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_set_gc: {e}")))?;

        // ADR 0012 D-2 (iter DH) — vm_string_fill_gc(s, ch) -> i64.
        let str_fill_func = module
            .declare_function(
                "vm_string_fill_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_fill_gc: {e}")))?;

        // ADR 0012 D-2 (iter DO) — vm_make_vector_buf(buf, n) -> i64.
        // Same shape as vector_ref_sig (two i64 in, one out — buf is
        // a pointer, n is a count).
        let make_vector_buf_func = module
            .declare_function(
                "vm_make_vector_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_make_vector_buf: {e}")))?;

        // ADR 0012 D-2 (iter DP) — vm_make_string_buf(buf, n) -> i64.
        // Same shape: buf ptr + count → fresh Gc<Value::String> handle.
        let make_string_buf_func = module
            .declare_function(
                "vm_make_string_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_make_string_buf: {e}")))?;

        // ADR 0012 D-2 (iter DQ) — vm_make_bytevector_buf(buf, n) -> i64.
        let make_bytevector_buf_func = module
            .declare_function(
                "vm_make_bytevector_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_make_bytevector_buf: {e}"))
            })?;

        // ADR 0012 D-2 (iter DR) — vm_string_append_buf(buf, n) -> i64.
        let string_append_buf_func = module
            .declare_function(
                "vm_string_append_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_append_buf: {e}"))
            })?;

        // ADR 0012 D-2 (iter DS) — vm_append_buf(buf, n) -> i64.
        let append_buf_func = module
            .declare_function(
                "vm_append_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_append_buf: {e}")))?;

        // ADR 0012 D-2 (iter DT) — vm_vector_append_buf(buf, n) -> i64.
        let vector_append_buf_func = module
            .declare_function(
                "vm_vector_append_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_append_buf: {e}"))
            })?;

        // ADR 0012 D-2 (iter DU) — vm_bytevector_append_buf(buf, n) -> i64.
        let bytevector_append_buf_func = module
            .declare_function(
                "vm_bytevector_append_buf",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_append_buf: {e}"))
            })?;

        // ADR 0012 D-2 (iter DB) — vm_string_copy_gc / vm_vector_copy_gc.
        let str_copy_func = module
            .declare_function(
                "vm_string_copy_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_copy_gc: {e}")))?;
        let vec_copy_func = module
            .declare_function(
                "vm_vector_copy_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_copy_gc: {e}")))?;

        // ADR 0012 D-2 (iter DC) — vm_bytevector_copy_gc(bv) -> i64.
        let bv_copy_func = module
            .declare_function(
                "vm_bytevector_copy_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_copy_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter DD) — type predicates on Any operand.
        let procedure_p_func = module
            .declare_function(
                "vm_procedure_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_procedure_p_gc: {e}")))?;
        let port_p_func = module
            .declare_function(
                "vm_port_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_port_p_gc: {e}")))?;
        let eof_p_func = module
            .declare_function(
                "vm_eof_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_eof_p_gc: {e}")))?;
        let symbol_p_func = module
            .declare_function(
                "vm_symbol_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_symbol_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter DE) — more type predicates on Any.
        let char_p_func = module
            .declare_function(
                "vm_char_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_char_p_gc: {e}")))?;
        let boolean_p_func = module
            .declare_function(
                "vm_boolean_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_boolean_p_gc: {e}")))?;
        let fixnum_p_func = module
            .declare_function(
                "vm_fixnum_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_fixnum_p_gc: {e}")))?;
        let flonum_p_func = module
            .declare_function(
                "vm_flonum_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter DF) — flonum transcendentals. One i64
        // in (f64 bit pattern), one out — pair_accessor_sig shape.
        let flonum_sin_func = module
            .declare_function(
                "vm_flonum_sin",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_sin: {e}")))?;

        // ADR 0012 D-2 (iter EH) — Flonum integer? helper.
        let flonum_is_integer_func = module
            .declare_function(
                "vm_flonum_is_integer",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_flonum_is_integer: {e}"))
            })?;
        let flonum_cos_func = module
            .declare_function(
                "vm_flonum_cos",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_cos: {e}")))?;
        let flonum_tan_func = module
            .declare_function(
                "vm_flonum_tan",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_tan: {e}")))?;
        let flonum_log_func = module
            .declare_function(
                "vm_flonum_log",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_log: {e}")))?;
        let flonum_exp_func = module
            .declare_function(
                "vm_flonum_exp",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_exp: {e}")))?;

        // ADR 0012 D-2 (iter DG) — inverse trig.
        let flonum_asin_func = module
            .declare_function(
                "vm_flonum_asin",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_asin: {e}")))?;
        let flonum_acos_func = module
            .declare_function(
                "vm_flonum_acos",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_acos: {e}")))?;
        let flonum_atan_func = module
            .declare_function(
                "vm_flonum_atan",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_atan: {e}")))?;

        // ADR 0012 D-2 (iter FM) — log/atan 2-arg.
        let flonum_log2_func = module
            .declare_function(
                "vm_flonum_log2",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_log2: {e}")))?;
        let flonum_atan2_func = module
            .declare_function(
                "vm_flonum_atan2",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_atan2: {e}")))?;

        // ADR 0012 D-2 (iter GA) — flexpt, fleven?, flodd?.
        let flonum_expt_func = module
            .declare_function(
                "vm_flonum_expt",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_flonum_expt: {e}")))?;
        let fl_even_p_func = module
            .declare_function(
                "vm_fl_even_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_fl_even_p: {e}")))?;
        let fl_odd_p_func = module
            .declare_function(
                "vm_fl_odd_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_fl_odd_p: {e}")))?;

        // ADR 0012 D-2 (iter GB) — string-titlecase / string-hash / symbol-hash.
        let string_titlecase_func = module
            .declare_function(
                "vm_string_titlecase_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_titlecase_gc: {e}"))
            })?;
        let string_hash_func = module
            .declare_function(
                "vm_string_hash_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_hash_gc: {e}")))?;
        let symbol_hash_func = module
            .declare_function(
                "vm_symbol_hash_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_symbol_hash_gc: {e}")))?;

        // ADR 0012 D-2 (iter GC) — port-subtype predicates.
        let input_port_p_func = module
            .declare_function(
                "vm_input_port_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_input_port_p_gc: {e}")))?;
        let output_port_p_func = module
            .declare_function(
                "vm_output_port_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_output_port_p_gc: {e}")))?;
        let binary_port_p_func = module
            .declare_function(
                "vm_binary_port_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_binary_port_p_gc: {e}")))?;
        let textual_port_p_func = module
            .declare_function(
                "vm_textual_port_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_textual_port_p_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GP) — output-port-open?.
        let output_port_open_p_func = module
            .declare_function(
                "vm_output_port_open_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_output_port_open_p_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GQ) — port-eof? + port-has-set-port-position!?.
        let port_eof_p_func = module
            .declare_function(
                "vm_port_eof_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_port_eof_p_gc: {e}")))?;
        let port_has_set_port_position_p_func = module
            .declare_function(
                "vm_port_has_set_port_position_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_port_has_set_port_position_p_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter GR) — port-position.
        let port_position_func = module
            .declare_function(
                "vm_port_position_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_port_position_gc: {e}")))?;

        // ADR 0012 D-2 (iter GD) — promise?.
        let promise_p_func = module
            .declare_function(
                "vm_promise_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_promise_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter GE) — R6RS div / mod.
        let div_euclid_func = module
            .declare_function(
                "vm_div_euclid",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_div_euclid: {e}")))?;
        let mod_euclid_func = module
            .declare_function(
                "vm_mod_euclid",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_mod_euclid: {e}")))?;
        // ADR 0012 D-2 (iter HO) — div0 / mod0.
        let div0_func = module
            .declare_function(
                "vm_div0",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_div0: {e}")))?;
        let mod0_func = module
            .declare_function(
                "vm_mod0",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_mod0: {e}")))?;
        // ADR 0012 D-2 (iter HQ) — hashtable-hash-function.
        let hashtable_hash_function_func = module
            .declare_function(
                "vm_hashtable_hash_function_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_hashtable_hash_function_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter HR) — make-hashtable 0-arg (Equal kind).
        let make_hashtable_equal_func = module
            .declare_function(
                "vm_make_hashtable_equal_gc",
                cranelift_module::Linkage::Import,
                &zero_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_make_hashtable_equal_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HS) — make-eq-hashtable / make-eqv-hashtable.
        let make_hashtable_eq_func = module
            .declare_function(
                "vm_make_hashtable_eq_gc",
                cranelift_module::Linkage::Import,
                &zero_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_make_hashtable_eq_gc: {e}"))
            })?;
        let make_hashtable_eqv_func = module
            .declare_function(
                "vm_make_hashtable_eqv_gc",
                cranelift_module::Linkage::Import,
                &zero_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_make_hashtable_eqv_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter GF) — hashtable?.
        let hashtable_p_func = module
            .declare_function(
                "vm_hashtable_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_hashtable_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter GG) — hashtable-size / hashtable-mutable?.
        let hashtable_size_func = module
            .declare_function(
                "vm_hashtable_size_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_size_gc: {e}"))
            })?;
        let hashtable_mutable_p_func = module
            .declare_function(
                "vm_hashtable_mutable_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_mutable_p_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter GH) — hashtable-keys / hashtable-values.
        let hashtable_keys_func = module
            .declare_function(
                "vm_hashtable_keys_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_keys_gc: {e}"))
            })?;
        let hashtable_values_func = module
            .declare_function(
                "vm_hashtable_values_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_values_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GI) — hashtable-clear!.
        let hashtable_clear_func = module
            .declare_function(
                "vm_hashtable_clear_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_clear_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GJ) — equal-hash + hashtable->alist.
        let equal_hash_func = module
            .declare_function(
                "vm_equal_hash_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_equal_hash_gc: {e}")))?;
        let hashtable_to_alist_func = module
            .declare_function(
                "vm_hashtable_to_alist_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_to_alist_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GK) — file-exists?.
        let file_exists_p_func = module
            .declare_function(
                "vm_file_exists_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_file_exists_p_gc: {e}")))?;
        // ADR 0012 D-2 (iter GL) — current-second / current-jiffy.
        let current_second_func = module
            .declare_function(
                "vm_current_second",
                cranelift_module::Linkage::Import,
                &zero_arg_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_current_second: {e}")))?;
        let current_jiffy_func = module
            .declare_function(
                "vm_current_jiffy",
                cranelift_module::Linkage::Import,
                &zero_arg_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_current_jiffy: {e}")))?;
        // ADR 0012 D-2 (iter GN) — append-reverse.
        let append_reverse_func = module
            .declare_function(
                "vm_append_reverse_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_append_reverse_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GO) — alist-copy.
        let alist_copy_func = module
            .declare_function(
                "vm_alist_copy_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_alist_copy_gc: {e}")))?;
        // ADR 0012 D-2 (iter GS) — delete + delete-duplicates.
        let delete_func = module
            .declare_function(
                "vm_delete_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_delete_gc: {e}")))?;
        let delete_duplicates_func = module
            .declare_function(
                "vm_delete_duplicates_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_delete_duplicates_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GT) — make-promise.
        let make_promise_func = module
            .declare_function(
                "vm_make_promise_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_make_promise_gc: {e}")))?;
        // ADR 0012 D-2 (iter GU) — force fast-path.
        let force_forced_func = module
            .declare_function(
                "vm_force_forced_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_force_forced_gc: {e}")))?;
        // ADR 0012 D-2 (iter GV) — hashtable-contains?.
        let hashtable_contains_p_func = module
            .declare_function(
                "vm_hashtable_contains_p_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_contains_p_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GW) — hashtable-delete!.
        let hashtable_delete_func = module
            .declare_function(
                "vm_hashtable_delete_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_delete_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter GX) — hashtable-set!.
        let hashtable_set_func = module
            .declare_function(
                "vm_hashtable_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_hashtable_set_gc: {e}")))?;
        // ADR 0012 D-2 (iter GY) — hashtable-ref.
        let hashtable_ref_func = module
            .declare_function(
                "vm_hashtable_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_hashtable_ref_gc: {e}")))?;
        // ADR 0012 D-2 (iter GZ) — hashtable-copy.
        let hashtable_copy_func = module
            .declare_function(
                "vm_hashtable_copy_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_hashtable_copy_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HA) — vector-copy 3-arg slice.
        let vector_copy_slice_func = module
            .declare_function(
                "vm_vector_copy_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_copy_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HT) — vector-copy 2-arg slice-to-end.
        let vector_copy_from_func = module
            .declare_function(
                "vm_vector_copy_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_copy_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HU) — bytevector-copy 2-arg slice-to-end.
        let bytevector_copy_from_func = module
            .declare_function(
                "vm_bytevector_copy_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_copy_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HV) — string-copy 2-arg slice-to-end.
        let string_copy_from_func = module
            .declare_function(
                "vm_string_copy_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_copy_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IA) — bytevector-fill! 3-arg fill-from.
        let bytevector_fill_from_func = module
            .declare_function(
                "vm_bytevector_fill_from_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_fill_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IB) — vector-fill! 3-arg fill-from.
        let vector_fill_from_func = module
            .declare_function(
                "vm_vector_fill_from_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_fill_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IC) — string-fill! 3-arg fill-from.
        let string_fill_from_func = module
            .declare_function(
                "vm_string_fill_from_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_fill_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter ID) — vector->string 3-arg slice.
        let vector_to_string_slice_func = module
            .declare_function(
                "vm_vector_to_string_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_vector_to_string_slice_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IE) — string->vector 3-arg slice.
        let string_to_vector_slice_func = module
            .declare_function(
                "vm_string_to_vector_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_string_to_vector_slice_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IF) — vector->list 3-arg slice.
        let vector_to_list_slice_func = module
            .declare_function(
                "vm_vector_to_list_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_to_list_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IG) — string->list 3-arg slice.
        let string_to_list_slice_func = module
            .declare_function(
                "vm_string_to_list_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_to_list_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IH) — bytevector->list 3-arg slice.
        let bytevector_to_list_slice_func = module
            .declare_function(
                "vm_bytevector_to_list_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_to_list_slice_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter HC) — bytevector-copy 3-arg slice.
        let bytevector_copy_slice_func = module
            .declare_function(
                "vm_bytevector_copy_slice_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_copy_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HD) — eof-object constructor.
        let eof_object_func = module
            .declare_function(
                "vm_eof_object_gc",
                cranelift_module::Linkage::Import,
                &zero_arg_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_eof_object_gc: {e}")))?;

        // ADR 0012 D-2 (iter FN) — bitwise-bit-count / -length.
        let bitwise_bit_count_func = module
            .declare_function(
                "vm_bitwise_bit_count",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bitwise_bit_count: {e}"))
            })?;
        let bitwise_length_func = module
            .declare_function(
                "vm_bitwise_length",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_bitwise_length: {e}")))?;

        // ADR 0012 D-2 (iter FO) — bitwise shift-left/-right + bit-set?.
        let bitwise_arith_shift_left_func = module
            .declare_function(
                "vm_bitwise_arith_shift_left",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bitwise_arith_shift_left: {e}"))
            })?;
        let bitwise_arith_shift_right_func = module
            .declare_function(
                "vm_bitwise_arith_shift_right",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bitwise_arith_shift_right: {e}"
                ))
            })?;
        let bitwise_bit_set_p_func = module
            .declare_function(
                "vm_bitwise_bit_set_p",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bitwise_bit_set_p: {e}"))
            })?;

        // ADR 0012 D-2 (iter FP) — bytevector-s8-ref/-set!.
        let bytevector_s8_ref_func = module
            .declare_function(
                "vm_bytevector_s8_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_s8_ref_gc: {e}"))
            })?;
        let bytevector_s8_set_func = module
            .declare_function(
                "vm_bytevector_s8_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_s8_set_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter FQ) — bytevector u16/s16 native ref/set!.
        let bytevector_u16_native_ref_func = module
            .declare_function(
                "vm_bytevector_u16_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_u16_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_s16_native_ref_func = module
            .declare_function(
                "vm_bytevector_s16_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_s16_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_u16_native_set_func = module
            .declare_function(
                "vm_bytevector_u16_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_u16_native_set_gc: {e}"
                ))
            })?;
        let bytevector_s16_native_set_func = module
            .declare_function(
                "vm_bytevector_s16_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_s16_native_set_gc: {e}"
                ))
            })?;

        // ADR 0012 D-2 (iter FR) — bytevector u32/s32 native ref/set!.
        let bytevector_u32_native_ref_func = module
            .declare_function(
                "vm_bytevector_u32_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_u32_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_s32_native_ref_func = module
            .declare_function(
                "vm_bytevector_s32_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_s32_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_u32_native_set_func = module
            .declare_function(
                "vm_bytevector_u32_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_u32_native_set_gc: {e}"
                ))
            })?;
        let bytevector_s32_native_set_func = module
            .declare_function(
                "vm_bytevector_s32_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_s32_native_set_gc: {e}"
                ))
            })?;

        // ADR 0012 D-2 (iter FS) — IEEE float native ref/set!.
        let bytevector_ieee_single_native_ref_func = module
            .declare_function(
                "vm_bytevector_ieee_single_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_ieee_single_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_ieee_double_native_ref_func = module
            .declare_function(
                "vm_bytevector_ieee_double_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_ieee_double_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_ieee_single_native_set_func = module
            .declare_function(
                "vm_bytevector_ieee_single_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_ieee_single_native_set_gc: {e}"
                ))
            })?;
        let bytevector_ieee_double_native_set_func = module
            .declare_function(
                "vm_bytevector_ieee_double_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_ieee_double_native_set_gc: {e}"
                ))
            })?;

        // ADR 0012 D-2 (iter FT) — bytevector u64/s64 native ref/set!.
        let bytevector_u64_native_ref_func = module
            .declare_function(
                "vm_bytevector_u64_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_u64_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_s64_native_ref_func = module
            .declare_function(
                "vm_bytevector_s64_native_ref_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_s64_native_ref_gc: {e}"
                ))
            })?;
        let bytevector_u64_native_set_func = module
            .declare_function(
                "vm_bytevector_u64_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_u64_native_set_gc: {e}"
                ))
            })?;
        let bytevector_s64_native_set_func = module
            .declare_function(
                "vm_bytevector_s64_native_set_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_s64_native_set_gc: {e}"
                ))
            })?;

        // ADR 0012 D-2 (iter FX) — fxfirst-bit-set.
        let fx_first_bit_set_func = module
            .declare_function(
                "vm_fx_first_bit_set",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_fx_first_bit_set: {e}")))?;

        // ADR 0012 D-2 (iter CI) — char Unicode predicates. One i64
        // in (codepoint), one i64 out (0/1) — pair_accessor_sig shape.
        let char_alphabetic_p_func = module
            .declare_function(
                "vm_char_alphabetic_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_char_alphabetic_p: {e}"))
            })?;
        let char_numeric_p_func = module
            .declare_function(
                "vm_char_numeric_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_char_numeric_p: {e}")))?;
        let char_whitespace_p_func = module
            .declare_function(
                "vm_char_whitespace_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_char_whitespace_p: {e}"))
            })?;

        // ADR 0012 D-2 (iter CJ) — char case ops. All take one i64
        // (codepoint), return one i64 — pair_accessor_sig shape.
        let char_upcase_func = module
            .declare_function(
                "vm_char_upcase",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_char_upcase: {e}")))?;
        let char_downcase_func = module
            .declare_function(
                "vm_char_downcase",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_char_downcase: {e}")))?;
        let char_upper_case_p_func = module
            .declare_function(
                "vm_char_upper_case_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_char_upper_case_p: {e}"))
            })?;
        let char_lower_case_p_func = module
            .declare_function(
                "vm_char_lower_case_p",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_char_lower_case_p: {e}"))
            })?;

        // ADR 0012 D-2 (iter CS) — char-foldcase / char-titlecase.
        let char_foldcase_func = module
            .declare_function(
                "vm_char_foldcase",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_char_foldcase: {e}")))?;
        let char_titlecase_func = module
            .declare_function(
                "vm_char_titlecase",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_char_titlecase: {e}")))?;

        // ADR 0012 D-2 (iter CV) — vm_digit_value(c) -> i64.
        let digit_value_func = module
            .declare_function(
                "vm_digit_value",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_digit_value: {e}")))?;

        // ADR 0012 D-2 (iter CW) — vm_vector_to_list_gc / vm_list_to_vector_gc.
        let vector_to_list_func = module
            .declare_function(
                "vm_vector_to_list_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_to_list_gc: {e}"))
            })?;
        let list_to_vector_func = module
            .declare_function(
                "vm_list_to_vector_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_list_to_vector_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter CX) — vm_string_to_list_gc / vm_list_to_string_gc.
        let string_to_list_func = module
            .declare_function(
                "vm_string_to_list_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_to_list_gc: {e}"))
            })?;
        let list_to_string_func = module
            .declare_function(
                "vm_list_to_string_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_list_to_string_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter DY) — vm_string_to_vector_gc / vm_vector_to_string_gc.
        let string_to_vector_func = module
            .declare_function(
                "vm_string_to_vector_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_to_vector_gc: {e}"))
            })?;
        let vector_to_string_func = module
            .declare_function(
                "vm_vector_to_string_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_to_string_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter EC) — vm_number_to_string_gc / vm_string_to_number_gc.
        let number_to_string_func = module
            .declare_function(
                "vm_number_to_string_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_number_to_string_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter II) — number->string 2-arg radix.
        let number_to_string_radix_func = module
            .declare_function(
                "vm_number_to_string_radix_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_number_to_string_radix_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IJ) — string->number 2-arg radix.
        let string_to_number_radix_func = module
            .declare_function(
                "vm_string_to_number_radix_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_string_to_number_radix_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IK) — make-list 1-arg.
        let make_list_unspec_func = module
            .declare_function(
                "vm_make_list_unspecified_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_make_list_unspecified_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter JE) — make-vector 1-arg.
        let make_vector_unspec_func = module
            .declare_function(
                "vm_alloc_vector_unspec_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_alloc_vector_unspec_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IL) — vector->list 2-arg slice-from.
        let vector_to_list_slice_from_func = module
            .declare_function(
                "vm_vector_to_list_slice_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_vector_to_list_slice_from_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IM) — string->list 2-arg slice-from.
        let string_to_list_slice_from_func = module
            .declare_function(
                "vm_string_to_list_slice_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_string_to_list_slice_from_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IN) — bytevector->list 2-arg slice-from.
        let bytevector_to_list_slice_from_func = module
            .declare_function(
                "vm_bytevector_to_list_slice_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_to_list_slice_from_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IO) — vector->string 2-arg slice-from.
        let vector_to_string_slice_from_func = module
            .declare_function(
                "vm_vector_to_string_slice_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_vector_to_string_slice_from_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IP) — string->vector 2-arg slice-from.
        let string_to_vector_slice_from_func = module
            .declare_function(
                "vm_string_to_vector_slice_from_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_string_to_vector_slice_from_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IQ) — vector-copy! 4-arg.
        let vector_copy_bang_from_func = module
            .declare_function(
                "vm_vector_copy_bang_from_gc",
                cranelift_module::Linkage::Import,
                &four_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_copy_bang_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IR) — bytevector-copy! 4-arg.
        let bytevector_copy_bang_from_func = module
            .declare_function(
                "vm_bytevector_copy_bang_from_gc",
                cranelift_module::Linkage::Import,
                &four_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_copy_bang_from_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IS) — string-copy! 4-arg.
        let string_copy_bang_from_func = module
            .declare_function(
                "vm_string_copy_bang_from_gc",
                cranelift_module::Linkage::Import,
                &four_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_copy_bang_from_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter IT) — vector-copy! 5-arg.
        let vector_copy_bang_slice_func = module
            .declare_function(
                "vm_vector_copy_bang_slice_gc",
                cranelift_module::Linkage::Import,
                &five_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_vector_copy_bang_slice_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IU) — bytevector-copy! 5-arg.
        let bytevector_copy_bang_slice_func = module
            .declare_function(
                "vm_bytevector_copy_bang_slice_gc",
                cranelift_module::Linkage::Import,
                &five_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_bytevector_copy_bang_slice_gc: {e}"
                ))
            })?;
        // ADR 0012 D-2 (iter IV) — string-copy! 5-arg.
        let string_copy_bang_slice_func = module
            .declare_function(
                "vm_string_copy_bang_slice_gc",
                cranelift_module::Linkage::Import,
                &five_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!(
                    "declare_function vm_string_copy_bang_slice_gc: {e}"
                ))
            })?;
        let string_to_number_func = module
            .declare_function(
                "vm_string_to_number_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_to_number_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter EJ) — vm_string_reverse_gc(s) -> i64.
        let string_reverse_func = module
            .declare_function(
                "vm_string_reverse_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_reverse_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter ET) — string case conversion.
        let string_upcase_func = module
            .declare_function(
                "vm_string_upcase_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_upcase_gc: {e}")))?;
        let string_downcase_func = module
            .declare_function(
                "vm_string_downcase_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_downcase_gc: {e}"))
            })?;
        let string_foldcase_func = module
            .declare_function(
                "vm_string_foldcase_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_foldcase_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter EU) — vm_string_contains_gc(h, n) -> i64.
        let string_contains_func = module
            .declare_function(
                "vm_string_contains_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_contains_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter EV) — string-prefix?/suffix?.
        let string_prefix_p_func = module
            .declare_function(
                "vm_string_prefix_p_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_prefix_p_gc: {e}"))
            })?;
        let string_suffix_p_func = module
            .declare_function(
                "vm_string_suffix_p_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_suffix_p_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter FE) — vm_string_join_gc(parts, sep) -> i64.
        let string_join_func = module
            .declare_function(
                "vm_string_join_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_join_gc: {e}")))?;

        // ADR 0012 D-2 (iter FF) — vm_string_split_gc(s, sep) -> i64.
        let string_split_func = module
            .declare_function(
                "vm_string_split_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_split_gc: {e}")))?;

        // ADR 0012 D-2 (iter FG) — vm_string_pad{,_right}_gc.
        let string_pad_func = module
            .declare_function(
                "vm_string_pad_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_pad_gc: {e}")))?;
        let string_pad_right_func = module
            .declare_function(
                "vm_string_pad_right_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_pad_right_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter FH) — string trim family.
        let string_trim_func = module
            .declare_function(
                "vm_string_trim_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_trim_gc: {e}")))?;
        let string_trim_left_func = module
            .declare_function(
                "vm_string_trim_left_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_trim_left_gc: {e}"))
            })?;
        let string_trim_right_func = module
            .declare_function(
                "vm_string_trim_right_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_trim_right_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter FI) — string-replace-all.
        let string_replace_all_func = module
            .declare_function(
                "vm_string_replace_all_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_replace_all_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HE) — string-replace (first occurrence).
        let string_replace_first_func = module
            .declare_function(
                "vm_string_replace_first_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_replace_first_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HF) — bytevector-fill! 4-arg slice.
        let bytevector_fill_slice_func = module
            .declare_function(
                "vm_bytevector_fill_slice_gc",
                cranelift_module::Linkage::Import,
                &four_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_fill_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HG) — vector-fill! 4-arg slice.
        let vector_fill_slice_func = module
            .declare_function(
                "vm_vector_fill_slice_gc",
                cranelift_module::Linkage::Import,
                &four_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_fill_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HH) — string-fill! 4-arg slice.
        let string_fill_slice_func = module
            .declare_function(
                "vm_string_fill_slice_gc",
                cranelift_module::Linkage::Import,
                &four_arg_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_fill_slice_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HI) — exact-nonnegative-integer?.
        let exact_nonneg_int_p_func = module
            .declare_function(
                "vm_exact_nonneg_int_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_exact_nonneg_int_p_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HJ) — bytevector=?.
        let bytevector_eq_p_func = module
            .declare_function(
                "vm_bytevector_eq_p_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_eq_p_gc: {e}"))
            })?;
        // ADR 0012 D-2 (iter HK) — vector=?.
        let vector_eq_p_func = module
            .declare_function(
                "vm_vector_eq_p_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_vector_eq_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter FJ) — string-take/-drop/-take-right/-drop-right.
        let string_take_func = module
            .declare_function(
                "vm_string_take_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_take_gc: {e}")))?;
        let string_drop_func = module
            .declare_function(
                "vm_string_drop_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_drop_gc: {e}")))?;
        let string_take_right_func = module
            .declare_function(
                "vm_string_take_right_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_take_right_gc: {e}"))
            })?;
        let string_drop_right_func = module
            .declare_function(
                "vm_string_drop_right_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_drop_right_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter FK).
        let string_contains_right_func = module
            .declare_function(
                "vm_string_contains_right_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_contains_right_gc: {e}"))
            })?;
        let string_index_func = module
            .declare_function(
                "vm_string_index_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_string_index_gc: {e}")))?;
        let string_index_right_func = module
            .declare_function(
                "vm_string_index_right_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_index_right_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter FL).
        let bytevector_to_u8_list_func = module
            .declare_function(
                "vm_bytevector_to_u8_list_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_to_u8_list_gc: {e}"))
            })?;
        let u8_list_to_bytevector_func = module
            .declare_function(
                "vm_u8_list_to_bytevector_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_u8_list_to_bytevector_gc: {e}"))
            })?;
        let string_to_utf8_func = module
            .declare_function(
                "vm_string_to_utf8_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_to_utf8_gc: {e}"))
            })?;
        let utf8_to_string_func = module
            .declare_function(
                "vm_utf8_to_string_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_utf8_to_string_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter EM) — vm_make_list_fill_gc(n, fill) -> i64.
        // Same shape as vector_ref_sig (two i64 in, one out).
        let make_list_fill_func = module
            .declare_function(
                "vm_make_list_fill_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_make_list_fill_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter EN) — vm_iota_n_gc(n) -> i64.
        let iota_n_func = module
            .declare_function(
                "vm_iota_n_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_iota_n_gc: {e}")))?;

        // ADR 0012 D-2 (iter FC) — vm_iota_ns_gc(count, start) -> i64.
        let iota_ns_func = module
            .declare_function(
                "vm_iota_ns_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_iota_ns_gc: {e}")))?;

        // ADR 0012 D-2 (iter FD) — vm_iota_nss_gc(count, start, step) -> i64.
        let iota_nss_func = module
            .declare_function(
                "vm_iota_nss_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_iota_nss_gc: {e}")))?;

        // ADR 0012 D-2 (iter EO) — vm_last_pair_gc / vm_last_gc.
        let last_pair_func = module
            .declare_function(
                "vm_last_pair_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_last_pair_gc: {e}")))?;
        let last_func = module
            .declare_function(
                "vm_last_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_last_gc: {e}")))?;

        // ADR 0012 D-2 (iter EX) — vm_take_gc / vm_drop_gc.
        let take_func = module
            .declare_function(
                "vm_take_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_take_gc: {e}")))?;
        let drop_func = module
            .declare_function(
                "vm_drop_gc",
                cranelift_module::Linkage::Import,
                &vector_ref_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_drop_gc: {e}")))?;

        // ADR 0012 D-2 (iter FB) — vm_concatenate_gc / vm_not_pair_p_gc.
        let concatenate_func = module
            .declare_function(
                "vm_concatenate_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_concatenate_gc: {e}")))?;
        let not_pair_p_func = module
            .declare_function(
                "vm_not_pair_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_not_pair_p_gc: {e}")))?;

        // ADR 0012 D-2 (iter EY) — SRFI-1 list classifiers.
        let null_list_p_func = module
            .declare_function(
                "vm_null_list_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_null_list_p_gc: {e}")))?;
        let proper_list_p_func = module
            .declare_function(
                "vm_proper_list_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_proper_list_p_gc: {e}")))?;
        let dotted_list_p_func = module
            .declare_function(
                "vm_dotted_list_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_dotted_list_p_gc: {e}")))?;
        let circular_list_p_func = module
            .declare_function(
                "vm_circular_list_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_circular_list_p_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter ER) — vm_vector_copy_bang_gc(dest, at, src).
        let vector_copy_bang_func = module
            .declare_function(
                "vm_vector_copy_bang_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_vector_copy_bang_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter ES) — bytevector-copy! / string-copy! 3-arg.
        let bytevector_copy_bang_func = module
            .declare_function(
                "vm_bytevector_copy_bang_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_bytevector_copy_bang_gc: {e}"))
            })?;
        let string_copy_bang_func = module
            .declare_function(
                "vm_string_copy_bang_gc",
                cranelift_module::Linkage::Import,
                &vector_set_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_copy_bang_gc: {e}"))
            })?;

        // ADR 0012 D-2 (iter CY) — vm_symbol_to_string_gc / vm_string_to_symbol_gc.
        let symbol_to_string_func = module
            .declare_function(
                "vm_symbol_to_string_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_symbol_to_string_gc: {e}"))
            })?;
        let string_to_symbol_func = module
            .declare_function(
                "vm_string_to_symbol_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_string_to_symbol_gc: {e}"))
            })?;

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
            env_set_nb_func,
            env_define_local_nb_func,
            alloc_pair_func,
            #[cfg(feature = "regions")]
            alloc_pair_region_func,
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
            value_add_nb_func,
            value_sub_nb_func,
            value_mul_nb_func,
            value_div_nb_func,
            value_lt_nb_func,
            value_eq_nb_func,
            value_le_nb_func,
            value_gt_nb_func,
            value_ge_nb_func,
            eq_any_func,
            equal_func,
            any_truthy_func,
            call_general_func,
            ic_dispatch_func,
            set_tailcall_func,
            request_deopt_func,
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
            string_lt_func,
            string_gt_func,
            string_le_func,
            string_ge_func,
            string_ci_eq_func,
            string_ci_lt_func,
            string_ci_gt_func,
            string_ci_le_func,
            string_ci_ge_func,
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
            member_func,
            assoc_func,
            list_tail_func,
            list_ref_func,
            substring_func,
            list_copy_func,
            list_set_func,
            gcd_func,
            lcm_func,
            expt_func,
            arith_shift_func,
            bv_p_func,
            bv_length_func,
            bv_u8_ref_func,
            bv_alloc_func,
            bv_u8_set_func,
            vec_fill_func,
            bv_fill_func,
            str_set_func,
            str_fill_func,
            make_vector_buf_func,
            make_string_buf_func,
            make_bytevector_buf_func,
            string_append_buf_func,
            append_buf_func,
            vector_append_buf_func,
            bytevector_append_buf_func,
            str_copy_func,
            vec_copy_func,
            bv_copy_func,
            procedure_p_func,
            port_p_func,
            eof_p_func,
            symbol_p_func,
            char_p_func,
            boolean_p_func,
            fixnum_p_func,
            flonum_p_func,
            flonum_sin_func,
            flonum_is_integer_func,
            flonum_cos_func,
            flonum_tan_func,
            flonum_log_func,
            flonum_exp_func,
            flonum_asin_func,
            flonum_acos_func,
            flonum_atan_func,
            flonum_log2_func,
            flonum_atan2_func,
            flonum_expt_func,
            fl_even_p_func,
            fl_odd_p_func,
            string_titlecase_func,
            string_hash_func,
            symbol_hash_func,
            input_port_p_func,
            output_port_p_func,
            binary_port_p_func,
            textual_port_p_func,
            output_port_open_p_func,
            port_eof_p_func,
            port_has_set_port_position_p_func,
            port_position_func,
            promise_p_func,
            div_euclid_func,
            div0_func,
            mod0_func,
            hashtable_hash_function_func,
            make_hashtable_equal_func,
            make_hashtable_eq_func,
            make_hashtable_eqv_func,
            mod_euclid_func,
            hashtable_p_func,
            hashtable_size_func,
            hashtable_mutable_p_func,
            hashtable_keys_func,
            hashtable_values_func,
            hashtable_clear_func,
            equal_hash_func,
            hashtable_to_alist_func,
            file_exists_p_func,
            current_second_func,
            current_jiffy_func,
            append_reverse_func,
            alist_copy_func,
            delete_func,
            delete_duplicates_func,
            make_promise_func,
            force_forced_func,
            hashtable_contains_p_func,
            hashtable_delete_func,
            hashtable_set_func,
            hashtable_ref_func,
            hashtable_copy_func,
            vector_copy_slice_func,
            vector_copy_from_func,
            bytevector_copy_from_func,
            string_copy_from_func,
            bytevector_fill_from_func,
            vector_fill_from_func,
            string_fill_from_func,
            vector_to_string_slice_func,
            string_to_vector_slice_func,
            vector_to_list_slice_func,
            string_to_list_slice_func,
            bytevector_to_list_slice_func,
            bytevector_copy_slice_func,
            eof_object_func,
            bitwise_bit_count_func,
            bitwise_length_func,
            bitwise_arith_shift_left_func,
            bitwise_arith_shift_right_func,
            bitwise_bit_set_p_func,
            bytevector_s8_ref_func,
            bytevector_s8_set_func,
            bytevector_u16_native_ref_func,
            bytevector_s16_native_ref_func,
            bytevector_u16_native_set_func,
            bytevector_s16_native_set_func,
            bytevector_u32_native_ref_func,
            bytevector_s32_native_ref_func,
            bytevector_u32_native_set_func,
            bytevector_s32_native_set_func,
            bytevector_ieee_single_native_ref_func,
            bytevector_ieee_double_native_ref_func,
            bytevector_ieee_single_native_set_func,
            bytevector_ieee_double_native_set_func,
            bytevector_u64_native_ref_func,
            bytevector_s64_native_ref_func,
            bytevector_u64_native_set_func,
            bytevector_s64_native_set_func,
            fx_first_bit_set_func,
            char_alphabetic_p_func,
            char_numeric_p_func,
            char_whitespace_p_func,
            char_upcase_func,
            char_downcase_func,
            char_upper_case_p_func,
            char_lower_case_p_func,
            char_foldcase_func,
            char_titlecase_func,
            digit_value_func,
            vector_to_list_func,
            list_to_vector_func,
            string_to_list_func,
            list_to_string_func,
            string_to_vector_func,
            vector_to_string_func,
            number_to_string_func,
            number_to_string_radix_func,
            string_to_number_func,
            string_to_number_radix_func,
            make_list_unspec_func,
            make_vector_unspec_func,
            vector_to_list_slice_from_func,
            string_to_list_slice_from_func,
            bytevector_to_list_slice_from_func,
            vector_to_string_slice_from_func,
            string_to_vector_slice_from_func,
            vector_copy_bang_from_func,
            bytevector_copy_bang_from_func,
            string_copy_bang_from_func,
            vector_copy_bang_slice_func,
            bytevector_copy_bang_slice_func,
            string_copy_bang_slice_func,
            string_reverse_func,
            string_upcase_func,
            string_downcase_func,
            string_foldcase_func,
            string_contains_func,
            string_prefix_p_func,
            string_suffix_p_func,
            string_join_func,
            string_split_func,
            string_pad_func,
            string_pad_right_func,
            string_trim_func,
            string_trim_left_func,
            string_trim_right_func,
            string_replace_all_func,
            string_replace_first_func,
            bytevector_fill_slice_func,
            vector_fill_slice_func,
            string_fill_slice_func,
            exact_nonneg_int_p_func,
            bytevector_eq_p_func,
            vector_eq_p_func,
            string_take_func,
            string_drop_func,
            string_take_right_func,
            string_drop_right_func,
            string_contains_right_func,
            string_index_func,
            string_index_right_func,
            bytevector_to_u8_list_func,
            u8_list_to_bytevector_func,
            string_to_utf8_func,
            utf8_to_string_func,
            make_list_fill_func,
            iota_n_func,
            iota_ns_func,
            iota_nss_func,
            last_pair_func,
            last_func,
            take_func,
            drop_func,
            null_list_p_func,
            proper_list_p_func,
            dotted_list_p_func,
            circular_list_p_func,
            concatenate_func,
            not_pair_p_func,
            vector_copy_bang_func,
            bytevector_copy_bang_func,
            string_copy_bang_func,
            symbol_to_string_func,
            string_to_symbol_func,
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

    /// Replace `func_ctx` with a fresh context after any compilation
    /// function returns an error without calling `builder.finalize()`.
    /// Without this reset, stale block-status entries from the aborted
    /// function collide with fresh block IDs in the next compilation
    /// (Cranelift block IDs restart from 0 per `ClifFunction`), causing
    /// `ensure_inserted_block` to skip layout insertion and producing an
    /// empty `func.layout` that panics inside `remove_constant_phis`.
    fn reset_func_ctx(&mut self) {
        self.func_ctx = FunctionBuilderContext::new();
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
        if let Err(e) = self.compile_inner_body(rir, inner_id, inner_seq, &inner_sig) {
            self.reset_func_ctx();
            return Err(e);
        }
        // Compile outer — a plain SystemV trampoline that calls inner.
        // The specialized (pure-fixnum) tier has its own raw ABI handled
        // entirely within `compile_inner_body`; the trampoline here is a
        // pure passthrough (`raw_abi = false`).
        if let Err(e) =
            self.compile_outer_trampoline(rir, outer_id, outer_seq, &outer_sig, inner_id, false)
        {
            self.reset_func_ctx();
            return Err(e);
        }
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

    /// Stage 3 baseline tier entry. Compiles `rir` under the uniform
    /// `NanboxValue` ABI: every param and the return is an i64 NB bit
    /// pattern, with type checks emitted inline per arithmetic op (Iter
    /// 3.2+) or delegated to NB-typed runtime helpers (`vm_value_*_nb`,
    /// added in Iter 3.0).
    ///
    /// **Iter 3.1 — skeleton scope**: only `Inst::LoadConst`, `Inst::Add`,
    /// and `Term::Return` are lowered. Add always delegates to
    /// `vm_value_add_nb` (no inline Fixnum fast path yet — that's
    /// Iter 3.2). LoadConst encodes the const as an NB i64 at compile
    /// time. Other RIR variants return `JitError::Unsupported` so the
    /// translator's coverage analysis can mark functions as
    /// baseline-eligible vs not.
    pub fn compile_uniform_nb(&mut self, rir: &RirFunction) -> Result<*const u8, JitError> {
        if rir.blocks.is_empty() {
            return Err(JitError::Codegen("function has no blocks".into()));
        }

        // Eligibility prewalk — return `Err` before any FunctionBuilder
        // is created so a fallback to `compile_pure_fixnum` finds the
        // Lowerer's `func_ctx` clean. If the prewalk passes, the actual
        // lowering is guaranteed not to hit an `Unsupported` case (every
        // Inst arm has a handler).
        //
        // Catches two classes:
        //  - RIR variants the baseline tier doesn't yet lower (most of
        //    the wide builtin surface — VecAlloc/StrRef/FlonumAdd/etc.
        //    plus Inst::Call and DeoptCheck).
        //  - Non-tail-position `CallSelf` which would emit a regular
        //    Cranelift `call` and burn host stack on deep recursions.
        //
        // #47 — exception to the non-tail-CallSelf rejection: when the
        // body makes a cross-function call (`Call`/`CallGeneral`), allow
        // it through. Such bodies can't fall back to the specialized
        // (SystemV) tier — that tier miscompiles cross-function calls
        // (issue #19), so they'd otherwise drop to the VM with no JIT at
        // all. uniform-NB lowers them correctly (proper IC dispatch).
        // The host-stack guard targets pathological pure-arithmetic
        // recursion (tak), which has no cross-call and still routes to
        // the SystemV tier; map-style helpers recurse to data depth.
        let has_cross_call = rir.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|i| matches!(i, Inst::Call(_, _, _) | Inst::CallGeneral(_, _, _)))
        });
        // ADR 0020 Strategy C iter-3 — does this body qualify for the
        // raw-Fixnum self-call ABI (#50)? When true the prewalk admits
        // its non-tail self-calls: they pass raw i64 and burn the same
        // host stack as the pure-fixnum tier (no Tail-conv frame blowup),
        // and the body + trampoline lower in the raw representation so
        // recursion never boxes args / unboxes results at the call
        // boundary. See `detect_uniform_nb_raw_abi`.
        let raw_abi = detect_uniform_nb_raw_abi(rir);
        for blk in &rir.blocks {
            let last_idx = blk.insts.len().saturating_sub(1);
            for (i, inst) in blk.insts.iter().enumerate() {
                match inst {
                    Inst::LoadConst(_, _)
                    | Inst::Add(_, _, _)
                    | Inst::Sub(_, _, _)
                    | Inst::Mul(_, _, _)
                    | Inst::Div(_, _, _)
                    | Inst::Lt(_, _, _)
                    | Inst::Eq(_, _, _)
                    | Inst::FlonumAdd(_, _, _)
                    | Inst::FlonumSub(_, _, _)
                    | Inst::FlonumMul(_, _, _)
                    | Inst::FlonumDiv(_, _, _)
                    | Inst::FlonumLt(_, _, _)
                    | Inst::FlonumEq(_, _, _)
                    | Inst::FlonumMax(_, _, _)
                    | Inst::FlonumMin(_, _, _)
                    | Inst::FlonumSqrt(_, _)
                    | Inst::FlonumAbs(_, _)
                    | Inst::FlonumFloor(_, _)
                    | Inst::FlonumCeil(_, _)
                    | Inst::FlonumTrunc(_, _)
                    | Inst::FlonumRound(_, _)
                    | Inst::Cons(_, _, _, _, _)
                    | Inst::Car(_, _)
                    | Inst::Cdr(_, _)
                    | Inst::PairP(_, _)
                    | Inst::NullP(_, _)
                    | Inst::VecAlloc(_, _, _)
                    | Inst::VecRef(_, _, _)
                    | Inst::VecSet(_, _, _, _)
                    | Inst::VecLength(_, _)
                    | Inst::VecP(_, _)
                    | Inst::AnyClone(_, _)
                    | Inst::AnyDrop(_)
                    | Inst::MakeClosure(_, _)
                    | Inst::EnvLookup(_, _)
                    | Inst::EnvLookupAny(_, _)
                    | Inst::EnvSet(_, _)
                    | Inst::EnvDefineLocal(_, _)
                    | Inst::Call(_, _, _)
                    | Inst::CallGeneral(_, _, _)
                    | Inst::BoxTyped(_, _, _)
                    | Inst::AnyToFix(_, _)
                    | Inst::AnyToBool(_, _)
                    | Inst::AnyToFlo(_, _)
                    | Inst::AnyTruthy(_, _)
                    | Inst::FixToFlo(_, _)
                    | Inst::IntCharBitcast(_, _) => {
                        // Phase 5 iter3 — BoxTyped is an identity in
                        // uniform-NB: the typed-lane src is already an
                        // NB carrier with its proper tag, and any
                        // consumer decoding via gc_i64_to_value handles
                        // every NB tag uniformly.
                    }
                    Inst::CallSelf(_, _) => {
                        // Non-tail CallSelf still rejected here — the
                        // uniform-NB inner uses CallConv::Tail which
                        // burns more host stack per non-tail frame than
                        // CallConv::SystemV. tak's 3× nested CallSelf
                        // overflows on benchmark-scale recursion.
                        // Specialized tier (SystemV) handles non-tail
                        // CallSelf without overflow; falling back keeps
                        // tak working.
                        //
                        // Pattern (a)/(b): plain tail-call (CallSelf
                        // is last inst). Pattern (c): CallSelf at n-2
                        // followed by a no-op BoxTyped on its dst.
                        let last_check_ab = i == last_idx;
                        let pattern_c = if i + 1 == last_idx {
                            if let (
                                Some(Inst::CallSelf(call_dst, _)),
                                Some(Inst::BoxTyped(_, box_src, _)),
                            ) = (blk.insts.get(i), blk.insts.get(last_idx))
                            {
                                box_src == call_dst
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        let last_check = last_check_ab || pattern_c;
                        let tail_check = detect_tail_call_self(rir, blk).is_some() || pattern_c;
                        let is_tail = last_check && tail_check;
                        if !is_tail && !has_cross_call && !raw_abi {
                            return Err(JitError::Unsupported(
                                "uniform-nb: non-tail CallSelf would burn host stack".into(),
                            ));
                        }
                    }
                    other => {
                        return Err(JitError::Unsupported(format!(
                            "uniform-nb: Inst {:?} not yet supported",
                            other
                        )));
                    }
                }
            }
            // All current `Term` variants are supported in uniform-NB.
            // If a new variant is added to cs-rir, this match becomes
            // non-exhaustive and rustc fails the build — preferable to
            // a runtime error during JIT lowering. (Earlier code had
            // a defensive `other =>` arm here, but rustc flagged it
            // unreachable since the explicit arm matches everything.)
            match &blk.terminator {
                Term::Return(_) | Term::Jump(_, _) | Term::Branch(_, _, _, _) => {}
            }
        }

        let outer_sig = {
            let mut sig = Signature::new(CallConv::SystemV);
            for _ in &rir.params {
                sig.params.push(AbiParam::new(I64));
            }
            sig.returns.push(AbiParam::new(I64));
            sig
        };
        // Choose the inner calling convention by whether the body needs
        // guaranteed tail calls. `return_call` — emitted only for a
        // tail-position self-`CallSelf` — requires `CallConv::Tail`.
        // When the body has no tail self-recursion (e.g. a map-style
        // helper whose self-call is non-tail, an argument to `cons`),
        // `CallConv::SystemV` is both sufficient and preferable: SystemV
        // frames are smaller (Tail makes every register caller-save), so
        // the non-tail recursion gets a higher host-stack ceiling and
        // avoids the Tail-conv spill overhead. This is what lets #47's
        // map-style cross-function bodies JIT here (correct IC dispatch)
        // instead of dropping to the VM, with no depth regression.
        let needs_tail_conv = rir
            .blocks
            .iter()
            .any(|b| detect_uniform_nb_tail_self(rir, b).0.is_some());
        let inner_conv = if needs_tail_conv {
            CallConv::Tail
        } else {
            CallConv::SystemV
        };
        let inner_sig = {
            let mut sig = Signature::new(inner_conv);
            for _ in &rir.params {
                sig.params.push(AbiParam::new(I64));
            }
            sig.returns.push(AbiParam::new(I64));
            sig
        };

        let outer_seq = self.fresh_id();
        let inner_seq = self.fresh_id();
        let outer_module_name = format!("{}#{}.nb_outer", rir.name, outer_seq);
        let inner_module_name = format!("{}#{}.nb_inner", rir.name, inner_seq);
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

        if let Err(e) =
            self.compile_inner_body_uniform_nb(rir, inner_id, inner_seq, &inner_sig, raw_abi)
        {
            self.reset_func_ctx();
            return Err(e);
        }
        // Re-use the existing trampoline: it forwards i64 args to inner.
        // Under the raw-Fixnum self-call ABI it also unboxes each NB arg
        // on the way in and boxes the raw result on the way out — the
        // NB↔raw perimeter.
        if let Err(e) =
            self.compile_outer_trampoline(rir, outer_id, outer_seq, &outer_sig, inner_id, raw_abi)
        {
            self.reset_func_ctx();
            return Err(e);
        }
        self.module
            .finalize_definitions()
            .map_err(|e| JitError::Codegen(format!("finalize_definitions: {e}")))?;
        self.last_inner_base = self.module.get_finalized_function(inner_id);
        Ok(self.module.get_finalized_function(outer_id))
    }

    /// Body lowering for the uniform-NB tier. Iter 3.1 supported only
    /// `LoadConst`/`Add`/`Return`; iter 3.2 adds `Sub`/`Mul`/`Lt`/`Eq`
    /// with inline Fixnum fast paths (NB tag check + 47-bit payload
    /// extract + checked op + Fixnum NB re-encode), `Term::Jump`, and
    /// `Term::Branch` (NB truthiness check vs the `Boolean(false)` NB
    /// constant). Slow paths still delegate to `vm_value_*_nb`.
    fn compile_inner_body_uniform_nb(
        &mut self,
        rir: &RirFunction,
        inner_id: cranelift_module::FuncId,
        inner_seq: u64,
        inner_sig: &Signature,
        raw_abi: bool,
    ) -> Result<(), JitError> {
        let func_name = UserFuncName::user(0, inner_seq as u32);
        let mut clif = ClifFunction::with_name_signature(func_name, inner_sig.clone());
        let mut value_map: HashMap<RirValue, cranelift_codegen::ir::Value> = HashMap::new();
        // ADR 0020 Strategy C — raw (unboxed, sign-extended i64) lane,
        // parallel to value_map's NB lane. A value is present here iff it
        // is a proven raw Fixnum (a Fixnum param/const or an unboxed
        // arith result); consumers needing an NB carrier read value_map.
        let mut raw_map: HashMap<RirValue, cranelift_codegen::ir::Value> = HashMap::new();
        {
            let mut builder = FunctionBuilder::new(&mut clif, &mut self.func_ctx);
            let nb_helpers = NbHelpers {
                add: self
                    .module
                    .declare_func_in_func(self.value_add_nb_func, builder.func),
                sub: self
                    .module
                    .declare_func_in_func(self.value_sub_nb_func, builder.func),
                mul: self
                    .module
                    .declare_func_in_func(self.value_mul_nb_func, builder.func),
                div: self
                    .module
                    .declare_func_in_func(self.value_div_nb_func, builder.func),
                lt: self
                    .module
                    .declare_func_in_func(self.value_lt_nb_func, builder.func),
                eq: self
                    .module
                    .declare_func_in_func(self.value_eq_nb_func, builder.func),
                le: self
                    .module
                    .declare_func_in_func(self.value_le_nb_func, builder.func),
                gt: self
                    .module
                    .declare_func_in_func(self.value_gt_nb_func, builder.func),
                ge: self
                    .module
                    .declare_func_in_func(self.value_ge_nb_func, builder.func),
                alloc_pair: self
                    .module
                    .declare_func_in_func(self.alloc_pair_func, builder.func),
                pair_car: self
                    .module
                    .declare_func_in_func(self.pair_car_func, builder.func),
                pair_cdr: self
                    .module
                    .declare_func_in_func(self.pair_cdr_func, builder.func),
                pair_p: self
                    .module
                    .declare_func_in_func(self.pair_p_func, builder.func),
                null_p: self
                    .module
                    .declare_func_in_func(self.null_p_func, builder.func),
                alloc_vector: self
                    .module
                    .declare_func_in_func(self.alloc_vector_func, builder.func),
                vector_ref: self
                    .module
                    .declare_func_in_func(self.vector_ref_func, builder.func),
                vector_set: self
                    .module
                    .declare_func_in_func(self.vector_set_func, builder.func),
                vector_length: self
                    .module
                    .declare_func_in_func(self.vector_length_func, builder.func),
                vector_p: self
                    .module
                    .declare_func_in_func(self.vector_p_func, builder.func),
                value_clone: self
                    .module
                    .declare_func_in_func(self.value_clone_func, builder.func),
                value_drop: self
                    .module
                    .declare_func_in_func(self.value_drop_func, builder.func),
                self_fn: self.module.declare_func_in_func(inner_id, builder.func),
                call_general: self
                    .module
                    .declare_func_in_func(self.call_general_func, builder.func),
                set_tailcall: self
                    .module
                    .declare_func_in_func(self.set_tailcall_func, builder.func),
                request_deopt: self
                    .module
                    .declare_func_in_func(self.request_deopt_func, builder.func),
                env_lookup_any: self
                    .module
                    .declare_func_in_func(self.env_lookup_any_func, builder.func),
                env_set_nb: self
                    .module
                    .declare_func_in_func(self.env_set_nb_func, builder.func),
                env_define_local_nb: self
                    .module
                    .declare_func_in_func(self.env_define_local_nb_func, builder.func),
                make_closure: self
                    .module
                    .declare_func_in_func(self.make_closure_func, builder.func),
                closure_id_peek: self
                    .module
                    .declare_func_in_func(self.closure_id_peek_func, builder.func),
                ic_dispatch: self
                    .module
                    .declare_func_in_func(self.ic_dispatch_func, builder.func),
            };

            // Block-id map: RIR BlockId -> Cranelift Block.
            let mut block_map: HashMap<cs_rir::BlockId, cranelift_codegen::ir::Block> =
                HashMap::new();
            for blk in &rir.blocks {
                let cb = builder.create_block();
                block_map.insert(blk.id, cb);
            }
            let entry_block = *block_map
                .get(&rir.entry)
                .ok_or_else(|| JitError::Codegen("entry block missing".into()))?;
            for _ in &rir.params {
                builder.append_block_param(entry_block, I64);
            }
            builder.switch_to_block(entry_block);
            builder.seal_block(entry_block);
            for (i, (rv, ty)) in rir.params.iter().enumerate() {
                let p = builder.block_params(entry_block)[i];
                if raw_abi {
                    // iter-3 raw ABI: the trampoline already unboxed each
                    // Fixnum arg, so `p` IS the raw sign-extended payload.
                    // Seed the raw lane directly; materialize an NB carrier
                    // lazily for any NB consumer (DCE'd if only unboxed
                    // arith / self-calls read it). All params are Fixnum
                    // (the raw-ABI gate guarantees it).
                    raw_map.insert(*rv, p);
                    let nb = box_raw_fixnum(&mut builder, p);
                    value_map.insert(*rv, nb);
                } else {
                    value_map.insert(*rv, p);
                    // ADR 0020 Strategy C — unbox Fixnum params into the
                    // raw lane once at entry. run_jit_body_once has already
                    // validated the Fixnum tag (deopting on a miss), so the
                    // raw payload is trustworthy without a per-op check.
                    if matches!(ty, cs_rir::Type::Fixnum) {
                        let r = unbox_nb_fixnum(&mut builder, p);
                        raw_map.insert(*rv, r);
                    }
                }
            }

            for blk in &rir.blocks {
                if blk.id == rir.entry {
                    continue;
                }
                let cb = block_map[&blk.id];
                for _ in &blk.params {
                    builder.append_block_param(cb, I64);
                }
            }

            for blk in &rir.blocks {
                let cb = block_map[&blk.id];
                if blk.id != rir.entry {
                    builder.switch_to_block(cb);
                    for (i, (rv, _ty)) in blk.params.iter().enumerate() {
                        let p = builder.block_params(cb)[i];
                        if raw_abi {
                            // iter-3 raw ABI: every block param is a raw
                            // Fixnum (the gate verified all incoming args
                            // are raw, so predecessors pass raw block-args
                            // — see `lower_terminator_uniform_nb`). Seed
                            // the raw lane; box lazily for NB consumers.
                            raw_map.insert(*rv, p);
                            let nb = box_raw_fixnum(&mut builder, p);
                            value_map.insert(*rv, nb);
                        } else {
                            value_map.insert(*rv, p);
                        }
                    }
                    builder.seal_block(cb);
                }
                // Tail-position CallSelf detection — mirrors
                // compile_inner_body. Emits `return_call` instead of a
                // regular `call` so deeply recursive bodies (tak, etc.)
                // don't burn host stack.
                //
                // Uniform-NB also recognizes the CallSelf-then-
                // BoxTyped pattern: when the recursive arm's value
                // must widen to Any to merge with a sibling typed
                // branch, the translator emits BoxTyped(_, callself,
                // _) before the trivial Jump-to-Return. BoxTyped is a
                // no-op in uniform-NB (all values are NB), so we can
                // treat the pair as tail-position together.
                // Tail self-recursion → `return_call` (legal only under
                // the Tail conv chosen above when this is `Some`). See
                // `detect_uniform_nb_tail_self`.
                let (tail_args, trim_extra) = detect_uniform_nb_tail_self(rir, blk);
                if tail_args.is_some() {
                    // Drop the CallSelf and (if pattern (c)) the
                    // trailing BoxTyped.
                    let n = blk.insts.len();
                    let trim = 1 + trim_extra;
                    let truncated = cs_rir::Block {
                        id: blk.id,
                        params: blk.params.clone(),
                        insts: blk.insts[..n - trim].to_vec(),
                        terminator: blk.terminator.clone(),
                    };
                    lower_inst_uniform_nb(
                        &mut builder,
                        &mut value_map,
                        &mut raw_map,
                        &nb_helpers,
                        &truncated,
                        raw_abi,
                    )?;
                    let args = tail_args.unwrap();
                    // iter-3 raw ABI: a tail self-call passes raw Fixnum
                    // args (the gate verified each is raw), matching the
                    // inner's raw signature. Otherwise pass NB carriers.
                    let cargs: Vec<cranelift_codegen::ir::Value> = args
                        .iter()
                        .map(|a| lookup(if raw_abi { &raw_map } else { &value_map }, *a))
                        .collect::<Result<_, _>>()?;
                    builder.ins().return_call(nb_helpers.self_fn, &cargs);
                } else if let Some((callee, gargs)) = detect_tail_call_general(rir, blk) {
                    // ADR 0019 — tail-position Call / CallGeneral. Lower
                    // the block's other insts (which produce `callee` and
                    // `gargs` as owned NB handles), then stash the call
                    // via `vm_jit_set_tailcall` and `return` a
                    // placeholder. The dispatch trampoline re-runs the
                    // callee in constant stack instead of recursing
                    // through the IC. Ownership of callee + args transfers
                    // into the slot exactly as it would have transferred
                    // to vm_ic_dispatch / vm_call_general.
                    let n = blk.insts.len();
                    let truncated = cs_rir::Block {
                        id: blk.id,
                        params: blk.params.clone(),
                        insts: blk.insts[..n - 1].to_vec(),
                        terminator: blk.terminator.clone(),
                    };
                    lower_inst_uniform_nb(
                        &mut builder,
                        &mut value_map,
                        &mut raw_map,
                        &nb_helpers,
                        &truncated,
                        raw_abi,
                    )?;
                    let callee_v = lookup(&value_map, callee)?;
                    let arg_vs: Vec<cranelift_codegen::ir::Value> = gargs
                        .iter()
                        .map(|a| lookup(&value_map, *a))
                        .collect::<Result<_, _>>()?;
                    let n_args = gargs.len();
                    let buf_bytes = std::cmp::max(8u32, (n_args as u32) * 8);
                    let buf_slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        buf_bytes,
                        3,
                    ));
                    for (i, av) in arg_vs.iter().enumerate() {
                        let _ = builder.ins().stack_store(*av, buf_slot, (i as i32) * 8);
                    }
                    let buf_addr = builder.ins().stack_addr(I64, buf_slot, 0);
                    let n_args_v = builder.ins().iconst(I64, n_args as i64);
                    let call = builder
                        .ins()
                        .call(nb_helpers.set_tailcall, &[callee_v, buf_addr, n_args_v]);
                    let placeholder = builder.inst_results(call)[0];
                    builder.ins().return_(&[placeholder]);
                } else {
                    lower_inst_uniform_nb(
                        &mut builder,
                        &mut value_map,
                        &mut raw_map,
                        &nb_helpers,
                        blk,
                        raw_abi,
                    )?;
                    lower_terminator_uniform_nb(
                        &mut builder,
                        &value_map,
                        &raw_map,
                        &block_map,
                        &blk.terminator,
                        raw_abi,
                    )?;
                }
            }

            builder.finalize();
        }

        let mut ctx = cranelift_codegen::Context::for_function(clif);
        // Pre-codegen well-formedness gate (issue #16). For some
        // control-flow shapes the uniform-NB lowering emits a
        // function whose blocks never get linked into `func.layout`
        // (a Cranelift FunctionBuilder state bug — issue #4: blocks
        // and instructions land in the DFG but the layout stays
        // empty). Handing such a function to `define_function`
        // panics deep inside Cranelift's `remove_constant_phis`
        // ("entry block unknown"), which the runtime can only
        // contain with `catch_unwind` + a thread-wide JIT poison.
        // Detect the malformed shape here and return a clean
        // `JitError` instead: `jit_tier_up_hook` then declines this
        // body to the VM tier without a panic, and JIT stays
        // enabled for every other function on the thread.
        verify_clif_lowerable(&ctx.func)?;
        self.module
            .define_function(inner_id, &mut ctx)
            .map_err(|e| JitError::Codegen(format!("define_function inner uniform_nb: {e}")))?;
        Ok(())
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
            #[cfg(feature = "regions")]
            let alloc_pair_region_fnref = self
                .module
                .declare_func_in_func(self.alloc_pair_region_func, builder.func);
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
            // iter DZ — equal? deep equality.
            let equal_fnref = self
                .module
                .declare_func_in_func(self.equal_func, builder.func);
            let any_truthy_fnref = self
                .module
                .declare_func_in_func(self.any_truthy_func, builder.func);
            let call_general_fnref = self
                .module
                .declare_func_in_func(self.call_general_func, builder.func);
            // iter JI — IC hot-path dispatch (installs TLS guards).
            let ic_dispatch_fnref = self
                .module
                .declare_func_in_func(self.ic_dispatch_func, builder.func);
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
            // iter DW — ordered string comparisons.
            let string_lt_fnref = self
                .module
                .declare_func_in_func(self.string_lt_func, builder.func);
            let string_gt_fnref = self
                .module
                .declare_func_in_func(self.string_gt_func, builder.func);
            let string_le_fnref = self
                .module
                .declare_func_in_func(self.string_le_func, builder.func);
            let string_ge_fnref = self
                .module
                .declare_func_in_func(self.string_ge_func, builder.func);
            // iter DX — string-ci comparisons.
            let string_ci_eq_fnref = self
                .module
                .declare_func_in_func(self.string_ci_eq_func, builder.func);
            let string_ci_lt_fnref = self
                .module
                .declare_func_in_func(self.string_ci_lt_func, builder.func);
            let string_ci_gt_fnref = self
                .module
                .declare_func_in_func(self.string_ci_gt_func, builder.func);
            let string_ci_le_fnref = self
                .module
                .declare_func_in_func(self.string_ci_le_func, builder.func);
            let string_ci_ge_fnref = self
                .module
                .declare_func_in_func(self.string_ci_ge_func, builder.func);
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
            // iter CH — member / assoc.
            let member_fnref = self
                .module
                .declare_func_in_func(self.member_func, builder.func);
            let assoc_fnref = self
                .module
                .declare_func_in_func(self.assoc_func, builder.func);
            // iter CK — list-tail / list-ref.
            let list_tail_fnref = self
                .module
                .declare_func_in_func(self.list_tail_func, builder.func);
            let list_ref_fnref = self
                .module
                .declare_func_in_func(self.list_ref_func, builder.func);
            // iter CM — substring.
            let substring_fnref = self
                .module
                .declare_func_in_func(self.substring_func, builder.func);
            // iter CN — list-copy.
            let list_copy_fnref = self
                .module
                .declare_func_in_func(self.list_copy_func, builder.func);
            // iter CO — list-set!.
            let list_set_fnref = self
                .module
                .declare_func_in_func(self.list_set_func, builder.func);
            // iter CP — gcd / lcm.
            let gcd_fnref = self
                .module
                .declare_func_in_func(self.gcd_func, builder.func);
            let lcm_fnref = self
                .module
                .declare_func_in_func(self.lcm_func, builder.func);
            // iter CT — expt.
            let expt_fnref = self
                .module
                .declare_func_in_func(self.expt_func, builder.func);
            // iter DL — arithmetic-shift.
            let arith_shift_fnref = self
                .module
                .declare_func_in_func(self.arith_shift_func, builder.func);
            // iter CQ — bytevector read ops.
            let bv_p_fnref = self
                .module
                .declare_func_in_func(self.bv_p_func, builder.func);
            let bv_length_fnref = self
                .module
                .declare_func_in_func(self.bv_length_func, builder.func);
            let bv_u8_ref_fnref = self
                .module
                .declare_func_in_func(self.bv_u8_ref_func, builder.func);
            // iter CR — bytevector write ops.
            let bv_alloc_fnref = self
                .module
                .declare_func_in_func(self.bv_alloc_func, builder.func);
            let bv_u8_set_fnref = self
                .module
                .declare_func_in_func(self.bv_u8_set_func, builder.func);
            // iter CZ — bulk fill ops.
            let vec_fill_fnref = self
                .module
                .declare_func_in_func(self.vec_fill_func, builder.func);
            let bv_fill_fnref = self
                .module
                .declare_func_in_func(self.bv_fill_func, builder.func);
            // iter DA — string-set!.
            let str_set_fnref = self
                .module
                .declare_func_in_func(self.str_set_func, builder.func);
            // iter DH — string-fill!.
            let str_fill_fnref = self
                .module
                .declare_func_in_func(self.str_fill_func, builder.func);
            // iter DO — variadic vector.
            let make_vector_buf_fnref = self
                .module
                .declare_func_in_func(self.make_vector_buf_func, builder.func);
            // iter DP — variadic string.
            let make_string_buf_fnref = self
                .module
                .declare_func_in_func(self.make_string_buf_func, builder.func);
            // iter DQ — variadic bytevector.
            let make_bytevector_buf_fnref = self
                .module
                .declare_func_in_func(self.make_bytevector_buf_func, builder.func);
            // iter DR — variadic string-append.
            let string_append_buf_fnref = self
                .module
                .declare_func_in_func(self.string_append_buf_func, builder.func);
            // iter DS — variadic append (lists).
            let append_buf_fnref = self
                .module
                .declare_func_in_func(self.append_buf_func, builder.func);
            // iter DT — variadic vector-append.
            let vector_append_buf_fnref = self
                .module
                .declare_func_in_func(self.vector_append_buf_func, builder.func);
            // iter DU — variadic bytevector-append.
            let bytevector_append_buf_fnref = self
                .module
                .declare_func_in_func(self.bytevector_append_buf_func, builder.func);
            // iter DB — string-copy / vector-copy.
            let str_copy_fnref = self
                .module
                .declare_func_in_func(self.str_copy_func, builder.func);
            let vec_copy_fnref = self
                .module
                .declare_func_in_func(self.vec_copy_func, builder.func);
            // iter DC — bytevector-copy.
            let bv_copy_fnref = self
                .module
                .declare_func_in_func(self.bv_copy_func, builder.func);
            // iter DD — type predicates on Any.
            let procedure_p_fnref = self
                .module
                .declare_func_in_func(self.procedure_p_func, builder.func);
            let port_p_fnref = self
                .module
                .declare_func_in_func(self.port_p_func, builder.func);
            let eof_p_fnref = self
                .module
                .declare_func_in_func(self.eof_p_func, builder.func);
            let symbol_p_fnref = self
                .module
                .declare_func_in_func(self.symbol_p_func, builder.func);
            // iter DE — more type predicates on Any.
            let char_p_fnref = self
                .module
                .declare_func_in_func(self.char_p_func, builder.func);
            let boolean_p_fnref = self
                .module
                .declare_func_in_func(self.boolean_p_func, builder.func);
            let fixnum_p_fnref = self
                .module
                .declare_func_in_func(self.fixnum_p_func, builder.func);
            let flonum_p_fnref = self
                .module
                .declare_func_in_func(self.flonum_p_func, builder.func);
            // iter DF — flonum transcendentals.
            let flonum_sin_fnref = self
                .module
                .declare_func_in_func(self.flonum_sin_func, builder.func);
            // iter EH — Flonum integer? helper.
            let flonum_is_integer_fnref = self
                .module
                .declare_func_in_func(self.flonum_is_integer_func, builder.func);
            let flonum_cos_fnref = self
                .module
                .declare_func_in_func(self.flonum_cos_func, builder.func);
            let flonum_tan_fnref = self
                .module
                .declare_func_in_func(self.flonum_tan_func, builder.func);
            let flonum_log_fnref = self
                .module
                .declare_func_in_func(self.flonum_log_func, builder.func);
            let flonum_exp_fnref = self
                .module
                .declare_func_in_func(self.flonum_exp_func, builder.func);
            // iter DG — inverse trig.
            let flonum_asin_fnref = self
                .module
                .declare_func_in_func(self.flonum_asin_func, builder.func);
            let flonum_acos_fnref = self
                .module
                .declare_func_in_func(self.flonum_acos_func, builder.func);
            let flonum_atan_fnref = self
                .module
                .declare_func_in_func(self.flonum_atan_func, builder.func);
            // iter FM — log/atan 2-arg.
            let flonum_log2_fnref = self
                .module
                .declare_func_in_func(self.flonum_log2_func, builder.func);
            let flonum_atan2_fnref = self
                .module
                .declare_func_in_func(self.flonum_atan2_func, builder.func);
            // iter GA — flexpt, fleven?, flodd?.
            let flonum_expt_fnref = self
                .module
                .declare_func_in_func(self.flonum_expt_func, builder.func);
            let fl_even_p_fnref = self
                .module
                .declare_func_in_func(self.fl_even_p_func, builder.func);
            let fl_odd_p_fnref = self
                .module
                .declare_func_in_func(self.fl_odd_p_func, builder.func);
            // iter GB — string-titlecase / string-hash / symbol-hash.
            let string_titlecase_fnref = self
                .module
                .declare_func_in_func(self.string_titlecase_func, builder.func);
            let string_hash_fnref = self
                .module
                .declare_func_in_func(self.string_hash_func, builder.func);
            let symbol_hash_fnref = self
                .module
                .declare_func_in_func(self.symbol_hash_func, builder.func);
            // iter GC — port-subtype predicates.
            let input_port_p_fnref = self
                .module
                .declare_func_in_func(self.input_port_p_func, builder.func);
            let output_port_p_fnref = self
                .module
                .declare_func_in_func(self.output_port_p_func, builder.func);
            let binary_port_p_fnref = self
                .module
                .declare_func_in_func(self.binary_port_p_func, builder.func);
            let textual_port_p_fnref = self
                .module
                .declare_func_in_func(self.textual_port_p_func, builder.func);
            // iter GP — output-port-open?.
            let output_port_open_p_fnref = self
                .module
                .declare_func_in_func(self.output_port_open_p_func, builder.func);
            // iter GQ — port-eof? + port-has-set-port-position!?.
            let port_eof_p_fnref = self
                .module
                .declare_func_in_func(self.port_eof_p_func, builder.func);
            let port_has_set_port_position_p_fnref = self
                .module
                .declare_func_in_func(self.port_has_set_port_position_p_func, builder.func);
            // iter GR — port-position.
            let port_position_fnref = self
                .module
                .declare_func_in_func(self.port_position_func, builder.func);
            // iter GD — promise?.
            let promise_p_fnref = self
                .module
                .declare_func_in_func(self.promise_p_func, builder.func);
            // iter GE — div/mod (Euclidean).
            let div_euclid_fnref = self
                .module
                .declare_func_in_func(self.div_euclid_func, builder.func);
            let mod_euclid_fnref = self
                .module
                .declare_func_in_func(self.mod_euclid_func, builder.func);
            // iter HO — div0/mod0 (centered).
            let div0_fnref = self
                .module
                .declare_func_in_func(self.div0_func, builder.func);
            let mod0_fnref = self
                .module
                .declare_func_in_func(self.mod0_func, builder.func);
            // iter HQ — hashtable-hash-function.
            let hashtable_hash_function_fnref = self
                .module
                .declare_func_in_func(self.hashtable_hash_function_func, builder.func);
            // iter HR — make-hashtable 0-arg.
            let make_hashtable_equal_fnref = self
                .module
                .declare_func_in_func(self.make_hashtable_equal_func, builder.func);
            // iter HS — make-eq/eqv-hashtable.
            let make_hashtable_eq_fnref = self
                .module
                .declare_func_in_func(self.make_hashtable_eq_func, builder.func);
            let make_hashtable_eqv_fnref = self
                .module
                .declare_func_in_func(self.make_hashtable_eqv_func, builder.func);
            // iter GF — hashtable?.
            let hashtable_p_fnref = self
                .module
                .declare_func_in_func(self.hashtable_p_func, builder.func);
            // iter GG — hashtable-size / hashtable-mutable?.
            let hashtable_size_fnref = self
                .module
                .declare_func_in_func(self.hashtable_size_func, builder.func);
            let hashtable_mutable_p_fnref = self
                .module
                .declare_func_in_func(self.hashtable_mutable_p_func, builder.func);
            // iter GH — hashtable-keys / hashtable-values.
            let hashtable_keys_fnref = self
                .module
                .declare_func_in_func(self.hashtable_keys_func, builder.func);
            let hashtable_values_fnref = self
                .module
                .declare_func_in_func(self.hashtable_values_func, builder.func);
            // iter GI — hashtable-clear!.
            let hashtable_clear_fnref = self
                .module
                .declare_func_in_func(self.hashtable_clear_func, builder.func);
            // iter GJ — equal-hash + hashtable->alist.
            let equal_hash_fnref = self
                .module
                .declare_func_in_func(self.equal_hash_func, builder.func);
            let hashtable_to_alist_fnref = self
                .module
                .declare_func_in_func(self.hashtable_to_alist_func, builder.func);
            // iter GK — file-exists?.
            let file_exists_p_fnref = self
                .module
                .declare_func_in_func(self.file_exists_p_func, builder.func);
            // iter GL — current-second / current-jiffy.
            let current_second_fnref = self
                .module
                .declare_func_in_func(self.current_second_func, builder.func);
            let current_jiffy_fnref = self
                .module
                .declare_func_in_func(self.current_jiffy_func, builder.func);
            // iter GN — append-reverse.
            let append_reverse_fnref = self
                .module
                .declare_func_in_func(self.append_reverse_func, builder.func);
            // iter GO — alist-copy.
            let alist_copy_fnref = self
                .module
                .declare_func_in_func(self.alist_copy_func, builder.func);
            // iter GS — delete + delete-duplicates.
            let delete_fnref = self
                .module
                .declare_func_in_func(self.delete_func, builder.func);
            let delete_duplicates_fnref = self
                .module
                .declare_func_in_func(self.delete_duplicates_func, builder.func);
            // iter GT — make-promise.
            let make_promise_fnref = self
                .module
                .declare_func_in_func(self.make_promise_func, builder.func);
            // iter GU — force fast-path.
            let force_forced_fnref = self
                .module
                .declare_func_in_func(self.force_forced_func, builder.func);
            // iter GV — hashtable-contains?.
            let hashtable_contains_p_fnref = self
                .module
                .declare_func_in_func(self.hashtable_contains_p_func, builder.func);
            // iter GW — hashtable-delete!.
            let hashtable_delete_fnref = self
                .module
                .declare_func_in_func(self.hashtable_delete_func, builder.func);
            // iter GX — hashtable-set!.
            let hashtable_set_fnref = self
                .module
                .declare_func_in_func(self.hashtable_set_func, builder.func);
            // iter GY — hashtable-ref.
            let hashtable_ref_fnref = self
                .module
                .declare_func_in_func(self.hashtable_ref_func, builder.func);
            // iter GZ — hashtable-copy.
            let hashtable_copy_fnref = self
                .module
                .declare_func_in_func(self.hashtable_copy_func, builder.func);
            // iter HA — vector-copy 3-arg slice.
            let vector_copy_slice_fnref = self
                .module
                .declare_func_in_func(self.vector_copy_slice_func, builder.func);
            // iter HT — vector-copy 2-arg slice-to-end.
            let vector_copy_from_fnref = self
                .module
                .declare_func_in_func(self.vector_copy_from_func, builder.func);
            // iter HU — bytevector-copy 2-arg slice-to-end.
            let bytevector_copy_from_fnref = self
                .module
                .declare_func_in_func(self.bytevector_copy_from_func, builder.func);
            // iter HV — string-copy 2-arg slice-to-end.
            let string_copy_from_fnref = self
                .module
                .declare_func_in_func(self.string_copy_from_func, builder.func);
            // iter IA — bytevector-fill! 3-arg fill-from.
            let bytevector_fill_from_fnref = self
                .module
                .declare_func_in_func(self.bytevector_fill_from_func, builder.func);
            // iter IB — vector-fill! 3-arg fill-from.
            let vector_fill_from_fnref = self
                .module
                .declare_func_in_func(self.vector_fill_from_func, builder.func);
            // iter IC — string-fill! 3-arg fill-from.
            let string_fill_from_fnref = self
                .module
                .declare_func_in_func(self.string_fill_from_func, builder.func);
            // iter ID — vector->string 3-arg slice.
            let vector_to_string_slice_fnref = self
                .module
                .declare_func_in_func(self.vector_to_string_slice_func, builder.func);
            // iter IE — string->vector 3-arg slice.
            let string_to_vector_slice_fnref = self
                .module
                .declare_func_in_func(self.string_to_vector_slice_func, builder.func);
            // iter IF — vector->list 3-arg slice.
            let vector_to_list_slice_fnref = self
                .module
                .declare_func_in_func(self.vector_to_list_slice_func, builder.func);
            // iter IG — string->list 3-arg slice.
            let string_to_list_slice_fnref = self
                .module
                .declare_func_in_func(self.string_to_list_slice_func, builder.func);
            // iter IH — bytevector->list 3-arg slice.
            let bytevector_to_list_slice_fnref = self
                .module
                .declare_func_in_func(self.bytevector_to_list_slice_func, builder.func);
            // iter HC — bytevector-copy 3-arg slice.
            let bytevector_copy_slice_fnref = self
                .module
                .declare_func_in_func(self.bytevector_copy_slice_func, builder.func);
            // iter HD — eof-object constructor.
            let eof_object_fnref = self
                .module
                .declare_func_in_func(self.eof_object_func, builder.func);
            // iter FN — bitwise-bit-count / -length.
            let bitwise_bit_count_fnref = self
                .module
                .declare_func_in_func(self.bitwise_bit_count_func, builder.func);
            let bitwise_length_fnref = self
                .module
                .declare_func_in_func(self.bitwise_length_func, builder.func);
            // iter FO — bitwise shift-left/-right + bit-set?.
            let bitwise_arith_shift_left_fnref = self
                .module
                .declare_func_in_func(self.bitwise_arith_shift_left_func, builder.func);
            let bitwise_arith_shift_right_fnref = self
                .module
                .declare_func_in_func(self.bitwise_arith_shift_right_func, builder.func);
            let bitwise_bit_set_p_fnref = self
                .module
                .declare_func_in_func(self.bitwise_bit_set_p_func, builder.func);
            // iter FP — bytevector-s8-ref/-set!.
            let bytevector_s8_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s8_ref_func, builder.func);
            let bytevector_s8_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s8_set_func, builder.func);
            // iter FQ — bytevector u16/s16 native-ref/-set!.
            let bytevector_u16_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_u16_native_ref_func, builder.func);
            let bytevector_s16_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s16_native_ref_func, builder.func);
            let bytevector_u16_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_u16_native_set_func, builder.func);
            let bytevector_s16_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s16_native_set_func, builder.func);
            // iter FR — bytevector u32/s32 native-ref/-set!.
            let bytevector_u32_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_u32_native_ref_func, builder.func);
            let bytevector_s32_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s32_native_ref_func, builder.func);
            let bytevector_u32_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_u32_native_set_func, builder.func);
            let bytevector_s32_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s32_native_set_func, builder.func);
            // iter FS — bytevector IEEE float native-ref/-set!.
            let bytevector_ieee_single_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_ieee_single_native_ref_func, builder.func);
            let bytevector_ieee_double_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_ieee_double_native_ref_func, builder.func);
            let bytevector_ieee_single_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_ieee_single_native_set_func, builder.func);
            let bytevector_ieee_double_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_ieee_double_native_set_func, builder.func);
            // iter FT — bytevector u64/s64 native-ref/-set!.
            let bytevector_u64_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_u64_native_ref_func, builder.func);
            let bytevector_s64_native_ref_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s64_native_ref_func, builder.func);
            let bytevector_u64_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_u64_native_set_func, builder.func);
            let bytevector_s64_native_set_fnref = self
                .module
                .declare_func_in_func(self.bytevector_s64_native_set_func, builder.func);
            // iter FX — fxfirst-bit-set.
            let fx_first_bit_set_fnref = self
                .module
                .declare_func_in_func(self.fx_first_bit_set_func, builder.func);
            // iter CI — char predicates.
            let char_alphabetic_p_fnref = self
                .module
                .declare_func_in_func(self.char_alphabetic_p_func, builder.func);
            let char_numeric_p_fnref = self
                .module
                .declare_func_in_func(self.char_numeric_p_func, builder.func);
            let char_whitespace_p_fnref = self
                .module
                .declare_func_in_func(self.char_whitespace_p_func, builder.func);
            // iter CJ — char case ops.
            let char_upcase_fnref = self
                .module
                .declare_func_in_func(self.char_upcase_func, builder.func);
            let char_downcase_fnref = self
                .module
                .declare_func_in_func(self.char_downcase_func, builder.func);
            let char_upper_case_p_fnref = self
                .module
                .declare_func_in_func(self.char_upper_case_p_func, builder.func);
            let char_lower_case_p_fnref = self
                .module
                .declare_func_in_func(self.char_lower_case_p_func, builder.func);
            // iter CS — char-foldcase / char-titlecase.
            let char_foldcase_fnref = self
                .module
                .declare_func_in_func(self.char_foldcase_func, builder.func);
            let char_titlecase_fnref = self
                .module
                .declare_func_in_func(self.char_titlecase_func, builder.func);
            // iter CV — digit-value.
            let digit_value_fnref = self
                .module
                .declare_func_in_func(self.digit_value_func, builder.func);
            // iter CW — vector->list / list->vector.
            let vector_to_list_fnref = self
                .module
                .declare_func_in_func(self.vector_to_list_func, builder.func);
            let list_to_vector_fnref = self
                .module
                .declare_func_in_func(self.list_to_vector_func, builder.func);
            // iter CX — string<->list.
            let string_to_list_fnref = self
                .module
                .declare_func_in_func(self.string_to_list_func, builder.func);
            let list_to_string_fnref = self
                .module
                .declare_func_in_func(self.list_to_string_func, builder.func);
            // iter DY — string<->vector.
            let string_to_vector_fnref = self
                .module
                .declare_func_in_func(self.string_to_vector_func, builder.func);
            let vector_to_string_fnref = self
                .module
                .declare_func_in_func(self.vector_to_string_func, builder.func);
            // iter EC — number<->string.
            let number_to_string_fnref = self
                .module
                .declare_func_in_func(self.number_to_string_func, builder.func);
            // iter II — number->string 2-arg radix.
            let number_to_string_radix_fnref = self
                .module
                .declare_func_in_func(self.number_to_string_radix_func, builder.func);
            let string_to_number_fnref = self
                .module
                .declare_func_in_func(self.string_to_number_func, builder.func);
            // iter IJ — string->number 2-arg radix.
            let string_to_number_radix_fnref = self
                .module
                .declare_func_in_func(self.string_to_number_radix_func, builder.func);
            // iter IK — make-list 1-arg.
            let make_list_unspec_fnref = self
                .module
                .declare_func_in_func(self.make_list_unspec_func, builder.func);
            // iter JE — make-vector 1-arg.
            let make_vector_unspec_fnref = self
                .module
                .declare_func_in_func(self.make_vector_unspec_func, builder.func);
            // iter IL — vector->list 2-arg slice-from.
            let vector_to_list_slice_from_fnref = self
                .module
                .declare_func_in_func(self.vector_to_list_slice_from_func, builder.func);
            // iter IM — string->list 2-arg slice-from.
            let string_to_list_slice_from_fnref = self
                .module
                .declare_func_in_func(self.string_to_list_slice_from_func, builder.func);
            // iter IN — bytevector->list 2-arg slice-from.
            let bytevector_to_list_slice_from_fnref = self
                .module
                .declare_func_in_func(self.bytevector_to_list_slice_from_func, builder.func);
            // iter IO — vector->string 2-arg slice-from.
            let vector_to_string_slice_from_fnref = self
                .module
                .declare_func_in_func(self.vector_to_string_slice_from_func, builder.func);
            // iter IP — string->vector 2-arg slice-from.
            let string_to_vector_slice_from_fnref = self
                .module
                .declare_func_in_func(self.string_to_vector_slice_from_func, builder.func);
            // iter IQ — vector-copy! 4-arg.
            let vector_copy_bang_from_fnref = self
                .module
                .declare_func_in_func(self.vector_copy_bang_from_func, builder.func);
            // iter IR — bytevector-copy! 4-arg.
            let bytevector_copy_bang_from_fnref = self
                .module
                .declare_func_in_func(self.bytevector_copy_bang_from_func, builder.func);
            // iter IS — string-copy! 4-arg.
            let string_copy_bang_from_fnref = self
                .module
                .declare_func_in_func(self.string_copy_bang_from_func, builder.func);
            // iter IT — vector-copy! 5-arg.
            let vector_copy_bang_slice_fnref = self
                .module
                .declare_func_in_func(self.vector_copy_bang_slice_func, builder.func);
            // iter IU — bytevector-copy! 5-arg.
            let bytevector_copy_bang_slice_fnref = self
                .module
                .declare_func_in_func(self.bytevector_copy_bang_slice_func, builder.func);
            // iter IV — string-copy! 5-arg.
            let string_copy_bang_slice_fnref = self
                .module
                .declare_func_in_func(self.string_copy_bang_slice_func, builder.func);
            // iter EJ — string-reverse.
            let string_reverse_fnref = self
                .module
                .declare_func_in_func(self.string_reverse_func, builder.func);
            // iter ET — string case conversion.
            let string_upcase_fnref = self
                .module
                .declare_func_in_func(self.string_upcase_func, builder.func);
            let string_downcase_fnref = self
                .module
                .declare_func_in_func(self.string_downcase_func, builder.func);
            let string_foldcase_fnref = self
                .module
                .declare_func_in_func(self.string_foldcase_func, builder.func);
            // iter EU — string-contains.
            let string_contains_fnref = self
                .module
                .declare_func_in_func(self.string_contains_func, builder.func);
            // iter EV — string-prefix?/suffix?.
            let string_prefix_p_fnref = self
                .module
                .declare_func_in_func(self.string_prefix_p_func, builder.func);
            let string_suffix_p_fnref = self
                .module
                .declare_func_in_func(self.string_suffix_p_func, builder.func);
            // iter FE — string-join.
            let string_join_fnref = self
                .module
                .declare_func_in_func(self.string_join_func, builder.func);
            // iter FF — string-split.
            let string_split_fnref = self
                .module
                .declare_func_in_func(self.string_split_func, builder.func);
            // iter FG — string-pad / string-pad-right.
            let string_pad_fnref = self
                .module
                .declare_func_in_func(self.string_pad_func, builder.func);
            let string_pad_right_fnref = self
                .module
                .declare_func_in_func(self.string_pad_right_func, builder.func);
            // iter FH — string trim family.
            let string_trim_fnref = self
                .module
                .declare_func_in_func(self.string_trim_func, builder.func);
            let string_trim_left_fnref = self
                .module
                .declare_func_in_func(self.string_trim_left_func, builder.func);
            let string_trim_right_fnref = self
                .module
                .declare_func_in_func(self.string_trim_right_func, builder.func);
            // iter FI — string-replace-all.
            let string_replace_all_fnref = self
                .module
                .declare_func_in_func(self.string_replace_all_func, builder.func);
            // iter HE — string-replace (first occurrence).
            let string_replace_first_fnref = self
                .module
                .declare_func_in_func(self.string_replace_first_func, builder.func);
            // iter HF — bytevector-fill! 4-arg slice.
            let bytevector_fill_slice_fnref = self
                .module
                .declare_func_in_func(self.bytevector_fill_slice_func, builder.func);
            // iter HG — vector-fill! 4-arg slice.
            let vector_fill_slice_fnref = self
                .module
                .declare_func_in_func(self.vector_fill_slice_func, builder.func);
            // iter HH — string-fill! 4-arg slice.
            let string_fill_slice_fnref = self
                .module
                .declare_func_in_func(self.string_fill_slice_func, builder.func);
            // iter HI — exact-nonnegative-integer?.
            let exact_nonneg_int_p_fnref = self
                .module
                .declare_func_in_func(self.exact_nonneg_int_p_func, builder.func);
            // iter HJ — bytevector=?.
            let bytevector_eq_p_fnref = self
                .module
                .declare_func_in_func(self.bytevector_eq_p_func, builder.func);
            // iter HK — vector=?.
            let vector_eq_p_fnref = self
                .module
                .declare_func_in_func(self.vector_eq_p_func, builder.func);
            // iter FJ — string-take/-drop/-take-right/-drop-right.
            let string_take_fnref = self
                .module
                .declare_func_in_func(self.string_take_func, builder.func);
            let string_drop_fnref = self
                .module
                .declare_func_in_func(self.string_drop_func, builder.func);
            let string_take_right_fnref = self
                .module
                .declare_func_in_func(self.string_take_right_func, builder.func);
            let string_drop_right_fnref = self
                .module
                .declare_func_in_func(self.string_drop_right_func, builder.func);
            // iter FK — string-contains-right / string-index / -index-right.
            let string_contains_right_fnref = self
                .module
                .declare_func_in_func(self.string_contains_right_func, builder.func);
            let string_index_fnref = self
                .module
                .declare_func_in_func(self.string_index_func, builder.func);
            let string_index_right_fnref = self
                .module
                .declare_func_in_func(self.string_index_right_func, builder.func);
            // iter FL — bytevector/utf8 conversion.
            let bytevector_to_u8_list_fnref = self
                .module
                .declare_func_in_func(self.bytevector_to_u8_list_func, builder.func);
            let u8_list_to_bytevector_fnref = self
                .module
                .declare_func_in_func(self.u8_list_to_bytevector_func, builder.func);
            let string_to_utf8_fnref = self
                .module
                .declare_func_in_func(self.string_to_utf8_func, builder.func);
            let utf8_to_string_fnref = self
                .module
                .declare_func_in_func(self.utf8_to_string_func, builder.func);
            // iter EM — make-list 2-arg.
            let make_list_fill_fnref = self
                .module
                .declare_func_in_func(self.make_list_fill_func, builder.func);
            // iter EN — iota 1-arg.
            let iota_n_fnref = self
                .module
                .declare_func_in_func(self.iota_n_func, builder.func);
            // iter FC — iota 2-arg.
            let iota_ns_fnref = self
                .module
                .declare_func_in_func(self.iota_ns_func, builder.func);
            // iter FD — iota 3-arg.
            let iota_nss_fnref = self
                .module
                .declare_func_in_func(self.iota_nss_func, builder.func);
            // iter EO — last-pair / last.
            let last_pair_fnref = self
                .module
                .declare_func_in_func(self.last_pair_func, builder.func);
            let last_fnref = self
                .module
                .declare_func_in_func(self.last_func, builder.func);
            // iter EX — take / drop.
            let take_fnref = self
                .module
                .declare_func_in_func(self.take_func, builder.func);
            let drop_fnref = self
                .module
                .declare_func_in_func(self.drop_func, builder.func);
            // iter FB — concatenate / not-pair?.
            let concatenate_fnref = self
                .module
                .declare_func_in_func(self.concatenate_func, builder.func);
            let not_pair_p_fnref = self
                .module
                .declare_func_in_func(self.not_pair_p_func, builder.func);
            // iter EY — SRFI-1 list classifiers.
            let null_list_p_fnref = self
                .module
                .declare_func_in_func(self.null_list_p_func, builder.func);
            let proper_list_p_fnref = self
                .module
                .declare_func_in_func(self.proper_list_p_func, builder.func);
            let dotted_list_p_fnref = self
                .module
                .declare_func_in_func(self.dotted_list_p_func, builder.func);
            let circular_list_p_fnref = self
                .module
                .declare_func_in_func(self.circular_list_p_func, builder.func);
            // iter ER — vector-copy!.
            let vector_copy_bang_fnref = self
                .module
                .declare_func_in_func(self.vector_copy_bang_func, builder.func);
            // iter ES — bytevector-copy! / string-copy!.
            let bytevector_copy_bang_fnref = self
                .module
                .declare_func_in_func(self.bytevector_copy_bang_func, builder.func);
            let string_copy_bang_fnref = self
                .module
                .declare_func_in_func(self.string_copy_bang_func, builder.func);
            // iter CY — symbol<->string.
            let symbol_to_string_fnref = self
                .module
                .declare_func_in_func(self.symbol_to_string_func, builder.func);
            let string_to_symbol_fnref = self
                .module
                .declare_func_in_func(self.string_to_symbol_func, builder.func);

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
                        #[cfg(feature = "regions")]
                        alloc_pair_region_fnref,
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
                        equal_fnref,
                        any_truthy_fnref,
                        call_general_fnref,
                        ic_dispatch_fnref,
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
                        string_lt_fnref,
                        string_gt_fnref,
                        string_le_fnref,
                        string_ge_fnref,
                        string_ci_eq_fnref,
                        string_ci_lt_fnref,
                        string_ci_gt_fnref,
                        string_ci_le_fnref,
                        string_ci_ge_fnref,
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
                        member_fnref,
                        assoc_fnref,
                        list_tail_fnref,
                        list_ref_fnref,
                        substring_fnref,
                        list_copy_fnref,
                        list_set_fnref,
                        gcd_fnref,
                        lcm_fnref,
                        expt_fnref,
                        arith_shift_fnref,
                        bv_p_fnref,
                        bv_length_fnref,
                        bv_u8_ref_fnref,
                        bv_alloc_fnref,
                        bv_u8_set_fnref,
                        vec_fill_fnref,
                        bv_fill_fnref,
                        str_set_fnref,
                        str_fill_fnref,
                        make_vector_buf_fnref,
                        make_string_buf_fnref,
                        make_bytevector_buf_fnref,
                        string_append_buf_fnref,
                        append_buf_fnref,
                        vector_append_buf_fnref,
                        bytevector_append_buf_fnref,
                        str_copy_fnref,
                        vec_copy_fnref,
                        bv_copy_fnref,
                        procedure_p_fnref,
                        port_p_fnref,
                        eof_p_fnref,
                        symbol_p_fnref,
                        char_p_fnref,
                        boolean_p_fnref,
                        fixnum_p_fnref,
                        flonum_p_fnref,
                        flonum_sin_fnref,
                        flonum_is_integer_fnref,
                        flonum_cos_fnref,
                        flonum_tan_fnref,
                        flonum_log_fnref,
                        flonum_exp_fnref,
                        flonum_asin_fnref,
                        flonum_acos_fnref,
                        flonum_atan_fnref,
                        flonum_log2_fnref,
                        flonum_atan2_fnref,
                        flonum_expt_fnref,
                        fl_even_p_fnref,
                        fl_odd_p_fnref,
                        string_titlecase_fnref,
                        string_hash_fnref,
                        symbol_hash_fnref,
                        input_port_p_fnref,
                        output_port_p_fnref,
                        binary_port_p_fnref,
                        textual_port_p_fnref,
                        output_port_open_p_fnref,
                        port_eof_p_fnref,
                        port_has_set_port_position_p_fnref,
                        port_position_fnref,
                        promise_p_fnref,
                        div_euclid_fnref,
                        mod_euclid_fnref,
                        div0_fnref,
                        mod0_fnref,
                        hashtable_hash_function_fnref,
                        make_hashtable_equal_fnref,
                        make_hashtable_eq_fnref,
                        make_hashtable_eqv_fnref,
                        hashtable_p_fnref,
                        hashtable_size_fnref,
                        hashtable_mutable_p_fnref,
                        hashtable_keys_fnref,
                        hashtable_values_fnref,
                        hashtable_clear_fnref,
                        equal_hash_fnref,
                        hashtable_to_alist_fnref,
                        file_exists_p_fnref,
                        current_second_fnref,
                        current_jiffy_fnref,
                        append_reverse_fnref,
                        alist_copy_fnref,
                        delete_fnref,
                        delete_duplicates_fnref,
                        make_promise_fnref,
                        force_forced_fnref,
                        hashtable_contains_p_fnref,
                        hashtable_delete_fnref,
                        hashtable_set_fnref,
                        hashtable_ref_fnref,
                        hashtable_copy_fnref,
                        vector_copy_slice_fnref,
                        vector_copy_from_fnref,
                        bytevector_copy_from_fnref,
                        string_copy_from_fnref,
                        bytevector_fill_from_fnref,
                        vector_fill_from_fnref,
                        string_fill_from_fnref,
                        vector_to_string_slice_fnref,
                        string_to_vector_slice_fnref,
                        vector_to_list_slice_fnref,
                        string_to_list_slice_fnref,
                        bytevector_to_list_slice_fnref,
                        bytevector_copy_slice_fnref,
                        eof_object_fnref,
                        bitwise_bit_count_fnref,
                        bitwise_length_fnref,
                        bitwise_arith_shift_left_fnref,
                        bitwise_arith_shift_right_fnref,
                        bitwise_bit_set_p_fnref,
                        bytevector_s8_ref_fnref,
                        bytevector_s8_set_fnref,
                        bytevector_u16_native_ref_fnref,
                        bytevector_s16_native_ref_fnref,
                        bytevector_u16_native_set_fnref,
                        bytevector_s16_native_set_fnref,
                        bytevector_u32_native_ref_fnref,
                        bytevector_s32_native_ref_fnref,
                        bytevector_u32_native_set_fnref,
                        bytevector_s32_native_set_fnref,
                        bytevector_ieee_single_native_ref_fnref,
                        bytevector_ieee_double_native_ref_fnref,
                        bytevector_ieee_single_native_set_fnref,
                        bytevector_ieee_double_native_set_fnref,
                        bytevector_u64_native_ref_fnref,
                        bytevector_s64_native_ref_fnref,
                        bytevector_u64_native_set_fnref,
                        bytevector_s64_native_set_fnref,
                        fx_first_bit_set_fnref,
                        char_alphabetic_p_fnref,
                        char_numeric_p_fnref,
                        char_whitespace_p_fnref,
                        char_upcase_fnref,
                        char_downcase_fnref,
                        char_upper_case_p_fnref,
                        char_lower_case_p_fnref,
                        char_foldcase_fnref,
                        char_titlecase_fnref,
                        digit_value_fnref,
                        vector_to_list_fnref,
                        list_to_vector_fnref,
                        string_to_list_fnref,
                        list_to_string_fnref,
                        string_to_vector_fnref,
                        vector_to_string_fnref,
                        number_to_string_fnref,
                        number_to_string_radix_fnref,
                        string_to_number_fnref,
                        string_to_number_radix_fnref,
                        make_list_unspec_fnref,
                        make_vector_unspec_fnref,
                        vector_to_list_slice_from_fnref,
                        string_to_list_slice_from_fnref,
                        bytevector_to_list_slice_from_fnref,
                        vector_to_string_slice_from_fnref,
                        string_to_vector_slice_from_fnref,
                        vector_copy_bang_from_fnref,
                        bytevector_copy_bang_from_fnref,
                        string_copy_bang_from_fnref,
                        vector_copy_bang_slice_fnref,
                        bytevector_copy_bang_slice_fnref,
                        string_copy_bang_slice_fnref,
                        string_reverse_fnref,
                        string_upcase_fnref,
                        string_downcase_fnref,
                        string_foldcase_fnref,
                        string_contains_fnref,
                        string_prefix_p_fnref,
                        string_suffix_p_fnref,
                        string_join_fnref,
                        string_split_fnref,
                        string_pad_fnref,
                        string_pad_right_fnref,
                        string_trim_fnref,
                        string_trim_left_fnref,
                        string_trim_right_fnref,
                        string_replace_all_fnref,
                        string_replace_first_fnref,
                        bytevector_fill_slice_fnref,
                        vector_fill_slice_fnref,
                        string_fill_slice_fnref,
                        exact_nonneg_int_p_fnref,
                        bytevector_eq_p_fnref,
                        vector_eq_p_fnref,
                        string_take_fnref,
                        string_drop_fnref,
                        string_take_right_fnref,
                        string_drop_right_fnref,
                        string_contains_right_fnref,
                        string_index_fnref,
                        string_index_right_fnref,
                        bytevector_to_u8_list_fnref,
                        u8_list_to_bytevector_fnref,
                        string_to_utf8_fnref,
                        utf8_to_string_fnref,
                        make_list_fill_fnref,
                        iota_n_fnref,
                        iota_ns_fnref,
                        iota_nss_fnref,
                        last_pair_fnref,
                        last_fnref,
                        take_fnref,
                        drop_fnref,
                        null_list_p_fnref,
                        proper_list_p_fnref,
                        dotted_list_p_fnref,
                        circular_list_p_fnref,
                        concatenate_fnref,
                        not_pair_p_fnref,
                        vector_copy_bang_fnref,
                        bytevector_copy_bang_fnref,
                        string_copy_bang_fnref,
                        symbol_to_string_fnref,
                        string_to_symbol_fnref,
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
        raw_abi: bool,
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
            // ADR 0020 Strategy C iter-3 — raw-Fixnum self-call ABI
            // perimeter. The trampoline speaks the NB ABI to its callers
            // (the dispatcher / IC), but `inner` consumes/produces raw
            // sign-extended Fixnums. The dispatcher (`run_jit_body_once`)
            // already validated each arg's Fixnum tag before this call,
            // so the unbox is safe; the raw result is re-boxed to NB.
            let call_args: Vec<cranelift_codegen::ir::Value> = if raw_abi {
                args_v
                    .iter()
                    .map(|&a| unbox_nb_fixnum(&mut builder, a))
                    .collect()
            } else {
                args_v
            };
            let inst = builder.ins().call(inner_fnref, &call_args);
            let results = builder.inst_results(inst).to_vec();
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "trampoline expected 1 result, got {}",
                    results.len()
                )));
            }
            let ret = if raw_abi {
                box_raw_fixnum(&mut builder, results[0])
            } else {
                results[0]
            };
            builder.ins().return_(&[ret]);
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
    detect_tail_call_self_inner(rir, block)
}

/// Tail-position `Call` / `CallGeneral` detection (ADR 0019). Same
/// structural shape as [`detect_tail_call_self`] — the block's last
/// inst's `dst` is returned directly (`Term::Return(dst)`) or through a
/// trivial join (`Term::Jump(t, [dst])` where `t` only returns its
/// param) — but for a *dynamic* callee. Returns `(callee, args)` so the
/// caller can emit a `vm_jit_set_tailcall(callee, args, n)` bounce plus
/// `return`, letting the dispatch trampoline run the callee in constant
/// stack instead of recursing through the IC.
fn detect_tail_call_general<'a>(
    rir: &'a RirFunction,
    block: &'a cs_rir::Block,
) -> Option<(RirValue, &'a [RirValue])> {
    let last = block.insts.last()?;
    let (dst, callee, args) = match last {
        Inst::Call(dst, callee, args) | Inst::CallGeneral(dst, callee, args) => {
            (dst, *callee, args.as_slice())
        }
        _ => return None,
    };
    match &block.terminator {
        Term::Return(ret_v) if ret_v == dst => Some((callee, args)),
        Term::Jump(target, jump_args) if jump_args.len() == 1 && jump_args[0] == *dst => {
            let target_block = rir.blocks.iter().find(|b| b.id == *target)?;
            if !target_block.insts.is_empty() {
                return None;
            }
            match (&target_block.terminator, target_block.params.first()) {
                (Term::Return(rv), Some((p, _))) if rv == p => Some((callee, args)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Uniform-NB tail self-recursion detection. Mirrors
/// [`detect_tail_call_self`] but also recognizes the
/// `CallSelf`-then-no-op-`BoxTyped` shape the uniform-NB translator
/// emits when a recursive arm's value widens to `Any` to merge with a
/// sibling typed branch (`BoxTyped` is an identity under the NB ABI).
///
/// Returns `(Some(tail_args), trim_extra)` when the block ends in a
/// tail-position self-call — `trim_extra` is the count of trailing
/// no-op insts (the `BoxTyped`) to drop alongside the `CallSelf` — or
/// `(None, 0)` otherwise. Used both to choose the inner `CallConv`
/// (`Tail` iff any block tail-self-recurses, so `return_call` is legal)
/// and to drive codegen at the call site.
fn detect_uniform_nb_tail_self<'a>(
    rir: &'a RirFunction,
    block: &'a cs_rir::Block,
) -> (Option<&'a [RirValue]>, usize) {
    let n = block.insts.len();
    if n >= 2 {
        if let (
            Some(Inst::CallSelf(call_dst, call_args)),
            Some(Inst::BoxTyped(box_dst, box_src, _)),
        ) = (block.insts.get(n - 2), block.insts.get(n - 1))
        {
            if box_src == call_dst {
                // Pattern (c) — trivial-join/return check using box_dst
                // as the value passed forward.
                let ok = match &block.terminator {
                    Term::Return(ret_v) => ret_v == box_dst,
                    Term::Jump(target, jump_args)
                        if jump_args.len() == 1 && jump_args[0] == *box_dst =>
                    {
                        rir.blocks
                            .iter()
                            .find(|b| b.id == *target)
                            .map_or(false, |tb| {
                                tb.insts.is_empty()
                                    && matches!(
                                        &tb.terminator,
                                        Term::Return(rv)
                                            if tb.params.first().map_or(false, |(p, _)| rv == p)
                                    )
                            })
                    }
                    _ => false,
                };
                if ok {
                    return (Some(call_args.as_slice()), 1);
                }
            }
        }
    }
    (detect_tail_call_self(rir, block), 0)
}

/// ADR 0020 Strategy C iter-3 — raw-Fixnum self-call ABI gate.
///
/// Decides whether a uniform-NB body can pass raw (unboxed, sign-
/// extended i64) Fixnum args and return raw to its own recursive
/// calls, boxing only at the NB perimeter (the outer trampoline).
/// This closes the fib/tak parity gap with the legacy pure-fixnum
/// tier (#50): a call-dominated body no longer boxes args / unboxes
/// the result at every self-call.
///
/// Returns `true` only for bodies where the raw representation
/// provably "closes":
///  1. all params Fixnum (at least one),
///  2. at least one `CallSelf` (it is self-recursive),
///  3. no cross-function `Call`/`CallGeneral` — those are the #47 NB
///     path and interact with the tail-call bounce; kept separate,
///  4. a forward-only (topologically ordered) CFG — excludes loops,
///     whose loop-carried block params are a later iter,
///  5. every block param is Fixnum and all its incoming jump-args are
///     produced raw,
///  6. every `Term::Return` value is produced raw,
///  7. every `CallSelf` arg is produced raw.
///
/// "Produced raw" = a Fixnum param/const, an `Add`/`Sub`/`Mul` whose
/// operands are both raw (the unboxed-arith lane), a `CallSelf` result
/// (raw under this ABI), or a raw block param. When any condition
/// fails the body keeps the iter-1 NB self-call behavior (correct,
/// just boxes at the call boundary).
fn detect_uniform_nb_raw_abi(rir: &RirFunction) -> bool {
    use cs_rir::Type;
    // 1. Non-empty, all-Fixnum params.
    if rir.params.is_empty() || !rir.params.iter().all(|(_, t)| matches!(t, Type::Fixnum)) {
        return false;
    }
    // 2/3. Pure self-recursion: ≥1 CallSelf, no cross-function call.
    let mut has_self_call = false;
    for blk in &rir.blocks {
        for inst in &blk.insts {
            match inst {
                Inst::CallSelf(_, _) => has_self_call = true,
                Inst::Call(_, _, _) | Inst::CallGeneral(_, _, _) => return false,
                _ => {}
            }
        }
    }
    if !has_self_call {
        return false;
    }
    // 4. Forward-only CFG: every edge targets a later block in
    // `rir.blocks` order (a topologically ordered DAG).
    let pos: HashMap<cs_rir::BlockId, usize> = rir
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();
    for blk in &rir.blocks {
        let src = pos[&blk.id];
        let fwd = |t: &cs_rir::BlockId| pos.get(t).map_or(false, |&p| p > src);
        let ok = match &blk.terminator {
            Term::Return(_) => true,
            Term::Jump(t, _) => fwd(t),
            Term::Branch(_, t, e, _) => fwd(t) && fwd(e),
        };
        if !ok {
            return false;
        }
    }
    // Predecessor arg vectors per block (positional to block params).
    let mut incoming: HashMap<cs_rir::BlockId, Vec<&Vec<RirValue>>> = HashMap::new();
    for blk in &rir.blocks {
        match &blk.terminator {
            Term::Jump(t, args) => incoming.entry(*t).or_default().push(args),
            Term::Branch(_, t, e, args) => {
                incoming.entry(*t).or_default().push(args);
                incoming.entry(*e).or_default().push(args);
            }
            Term::Return(_) => {}
        }
    }
    // Forward raw-set pass — blocks already proven topologically
    // ordered, so a value is defined (and its raw-ness known) before
    // any use.
    let mut raw: HashSet<RirValue> = HashSet::new();
    for (v, _) in &rir.params {
        raw.insert(*v);
    }
    for blk in &rir.blocks {
        // 5. Block params (non-entry) must be Fixnum + all-incoming-raw.
        if blk.id != rir.entry {
            let preds = incoming.get(&blk.id);
            for (i, (pv, pty)) in blk.params.iter().enumerate() {
                if !matches!(pty, Type::Fixnum) {
                    return false;
                }
                let all_raw = preds.map_or(false, |ps| {
                    !ps.is_empty()
                        && ps
                            .iter()
                            .all(|args| args.get(i).map_or(false, |a| raw.contains(a)))
                });
                if !all_raw {
                    return false;
                }
                raw.insert(*pv);
            }
        }
        for inst in &blk.insts {
            match inst {
                Inst::LoadConst(dst, Const::Fixnum(_)) => {
                    raw.insert(*dst);
                }
                Inst::Add(dst, l, r) | Inst::Sub(dst, l, r) | Inst::Mul(dst, l, r) => {
                    if raw.contains(l) && raw.contains(r) {
                        raw.insert(*dst);
                    }
                }
                Inst::CallSelf(dst, args) => {
                    // 7. Self-call args must be raw.
                    if !args.iter().all(|a| raw.contains(a)) {
                        return false;
                    }
                    raw.insert(*dst);
                }
                _ => {}
            }
        }
        // 6. Return value must be raw.
        if let Term::Return(v) = &blk.terminator {
            if !raw.contains(v) {
                return false;
            }
        }
    }
    true
}

fn detect_tail_call_self_inner<'a>(
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

/// Pre-codegen well-formedness gate for a lowered CLIF function
/// (issue #16). Returns `JitError::Unsupported` for functions
/// Cranelift's `define_function` / `optimize` pipeline cannot
/// handle, so `jit_tier_up_hook` declines the body to the VM tier
/// cleanly instead of catching a panic raised from inside
/// Cranelift.
///
/// Currently checks the one malformed shape observed in practice
/// (issue #4): a function whose block layout is empty. Cranelift's
/// `remove_constant_phis` pass calls `func.layout.entry_block()`
/// and `.expect("entry block unknown")`, aborting the host when it
/// is `None`. A non-empty `rir` can still lower to an empty layout
/// when a `FunctionBuilder` state bug leaves emitted blocks
/// unlinked; rather than depend on identifying every such RIR
/// shape up front, this gate inspects the actual lowered output.
fn verify_clif_lowerable(func: &ClifFunction) -> Result<(), JitError> {
    if func.layout.entry_block().is_none() {
        return Err(JitError::Malformed(
            "lowered CLIF has an empty block layout".into(),
        ));
    }
    Ok(())
}

/// Bundle of `FuncRef`s for the Stage 3 NB-typed slow-path helpers
/// plus the existing Gc-shape Pair / refcount helpers (which already
/// speak the NB i64 ABI thanks to K1 step 2b). Threaded through the
/// body-lowering free functions so they don't need an enormous arg
/// list.
struct NbHelpers {
    // Arithmetic / comparison slow paths.
    add: cranelift_codegen::ir::FuncRef,
    sub: cranelift_codegen::ir::FuncRef,
    mul: cranelift_codegen::ir::FuncRef,
    /// Phase 5b iter7 — Fixnum/Fixnum-compatible NB division. No
    /// inline fast path; `Inst::Div` always calls this.
    div: cranelift_codegen::ir::FuncRef,
    lt: cranelift_codegen::ir::FuncRef,
    eq: cranelift_codegen::ir::FuncRef,
    // `le`, `gt`, `ge` are wired through but not currently emitted —
    // the translator lowers `(> a b)` and `(>= a b)` by swapping
    // operands and calling `lt`/`le`. Keeping the FuncRefs declared
    // means a future code-path that wants direct calls can use them
    // without revisiting the helper-registration plumbing. (Without
    // #[allow(dead_code)] the compiler flags these unread.)
    #[allow(dead_code)]
    le: cranelift_codegen::ir::FuncRef,
    #[allow(dead_code)]
    gt: cranelift_codegen::ir::FuncRef,
    #[allow(dead_code)]
    ge: cranelift_codegen::ir::FuncRef,
    // Pair primitives.
    alloc_pair: cranelift_codegen::ir::FuncRef,
    pair_car: cranelift_codegen::ir::FuncRef,
    pair_cdr: cranelift_codegen::ir::FuncRef,
    pair_p: cranelift_codegen::ir::FuncRef,
    null_p: cranelift_codegen::ir::FuncRef,
    // Vector primitives.
    alloc_vector: cranelift_codegen::ir::FuncRef,
    vector_ref: cranelift_codegen::ir::FuncRef,
    vector_set: cranelift_codegen::ir::FuncRef,
    vector_length: cranelift_codegen::ir::FuncRef,
    vector_p: cranelift_codegen::ir::FuncRef,
    // Refcount management for Any-shape values.
    value_clone: cranelift_codegen::ir::FuncRef,
    value_drop: cranelift_codegen::ir::FuncRef,
    // Self-recursion (the inner func) and the general-call slow
    // path. `vm_call_general(callee, args_ptr, n_args, slot_ptr)`.
    self_fn: cranelift_codegen::ir::FuncRef,
    call_general: cranelift_codegen::ir::FuncRef,
    // ADR 0019 — proper-tail-call bounce. `vm_jit_set_tailcall(callee,
    // args_ptr, n_args) -> i64`. Emitted for tail-position Call /
    // CallGeneral instead of the IC dispatch.
    set_tailcall: cranelift_codegen::ir::FuncRef,
    request_deopt: cranelift_codegen::ir::FuncRef,
    // Env access. EnvLookup variants share `vm_env_lookup_any` for
    // uniform-NB (it returns an NB-shaped Gc<Value> wrap); EnvSet
    // uses the new `vm_env_set_nb`. `MakeClosure` invokes
    // `vm_make_closure` which reads the enclosing env from the
    // thread-local installed by `try_dispatch_jit_nb`.
    env_lookup_any: cranelift_codegen::ir::FuncRef,
    env_set_nb: cranelift_codegen::ir::FuncRef,
    env_define_local_nb: cranelift_codegen::ir::FuncRef,
    make_closure: cranelift_codegen::ir::FuncRef,
    // IC hot path (CallGeneral): `vm_closure_id_peek(callee) -> u32`
    // reads the callee's lambda id without consuming the handle;
    // `vm_ic_dispatch(callee, args_ptr, n_args, jit_ptr, slot_ptr)`
    // runs the cached native body with the same env / bytecode /
    // stack-map TLS guards `try_dispatch_jit_nb` installs. Mirrors
    // the specialized tier's CallGeneral plumbing so uniform-NB
    // sites also get IC speedups when the callee tiers up.
    closure_id_peek: cranelift_codegen::ir::FuncRef,
    ic_dispatch: cranelift_codegen::ir::FuncRef,
}

/// Stage 3 baseline-tier per-Inst lowering. Walks a single block's
/// instructions and emits Cranelift IR for the supported subset.
/// Returns `JitError::Unsupported(...)` on RIR variants the iter
/// hasn't covered yet — the translator's coverage analysis will
/// route those bodies to the specialized tier or bytecode.
fn lower_inst_uniform_nb(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    raw: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    helpers: &NbHelpers,
    blk: &cs_rir::Block,
    raw_abi: bool,
) -> Result<(), JitError> {
    for inst in &blk.insts {
        match inst {
            Inst::LoadConst(dst, c) => {
                let nb = encode_const_as_nb(c);
                let v = b.ins().iconst(I64, nb);
                map.insert(*dst, v);
                // ADR 0020 Strategy C — seed the raw lane for Fixnum
                // consts so downstream unboxed arith can consume them
                // without re-extracting a payload.
                if let Const::Fixnum(n) = c {
                    let rv = b.ins().iconst(I64, *n);
                    raw.insert(*dst, rv);
                }
            }
            &Inst::Add(dst, lhs, rhs) => {
                lower_nb_arith(
                    b,
                    map,
                    raw,
                    helpers,
                    helpers.add,
                    dst,
                    lhs,
                    rhs,
                    |b, l, r| b.ins().iadd(l, r),
                )?;
            }
            &Inst::Sub(dst, lhs, rhs) => {
                lower_nb_arith(
                    b,
                    map,
                    raw,
                    helpers,
                    helpers.sub,
                    dst,
                    lhs,
                    rhs,
                    |b, l, r| b.ins().isub(l, r),
                )?;
            }
            &Inst::Mul(dst, lhs, rhs) => {
                lower_nb_arith(
                    b,
                    map,
                    raw,
                    helpers,
                    helpers.mul,
                    dst,
                    lhs,
                    rhs,
                    |b, l, r| b.ins().imul(l, r),
                )?;
            }
            // Phase 6 Stage B1 — Inst::Div uses a speculative
            // exact-integer fast path for the (NB Fixnum) / (NB
            // Fixnum) case where the divide is exact. Falls back
            // to `vm_value_div_nb` for any other shape (non-Fixnum
            // operands, non-divisible result, sdiv-trap edge cases).
            // The motivating case is spectral-norm's `matrix-elt`
            // where `(/ (* ij (+ ij 1)) 2)` is always exact.
            &Inst::Div(dst, lhs, rhs) => {
                let a = lookup(map, lhs)?;
                let bv = lookup(map, rhs)?;
                let r = emit_nb_div_fixnum_fast(b, helpers.div, a, bv);
                map.insert(dst, r);
            }
            &Inst::Lt(dst, lhs, rhs) => {
                if let (Some(&ra), Some(&rb)) = (raw.get(&lhs), raw.get(&rhs)) {
                    let r = emit_raw_cmp_nb_bool(b, ra, rb, IntCC::SignedLessThan);
                    map.insert(dst, r);
                } else {
                    let a = lookup(map, lhs)?;
                    let bv = lookup(map, rhs)?;
                    let r = emit_nb_cmp_fixnum_fast(b, helpers.lt, a, bv, |b, l, r| {
                        b.ins().icmp(IntCC::SignedLessThan, l, r)
                    });
                    map.insert(dst, r);
                }
            }
            &Inst::Eq(dst, lhs, rhs) => {
                if let (Some(&ra), Some(&rb)) = (raw.get(&lhs), raw.get(&rhs)) {
                    let r = emit_raw_cmp_nb_bool(b, ra, rb, IntCC::Equal);
                    map.insert(dst, r);
                } else {
                    let a = lookup(map, lhs)?;
                    let bv = lookup(map, rhs)?;
                    let r = emit_nb_cmp_fixnum_fast(b, helpers.eq, a, bv, |b, l, r| {
                        b.ins().icmp(IntCC::Equal, l, r)
                    });
                    map.insert(dst, r);
                }
            }
            // (RIR has no Le/Gt/Ge variants — the translator rewrites
            // those to `Lt` with swapped args plus `not`. Helpers
            // `vm_value_le_nb`/`_gt_nb`/`_ge_nb` are exposed for future
            // use but currently unreferenced from this lowering.)
            // ─── Flonum primitives ─────────────────────────────────────
            // NB Flonum encoding IS raw `f64::to_bits` (sign-bit-clear
            // qNaN canonicalized), bit-identical to `JIT_RT_FLONUM`. So
            // these lower exactly like the specialized tier: bitcast i64
            // operand to f64, op, bitcast result to i64.
            &Inst::FlonumAdd(dst, lhs, rhs) => {
                fbinop(b, map, dst, lhs, rhs, |b, l, r| b.ins().fadd(l, r))?
            }
            &Inst::FlonumSub(dst, lhs, rhs) => {
                fbinop(b, map, dst, lhs, rhs, |b, l, r| b.ins().fsub(l, r))?
            }
            &Inst::FlonumMul(dst, lhs, rhs) => {
                fbinop(b, map, dst, lhs, rhs, |b, l, r| b.ins().fmul(l, r))?
            }
            &Inst::FlonumDiv(dst, lhs, rhs) => {
                fbinop(b, map, dst, lhs, rhs, |b, l, r| b.ins().fdiv(l, r))?
            }
            &Inst::FlonumMax(dst, lhs, rhs) => {
                fbinop(b, map, dst, lhs, rhs, |b, l, r| b.ins().fmax(l, r))?
            }
            &Inst::FlonumMin(dst, lhs, rhs) => {
                fbinop(b, map, dst, lhs, rhs, |b, l, r| b.ins().fmin(l, r))?
            }
            &Inst::FlonumSqrt(dst, src) => funary(b, map, dst, src, |b, x| b.ins().sqrt(x))?,
            &Inst::FlonumAbs(dst, src) => funary(b, map, dst, src, |b, x| b.ins().fabs(x))?,
            &Inst::FlonumFloor(dst, src) => funary(b, map, dst, src, |b, x| b.ins().floor(x))?,
            &Inst::FlonumCeil(dst, src) => funary(b, map, dst, src, |b, x| b.ins().ceil(x))?,
            &Inst::FlonumTrunc(dst, src) => funary(b, map, dst, src, |b, x| b.ins().trunc(x))?,
            &Inst::FlonumRound(dst, src) => funary(b, map, dst, src, |b, x| b.ins().nearest(x))?,
            &Inst::FlonumLt(dst, lhs, rhs) => {
                let l_i = lookup(map, lhs)?;
                let r_i = lookup(map, rhs)?;
                let mf = cranelift_codegen::ir::MemFlags::new();
                let l_f = b.ins().bitcast(F64, mf, l_i);
                let r_f = b.ins().bitcast(F64, mf, r_i);
                let cmp = b.ins().fcmp(
                    cranelift_codegen::ir::condcodes::FloatCC::LessThan,
                    l_f,
                    r_f,
                );
                let widened = b.ins().uextend(I64, cmp);
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let result = b.ins().bor_imm(widened, nb_false);
                map.insert(dst, result);
            }
            &Inst::FlonumEq(dst, lhs, rhs) => {
                let l_i = lookup(map, lhs)?;
                let r_i = lookup(map, rhs)?;
                let mf = cranelift_codegen::ir::MemFlags::new();
                let l_f = b.ins().bitcast(F64, mf, l_i);
                let r_f = b.ins().bitcast(F64, mf, r_i);
                let cmp = b
                    .ins()
                    .fcmp(cranelift_codegen::ir::condcodes::FloatCC::Equal, l_f, r_f);
                let widened = b.ins().uextend(I64, cmp);
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let result = b.ins().bor_imm(widened, nb_false);
                map.insert(dst, result);
            }
            &Inst::Cons(dst, car, _car_tag, cdr, _cdr_tag) => {
                // Uniform-NB ignores the per-operand tags emitted by the
                // specialized-tier translator: both operands are NB i64,
                // so we pass `JIT_RT_ANY` (which `vm_alloc_pair_gc`
                // routes through `gc_i64_to_value` = `to_value`).
                let car_v = lookup(map, car)?;
                let cdr_v = lookup(map, cdr)?;
                let any_tag = b.ins().iconst(I64, cs_vm::vm::JIT_RT_ANY as i64);
                let call = b
                    .ins()
                    .call(helpers.alloc_pair, &[car_v, any_tag, cdr_v, any_tag]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::Car(dst, src) => {
                // `vm_pair_car_gc` already decodes via `NanboxValue::to_value`
                // so it accepts NB_TAG_PAIR inputs natively. Returns an NB
                // (encoded via `value_to_gc_i64`).
                let v = lookup(map, src)?;
                let call = b.ins().call(helpers.pair_car, &[v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::Cdr(dst, src) => {
                let v = lookup(map, src)?;
                let call = b.ins().call(helpers.pair_cdr, &[v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::PairP(dst, src) => {
                // `vm_pair_p_gc` returns 0/1 i64; or in the NB_FALSE
                // pattern to land a `Boolean` NB.
                let v = lookup(map, src)?;
                let call = b.ins().call(helpers.pair_p, &[v]);
                let raw = b.inst_results(call)[0];
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let result = b.ins().bor_imm(raw, nb_false);
                map.insert(dst, result);
            }
            &Inst::NullP(dst, src) => {
                let v = lookup(map, src)?;
                let call = b.ins().call(helpers.null_p, &[v]);
                let raw = b.inst_results(call)[0];
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let result = b.ins().bor_imm(raw, nb_false);
                map.insert(dst, result);
            }
            // ─── Vector primitives ─────────────────────────────────────
            // The Vector helpers take their `idx`/`n` as RAW fixnum
            // i64s (the specialized tier's RIR Type::Fixnum encoding),
            // not NanboxValue. Uniform-NB has to decode the NB Fixnum
            // payload (sign-extend the 47-bit) on the way in. Same
            // for the output of `VecLength` — re-encode raw → NB.
            //
            // The Gc-tagged operands (`vec`, `fill`, `x`) flow through
            // unchanged: `vm_alloc_vector_gc`, `vm_vector_ref_gc`,
            // `vm_vector_set_gc` all decode their Gc<Value> args via
            // `gc_i64_to_value` (= `NanboxValue::to_value`), so an NB
            // i64 is accepted natively.
            &Inst::VecAlloc(dst, n_op, fill) => {
                let n_nb = lookup(map, n_op)?;
                let payload = b.ins().band_imm(n_nb, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let shl = b.ins().ishl_imm(payload, 17);
                let n_raw = b.ins().sshr_imm(shl, 17);
                let f_v = lookup(map, fill)?;
                let call = b.ins().call(helpers.alloc_vector, &[n_raw, f_v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::VecRef(dst, vec, idx) => {
                let v_v = lookup(map, vec)?;
                let i_nb = lookup(map, idx)?;
                let payload = b.ins().band_imm(i_nb, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let shl = b.ins().ishl_imm(payload, 17);
                let i_raw = b.ins().sshr_imm(shl, 17);
                let call = b.ins().call(helpers.vector_ref, &[v_v, i_raw]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::VecSet(dst, vec, idx, x) => {
                let v_v = lookup(map, vec)?;
                let i_nb = lookup(map, idx)?;
                let payload = b.ins().band_imm(i_nb, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let shl = b.ins().ishl_imm(payload, 17);
                let i_raw = b.ins().sshr_imm(shl, 17);
                let x_v = lookup(map, x)?;
                let call = b.ins().call(helpers.vector_set, &[v_v, i_raw, x_v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::VecLength(dst, vec) => {
                let v_v = lookup(map, vec)?;
                let call = b.ins().call(helpers.vector_length, &[v_v]);
                let raw = b.inst_results(call)[0];
                // Re-encode raw length → NB Fixnum.
                let payload = b.ins().band_imm(raw, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let result = b
                    .ins()
                    .bor_imm(payload, cs_vm::vm::NB_SIGNATURE_BITS as i64);
                map.insert(dst, result);
            }
            &Inst::VecP(dst, src) => {
                let v_v = lookup(map, src)?;
                let call = b.ins().call(helpers.vector_p, &[v_v]);
                let raw = b.inst_results(call)[0];
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let result = b.ins().bor_imm(raw, nb_false);
                map.insert(dst, result);
            }
            &Inst::AnyClone(dst, src) => {
                // `vm_value_clone_gc` increfs the NB payload (no-op for
                // inline immediates) and returns the same i64.
                let v = lookup(map, src)?;
                let call = b.ins().call(helpers.value_clone, &[v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::AnyDrop(src) => {
                let v = lookup(map, src)?;
                b.ins().call(helpers.value_drop, &[v]);
            }
            Inst::CallSelf(dst, args) => {
                // Recursive (non-tail) self-call.
                //
                // iter-3 raw-Fixnum ABI: when `raw_abi`, `inner` consumes
                // and produces raw sign-extended Fixnums, so pass raw args
                // (the gate verified each is raw) and treat the result as
                // raw — `dst` joins the raw lane, with its NB carrier
                // materialized lazily (DCE'd if only unboxed arith /
                // further self-calls consume it). This is what keeps a
                // call-dominated body (fib/tak) from boxing args /
                // unboxing the result at every recursion, matching the
                // pure-fixnum tier (#50). Otherwise pass/return NB.
                let cargs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(if raw_abi { raw } else { map }, *a))
                    .collect::<Result<_, _>>()?;
                let call = b.ins().call(helpers.self_fn, &cargs);
                let result = b.inst_results(call)[0];
                if raw_abi {
                    raw.insert(*dst, result);
                    let nb = box_raw_fixnum(b, result);
                    map.insert(*dst, nb);
                } else {
                    map.insert(*dst, result);
                }
            }
            &Inst::MakeClosure(dst, lambda_idx) => {
                // Builds a fresh `VmClosure` capturing the enclosing
                // env (read from the thread-local by `vm_make_closure`).
                // Result is an NB-shaped Gc<Value> wrap.
                let idx_v = b.ins().iconst(I64, lambda_idx as i64);
                let call = b.ins().call(helpers.make_closure, &[idx_v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::EnvLookup(dst, sym) => {
                // The specialized translator emits `EnvLookup` only for
                // Fixnum-typed bindings. Uniform-NB doesn't have that
                // type narrowing — route through `vm_env_lookup_any`
                // which returns an NB-shaped Gc<Value> wrap that works
                // for every type.
                let sym_v = b.ins().iconst(I64, sym as i64);
                let call = b.ins().call(helpers.env_lookup_any, &[sym_v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::EnvLookupAny(dst, sym) => {
                let sym_v = b.ins().iconst(I64, sym as i64);
                let call = b.ins().call(helpers.env_lookup_any, &[sym_v]);
                let result = b.inst_results(call)[0];
                b.declare_value_needs_stack_map(result);
                map.insert(dst, result);
            }
            &Inst::EnvSet(sym, value) => {
                let val = lookup(map, value)?;
                let sym_v = b.ins().iconst(I64, sym as i64);
                b.ins().call(helpers.env_set_nb, &[sym_v, val]);
            }
            &Inst::EnvDefineLocal(sym, value) => {
                let val = lookup(map, value)?;
                let sym_v = b.ins().iconst(I64, sym as i64);
                b.ins().call(helpers.env_define_local_nb, &[sym_v, val]);
            }
            // `Inst::Call` is the translator's "I have feedback about
            // this callee" form. Mirror the specialized tier's
            // inline-cache hot path: per-site `IcSlot` with hit/miss
            // branches. Hit dispatches through `vm_ic_dispatch` (NB-
            // aware after Phase 4 keystone follow-up); miss falls
            // through to `vm_call_general`, which populates the slot.
            Inst::Call(dst, callee, args) | Inst::CallGeneral(dst, callee, args) => {
                let callee_v = lookup(map, *callee)?;
                let n = args.len();
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;

                // Stack-allocate the args buffer (`vm_call_general` /
                // `vm_ic_dispatch` both take `*const i64` + count). At
                // least 8 bytes so `stack_addr` is valid for n == 0.
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

                // Per-site IC slot. Box::leak gives a stable
                // process-lifetime address; the i64 constant baked
                // into the body's pool is that address.
                let ic_slot: &'static crate::ic::IcSlot =
                    Box::leak(Box::new(crate::ic::IcSlot::new()));
                let slot_ptr_const = ic_slot as *const crate::ic::IcSlot as i64;
                let slot_addr_v = b.ins().iconst(I64, slot_ptr_const);

                // Peek callee's lambda id (doesn't consume the handle).
                let peek_inst = b.ins().call(helpers.closure_id_peek, &[callee_v]);
                let peeked_id_i32 = b.inst_results(peek_inst)[0];
                // Cached id is u32 at offset 0 of IcSlot (#[repr(C)]).
                let cached_id_i32 = b.ins().load(
                    cranelift_codegen::ir::types::I32,
                    cranelift_codegen::ir::MemFlags::new(),
                    slot_addr_v,
                    0,
                );
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
                b.append_block_param(join_block, I64);
                b.ins().brif(take_hit, hit_block, &[], miss_block, &[]);

                // ===== Hit =====
                b.switch_to_block(hit_block);
                b.seal_block(hit_block);
                let cached_jit_ptr_v =
                    b.ins()
                        .load(I64, cranelift_codegen::ir::MemFlags::new(), slot_addr_v, 8);
                let hit_inst = b.ins().call(
                    helpers.ic_dispatch,
                    &[callee_v, buf_addr, n_args_v, cached_jit_ptr_v, slot_addr_v],
                );
                let hit_result = b.inst_results(hit_inst)[0];
                b.ins().jump(
                    join_block,
                    &[cranelift_codegen::ir::BlockArg::Value(hit_result)],
                );

                // ===== Miss =====
                b.switch_to_block(miss_block);
                b.seal_block(miss_block);
                let miss_inst = b.ins().call(
                    helpers.call_general,
                    &[callee_v, buf_addr, n_args_v, slot_addr_v],
                );
                let miss_result = b.inst_results(miss_inst)[0];
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
            // Phase 5 iter3 — BoxTyped(dst, src, _tag) in uniform-NB
            // is a coordinate-system identity: `src` is already an NB
            // carrier holding the value with its proper NB tag (Fixnum
            // NB / Boolean NB / Character NB / Flonum bits / etc.). Any
            // recipient that decodes via `gc_i64_to_value` handles
            // every NB tag uniformly, so no wrap is needed. The `_tag`
            // arg becomes irrelevant in NB land.
            //
            // In specialized this op allocates a Gc<Value> wrap; here
            // it's free. The dst still needs a stack-map declaration so
            // GC can walk it (in case the value was actually a pointer-
            // tagged NB carrying a Gc handle).
            &Inst::BoxTyped(dst, src, _tag) => {
                let v = lookup(map, src)?;
                b.declare_value_needs_stack_map(v);
                map.insert(dst, v);
            }
            // Phase 5 iter3 — Any→typed conversions in uniform-NB are
            // also identities. The NB-aware downstream ops (Add/Sub/Mul/
            // Lt/Eq via emit_nb_arith_fixnum_fast / emit_nb_cmp_fixnum_
            // fast) check tags at runtime and dispatch fast-or-slow, so
            // no unboxing is needed. In specialized these ops would call
            // vm_unbox_* helpers; here they're free.
            &Inst::AnyToFix(dst, src) | &Inst::AnyToBool(dst, src) | &Inst::AnyToFlo(dst, src) => {
                let v = lookup(map, src)?;
                map.insert(dst, v);
            }
            // AnyTruthy: NB Branch terminator already compares against
            // NB_FALSE bit pattern (see `lower_terminator_uniform_nb`),
            // so any NB carrier works as a branch condition. Identity.
            &Inst::AnyTruthy(dst, src) => {
                let v = lookup(map, src)?;
                map.insert(dst, v);
            }
            // FixToFlo: convert NB Fixnum to NB Flonum (= f64 bits).
            // The translator's static value_types is best-effort —
            // EnvLookup values default to Fixnum but may be any NB
            // shape at runtime. So we dispatch on the tag: NB Fixnum
            // gets the integer→float conversion; anything else (real
            // Flonum f64 bits, Gc<Value> wrap, etc.) passes through
            // unchanged so downstream FlonumXxx ops bitcast it as the
            // f64 it already is. This is safe because NB Flonums ARE
            // f64 bit patterns and the only legitimate non-Fixnum
            // input here is a Flonum that the translator mis-tagged.
            &Inst::FixToFlo(dst, src) => {
                let v = lookup(map, src)?;
                let sig_masked = b.ins().band_imm(v, cs_vm::vm::NB_SIGNATURE_MASK as i64);
                let tag_masked = b.ins().band_imm(v, cs_vm::vm::NB_TAG_MASK as i64);
                let is_nb_sig = b.ins().icmp_imm(
                    IntCC::Equal,
                    sig_masked,
                    cs_vm::vm::NB_SIGNATURE_BITS as i64,
                );
                let is_fixnum_tag = b.ins().icmp_imm(IntCC::Equal, tag_masked, 0);
                let needs_conv = b.ins().band(is_nb_sig, is_fixnum_tag);

                let conv_block = b.create_block();
                let passthrough_block = b.create_block();
                let join_block = b.create_block();
                b.append_block_param(join_block, I64);
                b.ins()
                    .brif(needs_conv, conv_block, &[], passthrough_block, &[]);

                b.switch_to_block(conv_block);
                b.seal_block(conv_block);
                let payload = b.ins().band_imm(v, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let shifted = b.ins().ishl_imm(payload, 17);
                let signed = b.ins().sshr_imm(shifted, 17);
                let f = b.ins().fcvt_from_sint(F64, signed);
                let mf = cranelift_codegen::ir::MemFlags::new();
                let bits = b.ins().bitcast(I64, mf, f);
                b.ins()
                    .jump(join_block, &[cranelift_codegen::ir::BlockArg::Value(bits)]);

                b.switch_to_block(passthrough_block);
                b.seal_block(passthrough_block);
                b.ins()
                    .jump(join_block, &[cranelift_codegen::ir::BlockArg::Value(v)]);

                b.switch_to_block(join_block);
                b.seal_block(join_block);
                let result = b.block_params(join_block)[0];
                map.insert(dst, result);
            }
            // IntCharBitcast: same bit pattern, just retag the SSA value.
            // In NB land both Fixnum and Character carry the codepoint
            // in low bits — but tag differs. We need to re-tag: strip
            // the FIXNUM header, OR in the CHARACTER header.
            &Inst::IntCharBitcast(dst, src) => {
                let v = lookup(map, src)?;
                let payload = b.ins().band_imm(v, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let char_tag_bits = (cs_vm::vm::NB_SIGNATURE_BITS
                    | ((cs_vm::vm::NB_TAG_CHARACTER as u64) << 47))
                    as i64;
                let retagged = b.ins().bor_imm(payload, char_tag_bits);
                map.insert(dst, retagged);
            }
            other => {
                return Err(JitError::Unsupported(format!(
                    "uniform-nb: Inst {:?} not yet supported",
                    other
                )));
            }
        }
    }
    Ok(())
}

/// Stage 3 baseline-tier terminator lowering. Supports `Return`,
/// `Jump` (with block params), and `Branch` (NB truthiness check
/// against `Value::Boolean(false)` bit pattern).
fn lower_terminator_uniform_nb(
    b: &mut FunctionBuilder,
    map: &HashMap<RirValue, cranelift_codegen::ir::Value>,
    raw: &HashMap<RirValue, cranelift_codegen::ir::Value>,
    block_map: &HashMap<cs_rir::BlockId, cranelift_codegen::ir::Block>,
    term: &Term,
    raw_abi: bool,
) -> Result<(), JitError> {
    // iter-3 raw ABI: the function returns raw, and every block param is
    // a raw Fixnum, so `Return` values and `Jump`/`Branch` block-args are
    // read from the raw lane. (A `Branch` condition is a Boolean, never a
    // Fixnum, so it always reads the NB lane.)
    let arg_map = if raw_abi { raw } else { map };
    match term {
        Term::Return(v) => {
            let val = lookup(arg_map, *v)?;
            b.ins().return_(&[val]);
            Ok(())
        }
        Term::Jump(target, args) => {
            let tb = *block_map
                .get(target)
                .ok_or_else(|| JitError::Codegen(format!("unknown jump target {:?}", target)))?;
            let cargs: Vec<cranelift_codegen::ir::BlockArg> = args
                .iter()
                .map(|a| lookup(arg_map, *a).map(cranelift_codegen::ir::BlockArg::Value))
                .collect::<Result<_, _>>()?;
            b.ins().jump(tb, &cargs);
            Ok(())
        }
        Term::Branch(cond, then_b, else_b, args) => {
            let cv = lookup(map, *cond)?;
            let tb = *block_map
                .get(then_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown then target {:?}", then_b)))?;
            let eb = *block_map
                .get(else_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown else target {:?}", else_b)))?;
            // NB truthiness: falsy IFF the bit pattern equals NB_FALSE
            // (Value::Boolean(false) NB). Anything else is truthy.
            let nb_false_bits = cs_vm::vm::NanboxValue::FALSE.into_raw();
            let truthy = b.ins().icmp_imm(IntCC::NotEqual, cv, nb_false_bits);
            // RC3 iter 2.13 — pass block args to both successors so
            // their params get their incoming values.
            let cargs: Vec<cranelift_codegen::ir::BlockArg> = args
                .iter()
                .map(|a| lookup(arg_map, *a).map(cranelift_codegen::ir::BlockArg::Value))
                .collect::<Result<_, _>>()?;
            b.ins().brif(truthy, tb, &cargs, eb, &cargs);
            Ok(())
        }
    }
}

/// ADR 0020 Strategy C — unbox an NB Fixnum carrier to a raw
/// sign-extended i64. Emitted once at function entry for `Type::Fixnum`
/// params (the dispatcher in `run_jit_body_once` already validated the
/// Fixnum tag and deopts on a miss, so no per-op check is needed).
/// Mirrors the payload-extract in `emit_nb_arith_fixnum_fast`.
fn unbox_nb_fixnum(
    b: &mut FunctionBuilder,
    nb: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use cs_vm::vm::NB_PAYLOAD_MASK;
    let payload = b.ins().band_imm(nb, NB_PAYLOAD_MASK as i64);
    let shl = b.ins().ishl_imm(payload, 17);
    b.ins().sshr_imm(shl, 17)
}

/// ADR 0020 Strategy C — box a raw sign-extended i64 back into an NB
/// Fixnum carrier. Emitted where an unboxed value flows into an NB
/// consumer (return, call args, cons, …); when the only consumers are
/// further unboxed arith, Cranelift DCE removes it.
fn box_raw_fixnum(
    b: &mut FunctionBuilder,
    raw: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use cs_vm::vm::{NB_PAYLOAD_MASK, NB_SIGNATURE_BITS};
    let payload = b.ins().band_imm(raw, NB_PAYLOAD_MASK as i64);
    b.ins().bor_imm(payload, NB_SIGNATURE_BITS as i64)
}

/// ADR 0020 Strategy C — unboxed Fixnum arithmetic. `a`/`bv` are raw
/// sign-extended operands already proven Fixnum (no tag check).
/// Computes `op(a, bv)`, verifies the result fits the 47-bit NB Fixnum
/// range, and on overflow calls `vm_jit_request_deopt` — continuing
/// with the 47-bit-truncated value as a benign placeholder, since the
/// dispatcher discards the body's result once the sentinel is set and
/// retries on bytecode (which promotes to bignum). Returns raw.
fn emit_unboxed_fixnum_arith(
    b: &mut FunctionBuilder,
    request_deopt: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    bv: cranelift_codegen::ir::Value,
    op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let raw = op(b, a, bv);
    // 47-bit fit check: round-trip through a 17-bit sign extension.
    let raw_shl = b.ins().ishl_imm(raw, 17);
    let raw_ext = b.ins().sshr_imm(raw_shl, 17);
    let fits = b.ins().icmp(IntCC::Equal, raw, raw_ext);
    let ok_bb = b.create_block();
    let ov_bb = b.create_block();
    let join_bb = b.create_block();
    b.append_block_param(join_bb, I64);
    b.ins().brif(fits, ok_bb, &[], ov_bb, &[]);

    b.switch_to_block(ok_bb);
    b.seal_block(ok_bb);
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(raw)]);

    b.switch_to_block(ov_bb);
    b.seal_block(ov_bb);
    let reason = b
        .ins()
        .iconst(I64, cs_vm::vm::DEOPT_REASON_FIXNUM_OVERFLOW as i64);
    b.ins().call(request_deopt, &[reason]);
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(raw_ext)]);

    b.switch_to_block(join_bb);
    b.seal_block(join_bb);
    b.block_params(join_bb)[0]
}

/// ADR 0020 Strategy C — lower one NB binary arith op (`Add`/`Sub`/
/// `Mul`). When both operands are in the raw lane (proven Fixnum) the
/// op is unboxed: `dst` joins the raw lane and its NB carrier is
/// materialized lazily (DCE'd if only unboxed arith consumes it).
/// Otherwise the standard NB Fixnum fast path runs.
#[allow(clippy::too_many_arguments)]
fn lower_nb_arith(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    raw: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    helpers: &NbHelpers,
    slow_fnref: cranelift_codegen::ir::FuncRef,
    dst: RirValue,
    lhs: RirValue,
    rhs: RirValue,
    op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    if let (Some(&ra), Some(&rb)) = (raw.get(&lhs), raw.get(&rhs)) {
        let rr = emit_unboxed_fixnum_arith(b, helpers.request_deopt, ra, rb, op);
        raw.insert(dst, rr);
        let nb = box_raw_fixnum(b, rr);
        map.insert(dst, nb);
    } else {
        let a = lookup(map, lhs)?;
        let bv = lookup(map, rhs)?;
        let r = emit_nb_arith_fixnum_fast(b, slow_fnref, a, bv, op);
        map.insert(dst, r);
    }
    Ok(())
}

/// ADR 0020 Strategy C iter-3 — compare two raw (proven-Fixnum) i64
/// operands directly and encode the result as an NB Boolean, skipping
/// the per-operand tag check `emit_nb_cmp_fixnum_fast` would emit.
/// Same encoding as the Flonum compares: `(cmp as i64) | NB_FALSE`
/// yields `NB_FALSE` when false and the NB `#t` carrier when true, so
/// the NB-truthiness `Branch` lowering consumes it unchanged.
fn emit_raw_cmp_nb_bool(
    b: &mut FunctionBuilder,
    ra: cranelift_codegen::ir::Value,
    rb: cranelift_codegen::ir::Value,
    cc: IntCC,
) -> cranelift_codegen::ir::Value {
    let cmp = b.ins().icmp(cc, ra, rb);
    let widened = b.ins().uextend(I64, cmp);
    let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
    b.ins().bor_imm(widened, nb_false)
}

/// Emit the inline Fixnum-Fixnum fast path for an NB-typed binary
/// arithmetic op (`Add`/`Sub`/`Mul`). On Fixnum/Fixnum operands and no
/// 47-bit overflow, the op runs as a single i64 instruction and the
/// result is re-encoded as a Fixnum NB. Otherwise (mixed types or
/// overflow) the slow-path helper is called.
fn emit_nb_arith_fixnum_fast(
    b: &mut FunctionBuilder,
    slow_fnref: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    bv: cranelift_codegen::ir::Value,
    fast_op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use cs_vm::vm::{NB_PAYLOAD_MASK, NB_SIGNATURE_BITS, NB_SIGNATURE_MASK, NB_TAG_MASK};

    let combined_mask = (NB_SIGNATURE_MASK | NB_TAG_MASK) as i64;
    let fixnum_pattern = NB_SIGNATURE_BITS as i64; // NB_TAG_FIXNUM == 0.

    let a_masked = b.ins().band_imm(a, combined_mask);
    let b_masked = b.ins().band_imm(bv, combined_mask);
    let a_fix = b.ins().icmp_imm(IntCC::Equal, a_masked, fixnum_pattern);
    let b_fix = b.ins().icmp_imm(IntCC::Equal, b_masked, fixnum_pattern);
    let both_fix = b.ins().band(a_fix, b_fix);

    let fast_bb = b.create_block();
    let slow_bb = b.create_block();
    let join_bb = b.create_block();
    b.append_block_param(join_bb, I64);

    b.ins().brif(both_fix, fast_bb, &[], slow_bb, &[]);

    // FAST: extract Fixnum payloads, do op, check overflow.
    b.switch_to_block(fast_bb);
    b.seal_block(fast_bb);
    let a_payload = b.ins().band_imm(a, NB_PAYLOAD_MASK as i64);
    let b_payload = b.ins().band_imm(bv, NB_PAYLOAD_MASK as i64);
    let a_shl = b.ins().ishl_imm(a_payload, 17);
    let av = b.ins().sshr_imm(a_shl, 17);
    let b_shl = b.ins().ishl_imm(b_payload, 17);
    let bvv = b.ins().sshr_imm(b_shl, 17);
    let raw = fast_op(b, av, bvv);
    let raw_shl = b.ins().ishl_imm(raw, 17);
    let raw_ext = b.ins().sshr_imm(raw_shl, 17);
    let fits = b.ins().icmp(IntCC::Equal, raw, raw_ext);
    let ok_bb = b.create_block();
    let ov_bb = b.create_block();
    b.ins().brif(fits, ok_bb, &[], ov_bb, &[]);

    b.switch_to_block(ok_bb);
    b.seal_block(ok_bb);
    let payload_only = b.ins().band_imm(raw, NB_PAYLOAD_MASK as i64);
    let encoded = b.ins().bor_imm(payload_only, NB_SIGNATURE_BITS as i64);
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(encoded)]);

    b.switch_to_block(ov_bb);
    b.seal_block(ov_bb);
    let call_ov = b.ins().call(slow_fnref, &[a, bv]);
    let r_ov = b.inst_results(call_ov)[0];
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(r_ov)]);

    b.switch_to_block(slow_bb);
    b.seal_block(slow_bb);
    let call_slow = b.ins().call(slow_fnref, &[a, bv]);
    let r_slow = b.inst_results(call_slow)[0];
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(r_slow)]);

    b.switch_to_block(join_bb);
    b.seal_block(join_bb);
    b.block_params(join_bb)[0]
}

/// Phase 6 Stage B1 — speculative exact-integer fast path for NB
/// Fixnum-Fixnum division. Inlines `sdiv` + `srem` and takes the
/// fast path only when the divide is exact (rem == 0); otherwise
/// falls back to `vm_value_div_nb` which handles the Rational
/// path. Eliminates the ~3 ephemeral Rational allocations per
/// always-divisible `(/ Fixnum Fixnum)` call.
///
/// Motivating case: spectral-norm's `matrix-elt` computes
/// `(/ (* ij (+ ij 1)) 2)` where the product of consecutive
/// integers is always even — every call hits the fast path.
///
/// Guards (any failure → slow path call to `slow_fnref`):
/// - Both operands NB Fixnum-tagged.
/// - `b != 0` (sdiv with zero divisor traps on x86).
/// - NOT (`a == INT_MIN && b == -1`) (sdiv overflow trap).
/// - `rem == 0` (exact divisibility).
/// - `quot` fits in 47-bit NB Fixnum payload range.
///
/// `slow_bb` is shared across every "fast path failed" branch;
/// it's sealed last so all predecessors get registered.
fn emit_nb_div_fixnum_fast(
    b: &mut FunctionBuilder,
    slow_fnref: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    bv: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use cs_vm::vm::{NB_PAYLOAD_MASK, NB_SIGNATURE_BITS, NB_SIGNATURE_MASK, NB_TAG_MASK};

    let combined_mask = (NB_SIGNATURE_MASK | NB_TAG_MASK) as i64;
    let fixnum_pattern = NB_SIGNATURE_BITS as i64; // NB_TAG_FIXNUM == 0.

    // Check 1: both operands NB Fixnum-tagged.
    let a_masked = b.ins().band_imm(a, combined_mask);
    let b_masked = b.ins().band_imm(bv, combined_mask);
    let a_fix = b.ins().icmp_imm(IntCC::Equal, a_masked, fixnum_pattern);
    let b_fix = b.ins().icmp_imm(IntCC::Equal, b_masked, fixnum_pattern);
    let both_fix = b.ins().band(a_fix, b_fix);

    let try_fast_bb = b.create_block();
    let do_div_bb = b.create_block();
    let encode_bb = b.create_block();
    let slow_bb = b.create_block();
    let join_bb = b.create_block();
    b.append_block_param(join_bb, I64);

    b.ins().brif(both_fix, try_fast_bb, &[], slow_bb, &[]);

    // Extract sign-extended payloads. Check b != 0 and not the
    // INT_MIN / -1 overflow case before sdiv (those would trap).
    b.switch_to_block(try_fast_bb);
    b.seal_block(try_fast_bb);
    let a_payload = b.ins().band_imm(a, NB_PAYLOAD_MASK as i64);
    let b_payload = b.ins().band_imm(bv, NB_PAYLOAD_MASK as i64);
    let a_shl = b.ins().ishl_imm(a_payload, 17);
    let av = b.ins().sshr_imm(a_shl, 17);
    let b_shl = b.ins().ishl_imm(b_payload, 17);
    let bvv = b.ins().sshr_imm(b_shl, 17);

    let b_nonzero = b.ins().icmp_imm(IntCC::NotEqual, bvv, 0);
    // sdiv trap on i64::MIN / -1; gate explicitly.
    let a_eq_min = b.ins().icmp_imm(IntCC::Equal, av, i64::MIN);
    let b_eq_neg1 = b.ins().icmp_imm(IntCC::Equal, bvv, -1);
    let would_overflow = b.ins().band(a_eq_min, b_eq_neg1);
    let no_overflow = b.ins().bxor_imm(would_overflow, 1);
    let safe_to_divide = b.ins().band(b_nonzero, no_overflow);
    b.ins().brif(safe_to_divide, do_div_bb, &[], slow_bb, &[]);

    // Do the divide, check rem == 0 and quot fits 47-bit Fixnum.
    b.switch_to_block(do_div_bb);
    b.seal_block(do_div_bb);
    let quot = b.ins().sdiv(av, bvv);
    let rem = b.ins().srem(av, bvv);
    let rem_zero = b.ins().icmp_imm(IntCC::Equal, rem, 0);
    let quot_shl = b.ins().ishl_imm(quot, 17);
    let quot_ext = b.ins().sshr_imm(quot_shl, 17);
    let quot_fits = b.ins().icmp(IntCC::Equal, quot, quot_ext);
    let fast_ok = b.ins().band(rem_zero, quot_fits);
    b.ins().brif(fast_ok, encode_bb, &[], slow_bb, &[]);

    // Fast path: encode quot as NB Fixnum.
    b.switch_to_block(encode_bb);
    b.seal_block(encode_bb);
    let payload_only = b.ins().band_imm(quot, NB_PAYLOAD_MASK as i64);
    let encoded = b.ins().bor_imm(payload_only, NB_SIGNATURE_BITS as i64);
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(encoded)]);

    // Slow path: call vm_value_div_nb. Reached when any of the
    // guards failed (non-Fixnum operands, divide-by-zero, sdiv
    // overflow, non-exact result, or quot out of 47-bit range).
    b.switch_to_block(slow_bb);
    b.seal_block(slow_bb);
    let call_slow = b.ins().call(slow_fnref, &[a, bv]);
    let r_slow = b.inst_results(call_slow)[0];
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(r_slow)]);

    b.switch_to_block(join_bb);
    b.seal_block(join_bb);
    b.block_params(join_bb)[0]
}

/// Emit the inline Fixnum-Fixnum fast path for an NB-typed comparison
/// (`Lt`/`Eq`). The 0/1 result of the underlying `icmp` is or'd with
/// the `Boolean(false)` NB bit pattern — adding the low bit if true,
/// leaving it clear if false, matching `NanboxValue::boolean(b)`.
fn emit_nb_cmp_fixnum_fast(
    b: &mut FunctionBuilder,
    slow_fnref: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    bv: cranelift_codegen::ir::Value,
    fast_cmp: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use cs_vm::vm::{
        NanboxValue, NB_PAYLOAD_MASK, NB_SIGNATURE_BITS, NB_SIGNATURE_MASK, NB_TAG_MASK,
    };

    let combined_mask = (NB_SIGNATURE_MASK | NB_TAG_MASK) as i64;
    let fixnum_pattern = NB_SIGNATURE_BITS as i64;

    let a_masked = b.ins().band_imm(a, combined_mask);
    let b_masked = b.ins().band_imm(bv, combined_mask);
    let a_fix = b.ins().icmp_imm(IntCC::Equal, a_masked, fixnum_pattern);
    let b_fix = b.ins().icmp_imm(IntCC::Equal, b_masked, fixnum_pattern);
    let both_fix = b.ins().band(a_fix, b_fix);

    let fast_bb = b.create_block();
    let slow_bb = b.create_block();
    let join_bb = b.create_block();
    b.append_block_param(join_bb, I64);

    b.ins().brif(both_fix, fast_bb, &[], slow_bb, &[]);

    b.switch_to_block(fast_bb);
    b.seal_block(fast_bb);
    let a_payload = b.ins().band_imm(a, NB_PAYLOAD_MASK as i64);
    let b_payload = b.ins().band_imm(bv, NB_PAYLOAD_MASK as i64);
    let a_shl = b.ins().ishl_imm(a_payload, 17);
    let av = b.ins().sshr_imm(a_shl, 17);
    let b_shl = b.ins().ishl_imm(b_payload, 17);
    let bvv = b.ins().sshr_imm(b_shl, 17);
    let cmp = fast_cmp(b, av, bvv); // i8 (icmp result) of 0/1.
                                    // Widen to i64 so we can or-in the NB_FALSE pattern.
    let cmp_w = b.ins().uextend(I64, cmp);
    let nb_false = NanboxValue::FALSE.into_raw();
    let result = b.ins().bor_imm(cmp_w, nb_false);
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(result)]);

    b.switch_to_block(slow_bb);
    b.seal_block(slow_bb);
    let call_slow = b.ins().call(slow_fnref, &[a, bv]);
    let r_slow = b.inst_results(call_slow)[0];
    b.ins()
        .jump(join_bb, &[cranelift_codegen::ir::BlockArg::Value(r_slow)]);

    b.switch_to_block(join_bb);
    b.seal_block(join_bb);
    b.block_params(join_bb)[0]
}

/// Encode an RIR `Const` to its `NanboxValue` bit pattern as an i64
/// at compile time. The lowering emits a single `iconst` of this
/// value, so non-Fixnum constants don't pay an encode cost at run
/// time. Returns `None` (TODO) for variants that require heap
/// allocation; the skeleton handles inline-encodable kinds only.
fn encode_const_as_nb(c: &Const) -> i64 {
    use cs_vm::vm::NanboxValue;
    match c {
        Const::Fixnum(n) => NanboxValue::fixnum(*n).into_raw(),
        Const::Flonum(f) => NanboxValue::flonum(*f).into_raw(),
        Const::Boolean(b) => NanboxValue::boolean(*b).into_raw(),
        Const::Character(ch) => NanboxValue::character(*ch).into_raw(),
        Const::Null => NanboxValue::NULL.into_raw(),
        Const::Unspecified => NanboxValue::UNSPECIFIED.into_raw(),
        Const::Eof => NanboxValue::EOF.into_raw(),
        Const::Symbol(id) => NanboxValue::symbol(cs_core::Symbol(*id)).into_raw(),
        // String constants would need a static-table lookup that
        // materializes a `Gc<String>` on first access. Skeleton
        // doesn't support these yet — the translator will route any
        // function containing `Const::StringRef` to the specialized
        // tier (or bytecode).
        Const::StringRef(_) => {
            // Sentinel: the caller in `lower_inst_uniform_nb` won't
            // reach this branch because the Inst::LoadConst arm would
            // need to fail-out before invoking us. Defensive: return
            // `Value::Unspecified` NB to keep the lowering honest if
            // a translator bug routes us here.
            NanboxValue::UNSPECIFIED.into_raw()
        }
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
        Term::Branch(cond, then_b, else_b, args) => {
            let cv = lookup(map, *cond)?;
            let tb = *block_map
                .get(then_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown then target {:?}", then_b)))?;
            let eb = *block_map
                .get(else_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown else target {:?}", else_b)))?;
            let cargs: Vec<cranelift_codegen::ir::BlockArg> = args
                .iter()
                .map(|a| lookup(map, *a).map(cranelift_codegen::ir::BlockArg::Value))
                .collect::<Result<_, _>>()?;
            b.ins().brif(cv, tb, &cargs, eb, &cargs);
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
    #[cfg(feature = "regions")] alloc_pair_region_fnref: cranelift_codegen::ir::FuncRef,
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
    equal_fnref: cranelift_codegen::ir::FuncRef,
    any_truthy_fnref: cranelift_codegen::ir::FuncRef,
    call_general_fnref: cranelift_codegen::ir::FuncRef,
    ic_dispatch_fnref: cranelift_codegen::ir::FuncRef,
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
    string_lt_fnref: cranelift_codegen::ir::FuncRef,
    string_gt_fnref: cranelift_codegen::ir::FuncRef,
    string_le_fnref: cranelift_codegen::ir::FuncRef,
    string_ge_fnref: cranelift_codegen::ir::FuncRef,
    string_ci_eq_fnref: cranelift_codegen::ir::FuncRef,
    string_ci_lt_fnref: cranelift_codegen::ir::FuncRef,
    string_ci_gt_fnref: cranelift_codegen::ir::FuncRef,
    string_ci_le_fnref: cranelift_codegen::ir::FuncRef,
    string_ci_ge_fnref: cranelift_codegen::ir::FuncRef,
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
    member_fnref: cranelift_codegen::ir::FuncRef,
    assoc_fnref: cranelift_codegen::ir::FuncRef,
    list_tail_fnref: cranelift_codegen::ir::FuncRef,
    list_ref_fnref: cranelift_codegen::ir::FuncRef,
    substring_fnref: cranelift_codegen::ir::FuncRef,
    list_copy_fnref: cranelift_codegen::ir::FuncRef,
    list_set_fnref: cranelift_codegen::ir::FuncRef,
    gcd_fnref: cranelift_codegen::ir::FuncRef,
    lcm_fnref: cranelift_codegen::ir::FuncRef,
    expt_fnref: cranelift_codegen::ir::FuncRef,
    arith_shift_fnref: cranelift_codegen::ir::FuncRef,
    bv_p_fnref: cranelift_codegen::ir::FuncRef,
    bv_length_fnref: cranelift_codegen::ir::FuncRef,
    bv_u8_ref_fnref: cranelift_codegen::ir::FuncRef,
    bv_alloc_fnref: cranelift_codegen::ir::FuncRef,
    bv_u8_set_fnref: cranelift_codegen::ir::FuncRef,
    vec_fill_fnref: cranelift_codegen::ir::FuncRef,
    bv_fill_fnref: cranelift_codegen::ir::FuncRef,
    str_set_fnref: cranelift_codegen::ir::FuncRef,
    str_fill_fnref: cranelift_codegen::ir::FuncRef,
    make_vector_buf_fnref: cranelift_codegen::ir::FuncRef,
    make_string_buf_fnref: cranelift_codegen::ir::FuncRef,
    make_bytevector_buf_fnref: cranelift_codegen::ir::FuncRef,
    string_append_buf_fnref: cranelift_codegen::ir::FuncRef,
    append_buf_fnref: cranelift_codegen::ir::FuncRef,
    vector_append_buf_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_append_buf_fnref: cranelift_codegen::ir::FuncRef,
    str_copy_fnref: cranelift_codegen::ir::FuncRef,
    vec_copy_fnref: cranelift_codegen::ir::FuncRef,
    bv_copy_fnref: cranelift_codegen::ir::FuncRef,
    procedure_p_fnref: cranelift_codegen::ir::FuncRef,
    port_p_fnref: cranelift_codegen::ir::FuncRef,
    eof_p_fnref: cranelift_codegen::ir::FuncRef,
    symbol_p_fnref: cranelift_codegen::ir::FuncRef,
    char_p_fnref: cranelift_codegen::ir::FuncRef,
    boolean_p_fnref: cranelift_codegen::ir::FuncRef,
    fixnum_p_fnref: cranelift_codegen::ir::FuncRef,
    flonum_p_fnref: cranelift_codegen::ir::FuncRef,
    flonum_sin_fnref: cranelift_codegen::ir::FuncRef,
    flonum_is_integer_fnref: cranelift_codegen::ir::FuncRef,
    flonum_cos_fnref: cranelift_codegen::ir::FuncRef,
    flonum_tan_fnref: cranelift_codegen::ir::FuncRef,
    flonum_log_fnref: cranelift_codegen::ir::FuncRef,
    flonum_exp_fnref: cranelift_codegen::ir::FuncRef,
    flonum_asin_fnref: cranelift_codegen::ir::FuncRef,
    flonum_acos_fnref: cranelift_codegen::ir::FuncRef,
    flonum_atan_fnref: cranelift_codegen::ir::FuncRef,
    flonum_log2_fnref: cranelift_codegen::ir::FuncRef,
    flonum_atan2_fnref: cranelift_codegen::ir::FuncRef,
    flonum_expt_fnref: cranelift_codegen::ir::FuncRef,
    fl_even_p_fnref: cranelift_codegen::ir::FuncRef,
    fl_odd_p_fnref: cranelift_codegen::ir::FuncRef,
    string_titlecase_fnref: cranelift_codegen::ir::FuncRef,
    string_hash_fnref: cranelift_codegen::ir::FuncRef,
    symbol_hash_fnref: cranelift_codegen::ir::FuncRef,
    input_port_p_fnref: cranelift_codegen::ir::FuncRef,
    output_port_p_fnref: cranelift_codegen::ir::FuncRef,
    binary_port_p_fnref: cranelift_codegen::ir::FuncRef,
    textual_port_p_fnref: cranelift_codegen::ir::FuncRef,
    output_port_open_p_fnref: cranelift_codegen::ir::FuncRef,
    port_eof_p_fnref: cranelift_codegen::ir::FuncRef,
    port_has_set_port_position_p_fnref: cranelift_codegen::ir::FuncRef,
    port_position_fnref: cranelift_codegen::ir::FuncRef,
    promise_p_fnref: cranelift_codegen::ir::FuncRef,
    div_euclid_fnref: cranelift_codegen::ir::FuncRef,
    mod_euclid_fnref: cranelift_codegen::ir::FuncRef,
    div0_fnref: cranelift_codegen::ir::FuncRef,
    mod0_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_hash_function_fnref: cranelift_codegen::ir::FuncRef,
    make_hashtable_equal_fnref: cranelift_codegen::ir::FuncRef,
    make_hashtable_eq_fnref: cranelift_codegen::ir::FuncRef,
    make_hashtable_eqv_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_p_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_size_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_mutable_p_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_keys_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_values_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_clear_fnref: cranelift_codegen::ir::FuncRef,
    equal_hash_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_to_alist_fnref: cranelift_codegen::ir::FuncRef,
    file_exists_p_fnref: cranelift_codegen::ir::FuncRef,
    current_second_fnref: cranelift_codegen::ir::FuncRef,
    current_jiffy_fnref: cranelift_codegen::ir::FuncRef,
    append_reverse_fnref: cranelift_codegen::ir::FuncRef,
    alist_copy_fnref: cranelift_codegen::ir::FuncRef,
    delete_fnref: cranelift_codegen::ir::FuncRef,
    delete_duplicates_fnref: cranelift_codegen::ir::FuncRef,
    make_promise_fnref: cranelift_codegen::ir::FuncRef,
    force_forced_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_contains_p_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_delete_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_set_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_ref_fnref: cranelift_codegen::ir::FuncRef,
    hashtable_copy_fnref: cranelift_codegen::ir::FuncRef,
    vector_copy_slice_fnref: cranelift_codegen::ir::FuncRef,
    vector_copy_from_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_copy_from_fnref: cranelift_codegen::ir::FuncRef,
    string_copy_from_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_fill_from_fnref: cranelift_codegen::ir::FuncRef,
    vector_fill_from_fnref: cranelift_codegen::ir::FuncRef,
    string_fill_from_fnref: cranelift_codegen::ir::FuncRef,
    vector_to_string_slice_fnref: cranelift_codegen::ir::FuncRef,
    string_to_vector_slice_fnref: cranelift_codegen::ir::FuncRef,
    vector_to_list_slice_fnref: cranelift_codegen::ir::FuncRef,
    string_to_list_slice_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_to_list_slice_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_copy_slice_fnref: cranelift_codegen::ir::FuncRef,
    eof_object_fnref: cranelift_codegen::ir::FuncRef,
    bitwise_bit_count_fnref: cranelift_codegen::ir::FuncRef,
    bitwise_length_fnref: cranelift_codegen::ir::FuncRef,
    bitwise_arith_shift_left_fnref: cranelift_codegen::ir::FuncRef,
    bitwise_arith_shift_right_fnref: cranelift_codegen::ir::FuncRef,
    bitwise_bit_set_p_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s8_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s8_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_u16_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s16_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_u16_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s16_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_u32_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s32_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_u32_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s32_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_single_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_double_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_single_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_double_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_u64_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s64_native_ref_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_u64_native_set_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_s64_native_set_fnref: cranelift_codegen::ir::FuncRef,
    fx_first_bit_set_fnref: cranelift_codegen::ir::FuncRef,
    char_alphabetic_p_fnref: cranelift_codegen::ir::FuncRef,
    char_numeric_p_fnref: cranelift_codegen::ir::FuncRef,
    char_whitespace_p_fnref: cranelift_codegen::ir::FuncRef,
    char_upcase_fnref: cranelift_codegen::ir::FuncRef,
    char_downcase_fnref: cranelift_codegen::ir::FuncRef,
    char_upper_case_p_fnref: cranelift_codegen::ir::FuncRef,
    char_lower_case_p_fnref: cranelift_codegen::ir::FuncRef,
    char_foldcase_fnref: cranelift_codegen::ir::FuncRef,
    char_titlecase_fnref: cranelift_codegen::ir::FuncRef,
    digit_value_fnref: cranelift_codegen::ir::FuncRef,
    vector_to_list_fnref: cranelift_codegen::ir::FuncRef,
    list_to_vector_fnref: cranelift_codegen::ir::FuncRef,
    string_to_list_fnref: cranelift_codegen::ir::FuncRef,
    list_to_string_fnref: cranelift_codegen::ir::FuncRef,
    string_to_vector_fnref: cranelift_codegen::ir::FuncRef,
    vector_to_string_fnref: cranelift_codegen::ir::FuncRef,
    number_to_string_fnref: cranelift_codegen::ir::FuncRef,
    number_to_string_radix_fnref: cranelift_codegen::ir::FuncRef,
    string_to_number_fnref: cranelift_codegen::ir::FuncRef,
    string_to_number_radix_fnref: cranelift_codegen::ir::FuncRef,
    make_list_unspec_fnref: cranelift_codegen::ir::FuncRef,
    make_vector_unspec_fnref: cranelift_codegen::ir::FuncRef,
    vector_to_list_slice_from_fnref: cranelift_codegen::ir::FuncRef,
    string_to_list_slice_from_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_to_list_slice_from_fnref: cranelift_codegen::ir::FuncRef,
    vector_to_string_slice_from_fnref: cranelift_codegen::ir::FuncRef,
    string_to_vector_slice_from_fnref: cranelift_codegen::ir::FuncRef,
    vector_copy_bang_from_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_copy_bang_from_fnref: cranelift_codegen::ir::FuncRef,
    string_copy_bang_from_fnref: cranelift_codegen::ir::FuncRef,
    vector_copy_bang_slice_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_copy_bang_slice_fnref: cranelift_codegen::ir::FuncRef,
    string_copy_bang_slice_fnref: cranelift_codegen::ir::FuncRef,
    string_reverse_fnref: cranelift_codegen::ir::FuncRef,
    string_upcase_fnref: cranelift_codegen::ir::FuncRef,
    string_downcase_fnref: cranelift_codegen::ir::FuncRef,
    string_foldcase_fnref: cranelift_codegen::ir::FuncRef,
    string_contains_fnref: cranelift_codegen::ir::FuncRef,
    string_prefix_p_fnref: cranelift_codegen::ir::FuncRef,
    string_suffix_p_fnref: cranelift_codegen::ir::FuncRef,
    string_join_fnref: cranelift_codegen::ir::FuncRef,
    string_split_fnref: cranelift_codegen::ir::FuncRef,
    string_pad_fnref: cranelift_codegen::ir::FuncRef,
    string_pad_right_fnref: cranelift_codegen::ir::FuncRef,
    string_trim_fnref: cranelift_codegen::ir::FuncRef,
    string_trim_left_fnref: cranelift_codegen::ir::FuncRef,
    string_trim_right_fnref: cranelift_codegen::ir::FuncRef,
    string_replace_all_fnref: cranelift_codegen::ir::FuncRef,
    string_replace_first_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_fill_slice_fnref: cranelift_codegen::ir::FuncRef,
    vector_fill_slice_fnref: cranelift_codegen::ir::FuncRef,
    string_fill_slice_fnref: cranelift_codegen::ir::FuncRef,
    exact_nonneg_int_p_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_eq_p_fnref: cranelift_codegen::ir::FuncRef,
    vector_eq_p_fnref: cranelift_codegen::ir::FuncRef,
    string_take_fnref: cranelift_codegen::ir::FuncRef,
    string_drop_fnref: cranelift_codegen::ir::FuncRef,
    string_take_right_fnref: cranelift_codegen::ir::FuncRef,
    string_drop_right_fnref: cranelift_codegen::ir::FuncRef,
    string_contains_right_fnref: cranelift_codegen::ir::FuncRef,
    string_index_fnref: cranelift_codegen::ir::FuncRef,
    string_index_right_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_to_u8_list_fnref: cranelift_codegen::ir::FuncRef,
    u8_list_to_bytevector_fnref: cranelift_codegen::ir::FuncRef,
    string_to_utf8_fnref: cranelift_codegen::ir::FuncRef,
    utf8_to_string_fnref: cranelift_codegen::ir::FuncRef,
    make_list_fill_fnref: cranelift_codegen::ir::FuncRef,
    iota_n_fnref: cranelift_codegen::ir::FuncRef,
    iota_ns_fnref: cranelift_codegen::ir::FuncRef,
    iota_nss_fnref: cranelift_codegen::ir::FuncRef,
    last_pair_fnref: cranelift_codegen::ir::FuncRef,
    last_fnref: cranelift_codegen::ir::FuncRef,
    take_fnref: cranelift_codegen::ir::FuncRef,
    drop_fnref: cranelift_codegen::ir::FuncRef,
    null_list_p_fnref: cranelift_codegen::ir::FuncRef,
    proper_list_p_fnref: cranelift_codegen::ir::FuncRef,
    dotted_list_p_fnref: cranelift_codegen::ir::FuncRef,
    circular_list_p_fnref: cranelift_codegen::ir::FuncRef,
    concatenate_fnref: cranelift_codegen::ir::FuncRef,
    not_pair_p_fnref: cranelift_codegen::ir::FuncRef,
    vector_copy_bang_fnref: cranelift_codegen::ir::FuncRef,
    bytevector_copy_bang_fnref: cranelift_codegen::ir::FuncRef,
    string_copy_bang_fnref: cranelift_codegen::ir::FuncRef,
    symbol_to_string_fnref: cranelift_codegen::ir::FuncRef,
    string_to_symbol_fnref: cranelift_codegen::ir::FuncRef,
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
        // Phase 5b iter7 — Specialized tier can't safely inline Div
        // (Fixnum/Fixnum may return Rational). Reject so the body
        // falls back to bytecode for this RIR.
        Inst::Div(_, _, _) => {
            return Err(JitError::Unsupported(
                "specialized: Inst::Div not supported (use uniform-NB tier)".into(),
            ));
        }
        Inst::EnvDefineLocal(_, _) => {
            return Err(JitError::Unsupported(
                "specialized: Inst::EnvDefineLocal not supported (use uniform-NB tier)".into(),
            ));
        }
        Inst::Quotient(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().sdiv(l, r))?
        }
        Inst::Remainder(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().srem(l, r))?
        }
        Inst::Modulo(dst, lhs, rhs) => {
            // ADR 0012 D-2 (iter CL) — R6RS modulo: result has the
            // sign of the divisor. Compute as srem, then add the
            // divisor if remainder is non-zero and has a different
            // sign than the divisor.
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            let rem = b.ins().srem(l, r);
            let zero = b.ins().iconst(I64, 0);
            let rem_nonzero =
                b.ins()
                    .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, rem, zero);
            let rem_neg = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                rem,
                zero,
            );
            let div_neg = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                r,
                zero,
            );
            let sign_diff = b.ins().bxor(rem_neg, div_neg);
            let adjust = b.ins().band(rem_nonzero, sign_diff);
            let adjusted = b.ins().iadd(rem, r);
            let result = b.ins().select(adjust, adjusted, rem);
            map.insert(*dst, result);
        }
        Inst::FloorQuotient(dst, lhs, rhs) => {
            // ADR 0012 D-2 (iter ED) — R7RS floor-quotient: sdiv
            // truncates toward zero, but floor rounds toward
            // negative infinity. Adjust by -1 when the remainder is
            // nonzero and the signs of lhs/rhs differ.
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            let q = b.ins().sdiv(l, r);
            let rem = b.ins().srem(l, r);
            let zero = b.ins().iconst(I64, 0);
            let rem_nonzero =
                b.ins()
                    .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, rem, zero);
            let lhs_neg = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                l,
                zero,
            );
            let rhs_neg = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                r,
                zero,
            );
            let signs_differ = b.ins().bxor(lhs_neg, rhs_neg);
            let adjust = b.ins().band(rem_nonzero, signs_differ);
            let one = b.ins().iconst(I64, 1);
            let q_minus_one = b.ins().isub(q, one);
            let result = b.ins().select(adjust, q_minus_one, q);
            map.insert(*dst, result);
        }
        Inst::BitAnd(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().band(l, r))?
        }
        Inst::BitOr(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().bor(l, r))?,
        Inst::BitXor(dst, lhs, rhs) => {
            binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().bxor(l, r))?
        }
        Inst::BitNot(dst, src) => {
            // ADR 0012 D-2 (iter DK) — Cranelift native bnot.
            let s = lookup(map, *src)?;
            let v = b.ins().bnot(s);
            map.insert(*dst, v);
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
        // RC3 iter 2.14 — boolean negation. JIT booleans are i64 0/1;
        // xor with 1 flips. The AOT path emits the same `v ^ 1`
        // (which also flips NB_TRUE ↔ NB_FALSE since they differ
        // only in bit 0).
        Inst::NotBoolean(dst, src) => {
            let v = lookup(map, *src)?;
            let flipped = b.ins().bxor_imm(v, 1);
            map.insert(*dst, flipped);
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
        #[cfg(feature = "regions")]
        Inst::ConsRegion(dst, car, car_tag, cdr, cdr_tag) => {
            // Layer 3 — region-allocating cons. Same shape as
            // Inst::Cons but calls vm_alloc_pair_region (resolves
            // to cs-vm's vm_alloc_pair_region_gc). The runtime
            // helper consults the per-thread region-resolver to
            // pick the current `cs_gc::Region`, falling back to
            // Rc allocation if no region is in scope. The
            // returned i64 carries the low-bit Region-tagged
            // nanbox payload so the VM decoder can route through
            // `Gc::from_raw_jit_region`.
            let car_v = lookup(map, *car)?;
            let cdr_v = lookup(map, *cdr)?;
            let car_t = b.ins().iconst(I64, *car_tag as i64);
            let cdr_t = b.ins().iconst(I64, *cdr_tag as i64);
            let inst_ref = b
                .ins()
                .call(alloc_pair_region_fnref, &[car_v, car_t, cdr_v, cdr_t]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ConsRegion expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        #[cfg(not(feature = "regions"))]
        Inst::ConsRegion(_, _, _, _, _) => {
            return Err(JitError::Codegen(
                "Inst::ConsRegion encountered but `regions` feature is disabled".into(),
            ));
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
        Inst::EqualAny(dst, lhs, rhs) => {
            // ADR 0012 D-2 (iter DZ) — vm_equal_gc deep equality.
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            let inst_ref = b.ins().call(equal_fnref, &[l, r]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "EqualAny expected 1 result, got {}",
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
        Inst::FlonumIsNan(dst, src) => {
            // ADR 0012 D-2 (iter EF) — NaN ↔ unordered with itself.
            let s_i = lookup(map, *src)?;
            let mf = cranelift_codegen::ir::MemFlags::new();
            let s_f = b.ins().bitcast(F64, mf, s_i);
            let cmp = b.ins().fcmp(
                cranelift_codegen::ir::condcodes::FloatCC::Unordered,
                s_f,
                s_f,
            );
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::FlonumIsInfinite(dst, src) => {
            // ADR 0012 D-2 (iter EF) — abs(x) == +∞.
            let s_i = lookup(map, *src)?;
            let mf = cranelift_codegen::ir::MemFlags::new();
            let s_f = b.ins().bitcast(F64, mf, s_i);
            let abs = b.ins().fabs(s_f);
            let inf = b.ins().f64const(f64::INFINITY);
            let cmp = b
                .ins()
                .fcmp(cranelift_codegen::ir::condcodes::FloatCC::Equal, abs, inf);
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::FlonumIsFinite(dst, src) => {
            // ADR 0012 D-2 (iter EF) — abs(x) < +∞. Ordered LT
            // rejects NaN (returns false) and ±∞ (returns false),
            // covering "finite" in one comparison.
            let s_i = lookup(map, *src)?;
            let mf = cranelift_codegen::ir::MemFlags::new();
            let s_f = b.ins().bitcast(F64, mf, s_i);
            let abs = b.ins().fabs(s_f);
            let inf = b.ins().f64const(f64::INFINITY);
            let cmp = b.ins().fcmp(
                cranelift_codegen::ir::condcodes::FloatCC::LessThan,
                abs,
                inf,
            );
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::FlonumIsInteger(dst, src) => {
            // ADR 0012 D-2 (iter EH) — call vm_flonum_is_integer
            // which checks `x.is_finite() && x.fract() == 0.0`.
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_is_integer_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumIsInteger expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumSin(dst, src) => {
            // ADR 0012 D-2 (iter DF) — Cranelift has no native sin;
            // helper takes/returns i64-encoded f64.
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_sin_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumSin expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumCos(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_cos_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumCos expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumTan(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_tan_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumTan expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumLog(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_log_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumLog expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumExp(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_exp_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumExp expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumAsin(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_asin_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumAsin expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumAcos(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_acos_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumAcos expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::MakeHashtableEqual(dst)
        | Inst::MakeHashtableEq(dst)
        | Inst::MakeHashtableEqv(dst) => {
            // ADR 0012 D-2 (iter HR/HS) — make-hashtable* 0-arg.
            let fnref = match inst {
                Inst::MakeHashtableEqual(..) => make_hashtable_equal_fnref,
                Inst::MakeHashtableEq(..) => make_hashtable_eq_fnref,
                Inst::MakeHashtableEqv(..) => make_hashtable_eqv_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "MakeHashtable* expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::HashtableHashFn(dst, src) => {
            // ADR 0012 D-2 (iter HQ) — hashtable-hash-function. Returns
            // a Gc handle to the proc (Custom) or #f (Eq/Eqv/Equal).
            // Deopts on non-hashtable.
            let sv = lookup(map, *src)?;
            let inst_ref = b.ins().call(hashtable_hash_function_fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "HashtableHashFn expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::DivEuclid(dst, x, y)
        | Inst::ModEuclid(dst, x, y)
        | Inst::Div0(dst, x, y)
        | Inst::Mod0(dst, x, y) => {
            // ADR 0012 D-2 (iter GE / HO) — R6RS div/mod and div0/mod0.
            let xv = lookup(map, *x)?;
            let yv = lookup(map, *y)?;
            let fnref = match inst {
                Inst::DivEuclid(..) => div_euclid_fnref,
                Inst::ModEuclid(..) => mod_euclid_fnref,
                Inst::Div0(..) => div0_fnref,
                Inst::Mod0(..) => mod0_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[xv, yv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "DivEuclid/ModEuclid/Div0/Mod0 expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BitwiseArithShiftLeft(dst, n, count)
        | Inst::BitwiseArithShiftRight(dst, n, count)
        | Inst::BitwiseBitSetP(dst, n, count) => {
            // ADR 0012 D-2 (iter FO) — bitwise shift-left/-right + bit-set?.
            let nv = lookup(map, *n)?;
            let cv = lookup(map, *count)?;
            let fnref = match inst {
                Inst::BitwiseArithShiftLeft(..) => bitwise_arith_shift_left_fnref,
                Inst::BitwiseArithShiftRight(..) => bitwise_arith_shift_right_fnref,
                Inst::BitwiseBitSetP(..) => bitwise_bit_set_p_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[nv, cv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BitwiseShift/BitSetP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::Delete(dst, target, lst) => {
            // ADR 0012 D-2 (iter GS) — delete (2-arg).
            let tv = lookup(map, *target)?;
            let lv = lookup(map, *lst)?;
            let inst_ref = b.ins().call(delete_fnref, &[tv, lv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Delete expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::AlistCopy(dst, src)
        | Inst::DeleteDuplicates(dst, src)
        | Inst::MakePromise(dst, src)
        | Inst::ForceForced(dst, src) => {
            // ADR 0012 D-2 (iter GO/GS/GT/GU) — 1-arg ops returning fresh Gc handles.
            let sv = lookup(map, *src)?;
            let fnref = match inst {
                Inst::AlistCopy(..) => alist_copy_fnref,
                Inst::DeleteDuplicates(..) => delete_duplicates_fnref,
                Inst::MakePromise(..) => make_promise_fnref,
                Inst::ForceForced(..) => force_forced_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "AlistCopy/DeleteDuplicates expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::AppendReverse(dst, rev, tail) => {
            // ADR 0012 D-2 (iter GN) — append-reverse.
            let rv = lookup(map, *rev)?;
            let tv = lookup(map, *tail)?;
            let inst_ref = b.ins().call(append_reverse_fnref, &[rv, tv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "AppendReverse expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::CurrentSecond(dst) | Inst::CurrentJiffy(dst) => {
            // ADR 0012 D-2 (iter GL) — 0-arg time helpers.
            let fnref = match inst {
                Inst::CurrentSecond(..) => current_second_fnref,
                Inst::CurrentJiffy(..) => current_jiffy_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CurrentSecond/CurrentJiffy expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FileExistsP(dst, src) => {
            // ADR 0012 D-2 (iter GK) — file-exists?. Returns raw 0/1.
            let sv = lookup(map, *src)?;
            let inst_ref = b.ins().call(file_exists_p_fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FileExistsP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::HashtableContainsP(dst, ht, key) => {
            // ADR 0012 D-2 (iter GV) — hashtable-contains?. Returns raw 0/1.
            // Deopts on non-hashtable or Custom eq_kind.
            let htv = lookup(map, *ht)?;
            let kv = lookup(map, *key)?;
            let inst_ref = b.ins().call(hashtable_contains_p_fnref, &[htv, kv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "HashtableContainsP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::HashtableDelete(dst, ht, key) => {
            // ADR 0012 D-2 (iter GW) — hashtable-delete!. Returns a Gc
            // handle to Unspecified. Deopts on non-hashtable or Custom
            // eq_kind.
            let htv = lookup(map, *ht)?;
            let kv = lookup(map, *key)?;
            let inst_ref = b.ins().call(hashtable_delete_fnref, &[htv, kv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "HashtableDelete expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::HashtableSet(dst, ht, key, val) => {
            // ADR 0012 D-2 (iter GX) — hashtable-set!. Returns a Gc
            // handle to Unspecified. Deopts on non-hashtable or Custom
            // eq_kind.
            let htv = lookup(map, *ht)?;
            let kv = lookup(map, *key)?;
            let vv = lookup(map, *val)?;
            let inst_ref = b.ins().call(hashtable_set_fnref, &[htv, kv, vv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "HashtableSet expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::EofObject(dst) => {
            // ADR 0012 D-2 (iter HD) — eof-object constructor. 0-arg.
            // No deopt; returns a fresh Gc handle to Value::Eof.
            let inst_ref = b.ins().call(eof_object_fnref, &[]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "EofObject expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvCopySlice(dst, bv, start, end) => {
            // ADR 0012 D-2 (iter HC) — bytevector-copy 3-arg slice.
            // Returns fresh bytevector. Deopts on non-bytevector or
            // out-of-range.
            let bvv = lookup(map, *bv)?;
            let sv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(bytevector_copy_slice_fnref, &[bvv, sv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvCopySlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecCopyFrom(dst, vec, start) => {
            // ADR 0012 D-2 (iter HT) — vector-copy 2-arg slice-to-end.
            let vv = lookup(map, *vec)?;
            let sv = lookup(map, *start)?;
            let inst_ref = b.ins().call(vector_copy_from_fnref, &[vv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecCopyFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvCopyFrom(dst, bv, start) => {
            // ADR 0012 D-2 (iter HU) — bytevector-copy 2-arg slice-to-end.
            let bvv = lookup(map, *bv)?;
            let sv = lookup(map, *start)?;
            let inst_ref = b.ins().call(bytevector_copy_from_fnref, &[bvv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvCopyFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvFillFrom(dst, bv, fill, start) => {
            // ADR 0012 D-2 (iter IA) — bytevector-fill! 3-arg fill-from.
            let bvv = lookup(map, *bv)?;
            let fv = lookup(map, *fill)?;
            let sv = lookup(map, *start)?;
            let inst_ref = b.ins().call(bytevector_fill_from_fnref, &[bvv, fv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvFillFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecFillFrom(dst, vec, fill, start) => {
            // ADR 0012 D-2 (iter IB) — vector-fill! 3-arg fill-from.
            let vv = lookup(map, *vec)?;
            let fv = lookup(map, *fill)?;
            let sv = lookup(map, *start)?;
            let inst_ref = b.ins().call(vector_fill_from_fnref, &[vv, fv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecFillFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VectorToStringSlice(dst, vec, start, end) => {
            // ADR 0012 D-2 (iter ID) — vector->string 3-arg slice.
            let vv = lookup(map, *vec)?;
            let sv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(vector_to_string_slice_fnref, &[vv, sv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorToStringSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToVectorSlice(dst, s, start, end) => {
            // ADR 0012 D-2 (iter IE) — string->vector 3-arg slice.
            let sv = lookup(map, *s)?;
            let stv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(string_to_vector_slice_fnref, &[sv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToVectorSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToListSlice(dst, s, start, end) => {
            // ADR 0012 D-2 (iter IG) — string->list 3-arg slice.
            let sv = lookup(map, *s)?;
            let stv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(string_to_list_slice_fnref, &[sv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToListSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BytevectorToListSlice(dst, bv, start, end) => {
            // ADR 0012 D-2 (iter IH) — bytevector->list 3-arg slice.
            let bvv = lookup(map, *bv)?;
            let stv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b
                .ins()
                .call(bytevector_to_list_slice_fnref, &[bvv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BytevectorToListSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VectorToListSlice(dst, vec, start, end) => {
            // ADR 0012 D-2 (iter IF) — vector->list 3-arg slice.
            let vv = lookup(map, *vec)?;
            let sv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(vector_to_list_slice_fnref, &[vv, sv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorToListSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrFillFrom(dst, s, ch, start) => {
            // ADR 0012 D-2 (iter IC) — string-fill! 3-arg fill-from.
            let sv = lookup(map, *s)?;
            let cv = lookup(map, *ch)?;
            let stv = lookup(map, *start)?;
            let inst_ref = b.ins().call(string_fill_from_fnref, &[sv, cv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrFillFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrCopyFrom(dst, s, start) => {
            // ADR 0012 D-2 (iter HV) — string-copy 2-arg slice-to-end.
            let sv = lookup(map, *s)?;
            let stv = lookup(map, *start)?;
            let inst_ref = b.ins().call(string_copy_from_fnref, &[sv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrCopyFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecCopySlice(dst, vec, start, end) => {
            // ADR 0012 D-2 (iter HA) — vector-copy 3-arg slice.
            // Returns fresh vector. Deopts on non-vector or out-of-range.
            let vv = lookup(map, *vec)?;
            let sv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(vector_copy_slice_fnref, &[vv, sv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecCopySlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::HashtableCopy(dst, src) => {
            // ADR 0012 D-2 (iter GZ) — hashtable-copy. Returns a fresh
            // hashtable Gc handle. Deopts on non-hashtable.
            let sv = lookup(map, *src)?;
            let inst_ref = b.ins().call(hashtable_copy_fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "HashtableCopy expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::HashtableRef(dst, ht, key, default) => {
            // ADR 0012 D-2 (iter GY) — hashtable-ref. Returns matching
            // value or default. Deopts on non-hashtable or Custom eq_kind.
            let htv = lookup(map, *ht)?;
            let kv = lookup(map, *key)?;
            let dv = lookup(map, *default)?;
            let inst_ref = b.ins().call(hashtable_ref_fnref, &[htv, kv, dv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "HashtableRef expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::EqualHash(dst, src) => {
            // ADR 0012 D-2 (iter GJ) — equal-hash returns raw Fixnum.
            let sv = lookup(map, *src)?;
            let inst_ref = b.ins().call(equal_hash_fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "EqualHash expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StringTitlecase(dst, src)
        | Inst::HashtableKeys(dst, src)
        | Inst::HashtableValues(dst, src)
        | Inst::HashtableClear(dst, src)
        | Inst::HashtableToAlist(dst, src) => {
            // ADR 0012 D-2 (iter GB/GH/GI/GJ) — 1-arg ops returning
            // fresh Gc handles.
            let sv = lookup(map, *src)?;
            let fnref = match inst {
                Inst::StringTitlecase(..) => string_titlecase_fnref,
                Inst::HashtableKeys(..) => hashtable_keys_fnref,
                Inst::HashtableValues(..) => hashtable_values_fnref,
                Inst::HashtableClear(..) => hashtable_clear_fnref,
                Inst::HashtableToAlist(..) => hashtable_to_alist_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringTitlecase/Hashtable{{Keys,Values}} expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringHash(dst, src) | Inst::SymbolHash(dst, src) => {
            // ADR 0012 D-2 (iter GB) — Fixnum-returning hashes (no Gc).
            let sv = lookup(map, *src)?;
            let fnref = match inst {
                Inst::StringHash(..) => string_hash_fnref,
                Inst::SymbolHash(..) => symbol_hash_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "String/SymbolHash expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BitwiseBitCount(dst, src)
        | Inst::BitwiseLength(dst, src)
        | Inst::FxFirstBitSet(dst, src)
        | Inst::FlEvenP(dst, src)
        | Inst::FlOddP(dst, src) => {
            // ADR 0012 D-2 (iter FN/FX/GA) — 1-arg numeric helpers
            // (bit-count/-length, trailing-zeros, fl parity).
            let sv = lookup(map, *src)?;
            let fnref = match inst {
                Inst::BitwiseBitCount(..) => bitwise_bit_count_fnref,
                Inst::BitwiseLength(..) => bitwise_length_fnref,
                Inst::FxFirstBitSet(..) => fx_first_bit_set_fnref,
                Inst::FlEvenP(..) => fl_even_p_fnref,
                Inst::FlOddP(..) => fl_odd_p_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Bitwise/FxFirstBitSet/Fl{{Even,Odd}}P expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumLog2(dst, n, base)
        | Inst::FlonumAtan2(dst, n, base)
        | Inst::FlonumExpt(dst, n, base) => {
            // ADR 0012 D-2 (iter FM/GA) — log/atan/expt 2-arg.
            let nv = lookup(map, *n)?;
            let bv = lookup(map, *base)?;
            let fnref = match inst {
                Inst::FlonumLog2(..) => flonum_log2_fnref,
                Inst::FlonumAtan2(..) => flonum_atan2_fnref,
                Inst::FlonumExpt(..) => flonum_expt_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[nv, bv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumLog2/Atan2 expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumAtan(dst, src) => {
            let s = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_atan_fnref, &[s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumAtan expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
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
            // ADR 0012 D-1 (iter JI) — route the hit through
            // `vm_ic_dispatch` rather than a bare `call_indirect`.
            // A bare indirect call ran the cached body *without* the
            // env / bytecode / stack-map-frame TLS guards that
            // `try_dispatch_jit` installs — so a callee that did an
            // `EnvLookup`, built a nested closure, or triggered GC
            // misbehaved or crashed. `vm_ic_dispatch` installs those
            // guards, runs the body, decodes the return per the
            // callee's `jit_return_type`, and handles a mid-body
            // deopt. It takes the callee handle (consumed there) and
            // the same args buffer + count the miss path uses.
            //
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
            // Pass slot_addr too so `vm_ic_dispatch` can read the
            // slot's `cached_param_types` to unbox each Any-handle arg
            // into the typed lane the cached body expects. Without
            // this, the IC could only cache all-Any callees (the
            // jit_param_types_all_any gate in vm_call_general); now
            // typed-param callees (n-queens' `safe?`, with hints like
            // (Fixnum, Fixnum, Any)) hit the IC fast path too.
            let hit_inst = b.ins().call(
                ic_dispatch_fnref,
                &[callee_v, buf_addr, n_args_v, cached_jit_ptr_v, slot_addr_v],
            );
            let hit_result = {
                let rs = b.inst_results(hit_inst);
                if rs.len() != 1 {
                    return Err(JitError::Codegen("vm_ic_dispatch expected 1 result".into()));
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
        // ADR 0012 D-2 (iter DW) — ordered string comparisons.
        // ADR 0012 D-2 (iter DX) — string-ci comparisons.
        Inst::StrLt(dst, a, b_op)
        | Inst::StrGt(dst, a, b_op)
        | Inst::StrLe(dst, a, b_op)
        | Inst::StrGe(dst, a, b_op)
        | Inst::StrCiEq(dst, a, b_op)
        | Inst::StrCiLt(dst, a, b_op)
        | Inst::StrCiGt(dst, a, b_op)
        | Inst::StrCiLe(dst, a, b_op)
        | Inst::StrCiGe(dst, a, b_op) => {
            let a_v = lookup(map, *a)?;
            let b_v = lookup(map, *b_op)?;
            let fnref = match inst {
                Inst::StrLt(..) => string_lt_fnref,
                Inst::StrGt(..) => string_gt_fnref,
                Inst::StrLe(..) => string_le_fnref,
                Inst::StrGe(..) => string_ge_fnref,
                Inst::StrCiEq(..) => string_ci_eq_fnref,
                Inst::StrCiLt(..) => string_ci_lt_fnref,
                Inst::StrCiGt(..) => string_ci_gt_fnref,
                Inst::StrCiLe(..) => string_ci_le_fnref,
                Inst::StrCiGe(..) => string_ci_ge_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[a_v, b_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrCmp expected 1 result, got {}",
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
        Inst::Member(dst, item, lst) => {
            // ADR 0012 D-2 (iter CH) — vm_member_gc; equal?-flavored.
            let item_v = lookup(map, *item)?;
            let lst_v = lookup(map, *lst)?;
            let inst_ref = b.ins().call(member_fnref, &[item_v, lst_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Member expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Assoc(dst, key, alist) => {
            // ADR 0012 D-2 (iter CH) — vm_assoc_gc; equal?-flavored.
            let key_v = lookup(map, *key)?;
            let alist_v = lookup(map, *alist)?;
            let inst_ref = b.ins().call(assoc_fnref, &[key_v, alist_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Assoc expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListTail(dst, lst, n) => {
            // ADR 0012 D-2 (iter CK) — vm_list_tail_gc. Consumes lst,
            // returns Gc handle.
            let lst_v = lookup(map, *lst)?;
            let n_v = lookup(map, *n)?;
            let inst_ref = b.ins().call(list_tail_fnref, &[lst_v, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListTail expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListRef(dst, lst, n) => {
            // ADR 0012 D-2 (iter CK) — vm_list_ref_gc.
            let lst_v = lookup(map, *lst)?;
            let n_v = lookup(map, *n)?;
            let inst_ref = b.ins().call(list_ref_fnref, &[lst_v, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListRef expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Substring(dst, s, start, end) => {
            // ADR 0012 D-2 (iter CM) — vm_substring_gc consumes the
            // string handle, returns a fresh Gc<Value::String>. Mark
            // for stack-map tracking.
            let s_v = lookup(map, *s)?;
            let start_v = lookup(map, *start)?;
            let end_v = lookup(map, *end)?;
            let inst_ref = b.ins().call(substring_fnref, &[s_v, start_v, end_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Substring expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListCopy(dst, lst) => {
            // ADR 0012 D-2 (iter CN) — vm_list_copy_gc consumes the
            // handle, returns a fresh Gc handle to a freshly-spined
            // chain (or the atom unchanged for non-list input).
            let lst_v = lookup(map, *lst)?;
            let inst_ref = b.ins().call(list_copy_fnref, &[lst_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListCopy expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListSet(dst, lst, n, val) => {
            // ADR 0012 D-2 (iter CO) — vm_list_set_gc consumes lst
            // and val, returns Gc(Unspecified). Mark for stack-map
            // tracking even though the result is Unspecified — it's
            // still a Gc handle until the dispatcher decodes it.
            let lst_v = lookup(map, *lst)?;
            let n_v = lookup(map, *n)?;
            let val_v = lookup(map, *val)?;
            let inst_ref = b.ins().call(list_set_fnref, &[lst_v, n_v, val_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListSet expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Gcd(dst, a, b_op) => {
            // ADR 0012 D-2 (iter CP) — vm_gcd_fx. Raw Fixnum
            // operands and result; no stack-map declaration.
            let a_v = lookup(map, *a)?;
            let b_v = lookup(map, *b_op)?;
            let inst_ref = b.ins().call(gcd_fnref, &[a_v, b_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Gcd expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::Lcm(dst, a, b_op) => {
            // ADR 0012 D-2 (iter CP) — vm_lcm_fx; mirrors Gcd.
            let a_v = lookup(map, *a)?;
            let b_v = lookup(map, *b_op)?;
            let inst_ref = b.ins().call(lcm_fnref, &[a_v, b_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Lcm expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::Expt(dst, base, exp) => {
            // ADR 0012 D-2 (iter CT) — vm_expt_fx. Fixnum operands
            // and result; helper deopts on overflow / neg exp.
            let base_v = lookup(map, *base)?;
            let exp_v = lookup(map, *exp)?;
            let inst_ref = b.ins().call(expt_fnref, &[base_v, exp_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Expt expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::ArithShift(dst, n, count) => {
            // ADR 0012 D-2 (iter DL) — vm_arith_shift_fx.
            let n_v = lookup(map, *n)?;
            let count_v = lookup(map, *count)?;
            let inst_ref = b.ins().call(arith_shift_fnref, &[n_v, count_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ArithShift expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BvP(dst, src) => {
            // ADR 0012 D-2 (iter CQ) — vm_bytevector_p_gc. Boolean.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(bv_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BvLength(dst, src) => {
            // ADR 0012 D-2 (iter CQ) — vm_bytevector_length_gc. Fixnum.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(bv_length_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvLength expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BvU8Ref(dst, bv, k) => {
            // ADR 0012 D-2 (iter CQ) — vm_bytevector_u8_ref_gc. Fixnum.
            let bv_v = lookup(map, *bv)?;
            let k_v = lookup(map, *k)?;
            let inst_ref = b.ins().call(bv_u8_ref_fnref, &[bv_v, k_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvU8Ref expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BvU16NativeRef(dst, bv, k)
        | Inst::BvS16NativeRef(dst, bv, k)
        | Inst::BvU32NativeRef(dst, bv, k)
        | Inst::BvS32NativeRef(dst, bv, k)
        | Inst::BvIeeeSingleNativeRef(dst, bv, k)
        | Inst::BvIeeeDoubleNativeRef(dst, bv, k)
        | Inst::BvU64NativeRef(dst, bv, k)
        | Inst::BvS64NativeRef(dst, bv, k) => {
            // ADR 0012 D-2 (iter FQ/FR/FS/FT) — bytevector typed native-ref.
            let bv_v = lookup(map, *bv)?;
            let k_v = lookup(map, *k)?;
            let fnref = match inst {
                Inst::BvU16NativeRef(..) => bytevector_u16_native_ref_fnref,
                Inst::BvS16NativeRef(..) => bytevector_s16_native_ref_fnref,
                Inst::BvU32NativeRef(..) => bytevector_u32_native_ref_fnref,
                Inst::BvS32NativeRef(..) => bytevector_s32_native_ref_fnref,
                Inst::BvIeeeSingleNativeRef(..) => bytevector_ieee_single_native_ref_fnref,
                Inst::BvIeeeDoubleNativeRef(..) => bytevector_ieee_double_native_ref_fnref,
                Inst::BvU64NativeRef(..) => bytevector_u64_native_ref_fnref,
                Inst::BvS64NativeRef(..) => bytevector_s64_native_ref_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[bv_v, k_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Bv{{U,S}}16NativeRef expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BvU16NativeSet(dst, bv, k, val)
        | Inst::BvS16NativeSet(dst, bv, k, val)
        | Inst::BvU32NativeSet(dst, bv, k, val)
        | Inst::BvS32NativeSet(dst, bv, k, val)
        | Inst::BvIeeeSingleNativeSet(dst, bv, k, val)
        | Inst::BvIeeeDoubleNativeSet(dst, bv, k, val)
        | Inst::BvU64NativeSet(dst, bv, k, val)
        | Inst::BvS64NativeSet(dst, bv, k, val) => {
            // ADR 0012 D-2 (iter FQ/FR/FS/FT) — bytevector typed native-set!.
            let bv_v = lookup(map, *bv)?;
            let k_v = lookup(map, *k)?;
            let val_v = lookup(map, *val)?;
            let fnref = match inst {
                Inst::BvU16NativeSet(..) => bytevector_u16_native_set_fnref,
                Inst::BvS16NativeSet(..) => bytevector_s16_native_set_fnref,
                Inst::BvU32NativeSet(..) => bytevector_u32_native_set_fnref,
                Inst::BvS32NativeSet(..) => bytevector_s32_native_set_fnref,
                Inst::BvIeeeSingleNativeSet(..) => bytevector_ieee_single_native_set_fnref,
                Inst::BvIeeeDoubleNativeSet(..) => bytevector_ieee_double_native_set_fnref,
                Inst::BvU64NativeSet(..) => bytevector_u64_native_set_fnref,
                Inst::BvS64NativeSet(..) => bytevector_s64_native_set_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[bv_v, k_v, val_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Bv{{U,S}}16NativeSet expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvS8Ref(dst, bv, k) => {
            // ADR 0012 D-2 (iter FP) — vm_bytevector_s8_ref_gc. Fixnum
            // (sign-extended).
            let bv_v = lookup(map, *bv)?;
            let k_v = lookup(map, *k)?;
            let inst_ref = b.ins().call(bytevector_s8_ref_fnref, &[bv_v, k_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvS8Ref expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BvS8Set(dst, bv, k, val) => {
            // ADR 0012 D-2 (iter FP) — vm_bytevector_s8_set_gc.
            // Returns Gc(Unspecified).
            let bv_v = lookup(map, *bv)?;
            let k_v = lookup(map, *k)?;
            let val_v = lookup(map, *val)?;
            let inst_ref = b.ins().call(bytevector_s8_set_fnref, &[bv_v, k_v, val_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvS8Set expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvAlloc(dst, n, fill) => {
            // ADR 0012 D-2 (iter CR) — vm_alloc_bytevector_gc. Returns
            // a fresh Gc<Value::ByteVector>; mark for stack-map tracking.
            let n_v = lookup(map, *n)?;
            let fill_v = lookup(map, *fill)?;
            let inst_ref = b.ins().call(bv_alloc_fnref, &[n_v, fill_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvAlloc expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvU8Set(dst, bv, k, val) => {
            // ADR 0012 D-2 (iter CR) — vm_bytevector_u8_set_gc.
            // Consumes bv, returns Gc(Unspecified).
            let bv_v = lookup(map, *bv)?;
            let k_v = lookup(map, *k)?;
            let val_v = lookup(map, *val)?;
            let inst_ref = b.ins().call(bv_u8_set_fnref, &[bv_v, k_v, val_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvU8Set expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecFill(dst, vec, fill) => {
            // ADR 0012 D-2 (iter CZ) — vm_vector_fill_gc.
            let vec_v = lookup(map, *vec)?;
            let fill_v = lookup(map, *fill)?;
            let inst_ref = b.ins().call(vec_fill_fnref, &[vec_v, fill_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecFill expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvFill(dst, bv, fill) => {
            // ADR 0012 D-2 (iter CZ) — vm_bytevector_fill_gc.
            let bv_v = lookup(map, *bv)?;
            let fill_v = lookup(map, *fill)?;
            let inst_ref = b.ins().call(bv_fill_fnref, &[bv_v, fill_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvFill expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrSet(dst, s, k, ch) => {
            // ADR 0012 D-2 (iter DA) — vm_string_set_gc.
            let s_v = lookup(map, *s)?;
            let k_v = lookup(map, *k)?;
            let ch_v = lookup(map, *ch)?;
            let inst_ref = b.ins().call(str_set_fnref, &[s_v, k_v, ch_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrSet expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrFill(dst, s, ch) => {
            // ADR 0012 D-2 (iter DH) — vm_string_fill_gc.
            let s_v = lookup(map, *s)?;
            let ch_v = lookup(map, *ch)?;
            let inst_ref = b.ins().call(str_fill_fnref, &[s_v, ch_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrFill expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecBuild(dst, args) => {
            // ADR 0012 D-2 (iter DO) — variadic vector via
            // stack-allocated buffer + vm_make_vector_buf helper.
            // Same buffer-fill pattern as Inst::CallGeneral.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(make_vector_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecBuild expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrBuild(dst, args) => {
            // ADR 0012 D-2 (iter DP) — variadic string via
            // stack-allocated buffer + vm_make_string_buf helper.
            // Mirrors VecBuild; helper deopts on non-character arg.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(make_string_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrBuild expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvBuild(dst, args) => {
            // ADR 0012 D-2 (iter DQ) — variadic bytevector via
            // stack-allocated buffer + vm_make_bytevector_buf helper.
            // Mirrors VecBuild; helper masks each Fixnum to 8 bits
            // and deopts on non-fixnum.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(make_bytevector_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvBuild expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrAppend(dst, args) => {
            // ADR 0012 D-2 (iter DR) — variadic string-append via
            // stack-allocated buffer + vm_string_append_buf helper.
            // Same buffer-fill shape; helper deopts on non-string.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(string_append_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrAppend expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListAppend(dst, args) => {
            // ADR 0012 D-2 (iter DS) — variadic append via stack-
            // allocated buffer + vm_append_buf helper. Same buffer-
            // fill shape; helper deopts if any of the first n-1 args
            // is not a proper list.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(append_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListAppend expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecAppend(dst, args) => {
            // ADR 0012 D-2 (iter DT) — variadic vector-append via
            // stack-allocated buffer + vm_vector_append_buf helper.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(vector_append_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecAppend expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvAppend(dst, args) => {
            // ADR 0012 D-2 (iter DU) — variadic bytevector-append via
            // stack-allocated buffer + vm_bytevector_append_buf helper.
            let n = args.len();
            let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
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
            let n_v = b.ins().iconst(I64, n as i64);
            let inst_ref = b.ins().call(bytevector_append_buf_fnref, &[buf_addr, n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvAppend expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrCopy(dst, src) => {
            // ADR 0012 D-2 (iter DB) — vm_string_copy_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(str_copy_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrCopy expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecCopy(dst, src) => {
            // ADR 0012 D-2 (iter DB) — vm_vector_copy_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(vec_copy_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecCopy expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvCopy(dst, src) => {
            // ADR 0012 D-2 (iter DC) — vm_bytevector_copy_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(bv_copy_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvCopy expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ProcedureP(dst, src) => {
            // ADR 0012 D-2 (iter DD) — vm_procedure_p_gc. Boolean.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(procedure_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ProcedureP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::PortP(dst, src)
        | Inst::InputPortP(dst, src)
        | Inst::OutputPortP(dst, src)
        | Inst::BinaryPortP(dst, src)
        | Inst::TextualPortP(dst, src)
        | Inst::PromiseP(dst, src)
        | Inst::HashtableP(dst, src)
        | Inst::HashtableSize(dst, src)
        | Inst::HashtableMutableP(dst, src)
        | Inst::OutputPortOpenP(dst, src)
        | Inst::PortEofP(dst, src)
        | Inst::PortHasSetPortPositionP(dst, src)
        | Inst::PortPosition(dst, src) => {
            // ADR 0012 D-2 (iter DD/GC/GD/GF/GG/GP/GQ/GR) — port/promise/hashtable.
            let v_v = lookup(map, *src)?;
            let fnref = match inst {
                Inst::PortP(..) => port_p_fnref,
                Inst::InputPortP(..) => input_port_p_fnref,
                Inst::OutputPortP(..) => output_port_p_fnref,
                Inst::BinaryPortP(..) => binary_port_p_fnref,
                Inst::TextualPortP(..) => textual_port_p_fnref,
                Inst::PromiseP(..) => promise_p_fnref,
                Inst::HashtableP(..) => hashtable_p_fnref,
                Inst::HashtableSize(..) => hashtable_size_fnref,
                Inst::HashtableMutableP(..) => hashtable_mutable_p_fnref,
                Inst::OutputPortOpenP(..) => output_port_open_p_fnref,
                Inst::PortEofP(..) => port_eof_p_fnref,
                Inst::PortHasSetPortPositionP(..) => port_has_set_port_position_p_fnref,
                Inst::PortPosition(..) => port_position_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "PortP/family expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::EofP(dst, src) => {
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(eof_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "EofP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::SymbolP(dst, src) => {
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(symbol_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "SymbolP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharP(dst, src) => {
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::BoolP(dst, src) => {
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(boolean_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BoolP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FixnumP(dst, src) => {
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(fixnum_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FixnumP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::FlonumP(dst, src) => {
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(flonum_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "FlonumP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharAlphabeticP(dst, src) => {
            // ADR 0012 D-2 (iter CI) — vm_char_alphabetic_p.
            // Operand is a Character codepoint (NOT a Gc handle);
            // result is Boolean (0/1). No stack-map declaration.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_alphabetic_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharAlphabeticP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharNumericP(dst, src) => {
            // ADR 0012 D-2 (iter CI) — vm_char_numeric_p.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_numeric_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharNumericP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharWhitespaceP(dst, src) => {
            // ADR 0012 D-2 (iter CI) — vm_char_whitespace_p.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_whitespace_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharWhitespaceP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharUpcase(dst, src) => {
            // ADR 0012 D-2 (iter CJ) — vm_char_upcase. Returns a
            // Character codepoint; no stack-map declaration.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_upcase_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharUpcase expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharDowncase(dst, src) => {
            // ADR 0012 D-2 (iter CJ) — vm_char_downcase.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_downcase_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharDowncase expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharUpperCaseP(dst, src) => {
            // ADR 0012 D-2 (iter CJ) — vm_char_upper_case_p.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_upper_case_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharUpperCaseP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharLowerCaseP(dst, src) => {
            // ADR 0012 D-2 (iter CJ) — vm_char_lower_case_p.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_lower_case_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharLowerCaseP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharFoldcase(dst, src) => {
            // ADR 0012 D-2 (iter CS) — vm_char_foldcase.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_foldcase_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharFoldcase expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::CharTitlecase(dst, src) => {
            // ADR 0012 D-2 (iter CS) — vm_char_titlecase.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(char_titlecase_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "CharTitlecase expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::DigitValue(dst, src) => {
            // ADR 0012 D-2 (iter CV) — vm_digit_value returns an
            // Any-shape Gc handle (Fixnum or Boolean). Mark for
            // stack-map tracking.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(digit_value_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "DigitValue expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VectorToList(dst, src) => {
            // ADR 0012 D-2 (iter CW) — vm_vector_to_list_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(vector_to_list_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorToList expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListToVector(dst, src) => {
            // ADR 0012 D-2 (iter CW) — vm_list_to_vector_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(list_to_vector_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListToVector expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToList(dst, src) => {
            // ADR 0012 D-2 (iter CX) — vm_string_to_list_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(string_to_list_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToList expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::ListToString(dst, src) => {
            // ADR 0012 D-2 (iter CX) — vm_list_to_string_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(list_to_string_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListToString expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToVector(dst, src) => {
            // ADR 0012 D-2 (iter DY) — vm_string_to_vector_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(string_to_vector_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToVector expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VectorToString(dst, src) => {
            // ADR 0012 D-2 (iter DY) — vm_vector_to_string_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(vector_to_string_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorToString expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::NumberToString(dst, src) => {
            // ADR 0012 D-2 (iter EC) — vm_number_to_string_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(number_to_string_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "NumberToString expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::NumberToStringRadix(dst, src, radix) => {
            // ADR 0012 D-2 (iter II) — vm_number_to_string_radix_gc.
            let v_v = lookup(map, *src)?;
            let r_v = lookup(map, *radix)?;
            let inst_ref = b.ins().call(number_to_string_radix_fnref, &[v_v, r_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "NumberToStringRadix expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::MakeListUnspec(dst, n) => {
            // ADR 0012 D-2 (iter IK) — vm_make_list_unspecified_gc.
            let n_v = lookup(map, *n)?;
            let inst_ref = b.ins().call(make_list_unspec_fnref, &[n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "MakeListUnspec expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::MakeVectorUnspec(dst, n) => {
            // ADR 0012 D-2 (iter JE) — vm_alloc_vector_unspec_gc.
            let n_v = lookup(map, *n)?;
            let inst_ref = b.ins().call(make_vector_unspec_fnref, &[n_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "MakeVectorUnspec expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VectorToListSliceFrom(dst, v, start) => {
            // ADR 0012 D-2 (iter IL) — vm_vector_to_list_slice_from_gc.
            let vv = lookup(map, *v)?;
            let sv = lookup(map, *start)?;
            let inst_ref = b.ins().call(vector_to_list_slice_from_fnref, &[vv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorToListSliceFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToListSliceFrom(dst, s, start) => {
            // ADR 0012 D-2 (iter IM) — vm_string_to_list_slice_from_gc.
            let sv = lookup(map, *s)?;
            let stv = lookup(map, *start)?;
            let inst_ref = b.ins().call(string_to_list_slice_from_fnref, &[sv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToListSliceFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BytevectorToListSliceFrom(dst, bv, start) => {
            // ADR 0012 D-2 (iter IN) — vm_bytevector_to_list_slice_from_gc.
            let bvv = lookup(map, *bv)?;
            let stv = lookup(map, *start)?;
            let inst_ref = b
                .ins()
                .call(bytevector_to_list_slice_from_fnref, &[bvv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BytevectorToListSliceFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VectorToStringSliceFrom(dst, v, start) => {
            // ADR 0012 D-2 (iter IO) — vm_vector_to_string_slice_from_gc.
            let vv = lookup(map, *v)?;
            let sv = lookup(map, *start)?;
            let inst_ref = b.ins().call(vector_to_string_slice_from_fnref, &[vv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorToStringSliceFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecCopyBangFrom(dst, dest_v, at_v, src_v, start_v) => {
            // ADR 0012 D-2 (iter IQ) — vm_vector_copy_bang_from_gc.
            let dv = lookup(map, *dest_v)?;
            let av = lookup(map, *at_v)?;
            let sv = lookup(map, *src_v)?;
            let stv = lookup(map, *start_v)?;
            let inst_ref = b
                .ins()
                .call(vector_copy_bang_from_fnref, &[dv, av, sv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecCopyBangFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrCopyBangSlice(dst, dest_v, at_v, src_v, start_v, end_v) => {
            // ADR 0012 D-2 (iter IV) — vm_string_copy_bang_slice_gc.
            let dv = lookup(map, *dest_v)?;
            let av = lookup(map, *at_v)?;
            let sv = lookup(map, *src_v)?;
            let stv = lookup(map, *start_v)?;
            let ev = lookup(map, *end_v)?;
            let inst_ref = b
                .ins()
                .call(string_copy_bang_slice_fnref, &[dv, av, sv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrCopyBangSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvCopyBangSlice(dst, dest_v, at_v, src_v, start_v, end_v) => {
            // ADR 0012 D-2 (iter IU) — vm_bytevector_copy_bang_slice_gc.
            let dv = lookup(map, *dest_v)?;
            let av = lookup(map, *at_v)?;
            let sv = lookup(map, *src_v)?;
            let stv = lookup(map, *start_v)?;
            let ev = lookup(map, *end_v)?;
            let inst_ref = b
                .ins()
                .call(bytevector_copy_bang_slice_fnref, &[dv, av, sv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvCopyBangSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecCopyBangSlice(dst, dest_v, at_v, src_v, start_v, end_v) => {
            // ADR 0012 D-2 (iter IT) — vm_vector_copy_bang_slice_gc.
            let dv = lookup(map, *dest_v)?;
            let av = lookup(map, *at_v)?;
            let sv = lookup(map, *src_v)?;
            let stv = lookup(map, *start_v)?;
            let ev = lookup(map, *end_v)?;
            let inst_ref = b
                .ins()
                .call(vector_copy_bang_slice_fnref, &[dv, av, sv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecCopyBangSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StrCopyBangFrom(dst, dest_v, at_v, src_v, start_v) => {
            // ADR 0012 D-2 (iter IS) — vm_string_copy_bang_from_gc.
            let dv = lookup(map, *dest_v)?;
            let av = lookup(map, *at_v)?;
            let sv = lookup(map, *src_v)?;
            let stv = lookup(map, *start_v)?;
            let inst_ref = b
                .ins()
                .call(string_copy_bang_from_fnref, &[dv, av, sv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrCopyBangFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvCopyBangFrom(dst, dest_v, at_v, src_v, start_v) => {
            // ADR 0012 D-2 (iter IR) — vm_bytevector_copy_bang_from_gc.
            let dv = lookup(map, *dest_v)?;
            let av = lookup(map, *at_v)?;
            let sv = lookup(map, *src_v)?;
            let stv = lookup(map, *start_v)?;
            let inst_ref = b
                .ins()
                .call(bytevector_copy_bang_from_fnref, &[dv, av, sv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvCopyBangFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToVectorSliceFrom(dst, s, start) => {
            // ADR 0012 D-2 (iter IP) — vm_string_to_vector_slice_from_gc.
            let sv = lookup(map, *s)?;
            let stv = lookup(map, *start)?;
            let inst_ref = b.ins().call(string_to_vector_slice_from_fnref, &[sv, stv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToVectorSliceFrom expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToNumberRadix(dst, src, radix) => {
            // ADR 0012 D-2 (iter IJ) — vm_string_to_number_radix_gc.
            let v_v = lookup(map, *src)?;
            let r_v = lookup(map, *radix)?;
            let inst_ref = b.ins().call(string_to_number_radix_fnref, &[v_v, r_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToNumberRadix expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToNumber(dst, src) => {
            // ADR 0012 D-2 (iter EC) — vm_string_to_number_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(string_to_number_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToNumber expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringContains(dst, h, n) => {
            // ADR 0012 D-2 (iter EU) — vm_string_contains_gc(h, n).
            let hv = lookup(map, *h)?;
            let nv = lookup(map, *n)?;
            let inst_ref = b.ins().call(string_contains_fnref, &[hv, nv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringContains expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringJoin(dst, parts, sep) => {
            // ADR 0012 D-2 (iter FE) — vm_string_join_gc.
            let pv = lookup(map, *parts)?;
            let sv = lookup(map, *sep)?;
            let inst_ref = b.ins().call(string_join_fnref, &[pv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringJoin expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringSplit(dst, s, sep) => {
            // ADR 0012 D-2 (iter FF) — vm_string_split_gc.
            let sv = lookup(map, *s)?;
            let pv = lookup(map, *sep)?;
            let inst_ref = b.ins().call(string_split_fnref, &[sv, pv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringSplit expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BytevectorToU8List(dst, src)
        | Inst::U8ListToBytevector(dst, src)
        | Inst::StringToUtf8(dst, src)
        | Inst::Utf8ToString(dst, src) => {
            // ADR 0012 D-2 (iter FL) — bytevector/utf8 conversion.
            let sv = lookup(map, *src)?;
            let fnref = match inst {
                Inst::BytevectorToU8List(..) => bytevector_to_u8_list_fnref,
                Inst::U8ListToBytevector(..) => u8_list_to_bytevector_fnref,
                Inst::StringToUtf8(..) => string_to_utf8_fnref,
                Inst::Utf8ToString(..) => utf8_to_string_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BytevectorToU8List/etc expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringContainsRight(dst, h, n)
        | Inst::StringIndex(dst, h, n)
        | Inst::StringIndexRight(dst, h, n) => {
            // ADR 0012 D-2 (iter FK) — string-contains-right / string-index / -index-right.
            let hv = lookup(map, *h)?;
            let nv = lookup(map, *n)?;
            let fnref = match inst {
                Inst::StringContainsRight(..) => string_contains_right_fnref,
                Inst::StringIndex(..) => string_index_fnref,
                Inst::StringIndexRight(..) => string_index_right_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[hv, nv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringIndex/ContainsRight expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringTake(dst, s, n)
        | Inst::StringDrop(dst, s, n)
        | Inst::StringTakeRight(dst, s, n)
        | Inst::StringDropRight(dst, s, n) => {
            // ADR 0012 D-2 (iter FJ) — string-take/-drop/-take-right/-drop-right.
            let sv = lookup(map, *s)?;
            let nv = lookup(map, *n)?;
            let fnref = match inst {
                Inst::StringTake(..) => string_take_fnref,
                Inst::StringDrop(..) => string_drop_fnref,
                Inst::StringTakeRight(..) => string_take_right_fnref,
                Inst::StringDropRight(..) => string_drop_right_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv, nv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringTake/Drop expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringReplaceAll(dst, s, from, to) => {
            // ADR 0012 D-2 (iter FI) — vm_string_replace_all_gc.
            let sv = lookup(map, *s)?;
            let fv = lookup(map, *from)?;
            let tv = lookup(map, *to)?;
            let inst_ref = b.ins().call(string_replace_all_fnref, &[sv, fv, tv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringReplaceAll expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BvFillSlice(dst, bv, fill, start, end) => {
            // ADR 0012 D-2 (iter HF) — bytevector-fill! 4-arg slice.
            let bvv = lookup(map, *bv)?;
            let fv = lookup(map, *fill)?;
            let sv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b
                .ins()
                .call(bytevector_fill_slice_fnref, &[bvv, fv, sv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BvFillSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::BytevectorEqP(dst, a_v, b_v) => {
            // ADR 0012 D-2 (iter HJ) — bytevector=?. Returns raw 0/1.
            // Deopts on non-bytevector.
            let av = lookup(map, *a_v)?;
            let bv = lookup(map, *b_v)?;
            let inst_ref = b.ins().call(bytevector_eq_p_fnref, &[av, bv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "BytevectorEqP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::VectorEqP(dst, a_v, b_v) => {
            // ADR 0012 D-2 (iter HK) — vector=?. Returns raw 0/1.
            // Deopts on non-vector.
            let av = lookup(map, *a_v)?;
            let bv = lookup(map, *b_v)?;
            let inst_ref = b.ins().call(vector_eq_p_fnref, &[av, bv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VectorEqP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::ExactNonNegIntP(dst, src) => {
            // ADR 0012 D-2 (iter HI) — exact-nonnegative-integer?. Returns
            // raw 0/1. No deopt.
            let sv = lookup(map, *src)?;
            let inst_ref = b.ins().call(exact_nonneg_int_p_fnref, &[sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ExactNonNegIntP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StrFillSlice(dst, s, ch, start, end) => {
            // ADR 0012 D-2 (iter HH) — string-fill! 4-arg slice.
            let sv = lookup(map, *s)?;
            let cv = lookup(map, *ch)?;
            let stv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(string_fill_slice_fnref, &[sv, cv, stv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StrFillSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecFillSlice(dst, vec, fill, start, end) => {
            // ADR 0012 D-2 (iter HG) — vector-fill! 4-arg slice.
            let vv = lookup(map, *vec)?;
            let fv = lookup(map, *fill)?;
            let sv = lookup(map, *start)?;
            let ev = lookup(map, *end)?;
            let inst_ref = b.ins().call(vector_fill_slice_fnref, &[vv, fv, sv, ev]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "VecFillSlice expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringReplaceFirst(dst, s, from, to) => {
            // ADR 0012 D-2 (iter HE) — vm_string_replace_first_gc.
            let sv = lookup(map, *s)?;
            let fv = lookup(map, *from)?;
            let tv = lookup(map, *to)?;
            let inst_ref = b.ins().call(string_replace_first_fnref, &[sv, fv, tv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringReplaceFirst expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringTrim(dst, src)
        | Inst::StringTrimLeft(dst, src)
        | Inst::StringTrimRight(dst, src) => {
            // ADR 0012 D-2 (iter FH) — string trim family.
            let v_v = lookup(map, *src)?;
            let fnref = match inst {
                Inst::StringTrim(..) => string_trim_fnref,
                Inst::StringTrimLeft(..) => string_trim_left_fnref,
                Inst::StringTrimRight(..) => string_trim_right_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringTrim* expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringPad(dst, s, w) | Inst::StringPadRight(dst, s, w) => {
            // ADR 0012 D-2 (iter FG) — string-pad / string-pad-right.
            let sv = lookup(map, *s)?;
            let wv = lookup(map, *w)?;
            let fnref = match inst {
                Inst::StringPad(..) => string_pad_fnref,
                Inst::StringPadRight(..) => string_pad_right_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[sv, wv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringPad/Right expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringPrefixP(dst, p, s) | Inst::StringSuffixP(dst, p, s) => {
            // ADR 0012 D-2 (iter EV) — string-prefix?/suffix?.
            // Both helpers return Boolean (0/1) directly, no Gc handle.
            let pv = lookup(map, *p)?;
            let sv = lookup(map, *s)?;
            let fnref = match inst {
                Inst::StringPrefixP(..) => string_prefix_p_fnref,
                Inst::StringSuffixP(..) => string_suffix_p_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[pv, sv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringPrefix/SuffixP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::StringReverse(dst, src)
        | Inst::StringUpcase(dst, src)
        | Inst::StringDowncase(dst, src)
        | Inst::StringFoldcase(dst, src) => {
            // ADR 0012 D-2 (iter EJ / ET) — string transform family.
            let v_v = lookup(map, *src)?;
            let fnref = match inst {
                Inst::StringReverse(..) => string_reverse_fnref,
                Inst::StringUpcase(..) => string_upcase_fnref,
                Inst::StringDowncase(..) => string_downcase_fnref,
                Inst::StringFoldcase(..) => string_foldcase_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "String transform expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::MakeList(dst, n_v, fill_v) => {
            // ADR 0012 D-2 (iter EM) — vm_make_list_fill_gc(n, fill).
            let n = lookup(map, *n_v)?;
            let f = lookup(map, *fill_v)?;
            let inst_ref = b.ins().call(make_list_fill_fnref, &[n, f]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "MakeList expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::IotaN(dst, n_v) => {
            // ADR 0012 D-2 (iter EN) — vm_iota_n_gc(n).
            let n = lookup(map, *n_v)?;
            let inst_ref = b.ins().call(iota_n_fnref, &[n]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "IotaN expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::IotaNs(dst, c_v, s_v) => {
            // ADR 0012 D-2 (iter FC) — vm_iota_ns_gc(count, start).
            let c = lookup(map, *c_v)?;
            let s = lookup(map, *s_v)?;
            let inst_ref = b.ins().call(iota_ns_fnref, &[c, s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "IotaNs expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::IotaNss(dst, c_v, s_v, st_v) => {
            // ADR 0012 D-2 (iter FD) — vm_iota_nss_gc(count, start, step).
            let c = lookup(map, *c_v)?;
            let s = lookup(map, *s_v)?;
            let st = lookup(map, *st_v)?;
            let inst_ref = b.ins().call(iota_nss_fnref, &[c, s, st]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "IotaNss expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::LastPair(dst, src) | Inst::Last(dst, src) => {
            // ADR 0012 D-2 (iter EO) — last-pair / last.
            let v_v = lookup(map, *src)?;
            let fnref = match inst {
                Inst::LastPair(..) => last_pair_fnref,
                Inst::Last(..) => last_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "LastPair/Last expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::Concatenate(dst, src) => {
            // ADR 0012 D-2 (iter FB) — vm_concatenate_gc.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(concatenate_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Concatenate expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::NotPairP(dst, src) => {
            // ADR 0012 D-2 (iter FB) — vm_not_pair_p_gc (raw 0/1).
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(not_pair_p_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "NotPairP expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::NullListP(dst, src)
        | Inst::ProperListP(dst, src)
        | Inst::DottedListP(dst, src)
        | Inst::CircularListP(dst, src) => {
            // ADR 0012 D-2 (iter EY) — SRFI-1 list classifiers.
            // All four return raw 0/1 (Boolean carrier).
            let v_v = lookup(map, *src)?;
            let fnref = match inst {
                Inst::NullListP(..) => null_list_p_fnref,
                Inst::ProperListP(..) => proper_list_p_fnref,
                Inst::DottedListP(..) => dotted_list_p_fnref,
                Inst::CircularListP(..) => circular_list_p_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "ListClassifier expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            map.insert(*dst, result);
        }
        Inst::Take(dst, lst, n_v) | Inst::Drop(dst, lst, n_v) => {
            // ADR 0012 D-2 (iter EX) — take / drop.
            let lv = lookup(map, *lst)?;
            let nv = lookup(map, *n_v)?;
            let fnref = match inst {
                Inst::Take(..) => take_fnref,
                Inst::Drop(..) => drop_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[lv, nv]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Take/Drop expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::VecCopyBang(dst, dest_v, at_v, src_v)
        | Inst::BvCopyBang(dst, dest_v, at_v, src_v)
        | Inst::StrCopyBang(dst, dest_v, at_v, src_v) => {
            // ADR 0012 D-2 (iter ER / ES) — copy! family.
            let d = lookup(map, *dest_v)?;
            let a = lookup(map, *at_v)?;
            let s = lookup(map, *src_v)?;
            let fnref = match inst {
                Inst::VecCopyBang(..) => vector_copy_bang_fnref,
                Inst::BvCopyBang(..) => bytevector_copy_bang_fnref,
                Inst::StrCopyBang(..) => string_copy_bang_fnref,
                _ => unreachable!(),
            };
            let inst_ref = b.ins().call(fnref, &[d, a, s]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "Vec/Bv/StrCopyBang expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::SymbolToString(dst, src) => {
            // ADR 0012 D-2 (iter CY) — vm_symbol_to_string_gc.
            // Operand is a Symbol-shape Fixnum (no Gc); result is
            // an Any-shape Gc handle.
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(symbol_to_string_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "SymbolToString expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
            b.declare_value_needs_stack_map(result);
            map.insert(*dst, result);
        }
        Inst::StringToSymbol(dst, src) => {
            // ADR 0012 D-2 (iter CY) — vm_string_to_symbol_gc.
            // Operand is Any (Gc consumed); result is Symbol-shape
            // Fixnum (no Gc).
            let v_v = lookup(map, *src)?;
            let inst_ref = b.ins().call(string_to_symbol_fnref, &[v_v]);
            let result = {
                let results = b.inst_results(inst_ref);
                if results.len() != 1 {
                    return Err(JitError::Codegen(format!(
                        "StringToSymbol expected 1 result, got {}",
                        results.len()
                    )));
                }
                results[0]
            };
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

    /// Issue #16 — the pre-codegen verifier rejects a function
    /// with an empty block layout (the issue-#4 malformed shape)
    /// as `Malformed`, and accepts one with a block.
    #[test]
    fn verify_clif_lowerable_rejects_empty_block_layout() {
        use cranelift_codegen::ir::UserFuncName;
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(I64));
        sig.returns.push(AbiParam::new(I64));
        let mut func = ClifFunction::with_name_signature(UserFuncName::user(0, 0), sig);

        // No blocks in the layout — `entry_block()` is None, the
        // shape that panics Cranelift's `remove_constant_phis`.
        assert!(
            matches!(verify_clif_lowerable(&func), Err(JitError::Malformed(_))),
            "empty block layout must be rejected as Malformed"
        );

        // Append a block — now `entry_block()` resolves.
        let blk = func.dfg.make_block();
        func.layout.append_block(blk);
        assert!(
            verify_clif_lowerable(&func).is_ok(),
            "a function with a block in its layout must pass the verifier"
        );
    }

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
            terminator: Term::Branch(
                cs_rir::Value(2),
                cs_rir::BlockId(1),
                cs_rir::BlockId(2),
                Vec::new(),
            ),
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
            terminator: Term::Branch(
                cs_rir::Value(2),
                cs_rir::BlockId(1),
                cs_rir::BlockId(2),
                Vec::new(),
            ),
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
            terminator: Term::Branch(
                cs_rir::Value(2),
                cs_rir::BlockId(1),
                cs_rir::BlockId(2),
                Vec::new(),
            ),
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

    /// Build fib in the exact shape the bytecode→RIR translator emits
    /// for the JIT: an `if` whose two arms `Jump` to a join block with
    /// a Fixnum param, then `Return(param)`. The two `CallSelf`s are
    /// non-tail (their results feed `Add`). This is the canonical
    /// raw-ABI target (#50).
    fn fib_join_rir() -> RirFunction {
        let mut f = RirFunction::new("fib");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum)); // n
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(2)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(
                cs_rir::Value(2),
                cs_rir::BlockId(1),
                cs_rir::BlockId(2),
                Vec::new(),
            ),
        });
        // then arm: jump join with n.
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(0)]),
        });
        // else arm: s = fib(n-1) + fib(n-2); jump join with s.
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(4), Const::Fixnum(1)),
                Inst::Sub(cs_rir::Value(5), cs_rir::Value(0), cs_rir::Value(4)),
                Inst::CallSelf(cs_rir::Value(6), vec![cs_rir::Value(5)]),
                Inst::LoadConst(cs_rir::Value(7), Const::Fixnum(2)),
                Inst::Sub(cs_rir::Value(8), cs_rir::Value(0), cs_rir::Value(7)),
                Inst::CallSelf(cs_rir::Value(9), vec![cs_rir::Value(8)]),
                Inst::Add(cs_rir::Value(10), cs_rir::Value(6), cs_rir::Value(9)),
            ],
            terminator: Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(10)]),
        });
        // join: return the merged Fixnum.
        f.blocks.push(Block {
            id: cs_rir::BlockId(3),
            params: vec![(cs_rir::Value(3), cs_rir::Type::Fixnum)],
            insts: vec![],
            terminator: Term::Return(cs_rir::Value(3)),
        });
        f
    }

    #[test]
    fn raw_abi_accepts_join_block_fib() {
        // The join-block param (Value(3)) merges a Fixnum param and an
        // Add result — both raw — so the body closes under the raw ABI.
        assert!(detect_uniform_nb_raw_abi(&fib_join_rir()));
    }

    #[test]
    fn raw_abi_accepts_direct_return_fib() {
        // The simpler arm-returns-directly shape (no join block) also
        // qualifies: every Return is a raw value (param / Add result).
        let mut f = RirFunction::new("fib_direct");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(2)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(
                cs_rir::Value(2),
                cs_rir::BlockId(1),
                cs_rir::BlockId(2),
                Vec::new(),
            ),
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
        assert!(detect_uniform_nb_raw_abi(&f));
    }

    #[test]
    fn raw_abi_rejects_any_param() {
        // Same shape as fib but the param is Any → not all-Fixnum, so
        // the raw ABI does not apply (stays the iter-1 NB path).
        let mut f = fib_join_rir();
        f.params[0].1 = cs_rir::Type::Any;
        assert!(!detect_uniform_nb_raw_abi(&f));
    }

    #[test]
    fn raw_abi_rejects_no_self_call() {
        // A straight-line Fixnum body with no CallSelf gains nothing
        // from the raw self-call ABI.
        let f = super::testing::add_two_fixnums();
        assert!(!detect_uniform_nb_raw_abi(&f));
    }

    #[test]
    fn raw_abi_rejects_cross_call() {
        // A self-recursive Fixnum body that also makes a cross-function
        // call is the #47 NB path, not the raw ABI.
        let mut f = fib_join_rir();
        // Splice a cross-call into the else arm (block index 2).
        f.blocks[2].insts.push(Inst::Call(
            cs_rir::Value(11),
            cs_rir::Value(0),
            vec![cs_rir::Value(0)],
        ));
        assert!(!detect_uniform_nb_raw_abi(&f));
    }

    #[test]
    fn raw_abi_rejects_non_raw_return() {
        // If the join param's incoming includes a non-raw value (here a
        // Cons result), the param can't be raw and the gate rejects.
        let mut f = fib_join_rir();
        // Replace the then-arm's jump arg with a freshly Cons'd pair.
        f.blocks[1].insts.push(Inst::Cons(
            cs_rir::Value(20),
            cs_rir::Value(0),
            0,
            cs_rir::Value(0),
            0,
        ));
        f.blocks[1].terminator = Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(20)]);
        assert!(!detect_uniform_nb_raw_abi(&f));
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
