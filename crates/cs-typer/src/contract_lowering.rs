//! Phase 4 (R6RS++ §12) — type → contract lowering.
//!
//! Typed Racket's strategy for mixed-typed/untyped code: inside a
//! typed module, type-checked code runs without runtime checks
//! (static types are the safety guarantee); at the boundary
//! between typed and untyped code, contracts are inserted to
//! verify that untyped callers respect the typed signatures.
//!
//! This module is the **lowering step**: given a `cs_typer::Type`,
//! produce a Scheme source expression that, when evaluated in an
//! environment with `lib/contract/contract.scm` loaded, builds the
//! corresponding contract value.
//!
//! ## Mapping
//!
//! | Type                  | Contract expression                       |
//! |-----------------------|-------------------------------------------|
//! | Fixnum                | `integer?`                                |
//! | Flonum                | `real?`                                   |
//! | Boolean               | `boolean?`                                |
//! | Character             | `char?`                                   |
//! | Symbol                | `symbol?`                                 |
//! | Pair                  | `pair?`                                   |
//! | Vector                | `vector?`                                 |
//! | String                | `string?`                                 |
//! | ByteVector            | `bytevector?`                             |
//! | Procedure             | `procedure?`                              |
//! | Null                  | `null?`                                   |
//! | Any                   | `any/c`                                   |
//! | Never                 | `none/c`                                  |
//! | Union(ts)             | `(or/c ...lowered_ts)`                    |
//! | Procedure_({p,r,...}) | `(-> ...lowered_p lowered_r)`             |
//! | Listof(t)             | `(list-of/c lowered_t)`                   |
//! | Vectorof(t)           | `(vector-of/c lowered_t)`                 |
//! | Forall(_, body)       | lowered body (vars become Any)            |
//! | Var(_)                | `any/c`                                   |
//!
//! `list-of/c` and `vector-of/c` are NOT in the current contract
//! library (Phase 2B.5 shipped `list/c` for fixed-length lists
//! only). They're emitted here as the natural lowering and need a
//! contract-lib addition before lowered code can actually run —
//! that's a follow-up iter.
//!
//! `Procedure_.rest` (variadic tail) is currently dropped — the
//! arrow form doesn't express "variadic of type T". Encoded as a
//! TODO; the only correct fully-general arrow needs an extension
//! to the contract grammar (e.g., `(->* mandatory rest? rng)`).

use crate::types::{ProcType, Type};

/// Lower a `Type` to a Scheme contract expression as a String.
///
/// The output is meant to be embedded directly into Scheme source
/// (e.g., inside an `apply-contract` call). It's well-formed
/// Scheme — parenthesized as needed for compound forms.
pub fn type_to_contract(ty: &Type) -> String {
    match ty {
        Type::Fixnum => "integer?".into(),
        Type::Flonum => "real?".into(),
        Type::Boolean => "boolean?".into(),
        Type::Character => "char?".into(),
        Type::Symbol => "symbol?".into(),
        Type::Pair => "pair?".into(),
        Type::Vector => "vector?".into(),
        Type::String => "string?".into(),
        Type::ByteVector => "bytevector?".into(),
        Type::Procedure => "procedure?".into(),
        Type::Null => "null?".into(),
        Type::Any => "any/c".into(),
        Type::Never => "none/c".into(),

        Type::Union(ts) => {
            let mut s = String::from("(or/c");
            for t in ts {
                s.push(' ');
                s.push_str(&type_to_contract(t));
            }
            s.push(')');
            s
        }

        Type::Procedure_(proc) => proc_type_to_arrow(proc),

        // `list-of/c` and `vector-of/c` are emitted as the natural
        // lowering; the contract library will need them added.
        // Until then, downstream code can short-circuit Listof/
        // Vectorof to a plain `list?` / `vector?` predicate if the
        // element-type check is acceptable to drop.
        Type::Listof(elem) => format!("(list-of/c {})", type_to_contract(elem)),
        Type::Vectorof(elem) => format!("(vector-of/c {})", type_to_contract(elem)),

        // Polymorphism is treated as `Any` at the contract level
        // — there's no way to check "the same type at every
        // call site" purely dynamically. Forall body's free type
        // variables also lower to `any/c`.
        Type::Forall(_vars, body) => type_to_contract(body),
        Type::Var(_) => "any/c".into(),
    }
}

fn proc_type_to_arrow(proc: &ProcType) -> String {
    let mut s = String::from("(->");
    for p in &proc.params {
        s.push(' ');
        s.push_str(&type_to_contract(p));
    }
    // proc.rest is dropped — the contract grammar doesn't yet
    // express variadic tails. Documented at module top.
    s.push(' ');
    s.push_str(&type_to_contract(&proc.return_type));
    s.push(')');
    s
}
