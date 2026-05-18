# Standard Modules — Design

> Status: **Draft**
> Companion: `requirements.md`
> Anchor ADRs: 0008 (FFI design), 0014 (countable memory),
> 0015 (unified memory management), 0017 (escape analysis)

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│ Scheme program                                          │
│  (import (crab fs))                                     │
│  (read-file-string "config.toml")                       │
└───────────────────────────┬─────────────────────────────┘
                            │ R6RS library import
┌───────────────────────────▼─────────────────────────────┐
│ lib/crab/fs.scm                                         │
│  Re-exports + Scheme convenience procedures over the    │
│  registered Rust builtins.                              │
│  (define (read-file-string path) (__crab-fs-read-string │
│     (path->string path)))                               │
└───────────────────────────┬─────────────────────────────┘
                            │ host-procedure dispatch
┌───────────────────────────▼─────────────────────────────┐
│ Runtime (cs-runtime)                                    │
│  Procedure registry — looks up `__crab-fs-read-string`  │
│  to the registered Rust closure.                        │
└───────────────────────────┬─────────────────────────────┘
                            │ HostProcedure trait
┌───────────────────────────▼─────────────────────────────┐
│ crates/cs-stdlib-fs (Rust)                              │
│  pub fn register(rt: &mut Runtime) {                    │
│    rt.register_host_procedure("__crab-fs-read-string",  │
│      Arc::new(ReadStringProc));                         │
│    …                                                    │
│  }                                                      │
│  Each Proc is a thin wrapper over std::fs.              │
└─────────────────────────────────────────────────────────┘
```

Three layers, three concerns:

1. **Scheme layer** (`lib/crab/<name>.scm`) — R6RS library form,
   user-facing names, pure-Scheme convenience procedures, no
   knowledge of FFI mechanics.
2. **Registry layer** (`cs-runtime` + `cs-ffi`) — already in
   place per ADR 0008. The `(crab …)` modules are HostProcedure
   producers; the runtime is a HostProcedure consumer.
3. **Backing layer** (`crates/cs-stdlib-<name>`) — one Rust crate
   per module group, wraps the well-known Rust crate for the
   domain, exposes a single `register(&mut Runtime)` entry
   point.

## Crate layout

```
crates/
├── cs-stdlib-fs/        # (crab fs)
│   ├── Cargo.toml
│   ├── README.md        # procedure reference
│   ├── benches/
│   │   └── read_write.rs
│   ├── src/
│   │   ├── lib.rs       # register() + ProcVal exports
│   │   ├── procs.rs     # one fn per Scheme procedure
│   │   └── types.rs     # opaque Scheme value wrappers
│   └── tests/
│       └── smoke.rs     # Rust-side smoke (Scheme conformance lives in tests/conformance/)
├── cs-stdlib-path/      # (crab path)
├── cs-stdlib-os/        # (crab os)
├── cs-stdlib-process/   # (crab process)
├── cs-stdlib-time/      # (crab time)
├── cs-stdlib-tty/       # (crab tty)
├── cs-stdlib-signal/    # (crab signal)
├── cs-stdlib-string/    # (crab string)
├── cs-stdlib-regex/     # (crab regex)
├── cs-stdlib-format/    # (crab format) — pure Scheme; no Rust crate
├── cs-stdlib-json/      # (crab json)
├── cs-stdlib-csv/       # (crab csv)
├── cs-stdlib-toml/      # (crab toml)
├── cs-stdlib-url/       # (crab url)
├── cs-stdlib-uuid/      # (crab uuid)
├── cs-stdlib-hash/      # (crab hash) — replaces cs-ffi-sha2
├── cs-stdlib-base/      # (crab base) — base64/32/hex
├── cs-stdlib-compress/  # (crab compress)
├── cs-stdlib-archive/   # (crab archive)
├── cs-stdlib-random/    # (crab random)
├── cs-stdlib-net/       # (crab net tcp|udp|dns) — one crate, three sub-modules
├── cs-stdlib-http/      # (crab http client|server) — replaces cs-ffi-http
├── cs-stdlib-websocket/ # (crab websocket)
├── cs-stdlib-collection/# (crab collection queue|heap|set|map) — one crate
├── cs-stdlib-log/       # (crab log)
├── cs-stdlib-metrics/   # (crab metrics)
├── cs-stdlib-math/      # (crab math + math stats)
└── cs-stdlib-meta/      # (crab) meta module — `crab-list-modules`, `crab-module-info`
```

`cs-stdlib-meta` is special: it depends on every other
`cs-stdlib-*` crate (so it can detect which were compiled in),
re-exports their `register` functions, and provides the
introspection procedures from FR-2.

## Library naming

```scheme
(crab fs)        ; not (rnrs fs) — we don't re-define R6RS
(crab path)
(crab json)
(crab net tcp)   ; multi-word namespace for sub-modules
(crab http client)
(crab collection queue)
```

Rationale for `(crab …)`:

- **Distinct from `(rnrs …)`** so users see immediately whether a
  procedure is R6RS-portable or CrabScheme-specific.
- **Distinct from `(scheme …)`** (R7RS-WG2 reserves that
  namespace).
- **Single short prefix** instead of per-vendor noise like
  `(crabscheme stdlib fs)`.
- **Matches the project name** — discoverable from the binary
  name.

For backward compat with the existing `(cs …)` (BEAM prelude
ships under no prefix — `(import (beam …))` resolves through
`lib/beam/prelude.scm`), we keep `(crab …)` strictly additive.

## Procedure naming inside Rust

Each registered Rust procedure carries a `__crab-<module>-<name>`
symbol the Scheme wrapper imports. The double-underscore prefix
matches the existing convention for runtime-internal names
(`__test-summary__`, `__beam-internal-…`) and signals "don't
call this directly; use the library wrapper".

The Scheme wrapper at `lib/crab/<module>.scm` re-exports each
under the clean name:

```scheme
(library (crab fs)
  (export read-file-string write-file-string …)
  (import (rnrs base) (rnrs io ports))

  (define (read-file-string path)
    (__crab-fs-read-string (->string path)))

  (define (write-file-string path content)
    (__crab-fs-write-string (->string path) content))
  …)
