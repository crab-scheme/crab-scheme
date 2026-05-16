//! M6 Phase 6 Stage A — leaf-callee inlining infrastructure.
//!
//! Iter 1 (this module): the analysis + scaffolding surface. No
//! codegen wiring; no translator changes. Sets up the
//! data structures and eligibility analyzer that iter 2 uses to
//! actually splice callee bodies into caller RIR.
//!
//! ## Background — why Phase 5's iter6 failed
//!
//! M6 Phase 5 attempted compile-time inlining in iter6 (reverted
//! before commit, recorded in `docs/milestones/m6-phase5-exit.md`).
//! Two paths were tried — MakeClosure-peephole and env-based callee
//! resolution — and both produced miscompiles on spectral-norm
//! (1.2595 instead of the correct 1.2742). The root cause was an
//! ad-hoc `remap_inst` helper that passed `MakeClosure` and
//! `CallGeneral` through without correctly renumbering their embedded
//! `Value`s. This module exists so iter 2's translator wiring can
//! reuse a deliberately-designed remap discipline instead of
//! re-inventing it inline.
//!
//! ## Iter 1 deliverable
//!
//! - [`InlineMetadata`] — what's true about a candidate callee.
//! - [`InlineRejection`] — every reason a candidate gets rejected.
//! - [`analyze_for_inline`] — the eligibility analyzer. Returns
//!   `Ok(InlineMetadata)` when the callee qualifies; `Err(reason)`
//!   otherwise. Pure function over `&Function`; no codegen state.
//! - [`ValueRemap`] / [`BlockRemap`] — offset-based renumbering tables
//!   that iter 2 will plug into a `for_each_value_mut` walker. The
//!   tables themselves are tested in this module; the walker lands
//!   in iter 2 when there's a concrete splice site that drives the
//!   per-Inst-variant enumeration cost.
//!
//! ## Iter 2+ scope (out of iter 1)
//!
//! - `for_each_value_mut` over every `Inst` variant — mechanical but
//!   tedious; deferred until there's a call site that needs it.
//! - The actual splice: `splice_into(caller, callsite, callee, remap)`.
//! - Ownership / refcount semantics audit on `BoxTyped` / `AnyTo*`
//!   when inlining transfers ownership across the splice boundary.

use crate::{Block, BlockId, Function, Inst, Term, Value};

/// Hard cap on RIR-inst count for an inlineable callee. Above this,
/// inlining bloats caller code with diminishing benefit. Tuned to
/// admit the motivating Phase 5 case (`matrix-elt` in spectral-norm —
/// 3-op leaf with a `Div` helper call) while rejecting larger bodies
/// where inlining would explode code size at every call site.
///
/// Iter 2 may surface that this needs tuning; treat as a starting
/// point.
pub const MAX_INLINE_INSTS: usize = 20;

/// Why a callee was rejected for inlining. Distinct variants for
/// each reason so iter 2's caller logging can attribute "we tried
/// to inline X but it was rejected because Y" — useful when bench
/// movement is smaller than expected and we want to know whether
/// the eligibility gate is too tight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineRejection {
    /// `inst_count > MAX_INLINE_INSTS`. Bigger bodies stay as
    /// `CallGeneral` to avoid bloating every call site.
    TooLarge { inst_count: usize, limit: usize },

    /// Body contains `Call`, `CallSelf`, or `CallGeneral`. A
    /// callee that itself dispatches isn't a "leaf"; inlining it
    /// would re-introduce the IC dispatch overhead we're trying
    /// to eliminate. Multi-level inlining is iter 4+ territory.
    HasInternalCall,

    /// Body contains `MakeClosure`. Inlining closure construction
    /// requires capturing the inlined-into env (the caller's), not
    /// the original callee's env — the runtime helper
    /// `vm_make_closure` reads `JIT_CALLER_ENV` which would now be
    /// wrong. Defer until a future iter handles env retargeting.
    HasMakeClosure,

    /// Body contains `EnvSet` or `EnvDefineLocal`. Same env-retargeting
    /// concern as MakeClosure — set! semantics depend on which
    /// env layer the binding lives in.
    HasEnvMutation,

    /// Multiple `Return` terminators (multi-exit). Iter 2 only
    /// supports single-exit callees (one `Return` block + maybe
    /// `Jump`/`Branch` interior blocks). Multi-exit needs proper
    /// join-block synthesis at the splice site.
    MultipleReturns { count: usize },

    /// Zero `Return` terminators (infinite loop / unreachable
    /// terminator only). Defensive — shouldn't happen for
    /// real bytecode-derived RIR, but the analyzer surfaces it
    /// rather than silently accepting.
    NoReturn,

    /// Body contains an `Inst` variant the splice walker doesn't
    /// know how to remap (the supported set is the "pure-arithmetic
    /// + simple memory read" subset for iter 2). The string is the
    /// variant name for diagnostics; iter 3+ widens the set or
    /// switches to a derive-based walker.
    UnsupportedInst(&'static str),

    /// Body contains a terminator the splice walker doesn't handle.
    /// Iter 2 supports `Return`, `Jump`, and `Branch` (i.e. all
    /// current Term variants — this arm is reserved for if Term
    /// grows new shapes).
    UnsupportedTerm(&'static str),
}

/// What the analyzer learned about an inlineable callee. Iter 2
/// uses this to drive the splice: the caller-side allocator needs
/// `max_value` and `max_block` to choose fresh SSA ids; the splice
/// needs `return_block` to know which jump target replaces the
/// callsite.
///
/// Carries no references back to the analyzed `Function` so it
/// can be cheaply cloned and stored on a per-callee cache key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineMetadata {
    /// Total instruction count across all blocks (same as
    /// `Function::inst_count`, cached here for efficiency).
    pub inst_count: usize,

    /// Number of basic blocks in the callee.
    pub block_count: usize,

    /// Highest `Value` id used anywhere in the callee (params + block
    /// params + Inst-defined values). Iter 2's remap allocates
    /// caller-side fresh ids starting from `caller_max_value + 1`,
    /// and the per-Inst walker offsets each callee-side id by the
    /// caller-side base. `max_value + 1` is therefore the count of
    /// fresh ids to allocate.
    pub max_value: u32,

    /// Highest `BlockId` used in the callee. Same offset-based
    /// remapping discipline as `max_value`.
    pub max_block: u32,

    /// The single block whose terminator is `Return`. The splice
    /// replaces the callsite Inst with a copy of every callee Inst,
    /// then routes the `Return v` into the caller's continuation
    /// (typically by making the caller's existing post-call code
    /// receive the remapped `v` as the destination of the original
    /// callsite). Iter 2 enforces single-exit via the analyzer.
    pub return_block: BlockId,

    /// The Value that the return terminator yields, remapped on
    /// iter 2's splice. Stored here so the splice doesn't have to
    /// re-walk the callee to find the Return terminator.
    pub return_value: Value,
}

