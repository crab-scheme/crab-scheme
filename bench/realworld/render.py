#!/usr/bin/env python3
"""
Render the real-world bench harness's JSONL output into a markdown
table. One row per (benchmark, engine, tier); columns for wall-time
percentiles and memory metrics.

Usage:
    python3 render.py [path/to/results.jsonl]

Reads stdin if no path given. Writes markdown to stdout.

Pairs with bench/realworld/runner.sh (Phase C of the real-world
bench suite spec, docs/research/realworld_benchmarks_spec.md).
"""

import json
import sys


def fmt_seconds(s: float) -> str:
    """Format seconds with the resolution the magnitude suggests."""
    if s >= 1.0:
        return f"{s:.3f}s"
    if s >= 1e-3:
        return f"{s * 1000:.2f}ms"
    if s >= 1e-6:
        return f"{s * 1e6:.1f}µs"
    return f"{s * 1e9:.0f}ns"


def fmt_bytes(b: int) -> str:
    if b >= 1024 * 1024 * 1024:
        return f"{b / (1024 ** 3):.2f}GB"
    if b >= 1024 * 1024:
        return f"{b / (1024 ** 2):.2f}MB"
    if b >= 1024:
        return f"{b / 1024:.1f}KB"
    return f"{b}B"


def fmt_pct(p: float) -> str:
    return f"{p:.2f}%"


def load(path):
    if path == "-" or path is None:
        f = sys.stdin
    else:
        f = open(path, "r", encoding="utf-8")
    rows = []
    for ln in f:
        ln = ln.strip()
        if not ln:
            continue
        try:
            rows.append(json.loads(ln))
        except json.JSONDecodeError as e:
            print(f"WARN: skipping unparseable line: {e}", file=sys.stderr)
    if f is not sys.stdin:
        f.close()
    return rows


def render(rows):
    # Sort rows: by bench name, then engine, then tier.
    rows = sorted(
        rows,
        key=lambda r: (
            r.get("benchmark", ""),
            r.get("engine", ""),
            r.get("engine_tier", ""),
        ),
    )

    # Header.
    headers = [
        "benchmark",
        "engine/tier",
        "iters",
        "p50",
        "p95",
        "p99",
        "stddev",
        "bytes/iter",
        "GC%",
        "max pause",
        "status",
    ]
    print("| " + " | ".join(headers) + " |")
    print("|" + "|".join("---" for _ in headers) + "|")

    for r in rows:
        bench = r.get("benchmark", "?")
        engine = r.get("engine", "?")
        tier = r.get("engine_tier", "?")
        engine_tier = f"{engine}/{tier}"
        status = r.get("status", "ok")
        if status != "ok":
            # Error row — fill blanks.
            cells = [bench, engine_tier, "-", "-", "-", "-", "-", "-", "-", "-", status]
            print("| " + " | ".join(cells) + " |")
            continue
        wall = r.get("wall_time_seconds", {})
        mem = r.get("memory", {})
        iters = r.get("config", {}).get("measured_iters", len(wall.get("iters", [])))
        p50 = fmt_seconds(wall.get("p50", 0))
        p95 = fmt_seconds(wall.get("p95", 0))
        p99 = fmt_seconds(wall.get("p99", 0))
        stddev = fmt_seconds(wall.get("stddev", 0))
        bytes_total = mem.get("bytes_allocated_total", 0)
        bytes_per = bytes_total // max(iters, 1)
        bpi = fmt_bytes(bytes_per)
        gc_pct = fmt_pct(mem.get("gc_time_pct", 0))
        max_pause = fmt_seconds(mem.get("max_pause_ms", 0) / 1000.0)
        cells = [
            bench,
            engine_tier,
            str(iters),
            p50,
            p95,
            p99,
            stddev,
            bpi,
            gc_pct,
            max_pause,
            status,
        ]
        print("| " + " | ".join(cells) + " |")


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else None
    rows = load(path)
    if not rows:
        print("no rows", file=sys.stderr)
        sys.exit(1)
    render(rows)


if __name__ == "__main__":
    main()
