# ADR 0015 — Sandboxing

**Status:** Accepted
**Date:** 2026-05-18 (Proposed); 2026-05-18 (Accepted after Q1-Q6 resolution)
**Context:** R6RS++ Phase 4 (`docs/research/r6rs_extensions_spec.md` §"Phase 4 — Advanced research"); optimizer-plugins arc landed (`docs/milestones/r6rs-extensions-p4-optimizer-plugins-exit.md`)
**Predecessors:** ADR 0007 (JIT architecture); ADR 0014 (optimizer plugins); M10 Track W exit (`docs/milestones/m10-trackW-exit.md`)

## Context

R6RS++ Phase 4 lists "sandboxing" as a deliverable, illustrated by
a single phrase:

> `(import …)` from a sandboxed environment cannot escape

That one phrase is the entire surface the spec gives. This ADR
turns it into a layered design that exploits what we already have
(the WASM target ships and runs under wasmtime — a real
capability-based isolation boundary) rather than re-inventing
in-process isolation from scratch.

### What "sandboxing" plausibly means for crabscheme

Three distinct things in the literature, often conflated:

1. **Namespace isolation.** A piece of code can only see the
   bindings it was given. The classic R6RS `eval` + environment
   pattern. Today's runtime stubs this — every binding is global;
   `eval` ignores its environment argument (see comment at
   `crates/cs-runtime/src/builtins/mod.rs:10186-10191`). Not a
   security boundary — protects against accidents.

