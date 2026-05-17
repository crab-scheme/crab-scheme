//! BEAM-style actor + table + hot-reload primops, exposed to
//! Scheme as builtins. Behind the `actor` feature.
//!
//! See `docs/research/beam_runtime_spec.md`. This module is the
//! glue layer between the three Rust crates (cs-actor / cs-table /
//! cs-hotreload) and the user-facing Scheme surface that
//! `lib/beam/prelude.scm` builds on.
//!
//! ## What's here
//!
//! - [`SendableValue`] — the subset of `cs_core::Value` that can
//!   cross an actor boundary. Procedures, GC-managed cell types,
//!   and ports are *not* representable: per BEAM's copy-on-send
//!   model, every cross-actor value gets deep-cloned into the
//!   receiver's heap. Sharing a `Rc<Slot<T>>` across threads
//!   wouldn't be safe (Rc is `!Send`) and wouldn't match the
//!   spec's "send-copies semantics" choice.
//! - [`to_sendable`] / [`from_sendable`] — the boundary
//!   conversions. Source / sink for the `Payload` type-erased
//!   `Arc<dyn Any + Send + Sync>` used by cs-actor and cs-table.
//! - [`actor_table_primops`] / [`hotreload_primops`] —
//!   higher-order builtin entries to plug into
//!   `pure_builtins()` / `higher_order_builtins()` in `mod.rs`.
//!
//! ## What's *not* here (yet)
//!
//! - `(spawn thunk)` with a real Scheme thunk. The closure
//!   captures `Rc` references and Symbols local to its source
//!   Runtime, so it can't cross to a child actor's Runtime as-is.
//!   The first cut will require the spawned procedure to be a
//!   *top-level name* the child Runtime can resolve by re-load:
//!   `(spawn 'my-mod:my-proc)`. That keeps thunk-transport out
//!   of the boundary and matches BEAM's `spawn(Mod, Fun, Args)`.
//!   Implementation lands when this module joins the dispatch
//!   layer; design is sketched below.
//! - Selective receive at the Rust level — that's done in
//!   `lib/beam/prelude.scm`'s `(receive ...)` macro on top of
//!   the `raw-receive` primop. The Rust side stays a plain
//!   blocking dequeue.

#![cfg(feature = "actor")]

use std::sync::Arc;

use cs_core::{Number, Pair, SymbolTable, Value};

use cs_actor::{ActorPid, Payload};

// ============================================================
// SendableValue — the cross-actor value subset.
// ============================================================

/// A subset of `cs_core::Value` safe to ship across actor
/// boundaries.
///
/// Why a separate type instead of just `Value`?
///
/// 1. `Value` holds `Rc<Slot<T>>` for its GC-managed variants
///    (Pair, Vector, String, Hashtable, ...). `Rc` is `!Send`;
///    sending one across threads is UB.
///
/// 2. The interned `Symbol(u32)` IDs are per-`SymbolTable` and
///    therefore per-actor. Symbol "foo" might be `Symbol(42)` in
///    actor A and `Symbol(99)` in actor B. The boundary has to
///    carry the *name*, not the local ID.
///
/// 3. Procedures (`Rc<dyn Procedure>`) close over the source
///    Runtime's environment. Re-creating them in the receiver is
///    a bigger problem than this struct solves; see the module
///    docs on `(spawn 'name)`.
///
/// The encoding is `Send + Sync + 'static` so it can ride
/// straight inside a `cs_actor::Payload`.
#[derive(Debug, Clone, PartialEq)]
pub enum SendableValue {
    Null,
    Unspecified,
    Eof,
    Boolean(bool),
    Character(char),
    Fixnum(i64),
    Flonum(f64),
    /// Bigints fall through to their decimal-string serialization
    /// for now. We can revisit if profiling shows this is hot.
    BigInt(String),
    String(String),
    /// Symbols cross as their name; the receiver re-interns into
    /// its own SymbolTable on arrival.
    Symbol(String),
    Pair(Box<SendableValue>, Box<SendableValue>),
    Vector(Vec<SendableValue>),
    ByteVector(Vec<u8>),
    /// An actor PID is the canonical sendable handle.
    Pid(ActorPid),
}

