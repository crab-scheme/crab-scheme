//! CrabScheme stdlib module: `(crab yaml)`.
//!
//! YAML parsing and serialization via the maintained `yaml-rust2`. The
//! `(crab …)` answer to Python's PyYAML / Go's gopkg.in/yaml. Pure Rust,
//! wasm-portable.
//!
//! ## Representation
//!
//! YAML maps become association lists `((key . value) …)` with string
//! keys; sequences become lists; scalars become strings / numbers /
//! booleans; null becomes `#f`.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `yaml-parse`     | string | value | First document; maps→alist, seqs→list. |
//! | `yaml-stringify` | value  | string | A list of pairs emits as a map, else a sequence. |
//!
//! ```scheme
//! (import (crab yaml))
//! (yaml-parse "server:\n  host: localhost\n  port: 8080\n")
//! ; => (("server" . (("host" . "localhost") ("port" . 8080))))
//! ```

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};
use yaml_rust2::yaml::Hash as YamlHash;
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("yaml-parse", yaml_parse),
        UntypedProc::new("yaml-stringify", yaml_stringify),
    ]
}

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
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

fn read_list(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    while let Value::Pair(p) = cur {
        out.push(p.car());
        cur = p.cdr();
    }
    out
}

// ----- YAML -> Scheme -----

fn yaml_key_to_string(k: &Yaml) -> Value {
    match k {
        Yaml::String(s) => Value::string(s.clone()),
        Yaml::Integer(i) => Value::string(i.to_string()),
        Yaml::Boolean(b) => Value::string(if *b { "true" } else { "false" }),
        Yaml::Real(s) => Value::string(s.clone()),
        _ => Value::string("?"),
    }
}

fn yaml_to_value(y: &Yaml) -> Value {
    match y {
        Yaml::Boolean(b) => Value::Boolean(*b),
        Yaml::Integer(i) => Value::fixnum(*i),
        Yaml::Real(s) => s
            .parse::<f64>()
            .map(Value::flonum)
            .unwrap_or_else(|_| Value::string(s.clone())),
        Yaml::String(s) => Value::string(s.clone()),
        Yaml::Array(a) => Value::list(a.iter().map(yaml_to_value).collect::<Vec<_>>()),
        Yaml::Hash(h) => Value::list(
            h.iter()
                .map(|(k, v)| {
                    Value::Pair(cs_core::Pair::new(yaml_key_to_string(k), yaml_to_value(v)))
                })
                .collect::<Vec<_>>(),
        ),
        // Null, BadValue, Alias → #f
        _ => Value::Boolean(false),
    }
}

fn yaml_parse(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("yaml-parse", "1", args.len()));
    }
    let src = expect_string("yaml-parse", args, 0)?;
    let docs = YamlLoader::load_from_str(&src)
        .map_err(|e| FfiError::HostFailure(format!("yaml-parse: {}", e)))?;
    match docs.first() {
        Some(y) => Ok(yaml_to_value(y)),
        None => Ok(Value::Boolean(false)),
    }
}

// ----- Scheme -> YAML -----

fn value_to_yaml_key(v: &Value) -> Yaml {
    match v {
        Value::String(s) => Yaml::String(s.borrow().clone()),
        Value::Fixnum(i) => Yaml::Integer(*i),
        Value::Boolean(b) => Yaml::Boolean(*b),
        _ => Yaml::String("?".to_string()),
    }
}

fn value_to_yaml(v: &Value) -> Yaml {
    match v {
        Value::Boolean(b) => Yaml::Boolean(*b),
        Value::Fixnum(i) => Yaml::Integer(*i),
        nv @ (Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)) => {
            let n = nv.as_number().unwrap();
            Yaml::Real(format!("{}", n.to_f64()))
        }
        Value::String(s) => Yaml::String(s.borrow().clone()),
        Value::Null => Yaml::Null,
        Value::Pair(_) => {
            let items = read_list(v);
            // A non-empty proper list whose every element is a pair emits as
            // a YAML map; otherwise as a sequence.
            if !items.is_empty() && items.iter().all(|x| matches!(x, Value::Pair(_))) {
                let mut h = YamlHash::new();
                for it in &items {
                    if let Value::Pair(p) = it {
                        h.insert(value_to_yaml_key(&p.car()), value_to_yaml(&p.cdr()));
                    }
                }
                Yaml::Hash(h)
            } else {
                Yaml::Array(items.iter().map(value_to_yaml).collect())
            }
        }
        _ => Yaml::Null,
    }
}

fn yaml_stringify(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("yaml-stringify", "1", args.len()));
    }
    let y = value_to_yaml(&args[0]);
    let mut out = String::new();
    YamlEmitter::new(&mut out)
        .dump(&y)
        .map_err(|e| FfiError::HostFailure(format!("yaml-stringify: {:?}", e)))?;
    Ok(Value::string(out))
}
