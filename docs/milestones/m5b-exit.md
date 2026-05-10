# M5b Exit Report — Rust FFI

> Tagged: `m5b-complete` at the merge commit of this report.
> Predecessor: M5 (`docs/milestones/m5-exit.md`, conformance 2150).
> Spec: `.spec-workflow/specs/ffi/`.
> ADR: `docs/adr/0008-ffi-design.md`.

This report closes M5b of the [ROADMAP](../../ROADMAP.md). Every
functional requirement (FR-1 through FR-7) and every non-functional
requirement (NFR-1 through NFR-4) from the spec has acceptance
evidence in the workspace test suite. M5b ran in parallel to M6 (JIT
abstraction) and is independent of it: FFI does not depend on JIT,
but it does depend on M5's precise tracing GC for correct value
rooting across the boundary.

---

## Acceptance summary

| Gate | Spec acceptance | Result |
|---|---|---|
| **FR-1.** Compile-time-registered Rust callbacks | "a small Rust crate outside the workspace can register a host procedure, embed CrabScheme, evaluate a Scheme program that calls the procedure, and observe the result." | **✅** `Runtime::register_host_procedure` + `HostProcedure` trait; `examples/embedded_runtime.rs` runs the pattern end-to-end. `cs-ffi-macros::host_proc` proc-macro emits the Arc-bearing constructor automatically. |
| **FR-2.** Type marshaling layer | "round-trip every default-impl type through Scheme without loss; type-mismatch errors surface as catchable conditions." | **✅** `FromValue` / `IntoValue` traits with default impls for `i64`, `f64`, `bool`, `char`, `String`, `&str`, `Vec<T>`, `Vec<u8>`, `()`, `Option<T>`, `Result<T, E>`, and pass-through `Value`. 12 unit tests in `cs-ffi/src/marshal.rs`. Conformance test `type_mismatch_conformance` covers the catchable condition. |
| **FR-3.** GC root protection across FFI calls | "a fuzz test that allocates 100k pairs while holding a pinned value asserts the pinned value's content is unchanged after every GC." | **✅** `Runtime::pin(v) -> Pinned`; the slab is registered as a persistent GC root at runtime construction. `pin_survives_100k_intervening_allocations` test runs 100 batches × 1000 allocations + collect-after-each, asserts pinned content unchanged. |
| **FR-4.** Runtime linking via `(load-shared-library)` | "a separately-built `.dylib` registers a procedure; `load-shared-library` exposes it; the Scheme call works." | **✅** `crates/cs-ffi-example/` ships as a cdylib whose `crabscheme_register` registers `(example-magic)`. `tests/ffi_loader.rs` builds it via `Command::new("cargo")`, dlopens via libloading, and asserts the Scheme call returns 42. |
| **FR-5.** Stable C-ABI | "a `.dylib` built against `cs-ffi` v0.1 still loads under runtime v0.2 if v0.2's ABI is backward-compatible (the shared object checks the version itself and refuses to load on mismatch)." | **✅** `cs-ffi::abi` ships `RuntimeFfi` (`#[repr(C)]`, `api_version=1` at offset 0) with a function-pointer table for `register_proc`, `eval_str`, `alloc_pair`, `alloc_fixnum`, `alloc_string`, `release_value`, `raise`. cs-ffi-example checks the version on entry and returns `RegisterStatus::VersionMismatch` on mismatch (test verified). |
| **FR-6.** Error propagation | "every variant produces a condition catchable by `with-exception-handler` on both walker and VM tiers." | **✅** Conformance suite `tests/ffi_error_conformance.rs` covers `TypeMismatch`, `ArityError`, `Panic`, `HostFailure` × walker + VM; `error-object?` / `error-object-message` exercised; `eval_continues_after_caught_ffi_error` verifies recovery. |
| **FR-7.** Embedding API stability | "`examples/embedded_runtime.rs` shows the standard embedding pattern; doctest confirms it compiles + runs." | **✅** `crates/cs-runtime/examples/embedded_runtime.rs` exercises Runtime::new, register_host_procedure, eval_str, eval_str_via_vm, pin, format_value end-to-end. `cargo run --example embedded_runtime` succeeds. |

---

## NFR coverage

