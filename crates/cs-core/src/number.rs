//! Numeric tower: Fixnum, BigInt, Rational, Flonum.
//!
//! Foundation milestone implements R6RS contagion (any inexact contaminates
//! the result). Complex numbers are deferred.

use std::fmt;
use std::rc::Rc;

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Signed, ToPrimitive, Zero};

#[derive(Clone)]
pub enum Number {
    Fixnum(i64),
    Big(Rc<BigInt>),
    Rat(Rc<BigRational>),
    Flonum(f64),
}

impl Number {
    pub fn from_i64(v: i64) -> Self {
        Number::Fixnum(v)
    }

    pub fn from_f64(v: f64) -> Self {
        Number::Flonum(v)
    }

    /// Parse a base-10 integer literal, returning Fixnum when it fits in
    /// i64 or Big otherwise. Used by the lexer; returns None on bad input.
    pub fn parse_decimal_integer(s: &str) -> Option<Self> {
        if let Ok(v) = s.parse::<i64>() {
            return Some(Number::Fixnum(v));
        }
        // Out of i64 range — try BigInt.
        s.parse::<BigInt>().ok().map(|b| Number::Big(Rc::new(b)))
    }

    pub fn is_exact(&self) -> bool {
        !matches!(self, Number::Flonum(_))
    }

    pub fn is_zero(&self) -> bool {
        match self {
            Number::Fixnum(v) => *v == 0,
            Number::Big(b) => b.is_zero(),
            Number::Rat(r) => r.is_zero(),
            Number::Flonum(f) => *f == 0.0,
        }
    }

    pub fn to_f64(&self) -> f64 {
        match self {
            Number::Fixnum(v) => *v as f64,
            Number::Big(b) => b.to_f64().unwrap_or(f64::NAN),
            Number::Rat(r) => r.to_f64().unwrap_or(f64::NAN),
            Number::Flonum(f) => *f,
        }
    }

    pub fn add(&self, other: &Number) -> Number {
        if !self.is_exact() || !other.is_exact() {
            return Number::Flonum(self.to_f64() + other.to_f64());
        }
        match (self, other) {
            (Number::Fixnum(a), Number::Fixnum(b)) => match a.checked_add(*b) {
                Some(v) => Number::Fixnum(v),
                None => Number::Big(Rc::new(BigInt::from(*a) + BigInt::from(*b))),
            },
            _ => exact_add(self, other),
        }
    }

    pub fn sub(&self, other: &Number) -> Number {
        if !self.is_exact() || !other.is_exact() {
            return Number::Flonum(self.to_f64() - other.to_f64());
        }
        match (self, other) {
            (Number::Fixnum(a), Number::Fixnum(b)) => match a.checked_sub(*b) {
                Some(v) => Number::Fixnum(v),
                None => Number::Big(Rc::new(BigInt::from(*a) - BigInt::from(*b))),
            },
            _ => exact_sub(self, other),
        }
    }

    pub fn mul(&self, other: &Number) -> Number {
        if !self.is_exact() || !other.is_exact() {
            return Number::Flonum(self.to_f64() * other.to_f64());
        }
        match (self, other) {
            (Number::Fixnum(a), Number::Fixnum(b)) => match a.checked_mul(*b) {
                Some(v) => Number::Fixnum(v),
                None => Number::Big(Rc::new(BigInt::from(*a) * BigInt::from(*b))),
            },
            _ => exact_mul(self, other),
        }
    }

    pub fn div(&self, other: &Number) -> Result<Number, NumError> {
        // Flonum (inexact) division by zero yields IEEE-754 infinities or
        // NaN per R6RS — only exact division by exact zero raises.
        if other.is_zero() {
            if !self.is_exact() || !other.is_exact() {
                return Ok(Number::Flonum(self.to_f64() / other.to_f64()));
            }
            return Err(NumError::DivisionByZero);
        }
        if !self.is_exact() || !other.is_exact() {
            return Ok(Number::Flonum(self.to_f64() / other.to_f64()));
        }
        let a = to_rational(self);
        let b = to_rational(other);
        let r = a / b;
        Ok(simplify_rational(r))
    }

