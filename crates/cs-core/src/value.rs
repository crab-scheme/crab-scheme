//! The universal Scheme value type.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Seek, Write};
use std::rc::Rc;

use crate::nanbox::{
    decode_gc_handle_for_drop, nb_clone_owned, nb_decode_gc_ptr, nb_drop_owned, nb_is_tagged,
    nb_owning_payload_is_live, nb_payload_of, nb_tag_of, NanboxValue, NB_TAG_PAIR,
};
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
/// PR2 (cs-vnf.3): `car`/`cdr` are raw NaN-boxed payloads
/// (`Cell<u64>`), not `RefCell<Value>` — a `Value` is a 16-byte
/// tagged Rust enum with no stable/`repr`-fixed layout, so this
/// slot instead stores whatever i64 the JIT's uniform-NB tier
/// already produces/consumes, eliminating the Rust-enum <-> NB
/// transcode on every JIT car/cdr access (cs-vnf.3/cs-vnf.4). Both
/// fields are private and OWNING (each holds one strong
/// incref/decref obligation, exactly like the `Value` they used to
/// hold): read via `car()`/`cdr()` (clone-out, tombstone-upgrading),
/// write via `set_car()`/`set_cdr()`. Direct field access is no
/// longer possible from outside this file — all external call sites
/// were migrated to the accessors in PR1.
///
/// The cycle-break/tombstone machinery (`break_car_cycle`,
/// `break_cdr_cycle`, `Drop`) reads/writes the RAW slot via the
/// private `peek_car`/`peek_cdr`/`set_car_raw_owned`/
/// `set_cdr_raw_owned` helpers below, which — unlike `car()`/
/// `cdr()` — do NOT upgrade a tombstoned slot. Routing that
/// machinery through the upgrading public accessors would be
/// circular (it implements the tombstone mechanism, not a consumer
/// of it) and would silently make `break_*_cycle` inspect the
/// *pre-break* cyclic target's refcount instead of the slot it's
/// actually about to demote.
pub struct Pair {
    car: Cell<u64>,
    cdr: Cell<u64>,
    /// Cold-metadata presence bits: see [`HAS_SPAN`],
    /// [`HAS_CAR_TOMBSTONE`], [`HAS_CDR_TOMBSTONE`]. `Cell` so the
    /// post-construction span/tombstone setters don't need
    /// `&mut Pair`.
    flags: Cell<u8>,
}

impl fmt::Debug for Pair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Decode via the public (tombstone-upgrading) accessors —
        // Debug is a user-facing view, same shape a `RefCell<Value>`
        // derive would have produced pre-PR2.
        f.debug_struct("Pair")
            .field("car", &self.car())
            .field("cdr", &self.cdr())
            .finish()
    }
}

/// Encode an owned `Value` into the raw owning NB bit-pattern a
/// `Pair` slot stores.
#[inline]
fn encode_owned(v: Value) -> u64 {
    NanboxValue::from_value(v).into_raw() as u64
}

impl Pair {
    /// This pair's out-of-line-table key: the same address identity
    /// `Gc::as_addr` computes (`Rc::as_ptr`), obtained directly from
    /// `&self` since `Gc<Pair>` derefs straight to it.
    fn addr(&self) -> usize {
        self as *const Pair as usize
    }

    /// Non-upgrading, non-consuming decode of whatever NB payload is
    /// ACTUALLY stored in the car slot right now — bypasses tombstone
    /// upgrade. Only for the cycle-break/Drop machinery; everyone
    /// else must use [`car`](Self::car).
    fn peek_car(&self) -> Value {
        let raw = self.car.get() as i64;
        // A dead-region payload can reach here via the layer-4
        // cycle sweep, which can run mid-Drop-chain (triggered by
        // an unrelated Pair's own Drop; see the note in `Drop for
        // Pair`) — well after this pair's owning region already
        // bulk-freed. There's nothing live left to clone out;
        // treat it the same as a tombstoned slot (`Unspecified`)
        // rather than reconstructing a handle into freed arena
        // memory. SAFETY: `nb_owning_payload_is_live` doesn't
        // consume/mutate the slot, so this check is side-effect-free.
        if !unsafe { nb_owning_payload_is_live(raw) } {
            return Value::Unspecified;
        }
        // SAFETY: the slot holds a live owning NB payload (or inline
        // immediate) for as long as `self` is alive. `nb_clone_owned`
        // bumps its refcount (if any), so the returned `Value` is an
        // independent additional owner and the slot's own ownership
        // is untouched.
        unsafe { NanboxValue(nb_clone_owned(raw)).to_value() }
    }

    /// See [`peek_car`](Self::peek_car).
    fn peek_cdr(&self) -> Value {
        let raw = self.cdr.get() as i64;
        if !unsafe { nb_owning_payload_is_live(raw) } {
            return Value::Unspecified;
        }
        unsafe { NanboxValue(nb_clone_owned(raw)).to_value() }
    }

    /// Overwrite the car slot with an already-owned NB payload
    /// (`bits`), dropping whatever owning payload was there before.
    fn set_car_raw_owned(&self, bits: u64) {
        let old = self.car.replace(bits);
        // SAFETY: `old` was this slot's own owning payload; we hold
        // the only reference to it via `self.car` and have just
        // replaced that reference, so this is exactly one drop.
        unsafe { nb_drop_owned(old as i64) };
    }

    /// See [`set_car_raw_owned`](Self::set_car_raw_owned).
    fn set_cdr_raw_owned(&self, bits: u64) {
        let old = self.cdr.replace(bits);
        unsafe { nb_drop_owned(old as i64) };
    }

