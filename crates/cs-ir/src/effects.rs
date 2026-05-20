//! Effect sets for the core IR.
//!
//! An effect set is a small bitmap of the side-effect kinds a piece of
//! Scheme code may perform. Effects flow bottom-up: a function's effect
//! set is the union of its body's effect-bearing operations.
//!
//! The spec lives at `docs/research/sdk_spec/language.md` § effect
//! annotations (M01) and at `docs/research/sdk_spec/tasks/M01-foundations.md`.
//! `EffectSet` is the type promised to `cs-rir` consumers and the
//! effect-check pass added in iter D of M01.
//!
//! Iter A.1 (this file) ships the type alone — no `CoreExpr` field yet,
//! no inference pass yet. Subsequent iters wire it into IR nodes (A.2),
//! the inference pass (B), the `#:effects` annotation form (C), and
//! the static checker (D).

use std::fmt;

/// One side-effect kind. Stable variant ordering — values are bit
/// positions, so reordering or inserting in the middle breaks
/// `EffectSet`'s on-disk and in-memory bitmap layout.
///
/// Effects deliberately stop short of being a full type system:
/// `alloc` is here because GC pressure matters; `panic` is here so
/// non-throwing helpers can be statically distinguished; `agent`
/// and `audit` exist because they gate distinct policy decisions
/// at the agentic + capability layers (see `cs-cap` in M10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Effect {
    /// Heap allocation: `cons`, `make-vector`, closure creation.
    Alloc = 0,
    /// Filesystem / stdin / stdout / ports.
    Io = 1,
    /// Sockets, HTTP, gRPC, anything that crosses the host network.
    Net = 2,
    /// Wall-clock time: `current-time`, `current-seconds`. Forbidden
    /// in workflows and replicated state machines because it breaks
    /// deterministic replay.
    WallClock = 3,
    /// `(random)` and friends — system PRNG. Forbidden in workflows
    /// and replicated state machines for the same reason.
    Random = 4,
    /// Set! on a free variable, vector-set! on a closed-over vector,
    /// or anything else that mutates state reachable beyond the
    /// current activation. (Local mutation of a freshly-allocated
    /// value doesn't count.)
    Mutation = 5,
    /// `error`, `raise`, `assert`. Always permitted; the marker lets
    /// "this is total" callers reason about termination.
    Panic = 6,
    /// LLM model calls, tool dispatches. Workflows must wrap these
    /// in activities (which are themselves effect-isolated).
    Agent = 7,
    /// Recordable for compliance — every tool tagged `audit` writes
    /// to the audit log regardless of policy decision (see
    /// `cs-cap::AuditLog`).
    Audit = 8,
}

