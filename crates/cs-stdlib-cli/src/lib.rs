//! CrabScheme stdlib module: `(crab cli)`.
//!
//! Command-line argument and flag parsing — the `(crab …)` answer
//! to Python's `argparse`, Go's `flag`, and Clojure's `tools.cli`.
//!
//! The module is **pure**: parsing is a function of an option spec
//! plus an argument list, with no global state and no I/O. Obtain
//! the argument list however you like — typically
//! `(cdr (command-line))` (R6RS §6.4 — the `car` is the program
//! name) or any list of strings — and feed it to `cli-parse`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `cli-option`  | long short kind default help | option  | Build one option descriptor. |
//! | `cli-option?` | value                        | boolean | Descriptor predicate. |
//! | `cli-parse`   | options args                 | alist   | Parse `args` against `options`. |
//! | `cli-usage`   | prog description options     | string  | Formatted `--help` text. |
//!
//! ### Option descriptors
//!
//! `(cli-option long short kind default help)` builds one option:
//!
//! - `long` — string, the `--long` name (no leading dashes).
//! - `short` — single-character string (matched as `-x`) or `#f`.
//! - `kind` — one of the strings `"flag"`, `"string"`, `"int"`, or
//!   `"float"`. A `"flag"` takes no value and parses to a boolean;
//!   the others take a value.
//! - `default` — value used when the option is absent (usually `#f`
//!   for a flag).
//! - `help` — help string or `#f`.
//!
//! ### Parse result
//!
//! `(cli-parse options args)` returns an association list mapping
//! each option's `long` name to its parsed value (defaults filled
//! in for absent options), plus the key `"--"` whose value is the
//! list of positional arguments (everything that isn't an option,
//! including all tokens after a literal `--`). Look values up with
//! `assoc`:
//!
//! ```scheme
//! (define opts
//!   (list (cli-option "verbose" "v" "flag"   #f      "be loud")
//!         (cli-option "name"    #f  "string" "world" "who to greet")
//!         (cli-option "count"   "n" "int"    1       "repeat count")))
//!
//! (define r (cli-parse opts (list "--name=Ada" "-n" "3" "-v" "extra")))
//! (cdr (assoc "verbose" r))   ; => #t
//! (cdr (assoc "name" r))      ; => "Ada"
//! (cdr (assoc "count" r))     ; => 3
//! (cdr (assoc "--" r))        ; => ("extra")
//! ```
//!
//! ### Accepted argument syntax
//!
//! - `--long`, `--long=value`, `--long value`
//! - `-x`, `-x=value`, `-x value`
//! - a literal `--` ends option parsing; the rest are positional
//! - a lone `-` is a positional (stdin convention)
//!
//! A value option consumes the following token as its value, so a
//! value that itself starts with `-` must use the `=` form
//! (`--name=-5`); a negative number passed as the next token still
//! works (`--count -5`, because `-5` is consumed as the value).
//! Short-flag clustering (`-abc`) is **not** supported in this
//! version — write `-a -b -c`. Unknown options, a missing value, or
//! a value that fails to parse as `int`/`float` raise an error.

use std::cell::RefCell;
use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

/// Tag stored at index 0 of an option descriptor vector so
/// `cli-option?` and `read_option` can recognize one robustly.
const OPT_TAG: &str = "__cli-opt__";

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("cli-option", cli_option),
        UntypedProc::new("cli-option?", cli_option_p),
        UntypedProc::new("cli-parse", cli_parse),
        UntypedProc::new("cli-usage", cli_usage),
    ]
}

// ----- internal model -----

enum Kind {
    Flag,
    Str,
    Int,
    Float,
}

struct OptSpec {
    long: String,
    short: Option<String>,
    kind: Kind,
    default: Value,
    help: Option<String>,
}

fn kind_str(k: &Kind) -> &'static str {
    match k {
        Kind::Flag => "flag",
        Kind::Str => "string",
        Kind::Int => "int",
        Kind::Float => "float",
    }
}

fn parse_kind(s: &str) -> Result<Kind, FfiError> {
    match s {
        "flag" => Ok(Kind::Flag),
        "string" => Ok(Kind::Str),
        "int" => Ok(Kind::Int),
        "float" => Ok(Kind::Float),
        other => Err(fail(&format!(
            "cli-option: kind must be \"flag\", \"string\", \"int\", or \"float\"; got {:?}",
            other
        ))),
    }
}

// ----- error helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn fail(msg: &str) -> FfiError {
    FfiError::HostFailure(msg.to_string())
}

fn type_mismatch(expected: &'static str, got: &Value) -> FfiError {
    FfiError::TypeMismatch {
        expected,
        got: got.type_name().to_string(),
    }
}

// ----- descriptor recognition -----

fn is_tag(v: &Value) -> bool {
    matches!(v, Value::String(s) if s.borrow().as_str() == OPT_TAG)
}

fn is_opt_vector(v: &Value) -> bool {
    if let Value::Vector(items) = v {
        let items = items.borrow();
        items.len() == 6 && is_tag(&items[0])
    } else {
        false
    }
}