    pub fn new(car: Value, cdr: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Pair {
            car: Cell::new(encode_owned(car)),
            cdr: Cell::new(encode_owned(cdr)),
            flags: Cell::new(0),
        })
    }

    /// Construct a pair directly from two already-owned raw NB
    /// payloads, skipping the `Value` decode/encode round trip that
    /// [`new`](Self::new) pays via `encode_owned`. `car_bits`/
    /// `cdr_bits` must each be a live owning NB payload (or inline
    /// immediate) that the caller is transferring — exactly the same
    /// single-consumer contract as `new`'s `Value` arguments, just
    /// pre-encoded. cs-vnf.4: the uniform-NB JIT tier already holds
    /// both `cons` operands as raw NB i64s in registers, so this
    /// lets `Inst::Cons` skip `NanboxValue::to_value`/`from_value`
    /// entirely instead of decoding-then-re-encoding through `Value`.
    ///
    /// No tombstone/span bookkeeping applies to a freshly allocated
    /// pair (`flags` starts at 0, same as `new`).
    pub fn new_raw_nb(car_bits: u64, cdr_bits: u64) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Pair {
            car: Cell::new(car_bits),
            cdr: Cell::new(cdr_bits),
            flags: Cell::new(0),
        })
    }

    /// Construct a pair tagged with its reader-produced source span.
    pub fn with_source(car: Value, cdr: Value, span: cs_diag::Span) -> cs_gc::Gc<Self> {
        let gc = cs_gc::Gc::new(Pair {
            car: Cell::new(encode_owned(car)),
            cdr: Cell::new(encode_owned(cdr)),
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
                car: Cell::new(encode_owned(car)),
                cdr: Cell::new(encode_owned(cdr)),
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
        // Judge fixup (cs-vnf.3): `peek_car` is shared with the
        // cycle-break/Drop machinery, where a dead-region payload is
        // expected traffic and must degrade silently to
        // `Unspecified` (see `peek_car`'s own doc). But THIS is the
        // public, ordinary-read entry point — a dead-region payload
        // reaching a live `car()` call is a genuine
        // use-after-region-drop bug in the CALLER (layer-5 escape
        // analysis failed to prevent it), not expected traffic, and
        // silently returning `Unspecified` would mask it. Debug-only
        // (compiles to nothing in release, matching every other
        // panic-on-UAF check in `cs_gc`, e.g. `assert_region_live`):
        // keep it loud here while `peek_car` itself stays graceful.
        debug_assert!(
            unsafe { nb_owning_payload_is_live(self.car.get() as i64) },
            "Pair::car: use-after-region-drop — the car slot's owning \
             region has already dropped. Release builds silently return \
             Unspecified (via peek_car); this debug_assert exists so a \
             genuine live-code UAF through the public accessor stays loud \
             instead of being masked by peek_car's Drop-context tolerance."
        );
        self.peek_car()
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
        // See the matching debug_assert in `car()` for why this is
        // here and not in `peek_cdr` itself.
        debug_assert!(
            unsafe { nb_owning_payload_is_live(self.cdr.get() as i64) },
            "Pair::cdr: use-after-region-drop — the cdr slot's owning \
             region has already dropped. Release builds silently return \
             Unspecified (via peek_cdr); this debug_assert exists so a \
             genuine live-code UAF through the public accessor stays loud \
             instead of being masked by peek_cdr's Drop-context tolerance."
        );
        self.peek_cdr()
    }

    /// NB-native counterpart to [`car`](Self::car): identical
    /// tombstone/liveness semantics, but returns the raw owning NB
    /// bit pattern directly instead of decoding through `Value` —
    /// skips `NanboxValue::to_value`/`from_value` entirely. cs-vnf.4:
    /// the uniform-NB JIT tier already works in raw NB space, so
    /// this is the fast path `Inst::Car`'s lowering calls into.
    ///
    /// The untombstoned case is a single `Cell::get` (flags) +
    /// `nb_clone_owned` (refcount bump only, no `Value` construction
    /// at all). The tombstoned case still upgrades the weak entry to
    /// a `Value` (unavoidable — the tombstone table stores
    /// `WeakValue`, not raw NB bits) and re-encodes it; this is the
    /// rare path (cycle-detector-demoted slots only), so paying the
    /// encode there is fine.
    pub fn car_raw_nb(&self) -> u64 {
        if self.flags.get() & HAS_CAR_TOMBSTONE != 0 {
            let addr = self.addr();
            let upgraded = PAIR_CAR_TOMBSTONES
                .try_with(|t| t.borrow().get(&addr).and_then(WeakValue::upgrade))
                .ok()
                .flatten();
            return match upgraded {
                Some(v) => encode_owned(v),
                None => NanboxValue::UNSPECIFIED.into_raw() as u64,
            };
        }
        debug_assert!(
            unsafe { nb_owning_payload_is_live(self.car.get() as i64) },
            "Pair::car_raw_nb: use-after-region-drop — see the matching \
             debug_assert in car() for the full rationale."
        );
        unsafe { nb_clone_owned(self.car.get() as i64) as u64 }
    }

    /// NB-native counterpart to [`cdr`](Self::cdr). See
    /// [`car_raw_nb`](Self::car_raw_nb) for the full rationale.
    pub fn cdr_raw_nb(&self) -> u64 {
        if self.flags.get() & HAS_CDR_TOMBSTONE != 0 {
            let addr = self.addr();
            let upgraded = PAIR_CDR_TOMBSTONES
                .try_with(|t| t.borrow().get(&addr).and_then(WeakValue::upgrade))
                .ok()
                .flatten();
            return match upgraded {
                Some(v) => encode_owned(v),
                None => NanboxValue::UNSPECIFIED.into_raw() as u64,
            };
        }
        debug_assert!(
            unsafe { nb_owning_payload_is_live(self.cdr.get() as i64) },
            "Pair::cdr_raw_nb: use-after-region-drop — see the matching \
             debug_assert in cdr() for the full rationale."
        );
        unsafe { nb_clone_owned(self.cdr.get() as i64) as u64 }
    }

    /// NB-native counterpart to [`set_car`](Self::set_car): `bits`
    /// is an already-owned raw NB payload (the caller transfers
    /// ownership, exactly like `set_car`'s `Value` argument), so this
    /// skips `encode_owned`'s `Value` -> NB re-encode. Clears any
    /// weak tombstone, same as `set_car`.
    pub fn set_car_raw_nb(&self, bits: u64) {
        if self.flags.get() & HAS_CAR_TOMBSTONE != 0 {
            self.flags.set(self.flags.get() & !HAS_CAR_TOMBSTONE);
            let addr = self.addr();
            let _ = PAIR_CAR_TOMBSTONES.try_with(|t| {
                t.borrow_mut().remove(&addr);
            });
        }
        self.set_car_raw_owned(bits);
    }

    /// NB-native counterpart to [`set_cdr`](Self::set_cdr). See
    /// [`set_car_raw_nb`](Self::set_car_raw_nb).
    pub fn set_cdr_raw_nb(&self, bits: u64) {
        if self.flags.get() & HAS_CDR_TOMBSTONE != 0 {
            self.flags.set(self.flags.get() & !HAS_CDR_TOMBSTONE);
            let addr = self.addr();
            let _ = PAIR_CDR_TOMBSTONES.try_with(|t| {
                t.borrow_mut().remove(&addr);
            });
        }
        self.set_cdr_raw_owned(bits);
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
        self.set_car_raw_owned(encode_owned(v));
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
        self.set_cdr_raw_owned(encode_owned(v));
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
        // Raw, non-upgrading peek: if a prior break already
        // tombstoned this slot, `peek_car` sees the strong slot's
        // actual `Unspecified`, `heap_strong_count()` is `None`,
        // `total` is 0, and the guard below declines — same
        // outcome as the pre-PR2 code (which relied on the same
        // invariant: the strong slot reads back as `Unspecified`
        // once tombstoned).
        //
        // `peek_car` incref's on the way out (it's a clone-out
        // read), so `total` is one higher than the "true" strong
        // count until `peeked` drops — subtract that back out so
        // `baseline` comparisons are unaffected by our own peek.
        let peeked = self.peek_car();
        let total = peeked
            .heap_strong_count()
            .map(|n| n.saturating_sub(1))
            .unwrap_or(0);
        if total <= baseline {
            return false;
        }
        let Some(weak) = WeakValue::from_value(&peeked) else {
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
        self.set_car_raw_owned(encode_owned(Value::Unspecified));
        true
    }

    /// Cycle-break action for the cdr slot. See
    /// [`break_car_cycle`] for the `baseline` parameter
    /// convention.
    pub fn break_cdr_cycle(&self, baseline: usize) -> bool {
        let peeked = self.peek_cdr();
        let total = peeked
            .heap_strong_count()
            .map(|n| n.saturating_sub(1))
            .unwrap_or(0);
        if total <= baseline {
            return false;
        }
        let Some(weak) = WeakValue::from_value(&peeked) else {
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
        self.set_cdr_raw_owned(encode_owned(Value::Unspecified));
        true
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        // NOTE (PR2, cs-vnf.3): `self.car`/`self.cdr` are raw owning
        // NB `Cell<u64>` payloads now, not `RefCell<Value>` — this
        // Drop is responsible for explicitly decoding+dropping BOTH
        // slots exactly once (there's no automatic `Value` drop
        // glue anymore; a raw `u64` has none).
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

        // Drop the car slot directly (non-iterative). If car
        // happens to be part of a long car-chain this recurses
        // through the normal tag-dispatched drop below (same as
        // pre-PR2: only the CDR chain gets the iterative
        // unlinking treatment — car chains aren't the "long
        // proper list" shape it exists for).
        //
        // SAFETY: `self.car` holds this Pair's own live owning NB
        // payload (or inline immediate); `self` is being dropped
        // exactly once, so this fires exactly once.
        //
        // `replace` (not `get`), matching the pre-PR2 code's
        // `self.cdr.replace(Value::Null)` below: dropping this
        // payload can recurse arbitrarily (car may itself be a
        // Pair/Vector/Hashtable/Promise whose own drop runs) and
        // that recursive drop can trigger the layer-4 cycle-sweep,
        // which walks OTHER live pairs' car/cdr slots. If
        // `self.car` still held the same owning bits we're in the
        // middle of dropping, a sweep landing on `self` mid-drop
        // could read/tombstone/drop that same payload a second
        // time — replacing it with a non-owning immediate FIRST
        // closes that window.
        unsafe {
            nb_drop_owned(self.car.replace(NanboxValue::UNSPECIFIED.into_raw() as u64) as i64)
        };

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
        // is responsible for cleanup. `cur` is always an OWNED NB
        // payload (we transfer ownership out of whichever slot
        // currently holds it, one hop at a time). `replace`, same
        // reentrancy reasoning as the car drop above.
        let mut cur = self.cdr.replace(NanboxValue::NULL.into_raw() as u64) as i64;
        loop {
            let bits = cur as u64;
            if !(nb_is_tagged(bits) && nb_tag_of(bits) == NB_TAG_PAIR) {
                // Not a pair (Null, another leaf, or a different
                // heap type) — drop it normally (recursing through
                // its own destructor if it owns one) and stop.
                unsafe { nb_drop_owned(cur) };
                break;
            }
            // It's a pair: reconstitute the `Gc<Pair>` handle,
            // taking over the strong ref `cur` was carrying.
            let (ptr, is_region) = nb_decode_gc_ptr(nb_payload_of(bits));
            // SAFETY: `bits` is a live owning NB_TAG_PAIR payload
            // (either this Pair's own cdr, or a prior hop's cdr),
            // produced by the same `encode_owned`/`into_raw_jit`
            // machinery every other Pair construction uses.
            //
            // `decode_gc_handle_for_drop` (not `decode_gc_handle`):
            // this is a release, so an already-torn-down owning
            // region must be tolerated the same way `Gc<T>::drop`
            // tolerates it — a region's bulk-arena free never runs
            // `Pair::drop`, so a stale region-tagged cdr payload has
            // nothing left to release. `None` here means exactly
            // that: stop the walk, same as the "shared/region-
            // backed" `None` arm below.
            let Some(gc) = (unsafe { decode_gc_handle_for_drop::<Pair>(ptr, is_region) }) else {
                break;
            };
            match cs_gc::Gc::into_inner(gc) {
                Some(pair) => {
                    // Sole owner: take its cdr payload for the next
                    // hop, and neutralize `pair`'s own copy (an
                    // inline immediate, so "dropping" it is a
                    // no-op) so `pair`'s own `Drop::drop` — about
                    // to fire when `pair` goes out of scope below —
                    // doesn't also try to walk/drop the same
                    // payload we just took.
                    let next = pair.cdr.replace(NanboxValue::NULL.into_raw() as u64);
                    cur = next as i64;
                    // `pair` drops here: its own `Drop::drop` fires
                    // (flags cleanup + car drop via `nb_drop_owned`
                    // + a cdr walk that's now a one-iteration no-op
                    // since we already zeroed cdr to NULL).
                }
                // Shared (or region-backed): `Gc::into_inner`
                // already decremented the strong count as part of
                // consuming `gc` (mirrors `Rc::into_inner`'s
                // documented "drops one strong ref, returns None"
                // behavior on a shared handle) — there is nothing
                // further to release here.
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
        // Find the first heap-bearing value slot (the only kind
        // that could close a cycle through the hashtable), then
        // demote it via `break_value_cycle` (cs-i6p.2) so the
        // sweep path shares the same weak-tombstone/observability
        // guarantees as the mutation-site path instead of the
        // previous lossy direct zero-out.
        let idx = {
            let Ok(items) = self.items.try_borrow() else {
                // Borrow failed (mutating elsewhere); skip this
                // sweep round, the next one will retry.
                return false;
            };
            items.iter().position(|(_k, v)| {
                matches!(
                    v,
                    Value::Pair(_)
                        | Value::Vector(_)
                        | Value::Hashtable(_)
                        | Value::Promise(_)
                        | Value::String(_)
                        | Value::ByteVector(_)
                )
            })
        };
        match idx {
            // Safe to skip the `is_region` guard here: the layer-4
            // sweep only reaches `try_break_cycle` on hashtables
            // registered as cycle candidates, and
            // `record_cycle_with_candidate` already declines to
            // register region-allocated ones.
            Some(i) => self.break_value_cycle_unchecked(i, 0),
            None => false,
        }
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

/// `CsStr` is a leaf for cycle-tracing purposes, same as the bare
/// `String` it wraps (it holds no `Gc` children).
impl cs_gc::cycle::CycleVisit for CsStr {
    fn visit_children(&self, _ctx: &mut cs_gc::cycle::CycleVisitor) {}
}
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
    String(cs_gc::Weak<RefCell<CsStr>>),
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
            | Value::Fixnum(_)
            | Value::Flonum(_)
            | Value::BigNumber(_)
            | Value::Rational(_) => None,
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

/// # Vector cycle-break tombstones (cs-i6p.2)
///
/// Extends the [`Pair`] tombstone scheme (see its struct docs) to
/// `Value::Vector` slots. `Vector` has no first-party wrapper struct
/// — it's `Gc<RefCell<Vec<Value>>>>` over a foreign `RefCell` — so
/// there's nowhere to hang a `flags: Cell<u8>` field or a `Drop`
/// impl the way `Pair` does. That has two consequences vs. Pair's
/// design:
///
/// 1. **No per-slot presence bit.** [`VECTOR_ANY_TOMBSTONE`] is a
///    single global gate (not per-vector) so the overwhelmingly
///    common "no vector has ever been tombstoned" case costs one
///    `Cell::get` before any hashing.
/// 2. **No drop hook to evict stale entries.** Each
///    [`VectorTombstone`] additionally pins the *container's*
///    allocation via a `cs_gc::Weak<RefCell<Vec<Value>>>>` — as long
///    as that pin is alive, the container's control block can't be
///    freed and its address can't be handed to an unrelated
///    allocation, which is what would otherwise let a stale entry
///    alias onto a different vector at the same address. The
///    trade-off: once a vector slot is tombstoned, that small
///    fixed-size control block (not the vector's data — that drops
///    normally) leaks for the process lifetime if the vector itself
///    is never read again after being fully dropped. This is a
///    bounded, documented leak of one allocation header per
///    reclaimed cycle — categorically smaller than the unbounded
///    subgraph leak it replaces.
///
/// Only [`vector_get`] / [`vector_set`] (used by `vector-ref` /
/// `vector-set!`) go through the tombstone-aware funnel. Bulk
/// operations (`vector-map`, `vector-copy`, `vector->list`, the
/// printer, …) still read `Vec<Value>` slots directly and will see
/// `Value::Unspecified` rather than the transparently-upgraded
/// cyclic value for a demoted slot — the same category of residual
/// gap the pre-existing Hashtable `BreakCycle` sweep documents.
/// Closing that fully would require a first-party `Vector` wrapper
/// type and migrating on the order of 160 call sites across
/// cs-runtime/cs-vm/cs-jit-cranelift — the cost the
/// countable-memory exit report (iter 7.1.y) already priced and
/// deferred; out of scope here.
struct VectorTombstone {
    weak: WeakValue,
    /// See point 2 above — exists purely to keep the container's
    /// allocation (and thus its address) alive for as long as this
    /// tombstone entry exists. Never read.
    #[allow(dead_code)]
    container_pin: cs_gc::Weak<RefCell<Vec<Value>>>,
}

thread_local! {
    /// Out-of-line vector slot-cycle-break tombstone table, keyed by
    /// (vector address, slot index). See the module doc above.
    static VECTOR_TOMBSTONES: RefCell<HashMap<(usize, usize), VectorTombstone>> =
        RefCell::new(HashMap::new());

    /// `true` once any vector slot anywhere has ever been
    /// tombstoned. Lets [`vector_get`] / [`vector_set`] skip the
    /// `VECTOR_TOMBSTONES` lookup entirely in the common case.
    static VECTOR_ANY_TOMBSTONE: Cell<bool> = const { Cell::new(false) };
}

/// Read `v[i]`, upgrading a weak slot tombstone if present. Returns
/// `None` if `i` is out of bounds. See the module doc above for
/// which call sites this funnel actually covers.
///
/// Self-healing: a tombstone entry is only honored while the raw
/// slot still holds the `Unspecified` placeholder that
/// `vector_break_slot_cycle` wrote. Several writers go around
/// `vector_set` (`vector-fill!`, `vector-copy!`, `vector-sort!`, the
/// JIT's `vm_vector_set_gc`, …) and overwrite the raw slot directly
/// without knowing about the tombstone table; if the slot no longer
/// reads `Unspecified`, the tombstone is stale and is dropped here
/// instead of shadowing the real value. Known edge: a raw writer that
/// stores the literal unspecified value (e.g. `(vector-fill! v (if #f #f))`)
/// over a demoted slot is indistinguishable from the placeholder, so the
/// tombstone stays honored — inherent to the sentinel design.
pub fn vector_get(v: &cs_gc::Gc<RefCell<Vec<Value>>>, i: usize) -> Option<Value> {
    let raw = {
        let vb = v.borrow();
        if i >= vb.len() {
            return None;
        }
        vb[i].clone()
    };
    if !VECTOR_ANY_TOMBSTONE.with(Cell::get) {
        return Some(raw);
    }
    let addr = cs_gc::Gc::as_addr(v);
    if !matches!(raw, Value::Unspecified) {
        let _ = VECTOR_TOMBSTONES.try_with(|t| {
            t.borrow_mut().remove(&(addr, i));
        });
        return Some(raw);
    }
    let entry = VECTOR_TOMBSTONES
        .try_with(|t| t.borrow().get(&(addr, i)).map(|ts| ts.weak.clone()))
        .ok()
        .flatten();
    if let Some(weak) = entry {
        return Some(weak.upgrade().unwrap_or(Value::Unspecified));
    }
    Some(raw)
}

/// Write `val` into `v[i]`, clearing any weak tombstone (the new
/// value is unambiguously strong). Panics if `i` is out of bounds —
/// callers are expected to bounds-check first, matching the
/// pre-existing direct-index-write call sites this replaces.
pub fn vector_set(v: &cs_gc::Gc<RefCell<Vec<Value>>>, i: usize, val: Value) {
    if VECTOR_ANY_TOMBSTONE.with(Cell::get) {
        let addr = cs_gc::Gc::as_addr(v);
        let _ = VECTOR_TOMBSTONES.try_with(|t| {
            t.borrow_mut().remove(&(addr, i));
        });
    }
    v.borrow_mut()[i] = val;
}

/// Cycle-break action for `v[i]`. See `Pair::break_car_cycle` for
/// the `baseline` convention — the same reasoning applies here:
/// `(vector-set! v i v)`'s worst-case self-reference contributes
/// slot(1) + `args[2]`(1) + `args[0]`(1) = 3 transient strong refs.
///
/// Returns `false` (no-op) for region-allocated vectors — mirrors
/// `WeakValue::from_value`'s region guard; region drop reclaims
/// region cycles regardless.
pub fn vector_break_slot_cycle(
    v: &cs_gc::Gc<RefCell<Vec<Value>>>,
    i: usize,
    baseline: usize,
) -> bool {
    if cs_gc::Gc::is_region(v) {
        return false;
    }
    let current = {
        let vw = v.borrow();
        if i >= vw.len() {
            return false;
        }
        vw[i].clone()
    };
    let total = current.heap_strong_count().unwrap_or(0);
    if total <= baseline {
        return false;
    }
    let Some(weak) = WeakValue::from_value(&current) else {
        return false;
    };
    let addr = cs_gc::Gc::as_addr(v);
    let container_pin = cs_gc::Gc::downgrade(v);
    if VECTOR_TOMBSTONES
        .try_with(|t| {
            t.borrow_mut().insert(
                (addr, i),
                VectorTombstone {
                    weak,
                    container_pin,
                },
            );
        })
        .is_err()
    {
        return false;
    }
    VECTOR_ANY_TOMBSTONE.with(|f| f.set(true));
    v.borrow_mut()[i] = Value::Unspecified;
    true
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
        //
        // `peek_car`/`peek_cdr` (judge fixup, cs-vnf.3): same
        // reasoning as `CycleVisit for Pair` just below — this is
        // another cycle-sweep participant, not an external live-code
        // reader, so it must not trip `car()`/`cdr()`'s debug-only
        // UAF assert on legitimate dead-region traffic.
        self.peek_car().cycle_children(visit);
        self.peek_cdr().cycle_children(visit);
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
        //
        // `peek_car`/`peek_cdr`, NOT the public `car()`/`cdr()`
        // (judge fixup, cs-vnf.3): this IS the layer-4 cycle sweep
        // that can run mid-Drop-chain (see the note on `Drop for
        // Pair`) and legitimately encounter a dead-region payload on
        // some OTHER live pair it's walking through — that's
        // expected traffic here, not a bug, so it must stay silently
        // tolerant regardless of build profile. `car()`/`cdr()`
        // carry a debug-only loud UAF assert precisely because
        // THEIR callers are external/live-code and a dead-region hit
        // there IS a bug; routing this internal walk through them
        // would trip that assert on legitimate traffic. Mirrors the
        // identical fix already applied to `cs-vm`'s
        // `Bindings::visit_children`.
        let car = self.peek_car();
        car.visit_children(ctx);
        if ctx.done() {
            return;
        }
        let cdr = self.peek_cdr();
        cdr.visit_children(ctx);
    }
}

/// Hashtable equality kind.
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
///
/// `items` is the source of truth (preserves insertion order, which
/// existing iteration/keys/values/entries callers rely on). `index` is a
/// bucket accelerator over it: hash value -> indices into `items` sharing
/// that hash. It always mirrors `items` (`hash_of(items[i].0) == h` for
/// every `i` in `index[h]`) except transiently mid-mutation inside
/// cs-runtime's hashtable builtins, which keep the two in sync on every
/// insert/delete. A stale index (e.g. from mutating a key in place after
/// insertion, which no hashtable implementation defends against) can only
/// cause a false probe miss resolved by falling back to the real
/// eq/eqv/equal comparator — it can never cause a false hit.
#[derive(Debug)]
pub struct Hashtable {
    pub items: RefCell<Vec<(Value, Value)>>,
    pub eq_kind: HtEqKind,
    /// Populated only when `eq_kind == Custom`.
    pub custom: Option<CustomHashFns>,
    /// Hash -> indices into `items`. See struct docs for the invariant.
    pub index: RefCell<HashMap<u64, Vec<u32>>>,
    /// Cold-metadata presence bit: see [`HAS_VALUE_TOMBSTONE`]. `Cell`
    /// so the tombstone helpers don't need `&mut Hashtable`.
    flags: Cell<u8>,
}

/// Bit of [`Hashtable::flags`] set when this hashtable has at least
/// one entry in [`HASHTABLE_VALUE_TOMBSTONES`] keyed by its own
/// address. See [`Hashtable::value_at`] / [`Hashtable::break_value_cycle`].
const HAS_VALUE_TOMBSTONE: u8 = 0b001;

thread_local! {
    /// Out-of-line hashtable **value**-cycle-break tombstone table
    /// (cs-i6p.2), keyed by (hashtable address, `items` index).
    /// Mirrors [`PAIR_CAR_TOMBSTONES`] — see
    /// [`Hashtable::break_value_cycle`].
    ///
    /// Deliberately values only, never keys: a demoted key would
    /// desync `index`'s hash-bucket invariant (the bucket was built
    /// from the key's *original* hash/identity) and corrupt
    /// `hashtable-keys` / `hashtable->alist` / future lookups.
    /// Values carry no such invariant, so demoting them is sound.
    /// `Hashtable` is a first-party struct (unlike `Vector`, which
    /// is a bare `Gc<RefCell<Vec<Value>>>>` over foreign types), so
    /// — like `Pair` — it can carry its own `flags` bit and a `Drop`
    /// impl that evicts this table's entries when the table itself
    /// is freed, closing the address-reuse hazard `Pair`'s doc
    /// comment warns about without needing `Vector`'s container-pin
    /// workaround.
    static HASHTABLE_VALUE_TOMBSTONES: RefCell<HashMap<(usize, usize), WeakValue>> =
        RefCell::new(HashMap::new());
}

impl Hashtable {
    pub fn new(eq_kind: HtEqKind) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Hashtable {
            items: RefCell::new(Vec::new()),
            eq_kind,
            custom: None,
            index: RefCell::new(HashMap::new()),
            flags: Cell::new(0),
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
            index: RefCell::new(HashMap::new()),
            flags: Cell::new(0),
        })
    }

    /// Construct a `Hashtable` value (not yet `Gc`-wrapped) from
    /// pre-populated parts. `flags` is private (cs-i6p.2's
    /// tombstone-presence bit, mirroring `Pair`), so callers outside
    /// this module that need to build a `Hashtable` with existing
    /// `items`/`index`/`custom` data — e.g. lifetime-aware
    /// constructors that choose their own `Gc::new` vs.
    /// `Gc::new_in(region, ..)` — go through this instead of struct-
    /// literal syntax.
    pub fn from_parts(
        items: Vec<(Value, Value)>,
        eq_kind: HtEqKind,
        custom: Option<CustomHashFns>,
        index: HashMap<u64, Vec<u32>>,
    ) -> Self {
        Hashtable {
            items: RefCell::new(items),
            eq_kind,
            custom,
            index: RefCell::new(index),
            flags: Cell::new(0),
        }
    }

    /// This hashtable's out-of-line-table key: the same address
    /// identity `Gc::as_addr` computes, obtained directly from
    /// `&self` since `Gc<Hashtable>` derefs straight to it.
    fn addr(&self) -> usize {
        self as *const Hashtable as usize
    }

    /// Read `items[i].1`, upgrading a weak value-cycle tombstone if
    /// present. Returns `None` if `i` is out of bounds. Returns
    /// `Value::Unspecified` for a tombstone whose target has been
    /// fully reclaimed — mirrors `Pair::car`.
    ///
    /// Self-healing: a tombstone entry is only honored while the raw
    /// slot still reads `Unspecified`. Some writers touch
    /// `items[i].1` directly instead of going through
    /// `set_value_at` (e.g. VM-tier hashtable builtin overrides); if
    /// the raw slot no longer reads `Unspecified` the tombstone is
    /// stale and is dropped here rather than shadowing the real
    /// value.
    pub fn value_at(&self, i: usize) -> Option<Value> {
        let raw = {
            let items = self.items.borrow();
            if i >= items.len() {
                return None;
            }
            items[i].1.clone()
        };
        if self.flags.get() & HAS_VALUE_TOMBSTONE == 0 {
            return Some(raw);
        }
        let addr = self.addr();
        if !matches!(raw, Value::Unspecified) {
            let _ = HASHTABLE_VALUE_TOMBSTONES.try_with(|t| {
                t.borrow_mut().remove(&(addr, i));
            });
            return Some(raw);
        }
        let entry = HASHTABLE_VALUE_TOMBSTONES
            .try_with(|t| t.borrow().get(&(addr, i)).cloned())
            .ok()
            .flatten();
        if let Some(weak) = entry {
            return Some(weak.upgrade().unwrap_or(Value::Unspecified));
        }
        Some(raw)
    }

    /// Overwrite `items[i].1` with `v`, clearing any weak tombstone
    /// on that slot (the new value is unambiguously strong).
    /// Mirrors `Pair::set_car`. Panics if `i` is out of bounds.
    pub fn set_value_at(&self, i: usize, v: Value) {
        if self.flags.get() & HAS_VALUE_TOMBSTONE != 0 {
            let addr = self.addr();
            let _ = HASHTABLE_VALUE_TOMBSTONES.try_with(|t| {
                t.borrow_mut().remove(&(addr, i));
            });
        }
        self.items.borrow_mut()[i].1 = v;
    }

    /// Cycle-break action for `items[i].1`. See
    /// `Pair::break_car_cycle` for the `baseline` convention — the
    /// same reasoning applies here: `(hashtable-set! h k h)`'s
    /// worst-case self-reference contributes slot(1) + `args[2]`(1)
    /// + `args[0]`(1) = 3 transient strong refs.
    ///
    /// Returns `false` (no-op) for region-allocated hashtables —
    /// mirrors `vector_break_slot_cycle`'s region guard (which
    /// mirrors `WeakValue::from_value`'s): region drop reclaims
    /// region cycles regardless, and tombstoning by *this* table's
    /// address would leak a `HASHTABLE_VALUE_TOMBSTONES` entry keyed
    /// to an address that a region-bump-allocated table never gets a
    /// `Drop` call for on region reset — a stale entry that could
    /// alias onto whatever unrelated table the region next hands
    /// that address to.
    ///
    /// Takes `h: &Gc<Hashtable>` (rather than plain `&self`) so this
    /// can check `Gc::is_region` itself instead of relying on every
    /// call site to guard upstream — mirrors
    /// `vector_break_slot_cycle`'s own `is_region` check on the
    /// container.
    pub fn break_value_cycle(h: &cs_gc::Gc<Hashtable>, i: usize, baseline: usize) -> bool {
        if cs_gc::Gc::is_region(h) {
            return false;
        }
        h.break_value_cycle_unchecked(i, baseline)
    }

    /// The actual demote logic, minus the `is_region` guard. Only
    /// call this directly when the caller has already established
    /// `self` can't be region-allocated — today that's exactly the
    /// layer-4 sweep's `try_break_cycle`, which only ever runs on
    /// hashtables the cycle registry accepted, and
    /// `record_cycle_with_candidate` already declines to register
    /// region allocations (see that function's `is_region` early
    /// return). All other callers should go through
    /// `break_value_cycle` above.
    fn break_value_cycle_unchecked(&self, i: usize, baseline: usize) -> bool {
        let current = {
            let items = self.items.borrow();
            if i >= items.len() {
                return false;
            }
            items[i].1.clone()
        };
        let total = current.heap_strong_count().unwrap_or(0);
        if total <= baseline {
            return false;
        }
        let Some(weak) = WeakValue::from_value(&current) else {
            return false;
        };
        let addr = self.addr();
        if HASHTABLE_VALUE_TOMBSTONES
            .try_with(|t| {
                t.borrow_mut().insert((addr, i), weak);
            })
            .is_err()
        {
            return false;
        }
        self.flags.set(self.flags.get() | HAS_VALUE_TOMBSTONE);
        self.items.borrow_mut()[i].1 = Value::Unspecified;
        true
    }

    /// `Vec::swap_remove` over `items`, fixing up any value
    /// tombstone so it doesn't silently end up describing whatever
    /// unrelated entry `swap_remove` moves into slot `i`.
    /// `hashtable-set!`'s push (new key) and in-place-update (same
    /// key) paths never shift an existing index, so only the
    /// delete path needs this.
    pub fn swap_remove_item(&self, i: usize) -> (Value, Value) {
        let removed = self.items.borrow_mut().swap_remove(i);
        if self.flags.get() & HAS_VALUE_TOMBSTONE != 0 {
            let addr = self.addr();
            // `swap_remove(i)` moves the old last element (index
            // `items.len()` *after* removal) into slot `i`. Migrate
            // that element's tombstone, if any, to its new index.
            let moved_from = self.items.borrow().len();
            let _ = HASHTABLE_VALUE_TOMBSTONES.try_with(|t| {
                let mut t = t.borrow_mut();
                t.remove(&(addr, i));
                if let Some(w) = t.remove(&(addr, moved_from)) {
                    t.insert((addr, i), w);
                }
            });
        }
        removed
    }

    /// Clear `items`, dropping any value tombstones for this table.
    pub fn clear_items(&self) {
        self.items.borrow_mut().clear();
        if self.flags.get() & HAS_VALUE_TOMBSTONE != 0 {
            let addr = self.addr();
            let _ = HASHTABLE_VALUE_TOMBSTONES.try_with(|t| {
                t.borrow_mut().retain(|k, _| k.0 != addr);
            });
            self.flags.set(self.flags.get() & !HAS_VALUE_TOMBSTONE);
        }
    }

    /// Record that `items[item_idx]` hashes to `hash`.
    pub fn index_insert(&self, hash: u64, item_idx: u32) {
        self.index
            .borrow_mut()
            .entry(hash)
            .or_default()
            .push(item_idx);
    }

    /// Rebuild the bucket index from scratch via `hash_fn`. O(n); used
    /// after bulk mutation (delete, copy, clear) where incremental
    /// fixup isn't worth the bookkeeping.
    pub fn rebuild_index<F: Fn(&Value) -> u64>(&self, hash_fn: F) {
        let mut index = self.index.borrow_mut();
        index.clear();
        for (i, (k, _)) in self.items.borrow().iter().enumerate() {
            index.entry(hash_fn(k)).or_default().push(i as u32);
        }
    }
}

impl Drop for Hashtable {
    fn drop(&mut self) {
        // A freed hashtable address CANNOT be left pointing at
        // stale `HASHTABLE_VALUE_TOMBSTONES` entries — the allocator
        // may hand that same address to an unrelated `Hashtable`
        // next. `flags == 0` (no tombstone ever created) is the
        // overwhelming common case and short-circuits without
        // touching the table. Mirrors `Pair::drop`.
        if self.flags.get() & HAS_VALUE_TOMBSTONE != 0 {
            let addr = self.addr();
            let _ = HASHTABLE_VALUE_TOMBSTONES.try_with(|t| {
                t.borrow_mut().retain(|k, _| k.0 != addr);
            });
        }
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

/// A port: foundation supports string-, bytevector-, file-, and
/// stdout-backed ports. File output ports write through a `BufWriter`
/// opened at `open-output-file` time; `flush-output-port` flushes the
/// writer and `close-port` flushes and drops it — neither rewrites the
/// whole file. File input is currently slurped as a string-input-port
/// at open time (see `b_open_input_file` in cs-runtime); a streaming
/// file-input variant lands in a later milestone.
#[derive(Debug)]
pub enum Port {
    StringInput(RefCell<StringInputState>),
    StringOutput(RefCell<String>),
    ByteVectorInput(RefCell<ByteVectorInputState>),
    ByteVectorOutput(RefCell<Vec<u8>>),
    /// File output port, incrementally written via a `BufWriter<File>`.
    /// `writer` is `None` once the port has been closed; subsequent
    /// writes are rejected.
    FileOutput(RefCell<FileOutputState>),
    /// The process's standard output stream. Stateless: writes go
    /// straight to `io::stdout()`, matching the historical no-port
    /// fallback so ordering relative to other stdout writers is
    /// unchanged.
    Stdout,
}

#[derive(Debug)]
pub struct FileOutputState {
    pub path: String,
    writer: Option<BufWriter<File>>,
}

impl FileOutputState {
    fn open(path: String) -> io::Result<Self> {
        let file = File::create(&path)?;
        Ok(FileOutputState {
            path,
            writer: Some(BufWriter::new(file)),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.writer.is_none()
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.writer {
            Some(w) => w.write_all(bytes),
            None => Err(io::Error::new(io::ErrorKind::Other, "port is closed")),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.writer {
            Some(w) => w.flush(),
            None => Ok(()),
        }
    }

    pub fn close(&mut self) -> io::Result<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush()?;
        }
        Ok(())
    }

    /// Bytes written so far (buffered + flushed), used for `port-position`.
    pub fn position(&mut self) -> u64 {
        self.writer
            .as_mut()
            .and_then(|w| w.stream_position().ok())
            .unwrap_or(0)
    }

    /// Best-effort duplicate used by GC region-promotion, where the
    /// enclosing `Port` is re-allocated as a fresh `Gc`. `File` isn't
    /// `Clone`, so this flushes any buffered data and hands the new
    /// state a `try_clone`d fd (shares the OS file offset, so writes
    /// through either handle keep landing at the right place). On any
    /// I/O failure the duplicate comes back closed rather than losing
    /// data silently mid-write.
    pub fn duplicate(&mut self) -> FileOutputState {
        let writer = self.writer.as_mut().and_then(|w| {
            w.flush().ok()?;
            w.get_ref().try_clone().ok().map(BufWriter::new)
        });
        FileOutputState {
            path: self.path.clone(),
            writer,
        }
    }
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

    pub fn file_output(path: String) -> io::Result<cs_gc::Gc<Self>> {
        Ok(cs_gc::Gc::new(Port::FileOutput(RefCell::new(
            FileOutputState::open(path)?,
        ))))
    }

    pub fn stdout() -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::Stdout)
    }

    pub fn is_input(&self) -> bool {
        matches!(self, Port::StringInput(_) | Port::ByteVectorInput(_))
    }

    pub fn is_output(&self) -> bool {
        matches!(
            self,
            Port::StringOutput(_) | Port::ByteVectorOutput(_) | Port::FileOutput(_) | Port::Stdout
        )
    }

    pub fn is_textual(&self) -> bool {
        // Files can carry either, but the typical R6RS use of file output
        // is textual via `display`/`write`. Classify as textual; binary
        // file ports would be a separate variant.
        matches!(
            self,
            Port::StringInput(_) | Port::StringOutput(_) | Port::FileOutput(_) | Port::Stdout
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

/// Scheme string payload: a `String` plus a cached "is this string
/// all-ASCII" flag, recomputed only on construction and on mutation
/// (`set_from`). This lets the hot char-indexed primitives
/// (`string-length`, `string-ref`, ...) take an O(1)/byte-indexed
/// fast path for the dominant all-ASCII case instead of walking the
/// UTF-8 decode on every call (cs-byy).
///
/// Deref's to `String` so read-only call sites (`.chars()`, `.len()`,
/// `.as_bytes()`, ...) keep compiling unchanged. There is deliberately
/// no `DerefMut`: an in-place mutation through a generic `String`
/// method would silently invalidate `ascii` without recomputing it.
/// All mutation must go through [`CsStr::set_from`].
///
/// Deliberately NOT `Clone`: this lets the hundreds of pre-existing
/// `s.borrow().clone()` call sites across the workspace keep resolving
/// through `Deref` to `String::clone`, so they keep compiling and
/// keep returning a plain `String` (which is what nearly all of them
/// actually want) without being touched. Code that specifically wants
/// a full `CsStr` copy (cached flag included) calls [`CsStr::duplicate`].
#[derive(Debug, Default)]
pub struct CsStr {
    s: String,
    ascii: bool,
}

impl CsStr {
    pub fn new(s: impl Into<String>) -> Self {
        let s = s.into();
        let ascii = s.is_ascii();
        CsStr { s, ascii }
    }

    /// Explicit full copy (string content + cached ASCII flag), for the
    /// handful of call sites that need a `CsStr` rather than a `String`.
    pub fn duplicate(&self) -> Self {
        CsStr {
            s: self.s.clone(),
            ascii: self.ascii,
        }
    }

    /// True if the string is entirely ASCII, letting callers use a
    /// byte-indexed fast path instead of a `chars()` walk.
    #[inline]
    pub fn is_ascii_cached(&self) -> bool {
        self.ascii
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.s
    }

    /// Replace the string content, recomputing the ASCII cache.
    /// This is the only sanctioned way to mutate a `CsStr` in place.
    pub fn set_from(&mut self, new: String) {
        self.ascii = new.is_ascii();
        self.s = new;
    }

    /// Mutable byte access for in-place ASCII-preserving edits (e.g. a
    /// single-byte swap for `string-set!`). Caller must uphold the
    /// invariant that the buffer stays valid UTF-8 and all-ASCII, since
    /// the cached `ascii` flag is NOT recomputed here.
    ///
    /// # Safety
    /// Same contract as `String::as_bytes_mut`: writes must preserve
    /// valid UTF-8. Additionally the caller must only write ASCII
    /// bytes (0..=0x7f), since `ascii` is not recomputed.
    #[inline]
    pub unsafe fn ascii_bytes_mut(&mut self) -> &mut [u8] {
        debug_assert!(self.ascii, "ascii_bytes_mut called on non-ASCII CsStr");
        self.s.as_bytes_mut()
    }
}

impl std::ops::Deref for CsStr {
    type Target = String;
    #[inline]
    fn deref(&self) -> &String {
        &self.s
    }
}

impl std::fmt::Display for CsStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.s, f)
    }
}

impl PartialEq for CsStr {
    fn eq(&self, other: &Self) -> bool {
        self.s == other.s
    }
}
impl Eq for CsStr {}

impl PartialOrd for CsStr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.s.cmp(&other.s))
    }
}
impl Ord for CsStr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.s.cmp(&other.s)
    }
}

