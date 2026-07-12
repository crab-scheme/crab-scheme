//! Integration test for `(load-shared-library)`.
//!
//! Builds `cs-ffi-example` (cdylib) on demand, dlopens the resulting
//! `.dylib`/`.so`/`.dll`, and verifies the registered host
//! procedure is callable from Scheme. This is the M5b iter 6c
//! acceptance test for FR-4 from the spec.

use std::path::PathBuf;
use std::process::Command;

use cs_core::{Number, Value};
use cs_runtime::Runtime;

/// Build cs-ffi-example via `cargo build` and return the absolute
/// path to the produced shared library.
fn build_example_dylib() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "cs-ffi-example"])
        .current_dir(workspace_root)
        .status()
        .expect("cargo build cs-ffi-example failed to start");
    assert!(status.success(), "cs-ffi-example build failed");

    let stem = "cs_ffi_example";
    let candidates = [
        workspace_root
            .join("target/debug")
            .join(format!("lib{stem}.dylib")),
        workspace_root
            .join("target/debug")
            .join(format!("lib{stem}.so")),
        workspace_root
            .join("target/debug")
            .join(format!("{stem}.dll")),
    ];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    panic!(
        "cs-ffi-example dylib not found in {}/target/debug — looked for {:?}",
        workspace_root.display(),
        candidates,
    );
}

#[test]
fn load_shared_library_registers_example_magic() {
    let path = build_example_dylib();
    let path_str = path
        .to_str()
        .expect("dylib path is non-utf8")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    let mut rt = Runtime::new();
    let prog = format!(
        "(load-shared-library \"{path}\") (example-magic)",
        path = path_str
    );
    let result = rt.eval_str("<ffi_loader>", &prog).unwrap();
    match result {
        Value::Fixnum(n) => {
            assert_eq!(n, cs_ffi_example::EXAMPLE_MAGIC_VALUE);
        }
        other => panic!("expected fixnum 42, got {:?}", other),
    }
}

#[test]
fn load_shared_library_failure_surfaces_as_condition() {
    let mut rt = Runtime::new();
    let prog = r#"
        (call/cc
          (lambda (k)
            (with-exception-handler
              (lambda (c) (k 'caught))
              (lambda () (load-shared-library "/nonexistent/path.dylib")))))
    "#;
    let result = rt.eval_str("<ffi_loader>", prog).unwrap();
    match result {
        Value::Symbol(_) => {}
        other => panic!("expected 'caught, got {:?}", other),
    }
}

#[test]
fn rust_level_load_shared_library_works_directly() {
    // Rust embedders can also call Runtime::load_shared_library
    // without going through Scheme.
    let path = build_example_dylib();
    let mut rt = Runtime::new();
    rt.load_shared_library(path.to_str().unwrap()).unwrap();
    let result = rt.eval_str("<ffi_loader>", "(example-magic)").unwrap();
    match result {
        Value::Fixnum(n) => {
            assert_eq!(n, cs_ffi_example::EXAMPLE_MAGIC_VALUE);
        }
        other => panic!("expected fixnum 42, got {:?}", other),
    }
}
