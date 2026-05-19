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
//! (make-channel 0)             ; unbuffered rendezvous
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
use cs_channel::{await_select, try_select};
use cs_channel::{
    BroadcastRecv, BroadcastRegistry, ChannelError, ChannelId, ChannelRegistry, SelectClause,
    SelectKind, SelectOutcome, SubscriptionId,
};
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
            // capacity 0 = unbuffered rendezvous (sender blocks
            // until a receiver pairs up). capacity n>0 = bounded
            // buffer. No-argument = unbounded.
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

/// `(channel-select clauses biased?)` — wait on the first-ready
/// of N clauses. Returns `(<index> . <value>)` where index is
/// the 0-based position in the input list. value is the
/// received payload for recv clauses (`*closed*` if the channel
/// drained), `#t` for send/after/else success, or `*send-closed*`
/// for send clauses that hit a closed channel.
///
/// The Scheme `(select ...)` macro emits this call. Clauses
/// list shape:
///
///   ((recv ch))
///   ((send! ch v))
///   ((after ms))
///   ((else))
pub fn b_channel_select(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 && args.len() != 2 {
        return Err(format!(
            "channel-select: expected 1 or 2 arguments, got {}",
            args.len()
        ));
    }
    let clauses_v = &args[0];
    let biased = match args.get(1) {
        Some(Value::Boolean(b)) => *b,
        Some(other) => {
            return Err(format!(
                "channel-select: biased? must be a boolean, got {}",
                other.type_name()
            ));
        }
        None => false,
    };

    let raw_clauses = collect_list("channel-select", clauses_v)?;
    let mut clauses: Vec<SelectClause> = Vec::with_capacity(raw_clauses.len());
    for (i, c) in raw_clauses.into_iter().enumerate() {
        clauses.push(parse_select_clause(i, &c, syms)?);
    }

    // Pre-pass first — sync, callable from anywhere. Falls
    // through to block_on only if every clause would actually
    // block. That way `(select [(else) …])`-style probing works
    // from the REPL too (no tokio context required).
    let outcome: SelectOutcome = match try_select(clauses, biased) {
        Ok(o) => o,
        Err(remaining) => block_on("channel-select", await_select(remaining, biased))?,
    };

    let value_v = match outcome.kind {
        SelectKind::Recv => match outcome.value {
            Some(p) => payload_to_value(p, syms),
            None => Value::Symbol(syms.intern("*closed*")),
        },
        SelectKind::Send => Value::Boolean(true),
        SelectKind::After => Value::Boolean(true),
        SelectKind::Else => Value::Boolean(true),
        SelectKind::SendClosed => Value::Symbol(syms.intern("*send-closed*")),
    };

    let idx_v = Value::Number(Number::Fixnum(outcome.index as i64));
    Ok(Value::Pair(Pair::new(idx_v, value_v)))
}

/// Walk a Scheme proper list into a Vec<Value>. Errors on
/// non-list shapes (dotted pair, non-pair).
fn collect_list(who: &str, v: &Value) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        // Two-step: pull out the car/cdr clones in their own scope
        // so the Ref<> guards drop before we reassign `cur`.
        let next = match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                let cdr = p.cdr.borrow().clone();
                out.push(car);
                cdr
            }
            other => {
                return Err(format!(
                    "{}: expected proper list, got {}",
                    who,
                    other.type_name()
                ));
            }
        };
        cur = next;
    }
}

