//! Ahead-of-time compiler for CrabScheme.
//!
//! Consumes a `cs-rir::Function` and emits Rust source. The Rust
//! source compiles to native code via the standard cargo toolchain;
//! the same `cs-rir` IR feeds both the JIT (Cranelift → native
//! bytes) and the AOT (cs-aot → Rust source → rustc → native bytes).
//!
//! ## Status: M10 Track A iter 3 (whole-program glue)
//!
//! - **Multi-block**: functions with branching / loops now compile.
//!   The emitter falls into one of two shapes:
//!   - **Straight-line shape** (single block, `Return` terminator):
//!     emits `let v_N = ...;` lines and a tail `v_R`. Preserves the
//!     iter-1 output exactly for the simple case.
//!   - **Loop+match shape** (anything else): pre-declares every
//!     non-param SSA value as `let mut v_N: i64 = 0;` at function
//!     top, then dispatches via a `loop { match block { ... } }`
//!     state machine. `Jump(target, args)` assigns block params and
//!     `continue`s; `Branch(cond, t, e)` reads `cond != 0` and
//!     dispatches; `Return(v)` exits the loop via `return v_N;`.
//! - **Supported Inst variants**:
//!   - `LoadConst(Fixnum)` (iter 1) + `LoadConst(Boolean/Char/Null/
//!     Unspecified/Eof/Flonum)` in Nb mode (iter 2b)
//!   - `Add`, `Sub`, `Mul` (iter 1)
//!   - `Lt`, `Eq` — comparisons return 0/1 as i64 in RawI64, or an
//!     NB Boolean in Nb mode (iter 2a/2b).
//!   - `Move` — SSA alias copy (iter 2a).
//!   - `CallSelf` — recursive call to the function being emitted;
//!     compiles to a direct Rust call (iter 3). Enables AOT of
//!     fact, fib, and other self-recursive numeric kernels.
//! - **Two ABI modes** via [`EmitMode`]:
//!   - [`EmitMode::RawI64`] (iter-1/2a default) — each SSA value is
//!     a raw `i64`; arithmetic is `wrapping_*`; comparisons return
//!     0/1. Self-contained: no runtime dependency. Use this for
//!     functions that operate on pre-decoded fixnums (e.g. tight
//!     numeric kernels called from a Rust embedder that owns the
//!     NB encoding boundary).
//!   - [`EmitMode::Nb`] (iter 2b, new) — each SSA value is an i64
//!     carrying a [`NanboxValue`] bit pattern. Constants emit as
//!     NB-encoded literals computed at emit time. Arithmetic and
//!     comparisons call the runtime's `vm_value_*_nb` helpers
//!     (which handle Fixnum + Flonum + Rational uniformly).
//!     Matches the JIT's outer-trampoline ABI so AOT'd functions
//!     are call-compatible with the runtime's dispatch path.
//!     Requires `cs-vm` to be in scope at compile time.
//! - The emitter does not invoke `rustc`; it returns the source as a
//!   `String`. Callers feed that to their build system.
//!
//! ## Invariants & contracts
//!
//! - RawI64 output is `#![deny(unsafe_code)]` clean. Nb output uses
//!   `unsafe { cs_vm::vm::vm_value_*_nb(...) }` blocks — they call
//!   `extern "C"` runtime helpers, the same shape the JIT emits as
//!   direct calls. Callers that need unsafe-free output should pick
//!   RawI64; callers that need NB-compat interop accept the unsafe.
//! - Output parses as a valid `syn::File`; the AOT tests assert this
//!   for every emitted snippet, separately from semantic tests that
//!   actually `rustc` the source.
//! - Function naming: `pub extern "C" fn {sanitized(name)}(arg0: i64,
//!   ...) -> i64`. The `extern "C"` matches the JIT's outer-trampoline
//!   signature so the runtime's dispatch path can call AOT'd
//!   procedures the same way it calls JIT'd ones.

#![deny(unsafe_code)]

use std::collections::HashSet;
use std::fmt::Write;

use cs_rir::{BlockId, Const, Function, Inst, Term, Type, Value};

pub mod project;

/// Re-export of [`sanitize_ident`] for use by the project emitter.
/// The two callers need byte-for-byte identical name mangling (the
/// emitted function definition must match the call site in `main()`),
/// so the helper is shared rather than duplicated.
pub(crate) fn sanitize_ident_for_project(name: &str) -> String {
    sanitize_ident(name)
}

/// Rust source for the NB-mode inline fast-path helpers.
///
/// Emitted-source contract (RC2 iter A): in [`EmitMode::Nb`] every
/// `Inst::{Add,Sub,Mul,Lt,Eq}` lowers to a call like
/// `nb_add_inline(va, vb)`. The helper does the JIT's inline NB
/// Fixnum fast path open-coded — tag check → sign-extend → checked
/// arith → range check → re-encode — and only falls back to the
/// `vm_value_*_nb` runtime helper on a tag miss or overflow.
/// `#[inline(always)]` lets `rustc -O` constant-fold the tag check
/// away when type-feedback eventually proves both operands are
/// always Fixnum (the AOT analog of the JIT's type guards).
///
/// Callers must inject this source ONCE into the same translation
/// unit as the emitted functions:
/// - Single-function callers (test harnesses) prepend it before
///   the [`emit_with`] output in their main shim.
/// - [`project::emit_project`] prepends it inside `src/main.rs`
///   before any function bodies.
///
/// Mirrors the bit layout in `cs_vm::vm`: NB_SIGNATURE_BITS =
/// 0xFFF8_0000_0000_0000, tag occupies bits 47-50 (FIXNUM tag is
/// 0), payload is the low 47 bits sign-extended. The constants
/// here are duplicated literals (not `cs_vm` re-exports) so the
/// AOT source can stay self-contained at emit time — the runtime
/// `unsafe` calls are the only `cs_vm` references.
pub fn nb_helpers_source() -> &'static str {
    NB_HELPERS_SOURCE
}

const NB_HELPERS_SOURCE: &str = r##"// --- NB inline fast-path helpers (cs-aot RC2 iter A) -------------
//
// Tag check, sign-extend payload, checked arith, range check, encode.
// On any miss (non-Fixnum operand or 47-bit overflow), delegate to
// the cs_vm runtime helper which handles Flonum/Rational/etc.

#[allow(dead_code)]
const NB_FIX_SIG_MASK: u64 = 0xFFFF_8000_0000_0000; // SIGNATURE | TAG bits
#[allow(dead_code)]
const NB_FIX_SIG_PAT:  u64 = 0xFFF8_0000_0000_0000; // SIG | (FIXNUM_TAG<<47)
#[allow(dead_code)]
const NB_PAYLOAD_MASK: u64 = (1u64 << 47) - 1;
#[allow(dead_code)]
const NB_TRUE_BITS:    i64 = 0xFFF8_8000_0000_0001u64 as i64;
#[allow(dead_code)]
const NB_FALSE_BITS:   i64 = 0xFFF8_8000_0000_0000u64 as i64;

#[inline(always)]
#[allow(dead_code)]
fn nb_extract_fixnum(payload: u64) -> i64 {
    // Sign-extend the low 47 bits: shl 17, then arithmetic shr 17.
    (((payload & NB_PAYLOAD_MASK) as i64) << 17) >> 17
}

