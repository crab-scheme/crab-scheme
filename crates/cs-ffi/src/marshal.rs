//! Type marshaling between Rust types and Scheme `Value`s.
//!
//! `FromValue::from_value(&Value) -> Result<Self, FfiError>` reads a
//! Scheme value into the requested Rust type; `IntoValue::into_value`
//! goes the other way. Default impls cover the common case
//! (i64, f64, bool, char, String, Vec<u8>, etc.); user types
//! implement these by hand or via a future derive macro.

use crate::error::FfiError;
use cs_core::{Number, Value};

/// Convert a Scheme `Value` into a Rust type. Returns
/// `FfiError::TypeMismatch` when the value's runtime type doesn't
/// match what the Rust type expects.
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self, FfiError>;
}

/// Convert a Rust type into a Scheme `Value`. Total — no errors —
/// because every Rust value can be expressed as some `Value`.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

// ---- FromValue impls -------------------------------------------------------

impl FromValue for i64 {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::Number(Number::Fixnum(n)) => Ok(*n),
            other => Err(FfiError::TypeMismatch {
                expected: "i64",
                got: other.type_name().to_string(),
            }),
        }
    }
}

impl FromValue for f64 {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::Number(n) => Ok(n.to_f64()),
            other => Err(FfiError::TypeMismatch {
                expected: "f64",
                got: other.type_name().to_string(),
            }),
        }
    }
}

impl FromValue for bool {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::Boolean(b) => Ok(*b),
            other => Err(FfiError::TypeMismatch {
                expected: "boolean",
                got: other.type_name().to_string(),
            }),
        }
    }
}

impl FromValue for char {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::Character(c) => Ok(*c),
            other => Err(FfiError::TypeMismatch {
                expected: "character",
                got: other.type_name().to_string(),
            }),
        }
    }
}

impl FromValue for String {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::String(s) => Ok(s.borrow().to_string()),
            other => Err(FfiError::TypeMismatch {
                expected: "string",
                got: other.type_name().to_string(),
            }),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::ByteVector(b) => Ok(b.borrow().clone()),
            other => Err(FfiError::TypeMismatch {
                expected: "bytevector",
                got: other.type_name().to_string(),
            }),
        }
    }
}

impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        let mut out = Vec::new();
        let mut cur = v.clone();
        loop {
            match cur {
                Value::Null => return Ok(out),
                Value::Pair(p) => {
                    let car = p.car.borrow().clone();
                    out.push(T::from_value(&car)?);
                    cur = p.cdr.borrow().clone();
                }
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "proper list",
                        got: other.type_name().to_string(),
                    });
                }
            }
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        match v {
            Value::Boolean(false) => Ok(None),
            other => Ok(Some(T::from_value(other)?)),
        }
    }
}

impl FromValue for Value {
    fn from_value(v: &Value) -> Result<Self, FfiError> {
        Ok(v.clone())
    }
}

// ---- IntoValue impls -------------------------------------------------------

impl IntoValue for i64 {
    fn into_value(self) -> Value {
        Value::fixnum(self)
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::flonum(self)
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Boolean(self)
    }
}

impl IntoValue for char {
    fn into_value(self) -> Value {
        Value::Character(self)
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::string(self)
    }
}

impl<'a> IntoValue for &'a str {
    fn into_value(self) -> Value {
        Value::string(self.to_string())
    }
}

impl IntoValue for Vec<u8> {
    fn into_value(self) -> Value {
        Value::ByteVector(cs_core::Gc::new(std::cell::RefCell::new(self)))
    }
}

impl<T: IntoValue> IntoValue for Vec<T> {
    fn into_value(self) -> Value {
        Value::list(self.into_iter().map(IntoValue::into_value))
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Value {
        match self {
            None => Value::Boolean(false),
            Some(v) => v.into_value(),
        }
    }
}

impl IntoValue for () {
    fn into_value(self) -> Value {
        Value::Unspecified
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i64_round_trip() {
        let v = Value::fixnum(42);
        let n: i64 = i64::from_value(&v).unwrap();
        assert_eq!(n, 42);
        let back = n.into_value();
        match back {
            Value::Number(Number::Fixnum(42)) => {}
            other => panic!("expected fixnum 42, got {:?}", other),
        }
    }

    #[test]
    fn f64_accepts_fixnum_promotion() {
        let v = Value::fixnum(5);
        let f: f64 = f64::from_value(&v).unwrap();
        assert_eq!(f, 5.0);
    }

    #[test]
    fn string_round_trip() {
        let v = Value::string("hello");
        let s: String = String::from_value(&v).unwrap();
        assert_eq!(s, "hello");
        let back = s.into_value();
        match back {
            Value::String(_) => {}
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn type_mismatch_on_wrong_type() {
        let v = Value::string("not a number");
        let r = i64::from_value(&v);
        match r {
            Err(FfiError::TypeMismatch { expected, .. }) => {
                assert_eq!(expected, "i64");
            }
            _ => panic!("expected TypeMismatch"),
        }
    }

    #[test]
    fn vec_of_i64_from_proper_list() {
        let v = Value::list([Value::fixnum(1), Value::fixnum(2), Value::fixnum(3)]);
        let xs: Vec<i64> = Vec::<i64>::from_value(&v).unwrap();
        assert_eq!(xs, vec![1, 2, 3]);
    }

    #[test]
    fn vec_into_value_produces_list() {
        let xs: Vec<i64> = vec![10, 20];
        let v = xs.into_value();
        let back: Vec<i64> = Vec::<i64>::from_value(&v).unwrap();
        assert_eq!(back, vec![10, 20]);
    }

    #[test]
    fn option_none_is_false() {
        let none: Option<i64> = None;
        match none.into_value() {
            Value::Boolean(false) => {}
            other => panic!("expected #f, got {:?}", other),
        }
    }

    #[test]
    fn option_some_unwraps() {
        let some: Option<i64> = Some(7);
        match some.into_value() {
            Value::Number(Number::Fixnum(7)) => {}
            other => panic!("expected 7, got {:?}", other),
        }
    }

    #[test]
    fn option_from_value_handles_false_and_some() {
        let v = Value::Boolean(false);
        let r: Option<i64> = Option::from_value(&v).unwrap();
        assert_eq!(r, None);

        let v = Value::fixnum(5);
        let r: Option<i64> = Option::from_value(&v).unwrap();
        assert_eq!(r, Some(5));
    }

    #[test]
    fn unit_into_value_is_unspecified() {
        match ().into_value() {
            Value::Unspecified => {}
            other => panic!("expected unspecified, got {:?}", other),
        }
    }

    #[test]
    fn value_passthrough() {
        let v = Value::fixnum(99);
        let same: Value = Value::from_value(&v).unwrap();
        assert!(matches!(same, Value::Number(Number::Fixnum(99))));
    }

    #[test]
    fn vec_from_improper_list_errors() {
        let v = Value::Pair(cs_core::Pair::new(Value::fixnum(1), Value::fixnum(2)));
        let r = Vec::<i64>::from_value(&v);
        assert!(matches!(r, Err(FfiError::TypeMismatch { .. })));
    }
}
