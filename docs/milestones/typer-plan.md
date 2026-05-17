# Typer Plan — Optional Gradual Types for CrabScheme

> Status: **Open** as of 2026-05-17. Predecessor: 1.0-rc3
> (`aot-hardening` complete; 8/8 microbenches AOT correctly).
> Estimated duration: 6-10 weeks across seven phases.
> Spec slug: `typer`.
>
> **Target outcome:** ship `cs-typer`, an optional/gradual type
> checker for CrabScheme that lets programmers add `: Type`
> annotations to `define` / `lambda` forms and gets compile-time
> errors for mismatches, while preserving full backward
> compatibility with untyped R6RS code. The typer's inferences
> also feed the JIT and AOT pipelines as `param_type_hints`,
> turning user-supplied annotations into specialized lowerings.

## Why a typer now

The 1.0-rc3 release is fully runtime-typed: every operation
dispatches on NaN-box tag bits. Three reasons to layer
compile-time types on top:

1. **Catch errors before runtime.** A typo like `(car 5)` is a
   runtime exception today; a typer catches it at compile time
   with a precise source location.

2. **Feed the JIT / AOT directly.** The translator already
   accepts `param_type_hints: Option<&[Type]>` and emits
   specialized RIR for typed operands (FlonumAdd vs generic
   Add, FixnumP const-folds, etc.). The JIT gets one-shot
   observations at tier-up; AOT defaults all params to
   `Type::Any` because there's no annotation source. A typer
   gives both pipelines high-fidelity input without runtime
   feedback collection — the iter 2.16/2.17 perf regression on
   numeric kernels (mandelbrot 16× slower, spectral-norm 25×
   slower than Rust) recovers when params are typed at the
   source.

3. **Documentation that compiles.** `(define (place [row : Fixnum]
   [placed : (Listof Pair)]) : Fixnum ...)` self-describes in a
   way comments never enforce.

The cs-rir `Type` enum has 11 variants already aligned with the
NB tag space (Fixnum, Flonum, Boolean, Character, Pair, Vector,
String, ByteVector, Procedure, Symbol, Null, plus Any as the
gradual top). The translator's `value_types: HashMap<RirValue,
Type>` map already does occurrence-style narrowing via predicate
checks. The substrate exists; we add a source-level annotation
language and a checker that flows through to it.

## Design inspirations

Researched the typed-Scheme literature; key papers:

- **Tobin-Hochstadt + Felleisen, "The Design and Implementation
  of Typed Scheme"** (POPL 2008) — the canonical design. Local
  type inference + occurrence typing + union types + module-
  level migration. https://www2.ccs.neu.edu/racket/pubs/popl08-thf.pdf
- **Greenman + Felleisen, "A Spectrum of Type Soundness and
  Performance"** (ICFP 2018) — three soundness/perf points:
  sound+slow (per-boundary higher-order monitoring), first-order
  checks at boundaries (fast, weaker), erasure (fastest,
  unsound). https://dl.acm.org/doi/10.1145/3236766
- **Pierce + Turner, "Local Type Inference"** (1998) — the
  algorithm Typed Racket uses to avoid full Hindley-Milner.
  https://www.cis.upenn.edu/~bcpierce/papers/lti.pdf
- **Bonnaire-Sergeant, "Typed Clojure in Theory and Practice"**
  (PhD) — Typed Racket-style design applied to Clojure;
  pragmatic notes on annotation syntax. https://thesis.ambrosebs.com/
- **Siek, "Gradual Typing for Functional Languages"** (Scheme
  2006) — the foundational gradual-typing definition.
  http://scheme2006.cs.uchicago.edu/13-siek.pdf

Closely related implementations to study:

- **Typed Racket** — `:` annotation syntax, `(: name Type)` ascription
  forms, `define-type` aliases, occurrence typing via predicates.
- **Coalton** (Common Lisp) — strict ML-style HM, NOT gradual.
  Useful negative example: too restrictive for R6RS Scheme's
  any-can-be-anywhere semantics.
- **Typed Clojure** (core.typed) — annotation-heavy migratory
  design that didn't see wide adoption due to performance cost
  of contract wrappers. Cautionary tale on boundary checks.

## Design picks

We pick the **Typed Racket model with explicit migratory
opt-in**, simplified by deferring polymorphism and contracts:

- **Migratory opt-in at the function level.** Annotations are
  optional on every `define` / `lambda`. Untyped code stays
  untyped; typed code only gets checked at its top-level
  function boundary. No file-level `#lang typed/scheme`
  marker — annotation presence is the marker.

