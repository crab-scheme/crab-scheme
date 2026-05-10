# M5b FFI — Requirements

> Status: **Draft**
> Spec slug: `ffi`
> Roadmap slot: M5b (parallel to M6 JIT work)
> Predecessor: M5 (`docs/milestones/m5-exit.md`)

This spec adds a Rust FFI surface so CrabScheme programs can call
into Rust libraries and Rust programs can register host procedures
that Scheme can call. M5b is intentionally orthogonal to M6/M7 (JIT)
— FFI doesn't depend on JIT, but it does depend on M5's GC for
correct value rooting across the boundary.

The headline use cases:

1. **Embed CrabScheme in a Rust application** and have the host
   register Rust callbacks that Scheme code calls (the dominant
   case for embedded scripting).
2. **Call into existing Rust crates from Scheme programs** —
   serde, regex, hyper-style libraries — via either compile-time
   or runtime linking.
3. **Write CrabScheme builtins as external Rust crates** —
   currently every builtin lives inside `cs-runtime/src/builtins/`;
   users should be able to add domain-specific builtins from
   external crates without modifying the workspace.

---

## Functional requirements

### FR-1. Compile-time-registered Rust callbacks

A new crate `cs-ffi` exposes:

```rust
pub trait HostProcedure: Send + Sync {
    fn name(&self) -> &str;
    fn call(&self, args: &[Value]) -> Result<Value, FfiError>;
}

impl Runtime {
    pub fn register_host_procedure(&mut self, proc: Arc<dyn HostProcedure>);
}
```

Plus a `#[crabscheme::host_proc("scheme-name")]` attribute macro
on user functions that wires the registration up automatically:

```rust
#[crabscheme::host_proc("my-add")]
fn rust_add(args: &[Value]) -> Result<Value, FfiError> {
    // ...
}
```

Acceptance: a small Rust crate outside the workspace can register a
host procedure, embed CrabScheme, evaluate a Scheme program that
calls the procedure, and observe the result.

### FR-2. Type marshaling layer

Convert between Scheme `Value` and idiomatic Rust types via the
`FromValue` / `IntoValue` traits:

```rust
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self, FfiError>;
}

pub trait IntoValue {
    fn into_value(self) -> Value;
}
```

Default impls for: `i64`, `f64`, `bool`, `char`, `String`, `&str`,
`Vec<T>` (list), `Vec<u8>` (bytevector), `()` (unspecified),
`Option<T>` (`#f` ↔ None), `Result<T, E>` (raise on Err).

A typed wrapper macro for ergonomic Rust-side declarations:

```rust
#[crabscheme::host_proc("greet")]
fn greet(name: String) -> String {
    format!("Hello, {}!", name)
}
// At call time CrabScheme marshals the single arg from Scheme string
// to Rust String, calls `greet`, marshals back.
```

Acceptance: round-trip every default-impl type through Scheme
without loss; type-mismatch errors surface as catchable
&type-error conditions.

### FR-3. GC root protection across FFI calls

When Rust code holds a `Value` across a Scheme-level operation that
can trigger GC (re-entry, allocation, etc.), the held value must be
rooted. New API:

```rust
pub struct Pinned<'rt> {
    /* opaque */
}

impl Runtime {
    pub fn pin<'rt>(&'rt mut self, v: Value) -> Pinned<'rt>;
}

impl<'rt> Drop for Pinned<'rt> {
    // unroots on drop
}
```

`Pinned` holds the value alive across allocator collections; on
drop it unroots. The borrow on `&mut Runtime` enforces that no
concurrent Scheme operation can run while a pin is live (single-
threaded runtime model).

Acceptance: a fuzz test that allocates 100k pairs while holding
a pinned value asserts the pinned value's content is unchanged
after every GC.

### FR-4. Runtime linking via `(load-shared-library "path")`

A Scheme-level builtin that loads a `.dylib` / `.so` / `.dll` at
runtime and exposes its `crabscheme_register` entry point:

```scheme
(load-shared-library "libmyext.dylib")
;; the .so's `crabscheme_register` function ran, registering host
;; procedures named on the Rust side.
```

The shared library declares its registration via:

```rust
#[no_mangle]
pub extern "C" fn crabscheme_register(rt: *mut RuntimeFfi) {
    // unsafe; uses the FFI ABI in cs-ffi.
    let rt = unsafe { &mut *rt };
    rt.register_host_procedure(Arc::new(MyProc));
}
```

Acceptance: a separately-built `.dylib` registers a procedure;
`load-shared-library` exposes it; the Scheme call works.

### FR-5. Stable C-ABI

The `cs-ffi` crate defines a versioned C-ABI struct (`RuntimeFfi`)
that the runtime exposes to dynamically-loaded libraries. The
ABI carries:

