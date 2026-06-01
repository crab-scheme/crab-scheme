//! CrabScheme stdlib module: `(crab binary)`.
//!
//! Fixed-width binary packing and unpacking with a format string — the
//! `(crab …)` answer to Python's `struct` and Go's `encoding/binary`.
//! R6RS bytevectors already provide the per-element primitives; this is
//! the convenient format-string layer on top, for wire protocols and
//! file headers. Pure Rust, no dependencies.
//!
//! ## Format string
//!
//! A sequence of one-character type codes, optionally preceded by an
//! endianness marker (which stays in effect until changed). Spaces are
//! ignored.
//!
//! - Endianness: `>` or `!` big-endian (default, network order), `<`
//!   little-endian.
//! - Integers: `b`/`B` = signed/unsigned 8-bit, `h`/`H` = 16-bit,
//!   `i`/`I` = 32-bit, `q`/`Q` = 64-bit.
//! - Floats: `f` = 32-bit, `d` = 64-bit.
//!
//! ## Registered procedures
//!
//! | Scheme name | Args | Returns | Notes |
//! |---|---|---|---|
//! | `binary-pack`   | fmt value … | bytevector | One value per type code. |
//! | `binary-unpack` | fmt bytevector | list | Values in format order. |
//! | `binary-size`   | fmt         | fixnum | Packed byte length. |
//!
//! ```scheme
//! (import (crab binary))
//! (binary-pack ">ihb" 1000000 -5 65)   ; => 7-byte bytevector
//! (binary-unpack ">ihb" (binary-pack ">ihb" 1000000 -5 65))  ; => (1000000 -5 65)
//! (binary-size ">ihb")                 ; => 7
//! ```

use std::sync::Arc;

use cs_core::Value;
use cs_ffi::error::FfiError;
use cs_ffi::host::{HostProcedure, UntypedProc};

pub fn procs() -> Vec<Arc<dyn HostProcedure>> {
    vec![
        UntypedProc::new("binary-pack", binary_pack),
        UntypedProc::new("binary-unpack", binary_unpack),
        UntypedProc::new("binary-size", binary_size),
    ]
}

// ----- helpers -----

fn arity(name: &str, want: &str, got: usize) -> FfiError {
    FfiError::ArityError {
        name: name.into(),
        expected: want.into(),
        got,
    }
}

fn fail(msg: String) -> FfiError {
    FfiError::HostFailure(msg)
}

