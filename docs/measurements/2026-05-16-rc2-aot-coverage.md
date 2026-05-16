# RC2 AOT coverage scorecard on the microbench corpus (2026-05-16)

> Captured against commit landing `bench/aot-comparison.sh`.
> Hardware: Apple M-series, devenv shell.

## Headline

`crabscheme aot bench/microbench/scheme/<bench>.scm --entry <fn> --build`
on the 8 microbenches shipped in `bench/microbench/scheme/`:

| Bench           | AOT? | Time at canonical N | Blocker if not                             |
|-----------------|------|---------------------|--------------------------------------------|
| fib             | ✅   | 0.03 s @ fib(35)    | —                                          |
| ack             | ✅   | 0.00 s @ ack(3,6)   | —                                          |
| tak             | ❌   | —                   | `Inst::EnvLookupAny` (deep nested self-call) |
| nqueens         | ❌   | —                   | `Inst::EnvDefineLocal` (internal lets)     |
| mandelbrot      | ❌   | —                   | `Inst::EnvDefineLocal`                     |
| spectral-norm   | ❌   | —                   | `Inst::MakeClosure` (nested lambdas)       |
| binary-trees    | ❌   | —                   | `Inst::EnvDefineLocal`                     |
| alloc-stress    | ❌   | —                   | `Inst::EnvDefineLocal`                     |

2 / 8 AOT cleanly today. The 6 that don't surface the exact RIR
`Inst` variant cs-aot doesn't yet handle — each one is the iter
that adds it.

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
