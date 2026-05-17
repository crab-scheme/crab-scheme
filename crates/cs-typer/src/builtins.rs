//! Built-in primop type signatures.
//!
//! Maps the Scheme names of common primitives to their `ProcType`
//! signatures, so the bidirectional checker (iters 2.3-2.5) can
//! treat `(car xs)` as a typed application without the user having
//! to write a `:` ascription for it.
//!
//! **Scope (Phase 2).** Monomorphic signatures only — the table
//! lists what each primop accepts in the *simplest* atomic case.
//! `+` is `Fixnum → Fixnum → Fixnum` here, not `Number → … →
//! Number`, because Phase 2 doesn't have unions or a `Number`
//! supertype yet. Phase 3 widens these with `(U Fixnum Flonum)`
//! and primop overloads.
//!
//! **Scope (Phase 2, variadics).** `+`, `*`, `cons*`, etc. are
//! variadic; right now we type them as 2-ary (the common arity
//! the checker actually sees in typed user code). The checker
//! falls back to `Any` when the arity doesn't match the table
//! (iter 2.7), so calls with extra args still typecheck loosely.
//! Phase 3 introduces a proper `rest` field on `ProcType`.
//!
//! **Not in the table.** Anything missing falls through to `Any`
//! at lookup time (untyped fallback) — including I/O,
//! hashtables, records, conditions, the bulk of bytevector ops,
//! and most port operations. Typed code that needs those gets
//! `Any` for them; that's gradual typing's default.

use cs_core::{Symbol, SymbolTable};

use crate::env::TypeEnv;
use crate::types::{ProcType, Type};

