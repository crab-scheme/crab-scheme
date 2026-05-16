# Contributing to CrabScheme

Thanks for your interest. CrabScheme welcomes contributions across
all milestones — bug fixes, AOT/JIT extensions, conformance fixes,
docs, tooling, and tests.

## Code of conduct

Be excellent. We follow the spirit of the [Rust Code of
Conduct](https://www.rust-lang.org/policies/code-of-conduct).
Disagreement on technical matters is welcome; personal attacks
are not.

## Dev env

The repo is reproducible via [devenv.sh](https://devenv.sh):

```bash
devenv shell        # drops you into a shell with Rust 1.95 + chez + guile + gambit + racket + wasmtime
```

If you don't have devenv, you need:

- Rust 1.95 (matching `rust-toolchain.toml`)
- `wasm32-wasip1` target (`rustup target add wasm32-wasip1`)
- Optional for benches: chez, guile, gambit, racket, wasmtime

## Workflow

1. **Fork** the repo on GitHub.
2. **Branch** off `main` or the active `rc*` branch — keep
   branches focused on one logical change.
3. **Iterate** with small commits. Per
   `~/.claude/CLAUDE.md`-style discipline: prefer small bites and
   frequent commits over big-bang PRs.
4. **Test** locally before pushing — `cargo test --workspace
   --release` catches the obvious. The CI runs the same plus a
   `wasm32-wasip1` build job.
5. **PR** against `main` (or the relevant `rc*` branch). Link to
   any tracking issue / milestone exit report the change relates to.

## Commit messages

Roughly conventional-commits-ish:

```
<scope>: <subject line under 70 chars>

<wrapped paragraphs explaining WHY — not what, the diff shows that>

<optional: bullet list of follow-up TODOs / known limitations>
```

Common scopes: `cs-aot`, `cs-vm`, `cs-jit-cranelift`, `cs-runtime`,
`bench`, `docs`, `ci`, `RC2 iter X`.

Example: `cs-aot: VecAlloc/Ref/Set runtime-helper lowering`.

## Code style

```bash
cargo fmt --all              # rustfmt — CI enforces --check
cargo clippy --workspace     # not yet enforced; clean output preferred
```

The repo has hooks (per the maintainers' `.claude/` setup) that
auto-run rustfmt on touched files. If you don't have those, just
run `cargo fmt` before commit.

## Tests

- **Unit tests** live next to the code in `#[cfg(test)] mod tests
  { ... }` blocks. Lightweight, fast.
- **Integration tests** live in `crates/<name>/tests/*.rs`. These
  are full e2e — they may shell out to `cargo build`, `rustc`,
  or `wasmtime`.
- **Conformance tests** in `tests/conformance/foundation/*.scm`
  drive the walker + VM + JIT through R6RS-style assertions.
  Wire new fixtures into `crates/cs-cli/tests/conformance.rs`
  AND into the bulk count tally (see commit `1fe3f6f` for the
  "always wire to the runner" lesson).
- **WASM conformance** runs via `bench/wasm-conformance.sh` —
  reproducible, gates against native conformance.
- **AOT tests** in `crates/cs-aot/tests/` cover the pipeline end-
  to-end via subprocess `rustc` or `cargo build`.

Run all of them:

```bash
cargo test --workspace --release
bash bench/wasm-conformance.sh
bash bench/aot-comparison.sh
```

## Adding a new RIR Inst to cs-aot

See [`docs/user/aot.md`](docs/user/aot.md)'s "How to extend"
section. The iter-L through iter-T commits each add one or two
Insts; they make good copy-paste templates.

## Adding a conformance fixture

1. Drop `tests/conformance/foundation/<name>.scm` using the
   existing `(test-section ...)` + `(test-equal ...)` / `(test-
   eqv ...)` / `(test-true ...)` helpers from `_prelude.scm`.
2. Add a `#[test] fn conformance_<name>() { run_conformance_file
   ("<name>.scm"); }` block in
   `crates/cs-cli/tests/conformance.rs`.
3. Add `"<name>.scm"` to the `conformance_aggregate_count` `files`
   list in the same file.
4. Verify both walker + VM + JIT tiers pass via `cargo test -p
   cs-cli --test conformance`.

## License

By contributing, you agree your contributions are dual-licensed
under MIT and Apache-2.0 per `LICENSE-MIT` and `LICENSE-APACHE`.
This matches the Rust ecosystem norm.

## Reporting issues

- **Bugs**: GitHub issues with a minimal reproducer + the output
  of `crabscheme --version` + the platform (OS + arch). For AOT
  issues, please include `--emit-rir` output if possible.
- **AOT-unsupported-Inst surfacing**: please open a "wanted-iter"
  issue with the source program that hit it; helps prioritize
  which Inst lands next.
- **Feature requests**: open a discussion or issue describing the
  use case. Major features go through an ADR (`docs/adr/`); small
  ones go straight to a tracked issue.

## Where things live

| Concern | Crate / Path |
|---------|--------------|
| Reader (Scheme source → Datum) | `crates/cs-parse/` |
| Macro expander | `crates/cs-expand/` |
| Tree-walker interpreter | `crates/cs-runtime/` |
| Bytecode VM + NB encoding | `crates/cs-vm/` |
| RIR (JIT + AOT shared IR) | `crates/cs-rir/` |
| Cranelift JIT backend | `crates/cs-jit-cranelift/` |
| AOT (RIR → Rust source) | `crates/cs-aot/` |
| FFI traits + dynamic loading | `crates/cs-ffi*/` |
| `crabscheme` CLI | `crates/cs-cli/` |
| Conformance suite | `tests/conformance/foundation/` |
| Microbench | `bench/microbench/` |
| WASM conformance harness | `bench/wasm-conformance.sh` |
| AOT coverage scorecard | `bench/aot-comparison.sh` |
| Per-milestone exit reports | `docs/milestones/` |
| ADRs | `docs/adr/` |
| Perf + conformance measurements | `docs/measurements/` |
| AOT user guide | `docs/user/aot.md` |
