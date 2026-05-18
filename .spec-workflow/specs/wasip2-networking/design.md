# wasip2-networking — Design

> Status: **Draft**
> Companion: `requirements.md`
> Anchor ADRs: 0008 (FFI design), 0019 (stdlib-modules — TBD
> if separately written, otherwise the `stdlib-modules`
> design.md is the reference), 0020 (this spec — TBD)

## Architecture

The current native build (post `stdlib-modules`):

```
┌──── cs-cli (native target) ────────────────────────┐
│  features = default → jit + ffi-dynamic + stdlib   │
│  cs-stdlib-net  → std::net  (TcpStream/Listener)   │
│  cs-stdlib-http → ureq (client) + tiny_http (srv)  │
│  cs-stdlib-websocket → tungstenite                 │
└────────────────────────────────────────────────────┘

┌──── cs-cli (wasm32-wasip1 target) ─────────────────┐
│  features = wasm-stdlib                            │
│  cs-stdlib-net, http, websocket    EXCLUDED        │
└────────────────────────────────────────────────────┘
```

The shape after this spec:

```
┌──── cs-cli (native target) ────────────────────────┐
│  Unchanged. Native still uses std::net / ureq /    │
│  tiny_http / tungstenite.                          │
└────────────────────────────────────────────────────┘

┌──── cs-cli (wasm32-wasip1 target) ─────────────────┐
│  features = wasm-stdlib    (unchanged from now)    │
│  cs-stdlib-net, http, websocket    STILL EXCLUDED  │
│  — wasip1 simply has no sockets, full stop.        │
└────────────────────────────────────────────────────┘

┌──── cs-cli (wasm32-wasip2 target — NEW) ───────────┐
│  features = wasm-stdlib-full                       │
│  cs-stdlib-net  → std::net  (works on wasip2)      │
│  cs-stdlib-http → wasi-http-client (client)        │
│                 + wasi:http/incoming-handler (srv) │
│  cs-stdlib-websocket → tungstenite (client only)   │
└────────────────────────────────────────────────────┘
```