fn read_option(v: &Value) -> Result<OptSpec, FfiError> {
    let items = match v {
        Value::Vector(items) => items.borrow(),
        other => {
            return Err(fail(&format!(
                "cli: expected a cli-option descriptor; got {}",
                other.type_name()
            )))
        }
    };
    if items.len() != 6 || !is_tag(&items[0]) {
        return Err(fail(
            "cli: expected a cli-option descriptor (build one with cli-option)",
        ));
    }
    let long = match &items[1] {
        Value::String(s) => s.borrow().clone(),
        _ => return Err(fail("cli-option: long name must be a string")),
    };
    let short = match &items[2] {
        Value::String(s) => Some(s.borrow().clone()),
        _ => None,
    };
    let kind = match &items[3] {
        Value::String(s) => parse_kind(&s.borrow())?,
        _ => return Err(fail("cli-option: kind must be a string")),
    };
    let default = items[4].clone();
    let help = match &items[5] {
        Value::String(s) => Some(s.borrow().clone()),
        _ => None,
    };
    Ok(OptSpec {
        long,
        short,
        kind,
        default,
        help,
    })
}

fn read_options(v: &Value) -> Result<Vec<OptSpec>, FfiError> {
    let mut cur = v.clone();
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(read_option(&p.car())?);
                cur = p.cdr();
            }
            other => {
                return Err(fail(&format!(
                    "cli: options must be a proper list of cli-option descriptors; got {}",
                    other.type_name()
                )))
            }
        }
    }
}

fn collect_string_list(v: &Value, ctx: &str) -> Result<Vec<String>, FfiError> {
    let mut cur = v.clone();
    let mut out = Vec::new();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                match p.car() {
                    Value::String(s) => out.push(s.borrow().clone()),
                    other => {
                        return Err(fail(&format!(
                            "{}: expected list of strings; got element of type {}",
                            ctx,
                            other.type_name()
                        )))
                    }
                }
                cur = p.cdr();
            }
            other => {
                return Err(fail(&format!(
                    "{}: expected a proper list of strings; got {}",
                    ctx,
                    other.type_name()
                )))
            }
        }
    }
}

// ----- cli-option -----

fn cli_option(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 5 {
        return Err(arity("cli-option", "5", args.len()));
    }
    let long = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_mismatch("string (long name)", other)),
    };
    if long.is_empty() {
        return Err(fail("cli-option: long name must be non-empty"));
    }
    if long.starts_with('-') {
        return Err(fail(
            "cli-option: long name must not start with '-' (pass \"verbose\", not \"--verbose\")",
        ));
    }
    let short_v = match &args[1] {
        Value::Boolean(false) => Value::Boolean(false),
        Value::String(s) => {
            let st = s.borrow();
            if st.chars().count() != 1 {
                return Err(fail(
                    "cli-option: short name must be a single character or #f",
                ));
            }
            Value::string(st.clone())
        }
        other => {
            return Err(type_mismatch(
                "single-character string or #f (short name)",
                other,
            ))
        }
    };
    let kind = match &args[2] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_mismatch("string (kind)", other)),
    };
    parse_kind(&kind)?; // validate eagerly so a bad kind errors at build time
    let default = args[3].clone();
    let help_v = match &args[4] {
        Value::Boolean(false) => Value::Boolean(false),
        Value::String(_) => args[4].clone(),
        other => return Err(type_mismatch("string or #f (help)", other)),
    };
    let vec = vec![
        Value::string(OPT_TAG),
        Value::string(long),
        short_v,
        Value::string(kind),
        default,
        help_v,
    ];
    Ok(Value::Vector(cs_core::Gc::new(RefCell::new(vec))))
}

fn cli_option_p(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("cli-option?", "1", args.len()));
    }
    Ok(Value::Boolean(is_opt_vector(&args[0])))
}

// ----- cli-parse -----

fn parse_value(kind: &Kind, raw: &str, display: &str) -> Result<Value, FfiError> {
    match kind {
        Kind::Str => Ok(Value::string(raw.to_string())),
        Kind::Int => raw.trim().parse::<i64>().map(Value::fixnum).map_err(|_| {
            fail(&format!(
                "cli-parse: {} expects an integer, got {:?}",
                display, raw
            ))
        }),
        Kind::Float => raw.trim().parse::<f64>().map(Value::flonum).map_err(|_| {
            fail(&format!(
                "cli-parse: {} expects a number, got {:?}",
                display, raw
            ))
        }),
        Kind::Flag => unreachable!("flags are handled without a value"),
    }
}

