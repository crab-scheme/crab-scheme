# M5b FFI — Design

> Status: **Draft** — sketch only, fills out as we land scaffolding.
> Companion: `requirements.md`.

## Overview

Two-direction interop between CrabScheme and Rust:

1. **Scheme → Rust**: Scheme programs invoke Rust procedures
   registered with the runtime, either at compile time (Rust crate
   that depends on `cs-runtime`) or at runtime (`(load-shared-
   library "path")`).

2. **Rust → Scheme**: Rust programs embed `cs_runtime::Runtime`
   and evaluate Scheme programs / call Scheme procedures from Rust.
   The `Runtime` API is the embedding surface.

A new crate `cs-ffi` carries the boundary primitives (marshaling,
versioned C-ABI, `Pinned<'rt>`); a sibling `cs-ffi-macros` provides
the `#[host_proc]` attribute. End-user crates pull in either
`cs-runtime` (full embedding) or `cs-ffi` alone (FFI types only,
useful when authoring shared libraries).

## Components

### `cs-ffi` crate (new)

```rust
// abi.rs — the versioned C-ABI table.
#[repr(C)]
pub struct RuntimeFfi {
    pub api_version: u32,                             // bump on break
    pub register_proc: extern "C" fn(*mut RuntimeFfi, *const HostProcDecl) -> RegHandle,
    pub eval_str: extern "C" fn(*mut RuntimeFfi, *const c_char, *const c_char) -> EvalResult,
    pub alloc_pair: extern "C" fn(*mut RuntimeFfi, ValueRef, ValueRef) -> ValueRef,
    pub raise: extern "C" fn(*mut RuntimeFfi, ValueRef) -> !,
    // ...
}
```

```rust
// marshal.rs — typed conversion traits.
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self, FfiError>;
}
pub trait IntoValue {
    fn into_value(self) -> Value;
}
// blanket impls for i64, f64, bool, char, String, &str, Vec<T>, etc.
```

```rust
// pin.rs — RAII rooting for cross-call retention.
pub struct Pinned<'rt> {
    rt: &'rt mut Runtime,
    handle: RootHandle,
}
impl<'rt> Drop for Pinned<'rt> { /* unroots */ }
```

```rust
// error.rs — uniform error type.
pub enum FfiError {
    TypeMismatch { expected: &'static str, got: String },
    ArityError { name: String, expected: String, got: usize },
    Panic(String),
    HostFailure(String),
}
```

### `cs-ffi-macros` crate (new, proc-macro)

```rust
// Re-export the procedural macros under the `crabscheme` namespace
// for ergonomic user-facing usage.
#[proc_macro_attribute]
pub fn host_proc(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Generates a `Lazy<Arc<dyn HostProcedure>>` static + a registration
    // hook that's collected by `inventory` (or a custom collector).
    // The user-written function is wrapped: args[..] are demarshaled
    // via FromValue, the Result is marshaled back via IntoValue, and
    // catch_unwind converts panics to FfiError::Panic.
}
```

### `cs-runtime` extensions

- New module `cs-runtime/src/ffi.rs` — implements the runtime side
  of the C-ABI (functions in `RuntimeFfi`).
- Public API additions on `Runtime`:
  - `register_host_procedure(...)` — compile-time registration.
  - `pin(v: Value) -> Pinned<'_>` — RAII root.
  - `load_shared_library(path: &str) -> Result<(), FfiError>` —
    runtime dlopen.

### Shared-library example

```
crates/cs-ffi-example/
├── Cargo.toml          (cdylib + dylib, depends on cs-ffi only)
└── src/lib.rs          (uses #[host_proc] for two trivial procs)
```

Builds to `target/debug/libcs_ffi_example.dylib`; tests in
`cs-runtime/tests/ffi_loader.rs` load it and call its registered
procedures.

## C-ABI design

Choosing C-ABI rather than Rust ABI for the dynamic-linking path is
ADR 0008-D-2's call: it lets users build shared libraries with any
toolchain (rustc, gcc-derived languages, even hand-rolled
assembly), keeps the version-skew story tractable (versioned struct
with function pointers), and matches what Chez / Racket / Guile do.

The trade-off: marshaling cost. Every cross-boundary call passes
through `RuntimeFfi`'s function-pointer table rather than direct
function calls. For the compile-time-registered case (FR-1), we
bypass the table entirely — `register_host_procedure` is a normal
Rust function call.

```
┌───────────────────────┐                ┌─────────────────────┐
│  Scheme side          │   call host    │  Rust side          │
│                       │ ────proc────▶  │                     │
│  (procedure value     │                │  HostProcedure      │
│   carries Arc<dyn>)   │                │  trait object       │
└───────────────────────┘                └─────────────────────┘
                                                   │
                                       ┌───────────▼──────────┐
                                       │  args: &[Value]      │
                                       │  -- FromValue chain  │
                                       │  user function call  │
                                       │  -- IntoValue chain  │
                                       │  Result<Value, ..>   │
                                       └──────────────────────┘
```

