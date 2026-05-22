# M02 transport benchmark — how the QUIC cluster transport functions

Empirical characterisation of the cs-net transports (`Sim` / `TCP` /
`TCP+mTLS` / `QUIC`) under actor-systems workloads. Reproduces the kernels
the literature uses — Savina's ping-pong latency micro-benchmark
(AGERE'14), one-way throughput, and a head-of-line-blocking probe in the
spirit of PARTISAN (USENIX ATC'19) and the KTH study *"Low-latency
transport protocols in actor systems"* (2023).

**Harness:** `crates/cs-net/examples/actor_bench.rs`. Every transport runs
the *same* kernel through the `Transport` trait, so the comparison is
apples-to-apples.

```sh
cargo run --release -p cs-net --example actor_bench                  # Sim + TCP + mTLS
cargo run --release -p cs-net --example actor_bench --features quic  # + QUIC
```

> **Caveat:** loopback only, and the kernels busy-poll the sync `try_recv`
> bridge. Absolute numbers reflect *software* overhead (framing, syscalls,
> crypto, scheduling), **not** a real network. The cross-transport
> **ratios** are the signal, not the microsecond values.

## Results (one representative `--features quic` run, M3-class macOS)

| Transport | ping-pong RTT | thrpt 64 B | thrpt 1 KiB | thrpt 64 KiB | ctrl RTT idle | ctrl RTT under bulk | HoL factor |
|-----------|--------------:|-----------:|------------:|-------------:|--------------:|--------------------:|-----------:|
| **Sim** (in-memory) | 0.05 µs | 19.5 M/s · 1190 MiB/s | 4.39 M/s · 4287 MiB/s | 188 K/s · 11750 MiB/s | 0.05 µs | 0.05 µs | **0.9×** |
| **TCP** (plaintext) | 34 µs | 525 K/s · 32 MiB/s | 428 K/s · 418 MiB/s | 100 K/s · 6268 MiB/s | 30 µs | 183 µs | **6.2×** |
| **TCP + mTLS** | 53 µs | 390 K/s · 24 MiB/s | 399 K/s · 390 MiB/s | 54 K/s · 3367 MiB/s | 30 µs | 475 µs | **16.0×** |
| **QUIC** (mTLS, per-channel streams) | 41 µs | 1.72 M/s · 105 MiB/s | 324 K/s · 316 MiB/s | 5.6 K/s · 352 MiB/s | 40 µs | 1998 µs | **49.5×** |

(Run-to-run variance is high for Sim and small-message throughput; the
ordering and the HoL factors are stable across runs.)

## What we learned

### 1. Ping-pong latency — all transports are in the same ballpark
TCP ≈ 34 µs, QUIC ≈ 41 µs, mTLS ≈ 53 µs per round trip on loopback. QUIC is
*not* faster than TCP for tiny request/reply, and mTLS adds a per-message
crypto tax — exactly the KTH finding (QUIC's per-packet/event-loop overhead
keeps it from beating TCP on small-message actor ping-pong). Sim at 0.05 µs
is the in-process floor (a `Mutex` + `VecDeque` + memcpy).

### 2. Throughput — QUIC wins tiny, loses big (in our impl)
QUIC is **~3.3× faster than TCP at 64 B** (1.72 M vs 525 K msg/s): it
coalesces many small stream writes into few UDP datagrams. But QUIC is
**~17× slower than TCP at 64 KiB** (352 vs 6268 MiB/s). That large-message
collapse is an implementation artifact, not a QUIC property — see below.

### 3. Head-of-line blocking — the surprise, and the actionable finding
The whole point of one-QUIC-stream-per-channel is that a saturated `Bulk`
transfer must **not** delay `Control`. The benchmark shows the opposite:
QUIC's HoL factor (49×) is **worse** than single-stream TCP (6×). Our
per-channel streams are being defeated by two things:

1. **QUIC connection-level flow control is shared across streams.** quinn's
   `receive_window` is *"the maximum bytes the peer may transmit across all
   streams of a connection before becoming blocked."* The bulk flood
   exhausts that connection-wide budget, so `Control`'s own stream can't get
   credit. The quinn docs describe our exact failure mode under
   `stream_receive_window`: *"a single stream [may] monopolize receive
   buffers ... while still requiring data on other streams."* We set neither
   window, so we inherit defaults that bulk can monopolise.
2. **A single writer task + single mpsc serialises all channels.**
   `QuicTransport::from_connection` funnels every channel through one
   `mpsc` and one writer task (`while let Some((ch, payload)) = out_rx.recv()`).
   When the bulk stream's `write_all` blocks on flow control, that one task
   is stuck, and `Control` frames queued behind it in the FIFO mpsc can't be
   written. So channel "priorities" are advisory only — the same is true of
   the TCP transport (its 6–16× HoL factor has the same single-mpsc cause).

## Recommended follow-ups (turn the design into the benefit)

All addressable with APIs already in our quinn version:

- **Size the windows** on `quinn::TransportConfig`: raise `receive_window`
  (connection-wide) well above the bulk in-flight bytes, and keep
  `stream_receive_window` per-stream smaller, so `Bulk` cannot monopolise the
  connection budget and starve `Control`. This is the documented fix for #1
  and almost certainly also recovers the 64 KiB throughput.
- **Per-channel writer tasks** (or write straight from `send()` into a
  per-stream buffer) so a blocked `Bulk` `write_all` cannot stall `Control`.
  Fixes #2 for QUIC.
- **Map our `Channel` priority onto QUIC** via `SendStream::set_priority`
  (Control/Consensus high) and `TransportConfig::send_fairness`, so the wire
  scheduler honours the priority the `Channel` enum already declares.
- **TCP**: replace the single FIFO mpsc with per-channel queues drained in
  priority order, so the TCP transport's advisory priorities become real.

These are perf/robustness refinements on top of the M02 substrate — the
transport's behaviour is now *measured*, and the gap between "per-channel
streams exist" and "per-channel isolation is delivered" is quantified.
