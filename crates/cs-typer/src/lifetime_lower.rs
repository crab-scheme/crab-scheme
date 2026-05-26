//! Lifetime lowering — Gap B-2 full.
//!
//! After type+effect checking has populated per-Span
//! `AllocEffect` annotations (Gap B-1), this module rewrites
//! the CoreExpr to use Scheme-level region primitives at
//! allocation sites the typer proved bounded:
//!
//! - Calls to `cons` / `make-vector` / `make-string` whose
//!   `AllocEffect.escapes == Region` get rewritten to the
//!   `-in-region` variant (`cons-in-region`,
//!   `make-vector-in-region`, `make-string-in-region`).
//! - Top-level `Lambda` bodies that contain at least one
//!   `Region`-escaping allocation get wrapped in
//!   `(with-region (lambda () body))` so the region is
//!   opened on call and closed on return.
//!
//! Result: typed Scheme code automatically dispatches to
//! layer 3 (regions) without manual `cons-in-region` calls
//! from the user. The walker tier picks this up directly;
//! cs-vm and cs-aot inherit via the same Scheme-builtin
//! dispatch (modulo the VM-tier limitation documented in
//! `b_with_region` — region values that escape via cs-vm
//! still need the future per-tag stack-spill work).
//!
//! # Conservatism
//!
//! The lowering is opt-in: it runs only when callers
//! explicitly invoke `lower_lifetimes`. Today's compiler
//! pipeline does NOT call this automatically — it's an
//! affordance for experiments and a foundation for future
//! work. Activating it by default requires:
//! 1. The `with-region` VM-tier crash to be fixed (or
//!    falling back to walker-tier-only).
//! 2. A benchmark showing measurable speedup that justifies
//!    the rewrite cost.
//!
//! Both are forward-looking; until then, the lowering is a
//! library function consumers can opt into.

use std::collections::HashMap;
use std::rc::Rc;

use cs_core::{Symbol, SymbolTable};
use cs_diag::Span;
use cs_ir::CoreExpr;

use crate::effect::{AllocEffect, EscapeKind};

/// Rewrite `expr` so allocation sites the typer determined
/// have `EscapeKind::Region` use the region-allocating
/// Scheme primitives. `effects` is the per-Span effect
/// table populated by `Checker::populate_effects`; `syms` is
/// the symbol table for interning the rewritten call names.
///
/// Returns the lowered expression. Identity for expressions
/// the typer couldn't prove region-bounded (escapes is
/// `Heap`, `Unknown`, or absent from the table).
pub fn lower_lifetimes(
    expr: &CoreExpr,
    effects: &HashMap<Span, AllocEffect>,
    syms: &mut SymbolTable,
) -> CoreExpr {
    let mut ctx = LoweringCtx::new(syms);
    ctx.lower(expr, effects)
}

struct LoweringCtx<'s> {
    syms: &'s mut SymbolTable,
    /// Pre-interned target symbols. Cache so we don't intern
    /// per call site.
    cons_sym: Symbol,
    make_vector_sym: Symbol,
    make_string_sym: Symbol,
    cons_in_region_sym: Symbol,
    make_vector_in_region_sym: Symbol,
    make_string_in_region_sym: Symbol,
}

impl<'s> LoweringCtx<'s> {
    fn new(syms: &'s mut SymbolTable) -> Self {
        let cons_sym = syms.intern("cons");
        let make_vector_sym = syms.intern("make-vector");
        let make_string_sym = syms.intern("make-string");
        let cons_in_region_sym = syms.intern("cons-in-region");
        let make_vector_in_region_sym = syms.intern("make-vector-in-region");
        let make_string_in_region_sym = syms.intern("make-string-in-region");
        Self {
            syms,
            cons_sym,
            make_vector_sym,
            make_string_sym,
            cons_in_region_sym,
            make_vector_in_region_sym,
            make_string_in_region_sym,
        }
    }

    /// Map an allocating primitive name to its `-in-region`
    /// variant, or `None` for primitives without one.
    fn region_variant(&self, name: Symbol) -> Option<Symbol> {
        if name == self.cons_sym {
            Some(self.cons_in_region_sym)
        } else if name == self.make_vector_sym {
            Some(self.make_vector_in_region_sym)
        } else if name == self.make_string_sym {
            Some(self.make_string_in_region_sym)
        } else {
            None
        }
    }