impl PartialEq<str> for CsStr {
    fn eq(&self, other: &str) -> bool {
        self.s == other
    }
}
impl PartialEq<String> for CsStr {
    fn eq(&self, other: &String) -> bool {
        &self.s == other
    }
}
impl PartialEq<CsStr> for String {
    fn eq(&self, other: &CsStr) -> bool {
        self == &other.s
    }
}
impl PartialEq<&str> for CsStr {
    fn eq(&self, other: &&str) -> bool {
        self.s == *other
    }
}

impl From<String> for CsStr {
    fn from(s: String) -> Self {
        CsStr::new(s)
    }
}
impl From<&str> for CsStr {
    fn from(s: &str) -> Self {
        CsStr::new(s)
    }
}

/// The universal Scheme value.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Unspecified,
    Eof,
    Boolean(bool),
    Character(char),
    /// Exact integer that fits in a machine word. Hot arm of the
    /// numeric tower, flattened directly into `Value` (cs-3wa) so
    /// the whole enum stays 16 bytes: an inline `Number` would cost
    /// a second discriminant word. Use [`Value::as_number`] to bridge
    /// to the `Number` tower for arithmetic.
    Fixnum(i64),
    /// IEEE-754 double (inexact real). See [`Value::Fixnum`].
    Flonum(f64),
    /// Exact integer outside `i64` range (cold arm, boxed).
    BigNumber(Rc<num_bigint::BigInt>),
    /// Exact non-integer ratio (cold arm, boxed).
    Rational(Rc<num_rational::BigRational>),
    String(crate::Gc<RefCell<CsStr>>),
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
            | Value::Fixnum(_)
            | Value::Flonum(_)
            | Value::BigNumber(_)
            | Value::Rational(_) => {}
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
            | Value::Fixnum(_)
            | Value::Flonum(_)
            | Value::BigNumber(_)
            | Value::Rational(_)
            | Value::Symbol(_)
            | Value::Identifier { .. }
            | Value::String(_)
            | Value::ByteVector(_)
            | Value::Port(_)
    )
}