impl Effect {
    /// The lookup name for textual parses of `#:effects '(net io …)`.
    pub const fn name(self) -> &'static str {
        match self {
            Effect::Alloc => "alloc",
            Effect::Io => "io",
            Effect::Net => "net",
            Effect::WallClock => "wall-clock",
            Effect::Random => "random",
            Effect::Mutation => "mutation",
            Effect::Panic => "panic",
            Effect::Agent => "agent",
            Effect::Audit => "audit",
        }
    }

    /// Parse the textual name. Used by the `#:effects` annotation
    /// parser (M01 iter C).
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "alloc" => Effect::Alloc,
            "io" => Effect::Io,
            "net" => Effect::Net,
            "wall-clock" => Effect::WallClock,
            "random" => Effect::Random,
            "mutation" => Effect::Mutation,
            "panic" => Effect::Panic,
            "agent" => Effect::Agent,
            "audit" => Effect::Audit,
            _ => return None,
        })
    }

    const fn mask(self) -> u16 {
        1u16 << (self as u8)
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A bitmap of effects.
///
/// `Default` is the empty set (pure). All operations are constant-time
/// over a `u16`. The bitmap representation lets the effect-infer pass
/// fold large IR subtrees without heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EffectSet(u16);

impl EffectSet {
    pub const fn empty() -> Self {
        EffectSet(0)
    }

    /// Singleton — `EffectSet::of(Effect::Net)`.
    pub const fn of(e: Effect) -> Self {
        EffectSet(e.mask())
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, e: Effect) -> bool {
        (self.0 & e.mask()) != 0
    }

    /// Set union — `a | b`.
    pub const fn union(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 | other.0)
    }

    /// Set intersection — `a & b`.
    pub const fn intersect(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 & other.0)
    }

    /// Set difference — `a - b`. Effects in `self` not in `other`.
    pub const fn difference(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 & !other.0)
    }

    /// Subset test — `self ⊆ other`.
    pub const fn is_subset_of(self, other: EffectSet) -> bool {
        self.difference(other).is_empty()
    }

    /// Insert in place. Used by the inference pass when accumulating
    /// effects over a sequence of sub-expressions.
    pub fn insert(&mut self, e: Effect) {
        self.0 |= e.mask();
    }

    /// Iterate the effects in stable order (variant order).
    pub fn iter(self) -> impl Iterator<Item = Effect> {
        const ALL: [Effect; 9] = [
            Effect::Alloc,
            Effect::Io,
            Effect::Net,
            Effect::WallClock,
            Effect::Random,
            Effect::Mutation,
            Effect::Panic,
            Effect::Agent,
            Effect::Audit,
        ];
        ALL.into_iter().filter(move |e| self.contains(*e))
    }
}

impl fmt::Display for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        let mut first = true;
        for e in self.iter() {
            if !first {
                f.write_str(" ")?;
            }
            first = false;
            write!(f, "{}", e)?;
        }
        f.write_str("}")
    }
}

