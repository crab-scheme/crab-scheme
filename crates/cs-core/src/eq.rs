//! R6RS equality predicates: eq?, eqv?, equal?
//!
//! `equal?` handles cyclic structures via a visited-set fallback.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[cfg_attr(not(test), allow(unused_imports))]
use crate::value::{HtEqKind, Pair, Value};
use crate::Number;

/// R6RS `eq?`: identity for heap values, value equality for immediates.
pub fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Unspecified, Value::Unspecified) => true,
        (Value::Eof, Value::Eof) => true,
        (Value::Boolean(x), Value::Boolean(y)) => x == y,
        (Value::Character(x), Value::Character(y)) => x == y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        // Identifiers compare by (name, mark) pair. Two
        // identifiers with the same name but different marks
        // (e.g., from two distinct macro-expansion sites)
        // compare unequal -- this is the foundation of R6RS
        // bound-identifier=?. Identifier vs Symbol mixed is
        // false (the kinds are distinct types in R6RS).
        (Value::Identifier { name: n1, mark: m1 }, Value::Identifier { name: n2, mark: m2 }) => {
            n1 == n2 && m1 == m2
        }
        // eq? on numbers is only true for two equal Fixnums; every
        // other numeric shape (Flonum/Big/Rational) is eq?-false and
        // falls through to the outer `_ => false`.
        (Value::Fixnum(a), Value::Fixnum(b)) => a == b,
        (Value::String(x), Value::String(y)) => crate::Gc::ptr_eq(x, y),
        (Value::Pair(x), Value::Pair(y)) => crate::Gc::ptr_eq(x, y),
        (Value::Vector(x), Value::Vector(y)) => crate::Gc::ptr_eq(x, y),
        (Value::ByteVector(x), Value::ByteVector(y)) => crate::Gc::ptr_eq(x, y),
        (Value::Procedure(x), Value::Procedure(y)) => Rc::ptr_eq(x, y),
        (Value::Hashtable(x), Value::Hashtable(y)) => crate::Gc::ptr_eq(x, y),
        (Value::Port(x), Value::Port(y)) => crate::Gc::ptr_eq(x, y),
        (Value::Promise(x), Value::Promise(y)) => crate::Gc::ptr_eq(x, y),
        _ => false,
    }
}

/// R6RS `eqv?`: like `eq?` but compares numbers and characters by value.
pub fn eqv(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        return x.is_exact() == y.is_exact() && x.eq_value(&y);
    }
    eq(a, b)
}

/// R6RS `equal?`: structural equality. Handles cycles.
pub fn equal(a: &Value, b: &Value) -> bool {
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    equal_rec(a, b, &mut visited, 64)
}

fn equal_rec(a: &Value, b: &Value, visited: &mut HashSet<(usize, usize)>, fuel: u32) -> bool {
    if eqv(a, b) {
        return true;
    }
    match (a, b) {
        (Value::String(x), Value::String(y)) => *x.borrow() == *y.borrow(),
        (Value::Pair(x), Value::Pair(y)) => {
            let key = (crate::Gc::as_addr(x), crate::Gc::as_addr(y));
            if fuel == 0 {
                if !visited.insert(key) {
                    return true;
                }
            }
            let next_fuel = fuel.saturating_sub(1);
            equal_rec(&x.car.borrow(), &y.car.borrow(), visited, next_fuel)
                && equal_rec(&x.cdr.borrow(), &y.cdr.borrow(), visited, next_fuel)
        }
        (Value::Vector(x), Value::Vector(y)) => {
            let xb = x.borrow();
            let yb = y.borrow();
            if xb.len() != yb.len() {
                return false;
            }
            let next_fuel = fuel.saturating_sub(1);
            xb.iter()
                .zip(yb.iter())
                .all(|(a, b)| equal_rec(a, b, visited, next_fuel))
        }
        (Value::ByteVector(x), Value::ByteVector(y)) => *x.borrow() == *y.borrow(),
        _ => false,
    }
}

/// Hash a value consistently with the given equivalence kind: two values
/// that compare equal under `kind` (via [`eq`]/[`eqv`]/[`equal`]) MUST
/// hash to the same bucket. Hash collisions are always resolved by the
/// real comparator, so this only needs to be a good-enough distributor,
/// never perfectly injective. `HtEqKind::Custom` has no fixed hash here —
/// callers with a user-supplied hash procedure and an invocation context
/// compute it themselves and never reach this function.
pub fn hash_value(v: &Value, kind: HtEqKind) -> u64 {
    match kind {
        HtEqKind::Eq => hash_eq(v),
        HtEqKind::Eqv => hash_eqv(v),
        HtEqKind::Equal => hash_equal(v, 16),
        HtEqKind::Custom => unreachable!("custom hashing routes through the user hash proc"),
    }
}

