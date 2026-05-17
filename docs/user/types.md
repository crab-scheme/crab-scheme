# CrabScheme Types

CrabScheme has an optional, gradual type system inspired by
[Typed Racket][tr]. You can annotate any function or
top-level binding with a type; unannotated code keeps
running exactly as before. The checker catches mismatches
at compile time; well-typed code flows into the JIT and AOT
pipelines as `param_type_hints` for specialized codegen.

This document covers the surface syntax, the type lattice,
the gradual-typing semantics, and how to migrate untyped
code.

[tr]: https://docs.racket-lang.org/ts-guide/

## Quick start

```scheme
;; Top-level type ascription.
(: square (-> Fixnum Fixnum))
(define (square x) (fx* x x))

;; Inline form — typed `define` sugar.
(define (cube [x : Fixnum]) : Fixnum
  (fx* x (square x)))

;; Type aliases.
(define-type Number (U Fixnum Flonum))
(: doubled (-> Number Number))
(define (doubled [x : Number]) : Number
  (+ x x))
```

Run `crabscheme check my.scm` to typecheck without
evaluating; the exit code is 0 on clean, 1 on type errors,
2 on parse / expand failures. Add `--typecheck` to
`crabscheme aot --multi` to fail-fast before AOT.

## Annotation syntax

### Top-level ascription

```scheme
(: NAME TYPE)
```

Attaches `TYPE` to the next top-level `define` of `NAME`. The
ascription form itself produces no runtime value — it's
purely typer metadata. Place it before the binding it
ascribes. Multiple ascriptions for the same name keep the
first (subsequent ones are ignored).

### Typed `define` sugar

```scheme
(define (NAME [PARAM1 : T1] [PARAM2 : T2] …) : RETURN-TYPE
  BODY …)
```

Equivalent to writing `(: NAME (-> T1 T2 … RETURN-TYPE))`
plus an unannotated define. The typer synthesizes the
top-level ascription so downstream tools (JIT/AOT hints,
the editor / LSP) see both forms identically.

Partial annotations are allowed — any param can be left
bare, in which case its type defaults to `Any`:

```scheme
(define (mixed [x : Fixnum] y) : Fixnum
  (fx+ x (if (fixnum? y) y 0)))
```

### Type aliases

```scheme
(define-type NAME TYPE)
```

Introduces a name for any type. Aliases are substituted at
parse time; later aliases can reference earlier ones, but
forward references are not supported.

```scheme
(define-type Number (U Fixnum Flonum))
(define-type NumOrStr (U Number String))   ; resolves Number
```

## The type lattice

### Atomic types

Eleven atoms mirror the runtime's tag set:

| Type        | What it represents                       |
|-------------|------------------------------------------|
| `Fixnum`    | Machine-integer-tagged numbers           |
| `Flonum`    | IEEE-754 double-precision floats         |
| `Boolean`   | `#t` / `#f`                              |
| `Character` | A single Scheme character                |
| `Symbol`    | An interned symbol                       |
| `Pair`      | A cons cell                              |
| `Null`      | The empty list `()`                      |
| `Vector`    | An R6RS vector                           |
| `String`    | A Scheme string                          |
| `ByteVector`| A bytevector                             |
| `Procedure` | A first-class procedure                  |

### Special forms

| Type        | Meaning                                                                |
|-------------|------------------------------------------------------------------------|
| `Any`       | Gradual top. Untyped values; admits and is admitted by every position. |
| `Never`     | Bottom. Unreachable code (`(raise …)`, infinite loops).                |

### Compound types

```text
(U T1 T2 …)        ; union — value is one of the members
(-> T1 T2 … R)     ; procedure from T1, T2, … to R
(-> T1 … T R)      ; variadic — at least 0 trailing T-typed args
(Listof T)         ; homogeneous list
(Vectorof T)       ; homogeneous vector
```

Union members are sorted canonically: `(U Fixnum Flonum)` ==
`(U Flonum Fixnum)`. `Any` absorbs — `(U Fixnum Any)` is
just `Any`. Function types support contravariance on params
and covariance on return type.

## Gradual typing semantics

Two rules combine to make typed and untyped code coexist:

1. **Untyped code is `Any`.** Any binding without an
   annotation, any value from an untyped function call, any
   constant in an unannotated context — all get the gradual
   top type.

2. **`Any` flows both ways.** `Any` is a subtype of every
   type, and every type is a subtype of `Any`. So untyped
   code can call typed code with no ceremony (the untyped
   `Any`-typed argument satisfies a typed `Fixnum` param),
   and typed code can pass typed values into untyped
   contexts. No runtime contracts are inserted (yet) — see
   the "Runtime guarantees" section below.

### Predicate narrowing

Built-in type predicates (`fixnum?`, `string?`, `pair?`, …)
carry filter types. Inside an `if`'s then-branch, the
operand's type narrows to the predicate's positive type; in
the else-branch, to the negation:

```scheme
(define-type Maybe (U Fixnum #f))
(: safe-inc (-> Maybe Fixnum))
(define (safe-inc [x : Maybe]) : Fixnum
  (if (fixnum? x)
      (fx+ x 1)     ; x : Fixnum here
      0))           ; x : #f here
```

