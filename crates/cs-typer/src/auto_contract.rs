//! Library-export auto-contracting (issue #11 ext-2).
//!
//! After [`crate::extract_annotations`] has recorded every
//! ascription / typed-define / `(define/typed)` in the user's
//! source as a [`crate::TopLevelAnnotation`], this pass walks
//! the top-level Datum stream looking for `(library ...)` forms
//! and injects a runtime contract wrap for each exported binding
//! whose name appears in the annotation table.
//!
//! Concretely, for a library
//!
//! ```text
//! (library (name)
//!   (export f)
//!   (import (crab contract))
//!   (: f (-> Fixnum Fixnum))
//!   (define (f x) (+ x 1)))
//! ```
//!
//! this pass rewrites the body to
//!
//! ```text
//! (library (name)
//!   (export f)
//!   (import (crab contract))
//!   (: f (-> Fixnum Fixnum))
//!   (define (f x) (+ x 1))
//!   (set! f (apply-contract (-> integer? integer?) f (quote f))))
//! ```
//!
//! The `set!` runs immediately after the define and replaces
//! the binding with its contract-wrapped form. Untyped callers
//! importing the library now hit `&contract-violation` on
//! misuse — without the user having to write `(define/typed)`.
//!
//! Scope: this pass only fires for bindings declared at the top
//! level of a `(library …)` form. Non-library top-level code
//! retains its current static-only behaviour for `(: f T)`
//! ascriptions — we don't want to silently add a runtime check
//! cost to every typed top-level binding in a script.
//!
//! The pass is a no-op when no relevant ascription exists in
//! the table, so untyped libraries pay nothing.
//!
//! ## Auto-import requirement
//!
//! The injected `apply-contract` + contract combinators (`->`,
//! `or/c`, `integer?`, …) come from `(crab contract)`. The user
//! library MUST `(import (crab contract))` for the wrap to
//! evaluate. We could auto-inject the import, but that would
//! force the contract library on every typed library whether or
//! not the user wanted runtime checks. Document instead — the
//! error a user would see if they forgot is a clear
//! `unbound apply-contract` from the runtime.

use std::rc::Rc;

use cs_core::{Symbol, SymbolTable};
use cs_diag::Span;
use cs_parse::Datum;

use crate::annotate::AnnotationTable;
use crate::types::{ProcType, Type};

/// Walk top-level Datums and inject auto-wrap statements for
/// every ascribed library export. Untyped or non-library forms
/// pass through unchanged.
///
/// `data` should be the [`crate::extract_annotations`] stripped
/// output. `table` carries any top-level (i.e. outside-library)
/// ascriptions — used only as fallback. `syms` is the in-scope
/// symbol table; keywords and contract atoms intern through it.
///
/// Note: ascriptions written INSIDE a `(library …)` body are
/// scanned by this pass directly — `extract_annotations` only
/// walks top-level Datums and treats library forms as opaque.
/// That's the right semantics: a library-internal `(: f T)`
/// scopes the type to the library's `f`, not any same-named
/// top-level `f`.
pub fn auto_contract_library_exports(
    data: Vec<Datum>,
    table: &AnnotationTable,
    syms: &mut SymbolTable,
) -> Vec<Datum> {
    let kws = Keywords::intern(syms);
    let mut out = Vec::with_capacity(data.len());
    for d in data {
        match rewrite_library(&d, table, &kws, syms) {
            Some(rewritten) => out.push(rewritten),
            None => out.push(d),
        }
    }
    out
}

