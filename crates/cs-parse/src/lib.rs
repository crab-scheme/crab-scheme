//! Reader: token stream -> Datum tree.
//!
//! Foundation milestone: covers atoms, lists, dotted pairs, vectors, the four
//! quote-family abbreviations, and `#;` datum comments.

use std::rc::Rc;

use cs_core::{Number, Symbol, SymbolTable, Value};
use cs_diag::{FileId, Span};
use cs_lex::{LexError, Lexer, Token};

/// A datum tree. Spans are preserved on every node.
#[derive(Clone, Debug)]
pub enum Datum {
    Boolean(bool, Span),
    Number(Number, Span),
    Character(char, Span),
    String(Rc<String>, Span),
    Symbol(Symbol, Span),
    Null(Span),
    Pair(Rc<Datum>, Rc<Datum>, Span),
    Vector(Vec<Datum>, Span),
    /// R7RS bytevector literal `#u8(b0 b1 ...)`. Each element is an
    /// integer in 0..=255 at parse time; the reader validates the
    /// range and emits the byte sequence directly.
    ByteVector(Vec<u8>, Span),
}

impl Datum {
    pub fn span(&self) -> Span {
        match self {
            Datum::Boolean(_, s)
            | Datum::Number(_, s)
            | Datum::Character(_, s)
            | Datum::String(_, s)
            | Datum::Symbol(_, s)
            | Datum::Null(s)
            | Datum::Pair(_, _, s)
            | Datum::Vector(_, s)
            | Datum::ByteVector(_, s) => *s,
        }
    }

    /// Render this datum as Scheme-source-like text with symbol names
    /// resolved through `syms`. Used by the expander to include the
    /// offending form in error messages (e.g. `assert`'s failure).
    pub fn format_with(&self, syms: &SymbolTable) -> String {
        let mut out = String::new();
        format_datum(self, syms, &mut out);
        out
    }

    /// Convert this datum into a runtime [`Value`]. Used by `quote` lowering.
    pub fn to_value(&self) -> Value {
        match self {
            Datum::Boolean(b, _) => Value::Boolean(*b),
            Datum::Number(n, _) => Value::Number(n.clone()),
            Datum::Character(c, _) => Value::Character(*c),
            Datum::String(s, _) => Value::string((**s).clone()),
            Datum::Symbol(s, _) => Value::Symbol(*s),
            Datum::Null(_) => Value::Null,
            Datum::Pair(car, cdr, _) => {
                let car_v = car.to_value();
                let cdr_v = cdr.to_value();
                Value::Pair(cs_core::Pair::new(car_v, cdr_v))
            }
            Datum::Vector(items, _) => {
                let v: Vec<Value> = items.iter().map(|d| d.to_value()).collect();
                Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(v)))
            }
            Datum::ByteVector(bytes, _) => {
                Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(bytes.clone())))
            }
        }
    }
}

fn format_datum(d: &Datum, syms: &SymbolTable, out: &mut String) {
    use std::fmt::Write;
    match d {
        Datum::Boolean(b, _) => out.push_str(if *b { "#t" } else { "#f" }),
        Datum::Number(n, _) => {
            let _ = write!(out, "{}", n);
        }
        Datum::Character(c, _) => {
            let _ = write!(out, "#\\{}", c);
        }
        Datum::String(s, _) => {
            let _ = write!(out, "\"{}\"", s);
        }
        Datum::Symbol(s, _) => out.push_str(syms.name(*s)),
        Datum::Null(_) => out.push_str("()"),
        Datum::Pair(_, _, _) => {
            out.push('(');
            let mut cur = d.clone();
            let mut first = true;
            loop {
                match &cur {
                    Datum::Pair(car, cdr, _) => {
                        if !first {
                            out.push(' ');
                        }
                        format_datum(car, syms, out);
                        first = false;
                        cur = (**cdr).clone();
                    }
                    Datum::Null(_) => break,
                    other => {
                        out.push_str(" . ");
                        format_datum(other, syms, out);
                        break;
                    }
                }
            }
            out.push(')');
        }
        Datum::Vector(items, _) => {
            out.push_str("#(");
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                format_datum(it, syms, out);
            }
            out.push(')');
        }
        Datum::ByteVector(bytes, _) => {
            out.push_str("#u8(");
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let _ = write!(out, "{}", b);
            }
            out.push(')');
        }
    }
}

