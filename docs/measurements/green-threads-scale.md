# Green threads — scale & memory (P5.2)

> Measured on the `feat/green-threads` release binary (darwin-arm64), 2026-06-06.
> Probe: `spawn-source-green` N idle actors, each parked forever on
> `(raw-receive)`; peak RSS via `/usr/bin/time -l crabscheme run probe.scm N`.

## Per-actor RSS

| green idle actors | peak RSS | Δ/actor (vs N=1 base 16.5 MiB) |
|--:|--:|--:|
| 1 | 16.5 MiB | (base) |
| 500 | 471 MiB | ~909 KiB |
| 2000 | 1631 MiB | **~826 KiB** |
| 6000 | 2842 MiB | ~483 KiB* |

\* The N=6000 slope dips because 6000 `Runtime::new()`s don't all complete within
the 6 s hold window — *building* the per-actor Runtime is itself the cost. The
N=2000 figure (~826 KiB/actor) is the reliable steady-state number.

**Extrapolated:** 10k ≈ 8 GiB · 50k ≈ 40 GiB · 100k ≈ 80 GiB.

## Where the memory goes (and where it does NOT)

- **Dominant cost: the per-actor `Runtime`.** Every actor (green *or* dedicated)
  does `Runtime::new()` — a full per-actor builtins env (walker `top` + VM
  `vm_env`), the bundled-Scheme libraries eval'd into both tiers, the
  `SymbolTable`, macros, etc. A *parked idle* actor touches only a few stack
  pages, so ~826 KiB is essentially the Runtime. This is **not green-specific** —
  dedicated actors pay the same Runtime cost *plus* an OS thread; green just
  removes the thread (and the 4096 ceiling), which is why only green can reach
  these counts at all.
- **The coroutine stack is NOT the RSS lever.** mmap is lazily committed, so RSS
  = touched pages. And VM-tier green bodies stay *shallow* on the native stack:
  the bytecode VM heap-allocates its call frames, so deep Scheme recursion does
  not grow the native/coroutine stack. Probe: a green body computing
  `(sum 1..2_000_000)` (2 M-deep non-tail) returned the correct
  `2000001000000` on the **1 MiB** green stack with no overflow. So
  `GREEN_STACK_BYTES` is a *virtual*-footprint knob (and a safe backstop), not an
  RSS lever — and green bodies are effectively stack-overflow-safe via normal
  Scheme recursion. (The corosensei guard page remains as a backstop for
  pathological *native* recursion, e.g. a misbehaving builtin.)
- **Hard ceiling at extreme scale: `vm.max_map_count`.** One mmap per live
  coroutine stack → ~65 k concurrent green actors on a default-`sysctl` Linux
  before `mmap` fails, independent of stack size. 100 k needs the operator to
  raise `vm.max_map_count` (documented in ADR 0035).

## Conclusion

Green removes the thread-per-actor ceiling and the per-actor OS thread — a real,
large win, validated end-to-end on crab-cache (conformance + crash-recovery +
failover, throughput on par with Stage 1). But **50–100k concurrent actors is
memory-bound by the per-actor `Runtime` (~826 KiB each), not by stacks** — so the
next scale lever is a **shared Runtime** (below), not stack tuning.

---

# Shared-Runtime — feasibility & design

Goal: collapse the ~826 KiB/actor to a small per-actor overlay by sharing the
*immutable* base (builtins env + bundled libs + base symbols) across all actors
on a worker thread, leaving each actor only its own top-level defines + per-actor
mutable state.

## Feasibility (investigated)

- **Promising: `cs_vm::vm::Env` already has a parent chain.**
  `Env { parent: Option<Rc<Env>>, … }`, `Env::child(parent)`, and `get` walks the
  chain. A shared base env (builtins + libs, built once per worker) with a
  per-actor `Env::child(base)` for the body's defines is the natural shape — and
  the base is `Rc`-shared, so N actors cost one base + N small overlays.
- **Wall 1 — `DefineGlobal` defines at the chain *root*.** The VM's global-define
  walks to the root of the env chain (so a body `(define …)` would write into the
  *shared base*, breaking isolation). Needs a per-actor "define boundary" so
  defines land in the actor's child, not the shared base.
