//! `scalar-replace-cons` (#28) — eliminate non-escaping `cons` cells.
//!
//! Every `(cons a b)` heap-allocates a `Gc<Pair>` (`Rc::new` of a
//! ~120-byte struct plus refcount traffic). For a large class of
//! transient pairs the allocation is pure waste: a `cons` whose
//! result is *only* read back by `car` / `cdr` / `pair?` / `null?`
//! and never escapes the function is never mutated, never
//! `eq?`-compared, and never stored — so the pair object itself is
//! unobservable. We can replace the reads with the cons operands and
//! delete the allocation entirely.
//!
//! This is classic **scalar replacement of aggregates (SRA)**:
//!
//! ```text
//!   v   = cons a b          ;; v does not escape; uses are car/cdr/…
//!   d1  = car v             →   d1 = move a
//!   d2  = cdr v             →   d2 = move b
//!   d3  = pair? v           →   d3 = const #t
//!   d4  = null? v           →   d4 = const #f
//!                               (the `cons` is neutralized to `move v, a`
//!                                — v is now dead; no allocation remains)
//! ```
//!
//! # Relationship to `escape-to-region`
//!
//! [`super::escape_to_region`] promotes the *same* set of
//! non-escaping conses to bump-arena allocation (`Inst::ConsRegion`).
//! SRA targets the identical set but does strictly better — *no*
//! allocation at all, vs a region bump — so it runs in the `Early`
//! bucket, ahead of `escape-to-region`. A cons SRA eliminates is gone
//! before the region pass looks; conses SRA can't prove
//! (e.g. ones that flow through a block param) remain for the region
//! pass to consider.
//!
//! # Soundness
//!
//! The escape test is identical to `escape-to-region`'s and equally
//! conservative: `v` is eligible iff it never appears in any
//! terminator and every instruction that mentions `v` is one of
//! `Car(_, v)` / `Cdr(_, v)` / `PairP(_, v)` / `NullP(_, v)`. Any
//! other mention — a `SetCar`/`SetCdr` (mutation), a `Call` arg
//! (aliasing), an `EnvSet` (capture), a store into another aggregate,
//! or a terminator (return / block-arg flow) — disqualifies the cons.
//! Such a cons stays a real heap `Cons`.
//!
//! Because eligibility requires *all* uses to be those four read-only
//! forms, the eliminated pair's car/cdr operands dominate every read
//! (they were inputs to the `cons`, which dominated each read), so the
//! `Move` rewrites are SSA-valid. Eliminating the pair only *removes*
//! a live heap pointer, so it never invalidates a stack map.
//!
//! Operand enumeration reuses
//! [`cs_rir::inline::for_each_value_in_inst`], which is total only over
//! the variants [`cs_rir::inline::value_walker_covers`] reports; if the
//! function contains any variant outside that set we cannot enumerate
//! operands and so bail (eliminate nothing) — conservative and sound.

use cs_rir::inline::{
    for_each_value_in_inst, for_each_value_in_term, is_term_inline_supported, value_walker_covers,
};
use cs_rir::{Const, Function, Inst, Value};

use crate::{Bucket, Pass, PassContext};

/// The scalar-replace-cons pass. See module docs.
pub struct ScalarReplaceCons;

impl Pass for ScalarReplaceCons {
    fn name(&self) -> &'static str {
        "scalar-replace-cons"
    }

    fn bucket(&self) -> Bucket {
        // Run early: eliminating a cons removes a heap allocation and
        // forwards its operands directly to the reads, which both
        // shrinks the work for later passes and leaves `escape-to-region`
        // only the conses SRA could not prove (e.g. block-param flow).
        Bucket::Early
    }

    fn run(&self, func: &mut Function, ctx: &mut PassContext) {
        let n = scalar_replace_cons(func);
        if n > 0 {
            ctx.stats.record_mutations(self.name(), n);
        }
    }
}

/// Which read-only pair op a use site is — determines the rewrite.
#[derive(Clone, Copy)]
enum UseKind {
    Car,
    Cdr,
    PairP,
    NullP,
}

