//! ADR 0015 L2 — Scheme builtins for the WASM-instance sandbox.
//!
//! Gated by the `sandbox` Cargo feature. Wires
//! `cs-sandbox-wasm` into cs-runtime as a small set of Scheme
//! procedures:
//!
//! - `(make-wasm-sandbox preset [binary-path])` — preset is
//!   one of `'hygiene` / `'plugin` / `'adversarial`.
//!   Returns a sandbox value (vector record).
//! - `(sandbox? v)` — predicate.
//! - `(sandbox-eval s expr-source)` — evaluates `expr-source`
//!   (a string) inside the sandbox; returns the printed result
//!   string from the guest's stdout. Errors raise an
//!   `&error` condition with the SandboxError's display text.
//! - `(reset-sandbox! s)` — rebuilds the guest instance.
//! - `(drop-sandbox! s)` — releases the instance's wasmtime
//!   resources (Engine, Module cache, Linker, memory). After a
//!   drop, the sandbox value is still a valid `#('__sandbox__
//!   id)` shape (`sandbox?` returns `#t`) but `sandbox-eval` /
//!   `reset-sandbox!` raise `"sandbox has been dropped"`.
//!
//! Sandbox instances are stored in a thread-local registry
//! keyed by `u32` id. The Scheme value is `#('__sandbox__ id)`.
//! Without `drop-sandbox!` callers, sandboxes accumulate for the
//! thread's lifetime — fine for low-cardinality use (1–3 per
//! process) but a leak in long-lived servers that spawn many
//! sandboxes per request. Use `drop-sandbox!` explicitly in that
//! shape. Memory cost: ~MB per live sandbox (wasmtime Engine +
//! Module cache).

use std::cell::RefCell;

use cs_core::Value;
use cs_sandbox_wasm::{SandboxConfig, SandboxInstance};

use super::{arity_err, new_vector, type_err};

pub(crate) const SANDBOX_TAG: &str = "__sandbox__";

thread_local! {
    /// Per-thread sandbox registry. Indexed by `u32` id stored
    /// in the Scheme `#('__sandbox__ id)` record. The `Option`
    /// shape distinguishes a live slot (`Some(inst)`) from a
    /// dropped one (`None`) — ids stay stable across drops so
    /// `decode_sandbox_id` continues to work for diagnostics
    /// even after `drop-sandbox!`. The `Vec` is monotonic
    /// (ids never repurposed) which keeps the id space
    /// dead-tomb-stoning rather than reuse-after-free; for
    /// processes that spawn many sandboxes the right pattern is
    /// to call `drop-sandbox!` on each as soon as you're done.
    static SANDBOXES: RefCell<Vec<Option<SandboxInstance>>> = const { RefCell::new(Vec::new()) };
}

fn register_sandbox(sb: SandboxInstance) -> u32 {
    SANDBOXES.with(|slot| {
        let mut v = slot.borrow_mut();
        let id = v.len() as u32;
        v.push(Some(sb));
        id
    })
}

fn with_sandbox<R>(
    id: u32,
    f: impl FnOnce(&mut SandboxInstance) -> Result<R, String>,
) -> Result<R, String> {
    SANDBOXES.with(|slot| {
        let mut v = slot.borrow_mut();
        let idx = id as usize;
        let opt = v
            .get_mut(idx)
            .ok_or_else(|| format!("sandbox id {} out of range", id))?;
        let inst = opt
            .as_mut()
            .ok_or_else(|| "sandbox has been dropped".to_string())?;
        f(inst)
    })
}

/// Internal: check whether `v` is a `#('__sandbox__ id)` record.
pub(crate) fn is_sandbox_value(v: &Value) -> bool {
    if let Value::Vector(items) = v {
        let items = items.borrow();
        if items.len() == 2 {
            if let Value::String(s) = &items[0] {
                return s.borrow().as_str() == SANDBOX_TAG;
            }
        }
    }
    false
}

fn decode_sandbox_id(v: &Value) -> Option<u32> {
    let Value::Vector(items) = v else {
        return None;
    };
    let items = items.borrow();
    if items.len() != 2 {
        return None;
    }
    if !matches!(&items[0], Value::String(s) if s.borrow().as_str() == SANDBOX_TAG) {
        return None;
    }
    // The id was stored as Number::from_i64; round-trip via the
    // shared as_int_i64_pub helper avoids re-implementing the
    // BigInt-narrowing logic here.
    super::as_int_i64_pub("__sandbox-id__", &items[1])
        .ok()
        .and_then(|i| u32::try_from(i).ok())
}

fn preset_from_symbol(name: &str) -> Result<SandboxConfig, String> {
    match name {
        "hygiene" => Ok(SandboxConfig::hygiene()),
        "plugin" => Ok(SandboxConfig::plugin()),
        "adversarial" => Ok(SandboxConfig::adversarial()),
        other => Err(format!(
            "make-wasm-sandbox: unknown preset {:?} (expected 'hygiene, \
             'plugin, or 'adversarial)",
            other
        )),
    }
}

