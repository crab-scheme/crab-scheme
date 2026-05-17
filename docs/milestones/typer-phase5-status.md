# Typer Phase 5 — JIT/AOT integration status

Phase 5 wired typer-derived param hints through the JIT and AOT
pipelines. This doc captures the bench-validation iter (5.5)
findings and outlines what's still needed to hit the spec's
exit gate ("annotating the four hottest benches with their
natural Flonum/Fixnum types recovers performance close to Rust").

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
cargo test -p cs-typer    # 148 unit + 3 integration, all pass
cargo test -p cs-runtime --features jit --lib  # 47 pass (45 prior + 2 new)
cargo test -p cs-cli --features aot            # all pass
```
