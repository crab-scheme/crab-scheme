//! The universal Scheme value type.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::number::Number;
use crate::symbol::{Symbol, SymbolTable};

// Bring the CycleVisit trait into scope so per-type
// `visit_children` calls resolve under the feature.
#[cfg(feature = "countable-memory")]
use cs_gc::cycle::CycleVisit as _;

/// A pair (cons cell). Mutable per R6RS via `set-car!` / `set-cdr!`.
///
/// # Cycle-break tombstones (countable-memory iter 7.1)
///
/// Under `feature = "countable-memory"`, `car_weak` / `cdr_weak`
/// are weak-reference tombstones. The mutation cycle detector
/// in `cs-runtime` flips a slot from strong to weak via
/// [`break_car_cycle`]/[`break_cdr_cycle`]: the strong `Value`
/// gets replaced with `Unspecified` and a `WeakValue` referring
/// to the same allocation is parked in the tombstone. Reads via
/// the [`car`]/[`cdr`] accessors transparently `upgrade()` the
/// tombstone, so the user-observable cyclic structure stays
/// intact (R6RS requires `(set-cdr! x x)` to produce an
/// observable cyclic list); but the strong-count chain is
/// broken so refcount reclaims the cycle when no other strong
/// reference remains.
///
/// Direct field access to `.car`/`.cdr` is preserved for
/// backward compatibility with code paths that don't need to
/// observe broken cycles (the strong slot is `Unspecified` for
/// such cells). New code should prefer the accessors.
#[derive(Debug)]
pub struct Pair {
    pub car: RefCell<Value>,
    pub cdr: RefCell<Value>,
    #[cfg(feature = "countable-memory")]
    car_weak: RefCell<Option<WeakValue>>,
    #[cfg(feature = "countable-memory")]
    cdr_weak: RefCell<Option<WeakValue>>,
}

impl Pair {
    pub fn new(car: Value, cdr: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Pair {
            car: RefCell::new(car),
            cdr: RefCell::new(cdr),
            #[cfg(feature = "countable-memory")]
            car_weak: RefCell::new(None),
            #[cfg(feature = "countable-memory")]
            cdr_weak: RefCell::new(None),
        })
    }

    /// Construct a Pair allocated in `region`'s bump arena
    /// (region-memory iter 4). Layer-5 escape analysis emits
    /// this when it proves the Pair's lifetime is bounded by
    /// some surrounding scope.
    #[cfg(feature = "regions")]
    pub fn new_in(region: &cs_gc::Region, car: Value, cdr: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new_in(
            region,
            Pair {
                car: RefCell::new(car),
                cdr: RefCell::new(cdr),
                #[cfg(feature = "countable-memory")]
                car_weak: RefCell::new(None),
                #[cfg(feature = "countable-memory")]
                cdr_weak: RefCell::new(None),
            },
        )
    }

    /// Read the car as a `Value`. Under countable-memory, an
    /// upgraded weak tombstone takes precedence over the strong
    /// slot — so broken cycles still produce the user-observable
    /// cyclic value as long as some other strong reference
    /// holds the target alive. Returns `Value::Unspecified` if
    /// the tombstone target has been fully reclaimed.
    pub fn car(&self) -> Value {
        #[cfg(feature = "countable-memory")]
        {
            if let Some(w) = self.car_weak.borrow().as_ref() {
                return w.upgrade().unwrap_or(Value::Unspecified);
            }
        }
        self.car.borrow().clone()
    }

    /// Read the cdr as a `Value`. See [`car`] for the
    /// tombstone semantics.
    pub fn cdr(&self) -> Value {
        #[cfg(feature = "countable-memory")]
        {
            if let Some(w) = self.cdr_weak.borrow().as_ref() {
                return w.upgrade().unwrap_or(Value::Unspecified);
            }
        }
        self.cdr.borrow().clone()
    }

    /// Replace the car slot with `v`. Clears any weak tombstone
    /// (the new value is unambiguously strong).
    pub fn set_car(&self, v: Value) {
        #[cfg(feature = "countable-memory")]
        {
            self.car_weak.replace(None);
        }
        self.car.replace(v);
    }

    /// Replace the cdr slot with `v`. Clears any weak tombstone.
    pub fn set_cdr(&self, v: Value) {
        #[cfg(feature = "countable-memory")]
        {
            self.cdr_weak.replace(None);
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
    #[cfg(feature = "countable-memory")]
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
        *self.car_weak.borrow_mut() = Some(weak);
        *self.car.borrow_mut() = Value::Unspecified;
        true
    }

    /// Cycle-break action for the cdr slot. See
    /// [`break_car_cycle`] for the `baseline` parameter
    /// convention.
    #[cfg(feature = "countable-memory")]
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
        *self.cdr_weak.borrow_mut() = Some(weak);
        *self.cdr.borrow_mut() = Value::Unspecified;
        true
    }
}

/// Weak counterpart of [`Value`]'s heap-bearing variants, used by
/// the cycle-break tombstone machinery on [`Pair`]. Each variant
/// mirrors a `Value::*` heap variant with the underlying smart-
/// pointer downgraded.
///
/// Only present under `feature = "countable-memory"`; the tracing
/// path doesn't need this because the mark-sweep collector breaks
/// cycles via slot-zeroing rather than weak-pointer storage.
#[cfg(feature = "countable-memory")]
#[derive(Debug, Clone)]
pub enum WeakValue {
    String(cs_gc::Weak<RefCell<String>>),
    Pair(cs_gc::Weak<Pair>),
    Vector(cs_gc::Weak<RefCell<Vec<Value>>>),
    ByteVector(cs_gc::Weak<RefCell<Vec<u8>>>),
    Hashtable(cs_gc::Weak<Hashtable>),
    Port(cs_gc::Weak<Port>),
    Promise(cs_gc::Weak<Promise>),
    Procedure(std::rc::Weak<dyn Procedure>),
}

