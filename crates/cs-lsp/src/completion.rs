//! Completion + signature help (Phase 4 iters 4.1–4.5).
//!
//! Both fire while the user is mid-type, so the buffer often doesn't
//! parse (an open call has no close paren yet). So: completion always
//! offers builtins + special-form snippets (no parse needed) and adds
//! document-defined names best-effort; signature help finds the
//! enclosing call head *textually* (scan back to the unmatched `(`) so
//! it works on the incomplete call being typed.

use cs_core::SymbolTable;
use cs_diag::SourceMap;
use cs_parse::{read_all, Datum};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, DocumentSymbol, Documentation, InsertTextFormat,
    MarkupContent, MarkupKind, Position, SignatureHelp, SignatureInformation, SymbolKind,
};

use crate::builtins::{builtin_doc, signature, TABLE};
use crate::symbols::{document_symbols, find_define_signature};
use crate::text::position_to_offset;

/// Snippet scaffolds (LSP snippet syntax) for special forms.
const SNIPPETS: &[(&str, &str)] = &[
    ("define", "(define (${1:name} ${2:args}) ${0:body})"),
    ("lambda", "(lambda (${1:args}) ${0:body})"),
    ("let", "(let ((${1:name} ${2:val})) ${0:body})"),
    ("let*", "(let* ((${1:name} ${2:val})) ${0:body})"),
    ("letrec", "(letrec ((${1:name} ${2:val})) ${0:body})"),
    ("cond", "(cond (${1:test} ${2:body})\n  (else ${0:body}))"),
    (
        "case",
        "(case ${1:key}\n  ((${2:datum}) ${3:body})\n  (else ${0:body}))",
    ),
    ("when", "(when ${1:test} ${0:body})"),
    ("unless", "(unless ${1:test} ${0:body})"),
];

fn snippet_for(name: &str) -> Option<&'static str> {
    SNIPPETS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// Completion candidates: builtins/special forms (with snippets for the
/// latter) plus document-defined names. Independent of whether the
/// buffer parses — the static set is always available.
pub fn completion(name: &str, text: &str) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    for (n, doc) in TABLE {
        let detail = signature(n).unwrap_or(n).to_string();
        let documentation = Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
        if let Some(snip) = snippet_for(n) {
            items.push(CompletionItem {
                label: n.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some(detail),
                documentation,
                insert_text: Some(snip.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        } else {
            items.push(CompletionItem {
                label: n.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(detail),
                documentation,
                ..Default::default()
            });
        }
    }

    // Best-effort document-defined identifiers (empty if unparseable).
    let mut flat = Vec::new();
    flatten(&document_symbols(name, text), &mut flat);
    for (label, kind, line) in flat {
        items.push(CompletionItem {
            label,
            kind: Some(if kind == SymbolKind::FUNCTION {
                CompletionItemKind::FUNCTION
            } else {
                CompletionItemKind::VARIABLE
            }),
            detail: Some(format!("defined at line {}", line + 1)),
            ..Default::default()
        });
    }
    items
}

fn flatten(symbols: &[DocumentSymbol], out: &mut Vec<(String, SymbolKind, u32)>) {
    for s in symbols {
        out.push((s.name.clone(), s.kind, s.selection_range.start.line));
        if let Some(children) = &s.children {
            flatten(children, out);
        }
    }
}

/// Signature help for the call enclosing `position`: the builtin
/// signature, or a user function's `(name params…)`. `None` if the
/// cursor isn't inside a call with a known head.
pub fn signature_help(name: &str, text: &str, position: Position) -> Option<SignatureHelp> {
    let offset = position_to_offset(text, position);
    let head = enclosing_head(text, offset)?;

    if let Some(sig) = signature(&head) {
        return Some(make_signature(sig.to_string(), builtin_doc(&head)));
    }

    // User function — needs a parse; best-effort (often the rest of the
    // file is well-formed even while one call is being typed).
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let mut syms = SymbolTable::new();
    let data = read_all(file, text, &mut syms).ok()?;
    let forms: Vec<&Datum> = data.iter().collect();
    let define = syms.intern("define");
    let target = syms.intern(&head);
    let sig = find_define_signature(&forms, define, target, &syms)?;
    Some(make_signature(sig, None))
}

fn make_signature(label: String, doc: Option<&str>) -> SignatureHelp {
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: doc.map(|d| Documentation::String(d.to_string())),
            parameters: None,
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: None,
    }
}

/// The head identifier of the call enclosing byte `offset`, found by
/// scanning back to the nearest unmatched `(`. Byte-scan is safe: `(`
/// and `)` are ASCII and never appear inside a multibyte char.
fn enclosing_head(text: &str, offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = offset.min(bytes.len());
    let mut depth = 0i32;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' if depth == 0 => {
                let rest = &text[i + 1..offset.min(text.len())];
                let head = rest
                    .trim_start()
                    .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
                    .next()
                    .unwrap_or("");
                return (!head.is_empty()).then(|| head.to_string());
            }
            b'(' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn completion_offers_builtins_and_user_defines() {
        let items = completion("<t>", "(define (helper x) x)\n");
        let ls = labels(&items);
        assert!(ls.contains(&"cons"), "missing builtin cons");
        assert!(ls.contains(&"helper"), "missing user define helper");
    }

    // ---- Exit criterion 1: a `let` snippet completion ----
    #[test]
    fn let_snippet_completion_inserts_scaffold() {
        let items = completion("<t>", "");
        let let_item = items
            .iter()
            .find(|i| i.label == "let")
            .expect("a `let` completion item");
        assert_eq!(
            let_item.insert_text_format,
            Some(InsertTextFormat::SNIPPET),
            "let completion should be a snippet"
        );
        let snippet = let_item.insert_text.as_deref().unwrap_or("");
        assert!(snippet.contains("(let (("), "scaffold wrong: {snippet}");
        assert!(snippet.contains("${1:"), "no tab stops: {snippet}");
    }

    // ---- Exit criterion 2: signature help for (cons … ----
    #[test]
    fn signature_help_for_cons_even_when_incomplete() {
        // Incomplete call (no close paren) — the realistic mid-type case.
        let help = signature_help("<t>", "(cons ", Position::new(0, 6)).expect("signature help");
        assert_eq!(help.signatures[0].label, "(cons obj1 obj2)");
    }

    #[test]
    fn signature_help_for_user_function() {
        // User-function signatures need the buffer to parse (unlike the
        // textual builtin path), so use a complete call.
        let src = "(define (area w h) (* w h))\n(area 1 2)";
        let help = signature_help("<t>", src, Position::new(1, 6)).expect("signature help");
        assert_eq!(help.signatures[0].label, "(area w h)");
    }

    #[test]
    fn no_signature_help_outside_a_call() {
        assert!(signature_help("<t>", "cons", Position::new(0, 2)).is_none());
    }

    #[test]
    fn enclosing_head_skips_nested_calls() {
        // cursor after the inner "(* w " → head is `*`, not `area`.
        let src = "(area (* w ";
        assert_eq!(enclosing_head(src, src.len()).as_deref(), Some("*"));
    }
}
