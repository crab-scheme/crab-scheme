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
/// `compile_uniform_nb` call.
///
/// One module per backend instance is fine for iter 2 (we never
/// re-define the same function); iter 3 may want a per-module
/// finalize pattern for tier-up.
pub struct Lowerer {
    module: JITModule,
    ctx: Context,
    func_ctx: FunctionBuilderContext,
    next_id: u64,
    /// FuncId of the imported `vm_env_lookup_any` helper.
    /// `Inst::EnvLookupAny` lowers to a Cranelift call against
    /// this. Used by iter BU's translator when a free-var load
    /// flows to a `CallGeneral` callee position — the binding's
    /// shape is unknown at compile time, so we fetch a Gc handle.
    env_lookup_any_func: cranelift_module::FuncId,
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
    /// -> i64` (layer-3 region allocator). `Inst::ConsRegion` lowers
    /// to a call against this; the runtime helper allocates into the
    /// innermost in-scope `cs_gc::Region` (resolved via the
    /// cs-runtime region resolver) and falls back to Rc allocation if
    /// no region is in scope. Only declared with the `regions`
    /// feature — cs-vm's `vm_alloc_pair_region_gc` is `regions`-gated.
    /// Produced by the cs-opt `escape-to-region` pass (#51) and by the
    /// explicit `cons-in-region` builtin.
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
    /// FuncId of `vm_symbol_p_gc(v) -> i64`. ADR 0012 D-2 (iter DD).
    symbol_p_func: cranelift_module::FuncId,
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
    /// `vm_fixnum_to_nb(n) -> i64` — encode a raw i64 as NB (bignum if
    /// >47-bit). Used by the uniform-NB hash builtins (#50).
    fixnum_to_nb_func: cranelift_module::FuncId,
    /// FuncId of `vm_symbol_hash_gc(s) -> i64`. ADR 0012 D-2 (iter GB).
    symbol_hash_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_eof_p_gc(p) -> i64` (0/1). ADR 0012 D-2 (iter GQ).
    port_eof_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_has_set_port_position_p_gc(p) -> i64` (0/1).
    /// ADR 0012 D-2 (iter GQ).
    port_has_set_port_position_p_func: cranelift_module::FuncId,
    /// FuncId of `vm_port_position_gc(p) -> i64` (Fixnum).
    /// ADR 0012 D-2 (iter GR).
    port_position_func: cranelift_module::FuncId,
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
            "vm_env_lookup_any",
            cs_vm::vm::vm_env_lookup_any as *const u8,
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
        // counterpart. The runtime helper resolves the current region
        // via the cs-runtime resolver hook; falls back to Rc
        // allocation if no region is in scope.
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
        builder.symbol("vm_symbol_p_gc", cs_vm::vm::vm_symbol_p_gc as *const u8);
        // ADR 0012 D-2 (iter DE) — more type predicates on Any.
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
        builder.symbol("vm_fixnum_to_nb", cs_vm::vm::vm_fixnum_to_nb as *const u8);
        // ADR 0012 D-2 (iter GC) — port-subtype predicates.
        // ADR 0012 D-2 (iter GP) — output-port-open?.
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

        // Import env-lookup helpers: extern "C" fn(i64) -> i64.
        let mut env_lookup_sig = module.make_signature();
        env_lookup_sig.params.push(AbiParam::new(I64));
        env_lookup_sig.returns.push(AbiParam::new(I64));

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
        // Layer-3 region allocator, same (i64,i64,i64,i64)->i64 shape
        // as vm_alloc_pair. Only when `regions` is on.
        #[cfg(feature = "regions")]
        let alloc_pair_region_func = module
            .declare_function(
                "vm_alloc_pair_region",
                cranelift_module::Linkage::Import,
                &alloc_pair_sig,
            )
            .map_err(|e| {
                JitError::Codegen(format!("declare_function vm_alloc_pair_region: {e}"))
            })?;

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

