#!/usr/bin/env bash
# warmup_curve.sh — run the n-body warmup-curve benchmark across each
# CrabScheme tier (and the Rust reference + any other Scheme impls
# present on $PATH), parse the per-round timings, and emit two outputs:
#
#   1. A sampled comparison table written to stdout — shows the timing
#      for a handful of representative rounds (0, 1, 2, 5, 10, 50, 100,
#      500, last) per implementation, plus min/avg of the last 100
#      rounds (steady-state throughput).
#   2. A per-implementation TSV at $OUT_DIR/<impl>.tsv with every
#      (round, seconds) row, suitable for plotting.
#
# The n-body sources schedule 1500 rounds × 1000 steps. On a modern
# laptop CrabScheme vm-jit completes in ~70s, Rust in ~50ms.
#
# Run from any directory; paths are resolved relative to this script.
set -euo pipefail
cd "$(dirname "$0")"

ROOT=$(cd ../.. && pwd)
SCM="$ROOT/bench/microbench/scheme/nbody.scm"
RUST_SRC="$ROOT/bench/microbench/rust/nbody.rs"
RUST_OUT="$ROOT/target/release-microbench/nbody"
CS="$ROOT/target/release/crabscheme"
OUT_DIR="${OUT_DIR:-$ROOT/target/warmup_curve}"
mkdir -p "$OUT_DIR"

echo "==> building crabscheme (release)..."
(cd "$ROOT" && cargo build --release -p cs-cli >/dev/null 2>&1)
[ -x "$CS" ] || { echo "missing $CS"; exit 1; }

echo "==> building rust/nbody (-O)..."
if [ ! -x "$RUST_OUT" ] || [ "$RUST_SRC" -nt "$RUST_OUT" ]; then
  mkdir -p "$(dirname "$RUST_OUT")"
  rustc -O "$RUST_SRC" -o "$RUST_OUT"
fi

# Implementation list. Each entry is "name|command" — name labels the
# output, command runs the benchmark and writes per-round lines to
# stdout in the `nbody-round N SECONDS` format.
#
# `crabscheme-walker` (tree-walker tier) is excluded by default: the
# walker doesn't TCO the named-let outer loop and stack-overflows
# before producing useful curve data. Set INCLUDE_WALKER=1 to opt in.
declare -a IMPLS
if [ "${INCLUDE_WALKER:-0}" = "1" ]; then
  IMPLS+=("crabscheme-walker|$CS --tier walker run $SCM")
fi
IMPLS+=("crabscheme-vm|$CS --tier vm run $SCM")
IMPLS+=("crabscheme-jit|$CS --tier vm-jit run $SCM")
IMPLS+=("rust|$RUST_OUT")
# Racket and Gambit understand R7RS-small + current-second; Chez
# would need a current-time shim, so we leave it out.
if command -v racket >/dev/null 2>&1; then
  IMPLS+=("racket|racket $SCM")
fi
if command -v gsi >/dev/null 2>&1; then
  IMPLS+=("gambit|gsi $SCM")
fi

