//! Datum pre-processor — extracts type annotations from a
//! top-level program's `Vec<Datum>` and returns both the
//! stripped Datums (ready for cs-expand) and the populated
//! [`AnnotationTable`].
//!
//! ## Why pre-process instead of extending the expander
//!
//! cs-expand has no dependency on cs-typer (and shouldn't —
//! typing is optional). Handling annotations as a pre-pass over
//! the raw `Vec<Datum>` lets the entire downstream pipeline
//! (expand → compile → run) stay unaware of types when the
//! user hasn't asked for them, while preserving exact source
//! spans for the typer's later error reporting.
//!
//! ## Recognized forms (Phase 1 iter 1.4)
//!
//! - `(: NAME TYPE)` — `TopLevelAnnotation` attached by name.
//! - `(define-type NAME TYPE)` — `TypeAlias` declaration.
//! - `(define (NAME [P : T] ...) : R BODY ...)` — record a
//!   `LambdaAnnotation` keyed by the (define-form) span and
//!   strip the `: T` markers from the header.
//!
//! Inline `(lambda ([P : T]) : R BODY)` and typed `letrec`
//! heads are deferred to a later iter (the syntactic rewrites
//! cascade through more contexts).

use std::rc::Rc;

use cs_core::{Symbol, SymbolTable};
use cs_diag::{Diagnostic, Severity, Span};
use cs_parse::Datum;

use crate::annotate::{AnnotationTable, LambdaAnnotation, TopLevelAnnotation, TypeAlias};
use crate::parse_ann::{parse_type_ann, TypeAnnError, TypeDatum};
use crate::types::Type;

/// Pre-process a top-level program. Returns the stripped form
/// (annotation forms removed, typed `define`s stripped of `:`
/// markers) and the populated annotation table.
///
/// Malformed annotations surface as diagnostics; unrecognized
/// syntax passes through unchanged (the expander will catch
/// real errors).
pub fn extract_annotations(
    data: &[Datum],
    syms: &mut SymbolTable,
) -> (Vec<Datum>, AnnotationTable, Vec<Diagnostic>) {
    let kws = Keywords::intern(syms);
    let mut out_data: Vec<Datum> = Vec::with_capacity(data.len());
    let mut table = AnnotationTable::new();
    let mut diags: Vec<Diagnostic> = Vec::new();

    for d in data {
        match classify_top_level(d, syms, &kws, &mut diags) {
            TopForm::Ascription(ann) => {
                table.top_level.push(ann);
            }
            TopForm::DefineType(alias) => {
                table.aliases.push(alias);
            }
            TopForm::TypedDefine {
                stripped,
                lambda_ann,
                lambda_span,
            } => {
                table.lambdas.insert(lambda_span, lambda_ann);
                out_data.push(stripped);
            }
            TopForm::Passthrough => {
                out_data.push(d.clone());
            }
        }
    }

    (out_data, table, diags)
}

/// What the classifier returns for a top-level Datum.
enum TopForm {
    Ascription(TopLevelAnnotation),
    DefineType(TypeAlias),
    TypedDefine {
        stripped: Datum,
        lambda_ann: LambdaAnnotation,
        lambda_span: Span,
    },
    Passthrough,
}

/// Cached symbol IDs for the keywords this pre-processor
/// recognizes. Interned once per `extract_annotations` call.
struct Keywords {
    colon: Symbol,
    define: Symbol,
    define_type: Symbol,
}

impl Keywords {
    fn intern(syms: &mut SymbolTable) -> Self {
        Self {
            colon: syms.intern(":"),
            define: syms.intern("define"),
            define_type: syms.intern("define-type"),
        }
    }
}

fn classify_top_level(
    d: &Datum,
    syms: &SymbolTable,
    kws: &Keywords,
    diags: &mut Vec<Diagnostic>,
) -> TopForm {
    let elements = match flatten_proper_list(d) {
        Some(e) if !e.is_empty() => e,
        _ => return TopForm::Passthrough,
    };
    let head_sym = match &elements[0] {
        Datum::Symbol(s, _) => *s,
        _ => return TopForm::Passthrough,
    };
    if head_sym == kws.colon {
        return classify_ascription(&elements, syms, diags);
    }
    if head_sym == kws.define_type {
        return classify_define_type(&elements, d.span(), syms, diags);
    }
    if head_sym == kws.define {
        return classify_define(&elements, d.span(), syms, kws, diags);
    }
    TopForm::Passthrough
}

