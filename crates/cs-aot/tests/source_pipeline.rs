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
use cs_vm::jit_translate::{bytecode_to_rir_aot, bytecode_to_rir_aot_with_globals};

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
/// return the CompiledLambda matching `entry_name` (defined as
/// `(define (<entry_name> args...) body)` at the top level). Uses
/// the iter-H MakeClosure+SetVar scan to map names to lambda
/// indices. Panics with a useful diagnostic if the source fails
/// to parse/expand/compile, or if the entry isn't found.
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

    assert!(
        !bc.lambdas.is_empty(),
        "expected at least one CompiledLambda in bytecode for `{src}`, got 0"
    );

    let entry_sym = syms.intern(entry_name);
    let idx = lambda_index_by_name(&bc, entry_sym).unwrap_or_else(|| {
        let available = lambda_names_in_bytecode(&bc, &syms);
        panic!("entry `{entry_name}` not found among top-level defines; available: {available:?}")
    });
    (bc.lambdas[idx].clone(), entry_sym)
}

/// Walk top-level bytecode looking for the iter-H define pattern:
///   ... Inst::MakeClosure(i) | Inst::SetVar(sym) ...
/// Returns the lambda index that gets bound to `target_sym`, if any.
///
/// The compiler emits `(define (f args) body)` as the sequence
/// `MakeClosure(i)` followed directly by `SetVar(sym)` where `sym`
/// is the global name. There may be no intervening Insts in the
/// common case; we look for an adjacent SetVar to keep the matcher
/// tight (false positives would be confusing if a user picked a
/// name that happens to be reused as a SetVar target after some
/// unrelated MakeClosure).
fn lambda_index_by_name(bc: &cs_vm::opcode::Bytecode, target_sym: Symbol) -> Option<usize> {
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

/// Enumerate all top-level-bound lambda names for diagnostic
/// purposes. Same scan as `lambda_index_by_name` but accumulates.
fn lambda_names_in_bytecode(bc: &cs_vm::opcode::Bytecode, syms: &SymbolTable) -> Vec<String> {
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
    let rir = bytecode_to_rir_aot(&lam, fn_name, Some(entry_sym))
        .expect("bytecode_to_rir succeeds on source-derived lambda");

    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-source-{pkg_suffix}-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);

    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: format!("aot_src_{pkg_suffix}"),
        entry_fn_name: fn_name.to_string(),
        cs_vm_dep: None,
        cs_vm_path: Some(cs_vm_workspace_path()),
        multi_procedure: false,
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

#[test]
fn source_to_aot_picks_entry_by_name_in_multi_define() {
    // RC2 iter H — multi-define source. Two top-level defines
    // (`square` and `cube`); --entry-equivalent selects `cube`
    // which only self-references (the `(* n (* n n))` body
    // doesn't call square — that would surface as an unsupported
    // EnvLookup or general Call since AOT can't yet do cross-
    // procedure references).
    //
    // The lambda_index_by_name scanner finds cube's lambda index
    // by walking MakeClosure(i)+SetVar(sym) pairs in the top-level
    // bytecode. Without the iter-H lookup we'd default to lambdas[0]
    // (square) and the assertion would fail because square(3)=9 not 27.
    let src = "
        (define (square n) (* n n))
        (define (cube n) (* n (* n n)))
    ";
    let (lam, sym) = compile_source_to_lambda(src, "cube");
    let bin = aot_compile_and_run(lam, sym, "cube", "cube_multi");
    assert_eq!(run_with_arg(&bin, 0), 0);
    assert_eq!(run_with_arg(&bin, 3), 27);
    assert_eq!(run_with_arg(&bin, 5), 125);
    assert_eq!(run_with_arg(&bin, 10), 1000);
}

#[test]
fn source_to_aot_function_with_let_binding() {
    // RC2 iter J — `let` inside a function body. Pre-iter-J, the
    // bytecode→RIR translator emitted `EnvDefineLocal +
    // EnvLookupAny + AnyToFix` for the binding, which surfaced
    // as `UnsupportedInst` in cs-aot. Post-iter-J, `bytecode_to_
    // rir_aot` demotes the env round-trip to SSA aliases and the
    // identity-in-NB ops (AnyToFix etc.) lower to Move.
    //
    // `(define (f n) (let ((doubled (* n 2))) (+ doubled 1)))`
    // is the smallest test of this — exercises both
    // EnvDefineLocal + EnvLookupAny in one let-binding.
    let (lam, sym) = compile_source_to_lambda(
        "(define (f n) (let ((doubled (* n 2))) (+ doubled 1)))",
        "f",
    );
    let bin = aot_compile_and_run(lam, sym, "f", "let_doubled");
    assert_eq!(run_with_arg(&bin, 0), 1);
    assert_eq!(run_with_arg(&bin, 5), 11);
    assert_eq!(run_with_arg(&bin, 10), 21);
}

#[test]
fn source_to_aot_function_with_let_then_branch() {
    // RC3 Phase 2 iter 2.5 — multi-block demote. The `if`
    // introduces multiple blocks; the `let` binding in the entry
    // block needs to flow as an SSA alias into BOTH the then-
    // and else- arms via the cross-block alias map.
    //
    // (define (h n)
    //   (let ((doubled (* n 2)))
    //     (if (< doubled 100)
    //         doubled
    //         (* doubled 2))))
    //
    // → h(n) = min(2n, 4n). Catches the multi-block alias
    // propagation: the EnvLookupAny(doubled) reference inside
    // each branch must resolve to the entry-block Mul's result.
    let (lam, sym) = compile_source_to_lambda(
        "(define (h n) (let ((doubled (* n 2))) (if (< doubled 100) doubled (* doubled 2))))",
        "h",
    );
    let bin = aot_compile_and_run(lam, sym, "h", "let_then_branch");
    // (< doubled 100) is strict less-than, so doubled == 100 → else branch.
    assert_eq!(run_with_arg(&bin, 0), 0); // doubled=0; 0<100 → 0
    assert_eq!(run_with_arg(&bin, 10), 20); // doubled=20; 20<100 → 20
    assert_eq!(run_with_arg(&bin, 49), 98); // doubled=98; 98<100 → 98
    assert_eq!(run_with_arg(&bin, 50), 200); // doubled=100; !<100 → 200
    assert_eq!(run_with_arg(&bin, 100), 400); // doubled=200; !<100 → 400
}

#[test]
fn source_to_aot_function_with_nested_lets() {
    // Two let bindings in sequence — iter J's demote pass needs
    // to handle a chain of EnvDefineLocal+EnvLookupAny round-trips,
    // not just one. Both bindings live as SSA aliases after
    // demotion.
    //
    // `(define (g n)
    //    (let ((a (* n n)))
    //      (let ((b (+ a 1)))
    //        (- b 1))))`
    // → g(n) = n*n. Verifies the chained-let path lands clean.
    let (lam, sym) = compile_source_to_lambda(
        "(define (g n) (let ((a (* n n))) (let ((b (+ a 1))) (- b 1))))",
        "g",
    );
    let bin = aot_compile_and_run(lam, sym, "g", "nested_let");
    assert_eq!(run_with_arg(&bin, 0), 0);
    assert_eq!(run_with_arg(&bin, 3), 9);
    assert_eq!(run_with_arg(&bin, 7), 49);
}

/// RC3 iter 2.4 — multi-procedure AOT helper. Translates *every*
/// lambda in the bytecode (not just one entry) and emits a single
/// binary with multi-procedure dispatch. Returns the binary path
/// + the original Scheme name of the first top-level entry so the
/// caller can invoke `<bin> <name> <args...>`.
fn aot_compile_multi_and_run(src: &str, entry_name: &str, pkg_suffix: &str) -> PathBuf {
    let mut sources = SourceMap::new();
    let file_id = sources.add("<test>", src);
    let mut syms = SymbolTable::new();
    let data = read_all(file_id, src, &mut syms).expect("read_all");
    let mut macros = HashMap::new();
    let mut expander = Expander::new(&mut syms, &mut macros);
    let core = expander.expand_program(&data).expect("expand");
    drop(expander);
    let globals = HashMap::new();
    let primops = build_primops(&mut syms);
    let bc = compile_with_globals_and_primops(&core, &globals, &primops).expect("compile");

    // Mirror cs-cli's --multi scans (top-level + per-lambda) so
    // letrec / named-let inner-lambda self-name detection works.
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
    let mut name_by_idx: HashMap<usize, String> = HashMap::new();
    let mut sym_by_idx: HashMap<usize, cs_core::Symbol> = HashMap::new();
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
    let known_globals: std::collections::HashSet<u32> = scan_pairs(&bc.insts)
        .into_iter()
        .map(|(_, s)| s.0)
        .collect();
    let mut compatible: Vec<cs_rir::Function> = Vec::new();
    for (idx, lam) in bc.lambdas.iter().enumerate() {
        let (name, self_sym) = match name_by_idx.get(&idx) {
            Some(n) => (n.clone(), Some(syms.intern(n))),
            None => (format!("__aot_lambda_{idx}"), None),
        };
        let mut rir =
            bytecode_to_rir_aot_with_globals(lam, name.as_str(), self_sym, Some(&known_globals))
                .unwrap_or_else(|e| panic!("bytecode_to_rir_aot failed on lambda {idx}: {e:?}"));
        rir.lambda_index = Some(idx);
        rir.name_sym = if known_globals.contains(&sym_by_idx.get(&idx).map_or(0, |s| s.0)) {
            sym_by_idx.get(&idx).map(|s| s.0)
        } else {
            None
        };
        compatible.push(rir);
    }
    assert!(
        !compatible.is_empty(),
        "no AOT-compatible lambdas in source"
    );

    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-multi-{pkg_suffix}-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);

    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: format!("aot_multi_{pkg_suffix}"),
        entry_fn_name: entry_name.to_string(),
        cs_vm_dep: None,
        cs_vm_path: Some(cs_vm_workspace_path()),
        multi_procedure: true,
    };

    let emitted = emit_project(&compatible, &tmpdir, &opts)
        .unwrap_or_else(|e| panic!("emit_project failed for multi {pkg_suffix}: {e}"));

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
        "cargo build failed for multi {pkg_suffix}:\n--- stderr ---\n{}\n",
        String::from_utf8_lossy(&output.stderr),
    );
    target_dir.join(format!("release/{bin_name}"))
}

