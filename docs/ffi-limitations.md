# cs-ffi Limitations + Backlog (discovered 2026-05-16)

> Tracking doc for FFI gaps surfaced while building the M10
> Track W example plugins (`cs-ffi-sha2`, `cs-ffi-http`,
> `cs-cli-sha2`). Each entry includes the symptom that exposed it
> and a sketched fix. None of these block 1.0 — they're follow-on
> polish for the FFI surface.

## L1 — No value decoders in the C-ABI

**Symptom:** `cs-ffi-sha2`'s dlopen path can only register
`(sha256-empty)` (arity 0) because the C-ABI provides no way to
unpack a Scheme string argument into Rust `&[u8]`. Same blocker
forced `cs-ffi-http` to register `(http-get-example-com)` with a
hardcoded URL instead of taking a user-supplied string.

**Root cause:** `cs_ffi::abi::RuntimeFfi` exposes argument
*encoders* (`alloc_string`, `alloc_pair`, `alloc_fixnum`, …) but
NO matching decoders. A plugin can only construct values, not
introspect the ones it receives.

**Fix (sketched):** add per-type decode callbacks to `RuntimeFfi`:

```rust
pub decode_string: extern "C" fn(
    rt: *mut RuntimeFfi, v: ValueRef,
    out_ptr: *mut *const c_char, out_len: *mut usize,
) -> i32,
pub decode_bytevector: extern "C" fn(...) -> i32,
pub decode_fixnum: extern "C" fn(rt, v: ValueRef, out: *mut i64) -> i32,
pub decode_flonum: extern "C" fn(rt, v: ValueRef, out: *mut f64) -> i32,
```

Each returns 1 on type match (out-params written), 0 on type
mismatch (out-params untouched). The returned `(ptr, len)` borrow
is valid for the call's duration only — the plugin must copy if
it needs to retain.

Bumps `CRABSCHEME_FFI_API_VERSION` from 1 to 2. Existing v1
plugins continue to load but can't use the new decoders.

**Impact:** unblocks real-shape plugins. `cs-ffi-sha2` could then
register `(sha256 v)` via dlopen with the same signature as the
static-link path; `cs-ffi-http` could register `(http-get url)`.

## L2 — Asymmetric capabilities between C-ABI and trait surface

**Symptom:** `cs-ffi-sha2` has TWO different registered Scheme
names depending on path:
- dlopen: `(sha256-empty)` — 0 args, hardcoded input
- static-link: `(sha256 v)` — 1 arg, polymorphic

This is confusing for users porting a plugin between paths.

**Root cause:** the trait path (`HostProcedure` + `&[Value]`)
gives full Rust access to the value enum; the C-ABI path
(post-L1 fix) would give per-type decoders but still no
polymorphic-value enum access.

**Fix (sketched):** post-L1, document the dlopen path's
typed-decoder API as the "real" surface for cdylib plugins. Add a
helper trait `ValueRefDecode` in cs-ffi that wraps the C-ABI
decoders into a `&dyn HostProcedure`-style Rust API the dlopen
plugin author can call directly. Same plugin code becomes
buildable as either crate-type.

**Impact:** cs-ffi-sha2 could collapse to a single
`(sha256 v)` implementation across both paths.

## L3 — No structured error reporting from C-ABI plugins

**Symptom:** Both `cs-ffi-sha2`'s and `cs-ffi-http`'s dlopen
thunks flatten failures into a bare `EvalStatus::EvalError` with
empty `error: ValueRef { handle: 0 }`. The Scheme caller sees a
generic error without a message.

**Root cause:** Constructing a Scheme condition from a plugin
requires `alloc_*` callbacks to build the condition value, but
the cs-ffi C-ABI doesn't yet expose enough constructors for
typical condition shapes (compound conditions, message-condition
+ irritants).

**Fix (sketched):** add helper callbacks:

```rust
pub make_error_condition: extern "C" fn(
    rt: *mut RuntimeFfi,
    message_ptr: *const c_char, message_len: usize,
    who_ptr: *const c_char, who_len: usize,
) -> ValueRef,
```

Plus document the `raise` callback (already in v1) as taking the
constructed condition.

**Impact:** dlopen plugins can surface meaningful errors. Real
plugins want this even for a 1.0 release.

## L4 — tokio + wasm32-wasip1 incompatibility

**Symptom:** `cs-ffi-http` (which uses `reqwest::blocking` →
`tokio`) cannot compile for `wasm32-wasip1`. The build fails
with "cannot find `blocking` in `reqwest`" because tokio's
reactor requires OS-level epoll/kqueue/IOCP that wasm32-wasip1
doesn't expose.

**Root cause:** WASI Preview 1 (wasip1) doesn't have an outbound
HTTP capability or a poll/IO subsystem that tokio's reactor can
target. Async runtimes for wasm32-wasip1 use single-threaded
cooperative scheduling (e.g. `wasm-bindgen-futures`,
`wstd::runtime`) which incompatible-ABIs with tokio.