/// `(make-wasm-sandbox preset [binary-path])` — construct a new
/// sandbox. `preset` must be a symbol; `binary-path`, when
/// supplied, must be a string. When omitted, the runtime falls
/// back to the `CRABSCHEME_WASM_PATH` env var.
fn b_make_wasm_sandbox(args: &[Value], syms: &mut cs_core::SymbolTable) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("make-wasm-sandbox", "1 or 2", args.len()));
    }
    let sym = match &args[0] {
        Value::Symbol(s) => *s,
        v => return Err(type_err("make-wasm-sandbox", "symbol (preset)", v)),
    };
    let preset_name = syms.name(sym).to_string();
    let mut config = preset_from_symbol(&preset_name)?;
    if args.len() == 2 {
        let path = match &args[1] {
            Value::String(s) => std::path::PathBuf::from(s.borrow().clone()),
            v => return Err(type_err("make-wasm-sandbox", "string (binary path)", v)),
        };
        config.binary_path = Some(path);
    } else if let Ok(env_path) = std::env::var("CRABSCHEME_WASM_PATH") {
        config.binary_path = Some(env_path.into());
    }
    let inst = SandboxInstance::new(config).map_err(|e| format!("make-wasm-sandbox: {}", e))?;
    let id = register_sandbox(inst);
    Ok(new_vector(vec![
        Value::string(SANDBOX_TAG),
        Value::Fixnum(id.into()),
    ]))
}

fn b_sandbox_p(args: &[Value], _syms: &mut cs_core::SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("sandbox?", "1", args.len()));
    }
    Ok(Value::Boolean(is_sandbox_value(&args[0])))
}

fn b_sandbox_eval(args: &[Value], _syms: &mut cs_core::SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("sandbox-eval", "2", args.len()));
    }
    let id = decode_sandbox_id(&args[0])
        .ok_or_else(|| "sandbox-eval: first argument is not a sandbox".to_string())?;
    let expr = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("sandbox-eval", "string (expression source)", v)),
    };
    let result = with_sandbox(id, |sb| {
        sb.eval(&expr).map_err(|e| format!("sandbox-eval: {}", e))
    })?;
    Ok(Value::string(result))
}

fn b_reset_sandbox(args: &[Value], _syms: &mut cs_core::SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("reset-sandbox!", "1", args.len()));
    }
    let id = decode_sandbox_id(&args[0])
        .ok_or_else(|| "reset-sandbox!: argument is not a sandbox".to_string())?;
    with_sandbox(id, |sb| {
        sb.reset().map_err(|e| format!("reset-sandbox!: {}", e))
    })?;
    Ok(Value::Unspecified)
}

/// `(drop-sandbox! s)` — release the instance's wasmtime
/// resources. The slot is tomb-stoned (set to `None`) rather
/// than removed so id stability holds; subsequent `sandbox-eval`
/// / `reset-sandbox!` on the same value raise `"sandbox has been
/// dropped"`. Idempotent: dropping an already-dropped sandbox is
/// a no-op (Unspecified). A non-sandbox argument is the same
/// type error every other primop returns.
fn b_drop_sandbox(args: &[Value], _syms: &mut cs_core::SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("drop-sandbox!", "1", args.len()));
    }
    let id = decode_sandbox_id(&args[0])
        .ok_or_else(|| "drop-sandbox!: argument is not a sandbox".to_string())?;
    SANDBOXES.with(|slot| {
        let mut v = slot.borrow_mut();
        let idx = id as usize;
        if let Some(opt) = v.get_mut(idx) {
            // Drop the inner SandboxInstance (releases wasmtime
            // Engine + Module cache + Linker + Store). Tomb-stone
            // the slot to None so id stability holds for
            // diagnostics. Idempotent on an already-None slot.
            *opt = None;
        }
        // Out-of-range id is a no-op rather than an error: the
        // value's id space is monotonic; an id that's "past the
        // end" can only happen via a forged record, in which
        // case dropping is the right thing anyway.
        Ok::<(), String>(())
    })?;
    Ok(Value::Unspecified)
}

type SandboxSymsFn = fn(&[Value], &mut cs_core::SymbolTable) -> Result<Value, String>;

/// Entries to fold into `syms_builtins()`. Gated by the `sandbox`
/// feature; the main builtins table calls `sandbox::builtins()`
/// under cfg. Promoted from HoBuiltin to SymsBuiltin so both
/// walker AND VM tiers can dispatch to them — without this the
/// `make-wasm-sandbox` Scheme surface was walker-only and
/// `--tier vm` users saw "undefined variable: make-wasm-sandbox".
pub fn builtins() -> Vec<(&'static str, SandboxSymsFn)> {
    vec![
        ("make-wasm-sandbox", b_make_wasm_sandbox as SandboxSymsFn),
        ("sandbox?", b_sandbox_p as SandboxSymsFn),
        ("sandbox-eval", b_sandbox_eval as SandboxSymsFn),
        ("reset-sandbox!", b_reset_sandbox as SandboxSymsFn),
        ("drop-sandbox!", b_drop_sandbox as SandboxSymsFn),
    ]
}
