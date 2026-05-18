# Standard Modules — Requirements

> Status: **Draft**
> Spec slug: `stdlib-modules`
> Predecessor / sibling specs:
> - `stdlib` (M9 R6RS coverage — fills *language* gaps)
> - `ffi` (M5b — defines the host-procedure registration ABI)
> Companion ADR: TBD (will be drafted alongside design.md)

## Context

The `stdlib` spec rounded out CrabScheme's R6RS surface so Scheme
programs can rely on the published language standard. R6RS however
stops at the *language*: it has `(rnrs files)` for `file-exists?`
and `delete-file`, and that's nearly all it says about the host.
Anything beyond — directory traversal, environment variables,
process spawning, JSON, HTTP, hashing, regex, time formatting —
every CrabScheme program currently re-invents from scratch on top
of the FFI surface.

By comparison:

- **Go** ships `os`, `path/filepath`, `io`, `bufio`, `time`,
  `strings`, `strconv`, `encoding/json`, `encoding/csv`, `net/http`,
  `crypto/sha256`, `regexp`, `sort`, `log`, `fmt`, etc. — ~100
  standard packages that cover every common scripting need.
- **Python** ships `os`, `pathlib`, `shutil`, `sys`, `argparse`,
  `logging`, `json`, `csv`, `re`, `hashlib`, `urllib`,
  `http.server`, `subprocess`, `threading`, `datetime`, `time`,
  `random`, `socket`, `gzip`, `zipfile`, and dozens more.
- **Rust** ships `std::fs`, `std::io`, `std::path`, `std::env`,
  `std::process`, `std::time`, `std::thread`, `std::sync`,
  `std::collections`, `std::net`. (Crypto, JSON, HTTP, regex live
  on crates.io — but those crates are universally adopted.)
- **Haskell base** ships `Data.List`, `Data.Map`, `Data.IORef`,
  `System.IO`, `System.Environment`, `Text.Printf`,
  `Control.Concurrent`, `Control.Exception` — the always-imported
  prelude is small but the wider `base` package is the de-facto
  standard library every Haskell program uses.

CrabScheme has the infrastructure to ship the equivalent: a
working FFI (ADR 0008, the `cs-ffi` / `cs-ffi-trait` /
`cs-ffi-dynamic` features), three reference plugins
(`cs-ffi-example`, `cs-ffi-sha2`, `cs-ffi-http`), and both
static-link (rlib) and dynamic-load (cdylib) registration paths.
What's missing is a *curated, coordinated* set of modules with
consistent API design, packaged so users don't have to know which
Rust crate backs which Scheme procedure.

This spec defines that set.

---

## Goals

1. **Reduce per-program reinvention.** A new CrabScheme program
   that needs to read a config file, parse JSON, hit an HTTP
   endpoint, hash a payload, and log structured output should
   `import` four or five well-known libraries and be done — no
   FFI plumbing, no third-party crate hunt.
2. **Strong typing at the Rust boundary.** Push every operation
   that benefits from Rust's type system or ecosystem
   (PathBuf, Duration, regex::Regex, reqwest::Response,
   serde_json::Value, sha2::Sha256) down into a Rust plugin
   crate. The Scheme side sees opaque handles + conversion
   procedures, never raw pointers or unchecked indices.
3. **Predictable distribution.** A default native `crabscheme`
   build statically links the full set (one binary, no
   "first install these plugins" friction). WASM and minimal
   embed builds opt out of the host-dependent modules
   (FS, processes, HTTP server) via cargo features.
4. **R7RS library naming.** Every module is a proper R6RS / R7RS
   `(library …)` form with a stable name under the `(crab …)`
   namespace prefix (chosen to avoid colliding with `(rnrs …)`
   for the language standard and with `(scheme …)` for R7RS-WG2
   libraries). Imports look like `(import (crab fs))`,
   `(import (crab json))`, `(import (crab time))`.
5. **Documentation parity with Go.** Each module ships a
   reference doc that mirrors Go's `pkg.go.dev` style:
   one-line summary, full procedure list with type sketches,
   one usage example per group. Discoverability matters as
   much as availability.

## Non-goals

- A package manager. The `(crab …)` set ships *with* the
  CrabScheme distribution; third-party libraries continue to use
  the existing FFI dlopen path and external organization
  (e.g., `(import (someorg foo))` loaded out-of-band).
- A full async runtime. Concurrency primitives in this spec are
  the `(crab actor)` BEAM wrapper that already exists plus
  thread-safety adapters; an `asyncio`-equivalent stays out of
  scope.
- Cross-platform GUI / window-management / audio. Those belong
  in user-space plugins, not the stdlib.