/// The eligibility analyzer. Iter 2 calls this on every candidate
/// callee at a known callsite; takes the callee body in RIR form
/// and either accepts (with metadata) or rejects (with reason).
///
/// Pure: no allocation beyond the metadata struct, no mutation of
/// the input. Safe to call repeatedly on the same body if iter 2
/// needs to.
pub fn analyze_for_inline(func: &Function) -> Result<InlineMetadata, InlineRejection> {
    let inst_count = func.inst_count();
    if inst_count > MAX_INLINE_INSTS {
        return Err(InlineRejection::TooLarge {
            inst_count,
            limit: MAX_INLINE_INSTS,
        });
    }

    let mut return_count = 0usize;
    let mut return_block: Option<BlockId> = None;
    let mut return_value: Option<Value> = None;

    let mut max_value: u32 = func.params.iter().map(|(v, _)| v.0).max().unwrap_or(0);
    let mut max_block: u32 = func.blocks.iter().map(|b| b.id.0).max().unwrap_or(0);

    for block in &func.blocks {
        // Block params extend the SSA id space.
        for (v, _) in &block.params {
            if v.0 > max_value {
                max_value = v.0;
            }
        }
        if block.id.0 > max_block {
            max_block = block.id.0;
        }

        for inst in &block.insts {
            match inst {
                Inst::Call(_, _, _) | Inst::CallSelf(_, _) | Inst::CallGeneral(_, _, _) => {
                    return Err(InlineRejection::HasInternalCall);
                }
                Inst::MakeClosure(_, _) => {
                    return Err(InlineRejection::HasMakeClosure);
                }
                Inst::EnvSet(_, _) | Inst::EnvDefineLocal(_, _) => {
                    return Err(InlineRejection::HasEnvMutation);
                }
                // EnvLookup/EnvLookupAny in the callee would resolve
                // in the inlined-into env (caller's), not the original
                // callee's env. Without env retargeting (iter 4+), the
                // free-var binding semantics break. Reject so iter 2's
                // matrix-elt-class candidates (which only reference
                // params) flow through and free-var-touching callees
                // stay as CallGeneral.
                Inst::EnvLookup(_, _) | Inst::EnvLookupAny(_, _) => {
                    return Err(InlineRejection::UnsupportedInst("EnvLookup*"));
                }
                _ => {}
            }
            // Walker support gate — reject anything the splice walker
            // can't remap. Keeps the analyzer's acceptance set and the
            // walker's coverage in lockstep.
            if !is_inline_supported(inst) {
                return Err(InlineRejection::UnsupportedInst(inst_variant_name(inst)));
            }
            // Track the destination value (if any) to update max_value.
            // We use a lightweight inst_dst helper rather than walking
            // every variant here — iter 1 is allowed to be loose; the
            // exact `max_value` is recomputed conservatively in iter 2's
            // splice path. For iter 1 we keep this monotonic by also
            // scanning all `Value`-shaped fields of common patterns.
            if let Some(dst) = inst_dst(inst) {
                if dst.0 > max_value {
                    max_value = dst.0;
                }
            }
        }
        // Terminator must also be one the walker handles. Today every
        // Term variant (Return / Jump / Branch) is supported, but the
        // explicit gate guards against future Term additions.
        if !is_term_inline_supported(&block.terminator) {
            return Err(InlineRejection::UnsupportedTerm(term_variant_name(
                &block.terminator,
            )));
        }

        if let crate::Term::Return(v) = &block.terminator {
            return_count += 1;
            return_block = Some(block.id);
            return_value = Some(*v);
        }
    }

    match return_count {
        0 => Err(InlineRejection::NoReturn),
        1 => Ok(InlineMetadata {
            inst_count,
            block_count: func.block_count(),
            max_value,
            max_block,
            return_block: return_block.expect("return_count==1 -> Some"),
            return_value: return_value.expect("return_count==1 -> Some"),
        }),
        n => Err(InlineRejection::MultipleReturns { count: n }),
    }
}

/// Best-effort destination accessor: returns the `Value` an Inst
/// writes, when there is one. Used by the analyzer to track the
/// maximum SSA id across the body. Iter 2 will need a complete
/// `for_each_value_mut`; this helper only needs to return the
/// destination, not the sources, for iter 1's max-id tracking.
///
/// Conservatively returns `None` for variants where the destination
/// is either absent (`EnvSet`, `EnvDefineLocal`, `DeoptCheck`) or
/// not at a uniform position. The analyzer's `max_value` is allowed
/// to be a lower bound: iter 2 computes the true max during the
/// splice walker.
fn inst_dst(inst: &Inst) -> Option<Value> {
    // The overwhelming majority of Inst variants follow the shape
    // `Variant(dst, ...)`. Rather than enumerate every variant (300+)
    // we list the negative cases — variants where the first slot
    // is NOT a destination — and trust the positive case for
    // everything else. The match exhaustiveness pressure (compile
    // breaks when new variants land) is the safety net.
    match inst {
        // No destination at all.
        Inst::EnvSet(_, _) | Inst::EnvDefineLocal(_, _) | Inst::DeoptCheck(_, _) => None,
        // Every other variant has its dst in slot 0. We retrieve via
        // a positive match on the variants that the analyzer cares
        // about most (the ones that show up in matrix-elt-style
        // bodies). For variants outside this set, iter 1 returns
        // None — the analyzer's max_value lower-bound is then refined
        // by iter 2's splice walker.
        Inst::LoadConst(v, _) => Some(*v),
        Inst::Add(v, _, _)
        | Inst::Sub(v, _, _)
        | Inst::Mul(v, _, _)
        | Inst::Div(v, _, _)
        | Inst::FlonumAdd(v, _, _)
        | Inst::FlonumSub(v, _, _)
        | Inst::FlonumMul(v, _, _)
        | Inst::FlonumDiv(v, _, _)
        | Inst::FlonumLt(v, _, _)
        | Inst::FlonumEq(v, _, _)
        | Inst::Lt(v, _, _)
        | Inst::Eq(v, _, _) => Some(*v),
        Inst::FlonumSqrt(v, _)
        | Inst::FlonumAbs(v, _)
        | Inst::FlonumFloor(v, _)
        | Inst::FlonumCeil(v, _)
        | Inst::FlonumTrunc(v, _)
        | Inst::FlonumRound(v, _) => Some(*v),
        Inst::EnvLookup(v, _) | Inst::EnvLookupAny(v, _) => Some(*v),
        Inst::VecRef(v, _, _) | Inst::VecLength(v, _) | Inst::VecP(v, _) => Some(*v),
        Inst::BoxTyped(v, _, _)
        | Inst::AnyToFix(v, _)
        | Inst::AnyToBool(v, _)
        | Inst::AnyToFlo(v, _)
        | Inst::AnyTruthy(v, _) => Some(*v),
        // Conservative fallthrough: variants not in the matrix-elt
        // hot-set return None here. Iter 2's complete walker recomputes
        // the true max_value during splice; iter 1's lower-bound is
        // sufficient for the analyzer's caller-side bookkeeping (which
        // only cares about whether `max_value` is finite, not its
        // exact value).
        _ => None,
    }
}

