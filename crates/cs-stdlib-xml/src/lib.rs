//! CrabScheme stdlib module: `(crab xml)`.
//!
//! XML parsing and serialization — the `(crab …)` answer to Python's
//! `xml`, Go's `encoding/xml`, and Clojure's `data.xml`. Parsing uses
//! `roxmltree`; serialization is hand-written. Pure Rust → portable to
//! `wasm32`.
//!
//! ## Element representation
//!
//! An element is an opaque vector `#('__xml__ tag attrs children)`:
//!
//! - `tag` — the element name (string).
//! - `attrs` — an association list of `(name . value)` string pairs.
//! - `children` — a list whose items are either nested elements or text
//!   nodes (plain strings).
//!
//! (A string-keyed tree rather than symbol-based SXML, because the FFI
//! layer can't intern symbols; build trees with `xml-make` and read them
//! with the `xml-tag`/`xml-attrs`/`xml-children` accessors.)
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `xml-parse`     | string            | element | Parse a document; raises on malformed XML. |
//! | `xml-element?`  | value             | boolean | Element predicate. |
//! | `xml-tag`       | element           | string  | Element name. |
//! | `xml-attrs`     | element           | alist   | `(name . value)` pairs. |
//! | `xml-attr`      | element name      | string or #f | One attribute's value. |
//! | `xml-children`  | element           | list    | Child elements + text strings. |
//! | `xml-text`      | element           | string  | All descendant text, concatenated. |
//! | `xml-make`      | tag attrs children | element | Build an element. |
//! | `xml->string`   | element           | string  | Serialize to XML (escaped). |
//!
//! ```scheme
//! (import (crab xml))
//! (define doc (xml-parse "<user id=\"1\"><name>Ada</name></user>"))
//! (xml-tag doc)                       ; => "user"
//! (xml-attr doc "id")                 ; => "1"
//! (xml-text (car (xml-children doc))) ; => "Ada"
//! (xml->string (xml-make "p" '(("class" . "x")) (list "hi")))
//! ;; => "<p class=\"x\">hi</p>"
//! ```

use std::cell::RefCell;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

const XML_TAG: &str = "__xml__";

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("xml-parse", xml_parse),
        UntypedProc::new("xml-element?", xml_element_p),
        UntypedProc::new("xml-tag", xml_tag),
        UntypedProc::new("xml-attrs", xml_attrs),
        UntypedProc::new("xml-attr", xml_attr),
        UntypedProc::new("xml-children", xml_children),
        UntypedProc::new("xml-text", xml_text),
        UntypedProc::new("xml-make", xml_make),
        UntypedProc::new("xml->string", xml_to_string),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn fail(msg: String) -> FfiError {
    FfiError::HostFailure(msg)
}

fn type_err(name: &str, expected: &'static str, got: &Value) -> FfiError {
    FfiError::TypeMismatch {
        expected,
        got: format!("{} (in {})", got.type_name(), name),
    }
}

fn as_string(name: &str, v: &Value) -> Result<String, FfiError> {
    match v {
        Value::String(s) => Ok(s.borrow().clone()),
        other => Err(type_err(name, "string", other)),
    }
}

/// Collect a proper list's elements into a Vec.
fn read_list(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    while let Value::Pair(p) = cur {
        out.push(p.car());
        cur = p.cdr();
    }
    out
}

/// Read an attribute alist of `(name . value)` string pairs.
fn read_attrs(name: &str, v: &Value) -> Result<Vec<(String, String)>, FfiError> {
    let mut out = Vec::new();
    for entry in read_list(v) {
        match entry {
            Value::Pair(p) => {
                out.push((as_string(name, &p.car())?, as_string(name, &p.cdr())?));
            }
            other => {
                return Err(fail(format!(
                    "{}: each attribute must be a (name . value) pair, got {}",
                    name,
                    other.type_name()
                )))
            }
        }
    }
    Ok(out)
}

/// Build an element vector `#('__xml__ tag attrs children)`.
fn make_element(tag: String, attrs: Vec<(String, String)>, children: Vec<Value>) -> Value {
    let attr_pairs: Vec<Value> = attrs
        .into_iter()
        .map(|(k, v)| Value::Pair(cs_core::Pair::new(Value::string(k), Value::string(v))))
        .collect();
    Value::Vector(cs_core::Gc::new(RefCell::new(vec![
        Value::string(XML_TAG),
        Value::string(tag),
        Value::list(attr_pairs),
        Value::list(children),
    ])))
}

fn is_element(v: &Value) -> bool {
    if let Value::Vector(items) = v {
        let items = items.borrow();
        items.len() == 4 && matches!(&items[0], Value::String(s) if s.borrow().as_str() == XML_TAG)
    } else {
        false
    }
}

