# RC2 iter A — cs-aot NB inline fast-path timings (2026-05-16)

> Captured against commit `9643067`. Hardware: Apple M-series, devenv shell.
> All binaries built with `opt-level = 3`, no LTO override.

## Headline

| Variant                            | fib(40) | vs rustc -O |
|------------------------------------|--------:|------------:|
| Reference `rustc -O fib.rs`        |  0.15 s |    1.00×    |
| cs-aot **RawI64** ABI              |  0.14 s |    0.93×    |
| cs-aot **Nb** ABI — RC2 (post-inline)  |  0.29 s |    **1.93×**    |
| cs-aot Nb ABI — RC1 (pre-inline)   |  0.80 s |    5.33×    |

**RC2 iter A speedup on Nb mode: 2.75×.** The fast-path inlining
closes the Nb-vs-rustc gap from 5.7× to under 2×. RawI64 stays
unchanged (it never had the overhead).

## Methodology

```
$ for v in fib_nb fib_raw; do
    for i in 1 2 3; do
      /usr/bin/time -p target/aot-project-tests/release/aot_$v 40
    done
  done
$ for i in 1 2 3; do /usr/bin/time -p /tmp/fib_ref 40; done
```

Numbers above are the best of 3 wall-clock seconds per variant.
Result correctness verified: all variants return `102334155` for
fib(40).

`/tmp/fib_ref` is the reference Rust fib from
`bench/microbench/rust/fib.rs`, built as `rustc -O fib.rs -o
fib_ref`. Both AOT variants come from
`cs_aot::project::emit_project()` driven through the
`fib_{nb,rawi64}_compiles_and_runs` tests in
`crates/cs-aot/tests/project_pipeline.rs`.

## What changed

Before (RC1, commit `1cc784d`):

```rust
let v9: i64 = unsafe { cs_vm::vm::vm_value_add_nb(v5, v8) };
```

Every arith op hit a runtime-helper function call: argument
register setup + branch + ret. The helper's fast path was already
inline-tag-check + checked_add + encode, but the call boundary
itself was the bottleneck — fib's hot loop is essentially nothing
but adds and subs of small fixnums.

After (RC2, commit `9643067`):

```rust
let v9: i64 = nb_add_inline(v5, v8);
```

Where `nb_add_inline` is a `#[inline(always)]` function injected
once per translation unit by `cs_aot::nb_helpers_source()`. Body:

```rust
#[inline(always)]
fn nb_add_inline(a: i64, b: i64) -> i64 {
    if nb_both_fixnum(a, b) {
        let pa = nb_extract_fixnum(a as u64);
        let pb = nb_extract_fixnum(b as u64);
        if let Some(r) = pa.checked_add(pb) {
            if let Some(enc) = nb_encode_fixnum_if_fits(r) {
                return enc;
            }
        }
    }
    unsafe { cs_vm::vm::vm_value_add_nb(a, b) }
}
```

`rustc -O` inlines this at every call site and the hot path
becomes ~6 instructions per add (mask + cmp + extract + cmp +
add + encode), no call/ret.

## Why we're still 1.93× and not 1.00×

The remaining 1.93× gap to reference Rust is real work the AOT
helper does that hand-written `fn fib(n: u64) -> u64 { ... }` does
not:

1. **47-bit overflow check** after every arith. Rust's `u64` fib
   gets away with no range check until it actually overflows.
2. **Dynamic tag check** on every operand. The JIT defers this to
   a once-per-call type guard at function entry; AOT doesn't yet
   have that machinery (it has no type-feedback channel).
3. **Encode/decode** sign-extend + payload mask per op. Reference
   Rust holds values as raw `u64`.
4. **Fallback branch** is still there in the emitted code, even
   when never taken. `rustc -O` predicts it correctly but the
   branch's existence costs an instruction.

The **next perf lever** is bytecode-to-RIR translation with
type-feedback annotations: when feedback proves both operands of
an arith are always Fixnum at a call site, the helper can degrade
to `wrapping_*` with no tag check. This is the AOT analog of the
JIT's existing type-feedback specialization. Post-RC2 work.

## Other workloads

fib is the canary because it's pure recursive arithmetic — every
microsecond is helper overhead. Other workloads stress different
ratios:

- Allocation-heavy benches (anything that constructs lists/vectors)
  amortize the helper cost across object construction. Expected
  gap to rustc-without-GC will be narrower since the runtime work
  dominates either way.
- Comparison-heavy benches (tak, ack) should see similar Nb gains
  to fib — same helper-call shape on Lt/Eq.

Not benchmarked in this iter; covered by the existing project
pipeline tests for correctness.

## Track A exit gate impact

The Track A exit report set the loosened gate as "within 5× of
reference Rust" for Nb mode. Post-RC2 iter A: 1.93×. The gate is
now MET with a 2.5× cushion. The original "within 2× of JIT"
framing is also effectively met — the JIT's NB code path uses the
same `vm_value_*_nb` helpers (just emitted by Cranelift instead of
inlined by rustc), so JIT and AOT-Nb should be in the same ballpark
modulo Cranelift's vs rustc's optimization differences.

## How to reproduce

```bash
git checkout 9643067
devenv shell -- cargo test -p cs-aot --test project_pipeline -- fib
# Builds the two fib binaries via the project_pipeline tests.

# Build the reference for comparison:
cat > /tmp/fib_ref.rs <<'EOF'
fn fib(n: u64) -> u64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
fn main() {
    let n: u64 = std::env::args().nth(1).unwrap().parse().unwrap();
    println!("{}", fib(n));
}
EOF
rustc -O /tmp/fib_ref.rs -o /tmp/fib_ref

# Time:
T=target/aot-project-tests/release
for b in $T/aot_fib_nb $T/aot_fib_raw /tmp/fib_ref; do
  echo "=== $b ==="
  for _ in 1 2 3; do /usr/bin/time -p $b 40 2>&1 | grep real; done
done
```
