//! `crabscheme` binary — minimal CLI entry.

use std::fs;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use cs_core::{Value, WriteMode};
use cs_diag::{render_with, Diagnostic, SourceMap};
use cs_runtime::Runtime;

#[derive(Parser, Debug)]
#[command(
    name = "crabscheme",
    version,
    about = "CrabScheme — R6RS Scheme implementation in Rust"
)]
struct Cli {
    /// Evaluate an expression and print its value.
    #[arg(short = 'e', long = "eval", value_name = "EXPR")]
    expr: Option<String>,

    /// Execution tier: tree-walker (default) or vm (bytecode).
    #[arg(long = "tier", value_name = "TIER", default_value = "walker")]
    tier: String,

    /// When to color diagnostics: auto (TTY-dependent), always, or never.
    #[arg(long = "color", value_name = "WHEN", default_value = "auto")]
    color: String,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// Resolve the `--color` flag: 'auto' inspects whether stderr is a TTY.
fn color_enabled(flag: &str) -> bool {
    match flag {
        "always" => true,
        "never" => false,
        _ => is_stderr_tty(),
    }
}

fn is_stderr_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

fn render_diag(diag: &Diagnostic, sm: &SourceMap, color: bool) -> String {
    render_with(diag, sm, color)
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run a Scheme source file.
    Run {
        /// Path to the .scm file.
        file: String,
        /// Args passed to the script — surfaced via R6RS `(command-line)`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL.
    Repl,
    /// Ahead-of-time compile a Scheme source file to a cargo project
    /// (and optionally a native binary). RC2 iter G — accepts the
    /// subset of Scheme that lowers to cs-aot's supported RIR Insts
    /// (self-recursive numeric kernels: LoadConst/Add/Sub/Mul/Div +
    /// Lt/Eq + CallSelf + Flonum surface). Single-define-per-file
    /// programs at this iter; multi-define + top-level entry-point
    /// synthesis is post-RC2 work.
    #[cfg(feature = "aot")]
    Aot {
        /// Path to the .scm source file.
        file: String,
        /// Output directory for the emitted cargo project. Defaults
        /// to `<file-basename>-aot/` in the current directory.
        #[arg(short = 'o', long = "output", value_name = "DIR")]
        output: Option<String>,
        /// Name of the top-level (define (<entry> ...) ...) to use as
        /// the binary's entry. Defaults to the first lambda the
        /// bytecode compiler emits (typically the first top-level
        /// define).
        #[arg(long = "entry", value_name = "NAME")]
        entry: Option<String>,
        /// Also invoke `cargo build --release` on the emitted project.
        /// On success, prints the resulting native binary's path.
        #[arg(long = "build")]
        build: bool,
        /// RC2 iter R debug aid: print the entry function's RIR
        /// (post-`bytecode_to_rir_aot`) to stdout in a human-
        /// readable form. Useful when an `UnsupportedInst` error
        /// surfaces — shows which Insts the translator emitted.
        #[arg(long = "emit-rir")]
        emit_rir: bool,
        /// RC2 iter R debug aid: dump the AOT-emitted Rust source
        /// (the `src/main.rs` content) to stdout instead of (or
        /// after) writing to the output directory. Useful for
        /// inspecting what cs-aot would compile when debugging
        /// codegen issues.
        #[arg(long = "emit-rust-source")]
        emit_rust_source: bool,
    },
    /// RC3 Phase 4 iter 4.5: self-test the AOT installation. Runs
    /// a baked-in `(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))`
    /// through the full pipeline (parse → expand → compile → RIR →
    /// emit project → cargo build → run) and asserts the resulting
    /// binary returns `120` for `fact(5)`. Exit code 0 on success;
    /// non-zero (with diagnostic) on any pipeline stage failure.
    /// Useful for verifying a release-installed binary works on
    /// the user's platform before pointing it at real source files.
    #[cfg(feature = "aot")]
    AotDoctor,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let via_vm = cli.tier == "vm" || cli.tier == "vm-jit";
    let with_jit = cli.tier == "vm-jit";
    let color = color_enabled(&cli.color);

    if let Some(expr) = cli.expr {
        return run_eval(&expr, via_vm, with_jit, color);
    }

    match cli.cmd {
        Some(Cmd::Run { file, args }) => run_file(&file, &args, via_vm, with_jit, color),
        Some(Cmd::Repl) | None => run_repl(via_vm, color),
        #[cfg(feature = "aot")]
        Some(Cmd::Aot {
            file,
            output,
            entry,
            build,
            emit_rir,
            emit_rust_source,
        }) => run_aot(
            &file,
            output.as_deref(),
            entry.as_deref(),
            build,
            emit_rir,
            emit_rust_source,
        ),
        #[cfg(feature = "aot")]
        Some(Cmd::AotDoctor) => run_aot_doctor(),
    }
}

/// RC3 Phase 4 iter 4.5: AOT installation self-test.
///
/// Pipeline check matrix:
///
/// | Step                    | What goes wrong if this fails       |
/// |-------------------------|-------------------------------------|
/// | 1. parse                | cs-parse not in cs-cli's deps       |
/// | 2. expand               | cs-expand not in cs-cli's deps      |
/// | 3. compile              | cs-vm not in cs-cli's deps          |
/// | 4. bytecode_to_rir_aot  | translator bug or RIR variant gap   |
/// | 5. emit_project         | cs-aot Inst-coverage gap            |
/// | 6. resolve cs-vm dep    | path = unreachable from this binary |
/// | 7. cargo build          | rust toolchain / cargo install      |
/// | 8. run + verify         | NB ABI / runtime helper mismatch    |
///
/// Prints each step's result; exits 0 if all pass.
#[cfg(feature = "aot")]
fn run_aot_doctor() -> ExitCode {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Command;

    use cs_aot::project::{emit_project, ProjectOptions};
    use cs_aot::EmitMode;
    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;
    use cs_vm::compiler::PrimOp;
    use cs_vm::{compile_with_globals_and_primops, jit_translate::bytecode_to_rir_aot};

    println!("crabscheme aot-doctor: self-test");
    println!();

    let src = "(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))";
    let mut step = 0;
    let mut report = |label: &str, ok: bool, detail: &str| {
        step += 1;
        let badge = if ok { "  OK  " } else { " FAIL " };
        println!(
            "[{badge}] step {step}: {label}{}",
            if detail.is_empty() {
                "".to_string()
            } else {
                format!(" — {detail}")
            }
        );
    };
    let bail = |msg: &str| -> ExitCode {
        eprintln!("\ncrabscheme aot-doctor: failed: {msg}");
        ExitCode::from(1)
    };

    // ---- Steps 1-3: source → bytecode -----
    let mut sources = SourceMap::new();
    let file_id = sources.add("<doctor>", src);
    let mut syms = SymbolTable::new();
    let data = match read_all(file_id, src, &mut syms) {
        Ok(d) => {
            report("parse", true, &format!("{} datum(s)", d.len()));
            d
        }
        Err(e) => {
            report("parse", false, &e[0].message());
            return bail("parse failed");
        }
    };

    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = match expander.expand_program(&data) {
        Ok(c) => {
            report("expand", true, "");
            c
        }
        Err(e) => {
            let m = e.message().to_string();
            report("expand", false, &m);
            return bail("expand failed");
        }
    };
    drop(expander);

    let globals = HashMap::new();
    let primops = {
        let mut m = HashMap::new();
        for (op, kind) in &[
            ("+", PrimOp::Add),
            ("-", PrimOp::Sub),
            ("*", PrimOp::Mul),
            ("=", PrimOp::Eq),
            ("<", PrimOp::Lt),
        ] {
            m.insert(syms.intern(op), *kind);
        }
        m
    };
    let bc = match compile_with_globals_and_primops(&core, &globals, &primops) {
        Ok(b) => {
            report(
                "compile",
                true,
                &format!(
                    "{} top-level inst(s), {} lambda(s)",
                    bc_inst_count(&b),
                    b.lambdas.len()
                ),
            );
            b
        }
        Err(e) => {
            report("compile", false, &e.message);
            return bail("compile failed");
        }
    };

    // ---- Step 4: bytecode → RIR -----
    let fact_sym = syms.intern("fact");
    let lam = match bc.lambdas.first() {
        Some(l) => l,
        None => {
            report("bytecode_to_rir", false, "no lambdas in bytecode");
            return bail("no lambdas — compile may have folded fact away");
        }
    };
    let rir = match bytecode_to_rir_aot(lam, "fact", Some(fact_sym)) {
        Ok(r) => {
            report(
                "bytecode_to_rir_aot",
                true,
                &format!("{} block(s)", r.blocks.len()),
            );
            r
        }
        Err(e) => {
            report("bytecode_to_rir_aot", false, &format!("{e:?}"));
            return bail("bytecode→RIR failed");
        }
    };

    // ---- Step 5: emit_project -----
    let cs_vm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .join("cs-vm");
    let cs_vm_present = cs_vm_path.exists();
    report(
        "resolve cs-vm dep",
        cs_vm_present,
        &cs_vm_path.display().to_string(),
    );
    if !cs_vm_present {
        return bail(
            "cs-vm not at the expected dev-tree path — release-installed binaries can't AOT yet (Phase 1.3 crates.io publish addresses this)",
        );
    }

    let tmpdir = std::env::temp_dir().join(format!("crabscheme-aot-doctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmpdir);
    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: "doctor_fact".to_string(),
        entry_fn_name: "fact".to_string(),
        cs_vm_dep: None,
        cs_vm_path: Some(cs_vm_path),
    };
    let emitted = match emit_project(&[rir], &tmpdir, &opts) {
        Ok(e) => {
            report("emit_project", true, &e.project_dir.display().to_string());
            e
        }
        Err(e) => {
            report("emit_project", false, &format!("{e}"));
            return bail("project emit failed");
        }
    };

    // ---- Step 7: cargo build ------
    println!("  ...running cargo build --release (may take ~10s on a cold cache)...");
    let build_status = Command::new("cargo")
        .current_dir(&emitted.project_dir)
        .arg("build")
        .arg("--release")
        .status();
    match build_status {
        Ok(s) if s.success() => report("cargo build --release", true, ""),
        Ok(s) => {
            report("cargo build --release", false, &format!("exit {s}"));
            return bail("cargo build failed — is the rust toolchain installed?");
        }
        Err(e) => {
            report("cargo build --release", false, &format!("spawn: {e}"));
            return bail("cargo not on PATH");
        }
    }

    // ---- Step 8: run + verify ------
    let bin = &emitted.built_binary_path;
    let out = match Command::new(bin).arg("5").output() {
        Ok(o) => o,
        Err(e) => {
            report("run AOT binary", false, &format!("spawn: {e}"));
            return bail("binary couldn't be spawned");
        }
    };
    if !out.status.success() {
        report("run AOT binary", false, &format!("exit {}", out.status));
        return bail("binary exited non-zero");
    }
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let want = "120";
    if got == want {
        report("verify fact(5) = 120", true, "");
    } else {
        report("verify fact(5) = 120", false, &format!("got {got:?}"));
        return bail("AOT binary returned wrong result — silent codegen bug");
    }

    println!();
    println!("crabscheme aot-doctor: all checks passed.");
    println!("  AOT-compiled binary at: {}", bin.display());
    ExitCode::SUCCESS
}

#[cfg(feature = "aot")]
fn bc_inst_count(bc: &cs_vm::opcode::Bytecode) -> usize {
    bc.insts.len()
}

#[cfg(feature = "aot")]
fn run_aot(
    file: &str,
    output: Option<&str>,
    entry: Option<&str>,
    build: bool,
    emit_rir: bool,
    emit_rust_source: bool,
) -> ExitCode {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Command;

    use cs_aot::project::{emit_project, ProjectOptions};
    use cs_aot::EmitMode;
    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;
    use cs_vm::compiler::PrimOp;
    use cs_vm::{compile_with_globals_and_primops, jit_translate::bytecode_to_rir_aot};

    // --- Read source ----
    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme aot: cannot read {file}: {e}");
            return ExitCode::from(1);
        }
    };

    // --- Lex + parse + expand + compile ----
    //
    // Mirrors the same pipeline as cs-runtime's eval_str_via_vm but
    // stops at Bytecode rather than executing. We can't reuse
    // Runtime::eval_str_via_vm directly because it runs the bytecode;
    // the AOT path keeps the bytecode as data to translate.
    let mut sources = SourceMap::new();
    let file_id = sources.add(file, &src);
    let mut syms = SymbolTable::new();

    let data = match read_all(file_id, &src, &mut syms) {
        Ok(d) => d,
        Err(errs) => {
            let e = &errs[0];
            eprintln!("crabscheme aot: parse error: {}", e.message());
            return ExitCode::from(2);
        }
    };

    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = match expander.expand_program(&data) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("crabscheme aot: expand error: {}", e.message());
            return ExitCode::from(2);
        }
    };
    drop(expander);

    let globals = HashMap::new();
    // Primop table mirrors cs-runtime's; without it `(+ a b)` compiles
    // to a generic Call and the bytecode→RIR translator emits Insts
    // cs-aot doesn't yet handle.
    let primops = {
        let mut m = HashMap::new();
        m.insert(syms.intern("+"), PrimOp::Add);
        m.insert(syms.intern("-"), PrimOp::Sub);
        m.insert(syms.intern("*"), PrimOp::Mul);
        m.insert(syms.intern("<"), PrimOp::Lt);
        m.insert(syms.intern("<="), PrimOp::Le);
        m.insert(syms.intern(">"), PrimOp::Gt);
        m.insert(syms.intern(">="), PrimOp::Ge);
        m.insert(syms.intern("="), PrimOp::Eq);
        m
    };
    let bc = match compile_with_globals_and_primops(&core, &globals, &primops) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("crabscheme aot: compile error: {}", e.message);
            return ExitCode::from(2);
        }
    };

    if bc.lambdas.is_empty() {
        eprintln!(
            "crabscheme aot: no top-level lambdas in {file} — AOT requires \
             at least one (define (name args...) body) form"
        );
        return ExitCode::from(2);
    }

    // --- Pick the entry lambda.
    //
    // iter G default: `bc.lambdas[0]`. iter H: walk the top-level
    // bytecode for `MakeClosure(i) + SetVar(sym)` pairs and build a
    // name → lambda-index map so `--entry NAME` picks the right
    // function in multi-define files. When `--entry` isn't given we
    // try the file's basename first (matching the common convention
    // of `foo.scm` defining `(define (foo ...) ...)`); on miss we
    // fall back to the first defined lambda and warn that we
    // re-resolved.
    let available = lambda_names_in_top_level(&bc, &syms);
    let (entry_name, entry_sym, lam) = match entry {
        Some(want) => match lambda_index_for(&bc, syms.intern(want)) {
            Some(idx) => (want.to_string(), syms.intern(want), bc.lambdas[idx].clone()),
            None => {
                eprintln!(
                    "crabscheme aot: entry `{want}` not found; available: {available:?}\n\
                     hint: pick one with `--entry NAME`"
                );
                return ExitCode::from(2);
            }
        },
        None => {
            let basename = basename_no_ext(file).to_string();
            match lambda_index_for(&bc, syms.intern(&basename)) {
                Some(idx) => (
                    basename.clone(),
                    syms.intern(&basename),
                    bc.lambdas[idx].clone(),
                ),
                None if !available.is_empty() => {
                    // Fall back to the first defined lambda. Use
                    // its actual name so CallSelf inside the body
                    // resolves correctly.
                    let actual = available[0].clone();
                    if actual != basename {
                        eprintln!(
                            "crabscheme aot: file basename `{basename}` doesn't match \
                             any top-level define; defaulting to `{actual}` \
                             (available: {available:?})"
                        );
                    }
                    let actual_sym = syms.intern(&actual);
                    (actual, actual_sym, bc.lambdas[0].clone())
                }
                None => {
                    eprintln!("crabscheme aot: no top-level (define (NAME ...) ...) found");
                    return ExitCode::from(2);
                }
            }
        }
    };

    // --- Translate to RIR ----
    let rir = match bytecode_to_rir_aot(&lam, &entry_name, Some(entry_sym)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("crabscheme aot: bytecode→RIR error: {e:?}");
            eprintln!(
                "  (the translator emits Insts cs-aot doesn't yet handle for \
                 some Scheme constructs — see docs/milestones/m10-trackA-exit.md \
                 for the supported-Inst list)"
            );
            return ExitCode::from(3);
        }
    };

    // RC2 iter R: --emit-rir dumps the post-translate RIR to stdout
    // before emission. Useful when an UnsupportedInst surfaces:
    // shows exactly which Insts the translator emitted.
    if emit_rir {
        println!("// --- cs-aot RIR for `{entry_name}` ---");
        println!("// params: {:?}", rir.params);
        println!("// return_type: {:?}", rir.return_type);
        println!("// entry: {:?}", rir.entry);
        for block in &rir.blocks {
            println!("\n// {:?}:", block.id);
            if !block.params.is_empty() {
                println!("//   params: {:?}", block.params);
            }
            for inst in &block.insts {
                println!("//   {inst:?}");
            }
            println!("//   TERM: {:?}", block.terminator);
        }
        println!("// --- end RIR ---");
    }

    // --- Output dir + package name ----
    let out_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}-aot", basename_no_ext(file))));

    // Resolve cs-vm path relative to this binary's manifest — at
    // CARGO_MANIFEST_DIR build time, cs-vm sits at ../cs-vm in the
    // workspace. For end-user binaries shipped via the release
    // workflow, this path won't resolve at runtime; the emitted
    // Cargo.toml then needs cs-vm published to crates.io, which is
    // post-RC2 packaging work. For dev-loop usage from a built-from-
    // source binary the path is correct.
    let cs_vm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .join("cs-vm");
    if !cs_vm_path.exists() {
        eprintln!(
            "crabscheme aot: expected cs-vm at {} — \
             this binary's emitted project depends on cs-vm at that path. \
             AOT from a release-installed binary needs cs-vm published to \
             crates.io (post-RC2 packaging work).",
            cs_vm_path.display()
        );
        return ExitCode::from(4);
    }

    let pkg_name = sanitize_pkg_name(&entry_name);
    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: pkg_name.clone(),
        entry_fn_name: entry_name.clone(),
        cs_vm_dep: None, // fall through to legacy cs_vm_path
        cs_vm_path: Some(cs_vm_path),
    };

    let emitted = match emit_project(&[rir], &out_dir, &opts) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("crabscheme aot: project emit error: {e}");
            return ExitCode::from(4);
        }
    };

    println!(
        "crabscheme aot: emitted project at {}",
        emitted.project_dir.display()
    );
    println!("  entry: {entry_name}");
    println!("  package: {pkg_name}");

    // RC2 iter R: --emit-rust-source prints the generated src/main.rs
    // after emit. Useful when the resulting cargo build fails — lets
    // the user see exactly what got compiled.
    if emit_rust_source {
        let src_path = emitted.project_dir.join("src/main.rs");
        match fs::read_to_string(&src_path) {
            Ok(s) => {
                println!("// --- cs-aot src/main.rs ({}) ---", src_path.display());
                println!("{s}");
                println!("// --- end main.rs ---");
            }
            Err(e) => {
                eprintln!(
                    "crabscheme aot: --emit-rust-source: cannot read {}: {e}",
                    src_path.display()
                );
            }
        }
    }

    if !build {
        println!("  (re-run with --build to invoke `cargo build --release`)");
        return ExitCode::SUCCESS;
    }

    println!("  building (cargo build --release)...");
    let status = Command::new("cargo")
        .current_dir(&emitted.project_dir)
        .arg("build")
        .arg("--release")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("  built: {}", emitted.built_binary_path.display());
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("crabscheme aot: cargo build failed (exit {})", s);
            ExitCode::from(5)
        }
        Err(e) => {
            eprintln!("crabscheme aot: cargo not found / failed to spawn: {e}");
            ExitCode::from(5)
        }
    }
}

