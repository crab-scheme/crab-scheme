//! R6RS++ Phase 1.5 Iter A — `Value::Identifier { name, mark }`
//! semantics.
//!
//! Pins the new variant's behavior:
//! - eq?: identifiers compare by (name, mark) pair
//! - eqv?/equal?: same as eq? for identifiers (leaves, no
//!   structural recursion)
//! - Symbol vs Identifier mixing: always unequal (R6RS treats
//!   them as distinct types)
//! - type_name: "identifier"
//! - Display: prints the name (mark hidden in user output)
//! - format_with: same -- the mark is invisible to write/display
//!
//! Downstream iters (B-F) build creators in cs-expand and
//! upgrade `bound-identifier=?` / `free-identifier=?` /
//! `datum->syntax` to consume the mark. This test stays at the
//! cs-core layer.

use cs_core::{eq, SymbolTable, Value, WriteMode};

#[test]
fn type_name_is_identifier() {
    let mut syms = SymbolTable::new();
    let v = Value::Identifier {
        name: syms.intern("foo"),
        mark: 0,
    };
    assert_eq!(v.type_name(), "identifier");
}

#[test]
fn eq_compares_name_and_mark() {
    let mut syms = SymbolTable::new();
    let foo = syms.intern("foo");
    let bar = syms.intern("bar");

    let id_foo_0 = Value::Identifier { name: foo, mark: 0 };
    let id_foo_0_again = Value::Identifier { name: foo, mark: 0 };
    let id_foo_1 = Value::Identifier { name: foo, mark: 1 };
    let id_bar_0 = Value::Identifier { name: bar, mark: 0 };

    // Same (name, mark) -> equal.
    assert!(eq::eq(&id_foo_0, &id_foo_0_again));
    // Same name, different mark -> NOT equal (the hygiene point).
    assert!(!eq::eq(&id_foo_0, &id_foo_1));
    // Different name -> NOT equal.
    assert!(!eq::eq(&id_foo_0, &id_bar_0));
}

#[test]
fn symbol_and_identifier_with_same_name_are_unequal() {
    // R6RS distinguishes the two kinds. A bare reader-produced
    // symbol `foo` and an introduced identifier `foo` from a
    // macro expansion are not the same value even though their
    // user-visible name matches.
    let mut syms = SymbolTable::new();
    let foo = syms.intern("foo");
    let sym = Value::Symbol(foo);
    let id = Value::Identifier { name: foo, mark: 0 };
    assert!(!eq::eq(&sym, &id));
    assert!(!eq::eqv(&sym, &id));
    assert!(!eq::equal(&sym, &id));
}

#[test]
fn eqv_and_equal_match_eq_for_identifiers() {
    // Identifiers are leaves -- equal? has no structural
    // recursion to do, so it should behave exactly like eqv?
    // which behaves like eq? for identifiers.
    let mut syms = SymbolTable::new();
    let foo = syms.intern("foo");
    let id_a = Value::Identifier { name: foo, mark: 7 };
    let id_b = Value::Identifier { name: foo, mark: 7 };
    let id_c = Value::Identifier { name: foo, mark: 8 };
    assert!(eq::eq(&id_a, &id_b));
    assert!(eq::eqv(&id_a, &id_b));
    assert!(eq::equal(&id_a, &id_b));
    assert!(!eq::equal(&id_a, &id_c));
}

#[test]
fn write_hides_mark_in_user_output() {
    let mut syms = SymbolTable::new();
    let name = syms.intern("my-id");
    let id = Value::Identifier { name, mark: 42 };
    // write/display should look like the bare symbol -- the mark
    // is observable only via the hygiene predicates.
    assert_eq!(id.format_with(&syms, WriteMode::Display), "my-id");
    assert_eq!(id.format_with(&syms, WriteMode::Write), "my-id");
}

#[test]
fn debug_display_surfaces_mark() {
    // The `Display` impl (without a SymbolTable) is debug-style;
    // mark is exposed there so panics / debug dumps show it.
    let mut syms = SymbolTable::new();
    let name = syms.intern("dbg");
    let id = Value::Identifier { name, mark: 99 };
    let s = format!("{}", id);
    assert!(s.contains("identifier"), "expected debug form, got: {}", s);
    assert!(s.contains("99"), "expected mark in debug form, got: {}", s);
}

#[test]
fn is_truthy_for_identifier() {
    // Identifiers are not #f, so they're truthy.
    let mut syms = SymbolTable::new();
    let id = Value::Identifier {
        name: syms.intern("x"),
        mark: 0,
    };
    assert!(id.is_truthy());
}

#[test]
fn identifier_can_appear_inside_pair_for_structural_equal() {
    // Identifiers nested inside pairs participate in equal?
    // structural recursion correctly via the leaf eqv? check.
    let mut syms = SymbolTable::new();
    let foo = syms.intern("foo");
    let mk_id = |mark: u64| Value::Identifier { name: foo, mark };

    let list_a = Value::list([mk_id(5), Value::fixnum(1)]);
    let list_b = Value::list([mk_id(5), Value::fixnum(1)]);
    let list_c = Value::list([mk_id(6), Value::fixnum(1)]); // different mark

    assert!(eq::equal(&list_a, &list_b));
    assert!(!eq::equal(&list_a, &list_c));
}