impl Value {
    pub fn fixnum(v: i64) -> Self {
        Value::Fixnum(v)
    }

    pub fn flonum(v: f64) -> Self {
        Value::Flonum(v)
    }

    /// Bridge a numeric-tower [`Number`] into a `Value`. Cheap:
    /// Fixnum/Flonum copy, Big/Rat move the existing `Rc`.
    pub fn from_number(n: Number) -> Self {
        match n {
            Number::Fixnum(i) => Value::Fixnum(i),
            Number::Flonum(f) => Value::Flonum(f),
            Number::Big(b) => Value::BigNumber(b),
            Number::Rat(r) => Value::Rational(r),
        }
    }

    /// Bridge a numeric `Value` back into the [`Number`] tower for
    /// arithmetic, or `None` for non-numbers. Cheap: Fixnum/Flonum
    /// copy, Big/Rat bump the `Rc`.
    pub fn as_number(&self) -> Option<Number> {
        match self {
            Value::Fixnum(i) => Some(Number::Fixnum(*i)),
            Value::Flonum(f) => Some(Number::Flonum(*f)),
            Value::BigNumber(b) => Some(Number::Big(b.clone())),
            Value::Rational(r) => Some(Number::Rat(r.clone())),
            _ => None,
        }
    }

    /// `true` if this value is any arm of the numeric tower.
    pub fn is_number(&self) -> bool {
        matches!(
            self,
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_)
        )
    }

    pub fn string(s: impl Into<String>) -> Self {
        Value::String(crate::Gc::new(RefCell::new(CsStr::new(s.into()))))
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
            | Value::Fixnum(_)
            | Value::Flonum(_)
            | Value::BigNumber(_)
            | Value::Rational(_) => None,
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
            | Value::Fixnum(_)
            | Value::Flonum(_)
            | Value::BigNumber(_)
            | Value::Rational(_) => None,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Unspecified => "unspecified",
            Value::Eof => "eof",
            Value::Boolean(_) => "boolean",
            Value::Character(_) => "character",
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_) => {
                "number"
            }
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
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_) => {
                write!(out, "{}", self.as_number().unwrap())
            }
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
                Port::Stdout => write!(out, "#<stdout-port>"),
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
            Value::Fixnum(_) | Value::Flonum(_) | Value::BigNumber(_) | Value::Rational(_) => {
                write!(f, "{}", self.as_number().unwrap())
            }
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
                Port::Stdout => write!(f, "#<stdout-port>"),
            },
            Value::Promise(_) => write!(f, "#<promise>"),
        }
    }
}