#[inline(always)]
#[allow(dead_code)]
fn nb_encode_fixnum_if_fits(r: i64) -> Option<i64> {
    // 47-bit Fixnum range check: round-trip through sign-extend.
    let r_ext = (r << 17) >> 17;
    if r == r_ext {
        Some(((r as u64 & NB_PAYLOAD_MASK) | NB_FIX_SIG_PAT) as i64)
    } else {
        None
    }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_both_fixnum(a: i64, b: i64) -> bool {
    let (au, bu) = (a as u64, b as u64);
    (au & NB_FIX_SIG_MASK) == NB_FIX_SIG_PAT
        && (bu & NB_FIX_SIG_MASK) == NB_FIX_SIG_PAT
}

#[inline(always)]
#[allow(dead_code)]
fn nb_add_inline(a: i64, b: i64) -> i64 {
    if nb_both_fixnum(a, b) {
        let pa = nb_extract_fixnum(a as u64);
        let pb = nb_extract_fixnum(b as u64);
        if let Some(r) = pa.checked_add(pb) {
            if let Some(enc) = nb_encode_fixnum_if_fits(r) {
                return enc;
            }
        }
    }
    unsafe { cs_vm::vm::vm_value_add_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_sub_inline(a: i64, b: i64) -> i64 {
    if nb_both_fixnum(a, b) {
        let pa = nb_extract_fixnum(a as u64);
        let pb = nb_extract_fixnum(b as u64);
        if let Some(r) = pa.checked_sub(pb) {
            if let Some(enc) = nb_encode_fixnum_if_fits(r) {
                return enc;
            }
        }
    }
    unsafe { cs_vm::vm::vm_value_sub_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_mul_inline(a: i64, b: i64) -> i64 {
    if nb_both_fixnum(a, b) {
        let pa = nb_extract_fixnum(a as u64);
        let pb = nb_extract_fixnum(b as u64);
        if let Some(r) = pa.checked_mul(pb) {
            if let Some(enc) = nb_encode_fixnum_if_fits(r) {
                return enc;
            }
        }
    }
    unsafe { cs_vm::vm::vm_value_mul_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_lt_inline(a: i64, b: i64) -> i64 {
    if nb_both_fixnum(a, b) {
        let pa = nb_extract_fixnum(a as u64);
        let pb = nb_extract_fixnum(b as u64);
        return if pa < pb { NB_TRUE_BITS } else { NB_FALSE_BITS };
    }
    unsafe { cs_vm::vm::vm_value_lt_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_eq_inline(a: i64, b: i64) -> i64 {
    if nb_both_fixnum(a, b) {
        let pa = nb_extract_fixnum(a as u64);
        let pb = nb_extract_fixnum(b as u64);
        return if pa == pb { NB_TRUE_BITS } else { NB_FALSE_BITS };
    }
    unsafe { cs_vm::vm::vm_value_eq_nb(a, b) }
}

// --- Fixnum-typed fast paths (typer-hint-driven) -----------------
//
// Skip the `nb_both_fixnum` branch when the caller has proven
// both operands are Fixnum-typed (Phase 5++++ per-Value type
// inference in cs-aot). Still falls back to the runtime
// `vm_value_*_nb` on 47-bit-overflow so the result promotes to
// Bignum correctly — same semantics as the unspecialized inlines,
// just one fewer branch per call.

#[inline(always)]
#[allow(dead_code)]
fn nb_add_fixnum_inline(a: i64, b: i64) -> i64 {
    let pa = nb_extract_fixnum(a as u64);
    let pb = nb_extract_fixnum(b as u64);
    if let Some(r) = pa.checked_add(pb) {
        if let Some(enc) = nb_encode_fixnum_if_fits(r) {
            return enc;
        }
    }
    unsafe { cs_vm::vm::vm_value_add_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_sub_fixnum_inline(a: i64, b: i64) -> i64 {
    let pa = nb_extract_fixnum(a as u64);
    let pb = nb_extract_fixnum(b as u64);
    if let Some(r) = pa.checked_sub(pb) {
        if let Some(enc) = nb_encode_fixnum_if_fits(r) {
            return enc;
        }
    }
    unsafe { cs_vm::vm::vm_value_sub_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_mul_fixnum_inline(a: i64, b: i64) -> i64 {
    let pa = nb_extract_fixnum(a as u64);
    let pb = nb_extract_fixnum(b as u64);
    if let Some(r) = pa.checked_mul(pb) {
        if let Some(enc) = nb_encode_fixnum_if_fits(r) {
            return enc;
        }
    }
    unsafe { cs_vm::vm::vm_value_mul_nb(a, b) }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_lt_fixnum_inline(a: i64, b: i64) -> i64 {
    let pa = nb_extract_fixnum(a as u64);
    let pb = nb_extract_fixnum(b as u64);
    if pa < pb { NB_TRUE_BITS } else { NB_FALSE_BITS }
}

#[inline(always)]
#[allow(dead_code)]
fn nb_eq_fixnum_inline(a: i64, b: i64) -> i64 {
    let pa = nb_extract_fixnum(a as u64);
    let pb = nb_extract_fixnum(b as u64);
    if pa == pb { NB_TRUE_BITS } else { NB_FALSE_BITS }
}
// --- end NB inline fast-path helpers ----------------------------

"##;

/// Errors the AOT emitter can return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AotError {
    /// Function has zero blocks (defensive — shouldn't happen for
    /// well-formed RIR).
    EmptyFunction,

    /// Encountered an `Inst` variant the emitter doesn't yet handle.
    /// The string is the variant name for diagnostics. See the
    /// module doc for the supported set.
    UnsupportedInst(&'static str),

    /// Encountered a `Term` variant the emitter doesn't yet handle.
    /// All current Term variants (Return / Jump / Branch) are
    /// supported in iter 2a; reserved for future Term additions.
    UnsupportedTerm(&'static str),

    /// A `Const::Fixnum` value is the only constant flavor iter 1
    /// emits; other variants reach this arm.
    UnsupportedConst(&'static str),

    /// The function's `return_type` isn't one the iter-1 ABI can
    /// represent (only `Fixnum` for now).
    UnsupportedReturnType,

    /// A param's declared type isn't Fixnum.
    UnsupportedParamType,

    /// SSA value referenced before definition. Indicates malformed
    /// input RIR.
    UndefinedValue(Value),
}

impl std::fmt::Display for AotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AotError::EmptyFunction => write!(f, "cs-aot: function has no blocks"),
            AotError::UnsupportedInst(name) => {
                let (description, suggestion) = inst_user_hint(name);
                write!(
                    f,
                    "cs-aot: Inst::{name} not yet supported — {description}\n  \
                     suggestion: {suggestion}\n  \
                     reference: docs/user/aot.md (Supported/Unsupported tables)"
                )
            }
            AotError::UnsupportedTerm(name) => {
                write!(f, "cs-aot: Term::{name} not yet supported")
            }
            AotError::UnsupportedConst(name) => {
                write!(f, "cs-aot: Const::{name} not yet supported")
            }
            AotError::UnsupportedReturnType => {
                write!(f, "cs-aot: return type other than Fixnum not yet supported")
            }
            AotError::UnsupportedParamType => {
                write!(f, "cs-aot: param type other than Fixnum not yet supported")
            }
            AotError::UndefinedValue(v) => write!(f, "cs-aot: value v{} used before defined", v.0),
        }
    }
}

/// RC3 Phase 4 iter 4.1 + 4.4 — translate an Inst variant name into
/// (user-meaningful description, suggested workaround). Drives the
/// `UnsupportedInst` diagnostic format so users hit actionable
/// guidance instead of internal Inst names.

/// Detect a tail-call pattern at the end of `block`: when the
/// block's last inst is `CallSelf(dst, args)` and the
/// terminator flows the call's result straight into a
/// `Return`, return the call's args so the emitter can
/// rewrite the call into "rebind params + continue to entry"
/// — replacing the recursive function call with a loop.
///
/// Two terminator shapes count as a tail call:
///   1. `Return(dst)` directly — the call result is the
///      function's return value.
///   2. `Jump(B, [dst])` where block `B` has only one block
///      param, no insts, and a `Return(that_param)`
///      terminator — the call result threads through a
///      trivial join block.
///
/// Returns `None` for anything more complicated.
fn detect_tail_call<'a>(block: &'a cs_rir::Block, func: &'a Function) -> Option<&'a [Value]> {
    let last = block.insts.last()?;
    let (call_dst, args) = match last {
        Inst::CallSelf(dst, args) => (*dst, args.as_slice()),
        _ => return None,
    };
    match &block.terminator {
        Term::Return(v) if *v == call_dst => Some(args),
        Term::Jump(target, jump_args) if jump_args.len() == 1 && jump_args[0] == call_dst => {
            let target_block = func.blocks.iter().find(|b| b.id == *target)?;
            if target_block.params.len() != 1 || !target_block.insts.is_empty() {
                return None;
            }
            let (param_v, _) = target_block.params[0];
            match &target_block.terminator {
                Term::Return(rv) if *rv == param_v => Some(args),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Walk a `Function` in block order and infer each Value's
/// `Type` from the instruction that produced it. Used by the
/// codegen helpers (`nb_to_f64_expr` and friends) to skip the
/// defensive `as_fixnum()` discrimination when the operand's
/// type is statically known — Flonum operands of `FlonumAdd`
/// / `FlonumMul` / `FlonumSub` / `FlonumDiv` / etc. account for
/// the bulk of the gain on Flonum-heavy benches (mandelbrot,
/// nbody, spectral-norm).
///
/// Seeded from `func.params` (which carry typer-derived hints
/// per Phase 5+); subsequent Values pick up types from the
/// producing inst (FlonumAdd → Flonum, Add → Fixnum,
/// LoadConst(Flonum(_)) → Flonum, etc.). Unsupported insts
/// leave the Value out of the map, in which case downstream
/// helpers fall back to the defensive form.
fn infer_value_types(func: &Function) -> std::collections::HashMap<Value, Type> {
    let mut types: std::collections::HashMap<Value, Type> =
        std::collections::HashMap::with_capacity(func.params.len() + 16);
    for (v, ty) in &func.params {
        types.insert(*v, ty.clone());
    }
    for block in &func.blocks {
        for inst in &block.insts {
            let entry: Option<(Value, Type)> = match inst {
                Inst::LoadConst(dst, c) => {
                    let t = match c {
                        Const::Fixnum(_) => Type::Fixnum,
                        Const::Flonum(_) => Type::Flonum,
                        Const::Boolean(_) => Type::Boolean,
                        Const::Character(_) => Type::Character,
                        Const::Null => Type::Null,
                        Const::Symbol(_) => Type::Symbol,
                        Const::Unspecified | Const::Eof | Const::StringRef(_) => Type::Any,
                    };
                    Some((*dst, t))
                }
                // Integer arithmetic → Fixnum result.
                Inst::Add(dst, _, _) | Inst::Sub(dst, _, _) | Inst::Mul(dst, _, _) => {
                    Some((*dst, Type::Fixnum))
                }
                // Comparisons → Boolean (NB) or 0/1 i64 (RawI64);
                // we treat both as Boolean for type-propagation
                // purposes since neither emits Flonum arithmetic.
                Inst::Lt(dst, _, _)
                | Inst::Eq(dst, _, _)
                | Inst::FlonumLt(dst, _, _)
                | Inst::FlonumEq(dst, _, _) => Some((*dst, Type::Boolean)),
                // Flonum arithmetic → Flonum.
                Inst::FlonumAdd(dst, _, _)
                | Inst::FlonumSub(dst, _, _)
                | Inst::FlonumMul(dst, _, _)
                | Inst::FlonumDiv(dst, _, _)
                | Inst::FlonumSqrt(dst, _)
                | Inst::FlonumSin(dst, _)
                | Inst::FlonumCos(dst, _)
                | Inst::FlonumTan(dst, _)
                | Inst::FlonumLog(dst, _)
                | Inst::FlonumExp(dst, _)
                | Inst::FlonumAsin(dst, _)
                | Inst::FlonumAcos(dst, _)
                | Inst::FlonumAtan(dst, _)
                | Inst::FlonumLog2(dst, _, _)
                | Inst::FlonumAtan2(dst, _, _) => Some((*dst, Type::Flonum)),
                // SSA alias copy: dst inherits src's type.
                Inst::Move(dst, src) => types.get(src).cloned().map(|t| (*dst, t)),
                _ => None,
            };
            if let Some((d, t)) = entry {
                types.insert(d, t);
            }
        }
        // Block params get types from the predecessor's Jump
        // args. For simplicity we don't model that here — block
        // params fall through to "unknown" and the defensive
        // form fires. Adequate for the common case (the hot
        // path is the body, not the joins).
    }
    types
}

fn inst_user_hint(inst_name: &str) -> (&'static str, &'static str) {
    match inst_name {
        "MakeClosure" => (
            "your program uses a nested lambda or closure that captures variables \
             from an enclosing scope (e.g., `(lambda (x) ...)` inside another \
             function, or `(let* ((f (lambda ...))) ...)`)",
            "rewrite the inner lambda as a top-level `(define (f args) ...)` and \
             pass any captured values as extra arguments. AOT support for closures \
             is post-RC3 work (see Phase 2.1-2.4 in aot-hardening-plan.md).",
        ),
        "Call" | "CallGeneral" => (
            "your program calls a procedure value that AOT can't yet resolve to \
             a specific top-level define (the common case: passing a function as \
             an argument, or calling something looked up from a non-self global)",
            "AOT today only supports `CallSelf` (recursive calls to the function \
             being AOT'd). If the called function is also at the top level, \
             AOT each separately and chain them externally; otherwise this needs \
             Phase 2.2-2.3 (general-Call lowering).",
        ),
        "EnvLookupAny" | "EnvLookup" => (
            "your program references a variable that isn't an argument or a \
             let-binding within the AOT'd function — typically a free variable \
             captured from an enclosing scope, or a global that AOT can't yet \
             reach without runtime env support",
            "if the variable is a top-level define, inline the value or pass it \
             as an argument. For deeper fixes, this needs Phase 2.4 (env install \
             API) so AOT'd code can read from the runtime env.",
        ),
        "EnvDefineLocal" => (
            "an internal `(let ...)` or `(define ...)` binding the demote-to-SSA \
             pass couldn't lift — usually because the same name is defined in \
             multiple branches (multi-block + multi-define ambiguity)",
            "rename one of the bindings, OR restructure as a single shadowing \
             chain. iter 2.5's tier-strict rule bails on multi-block + multi-\
             define cases; future iters add φ-merge support.",
        ),
        "EnvSet" => (
            "your program uses `set!` on a free variable that AOT can't reach \
             without runtime env support",
            "rewrite the mutation as a recursive parameter pass (functional \
             style). Full `set!` support needs Phase 2.4 (env install API).",
        ),
        "Div" => (
            "Scheme `/` on Fixnum operands can produce a Rational, which the \
             AOT pipeline doesn't yet box; this Inst appears for Div in modes \
             newer cs-aot doesn't support",
            "use `quotient` for integer division if exactness isn't needed, OR \
             call this from a JIT'd context.",
        ),
        _ => (
            "cs-aot doesn't yet lower this Inst variant",
            "check docs/user/aot.md's supported-Inst table; if the gap is real \
             and your use case is important, file an issue with a minimal \
             reproducer.",
        ),
    }
}

impl std::error::Error for AotError {}

/// Output ABI for the emitted function. See the module doc for the
/// trade-offs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitMode {
    /// Raw `i64` carriers; arithmetic is `wrapping_*`; comparisons
    /// emit `if v_l <op> v_r { 1 } else { 0 }`. Self-contained:
    /// emitted source has no runtime dependency. Limited to Fixnum
    /// inputs (Flonum / other Number types panic-flow through
    /// undefined behavior — caller's responsibility to gate).
    RawI64,
    /// `i64` carriers holding [`cs_vm::vm::NanboxValue`] bit patterns.
    /// Arithmetic + comparisons delegate to `cs_vm::vm::vm_value_*_nb`
    /// runtime helpers which handle Fixnum + Flonum + Rational
    /// uniformly. Emitted source requires `cs-vm` in scope.
    Nb,
}

/// Emit Rust source for `func` using the iter-1/2a [`EmitMode::RawI64`]
/// ABI. Preserved for backward compatibility with iter-1 tests +
/// embedders that want self-contained fixnum-only emission. New code
/// should prefer [`emit_with`] with [`EmitMode::Nb`] for runtime
/// interop.
pub fn emit(func: &Function) -> Result<String, AotError> {
    emit_with(EmitMode::RawI64, func)
}

/// Emit Rust source for `func` under the specified [`EmitMode`].
/// The result is a complete definition: one `pub extern "C" fn`
/// matching the JIT's outer-trampoline signature.
pub fn emit_with(mode: EmitMode, func: &Function) -> Result<String, AotError> {
    emit_with_resolver(mode, func, &LambdaResolver::empty())
}

/// RC3 iter 2.2 Step 3 — resolver for MakeClosure / general Call.
///
/// Maps a bytecode-lambda-index (as it appears in
/// `Inst::MakeClosure(_, idx)`) to the AOT-emitted dispatch
/// wrapper's name + the lambda's arity. The project emitter
/// builds one of these from the funcs slice's `lambda_index`
/// field before calling `emit_with_resolver`.
///
/// When `MakeClosure(_, N)` references an index not in the
/// resolver, cs-aot emits the standard `UnsupportedInst`
/// diagnostic — the lambda was either not AOT-translated or
/// lives outside the AOT-emitted set.
#[derive(Debug, Clone, Default)]
pub struct LambdaResolver {
    pub by_idx: std::collections::HashMap<usize, LambdaInfo>,
    /// RC3 iter 2.7 — sym → lambda-index lookup so a surviving
    /// `EnvLookup`/`EnvLookupAny` of a top-level name resolves to
    /// the corresponding AOT'd function (emitted as a direct
    /// `vm_alloc_aot_procedure` call) rather than failing with
    /// an unresolved-capture error.
    pub by_name_sym: std::collections::HashMap<u32, usize>,
}

#[derive(Debug, Clone)]
pub struct LambdaInfo {
    /// Sanitized fn name as it appears in the emitted Rust source.
    pub fn_name: String,
    /// Number of positional args the underlying AOT'd fn takes.
    pub arity: usize,
    /// RC3 iter 2.4 — syms this lambda captures from its parent
    /// lexical scope. The caller of `MakeClosure` is responsible
    /// for gathering NB values for each sym (from its own captures
    /// or local defines) and passing them to
    /// `vm_alloc_aot_procedure_with_captures`.
    pub captures: Vec<u32>,
}

impl LambdaResolver {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn from_funcs(funcs: &[Function]) -> Self {
        let mut by_idx = std::collections::HashMap::new();
        let mut by_name_sym = std::collections::HashMap::new();
        for f in funcs {
            if let Some(idx) = f.lambda_index {
                by_idx.insert(
                    idx,
                    LambdaInfo {
                        fn_name: sanitize_ident(&f.name),
                        arity: f.params.len(),
                        captures: f.captures.clone(),
                    },
                );
                if let Some(sym) = f.name_sym {
                    by_name_sym.insert(sym, idx);
                }
            }
        }
        Self {
            by_idx,
            by_name_sym,
        }
    }
}

/// Like [`emit_with`] but also accepts a [`LambdaResolver`] that
/// `Inst::MakeClosure(_, idx)` and `Inst::Call(_, _, _)` use to
/// resolve cross-Function references. The project emitter calls
/// this directly with `LambdaResolver::from_funcs(funcs)`; the
/// no-resolver `emit_with` wrapper is kept for back-compat with
/// callers that don't care about closure / general-call lowering.
pub fn emit_with_resolver(
    mode: EmitMode,
    func: &Function,
    resolver: &LambdaResolver,
) -> Result<String, AotError> {
    // ---- Function-level validation -------------------------------
    if func.blocks.is_empty() {
        return Err(AotError::EmptyFunction);
    }
    // RawI64 mode only handles Fixnum-typed params/return. Nb mode
    // accepts any param/return type because the i64 NB carrier is
    // uniform across all Scheme value variants.
    if mode == EmitMode::RawI64 {
        if func.return_type != Type::Fixnum {
            return Err(AotError::UnsupportedReturnType);
        }
        for (_, ty) in &func.params {
            if *ty != Type::Fixnum {
                return Err(AotError::UnsupportedParamType);
            }
        }
    }

    // ---- Build the source --------------------------------------
    let mut out = String::new();
    let fn_name = sanitize_ident(&func.name);

    // Decide which shape to emit. Straight-line is reserved for the
    // simplest case (1 block, Return terminator) so iter-1 output is
    // preserved exactly for the snapshot tests + readability.
    let straight_line = func.blocks.len() == 1
        && matches!(func.blocks[0].terminator, Term::Return(_))
        && func.blocks[0].params.is_empty();

    // Doc comment: provenance + shape annotation so humans reading
    // the AOT'd source know which emitter path produced it.
    writeln!(out, "/// AOT-emitted from cs-rir Function `{}`.", func.name).unwrap();
    writeln!(out, "///").unwrap();
    writeln!(
        out,
        "/// {} param(s) · {} block(s) · {} inst(s) · {} · {} ABI",
        func.params.len(),
        func.blocks.len(),
        func.blocks.iter().map(|b| b.insts.len()).sum::<usize>(),
        if straight_line {
            "straight-line"
        } else {
            "loop+match"
        },
        match mode {
            EmitMode::RawI64 => "raw-i64",
            EmitMode::Nb => "NB-i64",
        }
    )
    .unwrap();

    // Function header. `extern "C"` matches the JIT's outer
    // trampoline so the runtime's dispatch path can `transmute` the
    // AOT'd fn pointer just like a JIT'd one.
    //
    // RC3 iter 2.4 Step 3: capturing functions prepend `__cap<sym>`
    // params (one per entry in func.captures) before the user-level
    // params. The dispatch wrapper unpacks captures + args and
    // passes both. EnvLookup(_, sym) in the body resolves to
    // `__cap<sym>` via the captures-index lookup.
    // RC3 iter 2.12 — `__self_handle: i64` is always the first param,
    // threaded by the dispatch wrapper. Holds the closure's own NB
    // Procedure handle. The body re-passes it as a capture value
    // when MakeClosure'ing an inner lambda that needs a forward
    // self-reference back to this closure. Even non-capturing fns
    // get this param (the dispatch ABI is uniform).
    write!(out, "pub extern \"C\" fn {fn_name}(__self_handle: i64").unwrap();
    for sym in &func.captures {
        out.push_str(", ");
        write!(out, "__cap{sym}: i64").unwrap();
    }
    for (v, _ty) in func.params.iter() {
        out.push_str(", ");
        // `mut` — tail-call optimization (when it fires)
        // reassigns these to the new args before
        // `continue`ing to the entry block; harmless `mut`
        // is fine for the cases where TCO doesn't fire.
        write!(out, "mut v{}: i64", v.0).unwrap();
    }
    out.push_str(") -> i64 {\n");
    // Suppress unused warning when the body doesn't need self_handle.
    out.push_str("    let _ = __self_handle;\n");

    // Pre-compute per-Value type map once; passed to all the
    // Flonum codegen helpers via inst_rhs → fbinop_rust /
    // fcmp_*_rust / funary_rust_method / fbinop_method_rust
    // so the defensive `as_fixnum()` discrimination can be
    // skipped when the operand's type is statically known.
    let types = infer_value_types(func);
    if straight_line {
        emit_straight_line(&mut out, func, mode, resolver, &types)?;
    } else {
        emit_loop_match(&mut out, func, mode, resolver, &types)?;
    }

    out.push_str("}\n");
    Ok(out)
}

/// Iter-1 shape: emit each Inst as a `let v_N = expr;` then the
/// Return value as a trailing tail expression. Preserves snapshot-
/// test output for simple functions and is more readable when the
/// CFG is degenerate.
fn emit_straight_line(
    out: &mut String,
    func: &Function,
    mode: EmitMode,
    resolver: &LambdaResolver,
    types: &std::collections::HashMap<Value, Type>,
) -> Result<(), AotError> {
    let block = &func.blocks[0];
    let mut defined: HashSet<Value> = func.params.iter().map(|(v, _)| *v).collect();
    let local_defs = collect_local_defines(func);

    let fn_name = sanitize_ident(&func.name);
    for inst in &block.insts {
        emit_inst_let(
            out,
            inst,
            &defined,
            mode,
            &fn_name,
            resolver,
            &func.captures,
            &local_defs,
            func.self_binding_sym,
            types,
        )?;
        if let Some(dst) = inst_dst(inst) {
            defined.insert(dst);
        }
    }

    if let Term::Return(v) = &block.terminator {
        require_defined(&defined, *v)?;
        writeln!(out, "    v{}", v.0).unwrap();
        Ok(())
    } else {
        // Should be unreachable given straight_line gate, but
        // defensive.
        Err(AotError::UnsupportedTerm(term_variant_name(
            &block.terminator,
        )))
    }
}

/// Iter-2a shape: pre-declare every non-param SSA Value as
/// `let mut v_N: i64 = 0;` at function top, then run a
/// `loop { match block { ... } }` state machine for block dispatch.
/// Verbose but always correct for arbitrary CFGs without needing
/// liveness analysis.
fn emit_loop_match(
    out: &mut String,
    func: &Function,
    mode: EmitMode,
    resolver: &LambdaResolver,
    types: &std::collections::HashMap<Value, Type>,
) -> Result<(), AotError> {
    // Pre-declare every Value that ISN'T already a function
    // parameter. Block params and Inst destinations all go here.
    // Using `let mut` + assignment (not `let`) means a Value defined
    // in one block can be read in another without scope issues.
    let params: HashSet<Value> = func.params.iter().map(|(v, _)| *v).collect();
    let mut all_values: Vec<Value> = Vec::new();
    for block in &func.blocks {
        for (v, _) in &block.params {
            if !params.contains(v) {
                all_values.push(*v);
            }
        }
        for inst in &block.insts {
            if let Some(dst) = inst_dst(inst) {
                if !params.contains(&dst) {
                    all_values.push(dst);
                }
            }
        }
    }
    all_values.sort_by_key(|v| v.0);
    all_values.dedup();

    for v in &all_values {
        writeln!(out, "    let mut v{}: i64 = 0;", v.0).unwrap();
    }

    // Dispatch state machine. `block` carries the current block id;
    // `loop { match block { ... } }` runs forever, with each block
    // arm either jumping (`block = ...; continue;`) or returning.
    writeln!(out, "    let mut block: u32 = {};", func.entry.0).unwrap();
    writeln!(out, "    loop {{").unwrap();
    writeln!(out, "        match block {{").unwrap();

    let fn_name = sanitize_ident(&func.name);
    let local_defs = collect_local_defines(func);
    for block in &func.blocks {
        writeln!(out, "            {} => {{", block.id.0).unwrap();
        // Phase 5++++: tail-call detection. If the block's
        // last inst is a `CallSelf(dst, args)` whose result
        // flows straight into a Return (either directly via
        // this block's terminator OR via a Jump to a Return-
        // only block), replace the recursive function call
        // with "rebind params + continue to entry" — saves
        // a stack frame per iteration, which matters a lot
        // for inner-loop kernels like mandelbrot's proc_loop.
        let tco = detect_tail_call(block, func);
        let last_inst_is_tco = tco.is_some();
        // Emit insts except the trailing CallSelf when
        // TCO is firing — the CallSelf gets replaced
        // wholesale with the param-rebind + continue.
        let inst_count = if last_inst_is_tco {
            block.insts.len().saturating_sub(1)
        } else {
            block.insts.len()
        };
        for inst in &block.insts[..inst_count] {
            emit_inst_assign(
                out,
                inst,
                mode,
                &fn_name,
                resolver,
                &func.captures,
                &local_defs,
                func.self_binding_sym,
                types,
            )?;
        }
        if let Some(args) = tco {
            // Reassign params then jump to entry. Order is
            // safe because each `args[i]` is an SSA Value
            // (a `vN` variable distinct from any param `vM`),
            // so `v{param} = v{arg}` writes don't clobber a
            // value still needed by later assignments.
            for ((param_v, _), arg_v) in func.params.iter().zip(args.iter()) {
                writeln!(out, "                v{} = v{};", param_v.0, arg_v.0).unwrap();
            }
            writeln!(out, "                block = {};", func.entry.0).unwrap();
            writeln!(out, "                continue;").unwrap();
        } else {
            emit_terminator(out, &block.terminator, func, mode)?;
        }
        writeln!(out, "            }}").unwrap();
    }

    // unreachable!() is the safety net: well-formed RIR never
    // dispatches to a non-existent block id, but rustc demands a
    // catch-all for non-exhaustive integer matches.
    writeln!(out, "            _ => unreachable!(),").unwrap();
    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    Ok(())
}

/// Emit an Inst as `let v_N: i64 = expr;` (straight-line shape).
/// Used only by `emit_straight_line`; the loop+match shape uses
/// `emit_inst_assign` instead (assignment to pre-declared mut).
/// RC3 iter 2.4 + 2.7 — scan a function and build a `sym → Value`
/// map covering both:
///   1. Positional param syms (`func.param_syms[i] → func.params[i].0`)
///   2. `EnvDefineLocal(sym, value)` insts (let / internal-define
///      bindings).
///
/// `MakeClosure` lowering uses this to resolve callee-capture-syms
/// to the right Value in the caller's scope. Params come first so a
/// shadowing local define wins (later inserts overwrite).
fn collect_local_defines(func: &Function) -> std::collections::HashMap<u32, Value> {
    let mut map = std::collections::HashMap::new();
    for (i, sym) in func.param_syms.iter().enumerate() {
        if let Some((v, _)) = func.params.get(i) {
            map.insert(*sym, *v);
        }
    }
    for block in &func.blocks {
        for inst in &block.insts {
            if let Inst::EnvDefineLocal(sym, val) = inst {
                map.insert(*sym, *val);
            }
        }
    }
    map
}

fn emit_inst_let(
    out: &mut String,
    inst: &Inst,
    defined: &HashSet<Value>,
    mode: EmitMode,
    self_fn_name: &str,
    resolver: &LambdaResolver,
    captures: &[u32],
    local_defs: &std::collections::HashMap<u32, Value>,
    self_binding_sym: Option<u32>,
    types: &std::collections::HashMap<Value, Type>,
) -> Result<(), AotError> {
    // RC3 iter 2.11 — EnvDefineLocal that survived demote (the
    // multi-block + multi-define case) lowers to a no-op: the
    // `collect_local_defines` pre-scan already records sym → Value,
    // so subsequent EnvLookups read directly from the source Value
    // via the `local_defs.contains_key(sym)` arm in inst_rhs. The
    // define itself produces no SSA result, so there's nothing to
    // emit.
    if matches!(inst, Inst::EnvDefineLocal(..)) {
        return Ok(());
    }
    // RC3 iter 2.15 — AnyDrop is a JIT-tier refcount-bookkeeping
    // op (frees the Box cloned at param-LoadVar time). In NB-AOT
    // mode the params arrive as plain i64 NB carriers — no
    // separate Box to drop — so the inst is a no-op. Heap-pointer-
    // tagged NB values (Pair, Vector, etc.) have their own
    // lifetime story tied to the proc_table / Gc heap, not to
    // explicit AnyDrop.
    if matches!(inst, Inst::AnyDrop(..)) {
        return Ok(());
    }
    let (dst, expr) = inst_rhs(
        inst,
        Some(defined),
        mode,
        self_fn_name,
        resolver,
        captures,
        local_defs,
        self_binding_sym,
        types,
    )?;
    writeln!(out, "    let v{}: i64 = {};", dst.0, expr).unwrap();
    Ok(())
}

/// Emit an Inst as `v_N = expr;` (loop+match shape). The Value is
/// pre-declared as `let mut` at function top.
fn emit_inst_assign(
    out: &mut String,
    inst: &Inst,
    mode: EmitMode,
    self_fn_name: &str,
    resolver: &LambdaResolver,
    captures: &[u32],
    local_defs: &std::collections::HashMap<u32, Value>,
    self_binding_sym: Option<u32>,
    types: &std::collections::HashMap<Value, Type>,
) -> Result<(), AotError> {
    // Same no-op as emit_inst_let for surviving EnvDefineLocal.
    if matches!(inst, Inst::EnvDefineLocal(..)) {
        return Ok(());
    }
    // Same no-op as emit_inst_let for AnyDrop (iter 2.15).
    if matches!(inst, Inst::AnyDrop(..)) {
        return Ok(());
    }
    // The loop+match shape pre-declares all values; SSA validity is
    // a property of well-formed RIR, not something we re-check here
    // (the check requires cross-block dataflow analysis we don't yet
    // do in the emitter). Pass `None` to skip.
    let (dst, expr) = inst_rhs(
        inst,
        None,
        mode,
        self_fn_name,
        resolver,
        captures,
        local_defs,
        self_binding_sym,
        types,
    )?;
    writeln!(out, "                v{} = {};", dst.0, expr).unwrap();
    Ok(())
}

/// Compute the (dst, RHS-expression) pair for an Inst. The expression
/// is in Rust source form ready to be assigned. When `defined` is
/// `Some(set)`, perform use-before-def detection against it; when
/// `None`, skip (the loop+match shape uses this).
fn inst_rhs(
    inst: &Inst,
    defined: Option<&HashSet<Value>>,
    mode: EmitMode,
    self_fn_name: &str,
    resolver: &LambdaResolver,
    captures: &[u32],
    local_defs: &std::collections::HashMap<u32, Value>,
    self_binding_sym: Option<u32>,
    types: &std::collections::HashMap<Value, Type>,
) -> Result<(Value, String), AotError> {
    let check = |v: Value| -> Result<(), AotError> {
        match defined {
            Some(set) if !set.contains(&v) => Err(AotError::UndefinedValue(v)),
            _ => Ok(()),
        }
    };

    Ok(match (inst, mode) {
        // ---- LoadConst ----
        (Inst::LoadConst(dst, c), EmitMode::RawI64) => (*dst, const_to_rust_i64(c)?),
        (Inst::LoadConst(dst, c), EmitMode::Nb) => (*dst, const_to_rust_nb(c)?),

        // ---- Arithmetic ----
        //
        // RawI64: `wrapping_*` ops, self-contained.
        // Nb: delegate to runtime helpers (matches JIT slow path;
        //     A2c-ish optimization can add inline fast paths).
        (Inst::Add(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_add(v{})", lhs.0, rhs.0))
        }
        (Inst::Sub(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_sub(v{})", lhs.0, rhs.0))
        }
        (Inst::Mul(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_mul(v{})", lhs.0, rhs.0))
        }
        // Nb mode: call into the prologue helpers (nb_*_inline) the
        // caller is expected to inject — open-coded tag check +
        // checked arith + encode, with fallback to vm_value_*_nb on
        // miss. See `nb_helpers_source()` for the helper definitions.
        (Inst::Add(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, nb_arith_call("add", lhs, rhs, types))
        }
        (Inst::Sub(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, nb_arith_call("sub", lhs, rhs, types))
        }
        (Inst::Mul(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, nb_arith_call("mul", lhs, rhs, types))
        }

        // ---- Comparisons ----
        //
        // RawI64: produce 0/1 i64, matching the JIT's `emit_nb_cmp_
        // fixnum_fast` pre-tag shape.
        // Nb: delegate to vm_value_*_nb which returns an NB Boolean.
        (Inst::Lt(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (
                *dst,
                format!("if v{} < v{} {{ 1 }} else {{ 0 }}", lhs.0, rhs.0),
            )
        }
        (Inst::Eq(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (
                *dst,
                format!("if v{} == v{} {{ 1 }} else {{ 0 }}", lhs.0, rhs.0),
            )
        }
        (Inst::Lt(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, nb_arith_call("lt", lhs, rhs, types))
        }
        (Inst::Eq(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, nb_arith_call("eq", lhs, rhs, types))
        }

        // ---- Move (alias) ----
        // Identical across modes: i64 → i64 copy.
        (Inst::Move(dst, src), _) => {
            check(*src)?;
            (*dst, format!("v{}", src.0))
        }

        // ---- Identity-in-NB ops (RC2 iter J) ----
        //
        // In the uniform-NB ABI cs-aot's Nb mode shares with the JIT,
        // these box/unbox/bitcast Insts are all no-ops because every
        // typed-lane value is already an NB carrier with its proper
        // tag. The JIT's lowering at cs-jit-cranelift/src/lowering.rs
        // explicitly comments "BoxTyped is an identity in uniform-NB"
        // and groups the rest of this set under the same identity arm.
        // cs-aot mirrors that — emit as Move.
        //
        // In RawI64 mode these don't have a defined meaning (RawI64
        // doesn't track NB tags), so we accept the identity emission
        // as the natural fallback; callers using RawI64 shouldn't be
        // feeding it programs that rely on Any/Fixnum unboxing.
        (Inst::AnyToFix(dst, src), _)
        | (Inst::AnyToBool(dst, src), _)
        | (Inst::AnyToFlo(dst, src), _)
        | (Inst::FixToFlo(dst, src), _)
        | (Inst::IntCharBitcast(dst, src), _) => {
            check(*src)?;
            (*dst, format!("v{}", src.0))
        }
        // RC3 iter 2.16 — AnyTruthy must actually check truthiness in
        // NB mode. NB Boolean false = 0xFFF8_8000_0000_0000; anything
        // else (including NB Fixnum 0, NB Null, NB Pair, etc.) is
        // truthy per R6RS (only #f is falsy). Emit as `(v != NB_FALSE)
        // as NB Boolean` — uses xor-with-1 trick: NB_FALSE | 0 stays
        // NB_FALSE, NB_FALSE | 1 = NB_TRUE. So result is NB Boolean
        // (true if v is truthy, false if v is NB_FALSE).
        // RawI64 mode: pass through as i64 0/1.
        (Inst::AnyTruthy(dst, src), mode) => {
            check(*src)?;
            let expr = match mode {
                EmitMode::RawI64 => format!("(v{} != 0) as i64", src.0),
                EmitMode::Nb => format!(
                    "((((v{} != 0xfff8_8000_0000_0000u64 as i64) as u64) | 0xfff8_8000_0000_0000u64) as i64)",
                    src.0
                ),
            };
            (*dst, expr)
        }
        // RC3 iter 2.14 — boolean negation. NB_TRUE / NB_FALSE differ
        // only in bit 0 (NB_TRUE = NB_FALSE | 1); xor with 1 flips
        // it. RawI64 mode treats the operand as 0/1 i64 so xor-with-1
        // also flips it. The dst type stays Boolean.
        (Inst::NotBoolean(dst, src), _) => {
            check(*src)?;
            (*dst, format!("(v{} ^ 1)", src.0))
        }
        (Inst::BoxTyped(dst, src, _tag), _) => {
            check(*src)?;
            (*dst, format!("v{}", src.0))
        }

        // ---- Type predicates (RC2 iter L) ----
        //
        // Each lowers to a single runtime-helper call returning 0/1
        // i64 (the helpers themselves are bool-as-i64, not NB-encoded).
        // RawI64 mode passes through; Nb mode wraps with the OR-with-
        // NB_FALSE_BITS trick so the result is an NB Boolean usable
        // by Branch and downstream consumers.
        (Inst::PairP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_pair_p_gc", src, m))
        }
        (Inst::NullP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_null_p_gc", src, m))
        }
        (Inst::VecP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_vector_p_gc", src, m))
        }
        (Inst::ProcedureP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_procedure_p_gc", src, m))
        }
        (Inst::SymbolP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_symbol_p_gc", src, m))
        }
        (Inst::FixnumP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_fixnum_p_gc", src, m))
        }
        (Inst::FlonumP(dst, src), m) => {
            check(*src)?;
            (*dst, tpred_rust("vm_flonum_p_gc", src, m))
        }

        // ---- Vector primitives (RC2 iter M) ----
        //
        // All call into cs-vm's `vm_*_vector_*_gc` runtime helpers.
        // Operands and results are NB carriers (Gc handles for the
        // vector, Fixnum-NB for indices/length, NB Any for values).
        // Identical lowering in both EmitModes because the heap
        // helpers operate on NB carriers regardless of the caller's
        // ABI choice.
        //
        // VecAlloc: `dst = make-vector(n, fill)`. vm_alloc_vector_gc
        // takes raw i64 length + Any-tagged fill (per `gc_i64_to_value`
        // decoder). RC3 iter 2.18 — n is an NB Fixnum carrier in NB
        // mode; extract the payload before passing as raw length.
        // The fill stays NB-encoded (vm_alloc_vector_gc decodes it
        // via gc_i64_to_value which handles NB).
        (Inst::VecAlloc(dst, n, fill), mode) => {
            check(*n)?;
            check(*fill)?;
            let n_expr = match mode {
                EmitMode::RawI64 => format!("v{}", n.0),
                EmitMode::Nb => {
                    format!("cs_vm::vm::NanboxValue(v{}).as_fixnum().unwrap_or(0)", n.0)
                }
            };
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_alloc_vector_gc({n_expr}, v{}) }}",
                    fill.0
                ),
            )
        }
        (Inst::VecRef(dst, vec, idx), mode) => {
            check(*vec)?;
            check(*idx)?;
            // vm_vector_ref_gc also takes raw i64 idx + NB-tagged vec.
            let idx_expr = match mode {
                EmitMode::RawI64 => format!("v{}", idx.0),
                EmitMode::Nb => format!(
                    "cs_vm::vm::NanboxValue(v{}).as_fixnum().unwrap_or(0)",
                    idx.0
                ),
            };
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_vector_ref_gc(cs_vm::vm::vm_value_clone_gc(v{}), {idx_expr}) }}",
                    vec.0
                ),
            )
        }
        (Inst::VecSet(dst, vec, idx, val), mode) => {
            check(*vec)?;
            check(*idx)?;
            check(*val)?;
            let idx_expr = match mode {
                EmitMode::RawI64 => format!("v{}", idx.0),
                EmitMode::Nb => format!(
                    "cs_vm::vm::NanboxValue(v{}).as_fixnum().unwrap_or(0)",
                    idx.0
                ),
            };
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_vector_set_gc(cs_vm::vm::vm_value_clone_gc(v{}), {idx_expr}, v{}) }}",
                    vec.0, val.0
                ),
            )
        }
        (Inst::VecLength(dst, vec), _) => {
            check(*vec)?;
            (
                *dst,
                format!("unsafe {{ cs_vm::vm::vm_vector_length_gc(v{}) }}", vec.0),
            )
        }

        // ---- Pair primitives (RC2 iter N) ----
        //
        // Cons takes per-operand tag bytes (NB JIT_RT_* values
        // embedded at translate time) that vm_alloc_pair_gc passes
        // to the underlying allocator. car/cdr are simple slot
        // accessors via the *_gc helpers.
        (Inst::Cons(dst, car_v, car_tag, cdr_v, cdr_tag), mode) => {
            check(*car_v)?;
            check(*cdr_v)?;
            // RC3 iter 2.16 — in NB mode, operands are NB-encoded i64
            // carriers. vm_alloc_pair_gc routes the tag byte through
            // i64_to_value, which only knows how to decode NB via the
            // JIT_RT_ANY arm (other tags treat the i64 as a raw
            // value and produce garbage). Force JIT_RT_ANY in NB
            // mode regardless of what the translator inferred. The
            // RawI64 mode keeps the original tag (operands are raw
            // i64 there).
            let (car_t, cdr_t) = match mode {
                EmitMode::RawI64 => (*car_tag, *cdr_tag),
                EmitMode::Nb => (15u8, 15u8), // JIT_RT_ANY
            };
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_alloc_pair_gc(v{}, {}u8, v{}, {}u8) }}",
                    car_v.0, car_t, cdr_v.0, cdr_t
                ),
            )
        }
        // RC3 iter 2.16 follow-up: vm_pair_car_gc / vm_pair_cdr_gc /
        // vm_length_gc CONSUME their input handle (linear ownership).
        // The demote pass aliases EnvLookupAny so multiple Scheme-
        // level references to the SAME let-bound pair collapse to a
        // single SSA Value. When two cs-rir Insts then consume that
        // Value (e.g., `(let ((p (cons 1 2))) (+ (car p) (cdr p)))`),
        // the second consumer hits a freed/borrowed pair → panic.
        //
        // Fix: clone the input handle (refcount bump via
        // vm_value_clone_gc) before each consume so each helper gets
        // its own owned reference. NB inline immediates (Fixnum etc.)
        // make the clone a no-op (vm_value_clone_gc checks
        // any_i64_is_inline). The cost is one branch per pair op for
        // the common heap path — negligible vs the helper call itself.
        (Inst::Car(dst, pair), _) => {
            check(*pair)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_pair_car_gc(cs_vm::vm::vm_value_clone_gc(v{})) }}",
                    pair.0
                ),
            )
        }
        (Inst::Cdr(dst, pair), _) => {
            check(*pair)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_pair_cdr_gc(cs_vm::vm::vm_value_clone_gc(v{})) }}",
                    pair.0
                ),
            )
        }
        // RC3 iter 2.15 — `(length list)`. vm_length_gc returns the
        // raw i64 count (not NB-encoded). Wrap in NanboxValue::fixnum
        // so downstream NB-consuming ops see a proper Fixnum carrier.
        // RawI64 mode passes the raw count through directly.
        // Also clones the input per the consume-on-use story above.
        (Inst::Length(dst, list), mode) => {
            check(*list)?;
            let call = format!(
                "unsafe {{ cs_vm::vm::vm_length_gc(cs_vm::vm::vm_value_clone_gc(v{})) }}",
                list.0
            );
            let expr = match mode {
                EmitMode::RawI64 => call,
                EmitMode::Nb => {
                    format!("cs_vm::vm::NanboxValue::fixnum({call}).into_raw()")
                }
            };
            (*dst, expr)
        }

        // ---- Equality predicates (RC2 iter S) ----
        //
        // EqAny / EqualAny both call cs-vm runtime helpers and
        // return 0/1 i64. Same NB-Boolean wrap pattern as iter L's
        // type predicates.
        (Inst::EqAny(dst, lhs, rhs), m) => {
            check(*lhs)?;
            check(*rhs)?;
            let call = format!(
                "unsafe {{ cs_vm::vm::vm_eq_any_gc(v{}, v{}) }}",
                lhs.0, rhs.0
            );
            let expr = match m {
                EmitMode::RawI64 => call,
                EmitMode::Nb => {
                    format!("(({call} as u64) | 0xfff8_8000_0000_0000u64) as i64")
                }
            };
            (*dst, expr)
        }
        (Inst::EqualAny(dst, lhs, rhs), m) => {
            check(*lhs)?;
            check(*rhs)?;
            let call = format!(
                "unsafe {{ cs_vm::vm::vm_equal_gc(v{}, v{}) }}",
                lhs.0, rhs.0
            );
            let expr = match m {
                EmitMode::RawI64 => call,
                EmitMode::Nb => {
                    format!("(({call} as u64) | 0xfff8_8000_0000_0000u64) as i64")
                }
            };
            (*dst, expr)
        }

        // ---- AnyClone (RC2 iter T) ----
        //
        // Bumps the refcount on an Any-tagged box. Lowers to
        // `vm_value_clone_gc(r)` which returns a fresh handle with
        // an incremented refcount on the same payload. Used when a
        // value needs to be consumed twice (e.g., passed to two
        // different runtime helpers).
        (Inst::AnyClone(dst, src), _) => {
            check(*src)?;
            (
                *dst,
                format!("unsafe {{ cs_vm::vm::vm_value_clone_gc(v{}) }}", src.0),
            )
        }

        // ---- Division ----
        //
        // RawI64 mode: integer division via `wrapping_div`. Panics on
        // /0 and on i64::MIN/-1 — both are caller errors, same shape
        // as Rust's stdlib `/` operator. Different semantics from
        // Scheme `(/ a b)` which can produce a Rational; RawI64 is
        // explicitly the unboxed-fixnum lane.
        //
        // Nb mode: delegate to vm_value_div_nb. Per cs-rir's Div doc,
        // there's no inline fast path because Fixnum/Fixnum can
        // produce a Rational (R6RS exact division), which doesn't
        // fit the NB Fixnum lane — always slow-path.
        (Inst::Div(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("v{}.wrapping_div(v{})", lhs.0, rhs.0))
        }
        (Inst::Div(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_value_div_nb(v{}, v{}) }}",
                    lhs.0, rhs.0
                ),
            )
        }

        // ---- Flonum arithmetic ----
        //
        // Both modes use the same emission: bitcast both i64 operands
        // to f64, apply the IEEE-754 op, bitcast result back to i64.
        // Matches the JIT's `fbinop` shape (cs-jit-cranelift/src/
        // lowering.rs).
        //
        // NB carrier note: NB Flonum encoding is raw f64 bits unless
        // the bit pattern collides with the tagged-NaN range, in
        // which case the runtime canonicalizes to `NB_NAN_BITS`. The
        // emitter doesn't do that canonicalization here — same as
        // the JIT — so arith producing such a NaN can in theory
        // round-trip as a "tagged" pattern. Practically, normal
        // arith never produces a sign=1 quiet NaN; the only path
        // is explicit `f64::from_bits(0xFFF8_...)` operands.
        (Inst::FlonumAdd(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("+", lhs, rhs, types))
        }
        (Inst::FlonumSub(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("-", lhs, rhs, types))
        }
        (Inst::FlonumMul(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("*", lhs, rhs, types))
        }
        (Inst::FlonumDiv(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("/", lhs, rhs, types))
        }

        // ---- Flonum comparisons ----
        //
        // IEEE-754 ordering, distinct from `Lt`/`Eq` which would
        // compare bit patterns and mishandle -0.0 / NaN. Result is:
        //   RawI64: 0/1 i64 (matches non-Flonum cmp emission).
        //   Nb:     NB Boolean — encoded by ORing the {0,1} compare
        //           result with NB_FALSE_BITS so 0→NB_FALSE and
        //           1→NB_TRUE. Mirrors the JIT's FlonumLt lowering.
        (Inst::FlonumLt(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_raw_rust("<", lhs, rhs, types))
        }
        (Inst::FlonumEq(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_raw_rust("==", lhs, rhs, types))
        }
        (Inst::FlonumLt(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_nb_rust("<", lhs, rhs, types))
        }
        (Inst::FlonumEq(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_nb_rust("==", lhs, rhs, types))
        }

        // ---- Flonum unary ops (RC2 iter D) ----
        //
        // All lower to Rust stdlib f64 methods. The JIT uses
        // Cranelift intrinsics for sqrt/abs/floor/ceil/trunc/round
        // (single x86 instructions) and runtime-helper calls for
        // the transcendentals — `rustc -O` produces equivalent
        // code for the AOT case (calls into libm for the
        // transcendentals, single instructions for the cheap ones).
        // Identical in both EmitModes; operand and dst are i64
        // carriers of f64 bit patterns.
        (Inst::FlonumSqrt(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("sqrt", src, types))
        }
        (Inst::FlonumAbs(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("abs", src, types))
        }
        (Inst::FlonumFloor(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("floor", src, types))
        }
        (Inst::FlonumCeil(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("ceil", src, types))
        }
        (Inst::FlonumTrunc(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("trunc", src, types))
        }
        (Inst::FlonumRound(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("round", src, types))
        }
        (Inst::FlonumSin(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("sin", src, types))
        }
        (Inst::FlonumCos(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("cos", src, types))
        }
        (Inst::FlonumTan(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("tan", src, types))
        }
        (Inst::FlonumLog(dst, src), _) => {
            check(*src)?;
            // Scheme `log` is the natural log → Rust's `f64::ln`.
            (*dst, funary_rust_method("ln", src, types))
        }
        (Inst::FlonumExp(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("exp", src, types))
        }
        (Inst::FlonumAsin(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("asin", src, types))
        }
        (Inst::FlonumAcos(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("acos", src, types))
        }
        (Inst::FlonumAtan(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("atan", src, types))
        }

        // ---- Flonum binary ops (RC2 iter D) ----
        (Inst::FlonumMax(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_method_rust("max", lhs, rhs, types))
        }
        (Inst::FlonumMin(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_method_rust("min", lhs, rhs, types))
        }
        // Per the JIT lowering, all three are (dst, n, base) shape.
        // Rust stdlib mappings:
        //   FlonumLog2 → n.log(base)  (log base `base` of n)
        //   FlonumAtan2 → n.atan2(base)
        //   FlonumExpt  → n.powf(base)
        (Inst::FlonumLog2(dst, n, base), _) => {
            check(*n)?;
            check(*base)?;
            (*dst, fbinop_method_rust("log", n, base, types))
        }
        (Inst::FlonumAtan2(dst, n, base), _) => {
            check(*n)?;
            check(*base)?;
            (*dst, fbinop_method_rust("atan2", n, base, types))
        }
        (Inst::FlonumExpt(dst, n, base), _) => {
            check(*n)?;
            check(*base)?;
            (*dst, fbinop_method_rust("powf", n, base, types))
        }

        // ---- Flonum predicates (RC2 iter D) ----
        //
        // is_nan + is_infinite return bool. Encode the same way as
        // FlonumLt/Eq: 0/1 i64 in RawI64 mode; (cmp | NB_FALSE_BITS)
        // in Nb mode.
        (Inst::FlonumIsNan(dst, src), EmitMode::RawI64) => {
            check(*src)?;
            (*dst, fpredicate_raw_rust("is_nan", src))
        }
        (Inst::FlonumIsInfinite(dst, src), EmitMode::RawI64) => {
            check(*src)?;
            (*dst, fpredicate_raw_rust("is_infinite", src))
        }
        (Inst::FlonumIsNan(dst, src), EmitMode::Nb) => {
            check(*src)?;
            (*dst, fpredicate_nb_rust("is_nan", src))
        }
        (Inst::FlonumIsInfinite(dst, src), EmitMode::Nb) => {
            check(*src)?;
            (*dst, fpredicate_nb_rust("is_infinite", src))
        }

        // ---- CallSelf (recursive call) ----
        //
        // Both modes lower identically: a direct Rust call to the
        // function being emitted. The AOT'd function is
        // `pub extern "C" fn {self_fn_name}(args...) -> i64`, so a
        // direct call uses the C ABI and threads i64 carriers
        // through registers — the same shape every operand already
        // uses. Recursion bottoms out naturally because the IR is
        // already in self-recursive form.
        //
        // RawI64 mode: each arg is a raw i64; recursion semantics
        //              are whatever the caller defines (unchecked
        //              arithmetic, etc.).
        // Nb mode: each arg is an NB carrier; the recursive call
        //          returns an NB carrier. Same contract as the
        //          inline arith helpers.
        (Inst::CallSelf(dst, args), _) => {
            for arg in args {
                check(*arg)?;
            }
            // RC3 iter 2.4 + 2.12: CallSelf in a capturing function
            // passes through the same captures (recursive calls are
            // within the same closure, so captures don't change).
            // Also threads __self_handle (iter 2.12) — the recursive
            // call lives in the same closure, so the same handle
            // applies.
            let mut call = String::from(self_fn_name);
            call.push('(');
            call.push_str("__self_handle");
            for sym in captures {
                call.push_str(", ");
                call.push_str(&format!("__cap{sym}"));
            }
            for arg in args {
                call.push_str(", ");
                call.push_str(&format!("v{}", arg.0));
            }
            call.push(')');
            (*dst, call)
        }

        // ---- EnvLookup / EnvLookupAny (RC3 iter 2.4 + 2.7) ----
        //
        // After the demote pass, surviving EnvLookups reference one
        // of three things, in lookup-priority order:
        //
        //   1. (iter 2.4) A captured free variable — the sym is in
        //      this function's captures list, so we read the
        //      `__cap<sym>` prefix param the dispatch wrapper
        //      unpacked.
        //   2. (iter 2.7) A top-level AOT'd function — the sym
        //      resolves through the resolver's by_name_sym table;
        //      we emit a `vm_alloc_aot_procedure` call to build a
        //      fresh Procedure NB value pointing at the other AOT'd
        //      fn's dispatch wrapper.
        //   3. (iter 2.4) An `EnvDefineLocal(sym, v)` already in
        //      scope — for top-level scripts that hoist things via
        //      define before MakeClosure. Emit a direct `v<v.0>`
        //      copy.
        //
        // None of the three → falls through to the unsupported-Inst
        // catch-all at the bottom of the match.
        (Inst::EnvLookup(dst, sym), _) | (Inst::EnvLookupAny(dst, sym), _)
            if captures.contains(sym) =>
        {
            (*dst, format!("__cap{sym}"))
        }
        (Inst::EnvLookup(dst, sym), _) | (Inst::EnvLookupAny(dst, sym), _)
            if self_binding_sym == Some(*sym) =>
        {
            // RC3 iter 2.13 — the iter 2.13 marker-materialization
            // turns SelfRef on the stack at a branch point into an
            // EnvLookup of the function's own self-name. The lookup
            // value at runtime IS this closure's own Procedure
            // handle — exactly what __self_handle holds (iter 2.12).
            // Without this arm we'd treat the lookup as a capture
            // and fail at the caller's MakeClosure (the caller would
            // need to provide this closure's handle, but the closure
            // doesn't exist yet at MakeClosure time).
            (*dst, "__self_handle".to_string())
        }
        (Inst::EnvLookup(dst, sym), EmitMode::Nb)
        | (Inst::EnvLookupAny(dst, sym), EmitMode::Nb)
            if resolver.by_name_sym.contains_key(sym) =>
        {
            let idx = resolver.by_name_sym[sym];
            let info = &resolver.by_idx[&idx];
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_alloc_aot_procedure({}_aot_dispatch as usize, {}u32) }}",
                    info.fn_name, info.arity
                ),
            )
        }
        (Inst::EnvLookup(dst, sym), _) | (Inst::EnvLookupAny(dst, sym), _)
            if local_defs.contains_key(sym) =>
        {
            let v = local_defs[sym];
            (*dst, format!("v{}", v.0))
        }

        // ---- MakeClosure (RC3 iter 2.2 Step 3 + 2.4) ----
        //
        // Wraps an AOT-emitted lambda's dispatch fn pointer in a
        // VmAotClosure via cs-vm's `vm_alloc_aot_procedure` (no
        // captures) or `vm_alloc_aot_procedure_with_captures` (with
        // captures). The resolver maps the bytecode-lambda-index to
        // the AOT'd fn name + arity + captures; if the lookup misses,
        // the lambda wasn't part of the AOT'd set and we fail cleanly.
        //
        // RC3 iter 2.4: gather captured values from the caller's
        // scope. Each callee-capture-sym resolves to either:
        //   - the caller's own captures list → emit `__cap<sym>`
        //   - an `EnvDefineLocal(sym, v)` earlier in the function →
        //     emit `v<v.0>`
        //   - neither → UnsupportedInst (sym not in scope at this
        //     MakeClosure point — likely a cross-block flow we don't
        //     yet model).
        (Inst::MakeClosure(dst, lambda_idx), EmitMode::Nb) => {
            let info = resolver
                .by_idx
                .get(&(*lambda_idx as usize))
                .ok_or_else(|| {
                    // The standard UnsupportedInst diagnostic already
                    // covers MakeClosure; the resolver miss case is
                    // semantically the same ("we can't emit this") for
                    // the user.
                    AotError::UnsupportedInst("MakeClosure")
                })?;
            if info.captures.is_empty() {
                (
                    *dst,
                    format!(
                        "unsafe {{ cs_vm::vm::vm_alloc_aot_procedure({}_aot_dispatch as usize, {}u32) }}",
                        info.fn_name, info.arity
                    ),
                )
            } else {
                let mut cap_exprs: Vec<String> = Vec::with_capacity(info.captures.len());
                for sym in &info.captures {
                    if captures.contains(sym) {
                        cap_exprs.push(format!("__cap{sym}"));
                    } else if let Some(v) = local_defs.get(sym) {
                        cap_exprs.push(format!("v{}", v.0));
                    } else if self_binding_sym == Some(*sym) {
                        // RC3 iter 2.12 — forward self-reference
                        // capture. The inner lambda's capture-sym is
                        // the CALLER's own letrec binding name. The
                        // caller's __self_handle (threaded by the
                        // dispatch ABI) is the NB Procedure handle
                        // for this very closure — exactly what the
                        // inner lambda needs to call back into us.
                        cap_exprs.push("__self_handle".to_string());
                    } else if let Some(other_idx) = resolver.by_name_sym.get(sym) {
                        // RC3 iter 2.7 — capture is a top-level AOT'd
                        // function. Allocate a fresh Procedure NB
                        // value pointing at the other fn's dispatch
                        // wrapper.
                        let other = &resolver.by_idx[other_idx];
                        cap_exprs.push(format!(
                            "unsafe {{ cs_vm::vm::vm_alloc_aot_procedure({}_aot_dispatch as usize, {}u32) }}",
                            other.fn_name, other.arity
                        ));
                    } else {
                        return Err(AotError::UnsupportedInst(
                            "MakeClosure with unresolved capture",
                        ));
                    }
                }
                let cap_csv = cap_exprs.join(", ");
                let n_caps = info.captures.len();
                (
                    *dst,
                    format!(
                        "{{ let __aot_caps: [i64; {n_caps}] = [{cap_csv}]; \
                         unsafe {{ cs_vm::vm::vm_alloc_aot_procedure_with_captures(\
                         {}_aot_dispatch as usize, {}u32, __aot_caps.as_ptr(), {n_caps}) }} }}",
                        info.fn_name, info.arity
                    ),
                )
            }
        }

        // ---- general Call (RC3 iter 2.2 Step 4) ----
        //
        // Dispatch through a Procedure-value via cs-vm's
        // `vm_call_aot_procedure`. The callee is an NB carrier
        // (Procedure tag), args are NB-encoded i64s.
        //
        // The emitted call boxes args into a stack-allocated array
        // + passes a pointer + len. arity validation happens inside
        // vm_call_aot_procedure.
        (Inst::Call(dst, callee, args), EmitMode::Nb)
        | (Inst::CallGeneral(dst, callee, args), EmitMode::Nb) => {
            // RC3 iter 2.4 follow-up: Call and CallGeneral lower
            // identically in AOT. The JIT distinguishes them for
            // the IC fast-path-vs-slow-path split; AOT doesn't have
            // ICs so both take the same `vm_call_aot_procedure`
            // dispatch.
            check(*callee)?;
            for arg in args {
                check(*arg)?;
            }
            let args_csv = args
                .iter()
                .map(|a| format!("v{}", a.0))
                .collect::<Vec<_>>()
                .join(", ");
            let n = args.len();
            (
                *dst,
                format!(
                    "{{ let __aot_args: [i64; {n}] = [{args_csv}]; \
                     unsafe {{ cs_vm::vm::vm_call_aot_procedure(v{}, __aot_args.as_ptr(), {n}) }} }}",
                    callee.0
                ),
            )
        }

        // RC3 iter 2.16 — ArithShift: positive count is left shift,
        // negative is arithmetic right shift. NB mode unpacks /
        // re-encodes; RawI64 passes through to vm_arith_shift_fx
        // which takes raw i64s.
        (Inst::ArithShift(dst, n, c), mode) => {
            check(*n)?;
            check(*c)?;
            let expr = match mode {
                EmitMode::RawI64 => format!(
                    "unsafe {{ cs_vm::vm::vm_arith_shift_fx(v{}, v{}) }}",
                    n.0, c.0
                ),
                EmitMode::Nb => format!(
                    "cs_vm::vm::NanboxValue::fixnum(unsafe {{ cs_vm::vm::vm_arith_shift_fx(\
                     cs_vm::vm::NanboxValue(v{}).as_fixnum().unwrap_or(0), \
                     cs_vm::vm::NanboxValue(v{}).as_fixnum().unwrap_or(0)) }}).into_raw()",
                    n.0, c.0
                ),
            };
            (*dst, expr)
        }

        // ---- Unsupported ----
        (other, _) => return Err(AotError::UnsupportedInst(inst_variant_name(other))),
    })
}