- A C-level FFI generator (auto-bind `.h` files). Each `(crab
  …)` module is a hand-curated Rust plugin; we choose the
  surface deliberately rather than expose raw libc.

---

## Module catalog

Status legend: ✅ already exists (in some form);
🟡 partial / scattered;
❌ missing.

### Tier 1 — host & system (default-on native; opt-out on WASM)

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab fs)` | 🟡 R6RS file-exists? only | `std::fs`, `std::io` | `read-file-bytes`, `read-file-string`, `write-file-bytes`, `write-file-string`, `append-file`, `delete-file`, `rename-file`, `copy-file`, `directory-list`, `directory-create`, `directory-create-all`, `directory-delete`, `file-exists?`, `directory-exists?`, `file-metadata` (size / modified / mode), `with-temp-file`, `with-temp-directory` |
| `(crab path)` | ❌ | `std::path`, `camino` | `path-join`, `path-split`, `path-basename`, `path-dirname`, `path-extension`, `path-stem`, `path-normalize`, `path-absolute`, `path-relative-to`, `path-is-absolute?`, `path-components`, `path-with-extension` |
| `(crab os)` | 🟡 env scattered | `std::env`, `std::process` | `get-env`, `set-env`, `env-vars`, `current-directory`, `change-directory`, `process-id`, `parent-process-id`, `hostname`, `username`, `platform`, `architecture`, `exit`, `args` (already in `(rnrs programs)`) |
| `(crab process)` | ❌ | `std::process`, `which` | `spawn` (returns process handle), `process-wait`, `process-kill`, `process-pid`, `process-stdin`, `process-stdout`, `process-stderr`, `process-exit-code`, `which` (search PATH), `run` (convenience: spawn + wait + capture) |
| `(crab signal)` | ❌ | `signal-hook` or `nix` | `signal-install` (callback for SIGINT/SIGTERM/etc.), `signal-uninstall`, `raise-signal` |
| `(crab time)` | 🟡 `current-second` + `current-jiffy` | `std::time`, `chrono` | `current-time` (returns time object), `time->seconds`, `time->components`, `make-time`, `time-add`, `time-diff`, `sleep`, `format-time` (strftime-style), `parse-time`, `monotonic-time` |
| `(crab tty)` | ❌ | `crossterm` or `console` | `is-tty?` (stdin/stdout/stderr), `terminal-size`, `tty-color-supported?`, `clear-screen`, `move-cursor`, `read-line-interactive` (with editing), `read-password` |

### Tier 2 — text, formatting, parsing

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab string)` | 🟡 R6RS basics | (Rust core + `unicode-segmentation`) | `string-split`, `string-join`, `string-trim`, `string-trim-left`, `string-trim-right`, `string-replace`, `string-contains?`, `string-starts-with?`, `string-ends-with?`, `string-pad-left`, `string-pad-right`, `string-repeat`, `string-grapheme-count`, `string-grapheme-list` |
| `(crab regex)` | ❌ | `regex` | `regex-compile`, `regex-match?`, `regex-find`, `regex-find-all`, `regex-captures`, `regex-replace`, `regex-replace-all`, `regex-split` |
| `(crab format)` | ❌ | (Scheme-side) | Printf-style: `format-string` (~a ~s ~d ~x …), `sprintf`, `print`, `println`, `eprint`, `eprintln`. Common Lisp-style `format` is a reasonable starting point. |
| `(crab json)` | ❌ | `serde_json` | `json-read` (port / string / bytes → value), `json-write` (value → port / string / bytes), `json-pretty-write`, `json-value?`, conversions: `value->scheme`, `scheme->value` |
| `(crab csv)` | ❌ | `csv` | `csv-read` (port → list of records), `csv-read-headers`, `csv-write`, customizable separator + quote chars |
| `(crab toml)` | ❌ | `toml` | `toml-read`, `toml-write` |
| `(crab url)` | ❌ | `url` | `url-parse`, `url-join`, `url-scheme`, `url-host`, `url-port`, `url-path`, `url-query`, `url-fragment`, `url-encode`, `url-decode` |
| `(crab uuid)` | ❌ | `uuid` | `uuid-v4`, `uuid-v7`, `uuid-parse`, `uuid->string`, `uuid->bytes` |