impl std::ops::BitOr for EffectSet {
    type Output = EffectSet;
    fn bitor(self, rhs: EffectSet) -> EffectSet {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for EffectSet {
    fn bitor_assign(&mut self, rhs: EffectSet) {
        self.0 |= rhs.0;
    }
}

impl FromIterator<Effect> for EffectSet {
    fn from_iter<I: IntoIterator<Item = Effect>>(iter: I) -> Self {
        let mut s = EffectSet::empty();
        for e in iter {
            s.insert(e);
        }
        s
    }
}

/// The set of effects forbidden inside a `(define-workflow …)` body
/// (M08) and a `#:state-machine` body for `(define-replicated-actor …)`
/// (M06). Both contexts require deterministic re-execution; any of
/// these effects breaks that.
pub const WORKFLOW_FORBIDDEN: EffectSet = EffectSet(
    Effect::Net.mask() | Effect::Io.mask() | Effect::WallClock.mask() | Effect::Random.mask(),
);

/// Symbol → declared-effect-set table populated by the macro expander
/// as it walks `(define name #:effects '(…) …)` forms. The effect-check
/// pass (M01 iter D) and the workflow expander (M08) consume this table
/// to reject bodies that exceed their declared bounds or include
/// forbidden effects.
///
/// Keyed by the top-level binding's `Symbol`. Anonymous lambdas have
/// no entry (they inherit their enclosing binding's annotation when
/// one exists; otherwise their effect set is whatever inference
/// computes).
///
/// The table travels with the `Expander` for the duration of a single
/// expansion session and is exposed to consumers (cs-runtime, the
/// future cs-workflow expander pass in M08) via a getter so they do
/// not need to depend on cs-expand internals.
#[derive(Debug, Clone, Default)]
pub struct EffectAnnotations {
    by_symbol: std::collections::HashMap<cs_core::Symbol, EffectSet>,
}

impl EffectAnnotations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a declared effect set for a top-level binding. Returns the
    /// previously declared set, if any, so the caller can detect
    /// duplicate declarations.
    pub fn declare(&mut self, name: cs_core::Symbol, effects: EffectSet) -> Option<EffectSet> {
        self.by_symbol.insert(name, effects)
    }

    /// Look up a declared effect set. `None` means the user never wrote
    /// `#:effects` on this binding — its declared bound is implicit
    /// "anything inference computes."
    pub fn get(&self, name: cs_core::Symbol) -> Option<EffectSet> {
        self.by_symbol.get(&name).copied()
    }

    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (cs_core::Symbol, EffectSet)> + '_ {
        self.by_symbol.iter().map(|(k, v)| (*k, *v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default_and_empty() {
        assert!(EffectSet::default().is_empty());
        assert!(EffectSet::empty().is_empty());
        assert!(!EffectSet::of(Effect::Net).is_empty());
    }

    #[test]
    fn singleton_contains_only_its_effect() {
        let s = EffectSet::of(Effect::Net);
        assert!(s.contains(Effect::Net));
        assert!(!s.contains(Effect::Io));
        assert!(!s.contains(Effect::Random));
    }

    #[test]
    fn union_is_set_union() {
        let a = EffectSet::of(Effect::Net);
        let b = EffectSet::of(Effect::Io);
        let u = a | b;
        assert!(u.contains(Effect::Net));
        assert!(u.contains(Effect::Io));
        assert!(!u.contains(Effect::Random));
    }

    #[test]
    fn subset_arithmetic_is_right() {
        let a = EffectSet::of(Effect::Net);
        let b = a | EffectSet::of(Effect::Io);
        assert!(a.is_subset_of(b));
        assert!(!b.is_subset_of(a));
        assert!(a.is_subset_of(a));
        assert!(EffectSet::empty().is_subset_of(a));
    }

    #[test]
    fn difference_drops_intersected_effects() {
        let a: EffectSet = [Effect::Net, Effect::Io, Effect::Random]
            .into_iter()
            .collect();
        let b = EffectSet::of(Effect::Io);
        let d = a.difference(b);
        assert!(d.contains(Effect::Net));
        assert!(!d.contains(Effect::Io));
        assert!(d.contains(Effect::Random));
    }

    #[test]
    fn iter_is_stable_order() {
        let s: EffectSet = [Effect::Random, Effect::Alloc, Effect::Net]
            .into_iter()
            .collect();
        let v: Vec<Effect> = s.iter().collect();
        // variant order — Alloc(0) < Net(2) < Random(4).
        assert_eq!(v, vec![Effect::Alloc, Effect::Net, Effect::Random]);
    }

    #[test]
    fn display_renders_set_brace() {
        let s: EffectSet = [Effect::Net, Effect::Io].into_iter().collect();
        assert_eq!(format!("{}", s), "{io net}");
        assert_eq!(format!("{}", EffectSet::empty()), "{}");
    }

    #[test]
    fn from_name_round_trips() {
        for e in [
            Effect::Alloc,
            Effect::Io,
            Effect::Net,
            Effect::WallClock,
            Effect::Random,
            Effect::Mutation,
            Effect::Panic,
            Effect::Agent,
            Effect::Audit,
        ] {
            assert_eq!(Effect::from_name(e.name()), Some(e));
        }
        assert_eq!(Effect::from_name("not-an-effect"), None);
    }

    #[test]
    fn workflow_forbidden_set_matches_spec() {
        // M01 spec: WORKFLOW_FORBIDDEN = {net, io, wall-clock, random}.
        assert!(WORKFLOW_FORBIDDEN.contains(Effect::Net));
        assert!(WORKFLOW_FORBIDDEN.contains(Effect::Io));
        assert!(WORKFLOW_FORBIDDEN.contains(Effect::WallClock));
        assert!(WORKFLOW_FORBIDDEN.contains(Effect::Random));
        // …and nothing else.
        assert!(!WORKFLOW_FORBIDDEN.contains(Effect::Alloc));
        assert!(!WORKFLOW_FORBIDDEN.contains(Effect::Mutation));
        assert!(!WORKFLOW_FORBIDDEN.contains(Effect::Panic));
        assert!(!WORKFLOW_FORBIDDEN.contains(Effect::Agent));
        assert!(!WORKFLOW_FORBIDDEN.contains(Effect::Audit));
    }
}