```

Users `(import (crab fs))` and see clean names; the
`__crab-fs-*` registry pollutes only the implementation layer.

## Opaque Scheme values

The recurring shape: a Rust crate has a type that benefits from
the Rust ecosystem (`PathBuf`, `Regex`, `Instant`, `Duration`,
`reqwest::Response`). The Scheme side needs to hold one of these
across multiple calls without flattening it to a string.

Pattern (mirrors the existing `Port` / `Hashtable` machinery in
`cs-core/src/value.rs`):

```rust
// cs-stdlib-path/src/types.rs
use std::path::PathBuf;
use cs_core::{Value, OpaquePayload};

pub struct PathPayload(pub PathBuf);

impl OpaquePayload for PathPayload {
    fn type_name(&self) -> &'static str { "path" }
}

pub fn path_to_value(p: PathBuf) -> Value {
    Value::Opaque(Gc::new(Box::new(PathPayload(p))))
}

pub fn value_to_path(v: &Value) -> Result<&PathBuf, String> {
    match v {
        Value::Opaque(g) => g
            .as_any()
            .downcast_ref::<PathPayload>()
            .map(|p| &p.0)
            .ok_or_else(|| "expected path".into()),
        _ => Err("expected path".into()),
    }
}
```

`Value::Opaque(Gc<Box<dyn OpaquePayload>>)` is a small extension
of the existing `Value` enum — design.md needs an ADR amendment
to ratify. Alternative: keep using `Value::Port` for ports and
`Value::Promise` for futures and add a small set of variants for
the most common stdlib types (Path, Instant, Regex). This avoids
introducing a new variant. Pick one in the ADR.

## Registration plumbing

```rust
// crates/cs-stdlib-meta/src/lib.rs
pub fn register_all(rt: &mut Runtime) {
    #[cfg(feature = "stdlib-fs")]
    cs_stdlib_fs::register(rt);
    #[cfg(feature = "stdlib-path")]
    cs_stdlib_path::register(rt);
    #[cfg(feature = "stdlib-os")]
    cs_stdlib_os::register(rt);
    … // one per module
}
```

`cs-runtime::Runtime::new` calls `register_all` (under
`#[cfg(feature = "stdlib")]`, default-on for native), then loads
the matching `lib/crab/*.scm` files. The
`(crab-list-modules)` introspection from FR-2 reads a static
manifest built up at registration time:

```rust
// in each cs-stdlib-<name>/src/lib.rs
pub fn register(rt: &mut Runtime) {
    rt.declare_stdlib_module("fs", &[
        "read-file-bytes",
        "read-file-string",
        "write-file-bytes",
        …
    ]);
    rt.register_host_procedure("__crab-fs-read-string", …);
    …
}
```

