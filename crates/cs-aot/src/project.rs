//! Whole-program AOT pipeline glue.
//!
//! Where [`emit`](crate::emit) and [`emit_with`](crate::emit_with)
//! produce a single function as a Rust source `String`, this module
//! takes a slice of [`Function`]s + an entry-function pick + a
//! target directory, and writes a complete cargo project that
//! builds to a standalone binary.
//!
//! The pipeline is intentionally minimal — just enough to validate
//! that AOT can hit static-binary status for self-recursive numeric
//! kernels. Closures, globals, cross-function calls beyond
//! `CallSelf`, and runtime support for non-numeric values are out
//! of scope for iter 3 and will land alongside iter 4's bench
//! integration + the post-1.0 broader RIR coverage.
//!
//! ## What the emitted project looks like
//!
//! ```text
//! <out_dir>/
//!   Cargo.toml      — declares the package, optionally cs-vm dep
//!   src/main.rs     — all funcs + a `fn main()` shim
//! ```
//!
//! The `main()` shim parses CLI args as decimal `i64`s, encodes them
//! according to the chosen [`EmitMode`], calls the entry function,
//! decodes the result, and prints it. This lets the user invoke the
//! built binary with `./factorial 10` and see `3628800`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cs_rir::Function;

use crate::{emit_with, sanitize_ident_for_project, AotError, EmitMode};

