//! Agent / coding-harness API (Phase 6).
//!
//! Plain, serde-serializable results for CrabScheme code intelligence,
//! shared by the headless CLI (`crabscheme-lsp <subcommand>`) and the
//! MCP server (`crabscheme-mcp`). Both surface the *same* analysis the
//! LSP gives editors — reusing [`crate::diagnostics`], [`crate::symbols`],
//! [`crate::references`], [`crate::hover`], [`crate::format`], and
//! [`crate::workspace`] — but shaped for tools rather than editors:
//!
//! * positions are **1-based line, 1-based column** (column in UTF-16
//!   code units, i.e. LSP+1) — the coordinates humans and agents already
//!   use when reading compiler/editor output;
//! * symbol kinds and diagnostic severities are lowercase strings;
//! * everything derives `Serialize`, so callers emit JSON directly.
//!
//! The LSP keeps speaking 0-based UTF-16 to editors; this module is the
//! translation layer to the harness coordinate system. Every entry point
//! is a pure function of its inputs — no I/O, no global state — so the
//! CLI and MCP server stay thin.

use std::path::Path;

use serde::Serialize;
use tower_lsp::lsp_types::{
    DiagnosticSeverity, DocumentSymbol, HoverContents, MarkedString, Position, Range, SymbolKind,
};

/// A 1-based source position. `col` is a UTF-16 code-unit offset + 1
/// (matches LSP semantics; for ASCII it's just the character column).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    /// Build from a caller-supplied 1-based line/col.
    pub fn new(line: u32, col: u32) -> Self {
        Pos { line, col }
    }

    fn from_lsp(p: Position) -> Self {
        Pos {
            line: p.line + 1,
            col: p.character + 1,
        }
    }

    fn to_lsp(self) -> Position {
        Position {
            line: self.line.saturating_sub(1),
            character: self.col.saturating_sub(1),
        }
    }
}

/// A 1-based source range (half-open in LSP terms; `end` is exclusive).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Loc {
    pub start: Pos,
    pub end: Pos,
}

impl Loc {
    fn from_range(r: Range) -> Self {
        Loc {
            start: Pos::from_lsp(r.start),
            end: Pos::from_lsp(r.end),
        }
    }
}

/// One front-end diagnostic.
#[derive(Debug, Clone, Serialize)]
pub struct Diag {
    pub severity: &'static str,
    pub message: String,
    pub range: Loc,
}

/// One symbol (a `define`d name) in a document outline.
#[derive(Debug, Clone, Serialize)]
pub struct Sym {
    pub name: String,
    pub kind: &'static str,
    pub range: Loc,
}

/// One workspace-symbol hit: a `Sym` plus the file it lives in.
#[derive(Debug, Clone, Serialize)]
pub struct WsSym {
    pub name: String,
    pub kind: &'static str,
    pub path: String,
    pub range: Loc,
}

/// Parse + expand `text` (named `name`) and return its diagnostics.
/// Empty when the program is well-formed.
pub fn check(name: &str, text: &str) -> Vec<Diag> {
    crate::diagnostics::analyze(name, text)
        .into_iter()
        .map(|d| Diag {
            severity: severity_str(d.severity),
            message: d.message,
            range: Loc::from_range(d.range),
        })
        .collect()
}

/// The document outline: every top-level and nested `define`, flattened.
pub fn symbols(name: &str, text: &str) -> Vec<Sym> {
    let mut out = Vec::new();
    flatten(&crate::symbols::document_symbols(name, text), &mut out);
    out
}

/// Definition site of the identifier at `pos`, if it is `define`d here.
pub fn definition(name: &str, text: &str, pos: Pos) -> Option<Loc> {
    crate::references::definition(name, text, pos.to_lsp()).map(Loc::from_range)
}

/// Every reference (definition included) to the identifier at `pos`.
pub fn references(name: &str, text: &str, pos: Pos) -> Vec<Loc> {
    crate::references::references(name, text, pos.to_lsp(), true)
        .into_iter()
        .map(Loc::from_range)
        .collect()
}