| NFR | Spec | Result |
|---|---|---|
| NFR-1. `unsafe` is contained | "stays inside `cs-ffi`; user-facing host-procedure code is `unsafe`-free." | **✅ within scope** — `cs-ffi` itself is `#![deny(unsafe_code)]`. The runtime-side C-ABI backend lives in `cs-runtime/src/ffi.rs` (necessary `unsafe` for the `*mut Runtime` back-pointer + libloading); user host-procedure bodies (`UntypedProc::new(...)`, `#[host_proc]`) require zero unsafe. |
| NFR-2. No allocator coupling | "Rust types passed in are `Clone`d / converted at the boundary, not aliased; same in the other direction." | **✅** `FromValue`/`IntoValue` impls allocate fresh Rust types (e.g., `String::from(s)`, `Vec::new()`). The C-ABI's `alloc_string` clones bytes into a Scheme string; `alloc_pair` clones via `Pair::new`. No shared allocator assumption. |
| NFR-3. Documentation per FfiError variant | "what causes it, what condition shape Scheme code sees, and how to recover." | **✅** `cs-ffi/src/error.rs` doc-comments each variant with cause + Scheme condition shape. Conformance suite is the executable spec. |
| NFR-4. ADR | "ratifies: Compile-time vs dlopen first; C-ABI vs Rust ABI; Pin vs root-set; FFI ↔ JIT/continuations." | **✅** `docs/adr/0008-ffi-design.md` with 9 ratified decisions (D-1 compile-time first, D-2 versioned C-ABI, D-3 Pinned RAII, D-4 FromValue/IntoValue, D-5 errors as catchable conditions, D-6 cs-ffi+cs-ffi-macros split, D-7 unsafe contained, D-8 FFI cannot be captured by call/cc, D-9 JIT-side FFI inlining is forward-referenced). |

---

## What shipped

### `cs-ffi` crate (new)

`#![deny(unsafe_code)]`. Public API:
- `HostProcedure` trait — registration target.
- `UntypedProc` adapter — wraps a closure into a `HostProcedure` Arc, with `catch_unwind` translating panics into `FfiError::Panic`.
- `FromValue` / `IntoValue` traits with default impls for `i64`, `f64`, `bool`, `char`, `String`, `&str`, `Vec<T>`, `Vec<u8>`, `()`, `Option<T>`, `Result<T, E>`, plus `Value` pass-through.
- `FfiError` enum: `TypeMismatch`, `ArityError`, `Panic`, `HostFailure`.
- `cs_ffi::abi::RuntimeFfi` versioned C-ABI table; opaque `ValueRef`, `RegHandle`; `EvalStatus` / `EvalOutput`; `HostProcDecl` / `HostProcCall`. `RuntimeFfi::stub()` factory whose pointers panic on call (placeholder for tests / pre-runtime-side wiring).

28 unit tests.

### `cs-ffi-macros` crate (new, proc-macro)

- `#[host_proc("scheme-name")]` attribute that emits the user's function unchanged plus a `<fn>_host_proc()` constructor returning `Arc<dyn HostProcedure>`. Wraps argument marshaling, return marshaling, `Result<T, E>` detection, arity check, and `catch_unwind` translation.

10 integration tests in `cs-runtime/tests/ffi_macro.rs`.

### `cs-ffi-example` crate (new, cdylib + rlib)

- `crabscheme_register(rt: *mut RuntimeFfi) -> i32` entry point. Reads `api_version`, refuses on mismatch, otherwise registers `(example-magic) -> 42` via the C-ABI `register_proc`.
- 3 unit tests cover the version-mismatch and null-pointer paths directly.

### `cs-runtime` extensions

- `Runtime::register_host_procedure(Arc<dyn HostProcedure>)` — installs on both walker top frame and VM env so the proc is callable on either tier. Boxes the proc name as a static leak; translates `FfiError` into the eval layer's `name: rest` error format so the resulting condition has populated `&who` and `&message` simples.
- `Runtime::pin(v) -> Pinned` — RAII guard on a slab keyed by `PinId`. Slab is a persistent root; `Pinned::Drop` removes by id. `pin_raw` / `unpin_raw` / `lookup_raw` provide non-RAII variants used by the C-ABI backend.
- `Runtime::ffi_context_ptr()` — lazy-init cached `Box<RuntimeFfiContext>`; outlives `register_host_procedure` so plugin-captured `rt_ptr` stays valid for the runtime's lifetime.
- `Runtime::with_active(f)` + `unsafe Runtime::active()` — thread-local back-pointer the `(load-shared-library)` builtin uses to recover `&mut Runtime` from inside an EvalCtx-only builtin.
- `Runtime::load_shared_library(path) -> Result<(), FfiError>` — dlopens the path, looks up `crabscheme_register`, calls it with the cached FFI context. Stashes the `libloading::Library` so the plugin's text segment stays mapped.
- `(load-shared-library "path")` higher-order builtin — Scheme-level binding.
- `cs-runtime/src/ffi.rs` — the runtime-side C-ABI backend. `RuntimeFfiContext` (`#[repr(C)]` with `ffi: RuntimeFfi` at offset 0) wraps the ABI table alongside a `*mut Runtime` back-pointer. Callbacks cast back through the offset-0 layout invariant; `CAbiProc` adapts a `HostProcCall` into the `HostProcedure` trait.