#[derive(Clone, Debug)]
pub enum ReaderError {
    Lex(LexError),
    UnexpectedToken { span: Span, what: String },
    UnclosedList { span: Span },
    Incomplete,
}

impl ReaderError {
    pub fn span(&self) -> Span {
        match self {
            ReaderError::Lex(e) => e.span(),
            ReaderError::UnexpectedToken { span, .. } | ReaderError::UnclosedList { span } => *span,
            ReaderError::Incomplete => Span::DUMMY,
        }
    }

    pub fn message(&self) -> String {
        match self {
            ReaderError::Lex(e) => e.message(),
            ReaderError::UnexpectedToken { what, .. } => format!("unexpected {what}"),
            ReaderError::UnclosedList { .. } => "unclosed list".into(),
            ReaderError::Incomplete => "incomplete input".into(),
        }
    }
}

pub struct Reader<'src> {
    lexer: Lexer<'src>,
    /// One-token lookahead.
    peeked: Option<(Token, Span)>,
}

impl<'src> Reader<'src> {
    pub fn new(file: FileId, src: &'src str) -> Self {
        Self {
            lexer: Lexer::new(file, src),
            peeked: None,
        }
    }

    fn next_tok(&mut self, syms: &mut SymbolTable) -> Result<(Token, Span), ReaderError> {
        if let Some(t) = self.peeked.take() {
            return Ok(t);
        }
        self.lexer.next_token(syms).map_err(ReaderError::Lex)
    }

