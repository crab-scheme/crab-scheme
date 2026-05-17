#!/usr/bin/env bash
#
# Cross-impl correctness spot-check. For each ported Tier-2 bench,
# runs the workload on (1) Chez and (2) CrabScheme VM, compares the
# stringified result. Doesn't measure timing — that's runner.sh's
# job; this is purely a "do we compute the same answer?" gate.
#
# Each bench file embeds its result computation in `(realworld-bench)`'s
# thunk. To make the result observable, the helper file
# `chez-shim.scm` provides a (realworld-bench) that runs the thunk
# once and prints the result so we can hash it.
#
# Usage:
#     bench/realworld/check-result-vs-chez.sh [--bench fib]

set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
REALWORLD="$ROOT/bench/realworld"
SCHEMES="$REALWORLD/schemes"
CS_BIN="$ROOT/target/release/crabscheme"
SHIM="$REALWORLD/chez-shim.scm"

# Only Chez is reliably available (devenv provides it as `scheme`).
# Gambit + Racket would slot here too when present.
CHEZ_BIN="${CHEZ_BIN:-$(command -v scheme || true)}"
if [ -z "$CHEZ_BIN" ]; then
  echo "Chez not found (looked for 'scheme' on PATH). Install via devenv shell." >&2
  exit 1
fi

FILTER_BENCH=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bench) FILTER_BENCH="$2"; shift 2 ;;
    *) echo "unknown flag: $1" >&2; exit 1 ;;
  esac
done

# Build crabscheme if needed.
if [ ! -x "$CS_BIN" ]; then
  echo "==> building crabscheme..." >&2
  (cd "$ROOT" && cargo build --release -p cs-cli --bin crabscheme 2>&1 | tail -1) >&2
fi

# Both engines use a "result-only" shim that runs the thunk once
# and writes the result via (write) + newline. The full harness in
# lib/harness.scm is for timing — irrelevant for this gate.
CRAB_SHIM="$REALWORLD/crab-shim.scm"
CHEZ_SHIM="$REALWORLD/chez-shim.scm"

printf "%-20s %-12s %-12s %s\n" "bench" "crab-sha" "chez-sha" "status"
echo "----------------------------------------------------------------"

declare -a BENCHES
while IFS= read -r -d '' f; do
  BENCHES+=("$(basename "$f" .scm)")
done < <(find "$SCHEMES" -maxdepth 1 -name '*.scm' -print0 | sort -z)

for bench in "${BENCHES[@]}"; do
  [ -n "$FILTER_BENCH" ] && [ "$bench" != "$FILTER_BENCH" ] && continue
  bench_file="$SCHEMES/$bench.scm"

  # Crab run via crab-shim.
  crab_tmp=$(mktemp -t "crab-$bench-XXXXXX.scm")
  cat "$CRAB_SHIM" "$bench_file" > "$crab_tmp"
  crab_out=$("$CS_BIN" --tier vm run "$crab_tmp" 2>/dev/null)
  rm -f "$crab_tmp"
  if [ -z "$crab_out" ]; then
    printf "%-20s %-12s %-12s %s\n" "$bench" "ERR" "-" "crab failed"
    continue
  fi
  crab_sha=$(echo "$crab_out" | shasum | awk '{print substr($1,1,10)}')

  # Chez run via chez-shim.
  chez_tmp=$(mktemp -t "chez-$bench-XXXXXX.scm")
  cat "$CHEZ_SHIM" "$bench_file" > "$chez_tmp"
  chez_out=$("$CHEZ_BIN" --script "$chez_tmp" 2>/dev/null || true)
  rm -f "$chez_tmp"
  if [ -z "$chez_out" ]; then
    printf "%-20s %-12s %-12s %s\n" "$bench" "$crab_sha" "ERR" "chez failed"
    continue
  fi
  chez_sha=$(echo "$chez_out" | shasum | awk '{print substr($1,1,10)}')

  if [ "$chez_out" = "$crab_out" ]; then
    printf "%-20s %-12s %-12s %s\n" "$bench" "$crab_sha" "$chez_sha" "MATCH"
  else
    printf "%-20s %-12s %-12s %s\n" "$bench" "$crab_sha" "$chez_sha" "DIFFER"
  fi
done
