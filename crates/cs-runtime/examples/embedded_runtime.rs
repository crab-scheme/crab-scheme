//! `cargo run --example embedded_runtime`
//!
//! Demonstrates the standard CrabScheme embedding pattern from a
//! Rust application:
//!
//! 1. Build a [`Runtime`].
//! 2. Register host procedures the Scheme code can call.
//! 3. Evaluate Scheme source on either the walker or the VM tier.
//! 4. Pin Values across allocations when holding them in Rust state.
//!
//! See `docs/adr/0008-ffi-design.md` and
//! `.spec-workflow/specs/ffi/{requirements,design}.md`.

use cs_ffi::{FromValue, IntoValue, UntypedProc};
use cs_runtime::Runtime;

fn main() -> Result<(), String> {
    run().map_err(|d| format!("{:?}", d))
}

fn run() -> Result<(), cs_diag::Diagnostic> {
    let mut rt = Runtime::new();

    // Register a Rust callback as the Scheme procedure (rust-greet).
    let greet = UntypedProc::new("rust-greet", |args| {
        let name = String::from_value(&args[0])?;
        Ok(format!("hello from rust, {}!", name).into_value())
    });
    rt.register_host_procedure(greet);

    // Register a small statistics helper as (rust-sum xs).
    let sum = UntypedProc::new("rust-sum", |args| {
        let xs: Vec<i64> = Vec::<i64>::from_value(&args[0])?;
        Ok(xs.iter().sum::<i64>().into_value())
    });
    rt.register_host_procedure(sum);

    // Evaluate Scheme that calls back into Rust.
    let greeting = rt.eval_str("<embed>", r#"(rust-greet "embedder")"#)?;
    println!(
        "(rust-greet ...): {}",
        rt.format_value(&greeting, cs_core::WriteMode::Display)
    );

    let total = rt.eval_str("<embed>", "(rust-sum '(1 2 3 4 5 6 7 8 9 10))")?;
    println!(
        "(rust-sum ...): {}",
        rt.format_value(&total, cs_core::WriteMode::Display)
    );

    // Pin a Scheme value across a GC-triggering operation. The pin
    // keeps `holdme` alive even though intervening eval allocates
    // many pairs.
    let holdme = rt.eval_str("<embed>", "(list 'pinned 'across 'gc)")?;
    let pin = rt.pin(holdme.clone());
    let _churn = rt.eval_str(
        "<embed>",
        "(let loop ((i 1000) (acc '())) \
            (if (= i 0) acc (loop (- i 1) (cons i acc))))",
    )?;
    rt.collect();
    println!(
        "pinned value after GC: {}",
        rt.format_value(&pin.value(), cs_core::WriteMode::Write)
    );

    // The same program runs on the VM tier with no source change.
    let vm_total = rt.eval_str_via_vm("<embed>", "(rust-sum '(100 200 300))")?;
    println!(
        "(rust-sum ...) on VM: {}",
        rt.format_value(&vm_total, cs_core::WriteMode::Display)
    );

    Ok(())
}
