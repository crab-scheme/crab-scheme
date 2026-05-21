//! Document symbols (Phase 2 iters 2.1/2.2) — the editor outline view.
//!
//! Walks the parsed datums for `define` forms and emits a nested
//! `DocumentSymbol` tree: `(define (f …) …)` → Function, `(define x …)`
//! → Variable, with inner defines (in lambda/let/begin bodies) surfaced
//! as children of their enclosing define.

use cs_core::{Symbol, SymbolTable};
use cs_diag::{SourceMap, Span};
use cs_parse::{read_all, Datum};
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};

use crate::text::span_to_range;

/// Build the outline for `text`. Empty if the source doesn't parse.
pub fn document_symbols(name: &str, text: &str) -> Vec<DocumentSymbol> {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let mut syms = SymbolTable::new();
    let data = match read_all(file, text, &mut syms) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let define = syms.intern("define");
    let forms: Vec<&Datum> = data.iter().collect();
    defines_in_forms(text, &syms, define, &forms)
}

/// Collect `define` forms in `forms` as DocumentSymbols. Non-define list
/// forms are recursed into so defines nested in begin/let/etc. surface
/// (attributed to the nearest enclosing define).
fn defines_in_forms(
    text: &str,
    syms: &SymbolTable,
    define: Symbol,
    forms: &[&Datum],
) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for &form in forms {
        if let Some(ds) = define_symbol(text, syms, define, form) {
            out.push(ds);
        } else if let Some(sub) = list_elements(form) {
            out.extend(defines_in_forms(text, syms, define, &sub));
        }
    }
    out
}

/// If `form` is a `define`, return its (name symbol, name span, kind).
/// Handles both `(define x …)` (Variable) and `(define (f …) …)`
/// (Function). `None` if `form` isn't a define.
pub(crate) fn define_head(form: &Datum, define: Symbol) -> Option<(Symbol, Span, SymbolKind)> {
    let elems = list_elements(form)?;
    if elems.len() < 2 || !is_symbol(elems[0], define) {
        return None;
    }
    match elems[1] {
        Datum::Symbol(s, sp) => Some((*s, *sp, SymbolKind::VARIABLE)),
        head_list => {
            let inner = list_elements(head_list)?;
            match inner.first()? {
                Datum::Symbol(s, sp) => Some((*s, *sp, SymbolKind::FUNCTION)),
                _ => None,
            }
        }
    }
}

#[allow(deprecated)] // DocumentSymbol::deprecated is a deprecated LSP field
fn define_symbol(
    text: &str,
    syms: &SymbolTable,
    define: Symbol,
    form: &Datum,
) -> Option<DocumentSymbol> {
    let (name_sym, name_span, kind) = define_head(form, define)?;
    // Body forms are everything after the name (elems[2..]).
    let elems = list_elements(form)?;
    let children = defines_in_forms(text, syms, define, &elems[2..]);
    Some(DocumentSymbol {
        name: syms.name(name_sym).to_string(),
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(text, form.span()),
        selection_range: span_to_range(text, name_span),
        children: (!children.is_empty()).then_some(children),
    })
}

/// Find where `target` is `define`d in `forms` (searching nested
/// bodies), returning the span of its name. Used by hover's
/// "defined at line N".
pub(crate) fn find_define_span(forms: &[&Datum], define: Symbol, target: Symbol) -> Option<Span> {
    for &form in forms {
        if let Some((sym, span, _)) = define_head(form, define) {
            if sym == target {
                return Some(span);
            }
            if let Some(elems) = list_elements(form) {
                if let Some(s) = find_define_span(&elems[2..], define, target) {
                    return Some(s);
                }
            }
        } else if let Some(sub) = list_elements(form) {
            if let Some(s) = find_define_span(&sub, define, target) {
                return Some(s);
            }
        }
    }
    None
}

/// If `d` is a (possibly improper) list, return its car data in order;
/// atoms return `None`.
pub(crate) fn list_elements(d: &Datum) -> Option<Vec<&Datum>> {
    match d {
        Datum::Pair(..) => {
            let mut out = Vec::new();
            let mut cur = d;
            while let Datum::Pair(car, cdr, _) = cur {
                out.push(car.as_ref());
                cur = cdr.as_ref();
            }
            Some(out)
        }
        _ => None,
    }
}

pub(crate) fn is_symbol(d: &Datum, sym: Symbol) -> bool {
    matches!(d, Datum::Symbol(s, _) if *s == sym)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_function_and_variable() {
        let syms = document_symbols("<t>", "(define (f x) (+ x 1))\n(define y 2)");
        assert_eq!(syms.len(), 2, "got: {syms:?}");
        assert_eq!(syms[0].name, "f");
        assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
        assert_eq!(syms[1].name, "y");
        assert_eq!(syms[1].kind, SymbolKind::VARIABLE);
    }

    #[test]
    fn nested_define_is_a_child() {
        let syms = document_symbols("<t>", "(define (f x)\n  (define (g y) y)\n  (g x))");
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "f");
        let children = syms[0].children.as_ref().expect("f has children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "g");
        assert_eq!(children[0].kind, SymbolKind::FUNCTION);
    }

    #[test]
    fn selection_range_covers_just_the_name() {
        let syms = document_symbols("<t>", "(define (foo a) a)");
        // "foo" starts at byte 9 → character 9 on line 0.
        let sel = syms[0].selection_range;
        assert_eq!(sel.start.character, 9, "got: {sel:?}");
    }

    #[test]
    fn unparseable_source_yields_no_symbols() {
        assert!(document_symbols("<t>", "(define (f").is_empty());
    }
}
