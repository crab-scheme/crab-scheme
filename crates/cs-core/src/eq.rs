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
        (Value::String(x), Value::String(y)) => crate::Gc::ptr_eq(x, y),
        (Value::Pair(x), Value::Pair(y)) => Rc::ptr_eq(x, y),
        (Value::Vector(x), Value::Vector(y)) => crate::Gc::ptr_eq(x, y),
        (Value::ByteVector(x), Value::ByteVector(y)) => crate::Gc::ptr_eq(x, y),
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

    #[test]
    fn equal_handles_self_cycle() {
        // Build (define x (cons 1 2)) ; (set-cdr! x x)
        let x = Pair::new(Value::fixnum(1), Value::fixnum(2));
        *x.cdr.borrow_mut() = Value::Pair(x.clone());
        let v = Value::Pair(x);
        // equal? on the same cyclic value must terminate and return true.
        assert!(equal(&v, &v));
    }

    #[test]
    fn equal_handles_distinct_cycles_same_shape() {
        // Two independently constructed self-cycles with the same car.
        let a = Pair::new(Value::fixnum(7), Value::fixnum(0));
        *a.cdr.borrow_mut() = Value::Pair(a.clone());
        let b = Pair::new(Value::fixnum(7), Value::fixnum(0));
        *b.cdr.borrow_mut() = Value::Pair(b.clone());
        let va = Value::Pair(a);
        let vb = Value::Pair(b);
        assert!(equal(&va, &vb));
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
        *x.cdr.borrow_mut() = Value::Pair(x.clone());
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