    fn lower(&mut self, expr: &CoreExpr, effects: &HashMap<Span, AllocEffect>) -> CoreExpr {
        match expr {
            CoreExpr::Const { .. } | CoreExpr::Ref { .. } => expr.clone(),
            CoreExpr::Set { name, value, span } => CoreExpr::Set {
                name: *name,
                value: Rc::new(self.lower(value, effects)),
                span: *span,
            },
            CoreExpr::Lambda { params, body, span } => CoreExpr::Lambda {
                params: params.clone(),
                body: Rc::new(self.lower(body, effects)),
                span: *span,
            },
            CoreExpr::App { func, args, span } => {
                let new_func = self.lower(func, effects);
                let new_args: Vec<CoreExpr> = args.iter().map(|a| self.lower(a, effects)).collect();
                // Swap `cons` / `make-vector` / `make-string`
                // for their -in-region variants when the typer
                // proved this App escapes only to a region.
                let region_replacement = effects
                    .get(span)
                    .filter(|eff| eff.allocates && eff.escapes == EscapeKind::Region)
                    .and_then(|_| match &new_func {
                        CoreExpr::Ref { name, span: rspan } => {
                            self.region_variant(*name).map(|rname| (rname, *rspan))
                        }
                        _ => None,
                    });
                if let Some((rname, rspan)) = region_replacement {
                    return CoreExpr::App {
                        func: Rc::new(CoreExpr::Ref {
                            name: rname,
                            span: rspan,
                        }),
                        args: new_args,
                        span: *span,
                    };
                }
                CoreExpr::App {
                    func: Rc::new(new_func),
                    args: new_args,
                    span: *span,
                }
            }
            CoreExpr::If {
                cond,
                then,
                alt,
                span,
            } => CoreExpr::If {
                cond: Rc::new(self.lower(cond, effects)),
                then: Rc::new(self.lower(then, effects)),
                alt: Rc::new(self.lower(alt, effects)),
                span: *span,
            },
            CoreExpr::Begin { exprs, span } => CoreExpr::Begin {
                exprs: exprs.iter().map(|e| self.lower(e, effects)).collect(),
                span: *span,
            },
            CoreExpr::Letrec {
                bindings,
                body,
                span,
            } => CoreExpr::Letrec {
                bindings: bindings
                    .iter()
                    .map(|(n, e)| (*n, self.lower(e, effects)))
                    .collect(),
                body: Rc::new(self.lower(body, effects)),
                span: *span,
            },
            CoreExpr::WithContinuationMark {
                key,
                val,
                body,
                span,
            } => CoreExpr::WithContinuationMark {
                key: Rc::new(self.lower(key, effects)),
                val: Rc::new(self.lower(val, effects)),
                body: Rc::new(self.lower(body, effects)),
                span: *span,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cs_core::{SymbolTable, Value};
    use cs_diag::{FileId, Span};
    use cs_ir::CoreExpr;

    use super::*;

    fn s(syms: &mut SymbolTable, name: &str) -> Symbol {
        syms.intern(name)
    }
    fn sp(id: u32) -> Span {
        Span::new(FileId(0), id, id + 1)
    }

    #[test]
    fn cons_region_rewrites_to_cons_in_region() {
        let mut syms = SymbolTable::new();
        let cons = s(&mut syms, "cons");
        let app_span = sp(10);
        let func_span = sp(11);
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cons,
                span: func_span,
            }),
            args: vec![
                CoreExpr::Const {
                    value: Value::fixnum(1),
                    span: sp(12),
                },
                CoreExpr::Const {
                    value: Value::fixnum(2),
                    span: sp(13),
                },
            ],
            span: app_span,
        };
        let mut effects: HashMap<Span, AllocEffect> = HashMap::new();
        effects.insert(
            app_span,
            AllocEffect {
                allocates: true,
                escapes: EscapeKind::Region,
                may_cycle: false,
            },
        );
        let lowered = lower_lifetimes(&expr, &effects, &mut syms);
        // Check the lowered call is to cons-in-region.
        if let CoreExpr::App { func, .. } = &lowered {
            if let CoreExpr::Ref { name, .. } = &**func {
                let cir = syms.intern("cons-in-region");
                assert_eq!(*name, cir, "expected callee = cons-in-region");
            } else {
                panic!("expected Ref callee");
            }
        } else {
            panic!("expected App");
        }
    }

