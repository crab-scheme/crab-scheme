//! `#:effects` declaration extraction (SDK milestone M01.C).
//!
//! A datum pre-processor — sibling to [`crate::extract`] — that runs before
//! cs-expand. It scans top-level `define` forms for an `#:effects '(…)`
//! annotation, records the declared [`EffectSet`] per definition name, and
//! returns the forms with the annotation **stripped** so cs-expand sees a
//! plain `(define …)`. The recorded table feeds the effect-check pass
//! (iter D), which infers each annotated body's effects and rejects any
//! that aren't a subset of the declaration.
//!
//! Both define shapes are handled:
//!
//! ```scheme
//! (define name        #:effects '(net io) value)      ; value form
//! (define (name args) #:effects '(io)     body …)     ; curried form
//! ```
//!
//! The `#:effects` keyword reads as an ordinary `Symbol` (the reader treats
//! `#:foo` as an identifier — see `lib/beam/web-contracts.scm`), so it's
//! matched by interned-symbol identity. Malformed annotations (non-quoted
//! operand, non-symbol effect, unknown effect name) surface as diagnostics
//! and the form passes through unstripped.

use std::collections::HashMap;

use cs_core::{Symbol, SymbolTable};
use cs_diag::{Diagnostic, Span};
use cs_ir::CoreExpr;
use cs_parse::Datum;

use crate::side_effect::{definition_body_effects, Effect, EffectSet};

/// Declared effect sets keyed by top-level definition name.
pub type EffectDecls = HashMap<Symbol, EffectSet>;

/// Extract `#:effects` declarations from a top-level program.
///
/// Returns the stripped data (ready for cs-expand), the declared effect set
/// per definition name, and diagnostics for malformed annotations.
pub fn extract_effect_decls(
    data: &[Datum],
    syms: &mut SymbolTable,
) -> (Vec<Datum>, EffectDecls, Vec<Diagnostic>) {
    let kw_define = syms.intern("define");
    let kw_effects = syms.intern("#:effects");
    let kw_quote = syms.intern("quote");

    let mut declared = EffectDecls::new();
    let mut diags = Vec::new();
    let mut out = Vec::with_capacity(data.len());

    for d in data {
        let stripped = strip_define_effects(
            d,
            kw_define,
            kw_effects,
            kw_quote,
            syms,
            &mut declared,
            &mut diags,
        );
        out.push(stripped.unwrap_or_else(|| d.clone()));
    }
    (out, declared, diags)
}

/// The effect-check pass (M01.D).
///
/// Runs on the **expanded** program (`cs_expand::Expander::expand_program`,
/// a top-level `Begin` of `Set` forms — top-level `define` lowers to `Set`).
/// For every top-level definition whose name carries a `#:effects`
/// declaration in `declared`, infer the body's effects and emit a
/// diagnostic if any inferred effect wasn't declared.
///
/// `declared` doubles as the effect environment: a checked body that calls
/// another declared function uses that function's declared set (so effects
/// compose across the program without an inter-procedural fixpoint).
pub fn check_effects(
    program: &CoreExpr,
    declared: &EffectDecls,
    syms: &SymbolTable,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    check_form(program, declared, syms, &mut diags);
    diags
}

fn check_form(
    form: &CoreExpr,
    declared: &EffectDecls,
    syms: &SymbolTable,
    diags: &mut Vec<Diagnostic>,
) {
    match form {
        CoreExpr::Begin { exprs, .. } => {
            for e in exprs {
                check_form(e, declared, syms, diags);
            }
        }
        CoreExpr::Set { name, value, span } => {
            if let Some(&declared_set) = declared.get(name) {
                let inferred = definition_body_effects(value, syms, declared);
                let undeclared = inferred.difference(declared_set);
                if !undeclared.is_empty() {
                    diags.push(Diagnostic::error(
                        format!(
                            "function `{}` performs effect(s) {undeclared} not in its \
                             #:effects declaration {declared_set}",
                            syms.name(*name)
                        ),
                        *span,
                    ));
                }
            }
        }
        _ => {}
    }
}

