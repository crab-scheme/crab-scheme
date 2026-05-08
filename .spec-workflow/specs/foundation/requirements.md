# Requirements Document — Foundation Milestone

## Introduction

The **Foundation** milestone establishes the load-bearing base of CrabScheme: a
working Scheme reader, a tree-walking interpreter for a meaningful subset of R6RS,
a CLI binary, and an interactive REPL. Foundation is intentionally scoped *below*
the macro system, the bytecode VM, and the JIT — those land in later specs. The
goal here is to have something that can read, evaluate, and print real Scheme code
end-to-end so every subsequent spec lands on a known-good base.

This is the moment we go from empty workspace to "you can type `(+ 1 2)` and see
`3`".

## Alignment with Product Vision

Foundation directly supports five of the seven product principles in
`product.md`:

- **Correctness first**: the tree-walker is the correctness baseline against which
  every later tier is differentially tested.
- **Layered backends, all switchable**: by establishing the IR and runtime
  interfaces now, we make the bytecode VM and JIT swap-in components rather than
  rewrites.
- **Verified at every layer**: conformance harness scaffolding is part of M2 so
  that R6RS pass-rate tracking begins on day one.
- **Diagnostics are a feature**: source spans flow from lexer → parser → IR →
  runtime errors from the start.
- **Embeddable and unsurprising**: the `Runtime` value type and `eval` API are
  fixed in this milestone and become the project's stable embed surface.

This milestone covers the project's first three roadmap steps:

| Roadmap | Scope                                                          |
| ------- | -------------------------------------------------------------- |
| M0      | Workspace bootstrap; `cs-core` value type; `cs-diag`           |
| M1      | Lexer + reader; round-trip pretty-printer; conformance harness |
| M2      | Tree-walking interpreter; REPL; CLI; first conformance tests   |

## Requirements

### Requirement 1 — Workspace Bootstrap

**User Story:** As a contributor cloning the repo, I want a working Cargo
workspace with all foundation crates, CI, and tooling preconfigured, so that I can
run `cargo test` and `cargo run` on first checkout without setup steps beyond
`rustup`.

#### Acceptance Criteria

1. WHEN a fresh clone is checked out, THEN `cargo build --workspace` SHALL succeed
   on Linux x86_64, macOS arm64, and macOS x86_64 with the toolchain pinned in
   `rust-toolchain.toml`.
2. WHEN `cargo test --workspace` is run on a fresh clone, THEN it SHALL pass with
   zero failures.
3. WHEN `cargo clippy --workspace --all-targets -- -D warnings` is run, THEN it
   SHALL succeed with zero warnings.
4. WHEN `cargo fmt --check` is run, THEN it SHALL succeed.
5. WHEN a PR is opened, THEN GitHub Actions SHALL run lint, test, and format checks
   and report status.
6. IF a contributor adds a new crate, THEN it SHALL be listed in workspace
   `Cargo.toml`, follow the `cs-<purpose>` naming convention, and have its
   dependency direction validated by CI.
7. WHEN `cargo doc --workspace --no-deps -- -D warnings` is run, THEN it SHALL
   succeed with zero rustdoc warnings.

### Requirement 2 — Core Value Representation

**User Story:** As a runtime author, I want a single `Value` type that represents
every R6RS datum kind I need for the foundation milestone, so that the
interpreter, reader, and tests share one source of truth.

#### Acceptance Criteria

1. WHEN code constructs a `Value` for any of {fixnum, flonum, boolean, character,
   string, symbol, pair, null/empty-list, vector, procedure, unspecified}, THEN
   the resulting `Value` SHALL round-trip through `Display` (`write`) and
   `Debug`-equivalent reader text identical to canonical R6RS notation.
2. IF a fixnum fits in `i61`, THEN it SHALL be stored inline (tagged) without heap
   allocation; otherwise it SHALL fall back to a heap-allocated bignum.
3. WHEN two `Value`s are compared for `eqv?`, THEN the result SHALL match R6RS
   §11.5 semantics for all foundation-supported types.
4. WHEN two `Value`s are compared for `equal?`, THEN structural equality SHALL be
   computed correctly for cyclic structures without infinite recursion.
5. IF `Value` is mutated (e.g. `set-car!`), THEN mutability SHALL be enforced via
   `RefCell`-style interior mutability scoped to the runtime, with no `unsafe` in
   the public API.
6. WHEN multiple `Runtime` instances exist in the same process, THEN their
   `Value`s SHALL NOT cross-contaminate (no global interner, no static state).
