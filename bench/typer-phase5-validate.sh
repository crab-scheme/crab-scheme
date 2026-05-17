#!/usr/bin/env bash
# Typer Phase 5 — bench validation script.
#
# Compares the AOT'd binary's runtime perf for a hot
# numeric microbenchmark with and without the typed-define
# annotations. Phase 5.5 deliverable per
# docs/milestones/typer-plan.md.
#
# Usage:  bench/typer-phase5-validate.sh [N] [iters]
# Defaults: N=100, iters=20.

set -euo pipefail
N="${1:-100}"
ITERS="${2:-20}"

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUTDIR="${TMPDIR:-/tmp}/cs-typer-phase5-validate"
rm -rf "$OUTDIR"
mkdir -p "$OUTDIR"

echo "==> Building cs-cli (release)"
cargo build --release -p cs-cli --features aot --bin crabscheme 2>&1 | tail -1
CLI="${REPO}/target/release/crabscheme"

for variant in mandelbrot mandelbrot-typed; do
  echo "==> AOT-compiling $variant"
  "$CLI" aot --multi "${REPO}/bench/microbench/scheme/${variant}.scm" \
    -o "$OUTDIR/$variant" --build 2>&1 | tail -1
done

# Mandelbrot's entrypoint binary name depends on the
# project name (cargo manifest), which crabscheme derives
# from the source filename. Both projects expose the
# `mandelbrot` Scheme function as an extern entry.
UNTYPED_BIN="$OUTDIR/mandelbrot/target/release/mandelbrot"
TYPED_BIN="$OUTDIR/mandelbrot-typed/target/release/mandelbrot-typed"

echo ""
echo "==> Correctness check at N=$N"
UNTYPED_OUT=$("$UNTYPED_BIN" mandelbrot "$N")
TYPED_OUT=$("$TYPED_BIN" mandelbrot "$N")
echo "  untyped: $UNTYPED_OUT"
echo "  typed  : $TYPED_OUT"
if [ "$UNTYPED_OUT" != "$TYPED_OUT" ]; then
  echo "FAIL — typed/untyped output differ"
  exit 1
fi

echo ""
echo "==> Timing $ITERS iterations at N=$N"
for label in "untyped:$UNTYPED_BIN" "typed:$TYPED_BIN"; do
  name="${label%%:*}"
  bin="${label##*:}"
  echo "  $name"
  /usr/bin/time -p sh -c "for i in \$(seq 1 $ITERS); do $bin mandelbrot $N > /dev/null; done" 2>&1 \
    | sed 's/^/    /'
done

echo ""
echo "Done. Output at $OUTDIR."
