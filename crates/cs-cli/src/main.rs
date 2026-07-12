//! `crabscheme` binary — minimal CLI entry.

use std::fs;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use cs_core::{Value, WriteMode};
use cs_diag::{render_with, Diagnostic, SourceMap};
use cs_runtime::Runtime;

// cs-vnf.2 — mimalloc as the global allocator. wasm32 targets keep the
// default allocator (see Cargo.toml's target-gated dependency).
#[cfg(not(target_family = "wasm"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Execution tier selection for `--tier`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Tier {
    Walker,
    Vm,
    #[value(name = "vm-jit")]
    VmJit,
}

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
    tier: Tier,

    /// When to color diagnostics: auto (TTY-dependent), always, or never.
    #[arg(long = "color", value_name = "WHEN", default_value = "auto")]
    color: String,

    /// Restrict (environment ...) to these import specs (ADR 0015 L1 sandbox).
    /// Pass once per spec, e.g. --sandbox-imports '(rnrs base)'.
    /// When set, any nested (environment ...) call naming an unlisted library
    /// returns an error. Used by SandboxRuntime; not intended for end users.
    #[arg(long = "sandbox-imports", value_name = "SPEC")]
    sandbox_imports: Vec<String>,

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
    /// (and optionally a native binary). Compiles the subset of Scheme
    /// that lowers to cs-aot's supported RIR Insts: numeric + flonum
    /// kernels, self- and mutual recursion, cross-procedure calls,
    /// global free-variable reads, multi-block let/if/cond, vectors,
    /// pairs/lists, and strings + general builtins (via walker-speed
    /// generic dispatch). Multi-define files and `--multi`
    /// multi-procedure binaries are supported. Not yet: closure values
    /// (`MakeClosure`) and `set!` on globals (`EnvSet`). See
    /// docs/user/aot.md for the full supported/unsupported tables.
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
        /// RC3 Phase 4 iter 4.3: print a per-lambda AOT-compatibility
        /// report and exit. Doesn't emit a cargo project or build
        /// anything. For each top-level `(define (name args) body)`
        /// in the source, tries `bytecode_to_rir_aot` and reports
        /// whether the resulting RIR is in cs-aot's supported set.
        /// Useful for surveying coverage before picking `--entry`.
        #[arg(long = "explain")]
        explain: bool,
        /// RC3 Phase 6 iter 6.3: emit a multi-procedure binary
        /// instead of a single-entry one. The resulting binary
        /// takes `<fn-name> <args...>` on the CLI and dispatches
        /// to whichever AOT-compatible top-level lambda matches.
        /// Incompatible lambdas (e.g., MakeClosure-blocked) are
        /// skipped with a warning printed at emit time, not
        /// included in the binary. Useful when a single source
        /// file defines several utility functions you want to
        /// AOT together.
        #[arg(long = "multi")]
        multi: bool,
        /// RC3 Phase 6 iter 6.6: after `--build`, also run the
        /// AOT'd binary AND the JIT tier on the given sample arg
        /// list, and warn if the two outputs disagree. Cheap
        /// insurance against silent codegen regressions; use when
        /// shipping a binary you can't independently verify.
        ///
        /// Format: `--verify "1 2 3"` (space-separated args, same
        /// shape the AOT'd binary would receive). Exit code 0 on
        /// match, non-zero with diagnostic on mismatch.
        #[arg(long = "verify", value_name = "ARGS")]
        verify: Option<String>,
        /// Typer Phase 6.2: run the typer's checker before
        /// AOT'ing. Surfaces type-mismatch / arity errors as
        /// `cs_diag::Diagnostic`s and exits 1 before invoking
        /// cs-aot. Default off (preserves the iter 5.3
        /// warn-and-proceed behavior where syntactic
        /// annotation errors print but don't block AOT).
        #[arg(long = "typecheck")]
        typecheck: bool,
        /// RC3 Phase 6 iter 6.4: cross-compile target triple. Passed
        /// verbatim to `cargo build --target=<TRIPLE>`. Requires the
        /// target to be installed via `rustup target add <TRIPLE>`.
        ///
        /// Examples:
        ///   --target wasm32-wasip1       — WASM (run with wasmtime)
        ///   --target x86_64-unknown-linux-gnu — Linux glibc x86_64
        ///   --target aarch64-apple-darwin     — Apple Silicon Mac
        ///
        /// On success, the binary path is reported with the target
        /// triple in it (target/<triple>/release/<name>). --verify
        /// is skipped when --target is set since the cross-compiled
        /// binary likely can't run on the host.
        #[arg(long = "target", value_name = "TRIPLE")]
        target: Option<String>,
    },
    /// RC3 Phase 4 iter 4.5: self-test the AOT installation. Runs
    /// a baked-in `(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))`
    /// through the front-end (parse → expand → compile → RIR → emit) then
    /// exercises both back-ends: level 1 (cargo project → `cargo build` →
    /// run, when a Rust toolchain is present) and level 3 (cranelift-object
    /// → system `cc` link against libcs_aot_rt.a → run, no toolchain). Each
    /// asserts the binary returns `120` for `fact(5)`. Exit 0 if at least
    /// one back-end is usable; non-zero (with per-step diagnostics) if the
    /// front-end fails or neither back-end works. Useful for verifying a
    /// release-installed binary works on the user's platform.
    #[cfg(feature = "aot")]
    AotDoctor,
    /// Typer Phase 6.1: typecheck a Scheme source file.
    ///
    /// Runs parse → extract annotations → expand → checker;
    /// prints diagnostics in the standard format. Exit code 0
    /// when no type errors surface, 1 when type errors surface,
    /// 2 on parse / expand errors. Untyped programs always exit
    /// 0 (typer is a no-op without annotations).
    #[cfg(feature = "aot")]
    Check {
        /// Path to the .scm source file.
        file: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let via_vm = cli.tier == Tier::Vm || cli.tier == Tier::VmJit;
    let with_jit = cli.tier == Tier::VmJit;
    let color = color_enabled(&cli.color);

    if let Some(expr) = cli.expr {
        let sandbox_imports = if cli.sandbox_imports.is_empty() {
            None
        } else {
            Some(cli.sandbox_imports)
        };
        return run_eval(&expr, via_vm, with_jit, color, sandbox_imports);
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
            explain,
            multi,
            verify,
            target,
            typecheck,
        }) => {
            if explain {
                run_aot_explain(&file)
            } else if multi {
                run_aot_multi(&file, output.as_deref(), build, typecheck, color)
            } else {
                let code = run_aot(
                    &file,
                    output.as_deref(),
                    entry.as_deref(),
                    build,
                    emit_rir,
                    emit_rust_source,
                    target.as_deref(),
                );
                // RC3 iter 6.6: --verify post-step. Skipped when
                // --target is set (cross-compiled binary likely
                // can't run on the host).
                if let Some(args) = verify {
                    if target.is_some() {
                        eprintln!(
                            "crabscheme aot --verify skipped (cross-compiled binary can't run on the host)"
                        );
                        code
                    } else if matches!(code, ExitCode::SUCCESS) && build {
                        let sample_args: Vec<&str> = args.split_whitespace().collect();
                        run_aot_verify(&file, entry.as_deref(), output.as_deref(), &sample_args)
                    } else {
                        eprintln!(
                            "crabscheme aot --verify requires --build (and a successful AOT build); skipping verify"
                        );
                        code
                    }
                } else {
                    code
                }
            }
        }
        #[cfg(feature = "aot")]
        Some(Cmd::AotDoctor) => run_aot_doctor(),
        #[cfg(feature = "aot")]
        Some(Cmd::Check { file }) => run_check(&file, color),
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
/// AOT level 1: locate the cs-vm crate the emitted project depends on.
/// Prefers the on-disk dev-tree path (a from-source build); falls back to
/// the workspace sources embedded in this binary (release tarball built
/// with `--features bundled-aot-sources`), extracted once to a cache dir.
/// `cs-runtime` is derived as cs-vm's sibling by `cs_aot::project`.
#[cfg(feature = "aot")]
fn resolve_cs_vm_path() -> std::io::Result<std::path::PathBuf> {
    let dev = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .join("cs-vm");
    // `CRABSCHEME_AOT_FORCE_BUNDLED=1` forces the embedded path even when
    // the dev tree exists — used to exercise the release path in tests.
    let force = std::env::var_os("CRABSCHEME_AOT_FORCE_BUNDLED").is_some();
    if dev.exists() && !force {
        return Ok(dev);
    }
    Ok(bundled_sources::ensure()?.join("crates/cs-vm"))
}

