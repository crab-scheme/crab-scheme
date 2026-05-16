#!/usr/bin/env bash
# WASM conformance runner. Builds the wasm32-wasip1 CrabScheme binary
# once, then runs every fixture under `tests/conformance/foundation/`
# via `wasmtime`, parses the `(__test-summary__)` result, and prints
# a per-fixture + aggregate pass/fail/error tally.
#
# Why a script instead of a `cargo test`-style harness: WASM runs
# under wasmtime, not the Rust test runner. The native conformance
# path is in `crates/cs-cli/tests/conformance.rs` and uses the same
# fixtures via `cs_runtime::Runtime`; this script is the WASM analog,
# kept as a separate runner because:
#   - WASI requires explicit `--dir` + `--env` flag plumbing per
#     fixture (the file-I/O fixtures wouldn't run without it).
#   - The WASM binary build is a separate cargo target.
#   - Running this from `cargo test` would couple Rust test
#     scheduling to wasmtime's runtime — extra moving parts for no
#     gain.
#
# Track W exit report's 2,438 / 1 / 2 number (W4 sweep, 2026-05-16)
# was produced ad-hoc; this script reproduces it. With the
# file-I/O fixtures' --dir mappings landed here, the "2 errored"
# bucket drops to 0, raising the run to 117/117 fixtures executed.

set -e
cd "$(dirname "$0")"
ROOT=$(cd .. && pwd)

WASM_BIN="$ROOT/target/wasm32-wasip1/release/crabscheme.wasm"
CORPUS="$ROOT/tests/conformance/foundation"
PRELUDE="$CORPUS/_prelude.scm"

# --- build the WASM binary if missing ---------------------------------
if [ ! -f "$WASM_BIN" ]; then
  echo "==> building crabscheme.wasm (wasm32-wasip1, --no-default-features)..."
  (cd "$ROOT" && cargo build --target wasm32-wasip1 --release \
                     -p cs-cli --no-default-features >/dev/null)
fi
if [ ! -f "$WASM_BIN" ]; then
  echo "expected $WASM_BIN; build did not produce it"
  exit 1
fi
if ! command -v wasmtime >/dev/null; then
  echo "wasmtime not on PATH — run via 'devenv shell -- bash $0' or install wasmtime"
  exit 1
fi

# --- WASI plumbing per fixture ---------------------------------------
#
# Most fixtures need nothing beyond `--dir=.` (so the runtime can
# read its own embedded files via WASI). The file-I/O fixtures need
# extra mappings.
#
# We use a single tmp directory ($WASI_TMPDIR) for the fixtures that
# want a writable /tmp. Wasmtime maps it both as the guest's
# "writable scratch" and as $TMPDIR via --env, mirroring what the
# native runtime sees from the host env.
WASI_TMPDIR=$(mktemp -d -t crabscheme-wasm-conformance.XXXXXX)
trap 'rm -rf "$WASI_TMPDIR"' EXIT

# Map a fixture name → extra wasmtime flags. Default: just `--dir=.`.
#
# - `r7rs_load.scm` calls `(open-output-file (string-append TMPDIR
#   "/cs_load_test.scm"))` and reads it back. Needs writable scratch
#   + the TMPDIR env var.
# - `sorting_files.scm` reads `/etc/hosts` as a known-present
#   readable file (to test `file-exists?` + `open-input-file` on a
#   real path). Needs `/etc` mapped read-only.
fixture_flags() {
  local name="$1"
  case "$name" in
    r7rs_load.scm)
      echo "--dir=. --dir=$WASI_TMPDIR --env=TMPDIR=$WASI_TMPDIR"
      ;;
    sorting_files.scm)
      echo "--dir=. --dir=/etc::/etc --dir=$WASI_TMPDIR --env=TMPDIR=$WASI_TMPDIR"
      ;;
    *)
      echo "--dir=."
      ;;
  esac
}

# --- run each fixture and parse the summary --------------------------
TMPSCM=$(mktemp -t crabscheme-wasm-fixture.XXXXXX.scm)
trap 'rm -f "$TMPSCM"; rm -rf "$WASI_TMPDIR"' EXIT

total_pass=0
total_fail=0
total_err=0
declare -a failed_files
declare -a errored_files

cd "$ROOT"

# Output: per-fixture line `<file> pass=<P> fail=<F>` or `<file> ERROR`.
# Aggregate appended at the bottom.

for path in "$CORPUS"/*.scm; do
  name=$(basename "$path")
  [ "$name" = "_prelude.scm" ] && continue

  # Concatenate prelude + body + summary call. The fixture's
  # top-level forms are evaluated; the trailing `(__test-summary__)`
  # is the program's final value, which `wasmtime ... -e` prints.
  {
    cat "$PRELUDE"
    cat "$path"
    echo
    echo '(__test-summary__)'
  } > "$TMPSCM"

  flags=$(fixture_flags "$name")
  # shellcheck disable=SC2086
  out=$(wasmtime run $flags "$WASM_BIN" -e "$(cat "$TMPSCM")" 2>&1) || {
    total_err=$((total_err + 1))
    errored_files+=("$name")
    printf "  %-40s ERROR (exit %s)\n" "$name" "$?"
    continue
  }

  # The summary tuple is the program's final value. wasmtime prints
  # any other side-effect output above it (display calls in fixtures),
  # then the result on the last non-empty line.
  summary=$(echo "$out" | grep -E '^\([0-9]+ [0-9]+ ' | tail -1)
  if [ -z "$summary" ]; then
    total_err=$((total_err + 1))
    errored_files+=("$name")
    printf "  %-40s ERROR (no summary parsed)\n" "$name"
    continue
  fi
  # Parse `(P F (failures...))` → just P and F.
  pass=$(echo "$summary" | sed -E 's/^\(([0-9]+) [0-9]+ .*/\1/')
  fail=$(echo "$summary" | sed -E 's/^\([0-9]+ ([0-9]+) .*/\1/')

  total_pass=$((total_pass + pass))
  total_fail=$((total_fail + fail))
  if [ "$fail" -gt 0 ]; then
    failed_files+=("$name (fail=$fail)")
  fi
  printf "  %-40s pass=%-5s fail=%s\n" "$name" "$pass" "$fail"
done

echo
echo "===================================================="
echo "WASM conformance summary"
echo "  total pass    : $total_pass"
echo "  total fail    : $total_fail"
echo "  errored files : $total_err"
echo "===================================================="
if [ ${#failed_files[@]} -gt 0 ]; then
  echo "Failed fixtures:"
  printf "  - %s\n" "${failed_files[@]}"
fi
if [ ${#errored_files[@]} -gt 0 ]; then
  echo "Errored fixtures:"
  printf "  - %s\n" "${errored_files[@]}"
fi

# Exit non-zero on any fail OR error so CI / `devenv test` can gate.
if [ "$total_fail" -gt 0 ] || [ "$total_err" -gt 0 ]; then
  exit 1
fi