/// Eliminate every provably non-escaping, directly-consumed
/// `Inst::Cons` in `func`. Returns the number of conses eliminated.
/// Bails (returns 0) if the function contains any instruction or
/// terminator whose operands the analysis cannot enumerate.
///
/// Exposed (crate-internal) for direct unit testing without a
/// `PassContext`.
pub(crate) fn scalar_replace_cons(func: &mut Function) -> usize {
    // Soundness gate: the use scan calls `for_each_value_in_inst`,
    // which panics on variants it doesn't model. If any inst/term is
    // outside the walker's coverage we can't enumerate operands, so we
    // can't prove non-escape for any cons → bail.
    let fully_covered = func.blocks.iter().all(|b| {
        b.insts.iter().all(value_walker_covers) && is_term_inline_supported(&b.terminator)
    });
    if !fully_covered {
        return 0;
    }

    // Locate every `Cons` with its car/cdr operands. The operands are
    // captured here, from the pre-rewrite body, so chained conses
    // (whose operand is another cons's result) resolve to a stable
    // value id regardless of rewrite order.
    let mut sites: Vec<ConsSite> = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        for (ii, inst) in b.insts.iter().enumerate() {
            if let Inst::Cons(dst, car, _, cdr, _) = inst {
                sites.push(ConsSite {
                    bi,
                    ii,
                    dst: *dst,
                    car: *car,
                    cdr: *cdr,
                });
            }
        }
    }
    if sites.is_empty() {
        return 0;
    }

    let mut eliminated = 0usize;
    for site in sites {
        let Some(uses) = collect_eliminable_uses(func, site.bi, site.ii, site.dst) else {
            continue; // escapes, or has a use we can't rewrite
        };

        // Rewrite each read of the pair to read the operand directly.
        for (ubi, uii, kind) in uses {
            let inst = &mut func.blocks[ubi].insts[uii];
            let dst = match *inst {
                Inst::Car(d, _) | Inst::Cdr(d, _) | Inst::PairP(d, _) | Inst::NullP(d, _) => d,
                // `collect_eliminable_uses` only ever returns these four.
                _ => unreachable!("eliminable use is not a read-only pair op"),
            };
            *inst = match kind {
                UseKind::Car => Inst::Move(dst, site.car),
                UseKind::Cdr => Inst::Move(dst, site.cdr),
                UseKind::PairP => Inst::LoadConst(dst, Const::Boolean(true)),
                UseKind::NullP => Inst::LoadConst(dst, Const::Boolean(false)),
            };
        }

        // Neutralize the defining cons. `v` is now dead (every read was
        // rewritten away), so a plain register move with no allocation
        // keeps the def site SSA-valid and is trivially DCE-able. We do
        // NOT remove the instruction: that would shift indices and
        // invalidate the positions recorded for other cons sites.
        func.blocks[site.bi].insts[site.ii] = Inst::Move(site.dst, site.car);
        eliminated += 1;
    }
    eliminated
}

struct ConsSite {
    bi: usize,
    ii: usize,
    dst: Value,
    car: Value,
    cdr: Value,
}