/// Whether a Rust toolchain (cargo + rustc) is on PATH. Gates AOT level 1
/// (emit a cargo project + `cargo build`) vs. the self-contained level-3
/// path (direct native codegen — not yet implemented).
#[cfg(feature = "aot")]
fn rust_toolchain_present() -> bool {
    let ok = |tool: &str| {
        std::process::Command::new(tool)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("cargo") && ok("rustc")
}

/// The workspace crate sources embedded into this binary (AOT level 1).
#[cfg(feature = "aot")]
mod bundled_sources {
    include!(concat!(env!("OUT_DIR"), "/bundled_sources.rs"));

    /// Extract the embedded sources to a per-version cache dir (once) and
    /// return its path. Errors if this binary was built without
    /// `--features bundled-aot-sources` (the table is empty).
    pub fn ensure() -> std::io::Result<std::path::PathBuf> {
        use std::io::{Error, ErrorKind};
        if BUNDLED_SOURCES.is_empty() {
            return Err(Error::new(
                ErrorKind::NotFound,
                "no workspace sources embedded (built without \
                 --features bundled-aot-sources) and cs-vm not on disk",
            ));
        }
        let base = cache_dir().join(concat!("crabscheme-aot-src-", env!("CARGO_PKG_VERSION")));
        let ready = base.join(".ready");
        if ready.exists() {
            return Ok(base);
        }
        for (rel, bytes) in BUNDLED_SOURCES {
            let p = base.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, bytes)?;
        }
        std::fs::write(&ready, env!("CARGO_PKG_VERSION"))?;
        Ok(base)
    }

    fn cache_dir() -> std::path::PathBuf {
        std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })
            .unwrap_or_else(std::env::temp_dir)
    }
}

/// Build the standard primop table the AOT bytecode→RIR pipeline
/// expects.
///
/// Mirrors the names cs-runtime registers for the same `PrimOp`
/// kinds: `+`, `-`, `*`, `<`, `<=`, `>`, `>=`, `=`. Without these
/// the compiler lowers e.g. `(+ a b)` to a generic `Call`, which
/// cs-aot's bytecode→RIR translator then refuses with an
/// `UnsupportedInst`. Shared across every AOT entry point
/// (`run_aot`, `run_aot_doctor`, `run_aot_explain`, `run_aot_multi`)
/// so adding a primop is one edit, not four. `run_aot_doctor`
/// previously declared a 5-op subset of this list; the extra
/// entries are harmless (the bytecode→RIR translator emits a
/// primop inst only when the source actually references it).
#[cfg(feature = "aot")]
fn aot_primop_table(
    syms: &mut cs_core::SymbolTable,
) -> std::collections::HashMap<cs_core::Symbol, cs_vm::compiler::PrimOp> {
    use cs_vm::compiler::PrimOp;
    let mut m = std::collections::HashMap::new();
    for (op, kind) in &[
        ("+", PrimOp::Add),
        ("-", PrimOp::Sub),
        ("*", PrimOp::Mul),
        ("<", PrimOp::Lt),
        ("<=", PrimOp::Le),
        (">", PrimOp::Gt),
        (">=", PrimOp::Ge),
        ("=", PrimOp::Eq),
    ] {
        m.insert(syms.intern(op), *kind);
    }
    m
}

