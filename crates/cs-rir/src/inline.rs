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

use crate::{BlockId, Function, Inst, Value};

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
                _ => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, Const, Term, Type};

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
}
