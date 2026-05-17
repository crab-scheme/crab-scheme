#!/usr/bin/env bash
# Typer perf scorecard: for each microbench, AOT-compile via
# `crabscheme aot --multi --build`, run it, and time vs the
# Rust reference. Prints a markdown table.
#
# Phase 5+ inner-let inference + recommendations #2-#5 are
# all annotation-agnostic, so the speedup applies to the
# stock benchmarks without modification. Benches that fail
# to AOT-compile are reported as `aot:fail`.

set -eu
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUTDIR="${TMPDIR:-/tmp}/cs-typer-scorecard"
ITERS="${ITERS:-50}"
mkdir -p "$OUTDIR"

echo "==> build release cs-cli"
(cd "$REPO" && cargo build --release -p cs-cli --features aot --bin crabscheme 2>&1 | tail -1)
CLI="$REPO/target/release/crabscheme"

# (bench-name, entry-fn-with-args). The args mirror the
# Rust reference's default N so the comparison is apples
# to apples.
declare -a BENCHES=(
  "fib:fib 25"
  "tak:tak 18 12 6"
  "ack:ack 3 6"
  "mandelbrot:mandelbrot 100"
  "mandelbrot-typed:mandelbrot 100"
  "nqueens:nqueens 8"
  "spectral-norm:spectral 100"
  "nbody:advance-loop 200"
)

aot_one() {
  local bench="$1"
  local scm="$REPO/bench/microbench/scheme/${bench}.scm"
  local out="$OUTDIR/${bench}-aot"
  rm -rf "$out"
  if ! "$CLI" aot --multi "$scm" -o "$out" --build > "$OUTDIR/${bench}.log" 2>&1; then
    return 1
  fi
  # The output binary's name is derived from the bench
  # source's basename — find it under target/release.
  find "$out/target/release" -maxdepth 1 -type f -perm -u+x \
       ! -name '*.d' ! -name '*.dSYM' 2>/dev/null | head -1
}

time_one() {
  local bin="$1"; shift
  /usr/bin/time -p sh -c "for i in \$(seq 1 $ITERS); do '$bin' $* > /dev/null; done" 2>&1 \
    | grep '^real' | awk '{print $2}'
}

# Build Rust refs.
RUST_DIR="$REPO/bench/microbench/rust"
RUST_OUT="$REPO/target/release-microbench"
mkdir -p "$RUST_OUT"
for src in "$RUST_DIR"/*.rs; do
  name=$(basename "$src" .rs)
  out="$RUST_OUT/$name"
  if [ ! -x "$out" ] || [ "$src" -nt "$out" ]; then
    echo "==> rustc -O $name"
    rustc -C opt-level=3 "$src" -o "$out" 2>&1 | tail -1
  fi
done

echo ""
echo "| Benchmark | Rust ref | CrabScheme AOT | Ratio | Status |"
echo "|-----------|----------|----------------|-------|--------|"

for spec in "${BENCHES[@]}"; do
  bench="${spec%%:*}"
  invocation="${spec#*:}"
  fn="${invocation%% *}"
  args="${invocation#* }"
  rust_bin="$RUST_OUT/$bench"
  # Strip "-typed" suffix for Rust ref name (typed and
  # untyped both compare to the same Rust binary).
  if [ ! -x "$rust_bin" ]; then
    rust_bin="$RUST_OUT/${bench%-typed}"
  fi
  if [ ! -x "$rust_bin" ]; then
    printf "| %-22s | n/a | n/a | n/a | rust:nobin |\n" "$bench"
    continue
  fi
  # AOT.
  if ! crab_bin=$(aot_one "$bench"); then
    rt=$(time_one "$rust_bin" 2>/dev/null || echo "ERR")
    printf "| %-22s | %ss | n/a | n/a | aot:fail |\n" "$bench" "$rt"
    continue
  fi
  # Time both — interleaved single runs (no warmup), since
  # the iter count amortizes startup.
  rt=$(time_one "$rust_bin")
  ct=$(time_one "$crab_bin" $fn $args 2>/dev/null || echo "ERR")
  if [ "$ct" = "ERR" ] || [ -z "$ct" ]; then
    printf "| %-22s | %ss | n/a | n/a | aot:runfail |\n" "$bench" "$rt"
  else
    ratio=$(awk -v c="$ct" -v r="$rt" 'BEGIN { if (r > 0) printf "%.1fx", c/r; else print "n/a" }')
    printf "| %-22s | %ss | %ss | %s | ok |\n" "$bench" "$rt" "$ct" "$ratio"
  fi
done

echo ""
echo "(Wall time for $ITERS iterations of each.)"
