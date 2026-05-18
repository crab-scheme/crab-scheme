//! Phase 4 iter 2 — end-to-end smoke test of contract lowering.
//!
//! Round-trips through the contracts library: cs-typer Type →
//! Scheme contract source → loaded into a runtime with
//! lib/contract/contract.scm → applied to a procedure → verified
//! at the runtime level.
//!
//! This is the foundational test that proves iter 1's lowering
//! produces source that actually runs with iter 2's expanded
//! contract library (list-of/c, vector-of/c).

use std::path::PathBuf;

use cs_core::WriteMode;
use cs_runtime::Runtime;
use cs_typer::contract_lowering::type_to_contract;
use cs_typer::types::{ProcType, Type};

fn disp(rt: &Runtime, v: &cs_core::Value) -> String {
    rt.format_value(v, WriteMode::Display)
}

fn rt_with_contract() -> Runtime {
    // Locate lib/contract relative to THIS test's manifest.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/contract/contract.scm");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {:?}: {}", path, e));
    let mut rt = Runtime::new();
    rt.eval_str("<contract>", &src).expect("load contract.scm");
    rt
}

// ---- atomic types round-trip ----

#[test]
fn lowered_fixnum_contract_accepts_integer() {
    let mut rt = rt_with_contract();
    let c = type_to_contract(&Type::Fixnum);
    // (apply-contract (-> integer? integer?) (lambda (x) x) 'id)
    let src = format!(
        "(define id (apply-contract (-> {} {}) (lambda (x) x) 'id))
         (id 42)",
        c, c
    );
    let v = rt.eval_str("<t>", &src).unwrap();
    assert_eq!(disp(&rt, &v), "42");
}

#[test]
fn lowered_fixnum_contract_rejects_non_integer() {
    let mut rt = rt_with_contract();
    let c = type_to_contract(&Type::Fixnum);
    rt.eval_str(
        "<t>",
        &format!(
            "(define id (apply-contract (-> {} {}) (lambda (x) x) 'id))",
            c, c
        ),
    )
    .unwrap();
    let err = rt
        .eval_str("<t>", "(id 'not-an-int)")
        .expect_err("symbol violates integer?");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}

// ---- procedure round-trip ----

#[test]
fn lowered_procedure_type_wraps_actual_proc() {
    let mut rt = rt_with_contract();
    // (-> Fixnum Fixnum Fixnum) lowers to (-> integer? integer? integer?)
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Fixnum, Type::Fixnum],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    let c = type_to_contract(&ty);
    let src = format!(
        "(define add (apply-contract {} (lambda (a b) (+ a b)) 'add))
         (add 3 4)",
        c
    );
    let v = rt.eval_str("<t>", &src).unwrap();
    assert_eq!(disp(&rt, &v), "7");
}

// ---- listof round-trip ----

#[test]
fn lowered_listof_runs_with_list_of_c_helper() {
    let mut rt = rt_with_contract();
    let ty = Type::Listof(Box::new(Type::Fixnum));
    let c = type_to_contract(&ty);
    let src = format!(
        "(define accept ({} (list 1 2 3)))
         accept",
        c
    );
    let v = rt.eval_str("<t>", &src).unwrap();
    assert_eq!(disp(&rt, &v), "#t");

    let src_bad = format!("({} (list 1 'oops 3))", c);
    let v = rt.eval_str("<t>", &src_bad).unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

#[test]
fn lowered_vectorof_runs_with_vector_of_c_helper() {
    let mut rt = rt_with_contract();
    let ty = Type::Vectorof(Box::new(Type::String));
    let c = type_to_contract(&ty);
    let src_good = format!("({} (vector \"a\" \"b\"))", c);
    let src_bad = format!("({} (vector \"a\" 2))", c);
    let v = rt.eval_str("<t>", &src_good).unwrap();
    assert_eq!(disp(&rt, &v), "#t");
    let v = rt.eval_str("<t>", &src_bad).unwrap();
    assert_eq!(disp(&rt, &v), "#f");
}

// ---- union round-trip ----

#[test]
fn lowered_union_runs_with_or_c() {
    let mut rt = rt_with_contract();
    let ty = Type::union(vec![Type::Fixnum, Type::String]);
    let c = type_to_contract(&ty);
    let src = format!("(list ({} 1) ({} \"hi\") ({} 'sym))", c, c, c);
    let v = rt.eval_str("<t>", &src).unwrap();
    assert_eq!(disp(&rt, &v), "(#t #t #f)");
}

// ---- listof inside arrow round-trip ----

#[test]
fn lowered_arrow_with_listof_param_round_trips() {
    let mut rt = rt_with_contract();
    // (-> (Listof Fixnum) Fixnum)
    let ty = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Listof(Box::new(Type::Fixnum))],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    let c = type_to_contract(&ty);
    let src = format!(
        "(define len (apply-contract {} length 'len))
         (len (list 10 20 30))",
        c
    );
    let v = rt.eval_str("<t>", &src).unwrap();
    assert_eq!(disp(&rt, &v), "3");

    let err = rt
        .eval_str("<t>", "(len (list 1 'bad))")
        .expect_err("non-int in list violates lowered (Listof Fixnum)");
    let s = format!("{}", err);
    assert!(s.contains("contract"), "got: {}", s);
}