### Tier 3 — encoding, compression, hashing

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab hash)` | 🟡 `cs-ffi-sha2` exists, ad-hoc | `sha2`, `sha1`, `md5`, `blake3`, `hmac` | `hash-sha256`, `hash-sha512`, `hash-sha1`, `hash-md5`, `hash-blake3` (each: bytes → bytes), `hmac` (key + bytes + algo → bytes), streaming variants `hash-create` / `hash-update!` / `hash-finalize` |
| `(crab base)` | ❌ | `base64`, `base32`, `hex` | `base64-encode`, `base64-decode`, `base32-encode`, `base32-decode`, `hex-encode`, `hex-decode` |
| `(crab compress)` | ❌ | `flate2`, `zstd` | `gzip-compress`, `gzip-decompress`, `deflate-compress`, `deflate-decompress`, `zstd-compress`, `zstd-decompress` |
| `(crab archive)` | ❌ | `tar`, `zip` | `tar-create`, `tar-extract`, `tar-list`, `zip-create`, `zip-extract`, `zip-list` |
| `(crab random)` | 🟡 some `random-integer` | `rand`, `getrandom` | `random-bytes` (cryptographic), `random-integer`, `random-flonum`, `random-choice`, `random-shuffle`, `make-random-source` (seeded), `random-source-state-ref` / `…-set!` |

### Tier 4 — networking

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab net tcp)` | ❌ | `std::net`, `tokio` (optional) | `tcp-connect`, `tcp-listen`, `tcp-accept`, `tcp-shutdown`, `socket-read`, `socket-write` |
| `(crab net udp)` | ❌ | `std::net` | `udp-bind`, `udp-send`, `udp-recv` |
| `(crab net dns)` | ❌ | `hickory-resolver` | `dns-resolve` (host → list of addrs), `dns-reverse` |
| `(crab http client)` | 🟡 `cs-ffi-http` exists, demo-only | `reqwest` or `ureq` | `http-get`, `http-post`, `http-put`, `http-delete`, `http-request` (full builder), `http-response-status`, `http-response-headers`, `http-response-body` |
| `(crab http server)` | ❌ | `axum`, `hyper` | `http-server-new`, `http-route-add!`, `http-server-listen`, `http-server-stop`, request/response builder procedures |
| `(crab websocket)` | ❌ | `tokio-tungstenite` | `ws-connect`, `ws-send`, `ws-recv`, `ws-close` |

### Tier 5 — data structures & algorithms

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab collection queue)` | ❌ | `std::collections::VecDeque` | `queue-new`, `queue-push!`, `queue-pop!`, `queue-peek`, `queue-length`, `queue-empty?` |
| `(crab collection heap)` | ❌ | `std::collections::BinaryHeap` | `heap-new`, `heap-push!`, `heap-pop!`, `heap-peek`, `heap-length` |
| `(crab collection set)` | ❌ | `std::collections::HashSet` / `BTreeSet` | `set-new`, `set-add!`, `set-remove!`, `set-contains?`, `set-union`, `set-intersect`, `set-difference`, `set->list` |
| `(crab collection map)` | 🟡 R6RS hashtables | `std::collections::BTreeMap` | Ordered-map operations: `omap-new`, `omap-set!`, `omap-ref`, `omap-keys` (sorted), `omap-range` |
| `(crab sort)` | ✅ `list-sort` / `vector-sort` exist | — | (No new work; documented under stdlib namespace.) |

### Tier 6 — observability & diagnostics

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab log)` | ❌ | `tracing` + `tracing-subscriber` | `log-trace`, `log-debug`, `log-info`, `log-warn`, `log-error`, `log-with-fields`, `log-set-level!`, `log-set-format!` (json / pretty / compact) |
| `(crab metrics)` | ❌ | `metrics` | `counter-new`, `counter-increment!`, `gauge-new`, `gauge-set!`, `histogram-new`, `histogram-observe!` |
| `(crab trace)` | ❌ | `tracing` | `span-enter`, `span-exit`, `span-with` (with-handler-style) |

### Tier 7 — math extensions

| Library | Status | Backing crate(s) | Primary procedures |
|---|---|---|---|
| `(crab math)` | 🟡 R6RS arithmetic | `libm` / Rust intrinsics | `math-erf`, `math-gamma`, `math-lgamma`, `math-bessel`, `math-hypot`, `math-cbrt` — coverage beyond R6RS basics |
| `(crab math bigint)` | ✅ `num-bigint` already wired | — | Document under stdlib namespace. |
| `(crab math stats)` | ❌ | `statrs` | `mean`, `median`, `variance`, `stddev`, `percentile`, distributions (normal / poisson / binomial / etc.) |

---

## Functional requirements

### FR-1. Catalog completion

Every Tier 1 + Tier 2 library above is implemented, documented,
tested with at least one positive + one negative example per
public procedure, and importable as `(import (crab …))` from any
CrabScheme program built with default features.