fn hash_eq(v: &Value) -> u64 {
    match v {
        Value::Null => 0x1,
        Value::Unspecified => 0x2,
        Value::Eof => 0x3,
        Value::Boolean(b) => {
            if *b {
                0x11
            } else {
                0x10
            }
        }
        Value::Character(c) => 0x100u64.wrapping_add(*c as u64),
        Value::Symbol(s) => 0x200u64.wrapping_add(s.0 as u64),
        // Two identifiers are only `eq?` when both name and mark match
        // (see `eq` above), so both must feed the hash.
        Value::Identifier { name, mark } => 0x300u64
            .wrapping_add(name.0 as u64)
            .wrapping_add(mark.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        // `eq` only ever returns true for two Fixnums; every other number
        // shape is `eq?`-false against everything (see `eq` above), so any
        // fixed bucket for them is safe — they'll never match in-bucket.
        Value::Fixnum(n) => 0x400u64.wrapping_add(*n as u64),
        Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_) => 0x4FF,
        Value::String(g) => 0x500u64.wrapping_add(crate::Gc::as_addr(g) as u64),
        Value::Pair(g) => 0x600u64.wrapping_add(crate::Gc::as_addr(g) as u64),
        Value::Vector(g) => 0x700u64.wrapping_add(crate::Gc::as_addr(g) as u64),
        Value::ByteVector(g) => 0x800u64.wrapping_add(crate::Gc::as_addr(g) as u64),
        Value::Procedure(p) => 0x900u64.wrapping_add(Rc::as_ptr(p) as *const () as usize as u64),
        Value::Hashtable(g) => 0xA00u64.wrapping_add(crate::Gc::as_addr(g) as u64),
        Value::Port(g) => 0xB00u64.wrapping_add(crate::Gc::as_addr(g) as u64),
        Value::Promise(g) => 0xC00u64.wrapping_add(crate::Gc::as_addr(g) as u64),
    }
}

fn hash_eqv(v: &Value) -> u64 {
    if let Some(n) = v.as_number() {
        return hash_number(&n);
    }
    hash_eq(v)
}

/// Hashes a number respecting exactness: an exact and inexact value that
/// happen to denote the same magnitude (e.g. `2` vs `2.0`) are NOT
/// `eqv?`-equal, so they're tagged into separate hash spaces.
fn hash_number(n: &Number) -> u64 {
    let mut h = DefaultHasher::new();
    match n {
        Number::Fixnum(i) => {
            0u8.hash(&mut h);
            i.hash(&mut h);
        }
        Number::Flonum(f) => {
            1u8.hash(&mut h);
            f.to_bits().hash(&mut h);
        }
        // Big/Rat are kept in canonical (reduced) form by their
        // libraries, so their Display output is a stable digest.
        Number::Big(b) => {
            2u8.hash(&mut h);
            b.to_string().hash(&mut h);
        }
        Number::Rat(r) => {
            3u8.hash(&mut h);
            r.to_string().hash(&mut h);
        }
    }
    h.finish()
}

