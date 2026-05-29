# Sandboxing — running untrusted Scheme safely

CrabScheme offers two layers of isolation for evaluating Scheme code
you don't fully trust:

| Layer | Mechanism | Cost | Use when |
|---|---|---|---|
| **L1** | Immutable / mutable namespaces; `eval` against a restricted environment | Sub-microsecond — same process | The code's imports need limiting (e.g. no `system`, no FFI) but you can trust it not to be actively malicious |
| **L2** | A real wasmtime instance hosting `crabscheme.wasm`; fuel + epoch + wall-clock limits | Sub-millisecond warm RTT | The code may be actively malicious — you need a memory + CPU + syscall boundary |

The two layers **compose**: you can run L1 inside L2 for
defense-in-depth (the WASM boundary contains a possible host-level
exploit; the L1 environment restricts what library imports the guest
can even reference).

Design in [ADR 0015](../adr/0015-sandboxing.md).

## L1 — environment + namespace isolation

### `(environment '(import-spec) ...)` — immutable snapshot

Returns a frozen binding snapshot. Pair with `eval` to run code that
can only see the imports you declare.

```scheme
(define safe-env
  (environment '(rnrs base)))   ; only (rnrs base) — no FFI, no I/O

(eval '(+ 2 3) safe-env)         ; ⇒ 5
(eval '(open-input-file "/etc/passwd") safe-env)
; ⇒ error: unbound identifier open-input-file
```

The bindings are taken from the current process at the moment
`(environment …)` runs and are immutable thereafter — even if the
host process later loads more libraries into the matching imports,
`safe-env` keeps the original snapshot.

### `(make-namespace ...)` — mutable namespace

Same idea but mutable. Useful for sandboxes that need to provide a
narrow capability surface that grows over the sandbox's life (e.g. a
plugin host that adds builtins per-plugin).

```scheme
(define ns (make-namespace '(rnrs base)))
; ... programmatically add bindings via namespace-set! / -ref ...
(eval expr ns)
```

### `--sandbox-imports` CLI flag

Restrict every `(environment …)` call in a script to a fixed set of
import specs. Useful for hosting untrusted scripts:

```sh
crabscheme --sandbox-imports '(rnrs base)' \
           --sandbox-imports '(crab string)' \
           run untrusted.scm
```

Any `(environment '(crab process))` call inside the script returns
an error. Programs not using `(environment …)` are unaffected.

### Known gaps (L1)

- `eval` against a restricted environment **does not yet reject
  unbound identifiers at expand time**. Unbound identifiers raise at
  evaluation time, not expansion time. ADR 0015 calls for the
  expand-time variant; tracked as open.

## L2 — WASM-instance sandboxing

Spawns a fresh wasmtime instance hosting `crabscheme.wasm`. The guest
runs at VM tier (no JIT, no `dlopen` FFI, no host syscalls except
explicit imports). Communication is value-marshalled across the
WASM boundary.

### The 4 builtins

```scheme
(make-wasm-sandbox 'preset)              ; ⇒ sandbox handle
(make-wasm-sandbox 'preset "path.wasm")  ; ⇒ sandbox with custom binary
(sandbox? thing)                          ; ⇒ #t / #f
(sandbox-eval sandbox "(...source...)")  ; ⇒ value
(reset-sandbox! sandbox)                  ; clears guest state, reuses instance
```

### The three threat-model presets

| Preset | Use when | Fuel | Wall clock | Memory | Network |
|---|---|---|---|---|---|
| `'hygiene` | Trusted code, lightweight isolation (e.g. multi-tenant REPL) | Generous | 5 s | 16 MB | Disabled |
| `'plugin` | Third-party code with declared capabilities | Moderate | 2 s | 8 MB | Disabled |
| `'adversarial` | Actively hostile code | Tight | 500 ms | 4 MB | Disabled |

Override individual fields by constructing a `SandboxConfig` in Rust
if the presets don't fit (`crates/cs-sandbox-wasm/src/lib.rs:70+`).

### Example

```scheme
(define sb (make-wasm-sandbox 'adversarial))

(sandbox-eval sb "(+ 2 3)")
; ⇒ 5

(sandbox-eval sb "(let loop () (loop))")
; ⇒ error: fuel exhausted (or wall-clock timeout)

(reset-sandbox! sb)
(sandbox-eval sb "(+ 1 1)")
; ⇒ 2  (fresh state in the same WASM instance)
```

### When to use L2 over L1

- **L1 only**: you trust the code not to be adversarial — you just
  want to keep its imports narrow. Order-of-magnitude faster.
- **L2**: untrusted source (e.g. user-supplied scripts in a server,
  plugin code from third parties, code from an LLM that you haven't
  reviewed).
- **L1 inside L2** (defense-in-depth): the WASM boundary contains
  any host-level exploit; the L1 environment restricts what the
  guest can even reference. Cost is L2's; the L1 check is free.

### Known gaps (L2)

- **No `drop-sandbox!`** — sandbox instances accumulate in a
  process-local thread-local (`SANDBOXES` in
  `crates/cs-runtime/src/builtins/sandbox.rs`). Fine for
  short-lived processes that create 1–3 sandboxes; a memory leak in
  long-lived servers that spawn many sandboxes per request. Tracked.

- **L2 guest tier is VM-only** — no JIT inside the sandbox. This is
  intentional (JIT codegen is a much larger attack surface than VM
  dispatch) but means CPU-heavy untrusted code runs at a 4–48× JIT
  perf gap.

## See also

- [ADR 0015](../adr/0015-sandboxing.md) — full design including
  the WASI capability surface, fuel/epoch semantics, and the
  L1/L2/L3 layering plan.
- `crates/cs-sandbox-wasm/src/lib.rs:70+` — `SandboxConfig` and the
  three presets in Rust.
- `crates/cs-runtime/src/builtins/sandbox.rs` — the L2 Scheme surface.
- `crates/cs-runtime/tests/phase4_sb_iter4_scheme.rs` — Scheme-level
  L2 lifecycle tests (`make-wasm-sandbox` / `sandbox-eval` /
  `reset-sandbox!`).
- `crates/cs-runtime/tests/realworld_sandbox_adversarial.rs` — L2
  adversarial-code exercises (infinite loop, memory bomb, etc.).
- L1 tests: search the test directory for `environment_` (immutable
  snapshot) and `make_namespace_` (mutable namespace).
