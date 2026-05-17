//! Parser from `cs_parse::Datum` → `TypeAnn` annotation AST.
//!
//! The typer's annotation syntax piggybacks on standard Scheme
//! S-expression syntax — `:` is a valid Scheme identifier
//! (R6RS `<initial>` includes `:`), so no lexer extension is
//! needed. The expander (iter 1.4) recognizes the `:` symbol in
//! specific positions: ascription forms `(: name type)`, typed
//! lambda params `[name : type]`, typed binding heads
//! `[(name : type) value]`. Each parses its type-position
//! Datum via [`parse_type_ann`].
//!
//! Iter 1.2 ships the AST + the parser. Recognized type
//! constructors:
//!
//! ```text
//! atom      ::= Fixnum | Flonum | Boolean | Character | Symbol
//!             | Pair | Vector | String | ByteVector | Procedure
//!             | Null | Any | Never
//! type      ::= atom
//!             | (U type ...)              ; union
//!             | (-> type ... return)      ; procedure
//!             | (Listof type)             ; homogeneous list
//!             | (Vectorof type)           ; homogeneous vector
//! ```
//!
//! Iter 1.3 will add `define-type` alias resolution; for now
//! every type-position Datum must use one of the constructors
//! above directly.

use crate::types::{ProcType, Type};

/// Source-level type annotation AST. Distinct from
/// [`crate::types::Type`] so the parser can preserve any
/// not-yet-resolved aliases or syntactic detail the checker
/// might want. For Phase 1 it's structurally identical to
/// `Type`; later phases may add `Alias(Symbol)` etc.
pub type TypeAnn = Type;

