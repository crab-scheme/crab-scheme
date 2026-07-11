//! The universal Scheme value type.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::number::Number;
use crate::symbol::{Symbol, SymbolTable};

// Bring the CycleVisit trait into scope so per-type
// `visit_children` calls resolve under the feature.
use cs_gc::cycle::CycleVisit as _;

/// Bit of [`Pair::flags`] set when this pair's address has an
/// entry in [`PAIR_SPANS`].
const HAS_SPAN: u8 = 0b001;
/// Bit of [`Pair::flags`] set when this pair's address has a car
/// tombstone in [`PAIR_CAR_TOMBSTONES`].
const HAS_CAR_TOMBSTONE: u8 = 0b010;
/// Bit of [`Pair::flags`] set when this pair's address has a cdr
/// tombstone in [`PAIR_CDR_TOMBSTONES`].
const HAS_CDR_TOMBSTONE: u8 = 0b100;

thread_local! {
    /// Out-of-line reader source-span table (cs-5te Pair diet).
    ///
    /// Keyed by the `Pair`'s allocation address — the same
    /// identity `Gc::as_addr` uses, obtainable as `&self as *const
    /// Pair as usize` from inside a `&Pair` method since `Gc<Pair>`
    /// derefs straight to the `Rc`-boxed value. Only reader-
    /// produced pairs ([`Pair::with_source`]) ever insert an
    /// entry; ordinary `(cons …)` pairs never touch this map.
    ///
    /// Lifecycle: entries are removed in `Pair::drop` (see
    /// `Pair::flags`), so a freed address can never resurrect a
    /// stale span for whatever pair the allocator later places
    /// there. The runtime is single-threaded per actor (`Pair`
    /// contains `RefCell`/`Rc` and so is already `!Send`/`!Sync`),
    /// which is what makes a thread-local sound here.
    static PAIR_SPANS: RefCell<HashMap<usize, cs_diag::Span>> =
        RefCell::new(HashMap::new());

    /// Out-of-line car cycle-break tombstone table. Populated only
    /// when the mutation cycle detector demotes the car edge from
    /// strong to weak ([`Pair::break_car_cycle`]) — essentially
    /// never in ordinary programs. Cleared on `set_car` over a
    /// demoted slot and on `Pair::drop`; see `Pair::flags` for the
    /// has-entry invariant.
    static PAIR_CAR_TOMBSTONES: RefCell<HashMap<usize, WeakValue>> =
        RefCell::new(HashMap::new());

    /// Cdr counterpart of [`PAIR_CAR_TOMBSTONES`]; see
    /// [`Pair::break_cdr_cycle`].
    static PAIR_CDR_TOMBSTONES: RefCell<HashMap<usize, WeakValue>> =
        RefCell::new(HashMap::new());
}

/// A pair (cons cell). Mutable per R6RS via `set-car!` / `set-cdr!`.
///
/// Pairs that originate from the reader carry their source-text span,
/// populated by `Datum::to_value`. Pairs created at run time via
/// `(cons …)` carry none. This is the foundation that R6RS++ §9's
/// `(syntax-source …)` accessors read. As of cs-5te the span itself
/// lives out of line in [`PAIR_SPANS`] (keyed by address) — it's cold
/// metadata that only reader pairs ever populate, so keeping it
/// inline would tax every cons cell for a feature most of them never
/// use. `flags` bit [`HAS_SPAN`] records whether this pair has an
/// entry, so the common no-span case is a single `Cell::get` with no
/// hashing.
///
/// # Cycle-break tombstones (countable-memory iter 7.1)
///
/// The mutation cycle detector in `cs-runtime` flips a slot from
/// strong to weak via [`break_car_cycle`]/[`break_cdr_cycle`]: the
/// strong `Value` gets replaced with `Unspecified` and a `WeakValue`
/// referring to the same allocation is parked in the out-of-line
/// tombstone table ([`PAIR_CAR_TOMBSTONES`]/[`PAIR_CDR_TOMBSTONES`],
/// cs-5te — previously an inline `RefCell<Option<WeakValue>>` per
/// slot). Reads via the [`car`]/[`cdr`] accessors transparently
/// `upgrade()` the tombstone, so the user-observable cyclic structure
/// stays intact (R6RS requires `(set-cdr! x x)` to produce an
/// observable cyclic list); but the strong-count chain is broken so
/// refcount reclaims the cycle when no other strong reference
/// remains. `flags` bits [`HAS_CAR_TOMBSTONE`]/[`HAS_CDR_TOMBSTONE`]
/// record whether a slot has a tombstone, so `car()`/`cdr()` pay one
/// `Cell::get` plus (in the overwhelmingly common untombstoned case)
/// one `RefCell::borrow` instead of two.
///
/// Direct field access to `.car`/`.cdr` is preserved for
/// backward compatibility with code paths that don't need to
/// observe broken cycles (the strong slot is `Unspecified` for
/// such cells). New code should prefer the accessors.
#[derive(Debug)]
pub struct Pair {
    pub car: RefCell<Value>,
    pub cdr: RefCell<Value>,
    /// Cold-metadata presence bits: see [`HAS_SPAN`],
    /// [`HAS_CAR_TOMBSTONE`], [`HAS_CDR_TOMBSTONE`]. `Cell` so the
    /// post-construction span/tombstone setters don't need
    /// `&mut Pair`.
    flags: Cell<u8>,
}

impl Pair {
    /// This pair's out-of-line-table key: the same address identity
    /// `Gc::as_addr` computes (`Rc::as_ptr`), obtained directly from
    /// `&self` since `Gc<Pair>` derefs straight to it.
    fn addr(&self) -> usize {
        self as *const Pair as usize
    }

