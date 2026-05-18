# wasip2-networking — Requirements

> Status: **Draft**
> Spec slug: `wasip2-networking`
> Predecessor: `stdlib-modules` (closed — 19 iters, 26/28 modules
> ship on WASM). This spec lifts the last 3 exclusions.
> Companion ADR: TBD (will be drafted alongside design.md, likely
> ADR 0020 — "wasip2 build target for networking stdlib").

## Context

The `stdlib-modules` spec landed 28 `(crab …)` modules. The
`wasm-stdlib` feature (cs-cli) ships 26 of them to
`wasm32-wasip1`. The 2 still excluded are:

| Module | Procs | Why excluded |
|---|---|---|
| `cs-stdlib-net` | tcp-connect/listen/accept/send/recv/close, udp-bind/send-to/recv-from/close, dns-resolve | `std::net::TcpStream` is stubbed on wasm32-wasip1 |
| `cs-stdlib-http` | http-get/post/request (client) + http-server-bind/accept/respond + request-method/url/headers/body (server) | `ureq` doesn't build for any wasm32 target; `tiny_http` requires `std::thread::spawn` which isn't on wasip2 std |
| `cs-stdlib-websocket` | ws-connect, ws-listen, ws-accept, ws-send, ws-recv, ws-close | tungstenite on top of `std::net::TcpStream` — same socket gap |

