//! Custom `#!lang` reader bridge — issue #10
//! (R6RS++ Phase 4: full `#!lang` reader protocol).
//!
//! When a source file starts with `#!lang NAME` and `(lang NAME)`
//! exports a `reader` procedure, the runtime calls it with the
//! file body (as a Scheme string) and feeds the returned datum
//! list to the expander *instead of* the host reader. This module
//! converts the [`Value`] tree the reader hands back into the
//! [`Datum`] tree the expander consumes.
//!
//! ## Spans
//!
//! Scheme values carry no source-text origin, so every datum
//! synthesized here gets the caller-supplied [`Span`] (the body
//! file's leading byte). Diagnostics about reader-produced forms
//! all point at the same anchor — coarse but unambiguous, and
//! good enough for an MVP. A future iter could thread richer
//! span info if the reader chooses to emit `(datum . span)`
//! pairs (out of scope here).
//!
//! ## Cycles
//!
//! `value_to_datum` does not detect cyclic value graphs. A
//! reader that returns a cyclic structure will recurse until the
//! stack overflows. Document-don't-enforce for the MVP — readers
//! are user code authored alongside the host runtime.

use std::rc::Rc;

use cs_core::Value;
use cs_diag::Span;
use cs_parse::Datum;

/// Convert the result of `(reader body)` into a list of datums.
///
/// `v` must be a proper Scheme list. Each element is converted via
/// [`value_to_datum`]; improper-list cdrs or non-datum cars raise
/// an `Err` whose message names the offending value's type.
pub(crate) fn value_to_datum_list(v: &Value, span: Span) -> Result<Vec<Datum>, String> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match &cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(value_to_datum(&p.car(), span)?);
                cur = p.cdr();
            }
            other => {
                return Err(format!(
                    "expected a proper list of datums; encountered {} in cdr position",
                    other.type_name()
                ));
            }
        }
    }
}

/// Convert one Scheme value into a datum. Recurses through pairs
/// (handling improper / dotted pairs naturally) and vectors.
/// Non-datum values (procedures, ports, records, continuations,
/// hash tables, etc.) are rejected with a typed error message.
pub(crate) fn value_to_datum(v: &Value, span: Span) -> Result<Datum, String> {
    match v {
        Value::Boolean(b) => Ok(Datum::Boolean(*b, span)),
        Value::Number(n) => Ok(Datum::Number(n.clone(), span)),
        Value::Character(c) => Ok(Datum::Character(*c, span)),
        Value::String(s) => Ok(Datum::String(Rc::new(s.borrow().clone()), span)),
        Value::Symbol(s) => Ok(Datum::Symbol(*s, span)),
        Value::Null => Ok(Datum::Null(span)),
        Value::Pair(p) => {
            let car = value_to_datum(&p.car(), span)?;
            let cdr = value_to_datum(&p.cdr(), span)?;
            Ok(Datum::Pair(Rc::new(car), Rc::new(cdr), span))
        }
        Value::Vector(v) => {
            let items = v.borrow();
            let mut conv = Vec::with_capacity(items.len());
            for it in items.iter() {
                conv.push(value_to_datum(it, span)?);
            }
            Ok(Datum::Vector(conv, span))
        }
        Value::ByteVector(bv) => Ok(Datum::ByteVector(bv.borrow().clone(), span)),
        other => Err(format!(
            "value of type {} is not a datum",
            other.type_name()
        )),
    }
}

/// Parse a leading `#!lang NAME` (or `#lang NAME`) header. Returns
/// `Some((name, rest_of_source))` if line 1 is a header, `None`
/// otherwise. The returned `rest` includes the trailing newline
/// after the header line, so prepending whitespace of the header
/// line's byte length to it preserves both line count and byte
/// offsets for diagnostic spans.
pub(crate) fn parse_lang_header(src: &str) -> Option<(&str, &str)> {
    let no_bom = src.strip_prefix('\u{FEFF}').unwrap_or(src);
    let (first_line, rest) = match no_bom.find('\n') {
        Some(idx) => (&no_bom[..idx], &no_bom[idx..]),
        None => (no_bom, ""),
    };
    let trimmed = first_line.trim_start();
    let name = trimmed
        .strip_prefix("#!lang ")
        .or_else(|| trimmed.strip_prefix("#lang "))
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().all(|c| !c.is_whitespace()))?;
    Some((name, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_basic() {
        let (name, rest) = parse_lang_header("#!lang demo\n(foo 1)").unwrap();
        assert_eq!(name, "demo");
        assert_eq!(rest, "\n(foo 1)");
    }

    #[test]
    fn header_short_form() {
        let (name, _) = parse_lang_header("#lang demo\n").unwrap();
        assert_eq!(name, "demo");
    }

    #[test]
    fn header_with_bom() {
        let (name, _) = parse_lang_header("\u{FEFF}#!lang demo\nx").unwrap();
        assert_eq!(name, "demo");
    }

    #[test]
    fn no_header_no_match() {
        assert!(parse_lang_header("(+ 1 2)").is_none());
        assert!(parse_lang_header("; comment\n#!lang demo\nx").is_none());
        assert!(parse_lang_header("#!lang\n").is_none()); // no name
    }

    #[test]
    fn header_only_no_body() {
        let (name, rest) = parse_lang_header("#!lang demo").unwrap();
        assert_eq!(name, "demo");
        assert_eq!(rest, "");
    }
}
