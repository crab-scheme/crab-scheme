# RC3 Phase 2 iter 2.4 — Closure Capture Design (in progress)

> Status: cs-vm side shipped (iter 2.4 Step 0). cs-aot side
> deferred to a focused multi-day session — design captured here.
> Predecessor: iter 2.2 design doc + iter 2.1 / 2.2 / 2.3 commits.

## What iter 2.4 Step 0 ships (this session)

`cs_vm::vm`'s public AOT-procedure API extended for captures:

- `pub type AotDispatchFn = unsafe extern "C" fn(captures: *const i64, n_captures: usize, args: *const i64, n_args: usize) -> i64;`
  Updated from iter 2.1's args-only signature to take captures
  alongside args.
- `pub unsafe extern "C" fn vm_alloc_aot_procedure_with_captures(disp_fn, arity, captures_ptr, n_captures) -> i64`
  New allocator for capturing closures.
- `VmAotClosure` gets a `captures: Vec<i64>` field. Stored as
  NB-encoded i64s (one strong ref each).
- `vm_call_aot_procedure` updated to pass captures + count
  alongside args + count to the dispatch fn.

Test `aot_procedure_with_captures_roundtrip` exercises the round
trip: alloc a closure with `captures = [100]`, invoke with
`args = [23]`, dispatch fn returns `captures[0] + args[0] = 123`.

cs-vm tests: 66 → 67.

## What iter 2.4 still needs (cs-aot side)

The cs-vm API is in place; the cs-aot side that generates
closures + threads captures through the translator is the real
remaining work. ~1-2 weeks of focused engineering.

### Step 1: capture analysis in the translator

When `cs_vm::jit_translate::bytecode_to_rir_aot` translates a
nested lambda, it needs to:

1. Analyze the lambda's body Insts for `EnvLookup(_, sym)` /
   `EnvLookupAny(_, sym)` referring to syms NOT in the lambda's
   own params or EnvDefineLocal.
2. Record those syms as the lambda's **capture list** (in
   declaration order — stable for the MakeClosure side).
