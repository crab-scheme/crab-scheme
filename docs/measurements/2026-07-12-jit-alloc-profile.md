# JIT-tier allocation profile attribution — alloc-stress / binary-trees / nqueens

> Bead: cs-vnf.1. Profile-only — no product code changed by this work.

## Methodology

- Built `crabscheme` in release mode with debug symbols retained:
  `CARGO_PROFILE_RELEASE_DEBUG=true devenv shell -- cargo build --release --bin crabscheme`.
- Profiler: [`samply`](https://github.com/mstange/samply) 0.13.1, obtained ad hoc via
  `devenv -O packages:pkgs samply shell -- ...` (not on PATH / not in `devenv.nix`; no
  devenv config was changed by this work).
- Each bench source under `bench/microbench/scheme/{alloc-stress,binary-trees,nqueens}.scm`
  was copied unmodified except for a single scaled-up input constant, into a scratch
  `prof-scm/` directory (deleted before commit), so each run's steady-state execution
  lasts several seconds under sampling:
  - `alloc-stress`: `n` 200 → 60000 (12M pairs allocated instead of 200K)
  - `binary-trees`: `depth` 10 → 18
  - `nqueens`: `(nqueens 8)` → `(nqueens 11)`
- Recorded with:
  `samply record --save-only --unstable-presymbolicate -o <bench>.json -- ./target/release/crabscheme --tier vm-jit run prof-scm/<bench>.scm`.
  `--unstable-presymbolicate` writes a `.syms.json` sidecar with resolved symbol ranges
  for every library referenced by the profile, which made it possible to get
  symbol-level attribution as plain text/tables (via a scratch Python parser of the
  Firefox Profiler JSON format) without needing the `samply load` browser UI.
- Percentages below are **self time** (leaf-frame sample counts / total samples), not
  inclusive time, unless noted.
- Caveat: wall-clock time for these runs was 3–4x CPU time (~30-40% CPU utilization
  observed via `time`), i.e. the process spent a lot of wall-clock time off-CPU
  (scheduling/paging, not busy-looping) — this doesn't affect the on-CPU symbol
  percentages below (samply only samples running threads) but means these are not
  clean single-core-bound compute benchmarks; worth a separate look if it recurs.
- Addresses inside `crabscheme`'s own binary and system libraries resolved to real
  symbol names via the sidecar. A separate class of addresses (`resource == -1`, no
  owning library) could **not** be resolved — these are the Cranelift-JIT-emitted
  native machine code for the compiled Scheme bodies themselves, generated at runtime
  with no debug info to symbolicate against. They're reported as a distinct bucket
  (`j — JIT-generated native code`) rather than folded into "other", since they
  represent productive execution of the user's compiled loop, not overhead.

## Per-bench % breakdown

Categories: **(a)** malloc/free inside `Rc::new`/drop paths, **(b)** out-of-line JIT
helper-call overhead (`vm_alloc_pair_gc`, `vm_pair_car_gc`/`vm_pair_cdr_gc`,
`vm_value_clone_gc`/`vm_value_drop_gc`, etc. — `crates/cs-vm/src/vm.rs:1902-2037` plus a
handful of sibling `vm_*_gc`/dispatch helpers elsewhere in the same file), **(c)**
refcount/RefCell traffic (`Value`/`Pair` `Clone`/`Drop` cascades, `Rc::drop_slow`),
**(d)** NaN-box encode/decode transcoding (`NanboxValue::to_value`/`from_value` and
friends), **(j)** JIT-generated native code (unresolvable, no owning lib — the
compiled loop body itself), **(o)** everything else (env lookups, dyld stubs, proc
table, startup).

| Bench | (a) malloc/Rc-alloc | (b) JIT helper calls | (c) refcount/drop | (d) NaN-box transcode | (j) JIT native code | (o) other | samples |
|---|---|---|---|---|---|---|---|
| alloc-stress  | 32.71% | 16.32% | 10.89% | 10.18% | 16.55% | 13.35% | 2525 |
| binary-trees  | 27.82% | 21.92% | 16.59% | 18.16% |  9.54% |  5.97% | 2495 |
| nqueens       | 20.66% | 26.19% |  9.48% | 19.48% |  7.69% | 16.50% | 1951 |

(a)+(b)+(c)+(d) — the four "planned optimization" categories combined — account for
**70.1%** of alloc-stress, **84.5%** of binary-trees, and **75.8%** of nqueens on-CPU
time. Allocation-adjacent cost (a+b+c, i.e. everything except NaN-box transcoding) is
**59.9% / 66.3% / 56.3%** respectively.

## Top-10 hottest symbols per bench

**alloc-stress** (2525 samples):

| % self | symbol |
|---|---|
| 10.50% | JIT-native (`0x7d7068154`, unresolved) |
| 7.21% | `cs_vm::vm::Env::get` |
| 6.89% | `cs_vm::vm::NanboxValue::to_value` |
| 5.19% | `vm_value_clone_gc` |
| 4.44% | `vm_alloc_pair_gc` |
| 3.96% | `core::ptr::drop_in_place<cs_core::value::Value>` |
| 3.64% | `cs_gc::rc_only::Gc<T>::new` |
| 3.37% | `vm_value_drop_gc` |
| 3.25% | `cs_vm::vm::NanboxValue::from_value` |
| 2.97% | `<cs_core::value::Pair as Drop>::drop` |

**binary-trees** (2495 samples):

| % self | symbol |
|---|---|
| 13.55% | `<cs_core::value::Value as Clone>::clone` |
| 11.74% | `cs_vm::vm::NanboxValue::to_value` |
| 11.22% | `libsystem_malloc.dylib` (tiny-region alloc, `0x315d0`) |
| 9.06% | `vm_value_clone_gc` |
| 6.37% | `cs_vm::vm::NanboxValue::from_value` |
| 4.81% | `vm_value_drop_gc` |
| 3.85% | `libsystem_malloc.dylib` (`0x2a0d4`, free-list) |
| 3.33% | `vm_pair_car_gc` |
| 3.13% | `cs_gc::rc_only::Gc<T>::new` |
| 2.93% | `vm_alloc_pair_gc` |

**nqueens** (1951 samples):

| % self | symbol |
|---|---|
| 8.82% | `cs_vm::vm::NanboxValue::to_value` |
| 7.48% | `cs_vm::vm::Env::get` |
| 6.36% | `vm_value_clone_gc` |
| 5.59% | `cs_vm::vm::NanboxValue::from_value` |
| 5.59% | `vm_ic_dispatch` |
| 4.41% | `vm_env_lookup_any` |
| 4.36% | `cs_vm::vm::Bindings::insert_nb` |
| 3.18% | `core::ptr::drop_in_place<cs_core::value::Value>` |
| 2.26% | `vm_value_drop_gc` |
| 2.10% | `cs_vm::vm::proc_table::alloc` |

## Conclusion

Across all three benches, `vm_value_clone_gc`/`vm_value_drop_gc` — the generic
clone/drop helper pair that every out-of-line pair access, closure capture, and
environment write routes through — dominate: `vm_value_clone_gc` is consistently
top-5, with `vm_value_drop_gc` close behind (top 10 in all three benches), and they in turn
bottom out in `Rc`-refcount traffic ((c)) and, for anything crossing the
allocate-a-cons boundary, straight into `libsystem_malloc`/`cs_gc::rc_only::Gc::new`
((a)). binary-trees is the clearest case: it is dominated end-to-end by
`Value::clone` → `vm_value_clone_gc` → malloc/Gc::new, because every `cons` and every
`car`/`cdr` read forces a full `Rc`-bumping clone of a boxed `Value` across the JIT/Rust
ABI boundary — there is no fast path that keeps a freshly-allocated, non-escaping pair
inline in registers/JIT-native representation. alloc-stress and nqueens show the same
pattern with more of the cost redistributed into `Env::get`/`vm_env_lookup_any`
(list-building via `let loop` accumulators, and n-queens' `placed` list respectively,
both do heavy environment-slot reads inline with the allocation traffic). NaN-box
transcoding (d) is real but secondary everywhere (10–19%) — it's the cost of crossing
the JIT-native/heap-`Value` boundary, not the dominant cost of building that boundary.
Given that combined (a)+(b)+(c) — the allocation/dealloc/refcount stack, exclusive of
transcoding — is 56–66% of on-CPU time in all three benches, and that the single
largest individual contributor everywhere is the clone/drop-into-malloc chain (not the
allocator call sites in isolation), the data points most strongly toward **car/cdr and
cons inline fast paths** (keeping non-escaping, freshly-built pairs off the
generic-clone/generic-drop/malloc path entirely) as the highest-leverage next step,
with **nursery regions** as a close second for the allocation-heavy benches
(alloc-stress, binary-trees) specifically, since a bump-allocated nursery would cut
into the `libsystem_malloc` slice ((a), 21–33%) directly. A raw **allocator swap**
alone would only address the (a) slice (21–33%) and leave the larger (b)+(c)
helper-call/refcount overhead (27–39%) untouched, so it ranks last of the three
options based on this data.

## Tool-availability caveats

- `samply` is not installed on PATH or declared in `devenv.nix`; it was pulled in ad
  hoc per-invocation via `devenv -O packages:pkgs samply shell -- ...` and no devenv
  config was changed.
- `--unstable-presymbolicate` is (per its own `--help` text) an unstable/likely-to-change
  samply flag; it was the only way found to get plain-text/offline symbol resolution
  without the `samply load` browser UI. If it changes format in a future samply
  release, the scratch parser (deleted, not committed) would need adjusting.
- Percentages are self-time sample counts at ~1000 Hz-equivalent sampling (sample
  counts 1951–2525 per run); fine for the top-10/bucket-level breakdown above but not
  precise enough to distinguish sub-0.5% symbols confidently.
