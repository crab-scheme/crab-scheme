//! Bridge from `cs_typer::Type` to `cs_rir::Type`.
//!
//! Phase 5 iter 5.1: produces `param_type_hints` the JIT / AOT
//! pipelines consume. `cs_rir::Type` is the narrower runtime
//! vocabulary used at translate time (11 atoms + `Any`); the
//! typer's lattice is richer (unions, function types, container
//! types) so the bridge is lossy.
//!
//! Lowering rules:
//!
//! - Atoms (Fixnum, Flonum, Boolean, Character, Symbol, Pair,
//!   Vector, String, ByteVector, Procedure, Null) map 1-1.
//! - `Any` and `Never` both lower to `Any`. (`Never` means
//!   "unreachable" — at runtime the value can't exist, so `Any`
//!   is the safe fallback.)
//! - `Procedure_(…)` collapses to `Procedure` — the runtime
//!   doesn't carry arrow types.
//! - `Listof T` and `Vectorof T` lower to `Any` because:
//!   - `Listof` is `(U Pair Null)` at runtime — a pair-or-empty
//!     distinction the dispatcher can't exploit.
//!   - `Vectorof` is structurally `Vector` but element types
//!     don't propagate through the JIT's specialized opcodes.
//! - `Union` lowers to the common `cs_rir::Type` if all members
//!   agree; otherwise `Any`. So `(U Fixnum Fixnum)` (already
//!   collapsed by the constructor) is fine; `(U Fixnum Flonum)`
//!   becomes `Any` because the JIT can't dispatch on the union.
//!
//! The other half of this module — `param_hints_from_table` —
//! walks the `AnnotationTable.lambdas` map and produces
//! `HashMap<Span, Vec<cs_rir::Type>>` keyed by lambda span. The
//! caller (cs-cli, cs-runtime/jit.rs) matches each
//! `CompiledLambda`'s span against this map to fetch the hints
//! for `bytecode_to_rir_full`.

use std::collections::HashMap;

use cs_core::Symbol;
use cs_diag::Span;

use crate::annotate::AnnotationTable;
use crate::types::Type;

/// Lower a `cs_typer::Type` to the narrower `cs_rir::Type`.
///
/// Lossy by design — see the module docstring for rules.
pub fn lower(t: &Type) -> cs_rir::Type {
    match t {
        Type::Fixnum => cs_rir::Type::Fixnum,
        Type::Flonum => cs_rir::Type::Flonum,
        Type::Boolean => cs_rir::Type::Boolean,
        Type::Character => cs_rir::Type::Character,
        Type::Symbol => cs_rir::Type::Symbol,
        Type::Pair => cs_rir::Type::Pair,
        Type::Vector => cs_rir::Type::Vector,
        Type::String => cs_rir::Type::String,
        Type::ByteVector => cs_rir::Type::ByteVector,
        Type::Procedure => cs_rir::Type::Procedure,
        Type::Null => cs_rir::Type::Null,
        Type::Any | Type::Never => cs_rir::Type::Any,
        Type::Procedure_(_) => cs_rir::Type::Procedure,
        // Containers — element type can't propagate through
        // cs_rir's flat atom set.
        Type::Listof(_) | Type::Vectorof(_) => cs_rir::Type::Any,
        Type::Union(members) => {
            // Lower each member, then check whether they all
            // agree. If so, the union has a single runtime
            // representation; if not, fall back to Any.
            let mut iter = members.iter().map(lower);
            let Some(first) = iter.next() else {
                return cs_rir::Type::Any;
            };
            if iter.all(|x| x == first) {
                first
            } else {
                cs_rir::Type::Any
            }
        }
    }
}

/// Walk an `AnnotationTable` and produce `param_type_hints`
/// keyed by lambda span.
///
/// For each `LambdaAnnotation`, emit a `Vec<cs_rir::Type>`
/// parallel to the declared `param_types`. Unannotated slots
/// (None in `param_types`) lower to `Any`. The Vec's length
/// equals the recorded `param_types` length — callers should
/// truncate/pad to match the `CompiledLambda`'s actual `params`
/// count (the typer doesn't see the compiled form).
pub fn param_hints_from_table(table: &AnnotationTable) -> HashMap<Span, Vec<cs_rir::Type>> {
    table
        .lambdas
        .iter()
        .map(|(span, ann)| {
            let v: Vec<cs_rir::Type> = ann
                .param_types
                .iter()
                .map(|opt| opt.as_ref().map(lower).unwrap_or(cs_rir::Type::Any))
                .collect();
            (*span, v)
        })
        .collect()
}

