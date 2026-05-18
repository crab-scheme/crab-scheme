# beam-runtime worktree

This worktree carries the BEAM-style runtime work described in
`docs/research/beam_runtime_spec.md`. Branch: `beam-runtime` off
`main`.

## What's here vs main

```
crates/
  cs-actor/        <-- scaffold, B2
  cs-table/        <-- scaffold, B4
  cs-supervisor/   <-- scaffold, B5
  cs-hotreload/    <-- scaffold, B6
```

Each crate has a stub `lib.rs` with the public API sketched per the
spec, plus a `Cargo.toml` wired into the workspace. Nothing
implements anything yet — the scaffolds exist so the workspace
builds with the new module layout and we can land each phase as a
focused PR without re-litigating crate boundaries.

## Active task ladder

The B1-B8 phases from the spec are tracked as tasks #92-#99
in the harness's task list (`TaskList`). Currently:

| Phase | Task | Status |
|-------|------|--------|
| B1    | #92  | in_progress |
| B2-B8 | #93-#99 | pending |

B1 (per-Heap gc-stats migration) is the only NON-new-code phase —
it's a prerequisite cleanup that reverts Phase B/F's process-global
counters to per-Heap. Has to land before any cs-actor work because
each actor wants independent gc-stats and the global counters would
mingle them.

## Workflow

```bash
# Enter the worktree
cd .claude/worktrees/beam-runtime

# Or from anywhere
git worktree list
git -C .claude/worktrees/beam-runtime status

# Build just the new crates
cargo build -p cs-actor -p cs-table -p cs-supervisor -p cs-hotreload

# Build the whole workspace from the worktree
cargo build --workspace

# Push the branch when ready
git push -u origin beam-runtime
```

The worktree shares the `target/` directory with the main worktree
by default — it's at the repo root. Builds are incremental across
worktrees.

## When this branch is ready to merge

- All B1-B8 task checks green (or the subset being merged).
- Bench scorecard at `bench/realworld/` shows no regression for
  single-actor workloads.
- New conformance tests in `tests/conformance/foundation/` cover
  the spawn/send/receive/supervise/reload semantics.
- The full spec's "Goals" table (G1-G5) measured + reported in the
  PR description.

## Risks called out in the spec

1. **One Runtime per actor** — every cross-actor primop has to be
   crisp about "my Runtime" vs "their Runtime." Expect 2-3 API
   iterations before the shape settles.
2. **JIT + hot reload** — Cranelift's safepoint support is evolving.
   May need to pin cranelift or contribute upstream.
3. **GC threading** — moving `cs_gc::Heap` from `!Send` to
   "Send-only-between-yields" is a real audit. Coordinate with the
   in-flight perf follow-ups (#88 heap-rooting migration).

See `docs/research/beam_runtime_spec.md#open-questions` for the
eight specific open questions.
