#!/usr/bin/env bash
# Proper-tail-call regression guard (ADR 0019).
#
# Runs bench/microbench/scheme/tco.scm on all three execution tiers and
# checks each produces the same result in constant stack. Pre-fix the
# JIT (`vm-jit`) overflows the host stack and aborts; this script fails
# loudly when that regresses.
set -u
cd "$(dirname "$0")"
ROOT=$(cd ../.. && pwd)
CS="$ROOT/target/release/crabscheme"
SCM="$ROOT/bench/microbench/scheme/tco.scm"
EXPECT="tco = 9000000"

if [ ! -x "$CS" ]; then
  echo "==> building crabscheme (release)..."
  (cd "$ROOT" && cargo build --release -p cs-cli >/dev/null 2>&1) || {
    echo "FAIL: build failed"; exit 1; }
fi

rc=0
for tier in walker vm vm-jit; do
  out=$("$CS" --tier "$tier" run "$SCM" 2>&1)
  status=$?
  if [ $status -ne 0 ]; then
    echo "FAIL [$tier]: exited $status (stack overflow / abort?)"
    echo "       output: $out"
    rc=1
  elif [ "$out" != "$EXPECT" ]; then
    echo "FAIL [$tier]: expected '$EXPECT', got '$out'"
    rc=1
  else
    echo "ok   [$tier]: $out"
  fi
done
exit $rc
