//! Native etcdserverpb hot-path codec (cw-b5w.2). Behind the `grpc`
//! feature, alongside the gRPC transport primops.
//!
//! Profiling crab-watchstore under `etcdctl check perf --load m`
//! (docs/perf-profile-load-m.md over there) showed the dominant
//! on-CPU cost is the interpreted proto3 codec in proto.scm:
//! byte-at-a-time bytevector ops + list churn inside the VM dispatch
//! loop, for every KV/Put and KV/Range. These four builtins move
//! exactly that work — decode of the two hot request messages and
//! encode of their responses — to native Rust, reading/writing the
//! payload bytes directly. The pure-Scheme codec in proto.scm stays
//! the reference implementation for every other message.
//!
//! Surface (field numbers/types mirror etcdserverpb; lists are
//! positional, in schema field order):
//!
//! ```ignore
//! (etcd-pb-decode-put bytes)
//!   => (key value lease prev_kv ignore_value ignore_lease)
//!      bv  bv    int   bool    bool         bool
//! (etcd-pb-decode-range bytes)
//!   => (key range_end limit revision sort_order sort_target
//!       serializable keys_only count_only
//!       min_mod max_mod min_create max_create)
//! (etcd-pb-encode-put-resp cluster-id member-id rev term prev-kv|#f)
//!   => PutResponse bytes; prev-kv = (key value create mod version lease)
//!      (the mvcc read-seam tuple order, NOT KeyValue field order)
//! (etcd-pb-encode-range-resp cluster-id member-id rev term kvs more count)
//!   => RangeResponse bytes; kvs = list of mvcc tuples as above
//! ```
//!
//! proto3 semantics: decode fills absent fields with 0/#f/empty-bv and
//! skips unknown fields by wire type; encode omits default values
//! (0 / false / empty bytes), matching proto.scm's pb-encode.

#![cfg(feature = "grpc")]

use std::cell::RefCell;

use cs_core::{SymbolTable, Value};

pub fn etcdpb_syms_builtins() -> Vec<(
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
)> {
    vec![
        ("etcd-pb-decode-put", b_decode_put),
        ("etcd-pb-decode-range", b_decode_range),
        ("etcd-pb-encode-put-resp", b_encode_put_resp),
        ("etcd-pb-encode-range-resp", b_encode_range_resp),
    ]
}

// ---- Value helpers ------------------------------------------------------

fn bv_value(bytes: Vec<u8>) -> Value {
    Value::ByteVector(cs_core::Gc::new(RefCell::new(bytes)))
}

fn expect_bv(v: &Value, who: &str) -> Result<Vec<u8>, String> {
    match v {
        Value::ByteVector(b) => Ok(b.borrow().clone()),
        other => Err(format!(
            "{}: expected bytevector, got {}",
            who,
            other.type_name()
        )),
    }
}

fn expect_i64(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Fixnum(n) => Ok(*n),
        other => Err(format!(
            "{}: expected fixnum, got {}",
            who,
            other.type_name()
        )),
    }
}

fn list_items(v: &Value, who: &str) -> Result<Vec<Value>, String> {
    list_items_rest(v.clone(), Vec::new(), who)
}

fn list_items_rest(mut v: Value, mut out: Vec<Value>, who: &str) -> Result<Vec<Value>, String> {
    loop {
        match v {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(p.car());
                v = p.cdr();
            }
            other => {
                return Err(format!(
                    "{}: expected proper list, got {}",
                    who,
                    other.type_name()
                ))
            }
        }
    }
}

