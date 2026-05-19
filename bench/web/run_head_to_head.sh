#!/usr/bin/env bash
# Head-to-head bench: same load (10 s × 256 keep-alive conns,
# no pipelining, loopback) against each framework's
# "Hello, World!" /plain handler.
#
# All servers must be pre-built. Run:
#
#   cargo build --release -p cs-web --example tfb_server --example tfb_client
#   cargo build --release --manifest-path bench/web/competitors/axum/Cargo.toml
#   go build -C bench/web/competitors/go-hello -o "$PWD/bench/web/competitors/go-hello-bin" .
#
# Then `./bench/web/run_head_to_head.sh`.

set -euo pipefail

DURATION="${DURATION:-10}"
CONNS="${CONNS:-256}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CLIENT="$ROOT/target/release/examples/tfb_client"

# Server config: name, command-line, port to use.
run_one() {
  local name="$1"; shift
  local port="$1"; shift
  local addr_arg_style="$1"; shift  # "hostport" or "port"
  local cmd=("$@")
  echo "--- $name ---"
  # Start the server in the background, log to a temp file.
  local log="/tmp/bench-$name.log"
  local server_arg
  if [[ "$addr_arg_style" == "hostport" ]]; then
    server_arg="127.0.0.1:$port"
  else
    server_arg="$port"
  fi
  "${cmd[@]}" "$server_arg" > "$log" 2>&1 &
  local pid=$!
  sleep 0.5
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "  (server failed to start; log: $log)"
    cat "$log" | head -5
    return
  fi
  # The actual bound port — most servers print it. We pass a
  # fixed port above so the address is predictable.
  local addr="127.0.0.1:$port"
  # Two warm-up requests.
  curl -sS "http://$addr/plain" > /dev/null || true
  curl -sS "http://$addr/plain" > /dev/null || true
  # Run the client.
  "$CLIENT" "$addr" /plain "$DURATION" "$CONNS"
  # Tear down.
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  rm -f "$log"
}

echo "head-to-head: $DURATION s × $CONNS connections, loopback, no pipelining"
echo

run_one cs-web 18001 hostport "$ROOT/target/release/examples/tfb_server"
run_one axum   18002 hostport "$ROOT/bench/web/competitors/axum/target/release/axum-hello"
run_one go     18003 hostport "$ROOT/bench/web/competitors/go-hello-bin"
run_one node   18004 port     node "$ROOT/bench/web/competitors/node-hello.js"
run_one python 18005 port     python3 "$ROOT/bench/web/competitors/python-hello.py"
