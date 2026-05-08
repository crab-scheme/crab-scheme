//! R6RS equality predicates: eq?, eqv?, equal?
//!
//! `equal?` handles cyclic structures via a visited-set fallback.

use std::collections::HashSet;
use std::rc::Rc;

use crate::value::{Pair, Value};

/// R6RS `eq?`: identity for heap values, value equality for immediates.
pub fn eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Unspecified, Value::Unspecified) => true,
        (Value::Eof, Value::Eof) => true,
        (Value::Boolean(x), Value::Boolean(y)) => x == y,
        (Value::Character(x), Value::Character(y)) => x == y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => match (x, y) {
            (crate::Number::Fixnum(a), crate::Number::Fixnum(b)) => a == b,
            _ => false, // Other number types: eq? is implementation-defined; we say false unless identity.
        },
        (Value::String(x), Value::String(y)) => Rc::ptr_eq(x, y),
        (Value::Pair(x), Value::Pair(y)) => Rc::ptr_eq(x, y),
        (Value::Vector(x), Value::Vector(y)) => Rc::ptr_eq(x, y),
        (Value::ByteVector(x), Value::ByteVector(y)) => Rc::ptr_eq(x, y),
        (Value::Procedure(x), Value::Procedure(y)) => Rc::ptr_eq(x, y),
        (Value::Hashtable(x), Value::Hashtable(y)) => Rc::ptr_eq(x, y),
        (Value::Port(x), Value::Port(y)) => Rc::ptr_eq(x, y),
        (Value::Promise(x), Value::Promise(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

/// R6RS `eqv?`: like `eq?` but compares numbers and characters by value.
pub fn eqv(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.is_exact() == y.is_exact() && x.eq_value(y),
        _ => eq(a, b),
    }
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
            let key = (
                Rc::as_ptr(x) as *const Pair as usize,
                Rc::as_ptr(y) as *const Pair as usize,
            );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Number;

    #[test]
    fn eq_fixnum() {
        let a = Value::fixnum(5);
        let b = Value::fixnum(5);
        assert!(eq(&a, &b));
    }

    #[test]
    fn eqv_compares_numbers() {
        let a = Value::fixnum(5);
        let b = Value::Number(Number::Fixnum(5));
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
}
