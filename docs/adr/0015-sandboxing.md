# ADR 0015 — Sandboxing

**Status:** Proposed
**Date:** 2026-05-18
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

1. **Real environment representation.** Replace the sentinel with
   a structure that carries the import set (library specs to
   resolve at eval time). Likely shape:

   ```rust
   pub struct Environment {
       imports: Vec<LibrarySpec>,
       /// Resolved binding set, computed lazily on first use.
       /// Maps each visible name to the value at construction
       /// time (snapshot semantics — re-defining a name in the
       /// host doesn't affect existing environments).
       resolved: OnceLock<HashMap<Symbol, Value>>,
   }
   ```

2. **`eval` consults the environment.** Today's `eval` runs
   against the global top-level frame. The new path: when given
   a non-sentinel environment, build a fresh `Frame` whose
   bindings are exactly the env's `resolved` map, run the
   expression against that.

3. **Reject undefined references at expand time.** The expander
   already knows which symbols are bound; running it against a
   restricted environment means unknown names produce
   `unbound-identifier` at expand, not runtime — better
   ergonomics, matches Chez behavior.

This is iter-1 of L1. Two iters of follow-up for a complete
implementation:

- Iter 2: composite environments. `(environment '(rnrs base)
  '(my custom lib))` combines two import sets.
- Iter 3: `eval` accepts a constructed environment from a saved
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
    /// Wasmtime fuel — roughly proportional to executed Wasm
    /// instructions. None = unlimited (don't use for adversarial
    /// code). Default Some(10_000_000).
    pub fuel: Option<u64>,
    /// Paths the guest can read/write. Empty = no filesystem.
    pub allow_paths: Vec<PathBuf>,
    /// Whether to grant network capabilities. Default false.
    /// Currently a no-op placeholder; wasi-sockets adoption in
    /// wasmtime is in flux.
    pub allow_network: bool,
    /// Wall-clock timeout for a single sandbox-eval call.
    /// Independent of fuel — covers I/O stalls. Default 30s.
    pub wall_clock_timeout: Duration,
    /// Initial library import-spec the guest's eval will see.
    /// Default: ("(rnrs base)").
    pub imports: Vec<String>,
}

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

Newline-framed text on stdin/stdout, each line a self-describing
record:

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

Why a text protocol on stdin/stdout, not a richer Wasm-component-model
interface? Three reasons:

1. **WASI Preview 1 stability.** Preview 2 + the component model
   are still in flux; Preview 1's stdin/stdout works today and
   the conformance suite already exercises it.
2. **Debuggability.** A text protocol prints in test logs and
   captures cleanly under `wasmtime run`. The guest binary is
   the same one users can run interactively, which makes
   reproducing host-vs-guest behavior trivial.
3. **Zero ABI surface.** Adding richer host↔guest bindings means
   new wasmtime exports + matching imports in the guest; the
   text protocol uses the existing CLI-script-evaluation path
   in crabscheme.wasm and adds no new ABI to maintain.

The cost is round-trip latency — each `eval` is one process-IO
hop. For the use cases sandboxing addresses (running notebook
cells, plugin scripts, untrusted user-submitted programs), that
latency is dominated by the eval itself.

#### Resource semantics

| Resource    | Mechanism                                         | Failure mode                  |
|-------------|---------------------------------------------------|-------------------------------|
| Memory      | `wasmtime::ResourceLimiter` rejects allocs >limit | `MemoryExhausted` error       |
| CPU         | `wasmtime::Engine` fuel-consuming mode            | `FuelExhausted` error         |
| Wall clock  | Host-side `tokio::time::timeout` around the call  | `Timeout` error               |
| Filesystem  | `WasiCtx::preopened_dir` enumeration              | `CapabilityDenied` (WASI)     |
| Network     | Default off; wasi-sockets opt-in (future)         | `CapabilityDenied` placeholder|
| FFI         | `--no-default-features` build has no dlopen       | guest builtin returns error   |

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

## Open questions

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

- [ ] **Phase-4-sb iter 1:** new `cs-sandbox-wasm` crate with
  `SandboxConfig`, `SandboxInstance`, `SandboxError`. Wasmtime
  dep gated behind a feature flag. Spawns crabscheme.wasm at
  init; implements the stdin/stdout text protocol; supports
  basic value round-trip (Fixnum, Flonum, Boolean, String,
  Symbol, list). Tests verify: simple eval succeeds, restricted
  filesystem denial, fuel exhaustion, memory exhaustion.

- [ ] **Phase-4-sb iter 2:** L1 — real environment representation
  + `eval` consulting it. Replace the sentinel; implement
  `(environment '(rnrs base))` returning a real env. Tests:
  unknown identifier in restricted env errors at expand time,
  defined identifier succeeds, `set!` inside an env doesn't
  affect the host top level.

- [ ] **Phase-4-sb iter 3:** L1 layered inside L2 — the
  guest's eval defaults to the SandboxConfig.imports
  environment. Defense-in-depth verification: even when the
  host accidentally grants too-much path access, restricted
  `imports` in the guest prevent `(open-output-file ...)` from
  resolving.

- [ ] **Phase-4-sb iter 4:** Scheme builtins —
  `(make-wasm-sandbox ...)`, `(sandbox-eval s expr)`,
  `(sandbox-config s)`, `(reset-sandbox s)`. Wire to the L2
  crate. Cs-runtime adds `cs-sandbox-wasm` as a feature-gated
  dep.

- [ ] **Phase-4-sb iter 5 (stretch):** preloaded user libraries
  in the sandbox; cross-eval state preservation toggle;
  diagnostic improvements.

- [ ] **Phase-4-sb iter 6 (stretch):** wasi-sockets opt-in for
  network access when wasmtime's Preview 2 sockets land
  stable.

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
