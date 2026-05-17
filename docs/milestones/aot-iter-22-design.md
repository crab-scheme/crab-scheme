# RC3 Phase 2 iter 2.2 — MakeClosure Lowering Design (WIP)

> Status: Design captured at iter 2.1 close. Implementation deferred
> to a dedicated session; iter 2.1 (`vm_alloc_aot_procedure` +
> `vm_call_aot_procedure`) shipped as the runtime foundation.

## Problem statement

cs-rir's `Inst::MakeClosure(dst, lambda_idx)` uses a `usize` index
into the original bytecode's `lambdas` vec. cs-aot operates on
`cs_rir::Function` values directly — it has no knowledge of which
lambda index in some original Bytecode each Function came from, so
it can't lower `MakeClosure(_, N)` without that mapping.

## What iter 2.1 ships

- `pub type AotDispatchFn = unsafe extern "C" fn(*const i64, usize) -> i64;`
- `pub struct VmAotClosure { fn_ptr, arity, name }` impl Procedure + Trace.
- `pub unsafe extern "C" fn vm_alloc_aot_procedure(disp_fn: usize, arity: u32) -> i64`
- `pub unsafe extern "C" fn vm_call_aot_procedure(proc_nb: i64, args_ptr: *const i64, n_args: usize) -> i64`

cs-aot calls into these via `unsafe { cs_vm::vm::vm_alloc_aot_procedure(...) }`
once iter 2.2 lands the emission.

## What iter 2.2 needs

### Step 1: thread lambda names through cs-aot's project emitter

`cs_aot::project::emit_project(funcs, out_dir, opts)` already takes
a slice of Functions. iter 2.2 either:

**Option A (preferred): add `lambda_index: Option<usize>` to `Function`.**
A new field on `cs_rir::Function` recording which bytecode lambda
index this function came from. The bytecode→RIR translator sets it
when it knows; cs-aot tests setting it manually.

```rust
pub struct Function {
    pub name: String,
    pub params: Vec<(Value, Type)>,
    pub return_type: Type,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
    pub lambda_index: Option<usize>,  // NEW
}
```

emit_project then builds a `HashMap<usize, &str>` lookup table from
the funcs slice's `lambda_index` field. `MakeClosure(dst, N)` lowers
to a call resolving N via this table.

**Option B (alternative): pass a side-channel table.**

```rust
emit_project(funcs, lambda_name_map: &HashMap<usize, String>, ...)
```

Caller (cs-cli / tests) builds the map. Cleaner — no cs-rir change.
But requires every caller to construct the map. Option A pushes the
responsibility to the translator (already has the info).

Recommend Option A. cs-rir change is small + makes the relationship
explicit.

### Step 2: emit a dispatch wrapper per Function

cs-aot currently emits each Function as:

```rust
pub extern "C" fn fact(v0: i64) -> i64 { ... }
```

iter 2.2 adds a wrapper:

```rust
#[no_mangle]
pub unsafe extern "C" fn fact_aot_dispatch(args: *const i64, n: usize) -> i64 {
    debug_assert_eq!(n, 1);
    fact(unsafe { *args })
}
```

One per Function. Cheap at runtime; cargo's --release inlines them.

### Step 3: lower `MakeClosure(dst, N)` in cs-aot