fn classify_ascription(
    elements: &[Datum],
    syms: &SymbolTable,
    diags: &mut Vec<Diagnostic>,
) -> TopForm {
    if elements.len() != 3 {
        return TopForm::Passthrough;
    }
    let name = match &elements[1] {
        Datum::Symbol(s, _) => *s,
        _ => return TopForm::Passthrough,
    };
    let type_datum = &elements[2];
    match parse_datum_as_type(type_datum, syms) {
        Ok(t) => TopForm::Ascription(TopLevelAnnotation {
            name,
            type_ann: t,
            ascription_span: elements[0].span(),
        }),
        Err(e) => {
            diags.push(type_ann_diag(e, type_datum.span()));
            TopForm::Passthrough
        }
    }
}

fn classify_define_type(
    elements: &[Datum],
    span: Span,
    syms: &SymbolTable,
    diags: &mut Vec<Diagnostic>,
) -> TopForm {
    if elements.len() != 3 {
        return TopForm::Passthrough;
    }
    let name = match &elements[1] {
        Datum::Symbol(s, _) => *s,
        _ => return TopForm::Passthrough,
    };
    let type_datum = &elements[2];
    match parse_datum_as_type(type_datum, syms) {
        Ok(target) => TopForm::DefineType(TypeAlias {
            name,
            target,
            define_span: span,
        }),
        Err(e) => {
            diags.push(type_ann_diag(e, type_datum.span()));
            TopForm::Passthrough
        }
    }
}

/// Classify a `(define ...)` form. We're only interested in
/// the typed shape `(define (NAME PARAM ...) : RET BODY ...)`
/// where at least one PARAM is `[P : T]`-shaped OR a return
/// type is given. Otherwise pass through unchanged.
fn classify_define(
    elements: &[Datum],
    top_span: Span,
    syms: &SymbolTable,
    kws: &Keywords,
    diags: &mut Vec<Diagnostic>,
) -> TopForm {
    if elements.len() < 3 {
        return TopForm::Passthrough;
    }
    let head_list = match flatten_proper_list(&elements[1]) {
        Some(items) if !items.is_empty() => items,
        _ => return TopForm::Passthrough,
    };
    // Header element [0] is the fn name; [1..] are params.
    let name_datum = head_list[0].clone();
    if !matches!(&name_datum, Datum::Symbol(_, _)) {
        return TopForm::Passthrough;
    }
    let mut stripped_params: Vec<Datum> = Vec::with_capacity(head_list.len() - 1);
    let mut param_types: Vec<Option<Type>> = Vec::with_capacity(head_list.len() - 1);
    let mut any_param_annotated = false;
    for p in &head_list[1..] {
        match strip_param_ann(p, syms, kws, diags) {
            Some((name_d, t_opt)) => {
                stripped_params.push(name_d);
                if t_opt.is_some() {
                    any_param_annotated = true;
                }
                param_types.push(t_opt);
            }
            None => return TopForm::Passthrough,
        }
    }
    // Check for `: RET` between header and body.
    // Shape: elements[0]=`define`, [1]=header, [2]=`:` (maybe),
    // [3]=ret-type, [4..]=body. Otherwise [2..] is body.
    let mut body_first_idx = 2;
    let mut return_type: Option<Type> = None;
    if elements.len() >= 5 {
        if let Datum::Symbol(s, _) = &elements[2] {
            if *s == kws.colon {
                match parse_datum_as_type(&elements[3], syms) {
                    Ok(t) => {
                        return_type = Some(t);
                        body_first_idx = 4;
                    }
                    Err(e) => {
                        diags.push(type_ann_diag(e, elements[3].span()));
                        // Fall through: skip the (broken) annotation
                        // but keep parsing the rest of the form.
                        body_first_idx = 4;
                    }
                }
            }
        }
    }
    if !any_param_annotated && return_type.is_none() {
        return TopForm::Passthrough;
    }

    // Build stripped (define (name p ...) body ...).
    let stripped_header = build_proper_list(
        std::iter::once(name_datum)
            .chain(stripped_params)
            .collect::<Vec<_>>(),
        elements[1].span(),
    );
    let mut new_form: Vec<Datum> = Vec::with_capacity(2 + (elements.len() - body_first_idx));
    new_form.push(elements[0].clone());
    new_form.push(stripped_header);
    for b in &elements[body_first_idx..] {
        new_form.push(b.clone());
    }
    let stripped = build_proper_list(new_form, top_span);

    let lambda_ann = LambdaAnnotation {
        param_types,
        return_type,
        rest_type: None,
    };
    TopForm::TypedDefine {
        stripped,
        lambda_ann,
        // Key by the OUTER define-form span. The expander
        // synthesizes the inner lambda with the same span;
        // the type checker matches by span on the resulting
        // CoreExpr::Lambda.
        lambda_span: top_span,
    }
}

