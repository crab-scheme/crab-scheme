#!/usr/bin/env bash
# Scaled VM-vs-JIT microbench (hyperfine).
#
# run.sh uses small N so every benchmark fits the walker's stack and
# all comparison Schemes run — but at those N the crabscheme tiers
# finish near the ~sub-10ms process-startup floor, so the JIT numbers
# are startup-bound, not compute-bound. This script scales N up so
# compute dominates, and uses hyperfine (warmup + repeats) to compare
# the bytecode VM against the Cranelift JIT honestly.
#
# Requires hyperfine on PATH (devenv shell provides it).
set -eu
cd "$(dirname "$0")"
ROOT=$(cd ../.. && pwd)
CS="$ROOT/target/release/crabscheme"
SRC="$ROOT/bench/microbench/scheme"

if ! command -v hyperfine >/dev/null 2>&1; then
  echo "hyperfine not found on PATH (try: devenv shell)"; exit 1
fi
echo "==> building crabscheme (release)..."
(cd "$ROOT" && cargo build --release -p cs-cli >/dev/null 2>&1)

SCALED=$(mktemp -d)
trap 'rm -rf "$SCALED"' EXIT
# name:sed-substitution pairs that scale each benchmark's N so the JIT
# tier runs ~0.3-2s (compute >> startup).
sed 's/(define n 25)/(define n 32)/'    "$SRC/fib.scm"           > "$SCALED/fib.scm"
sed 's/(define n 50)/(define n 500)/'   "$SRC/spectral-norm.scm" > "$SCALED/spectral-norm.scm"
sed 's/(define depth 10)/(define depth 16)/' "$SRC/binary-trees.scm" > "$SCALED/binary-trees.scm"
sed 's/(define n 200)/(define n 6000)/' "$SRC/alloc-stress.scm"  > "$SCALED/alloc-stress.scm"

for b in fib spectral-norm binary-trees alloc-stress; do
  echo "### $b"
  hyperfine --warmup 1 --runs 5 -N \
    "$CS --tier vm run $SCALED/$b.scm" \
    "$CS --tier vm-jit run $SCALED/$b.scm"
done