In `inst_rhs`'s match (in src/lib.rs), add an arm for `Inst::
MakeClosure(dst, lambda_idx)`. Look up the name from the project-
level lookup table (Step 1's `HashMap<usize, &str>`):

```rust
(Inst::MakeClosure(dst, lambda_idx), EmitMode::Nb) => {
    let name = lambda_name_map.get(lambda_idx)
        .ok_or_else(|| AotError::UnsupportedInst("MakeClosure refers to non-AOT-emitted lambda"))?;
    let arity = funcs.iter().find(|f| f.name == name).unwrap().params.len();
    let dispatch = format!("{name}_aot_dispatch");
    (*dst, format!(
        "unsafe {{ cs_vm::vm::vm_alloc_aot_procedure({dispatch} as usize, {arity}) }}"
    ))
}
```

The `inst_rhs` signature would need a `&HashMap<usize, &str>` arg
threaded through the emit pipeline. Functions with MakeClosure
referring to a lambda OUTSIDE the AOT-emitted set fail with a
clean diagnostic (the lambda is built-in or otherwise unreachable).

### Step 4: lower `Inst::Call(dst, callee, args)` for general dispatch

Today `Inst::CallSelf(dst, args)` lowers to a direct Rust call.
iter 2.3 adds `Inst::Call(dst, callee, args)` for cases where the
callee is a procedure value (not necessarily self).

```rust
(Inst::Call(dst, callee, args), _) => {
    // Build an args slice on the stack, call through the
    // vm_call_aot_procedure helper.
    let args_init = args.iter().enumerate()
        .map(|(i, v)| format!("args_arr[{i}] = v{}", v.0))
        .collect::<Vec<_>>()
        .join("; ");
    (*dst, format!(
        "{{ let mut args_arr: [i64; {n}] = [0; {n}]; \
           {args_init}; \
           unsafe {{ cs_vm::vm::vm_call_aot_procedure(\
             v{callee}, args_arr.as_ptr(), {n}\
           ) }} }}",
        n = args.len(),
        callee = callee.0,
    ))
}
```

### Step 5: closure capture (iter 2.4)

The above handles NON-capturing lambdas. For capturing closures
(e.g., spectral-norm's `matrix-elt` referencing the outer `n`),
the dispatch wrapper would need access to the captured env values.

Two approaches:

**5a. Pass captures as extra args.** The translator detects which
free vars a lambda uses; rewrites the lambda's signature to take
them as additional params. MakeClosure becomes "allocate a closure
that remembers these args". Call dispatches all captured + supplied
args. This requires translator-level work (rewriting lambda sigs +
MakeClosure call sites) and a new VmAotClosure variant that
remembers the captured args.

**5b. Reuse cs-vm's Env install.** Add a public API
`pub unsafe extern "C" fn vm_install_jit_caller_env(env_ptr: *const
Env) -> *const Env;` that installs JIT_CALLER_ENV for the duration
of an AOT call. The AOT'd dispatch wrapper installs the env around
the inner fn call, restores on return. The lambda body uses normal
EnvLookupAny which now finds the captured bindings.

5b is more architecturally similar to what the JIT does. 5a is
simpler but doesn't compose with the rest of the runtime's
env-based semantics.

### Step 6: tests

For iter 2.2 (non-capturing):

```scheme
(define (square n) (* n n))
(define (apply-square xs) (map square xs))  ; map expects a Procedure
```

Wait — `map` is a builtin, not AOT-emitted. iter 2.2 wouldn't help
unless we ALSO handle calls to built-in procedures, which is iter
2.3's broader scope. A truer 2.2-only test:

```scheme
(define (compose f g) (lambda (x) (f (g x))))  ; nested lambda
;; ... actually this captures `f` and `g`, so it's iter 2.4 territory.
```

A pure non-capturing test that's interesting is hard to construct
without contrived examples. Most real nested lambdas capture.

Honest assessment: **iter 2.2 alone doesn't unlock real-world
benches**. Even when paired with 2.3 (general Call), iter 2.4
(capture) is the gate. Plan to do 2.2-2.4 as one tightly-coupled
push, not three separate iters.

## Estimated effort post-2.1

- iter 2.2 (lambda-name threading + dispatch wrappers + MakeClosure
  emit): ~1 week. Architectural change to cs-rir (Option A above) is
  the high-risk piece.
- iter 2.3 (general Call emit): ~3 days. Mechanical once 2.2 is in.
- iter 2.4 (capture, choice between 5a/5b): ~1.5 weeks. The
  bigger of the two. 5b path needs cs-vm to expose env install
  publicly + AOT to construct + populate an Env from captured
  values at MakeClosure time.
- 2.x tests: ~3 days for an end-to-end "closure-using Scheme
  program AOTs to native binary" demo.

Total: ~3 weeks of focused work post-iter-2.1. Sequencing matters
— 2.2 → 2.3 → 2.4 in order, with 2.4 likely getting its own design
ADR before implementation.

## What this iter (2.1) doesn't change

iter 2.1 only adds the API surface. cs-aot today still rejects
MakeClosure with the clean diagnostic from iter 4.1. Existing
behavior is unchanged for users who don't write programs hitting
the new API.