/// Prints each step's result; exits 0 if all pass.
#[cfg(feature = "aot")]
fn run_aot_doctor() -> ExitCode {
    use std::collections::HashMap;
    use std::process::Command;

    use cs_aot::project::{emit_project, ProjectOptions};
    use cs_aot::EmitMode;
    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;
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
    let primops = aot_primop_table(&mut syms);
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
    // Dev-tree path if present, else the workspace sources embedded in this
    // binary (release tarball built with --features bundled-aot-sources).
    let cs_vm_path = match resolve_cs_vm_path() {
        Ok(p) => p,
        Err(e) => return bail(&format!("cannot locate cs-vm sources: {e}")),
    };
    report(
        "resolve cs-vm dep",
        cs_vm_path.exists(),
        &cs_vm_path.display().to_string(),
    );

    let tmpdir = std::env::temp_dir().join(format!("crabscheme-aot-doctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmpdir);
    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: "doctor_fact".to_string(),
        entry_fn_name: "fact".to_string(),
        cs_vm_dep: None,
        cs_vm_path: Some(cs_vm_path),
        multi_procedure: false,
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

    // ---- Step 7: level-1 build (cargo). Non-fatal: on a toolchain-free
    //      host, level 3 (below) may be the only available backend.
    let mut l1_ok = false;
    if rust_toolchain_present() {
        println!("  ...running cargo build --release (may take ~10s on a cold cache)...");
        match Command::new("cargo")
            .current_dir(&emitted.project_dir)
            .arg("build")
            .arg("--release")
            .status()
        {
            Ok(s) if s.success() => {
                report("level 1: cargo build --release", true, "");
                let bin = &emitted.built_binary_path;
                match Command::new(bin).arg("5").output() {
                    Ok(o)
                        if o.status.success()
                            && String::from_utf8_lossy(&o.stdout).trim() == "120" =>
                    {
                        report("level 1: fact(5) = 120", true, &bin.display().to_string());
                        l1_ok = true;
                    }
                    Ok(o) => report(
                        "level 1: fact(5) = 120",
                        false,
                        &format!("got {:?}", String::from_utf8_lossy(&o.stdout).trim()),
                    ),
                    Err(e) => report("level 1: run binary", false, &format!("spawn: {e}")),
                }
            }
            Ok(s) => report(
                "level 1: cargo build --release",
                false,
                &format!("exit {s}"),
            ),
            Err(e) => report(
                "level 1: cargo build --release",
                false,
                &format!("spawn: {e}"),
            ),
        }
    } else {
        report(
            "level 1: cargo + rustc",
            false,
            "not on PATH — skipping (level 3 below is the toolchain-free path)",
        );
    }

    // ---- Step 8: level-3 self-test (cranelift-object + system cc, no
    //      rustc). Lowers the same `fact` to a relocatable object, links it
    //      against libcs_aot_rt.a, and verifies fact(5) = 120.
    let mut l3_ok = false;
    {
        use cs_jit_cranelift::ObjectLowerer;
        use cs_vm::jit_translate::bytecode_to_rir;

        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
        let cc_ok = Command::new(&cc)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        report("level 3: system cc", cc_ok, &cc);
        let archive = resolve_aot_archive();
        report(
            "level 3: runtime archive",
            archive.is_some(),
            &archive
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "libcs_aot_rt.a not found".to_string()),
        );
        if let (true, Some(archive)) = (cc_ok, archive) {
            let lowered = bytecode_to_rir(lam, "fact", Some(fact_sym))
                .map_err(|e| format!("{e:?}"))
                .and_then(|rir| {
                    let mut lo =
                        ObjectLowerer::new_object("doctor").map_err(|e| format!("{e:?}"))?;
                    lo.set_entry_export("crabscheme_aot_entry");
                    lo.define_uniform_nb(&rir).map_err(|e| format!("{e:?}"))?;
                    lo.finish_object().map_err(|e| format!("{e:?}"))
                });
            match lowered {
                Ok(obj) => {
                    report(
                        "level 3: emit object",
                        true,
                        &format!("{} bytes", obj.len()),
                    );
                    let l3dir = tmpdir.join("l3");
                    let _ = std::fs::create_dir_all(&l3dir);
                    let (op, mp, bp) = (
                        l3dir.join("prog.o"),
                        l3dir.join("main.c"),
                        l3dir.join("fact"),
                    );
                    let _ = std::fs::write(&op, &obj);
                    let _ = std::fs::write(&mp, generate_c_main("crabscheme_aot_entry", 1));
                    let mut cmd = Command::new(&cc);
                    cmd.arg(&mp).arg(&op).arg(&archive).arg("-o").arg(&bp);
                    #[cfg(not(target_os = "macos"))]
                    {
                        cmd.arg("-lpthread").arg("-ldl").arg("-lm");
                    }
                    match cmd.status() {
                        Ok(s) if s.success() => match Command::new(&bp).arg("5").output() {
                            Ok(o)
                                if o.status.success()
                                    && String::from_utf8_lossy(&o.stdout).trim() == "120" =>
                            {
                                report("level 3: fact(5) = 120", true, &bp.display().to_string());
                                l3_ok = true;
                            }
                            Ok(o) => report(
                                "level 3: fact(5) = 120",
                                false,
                                &format!("got {:?}", String::from_utf8_lossy(&o.stdout).trim()),
                            ),
                            Err(e) => report("level 3: run binary", false, &format!("spawn: {e}")),
                        },
                        Ok(s) => report("level 3: cc link", false, &format!("exit {s}")),
                        Err(e) => report("level 3: cc link", false, &format!("spawn: {e}")),
                    }
                }
                Err(e) => report("level 3: emit object", false, &e),
            }
        }
    }

    println!();
    match (l1_ok, l3_ok) {
        (true, true) => println!("crabscheme aot-doctor: ready — level 1 + level 3."),
        (true, false) => println!("crabscheme aot-doctor: ready — level 1 (cargo+rustc) only."),
        (false, true) => {
            println!("crabscheme aot-doctor: ready — level 3 only (toolchain-free: cc + archive).")
        }
        (false, false) => {
            return bail(
                "neither level 1 (cargo+rustc) nor level 3 (cc + libcs_aot_rt.a) is usable",
            )
        }
    }
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
    target: Option<&str>,
) -> ExitCode {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Command;

    use cs_aot::project::{emit_project, ProjectOptions};
    use cs_aot::EmitMode;
    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;
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

    // cs-qrm: populate globals from the runtime's builtins so the
    // compiler folds (not p), (/ a b), (display x), etc. to
    // Const(Procedure) — same as run_aot_multi's RC3 iter 2.14 fix.
    // Without this every builtin ref compiles to LoadVar/EnvLookupAny
    // + CallGeneral, paying full uncached procedure-dispatch cost per
    // call even for a one-bit boolean flip like `not` (see
    // docs/measurements/2026-07-10-jit-vs-aot-tak.md).
    let rt_globals = cs_runtime::Runtime::new().builtin_procs_by_name();
    let mut globals: HashMap<cs_core::Symbol, cs_core::Value> = HashMap::new();
    for (name, val) in rt_globals {
        let sym = syms.intern(&name);
        globals.insert(sym, val);
    }
    let primops = aot_primop_table(&mut syms);
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
    let mut rir = match bytecode_to_rir_aot(&lam, &entry_name, Some(entry_sym)) {
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
    // RC3 iter 2.2 Step 1 — record the source lambda index for
    // cs-aot's MakeClosure resolver. For single-entry mode we
    // also annotate so any nested MakeClosure inside the entry
    // can reference itself (degenerate, but consistent).
    if let Some(idx) = lambda_index_for(&bc, entry_sym) {
        rir.lambda_index = Some(idx);
    }

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

    // AOT level gate (#249). `--build` with a Rust toolchain present uses
    // level 1 (emit a cargo project + `cargo build`). Without cargo+rustc
    // on PATH — or with CRABSCHEME_AOT_FORCE_OBJECT=1 — fall back to level 3:
    // a self-contained cranelift-object `.o` linked by the system `cc`
    // against the prebuilt cs-aot-rt archive, no Rust toolchain required.
    let force_object = std::env::var_os("CRABSCHEME_AOT_FORCE_OBJECT").is_some();
    if build && (force_object || !rust_toolchain_present()) {
        return run_aot_object(file, &lam, &entry_name, entry_sym, output, target);
    }
    // Resolve cs-vm: the dev-tree path for a from-source build, else the
    // workspace sources embedded in this binary (release tarball).
    let cs_vm_path = match resolve_cs_vm_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("crabscheme aot: cannot locate cs-vm sources: {e}");
            return ExitCode::from(4);
        }
    };

    let pkg_name = sanitize_pkg_name(&entry_name);
    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: pkg_name.clone(),
        entry_fn_name: entry_name.clone(),
        cs_vm_dep: None, // fall through to legacy cs_vm_path
        cs_vm_path: Some(cs_vm_path),
        multi_procedure: false,
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

    // RC3 iter 6.4: --target cross-compile. Builds with
    // `cargo build --release --target=<triple>` so the output
    // lives at `target/<triple>/release/<pkg>` instead of
    // `target/release/<pkg>`.
    let target_blurb = if let Some(t) = target {
        format!(" --target={t}")
    } else {
        String::new()
    };
    println!("  building (cargo build --release{target_blurb})...");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&emitted.project_dir)
        .arg("build")
        .arg("--release");
    if let Some(t) = target {
        cmd.arg(format!("--target={t}"));
    }
    let status = cmd.status();
    match status {
        Ok(s) if s.success() => {
            // Re-derive the binary path: cargo's output dir gets
            // the target triple inserted when --target is set.
            // WASM targets (`wasm32-*`) produce a `.wasm` extension
            // cargo doesn't include on the file-name we stored.
            let bin_dir = if let Some(t) = target {
                emitted.project_dir.join("target").join(t).join("release")
            } else {
                emitted.project_dir.join("target").join("release")
            };
            let base_name = emitted.built_binary_path.file_name().unwrap();
            let bin_path = if target.map(|t| t.starts_with("wasm32-")).unwrap_or(false) {
                let mut p = bin_dir.join(base_name);
                p.set_extension("wasm");
                p
            } else {
                bin_dir.join(base_name)
            };
            println!("  built: {}", bin_path.display());
            if target.map(|t| t.starts_with("wasm32-")).unwrap_or(false) {
                println!("  usage: wasmtime run {} <args...>", bin_path.display());
            }
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("crabscheme aot: cargo build failed (exit {})", s);
            if target.is_some() {
                eprintln!(
                    "  hint: the target may need `rustup target add {}` before cross-compile works",
                    target.unwrap()
                );
            }
            ExitCode::from(5)
        }
        Err(e) => {
            eprintln!("crabscheme aot: cargo not found / failed to spawn: {e}");
            ExitCode::from(5)
        }
    }
}