/// Convert a Scheme `Value` to its sendable representation. Fails
/// for types that can't safely cross actor boundaries
/// (procedures, ports, promises, hashtables — the latter is
/// representable in principle but the v1 design treats it as a
/// per-actor concept; share state via `cs-table` instead).
pub fn to_sendable(v: &Value) -> Result<SendableValue, String> {
    match v {
        Value::Null => Ok(SendableValue::Null),
        Value::Unspecified => Ok(SendableValue::Unspecified),
        Value::Eof => Ok(SendableValue::Eof),
        Value::Boolean(b) => Ok(SendableValue::Boolean(*b)),
        Value::Character(c) => Ok(SendableValue::Character(*c)),
        Value::Number(n) => num_to_sendable(n),
        Value::String(s) => Ok(SendableValue::String(s.borrow().clone())),
        // Symbol -> string requires a SymbolTable to resolve the
        // u32 to its name; the caller supplies one via the
        // `to_sendable_in` variant below. The bare `to_sendable`
        // can't see one, so it rejects symbols. Callers in this
        // module use the `_in` variant.
        Value::Symbol(_) => {
            Err("to_sendable: symbol requires SymbolTable; use to_sendable_in".into())
        }
        Value::Pair(p) => {
            let head = to_sendable(&p.car.borrow())?;
            let tail = to_sendable(&p.cdr.borrow())?;
            Ok(SendableValue::Pair(Box::new(head), Box::new(tail)))
        }
        Value::Vector(v) => {
            let items: Result<Vec<_>, _> = v.borrow().iter().map(to_sendable).collect();
            Ok(SendableValue::Vector(items?))
        }
        Value::ByteVector(bv) => Ok(SendableValue::ByteVector(bv.borrow().clone())),
        Value::Procedure(_) => Err("to_sendable: procedures cannot cross actor boundaries".into()),
        Value::Hashtable(_) => {
            Err("to_sendable: hashtables are per-actor; use cs-table for shared state".into())
        }
        Value::Port(_) => Err("to_sendable: ports cannot cross actor boundaries".into()),
        Value::Promise(_) => Err("to_sendable: promises cannot cross actor boundaries".into()),
    }
}

/// Like [`to_sendable`] but resolves Symbols against the source
/// SymbolTable so they cross as names. Recursive callers must
/// keep using this variant so nested symbols inside pairs /
/// vectors are also resolved.
pub fn to_sendable_in(v: &Value, syms: &SymbolTable) -> Result<SendableValue, String> {
    match v {
        Value::Symbol(s) => Ok(SendableValue::Symbol(syms.name(*s).to_string())),
        Value::Pair(p) => {
            let head = to_sendable_in(&p.car.borrow(), syms)?;
            let tail = to_sendable_in(&p.cdr.borrow(), syms)?;
            Ok(SendableValue::Pair(Box::new(head), Box::new(tail)))
        }
        Value::Vector(v) => {
            let items: Result<Vec<_>, _> =
                v.borrow().iter().map(|e| to_sendable_in(e, syms)).collect();
            Ok(SendableValue::Vector(items?))
        }
        other => to_sendable(other),
    }
}

/// Rebuild a Scheme `Value` from a sendable representation in
/// the destination actor's environment. Re-interns symbols
/// against the destination SymbolTable.
pub fn from_sendable(s: &SendableValue, syms: &mut SymbolTable) -> Value {
    match s {
        SendableValue::Null => Value::Null,
        SendableValue::Unspecified => Value::Unspecified,
        SendableValue::Eof => Value::Eof,
        SendableValue::Boolean(b) => Value::Boolean(*b),
        SendableValue::Character(c) => Value::Character(*c),
        SendableValue::Fixnum(n) => Value::Number(Number::Fixnum(*n)),
        SendableValue::Flonum(f) => Value::Number(Number::from_f64(*f)),
        SendableValue::BigInt(d) => {
            // Round-trip via the decimal string. Parser failure
            // is impossible: we produced this string in
            // num_to_sendable from a valid bigint.
            Value::Number(Number::parse_decimal_integer(d).expect("bigint round-trip"))
        }
        SendableValue::String(s) => {
            Value::String(cs_core::Gc::new(std::cell::RefCell::new(s.clone())))
        }
        SendableValue::Symbol(name) => Value::Symbol(syms.intern(name)),
        SendableValue::Pair(car, cdr) => {
            let car_v = from_sendable(car, syms);
            let cdr_v = from_sendable(cdr, syms);
            Value::Pair(Pair::new(car_v, cdr_v))
        }
        SendableValue::Vector(items) => {
            let rebuilt: Vec<Value> = items.iter().map(|e| from_sendable(e, syms)).collect();
            Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(rebuilt)))
        }
        SendableValue::ByteVector(bytes) => {
            Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(bytes.clone())))
        }
        SendableValue::Pid(_pid) => {
            // PIDs are represented in Scheme by a symbol of the
            // form `<node.local>`. Once cs-typer gains a real
            // `Pid` type variant we'll replace this with
            // `Value::Pid(pid)`. For now the symbolic form is
            // stable and printable.
            let s = format!("<pid:{}>", _pid);
            Value::Symbol(syms.intern(&s))
        }
    }
}