    fn peek_tok<'a>(
        &'a mut self,
        syms: &mut SymbolTable,
    ) -> Result<&'a (Token, Span), ReaderError> {
        if self.peeked.is_none() {
            let t = self.lexer.next_token(syms).map_err(ReaderError::Lex)?;
            self.peeked = Some(t);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    /// Read the next datum (skipping `#;`-commented datums).
    /// Returns `Ok(None)` on EOF.
    pub fn read(&mut self, syms: &mut SymbolTable) -> Result<Option<Datum>, ReaderError> {
        loop {
            let (tok, span) = self.next_tok(syms)?;
            return match tok {
                Token::Eof => Ok(None),
                Token::HashSemi => {
                    // Skip a datum.
                    let _ = self.read_required(syms, span)?;
                    continue;
                }
                other => self.read_token(other, span, syms).map(Some),
            };
        }
    }

    fn read_required(&mut self, syms: &mut SymbolTable, prev: Span) -> Result<Datum, ReaderError> {
        match self.read(syms)? {
            Some(d) => Ok(d),
            None => Err(ReaderError::UnexpectedToken {
                span: prev,
                what: "EOF".into(),
            }),
        }
    }

    fn read_token(
        &mut self,
        tok: Token,
        span: Span,
        syms: &mut SymbolTable,
    ) -> Result<Datum, ReaderError> {
        match tok {
            Token::Boolean(b) => Ok(Datum::Boolean(b, span)),
            Token::Number(n) => Ok(Datum::Number(n, span)),
            Token::Character(c) => Ok(Datum::Character(c, span)),
            Token::String(s) => Ok(Datum::String(Rc::new(s), span)),
            Token::Identifier(s) => Ok(Datum::Symbol(s, span)),
            Token::LParen | Token::LBracket => self.read_list(span, syms),
            Token::HashLParen => self.read_vector(span, syms),
            Token::HashU8LParen => self.read_bytevector(span, syms),
            Token::Quote => self.read_quote_form("quote", span, syms),
            Token::QuasiQuote => self.read_quote_form("quasiquote", span, syms),
            Token::Unquote => self.read_quote_form("unquote", span, syms),
            Token::UnquoteSplicing => self.read_quote_form("unquote-splicing", span, syms),
            Token::RParen | Token::RBracket => Err(ReaderError::UnexpectedToken {
                span,
                what: "closing paren without opener".into(),
            }),
            Token::Dot => Err(ReaderError::UnexpectedToken {
                span,
                what: "'.' outside of list".into(),
            }),
            Token::HashSemi => unreachable!("handled in read"),
            Token::Eof => unreachable!("handled in read"),
        }
    }

    fn read_list(&mut self, open_span: Span, syms: &mut SymbolTable) -> Result<Datum, ReaderError> {
        let mut items: Vec<Datum> = Vec::new();
        let mut tail: Option<Datum> = None;
        loop {
            let (tok, span) = match self.peek_tok(syms) {
                Ok(t) => (t.0.clone(), t.1),
                Err(e) => return Err(e),
            };
            match tok {
                Token::RParen | Token::RBracket => {
                    self.next_tok(syms)?;
                    let close_span = span;
                    let full_span = open_span.merge(close_span);
                    let mut acc = tail.unwrap_or(Datum::Null(close_span));
                    for item in items.into_iter().rev() {
                        let s = item.span().merge(acc.span());
                        acc = Datum::Pair(Rc::new(item), Rc::new(acc), s);
                    }
                    return Ok(match acc {
                        Datum::Null(_) => Datum::Null(full_span),
                        Datum::Pair(a, b, _) => Datum::Pair(a, b, full_span),
                        other => other,
                    });
                }
                Token::Dot => {
                    self.next_tok(syms)?;
                    let cdr = self.read_required(syms, span)?;
                    tail = Some(cdr);
                    let (close_tok, close_span) = self.next_tok(syms)?;
                    if !matches!(close_tok, Token::RParen | Token::RBracket) {
                        return Err(ReaderError::UnexpectedToken {
                            span: close_span,
                            what: "expected ')' after dotted tail".into(),
                        });
                    }
                    let full_span = open_span.merge(close_span);
                    let mut acc = tail.unwrap();
                    for item in items.into_iter().rev() {
                        let s = item.span().merge(acc.span());
                        acc = Datum::Pair(Rc::new(item), Rc::new(acc), s);
                    }
                    return Ok(match acc {
                        Datum::Pair(a, b, _) => Datum::Pair(a, b, full_span),
                        other => other,
                    });
                }
                Token::Eof => return Err(ReaderError::UnclosedList { span: open_span }),
                _ => {
                    let item = match self.read(syms)? {
                        Some(d) => d,
                        None => return Err(ReaderError::UnclosedList { span: open_span }),
                    };
                    items.push(item);
                }
            }
        }
    }

    /// R7RS `#u8(b0 b1 ... bN)` — each element must be an exact integer
    /// in 0..=255 at parse time.
    fn read_bytevector(
        &mut self,
        open_span: Span,
        syms: &mut SymbolTable,
    ) -> Result<Datum, ReaderError> {
        let mut bytes: Vec<u8> = Vec::new();
        loop {
            let (tok, span) = match self.peek_tok(syms) {
                Ok(t) => (t.0.clone(), t.1),
                Err(e) => return Err(e),
            };
            match tok {
                Token::RParen | Token::RBracket => {
                    self.next_tok(syms)?;
                    let full_span = open_span.merge(span);
                    return Ok(Datum::ByteVector(bytes, full_span));
                }
                Token::Eof => return Err(ReaderError::UnclosedList { span: open_span }),
                Token::Number(n) => {
                    self.next_tok(syms)?;
                    let v = match &n {
                        Number::Fixnum(v) => *v,
                        _ => {
                            return Err(ReaderError::UnexpectedToken {
                                span,
                                what: "bytevector element must be an exact byte (0..=255)".into(),
                            })
                        }
                    };
                    if !(0..=255).contains(&v) {
                        return Err(ReaderError::UnexpectedToken {
                            span,
                            what: format!("byte literal {} out of 0..=255", v),
                        });
                    }
                    bytes.push(v as u8);
                }
                _ => {
                    return Err(ReaderError::UnexpectedToken {
                        span,
                        what: "bytevector element must be a byte literal".into(),
                    })
                }
            }
        }
    }

    fn read_vector(
        &mut self,
        open_span: Span,
        syms: &mut SymbolTable,
    ) -> Result<Datum, ReaderError> {
        let mut items: Vec<Datum> = Vec::new();
        loop {
            let (tok, span) = match self.peek_tok(syms) {
                Ok(t) => (t.0.clone(), t.1),
                Err(e) => return Err(e),
            };
            match tok {
                Token::RParen | Token::RBracket => {
                    self.next_tok(syms)?;
                    let full_span = open_span.merge(span);
                    return Ok(Datum::Vector(items, full_span));
                }
                Token::Eof => return Err(ReaderError::UnclosedList { span: open_span }),
                _ => {
                    let item = self.read_required(syms, open_span)?;
                    items.push(item);
                }
            }
        }
    }

    fn read_quote_form(
        &mut self,
        name: &str,
        span: Span,
        syms: &mut SymbolTable,
    ) -> Result<Datum, ReaderError> {
        let inner = self.read_required(syms, span)?;
        let inner_span = inner.span();
        let full_span = span.merge(inner_span);
        let sym = syms.intern(name);
        let head = Datum::Symbol(sym, span);
        let null = Datum::Null(inner_span);
        let pair_inner = Datum::Pair(Rc::new(inner), Rc::new(null), inner_span);
        Ok(Datum::Pair(Rc::new(head), Rc::new(pair_inner), full_span))
    }
}