/// Emit the terminator for a block in the loop+match shape.
fn emit_terminator(
    out: &mut String,
    term: &Term,
    func: &Function,
    mode: EmitMode,
) -> Result<(), AotError> {
    match term {
        Term::Return(v) => {
            writeln!(out, "                return v{};", v.0).unwrap();
        }
        Term::Jump(target, args) => {
            // Assign target block's params from `args` before
            // jumping. RIR contract: args.len() == target.params.len().
            let target_block = func
                .blocks
                .iter()
                .find(|b| b.id == *target)
                .ok_or(AotError::UnsupportedTerm("Jump to unknown block"))?;
            if args.len() != target_block.params.len() {
                // Malformed RIR: arg/param count mismatch.
                return Err(AotError::UnsupportedTerm(
                    "Jump arity mismatch with target block params",
                ));
            }
            for (arg_v, (param_v, _ty)) in args.iter().zip(target_block.params.iter()) {
                writeln!(out, "                v{} = v{};", param_v.0, arg_v.0).unwrap();
            }
            writeln!(out, "                block = {};", target.0).unwrap();
            writeln!(out, "                continue;").unwrap();
        }
        Term::Branch(cond, then_target, else_target, args) => {
            let truthy_pred = match mode {
                EmitMode::RawI64 => format!("v{} != 0", cond.0),
                EmitMode::Nb => format!("v{} != {}", cond.0, nb_false_literal()),
            };
            // RC3 iter 2.13 — emit per-target block-arg assignments
            // before jumping. Same shape as Term::Jump: each arg
            // copies into the target's params. Both then- and else-
            // targets receive the same args (both successors of a
            // brif inherit the predecessor's stack).
            let emit_args_for = |out: &mut String, target: BlockId| {
                if args.is_empty() {
                    return;
                }
                let target_block = match func.blocks.iter().find(|b| b.id == target) {
                    Some(b) => b,
                    None => return,
                };
                if args.len() != target_block.params.len() {
                    return;
                }
                for (arg_v, (param_v, _ty)) in args.iter().zip(target_block.params.iter()) {
                    writeln!(out, "                    v{} = v{};", param_v.0, arg_v.0).unwrap();
                }
            };
            writeln!(out, "                if {truthy_pred} {{").unwrap();
            emit_args_for(out, *then_target);
            writeln!(out, "                    block = {};", then_target.0).unwrap();
            writeln!(out, "                    continue;").unwrap();
            writeln!(out, "                }} else {{").unwrap();
            emit_args_for(out, *else_target);
            writeln!(out, "                    block = {};", else_target.0).unwrap();
            writeln!(out, "                    continue;").unwrap();
            writeln!(out, "                }}").unwrap();
        }
    }
    Ok(())
}