// ---- proto3 wire reader --------------------------------------------------

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self, who: &str) -> Result<u64, String> {
        let mut x: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = *self
                .b
                .get(self.p)
                .ok_or_else(|| format!("{}: truncated varint", who))?;
            self.p += 1;
            if shift >= 64 {
                return Err(format!("{}: varint too long", who));
            }
            x |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(x);
            }
            shift += 7;
        }
    }

    fn len_delim(&mut self, who: &str) -> Result<&'a [u8], String> {
        let n = self.varint(who)? as usize;
        let end = self
            .p
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(|| format!("{}: truncated length-delimited field", who))?;
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }

    fn skip(&mut self, wt: u64, who: &str) -> Result<(), String> {
        match wt {
            0 => {
                self.varint(who)?;
            }
            1 => {
                self.p = (self.p + 8).min(self.b.len());
            }
            2 => {
                self.len_delim(who)?;
            }
            5 => {
                self.p = (self.p + 4).min(self.b.len());
            }
            _ => return Err(format!("{}: bad wire type {}", who, wt)),
        }
        Ok(())
    }

    fn done(&self) -> bool {
        self.p >= self.b.len()
    }
}

// ---- proto3 wire writer --------------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn put_tag(out: &mut Vec<u8>, field: u64, wt: u64) {
    put_varint(out, (field << 3) | wt);
}

/// int64/uint64 varint field; omitted when 0 (proto3 default).
fn put_int(out: &mut Vec<u8>, field: u64, n: i64) {
    if n != 0 {
        put_tag(out, field, 0);
        put_varint(out, n as u64); // negatives: full 64-bit two's complement
    }
}

/// bytes field; omitted when empty (proto3 default).
fn put_bytes(out: &mut Vec<u8>, field: u64, b: &[u8]) {
    if !b.is_empty() {
        put_tag(out, field, 2);
        put_varint(out, b.len() as u64);
        out.extend_from_slice(b);
    }
}

/// embedded message; always emitted (a present header with all-default
/// fields still needs its tag, like proto.scm's pb-encode of a message).
fn put_msg(out: &mut Vec<u8>, field: u64, m: &[u8]) {
    put_tag(out, field, 2);
    put_varint(out, m.len() as u64);
    out.extend_from_slice(m);
}

fn encode_header(cluster_id: i64, member_id: i64, rev: i64, term: i64) -> Vec<u8> {
    let mut h = Vec::with_capacity(24);
    put_int(&mut h, 1, cluster_id);
    put_int(&mut h, 2, member_id);
    put_int(&mut h, 3, rev);
    put_int(&mut h, 4, term);
    h
}

/// mvcc read-seam tuple (key value create mod version lease) -> KeyValue
/// message bytes (field order 1 key, 2 create, 3 mod, 4 version, 5 value,
/// 6 lease).
fn encode_keyvalue(tuple: &Value, who: &str) -> Result<Vec<u8>, String> {
    let t = list_items_rest(tuple.clone(), Vec::new(), who)?;
    if t.len() != 6 {
        return Err(format!(
            "{}: kv tuple must have 6 elements, got {}",
            who,
            t.len()
        ));
    }
    let key = expect_bv(&t[0], who)?;
    let value = expect_bv(&t[1], who)?;
    let create = expect_i64(&t[2], who)?;
    let modr = expect_i64(&t[3], who)?;
    let version = expect_i64(&t[4], who)?;
    let lease = expect_i64(&t[5], who)?;
    let mut kv = Vec::with_capacity(key.len() + value.len() + 24);
    put_bytes(&mut kv, 1, &key);
    put_int(&mut kv, 2, create);
    put_int(&mut kv, 3, modr);
    put_int(&mut kv, 4, version);
    put_bytes(&mut kv, 5, &value);
    put_int(&mut kv, 6, lease);
    Ok(kv)
}

// ---- builtins -------------------------------------------------------------

