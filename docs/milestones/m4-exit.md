# M4 Exit Report — Bytecode VM (Warm Tier)

> Tagged: `m4-complete` at commit (see git log).
> Conformance baseline: **1460 individual Scheme tests passing** across
> 68 test files (cli) and 70 (vm). Walker↔VM differential parity holds.

This report retroactively closes M0–M4 of the [ROADMAP](../../ROADMAP.md).
Foundation, macro, and bytecode-VM work all happened on `main` without
intermediate milestone tags; this is the first formal exit gate.

---

## What shipped

### M0–M2 (Foundation)
- `cs-diag` / `cs-core` / `cs-lex` / `cs-parse` / `cs-ir` / `cs-expand` /
  `cs-runtime` / `cs-cli` / `cs-repl` workspace crates.
- Tree-walker interpreter with trampolining tail calls.
- R6RS reader (datum syntax, reader macros, `#;` datum comments,
  `#| ... |#` nested block comments, `#x`/`#b`/`#o`/`#d` radix prefixes,
  `#e`/`#i` exactness prefixes).
- IEEE-754 literals (`+inf.0`, `-inf.0`, `+nan.0`).
- Bignum integer literals via `Number::parse_decimal_integer`.
- Numeric tower: `Fixnum (i64)` / `Big (Rc<BigInt>)` /
  `Rat (Rc<BigRational>)` / `Flonum (f64)` with R6RS contagion,
  fixnum-overflow → BigInt promotion, exact ↔ inexact coercion via
  `BigRational::from_float` for non-integral flonums.
- Conditions hierarchy: standard types, `define-condition-type`,
  `parent`, `display-condition`, `error-object-*`.
- Records with `parent` chaining (`__record-parents__` registry).
- Hashtables (`eq` / `eqv` / `equal` / `Custom` user-supplied
  hash+equiv); `hashtable-ref/-set!/-delete!/-contains?/-copy/
  -mutable?/-equivalence-function/-hash-function/-entries`.
- Bytevectors with R6RS typed accessors at every width
  (`u/s {8,16,32,64}` ref/set! plus IEEE single/double, both
  endianness-explicit and native-shortcut variants).
- String/char/vector ops: full SRFI-1 + SRFI-13 surface area,
  cXXr accessors depths 2–4, vector-append/subvector,
  list-copy/make-list, etc.
- Ports: `StringInput`/`StringOutput` (text), `ByteVectorInput`/
  `ByteVectorOutput` (binary), `FileOutput`, `with-*-string` and
  `with-*-file` HO wrappers.
- Multi-value returns via `pending_values` (walker) and
  `vm_set_pending_values` (VM).
- R7RS time + env API: `current-second`, `current-jiffy`,
  `jiffies-per-second`, `get-environment-variable(s)`, `command-line`.

### M3 (Macros)
- Hygienic `syntax-rules` with mark/rename hygiene.
- `let-syntax`, `letrec-syntax`, `define-syntax`.
- `guard`, `case`, `case-lambda`, `cond-expand`, `assert`,
  `parameterize`, `let-values`, `let*-values`, `delay`, `do`,
  `quasiquote`/`unquote`/`unquote-splicing`.
- `library` / `import` / `export` shape recognition with
  `only`/`except`/`prefix`/`rename` modifier parsing; `rename`
  effective via synthesized define forms.
- `(endianness big|little)` macro.

### M4 (Bytecode VM)
- `cs-vm` crate: stack-machine bytecode VM with frame stack,
  fast-primop closure bodies, fused compare-branch opcodes.
- Lowering pass: `CoreExpr → cs-vm bytecode` in cs-runtime.
- Hybrid Vec/HashMap env bindings (threshold 12).
- Differential walker↔VM parity verified per file in
  `crates/cs-runtime/tests/vm_conformance.rs` (every VM-tier file
  asserts pass count equals walker-tier).