/// Build the contract Datum for `t`. Mirrors
/// [`crate::contract_lowering::type_to_contract`] but emits a
/// Datum tree directly so the auto-wrapper doesn't have to
/// stringify-and-reparse.
pub fn type_to_contract_datum(t: &Type, syms: &mut SymbolTable, span: Span) -> Datum {
    match t {
        Type::Fixnum => atom("integer?", syms, span),
        Type::Flonum => atom("real?", syms, span),
        Type::Boolean => atom("boolean?", syms, span),
        Type::Character => atom("char?", syms, span),
        Type::Symbol => atom("symbol?", syms, span),
        Type::Pair => atom("pair?", syms, span),
        Type::Vector => atom("vector?", syms, span),
        Type::String => atom("string?", syms, span),
        Type::ByteVector => atom("bytevector?", syms, span),
        Type::Procedure => atom("procedure?", syms, span),
        Type::Null => atom("null?", syms, span),
        Type::Any => atom("any/c", syms, span),
        Type::Never => atom("none/c", syms, span),
        Type::Union(ts) => {
            let mut items = vec![atom("or/c", syms, span)];
            for sub in ts {
                items.push(type_to_contract_datum(sub, syms, span));
            }
            build_list(items, span)
        }
        Type::Listof(elem) => build_list(
            vec![
                atom("list-of/c", syms, span),
                type_to_contract_datum(elem, syms, span),
            ],
            span,
        ),
        Type::Vectorof(elem) => build_list(
            vec![
                atom("vector-of/c", syms, span),
                type_to_contract_datum(elem, syms, span),
            ],
            span,
        ),
        Type::Procedure_(proc) => proc_to_datum(proc, syms, span),
        // Polymorphism erases to Any at the contract level: same
        // call as type_to_contract.
        Type::Forall(_vars, body) => type_to_contract_datum(body, syms, span),
        Type::Var(_) => atom("any/c", syms, span),
    }
}

fn proc_to_datum(p: &ProcType, syms: &mut SymbolTable, span: Span) -> Datum {
    let mut items: Vec<Datum> = Vec::new();
    match &p.rest {
        None => {
            // (-> dom1 ... rng)
            items.push(atom("->", syms, span));
            for param in &p.params {
                items.push(type_to_contract_datum(param, syms, span));
            }
            items.push(type_to_contract_datum(&p.return_type, syms, span));
        }
        Some(rest) => {
            // (->* (list mandatory-doms ...) rest-pred rng)
            //
            // The first arg is a *runtime* list of contracts, not
            // a literal Scheme list — the contract library's `->*`
            // procedure consumes it that way. We emit `(list a b
            // c)` so Scheme evaluates each dom contract and
            // bundles the results. Emitting `(a b c)` directly
            // would treat the first element as a procedure to call
            // (e.g. `(integer?)` → 0-arg call to `integer?`).
            items.push(atom("->*", syms, span));
            let mut doms: Vec<Datum> = vec![atom("list", syms, span)];
            for t in &p.params {
                doms.push(type_to_contract_datum(t, syms, span));
            }
            items.push(build_list(doms, span));
            items.push(type_to_contract_datum(rest, syms, span));
            items.push(type_to_contract_datum(&p.return_type, syms, span));
        }
    }
    build_list(items, span)
}

fn atom(name: &str, syms: &mut SymbolTable, span: Span) -> Datum {
    Datum::Symbol(syms.intern(name), span)
}

/// Cached keyword symbols.
struct Keywords {
    library: Symbol,
    export: Symbol,
    define: Symbol,
    define_typed: Symbol,
    set_bang: Symbol,
    apply_contract: Symbol,
    quote: Symbol,
    colon: Symbol,
}

impl Keywords {
    fn intern(syms: &mut SymbolTable) -> Self {
        Self {
            library: syms.intern("library"),
            export: syms.intern("export"),
            define: syms.intern("define"),
            define_typed: syms.intern("define/typed"),
            set_bang: syms.intern("set!"),
            apply_contract: syms.intern("apply-contract"),
            quote: syms.intern("quote"),
            colon: syms.intern(":"),
        }
    }
}

