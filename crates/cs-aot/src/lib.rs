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

use cs_rir::{Const, Function, Inst, Term, Type, Value};

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
    write!(out, "pub extern \"C\" fn {fn_name}(").unwrap();
    let mut first_param = true;
    for sym in &func.captures {
        if !first_param {
            out.push_str(", ");
        }
        first_param = false;
        write!(out, "__cap{sym}: i64").unwrap();
    }
    for (v, _ty) in func.params.iter() {
        if !first_param {
            out.push_str(", ");
        }
        first_param = false;
        write!(out, "v{}: i64", v.0).unwrap();
    }
    out.push_str(") -> i64 {\n");

    if straight_line {
        emit_straight_line(&mut out, func, mode, resolver)?;
    } else {
        emit_loop_match(&mut out, func, mode, resolver)?;
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
        for inst in &block.insts {
            emit_inst_assign(
                out,
                inst,
                mode,
                &fn_name,
                resolver,
                &func.captures,
                &local_defs,
            )?;
        }
        emit_terminator(out, &block.terminator, func, mode)?;
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
    let (dst, expr) = inst_rhs(
        inst,
        Some(defined),
        mode,
        self_fn_name,
        resolver,
        captures,
        local_defs,
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
) -> Result<(), AotError> {
    // Same no-op as emit_inst_let for surviving EnvDefineLocal.
    if matches!(inst, Inst::EnvDefineLocal(..)) {
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
            (*dst, format!("nb_add_inline(v{}, v{})", lhs.0, rhs.0))
        }
        (Inst::Sub(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("nb_sub_inline(v{}, v{})", lhs.0, rhs.0))
        }
        (Inst::Mul(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("nb_mul_inline(v{}, v{})", lhs.0, rhs.0))
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
            (*dst, format!("nb_lt_inline(v{}, v{})", lhs.0, rhs.0))
        }
        (Inst::Eq(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, format!("nb_eq_inline(v{}, v{})", lhs.0, rhs.0))
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
        | (Inst::AnyTruthy(dst, src), _)
        | (Inst::FixToFlo(dst, src), _)
        | (Inst::IntCharBitcast(dst, src), _) => {
            check(*src)?;
            (*dst, format!("v{}", src.0))
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
        // VecAlloc: `dst = make-vector(n, fill)` — n is Fixnum, fill
        // is Any (defaulting to Unspecified via the bytecode
        // compiler's desugaring), dst is a vector Gc handle.
        (Inst::VecAlloc(dst, n, fill), _) => {
            check(*n)?;
            check(*fill)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_alloc_vector_gc(v{}, v{}) }}",
                    n.0, fill.0
                ),
            )
        }
        (Inst::VecRef(dst, vec, idx), _) => {
            check(*vec)?;
            check(*idx)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_vector_ref_gc(v{}, v{}) }}",
                    vec.0, idx.0
                ),
            )
        }
        (Inst::VecSet(dst, vec, idx, val), _) => {
            check(*vec)?;
            check(*idx)?;
            check(*val)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_vector_set_gc(v{}, v{}, v{}) }}",
                    vec.0, idx.0, val.0
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
        (Inst::Cons(dst, car_v, car_tag, cdr_v, cdr_tag), _) => {
            check(*car_v)?;
            check(*cdr_v)?;
            (
                *dst,
                format!(
                    "unsafe {{ cs_vm::vm::vm_alloc_pair_gc(v{}, {}u8, v{}, {}u8) }}",
                    car_v.0, car_tag, cdr_v.0, cdr_tag
                ),
            )
        }
        (Inst::Car(dst, pair), _) => {
            check(*pair)?;
            (
                *dst,
                format!("unsafe {{ cs_vm::vm::vm_pair_car_gc(v{}) }}", pair.0),
            )
        }
        (Inst::Cdr(dst, pair), _) => {
            check(*pair)?;
            (
                *dst,
                format!("unsafe {{ cs_vm::vm::vm_pair_cdr_gc(v{}) }}", pair.0),
            )
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
            (*dst, fbinop_rust("+", lhs, rhs))
        }
        (Inst::FlonumSub(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("-", lhs, rhs))
        }
        (Inst::FlonumMul(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("*", lhs, rhs))
        }
        (Inst::FlonumDiv(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_rust("/", lhs, rhs))
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
            (*dst, fcmp_raw_rust("<", lhs, rhs))
        }
        (Inst::FlonumEq(dst, lhs, rhs), EmitMode::RawI64) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_raw_rust("==", lhs, rhs))
        }
        (Inst::FlonumLt(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_nb_rust("<", lhs, rhs))
        }
        (Inst::FlonumEq(dst, lhs, rhs), EmitMode::Nb) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fcmp_nb_rust("==", lhs, rhs))
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
            (*dst, funary_rust_method("sqrt", src))
        }
        (Inst::FlonumAbs(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("abs", src))
        }
        (Inst::FlonumFloor(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("floor", src))
        }
        (Inst::FlonumCeil(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("ceil", src))
        }
        (Inst::FlonumTrunc(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("trunc", src))
        }
        (Inst::FlonumRound(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("round", src))
        }
        (Inst::FlonumSin(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("sin", src))
        }
        (Inst::FlonumCos(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("cos", src))
        }
        (Inst::FlonumTan(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("tan", src))
        }
        (Inst::FlonumLog(dst, src), _) => {
            check(*src)?;
            // Scheme `log` is the natural log → Rust's `f64::ln`.
            (*dst, funary_rust_method("ln", src))
        }
        (Inst::FlonumExp(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("exp", src))
        }
        (Inst::FlonumAsin(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("asin", src))
        }
        (Inst::FlonumAcos(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("acos", src))
        }
        (Inst::FlonumAtan(dst, src), _) => {
            check(*src)?;
            (*dst, funary_rust_method("atan", src))
        }

        // ---- Flonum binary ops (RC2 iter D) ----
        (Inst::FlonumMax(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_method_rust("max", lhs, rhs))
        }
        (Inst::FlonumMin(dst, lhs, rhs), _) => {
            check(*lhs)?;
            check(*rhs)?;
            (*dst, fbinop_method_rust("min", lhs, rhs))
        }
        // Per the JIT lowering, all three are (dst, n, base) shape.
        // Rust stdlib mappings:
        //   FlonumLog2 → n.log(base)  (log base `base` of n)
        //   FlonumAtan2 → n.atan2(base)
        //   FlonumExpt  → n.powf(base)
        (Inst::FlonumLog2(dst, n, base), _) => {
            check(*n)?;
            check(*base)?;
            (*dst, fbinop_method_rust("log", n, base))
        }
        (Inst::FlonumAtan2(dst, n, base), _) => {
            check(*n)?;
            check(*base)?;
            (*dst, fbinop_method_rust("atan2", n, base))
        }
        (Inst::FlonumExpt(dst, n, base), _) => {
            check(*n)?;
            check(*base)?;
            (*dst, fbinop_method_rust("powf", n, base))
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
            // RC3 iter 2.4: CallSelf in a capturing function passes
            // through the same captures (recursive calls are within
            // the same closure, so captures don't change).
            let mut call = String::from(self_fn_name);
            call.push('(');
            let mut first = true;
            for sym in captures {
                if !first {
                    call.push_str(", ");
                }
                first = false;
                call.push_str(&format!("__cap{sym}"));
            }
            for arg in args {
                if !first {
                    call.push_str(", ");
                }
                first = false;
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
                        // Sym is neither in our captures nor locally
                        // defined via EnvDefineLocal nor a known
                        // top-level AOT'd function. The bytecode
                        // likely binds it via a path we don't yet
                        // model (e.g., direct env install from a
                        // letrec frame outside our scan).
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
        Term::Branch(cond, then_target, else_target) => {
            // Truthiness predicate depends on ABI:
            //   - RawI64 mode: Lt/Eq emit `1` for true, `0` for false,
            //     so plain `cond != 0` works.
            //   - Nb mode: Lt/Eq delegate to `vm_value_*_nb` which
            //     return NB-encoded Booleans. NB false has a specific
            //     bit pattern (0xFFF8_8000_0000_0000); EVERY other
            //     value — including NB Fixnum 0 and NB true — is
            //     truthy, matching Scheme's `#f`-is-the-only-false
            //     semantics. We compare against the NB false literal
            //     so `(if 0 a b)` correctly takes the `a` branch.
            let truthy_pred = match mode {
                EmitMode::RawI64 => format!("v{} != 0", cond.0),
                EmitMode::Nb => format!("v{} != {}", cond.0, nb_false_literal()),
            };
            writeln!(out, "                if {truthy_pred} {{").unwrap();
            writeln!(out, "                    block = {};", then_target.0).unwrap();
            writeln!(out, "                    continue;").unwrap();
            writeln!(out, "                }} else {{").unwrap();
            writeln!(out, "                    block = {};", else_target.0).unwrap();
            writeln!(out, "                    continue;").unwrap();
            writeln!(out, "                }}").unwrap();
        }
    }
    Ok(())
}

