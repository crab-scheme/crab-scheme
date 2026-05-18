# Standard Modules — Tasks

> Status: **Draft**
> Spec slug: `stdlib-modules`
> Companion: `requirements.md`, `design.md`

Each iter is a coherent landable chunk. Where two crates are
listed together, they ship in the same iter because they're
small, share a backing crate, or depend on each other tightly.

The first column maps the iter to the requirements.md plan-order
slot.

| # | Iter title | Adds | Depends on | Acceptance |
|---|---|---|---|---|
| 1 | Spec + ADR | `requirements.md`, `design.md`, ADR 0019 draft. | — | Specs land on `main`; no code. |
| 2 | `(crab path)` + `(crab fs)` | `cs-stdlib-path`, `cs-stdlib-fs`, `lib/crab/{path,fs}.scm`, two conformance files, READMEs. | Iter 1. | Tier-1 smoke for fs+path passes on walker/vm/vm-jit. |
| 3 | `(crab os)` + `(crab process)` | `cs-stdlib-os`, `cs-stdlib-process`, wrappers, conformance, READMEs. | Iter 2 (uses `(crab path)` for process cwd). | `(spawn (list "echo" "hi"))` returns a handle; `(process-wait …)` returns exit code 0. |
| 4 | `(crab string)` + `(crab format)` + `(crab regex)` | Three crates; `(crab format)` is pure Scheme so no Rust crate. | Iter 1. | Regex compile-once-match-many round-trip; printf-style `(format-string "~d/~d" 3 4) → "3/4"`. |
| 5 | `(crab time)` + `(crab random)` + `(crab uuid)` | Three crates; share `getrandom`/`chrono` backings. | Iter 1. | `(uuid-v4)` returns a fresh value; `(monotonic-time)` strictly increases. |
| 6 | `(crab json)` + `(crab csv)` + `(crab toml)` + `(crab base)` + `(crab url)` | Five crates; encoding cluster. | Iter 4 (uses `(crab string)`). | Each codec round-trips a non-trivial fixture. |
| 7 | `(crab hash)` + `(crab compress)` + `(crab archive)` | Three crates; binary-data cluster. Migrate `cs-ffi-sha2` into `cs-stdlib-hash`. | Iter 6 (uses `(crab base)` for hex output). | 1 GB streaming gzip round-trip passes FR-5. |
| 8 | `(crab log)` + `(crab metrics)` + `(crab trace)` | Three crates wrapping `tracing` + `metrics`. | Iter 5 (uses `(crab time)`). | JSON-formatted log line lands on stderr; counter increments visible via `(metrics-snapshot)`. |
| 9 | `(crab net tcp)` + `(crab net udp)` + `(crab net dns)` | `cs-stdlib-net` (single crate, three sub-libraries). | Iter 3 (process spawn for test fixtures). | TCP echo round-trip; DNS resolution of `localhost`. |
| 10 | `(crab http client)` + `(crab websocket)` | `cs-stdlib-http` (client only first), `cs-stdlib-websocket`. Migrate `cs-ffi-http` into the new crate. | Iter 9, iter 6 (uses `(crab url)`, `(crab json)`). | GET against a localhost test server returns expected JSON body. |
| 11 | `(crab http server)` | Server side of `cs-stdlib-http`; introduces Tokio runtime + BEAM-actor bridge per design.md. | Iter 10, BEAM runtime. | A 10-line "hello world" HTTP server responds to a curl request. |
| 12 | `(crab collection queue|heap|set|map)` + `(crab math …)` extensions | `cs-stdlib-collection`, `cs-stdlib-math`. | Iter 1. | Heap-sort via `(crab collection heap)` matches `list-sort` output for a 10k-element shuffled vector. |
| 13 | `(crab tty)` + `(crab signal)` | Two small crates; Tier-1 leftovers. | Iter 3. | `(terminal-size)` returns sensible values in an interactive shell; SIGINT handler fires from `(raise-signal 'SIGINT)`. |
| 14 | `(crab)` meta + introspection | `cs-stdlib-meta` introspection wired; `(crab-list-modules)` and `(crab-module-info …)` per FR-2. | Iters 2–13. | Smoke test asserts every shipped module is in the list. |
| 15 | Bench inclusion + WASM subset validation + exit report | One `(crab json)`-using bench, one `(crab fs)`-using bench in `bench/realworld/`; WASM-subset build green; `docs/milestones/stdlib-modules-exit.md`. | Iters 2–14. | Realworld JSONL contains the two stdlib benches green on vm; WASM `cargo build --target wasm32-wasip1` clean with the documented subset features. |

## Cross-cutting

Throughout the iter sequence:

- Each iter that lands a new crate also writes the matching
  `crates/cs-stdlib-<name>/README.md` (NFR-5) and adds the
  module to `lib/crab/README.md`.
- Each iter that introduces a new condition type extends
  `crates/cs-core/src/condition.rs` registry (per FR-6 / the
  design.md error-model section).
- Each iter that adds a Rust dependency declares it at workspace
  level in `Cargo.toml`'s `[workspace.dependencies]` so version
  pinning stays centralized.
- Per-iter conformance tests follow the
  `tests/conformance/__test-summary__` shape so CI's existing
  rollup picks them up without harness changes.

## Out of iter scope

These are not on the iter list — they're explicitly deferred:

- Database drivers (`cs-stdlib-sqlite`, `cs-stdlib-postgres`).
  Reasonable future spec.
- Async runtime (`(crab async)`). Out of scope.
- GUI / windowing. User-space plugin territory.
- A package manager. Future ecosystem work.
- LSP integration. Future tooling spec.

## Rollback story

- Iters 2–14 are pure additions behind per-module cargo features
  default-off (or default-on after each iter's exit gate is
  met). Reverting a single iter is a one-commit revert that
  doesn't touch downstream iters because no later module
  imports an earlier one as a hard dependency (the wrapper
  imports are R6RS `(import …)` which only bind at runtime).
- Iter 15 (flip `stdlib` default-on) is reversible by flipping
  the feature default back; users who came to rely on the
  default would need to add `--features stdlib` to opt back in.
  Cost is documentation, not code.