    pub fn neg(&self) -> Number {
        match self {
            Number::Fixnum(v) => match v.checked_neg() {
                Some(v) => Number::Fixnum(v),
                None => Number::Big(Rc::new(-BigInt::from(*v))),
            },
            Number::Big(b) => Number::Big(Rc::new(-(b.as_ref().clone()))),
            Number::Rat(r) => Number::Rat(Rc::new(-(r.as_ref().clone()))),
            Number::Flonum(f) => Number::Flonum(-f),
        }
    }

    pub fn abs(&self) -> Number {
        match self {
            Number::Fixnum(v) => match v.checked_abs() {
                Some(v) => Number::Fixnum(v),
                None => Number::Big(Rc::new(BigInt::from(*v).abs())),
            },
            Number::Big(b) => Number::Big(Rc::new(b.as_ref().abs())),
            Number::Rat(r) => Number::Rat(Rc::new(r.as_ref().abs())),
            Number::Flonum(f) => Number::Flonum(f.abs()),
        }
    }

    pub fn cmp(&self, other: &Number) -> std::cmp::Ordering {
        if !self.is_exact() || !other.is_exact() {
            self.to_f64()
                .partial_cmp(&other.to_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        } else {
            let a = to_rational(self);
            let b = to_rational(other);
            a.cmp(&b)
        }
    }

    pub fn eq_value(&self, other: &Number) -> bool {
        // R6RS `=`: numerically equal across types, ignoring exactness.
        if !self.is_exact() || !other.is_exact() {
            return self.to_f64() == other.to_f64();
        }
        let a = to_rational(self);
        let b = to_rational(other);
        a == b
    }

    pub fn is_integer(&self) -> bool {
        match self {
            Number::Fixnum(_) | Number::Big(_) => true,
            Number::Rat(r) => r.is_integer(),
            Number::Flonum(f) => f.is_finite() && f.fract() == 0.0,
        }
    }

    /// Promote any integer-shaped Number to a BigInt. Useful for paths
    /// that need uniform i64-or-Big arithmetic (quotient/remainder/etc.)
    /// without quadratic dispatch on the four enum variants.
    /// Returns None for non-integer Numbers (rationals, non-int flonums).
    pub fn to_bigint(&self) -> Option<BigInt> {
        match self {
            Number::Fixnum(v) => Some(BigInt::from(*v)),
            Number::Big(b) => Some(b.as_ref().clone()),
            Number::Rat(r) if r.is_integer() => Some(r.numer().clone()),
            Number::Flonum(f) if f.is_finite() && f.fract() == 0.0 => {
                // Flonums with integer values up to ~2^53 round-trip cleanly.
                // We approximate by truncating; callers should already have
                // checked is_integer() before calling.
                Some(BigInt::from(*f as i64))
            }
            _ => None,
        }
    }

    /// Truncating quotient (R5RS/R6RS `quotient`). Result has the sign
    /// of x*y where (x, y) = (self, other). Errors on zero divisor.
    pub fn quotient(&self, other: &Number) -> Result<Number, NumError> {
        if other.is_zero() {
            return Err(NumError::DivisionByZero);
        }
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        Ok(simplify_bigint(&a / &b))
    }

    /// Truncating remainder (R5RS/R6RS `remainder`). Result has the sign
    /// of x. `(remainder x y) = x - y * (quotient x y)`.
    pub fn remainder(&self, other: &Number) -> Result<Number, NumError> {
        if other.is_zero() {
            return Err(NumError::DivisionByZero);
        }
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        Ok(simplify_bigint(&a % &b))
    }

    /// Floor modulo (R5RS/R6RS `modulo`). Result has the sign of y.
    /// `(modulo x y) = (remainder x y) + y` when signs differ.
    pub fn modulo(&self, other: &Number) -> Result<Number, NumError> {
        if other.is_zero() {
            return Err(NumError::DivisionByZero);
        }
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        let r = &a % &b;
        let result = if (r.sign() == num_bigint::Sign::Minus && b.sign() == num_bigint::Sign::Plus)
            || (r.sign() == num_bigint::Sign::Plus && b.sign() == num_bigint::Sign::Minus)
        {
            &r + &b
        } else {
            r
        };
        Ok(simplify_bigint(result))
    }

    /// R6RS Euclidean div: `nd` such that `0 ≤ x − y·nd < |y|`.
    pub fn euclid_div(&self, other: &Number) -> Result<Number, NumError> {
        if other.is_zero() {
            return Err(NumError::DivisionByZero);
        }
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        let q_trunc = &a / &b;
        let r = &a - &q_trunc * &b;
        let q_eucl = if r.sign() == num_bigint::Sign::Minus {
            if b.sign() == num_bigint::Sign::Plus {
                q_trunc - 1
            } else {
                q_trunc + 1
            }
        } else {
            q_trunc
        };
        Ok(simplify_bigint(q_eucl))
    }

    /// R6RS Euclidean mod: always non-negative remainder.
    pub fn euclid_mod(&self, other: &Number) -> Result<Number, NumError> {
        let d = self.euclid_div(other)?;
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        let dn = d.to_bigint().ok_or(NumError::DivisionByZero)?;
        Ok(simplify_bigint(&a - &dn * &b))
    }
}

fn simplify_bigint(b: BigInt) -> Number {
    if let Some(small) = b.to_i64() {
        Number::Fixnum(small)
    } else {
        Number::Big(Rc::new(b))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum NumError {
    DivisionByZero,
}

fn to_rational(n: &Number) -> BigRational {
    match n {
        Number::Fixnum(v) => BigRational::from(BigInt::from(*v)),
        Number::Big(b) => BigRational::from(b.as_ref().clone()),
        Number::Rat(r) => r.as_ref().clone(),
        Number::Flonum(_) => unreachable!("to_rational on flonum"),
    }
}

fn simplify_rational(r: BigRational) -> Number {
    if r.is_integer() {
        let n = r.numer();
        if let Some(small) = n.to_i64() {
            return Number::Fixnum(small);
        }
        return Number::Big(Rc::new(n.clone()));
    }
    Number::Rat(Rc::new(r))
}

fn exact_add(a: &Number, b: &Number) -> Number {
    simplify_rational(to_rational(a) + to_rational(b))
}

fn exact_sub(a: &Number, b: &Number) -> Number {
    simplify_rational(to_rational(a) - to_rational(b))
}

fn exact_mul(a: &Number, b: &Number) -> Number {
    simplify_rational(to_rational(a) * to_rational(b))
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Number::Fixnum(v) => write!(f, "{}", v),
            Number::Big(b) => write!(f, "{}", b),
            Number::Rat(r) => write!(f, "{}/{}", r.numer(), r.denom()),
            Number::Flonum(v) => {
                if v.is_nan() {
                    write!(f, "+nan.0")
                } else if v.is_infinite() {
                    write!(f, "{}inf.0", if *v > 0.0 { "+" } else { "-" })
                } else if v.fract() == 0.0 && v.is_finite() {
                    write!(f, "{}.0", *v as i64)
                } else {
                    write!(f, "{}", v)
                }
            }
        }
    }
}

impl fmt::Debug for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_fixnum() {
        let a = Number::Fixnum(2);
        let b = Number::Fixnum(3);
        let c = a.add(&b);
        assert!(matches!(c, Number::Fixnum(5)));
    }

