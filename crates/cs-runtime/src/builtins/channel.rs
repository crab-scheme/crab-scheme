//! Channel primops exposed to Scheme. Behind the `channel`
//! feature. Spec: `docs/research/channels_spec.md`.
//!
//! Surface (registered into the top-level env by
//! `channel_syms_builtins`):
//!
//! ```ignore
//! ; Construct
//! (make-channel)               ; unbounded
//! (make-channel 100)           ; bounded, cap 100
//! ;; capacity 0 (rendezvous) is a CH-C deliverable, not v1.
//!
//! ; Send / recv
//! (channel-send!     ch v)     ; blocking; unspec
//! (channel-try-send! ch v)     ; non-blocking; #t / #f
//! (channel-recv      ch)       ; blocking; v or '*closed*
//! (channel-try-recv  ch)       ; non-blocking; v / '*empty* / '*closed*
//!
//! ; Lifecycle
//! (channel-close!    ch)       ; unspec
//! (channel-closed?   ch)       ; #t / #f
//! (channel-len       ch)       ; fixnum
//! (channel-capacity  ch)       ; fixnum or #f
//! (channel?          v)        ; #t / #f
//! ```
//!
//! Channel values surface as the tagged pair `(channel <id>)` —
//! same convention as `('*web-request* h)` from cs-web and PIDs
//! like `<pid:<n.m>>`. The 2-element list rides freely across
//! actor boundaries via the existing Value ↔ SendableValue
//! converters (no dedicated SendableValue variant needed).
//!
//! Operations that require blocking (`channel-send!` to a full
//! bounded channel, `channel-recv` on an empty one) must run
//! inside an actor body — they need a tokio context to drive
//! the underlying mpsc await. From the REPL or a non-actor
//! script, they error with a clear message; the try-* variants
//! work everywhere.

#![cfg(feature = "channel")]

use std::sync::{Arc, OnceLock};

use cs_actor::Payload;
use cs_channel::{ChannelError, ChannelId, ChannelRegistry};
use cs_core::{Number, Pair, SymbolTable, Value};

use crate::builtins::beam::{from_sendable, to_sendable_in, SendableValue};

// ---------------------------------------------------------------
// Process-global registry. Same lifetime model as beam_state and
// cs-web's server registry.
// ---------------------------------------------------------------

/// Process-global channel registry. Exposed so embedders (and
/// tests) can reach a channel by ID without going through the
/// Scheme-value wrapper. Don't store the returned reference;
/// the OnceLock is initialized lazily on first call.
pub fn registry() -> &'static ChannelRegistry {
    static R: OnceLock<ChannelRegistry> = OnceLock::new();
    R.get_or_init(ChannelRegistry::new)
}

// ---------------------------------------------------------------
// Async dispatch helper. Channel ops need a tokio context; we use
// the calling thread's via `Handle::try_current`. Inside an actor
// body (spawn_sync_body_on_task wraps the body in
// `block_in_place`), this returns the cs-actor runtime handle.
// From the REPL or a non-actor script, it errors.
// ---------------------------------------------------------------

fn block_on<F: std::future::Future>(who: &str, fut: F) -> Result<F::Output, String> {
    let handle = tokio::runtime::Handle::try_current().map_err(|_| {
        format!(
            "{}: called outside an actor (no tokio context). \
             Wrap the call in (spawn 'name ...) or a Scheme actor body.",
            who
        )
    })?;
    Ok(tokio::task::block_in_place(|| handle.block_on(fut)))
}

// ---------------------------------------------------------------
// Channel-value shape: `(channel <id-fixnum>)`. Encoding /
// decoding helpers.
// ---------------------------------------------------------------

fn make_channel_value(id: ChannelId, syms: &mut SymbolTable) -> Value {
    let tag = Value::Symbol(syms.intern("channel"));
    let id_v = Value::Number(Number::Fixnum(id.0 as i64));
    Value::Pair(Pair::new(tag, Value::Pair(Pair::new(id_v, Value::Null))))
}

/// Decode `(channel <id>)` → `ChannelId`. Returns Err for any
/// other shape (non-pair, wrong tag, wrong cdr).
fn channel_value_to_id(v: &Value, syms: &SymbolTable, who: &str) -> Result<ChannelId, String> {
    let (head, tail) = match v {
        Value::Pair(p) => (p.car.borrow(), p.cdr.borrow()),
        other => {
            return Err(format!(
                "{}: expected a channel value, got {}",
                who,
                other.type_name()
            ));
        }
    };
    match &*head {
        Value::Symbol(s) if syms.name(*s) == "channel" => {}
        _ => return Err(format!("{}: not a channel value (wrong tag)", who)),
    }
    let (id, rest) = match &*tail {
        Value::Pair(p) => (p.car.borrow(), p.cdr.borrow()),
        _ => return Err(format!("{}: malformed channel value (no id slot)", who)),
    };
    match (&*id, &*rest) {
        (Value::Number(Number::Fixnum(n)), Value::Null) => {
            if *n < 0 {
                Err(format!(
                    "{}: channel id must be non-negative, got {}",
                    who, n
                ))
            } else {
                Ok(ChannelId(*n as u64))
            }
        }
        _ => Err(format!("{}: malformed channel value (bad id)", who)),
    }
}

fn channel_is_value(v: &Value, syms: &SymbolTable) -> bool {
    channel_value_to_id(v, syms, "").is_ok()
}

fn fetch(id: ChannelId, who: &str) -> Result<Arc<cs_channel::Channel>, String> {
    registry()
        .lookup(id)
        .ok_or_else(|| format!("{}: channel {} not found (already dropped?)", who, id))
}