fn run_multi_with_args(bin: &PathBuf, entry: &str, args: &[i64]) -> i64 {
    let mut cmd = Command::new(bin);
    cmd.arg(entry);
    for a in args {
        cmd.arg(a.to_string());
    }
    let out = cmd.output().expect("binary executes");
    assert!(
        out.status.success(),
        "binary failed: stdout=`{}` stderr=`{}`",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .parse::<i64>()
        .expect("i64 parse")
}

#[test]
fn source_to_aot_named_let_self_recursion() {
    // RC3 iter 2.9 — named-let with an internally-recursive inner
    // lambda. The named-let desugars to a letrec; the translator's
    // self-name detection (extended in 2.9 to scan ALL lambdas'
    // bodies for MakeClosure+DefineLocal pairs) passes `loop_sym`
    // as the inner lambda's self_name. CallSelf detection then
    // covers the recursive `(loop ...)` calls, so the inner lambda
    // doesn't need to capture itself.
    //
    // Also exercises the proc_loop ident remapping (since `loop` is
    // a Rust keyword) — sanitize_ident now guards against keywords.
    let bin = aot_compile_multi_and_run(
        "(define (sum-to n) (let loop ((i 0) (acc 0)) \
           (if (> i n) acc (loop (+ i 1) (+ acc i)))))",
        "sum-to",
        "named_let",
    );
    assert_eq!(run_multi_with_args(&bin, "sum-to", &[0]), 0);
    assert_eq!(run_multi_with_args(&bin, "sum-to", &[5]), 15);
    assert_eq!(run_multi_with_args(&bin, "sum-to", &[10]), 55);
    assert_eq!(run_multi_with_args(&bin, "sum-to", &[100]), 5050);
}

#[test]
fn source_to_aot_mutual_recursion() {
    // RC3 iter 2.8 (free-rides on iter 2.7) — mutual recursion
    // between two top-level fns. `even?` calls `odd?` calls `even?`.
    // The cross-procedure-reference mechanism resolves each call
    // through the LambdaResolver's by_name_sym table; mutual
    // recursion just works because both fns are emitted into the
    // same project with the same resolver.
    let bin = aot_compile_multi_and_run(
        "(define (even? n) (if (= n 0) 1 (odd? (- n 1)))) \
         (define (odd? n) (if (= n 0) 0 (even? (- n 1))))",
        "even?",
        "mutual_rec",
    );
    assert_eq!(run_multi_with_args(&bin, "even?", &[0]), 1);
    assert_eq!(run_multi_with_args(&bin, "even?", &[4]), 1);
    assert_eq!(run_multi_with_args(&bin, "even?", &[7]), 0);
    assert_eq!(run_multi_with_args(&bin, "odd?", &[1]), 1);
    assert_eq!(run_multi_with_args(&bin, "odd?", &[5]), 1);
    assert_eq!(run_multi_with_args(&bin, "odd?", &[8]), 0);
}

#[test]
fn source_to_aot_cross_procedure_reference() {
    // RC3 iter 2.7 — top-level cross-procedure references. `apply-
    // double` calls `double` by name; the bytecode emits
    // `EnvLookupAny(double_sym) + CallGeneral`. With the iter-2.7
    // resolver, the demote-surviving EnvLookup resolves through
    // `LambdaResolver::by_name_sym` to a direct
    // `vm_alloc_aot_procedure(double_aot_dispatch, 1)` call instead
    // of failing on a missing capture.
    //
    // Exercises:
    //   1. translator's known_globals excludes `double` from
    //      `apply-double`'s captures list
    //   2. cs-aot's EnvLookup arm emits the vm_alloc call directly
    //   3. CallGeneral routes through vm_call_aot_procedure to the
    //      other AOT'd fn's dispatch wrapper
    let bin = aot_compile_multi_and_run(
        "(define (double x) (* x 2)) (define (apply-double n) (double n))",
        "apply-double",
        "cross_proc",
    );
    assert_eq!(run_multi_with_args(&bin, "double", &[3]), 6);
    assert_eq!(run_multi_with_args(&bin, "apply-double", &[5]), 10);
    assert_eq!(run_multi_with_args(&bin, "apply-double", &[100]), 200);
    assert_eq!(run_multi_with_args(&bin, "apply-double", &[-7]), -14);
}

#[test]
fn source_to_aot_capturing_closure_inline_call() {
    // RC3 iter 2.4 — capturing closure end-to-end. The outer
    // `adder` builds an inner lambda that captures `x`, then
    // immediately calls it with `y`.
    //
    // Exercises the full new path:
    //   1. translator's record_captures populates inner.captures = [x_sym]
    //   2. cs-aot emits inner with `__cap<x_sym>: i64` prefix param
    //   3. outer's MakeClosure resolves the inner's capture to the
    //      outer's `x` value and calls
    //      vm_alloc_aot_procedure_with_captures
    //   4. outer's Call routes through vm_call_aot_procedure which
    //      forwards the captures slice to the inner's dispatch
    //      wrapper
    //   5. inner's body reads `__cap<x_sym>` and adds it to `z`
    let bin = aot_compile_multi_and_run(
        "(define (adder x y) ((lambda (z) (+ x z)) y))",
        "adder",
        "capturing_inline",
    );
    assert_eq!(run_multi_with_args(&bin, "adder", &[5, 10]), 15);
    assert_eq!(run_multi_with_args(&bin, "adder", &[0, 0]), 0);
    assert_eq!(run_multi_with_args(&bin, "adder", &[100, -50]), 50);
    assert_eq!(run_multi_with_args(&bin, "adder", &[-7, -3]), -10);
}

#[test]
fn compile_source_diagnostics_list_available_entries_on_typo() {
    // Negative-path coverage: if --entry NAME doesn't match any
    // top-level define, the helper panics with the available names
    // for friendly error reporting. Exercises the
    // lambda_names_in_bytecode helper end-to-end.
    let src = "
        (define (sq n) (* n n))
        (define (cb n) (* n (* n n)))
    ";
    let result = std::panic::catch_unwind(|| compile_source_to_lambda(src, "nonexistent"));
    let msg = result
        .err()
        .and_then(|payload| {
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        })
        .expect("expected a panic with a message");
    assert!(
        msg.contains("nonexistent") && msg.contains("sq") && msg.contains("cb"),
        "expected diagnostic to mention the requested + available entries, got: {msg}"
    );
}