/// AOT **level 3** driver — compile the entry lambda to a self-contained
/// native binary with no Rust toolchain. Reuses the JIT's Cranelift
/// lowering (`cs_jit_cranelift::ObjectLowerer`) to emit a relocatable `.o`,
/// generates a tiny C `main`, and links both against the prebuilt
/// `libcs_aot_rt.a` archive with the system `cc`. See `docs/user/aot.md`.
///
/// Scope: a single self-contained function (only self-recursion). Cross-
/// function programs lower `Inst::Call`/`CallGeneral`, which need runtime
/// procedure registration the standalone binary lacks — those decline here
/// with a pointer to the L1 (cargo+rustc) multi-procedure path.
#[cfg(feature = "aot")]
fn run_aot_object(
    file: &str,
    lam: &cs_vm::opcode::CompiledLambda,
    entry_name: &str,
    entry_sym: cs_core::Symbol,
    output: Option<&str>,
    target: Option<&str>,
) -> ExitCode {
    use std::path::PathBuf;
    use std::process::Command;

    use cs_jit_cranelift::ObjectLowerer;
    use cs_rir::Inst;
    use cs_vm::jit_translate::bytecode_to_rir;

    // The symbol the emitted object exports + the generated C main calls.
    const ENTRY_SYM: &str = "crabscheme_aot_entry";

    // L3 emits host-native objects only; cross-compilation needs L1.
    if let Some(t) = target {
        eprintln!(
            "crabscheme aot --build --target={t}: the toolchain-free (level 3) \
             backend emits host-native objects only. Install cargo+rustc for \
             cross-compilation (level 1)."
        );
        return ExitCode::from(4);
    }

    // JIT-dialect RIR: builtins lower to dedicated Insts the object backend
    // handles (not the AOT `CallBuiltin` dialect, which the standalone
    // binary can't dispatch).
    let rir = match bytecode_to_rir(lam, entry_name, Some(entry_sym)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("crabscheme aot (level 3): bytecode→RIR error: {e:?}");
            return ExitCode::from(3);
        }
    };

    // Self-contained check: a cross-function call needs runtime dispatch the
    // standalone binary can't satisfy (self-recursion lowers to a direct
    // call and is fine).
    let has_cross_call = rir.blocks.iter().any(|b| {
        b.insts
            .iter()
            .any(|i| matches!(i, Inst::Call(..) | Inst::CallGeneral(..)))
    });
    if has_cross_call {
        eprintln!(
            "crabscheme aot (level 3): `{entry_name}` calls other functions, which \
             the toolchain-free backend can't link yet (only self-recursion is \
             supported). Install cargo+rustc to use level 1 (multi-procedure AOT)."
        );
        return ExitCode::from(4);
    }

    // Lower to a relocatable object exporting ENTRY_SYM.
    let mut lo = match ObjectLowerer::new_object(entry_name) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("crabscheme aot (level 3): object backend init failed: {e:?}");
            return ExitCode::from(4);
        }
    };
    lo.set_entry_export(ENTRY_SYM);
    if let Err(e) = lo.define_uniform_nb(&rir) {
        eprintln!(
            "crabscheme aot (level 3): cannot compile `{entry_name}`: {e:?}\n  \
             (the level-3 backend lowers the same Inst set as the JIT; an \
             unsupported op means this function needs level 1 — cargo+rustc.)"
        );
        return ExitCode::from(3);
    }
    let obj_bytes = match lo.finish_object() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("crabscheme aot (level 3): object emit failed: {e:?}");
            return ExitCode::from(4);
        }
    };

    // Locate the prebuilt runtime archive.
    let archive = match resolve_aot_archive() {
        Some(p) => p,
        None => {
            eprintln!(
                "crabscheme aot (level 3): cannot find the runtime archive \
                 libcs_aot_rt.a. It ships beside the binary in release tarballs; \
                 set CRABSCHEME_AOT_ARCHIVE=/path/to/libcs_aot_rt.a to override."
            );
            return ExitCode::from(4);
        }
    };

    // Output binary path (compiler `-o` semantics: the binary, not a dir).
    let bin_path = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(basename_no_ext(file)));

    // Intermediates in a per-process temp dir.
    let tmp = std::env::temp_dir().join(format!("crabscheme-aot-{}", std::process::id()));
    if let Err(e) = fs::create_dir_all(&tmp) {
        eprintln!(
            "crabscheme aot (level 3): cannot create temp dir {}: {e}",
            tmp.display()
        );
        return ExitCode::from(4);
    }
    let obj_path = tmp.join("prog.o");
    let main_c_path = tmp.join("main.c");
    if let Err(e) = fs::write(&obj_path, &obj_bytes) {
        eprintln!(
            "crabscheme aot (level 3): cannot write {}: {e}",
            obj_path.display()
        );
        return ExitCode::from(4);
    }
    if let Err(e) = fs::write(&main_c_path, generate_c_main(ENTRY_SYM, rir.params.len())) {
        eprintln!(
            "crabscheme aot (level 3): cannot write {}: {e}",
            main_c_path.display()
        );
        return ExitCode::from(4);
    }

    // Link: cc main.c prog.o libcs_aot_rt.a -o bin
    println!(
        "crabscheme aot (level 3): linking {} with cc...",
        bin_path.display()
    );
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = Command::new(&cc);
    cmd.arg(&main_c_path)
        .arg(&obj_path)
        .arg(&archive)
        .arg("-o")
        .arg(&bin_path);
    // Rust std needs the platform thread/dl/math libs on Linux; macOS folds
    // them into libSystem (and rejects -ldl), so only add them elsewhere.
    #[cfg(not(target_os = "macos"))]
    {
        cmd.arg("-lpthread").arg("-ldl").arg("-lm");
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            println!(
                "crabscheme aot: built {} (level 3 — no Rust toolchain)",
                bin_path.display()
            );
            println!("  entry: {entry_name}");
            println!("  archive: {}", archive.display());
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("crabscheme aot (level 3): cc failed with status {s}");
            ExitCode::from(5)
        }
        Err(e) => {
            eprintln!("crabscheme aot (level 3): cannot spawn `{cc}`: {e}");
            ExitCode::from(5)
        }
    }
}

/// Locate the prebuilt `libcs_aot_rt.a` runtime archive the level-3 link
/// step needs. Checks, in order: `CRABSCHEME_AOT_ARCHIVE`, beside the
/// running binary (release-tarball layout `crabscheme` + `libcs_aot_rt.a`,
/// which also matches the dev `target/<profile>/` directory), and a `lib/`
/// subdir of either.
#[cfg(feature = "aot")]
fn resolve_aot_archive() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    const NAME: &str = "libcs_aot_rt.a";

    if let Some(p) = std::env::var_os("CRABSCHEME_AOT_ARCHIVE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for cand in [dir.join(NAME), dir.join("lib").join(NAME)] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }
    None
}