/// Offset-based remapping table for SSA `Value`s. Iter 2 constructs
/// one of these at each splice site by setting `base = caller_max_value + 1`,
/// then applies it to every callee `Value` reference via the per-
/// `Inst` walker (iter 2's `for_each_value_mut`).
///
/// The offset shape (base + callee_id) avoids per-id collision
/// checks: callee ids are dense from 0 to `max_value` so adding
/// `base` produces a fresh range with no overlap to caller ids.
/// Use [`Self::map`] to apply.
///
/// Iter 1 only validates the table's behavior in tests. Iter 2
/// drives it from the actual splice site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueRemap {
    base: u32,
}

impl ValueRemap {
    /// Construct a remap that shifts every callee `Value` id by
    /// `base`. Pass `caller.max_value_id() + 1` to guarantee the
    /// remapped ids don't collide with caller's existing ids.
    pub fn new(base: u32) -> Self {
        Self { base }
    }

    /// Apply the remap to a single `Value`. Total function (no
    /// fallible cases) — the remap is purely additive.
    pub fn map(&self, v: Value) -> Value {
        Value(v.0.checked_add(self.base).expect("ValueRemap overflow"))
    }

    /// The offset baked into this remap. Iter 2 uses this when
    /// allocating fresh caller-side ids after the splice (the new
    /// caller `max_value` is `self.base + callee_max_value`).
    pub fn base(&self) -> u32 {
        self.base
    }
}

/// Companion to [`ValueRemap`] for `BlockId` remapping. Same offset
/// discipline; iter 2 uses both together at every splice site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRemap {
    base: u32,
}

impl BlockRemap {
    pub fn new(base: u32) -> Self {
        Self { base }
    }

    pub fn map(&self, b: BlockId) -> BlockId {
        BlockId(b.0.checked_add(self.base).expect("BlockRemap overflow"))
    }

    pub fn base(&self) -> u32 {
        self.base
    }
}

/// Splice request: combine a remap base with per-param substitutions
/// the way iter 2's translator splice site needs. Construct via
/// [`SpliceRequest::new`].
///
/// Iter 2 (single-block callee) builds one of these per inline site:
/// the caller's existing arg `Value`s replace the callee's param
/// `Value`s, and all other callee values are offset-renumbered to
/// avoid collision with the caller's existing SSA id space.
///
/// The param substitution is necessary because the callee's params
/// are SSA ids 0..n-1, while the caller's argument values come from
/// arbitrary positions in the caller's SSA id space. A pure offset
/// remap would shift the callee's param ids to fresh ones that
/// aren't bound to anything — the splice would emit dead code.
#[derive(Debug, Clone)]
pub struct SpliceRequest {
    /// Per-param substitution table — `param_subst[i]` is the
    /// caller-side `Value` that replaces the callee's `params[i].0`
    /// throughout the inlined body. Length must match the callee's
    /// param count; iter 2 enforces this at the call site.
    pub param_subst: Vec<Value>,
    /// Offset applied to every callee-side `Value` that isn't a
    /// parameter. Pass `caller_max_value + 1` to guarantee no
    /// collision with caller's existing ids.
    pub value_offset: u32,
    /// Offset applied to every callee-side `BlockId`. Iter 2's
    /// single-block case doesn't use this (the single block's insts
    /// get appended to the caller's current block, no block id
    /// survives); reserved for iter 3's multi-block splice.
    pub block_offset: u32,
}

impl SpliceRequest {
    pub fn new(param_subst: Vec<Value>, value_offset: u32, block_offset: u32) -> Self {
        Self {
            param_subst,
            value_offset,
            block_offset,
        }
    }

    /// Remap a single callee-side `Value` to its caller-side
    /// substitute. Param indices (callee values 0..n-1) hit the
    /// substitution table; everything else gets the offset.
    pub fn remap_value(&self, v: Value, n_params: u32) -> Value {
        if v.0 < n_params {
            self.param_subst[v.0 as usize]
        } else {
            // Non-param callee values are renumbered by offset.
            // The base is value_offset; subtract n_params so the
            // first non-param value (callee `Value(n_params)`) maps
            // to caller `Value(value_offset)` rather than
            // `Value(value_offset + n_params)`. This makes the
            // caller-side allocator predictably the next free id.
            Value(
                v.0.checked_sub(n_params)
                    .and_then(|x| x.checked_add(self.value_offset))
                    .expect("SpliceRequest remap underflow/overflow"),
            )
        }
    }

    /// Remap a callee-side `BlockId`. Iter 2's single-block path
    /// never calls this (no block survives the splice into a single
    /// caller block), but the API exists for iter 3.
    pub fn remap_block(&self, b: BlockId) -> BlockId {
        BlockId(
            b.0.checked_add(self.block_offset)
                .expect("SpliceRequest block remap overflow"),
        )
    }
}