`not` flips polarity. `and` and `or` compose via the
expander's nested-`if` desugaring, so narrowings chain
naturally:

```scheme
(define-type PairOrNull (U Pair Null))
(: safe-first (-> PairOrNull Any))
(define (safe-first [lst : PairOrNull]) : Any
  (if (and (pair? lst) (number? (car lst)))
      (car lst)                        ; lst : Pair
      'nope))
```

### Per-binding refinement

`(let ((x …)) …)` desugars to an immediately-applied
lambda. When the lambda's param is unannotated, the body
sees `x` typed as the inferred type of the bound value (not
`Any`):

```scheme
(let ((n (string-length "hi")))    ; n : Fixnum
  (fx+ n 1))
```

For explicitly-typed binds, the declared type wins.

## Numeric primops — generic vs narrow

Phase 3 widened generic arithmetic to operate on `Number`:

| Primop            | Signature                              |
|-------------------|----------------------------------------|
| `+ - * /`         | `(-> Number Number Number)`            |
| `< > <= >= =`     | `(-> Number Number Boolean)`           |
| `abs min max …`   | `(-> Number Number Number)`            |
| `modulo quotient` | `(-> Fixnum Fixnum Fixnum)` (integer)  |
| `fx+ fx- fx< …`   | `(-> Fixnum Fixnum Fixnum)` (narrow)   |
| `fl+ fl- fl< …`   | `(-> Flonum Flonum Flonum)` (narrow)   |

If you need Fixnum-only or Flonum-only precision through a
return-type check, use the narrow family. Generic `(* x x)`
inside a Fixnum-returning function fails because the result
type is `(U Fixnum Flonum)`, not `Fixnum`:

```scheme
;; Fails: returns (U Fixnum Flonum), not Fixnum.
(: bad (-> Fixnum Fixnum))
(define (bad x) (* x x))

;; OK: fx* is Fixnum-only.
(: good (-> Fixnum Fixnum))
(define (good x) (fx* x x))
```

## Runtime guarantees

The typer is **erasure-style**: annotations have no
runtime effect. No contracts are inserted at typed/untyped
boundaries, so a malicious or buggy untyped caller passing
a `String` where a typed `Fixnum` was promised will
manifest at the first downstream operation that cares.

This matches the "Spectrum of Type Soundness and
Performance" (Greenman & Felleisen) `Erasure` model: low
overhead, no extra allocations, no per-call dispatch — at
the cost of weak runtime guarantees. A future iter may
add optional contract insertion at typed/untyped
boundaries.

## Migration patterns

### Untyped → fully typed

Start with `crabscheme check`. The checker exits 0 on any
untyped program. Add ascriptions incrementally:

```scheme
;; Step 1: add a single top-level ascription.
(: foo (-> Fixnum Fixnum))
(define (foo x) (fx* x 2))

;; Step 2: tighten an unannotated helper that foo calls.
(: helper (-> Fixnum Fixnum))
(define (helper x) (fx+ x 1))
```

The typer's gradual rule means each newly-annotated
function checks independently; you don't have to convert
your whole program at once.

### Mixed-type call sites

If a typed function receives a value from untyped code, the
gradual `Any → T` rule lets it pass. Inside the typed body,
the param is treated as its declared type — so the untyped
caller is the source of any runtime mistype. Conversely,
typed code calling untyped code gets `Any` back from any
untyped function:

```scheme
(define (untyped-counter)
  (let ((n 0))
    (lambda ()
      (set! n (+ n 1))
      n)))

(: use-counter (-> Any Fixnum))
(define (use-counter c) : Fixnum
  (if (fixnum? (c))
      (c)
      0))
```

### Generic arithmetic in typed Fixnum kernels

If your hot path is integer arithmetic and you've ascribed
it as `Fixnum → Fixnum`, switch generic `+ - *` to the
`fx*` family. The Phase-3 widening is correct (`+` does
accept Flonums at runtime) but throws away Fixnum-precision
for the typer. The `fx*` family is the same speed in
practice; you just opt into the narrow signature.

## CLI subcommands

| Command                                 | Behavior                                    |
|-----------------------------------------|---------------------------------------------|
| `crabscheme check FILE.scm`             | Typecheck only. Exit 0 / 1 / 2.             |
| `crabscheme aot --multi --typecheck …`  | Typecheck before AOT; fail fast on errors.  |
| `crabscheme aot --multi …` (no flag)    | Warn on annotation syntax errors; proceed.  |
| `crabscheme repl`                       | Typecheck annotated REPL input inline.      |

## What's not supported (yet)

- `(let ([x : T value]) …)` — typed `let`/`letrec` binding
  syntax. The parser already accepts the `[name : T value]`
  shape; wiring it through extract is open work.
- Polymorphism (`(All (T) (-> T T))`) — Phase 7.
- Recursive type aliases (`(define-type Tree (U Leaf Pair))`
  where Pair refers back to Tree).
- Contract insertion at typed/untyped boundaries — see
  "Runtime guarantees" above.
- Refinement of `(Listof T)` element types via
  `pair? + (car lst)` — narrowing is per-binding, not
  through structural decomposition.

See `docs/milestones/typer-plan.md` for the phase-by-phase
implementation status and the open backlog.