    #[test]
    fn add_overflow_promotes_to_big() {
        let a = Number::Fixnum(i64::MAX);
        let b = Number::Fixnum(1);
        let c = a.add(&b);
        assert!(matches!(c, Number::Big(_)));
    }

    #[test]
    fn div_produces_rational() {
        let a = Number::Fixnum(1);
        let b = Number::Fixnum(3);
        let c = a.div(&b).unwrap();
        match c {
            Number::Rat(r) => {
                assert_eq!(*r.numer(), BigInt::from(1));
                assert_eq!(*r.denom(), BigInt::from(3));
            }
            _ => panic!("expected rational, got {:?}", c),
        }
    }

    #[test]
    fn div_simplifies_to_integer() {
        let a = Number::Fixnum(6);
        let b = Number::Fixnum(2);
        let c = a.div(&b).unwrap();
        assert!(matches!(c, Number::Fixnum(3)));
    }

    #[test]
    fn inexact_contagion() {
        let a = Number::Fixnum(1);
        let b = Number::Flonum(2.0);
        let c = a.add(&b);
        assert!(matches!(c, Number::Flonum(_)));
    }

    #[test]
    fn div_by_zero_errors() {
        let a = Number::Fixnum(1);
        let b = Number::Fixnum(0);
        assert!(matches!(a.div(&b), Err(NumError::DivisionByZero)));
    }
}