/// Emit a Flonum binary op as a Rust expression. The two operands
/// are i64 carriers of f64 bit patterns; emit
/// `(f64::from_bits(va as u64) <op> f64::from_bits(vb as u64))
/// .to_bits() as i64`. Identical in both EmitModes.
fn fbinop_rust(op: &str, lhs: &Value, rhs: &Value) -> String {
    format!(
        "(f64::from_bits(v{l} as u64) {op} f64::from_bits(v{r} as u64)).to_bits() as i64",
        l = lhs.0,
        r = rhs.0,
        op = op,
    )
}

/// RawI64-mode Flonum comparison: result is 0/1 i64.
fn fcmp_raw_rust(op: &str, lhs: &Value, rhs: &Value) -> String {
    format!(
        "if f64::from_bits(v{l} as u64) {op} f64::from_bits(v{r} as u64) {{ 1 }} else {{ 0 }}",
        l = lhs.0,
        r = rhs.0,
        op = op,
    )
}

/// Nb-mode Flonum comparison: result is an NB Boolean encoded by
/// OR'ing the (0|1) compare with `NB_FALSE_BITS` — so 0→NB_FALSE,
/// 1→NB_TRUE. Mirrors the JIT's FlonumLt lowering shape.
fn fcmp_nb_rust(op: &str, lhs: &Value, rhs: &Value) -> String {
    format!(
        "((if f64::from_bits(v{l} as u64) {op} f64::from_bits(v{r} as u64) {{ 1u64 }} else {{ 0u64 }}) | 0xfff8_8000_0000_0000u64) as i64",
        l = lhs.0,
        r = rhs.0,
        op = op,
    )
}

