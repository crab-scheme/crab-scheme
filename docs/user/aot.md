# CrabScheme AOT Compiler — User Guide

> Status: Active (post-RC2 hardening). The AOT pipeline ships as
> `crabscheme aot prog.scm`. The coverage tables below were verified
> against the built binary on 2026-06-02 (they were substantially
> narrower at RC2). See `docs/milestones/m10-trackA-exit.md` for the
> M10 close-out, `docs/milestones/aot-hardening-plan.md` for the
> ongoing hardening phases, and `docs/measurements/2026-05-16-rc2-aot-*`
> for per-iter perf and coverage data.

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

## AOT levels (toolchain-free builds)

`crabscheme aot --build` has two back-ends and picks one automatically:

| Level | When | How | Scope |
|-------|------|-----|-------|
| **1** | `cargo` + `rustc` on `PATH` | emit a Cargo project → `cargo build --release` (rustc codegen, full optimization) | everything in *What works* below, incl. multi-procedure (`--multi`) |
| **3** | no Rust toolchain (or `CRABSCHEME_AOT_FORCE_OBJECT=1`) | reuse the JIT's Cranelift lowering to emit a relocatable `.o`, then link with the system **`cc`** against the prebuilt `libcs_aot_rt.a` archive | **a single self-contained function** (self-recursion only) |

Level 3 needs only a C linker (`cc`/`clang`/`gcc`) — **no Rust toolchain**.
The pieces ship in the release tarball: the `crabscheme` binary plus
`libcs_aot_rt.a` beside it (the runtime archive of the `vm_*` helpers the
emitted object calls). `crabscheme aot-doctor` self-tests both back-ends and
reports which are usable on your machine.

```bash
# On a box with cc but no rustc/cargo, this still works:
$ crabscheme aot fib.scm --build -o fib
crabscheme aot (level 3): linking fib with cc...
crabscheme aot: built fib (level 3 — no Rust toolchain)
$ ./fib 25            # → 75025
```

**Level-3 limits (today):** one self-recursive entry. A program whose entry
calls *other* functions (`Inst::Call`) needs the runtime procedure
registration only level 1 sets up, so it declines with a pointer to install
a toolchain. `--multi` and `--target` (cross-compile) are level-1 only.
Override the archive location with `CRABSCHEME_AOT_ARCHIVE=/path/libcs_aot_rt.a`.

> The archive **must be built in its own `cargo` invocation**
> (`cargo build --release -p cs-aot-rt`): building it alongside `cs-cli`
> lets Cargo feature-unification pull stdlib-only deps (TLS/HTTP →
> `security-framework` etc.) into the archive that `cc` can't auto-link.
> The release workflow already does this in a separate step.

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
      --explain           Survey lambdas + report AOT compatibility per entry
                          (RC3 Phase 4). Doesn't emit a project; exits 0 if
                          ≥1 entry is compatible, 3 if none.
      --multi             Emit one multi-procedure binary; dispatches
                          `<binary> <fn> <args…>`. Incompatible defines are
                          skipped with a warning at emit time.
      --verify "ARGS"     After --build, run BOTH the AOT'd binary and the JIT
                          tier on ARGS and warn if the outputs disagree.
      --target TRIPLE     Cross-compile (passed to `cargo build --target`;
                          requires `rustup target add TRIPLE`). Level 1 only.
      --typecheck         Run the typer's checker before AOT; exit 1 on
                          type / arity errors instead of warn-and-proceed.

crabscheme aot-doctor                    (RC3 Phase 4)

  Self-test the AOT installation. Runs a baked-in fact program
  through the full pipeline + asserts the binary returns 120 for
  fact(5). Useful for verifying a release-installed binary works
  on the user's platform.
```

### Typical user flow

```bash
1. crabscheme aot-doctor                          # verify install
2. crabscheme aot prog.scm --explain              # survey what compiles
3. crabscheme aot prog.scm --entry <name> --build # ship it
4. ./prog-aot/target/release/<name> <args>        # run
5. ./prog-aot/target/release/<name> --version     # check provenance
```

### AOT'd binary flags

Every AOT-compiled binary (the output of `crabscheme aot ... --build`)
intercepts `--version` / `-V` before the entry call:

```
$ ./fact-aot/target/release/fact --version
compiled by crabscheme (cs-aot 0.0.1) from entry `fact` (NB ABI)
```

Bug reports should include this line so the maintainers know which
crabscheme produced the binary.

### Diagnostics

When the AOT pipeline rejects a program, the error includes a
user-meaningful description + suggested workaround:

```
$ crabscheme aot multi-fn.scm --entry main
crabscheme aot: project emit error: cs-aot project: emit error in function `main`:
  cs-aot: Inst::EnvLookupAny not yet supported — your program references a
  variable that isn't an argument or a let-binding within the AOT'd function
  — typically a free variable captured from an enclosing scope, or a global
  that AOT can't yet reach without runtime env support
    suggestion: if the variable is a top-level define, inline the value or
    pass it as an argument. For deeper fixes, this needs Phase 2.4 (env
    install API) so AOT'd code can read from the runtime env.
    reference: docs/user/aot.md (Supported/Unsupported tables)