    pub fn new(car: Value, cdr: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Pair {
            car: RefCell::new(car),
            cdr: RefCell::new(cdr),
            flags: Cell::new(0),
        })
    }

    /// Construct a pair tagged with its reader-produced source span.
    pub fn with_source(car: Value, cdr: Value, span: cs_diag::Span) -> cs_gc::Gc<Self> {
        let gc = cs_gc::Gc::new(Pair {
            car: RefCell::new(car),
            cdr: RefCell::new(cdr),
            flags: Cell::new(HAS_SPAN),
        });
        let addr = cs_gc::Gc::as_addr(&gc);
        // `try_with` for TLS-teardown robustness; a failed insert
        // (thread exiting mid-read — vanishingly unlikely) degrades
        // to a span-less pair rather than a panic.
        if PAIR_SPANS
            .try_with(|t| {
                t.borrow_mut().insert(addr, span);
            })
            .is_err()
        {
            gc.flags.set(gc.flags.get() & !HAS_SPAN);
        }
        gc
    }

    pub fn source_span(&self) -> Option<cs_diag::Span> {
        if self.flags.get() & HAS_SPAN == 0 {
            return None;
        }
        let addr = self.addr();
        // `try_with`: if the thread's TLS is already tearing down
        // (a pair being dropped from another thread-local's dtor),
        // the table is gone — report "no span" rather than panic.
        PAIR_SPANS
            .try_with(|t| t.borrow().get(&addr).copied())
            .ok()
            .flatten()
    }

    /// Construct a Pair allocated in `region`'s bump arena
    /// (region-memory iter 4). Layer-5 escape analysis emits
    /// this when it proves the Pair's lifetime is bounded by
    /// some surrounding scope.
    ///
    /// Region pairs never carry a source span (the reader always
    /// allocates via `with_source`/`Gc::new`) and never get demoted
    /// by the cycle detector (`is_region` guards every
    /// `break_car_cycle`/`break_cdr_cycle` call site, and
    /// `Gc::downgrade` panics on a region handle) — so `flags`
    /// starts and stays `0` and no out-of-line table entry is ever
    /// created for a region-allocated address. That matters because
    /// region drop is a bulk arena free that does NOT run `Pair`'s
    /// `Drop::drop` (no dtor is registered for it), so any table
    /// entry keyed by a region address would otherwise leak forever.
    #[cfg(feature = "regions")]
    pub fn new_in(region: &cs_gc::Region, car: Value, cdr: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new_in(
            region,
            Pair {
                car: RefCell::new(car),
                cdr: RefCell::new(cdr),
                flags: Cell::new(0),
            },
        )
    }

    /// Read the car as a `Value`. An upgraded weak tombstone takes
    /// precedence over the strong slot — so broken cycles still
    /// produce the user-observable cyclic value as long as some
    /// other strong reference holds the target alive. Returns
    /// `Value::Unspecified` if the tombstone target has been fully
    /// reclaimed.
    pub fn car(&self) -> Value {
        if self.flags.get() & HAS_CAR_TOMBSTONE != 0 {
            let addr = self.addr();
            // `try_with`: during TLS teardown the table is gone —
            // treat as a reclaimed tombstone target.
            let upgraded = PAIR_CAR_TOMBSTONES
                .try_with(|t| t.borrow().get(&addr).and_then(WeakValue::upgrade))
                .ok()
                .flatten();
            return upgraded.unwrap_or(Value::Unspecified);
        }
        self.car.borrow().clone()
    }

    /// Read the cdr as a `Value`. See [`car`] for the
    /// tombstone semantics.
    pub fn cdr(&self) -> Value {
        if self.flags.get() & HAS_CDR_TOMBSTONE != 0 {
            let addr = self.addr();
            let upgraded = PAIR_CDR_TOMBSTONES
                .try_with(|t| t.borrow().get(&addr).and_then(WeakValue::upgrade))
                .ok()
                .flatten();
            return upgraded.unwrap_or(Value::Unspecified);
        }
        self.cdr.borrow().clone()
    }

    /// Replace the car slot with `v`. Clears any weak tombstone
    /// (the new value is unambiguously strong).
    pub fn set_car(&self, v: Value) {
        if self.flags.get() & HAS_CAR_TOMBSTONE != 0 {
            self.flags.set(self.flags.get() & !HAS_CAR_TOMBSTONE);
            let addr = self.addr();
            // `try_with`: if TLS is tearing down, the table (and its
            // entry) is being freed wholesale — nothing to remove.
            let _ = PAIR_CAR_TOMBSTONES.try_with(|t| {
                t.borrow_mut().remove(&addr);
            });
        }
        self.car.replace(v);
    }

    /// Replace the cdr slot with `v`. Clears any weak tombstone.
    pub fn set_cdr(&self, v: Value) {
        if self.flags.get() & HAS_CDR_TOMBSTONE != 0 {
            self.flags.set(self.flags.get() & !HAS_CDR_TOMBSTONE);
            let addr = self.addr();
            let _ = PAIR_CDR_TOMBSTONES.try_with(|t| {
                t.borrow_mut().remove(&addr);
            });
        }
        self.cdr.replace(v);
    }

    /// Cycle-break action for the car slot. Downgrades the
    /// current strong `Value` to a `WeakValue`, parks it in
    /// `car_weak`, and replaces the strong slot with
    /// `Unspecified`. Returns `true` on demote, `false` on skip.
    ///
    /// Skipped when:
    /// - The current car is a leaf value (no heap pointer to
    ///   downgrade).
    /// - The strong-count guard fires: the value's only strong
    ///   holders are this slot and the caller's mutation
    ///   argument (which is about to drop). Demoting would
    ///   orphan the value before any external reclaim could
    ///   observe the cycle.
    ///
    /// `baseline` is the number of transient strong refs to
    /// the value that the caller knows will drop right after
    /// this mutation completes. The slot itself counts (+1),
    /// plus any temporary references the caller holds
    /// (e.g., `args[1]` in `b_set_car`). The guard demotes
    /// only when total strong > baseline — i.e., there is at
    /// least one EXTERNAL anchor that will outlive the
    /// mutation. Without this gate the demote would orphan
    /// the value as soon as the caller's temps drop.
    ///
    /// Typical baselines:
    /// - Walker tier `b_set_car` / `b_set_cdr`: 2
    ///   (slot + `args[1]`).
    /// - VM tier helpers should pass their own baseline
    ///   reflecting NB decode + arg-slot + frame-stack
    ///   transient refs.
    ///
    /// The guard preserves observability for cycles whose
    /// only anchor is in the freshly-mutated subgraph (e.g.
    /// `(set-car! env (cons name val))` closures-over-env in
    /// the metacircular), at the cost of leaking those
    /// cycles refcount-wise. ADR 0014 §"iter 7.1.x.y" tracks
    /// the move from caller-supplied baselines to full
    /// Bacon-Rajan trial deletion that picks safe cycle
    /// edges without caller hints.
    pub fn break_car_cycle(&self, baseline: usize) -> bool {
        // Read strong count WITHOUT cloning so the count
        // reflects the slot's contribution plus the caller's
        // declared transient refs and any external anchors.
        let car_borrow = self.car.borrow();
        let total = car_borrow.heap_strong_count().unwrap_or(0);
        drop(car_borrow);
        if total <= baseline {
            return false;
        }
        let val = self.car();
        let Some(weak) = WeakValue::from_value(&val) else {
            return false; // leaf (shouldn't reach: heap_strong_count returned Some)
        };
        let addr = self.addr();
        // Insert-first ordering: the flag is only set if the entry
        // actually landed, preserving the flag⇔entry invariant. A
        // `try_with` failure (TLS teardown) declines the demote.
        if PAIR_CAR_TOMBSTONES
            .try_with(|t| {
                t.borrow_mut().insert(addr, weak);
            })
            .is_err()
        {
            return false;
        }
        self.flags.set(self.flags.get() | HAS_CAR_TOMBSTONE);
        *self.car.borrow_mut() = Value::Unspecified;
        true
    }

    /// Cycle-break action for the cdr slot. See
    /// [`break_car_cycle`] for the `baseline` parameter
    /// convention.
    pub fn break_cdr_cycle(&self, baseline: usize) -> bool {
        let cdr_borrow = self.cdr.borrow();
        let total = cdr_borrow.heap_strong_count().unwrap_or(0);
        drop(cdr_borrow);
        if total <= baseline {
            return false;
        }
        let val = self.cdr();
        let Some(weak) = WeakValue::from_value(&val) else {
            return false;
        };
        let addr = self.addr();
        if PAIR_CDR_TOMBSTONES
            .try_with(|t| {
                t.borrow_mut().insert(addr, weak);
            })
            .is_err()
        {
            return false;
        }
        self.flags.set(self.flags.get() | HAS_CDR_TOMBSTONE);
        *self.cdr.borrow_mut() = Value::Unspecified;
        true
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        // Clear this pair's out-of-line table entries (cs-5te). A
        // freed address CANNOT be left pointing at stale span/
        // tombstone data — the allocator may hand that same address
        // to an unrelated `Pair` next, which would otherwise
        // silently inherit someone else's source span or (worse) a
        // dangling tombstone. `flags == 0` (the overwhelming common
        // case — no span, no tombstone) short-circuits on a single
        // `Cell::get` with no hashing at all. This runs for every
        // `Pair` drop, including the intermediate pairs unlinked by
        // the cdr-chain walk below (each is a real `Pair` value
        // going out of scope, so its own `Drop::drop` fires too).
        //
        // `try_with`, not `with`: pairs also drop during TLS
        // teardown (e.g. a thread-local VM closure cache holding
        // list constants, freed by the runtime's `run_dtors` at
        // thread exit). At that point the side tables may already
        // be destroyed — and since the entire map is being freed
        // wholesale, skipping the per-entry removal loses nothing.
        let flags = self.flags.get();
        if flags != 0 {
            let addr = self.addr();
            if flags & HAS_SPAN != 0 {
                let _ = PAIR_SPANS.try_with(|t| {
                    t.borrow_mut().remove(&addr);
                });
            }
            if flags & HAS_CAR_TOMBSTONE != 0 {
                let _ = PAIR_CAR_TOMBSTONES.try_with(|t| {
                    t.borrow_mut().remove(&addr);
                });
            }
            if flags & HAS_CDR_TOMBSTONE != 0 {
                let _ = PAIR_CDR_TOMBSTONES.try_with(|t| {
                    t.borrow_mut().remove(&addr);
                });
            }
        }

        // Iteratively unlink the cdr chain. Without this,
        // dropping a long list `(cons x1 (cons x2 …))` triggers
        // recursive `Rc<Pair>::drop` calls — one stack frame per
        // pair — and overflows the host stack at ~100k-500k
        // elements (paraffins n=20 + macOS 8 MB default).
        //
        // The walk only descends when this Pair is the sole
        // strong holder of the next cdr-Pair (`Gc::into_inner`
        // returns `Some`). Shared pairs and region-backed pairs
        // stop the walk — their other holders / the region drop
        // is responsible for cleanup.
        let mut cur = self.cdr.replace(Value::Null);
        while let Value::Pair(gc) = cur {
            match cs_gc::Gc::into_inner(gc) {
                Some(mut pair) => {
                    cur = std::mem::replace(pair.cdr.get_mut(), Value::Null);
                    // `pair` drops at scope end: car drops
                    // naturally (typically a leaf), cdr is
                    // Null so no further recursion fires.
                }
                None => break,
            }
        }
    }
}