(Three modules, not two, because the previous spec counted "http
client + server" as one. We treat them per-crate here.)

The blocker is uniform: **WASI preview 1 has no sockets**.
WASI preview 2 has the `wasi:sockets-0.2.0` interface and Rust
1.83+ exposes `std::net::TcpStream` against it on
`wasm32-wasip2`. So the gap is closable — but not without a
target change and a partial source rewrite.

## Goals

1. **WASM users get networking.** A program that does
   `(http-get "https://example.com")` should work on WASM, not
   raise "unsupported".
2. **One Scheme-side surface.** A WASI user shouldn't need to
   know which HTTP-client crate is underneath — `(http-get …)`
   means the same thing on native, wasip1, and wasip2.
3. **Native users lose nothing.** The current native build (ureq
   + tiny_http) keeps working unchanged. The WASM-target swap is
   `cfg`-gated.
4. **Build system supports the new target.** `cargo build --target
   wasm32-wasip2 -p cs-cli --features wasm-stdlib-full` produces
   a working binary; CI matrix gains a wasip2 job.

## Non-goals

- **Migrating away from wasip1.** wasip1 remains a supported
  target — embedders that don't need sockets pay nothing.
- **Async / preview 3.** WASI 0.3 adds async primitives but no
  process/socket changes that affect this spec. Defer.
- **Replacing the BEAM actor model for WASM concurrency.** The
  actor system is tokio-based and doesn't run on WASM at all
  (this spec doesn't change that). HTTP server on wasip2 uses
  the `wasi:http/incoming-handler` pattern, which is itself a
  reactive model.
- **WebSocket server on wasip2.** Pre-opened-socket complications
  + no WASI WS standard make this not worth the complexity in
  v1 of this spec. WS client is in scope; WS server is deferred.
- **Wasmer / WasmEdge runtime support.** v1 targets Wasmtime 16+
  for the networking stack. Other runtimes can be added later
  but their wasi:sockets / wasi:http coverage lags.

## Functional requirements

**FR-1**: `cargo build --target wasm32-wasip2 -p cs-cli
--no-default-features --features wasm-stdlib-full` produces a
`crabscheme.wasm` that includes all 28 stdlib modules. A
matching `wasm-stdlib` (wasip1) builds 26 modules (the existing
behavior).

**FR-2**: On wasip2, the 4 client/UDP/DNS net procs work end to
end against a real HTTP endpoint when run under Wasmtime 16+
(see "Test plan" in design.md):
- `(tcp-connect host port)` opens a TCP stream
- `(udp-bind / send-to / recv-from)` round-trip a packet
- `(dns-resolve host)` returns at least one address

**FR-3**: On wasip2, `(http-get URL)` returns a response alist
shape-compatible with the native build (same keys, same value
types).

**FR-4**: On wasip2, the HTTP server entry point is **different
in shape from native** but reaches the same end:
- Native: `(http-server-bind addr)` + `(http-server-accept)`
  loop in user Scheme
- wasip2: register a handler via
  `(http-incoming-handler proc)` that the runtime calls
  per-request. The Scheme handler returns the response alist
  the runtime then writes back. This is an API surface
  divergence; documented as such.

**FR-5**: TCP server (`tcp-listen` / `tcp-accept`) is **deferred
in v1**. wasi:sockets 0.2 doesn't standardize socket creation;
listeners require runtime-provided pre-opened sockets. Document
this gap; raise `FfiError::HostFailure` at call time on wasip2,
same shape as `process` does on wasip1.

**FR-6**: WebSocket client (`ws-connect` + send/recv/close) works
on wasip2 by virtue of tungstenite-on-`std::net::TcpStream`
working there. WS server (`ws-listen`/`ws-accept`) is deferred
in v1 alongside TCP server.

**FR-7**: All 30 existing conformance tests still pass on native.
At least 4 new conformance tests cover the wasip2 paths
(client GET, TCP round-trip, DNS resolve, incoming-handler
shape).

## Non-functional requirements

**NFR-1**: Native default build behaviour unchanged. Procedure
names unchanged. Existing wasip1 `wasm-stdlib` feature
unchanged in coverage (26 modules; the 3 networking ones do
NOT get added there).

**NFR-2**: TLS verification on by default for `https://` requests
on both targets. wasip2 outsources TLS to the runtime (no
crypto in the WASM module); document the trust boundary.

**NFR-3**: Conformance tests for wasip2 procs run in CI under
Wasmtime ≥ 16. CI matrix gains one `cargo build --target
wasm32-wasip2 --features wasm-stdlib-full` job + one
`wasmtime run --invoke conformance-runner-wasip2 …` job.

**NFR-4**: Binary size budget. wasip2 + wasm-stdlib-full debug
build target ≤ 100 MB (currently 91 MB at wasip1
+ wasm-stdlib). Release-mode wasip2 target ≤ 25 MB; if
ruzstd / wasi-http-client overflow this we file a
size-investigation follow-up.

## Risks

1. **Runtime support narrows for the networking subset.**
   Wasmtime 16+ only. Embedders on older Wasmtime / Wasmer /
   WasmEdge / Jco can still use the 26-module wasip1 build but
   not the 28-module wasip2 build. Document; do not block.

2. **HTTP server API divergence (FR-4)** is a real surface-area
   wart. Single-binary-multi-target Scheme code has to
   `cond-expand` between `http-server-accept` (native) and
   `http-incoming-handler` (wasip2). Mitigation: a Scheme
   shim library `(crab http server)` that exposes both shapes
   so programs that opt in to one don't break on the other
   platform — they fail at import time on the missing one.

3. **wasi:sockets 0.2 is young.** Known bug in Wasmtime 27
   ([bytecodealliance/wasmtime#9938](https://github.com/bytecodealliance/wasmtime/issues/9938))
   where socket read returns 0 instead of partial bytes; pin
   to Wasmtime 28+ in the CI matrix or document a workaround.

4. **TCP-listen / WebSocket-server deferral creates a partial
   surface.** `(tcp-listen)` on wasip2 raises at call time. We
   keep parity with how `process` behaves on wasip1 (existing
   precedent) but it's still asymmetric vs the native build.
   Document in the per-crate README and in
   `(crab-list-modules)`-derived help.

5. **CI cost.** Adding a wasip2 build + test matrix ~doubles
   WASM CI time. Acceptable.

## Out of scope

- WebSocket server on wasip2 (deferred — see Non-goals).
- TCP server (`tcp-listen`) on wasip2 (deferred — see FR-5).
- Async (wasi:io/poll / wasi 0.3 future-stream). Sync only.
- Multi-runtime support (Wasmer / WasmEdge). Wasmtime-only v1.
- WASIX / Wasmer process spawning.
- WASM browser target (`wasm32-unknown-unknown`). Different
  problem space (JS bindings via wasm-bindgen).