#[cfg(feature = "aot")]
fn basename_no_ext(path: &str) -> &str {
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("aot");
    stem
}

/// Walk top-level bytecode for `MakeClosure(i) + SetVar(sym)` pairs
/// and return the lambda index bound to `target_sym`, if any. The
/// adjacency check keeps the matcher tight — see the matching
/// helper in `crates/cs-aot/tests/source_pipeline.rs` for rationale.
#[cfg(feature = "aot")]
fn lambda_index_for(bc: &cs_vm::opcode::Bytecode, target_sym: cs_core::Symbol) -> Option<usize> {
    for window in bc.insts.windows(2) {
        if let (cs_vm::opcode::Inst::MakeClosure(idx), cs_vm::opcode::Inst::SetVar(sym)) =
            (&window[0], &window[1])
        {
            if *sym == target_sym {
                return Some(*idx);
            }
        }
    }
    None
}

/// Enumerate all top-level-bound lambda names — used to render
/// useful diagnostics ("available: [...]") when `--entry NAME`
/// doesn't match anything.
#[cfg(feature = "aot")]
fn lambda_names_in_top_level(
    bc: &cs_vm::opcode::Bytecode,
    syms: &cs_core::SymbolTable,
) -> Vec<String> {
    let mut names = Vec::new();
    for window in bc.insts.windows(2) {
        if let (cs_vm::opcode::Inst::MakeClosure(_), cs_vm::opcode::Inst::SetVar(sym)) =
            (&window[0], &window[1])
        {
            names.push(syms.name(*sym).to_string());
        }
    }
    names
}