/// Emit a Flonum binary op as a Rust expression.
///
/// RC3 iter 2.17 — operand-shape NB-aware. Previous version
/// assumed both operands are RAW f64 bit patterns (NB Flonum's
/// representation). When mixed-type arith feeds a Flonum op an
/// NB Fixnum (e.g., `(+ (* x x) (* 1.5 1.5))` where x = NB Fix(2)),
/// `f64::from_bits(NB_Fixnum_bits)` interprets the tagged-NaN
/// range as a NaN, all subsequent f64 ops propagate NaN, and the
/// result is garbage. Mandelbrot hit this in
/// `(> (+ (* zr zr) (* zi zi)) 4.0)` where zr/zi were typed Any
/// but flowed as NB Fixnums initially (because loop's params
/// defaulted to Any, losing the Flonum specialization at the call
/// site).
///
/// Fix: extract a f64 from each operand. If NB Fixnum, convert
/// payload to f64; if NB Flonum (or RawI64 mode with raw f64
/// bits), use bit pattern directly. NanboxValue::as_fixnum
/// handles the discrimination.
/// Pick the right `nb_<op>_inline` helper for a generic NB
/// arithmetic op. When the per-Value type map proves BOTH
/// operands are Fixnum-typed, route to the `nb_<op>_fixnum_inline`
/// fast path which skips the `nb_both_fixnum` runtime branch.
/// Otherwise use the defensive `nb_<op>_inline` that re-validates
/// at runtime (correct for any combination of types).
///
/// `op` is one of "add", "sub", "mul", "lt", "eq" — matches the
/// helper-name suffix in the AOT prologue (`NB_HELPERS_SOURCE`).
fn nb_arith_call(
    op: &str,
    lhs: &Value,
    rhs: &Value,
    types: &std::collections::HashMap<Value, Type>,
) -> String {
    let both_fixnum = matches!(types.get(lhs), Some(Type::Fixnum))
        && matches!(types.get(rhs), Some(Type::Fixnum));
    let suffix = if both_fixnum {
        "fixnum_inline"
    } else {
        "inline"
    };
    format!("nb_{op}_{suffix}(v{}, v{})", lhs.0, rhs.0)
}