`Runtime::declare_stdlib_module` populates a `BTreeMap<&'static
str, &'static [&'static str]>` that `crab-list-modules` reads.
For dlopen-loaded stdlib modules, the same machinery works — the
plugin's `crabscheme_register` entry point calls
`declare_stdlib_module` through the C-ABI surface.

## Workspace Cargo manifest

```toml
# Cargo.toml
[workspace]
members = [
  …existing…
  "crates/cs-stdlib-fs",
  "crates/cs-stdlib-path",
  "crates/cs-stdlib-os",
  …
  "crates/cs-stdlib-meta",
]

[workspace.dependencies]
cs-stdlib-fs       = { path = "crates/cs-stdlib-fs" }
cs-stdlib-path     = { path = "crates/cs-stdlib-path" }
…
cs-stdlib-meta     = { path = "crates/cs-stdlib-meta" }

# Backing crates pinned at workspace level.
serde      = { version = "1", features = ["derive"] }
serde_json = "1"
regex      = "1"
chrono     = "0.4"
reqwest    = { version = "0.12", default-features = false, features = ["rustls-tls"] }
tracing    = "0.1"
…
```

```toml
# crates/cs-runtime/Cargo.toml additions
[features]
default = ["jit", "ffi-dynamic", "regions", "stdlib"]
stdlib = [
    "dep:cs-stdlib-meta",
    "stdlib-fs", "stdlib-path", "stdlib-os", "stdlib-process",
    "stdlib-time", "stdlib-tty", "stdlib-signal",
    "stdlib-string", "stdlib-regex", "stdlib-format",
    "stdlib-json", "stdlib-csv", "stdlib-toml", "stdlib-url", "stdlib-uuid",
    "stdlib-hash", "stdlib-base", "stdlib-compress", "stdlib-archive", "stdlib-random",
    "stdlib-net", "stdlib-http", "stdlib-websocket",
    "stdlib-collection", "stdlib-log", "stdlib-metrics",
    "stdlib-math",
]
# Per-module opt-out: e.g. `--no-default-features --features
# jit,ffi-dynamic,regions,stdlib-json` ships an embedder with only
# JSON.
stdlib-fs       = ["cs-stdlib-meta/stdlib-fs"]
stdlib-path     = ["cs-stdlib-meta/stdlib-path"]
…
```

This shape lets:

- Default native: one `cargo build` → every module compiled in.
- Minimal embed: `--no-default-features --features
  jit,ffi-dynamic,regions,stdlib-json,stdlib-time` ships just
  those two.
- WASM: `--no-default-features --features
  ffi-trait,stdlib-string,stdlib-format,stdlib-json,stdlib-encoding,…`
  ships the host-independent set.

## Scheme-side library wrappers

The wrappers live at `lib/crab/<name>.scm`. They're loaded
automatically by `Runtime::new`, mirroring the existing
`lib/beam/prelude.scm` loading path:

```rust
// cs-runtime/src/lib.rs (snippet)
#[cfg(feature = "stdlib")]
fn load_stdlib_wrappers(rt: &mut Runtime) {
    // Order matters: path < fs (fs uses path); url < http (http uses url).
    for module in &[
        "path", "fs", "os", "process", "time", "tty", "signal",
        "string", "format", "regex",
        "base", "hash", "random", "uuid",
        "json", "csv", "toml", "url",
        "compress", "archive",
        "net-tcp", "net-udp", "net-dns",
        "http-client", "http-server", "websocket",
        "collection",
        "log", "metrics",
        "math", "math-stats",
    ] {
        let path = format!("lib/crab/{}.scm", module);
        let src = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            // Missing wrapper is a build error in CI; at runtime
            // it just means the module isn't shipped. Skip.
            String::new()
        });
        if !src.is_empty() {
            rt.eval_str_via_vm(&path, &src).expect("stdlib wrapper load");
        }
    }
}
```

Per NFR-4, this matches what `lib/beam/prelude.scm` does today —
the BEAM library has already paved the path.

### Wrapper file shape

```scheme
; lib/crab/fs.scm
(library (crab fs)
  (export
    read-file-bytes  read-file-string
    write-file-bytes write-file-string
    append-file
    delete-file rename-file copy-file
    directory-list directory-create directory-create-all
    directory-delete
    file-exists? directory-exists?
    file-metadata
    with-temp-file with-temp-directory)

  (import (rnrs base)
          (rnrs io ports)
          (rnrs records syntactic)
          (crab path))

  ; --- thin wrappers over Rust-side builtins ---

  (define (read-file-bytes p)
    (__crab-fs-read-bytes (path->string p)))

  (define (read-file-string p)
    (__crab-fs-read-string (path->string p)))

  (define (write-file-bytes p bv)
    (__crab-fs-write-bytes (path->string p) bv))

  (define (write-file-string p s)
    (__crab-fs-write-string (path->string p) s))

  ; --- pure-Scheme convenience built on the above ---

  (define-record-type file-metadata
    (fields size modified mode))

  (define (with-temp-file proc)
    (let ((path (__crab-fs-temp-file)))
      (dynamic-wind
        (lambda () #f)
        (lambda () (proc path))
        (lambda () (when (file-exists? path) (delete-file path)))))))
```

Pure-Scheme convenience (`with-temp-file`, `with-temp-directory`)
lives in the wrapper. Operations that need the Rust ecosystem
(`__crab-fs-read-bytes`) come from the registered builtin.

## Error model

Each Rust builtin that can fail returns a `Result<Value, Value>`
where the error arm is a fully-constructed R6RS condition value:

```rust
// crates/cs-stdlib-fs/src/procs.rs
pub fn read_string(args: &[Value]) -> Result<Value, Value> {
    let path = args[0].as_string().ok_or_else(|| string_type_err())?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Value::String(Rc::new(RefCell::new(s)))),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Err(fs_condition("file-not-found", &path, &e.to_string()))
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            Err(fs_condition("permission-denied", &path, &e.to_string()))
        }
        Err(e) => Err(fs_condition("io-error", &path, &e.to_string())),
    }
}