```

The Inst names a program is most likely to hit today are `MakeClosure`
(closure values) and `EnvSet` (`set!` on globals); each `UnsupportedInst`
carries its own description + actionable workaround. See
`crates/cs-aot/src/lib.rs` `inst_user_hint(...)`.

## What works

Verified against the built binary on 2026-06-02. The AOT path compiles
a broad slice of the R6RS foundation:

**Program shape**
- Multiple top-level `(define (name args) body)` forms in one file
- Self-recursion **and** tail recursion (looping kernels)
- Cross-procedure calls — `f` calling `g` (`Inst::Call` / `CallGeneral`)
- **Mutual recursion** — `even?`/`odd?`-style co-recursive defines
- Global free-variable *reads* (`(define c 10) (define (f) (+ c 1))`)
- `--multi`: emit one binary exposing every compatible define via
  `<binary> <fn> <args…>`

**Operations** (the supported `cs_rir::Inst` set):

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
- `CallSelf` (self-recursion) + `Call` / `CallGeneral` (calls to other
  top-level defines, including mutual recursion)
- Terminators: `Return`, `Jump` (with block-param args), `Branch`
- `let` / `if` / `cond` across **multiple basic blocks** (the
  demote-env-to-SSA pass handles multi-block, not just single-block)
- Global free-variable reads (`EnvLookup` / `EnvLookupAny` resolving to
  a top-level define)

### Strings & general builtins
Arbitrary stdlib builtins (`string-append`, `string-length`,
`number->string`, `reverse`, `assoc`, `display`, …) lower via a generic
by-name dispatch into the bundled runtime. These run at **walker speed**
(not the inline-NB fast path the numeric kernels get), but they *work* —
a program mixing tight numeric loops with occasional string / list / I-O
builtins AOTs and runs correctly.

## What doesn't work yet

| Construct | Blocker | Tracking |
|-----------|---------|----------|
| Capturing closures / closure *values* | `Inst::MakeClosure` — any expression that **creates** a closure: a `lambda` passed as an argument (incl. `(map (lambda …) …)`), a `let`-bound lambda, or a returned lambda. The cs-vm capture ABI (`vm_alloc_aot_procedure_with_captures`) is in place; the cs-aot lowering is the remaining coverage work. | #280 |
| `set!` on free / global variables | `Inst::EnvSet` needs runtime env write-back | post-1.0 |
| Bare top-level side effects | AOT needs ≥1 `(define (name …) …)`; a bare `(display …)` at top level isn't an entry — wrap it in `(define (main) …)` and use `--entry main` | by design |
| FFI in AOT'd binaries | AOT'd binaries are short-lived; FFI assumes a long-running runtime | future (separate plan) |
| Multi-shot `call/cc` | inherits the M8 walker-tier deferral | future |
| Browser WASM | `wasm32-unknown-unknown` (no WASI; needs JS-bound stdio) | future |

> **Previously listed here, now working:** non-self procedure calls,
> mutual recursion, free-variable reads, multi-block `let`, and strings /
> general builtins. An older doc or error message implying these don't
> compile predates the RC3 coverage iters.

When you hit one of these, the CLI prints the Inst plus a
user-meaningful description, e.g.:

```
cs-aot: Inst::MakeClosure not yet supported — your program uses a
nested lambda or closure that captures variables from an enclosing
scope (e.g., `(lambda (x) ...)` inside another function, or
`(let* ((f (lambda ...))) ...)`)
```

`--explain` surveys every top-level define and reports which compile;
`--emit-rir` shows the full RIR. Sometimes a slightly different Scheme
phrasing avoids the unsupported Inst (e.g., hoist a `let`-bound lambda
to a top-level `define` so it's a named procedure rather than a
`MakeClosure` closure value).

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