/// Walker over an `Inst`'s `Value` operands. Iter 2's splice path
/// uses this to renumber every callee value (via `SpliceRequest`)
/// when cloning the callee's instructions into the caller.
///
/// **Coverage:** the matrix-elt-class "pure-arithmetic + simple
/// memory read" subset. The eligibility analyzer
/// (`analyze_for_inline`) rejects callees containing any
/// unsupported variant, so a successful analyze implies this walker
/// can handle every inst in the body — i.e. an `unreachable!()` arm
/// at the end is safe.
///
/// `f` is called once per `Value` reference in the inst. Includes
/// destinations (so renumbering picks up the dst's new id) and all
/// operand sources. Does NOT visit non-`Value` fields (constants,
/// type tags, symbol ids, lambda indices).
pub fn for_each_value_in_inst<F: FnMut(&mut Value)>(inst: &mut Inst, mut f: F) {
    match inst {
        // Destination-only (no Value sources).
        Inst::LoadConst(d, _) => f(d),

        // dst + 2 sources — the arith/cmp common shape.
        Inst::Add(d, a, b)
        | Inst::Sub(d, a, b)
        | Inst::Mul(d, a, b)
        | Inst::Div(d, a, b)
        | Inst::FlonumAdd(d, a, b)
        | Inst::FlonumSub(d, a, b)
        | Inst::FlonumMul(d, a, b)
        | Inst::FlonumDiv(d, a, b)
        | Inst::FlonumLt(d, a, b)
        | Inst::FlonumEq(d, a, b)
        | Inst::FlonumMax(d, a, b)
        | Inst::FlonumMin(d, a, b)
        | Inst::FlonumExpt(d, a, b)
        | Inst::Lt(d, a, b)
        | Inst::Eq(d, a, b)
        | Inst::Quotient(d, a, b)
        | Inst::Remainder(d, a, b)
        | Inst::Modulo(d, a, b)
        | Inst::FloorQuotient(d, a, b)
        | Inst::Gcd(d, a, b)
        | Inst::Lcm(d, a, b)
        | Inst::BitAnd(d, a, b)
        | Inst::BitOr(d, a, b)
        | Inst::BitXor(d, a, b)
        | Inst::BitwiseArithShiftLeft(d, a, b)
        | Inst::BitwiseArithShiftRight(d, a, b)
        | Inst::BitwiseBitSetP(d, a, b)
        | Inst::EqAny(d, a, b)
        | Inst::EqualAny(d, a, b)
        | Inst::VecRef(d, a, b)
        | Inst::StrRef(d, a, b) => {
            f(d);
            f(a);
            f(b);
        }

        // dst + 1 source — unary ops.
        Inst::FlonumSqrt(d, s)
        | Inst::FlonumAbs(d, s)
        | Inst::FlonumFloor(d, s)
        | Inst::FlonumCeil(d, s)
        | Inst::FlonumTrunc(d, s)
        | Inst::FlonumRound(d, s)
        | Inst::FlonumSin(d, s)
        | Inst::FlonumCos(d, s)
        | Inst::FlonumTan(d, s)
        | Inst::FlonumLog(d, s)
        | Inst::FlonumExp(d, s)
        | Inst::FlonumAsin(d, s)
        | Inst::FlonumAcos(d, s)
        | Inst::FlonumAtan(d, s)
        | Inst::FlEvenP(d, s)
        | Inst::FlOddP(d, s)
        | Inst::BitwiseBitCount(d, s)
        | Inst::BitwiseLength(d, s)
        | Inst::FxFirstBitSet(d, s)
        | Inst::FixToFlo(d, s)
        | Inst::IntCharBitcast(d, s)
        | Inst::VecLength(d, s)
        | Inst::VecP(d, s)
        | Inst::StrLength(d, s)
        | Inst::StrP(d, s)
        | Inst::AnyToFix(d, s)
        | Inst::AnyToBool(d, s)
        | Inst::AnyToFlo(d, s)
        | Inst::AnyTruthy(d, s)
        | Inst::Move(d, s) => {
            f(d);
            f(s);
        }

        // dst + 1 source + tag (u8).
        Inst::BoxTyped(d, s, _tag) => {
            f(d);
            f(s);
        }

        // 1 source + Type — DeoptCheck has no dst, just a guard on src.
        Inst::DeoptCheck(s, _t) => f(s),

        // RC3 iter 2.7 — the demote-pass now walks every surviving
        // Inst (not just is_inline_supported ones) to rewrite operands
        // via the alias map. The inliner's analyzer still uses
        // is_inline_supported as its gate; the walker covers a strict
        // superset so the demote pass doesn't crash on supported-by-
        // cs-aot-but-not-the-inliner variants. Each arm below covers
        // the Value operands of one such variant.
        //
        // ---- closure / call / env (post-demote survivors) ----
        Inst::MakeClosure(d, _idx) => f(d),
        Inst::Call(d, callee, args) | Inst::CallGeneral(d, callee, args) => {
            f(d);
            f(callee);
            for a in args {
                f(a);
            }
        }
        Inst::CallSelf(d, args) => {
            f(d);
            for a in args {
                f(a);
            }
        }
        Inst::EnvLookup(d, _sym) | Inst::EnvLookupAny(d, _sym) => f(d),
        Inst::EnvSet(_sym, v) => f(v),
        Inst::EnvDefineLocal(_sym, v) => f(v),

        // ---- vector / pair / type-predicate operands ----
        Inst::VecAlloc(d, n, fill) => {
            f(d);
            f(n);
            f(fill);
        }
        Inst::VecSet(d, v, idx, val) => {
            f(d);
            f(v);
            f(idx);
            f(val);
        }
        Inst::Cons(d, car_v, _car_tag, cdr_v, _cdr_tag) => {
            f(d);
            f(car_v);
            f(cdr_v);
        }
        Inst::Car(d, p) | Inst::Cdr(d, p) => {
            f(d);
            f(p);
        }
        Inst::AnyClone(d, s)
        | Inst::PairP(d, s)
        | Inst::NullP(d, s)
        | Inst::ProcedureP(d, s)
        | Inst::SymbolP(d, s)
        | Inst::FixnumP(d, s)
        | Inst::FlonumP(d, s) => {
            f(d);
            f(s);
        }

        // Any other Inst variant landing here means a NEW Inst variant
        // was added without extending this walker. The analyzer's
        // `is_inline_supported` set was historically the discipline
        // gate; today the demote pass relies on this walker covering
        // all surviving Inst variants. If you add an Inst, add a match
        // arm here that calls `f` on each Value operand.
        _ => unreachable!(
            "for_each_value_in_inst: variant {} not in walker — add an arm covering its Value operands",
            inst_variant_name(inst)
        ),
    }
}

