# CrabScheme AOT Compiler — User Guide

> Status: RC2 work. The AOT pipeline ships as `crabscheme aot
> prog.scm`; see `docs/milestones/m10-trackA-exit.md` for the
> M10 close-out and `docs/measurements/2026-05-16-rc2-aot-*` for
> per-iter perf and coverage data.

## What it does

`crabscheme aot prog.scm --build` compiles a Scheme source file
into a standalone native binary via:

```
prog.scm
   │
   ├──▶ cs_parse::read_all                (lex + read)
   ├──▶ cs_expand::Expander::expand_program  (macro expand)
   ├──▶ cs_vm::compile_with_globals_and_primops  (Scheme → bytecode)
   ├──▶ cs_vm::jit_translate::bytecode_to_rir_aot
   │       └─ JIT translator + demote-env-to-SSA pass
   ├──▶ cs_rir::Function                  (the IR JIT and AOT share)
   ├──▶ cs_aot::project::emit_project     (Cargo.toml + src/main.rs)
   ├──▶ cargo build --release             (rustc compiles emitted source)
   └──▶ native binary at `<basename>-aot/target/release/<entry>`
```

The resulting binary runs without any Scheme runtime install —
it's a single self-contained ELF / Mach-O.

## Quickstart

```bash
# Single-define source file.
$ cat > fact.scm <<'EOF'
(define (fact n) (if (= n 0) 1 (* n (fact (- n 1)))))
EOF
$ crabscheme aot fact.scm --build
crabscheme aot: emitted project at fact-aot
  entry: fact
  package: fact
  building (cargo build --release)...
  built: fact-aot/target/release/fact

$ ./fact-aot/target/release/fact 10
3628800
```

```bash
# Multi-define source — pick the entry by name.
$ cat > multi.scm <<'EOF'
(define (square n) (* n n))
(define (cube n) (* n (* n n)))
EOF
$ crabscheme aot multi.scm --entry cube --build
$ ./multi-aot/target/release/cube 5  # → 125
```

## CLI flags

```
crabscheme aot <file> [flags]

  -o, --output DIR        Where to write the cargo project (default <basename>-aot/)
      --entry NAME        Top-level (define (NAME args) ...) to use as entry
                          (default: file basename, falling back to first define)
      --build             Also invoke `cargo build --release` and print binary path
      --emit-rir          Print the post-translate RIR to stdout (debug aid)
      --emit-rust-source  Print the emitted src/main.rs to stdout (debug aid)
```

## What works

cs-aot's iter-as-of-RC2 supported `cs_rir::Inst` set:

### Arithmetic
- Integer: `LoadConst(Fixnum)`, `Add`, `Sub`, `Mul`, `Div` (wrapping;
  Nb mode delegates to runtime for Rational result handling)
- Comparisons: `Lt`, `Eq`
- Bit pattern: `IntCharBitcast`

### Flonum (IEEE-754)
- Arith: `FlonumAdd`, `FlonumSub`, `FlonumMul`, `FlonumDiv`
- Compare: `FlonumLt`, `FlonumEq`
- Unary: `FlonumSqrt`, `FlonumAbs`, `FlonumFloor`, `FlonumCeil`,
  `FlonumTrunc`, `FlonumRound`, `FlonumSin`, `FlonumCos`,
  `FlonumTan`, `FlonumLog`, `FlonumExp`, `FlonumAsin`,
  `FlonumAcos`, `FlonumAtan`
- Binary: `FlonumMax`, `FlonumMin`, `FlonumLog2` (n.log(base)),
  `FlonumAtan2`, `FlonumExpt` (n.powf(base))
- Predicates: `FlonumIsNan`, `FlonumIsInfinite`
- Type promotions: `FixToFlo`

### Type predicates
`PairP`, `NullP`, `VecP`, `ProcedureP`, `SymbolP`, `FixnumP`, `FlonumP`

### Vectors
`VecAlloc`, `VecRef`, `VecSet`, `VecLength`

### Pairs
`Cons` (with NB tag bytes), `Car`, `Cdr`

### Equality
`EqAny` (`eq?`), `EqualAny` (`equal?`)

### NB carrier ops (identity in uniform-NB ABI)
`Move`, `BoxTyped`, `AnyToFix`, `AnyToBool`, `AnyToFlo`,
`AnyTruthy`, `AnyClone`