/// Gap C-3: layer-4 sweep dispatch for `Pair`. Called by
/// `cs_gc::cycle_registry::run_sweep` on every candidate.
/// Tries cdr-cycle first (more common back-edge from list
/// construction), then car-cycle. `baseline = 0` because the
/// sweep runs outside any mutation — no transient args[…]
/// refs inflate the strong count, so any strong_count > 0
/// reflects only persistent holders.
impl cs_gc::cycle::BreakCycle for Pair {
    fn try_break_cycle(&self) -> bool {
        self.break_cdr_cycle(0) || self.break_car_cycle(0)
    }
}

/// Gap C-3 ext: Hashtable layer-4 sweep dispatch. Scans the
/// items vec for the first slot whose value is a heap-
/// bearing `Value`; demotes it to `Unspecified`. Mirrors
/// the conservative pattern from `Pair::break_cdr_cycle`:
/// pick the first heap-bearing slot and break it.
/// Returns `true` if a slot was demoted; `false` if the
/// hashtable holds no heap-bearing values (no cycle to
/// break).
///
/// Hashtable cycles in practice are rare — they form when a
/// stored value transitively contains the hashtable itself
/// (e.g., `(hashtable-set! h 'key h)`). The cycle detector
/// (layer 2) flags these on the `hashtable-set!` site; this
/// trait impl lets the layer-4 sweep also reclaim them.
impl cs_gc::cycle::BreakCycle for Hashtable {
    fn try_break_cycle(&self) -> bool {
        // Take a borrow; iterate looking for the first
        // value slot that's heap-bearing (could close a
        // cycle). Replace it with Unspecified.
        let Ok(mut items) = self.items.try_borrow_mut() else {
            // Borrow failed (mutating elsewhere); skip this
            // sweep round, the next one will retry.
            return false;
        };
        for (_k, v) in items.iter_mut() {
            // Heap-bearing variants are exactly the ones
            // that could form a cycle through the hashtable.
            let demote = matches!(
                v,
                Value::Pair(_)
                    | Value::Vector(_)
                    | Value::Hashtable(_)
                    | Value::Promise(_)
                    | Value::String(_)
                    | Value::ByteVector(_)
            );
            if demote {
                *v = Value::Unspecified;
                return true;
            }
        }
        false
    }
}

/// Default no-op `BreakCycle` impls for Port + Promise. Port
/// holds no Gc children (its variants are all leaf states),
/// so the cycle counter would never fire for it anyway.
/// Promise stores a single Value which can hold Gc handles
/// — a future iter could give Promise a real break impl
/// (demote the pending/forced Value to Unspecified) but for
/// now Promise cycles are rare in practice and the layer-2
/// detector handles the common case.
///
/// Vector and String / ByteVector are `Gc<RefCell<...>>`
/// where `RefCell` is foreign — the orphan rule prevents a
/// per-type impl from cs-core. cs-gc's blanket `impl<T>
/// BreakCycle for RefCell<T>` covers them with the default
/// no-op. Real break dispatch for these would need either:
/// (a) Rust trait specialization (unstable), or (b) a
/// `Vector` newtype wrapper to take ownership of the inner
/// type. Tracked as a known limitation in the gap-closure
/// follow-on.
impl cs_gc::cycle::BreakCycle for Port {}
impl cs_gc::cycle::BreakCycle for Promise {}

/// Weak counterpart of [`Value`]'s heap-bearing variants, used by
/// the cycle-break tombstone machinery on [`Pair`]. Each variant
/// mirrors a `Value::*` heap variant with the underlying smart-
/// pointer downgraded.
///
/// Only present under `feature = "countable-memory"`; the tracing
/// path doesn't need this because the mark-sweep collector breaks
/// cycles via slot-zeroing rather than weak-pointer storage.
#[derive(Debug, Clone)]
pub enum WeakValue {
    String(cs_gc::Weak<RefCell<String>>),
    Pair(cs_gc::Weak<Pair>),
    Vector(cs_gc::Weak<RefCell<Vec<Value>>>),
    ByteVector(cs_gc::Weak<RefCell<Vec<u8>>>),
    Hashtable(cs_gc::Weak<Hashtable>),
    Port(cs_gc::Weak<Port>),
    Promise(cs_gc::Weak<Promise>),
    Procedure(std::rc::Weak<Box<dyn Procedure>>),
}

