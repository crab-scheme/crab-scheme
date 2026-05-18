//! Phase 4 iter 1 — type → contract lowering tests.
//!
//! Verifies the Rust translator produces well-formed Scheme
//! contract expressions. End-to-end integration with the
//! contracts library at runtime is a separate iter; this layer
//! just owns the string lowering.

use cs_typer::contract_lowering::type_to_contract;
use cs_typer::types::{ProcType, Type};

// ---- atomic types ----

#[test]
fn atomic_types_map_to_predicates() {
    assert_eq!(type_to_contract(&Type::Fixnum), "integer?");
    assert_eq!(type_to_contract(&Type::Flonum), "real?");
    assert_eq!(type_to_contract(&Type::Boolean), "boolean?");
    assert_eq!(type_to_contract(&Type::Character), "char?");
    assert_eq!(type_to_contract(&Type::Symbol), "symbol?");
    assert_eq!(type_to_contract(&Type::Pair), "pair?");
    assert_eq!(type_to_contract(&Type::Vector), "vector?");
    assert_eq!(type_to_contract(&Type::String), "string?");
    assert_eq!(type_to_contract(&Type::ByteVector), "bytevector?");
    assert_eq!(type_to_contract(&Type::Procedure), "procedure?");
    assert_eq!(type_to_contract(&Type::Null), "null?");
}

#[test]
fn any_lowers_to_any_slash_c() {
    assert_eq!(type_to_contract(&Type::Any), "any/c");
}

#[test]
fn never_lowers_to_none_slash_c() {
    assert_eq!(type_to_contract(&Type::Never), "none/c");
}

// ---- unions ----

#[test]
fn union_lowers_to_or_slash_c() {
    let ty = Type::union(vec![Type::Fixnum, Type::Flonum]);
    let s = type_to_contract(&ty);
    // union normalization sorts members; just verify both
    // predicates appear inside an or/c form.
    assert!(s.starts_with("(or/c "), "got: {}", s);
    assert!(s.contains("integer?"), "got: {}", s);
    assert!(s.contains("real?"), "got: {}", s);
    assert!(s.ends_with(')'), "got: {}", s);
}

#[test]
fn union_with_three_members() {
    let ty = Type::union(vec![Type::Fixnum, Type::Boolean, Type::String]);
    let s = type_to_contract(&ty);
    assert!(s.starts_with("(or/c "));
    assert!(s.contains("integer?"));
    assert!(s.contains("boolean?"));
    assert!(s.contains("string?"));
}

#[test]
fn union_with_any_member_still_emits_full_or() {
    // The translator doesn't optimize `(U Any T)` to `any/c`
    // (the typer's union constructor may or may not — we just
    // pass-through whatever Type we're given).
    let ty = Type::union(vec![Type::Any, Type::Fixnum]);
    let s = type_to_contract(&ty);
    // Whatever the constructor canonicalized to, we lower it
    // faithfully. If it canonicalized to bare Any, accept that.
    assert!(s == "any/c" || s.contains("any/c"), "got: {}", s);
}

// ---- procedures (arrow) ----

#[test]
fn procedure_lowers_to_arrow() {
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Fixnum, Type::String],
        return_type: Type::Boolean,
        rest: None,
        filter: None,
    }));
    assert_eq!(type_to_contract(&ty), "(-> integer? string? boolean?)");
}

#[test]
fn nullary_procedure_arrow() {
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    assert_eq!(type_to_contract(&ty), "(-> integer?)");
}

#[test]
fn procedure_with_any_lowers_naturally() {
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Any],
        return_type: Type::Any,
        rest: None,
        filter: None,
    }));
    assert_eq!(type_to_contract(&ty), "(-> any/c any/c)");
}

// ---- procedures with rest-arg (variadic tail) → (->* ...) ----

#[test]
fn procedure_with_rest_lowers_to_arrow_star() {
    // (-> String Number ... Boolean) — one mandatory String,
    // variadic Number tail, returns Boolean.
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::String],
        return_type: Type::Boolean,
        rest: Some(Type::Fixnum),
        filter: None,
    }));
    assert_eq!(type_to_contract(&ty), "(->* (string?) integer? boolean?)");
}