/// Emit a Rust expression that converts the i64 NB carrier in
/// `v{n}` to f64. When `ty == Some(Type::Flonum)`, skip the NB-
/// Fixnum check and bitcast directly — this is the common path
/// for FlonumAdd/Mul/etc. operands whose type is statically
/// known from the inferred value-types map. When the type is
/// unknown (None) or anything else, fall back to the defensive
/// `as_fixnum() / from_bits` discrimination so a NB-Fixnum
/// operand mistakenly fed where a Flonum was expected still
/// promotes correctly. (RC3 iter 2.17 introduced the defensive
/// form; this commit narrows it to the cases that actually
/// need it.)
fn nb_to_f64_expr(v: &Value, ty: Option<&Type>) -> String {
    match ty {
        Some(Type::Flonum) => format!("f64::from_bits(v{} as u64)", v.0),
        _ => format!(
            "(if let Some(__nb_n) = cs_vm::vm::NanboxValue(v{v}).as_fixnum() {{ \
             __nb_n as f64 }} else {{ f64::from_bits(v{v} as u64) }})",
            v = v.0
        ),
    }
}

fn fbinop_rust(
    op: &str,
    lhs: &Value,
    rhs: &Value,
    types: &std::collections::HashMap<Value, Type>,
) -> String {
    format!(
        "({lhs_f} {op} {rhs_f}).to_bits() as i64",
        lhs_f = nb_to_f64_expr(lhs, types.get(lhs)),
        rhs_f = nb_to_f64_expr(rhs, types.get(rhs)),
        op = op,
    )
}