// ---------------------------------------------------------------
// Primops (host-fn shape). The `b_channel_*` wrappers convert
// Value ↔ args and call into the primop bodies.
// ---------------------------------------------------------------

fn check_arity(who: &str, args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(format!(
            "{}: expected {} argument{}, got {}",
            who,
            expected,
            if expected == 1 { "" } else { "s" },
            args.len()
        ))
    }
}

fn value_to_i64(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Number(Number::Fixnum(n)) => Ok(*n),
        other => Err(format!(
            "{}: expected fixnum, got {}",
            who,
            other.type_name()
        )),
    }
}

pub fn b_make_channel(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    let capacity = match args.len() {
        0 => None,
        1 => {
            let n = value_to_i64(&args[0], "make-channel")?;
            if n < 0 {
                return Err("make-channel: capacity must be non-negative".into());
            }
            if n == 0 {
                return Err(
                    "make-channel: capacity 0 (unbuffered rendezvous) is a CH-C deliverable, \
                     not implemented in v1. Use (make-channel) for unbounded or (make-channel n>0) for bounded."
                        .into(),
                );
            }
            Some(n as usize)
        }
        _ => {
            return Err(format!(
                "make-channel: expected 0 or 1 arguments, got {}",
                args.len()
            ))
        }
    };
    let id = registry().create(capacity);
    Ok(make_channel_value(id, syms))
}

pub fn b_channel_p(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel?", args, 1)?;
    Ok(Value::Boolean(channel_is_value(&args[0], syms)))
}

pub fn b_channel_send(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-send!", args, 2)?;
    let id = channel_value_to_id(&args[0], syms, "channel-send!")?;
    let payload_sv = to_sendable_in(&args[1], syms)?;
    let ch = fetch(id, "channel-send!")?;
    let payload: Payload = Arc::new(payload_sv);
    block_on("channel-send!", ch.send(payload))?.map_err(|e| format!("channel-send!: {}", e))?;
    Ok(Value::Unspecified)
}

pub fn b_channel_try_send(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-try-send!", args, 2)?;
    let id = channel_value_to_id(&args[0], syms, "channel-try-send!")?;
    let payload_sv = to_sendable_in(&args[1], syms)?;
    let ch = fetch(id, "channel-try-send!")?;
    let payload: Payload = Arc::new(payload_sv);
    match ch.try_send(payload) {
        Ok(true) => Ok(Value::Boolean(true)),
        Ok(false) => Ok(Value::Boolean(false)),
        Err(ChannelError::Closed(_)) => Err("channel-try-send!: send on closed channel".into()),
        Err(e) => Err(format!("channel-try-send!: {}", e)),
    }
}

fn payload_to_value(p: Payload, syms: &mut SymbolTable) -> Value {
    match p.downcast::<SendableValue>() {
        Ok(arc_sv) => from_sendable(&arc_sv, syms),
        Err(_) => Value::Symbol(syms.intern("*opaque-payload*")),
    }
}

pub fn b_channel_recv(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-recv", args, 1)?;
    let id = channel_value_to_id(&args[0], syms, "channel-recv")?;
    let ch = fetch(id, "channel-recv")?;
    let result =
        block_on("channel-recv", ch.recv())?.map_err(|e| format!("channel-recv: {}", e))?;
    Ok(match result {
        Some(p) => payload_to_value(p, syms),
        None => Value::Symbol(syms.intern("*closed*")),
    })
}

pub fn b_channel_try_recv(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-try-recv", args, 1)?;
    let id = channel_value_to_id(&args[0], syms, "channel-try-recv")?;
    let ch = fetch(id, "channel-try-recv")?;
    match ch.try_recv() {
        Ok(Some(p)) => Ok(payload_to_value(p, syms)),
        Ok(None) => {
            if ch.is_closed() {
                Ok(Value::Symbol(syms.intern("*closed*")))
            } else {
                Ok(Value::Symbol(syms.intern("*empty*")))
            }
        }
        Err(e) => Err(format!("channel-try-recv: {}", e)),
    }
}

pub fn b_channel_close(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-close!", args, 1)?;
    let id = channel_value_to_id(&args[0], syms, "channel-close!")?;
    let ch = fetch(id, "channel-close!")?;
    ch.close();
    Ok(Value::Unspecified)
}

pub fn b_channel_closed_p(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-closed?", args, 1)?;
    let id = channel_value_to_id(&args[0], syms, "channel-closed?")?;
    let ch = fetch(id, "channel-closed?")?;
    Ok(Value::Boolean(ch.is_closed()))
}

pub fn b_channel_len(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-len", args, 1)?;
    let id = channel_value_to_id(&args[0], syms, "channel-len")?;
    let ch = fetch(id, "channel-len")?;
    Ok(Value::Number(Number::Fixnum(ch.len() as i64)))
}

pub fn b_channel_capacity(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("channel-capacity", args, 1)?;
    let id = channel_value_to_id(&args[0], syms, "channel-capacity")?;
    let ch = fetch(id, "channel-capacity")?;
    Ok(match ch.capacity() {
        Some(n) => Value::Number(Number::Fixnum(n as i64)),
        None => Value::Boolean(false),
    })
}

pub fn channel_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    vec![
        ("make-channel", b_make_channel),
        ("channel?", b_channel_p),
        ("channel-send!", b_channel_send),
        ("channel-try-send!", b_channel_try_send),
        ("channel-recv", b_channel_recv),
        ("channel-try-recv", b_channel_try_recv),
        ("channel-close!", b_channel_close),
        ("channel-closed?", b_channel_closed_p),
        ("channel-len", b_channel_len),
        ("channel-capacity", b_channel_capacity),
    ]
}