Acceptance: a smoke program that uses **every Tier-1 library**
from a single file (`tests/conformance/stdlib-modules/tier1.scm`)
runs to completion on `--tier walker`, `--tier vm`, and
`--tier vm-jit`, exit code 0.

### FR-2. Module discoverability

`(import (crab))` exposes a meta-procedure `(crab-list-modules)`
that returns the alphabetical list of every available `(crab …)`
library on the current build. `(crab-module-info 'fs)` returns
an association list `((procedures . (read-file-bytes …)) (loaded
. #t) (origin . static|dynamic|missing))`.

Acceptance: the smoke test above asserts the listing matches the
build's compiled-in set.

### FR-3. Strong Rust-side types

Every operation whose Rust counterpart has a non-primitive type
exposes that type as an opaque Scheme value (`record-type` or
opaque pointer guard), not as a serialized string or untyped
fixnum.

Examples:

- `(crab path)` exposes `path?` values backed by `Box<PathBuf>`,
  not raw strings. `path->string` converts on demand.
- `(crab time)` exposes `instant?` and `duration?` values backed
  by `Box<std::time::Instant>` / `Box<Duration>`.
- `(crab regex)` exposes `regex?` values backed by `Box<Regex>` —
  compiled once, matched many times.
- `(crab json)` exposes `json-value?` recursively (or maps to
  Scheme `Value` directly — design.md decides).

Acceptance: each module's tests construct a typed value, pass
it through 2+ procedures, and assert type-predicate true at
each step.

### FR-4. Graceful per-feature disable

Embed targets that can't support a module disable it via cargo
feature. The remaining modules continue to import, and
`(crab-module-info 'fs)` reports `(origin . missing)` for the
disabled set. No runtime errors for *importing* a missing module
— only for *calling* it.

Acceptance: a WASM build (`--no-default-features --features
ffi-trait,stdlib-text,stdlib-encoding`) compiles cleanly and
runs the Tier-2/Tier-3 portion of the smoke test; the Tier-1
portion reports each missing module gracefully.

### FR-5. Streaming where it matters

File / network / compression APIs all expose a streaming variant
in addition to the slurp-style convenience. `read-file-bytes`
slurps; `with-file-input-port` / `port-read-bytevector!` stream.
Same shape for compression (`gzip-stream-create` /
`gzip-stream-write!` /  `gzip-stream-finalize`) and HTTP
(`http-response-body-stream` returns a port).

Acceptance: a 1 GB synthetic file processes end-to-end through
`gzip-compress` + `gzip-decompress` (streaming form) without
RSS exceeding the file's chunk-size × constant factor.

### FR-6. Error model parity

Every stdlib procedure raises R6RS `&condition` values for
recoverable errors (file-not-found, network timeout, JSON parse
error, etc.), not Rust `Err(_)` strings. The condition hierarchy
mirrors the originating subsystem:

```
&condition
└── &stdlib
    ├── &fs (file-not-found, permission-denied, …)
    ├── &net (connection-refused, timeout, dns-resolution, …)
    ├── &encoding (json-parse, csv-parse, …)
    └── &os (process-spawn-failed, signal-not-handled, …)
```

Acceptance: every documented error mode has a test asserting the
right `&stdlib`-rooted condition type fires.

### FR-7. Bench inclusion

The realworld bench harness gets at least one
`(crab …)`-exercising bench (e.g., a JSON parse round-trip, a
fs+regex grep, an HTTP echo) to anchor the perf story.

Acceptance: `bench/realworld/schemes/stdlib_json.scm` and at
least one other land green on `--tier vm` and `--tier vm-jit`
in the realworld bench JSONL.

---

## Non-functional requirements

### NFR-1. One Rust crate per `(crab …)` module group

`(crab fs)` → `crates/cs-stdlib-fs/`. `(crab json)` →
`crates/cs-stdlib-json/`. Each crate is a Rust library that
exposes a single `pub fn register(rt: &mut Runtime)` entry point
following the existing `cs-ffi-sha2` pattern (and matches the
`HostProcedure` trait for compile-time registration; the dlopen
path falls out for free per ADR 0008 D-1/D-2).

### NFR-2. Default native build static-links the full Tier-1
through Tier-6 set.

Users do not have to `(load-shared-library …)` to use any `(crab
…)` module on a default build. The `cs-cli` build's
`Cargo.toml` lists every Tier-1–6 crate as a non-optional
dependency on native targets.

### NFR-3. WASM build size kept reasonable

