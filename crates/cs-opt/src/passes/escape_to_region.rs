//! `escape-to-region` (#51, ADR 0020 Strategy C / ADR 0017 layer 5)
//! — promote non-escaping `cons` cells to region allocation.
//!
//! A conservative, intra-procedural escape analysis: for each
//! `Inst::Cons` result `v`, if `v` provably does not escape the
//! function, rewrite the instruction to `Inst::ConsRegion`. The
//! region-allocating cons (`vm_alloc_pair_region`) places the pair in
//! the innermost in-scope `cs_gc::Region`, or falls back to Rc-heap
//! allocation when no region is in scope — so the rewrite is never
//! *unsafe*, only sometimes a no-op (heap == heap).
//!
//! # Why "doesn't escape the function" is the right (and safe) test
//!
//! Regions are RAII thunk-scoped (`with-region` opens a region around a
//! call and the guard drops on return; `RegionScope` in
//! cs-runtime/src/regions.rs). Region scopes are therefore balanced
//! across call boundaries: no function can leave a region open on
//! return, and a region opened *inside* a `with-region` thunk lives in
//! that thunk's own RIR function, not its callees'. Consequently, **the
//! innermost region in scope at any cons site within a function G was
//! opened by a caller of G and outlives G's activation.** A cons that
//! does not escape G is dead before G returns — hence dead before that
//! region drops. Region-allocating it is safe; if no region is in
//! scope, the helper falls back to the heap. Either way: no
//! use-after-free, provided the escape analysis is *sound* (we only
//! ever promote when we can prove non-escape; when in any doubt we
//! leave the heap `Cons` unchanged).
//!
//! # Soundness of the analysis
//!
//! `v` escapes if it reaches any of: a `Return`, a block argument
//! (`Jump`/`Branch` args — it could flow to a returned block param), a
//! call (callee/arg of `Call`/`CallGeneral`/`CallSelf`), an
//! environment store (`EnvSet`/`EnvDefineLocal` — may be captured by a
//! closure), or a stored slot of another aggregate (`Cons`/`ConsRegion`
//! car/cdr, `VecAlloc` fill, `VecSet` value, `Move`, …). The only uses
//! that do NOT escape `v` are read-only pair operations whose result is
//! a *fresh* value: `car`, `cdr`, `pair?`, `null?`. So: `v` is
//! non-escaping iff every use of `v` is the pair operand of one of
//! those four, and `v` never appears in any terminator.
//!
//! To enumerate operands we reuse [`cs_rir::inline::for_each_value_in_inst`],
//! which is total only over the variants
//! [`cs_rir::inline::value_walker_covers`] reports. If the function
//! contains *any* variant outside that set we cannot enumerate its
//! operands, so we cannot prove non-escape for *any* cons in the
//! function → we bail (rewrite nothing). This is conservative and
//! sound: a missed promotion only costs perf, never correctness.

use cs_rir::inline::{
    for_each_value_in_inst, for_each_value_in_term, is_term_inline_supported, value_walker_covers,
};
use cs_rir::{Function, Inst, Value};

use crate::{Bucket, Pass, PassContext};

/// The escape-to-region pass. See module docs.
pub struct EscapeToRegion;

impl Pass for EscapeToRegion {
    fn name(&self) -> &'static str {
        "escape-to-region"
    }

    fn bucket(&self) -> Bucket {
        // Run in the default bucket: after any early normalization but
        // before late diagnostics. It only rewrites `Cons` → `ConsRegion`
        // (same operands), so it neither needs nor disturbs ordering.
        Bucket::Default
    }

    fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        let promoted = promote_non_escaping_cons(func);
        if promoted > 0 {
            ctx.stats.record_mutations(self.name(), promoted);
        }
    }
}

