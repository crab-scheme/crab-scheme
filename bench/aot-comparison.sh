#!/usr/bin/env bash
# AOT vs other-tier perf comparison for the microbench corpus.
#
# For each .scm in `bench/microbench/scheme/`, attempts:
#   1. AOT compile via `crabscheme aot <bench>.scm --entry <fn> --build`.
#   2. If the build succeeds, time the resulting native binary at the
#      bench's canonical N (per BENCH_N table below).
#   3. Compare to the JIT, VM, walker, and rustc -O timings.
#
# Benches that fail to AOT (because the bytecode → RIR translator
# emits an Inst cs-aot doesn't yet handle — typically EnvLookupAny
# or general Call for non-self cross-procedure references) are
# reported as `AOT: unsupported` with the specific Inst name. This
# is the "RC2 AOT coverage scorecard" view.
#
# Why a separate script and not extension to `microbench/run.sh`:
# the existing harness invokes each impl on the full .scm file
# (which includes top-level driver code like `(display (fib 25))`
# that AOT can't yet handle). The AOT path extracts just the
# defined function and drives it from CLI args, which is a different
# invocation shape. Keeping a separate file avoids coupling.

set -e
cd "$(dirname "$0")"
ROOT=$(cd .. && pwd)

CRABSCHEME="$ROOT/target/release/crabscheme"

# Build crabscheme if missing.
if [ ! -x "$CRABSCHEME" ]; then
  echo "==> building crabscheme (release)..."
  (cd "$ROOT" && cargo build --release -p cs-cli >/dev/null 2>&1)
fi
if [ ! -x "$CRABSCHEME" ]; then
  echo "expected $CRABSCHEME — `cargo build --release -p cs-cli` failed?"
  exit 1
fi

# Per-bench config: entry function name and the canonical N to time
# at. The N values match what `microbench/scheme/<bench>.scm` uses
# for the in-file driver — kept identical so AOT timings line up
# with the other-tier rows in `microbench/run.sh`.
declare -A BENCH_ENTRY
declare -A BENCH_ARGS
BENCH_ENTRY[fib]="fib";        BENCH_ARGS[fib]="35"
BENCH_ENTRY[ack]="ack";        BENCH_ARGS[ack]="3 6"
BENCH_ENTRY[tak]="tak";        BENCH_ARGS[tak]="18 12 6"
BENCH_ENTRY[nqueens]="nqueens"; BENCH_ARGS[nqueens]="9"
BENCH_ENTRY[mandelbrot]="mandelbrot"; BENCH_ARGS[mandelbrot]="80"
BENCH_ENTRY[spectral-norm]="spectral-norm"; BENCH_ARGS[spectral-norm]="100"
BENCH_ENTRY[binary-trees]="run"; BENCH_ARGS[binary-trees]="10"
BENCH_ENTRY[alloc-stress]="alloc-stress"; BENCH_ARGS[alloc-stress]="200"
# nbody uses top-level vector globals so it can't AOT today; omitted.

OUT_DIR="$ROOT/target/aot-comparison"
mkdir -p "$OUT_DIR"

printf "%-15s %-22s  %s\n" "benchmark" "AOT (s, best of 3)" "status"
echo   "---------------------------------------------------------------"

for bench in fib ack tak nqueens mandelbrot spectral-norm binary-trees alloc-stress; do
  src="$ROOT/bench/microbench/scheme/${bench}.scm"
  entry="${BENCH_ENTRY[$bench]}"
  args="${BENCH_ARGS[$bench]}"
  proj_dir="$OUT_DIR/$bench"
  bin="$proj_dir/target/release/$entry"

  # Skip rebuild if the binary is fresher than the source.
  if [ ! -x "$bin" ] || [ "$src" -nt "$bin" ]; then
    # Try to AOT. Failure modes: parse/expand/compile error, or
    # bytecode → RIR error (the iter-1 RIR coverage gap), or cargo
    # build error.
    build_log=$(mktemp -t aot-comparison.XXXX.log)
    if ! "$CRABSCHEME" aot "$src" --entry "$entry" -o "$proj_dir" --build \
         >"$build_log" 2>&1; then
      # Extract the most useful failure-line from the log. The "aot:
      # emitted project at ..." + "entry: ..." lines are success-path
      # output and never indicate a problem; the failure is in the
      # cargo-build error, an `UnsupportedInst/Term`, or a parse /
      # expand / compile diagnostic. Match those specifically.
      reason=$(grep -E 'UnsupportedInst|UnsupportedTerm|cargo build failed|emit error|parse error|expand error|compile error|bytecode→RIR error' \
                    "$build_log" | head -1)
      [ -z "$reason" ] && reason="(see $build_log for details)"
      printf "%-15s %-22s  AOT failed: %s\n" "$bench" "—" "$reason"
      continue
    fi
    rm -f "$build_log"
  fi

  if [ ! -x "$bin" ]; then
    printf "%-15s %-22s  build produced no binary at %s\n" "$bench" "—" "$bin"
    continue
  fi

  # Time best-of-3 wall-clock.
  best=""
  # shellcheck disable=SC2086
  for _ in 1 2 3; do
    t=$( { /usr/bin/time -p "$bin" $args >/dev/null; } 2>&1 \
         | awk '/^real/ {print $2}')
    if [ -z "$best" ] || awk "BEGIN{exit !($t < $best)}"; then
      best=$t
    fi
  done
  printf "%-15s %-22s  OK (entry=%s args=%q)\n" "$bench" "${best}s" "$entry" "$args"
done

echo
echo "(see bench/microbench/run.sh for walker / VM / JIT / Chez / Guile / Gambit / rustc-O timings on the same benches)"