/// Walker over a `Term`'s `Value` operands. Same lockstep discipline
/// with `is_term_inline_supported` as `for_each_value_in_inst` has
/// with `is_inline_supported`. Iter 2 covers every existing Term
/// variant; the explicit gate is for future-proofing.
pub fn for_each_value_in_term<F: FnMut(&mut Value)>(term: &mut Term, mut f: F) {
    match term {
        Term::Return(v) => f(v),
        Term::Jump(_block, args) => {
            for a in args {
                f(a);
            }
        }
        Term::Branch(cond, _then, _else) => f(cond),
    }
}

/// Walker over a `Term`'s `BlockId` operands. Iter 3 (multi-block
/// splice) uses this; iter 2 doesn't call it directly because the
/// single-block path doesn't preserve callee block ids.
pub fn for_each_block_in_term<F: FnMut(&mut BlockId)>(term: &mut Term, mut f: F) {
    match term {
        Term::Return(_) => {}
        Term::Jump(b, _) => f(b),
        Term::Branch(_, t, e) => {
            f(t);
            f(e);
        }
    }
}

/// Eligibility predicate for the splice walker. Kept in lockstep
/// with `for_each_value_in_inst`'s match arms — every variant the
/// walker handles, this returns true for; every variant outside the
/// supported set, this returns false for, and the analyzer rejects
/// callees containing it.
pub fn is_inline_supported(inst: &Inst) -> bool {
    matches!(
        inst,
        Inst::LoadConst(_, _)
            | Inst::Add(_, _, _)
            | Inst::Sub(_, _, _)
            | Inst::Mul(_, _, _)
            | Inst::Div(_, _, _)
            | Inst::FlonumAdd(_, _, _)
            | Inst::FlonumSub(_, _, _)
            | Inst::FlonumMul(_, _, _)
            | Inst::FlonumDiv(_, _, _)
            | Inst::FlonumLt(_, _, _)
            | Inst::FlonumEq(_, _, _)
            | Inst::FlonumMax(_, _, _)
            | Inst::FlonumMin(_, _, _)
            | Inst::FlonumExpt(_, _, _)
            | Inst::Lt(_, _, _)
            | Inst::Eq(_, _, _)
            | Inst::Quotient(_, _, _)
            | Inst::Remainder(_, _, _)
            | Inst::Modulo(_, _, _)
            | Inst::FloorQuotient(_, _, _)
            | Inst::Gcd(_, _, _)
            | Inst::Lcm(_, _, _)
            | Inst::BitAnd(_, _, _)
            | Inst::BitOr(_, _, _)
            | Inst::BitXor(_, _, _)
            | Inst::BitwiseArithShiftLeft(_, _, _)
            | Inst::BitwiseArithShiftRight(_, _, _)
            | Inst::BitwiseBitSetP(_, _, _)
            | Inst::EqAny(_, _, _)
            | Inst::EqualAny(_, _, _)
            | Inst::VecRef(_, _, _)
            | Inst::StrRef(_, _, _)
            | Inst::FlonumSqrt(_, _)
            | Inst::FlonumAbs(_, _)
            | Inst::FlonumFloor(_, _)
            | Inst::FlonumCeil(_, _)
            | Inst::FlonumTrunc(_, _)
            | Inst::FlonumRound(_, _)
            | Inst::FlonumSin(_, _)
            | Inst::FlonumCos(_, _)
            | Inst::FlonumTan(_, _)
            | Inst::FlonumLog(_, _)
            | Inst::FlonumExp(_, _)
            | Inst::FlonumAsin(_, _)
            | Inst::FlonumAcos(_, _)
            | Inst::FlonumAtan(_, _)
            | Inst::FlEvenP(_, _)
            | Inst::FlOddP(_, _)
            | Inst::BitwiseBitCount(_, _)
            | Inst::BitwiseLength(_, _)
            | Inst::FxFirstBitSet(_, _)
            | Inst::FixToFlo(_, _)
            | Inst::IntCharBitcast(_, _)
            | Inst::VecLength(_, _)
            | Inst::VecP(_, _)
            | Inst::StrLength(_, _)
            | Inst::StrP(_, _)
            | Inst::AnyToFix(_, _)
            | Inst::AnyToBool(_, _)
            | Inst::AnyToFlo(_, _)
            | Inst::AnyTruthy(_, _)
            | Inst::Move(_, _)
            | Inst::BoxTyped(_, _, _)
            | Inst::DeoptCheck(_, _)
    )
}

/// Eligibility predicate for terminators in the splice walker. Today
/// covers every existing Term variant; reserved for future Term
/// additions.
pub fn is_term_inline_supported(term: &Term) -> bool {
    matches!(
        term,
        Term::Return(_) | Term::Jump(_, _) | Term::Branch(_, _, _)
    )
}

/// Variant-name diagnostics for `UnsupportedInst` rejections. Keeps
/// the rejection reason explicit so iter 2's caller-side logging
/// can attribute "tried to inline X but rejected because variant
/// `Foo` isn't in the walker yet".
fn inst_variant_name(inst: &Inst) -> &'static str {
    // Match arms ordered by ergonomic groupings (arith, cmp, mem,
    // env, call). Falls through to "<other>" rather than failing
    // hard so the rejection still produces actionable telemetry
    // when a new variant lands without a corresponding name arm.
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
        Inst::FlonumLt(..) => "FlonumLt",
        Inst::FlonumEq(..) => "FlonumEq",
        Inst::Call(..) => "Call",
        Inst::CallSelf(..) => "CallSelf",
        Inst::CallGeneral(..) => "CallGeneral",
        Inst::EnvLookup(..) => "EnvLookup",
        Inst::EnvLookupAny(..) => "EnvLookupAny",
        Inst::EnvSet(..) => "EnvSet",
        Inst::EnvDefineLocal(..) => "EnvDefineLocal",
        Inst::MakeClosure(..) => "MakeClosure",
        Inst::VecAlloc(..) => "VecAlloc",
        Inst::VecSet(..) => "VecSet",
        Inst::StrAlloc(..) => "StrAlloc",
        Inst::BoxTyped(..) => "BoxTyped",
        Inst::DeoptCheck(..) => "DeoptCheck",
        _ => "<other>",
    }
}

