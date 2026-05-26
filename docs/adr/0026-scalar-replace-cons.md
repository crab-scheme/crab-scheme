# ADR 0026: Scalar replacement of non-escaping cons cells

> Status: Accepted
> Date: 2026-05-25
> Authors: crab-scheme contributors

## Context

Issue #28: every `(cons a b)` heap-allocates a `Gc<Pair>` —
`Rc::new` of a ~120-byte struct (two `RefCell<Value>`, a
`Cell<Option<Span>>`, and two `RefCell<Option<WeakValue>>`
cycle-tombstone fields) plus reference-count traffic and the
eventual free. For a large class of *transient* pairs this
allocation is pure waste: the pair is built, read once or twice,
and discarded.

The issue proposes "storing a pair of two immediates inline" to
skip the allocation. Investigation (see the #28 PR description)
showed a general inline-pair *representation* is not viable:

- `Value` is a plain Rust enum, and Scheme pairs are **mutable**
  (`set-car!`/`set-cdr!` via interior `RefCell`), **identity-
  bearing** (`eq?` is `Rc::ptr_eq`), and **cycle-managed** (the
  tombstone fields require `Gc<Pair>` heap identity). A by-value
  `Value::InlinePair` breaks all three.
- The bytecode VM / JIT `NanboxValue` encoding has all 16 of its
  4-bit tags assigned, leaving no room for an inline-pair tag, and
  its 47-bit payload can't hold two arbitrary immediates.

So the allocation can't be removed by changing how a pair is
*stored*. It can be removed by not creating the pair *at all* when
the program can't observe it — a classic optimizing-compiler
transform.

## Decision

Add **`scalar-replace-cons`**, a `cs-opt` RIR pass implementing
**scalar replacement of aggregates (SRA)** for non-escaping cons
cells, and seed it **default-on** for JIT compilation.

### The transform

A cons whose result `v` never escapes and is read back only by
`car` / `cdr` / `pair?` / `null?` is provably never mutated, never
`eq?`-compared, and never stored — so the pair object is
unobservable. Rewrite each read to the cons operand and delete the
allocation:

```text
  v  = cons a b          ;; v does not escape
  d1 = car v        →    d1 = move a
  d2 = cdr v        →    d2 = move b
  d3 = pair? v      →    d3 = const #t
  d4 = null? v      →    d4 = const #f
                         (the cons is neutralized to `move v, a`;
                          v is now dead — no allocation remains)
```

The escape test is identical to the existing `escape-to-region`
pass's `cons_escapes`: `v` is eligible iff it never appears in a
terminator and every instruction mentioning it is one of the four
read-only pair ops with `v` in the operand position. Any other
mention — `SetCar`/`SetCdr` (mutation), a `Call` arg (aliasing),
an `EnvSet`/`EnvDefineLocal` (capture), a store into another
aggregate, or a terminator (return / block-arg flow) —
disqualifies the cons, which then stays a real heap allocation.

### Relationship to `escape-to-region` (#51 / ADR 0020 Strategy C)

`escape-to-region` promotes the *same* set of non-escaping conses
to bump-arena allocation (`Inst::ConsRegion`). SRA targets the
identical set but does strictly better — **no allocation at all**
vs a region bump — and, crucially, its benefit is **not
`with-region`-gated**: `ConsRegion` only avoids the heap when a
region scope is dynamically in scope, whereas SRA eliminates the
allocation unconditionally. SRA therefore runs in the `Early`
bucket, ahead of `escape-to-region`; conses it eliminates are gone
before the region pass looks, and conses it can't prove remain for
the region pass to consider. The two compose cleanly.

### Why default-on

Unlike the region/escape passes (which need a `with-region` scope
to pay off and are therefore opt-in), SRA is **unconditionally
sound and unconditionally beneficial** — it only removes pairs
proven unobservable, and removing them is always a win. So it is
seeded into a new process-global "always-on" pass list
(`cs_opt::set_default_on_passes`, called once from
`Runtime::new`) that `run_active_pipeline` runs on every JIT
compile on every thread, ahead of the per-thread user-installed
passes. The user's `active-optimizer-passes` parameter and
`install-optimizer-pass!` remain orthogonal.

## Scope and limitations

SRA fires on **directly-consumed conses on the JIT tier** — the
cons result flows straight into a read as an SSA temp. Two honest
limitations, both pre-existing architectural facts rather than SRA
shortcomings:

1. **`let`-bound transient pairs on the JIT tier are not
   eliminated.** The JIT translation keeps `let` locals in env
   slots (`EnvDefineLocal`/`EnvLookup`); only the AOT path runs
   `demote_env_to_ssa`. A let-bound pair therefore reaches SRA as
   an env store, which the escape test (correctly) treats as
   escaping. Demoting env locals to SSA on the JIT path is #51b,
   deferred — and unsound under deopt, since the deopt path
   reconstructs the interpreter frame from those env slots. SRA is
   forward-compatible: if #51b ever lands a deopt-safe JIT demote,
   let-bound transient pairs become SSA and SRA eliminates them
   with no further work here.

2. **The AOT tier is unaffected.** In AOT mode every builtin
   (including `cons`/`car`/`cdr`) lowers to generic
   `Inst::CallBuiltin` dispatch rather than the dedicated
   `Inst::Cons`/`Car`/`Cdr` the pass matches, so there is nothing
   for SRA to rewrite. AOT allocation optimization is a separate
   track.

Directly-consumed conses arise from `(car (cons …))` idioms,
macro/desugar output (multiple-value encodings, quasiquote
fragments consumed in place), and post-inlining contexts.

### Deopt soundness

SRA *removes* a value rather than (like `escape-to-region`) merely
re-tagging its allocator, so it raises a deopt-reconstruction
question the region pass did not. It is safe because eligibility
requires the cons to be proven a pair (it is the freshly-built
cons), so the rewritten reads carry no type guard and introduce no
deopt point; and at every *other* deopt point in the function the
eliminated pair is already consumed and not live, so its absence
never starves a reconstruction. This is verified empirically by a
deopt-crossing integration test plus the full JIT/differential/
conformance suite running green with the pass default-on.

## Consequences

### Positive
- Transient directly-consumed conses cost zero allocation on the
  JIT tier, automatically (default-on), with no `with-region`
  needed. The first allocation-*eliminating* optimizer pass (the
  others promote or fold).
- Composes with and strictly improves on `escape-to-region` for
  the overlap set.
- Confined to `cs-opt` + a one-line default-on seed in
  `cs-runtime`; no changes to the `Value`/`Pair` representation,
  the `NanboxValue` encoding, or pair semantics.

### Negative / risks
- Per-cons escape scan is O(conses × insts) per function (same as
  `escape-to-region`); acceptable because JIT'd functions are
  size-bounded, and the soundness gate bails in O(insts) on any
  uncovered variant. A single-scan use-map is a possible future
  optimization.
- `let`-bound JIT pairs and the AOT tier are out of reach (see
  Limitations); the headline win is narrower than the issue's
  "every two-immediate pair" framing.

### Things that don't change
- `Value`, `Pair`, `eq?`, `set-car!`/`set-cdr!`, the cycle
  collector, and the `NanboxValue` encoding are untouched.
- Opt-in passes (`escape-to-region`, etc.) keep their existing
  behavior; only the always-on list is new.

## Alternatives considered

- **Inline-pair `Value` variant / NaN-box packing.** Rejected:
  breaks mutation + identity + cycle management; no spare NB tag.
- **Run the optimizer pipeline on the AOT path after its demote.**
  Rejected: AOT lowers cons to `CallBuiltin`, so there is no
  `Inst::Cons` for SRA to match — the wiring would be a no-op.
- **Local env-slot demotion inside SRA** (to reach let-bound JIT
  pairs). Rejected: equivalent to #51b; requires capture analysis
  and is unsound under deopt.
- **Shrink the `Pair` struct** (lazify the rarely-used tombstone /
  source fields). Complementary broad win across all tiers; filed
  as a follow-up rather than mixed into this pass.

## Follow-ups

- [ ] Pair-struct shrink (lazy tombstone/source) — all-tier
      per-cons cost reduction.
- [ ] #51b deopt-safe JIT env demotion — unlocks let-bound
      transient-pair elimination through this same pass.
- [ ] Single-scan use-map to drop the O(conses × insts) factor.

## References

- Issue #28 — inline small-pair storage.
- Issue #51 / ADR 0020 — RIR optimization pipeline, escape
  analysis (the sibling `escape-to-region` pass SRA refines).
- ADR 0014 — the `cs-opt` pluggable optimizer-pass framework.
- `crates/cs-opt/src/passes/scalar_replace_cons.rs` — the pass.
- `crates/cs-opt/src/passes/escape_to_region.rs` — the shared
  escape predicate this pass mirrors.