7. WHEN a symbol is interned, THEN `eq?` on two symbols with the same name within
   the same `Runtime` SHALL return `#t` in O(1).

### Requirement 3 — Numeric Tower (Foundation Subset)

**User Story:** As a Scheme programmer, I want exact integers and inexact reals
that obey R6RS numeric semantics, so that arithmetic operations produce the
correct value and tag.

#### Acceptance Criteria

1. WHEN `+`, `-`, `*`, `/` are applied to fixnums and the result fits, THEN the
   result SHALL be a fixnum.
2. IF a fixnum operation overflows, THEN the result SHALL be promoted to bignum
   without raising.
3. WHEN integer division `/` produces a non-integer, THEN the result SHALL be an
   exact rational (or bignum if integral) — never silently truncated.
4. WHEN any operation involves an inexact operand, THEN the result SHALL be
   inexact per R6RS §11.7.1 contagion rules.
5. WHEN the literal `1/3` is read, THEN it SHALL produce an exact rational, not
   a flonum approximation.
6. WHEN `(exact->inexact 1/3)` is called, THEN it SHALL produce an IEEE 754 double
   matching the standard rounding.
7. *Note*: complex numbers are deferred to a later spec; this milestone supports
   {fixnum, bignum, rational, flonum}.

### Requirement 4 — Lexer and Reader

**User Story:** As a Scheme programmer, I want CrabScheme to read all syntactic
forms in R6RS §4 (lexical syntax and datum syntax), so that valid Scheme files
parse without modification.

#### Acceptance Criteria

1. WHEN a Scheme source file containing any combination of {numbers in any radix
   notation, strings with all R6RS escape sequences, characters including named
   forms `#\space` and `#\nul`, symbols including pipe-quoted `|with spaces|`,
   booleans, pairs, lists, vectors `#(…)`, bytevectors `#vu8(…)`, quote `'`,
   quasiquote `` ` ``, unquote `,`, unquote-splicing `,@`, datum comments `#;`,
   block comments `#| … |#`, line comments `;`} is read, THEN it SHALL produce a
   `Datum` tree matching the canonical R6RS interpretation.
2. WHEN any token or datum is read, THEN it SHALL carry a source span
   (`(file_id, byte_start, byte_end, line, column)`) accessible for diagnostics.
3. WHEN syntactically invalid input is read, THEN the reader SHALL produce a
   structured `ReaderError` with span and a human-readable message — never panic.
4. WHEN a string contains Unicode escapes `\xHHHH;`, THEN the result SHALL be the
   correct codepoint per R6RS §4.2.7, with NFC normalization applied to symbols.
5. WHEN comments and whitespace surround a datum, THEN they SHALL be preserved as
   metadata if the reader is configured for round-trip mode (used by the AOT
   emitter and pretty-printer); otherwise they SHALL be stripped.
6. WHEN a `#!r6rs` or `#!r7rs` directive is read, THEN it SHALL be recognized and
   recorded in the source's metadata; the directive itself SHALL NOT appear as a
   datum.
7. WHEN `(read)` is called from Scheme on a port, THEN the resulting datum SHALL
   match the offline reader exactly (single reader implementation, two entry
   points).

### Requirement 5 — Core Form Recognition and Tree-Walking Evaluation

**User Story:** As a Scheme programmer, I want to evaluate the R6RS core forms
without depending on the full macro system, so that I can run real Scheme code at
this milestone.

#### Acceptance Criteria

1. WHEN any of {`quote`, `lambda`, `if`, `set!`, `begin`, `define`, `let`,
   `let*`, `letrec`, `letrec*`, `and`, `or`, `cond`, `case`, `when`, `unless`} is
   encountered, THEN it SHALL be evaluated per R6RS §11 semantics.
2. WHEN a procedure is applied with the wrong arity, THEN a structured runtime
   condition SHALL be raised with the procedure name (if available) and the
   argument count.
3. WHEN a tail call is made (per R6RS §11.20 tail-call definitions), THEN the
   stack SHALL NOT grow — proper tail-call elimination is mandatory.
4. WHEN a free variable is referenced, THEN an `&undefined` condition SHALL be
   raised with the variable name and source span.
5. WHEN `define` is used at top level, THEN the binding SHALL be installed in the
   current `Runtime`'s top-level environment.
6. WHEN `define` is used inside a body, THEN it SHALL be lifted to a `letrec*`
   per R6RS §11.4.6 internal-definition semantics.
7. WHEN a closure is constructed, THEN it SHALL capture its lexical environment
   correctly, with no sharing bugs across separate closures.