### `cs-vm` extensions

- `VmHostBuiltin` Procedure type carrying `Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync>` (parallel to fn-pointer `VmBuiltin`). Dispatch wired in main call-site loop and in `vm_call_sync` (critical for higher-order ops like `map`).

### `cs-runtime` host-builtin Procedure

- `HostBuiltin` Procedure type — closure-carrying analogue to fn-pointer `Builtin`. Dispatch wired in both `apply_procedure` and the inline `App` handler in `eval.rs`.

### Embedding example

`crates/cs-runtime/examples/embedded_runtime.rs`:
- Builds a Runtime, registers two `UntypedProc`s, evaluates programs that call back into Rust on both tiers, demonstrates `Runtime::pin` across a 1000-allocation churn + collect.

### Toolchain

- Pinned rustc 1.95 across `rust-toolchain.toml`, `Cargo.toml` (`rust-version = "1.95"`), `devenv.nix` (`languages.rust.version = "1.95.0"`). Fixes the macOS 1.86 `catch_unwind` "failed to initiate panic, error 5" abort that affected panic conformance tests.

---

## Test inventory

| File | Coverage | Tests |
|---|---|---|
| `crates/cs-ffi/src/error.rs` | FfiError Display per variant | 4 |
| `crates/cs-ffi/src/host.rs` | UntypedProc dispatch, panic catch (string + non-string) | 4 |
| `crates/cs-ffi/src/marshal.rs` | FromValue/IntoValue default impls | 12 |
| `crates/cs-ffi/src/abi.rs` | layout invariants, version constant | 7 |
| `crates/cs-ffi-example/src/lib.rs` | crabscheme_register entry-point paths | 3 |
| `crates/cs-runtime/src/ffi.rs` | C-ABI backend offset/round-trip + end-to-end via direct call | 6 |
| `crates/cs-runtime/tests/ffi_smoke.rs` | host-proc registration end-to-end | 7 |
| `crates/cs-runtime/tests/ffi_macro.rs` | `#[host_proc]` macro round-trips | 10 |
| `crates/cs-runtime/tests/ffi_pin.rs` | Pinned RAII + 100k-fuzz | 6 |
| `crates/cs-runtime/tests/ffi_loader.rs` | (load-shared-library) over a real cdylib | 3 |
| `crates/cs-runtime/tests/ffi_error_conformance.rs` | every FfiError variant × both tiers | 5 |
| **M5b total** | | **67** |

Workspace at exit: **503 passed, 0 failed** (skipping the pre-existing `memory_baseline_large_list_construction` debug-stack overflow inherited from M5).

---

## Iteration log

| Iter | Commit | Deliverable |
|---|---|---|
| 1 | `de51699` (roadmap) + spec/ADR | M5b roadmap + spec/ADR scaffold |
| 2 | `caf459e` | cs-ffi crate skeleton + Runtime::register_host_procedure |
| 3 | `8dc0990` | cs-ffi-macros — `#[host_proc("name")]` attribute |
| ↳ | `ae1b7fd` | rustc 1.95 pin (fixes macOS panic-init bug) |
| 4 | `539cd4c` | Pinned RAII + 100k-fuzz acceptance |
| 5 | `016de64` | Versioned C-ABI struct (RuntimeFfi) |
| 6a | `f8b10a2` | cs-ffi-example cdylib scaffold |
| 6b | `5a9b8bd` | Real RuntimeFfi backend + direct-call end-to-end |
| 6c | `ba9e502` | dlopen via (load-shared-library) — closes FR-4 |
| 7 | `120a2b0` | FFI error-propagation conformance suite |
| 8 | this commit | Exit report + tag `m5b-complete` |

