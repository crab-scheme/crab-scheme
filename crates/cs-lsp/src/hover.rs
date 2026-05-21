//! Hover (Phase 2 iters 2.3/2.4).
//!
//! Resolves the identifier under the cursor and shows: a builtin/
//! special-form signature (from [`crate::builtins`]), or "defined at
//! line N" for a user binding, or "unbound" otherwise.

use cs_core::{Symbol, SymbolTable};
use cs_diag::{SourceMap, Span};
use cs_parse::{read_all, Datum};
use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position};

use crate::builtins::builtin_doc;
use crate::symbols::find_define_span;
use crate::text::{offset_to_position, position_to_offset, span_to_range};

/// Hover info for `position` in `text`, or `None` if the cursor isn't on
/// an identifier (or the source doesn't parse).
pub fn hover(name: &str, text: &str, position: Position) -> Option<Hover> {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let mut syms = SymbolTable::new();
    let data = read_all(file, text, &mut syms).ok()?;

    let offset = position_to_offset(text, position);
    let forms: Vec<&Datum> = data.iter().collect();
    let (sym, span) = symbol_at(&forms, offset)?;
    let ident = syms.name(sym).to_string();
    let define = syms.intern("define");

    let markdown = if let Some(doc) = builtin_doc(&ident) {
        doc.to_string()
    } else if let Some(def_span) = find_define_span(&forms, define, sym) {
        let line = offset_to_position(text, def_span.start as usize).line + 1;
        format!("`{ident}` — defined at line {line}")
    } else {
        format!("`{ident}` — unbound")
    };

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: Some(span_to_range(text, span)),
    })
}

/// Innermost identifier whose span contains `offset`, across all forms.
fn symbol_at(forms: &[&Datum], offset: usize) -> Option<(Symbol, Span)> {
    forms.iter().find_map(|f| symbol_at_datum(f, offset))
}

fn symbol_at_datum(d: &Datum, offset: usize) -> Option<(Symbol, Span)> {
    if !span_contains(d.span(), offset) {
        return None;
    }
    match d {
        Datum::Symbol(s, sp) => Some((*s, *sp)),
        Datum::Pair(car, cdr, _) => {
            symbol_at_datum(car, offset).or_else(|| symbol_at_datum(cdr, offset))
        }
        Datum::Vector(elems, _) => elems.iter().find_map(|e| symbol_at_datum(e, offset)),
        _ => None,
    }
}

fn span_contains(span: Span, offset: usize) -> bool {
    !span.is_dummy() && (span.start as usize) <= offset && offset < (span.end as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hover_text(src: &str, pos: Position) -> String {
        match hover("<t>", src, pos).expect("hover").contents {
            HoverContents::Markup(m) => m.value,
            other => panic!("expected markup, got {other:?}"),
        }
    }

    #[test]
    fn hover_builtin_shows_signature() {
        // cursor inside "cons" of "(cons 1 2)"
        let md = hover_text("(cons 1 2)", Position::new(0, 2));
        assert!(md.contains("pair"), "got: {md}");
    }

    #[test]
    fn hover_special_form() {
        let md = hover_text("(lambda (x) x)", Position::new(0, 3));
        assert!(md.contains("procedure"), "got: {md}");
    }

    #[test]
    fn hover_user_binding_shows_definition_line() {
        // f defined on line 0; hover its use on line 1.
        let md = hover_text("(define (f x) x)\n(f 1)", Position::new(1, 1));
        assert!(md.contains("defined at line 1"), "got: {md}");
    }

    #[test]
    fn hover_unbound_identifier() {
        let md = hover_text("(zork 1)", Position::new(0, 2));
        assert!(md.contains("unbound"), "got: {md}");
    }

    #[test]
    fn hover_off_identifier_is_none() {
        // Position on whitespace / past content.
        assert!(hover("<t>", "(cons 1 2)", Position::new(5, 0)).is_none());
    }
}
