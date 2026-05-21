//! Semantic tokens (Phase 5 iter 5.1).
//!
//! `textDocument/semanticTokens/full`: re-lexes the buffer with cs-lex
//! and classifies each identifier as a special form (keyword), a known
//! builtin (function), or anything else (variable); numbers and strings
//! get their own types. This is the value a TextMate grammar can't give —
//! the keyword-vs-builtin-vs-local distinction comes from the real lexer
//! plus the builtin table, not regexes.
//!
//! Output is the LSP delta-encoded form (5 u32 per token, each relative
//! to the previous token). Lengths are UTF-16 code units, per spec.
//! Lexing is best-effort: a lex error simply ends the token stream
//! (partial highlighting beats none while typing).

use cs_core::SymbolTable;
use cs_diag::SourceMap;
use cs_lex::{Lexer, Token};
use tower_lsp::lsp_types::{SemanticToken, SemanticTokenType, SemanticTokens};

use crate::builtins::builtin_doc;
use crate::text::offset_to_position;

/// Legend: token-type index → LSP type. The server capability advertises
/// this exact order; the `token_type` field below indexes into it.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,  // 0 — special forms
    SemanticTokenType::FUNCTION, // 1 — known builtins
    SemanticTokenType::VARIABLE, // 2 — everything else
    SemanticTokenType::NUMBER,   // 3
    SemanticTokenType::STRING,   // 4
];

const KEYWORD: u32 = 0;
const FUNCTION: u32 = 1;
const VARIABLE: u32 = 2;
const NUMBER: u32 = 3;
const STRING: u32 = 4;

/// R[567]RS special forms / syntactic keywords. Identifiers naming one of
/// these highlight as keywords rather than functions/variables.
const SPECIAL_FORMS: &[&str] = &[
    "define",
    "define-syntax",
    "define-values",
    "define-record-type",
    "lambda",
    "case-lambda",
    "named-lambda",
    "let",
    "let*",
    "letrec",
    "letrec*",
    "let-values",
    "let*-values",
    "let-syntax",
    "letrec-syntax",
    "if",
    "cond",
    "case",
    "when",
    "unless",
    "and",
    "or",
    "begin",
    "do",
    "quote",
    "quasiquote",
    "unquote",
    "unquote-splicing",
    "set!",
    "delay",
    "delay-force",
    "parameterize",
    "guard",
    "syntax-rules",
    "syntax-case",
    "else",
    "=>",
];

/// Semantic tokens for the whole buffer, delta-encoded per the LSP spec.
pub fn semantic_tokens(name: &str, text: &str) -> SemanticTokens {
    let raw = classify(name, text);
    let mut data = Vec::with_capacity(raw.len());
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for (line, start, length, token_type) in raw {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            start - prev_start
        } else {
            start
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });
        prev_line = line;
        prev_start = start;
    }
    SemanticTokens {
        result_id: None,
        data,
    }
}

/// Absolute `(line, start_utf16, length_utf16, type)` per token, in source
/// order. Stops at the first lex error (best-effort highlighting).
fn classify(name: &str, text: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut sources = SourceMap::new();
    let file = sources.add(name, text);
    let mut syms = SymbolTable::new();
    let mut lexer = Lexer::new(file, text);
    let mut out = Vec::new();
    loop {
        match lexer.next_token(&mut syms) {
            Ok((Token::Eof, _)) => break,
            Ok((tok, span)) => {
                let Some(ty) = token_type(&tok, &syms) else {
                    continue;
                };
                let (start, end) = (span.start as usize, span.end as usize);
                if end <= start || end > text.len() {
                    continue;
                }
                let pos = offset_to_position(text, start);
                let length = text[start..end].encode_utf16().count() as u32;
                if length == 0 {
                    continue;
                }
                out.push((pos.line, pos.character, length, ty));
            }
            Err(_) => break,
        }
    }
    out
}

fn token_type(tok: &Token, syms: &SymbolTable) -> Option<u32> {
    match tok {
        Token::Identifier(s) => {
            let n = syms.name(*s);
            Some(if SPECIAL_FORMS.contains(&n) {
                KEYWORD
            } else if builtin_doc(n).is_some() {
                FUNCTION
            } else {
                VARIABLE
            })
        }
        Token::Number(_) => Some(NUMBER),
        Token::String(_) => Some(STRING),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the delta stream back to absolute `(line, char, len, type)`
    /// so tests can assert on positions directly.
    fn decode(toks: &SemanticTokens) -> Vec<(u32, u32, u32, u32)> {
        let mut out = Vec::new();
        let (mut line, mut start) = (0u32, 0u32);
        for t in &toks.data {
            if t.delta_line == 0 {
                start += t.delta_start;
            } else {
                line += t.delta_line;
                start = t.delta_start;
            }
            out.push((line, start, t.length, t.token_type));
        }
        out
    }

    #[test]
    fn classifies_keyword_builtin_variable() {
        // (define x (cons 1 2))
        //  ^kw    ^var ^fn
        let toks = semantic_tokens("t.scm", "(define x (cons 1 2))");
        let d = decode(&toks);
        // define → keyword at col 1
        assert!(
            d.contains(&(0, 1, 6, KEYWORD)),
            "define not a keyword: {d:?}"
        );
        // x → variable at col 8
        assert!(d.contains(&(0, 8, 1, VARIABLE)), "x not a variable: {d:?}");
        // cons → function at col 11
        assert!(
            d.contains(&(0, 11, 4, FUNCTION)),
            "cons not a function: {d:?}"
        );
        // 1 and 2 → numbers
        assert_eq!(
            d.iter().filter(|&&(.., ty)| ty == NUMBER).count(),
            2,
            "expected two number tokens: {d:?}"
        );
    }

    #[test]
    fn delta_encodes_across_lines() {
        let toks = semantic_tokens("t.scm", "(define a 1)\n(define b 2)");
        let d = decode(&toks);
        // Two `define` keywords, one per line, both at col 1.
        let kws: Vec<_> = d.iter().filter(|&&(.., ty)| ty == KEYWORD).collect();
        assert_eq!(kws.len(), 2, "{d:?}");
        assert_eq!(*kws[0], (0, 1, 6, KEYWORD));
        assert_eq!(*kws[1], (1, 1, 6, KEYWORD));
    }

    #[test]
    fn strings_are_string_typed() {
        let toks = semantic_tokens("t.scm", "(display \"hi\")");
        let d = decode(&toks);
        assert!(
            d.iter().any(|&(.., ty)| ty == STRING),
            "no string token: {d:?}"
        );
    }

    #[test]
    fn partial_input_still_highlights() {
        // Unterminated list: still want tokens up to the lex point.
        let toks = semantic_tokens("t.scm", "(cons 1");
        let d = decode(&toks);
        assert!(
            d.iter().any(|&(.., ty)| ty == FUNCTION),
            "cons not highlighted in partial input: {d:?}"
        );
    }
}