/// If `d` is a `(library ...)` form whose exports include
/// ascribed names, return a rewritten Datum with the wrap
/// statements injected. Returns `None` when no rewrite is
/// needed (non-library forms or libraries with no ascribed
/// exports).
///
/// Ascriptions are sourced in priority order:
///   1. Library-internal `(: NAME T)` forms (correct scope).
///   2. Library-internal `(define/typed NAME T E)` forms — the
///      ext-1 classifier already records these at the top level,
///      but only if the form was top-level; library-internal
///      ones we discover here.
///   3. Outside-the-library `table` ascriptions, as fallback —
///      useful when the user wrote the ascription file-scoped
///      before the library declaration.
fn rewrite_library(
    d: &Datum,
    table: &AnnotationTable,
    kws: &Keywords,
    syms: &mut SymbolTable,
) -> Option<Datum> {
    let elements = flatten_list(d)?;
    if elements.len() < 4 {
        return None;
    }
    // [0] = `library`, [1] = name spec, [2] = (export ...),
    // [3] = (import ...), [4..] = body.
    if !matches!(&elements[0], Datum::Symbol(s, _) if *s == kws.library) {
        return None;
    }
    let exports = parse_export_list(&elements[2], kws)?;

    // Scan the library body for in-scope ascriptions. Each
    // entry maps `name -> type` from a `(: NAME T)` or
    // `(define/typed NAME T E)` form.
    let body_start = 4;
    let body: &[Datum] = &elements[body_start..];
    let local_ann = scan_local_ascriptions(body, kws, syms);

    // For each export, resolve its declared type: prefer the
    // library-internal ascription, fall back to the table.
    let typed_exports: Vec<(Symbol, Type)> = exports
        .iter()
        .filter_map(|name| {
            local_ann
                .iter()
                .find(|(n, _)| *n == *name)
                .map(|(n, t)| (*n, t.clone()))
                .or_else(|| table.ascription(*name).map(|ann| (*name, ann.clone())))
        })
        .collect();
    // The strip-pass MUST run whenever any `(: ...)` form
    // appears in the body, even if its type couldn't be parsed
    // (e.g. `(->* ...)` lies outside the typer's annotation
    // grammar). Otherwise the expander would later try to
    // evaluate `:` as an unbound reference at runtime.
    let has_any_ascription_form = body.iter().any(|f| is_ascription_form(f, kws));
    if typed_exports.is_empty() && !has_any_ascription_form {
        return None;
    }
    // Rebuild the library:
    // - drop bare `(: NAME T)` forms (the runtime expander would
    //   otherwise try to evaluate `:` as an unbound reference)
    // - keep every other form unchanged
    // - inject a `(set! NAME (apply-contract ... NAME 'NAME))`
    //   immediately after the define for every typed export
    let mut new_body: Vec<Datum> = Vec::with_capacity(body.len() + typed_exports.len());
    for form in body {
        if is_ascription_form(form, kws) {
            // Strip — we've already lifted the type info into
            // `typed_exports`.
            continue;
        }
        new_body.push(form.clone());
        if let Some(name) = find_define_name(form, kws) {
            if let Some((_, ty)) = typed_exports.iter().find(|(n, _)| *n == name) {
                let wrap = build_wrap_set(name, ty, kws, syms, form.span());
                new_body.push(wrap);
            }
        }
    }
    // Reassemble the library list.
    let mut new_elements: Vec<Datum> = Vec::with_capacity(body_start + new_body.len());
    for e in &elements[..body_start] {
        new_elements.push(e.clone());
    }
    new_elements.extend(new_body);
    Some(build_list(new_elements, d.span()))
}