impl WeakValue {
    /// Construct a `WeakValue` from `v` if `v` carries a
    /// downgradeable heap pointer. Returns `None` for:
    ///
    /// - Leaf values (Null, Boolean, Fixnum, Character,
    ///   Symbol, Eof, Unspecified, immediate Numbers) —
    ///   those don't need weak storage because they have no
    ///   allocation to leak.
    /// - **Region-allocated heap values** (parallel-runtime
    ///   C5.1): `Gc::downgrade` on a region-backed `Gc<T>`
    ///   panics, because region cells have no defined
    ///   weak-ref semantics (the region's bulk drop is the
    ///   reclamation, not refcount → 0). The cycle-break
    ///   path used to silently produce a dead Weak; now it
    ///   skips the slot entirely (`break_car_cycle` /
    ///   `break_cdr_cycle` return `false`, leaving the
    ///   region value in place). Layer-5 escape analysis is
    ///   supposed to make this case unreachable, but the
    ///   guard here closes the manual `(cons rc-pair
    ///   region-pair)` escape hatch safely.
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::String(s) if !cs_gc::Gc::is_region(s) => {
                Some(WeakValue::String(cs_gc::Gc::downgrade(s)))
            }
            Value::Pair(p) if !cs_gc::Gc::is_region(p) => {
                Some(WeakValue::Pair(cs_gc::Gc::downgrade(p)))
            }
            Value::Vector(v) if !cs_gc::Gc::is_region(v) => {
                Some(WeakValue::Vector(cs_gc::Gc::downgrade(v)))
            }
            Value::ByteVector(b) if !cs_gc::Gc::is_region(b) => {
                Some(WeakValue::ByteVector(cs_gc::Gc::downgrade(b)))
            }
            Value::Hashtable(h) if !cs_gc::Gc::is_region(h) => {
                Some(WeakValue::Hashtable(cs_gc::Gc::downgrade(h)))
            }
            Value::Port(p) if !cs_gc::Gc::is_region(p) => {
                Some(WeakValue::Port(cs_gc::Gc::downgrade(p)))
            }
            Value::Promise(p) if !cs_gc::Gc::is_region(p) => {
                Some(WeakValue::Promise(cs_gc::Gc::downgrade(p)))
            }
            Value::Procedure(p) => Some(WeakValue::Procedure(Rc::downgrade(p))),
            // C5.1 guard: region-backed heap variants fall
            // through here. The cycle-break path skips them.
            Value::String(_)
            | Value::Pair(_)
            | Value::Vector(_)
            | Value::ByteVector(_)
            | Value::Hashtable(_)
            | Value::Port(_)
            | Value::Promise(_) => None,
            // Leaf values — no heap allocation to weaken.
            // `Value::Identifier` is leaf-shaped (Symbol + u64 mark),
            // no heap pointer to downgrade.
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::Number(_) => None,
        }
    }

    /// Attempt to upgrade this weak handle back to a strong
    /// `Value`. Returns `None` if the underlying allocation has
    /// been reclaimed (the cycle has been fully broken).
    pub fn upgrade(&self) -> Option<Value> {
        match self {
            WeakValue::String(w) => w.upgrade().map(Value::String),
            WeakValue::Pair(w) => w.upgrade().map(Value::Pair),
            WeakValue::Vector(w) => w.upgrade().map(Value::Vector),
            WeakValue::ByteVector(w) => w.upgrade().map(Value::ByteVector),
            WeakValue::Hashtable(w) => w.upgrade().map(Value::Hashtable),
            WeakValue::Port(w) => w.upgrade().map(Value::Port),
            WeakValue::Promise(w) => w.upgrade().map(Value::Promise),
            WeakValue::Procedure(w) => w.upgrade().map(Value::Procedure),
        }
    }
}

// ---- parallel-runtime spec C4.2: CycleChildren impls ----
//
// These are used by the Bacon-Rajan trial-deletion walk to
// enumerate direct heap-cell children for refcount adjustment.
// Distinct from CycleVisit, which dedups via a visitor and
// includes leaves. CycleChildren emits only heap addresses
// (Pair / Vector / Hashtable / Promise / Procedure /
// String / ByteVector / Port) — exactly what BR's mark_gray
// and scan_gray phases need to decrement/restore.
//
// Architecture: impls forward through `Value::cycle_children`
// which emits the heap_addr if any. Vector's storage
// (`RefCell<Vec<Value>>`) rides cs-gc's blanket impls for
// `RefCell<T: CycleChildren>` + `Vec<T: CycleChildren>` so
// there's no orphan-rule clash here.

#[cfg(feature = "tracing-cycle-collector")]
impl cs_gc::cycle_registry::CycleChildren for Value {
    fn cycle_children(&self, visit: &mut dyn FnMut(usize)) {
        if let Some(a) = self.heap_addr() {
            visit(a);
        }
    }
}

#[cfg(feature = "tracing-cycle-collector")]
impl cs_gc::cycle_registry::CycleChildren for Pair {
    fn cycle_children(&self, visit: &mut dyn FnMut(usize)) {
        // Read through the accessors so a previous
        // break_*_cycle's tombstoned weak still surfaces the
        // cyclic edge for BR's walk — matches CycleVisit.
        self.car().cycle_children(visit);
        self.cdr().cycle_children(visit);
    }
}

#[cfg(feature = "tracing-cycle-collector")]
impl cs_gc::cycle_registry::CycleChildren for Hashtable {
    fn cycle_children(&self, visit: &mut dyn FnMut(usize)) {
        for (k, v) in self.items.borrow().iter() {
            k.cycle_children(visit);
            v.cycle_children(visit);
        }
        if let Some(c) = &self.custom {
            c.hash.cycle_children(visit);
            c.equiv.cycle_children(visit);
        }
    }
}

#[cfg(feature = "tracing-cycle-collector")]
impl cs_gc::cycle_registry::CycleChildren for Promise {
    fn cycle_children(&self, visit: &mut dyn FnMut(usize)) {
        match &*self.state.borrow() {
            PromiseState::Pending(v) | PromiseState::Forced(v) => v.cycle_children(visit),
        }
    }
}

impl cs_gc::cycle::CycleVisit for Pair {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        // Walk via the accessors so the detector observes the
        // user-visible cyclic structure even after a previous
        // break_*_cycle has tombstoned the strong slot. The
        // upgraded weak still represents the cycle from the
        // user's perspective; if a new mutation reconstructs
        // the cycle, the detector should still fire.
        //
        // Iter 7.1.x.z investigated an in-walk back-edge demote
        // (when a Pair sees self.car/cdr targeting root, try
        // to demote that slot directly) but reclaimed nested
        // closure-env structures in deeply-recursive
        // metacircular workloads — even with the
        // strong-count guard, recursive go-closure traversal
        // through env_su found enough demote candidates to
        // make some env subtree unreachable mid-computation.
        // The robust fix requires reconstructing the cycle
        // path and applying full Bacon-Rajan trial deletion
        // (counting internal vs external edges per cycle
        // node, picking a safe-to-weaken edge). Deferred —
        // see ADR 0014 §"iter 7.1.x.z note". The iter
        // 7.1.x.y caller-supplied baseline at the root level
        // remains the in-tree safe demote path.
        let car = self.car();
        car.visit_children(ctx);
        if ctx.done() {
            return;
        }
        let cdr = self.cdr();
        cdr.visit_children(ctx);
    }
}

/// Hashtable equality kind. Real hashing comes later; foundation uses
/// linear search over a Vec — correctness first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtEqKind {
    /// `eq?` — pointer/identity equality.
    Eq,
    /// `eqv?` — value equality for numbers and characters, identity for heap.
    Eqv,
    /// `equal?` — structural equality.
    Equal,
    /// User-supplied (hash, equiv) procedures stored on the Hashtable.
    /// The procedures live in `Hashtable::custom`.
    Custom,
}

/// User-supplied hash and equivalence procedures attached to a hashtable
/// created with the 2-arg form of `(make-hashtable hash equiv)`. The
/// runtime calls these via the standard procedure-application path on
/// every set!/ref/contains?/delete!.
#[derive(Debug)]
pub struct CustomHashFns {
    pub hash: Value,
    pub equiv: Value,
}

/// R6RS hashtable.
#[derive(Debug)]
pub struct Hashtable {
    pub items: RefCell<Vec<(Value, Value)>>,
    pub eq_kind: HtEqKind,
    /// Populated only when `eq_kind == Custom`.
    pub custom: Option<CustomHashFns>,
}