/// Walk `AnnotationTable.top_level` and produce
/// `param_type_hints` keyed by the bound name.
///
/// This is the form the AOT pipeline (cs-cli `aot --multi`)
/// and the JIT tier-up hook can consume: at compile-time they
/// know the lambda's bound symbol (via the surrounding `Set`
/// form's name), but not the source `Span` of the inner
/// Lambda CoreExpr.
///
/// Only ascriptions whose declared type is a `Procedure_` are
/// included; non-procedure ascriptions (e.g., `(: x Fixnum)`)
/// don't contribute hints. Each procedure's `params` are
/// lowered via [`lower`]; the result's length equals
/// `params.len()` and callers should pad with `Any` or
/// truncate to match the `CompiledLambda`'s actual `params`
/// count.
pub fn hints_by_name(table: &AnnotationTable) -> HashMap<Symbol, Vec<cs_rir::Type>> {
    table
        .top_level
        .iter()
        .filter_map(|ta| match &ta.type_ann {
            Type::Procedure_(pt) => {
                let v: Vec<cs_rir::Type> = pt.params.iter().map(lower).collect();
                Some((ta.name, v))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::LambdaAnnotation;
    use cs_diag::FileId;

    fn span(start: u32, end: u32) -> Span {
        Span::new(FileId(0), start, end)
    }

    // ---- lower(): atom 1-1 mapping ----

    #[test]
    fn lower_atoms_map_one_to_one() {
        let cases = [
            (Type::Fixnum, cs_rir::Type::Fixnum),
            (Type::Flonum, cs_rir::Type::Flonum),
            (Type::Boolean, cs_rir::Type::Boolean),
            (Type::Character, cs_rir::Type::Character),
            (Type::Symbol, cs_rir::Type::Symbol),
            (Type::Pair, cs_rir::Type::Pair),
            (Type::Vector, cs_rir::Type::Vector),
            (Type::String, cs_rir::Type::String),
            (Type::ByteVector, cs_rir::Type::ByteVector),
            (Type::Procedure, cs_rir::Type::Procedure),
            (Type::Null, cs_rir::Type::Null),
        ];
        for (typer_t, rir_t) in cases {
            assert_eq!(lower(&typer_t), rir_t, "atom {typer_t:?}");
        }
    }

    #[test]
    fn lower_any_and_never_to_any() {
        assert_eq!(lower(&Type::Any), cs_rir::Type::Any);
        assert_eq!(lower(&Type::Never), cs_rir::Type::Any);
    }

    #[test]
    fn lower_procedure_underscore_to_procedure() {
        let pt = Type::Procedure_(Box::new(crate::types::ProcType {
            params: vec![Type::Fixnum],
            return_type: Type::Fixnum,
            rest: None,
            filter: None,
        }));
        assert_eq!(lower(&pt), cs_rir::Type::Procedure);
    }

    #[test]
    fn lower_containers_to_any() {
        assert_eq!(
            lower(&Type::Listof(Box::new(Type::Fixnum))),
            cs_rir::Type::Any
        );
        assert_eq!(
            lower(&Type::Vectorof(Box::new(Type::Fixnum))),
            cs_rir::Type::Any
        );
    }

    // ---- lower(): unions ----

    #[test]
    fn lower_union_collapses_to_any_when_mixed() {
        let u = Type::union(vec![Type::Fixnum, Type::Flonum]);
        assert_eq!(lower(&u), cs_rir::Type::Any);
    }

    #[test]
    fn lower_union_keeps_atom_when_all_agree_at_rir_level() {
        // Listof Fixnum and Vectorof Fixnum both lower to Any,
        // so a union of them is also Any. Pair and Listof both
        // lower to different cs_rir variants — Pair → Pair,
        // Listof → Any → still mixed → Any.
        let u = Type::union(vec![Type::Listof(Box::new(Type::Fixnum)), Type::Pair]);
        assert_eq!(lower(&u), cs_rir::Type::Any);
    }

    // ---- param_hints_from_table ----

    #[test]
    fn hints_from_empty_table_are_empty() {
        let table = AnnotationTable::new();
        let hints = param_hints_from_table(&table);
        assert!(hints.is_empty());
    }

    #[test]
    fn hints_for_typed_lambda_lower_each_param() {
        let mut table = AnnotationTable::new();
        let s = span(10, 20);
        table.lambdas.insert(
            s,
            LambdaAnnotation {
                param_types: vec![Some(Type::Fixnum), Some(Type::Flonum), None],
                return_type: Some(Type::Fixnum),
                rest_type: None,
            },
        );
        let hints = param_hints_from_table(&table);
        assert_eq!(hints.len(), 1);
        let v = hints.get(&s).unwrap();
        assert_eq!(
            v,
            &vec![
                cs_rir::Type::Fixnum,
                cs_rir::Type::Flonum,
                cs_rir::Type::Any
            ]
        );
    }

    #[test]
    fn hints_collapse_union_param_to_any() {
        let mut table = AnnotationTable::new();
        let s = span(0, 5);
        table.lambdas.insert(
            s,
            LambdaAnnotation {
                param_types: vec![Some(Type::union(vec![Type::Fixnum, Type::Flonum]))],
                return_type: None,
                rest_type: None,
            },
        );
        let hints = param_hints_from_table(&table);
        let v = hints.get(&s).unwrap();
        assert_eq!(v, &vec![cs_rir::Type::Any]);
    }

    // ---- hints_by_name (Phase 5.2) ----

    #[test]
    fn hints_by_name_from_top_level_ascription() {
        let mut table = AnnotationTable::new();
        let name = cs_core::Symbol(7);
        table.top_level.push(crate::annotate::TopLevelAnnotation {
            name,
            type_ann: Type::Procedure_(Box::new(crate::types::ProcType {
                params: vec![Type::Fixnum, Type::Flonum],
                return_type: Type::Fixnum,
                rest: None,
                filter: None,
            })),
            ascription_span: span(0, 5),
        });
        let h = hints_by_name(&table);
        let v = h.get(&name).unwrap();
        assert_eq!(v, &vec![cs_rir::Type::Fixnum, cs_rir::Type::Flonum]);
    }

    #[test]
    fn hints_by_name_skips_non_procedure_ascriptions() {
        // `(: PI Flonum)` doesn't carry param hints — it's a
        // value ascription, not a procedure type.
        let mut table = AnnotationTable::new();
        let pi = cs_core::Symbol(42);
        table.top_level.push(crate::annotate::TopLevelAnnotation {
            name: pi,
            type_ann: Type::Flonum,
            ascription_span: span(0, 5),
        });
        let h = hints_by_name(&table);
        assert!(h.is_empty(), "non-proc ascription should not appear: {h:?}");
    }
}
