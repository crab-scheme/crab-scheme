# ADR 0008 — Rust FFI Design

> Status: **Accepted** for M5b.
> Companion: `.spec-workflow/specs/ffi/{requirements,design}.md`.
> Predecessor ADRs: 0006 (GC), 0007 (JIT).

## Context

CrabScheme needs a way for Scheme programs to call Rust code (and
vice versa). The roadmap puts this at M5b — a peer of M6 / M7 that
can ship in parallel since FFI doesn't depend on JIT.

Two distinct use cases drive the design:

1. **Embed CrabScheme in a Rust app**: dominant in scripting /
   game / config-DSL contexts. The host registers Rust procedures;
   Scheme code calls them.
2. **Call Rust libraries from Scheme programs**: serde, regex, hyper
   — useful for production Scheme programs that want existing Rust
   ecosystem code.

Plus a third bonus case: **author CrabScheme builtins as external
crates** rather than in-tree.

## Decisions

### D-1. Compile-time registration first; runtime dlopen second

`cs-runtime::Runtime::register_host_procedure(...)` is the primary
path. Build your Rust crate against `cs-runtime`, register your
functions in `Runtime::new()` or shortly after. No dynamic
linking, no shared library, no version negotiation.

Runtime dlopen via `(load-shared-library "path")` lands as a
follow-up after the compile-time path is solid.

**Rationale:**
- Compile-time is simpler, faster (direct function calls), more
  type-safe.
- Most embedded use cases don't need dynamic loading; the host
  knows at build time which procedures it's exposing.
- Dynamic loading needs versioned ABI (D-2), error handling for
  load failures, platform-specific .so / .dylib / .dll handling.
  Big surface area; defer until the simple path is proven.

**Alternatives considered:**
- **Runtime dlopen first**: matches Chez / Racket / Guile but
  pushes ABI complexity into the first iter. Out.
- **Both at once**: doable, doubles the iter count. Out.

### D-2. Stable C-ABI with versioned function-pointer table for dynamic linking

