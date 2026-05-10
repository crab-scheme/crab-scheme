{ pkgs, lib, ... }:

{
  # ---- project toolchain ----------------------------------------------------
  # Rust stable toolchain. cargo + rustc + rustfmt come automatically.
  languages.rust = {
    enable = true;
    channel = "stable";
  };

  # ---- packages ------------------------------------------------------------
  # Native tools the project + tests need. The Scheme implementations down
  # below are for the bench/microbench cross-implementation comparison;
  # bench/microbench/run.sh auto-detects them on PATH.
  packages = with pkgs; [
    # core build deps
    pkg-config

    # Useful while iterating on the codebase.
    just

    # Other Scheme implementations for microbench cross-comparison.
    # Each enables a row in `bench/microbench/run.sh`'s table.
    #
    # racket is intentionally NOT here — racket-minimal fails to build
    # from source on aarch64-darwin in the rolling nixpkgs we pin.
    # Install it via the official Racket installer if you want a
    # racket column in the bench table.
    chez # Chez Scheme — fast batch interpreter
    guile_3_0 # GNU Guile (R7RS-ish via #!r7rs)
    gambit # gsi for Gambit Scheme
  ];

  # ---- scripts ------------------------------------------------------------
  # `devenv shell` exposes these as plain commands.
  scripts.bench-micro = {
    description = "Run the cross-implementation microbenchmark suite.";
    exec = ''
      cd "$(git rev-parse --show-toplevel)"
      exec bash bench/microbench/run.sh "$@"
    '';
  };

  scripts.test-all = {
    description = "Run the full test suite (workspace tests).";
    exec = ''
      cd "$(git rev-parse --show-toplevel)"
      exec cargo test --workspace "$@"
    '';
  };

  scripts.test-conformance = {
    description = "Run the conformance suite on both walker and VM tiers.";
    exec = ''
      cd "$(git rev-parse --show-toplevel)"
      cargo test -p cs-cli --test conformance "$@"
      cargo test -p cs-runtime --test vm_conformance "$@"
    '';
  };

  # ---- enterShell ----------------------------------------------------------
  # Banner so users see which Schemes are installed and what to do next.
  enterShell = ''
    echo "crabscheme dev shell — comparison Schemes available:"
    if command -v racket >/dev/null 2>&1; then
      echo "  - racket ($(command -v racket))"
    fi
    if command -v scheme >/dev/null 2>&1 && scheme --help 2>&1 | grep -q -- '--script'; then
      echo "  - chez   ($(command -v scheme))"
    fi
    if command -v guile >/dev/null 2>&1; then
      echo "  - guile  ($(command -v guile))"
    fi
    if command -v gsi >/dev/null 2>&1; then
      echo "  - gambit ($(command -v gsi))"
    fi
    echo ""
    echo "Try:"
    echo "  bench-micro            # cross-implementation timing table"
    echo "  test-all               # full workspace test suite"
    echo "  test-conformance       # walker + VM conformance"
  '';
}
