# Post-inline (.2/.4) allocation profile re-check — alloc-stress / binary-trees / nqueens

> Bead: cs-vnf.5 Phase 1. Profile-only, no product code changed by this work.
> Re-runs the exact methodology from `docs/measurements/2026-07-12-jit-alloc-profile.md`
> (cs-vnf.1) at `56ff0cf` (head of `feat/memory-epics` at the time cs-vnf.5 was picked
> up), i.e. **after** mimalloc landed as the global allocator (cs-vnf.2) and after
> NB-native 24-byte pairs + JIT inline `car`/`cdr`/`cons` landed (cs-vnf.3/.4).

## Methodology

Identical to cs-vnf.1's doc: release build with debug symbols
(`CARGO_PROFILE_RELEASE_DEBUG=true devenv shell -- cargo build --release --bin crabscheme`),
`samply record --save-only --unstable-presymbolicate`, same three benches scaled the
same way (`alloc-stress` n=200→60000, `binary-trees` depth=10→18, `nqueens` (nqueens 8)→(nqueens
11)), run at `--tier vm-jit`. Symbol attribution this time was done with a scratch
Python script (`prof-scm/parse.py`, not committed) that resolves `frameTable.address`
(rva) against the `.syms.json` sidecar's per-library `symbol_table`/`known_addresses`,
rather than the samply browser UI. Percentages are self-time (leaf-frame sample
share), same as cs-vnf.1.

## Malloc/allocation share, before vs. after .2/.4

| Bench | cs-vnf.1 malloc/Rc-alloc share (a) | post-.2/.4 malloc share (mimalloc symbols + `Gc::new`) | samples |
|---|---|---|---|
| alloc-stress  | 32.71% | **18.28%** | 2046 |
| binary-trees  | 27.82% | **24.87%** | 2866 |
| nqueens       | 20.66% | **7.19%**  | 2863 |

Symbols folded into "malloc share": `mi_malloc_aligned`, `mi_free`,
`mi_page_free_list_extend`, `mi_page_queue_find_free_ex`, `mi_bchunk_try_find_and_clear`,
`mi_theap_malloc_zero_aligned_at_generic` (mimalloc's allocation/free-list-refill slow
path), plus `cs_gc::rc_only::Gc<T>::new` (the Rc-allocation wrapper that calls into
mimalloc for every `cons`).

Full top-25-symbol dumps for all three benches are in this session's tool output;
not reproduced in full here since only the decision-relevant aggregate matters, but
the raw `.json`/`.syms.json` samply captures were **not** committed (scratch, per
cs-vnf.1's own precedent of not committing scratch profile captures).

## Decision-gate read

The bead's stated gate: "if malloc+free share is now under ~10-12% on the alloc
benches, a nursery cannot pay for its complexity — skip." Two of three benches
(alloc-stress 18.3%, binary-trees 24.9%) are **above** that threshold — on a naive
read of the gate alone, that's a GO signal, not skip. binary-trees in particular
barely moved (27.8% → 24.9%) despite mimalloc + inline cons landing: total bench
time dropped ~24% (cs-vnf.4) and, since the share also fell slightly, absolute
malloc time dropped even faster (~32% — mimalloc cut per-cons cost too); both
numerator and denominator shrank together, leaving the *share* nearly flat because
each `cons` still costs one mimalloc allocation. Only nqueens (7.2%, few cons-heavy hot loops relative to env
lookup / NaN-box transcode / IC dispatch) clears the skip bar on its own.

**But the malloc-share number alone is the wrong test here**, and the bead names the
second, better test explicitly: *"If the existing Region/escape-analysis (#28 SRA,
escape-to-region pass) already covers the profitable cases, that's also a legitimate
skip finding."* Checking that:

- `alloc-stress`'s hot allocation site is `make-list-n`: it builds a 1000-cons list
  **inside a helper function**, returns the list across the call boundary to
  `alloc-stress`'s loop, which immediately calls `length` on it and discards the
  result. This is a genuinely short-lived allocation — but it escapes `make-list-n`'s
  own frame (it's the return value) and crosses into a second call (`length`). #28's
  SRA pass only promotes *directly-consumed, non-escaping* conses within a single
  frame (see `crates/cs-gc/src/region.rs`'s own doc comment: region-safety requires
  "Layer 5 (escape analysis)" to prove non-escape, and manual/partial region use
  "requires the programmer's own discipline" otherwise). A cross-call, returned-value
  cons is exactly the case SRA cannot currently prove safe.
- `binary-trees`'s hot allocation site is `make-tree`: every `cons` there builds a
  tree node that's stored in the *caller's* recursive structure and lives across the
  entire `check`/traversal pass (each tree dies right after its check, but every node
  escapes `make-tree` via the return value). Exploiting that shape needs
  interprocedural return-value promotion — the deferred #51b/#51c wall — and a
  promote-on-escape nursery would pay a promotion-copy tax on every node, since all
  of them escape their allocating frame.
- So the residual malloc share splits into two buckets neither of which a *new*
  nursery mechanism profitably attacks: (1) truly-escaping, long-lived conses
  (binary-trees, most of nqueens' minimal residual) — a nursery can't help these,
  full stop; (2) short-lived-but-cross-call conses (alloc-stress) — this is precisely
  the interprocedural/cross-call escape case that **`#51b` (let-temp promotion) and
  `#51c` (closures) already attempted and explicitly deferred** (see
  `project_jit_proper_tail_calls` memory / issue #51 comment): "demote-on-JIT-path
  breaks 10+ diff tests (AOT-only-proven pass), pass-extension is a fragile
  cross-crate UAF coupling." cs-vnf.5's Phase 2 scope (per-call-frame nursery with
  escape promotion on write into non-nursery memory) is architecturally the same
  mechanism under a different name, on the same RC-heap substrate, with the same
  UAF-coupling risk previously found fragile enough to defer.

## Decision: SKIP

No new nursery is being built. The malloc share that remains after cs-vnf.2/.3/.4 is
concentrated in exactly the two categories a bump-nursery-with-promotion cannot
profitably improve on an RC heap (an *evacuating* nursery is off the table entirely:
raw `Rc` pointers are embedded in NaN-boxed `u64` payloads with no read barrier or
forwarding support, so the heap is non-moving by construction): (1) genuinely-escaping, long-lived allocations,
where a nursery only adds a promotion-copy cost; and (2) cross-call short-lived
allocations, where the *only* profitable mechanism (interprocedural escape-to-region
promotion) is the one `#51b`/`#51c` already prototyped and deferred for
UAF-fragility reasons that have not changed since. Re-attempting that mechanism under
`cs-vnf.5` within a 3-hour timebox would very likely hit the identical wall rather
than find new ground — the bead's own fragility warning ("bail with findings if the
wall reappears") is satisfied by inspection before implementation, not after.

Closing `cs-vnf.5` with this finding rather than building a nursery to justify the
bead, per Phase 1 instructions. The remaining `#51b`/`#51c` interprocedural
escape-analysis wall stays open (already tracked in `project_jit_proper_tail_calls`);
a future attempt would need to solve *that* problem (safe promotion across
`!Send`/JIT-tier boundaries) rather than reframe it as a nursery.