Three WASM-related build configurations after this spec:
1. No-WASM native (today's default).
2. **`wasm-stdlib` on wasip1** (today's wasm subset; unchanged).
3. **`wasm-stdlib-full` on wasip2** (NEW; adds the 3 networking
   modules; requires Wasmtime ≥ 16).

## Per-crate plan

### cs-stdlib-net

`std::net::{TcpStream, UdpSocket, ToSocketAddrs}` work on
`wasm32-wasip2` from Rust 1.83+. `TcpListener::accept` is the
gap (no `wasi:sockets-0.2.0` socket-creation primitive; needs
runtime-provided pre-opened socket).

**Plan**:
- No changes to `cs-stdlib-net/Cargo.toml` (still pure
  `std::net`).
- Add cfg-gate on `tcp_listen` + `tcp_accept` for
  `cfg(all(target_arch = "wasm32", target_os = "wasi"))` that
  returns `FfiError::HostFailure("tcp-listen: not supported on
  WASI (no socket creation in wasi:sockets-0.2.0); use
  pre-opened sockets via the host runtime")`. Same shape as
  the iter-19 process stub.
- Add cs-cli feature `wasm-stdlib-full` that bundles
  `wasm-stdlib` + `stdlib-net`.

**Procs that work on wasip2 (FR-2)**:
`tcp-connect`, `tcp-send`, `tcp-recv`, `tcp-close`, `udp-bind`,
`udp-send-to`, `udp-recv-from`, `udp-close`, `dns-resolve`.

**Procs that raise on wasip2 (FR-5)**:
`tcp-listen`, `tcp-accept`.

### cs-stdlib-http (client)

`ureq` does not build for any wasm32 target. We swap it for
**`wasi-http-client = "0.2.1"`** on the wasip2 target, keeping
ureq for native.

```toml
[dependencies]
cs-core = { workspace = true }
cs-ffi = { workspace = true }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
ureq = { version = "2", default-features = false, features = ["tls"] }
tiny_http = "0.12"

[target.'cfg(target_arch = "wasm32")'.dependencies]
wasi-http-client = "0.2.1"
```

**Source layout**: split into `client.rs` (already exists),
`server.rs` (already exists), `client_native.rs` / `client_wasi.rs`
behind cfg gates. Each module owns one impl of `(http-get …)`
and re-exports it.

**Shape**: response alist keys are the same across both impls —
`status`, `headers`, `body`. Body bytes/string handling
identical.

### cs-stdlib-http (server)

`tiny_http`'s accept-loop-on-thread model doesn't translate to
wasip2 (no `std::thread::spawn`). The wasip2 idiom is
**`wasi:http/incoming-handler`**: the runtime calls into the
WASM module per request, the module returns a response, the
runtime writes it back.

**API divergence** (FR-4, Risk 2):

Native shape — accept-loop style:
```scheme
(import (crab http server))
(define srv (http-server-bind "127.0.0.1:8080"))
(let loop ()
  (let ((req (http-server-accept srv)))
    (when req
      (http-respond req 200 '(("content-type" . "text/plain")) #vu8(72 105))))
  (loop))
```

wasip2 shape — handler-callback style:
```scheme
(import (crab http server))
(http-incoming-handler
  (lambda (req)
    ;; (req is the same shape as the native accept result)
    (values 200
            '(("content-type" . "text/plain"))
            #vu8(72 105))))
;; Module then sits in main until the runtime invokes the handler.
```

**Implementation plan**: register `http-incoming-handler` as
a Scheme proc that stashes the lambda in a thread-local
`OnceLock<Box<dyn Fn(Request) -> (i64, Vec<(String,String)>, Vec<u8>)>>`.
The component-model `wasi:http/incoming-handler/handle` export
(generated by wit-bindgen) looks up the stashed lambda + calls
the Scheme VM to run it.

The cs-cli wasm32-wasip2 build needs to declare this export at
the component level. That's done via `wit-bindgen` macros +
adding a `[lib]` section + linker arg. Details land in tasks.md.

### cs-stdlib-websocket

`tungstenite = "0.24"` is generic over `Stream: Read + Write`.
On `wasm32-wasip2` it should compile against `std::net::TcpStream`
since the latter works there. WS server requires `TcpListener`
which is deferred (FR-6).

**Plan**:
- No `Cargo.toml` changes (tungstenite-on-std).
- cfg-gate `ws_listen` + `ws_accept` to raise on wasip2 (same
  shape as `tcp_listen`).
- Conformance test exercises client only on wasip2.

## Feature graph

```
cs-cli features
├── default = [jit, ffi-dynamic, aot, stdlib]
├── wasm-stdlib = [stdlib-path, …, stdlib-meta]   (26 modules)
└── wasm-stdlib-full = [wasm-stdlib, stdlib-net,
                        stdlib-http, stdlib-websocket]  (29)
```

The umbrella `stdlib` (full native) still pulls all 29 in.

## Test plan

A wasip2 conformance run needs a real wasi runtime in CI.
**Wasmtime 28+** (pinning past the socket-read bug in 27) is
the v1 runtime.

Per-target conformance:

| Test | native | wasip1 | wasip2 |
|---|---|---|---|
| All 30 existing crab-* | ✓ | 23 of 30 (excluded: net/http/ws ×3) | new — same 23 + 4 new |
| `crab-net.scm` (TCP client + UDP + DNS) | ✓ | n/a (excluded) | ✓ (new, runs against `httpbin.org` or local server) |
| `crab-http.scm` (client GET) | ✓ | n/a | ✓ (new) |
| `crab-http-server.scm` | ✓ (accept-loop) | n/a | ✓ (incoming-handler shape) |
| `crab-websocket.scm` (client) | ✓ | n/a | ✓ (new) |
| `crab-net.scm — tcp-listen raises` | n/a | n/a | ✓ (new, asserts FfiError) |

CI matrix gains 3 jobs:
- `cargo build --target wasm32-wasip2 -p cs-cli --no-default-features --features wasm-stdlib-full`
- `wasmtime serve crabscheme.wasm` (smoke test for the
  component model export)
- conformance run: invoke crabscheme.wasm with an arg pointing
  at the per-test .scm fixture, capture exit code + stdout

## Trade-offs

1. **Runtime support narrowing.** wasip2 is Wasmtime-only-in-
   practice today. Wasmer + WasmEdge are working on wasi:sockets
   but lag by 6-12 months. Acceptable for v1; document the
   limitation, allow embedders to keep using the wasip1 build
   if they need broader runtime coverage.

2. **API divergence for HTTP server.** Real wart — Scheme code
   doing `(http-server-accept …)` on native can't run unchanged
   on wasip2. Mitigated by:
   - The Scheme-level `(crab http server)` library exporting
     both `http-server-accept` and `http-incoming-handler`.
   - A target-feature query (`(crab-target)` returning
     `"native"`, `"wasi-p1"`, or `"wasi-p2"`) so programs can
     `cond-expand`.
   - Documentation of the two shapes side-by-side.

3. **TLS trust boundary moves.** Native: rustls + webpki-roots
   inside the binary. wasip2: TLS handled by the runtime
   (Wasmtime's wasi:http impl). Document; not a security
   regression but a different threat model.

4. **Binary size budget.** wasi-http-client is small (~12 KB)
   but the wasip2 component-model adapter adds metadata.
   91 MB → estimated 95-100 MB debug for wasip2 + full
   stdlib. Acceptable per NFR-4 if we don't overshoot.

5. **WS server + TCP server gap.** Deferred. Users wanting
   "WS server in WASM" can't have it in v1. Document; revisit
   if wasi:sockets 0.3 or a `wasi:websocket` proposal lands.

## Rollback story

- The wasip2 build is a NEW feature behind a NEW feature flag
  (`wasm-stdlib-full`). Reverting this spec: delete the feature
  + delete the wasip2 CI job + delete the cfg-gated branches
  in `cs-stdlib-{net,http,websocket}`. Native + wasip1 builds
  unaffected throughout.
- API surface: any new Scheme procs (`http-incoming-handler`,
  `crab-target`) are additions only — pre-existing code keeps
  working.
- Cargo.lock impact: `wasi-http-client` becomes a transitive
  optional dep, only resolved when the wasip2 target is in use.
