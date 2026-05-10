#!/usr/bin/env bash
# Microbenchmark runner. Builds CrabScheme (release) once, builds each
# Rust reference impl with -O once, then runs every benchmark on each
# implementation and prints a comparison table.
#
# Adapt SCHEMES (a "name:command" list) to add Racket / Chez / Guile /
# etc. for cross-implementation comparison.

set -e
cd "$(dirname "$0")"

ROOT=$(cd ../.. && pwd)
RUST_DIR="$ROOT/bench/microbench/rust"
SCM_DIR="$ROOT/bench/microbench/scheme"
BINS="$ROOT/target/release"

# --- build crabscheme in release once ---
echo "==> building crabscheme (release)..."
(cd "$ROOT" && cargo build --release -p cs-cli >/dev/null 2>&1)
CS="$BINS/crabscheme"
if [ ! -x "$CS" ]; then
  echo "expected $CS; got nothing — check 'cargo build --release -p cs-cli'"
  exit 1
fi

# --- build rust reference impls into target/release-microbench/ ---
RUST_OUT="$ROOT/target/release-microbench"
mkdir -p "$RUST_OUT"
for src in "$RUST_DIR"/*.rs; do
  name=$(basename "$src" .rs)
  out="$RUST_OUT/$name"
  if [ ! -x "$out" ] || [ "$src" -nt "$out" ]; then
    echo "==> building rust/$name..."
    rustc -O "$src" -o "$out"
  fi
done

# --- run table ---
BENCHES=(fib tak ack nqueens mandelbrot spectral-norm binary-trees alloc-stress)

# Optional implementations to compare against. Set RACKET=racket, CHEZ=chez, etc.
declare -a IMPLS
IMPLS=("crabscheme-walker:$CS --tier walker run")
IMPLS+=("crabscheme-vm:$CS --tier vm run")
if command -v racket >/dev/null 2>&1; then IMPLS+=("racket:racket"); fi
# Chez Scheme ships its REPL binary as `scheme` (or sometimes `chez`).
# Detection: prefer `chez` if present; otherwise check that `scheme`
# documents a `--script` flag (a Chez idiom).
if command -v chez >/dev/null 2>&1; then
  IMPLS+=("chez:chez --script")
elif command -v scheme >/dev/null 2>&1 && scheme --help 2>&1 | grep -q -- '--script'; then
  IMPLS+=("chez:scheme --script")
fi
if command -v guile >/dev/null 2>&1; then IMPLS+=("guile:guile -q"); fi
if command -v gsi >/dev/null 2>&1; then IMPLS+=("gambit:gsi"); fi

# header
printf "%-22s" "benchmark"
for spec in "${IMPLS[@]}"; do
  name=${spec%%:*}
  printf " %14s" "$name"
done
printf " %14s\n" "rust-O"

# Use macOS or GNU /usr/bin/time -p (POSIX) for "real" wall time.
TIMER="/usr/bin/time -p"

run_and_time() {
  # Run the command, capture both wall time and stdout. Returns "TIME(s) OUTPUT"
  local cmd="$1"
  local start_ns=$(perl -MTime::HiRes=time -e 'printf("%.6f\n", time())')
  out=$(eval "$cmd" 2>/dev/null) || return 1
  local end_ns=$(perl -MTime::HiRes=time -e 'printf("%.6f\n", time())')
  local dur=$(perl -e "printf('%.3f', $end_ns - $start_ns)")
  echo "$dur"
}

for b in "${BENCHES[@]}"; do
  printf "%-22s" "$b"
  scm_file="$SCM_DIR/$b.scm"
  for spec in "${IMPLS[@]}"; do
    name=${spec%%:*}
    cmd_prefix=${spec#*:}
    if [ -f "$scm_file" ]; then
      t=$(run_and_time "$cmd_prefix '$scm_file'") || t="ERR"
      printf " %14s" "${t}s"
    else
      printf " %14s" "-"
    fi
  done
  rust_bin="$RUST_OUT/$b"
  if [ -x "$rust_bin" ]; then
    t=$(run_and_time "$rust_bin") || t="ERR"
    printf " %14s\n" "${t}s"
  else
    printf " %14s\n" "-"
  fi
done