/// Build the (name, ProcType) table. Allocated fresh on each call —
/// the table is small (~80 entries) and only consulted once at the
/// start of a check run.
pub fn primop_table() -> Vec<(&'static str, ProcType)> {
    let fx = || Type::Fixnum;
    let fl = || Type::Flonum;
    let bool_ = || Type::Boolean;
    let ch = || Type::Character;
    let sym = || Type::Symbol;
    let pair = || Type::Pair;
    let vec_ = || Type::Vector;
    let str_ = || Type::String;
    let bv = || Type::ByteVector;
    let proc_ = || Type::Procedure;
    let null = || Type::Null;
    let any = || Type::Any;
    // Phase 3: `Number = (U Fixnum Flonum)` — the shared
    // numeric union generic arithmetic accepts. Rationals and
    // BigInts are still subsumed by Fixnum for Phase 2/3; they
    // get their own atoms when the lattice grows.
    let num = || Type::union(vec![Type::Fixnum, Type::Flonum]);

    // Builder shortcuts.
    let p2 = |a, b, r| ProcType {
        params: vec![a, b],
        return_type: r,
        rest: None,
        filter: None,
    };
    let p1 = |a, r| ProcType {
        params: vec![a],
        return_type: r,
        rest: None,
        filter: None,
    };
    let p0 = |r| ProcType {
        params: vec![],
        return_type: r,
        rest: None,
        filter: None,
    };
    let p3 = |a, b, c, r| ProcType {
        params: vec![a, b, c],
        return_type: r,
        rest: None,
        filter: None,
    };
    // Predicate builder: takes a positive proposition (the
    // filter) and returns a `(-> Any Boolean)` ProcType with
    // `filter: Some(positive)` populated. Used for all
    // type-predicate primops below.
    let pred = |positive: Type| ProcType {
        params: vec![Type::Any],
        return_type: Type::Boolean,
        rest: None,
        filter: Some(positive),
    };

    vec![
        // ---- generic arithmetic — Phase 3 widening ----
        // These accept either Fixnum OR Flonum and (for
        // arithmetic) return the union. Code that needs
        // Fixnum-only precision should use fx+/fx-/etc.;
        // Flonum-only should use fl+/fl-/etc.
        ("+", p2(num(), num(), num())),
        ("-", p2(num(), num(), num())),
        ("*", p2(num(), num(), num())),
        ("/", p2(num(), num(), num())),
        ("=", p2(num(), num(), bool_())),
        ("<", p2(num(), num(), bool_())),
        (">", p2(num(), num(), bool_())),
        ("<=", p2(num(), num(), bool_())),
        (">=", p2(num(), num(), bool_())),
        ("zero?", p1(num(), bool_())),
        ("positive?", p1(num(), bool_())),
        ("negative?", p1(num(), bool_())),
        ("odd?", p1(num(), bool_())),
        ("even?", p1(num(), bool_())),
        ("abs", p1(num(), num())),
        ("min", p2(num(), num(), num())),
        ("max", p2(num(), num(), num())),
        // Division-family: modulo/quotient/remainder/expt are
        // R6RS-integer-typed. Kept Fixnum-narrow because they
        // don't accept Flonum operands.
        ("modulo", p2(fx(), fx(), fx())),
        ("quotient", p2(fx(), fx(), fx())),
        ("remainder", p2(fx(), fx(), fx())),
        ("expt", p2(num(), num(), num())),
        ("square", p1(num(), num())),
        // ---- R6RS fixnum-only ops ----
        ("fx+", p2(fx(), fx(), fx())),
        ("fx-", p2(fx(), fx(), fx())),
        ("fx*", p2(fx(), fx(), fx())),
        ("fxdiv", p2(fx(), fx(), fx())),
        ("fxmod", p2(fx(), fx(), fx())),
        ("fx=?", p2(fx(), fx(), bool_())),
        ("fx<?", p2(fx(), fx(), bool_())),
        ("fx>?", p2(fx(), fx(), bool_())),
        ("fx<=?", p2(fx(), fx(), bool_())),
        ("fx>=?", p2(fx(), fx(), bool_())),
        // ---- R6RS flonum-only ops ----
        ("fl+", p2(fl(), fl(), fl())),
        ("fl-", p2(fl(), fl(), fl())),
        ("fl*", p2(fl(), fl(), fl())),
        ("fl/", p2(fl(), fl(), fl())),
        ("fl=?", p2(fl(), fl(), bool_())),
        ("fl<?", p2(fl(), fl(), bool_())),
        ("fl>?", p2(fl(), fl(), bool_())),
        ("fl<=?", p2(fl(), fl(), bool_())),
        ("fl>=?", p2(fl(), fl(), bool_())),
        ("flsqrt", p1(fl(), fl())),
        ("flabs", p1(fl(), fl())),
        ("fixnum->flonum", p1(fx(), fl())),
        // ---- type predicates (Phase 4 occurrence typing) ----
        // Each gets a `filter` field carrying the positive
        // proposition. In `(if (string? x) …)` the then-branch
        // sees `x` narrowed to `String`; the else-branch sees
        // the difference.
        ("fixnum?", pred(fx())),
        ("flonum?", pred(fl())),
        ("number?", pred(num())),
        ("integer?", pred(fx())),
        ("boolean?", pred(bool_())),
        ("pair?", pred(pair())),
        ("null?", pred(null())),
        // `list?` accepts non-pair non-null inputs too (proper
        // list check), so its filter is `(U Pair Null)`.
        ("list?", pred(Type::union(vec![pair(), null()]))),
        ("symbol?", pred(sym())),
        ("string?", pred(str_())),
        ("char?", pred(ch())),
        ("vector?", pred(vec_())),
        ("procedure?", pred(proc_())),
        ("bytevector?", pred(bv())),
        // ---- pairs / lists ----
        // Variadic list / vector constructors — Phase 3.4
        // exposes `rest` in ProcType so these get proper
        // signatures: any number of Any-typed args, returns a
        // homogeneous Listof Any / Vector. The `rest: Some(any())`
        // means "zero or more trailing Any args."
        (
            "list",
            ProcType {
                params: vec![],
                return_type: Type::Listof(Box::new(Type::Any)),
                rest: Some(Type::Any),
                filter: None,
            },
        ),
        (
            "vector",
            ProcType {
                params: vec![],
                return_type: vec_(),
                rest: Some(Type::Any),
                filter: None,
            },
        ),
        ("cons", p2(any(), any(), pair())),
        ("car", p1(pair(), any())),
        ("cdr", p1(pair(), any())),
        ("set-car!", p2(pair(), any(), any())),
        ("set-cdr!", p2(pair(), any(), any())),
        ("length", p1(pair(), fx())),
        ("reverse", p1(pair(), pair())),
        ("list-ref", p2(pair(), fx(), any())),
        ("list-tail", p2(pair(), fx(), pair())),
        ("null", p0(null())),
        // ---- equality ----
        ("eq?", p2(any(), any(), bool_())),
        ("eqv?", p2(any(), any(), bool_())),
        ("equal?", p2(any(), any(), bool_())),
        ("not", p1(any(), bool_())),
        // ---- strings ----
        ("string-length", p1(str_(), fx())),
        ("string-ref", p2(str_(), fx(), ch())),
        ("string=?", p2(str_(), str_(), bool_())),
        ("string<?", p2(str_(), str_(), bool_())),
        ("string>?", p2(str_(), str_(), bool_())),
        ("string-append", p2(str_(), str_(), str_())),
        ("substring", p3(str_(), fx(), fx(), str_())),
        ("string->symbol", p1(str_(), sym())),
        ("symbol->string", p1(sym(), str_())),
        ("number->string", p1(fx(), str_())),
        ("string->number", p1(str_(), fx())),
        ("make-string", p2(fx(), ch(), str_())),
        // ---- characters ----
        ("char=?", p2(ch(), ch(), bool_())),
        ("char<?", p2(ch(), ch(), bool_())),
        ("char>?", p2(ch(), ch(), bool_())),
        ("char->integer", p1(ch(), fx())),
        ("integer->char", p1(fx(), ch())),
        // ---- vectors ----
        ("vector-length", p1(vec_(), fx())),
        ("vector-ref", p2(vec_(), fx(), any())),
        ("vector-set!", p3(vec_(), fx(), any(), any())),
        ("make-vector", p2(fx(), any(), vec_())),
        // ---- bytevectors ----
        ("bytevector-length", p1(bv(), fx())),
        ("bytevector-u8-ref", p2(bv(), fx(), fx())),
        ("bytevector-u8-set!", p3(bv(), fx(), fx(), any())),
        ("make-bytevector", p2(fx(), fx(), bv())),
        // ---- procedure introspection ----
        ("apply", p2(proc_(), pair(), any())),
        // ---- I/O (returns are usually Any/unspecified) ----
        ("display", p1(any(), any())),
        ("write", p1(any(), any())),
        ("newline", p0(any())),
        // ---- iter 6.6 stdlib annotations ----
        // Numeric: rounding + conversions + bitwise.
        ("floor", p1(num(), num())),
        ("ceiling", p1(num(), num())),
        ("round", p1(num(), num())),
        ("truncate", p1(num(), num())),
        ("exact", p1(num(), num())),
        ("inexact", p1(num(), num())),
        ("exact?", pred(fx())),
        ("inexact?", pred(fl())),
        ("exact->inexact", p1(num(), fl())),
        ("inexact->exact", p1(num(), fx())),
        ("bitwise-and", p2(fx(), fx(), fx())),
        ("bitwise-or", p2(fx(), fx(), fx())),
        ("bitwise-xor", p2(fx(), fx(), fx())),
        ("bitwise-not", p1(fx(), fx())),
        // Character classification (predicates) and case.
        ("char-alphabetic?", pred(ch())),
        ("char-numeric?", pred(ch())),
        ("char-whitespace?", pred(ch())),
        ("char-upper-case?", pred(ch())),
        ("char-lower-case?", pred(ch())),
        ("char-upcase", p1(ch(), ch())),
        ("char-downcase", p1(ch(), ch())),
        ("char-foldcase", p1(ch(), ch())),
        ("char<=?", p2(ch(), ch(), bool_())),
        ("char>=?", p2(ch(), ch(), bool_())),
        // String operations.
        ("string<=?", p2(str_(), str_(), bool_())),
        ("string>=?", p2(str_(), str_(), bool_())),
        ("string-upcase", p1(str_(), str_())),
        ("string-downcase", p1(str_(), str_())),
        ("string-foldcase", p1(str_(), str_())),
        ("string-titlecase", p1(str_(), str_())),
        ("string-prefix?", p2(str_(), str_(), bool_())),
        ("string-suffix?", p2(str_(), str_(), bool_())),
        (
            "string-contains",
            p2(str_(), str_(), Type::union(vec![fx(), bool_()])),
        ),
        ("string-copy", p1(str_(), str_())),
        ("string->list", p1(str_(), Type::Listof(Box::new(ch())))),
        ("list->string", p1(pair(), str_())),
        // List operations beyond Phase 2's `cons/car/cdr/length`.
        ("append", p2(pair(), pair(), pair())),
        ("list-copy", p1(pair(), pair())),
        (
            "memq",
            p2(any(), pair(), Type::union(vec![pair(), bool_()])),
        ),
        (
            "memv",
            p2(any(), pair(), Type::union(vec![pair(), bool_()])),
        ),
        (
            "assq",
            p2(any(), pair(), Type::union(vec![pair(), bool_()])),
        ),
        (
            "assv",
            p2(any(), pair(), Type::union(vec![pair(), bool_()])),
        ),
        ("iota", p1(fx(), Type::Listof(Box::new(fx())))),
        ("first", p1(pair(), any())),
        ("second", p1(pair(), any())),
        ("third", p1(pair(), any())),
        ("last", p1(pair(), any())),
        // Vector operations beyond Phase 2.
        ("vector-fill!", p2(vec_(), any(), any())),
        ("vector-copy", p1(vec_(), vec_())),
        ("vector-append", p2(vec_(), vec_(), vec_())),
        ("vector->list", p1(vec_(), Type::Listof(Box::new(any())))),
        ("list->vector", p1(pair(), vec_())),
        // Hashtables.
        ("hashtable?", pred(any())),
        ("hashtable-size", p1(any(), fx())),
        ("hashtable-keys", p1(any(), vec_())),
        ("hashtable-values", p1(any(), vec_())),
        ("make-eq-hashtable", p0(any())),
        ("make-eqv-hashtable", p0(any())),
        // EOF/port predicates.
        ("eof-object?", pred(any())),
        ("port?", pred(any())),
        ("input-port?", pred(any())),
        ("output-port?", pred(any())),
    ]
}