2. **Capability isolation.** A piece of code can't perform side
   effects it wasn't authorized for. Most relevant: filesystem,
   network, dlopen, FFI. WASI already gives this for free for
   our WASM build (`--dir=...` controls visible paths, network is
   absent by default, dlopen doesn't exist).

3. **Resource isolation.** A piece of code can't exhaust memory
   or CPU. wasmtime's fuel + memory limits give this for the
   WASM build; in-process Rust execution doesn't have a clean
   answer without thread + custodian machinery (which we don't
   have either).

The Phase 4 spec phrase blurs #1 and #2 — "cannot escape" is
language that reads more like capability isolation than namespace
isolation. The user expectation, based on the `#!lang`-installs-
optimizer-pass example in ADR 0014, is something like Racket's
`racket/sandbox`: a way to run untrusted-or-suspicious Scheme
code without it being able to read the host's filesystem, eat the
host's memory, or hang forever.

### What's already in place

- **`wasm32-wasip1` build works.** M10 Track W shipped: 2.6 MB
  WASM binary; runs full conformance suite under wasmtime to
  match-native rates (99.96% pass, 0pp gap). WASI sandbox
  enforces filesystem boundaries by default (2 conformance
  fixtures fail correctly because they touch unmapped paths).
- **Feature flags exist:** `default = ["jit", "ffi-dynamic"]`;
  `--no-default-features` produces a sandbox-friendly binary
  with no native codegen and no dlopen.
- **Environment-aware `eval` is already deferred work.** The
  runtime carries `(environment ...)` / `(interaction-environment)`
  / `(null-environment 5)` as builtins that return opaque
  sentinels; the design space for real per-environment binding
  filtering is open.

### Prior art

- **R6RS `(rnrs eval)`:** standardized `(eval expr env)` with
  `(environment import-spec ...)` building an environment from
  declared imports. The semantics are language-level — Chez
  enforces them via expand-time visibility, not memory
  isolation. Adversarial code that can reach `(load-shared-object
  ...)` escapes.

- **Racket `racket/sandbox`:** `make-evaluator` builds a
  per-evaluator namespace with custodians (resource ownership
  trees), inspectors (struct-type access control), and
  configurable permission policies. Memory limit via custodian +
  GC; time limit via thread + alarm. Explicitly NOT a security
  boundary against adversarial code per Matthew Flatt's
  documentation — too many language-level escape hatches to
  audit exhaustively.

- **Lua `setfenv` / `_ENV`:** the original "give untrusted code
  a fresh global table" model. Not a security boundary either
  — Lua's debug library has been the source of numerous escape
  CVEs.

- **WebAssembly + WASI:** real capability-based isolation. The
  WASM linear memory model is enforced by the JIT (wasmtime
  validates), so even adversarial WASM code can't read host
  memory outside its sandbox. WASI capabilities are explicit:
  what the embedder doesn't grant, the guest can't access.
  This is the only widely-deployed in-process "actually
  isolated from adversaries" story for non-native code.

### Why WASM specifically

We already build a `crabscheme.wasm` and run it under wasmtime
end-to-end. The "internal sandboxing mechanism" the user asks
for has a clean shape: when the host program wants to evaluate
untrusted Scheme code, instantiate a wasmtime `Engine` +
`Module` of the crabscheme runtime itself, hand it the untrusted
source plus a constrained WASI environment, and consume its
results through a narrow interface. The host stays on native
crabscheme; the guest runs inside a real sandbox.

This is unusual: a Scheme implementation using itself-compiled-
to-WASM as its own sandbox engine. The alternative would be to
build a separate, smaller Scheme interpreter just for sandboxed
code (Racket's sandbox is roughly this — a stripped-down
namespace) — but we already paid the cost of making the runtime
WASM-portable, and reusing it has the side benefit that
sandboxed code has the same language semantics as host code.

## Decision

**Ship sandboxing in two layers, the second built on the first:**

1. **Layer L1 — Namespace-restricted in-process `eval`.** The
   foundational R6RS surface: real per-environment binding
   filtering, real `(environment '(rnrs base))` that produces an
   environment containing only the named library's exports,
   `eval` that actually consults its environment argument. This
   is namespace isolation — NOT a security boundary against
   adversarial code, but the foundation everything else builds
   on, AND a real ergonomic win for plugin authors who want to
   limit what their guests can see.

2. **Layer L2 — WASM-instance sandbox.** A real isolation
   boundary built on wasmtime + WASI. The host crabscheme spawns
   a wasmtime `Instance` of the no-default-features
   `crabscheme.wasm` binary, hands it the source to evaluate,
   constrains its WASI capabilities (no filesystem unless
   explicitly mapped, no network ever, configurable memory + fuel
   limits), and returns the result through a stdin/stdout
   protocol. This IS a security boundary; adversarial Scheme
   inside the sandbox cannot read host memory, write host files,
   or spin forever (fuel runs out).

The two layers serve different threat models:
- **L1** for "I want to limit accidental scope leak — what
  bindings can this evaluated expression see?" Adversaries can
  escape via `(load-shared-object ...)` or other capability
  builtins; this layer's job is hygiene, not security.
- **L2** for "I want to run code I don't fully trust — at most
  it can return a value to me." Adversaries face a real WASM
  memory boundary and a WASI capability barrier.

User-facing surface, top down:

```scheme
;; Layer 1 — namespace-restricted eval
(define env
  (environment '(rnrs base) '(rnrs lists)))  ;; only these
(eval '(map car '((1 2) (3 4))) env)        ;; -> '(1 3)
(eval '(open-output-file "/etc/passwd") env) ;; -> unbound; rejected at expand time

;; Layer 2 — capability-isolated sandbox
(define s
  (make-wasm-sandbox
    #:memory-limit (* 64 1024 1024)   ; 64 MiB
    #:fuel 1000000                    ; ~instructions
    #:allow-paths '()                 ; no filesystem
    #:env '(rnrs base)))              ; namespace inside the sandbox
(sandbox-eval s '(+ 1 2 3))           ; -> 6
(sandbox-eval s '(open-output-file "/etc/passwd"))
                                       ; -> sandbox-violation condition raised in host
(sandbox-eval s '(let loop () (loop))) ; -> sandbox-fuel-exhausted condition
```

L2's `make-wasm-sandbox` takes the same `import-spec` list as L1's
`environment` so the user can layer them naturally: "this is a
sandboxed evaluator that only sees (rnrs base) inside its own
WASM instance."

## Detailed design

### Layer 1 — namespace-restricted `eval`

**Already partial.** `environment` / `interaction-environment` /
`null-environment` are builtins returning sentinels. The work for
L1 is:

L1 ships TWO clearly-named constructors with different semantics —
the user's Q4 decision (see Resolution section). Both implement
namespace restriction; they differ in mutation semantics:

1. **`(environment '(rnrs base) ...)` — R6RS strict snapshot
   (Chez behavior, R6RS §15.2).** Returns an **immutable**
   environment. The binding set is resolved at construction time;
   subsequent changes to the host's libraries don't affect it;
   `(eval '(set! x 1) env)` raises `&assertion`. Best for
   reasoning about sandboxed code; prevents subtle aliasing bugs.

2. **`(make-namespace '(rnrs base) ...)` — Racket-style live
   namespace (NEW form).** Returns a **mutable container**. The
   host can `namespace-set-variable-value!` after construction;
   mutations are visible to code holding the namespace. Better
   for interactive REPL workflows where iterative redefinition
   is the point.

Internally both are represented similarly; the difference is the
`mutable: bool` flag on the inner struct and which builtins are
allowed against it:

   ```rust
   pub struct Environment {
       imports: Vec<LibrarySpec>,
       /// Resolved binding set. For `environment`, populated once
       /// at construction (snapshot semantics). For
       /// `make-namespace`, mutable Cell across host operations.
       bindings: RefCell<HashMap<Symbol, Value>>,
       mutable: bool,
   }
   ```

The `eval` builtin consults the environment regardless of
mutability — it just constructs a fresh `Frame` from `bindings`
and runs the expression there. Mutation primitives
(`namespace-set-variable-value!`, etc.) check `mutable` and raise
`&assertion` when false.

Reject undefined references at expand time: the expander already
knows which symbols are bound; running it against a restricted
environment means unknown names produce `unbound-identifier` at
expand, not runtime — better ergonomics, matches Chez behavior.

L1 rollout (sub-iters):
- L1.1: `(environment ...)` immutable snapshot (R6RS strict).
- L1.2: `(make-namespace ...)` mutable variant +
  `namespace-set-variable-value!` / `namespace-undefine-variable!`.
- L1.3: composite construction. `(environment '(rnrs base) '(my
  custom lib))` combines two import sets.
- L1.4: `eval` accepts a constructed environment from a saved
  source location (the "first-class environment" R6RS pattern).

### Layer 2 — WASM-instance sandbox

#### Architecture

```
┌──────────────────── HOST PROCESS (native crabscheme) ────────────────────┐
│                                                                          │
│  (sandbox-eval s expr)                                                   │
│            │                                                             │
│            ▼                                                             │
│  ┌─────────────────────────┐    ┌───────────────────────────────────┐    │
│  │  cs-sandbox-wasm crate  │ ── │ wasmtime::{Engine,Module,Linker}  │    │
│  │  - SandboxConfig        │    │                                   │    │
│  │  - SandboxInstance      │    │  WASI ctx with constrained:       │    │
│  │  - encode/decode Value  │    │   - preopened dirs (none default) │    │
│  └─────────────────────────┘    │   - stdin: serialized expr        │    │
│            │                    │   - stdout: serialized result     │    │
│            │                    │   - memory limit (MemoryConfig)   │    │
│            │                    │   - fuel (engine config)          │    │
│            ▼                    └───────────────────────────────────┘    │
│  ┌───────────────────── GUEST INSTANCE (crabscheme.wasm) ────────────┐   │
│  │                                                                   │   │
│  │  Reads expr from stdin                                            │   │
│  │  Parses + expands + evals under restricted env                    │   │
│  │  Serializes result to stdout                                      │   │
│  │  Or: serializes error condition to stderr                         │   │
│  │                                                                   │   │
│  └───────────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────────┘
```

The host never shares memory with the guest — they communicate via
a serialization protocol on stdin/stdout. The guest runs the
full crabscheme.wasm binary, just configured with a restricted
WASI context.

#### Crate boundary: `cs-sandbox-wasm`

New optional crate (feature-gated; default off for non-sandbox
embedders).

```rust
pub struct SandboxConfig {
    /// Max linear memory the guest can allocate (bytes). Default 64 MiB.
    pub memory_limit: usize,
    /// Wasmtime fuel — roughly 1 unit per executed Wasm
    /// instruction. None = unlimited (don't use for adversarial
    /// code). Default Some(10_000_000). Deterministic; same
    /// program hits the same trap point regardless of host load.
    pub fuel: Option<u64>,
    /// Cheaper alternative to fuel: epoch interruption. Host
    /// ticks an epoch counter at this interval; guest traps when
    /// its store's epoch deadline expires. Non-deterministic but
    /// much lower per-instruction overhead. None = disabled.
    /// Mutually exclusive with `fuel` in the default config —
    /// pick one. Default None (fuel is the default CPU bound).
    pub epoch_tick_interval: Option<Duration>,
    /// Paths the guest can read/write. Empty = no filesystem.
    pub allow_paths: Vec<PathBuf>,
    /// Whether to grant network capabilities (wasi-sockets, WASI
    /// 0.2). Default false. Lands working in iter 1 (research
    /// log Q2 — wasi-sockets is in the stable WASI 0.2 surface).
    pub allow_network: bool,
    /// Wall-clock timeout for a single sandbox-eval call.
    /// Independent of fuel — covers I/O stalls. Default 30s.
    pub wall_clock_timeout: Duration,
    /// Initial library import-spec the guest's eval will see.
    /// Default: ("(rnrs base)").
    pub imports: Vec<String>,
    /// Whether to reuse the same wasmtime Instance across
    /// multiple sandbox-eval calls on this SandboxInstance.
    /// - true: REPL-like; bindings/state persist across calls;
    ///   faster per-call. Recommended for friendly threat
    ///   models.
    /// - false: each eval spawns a fresh wasmtime Instance;
    ///   no cross-call state. Slower per-call but strongest
    ///   isolation. Recommended for adversarial use cases.
    /// Default depends on construction site: the spec-driven
    /// defaults below pick the safe value per use case rather
    /// than forcing one global default.
    pub reuse_instance: bool,
}
```

**Defaults per threat-model preset.** Most users don't write a
`SandboxConfig` from scratch; the crate exposes three named
presets matching the user's threat-model decision (see Resolution
section, Q1):

