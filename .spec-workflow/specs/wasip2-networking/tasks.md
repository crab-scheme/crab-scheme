# wasip2-networking — Tasks

> Status: **Draft**
> Spec slug: `wasip2-networking`
> Companion: `requirements.md`, `design.md`

Each iter is a coherent landable chunk. Estimated total scope:
**~2 weeks** of work. v1 targets sync + Wasmtime only;
multi-runtime and async are out of scope.

| # | Iter title | Adds | Depends on | Acceptance |
|---|---|---|---|---|
| 1 | Spec + ADR + CI scaffold | This spec, ADR 0020, GitHub Actions matrix gain a `wasm32-wasip2 build` job that compiles cs-cli without any of the new networking modules (just confirms the target works in CI). | — | wasip2 build job green; ADR + spec land on `main`. |
| 2 | wasm-stdlib-full feature flag + net (client side) | `cs-cli/wasm-stdlib-full` feature; `cs-stdlib-net` cfg-gates `tcp-listen` + `tcp-accept` to raise on wasip2; rest of net works via `std::net`. | Iter 1, Rust ≥ 1.83 in the dev env. | `cargo build --target wasm32-wasip2 --features wasm-stdlib-full` clean. A `wasmtime run` smoke test calls `tcp-connect` against a local netcat listener and sends a byte. |
| 3 | http client (wasi-http-client) | Target-cfg-gated `cs-stdlib-http` Cargo.toml; `client.rs` split into `client_native.rs` (ureq) + `client_wasi.rs` (wasi-http-client). Same response alist shape. | Iter 2. | `(http-get "https://example.com")` returns shape-compatible response under wasmtime 28+. New conformance test `crab-http.scm` runs on both targets. |
| 4 | websocket client | tungstenite-on-std::net works as-is on wasip2; cfg-gate `ws-listen` + `ws-accept` to raise. | Iter 2. | `(ws-connect …)` round-trips a message under wasmtime 28+. WS server raises clean error. |
| 5 | http server (incoming-handler) | New Scheme proc `http-incoming-handler`; wit-bindgen integration for `wasi:http/incoming-handler/handle` export; lib component declaration in cs-cli's wasip2 build. | Iter 3, wit-bindgen toolchain. | `wasmtime serve crabscheme.wasm` responds to `curl localhost:8080` with the response from the registered Scheme lambda. |
| 6 | Scheme shim + `(crab-target)` proc | `lib/crab/http/server.scm` exports both `http-server-accept` (native) and `http-incoming-handler` (wasip2) with a clear error when called on the wrong target; `(crab-target)` returns `"native"`/`"wasi-p1"`/`"wasi-p2"` for cond-expand. | Iter 5. | A single Scheme file can target both via cond-expand; demo in docs/examples/. |
| 7 | CI matrix + conformance harness | GitHub Actions: build wasip2-full + run wasmtime conformance for `crab-{net,http,websocket}.scm`; conformance.rs gains 4 new `#[cfg(target = wasm32-wasip2)]` test entries (the cs-cli side; the running side is on wasmtime). | Iter 5. | All 4 wasip2 conformance tests green in CI under Wasmtime 28+. |
| 8 | Exit report + docs | `docs/milestones/wasip2-networking-exit.md`; update `docs/milestones/stdlib-modules-exit.md` to reflect 29/29 portable (with caveats); per-crate README updates for the cfg gates. | Iters 2–7. | Exit report lands on main; PR squash-merges; release notes mention the new wasm-stdlib-full feature. |

## Cross-cutting

Throughout the iter sequence:

- Each iter that lands new Rust dependencies declares them at
  the workspace level in `Cargo.toml` (`wasi-http-client`,
  `wit-bindgen` if iter 5 needs it).
- Each iter that adds a Scheme proc to a module that already
  has a `README.md` updates that README.
- Each iter that flips per-target behavior adds a doc note in
  the crate's module-level `//!` rustdoc.
- CI matrix changes go in `.github/workflows/ci.yml` in the
  iter that introduces them; don't batch.

## Dependencies summary

New crates (Cargo.lock additions on wasip2 target only):

- `wasi-http-client = "0.2.1"` (iter 3)
- `wit-bindgen = "0.30"` or similar (iter 5 — version TBD by
  what Wasmtime 28's wasi:http requires)
- Possibly `cargo-component = "0.16"` as a dev-dependency (iter
  5 — the component-model build needs this to emit the right
  `.wasm` component metadata; alternatively use the
  `wasm-component-ld` linker directly)

No new native deps. Native build's `Cargo.lock` shouldn't change.

## Out of iter scope (deferred follow-ups)

These are not on the iter list — call out post-v1 work:

- WebSocket server on wasip2. Needs `tcp-listen` or a
  separate `wasi:websocket` standard; neither exists.
- TCP server (`tcp-listen` / `tcp-accept`) on wasip2. Same
  gap.
- Wasmer / WasmEdge runtime support for the networking
  features. Lags upstream; revisit when those runtimes' WASI
  coverage matches Wasmtime's.
- Browser target (`wasm32-unknown-unknown`) for the networking
  modules. Different problem space (JS bindings).
- WASI 0.3 async migration. Sync only for v1.

## Rollback story

- Iters 2–7 are pure additions behind the new `wasm-stdlib-full`
  feature. Reverting the spec: delete that feature, delete the
  cfg-gated `_wasi` modules in `cs-stdlib-http`, drop the
  wit-bindgen build glue, drop the wasip2 CI job. Native +
  wasip1 builds unaffected throughout.
- Iter 6's `(crab-target)` proc is a permanent addition even
  if rolled back (useful for cond-expand independent of this
  spec).

## Open questions to resolve before iter 2

1. **Wit-bindgen vs component-model adapter?** Iter 5
   architectural choice — does the cs-cli wasip2 build emit a
   component directly (via cargo-component) or a core module +
   adapter? Spike during iter 1.
2. **Wasmtime version pin.** Currently saying 28+ to dodge the
   socket-read bug in 27. Confirm 28 fixes it; if not, push to
   29 or 30.
3. **wasi-http-client maintenance status.** Look at issue
   tracker + PR cadence; if abandoned, consider `wstd` or
   `waki` as alternatives. Spike during iter 1.
4. **Scheme-side `cond-expand` mechanism.** R7RS has
   cond-expand; CrabScheme has it for `(srfi …)` feature
   detection. Does it cover `(crabscheme target wasi-p2)`-shape
   queries? If not, add a small extension or rely on the
   `(crab-target)` proc (iter 6).