/// Error from parsing a `Datum` as a type annotation.
#[derive(Debug, Clone)]
pub enum TypeAnnError {
    /// The Datum at this position is not a recognized type
    /// constructor or atomic name. Carries a human-readable
    /// description of what was found.
    UnknownType(String),
    /// `(U)` (empty union), `(->)` (no return type),
    /// `(Listof a b)` (too many args), etc. Carries the
    /// constructor name + a description of the malformed
    /// shape.
    MalformedConstructor(&'static str, String),
}

impl TypeAnnError {
    pub fn message(&self) -> String {
        match self {
            TypeAnnError::UnknownType(s) => format!("unknown type: {s}"),
            TypeAnnError::MalformedConstructor(c, s) => {
                format!("malformed type constructor `{c}`: {s}")
            }
        }
    }
}

/// Reduced-shape view of a Datum the parser walks. Decoupled
/// from `cs_parse::Datum` to keep this crate dep-light (and to
/// document exactly what shapes the parser needs to see —
/// symbols, lists, and that's it for now). Callers in
/// cs-expand convert from `Datum` to `TypeDatum` via the
/// helpers in iter 1.4.
#[derive(Debug, Clone)]
pub enum TypeDatum {
    /// A bare identifier (atomic type name or constructor head).
    Sym(String),
    /// A list. `( elt1 elt2 ... )`. Empty list is also a TypeDatum::List(vec![]).
    List(Vec<TypeDatum>),
}

/// Parse a `TypeDatum` as a type annotation, recursively.
///
/// Equivalent to [`parse_type_ann_with_aliases`] with an empty
/// alias table. Use the aliases-aware form when callers have
/// `define-type` aliases in scope.
pub fn parse_type_ann(d: &TypeDatum) -> Result<TypeAnn, TypeAnnError> {
    parse_type_ann_with_aliases(d, &[])
}

/// Parse a `TypeDatum` with an alias table in scope.
///
/// When an atom name doesn't match a built-in (Fixnum, …) the
/// parser consults `aliases` (by name) and substitutes the
/// alias's target type. This is how `define-type` references
/// resolve at parse time — Phase 3.5.
///
/// `aliases` is a slice rather than a HashMap because: (a) the
/// table is tiny in practice (a handful per program), (b) the
/// caller (extract.rs) is already accumulating aliases as a
/// `Vec<TypeAlias>` and prefers not to maintain a separate
/// hash, and (c) ordering matters — a later `define-type` may
/// reference an earlier one, so we look up from the end.
pub fn parse_type_ann_with_aliases(
    d: &TypeDatum,
    aliases: &[(String, Type)],
) -> Result<TypeAnn, TypeAnnError> {
    match d {
        TypeDatum::Sym(name) => atom_from_name(name, aliases),
        TypeDatum::List(elements) => parse_list(elements, aliases),
    }
}

fn atom_from_name(name: &str, aliases: &[(String, Type)]) -> Result<TypeAnn, TypeAnnError> {
    match name {
        "Fixnum" => Ok(Type::Fixnum),
        "Flonum" => Ok(Type::Flonum),
        "Boolean" => Ok(Type::Boolean),
        "Character" => Ok(Type::Character),
        "Symbol" => Ok(Type::Symbol),
        "Pair" => Ok(Type::Pair),
        "Vector" => Ok(Type::Vector),
        "String" => Ok(Type::String),
        "ByteVector" => Ok(Type::ByteVector),
        "Procedure" => Ok(Type::Procedure),
        "Null" => Ok(Type::Null),
        "Any" => Ok(Type::Any),
        "Never" => Ok(Type::Never),
        _ => {
            // Try aliases (most recent first, so later defs
            // shadow earlier ones with the same name — Scheme
            // convention).
            for (a_name, a_type) in aliases.iter().rev() {
                if a_name == name {
                    return Ok(a_type.clone());
                }
            }
            Err(TypeAnnError::UnknownType(name.to_string()))
        }
    }
}

fn parse_list(elements: &[TypeDatum], aliases: &[(String, Type)]) -> Result<TypeAnn, TypeAnnError> {
    if elements.is_empty() {
        return Err(TypeAnnError::MalformedConstructor(
            "()",
            "empty list is not a type".into(),
        ));
    }
    let head_name = match &elements[0] {
        TypeDatum::Sym(s) => s.as_str(),
        TypeDatum::List(_) => {
            return Err(TypeAnnError::UnknownType(
                "type-constructor head must be a symbol".into(),
            ));
        }
    };
    let rest = &elements[1..];
    match head_name {
        "U" => parse_union(rest, aliases),
        "->" => parse_arrow(rest, aliases),
        "Listof" => parse_unary("Listof", rest, Type::Listof, aliases),
        "Vectorof" => parse_unary("Vectorof", rest, Type::Vectorof, aliases),
        _ => Err(TypeAnnError::UnknownType(format!(
            "unknown type constructor: {head_name}"
        ))),
    }
}

fn parse_union(args: &[TypeDatum], aliases: &[(String, Type)]) -> Result<TypeAnn, TypeAnnError> {
    let members: Result<Vec<Type>, _> = args
        .iter()
        .map(|d| parse_type_ann_with_aliases(d, aliases))
        .collect();
    Ok(Type::union(members?))
}

fn parse_arrow(args: &[TypeDatum], aliases: &[(String, Type)]) -> Result<TypeAnn, TypeAnnError> {
    // `(-> ret)` is a thunk → `(-> Never ret)` isn't right. We
    // require at least one element (the return type) — the
    // "params" of a 0-arg arrow are simply the empty params
    // vec. So `(-> Fixnum)` is a thunk returning Fixnum.
    //
    // Phase 3.4: `(-> A B C ... R)` is a rest-arg arrow:
    //   params = [A, B]
    //   rest   = C       (the type of each trailing arg)
    //   return = R
    // The literal symbol `...` marks the rest position.
    if args.is_empty() {
        return Err(TypeAnnError::MalformedConstructor(
            "->",
            "needs at least a return type, e.g. `(-> Fixnum)`".into(),
        ));
    }
    let dots_pos = args
        .iter()
        .position(|d| matches!(d, TypeDatum::Sym(s) if s == "..."));
    let (params, rest, return_d) = match dots_pos {
        None => {
            let (ret, params) = args.split_last().unwrap();
            (params, None, ret)
        }
        Some(i) => {
            if i == 0 {
                return Err(TypeAnnError::MalformedConstructor(
                    "->",
                    "`...` must be preceded by a type, e.g. `(-> Fixnum ... Fixnum)`".into(),
                ));
            }
            if i + 1 >= args.len() {
                return Err(TypeAnnError::MalformedConstructor(
                    "->",
                    "`...` must be followed by a return type".into(),
                ));
            }
            if args.len() - (i + 1) != 1 {
                return Err(TypeAnnError::MalformedConstructor(
                    "->",
                    "exactly one return type must follow `...`".into(),
                ));
            }
            let fixed = &args[0..i - 1];
            let rest_d = &args[i - 1];
            let ret_d = &args[i + 1];
            (fixed, Some(rest_d), ret_d)
        }
    };
    let parsed_params: Result<Vec<Type>, _> = params
        .iter()
        .map(|d| parse_type_ann_with_aliases(d, aliases))
        .collect();
    let parsed_rest = rest
        .map(|d| parse_type_ann_with_aliases(d, aliases))
        .transpose()?;
    let return_type = parse_type_ann_with_aliases(return_d, aliases)?;
    Ok(Type::Procedure_(Box::new(ProcType {
        params: parsed_params?,
        return_type,
        rest: parsed_rest,
        filter: None,
    })))
}

fn parse_unary(
    name: &'static str,
    args: &[TypeDatum],
    wrap: fn(Box<Type>) -> Type,
    aliases: &[(String, Type)],
) -> Result<TypeAnn, TypeAnnError> {
    if args.len() != 1 {
        return Err(TypeAnnError::MalformedConstructor(
            name,
            format!("expected 1 argument, got {}", args.len()),
        ));
    }
    Ok(wrap(Box::new(parse_type_ann_with_aliases(
        &args[0], aliases,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> TypeDatum {
        TypeDatum::Sym(s.to_string())
    }
    fn list(elts: Vec<TypeDatum>) -> TypeDatum {
        TypeDatum::List(elts)
    }

    #[test]
    fn atoms_parse() {
        assert_eq!(parse_type_ann(&sym("Fixnum")).unwrap(), Type::Fixnum);
        assert_eq!(parse_type_ann(&sym("Any")).unwrap(), Type::Any);
        assert_eq!(parse_type_ann(&sym("Never")).unwrap(), Type::Never);
    }

    #[test]
    fn unknown_atom_errors() {
        assert!(matches!(
            parse_type_ann(&sym("Zorblax")),
            Err(TypeAnnError::UnknownType(_))
        ));
    }

    #[test]
    fn union_parses_and_normalizes() {
        // (U Fixnum Flonum)
        let d = list(vec![sym("U"), sym("Fixnum"), sym("Flonum")]);
        assert_eq!(
            parse_type_ann(&d).unwrap(),
            Type::Union(vec![Type::Fixnum, Type::Flonum])
        );
    }

    #[test]
    fn union_singleton_collapses() {
        let d = list(vec![sym("U"), sym("Fixnum")]);
        assert_eq!(parse_type_ann(&d).unwrap(), Type::Fixnum);
    }

    #[test]
    fn arrow_thunk_parses() {
        // (-> Fixnum) → 0-arg procedure returning Fixnum
        let d = list(vec![sym("->"), sym("Fixnum")]);
        let got = parse_type_ann(&d).unwrap();
        assert_eq!(
            got,
            Type::Procedure_(Box::new(ProcType {
                params: vec![],
                return_type: Type::Fixnum,
                rest: None,
                filter: None,
            }))
        );
    }

    #[test]
    fn arrow_with_rest_parses() {
        // `(-> Fixnum ... Fixnum)` — variadic Fixnum → Fixnum.
        let parsed = parse_type_ann(&list(vec![
            sym("->"),
            sym("Fixnum"),
            sym("..."),
            sym("Fixnum"),
        ]))
        .unwrap();
        match parsed {
            Type::Procedure_(pt) => {
                assert!(pt.params.is_empty(), "fixed params should be empty");
                assert_eq!(pt.rest, Some(Type::Fixnum));
                assert_eq!(pt.return_type, Type::Fixnum);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn arrow_with_fixed_then_rest_parses() {
        // `(-> A B C ... R)` → fixed=[A,B], rest=C, return=R.
        let parsed = parse_type_ann(&list(vec![
            sym("->"),
            sym("Fixnum"),
            sym("String"),
            sym("Boolean"),
            sym("..."),
            sym("Fixnum"),
        ]))
        .unwrap();
        match parsed {
            Type::Procedure_(pt) => {
                assert_eq!(pt.params, vec![Type::Fixnum, Type::String]);
                assert_eq!(pt.rest, Some(Type::Boolean));
                assert_eq!(pt.return_type, Type::Fixnum);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn arrow_with_dots_alone_errors() {
        // `(-> ... Fixnum)` — `...` at index 0 has no preceding
        // type. Should surface a MalformedConstructor.
        let parsed = parse_type_ann(&list(vec![sym("->"), sym("..."), sym("Fixnum")]));
        assert!(matches!(
            parsed,
            Err(TypeAnnError::MalformedConstructor("->", _))
        ));
    }

    #[test]
    fn arrow_with_dots_no_return_errors() {
        // `(-> Fixnum ...)` — `...` not followed by a return.
        let parsed = parse_type_ann(&list(vec![sym("->"), sym("Fixnum"), sym("...")]));
        assert!(matches!(
            parsed,
            Err(TypeAnnError::MalformedConstructor("->", _))
        ));
    }

    #[test]
    fn arrow_with_params_parses() {
        // (-> Fixnum Fixnum Fixnum) → (Fixnum, Fixnum) → Fixnum
        let d = list(vec![sym("->"), sym("Fixnum"), sym("Fixnum"), sym("Fixnum")]);
        let got = parse_type_ann(&d).unwrap();
        assert_eq!(
            got,
            Type::Procedure_(Box::new(ProcType {
                params: vec![Type::Fixnum, Type::Fixnum],
                return_type: Type::Fixnum,
                rest: None,
                filter: None,
            }))
        );
    }

    #[test]
    fn arrow_empty_errors() {
        let d = list(vec![sym("->")]);
        assert!(matches!(
            parse_type_ann(&d),
            Err(TypeAnnError::MalformedConstructor("->", _))
        ));
    }

    #[test]
    fn listof_parses() {
        // (Listof Fixnum)
        let d = list(vec![sym("Listof"), sym("Fixnum")]);
        assert_eq!(
            parse_type_ann(&d).unwrap(),
            Type::Listof(Box::new(Type::Fixnum))
        );
    }

    #[test]
    fn vectorof_parses() {
        let d = list(vec![sym("Vectorof"), sym("Flonum")]);
        assert_eq!(
            parse_type_ann(&d).unwrap(),
            Type::Vectorof(Box::new(Type::Flonum))
        );
    }

    #[test]
    fn listof_wrong_arity_errors() {
        let d = list(vec![sym("Listof"), sym("Fixnum"), sym("Flonum")]);
        assert!(matches!(
            parse_type_ann(&d),
            Err(TypeAnnError::MalformedConstructor("Listof", _))
        ));
    }

    #[test]
    fn nested_arrow_in_union() {
        // (U Fixnum (-> Fixnum Fixnum))
        let d = list(vec![
            sym("U"),
            sym("Fixnum"),
            list(vec![sym("->"), sym("Fixnum"), sym("Fixnum")]),
        ]);
        let got = parse_type_ann(&d).unwrap();
        assert_eq!(
            got,
            Type::Union(vec![
                Type::Fixnum,
                Type::Procedure_(Box::new(ProcType {
                    params: vec![Type::Fixnum],
                    return_type: Type::Fixnum,
                    rest: None,
                    filter: None,
                })),
            ])
        );
    }

    #[test]
    fn empty_list_errors() {
        let d = list(vec![]);
        assert!(matches!(
            parse_type_ann(&d),
            Err(TypeAnnError::MalformedConstructor("()", _))
        ));
    }
}