- Per-call dispatch hoist on every standard SRFI-1-shape HO list
  op (fold-left/right, filter, find, any, every, count, partition)
  + vector-fold/map/for-each: when the proc is a known
  `VmBuiltin`, skip the per-iteration `vm_call_sync`
  match/downcast.
- Three builtin tiers: `Pure(&[Value])`,
  `Higher(&[Value], &mut EvalCtx)`, `Syms(&[Value],
  &mut SymbolTable)` — each registers identically on both walker
  and VM via shared tables in `cs-runtime/builtins/mod.rs`.

---

## What was deferred / not in scope at M4

Marked as future-milestone work, not regressions:

| Feature | Target | Note |
|---|---|---|
| Multi-shot continuations | M8 | Today's `call/cc` is escape-only |
| Per-library scope frames | M9 | `import`/`export` parse but don't isolate namespaces |
| Custom port types | M9 | Foundation has the data variants we need |
| HolyJIT integration | M7 | Not started |
| Cranelift JIT | M6 | Not started |
| Precise tracing GC | **M5 (next)** | Currently `Rc<RefCell<...>>` everywhere |
| Generational GC | M5 follow-up | Stop-the-world mark-sweep first |
| AOT compiler | M10 | Not started |
| WASM target | M10 | Not started |
| Verified core | M11 | Not started |
| `string-foldcase` Unicode tables | future | Currently ASCII-folding |

---

## Baselines

### Conformance
- **1460 individual test cases passing** in
  `tests/conformance/foundation/*.scm`.
- **68 cli conformance test files** (one per `.scm`).
- **70 vm conformance test files** (cli + extra walker-vs-VM pairs).
- Aggregate-count regression check fails CI if total drops below
  prior trunk baseline (see `crates/cs-cli/tests/conformance.rs`
  `conformance_aggregate_count` test).

### Differential walker↔VM parity
- Every `tests/conformance/foundation/*.scm` runnable on the VM
  asserts walker pass count == VM pass count.
- A small number of files (e.g. `bytevectors_misc.scm` with
  gensym + with-output-to-string) are walker-only at this milestone
  pending cross-tier bridge work.

### Performance
No criterion benchmarks committed at M4 exit. Performance work has
focused on iterative dispatch-hoist wins (iter 81 fold-left ~9%, iter
91 fold-right/filter/find/any/every/count/partition). Benchmark
baseline capture is a M5-or-later deliverable.

---

## Pre-M5 work captured here

The pre-M5 plan in `.claude/pre-m5-plan.md` finished:
- **Item 1**: custom hash + equiv in `make-hashtable` — locks down
  the last `Hashtable` shape change before GC tracing.
- **Item 2a**: R6RS import-spec modifier parsing (`only`/`except`/
  `prefix`/`rename`) at the expander.
- **Item 2b**: library declaration validation + per-Expander
  registry tracking name/exports.
- **Item 2c**: per-library scope frames — deferred to M9 since it
  doesn't affect Value heap layout.

Net effect: the Value enum's heap-pointer surface is stable. M5
GC tracing code can be written once.

---

## Known limitations going into M5

1. **`Rc<T>` everywhere.** No cycle collection — `equal?` cycle-safety
   is the current line of defense. M5 swaps this out for tracing GC.
2. **No JIT** — only the bytecode VM is the warm tier.
3. **Library namespaces** — bindings are global; `(only ...)` etc.
   parse but don't restrict.
4. **`call/cc`** — one-shot escape only; no multi-shot.
5. **No criterion benchmarks** — perf work has been correctness-first.

---

## Pointers

- ROADMAP: `ROADMAP.md`
- Foundation spec: `.spec-workflow/specs/foundation/`
- M5 spec: `.spec-workflow/specs/gc/` (created at this milestone)
- Test corpus: `tests/conformance/foundation/`
- Pre-M5 plan trace: `.claude/pre-m5-plan.md`