/// Parse one clause `(recv ch)` / `(send! ch v)` / `(after ms)`
/// / `(else)` into a SelectClause.
fn parse_select_clause(
    idx: usize,
    v: &Value,
    syms: &mut SymbolTable,
) -> Result<SelectClause, String> {
    let items = collect_list("channel-select clause", v)?;
    if items.is_empty() {
        return Err(format!("channel-select: clause #{} is empty", idx));
    }
    let head_name = match &items[0] {
        Value::Symbol(s) => syms.name(*s).to_string(),
        other => {
            return Err(format!(
                "channel-select: clause #{} head must be a symbol, got {}",
                idx,
                other.type_name()
            ));
        }
    };
    match head_name.as_str() {
        "recv" => {
            if items.len() != 2 {
                return Err(format!(
                    "channel-select: (recv ch) takes 1 arg, clause #{} has {}",
                    idx,
                    items.len() - 1
                ));
            }
            let id = channel_value_to_id(&items[1], syms, "channel-select recv")?;
            let ch = fetch(id, "channel-select recv")?;
            Ok(SelectClause::Recv(ch))
        }
        "send!" => {
            if items.len() != 3 {
                return Err(format!(
                    "channel-select: (send! ch v) takes 2 args, clause #{} has {}",
                    idx,
                    items.len() - 1
                ));
            }
            let id = channel_value_to_id(&items[1], syms, "channel-select send!")?;
            let ch = fetch(id, "channel-select send!")?;
            let payload_sv = to_sendable_in(&items[2], syms)?;
            let payload: Payload = Arc::new(payload_sv);
            Ok(SelectClause::Send(ch, payload))
        }
        "after" => {
            if items.len() != 2 {
                return Err(format!(
                    "channel-select: (after ms) takes 1 arg, clause #{} has {}",
                    idx,
                    items.len() - 1
                ));
            }
            let ms = value_to_i64(&items[1], "channel-select after")?;
            if ms < 0 {
                return Err("channel-select: after-ms must be non-negative".into());
            }
            Ok(SelectClause::After(std::time::Duration::from_millis(
                ms as u64,
            )))
        }
        "else" => {
            if items.len() != 1 {
                return Err(format!(
                    "channel-select: (else) takes no args, clause #{} has {}",
                    idx,
                    items.len() - 1
                ));
            }
            Ok(SelectClause::Else)
        }
        other => Err(format!(
            "channel-select: unknown clause head '{}' in clause #{} \
             (expected recv / send! / after / else)",
            other, idx
        )),
    }
}

// ---------------------------------------------------------------
// Broadcast channels — per interview decision 3, separate API
// from mpsc. (broadcast <id>) and (broadcast-sub <id>) tagged
// pairs are distinct from (channel <id>).
// ---------------------------------------------------------------

pub fn broadcast_registry() -> &'static BroadcastRegistry {
    static R: OnceLock<BroadcastRegistry> = OnceLock::new();
    R.get_or_init(BroadcastRegistry::new)
}

fn make_broadcast_value(id: ChannelId, syms: &mut SymbolTable) -> Value {
    let tag = Value::Symbol(syms.intern("broadcast"));
    let id_v = Value::Number(Number::Fixnum(id.0 as i64));
    Value::Pair(Pair::new(tag, Value::Pair(Pair::new(id_v, Value::Null))))
}

fn make_sub_value(id: SubscriptionId, syms: &mut SymbolTable) -> Value {
    let tag = Value::Symbol(syms.intern("broadcast-sub"));
    let id_v = Value::Number(Number::Fixnum(id.0 as i64));
    Value::Pair(Pair::new(tag, Value::Pair(Pair::new(id_v, Value::Null))))
}

fn value_to_tagged_id(
    v: &Value,
    expected_tag: &str,
    syms: &SymbolTable,
    who: &str,
) -> Result<u64, String> {
    let (head, tail) = match v {
        Value::Pair(p) => (p.car.borrow(), p.cdr.borrow()),
        other => {
            return Err(format!(
                "{}: expected a {} value, got {}",
                who,
                expected_tag,
                other.type_name()
            ));
        }
    };
    match &*head {
        Value::Symbol(s) if syms.name(*s) == expected_tag => {}
        _ => return Err(format!("{}: not a {} value (wrong tag)", who, expected_tag)),
    }
    let (id, rest) = match &*tail {
        Value::Pair(p) => (p.car.borrow(), p.cdr.borrow()),
        _ => {
            return Err(format!(
                "{}: malformed {} value (no id slot)",
                who, expected_tag
            ))
        }
    };
    match (&*id, &*rest) {
        (Value::Number(Number::Fixnum(n)), Value::Null) if *n >= 0 => Ok(*n as u64),
        _ => Err(format!(
            "{}: malformed {} value (bad id)",
            who, expected_tag
        )),
    }
}

pub fn b_make_broadcast_channel(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("make-broadcast-channel", args, 1)?;
    let cap = value_to_i64(&args[0], "make-broadcast-channel")?;
    if cap < 1 {
        return Err(
            "make-broadcast-channel: capacity must be at least 1 (tokio broadcast minimum)".into(),
        );
    }
    let id = broadcast_registry().create(cap as usize);
    Ok(make_broadcast_value(id, syms))
}

pub fn b_broadcast_p(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast?", args, 1)?;
    Ok(Value::Boolean(
        value_to_tagged_id(&args[0], "broadcast", syms, "").is_ok(),
    ))
}

