# CrabScheme

R6RS-flavored Scheme implementation in Rust. Two execution tiers (tree-walker
and bytecode VM) verified to produce identical results across 33 conformance
files (~700 R6RS test cases). VM tier runs 2.5–3× faster than the walker on
recursion-heavy workloads.

```
$ crabscheme -e '(letrec ((fib (lambda (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))) (fib 25))'
75025
```

## Status

The implementation is a working subset of R6RS suitable for running real
programs. Foundation milestones M0–M4 (lexer, parser, expander, tree-walker,
bytecode VM) and M9 (~85% of the R6RS standard library surface) are complete;
M8 (escape-only `call/cc`) shipped in the latest iteration.

What works:

- Lexer + reader + R6RS-flavored expander (incl. `syntax-rules` with
  hygienic binder renaming, `let`, `let*`, `letrec`, `do`, `case`,
  `cond`, `guard`, `quasiquote`, `define-record-type`)
- Numeric tower: fixnum, bignum, rational, flonum (auto-promote on overflow)
- Strings, characters, vectors, bytevectors, hashtables (eq/eqv/equal),
  ports (string-input, string-output, file-input), promises, parameters,
  conditions
- Tree-walking interpreter with proper TCE
- Bytecode VM with stack machine + TCE + const-folded globals
- Cross-tier higher-order bridge: `apply`, `map`, `for-each`, `filter`,
  `find`, `any`, `every`, `fold-left`, `fold-right`, `reduce`, `count`,
  `partition`, `values`, `call-with-values`, `vector-map`,
  `vector-for-each`, `vector-fold`, `vector-filter`, `string-map`,
  `string-for-each`, `hashtable-walk`, `hashtable-fold`,
  `hashtable-for-each`, `hashtable-update!`, `unfold`, `tabulate`,
  `remove`, `force`, `display`, `write`, `newline`, `read`, `read-line`,
  `with-output-to-string`, `with-input-from-string`,
  `current-input-port`, `current-output-port`, `raise`, `error`,
  `with-exception-handler`, `dynamic-wind`, `call/cc`,
  `call-with-current-continuation`, `eval`, `list-sort`, `vector-sort`,
  `vector-sort!`
- 33 conformance files run identically on both tiers

What's deferred (post-M9):

- M5: precise tracing GC (currently uses Rc; cycles aren't reclaimed)
- M6/M7: Cranelift JIT, HolyJIT integration
- M8: full multi-shot `call/cc` (only escape-only is implemented;
  capturing a continuation and invoking it after its dynamic extent has
  ended is not supported)
- M10: AOT compilation, WASM target
- M11: verified core proofs

## Quickstart

```bash
# Evaluate an expression and print the result.
cargo run --release -- -e '(* 6 7)'

# Run a Scheme source file.
cargo run --release -- run examples/factorial.scm

# Start an interactive REPL (tree-walker by default).
cargo run --release -- repl

# Same, on the bytecode VM tier.
cargo run --release -- --tier vm repl
```

### REPL commands

The REPL accepts both Scheme expressions and `:`-prefixed commands:

| Command | Effect |
|---|---|
| `:help` | List commands |
| `:quit` | Exit (also `^D`) |
| `:tier walker\|vm` | Switch execution tier |
| `:time <expr>` | Evaluate `<expr>` and print wall-clock time |
| `:reset` | Reinitialize the runtime, dropping all definitions |

Example session:

```
crabscheme 0.0.1 (walker) — :help for commands, ^D to exit
> (define (fib n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
> :time (fib 25)
75025
; 105.012ms
> :tier vm
tier: vm
vm> :time (fib 25)
75025
; 38.940ms
```

## Architecture

Crate layout:

| Crate | Purpose |
|---|---|
| `cs-core` | Universal `Value` enum, `Symbol`/`SymbolTable`, numeric tower, eq/eqv/equal |
| `cs-diag` | Spans, source map, diagnostic rendering |
| `cs-lex` | Tokenizer |
| `cs-parse` | Reader producing `Datum` from token stream |
| `cs-ir` | `CoreExpr` — post-expansion AST |
| `cs-expand` | Macro expander (R6RS `syntax-rules`, derived forms) |
| `cs-runtime` | Tree-walking interpreter, environments, ~80 builtins |
| `cs-vm` | Stack-based bytecode VM with const-folded globals |
| `cs-cli` | `crabscheme` binary (REPL, `-e`, `run`) |

Both interpreters dispatch to the same `cs-core::Value` types and call into
the same `cs-runtime` builtins where possible. The VM has an HO-bridge layer
(`vm_call_sync`) that re-enters the VM for each closure invocation from a
higher-order builtin.

## Testing

```bash
# Full workspace test suite (~205 Rust tests + 33 VM conformance + 33 walker conformance).
cargo test --workspace

# Performance baseline (native Rust vs walker vs VM).
cargo test --release --test perf_baseline -- --ignored --nocapture
```

Differential testing: every conformance file runs through both tiers and
their pass counts must match exactly. The VM is ~3× faster than the walker
across the suite.

## Performance

Release-mode benchmarks vs native hand-rolled Rust (lower is better):

| Workload | Native Rust | Walker | VM | VM/Walker |
|---|---|---|---|---|
| `fib(25)` recursive | 150 µs | 105 ms | 39 ms | 2.7× |
| `loop 100k` (tail-call) | 8 ns | 47 ms | 16 ms | 2.9× |
| `ackermann(3,6)` | 410 µs | 91 ms | 32 ms | 2.8× |
| `fold-left + 0 (range 10k)` | 2.7 µs | 6.0 ms | 2.7 ms | 2.4× |
| `(map (lambda (x) (* x x)) (range 1k))` | 130 ns | 760 µs | 400 µs | 1.9× |

Interpreter overhead vs native Rust ranges from ~200× (fib) to ~5M× (the
trivial `loop` workload, where the native loop is just `acc++; n--`).
There's plenty of headroom for a future JIT to capture.

## License

TBD.
