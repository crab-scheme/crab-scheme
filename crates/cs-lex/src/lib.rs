//! Lexer for CrabScheme.
//!
//! This is a hand-rolled tokenizer (logos available in deps for future use).
//! Foundation milestone covers a meaningful subset of R6RS §4.2 lexical
//! syntax: numbers (fixnum, flonum), booleans, characters (named + literal),
//! strings (with common escapes), symbols, parens/brackets, quote family,
//! datum comment, line comment, block comment (nested).

use cs_core::{Number, Symbol, SymbolTable};
use cs_diag::{FileId, Span};

#[derive(Clone, Debug)]
pub enum Token {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Quote,
    QuasiQuote,
    Unquote,
    UnquoteSplicing,
    Dot,
    HashLParen,   // #(
    HashU8LParen, // #u8(  — R7RS bytevector literal opener
    HashSemi,     // #;
    Boolean(bool),
    Number(Number),
    String(String),
    Character(char),
    Identifier(Symbol),
    Eof,
}

#[derive(Clone, Debug)]
pub enum LexError {
    UnexpectedChar { c: char, span: Span },
    UnterminatedString { span: Span },
    UnterminatedBlockComment { span: Span },
    BadEscape { span: Span },
    BadCharacter { span: Span },
    BadNumber { span: Span, src: String },
}

impl LexError {
    pub fn span(&self) -> Span {
        match self {
            LexError::UnexpectedChar { span, .. }
            | LexError::UnterminatedString { span }
            | LexError::UnterminatedBlockComment { span }
            | LexError::BadEscape { span }
            | LexError::BadCharacter { span }
            | LexError::BadNumber { span, .. } => *span,
        }
    }

    pub fn message(&self) -> String {
        match self {
            LexError::UnexpectedChar { c, .. } => format!("unexpected character '{}'", c),
            LexError::UnterminatedString { .. } => "unterminated string literal".into(),
            LexError::UnterminatedBlockComment { .. } => "unterminated block comment".into(),
            LexError::BadEscape { .. } => "invalid escape sequence in string".into(),
            LexError::BadCharacter { .. } => "invalid character literal".into(),
            LexError::BadNumber { src, .. } => format!("invalid number literal '{}'", src),
        }
    }
}