/// Effect constraint for a state-migration thunk (M01 iter E).
///
/// `(define-state-migration …)` registers a thunk that hot-upgrade runs on
/// each actor's saved state. Replays must be reproducible, so the thunk may
/// only allocate the new state and mutate locals
/// ([`EffectSet::MIGRATION_ALLOWED`]) — never I/O, network, wall-clock, or
/// randomness. Returns a diagnostic naming any disallowed effect.
///
/// `value` is the migration thunk (a lambda); `declared` is the program's
/// effect environment for resolving any calls it makes.
pub fn check_migration_body(
    name: &str,
    value: &CoreExpr,
    syms: &SymbolTable,
    declared: &EffectDecls,
) -> Option<Diagnostic> {
    let inferred = definition_body_effects(value, syms, declared);
    let disallowed = inferred.difference(EffectSet::MIGRATION_ALLOWED);
    if disallowed.is_empty() {
        None
    } else {
        Some(Diagnostic::error(
            format!(
                "state migration `{name}` performs disallowed effect(s) {disallowed}; \
                 migrations must be deterministic (allowed: {})",
                EffectSet::MIGRATION_ALLOWED
            ),
            value.span(),
        ))
    }
}

/// If `d` is `(define HEAD #:effects QUOTED REST…)`, record the declaration
/// and return the form with the `#:effects QUOTED` pair removed. Returns
/// `None` (caller keeps the original) when `d` isn't an effect-annotated
/// define.
fn strip_define_effects(
    d: &Datum,
    kw_define: Symbol,
    kw_effects: Symbol,
    kw_quote: Symbol,
    syms: &SymbolTable,
    declared: &mut EffectDecls,
    diags: &mut Vec<Diagnostic>,
) -> Option<Datum> {
    let span = d.span();
    let items = datum_to_vec(d)?;
    // Need at least: (define HEAD #:effects QUOTED …) → 4 items, and the
    // head must be `define`.
    if items.len() < 4 || !is_symbol(&items[0], kw_define) {
        return None;
    }
    // The annotation sits right after the name/header (items[1]).
    if !is_symbol(&items[2], kw_effects) {
        return None;
    }
    let name = match define_name(&items[1]) {
        Some(n) => n,
        None => return None,
    };

    match parse_quoted_effects(&items[3], kw_quote, syms) {
        Ok(set) => {
            declared.insert(name, set);
            // Rebuild without items[2] (#:effects) and items[3] (the list).
            let mut kept = Vec::with_capacity(items.len() - 2);
            kept.push(items[0].clone());
            kept.push(items[1].clone());
            kept.extend(items[4..].iter().cloned());
            Some(vec_to_list(kept, span))
        }
        Err(msg) => {
            diags.push(Diagnostic::error(msg, items[3].span()));
            // Leave the form unstripped; the diagnostic already flags it.
            None
        }
    }
}

/// The definition name for an effect declaration: `(define x …)` → `x`;
/// `(define (f a b) …)` → `f`. `None` for shapes we don't annotate.
fn define_name(head: &Datum) -> Option<Symbol> {
    match head {
        Datum::Symbol(s, _) => Some(*s),
        Datum::Pair(car, _, _) => match &**car {
            Datum::Symbol(s, _) => Some(*s),
            _ => None,
        },
        _ => None,
    }
}

/// Parse `(quote (eff …))` into an [`EffectSet`]. Errors (as a message) for
/// a non-quote operand, a non-symbol effect, or an unknown effect name.
fn parse_quoted_effects(
    d: &Datum,
    kw_quote: Symbol,
    syms: &SymbolTable,
) -> Result<EffectSet, String> {
    let items = datum_to_vec(d)
        .ok_or_else(|| "#:effects: expected a quoted list, e.g. '(net io)".to_string())?;
    if items.len() != 2 || !is_symbol(&items[0], kw_quote) {
        return Err("#:effects: expected a quoted list, e.g. '(net io)".to_string());
    }
    // The quoted datum may be the empty list `'()` (pure) or `(e1 e2 …)`.
    let effects = match &items[1] {
        Datum::Null(_) => Vec::new(),
        list => datum_to_vec(list)
            .ok_or_else(|| "#:effects: expected a list of effect symbols".to_string())?,
    };
    let mut set = EffectSet::PURE;
    for e in &effects {
        let Datum::Symbol(s, _) = e else {
            return Err("#:effects: each effect must be a symbol".to_string());
        };
        let name = syms.name(*s);
        match Effect::from_name(name) {
            Some(eff) => set.insert(eff),
            None => {
                return Err(format!(
                    "#:effects: unknown effect `{name}` (known: alloc io net wall-clock \
                     random mutation panic agent audit)"
                ))
            }
        }
    }
    Ok(set)
}