fn num_to_sendable(n: &Number) -> Result<SendableValue, String> {
    match n {
        Number::Fixnum(i) => Ok(SendableValue::Fixnum(*i)),
        Number::Flonum(f) => Ok(SendableValue::Flonum(*f)),
        Number::Big(b) => Ok(SendableValue::BigInt(b.to_str_radix(10))),
        Number::Rat(_) => Err("to_sendable: rationals not yet supported across actors".into()),
    }
}

// ============================================================
// Payload wrappers.
// ============================================================
//
// cs-actor `Payload` and cs-table value cells are both
// `Arc<dyn Any + Send + Sync>`. We carry a SendableValue inside
// the Arc; downcast at the receiver to pull it back out.

/// Wrap a SendableValue as a cs-actor `Payload`.
pub fn payload_of(s: SendableValue) -> Payload {
    Arc::new(s)
}

/// Unwrap a cs-actor `Payload` back to a SendableValue.
/// Returns `None` if the payload was produced by some other code
/// path with a different inner type (e.g., a Rust-side actor
/// embedding raw `String` payloads in a test).
pub fn payload_to_sendable(p: &Payload) -> Option<SendableValue> {
    p.downcast_ref::<SendableValue>().cloned()
}

// ============================================================
// Cross-actor key conversion for cs-table.
// ============================================================
//
// cs-table's `Key` enum is intentionally narrow (Fixnum / String
// / Bytes). Map a SendableValue → Key for the small set of
// values that have sensible Hash + Ord; reject everything else.

pub fn key_of(s: &SendableValue) -> Result<cs_table::Key, String> {
    match s {
        SendableValue::Fixnum(i) => Ok(cs_table::Key::Fixnum(*i)),
        SendableValue::String(s) => Ok(cs_table::Key::String(s.clone())),
        SendableValue::Symbol(name) => Ok(cs_table::Key::String(name.clone())),
        SendableValue::ByteVector(b) => Ok(cs_table::Key::Bytes(b.clone())),
        other => Err(format!(
            "cs-table key must be fixnum / string / symbol / bytevector; got {:?}",
            other
        )),
    }
}