pub fn b_broadcast_sub_p(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-sub?", args, 1)?;
    Ok(Value::Boolean(
        value_to_tagged_id(&args[0], "broadcast-sub", syms, "").is_ok(),
    ))
}

pub fn b_broadcast_subscribe(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-subscribe", args, 1)?;
    let id = ChannelId(value_to_tagged_id(
        &args[0],
        "broadcast",
        syms,
        "broadcast-subscribe",
    )?);
    let sub_id = broadcast_registry()
        .subscribe(id)
        .ok_or_else(|| format!("broadcast-subscribe: channel {} not found or closed", id))?;
    Ok(make_sub_value(sub_id, syms))
}

pub fn b_broadcast_send(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-send!", args, 2)?;
    let id = ChannelId(value_to_tagged_id(
        &args[0],
        "broadcast",
        syms,
        "broadcast-send!",
    )?);
    let ch = broadcast_registry()
        .lookup(id)
        .ok_or_else(|| format!("broadcast-send!: channel {} not found", id))?;
    let payload_sv = to_sendable_in(&args[1], syms)?;
    let payload: Payload = Arc::new(payload_sv);
    let n = ch
        .send(payload)
        .map_err(|e| format!("broadcast-send!: {}", e))?;
    Ok(Value::Number(Number::Fixnum(n as i64)))
}

fn broadcast_recv_to_value(r: BroadcastRecv, syms: &mut SymbolTable) -> Value {
    match r {
        BroadcastRecv::Value(p) => payload_to_value(p, syms),
        BroadcastRecv::Lagged(_) => Value::Symbol(syms.intern("*lagged*")),
        BroadcastRecv::Closed => Value::Symbol(syms.intern("*closed*")),
        BroadcastRecv::Empty => Value::Symbol(syms.intern("*empty*")),
    }
}

pub fn b_broadcast_recv(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-recv", args, 1)?;
    let sub_id = SubscriptionId(value_to_tagged_id(
        &args[0],
        "broadcast-sub",
        syms,
        "broadcast-recv",
    )?);
    let sub = broadcast_registry()
        .lookup_sub(sub_id)
        .ok_or_else(|| format!("broadcast-recv: subscription {} not found", sub_id))?;
    let r = block_on("broadcast-recv", sub.recv())?;
    Ok(broadcast_recv_to_value(r, syms))
}

pub fn b_broadcast_try_recv(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-try-recv", args, 1)?;
    let sub_id = SubscriptionId(value_to_tagged_id(
        &args[0],
        "broadcast-sub",
        syms,
        "broadcast-try-recv",
    )?);
    let sub = broadcast_registry()
        .lookup_sub(sub_id)
        .ok_or_else(|| format!("broadcast-try-recv: subscription {} not found", sub_id))?;
    Ok(broadcast_recv_to_value(sub.try_recv(), syms))
}

pub fn b_broadcast_close(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-close!", args, 1)?;
    let id = ChannelId(value_to_tagged_id(
        &args[0],
        "broadcast",
        syms,
        "broadcast-close!",
    )?);
    let ch = broadcast_registry()
        .lookup(id)
        .ok_or_else(|| format!("broadcast-close!: channel {} not found", id))?;
    ch.close();
    Ok(Value::Unspecified)
}

pub fn b_broadcast_closed_p(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    check_arity("broadcast-closed?", args, 1)?;
    let id = ChannelId(value_to_tagged_id(
        &args[0],
        "broadcast",
        syms,
        "broadcast-closed?",
    )?);
    let ch = broadcast_registry()
        .lookup(id)
        .ok_or_else(|| format!("broadcast-closed?: channel {} not found", id))?;
    Ok(Value::Boolean(ch.is_closed()))
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
        ("channel-select", b_channel_select),
        // Broadcast channels
        ("make-broadcast-channel", b_make_broadcast_channel),
        ("broadcast?", b_broadcast_p),
        ("broadcast-sub?", b_broadcast_sub_p),
        ("broadcast-subscribe", b_broadcast_subscribe),
        ("broadcast-send!", b_broadcast_send),
        ("broadcast-recv", b_broadcast_recv),
        ("broadcast-try-recv", b_broadcast_try_recv),
        ("broadcast-close!", b_broadcast_close),
        ("broadcast-closed?", b_broadcast_closed_p),
    ]
}