3. Rewrite the body's EnvLookup references: each `EnvLookup(dst,
   sym)` for a captured sym becomes a "load from captures slice
   at index N" inst.

cs-rir would need a new Inst for this — call it
`Inst::LoadCapture(dst: Value, capture_idx: u32)`. Or repurpose
EnvLookup with a special source tag.

### Step 2: capture-list metadata on `cs_rir::Function`

The lambda's capture list needs to be visible to cs-aot at emit
time so the AOT'd dispatch wrapper can unpack captures with the
right sym → idx mapping.

```rust
pub struct Function {
    ...existing fields...
    pub lambda_index: Option<usize>,
    /// RC3 iter 2.4 — captured free-var syms in declaration order.
    /// Empty for non-capturing functions. cs-aot's MakeClosure
    /// emission uses this to know how many captures to gather +
    /// in what order.
    pub captures: Vec<Symbol>,  // NEW
}
```

### Step 3: cs-aot dispatch-wrapper emission with captures

The per-Function dispatch wrapper (today written by
`write_aot_dispatch_wrapper` in cs-aot/src/project.rs) becomes:

```rust
#[no_mangle]
pub unsafe extern "C" fn matrix_elt_aot_dispatch(
    captures: *const i64,
    n_captures: usize,
    args: *const i64,
    n_args: usize,
) -> i64 {
    debug_assert_eq!(n_captures, 2);  // matrix-elt captures n, base
    debug_assert_eq!(n_args, 2);
    matrix_elt(
        *captures.add(0),   // n
        *captures.add(1),   // base
        *args.add(0),       // i
        *args.add(1),       // j
    )
}
```

The wrapper's typed inner fn (`matrix_elt`) gets EXTRA params for
captures — the translator rewrote `EnvLookup(n)` to a load from
the first capture param, etc.

### Step 4: cs-aot MakeClosure emission with captures

Today's emit (iter 2.2):

```rust
unsafe { cs_vm::vm::vm_alloc_aot_procedure(matrix_elt_aot_dispatch as usize, 2u32) }
```

iter 2.4:

```rust
{
    let __captures: [i64; 2] = [v_n, v_base];  // values caller holds
    unsafe {
        cs_vm::vm::vm_alloc_aot_procedure_with_captures(
            matrix_elt_aot_dispatch as usize,
            2u32,
            __captures.as_ptr(),
            2,
        )
    }
}
```

cs-aot's `inst_rhs` MakeClosure arm needs to know which VALUES
in the caller's scope to gather as captures. That info lives in
the cs-rir Inst — `MakeClosure(dst, lambda_idx)` would expand to
`MakeClosure(dst, lambda_idx, captures: Vec<Value>)` carrying
the SSA values to gather.

### Step 5: tests

Once Steps 1-4 land, the existing `bench/aot-comparison.sh`
microbench corpus should AOT cleanly. Specifically:

- spectral-norm: `matrix-elt` captures `n`, `base`; `mul-Av`'s
  inner `(lambda (i) ...)` captures `v`, `out`.
- nqueens: nested `(lambda (col) ...)` captures `row`, `placed`.
- mandelbrot: similar pattern.

A diff test (extending `diff_aot_vs_jit.rs`) asserts AOT outputs
match JIT outputs for each.

## Risks + open questions

### Refcount safety

Captures carry NB carriers with strong refcounts on their
payloads. The cs-aot side gathers them via a stack `[i64; N]`;
the cs-vm side stores them in `VmAotClosure::captures` (Vec).
Need to confirm:
- Are the captures `clone`d when allocated (incref) or moved
  (transferred ownership)?
- When the closure is freed (proc_table drop), does each capture
  get its refcount decremented?

Current `vm_alloc_aot_procedure_with_captures` MOVES captures
into the Vec (no incref). Caller must NOT touch them after the
call. That's the consume-on-use convention but may surprise
callers used to incref-style passing.

### GC tracing

VmAotClosure's `Trace` impl is a no-op. If captures hold Gc-
allocated pointers, they're rooted via the Rc count alone — not
via the GC tracer. If the closure is reachable but the Gc
tracer doesn't see the captures, premature collection could
free them. Need to either:
- Add real tracing to VmAotClosure (visit each capture)
- Ensure the GC tracer follows Rc chains (it might already; the
  proc_table's drop is the freeing path, and that's Rc-based)

Conservative posture: ship iter 2.4 with no-op Trace + a note
that captures with Gc-tracked payloads are at risk. Real fix in
a follow-up.

### Performance

Each MakeClosure now copies N values into a fresh Vec (heap
allocation per closure). For tight loops creating closures
(spectral-norm's per-iteration `(lambda (i) ...)`), this could
be allocation-heavy.

Optimization paths post-iter-2.4:
- Stack-allocate small capture lists (inline-storage like
  smallvec)
- Pool captures Vec allocations
- Type-feedback: when the JIT proves a closure isn't escaping
  the call site, replace with inline call

## Sequencing

iter 2.4's cs-aot side (Steps 1-4) is the work that should land
together. Step 5 is the validation gate.

Realistic effort: 1-2 weeks. The cs-rir Inst changes (LoadCapture
+ MakeClosure-with-captures-values) touch a lot of code paths
across cs-vm's jit_translate + the JIT lowering. Each path
needs to be audited.

## What's actually shipped

- ✅ cs-vm side complete (iter 2.4 Step 0)
- ✅ Captures round-trip test
- ✅ Design captured for the cs-aot side

What's NOT shipped:
- cs-rir `Function::captures` field (Step 2)
- cs-rir `LoadCapture` Inst (Step 1)
- cs-rir MakeClosure variant with captures (Step 4)
- cs-vm translator changes to populate the capture list (Step 1)
- cs-aot dispatch-wrapper changes (Step 3)
- cs-aot MakeClosure emission with captures (Step 4)
- bench / diff tests (Step 5)

For the bench scorecard to move from 2/8 to 6+/8, all of the
above need to land coherently. That's a dedicated rc4-or-later
session.
