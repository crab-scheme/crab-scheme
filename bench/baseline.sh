#!/usr/bin/env bash
# Perf baseline harness (cs-vm3). Builds crabscheme release, runs the 8
# canonical microbenchmarks across the walker/vm/vm-jit tiers, and
# records wall time (hyperfine), max RSS, and gc-stats alloc counters.
#
# Usage:
#   bench/baseline.sh                    # record a new baseline JSON
#   bench/baseline.sh --diff <file.json> # rerun and diff vs a saved baseline
set -euo pipefail
CALLER_PWD="$PWD"
cd "$(dirname "$0")"
ROOT=$(cd .. && pwd)
SCM_DIR="$ROOT/bench/microbench/scheme"
BASELINE_DIR="$ROOT/bench/baselines"
CS="$ROOT/target/release/crabscheme"

BENCHES=(fib tak ack nqueens mandelbrot spectral-norm binary-trees alloc-stress)
TIERS=(walker vm vm-jit)

DIFF_AGAINST=""
if [ "${1:-}" = "--diff" ]; then
  DIFF_AGAINST="${2:?--diff requires a baseline JSON path}"
  case "$DIFF_AGAINST" in
    /*) ;;
    *) DIFF_AGAINST="$CALLER_PWD/$DIFF_AGAINST" ;;
  esac
fi

HAVE_HYPERFINE=1
command -v hyperfine >/dev/null 2>&1 || HAVE_HYPERFINE=0

echo "==> building crabscheme (release)..." >&2
(cd "$ROOT" && cargo build --release -p cs-cli >/dev/null 2>&1)
if [ ! -x "$CS" ]; then
  echo "expected $CS; got nothing — check 'cargo build --release -p cs-cli'" >&2
  exit 1
fi

TMP_TSV=$(mktemp)
trap 'rm -f "$TMP_TSV" "$TMP_TSV.scm"' EXIT

time_cell() {
  # Prints "median_s stddev_s" or "error error" to stdout.
  local tier="$1" file="$2"
  if [ "$HAVE_HYPERFINE" = "1" ]; then
    local hf_json
    hf_json=$(mktemp)
    if hyperfine --warmup 3 --export-json "$hf_json" \
      "$CS --tier $tier run $file" >/dev/null 2>&1; then
      python3 -c "
import json
r = json.load(open('$hf_json'))['results'][0]
print(r['median'], r['stddev'])
"
    else
      echo "error error"
    fi
    rm -f "$hf_json"
  else
    # Fallback: 5-trial median via perl wall-clock timing (no hyperfine
    # on PATH — matches bench/microbench/run.sh's timing convention).
    local times=() t start end
    for _ in 1 2 3 4 5; do
      start=$(perl -MTime::HiRes=time -e 'printf("%.6f\n", time())')
      if ! "$CS" --tier "$tier" run "$file" >/dev/null 2>&1; then
        echo "error error"
        return
      fi
      end=$(perl -MTime::HiRes=time -e 'printf("%.6f\n", time())')
      times+=("$(perl -e "printf('%.6f', $end - $start)")")
    done
    python3 -c "
import statistics
xs = [${times[*]/%/,}]
print(statistics.median(xs), statistics.pstdev(xs))
"
  fi
}

mem_cell() {
  # Prints "max_rss_bytes bytes_allocated_total alloc_count_total
  # live_slots" or "error error error error" to stdout. Runs the
  # benchmark once more (untimed for wall-clock purposes) with a
  # trailing (gc-stats) call spliced in, under `/usr/bin/time -l`
  # (macOS: "maximum resident set size" is already in bytes).
  local tier="$1" file="$2"
  local wrapped="$TMP_TSV.scm"
  cat "$file" >"$wrapped"
  printf '\n(display (gc-stats))(newline)\n' >>"$wrapped"
  local time_out
  time_out=$(mktemp)
  if ! /usr/bin/time -l "$CS" --tier "$tier" run "$wrapped" >"$TMP_TSV.out" 2>"$time_out"; then
    echo "error error error error"
    rm -f "$time_out"
    return
  fi
  local rss
  rss=$(awk '/maximum resident set size/ {print $1}' "$time_out")
  rm -f "$time_out"
  python3 -c "
import re
out = open('$TMP_TSV.out').read()
m = re.search(r'\(bytes-allocated-total\s*\.\s*(-?[0-9]+)\)', out)
a = re.search(r'\(alloc-count-total\s*\.\s*(-?[0-9]+)\)', out)
l = re.search(r'\(live-slots\s*\.\s*(-?[0-9]+)\)', out)
bytes_ = m.group(1) if m else 'null'
allocs = a.group(1) if a else 'null'
live = l.group(1) if l else 'null'
print('${rss:-null}', bytes_, allocs, live)
"
}

echo "==> running benchmarks (this reruns everything; several minutes)..." >&2
for b in "${BENCHES[@]}"; do
  scm_file="$SCM_DIR/$b.scm"
  [ -f "$scm_file" ] || continue
  for tier in "${TIERS[@]}"; do
    echo "   $b / $tier" >&2
    read -r median stddev <<<"$(time_cell "$tier" "$scm_file")"
    read -r rss bytes allocs live <<<"$(mem_cell "$tier" "$scm_file")"
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$b" "$tier" "$median" "$stddev" "$rss" "$bytes" "$allocs" "$live" >>"$TMP_TSV"
  done
done

SHA=$(git -C "$ROOT" rev-parse --short HEAD)
DATE=$(date +%Y-%m-%d)
OUT_JSON="$BASELINE_DIR/${DATE}-${SHA}.json"
mkdir -p "$BASELINE_DIR"

# --- emit JSON + human table, and (if --diff) compare vs a prior baseline ---
python3 - "$TMP_TSV" "$OUT_JSON" "$SHA" "$DATE" "$DIFF_AGAINST" "$HAVE_HYPERFINE" <<'PYEOF'
import sys, json

tsv_path, out_json, sha, date, diff_against, have_hyperfine = sys.argv[1:7]
have_hyperfine = have_hyperfine == "1"

def to_num(s):
    if s in ("error", "null", ""):
        return None
    try:
        return int(s)
    except ValueError:
        return float(s)

rows = []
with open(tsv_path) as f:
    for line in f:
        b, tier, median, stddev, rss, bytes_, allocs, live = line.rstrip("\n").split("\t")
        rows.append({
            "benchmark": b,
            "tier": tier,
            "median_s": to_num(median),
            "stddev_s": to_num(stddev),
            "max_rss_bytes": to_num(rss),
            "bytes_allocated_total": to_num(bytes_),
            "alloc_count_total": to_num(allocs),
            "live_slots": to_num(live),
        })

doc = {
    "sha": sha,
    "date": date,
    "timing_source": "hyperfine" if have_hyperfine else "perl-5-trial-median-fallback",
    "cells": rows,
}
with open(out_json, "w") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")

def fmt(v, unit=""):
    if v is None:
        return "error"
    if unit == "s":
        return f"{v:.4f}s"
    if unit == "B":
        if v >= 1_000_000:
            return f"{v/1_000_000:.1f}MB"
        if v >= 1_000:
            return f"{v/1_000:.1f}KB"
        return f"{v}B"
    return str(v)

print()
print(f"baseline: {out_json}")
print(f"sha={sha} date={date} timing_source={doc['timing_source']}")
print()
header = f"{'benchmark':<16}{'tier':<10}{'median':>10}{'stddev':>10}{'max_rss':>10}{'bytes_alloc':>14}{'allocs':>12}"
print(header)
print("-" * len(header))
for r in rows:
    print(f"{r['benchmark']:<16}{r['tier']:<10}"
          f"{fmt(r['median_s'],'s'):>10}{fmt(r['stddev_s'],'s'):>10}"
          f"{fmt(r['max_rss_bytes'],'B'):>10}{fmt(r['bytes_allocated_total'],'B'):>14}"
          f"{fmt(r['alloc_count_total']):>12}")

if diff_against:
    with open(diff_against) as f:
        base = json.load(f)
    base_by_key = {(c["benchmark"], c["tier"]): c for c in base["cells"]}
    print()
    print(f"diff vs {diff_against} (sha={base.get('sha','?')} date={base.get('date','?')})")
    print()
    dheader = f"{'benchmark':<16}{'tier':<10}{'median_now':>12}{'median_base':>12}{'delta%':>10}{'rss_delta%':>12}"
    print(dheader)
    print("-" * len(dheader))
    for r in rows:
        key = (r["benchmark"], r["tier"])
        b = base_by_key.get(key)
        if b is None or r["median_s"] is None or b.get("median_s") is None:
            print(f"{r['benchmark']:<16}{r['tier']:<10}{'n/a':>12}{'n/a':>12}{'n/a':>10}{'n/a':>12}")
            continue
        delta = (r["median_s"] - b["median_s"]) / b["median_s"] * 100
        rss_delta = "n/a"
        if r["max_rss_bytes"] is not None and b.get("max_rss_bytes"):
            rss_delta = f"{(r['max_rss_bytes'] - b['max_rss_bytes']) / b['max_rss_bytes'] * 100:+.1f}%"
        print(f"{r['benchmark']:<16}{r['tier']:<10}{fmt(r['median_s'],'s'):>12}"
              f"{fmt(b['median_s'],'s'):>12}{delta:>9.1f}%{rss_delta:>12}")
PYEOF

echo
echo "wrote $OUT_JSON"