impl Hashtable {
    pub fn new(eq_kind: HtEqKind) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Hashtable {
            items: RefCell::new(Vec::new()),
            eq_kind,
            custom: None,
        })
    }

    /// Construct a hashtable with user-supplied hash + equiv procedures.
    /// `eq_kind` is set to `Custom`; the runtime is responsible for
    /// dispatching the stored procs on every key comparison.
    pub fn new_custom(hash: Value, equiv: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Hashtable {
            items: RefCell::new(Vec::new()),
            eq_kind: HtEqKind::Custom,
            custom: Some(CustomHashFns { hash, equiv }),
        })
    }
}

impl cs_gc::cycle::CycleVisit for Hashtable {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        for (k, v) in self.items.borrow().iter() {
            if ctx.done() {
                return;
            }
            k.visit_children(ctx);
            if ctx.done() {
                return;
            }
            v.visit_children(ctx);
        }
        if let Some(c) = &self.custom {
            if ctx.done() {
                return;
            }
            c.hash.visit_children(ctx);
            if ctx.done() {
                return;
            }
            c.equiv.visit_children(ctx);
        }
    }
}

/// A port: foundation supports string-, bytevector-, and file-backed
/// ports. File output ports buffer in memory and flush on `close-port`.
/// File input is currently slurped as a string-input-port at open time
/// (see `b_open_input_file` in cs-runtime); a streaming file-input
/// variant lands in a later milestone.
#[derive(Debug)]
pub enum Port {
    StringInput(RefCell<StringInputState>),
    StringOutput(RefCell<String>),
    ByteVectorInput(RefCell<ByteVectorInputState>),
    ByteVectorOutput(RefCell<Vec<u8>>),
    /// File output port. `buf` accumulates writes; `close-port` writes
    /// the buffer to `path`. `closed` flips true on close so subsequent
    /// writes are rejected.
    FileOutput(RefCell<FileOutputState>),
}

#[derive(Debug, Clone)]
pub struct FileOutputState {
    pub path: String,
    pub buf: Vec<u8>,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct StringInputState {
    pub chars: Vec<char>,
    pub pos: usize,
}

#[derive(Debug, Clone)]
pub struct ByteVectorInputState {
    pub bytes: Vec<u8>,
    pub pos: usize,
}

impl Port {
    pub fn string_input(s: &str) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::StringInput(RefCell::new(StringInputState {
            chars: s.chars().collect(),
            pos: 0,
        })))
    }

    pub fn string_output() -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::StringOutput(RefCell::new(String::new())))
    }

    pub fn bytevector_input(bytes: Vec<u8>) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::ByteVectorInput(RefCell::new(ByteVectorInputState {
            bytes,
            pos: 0,
        })))
    }

    pub fn bytevector_output() -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::ByteVectorOutput(RefCell::new(Vec::new())))
    }

    pub fn file_output(path: String) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::FileOutput(RefCell::new(FileOutputState {
            path,
            buf: Vec::new(),
            closed: false,
        })))
    }

    pub fn is_input(&self) -> bool {
        matches!(self, Port::StringInput(_) | Port::ByteVectorInput(_))
    }

    pub fn is_output(&self) -> bool {
        matches!(
            self,
            Port::StringOutput(_) | Port::ByteVectorOutput(_) | Port::FileOutput(_)
        )
    }

    pub fn is_textual(&self) -> bool {
        // Files can carry either, but the typical R6RS use of file output
        // is textual via `display`/`write`. Classify as textual; binary
        // file ports would be a separate variant.
        matches!(
            self,
            Port::StringInput(_) | Port::StringOutput(_) | Port::FileOutput(_)
        )
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Port::ByteVectorInput(_) | Port::ByteVectorOutput(_))
    }
}

impl cs_gc::cycle::CycleVisit for Port {
    fn visit_children(&self, _ctx: &mut cs_gc::cycle::CycleVisitor) {
        // Leaf: Port variants hold no Gc<...> children.
    }
}

/// A R5RS/R6RS promise. Memoized lazy value.
#[derive(Debug)]
pub struct Promise {
    pub state: RefCell<PromiseState>,
}

#[derive(Debug)]
pub enum PromiseState {
    /// Holding the un-forced thunk procedure.
    Pending(Value),
    /// Holding the memoized result.
    Forced(Value),
}

impl Promise {
    pub fn pending(thunk: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Promise {
            state: RefCell::new(PromiseState::Pending(thunk)),
        })
    }
}

impl cs_gc::cycle::CycleVisit for Promise {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        match &*self.state.borrow() {
            PromiseState::Pending(v) | PromiseState::Forced(v) => v.visit_children(ctx),
        }
    }
}

/// Type-erased procedure dispatch. Concrete builtin and closure types live in
/// `cs-runtime`; eval downcasts via [`as_any`].
///
/// Under the default (tracing) representation, Procedure has
/// `cs_gc::Trace` as a supertrait so closure environments and parameter
/// cells participate in mark-sweep tracing. Under
/// `feature = "countable-memory"` the supertrait is gone — reclamation
/// runs by Rc refcount alone — and Procedure gains an optional
/// `visit_closure_children` method that the synchronous cycle
/// collector consults when walking a Value::Procedure. Most builtins
/// hold no Scheme heap children and inherit the empty default impl;
/// only closures and Parameter override it.

pub trait Procedure: fmt::Debug + 'static {
    fn as_any(&self) -> &dyn Any;
    fn name(&self) -> Option<&str> {
        None
    }
    /// Cheap discriminant for the concrete `Procedure` impl, so call
    /// sites (cs-vm's dispatch loop) can `match` once instead of
    /// running a sequential chain of `downcast_ref` probes (each an
    /// independent `TypeId` compare). The downcast inside the winning
    /// match arm is still required to get at the concrete type's
    /// fields — `kind()` only picks which single downcast to do.
    /// Defaults to `Other` for exotic/test-only impls that no
    /// dispatch site needs to distinguish.
    fn kind(&self) -> ProcKind {
        ProcKind::Other
    }
    /// Visit every `Gc<...>` child this procedure holds in its
    /// closure environment. Default impl is empty — appropriate
    /// for builtins and zero-payload procedure markers. Closures,
    /// continuations, and Parameter override it.
    fn visit_closure_children(&self, _ctx: &mut cs_gc::cycle::CycleVisitor) {}
}

/// Discriminant for every concrete `Procedure` impl that a dispatch
/// site actually distinguishes. See [`Procedure::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProcKind {
    /// Anything without a dedicated variant (test-only impls, future
    /// additions not yet wired into a `match`). `downcast_ref` chains
    /// remain the fallback for these.
    Other,
    Parameter,
    // cs-runtime tree-walker procedure types.
    Builtin,
    Closure,
    Continuation,
    HostBuiltin,
    // cs-vm bytecode-tier procedure types.
    VmClosure,
    VmBuiltin,
    VmBuiltinSyms,
    VmHostBuiltin,
    VmApply,
    VmMap,
    VmForEach,
    VmFilter,
    VmFind,
    VmAny,
    VmEvery,
    VmFoldLeft,
    VmFoldRight,
    VmReduce,
    VmCount,
    VmPartition,
    VmValues,
    VmCallWithValues,
    VmVectorMap,
    VmVectorForEach,
    VmVectorFold,
    VmVectorFilter,
    VmStringMap,
    VmStringForEach,
    VmHashtableWalk,
    VmHashtableForEach,
    VmHashtableFold,
    VmHashtableUpdate,
    VmUnfold,
    VmListSort,
    VmVectorSort,
    VmVectorSortBang,
    VmTabulate,
    VmRemove,
    VmForce,
    VmEval,
    VmDisplay,
    VmWrite,
    VmNewline,
    VmWithOutputToString,
    VmWithInputFromString,
    VmWithOutputToFile,
    VmWithInputFromFile,
    VmCurrentInputPort,
    VmCurrentOutputPort,
    VmRaise,
    VmErrorFn,
    VmAssertionViolation,
    VmWithExceptionHandler,
    VmCallCc,
    VmDynamicWind,
    VmCurrentContinuationMarks,
    VmContinuation,
    VmAotClosure,
}

