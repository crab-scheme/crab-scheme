//! Issue #48 regression: the explicit VM / VM+JIT tiers must offer the
//! same higher-order SRFI-1/13 list/pair/string/vector builtins — and the
//! file-I/O higher-order builtins — that the walker has.
//!
//! These resolved fine in the default auto-tier-up mode (a hot caller
//! tiers up but the builtins still resolve), so the gap only bit the
//! explicit `--tier vm` / `--tier vm-jit` flags. It hid because the cargo
//! conformance harness exercises the walker tier only (in-process
//! `eval_str`). This differential runs every case on all three
//! configurations and asserts they agree.

use cs_core::{Value, WriteMode};
use cs_runtime::Runtime;

fn walker(src: &str) -> String {
    let mut rt = Runtime::new();
    let v = rt.eval_str("<t>", src).expect("walker eval");
    rt.format_value(&v, WriteMode::Write)
}

fn vm(src: &str) -> String {
    let mut rt = Runtime::new();
    let v = rt.eval_str_via_vm("<t>", src).expect("vm eval");
    rt.format_value(&v, WriteMode::Write)
}

fn vm_jit(src: &str) -> String {
    let mut rt = Runtime::new();
    rt.install_jit().expect("install jit");
    let v = rt.eval_str_via_vm("<t>", src).expect("vm+jit eval");
    rt.format_value(&v, WriteMode::Write)
}

/// Walker, VM, and VM+JIT must all produce the same printed value.
fn agree(src: &str) {
    let w = walker(src);
    assert_eq!(w, vm(src), "VM differs from walker: {src}");
    assert_eq!(w, vm_jit(src), "VM+JIT differs from walker: {src}");
}

#[test]
fn srfi1_list_higher_order_parity() {
    for src in [
        "(take-while odd? (list 1 3 5 2))",
        "(drop-while odd? (list 1 3 5 2))",
        "(find-tail even? (list 1 3 4 5))",
        "(list-index even? (list 1 3 4 5))",
        "(filter-map (lambda (x) (and (odd? x) (* x x))) (list 1 2 3 4))",
        "(filter-map + (list 1 2) (list 3 4))",
        "(append-map (lambda (x) (list x x)) (list 1 2 3))",
        "(append-map list (list 1 2) (list 3 4))",
        "(list-tabulate 4 (lambda (i) (* i i)))",
        "(reduce-right cons (quote ()) (list 1 2 3))",
    ] {
        agree(src);
    }
}

#[test]
fn srfi1_multi_value_parity() {
    // span / break / unzip2 return multiple values through the VM's
    // thread-local channel; the bridge must forward ctx.pending_values.
    for src in [
        "(call-with-values (lambda () (span odd? (list 1 3 5 2))) list)",
        "(call-with-values (lambda () (break even? (list 1 3 5 2))) list)",
        "(call-with-values (lambda () (unzip2 (list (list 1 2) (list 3 4)))) list)",
    ] {
        agree(src);
    }
}

#[test]
fn pair_string_vector_higher_order_parity() {
    for src in [
        "(pair-fold cons (quote ()) (list 1 2 3))",
        "(pair-fold-right cons (quote ()) (list 1 2 3))",
        "(string-fold cons (quote ()) \"abc\")",
        "(string-fold-right cons (quote ()) \"abc\")",
        "(string-tabulate (lambda (i) #\\a) 3)",
        "(vector-fold-right + 0 (vector 1 2 3))",
        "(unfold-right (lambda (x) (= x 0)) (lambda (x) x) (lambda (x) (- x 1)) 3)",
    ] {
        agree(src);
    }
}

#[test]
fn exact_issue_repro() {
    // The exact command from the issue body, on the explicit tiers.
    assert_eq!(vm("(take-while odd? (list 1 3 5 2))"), "(1 3 5)");
    assert_eq!(vm_jit("(take-while odd? (list 1 3 5 2))"), "(1 3 5)");
}

#[test]
fn file_output_ports_and_call_with_file_parity() {
    // write-string + write-char to a file output port, then read it back
    // via call-with-input-file — exercises the VM-tier FileOutput port fix
    // plus the call-with-*-file bridge. (The r7rs_load.scm fixture hit this
    // same path: "write-string: not an output port" under --tier vm.)
    let dir = std::env::temp_dir().join(format!(
        "cs48-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("o.txt");
    let p = p.display().to_string();
    let src = format!(
        "(let ((op (open-output-file \"{p}\"))) \
           (write-string \"hi \" op) (write-char #\\X op) (close-port op) \
           (call-with-input-file \"{p}\" (lambda (ip) (read-line ip))))"
    );
    let expect = "\"hi X\"";
    assert_eq!(walker(&src), expect, "walker");
    assert_eq!(vm(&src), expect, "vm");
    assert_eq!(vm_jit(&src), expect, "vm+jit");
    std::fs::remove_dir_all(&dir).ok();
}