/// Clone field `idx` of an element vector, validating the shape.
fn element_field(name: &str, v: &Value, idx: usize) -> Result<Value, FfiError> {
    if !is_element(v) {
        return Err(type_err(name, "xml element", v));
    }
    let Value::Vector(items) = v else {
        unreachable!("is_element checked vector");
    };
    Ok(items.borrow()[idx].clone())
}

// ----- parsing -----

fn convert_node(node: roxmltree::Node) -> Value {
    let tag = node.tag_name().name().to_string();
    let attrs: Vec<(String, String)> = node
        .attributes()
        .map(|a| (a.name().to_string(), a.value().to_string()))
        .collect();
    let children: Vec<Value> = node
        .children()
        .filter_map(|c| {
            if c.is_element() {
                Some(convert_node(c))
            } else if c.is_text() {
                // Drop whitespace-only text (inter-element formatting);
                // keep text with content as a plain string child.
                c.text()
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| Value::string(t.to_string()))
            } else {
                None // comments / processing instructions are skipped
            }
        })
        .collect();
    make_element(tag, attrs, children)
}

fn xml_parse(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml-parse", "1", args.len()));
    }
    let src = as_string("xml-parse", &args[0])?;
    let doc = roxmltree::Document::parse(&src).map_err(|e| fail(format!("xml-parse: {}", e)))?;
    Ok(convert_node(doc.root_element()))
}

// ----- accessors -----

fn xml_element_p(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml-element?", "1", args.len()));
    }
    Ok(Value::Boolean(is_element(&args[0])))
}

fn xml_tag(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml-tag", "1", args.len()));
    }
    element_field("xml-tag", &args[0], 1)
}

fn xml_attrs(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml-attrs", "1", args.len()));
    }
    element_field("xml-attrs", &args[0], 2)
}

fn xml_children(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml-children", "1", args.len()));
    }
    element_field("xml-children", &args[0], 3)
}

fn xml_attr(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("xml-attr", "2", args.len()));
    }
    let attrs = element_field("xml-attr", &args[0], 2)?;
    let name = as_string("xml-attr", &args[1])?;
    for entry in read_list(&attrs) {
        if let Value::Pair(p) = entry {
            if matches!(&p.car(), Value::String(s) if *s.borrow() == name) {
                return Ok(p.cdr());
            }
        }
    }
    Ok(Value::Boolean(false))
}

fn collect_text(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => out.push_str(&s.borrow()),
        _ if is_element(v) => {
            if let Ok(children) = element_field("xml-text", v, 3) {
                for c in read_list(&children) {
                    collect_text(&c, out);
                }
            }
        }
        _ => {}
    }
}

fn xml_text(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml-text", "1", args.len()));
    }
    if !is_element(&args[0]) {
        return Err(type_err("xml-text", "xml element", &args[0]));
    }
    let mut out = String::new();
    collect_text(&args[0], &mut out);
    Ok(Value::string(out))
}

// ----- construction -----

fn xml_make(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 3 {
        return Err(arity("xml-make", "3", args.len()));
    }
    let tag = as_string("xml-make", &args[0])?;
    let attrs = read_attrs("xml-make", &args[1])?;
    let children = read_list(&args[2]);
    // Validate children are elements or text strings up front.
    for c in &children {
        if !matches!(c, Value::String(_)) && !is_element(c) {
            return Err(fail(format!(
                "xml-make: each child must be an xml element or a string, got {}",
                c.type_name()
            )));
        }
    }
    Ok(make_element(tag, attrs, children))
}

// ----- serialization -----

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

fn serialize(v: &Value, out: &mut String) -> Result<(), FfiError> {
    if let Value::String(s) = v {
        escape_text(&s.borrow(), out);
        return Ok(());
    }
    if !is_element(v) {
        return Err(fail(format!(
            "xml->string: expected an xml element or text string, got {}",
            v.type_name()
        )));
    }
    let tag = as_string("xml->string", &element_field("xml->string", v, 1)?)?;
    let attrs = read_attrs("xml->string", &element_field("xml->string", v, 2)?)?;
    let children = read_list(&element_field("xml->string", v, 3)?);

    out.push('<');
    out.push_str(&tag);
    for (k, val) in attrs {
        out.push(' ');
        out.push_str(&k);
        out.push_str("=\"");
        escape_attr(&val, out);
        out.push('"');
    }
    if children.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        for c in &children {
            serialize(c, out)?;
        }
        out.push_str("</");
        out.push_str(&tag);
        out.push('>');
    }
    Ok(())
}

fn xml_to_string(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("xml->string", "1", args.len()));
    }
    let mut out = String::new();
    serialize(&args[0], &mut out)?;
    Ok(Value::string(out))
}
