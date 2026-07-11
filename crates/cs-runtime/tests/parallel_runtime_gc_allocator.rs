//! parallel-runtime spec C5.2 — `(gc-allocator v)` Scheme
//! builtin. Validates the Rust-level `allocator_tier` helper
//! across all three result branches.

use std::cell::RefCell;

use cs_core::{Hashtable, HtEqKind, Pair, Value};
use cs_gc::Gc;

use cs_runtime::builtins::allocator_tier;

#[test]
fn rc_pair_reports_rc() {
    let p = Pair::new(Value::Null, Value::Null);
    assert_eq!(allocator_tier(&Value::Pair(p)), "rc");
}

#[test]
fn rc_vector_reports_rc() {
    let v: Gc<RefCell<Vec<Value>>> = Gc::new(RefCell::new(Vec::new()));
    assert_eq!(allocator_tier(&Value::Vector(v)), "rc");
}

#[test]
fn rc_hashtable_reports_rc() {
    let h = Hashtable::new(HtEqKind::Eq);
    assert_eq!(allocator_tier(&Value::Hashtable(h)), "rc");
}

#[cfg(feature = "regions")]
#[test]
fn region_pair_reports_region() {
    let region = cs_gc::Region::new();
    let p = Pair::new_in(&region, Value::Boolean(true), Value::Null);
    assert_eq!(allocator_tier(&Value::Pair(p)), "region");
}

#[cfg(feature = "regions")]
#[test]
fn region_vector_reports_region() {
    let region = cs_gc::Region::new();
    let v: Gc<RefCell<Vec<Value>>> = Gc::new_in(&region, RefCell::new(Vec::new()));
    assert_eq!(allocator_tier(&Value::Vector(v)), "region");
}

#[test]
fn leaf_values_report_leaf() {
    // Every leaf variant — exercises the catch-all arm.
    assert_eq!(allocator_tier(&Value::Null), "leaf");
    assert_eq!(allocator_tier(&Value::Unspecified), "leaf");
    assert_eq!(allocator_tier(&Value::Eof), "leaf");
    assert_eq!(allocator_tier(&Value::Boolean(true)), "leaf");
    assert_eq!(allocator_tier(&Value::Character('z')), "leaf");
    assert_eq!(
        allocator_tier(&Value::Number(cs_core::Number::Fixnum(42))),
        "leaf"
    );
    assert_eq!(
        allocator_tier(&Value::Number(cs_core::Number::Flonum(1.5))),
        "leaf"
    );
}

#[test]
fn string_classification() {
    let s = Gc::new(RefCell::new(cs_core::CsStr::new("hello")));
    assert_eq!(allocator_tier(&Value::String(s)), "rc");
}

#[cfg(feature = "regions")]
#[test]
fn region_string_reports_region() {
    let region = cs_gc::Region::new();
    let s = Gc::new_in(&region, RefCell::new(cs_core::CsStr::new("regional")));
    assert_eq!(allocator_tier(&Value::String(s)), "region");
}