# Run each impl, capture lines like `nbody-round N SECONDS`, store as
# (round, seconds) TSV in $OUT_DIR/<name>.tsv.
echo "==> running benchmarks..."
for spec in "${IMPLS[@]}"; do
  name=${spec%%|*}
  cmd=${spec#*|}
  echo "    $name..."
  log="$OUT_DIR/$name.log"
  tsv="$OUT_DIR/$name.tsv"
  if ! $cmd > "$log" 2>&1; then
    echo "        FAILED — see $log"
    continue
  fi
  awk '
    /^nbody-round / { print $2 "\t" $3 }
  ' "$log" > "$tsv"
done

# Fixed column width. 20 cols per impl is wide enough for the
# longest fixed-precision time strings we print (12 chars) plus
# breathing room.
COL_W=20
LABEL_W=18

cell() {
  printf "%${COL_W}s" "$1"
}

# Format a per-round time consistently across impls (6 fractional
# digits so the display layout stays stable).
format_time() {
  if [ -z "$1" ] || [ "$1" = "-" ]; then
    printf "%s" "-"
    return
  fi
  printf "%.6f" "$1"
}

print_header() {
  printf "%-${LABEL_W}s" "$1"
  for spec in "${IMPLS[@]}"; do
    name=${spec%%|*}
    cell "$name"
  done
  echo
}

SAMPLES=(0 1 2 5 10 50 100 500 1000 1499)
echo
echo "Per-round timings (seconds) for selected sample rounds:"
echo
print_header "round"
for r in "${SAMPLES[@]}"; do
  printf "%-${LABEL_W}s" "round $r"
  for spec in "${IMPLS[@]}"; do
    name=${spec%%|*}
    tsv="$OUT_DIR/$name.tsv"
    if [ -s "$tsv" ]; then
      val=$(awk -v r="$r" '$1 == r { print $2; exit }' "$tsv")
      val=$(format_time "$val")
    else
      val="-"
    fi
    cell "$val"
  done
  echo
done

# Steady-state summary: min and average of the last 100 rounds.
echo
echo "Steady-state (rounds 1400..1499):"
echo
print_header "metric"

printf "%-${LABEL_W}s" "min sec/round"
for spec in "${IMPLS[@]}"; do
  name=${spec%%|*}
  tsv="$OUT_DIR/$name.tsv"
  if [ -s "$tsv" ]; then
    val=$(awk '$1 >= 1400 && $1 <= 1499 {
        if (min == "" || $2+0 < min+0) min = $2
    } END { if (min == "") print "-"; else printf "%.6f", min }' "$tsv")
  else
    val="-"
  fi
  cell "$val"
done
echo

printf "%-${LABEL_W}s" "avg sec/round"
for spec in "${IMPLS[@]}"; do
  name=${spec%%|*}
  tsv="$OUT_DIR/$name.tsv"
  if [ -s "$tsv" ]; then
    val=$(awk '$1 >= 1400 && $1 <= 1499 {
        s += $2; n++
    } END { if (n == 0) print "-"; else printf "%.6f", s/n }' "$tsv")
  else
    val="-"
  fi
  cell "$val"
done
echo

# Speedup vs crabscheme-vm (the no-JIT baseline) at steady state.
vm_tsv="$OUT_DIR/crabscheme-vm.tsv"
if [ -s "$vm_tsv" ]; then
  vm_avg=$(awk '$1 >= 1400 && $1 <= 1499 { s += $2; n++ } END { if (n) print s/n }' "$vm_tsv")
  printf "%-${LABEL_W}s" "× faster than vm"
  for spec in "${IMPLS[@]}"; do
    name=${spec%%|*}
    tsv="$OUT_DIR/$name.tsv"
    if [ -s "$tsv" ]; then
      avg=$(awk '$1 >= 1400 && $1 <= 1499 { s += $2; n++ } END { if (n) print s/n }' "$tsv")
      ratio=$(awk -v a="$vm_avg" -v b="$avg" 'BEGIN { if (b > 0) printf "%.2fx", a/b; else print "-" }')
    else
      ratio="-"
    fi
    cell "$ratio"
  done
  echo
fi

# Cold/hot ratio per impl: how much faster steady-state is than round 0.
echo
printf "%-${LABEL_W}s" "warmup gain"
for spec in "${IMPLS[@]}"; do
  name=${spec%%|*}
  tsv="$OUT_DIR/$name.tsv"
  if [ -s "$tsv" ]; then
    ratio=$(awk '
      $1 == 0 { cold = $2 }
      $1 >= 1400 && $1 <= 1499 { s += $2; n++ }
      END {
        if (cold == "" || n == 0 || s == 0) { print "-" }
        else { printf "%.2fx", cold * n / s }
      }
    ' "$tsv")
  else
    ratio="-"
  fi
  cell "$ratio"
done
echo

echo
echo "Per-impl TSVs at $OUT_DIR/<impl>.tsv (round<TAB>seconds)."
echo "Done."
