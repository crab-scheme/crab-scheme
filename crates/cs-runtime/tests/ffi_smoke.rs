//! End-to-end smoke tests for `Runtime::register_host_procedure`
//! (M5b iter 2). Each test registers a Rust procedure, evaluates a
//! tiny Scheme program that calls it, and asserts the round-trip.
//!
//! Both walker and VM tiers run every test so we catch tier-specific
//! dispatch bugs early.

use std::sync::Arc;

use cs_core::{Number, Value};
use cs_ffi::{FfiError, FromValue, IntoValue, UntypedProc};
use cs_runtime::Runtime;

/// Convenience: register on a fresh Runtime, eval the source, and
/// return the result (after both tiers verified to agree).
fn eval_with_proc(proc: Arc<dyn cs_ffi::HostProcedure>, src: &str) -> (Value, Value) {
    let mut rt_walker = Runtime::new();
    rt_walker.register_host_procedure(proc.clone());
    let walker = rt_walker
        .eval_str("<test>", src)
        .expect("walker eval succeeds");

    let mut rt_vm = Runtime::new();
    rt_vm.register_host_procedure(proc);
    let vm = rt_vm
        .eval_str_via_vm("<test>", src)
        .expect("vm eval succeeds");

    (walker, vm)
}

#[test]
fn registered_proc_is_callable() {
    let p = UntypedProc::new("rust-add", |args| {
        let a = i64::from_value(&args[0])?;
        let b = i64::from_value(&args[1])?;
        Ok((a + b).into_value())
    });
    let (walker, vm) = eval_with_proc(p, "(rust-add 2 3)");
    match walker {
        Value::Fixnum(5) => {}
        other => panic!("walker: expected 5, got {:?}", other),
    }
    match vm {
        Value::Fixnum(5) => {}
        other => panic!("vm: expected 5, got {:?}", other),
    }
}

#[test]
fn type_mismatch_caught_via_with_exception_handler() {
    let p = UntypedProc::new("strict-add", |args| {
        let a = i64::from_value(&args[0])?;
        let b = i64::from_value(&args[1])?;
        Ok((a + b).into_value())
    });
    let src = r#"
        (call/cc
          (lambda (k)
            (with-exception-handler
              (lambda (c) (k 'caught))
              (lambda () (strict-add 1 "not-a-number")))))
    "#;
    let (walker, vm) = eval_with_proc(p, src);
    match walker {
        Value::Symbol(_) => {}
        other => panic!("walker: expected 'caught, got {:?}", other),
    }
    match vm {
        Value::Symbol(_) => {}
        other => panic!("vm: expected 'caught, got {:?}", other),
    }
}

#[test]
fn host_failure_caught_via_with_exception_handler() {
    let p = UntypedProc::new("always-fails", |_| {
        Err(FfiError::HostFailure("boom".into()))
    });
    let src = r#"
        (call/cc
          (lambda (k)
            (with-exception-handler
              (lambda (c) (k (error-object-message c)))
              (lambda () (always-fails)))))
    "#;
    let (walker, vm) = eval_with_proc(p, src);
    match walker {
        Value::String(s) => assert!(s.borrow().contains("boom")),
        other => panic!("walker: expected string, got {:?}", other),
    }
    match vm {
        Value::String(s) => assert!(s.borrow().contains("boom")),
        other => panic!("vm: expected string, got {:?}", other),
    }
}

#[test]
fn proc_returning_string_round_trips() {
    let p = UntypedProc::new("greet", |args| {
        let name = String::from_value(&args[0])?;
        Ok(format!("hello, {}!", name).into_value())
    });
    let (walker, vm) = eval_with_proc(p, r#"(greet "world")"#);
    match walker {
        Value::String(s) => assert_eq!(*s.borrow(), "hello, world!"),
        other => panic!("walker: expected string, got {:?}", other),
    }
    match vm {
        Value::String(s) => assert_eq!(*s.borrow(), "hello, world!"),
        other => panic!("vm: expected string, got {:?}", other),
    }
}

#[test]
fn proc_taking_list_round_trips() {
    let p = UntypedProc::new("sum", |args| {
        let xs: Vec<i64> = Vec::<i64>::from_value(&args[0])?;
        Ok(xs.iter().sum::<i64>().into_value())
    });
    let (walker, vm) = eval_with_proc(p, "(sum '(1 2 3 4 5))");
    match walker {
        Value::Fixnum(15) => {}
        other => panic!("walker: expected 15, got {:?}", other),
    }
    match vm {
        Value::Fixnum(15) => {}
        other => panic!("vm: expected 15, got {:?}", other),
    }
}

#[test]
fn registered_proc_works_inside_higher_order() {
    // Map a host procedure over a list — exercises the cs-vm
    // VmHostBuiltin dispatch from inside vm_call_sync.
    let p = UntypedProc::new("rust-double", |args| {
        let n = i64::from_value(&args[0])?;
        Ok((n * 2).into_value())
    });
    let (walker, vm) = eval_with_proc(p, "(map rust-double '(1 2 3))");
    let check = |v: Value, label: &str| match v {
        Value::Pair(_) => {
            let xs: Vec<i64> = Vec::<i64>::from_value(&v).unwrap();
            assert_eq!(xs, vec![2, 4, 6], "{label}");
        }
        other => panic!("{}: expected list, got {:?}", label, other),
    };
    check(walker, "walker");
    check(vm, "vm");
}

#[test]
fn unspecified_return_works() {
    use std::sync::atomic::{AtomicI64, Ordering};
    static SIDE_EFFECT: AtomicI64 = AtomicI64::new(0);
    let p = UntypedProc::new("side-effect", |args| {
        let n = i64::from_value(&args[0])?;
        SIDE_EFFECT.fetch_add(n, Ordering::SeqCst);
        Ok(().into_value())
    });
    SIDE_EFFECT.store(0, Ordering::SeqCst);
    let (_w, _v) = eval_with_proc(p, "(side-effect 7) (side-effect 35)");
    // walker + vm each ran the program once -> 42 each, so 84 total.
    assert_eq!(SIDE_EFFECT.load(Ordering::SeqCst), 84);
}