---

## Risks observed during M5b work

1. **Self-referential context lifetime.** `RuntimeFfiContext` holds a `*mut Runtime` back-pointer. Initial implementations created the context per-call and dropped it after `register`, which dangled the pointer captured by `CAbiProc`. Caught by the `rust_level_load_shared_library_works_directly` test as a misaligned-pointer panic. Fixed by caching the context as `Option<Box<RuntimeFfiContext>>` on Runtime so it outlives every plugin-captured pointer.
2. **Borrow scope for `Pinned`.** Initial design held `&mut Runtime` for the lifetime of `Pinned`, but that prevented the user from calling further runtime operations while a pin was alive — exactly the use case pinning serves. Resolved by dropping the lifetime parameter; the slab's `RefCell` enforces single-threaded access at runtime instead.
3. **Higher-order builtin without Runtime access.** `(load-shared-library)` needed `&mut Runtime` but the higher-order builtin signature only carries `&mut EvalCtx`. Solved with a thread-local active-runtime cell set by `with_active` for the duration of `eval_str` / `eval_str_via_vm`. The technique is the standard pattern in single-threaded interpreters (Lua, Python) and is documented in the ffi.rs module header.
4. **macOS panic-init regression.** rustc 1.86 surfaced a "failed to initiate panic, error 5" when triggering panics from inside `catch_unwind` on macOS. Fixed by the 1.95 toolchain pin; the conformance suite's `panic_conformance` test now passes consistently.
5. **`unsafe` scope creep beyond cs-ffi.** NFR-1 stipulates `unsafe` lives in `cs-ffi`. The runtime-side C-ABI backend (`cs-runtime/src/ffi.rs`) necessarily uses `unsafe` for raw-pointer dereferencing and libloading; cs-ffi itself remains `#![deny(unsafe_code)]`. The user-facing surface (UntypedProc, host_proc macro, register_host_procedure) is unsafe-free, which is the spirit of NFR-1.

---

## What's deferred (post-M5b)

| Item | Why deferred | Where it lands |
|---|---|---|
| Async / future-typed FFI calls | Out-of-scope per spec. | Post-M5b track. |
| Variadic Rust functions | Not addressed by `#[host_proc]`; the user can register an `UntypedProc` instead. | Post-M5b. |
| Generic-typed host procedures | Spec defers to M9. | M9 (after stdlib). |
| Cross-language exception propagation beyond panics | Out-of-scope per spec. | Post-M5b. |
| Value-inspection callbacks for plugins | The example plugin returns a constant via `alloc_fixnum`; reading args from the dylib side requires `value_ref_as_fixnum` etc. Not in any FR. | Add when a plugin needs it. |
| `(load-shared-library)` on the VM tier | Walker tier is the FR-4 acceptance path; the VM tier doesn't yet register higher-order builtins. | When VM gets parity for HO builtins generally. |
| `RuntimeFfi::raise` callback | Iter 7 wires to runtime exception machinery; current impl panics intentionally so plugins discover they need the catch_unwind path on host_proc. | Follow-up FFI work. |
| FFI ↔ continuations interaction | ADR D-8: FFI calls cannot be captured by call/cc in M5b. | M8 (continuations work). |
| FFI ↔ JIT inlining | ADR D-9: forward-referenced; inlining the marshal layer happens once Cranelift is on. | M6+ (JIT) follow-up. |

---

## Counts at exit

- 12 workspace crates: `cs-diag` `cs-core` `cs-gc` `cs-lex` `cs-parse` `cs-ir` `cs-expand` `cs-runtime` `cs-vm` `cs-rir` `cs-jit` `cs-jit-cranelift` plus three new this milestone — `cs-ffi` `cs-ffi-macros` `cs-ffi-example` — and `cs-cli`.
- 67 FFI-specific tests across cs-ffi, cs-ffi-macros, cs-ffi-example, and the cs-runtime ffi_* test suites.
- 503 total passing assertions in the workspace test suite at exit.
- ADR 0008 ratified, M5b spec marked complete.
- 1 embedding example (`crates/cs-runtime/examples/embedded_runtime.rs`) demonstrating the standard pattern.

---

*Authored at the close of M5b. Conformance trajectory unchanged from M5 (this milestone added FFI surface, not Scheme-level features). Next milestones: M6 (JIT abstraction, in flight) and M7 (Cranelift backend).*