/// RawI64-mode Flonum comparison: result is 0/1 i64.
/// RC3 iter 2.17 — see fbinop_rust for NB-Fixnum handling rationale.
fn fcmp_raw_rust(
    op: &str,
    lhs: &Value,
    rhs: &Value,
    types: &std::collections::HashMap<Value, Type>,
) -> String {
    format!(
        "if {lhs_f} {op} {rhs_f} {{ 1 }} else {{ 0 }}",
        lhs_f = nb_to_f64_expr(lhs, types.get(lhs)),
        rhs_f = nb_to_f64_expr(rhs, types.get(rhs)),
        op = op,
    )
}

/// Nb-mode Flonum comparison: result is an NB Boolean encoded by
/// OR'ing the (0|1) compare with `NB_FALSE_BITS` — so 0→NB_FALSE,
/// 1→NB_TRUE. Mirrors the JIT's FlonumLt lowering shape.
/// RC3 iter 2.17 — NB-Fixnum-aware operand conversion.
fn fcmp_nb_rust(
    op: &str,
    lhs: &Value,
    rhs: &Value,
    types: &std::collections::HashMap<Value, Type>,
) -> String {
    format!(
        "((if {lhs_f} {op} {rhs_f} {{ 1u64 }} else {{ 0u64 }}) | 0xfff8_8000_0000_0000u64) as i64",
        lhs_f = nb_to_f64_expr(lhs, types.get(lhs)),
        rhs_f = nb_to_f64_expr(rhs, types.get(rhs)),
        op = op,
    )
}

/// Flonum unary method call: `f64::from_bits(v0 as u64).METHOD()
/// .to_bits() as i64`. Used for sqrt/abs/floor/ceil/trunc/round/sin/
/// cos/tan/ln/exp/asin/acos/atan.
/// RC3 iter 2.17 — NB-Fixnum-aware via NanboxValue::as_fixnum.
fn funary_rust_method(
    method: &str,
    src: &Value,
    types: &std::collections::HashMap<Value, Type>,
) -> String {
    match types.get(src) {
        Some(Type::Flonum) => format!(
            "f64::from_bits(v{s} as u64).{method}().to_bits() as i64",
            s = src.0,
            method = method,
        ),
        _ => format!(
            "(if let Some(__nb_n) = cs_vm::vm::NanboxValue(v{s}).as_fixnum() {{ \
             (__nb_n as f64).{method}() }} else {{ f64::from_bits(v{s} as u64).{method}() }}).to_bits() as i64",
            s = src.0,
            method = method,
        ),
    }
}

/// Flonum binary method call: `lhs.METHOD(rhs)` shape. Used for
/// max/min/log (base)/atan2/powf.
/// RC3 iter 2.17 — NB-Fixnum-aware operand conversion.
fn fbinop_method_rust(
    method: &str,
    lhs: &Value,
    rhs: &Value,
    types: &std::collections::HashMap<Value, Type>,
) -> String {
    let to_f64 = |v: &Value| -> String { nb_to_f64_expr(v, types.get(v)) };
    format!(
        "{lhs_f}.{method}({rhs_f}).to_bits() as i64",
        lhs_f = to_f64(lhs),
        rhs_f = to_f64(rhs),
        method = method,
    )
}

/// RawI64-mode Flonum predicate (`is_nan` / `is_infinite`): result
/// is 0/1 i64.
fn fpredicate_raw_rust(method: &str, src: &Value) -> String {
    format!(
        "if f64::from_bits(v{s} as u64).{method}() {{ 1 }} else {{ 0 }}",
        s = src.0,
        method = method,
    )
}

/// Nb-mode Flonum predicate: NB Boolean via OR-with-NB_FALSE.
fn fpredicate_nb_rust(method: &str, src: &Value) -> String {
    format!(
        "((f64::from_bits(v{s} as u64).{method}() as u64) | 0xfff8_8000_0000_0000u64) as i64",
        s = src.0,
        method = method,
    )
}

/// Type-predicate runtime-helper call (RC2 iter L).
/// `helper_name` is one of the `vm_*_p_gc` functions in cs_vm::vm,
/// each returning 0/1 i64 for the predicate result. RawI64 mode
/// passes through; Nb mode encodes as NB Boolean via the same OR-
/// with-NB_FALSE trick used for Lt/Eq results.
fn tpred_rust(helper_name: &str, src: &Value, mode: EmitMode) -> String {
    let call = format!("unsafe {{ cs_vm::vm::{helper_name}(v{}) }}", src.0);
    match mode {
        EmitMode::RawI64 => call,
        EmitMode::Nb => format!("(({call} as u64) | 0xfff8_8000_0000_0000u64) as i64"),
    }
}

/// The NB-encoded `#f` literal as a Rust source expression. Used by
/// NB-mode Branch terminators for the truthiness test.
fn nb_false_literal() -> &'static str {
    // NB_SIGNATURE_BITS | (NB_TAG_BOOLEAN << NB_TAG_SHIFT) | 0
    //   = 0xFFF8_0000_0000_0000 | (1 << 47)
    //   = 0xFFF8_8000_0000_0000
    "0xfff8_8000_0000_0000u64 as i64"
}