**Fix (not in our scope):** WASI Preview 2 (component model)
adds the `wasi:http` interface for outbound HTTP. Once the
ecosystem stabilizes (wasmtime's Preview 2 support is currently
experimental; reqwest's Preview 2 backend is in development),
cs-ffi-http could target `wasm32-wasip2` or use a different HTTP
client compatible with Preview 2.

**Impact:** This isn't a cs-ffi bug — it's a structural ecosystem
constraint that drives our `ffi-dynamic` (native) vs `ffi-trait`
(WASM-OK) split. cs-ffi-http is the demonstration that the split
is meaningful: a real-world plugin using a real-world library
ends up in the native-only bucket today.

## L5 — No way for plugins to declare typed arity/signature

**Symptom:** `HostProcDecl` has an `arity: u32` field but no
type-signature description. A plugin can declare `(sha256 v)` as
arity 1 but can't communicate "v must be string-or-bytevector"
to the runtime. The runtime can't validate before calling, and
type errors surface as `EvalStatus::EvalError` from inside the
plugin's call thunk.

**Root cause:** The cs-ffi design (per ADR 0008) opted for
untyped plugin signatures to keep the C-ABI minimal.

**Fix (sketched):** add an optional `signature: *const c_char` to
`HostProcDecl` carrying a Scheme-style type spec (e.g.
`"(string-or-bytevector) -> string"`). The runtime can use it for
documentation / error messages; it doesn't validate before
calling. Backward compat: NULL signature = no info, matching v1
behavior.

**Impact:** small UX improvement; not blocking.

## L6 — Plugin registration is fire-and-forget

**Symptom:** `crabscheme_register` returns a status code but
the runtime currently discards it — `(load-shared-library)`
succeeds even if the plugin reports `VersionMismatch`.

**Root cause:** `Runtime::load_shared_library` (cs-runtime/ffi.rs)
calls the plugin's register entry but doesn't propagate the
return value back to the Scheme caller.

**Fix (sketched):** thread the status through. Either:
- `(load-shared-library path)` returns the integer status (current
  semantics is `unspecified`).
- Non-Ok status raises a Scheme condition with the status value.

**Impact:** plugins with version mismatch silently fail to
register. Bug worth fixing for production use.

## L7 — Tooling: no `cargo crabscheme-plugin` helper

**Symptom:** plugin authors must hand-write the `cdylib` +
`rlib` Cargo.toml + the `crabscheme_register` extern + the C-ABI
boilerplate.

**Fix (sketched, large scope):** a `cargo-crabscheme-plugin`
subcommand or template that scaffolds a new plugin with both
modes (dlopen + static-link) wired up correctly. Could integrate
with the `cs-ffi-macros` proc-macro crate.

**Impact:** developer experience. Not blocking but reduces
adoption friction.

## Priorities

For the cs-ffi roadmap (post-1.0):

| # | Item | Effort | Unblocks |
|---|------|-------:|----------|
| L1 | Value decoders in C-ABI | small | real dlopen plugins |
| L6 | Surface registration status | tiny | correctness on misversioned plugins |
| L3 | Structured error reporting | small | production plugins |
| L2 | Symmetric trait/C-ABI surface | medium | cleaner plugin code |
| L5 | Typed signatures | tiny | UX |
| L4 | wasi-http (ecosystem) | ext | wait for upstream |
| L7 | Plugin scaffold tooling | medium | DX |

None block 1.0 RC. L1 + L6 together would make dlopen plugins
genuinely useful for real workloads; recommend bundling them as
a "cs-ffi v2" iter before declaring the FFI surface stable.

## How to verify state

```bash
# Reproduce the examples this doc references:
cargo build --release -p cs-ffi-sha2
./target/release/crabscheme \
  -e '(load-shared-library "target/release/libcs_ffi_sha2.dylib") (display (sha256-empty))'
# => e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855

cargo build --release -p cs-ffi-http
./target/release/crabscheme \
  -e '(load-shared-library "target/release/libcs_ffi_http.dylib") (display (string-length (http-get-example-com)))'
# => 528 (or whatever example.com returns)

cargo build --release -p cs-cli-sha2
./target/release/crabscheme-sha2 -e '(display (sha256 "hello"))'
# => 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824

devenv shell -- cargo build --release --target wasm32-wasip1 -p cs-cli-sha2
devenv shell -- bash -c 'wasmtime run --dir=. \
  target/wasm32-wasip1/release/crabscheme-sha2.wasm \
  -e "(display (sha256 \"hello\"))"'
# => 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824 (same!)

# And the negative case:
devenv shell -- cargo build --release --target wasm32-wasip1 -p cs-ffi-http
# => error[E0433]: cannot find `blocking` in `reqwest`
```