/// Structural hash mirroring `equal_rec`, with a depth cap so deep or
/// cyclic structures degrade to bucket collisions (resolved by the real
/// `equal?` comparator) instead of looping or blowing the stack.
fn hash_equal(v: &Value, depth: u32) -> u64 {
    if depth == 0 {
        return 0xD; // depth-exhausted: collapse into one shared bucket.
    }
    match v {
        Value::String(s) => {
            let mut h = DefaultHasher::new();
            10u8.hash(&mut h);
            s.borrow().hash(&mut h);
            h.finish()
        }
        Value::Pair(p) => {
            let mut h = DefaultHasher::new();
            11u8.hash(&mut h);
            hash_equal(&p.car(), depth - 1).hash(&mut h);
            hash_equal(&p.cdr(), depth - 1).hash(&mut h);
            h.finish()
        }
        Value::Vector(vec) => {
            let b = vec.borrow();
            let mut h = DefaultHasher::new();
            12u8.hash(&mut h);
            b.len().hash(&mut h);
            for item in b.iter() {
                hash_equal(item, depth - 1).hash(&mut h);
            }
            h.finish()
        }
        Value::ByteVector(bv) => {
            let mut h = DefaultHasher::new();
            13u8.hash(&mut h);
            bv.borrow().hash(&mut h);
            h.finish()
        }
        _ => hash_eqv(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_fixnum() {
        let a = Value::fixnum(5);
        let b = Value::fixnum(5);
        assert!(eq(&a, &b));
    }

    #[test]
    fn eqv_compares_numbers() {
        let a = Value::fixnum(5);
        let b = Value::Fixnum(5);
        assert!(eqv(&a, &b));
    }

    #[test]
    fn equal_lists() {
        let a = Value::list([Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
        let b = Value::list([Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
        assert!(equal(&a, &b));
    }

    #[test]
    fn equal_strings() {
        let a = Value::string("hello");
        let b = Value::string("hello");
        assert!(equal(&a, &b));
        assert!(!eq(&a, &b)); // distinct heap allocations
    }

    #[test]
    fn equal_handles_self_cycle() {
        // Build (define x (cons 1 2)) ; (set-cdr! x x)
        let x = Pair::new(Value::fixnum(1), Value::fixnum(2));
        x.set_cdr(Value::Pair(x.clone()));
        let v = Value::Pair(x);
        // equal? on the same cyclic value must terminate and return true.
        assert!(equal(&v, &v));
    }

    #[test]
    fn equal_handles_distinct_cycles_same_shape() {
        // Two independently constructed self-cycles with the same car.
        let a = Pair::new(Value::fixnum(7), Value::fixnum(0));
        a.set_cdr(Value::Pair(a.clone()));
        let b = Pair::new(Value::fixnum(7), Value::fixnum(0));
        b.set_cdr(Value::Pair(b.clone()));
        let va = Value::Pair(a);
        let vb = Value::Pair(b);
        assert!(equal(&va, &vb));
    }

    #[test]
    fn hash_agrees_with_eq_for_fixnums() {
        let a = Value::fixnum(42);
        let b = Value::fixnum(42);
        assert!(eq(&a, &b));
        assert_eq!(hash_value(&a, HtEqKind::Eq), hash_value(&b, HtEqKind::Eq));
    }

    #[test]
    fn hash_agrees_with_equal_for_nested_lists() {
        let a = Value::list([
            Value::fixnum(1),
            Value::list([Value::fixnum(2), Value::fixnum(3)]),
        ]);
        let b = Value::list([
            Value::fixnum(1),
            Value::list([Value::fixnum(2), Value::fixnum(3)]),
        ]);
        assert!(equal(&a, &b));
        assert_eq!(
            hash_value(&a, HtEqKind::Equal),
            hash_value(&b, HtEqKind::Equal)
        );
    }

    #[test]
    fn hash_exactness_respected_for_eqv() {
        // 2 and 2.0 are not `eqv?`-equal (exactness differs); their hashes
        // aren't required to differ, but the common case should, and
        // correctness never depends on it (eqv? still resolves collisions).
        let exact = Value::fixnum(2);
        let inexact = Value::Flonum(2.0);
        assert!(!eqv(&exact, &inexact));
        assert_ne!(
            hash_value(&exact, HtEqKind::Eqv),
            hash_value(&inexact, HtEqKind::Eqv)
        );
    }

    #[test]
    fn hash_equal_depth_cap_terminates_on_self_cycle() {
        // A self-cyclic pair must not loop or stack-overflow when hashed
        // under equal-kind; the depth cap collapses it into a constant.
        let x = Pair::new(Value::fixnum(1), Value::fixnum(2));
        x.set_cdr(Value::Pair(x.clone()));
        let v = Value::Pair(x);
        let _ = hash_value(&v, HtEqKind::Equal); // must terminate
    }

    #[test]
    fn hash_equal_vectors_and_bytevectors() {
        let a = Value::Vector(crate::Gc::new(std::cell::RefCell::new(vec![
            Value::fixnum(1),
            Value::fixnum(2),
        ])));
        let b = Value::Vector(crate::Gc::new(std::cell::RefCell::new(vec![
            Value::fixnum(1),
            Value::fixnum(2),
        ])));
        assert!(equal(&a, &b));
        assert_eq!(
            hash_value(&a, HtEqKind::Equal),
            hash_value(&b, HtEqKind::Equal)
        );
    }
}

#[cfg(test)]
mod write_cycle_tests {
    use super::*;
    use crate::value::WriteMode;
    use crate::SymbolTable;

    #[test]
    fn write_does_not_loop_on_self_cycle() {
        let syms = SymbolTable::new();
        let x = Pair::new(Value::fixnum(1), Value::fixnum(2));
        x.set_cdr(Value::Pair(x.clone()));
        let v = Value::Pair(x);
        let s = v.format_with(&syms, WriteMode::Write);
        // Output should mention #<cycle>; just verify it terminates and is
        // bounded.
        assert!(s.contains("#<cycle>"), "{}", s);
        assert!(s.len() < 64, "output unexpectedly long: {}", s);
    }

    #[test]
    fn write_does_not_loop_on_self_cycle_vector() {
        let syms = SymbolTable::new();
        let v_inner: crate::Gc<std::cell::RefCell<Vec<Value>>> =
            crate::Gc::new(std::cell::RefCell::new(vec![Value::Boolean(false)]));
        v_inner.borrow_mut()[0] = Value::Vector(v_inner.clone());
        let v = Value::Vector(v_inner);
        let s = v.format_with(&syms, WriteMode::Write);
        assert!(s.contains("#(...)"), "{}", s);
        assert!(s.len() < 64, "output unexpectedly long: {}", s);
    }
}
