//! CrabScheme stdlib module: `(crab)` meta / introspection.
//!
//! Iter 14 of the `stdlib-modules` spec.
//!
//! Two procedures let user code discover which `(crab …)` modules
//! were compiled into the current build:
//!
//! - `(crab-list-modules)` returns an alphabetical list of module
//!   name strings (e.g. `("base" "collection" "fs" …)`).
//! - `(crab-module-procedures module-name)` returns the list of
//!   procedure names that module registered (e.g. for `"fs"`:
//!   `("read-file-string" "write-file-string" …)`), or `#f` if
//!   the module isn't compiled in.
//!
//! The manifest is derived at runtime by enumerating each enabled
//! cs-stdlib-* crate's `procs()` — no hand-maintained name list to
//! drift out of sync.

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("crab-list-modules", crab_list_modules),
        UntypedProc::new("crab-module-procedures", crab_module_procedures),
    ]
}

// ----- manifest assembly -----

fn string_value(s: impl Into<String>) -> Value {
    Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.into())))
}

/// `(name, list-of-procedure-names)` for every compiled-in module.
fn manifest() -> Vec<(&'static str, Vec<String>)> {
    let mut out: Vec<(&'static str, Vec<String>)> = Vec::new();

    macro_rules! add {
        ($name:literal, $feat:literal, $module:ident) => {
            #[cfg(feature = $feat)]
            {
                let names: Vec<String> = $module::procs()
                    .iter()
                    .map(|p| p.name().to_string())
                    .collect();
                out.push(($name, names));
            }
        };
    }

    add!("path", "meta-path", cs_stdlib_path);
    add!("fs", "meta-fs", cs_stdlib_fs);
    add!("os", "meta-os", cs_stdlib_os);
    add!("process", "meta-process", cs_stdlib_process);
    add!("string", "meta-string", cs_stdlib_string);
    add!("format", "meta-format", cs_stdlib_format);
    add!("regex", "meta-regex", cs_stdlib_regex);
    add!("time", "meta-time", cs_stdlib_time);
    add!("random", "meta-random", cs_stdlib_random);
    add!("uuid", "meta-uuid", cs_stdlib_uuid);
    add!("json", "meta-json", cs_stdlib_json);
    add!("csv", "meta-csv", cs_stdlib_csv);
    add!("toml", "meta-toml", cs_stdlib_toml);
    add!("base", "meta-base", cs_stdlib_base);
    add!("url", "meta-url", cs_stdlib_url);
    add!("hash", "meta-hash", cs_stdlib_hash);
    add!("compress", "meta-compress", cs_stdlib_compress);
    add!("deflate", "meta-deflate", cs_stdlib_deflate);
    add!("archive", "meta-archive", cs_stdlib_archive);
    add!("log", "meta-log", cs_stdlib_log);
    add!("metrics", "meta-metrics", cs_stdlib_metrics);
    add!("net", "meta-net", cs_stdlib_net);
    add!("http", "meta-http", cs_stdlib_http);
    add!("websocket", "meta-websocket", cs_stdlib_websocket);
    add!("collection", "meta-collection", cs_stdlib_collection);
    add!("math", "meta-math", cs_stdlib_math);
    add!("tty", "meta-tty", cs_stdlib_tty);
    add!("signal", "meta-signal", cs_stdlib_signal);
    add!("cli", "meta-cli", cs_stdlib_cli);
    add!("crypto", "meta-crypto", cs_stdlib_crypto);
    add!("sql", "meta-sql", cs_stdlib_sql);
    add!("xml", "meta-xml", cs_stdlib_xml);
    add!("binary", "meta-binary", cs_stdlib_binary);
    add!("template", "meta-template", cs_stdlib_template);
    add!("ini", "meta-ini", cs_stdlib_ini);
    add!("yaml", "meta-yaml", cs_stdlib_yaml);

    out.sort_by_key(|(n, _)| *n);
    out
}

// ----- procedures -----

fn crab_list_modules(args: &[Value]) -> Result<Value, FfiError> {
    if !args.is_empty() {
        return Err(FfiError::ArityError {
            name: "crab-list-modules".into(),
            expected: "0".into(),
            got: args.len(),
        });
    }
    let names: Vec<Value> = manifest()
        .into_iter()
        .map(|(n, _)| string_value(n.to_string()))
        .collect();
    Ok(Value::list(names))
}

fn crab_module_procedures(args: &[Value]) -> Result<Value, FfiError> {
    let needle = match args.first() {
        Some(Value::String(s)) => s.borrow().clone(),
        Some(other) => {
            return Err(FfiError::TypeMismatch {
                expected: "string",
                got: other.type_name().to_string(),
            })
        }
        None => {
            return Err(FfiError::ArityError {
                name: "crab-module-procedures".into(),
                expected: "1".into(),
                got: 0,
            })
        }
    };
    for (name, procs) in manifest() {
        if name == needle {
            let entries: Vec<Value> = procs.into_iter().map(string_value).collect();
            return Ok(Value::list(entries));
        }
    }
    Ok(Value::Boolean(false))
}