/// Install the primop table into `env` at the top-level frame.
/// Intended to be called once after `TypeEnv::new()`, before any
/// scopes are pushed.
pub fn install_primops(env: &mut TypeEnv, syms: &mut SymbolTable) {
    for (name, pt) in primop_table() {
        let sym = syms.intern(name);
        let ty = Type::Procedure_(Box::new(pt));
        env.define_top_level(sym, ty);
    }
}

/// Just the (Symbol, Type) seeding step — exposed for tests that
/// want to build their own env without the install side effect.
pub fn primop_pairs(syms: &mut SymbolTable) -> Vec<(Symbol, Type)> {
    primop_table()
        .into_iter()
        .map(|(name, pt)| {
            let s = syms.intern(name);
            (s, Type::Procedure_(Box::new(pt)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_nontrivial_size() {
        // Lower-bound only — sanity check the table got
        // populated, not a brittle exact count. Iter 6.6 grew
        // this to ~140; we keep the bound conservative so
        // future trims don't break the test.
        let t = primop_table();
        assert!(t.len() >= 130, "expected ≥130 primops, got {}", t.len());
    }

    #[test]
    fn no_duplicate_names() {
        let t = primop_table();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (name, _) in &t {
            assert!(
                seen.insert(name),
                "duplicate primop name in table: {}",
                name
            );
        }
    }

    #[test]
    fn install_seeds_env() {
        let mut syms = SymbolTable::new();
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        // A representative cross-section.
        for name in [
            "+",
            "car",
            "cdr",
            "fx+",
            "fl+",
            "string-length",
            "vector-length",
        ] {
            let s = syms.intern(name);
            assert!(env.lookup(s).is_some(), "{name} not seeded into env");
        }
    }

    #[test]
    fn plus_signature_is_number_number_number() {
        // Phase 3: `+` widened from `(-> Fx Fx Fx)` to
        // `(-> (U Fx Fl) (U Fx Fl) (U Fx Fl))`. Code that
        // needs Fixnum-only precision should switch to `fx+`.
        let mut syms = SymbolTable::new();
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        let plus = syms.intern("+");
        let ty = env.lookup(plus).cloned().unwrap();
        let num = Type::union(vec![Type::Fixnum, Type::Flonum]);
        match ty {
            Type::Procedure_(pt) => {
                assert_eq!(pt.params, vec![num.clone(), num.clone()]);
                assert_eq!(pt.return_type, num);
                assert!(pt.rest.is_none());
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn car_signature_is_pair_to_any() {
        let mut syms = SymbolTable::new();
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        let car = syms.intern("car");
        let ty = env.lookup(car).cloned().unwrap();
        match ty {
            Type::Procedure_(pt) => {
                assert_eq!(pt.params, vec![Type::Pair]);
                assert_eq!(pt.return_type, Type::Any);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }

    #[test]
    fn string_length_signature() {
        let mut syms = SymbolTable::new();
        let mut env = TypeEnv::new();
        install_primops(&mut env, &mut syms);
        let sl = syms.intern("string-length");
        let ty = env.lookup(sl).cloned().unwrap();
        match ty {
            Type::Procedure_(pt) => {
                assert_eq!(pt.params, vec![Type::String]);
                assert_eq!(pt.return_type, Type::Fixnum);
            }
            other => panic!("expected Procedure_, got {other:?}"),
        }
    }
}