#[test]
fn nullary_mandatory_procedure_with_rest() {
    // (-> Number ... Number) — no mandatories, all variadic.
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![],
        return_type: Type::Fixnum,
        rest: Some(Type::Fixnum),
        filter: None,
    }));
    assert_eq!(type_to_contract(&ty), "(->* () integer? integer?)");
}

#[test]
fn procedure_with_multiple_mandatories_and_rest() {
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::String, Type::Boolean],
        return_type: Type::Any,
        rest: Some(Type::Any),
        filter: None,
    }));
    assert_eq!(
        type_to_contract(&ty),
        "(->* (string? boolean?) any/c any/c)"
    );
}

// ---- containers ----

#[test]
fn listof_lowers_to_list_of_c() {
    let ty = Type::Listof(Box::new(Type::Fixnum));
    assert_eq!(type_to_contract(&ty), "(list-of/c integer?)");
}

#[test]
fn vectorof_lowers_to_vector_of_c() {
    let ty = Type::Vectorof(Box::new(Type::String));
    assert_eq!(type_to_contract(&ty), "(vector-of/c string?)");
}

// ---- polymorphism (erased to Any) ----

#[test]
fn forall_lowers_body_with_vars_as_any() {
    // (All (T) (-> T T)) — body is (-> Var(T) Var(T)). Vars
    // become any/c.
    let sym = cs_core::Symbol(0);
    let body = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Var(sym)],
        return_type: Type::Var(sym),
        rest: None,
        filter: None,
    }));
    let ty = Type::Forall(vec![sym], Box::new(body));
    assert_eq!(type_to_contract(&ty), "(-> any/c any/c)");
}

#[test]
fn bare_type_var_lowers_to_any() {
    let sym = cs_core::Symbol(0);
    assert_eq!(type_to_contract(&Type::Var(sym)), "any/c");
}

// ---- nesting ----

#[test]
fn nested_arrow_lowers_correctly() {
    // (-> (-> Fixnum Fixnum) Fixnum) — higher-order
    let inner = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Fixnum],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    let outer = Type::Procedure_(Box::new(ProcType {
        params: vec![inner],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    assert_eq!(
        type_to_contract(&outer),
        "(-> (-> integer? integer?) integer?)"
    );
}

#[test]
fn union_inside_arrow() {
    let dom = Type::union(vec![Type::Fixnum, Type::Flonum]);
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![dom],
        return_type: Type::Boolean,
        rest: None,
        filter: None,
    }));
    let s = type_to_contract(&ty);
    assert!(s.starts_with("(-> (or/c "), "got: {}", s);
    assert!(s.ends_with(" boolean?)"), "got: {}", s);
}

#[test]
fn arrow_inside_union() {
    let arrow = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Fixnum],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    let ty = Type::union(vec![Type::Procedure, arrow]);
    let s = type_to_contract(&ty);
    assert!(s.starts_with("(or/c "), "got: {}", s);
    assert!(s.contains("procedure?"), "got: {}", s);
    assert!(s.contains("(-> integer? integer?)"), "got: {}", s);
}

// ---- regression / well-formedness ----

#[test]
fn output_is_well_formed_parenthesized() {
    // Sanity: every emitted string with a `(` must have matching
    // `)`. Quick eyeball check across a representative sample.
    let cases = vec![
        Type::Fixnum,
        Type::Any,
        Type::union(vec![Type::Fixnum, Type::Flonum]),
        Type::Listof(Box::new(Type::String)),
        Type::Procedure_(Box::new(ProcType {
            params: vec![Type::Fixnum, Type::Fixnum],
            return_type: Type::Fixnum,
            rest: None,
            filter: None,
        })),
    ];
    for c in cases {
        let s = type_to_contract(&c);
        let opens = s.chars().filter(|c| *c == '(').count();
        let closes = s.chars().filter(|c| *c == ')').count();
        assert_eq!(opens, closes, "mismatched parens in: {}", s);
    }
}