pub fn read_all(
    file: FileId,
    src: &str,
    syms: &mut SymbolTable,
) -> Result<Vec<Datum>, Vec<ReaderError>> {
    let mut reader = Reader::new(file, src);
    let mut data = Vec::new();
    let mut errors = Vec::new();
    loop {
        match reader.read(syms) {
            Ok(Some(d)) => data.push(d),
            Ok(None) => break,
            Err(e) => {
                errors.push(e);
                break;
            }
        }
    }
    if errors.is_empty() {
        Ok(data)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_one(src: &str) -> (Datum, SymbolTable) {
        let mut syms = SymbolTable::new();
        let data = read_all(FileId(0), src, &mut syms).unwrap();
        assert_eq!(data.len(), 1);
        (data.into_iter().next().unwrap(), syms)
    }

    #[test]
    fn fixnum_atom() {
        let (d, _) = read_one("42");
        match d {
            Datum::Number(Number::Fixnum(42), _) => {}
            _ => panic!("expected 42"),
        }
    }

    #[test]
    fn empty_list() {
        let (d, _) = read_one("()");
        assert!(matches!(d, Datum::Null(_)));
    }

    #[test]
    fn proper_list() {
        let (d, syms) = read_one("(+ 1 2)");
        match &d {
            Datum::Pair(car, cdr, _) => {
                match car.as_ref() {
                    Datum::Symbol(s, _) => assert_eq!(syms.name(*s), "+"),
                    _ => panic!("expected symbol"),
                }
                // cdr should be a pair containing 1 and (2)
                match cdr.as_ref() {
                    Datum::Pair(_, _, _) => {}
                    _ => panic!("expected pair"),
                }
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn dotted_pair() {
        let (d, _) = read_one("(1 . 2)");
        match d {
            Datum::Pair(car, cdr, _) => {
                assert!(matches!(*car, Datum::Number(Number::Fixnum(1), _)));
                assert!(matches!(*cdr, Datum::Number(Number::Fixnum(2), _)));
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn quote_abbreviation() {
        let (d, syms) = read_one("'foo");
        match &d {
            Datum::Pair(car, cdr, _) => {
                match car.as_ref() {
                    Datum::Symbol(s, _) => assert_eq!(syms.name(*s), "quote"),
                    _ => panic!("expected 'quote'"),
                }
                match cdr.as_ref() {
                    Datum::Pair(inner_car, inner_cdr, _) => {
                        match inner_car.as_ref() {
                            Datum::Symbol(s, _) => assert_eq!(syms.name(*s), "foo"),
                            _ => panic!("expected 'foo'"),
                        }
                        assert!(matches!(**inner_cdr, Datum::Null(_)));
                    }
                    _ => panic!("expected pair"),
                }
            }
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn datum_comment() {
        let mut syms = SymbolTable::new();
        let data = read_all(FileId(0), "(1 #;2 3)", &mut syms).unwrap();
        // Should parse as (1 3)
        assert_eq!(data.len(), 1);
    }

    #[test]
    fn vector() {
        let (d, _) = read_one("#(1 2 3)");
        match d {
            Datum::Vector(items, _) => assert_eq!(items.len(), 3),
            _ => panic!("expected vector"),
        }
    }

    #[test]
    fn nested_lists() {
        let (d, _) = read_one("((1 2) (3 4))");
        match d {
            Datum::Pair(_, _, _) => {}
            _ => panic!("expected pair"),
        }
    }

    #[test]
    fn unclosed_list_errors() {
        let mut syms = SymbolTable::new();
        let r = read_all(FileId(0), "(1 2", &mut syms);
        assert!(r.is_err());
    }
}

#[cfg(test)]
mod proptests {
    //! Hand-rolled property-style tests for the reader. The key invariant is
    //! `read(write(d)) == d`. We sweep through ~thousands of generated cases
    //! per property using a small deterministic generator (no proptest dep
    //! to keep the link path clean on macOS).

    use super::*;
    use cs_core::{Number, Value, WriteMode};

    // Tiny LCG RNG for reproducibility without external deps.
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(
                seed.wrapping_mul(2862933555777941757)
                    .wrapping_add(3037000493),
            )
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }
        fn next_in(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
        fn next_i64(&mut self) -> i64 {
            let v = self.next_u64() as i64;
            v / 2 // keep within i64::MIN/2..i64::MAX/2
        }
    }

    fn gen_atom(rng: &mut Lcg) -> Value {
        match rng.next_in(4) {
            0 => Value::Null,
            1 => Value::Boolean(rng.next_in(2) == 0),
            _ => Value::fixnum(rng.next_i64()),
        }
    }

    fn gen_value(rng: &mut Lcg, depth: u32) -> Value {
        if depth == 0 || rng.next_in(3) == 0 {
            return gen_atom(rng);
        }
        let len = rng.next_in(6);
        let items: Vec<Value> = (0..len).map(|_| gen_value(rng, depth - 1)).collect();
        Value::list(items)
    }

    #[test]
    fn roundtrip_simple_values_1000() {
        let mut rng = Lcg::new(0xC0FFEE);
        for _ in 0..1000 {
            let v = gen_value(&mut rng, 4);
            let syms = SymbolTable::new();
            let written = v.format_with(&syms, WriteMode::Write);
            let mut syms2 = SymbolTable::new();
            let parsed = read_all(FileId(0), &written, &mut syms2)
                .unwrap_or_else(|_| panic!("failed to parse: {}", written));
            assert_eq!(parsed.len(), 1, "expected one datum from {}", written);
            let parsed_val = parsed[0].to_value();
            assert!(
                cs_core::eq::equal(&v, &parsed_val),
                "round-trip mismatch:\n  input:  {}\n  output: {}",
                v.format_with(&syms, WriteMode::Write),
                parsed_val.format_with(&syms2, WriteMode::Write),
            );
        }
    }

    #[test]
    fn roundtrip_fixnum_sweep() {
        // Sweep representative fixnums including edges.
        let cases: Vec<i64> = vec![
            0,
            1,
            -1,
            42,
            -42,
            i32::MAX as i64,
            i32::MIN as i64,
            i64::MAX / 2,
            i64::MIN / 2,
        ];
        for n in cases {
            let written = format!("{}", Number::Fixnum(n));
            let mut syms = SymbolTable::new();
            let parsed = read_all(FileId(0), &written, &mut syms).unwrap();
            match &parsed[0] {
                Datum::Number(Number::Fixnum(m), _) => assert_eq!(*m, n),
                other => panic!("expected fixnum, got {:?}", other),
            }
        }
    }

    #[test]
    fn roundtrip_lists_of_fixnums_1000() {
        let mut rng = Lcg::new(0xBADF00D);
        for _ in 0..1000 {
            let len = rng.next_in(20);
            let items: Vec<i64> = (0..len).map(|_| rng.next_i64()).collect();
            let v = Value::list(items.iter().map(|n| Value::fixnum(*n)));
            let syms = SymbolTable::new();
            let written = v.format_with(&syms, WriteMode::Write);
            let mut syms2 = SymbolTable::new();
            let parsed = read_all(FileId(0), &written, &mut syms2).unwrap();
            let parsed_val = parsed[0].to_value();
            assert!(
                cs_core::eq::equal(&v, &parsed_val),
                "round-trip mismatch on list of {} fixnums",
                items.len(),
            );
        }
    }
}