/// A dynamic parameter procedure (R6RS `make-parameter`). Lives in cs-core
/// so both the tree-walker and the VM can dispatch a single concrete type.
/// Calling with 0 args reads `cell`; with 1 arg writes it.
#[derive(Debug)]
pub struct Parameter {
    pub cell: RefCell<Value>,
}

impl Procedure for Parameter {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("parameter")
    }
    fn kind(&self) -> ProcKind {
        ProcKind::Parameter
    }
    fn visit_closure_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        self.cell.borrow().visit_children(ctx);
    }
}

impl cs_gc::cycle::CycleVisit for Parameter {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        self.cell.borrow().visit_children(ctx);
    }
}

pub fn make_parameter(initial: Value) -> Value {
    let p: Rc<Box<dyn Procedure>> = Rc::new(Box::new(Parameter {
        cell: RefCell::new(initial),
    }));
    Value::Procedure(p)
}

/// The universal Scheme value.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Unspecified,
    Eof,
    Boolean(bool),
    Character(char),
    Number(Number),
    String(crate::Gc<RefCell<String>>),
    Symbol(Symbol),
    /// Hygienic identifier: a name plus a per-macro-call mark.
    /// Distinct from `Symbol` in R6RS terms (an identifier is a
    /// syntax object wrapping a name with lexical context).
    ///
    /// * `mark == 0` is the "unmarked" identifier produced by
    ///   `datum->syntax` when given a non-marked context, or by
    ///   reader input that's been wrapped into a syntax object
    ///   without flowing through a macro expansion.
    /// * Each `syntax-case` template instantiation stamps the
    ///   non-pvar identifiers in the template with a fresh
    ///   mark unique to that expansion site; this is the
    ///   mechanism that lets `bound-identifier=?` distinguish
    ///   two `(mark-a x)` / `(mark-b x)` invocations.
    ///
    /// `Symbol` and `Identifier` interoperate at most
    /// predicates (`identifier?` accepts either; `eq?` returns
    /// false across the kinds even when the symbol/identifier
    /// share a name). The migration that introduced this
    /// variant is tracked in the R6RS++ plan as Phase 1.5.
    Identifier {
        name: Symbol,
        mark: u64,
    },
    Pair(crate::Gc<Pair>),
    Vector(crate::Gc<RefCell<Vec<Value>>>),
    ByteVector(crate::Gc<RefCell<Vec<u8>>>),
    Procedure(Rc<Box<dyn Procedure>>),
    Hashtable(crate::Gc<Hashtable>),
    Port(crate::Gc<Port>),
    Promise(crate::Gc<Promise>),
}

/// Format mode for [`Value::write_to`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteMode {
    /// R6RS `write`: read-back-able, strings quoted, characters as `#\\x`.
    Write,
    /// R6RS `display`: human-friendly, strings unquoted, raw characters.
    Display,
}

/// `Trace` impl for Value enumerates the heap-pointer variants so the
/// GC can reach every transitively-reachable allocation during a mark
/// pass.
///
/// All standard heap-data variants (Pair / Vector / String / ByteVector
/// / Hashtable / Port / Promise) are `Gc<T>`-backed and trace through.
/// `Procedure` is the one exception: it stays on `Rc<dyn Procedure>`
/// in Phase 1 because `Gc<dyn Procedure>` requires the unstable
/// `CoerceUnsized` trait. Closure environments and parameter cells
/// still participate in tracing because the concrete `Procedure` impls
/// in cs-runtime / cs-vm provide non-trivial `Trace` impls; we just
/// don't reach them through this entry — they're reachable via the
/// walker top frame and the VM root env, both of which are root closures.
/// True cycles that go *through* `Rc<dyn Procedure>` would leak in
/// Phase 1; the M5 spec's exit gate calls this out explicitly.

/// Under countable-memory, every heap-bearing Value variant
/// (including Procedure) participates in cycle detection. Pair
/// /Vector/etc. forward through the cs_gc blanket impl for Gc<T>;
/// Procedure forwards through the trait's `visit_closure_children`
/// hook so concrete closures and parameters can enumerate their
/// captured values without each Value match-arm knowing the
/// concrete type.
impl cs_gc::cycle::CycleVisit for Value {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        match self {
            Value::String(s) => {
                if ctx.visit(s) {
                    s.visit_children(ctx);
                }
            }
            Value::ByteVector(v) => {
                if ctx.visit(v) {
                    v.visit_children(ctx);
                }
            }
            Value::Vector(v) => {
                if ctx.visit(v) {
                    v.visit_children(ctx);
                }
            }
            Value::Pair(p) => {
                if ctx.visit(p) {
                    p.visit_children(ctx);
                }
            }
            Value::Hashtable(h) => {
                if ctx.visit(h) {
                    h.visit_children(ctx);
                }
            }
            Value::Port(p) => {
                if ctx.visit(p) {
                    p.visit_children(ctx);
                }
            }
            Value::Promise(p) => {
                if ctx.visit(p) {
                    p.visit_children(ctx);
                }
            }
            Value::Procedure(p) => {
                // Procedure is Rc<dyn Procedure>; we don't have a
                // Gc<...> to register identity for. Forward to the
                // trait's closure-child hook so concrete impls
                // enumerate their captured Gc<...> children.
                p.visit_closure_children(ctx);
            }
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::Number(_) => {}
        }
    }
}