/// Generate the C `main` for a level-3 AOT binary. The emitted object
/// exports `entry_sym` as `int64_t entry(int64_t…)` taking `arity`
/// nan-boxed args and returning a nan-boxed result; the C glue parses argv
/// integers, encodes them via `cs_aot_nb_fixnum`, calls the entry, and
/// prints through `cs_aot_print_result` (both from `libcs_aot_rt.a`), so
/// the glue never hard-codes the nan-box ABI.
#[cfg(feature = "aot")]
fn generate_c_main(entry_sym: &str, arity: usize) -> String {
    let mut s = String::new();
    s.push_str("#include <stdint.h>\n#include <stdio.h>\n#include <stdlib.h>\n\n");
    s.push_str("extern int64_t cs_aot_nb_fixnum(int64_t);\n");
    s.push_str("extern void cs_aot_print_result(int64_t);\n");
    let params = if arity == 0 {
        "void".to_string()
    } else {
        vec!["int64_t"; arity].join(", ")
    };
    s.push_str(&format!("extern int64_t {entry_sym}({params});\n\n"));
    s.push_str("int main(int argc, char **argv) {\n");
    s.push_str(&format!("    if (argc < {}) {{\n", arity + 1));
    let usage: String = (0..arity).map(|i| format!(" <arg{i}>")).collect();
    s.push_str(&format!(
        "        fprintf(stderr, \"usage: %s{usage}\\n\", argv[0]);\n"
    ));
    s.push_str("        return 2;\n    }\n");
    let call_args: String = (0..arity)
        .map(|i| format!("cs_aot_nb_fixnum(atoll(argv[{}]))", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    s.push_str(&format!("    int64_t r = {entry_sym}({call_args});\n"));
    s.push_str("    cs_aot_print_result(r);\n");
    s.push_str("    return 0;\n}\n");
    s
}

#[cfg(feature = "aot")]
fn basename_no_ext(path: &str) -> &str {
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("aot");
    stem
}

/// RC3 Phase 4 iter 4.3: AOT compatibility survey for a Scheme
/// source file. Lists every top-level `(define (name args) body)`
/// the bytecode compiler emits and reports whether each one passes
/// `bytecode_to_rir_aot` + `emit_with(Nb)` cleanly.
///
/// Doesn't emit a cargo project or build anything — just enumerates
/// + probes. Useful for users debugging "why doesn't my program
/// AOT" who want to know which entries are compatible before
/// picking `--entry` and trying `--build`.
#[cfg(feature = "aot")]
fn run_aot_explain(file: &str) -> ExitCode {
    use std::collections::HashMap;

    use cs_aot::{emit_with, EmitMode};
    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;
    use cs_vm::{compile_with_globals_and_primops, jit_translate::bytecode_to_rir_aot};

    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme aot --explain: cannot read {file}: {e}");
            return ExitCode::from(1);
        }
    };

    let mut sources = SourceMap::new();
    let file_id = sources.add(file, &src);
    let mut syms = SymbolTable::new();
    let data = match read_all(file_id, &src, &mut syms) {
        Ok(d) => d,
        Err(errs) => {
            eprintln!(
                "crabscheme aot --explain: parse error: {}",
                errs[0].message()
            );
            return ExitCode::from(2);
        }
    };
    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = match expander.expand_program(&data) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("crabscheme aot --explain: expand error: {}", e.message());
            return ExitCode::from(2);
        }
    };
    drop(expander);
    let globals = HashMap::new();
    let primops = aot_primop_table(&mut syms);
    let bc = match compile_with_globals_and_primops(&core, &globals, &primops) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("crabscheme aot --explain: compile error: {}", e.message);
            return ExitCode::from(2);
        }
    };

    // Build the name → lambda-index map via MakeClosure+SetVar
    // pairs (same scanner the run_aot uses for --entry resolution).
    let mut entries: Vec<(String, usize)> = Vec::new();
    for window in bc.insts.windows(2) {
        if let (cs_vm::opcode::Inst::MakeClosure(idx), cs_vm::opcode::Inst::SetVar(sym)) =
            (&window[0], &window[1])
        {
            entries.push((syms.name(*sym).to_string(), *idx));
        }
    }

    println!("crabscheme aot --explain: {}", file);
    println!("  {} top-level lambda(s)", entries.len());
    println!();

    if entries.is_empty() {
        println!(
            "  no top-level defines found — AOT requires at least one\n  \
             (define (name args...) body) form in the source."
        );
        return ExitCode::SUCCESS;
    }

    let mut compatible: Vec<String> = Vec::new();
    let mut incompatible: Vec<(String, String)> = Vec::new();

    for (name, idx) in &entries {
        let entry_sym = syms.intern(name);
        let lam = &bc.lambdas[*idx];
        let arity = lam.params.len();
        match bytecode_to_rir_aot(lam, name.as_str(), Some(entry_sym)) {
            Ok(rir) => match emit_with(EmitMode::Nb, &rir) {
                Ok(_) => {
                    println!(
                        "  ✓ {name}  ({arity} param{}, RIR: {} block(s), {} inst(s))",
                        if arity == 1 { "" } else { "s" },
                        rir.blocks.len(),
                        rir.blocks.iter().map(|b| b.insts.len()).sum::<usize>(),
                    );
                    compatible.push(name.clone());
                }
                Err(e) => {
                    // Just the first line of the diagnostic — full
                    // user-hint output would be too verbose for a
                    // survey table.
                    let summary = format!("{e}")
                        .lines()
                        .next()
                        .unwrap_or("emit error")
                        .to_string();
                    println!("  ✗ {name}  ({arity} param) — {summary}");
                    incompatible.push((name.clone(), summary));
                }
            },
            Err(e) => {
                let summary = format!("bytecode→RIR error: {e:?}");
                println!("  ✗ {name}  ({arity} param) — {summary}");
                incompatible.push((name.clone(), summary));
            }
        }
    }

    println!();
    if !compatible.is_empty() {
        println!("AOT-compatible entries ({}):", compatible.len());
        for n in &compatible {
            println!("  crabscheme aot {file} --entry {n} --build");
        }
    }
    if !incompatible.is_empty() {
        println!();
        println!("Incompatible entries ({}):", incompatible.len());
        for (n, reason) in &incompatible {
            println!("  {n}: {reason}");
        }
        println!();
        println!(
            "See `docs/user/aot.md` for the supported-Inst table and rewrite\n\
             suggestions per blocker."
        );
    }

    if compatible.is_empty() {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

/// RC3 Phase 6 iter 6.3: emit a multi-procedure AOT'd binary.
///
/// Reads the source, enumerates every top-level `(define (NAME args) body)`,
/// tries `bytecode_to_rir_aot` + emit on each. Compatible entries become
/// match arms in the emitted binary's dispatch shim; incompatible ones
/// are warned at emit time and skipped.
///
/// Resulting binary takes `<fn> <args...>`:
///
///   $ ./mylib-aot/target/release/mylib square 5
///   25
///   $ ./mylib-aot/target/release/mylib cube 5
///   125
/// RC3 iter 2.16 — transitive capture propagation for AOT.
///
/// For each function F: if F's body contains `MakeClosure(I)` for
/// inner lambda I, and I captures sym S, then F must ALSO capture
/// S unless S is already in F's locals (params, EnvDefineLocal,
/// self_binding_sym, or top-level globals). Iterates to a fixed
/// point so multi-level lifting (nqueens's anon → place → nqueens)
/// resolves correctly in one pass.
///
/// Mutates each function's `captures` field in place; the cs-aot
/// emitter then picks up the new captures via its existing arms:
/// the function header gets `__cap<sym>` for the new entry, the
/// dispatch wrapper unpacks one more capture slot, and the parent's
/// MakeClosure capture-gather provides a value.
#[cfg(feature = "aot")]
fn propagate_transitive_captures(
    funcs: &mut [cs_rir::Function],
    known_globals: &std::collections::HashSet<u32>,
) {
    use std::collections::{HashMap, HashSet};

    // Build idx → position in funcs slice for fast lookup. funcs
    // were translated in bytecode-lambda-index order so position ==
    // lambda_index, but be defensive.
    let mut by_idx: HashMap<usize, usize> = HashMap::new();
    for (pos, f) in funcs.iter().enumerate() {
        if let Some(idx) = f.lambda_index {
            by_idx.insert(idx, pos);
        }
    }

    // Snapshot each function's "locals" (the syms a capture-need
    // can be satisfied by without becoming a transitive capture):
    // - param_syms (function's positional args)
    // - EnvDefineLocal syms (let-binding scans inside the body)
    // - self_binding_sym (the function's own letrec/top-level name)
    // - known_globals (top-level fns resolvable via by_name_sym)
    let locals: Vec<HashSet<u32>> = funcs
        .iter()
        .map(|f| {
            let mut set: HashSet<u32> = HashSet::new();
            set.extend(f.param_syms.iter().copied());
            if let Some(s) = f.self_binding_sym {
                set.insert(s);
            }
            set.extend(known_globals.iter().copied());
            for block in &f.blocks {
                for inst in &block.insts {
                    if let cs_rir::Inst::EnvDefineLocal(sym, _) = inst {
                        set.insert(*sym);
                    }
                }
            }
            set
        })
        .collect();

    // Fixed-point: propagate inner-lambda captures upward.
    loop {
        let mut changed = false;
        // Collect all MakeClosure edges before mutating funcs (so
        // we can read inner.captures without holding two borrows).
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for (parent_pos, f) in funcs.iter().enumerate() {
            for block in &f.blocks {
                for inst in &block.insts {
                    if let cs_rir::Inst::MakeClosure(_, inner_idx) = inst {
                        if let Some(&inner_pos) = by_idx.get(&(*inner_idx as usize)) {
                            edges.push((parent_pos, inner_pos));
                        }
                    }
                }
            }
        }
        for (parent_pos, inner_pos) in edges {
            // Snapshot the inner's captures so we don't hold a
            // borrow across the mutating parent update.
            let inner_caps: Vec<u32> = funcs[inner_pos].captures.clone();
            for sym in inner_caps {
                if locals[parent_pos].contains(&sym) {
                    continue;
                }
                let parent = &mut funcs[parent_pos];
                if !parent.captures.contains(&sym) {
                    parent.captures.push(sym);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

#[cfg(feature = "aot")]
/// Typer Phase 6.1: `crabscheme check FILE.scm`.
///
/// Runs parse → extract annotations → expand → checker.
/// Prints type-error diagnostics via cs-diag's standard
/// renderer (color-aware). Exit code:
///   0 — clean (no type errors)
///   1 — type errors found
///   2 — parse / expand / I/O failure before checking
#[cfg(feature = "aot")]
fn run_check(file: &str, color: bool) -> ExitCode {
    use std::collections::HashMap;

    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;

    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme check: cannot read {file}: {e}");
            return ExitCode::from(2);
        }
    };
    let mut sources = SourceMap::new();
    let file_id = sources.add(file, &src);
    let mut syms = SymbolTable::new();
    let data = match read_all(file_id, &src, &mut syms) {
        Ok(d) => d,
        Err(errs) => {
            for e in &errs {
                eprintln!("crabscheme check: parse error: {}", e.message());
            }
            return ExitCode::from(2);
        }
    };
    // M01: strip `#:effects` declarations + record the declared effect set
    // per definition before the annotation pre-pass / expander see the forms.
    let (data, effect_decls, effect_decl_diags) = cs_typer::extract_effect_decls(&data, &mut syms);
    // Typer pre-pass: strip annotations + diagnostics for
    // malformed ones (typer-bad-annotation).
    let (data, table, ann_diags) = cs_typer::extract_annotations(&data, &mut syms);
    let mut any_error = false;
    for d in effect_decl_diags.iter().chain(ann_diags.iter()) {
        if matches!(d.severity, cs_diag::Severity::Error) {
            any_error = true;
        }
        eprintln!("{}", render_diag(d, &sources, color));
    }
    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = match expander.expand_program(&data) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("crabscheme check: expand error: {}", e.message());
            return ExitCode::from(2);
        }
    };
    drop(expander);
    // Type check. Untyped programs (table.is_empty()) get
    // an empty error list — we still run the walker but
    // every node falls through the unannotated path.
    let mut checker = cs_typer::Checker::new(&table, &mut syms);
    let type_errors = checker.check_program(&core);
    for err in &type_errors {
        any_error = true;
        let diag = err.to_diagnostic();
        eprintln!("{}", render_diag(&diag, &sources, color));
    }
    // M01: effect-check pass — reject any body that performs an effect not
    // listed in its `#:effects` declaration.
    let effect_diags = cs_typer::check_effects(&core, &effect_decls, &syms);
    for d in &effect_diags {
        any_error = true;
        eprintln!("{}", render_diag(d, &sources, color));
    }
    if any_error {
        let err_count = |ds: &[cs_diag::Diagnostic]| {
            ds.iter()
                .filter(|d| matches!(d.severity, cs_diag::Severity::Error))
                .count()
        };
        let count = type_errors.len()
            + effect_diags.len()
            + err_count(&effect_decl_diags)
            + err_count(&ann_diags);
        eprintln!(
            "crabscheme check: {} error{} in {file}",
            count,
            if count == 1 { "" } else { "s" }
        );
        ExitCode::from(1)
    } else {
        eprintln!("crabscheme check: {file} ok");
        ExitCode::SUCCESS
    }
}

#[cfg(feature = "aot")]
fn run_aot_multi(
    file: &str,
    output: Option<&str>,
    build: bool,
    typecheck: bool,
    color: bool,
) -> ExitCode {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Command;

    use cs_aot::project::{emit_project, ProjectOptions};
    use cs_aot::EmitMode;
    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;
    use cs_vm::compile_with_globals_and_primops;

    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme aot --multi: cannot read {file}: {e}");
            return ExitCode::from(1);
        }
    };

    let mut sources = SourceMap::new();
    let file_id = sources.add(file, &src);
    let mut syms = SymbolTable::new();
    let data = match read_all(file_id, &src, &mut syms) {
        Ok(d) => d,
        Err(errs) => {
            eprintln!("crabscheme aot --multi: parse error: {}", errs[0].message());
            return ExitCode::from(2);
        }
    };
    // Phase 5.3: extract user annotations BEFORE expansion so
    // cs-expand never sees `[x : T]` markers, then later in
    // this function consult the typer's hint table to seed
    // per-lambda `param_type_hints`. Annotation diagnostics
    // print as warnings but don't block AOT; full Checker
    // validation is iter 6.2 territory.
    let (data, typer_table, typer_diags) = cs_typer::extract_annotations(&data, &mut syms);
    for d in &typer_diags {
        eprintln!(
            "crabscheme aot --multi: typer warning: {} ({:?})",
            d.message, d.severity
        );
    }
    let mut typer_hints = cs_typer::hints_by_name(&typer_table);
    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = match expander.expand_program(&data) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("crabscheme aot --multi: expand error: {}", e.message());
            return ExitCode::from(2);
        }
    };
    drop(expander);
    // Always run the Checker so inner-let named-loop bodies
    // pick up inferred Flonum/Fixnum hints from their body's
    // initial call. `--typecheck` additionally surfaces the
    // diagnostics and fail-fasts on any type error; without it,
    // we run the Checker silently for its side effect on
    // `inferred_param_hints`.
    let mut checker = cs_typer::Checker::new(&typer_table, &mut syms);
    let type_errors = checker.check_program(&core);
    // Merge typer-inferred hints (from named-let bodies etc.)
    // with the by-name ascription map. Inferred hints win on
    // collision since they reflect actual call-site shapes
    // (relevant when a binding name shadows a top-level
    // ascription, which shouldn't normally happen but is
    // harmless to handle).
    for (name, hints) in checker.inferred_hints_by_name() {
        typer_hints.insert(name, hints);
    }
    if typecheck {
        let ann_errors = typer_diags
            .iter()
            .filter(|d| matches!(d.severity, cs_diag::Severity::Error))
            .count();
        for err in &type_errors {
            let diag = err.to_diagnostic();
            eprintln!("{}", render_diag(&diag, &sources, color));
        }
        let total = ann_errors + type_errors.len();
        if total > 0 {
            eprintln!(
                "crabscheme aot --multi: --typecheck failed: {} type error{} in {file}",
                total,
                if total == 1 { "" } else { "s" }
            );
            return ExitCode::from(1);
        }
    }
    // RC3 iter 2.14 — populate globals from the runtime's builtins so
    // the compiler folds (/ a b), (display x), (not p), etc. to
    // Const(Procedure). Without this they'd compile to LoadVar which
    // becomes an EnvLookup → unresolved capture in AOT.
    let rt_globals = cs_runtime::Runtime::new().builtin_procs_by_name();
    let mut globals: HashMap<cs_core::Symbol, cs_core::Value> = HashMap::new();
    for (name, val) in rt_globals {
        let sym = syms.intern(&name);
        globals.insert(sym, val);
    }
    let primops = aot_primop_table(&mut syms);
    let bc = match compile_with_globals_and_primops(&core, &globals, &primops) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("crabscheme aot --multi: compile error: {}", e.message);
            return ExitCode::from(2);
        }
    };

    // RC3 iter 2.2 Step 5: enumerate ALL lambdas in bc.lambdas
    // (not just SetVar-bound ones) so that nested anonymous
    // lambdas referenced via `Inst::MakeClosure(_, idx)` from
    // other functions can be resolved by cs-aot's LambdaResolver.
    //
    // Named lambdas (those with MakeClosure+SetVar pairs) get
    // their original name from the SetVar sym. Anonymous lambdas
    // get a synthetic name `__aot_lambda_<idx>` so they have a
    // stable Rust identifier for emission.
    let mut name_by_idx: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    let mut sym_by_idx: std::collections::HashMap<usize, cs_core::Symbol> =
        std::collections::HashMap::new();
    // RC3 iter 2.9 — self-name detection for letrec / named-let-
    // bound inner lambdas. Scan EVERY lambda's body (not just the
    // top-level insts) for MakeClosure(idx)+SetVar(sym) and
    // MakeClosure(idx)+DefineLocal(sym) patterns. The first gives
    // top-level (define) names; the second gives letrec / named-let
    // (DefineLocal) names. Passing the right self_sym to the
    // translator lets CallSelf detection kick in on the inner
    // lambda's recursive calls, avoiding the chicken-and-egg
    // "lambda captures itself" problem in MakeClosure.
    let scan_pairs = |insts: &[cs_vm::opcode::Inst]| -> Vec<(usize, cs_core::Symbol)> {
        let mut pairs = Vec::new();
        for window in insts.windows(2) {
            if let cs_vm::opcode::Inst::MakeClosure(idx) = &window[0] {
                if let cs_vm::opcode::Inst::SetVar(sym)
                | cs_vm::opcode::Inst::DefineGlobal(sym)
                | cs_vm::opcode::Inst::DefineLocal(sym) = &window[1]
                {
                    pairs.push((*idx, *sym));
                }
            }
        }
        pairs
    };
    for (idx, sym) in scan_pairs(&bc.insts) {
        name_by_idx.insert(idx, syms.name(sym).to_string());
        sym_by_idx.insert(idx, sym);
    }
    for lam in bc.lambdas.iter() {
        for (idx, sym) in scan_pairs(&lam.body) {
            name_by_idx
                .entry(idx)
                .or_insert_with(|| syms.name(sym).to_string());
            sym_by_idx.entry(idx).or_insert(sym);
        }
    }
    // RC3 iter 2.15 — disambiguate name collisions. Multiple
    // letrec / named-let inner lambdas can share a binding name
    // (`loop` is the canonical case — alloc-stress has two of
    // them in different scopes). The bytecode-lambda-index is
    // globally unique so suffix it onto duplicates: first
    // occurrence keeps the bare name (for the common case);
    // duplicates become `loop_2`, `loop_3`, etc.
    {
        let mut name_to_first_idx: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let entries: Vec<(usize, String)> =
            name_by_idx.iter().map(|(i, n)| (*i, n.clone())).collect();
        for (idx, name) in entries {
            match name_to_first_idx.get(&name) {
                None => {
                    name_to_first_idx.insert(name.clone(), idx);
                }
                Some(&first) if first == idx => {}
                Some(_) => {
                    name_by_idx.insert(idx, format!("{name}_{idx}"));
                }
            }
        }
    }
    // RC3 iter 2.7 — known-globals set so the translator excludes
    // top-level AOT'd function names from the captures list.
    // Surviving EnvLookups of these syms then resolve through the
    // emitter's by_name_sym table to direct
    // `vm_alloc_aot_procedure` calls (cross-procedure references).
    //
    // RC3 iter 2.9: only TOP-LEVEL syms (those from bc.insts) belong
    // here — letrec/named-let bindings are local and need their
    // EnvDefineLocal-driven Value resolution, not a top-level lookup.
    let top_level_syms: std::collections::HashSet<u32> = {
        let mut s = std::collections::HashSet::new();
        for (idx, sym) in scan_pairs(&bc.insts) {
            let _ = idx;
            s.insert(sym.0);
        }
        s
    };
    let known_globals: std::collections::HashSet<u32> = top_level_syms;
    let mut compatible_funcs: Vec<cs_rir::Function> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for (idx, lam) in bc.lambdas.iter().enumerate() {
        // RC3 iter 2.15 — self_sym must be the ORIGINAL Scheme sym
        // (from sym_by_idx), NOT syms.intern(name). When the
        // disambiguator suffixes a collision (e.g., the second
        // `loop` becomes `loop_42`), `syms.intern("loop_42")` would
        // create a fresh sym that doesn't match the lambda's body's
        // EnvLookup(loop_sym) → the CallSelf detection misses and
        // `loop` becomes a capture.
        let (name, mut self_sym) = match name_by_idx.get(&idx) {
            Some(n) => (n.clone(), sym_by_idx.get(&idx).copied()),
            None => (format!("__aot_lambda_{idx}"), None),
        };
        // cw-6m8: the closure-cycle leak fix moved named-let/letrec
        // self-recursion off the captured-self shape onto the lambda's
        // own `self_bind` field, so the MakeClosure+DefineLocal scan
        // above no longer finds it and `self_sym` comes out None — the
        // self-call then lowers to an unbound (null) load and aborts at
        // runtime. `self_bind` is authoritative when present, so prefer
        // it so CallSelf detection still fires.
        if let Some(sb) = lam.self_bind {
            self_sym = Some(sb);
        }
        // Phase 5.3: consume typer-derived hints when the user
        // annotated the function. Lookup by the lambda's bound
        // symbol (which the MakeClosure+SetVar scan above
        // populated). Annotated params override the safe Any
        // default, recovering the JIT/AOT specialization the
        // iter 2.15/2.16 generalization gave up.
        //
        // The typer's hint Vec length matches the user's
        // declared `param_types.len()` from the annotation; we
        // pad or truncate to `lam.params.len()` so call sites
        // never get a mismatched-length slice. Padded slots use
        // Any (same gradual default as the RC3 fallback).
        let computed_hints: Vec<cs_rir::Type> = self_sym
            .and_then(|s| typer_hints.get(&s))
            .map(|h| {
                let mut v: Vec<cs_rir::Type> = h.clone();
                v.resize(lam.params.len(), cs_rir::Type::Any);
                v
            })
            .unwrap_or_else(|| vec![cs_rir::Type::Any; lam.params.len()]);
        let hints: Option<&[cs_rir::Type]> = Some(&computed_hints);
        match cs_vm::jit_translate::bytecode_to_rir_aot_with_param_types(
            lam,
            name.as_str(),
            self_sym,
            Some(&known_globals),
            hints,
        ) {
            Ok(mut rir) => {
                // RC3 iter 2.2 Step 1 — annotate the RIR with its
                // source lambda index so cs-aot's MakeClosure
                // resolver can find this function by index.
                rir.lambda_index = Some(idx);
                // RC3 iter 2.7 — TOP-LEVEL binding sym so
                // cross-procedure references through the resolver's
                // by_name_sym table can find this function. Letrec /
                // named-let inner-lambda bindings are intentionally
                // excluded (they shouldn't be resolvable cross-fn).
                rir.name_sym = if known_globals.contains(&sym_by_idx.get(&idx).map_or(0, |s| s.0)) {
                    sym_by_idx.get(&idx).map(|s| s.0)
                } else {
                    None
                };
                // RC3 iter 2.12 — ALL binding syms (top-level OR
                // letrec / named-let). When this fn's body emits a
                // MakeClosure for an inner lambda whose captures
                // include THIS fn's own binding sym, the capture-
                // gather emits `__self_handle` (forward self-ref).
                rir.self_binding_sym = lam
                    .self_bind
                    .map(|s| s.0)
                    .or_else(|| sym_by_idx.get(&idx).map(|s| s.0));
                compatible_funcs.push(rir);
            }
            Err(e) => skipped.push((name, format!("{e:?}"))),
        }
    }

    if compatible_funcs.is_empty() {
        eprintln!("crabscheme aot --multi: no AOT-compatible top-level lambdas in {file}");
        if !skipped.is_empty() {
            eprintln!("  skipped entries:");
            for (n, r) in &skipped {
                eprintln!("    {n}: {r}");
            }
        }
        return ExitCode::from(3);
    }
    // RC3 iter 2.16 — transitive capture propagation. If function F
    // contains MakeClosure(I) for inner lambda I, and I captures sym
    // S, then F must ALSO capture S (so F can pass S as a value to
    // I's vm_alloc_aot_procedure_with_captures call). Without this,
    // the cs-aot capture-gather hits an "unresolved capture" error
    // when S isn't in F's own scope.
    //
    // Example: nqueens's `(let loop ((p placed)) ...)` lambda. Inner
    // let-body lambda captures col, row from safe?'s scope. loop
    // MakeClosures the inner-let-body but doesn't itself reference
    // col/row directly — so record_captures missed them for loop.
    // Iter 2.16 closes this with a fixed-point analysis: for each
    // lambda, walk its body for MakeClosure(I); for each capture
    // sym of I that isn't in F's locals/params/self/by_name_sym/
    // top-level globals, add it to F's captures.
    propagate_transitive_captures(&mut compatible_funcs, &known_globals);
    // Always report what got skipped so downstream MakeClosure
    // failures (which surface as "MakeClosure not yet supported" in
    // the resolver-miss case) can be traced back to their cause.
    if !skipped.is_empty() {
        eprintln!(
            "crabscheme aot --multi: {} lambda(s) skipped during translation; downstream MakeClosure of these will fail:",
            skipped.len()
        );
        for (n, r) in &skipped {
            eprintln!("    {n}: {r}");
        }
    }

    let basename = basename_no_ext(file).to_string();
    let out_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{basename}-aot")));
    // Toolchain gate. --multi is the multi-procedure path: the AOT'd
    // functions call each other through the runtime, which only the
    // level-1 (cargo+rustc) build registers. The toolchain-free level-3
    // backend handles single self-contained functions only, so there's no
    // L3 fallback here — install a toolchain, or AOT one self-recursive
    // entry at a time (plain `aot <file> --build`, which falls back to L3).
    if build && !rust_toolchain_present() {
        eprintln!(
            "crabscheme aot --multi --build: no Rust toolchain (cargo + rustc) on PATH. \
             --multi needs level 1 (cross-procedure calls go through the runtime). \
             Install rustup, drop --build to emit the project for building elsewhere, \
             or compile a single self-recursive entry with `aot <file> --build` \
             (which uses the toolchain-free level-3 backend)."
        );
        return ExitCode::from(4);
    }
    let cs_vm_path = match resolve_cs_vm_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("crabscheme aot --multi: cannot locate cs-vm sources: {e}");
            return ExitCode::from(4);
        }
    };
    let pkg_name = sanitize_pkg_name(&basename);

    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: pkg_name.clone(),
        // entry_fn_name is unused in multi mode but still required.
        entry_fn_name: compatible_funcs[0].name.clone(),
        cs_vm_dep: None,
        cs_vm_path: Some(cs_vm_path),
        multi_procedure: true,
    };

    let emitted = match emit_project(&compatible_funcs, &out_dir, &opts) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("crabscheme aot --multi: project emit error: {e}");
            return ExitCode::from(4);
        }
    };

    println!(
        "crabscheme aot --multi: emitted project at {} with {} entr{}",
        emitted.project_dir.display(),
        compatible_funcs.len(),
        if compatible_funcs.len() == 1 {
            "y"
        } else {
            "ies"
        },
    );
    println!(
        "  entries: {}",
        compatible_funcs
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if !skipped.is_empty() {
        println!(
            "  skipped {} incompatible entr{}:",
            skipped.len(),
            if skipped.len() == 1 { "y" } else { "ies" }
        );
        for (n, r) in &skipped {
            // First line of the diagnostic only — keeps the CLI output tidy.
            let summary = r.lines().next().unwrap_or(r);
            println!("    {n}: {summary}");
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
            println!(
                "  usage: {} <fn> <args...>",
                emitted.built_binary_path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(s) => {
            eprintln!("crabscheme aot --multi: cargo build failed (exit {s})");
            ExitCode::from(5)
        }
        Err(e) => {
            eprintln!("crabscheme aot --multi: cargo not found / failed to spawn: {e}");
            ExitCode::from(5)
        }
    }
}

/// RC3 Phase 6 iter 6.6: AOT-vs-JIT cross-check.
///
/// After a successful `crabscheme aot ... --build`, this helper
/// runs both the AOT'd binary AND the JIT tier on the same sample
/// args and asserts the outputs match. Mismatch = silent codegen
/// regression; the diff harness in `tests/diff_aot_vs_jit.rs` uses
/// the same pattern as test coverage.
///
/// The caller has already AOT-built the project at `<basename>-aot/`
/// (or the user's `-o` dir). We re-derive the binary path, run it,
/// then re-run via cs_runtime + JIT for the same `(entry args)`
/// invocation. On mismatch, print both outputs + exit non-zero.
#[cfg(feature = "aot")]
fn run_aot_verify(
    file: &str,
    entry: Option<&str>,
    output: Option<&str>,
    sample_args: &[&str],
) -> ExitCode {
    use std::path::PathBuf;
    use std::process::Command;

    // 1. Resolve the entry name + binary path the same way run_aot did.
    let entry_name = entry.unwrap_or_else(|| basename_no_ext(file)).to_string();
    let pkg_name = sanitize_pkg_name(&entry_name);
    let out_dir = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}-aot", basename_no_ext(file))));
    let bin_path = out_dir.join("target/release").join(&pkg_name);
    if !bin_path.exists() {
        eprintln!(
            "crabscheme aot --verify: AOT binary not found at {} (did --build succeed?)",
            bin_path.display()
        );
        return ExitCode::from(1);
    }

    // 2. Run AOT binary with sample args.
    let aot_out = Command::new(&bin_path)
        .args(sample_args)
        .output()
        .expect("AOT binary executes");
    if !aot_out.status.success() {
        eprintln!(
            "crabscheme aot --verify: AOT binary exited non-zero ({})",
            aot_out.status
        );
        return ExitCode::from(2);
    }
    let aot_stdout = String::from_utf8_lossy(&aot_out.stdout).trim().to_string();

    // 3. Run the same call through cs_runtime + JIT.
    let src = match fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("crabscheme aot --verify: cannot re-read {file}: {e}");
            return ExitCode::from(1);
        }
    };
    let mut rt = Runtime::new();
    #[cfg(feature = "jit")]
    if let Err(e) = rt.install_jit() {
        eprintln!("crabscheme aot --verify: failed to install JIT: {e}");
        return ExitCode::from(1);
    }
    if let Err(diag) = rt.eval_str_via_vm(file, &src) {
        eprintln!(
            "crabscheme aot --verify: JIT-tier eval failed on source: {}",
            diag.message
        );
        return ExitCode::from(1);
    }
    let call_expr = format!("({entry_name} {})", sample_args.join(" "));
    let v = match rt.eval_str_via_vm("<verify-call>", &call_expr) {
        Ok(v) => v,
        Err(diag) => {
            eprintln!(
                "crabscheme aot --verify: JIT-tier call `{call_expr}` failed: {}",
                diag.message
            );
            return ExitCode::from(1);
        }
    };
    let jit_stdout = match &v {
        cs_core::Value::Fixnum(n) => n.to_string(),
        // AOT today only returns Fixnums via the Nb shim; other Value
        // variants would indicate a contract mismatch with AOT's
        // emitted main shim.
        other => {
            eprintln!(
                "crabscheme aot --verify: JIT-tier returned non-Fixnum Value: {other:?} \
                 (AOT can only verify Fixnum-returning entries today)"
            );
            return ExitCode::from(1);
        }
    };

    // 4. Compare.
    if aot_stdout == jit_stdout {
        println!(
            "crabscheme aot --verify: OK — AOT and JIT agree on `({entry_name} {}): {jit_stdout}`",
            sample_args.join(" ")
        );
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "crabscheme aot --verify: MISMATCH on `({entry_name} {})`:\n  AOT: {aot_stdout:?}\n  JIT: {jit_stdout:?}\n\nThis indicates a codegen bug; please file an issue with the source + sample args.",
            sample_args.join(" ")
        );
        ExitCode::from(6)
    }
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