/// Collect a proper list `(a b c)` into `vec![a, b, c]`. `None` for an
/// improper or non-list datum.
fn datum_to_vec(d: &Datum) -> Option<Vec<Datum>> {
    let mut out = Vec::new();
    let mut cur = d;
    loop {
        match cur {
            Datum::Null(_) => return Some(out),
            Datum::Pair(car, cdr, _) => {
                out.push((**car).clone());
                cur = cdr;
            }
            _ => return None,
        }
    }
}

/// Rebuild a proper list datum from its elements.
fn vec_to_list(items: Vec<Datum>, span: Span) -> Datum {
    let mut acc = Datum::Null(span);
    for item in items.into_iter().rev() {
        acc = Datum::Pair(std::rc::Rc::new(item), std::rc::Rc::new(acc), span);
    }
    acc
}

fn is_symbol(d: &Datum, sym: Symbol) -> bool {
    matches!(d, Datum::Symbol(s, _) if *s == sym)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a single top-level form from source via the real reader.
    fn read_one(src: &str, syms: &mut SymbolTable) -> Vec<Datum> {
        let mut sources = cs_diag::SourceMap::new();
        let fid = sources.add("<test>", src);
        cs_parse::read_all(fid, src, syms).expect("parse")
    }

    fn render(d: &Datum, syms: &SymbolTable) -> String {
        match d {
            Datum::Symbol(s, _) => syms.name(*s).to_string(),
            Datum::Number(n, _) => format!("{n}"),
            Datum::Null(_) => "()".to_string(),
            Datum::Pair(..) => {
                let items = datum_to_vec(d).unwrap_or_default();
                let inner: Vec<String> = items.iter().map(|i| render(i, syms)).collect();
                format!("({})", inner.join(" "))
            }
            _ => "?".to_string(),
        }
    }

    #[test]
    fn extracts_value_form() {
        let mut syms = SymbolTable::new();
        let data = read_one("(define f #:effects '(net io) (lambda (x) x))", &mut syms);
        let (out, decls, diags) = extract_effect_decls(&data, &mut syms);
        assert!(diags.is_empty(), "diags: {diags:?}");
        let f = syms.intern("f");
        assert_eq!(
            decls.get(&f),
            Some(&EffectSet::from_effects([Effect::Net, Effect::Io]))
        );
        // #:effects '(net io) is stripped.
        assert_eq!(render(&out[0], &syms), "(define f (lambda (x) x))");
    }

    #[test]
    fn extracts_curried_form() {
        let mut syms = SymbolTable::new();
        let data = read_one("(define (g x) #:effects '(io) x)", &mut syms);
        let (out, decls, diags) = extract_effect_decls(&data, &mut syms);
        assert!(diags.is_empty(), "diags: {diags:?}");
        let g = syms.intern("g");
        assert_eq!(decls.get(&g), Some(&EffectSet::single(Effect::Io)));
        assert_eq!(render(&out[0], &syms), "(define (g x) x)");
    }

    #[test]
    fn empty_effects_list_is_pure() {
        let mut syms = SymbolTable::new();
        let data = read_one("(define f #:effects '() 42)", &mut syms);
        let (_out, decls, diags) = extract_effect_decls(&data, &mut syms);
        assert!(diags.is_empty());
        let f = syms.intern("f");
        assert_eq!(decls.get(&f), Some(&EffectSet::PURE));
    }

    #[test]
    fn plain_define_passes_through() {
        let mut syms = SymbolTable::new();
        let data = read_one("(define h 5)", &mut syms);
        let (out, decls, diags) = extract_effect_decls(&data, &mut syms);
        assert!(decls.is_empty());
        assert!(diags.is_empty());
        assert_eq!(render(&out[0], &syms), "(define h 5)");
    }

    #[test]
    fn unknown_effect_is_a_diagnostic() {
        let mut syms = SymbolTable::new();
        let data = read_one("(define f #:effects '(bogus) 1)", &mut syms);
        let (_out, decls, diags) = extract_effect_decls(&data, &mut syms);
        assert!(decls.is_empty(), "bad decl must not be recorded");
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("unknown effect `bogus`"),
            "{:?}",
            diags[0]
        );
    }

    #[test]
    fn non_quoted_operand_is_a_diagnostic() {
        let mut syms = SymbolTable::new();
        let data = read_one("(define f #:effects 5 1)", &mut syms);
        let (_out, _decls, diags) = extract_effect_decls(&data, &mut syms);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("quoted list"), "{:?}", diags[0]);
    }

    // === iter D: effect-check pass (extract → expand → check) ===

    /// Full pipeline: parse, extract `#:effects`, expand, then run the
    /// effect check. Returns the diagnostic messages.
    fn check_src(src: &str) -> Vec<String> {
        let mut syms = SymbolTable::new();
        let mut sources = cs_diag::SourceMap::new();
        let fid = sources.add("<test>", src);
        let data = cs_parse::read_all(fid, src, &mut syms).expect("parse");
        let (stripped, declared, ediags) = extract_effect_decls(&data, &mut syms);
        assert!(ediags.is_empty(), "extract diags: {ediags:?}");
        let mut macros = std::collections::HashMap::new();
        let mut expander = cs_expand::Expander::new(&mut syms, &mut macros);
        let program = expander.expand_program(&stripped).expect("expand");
        drop(expander);
        check_effects(&program, &declared, &syms)
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn undeclared_effect_is_rejected() {
        let diags = check_src("(define f #:effects '(io) (lambda (p) (http-get p)))");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].contains("net"), "{}", diags[0]);
    }

    #[test]
    fn declared_effect_conforms() {
        let diags = check_src("(define f #:effects '(io net) (lambda (p) (http-get p)))");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn pure_body_with_pure_declaration_is_ok() {
        let diags = check_src("(define f #:effects '() (lambda (x) (+ x 1)))");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn undeclared_mutation_is_rejected() {
        let diags = check_src("(define f #:effects '() (lambda (x) (vector-set! x 0 1)))");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].contains("mutation"), "{}", diags[0]);
    }

    #[test]
    fn effects_compose_across_declared_functions() {
        // f calls g (declared net); f declaring net conforms.
        let ok = check_src(
            "(define g #:effects '(net) (lambda (x) (http-get x)))\
             (define f #:effects '(net) (lambda (x) (g x)))",
        );
        assert!(ok.is_empty(), "{ok:?}");
        // f declaring only io must be rejected — calling g performs net.
        let bad = check_src(
            "(define g #:effects '(net) (lambda (x) (http-get x)))\
             (define f #:effects '(io) (lambda (x) (g x)))",
        );
        assert_eq!(bad.len(), 1, "{bad:?}");
        assert!(bad[0].contains("net"), "{}", bad[0]);
    }

    // === iter E: state-migration effect constraint ===

    /// Expand `(define m <thunk>)` and run the migration-effect check on the
    /// thunk; return the diagnostic message, if any.
    fn migration_diag(thunk_src: &str) -> Option<String> {
        let src = format!("(define m {thunk_src})");
        let mut syms = SymbolTable::new();
        let mut sources = cs_diag::SourceMap::new();
        let fid = sources.add("<test>", &src);
        let data = cs_parse::read_all(fid, &src, &mut syms).expect("parse");
        let (stripped, declared, _) = extract_effect_decls(&data, &mut syms);
        let mut macros = std::collections::HashMap::new();
        let mut expander = cs_expand::Expander::new(&mut syms, &mut macros);
        let program = expander.expand_program(&stripped).expect("expand");
        drop(expander);
        let value = first_set_value(&program).expect("a top-level define");
        check_migration_body("m", value, &syms, &declared).map(|d| d.message)
    }

    fn first_set_value(program: &CoreExpr) -> Option<&CoreExpr> {
        match program {
            CoreExpr::Begin { exprs, .. } => exprs.iter().find_map(first_set_value),
            CoreExpr::Set { value, .. } => Some(&**value),
            _ => None,
        }
    }

    #[test]
    fn pure_migration_is_allowed() {
        assert_eq!(migration_diag("(lambda (s) (cons s 0))"), None);
    }

    #[test]
    fn mutation_migration_is_allowed() {
        assert_eq!(
            migration_diag("(lambda (s) (begin (vector-set! s 0 1) s))"),
            None
        );
    }

    #[test]
    fn wall_clock_migration_is_rejected() {
        let d = migration_diag("(lambda (s) (cons s (current-time)))").expect("must reject");
        assert!(d.contains("wall-clock"), "{d}");
    }

    #[test]
    fn io_migration_is_rejected() {
        let d = migration_diag("(lambda (s) (begin (display s) s))").expect("must reject");
        assert!(d.contains("io"), "{d}");
    }
}