/// Rewrite every provably non-escaping `Inst::Cons` in `func` to
/// `Inst::ConsRegion`. Returns the number of rewrites. Bails (returns
/// 0) if the function contains any instruction or terminator whose
/// operands the analysis cannot enumerate.
///
/// Exposed (crate-internal) for direct unit testing without a
/// `PassContext`.
pub(crate) fn promote_non_escaping_cons(func: &mut Function) -> usize {
    // Soundness gate: the escape scan calls `for_each_value_in_inst`,
    // which panics on variants it doesn't model. If any inst/term is
    // outside the walker's coverage we can't enumerate operands, so we
    // can't prove non-escape for any cons → bail.
    let fully_covered = func.blocks.iter().all(|b| {
        b.insts.iter().all(value_walker_covers) && is_term_inline_supported(&b.terminator)
    });
    if !fully_covered {
        return 0;
    }

    // Locate every `Cons` (block index, inst index, dst value).
    let mut sites: Vec<(usize, usize, Value)> = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for (ii, inst) in b.insts.iter().enumerate() {
            if let Inst::Cons(dst, _, _, _, _) = inst {
                sites.push((bi, ii, *dst));
            }
        }
    }
    if sites.is_empty() {
        return 0;
    }

    // Decide each independently (analysis reads the pre-rewrite body;
    // an in-place Cons→ConsRegion swap doesn't move indices, and
    // ConsRegion is itself walker-covered, so later iterations still
    // see a consistent function).
    let promote: Vec<(usize, usize)> = sites
        .into_iter()
        .filter(|&(bi, ii, v)| !cons_escapes(func, bi, ii, v))
        .map(|(bi, ii, _)| (bi, ii))
        .collect();

    for &(bi, ii) in &promote {
        if let Inst::Cons(d, car, ct, cdr, dt) = func.blocks[bi].insts[ii] {
            func.blocks[bi].insts[ii] = Inst::ConsRegion(d, car, ct, cdr, dt);
        }
    }
    promote.len()
}

