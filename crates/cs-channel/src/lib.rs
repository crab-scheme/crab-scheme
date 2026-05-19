//! cs-channel — first-class MPMC channels for CrabScheme.
//!
//! See `docs/research/channels_spec.md` for the design. Summary:
//!
//! - `Channel` wraps a tokio mpsc sender + receiver pair. Bounded
//!   channels (`new_bounded(n)`) block sends at capacity; unbounded
//!   (`new_unbounded()`) never blocks sends.
//! - Receivers are wrapped in a `Mutex` so the channel is MPMC at
//!   the API level: any number of consumers can call `recv` on the
//!   same channel; they serialize through the lock, one pending at
//!   a time. For true parallel MPMC at high throughput, a future
//!   v2 could swap to `flume` or `async_channel`.
//! - `close()` is a soft flag + dropping the original sender.
//!   Pending messages drain; subsequent `try_send` returns
//!   `ChannelError::Closed`; `recv` returns `Ok(None)` once the
//!   buffer is empty.
//! - `ChannelRegistry` is the process-global table mapping
//!   `ChannelId` → `Arc<Channel>`. Channels created in one actor
//!   are reachable from another by ID alone, so the cs-runtime
//!   bridge can carry channels as `SendableValue::Channel(id)`
//!   across actor boundaries.
//!
//! Payload type is `Arc<dyn Any + Send + Sync>` (same shape as
//! `cs_actor::Payload`) so cs-channel can compile without a dep on
//! cs-runtime; the runtime wraps SendableValue at the boundary.

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use dashmap::DashMap;
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};

/// Type-erased payload type. Channels carry these; the cs-runtime
/// bridge wraps `SendableValue` into one on send and downcasts on
/// recv.
pub type Payload = Arc<dyn Any + Send + Sync>;

/// Opaque channel handle. Cheap to copy / hash; valid for the
/// lifetime of the registry slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId(pub u64);

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<channel:{}>", self.0)
    }
}

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("channel {0} not found (dropped?)")]
    NotFound(ChannelId),
    #[error("channel {0} is closed")]
    Closed(ChannelId),
    #[error("channel {0} would block")]
    WouldBlock(ChannelId),
}

/// One channel. Owns its sender + receiver pair; the receiver is
/// behind a `Mutex` so multiple actors can call recv (they
/// serialize at the lock).
pub struct Channel {
    id: ChannelId,
    capacity: Option<usize>,
    depth: AtomicUsize,
    closed: AtomicBool,
    inner: ChannelInner,
}

enum ChannelInner {
    // Senders live behind a `Mutex<Option<…>>` so `close()` can
    // *drop* them — receivers see EOF only when every Sender
    // drops, so a soft close-flag alone leaves a blocking `recv`
    // hanging forever. Hot-path sends clone the Sender out under
    // the lock, drop the lock, then `.await` the clone — no lock
    // crosses the await point.
    Unbounded {
        tx: StdMutex<Option<mpsc::UnboundedSender<Payload>>>,
        rx: Mutex<mpsc::UnboundedReceiver<Payload>>,
    },
    Bounded {
        tx: StdMutex<Option<mpsc::Sender<Payload>>>,
        rx: Mutex<mpsc::Receiver<Payload>>,
    },
}