fn fs_condition(kind: &str, path: &str, msg: &str) -> Value {
    cs_core::make_compound_condition(&[
        cs_core::condition("&stdlib"),
        cs_core::condition("&fs"),
        cs_core::condition("&" .to_string() + kind),
        cs_core::condition_message(msg),
        cs_core::condition_irritants(vec![Value::String(path.into())]),
    ])
}
```

The R6RS-conformant condition hierarchy already exists
(`crates/cs-core/src/condition.rs`); each stdlib crate adds its
own subtree under `&stdlib`. The condition-type RTDs are
registered once per process (Lazy/OnceLock).

## Build-time gating per module

Each `cs-stdlib-<name>` crate has its own cargo feature in
`cs-runtime`:

```toml
# crates/cs-runtime/Cargo.toml
[features]
stdlib-fs       = ["dep:cs-stdlib-fs"]
stdlib-path     = ["dep:cs-stdlib-path"]
stdlib-net      = ["dep:cs-stdlib-net"]
…
```

The `stdlib` umbrella feature turns them all on; `--no-default-features
--features stdlib-json` turns just one on. WASM builds drop the
host-dependent set:

```toml
# WASM build profile (in xtask or just documented)
cargo build --target wasm32-wasip1 -p cs-cli \
  --no-default-features \
  --features ffi-trait,regions,stdlib-string,stdlib-format,stdlib-json,stdlib-csv,stdlib-toml,stdlib-base,stdlib-hash,stdlib-uuid,stdlib-random,stdlib-regex,stdlib-collection,stdlib-log,stdlib-metrics,stdlib-math
