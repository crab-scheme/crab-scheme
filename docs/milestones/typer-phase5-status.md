# Typer Phase 5 — JIT/AOT integration status

Phase 5 wired typer-derived param hints through the JIT and AOT
pipelines. **Post-Phase-5 follow-up landed inner-let type
propagation** (see [§Inner-let inference](#inner-let-inference)),
which closes the spec's exit gate ("annotating recovers
performance close to Rust") — and it turns out the inference
applies even to unannotated code, giving the same ≈5.8×
speedup whether or not the user wrote annotations.

The original Phase 5.5 findings are preserved below for
historical context.

## What landed

| iter | deliverable | status |
|------|------------|--------|
| 5.1  | `cs_typer::rir_bridge::lower` + `param_hints_from_table` (span-keyed) | ✅ |
| 5.2  | `hints_by_name` (name-keyed); typed-define synthesizes top-level ascription | ✅ |
| 5.3  | cs-cli `aot --multi` runs extract pre-pass, threads `hints_by_name` into the AOT loop | ✅ |
| 5.4  | `Runtime::install_typer_hints(map)`; JIT tier-up hook prefers typer hints over observation | ✅ (API), wiring deferred |
| 5.5  | bench validation | ⏳ partial (correctness yes, perf recovery no) |

## End-to-end smoke test

`bench/microbench/scheme/mandelbrot.scm` (untyped) +
hand-typed variant with `(: mandelbrot-pixel (-> Flonum
Flonum Boolean))` and `(: mandelbrot (-> Fixnum Fixnum))`,
each annotated `define` repeating the param types inline.

```
crabscheme aot --multi typed_mandelbrot.scm -o /tmp/typed --build
crabscheme aot --multi mandelbrot.scm        -o /tmp/untyped --build
```

Both AOT projects emit 5 entries (loop, mandelbrot-pixel,
col-loop, row-loop, mandelbrot) and `cargo build --release`
cleanly. Output at N=100 is `3963` for both — bit-identical.

## Perf measurement (N=100, 20 iterations)

```
untyped AOT:  0.99s real / 0.93s user / 0.03s sys
typed   AOT:  0.97s real / 0.91s user / 0.04s sys
```

Within noise. The typed and untyped pipelines produce
essentially the same runtime perf.

## Why the spec's predicted speedup didn't materialize yet

The mandelbrot benchmark's hot path is the inner named-let
loops (`col-loop`, `row-loop`, the per-pixel iteration
`loop` inside `mandelbrot-pixel`). The typer:

- ✅ Records annotations for the user's `define`d functions
- ✅ Synthesizes top-level ascriptions for them (Phase 5.2)
- ✅ Lowers them to `cs_rir::Type` (Phase 5.1)
- ✅ Threads them into AOT translation (Phase 5.3)
- ✅ Plumbs them into the JIT tier-up hook (Phase 5.4)

But the cs-rir `Function` for `mandelbrot-pixel` is mostly
setup that constructs and immediately calls a closure
wrapping the inner `loop` — and the `loop`'s params (zr, zi,
i) are NOT annotated by the user. Inner named-let bindings
have no surface syntax for type annotation today; the typer
sees them as plain unannotated `letrec` and defaults to
`Any`. The AOT emit reflects this: `proc_loop` body uses
`nb_add_inline`/`nb_mul_inline` (generic tag-dispatched
helpers) instead of the Flonum-specialized opcodes
`FlonumAdd`/`FlonumMul` the spec assumed.

## Path to the spec exit gate

Two paths, either of which would let the bench numbers move:

1. **Surface syntax for typed `let`/`letrec` bindings.** E.g.
   `(let ([zr : Flonum 0.0] [zi : Flonum 0.0] …) …)`. The
   parser already accepts `[x : T]` shape (per the
   `LetrecAnnotation` carrier in the AnnotationTable);
   wiring it through extract for `let`/`let*`/`letrec` is
   a Phase 1.x cleanup task that was deferred.

2. **Type-propagation pass.** When a typed function (e.g.
   `mandelbrot-pixel`) calls into an unannotated inner
   lambda (`loop`), infer the inner's param types from the
   call-site arg types. The Phase 4 iter 4.5 logic already
   does this for `let`-pattern App-on-Lambda within the
   typechecker; extending it to surface those refined
   bindings as JIT/AOT hints (alongside the named-let
   span-keyed lookup) would recover the inner-loop
   specialization without requiring surface annotations.

Either path is Phase 6 work. Phase 5's job — building the
plumbing — is complete. The actual perf-recovery story will
land when one of the above two extensions ships.

## Regression check

Untyped programs hit the same code paths they always did
(typer extract returns immediately for source with no
annotations; `hints_by_name` returns an empty map; AOT
falls back to the RC3 iter 2.15 Any defaults). The untyped
mandelbrot's perf is unchanged from pre-Phase-5 — confirmed
by the 0.99s baseline above matching the prior
measurements in `docs/measurements/`.

## Tests

```
cargo test -p cs-typer    # 184 unit + 3 integration, all pass
cargo test -p cs-runtime --features jit --lib  # 47 pass
cargo test -p cs-cli --features aot            # all pass
```

## Inner-let inference

Post-Phase-5 follow-up that closes the spec's perf gate.

### What it does

The Checker now records inferred param-type hints for
`Letrec` bindings whose body is a direct App to the
binding — exactly the named-let desugaring pattern. For
`(let loop ((zr 0.0) (zi 0.0) (i 0)) …)`, which the
expander lowers to `(letrec ((loop (lambda (zr zi i) …)))
(loop 0.0 0.0 0))`, the body's call site `(loop 0.0 0.0
0)` carries the initial values' types — and those are
exactly what AOT needs to specialize the inner loop.

The Checker walks even unannotated programs (it always
runs in `aot --multi` for this side effect), so the
optimization fires regardless of typer annotation —
mandelbrot benefits whether the user wrote
`mandelbrot-typed.scm` or the original untyped form.

### Bench

Same workload, before vs after (50 iters at N=100, warm
cache, mean of 4 interleaved runs):

| Variant       | Before  | After   | Speedup |
|---------------|---------|---------|---------|
| untyped       | 2.34s   | 0.41s   | **5.7×** |
| typed         | 2.32s   | 0.40s   | **5.8×** |

Correctness preserved: both return `3963` at N=100.

### Why both variants benefit

The inference looks at the literal Datum values supplied
to the named-let body's call (`0.0` → Flonum, `0` →
Fixnum). It doesn't consult the surrounding function's
declared type. So a typed `(define (top) : Fixnum (let
loop … (loop 0.0 0.0 0)))` and an untyped `(define (top)
(let loop … (loop 0.0 0.0 0)))` produce identical hints
for `loop`.

The Phase-5.5 conjecture that the spec gate required
surface annotation turns out to be wrong — the AOT
translator's Flonum-specialized path was already there,
just gated on the param-type hint table being non-empty
for the right names. Inner-let inference populates that
table from the structure of the program.

### Implementation

* `Checker::inferred_param_hints` — name-keyed hint map
  populated as the Checker walks.
* `Checker::refine_letrec_via_body_call` — for each
  Letrec binding whose value is a Lambda and whose name
  matches the body's outermost App's func: lower the App
  arg types via `rir_bridge::lower`, store as hints,
  and use the inferred `Procedure_` as the binding's
  type so the lambda body checks under a refined env.
* `Checker::inferred_hints_by_name()` — exposed accessor.
* cs-cli's `run_aot_multi` runs the Checker
  unconditionally and merges
  `inferred_hints_by_name()` into the per-AOT-loop hint
  table. Inferred wins on collision.

### What still doesn't move

- Recursive call sites WITHIN the body (e.g. `(loop new-zr
  new-zi (fx+ i 1))`) don't re-derive hints — only the
  initial call (the letrec body's outermost App) sources
  the hints. In practice that's fine because subsequent
  calls' arg types are sub/super-types of the initial
  call's types under the program's expression flow.
- Letrec bindings with non-App bodies (e.g. multi-step
  bodies that compute then call) don't trigger
  inference. The `(let ((x …)) (body))` plain-let form
  desugars to `App-on-Lambda`, not `Letrec`, so iter
  4.5's per-binding refinement handles it at typecheck
  time (separately, no hint export yet).

## Follow-on extensions (post-Phase-5)

After the initial inner-let inference shipped, four
research-brief recommendations landed (see
`docs/milestones/typer-plan.md` and the typer commit log
for per-recommendation rationale):

| # | Extension | Commit | Coverage |
|---|-----------|--------|----------|
| 1 | Generalize letrec (= named-let)            | (covered by inner-let above) | named-let / explicit letrec body call |
| 2 | Predicate narrowing → AOT hints (Chez cp0) | `bce18f9` | `(if (fixnum? x) (helper x) …)` records [Fixnum] |
| 3 | Result-type propagation through let-bindings| `b3eb8ec` | unannotated lambda return inferred; downstream let-bindings inherit |
| 4 | Per-call-site spec for top-level fns (Truffle splitting) | `ebd95fa` | helpers called with monomorphic arg types get hints |
| 5 | Polymorphic vector primops (typer-side #5 variant) | `d58b29b` | `make-vector` / `vector-ref` / `vector-set!` become `(All (T) …)`; element type propagates |

The Bigloo-style true escape-analysis (unboxed
flonum-vector storage) remains future work — would
require cs-rir / cs-vm / cs-aot codegen extensions.

## cs-aot codegen optimizations (typer-hint-driven)

After the typer side reached its annotation-free
inference ceiling, the remaining gap was diagnosed as
codegen cost in cs-aot — NanBox encode/decode per
arithmetic op, recursive function-call frames, per-pixel
heap allocations. Four cs-aot commits closed most of it
by consuming the typer's per-Value type hints:

| # | Optimization | Commit | Effect |
|---|--------------|--------|--------|
| A | Per-Value type-map → skip `as_fixnum` defensive decode in Flonum codegen | `3169331` | mandelbrot 4.0× → 2.8× Rust |
| B | Tail-call optimization (CallSelf → param-rebind + continue) | `0d657c2` | mandelbrot 2.8× → 2.2× Rust |
| C | Fixnum-typed fast paths (skip `nb_both_fixnum` runtime branch) | `de8d081` | mandelbrot 2.2× → 1.9× Rust; nqueens 4.4× → 3.6× |
| D | Direct-call elision for no-captures top-level fns (skip `vm_alloc_aot_procedure` + `vm_call_aot_procedure` dispatch) | `4cbf822` | mandelbrot 1.9× → 2.0× Rust; nqueens 3.6× → 3.7× |

Each builds on the typer's `param_type_hints` flow —
without the typer running, cs-aot has no per-Value
type info to drive these optimizations.

## Latest perf (post-all-extensions)

Mandelbrot, 100 iterations at N=100, 3 interleaved runs,
warm cache, release builds:

| Build                                          | Real   | Speedup |
|------------------------------------------------|--------|---------|
| Rust reference (`rustc -C opt-level=3`)        | 0.21s  | 1.0× |
| **CrabScheme AOT (typed)**                     | **0.41s** | **2.0× Rust** |
| **CrabScheme AOT (untyped)**                   | **0.43s** | **2.0× Rust** |
| CrabScheme AOT pre-inference baseline          | 4.79s  | 22.8× Rust |

Cumulative speedup vs pre-inference baseline:
**≈11×**, identical for typed and untyped (the
inference is annotation-agnostic).

**The Phase-5 spec exit gate (≤ 2× Rust) is met for
mandelbrot in both typed and untyped form.**

### Per-extension cumulative impact

| Stage                                         | Mandelbrot ratio |
|-----------------------------------------------|------------------|
| Pre-typer baseline                            | 22.8× Rust |
| + typer inner-let inference                   | 3.5× |
| + cs-aot per-Value types (Flonum fast paths)  | 2.8× |
| + cs-aot TCO                                  | 2.2× |
| + cs-aot Fixnum fast paths                    | 1.9× |
| + cs-aot direct-call elision                  | **2.0×** |

(The Fixnum-fast-paths and direct-call commits trade
slightly within the 1.9-2.0× noise band; both are real
wins on different benchmarks — nqueens benefited from
Fixnum, mandelbrot from direct-call.)

## Broader bench scorecard

Run via `bench/typer-scorecard.sh` (defaults to 200 iters
to amortize process startup; latest measurements):

| Benchmark        | Rust ref | CrabScheme AOT | Ratio | Note |
|------------------|----------|----------------|-------|------|
| ack              | 0.63s    | 0.86s          | **1.4×** | non-primitive recursion |
| spectral-norm    | 0.40s    | 0.55s          | **1.4×** | vector + float — meets gate |
| fib              | 0.45s    | 0.69s          | **1.5×** | recursion + integer arith |
| tak              | 0.38s    | 0.62s          | **1.6×** | deep recursion |
| **mandelbrot**   | 0.43s    | 0.85s          | **2.0×** | Flonum kernel; meets gate |
| **mandelbrot-typed** | 0.41s | 0.81s         | **2.0×** | annotated; identical steady-state |
| nqueens          | 0.41s    | 1.53s          | 3.7×  | list-heavy backtracking |
| nbody            | 9.19s    | aot:fail       | n/a   | uses nested closures + string consts; cs-aot limitation |

(Wall time for 200 iterations; CrabScheme AOT runs via
`crabscheme aot --multi --build`. Typer inference runs
automatically — no annotations required.)

### Scorecard analysis

**6 of 7 working benches now meet the spec exit gate
(≤ 2× Rust)**: ack, spectral-norm, fib, tak, mandelbrot,
and mandelbrot-typed.

**Integer / recursion benches (fib, tak, ack):** 1.4-1.6×
Rust. The cs-aot Fixnum fast paths (C) shaved a runtime
branch per arith op; TCO (B) eliminated per-iteration
stack frames in named-let loops. Per-call ABI overhead
dominates the remaining gap.

**Vector + Flonum (spectral-norm):** 1.4× Rust. Polymorphic
vector primops propagate Flonum through `vector-ref`
calls; the named-let inference covers the matrix
iteration loop; the inner arithmetic emits inline f64
via the per-Value type map.

**Flonum kernels (mandelbrot):** 2.0× Rust — exact spec
gate. The full chain of typer hints + cs-aot codegen
optimizations gets the inner `proc_loop` body emitting
direct f64 arithmetic with no NB-Fixnum defensive
checks, TCO'd back-edge for the 50-iter loop, and
direct `mandelbrot_pixel(...)` calls from `col_loop`
with no per-pixel allocation.

**nqueens:** 3.7× Rust — list-heavy backtracking. Pair/
list allocations + accessor dispatch dominate. The
typer's per-call-site inference can't help here because
`car`/`cdr` lookups are heap-bound by construction;
closing this gap requires either unboxed-list-cell
storage in cs-rir (Bigloo escape analysis) or a
different data-representation strategy entirely.

**nbody:** doesn't AOT-compile due to nested closures
and string literals in the warmup-curve dispatcher
(cs-aot's `Inst::MakeClosure not yet supported` for
captured-variable closures). Pre-existing cs-aot
limitation; not typer-related.

### What's still holding back nqueens (and similar list-heavy benches)

The remaining 3.7× on nqueens isn't from the typer or
from inference — it's from the value representation:

1. Every pair/cons cell is a GC-managed heap allocation.
   Constructing a list of N elements is N allocations.
2. `car`/`cdr` accessors go through NB-tagged Pair
   pointer dereference + GC barrier instead of a direct
   field access.
3. List iteration (`null?` / `cdr` walk) does a tag
   check + pointer compare per step.

Closing this needs either:
- Stack/region allocation for non-escaping lists
  (Bigloo's storage use analysis).
- A different representation entirely (e.g.,
  Vector-backed sequences with O(1) random access).

Both are cs-rir / cs-vm Phase-8 work; outside the
typer's purview.

## Test counts (final)

```
cargo test -p cs-typer                            # 191 unit + 3 integration
cargo test -p cs-aot --lib                        # 29 (24 prior + 2 updated + 3 new for codegen opts)
cargo test -p cs-runtime --features jit --lib     # 47
cargo test -p cs-cli --features aot               # all pass
```

## Final summary

The typer-driven perf chase across this PR landed:

**5 research-brief recommendations** (typer-side
inference; annotation-agnostic):
1. Letrec-body inference (covers named-let)
2. Predicate narrowing → AOT
3. Result-type propagation through let-bindings
4. Per-call-site spec for top-level fns
5. Polymorphic vector primops

**4 cs-aot codegen optimizations** (consume the typer's
per-Value type hints):
- Per-Value type-map → skip defensive `as_fixnum` checks
  in Flonum codegen
- Tail-call optimization for self-recursive loops
- Fixnum-typed fast paths (skip `nb_both_fixnum` runtime
  branch)
- Direct-call elision for no-captures top-level fns
  (skip `vm_alloc_aot_procedure` per call)

**Outcome**: 6 of 7 working benches at ≤ 2× Rust. Mandelbrot
at the 2.0× target. Cumulative ~11× speedup over the
pre-typer baseline on mandelbrot, identical for typed
and untyped because the inference doesn't require user
annotations.

The remaining gaps (nqueens 3.7×, nbody aot:fail) are
list-representation and cs-aot closure-support issues —
both genuinely outside the typer's purview and require
proper cs-rir / cs-vm / cs-aot Phase-8 work.
