//! `crabscheme-sha2` — custom CrabScheme CLI with `(sha256 ...)`
//! pre-registered.
//!
//! Demonstrates the **static-link FFI** path. Unlike the stock
//! `crabscheme` binary (which loads plugins via
//! `(load-shared-library)`), this binary statically links
//! [`cs_ffi_sha2`] and calls
//! [`cs_runtime::Runtime::register_host_procedure`] at startup so
//! `(sha256 ...)` is available without any `load-shared-library`
//! dance.
//!
//! That matters for WASM: `dlopen` doesn't exist on `wasm32-wasip1`,
//! so dynamic plugins are off the table. Statically-linked plugins
//! work fine. The same binary built native and built for WASM
//! exposes the exact same Scheme surface.
//!
//! ## Build
//!
//! ```sh
//! # Native
//! cargo build --release -p cs-cli-sha2
//! ./target/release/crabscheme-sha2 -e '(display (sha256 "hello"))'
//!
//! # WASM (inside `devenv shell` for the wasm32-wasip1 std lib)
//! cargo build --release --target wasm32-wasip1 -p cs-cli-sha2
//! wasmtime run --dir=. target/wasm32-wasip1/release/crabscheme-sha2.wasm \
//!   -- -e '(display (sha256 "hello"))'
//! ```
//!
//! Both produce the same SHA-256 digest:
//! `2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824`.

use std::fs;
use std::process::ExitCode;

use clap::Parser;

use cs_core::{Value, WriteMode};
use cs_runtime::Runtime;

#[derive(Parser)]
#[command(
    name = "crabscheme-sha2",
    about = "CrabScheme + statically-registered (sha256) builtin via cs-ffi-sha2"
)]
struct Cli {
    /// Evaluate `EXPR` and print the result.
    #[arg(short, long)]
    eval: Option<String>,
    /// Run a Scheme source file.
    file: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let mut rt = Runtime::new();
    // Static FFI registration — equivalent to
    // `(load-shared-library)` on native but works in WASM where
    // dlopen doesn't.
    rt.register_host_procedure(cs_ffi_sha2::make_sha256_proc());

    let (src, name) = match (cli.eval, cli.file) {
        (Some(expr), None) => (expr, "<command-line>".to_string()),
        (None, Some(path)) => match fs::read_to_string(&path) {
            Ok(s) => (s, path),
            Err(e) => {
                eprintln!("crabscheme-sha2: cannot read {}: {}", path, e);
                return ExitCode::from(1);
            }
        },
        _ => {
            eprintln!("usage: crabscheme-sha2 [-e EXPR | FILE]");
            return ExitCode::from(2);
        }
    };

    match rt.eval_str(&name, &src) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            eprintln!("crabscheme-sha2: {}", diag.message);
            ExitCode::from(2)
        }
    }
}