/// Return the destination Value an Inst writes, if any. Used for
/// pre-declaring all non-param Values in the loop+match shape.
fn inst_dst(inst: &Inst) -> Option<Value> {
    match inst {
        Inst::LoadConst(v, _) => Some(*v),
        Inst::Add(v, _, _) | Inst::Sub(v, _, _) | Inst::Mul(v, _, _) | Inst::Div(v, _, _) => {
            Some(*v)
        }
        Inst::Lt(v, _, _) | Inst::Eq(v, _, _) => Some(*v),
        Inst::Move(v, _) => Some(*v),
        Inst::CallSelf(v, _) => Some(*v),
        // RC3 iter 2.2 Steps 3-4 — MakeClosure + general Call.
        Inst::MakeClosure(v, _) => Some(*v),
        Inst::Call(v, _, _) => Some(*v),
        Inst::CallGeneral(v, _, _) => Some(*v),
        // RC3 iter 2.4 — EnvLookup post-demote (captures).
        Inst::EnvLookup(v, _) | Inst::EnvLookupAny(v, _) => Some(*v),
        // RC2 iter C — Flonum arith/cmp Insts.
        Inst::FlonumAdd(v, _, _)
        | Inst::FlonumSub(v, _, _)
        | Inst::FlonumMul(v, _, _)
        | Inst::FlonumDiv(v, _, _) => Some(*v),
        Inst::FlonumLt(v, _, _) | Inst::FlonumEq(v, _, _) => Some(*v),
        // RC2 iter J — identity-in-NB ops (BoxTyped, AnyTo*, FixToFlo,
        // IntCharBitcast). All emit to a Move-style alias; their dst
        // must be pre-declared for loop+match emission.
        Inst::BoxTyped(v, _, _) => Some(*v),
        Inst::AnyToFix(v, _)
        | Inst::AnyToBool(v, _)
        | Inst::AnyToFlo(v, _)
        | Inst::AnyTruthy(v, _)
        | Inst::FixToFlo(v, _)
        | Inst::IntCharBitcast(v, _) => Some(*v),
        // RC3 iter 2.14 — boolean negation.
        Inst::NotBoolean(v, _) => Some(*v),
        // RC2 iter L — type predicates.
        Inst::PairP(v, _)
        | Inst::NullP(v, _)
        | Inst::VecP(v, _)
        | Inst::ProcedureP(v, _)
        | Inst::SymbolP(v, _)
        | Inst::FixnumP(v, _)
        | Inst::FlonumP(v, _) => Some(*v),
        // RC2 iter M — vector primitives.
        Inst::VecAlloc(v, _, _)
        | Inst::VecRef(v, _, _)
        | Inst::VecSet(v, _, _, _)
        | Inst::VecLength(v, _) => Some(*v),
        // RC2 iter N — pair primitives.
        Inst::Cons(v, _, _, _, _) | Inst::Car(v, _) | Inst::Cdr(v, _) => Some(*v),
        // RC3 iter 2.15 — list length.
        Inst::Length(v, _) => Some(*v),
        // RC3 iter 2.16 — arithmetic shift.
        Inst::ArithShift(v, _, _) => Some(*v),
        // RC2 iter S — equality predicates on Any values.
        Inst::EqAny(v, _, _) | Inst::EqualAny(v, _, _) => Some(*v),
        // RC2 iter T — Any-handle refcount clone.
        Inst::AnyClone(v, _) => Some(*v),
        // RC2 iter D — Flonum unary / binary / predicate Insts.
        Inst::FlonumSqrt(v, _)
        | Inst::FlonumAbs(v, _)
        | Inst::FlonumFloor(v, _)
        | Inst::FlonumCeil(v, _)
        | Inst::FlonumTrunc(v, _)
        | Inst::FlonumRound(v, _)
        | Inst::FlonumSin(v, _)
        | Inst::FlonumCos(v, _)
        | Inst::FlonumTan(v, _)
        | Inst::FlonumLog(v, _)
        | Inst::FlonumExp(v, _)
        | Inst::FlonumAsin(v, _)
        | Inst::FlonumAcos(v, _)
        | Inst::FlonumAtan(v, _) => Some(*v),
        Inst::FlonumMax(v, _, _) | Inst::FlonumMin(v, _, _) => Some(*v),
        Inst::FlonumLog2(v, _, _) | Inst::FlonumAtan2(v, _, _) | Inst::FlonumExpt(v, _, _) => {
            Some(*v)
        }
        Inst::FlonumIsNan(v, _) | Inst::FlonumIsInfinite(v, _) => Some(*v),
        // Any inst not in the supported set: returning None is fine
        // because the emitter rejects unsupported variants before
        // pre-declaration anyway. Keeps this helper lean.
        _ => None,
    }
}

fn term_variant_name(term: &Term) -> &'static str {
    match term {
        Term::Return(_) => "Return",
        Term::Jump(_, _) => "Jump",
        Term::Branch(_, _, _, _) => "Branch",
    }
}

/// Sanitize a Scheme procedure name into a valid Rust identifier.
/// Replaces non-alphanumeric characters with underscores; prepends
/// `proc_` if the result would start with a digit. Empty names
/// become `proc_anon`.
fn sanitize_ident(name: &str) -> String {
    if name.is_empty() {
        return "proc_anon".to_string();
    }
    let mut s = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
        } else {
            s.push('_');
        }
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s = format!("proc_{s}");
    }
    // RC3 iter 2.9 — guard against Scheme names that happen to be
    // Rust keywords (`loop`, `if`, `match`, `fn`, `let`, etc.) by
    // prefixing with `proc_` if the candidate ident is reserved.
    // The Scheme→Rust ident remapping is sticky for downstream
    // resolver lookups since by_name_sym is keyed by sym not name.
    const RUST_KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "do", "dyn", "else", "enum",
        "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro",
        "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
        "super", "trait", "true", "try", "type", "union", "unsafe", "unsized", "use", "virtual",
        "where", "while", "yield",
    ];
    if RUST_KEYWORDS.contains(&s.as_str()) {
        s = format!("proc_{s}");
    }
    s
}

fn const_to_rust_i64(c: &Const) -> Result<String, AotError> {
    match c {
        Const::Fixnum(n) => {
            // i64::MIN renders as e.g. `-9223372036854775808i64`;
            // Rust parses negative literals as a unary `-` over the
            // positive literal, which doesn't work for i64::MIN.
            // Round-trip via the `i64` suffix and bit pattern when
            // MIN is involved.
            if *n == i64::MIN {
                Ok("i64::MIN".to_string())
            } else {
                Ok(format!("{n}i64"))
            }
        }
        Const::Boolean(_) => Err(AotError::UnsupportedConst("Boolean")),
        Const::Flonum(_) => Err(AotError::UnsupportedConst("Flonum")),
        Const::Character(_) => Err(AotError::UnsupportedConst("Character")),
        Const::Null => Err(AotError::UnsupportedConst("Null")),
        Const::Unspecified => Err(AotError::UnsupportedConst("Unspecified")),
        Const::Eof => Err(AotError::UnsupportedConst("Eof")),
        Const::Symbol(_) => Err(AotError::UnsupportedConst("Symbol")),
        Const::StringRef(_) => Err(AotError::UnsupportedConst("StringRef")),
    }
}

/// Compute the NB-encoded i64 literal for a Const at emit time.
/// The bit pattern is computed using the same formulas as
/// `cs_vm::vm::NanboxValue::{fixnum, boolean, ...}`, so emitted
/// code carries the literal bits directly instead of calling into
/// the runtime for the encoding.
///
/// This keeps the emitter standalone (no cs-vm at emit time) and
/// lets `rustc -O` constant-fold the literal at compile time.
fn const_to_rust_nb(c: &Const) -> Result<String, AotError> {
    // NB layout (mirroring cs_vm::vm::NB_*):
    //   bits 63..51:  signature 0xFFF8 (12 bits set + sign bit)
    //   bits 50..47:  tag (4 bits)
    //   bits 46..0:   payload (47 bits)
    const NB_SIGNATURE_BITS: u64 = 0xFFF8_0000_0000_0000;
    const NB_TAG_SHIFT: u32 = 47;
    const NB_PAYLOAD_MASK: u64 = (1u64 << 47) - 1;
    // Tag values from cs_vm::vm.
    const NB_TAG_FIXNUM: u64 = 0;
    const NB_TAG_BOOLEAN: u64 = 1;
    const NB_TAG_CHARACTER: u64 = 2;
    const NB_TAG_SYMBOL: u64 = 3;
    const NB_TAG_NULL: u64 = 4;
    const NB_TAG_UNSPECIFIED: u64 = 5;
    const NB_TAG_EOF: u64 = 6;
    let nb_make = |tag: u64, payload: u64| -> u64 {
        NB_SIGNATURE_BITS | ((tag & 0xF) << NB_TAG_SHIFT) | (payload & NB_PAYLOAD_MASK)
    };
    let bits: u64 = match c {
        Const::Fixnum(n) => nb_make(NB_TAG_FIXNUM, (*n as u64) & NB_PAYLOAD_MASK),
        Const::Boolean(false) => nb_make(NB_TAG_BOOLEAN, 0),
        Const::Boolean(true) => nb_make(NB_TAG_BOOLEAN, 1),
        Const::Character(c) => nb_make(NB_TAG_CHARACTER, *c as u64),
        Const::Null => nb_make(NB_TAG_NULL, 0),
        Const::Unspecified => nb_make(NB_TAG_UNSPECIFIED, 0),
        Const::Eof => nb_make(NB_TAG_EOF, 0),
        Const::Flonum(f) => f.to_bits(), // NB Flonum = raw f64 bits
        // RC3 iter 2.16 — Symbol payload is the cs_core::Symbol u32.
        // The runtime symbol table stays in cs-vm; AOT'd programs
        // that compare symbols (e.g., spectral-norm's `'av` / `'au`
        // case-marker sentinel) just need the sym id to round-trip.
        Const::Symbol(s) => nb_make(NB_TAG_SYMBOL, *s as u64),
        Const::StringRef(_) => return Err(AotError::UnsupportedConst("StringRef")),
    };
    // Render as a hex literal with the i64 suffix. Using hex makes
    // the high bits readable (NB carriers all start with 0xFFF8...).
    Ok(format!("0x{:016x}u64 as i64", bits))
}

fn require_defined(defined: &HashSet<Value>, v: Value) -> Result<(), AotError> {
    if defined.contains(&v) {
        Ok(())
    } else {
        Err(AotError::UndefinedValue(v))
    }
}