#[cfg(feature = "aot")]
fn sanitize_pkg_name(s: &str) -> String {
    // Cargo package names: lowercase letters, digits, underscores,
    // hyphens. Replace anything else with underscore; collapse
    // double-underscores; prefix with `aot_` if empty / starts with
    // digit.
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() || out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("aot_{out}")
    } else {
        out
    }
}

fn eval_with_tier(
    rt: &mut Runtime,
    name: &str,
    src: &str,
    via_vm: bool,
) -> Result<Value, cs_diag::Diagnostic> {
    if via_vm {
        rt.eval_str_via_vm(name, src)
    } else {
        rt.eval_str(name, src)
    }
}

fn run_eval(src: &str, via_vm: bool, with_jit: bool, color: bool) -> ExitCode {
    let mut rt = Runtime::new();
    if with_jit {
        #[cfg(feature = "jit")]
        if let Err(e) = rt.install_jit() {
            eprintln!("crabscheme: failed to install JIT: {e}");
            return ExitCode::from(1);
        }
        #[cfg(not(feature = "jit"))]
        {
            eprintln!("crabscheme: --tier vm-jit requested but binary built without `jit` feature");
            return ExitCode::from(1);
        }
    }
    match eval_with_tier(&mut rt, "<command-line>", src, via_vm) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            let s = render_diag(&diag, rt.source_map(), color);
            eprintln!("{}", s);
            ExitCode::from(2)
        }
    }
}

