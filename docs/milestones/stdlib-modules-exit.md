# stdlib-modules exit report

Closes the batteries-included `(crab …)` module suite described in
`.spec-workflow/specs/stdlib-modules/{requirements,design,tasks}.md`,
branch `stdlib-modules-spec`. Scope is **15 iters reachable**:
spec + 26 functional modules + meta + bench/WASM closeout, all
landed on the spec's planned scope.

## What shipped

### Iter summary

| Iter | Title | Status | Key landed |
|------|-------|--------|------------|
| 1  | Spec + ADR | ✓ | requirements / design / tasks under `.spec-workflow/specs/stdlib-modules/` |
| 2  | `(crab path)` + `(crab fs)` | ✓ | `cs-stdlib-path`, `cs-stdlib-fs`; 20 + 28 procs |
| 3  | `(crab os)` + `(crab process)` | ✓ | env vars, args, hostname, spawn, wait, exit-code |
| 4  | `(crab string)` + `(crab format)` + `(crab regex)` | ✓ | unicode case, printf-style, `regex` crate |
| 5  | `(crab time)` + `(crab random)` + `(crab uuid)` | ✓ | chrono, rand, uuid-v4 |
| 6  | `(crab json/csv/toml/base/url)` | ✓ | encoding cluster — five codecs round-trip |
| 7  | `(crab hash/compress/archive)` | ✓ | sha2/sha1/md5, flate2 (gzip+zlib+deflate+zstd), tar |
| 8  | `(crab log)` + `(crab metrics)` | ✓ | tracing subscriber + metrics counters/gauges/histograms |
| 9  | `(crab net tcp/udp/dns)` | ✓ | std-sockets-only; TCP echo, UDP send/recv, DNS resolve |
| 10 | `(crab http client)` + `(crab websocket)` | ✓ | ureq (sync, no tokio), tungstenite |
| 11 | `(crab http server)` | ✓ | tiny_http; lifecycle + error-shape tests (E2E deferred — see Follow-up) |
| 12 | `(crab collection)` + `(crab math)` ext | ✓ | queue/heap/set/map, stats (mean/median/stddev) |
| 13 | `(crab tty)` + `(crab signal)` | ✓ | terminal-size, signal-hook poll API |
| 14 | `(crab)` meta + introspection | ✓ | `crab-list-modules` + `crab-module-procedures`, runtime-derived manifest |
| 15 | Bench inclusion + WASM-subset + exit | ✓ | crab-json + crab-fs realworld benches; `wasm-stdlib` feature; this doc |

### Rust crates (28 new + 1 reorganized)

```
crates/cs-stdlib-path        ─┐
crates/cs-stdlib-fs           │  Tier 1: filesystem + OS
crates/cs-stdlib-os           │
crates/cs-stdlib-process     ─┘
crates/cs-stdlib-string      ─┐
crates/cs-stdlib-format       │  Tier 2: text
crates/cs-stdlib-regex       ─┘
crates/cs-stdlib-time        ─┐
crates/cs-stdlib-random       │  Tier 3: time + random + id
crates/cs-stdlib-uuid        ─┘
crates/cs-stdlib-json        ─┐
crates/cs-stdlib-csv          │
crates/cs-stdlib-toml         │  Tier 4: encoding
crates/cs-stdlib-base         │
crates/cs-stdlib-url         ─┘
crates/cs-stdlib-hash        ─┐
crates/cs-stdlib-compress     │  Tier 5: binary data
crates/cs-stdlib-archive     ─┘
crates/cs-stdlib-log         ─┐
crates/cs-stdlib-metrics     ─┘  Tier 6: observability
crates/cs-stdlib-net         ─┐
crates/cs-stdlib-http         │  Tier 7: networking
crates/cs-stdlib-websocket   ─┘
crates/cs-stdlib-collection  ─┐
crates/cs-stdlib-math         │  Tier 8: data structures + math
crates/cs-stdlib-tty          │  + remaining tier-1 leftovers
crates/cs-stdlib-signal      ─┘
crates/cs-stdlib-meta            Iter 14: introspection / `(crab)`
```

`cs-ffi-sha2` and `cs-ffi-http` (the M10 W-track demo crates)
remain in-tree — `cs-stdlib-hash` and `cs-stdlib-http` were
written as fresh implementations rather than migrations, so the
existing CLIs (`cs-cli-sha2`) keep working without churn.

### Conformance

Per-module conformance test in `tests/conformance/foundation/crab-*.scm`
(one file per module). Wired into `crates/cs-cli/tests/conformance.rs`
behind `#[cfg(feature = "stdlib-<name>")]`. All 29 `conformance_crab_*`
tests pass with the default `stdlib` umbrella feature on; full
146-test conformance suite green.

| Module | Test asserts |
|--------|--------------|
| path   | 18 |
| fs     | 12 |
| os     | 8  |
| process| 10 |
| string | 14 |
| format | 11 |
| regex  | 14 |
| time   | 13 |
| random | 9  |
| uuid   | 7  |
| json   | 19 |
| csv    | 12 |
| toml   | 8  |
| base   | 8  |
| url    | 11 |
| hash   | 16 |
| compress | 9 |
| archive | 8 |
| log    | 5  |
| metrics| 9  |
| net    | 10 |
| http (client) | 7 |
| http (server) | 6 |
| websocket | 5 |
| collection | 16 |
| math   | 12 |
| tty    | 4  |
| signal | 5  |
| meta   | 20 |