/// cs-vnf.3 PR2 regression: `Pair`'s raw `Cell<u64>` NB slots must
/// tolerate an owning region-tagged payload whose region has
/// already dropped — a real (if rare) scenario reachable via a
/// Pair's own `Drop` triggering the layer-4 cycle sweep, which can
/// walk OTHER live pairs' slots (see the note in `Drop for Pair`).
/// Before this fix, `nb_drop_owned`/`peek_car`/`peek_cdr` decoded
/// every pointer-typed tag through the strict
/// `decode_gc_handle`/`from_raw_jit_region`, which panics on
/// exactly this condition (`from_raw_jit_region: slot region_id
/// was 0`) — a panic that, reached from inside a recursive `Drop`
/// chain, aborts the process rather than unwinding. These tests
/// build that exact condition directly and cheaply (no VM/cycle
/// sweep needed at the cs-core layer): a `Pair` slot whose payload
/// is a region-tagged NB encoding for a `Region` that has already
/// dropped.
#[cfg(all(test, feature = "regions"))]
mod region_drop_tolerance_tests {
    use super::*;
    use cs_gc::Region;

    /// A raw region-tagged NB payload for a `Region` that has
    /// already dropped. `Region::drop` marks the region dead in
    /// `cs_gc`'s thread-local slab deterministically (before the
    /// arena memory itself is freed), so this is reproducible every
    /// run, not a timing-dependent UAF.
    fn dead_region_owning_bits() -> u64 {
        let region = Region::new();
        let p = cs_core_pair_new_in_for_test(&region);
        let bits = encode_owned(Value::Pair(p));
        drop(region);
        bits
    }

