# ADR 0035: `spawn-source` green by default

> Status: Accepted
> Date: 2026-06-06
> Authors: crab-scheme contributors

## Context

`(spawn-source SOURCE ENTRY …)` runs a free-form Scheme actor body — its own
`(receive)` / `(raw-receive)` / `(tcp-recv)` loop, the shape every real actor app
(crab-cache, the consensus demos) uses. Historically it ran on a **dedicated OS
thread** via `block_in_place`, which caps at `max_blocking_threads` (4096) and
costs a thread (≈ 1–2 MiB committed stack + scheduler overhead) per actor — so
memory and the ceiling scale with *connection count*, not work.

The green-threads milestone (spec `.spec-workflow/specs/green-threads`) made a
whole-body actor run as a stackful coroutine on the parking `LocalSet` pool
(`green_source_body`), where it **parks** (releases its worker) on
`(receive)` / `(sleep)` (M1) and on `(tcp-recv)` / `(tcp-send)` (M2, cooperative
async I/O). N ≫ M actors multiplex onto M workers; there is no thread-per-actor
ceiling. Behavioral parity (links, monitors, `DOWN`, trap-exit, panic→Error) and
a region-park guard are in place and tested.

With cooperative TCP landed (INV-1 satisfied — a blocking socket read no longer
freezes a shared worker), the dedicated default is the wrong default: it is the
*less* scalable choice for the I/O- and mailbox-bound actors that dominate.

## Decision

**`(spawn-source …)` is green by default.** Two explicit siblings pin the model:

- `(spawn-source-green …)` — always green (alias of the new default).
- `(spawn-source-dedicated …)` — always a dedicated `block_in_place` thread, for
  an actor that genuinely must own one: blocking work with **no cooperative
  counterpart** (a long blocking `fsync`; a sole-drainer poll loop that is also a
  protocol clock). These are the INV-2 / INV-3 cases from the spec.

A global escape hatch makes the flip reversible without a code change:
`CRABSCHEME_ACTOR_DEFAULT=dedicated` forces the old default (read once, cached;
set it before the first spawn). Anything else / unset = green.

`(spawn name …)` (Rust-closure bodies) is unchanged (still dedicated) — its
bodies are arbitrary Rust, out of scope here.

## Consequences

- **Scale:** actor count is bounded by memory, not the 4096 thread ceiling.
  Per-actor cost drops by one OS thread. (The remaining per-actor cost is the
  `Runtime` each actor builds; reducing *that* — a shared/pooled `Runtime` — is
  the next scale lever, tracked separately. The coroutine stack is touched-pages
  for RSS and a 1 MiB virtual reservation, `GREEN_STACK_BYTES`.)
- **Behavior:** green and dedicated converge on `on_actor_termination`; links /
  monitors / trap-exit / panic→Error behave identically. A Scheme-level error
  still logs + exits Normal on both paths (only a Rust panic → `Error`).
- **One real constraint to honor:** an actor doing a long *blocking* call with no
  cooperative form (fsync, a blocking 3rd-party FFI call) must use
  `spawn-source-dedicated`, or it will freeze its shared worker for the duration.
  Cooperative `(sleep)` / `(tcp-recv)` / `(tcp-send)` / `(receive)` are fine on
  green; everything else blocking is not.
- **`(with-region)` across a park** is refused on the green path (the shared TLS
  region stack would interleave); the actor dies with a clear error. Dedicated
  actors are unaffected.
- **Migration:** in-tree `spawn-source` callers were audited; none rely on
  dedicated-only semantics (the consensus demos run fine green — `raft-net-tcp`
  even gains cooperative I/O). crab-cache pins its shard + peer-poller actors to
  `spawn-source-dedicated` (fsync / Raft clock) and lets conns / pusher / broker
  go green.
- **Ceiling at extreme scale:** one mmap per live coroutine means ~`vm.max_map_count`
  (Linux default ~65 k) bounds concurrent green actors; operators raise the sysctl
  for 100 k+. Documented, not worked around here.
