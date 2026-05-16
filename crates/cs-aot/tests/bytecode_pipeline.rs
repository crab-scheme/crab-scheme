//! RC2 iter E proof-of-concept: pipe a `CompiledLambda` through
//! `cs_vm::jit_translate::bytecode_to_rir` and feed the resulting
//! `cs_rir::Function` straight into cs-aot's project emitter.
//!
//! Hypothesis: the bytecode→RIR translator (originally written for
//! the JIT) already produces RIR that fits within cs-aot's
//! supported Inst set for simple self-recursive numeric kernels.
//! If this test passes, we have the bytecode→RIR glue for free —
//! AOT can compile any Scheme program whose bytecode→RIR output
//! lands in the supported subset.
//!
//! What this skips intentionally (vs a full `crabscheme aot
//! prog.scm`):
//!
//! - Source → CoreExpr → Bytecode parsing chain. Driven separately;
//!   not a new failure surface to test here.
//! - Top-level vs lambda distinction. Real programs have a top-level
//!   bytecode that defines + invokes lambdas; this test starts from
//!   a single `CompiledLambda` (the recursive function's body) and
//!   drives it standalone.
//!
//! If this test PASSES, RC2's "compile a Scheme program" gap is
//! reduced to the source→bytecode→entry-point glue + a CLI wiring
//! pass — both small. If it FAILS, the error tells us exactly
//! which Insts the translator emits that cs-aot doesn't yet
//! handle; those become the iter-E TODO list.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

use cs_aot::project::{emit_project, ProjectOptions};
use cs_aot::EmitMode;
use cs_core::{Number, SymbolTable, Value};
use cs_diag::Span;
use cs_vm::jit_translate::bytecode_to_rir;
use cs_vm::opcode::{CompiledLambda, Inst as VmInst};

/// Hand-built bytecode for:
///   (define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))
///
/// Bytecode shape (mirrors the cs-jit-cranelift fib_lambda
/// template):
///
///   00 LoadVar(n)
///   01 Const(0)
///   02 EqFx2                ; (= n 0)
///   03 JumpIfFalse(6)       ; → recurse branch
///   04 Const(1)
///   05 Return
///   06 LoadVar(n)
///   07 LoadVar(fact)
///   08 LoadVar(n)
///   09 Const(1)
///   10 SubFx2               ; (- n 1)
///   11 Call(1)              ; (fact (- n 1))
///   12 MulFx2               ; (* n (fact ...))
///   13 Return
fn fact_lambda(syms: &mut SymbolTable) -> (CompiledLambda, cs_core::Symbol) {
    let n = syms.intern("n");
    let fact = syms.intern("fact");
    let body = vec![
        VmInst::LoadVar(n),
        VmInst::Const(Value::Number(Number::Fixnum(0))),
        VmInst::EqFx2,
        VmInst::JumpIfFalse(6),
        VmInst::Const(Value::Number(Number::Fixnum(1))),
        VmInst::Return,
        VmInst::LoadVar(n),
        VmInst::LoadVar(fact),
        VmInst::LoadVar(n),
        VmInst::Const(Value::Number(Number::Fixnum(1))),
        VmInst::SubFx2,
        VmInst::Call(1),
        VmInst::MulFx2,
        VmInst::Return,
    ];
    let len = body.len();
    let lam = CompiledLambda {
        params: vec![n],
        rest: None,
        body: Rc::new(body),
        spans: Rc::new(vec![Span::DUMMY; len]),
        fast: None,
        profile: Default::default(),
    };
    (lam, fact)
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
        .join("target/aot-bytecode-pipeline-tests")
}

#[test]
fn bytecode_to_rir_to_aot_compiles_fact_end_to_end() {
    // Translate fact bytecode → RIR.
    let mut syms = SymbolTable::new();
    let (lam, fact_sym) = fact_lambda(&mut syms);
    let rir = bytecode_to_rir(&lam, "fact", Some(fact_sym))
        .expect("bytecode_to_rir should accept this lambda");

    // Sanity: the translator returned a `Function` with at least
    // one block. If it ever produces something cs-aot can't emit,
    // the next call (emit_project) will surface AotError::Unsupported
    // {Inst,Term} with the variant name — that's the diagnostic the
    // iter-E TODO list comes from.
    assert!(
        !rir.blocks.is_empty(),
        "bytecode_to_rir returned an empty function: {rir:?}"
    );

    // Compile the RIR through cs-aot's project emitter.
    let pid = std::process::id();
    let tmpdir = std::env::temp_dir().join(format!("cs-aot-bytecode-fact-{pid}"));
    let _ = std::fs::remove_dir_all(&tmpdir);

    let opts = ProjectOptions {
        mode: EmitMode::Nb,
        package_name: "aot_fact_bytecode".to_string(),
        entry_fn_name: "fact".to_string(),
        cs_vm_path: Some(cs_vm_workspace_path()),
    };

    let emitted = match emit_project(&[rir.clone()], &tmpdir, &opts) {
        Ok(e) => e,
        Err(e) => {
            // Convert the AotError into a diagnostic that tells us
            // *which* Inst the translator emits that we don't yet
            // handle in cs-aot. This is the iter-E TODO surface.
            panic!(
                "emit_project failed on bytecode_to_rir output for fact:\n\
                 error: {e}\n\
                 RIR blocks: {:?}\n",
                rir.blocks
            );
        }
    };

    // Build via cargo.
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
        "cargo build failed on bytecode-pipeline-emitted source:\n--- stderr ---\n{}\n--- stdout ---\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    let bin = target_dir.join(format!("release/{bin_name}"));

    let run = |n: i64| -> i64 {
        let out = Command::new(&bin)
            .arg(n.to_string())
            .output()
            .expect("binary executes");
        assert!(out.status.success(), "fact binary failed: {out:?}");
        String::from_utf8(out.stdout)
            .expect("utf8")
            .trim()
            .parse::<i64>()
            .expect("i64 parse")
    };

    assert_eq!(run(0), 1);
    assert_eq!(run(1), 1);
    assert_eq!(run(5), 120);
    assert_eq!(run(10), 3628800);
    assert_eq!(run(12), 479001600);
}