```rust
impl SandboxConfig {
    /// Hygiene-only L2 wrapper. Friendly code; cross-call state
    /// preserved (REPL feel). reuse_instance=true, fuel=None,
    /// 5min wall-clock, allow_paths empty, allow_network=false.
    pub fn hygiene() -> Self;

    /// Plugin / extension use case. Untrusted-by-default but
    /// long-running. reuse_instance=true, fuel=Some(100M),
    /// 30s wall-clock, allow_paths empty, allow_network=false.
    pub fn plugin() -> Self;

    /// Adversarial / per-eval-fresh use case (code playground,
    /// untrusted user submission). reuse_instance=false,
    /// fuel=Some(10M), 5s wall-clock, allow_paths empty,
    /// allow_network=false.
    pub fn adversarial() -> Self;
}
```

Users start from a preset and override specific fields.

pub struct SandboxInstance { /* wasmtime guts */ }

impl SandboxInstance {
    pub fn new(config: SandboxConfig) -> Result<Self, SandboxError>;
    pub fn eval(&mut self, expr: &Value, syms: &SymbolTable)
        -> Result<Value, SandboxError>;
}

#[derive(Debug)]
pub enum SandboxError {
    /// The guest's eval raised a condition.
    GuestRaised(SerializedCondition),
    /// Fuel exhausted before the eval completed.
    FuelExhausted,
    /// Wall-clock timeout exceeded.
    Timeout,
    /// Memory limit exceeded.
    MemoryExhausted,
    /// Guest tried to access a path/capability not granted.
    CapabilityDenied(String),
    /// Serialization mismatch — shouldn't happen with versioned
    /// protocol but caught defensively.
    ProtocolError(String),
    /// Wasmtime/runtime internal failure (corrupted binary,
    /// linker mismatch, etc.).
    Internal(String),
}
```

#### Wire protocol (host ↔ guest)

**Iter 1 ships the text protocol; iter 5 evaluates migrating to
the WASM component model.** Reasoning recorded under Q1 below.

Iter 1 wire format: newline-framed text on stdin/stdout, each
line a self-describing record:

```
> EVAL <length>
> <s-expression source for length bytes>
< OK <length>
< <s-expression of result>
```

Or on error:
```
< ERR <kind> <length>
< <s-expression of condition record>
```

`<kind>` is one of `raised`, `fuel`, `memory`, `capability`,
`internal`. The host's `SandboxError` mirrors the wire kinds.

Why the text protocol initially:

1. **Debuggability.** A text protocol prints in test logs and
   captures cleanly under `wasmtime run`. The guest binary is
   the same one users can run interactively, which makes
   reproducing host-vs-guest behavior trivial.
2. **Zero ABI surface.** Adding richer host↔guest bindings means
   new wasmtime exports + matching imports in the guest; the
   text protocol uses the existing CLI-script-evaluation path
   in crabscheme.wasm and adds no new ABI to maintain.
3. **Defers component-model commitment until after iter 4** when
   we have a working baseline to compare against.

The cost is round-trip latency — each `eval` is one process-IO
hop. For the use cases sandboxing addresses (running notebook
cells, plugin scripts, untrusted user-submitted programs), that
latency is dominated by the eval itself.

After iter 4 (Scheme builtins shipped), iter 5 evaluates whether
to migrate to component-model bindings using `wasmtime::component::Component`
and WIT-defined imports/exports. WASI 0.2 + components are stable
on the wasmtime 36.x LTS line as of 2026 (research log Q2); the
migration would replace the text protocol with typed function
calls but otherwise preserves the architecture. Decision deferred
until iter-4 perf numbers tell us whether the text-protocol RTT is
actually a bottleneck.

#### Resource semantics

| Resource    | Mechanism                                                  | Failure mode                 |
|-------------|------------------------------------------------------------|------------------------------|
| Memory      | `wasmtime::ResourceLimiter` impl via `Store::limiter(fn)`  | `MemoryExhausted` error      |
| CPU (det.)  | `Config::consume_fuel(true)` + `Store::set_fuel(N)`        | `FuelExhausted` error        |
| CPU (cheap) | `Config::epoch_interruption(true)` + epoch deadline + host ticker | `Timeout` error       |
| Wall clock  | Host-side `tokio::time::timeout` around the call           | `Timeout` error              |
| Filesystem  | `WasiCtx::preopened_dir` enumeration (WASI 0.2)            | `CapabilityDenied` (WASI)    |
| Network     | wasi-sockets opt-in via `SandboxConfig.allow_network`      | `CapabilityDenied` (WASI)    |
| FFI         | `--no-default-features` build has no dlopen                | guest builtin returns error  |

**Fuel vs epoch interruption:** fuel is deterministic — same program
hits the same trap point regardless of host load — but counts every
WASM instruction (per-instruction host overhead). Epoch interruption
is much cheaper (host increments an epoch counter periodically; the
guest checks it at trap-safe points) but non-deterministic (depends
on epoch tick rate vs guest execution speed). `SandboxConfig`
exposes both; the default config enables fuel for reproducibility,
recommends epoch interruption for production where the cheaper-
per-instruction cost matters and determinism doesn't.

**Wasmtime version pin:** wasmtime adopted an LTS policy in 2025.
The cs-sandbox-wasm crate pins to the **36.x LTS line** (24-month
support, modern component-model APIs available when iter-5
migration lands). Patch updates within the line are guaranteed
API-compatible; major-version moves are explicit per-release
decisions.

**WASI 0.2 readiness:** unlike the original ADR draft, WASI 0.2 +
the component model are stable as of early 2026 (research log
Q2). `wasmtime::component::Component` is treated as stable on
the 36.x LTS line; wasi-sockets is part of the stable surface.
The ADR's "allow_network is a no-op placeholder" caveat is
**removed**: iter-1 ships `allow_network` working via wasi-sockets
when wasmtime is configured with the appropriate Linker bindings,
defaulting off.

Memory limit and fuel are enforced by wasmtime itself — they
can't be bypassed by guest Scheme code because guest Scheme
runs inside guest WASM, and wasmtime is what observes WASM
instruction execution. Filesystem isolation is WASI's job; the
guest can call `(open-output-file ...)` but WASI rejects when
the path isn't in `preopened_dir`.

### How the layers compose

Inside the L2 WASM sandbox, the guest runs the same crabscheme
runtime. The guest's `eval` should default to L1 namespace
restriction matching `SandboxConfig.imports` — defense in depth.
Even if the host accidentally maps a sensitive path, the guest's
restricted environment makes `(open-output-file ...)` an unbound
identifier.

The user-facing `sandbox-eval` plumbs both layers automatically:
the host's L2 instance is constructed once with the desired
imports; per-call evals run against that L1 environment inside
that L2 instance. The two-layer composition is invisible to the
user.

## Consequences

### Positive

- **A real security boundary becomes available** without
  inventing custodian-or-thread isolation machinery from scratch.
  WASM + WASI is the only widely-deployed in-process isolation
  model that holds against adversarial guest code.
- **The existing WASM target earns dual use.** crabscheme.wasm
  was originally built for portability (run anywhere wasmtime
  runs); sandboxing reuses it for isolation. Same binary, two
  consumer stories.
- **L1 is independently useful even without L2.** A REPL
  embedded in a editor wants namespace-restricted eval for
  user-typed expressions; "you have access to these libraries
  and no others" is the natural sandbox UI for non-adversarial
  contexts.
- **The two-layer composition is the right defense-in-depth
  story.** L1 inside L2 means an adversary has to break both
  the namespace restriction AND the WASM/WASI boundary.
- **Doesn't disturb any existing perf gates** (ADR 0013).
  Sandboxing is opt-in; non-sandbox runtime paths are untouched.

### Negative / cost

- **Wasmtime is a heavy dependency.** Pulling it into a new
  optional crate adds compile time (~30s on a warm build) and
  binary size when enabled. Feature-gating keeps non-sandbox
  builds clean.
- **L2 round-trip latency.** Each `sandbox-eval` is a process-
  IO call (~100µs base overhead). Acceptable for the threat
  model (rare untrusted-code evaluation), unsuitable for fine-
  grained "every function call goes through the sandbox."
- **Operational complexity.** Two failure modes per sandbox
  call (guest condition vs. resource exhaustion vs. capability
  denial). The error mapping is straightforward but adds
  surface for users to understand.
- **WASI Preview 1 limitations.** No sockets (Preview 2's still
  unstable). For now `allow_network` is documented but is a
  no-op. When wasi-sockets stabilizes (or when we move to
  Preview 2 for other reasons), it lands.
- **Crash inside guest.** A wasmtime trap doesn't kill the
  host process (this is the whole point), but it does fail the
  in-flight eval cleanly. Documented as `SandboxError::Internal`
  — distinguishable from guest-level Scheme errors.

### Neutral

- **cs-typer, cs-opt, cs-typer's typer-hints don't apply inside
  the L2 guest** (because the guest is the no-default-features
  build with no JIT). Sandboxed code runs at bytecode-VM tier.
  This is a feature, not a bug: the JIT is the largest unaudited
  attack surface in the runtime; running guest code at the
  VM tier eliminates that class of risk.

## Alternatives considered

### Alt A — Subprocess crabscheme native binary
Run guest as a child process; communicate over pipes; resource
limits via `setrlimit` / `prlimit`.

**Rejected.** Same conceptual model as the WASM-sandbox approach
but with weaker isolation. `setrlimit` is per-process not per-
"sandbox-eval"; one runaway eval kills all evaluations in that
subprocess. WASM-per-instance gives true per-eval resource
budgeting. Also: native crabscheme has JIT and dlopen by default,
giving the adversary primitives to escape; the no-default-
features WASM binary doesn't.

### Alt B — In-process Rust thread with custodian
Like racket/sandbox but in Rust: spawn a tokio task with a
private namespace, cancel on timeout, account memory against a
per-task allocator.

**Rejected.** Doesn't survive contact with malicious guest. A
Scheme expression has access to the entire runtime's Rust
allocator unless we run a fresh sub-runtime, and a fresh sub-
runtime still shares the host's address space — `Value`s flow
freely. The R6RS+ macros / FFI / dlopen surface is too large to
audit for escape primitives. Acceptable for non-adversarial
contexts but doesn't solve the problem; doing it instead of L2
means we ship a sandbox that's a security boundary in name only.

### Alt C — Embed a separate restricted Scheme interpreter
Build a small bytecode interpreter just for sandboxed code.
Restrict its builtins to a hand-audited safe subset.

**Rejected.** Code-duplication overhead — every R6RS feature
needs reimplementation in the sandbox interpreter, and the
language has to be a strict subset that user code may not be
able to use freely. We'd be re-inventing the runtime to get
isolation we can already buy from WASM.

### Alt D — L1 only; no real-isolation layer
Ship namespace-restricted eval, document that "sandboxing" is
hygiene-only.

**Rejected as primary; accepted as L1 component.** Shipping JUST
L1 means the spec deliverable is technically present but the
spec phrase ("cannot escape") is misleading — L1 doesn't
constrain capabilities. Either ship both layers and let users
choose, or rename the deliverable to "namespace restriction"
and don't claim sandboxing.

## Resolution of open questions

The original "Open questions" section captured 6 design questions
flagged at Proposed time. Subsequent research + user decisions
resolved them as follows:

### Threat-model (interview Q1) — both, tiered
Ship BOTH layers. L1 for the common case (cheap, in-process); L2
for the marked-untrusted path (opt-in, real isolation). The
SandboxConfig presets (`hygiene` / `plugin` / `adversarial`)
match the three concrete use cases this targets.

### Use cases driving this (interview Q2) — REPL/notebooks + plugins + untrusted user code
All three are in scope. REPL/notebook drives the `hygiene` /
persistent-instance preset. Plugin marketplace drives the
`plugin` preset with reasonable fuel limits. Untrusted code (web
playground, judge-like CI) drives the `adversarial` preset with
fresh-per-eval isolation. All three flow through the same
`make-wasm-sandbox` constructor with different presets.

### L1 binding semantics (interview Q3) — both, separate constructors
Ship two constructors:
- `(environment '(rnrs base) ...)` — R6RS strict immutable
  snapshot (Chez behavior). `(eval '(set! x 1) env)` raises
  `&assertion`.
- `(make-namespace '(rnrs base) ...)` — Racket-style live mutable
  container. `namespace-set-variable-value!` etc. work.

Both delegate to the same internal `Environment` struct; the
difference is a `mutable: bool` flag + which builtins are
allowed against it. Cost of both is one extra builtin
constructor; benefit is users get the semantic they expect
without confusion.

### Instance reuse (interview Q4) — user chooses via config flag
`SandboxConfig.reuse_instance: bool` field; per-preset defaults
documented above. No single global default — each preset picks the
safe value for its threat model.

### Wasmtime version (research Q1) — pin 36.x LTS
Wasmtime adopted an LTS policy in 2025; 36.x is the modern LTS
line with 24-month support and component-model APIs available.
The cs-sandbox-wasm crate pins to `wasmtime = "36"` in its
Cargo.toml; patch updates within 36.x are guaranteed
API-compatible. Major-version migrations are explicit per-release
decisions.

### WASI Preview 2 + component model (research Q2) — stable, defer migration to iter 5
WASI 0.2 + component model are stable on the 36.x LTS line as of
early 2026. The original ADR rejected component-model bindings
"because Preview 2 is in flux"; that rationale is obsolete.
However, iter 1 still ships the text-protocol design because (a)
the same code path is exercised by interactive `wasmtime run`,
giving debugging parity, and (b) it doesn't commit to a specific
WIT-typed ABI before we have perf data on whether the protocol
RTT is actually a bottleneck. **Iter 5 evaluates migrating to
component bindings** based on iter-4 measurements.

### Fuel vs epoch interruption (research Q3) — both available; fuel is default
Both `Config::consume_fuel` (deterministic per-instruction) and
`Config::epoch_interruption` (cheap non-deterministic) are
exposed via `SandboxConfig.fuel` and `SandboxConfig.epoch_tick_interval`.
The default preset configs use fuel for reproducibility; embedders
who care about per-instruction cost more than determinism can
switch to epoch interruption.

### Wasi-sockets (was: future iter) — available in iter 1
wasi-sockets is part of the stable WASI 0.2 surface; the original
ADR's "no-op placeholder" caveat is removed. Iter 1 ships
`SandboxConfig.allow_network` working via the wasmtime Linker's
wasi-sockets bindings, defaulting off.

### Layer-1 binding snapshot semantics (was: open) — answered by interview Q3
Resolved by shipping both constructors. `environment` snapshots;
`make-namespace` lives. Behavior is opt-in per call site.

## Open questions (residual)

1. **Cross-eval continuation handles for plugin REPLs.** When
   `reuse_instance=true`, can the guest expose call-cc'd
   continuations across multiple eval calls? Probably yes (the
   guest stays alive between calls), but the host has no way to
   serialize / address them. Defer until a plugin actually wants
   this; document the limit ("continuations are intra-eval
   only") in the iter-4 builtin docs.

2. **Component-model migration go/no-go.** Iter 5's decision
   depends on iter-4 measurements: text-protocol RTT vs
   component-call RTT on representative workloads. Pre-commit
   to nothing; the iter-5 outcome may keep the text protocol if
   the perf delta is small enough.

3. **Preloaded user libraries in the sandbox.** Mentioned in the
   original Open Questions; still tracked as iter 5 follow-up.
   The shape (`SandboxConfig.preload: Vec<(name, source)>`)
   stays as written.

1. **WASM-component-model migration.** Preview 2 + components
   would let us replace the stdin/stdout protocol with typed
   imports/exports. Cleaner ABI; better debugging hooks; cost is
   a wasmtime-version pin and an ongoing-instability risk. Not
   blocking iter 1; revisit when Preview 2 stabilizes upstream.

2. **Module caching across evals.** Spinning up a new WASM
   `Instance` for every `sandbox-eval` call is wasteful when
   evaluating many expressions in the same sandbox. The natural
   answer is `SandboxInstance::eval` reuses the same `Instance`
   across calls — but that means the guest sees state from
   previous calls. Spec the sharing semantics explicitly:
   `make-wasm-sandbox` returns a long-lived instance; explicit
   `reset-sandbox` rebuilds it. Default = reuse.

3. **Pre-loaded libraries in the sandbox.** The user might want
   the guest to have access to a custom library
   (e.g., the host's domain-specific helpers). That's a
   `SandboxConfig.preload: Vec<(String, String)>` carrying
   (library-name, library-source) — preloaded into the guest at
   `Instance` creation. Tracked but not in iter 1.

4. **Wasmtime version pinning.** Wasmtime breaks API frequently
   between major releases. Pin to a known-good minor version in
   the new crate's `Cargo.toml`; upgrade explicitly per release.

5. **`SandboxError::Timeout` vs `FuelExhausted` disambiguation.**
   Fuel exhaustion is deterministic (same program → same point);
   wall-clock timeout is non-deterministic (depends on host
   load). Tests should prefer fuel limits to avoid flakes; the
   default config exposes both so users can mix.

6. **Layer-1 binding snapshot semantics.** When `environment`
   resolves imports to a `HashMap<Symbol, Value>`, it captures
   the current value of each binding. A subsequent `(set! foo
   ...)` in the host doesn't affect the env's `foo`. Documented
   in the design — matches R6RS semantics for first-class
   environments (Chez does the same).

## Action items

Per-iter scope updated to reflect the Q1-Q6 resolutions above.

- [ ] **Phase-4-sb iter 1:** new `cs-sandbox-wasm` crate. Pins
  `wasmtime = "36"`. Ships `SandboxConfig` with all fields from
  the Detailed Design table (including `epoch_tick_interval`,
  `allow_network`, `reuse_instance`); the three presets
  (`hygiene`, `plugin`, `adversarial`); `SandboxInstance` and
  `SandboxError`. Implements the stdin/stdout text protocol;
  supports basic value round-trip (Fixnum, Flonum, Boolean,
  String, Symbol, list). Tests verify: simple eval succeeds,
  restricted filesystem denial, fuel exhaustion, memory
  exhaustion, fresh-vs-reuse semantics across two evals.

- [ ] **Phase-4-sb iter 2 — L1.1:** `(environment '(rnrs base))`
  immutable snapshot (R6RS strict). Replace the sentinel
  `Value::Symbol("__top-level-env__")` with a real
  `Environment` struct; `eval` constructs a `Frame` from its
  bindings. Tests: unknown identifier errors at expand time,
  defined identifier succeeds, `(eval '(set! x 1) env)` raises
  `&assertion`.

- [ ] **Phase-4-sb iter 3 — L1.2:** `(make-namespace '(rnrs
  base))` mutable variant + `namespace-set-variable-value!` /
  `namespace-undefine-variable!`. Same `Environment` struct
  with `mutable: true`. Tests: mutate via builtin succeeds,
  mutation is visible to subsequent `eval` against the same
  namespace, snapshot env from iter 2 still errors on `set!`.

- [ ] **Phase-4-sb iter 4 — Scheme builtins for L2:**
  `(make-wasm-sandbox preset ...)`, `(sandbox-eval s expr)`,
  `(sandbox-config s)`, `(reset-sandbox s)`. cs-runtime adds
  `cs-sandbox-wasm` as a feature-gated dep. Tests: eval
  through the sandbox returns correct values; `adversarial`
  preset's fuel exhaustion fires; `plugin` preset's wall-clock
  fires; hygiene preset preserves state across two evals.

- [ ] **Phase-4-sb iter 5 — L1 inside L2 (defense-in-depth):**
  guest's eval defaults to the SandboxConfig.imports
  environment (using iter 2's `environment`). Defense-in-depth
  verification test: even when the host accidentally grants
  too-much path access, restricted `imports` in the guest
  prevent `(open-output-file ...)` from resolving at expand
  time. Both `environment` and `make-namespace` are usable
  inside the sandboxed guest (per L1.1/L1.2 surface).

- [ ] **Phase-4-sb iter 6 — measurements:** text-protocol RTT vs
  hypothetical component-call RTT on representative workloads
  (1k-eval REPL loop, single-eval cold start, big-result
  round-trip). Decide whether iter 7's component-model
  migration is worth it. If yes, iter 7 ships the WIT bindings;
  if no, document the decision in this ADR and call iter 6
  the close of the work.

- [ ] **Phase-4-sb iter 7 (conditional):** migrate to wasmtime
  component-model bindings if iter-6 measurements show
  text-protocol RTT is a bottleneck. Replace stdin/stdout
  framing with WIT-typed function calls; preserve the
  SandboxConfig + SandboxError surface unchanged.

- [ ] **Phase-4-sb iter 8 (stretch):** preloaded user libraries
  in the sandbox (`SandboxConfig.preload: Vec<(name,
  source)>`); cross-eval continuation handles (currently
  intra-eval only); diagnostic improvements.

## References

- `docs/research/r6rs_extensions_spec.md` — §"Phase 4" rollout
  table (single-sentence sandboxing deliverable) + §"Risks" entry
  on Racket reader-state bugs
- `docs/milestones/m10-trackW-exit.md` — WASM target ships at
  match-native conformance; sandboxing dual-uses this
- `docs/milestones/r6rs-extensions-p4-optimizer-plugins-exit.md`
  — sibling Phase 4 deliverable; same ADR-driven rollout shape
- `crates/cs-runtime/src/builtins/mod.rs:10186-10191` — R6RS
  environment stubs (the work this ADR makes concrete)
- ADR 0007 — JIT architecture (the JIT is what we deliberately
  exclude from the L2 guest)
- ADR 0014 — Optimizer plugins (companion Phase 4 ADR; this one
  follows the same Pass/Registry/Pipeline structure for
  consistency)
- Racket `racket/sandbox` documentation — prior art for
  language-level sandboxing + explicit non-security framing
- WASI Preview 1 specification — the capability model we lean on
- Wasmtime fuel and resource-limit docs — primary references
  for L2 implementation
- Wasmtime LTS policy
  (https://bytecodealliance.org/articles/wasmtime-lts) —
  rationale for the 36.x pin
- WASI 0.2 status
  (https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)
  — confirmation that component model + wasi-sockets are stable
- R6RS §15.2 environment semantics
  (https://www.r6rs.org/final/html/r6rs-lib/r6rs-lib-Z-H-17.html)
  — basis for the `environment` constructor's immutable
  snapshot behavior
- Racket namespace docs
  (https://docs.racket-lang.org/reference/Namespaces.html) —
  basis for the `make-namespace` constructor's live-mutable
  behavior