    // Thin wrapper so this module doesn't need `cs_gc::Gc::is_region`
    // asserted inline at every call site.
    fn cs_core_pair_new_in_for_test(region: &Region) -> cs_gc::Gc<Pair> {
        let p = Pair::new_in(region, Value::fixnum(11), Value::fixnum(22));
        assert!(
            cs_gc::Gc::is_region(&p),
            "test setup: must be region-backed"
        );
        p
    }

    /// Reading a live pair's car when the car slot's payload is a
    /// dead-region pointer must not panic — it degrades to
    /// `Unspecified`, the same graceful fallback an
    /// already-tombstoned slot gets.
    #[test]
    fn peek_car_tolerates_dead_region_payload() {
        let bits = dead_region_owning_bits();
        let holder = Pair::new(Value::Unspecified, Value::Unspecified);
        holder.set_car_raw_owned(bits);
        // `peek_car` directly, not the public `car()` — `car()` now
        // carries a debug-only loud UAF assert (judge fixup) since a
        // dead-region payload reaching the PUBLIC accessor is a real
        // bug the caller should hear about; `peek_car` is the
        // Drop/cycle-sweep-internal path that must stay silently
        // tolerant no matter the build profile, which is what this
        // test actually exercises.
        assert!(matches!(holder.peek_car(), Value::Unspecified));
    }