        // box_typed_sig: (i64, i64) -> i64, shared by the typed
        // arith/cmp helpers declared below.
        let mut box_typed_sig = module.make_signature();
        box_typed_sig.params.push(AbiParam::new(I64));
        box_typed_sig.params.push(AbiParam::new(I64));
        box_typed_sig.returns.push(AbiParam::new(I64));

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
        let symbol_p_func = module
            .declare_function(
                "vm_symbol_p_gc",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_symbol_p_gc: {e}")))?;

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
        let fixnum_to_nb_func = module
            .declare_function(
                "vm_fixnum_to_nb",
                cranelift_module::Linkage::Import,
                &pair_accessor_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function vm_fixnum_to_nb: {e}")))?;

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
            env_lookup_any_func,
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
            symbol_p_func,
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
            fixnum_to_nb_func,
            port_eof_p_func,
            port_has_set_port_position_p_func,
            port_position_func,
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
        // is created so the VM fallback (backend declines → body stays
        // on bytecode) finds the Lowerer's `func_ctx` clean. If the
        // prewalk passes, the actual lowering is guaranteed not to hit
        // an `Unsupported` case (every Inst arm has a handler).
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
        // A non-tail CallSelf is safe on uniform-NB when the inner uses
        // SystemV conv: small frames, same host-stack ceiling as the
        // pure-fixnum tier. The inner is Tail conv ONLY when the body has
        // a tail self-call (return_call). So precompute whether any block
        // tail-self-recurses: if not, a non-tail CallSelf is admissible
        // (lowers like pure-fixnum); if so, a non-tail CallSelf in the
        // same body would burn Tail-conv frames and stays rejected
        // (unless the #47 cross-call exception or the raw-Fixnum ABI
        // applies). This is what lets the pointer self-recursive bodies
        // (binary-trees / alloc-stress helpers) JIT here instead of on
        // the legacy pure-fixnum tier — the precondition for retiring it
        // (#50).
        let has_tail_self = rir
            .blocks
            .iter()
            .any(|b| detect_uniform_nb_tail_self(rir, b).0.is_some());
        for blk in &rir.blocks {
            let last_idx = blk.insts.len().saturating_sub(1);
            for (i, inst) in blk.insts.iter().enumerate() {
                match inst {
                    // Layer-3 region cons (#51). Supported only with the
                    // `regions` feature — its lowering arm and the
                    // `vm_alloc_pair_region` helper are `regions`-gated.
                    // Without `regions` it falls to the catch-all below
                    // (declines → VM).
                    #[cfg(feature = "regions")]
                    Inst::ConsRegion(_, _, _, _, _) => {}
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
                    | Inst::NotBoolean(_, _)
                    | Inst::EqAny(_, _, _)
                    | Inst::Move(_, _)
                    | Inst::BitAnd(_, _, _)
                    | Inst::BitOr(_, _, _)
                    | Inst::BitXor(_, _, _)
                    | Inst::BitNot(_, _)
                    | Inst::AbsFixnum(_, _)
                    | Inst::MaxFixnum(_, _, _)
                    | Inst::MinFixnum(_, _, _)
                    | Inst::Quotient(_, _, _)
                    | Inst::Remainder(_, _, _)
                    | Inst::Modulo(_, _, _)
                    | Inst::FloorQuotient(_, _, _)
                    | Inst::IntCharBitcast(_, _)
                    | Inst::CharToInt(_, _)
                    | Inst::Assq(_, _, _)
                    | Inst::Assv(_, _, _)
                    | Inst::Assoc(_, _, _)
                    | Inst::Memq(_, _, _)
                    | Inst::Memv(_, _, _)
                    | Inst::Member(_, _, _)
                    | Inst::Reverse(_, _)
                    | Inst::ListCopy(_, _)
                    | Inst::LastPair(_, _)
                    | Inst::Concatenate(_, _)
                    | Inst::AppendReverse(_, _, _)
                    | Inst::NotPairP(_, _)
                    | Inst::ListP(_, _)
                    | Inst::ProperListP(_, _)
                    | Inst::CharUpcase(_, _)
                    | Inst::CharDowncase(_, _)
                    | Inst::CharFoldcase(_, _)
                    | Inst::CharTitlecase(_, _)
                    | Inst::CharAlphabeticP(_, _)
                    | Inst::CharNumericP(_, _)
                    | Inst::CharWhitespaceP(_, _)
                    | Inst::CharUpperCaseP(_, _)
                    | Inst::CharLowerCaseP(_, _)
                    | Inst::FlonumSin(_, _)
                    | Inst::FlonumCos(_, _)
                    | Inst::FlonumTan(_, _)
                    | Inst::FlonumAsin(_, _)
                    | Inst::FlonumAcos(_, _)
                    | Inst::FlonumAtan(_, _)
                    | Inst::FlonumExp(_, _)
                    | Inst::FlonumLog(_, _)
                    | Inst::FlonumExpt(_, _, _)
                    | Inst::FlonumLog2(_, _, _)
                    | Inst::FlonumAtan2(_, _, _)
                    | Inst::FlonumIsInteger(_, _)
                    | Inst::FlEvenP(_, _)
                    | Inst::FlOddP(_, _)
                    | Inst::FlonumIsNan(_, _)
                    | Inst::FlonumIsInfinite(_, _)
                    | Inst::FlonumIsFinite(_, _)
                    | Inst::SymbolP(_, _)
                    | Inst::ProcedureP(_, _)
                    | Inst::PortP(_, _)
                    | Inst::DottedListP(_, _)
                    | Inst::CircularListP(_, _)
                    | Inst::NullListP(_, _)
                    | Inst::FileExistsP(_, _)
                    | Inst::PortEofP(_, _)
                    | Inst::EofObject(_)
                    | Inst::MakePromise(_, _)
                    | Inst::ForceForced(_, _)
                    | Inst::AlistCopy(_, _)
                    | Inst::DeleteDuplicates(_, _)
                    | Inst::VecCopy(_, _)
                    | Inst::VecFill(_, _, _)
                    | Inst::VectorToList(_, _)
                    | Inst::VectorToString(_, _)
                    | Inst::VectorEqP(_, _, _)
                    | Inst::VecCopyFrom(_, _, _)
                    | Inst::VecCopySlice(_, _, _, _)
                    | Inst::VectorToListSlice(_, _, _, _)
                    | Inst::VectorToListSliceFrom(_, _, _)
                    | Inst::VectorToStringSlice(_, _, _, _)
                    | Inst::VectorToStringSliceFrom(_, _, _)
                    | Inst::VecFillFrom(_, _, _, _)
                    | Inst::VecFillSlice(_, _, _, _, _)
                    | Inst::VecCopyBang(_, _, _, _)
                    | Inst::VecCopyBangFrom(_, _, _, _, _)
                    | Inst::VecCopyBangSlice(_, _, _, _, _, _)
                    | Inst::StrP(_, _)
                    | Inst::StrLength(_, _)
                    | Inst::StrLt(_, _, _)
                    | Inst::StrGt(_, _, _)
                    | Inst::StrLe(_, _, _)
                    | Inst::StrGe(_, _, _)
                    | Inst::StrCiEq(_, _, _)
                    | Inst::StrCiLt(_, _, _)
                    | Inst::StrCiGt(_, _, _)
                    | Inst::StrCiLe(_, _, _)
                    | Inst::StrCiGe(_, _, _)
                    | Inst::StringDowncase(_, _)
                    | Inst::StringUpcase(_, _)
                    | Inst::StringFoldcase(_, _)
                    | Inst::StringTitlecase(_, _)
                    | Inst::StringReverse(_, _)
                    | Inst::StringPrefixP(_, _, _)
                    | Inst::StringSuffixP(_, _, _)
                    | Inst::BvP(_, _)
                    | Inst::BvLength(_, _)
                    | Inst::BvAlloc(_, _, _)
                    | Inst::BvCopy(_, _)
                    | Inst::BvFill(_, _, _)
                    | Inst::BytevectorEqP(_, _, _)
                    | Inst::BvCopyFrom(_, _, _)
                    | Inst::BvCopySlice(_, _, _, _)
                    | Inst::BvFillFrom(_, _, _, _)
                    | Inst::BvFillSlice(_, _, _, _, _)
                    | Inst::BytevectorToListSlice(_, _, _, _)
                    | Inst::BytevectorToListSliceFrom(_, _, _)
                    | Inst::BvCopyBang(_, _, _, _)
                    | Inst::BvCopyBangFrom(_, _, _, _, _)
                    | Inst::BvCopyBangSlice(_, _, _, _, _, _)
                    | Inst::BvIeeeSingleNativeRef(_, _, _)
                    | Inst::BvIeeeDoubleNativeRef(_, _, _)
                    | Inst::BvIeeeSingleNativeSet(_, _, _, _)
                    | Inst::BvIeeeDoubleNativeSet(_, _, _, _)
                    | Inst::HashtableP(_, _)
                    | Inst::HashtableMutableP(_, _)
                    | Inst::HashtableContainsP(_, _, _)
                    | Inst::HashtableSize(_, _)
                    | Inst::HashtableKeys(_, _)
                    | Inst::HashtableValues(_, _)
                    | Inst::HashtableClear(_, _)
                    | Inst::HashtableToAlist(_, _)
                    | Inst::HashtableCopy(_, _)
                    | Inst::HashtableHashFn(_, _)
                    | Inst::HashtableDelete(_, _, _)
                    | Inst::HashtableSet(_, _, _, _)
                    | Inst::HashtableRef(_, _, _, _)
                    | Inst::MakeHashtableEqual(_)
                    | Inst::MakeHashtableEq(_)
                    | Inst::MakeHashtableEqv(_)
                    | Inst::StrCopy(_, _)
                    | Inst::Substring(_, _, _, _)
                    | Inst::StrCopyFrom(_, _, _)
                    | Inst::StringTake(_, _, _)
                    | Inst::StringTakeRight(_, _, _)
                    | Inst::StringDrop(_, _, _)
                    | Inst::StringDropRight(_, _, _)
                    | Inst::StringPad(_, _, _)
                    | Inst::StringPadRight(_, _, _)
                    | Inst::StringSplit(_, _, _)
                    | Inst::StringJoin(_, _, _)
                    | Inst::StringReplaceAll(_, _, _, _)
                    | Inst::StringReplaceFirst(_, _, _, _)
                    | Inst::StringTrim(_, _)
                    | Inst::StringTrimLeft(_, _)
                    | Inst::StringTrimRight(_, _)
                    | Inst::StringToList(_, _)
                    | Inst::StringToVector(_, _)
                    | Inst::StringToListSlice(_, _, _, _)
                    | Inst::StringToListSliceFrom(_, _, _)
                    | Inst::StringToVectorSlice(_, _, _, _)
                    | Inst::StringToVectorSliceFrom(_, _, _)
                    | Inst::ListToVector(_, _)
                    | Inst::ListToString(_, _)
                    | Inst::StrRef(_, _, _)
                    | Inst::StrSet(_, _, _, _)
                    | Inst::StrAlloc(_, _, _)
                    | Inst::StrFill(_, _, _)
                    | Inst::StrFillFrom(_, _, _, _)
                    | Inst::StrFillSlice(_, _, _, _, _)
                    | Inst::StrCopyBang(_, _, _, _)
                    | Inst::StrCopyBangFrom(_, _, _, _, _)
                    | Inst::StrCopyBangSlice(_, _, _, _, _, _)
                    | Inst::ListTail(_, _, _)
                    | Inst::ListRef(_, _, _)
                    | Inst::ListSet(_, _, _, _)
                    | Inst::Take(_, _, _)
                    | Inst::Drop(_, _, _)
                    | Inst::MakeList(_, _, _)
                    | Inst::MakeListUnspec(_, _)
                    | Inst::MakeVectorUnspec(_, _)
                    | Inst::BitwiseBitCount(_, _)
                    | Inst::BitwiseLength(_, _)
                    | Inst::FxFirstBitSet(_, _)
                    | Inst::BitwiseBitSetP(_, _, _)
                    | Inst::ExactNonNegIntP(_, _)
                    | Inst::EqualAny(_, _, _)
                    | Inst::Expt(_, _, _)
                    | Inst::Gcd(_, _, _)
                    | Inst::Lcm(_, _, _)
                    | Inst::ArithShift(_, _, _)
                    | Inst::BitwiseArithShiftLeft(_, _, _)
                    | Inst::BitwiseArithShiftRight(_, _, _)
                    | Inst::DivEuclid(_, _, _)
                    | Inst::ModEuclid(_, _, _)
                    | Inst::NumberToString(_, _)
                    | Inst::StringToNumber(_, _)
                    | Inst::StringContains(_, _, _)
                    | Inst::StringContainsRight(_, _, _)
                    | Inst::NumberToStringRadix(_, _, _)
                    | Inst::StringToNumberRadix(_, _, _)
                    | Inst::IotaN(_, _)
                    | Inst::IotaNs(_, _, _)
                    | Inst::IotaNss(_, _, _, _)
                    | Inst::DigitValue(_, _)
                    | Inst::StringIndex(_, _, _)
                    | Inst::StringIndexRight(_, _, _)
                    | Inst::StrAppend(_, _)
                    | Inst::VecAppend(_, _)
                    | Inst::VecBuild(_, _)
                    | Inst::BvBuild(_, _)
                    | Inst::BvAppend(_, _)
                    | Inst::SymbolToString(_, _)
                    | Inst::StringToSymbol(_, _)
                    | Inst::StringHash(_, _)
                    | Inst::SymbolHash(_, _)
                    | Inst::EqualHash(_, _)
                    | Inst::SetCar(_, _, _)
                    | Inst::SetCdr(_, _, _)
                    | Inst::Length(_, _)
                    | Inst::Last(_, _)
                    | Inst::ListAppend(_, _)
                    | Inst::BytevectorToU8List(_, _)
                    | Inst::U8ListToBytevector(_, _)
                    | Inst::StringToUtf8(_, _)
                    | Inst::Utf8ToString(_, _)
                    | Inst::PortPosition(_, _)
                    | Inst::PortHasSetPortPositionP(_, _)
                    | Inst::CurrentJiffy(_)
                    | Inst::CurrentSecond(_)
                    | Inst::BvU8Ref(_, _, _)
                    | Inst::BvS8Ref(_, _, _)
                    | Inst::BvU16NativeRef(_, _, _)
                    | Inst::BvS16NativeRef(_, _, _)
                    | Inst::BvU32NativeRef(_, _, _)
                    | Inst::BvS32NativeRef(_, _, _)
                    | Inst::BvU64NativeRef(_, _, _)
                    | Inst::BvS64NativeRef(_, _, _)
                    | Inst::BvU8Set(_, _, _, _)
                    | Inst::BvS8Set(_, _, _, _)
                    | Inst::BvU16NativeSet(_, _, _, _)
                    | Inst::BvS16NativeSet(_, _, _, _)
                    | Inst::BvU32NativeSet(_, _, _, _)
                    | Inst::BvS32NativeSet(_, _, _, _)
                    | Inst::BvU64NativeSet(_, _, _, _)
                    | Inst::BvS64NativeSet(_, _, _, _)
                    | Inst::Div0(_, _, _)
                    | Inst::Mod0(_, _, _)
                    | Inst::StrBuild(_, _) => {
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
                        if !is_tail && !has_cross_call && !raw_abi && has_tail_self {
                            return Err(JitError::Unsupported(
                                "uniform-nb: non-tail CallSelf would burn Tail-conv host stack"
                                    .into(),
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
        // Reuse `has_tail_self` (computed in the prewalk): the inner needs
        // the Tail conv exactly when a tail self-call will emit
        // `return_call`. Everything else takes SystemV (smaller frames,
        // higher host-stack ceiling).
        let inner_conv = if has_tail_self {
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
                eq_any: self
                    .module
                    .declare_func_in_func(self.eq_any_func, builder.func),
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
                #[cfg(feature = "regions")]
                alloc_pair_region: self
                    .module
                    .declare_func_in_func(self.alloc_pair_region_func, builder.func),
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
                assq: self
                    .module
                    .declare_func_in_func(self.assq_func, builder.func),
                assv: self
                    .module
                    .declare_func_in_func(self.assv_func, builder.func),
                assoc: self
                    .module
                    .declare_func_in_func(self.assoc_func, builder.func),
                memq: self
                    .module
                    .declare_func_in_func(self.memq_func, builder.func),
                memv: self
                    .module
                    .declare_func_in_func(self.memv_func, builder.func),
                member: self
                    .module
                    .declare_func_in_func(self.member_func, builder.func),
                reverse: self
                    .module
                    .declare_func_in_func(self.reverse_func, builder.func),
                list_copy: self
                    .module
                    .declare_func_in_func(self.list_copy_func, builder.func),
                last_pair: self
                    .module
                    .declare_func_in_func(self.last_pair_func, builder.func),
                concatenate: self
                    .module
                    .declare_func_in_func(self.concatenate_func, builder.func),
                append_reverse: self
                    .module
                    .declare_func_in_func(self.append_reverse_func, builder.func),
                not_pair_p: self
                    .module
                    .declare_func_in_func(self.not_pair_p_func, builder.func),
                list_p: self
                    .module
                    .declare_func_in_func(self.list_p_func, builder.func),
                proper_list_p: self
                    .module
                    .declare_func_in_func(self.proper_list_p_func, builder.func),
                char_upcase: self
                    .module
                    .declare_func_in_func(self.char_upcase_func, builder.func),
                char_downcase: self
                    .module
                    .declare_func_in_func(self.char_downcase_func, builder.func),
                char_foldcase: self
                    .module
                    .declare_func_in_func(self.char_foldcase_func, builder.func),
                char_titlecase: self
                    .module
                    .declare_func_in_func(self.char_titlecase_func, builder.func),
                char_alphabetic_p: self
                    .module
                    .declare_func_in_func(self.char_alphabetic_p_func, builder.func),
                char_numeric_p: self
                    .module
                    .declare_func_in_func(self.char_numeric_p_func, builder.func),
                char_whitespace_p: self
                    .module
                    .declare_func_in_func(self.char_whitespace_p_func, builder.func),
                char_upper_case_p: self
                    .module
                    .declare_func_in_func(self.char_upper_case_p_func, builder.func),
                char_lower_case_p: self
                    .module
                    .declare_func_in_func(self.char_lower_case_p_func, builder.func),
                flonum_sin: self
                    .module
                    .declare_func_in_func(self.flonum_sin_func, builder.func),
                flonum_cos: self
                    .module
                    .declare_func_in_func(self.flonum_cos_func, builder.func),
                flonum_tan: self
                    .module
                    .declare_func_in_func(self.flonum_tan_func, builder.func),
                flonum_asin: self
                    .module
                    .declare_func_in_func(self.flonum_asin_func, builder.func),
                flonum_acos: self
                    .module
                    .declare_func_in_func(self.flonum_acos_func, builder.func),
                flonum_atan: self
                    .module
                    .declare_func_in_func(self.flonum_atan_func, builder.func),
                flonum_exp: self
                    .module
                    .declare_func_in_func(self.flonum_exp_func, builder.func),
                flonum_log: self
                    .module
                    .declare_func_in_func(self.flonum_log_func, builder.func),
                flonum_expt: self
                    .module
                    .declare_func_in_func(self.flonum_expt_func, builder.func),
                flonum_log2: self
                    .module
                    .declare_func_in_func(self.flonum_log2_func, builder.func),
                flonum_atan2: self
                    .module
                    .declare_func_in_func(self.flonum_atan2_func, builder.func),
                flonum_is_integer: self
                    .module
                    .declare_func_in_func(self.flonum_is_integer_func, builder.func),
                fl_even_p: self
                    .module
                    .declare_func_in_func(self.fl_even_p_func, builder.func),
                fl_odd_p: self
                    .module
                    .declare_func_in_func(self.fl_odd_p_func, builder.func),
                symbol_p: self
                    .module
                    .declare_func_in_func(self.symbol_p_func, builder.func),
                procedure_p: self
                    .module
                    .declare_func_in_func(self.procedure_p_func, builder.func),
                port_p: self
                    .module
                    .declare_func_in_func(self.port_p_func, builder.func),
                dotted_list_p: self
                    .module
                    .declare_func_in_func(self.dotted_list_p_func, builder.func),
                circular_list_p: self
                    .module
                    .declare_func_in_func(self.circular_list_p_func, builder.func),
                null_list_p: self
                    .module
                    .declare_func_in_func(self.null_list_p_func, builder.func),
                file_exists_p: self
                    .module
                    .declare_func_in_func(self.file_exists_p_func, builder.func),
                port_eof_p: self
                    .module
                    .declare_func_in_func(self.port_eof_p_func, builder.func),
                eof_object: self
                    .module
                    .declare_func_in_func(self.eof_object_func, builder.func),
                make_promise: self
                    .module
                    .declare_func_in_func(self.make_promise_func, builder.func),
                force_forced: self
                    .module
                    .declare_func_in_func(self.force_forced_func, builder.func),
                alist_copy: self
                    .module
                    .declare_func_in_func(self.alist_copy_func, builder.func),
                delete_duplicates: self
                    .module
                    .declare_func_in_func(self.delete_duplicates_func, builder.func),
                vec_copy: self
                    .module
                    .declare_func_in_func(self.vec_copy_func, builder.func),
                vec_fill: self
                    .module
                    .declare_func_in_func(self.vec_fill_func, builder.func),
                vector_to_list: self
                    .module
                    .declare_func_in_func(self.vector_to_list_func, builder.func),
                vector_to_string: self
                    .module
                    .declare_func_in_func(self.vector_to_string_func, builder.func),
                vector_eq_p: self
                    .module
                    .declare_func_in_func(self.vector_eq_p_func, builder.func),
                vector_copy_from: self
                    .module
                    .declare_func_in_func(self.vector_copy_from_func, builder.func),
                vector_copy_slice: self
                    .module
                    .declare_func_in_func(self.vector_copy_slice_func, builder.func),
                vector_to_list_slice: self
                    .module
                    .declare_func_in_func(self.vector_to_list_slice_func, builder.func),
                vector_to_list_slice_from: self
                    .module
                    .declare_func_in_func(self.vector_to_list_slice_from_func, builder.func),
                vector_to_string_slice: self
                    .module
                    .declare_func_in_func(self.vector_to_string_slice_func, builder.func),
                vector_to_string_slice_from: self
                    .module
                    .declare_func_in_func(self.vector_to_string_slice_from_func, builder.func),
                vector_fill_from: self
                    .module
                    .declare_func_in_func(self.vector_fill_from_func, builder.func),
                vector_fill_slice: self
                    .module
                    .declare_func_in_func(self.vector_fill_slice_func, builder.func),
                vector_copy_bang: self
                    .module
                    .declare_func_in_func(self.vector_copy_bang_func, builder.func),
                vector_copy_bang_from: self
                    .module
                    .declare_func_in_func(self.vector_copy_bang_from_func, builder.func),
                vector_copy_bang_slice: self
                    .module
                    .declare_func_in_func(self.vector_copy_bang_slice_func, builder.func),
                string_p: self
                    .module
                    .declare_func_in_func(self.string_p_func, builder.func),
                string_length: self
                    .module
                    .declare_func_in_func(self.string_length_func, builder.func),
                string_lt: self
                    .module
                    .declare_func_in_func(self.string_lt_func, builder.func),
                string_gt: self
                    .module
                    .declare_func_in_func(self.string_gt_func, builder.func),
                string_le: self
                    .module
                    .declare_func_in_func(self.string_le_func, builder.func),
                string_ge: self
                    .module
                    .declare_func_in_func(self.string_ge_func, builder.func),
                string_ci_eq: self
                    .module
                    .declare_func_in_func(self.string_ci_eq_func, builder.func),
                string_ci_lt: self
                    .module
                    .declare_func_in_func(self.string_ci_lt_func, builder.func),
                string_ci_gt: self
                    .module
                    .declare_func_in_func(self.string_ci_gt_func, builder.func),
                string_ci_le: self
                    .module
                    .declare_func_in_func(self.string_ci_le_func, builder.func),
                string_ci_ge: self
                    .module
                    .declare_func_in_func(self.string_ci_ge_func, builder.func),
                string_downcase: self
                    .module
                    .declare_func_in_func(self.string_downcase_func, builder.func),
                string_upcase: self
                    .module
                    .declare_func_in_func(self.string_upcase_func, builder.func),
                string_foldcase: self
                    .module
                    .declare_func_in_func(self.string_foldcase_func, builder.func),
                string_titlecase: self
                    .module
                    .declare_func_in_func(self.string_titlecase_func, builder.func),
                string_reverse: self
                    .module
                    .declare_func_in_func(self.string_reverse_func, builder.func),
                string_prefix_p: self
                    .module
                    .declare_func_in_func(self.string_prefix_p_func, builder.func),
                string_suffix_p: self
                    .module
                    .declare_func_in_func(self.string_suffix_p_func, builder.func),
                bv_p: self
                    .module
                    .declare_func_in_func(self.bv_p_func, builder.func),
                bv_length: self
                    .module
                    .declare_func_in_func(self.bv_length_func, builder.func),
                bv_alloc: self
                    .module
                    .declare_func_in_func(self.bv_alloc_func, builder.func),
                bv_copy: self
                    .module
                    .declare_func_in_func(self.bv_copy_func, builder.func),
                bv_fill: self
                    .module
                    .declare_func_in_func(self.bv_fill_func, builder.func),
                bytevector_eq_p: self
                    .module
                    .declare_func_in_func(self.bytevector_eq_p_func, builder.func),
                bytevector_copy_from: self
                    .module
                    .declare_func_in_func(self.bytevector_copy_from_func, builder.func),
                bytevector_copy_slice: self
                    .module
                    .declare_func_in_func(self.bytevector_copy_slice_func, builder.func),
                bytevector_fill_from: self
                    .module
                    .declare_func_in_func(self.bytevector_fill_from_func, builder.func),
                bytevector_fill_slice: self
                    .module
                    .declare_func_in_func(self.bytevector_fill_slice_func, builder.func),
                bytevector_to_list_slice: self
                    .module
                    .declare_func_in_func(self.bytevector_to_list_slice_func, builder.func),
                bytevector_to_list_slice_from: self
                    .module
                    .declare_func_in_func(self.bytevector_to_list_slice_from_func, builder.func),
                bytevector_copy_bang: self
                    .module
                    .declare_func_in_func(self.bytevector_copy_bang_func, builder.func),
                bytevector_copy_bang_from: self
                    .module
                    .declare_func_in_func(self.bytevector_copy_bang_from_func, builder.func),
                bytevector_copy_bang_slice: self
                    .module
                    .declare_func_in_func(self.bytevector_copy_bang_slice_func, builder.func),
                bytevector_ieee_single_native_ref: self.module.declare_func_in_func(
                    self.bytevector_ieee_single_native_ref_func,
                    builder.func,
                ),
                bytevector_ieee_double_native_ref: self.module.declare_func_in_func(
                    self.bytevector_ieee_double_native_ref_func,
                    builder.func,
                ),
                bytevector_ieee_single_native_set: self.module.declare_func_in_func(
                    self.bytevector_ieee_single_native_set_func,
                    builder.func,
                ),
                bytevector_ieee_double_native_set: self.module.declare_func_in_func(
                    self.bytevector_ieee_double_native_set_func,
                    builder.func,
                ),
                hashtable_p: self
                    .module
                    .declare_func_in_func(self.hashtable_p_func, builder.func),
                hashtable_size: self
                    .module
                    .declare_func_in_func(self.hashtable_size_func, builder.func),
                hashtable_mutable_p: self
                    .module
                    .declare_func_in_func(self.hashtable_mutable_p_func, builder.func),
                hashtable_keys: self
                    .module
                    .declare_func_in_func(self.hashtable_keys_func, builder.func),
                hashtable_values: self
                    .module
                    .declare_func_in_func(self.hashtable_values_func, builder.func),
                hashtable_clear: self
                    .module
                    .declare_func_in_func(self.hashtable_clear_func, builder.func),
                hashtable_to_alist: self
                    .module
                    .declare_func_in_func(self.hashtable_to_alist_func, builder.func),
                hashtable_contains_p: self
                    .module
                    .declare_func_in_func(self.hashtable_contains_p_func, builder.func),
                hashtable_delete: self
                    .module
                    .declare_func_in_func(self.hashtable_delete_func, builder.func),
                hashtable_set: self
                    .module
                    .declare_func_in_func(self.hashtable_set_func, builder.func),
                hashtable_ref: self
                    .module
                    .declare_func_in_func(self.hashtable_ref_func, builder.func),
                hashtable_copy: self
                    .module
                    .declare_func_in_func(self.hashtable_copy_func, builder.func),
                hashtable_hash_function: self
                    .module
                    .declare_func_in_func(self.hashtable_hash_function_func, builder.func),
                make_hashtable_equal: self
                    .module
                    .declare_func_in_func(self.make_hashtable_equal_func, builder.func),
                make_hashtable_eq: self
                    .module
                    .declare_func_in_func(self.make_hashtable_eq_func, builder.func),
                make_hashtable_eqv: self
                    .module
                    .declare_func_in_func(self.make_hashtable_eqv_func, builder.func),
                str_copy: self
                    .module
                    .declare_func_in_func(self.str_copy_func, builder.func),
                substring: self
                    .module
                    .declare_func_in_func(self.substring_func, builder.func),
                string_copy_from: self
                    .module
                    .declare_func_in_func(self.string_copy_from_func, builder.func),
                string_take: self
                    .module
                    .declare_func_in_func(self.string_take_func, builder.func),
                string_take_right: self
                    .module
                    .declare_func_in_func(self.string_take_right_func, builder.func),
                string_drop: self
                    .module
                    .declare_func_in_func(self.string_drop_func, builder.func),
                string_drop_right: self
                    .module
                    .declare_func_in_func(self.string_drop_right_func, builder.func),
                string_pad: self
                    .module
                    .declare_func_in_func(self.string_pad_func, builder.func),
                string_pad_right: self
                    .module
                    .declare_func_in_func(self.string_pad_right_func, builder.func),
                string_split: self
                    .module
                    .declare_func_in_func(self.string_split_func, builder.func),
                string_join: self
                    .module
                    .declare_func_in_func(self.string_join_func, builder.func),
                string_replace_all: self
                    .module
                    .declare_func_in_func(self.string_replace_all_func, builder.func),
                string_replace_first: self
                    .module
                    .declare_func_in_func(self.string_replace_first_func, builder.func),
                string_trim: self
                    .module
                    .declare_func_in_func(self.string_trim_func, builder.func),
                string_trim_left: self
                    .module
                    .declare_func_in_func(self.string_trim_left_func, builder.func),
                string_trim_right: self
                    .module
                    .declare_func_in_func(self.string_trim_right_func, builder.func),
                string_to_list: self
                    .module
                    .declare_func_in_func(self.string_to_list_func, builder.func),
                string_to_vector: self
                    .module
                    .declare_func_in_func(self.string_to_vector_func, builder.func),
                string_to_list_slice: self
                    .module
                    .declare_func_in_func(self.string_to_list_slice_func, builder.func),
                string_to_list_slice_from: self
                    .module
                    .declare_func_in_func(self.string_to_list_slice_from_func, builder.func),
                string_to_vector_slice: self
                    .module
                    .declare_func_in_func(self.string_to_vector_slice_func, builder.func),
                string_to_vector_slice_from: self
                    .module
                    .declare_func_in_func(self.string_to_vector_slice_from_func, builder.func),
                list_to_vector: self
                    .module
                    .declare_func_in_func(self.list_to_vector_func, builder.func),
                list_to_string: self
                    .module
                    .declare_func_in_func(self.list_to_string_func, builder.func),
                string_ref: self
                    .module
                    .declare_func_in_func(self.string_ref_func, builder.func),
                str_set: self
                    .module
                    .declare_func_in_func(self.str_set_func, builder.func),
                alloc_string: self
                    .module
                    .declare_func_in_func(self.alloc_string_func, builder.func),
                str_fill: self
                    .module
                    .declare_func_in_func(self.str_fill_func, builder.func),
                string_fill_from: self
                    .module
                    .declare_func_in_func(self.string_fill_from_func, builder.func),
                string_fill_slice: self
                    .module
                    .declare_func_in_func(self.string_fill_slice_func, builder.func),
                string_copy_bang: self
                    .module
                    .declare_func_in_func(self.string_copy_bang_func, builder.func),
                string_copy_bang_from: self
                    .module
                    .declare_func_in_func(self.string_copy_bang_from_func, builder.func),
                string_copy_bang_slice: self
                    .module
                    .declare_func_in_func(self.string_copy_bang_slice_func, builder.func),
                list_tail: self
                    .module
                    .declare_func_in_func(self.list_tail_func, builder.func),
                list_ref: self
                    .module
                    .declare_func_in_func(self.list_ref_func, builder.func),
                list_set: self
                    .module
                    .declare_func_in_func(self.list_set_func, builder.func),
                take: self
                    .module
                    .declare_func_in_func(self.take_func, builder.func),
                drop: self
                    .module
                    .declare_func_in_func(self.drop_func, builder.func),
                make_list: self
                    .module
                    .declare_func_in_func(self.make_list_fill_func, builder.func),
                make_list_unspec: self
                    .module
                    .declare_func_in_func(self.make_list_unspec_func, builder.func),
                make_vector_unspec: self
                    .module
                    .declare_func_in_func(self.make_vector_unspec_func, builder.func),
                bitwise_bit_count: self
                    .module
                    .declare_func_in_func(self.bitwise_bit_count_func, builder.func),
                bitwise_length: self
                    .module
                    .declare_func_in_func(self.bitwise_length_func, builder.func),
                fx_first_bit_set: self
                    .module
                    .declare_func_in_func(self.fx_first_bit_set_func, builder.func),
                bitwise_bit_set_p: self
                    .module
                    .declare_func_in_func(self.bitwise_bit_set_p_func, builder.func),
                exact_non_neg_int_p: self
                    .module
                    .declare_func_in_func(self.exact_nonneg_int_p_func, builder.func),
                equal_any: self
                    .module
                    .declare_func_in_func(self.equal_func, builder.func),
                expt: self
                    .module
                    .declare_func_in_func(self.expt_func, builder.func),
                gcd: self
                    .module
                    .declare_func_in_func(self.gcd_func, builder.func),
                lcm: self
                    .module
                    .declare_func_in_func(self.lcm_func, builder.func),
                arith_shift: self
                    .module
                    .declare_func_in_func(self.arith_shift_func, builder.func),
                bitwise_arith_shift_left: self
                    .module
                    .declare_func_in_func(self.bitwise_arith_shift_left_func, builder.func),
                bitwise_arith_shift_right: self
                    .module
                    .declare_func_in_func(self.bitwise_arith_shift_right_func, builder.func),
                div_euclid: self
                    .module
                    .declare_func_in_func(self.div_euclid_func, builder.func),
                mod_euclid: self
                    .module
                    .declare_func_in_func(self.mod_euclid_func, builder.func),
                number_to_string: self
                    .module
                    .declare_func_in_func(self.number_to_string_func, builder.func),
                string_to_number: self
                    .module
                    .declare_func_in_func(self.string_to_number_func, builder.func),
                string_contains: self
                    .module
                    .declare_func_in_func(self.string_contains_func, builder.func),
                string_contains_right: self
                    .module
                    .declare_func_in_func(self.string_contains_right_func, builder.func),
                number_to_string_radix: self
                    .module
                    .declare_func_in_func(self.number_to_string_radix_func, builder.func),
                string_to_number_radix: self
                    .module
                    .declare_func_in_func(self.string_to_number_radix_func, builder.func),
                iota_n: self
                    .module
                    .declare_func_in_func(self.iota_n_func, builder.func),
                iota_ns: self
                    .module
                    .declare_func_in_func(self.iota_ns_func, builder.func),
                iota_nss: self
                    .module
                    .declare_func_in_func(self.iota_nss_func, builder.func),
                digit_value: self
                    .module
                    .declare_func_in_func(self.digit_value_func, builder.func),
                string_index: self
                    .module
                    .declare_func_in_func(self.string_index_func, builder.func),
                string_index_right: self
                    .module
                    .declare_func_in_func(self.string_index_right_func, builder.func),
                string_append_buf: self
                    .module
                    .declare_func_in_func(self.string_append_buf_func, builder.func),
                vector_append_buf: self
                    .module
                    .declare_func_in_func(self.vector_append_buf_func, builder.func),
                make_vector_buf: self
                    .module
                    .declare_func_in_func(self.make_vector_buf_func, builder.func),
                make_bytevector_buf: self
                    .module
                    .declare_func_in_func(self.make_bytevector_buf_func, builder.func),
                bytevector_append_buf: self
                    .module
                    .declare_func_in_func(self.bytevector_append_buf_func, builder.func),
                string_to_symbol: self
                    .module
                    .declare_func_in_func(self.string_to_symbol_func, builder.func),
                symbol_to_string: self
                    .module
                    .declare_func_in_func(self.symbol_to_string_func, builder.func),
                string_hash: self
                    .module
                    .declare_func_in_func(self.string_hash_func, builder.func),
                symbol_hash: self
                    .module
                    .declare_func_in_func(self.symbol_hash_func, builder.func),
                equal_hash: self
                    .module
                    .declare_func_in_func(self.equal_hash_func, builder.func),
                fixnum_to_nb: self
                    .module
                    .declare_func_in_func(self.fixnum_to_nb_func, builder.func),
                set_car: self
                    .module
                    .declare_func_in_func(self.set_car_func, builder.func),
                set_cdr: self
                    .module
                    .declare_func_in_func(self.set_cdr_func, builder.func),
                length: self
                    .module
                    .declare_func_in_func(self.length_func, builder.func),
                last: self
                    .module
                    .declare_func_in_func(self.last_func, builder.func),
                append_buf: self
                    .module
                    .declare_func_in_func(self.append_buf_func, builder.func),
                bytevector_to_u8_list: self
                    .module
                    .declare_func_in_func(self.bytevector_to_u8_list_func, builder.func),
                u8_list_to_bytevector: self
                    .module
                    .declare_func_in_func(self.u8_list_to_bytevector_func, builder.func),
                string_to_utf8: self
                    .module
                    .declare_func_in_func(self.string_to_utf8_func, builder.func),
                utf8_to_string: self
                    .module
                    .declare_func_in_func(self.utf8_to_string_func, builder.func),
                port_position: self
                    .module
                    .declare_func_in_func(self.port_position_func, builder.func),
                port_has_set_port_position_p: self
                    .module
                    .declare_func_in_func(self.port_has_set_port_position_p_func, builder.func),
                current_jiffy: self
                    .module
                    .declare_func_in_func(self.current_jiffy_func, builder.func),
                current_second: self
                    .module
                    .declare_func_in_func(self.current_second_func, builder.func),
                bv_u8_ref: self
                    .module
                    .declare_func_in_func(self.bv_u8_ref_func, builder.func),
                bv_u8_set: self
                    .module
                    .declare_func_in_func(self.bv_u8_set_func, builder.func),
                bv_s8_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_s8_ref_func, builder.func),
                bv_s8_set: self
                    .module
                    .declare_func_in_func(self.bytevector_s8_set_func, builder.func),
                bv_u16_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_u16_native_ref_func, builder.func),
                bv_u16_set: self
                    .module
                    .declare_func_in_func(self.bytevector_u16_native_set_func, builder.func),
                bv_s16_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_s16_native_ref_func, builder.func),
                bv_s16_set: self
                    .module
                    .declare_func_in_func(self.bytevector_s16_native_set_func, builder.func),
                bv_u32_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_u32_native_ref_func, builder.func),
                bv_u32_set: self
                    .module
                    .declare_func_in_func(self.bytevector_u32_native_set_func, builder.func),
                bv_s32_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_s32_native_ref_func, builder.func),
                bv_s32_set: self
                    .module
                    .declare_func_in_func(self.bytevector_s32_native_set_func, builder.func),
                bv_u64_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_u64_native_ref_func, builder.func),
                bv_u64_set: self
                    .module
                    .declare_func_in_func(self.bytevector_u64_native_set_func, builder.func),
                bv_s64_ref: self
                    .module
                    .declare_func_in_func(self.bytevector_s64_native_ref_func, builder.func),
                bv_s64_set: self
                    .module
                    .declare_func_in_func(self.bytevector_s64_native_set_func, builder.func),
                div0: self
                    .module
                    .declare_func_in_func(self.div0_func, builder.func),
                mod0: self
                    .module
                    .declare_func_in_func(self.mod0_func, builder.func),
                make_string_buf: self
                    .module
                    .declare_func_in_func(self.make_string_buf_func, builder.func),
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
                // Tail-position CallSelf detection. Emits `return_call`
                // instead of a regular `call` so deeply recursive
                // bodies (tak, etc.) don't burn host stack.
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
    /// `vm_eq_any(a, b) -> i64` (0/1). `Inst::EqAny` — pointer/`eq?`
    /// identity over Any operands. Consumes both NB carriers.
    eq_any: cranelift_codegen::ir::FuncRef,
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
    #[cfg(feature = "regions")]
    alloc_pair_region: cranelift_codegen::ir::FuncRef,
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
    // #50 — typed list builtins (pointer-in → pointer/boolean out). All
    // `vm_*_gc` helpers decode NB carriers natively, so uniform-NB calls
    // the same ones the specialized tier does.
    assq: cranelift_codegen::ir::FuncRef,
    assv: cranelift_codegen::ir::FuncRef,
    assoc: cranelift_codegen::ir::FuncRef,
    memq: cranelift_codegen::ir::FuncRef,
    memv: cranelift_codegen::ir::FuncRef,
    member: cranelift_codegen::ir::FuncRef,
    reverse: cranelift_codegen::ir::FuncRef,
    list_copy: cranelift_codegen::ir::FuncRef,
    last_pair: cranelift_codegen::ir::FuncRef,
    concatenate: cranelift_codegen::ir::FuncRef,
    append_reverse: cranelift_codegen::ir::FuncRef,
    not_pair_p: cranelift_codegen::ir::FuncRef,
    list_p: cranelift_codegen::ir::FuncRef,
    proper_list_p: cranelift_codegen::ir::FuncRef,
    // #50 — char builtins. Helpers take/return a RAW codepoint i64, so
    // uniform-NB decodes the NB Character payload in and re-tags out.
    char_upcase: cranelift_codegen::ir::FuncRef,
    char_downcase: cranelift_codegen::ir::FuncRef,
    char_foldcase: cranelift_codegen::ir::FuncRef,
    char_titlecase: cranelift_codegen::ir::FuncRef,
    char_alphabetic_p: cranelift_codegen::ir::FuncRef,
    char_numeric_p: cranelift_codegen::ir::FuncRef,
    char_whitespace_p: cranelift_codegen::ir::FuncRef,
    char_upper_case_p: cranelift_codegen::ir::FuncRef,
    char_lower_case_p: cranelift_codegen::ir::FuncRef,
    // #50 — flonum transcendentals. NB Flonum IS the f64 bit pattern, so
    // these are pass-through helper calls (no encode/decode).
    flonum_sin: cranelift_codegen::ir::FuncRef,
    flonum_cos: cranelift_codegen::ir::FuncRef,
    flonum_tan: cranelift_codegen::ir::FuncRef,
    flonum_asin: cranelift_codegen::ir::FuncRef,
    flonum_acos: cranelift_codegen::ir::FuncRef,
    flonum_atan: cranelift_codegen::ir::FuncRef,
    flonum_exp: cranelift_codegen::ir::FuncRef,
    flonum_log: cranelift_codegen::ir::FuncRef,
    flonum_expt: cranelift_codegen::ir::FuncRef,
    flonum_log2: cranelift_codegen::ir::FuncRef,
    flonum_atan2: cranelift_codegen::ir::FuncRef,
    flonum_is_integer: cranelift_codegen::ir::FuncRef,
    fl_even_p: cranelift_codegen::ir::FuncRef,
    fl_odd_p: cranelift_codegen::ir::FuncRef,
    // #50 — misc predicate / pointer builtins.
    symbol_p: cranelift_codegen::ir::FuncRef,
    procedure_p: cranelift_codegen::ir::FuncRef,
    port_p: cranelift_codegen::ir::FuncRef,
    dotted_list_p: cranelift_codegen::ir::FuncRef,
    circular_list_p: cranelift_codegen::ir::FuncRef,
    null_list_p: cranelift_codegen::ir::FuncRef,
    file_exists_p: cranelift_codegen::ir::FuncRef,
    port_eof_p: cranelift_codegen::ir::FuncRef,
    eof_object: cranelift_codegen::ir::FuncRef,
    make_promise: cranelift_codegen::ir::FuncRef,
    force_forced: cranelift_codegen::ir::FuncRef,
    alist_copy: cranelift_codegen::ir::FuncRef,
    delete_duplicates: cranelift_codegen::ir::FuncRef,
    // #50 — vector builtins (NB vector pointer + raw index args; the
    // gc-helpers deopt internally on out-of-range).
    vec_copy: cranelift_codegen::ir::FuncRef,
    vec_fill: cranelift_codegen::ir::FuncRef,
    vector_to_list: cranelift_codegen::ir::FuncRef,
    vector_to_string: cranelift_codegen::ir::FuncRef,
    vector_eq_p: cranelift_codegen::ir::FuncRef,
    vector_copy_from: cranelift_codegen::ir::FuncRef,
    vector_copy_slice: cranelift_codegen::ir::FuncRef,
    vector_to_list_slice: cranelift_codegen::ir::FuncRef,
    vector_to_list_slice_from: cranelift_codegen::ir::FuncRef,
    vector_to_string_slice: cranelift_codegen::ir::FuncRef,
    vector_to_string_slice_from: cranelift_codegen::ir::FuncRef,
    vector_fill_from: cranelift_codegen::ir::FuncRef,
    vector_fill_slice: cranelift_codegen::ir::FuncRef,
    vector_copy_bang: cranelift_codegen::ir::FuncRef,
    vector_copy_bang_from: cranelift_codegen::ir::FuncRef,
    vector_copy_bang_slice: cranelift_codegen::ir::FuncRef,
    // #50 — string predicate/comparison/transform/hash builtins
    // (pointer-in → pointer / boolean / Fixnum out).
    string_p: cranelift_codegen::ir::FuncRef,
    string_length: cranelift_codegen::ir::FuncRef,
    string_lt: cranelift_codegen::ir::FuncRef,
    string_gt: cranelift_codegen::ir::FuncRef,
    string_le: cranelift_codegen::ir::FuncRef,
    string_ge: cranelift_codegen::ir::FuncRef,
    string_ci_eq: cranelift_codegen::ir::FuncRef,
    string_ci_lt: cranelift_codegen::ir::FuncRef,
    string_ci_gt: cranelift_codegen::ir::FuncRef,
    string_ci_le: cranelift_codegen::ir::FuncRef,
    string_ci_ge: cranelift_codegen::ir::FuncRef,
    string_downcase: cranelift_codegen::ir::FuncRef,
    string_upcase: cranelift_codegen::ir::FuncRef,
    string_foldcase: cranelift_codegen::ir::FuncRef,
    string_titlecase: cranelift_codegen::ir::FuncRef,
    string_reverse: cranelift_codegen::ir::FuncRef,
    string_prefix_p: cranelift_codegen::ir::FuncRef,
    string_suffix_p: cranelift_codegen::ir::FuncRef,
    // #50 — bytevector builtins (NB bv pointer + raw byte/index/count
    // args; NB Flonum f64-bit values pass through).
    bv_p: cranelift_codegen::ir::FuncRef,
    bv_length: cranelift_codegen::ir::FuncRef,
    bv_alloc: cranelift_codegen::ir::FuncRef,
    bv_copy: cranelift_codegen::ir::FuncRef,
    bv_fill: cranelift_codegen::ir::FuncRef,
    bytevector_eq_p: cranelift_codegen::ir::FuncRef,
    bytevector_copy_from: cranelift_codegen::ir::FuncRef,
    bytevector_copy_slice: cranelift_codegen::ir::FuncRef,
    bytevector_fill_from: cranelift_codegen::ir::FuncRef,
    bytevector_fill_slice: cranelift_codegen::ir::FuncRef,
    bytevector_to_list_slice: cranelift_codegen::ir::FuncRef,
    bytevector_to_list_slice_from: cranelift_codegen::ir::FuncRef,
    bytevector_copy_bang: cranelift_codegen::ir::FuncRef,
    bytevector_copy_bang_from: cranelift_codegen::ir::FuncRef,
    bytevector_copy_bang_slice: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_single_native_ref: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_double_native_ref: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_single_native_set: cranelift_codegen::ir::FuncRef,
    bytevector_ieee_double_native_set: cranelift_codegen::ir::FuncRef,
    // #50 — hashtable builtins (HT pointer + NB keys/values; no index
    // decoding).
    hashtable_p: cranelift_codegen::ir::FuncRef,
    hashtable_size: cranelift_codegen::ir::FuncRef,
    hashtable_mutable_p: cranelift_codegen::ir::FuncRef,
    hashtable_keys: cranelift_codegen::ir::FuncRef,
    hashtable_values: cranelift_codegen::ir::FuncRef,
    hashtable_clear: cranelift_codegen::ir::FuncRef,
    hashtable_to_alist: cranelift_codegen::ir::FuncRef,
    hashtable_contains_p: cranelift_codegen::ir::FuncRef,
    hashtable_delete: cranelift_codegen::ir::FuncRef,
    hashtable_set: cranelift_codegen::ir::FuncRef,
    hashtable_ref: cranelift_codegen::ir::FuncRef,
    hashtable_copy: cranelift_codegen::ir::FuncRef,
    hashtable_hash_function: cranelift_codegen::ir::FuncRef,
    make_hashtable_equal: cranelift_codegen::ir::FuncRef,
    make_hashtable_eq: cranelift_codegen::ir::FuncRef,
    make_hashtable_eqv: cranelift_codegen::ir::FuncRef,
    // #50 — string pointer/index builtins (string pointer + raw
    // count/index args → string/list/vector pointer).
    str_copy: cranelift_codegen::ir::FuncRef,
    substring: cranelift_codegen::ir::FuncRef,
    string_copy_from: cranelift_codegen::ir::FuncRef,
    string_take: cranelift_codegen::ir::FuncRef,
    string_take_right: cranelift_codegen::ir::FuncRef,
    string_drop: cranelift_codegen::ir::FuncRef,
    string_drop_right: cranelift_codegen::ir::FuncRef,
    string_pad: cranelift_codegen::ir::FuncRef,
    string_pad_right: cranelift_codegen::ir::FuncRef,
    string_split: cranelift_codegen::ir::FuncRef,
    string_join: cranelift_codegen::ir::FuncRef,
    string_replace_all: cranelift_codegen::ir::FuncRef,
    string_replace_first: cranelift_codegen::ir::FuncRef,
    string_trim: cranelift_codegen::ir::FuncRef,
    string_trim_left: cranelift_codegen::ir::FuncRef,
    string_trim_right: cranelift_codegen::ir::FuncRef,
    string_to_list: cranelift_codegen::ir::FuncRef,
    string_to_vector: cranelift_codegen::ir::FuncRef,
    string_to_list_slice: cranelift_codegen::ir::FuncRef,
    string_to_list_slice_from: cranelift_codegen::ir::FuncRef,
    string_to_vector_slice: cranelift_codegen::ir::FuncRef,
    string_to_vector_slice_from: cranelift_codegen::ir::FuncRef,
    list_to_vector: cranelift_codegen::ir::FuncRef,
    list_to_string: cranelift_codegen::ir::FuncRef,
    // #50 — string codepoint builtins (char args/results decode/encode
    // the NB Character codepoint payload; indices decode NB→raw).
    string_ref: cranelift_codegen::ir::FuncRef,
    str_set: cranelift_codegen::ir::FuncRef,
    alloc_string: cranelift_codegen::ir::FuncRef,
    str_fill: cranelift_codegen::ir::FuncRef,
    string_fill_from: cranelift_codegen::ir::FuncRef,
    string_fill_slice: cranelift_codegen::ir::FuncRef,
    string_copy_bang: cranelift_codegen::ir::FuncRef,
    string_copy_bang_from: cranelift_codegen::ir::FuncRef,
    string_copy_bang_slice: cranelift_codegen::ir::FuncRef,
    // #50 — list-index, fixnum-bit, and predicate builtins.
    list_tail: cranelift_codegen::ir::FuncRef,
    list_ref: cranelift_codegen::ir::FuncRef,
    list_set: cranelift_codegen::ir::FuncRef,
    take: cranelift_codegen::ir::FuncRef,
    drop: cranelift_codegen::ir::FuncRef,
    make_list: cranelift_codegen::ir::FuncRef,
    make_list_unspec: cranelift_codegen::ir::FuncRef,
    make_vector_unspec: cranelift_codegen::ir::FuncRef,
    bitwise_bit_count: cranelift_codegen::ir::FuncRef,
    bitwise_length: cranelift_codegen::ir::FuncRef,
    fx_first_bit_set: cranelift_codegen::ir::FuncRef,
    bitwise_bit_set_p: cranelift_codegen::ir::FuncRef,
    exact_non_neg_int_p: cranelift_codegen::ir::FuncRef,
    equal_any: cranelift_codegen::ir::FuncRef,
    // #50 — fixnum `_fx` arithmetic (raw operands + raw result that may
    // overflow 47 bits → 47-bit check + deopt via nb_fx_call_or_deopt).
    expt: cranelift_codegen::ir::FuncRef,
    gcd: cranelift_codegen::ir::FuncRef,
    lcm: cranelift_codegen::ir::FuncRef,
    arith_shift: cranelift_codegen::ir::FuncRef,
    bitwise_arith_shift_left: cranelift_codegen::ir::FuncRef,
    bitwise_arith_shift_right: cranelift_codegen::ir::FuncRef,
    div_euclid: cranelift_codegen::ir::FuncRef,
    mod_euclid: cranelift_codegen::ir::FuncRef,
    // #50 — number/string conversions + substring search (NB pass-through:
    // number/string operands and number-or-#f / index-or-#f results all
    // ride the NB lane unchanged).
    number_to_string: cranelift_codegen::ir::FuncRef,
    string_to_number: cranelift_codegen::ir::FuncRef,
    string_contains: cranelift_codegen::ir::FuncRef,
    string_contains_right: cranelift_codegen::ir::FuncRef,
    // #50 — radix conversions, iota, digit-value, string-index (NB
    // number/string + raw radix/count/start/step/codepoint args).
    number_to_string_radix: cranelift_codegen::ir::FuncRef,
    string_to_number_radix: cranelift_codegen::ir::FuncRef,
    iota_n: cranelift_codegen::ir::FuncRef,
    iota_ns: cranelift_codegen::ir::FuncRef,
    iota_nss: cranelift_codegen::ir::FuncRef,
    digit_value: cranelift_codegen::ir::FuncRef,
    string_index: cranelift_codegen::ir::FuncRef,
    string_index_right: cranelift_codegen::ir::FuncRef,
    // #50 — variadic buffer builders (NB args via a stack buffer).
    string_append_buf: cranelift_codegen::ir::FuncRef,
    vector_append_buf: cranelift_codegen::ir::FuncRef,
    make_vector_buf: cranelift_codegen::ir::FuncRef,
    make_bytevector_buf: cranelift_codegen::ir::FuncRef,
    bytevector_append_buf: cranelift_codegen::ir::FuncRef,
    // #50 — symbol<->string. Helpers use a RAW symbol id (not NB), so
    // uniform-NB decodes/encodes the NB Symbol's id payload.
    string_to_symbol: cranelift_codegen::ir::FuncRef,
    symbol_to_string: cranelift_codegen::ir::FuncRef,
    // #50 — hash builtins. Helpers return a full-range i64 hash; encode
    // via `fixnum_to_nb` (bignum if >47-bit) rather than truncating.
    string_hash: cranelift_codegen::ir::FuncRef,
    symbol_hash: cranelift_codegen::ir::FuncRef,
    equal_hash: cranelift_codegen::ir::FuncRef,
    fixnum_to_nb: cranelift_codegen::ir::FuncRef,
    // #50 — pair mutate / list / conversions / port / time builtins.
    set_car: cranelift_codegen::ir::FuncRef,
    set_cdr: cranelift_codegen::ir::FuncRef,
    length: cranelift_codegen::ir::FuncRef,
    last: cranelift_codegen::ir::FuncRef,
    append_buf: cranelift_codegen::ir::FuncRef,
    bytevector_to_u8_list: cranelift_codegen::ir::FuncRef,
    u8_list_to_bytevector: cranelift_codegen::ir::FuncRef,
    string_to_utf8: cranelift_codegen::ir::FuncRef,
    utf8_to_string: cranelift_codegen::ir::FuncRef,
    port_position: cranelift_codegen::ir::FuncRef,
    port_has_set_port_position_p: cranelift_codegen::ir::FuncRef,
    current_jiffy: cranelift_codegen::ir::FuncRef,
    current_second: cranelift_codegen::ir::FuncRef,
    // #50 — bytevector typed accessors (bv pointer + raw offset; ref →
    // fixnum-or-bignum via fixnum_to_nb, set takes a raw int value),
    // Div0/Mod0 (fx arith), and string (build).
    bv_u8_ref: cranelift_codegen::ir::FuncRef,
    bv_u8_set: cranelift_codegen::ir::FuncRef,
    bv_s8_ref: cranelift_codegen::ir::FuncRef,
    bv_s8_set: cranelift_codegen::ir::FuncRef,
    bv_u16_ref: cranelift_codegen::ir::FuncRef,
    bv_u16_set: cranelift_codegen::ir::FuncRef,
    bv_s16_ref: cranelift_codegen::ir::FuncRef,
    bv_s16_set: cranelift_codegen::ir::FuncRef,
    bv_u32_ref: cranelift_codegen::ir::FuncRef,
    bv_u32_set: cranelift_codegen::ir::FuncRef,
    bv_s32_ref: cranelift_codegen::ir::FuncRef,
    bv_s32_set: cranelift_codegen::ir::FuncRef,
    bv_u64_ref: cranelift_codegen::ir::FuncRef,
    bv_u64_set: cranelift_codegen::ir::FuncRef,
    bv_s64_ref: cranelift_codegen::ir::FuncRef,
    bv_s64_set: cranelift_codegen::ir::FuncRef,
    div0: cranelift_codegen::ir::FuncRef,
    mod0: cranelift_codegen::ir::FuncRef,
    make_string_buf: cranelift_codegen::ir::FuncRef,
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
            // Layer-3 region-allocating cons (#51 escape analysis). Same
            // NB-operand shape as `Inst::Cons` but routes to
            // `vm_alloc_pair_region`, which allocates the pair in the
            // innermost in-scope region (or falls back to Rc if none).
            // Only reachable when `regions` is on; the prewalk gates
            // ConsRegion behind the same feature, so without `regions`
            // it never reaches lowering (declines → VM).
            #[cfg(feature = "regions")]
            &Inst::ConsRegion(dst, car, _car_tag, cdr, _cdr_tag) => {
                let car_v = lookup(map, car)?;
                let cdr_v = lookup(map, cdr)?;
                let any_tag = b.ins().iconst(I64, cs_vm::vm::JIT_RT_ANY as i64);
                let call = b
                    .ins()
                    .call(helpers.alloc_pair_region, &[car_v, any_tag, cdr_v, any_tag]);
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
            // EqAny: `eq?`-style identity over Any operands. The helper
            // `vm_eq_any` decodes both NB carriers (consuming them) and
            // returns a raw 0/1; re-encode that as an NB Boolean. Same
            // 0/1→NB pattern as PairP/NullP/VecP. (#50 — the last inst a
            // real benchmark, contract-overhead, needed off pure-fixnum.)
            &Inst::EqAny(dst, lhs, rhs) => {
                let l = lookup(map, lhs)?;
                let r = lookup(map, rhs)?;
                let call = b.ins().call(helpers.eq_any, &[l, r]);
                let raw_bit = b.inst_results(call)[0];
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let result = b.ins().bor_imm(raw_bit, nb_false);
                map.insert(dst, result);
            }
            // NotBoolean: Scheme `(not x)` — #t iff x is #f. In NB,
            // falsy ⟺ the carrier equals NB_FALSE; everything else is
            // truthy. So `dst = (src == NB_FALSE) ? #t : #f`, encoded as
            // `(src == NB_FALSE) | NB_FALSE` (1|NB_FALSE = NB_TRUE,
            // 0|NB_FALSE = NB_FALSE). The translator only emits this for
            // a Boolean-typed src, but computing the full predicate makes
            // it correct for any NB value (e.g. an AnyTruthy result that
            // uniform-NB lowers as an identity passthrough). Result is an
            // NB Boolean (never raw).
            &Inst::NotBoolean(dst, src) => {
                let v = lookup(map, src)?;
                let nb_false = cs_vm::vm::NanboxValue::FALSE.into_raw();
                let is_false = b.ins().icmp_imm(IntCC::Equal, v, nb_false);
                let widened = b.ins().uextend(I64, is_false);
                let result = b.ins().bor_imm(widened, nb_false);
                map.insert(dst, result);
            }
            // IntCharBitcast (integer->char): same codepoint payload,
            // strip the Fixnum header and OR in the Character header.
            // In NB land both Fixnum and Character carry the codepoint
            // in the low 47 bits — only the tag differs.
            &Inst::IntCharBitcast(dst, src) => {
                let v = lookup(map, src)?;
                let payload = b.ins().band_imm(v, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let char_tag_bits = (cs_vm::vm::NB_SIGNATURE_BITS
                    | ((cs_vm::vm::NB_TAG_CHARACTER as u64) << 47))
                    as i64;
                let retagged = b.ins().bor_imm(payload, char_tag_bits);
                map.insert(dst, retagged);
            }
            // Move: SSA copy. Carry both the NB carrier and (if present)
            // the raw lane so a moved Fixnum stays unboxed under the raw
            // ABI. (char->integer no longer lowers to Move — it uses
            // CharToInt — so a Move never carries a tag/type mismatch.)
            &Inst::Move(dst, src) => {
                let v = lookup(map, src)?;
                map.insert(dst, v);
                if let Some(&rv) = raw.get(&src) {
                    raw.insert(dst, rv);
                }
            }
            // #50 — integer ops formerly only on the pure-fixnum tier.
            // Each takes a tag-checked Fixnum fast path and deopts (→
            // bytecode) on a non-Fixnum operand (bignum), a zero divisor,
            // or a 47-bit overflow. Bitwise/min/max never overflow.
            &Inst::BitAnd(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_binop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().band(l, r),
                );
                map.insert(dst, r);
            }
            &Inst::BitOr(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_binop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().bor(l, r),
                );
                map.insert(dst, r);
            }
            &Inst::BitXor(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_binop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().bxor(l, r),
                );
                map.insert(dst, r);
            }
            &Inst::MaxFixnum(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_binop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().smax(l, r),
                );
                map.insert(dst, r);
            }
            &Inst::MinFixnum(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_binop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().smin(l, r),
                );
                map.insert(dst, r);
            }
            &Inst::BitNot(dst, src) => {
                let r = emit_nb_fixnum_unop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, src)?,
                    false, // ~x stays in 47-bit range
                    |b, x| b.ins().bnot(x),
                );
                map.insert(dst, r);
            }
            &Inst::AbsFixnum(dst, src) => {
                let r = emit_nb_fixnum_unop_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, src)?,
                    true, // abs(-2^46) overflows
                    |b, x| b.ins().iabs(x),
                );
                map.insert(dst, r);
            }
            &Inst::Quotient(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_div_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().sdiv(l, r),
                );
                map.insert(dst, r);
            }
            &Inst::Remainder(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_div_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| b.ins().srem(l, r),
                );
                map.insert(dst, r);
            }
            // R6RS modulo: result takes the sign of the divisor. Compute
            // srem, then add the divisor when the remainder is non-zero
            // and its sign differs from the divisor's (mirrors the
            // specialized tier).
            &Inst::Modulo(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_div_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| {
                        let rem = b.ins().srem(l, r);
                        let zero = b.ins().iconst(I64, 0);
                        let rem_nz = b.ins().icmp(IntCC::NotEqual, rem, zero);
                        let rem_neg = b.ins().icmp(IntCC::SignedLessThan, rem, zero);
                        let div_neg = b.ins().icmp(IntCC::SignedLessThan, r, zero);
                        let sign_diff = b.ins().bxor(rem_neg, div_neg);
                        let adjust = b.ins().band(rem_nz, sign_diff);
                        let adjusted = b.ins().iadd(rem, r);
                        b.ins().select(adjust, adjusted, rem)
                    },
                );
                map.insert(dst, r);
            }
            // R7RS floor-quotient: sdiv truncates toward zero; subtract 1
            // when the remainder is non-zero and the operand signs differ.
            &Inst::FloorQuotient(dst, lhs, rhs) => {
                let r = emit_nb_fixnum_div_or_deopt(
                    b,
                    helpers.request_deopt,
                    lookup(map, lhs)?,
                    lookup(map, rhs)?,
                    |b, l, r| {
                        let q = b.ins().sdiv(l, r);
                        let rem = b.ins().srem(l, r);
                        let zero = b.ins().iconst(I64, 0);
                        let rem_nz = b.ins().icmp(IntCC::NotEqual, rem, zero);
                        let l_neg = b.ins().icmp(IntCC::SignedLessThan, l, zero);
                        let r_neg = b.ins().icmp(IntCC::SignedLessThan, r, zero);
                        let sign_diff = b.ins().bxor(l_neg, r_neg);
                        let adjust = b.ins().band(rem_nz, sign_diff);
                        let q_dec = b.ins().iadd_imm(q, -1);
                        b.ins().select(adjust, q_dec, q)
                    },
                );
                map.insert(dst, r);
            }
            // #50 — typed list builtins. Pointer-in → Gc-handle out: call
            // the `vm_*_gc` helper (decodes NB natively), result is an NB
            // carrier marked for stack-map tracking. The translator's
            // linearity (AnyClone before consuming uses) makes the
            // consuming helper calls correct, same as the specialized tier.
            &Inst::Assq(dst, key, alist) => {
                let r = nb_ptr_call(b, helpers.assq, &[lookup(map, key)?, lookup(map, alist)?]);
                map.insert(dst, r);
            }
            &Inst::Assv(dst, key, alist) => {
                let r = nb_ptr_call(b, helpers.assv, &[lookup(map, key)?, lookup(map, alist)?]);
                map.insert(dst, r);
            }
            &Inst::Assoc(dst, key, alist) => {
                let r = nb_ptr_call(b, helpers.assoc, &[lookup(map, key)?, lookup(map, alist)?]);
                map.insert(dst, r);
            }
            &Inst::Memq(dst, item, lst) => {
                let r = nb_ptr_call(b, helpers.memq, &[lookup(map, item)?, lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::Memv(dst, item, lst) => {
                let r = nb_ptr_call(b, helpers.memv, &[lookup(map, item)?, lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::Member(dst, item, lst) => {
                let r = nb_ptr_call(b, helpers.member, &[lookup(map, item)?, lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::Reverse(dst, lst) => {
                let r = nb_ptr_call(b, helpers.reverse, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::ListCopy(dst, lst) => {
                let r = nb_ptr_call(b, helpers.list_copy, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::LastPair(dst, lst) => {
                let r = nb_ptr_call(b, helpers.last_pair, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::Concatenate(dst, lst) => {
                let r = nb_ptr_call(b, helpers.concatenate, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::AppendReverse(dst, lst, tail) => {
                let r = nb_ptr_call(
                    b,
                    helpers.append_reverse,
                    &[lookup(map, lst)?, lookup(map, tail)?],
                );
                map.insert(dst, r);
            }
            // List predicates → NB Boolean.
            &Inst::NotPairP(dst, src) => {
                let r = nb_bool_call(b, helpers.not_pair_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::ListP(dst, src) => {
                let r = nb_bool_call(b, helpers.list_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::ProperListP(dst, src) => {
                let r = nb_bool_call(b, helpers.proper_list_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            // #50 — char transforms: NB Character → NB Character.
            &Inst::CharUpcase(dst, src) => {
                let r = nb_char_op(b, helpers.char_upcase, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharDowncase(dst, src) => {
                let r = nb_char_op(b, helpers.char_downcase, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharFoldcase(dst, src) => {
                let r = nb_char_op(b, helpers.char_foldcase, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharTitlecase(dst, src) => {
                let r = nb_char_op(b, helpers.char_titlecase, lookup(map, src)?);
                map.insert(dst, r);
            }
            // #50 — char predicates: NB Character → NB Boolean.
            &Inst::CharAlphabeticP(dst, src) => {
                let r = nb_char_pred(b, helpers.char_alphabetic_p, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharNumericP(dst, src) => {
                let r = nb_char_pred(b, helpers.char_numeric_p, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharWhitespaceP(dst, src) => {
                let r = nb_char_pred(b, helpers.char_whitespace_p, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharUpperCaseP(dst, src) => {
                let r = nb_char_pred(b, helpers.char_upper_case_p, lookup(map, src)?);
                map.insert(dst, r);
            }
            &Inst::CharLowerCaseP(dst, src) => {
                let r = nb_char_pred(b, helpers.char_lower_case_p, lookup(map, src)?);
                map.insert(dst, r);
            }
            // #50 — flonum transcendentals. NB Flonum is the f64 bit
            // pattern and the helpers take/return i64-encoded f64, so
            // these are pass-through (no encode/decode, no stack-map).
            &Inst::FlonumSin(dst, s)
            | &Inst::FlonumCos(dst, s)
            | &Inst::FlonumTan(dst, s)
            | &Inst::FlonumAsin(dst, s)
            | &Inst::FlonumAcos(dst, s)
            | &Inst::FlonumAtan(dst, s)
            | &Inst::FlonumExp(dst, s)
            | &Inst::FlonumLog(dst, s) => {
                let fnref = match inst {
                    Inst::FlonumSin(..) => helpers.flonum_sin,
                    Inst::FlonumCos(..) => helpers.flonum_cos,
                    Inst::FlonumTan(..) => helpers.flonum_tan,
                    Inst::FlonumAsin(..) => helpers.flonum_asin,
                    Inst::FlonumAcos(..) => helpers.flonum_acos,
                    Inst::FlonumAtan(..) => helpers.flonum_atan,
                    Inst::FlonumExp(..) => helpers.flonum_exp,
                    Inst::FlonumLog(..) => helpers.flonum_log,
                    _ => unreachable!(),
                };
                let sv = lookup(map, s)?;
                let call = b.ins().call(fnref, &[sv]);
                let r = b.inst_results(call)[0];
                map.insert(dst, r);
            }
            // 2-arg flonum transcendentals (also pass-through).
            &Inst::FlonumExpt(dst, a, c)
            | &Inst::FlonumLog2(dst, a, c)
            | &Inst::FlonumAtan2(dst, a, c) => {
                let fnref = match inst {
                    Inst::FlonumExpt(..) => helpers.flonum_expt,
                    Inst::FlonumLog2(..) => helpers.flonum_log2,
                    Inst::FlonumAtan2(..) => helpers.flonum_atan2,
                    _ => unreachable!(),
                };
                let av = lookup(map, a)?;
                let cv = lookup(map, c)?;
                let call = b.ins().call(fnref, &[av, cv]);
                let r = b.inst_results(call)[0];
                map.insert(dst, r);
            }
            // flonum predicates via helper → NB Boolean.
            &Inst::FlonumIsInteger(dst, s) => {
                let r = nb_bool_call(b, helpers.flonum_is_integer, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::FlEvenP(dst, s) => {
                let r = nb_bool_call(b, helpers.fl_even_p, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::FlOddP(dst, s) => {
                let r = nb_bool_call(b, helpers.fl_odd_p, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            // flonum predicates inline (fcmp) → NB Boolean.
            &Inst::FlonumIsNan(dst, s) => {
                let s_i = lookup(map, s)?;
                let mf = cranelift_codegen::ir::MemFlags::new();
                let s_f = b.ins().bitcast(F64, mf, s_i);
                let cmp = b.ins().fcmp(
                    cranelift_codegen::ir::condcodes::FloatCC::Unordered,
                    s_f,
                    s_f,
                );
                let widened = b.ins().uextend(I64, cmp);
                let r = b
                    .ins()
                    .bor_imm(widened, cs_vm::vm::NanboxValue::FALSE.into_raw());
                map.insert(dst, r);
            }
            &Inst::FlonumIsInfinite(dst, s) => {
                let s_i = lookup(map, s)?;
                let mf = cranelift_codegen::ir::MemFlags::new();
                let s_f = b.ins().bitcast(F64, mf, s_i);
                let abs = b.ins().fabs(s_f);
                let inf = b.ins().f64const(f64::INFINITY);
                let cmp = b
                    .ins()
                    .fcmp(cranelift_codegen::ir::condcodes::FloatCC::Equal, abs, inf);
                let widened = b.ins().uextend(I64, cmp);
                let r = b
                    .ins()
                    .bor_imm(widened, cs_vm::vm::NanboxValue::FALSE.into_raw());
                map.insert(dst, r);
            }
            &Inst::FlonumIsFinite(dst, s) => {
                let s_i = lookup(map, s)?;
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
                let r = b
                    .ins()
                    .bor_imm(widened, cs_vm::vm::NanboxValue::FALSE.into_raw());
                map.insert(dst, r);
            }
            // #50 — misc predicates → NB Boolean.
            &Inst::SymbolP(dst, src) => {
                let r = nb_bool_call(b, helpers.symbol_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::ProcedureP(dst, src) => {
                let r = nb_bool_call(b, helpers.procedure_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::PortP(dst, src) => {
                let r = nb_bool_call(b, helpers.port_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::DottedListP(dst, src) => {
                let r = nb_bool_call(b, helpers.dotted_list_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::CircularListP(dst, src) => {
                let r = nb_bool_call(b, helpers.circular_list_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::NullListP(dst, src) => {
                let r = nb_bool_call(b, helpers.null_list_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::FileExistsP(dst, src) => {
                let r = nb_bool_call(b, helpers.file_exists_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::PortEofP(dst, src) => {
                let r = nb_bool_call(b, helpers.port_eof_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            // #50 — misc pointer/handle builtins → NB Gc handle.
            &Inst::EofObject(dst) => {
                let r = nb_ptr_call(b, helpers.eof_object, &[]);
                map.insert(dst, r);
            }
            &Inst::MakePromise(dst, src) => {
                let r = nb_ptr_call(b, helpers.make_promise, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::ForceForced(dst, src) => {
                let r = nb_ptr_call(b, helpers.force_forced, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::AlistCopy(dst, src) => {
                let r = nb_ptr_call(b, helpers.alist_copy, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::DeleteDuplicates(dst, src) => {
                let r = nb_ptr_call(b, helpers.delete_duplicates, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            // #50 — vector builtins. NB vector pointer passed through;
            // index/count args decoded NB→raw (the gc-helpers take raw
            // indices and deopt internally on out-of-range). Fill args
            // are values, passed NB.
            &Inst::VecCopy(dst, src) => {
                let r = nb_ptr_call(b, helpers.vec_copy, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::VecFill(dst, vec, fill) => {
                let r = nb_ptr_call(
                    b,
                    helpers.vec_fill,
                    &[lookup(map, vec)?, lookup(map, fill)?],
                );
                map.insert(dst, r);
            }
            &Inst::VectorToList(dst, src) => {
                let r = nb_ptr_call(b, helpers.vector_to_list, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::VectorToString(dst, src) => {
                let r = nb_ptr_call(b, helpers.vector_to_string, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::VectorEqP(dst, a, c) => {
                let r = nb_bool_call(b, helpers.vector_eq_p, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::VecCopyFrom(dst, vec, start) => {
                let vv = lookup(map, vec)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.vector_copy_from, &[vv, sv]);
                map.insert(dst, r);
            }
            &Inst::VecCopySlice(dst, vec, start, end) => {
                let vv = lookup(map, vec)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.vector_copy_slice, &[vv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::VectorToListSlice(dst, vec, start, end) => {
                let vv = lookup(map, vec)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.vector_to_list_slice, &[vv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::VectorToListSliceFrom(dst, vec, start) => {
                let vv = lookup(map, vec)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.vector_to_list_slice_from, &[vv, sv]);
                map.insert(dst, r);
            }
            &Inst::VectorToStringSlice(dst, vec, start, end) => {
                let vv = lookup(map, vec)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.vector_to_string_slice, &[vv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::VectorToStringSliceFrom(dst, vec, start) => {
                let vv = lookup(map, vec)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.vector_to_string_slice_from, &[vv, sv]);
                map.insert(dst, r);
            }
            &Inst::VecFillFrom(dst, vec, fill, start) => {
                let vv = lookup(map, vec)?;
                let fv = lookup(map, fill)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.vector_fill_from, &[vv, fv, sv]);
                map.insert(dst, r);
            }
            &Inst::VecFillSlice(dst, vec, fill, start, end) => {
                let vv = lookup(map, vec)?;
                let fv = lookup(map, fill)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.vector_fill_slice, &[vv, fv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::VecCopyBang(dst, dest, at, src) => {
                let d = lookup(map, dest)?;
                let a = unbox_nb_fixnum(b, lookup(map, at)?);
                let s = lookup(map, src)?;
                let r = nb_ptr_call(b, helpers.vector_copy_bang, &[d, a, s]);
                map.insert(dst, r);
            }
            &Inst::VecCopyBangFrom(dst, dest, at, src, start) => {
                let d = lookup(map, dest)?;
                let a = unbox_nb_fixnum(b, lookup(map, at)?);
                let s = lookup(map, src)?;
                let st = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.vector_copy_bang_from, &[d, a, s, st]);
                map.insert(dst, r);
            }
            &Inst::VecCopyBangSlice(dst, dest, at, src, start, end) => {
                let d = lookup(map, dest)?;
                let a = unbox_nb_fixnum(b, lookup(map, at)?);
                let s = lookup(map, src)?;
                let st = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.vector_copy_bang_slice, &[d, a, s, st, ev]);
                map.insert(dst, r);
            }
            // #50 — string predicate/comparison/transform/hash builtins.
            // Pointer operands pass through NB; results are NB pointer
            // (transforms), NB Boolean (predicates/compares), or NB
            // Fixnum (length/hash).
            &Inst::StrP(dst, s) => {
                let r = nb_bool_call(b, helpers.string_p, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StrLength(dst, s) => {
                let r = nb_fixnum_call(b, helpers.string_length, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StrLt(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_lt, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrGt(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_gt, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrLe(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_le, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrGe(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_ge, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrCiEq(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_ci_eq, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrCiLt(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_ci_lt, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrCiGt(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_ci_gt, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrCiLe(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_ci_le, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StrCiGe(dst, a, c) => {
                let r = nb_bool_call(b, helpers.string_ci_ge, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            &Inst::StringDowncase(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_downcase, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringUpcase(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_upcase, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringFoldcase(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_foldcase, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringTitlecase(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_titlecase, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringReverse(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_reverse, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringPrefixP(dst, a, c) => {
                let r = nb_bool_call(
                    b,
                    helpers.string_prefix_p,
                    &[lookup(map, a)?, lookup(map, c)?],
                );
                map.insert(dst, r);
            }
            &Inst::StringSuffixP(dst, a, c) => {
                let r = nb_bool_call(
                    b,
                    helpers.string_suffix_p,
                    &[lookup(map, a)?, lookup(map, c)?],
                );
                map.insert(dst, r);
            }
            // #50 — bytevector builtins. bv pointer passes NB; byte/
            // count/offset/index args decode NB→raw; IEEE f64 values are
            // NB Flonum (f64 bits) and pass through.
            &Inst::BvP(dst, src) => {
                let r = nb_bool_call(b, helpers.bv_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::BvLength(dst, src) => {
                let r = nb_fixnum_call(b, helpers.bv_length, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::BvAlloc(dst, n, fill) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let fv = unbox_nb_fixnum(b, lookup(map, fill)?);
                let r = nb_ptr_call(b, helpers.bv_alloc, &[nv, fv]);
                map.insert(dst, r);
            }
            &Inst::BvCopy(dst, src) => {
                let r = nb_ptr_call(b, helpers.bv_copy, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::BvFill(dst, bv, fill) => {
                let bvv = lookup(map, bv)?;
                let fv = unbox_nb_fixnum(b, lookup(map, fill)?);
                let r = nb_ptr_call(b, helpers.bv_fill, &[bvv, fv]);
                map.insert(dst, r);
            }
            &Inst::BytevectorEqP(dst, a, c) => {
                let r = nb_bool_call(
                    b,
                    helpers.bytevector_eq_p,
                    &[lookup(map, a)?, lookup(map, c)?],
                );
                map.insert(dst, r);
            }
            &Inst::BvCopyFrom(dst, bv, start) => {
                let bvv = lookup(map, bv)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.bytevector_copy_from, &[bvv, sv]);
                map.insert(dst, r);
            }
            &Inst::BvCopySlice(dst, bv, start, end) => {
                let bvv = lookup(map, bv)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.bytevector_copy_slice, &[bvv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::BvFillFrom(dst, bv, fill, start) => {
                let bvv = lookup(map, bv)?;
                let fv = unbox_nb_fixnum(b, lookup(map, fill)?);
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.bytevector_fill_from, &[bvv, fv, sv]);
                map.insert(dst, r);
            }
            &Inst::BvFillSlice(dst, bv, fill, start, end) => {
                let bvv = lookup(map, bv)?;
                let fv = unbox_nb_fixnum(b, lookup(map, fill)?);
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.bytevector_fill_slice, &[bvv, fv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::BytevectorToListSlice(dst, bv, start, end) => {
                let bvv = lookup(map, bv)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.bytevector_to_list_slice, &[bvv, sv, ev]);
                map.insert(dst, r);
            }
            &Inst::BytevectorToListSliceFrom(dst, bv, start) => {
                let bvv = lookup(map, bv)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.bytevector_to_list_slice_from, &[bvv, sv]);
                map.insert(dst, r);
            }
            &Inst::BvCopyBang(dst, to, at, from) => {
                let tv = lookup(map, to)?;
                let av = unbox_nb_fixnum(b, lookup(map, at)?);
                let fv = lookup(map, from)?;
                let r = nb_ptr_call(b, helpers.bytevector_copy_bang, &[tv, av, fv]);
                map.insert(dst, r);
            }
            &Inst::BvCopyBangFrom(dst, to, at, from, start) => {
                let tv = lookup(map, to)?;
                let av = unbox_nb_fixnum(b, lookup(map, at)?);
                let fv = lookup(map, from)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.bytevector_copy_bang_from, &[tv, av, fv, sv]);
                map.insert(dst, r);
            }
            &Inst::BvCopyBangSlice(dst, to, at, from, start, end) => {
                let tv = lookup(map, to)?;
                let av = unbox_nb_fixnum(b, lookup(map, at)?);
                let fv = lookup(map, from)?;
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.bytevector_copy_bang_slice, &[tv, av, fv, sv, ev]);
                map.insert(dst, r);
            }
            // IEEE native ref → NB Flonum (f64 bits, pass through); the
            // offset arg decodes NB→raw.
            &Inst::BvIeeeSingleNativeRef(dst, bv, k) => {
                let bvv = lookup(map, bv)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let call = b
                    .ins()
                    .call(helpers.bytevector_ieee_single_native_ref, &[bvv, kv]);
                let r = b.inst_results(call)[0];
                map.insert(dst, r);
            }
            &Inst::BvIeeeDoubleNativeRef(dst, bv, k) => {
                let bvv = lookup(map, bv)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let call = b
                    .ins()
                    .call(helpers.bytevector_ieee_double_native_ref, &[bvv, kv]);
                let r = b.inst_results(call)[0];
                map.insert(dst, r);
            }
            // IEEE native set: offset decodes; value is NB Flonum (f64
            // bits) passed through.
            &Inst::BvIeeeSingleNativeSet(dst, bv, k, val) => {
                let bvv = lookup(map, bv)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let vv = lookup(map, val)?;
                let r = nb_ptr_call(b, helpers.bytevector_ieee_single_native_set, &[bvv, kv, vv]);
                map.insert(dst, r);
            }
            &Inst::BvIeeeDoubleNativeSet(dst, bv, k, val) => {
                let bvv = lookup(map, bv)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let vv = lookup(map, val)?;
                let r = nb_ptr_call(b, helpers.bytevector_ieee_double_native_set, &[bvv, kv, vv]);
                map.insert(dst, r);
            }
            // #50 — hashtable builtins. HT pointer + keys/values pass NB
            // (no index decoding); results are NB handle / Boolean / Fixnum.
            &Inst::HashtableP(dst, src) => {
                let r = nb_bool_call(b, helpers.hashtable_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableMutableP(dst, src) => {
                let r = nb_bool_call(b, helpers.hashtable_mutable_p, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableContainsP(dst, ht, key) => {
                let r = nb_bool_call(
                    b,
                    helpers.hashtable_contains_p,
                    &[lookup(map, ht)?, lookup(map, key)?],
                );
                map.insert(dst, r);
            }
            &Inst::HashtableSize(dst, src) => {
                let r = nb_fixnum_call(b, helpers.hashtable_size, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableKeys(dst, src) => {
                let r = nb_ptr_call(b, helpers.hashtable_keys, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableValues(dst, src) => {
                let r = nb_ptr_call(b, helpers.hashtable_values, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableClear(dst, src) => {
                let r = nb_ptr_call(b, helpers.hashtable_clear, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableToAlist(dst, src) => {
                let r = nb_ptr_call(b, helpers.hashtable_to_alist, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableCopy(dst, src) => {
                let r = nb_ptr_call(b, helpers.hashtable_copy, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableHashFn(dst, src) => {
                let r = nb_ptr_call(b, helpers.hashtable_hash_function, &[lookup(map, src)?]);
                map.insert(dst, r);
            }
            &Inst::HashtableDelete(dst, ht, key) => {
                let r = nb_ptr_call(
                    b,
                    helpers.hashtable_delete,
                    &[lookup(map, ht)?, lookup(map, key)?],
                );
                map.insert(dst, r);
            }
            &Inst::HashtableSet(dst, ht, key, val) => {
                let r = nb_ptr_call(
                    b,
                    helpers.hashtable_set,
                    &[lookup(map, ht)?, lookup(map, key)?, lookup(map, val)?],
                );
                map.insert(dst, r);
            }
            &Inst::HashtableRef(dst, ht, key, default) => {
                let r = nb_ptr_call(
                    b,
                    helpers.hashtable_ref,
                    &[lookup(map, ht)?, lookup(map, key)?, lookup(map, default)?],
                );
                map.insert(dst, r);
            }
            &Inst::MakeHashtableEqual(dst) => {
                let r = nb_ptr_call(b, helpers.make_hashtable_equal, &[]);
                map.insert(dst, r);
            }
            &Inst::MakeHashtableEq(dst) => {
                let r = nb_ptr_call(b, helpers.make_hashtable_eq, &[]);
                map.insert(dst, r);
            }
            &Inst::MakeHashtableEqv(dst) => {
                let r = nb_ptr_call(b, helpers.make_hashtable_eqv, &[]);
                map.insert(dst, r);
            }
            // #50 — string pointer/index builtins. String/list pointers
            // pass NB; count/index args decode NB→raw; results are NB
            // string/list/vector handles.
            &Inst::StrCopy(dst, s) => {
                let r = nb_ptr_call(b, helpers.str_copy, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::Substring(dst, s, start, end) => {
                let sv = lookup(map, s)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.substring, &[sv, stv, ev]);
                map.insert(dst, r);
            }
            &Inst::StrCopyFrom(dst, s, start) => {
                let sv = lookup(map, s)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.string_copy_from, &[sv, stv]);
                map.insert(dst, r);
            }
            &Inst::StringTake(dst, s, n) => {
                let sv = lookup(map, s)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.string_take, &[sv, nv]);
                map.insert(dst, r);
            }
            &Inst::StringTakeRight(dst, s, n) => {
                let sv = lookup(map, s)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.string_take_right, &[sv, nv]);
                map.insert(dst, r);
            }
            &Inst::StringDrop(dst, s, n) => {
                let sv = lookup(map, s)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.string_drop, &[sv, nv]);
                map.insert(dst, r);
            }
            &Inst::StringDropRight(dst, s, n) => {
                let sv = lookup(map, s)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.string_drop_right, &[sv, nv]);
                map.insert(dst, r);
            }
            &Inst::StringPad(dst, s, n) => {
                let sv = lookup(map, s)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.string_pad, &[sv, nv]);
                map.insert(dst, r);
            }
            &Inst::StringPadRight(dst, s, n) => {
                let sv = lookup(map, s)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.string_pad_right, &[sv, nv]);
                map.insert(dst, r);
            }
            &Inst::StringSplit(dst, s, sep) => {
                let r = nb_ptr_call(
                    b,
                    helpers.string_split,
                    &[lookup(map, s)?, lookup(map, sep)?],
                );
                map.insert(dst, r);
            }
            &Inst::StringJoin(dst, lst, sep) => {
                let r = nb_ptr_call(
                    b,
                    helpers.string_join,
                    &[lookup(map, lst)?, lookup(map, sep)?],
                );
                map.insert(dst, r);
            }
            &Inst::StringReplaceAll(dst, s, from, to) => {
                let r = nb_ptr_call(
                    b,
                    helpers.string_replace_all,
                    &[lookup(map, s)?, lookup(map, from)?, lookup(map, to)?],
                );
                map.insert(dst, r);
            }
            &Inst::StringReplaceFirst(dst, s, from, to) => {
                let r = nb_ptr_call(
                    b,
                    helpers.string_replace_first,
                    &[lookup(map, s)?, lookup(map, from)?, lookup(map, to)?],
                );
                map.insert(dst, r);
            }
            &Inst::StringTrim(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_trim, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringTrimLeft(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_trim_left, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringTrimRight(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_trim_right, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringToList(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_to_list, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringToVector(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_to_vector, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringToListSlice(dst, s, start, end) => {
                let sv = lookup(map, s)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.string_to_list_slice, &[sv, stv, ev]);
                map.insert(dst, r);
            }
            &Inst::StringToListSliceFrom(dst, s, start) => {
                let sv = lookup(map, s)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.string_to_list_slice_from, &[sv, stv]);
                map.insert(dst, r);
            }
            &Inst::StringToVectorSlice(dst, s, start, end) => {
                let sv = lookup(map, s)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.string_to_vector_slice, &[sv, stv, ev]);
                map.insert(dst, r);
            }
            &Inst::StringToVectorSliceFrom(dst, s, start) => {
                let sv = lookup(map, s)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.string_to_vector_slice_from, &[sv, stv]);
                map.insert(dst, r);
            }
            &Inst::ListToVector(dst, lst) => {
                let r = nb_ptr_call(b, helpers.list_to_vector, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::ListToString(dst, lst) => {
                let r = nb_ptr_call(b, helpers.list_to_string, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            // #50 — string codepoint builtins. Char operands decode their
            // NB Character codepoint payload (band PAYLOAD_MASK); indices
            // decode NB→raw; StrRef's codepoint result re-tags to an NB
            // Character.
            &Inst::StrRef(dst, s, idx) => {
                let sv = lookup(map, s)?;
                let iv = unbox_nb_fixnum(b, lookup(map, idx)?);
                let call = b.ins().call(helpers.string_ref, &[sv, iv]);
                let raw = b.inst_results(call)[0];
                let payload = b.ins().band_imm(raw, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let char_tag_bits = (cs_vm::vm::NB_SIGNATURE_BITS
                    | ((cs_vm::vm::NB_TAG_CHARACTER as u64) << 47))
                    as i64;
                let r = b.ins().bor_imm(payload, char_tag_bits);
                map.insert(dst, r);
            }
            &Inst::StrSet(dst, s, k, ch) => {
                let sv = lookup(map, s)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let chv = b
                    .ins()
                    .band_imm(lookup(map, ch)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.str_set, &[sv, kv, chv]);
                map.insert(dst, r);
            }
            &Inst::StrAlloc(dst, n, fill) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let fv = b
                    .ins()
                    .band_imm(lookup(map, fill)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.alloc_string, &[nv, fv]);
                map.insert(dst, r);
            }
            &Inst::StrFill(dst, s, ch) => {
                let sv = lookup(map, s)?;
                let chv = b
                    .ins()
                    .band_imm(lookup(map, ch)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.str_fill, &[sv, chv]);
                map.insert(dst, r);
            }
            &Inst::StrFillFrom(dst, s, ch, start) => {
                let sv = lookup(map, s)?;
                let chv = b
                    .ins()
                    .band_imm(lookup(map, ch)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.string_fill_from, &[sv, chv, stv]);
                map.insert(dst, r);
            }
            &Inst::StrFillSlice(dst, s, ch, start, end) => {
                let sv = lookup(map, s)?;
                let chv = b
                    .ins()
                    .band_imm(lookup(map, ch)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.string_fill_slice, &[sv, chv, stv, ev]);
                map.insert(dst, r);
            }
            &Inst::StrCopyBang(dst, to, at, from) => {
                let tv = lookup(map, to)?;
                let av = unbox_nb_fixnum(b, lookup(map, at)?);
                let fv = lookup(map, from)?;
                let r = nb_ptr_call(b, helpers.string_copy_bang, &[tv, av, fv]);
                map.insert(dst, r);
            }
            &Inst::StrCopyBangFrom(dst, to, at, from, start) => {
                let tv = lookup(map, to)?;
                let av = unbox_nb_fixnum(b, lookup(map, at)?);
                let fv = lookup(map, from)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.string_copy_bang_from, &[tv, av, fv, stv]);
                map.insert(dst, r);
            }
            &Inst::StrCopyBangSlice(dst, to, at, from, start, end) => {
                let tv = lookup(map, to)?;
                let av = unbox_nb_fixnum(b, lookup(map, at)?);
                let fv = lookup(map, from)?;
                let stv = unbox_nb_fixnum(b, lookup(map, start)?);
                let ev = unbox_nb_fixnum(b, lookup(map, end)?);
                let r = nb_ptr_call(b, helpers.string_copy_bang_slice, &[tv, av, fv, stv, ev]);
                map.insert(dst, r);
            }
            // #50 — list-index builtins. List pointer passes NB; index/
            // count args decode NB→raw; element/result are NB.
            &Inst::ListTail(dst, lst, n) => {
                let lv = lookup(map, lst)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.list_tail, &[lv, nv]);
                map.insert(dst, r);
            }
            &Inst::ListRef(dst, lst, n) => {
                let lv = lookup(map, lst)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.list_ref, &[lv, nv]);
                map.insert(dst, r);
            }
            &Inst::ListSet(dst, lst, n, v) => {
                let lv = lookup(map, lst)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let vv = lookup(map, v)?;
                let r = nb_ptr_call(b, helpers.list_set, &[lv, nv, vv]);
                map.insert(dst, r);
            }
            &Inst::Take(dst, lst, n) => {
                let lv = lookup(map, lst)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.take, &[lv, nv]);
                map.insert(dst, r);
            }
            &Inst::Drop(dst, lst, n) => {
                let lv = lookup(map, lst)?;
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.drop, &[lv, nv]);
                map.insert(dst, r);
            }
            &Inst::MakeList(dst, n, fill) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let fv = lookup(map, fill)?;
                let r = nb_ptr_call(b, helpers.make_list, &[nv, fv]);
                map.insert(dst, r);
            }
            &Inst::MakeListUnspec(dst, n) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.make_list_unspec, &[nv]);
                map.insert(dst, r);
            }
            &Inst::MakeVectorUnspec(dst, n) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.make_vector_unspec, &[nv]);
                map.insert(dst, r);
            }
            // #50 — fixnum bit ops. Operand decodes NB→raw; result is a
            // small Fixnum (count/length/position).
            &Inst::BitwiseBitCount(dst, x) => {
                let xv = unbox_nb_fixnum(b, lookup(map, x)?);
                let r = nb_fixnum_call(b, helpers.bitwise_bit_count, &[xv]);
                map.insert(dst, r);
            }
            &Inst::BitwiseLength(dst, x) => {
                let xv = unbox_nb_fixnum(b, lookup(map, x)?);
                let r = nb_fixnum_call(b, helpers.bitwise_length, &[xv]);
                map.insert(dst, r);
            }
            &Inst::FxFirstBitSet(dst, x) => {
                let xv = unbox_nb_fixnum(b, lookup(map, x)?);
                let r = nb_fixnum_call(b, helpers.fx_first_bit_set, &[xv]);
                map.insert(dst, r);
            }
            &Inst::BitwiseBitSetP(dst, n, i) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let iv = unbox_nb_fixnum(b, lookup(map, i)?);
                let r = nb_bool_call(b, helpers.bitwise_bit_set_p, &[nv, iv]);
                map.insert(dst, r);
            }
            // #50 — predicates over Any operands (passed NB).
            &Inst::ExactNonNegIntP(dst, x) => {
                let r = nb_bool_call(b, helpers.exact_non_neg_int_p, &[lookup(map, x)?]);
                map.insert(dst, r);
            }
            &Inst::EqualAny(dst, a, c) => {
                let r = nb_bool_call(b, helpers.equal_any, &[lookup(map, a)?, lookup(map, c)?]);
                map.insert(dst, r);
            }
            // #50 — fixnum `_fx` arithmetic. Operands decode NB→raw; the
            // raw result is fit-checked to 47 bits (deopt → bytecode
            // bignum/rational otherwise). DivEuclid/ModEuclid deopt
            // internally on a zero divisor.
            &Inst::Expt(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.expt, &[av, cv]);
                map.insert(dst, r);
            }
            &Inst::Gcd(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.gcd, &[av, cv]);
                map.insert(dst, r);
            }
            &Inst::Lcm(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.lcm, &[av, cv]);
                map.insert(dst, r);
            }
            &Inst::ArithShift(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r =
                    nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.arith_shift, &[av, cv]);
                map.insert(dst, r);
            }
            &Inst::BitwiseArithShiftLeft(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(
                    b,
                    helpers.request_deopt,
                    helpers.bitwise_arith_shift_left,
                    &[av, cv],
                );
                map.insert(dst, r);
            }
            &Inst::BitwiseArithShiftRight(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(
                    b,
                    helpers.request_deopt,
                    helpers.bitwise_arith_shift_right,
                    &[av, cv],
                );
                map.insert(dst, r);
            }
            &Inst::DivEuclid(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r =
                    nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.div_euclid, &[av, cv]);
                map.insert(dst, r);
            }
            &Inst::ModEuclid(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r =
                    nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.mod_euclid, &[av, cv]);
                map.insert(dst, r);
            }
            // #50 — number/string conversions + substring search. All
            // operands and results ride the NB lane (number, string, and
            // number-or-#f / index-or-#f results need no encode/decode).
            &Inst::NumberToString(dst, x) => {
                let r = nb_ptr_call(b, helpers.number_to_string, &[lookup(map, x)?]);
                map.insert(dst, r);
            }
            &Inst::StringToNumber(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_to_number, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::StringContains(dst, h, n) => {
                let r = nb_ptr_call(
                    b,
                    helpers.string_contains,
                    &[lookup(map, h)?, lookup(map, n)?],
                );
                map.insert(dst, r);
            }
            &Inst::StringContainsRight(dst, h, n) => {
                let r = nb_ptr_call(
                    b,
                    helpers.string_contains_right,
                    &[lookup(map, h)?, lookup(map, n)?],
                );
                map.insert(dst, r);
            }
            // #50 — radix conversions: NB number/string operand, raw
            // radix arg, NB result.
            &Inst::NumberToStringRadix(dst, n, radix) => {
                let nv = lookup(map, n)?;
                let rv = unbox_nb_fixnum(b, lookup(map, radix)?);
                let r = nb_ptr_call(b, helpers.number_to_string_radix, &[nv, rv]);
                map.insert(dst, r);
            }
            &Inst::StringToNumberRadix(dst, s, radix) => {
                let sv = lookup(map, s)?;
                let rv = unbox_nb_fixnum(b, lookup(map, radix)?);
                let r = nb_ptr_call(b, helpers.string_to_number_radix, &[sv, rv]);
                map.insert(dst, r);
            }
            // #50 — iota: all count/start/step args are raw Fixnums.
            &Inst::IotaN(dst, n) => {
                let nv = unbox_nb_fixnum(b, lookup(map, n)?);
                let r = nb_ptr_call(b, helpers.iota_n, &[nv]);
                map.insert(dst, r);
            }
            &Inst::IotaNs(dst, count, start) => {
                let cv = unbox_nb_fixnum(b, lookup(map, count)?);
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let r = nb_ptr_call(b, helpers.iota_ns, &[cv, sv]);
                map.insert(dst, r);
            }
            &Inst::IotaNss(dst, count, start, step) => {
                let cv = unbox_nb_fixnum(b, lookup(map, count)?);
                let sv = unbox_nb_fixnum(b, lookup(map, start)?);
                let stv = unbox_nb_fixnum(b, lookup(map, step)?);
                let r = nb_ptr_call(b, helpers.iota_nss, &[cv, sv, stv]);
                map.insert(dst, r);
            }
            // #50 — digit-value / string-index: codepoint arg decodes the
            // NB Character payload; result is an NB Fixnum-or-#f.
            &Inst::DigitValue(dst, c) => {
                let cv = b
                    .ins()
                    .band_imm(lookup(map, c)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.digit_value, &[cv]);
                map.insert(dst, r);
            }
            &Inst::StringIndex(dst, s, c) => {
                let sv = lookup(map, s)?;
                let cv = b
                    .ins()
                    .band_imm(lookup(map, c)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.string_index, &[sv, cv]);
                map.insert(dst, r);
            }
            &Inst::StringIndexRight(dst, s, c) => {
                let sv = lookup(map, s)?;
                let cv = b
                    .ins()
                    .band_imm(lookup(map, c)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.string_index_right, &[sv, cv]);
                map.insert(dst, r);
            }
            // #50 — variadic buffer builders. NB args go through a stack
            // buffer; result is an NB string/vector/bytevector handle.
            Inst::StrAppend(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.string_append_buf, &arg_vs);
                map.insert(*dst, r);
            }
            Inst::VecAppend(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.vector_append_buf, &arg_vs);
                map.insert(*dst, r);
            }
            Inst::VecBuild(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.make_vector_buf, &arg_vs);
                map.insert(*dst, r);
            }
            Inst::BvBuild(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.make_bytevector_buf, &arg_vs);
                map.insert(*dst, r);
            }
            Inst::BvAppend(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.bytevector_append_buf, &arg_vs);
                map.insert(*dst, r);
            }
            // #50 — symbol<->string. The helpers use RAW symbol ids:
            // symbol->string decodes the NB Symbol's id payload in;
            // string->symbol returns a raw id that re-tags to an NB
            // Symbol (without this, string->symbol left the value looking
            // like a plain number and symbol->string then failed).
            &Inst::SymbolToString(dst, sym) => {
                let id = b
                    .ins()
                    .band_imm(lookup(map, sym)?, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let r = nb_ptr_call(b, helpers.symbol_to_string, &[id]);
                map.insert(dst, r);
            }
            &Inst::StringToSymbol(dst, s) => {
                let call = b.ins().call(helpers.string_to_symbol, &[lookup(map, s)?]);
                let raw_id = b.inst_results(call)[0];
                let payload = b.ins().band_imm(raw_id, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let sym_tag_bits = (cs_vm::vm::NB_SIGNATURE_BITS
                    | ((cs_vm::vm::NB_TAG_SYMBOL as u64) << 47))
                    as i64;
                let r = b.ins().bor_imm(payload, sym_tag_bits);
                map.insert(dst, r);
            }
            // #50 — hash builtins. The helper returns a full-range i64
            // hash; encode via vm_fixnum_to_nb (bignum if >47-bit) rather
            // than the 47-bit-truncating box_raw_fixnum.
            &Inst::StringHash(dst, s) => {
                let call = b.ins().call(helpers.string_hash, &[lookup(map, s)?]);
                let raw = b.inst_results(call)[0];
                let enc = b.ins().call(helpers.fixnum_to_nb, &[raw]);
                let r = b.inst_results(enc)[0];
                map.insert(dst, r);
            }
            &Inst::SymbolHash(dst, s) => {
                let call = b.ins().call(helpers.symbol_hash, &[lookup(map, s)?]);
                let raw = b.inst_results(call)[0];
                let enc = b.ins().call(helpers.fixnum_to_nb, &[raw]);
                let r = b.inst_results(enc)[0];
                map.insert(dst, r);
            }
            &Inst::EqualHash(dst, x) => {
                let call = b.ins().call(helpers.equal_hash, &[lookup(map, x)?]);
                let raw = b.inst_results(call)[0];
                let enc = b.ins().call(helpers.fixnum_to_nb, &[raw]);
                let r = b.inst_results(enc)[0];
                map.insert(dst, r);
            }
            // #50 — pair mutate / list / conversion / port / time.
            &Inst::SetCar(dst, p, v) => {
                let r = nb_ptr_call(b, helpers.set_car, &[lookup(map, p)?, lookup(map, v)?]);
                map.insert(dst, r);
            }
            &Inst::SetCdr(dst, p, v) => {
                let r = nb_ptr_call(b, helpers.set_cdr, &[lookup(map, p)?, lookup(map, v)?]);
                map.insert(dst, r);
            }
            &Inst::Length(dst, lst) => {
                let r = nb_fixnum_call(b, helpers.length, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::Last(dst, lst) => {
                let r = nb_ptr_call(b, helpers.last, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            Inst::ListAppend(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.append_buf, &arg_vs);
                map.insert(*dst, r);
            }
            &Inst::BytevectorToU8List(dst, bv) => {
                let r = nb_ptr_call(b, helpers.bytevector_to_u8_list, &[lookup(map, bv)?]);
                map.insert(dst, r);
            }
            &Inst::U8ListToBytevector(dst, lst) => {
                let r = nb_ptr_call(b, helpers.u8_list_to_bytevector, &[lookup(map, lst)?]);
                map.insert(dst, r);
            }
            &Inst::StringToUtf8(dst, s) => {
                let r = nb_ptr_call(b, helpers.string_to_utf8, &[lookup(map, s)?]);
                map.insert(dst, r);
            }
            &Inst::Utf8ToString(dst, bv) => {
                let r = nb_ptr_call(b, helpers.utf8_to_string, &[lookup(map, bv)?]);
                map.insert(dst, r);
            }
            &Inst::PortPosition(dst, port) => {
                let r = nb_fixnum_call(b, helpers.port_position, &[lookup(map, port)?]);
                map.insert(dst, r);
            }
            &Inst::PortHasSetPortPositionP(dst, port) => {
                let r = nb_bool_call(
                    b,
                    helpers.port_has_set_port_position_p,
                    &[lookup(map, port)?],
                );
                map.insert(dst, r);
            }
            // current-jiffy returns a raw nanosecond count (>47-bit) →
            // encode via fixnum_to_nb; current-second returns f64 bits
            // (NB Flonum) → pass through.
            &Inst::CurrentJiffy(dst) => {
                let call = b.ins().call(helpers.current_jiffy, &[]);
                let raw = b.inst_results(call)[0];
                let enc = b.ins().call(helpers.fixnum_to_nb, &[raw]);
                let r = b.inst_results(enc)[0];
                map.insert(dst, r);
            }
            &Inst::CurrentSecond(dst) => {
                let call = b.ins().call(helpers.current_second, &[]);
                let r = b.inst_results(call)[0];
                map.insert(dst, r);
            }
            // #50 — bytevector typed reads. bv pointer NB, offset decodes
            // NB→raw; the raw int result is encoded fixnum-or-bignum.
            &Inst::BvU8Ref(dst, bv, k)
            | &Inst::BvS8Ref(dst, bv, k)
            | &Inst::BvU16NativeRef(dst, bv, k)
            | &Inst::BvS16NativeRef(dst, bv, k)
            | &Inst::BvU32NativeRef(dst, bv, k)
            | &Inst::BvS32NativeRef(dst, bv, k)
            | &Inst::BvU64NativeRef(dst, bv, k)
            | &Inst::BvS64NativeRef(dst, bv, k) => {
                let fnref = match inst {
                    Inst::BvU8Ref(..) => helpers.bv_u8_ref,
                    Inst::BvS8Ref(..) => helpers.bv_s8_ref,
                    Inst::BvU16NativeRef(..) => helpers.bv_u16_ref,
                    Inst::BvS16NativeRef(..) => helpers.bv_s16_ref,
                    Inst::BvU32NativeRef(..) => helpers.bv_u32_ref,
                    Inst::BvS32NativeRef(..) => helpers.bv_s32_ref,
                    Inst::BvU64NativeRef(..) => helpers.bv_u64_ref,
                    Inst::BvS64NativeRef(..) => helpers.bv_s64_ref,
                    _ => unreachable!(),
                };
                let bvv = lookup(map, bv)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let r = nb_call_fx_nb(b, fnref, helpers.fixnum_to_nb, &[bvv, kv]);
                map.insert(dst, r);
            }
            // #50 — bytevector typed writes. offset + int value decode.
            &Inst::BvU8Set(dst, bv, k, v)
            | &Inst::BvS8Set(dst, bv, k, v)
            | &Inst::BvU16NativeSet(dst, bv, k, v)
            | &Inst::BvS16NativeSet(dst, bv, k, v)
            | &Inst::BvU32NativeSet(dst, bv, k, v)
            | &Inst::BvS32NativeSet(dst, bv, k, v)
            | &Inst::BvU64NativeSet(dst, bv, k, v)
            | &Inst::BvS64NativeSet(dst, bv, k, v) => {
                let fnref = match inst {
                    Inst::BvU8Set(..) => helpers.bv_u8_set,
                    Inst::BvS8Set(..) => helpers.bv_s8_set,
                    Inst::BvU16NativeSet(..) => helpers.bv_u16_set,
                    Inst::BvS16NativeSet(..) => helpers.bv_s16_set,
                    Inst::BvU32NativeSet(..) => helpers.bv_u32_set,
                    Inst::BvS32NativeSet(..) => helpers.bv_s32_set,
                    Inst::BvU64NativeSet(..) => helpers.bv_u64_set,
                    Inst::BvS64NativeSet(..) => helpers.bv_s64_set,
                    _ => unreachable!(),
                };
                let bvv = lookup(map, bv)?;
                let kv = unbox_nb_fixnum(b, lookup(map, k)?);
                let vv = unbox_nb_fixnum(b, lookup(map, v)?);
                let r = nb_ptr_call(b, fnref, &[bvv, kv, vv]);
                map.insert(dst, r);
            }
            // #50 — div0/mod0 (R6RS rounding division), fx arith.
            &Inst::Div0(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.div0, &[av, cv]);
                map.insert(dst, r);
            }
            &Inst::Mod0(dst, a, c) => {
                let av = unbox_nb_fixnum(b, lookup(map, a)?);
                let cv = unbox_nb_fixnum(b, lookup(map, c)?);
                let r = nb_fx_call_or_deopt(b, helpers.request_deopt, helpers.mod0, &[av, cv]);
                map.insert(dst, r);
            }
            // #50 — string (build) from chars: variadic buffer.
            Inst::StrBuild(dst, args) => {
                let arg_vs: Vec<cranelift_codegen::ir::Value> = args
                    .iter()
                    .map(|a| lookup(map, *a))
                    .collect::<Result<_, _>>()?;
                let r = nb_buf_call(b, helpers.make_string_buf, &arg_vs);
                map.insert(*dst, r);
            }
            // CharToInt (char->integer): the inverse — keep the codepoint
            // payload, retag as Fixnum (signature bits, tag 0). Without
            // this dedicated direction `char->integer` left the value
            // Character-tagged (it used to lower to Move), so
            // `(char->integer (integer->char x))` returned a Character
            // instead of a Number on uniform-NB.
            &Inst::CharToInt(dst, src) => {
                let v = lookup(map, src)?;
                let payload = b.ins().band_imm(v, cs_vm::vm::NB_PAYLOAD_MASK as i64);
                let fixnum = b
                    .ins()
                    .bor_imm(payload, cs_vm::vm::NB_SIGNATURE_BITS as i64);
                map.insert(dst, fixnum);
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

/// #50 — is `v` an NB Fixnum carrier? Returns an i8 bool. Mirrors the
/// (signature|tag)==Fixnum-pattern check in `emit_nb_arith_fixnum_fast`.
fn emit_is_fixnum_nb(
    b: &mut FunctionBuilder,
    v: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    use cs_vm::vm::{NB_SIGNATURE_BITS, NB_SIGNATURE_MASK, NB_TAG_MASK};
    let combined_mask = (NB_SIGNATURE_MASK | NB_TAG_MASK) as i64;
    let masked = b.ins().band_imm(v, combined_mask);
    b.ins()
        .icmp_imm(IntCC::Equal, masked, NB_SIGNATURE_BITS as i64)
}

/// #50 — request a deopt with `reason`. The dispatcher discards the JIT
/// body's result and reruns on bytecode (which handles bignum operands,
/// raises div-by-zero, etc.).
fn emit_request_deopt(
    b: &mut FunctionBuilder,
    request_deopt: cranelift_codegen::ir::FuncRef,
    reason: u8,
) {
    let r = b.ins().iconst(I64, reason as i64);
    b.ins().call(request_deopt, &[r]);
}

/// #50 — call a `vm_*_gc` helper that returns an NB Gc handle (pair,
/// list, etc.), declaring the result for GC stack-map tracking. The
/// shared shape for the typed pointer-builtins ported to uniform-NB.
fn nb_ptr_call(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    args: &[cranelift_codegen::ir::Value],
) -> cranelift_codegen::ir::Value {
    let call = b.ins().call(fnref, args);
    let r = b.inst_results(call)[0];
    b.declare_value_needs_stack_map(r);
    r
}

/// #50 — call a variadic `*_buf` builtin (string/vector/bytevector
/// append/build). Stack-allocates an i64 buffer, stores the NB args,
/// and calls `fnref(buf_ptr, n_args)`; the result is an NB handle marked
/// for stack-map tracking. Mirrors the specialized tier's buffer shape.
fn nb_buf_call(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    arg_vs: &[cranelift_codegen::ir::Value],
) -> cranelift_codegen::ir::Value {
    let n = arg_vs.len();
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
    let call = b.ins().call(fnref, &[buf_addr, n_v]);
    let r = b.inst_results(call)[0];
    b.declare_value_needs_stack_map(r);
    r
}

/// #50 — call a `vm_*_gc` predicate helper that returns a raw 0/1 and
/// re-encode it as an NB Boolean (same pattern as PairP/NullP/VecP).
fn nb_bool_call(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    args: &[cranelift_codegen::ir::Value],
) -> cranelift_codegen::ir::Value {
    let call = b.ins().call(fnref, args);
    let raw = b.inst_results(call)[0];
    b.ins()
        .bor_imm(raw, cs_vm::vm::NanboxValue::FALSE.into_raw())
}

/// #50 — char transform (upcase/downcase/…): decode the NB Character's
/// codepoint payload, call the raw-codepoint helper, re-tag the result
/// as an NB Character.
fn nb_char_op(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    nb_char: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let cp = b.ins().band_imm(nb_char, cs_vm::vm::NB_PAYLOAD_MASK as i64);
    let call = b.ins().call(fnref, &[cp]);
    let raw = b.inst_results(call)[0];
    let payload = b.ins().band_imm(raw, cs_vm::vm::NB_PAYLOAD_MASK as i64);
    let char_tag_bits =
        (cs_vm::vm::NB_SIGNATURE_BITS | ((cs_vm::vm::NB_TAG_CHARACTER as u64) << 47)) as i64;
    b.ins().bor_imm(payload, char_tag_bits)
}

/// #50 — char predicate (alphabetic?/numeric?/…): decode the NB
/// Character's codepoint, call the raw-codepoint helper (returns 0/1),
/// re-encode as an NB Boolean.
fn nb_char_pred(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    nb_char: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let cp = b.ins().band_imm(nb_char, cs_vm::vm::NB_PAYLOAD_MASK as i64);
    let call = b.ins().call(fnref, &[cp]);
    let raw = b.inst_results(call)[0];
    b.ins()
        .bor_imm(raw, cs_vm::vm::NanboxValue::FALSE.into_raw())
}

/// #50 — call an `_fx` helper (gcd/lcm/expt/arith-shift/div-euclid/…)
/// that takes RAW Fixnum operands and returns a RAW i64 which may exceed
/// the 47-bit NB Fixnum range. Check the fit and box, else request a
/// FIXNUM_OVERFLOW deopt → bytecode recomputes (bignum/rational). The
/// helper also deopts internally on its own overflow / zero-divisor /
/// non-fixnum cases, returning a benign 0 that the dispatcher discards.
fn nb_fx_call_or_deopt(
    b: &mut FunctionBuilder,
    request_deopt: cranelift_codegen::ir::FuncRef,
    fnref: cranelift_codegen::ir::FuncRef,
    args: &[cranelift_codegen::ir::Value],
) -> cranelift_codegen::ir::Value {
    let call = b.ins().call(fnref, args);
    let raw = b.inst_results(call)[0];
    let shl = b.ins().ishl_imm(raw, 17);
    let ext = b.ins().sshr_imm(shl, 17);
    let fits = b.ins().icmp(IntCC::Equal, raw, ext);
    let ok = b.create_block();
    let ov = b.create_block();
    let join = b.create_block();
    b.append_block_param(join, I64);
    b.ins().brif(fits, ok, &[], ov, &[]);
    b.switch_to_block(ov);
    b.seal_block(ov);
    emit_request_deopt(b, request_deopt, cs_vm::vm::DEOPT_REASON_FIXNUM_OVERFLOW);
    let ph = box_raw_fixnum(b, ext);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(ph)]);
    b.switch_to_block(ok);
    b.seal_block(ok);
    let nb = box_raw_fixnum(b, raw);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(nb)]);
    b.switch_to_block(join);
    b.seal_block(join);
    b.block_params(join)[0]
}

/// #50 — call a helper that returns a raw fixnum-range i64 (length,
/// hash, …) and re-encode it as an NB Fixnum (same as VecLength).
fn nb_fixnum_call(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    args: &[cranelift_codegen::ir::Value],
) -> cranelift_codegen::ir::Value {
    let call = b.ins().call(fnref, args);
    let raw = b.inst_results(call)[0];
    box_raw_fixnum(b, raw)
}

/// #50 — call a helper returning a raw i64 that may exceed 47 bits
/// (bytevector u64/s64 native ref, …) and encode it via `fixnum_to_nb`
/// (bignum if needed). Safe for the smaller-width reads too.
fn nb_call_fx_nb(
    b: &mut FunctionBuilder,
    fnref: cranelift_codegen::ir::FuncRef,
    fixnum_to_nb: cranelift_codegen::ir::FuncRef,
    args: &[cranelift_codegen::ir::Value],
) -> cranelift_codegen::ir::Value {
    let call = b.ins().call(fnref, args);
    let raw = b.inst_results(call)[0];
    let enc = b.ins().call(fixnum_to_nb, &[raw]);
    b.inst_results(enc)[0]
}

/// #50 — trap-free Fixnum binop (bitwise-and/or/xor, min, max) on NB
/// operands. Fast path (both Fixnum NB): unbox to sign-extended raw,
/// apply `raw_op` (whose result is in 47-bit range for these ops), and
/// re-encode. Slow path (any non-Fixnum operand, e.g. a bignum):
/// request a `FIXNUM_MISS` deopt and return a benign placeholder
/// (discarded). Makes uniform-NB cover these without the pure-fixnum
/// tier, with no new runtime helper.
fn emit_nb_fixnum_binop_or_deopt(
    b: &mut FunctionBuilder,
    request_deopt: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    bv: cranelift_codegen::ir::Value,
    raw_op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let a_fix = emit_is_fixnum_nb(b, a);
    let b_fix = emit_is_fixnum_nb(b, bv);
    let both = b.ins().band(a_fix, b_fix);
    let fast = b.create_block();
    let slow = b.create_block();
    let join = b.create_block();
    b.append_block_param(join, I64);
    b.ins().brif(both, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    b.seal_block(fast);
    let ar = unbox_nb_fixnum(b, a);
    let br = unbox_nb_fixnum(b, bv);
    let rr = raw_op(b, ar, br);
    let nb = box_raw_fixnum(b, rr);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(nb)]);

    b.switch_to_block(slow);
    b.seal_block(slow);
    emit_request_deopt(b, request_deopt, cs_vm::vm::DEOPT_REASON_FIXNUM_MISS);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(a)]);

    b.switch_to_block(join);
    b.seal_block(join);
    b.block_params(join)[0]
}

/// #50 — Fixnum unary op (bitwise-not, abs) on an NB operand. Fast path
/// (Fixnum NB): unbox, apply `raw_op`. When `check_overflow` (abs of
/// `-2^46` is `2^46`, out of 47-bit range), verify the result fits and
/// request a `FIXNUM_OVERFLOW` deopt otherwise. Slow path (non-Fixnum):
/// `FIXNUM_MISS` deopt. Returns the (possibly placeholder) NB result.
fn emit_nb_fixnum_unop_or_deopt(
    b: &mut FunctionBuilder,
    request_deopt: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    check_overflow: bool,
    raw_op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let a_fix = emit_is_fixnum_nb(b, a);
    let fast = b.create_block();
    let slow = b.create_block();
    let join = b.create_block();
    b.append_block_param(join, I64);
    b.ins().brif(a_fix, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    b.seal_block(fast);
    let ar = unbox_nb_fixnum(b, a);
    let rr = raw_op(b, ar);
    if check_overflow {
        let shl = b.ins().ishl_imm(rr, 17);
        let ext = b.ins().sshr_imm(shl, 17);
        let fits = b.ins().icmp(IntCC::Equal, rr, ext);
        let ov = b.create_block();
        let ok = b.create_block();
        b.ins().brif(fits, ok, &[], ov, &[]);
        b.switch_to_block(ov);
        b.seal_block(ov);
        emit_request_deopt(b, request_deopt, cs_vm::vm::DEOPT_REASON_FIXNUM_OVERFLOW);
        let ph = box_raw_fixnum(b, ext);
        b.ins()
            .jump(join, &[cranelift_codegen::ir::BlockArg::Value(ph)]);
        b.switch_to_block(ok);
        b.seal_block(ok);
        let nb = box_raw_fixnum(b, rr);
        b.ins()
            .jump(join, &[cranelift_codegen::ir::BlockArg::Value(nb)]);
    } else {
        let nb = box_raw_fixnum(b, rr);
        b.ins()
            .jump(join, &[cranelift_codegen::ir::BlockArg::Value(nb)]);
    }

    b.switch_to_block(slow);
    b.seal_block(slow);
    emit_request_deopt(b, request_deopt, cs_vm::vm::DEOPT_REASON_FIXNUM_MISS);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(a)]);

    b.switch_to_block(join);
    b.seal_block(join);
    b.block_params(join)[0]
}

/// #50 — Fixnum integer division family (quotient, remainder, modulo,
/// floor-quotient) on NB operands. The fast path requires both operands
/// Fixnum AND the divisor non-zero (`sdiv`/`srem` *trap* on a zero
/// divisor, so the guard must precede the op) AND the result in 47-bit
/// range (`(-2^46) quotient -1 == 2^46` overflows). `raw_op` receives
/// sign-extended raw operands (divisor guaranteed non-zero) and returns
/// the raw result. Any failed guard requests a deopt → bytecode
/// computes the bignum / raises the div-by-zero condition.
fn emit_nb_fixnum_div_or_deopt(
    b: &mut FunctionBuilder,
    request_deopt: cranelift_codegen::ir::FuncRef,
    a: cranelift_codegen::ir::Value,
    bv: cranelift_codegen::ir::Value,
    raw_op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let a_fix = emit_is_fixnum_nb(b, a);
    let b_fix = emit_is_fixnum_nb(b, bv);
    let both = b.ins().band(a_fix, b_fix);
    // NB Fixnum 0 is exactly NB_SIGNATURE_BITS (payload 0, tag 0).
    let b_nonzero = b
        .ins()
        .icmp_imm(IntCC::NotEqual, bv, cs_vm::vm::NB_SIGNATURE_BITS as i64);
    let cond = b.ins().band(both, b_nonzero);
    let fast = b.create_block();
    let slow = b.create_block();
    let join = b.create_block();
    b.append_block_param(join, I64);
    b.ins().brif(cond, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    b.seal_block(fast);
    let ar = unbox_nb_fixnum(b, a);
    let br = unbox_nb_fixnum(b, bv);
    let rr = raw_op(b, ar, br);
    // 47-bit fit check (quotient overflow edge).
    let shl = b.ins().ishl_imm(rr, 17);
    let ext = b.ins().sshr_imm(shl, 17);
    let fits = b.ins().icmp(IntCC::Equal, rr, ext);
    let ov = b.create_block();
    let ok = b.create_block();
    b.ins().brif(fits, ok, &[], ov, &[]);
    b.switch_to_block(ov);
    b.seal_block(ov);
    emit_request_deopt(b, request_deopt, cs_vm::vm::DEOPT_REASON_FIXNUM_OVERFLOW);
    let ph = box_raw_fixnum(b, ext);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(ph)]);
    b.switch_to_block(ok);
    b.seal_block(ok);
    let nb = box_raw_fixnum(b, rr);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(nb)]);

    b.switch_to_block(slow);
    b.seal_block(slow);
    emit_request_deopt(b, request_deopt, cs_vm::vm::DEOPT_REASON_ARITH_MISS);
    b.ins()
        .jump(join, &[cranelift_codegen::ir::BlockArg::Value(a)]);

    b.switch_to_block(join);
    b.seal_block(join);
    b.block_params(join)[0]
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
        match lowerer.compile_uniform_nb(&f) {
            Err(JitError::Codegen(msg)) => assert!(msg.contains("no blocks")),
            other => panic!("expected Codegen error, got {:?}", other),
        }
    }
}