/// Hover documentation for the identifier at `pos`, as plain text.
pub fn hover(name: &str, text: &str, pos: Pos) -> Option<String> {
    crate::hover::hover(name, text, pos.to_lsp()).map(|h| match h.contents {
        HoverContents::Markup(m) => m.value,
        HoverContents::Scalar(MarkedString::String(s)) => s,
        HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
        HoverContents::Array(parts) => parts
            .into_iter()
            .map(|m| match m {
                MarkedString::String(s) => s,
                MarkedString::LanguageString(ls) => ls.value,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// Reformat source with canonical indentation.
pub fn format(text: &str) -> String {
    crate::format::format(text)
}

/// Workspace-wide symbol search: every `define` under `root` whose name
/// contains `query` (case-insensitive; empty matches all).
pub fn workspace_symbols(root: &Path, query: &str) -> Vec<WsSym> {
    #[allow(deprecated)] // SymbolInformation is deprecated upstream
    crate::workspace::workspace_symbols(root, query)
        .into_iter()
        .map(|si| WsSym {
            name: si.name,
            kind: kind_str(si.kind),
            path: si
                .location
                .uri
                .to_file_path()
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| si.location.uri.to_string()),
            range: Loc::from_range(si.location.range),
        })
        .collect()
}

#[allow(deprecated)] // DocumentSymbol::deprecated field is required to construct
fn flatten(syms: &[DocumentSymbol], out: &mut Vec<Sym>) {
    for s in syms {
        out.push(Sym {
            name: s.name.clone(),
            kind: kind_str(s.kind),
            range: Loc::from_range(s.selection_range),
        });
        if let Some(children) = &s.children {
            flatten(children, out);
        }
    }
}

fn severity_str(s: Option<DiagnosticSeverity>) -> &'static str {
    match s {
        Some(DiagnosticSeverity::WARNING) => "warning",
        Some(DiagnosticSeverity::INFORMATION) => "info",
        Some(DiagnosticSeverity::HINT) => "hint",
        _ => "error",
    }
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_reports_one_based_positions() {
        // Unclosed list on line 1 (1-based).
        let diags = check("t.scm", "(+ 1 2");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].severity, "error");
        assert_eq!(diags[0].range.start.line, 1, "should be 1-based line");
    }

    #[test]
    fn check_empty_for_valid() {
        assert!(check("t.scm", "(define x 1)").is_empty());
    }

    #[test]
    fn symbols_flattens_defines() {
        let syms = symbols("t.scm", "(define (f x) x)\n(define y 2)");
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"f"), "{names:?}");
        assert!(names.contains(&"y"), "{names:?}");
        let f = syms.iter().find(|s| s.name == "f").unwrap();
        assert_eq!(f.kind, "function");
        assert_eq!(f.range.start.line, 1, "f defined on line 1 (1-based)");
    }

    #[test]
    fn definition_and_references_round_trip() {
        let text = "(define (f x) x)\n(f 1)\n(f 2)";
        // Cursor on the use of `f` at line 2, col 2 (1-based).
        let def = definition("t.scm", text, Pos::new(2, 2)).expect("def");
        assert_eq!(def.start.line, 1, "definition on line 1");
        // 3 occurrences: define + two uses.
        let refs = references("t.scm", text, Pos::new(2, 2));
        assert_eq!(refs.len(), 3, "{refs:?}");
    }

    #[test]
    fn hover_describes_builtin() {
        let text = "(cons 1 2)";
        let h = hover("t.scm", text, Pos::new(1, 2)).expect("hover");
        assert!(h.contains("cons"), "hover: {h}");
    }

    #[test]
    fn format_reindents() {
        assert_eq!(
            format("(define (f x)\n(+ x\n1))"),
            "(define (f x)\n  (+ x\n    1))"
        );
    }

    #[test]
    fn serializes_to_json() {
        let diags = check("t.scm", "(+ 1 2");
        let json = serde_json::to_string(&diags).unwrap();
        assert!(json.contains("\"severity\":\"error\""), "{json}");
        assert!(json.contains("\"line\":1"), "{json}");
    }
}
