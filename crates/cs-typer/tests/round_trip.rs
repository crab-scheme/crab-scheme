//! Phase 1 iter 1.6 — round-trip integration test.
//!
//! Verify that running cs-typer's `extract_annotations` pre-pass
//! over a typed program produces:
//!   (a) a stripped Datum stream that the rest of the pipeline
//!       (cs-expand → cs-vm::compile) accepts without error,
//!   (b) an AnnotationTable populated with the user's types.
//!
//! Iter 1.6 doesn't actually CHECK types yet (that's Phase 2 +
//! onward) — it just proves the front-end can carry annotations
//! through the pipeline without breaking anything.

use std::collections::HashMap;

use cs_core::SymbolTable;
use cs_diag::SourceMap;
use cs_expand::Expander;
use cs_parse::read_all;
use cs_typer::{extract_annotations, ProcType, Type};

fn parse(src: &str) -> (Vec<cs_parse::Datum>, SymbolTable, SourceMap) {
    let mut sm = SourceMap::new();
    let f = sm.add("<round-trip-test>", src);
    let mut syms = SymbolTable::new();
    let data = read_all(f, src, &mut syms).expect("parse succeeds");
    (data, syms, sm)
}

#[test]
fn typed_fib_round_trips_through_pipeline() {
    let src = "\
        (: fib (-> Fixnum Fixnum))
        (define (fib [n : Fixnum]) : Fixnum
          (if (< n 2)
              n
              (+ (fib (- n 1)) (fib (- n 2)))))
    ";
    let (data, mut syms, _sm) = parse(src);

    // Pre-pass: extract annotations.
    let (stripped, table, diags) = extract_annotations(&data, &mut syms);
    assert!(diags.is_empty(), "annotation diags: {diags:?}");

    // Table assertions:
    // - One top-level ascription `(: fib (-> Fixnum Fixnum))`.
    // - One lambda annotation for the typed define.
    assert_eq!(table.top_level.len(), 1);
    assert_eq!(syms.name(table.top_level[0].name), "fib");
    let want = Type::Procedure_(Box::new(ProcType {
        params: vec![Type::Fixnum],
        return_type: Type::Fixnum,
        rest: None,
        filter: None,
    }));
    assert_eq!(table.top_level[0].type_ann, want);

    assert_eq!(table.lambdas.len(), 1);
    let lambda_ann = table.lambdas.values().next().unwrap();
    assert_eq!(lambda_ann.param_types, vec![Some(Type::Fixnum)]);
    assert_eq!(lambda_ann.return_type, Some(Type::Fixnum));

    // Stripped data should still contain the define.
    assert_eq!(stripped.len(), 1);

    // Hand stripped data to the expander — it should succeed
    // (annotation markers are gone; the body is normal Scheme).
    let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
    let mut exp = Expander::new(&mut syms, &mut macros);
    let core = exp
        .expand_program(&stripped)
        .expect("expand succeeds on stripped data");
    drop(exp);

    // Light sanity: the expanded core should compile via cs-vm.
    let globals = HashMap::new();
    let primops: HashMap<_, _> = [
        (syms.intern("<"), cs_vm::compiler::PrimOp::Lt),
        (syms.intern("+"), cs_vm::compiler::PrimOp::Add),
        (syms.intern("-"), cs_vm::compiler::PrimOp::Sub),
    ]
    .into_iter()
    .collect();
    let bc = cs_vm::compile_with_globals_and_primops(&core, &globals, &primops)
        .expect("compile succeeds on expanded core");
    assert!(!bc.lambdas.is_empty(), "fib lambda should compile");
}

#[test]
fn untyped_program_unchanged_through_pre_pass() {
    let src = "\
        (define (square x) (* x x))
        (define (cube x) (* x (square x)))
    ";
    let (data, mut syms, _sm) = parse(src);

    let (stripped, table, diags) = extract_annotations(&data, &mut syms);
    assert!(diags.is_empty());

    // Zero annotations → empty table; data passes through unchanged.
    assert!(table.is_empty(), "expected empty table for untyped code");
    assert_eq!(stripped.len(), data.len());

    // Expand + compile to prove the round-trip stays valid.
    let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
    let mut exp = Expander::new(&mut syms, &mut macros);
    let core = exp.expand_program(&stripped).expect("expand succeeds");
    drop(exp);

    let globals = HashMap::new();
    let primops: HashMap<_, _> =
        std::iter::once((syms.intern("*"), cs_vm::compiler::PrimOp::Mul)).collect();
    cs_vm::compile_with_globals_and_primops(&core, &globals, &primops).expect("compile succeeds");
}

#[test]
fn mixed_typed_and_untyped_program() {
    let src = "\
        (define-type Number (U Fixnum Flonum))
        (define (untyped-helper x) (+ x 1))
        (: doubled (-> Fixnum Fixnum))
        (define (doubled [x : Fixnum]) : Fixnum (* x 2))
    ";
    let (data, mut syms, _sm) = parse(src);

    let (stripped, table, diags) = extract_annotations(&data, &mut syms);
    assert!(diags.is_empty());

    // Annotations:
    // - 1 alias (Number)
    // - 1 ascription (doubled)
    // - 1 lambda annotation (for the typed define)
    assert_eq!(table.aliases.len(), 1);
    assert_eq!(syms.name(table.aliases[0].name), "Number");
    assert_eq!(table.top_level.len(), 1);
    assert_eq!(syms.name(table.top_level[0].name), "doubled");
    assert_eq!(table.lambdas.len(), 1);

    // Stripped: the alias + ascription drop; the two defines remain.
    assert_eq!(stripped.len(), 2);

    // Pipeline still works.
    let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
    let mut exp = Expander::new(&mut syms, &mut macros);
    let core = exp.expand_program(&stripped).expect("expand");
    drop(exp);

    let globals = HashMap::new();
    let primops: HashMap<_, _> = [
        (syms.intern("+"), cs_vm::compiler::PrimOp::Add),
        (syms.intern("*"), cs_vm::compiler::PrimOp::Mul),
    ]
    .into_iter()
    .collect();
    cs_vm::compile_with_globals_and_primops(&core, &globals, &primops).expect("compile");
}