fn cli_parse(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("cli-parse", "2", args.len()));
    }
    let opts = read_options(&args[0])?;
    let argv = collect_string_list(&args[1], "cli-parse args")?;

    // Result values in option order, seeded with each option's default.
    let mut values: Vec<Value> = opts.iter().map(|o| o.default.clone()).collect();
    let mut positionals: Vec<Value> = Vec::new();
    let mut stop = false;

    let mut i = 0;
    while i < argv.len() {
        let tok = &argv[i];
        // Positional: after `--`, a lone `-`, or any non-dash token.
        if stop || tok == "-" || !tok.starts_with('-') {
            positionals.push(Value::string(tok.clone()));
            i += 1;
            continue;
        }
        // A literal `--` ends option parsing.
        if tok == "--" {
            stop = true;
            i += 1;
            continue;
        }
        // Option token. Split into name + optional inline `=value`.
        let (is_long, body) = match tok.strip_prefix("--") {
            Some(rest) => (true, rest),
            None => (false, &tok[1..]),
        };
        let (name_part, inline_val) = match body.find('=') {
            Some(p) => (&body[..p], Some(body[p + 1..].to_string())),
            None => (body, None),
        };
        // Resolve the option index.
        let idx = if is_long {
            opts.iter()
                .position(|o| o.long == name_part)
                .ok_or_else(|| fail(&format!("cli-parse: unknown option --{}", name_part)))?
        } else {
            if name_part.chars().count() != 1 {
                return Err(fail(&format!(
                    "cli-parse: malformed short option -{} \
                     (clustering is not supported; write -a -b)",
                    name_part
                )));
            }
            opts.iter()
                .position(|o| o.short.as_deref() == Some(name_part))
                .ok_or_else(|| fail(&format!("cli-parse: unknown option -{}", name_part)))?
        };
        let display = format!("--{}", opts[idx].long);
        match opts[idx].kind {
            Kind::Flag => {
                if inline_val.is_some() {
                    return Err(fail(&format!(
                        "cli-parse: {} is a flag and takes no value",
                        display
                    )));
                }
                values[idx] = Value::Boolean(true);
                i += 1;
            }
            _ => {
                let raw = match inline_val {
                    Some(v) => {
                        i += 1;
                        v
                    }
                    None => {
                        if i + 1 >= argv.len() {
                            return Err(fail(&format!("cli-parse: {} requires a value", display)));
                        }
                        let v = argv[i + 1].clone();
                        i += 2;
                        v
                    }
                };
                values[idx] = parse_value(&opts[idx].kind, &raw, &display)?;
            }
        }
    }

    // Build the result alist: (long . value) … then ("--" . positionals).
    let mut entries: Vec<Value> = Vec::with_capacity(opts.len() + 1);
    for (o, v) in opts.iter().zip(values) {
        entries.push(Value::Pair(cs_core::Pair::new(
            Value::string(o.long.clone()),
            v,
        )));
    }
    entries.push(Value::Pair(cs_core::Pair::new(
        Value::string("--"),
        Value::list(positionals),
    )));
    Ok(Value::list(entries))
}

// ----- cli-usage -----

/// Render an option's default for the usage line, or `None` when
/// there's no useful default to show (a `#f` default reads as
/// "unset"). Self-contained so the crate needs no `Number` internals.
fn render_default(v: &Value) -> Option<String> {
    match v {
        Value::Boolean(false) => None,
        Value::Boolean(true) => Some("#t".to_string()),
        Value::String(s) => Some(s.borrow().clone()),
        nv @ (Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)) => {
            let n = nv.as_number().unwrap();
            let f = n.to_f64();
            if f.fract() == 0.0 && f.abs() < 1e15 {
                Some(format!("{}", f as i64))
            } else {
                Some(format!("{}", f))
            }
        }
        _ => None,
    }
}

fn format_usage(prog: &str, desc: Option<&str>, opts: &[OptSpec]) -> String {
    let mut lines: Vec<String> = vec![format!("Usage: {} [options]", prog)];
    if let Some(d) = desc {
        lines.push(String::new());
        lines.push(d.to_string());
    }
    if !opts.is_empty() {
        lines.push(String::new());
        lines.push("Options:".to_string());
        // Left column: "-x, --long <kind>".
        let left: Vec<String> = opts
            .iter()
            .map(|o| {
                let mut s = match &o.short {
                    Some(c) => format!("-{}, ", c),
                    None => "    ".to_string(),
                };
                s.push_str(&format!("--{}", o.long));
                if !matches!(o.kind, Kind::Flag) {
                    s.push_str(&format!(" <{}>", kind_str(&o.kind)));
                }
                s
            })
            .collect();
        let width = left.iter().map(|s| s.len()).max().unwrap_or(0);
        for (o, l) in opts.iter().zip(left.iter()) {
            let mut right = o.help.clone().unwrap_or_default();
            if let Some(dn) = render_default(&o.default) {
                if !right.is_empty() {
                    right.push(' ');
                }
                right.push_str(&format!("(default: {})", dn));
            }
            if right.is_empty() {
                lines.push(format!("  {}", l));
            } else {
                lines.push(format!("  {:width$}  {}", l, right, width = width));
            }
        }
    }
    lines.join("\n")
}

fn cli_usage(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 3 {
        return Err(arity("cli-usage", "3", args.len()));
    }
    let prog = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_mismatch("string (program name)", other)),
    };
    let desc = match &args[1] {
        Value::String(s) => Some(s.borrow().clone()),
        Value::Boolean(false) => None,
        other => return Err(type_mismatch("string or #f (description)", other)),
    };
    let opts = read_options(&args[2])?;
    Ok(Value::string(format_usage(&prog, desc.as_deref(), &opts)))
}
