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
    make_parameter, Hashtable, HtEqKind, Pair, Parameter, Port, Procedure, Promise, PromiseState,
    StringInputState, Value, WriteMode,
};
