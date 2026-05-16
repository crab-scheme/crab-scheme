# RC2 AOT coverage scorecard on the microbench corpus (2026-05-16)

> Captured against commit landing `bench/aot-comparison.sh`.
> Hardware: Apple M-series, devenv shell.

## Headline

`crabscheme aot bench/microbench/scheme/<bench>.scm --entry <fn> --build`
on the 8 microbenches shipped in `bench/microbench/scheme/`:

| Bench           | AOT? | Time at canonical N | Blocker if not (post-iter-J) |
|-----------------|------|---------------------|------------------------------|
| fib             | ✅   | 0.03 s @ fib(35)    | —                            |
| ack             | ✅   | 0.00 s @ ack(3,6)   | —                            |
| tak             | ❌   | —                   | `EnvLookupAny` (multi-block — demote skipped) |
| nqueens         | ❌   | —                   | `MakeClosure` (nested lambdas) |
| mandelbrot      | ❌   | —                   | `MakeClosure`                |
| spectral-norm   | ❌   | —                   | demote edge case (chained aliases — single-block + use-before-def) |
| binary-trees    | ❌   | —                   | `MakeClosure`                |
| alloc-stress    | ❌   | —                   | `MakeClosure`                |

2 / 8 AOT cleanly today. The 6 that don't surface the exact RIR
`Inst` variant cs-aot doesn't yet handle — each one is the iter
that adds it.

### Iter-J update (commit `c1c8222`)

RC2 iter J landed `bytecode_to_rir_aot` + identity-in-NB Inst
lowering (BoxTyped/AnyToFix/AnyToBool/AnyToFlo/AnyTruthy/FixToFlo/
IntCharBitcast), which AOT-enables `let`-binding programs. The
shifted blocker map:

|                              | Pre-iter-J | Post-iter-J |
|------------------------------|------------|-------------|
| `EnvDefineLocal` blockers    | 4 benches  | 0           |
| `EnvLookupAny` blockers      | 1 bench    | 1 (tak — multi-block, demote skipped) |
| `MakeClosure` blockers       | 1 bench    | 4 (nqueens, mandelbrot, binary-trees, alloc-stress — newly visible after EnvDefineLocal fix) |
| Demote-pass edge case        | 0          | 1 (spectral-norm) |

External OK count unchanged (still 2/8) because the four
EnvDefineLocal-blocked benches also have nested lambdas — the
iter-J fix exposed the OTHER reason they don't AOT. But pure
`let`-using programs without nested lambdas now AOT (proven by
`source_to_aot_function_with_let_binding` +
`source_to_aot_function_with_nested_lets` tests).

`nbody.scm` not in the table — it uses top-level vector globals,
which AOT can't drive from CLI args (extracting one lambda by
name leaves the globals undefined). That's a deeper "top-level
program" gap covered separately by post-RC2's entry-point
synthesis work.

## What the blockers mean (post-RC2 iter map)

The bytecode → RIR translator (`cs_vm::jit_translate::bytecode_to_rir`)
emits these Insts when the source uses constructs cs-aot doesn't
yet lower. Each is a tractable addition — same shape as the
existing arith/Flonum helpers:

- **`EnvDefineLocal`**: emitted for internal `(let ((x ...)) ...)`
  and `(do ((i ...) ...) ...)` forms. The expander desugars these
  into a frame-local binding, and the bytecode → RIR pass currently
  represents that as an EnvDefineLocal Inst even when the binding
  could live in a fresh SSA `Value` directly. Two ways to fix:
  (a) extend the translator to recognize "let bindings the
  lifetime of which doesn't escape this lambda" and use SSA Values
  directly; (b) add EnvDefineLocal lowering to cs-aot, with a per-
  function `HashMap<Symbol, i64>` for the env frame. (a) is the
  cleaner long-term fix but bigger.

- **`MakeClosure`**: a defined function references a procedure
  value at runtime — spectral-norm's matrix-elt is bound in a
  let, then called inside another function. Lowering needs the
  cs-vm `Procedure` heap allocator + a global Procedure table.

- **`EnvLookupAny`**: cross-lambda reference. tak's nested
  `(tak (tak ...) (tak ...) (tak ...))` triple call may have a
  pattern the translator's CallSelf detection misses. Worth a
  closer look at whether the translator can be taught to always
  use CallSelf for the lambda-being-translated's name, regardless
  of nesting depth.

## How to reproduce

```bash
devenv shell -- cargo build -p cs-cli --release
devenv shell -- bash bench/aot-comparison.sh
```

The script reuses the workspace's `target/aot-comparison/<name>/`
for each bench's AOT'd cargo project. Cached cargo builds; second
runs are fast.

## What this is NOT

This isn't a head-to-head AOT-vs-JIT perf table because the AOT
runs use a different N (driven from CLI args at any value the
user picks). The microbench harness in `bench/microbench/run.sh`
times each impl at the bench file's own canonical N (e.g. fib(25)
in the .scm). When future iters wire AOT into the main harness
at matching N values, the timing comparison goes there.

What this IS: a scorecard for cs-aot's RIR-Inst coverage against a
real workload corpus, with each gap named for the iter that
closes it.