/// Errors specific to project emission (separate from per-function
/// emit errors; those bubble through `Aot`).
#[derive(Debug)]
pub enum ProjectError {
    /// Filesystem error while writing the project skeleton.
    Io(io::Error),
    /// `entry_fn_name` didn't match any function in the input slice.
    EntryNotFound(String),
    /// The emitter rejected one of the functions; the variant carries
    /// (function name, underlying AotError).
    Emit(String, AotError),
    /// Caller passed an empty function slice.
    NoFunctions,
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Io(e) => write!(f, "cs-aot project: I/O error: {e}"),
            ProjectError::EntryNotFound(n) => {
                write!(f, "cs-aot project: entry function `{n}` not in funcs slice")
            }
            ProjectError::Emit(n, e) => {
                write!(f, "cs-aot project: emit error in function `{n}`: {e}")
            }
            ProjectError::NoFunctions => write!(f, "cs-aot project: funcs slice is empty"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<io::Error> for ProjectError {
    fn from(e: io::Error) -> Self {
        ProjectError::Io(e)
    }
}

/// Options controlling the emitted cargo project.
#[derive(Debug, Clone)]
pub struct ProjectOptions {
    /// ABI mode for emitted functions. See [`EmitMode`].
    pub mode: EmitMode,
    /// Package name written into the emitted `Cargo.toml`. Becomes
    /// the binary name as well.
    pub package_name: String,
    /// Name of the entry function (must match one of the input
    /// `Function`s). The emitted `main()` calls it with parsed CLI
    /// args. The entry's arity determines how many CLI args main
    /// requires.
    pub entry_fn_name: String,
    /// Absolute path to the cs-vm crate. Required for Nb mode (the
    /// emitted main.rs references `cs_vm::vm::NanboxValue` to encode
    /// args + decode the result). Ignored in RawI64 mode.
    pub cs_vm_path: Option<PathBuf>,
}

/// Result of a successful project emission. Caller passes
/// [`built_binary_path`](Self::built_binary_path) to `cargo build`
/// or runs the binary directly.
#[derive(Debug, Clone)]
pub struct EmittedProject {
    /// The directory the project was written to.
    pub project_dir: PathBuf,
    /// Path the `cargo build --release` will produce. Caller is
    /// responsible for actually invoking cargo.
    pub built_binary_path: PathBuf,
}

/// Write a complete cargo project to `out_dir`. The directory is
/// created (along with `src/`) if missing; existing files are
/// overwritten.
///
/// Returns paths the caller needs to drive cargo + invoke the
/// resulting binary.
pub fn emit_project(
    funcs: &[Function],
    out_dir: &Path,
    opts: &ProjectOptions,
) -> Result<EmittedProject, ProjectError> {
    if funcs.is_empty() {
        return Err(ProjectError::NoFunctions);
    }
    // Resolve the entry function up-front so we know its arity.
    let entry = funcs
        .iter()
        .find(|f| f.name == opts.entry_fn_name)
        .ok_or_else(|| ProjectError::EntryNotFound(opts.entry_fn_name.clone()))?;

    fs::create_dir_all(out_dir.join("src"))?;

    fs::write(out_dir.join("Cargo.toml"), render_cargo_toml(opts))?;
    fs::write(
        out_dir.join("src/main.rs"),
        render_main_rs(funcs, entry, opts)?,
    )?;

    Ok(EmittedProject {
        project_dir: out_dir.to_path_buf(),
        built_binary_path: out_dir
            .join("target")
            .join("release")
            .join(&opts.package_name),
    })
}

fn render_cargo_toml(opts: &ProjectOptions) -> String {
    let mut s = String::new();
    s.push_str("[package]\n");
    s.push_str(&format!("name = \"{}\"\n", opts.package_name));
    s.push_str("version = \"0.0.1\"\n");
    s.push_str("edition = \"2021\"\n\n");

    s.push_str("[dependencies]\n");
    if opts.mode == EmitMode::Nb {
        // Nb mode requires cs-vm in scope for the runtime helpers +
        // NanboxValue encode/decode in the main shim. The path is
        // emitted with absolute resolution so the project compiles
        // wherever it's dropped (no relative-path fragility).
        let cs_vm_path = opts.cs_vm_path.as_ref().expect(
            "ProjectOptions::cs_vm_path must be set when mode == Nb \
             (caller should resolve cs-vm's location before emitting)",
        );
        s.push_str(&format!(
            "cs-vm = {{ path = \"{}\" }}\n",
            cs_vm_path.display()
        ));
    }
    s.push('\n');

    s.push_str(&format!(
        "[[bin]]\nname = \"{}\"\npath = \"src/main.rs\"\n\n",
        opts.package_name
    ));

    // opt-level=3 + no LTO: matches workspace defaults for
    // dev-cycle predictability. iter-4 bench harness can override
    // by editing the emitted Cargo.toml directly.
    s.push_str("[profile.release]\nopt-level = 3\n");

    s
}

fn render_main_rs(
    funcs: &[Function],
    entry: &Function,
    opts: &ProjectOptions,
) -> Result<String, ProjectError> {
    let mut src = String::new();
    src.push_str(
        "//! AOT-emitted by cs-aot iter 3.\n\
         //!\n\
         //! Do not edit by hand — re-run cs-aot::project::emit_project to refresh.\n",
    );
    src.push_str("#![allow(unused, unused_unsafe)]\n\n");

    // Emit every Function in declaration order. Self-recursion (via
    // `CallSelf`) works because each function refers to itself by
    // its sanitized name — Rust resolves the recursive reference at
    // the module scope.
    for f in funcs {
        let body = emit_with(opts.mode, f).map_err(|e| ProjectError::Emit(f.name.clone(), e))?;
        src.push_str(&body);
        src.push('\n');
    }

    // Main shim. The shape depends on EmitMode:
    //   RawI64: parse args as i64, pass directly, print i64 result.
    //   Nb:     parse args as i64, NB-encode as Fixnum, call entry,
    //           decode NB result, print as i64.
    let entry_name = sanitize_ident_for_project(&entry.name);
    let n_params = entry.params.len();

    src.push_str("fn main() {\n");
    src.push_str("    let args: Vec<String> = std::env::args().collect();\n");
    src.push_str(&format!("    if args.len() < {} {{\n", n_params + 1));
    src.push_str(&format!(
        "        eprintln!(\"usage: {{}} {}\", args[0]);\n",
        (0..n_params)
            .map(|i| format!("<arg{i}>"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    src.push_str("        std::process::exit(2);\n");
    src.push_str("    }\n");

    let arg_exprs: Vec<String> = (0..n_params)
        .map(|i| match opts.mode {
            EmitMode::RawI64 => format!("args[{}].parse::<i64>().unwrap()", i + 1),
            EmitMode::Nb => format!(
                "cs_vm::vm::NanboxValue::fixnum(args[{}].parse::<i64>().unwrap()).into_raw()",
                i + 1
            ),
        })
        .collect();

    src.push_str(&format!(
        "    let raw_result: i64 = {entry_name}({});\n",
        arg_exprs.join(", ")
    ));
    match opts.mode {
        EmitMode::RawI64 => {
            src.push_str("    println!(\"{}\", raw_result);\n");
        }
        EmitMode::Nb => {
            src.push_str("    let nb = cs_vm::vm::NanboxValue(raw_result);\n");
            src.push_str(
                "    let decoded = nb.as_fixnum().expect(\"entry returned a non-Fixnum NB value\");\n",
            );
            src.push_str("    println!(\"{}\", decoded);\n");
        }
    }
    src.push_str("}\n");

    Ok(src)
}