### Requirement 6 — Built-in Procedures (Foundation Subset)

**User Story:** As a Scheme programmer, I want the core R6RS arithmetic, list,
string, character, comparison, and I/O procedures available in the foundation
milestone, so that I can write meaningful programs.

#### Acceptance Criteria

1. WHEN any of the following are called, THEN they SHALL behave per R6RS §11:
   `+`, `-`, `*`, `/`, `=`, `<`, `>`, `<=`, `>=`, `zero?`, `positive?`,
   `negative?`, `abs`, `min`, `max`, `quotient`, `remainder`, `modulo`,
   `expt`, `exact`, `inexact`, `number?`, `integer?`, `rational?`, `real?`,
   `boolean?`, `pair?`, `null?`, `symbol?`, `string?`, `procedure?`,
   `cons`, `car`, `cdr`, `list`, `length`, `append`, `reverse`, `map`,
   `for-each`, `assoc`, `member`, `eq?`, `eqv?`, `equal?`, `not`,
   `string-length`, `string-ref`, `string->symbol`, `symbol->string`,
   `string->list`, `list->string`, `string=?`, `string<?`,
   `display`, `write`, `newline`, `read`, `eof-object?`,
   `error`, `apply`, `values`, `call-with-values`, `dynamic-wind`.
2. WHEN `dynamic-wind` is invoked, THEN before/thunk/after SHALL execute in the
   order R6RS §11.15 mandates, even if the thunk raises.
3. WHEN `apply` is called with a procedure and a list, THEN the procedure SHALL
   be called with the list elements as arguments.
4. WHEN `error` is called, THEN it SHALL raise a `&error` condition with the
   supplied message and irritants.
5. WHEN any built-in is called with arguments of the wrong type, THEN it SHALL
   raise a `&assertion` condition with span pointing to the call site.
6. *Note*: `call-with-current-continuation` is **not** required at foundation;
   added in the continuations spec (M3+). Tail calls must work; full
   continuations need not.

### Requirement 7 — REPL

**User Story:** As a Scheme practitioner, I want an interactive REPL with line
editing, history, and multi-line input, so that I can explore the language
interactively.

#### Acceptance Criteria

1. WHEN `crabscheme repl` (or just `crabscheme` with no args) is run, THEN a
   REPL prompt SHALL appear within 50 ms.
2. WHEN the user types a complete Scheme expression and presses Enter, THEN the
   expression SHALL be read, evaluated, and the result printed using
   `write`-format, followed by a fresh prompt.
3. WHEN the user types an incomplete expression (unbalanced parens or string),
   THEN the REPL SHALL switch to a continuation prompt and accept additional
   input until the expression is complete.
4. WHEN the user presses Up/Down arrows, THEN history navigation SHALL work.
5. WHEN the user presses Ctrl-C during input, THEN the current input SHALL be
   discarded and a fresh prompt printed (no exit).
6. WHEN the user presses Ctrl-D on an empty prompt, THEN the REPL SHALL exit
   cleanly with status 0.
7. WHEN evaluation raises a Scheme condition, THEN the REPL SHALL print a
   diagnostic with source span pointing into the input buffer, then return to
   the prompt.
8. WHEN `crabscheme repl --history <path>` is supplied, THEN history SHALL be
   persisted to `<path>` across sessions; default is
   `${XDG_DATA_HOME:-~/.local/share}/crabscheme/history`.

### Requirement 8 — CLI

**User Story:** As a script author, I want a `crabscheme` binary with the
standard CLI shapes I expect from a Scheme implementation, so that I can run
files, evaluate one-shot expressions, and embed CrabScheme in shell pipelines.

#### Acceptance Criteria

1. WHEN `crabscheme run <file.scm>` is invoked, THEN the file SHALL be read,
   expanded, evaluated, and the process SHALL exit with the file's exit status
   (0 on normal completion).
2. WHEN `crabscheme -e '<expr>'` is invoked, THEN `<expr>` SHALL be evaluated
   and its result printed to stdout in `write` format.
3. WHEN `crabscheme --` is invoked with subsequent arguments, THEN those
   arguments SHALL be available to the program via `(command-line)`.
4. WHEN `crabscheme --help` is invoked, THEN structured help text SHALL be
   printed, including all subcommands and flags.
5. WHEN `crabscheme --version` is invoked, THEN the version, target triple, git
   commit (if available), and Rust toolchain SHALL be printed.