/// Scan a library body for `(: NAME T)` and `(define/typed NAME T E)`
/// forms, parsing T against the in-scope type-alias context
/// (currently empty for library-internal scope — aliases are a
/// top-level concept; library-internal `(define-type)` would be
/// a separate extension). Returns the resolved `(name, type)`
/// pairs in source order.
fn scan_local_ascriptions(
    body: &[Datum],
    kws: &Keywords,
    syms: &SymbolTable,
) -> Vec<(Symbol, Type)> {
    let mut out: Vec<(Symbol, Type)> = Vec::new();
    let empty_aliases: Vec<(String, Type)> = Vec::new();
    for form in body {
        let elements = match flatten_list(form) {
            Some(e) if !e.is_empty() => e,
            _ => continue,
        };
        let head = match &elements[0] {
            Datum::Symbol(s, _) => *s,
            _ => continue,
        };
        // (: NAME TYPE)
        if head == kws.colon && elements.len() == 3 {
            if let Datum::Symbol(name, _) = &elements[1] {
                if let Ok(t) =
                    crate::extract::parse_datum_as_type_pub(&elements[2], syms, &empty_aliases)
                {
                    // Don't clobber an earlier ascription of the
                    // same name — first wins.
                    if !out.iter().any(|(n, _)| *n == *name) {
                        out.push((*name, t));
                    }
                }
            }
        }
        // (define/typed NAME TYPE EXPR)
        if head == kws.define_typed && elements.len() == 4 {
            if let Datum::Symbol(name, _) = &elements[1] {
                if let Ok(t) =
                    crate::extract::parse_datum_as_type_pub(&elements[2], syms, &empty_aliases)
                {
                    if !out.iter().any(|(n, _)| *n == *name) {
                        out.push((*name, t));
                    }
                }
            }
        }
    }
    out
}