/// If cons-result `v` (defined at `func.blocks[def_bi].insts[def_ii]`)
/// is non-escaping and every use is a rewritable read-only pair op,
/// return the list of `(block, inst, kind)` use sites. Otherwise return
/// `None` (the cons must stay a heap allocation).
///
/// Precondition: `func` is fully walker-covered (checked by the
/// caller), so `for_each_value_in_inst` is total here.
fn collect_eliminable_uses(
    func: &Function,
    def_bi: usize,
    def_ii: usize,
    v: Value,
) -> Option<Vec<(usize, usize, UseKind)>> {
    let mut uses: Vec<(usize, usize, UseKind)> = Vec::new();
    for (bi, b) in func.blocks.iter().enumerate() {
        // Any appearance of `v` in a terminator escapes it: `Return(v)`
        // returns it; a `Jump`/`Branch` arg flows it to a block param
        // that may itself be returned (block-param flow is not tracked —
        // conservatively treat every terminator mention as an escape).
        let mut term = b.terminator.clone();
        let mut in_term = false;
        for_each_value_in_term(&mut term, |x| {
            if *x == v {
                in_term = true;
            }
        });
        if in_term {
            return None;
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
            // `v` is used here. SSA guarantees `v` is only ever defined
            // by its `Cons`, so any mention is a use (in operand
            // position — the dst is a fresh value). Only the four
            // read-only pair ops keep `v` non-escaping and are
            // rewritable; anything else disqualifies the whole cons.
            let kind = match inst {
                Inst::Car(_, p) if *p == v => UseKind::Car,
                Inst::Cdr(_, p) if *p == v => UseKind::Cdr,
                Inst::PairP(_, s) if *s == v => UseKind::PairP,
                Inst::NullP(_, s) if *s == v => UseKind::NullP,
                _ => return None,
            };
            uses.push((bi, ii, kind));
        }
    }
    Some(uses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_rir::{Block, BlockId, Const, Function, Inst, Term, Type, Value};

    /// A 2-Fixnum-param function whose single block conses `(a . b)`
    /// into `Value(2)`, then runs `tail` (the uses) and terminates with
    /// `terminator`.
    fn cons_fn(tail: Vec<Inst>, terminator: Term) -> Function {
        let mut f = Function::new("sra_test");
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

    #[test]
    fn car_cdr_reads_are_scalar_replaced() {
        // v=cons(a,b); d=car v; e=cdr v; return d   (e dead but present)
        let mut f = cons_fn(
            vec![Inst::Car(Value(3), Value(2)), Inst::Cdr(Value(4), Value(2))],
            Term::Return(Value(3)),
        );
        let n = scalar_replace_cons(&mut f);
        assert_eq!(n, 1, "the non-escaping cons should be eliminated");
        // cons → move v, a ; car → move d, a ; cdr → move e, b
        assert!(matches!(
            f.blocks[0].insts[0],
            Inst::Move(Value(2), Value(0))
        ));
        assert!(matches!(
            f.blocks[0].insts[1],
            Inst::Move(Value(3), Value(0))
        ));
        assert!(matches!(
            f.blocks[0].insts[2],
            Inst::Move(Value(4), Value(1))
        ));
        // no Cons remains
        assert!(!f.blocks[0]
            .insts
            .iter()
            .any(|i| matches!(i, Inst::Cons(..))));
    }

    #[test]
    fn pair_p_and_null_p_become_constants() {
        let mut f = cons_fn(
            vec![
                Inst::PairP(Value(3), Value(2)),
                Inst::NullP(Value(4), Value(2)),
            ],
            Term::Return(Value(3)),
        );
        let n = scalar_replace_cons(&mut f);
        assert_eq!(n, 1);
        assert!(matches!(
            f.blocks[0].insts[1],
            Inst::LoadConst(Value(3), Const::Boolean(true))
        ));
        assert!(matches!(
            f.blocks[0].insts[2],
            Inst::LoadConst(Value(4), Const::Boolean(false))
        ));
    }

    #[test]
    fn returned_cons_is_not_eliminated() {
        // v escapes via the return terminator.
        let mut f = cons_fn(vec![], Term::Return(Value(2)));
        let n = scalar_replace_cons(&mut f);
        assert_eq!(n, 0, "an escaping (returned) cons must stay heap-allocated");
        assert!(matches!(f.blocks[0].insts[0], Inst::Cons(..)));
    }

    #[test]
    fn mutated_cons_is_not_eliminated() {
        // set-car! is not a read-only use → the cons escapes the SRA set.
        let mut f = cons_fn(
            vec![
                Inst::SetCar(Value(3), Value(2), Value(0)),
                Inst::Car(Value(4), Value(2)),
            ],
            Term::Return(Value(4)),
        );
        let n = scalar_replace_cons(&mut f);
        assert_eq!(n, 0, "a mutated cons must stay heap-allocated");
        assert!(matches!(f.blocks[0].insts[0], Inst::Cons(..)));
    }

    #[test]
    fn cons_passed_to_a_call_is_not_eliminated() {
        // Aliasing into a call arg escapes.
        let mut f = cons_fn(
            vec![Inst::Call(Value(3), Value(99), vec![Value(2)])],
            Term::Return(Value(3)),
        );
        let n = scalar_replace_cons(&mut f);
        assert_eq!(n, 0);
        assert!(matches!(f.blocks[0].insts[0], Inst::Cons(..)));
    }

    #[test]
    fn multiple_reads_of_same_pair_all_rewrite() {
        // (car v) twice + (cdr v): all three reads forwarded.
        let mut f = cons_fn(
            vec![
                Inst::Car(Value(3), Value(2)),
                Inst::Car(Value(4), Value(2)),
                Inst::Cdr(Value(5), Value(2)),
            ],
            Term::Return(Value(3)),
        );
        let n = scalar_replace_cons(&mut f);
        assert_eq!(n, 1);
        assert!(matches!(
            f.blocks[0].insts[1],
            Inst::Move(Value(3), Value(0))
        ));
        assert!(matches!(
            f.blocks[0].insts[2],
            Inst::Move(Value(4), Value(0))
        ));
        assert!(matches!(
            f.blocks[0].insts[3],
            Inst::Move(Value(5), Value(1))
        ));
    }
}
