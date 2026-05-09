//! Core value types and numeric tower for CrabScheme.
//!
//! This crate defines:
//! - [`Value`]: the universal Scheme value type
//! - [`Symbol`] / [`SymbolTable`]: per-Runtime interned symbols
//! - [`Number`]: the numeric tower (Fixnum, BigInt, Rational, Flonum)
//! - Equality predicates [`eq::eq`], [`eq::eqv`], [`eq::equal`]

pub mod eq;
pub mod number;
pub mod symbol;
pub mod value;

pub use number::{NumError, Number};
pub use symbol::{Symbol, SymbolTable};
pub use value::{
    make_parameter, FileOutputState, Hashtable, HtEqKind, Pair, Parameter, Port, Procedure,
    Promise, PromiseState, StringInputState, Value, WriteMode,
};

thread_local! {
    /// One-element cache of the value most recently passed to a builtin's
    /// `type_err`-style helper. The walker and VM dispatchers drain it
    /// when wrapping a builtin's `Err(String)` into a raised condition,
    /// attaching the value as an `&irritants` simple. This means
    /// `(condition-irritants c)` returns the actual offending value
    /// instead of just embedding its type name in the message.
    ///
    /// Lives in cs-core so both cs-runtime (walker) and cs-vm can share
    /// the same thread-local without a circular dependency.
    static BUILTIN_ERR_IRRITANT: std::cell::RefCell<Option<Value>> =
        const { std::cell::RefCell::new(None) };
}

/// Stash an offending value to be attached as an `&irritants` simple
/// when the next builtin `Err(String)` is converted into a condition.
/// Idempotent — overwrites any previous unread value (which is exactly
/// what we want, since stale state from an unrelated path shouldn't
/// leak into a later error).
pub fn stash_builtin_err_irritant(v: Value) {
    BUILTIN_ERR_IRRITANT.with(|c| *c.borrow_mut() = Some(v));
}

/// Drain the thread-local irritant cache. Returns `Vec::new()` when no
/// builtin had stashed a value.
pub fn take_builtin_err_irritant() -> Vec<Value> {
    BUILTIN_ERR_IRRITANT.with(|c| c.borrow_mut().take().into_iter().collect())
}