/// `(etcd-pb-decode-put bytes)` ->
/// `(key value lease prev_kv ignore_value ignore_lease)`
fn b_decode_put(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    const WHO: &str = "etcd-pb-decode-put";
    if args.len() != 1 {
        return Err(format!("{}: expected 1 argument, got {}", WHO, args.len()));
    }
    let bytes = match &args[0] {
        Value::ByteVector(b) => b.borrow(),
        other => {
            return Err(format!(
                "{}: expected bytevector, got {}",
                WHO,
                other.type_name()
            ))
        }
    };
    let mut r = Reader { b: &bytes, p: 0 };
    let (mut key, mut value) = (Vec::new(), Vec::new());
    let mut lease: i64 = 0;
    let (mut prev_kv, mut ignore_value, mut ignore_lease) = (false, false, false);
    while !r.done() {
        let tag = r.varint(WHO)?;
        let (field, wt) = (tag >> 3, tag & 7);
        match (field, wt) {
            (1, 2) => key = r.len_delim(WHO)?.to_vec(),
            (2, 2) => value = r.len_delim(WHO)?.to_vec(),
            (3, 0) => lease = r.varint(WHO)? as i64,
            (4, 0) => prev_kv = r.varint(WHO)? != 0,
            (5, 0) => ignore_value = r.varint(WHO)? != 0,
            (6, 0) => ignore_lease = r.varint(WHO)? != 0,
            _ => r.skip(wt, WHO)?,
        }
    }
    Ok(Value::list([
        bv_value(key),
        bv_value(value),
        Value::fixnum(lease),
        Value::Boolean(prev_kv),
        Value::Boolean(ignore_value),
        Value::Boolean(ignore_lease),
    ]))
}

/// `(etcd-pb-decode-range bytes)` -> 13-element list in schema field order.
fn b_decode_range(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    const WHO: &str = "etcd-pb-decode-range";
    if args.len() != 1 {
        return Err(format!("{}: expected 1 argument, got {}", WHO, args.len()));
    }
    let bytes = match &args[0] {
        Value::ByteVector(b) => b.borrow(),
        other => {
            return Err(format!(
                "{}: expected bytevector, got {}",
                WHO,
                other.type_name()
            ))
        }
    };
    let mut r = Reader { b: &bytes, p: 0 };
    let (mut key, mut range_end) = (Vec::new(), Vec::new());
    let mut ints = [0i64; 9]; // limit rev sort_o sort_t min_mod max_mod min_cr max_cr (+1 spare slot mapping below)
    let (mut serializable, mut keys_only, mut count_only) = (false, false, false);
    while !r.done() {
        let tag = r.varint(WHO)?;
        let (field, wt) = (tag >> 3, tag & 7);
        match (field, wt) {
            (1, 2) => key = r.len_delim(WHO)?.to_vec(),
            (2, 2) => range_end = r.len_delim(WHO)?.to_vec(),
            (3, 0) => ints[0] = r.varint(WHO)? as i64, // limit
            (4, 0) => ints[1] = r.varint(WHO)? as i64, // revision
            (5, 0) => ints[2] = r.varint(WHO)? as i64, // sort_order
            (6, 0) => ints[3] = r.varint(WHO)? as i64, // sort_target
            (7, 0) => serializable = r.varint(WHO)? != 0,
            (8, 0) => keys_only = r.varint(WHO)? != 0,
            (9, 0) => count_only = r.varint(WHO)? != 0,
            (10, 0) => ints[4] = r.varint(WHO)? as i64, // min_mod_revision
            (11, 0) => ints[5] = r.varint(WHO)? as i64, // max_mod_revision
            (12, 0) => ints[6] = r.varint(WHO)? as i64, // min_create_revision
            (13, 0) => ints[7] = r.varint(WHO)? as i64, // max_create_revision
            _ => r.skip(wt, WHO)?,
        }
    }
    Ok(Value::list([
        bv_value(key),
        bv_value(range_end),
        Value::fixnum(ints[0]),
        Value::fixnum(ints[1]),
        Value::fixnum(ints[2]),
        Value::fixnum(ints[3]),
        Value::Boolean(serializable),
        Value::Boolean(keys_only),
        Value::Boolean(count_only),
        Value::fixnum(ints[4]),
        Value::fixnum(ints[5]),
        Value::fixnum(ints[6]),
        Value::fixnum(ints[7]),
    ]))
}

