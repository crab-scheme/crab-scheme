//! Source → LSP diagnostics (Phase 1 iters 1.3/1.4/1.6).
//!
//! [`analyze`] runs the CrabScheme front-end on a document's text and
//! returns LSP diagnostics for any parse or expand errors. It reuses
//! `cs_parse::read_all` and `cs_expand::Expander` verbatim — the same
//! pipeline `crabscheme run` uses — so the squigglies an editor shows
//! match exactly what the compiler would report.
//!
//! Span conversion is UTF-16-aware: LSP `Position.character` counts
//! UTF-16 code units from the line start (not bytes), so a diagnostic
//! after a multibyte character lands in the right column.

use std::collections::HashMap;

use cs_core::SymbolTable;
use cs_diag::{SourceMap, Span};
use cs_expand::Expander;
use cs_parse::read_all;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// Parse + expand `text` (named `name` for the source map) and return
/// one LSP diagnostic per front-end error. Empty when the program is
/// well-formed.
///
/// Parse errors short-circuit expansion (you can't expand what didn't
/// read). All diagnostics are `ERROR` severity with source
/// `"crabscheme"`.
pub fn analyze(name: &str, text: &str) -> Vec<Diagnostic> {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let mut syms = SymbolTable::new();

    match read_all(file, text, &mut syms) {
        Err(errors) => errors
            .iter()
            .map(|e| diagnostic(&sources, e.span(), e.message()))
            .collect(),
        Ok(data) => {
            let mut macros = HashMap::new();
            let mut expander = Expander::new(&mut syms, &mut macros);
            match expander.expand_program(&data) {
                Ok(_) => Vec::new(),
                Err(e) => vec![diagnostic(&sources, e.span(), e.message())],
            }
        }
    }
}

fn diagnostic(sources: &SourceMap, span: Span, message: String) -> Diagnostic {
    Diagnostic {
        range: span_to_range(sources, span),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("crabscheme".to_string()),
        message,
        ..Default::default()
    }
}

/// Convert a `cs_diag::Span` to an LSP `Range` (0-based line, 0-based
/// UTF-16 character).
fn span_to_range(sources: &SourceMap, span: Span) -> Range {
    Range {
        start: offset_to_position(sources, span, span.start),
        end: offset_to_position(sources, span, span.end),
    }
}

/// Byte offset → LSP `Position`. `cs_diag::SourceMap::line_col` gives a
/// 1-based line and a *byte* column; LSP needs 0-based and UTF-16, so
/// compute both directly from the source text.
fn offset_to_position(sources: &SourceMap, span: Span, byte_off: u32) -> Position {
    if span.is_dummy() {
        return Position::new(0, 0);
    }
    let src = sources.contents(span.file);
    // Clamp to a char boundary defensively (spans are token-aligned,
    // but never index mid-codepoint).
    let mut off = (byte_off as usize).min(src.len());
    while off > 0 && !src.is_char_boundary(off) {
        off -= 1;
    }
    let before = &src[..off];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = src[line_start..off].encode_utf16().count() as u32;
    Position::new(line, character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_program_has_no_diagnostics() {
        assert!(analyze("<t>", "(define (f x) (+ x 1))\n(f 41)").is_empty());
    }

    #[test]
    fn unclosed_list_is_an_error() {
        let diags = analyze("<t>", "(+ 1 2");
        assert_eq!(diags.len(), 1, "got: {diags:?}");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].source.as_deref(), Some("crabscheme"));
    }

    #[test]
    fn expand_error_surfaces_after_clean_parse() {
        // Parses fine (balanced), but `(define)` is a malformed form the
        // expander rejects — proves the expand stage runs.
        let diags = analyze("<t>", "(define)");
        assert_eq!(diags.len(), 1, "got: {diags:?}");
    }

    #[test]
    fn diagnostic_position_is_zero_based() {
        // Error on the second line → line index 1 (0-based).
        let diags = analyze("<t>", "(define x 1)\n(+ 1 2");
        assert_eq!(diags.len(), 1, "got: {diags:?}");
        assert_eq!(diags[0].range.start.line, 1, "got: {:?}", diags[0].range);
    }

    #[test]
    fn utf16_column_counts_code_units_not_bytes() {
        // "λx": λ is 2 UTF-8 bytes but 1 UTF-16 code unit, so 'x' (byte
        // offset 2) sits at UTF-16 character 1 — span→range must report
        // 1, not the byte offset 2. Tested directly on a crafted span so
        // it doesn't depend on where the reader happens to flag an error.
        let mut sm = SourceMap::new();
        let file = sm.add("<t>", "λx");
        let range = span_to_range(
            &sm,
            Span {
                file,
                start: 2,
                end: 3,
            },
        );
        assert_eq!(range.start.line, 0);
        assert_eq!(
            range.start.character, 1,
            "λ is 1 UTF-16 unit; got: {range:?}"
        );
        assert_eq!(
            range.end.character, 2,
            "x ends at UTF-16 char 2; got: {range:?}"
        );
    }

    #[test]
    fn span_on_later_line_reports_zero_based_line() {
        let mut sm = SourceMap::new();
        let file = sm.add("<t>", "a\nbc\nd");
        // 'c' is at byte 3 → line 1 (0-based), character 1.
        let range = span_to_range(
            &sm,
            Span {
                file,
                start: 3,
                end: 4,
            },
        );
        assert_eq!(
            (range.start.line, range.start.character),
            (1, 1),
            "got: {range:?}"
        );
    }
}