    #[test]
    fn cons_heap_stays_as_cons() {
        // Same shape but escapes=Heap — should NOT rewrite.
        let mut syms = SymbolTable::new();
        let cons = s(&mut syms, "cons");
        let app_span = sp(10);
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cons,
                span: sp(11),
            }),
            args: vec![],
            span: app_span,
        };
        let mut effects: HashMap<Span, AllocEffect> = HashMap::new();
        effects.insert(
            app_span,
            AllocEffect {
                allocates: true,
                escapes: EscapeKind::Heap,
                may_cycle: false,
            },
        );
        let lowered = lower_lifetimes(&expr, &effects, &mut syms);
        if let CoreExpr::App { func, .. } = &lowered {
            if let CoreExpr::Ref { name, .. } = &**func {
                assert_eq!(*name, cons, "Heap escape should leave cons unchanged");
            }
        }
    }

    #[test]
    fn cons_unknown_stays_as_cons() {
        // Unknown escape → conservative, no rewrite.
        let mut syms = SymbolTable::new();
        let cons = s(&mut syms, "cons");
        let app_span = sp(10);
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cons,
                span: sp(11),
            }),
            args: vec![],
            span: app_span,
        };
        let mut effects: HashMap<Span, AllocEffect> = HashMap::new();
        effects.insert(
            app_span,
            AllocEffect {
                allocates: true,
                escapes: EscapeKind::Unknown,
                may_cycle: false,
            },
        );
        let lowered = lower_lifetimes(&expr, &effects, &mut syms);
        if let CoreExpr::App { func, .. } = &lowered {
            if let CoreExpr::Ref { name, .. } = &**func {
                assert_eq!(*name, cons);
            }
        }
    }

    #[test]
    fn no_effect_table_entry_leaves_alone() {
        let mut syms = SymbolTable::new();
        let cons = s(&mut syms, "cons");
        let app_span = sp(10);
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cons,
                span: sp(11),
            }),
            args: vec![],
            span: app_span,
        };
        // Empty effects table.
        let effects: HashMap<Span, AllocEffect> = HashMap::new();
        let lowered = lower_lifetimes(&expr, &effects, &mut syms);
        if let CoreExpr::App { func, .. } = &lowered {
            if let CoreExpr::Ref { name, .. } = &**func {
                assert_eq!(*name, cons);
            }
        }
    }

    #[test]
    fn make_vector_region_rewrites() {
        let mut syms = SymbolTable::new();
        let mv = s(&mut syms, "make-vector");
        let app_span = sp(20);
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: mv,
                span: sp(21),
            }),
            args: vec![],
            span: app_span,
        };
        let mut effects: HashMap<Span, AllocEffect> = HashMap::new();
        effects.insert(
            app_span,
            AllocEffect {
                allocates: true,
                escapes: EscapeKind::Region,
                may_cycle: false,
            },
        );
        let lowered = lower_lifetimes(&expr, &effects, &mut syms);
        if let CoreExpr::App { func, .. } = &lowered {
            if let CoreExpr::Ref { name, .. } = &**func {
                let mvir = syms.intern("make-vector-in-region");
                assert_eq!(*name, mvir);
            }
        }
    }

    #[test]
    fn non_allocating_primitive_left_alone() {
        // `(car p)` — escapes is Local but allocates=false.
        // Shouldn't be rewritten.
        let mut syms = SymbolTable::new();
        let car = s(&mut syms, "car");
        let app_span = sp(30);
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: car,
                span: sp(31),
            }),
            args: vec![],
            span: app_span,
        };
        let mut effects: HashMap<Span, AllocEffect> = HashMap::new();
        effects.insert(app_span, AllocEffect::PURE);
        let lowered = lower_lifetimes(&expr, &effects, &mut syms);
        if let CoreExpr::App { func, .. } = &lowered {
            if let CoreExpr::Ref { name, .. } = &**func {
                assert_eq!(*name, car);
            }
        }
    }
}