```

## Streaming I/O

The `Port` value already supports byte / text reads/writes. Each
stdlib I/O procedure has two flavors:

- **Slurp** — convenience for small payloads:
  `(read-file-string "config.toml")`, `(json-read-string s)`.
- **Stream** — port-based for large payloads:
  `(with-file-input-port "huge.log" (lambda (p) (json-read p)))`,
  `(gzip-stream-create out-port)` returning a port wrapper that
  compresses on write.

The Rust backing crates use their crate's streaming API
(`serde_json::from_reader`, `flate2::write::GzEncoder`, etc.).
The Port → Read/Write adapter lives in
`cs-stdlib-meta/src/port_adapter.rs` and converts a Scheme
`Port` value into a `std::io::Read` or `std::io::Write` so the
backing crate's streaming form works unmodified.

## Concurrency posture

The runtime is single-threaded; `(crab …)` modules follow suit.
Net I/O is synchronous; long blocking operations
(`socket-read`, `process-wait`) are documented as blocking the
single Scheme thread. The existing BEAM actor system
(`(import (beam))`) is the parallelism primitive — async I/O
support is a separate, larger spec.

Sole exception: HTTP server (`(crab http server)`) needs a
multi-threaded executor. It runs Tokio inside the cs-stdlib-http
crate and bridges incoming requests back to Scheme via the BEAM
actor primitives. This is the only `(crab …)` module that
implies the BEAM feature.

## Testing strategy

Three tiers:

1. **Rust-side smoke** at `crates/cs-stdlib-<name>/tests/` — each
   builtin is callable from Rust with a synthetic
   `HostProcedure` harness, asserts the round-trip works
   without involving the Scheme tier.
2. **Scheme-side conformance** at
   `tests/conformance/crab-<name>.scm` — one positive + one
   negative case per public procedure, asserted with the
   existing `__test-summary__` harness. Runs on walker / vm /
   vm-jit tiers, gated to skip when the corresponding feature
   is off.
3. **Cross-module integration** at
   `tests/conformance/crab-integration/*.scm` — a smoke
   workload combining 3+ modules (e.g., "fetch JSON from
   HTTP, hash it, write to disk, log result"). Per FR-1's
   smoke test.

## Documentation

Each module ships:

1. `crates/cs-stdlib-<name>/README.md` — procedure reference
   (NFR-5).
2. A short example in `examples/crab-<name>-tour.scm` showing
   the module in 20–40 lines.
3. The top-level `lib/crab/README.md` — module index, links to
   each.

Future tooling work (out of scope here) will surface these in an
LSP and on the project website.

## Reuse of existing crates

Three crates already exist that should migrate into the new
shape:

- `cs-ffi-sha2` → `cs-stdlib-hash` (broaden to SHA-1 / MD5 /
  Blake3 / HMAC; ship as `(crab hash)`).
- `cs-ffi-http` → `cs-stdlib-http` (broaden to client + server,
  proper error model, streaming bodies).
- `cs-ffi-example` stays as a *teaching* example for how to
  author a third-party FFI plugin — kept out of `(crab …)`.

## ADR scope

A new ADR (next-numbered after 0018) will ratify:

1. The `(crab …)` namespace as the project's stdlib home.
2. The per-module-crate layout (NFR-1).
3. The opaque-payload mechanism (`Value::Opaque` vs per-type
   variants — pick one with rationale).
4. The error-model rule (`&stdlib`-rooted condition hierarchy).
5. The WASM-subset baseline.

Reuse / supersedes nothing (it adds; doesn't replace).

## Rollout

- **Iter 1** (this iter): requirements.md + design.md + ADR
  draft. No code yet.
- **Iter 2 onward** per requirements.md plan order. Each iter
  lands one or two crates + their Scheme wrappers +
  conformance tests + README behind a per-module cargo feature
  flag, default-off. The `stdlib` umbrella feature flips on
  iter-by-iter as each module reaches "tested + documented".
- **Final iter**: flip `stdlib` default-on, write exit report,
  remove the now-superseded `cs-ffi-sha2` / `cs-ffi-http`
  crates (replaced by their `cs-stdlib-*` successors), update
  `lib/crab/README.md` as the canonical module index.

## Risks and open questions

1. **Binary size** — every module compiled in adds to the
   default `crabscheme` binary. A naive add-everything build
   could blow past 100 MB. Mitigation: per-module cargo
   features (NFR-3) + lazy-loading via dlopen for the heavy
   tail (`cs-stdlib-http`, `cs-stdlib-archive`,
   `cs-stdlib-compress`). Measure after iter 7 lands the first
   binary-heavy modules; decide on default vs opt-in then.
2. **Opaque payload variant vs new `Value` variants** —
   `Value::Opaque(Gc<Box<dyn OpaquePayload>>)` is general but
   needs a `downcast` step at every accessor; per-type variants
   (`Value::Path`, `Value::Regex`, `Value::Instant`) are
   faster but bloat `Value` toward 50+ variants over time.
   ADR-decide; the slow start (only `Value::Opaque`) is the
   safer default.
3. **Versioning** — Rust crate updates can break ABI for dlopen
   users. The static-link path is unaffected, but
   `(load-shared-library "libcs_stdlib_http.dylib")` works only
   when the dylib matches the host's `cs-stdlib-http` version.
   Document the constraint; add a version check to
   `declare_stdlib_module`.
4. **Test surface explosion** — 24+ modules × 8 procedures
   average × 2 cases = ~400 new conformance tests. CI
   wallclock budget needs review; consider parallel test
   execution per module group.
5. **Sphinx of black quartz, judge my vow** — namespace
   collision check: `(crab …)` is short, but the word might
   carry baggage (`apt-cyg`, `Crab Apple`, etc.). Acceptable;
   the project name already invokes the same root.
