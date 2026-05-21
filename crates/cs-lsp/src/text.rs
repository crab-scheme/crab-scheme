//! Coordinate conversion between CrabScheme byte spans and LSP
//! positions. Shared by diagnostics, document symbols, and hover.
//!
//! Each document is parsed as a single source file, so a `cs_diag::Span`
//! and an LSP position are both just views on the same text: spans use
//! byte offsets, LSP uses (0-based line, 0-based UTF-16 character).

use cs_diag::Span;
use tower_lsp::lsp_types::{Position, Range};

/// Convert a byte span to an LSP range against `text`.
pub fn span_to_range(text: &str, span: Span) -> Range {
    if span.is_dummy() {
        return Range::new(Position::new(0, 0), Position::new(0, 0));
    }
    Range::new(
        offset_to_position(text, span.start as usize),
        offset_to_position(text, span.end as usize),
    )
}

/// Byte offset → LSP position (0-based line, 0-based UTF-16 character).
pub fn offset_to_position(text: &str, byte_off: usize) -> Position {
    let mut off = byte_off.min(text.len());
    while off > 0 && !text.is_char_boundary(off) {
        off -= 1;
    }
    let before = &text[..off];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let character = text[line_start..off].encode_utf16().count() as u32;
    Position::new(line, character)
}

/// LSP position → byte offset into `text`. Clamps past-EOF positions to
/// `text.len()` and past-EOL characters to the line's end.
pub fn position_to_offset(text: &str, pos: Position) -> usize {
    // Byte offset of the start of line `pos.line`.
    let mut line_start = 0usize;
    if pos.line > 0 {
        let mut cur = 0u32;
        let mut found = false;
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                cur += 1;
                if cur == pos.line {
                    line_start = i + 1;
                    found = true;
                    break;
                }
            }
        }
        if !found {
            return text.len();
        }
    }
    // Walk UTF-16 code units across the line to `pos.character`.
    let mut utf16 = 0u32;
    let mut byte = line_start;
    for ch in text[line_start..].chars() {
        if ch == '\n' || utf16 >= pos.character {
            break;
        }
        utf16 += ch.len_utf16() as u32;
        byte += ch.len_utf8();
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_position_roundtrip_ascii() {
        let t = "abc\ndef";
        assert_eq!(offset_to_position(t, 5), Position::new(1, 1)); // 'e'
        assert_eq!(position_to_offset(t, Position::new(1, 1)), 5);
    }

    #[test]
    fn utf16_units_not_bytes() {
        let t = "λx"; // λ = 2 bytes, 1 UTF-16 unit
        assert_eq!(offset_to_position(t, 2), Position::new(0, 1)); // 'x'
        assert_eq!(position_to_offset(t, Position::new(0, 1)), 2);
    }

    #[test]
    fn position_past_eof_clamps() {
        let t = "ab";
        assert_eq!(position_to_offset(t, Position::new(9, 9)), t.len());
    }
}
