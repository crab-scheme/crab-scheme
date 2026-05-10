//! Tests for the `#[host_proc("name")]` proc-macro from
//! `cs-ffi-macros`. Each test declares a Rust function with the
//! macro, calls the generated `_host_proc` constructor, registers
//! the result on a Runtime, and runs Scheme code that exercises it.
//!
//! Both walker and VM tiers run every test for parity.

use cs_core::{Number, Value};
use cs_ffi_macros::host_proc;
use cs_runtime::Runtime;

#[host_proc("rust-add")]
fn rust_add(a: i64, b: i64) -> i64 {
    a + b
}

#[host_proc("rust-greet")]
fn rust_greet(name: String) -> String {
    format!("hello, {}!", name)
}

#[host_proc("rust-sum")]
fn rust_sum(xs: Vec<i64>) -> i64 {
    xs.iter().sum()
}

#[host_proc("rust-divide")]
fn rust_divide(a: f64, b: f64) -> Result<f64, String> {
    if b == 0.0 {
        Err("division by zero".to_string())
    } else {
        Ok(a / b)
    }
}

#[host_proc("rust-noop")]
fn rust_noop() {}

fn run_both(src: &str, registrations: impl Fn(&mut Runtime)) -> (Value, Value) {
    let mut rt_walker = Runtime::new();
    registrations(&mut rt_walker);
    let walker = rt_walker
        .eval_str("<test>", src)
        .expect("walker eval succeeds");

    let mut rt_vm = Runtime::new();
    registrations(&mut rt_vm);
    let vm = rt_vm
        .eval_str_via_vm("<test>", src)
        .expect("vm eval succeeds");

    (walker, vm)
}

#[test]
fn macro_generates_callable_two_arg_fn() {
    let (walker, vm) = run_both("(rust-add 17 25)", |rt| {
        rt.register_host_procedure(rust_add_host_proc());
    });
    match (walker, vm) {
        (Value::Number(Number::Fixnum(42)), Value::Number(Number::Fixnum(42))) => {}
        other => panic!("expected (42, 42) on both tiers, got {:?}", other),
    }
}

#[test]
fn macro_handles_string_arg_and_return() {
    let (walker, vm) = run_both(r#"(rust-greet "world")"#, |rt| {
        rt.register_host_procedure(rust_greet_host_proc());
    });
    let check = |v: Value, label: &str| match v {
        Value::String(s) => assert_eq!(*s.borrow(), "hello, world!", "{label}"),
        other => panic!("{}: expected string, got {:?}", label, other),
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn macro_handles_list_arg() {
    let (walker, vm) = run_both("(rust-sum '(10 20 30 40))", |rt| {
        rt.register_host_procedure(rust_sum_host_proc());
    });
    match (walker, vm) {
        (Value::Number(Number::Fixnum(100)), Value::Number(Number::Fixnum(100))) => {}
        other => panic!("expected (100, 100) on both tiers, got {:?}", other),
    }
}

#[test]
fn macro_handles_result_ok() {
    let (walker, vm) = run_both("(rust-divide 10.0 2.0)", |rt| {
        rt.register_host_procedure(rust_divide_host_proc());
    });
    let check = |v: Value, label: &str| match v {
        Value::Number(n) => {
            assert!((n.to_f64() - 5.0).abs() < 1e-9, "{label}: {:?}", n);
        }
        other => panic!("{}: expected number, got {:?}", label, other),
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn macro_handles_result_err_via_handler() {
    let src = r#"
        (call/cc
          (lambda (k)
            (with-exception-handler
              (lambda (c) (k (error-object-message c)))
              (lambda () (rust-divide 1.0 0.0)))))
    "#;
    let (walker, vm) = run_both(src, |rt| {
        rt.register_host_procedure(rust_divide_host_proc());
    });
    let check = |v: Value, label: &str| match v {
        Value::String(s) => {
            let msg = s.borrow().clone();
            assert!(msg.contains("division by zero"), "{label}: {}", msg);
        }
        other => panic!("{}: expected error message string, got {:?}", label, other),
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn macro_handles_zero_args_unit_return() {
    let (walker, vm) = run_both("(rust-noop)", |rt| {
        rt.register_host_procedure(rust_noop_host_proc());
    });
    match (walker, vm) {
        (Value::Unspecified, Value::Unspecified) => {}
        other => panic!("expected unspecified on both tiers, got {:?}", other),
    }
}

#[test]
fn macro_arity_error_caught_by_handler() {
    let src = r#"
        (call/cc
          (lambda (k)
            (with-exception-handler
              (lambda (c) (k 'caught))
              (lambda () (rust-add 1 2 3)))))
    "#;
    let (walker, vm) = run_both(src, |rt| {
        rt.register_host_procedure(rust_add_host_proc());
    });
    let check = |v: Value, label: &str| match v {
        Value::Symbol(_) => {}
        other => panic!("{}: expected 'caught, got {:?}", label, other),
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn macro_type_mismatch_caught_by_handler() {
    let src = r#"
        (call/cc
          (lambda (k)
            (with-exception-handler
              (lambda (c) (k 'caught))
              (lambda () (rust-add 1 "not-a-number")))))
    "#;
    let (walker, vm) = run_both(src, |rt| {
        rt.register_host_procedure(rust_add_host_proc());
    });
    let check = |v: Value, label: &str| match v {
        Value::Symbol(_) => {}
        other => panic!("{}: expected 'caught, got {:?}", label, other),
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn macro_proc_works_in_higher_order_context() {
    // Use the macro-generated proc inside (map ...) so we exercise
    // the VmHostBuiltin dispatch from vm_call_sync.
    let (walker, vm) = run_both("(map rust-add '(1 2 3) '(10 20 30))", |rt| {
        rt.register_host_procedure(rust_add_host_proc());
    });
    let check = |v: Value, label: &str| {
        let xs: Vec<i64> = cs_ffi::FromValue::from_value(&v).expect(label);
        assert_eq!(xs, vec![11, 22, 33], "{label}");
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn original_function_still_callable_directly() {
    // The macro must keep the original function intact so the user
    // can call it from Rust too, not only via Scheme.
    assert_eq!(rust_add(2, 3), 5);
    assert_eq!(rust_greet("Rust".into()), "hello, Rust!");
    assert_eq!(rust_sum(vec![1, 2, 3]), 6);
}
