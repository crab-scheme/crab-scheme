# Byte-cache & store native builtins

CrabScheme ships a small set of Rust-side builtins for building **high-throughput
byte caches** (length-prefixed key→value stores, Redis-style front-ends) in
Scheme without paying interpreter overhead on the hot path. They are
**semantics-free** — length-prefixed bytes only, with no value typing, TTL,
eviction, or protocol logic (that stays in Scheme) — and are the foundation of
[crab-cache](https://github.com/crab-scheme/crab-cache)'s fast paths.

Two groups: a **durable store** (RocksDB FFI, build with `--features stdlib-store`)
and **in-memory table + RESP framing** (the `beam` actor builtins, always on).

## Durable store (RocksDB FFI)

| builtin | args | returns |
|---|---|---|
| `store-open` | path | db-handle (fixnum) |
| `store-put` | db cf key val [sync?] | unspecified |
| `store-get` | db cf key | bytevector \| `#f` |
| `store-delete` | db cf key [sync?] | unspecified |
| `store-write-batch` | db ops [sync?] | unspecified |
| `store-flush-wal` | db [sync?] | unspecified |

Keys/values are bytevectors; `cf` is a column-family name string; `sync?`
(default `#f`) controls whether the call fsyncs before returning.

### Group commit: `store-write-batch` and `store-flush-wal`

Fsyncing on every write (`sync? = #t`) serializes all writers behind the fsync
barrier — on a real-fsync host that caps throughput at the device's fsync rate.
Two builtins amortize it:

- **`store-write-batch db ops [sync?]`** applies many ops under **one** fsync.
  `ops` is a list where each element is `("put" cf key val)` or
  `("delete" cf key)`. Use it to collapse a multi-write command (an MSET, or a
  value + directory-entry pair) into a single synced write instead of N.
  ```scheme
  (store-write-batch db (list (list "put" "default" k v)
                              (list "put" "meta"    dk dv)) #t)  ; one fsync, both writes
  ```

- **`store-flush-wal db [sync?]`** is the **group-commit** primitive. Write each
  record with `sync? = #f` (it goes to the WAL, flushed to the OS but *not*
  fsync'd), accumulate a batch across concurrent writers, then call
  `(store-flush-wal db #t)` **once** — a single fsync that durably persists every
  accumulated record. Ack the waiters only *after* it returns.
  ```scheme
  ;; per write (no fsync, just the WAL):
  (store-put db cf k v #f)
  ;; once per batch/tick (one fsync covers the whole batch):
  (store-flush-wal db #t)
  ```
  This is how crab-cache lifted durable SET **~6×** — one fsync per batch/tick
  instead of two per write — beating etcd ~6× and closing most of the Redis gap
  (see crab-cache's `docs/measurements/2026-06-07-linux-fsync-vs-etcd.md`). The
  durability invariant is yours to uphold: **never ack a writer before its record
  is inside a completed `store-flush-wal`.**

## In-memory table + RESP framing (beam)

A byte cache serves reads from an in-memory [`cs-table`] keyed by raw key bytes.
These builtins frame the reply straight from the stored payload — no deep clone,
no Scheme encode pass.

| builtin | args | returns |
|---|---|---|
| `table-get-resp-bulk` | name key | bytevector (`$<len>\r\n<bytes>\r\n`) \| `#f` |
| `conn-serve-gets` | data node-name nshards | `(cons out consumed)` |

- **`table-get-resp-bulk name key`** looks up a **bytevector** value in table
  `name` and returns its RESP *bulk* framing as one fresh bytevector (`#f` if the
  key is absent or its value isn't a bytevector). The value bytes go straight from
  the table payload into the framed buffer — skipping both the `table-lookup`
  deep-clone and a separate `resp-encode`.

- **`conn-serve-gets data node-name nshards`** is the **fully-fused GET path**.
  Given a raw RESP read buffer, it serves the *leading run* of locally-led GET
  **hits** entirely in Rust: for each `*2\r\n$3\r\nGET\r\n$K\r\n<key>\r\n` frame it
  parses, CRC16-slot-hashes (Redis-Cluster-compatible, hashtag-aware), checks
  leadership (`cc-shard-leader["<node>:<shard>"] == node-name`), looks up `cc-str`,
  and appends the bulk frame — then **stops** at the first frame that is anything
  else (partial, non-GET, wrong arity, non-local slot, or a miss), consuming
  nothing past it. Returns `(cons out consumed)`:
  ```scheme
  (let* ((fg (conn-serve-gets data node nshards))
         (out (car fg)) (consumed (cdr fg)) (dlen (bytevector-length data)))
    (when (> (bytevector-length out) 0) (tcp-send sock out))
    (if (= consumed dlen)
        (loop empty-buf)
        (interpreted-path (subbv data consumed dlen))))   ; SET/non-local/miss/SUBSCRIBE/partial
  ```
  Batch-processing every GET in a read buffer in one call took crab-cache GET from
  ~40k → **121k rps @ -P1** and unlocked pipelining (**~1.5M @ -P16**, beating
  Redis pipelined).

> **Why semantics-free?** TTL, eviction, value types, cluster topology, and
> protocol edge cases live in Scheme; these builtins only do the byte-level hot
> work (lookup, CRC16, length-prefix framing, batched fsync) that an interpreter
> shouldn't sit on the critical path for. A miss or any non-trivial case falls
> back to Scheme — so correctness (TTL lazy-expiry, MOVED, warm-on-miss) is never
> decided in Rust.

[`cs-table`]: ../../crates/cs-table