- **Wall 2 — `SymbolTable` is per-Runtime and not shareable.** `by_name:
  HashMap<Rc<str>, Symbol>`, `by_id: Vec<Rc<str>>`, `intern(&mut self)`, threaded
  as `&mut SymbolTable` throughout cs-runtime/cs-vm. A shared base env is keyed by
  `Symbol` *id*, so an actor's lookups only hit the base if its symbol ids match
  the base's. Requires either a per-worker shared interner (`Rc<RefCell<…>>` +
  reworking the pervasive `&mut SymbolTable` API) or a canonical/deterministic
  base interning that every actor reproduces.
- **Wall 3 — per-actor isolation of the mutable rest.** `macros`,
  `library_exports`, `pinned`, JIT lowerer/poison, `command_line` are per-Runtime
  mutable; they must stay per-actor (overlay), not shared, or one actor's
  macro/define leaks to peers. The const-folding in `eval_str_via_vm_inner` must
  treat base bindings as immutable (safe to fold) and overlay bindings as
  per-actor.

## Proposed shape

Split `Runtime` into:
- **`RuntimeImage`** (per worker thread, built once, `Rc`-shared, immutable):
  base `vm_env` (builtins + bundled libs), base `top`, the base `SymbolTable`
  snapshot, base macros.
- **`RuntimeInstance`** (per actor, cheap): `Env::child(image.vm_env)` overlay for
  defines, a small per-actor symbol extension over the shared base, per-actor
  macros/pinned/library_exports/command_line, JIT state.

`green_source_body` would `Runtime::from_image(worker_image())` instead of
`Runtime::new()`. The worker image is a thread-local `OnceCell` built on first use
(the LocalSet worker is single-threaded, so the `Rc` base is never shared across
threads — the same `!Send` isolation that makes per-actor Runtimes sound).

## Refined approach — avoid the `&mut SymbolTable` surgery (canonical base ids)

The `&mut SymbolTable` surface is **33 files** — rewriting it to a shared
`Rc<RefCell<SymbolTable>>` interner is the worst of the work. A cheaper path
shares the big thing (the env: builtins + bundled libs) *without* sharing the
interner:

1. **Canonical base ids.** Intern the builtins (+ bundled-lib top-level names) in
   a fixed, deterministic order at base construction so every actor's table gives
   them the *same* `Symbol` ids. Then a base env keyed by those ids is valid for
   any actor.
2. **Shared base env, per-actor child.** Build the base `vm_env` (builtins +
   bundled libs) **once per worker thread** (`Rc`, thread-local — sound: the
   LocalSet worker is single-threaded, same `!Send` isolation as today). Each
   actor runs its body in `Env::child(base)`; its `(define …)`s land in the child.
3. **Cheap per-actor syms clone.** Each actor still owns a `SymbolTable` (no API
   change), but cloned from the canonical base — `Rc<str>` entries are refcount
   bumps, so the clone is a HashMap+Vec copy of a few hundred entries (~tens of
   KiB), not a rebuild of the whole env. New symbols the body interns extend the
   clone at ids past the base.

Per-actor cost then ≈ syms clone + child env + per-actor mutable state, instead of
a full `Runtime::new()`. Estimate ~50–100 KiB/actor (vs ~826 KiB) → ~10× → 50k
feasible (~3–5 GiB). A later `Rc<RefCell>` shared interner (the 33-file change)
would shave the syms clone too, if needed.

## Walls (precise)

- **Wall 1 — define/`set!`-at-root.** The body's `(define …)` must land in the
  per-actor child, not the shared base. The VM `Inst::DefineGlobal` handler and
  the JIT `set!` helpers (`vm_env_set_fixnum`/`vm_env_set_nb`, `cs-vm/src/vm.rs`
  ~387/430) both **walk the parent chain to the root** for an undefined define/
  `set!`. Needs a per-actor "define boundary" so they stop at the child root.
- **Wall 2 — canonical interning** (above): deterministic base-id assignment +
  cheap per-actor clone.
- **Wall 3 — per-actor isolation of the mutable rest.** `macros`,
  `library_exports`, `pinned`, JIT state, `command_line` stay per-actor; the
  const-folder (`eval_str_via_vm_inner`) must treat base bindings as immutable
  (foldable) and child bindings as per-actor.

## Honest scoping