- API version (u32; bump on breaking change).
- A function pointer table for the operations external code can
  invoke (allocate, raise, register procedure, eval string, etc).

Acceptance: a `.dylib` built against `cs-ffi` v0.1 still loads
under runtime v0.2 if v0.2's ABI is backward-compatible (the
shared object checks the version itself and refuses to load on
mismatch).

### FR-6. Error propagation

FFI errors must propagate as catchable Scheme conditions:

- `FfiError::TypeMismatch` → `&type-error` simple in the condition
- `FfiError::Panic(msg)` → `&error` with the panic message as
  message; rust-side panics are caught at the FFI boundary and
  translated rather than aborted.
- `FfiError::ArityError { expected, got }` → standard arity error
  as if a Scheme builtin had raised it.
- `FfiError::HostFailure(msg)` → generic `&error`.

Acceptance: every variant produces a condition catchable by
`with-exception-handler` on both walker and VM tiers.

### FR-7. Embedding API stability

The existing public API on `cs_runtime::Runtime` (`new()`,
`eval_str()`, `eval_str_via_vm()`, `format_value()`, etc.) is
considered the embedding API and must remain source-compatible
within a major version. Document this in `cs-runtime/README.md`
and mark FFI-relevant additions as `#[doc(stable)]`.

Acceptance: a small `examples/embedded_runtime.rs` in `cs-runtime`
shows the standard embedding pattern; doctest confirms it
compiles + runs.

---

## Non-functional requirements

### NFR-1. `unsafe` is contained in `cs-ffi`

Dynamic linking and C-ABI marshaling necessarily use `unsafe`.
The blast radius stays inside `cs-ffi`; user-facing host-procedure
code is `unsafe`-free.

### NFR-2. No allocator coupling

The FFI must not assume the Scheme heap and the Rust caller share
an allocator. Rust types passed in are `Clone`d / converted at the
boundary, not aliased; same in the other direction.

### NFR-3. Documentation: every FfiError variant

Each variant of `FfiError` documents what causes it, what condition
shape Scheme code sees, and how to recover.

### NFR-4. ADR

`docs/adr/0008-ffi-design.md` ratifies:
- Compile-time registration first vs runtime dlopen first.
- Why we picked C-ABI with a versioned table over Rust-ABI inline.
- Pin-based rooting vs root-set-list registration.
- How FFI interacts with the future JIT (M6+) and continuations (M8).

---

## Out of scope (deferred)

| Item | Where it lives |
|---|---|
| Async / future-typed FFI calls | post-M5b |
| Variadic Rust functions | post-M5b |
| Generic-typed host procedures | M9 (after stdlib) |
| Cross-language exception propagation beyond panics | post-M5b |
| WASM-specific FFI surface | M10 (`wasm` track) |

---

## Risks

1. **Lifetime confusion at the boundary.** Rust's borrow checker
   doesn't see across FFI; users may accidentally hold dangling
   references.
   *Mitigation:* the `Pinned<'rt>` wrapper ties value liveness to
   a `&mut Runtime` borrow.

2. **Panic across FFI boundary.** A Rust panic in a host procedure
   that crosses the boundary into Scheme must not abort the
   process.
   *Mitigation:* `catch_unwind` at every boundary; translate to
   `FfiError::Panic`.

3. **ABI drift between runtime + shared library.** Users build
   shared libs against an older `cs-ffi`; runtime evolves; their
   .so won't load.
   *Mitigation:* versioned ABI (FR-5); `load-shared-library`
   error message names the mismatch clearly.

4. **Holding a `Value` across GC.** If the user grabs a `Value`
   from `args` and stores it in a Rust struct that outlives the
   FFI call, the next GC may sweep its contents.
   *Mitigation:* `Pinned<'rt>` is required for cross-call retention;
   document loudly.

---

## Acceptance summary

| Gate | Source |
|---|---|
| `cs-ffi` crate exists | workspace member |
| `#[crabscheme::host_proc]` attribute macro | `cs-ffi-macros` crate |
| `FromValue` / `IntoValue` defaults | `cs-ffi/src/marshal.rs` |
| `Runtime::pin` lifetime API | `cs-runtime/src/lib.rs` |
| `(load-shared-library)` builtin | `cs-runtime/src/builtins/...` |
| Versioned C-ABI struct | `cs-ffi/src/abi.rs` |
| `FfiError` → catchable Scheme conditions | both tiers green on a test that catches each variant |
| Embedded-runtime example builds & runs | `cs-runtime/examples/embedded_runtime.rs` |
| External-shared-library example builds & loads | `crates/cs-ffi-example/` (workspace member, target-dir output is a `.dylib`) |
| ADR 0008 written | `docs/adr/0008-ffi-design.md` |