/// Strip an inline param annotation. Recognizes:
/// - `name` → (Datum::Symbol(name), None)
/// - `(name : T)` (which is how brackets like `[name : T]`
///   parse, since cs-parse treats `[...]` as alias for `(...)`)
///   → (Datum::Symbol(name), Some(parsed T))
fn strip_param_ann(
    d: &Datum,
    syms: &SymbolTable,
    kws: &Keywords,
    diags: &mut Vec<Diagnostic>,
) -> Option<(Datum, Option<Type>)> {
    if matches!(d, Datum::Symbol(_, _)) {
        return Some((d.clone(), None));
    }
    let elements = flatten_proper_list(d)?;
    if elements.len() != 3 {
        return None;
    }
    let name = match &elements[0] {
        Datum::Symbol(_, _) => elements[0].clone(),
        _ => return None,
    };
    let is_colon = matches!(&elements[1], Datum::Symbol(s, _) if *s == kws.colon);
    if !is_colon {
        return None;
    }
    match parse_datum_as_type(&elements[2], syms) {
        Ok(t) => Some((name, Some(t))),
        Err(e) => {
            diags.push(type_ann_diag(e, elements[2].span()));
            Some((name, None))
        }
    }
}

/// Convert a [`Datum`] into a [`TypeDatum`] then parse as a
/// [`Type`]. Atomic type names (`Fixnum`, `Any`, etc.) match
/// against canonical strings — symbol IDs go via `syms.name()`.
fn parse_datum_as_type(d: &Datum, syms: &SymbolTable) -> Result<Type, TypeAnnError> {
    let td = datum_to_type_datum(d, syms)?;
    parse_type_ann(&td)
}

fn datum_to_type_datum(d: &Datum, syms: &SymbolTable) -> Result<TypeDatum, TypeAnnError> {
    match d {
        Datum::Symbol(s, _) => Ok(TypeDatum::Sym(syms.name(*s).to_string())),
        Datum::Null(_) => Ok(TypeDatum::List(vec![])),
        Datum::Pair(_, _, _) => {
            let elements = flatten_proper_list(d).ok_or_else(|| {
                TypeAnnError::UnknownType("improper list in type position".into())
            })?;
            let mut out = Vec::with_capacity(elements.len());
            for elt in &elements {
                out.push(datum_to_type_datum(elt, syms)?);
            }
            Ok(TypeDatum::List(out))
        }
        _ => Err(TypeAnnError::UnknownType(
            "expected symbol or list in type position".into(),
        )),
    }
}

/// Walk a `Datum::Pair`/`Datum::Null` chain and return the
/// proper-list elements as a Vec. Returns None for improper
/// lists.
fn flatten_proper_list(d: &Datum) -> Option<Vec<Datum>> {
    let mut out = Vec::new();
    let mut cur = d.clone();
    loop {
        match cur {
            Datum::Null(_) => return Some(out),
            Datum::Pair(car, cdr, _) => {
                out.push((*car).clone());
                cur = (*cdr).clone();
            }
            _ => return None,
        }
    }
}

/// Build a proper-list `Datum::Pair(... Null)` chain from a Vec.
fn build_proper_list(items: Vec<Datum>, list_span: Span) -> Datum {
    let mut tail = Datum::Null(list_span);
    for d in items.into_iter().rev() {
        let s = d.span();
        tail = Datum::Pair(Rc::new(d), Rc::new(tail), s);
    }
    // Repair the outermost pair's span to be the full list span.
    // cs-parse assigns the *enclosing* `(...)` source range to the
    // outermost Pair's span; the iteration above leaves it as the
    // first element's span (e.g. just `define` for `(define ...)`).
    // The typechecker keys `LambdaAnnotation`s by the outer span,
    // so a mismatch here loses the annotation at lookup time.
    match tail {
        Datum::Pair(a, b, _) => Datum::Pair(a, b, list_span),
        other => other,
    }
}