### Control flow
- `CallSelf` (recursive call to the function being emitted)
- Terminators: `Return`, `Jump` (with block-param args), `Branch`
- `let` bindings in single-block functions (via the demote-env-to-SSA pass)

## What doesn't work yet

| Construct | Blocker | Tracking |
|-----------|---------|----------|
| Non-self procedure calls | `Inst::Call` / `Inst::CallGeneral` need Procedure heap + dispatch | iter K (post-RC2) |
| Nested lambdas / closures | `Inst::MakeClosure` needs Procedure heap | iter K |
| Free variables (cross-procedure refs) | `Inst::EnvLookupAny` needs JIT_CALLER_ENV API | iter U |
| Multi-block `let` bindings | Demote pass only handles single-block | iter O (deferred) |
| Top-level side effects | `(display ...)` outside of `(define ...)` | iter Q |
| Mutually recursive defines | Global Procedure registry | iter K + W |
| Strings / bytevectors | Most string Insts not yet lowered | future |
| `set!` on globals | `Inst::EnvSet` needs JIT_CALLER_ENV API | iter U |
| Browser WASM | `wasm32-unknown-unknown` (no WASI) | future |

When you hit one of these, the CLI prints:

```
crabscheme aot: project emit error: cs-aot project: emit error in function `f`: cs-aot: Inst::<name> not yet supported (iter 1)
```

The `<name>` tells you which Inst the bytecode→RIR translator emitted
that cs-aot doesn't yet handle. Use `--emit-rir` to see the full
RIR — sometimes a slightly different Scheme phrasing avoids the
unsupported Inst (e.g., a `let` that's pure SSA-style demotes
cleanly; one that's captured by a closure surfaces `MakeClosure`).

## Debugging

```bash
# See what RIR the translator emitted.
$ crabscheme aot prog.scm --emit-rir

# See what Rust source got generated.
$ crabscheme aot prog.scm --emit-rust-source

# Both, plus actually build.
$ crabscheme aot prog.scm --emit-rir --emit-rust-source --build
```

When a generated binary returns the wrong result, the typical
sequence is:

1. Run with `--emit-rir` to confirm the RIR matches the source.
2. Run with `--emit-rust-source` to inspect the generated Rust.
3. Compare the generated Rust against what `rustc -O` of a
   hand-written equivalent would produce.
4. The two NB ABI modes (`RawI64` and `Nb`) lower differently;
   `RawI64` (which the CLI doesn't expose yet — internal) skips
   NB encode/decode entirely for self-contained Fixnum kernels.

## Performance

For self-recursive Fixnum kernels (fib, fact, ack):

- `RawI64` ABI matches `rustc -O fib.rs` to the centisecond on
  fib(40) — emits the same x86_64 add/sub/mul/cmp.
- `Nb` ABI (the default, used by the CLI) runs ~2× slower than
  reference Rust on fib post-RC2 inline-fast-path work — every
  arith op is a tag-check + checked-arith + NB-encode helper.
  The remaining gap is dynamic tag checks the JIT defers to a
  per-function type guard; AOT doesn't yet have a type-feedback
  channel.

See `docs/measurements/2026-05-16-rc2-aot-nb-inline.md` for the
detailed RC2 perf numbers.

## How to extend

Adding a new `cs_rir::Inst` variant to cs-aot is mechanical:

1. **Find the runtime helper** in `cs-vm/src/vm.rs` that the
   JIT calls for this Inst. Most heap-touching ones have a
   `vm_<name>_gc` shape: `vm_pair_p_gc(i64) -> i64`,
   `vm_alloc_vector_gc(i64, i64) -> i64`, etc.
2. **Add an arm in `inst_rhs`** at `crates/cs-aot/src/lib.rs`.
   The pattern is:
   ```rust
   (Inst::Foo(dst, src), _) => {
       check(*src)?;
       (*dst, format!("unsafe {{ cs_vm::vm::vm_foo(v{}) }}", src.0))
   }
   ```
   For predicates returning 0/1 i64, use the `tpred_rust(...)`
   or `fpredicate_*_rust(...)` helpers to wrap as NB Boolean in
   Nb mode.
3. **Add the dst to `inst_dst`** so the loop+match shape
   pre-declares it as a `let mut`.
4. **Add a smoke test** in `crates/cs-aot/src/lib.rs`'s `tests`
   module, or an e2e test in `crates/cs-aot/tests/`.

The iter-L through iter-T commits each follow this template
exactly — git log `--grep RC2 iter` for live examples.