fn inst_variant_name(inst: &Inst) -> &'static str {
    // Iter-1 only needs names for the unsupported variants the
    // analyzer might reach. We list the common shapes; anything
    // else falls through to "<other>" — diagnostic-only, not a
    // correctness path.
    match inst {
        Inst::LoadConst(..) => "LoadConst",
        Inst::Add(..) => "Add",
        Inst::Sub(..) => "Sub",
        Inst::Mul(..) => "Mul",
        Inst::Div(..) => "Div",
        Inst::FlonumAdd(..) => "FlonumAdd",
        Inst::FlonumSub(..) => "FlonumSub",
        Inst::FlonumMul(..) => "FlonumMul",
        Inst::FlonumDiv(..) => "FlonumDiv",
        Inst::Lt(..) => "Lt",
        Inst::Eq(..) => "Eq",
        Inst::Call(..) => "Call",
        Inst::CallSelf(..) => "CallSelf",
        Inst::CallGeneral(..) => "CallGeneral",
        Inst::EnvLookup(..) => "EnvLookup",
        Inst::EnvLookupAny(..) => "EnvLookupAny",
        Inst::EnvSet(..) => "EnvSet",
        Inst::EnvDefineLocal(..) => "EnvDefineLocal",
        Inst::MakeClosure(..) => "MakeClosure",
        Inst::VecAlloc(..) => "VecAlloc",
        Inst::VecRef(..) => "VecRef",
        Inst::VecSet(..) => "VecSet",
        Inst::BoxTyped(..) => "BoxTyped",
        _ => "<other>",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_rir::{Block, BlockId, Const, Inst, Term, Type, Value};

    /// Helper: a single-block function `(define (sq x) (* x x))`.
    fn sq_function() -> Function {
        let mut f = Function::new("sq");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Mul(Value(1), Value(0), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        f
    }

    /// Helper: `(define (add3 a b c) (+ (+ a b) c))`.
    fn add3_function() -> Function {
        let mut f = Function::new("add3");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.params.push((Value(2), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Add(Value(3), Value(0), Value(1)),
                Inst::Add(Value(4), Value(3), Value(2)),
            ],
            terminator: Term::Return(Value(4)),
        });
        f
    }

    #[test]
    fn emits_sq() {
        let src = emit(&sq_function()).unwrap();
        // RC3 iter 2.12 — every fn gets `__self_handle: i64` first.
        assert!(src.contains("pub extern \"C\" fn sq(__self_handle: i64, mut v0: i64) -> i64"));
        assert!(src.contains("let v1: i64 = v0.wrapping_mul(v0);"));
        assert!(src.contains("    v1\n"));
    }

    #[test]
    fn emits_add3() {
        let src = emit(&add3_function()).unwrap();
        assert!(src.contains(
            "pub extern \"C\" fn add3(__self_handle: i64, mut v0: i64, mut v1: i64, mut v2: i64) -> i64"
        ));
        assert!(src.contains("let v3: i64 = v0.wrapping_add(v1);"));
        assert!(src.contains("let v4: i64 = v3.wrapping_add(v2);"));
        assert!(src.contains("    v4\n"));
    }

    #[test]
    fn emits_loadconst() {
        let mut f = Function::new("answer");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(42))],
            terminator: Term::Return(Value(0)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("pub extern \"C\" fn answer(__self_handle: i64) -> i64"));
        assert!(src.contains("let v0: i64 = 42i64;"));
    }

    #[test]
    fn loadconst_i64_min_uses_const_to_avoid_unary_neg() {
        // `-9223372036854775808i64` is a literal Rust rejects (the
        // negation overflows i64). Emit via `i64::MIN` instead.
        let mut f = Function::new("min_val");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(i64::MIN))],
            terminator: Term::Return(Value(0)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("let v0: i64 = i64::MIN;"));
    }

    #[test]
    fn emitted_source_parses_as_rust() {
        // The most important non-runtime correctness check: every
        // emitter output must be valid Rust. We parse via `syn`;
        // anything malformed gets caught here before users hit a
        // confusing rustc error inside their AOT'd binary.
        for src in [
            emit(&sq_function()).unwrap(),
            emit(&add3_function()).unwrap(),
        ] {
            let parsed = syn::parse_file(&src);
            assert!(
                parsed.is_ok(),
                "emitted source failed to parse as Rust:\n--- begin ---\n{src}--- end ---\nerror: {:?}",
                parsed.err()
            );
        }
    }

    #[test]
    fn rejects_empty_function() {
        let f = Function::new("empty");
        assert_eq!(emit(&f), Err(AotError::EmptyFunction));
    }

    #[test]
    fn multi_block_jump_emits_loop_match() {
        // Two-block trivial: entry jumps to block 1 which returns 7.
        // Exercises Jump(no args) + the loop+match shape.
        let mut f = Function::new("jump_to_return");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(7))],
            terminator: Term::Jump(BlockId(1), vec![Value(0)]),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![(Value(1), Type::Fixnum)],
            insts: vec![],
            terminator: Term::Return(Value(1)),
        });
        let src = emit(&f).unwrap();
        // Multi-block always uses loop+match shape.
        assert!(src.contains("let mut v0: i64 = 0;"));
        assert!(src.contains("let mut v1: i64 = 0;"));
        assert!(src.contains("let mut block: u32 = 0;"));
        assert!(src.contains("loop {"));
        assert!(src.contains("match block {"));
        assert!(src.contains("0 => {"));
        assert!(src.contains("v0 = 7i64;"));
        // Jump assigns block param then dispatches.
        assert!(src.contains("v1 = v0;"));
        assert!(src.contains("block = 1;"));
        assert!(src.contains("continue;"));
        assert!(src.contains("1 => {"));
        assert!(src.contains("return v1;"));
        assert!(src.contains("_ => unreachable!(),"));
    }

    #[test]
    fn tail_callself_rewrites_to_param_rebind_continue() {
        // A loop-shaped recursion: block 0 either Returns x,
        // or CallSelfs with (x-1) and Returns the result.
        // The CallSelf-then-Return pattern is exactly what
        // `detect_tail_call` recognizes.
        //
        //   (define (countdown x)
        //     (if (= x 0) x (countdown (- x 1))))
        //
        // RIR:
        //   block 0: v1=0; v2=v0==v1; Branch(v2, 1, 2)
        //   block 1: Return v0
        //   block 2: v3=1; v4=v0-v3; v5=CallSelf(v4); Return v5
        let mut f = Function::new("countdown");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(1), Const::Fixnum(0)),
                Inst::Eq(Value(2), Value(0), Value(1)),
            ],
            terminator: Term::Branch(Value(2), BlockId(1), BlockId(2), vec![]),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(Value(0)),
        });
        f.blocks.push(Block {
            id: BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(3), Const::Fixnum(1)),
                Inst::Sub(Value(4), Value(0), Value(3)),
                Inst::CallSelf(Value(5), vec![Value(4)]),
            ],
            terminator: Term::Return(Value(5)),
        });
        let src = emit(&f).unwrap();
        // Param decl is `mut` (TCO reassigns it).
        assert!(
            src.contains("mut v0: i64"),
            "param should be mut for TCO; got:\n{src}"
        );
        // Tail call rewritten to rebind + continue — NO
        // direct recursive call inside the body. The header
        // `fn countdown(...)` always contains `countdown(`,
        // so check for the CALL shape `= countdown(` (used
        // by emit_inst_let / emit_inst_assign for non-TCO
        // CallSelf).
        assert!(
            !src.contains("= countdown(__self_handle"),
            "tail CallSelf should NOT emit a recursive call; got:\n{src}"
        );
        assert!(
            src.contains("v0 = v4;"),
            "tail call should rebind v0 to the new arg v4; got:\n{src}"
        );
        assert!(
            src.contains("block = 0;"),
            "tail call should continue to entry block; got:\n{src}"
        );
    }

    #[test]
    fn non_tail_callself_keeps_recursive_call() {
        // A non-tail recursive shape: the call result is
        // ADDed before being returned. Detect_tail_call
        // should NOT fire — the recursive call has to
        // happen as a real function call so the post-call
        // arithmetic runs.
        //
        //   (define (f x)
        //     (if (= x 0) 1 (+ x (f (- x 1)))))
        //
        // RIR block 2: v3=1; v4=v0-v3; v5=CallSelf(v4);
        //              v6=v0+v5; Return v6
        let mut f = Function::new("non_tail");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(1), Const::Fixnum(0)),
                Inst::Eq(Value(2), Value(0), Value(1)),
            ],
            terminator: Term::Branch(Value(2), BlockId(1), BlockId(2), vec![]),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(7), Const::Fixnum(1))],
            terminator: Term::Return(Value(7)),
        });
        f.blocks.push(Block {
            id: BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(3), Const::Fixnum(1)),
                Inst::Sub(Value(4), Value(0), Value(3)),
                Inst::CallSelf(Value(5), vec![Value(4)]),
                Inst::Add(Value(6), Value(0), Value(5)),
            ],
            terminator: Term::Return(Value(6)),
        });
        let src = emit(&f).unwrap();
        // CallSelf is NOT the last inst — Add comes after.
        // TCO doesn't fire; recursive call stays.
        assert!(
            src.contains("= non_tail(__self_handle"),
            "non-tail CallSelf should emit a recursive call; got:\n{src}"
        );
    }

    #[test]
    fn branch_emits_if_else_dispatch() {
        // (define (abs-ish x) (if (< x 0) (- 0 x) x))
        // Translated to RIR with Branch:
        //   block 0: v1 = 0; v2 = v0 < v1; Branch(v2, 1, 2)
        //   block 1: v3 = 0; v4 = v3 - v0; Return v4
        //   block 2: Return v0
        let mut f = Function::new("abs_ish");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(1), Const::Fixnum(0)),
                Inst::Lt(Value(2), Value(0), Value(1)),
            ],
            terminator: Term::Branch(Value(2), BlockId(1), BlockId(2), Vec::new()),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(3), Const::Fixnum(0)),
                Inst::Sub(Value(4), Value(3), Value(0)),
            ],
            terminator: Term::Return(Value(4)),
        });
        f.blocks.push(Block {
            id: BlockId(2),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(Value(0)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("v2 = if v0 < v1 { 1 } else { 0 };"));
        assert!(src.contains("if v2 != 0 {"));
        assert!(src.contains("block = 1;"));
        assert!(src.contains("block = 2;"));
    }

    #[test]
    fn lt_and_eq_emit_zero_one() {
        let mut f = Function::new("cmp");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Lt(Value(2), Value(0), Value(1)),
                Inst::Eq(Value(3), Value(0), Value(1)),
                Inst::Add(Value(4), Value(2), Value(3)),
            ],
            terminator: Term::Return(Value(4)),
        });
        let src = emit(&f).unwrap();
        // Single-block + Return uses straight-line shape.
        assert!(src.contains("let v2: i64 = if v0 < v1 { 1 } else { 0 };"));
        assert!(src.contains("let v3: i64 = if v0 == v1 { 1 } else { 0 };"));
    }

    #[test]
    fn move_emits_alias() {
        let mut f = Function::new("alias");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Move(Value(1), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("let v1: i64 = v0;"));
    }

    #[test]
    fn div_raw_emits_wrapping_div() {
        // RawI64 mode: Div compiles to `wrapping_div` (integer
        // division with Rust's usual panic-on-zero / overflow
        // semantics). Different shape from Add/Sub/Mul because
        // there's no inline fast path possible in NB mode either
        // (Fixnum/Fixnum can produce Rational).
        let mut f = Function::new("d");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Div(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        let src = emit(&f).unwrap();
        assert!(src.contains("let v2: i64 = v0.wrapping_div(v1);"));
    }

    #[test]
    fn div_nb_emits_runtime_helper() {
        // Nb mode: Div always slow-pathed to vm_value_div_nb (per
        // cs-rir's Div doc — Fixnum/Fixnum can produce a Rational
        // which doesn't fit the NB Fixnum lane).
        let mut f = Function::new("d_nb");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Div(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        assert!(
            src.contains("vm_value_div_nb"),
            "Div in Nb mode should call vm_value_div_nb: {src}"
        );
    }

    #[test]
    fn flonum_arith_emits_bitcast_pattern() {
        // FlonumAdd/Sub/Mul/Div lower identically in both modes:
        // bitcast i64→f64, apply op, bitcast back via to_bits.
        // Matches the JIT's fbinop helper in cs-jit-cranelift.
        let mut f = Function::new("fadd");
        f.params.push((Value(0), Type::Flonum));
        f.params.push((Value(1), Type::Flonum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::FlonumAdd(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        f.return_type = Type::Flonum;
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        // The exact f64::from_bits / .to_bits as i64 shape is the
        // contract; downstream consumers depend on this being
        // syntactically a pure expression (no helper call).
        assert!(src.contains("f64::from_bits(v0 as u64)"));
        assert!(src.contains("f64::from_bits(v1 as u64)"));
        assert!(src.contains(".to_bits() as i64"));
        assert!(src.contains(" + "));
    }

    #[test]
    fn flonum_unary_emits_method_call() {
        // RC2 iter D: FlonumSqrt → `f64::from_bits(...).sqrt().to_bits()
        // as i64`. Same template for all the unary f64 methods.
        let mut f = Function::new("rt");
        f.params.push((Value(0), Type::Flonum));
        f.return_type = Type::Flonum;
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::FlonumSqrt(Value(1), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        // Post-typer-hints (Phase-5+++): when the operand's
        // type is statically known Flonum (here from the
        // param-type hint), `infer_value_types` records it and
        // the codegen skips the defensive `as_fixnum()`
        // discrimination — direct `f64::from_bits` instead.
        // The post-decode `.sqrt().to_bits() as i64` shape
        // stays the same.
        assert!(src.contains(".sqrt()"));
        assert!(src.contains(".to_bits() as i64"));
        assert!(src.contains("f64::from_bits(v0 as u64)"));
        assert!(
            !src.contains("NanboxValue(v0).as_fixnum()"),
            "param-typed Flonum should skip the as_fixnum check; got:\n{src}"
        );
    }

    #[test]
    fn flonum_binary_method_emits_method_call_chain() {
        // FlonumExpt(dst, n, base) → n.powf(base) per cs-rir docs +
        // JIT lowering.
        let mut f = Function::new("p");
        f.params.push((Value(0), Type::Flonum));
        f.params.push((Value(1), Type::Flonum));
        f.return_type = Type::Flonum;
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::FlonumExpt(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        // Same Phase-5+++ refinement: both operands are
        // param-typed Flonum, so the as_fixnum check is gone.
        assert!(src.contains(".powf("));
        assert!(src.contains("f64::from_bits(v0 as u64)"));
        assert!(src.contains("f64::from_bits(v1 as u64)"));
        assert!(!src.contains("NanboxValue(v0).as_fixnum()"));
        assert!(!src.contains("NanboxValue(v1).as_fixnum()"));
    }

    #[test]
    fn flonum_unknown_type_keeps_defensive_decode() {
        // When the operand's type ISN'T statically known
        // (e.g., a block param without an inferred type),
        // the defensive `as_fixnum() / from_bits` pattern
        // still fires. This preserves correctness for cases
        // where a NB-Fixnum might sneak in.
        //
        // We construct a function whose Flonum op consumes a
        // value defined by a non-Flonum-producing inst —
        // here, AnyClone of a param — so infer_value_types
        // doesn't record a Flonum for it.
        let mut f = Function::new("u");
        f.params.push((Value(0), Type::Any));
        f.return_type = Type::Flonum;
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::FlonumSqrt(Value(1), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        assert!(
            src.contains("NanboxValue(v0).as_fixnum()"),
            "Any-typed operand should keep the defensive decode; got:\n{src}"
        );
    }

    #[test]
    fn flonum_predicate_nb_encodes_as_nb_boolean() {
        // FlonumIsNan in Nb mode uses the same NB-Boolean OR-with-
        // NB_FALSE_BITS encoding as FlonumLt.
        let mut f = Function::new("nan_p");
        f.params.push((Value(0), Type::Flonum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::FlonumIsNan(Value(1), Value(0))],
            terminator: Term::Return(Value(1)),
        });
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        assert!(src.contains("f64::from_bits(v0 as u64).is_nan()"));
        assert!(src.contains("0xfff8_8000_0000_0000u64"));
    }

    #[test]
    fn flonum_cmp_nb_encodes_via_or_with_nb_false() {
        // FlonumLt in Nb mode uses the OR-with-NB_FALSE_BITS trick
        // the JIT uses: compare → {0,1} → | NB_FALSE → NB_BOOLEAN.
        // Different shape from Lt/Eq because those go through the
        // nb_lt_inline / nb_eq_inline helpers (Fixnum-typed fast
        // path); FlonumLt is bit-pattern-bitcast → IEEE-754 cmp.
        let mut f = Function::new("flt");
        f.params.push((Value(0), Type::Flonum));
        f.params.push((Value(1), Type::Flonum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::FlonumLt(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        let src = emit_with(EmitMode::Nb, &f).unwrap();
        assert!(src.contains("0xfff8_8000_0000_0000u64"));
        assert!(src.contains("f64::from_bits(v0 as u64)"));
        assert!(src.contains(" < "));
    }

    #[test]
    fn rejects_unsupported_inst() {
        // `EnvSet` (Set! to a closure-captured or top-level var) isn't
        // yet handled — needs a mutable-binding story for AOT'd code
        // (the demote pass treats syms as immutable). Used here as
        // the canary that the unsupported-Inst error path still
        // fires; the list of supported Insts grew significantly
        // across RC3 Phase 2 (MakeClosure, Call, CallGeneral, and
        // capturing-closure EnvLookup all now lower).
        let mut f = Function::new("envset_unsup");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::EnvSet(42, Value(0))],
            terminator: Term::Return(Value(0)),
        });
        f.return_type = Type::Fixnum;
        // Nb mode accepts any param/return type; that's the easier
        // path to exercise the unsupported-Inst rejection.
        assert_eq!(
            emit_with(EmitMode::Nb, &f),
            Err(AotError::UnsupportedInst("EnvSet"))
        );
    }

    #[test]
    fn rejects_use_before_def() {
        let mut f = Function::new("bad");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Add(Value(0), Value(99), Value(99))],
            terminator: Term::Return(Value(0)),
        });
        assert_eq!(emit(&f), Err(AotError::UndefinedValue(Value(99))));
    }

    #[test]
    fn rejects_non_fixnum_return_type() {
        let mut f = Function::new("flo");
        f.return_type = Type::Flonum;
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(0), Const::Fixnum(0))],
            terminator: Term::Return(Value(0)),
        });
        assert_eq!(emit(&f), Err(AotError::UnsupportedReturnType));
    }

    #[test]
    fn sanitize_handles_scheme_names() {
        assert_eq!(sanitize_ident("foo"), "foo");
        assert_eq!(sanitize_ident("matrix-elt"), "matrix_elt");
        assert_eq!(sanitize_ident("list?"), "list_");
        assert_eq!(sanitize_ident("set!"), "set_");
        assert_eq!(sanitize_ident("string->list"), "string__list");
        assert_eq!(sanitize_ident(""), "proc_anon");
        assert_eq!(sanitize_ident("1plus"), "proc_1plus");
    }
}