fn run_file(
    path: &str,
    script_args: &[String],
    via_vm: bool,
    with_jit: bool,
    color: bool,
) -> ExitCode {
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme: cannot read {}: {}", path, e);
            return ExitCode::from(1);
        }
    };
    let mut rt = Runtime::new();
    if with_jit {
        #[cfg(feature = "jit")]
        if let Err(e) = rt.install_jit() {
            eprintln!("crabscheme: failed to install JIT: {e}");
            return ExitCode::from(1);
        }
        #[cfg(not(feature = "jit"))]
        {
            eprintln!("crabscheme: --tier vm-jit requested but binary built without `jit` feature");
            return ExitCode::from(1);
        }
    }
    // R6RS `(command-line)` — script path + args after it. Strip the
    // crabscheme dispatcher's own argv so user code sees the same
    // shape as `gsi script.scm a b` would.
    let mut argv: Vec<String> = Vec::with_capacity(script_args.len() + 1);
    argv.push(path.to_string());
    argv.extend(script_args.iter().cloned());
    rt.set_command_line(argv);
    match eval_with_tier(&mut rt, path, &src, via_vm) {
        Ok(v) => {
            if !matches!(v, Value::Unspecified) {
                println!("{}", rt.format_value(&v, WriteMode::Write));
            }
            ExitCode::SUCCESS
        }
        Err(diag) => {
            let s = render_diag(&diag, rt.source_map(), color);
            eprintln!("{}", s);
            ExitCode::from(2)
        }
    }
}