pub struct Lexer<'src> {
    file: FileId,
    src: &'src str,
    bytes: &'src [u8],
    pos: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(file: FileId, src: &'src str) -> Self {
        Self {
            file,
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start as u32, end as u32)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<u8> {
        self.bytes.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), LexError> {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => {
                    self.bump();
                }
                Some(b';') => {
                    while let Some(b) = self.peek() {
                        self.bump();
                        if b == b'\n' {
                            break;
                        }
                    }
                }
                Some(b'#') if matches!(self.peek2(), Some(b'|')) => {
                    let start = self.pos;
                    self.bump();
                    self.bump();
                    let mut depth = 1usize;
                    while depth > 0 {
                        match (self.peek(), self.peek2()) {
                            (Some(b'#'), Some(b'|')) => {
                                self.bump();
                                self.bump();
                                depth += 1;
                            }
                            (Some(b'|'), Some(b'#')) => {
                                self.bump();
                                self.bump();
                                depth -= 1;
                            }
                            (Some(_), _) => {
                                self.bump();
                            }
                            (None, _) => {
                                return Err(LexError::UnterminatedBlockComment {
                                    span: self.span(start, self.pos),
                                });
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    pub fn next_token(&mut self, syms: &mut SymbolTable) -> Result<(Token, Span), LexError> {
        self.skip_whitespace_and_comments()?;
        let start = self.pos;
        let Some(b) = self.peek() else {
            return Ok((Token::Eof, self.span(start, start)));
        };
        match b {
            b'(' => {
                self.bump();
                Ok((Token::LParen, self.span(start, self.pos)))
            }
            b')' => {
                self.bump();
                Ok((Token::RParen, self.span(start, self.pos)))
            }
            b'[' => {
                self.bump();
                Ok((Token::LBracket, self.span(start, self.pos)))
            }
            b']' => {
                self.bump();
                Ok((Token::RBracket, self.span(start, self.pos)))
            }
            b'\'' => {
                self.bump();
                Ok((Token::Quote, self.span(start, self.pos)))
            }
            b'`' => {
                self.bump();
                Ok((Token::QuasiQuote, self.span(start, self.pos)))
            }
            b',' => {
                self.bump();
                if self.peek() == Some(b'@') {
                    self.bump();
                    Ok((Token::UnquoteSplicing, self.span(start, self.pos)))
                } else {
                    Ok((Token::Unquote, self.span(start, self.pos)))
                }
            }
            b'"' => self.read_string(start),
            b'#' => self.read_hash(start, syms),
            c if c.is_ascii_digit() => self.read_number(start),
            b'+' | b'-' => {
                // Could be sign-prefixed number, special infinity/NaN
                // literal (`+inf.0`, `-nan.0`, etc.), or a bare `+`/`-`
                // identifier.
                if let Some(next) = self.peek2() {
                    if next.is_ascii_digit() || next == b'.' {
                        return self.read_number(start);
                    }
                    // Sniff for `[+-](inf|nan)\.0` literals by peeking
                    // the next 5 bytes after the sign. Only match when
                    // followed by a delimiter so `+inflate` etc. stay as
                    // identifiers.
                    let after_sign = self.pos + 1;
                    let tail = self.bytes.get(after_sign..after_sign + 5);
                    if matches!(tail, Some(b"inf.0") | Some(b"nan.0")) {
                        let after_lit = after_sign + 5;
                        let next_after = self.bytes.get(after_lit).copied();
                        if next_after.map_or(true, is_delimiter) {
                            return self.read_number(start);
                        }
                    }
                }
                self.read_identifier(start, syms)
            }
            b'.' => {
                // Could be `.` token or part of an identifier or number
                if let Some(next) = self.peek2() {
                    if next.is_ascii_digit() {
                        return self.read_number(start);
                    }
                    if is_delimiter(next) {
                        self.bump();
                        return Ok((Token::Dot, self.span(start, self.pos)));
                    }
                } else {
                    self.bump();
                    return Ok((Token::Dot, self.span(start, self.pos)));
                }
                self.read_identifier(start, syms)
            }
            c if is_initial_ident(c) => self.read_identifier(start, syms),
            c => {
                self.bump();
                Err(LexError::UnexpectedChar {
                    c: c as char,
                    span: self.span(start, self.pos),
                })
            }
        }
    }

    fn read_string(&mut self, start: usize) -> Result<(Token, Span), LexError> {
        self.bump(); // consume opening "
        let mut out = String::new();
        loop {
            match self.peek() {
                None => {
                    return Err(LexError::UnterminatedString {
                        span: self.span(start, self.pos),
                    });
                }
                Some(b'"') => {
                    self.bump();
                    return Ok((Token::String(out), self.span(start, self.pos)));
                }
                Some(b'\\') => {
                    self.bump();
                    let esc_start = self.pos;
                    match self.bump() {
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'a') => out.push('\u{07}'),
                        Some(b'b') => out.push('\u{08}'),
                        Some(b'0') => out.push('\0'),
                        _ => {
                            return Err(LexError::BadEscape {
                                span: self.span(esc_start, self.pos),
                            });
                        }
                    }
                }
                Some(_) => {
                    let ch = self.read_char_at_pos();
                    out.push(ch);
                }
            }
        }
    }

    fn read_char_at_pos(&mut self) -> char {
        let s = &self.src[self.pos..];
        let ch = s.chars().next().unwrap();
        self.pos += ch.len_utf8();
        ch
    }

    fn read_hash(
        &mut self,
        start: usize,
        _syms: &mut SymbolTable,
    ) -> Result<(Token, Span), LexError> {
        self.bump(); // consume #
        match self.peek() {
            Some(b't') => {
                self.bump();
                Ok((Token::Boolean(true), self.span(start, self.pos)))
            }
            Some(b'f') => {
                self.bump();
                Ok((Token::Boolean(false), self.span(start, self.pos)))
            }
            Some(b'(') => {
                self.bump();
                Ok((Token::HashLParen, self.span(start, self.pos)))
            }
            Some(b';') => {
                self.bump();
                Ok((Token::HashSemi, self.span(start, self.pos)))
            }
            Some(b'\\') => self.read_character(start),
            Some(b'x') | Some(b'X') => self.read_radix_number(start, 16),
            Some(b'b') | Some(b'B') => self.read_radix_number(start, 2),
            Some(b'o') | Some(b'O') => self.read_radix_number(start, 8),
            Some(b'd') | Some(b'D') => {
                // #d means decimal — consume the prefix then read number normally.
                self.bump();
                self.read_number(start)
            }
            // R7RS `#u8(` opens a bytevector literal. The lexer just
            // emits the open-paren token; the parser collects the
            // following u8 datums and builds a ByteVector.
            Some(b'u')
                if self.bytes.get(self.pos + 1).copied() == Some(b'8')
                    && self.bytes.get(self.pos + 2).copied() == Some(b'(') =>
            {
                self.bump(); // u
                self.bump(); // 8
                self.bump(); // (
                Ok((Token::HashU8LParen, self.span(start, self.pos)))
            }
            _ => Err(LexError::UnexpectedChar {
                c: '#',
                span: self.span(start, self.pos),
            }),
        }
    }

    fn read_radix_number(&mut self, start: usize, radix: u32) -> Result<(Token, Span), LexError> {
        self.bump(); // consume the radix marker letter
        let digits_start = self.pos;
        if matches!(self.peek(), Some(b'+') | Some(b'-')) {
            self.bump();
        }
        while let Some(b) = self.peek() {
            let valid = match radix {
                2 => matches!(b, b'0' | b'1'),
                8 => matches!(b, b'0'..=b'7'),
                10 => b.is_ascii_digit(),
                16 => b.is_ascii_hexdigit(),
                _ => false,
            };
            if !valid {
                break;
            }
            self.bump();
        }
        let text = &self.src[digits_start..self.pos];
        let span = self.span(start, self.pos);
        if text.is_empty() || text == "+" || text == "-" {
            return Err(LexError::BadNumber {
                span,
                src: text.into(),
            });
        }
        let num = i64::from_str_radix(text, radix).map_err(|_| LexError::BadNumber {
            span,
            src: text.into(),
        })?;
        Ok((Token::Number(Number::Fixnum(num)), span))
    }

    fn read_character(&mut self, start: usize) -> Result<(Token, Span), LexError> {
        self.bump(); // consume backslash
        let name_start = self.pos;
        // Always consume at least one character — even if it's a
        // delimiter (e.g. `#\(`, `#\ `, `#\)`). After that, keep
        // consuming non-delimiters for multi-char names like
        // `#\space`, `#\xFF`.
        if self.peek().is_some() {
            self.bump();
        }
        while let Some(b) = self.peek() {
            if is_delimiter(b) {
                break;
            }
            self.bump();
        }
        let name = &self.src[name_start..self.pos];
        // R7RS named characters + R6RS xHHHH; hex form.
        let ch = match name {
            "space" => ' ',
            "newline" => '\n',
            "tab" => '\t',
            "return" => '\r',
            "nul" | "null" => '\0',
            "alarm" => '\u{07}',
            "backspace" => '\u{08}',
            "delete" => '\u{7F}',
            "escape" => '\u{1B}',
            // R7RS `#\xH...` hex literal (no semicolon required).
            // R6RS `#\xH...;` hex literal — we accept either.
            s if s.starts_with('x') && s.len() > 1 => {
                let hex = s[1..].trim_end_matches(';');
                match u32::from_str_radix(hex, 16) {
                    Ok(cp) => match char::from_u32(cp) {
                        Some(c) => c,
                        None => {
                            return Err(LexError::BadCharacter {
                                span: self.span(start, self.pos),
                            });
                        }
                    },
                    Err(_) => {
                        return Err(LexError::BadCharacter {
                            span: self.span(start, self.pos),
                        });
                    }
                }
            }
            s if s.chars().count() == 1 => s.chars().next().unwrap(),
            _ => {
                return Err(LexError::BadCharacter {
                    span: self.span(start, self.pos),
                });
            }
        };
        Ok((Token::Character(ch), self.span(start, self.pos)))
    }

    fn read_number(&mut self, start: usize) -> Result<(Token, Span), LexError> {
        // Consume sign (only if a number actually starts here — bare
        // `+`/`-` were filtered out by the caller).
        let signed = matches!(self.peek(), Some(b'+') | Some(b'-'));
        if signed {
            self.bump();
        }
        // Special-case the `inf.0` / `nan.0` IEEE-754 literals. The
        // dispatcher already verified the next 5 bytes match one of
        // these and the byte after is a delimiter, so we just consume
        // them.
        if let Some(tail) = self.bytes.get(self.pos..self.pos + 5) {
            if tail == b"inf.0" || tail == b"nan.0" {
                for _ in 0..5 {
                    self.bump();
                }
                let text = &self.src[start..self.pos];
                let span = self.span(start, self.pos);
                let num = parse_number(text).ok_or_else(|| LexError::BadNumber {
                    span,
                    src: text.into(),
                })?;
                return Ok((Token::Number(num), span));
            }
        }
        let mut saw_dot = false;
        let mut saw_slash = false;
        let mut saw_exp = false;
        while let Some(b) = self.peek() {
            match b {
                b'0'..=b'9' => {
                    self.bump();
                }
                b'.' if !saw_dot && !saw_slash => {
                    saw_dot = true;
                    self.bump();
                }
                b'/' if !saw_slash && !saw_dot => {
                    saw_slash = true;
                    self.bump();
                }
                b'e' | b'E' if !saw_exp => {
                    saw_exp = true;
                    saw_dot = true;
                    self.bump();
                    if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                        self.bump();
                    }
                }
                _ => break,
            }
        }
        let text = &self.src[start..self.pos];
        let span = self.span(start, self.pos);
        let num = parse_number(text).ok_or_else(|| LexError::BadNumber {
            span,
            src: text.into(),
        })?;
        Ok((Token::Number(num), span))
    }

    fn read_identifier(
        &mut self,
        start: usize,
        syms: &mut SymbolTable,
    ) -> Result<(Token, Span), LexError> {
        while let Some(b) = self.peek() {
            if is_delimiter(b) {
                break;
            }
            self.bump();
        }
        let text = &self.src[start..self.pos];
        let sym = syms.intern(text);
        Ok((Token::Identifier(sym), self.span(start, self.pos)))
    }
}

fn is_delimiter(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'(' | b')' | b'[' | b']' | b'"' | b'\'' | b'`' | b',' | b';'
        )
}

fn is_initial_ident(b: u8) -> bool {
    matches!(b,
        b'a'..=b'z' | b'A'..=b'Z'
            | b'!' | b'$' | b'%' | b'&' | b'*' | b'/' | b':' | b'<' | b'=' | b'>'
            | b'?' | b'^' | b'_' | b'~'
    )
}

fn parse_number(s: &str) -> Option<Number> {
    // IEEE-754 special literals first — these don't fit the generic
    // numeric grammar below. R6RS lists exactly these four.
    match s {
        "+inf.0" => return Some(Number::Flonum(f64::INFINITY)),
        "-inf.0" => return Some(Number::Flonum(f64::NEG_INFINITY)),
        "+nan.0" | "-nan.0" => return Some(Number::Flonum(f64::NAN)),
        _ => {}
    }
    if let Some(slash_idx) = s.find('/') {
        let num: i64 = s[..slash_idx].parse().ok()?;
        let den: i64 = s[slash_idx + 1..].parse().ok()?;
        if den == 0 {
            return None;
        }
        let n = Number::Fixnum(num);
        let d = Number::Fixnum(den);
        return n.div(&d).ok();
    }
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s.parse::<f64>().ok().map(Number::Flonum)
    } else {
        Number::parse_decimal_integer(s)
    }
}

pub fn lex_all(
    file: FileId,
    src: &str,
    syms: &mut SymbolTable,
) -> Result<Vec<(Token, Span)>, Vec<LexError>> {
    let mut lexer = Lexer::new(file, src);
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    loop {
        match lexer.next_token(syms) {
            Ok((Token::Eof, span)) => {
                tokens.push((Token::Eof, span));
                break;
            }
            Ok(tok) => tokens.push(tok),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok(tokens)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex(src: &str) -> Vec<Token> {
        let mut syms = SymbolTable::new();
        let toks = lex_all(FileId(0), src, &mut syms).unwrap();
        toks.into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn parens_and_eof() {
        let toks = lex("()");
        assert!(matches!(toks[0], Token::LParen));
        assert!(matches!(toks[1], Token::RParen));
        assert!(matches!(toks[2], Token::Eof));
    }

    #[test]
    fn fixnum() {
        let toks = lex("42");
        match &toks[0] {
            Token::Number(Number::Fixnum(42)) => {}
            other => panic!("expected fixnum 42, got {:?}", other),
        }
    }

    #[test]
    fn negative_fixnum() {
        let toks = lex("-7");
        match &toks[0] {
            Token::Number(Number::Fixnum(-7)) => {}
            other => panic!("expected fixnum -7, got {:?}", other),
        }
    }

    #[test]
    fn flonum() {
        let toks = lex("3.14");
        assert!(matches!(toks[0], Token::Number(Number::Flonum(_))));
    }

    #[test]
    fn boolean() {
        let toks = lex("#t #f");
        assert!(matches!(toks[0], Token::Boolean(true)));
        assert!(matches!(toks[1], Token::Boolean(false)));
    }

    #[test]
    fn string_literal() {
        let toks = lex(r#""hello, world""#);
        match &toks[0] {
            Token::String(s) => assert_eq!(s, "hello, world"),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn string_with_escapes() {
        let toks = lex(r#""a\nb\tc""#);
        match &toks[0] {
            Token::String(s) => assert_eq!(s, "a\nb\tc"),
            other => panic!("expected string, got {:?}", other),
        }
    }

    #[test]
    fn identifier() {
        let mut syms = SymbolTable::new();
        let toks = lex_all(FileId(0), "foo-bar?", &mut syms).unwrap();
        match toks[0].0 {
            Token::Identifier(s) => assert_eq!(syms.name(s), "foo-bar?"),
            _ => panic!("expected identifier"),
        }
    }

    #[test]
    fn plus_as_identifier() {
        let mut syms = SymbolTable::new();
        let toks = lex_all(FileId(0), "+", &mut syms).unwrap();
        match toks[0].0 {
            Token::Identifier(s) => assert_eq!(syms.name(s), "+"),
            _ => panic!("expected '+' identifier"),
        }
    }

    #[test]
    fn arithmetic_application() {
        let mut syms = SymbolTable::new();
        let toks = lex_all(FileId(0), "(+ 1 2)", &mut syms).unwrap();
        assert!(matches!(toks[0].0, Token::LParen));
        match &toks[1].0 {
            Token::Identifier(s) => assert_eq!(syms.name(*s), "+"),
            _ => panic!("expected '+'"),
        }
        assert!(matches!(toks[2].0, Token::Number(Number::Fixnum(1))));
        assert!(matches!(toks[3].0, Token::Number(Number::Fixnum(2))));
        assert!(matches!(toks[4].0, Token::RParen));
    }

    #[test]
    fn line_comment() {
        let toks = lex("; comment\n42");
        assert!(matches!(toks[0], Token::Number(Number::Fixnum(42))));
    }

    #[test]
    fn block_comment_nested() {
        let toks = lex("#| outer #| inner |# back |# 42");
        assert!(matches!(toks[0], Token::Number(Number::Fixnum(42))));
    }

    #[test]
    fn quote() {
        let toks = lex("'foo");
        assert!(matches!(toks[0], Token::Quote));
        assert!(matches!(toks[1], Token::Identifier(_)));
    }

    #[test]
    fn character_named() {
        let toks = lex("#\\space");
        assert!(matches!(toks[0], Token::Character(' ')));
    }

    #[test]
    fn character_literal() {
        let toks = lex("#\\a");
        assert!(matches!(toks[0], Token::Character('a')));
    }

    #[test]
    fn hex_radix() {
        let toks = lex("#xff");
        assert!(matches!(toks[0], Token::Number(Number::Fixnum(255))));
    }

    #[test]
    fn binary_radix() {
        let toks = lex("#b1010");
        assert!(matches!(toks[0], Token::Number(Number::Fixnum(10))));
    }

    #[test]
    fn octal_radix() {
        let toks = lex("#o17");
        assert!(matches!(toks[0], Token::Number(Number::Fixnum(15))));
    }

    #[test]
    fn radix_negative() {
        let toks = lex("#x-ff");
        assert!(matches!(toks[0], Token::Number(Number::Fixnum(-255))));
    }
}