/// `true` if `v` provably cannot hold an outgoing reference to
/// another `Value` — i.e. its `CycleVisit::visit_children` is a
/// no-op no matter what it contains.
///
/// A mutation (`set-car!`, `vector-set!`, ...) can only *close* a
/// cycle through the edge it just wrote: the new target must have
/// some outgoing path back to the mutated root. If the newly
/// stored value has no outgoing `Value` edges at all, no such path
/// exists, so callers can skip `cs_gc::cycle::check_and_break`
/// entirely for that store.
///
/// `String` and `ByteVector` are `Gc`-backed but hold only raw
/// bytes/chars, not further `Value`s, so they qualify. `Pair` /
/// `Vector` / `Hashtable` / `Promise` / `Procedure` can all hold
/// arbitrary `Value` children (directly or via a captured
/// environment) and must still go through the full check.
pub fn value_is_acyclic_leaf(v: &Value) -> bool {
    matches!(
        v,
        Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Number(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::String(_)
            | Value::ByteVector(_)
            | Value::Port(_)
    )
}

impl Value {
    pub fn fixnum(v: i64) -> Self {
        Value::Number(Number::Fixnum(v))
    }

    pub fn flonum(v: f64) -> Self {
        Value::Number(Number::Flonum(v))
    }

    pub fn string(s: impl Into<String>) -> Self {
        Value::String(crate::Gc::new(RefCell::new(s.into())))
    }

    pub fn list(items: impl IntoIterator<Item = Value>) -> Self {
        let mut v: Vec<Value> = items.into_iter().collect();
        let mut acc = Value::Null;
        while let Some(item) = v.pop() {
            acc = Value::Pair(Pair::new(item, acc));
        }
        acc
    }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Boolean(false))
    }

    /// Allocation address for this Value's underlying heap
    /// allocation, if any. Returns `None` for leaf values
    /// (no allocation, no address).
    ///
    /// Used by the parallel-runtime C4.2 `CycleChildren` walk
    /// to enumerate child container addresses for Bacon-Rajan
    /// trial deletion. Mirrors the receiver shape of
    /// [`heap_strong_count`].
    #[cfg(feature = "tracing-cycle-collector")]
    pub fn heap_addr(&self) -> Option<usize> {
        match self {
            Value::String(g) => Some(cs_gc::Gc::as_addr(g)),
            Value::Pair(g) => Some(cs_gc::Gc::as_addr(g)),
            Value::Vector(g) => Some(cs_gc::Gc::as_addr(g)),
            Value::ByteVector(g) => Some(cs_gc::Gc::as_addr(g)),
            Value::Hashtable(g) => Some(cs_gc::Gc::as_addr(g)),
            Value::Port(g) => Some(cs_gc::Gc::as_addr(g)),
            Value::Promise(g) => Some(cs_gc::Gc::as_addr(g)),
            // Procedure is std::rc::Rc, not cs_gc::Gc — exposes
            // its addr via Rc::as_ptr cast.
            Value::Procedure(rc) => Some(std::rc::Rc::as_ptr(rc) as *const () as usize),
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::Number(_) => None,
        }
    }

    /// Total strong-reference count for this Value's underlying
    /// heap allocation, if any. Returns `None` for leaf values
    /// (no allocation to count). Used by `Pair::break_*_cycle`'s
    /// strong-count guard.
    pub fn heap_strong_count(&self) -> Option<usize> {
        match self {
            Value::String(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::Pair(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::Vector(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::ByteVector(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::Hashtable(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::Port(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::Promise(g) => Some(cs_gc::Gc::strong_count(g)),
            Value::Procedure(rc) => Some(Rc::strong_count(rc)),
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::Number(_) => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Unspecified => "unspecified",
            Value::Eof => "eof",
            Value::Boolean(_) => "boolean",
            Value::Character(_) => "character",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Identifier { .. } => "identifier",
            Value::Pair(_) => "pair",
            Value::Vector(_) => "vector",
            Value::ByteVector(_) => "bytevector",
            Value::Procedure(_) => "procedure",
            Value::Hashtable(_) => "hashtable",
            Value::Port(_) => "port",
            Value::Promise(_) => "promise",
        }
    }

    /// Write this value to `out` using `syms` to resolve symbol names.
    /// Use [`format_with`] for a string-returning convenience.
    pub fn write_to(
        &self,
        out: &mut dyn fmt::Write,
        syms: &SymbolTable,
        mode: WriteMode,
    ) -> fmt::Result {
        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
        self.write_to_visited(out, syms, mode, &mut visited)
    }

    fn write_to_visited(
        &self,
        out: &mut dyn fmt::Write,
        syms: &SymbolTable,
        mode: WriteMode,
        visited: &mut std::collections::HashSet<usize>,
    ) -> fmt::Result {
        match self {
            Value::Null => write!(out, "()"),
            Value::Unspecified => write!(out, "#<unspecified>"),
            Value::Eof => write!(out, "#<eof>"),
            Value::Boolean(true) => write!(out, "#t"),
            Value::Boolean(false) => write!(out, "#f"),
            Value::Character(c) => match mode {
                WriteMode::Write => match c {
                    ' ' => write!(out, "#\\space"),
                    '\n' => write!(out, "#\\newline"),
                    '\t' => write!(out, "#\\tab"),
                    '\r' => write!(out, "#\\return"),
                    '\0' => write!(out, "#\\nul"),
                    c => write!(out, "#\\{}", c),
                },
                WriteMode::Display => write!(out, "{}", c),
            },
            Value::Number(n) => write!(out, "{}", n),
            Value::String(s) => match mode {
                WriteMode::Write => write!(out, "\"{}\"", escape_string(&s.borrow())),
                WriteMode::Display => write!(out, "{}", s.borrow()),
            },
            Value::Symbol(s) => write!(out, "{}", syms.name(*s)),
            // Identifier renders identically to a symbol with
            // the same name in normal write/display output;
            // the mark is observable only via R6RS hygiene
            // predicates (`bound-identifier=?`).
            Value::Identifier { name, .. } => write!(out, "{}", syms.name(*name)),
            Value::Pair(p) => write_pair(out, p, syms, mode, visited),
            Value::Vector(v) => {
                let ptr = crate::Gc::as_addr(v);
                if !visited.insert(ptr) {
                    return write!(out, "#(...)");
                }
                let res = (|| -> fmt::Result {
                    write!(out, "#(")?;
                    let inner = v.borrow();
                    for (i, item) in inner.iter().enumerate() {
                        if i > 0 {
                            write!(out, " ")?;
                        }
                        item.write_to_visited(out, syms, mode, visited)?;
                    }
                    write!(out, ")")
                })();
                visited.remove(&ptr);
                res
            }
            Value::Procedure(p) => match p.name() {
                Some(n) => write!(out, "#<procedure {}>", n),
                None => write!(out, "#<procedure>"),
            },
            Value::ByteVector(bv) => {
                write!(out, "#vu8(")?;
                let bv = bv.borrow();
                for (i, b) in bv.iter().enumerate() {
                    if i > 0 {
                        write!(out, " ")?;
                    }
                    write!(out, "{}", b)?;
                }
                write!(out, ")")
            }
            Value::Hashtable(h) => write!(out, "#<hashtable size={}>", h.items.borrow().len()),
            Value::Port(p) => match &**p {
                Port::StringInput(_) => write!(out, "#<input-port>"),
                Port::StringOutput(_) => write!(out, "#<output-port>"),
                Port::ByteVectorInput(_) => write!(out, "#<binary-input-port>"),
                Port::ByteVectorOutput(_) => write!(out, "#<binary-output-port>"),
                Port::FileOutput(s) => {
                    write!(out, "#<file-output-port {:?}>", s.borrow().path)
                }
            },
            Value::Promise(_) => write!(out, "#<promise>"),
        }
    }

    /// Convenience: format using a SymbolTable.
    pub fn format_with(&self, syms: &SymbolTable, mode: WriteMode) -> String {
        let mut s = String::new();
        let _ = self.write_to(&mut s, syms, mode);
        s
    }
}

fn write_pair(
    out: &mut dyn fmt::Write,
    p: &Pair,
    syms: &SymbolTable,
    mode: WriteMode,
    visited: &mut std::collections::HashSet<usize>,
) -> fmt::Result {
    let head_ptr = p as *const Pair as usize;
    if !visited.insert(head_ptr) {
        return write!(out, "(...)");
    }
    let result = write_pair_inner(out, p, syms, mode, visited);
    visited.remove(&head_ptr);
    result
}

fn write_pair_inner(
    out: &mut dyn fmt::Write,
    p: &Pair,
    syms: &SymbolTable,
    mode: WriteMode,
    visited: &mut std::collections::HashSet<usize>,
) -> fmt::Result {
    write!(out, "(")?;
    let mut first = true;
    let mut cur_car = p.car();
    let mut cur_cdr = p.cdr();
    loop {
        if !first {
            write!(out, " ")?;
        }
        first = false;
        cur_car.write_to_visited(out, syms, mode, visited)?;
        match cur_cdr {
            Value::Null => break,
            Value::Pair(next) => {
                let next_ptr = crate::Gc::as_addr(&next);
                if !visited.insert(next_ptr) {
                    write!(out, " . #<cycle>")?;
                    break;
                }
                cur_car = next.car();
                cur_cdr = next.cdr();
            }
            other => {
                write!(out, " . ")?;
                other.write_to_visited(out, syms, mode, visited)?;
                break;
            }
        }
    }
    write!(out, ")")
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Default Display: opaque renderings for symbols (`#<symbol#N>`) when no
/// SymbolTable is available. Use [`Value::format_with`] for full output.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "()"),
            Value::Unspecified => write!(f, "#<unspecified>"),
            Value::Eof => write!(f, "#<eof>"),
            Value::Boolean(true) => write!(f, "#t"),
            Value::Boolean(false) => write!(f, "#f"),
            Value::Character(c) => write!(f, "#\\{}", c),
            Value::Number(n) => write!(f, "{}", n),
            Value::String(s) => write!(f, "\"{}\"", s.borrow()),
            Value::Symbol(s) => write!(f, "#<symbol#{}>", s.0),
            // Debug-style Display for Identifier surfaces the
            // mark so it's visible in debug dumps / Rust-side
            // panics; user-facing write/display in
            // `write_to_visited` hides it.
            Value::Identifier { name, mark } => {
                write!(f, "#<identifier#{}:{}>", name.0, mark)
            }
            Value::Pair(p) => display_pair(f, p),
            Value::Vector(v) => {
                write!(f, "#(")?;
                let v = v.borrow();
                for (i, item) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, ")")
            }
            Value::Procedure(p) => match p.name() {
                Some(n) => write!(f, "#<procedure {}>", n),
                None => write!(f, "#<procedure>"),
            },
            Value::ByteVector(bv) => {
                write!(f, "#vu8(")?;
                let bv = bv.borrow();
                for (i, b) in bv.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", b)?;
                }
                write!(f, ")")
            }
            Value::Hashtable(h) => write!(f, "#<hashtable size={}>", h.items.borrow().len()),
            Value::Port(p) => match &**p {
                Port::StringInput(_) => write!(f, "#<input-port>"),
                Port::StringOutput(_) => write!(f, "#<output-port>"),
                Port::ByteVectorInput(_) => write!(f, "#<binary-input-port>"),
                Port::ByteVectorOutput(_) => write!(f, "#<binary-output-port>"),
                Port::FileOutput(s) => {
                    write!(f, "#<file-output-port {:?}>", s.borrow().path)
                }
            },
            Value::Promise(_) => write!(f, "#<promise>"),
        }
    }
}

#[cfg(test)]
mod pair_diet_tests {
    use super::*;

    /// cs-5te: `Pair` shrank from 144B (car+cdr RefCells, an inline
    /// `Cell<Option<Span>>` source slot, and two inline
    /// `RefCell<Option<WeakValue>>` tombstone slots) down to just
    /// `car`, `cdr`, and a 1-byte flags `Cell<u8>` (rounded up by
    /// alignment). Span + tombstones now live in thread-local side
    /// tables keyed by address. With the +16B `Rc` header this is a
    /// 160B -> 88B cons cell: `car`(16) + `cdr`(16) + `flags`(1,
    /// padded to 8 for `Value`'s 8-byte alignment) = 40B measured
    /// (72B on this build once `Value`'s own layout is counted in —
    /// see the assertion below), well under the 80B target.
    #[test]
    fn pair_is_diet_sized() {
        let before = 144usize;
        let after = std::mem::size_of::<Pair>();
        assert_eq!(
            after, 72,
            "Pair size changed: {after}B (was {before}B before cs-5te, cs-5te landed it at 72B)"
        );
        assert!(
            after <= 80,
            "Pair grew past the cs-5te <= 80B target: {after}B"
        );
    }

    #[test]
    fn plain_cons_has_no_span_or_tombstone() {
        let p = Pair::new(Value::Number(Number::Fixnum(1)), Value::Null);
        assert_eq!(p.source_span(), None);
        assert_eq!(p.flags.get(), 0);
    }

    #[test]
    fn with_source_round_trips_span() {
        let span = cs_diag::Span {
            file: cs_diag::FileId(3),
            start: 10,
            end: 20,
        };
        let p = Pair::with_source(Value::Null, Value::Null, span);
        assert_eq!(p.source_span(), Some(span));
        assert_eq!(p.flags.get() & HAS_SPAN, HAS_SPAN);
    }

    #[test]
    fn dropped_pair_clears_its_span_table_entry() {
        let span = cs_diag::Span {
            file: cs_diag::FileId(1),
            start: 0,
            end: 1,
        };
        let addr = {
            let p = Pair::with_source(Value::Null, Value::Null, span);
            cs_gc::Gc::as_addr(&p)
        };
        // The pair is gone; its address must not resurrect a span
        // for a would-be new pair placed there.
        let leaked = PAIR_SPANS.with(|t| t.borrow().get(&addr).copied());
        assert_eq!(leaked, None);
    }

    #[test]
    fn tombstone_round_trip_and_clear_on_set() {
        // Build a two-cycle so break_car_cycle's strong-count guard
        // (baseline) is satisfied: p's car points at itself via an
        // intermediate strong ref that we then drop, leaving one
        // EXTERNAL anchor (`p` itself) — mirrors the real cycle-
        // detector's usage pattern.
        let p = Pair::new(Value::Null, Value::Null);
        p.set_car(Value::Pair(p.clone()));
        // Strong refs to the car's target (`p`) right now: the
        // `p` binding itself + the car slot = 2. baseline=1 leaves
        // one external anchor (the `p` binding), so the demote
        // should fire.
        assert!(p.break_car_cycle(1));
        assert_eq!(p.flags.get() & HAS_CAR_TOMBSTONE, HAS_CAR_TOMBSTONE);
        // Tombstone upgrade returns the cyclic value transparently.
        assert!(matches!(p.car(), Value::Pair(_)));
        // set_car! over the demoted slot clears the tombstone.
        p.set_car(Value::Null);
        assert_eq!(p.flags.get() & HAS_CAR_TOMBSTONE, 0);
        let addr = p.addr();
        let leaked = PAIR_CAR_TOMBSTONES.with(|t| t.borrow().contains_key(&addr));
        assert!(!leaked, "set_car! must clear the tombstone table entry");
    }
}

#[cfg(test)]
mod procedure_pointer_tests {
    /// cs-7kg: `Rc<dyn Procedure>` is a 16B fat pointer (data + vtable),
    /// which denies `Value::Procedure` a niche for its discriminant and
    /// pins `Value` at 24B. `Box<dyn Procedure>` is `Sized`, so wrapping
    /// it in `Rc<Box<dyn Procedure>>` moves the vtable into the `RcBox`
    /// heap payload and leaves the pointer itself thin (8B). `Value`
    /// stays 24B until the follow-up flattening (cs-7xg stage 2, deferred
    /// until this pointer is thin).
    #[test]
    fn procedure_rc_is_thin() {
        assert_eq!(
            std::mem::size_of::<std::rc::Rc<Box<dyn super::Procedure>>>(),
            8,
            "Rc<Box<dyn Procedure>> must be a thin 8B pointer"
        );
    }
}

fn display_pair(f: &mut fmt::Formatter<'_>, p: &Pair) -> fmt::Result {
    write!(f, "(")?;
    let mut first = true;
    let mut cur_car = p.car();
    let mut cur_cdr = p.cdr();
    loop {
        if !first {
            write!(f, " ")?;
        }
        first = false;
        write!(f, "{}", cur_car)?;
        match cur_cdr {
            Value::Null => break,
            Value::Pair(next) => {
                cur_car = next.car();
                cur_cdr = next.cdr();
            }
            other => {
                write!(f, " . {}", other)?;
                break;
            }
        }
    }
    write!(f, ")")
}