fn run_repl(start_via_vm: bool, color: bool) -> ExitCode {
    let mut rt = Runtime::new();
    let mut counter: u32 = 0;
    let mut via_vm = start_via_vm;
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buffer = String::new();
    println!(
        "crabscheme {} ({}) — :help for commands, ^D to exit",
        env!("CARGO_PKG_VERSION"),
        if via_vm { "vm" } else { "walker" },
    );
    loop {
        if buffer.is_empty() {
            print!("{}> ", if via_vm { "vm" } else { "" });
        } else {
            print!("… ");
        }
        let _ = stdout.flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                println!();
                return ExitCode::SUCCESS;
            }
            Ok(_) => {}
            Err(_) => return ExitCode::from(1),
        }
        // REPL command: line starts with `:` and we're not mid-expression.
        let trimmed = line.trim();
        if buffer.is_empty() && trimmed.starts_with(':') {
            match handle_repl_cmd(trimmed, &mut via_vm, &mut rt, color) {
                ReplCmdResult::Continue => {}
                ReplCmdResult::Quit => return ExitCode::SUCCESS,
            }
            continue;
        }
        buffer.push_str(&line);
        if !is_balanced(&buffer) {
            continue;
        }
        counter += 1;
        let name = format!("<repl:{}>", counter);
        let to_eval = std::mem::take(&mut buffer);
        let result = if via_vm {
            rt.eval_str_via_vm(&name, &to_eval)
        } else {
            rt.eval_str(&name, &to_eval)
        };
        match result {
            Ok(v) => {
                if !matches!(v, Value::Unspecified) {
                    println!("{}", rt.format_value(&v, WriteMode::Write));
                }
            }
            Err(diag) => {
                let s = render_diag(&diag, rt.source_map(), color);
                eprint!("{}", s);
            }
        }
    }
}