fn term_variant_name(term: &Term) -> &'static str {
    match term {
        Term::Return(_) => "Return",
        Term::Jump(_, _) => "Jump",
        Term::Branch(_, _, _) => "Branch",
    }
}

/// Splice a single-block inlineable callee into the caller's current
/// block. Iter 2's primary entry point — given the caller-side block
/// receiving the inline (typically the one mid-translation that's
/// about to emit a `CallGeneral`), the callee's RIR, the analyzer's
/// metadata, and a splice request mapping params + offsetting
/// values, append the callee's instructions to the caller-side
/// instruction vector and return the caller-side value that holds
/// the callee's return result.
///
/// Caller is responsible for:
/// - Verifying eligibility via `analyze_for_inline` BEFORE calling
///   this (the precondition is that the analyzer accepted the
///   callee; this fn does not re-validate).
/// - Constructing the `SpliceRequest` with the right `param_subst`
///   (one entry per callee param, in order) and `value_offset`
///   (caller's `next_value_id`).
/// - Advancing the caller's `next_value_id` to
///   `splice.value_offset + callee.max_value - n_params + 1` after
///   the splice (the highest fresh id the callee body claimed).
/// - Wiring the returned `Value` into wherever the original
///   `CallGeneral`'s dst would have flowed.
///
/// Iter 2 only handles single-block callees (one block = entry
/// block, terminator = `Return v`). Multi-block is iter 3.
pub fn splice_single_block(
    caller_block_insts: &mut Vec<Inst>,
    callee: &Function,
    metadata: &InlineMetadata,
    splice: &SpliceRequest,
) -> Value {
    debug_assert_eq!(
        callee.blocks.len(),
        1,
        "splice_single_block: callee must be single-block (iter 2)"
    );
    debug_assert_eq!(
        splice.param_subst.len(),
        callee.params.len(),
        "splice_single_block: param_subst len must match callee params"
    );
    let n_params = callee.params.len() as u32;
    let callee_block: &Block = &callee.blocks[0];
    debug_assert_eq!(
        callee_block.id, metadata.return_block,
        "splice_single_block: single-block callee's only block must be the return block"
    );

    for inst in &callee_block.insts {
        let mut cloned = inst.clone();
        for_each_value_in_inst(&mut cloned, |v| {
            *v = splice.remap_value(*v, n_params);
        });
        caller_block_insts.push(cloned);
    }
    // The callee's `Return` value, remapped, is what the caller's
    // post-splice code should see in place of the original
    // CallGeneral's dst. Caller is responsible for wiring this
    // through (e.g. via a final `Move dst <- returned` if dst is
    // pre-allocated, or by using the returned `Value` directly as
    // the new dst for downstream insts).
    splice.remap_value(metadata.return_value, n_params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Const, Type};

    /// Helper: build a single-block matrix-elt-shaped function.
    /// Shape: `(define (matrix-elt i j) ...)` — eligible for inline.
    fn matrix_elt_like() -> Function {
        let mut f = Function::new("matrix-elt");
        f.params.push((Value(0), Type::Fixnum)); // i
        f.params.push((Value(1), Type::Fixnum)); // j
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Add(Value(2), Value(0), Value(1)),
                Inst::LoadConst(Value(3), Const::Fixnum(1)),
                Inst::Add(Value(4), Value(2), Value(3)),
            ],
            terminator: Term::Return(Value(4)),
        });
        f
    }

    #[test]
    fn analyze_accepts_simple_leaf() {
        let f = matrix_elt_like();
        let md = analyze_for_inline(&f).expect("matrix-elt should be inlineable");
        assert_eq!(md.inst_count, 3);
        assert_eq!(md.block_count, 1);
        assert_eq!(md.max_value, 4);
        assert_eq!(md.max_block, 0);
        assert_eq!(md.return_block, BlockId(0));
        assert_eq!(md.return_value, Value(4));
    }

    #[test]
    fn analyze_rejects_too_large() {
        let mut f = Function::new("big");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        let mut insts = Vec::new();
        for i in 0..30 {
            insts.push(Inst::LoadConst(Value(i + 1), Const::Fixnum(i as i64)));
        }
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts,
            terminator: Term::Return(Value(1)),
        });
        match analyze_for_inline(&f) {
            Err(InlineRejection::TooLarge { inst_count, limit }) => {
                assert_eq!(inst_count, 30);
                assert_eq!(limit, MAX_INLINE_INSTS);
            }
            other => panic!("expected TooLarge, got {:?}", other),
        }
    }

    #[test]
    fn analyze_rejects_internal_call() {
        let mut f = Function::new("calls-something");
        f.params.push((Value(0), Type::Procedure));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::CallGeneral(Value(2), Value(0), vec![Value(1)])],
            terminator: Term::Return(Value(2)),
        });
        assert_eq!(
            analyze_for_inline(&f),
            Err(InlineRejection::HasInternalCall)
        );
    }

    #[test]
    fn analyze_rejects_self_recursion() {
        let mut f = Function::new("rec");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::CallSelf(Value(1), vec![Value(0)])],
            terminator: Term::Return(Value(1)),
        });
        assert_eq!(
            analyze_for_inline(&f),
            Err(InlineRejection::HasInternalCall)
        );
    }

    #[test]
    fn analyze_rejects_make_closure() {
        let mut f = Function::new("closure-builder");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::MakeClosure(Value(0), 7)],
            terminator: Term::Return(Value(0)),
        });
        assert_eq!(analyze_for_inline(&f), Err(InlineRejection::HasMakeClosure));
    }

    #[test]
    fn analyze_rejects_env_mutation() {
        let mut f = Function::new("mut");
        f.params.push((Value(0), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::EnvSet(42, Value(0))],
            terminator: Term::Return(Value(0)),
        });
        assert_eq!(analyze_for_inline(&f), Err(InlineRejection::HasEnvMutation));
    }

    #[test]
    fn analyze_rejects_multiple_returns() {
        // Two-block function with both blocks terminating in Return.
        // Reached via a Branch on the entry block. Multi-exit; iter 2
        // doesn't handle this shape yet.
        let mut f = Function::new("twoexit");
        f.params.push((Value(0), Type::Boolean));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(1), Const::Fixnum(0))],
            terminator: Term::Branch(Value(0), BlockId(1), BlockId(2)),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(2), Const::Fixnum(1))],
            terminator: Term::Return(Value(2)),
        });
        f.blocks.push(Block {
            id: BlockId(2),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(3), Const::Fixnum(2))],
            terminator: Term::Return(Value(3)),
        });
        assert_eq!(
            analyze_for_inline(&f),
            Err(InlineRejection::MultipleReturns { count: 2 })
        );
    }

    #[test]
    fn analyze_rejects_no_return() {
        let mut f = Function::new("loops");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![],
            terminator: Term::Jump(BlockId(0), vec![]),
        });
        assert_eq!(analyze_for_inline(&f), Err(InlineRejection::NoReturn));
    }

    #[test]
    fn analyze_accepts_multi_block_single_return() {
        // Branch + join — single Return — should be accepted in iter 2
        // (multi-block but single-exit). Iter 1's analyzer accepts it;
        // iter 2's splice handler may further gate.
        let mut f = Function::new("branchjoin");
        f.params.push((Value(0), Type::Boolean));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(1), Const::Fixnum(0))],
            terminator: Term::Branch(Value(0), BlockId(1), BlockId(2)),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(2), Const::Fixnum(1))],
            terminator: Term::Jump(BlockId(3), vec![Value(2)]),
        });
        f.blocks.push(Block {
            id: BlockId(2),
            params: vec![],
            insts: vec![Inst::LoadConst(Value(3), Const::Fixnum(2))],
            terminator: Term::Jump(BlockId(3), vec![Value(3)]),
        });
        f.blocks.push(Block {
            id: BlockId(3),
            params: vec![(Value(4), Type::Fixnum)],
            insts: vec![],
            terminator: Term::Return(Value(4)),
        });
        let md = analyze_for_inline(&f).expect("multi-block single-return is accepted");
        assert_eq!(md.block_count, 4);
        assert_eq!(md.max_block, 3);
        assert_eq!(md.return_block, BlockId(3));
        assert_eq!(md.return_value, Value(4));
    }

    #[test]
    fn value_remap_offsets_correctly() {
        let r = ValueRemap::new(100);
        assert_eq!(r.map(Value(0)), Value(100));
        assert_eq!(r.map(Value(5)), Value(105));
        assert_eq!(r.base(), 100);
    }

    #[test]
    fn block_remap_offsets_correctly() {
        let r = BlockRemap::new(50);
        assert_eq!(r.map(BlockId(0)), BlockId(50));
        assert_eq!(r.map(BlockId(3)), BlockId(53));
        assert_eq!(r.base(), 50);
    }

    #[test]
    fn value_remap_with_zero_base_is_identity() {
        // Zero base is legal (no shift) — used in tests that want to
        // verify the walker without actually renumbering.
        let r = ValueRemap::new(0);
        for i in 0..10 {
            assert_eq!(r.map(Value(i)), Value(i));
        }
    }

    #[test]
    #[should_panic(expected = "ValueRemap overflow")]
    fn value_remap_overflow_panics() {
        let r = ValueRemap::new(u32::MAX);
        r.map(Value(1));
    }

    // ===== Iter 2 — walker + splice tests =====

    #[test]
    fn walker_visits_all_values_in_arith_inst() {
        let mut inst = Inst::Add(Value(10), Value(20), Value(30));
        let mut seen: Vec<u32> = Vec::new();
        for_each_value_in_inst(&mut inst, |v| seen.push(v.0));
        assert_eq!(seen, vec![10, 20, 30]);
    }

    #[test]
    fn walker_renumbers_in_place() {
        let mut inst = Inst::Mul(Value(5), Value(7), Value(9));
        for_each_value_in_inst(&mut inst, |v| *v = Value(v.0 + 100));
        if let Inst::Mul(d, a, b) = inst {
            assert_eq!(d, Value(105));
            assert_eq!(a, Value(107));
            assert_eq!(b, Value(109));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn walker_handles_unary_loadconst_and_deoptcheck() {
        let mut load = Inst::LoadConst(Value(3), Const::Fixnum(42));
        let mut seen: Vec<u32> = Vec::new();
        for_each_value_in_inst(&mut load, |v| seen.push(v.0));
        assert_eq!(seen, vec![3]); // only dst

        let mut deopt = Inst::DeoptCheck(Value(8), Type::Fixnum);
        seen.clear();
        for_each_value_in_inst(&mut deopt, |v| seen.push(v.0));
        assert_eq!(seen, vec![8]); // only src (no dst)
    }

    #[test]
    fn walker_handles_boxtyped_three_fields() {
        let mut bt = Inst::BoxTyped(Value(11), Value(22), 3 /* JIT_RT_FLONUM */);
        let mut seen: Vec<u32> = Vec::new();
        for_each_value_in_inst(&mut bt, |v| seen.push(v.0));
        assert_eq!(seen, vec![11, 22]); // tag (u8) is NOT a Value
    }

    #[test]
    fn term_walker_visits_jump_args() {
        let mut term = Term::Jump(BlockId(7), vec![Value(1), Value(2), Value(3)]);
        let mut seen: Vec<u32> = Vec::new();
        for_each_value_in_term(&mut term, |v| seen.push(v.0));
        assert_eq!(seen, vec![1, 2, 3]);
    }

    #[test]
    fn term_walker_visits_branch_cond() {
        let mut term = Term::Branch(Value(42), BlockId(1), BlockId(2));
        let mut seen: Vec<u32> = Vec::new();
        for_each_value_in_term(&mut term, |v| seen.push(v.0));
        assert_eq!(seen, vec![42]); // blocks aren't Values
    }

    #[test]
    fn term_walker_blocks_visits_branch_targets() {
        let mut term = Term::Branch(Value(0), BlockId(3), BlockId(7));
        let mut seen: Vec<u32> = Vec::new();
        for_each_block_in_term(&mut term, |b| seen.push(b.0));
        assert_eq!(seen, vec![3, 7]);
    }

    #[test]
    fn is_inline_supported_accepts_arith() {
        assert!(is_inline_supported(&Inst::Add(
            Value(0),
            Value(1),
            Value(2)
        )));
        assert!(is_inline_supported(&Inst::FlonumMul(
            Value(0),
            Value(1),
            Value(2)
        )));
        assert!(is_inline_supported(&Inst::LoadConst(
            Value(0),
            Const::Fixnum(1)
        )));
    }

    #[test]
    fn is_inline_supported_rejects_calls_and_closures() {
        assert!(!is_inline_supported(&Inst::Call(
            Value(0),
            Value(1),
            vec![Value(2)]
        )));
        assert!(!is_inline_supported(&Inst::CallGeneral(
            Value(0),
            Value(1),
            vec![]
        )));
        assert!(!is_inline_supported(&Inst::MakeClosure(Value(0), 7)));
        assert!(!is_inline_supported(&Inst::EnvLookup(Value(0), 42)));
        assert!(!is_inline_supported(&Inst::EnvSet(42, Value(0))));
    }

    #[test]
    fn analyzer_rejects_unsupported_variant() {
        // VecAlloc isn't in iter 2's supported set (allocates,
        // ownership-tracking deferred to iter 4+). Should produce
        // UnsupportedInst rather than silently accepting.
        let mut f = Function::new("alloc-body");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Any));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::VecAlloc(Value(2), Value(0), Value(1))],
            terminator: Term::Return(Value(2)),
        });
        match analyze_for_inline(&f) {
            Err(InlineRejection::UnsupportedInst(name)) => {
                assert_eq!(name, "VecAlloc");
            }
            other => panic!("expected UnsupportedInst, got {:?}", other),
        }
    }

    #[test]
    fn splice_request_remaps_params_via_substitution() {
        // Callee with 2 params [Value(0), Value(1)] and a body using
        // Value(2)..Value(4). Splice substitutes caller args [V(100),
        // V(200)] for the params, and offsets non-param values by 50.
        let req = SpliceRequest::new(vec![Value(100), Value(200)], 50, 0);
        // Param 0 -> substitution
        assert_eq!(req.remap_value(Value(0), 2), Value(100));
        assert_eq!(req.remap_value(Value(1), 2), Value(200));
        // Non-param values get offset (callee Value(2) -> caller
        // Value(50), Value(3) -> Value(51), etc.)
        assert_eq!(req.remap_value(Value(2), 2), Value(50));
        assert_eq!(req.remap_value(Value(3), 2), Value(51));
        assert_eq!(req.remap_value(Value(4), 2), Value(52));
    }

    #[test]
    fn splice_request_zero_params_pure_offset() {
        // No params -> all values get offset. The substitution table
        // is empty.
        let req = SpliceRequest::new(vec![], 100, 0);
        assert_eq!(req.remap_value(Value(0), 0), Value(100));
        assert_eq!(req.remap_value(Value(5), 0), Value(105));
    }

    #[test]
    fn splice_single_block_inlines_matrix_elt_like() {
        // Build a 2-param callee with 3 insts + Return — the
        // matrix-elt-shape test fixture.
        let callee = matrix_elt_like();
        let md = analyze_for_inline(&callee).expect("eligible");

        // Caller's "current block" insts buffer starts empty for the
        // test. Caller's args for the call are Value(100), Value(101).
        // Caller's next_value_id is 200 — every callee non-param value
        // gets renumbered starting from 200.
        let mut caller_insts: Vec<Inst> = Vec::new();
        let req = SpliceRequest::new(vec![Value(100), Value(101)], 200, 0);
        let returned = splice_single_block(&mut caller_insts, &callee, &md, &req);

        // The callee had:
        //   Add(V(2), V(0)=i, V(1)=j)
        //   LoadConst(V(3), Fixnum(1))
        //   Add(V(4), V(2), V(3))
        //   Return V(4)
        //
        // After splicing with params=[V(100), V(101)] and offset=200:
        //   Add(V(200), V(100), V(101))
        //   LoadConst(V(201), Fixnum(1))
        //   Add(V(202), V(200), V(201))
        // The returned `Value` is the remapped Return value V(4) -> V(202).
        assert_eq!(caller_insts.len(), 3);
        match &caller_insts[0] {
            Inst::Add(d, a, b) => {
                assert_eq!(*d, Value(200));
                assert_eq!(*a, Value(100));
                assert_eq!(*b, Value(101));
            }
            _ => panic!("inst 0 should be Add"),
        }
        match &caller_insts[1] {
            Inst::LoadConst(d, Const::Fixnum(1)) => {
                assert_eq!(*d, Value(201));
            }
            _ => panic!("inst 1 should be LoadConst(_, 1)"),
        }
        match &caller_insts[2] {
            Inst::Add(d, a, b) => {
                assert_eq!(*d, Value(202));
                assert_eq!(*a, Value(200));
                assert_eq!(*b, Value(201));
            }
            _ => panic!("inst 2 should be Add"),
        }
        assert_eq!(returned, Value(202));
    }

    #[test]
    fn splice_appends_to_existing_caller_insts() {
        // Verify the splice APPENDS rather than overwrites the
        // caller's existing insts. Iter 2's translator wiring relies
        // on this.
        let callee = matrix_elt_like();
        let md = analyze_for_inline(&callee).expect("eligible");
        let mut caller_insts: Vec<Inst> = vec![
            Inst::LoadConst(Value(50), Const::Fixnum(99)),
            Inst::Add(Value(51), Value(50), Value(50)),
        ];
        let req = SpliceRequest::new(vec![Value(51), Value(50)], 60, 0);
        let returned = splice_single_block(&mut caller_insts, &callee, &md, &req);
        // 2 pre-existing + 3 callee insts = 5 total
        assert_eq!(caller_insts.len(), 5);
        // Pre-existing insts unchanged.
        match &caller_insts[0] {
            Inst::LoadConst(Value(50), Const::Fixnum(99)) => {}
            _ => panic!("caller_insts[0] mutated"),
        }
        // Returned is the remapped final value.
        // Callee's V(4) Return -> V(62) caller-side (60 + (4 - 2))
        assert_eq!(returned, Value(62));
    }
}
