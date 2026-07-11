//! Numeric tower: Fixnum, BigInt, Rational, Flonum.
//!
//! Foundation milestone implements R6RS contagion (any inexact contaminates
//! the result). Complex numbers are deferred.

use std::fmt;
use std::rc::Rc;

use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{Num, One, Signed, ToPrimitive, Zero};

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

    /// Parse a radix integer literal (base 2, 8, 10, or 16), returning
    /// Fixnum when it fits in i64 or Big otherwise. The input `s` may
    /// carry a leading `'-'` sign (matching what `i64::from_str_radix`
    /// and `BigInt::from_str_radix` both accept). Returns None on bad
    /// input (invalid digits for the given radix).
    pub fn parse_radix_integer(s: &str, radix: u32) -> Option<Self> {
        if let Ok(v) = i64::from_str_radix(s, radix) {
            return Some(Number::Fixnum(v));
        }
        // Out of i64 range — try BigInt.
        BigInt::from_str_radix(s, radix)
            .ok()
            .map(|b| Number::Big(Rc::new(b)))
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
            return self
                .to_f64()
                .partial_cmp(&other.to_f64())
                .unwrap_or(std::cmp::Ordering::Equal);
        }
        match (self, other) {
            (Number::Fixnum(a), Number::Fixnum(b)) => a.cmp(b),
            (Number::Big(a), Number::Big(b)) => a.as_ref().cmp(b.as_ref()),
            (Number::Fixnum(a), Number::Big(b)) => match b.to_i64() {
                Some(b) => a.cmp(&b),
                None => BigInt::from(*a).cmp(b.as_ref()),
            },
            (Number::Big(a), Number::Fixnum(b)) => match a.to_i64() {
                Some(a) => a.cmp(b),
                None => a.as_ref().cmp(&BigInt::from(*b)),
            },
            _ => {
                let a = to_rational(self);
                let b = to_rational(other);
                a.cmp(&b)
            }
        }
    }

    pub fn eq_value(&self, other: &Number) -> bool {
        // R6RS `=`: numerically equal across types, ignoring exactness.
        if !self.is_exact() || !other.is_exact() {
            return self.to_f64() == other.to_f64();
        }
        match (self, other) {
            (Number::Fixnum(a), Number::Fixnum(b)) => a == b,
            (Number::Big(a), Number::Big(b)) => a == b,
            (Number::Fixnum(a), Number::Big(b)) | (Number::Big(b), Number::Fixnum(a)) => {
                match b.to_i64() {
                    Some(b) => *a == b,
                    None => false,
                }
            }
            _ => {
                let a = to_rational(self);
                let b = to_rational(other);
                a == b
            }
        }
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

    /// R6RS `numerator`. For exact rationals, returns the numerator as
    /// a Number (Fixnum or Big); for integers, returns the integer
    /// itself. For inexact (flonum) inputs, computes via flonum
    /// rationalization and returns a flonum integer.
    pub fn numerator(&self) -> Option<Number> {
        match self {
            Number::Fixnum(_) | Number::Big(_) => Some(self.clone()),
            Number::Rat(r) => Some(simplify_bigint(r.numer().clone())),
            Number::Flonum(f) => {
                if !f.is_finite() {
                    return None;
                }
                let r = BigRational::from_float(*f)?;
                let n = r.numer();
                Some(Number::Flonum(n.to_f64().unwrap_or(f64::NAN)))
            }
        }
    }

    /// R6RS `denominator`. Returns 1 for integers, the denominator for
    /// rationals, and a flonum integer for finite flonums.
    pub fn denominator(&self) -> Option<Number> {
        match self {
            Number::Fixnum(_) | Number::Big(_) => Some(Number::Fixnum(1)),
            Number::Rat(r) => Some(simplify_bigint(r.denom().clone())),
            Number::Flonum(f) => {
                if !f.is_finite() {
                    return None;
                }
                let r = BigRational::from_float(*f)?;
                let d = r.denom();
                Some(Number::Flonum(d.to_f64().unwrap_or(f64::NAN)))
            }
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

    /// R6RS `exact-integer-sqrt`: returns `(s, r)` such that
    /// `s² ≤ n` and `n = s² + r` with `r < 2s + 1`. Bignum-correct
    /// via Newton's method on the BigInt directly. Raises on negative
    /// inputs (R6RS &assertion).
    pub fn exact_integer_sqrt(&self) -> Option<(Number, Number)> {
        // Coerce to BigInt; non-integer inputs return None.
        let n = self.to_bigint()?;
        if n.is_negative() {
            return None;
        }
        if n.is_zero() {
            return Some((Number::Fixnum(0), Number::Fixnum(0)));
        }
        // Newton's method on BigInt.
        // Initial guess: 2^(bit_length / 2).
        let bits = n.bits();
        let mut x = BigInt::from(1u64) << ((bits / 2) + 1);
        loop {
            let y = (&x + &n / &x) >> 1;
            if y >= x {
                break;
            }
            x = y;
        }
        // Now x is floor(sqrt(n)).
        let r = &n - &x * &x;
        Some((simplify_bigint(x), simplify_bigint(r)))
    }

    /// R7RS `floor-quotient`: floor(x/y), i.e. rounded toward -∞.
    /// Differs from R5RS `quotient` (truncated) for negative operands.
    /// Identity: `x = y · floor-quotient + floor-remainder`, where
    /// `floor-remainder` has the same sign as the divisor (== `modulo`).
    pub fn floor_quotient(&self, other: &Number) -> Result<Number, NumError> {
        if other.is_zero() {
            return Err(NumError::DivisionByZero);
        }
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        let r = &a % &b;
        // floor-remainder follows the divisor's sign.
        let fr = if (r.sign() == num_bigint::Sign::Minus && b.sign() == num_bigint::Sign::Plus)
            || (r.sign() == num_bigint::Sign::Plus && b.sign() == num_bigint::Sign::Minus)
        {
            &r + &b
        } else {
            r
        };
        // (x - fr) / y is exact integer division.
        let q = (&a - &fr) / &b;
        Ok(simplify_bigint(q))
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

    /// R6RS centered div0: result `nd` such that `-|y|/2 ≤ x − y·nd < |y|/2`.
    /// Handles ties (when |y| is even and the Euclidean remainder is
    /// exactly |y|/2) by shifting downward so the remainder lands in
    /// the half-open interval.
    pub fn euclid_div0(&self, other: &Number) -> Result<Number, NumError> {
        if other.is_zero() {
            return Err(NumError::DivisionByZero);
        }
        let d = self.euclid_div(other)?;
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        let dn = d.to_bigint().ok_or(NumError::DivisionByZero)?;
        let m = &a - &dn * &b;
        let abs_b = b.abs();
        // Shift if 2·m > |b| (always above the upper boundary), or
        // 2·m == |b| AND |b| is even (boundary tie — round to land in
        // [-|y|/2, |y|/2)).
        let two_m = &m * 2;
        let shift = two_m > abs_b || (two_m == abs_b && (&abs_b % 2u32).is_zero());
        let result = if shift {
            if b.sign() == num_bigint::Sign::Plus {
                dn + 1
            } else {
                dn - 1
            }
        } else {
            dn
        };
        Ok(simplify_bigint(result))
    }

    /// R6RS centered mod0: remainder in `[-|y|/2, |y|/2)`.
    pub fn euclid_mod0(&self, other: &Number) -> Result<Number, NumError> {
        let d0 = self.euclid_div0(other)?;
        let a = self.to_bigint().ok_or(NumError::DivisionByZero)?;
        let b = other.to_bigint().ok_or(NumError::DivisionByZero)?;
        let d0n = d0.to_bigint().ok_or(NumError::DivisionByZero)?;
        Ok(simplify_bigint(&a - &d0n * &b))
    }
}

fn simplify_bigint(b: BigInt) -> Number {
    if let Some(small) = b.to_i64() {
        Number::Fixnum(small)
    } else {
        Number::Big(Rc::new(b))
    }
}

impl Number {
    /// R6RS `exact`. Coerces a Number to its exact form. Integer-valued
    /// flonums become Fixnum/Big; non-integral finite flonums become
    /// Rat via BigRational::from_float (the exact bit-pattern as a
    /// dyadic rational). Non-finite flonums (inf/NaN) cannot be
    /// represented exactly and return None.
    pub fn to_exact(&self) -> Option<Number> {
        match self {
            n if n.is_exact() => Some(n.clone()),
            Number::Flonum(f) => {
                if !f.is_finite() {
                    return None;
                }
                if f.fract() == 0.0 && (*f as i64 as f64) == *f {
                    return Some(Number::Fixnum(*f as i64));
                }
                BigRational::from_float(*f).map(simplify_rational)
            }
            _ => unreachable!(),
        }
    }
}

impl Number {
    /// R6RS `bitwise-and` — two's-complement infinite-precision bitwise AND.
    /// Returns `None` for non-integer (rational, non-integer flonum) operands.
    ///
    /// # Examples
    /// ```
    /// use cs_core::number::Number;
    /// let a = Number::Big(std::rc::Rc::new("18446744073709551615".parse().unwrap()));
    /// let b = Number::Fixnum(127);
    /// assert!(matches!(a.bit_and(&b), Some(Number::Fixnum(127))));
    /// ```
    pub fn bit_and(&self, other: &Number) -> Option<Number> {
        let a = self.to_bigint()?;
        let b = other.to_bigint()?;
        Some(simplify_bigint(&a & &b))
    }

    /// R6RS `bitwise-ior` / `bitwise-or` — two's-complement infinite-precision
    /// bitwise OR. Returns `None` for non-integer operands.
    pub fn bit_or(&self, other: &Number) -> Option<Number> {
        let a = self.to_bigint()?;
        let b = other.to_bigint()?;
        Some(simplify_bigint(&a | &b))
    }

    /// R6RS `bitwise-xor` — two's-complement infinite-precision bitwise XOR.
    /// Returns `None` for non-integer operands.
    pub fn bit_xor(&self, other: &Number) -> Option<Number> {
        let a = self.to_bigint()?;
        let b = other.to_bigint()?;
        Some(simplify_bigint(&a ^ &b))
    }

    /// R6RS `bitwise-not` — two's-complement bitwise NOT.
    /// Identity: `!(n) = -(n + 1)`.
    /// Returns `None` for non-integer operands.
    pub fn bit_not(&self) -> Option<Number> {
        let n = self.to_bigint()?;
        // Two's complement: !(n) = -(n + 1)
        Some(simplify_bigint(-n - BigInt::one()))
    }

    /// R6RS `arithmetic-shift` — infinite-precision two's-complement shift.
    /// Positive `count` = left shift (result grows); negative = arithmetic
    /// (floor) right shift.  Returns `None` if either operand is non-integer
    /// or if `count` is outside `i64` range (absurd in practice).
    ///
    /// # Examples
    /// ```
    /// use cs_core::number::Number;
    /// // (arithmetic-shift 1 64) = 2^64
    /// let result = Number::Fixnum(1).arith_shift(&Number::Fixnum(64)).unwrap();
    /// assert!(matches!(result, Number::Big(_)));
    /// // (arithmetic-shift 255 -4) = 15
    /// assert!(matches!(
    ///     Number::Fixnum(255).arith_shift(&Number::Fixnum(-4)),
    ///     Some(Number::Fixnum(15))
    /// ));
    /// ```
    pub fn arith_shift(&self, count: &Number) -> Option<Number> {
        let n = self.to_bigint()?;
        let c = count.to_bigint()?;
        let c_i64 = c.to_i64()?;
        if c_i64 >= 0 {
            Some(simplify_bigint(n << (c_i64 as usize)))
        } else {
            let abs = (-c_i64) as usize;
            Some(simplify_bigint(bigint_arith_shr(n, abs)))
        }
    }

    /// R6RS `bitwise-bit-count` — population count (number of set bits) in
    /// infinite-precision two's-complement representation.
    /// For `n >= 0`: number of 1 bits in the magnitude.
    /// For `n < 0`: `-1 - bit_count(bitwise-not n)` = `-1 - bit_count(-n - 1)`.
    /// Result is always a `Fixnum` (count ≤ bit-width of a finite number).
    /// Returns `None` for non-integer operands.
    pub fn bit_count(&self) -> Option<Number> {
        match self {
            Number::Fixnum(n) => {
                let v = if *n >= 0 {
                    n.count_ones() as i64
                } else {
                    -1 - ((!n).count_ones() as i64)
                };
                Some(Number::Fixnum(v))
            }
            Number::Big(b) => {
                if b.sign() != num_bigint::Sign::Minus {
                    // Non-negative: sum popcount over all bytes of the magnitude.
                    let count: u64 = b
                        .magnitude()
                        .to_bytes_le()
                        .iter()
                        .map(|&byte| u64::from(byte.count_ones()))
                        .sum();
                    Some(Number::Fixnum(count as i64))
                } else {
                    // Negative n: bit_count(n) = -1 - bit_count(-n - 1)
                    let pos = -b.as_ref() - BigInt::one();
                    let count: u64 = pos
                        .magnitude()
                        .to_bytes_le()
                        .iter()
                        .map(|&byte| u64::from(byte.count_ones()))
                        .sum();
                    Some(Number::Fixnum(-1 - count as i64))
                }
            }
            _ => None,
        }
    }

    /// R6RS `bitwise-length` — number of bits needed to represent `n`
    /// (excluding the sign bit), i.e. `floor(log2(|n|)) + 1` for `|n| > 0`,
    /// or 0 for `n = 0` and `n = -1`.
    /// For `n >= 0`: `BigInt::bits()` of the magnitude.
    /// For `n < 0`: `bit_length(-n - 1)` (two's-complement sign extension).
    /// Result is always a `Fixnum`. Returns `None` for non-integers.
    pub fn bit_length(&self) -> Option<Number> {
        match self {
            Number::Fixnum(n) => {
                let abs = if *n < 0 { !n } else { *n };
                let bits = if abs == 0 {
                    0
                } else {
                    64 - abs.leading_zeros() as i64
                };
                Some(Number::Fixnum(bits))
            }
            Number::Big(b) => {
                if b.sign() != num_bigint::Sign::Minus {
                    // Non-negative: number of significant bits.
                    Some(Number::Fixnum(b.bits() as i64))
                } else {
                    // Negative: bit_length(-n - 1), which is >= 0.
                    let pos = -b.as_ref() - BigInt::one();
                    Some(Number::Fixnum(pos.bits() as i64))
                }
            }
            _ => None,
        }
    }

    /// R6RS `bitwise-bit-set?` — test whether bit `bit` is set in
    /// two's-complement infinite-precision representation.
    /// Negative `bit` index returns `false` (lenient).
    /// Returns `None` if `self` or `bit` is non-integer, or if `bit`
    /// is non-negative but out of `i64` range (absurd in practice).
    pub fn bit_set_p(&self, bit: &Number) -> Option<bool> {
        let b = bit.to_bigint()?;
        // Negative bit index → false (lenient, matches R6RS spec extension).
        if b.sign() == num_bigint::Sign::Minus {
            return Some(false);
        }
        // Right-shift self by `bit` and test the LSB.
        let neg_bit = bit.neg();
        let shifted = self.arith_shift(&neg_bit)?;
        let lsb = shifted.bit_and(&Number::Fixnum(1))?;
        Some(!lsb.is_zero())
    }
}

/// Arithmetic (floor toward −∞) right shift for `BigInt`.
/// For non-negative `n`: plain `n >> abs`.
/// For negative `n`: `-((-n - 1) >> abs) - 1`, preserving two's-complement
/// sign extension.
fn bigint_arith_shr(n: BigInt, abs: usize) -> BigInt {
    if n >= BigInt::zero() {
        n >> abs
    } else {
        let pos = -&n - BigInt::one();
        -(pos >> abs) - BigInt::one()
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
    fn parse_radix_integer_fixnum() {
        // Small hex — fits in i64 → Fixnum.
        assert!(matches!(
            Number::parse_radix_integer("ff", 16),
            Some(Number::Fixnum(255))
        ));
    }

    #[test]
    fn parse_radix_integer_big_hex() {
        // 2^63 in hex — exceeds i64::MAX → Big.
        let n = Number::parse_radix_integer("8000000000000000", 16).unwrap();
        match n {
            Number::Big(b) => {
                let expected: BigInt = "9223372036854775808".parse().unwrap();
                assert_eq!(*b, expected);
            }
            _ => panic!("expected Big, got {:?}", n),
        }
    }

    #[test]
    fn parse_radix_integer_negative_fixnum() {
        // Negative hex that fits in i64 → Fixnum.
        assert!(matches!(
            Number::parse_radix_integer("-ff", 16),
            Some(Number::Fixnum(-255))
        ));
    }

    #[test]
    fn parse_radix_integer_bad_input() {
        // Invalid digits → None.
        assert!(Number::parse_radix_integer("xyz", 16).is_none());
        assert!(Number::parse_radix_integer("2", 2).is_none());
    }

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

    // ---- bignum bitwise tests ----

    fn big_int(s: &str) -> BigInt {
        s.parse().unwrap()
    }

    fn unwrap_fixnum(n: Option<Number>) -> i64 {
        match n {
            Some(Number::Fixnum(v)) => v,
            other => panic!("expected Some(Fixnum), got {:?}", other),
        }
    }

    fn unwrap_bigint(n: Option<Number>) -> BigInt {
        match n {
            Some(Number::Big(b)) => b.as_ref().clone(),
            other => panic!("expected Some(Big), got {:?}", other),
        }
    }

    #[test]
    fn bit_and_bignum_u64max_and_127() {
        // (bitwise-and (2^64 - 1) 127) = 127 — the bead's canonical failing case.
        let a = Number::Big(Rc::new(big_int("18446744073709551615")));
        let b = Number::Fixnum(127);
        assert_eq!(unwrap_fixnum(a.bit_and(&b)), 127);
    }

    #[test]
    fn bit_and_fixnum_fast_path() {
        // Fixnum inputs still normalize back to Fixnum.
        assert_eq!(
            unwrap_fixnum(Number::Fixnum(12).bit_and(&Number::Fixnum(10))),
            8
        );
    }

    #[test]
    fn bit_or_bignum() {
        // (bitwise-ior 2^100 1) = 2^100 + 1
        let pow100: BigInt = BigInt::one() << 100_usize;
        let a = Number::Big(Rc::new(pow100.clone()));
        let result = unwrap_bigint(a.bit_or(&Number::Fixnum(1)));
        assert_eq!(result, pow100 + BigInt::one());
    }

    #[test]
    fn bit_xor_same_value_is_zero() {
        let pow70: BigInt = BigInt::one() << 70_usize;
        let a = Number::Big(Rc::new(pow70.clone()));
        let b = Number::Big(Rc::new(pow70));
        assert_eq!(unwrap_fixnum(a.bit_xor(&b)), 0);
    }

    #[test]
    fn bit_not_bignum() {
        // (bitwise-not 2^64) = -(2^64 + 1)
        let pow64: BigInt = BigInt::one() << 64_usize;
        let n = Number::Big(Rc::new(pow64.clone()));
        let result = unwrap_bigint(n.bit_not());
        let expected: BigInt = -(pow64 + BigInt::one());
        assert_eq!(result, expected);
    }

    #[test]
    fn bit_not_negative_roundtrip() {
        // !(!(n)) = n for any integer n.
        for v in [-100i64, -1, 0, 1, 127, i64::MAX] {
            let result = unwrap_fixnum(Number::Fixnum(v).bit_not().unwrap().bit_not());
            assert_eq!(result, v, "bit_not roundtrip failed for {}", v);
        }
    }

    #[test]
    fn arith_shift_left_big() {
        // (arithmetic-shift 1 64) = 2^64
        let result = unwrap_bigint(Number::Fixnum(1).arith_shift(&Number::Fixnum(64)));
        let expected: BigInt = BigInt::one() << 64_usize;
        assert_eq!(result, expected);
    }

    #[test]
    fn arith_shift_left_small() {
        // (arithmetic-shift 1 3) = 8 — stays Fixnum.
        assert_eq!(
            unwrap_fixnum(Number::Fixnum(1).arith_shift(&Number::Fixnum(3))),
            8
        );
    }

    #[test]
    fn arith_shift_right_fixnum() {
        // (arithmetic-shift 255 -4) = 15
        assert_eq!(
            unwrap_fixnum(Number::Fixnum(255).arith_shift(&Number::Fixnum(-4))),
            15
        );
    }

    #[test]
    fn arith_shift_right_negative_one() {
        // (arithmetic-shift -1 -80) = -1  (sign extension: all ones >> anything = -1)
        assert_eq!(
            unwrap_fixnum(Number::Fixnum(-1).arith_shift(&Number::Fixnum(-80))),
            -1
        );
    }

    #[test]
    fn arith_shift_left_negative_n() {
        // (arithmetic-shift -1 80) = -(2^80)
        let result = unwrap_bigint(Number::Fixnum(-1).arith_shift(&Number::Fixnum(80)));
        let expected: BigInt = -(BigInt::one() << 80_usize);
        assert_eq!(result, expected);
    }

    #[test]
    fn arith_shift_right_bignum() {
        // (arithmetic-shift 2^80 -40) = 2^40 = 1099511627776 (fits in Fixnum).
        let pow80 = Number::Big(Rc::new(BigInt::one() << 80_usize));
        // 2^40 fits in i64, so simplify_bigint returns Fixnum.
        assert_eq!(
            unwrap_fixnum(pow80.arith_shift(&Number::Fixnum(-40))),
            1_i64 << 40
        );
    }

    #[test]
    fn bit_and_negative_two_complement() {
        // (-1) & 127 = 127
        assert_eq!(
            unwrap_fixnum(Number::Fixnum(-1).bit_and(&Number::Fixnum(127))),
            127
        );
        // (-2) & (-1) = -2
        assert_eq!(
            unwrap_fixnum(Number::Fixnum(-2).bit_and(&Number::Fixnum(-1))),
            -2
        );
    }

    // ---- bit_count tests ----

    #[test]
    fn bit_count_fixnum_positive() {
        assert_eq!(unwrap_fixnum(Number::Fixnum(0).bit_count()), 0);
        assert_eq!(unwrap_fixnum(Number::Fixnum(1).bit_count()), 1);
        assert_eq!(unwrap_fixnum(Number::Fixnum(0b1010_1010).bit_count()), 4);
    }

    #[test]
    fn bit_count_fixnum_negative() {
        // R6RS: bit_count(-1) = -1  (bitwise-not(-1)=0, popcount(0)=0, -1-0=-1)
        assert_eq!(unwrap_fixnum(Number::Fixnum(-1).bit_count()), -1);
        // bit_count(-2) = -2  (bitwise-not(-2)=1, popcount(1)=1, -1-1=-2)
        assert_eq!(unwrap_fixnum(Number::Fixnum(-2).bit_count()), -2);
    }

    #[test]
    fn bit_count_bignum_pow100() {
        // 2^100 has exactly 1 set bit.
        let n = Number::Big(Rc::new(BigInt::one() << 100_usize));
        assert_eq!(unwrap_fixnum(n.bit_count()), 1);
    }

    #[test]
    fn bit_count_bignum_all_ones() {
        // 2^100 - 1 has 100 set bits.
        let n = Number::Big(Rc::new((BigInt::one() << 100_usize) - BigInt::one()));
        assert_eq!(unwrap_fixnum(n.bit_count()), 100);
    }

    #[test]
    fn bit_count_bignum_negative() {
        // bit_count(-(2^64)) = -1 - bit_count(2^64 - 1)
        // 2^64 - 1 has 64 set bits, so result = -1 - 64 = -65.
        let n = Number::Big(Rc::new(-(BigInt::one() << 64_usize)));
        assert_eq!(unwrap_fixnum(n.bit_count()), -65);
    }

    // ---- bit_length tests ----

    #[test]
    fn bit_length_fixnum() {
        assert_eq!(unwrap_fixnum(Number::Fixnum(0).bit_length()), 0);
        assert_eq!(unwrap_fixnum(Number::Fixnum(1).bit_length()), 1);
        assert_eq!(unwrap_fixnum(Number::Fixnum(4).bit_length()), 3);
        // R6RS: bitwise-length(-1) = 0  (bitwise-not(-1) = 0, length(0) = 0)
        assert_eq!(unwrap_fixnum(Number::Fixnum(-1).bit_length()), 0);
        // bitwise-length(-2) = 1  (-(-2)-1 = 1, length(1) = 1)
        assert_eq!(unwrap_fixnum(Number::Fixnum(-2).bit_length()), 1);
    }

    #[test]
    fn bit_length_bignum_pow100() {
        // bitwise-length(2^100) = 101
        let n = Number::Big(Rc::new(BigInt::one() << 100_usize));
        assert_eq!(unwrap_fixnum(n.bit_length()), 101);
    }

    #[test]
    fn bit_length_bignum_negative() {
        // bitwise-length(-(2^100)) = bit_length(2^100 - 1) = 100
        let n = Number::Big(Rc::new(-(BigInt::one() << 100_usize)));
        assert_eq!(unwrap_fixnum(n.bit_length()), 100);
    }

    // ---- bit_set_p tests ----

    #[test]
    fn bit_set_p_bignum_exact_bit() {
        // bit 100 of 2^100 is set; bit 99 is not.
        let n = Number::Big(Rc::new(BigInt::one() << 100_usize));
        assert_eq!(n.bit_set_p(&Number::Fixnum(100)), Some(true));
        assert_eq!(n.bit_set_p(&Number::Fixnum(99)), Some(false));
        assert_eq!(n.bit_set_p(&Number::Fixnum(0)), Some(false));
    }

    #[test]
    fn bit_set_p_negative_n_high_bit() {
        // All bits of -1 are set (two's complement).
        assert_eq!(
            Number::Fixnum(-1).bit_set_p(&Number::Fixnum(200)),
            Some(true)
        );
        assert_eq!(Number::Fixnum(-1).bit_set_p(&Number::Fixnum(0)), Some(true));
    }

    #[test]
    fn bit_set_p_negative_bit_index_returns_false() {
        assert_eq!(
            Number::Fixnum(42).bit_set_p(&Number::Fixnum(-1)),
            Some(false)
        );
    }

    #[test]
    fn bit_set_p_fixnum_regressions() {
        assert_eq!(
            Number::Fixnum(0b1010).bit_set_p(&Number::Fixnum(1)),
            Some(true)
        );
        assert_eq!(
            Number::Fixnum(0b1010).bit_set_p(&Number::Fixnum(0)),
            Some(false)
        );
    }

    // --- cs-gl8: direct fixnum/bignum comparison fast paths ---

    #[test]
    fn cmp_fixnum_fixnum_all_orderings() {
        assert_eq!(
            Number::Fixnum(1).cmp(&Number::Fixnum(2)),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            Number::Fixnum(2).cmp(&Number::Fixnum(2)),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            Number::Fixnum(3).cmp(&Number::Fixnum(2)),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            Number::Fixnum(-5).cmp(&Number::Fixnum(5)),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn eq_value_fixnum_fixnum() {
        assert!(Number::Fixnum(7).eq_value(&Number::Fixnum(7)));
        assert!(!Number::Fixnum(7).eq_value(&Number::Fixnum(8)));
        assert!(Number::Fixnum(-3).eq_value(&Number::Fixnum(-3)));
    }

    #[test]
    fn cmp_big_big() {
        let a = Number::Big(Rc::new("99999999999999999999".parse::<BigInt>().unwrap()));
        let b = Number::Big(Rc::new("100000000000000000000".parse::<BigInt>().unwrap()));
        assert_eq!(a.cmp(&b), std::cmp::Ordering::Less);
        assert_eq!(b.cmp(&a), std::cmp::Ordering::Greater);
        assert_eq!(a.cmp(&a.clone()), std::cmp::Ordering::Equal);
    }

    #[test]
    fn eq_value_big_big() {
        let a = Number::Big(Rc::new(
            "123456789012345678901234567890".parse::<BigInt>().unwrap(),
        ));
        let b = Number::Big(Rc::new(
            "123456789012345678901234567890".parse::<BigInt>().unwrap(),
        ));
        let c = Number::Big(Rc::new(
            "123456789012345678901234567891".parse::<BigInt>().unwrap(),
        ));
        assert!(a.eq_value(&b));
        assert!(!a.eq_value(&c));
    }

    #[test]
    fn cmp_fixnum_vs_big_fitting_i64() {
        // Big that fits in i64 range, compared against a fixnum.
        let big_fits = Number::Big(Rc::new(BigInt::from(1000_i64)));
        assert_eq!(Number::Fixnum(500).cmp(&big_fits), std::cmp::Ordering::Less);
        assert_eq!(
            big_fits.cmp(&Number::Fixnum(500)),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            Number::Fixnum(1000).cmp(&big_fits),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn cmp_fixnum_vs_big_non_fitting_positive() {
        // Big far larger than any i64, positive sign.
        let huge = Number::Big(Rc::new(
            "999999999999999999999999".parse::<BigInt>().unwrap(),
        ));
        assert_eq!(
            Number::Fixnum(i64::MAX).cmp(&huge),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            huge.cmp(&Number::Fixnum(i64::MAX)),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            Number::Fixnum(i64::MIN).cmp(&huge),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn cmp_fixnum_vs_big_non_fitting_negative() {
        // Big far smaller than any i64, negative sign.
        let huge_neg = Number::Big(Rc::new(
            "-999999999999999999999999".parse::<BigInt>().unwrap(),
        ));
        assert_eq!(
            Number::Fixnum(i64::MIN).cmp(&huge_neg),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            huge_neg.cmp(&Number::Fixnum(i64::MIN)),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            Number::Fixnum(0).cmp(&huge_neg),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn eq_value_fixnum_vs_big() {
        let big_fits = Number::Big(Rc::new(BigInt::from(42_i64)));
        assert!(Number::Fixnum(42).eq_value(&big_fits));
        assert!(big_fits.eq_value(&Number::Fixnum(42)));
        assert!(!Number::Fixnum(43).eq_value(&big_fits));

        let huge = Number::Big(Rc::new(
            "999999999999999999999999".parse::<BigInt>().unwrap(),
        ));
        assert!(!Number::Fixnum(42).eq_value(&huge));
        assert!(!huge.eq_value(&Number::Fixnum(42)));
    }

    #[test]
    fn cmp_i64_min_max_vs_bignums_just_outside_range() {
        let just_above_max = Number::Big(Rc::new(BigInt::from(i64::MAX) + BigInt::one()));
        let just_below_min = Number::Big(Rc::new(BigInt::from(i64::MIN) - BigInt::one()));

        assert_eq!(
            Number::Fixnum(i64::MAX).cmp(&just_above_max),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            Number::Fixnum(i64::MIN).cmp(&just_below_min),
            std::cmp::Ordering::Greater
        );

        // Exact boundary values (fit i64) still compare correctly.
        let exact_max = Number::Big(Rc::new(BigInt::from(i64::MAX)));
        let exact_min = Number::Big(Rc::new(BigInt::from(i64::MIN)));
        assert_eq!(
            Number::Fixnum(i64::MAX).cmp(&exact_max),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            Number::Fixnum(i64::MIN).cmp(&exact_min),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn cmp_and_eq_mixed_exact_rational_unaffected() {
        // Rational-involving exact pairs must still go through to_rational.
        let half = Number::Rat(Rc::new(BigRational::new(BigInt::from(1), BigInt::from(2))));
        assert_eq!(half.cmp(&Number::Fixnum(1)), std::cmp::Ordering::Less);
        assert_eq!(Number::Fixnum(0).cmp(&half), std::cmp::Ordering::Less);
        assert!(!half.eq_value(&Number::Fixnum(0)));
        let one_as_rat = Number::Rat(Rc::new(BigRational::new(BigInt::from(2), BigInt::from(2))));
        // 2/2 simplifies conceptually to 1, though stored as Rat variant here.
        assert!(one_as_rat.eq_value(&Number::Fixnum(1)));
    }

    #[test]
    fn eq_value_inexact_contagion_unchanged() {
        // (= 2 2.0) must remain true via the inexact path, untouched by
        // the new exact fast-path arms.
        assert!(Number::Fixnum(2).eq_value(&Number::Flonum(2.0)));
        assert!(!Number::Fixnum(2).eq_value(&Number::Flonum(2.5)));
        assert_eq!(
            Number::Fixnum(2).cmp(&Number::Flonum(2.0)),
            std::cmp::Ordering::Equal
        );
    }
}