6. WHEN any CLI invocation encounters an unrecoverable error before the runtime
   is ready (e.g. file not found), THEN it SHALL exit with a non-zero status and
   a diagnostic to stderr.
7. WHEN `crabscheme run --trace=<phase[,phase…]>` is supplied, THEN structured
   trace events for the listed phases SHALL be emitted to stderr in JSON Lines.

### Requirement 9 — Diagnostics

**User Story:** As a developer using CrabScheme, I want errors that point at the
exact source location and explain what went wrong, so that I can fix problems
quickly.

#### Acceptance Criteria

1. WHEN any error originates from a span-bearing source (file or REPL input),
   THEN the diagnostic SHALL include the file name (or "<repl>"), line and
   column, the source line itself with a caret pointing at the offending span.
2. WHEN multiple errors occur in a single read/expand pass, THEN the reader and
   expander SHALL accumulate them up to a configurable limit (default 10) and
   emit them all before exiting.
3. WHEN an error references a Scheme identifier, THEN the diagnostic SHALL
   include the identifier name verbatim.
4. WHEN diagnostics are rendered, THEN ANSI color SHALL be used on TTY stderr
   only; `--color=always|never|auto` SHALL override.
5. WHEN diagnostic output is piped to a non-TTY, THEN no ANSI codes SHALL be
   emitted.
6. WHEN a runtime condition is raised, THEN its `&irritants` and `&who` fields
   SHALL be rendered legibly.

### Requirement 10 — Conformance Harness Scaffolding

**User Story:** As a project maintainer, I want a conformance test harness that
runs at least a starter set of R6RS tests on every CI run, so that pass-rate
regressions are caught immediately.

#### Acceptance Criteria

1. WHEN `cargo xtask conformance` is run, THEN it SHALL execute a curated R6RS
   test corpus and report pass/fail/skip counts plus a list of failures.
2. WHEN the corpus is run, THEN each test SHALL execute in an isolated `Runtime`
   instance.
3. WHEN a test passes on the tree-walker, THEN it SHALL be marked PASS; tests
   requiring features not yet implemented SHALL be marked SKIP with a reason.
4. WHEN the harness completes, THEN it SHALL emit a JSON report
   (`target/conformance.json`) consumable by the dashboard generator.
5. WHEN CI runs, THEN the conformance pass count SHALL be visible as a status
   check; any decrease from the previous trunk commit fails the PR.
6. WHEN the foundation milestone closes, THEN at least 100 R6RS tests covering
   §11.5 (equivalence), §11.7 (arithmetic), §11.8 (booleans), §11.9 (lists),
   §11.10 (symbols), §11.11 (characters), §11.12 (strings), and §11.16
   (procedures) SHALL be passing.

## Non-Functional Requirements

### Code Architecture and Modularity

- **Single Responsibility Principle**: each crate has the responsibility named in
  `structure.md`. The reader does not evaluate; the evaluator does not parse.
- **Modular Design**: the value type, lexer, parser, expander, and runtime are
  independent crates connected only by their public types.
- **Dependency Management**: dependency direction enforced by CI; no upward or
  cyclic deps. Workspace deps unified in root `Cargo.toml`.
- **Clear Interfaces**: every cross-crate contract is a Rust trait or a small set
  of public types; no leaking of `pub(crate)` internals.

### Performance

- Cold start (`crabscheme -e '(+ 1 2)'`): < 50 ms wall-clock on commodity laptops.
- REPL prompt latency after startup: < 5 ms.
- Tree-walker `(fib 25)`: completes in < 1 s on commodity laptops (sanity floor;
  the tree-walker is *not* the perf tier).
- Reader throughput: > 5 MB/s on reasonable Scheme source.
- Memory: foundation runtime baseline RSS < 25 MB on cold REPL.

### Security

- No `unsafe` in any foundation crate without an inline `// SAFETY:` justification
  reviewed at PR time.
- File I/O paths from CLI are opened with the user's privileges only; no
  privilege escalation paths.
- REPL history files are written with mode `0600` on POSIX.

### Reliability

- Zero panics on any well-formed input, well-formed or otherwise. All errors are
  structured `Result`s or Scheme conditions.
- 1-hour fuzz run on the lexer and reader produces no panics or memory errors.
- Differential test corpus (≥ 200 expressions) passes between offline and REPL
  evaluation paths.

### Usability

- Diagnostics readable by a Scheme programmer with no Rust knowledge.
- `crabscheme --help` usable as a single source of CLI truth.
- REPL behavior matches well-established conventions (Chez/Racket-style).
