# ADR 0029: Expand-time built-in syntax-class checks for `define-syntax-parser`

> Status: Accepted
> Date: 2026-05-26
> Authors: crab-scheme contributors

## Context

`define-syntax-parser` (R6RS++ Phase 2A.1) lets a macro pattern annotate
a pattern variable with a syntax class: `name:id`, `n:number`,
`s:string`, `e:expr`, or a user class registered via
`define-syntax-class`. Until now every class was enforced the same way:
the desugar wrapped the rule body in a runtime predicate check —

```scheme
(if (identifier? (quote name)) <body> (error 'm "expected id for `name`" ...))
```

ADR 0028 (the #32 migration) hit the wall this creates: the `if`-wrap
forces `<body>` into **expression position**, so any macro whose body is
a *definition* (`define`, `define-record-type`) fails to expand with
"define not allowed in expression position". Every name-taking macro
worth validating — `define/contract`, `define/typed`, `define-record` —
is exactly that shape, so the #32 migration could add **zero** `:class`
annotations. ADR 0028 left "expand-time `:class` checking" as the
follow-up; this ADR is that follow-up.

The existing `phase2_syntax_parser.rs` test module already anticipated
the direction: *"On class mismatch the expanded code raises an error at
RUNTIME … Phase 2A.4 lifts this to expand-time pinpointing."*

## Decision

### Built-in classes are checked at expand time; the body is left unwrapped

The three built-in **structural** classes become checks on the matched
*syntax*, run after a rule matches but before its template instantiates
(in `try_expand_macro`):

| class    | satisfied when the matched form is |
|----------|------------------------------------|
| `id`     | an identifier (a symbol)           |
| `number` | a number literal                   |
| `string` | a string literal                   |
| `expr`   | anything (no check)                |

A violation is a pinpointed `ExpandError` anchored at the offending
sub-form. Crucially the rule **body is emitted unwrapped**, so a
definition body stays in definition position. That is the entire point:
`(define-syntax-parser def-const ((_ name:id val) (define name val)))`
now expands, and `(def-const 5 v)` is an expand-time "expected id"
error.

The class metadata is carried on the `Macro` (`class_checks: Vec<Vec<
ClassCheck>>`, parallel to `rules`). Because that metadata cannot
survive a `syntax-rules` desugar, a class-annotated (or combinator-using)
`define-syntax-parser` now builds its `Macro` **directly**; the common
no-class, no-combinator case still desugars to `syntax-rules` unchanged.

### Built-in classes are syntactic; user classes stay runtime

`:number` now means "**a number literal**", not "evaluates to a number".
This is the only behavior change, and it is inherent: at expand time the
only thing available is the syntax, so a syntax class can only be a
syntactic predicate (this is precisely Racket's `syntax-parse` model).

A runtime *value* guard is a different feature, and it already exists:
user-defined classes registered with `(define-syntax-class c pred?)`
keep the runtime `(if (pred? v) body (error …))` wrap, since `pred?` can
only run against a runtime value. The split is therefore clean:

- **built-in class** → syntactic, expand-time, composes with a
  definition body;
- **user class** → runtime value predicate, does *not* compose with a
  definition body (a fundamental limit, not a missing feature).

### A matched rule's class failure is a hard error (no fall-through)

If a rule matches structurally but fails a class check, expansion stops
with the pinpointed error rather than falling through to later clauses.
This preserves the prior (runtime-wrap) behavior and gives the better
diagnostic the feature is for: `(define/contract 5 …)` reports "expected
id", not "no matching rule".

### The motivating library macros now carry `:id`

With the body-position blocker gone, the `name` argument of all five
definition-bodied stdlib macros is annotated `name:id`:
`define/contract`, `provide/contract` (each provided name, via the
ellipsis), `define/typed`, `define-record`, `define-record-mutable`. A
non-identifier name is now a pinpointed expand-time error. Every real
in-tree use passes a bare identifier (verified), so this is
behavior-preserving for valid code.

## Consequences

### Positive
- Definition-bodied macros can finally validate their identifier
  arguments — the deliverable #32 deferred.
- Errors move from run time to expand time and are pinpointed at the
  offending form (LSP-underlineable, via the existing #33 span plumbing).
- One coherent model: built-in = syntactic, user-defined = runtime value.

### Negative / limitations
- `:number` / `:string` no longer accept a compound expression that
  *evaluates* to a number/string (e.g. `(num-double (+ 1 2))`); that is
  the documented semantic shift. No in-tree macro relied on the old
  value semantics; for a value guard, use a `define-syntax-class`
  predicate.
- A user (runtime) value-class on a definition-bodied macro still cannot
  compose (the wrap still demotes the body). This is fundamental — such
  a class can only be checked at run time.

## Testing

- `phase2a4_expand_class_checks` (7, new): definition-body `:id` macro
  expands + runs; multi-`define` body; the check fires at expand time
  (a never-called lambda body still errors); `:number`/`:string` literal
  semantics; the check pins the structurally-matched clause; a classless
  parser macro is unchanged.
- `phase4_define_typed::define_typed_rejects_non_identifier_name` (new):
  the real `define/typed` macro rejects a numeric name with "expected
  id".
- Unchanged and green: `phase2_syntax_parser`, `phase2_syntax_class`
  (user value-class still runtime), `phase2a3_syntax_parse_combinators`
  (class inside `~optional`), `phase2a5_syntax_parser_literals`, and all
  contract / typed / record suites (the annotated macros).

## Follow-ups
- Field names in `define-record` (`(field:id ...)`) could likewise be
  validated; left out to keep this change to the documented "name
  argument" scope.
- A `Datum`-level "is this a valid definition name" check could later
  subsume the structural `id` class if hygiene introduces renamed
  identifiers.

## References
- Issue #32 / internal task #143; ADR 0028 (literals migration, the
  follow-up source).
- `crates/cs-expand/src/lib.rs` — `Macro::class_checks` / `BuiltinClass`
  / `ClassCheck`, `expand_define_syntax_parser` (partition),
  `try_expand_macro` + `check_expand_classes` (the expand-time check).
- ADR 0021 (static `define/typed`), #31 (syntax-parse combinators), #33
  (expand-time error pinpointing — the span plumbing reused here).
