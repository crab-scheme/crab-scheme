# wasip2-networking exit report

Closes **issue #9** — the last three WASM stdlib gaps
(`cs-stdlib-net`, `cs-stdlib-http`, `cs-stdlib-websocket`) that
`stdlib-modules` left as **26 of 28 portable** on `wasm32-wasip1`.
The spec lives at `.spec-workflow/specs/wasip2-networking/
{requirements,design,tasks}.md`; the design decisions are recorded in
**ADR 0033**. Scope was **8 iters** delivered as PRs #87..#94.

## What shipped

### Iter summary

| Iter | Title | PR | Notes |
|------|-------|----|-------|
| 1 | Spec + ADR + CI scaffold | #87 | ADR 0033 + new `wasm-build-wasip2` CI job (no networking modules); confirms wasip2 builds clean. |
| 2 | `wasm-stdlib-full` feature + `cs-stdlib-net` on wasip2 | #88 | `tcp-listen`/`tcp-accept`/`udp-bind` cfg-gated to raise on wasi (no socket creation in wasi:sockets 0.2). Also target-gated `which` → non-wasi in `cs-stdlib-process` (rustix 0.38's wasip2 backend needs nightly). |
| 3 | HTTP client back-end split | #89 | `cs-stdlib-http/src/client/{client_native,client_wasi}.rs` — native uses `ureq`, wasip2 uses `wasi-http-client = "0.2.1"`. Same response-alist shape. Server moved to parallel `server_native`/`server_wasi_stub` structure (sets up iter-5). |
| 4 | WebSocket client | #90 | tungstenite per-target: `rustls-tls-native-roots` (native) vs `rustls-tls-webpki-roots` (wasi). `ws-listen`/`-accept` cfg-gates: N/A (no such procs in the client-only crate). |
| 5 | HTTP server (incoming-handler) | #91 | `wasi::http::proxy::export!(CsHttpHandler)` + `Guest::handle` + new `http-incoming-handler` Scheme proc. Iter-5 ships the wit-bindgen/component-export wiring; the Scheme-lambda call-through from `handle()` is **iter-5b** (deferred — needs Runtime-accessible-from-static-context plumbing). |
| 6 | `(crab-target)` proc + cond-expand example | #92 | New builtin returns `"native"` / `"wasi-p1"` / `"wasi-p2"`. The spec's `lib/crab/http/server.scm` shim N/A — `(crab …)` modules are Rust-registered procs, not Scheme source — replaced by `docs/examples/wasip2-http-server-cond-expand.scm`. |
| 7 | CI matrix: build wasip2 with `wasm-stdlib-full` | #93 | New `wasm-build-wasip2-full` job. Runtime conformance (wasmtime 28+ + sidecar test server) deferred to a follow-up — its own CI-orchestration project. |
| 8 | Exit report + docs | #94 (this) | This file; `stdlib-modules-exit.md` update; per-crate README notes. |

### Build matrix now in CI

| Target | Features | Job |
|---|---|---|
| `ubuntu-24.04` | default | `test ubuntu-24.04` |
| `macos-14` | default | `test macos-14` |
| `wasm32-wasip1` | `wasm-stdlib` | `wasm-build` (existing) |
| **`wasm32-wasip2`** | `--no-default-features` | `wasm-build-wasip2` (iter-1) |
| **`wasm32-wasip2`** | **`wasm-stdlib-full`** | **`wasm-build-wasip2-full` (iter-7)** |

### Coverage

`stdlib-modules` left WASM coverage at 26 of 28 portable
(`cs-stdlib-net` / `cs-stdlib-http` / `cs-stdlib-websocket` excluded
on wasip1). On `wasm32-wasip2` with `wasm-stdlib-full`:

- `cs-stdlib-net`: portable for TCP client + UDP send/recv + DNS;
  `tcp-listen` / `tcp-accept` / `udp-bind` raise (`wasi:sockets 0.2`
  doesn't standardize socket creation — pointer to the
  `wasi:http/incoming-handler` shape in the error).
- `cs-stdlib-http`: client portable via `wasi-http-client`; server
  shape diverges (handler-callback via `http-incoming-handler` in
  place of the native accept loop), with the native procs raising on
  wasi.
- `cs-stdlib-websocket`: client portable via tungstenite-on-`std::net`
  (TLS bundled-CA on wasi vs OS roots on native).

So **29 of 29 modules portable** on wasm32-wasip2 with the caveats
above (passive sockets + server shape divergence), versus 26 of 28
prior to this milestone.

## Tradeoffs accepted (per ADR 0033)

1. **Runtime support narrows for the wasip2 networking subset** to
   **Wasmtime 28+**. Wasmer / WasmEdge lag on `wasi:sockets 0.2`
   compliance; the existing `wasm-stdlib` wasip1 build stays available
   for broader-runtime targets.
2. **HTTP server API divergence**: native uses `http-server-bind` +
   accept loop; wasip2 uses `http-incoming-handler` + per-request
   callback. Mitigated by `(crab-target)` + cond-expand
   (`docs/examples/wasip2-http-server-cond-expand.scm`). Same Scheme
   source can target both via cond-expand; otherwise fails clearly at
   call time on the missing shape.
3. **TLS trust moves to the runtime / bundled CAs** on wasip2 — the
   host has no portable native cert store, so the WS client uses
   `rustls-tls-webpki-roots` (bundled Mozilla CAs) instead of the
   native-roots backend.
4. **WS server + TCP server gap** persists. `wasi:sockets 0.2` doesn't
   standardize socket creation, and `wasi:websocket` doesn't exist
   yet. Both raise `HostFailure` at call time on wasip2, matching the
   existing wasip1 stub shape. Revisit when `wasi:sockets 0.3` or
   `wasi:websocket` lands.

## Explicitly deferred (post-1.0)

These are *not* listed in the iter sequence and remain open:

- **Iter-5b**: actual Scheme-lambda invocation from
  `wasi:http/incoming-handler::handle`. The component export is wired;
  the call-through needs Runtime-accessible-from-static-context
  plumbing so `handle()` can drive the registered lambda. The current
  placeholder returns 200 (handler registered) or 503 (none).
- **Iter-7b**: end-to-end wasmtime conformance harness for
  `crab-{net,http,websocket}.scm`. Needs `wasm-tools component new` +
  the `wasi_snapshot_preview1` adapter + a sidecar test server in CI.
- WebSocket server on wasip2, TCP server on wasip2.
- Wasmer / WasmEdge runtime support for the networking subset.
- Browser target (`wasm32-unknown-unknown`) for the networking modules.
- WASI 0.3 async migration (sync only for v1).

## Verification

Each iter committed with:

- `cargo build --target wasm32-wasip2 -p cs-cli --no-default-features
   --features wasm-stdlib-full` clean.
- `cargo build --target wasm32-wasip1 -p cs-cli --no-default-features
   --features wasm-stdlib` clean (no regression).
- `cargo build -p cs-cli` (native, default features) clean.
- fmt + clippy clean on touched code; new tests added where the iter
  introduced runtime behavior (`(crab-target)` test in iter-6).

The `wasm-build-wasip2-full` CI job (iter-7) gates the full-feature
wasip2 build on every PR going forward.

## References

- Issue #9; `.spec-workflow/specs/wasip2-networking/`.
- ADR 0033 (design + tradeoffs).
- `docs/examples/wasip2-http-server-cond-expand.scm` (iter-6 example).
- WASI: `wasi:sockets-0.2.0`, `wasi:http-0.2.0`, `wasi-http-client v0.2.1`,
  `wasi v0.13`, Wasmtime 28+.