### Realworld benches (iter 15)

Two new benches in `bench/realworld/schemes/` exercising stdlib
crates inside the timing harness:

- **`crab-json`** — builds 200 record-shaped alists,
  stringifies + parses back, asserts round-trip count. p50
  ≈ 0.2 ms on vm tier.
- **`crab-fs`** — write + read + append + read + delete cycle
  on a 1850-byte payload. p50 ≈ 0.2 ms on vm tier.

Chez parity (`check-result-vs-chez.sh`) reports "chez failed"
for both — expected, Chez has no `(crab …)`. The runner's JSONL
output is what the spec gates on.

### WASM-subset build (iter 15)

`cs-cli` now defines a `wasm-stdlib` convenience feature pulling
in the 20 WASM-safe modules:

```
path, fs, string, format, regex, time, random, uuid,
json, csv, toml, base, url, hash, archive,
log, metrics, collection, math, meta
```

Build:

```
cargo build --target wasm32-wasip1 -p cs-cli \
  --no-default-features --features wasm-stdlib
```

Produces `target/wasm32-wasip1/debug/crabscheme.wasm` (86 MB
debug, smaller in release).

**Excluded** (and why):

- `os` — `gethostname` crate doesn't compile for `wasm32-wasip1`
  (Unix-only platform module).
- `process` — `std::process::Command::spawn` is unimplemented in
  WASI preview 1.
- `net` / `http` / `websocket` — no real socket support; ureq +
  tiny_http + tungstenite all need `std::net::TcpStream`.
- `signal` — `signal-hook` is Unix-only. We do have a Windows
  stub but no WASI stub; `signal-watch!` would silently no-op.
- `tty` — `terminal-size` reads IOCTLs.
- `compress` — `zstd-sys` triggers `cc-rs` clang invocation with
  flags (`-fzero-call-used-regs`) that nix-wrapped clang rejects
  for `wasm32-wasip1`. flate2 alone works, but it's bundled into
  the compress crate alongside zstd; splitting them out is a
  trivial follow-up (#TBD).

### Cross-cutting cleanup landed in iter 15

A pre-existing bug in iter-2-onward wiring was found and fixed
while validating the WASM build: each `cs-cli/stdlib-<name>`
feature was forwarding to `cs-runtime/stdlib` (the umbrella),
which transitively pulled in every other stdlib crate. Subset
embeds (the entire raison d'être of per-module flags) thus
never actually worked — enabling `stdlib-path` on cs-cli pulled
in ureq, tungstenite, zstd-sys, etc.

Fix: per-module cs-cli features now forward only to their
matching `cs-runtime/stdlib-X`. `Runtime::register_stdlib` is
no longer gated on the umbrella — its body compiles to no-ops
when no `stdlib-X` is enabled, so the function exists
unconditionally and per-module gating works as documented.

## Architecture decisions

The spec's "Rust crate per module" decision held up across all
26 functional modules. Three patterns recurred enough to call
out:

### Pattern 1 — Fixnum-handle slabs for opaque values

Modules that needed to expose Rust state (regex compiled
patterns, hashtable iterators, signal queues, http server
handles) used a `thread_local!(RefCell<HashMap<i64, T>>)` slab
keyed by a monotonically-increasing fixnum. Scheme code holds
the fixnum, Rust resolves it on each call. This sidestepped
needing `Value::Opaque` (which is still deferred) and matches
the runtime's single-threaded execution model.

### Pattern 2 — Sync wrappers over async ecosystem crates

For `(crab http)` (ureq instead of reqwest) and
`(crab http server)` (tiny_http instead of axum) we picked
sync crates explicitly. Rationale: cs-runtime is
single-threaded; concurrency comes from the BEAM actor system
in iter 11's design. Pulling in tokio for HTTP would have
required cross-runtime bridging that wasn't worth the cost
for batteries-included.

### Pattern 3 — Feature unification via the meta crate

`cs-stdlib-meta` introspects which sibling crates are
compiled in. The wiring is non-obvious: cs-runtime's
`stdlib-<name>` feature also enables
`cs-stdlib-meta?/meta-<name>` via the `?` optional-dep
syntax. Result: meta's `crab-list-modules` returns exactly
the registered set, even for subset embeds.

## Follow-up (post-1.0)

Three items deferred during the iter sequence; none block 1.0:

- **HTTP server E2E test** — iter 11 ships lifecycle + error
  shape tests but not a full request/response round-trip
  through a curl client. Sketched: a separate process to drive
  the client side (Scheme runtime is single-threaded).
- **Split flate2 out of `cs-stdlib-compress`** — would let
  WASM users get gzip without inheriting the zstd-sys
  cross-compile issue. ~30-min fix.
- **WASI stub for `(crab signal)`** — currently Unix-only with
  a Windows stub; same shape for WASI.

## Closing state

- `stdlib-modules-spec` branch head: this commit.
- 28 new crates + 29 conformance tests + 2 realworld benches +
  1 WASM build matrix entry.
- All 146 conformance tests green on native.
- WASM build green with `wasm-stdlib` feature; 20 of 27
  modules portable.
- Default cs-cli build unchanged in behaviour — every module
  the umbrella was advertising before iter 15 is still
  enabled; we just stopped enabling them transitively when an
  embedder asked for a subset.