/// Flonum unary method call: `f64::from_bits(v0 as u64).METHOD()
/// .to_bits() as i64`. Used for sqrt/abs/floor/ceil/trunc/round/sin/
/// cos/tan/ln/exp/asin/acos/atan.
fn funary_rust_method(method: &str, src: &Value) -> String {
    format!(
        "f64::from_bits(v{s} as u64).{method}().to_bits() as i64",
        s = src.0,
        method = method,
    )
}

/// Flonum binary method call: `lhs.METHOD(rhs)` shape. Used for
/// max/min/log (base)/atan2/powf.
fn fbinop_method_rust(method: &str, lhs: &Value, rhs: &Value) -> String {
    format!(
        "f64::from_bits(v{l} as u64).{method}(f64::from_bits(v{r} as u64)).to_bits() as i64",
        l = lhs.0,
        r = rhs.0,
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
        Term::Branch(_, _, _) => "Branch",
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
        Const::Symbol(_) => return Err(AotError::UnsupportedConst("Symbol")),
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
        // Smoke: contains the expected pieces.
        assert!(src.contains("pub extern \"C\" fn sq(v0: i64) -> i64"));
        assert!(src.contains("let v1: i64 = v0.wrapping_mul(v0);"));
        assert!(src.contains("    v1\n"));
    }

    #[test]
    fn emits_add3() {
        let src = emit(&add3_function()).unwrap();
        assert!(src.contains("pub extern \"C\" fn add3(v0: i64, v1: i64, v2: i64) -> i64"));
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
        assert!(src.contains("pub extern \"C\" fn answer() -> i64"));
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
            terminator: Term::Branch(Value(2), BlockId(1), BlockId(2)),
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
        assert!(src.contains("f64::from_bits(v0 as u64).sqrt().to_bits() as i64"));
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
        assert!(src.contains(".powf(f64::from_bits(v1 as u64))"));
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