    /// Same for cdr.
    #[test]
    fn peek_cdr_tolerates_dead_region_payload() {
        let bits = dead_region_owning_bits();
        let holder = Pair::new(Value::Unspecified, Value::Unspecified);
        holder.set_cdr_raw_owned(bits);
        assert!(matches!(holder.peek_cdr(), Value::Unspecified));
    }

    /// Dropping a `Pair` whose car (or cdr) slot holds a
    /// dead-region payload must not panic — `nb_drop_owned` skips
    /// the release (nothing left to release; region bulk-free
    /// already reclaimed it).
    #[test]
    fn drop_tolerates_dead_region_payload_in_car() {
        let bits = dead_region_owning_bits();
        let holder = Pair::new(Value::Unspecified, Value::Unspecified);
        holder.set_car_raw_owned(bits);
        drop(holder); // must not panic
    }

    #[test]
    fn drop_tolerates_dead_region_payload_in_cdr() {
        let bits = dead_region_owning_bits();
        let holder = Pair::new(Value::Unspecified, Value::Unspecified);
        holder.set_cdr_raw_owned(bits);
        drop(holder); // must not panic
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
    ///
    /// cs-vnf.3 PR2: `car`/`cdr` flip from `RefCell<Value>` (16B
    /// each — a 16-byte tagged Rust enum behind a `RefCell` borrow
    /// flag) to raw NaN-boxed `Cell<u64>` (8B each). New size:
    /// `car`(8) + `cdr`(8) + `flags`(1, padded to 8) = 24B — a
    /// further 56B -> 24B drop (with the +16B `Rc` header, cons
    /// cells shrink 72B -> 40B).
    #[test]
    fn pair_is_diet_sized() {
        let before = 56usize;
        let after = std::mem::size_of::<Pair>();
        assert_eq!(
            after, 24,
            "Pair size changed: {after}B (was {before}B before cs-vnf.3 PR2's Cell<u64> NB-slot flip)"
        );
        assert!(
            after <= 80,
            "Pair grew past the cs-5te <= 80B target: {after}B"
        );
    }

    #[test]
    fn plain_cons_has_no_span_or_tombstone() {
        let p = Pair::new(Value::Fixnum(1), Value::Null);
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

    #[test]
    fn vector_tombstone_round_trip_and_clear_on_set() {
        // Mirrors `tombstone_round_trip_and_clear_on_set` above:
        // a self-referential vector slot with one external anchor
        // (the `v` binding) beyond the demote's own transient refs.
        let v = cs_gc::Gc::new(RefCell::new(vec![Value::Null]));
        vector_set(&v, 0, Value::Vector(v.clone()));
        // Strong refs to `v` right now: the `v` binding + the slot
        // = 2. baseline=1 leaves one external anchor, so the
        // demote should fire.
        assert!(vector_break_slot_cycle(&v, 0, 1));
        assert!(VECTOR_ANY_TOMBSTONE.with(Cell::get));
        // Tombstone upgrade returns the cyclic value transparently.
        assert!(matches!(vector_get(&v, 0), Some(Value::Vector(_))));
        // Writing over the demoted slot clears the tombstone.
        vector_set(&v, 0, Value::Null);
        let addr = cs_gc::Gc::as_addr(&v);
        let leaked = VECTOR_TOMBSTONES.with(|t| t.borrow().contains_key(&(addr, 0)));
        assert!(!leaked, "vector_set must clear the tombstone table entry");
    }

    #[test]
    fn hashtable_value_tombstone_round_trip_and_clear_on_set() {
        let h = Hashtable::new(HtEqKind::Eq);
        h.items
            .borrow_mut()
            .push((Value::Symbol(Symbol(0)), Value::Unspecified));
        h.set_value_at(0, Value::Hashtable(h.clone()));
        // Strong refs to `h` right now: the `h` binding + the
        // value slot = 2. baseline=1 leaves one external anchor.
        assert!(Hashtable::break_value_cycle(&h, 0, 1));
        assert_eq!(h.flags.get() & HAS_VALUE_TOMBSTONE, HAS_VALUE_TOMBSTONE);
        // Tombstone upgrade returns the cyclic value transparently.
        assert!(matches!(h.value_at(0), Some(Value::Hashtable(_))));
        // Writing over the demoted slot clears the tombstone.
        h.set_value_at(0, Value::Unspecified);
        let addr = h.addr();
        let leaked = HASHTABLE_VALUE_TOMBSTONES.with(|t| t.borrow().contains_key(&(addr, 0)));
        assert!(!leaked, "set_value_at must clear the tombstone table entry");
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

    /// cs-3wa: the numeric tower is flattened directly into `Value`
    /// (`Fixnum(i64)`/`Flonum(f64)`/`BigNumber`/`Rational`) instead of
    /// an inline 16B `Number`, so the whole enum is a single
    /// discriminant word plus an 8B payload. This locks that in.
    #[test]
    fn value_is_two_words() {
        assert_eq!(
            std::mem::size_of::<super::Value>(),
            16,
            "Value must be 16 bytes: 8B discriminant/niche + 8B payload"
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