For dynamic linking:

```
                                          (.dylib)
┌───────────────────────┐                ┌─────────────────────┐
│  Scheme side          │  call into     │  Rust side          │
│  (load-shared-lib)    │ ──crabscheme── │  HostProcedure via  │
│                       │   _register    │  versioned ABI      │
└───────────────────────┘                └─────────────────────┘
        │                                          │
        ▼                                          ▼
 RuntimeFfi struct                       crabscheme_register
 with function-ptr table                 reads version, registers
 + version=1                             procedures
```

## Marshaling layer

`FromValue` / `IntoValue` are the typed conversion traits. Default
impls cover the common case; user types implement them by hand or
via a derive macro (deferred to M9).

Edge cases handled at the boundary:

- **`String` ↔ Scheme String**: clone bytes; preserve UTF-8.
- **`&str` ↔ Scheme String**: borrow against the call's argument
  lifetime; cannot be retained.
- **`Vec<u8>` ↔ Bytevector**: clone bytes.
- **`Vec<T>` ↔ Scheme List**: walk the proper-list spine; reject
  improper lists with `TypeMismatch`.
- **`Option<T>` ↔ value or `#f`**: `None` ↔ `#f`. Any non-`#f`
  value calls into `FromValue<T>`.
- **`Result<T, E: Display>` ↔ value or raise**: `Ok(v)` returns
  `IntoValue::into_value(v)`. `Err(e)` raises a Scheme condition
  whose message is the `Display` of `e`.

## GC interaction

M5 ships precise rooting via explicit `Heap::add_root` /
`Heap::remove_root`. The `Pinned<'rt>` API wraps that:

```rust
let pinned: Pinned = rt.pin(some_value);
// `some_value` is now reachable from the GC root set; the
// `&mut Runtime` borrow on `rt` is held.
do_things_that_might_alloc(&mut *pinned.rt);
// when `pinned` drops, the value is unrooted.
```

Because the borrow on `&mut Runtime` is exclusive, no concurrent
Scheme operation can run while a pin is live. This sidesteps the
two-thread / single-threaded distinction (we're single-threaded
either way for now) and matches how `Runtime` is used in tests
already.

## Continuations interaction (M8 forward-ref)

When M8 lands first-class continuations, FFI calls cannot be
captured by `call/cc`. The boundary is one-shot. If a Scheme
procedure passes a continuation through Rust and tries to invoke
it later, that's an error (or a "permanent escape" — open
question).

For M5b: document this as a deferred issue; M8's spec finalizes
the semantics.

## JIT interaction (M6 forward-ref)

JIT-compiled procedures call into Rust through the same C-ABI
table as the bytecode VM. The JIT inlines the marshaling for
known FFI signatures (specialized to the concrete `FromValue` /
`IntoValue` impls); generic FFI calls go through the normal
runtime dispatch.

## Iter schedule

1. **Iter 1** (this iter): scaffold spec + ADR + ROADMAP entry.
   No code yet.
2. **Iter 2**: `cs-ffi` crate skeleton — `FfiError`, `FromValue`
   trait, blanket impls for primitives. `cs-runtime::Runtime::
   register_host_procedure` API. First end-to-end test:
   register a Rust function, eval `(my-add 2 3)`, observe `5`.
3. **Iter 3**: `cs-ffi-macros` `#[host_proc]` attribute. Type
   marshaling for `String`, `Vec<T>`, `Option<T>`, `Result<T, E>`.
4. **Iter 4**: `Pinned<'rt>` rooting; fuzz test for cross-GC
   retention.
5. **Iter 5**: versioned C-ABI struct in `cs-ffi::abi`.
6. **Iter 6**: `cs-ffi-example` external dylib + runtime
   `(load-shared-library)` builtin.
7. **Iter 7**: error propagation through both tiers; conformance
   tests for each `FfiError` variant.
8. **Iter 8**: M5b exit report, tag `m5b-complete`.

## File-level diff scope (estimate)

| Crate | LOC change |
|---|---|
| `cs-ffi` (new) | ~600 |
| `cs-ffi-macros` (new, proc-macro) | ~250 |
| `cs-ffi-example` (new, .dylib) | ~80 |
| `cs-runtime/src/ffi.rs` (new module) | ~400 |
| `cs-runtime/src/builtins/mod.rs` | ~100 (`load-shared-library`) |
| `cs-runtime/examples/` | ~80 |
| Tests | ~300 |