WASM builds default to the Tier-2 (text/format) + Tier-3
(encoding/hash) + Tier-5 (collection) subset, which uses no
host I/O. Tier-1 (fs/os/process/tty/signal) and Tier-4 (net)
are opt-out by default and opt-in via explicit cargo feature
flags — but if the embedder turns them on for WASM, they
compile with `wasm32-wasip1` shims that delegate to WASI calls
where possible and raise `&stdlib-not-supported` otherwise.

### NFR-4. Scheme-side wrappers in `lib/crab/`

Each module ships a thin Scheme wrapper at `lib/crab/<name>.scm`
that:

1. Imports the runtime-registered Rust builtins (each prefixed
   `__crab-<module>-<proc>` to avoid name collision).
2. Re-exports under the clean R6RS library form `(crab <name>)`
   with the documented names.
3. Adds any pure-Scheme convenience procedures that don't need
   Rust backing (e.g., `path-with-extension` can be Scheme-side
   if `path-split` + `path-with-name` exist as builtins).

`Runtime::new` loads every `lib/crab/*.scm` automatically on
startup — same pattern as `lib/beam/prelude.scm` today.

### NFR-5. Reference docs co-located with each module

`crates/cs-stdlib-<name>/README.md` is the procedure reference:
one-paragraph summary, full procedure list with type sketches,
one usage example per logical group. The top-level
`lib/crab/README.md` is the module index linking to each.

Authoritative source for embedders, examples for IDE LSPs to
surface inline, and reduces the support burden of "is there a
way to do X in CrabScheme?".

### NFR-6. Per-module benchmarks

Each crate `crates/cs-stdlib-<name>/` ships a `benches/`
directory exercising its hot path against the native Rust crate
it wraps. The goal is to keep the per-call FFI overhead bounded
(measured in nanoseconds) so the stdlib doesn't become a perf
trap for users who'd otherwise reach for the raw Rust crate
from an embedder.

Target overhead: ≤ 100 ns per `(crab …)` call vs the
equivalent direct Rust call, for procedures whose body is
≤ 1 µs of work. Procedures whose body is ≥ 10 µs (filesystem,
network, compression) get an unconstrained overhead budget.

### NFR-7. No `unsafe` outside the FFI boundary

The per-crate FFI registration code is the only place `unsafe`
is permitted (and only for the well-defined ABI per ADR 0012
D-2). Everything else in the crate is safe Rust.

### NFR-8. Conformance discipline

Each module's conformance test file is written *before*
implementation, in the existing `tests/conformance/`-style
shape: one positive + one negative case per public procedure,
asserted with the `__test-summary__` harness so the count
shows up in CI rollups.

---

## Out of scope

| Item | Where it lives |
|---|---|
| Package manager (`crab install foo`) | Future ecosystem work |
| GUI / windowing | User-space plugins |
| Audio / video | User-space plugins |
| Database drivers (SQLite, Postgres) | Phase 2 candidate, not core |
| Async runtime (asyncio-equivalent) | Out of scope; concurrency stays single-threaded + BEAM actor system |
| C `.h` auto-binder | Out of scope; each module hand-curated |
| LSP for `(crab …)` | Future tooling work |

---

## Plan order (high level)

1. **Iter 1** — this requirements.md, design.md, ADR.
2. **Iter 2** — `(crab path)` + `(crab fs)` (host-IO foundation; both Tier-1; small Rust surface).
3. **Iter 3** — `(crab os)` + `(crab process)` (build on iter 2).
4. **Iter 4** — `(crab string)` + `(crab format)` + `(crab regex)` (Tier-2 essentials).
5. **Iter 5** — `(crab time)` + `(crab random)` + `(crab uuid)`.
6. **Iter 6** — `(crab json)` + `(crab csv)` + `(crab toml)` + `(crab base)` (Tier-3 encoding).
7. **Iter 7** — `(crab hash)` + `(crab compress)` (Tier-3 binary).
8. **Iter 8** — `(crab log)` + `(crab metrics)` (Tier-6 observability).
9. **Iter 9** — `(crab net tcp)` + `(crab net dns)` + `(crab http client)` (Tier-4).
10. **Iter 10** — `(crab http server)` + `(crab websocket)` (Tier-4 server).
11. **Iter 11** — `(crab collection …)` + `(crab math …)` extensions (Tier-5 + Tier-7).
12. **Iter 12** — `(crab tty)` + `(crab signal)` (Tier-1 leftovers).
13. **Iter 13** — exit report, bench inclusion, WASM-subset validation.

Each iter is a coherent landable chunk; the ordering puts the
host-foundation pieces first so later modules can build on them
(e.g., `(crab log)` writes to fs by default; `(crab http
client)` uses `(crab url)`).