/// `(etcd-pb-encode-put-resp cluster-id member-id rev term prev-kv|#f)`
fn b_encode_put_resp(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    const WHO: &str = "etcd-pb-encode-put-resp";
    if args.len() != 5 {
        return Err(format!("{}: expected 5 arguments, got {}", WHO, args.len()));
    }
    let header = encode_header(
        expect_i64(&args[0], WHO)?,
        expect_i64(&args[1], WHO)?,
        expect_i64(&args[2], WHO)?,
        expect_i64(&args[3], WHO)?,
    );
    let mut out = Vec::with_capacity(header.len() + 8);
    put_msg(&mut out, 1, &header);
    if !matches!(args[4], Value::Boolean(false)) {
        let kv = encode_keyvalue(&args[4], WHO)?;
        put_msg(&mut out, 2, &kv);
    }
    Ok(bv_value(out))
}

/// `(etcd-pb-encode-range-resp cluster-id member-id rev term kvs more count)`
fn b_encode_range_resp(args: &[Value], _syms: &mut SymbolTable) -> Result<Value, String> {
    const WHO: &str = "etcd-pb-encode-range-resp";
    if args.len() != 7 {
        return Err(format!("{}: expected 7 arguments, got {}", WHO, args.len()));
    }
    let header = encode_header(
        expect_i64(&args[0], WHO)?,
        expect_i64(&args[1], WHO)?,
        expect_i64(&args[2], WHO)?,
        expect_i64(&args[3], WHO)?,
    );
    let kvs = list_items(&args[4], WHO)?;
    let more = args[5].is_truthy();
    let count = expect_i64(&args[6], WHO)?;
    let mut out = Vec::with_capacity(header.len() + 16 + kvs.len() * 64);
    put_msg(&mut out, 1, &header);
    for t in &kvs {
        let kv = encode_keyvalue(t, WHO)?;
        put_msg(&mut out, 2, &kv);
    }
    if more {
        put_tag(&mut out, 3, 0);
        put_varint(&mut out, 1);
    }
    put_int(&mut out, 4, count);
    Ok(bv_value(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bv(b: &[u8]) -> Value {
        bv_value(b.to_vec())
    }

    fn syms() -> SymbolTable {
        SymbolTable::new()
    }

    fn as_bytes(v: &Value) -> Vec<u8> {
        match v {
            Value::ByteVector(b) => b.borrow().clone(),
            _ => panic!("not a bytevector"),
        }
    }

    fn nth(v: &Value, i: usize) -> Value {
        list_items(v, "test").unwrap()[i].clone()
    }

    #[test]
    fn decode_put_roundtrip() {
        // PutRequest{key="k", value="vv", lease=300, prev_kv=true}
        let mut m = Vec::new();
        put_bytes(&mut m, 1, b"k");
        put_bytes(&mut m, 2, b"vv");
        put_int(&mut m, 3, 300);
        put_tag(&mut m, 4, 0);
        put_varint(&mut m, 1);
        let out = b_decode_put(&[bv(&m)], &mut syms()).unwrap();
        assert_eq!(as_bytes(&nth(&out, 0)), b"k");
        assert_eq!(as_bytes(&nth(&out, 1)), b"vv");
        assert!(matches!(nth(&out, 2), Value::Fixnum(300)));
        assert!(matches!(nth(&out, 3), Value::Boolean(true)));
        assert!(matches!(nth(&out, 4), Value::Boolean(false)));
    }

    #[test]
    fn decode_skips_unknown_fields() {
        let mut m = Vec::new();
        put_bytes(&mut m, 1, b"k");
        put_int(&mut m, 99, 7); // unknown varint field
        put_bytes(&mut m, 98, b"junk"); // unknown len field
        put_bytes(&mut m, 2, b"v");
        let out = b_decode_put(&[bv(&m)], &mut syms()).unwrap();
        assert_eq!(as_bytes(&nth(&out, 0)), b"k");
        assert_eq!(as_bytes(&nth(&out, 1)), b"v");
    }

    #[test]
    fn decode_range_fields() {
        // RangeRequest{key="a", range_end="b", limit=10, revision=42, count_only=true}
        let mut m = Vec::new();
        put_bytes(&mut m, 1, b"a");
        put_bytes(&mut m, 2, b"b");
        put_int(&mut m, 3, 10);
        put_int(&mut m, 4, 42);
        put_tag(&mut m, 9, 0);
        put_varint(&mut m, 1);
        let out = b_decode_range(&[bv(&m)], &mut syms()).unwrap();
        assert_eq!(as_bytes(&nth(&out, 0)), b"a");
        assert_eq!(as_bytes(&nth(&out, 1)), b"b");
        assert!(matches!(nth(&out, 2), Value::Fixnum(10)));
        assert!(matches!(nth(&out, 3), Value::Fixnum(42)));
        assert!(matches!(nth(&out, 8), Value::Boolean(true)));
        assert!(matches!(nth(&out, 6), Value::Boolean(false)));
    }

    #[test]
    fn encode_put_resp_with_prev() {
        let prev = Value::list([
            bv(b"k"),
            bv(b"old"),
            Value::fixnum(2),
            Value::fixnum(5),
            Value::fixnum(3),
            Value::fixnum(0),
        ]);
        let out = b_encode_put_resp(
            &[
                Value::fixnum(7),
                Value::fixnum(8),
                Value::fixnum(9),
                Value::fixnum(2),
                prev,
            ],
            &mut syms(),
        )
        .unwrap();
        // header: field1 msg {1:7 2:8 3:9 4:2} = 08 07 10 08 18 09 20 02
        let expect_header: &[u8] = &[0x0a, 8, 0x08, 7, 0x10, 8, 0x18, 9, 0x20, 2];
        let got = as_bytes(&out);
        assert_eq!(&got[..expect_header.len()], expect_header);
        // prev_kv: field2 msg {1:"k" 2:2 3:5 4:3 5:"old"} (lease 0 omitted)
        let kv: &[u8] = &[
            0x12, 14, 0x0a, 1, b'k', 0x10, 2, 0x18, 5, 0x20, 3, 0x2a, 3, b'o', b'l', b'd',
        ];
        assert_eq!(&got[expect_header.len()..], kv);
    }

    #[test]
    fn encode_range_resp_kvs_more_count() {
        let t = Value::list([
            bv(b"k1"),
            bv(b"v1"),
            Value::fixnum(1),
            Value::fixnum(1),
            Value::fixnum(1),
            Value::fixnum(0),
        ]);
        let out = b_encode_range_resp(
            &[
                Value::fixnum(0), // cluster 0 -> omitted inside header
                Value::fixnum(0),
                Value::fixnum(3),
                Value::fixnum(1),
                Value::list([t]),
                Value::Boolean(true),
                Value::fixnum(5),
            ],
            &mut syms(),
        )
        .unwrap();
        let got = as_bytes(&out);
        // header {3:3 4:1}, kv msg, more=1 (field3), count=5 (field4)
        let expect: &[u8] = &[
            0x0a, 4, 0x18, 3, 0x20, 1, // header
            0x12, 14, 0x0a, 2, b'k', b'1', 0x10, 1, 0x18, 1, 0x20, 1, 0x2a, 2, b'v', b'1', 0x18,
            1, // more=true
            0x20, 5, // count=5
        ];
        assert_eq!(got, expect);
    }

    #[test]
    fn negative_int64_is_ten_byte_twos_complement() {
        let mut m = Vec::new();
        put_int(&mut m, 3, -1);
        assert_eq!(m.len(), 1 + 10); // tag + 10-byte varint
        let out = b_decode_put(&[bv(&m)], &mut syms()).unwrap();
        assert!(matches!(nth(&out, 2), Value::Fixnum(-1)));
    }
}
