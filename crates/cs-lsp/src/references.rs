//! Go-to-definition, find-references, and document-highlight
//! (Phase 3 iters 3.2/3.4/3.5).
//!
//! All same-file (cross-file lookup is Phase 5). Resolution is by symbol
//! id, which the reader interns per name — so references match every
//! textual use of the identifier, including its definition. Lexical
//! scope / hygiene is not honored yet (the Phase 3.1 expander-scope
//! refinement); shadowed bindings with the same name are conflated.

use cs_core::SymbolTable;
use cs_diag::SourceMap;
use cs_parse::{read_all, Datum};
use tower_lsp::lsp_types::{Position, Range};

use crate::symbols::{collect_symbol_spans, find_define_span, symbol_at};
use crate::text::{position_to_offset, span_to_range};

fn parse(name: &str, text: &str) -> Option<(SymbolTable, Vec<Datum>)> {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let mut syms = SymbolTable::new();
    let data = read_all(file, text, &mut syms).ok()?;
    Some((syms, data))
}

/// The definition site (name span) of the identifier under `position`,
/// as a range in this document. `None` if not on an identifier or it
/// isn't `define`d here.
pub fn definition(name: &str, text: &str, position: Position) -> Option<Range> {
    let (mut syms, data) = parse(name, text)?;
    let offset = position_to_offset(text, position);
    let forms: Vec<&Datum> = data.iter().collect();
    let (sym, _) = symbol_at(&forms, offset)?;
    let define = syms.intern("define");
    let span = find_define_span(&forms, define, sym)?;
    Some(span_to_range(text, span))
}

/// Every occurrence of the identifier under `position`. When
/// `include_definition` is false, the `define` name span is omitted.
pub fn references(
    name: &str,
    text: &str,
    position: Position,
    include_definition: bool,
) -> Vec<Range> {
    let Some((mut syms, data)) = parse(name, text) else {
        return Vec::new();
    };
    let offset = position_to_offset(text, position);
    let forms: Vec<&Datum> = data.iter().collect();
    let Some((sym, _)) = symbol_at(&forms, offset) else {
        return Vec::new();
    };
    let define = syms.intern("define");
    let def_span = find_define_span(&forms, define, sym);
    collect_symbol_spans(&forms, sym)
        .into_iter()
        .filter(|sp| include_definition || Some(*sp) != def_span)
        .map(|sp| span_to_range(text, sp))
        .collect()
}

/// All occurrences of the identifier under `position` in this document
/// (for `documentHighlight` — same machinery as references).
pub fn document_highlights(name: &str, text: &str, position: Position) -> Vec<Range> {
    references(name, text, position, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "(define (f x) (f x))\n(f 1)";

    #[test]
    fn definition_of_use_points_to_define() {
        // hover the use on line 1 → definition is f's name on line 0.
        let r = definition("<t>", SRC, Position::new(1, 1)).expect("definition");
        assert_eq!(r.start.line, 0, "got: {r:?}");
        // "f" in "(define (f x)" is at character 9.
        assert_eq!(r.start.character, 9, "got: {r:?}");
    }

    #[test]
    fn references_find_all_three_occurrences() {
        // f appears: define name (L0), recursive call (L0), use (L1).
        let refs = references("<t>", SRC, Position::new(1, 1), true);
        assert_eq!(refs.len(), 3, "got: {refs:?}");
    }

    #[test]
    fn references_excluding_declaration_drops_the_define() {
        let refs = references("<t>", SRC, Position::new(1, 1), false);
        assert_eq!(refs.len(), 2, "got: {refs:?}");
        // none of them is the define-name span (line 0, char 9)
        assert!(
            !refs
                .iter()
                .any(|r| r.start.line == 0 && r.start.character == 9),
            "declaration should be excluded: {refs:?}"
        );
    }

    #[test]
    fn definition_off_identifier_is_none() {
        assert!(definition("<t>", SRC, Position::new(5, 0)).is_none());
    }

    #[test]
    fn highlights_match_references() {
        let hl = document_highlights("<t>", SRC, Position::new(0, 9));
        assert_eq!(hl.len(), 3, "got: {hl:?}");
    }
}