fn run_eval(
    src: &str,
    via_vm: bool,
    with_jit: bool,
    color: bool,
    sandbox_imports: Option<Vec<String>>,
) -> ExitCode {
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
    rt.set_sandbox_import_policy(sandbox_imports);
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

/// Typer Phase 6.3: REPL annotation support.
///
/// Parses the REPL input through a throw-away SymbolTable,
/// runs `extract_annotations`, and:
///   - returns `None` if no annotations are present (so the
///     caller short-circuits and feeds the original text to
///     eval, no double-parse needed downstream),
///   - otherwise runs the Checker, prints diagnostics to
///     stderr, and returns the stringified stripped Datums
///     (which cs-expand can consume without choking on the
///     `:` markers).
///
/// Returns `None` on any parse error too — let the regular
/// eval path report it through cs-runtime's own diagnostic
/// machinery, so we don't double-print.
#[cfg(feature = "aot")]
fn typecheck_repl_input(src: &str, name: &str, color: bool) -> Option<String> {
    use std::collections::HashMap;

    use cs_core::SymbolTable;
    use cs_expand::Expander;
    use cs_parse::read_all;

    let mut sources = SourceMap::new();
    let file_id = sources.add(name, src);
    let mut syms = SymbolTable::new();
    let data = read_all(file_id, src, &mut syms).ok()?;
    let (stripped, table, ann_diags) = cs_typer::extract_annotations(&data, &mut syms);
    if table.is_empty() && ann_diags.is_empty() {
        // Untyped input — pass-through.
        return None;
    }
    for d in &ann_diags {
        eprintln!("{}", render_diag(d, &sources, color));
    }
    // Run the checker on the expanded form. Errors only print
    // diagnostics; we still eval (so the user can iterate on
    // a partially-typechecked snippet). A future flag could
    // gate eval on a clean check.
    let mut macros: HashMap<cs_core::Symbol, cs_expand::Macro> = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = expander.expand_program(&stripped).ok()?;
    drop(expander);
    let mut checker = cs_typer::Checker::new(&table, &mut syms);
    let type_errors = checker.check_program(&core);
    for err in &type_errors {
        let diag = err.to_diagnostic();
        eprintln!("{}", render_diag(&diag, &sources, color));
    }
    // Stringify stripped Datums via `format_with` so cs-expand
    // (called inside rt.eval_str) sees a clean form.
    let mut out = String::new();
    for d in &stripped {
        out.push_str(&d.format_with(&syms));
        out.push('\n');
    }
    Some(out)
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
        let mut to_eval = std::mem::take(&mut buffer);
        // Phase 6.3: typer pre-pass for annotated REPL input.
        // We parse once with a throw-away SymbolTable, run
        // extract_annotations, and only if annotations are
        // present do we (a) run the Checker and surface
        // diagnostics, (b) replace `to_eval` with the
        // stringified stripped form so cs-expand never sees
        // `[x : T]` markers. Untyped input is left untouched
        // and the parse is "wasted" — a fine tradeoff for an
        // interactive REPL.
        #[cfg(feature = "aot")]
        {
            to_eval = typecheck_repl_input(&to_eval, &name, color).unwrap_or(to_eval);
        }
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