/// Does cons-result `v` (defined at `func.blocks[def_bi].insts[def_ii]`)
/// escape the function? Precondition: `func` is fully walker-covered
/// (checked by the caller), so `for_each_value_in_inst` is total here.
fn cons_escapes(func: &Function, def_bi: usize, def_ii: usize, v: Value) -> bool {
    for (bi, b) in func.blocks.iter().enumerate() {
        // Any appearance of `v` in a terminator escapes it: `Return(v)`
        // returns it; a `Jump`/`Branch` arg flows it to a block param
        // that may itself be returned (we don't track block-param flow
        // — conservatively treat all block args as escaping).
        let mut term = b.terminator.clone();
        let mut in_term = false;
        for_each_value_in_term(&mut term, |x| {
            if *x == v {
                in_term = true;
            }
        });
        if in_term {
            return true;
        }

        for (ii, inst) in b.insts.iter().enumerate() {
            if bi == def_bi && ii == def_ii {
                continue; // the defining Cons — not a use of `v`
            }
            let mut uses_v = false;
            let mut clone = inst.clone();
            for_each_value_in_inst(&mut clone, |x| {
                if *x == v {
                    uses_v = true;
                }
            });
            if !uses_v {
                continue;
            }
            // `v` is used by this inst. SSA guarantees `v` is only ever
            // defined by its `Cons`, so any match here is a use, never a
            // (re)definition. The only non-escaping uses are read-only
            // pair ops whose result is a fresh value.
            match inst {
                Inst::Car(_, p) | Inst::Cdr(_, p) if *p == v => {}
                Inst::PairP(_, s) | Inst::NullP(_, s) if *s == v => {}
                // Anything else that mentions `v` lets it escape:
                // stored into an aggregate, passed to a call, moved/
                // aliased, env-captured, or used as a non-pair operand.
                _ => return true,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};

    /// A 2-Fixnum-param function whose single block conses `(a . b)`
    /// then runs `tail` (the inst(s) that use the pair) and returns
    /// `ret`. Caller supplies the body tail + terminator.
    fn cons_fn(tail: Vec<Inst>, terminator: Term) -> Function {
        let mut f = Function::new("cons_test");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        let mut insts = vec![Inst::Cons(Value(2), Value(0), 0, Value(1), 0)];
        insts.extend(tail);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts,
            terminator,
        });
        f
    }

    fn nth_inst(f: &Function, i: usize) -> &Inst {
        &f.blocks[0].insts[i]
    }

    #[test]
    fn non_escaping_cons_is_promoted() {
        // (let ((p (cons a b))) (+ (car p) (cdr p))) — p never escapes.
        let mut f = cons_fn(
            vec![
                Inst::Car(Value(3), Value(2)),
                Inst::Cdr(Value(4), Value(2)),
                Inst::Add(Value(5), Value(3), Value(4)),
            ],
            Term::Return(Value(5)),
        );
        assert_eq!(promote_non_escaping_cons(&mut f), 1);
        assert!(matches!(nth_inst(&f, 0), Inst::ConsRegion(..)));
    }

    #[test]
    fn pair_predicate_use_is_non_escaping() {
        // (pair? (cons a b)) — read-only, promotable.
        let mut f = cons_fn(
            vec![Inst::PairP(Value(3), Value(2))],
            Term::Return(Value(3)),
        );
        assert_eq!(promote_non_escaping_cons(&mut f), 1);
        assert!(matches!(nth_inst(&f, 0), Inst::ConsRegion(..)));
    }

    #[test]
    fn returned_cons_stays_heap() {
        // (cons a b) returned directly — escapes via Return.
        let mut f = cons_fn(vec![], Term::Return(Value(2)));
        assert_eq!(promote_non_escaping_cons(&mut f), 0);
        assert!(matches!(nth_inst(&f, 0), Inst::Cons(..)));
    }

    #[test]
    fn cons_passed_to_call_stays_heap() {
        // (f (cons a b)) — escapes into the callee.
        let mut f = cons_fn(
            vec![Inst::Call(Value(3), Value(0), vec![Value(2)])],
            Term::Return(Value(3)),
        );
        assert_eq!(promote_non_escaping_cons(&mut f), 0);
        assert!(matches!(nth_inst(&f, 0), Inst::Cons(..)));
    }

    #[test]
    fn cons_stored_in_another_pair_stays_heap() {
        // (cons (cons a b) b) — inner pair escapes into the outer pair.
        // Inner = Value(2); outer = Value(3) stores Value(2) as car.
        let mut f = cons_fn(
            vec![Inst::Cons(Value(3), Value(2), 0, Value(1), 0)],
            Term::Return(Value(3)),
        );
        // Inner (Value 2) escapes into outer; outer (Value 3) escapes
        // via Return. Neither is promoted.
        assert_eq!(promote_non_escaping_cons(&mut f), 0);
        assert!(matches!(nth_inst(&f, 0), Inst::Cons(..)));
        assert!(matches!(nth_inst(&f, 1), Inst::Cons(..)));
    }

    #[test]
    fn cons_in_block_arg_stays_heap() {
        // The pair flows as a Jump block-arg → conservatively escapes.
        let mut f = Function::new("blockarg");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![Inst::Cons(Value(2), Value(0), 0, Value(1), 0)],
            terminator: Term::Jump(BlockId(1), vec![Value(2)]),
        });
        f.blocks.push(Block {
            id: BlockId(1),
            params: vec![(Value(3), Type::Any)],
            insts: vec![],
            terminator: Term::Return(Value(3)),
        });
        assert_eq!(promote_non_escaping_cons(&mut f), 0);
        assert!(matches!(&f.blocks[0].insts[0], Inst::Cons(..)));
    }

    #[test]
    fn bails_on_uncovered_variant() {
        // A non-escaping cons, but the function also contains a
        // walker-uncovered inst (StrAlloc) → the whole function bails
        // (we can't enumerate StrAlloc's operands to rule out a use).
        let mut f = cons_fn(
            vec![
                Inst::StrAlloc(Value(3), Value(0), Value(1)),
                Inst::Car(Value(4), Value(2)),
            ],
            Term::Return(Value(4)),
        );
        assert!(!value_walker_covers(&Inst::StrAlloc(
            Value(3),
            Value(0),
            Value(1)
        )));
        assert_eq!(promote_non_escaping_cons(&mut f), 0);
        assert!(matches!(nth_inst(&f, 0), Inst::Cons(..)));
    }

    #[test]
    fn mixed_promotes_only_the_non_escaping_one() {
        // Two conses: p1 (Value 2) only read via car → promote;
        // p2 (Value 4) returned → stays heap.
        let mut f = Function::new("mixed");
        f.params.push((Value(0), Type::Fixnum));
        f.params.push((Value(1), Type::Fixnum));
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Cons(Value(2), Value(0), 0, Value(1), 0), // p1
                Inst::Car(Value(3), Value(2)),                  // read p1
                Inst::Cons(Value(4), Value(3), 0, Value(1), 0), // p2
            ],
            terminator: Term::Return(Value(4)), // p2 escapes
        });
        assert_eq!(promote_non_escaping_cons(&mut f), 1);
        assert!(matches!(&f.blocks[0].insts[0], Inst::ConsRegion(..))); // p1
        assert!(matches!(&f.blocks[0].insts[2], Inst::Cons(..))); // p2
    }

    #[test]
    fn const_const_cons_promoted() {
        // (cons 1 2) read by cdr — both operands constants, still a
        // heap-eligible non-escaping pair.
        let mut f = Function::new("kk");
        f.entry = BlockId(0);
        f.blocks.push(Block {
            id: BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(Value(0), Const::Fixnum(1)),
                Inst::LoadConst(Value(1), Const::Fixnum(2)),
                Inst::Cons(Value(2), Value(0), 0, Value(1), 0),
                Inst::Cdr(Value(3), Value(2)),
            ],
            terminator: Term::Return(Value(3)),
        });
        assert_eq!(promote_non_escaping_cons(&mut f), 1);
        assert!(matches!(&f.blocks[0].insts[2], Inst::ConsRegion(..)));
    }
}
