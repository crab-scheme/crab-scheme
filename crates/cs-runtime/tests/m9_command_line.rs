//! M9 iter K — `(command-line)` honors `Runtime::set_command_line`.
//!
//! Per R6RS §6.4 the result is `(<program-path> <arg> ...)` —
//! the script path followed by post-script args. Without an
//! explicit override the runtime falls back to `std::env::args()`
//! for backward compat (REPL, `-e`, embedded use).

use cs_core::Value;
use cs_runtime::Runtime;

#[test]
fn command_line_returns_runtime_override() {
    let mut rt = Runtime::new();
    rt.set_command_line(vec!["script.scm".into(), "first".into(), "second".into()]);
    let r = rt.eval_str("<test>", "(command-line)").unwrap();
    let strs: Vec<String> = match r {
        Value::Pair(_) | Value::Null => extract_string_list(&r, &rt),
        other => panic!("expected list, got {:?}", other),
    };
    assert_eq!(strs, vec!["script.scm", "first", "second"]);
}

#[test]
fn command_line_falls_back_to_env_when_unset() {
    let mut rt = Runtime::new();
    let r = rt.eval_str("<test>", "(command-line)").unwrap();
    // Just verify it's a non-empty list of strings — the actual
    // contents depend on how cargo invoked the test runner.
    let strs = match r {
        Value::Pair(_) | Value::Null => extract_string_list(&r, &rt),
        other => panic!("expected list, got {:?}", other),
    };
    assert!(
        !strs.is_empty(),
        "fallback should at least include the test runner path"
    );
}

#[test]
fn command_line_override_visible_on_vm_tier() {
    let mut rt = Runtime::new();
    rt.set_command_line(vec!["myscript".into(), "a".into(), "b".into()]);
    let r = rt.eval_str_via_vm("<test>", "(command-line)").unwrap();
    let strs = match r {
        Value::Pair(_) | Value::Null => extract_string_list(&r, &rt),
        other => panic!("expected list, got {:?}", other),
    };
    assert_eq!(strs, vec!["myscript", "a", "b"]);
}

fn extract_string_list(v: &Value, rt: &Runtime) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                match car {
                    Value::String(s) => out.push(s.borrow().clone()),
                    other => panic!(
                        "non-string in (command-line): {}",
                        rt.format_value(&other, cs_core::WriteMode::Write)
                    ),
                }
                cur = p.cdr.borrow().clone();
            }
            other => panic!("improper list: {:?}", other),
        }
    }
    out
}