fn expect_string(name: &str, args: &[Value], idx: usize) -> Result<String, FfiError> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.borrow().clone()),
        Some(other) => Err(FfiError::TypeMismatch {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        None => Err(arity(name, &format!(">= {}", idx + 1), args.len())),
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Big,
    Little,
}

struct Field {
    code: char,
    endian: Endian,
}

fn parse_format(fmt: &str) -> Result<Vec<Field>, FfiError> {
    let mut endian = Endian::Big;
    let mut fields = Vec::new();
    for c in fmt.chars() {
        match c {
            '>' | '!' => endian = Endian::Big,
            '<' => endian = Endian::Little,
            ' ' => {}
            'b' | 'B' | 'h' | 'H' | 'i' | 'I' | 'q' | 'Q' | 'f' | 'd' => {
                fields.push(Field { code: c, endian })
            }
            other => return Err(fail(format!("binary: unknown format char {:?}", other))),
        }
    }
    Ok(fields)
}

fn code_size(code: char) -> usize {
    match code {
        'b' | 'B' => 1,
        'h' | 'H' => 2,
        'i' | 'I' | 'f' => 4,
        'q' | 'Q' | 'd' => 8,
        _ => 0,
    }
}

fn is_float(code: char) -> bool {
    code == 'f' || code == 'd'
}

fn is_signed(code: char) -> bool {
    code.is_ascii_lowercase()
}

/// Inclusive value range for an integer code, as i128 to hold u64's top.
fn int_range(code: char) -> (i128, i128) {
    match code {
        'b' => (i8::MIN as i128, i8::MAX as i128),
        'B' => (0, u8::MAX as i128),
        'h' => (i16::MIN as i128, i16::MAX as i128),
        'H' => (0, u16::MAX as i128),
        'i' => (i32::MIN as i128, i32::MAX as i128),
        'I' => (0, u32::MAX as i128),
        'q' => (i64::MIN as i128, i64::MAX as i128),
        'Q' => (0, u64::MAX as i128),
        _ => (0, 0),
    }
}

// ----- pack -----

fn write_int(out: &mut Vec<u8>, v: i64, size: usize, endian: Endian) {
    let be = v.to_be_bytes(); // 8 bytes, MSB first
    let low = &be[8 - size..]; // low `size` bytes, big-endian order
    match endian {
        Endian::Big => out.extend_from_slice(low),
        Endian::Little => out.extend(low.iter().rev()),
    }
}

fn write_float(out: &mut Vec<u8>, v: f64, code: char, endian: Endian) {
    let bytes: Vec<u8> = if code == 'f' {
        (v as f32).to_be_bytes().to_vec()
    } else {
        v.to_be_bytes().to_vec()
    };
    match endian {
        Endian::Big => out.extend_from_slice(&bytes),
        Endian::Little => out.extend(bytes.iter().rev()),
    }
}

fn binary_pack(args: &[Value]) -> Result<Value, FfiError> {
    if args.is_empty() {
        return Err(arity("binary-pack", ">= 1", args.len()));
    }
    let fmt = expect_string("binary-pack", args, 0)?;
    let fields = parse_format(&fmt)?;
    let values = &args[1..];
    if values.len() != fields.len() {
        return Err(fail(format!(
            "binary-pack: format needs {} value(s), got {}",
            fields.len(),
            values.len()
        )));
    }
    let mut out = Vec::new();
    for (field, val) in fields.iter().zip(values) {
        if is_float(field.code) {
            let f = match val {
                Value::Number(n) => n.to_f64(),
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "number (float field)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            write_float(&mut out, f, field.code, field.endian);
        } else {
            let v = match val {
                Value::Number(cs_core::Number::Fixnum(v)) => *v,
                other => {
                    return Err(FfiError::TypeMismatch {
                        expected: "fixnum (integer field)",
                        got: other.type_name().to_string(),
                    })
                }
            };
            let (lo, hi) = int_range(field.code);
            if (v as i128) < lo || (v as i128) > hi {
                return Err(fail(format!(
                    "binary-pack: {} out of range for '{}'",
                    v, field.code
                )));
            }
            write_int(&mut out, v, code_size(field.code), field.endian);
        }
    }
    Ok(Value::ByteVector(cs_core::Gc::new(
        std::cell::RefCell::new(out),
    )))
}

// ----- unpack -----

fn read_int(bytes: &[u8], signed: bool, endian: Endian) -> i64 {
    let size = bytes.len();
    let be: Vec<u8> = match endian {
        Endian::Big => bytes.to_vec(),
        Endian::Little => bytes.iter().rev().cloned().collect(),
    };
    let mut buf = [0u8; 8];
    buf[8 - size..].copy_from_slice(&be);
    let mut v = i64::from_be_bytes(buf);
    if signed && size < 8 {
        let bits = size * 8;
        let sign_bit = 1i64 << (bits - 1);
        if v & sign_bit != 0 {
            v |= !((1i64 << bits) - 1);
        }
    }
    v
}

fn read_float(bytes: &[u8], code: char, endian: Endian) -> f64 {
    let be: Vec<u8> = match endian {
        Endian::Big => bytes.to_vec(),
        Endian::Little => bytes.iter().rev().cloned().collect(),
    };
    if code == 'f' {
        let mut b = [0u8; 4];
        b.copy_from_slice(&be);
        f32::from_be_bytes(b) as f64
    } else {
        let mut b = [0u8; 8];
        b.copy_from_slice(&be);
        f64::from_be_bytes(b)
    }
}

fn binary_unpack(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 2 {
        return Err(arity("binary-unpack", "2", args.len()));
    }
    let fmt = expect_string("binary-unpack", args, 0)?;
    let bv = match &args[1] {
        Value::ByteVector(b) => b.borrow().clone(),
        other => {
            return Err(FfiError::TypeMismatch {
                expected: "bytevector",
                got: other.type_name().to_string(),
            })
        }
    };
    let fields = parse_format(&fmt)?;
    let need: usize = fields.iter().map(|f| code_size(f.code)).sum();
    if bv.len() < need {
        return Err(fail(format!(
            "binary-unpack: need {} bytes, got {}",
            need,
            bv.len()
        )));
    }
    let mut out = Vec::with_capacity(fields.len());
    let mut off = 0;
    for field in &fields {
        let size = code_size(field.code);
        let slice = &bv[off..off + size];
        off += size;
        if is_float(field.code) {
            out.push(Value::flonum(read_float(slice, field.code, field.endian)));
        } else {
            out.push(Value::fixnum(read_int(
                slice,
                is_signed(field.code),
                field.endian,
            )));
        }
    }
    Ok(Value::list(out))
}

fn binary_size(args: &[Value]) -> Result<Value, FfiError> {
    if args.len() != 1 {
        return Err(arity("binary-size", "1", args.len()));
    }
    let fmt = expect_string("binary-size", args, 0)?;
    let fields = parse_format(&fmt)?;
    let total: usize = fields.iter().map(|f| code_size(f.code)).sum();
    Ok(Value::fixnum(total as i64))
}