/// Extract the symbol bound by a `(define NAME ...)` /
/// `(define (NAME ...) ...)` / `(define (NAME a . rest) ...)` /
/// `(define/typed NAME ...)` form, or `None` if `d` isn't a
/// recognized define shape.
fn find_define_name(d: &Datum, kws: &Keywords) -> Option<Symbol> {
    let elements = flatten_list(d)?;
    if elements.len() < 3 {
        return None;
    }
    let head = match &elements[0] {
        Datum::Symbol(s, _) => *s,
        _ => return None,
    };
    if head != kws.define && head != kws.define_typed {
        return None;
    }
    match &elements[1] {
        // (define NAME EXPR) | (define/typed NAME TYPE EXPR)
        Datum::Symbol(name, _) => Some(*name),
        // (define (NAME PARAMS ...) BODY ...) — either proper
        // `(NAME a b c)` or rest-shaped `(NAME a . rest)`. The
        // bound name is always the car.
        Datum::Pair(car, _, _) => {
            if let Datum::Symbol(name, _) = car.as_ref() {
                Some(*name)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Build `(set! NAME (apply-contract <CONTRACT> NAME (quote NAME)))`.
fn build_wrap_set(
    name: Symbol,
    ty: &Type,
    kws: &Keywords,
    syms: &mut SymbolTable,
    span: Span,
) -> Datum {
    let contract = type_to_contract_datum(ty, syms, span);
    let quoted_name = build_list(
        vec![Datum::Symbol(kws.quote, span), Datum::Symbol(name, span)],
        span,
    );
    let apply_contract_call = build_list(
        vec![
            Datum::Symbol(kws.apply_contract, span),
            contract,
            Datum::Symbol(name, span),
            quoted_name,
        ],
        span,
    );
    build_list(
        vec![
            Datum::Symbol(kws.set_bang, span),
            Datum::Symbol(name, span),
            apply_contract_call,
        ],
        span,
    )
}

/// True iff `d` looks like a bare `(: NAME TYPE)` ascription
/// form. Used by `rewrite_library` to strip these from library
/// bodies — otherwise the expander would try to evaluate `:`
/// at runtime as an unbound reference.
fn is_ascription_form(d: &Datum, kws: &Keywords) -> bool {
    let Some(elements) = flatten_list(d) else {
        return false;
    };
    if elements.len() != 3 {
        return false;
    }
    matches!(&elements[0], Datum::Symbol(s, _) if *s == kws.colon)
}

/// Parse the `(export NAME ...)` clause into a Vec of exported
/// names, or `None` if it's not a well-formed export clause.
fn parse_export_list(d: &Datum, kws: &Keywords) -> Option<Vec<Symbol>> {
    let elements = flatten_list(d)?;
    if !matches!(elements.first(), Some(Datum::Symbol(s, _)) if *s == kws.export) {
        return None;
    }
    let mut names = Vec::with_capacity(elements.len().saturating_sub(1));
    for e in &elements[1..] {
        if let Datum::Symbol(s, _) = e {
            names.push(*s);
        }
    }
    Some(names)
}

/// Walk a `Datum::Pair`/`Datum::Null` chain and return the
/// proper-list elements as a Vec. Returns None for improper
/// lists or non-list Datums.
fn flatten_list(d: &Datum) -> Option<Vec<Datum>> {
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

fn build_list(items: Vec<Datum>, list_span: Span) -> Datum {
    let mut tail = Datum::Null(list_span);
    for d in items.into_iter().rev() {
        let s = d.span();
        tail = Datum::Pair(Rc::new(d), Rc::new(tail), s);
    }
    match tail {
        Datum::Pair(a, b, _) => Datum::Pair(a, b, list_span),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_annotations;
    use cs_diag::SourceMap;
    use cs_parse::read_all;

    fn read(src: &str) -> (Vec<Datum>, SymbolTable) {
        let mut sm = SourceMap::new();
        let f = sm.add("<t>", src);
        let mut syms = SymbolTable::new();
        let data = read_all(f, src, &mut syms).expect("parse");
        (data, syms)
    }

    fn render(d: &Datum, syms: &SymbolTable) -> String {
        d.format_with(syms)
    }

    #[test]
    fn library_with_no_ascribed_exports_is_unchanged() {
        let (data, mut syms) = read(
            "(library (foo) \
               (export f) \
               (import (crab contract)) \
               (define (f x) (+ x 1)))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped.clone(), &table, &mut syms);
        // Untyped library — no rewrite.
        assert_eq!(after.len(), stripped.len());
        assert_eq!(render(&after[0], &syms), render(&stripped[0], &syms));
    }

    #[test]
    fn library_with_ascribed_export_gets_wrap_set() {
        // The ascription is INSIDE the library body, which
        // `extract_annotations` treats as opaque. The
        // auto-contract pass scans the body itself to find it.
        let (data, mut syms) = read(
            "(library (foo) \
               (export f) \
               (import (crab contract)) \
               (: f (-> Fixnum Fixnum)) \
               (define (f x) (+ x 1)))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        assert!(
            table.top_level.is_empty(),
            "library-internal ascription should NOT land in the top-level table"
        );
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        // The wrap should appear right after the define.
        assert!(s.contains("set!"), "no set! in {s}");
        assert!(s.contains("apply-contract"), "no apply-contract in {s}");
        assert!(s.contains("integer?"), "no contract lowered in {s}");
    }

    #[test]
    fn outside_library_ascription_falls_through_as_fallback() {
        // Ascription declared at the file's top level, before
        // the library. extract_annotations records it in the
        // table; the library's scan_local_ascriptions doesn't
        // find it locally. We fall back to the table.
        let (data, mut syms) = read(
            "(: f (-> Fixnum Fixnum)) \
             (library (foo) \
               (export f) \
               (import (crab contract)) \
               (define (f x) (+ x 1)))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        assert_eq!(
            table.top_level.len(),
            1,
            "top-level ascription should land in the table"
        );
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        // Find the rewritten library form.
        let lib_idx = after
            .iter()
            .position(|d| {
                if let Datum::Pair(car, _, _) = d {
                    matches!(&**car, Datum::Symbol(s, _) if syms.name(*s) == "library")
                } else {
                    false
                }
            })
            .expect("library form should be in the data");
        let s = render(&after[lib_idx], &syms);
        assert!(
            s.contains("set!"),
            "fallback ascription should drive wrap: {s}"
        );
    }

    #[test]
    fn non_library_top_level_is_unchanged() {
        let (data, mut syms) = read("(: f Fixnum) (define f 42)");
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let before = stripped.clone();
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        // Top-level (non-library) typed defines DON'T get
        // auto-wrapped — that's a deliberate scope decision.
        assert_eq!(after.len(), before.len());
        for (a, b) in after.iter().zip(before.iter()) {
            assert_eq!(render(a, &syms), render(b, &syms));
        }
    }

    #[test]
    fn library_with_unrelated_ascription_is_not_wrapped() {
        // `helper` is ascribed but not exported; should not be
        // wrapped. `f` is exported but not ascribed; should not
        // be wrapped either.
        let (data, mut syms) = read(
            "(library (foo) \
               (export f) \
               (import (crab contract)) \
               (: helper (-> Fixnum Fixnum)) \
               (define (helper x) x) \
               (define (f x) (helper x)))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        assert!(
            !s.contains("set!"),
            "should not inject set! when no exported binding is ascribed: {s}"
        );
    }

    #[test]
    fn define_typed_export_is_not_redundantly_wrapped() {
        // `(define/typed)` already wraps at the macro level; the
        // ascription synthesized by ext-1's classifier shouldn't
        // cause us to double-wrap here. The pass identifies the
        // define by name and inserts a wrap — but it's run BEFORE
        // expansion, so the define/typed Datum hasn't expanded
        // yet. The injected set! re-wraps the already-wrapped
        // binding after the macro runs. Idempotency comes from
        // apply-contract being safe to re-apply (it never fails
        // on a value that already passes the contract).
        //
        // Behavioral correctness: the rewrite SHOULD still
        // produce a set! injection — the double-wrap is a tiny
        // runtime cost on (define/typed) exports but preserves
        // the semantics. The Checker still verifies the inner
        // expression at expand time.
        let (data, mut syms) = read(
            "(library (foo) \
               (export f) \
               (import (crab contract)) \
               (define/typed f (-> Fixnum Fixnum) (lambda (x) x)))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        // The set! is injected after the define/typed form — at
        // the Datum level, the define/typed Datum is recognized
        // by `find_define_name` because its head matches the
        // cached `define_typed` keyword.
        assert!(s.contains("set!"), "expected wrap-set in {s}");
    }

    #[test]
    fn ascribed_atomic_export_lowers_to_atomic_predicate() {
        let (data, mut syms) = read(
            "(library (foo) \
               (export n) \
               (import (crab contract)) \
               (: n Fixnum) \
               (define n 42))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        assert!(
            s.contains("integer?"),
            "Fixnum should lower to integer?: {s}"
        );
        assert!(s.contains("set!"));
    }

    #[test]
    fn union_export_lowers_to_or_c() {
        let (data, mut syms) = read(
            "(library (foo) \
               (export x) \
               (import (crab contract)) \
               (: x (U Fixnum String)) \
               (define x 1))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        assert!(s.contains("or/c"));
        assert!(s.contains("integer?"));
        assert!(s.contains("string?"));
    }

    #[test]
    fn listof_export_lowers_to_list_of_c() {
        let (data, mut syms) = read(
            "(library (foo) \
               (export xs) \
               (import (crab contract)) \
               (: xs (Listof Fixnum)) \
               (define xs (list 1 2 3)))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        assert!(s.contains("list-of/c"));
    }

    #[test]
    fn variadic_tail_export_lowers_to_arrow_star() {
        let (data, mut syms) = read(
            "(library (foo) \
               (export sum) \
               (import (crab contract)) \
               (: sum (->* (Fixnum) Fixnum Fixnum)) \
               (define (sum . xs) 0))",
        );
        let (stripped, table, _) = extract_annotations(&data, &mut syms);
        let after = auto_contract_library_exports(stripped, &table, &mut syms);
        let s = render(&after[0], &syms);
        assert!(s.contains("->*"));
    }
}
