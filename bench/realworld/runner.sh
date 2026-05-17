#!/usr/bin/env bash
#
# Real-world benchmark runner. Phase C of the spec — wraps the
# existing microbenches (and future Tier-2/3 benches) in the
# Phase A/B GC instrumentation, captures JSONL, hands off to
# render.py for the human-readable table.
#
# Usage:
#   bench/realworld/runner.sh                          # all benches, all detected engines/tiers
#   bench/realworld/runner.sh --bench fib              # one bench
#   bench/realworld/runner.sh --engine crabscheme-vm   # one engine
#   bench/realworld/runner.sh --tier vm                # one tier
#   bench/realworld/runner.sh --output results/foo.jsonl
#   bench/realworld/runner.sh --warmup 5 --measure 20  # override defaults
#   bench/realworld/runner.sh --time-budget 120
#
# One subprocess per (engine, bench, tier) — clean heap state per
# run, no cross-bench contamination. Each subprocess emits one
# JSON line to stdout; the runner appends to the JSONL output.

set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REALWORLD="$ROOT/bench/realworld"
HARNESS="$REALWORLD/lib/harness.scm"
SCHEMES="$REALWORLD/schemes"
RESULTS_DIR="$REALWORLD/results"
DEFAULT_OUTPUT="$RESULTS_DIR/latest.jsonl"

# Defaults — match the spec's "10 iters OR 60 s" budget.
WARMUP="${REALWORLD_WARMUP_ITERS:-3}"
MEASURE="${REALWORLD_MEASURE_ITERS:-10}"
BUDGET="${REALWORLD_TIME_BUDGET_SEC:-60}"

# Filter flags.
FILTER_BENCH=""
FILTER_ENGINE=""
FILTER_TIER=""
OUTPUT="$DEFAULT_OUTPUT"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bench) FILTER_BENCH="$2"; shift 2 ;;
    --engine) FILTER_ENGINE="$2"; shift 2 ;;
    --tier) FILTER_TIER="$2"; shift 2 ;;
    --output) OUTPUT="$2"; shift 2 ;;
    --warmup) WARMUP="$2"; shift 2 ;;
    --measure) MEASURE="$2"; shift 2 ;;
    --time-budget) BUDGET="$2"; shift 2 ;;
    -h|--help)
      sed -n '3,20p' "$0"
      exit 0
      ;;
    *) echo "unknown flag: $1" >&2; exit 1 ;;
  esac
done

mkdir -p "$RESULTS_DIR"
: > "$OUTPUT"  # truncate

# Build crabscheme once.
echo "==> building crabscheme (release + aot)..." >&2
(cd "$ROOT" && cargo build --release -p cs-cli --features aot --bin crabscheme \
   2>&1 | tail -3) >&2
CS_BIN="$ROOT/target/release/crabscheme"
if [ ! -x "$CS_BIN" ]; then
  echo "missing $CS_BIN — cs-cli build failed?" >&2
  exit 2
fi

# Engine/tier matrix. For Phase C only CrabScheme tiers; cross-impl
# (Chez, Gambit, Racket, Guile) lands in Phase D when Tier-2 benches
# arrive.
declare -a ENGINES=(
  "crabscheme:walker"   # tree-walker tier
  "crabscheme:vm"       # bytecode VM tier
)
# AOT tier is more involved (each bench needs --multi build + run);
# wire when Tier-2 benches need AOT comparison.

# Bench discovery — every .scm in schemes/ is a bench.
declare -a BENCHES
while IFS= read -r -d '' f; do
  BENCHES+=("$(basename "$f" .scm)")
done < <(find "$SCHEMES" -maxdepth 1 -name '*.scm' -print0 | sort -z)

if [ ${#BENCHES[@]} -eq 0 ]; then
  echo "no benches in $SCHEMES" >&2
  exit 3
fi

# Resolve crabscheme version once (passed to harness via env so it
# lands in the JSON document's engine_version field).
CS_VERSION=$("$CS_BIN" --version 2>/dev/null | awk '{print $NF}' || echo "dev")

# Run a single (engine, tier, bench) combination. Concatenates the
# harness + bench file, invokes the engine, appends stdout to the
# JSONL output.
run_one() {
  local engine="$1"
  local tier="$2"
  local bench="$3"
  local bench_file="$SCHEMES/$bench.scm"
  if [ ! -f "$bench_file" ]; then
    echo "missing bench: $bench_file" >&2
    return 1
  fi
  # Concatenate harness + bench into a temp file. The harness's
  # `(realworld-bench ...)` invocation at the bottom of the bench
  # file triggers the timing loop + JSON emit.
  local tmpfile
  tmpfile=$(mktemp -t "rw-$bench-$tier-XXXXXX.scm")
  cat "$HARNESS" "$bench_file" > "$tmpfile"
  # Set env vars so the harness picks up the right config.
  REALWORLD_ENGINE="$engine" \
  REALWORLD_ENGINE_TIER="$tier" \
  REALWORLD_ENGINE_VERSION="$CS_VERSION" \
  REALWORLD_WARMUP_ITERS="$WARMUP" \
  REALWORLD_MEASURE_ITERS="$MEASURE" \
  REALWORLD_TIME_BUDGET_SEC="$BUDGET" \
    "$CS_BIN" --tier "$tier" run "$tmpfile" \
      2> "$tmpfile.stderr" \
      || true  # never let set -e kill the loop on a failing bench
  local rc=${PIPESTATUS[0]:-$?}
  if [ $rc -ne 0 ]; then
    # Emit a failure record so render.py can show "fail" cells
    # instead of dropping rows silently.
    cat <<EOF
{"schema_version":"1.0","engine":"$engine","engine_tier":"$tier","benchmark":"$bench","status":"error","exit_code":$rc,"stderr_tail":$(tail -c 256 "$tmpfile.stderr" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}
EOF
  fi
  rm -f "$tmpfile" "$tmpfile.stderr"
}

# Main loop.
for engine_spec in "${ENGINES[@]}"; do
  engine="${engine_spec%%:*}"
  tier="${engine_spec##*:}"

  [ -n "$FILTER_ENGINE" ] && [ "$engine" != "$FILTER_ENGINE" ] && continue
  [ -n "$FILTER_TIER" ] && [ "$tier" != "$FILTER_TIER" ] && continue

  for bench in "${BENCHES[@]}"; do
    [ -n "$FILTER_BENCH" ] && [ "$bench" != "$FILTER_BENCH" ] && continue
    echo "==> [$engine/$tier] $bench" >&2
    run_one "$engine" "$tier" "$bench" >> "$OUTPUT"
  done
done

echo "==> JSONL: $OUTPUT" >&2
echo "==> rows: $(wc -l < "$OUTPUT")" >&2
echo "==> render with: python3 $REALWORLD/render.py $OUTPUT" >&2
