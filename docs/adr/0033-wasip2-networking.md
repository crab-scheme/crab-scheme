# ADR 0033: WASM stdlib networking via `wasm32-wasip2`

> Status: Accepted (foundation: iter-1 scaffold; iters 2–8 land per the spec)
> Date: 2026-05-27
> Authors: crab-scheme contributors

## Context

The stdlib-modules milestone shipped 26 of 28 portable `(crab …)`
modules; **`cs-stdlib-net`, `cs-stdlib-http`, and `cs-stdlib-websocket`**
were excluded on WASM because they all bottom out on
`std::net::TcpStream`, which is stubbed on `wasm32-wasip1` (WASI preview
1 has no sockets). `ureq` doesn't build on any `wasm32-*` target, and
`tiny_http`'s accept-loop server needs `std::thread::spawn`.

`wasm32-wasip2` closes that gap upstream: `wasi:sockets-0.2.0` (Rust
1.83+ exposes `std::net::TcpStream` against it) plus `wasi:http-0.2.0`
for the server side. With our toolchain pinned to 1.95 this is reachable
today; the cost is that the wasip2 build narrows to **Wasmtime 28+** in
practice (Wasmer / WasmEdge lag on `wasi:sockets` compliance).

The full FRs / NFRs / risks / non-goals + per-crate code plan live in
`.spec-workflow/specs/wasip2-networking/{requirements,design,tasks}.md`.
This ADR captures the design choices that need durable record and lays
down the iter-1 scaffold.

## Decision

Close the three gaps on `wasm32-wasip2` behind a new opt-in Cargo
feature, using upstream wasi-* libraries rather than building our own.
The wasip1 build stays exactly as it is today (the new feature is
wasip2-only); native is untouched.

### Per-module substitutions

| Module | Native | `wasip2` |
|---|---|---|
| `cs-stdlib-net` | `std::net` | `std::net` *(works on wasip2; `tcp-listen`/`tcp-accept` cfg-gated to raise — WASI sockets 0.2 doesn't standardize socket creation)* |
| `cs-stdlib-http` (client) | `ureq` | `wasi-http-client = "0.2.1"` |
| `cs-stdlib-http` (server) | `tiny_http` accept-loop | `wasi:http/incoming-handler` via wit-bindgen (handler-callback shape) |
| `cs-stdlib-websocket` | tungstenite-on-`std::net` | tungstenite-on-`std::net` *(works as-is; `ws-listen`/`ws-accept` cfg-gated)* |

### New surface
- Cargo feature `wasm-stdlib-full` (opt-in) — pulls the three modules
  into a wasip2 build. The default `wasm-stdlib` feature stays portable
  across both wasi targets.
- New Scheme proc `http-incoming-handler` (iter-5) for the
  wasi-http server shape.
- New Scheme proc `(crab-target)` returning `"native"` / `"wasi-p1"` /
  `"wasi-p2"` for cond-expand (iter-6) — a permanent useful addition
  regardless of this spec.
- Scheme shim `lib/crab/http/server.scm` exposes *both* the native
  `http-server-accept` and the wasip2 `http-incoming-handler`, with a
  clear error when called on the wrong target.

### Tradeoffs accepted
1. **Runtime support narrows** for the wasip2 networking subset — Wasmtime
   28+ in practice. The existing `wasm-stdlib` wasip1 build stays
   available for broader-runtime targets.
2. **HTTP server API divergence**: native = accept-loop; wasip2 =
   handler-callback. Mitigated by the Scheme shim + `(crab-target)`. Same
   source can target both via cond-expand; otherwise fails clearly at
   import time on the missing shape.
3. **TLS trust moves to the runtime**: native uses `rustls` +
   `webpki-roots` inside the binary; wasip2 outsources to the runtime
   (Wasmtime handles HTTPS upgrade transparently). Different threat
   model, not a regression.
4. **WS server + TCP server gap** persists on wasip2 — `wasi:sockets-0.2`
   doesn't standardize socket creation, and `wasi:websocket` doesn't
   exist yet. Both raise `FfiError::HostFailure` at call time on wasip2,
   matching the existing wasip1 stub shape. Revisit when `wasi:sockets`
   0.3 or a `wasi:websocket` proposal lands.

### Explicitly out of scope (v1)
WebSocket server on wasip2, TCP server on wasip2, Wasmer/WasmEdge
support for the new subset, browser target
(`wasm32-unknown-unknown`), WASI 0.3 async migration.

## This iter (iter-1) — scaffold only

Lands: this ADR + a new `wasm-build-wasip2` job in `.github/workflows/
ci.yml` that runs `cargo build --target wasm32-wasip2 --release -p cs-cli
--no-default-features`. Confirms the wasip2 target works as a CI
baseline before iter-2 introduces the `wasm-stdlib-full` feature. The
spec itself is already on `main` under `.spec-workflow/specs/wasip2-
networking/`. No code module changes in this iter.

Acceptance for iter-1: the new CI job goes green; this ADR + the spec
are on `main`. ✓

## Consequences

### Positive
- Closes the `(crab net|http|websocket)` portability gap (29/29
  portable on wasip2 once the iter sequence completes).
- Uses upstream wasi-* libraries — no in-tree socket / HTTP / WS
  protocol code to maintain.
- Iter sequence is purely additive behind `wasm-stdlib-full`; the
  default wasip1 build is untouched throughout.

### Negative / limitations
- New runtime constraint for the networking subset (Wasmtime 28+).
- Public surface gains target-conditional procedures (server-shape
  asymmetry, mitigated by the Scheme shim + `(crab-target)`).
- New transitive deps on the wasip2 target only (`wasi-http-client`
  + likely `wit-bindgen` for iter-5).

### Rollback
The iter sequence is structured for easy revert: drop the
`wasm-stdlib-full` feature + the cfg-gated `_wasi` modules + the
wit-bindgen build glue + the wasip2 CI job. Native + wasip1 untouched
throughout, so reverting at any point is safe.

## References
- Issue #9; spec at `.spec-workflow/specs/wasip2-networking/`
  (`requirements.md`, `design.md`, `tasks.md` — 8 iters, ~2 weeks).
- `docs/milestones/stdlib-modules-exit.md` — the 26/28 baseline this
  closes against.
- WASI: `wasi:sockets-0.2.0`, `wasi:http-0.2.0`, Wasmtime 28+.