Still a **major core change** (cs-runtime + cs-vm), touching the VM+JIT
define/`set!`-at-root paths, base construction + the per-worker image cache, the
const-folder, and the per-actor isolation invariants — with the full ~1000-test
suite to keep green. A **milestone of its own**. The Env parent-chain + the
canonical-ids approach make it *feasible without the 33-file `&mut` rewrite*;
Walls 1–3 are the work. Success metric: < ~50 KiB/actor overlay (→ 50–100k
feasible with a raised `vm.max_map_count`), full suite green.

## Result (landed on `feat/shared-runtime`)

Landed: Wall 1 (Env define-boundary, `fcab895`) + `SymbolTable: Clone`
(`3e52142`) + per-worker `RuntimeImage` + `Runtime::from_image` overlay
(`902b274`) + base+extension `SymbolTable` (`68a9fd0`). `green_source_body`
overlays a per-worker shared base (env **and** symbol table) instead of
`Runtime::new()` per actor.

| green idle actors | original | shared env | **shared env + syms** |
|--:|--:|--:|--:|
| 1 | 16.5 | 16.6 | 16.7 MiB |
| 2000 | 1631 | 477 | **258 MiB** |
| 6000 | 2842 | 1381 | **721 MiB** |
| **Δ/actor** | **~826 KiB** | ~230 KiB | **~118 KiB** |

**~826 → ~118 KiB/actor — 7×.** 50k ≈ 5.9 GiB (was 40), 100k ≈ 11.8 GiB (was 80).
Slope flat across N. All green suites + crab-cache conformance stay green. Blast
radius is `green_source_body` only (dedicated/`spawn-activation` still
`Runtime::new()`; activation can adopt `from_image` next).

**The macros clone is *not* a factor** — measured directly (empty-`macros`
`from_image` probe): 475 MiB vs 477 MiB at N=2000, i.e. ~1–2 KiB/actor. The
bundled-macro map is small; a macros base/overlay would be invasive (expander
integration) for ~0 RSS. The big residual was the **`syms` clone** (~110 KiB),
now shared via the base+extension table above (the `Rc` base is shared; each
actor holds only its own symbols — no `&mut SymbolTable` API change).

**Remaining ~118 KiB** is the genuinely per-actor work: the loaded **body**
(source → bytecode + closures + overlay bindings) and the coroutine **stack**'s
touched pages. The next lever is **shared body compilation** — compile a source
once and share the bytecode across actors running the *same* body (e.g.
crab-cache's identical per-conn `conn.scm`); that's a distinct optimization from
the shared Runtime. 118 KiB already makes 50k practical (~6 GiB) and 100k feasible
(~12 GiB) with a raised `vm.max_map_count`.

## Result 2 — shared body compilation (`feat/shared-body-compilation`)

Split the eval pipeline (`compile_program_via_vm` + `run_bytecode`, `ec4ae2d`)
and added a per-worker, per-source compilation cache (`eval_str_via_vm_cached`,
`84f7afd`): the first green actor with a given body compiles it; later actors
adopt the cached symbol table as their syms base and re-run the cached
`Bytecode` against their own overlay (the body's closures share the cached code
chunks; only the closures + overlay bindings are per-actor). Sound because green
actors share the per-worker base env, so the bytecode's builtin ids resolve and
the compile-time const-fold saw only base builtins.

Measured the per-actor cost as a function of body size (N=2000), with and without
the body cache, on top of the shared Runtime:

| body | shared Runtime | **+ body cache** |
|---|--:|--:|
| trivial (`idle`) | ~118 KiB | **~40 KiB** |
| 200 `define`s | ~354 KiB | **~72 KiB** |

So a non-trivial body drops **~5×** more (354 → 72), and even the trivial body
~3× (the per-actor parse/expand/compile + its bytecode is eliminated). Combined
with the shared Runtime that is **~20×** off the original 826 KiB for a small
body. **At/under the <50 KiB target** for typical bodies. 50k actors ≈ 2–3.6 GiB,
100k ≈ 4–7 GiB. All green suites + crab-cache conformance green.

**Cumulative:** original 826 KiB/actor → shared env 230 → + shared syms 118 →
+ shared body compilation **~40–72 KiB/actor** (body-dependent). The remaining
floor is the coroutine stack's touched pages + the per-actor closures/bindings —
genuinely per-actor.