enum ReplCmdResult {
    Continue,
    Quit,
}

fn handle_repl_cmd(line: &str, via_vm: &mut bool, rt: &mut Runtime, color: bool) -> ReplCmdResult {
    let mut parts = line.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().unwrap_or("").trim();
    match cmd {
        ":quit" | ":q" | ":exit" => ReplCmdResult::Quit,
        ":help" | ":h" | ":?" => {
            println!(
                ":help                  this list\n\
                 :quit                  exit (also ^D)\n\
                 :tier walker|vm        switch execution tier (current: {})\n\
                 :time <expr>           evaluate <expr> and report wall time\n\
                 :load <path>           load and run a Scheme file in this session\n\
                 :reset                 reinitialize runtime, dropping definitions",
                if *via_vm { "vm" } else { "walker" }
            );
            ReplCmdResult::Continue
        }
        ":tier" => {
            match arg {
                "walker" => {
                    *via_vm = false;
                    println!("tier: walker");
                }
                "vm" => {
                    *via_vm = true;
                    println!("tier: vm");
                }
                "" => {
                    println!("tier: {}", if *via_vm { "vm" } else { "walker" });
                }
                other => println!("unknown tier {:?} — use walker or vm", other),
            }
            ReplCmdResult::Continue
        }
        ":time" => {
            if arg.is_empty() {
                println!(":time needs an expression");
                return ReplCmdResult::Continue;
            }
            let t = std::time::Instant::now();
            let r = if *via_vm {
                rt.eval_str_via_vm("<:time>", arg)
            } else {
                rt.eval_str("<:time>", arg)
            };
            let dt = t.elapsed();
            match r {
                Ok(v) => {
                    if !matches!(v, Value::Unspecified) {
                        println!("{}", rt.format_value(&v, WriteMode::Write));
                    }
                    println!("; {:.3?}", dt);
                }
                Err(diag) => {
                    let s = render_diag(&diag, rt.source_map(), color);
                    eprint!("{}", s);
                }
            }
            ReplCmdResult::Continue
        }
        ":load" => {
            if arg.is_empty() {
                println!(":load needs a file path");
                return ReplCmdResult::Continue;
            }
            match fs::read_to_string(arg) {
                Ok(src) => {
                    let r = if *via_vm {
                        rt.eval_str_via_vm(arg, &src)
                    } else {
                        rt.eval_str(arg, &src)
                    };
                    match r {
                        Ok(v) => {
                            if !matches!(v, Value::Unspecified) {
                                println!("{}", rt.format_value(&v, WriteMode::Write));
                            }
                            println!("; loaded {}", arg);
                        }
                        Err(diag) => {
                            let s = render_diag(&diag, rt.source_map(), color);
                            eprint!("{}", s);
                        }
                    }
                }
                Err(e) => println!(":load {}: {}", arg, e),
            }
            ReplCmdResult::Continue
        }
        ":reset" => {
            *rt = Runtime::new();
            println!("runtime reset");
            ReplCmdResult::Continue
        }
        other => {
            println!("unknown REPL command {:?} — try :help", other);
            ReplCmdResult::Continue
        }
    }
}

fn is_balanced(src: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    depth <= 0 && !in_string
}
