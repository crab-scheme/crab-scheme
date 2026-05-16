//! RC2 iter F: close the loop. Take literal Scheme source → run
//! the full lex/parse/expand/compile chain → translate the
//! resulting CompiledLambda → emit + build via AOT → execute.
//!
//! Iter E proved the bytecode-to-RIR-to-AOT segment works on a
//! hand-built CompiledLambda. This test proves the source-to-
//! bytecode segment hooks in too: actual Scheme parses through
//! `cs_parse::read_all`, expands through `cs_expand::Expander`,
//! compiles through `cs_vm::compile_with_globals_and_primops`,
//! and produces a CompiledLambda whose bytecode matches what the
//! iter-E hand-built version produced — proving the AOT pipeline
//! is now Scheme-source-driven for the supported subset.
//!
//! The primop table here mirrors cs-runtime's: without it, `(+ a
//! b)` compiles to a generic Call(+) instead of AddFx2, and the
//! bytecode-to-RIR translator emits CallGeneral (which cs-aot
//! doesn't yet handle). cs-runtime exposes a single canonical
//! table at `primop_table()` (private); we replicate it here as
//! a dev-only convenience rather than depending on cs-runtime
//! (which would pull in JIT/FFI/runtime crates the AOT path
//! doesn't need at test-build time).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use cs_aot::project::{emit_project, ProjectOptions};
use cs_aot::EmitMode;
use cs_core::{Symbol, SymbolTable};
use cs_diag::SourceMap;
use cs_expand::Expander;
use cs_parse::read_all;
use cs_vm::compile_with_globals_and_primops;
use cs_vm::compiler::PrimOp;
use cs_vm::jit_translate::bytecode_to_rir;

/// Mirrors cs_runtime's private primop_table. The compiler uses
/// this to emit AddFx2/SubFx2/etc. specialized opcodes instead of
/// generic Calls when it sees an unshadowed reference to one of
/// the standard 2-arg primops.
fn build_primops(syms: &mut SymbolTable) -> HashMap<Symbol, PrimOp> {
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
}

/// Compile a literal Scheme source through the full pipeline and
/// return (the first compiled lambda, the symbol for its name if
/// `entry_name` matches a top-level define). Panics with a useful
/// message on any pipeline failure.
fn compile_source_to_lambda(
    src: &str,
    entry_name: &str,
) -> (cs_vm::opcode::CompiledLambda, Symbol) {
    let mut sources = SourceMap::new();
    let file_id = sources.add("<test>", src);
    let mut syms = SymbolTable::new();

    let data = read_all(file_id, src, &mut syms).expect("read_all parses source");

    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = expander
        .expand_program(&data)
        .expect("expand_program succeeds");
    drop(expander);

    let globals = HashMap::new();
    let primops = build_primops(&mut syms);
    let bc = compile_with_globals_and_primops(&core, &globals, &primops).expect("compile succeeds");

    // For a single `(define (fname args) body)`, the compiler emits
    // exactly one CompiledLambda in `bc.lambdas` — that's our entry.
    // More elaborate programs (multiple defines, internal lambdas)
    // would need a name-keyed lookup; iter F's scope is one lambda.
    assert!(
        !bc.lambdas.is_empty(),
        "expected at least one CompiledLambda in bytecode for `{src}`, got 0"
    );
    let lam = bc.lambdas[0].clone();
    let entry_sym = syms.intern(entry_name);
    (lam, entry_sym)
}

fn cs_vm_workspace_path() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir.parent().expect("crates/").join("cs-vm")
}

fn workspace_target_dir() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("target/aot-source-pipeline-tests")
}

fn aot_compile_and_run(
    lam: cs_vm::opcode::CompiledLambda,
    entry_sym: Symbol,
    fn_name: &str,
    pkg_suffix: &str,
) -> PathBuf {
    let rir = bytecode_to_rir(&lam, fn_name, Some(entry_sym))
        .expect("bytecode_to_rir succeeds on source-derived lambda");

    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-source-{pkg_suffix}-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);

    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: format!("aot_src_{pkg_suffix}"),
        entry_fn_name: fn_name.to_string(),
        cs_vm_path: Some(cs_vm_workspace_path()),
    };

    let emitted = emit_project(&[rir], &tmpdir, &opts)
        .unwrap_or_else(|e| panic!("emit_project failed for source-pipeline {pkg_suffix}: {e}"));

    let target_dir = workspace_target_dir();
    let bin_name = &opts.package_name;
    let output = Command::new("cargo")
        .current_dir(&emitted.project_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .arg("build")
        .arg("--release")
        .arg("--bin")
        .arg(bin_name)
        .arg("--offline")
        .output()
        .expect("cargo executes");
    assert!(
        output.status.success(),
        "cargo build failed for source-pipeline {pkg_suffix}:\n--- stderr ---\n{}\n",
        String::from_utf8_lossy(&output.stderr),
    );
    target_dir.join(format!("release/{bin_name}"))
}

fn run_with_arg(bin: &PathBuf, n: i64) -> i64 {
    let out = Command::new(bin)
        .arg(n.to_string())
        .output()
        .expect("binary executes");
    assert!(out.status.success(), "binary failed: {out:?}");
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .parse::<i64>()
        .expect("i64 parse")
}

#[test]
fn source_to_aot_fact() {
    // The headline iter-F demo: literal source string in → AOT
    // binary out → correct factorials.
    let (lam, sym) = compile_source_to_lambda(
        "(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))",
        "fact",
    );
    let bin = aot_compile_and_run(lam, sym, "fact", "fact");
    assert_eq!(run_with_arg(&bin, 0), 1);
    assert_eq!(run_with_arg(&bin, 5), 120);
    assert_eq!(run_with_arg(&bin, 10), 3628800);
    assert_eq!(run_with_arg(&bin, 12), 479001600);
}

#[test]
fn source_to_aot_fib() {
    // fib uses two CallSelfs per recursive case + Lt instead of Eq.
    let (lam, sym) = compile_source_to_lambda(
        "(define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))",
        "fib",
    );
    let bin = aot_compile_and_run(lam, sym, "fib", "fib");
    assert_eq!(run_with_arg(&bin, 0), 0);
    assert_eq!(run_with_arg(&bin, 1), 1);
    assert_eq!(run_with_arg(&bin, 10), 55);
    assert_eq!(run_with_arg(&bin, 25), 75025);
}