When dynamic linking does land (per D-1's "follow-up"), the .dylib
sees the runtime through a versioned C-ABI struct
(`RuntimeFfi`). The struct's first field is `api_version: u32`;
breaking changes bump the version; the .dylib's
`crabscheme_register` entry point checks the version and refuses
to load on mismatch.

**Rationale:**
- Rust ABI is unstable. C-ABI is the lingua franca.
- Versioned struct lets the runtime evolve without invalidating
  every shared library in the wild.
- Matches Postgres extensions, Vim plugins, dlsym-based plugin
  systems generally.

**Alternatives considered:**
- **Rust-only inline**: forces every plugin to track exact
  rustc versions. Out.
- **WASM as the plugin format**: M10 territory; we'll have a
  WASM target by then. Out for now.

### D-3. `Pinned<'rt>` RAII rooting via `&mut Runtime` borrow

When Rust code holds a Scheme `Value` across a Scheme-level
operation that may GC, it must root via `Runtime::pin(value)`.
The returned `Pinned<'rt>` holds the runtime borrow; on drop, it
unroots.

**Rationale:**
- Compiler enforces single-threaded access through the borrow.
- RAII matches Rust's idiom — no manual unroot calls.
- Handles cross-FFI-call retention naturally: store the
  `Pinned` in a Rust struct that owns the lifetime.

**Alternatives considered:**
- **Manual `add_root` / `remove_root`**: error-prone; users
  forget to unroot.
- **Rooted reference type without runtime borrow**: needs a
  shared-mutability story we don't have without `Mutex`/`RefCell`
  on `Runtime`. Out for M5b; revisit if multithreading lands.

### D-4. `FromValue` / `IntoValue` traits for type marshaling

User functions take and return idiomatic Rust types (`i64`,
`String`, `Vec<T>`, `Option<T>`, etc.). The `#[host_proc]` macro
inserts the marshaling: each arg goes through `FromValue::from_value`,
the return value through `IntoValue::into_value`.

**Rationale:**
- Hides the Scheme `Value` type from the user-facing function
  signature; the function body is plain Rust.
- Standard Rust pattern; serde users will recognize the shape.
- Easy to extend: user types implement the two traits manually
  or via a future derive macro.

**Alternatives considered:**
- **Always pass `&[Value]`**: more flexible but requires every
  user function to manually demarshal. Tedious.
- **Code-generate per-signature wrappers without traits**: more
  efficient but worse error messages and harder to extend. Out.

### D-5. Errors propagate as catchable Scheme conditions

Rust panics, type mismatches, arity errors, and host-failure errors
all surface as Scheme conditions that `with-exception-handler` can
catch. Specifically:

- `FfiError::TypeMismatch` → condition with `&type-error` simple
- `FfiError::ArityError` → standard arity error
- `FfiError::Panic(msg)` → `&error` with the panic message
- `FfiError::HostFailure(msg)` → `&error` with the host's message

**Rationale:**
- Scheme code shouldn't have to know whether an error originated
  in Scheme, in a Scheme builtin, or in Rust FFI — the handler
  protocol is the same.
- Panics escaping the FFI boundary would abort the runtime; we
  catch them via `catch_unwind` and translate.

**Alternatives considered:**
- **Abort on panic**: fast but unsafe; one buggy plugin nukes
  the whole runtime. Out.
- **Custom condition hierarchy for FFI**: adds complexity for
  marginal benefit; existing `&error` / `&type-error` are
  sufficient.

### D-6. `cs-ffi` is the public FFI crate; `cs-ffi-macros` ships separately

Two crates:
- `cs-ffi` — pure Rust types (traits, error, `Pinned`, ABI struct).
  Plugin authors depend on this only.
- `cs-ffi-macros` — proc-macro crate exposing `#[host_proc]`. Plugin
  authors depend on this for the ergonomic registration.

**Rationale:**
- Proc-macro crates are slow to compile; isolating means users who
  don't need the macro (e.g. they hand-write registrations) skip
  the build cost.
- Mirrors the serde / serde-derive split.

### D-7. `unsafe` is contained in `cs-ffi`'s ABI module

Dynamic linking, raw pointer arithmetic, and `catch_unwind` are
necessarily `unsafe`. They live in `cs-ffi/src/abi.rs` and
`cs-ffi/src/dlopen.rs`. The rest of the workspace (and user-
facing FFI code) stays `unsafe`-free.

### D-8. FFI calls cannot be captured by `call/cc`

When M8 lands first-class continuations, FFI boundaries are
opaque to capture. A Scheme continuation captured inside a
host-procedure call is invalid for invocation outside it. The
exact error semantics are M8's call to make; for M5b we just
document the limitation.

### D-9. JIT-side FFI inlining (forward-ref to M6)

When M6's JIT lowers a call to a known FFI procedure with concrete
argument types, it can inline the marshaling chain (`FromValue`
calls become direct register loads, `IntoValue` calls become
register stores). Generic FFI calls go through the same dispatch
the bytecode VM uses.

The JIT's RIR (`cs-rir`) gains a `Inst::FfiCall` variant in M6 that
the lowerer consumes; M5b's job is to make sure the runtime side
of the FFI is amenable to that inlining (i.e. the registration
table is queryable from the JIT).

## Consequences

- **Pro:** Two-direction interop with idiomatic Rust on the user
  side. Embedding stays simple.
- **Pro:** Versioned ABI lets shared-lib plugins survive runtime
  upgrades.
- **Pro:** GC rooting story is type-safe via the borrow checker.
- **Con:** Marshaling cost on every cross-boundary call. Mitigated
  for known signatures by the JIT in M6+.
- **Con:** Two crates (`cs-ffi` + `cs-ffi-macros`) for users who
  want the ergonomic API. Standard Rust idiom, low confusion cost.

## Out of scope (deferred follow-ups)

- Async / future-typed FFI calls
- Variadic Rust functions
- Generic-typed host procedures (need M9 stdlib infrastructure)
- Cross-language exception propagation beyond panics
- WASM-specific FFI surface (M10)
