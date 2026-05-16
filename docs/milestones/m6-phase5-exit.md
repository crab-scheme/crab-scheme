# M6 Phase 5 Exit Report — IC + uniform-NB coverage push (1.74× → 2.31×)

> Status: **Closed** — tag `m6-phase5-complete` at the commit landing this report.
> Predecessor: M6 Phase 4 (`docs/milestones/m6-phase4-exit.md`, tag `m6-phase4-complete`).
> Spec: `jit-cranelift` (Phase 4's spec doc still anchors; Phase 5 tracked through this report and the per-iter commit log).

## Phase 5 scope

After Phase 4 closed with the uniform-NB tier merged but **unused** at scale (iter2 diagnostic surfaced that every hot bench was falling back to specialized), Phase 5's brief was singular: **close the geomean gap to the pre-1.0 perf gate**.

- Entry baseline: **1.74× geomean** over bytecode VM on the 8-bench microbench (`docs/measurements/2026-05-15-pre-1.0-gates.md`).
- Target: **≥5× geomean** (pre-1.0 gate from ROADMAP).
- Constraint: no rewrite of the JIT pipeline; incremental work against the existing specialized + uniform-NB two-tier shape.

Phase 5 ran ten iters (iter1–iter9 plus the closeout attempt at Lever 4). Three of the ten were reverted (iter6, iter8, the Lever 4 cross-conv `return_call` attempt) — each surfaced a soundness or compiler-support wall and informed the next iter.

## Decision

**Close M6 Phase 5 at the +33% over baseline mark (1.74× → 2.31×).** The 5× gate is not met. The remaining gap requires architectural rework (call-site dispatch redesign + escape-analysis allocation elimination + type-feedback specialization), not more incremental coverage. Phase 5's exit is the natural seam to either reframe the gate via ADR or open a larger JIT redesign track. Both are tracked in "Next priorities" below.

Tagging `m6-phase5-complete` is consistent with how Phase 2/3/4 closed: each phase shipped a tractable scope on top of the previous; "JIT meets the 5× gate" is bigger than this phase delivers.

---

## What shipped

### Iteration log (with commits + headline movement)

| Iter | Commit | Headline | Geomean |
|------|--------|----------|---------|
| baseline | `4673d76` | — | **1.74×** |
| iter1 | `f24699c` | Lazy `value_args` in `vm_ic_dispatch` (refactor) | 1.74× (no move) |
| iter2 | (diagnostic only) | Found: every hot bench falls back to specialized — uniform-NB unused | — |
| iter3 | `53207f2` | Widen uniform-NB to BoxTyped / AnyToFix/Bool/Flo / AnyTruthy / FixToFlo / IntCharBitcast + CallSelf-then-BoxTyped tail pattern + vector-ref/set!/length/? free-var promotion | **2.18× (+25%)** |
| iter4 | `9f20e9e` | car/cdr free-var EnvLookup→Any promotion | 2.22× |
| iter5 | `a077b04` | Variadic `/` with MakeClosure-gated lowering | ~2.18× |
| iter6 | REVERTED | Compile-time inlining attempt — SSA renumbering bug + missing iter7 prerequisite | — |
| iter7 | `741db0a` | `vm_value_div_nb` runtime helper + `Inst::Div` — Fixnum/Fixnum `/` finally JITs | ~2.27× |
| iter8 | REVERTED | EnterScope/DefineLocal via env helper — exit 133 at scale (refcount use-after-free) | — |
| iter8b | `23dfae1` | EnterScope/DefineLocal fixed — fresh `EnvLookupAny` on every LoadVar, no SSA reuse | ~2.27× |
| iter9 | `d250ba3` | Single-binding let* inlining at bytecode compiler | **2.31×** |
| Lever 4 | REVERTED | Outer trampoline cross-conv `return_call(Tail)` — Cranelift verifier rejects | — |

### Headline bench wins (baseline → exit)

| Bench | Baseline | Exit | Speedup gain |
|-------|---------:|-----:|-------------:|
| mandelbrot     | 0.99× | **4.31×** | +335% |
| binary-trees   | 3.69× | 4.00× | +8% |
| nqueens        | 1.13× | 1.36× | +20% |
| spectral-norm  | 0.99× | 1.48× | +49% |
| fib            | ~2.4× | 2.56× | +7% |
| tak            | ~1.9× | 2.14× | +13% |
| ack            | ~2.1× | 2.25× | +7% |
| alloc-stress   | ~1.8× | 1.89× | +5% |

Mandelbrot was the structural keystone — iter3 unlocked the flonum inner loop's path to uniform-NB tier, moving it from 0.99× (parity with VM) to over 4×. The other wins are largely from coverage expansion: bodies that previously rejected at the translator now compile through uniform-NB.

### Per-bench IC stats (post-iter9, for reference)

| Bench | JIT calls | hits | misses | hit-rate |
|-------|----------:|-----:|-------:|---------:|
| spectral-norm(50) | 100,937 | 97,971 | 23 | **99.98%** |
| nqueens(8) | ~98k | ~80k | ~17k | ~83% |
| binary-trees(10) | 2,658 | 672 | 2 | 99.7% |
| mandelbrot(60) | mostly tail-self-call (loops in-frame) | — | — | n/a |
| alloc-stress(200) | 199 (CallSelf, no IC events) | 0 | 0 | n/a |

The 99.98% hit rate on spectral-norm post-iter7 confirms that the IC infrastructure is healthy — the dispatch overhead is structural, not from misses thrashing the cache.

---

## Why Phase 5 didn't hit the 5× gate

The 1.74× → 2.31× arc is a **+33% improvement** distributed across nine iters. Each iter gained 3-10%; compounding flattens out because the wins target the same call-site dispatch hot path that's now near-optimal at the current architecture's limit.

The structural bottlenecks that block further incremental progress:

1. **`vm_ic_dispatch` overhead per IC hit.** Even at 99.98% hit rate, every JIT-to-JIT call funnels through:
   - SystemV calling convention crossing (caller is Tail-conv inner, callee is SystemV outer trampoline → Tail-conv inner).
   - Per-arg type check against `cached_param_types` (4-byte mask + branch per arg).
   - Per-arg refcount via `vm_value_clone_gc` (function call per arg; no-op for NB-inline, atomic incref for heap).
   - `JitCtxGuard::install` + `JitFrameGuard::install` (TLS save + write, RAII restore on exit).
   - Optional frame-env construction (`needs_frame_env`-gated; n-queens-style closure capture).

   Inlining any of these into JIT IR balloons code size dramatically at every call site. The Lever 4 attempt at making the outer trampoline tail-call inner (which would have collapsed the SystemV→Tail crossing into one frame) failed: Cranelift's verifier rejects `return_call` across calling conventions.

2. **Allocation pressure in flonum-heavy benches.** Spectral-norm's `matrix-elt` does `(/ (* ij (+ ij 1)) 2)` — the product of consecutive integers is always even, so the division is always exact, but the JIT can't prove this statically. Every call allocates a `Rational` (one heap alloc per invocation). For ~50k calls, that's the dominant cost.

3. **No type-feedback-driven re-specialization.** Hot loops with stable arg types should compile to type-stripped inner loops. The current uniform-NB tier emits per-op type checks (`emit_nb_arith_fixnum_fast`); a speculative Flonum-only or Fixnum-only specialization would skip those checks entirely.

4. **No automatic inlining of small callees.** `matrix-elt` is a 3-op leaf called millions of times. Inlining it would eliminate the IC dispatch per call. Iter6 attempted this and was reverted due to SSA renumbering bugs in the pass-through; making it correct requires careful block remapping and ownership analysis.

These four blockers are not incremental — each is a multi-week-to-multi-month investment. The "next architectural moves" section below sketches what each would look like.

---

## Empirically ruled out (won't unlock the gap)

- **Removing the type guard in uniform-NB branches** → zero perf change. The guard is correctness-critical, not a perf tax.
- **Disabling JIT counter bumps** → zero perf change. The atomic ops are amortized.
- **Cross-CallConv `return_call`** (Lever 4 attempt) → Cranelift verifier rejects `return_call(SystemV_caller, Tail_callee)`. A workaround would require either making the outer Tail-conv (breaks the runtime ABI for `transmute extern "C"` dispatch) or building a custom calling-convention crossing trampoline that does the stack/register shuffle by hand.

## Tried-and-reverted (with reasons)

- **iter6 (compile-time inlining)** — Both speculative paths (MakeClosure peephole + env-based) had SSA renumbering bugs in the pass-through. spectral-norm produced wrong answer (1.2595 vs 1.2742). Reverted entire iter6. Would need correct block remapping + ownership analysis to land safely.

- **iter8 (EnterScope/DefineLocal first attempt)** — Tracked DefineLocal'd bindings in both the SSA map AND the runtime env, but reused the same SSA value across both paths. `RirInst::EnvDefineLocal` consumed the value via `to_value()` (ownership transfer), leaving subsequent LoadVars hitting use-after-free. Bisected via `nq6.scm` at varying n; n=6 worked, n=7+ crashed (exit 133). Fixed in iter8b by emitting fresh `EnvLookupAny` per LoadVar instead of reusing the consumed SSA value.

- **Lever 4 (cross-conv `return_call` for outer trampoline)** — One-line change in `compile_outer_trampoline` (replace `call`+`return_` with `return_call`). Compiles, but `define_function outer` fails verification at finalize. Cranelift does not support `return_call` across SystemV ↔ Tail conv. Reverted; would need either custom trampoline shape or a direct-inner-pointer cache to bypass the outer entirely.

---

## Concrete code changes

Iter3 (`53207f2`) and iter9 (`d250ba3`) account for ~75% of Phase 5's gain. The full diff lives across these crates:

- **`crates/cs-jit-cranelift/src/lowering.rs`** — uniform-NB coverage expansion (BoxTyped, AnyTo*, AnyTruthy, FixToFlo, IntCharBitcast as identity in NB; Inst::Div + Inst::EnvDefineLocal lowering with helper call; CallSelf-then-BoxTyped tail pattern recognition).
- **`crates/cs-vm/src/vm.rs`** — `vm_value_div_nb`, `vm_env_define_local_nb` runtime helpers; `GenericArith::Div` arm.
- **`crates/cs-vm/src/jit_translate.rs`** — free-var EnvLookup→Any promotion for vector-ref/set!/length/?, car/cdr; EnterScope/LeaveScope/DefineLocal handlers; `local_scopes` tracking with fresh-EnvLookupAny per LoadVar.
- **`crates/cs-vm/src/compiler.rs`** — single-binding let* inlining (`((lambda (x) body) arg)` → EnterScope + DefineLocal + body + LeaveScope) when arity == 1.
- **`crates/cs-rir/src/lib.rs`** — new RIR variants: `Div(Value, Value, Value)`, `EnvDefineLocal(u32, Value)`.

---

## Next architectural moves (beyond Phase 5's incremental scope)

Each of these is a multi-iter project — not a Phase 5 follow-up. They're listed here for the next milestone planning, in rough order of expected payoff per investment:

### A. Type-feedback-driven specialization

Track arg type histories per-IcSlot; when a slot sees N consecutive monomorphic calls with stable types (e.g. all-Flonum), trigger re-compilation of the callee for a type-stripped variant that skips per-op type checks. Existing infrastructure: `LambdaProfile` already records arg-type feedback; the missing piece is the re-compile trigger + the specialized lowering pass.

Expected payoff: significant for hot flonum loops (spectral-norm, mandelbrot, n-body) where every arith op currently does a tag-check + payload-extract + slow-path-branch. Removing those would compress the inner loop by 3-4× in instruction count.

### B. Compile-time inlining of leaf callees (correct version of iter6)

Detect callees that are: short (< 20 RIR insts), pure (no Call/CallGeneral inside), and called from a JIT-eligible body. Splice their RIR into the caller during translation, with proper SSA renumbering and ownership analysis.

Expected payoff: eliminates IC dispatch entirely for the inlined calls. Spectral-norm's `matrix-elt` would inline into the j-loop body, removing ~50k IC dispatches per run.

### C. Escape-analysis-driven allocation elimination

For Rational/Flonum/Pair allocations whose lifetime is provably within a single JIT body, replace heap alloc with stack alloc or direct register storage. Spectral-norm's `(/ (* ij (+ ij 1)) 2)` is the motivating case: the resulting Rational is consumed by `(/ 1.0 denom)` two ops later, never escapes.

Expected payoff: dependent on alloc rate per bench. Spectral-norm allocates roughly 1 Rational + 1 Flonum per matrix-elt call ≈ 100k allocs per run; eliminating them is the single biggest unmeasured win.

### D. Direct-inner-pointer IC slot (lighter Lever 4 alternative)

Cache the **inner** (Tail-conv) function pointer in the IcSlot, alongside the existing outer (SystemV) pointer. At the IC hit path in JIT code, emit `call_indirect` with Tail conv signature to the inner directly — skipping the outer trampoline frame. Requires the JIT body's inner signature to match the callee's inner signature (both uniform-NB → both `(i64, i64, …) → i64` Tail-conv).

Expected payoff: saves one stack frame per JIT-to-JIT call (~10ns × 100k calls ≈ 1ms per spectral-norm run, or ~2%). Smaller than A/B/C but tractable as a single-iter project.

### E. Reframe the 5× gate via ADR

The 5× gate predates the measurement infrastructure built in Phase 4. Re-measure against a clearer baseline (e.g. "competitive with mature bytecode interpreters: ≥2× Chez, ≥3× Guile on geomean") and write an ADR proposing the reframe. This is bookkeeping; the actual perf number changes only with A-D.

---

## Test posture

884 / 0 tests passing on workspace `cargo test --release`. All 8 microbench cases produce correct results on all four tiers (walker, vm, vm-jit, plus the cross-language comparison rows). The closeout commit (`d250ba3`) is the green head.

## Acknowledgements

Phase 5 was nine iters of "find the next bottleneck, widen the tier coverage, re-measure." iter3 (the BoxTyped + free-var promotion bundle) was the keystone — it moved mandelbrot from parity-with-VM to over 4× speedup in one commit by making the flonum inner loop actually eligible for uniform-NB. Iter9's single-binding let* inlining gate was the prettiest piece of engineering: a small heuristic refinement that simultaneously preserved a 1.5× win on spectral-norm/mandelbrot and avoided a 0.94× regression on nqueens.

The Lever 4 wall (Cranelift's verifier rejecting cross-conv `return_call`) is the cleanest signal that further wins via incremental call-site shaving have hit diminishing returns. The next phase's investment shape (architectural — type-feedback specialization, inlining, escape analysis) is structurally different from Phase 5's iter-by-iter shape.

---

*Authored 2026-05-16 at the close of M6 Phase 5. JIT geomean now 2.31× over bytecode VM, a +33% gain over Phase 4's exit. Pre-1.0 perf gate (5× geomean) remains unmet; next move is either an ADR reframe or a multi-iter architectural track on type-feedback specialization + leaf-callee inlining + allocation elimination.*
