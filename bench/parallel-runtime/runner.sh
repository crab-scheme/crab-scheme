#!/usr/bin/env bash
#
# parallel-runtime bench harness — C6.1 skeleton.
#
# Runs each Scheme file in `schemes/` against `crabscheme run`
# with `--features actor regions tracing-cycle-collector`,
# captures stdout + exit code, and reports pass/fail per
# bench's own success criteria (each script prints a final
# `OK <metric>` line on success or `FAIL <reason>` on
# failure).
#
# Usage:
#   bench/parallel-runtime/runner.sh                    # all benches
#   bench/parallel-runtime/runner.sh --bench echo-10m   # one bench
#   bench/parallel-runtime/runner.sh --time-budget 600  # longer per-bench budget
#
# These benches gate the parallel-runtime spec's M1/M2/M3
# milestones — see `.spec-workflow/specs/parallel-runtime/
# requirements.md` for the headline numbers each is checking
# (1M spawn/sec, 10M echo/sec, sub-100ms responder under
# 100k-candidate sweep, etc.).

set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BENCH_DIR="$ROOT/bench/parallel-runtime"
SCHEMES="$BENCH_DIR/schemes"
RESULTS_DIR="$BENCH_DIR/results"

# Longer budgets than the realworld microbenches — these
# scenarios are inherently longer (1M actor spawn, 10M
# messages, 100k cycles to sweep).
BUDGET="${PR_TIME_BUDGET_SEC:-300}"
FILTER_BENCH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bench) FILTER_BENCH="$2"; shift 2 ;;
    --time-budget) BUDGET="$2"; shift 2 ;;
    -h|--help) sed -n '3,20p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 1 ;;
  esac
done

mkdir -p "$RESULTS_DIR"

# Build crabscheme with the parallel-runtime feature set.
# `actor` is already in cs-cli's default; `regions` is
# always-on at the cs-runtime layer (ADR 0013); only
# `tracing-cycle-collector` needs an opt-in.
CS_FEATURES="tracing-cycle-collector"
echo "==> building crabscheme (release, features '$CS_FEATURES')..." >&2
(cd "$ROOT" && cargo build --release -p cs-cli --features "$CS_FEATURES" --bin crabscheme \
   2>&1 | tail -3) >&2
CS_BIN="$ROOT/target/release/crabscheme"
if [ ! -x "$CS_BIN" ]; then
  echo "missing $CS_BIN — cs-cli build failed?" >&2
  exit 2
fi

# Bench discovery.
declare -a BENCHES
while IFS= read -r -d '' f; do
  BENCHES+=("$(basename "$f" .scm)")
done < <(find "$SCHEMES" -maxdepth 1 -name '*.scm' -print0 | sort -z)

if [ ${#BENCHES[@]} -eq 0 ]; then
  echo "no benches in $SCHEMES" >&2
  exit 3
fi

# Run one bench. Returns 0 if stdout's last line starts with
# "OK"; non-zero otherwise (the bench's own failure path
# emits "FAIL ..." and exits non-zero, or just times out).
run_one() {
  local bench="$1"
  local bench_file="$SCHEMES/$bench.scm"
  if [ ! -f "$bench_file" ]; then
    echo "missing bench: $bench_file" >&2
    return 1
  fi
  local log_file="$RESULTS_DIR/$bench.log"
  local rc=0
  PR_BUDGET="$BUDGET" \
    timeout "$BUDGET" "$CS_BIN" run "$bench_file" > "$log_file" 2>&1 \
    || rc=$?
  local last_line
  last_line=$(tail -n 1 "$log_file" 2>/dev/null || echo "")
  if [ $rc -eq 0 ] && [[ "$last_line" == OK* ]]; then
    echo "  PASS $bench — $last_line"
    return 0
  else
    echo "  FAIL $bench (rc=$rc) — last: $last_line"
    return 1
  fi
}

pass=0
fail=0
for bench in "${BENCHES[@]}"; do
  [ -n "$FILTER_BENCH" ] && [ "$bench" != "$FILTER_BENCH" ] && continue
  echo "==> $bench" >&2
  if run_one "$bench"; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
  fi
done

echo "==> $pass passed, $fail failed (logs in $RESULTS_DIR/)" >&2
[ $fail -eq 0 ]