fn type_ann_diag(err: TypeAnnError, span: Span) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: Some("typer-bad-annotation"),
        message: err.message(),
        primary: span,
        labels: vec![],
        notes: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_diag::SourceMap;
    use cs_parse::read_all;

    fn read(src: &str) -> (Vec<Datum>, SymbolTable) {
        let mut sm = SourceMap::new();
        let f = sm.add("<t>", src);
        let mut syms = SymbolTable::new();
        let data = read_all(f, src, &mut syms).expect("parse");
        (data, syms)
    }

    #[test]
    fn ascription_form_is_extracted() {
        let (data, mut syms) = read("(: fib (-> Fixnum Fixnum)) (define (fib n) n)");
        let (stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty(), "diagnostics: {diags:?}");
        assert_eq!(stripped.len(), 1, "ascription should drop from data");
        assert_eq!(table.top_level.len(), 1);
        let ann = &table.top_level[0];
        assert_eq!(syms.name(ann.name), "fib");
        match &ann.type_ann {
            Type::Procedure_(pt) => {
                assert_eq!(pt.params, vec![Type::Fixnum]);
                assert_eq!(pt.return_type, Type::Fixnum);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn define_type_form_is_extracted() {
        let (data, mut syms) = read("(define-type Number (U Fixnum Flonum))");
        let (stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty());
        assert!(stripped.is_empty());
        assert_eq!(table.aliases.len(), 1);
        let alias = &table.aliases[0];
        assert_eq!(syms.name(alias.name), "Number");
        assert_eq!(alias.target, Type::Union(vec![Type::Fixnum, Type::Flonum]));
    }

    #[test]
    fn ascription_with_bad_type_surfaces_diagnostic() {
        let (data, mut syms) = read("(: x Zorblax)");
        let (_stripped, _table, diags) = extract_annotations(&data, &mut syms);
        assert_eq!(diags.len(), 1, "expected one diag, got {diags:?}");
        assert!(
            diags[0].message.contains("Zorblax"),
            "expected unknown-type diag mentioning `Zorblax`, got: {}",
            diags[0].message
        );
    }

    #[test]
    fn unannotated_define_passes_through() {
        let (data, mut syms) = read("(define (f x) (+ x 1))");
        let (stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty());
        assert_eq!(stripped.len(), 1);
        assert!(table.is_empty());
    }

    #[test]
    fn empty_program_round_trips() {
        let (data, mut syms) = read("");
        let (stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty());
        assert!(stripped.is_empty());
        assert!(table.is_empty());
    }

    #[test]
    fn multiple_ascriptions_preserve_order() {
        let (data, mut syms) = read("(: a Fixnum) (: b Flonum) (: c Boolean)");
        let (_stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty());
        assert_eq!(table.top_level.len(), 3);
        assert_eq!(syms.name(table.top_level[0].name), "a");
        assert_eq!(syms.name(table.top_level[1].name), "b");
        assert_eq!(syms.name(table.top_level[2].name), "c");
        assert_eq!(table.top_level[0].type_ann, Type::Fixnum);
        assert_eq!(table.top_level[1].type_ann, Type::Flonum);
        assert_eq!(table.top_level[2].type_ann, Type::Boolean);
    }

    #[test]
    fn typed_define_strips_inline_param_annotations() {
        let (data, mut syms) = read("(define (sq [x : Fixnum]) : Fixnum (* x x))");
        let (stripped, table, diags) = extract_annotations(&data, &mut syms);
        assert!(diags.is_empty(), "diags: {diags:?}");
        assert_eq!(stripped.len(), 1, "one stripped define");
        // The annotation should be in the table.
        assert_eq!(table.lambdas.len(), 1);
        let ann = table.lambdas.values().next().unwrap();
        assert_eq!(ann.param_types, vec![Some(Type::Fixnum)]);
        assert_eq!(ann.return_type, Some(Type::Fixnum));
        // The stripped form should be a normal `(define (sq x) (* x x))`.
        let stripped_render = stripped[0].format_with(&syms);
        // Light verification: no `:` should remain.
        assert!(
            !stripped_render.contains(" : "),
            "stripped form should have no colon markers; got: {stripped_render}"
        );
    }

    #[test]
    fn typed_define_with_partial_param_annotations() {
        // Only second param annotated.
        let (data, mut syms) = read("(define (f x [y : Fixnum]) (+ x y))");
        let (stripped, table, _diags) = extract_annotations(&data, &mut syms);
        assert_eq!(stripped.len(), 1);
        assert_eq!(table.lambdas.len(), 1);
        let ann = table.lambdas.values().next().unwrap();
        assert_eq!(ann.param_types, vec![None, Some(Type::Fixnum)]);
        assert_eq!(ann.return_type, None);
    }

    #[test]
    fn typed_define_return_only() {
        let (data, mut syms) = read("(define (zero) : Fixnum 0)");
        let (stripped, table, _diags) = extract_annotations(&data, &mut syms);
        assert_eq!(stripped.len(), 1);
        assert_eq!(table.lambdas.len(), 1);
        let ann = table.lambdas.values().next().unwrap();
        assert!(ann.param_types.is_empty());
        assert_eq!(ann.return_type, Some(Type::Fixnum));
    }
}