#[cfg(feature = "countable-memory")]
impl WeakValue {
    /// Construct a `WeakValue` from `v` if `v` carries a heap
    /// pointer. Returns `None` for leaf values (Null, Boolean,
    /// Fixnum, Character, Symbol, Eof, Unspecified, immediate
    /// Numbers) — those don't need weak storage because they
    /// have no allocation to leak.
    pub fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::String(s) => Some(WeakValue::String(cs_gc::Gc::downgrade(s))),
            Value::Pair(p) => Some(WeakValue::Pair(cs_gc::Gc::downgrade(p))),
            Value::Vector(v) => Some(WeakValue::Vector(cs_gc::Gc::downgrade(v))),
            Value::ByteVector(b) => Some(WeakValue::ByteVector(cs_gc::Gc::downgrade(b))),
            Value::Hashtable(h) => Some(WeakValue::Hashtable(cs_gc::Gc::downgrade(h))),
            Value::Port(p) => Some(WeakValue::Port(cs_gc::Gc::downgrade(p))),
            Value::Promise(p) => Some(WeakValue::Promise(cs_gc::Gc::downgrade(p))),
            Value::Procedure(p) => Some(WeakValue::Procedure(Rc::downgrade(p))),
            // Leaf values — no heap allocation to weaken.
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
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

#[cfg(not(feature = "countable-memory"))]
impl cs_gc::Trace for Pair {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        self.car.borrow().trace(marker);
        self.cdr.borrow().trace(marker);
    }
}

#[cfg(feature = "countable-memory")]
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

#[cfg(not(feature = "countable-memory"))]
impl cs_gc::Trace for Hashtable {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        for (k, v) in self.items.borrow().iter() {
            k.trace(marker);
            v.trace(marker);
        }
        if let Some(c) = &self.custom {
            c.hash.trace(marker);
            c.equiv.trace(marker);
        }
    }
}

#[cfg(feature = "countable-memory")]
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

#[cfg(not(feature = "countable-memory"))]
impl cs_gc::Trace for Port {
    fn trace(&self, _marker: &mut cs_gc::Marker) {
        // Leaf: every Port variant holds either chars/bytes/Strings or a
        // file-output buffer. None contain a Value or Gc<T>, so there's
        // nothing to mark transitively.
    }
}

#[cfg(feature = "countable-memory")]
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

#[cfg(not(feature = "countable-memory"))]
impl cs_gc::Trace for Promise {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        match &*self.state.borrow() {
            PromiseState::Pending(v) | PromiseState::Forced(v) => v.trace(marker),
        }
    }
}

#[cfg(feature = "countable-memory")]
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
#[cfg(not(feature = "countable-memory"))]
pub trait Procedure: fmt::Debug + cs_gc::Trace + 'static {
    fn as_any(&self) -> &dyn Any;
    fn name(&self) -> Option<&str> {
        None
    }
}

#[cfg(feature = "countable-memory")]
pub trait Procedure: fmt::Debug + 'static {
    fn as_any(&self) -> &dyn Any;
    fn name(&self) -> Option<&str> {
        None
    }
    /// Visit every `Gc<...>` child this procedure holds in its
    /// closure environment. Default impl is empty — appropriate
    /// for builtins and zero-payload procedure markers. Closures,
    /// continuations, and Parameter override it.
    fn visit_closure_children(&self, _ctx: &mut cs_gc::cycle::CycleVisitor) {}
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
    #[cfg(feature = "countable-memory")]
    fn visit_closure_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        self.cell.borrow().visit_children(ctx);
    }
}

#[cfg(not(feature = "countable-memory"))]
impl cs_gc::Trace for Parameter {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        self.cell.borrow().trace(marker);
    }
}

#[cfg(feature = "countable-memory")]
impl cs_gc::cycle::CycleVisit for Parameter {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        self.cell.borrow().visit_children(ctx);
    }
}

pub fn make_parameter(initial: Value) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(Parameter {
        cell: RefCell::new(initial),
    });
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
    Pair(crate::Gc<Pair>),
    Vector(crate::Gc<RefCell<Vec<Value>>>),
    ByteVector(crate::Gc<RefCell<Vec<u8>>>),
    Procedure(Rc<dyn Procedure>),
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
#[cfg(not(feature = "countable-memory"))]
impl cs_gc::Trace for Value {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        match self {
            // Gc<T>-backed heap variants.
            Value::String(s) => s.trace(marker),
            Value::ByteVector(v) => v.trace(marker),
            Value::Vector(v) => v.trace(marker),
            Value::Pair(p) => p.trace(marker),
            Value::Hashtable(h) => h.trace(marker),
            Value::Port(p) => p.trace(marker),
            Value::Promise(p) => p.trace(marker),
            // Rc-backed (Phase 1 limitation, see doc above).
            Value::Procedure(_) => {}
            // Leaf variants — no heap pointers.
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Number(_) => {}
        }
    }
}

/// Under countable-memory, every heap-bearing Value variant
/// (including Procedure) participates in cycle detection. Pair
/// /Vector/etc. forward through the cs_gc blanket impl for Gc<T>;
/// Procedure forwards through the trait's `visit_closure_children`
/// hook so concrete closures and parameters can enumerate their
/// captured values without each Value match-arm knowing the
/// concrete type.
#[cfg(feature = "countable-memory")]
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
            | Value::Number(_) => {}
        }
    }
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

    /// Total strong-reference count for this Value's underlying
    /// heap allocation, if any. Returns `None` for leaf values
    /// (no allocation to count). Used by `Pair::break_*_cycle`'s
    /// strong-count guard.
    #[cfg(feature = "countable-memory")]
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