// ============================================================
// Tests.
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_atoms() {
        let mut syms = SymbolTable::new();

        let cases = [
            Value::Null,
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Character('λ'),
            Value::Number(Number::Fixnum(42)),
            Value::Number(Number::Flonum(3.14)),
            Value::Unspecified,
            Value::Eof,
        ];

        for v in &cases {
            let s = to_sendable_in(v, &syms).expect("encode");
            let back = from_sendable(&s, &mut syms);
            // Round-trip via the SendableValue projection:
            // re-encoding the rebuilt Value should hit the same
            // SendableValue (Value's PartialEq for Gc<RefCell<_>>
            // compares pointers, which won't match across rebuilds).
            let s2 = to_sendable_in(&back, &syms).expect("re-encode");
            assert_eq!(s, s2, "round-trip mismatch for {:?}", v);
        }
    }

    #[test]
    fn round_trip_string() {
        let mut syms = SymbolTable::new();
        let v = Value::String(cs_core::Gc::new(std::cell::RefCell::new("hello".into())));
        let s = to_sendable_in(&v, &syms).expect("encode");
        assert_eq!(s, SendableValue::String("hello".into()));
        let back = from_sendable(&s, &mut syms);
        if let Value::String(g) = &back {
            assert_eq!(*g.borrow(), "hello");
        } else {
            panic!("expected String");
        }
    }

    #[test]
    fn round_trip_symbol_reinterns() {
        // Symbol IDs are per-table. Sending "foo" across a
        // boundary should resolve to the destination's `foo`
        // symbol, NOT the source's u32.
        let mut src = SymbolTable::new();
        // Force the destination to allocate other symbols first
        // so its `foo` ends up at a different u32.
        let mut dst = SymbolTable::new();
        dst.intern("a");
        dst.intern("b");
        dst.intern("c");

        let src_foo = src.intern("foo");
        let v = Value::Symbol(src_foo);

        let s = to_sendable_in(&v, &src).expect("encode");
        assert_eq!(s, SendableValue::Symbol("foo".into()));

        let back = from_sendable(&s, &mut dst);
        if let Value::Symbol(dst_foo) = back {
            assert_eq!(dst.name(dst_foo), "foo");
            assert_ne!(src_foo.0, dst_foo.0, "interning offsets should differ");
        } else {
            panic!("expected Symbol");
        }
    }

    #[test]
    fn round_trip_pair_and_list() {
        let mut syms = SymbolTable::new();

        // (1 2 3) as a proper list = (1 . (2 . (3 . ())))
        let a = Pair::new(Value::Number(Number::Fixnum(3)), Value::Null);
        let b = Pair::new(Value::Number(Number::Fixnum(2)), Value::Pair(a));
        let lst = Value::Pair(Pair::new(Value::Number(Number::Fixnum(1)), Value::Pair(b)));

        let s = to_sendable_in(&lst, &syms).expect("encode");
        let back = from_sendable(&s, &mut syms);
        let s2 = to_sendable_in(&back, &syms).expect("re-encode");
        assert_eq!(s, s2);
    }

    #[test]
    fn round_trip_nested_with_symbols() {
        let mut src = SymbolTable::new();
        let mut dst = SymbolTable::new();

        let hello = src.intern("hello");
        let world = src.intern("world");
        // (hello . world)
        let v = Value::Pair(Pair::new(Value::Symbol(hello), Value::Symbol(world)));

        let s = to_sendable_in(&v, &src).expect("encode");
        let back = from_sendable(&s, &mut dst);
        let s2 = to_sendable_in(&back, &dst).expect("re-encode");
        assert_eq!(s, s2);
    }

    #[test]
    fn round_trip_vector() {
        let mut syms = SymbolTable::new();
        let v = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(vec![
            Value::Number(Number::Fixnum(1)),
            Value::Boolean(true),
            Value::Null,
        ])));
        let s = to_sendable_in(&v, &syms).expect("encode");
        let back = from_sendable(&s, &mut syms);
        let s2 = to_sendable_in(&back, &syms).expect("re-encode");
        assert_eq!(s, s2);
    }

    #[test]
    fn reject_unsendable_types() {
        let syms = SymbolTable::new();

        // Hashtables are per-actor in this design; share state via
        // cs-table instead. The encoder must reject them.
        use cs_core::{Hashtable, HtEqKind};
        let ht = Value::Hashtable(Hashtable::new(HtEqKind::Eq));
        match to_sendable_in(&ht, &syms) {
            Err(msg) => assert!(msg.contains("hashtable")),
            Ok(_) => panic!("hashtable should not be sendable"),
        }
    }

    #[test]
    fn payload_round_trip() {
        let mut syms = SymbolTable::new();
        let v = Value::Pair(Pair::new(
            Value::Number(Number::Fixnum(7)),
            Value::Boolean(false),
        ));
        let s = to_sendable_in(&v, &syms).expect("encode");
        let p = payload_of(s.clone());
        let s2 = payload_to_sendable(&p).expect("downcast");
        assert_eq!(s, s2);

        let back = from_sendable(&s2, &mut syms);
        let s3 = to_sendable_in(&back, &syms).expect("re-encode");
        assert_eq!(s, s3);
    }

    #[test]
    fn cs_table_key_conversion() {
        assert!(matches!(
            key_of(&SendableValue::Fixnum(7)),
            Ok(cs_table::Key::Fixnum(7))
        ));
        assert!(matches!(
            key_of(&SendableValue::String("k".into())),
            Ok(cs_table::Key::String(_))
        ));
        assert!(matches!(
            key_of(&SendableValue::Symbol("s".into())),
            Ok(cs_table::Key::String(_))
        ));
        assert!(matches!(
            key_of(&SendableValue::ByteVector(vec![1, 2, 3])),
            Ok(cs_table::Key::Bytes(_))
        ));
        // Booleans aren't sensible Hash + Ord keys for ETS.
        assert!(key_of(&SendableValue::Boolean(true)).is_err());
    }

    #[test]
    fn pid_round_trips_via_symbol() {
        // PIDs aren't a Value variant yet (waits on cs-typer's
        // Pid type). The interim encoding rebuilds them as a
        // printable symbol; verify the projection is stable.
        let mut syms = SymbolTable::new();
        let pid = ActorPid {
            node: 0,
            local_id: 17,
        };
        let s = SendableValue::Pid(pid);
        let v = from_sendable(&s, &mut syms);
        if let Value::Symbol(sym) = v {
            assert_eq!(syms.name(sym), "<pid:<0.17>>");
        } else {
            panic!("expected Symbol for PID");
        }
    }
}
