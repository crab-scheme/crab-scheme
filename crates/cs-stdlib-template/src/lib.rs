//! CrabScheme stdlib module: `(crab template)`.
//!
//! A small mustache-style template engine — the `(crab …)` answer to
//! Go's `text/template` + `html/template`, Python's jinja, and Clojure's
//! selmer. Pure Rust, no dependencies; pairs with `(crab http)`.
//!
//! ## Syntax
//!
//! The data context is an association list of `(name . value)` pairs;
//! values may be strings, numbers, booleans, nested alists, or lists.
//!
//! - `{{ key }}` — interpolate, HTML-escaped. `key` may be a dotted path
//!   (`{{ user.name }}`) into nested alists, or `.` for the current item.
//! - `{{{ key }}}` — interpolate **raw** (no escaping).
//! - `{{#each items}} … {{/each}}` — repeat the body once per element of
//!   the list at `items`, with the element as the context.
//! - `{{#if key}} … {{/if}}` — render the body when `key` is truthy
//!   (present, not `#f`, not an empty string or list).
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `template-render` | template data | string | Render `template` against the `data` alist. |
//! | `html-escape`     | string        | string | Escape `& < > " '`. |
//!
//! ```scheme
//! (import (crab template))
//! (template-render
//!   "<h1>{{title}}</h1><ul>{{#each items}}<li>{{.}}</li>{{/each}}</ul>"
//!   '(("title" . "Crabs") ("items" . ("Ada" "Alan"))))
//! ;; => "<h1>Crabs</h1><ul><li>Ada</li><li>Alan</li></ul>"
//! ```

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("template-render", template_render),
        UntypedProc::new("html-escape", html_escape_proc),
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

fn fail(msg: impl Into<String>) -> FfiError {
    FfiError::HostFailure(msg.into())
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

// ----- tokenizer -----

enum Token {
    Text(String),
    Var { path: String, raw: bool },
    EachOpen(String),
    IfOpen(String),
    Close,
}

fn tokenize(t: &str) -> Result<Vec<Token>, FfiError> {
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    while i < t.len() {
        if t[i..].starts_with("{{{") {
            if i > text_start {
                tokens.push(Token::Text(t[text_start..i].to_string()));
            }
            let end = t[i + 3..]
                .find("}}}")
                .ok_or_else(|| fail("template: unclosed {{{"))?;
            let inner = t[i + 3..i + 3 + end].trim();
            tokens.push(Token::Var {
                path: inner.to_string(),
                raw: true,
            });
            i = i + 3 + end + 3;
            text_start = i;
        } else if t[i..].starts_with("{{") {
            if i > text_start {
                tokens.push(Token::Text(t[text_start..i].to_string()));
            }
            let end = t[i + 2..]
                .find("}}")
                .ok_or_else(|| fail("template: unclosed {{"))?;
            let inner = t[i + 2..i + 2 + end].trim();
            let tok = if let Some(rest) = inner.strip_prefix("#each ") {
                Token::EachOpen(rest.trim().to_string())
            } else if let Some(rest) = inner.strip_prefix("#if ") {
                Token::IfOpen(rest.trim().to_string())
            } else if inner == "/each" || inner == "/if" {
                Token::Close
            } else {
                Token::Var {
                    path: inner.to_string(),
                    raw: false,
                }
            };
            tokens.push(tok);
            i = i + 2 + end + 2;
            text_start = i;
        } else {
            // Advance one UTF-8 character so `t[i..]` stays on a boundary.
            i += 1;
            while i < t.len() && !t.is_char_boundary(i) {
                i += 1;
            }
        }
    }
    if text_start < t.len() {
        tokens.push(Token::Text(t[text_start..].to_string()));
    }
    Ok(tokens)
}

/// Index of the `Close` matching the block opened just before `start`.
fn find_close(tokens: &[Token], start: usize) -> Result<usize, FfiError> {
    let mut depth = 1;
    let mut i = start;
    while i < tokens.len() {
        match &tokens[i] {
            Token::EachOpen(_) | Token::IfOpen(_) => depth += 1,
            Token::Close => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Err(fail(
        "template: unclosed block (missing {{/each}} or {{/if}})",
    ))
}

// ----- context lookup -----

/// Look up `key` (one segment, no dots) in an alist context.
fn assoc_get(ctx: &Value, key: &str) -> Value {
    let mut cur = ctx.clone();
    while let Value::Pair(p) = cur {
        if let Value::Pair(entry) = p.car() {
            if matches!(&entry.car(), Value::String(s) if s.borrow().as_str() == key) {
                return entry.cdr();
            }
        }
        cur = p.cdr();
    }
    Value::Null
}

/// Resolve a dotted path (or `.`) against the context.
fn lookup(ctx: &Value, path: &str) -> Value {
    if path == "." {
        return ctx.clone();
    }
    let mut cur = ctx.clone();
    for seg in path.split('.') {
        cur = assoc_get(&cur, seg);
        if matches!(cur, Value::Null) {
            return Value::Null;
        }
    }
    cur
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Boolean(false) => false,
        Value::String(s) => !s.borrow().is_empty(),
        _ => true,
    }
}

fn list_items(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    while let Value::Pair(p) = cur {
        out.push(p.car());
        cur = p.cdr();
    }
    out
}

fn render_value(v: &Value, raw: bool, out: &mut String) {
    let s = match v {
        Value::String(s) => s.borrow().clone(),
        Value::Number(cs_core::Number::Fixnum(i)) => i.to_string(),
        Value::Number(n) => format!("{}", n.to_f64()),
        Value::Boolean(true) => "true".to_string(),
        _ => String::new(), // #f, null, lists, alists render empty
    };
    if raw {
        out.push_str(&s);
    } else {
        escape_html(&s, out);
    }
}

// ----- renderer -----

/// Render `tokens[start..end)` against `ctx` into `out`.
fn render(
    tokens: &[Token],
    start: usize,
    end: usize,
    ctx: &Value,
    out: &mut String,
) -> Result<(), FfiError> {
    let mut i = start;
    while i < end {
        match &tokens[i] {
            Token::Text(s) => out.push_str(s),
            Token::Var { path, raw } => render_value(&lookup(ctx, path), *raw, out),
            Token::EachOpen(key) => {
                let close = find_close(tokens, i + 1)?;
                for item in list_items(&lookup(ctx, key)) {
                    render(tokens, i + 1, close, &item, out)?;
                }
                i = close;
            }
            Token::IfOpen(key) => {
                let close = find_close(tokens, i + 1)?;
                if truthy(&lookup(ctx, key)) {
                    render(tokens, i + 1, close, ctx, out)?;
                }
                i = close;
            }
            Token::Close => {
                return Err(fail("template: stray {{/each}} or {{/if}}"));
            }
        }
        i += 1;
    }
    Ok(())
}

// ----- procedures -----

fn template_render(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("template-render", "2", args.len()));
    }
    let template = expect_string("template-render", args, 0)?;
    let data = args[1].clone();
    let tokens = tokenize(&template)?;
    let mut out = String::new();
    render(&tokens, 0, tokens.len(), &data, &mut out)?;
    Ok(string_value(out))
}

fn html_escape_proc(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("html-escape", "1", args.len()));
    }
    let s = expect_string("html-escape", args, 0)?;
    let mut out = String::with_capacity(s.len());
    escape_html(&s, &mut out);
    Ok(string_value(out))
}