- **Local Type Inference** (Pierce-Turner). Bidirectional
  checking: inferred types flow up from expressions, expected
  types flow down from annotations. No global HM unification.

- **Union types** (`(U Fixnum Flonum)`) — small finite unions
  only, no recursive unions in Phase 1. Sufficient for
  occurrence typing without full set-theoretic types.

- **Occurrence typing** via predicates. `(if (number? x) (+ x 1)
  x)` narrows x's type in the then-branch. The translator
  already does this for `value_types`; we lift it to source
  level.

- **Erasure at runtime** (Greenman's "Spectrum" middle ground).
  Annotations get checked at compile time, then erased. NO
  higher-order contracts at typed/untyped boundaries. This is
  unsound when an untyped caller passes garbage to a typed
  function — the runtime will catch it via the existing NB
  dispatch — but it has zero runtime cost.

- **No polymorphism in Phase 1**. `(define (id [x : Any]) : Any
  x)` works; `(define (id : (∀ T (-> T T)) ...)` doesn't.
  Phase 7 revisits if time permits.

- **Type annotation syntax** modeled on Typed Racket:

  ```scheme
  ; Function with typed params + return
  (define (fib [n : Fixnum]) : Fixnum
    (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))

  ; Ascription form for ambient annotations
  (: helper (-> Fixnum Fixnum Fixnum))
  (define (helper a b) (+ a b))

  ; Type aliases
  (define-type IntList (Listof Fixnum))
  ```

## Architecture

### New crate: `cs-typer`

```
crates/cs-typer/
├── Cargo.toml          — depends on cs-core, cs-diag, cs-ir,
│                         cs-rir (for Type), cs-expand (for
│                         AST shape recognition)
├── src/
│   ├── lib.rs          — pub fn check(expr: &CoreExpr) -> Result<TypedExpr, Vec<Diagnostic>>
│   ├── types.rs        — extended `Type` (unions, function types,
│   │                     type variables for Phase 7)
│   ├── env.rs          — TypeEnv (Symbol → Type) with scoping
│   ├── check.rs        — bidirectional check / infer
│   ├── narrow.rs       — occurrence typing (predicate → refined type)
│   ├── parse_ann.rs    — parse `: Type` annotation forms
│   ├── annotate.rs     — TypedExpr — CoreExpr + per-node Type slots
│   └── diagnostics.rs  — TypeError → cs_diag::Diagnostic
└── tests/
    ├── primitives.rs   — basic Fixnum/Flonum/Bool inference
    ├── occurrence.rs   — predicate-narrowing tests
    ├── functions.rs    — applied-fn arg checking
    ├── unions.rs       — (U Fixnum Flonum) handling
    └── migration.rs    — typed-calling-untyped, untyped-calling-typed
```

### Pipeline integration

Current pipeline:
```
source → cs-lex → cs-parse (Datum) → cs-expand (CoreExpr)
       → cs-vm::compile (Bytecode) → VM/JIT/AOT
```

New pipeline (typer is OPTIONAL — gated on whether the expanded
CoreExpr contains type annotations):
```
source → cs-lex → cs-parse → cs-expand (CoreExpr with ann slots)
       → cs-typer::check (TypedExpr, or untyped passthrough)
       → cs-vm::compile (Bytecode with param_type_hints derived
         from TypedExpr) → VM/JIT/AOT
```

When the expanded CoreExpr has zero type annotations, cs-typer
returns the expression unchanged (no-op). When it has at least
one annotation, the checker runs over the whole program; errors
become diagnostics; successful check produces a TypedExpr that
the compiler uses to populate `LambdaProfile.jit_param_types`
and AOT's `param_type_hints`.

### New cs-ir extensions

`CoreExpr` gains optional annotation slots:

```rust
pub enum CoreExpr {
    Const { ... },
    Ref { name, span, ann: Option<TypeAnn> },         // NEW: ann
    Set { ... },
    Lambda {
        params: Params,
        param_types: Vec<Option<TypeAnn>>,            // NEW
        return_type: Option<TypeAnn>,                 // NEW
        body, span
    },
    App { func, args, span, ann: Option<TypeAnn> },   // NEW: ann (cast)
    If { ... },
    Begin { ... },
    Letrec {
        bindings: Vec<(Symbol, Option<TypeAnn>, CoreExpr)>,  // NEW: per-binding ann
        body, span
    },
}
```

`TypeAnn` is the parsed-but-unchecked annotation surface (an AST
of types). cs-typer takes `TypeAnn`s and `CoreExpr` and produces
`TypedExpr` (a parallel structure with required type slots).

### Lexer + parser changes

cs-lex needs `:` as a punctuation token. Currently `:` is a
legal identifier character; we add a special case: standalone
`:` followed by whitespace is the annotation marker. (Identifiers
starting with `:` like `:foo` keep working — only the standalone
`:` between bindings is special.)

cs-parse passes annotation syntax through as nested Datums; the
expander interprets them.

### Expander changes

New keyword arms for:
- `(: NAME TYPE)` — ascription (binds an external annotation to
  the next top-level `define`)
- `(define-type ALIAS TYPE)` — type alias declaration
- `(define (NAME [P : T] ...) : RET BODY)` — typed define
- `(lambda ([P : T] ...) : RET BODY)` — typed lambda
- `(letrec ([(NAME : T) VAL] ...) BODY)` — typed letrec

The expander parses annotations into `TypeAnn` AST and attaches
them to the appropriate CoreExpr slots. Unannotated forms work
exactly as today (the new slots default to `None`).

## Type representation

```rust
pub enum Type {
    // Atomic — match cs_rir::Type variants
    Fixnum, Flonum, Boolean, Character, Symbol,
    Pair, Vector, String, ByteVector, Procedure, Null,

    // Top — gradual unknown. Untyped code's params/returns.
    Any,

    // Bottom — never-returning expression (error, infinite loop).
    Never,

    // Small finite unions. (U Fixnum Flonum) etc.
    Union(Vec<Type>),

    // Procedure with arity + arg + return types.
    Procedure(Box<ProcType>),

    // Parameterized container types. (Listof T), (Vectorof T).
    Listof(Box<Type>),
    Vectorof(Box<Type>),

    // Phase 7 (deferred): polymorphism
    // Forall(Vec<TyVar>, Box<Type>),
    // Var(TyVar),
}

pub struct ProcType {
    pub params: Vec<Type>,
    pub return_type: Type,
    pub rest: Option<Type>,   // (-> Fixnum ... Fixnum) for rest-args
}
```

Subtype relation (Phase 2):
- `T <: Any` always
- `Never <: T` always
- `T <: U` iff `T` is in `U`'s union members
- `(-> A1 ... An R)` <: `(-> B1 ... Bn S)` iff each `Bi <: Ai`
  (contravariant params) and `R <: S` (covariant return)
- `(Listof T) <: (Listof U)` iff `T <: U` (Scheme lists are
  immutable enough that this is sound for our purposes; revisit
  for mutable containers)

## Phasing

Seven phases. Each is independently useful; the typer
graduates from "annotations parse" through "occurrence typing"
to "polymorphism".

### Phase 1: Annotation syntax + parser (≈1 week)

Goal: programs with type annotations parse cleanly. No checking
yet — annotations are stored but ignored.

**Iters:**

- **1.1** New crate skeleton. `crates/cs-typer/` with empty
  module structure. Cargo workspace entry.
- **1.2** Lexer extension. `cs-lex` recognizes standalone `:`
  between whitespace as `Token::Colon`. Existing
  `:`-containing identifiers (`->`, `:foo`) keep working.
- **1.3** `TypeAnn` AST. Define the parsed annotation tree.
  Types include atoms (`Fixnum`, `Any`), `(U ...)`, `(-> ... R)`,
  `(Listof T)`, `(Vectorof T)`. cs-parse can produce these via
  the existing Datum mechanism.
- **1.4** Expander integration. New arms in cs-expand for:
  `(: name type)` ascription, `(define-type alias type)` alias,
  typed `define` / `lambda` / `letrec`. Annotations end up
  attached to the matching CoreExpr nodes' new optional slots.
- **1.5** CoreExpr extensions. Add optional `ann` /
  `param_types` / `return_type` fields. All existing callers
  still work (fields default to None).
- **1.6** Round-trip test. Parse + expand a fully-annotated
  fib; assert the resulting CoreExpr has the right
  annotations attached.

**Exit gate:** `(define (fib [n : Fixnum]) : Fixnum (if (< n 2)
n (+ (fib (- n 1)) (fib (- n 2)))))` parses without errors;
inspecting the CoreExpr shows the n:Fixnum param and Fixnum
return.

### Phase 2: Bidirectional checking — atomic types (≈1.5 weeks)

Goal: the checker validates basic typed functions. No unions,
no occurrence typing yet.

**Iters:**

- **2.1** `TypeEnv` data structure. Symbol → Type with
  hierarchical scoping (extend on `Lambda` / `Letrec`).
- **2.2** Built-in primop type table. ~80 primops:
  `(+ : (-> Fixnum Fixnum Fixnum))` (specialized later for
  unions), `(car : (-> Pair Any))`, etc. Source-of-truth in
  `cs-typer/src/builtins.rs`.
- **2.3** `infer(expr) -> Type`. Bottom-up inference for
  Const, Ref (lookup), App (apply procedure type to arg
  types), Begin (last expr's type), If (LUB of branches).
- **2.4** `check(expr, expected) -> Result<()>`. Top-down
  checking driven by annotations: at a Lambda with annotated
  params + return, check body against return_type with params
  in scope as their declared types.
- **2.5** Lambda checking. Annotated params populate the type
  env; body checked against declared return. App checks each
  arg against the corresponding param type.
- **2.6** Letrec checking. Annotated bindings establish their
  types BEFORE checking their values (so recursive references
  see the binding's type).
- **2.7** Untyped fallback. Any CoreExpr node lacking an
  annotation gets type `Any` (the gradual top). Untyped code
  through a typed function: arg `Any` <: declared param type
  if the param is `Any`, else fail.
- **2.8** Diagnostics. Type errors become `cs_diag::Diagnostic`
  with the offending CoreExpr's Span. Format: "expected
  Fixnum, got String".

**Exit gate:** annotated fib typechecks. `(define (fib [n :
Fixnum]) : Fixnum (string-length n))` fails with "expected
Fixnum, got String" at the right source location.

### Phase 3: Union types + procedure types (≈1 week)

Goal: handle `(U Fixnum Flonum)` and procedure signatures.

**Iters:**

- **3.1** Subtype relation. `is_subtype(t, u) -> bool` with
  reflexivity, transitivity, union variance, procedure
  contravariance.
- **3.2** Union narrowing. App against a union procedure type
  (e.g., `+` having `(-> Number Number Number)` where
  `Number = (U Fixnum Flonum)`) returns the union; LUB of
  branch types in If produces unions as needed.
- **3.3** Function-type checking. `(define (g [f : (-> Fixnum
  Fixnum)]) : Fixnum (f 5))` typechecks; `(g 42)` fails with
  "expected procedure, got Fixnum".
- **3.4** Multi-arity / variadic. `(define (sum . xs) ...)`
  with `(: sum (-> Fixnum ... Fixnum))` — rest-args.
- **3.5** Type-alias resolution. `(define-type Number (U
  Fixnum Flonum))` registered in the type env; references
  expand at check time.

**Exit gate:** mandelbrot's `mandelbrot-pixel : (-> Flonum Flonum
Boolean)` typechecks with explicit annotations; passing a
non-Flonum is a type error.

### Phase 4: Occurrence typing (≈1 week)

Goal: `(if (number? x) (+ x 1) x)` narrows x's type in each
branch.

**Iters:**

- **4.1** Predicate type signatures. Built-in predicates get
  filter types: `(number? : (-> Any Boolean : (U Fixnum Flonum)))`.
  The `:` propositional return says "if this returns true, the
  arg is a (U Fixnum Flonum)".
- **4.2** Branch narrowing. If checker: when condition is a
  predicate-typed App, narrow the arg in the then-branch to
  the predicate's positive proposition; in the else-branch
  to its negation.
- **4.3** Negation handling. `(not (number? x))` flips
  positive ↔ negative. `(and (number? x) (positive? x))`
  intersects.
- **4.4** Refinement of unions. If x : (U Fixnum String) and
  we enter `(when (string? x) ...)`, x is narrowed to String
  in the body.
- **4.5** Per-binding refinement. `(let ([x (if cond a b)])
  ...)` — x's type is LUB of a and b's types.

**Exit gate:** the safe? function in nqueens (uses null? and
car/cdr on lists) typechecks without explicit unions.

### Phase 5: JIT / AOT integration (≈1 week)

Goal: typer-inferred types feed the JIT/AOT pipelines as
`param_type_hints`.

**Iters:**

- **5.1** TypedExpr → param hints. After successful check,
  walk the TypedExpr and produce `HashMap<lambda_index,
  Vec<cs_rir::Type>>` mapping each lambda's params to types.
- **5.2** Bytecode compiler threads hints through. When
  compiling a Lambda with typed params, attach the hints to
  the emitted CompiledLambda. The translator's
  `bytecode_to_rir_full` reads them via the existing
  `param_type_hints` parameter.
- **5.3** AOT integration. cs-cli's `aot --multi` uses the
  typer's hints (when available) instead of defaulting to
  Any. Mandelbrot with `[cr : Flonum] [ci : Flonum]`
  annotations recovers Flonum specialization — the iter
  2.16/2.17 regression goes back to near-Rust performance.
- **5.4** JIT integration. The JIT's tier-up hook
  (cs-runtime/src/jit.rs:72-121) consults the typer-derived
  hints when available, treating them as authoritative (not
  single-sample observations).
- **5.5** Bench validation. Re-run the full microbench scorecard:
  - Annotated mandelbrot: target ≤ 2× Rust (recovered from
    16× regression)
  - Annotated spectral-norm: target ≤ 3× Rust (recovered from
    25× regression)
  - Annotated fib: target unchanged (already 3× Rust)
  - Unannotated benches: zero regression (typer no-op)

**Exit gate:** annotating the four hottest benches with their
natural Flonum/Fixnum types recovers performance close to Rust.

### Phase 6: Polish + CLI surface (≈1 week)

Goal: typer is a real product surface, not an internal
machinery.

**Iters:**

- **6.1** `crabscheme check FILE.scm` subcommand. Run parse +
  expand + typecheck; print diagnostics in the standard
  format. Exit 0 on clean, 1 on type errors.
- **6.2** `crabscheme aot --typecheck` flag. Run typecheck
  before AOT'ing; fail fast on type errors.
- **6.3** `crabscheme repl` typechecks each input line if it
  has annotations (otherwise treats as untyped).
- **6.4** Editor integration. cs-lsp (the parallel
  `lsp-server-plan.md` work) consumes typer diagnostics for
  the diagnostics-on-save feature.
- **6.5** Documentation: `docs/user/types.md` covers the
  annotation syntax, the supported types, and migration
  patterns.
- **6.6** Stdlib annotations. Add type signatures to the
  most-used 50 builtins in `cs-runtime/src/builtins/mod.rs`
  via the type table so user code calling them gets full
  checking.

**Exit gate:** a new user reads `docs/user/types.md` and adds
annotations to their .scm file; the editor highlights type
errors as they type; `crabscheme check` reports cleanly when
they fix them.

### Phase 7 (optional, ≈2 weeks): Polymorphism

Goal: `(define (id [x : T]) : T x)` works with explicit type
parameters.

**Iters:**

- **7.1** `Forall` type representation. `(All (T) (-> T T))`.
- **7.2** Type variable lookup + substitution.
- **7.3** Type application syntax: `(inst id Fixnum)`.
- **7.4** Implicit instantiation at call sites via Local Type
  Inference's argument-driven substitution.
- **7.5** Stdlib generic signatures. `(map : (All (A B) (-> (-> A
  B) (Listof A) (Listof B))))`.

**Exit gate:** a polymorphic identity function and a generic
map typecheck. Skip if user demand isn't there.

## Cross-phase concerns

### Performance budget

- Typecheck a 1000-LoC file: target < 100 ms (LSP-friendly).
- Memory per TypedExpr: 2-3× the underlying CoreExpr (acceptable
  because expressions are short-lived in the pipeline).
- No runtime cost for typed code (erasure model).

### Soundness story (or lack thereof)

Erasure-only typing is unsound at typed/untyped boundaries:

```scheme
; untyped:
(define (lie) "not-a-number")

; typed:
(define (f [x : Fixnum]) : Fixnum (+ x 1))

; Boom: (f (lie)) — typer would allow it because (lie) is Any;
; runtime throws "expected Fixnum, got String" from `+`.
```

This is intentional. Documenting it clearly is more honest than
shipping contracts. A future iter (post-Phase 7) could add
optional contract-insertion gated on a `--strict` flag for
users who want full soundness at a perf cost.

### Type annotation discoverability

A typer that lives in a separate crate but isn't easily found
won't get used. The cs-cli should print a helpful note on first
typecheck error: "If you don't want type checking, remove the
`: Type` annotations from your file."

### Backward compatibility

NEVER required. Any program that parsed pre-typer parses post-
typer. Any program that compiled pre-typer compiles post-typer.
The typer activates only when annotations are present.

### Interaction with macros

Hygienic macro expansion runs BEFORE type checking (the typer
operates on expanded CoreExpr). A macro that introduces a `:`
binding name (e.g., for some custom let-form) might confuse the
ascription parser — Phase 1 iter 1.2 needs to be careful that
the lexer only treats `:` as a token in expander-recognized
positions, not inside arbitrary macros.

### Comparing to JIT type feedback

The JIT's single-sample tier-up observation is opportunistic;
the typer is authoritative. When both are available, the
typer's hints take precedence. The JIT's deopt counter
(`jit_deopt_count`) still fires on type mismatches — useful
diagnostic ("you annotated this as Fixnum but the runtime sees
Flonum on iteration 47").

## Risks + open questions

1. **`:` lexer ambiguity.** R6RS identifiers can contain `:`;
   we need to be careful that `set!` keeps working and that
   keyword-like identifiers (`:rest`, `:key`) used by SRFI-89
   don't break. The plan: only standalone `:` between
   whitespace becomes the token; `set!` etc. tokenize as
   identifiers.

2. **Mutable container variance.** `(Vectorof T)` is invariant
   (mutation breaks covariance), but listof in immutable Scheme
   can be covariant. Phase 3 iter 3.1 needs to pin this down.

3. **Union types getting unwieldy.** `(U Fixnum Flonum Pair
   Vector String ByteVector Procedure Symbol Null Character)` is
   noisy. Consider a `Number = (U Fixnum Flonum)` standard alias
   shipped with the typer.

4. **Cross-file checking.** R6RS `(library ... (import ...))`
   crosses files; the typer needs access to imported library
   annotations. Phase 1 ships single-file only; multi-file
   pulls in cs-expand's library machinery.

5. **AOT regression recovery.** Phase 5 iter 5.5 is the
   payoff: re-baselining mandelbrot and spectral-norm at near-
   Rust speed with annotations. If recovery is poor (worse
   than 4× Rust), revisit — maybe the issue is elsewhere in
   the cs-aot lowering.

6. **Annotation syntax disagreements.** The exact bracketing
   (`[n : Fixnum]` vs `[n :: Fixnum]` vs `(n : Fixnum)`) is
   bike-shedable. Default to Typed Racket's `[name : Type]`
   because it's the most-deployed convention and shows up in
   tutorials.

7. **Polymorphism complexity.** Phase 7 is genuinely hard
   (substitution, instantiation, contravariance interactions).
   Defer until user feedback says "I need this for X".

## Reference points

- Typed Racket guide: https://docs.racket-lang.org/ts-guide/
- Typed Racket reference: https://docs.racket-lang.org/ts-reference/
- Typed Clojure (core.typed): https://github.com/clojure/core.typed
- Coalton (HM in Common Lisp): https://github.com/coalton-lang/coalton
- Pierce + Turner LTI: https://www.cis.upenn.edu/~bcpierce/papers/lti.pdf
- Tobin-Hochstadt + Felleisen POPL'08:
  https://www2.ccs.neu.edu/racket/pubs/popl08-thf.pdf
- Greenman + Felleisen ICFP'18 (spectrum):
  https://dl.acm.org/doi/10.1145/3236766
- Bonnaire-Sergeant PhD: https://thesis.ambrosebs.com/

## Success metrics

- **Coverage**: every well-typed program from R7RS-small
  typechecks when minimally annotated. Every common type error
  is caught with a precise span.
- **Performance**: annotated mandelbrot + spectral-norm AOT'd
  binaries reach ≤ 3× Rust (recovering the iter 2.16/2.17 perf
  regression for typed code).
- **Adoption**: at least three of the eight microbenches ship
  with annotations in `bench/microbench/scheme/typed/` so users
  can see the syntax in real code.
- **Tests**: ≥ 100 cs-typer unit tests covering each Phase's
  features.
- **Diagnostics quality**: every error message identifies the
  offending source location and the expected vs actual type.

## Architectural notes specific to crabscheme

The substrate survey turned up:

- `cs_rir::Type` (11 variants) is the natural type vocabulary —
  use it directly. Extend with `Union(Vec<Type>)` / `Procedure(ProcType)`
  / `Listof(Box<Type>)` / `Vectorof(Box<Type>)` either in cs-rir
  itself or in a parallel `cs-typer::Type` wrapper. Prefer the
  wrapper to keep cs-rir minimal (downstream pipelines don't need
  the richer types).

- `param_type_hints: Option<&[cs_rir::Type]>` at
  `bytecode_to_rir_full` is the ready injection point for
  Phase 5. No new translator API needed.

- The translator's `value_types: HashMap<RirValue, Type>`
  occurrence-typing logic is the closest existing model for
  what the source-level typer needs to do. Reuse the patterns
  (predicate matching, branch refinement) at the CoreExpr
  level.

- `LambdaProfile.jit_deopt_count` provides a runtime feedback
  channel: a typed Lambda whose deopt counter is non-zero is
  a type-annotation lie. Could be surfaced as a `crabscheme
  check --runtime` warning that compares typer assumptions
  against JIT observations.

- `Hashtable`, `Port`, `Promise` have NB tags but no
  `cs_rir::Type` variant. The typer surfaces this gap: either
  add the variants (and lower in all backends) or treat these
  as `Any` in the typer. Defer to user demand.

- `CoreExpr` carries `Span` on every node. Diagnostic
  infrastructure is ready.

The typer is conceived as **additive only** — no existing tier
behaves differently for untyped code. Annotated code goes
faster (via better hints) and catches errors at compile time;
unannotated code is unchanged.