impl Channel {
    fn new_unbounded(id: ChannelId) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id,
            capacity: None,
            depth: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            inner: ChannelInner::Unbounded {
                tx: StdMutex::new(Some(tx)),
                rx: Mutex::new(rx),
            },
        }
    }

    fn new_bounded(id: ChannelId, capacity: usize) -> Self {
        // tokio mpsc requires capacity >= 1. The spec's
        // "unbuffered (capacity 0)" rendezvous semantics are a
        // CH-C deliverable — v1 floors at 1.
        let cap = capacity.max(1);
        let (tx, rx) = mpsc::channel(cap);
        Self {
            id,
            capacity: Some(cap),
            depth: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            inner: ChannelInner::Bounded {
                tx: StdMutex::new(Some(tx)),
                rx: Mutex::new(rx),
            },
        }
    }

    pub fn id(&self) -> ChannelId {
        self.id
    }
    pub fn capacity(&self) -> Option<usize> {
        self.capacity
    }
    pub fn len(&self) -> usize {
        self.depth.load(Ordering::Acquire)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Blocking send. Returns `Closed` if the channel was already
    /// closed when the send was attempted.
    pub async fn send(&self, v: Payload) -> Result<(), ChannelError> {
        if self.is_closed() {
            return Err(ChannelError::Closed(self.id));
        }
        // Clone the sender out under the std lock, drop the
        // lock, then await on the clone — never hold a lock
        // across an .await point.
        let result = match &self.inner {
            ChannelInner::Unbounded { tx, .. } => {
                let tx_clone = match tx.lock().expect("tx lock poisoned").as_ref() {
                    Some(s) => s.clone(),
                    None => return Err(ChannelError::Closed(self.id)),
                };
                tx_clone.send(v).map_err(|_| ChannelError::Closed(self.id))
            }
            ChannelInner::Bounded { tx, .. } => {
                let tx_clone = match tx.lock().expect("tx lock poisoned").as_ref() {
                    Some(s) => s.clone(),
                    None => return Err(ChannelError::Closed(self.id)),
                };
                tx_clone
                    .send(v)
                    .await
                    .map_err(|_| ChannelError::Closed(self.id))
            }
        };
        if result.is_ok() {
            self.depth.fetch_add(1, Ordering::AcqRel);
        }
        result
    }

    /// Non-blocking send. Returns `Ok(true)` on success, `Ok(false)`
    /// if the channel was at capacity (would-block), or
    /// `Err(Closed)` if closed.
    pub fn try_send(&self, v: Payload) -> Result<bool, ChannelError> {
        if self.is_closed() {
            return Err(ChannelError::Closed(self.id));
        }
        let ok = match &self.inner {
            ChannelInner::Unbounded { tx, .. } => {
                let guard = tx.lock().expect("tx lock poisoned");
                match guard.as_ref() {
                    Some(s) => match s.send(v) {
                        Ok(()) => true,
                        Err(_) => return Err(ChannelError::Closed(self.id)),
                    },
                    None => return Err(ChannelError::Closed(self.id)),
                }
            }
            ChannelInner::Bounded { tx, .. } => {
                let guard = tx.lock().expect("tx lock poisoned");
                match guard.as_ref() {
                    Some(s) => match s.try_send(v) {
                        Ok(()) => true,
                        Err(mpsc::error::TrySendError::Full(_)) => false,
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            return Err(ChannelError::Closed(self.id));
                        }
                    },
                    None => return Err(ChannelError::Closed(self.id)),
                }
            }
        };
        if ok {
            self.depth.fetch_add(1, Ordering::AcqRel);
        }
        Ok(ok)
    }

    /// Blocking recv. Returns `Ok(Some(v))` for a value,
    /// `Ok(None)` if the channel is closed AND drained.
    pub async fn recv(&self) -> Result<Option<Payload>, ChannelError> {
        let value = match &self.inner {
            ChannelInner::Unbounded { rx, .. } => {
                let mut guard = rx.lock().await;
                guard.recv().await
            }
            ChannelInner::Bounded { rx, .. } => {
                let mut guard = rx.lock().await;
                guard.recv().await
            }
        };
        if value.is_some() {
            self.depth.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(value)
    }

    /// Non-blocking recv. Returns `Ok(Some(v))` for a value,
    /// `Ok(None)` if the channel is empty (regardless of closed
    /// state). The caller distinguishes "empty open" from
    /// "empty closed" via `is_closed`.
    pub fn try_recv(&self) -> Result<Option<Payload>, ChannelError> {
        let value = match &self.inner {
            ChannelInner::Unbounded { rx, .. } => match rx.try_lock() {
                Ok(mut guard) => match guard.try_recv() {
                    Ok(v) => Some(v),
                    Err(mpsc::error::TryRecvError::Empty) => None,
                    Err(mpsc::error::TryRecvError::Disconnected) => None,
                },
                Err(_) => return Err(ChannelError::WouldBlock(self.id)),
            },
            ChannelInner::Bounded { rx, .. } => match rx.try_lock() {
                Ok(mut guard) => match guard.try_recv() {
                    Ok(v) => Some(v),
                    Err(mpsc::error::TryRecvError::Empty) => None,
                    Err(mpsc::error::TryRecvError::Disconnected) => None,
                },
                Err(_) => return Err(ChannelError::WouldBlock(self.id)),
            },
        };
        if value.is_some() {
            self.depth.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(value)
    }

    /// Mark the channel closed. Returns true if it transitioned
    /// from open to closed, false if already closed. Drops the
    /// Channel's owned Sender so that, once any in-flight send
    /// clone is also dropped, receivers see EOF (`recv().await`
    /// returns None) after draining the buffered messages.
    pub fn close(&self) -> bool {
        let was_open = !self.closed.swap(true, Ordering::AcqRel);
        if was_open {
            match &self.inner {
                ChannelInner::Unbounded { tx, .. } => {
                    *tx.lock().expect("tx lock poisoned") = None;
                }
                ChannelInner::Bounded { tx, .. } => {
                    *tx.lock().expect("tx lock poisoned") = None;
                }
            }
        }
        was_open
    }
}

/// Process-global registry of live channels. Channels live until
/// explicitly dropped via `drop_channel` (or until the registry
/// itself drops, but that only happens at process shutdown).
pub struct ChannelRegistry {
    next_id: AtomicU64,
    chans: DashMap<ChannelId, Arc<Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            chans: DashMap::new(),
        }
    }

    fn fresh_id(&self) -> ChannelId {
        ChannelId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Create a new channel. `capacity = None` → unbounded;
    /// `Some(n)` → bounded with at-least-1 capacity (the spec's
    /// "unbuffered" rendezvous (capacity 0) is a CH-C item).
    pub fn create(&self, capacity: Option<usize>) -> ChannelId {
        let id = self.fresh_id();
        let ch = match capacity {
            None => Channel::new_unbounded(id),
            Some(n) => Channel::new_bounded(id, n),
        };
        self.chans.insert(id, Arc::new(ch));
        id
    }

    pub fn lookup(&self, id: ChannelId) -> Option<Arc<Channel>> {
        self.chans.get(&id).map(|e| Arc::clone(e.value()))
    }

    /// Remove the channel from the registry. The Arc<Channel> may
    /// stay alive briefly while other holders drop their refs;
    /// the slot itself is reclaimable immediately.
    pub fn drop_channel(&self, id: ChannelId) -> bool {
        self.chans.remove(&id).is_some()
    }

    pub fn len(&self) -> usize {
        self.chans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chans.is_empty()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------
// channel-select — wait on first-ready of several clauses.
// ---------------------------------------------------------------

/// One clause of a `(select …)` form. cs-runtime parses the
/// Scheme-side clause list into this Rust enum before calling
/// `select_async`.
pub enum SelectClause {
    /// Wait for a value on the channel. On success, the
    /// SelectOutcome's `value` is the payload; if the channel
    /// closed-and-drained, `value` is `None` and outcome.kind
    /// is still `Recv`.
    Recv(Arc<Channel>),
    /// Wait for capacity to send `payload`. On success, value is
    /// `None` (the send happened); on closed-channel error,
    /// returned as `outcome.kind = SendClosed`.
    Send(Arc<Channel>, Payload),
    /// Fire after `duration` if nothing else is ready.
    After(std::time::Duration),
    /// Pseudo-clause: fire immediately if every other clause
    /// would block. Handled by a pre-pass before `select_all`.
    Else,
}

/// The shape of an awaited select clause's result.
pub enum SelectKind {
    Recv,
    Send,
    After,
    Else,
    /// Send clause hit a closed channel (counts as ready —
    /// the clause "fired" but couldn't deliver).
    SendClosed,
}

pub struct SelectOutcome {
    pub index: usize,
    pub kind: SelectKind,
    pub value: Option<Payload>,
}

/// Pre-pass: try every non-blocking clause synchronously. If any
/// is ready (recv has a value or is closed-drained), pick one and
/// return it. Otherwise: if an `Else` clause exists, fire it. If
/// neither, return `None` — caller must fall through to
/// `await_select` to actually block. This is sync, callable from
/// any context (REPL, top-level, no tokio runtime needed).
///
/// `clauses` is consumed; on Some, the chosen clause's payload
/// is moved out and the rest dropped; on None, all clauses move
/// into the returned Vec for await.
pub fn try_select(
    mut clauses: Vec<SelectClause>,
    biased: bool,
) -> Result<SelectOutcome, Vec<SelectClause>> {
    let mut else_idx: Option<usize> = None;
    let mut ready_indices: Vec<usize> = Vec::new();
    let mut ready_values: Vec<Option<Payload>> = Vec::new();
    let mut ready_kinds: Vec<SelectKind> = Vec::new();
    for (i, c) in clauses.iter().enumerate() {
        match c {
            SelectClause::Else => {
                if else_idx.is_none() {
                    else_idx = Some(i);
                }
            }
            SelectClause::Recv(ch) => match ch.try_recv() {
                Ok(Some(v)) => {
                    ready_indices.push(i);
                    ready_values.push(Some(v));
                    ready_kinds.push(SelectKind::Recv);
                }
                Ok(None) if ch.is_closed() => {
                    ready_indices.push(i);
                    ready_values.push(None);
                    ready_kinds.push(SelectKind::Recv);
                }
                _ => {} // WouldBlock / Empty — falls through.
            },
            SelectClause::Send(_, _) | SelectClause::After(_) => {
                // Send pre-pass would move the payload out under
                // try_send; skip and handle in the await branch.
                // After fires only as part of the await.
            }
        }
    }

    if !ready_indices.is_empty() {
        let pick = if biased {
            0
        } else {
            let now_ns = std::time::Instant::now().elapsed().as_nanos() as usize;
            now_ns % ready_indices.len()
        };
        return Ok(SelectOutcome {
            index: ready_indices[pick],
            kind: std::mem::replace(&mut ready_kinds[pick], SelectKind::Else),
            value: ready_values[pick].take(),
        });
    }

    if let Some(idx) = else_idx {
        // Drop everything (no more references needed).
        clauses.clear();
        return Ok(SelectOutcome {
            index: idx,
            kind: SelectKind::Else,
            value: None,
        });
    }

    Err(clauses)
}

/// Block until at least one clause is ready. Must be called
/// inside a tokio runtime context. Use [`try_select`] first to
/// short-circuit synchronous cases.
pub async fn await_select(clauses: Vec<SelectClause>, _biased: bool) -> SelectOutcome {
    use futures_util::future::select_all;
    let mut futs: Vec<futures_util::future::BoxFuture<'_, SelectOutcome>> =
        Vec::with_capacity(clauses.len());
    for (i, c) in clauses.into_iter().enumerate() {
        let fut: futures_util::future::BoxFuture<'_, SelectOutcome> = match c {
            SelectClause::Recv(ch) => Box::pin(async move {
                let v = ch.recv().await.ok().flatten();
                SelectOutcome {
                    index: i,
                    kind: SelectKind::Recv,
                    value: v,
                }
            }),
            SelectClause::Send(ch, v) => Box::pin(async move {
                let kind = match ch.send(v).await {
                    Ok(()) => SelectKind::Send,
                    Err(_) => SelectKind::SendClosed,
                };
                SelectOutcome {
                    index: i,
                    kind,
                    value: None,
                }
            }),
            SelectClause::After(d) => Box::pin(async move {
                tokio::time::sleep(d).await;
                SelectOutcome {
                    index: i,
                    kind: SelectKind::After,
                    value: None,
                }
            }),
            SelectClause::Else => {
                // try_select should have caught this — defensive
                // pending future so it doesn't win the race.
                Box::pin(std::future::pending::<SelectOutcome>())
            }
        };
        futs.push(fut);
    }

    let (out, _, _) = select_all(futs).await;
    out
}

/// Convenience: combine `try_select` + `await_select`. Caller
/// must be inside a tokio runtime context if any clause might
/// block.
pub async fn select_async(clauses: Vec<SelectClause>, biased: bool) -> SelectOutcome {
    match try_select(clauses, biased) {
        Ok(outcome) => outcome,
        Err(remaining) => await_select(remaining, biased).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper for the synchronous tests below — wraps a single-
    /// threaded tokio runtime so we can call `.await` from a
    /// `#[test]` fn without `#[tokio::test]` machinery.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn p<T: Send + Sync + 'static>(v: T) -> Payload {
        Arc::new(v)
    }

    fn downcast_i32(p: Payload) -> i32 {
        *p.downcast::<i32>().expect("i32")
    }

    #[test]
    fn unbounded_round_trip() {
        let r = rt();
        let reg = ChannelRegistry::new();
        let id = reg.create(None);
        let ch = reg.lookup(id).unwrap();

        r.block_on(async {
            ch.send(p(1i32)).await.unwrap();
            ch.send(p(2i32)).await.unwrap();
            assert_eq!(ch.len(), 2);
            assert_eq!(downcast_i32(ch.recv().await.unwrap().unwrap()), 1);
            assert_eq!(downcast_i32(ch.recv().await.unwrap().unwrap()), 2);
            assert_eq!(ch.len(), 0);
        });
    }

    #[test]
    fn bounded_try_send_returns_false_when_full() {
        let reg = ChannelRegistry::new();
        let id = reg.create(Some(2));
        let ch = reg.lookup(id).unwrap();
        assert_eq!(ch.try_send(p(1i32)).unwrap(), true);
        assert_eq!(ch.try_send(p(2i32)).unwrap(), true);
        assert_eq!(ch.try_send(p(3i32)).unwrap(), false); // full
        assert_eq!(ch.len(), 2);
    }

    #[test]
    fn close_then_send_errors_recv_drains() {
        let r = rt();
        let reg = ChannelRegistry::new();
        let id = reg.create(None);
        let ch = reg.lookup(id).unwrap();
        ch.try_send(p(1i32)).unwrap();
        ch.try_send(p(2i32)).unwrap();
        assert!(ch.close());
        assert!(!ch.close()); // idempotent
        assert!(ch.is_closed());

        // Sends after close error.
        match ch.try_send(p(3i32)) {
            Err(ChannelError::Closed(_)) => {}
            other => panic!("expected Closed, got {:?}", other),
        }

        // Receivers drain the buffered messages.
        r.block_on(async {
            assert_eq!(downcast_i32(ch.recv().await.unwrap().unwrap()), 1);
            assert_eq!(downcast_i32(ch.recv().await.unwrap().unwrap()), 2);
        });
    }

    #[test]
    fn registry_drop_channel_removes_slot() {
        let reg = ChannelRegistry::new();
        let id = reg.create(None);
        assert_eq!(reg.len(), 1);
        assert!(reg.drop_channel(id));
        assert_eq!(reg.len(), 0);
        assert!(reg.lookup(id).is_none());
        assert!(!reg.drop_channel(id)); // idempotent
    }

    #[test]
    fn try_recv_returns_none_on_empty() {
        let reg = ChannelRegistry::new();
        let id = reg.create(None);
        let ch = reg.lookup(id).unwrap();
        assert!(ch.try_recv().unwrap().is_none());
    }

    #[test]
    fn cross_thread_mpmc_serializes_through_recv_mutex() {
        let r = rt();
        let reg = Arc::new(ChannelRegistry::new());
        let id = reg.create(None);
        let ch = reg.lookup(id).unwrap();

        // Producer fires 100 messages from another task then
        // closes — required so the consumers' `recv().await`
        // returns None on the empty-and-closed condition and
        // they break out cleanly.
        let prod_ch = Arc::clone(&ch);
        r.spawn(async move {
            for i in 0..100i32 {
                prod_ch.send(p(i)).await.unwrap();
            }
            prod_ch.close();
        });

        // Two consumers each pull until they collectively see 100.
        let total = Arc::new(AtomicUsize::new(0));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let ch = Arc::clone(&ch);
            let total = Arc::clone(&total);
            joins.push(r.spawn(async move {
                loop {
                    match ch.recv().await {
                        Ok(Some(_)) => {
                            if total.fetch_add(1, Ordering::AcqRel) + 1 >= 100 {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }));
        }
        r.block_on(async {
            for j in joins {
                let _ = j.await;
            }
        });
        assert_eq!(total.load(Ordering::Acquire), 100);
    }
}
