//! Observable side-effect inference (SDK milestone M01).
//!
//! Distinct from the *allocation* effect analysis in [`crate::effect`]
//! (`AllocEffect` — what the memory allocator must do). This module tracks
//! the **observable side effects** a computation may perform — network,
//! I/O, wall-clock reads, randomness, mutation, etc. — so the language can:
//!
//! - let a definition declare its allowed effects
//!   (`(define f #:effects '(net io) …)`) and reject a body that performs
//!   an undeclared one (M01 iter D);
//! - statically forbid non-deterministic effects (net / wall-clock /
//!   random) inside durable workflow bodies (M08) so replay is sound;
//! - gate privileged effects (net / agent / audit) behind capabilities
//!   (M10).
//!
//! # Iter A (this commit)
//!
//! Ships the [`Effect`] enum + the [`EffectSet`] bitset and its lattice
//! algebra (union / subset / difference). The bottom-up inferencer over
//! `cs_ir::CoreExpr` and the builtin effect table land in iter B; the
//! `#:effects` surface syntax in iter C; the check pass in iter D.
//!
//! # Lattice
//!
//! [`EffectSet`] is the powerset of [`Effect`] under union — a bounded
//! join-semilattice with bottom [`EffectSet::PURE`] (the empty set) and
//! top [`EffectSet::ALL`]. "Body conforms to declaration" is the subset
//! test `inferred ⊆ declared` ([`EffectSet::is_subset`]).

use std::fmt;

/// A single observable side effect a computation may perform.
///
/// Ordered to match the SDK roadmap's M01 brief. `Pure` is *not* a variant
/// — purity is the empty [`EffectSet`] ([`EffectSet::PURE`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Effect {
    /// Allocates heap memory (coarse flag; the fine-grained escape
    /// analysis lives in [`crate::effect::AllocEffect`]). Allowed inside
    /// workflows and state migrations.
    Alloc = 0,
    /// Filesystem / port I/O (`read-file`, `display` to a file port, …).
    Io,
    /// Network access (`http-get`, socket sends, remote actor `send`).
    Net,
    /// Reads the wall clock (`current-time`, `current-jiffy`) — a source
    /// of non-determinism forbidden in workflow bodies.
    WallClock,
    /// Randomness (`random`, `random-real`) — non-deterministic.
    Random,
    /// Mutation of existing state (`set!`, `vector-set!`, `set-car!`, …).
    Mutation,
    /// May raise / escape via a non-local control transfer (`raise`,
    /// `error`, `assert`).
    Panic,
    /// Invokes an agentic operation (model call, tool call) — privileged.
    Agent,
    /// Writes the audit log — privileged, never elided.
    Audit,
}

impl Effect {
    /// Every effect, in declaration order. Iteration over an
    /// [`EffectSet`] yields effects in this order.
    pub const ALL: [Effect; 9] = [
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

    /// Bit position of this effect within an [`EffectSet`].
    const fn bit(self) -> u16 {
        1u16 << (self as u16)
    }

    /// The effect's canonical Scheme-symbol name (as written in an
    /// `#:effects '(…)` list).
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

    /// Parse an effect from its Scheme-symbol name. `None` for an
    /// unrecognized name (the `#:effects` parser turns this into a
    /// diagnostic in iter C).
    pub fn from_name(name: &str) -> Option<Effect> {
        Effect::ALL.into_iter().find(|e| e.name() == name)
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of observable side effects, represented as a bitset.
///
/// Compose with [`EffectSet::union`] (the lattice join). "Body conforms to
/// declaration" is `inferred.is_subset(declared)`. [`EffectSet::PURE`] (the
/// empty set) is the bottom element.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EffectSet(u16);

impl EffectSet {
    /// The empty effect set — a pure computation. Bottom of the lattice
    /// and the identity for [`Self::union`].
    pub const PURE: EffectSet = EffectSet(0);

    /// Every effect set — top of the lattice. Mostly useful as the
    /// conservative effect of an unanalyzable callee.
    pub const ALL: EffectSet = EffectSet((1u16 << Effect::ALL.len()) - 1);

    /// A set containing exactly `e`.
    pub const fn single(e: Effect) -> EffectSet {
        EffectSet(e.bit())
    }

    /// Build a set from an iterator of effects.
    pub fn from_effects(effects: impl IntoIterator<Item = Effect>) -> EffectSet {
        effects
            .into_iter()
            .fold(EffectSet::PURE, |acc, e| acc.with(e))
    }

    /// `true` if no effects — the computation is pure.
    pub const fn is_pure(self) -> bool {
        self.0 == 0
    }

    /// `true` if `e` is in the set.
    pub const fn contains(self, e: Effect) -> bool {
        self.0 & e.bit() != 0
    }

    /// Add `e` (in place).
    pub fn insert(&mut self, e: Effect) {
        self.0 |= e.bit();
    }

    /// This set plus `e` (by value).
    pub const fn with(self, e: Effect) -> EffectSet {
        EffectSet(self.0 | e.bit())
    }

    /// Lattice join — the union of two effect sets.
    pub const fn union(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 | other.0)
    }

    /// The effects in both sets.
    pub const fn intersection(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 & other.0)
    }

    /// `self \ other` — the effects in `self` not in `other`. Used by the
    /// check pass to report exactly which effects were undeclared.
    pub const fn difference(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 & !other.0)
    }

    /// `true` if every effect in `self` is also in `other` — i.e.
    /// `self ⊆ other`. This is the conformance test: an inferred set must
    /// be a subset of the declared set.
    pub const fn is_subset(self, other: EffectSet) -> bool {
        self.0 & !other.0 == 0
    }

    /// Number of effects in the set.
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// `true` if the set is empty (alias of [`Self::is_pure`], for the
    /// clippy `len`-without-`is_empty` lint).
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Iterate the effects present, in [`Effect::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = Effect> {
        Effect::ALL.into_iter().filter(move |&e| self.contains(e))
    }
}

impl FromIterator<Effect> for EffectSet {
    fn from_iter<I: IntoIterator<Item = Effect>>(iter: I) -> Self {
        EffectSet::from_effects(iter)
    }
}

impl fmt::Debug for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (i, e) in self.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{e}")?;
        }
        f.write_str("}")
    }
}

impl fmt::Display for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_round_trips() {
        for e in Effect::ALL {
            assert_eq!(Effect::from_name(e.name()), Some(e), "{e}");
        }
        assert_eq!(Effect::from_name("not-an-effect"), None);
        // The two-word name is hyphenated.
        assert_eq!(Effect::WallClock.name(), "wall-clock");
    }

    #[test]
    fn pure_is_empty_bottom() {
        assert!(EffectSet::PURE.is_pure());
        assert!(EffectSet::PURE.is_empty());
        assert_eq!(EffectSet::PURE.len(), 0);
        // Identity for union.
        let e = EffectSet::single(Effect::Net);
        assert_eq!(EffectSet::PURE.union(e), e);
        assert_eq!(e.union(EffectSet::PURE), e);
    }

    #[test]
    fn all_contains_every_effect() {
        for e in Effect::ALL {
            assert!(EffectSet::ALL.contains(e), "ALL missing {e}");
        }
        assert_eq!(EffectSet::ALL.len(), Effect::ALL.len() as u32);
    }

    #[test]
    fn single_and_contains() {
        let net = EffectSet::single(Effect::Net);
        assert!(net.contains(Effect::Net));
        assert!(!net.contains(Effect::Io));
        assert_eq!(net.len(), 1);
    }

    #[test]
    fn union_is_idempotent_commutative_associative() {
        let a = EffectSet::from_effects([Effect::Net, Effect::Io]);
        let b = EffectSet::from_effects([Effect::Io, Effect::Random]);
        let c = EffectSet::single(Effect::Mutation);
        assert_eq!(a.union(a), a, "idempotent");
        assert_eq!(a.union(b), b.union(a), "commutative");
        assert_eq!(a.union(b).union(c), a.union(b.union(c)), "associative");
    }

    #[test]
    fn subset_is_the_conformance_test() {
        let declared = EffectSet::from_effects([Effect::Net, Effect::Io, Effect::Audit]);
        // inferred ⊆ declared → conforms
        assert!(EffectSet::single(Effect::Net).is_subset(declared));
        assert!(EffectSet::from_effects([Effect::Net, Effect::Io]).is_subset(declared));
        assert!(EffectSet::PURE.is_subset(declared));
        assert!(declared.is_subset(declared));
        // a non-declared effect → not a subset
        assert!(!EffectSet::single(Effect::Random).is_subset(declared));
        assert!(!EffectSet::from_effects([Effect::Net, Effect::Random]).is_subset(declared));
    }

    #[test]
    fn difference_reports_undeclared_effects() {
        let inferred = EffectSet::from_effects([Effect::Net, Effect::Io, Effect::Random]);
        let declared = EffectSet::from_effects([Effect::Io]);
        // The check pass reports `inferred \ declared` = {net, random}.
        let undeclared = inferred.difference(declared);
        assert_eq!(
            undeclared,
            EffectSet::from_effects([Effect::Net, Effect::Random])
        );
        assert!(undeclared.contains(Effect::Net));
        assert!(undeclared.contains(Effect::Random));
        assert!(!undeclared.contains(Effect::Io));
    }

    #[test]
    fn intersection() {
        let a = EffectSet::from_effects([Effect::Net, Effect::Io]);
        let b = EffectSet::from_effects([Effect::Io, Effect::Random]);
        assert_eq!(a.intersection(b), EffectSet::single(Effect::Io));
    }

    #[test]
    fn iter_yields_declaration_order() {
        let s = EffectSet::from_effects([Effect::Audit, Effect::Net, Effect::Io]);
        let got: Vec<Effect> = s.iter().collect();
        // Effect::ALL order: …, Io, Net, …, Audit
        assert_eq!(got, vec![Effect::Io, Effect::Net, Effect::Audit]);
    }

    #[test]
    fn from_iterator_trait() {
        let s: EffectSet = [Effect::Net, Effect::Io].into_iter().collect();
        assert_eq!(s, EffectSet::from_effects([Effect::Net, Effect::Io]));
    }

    #[test]
    fn debug_format_is_stable() {
        assert_eq!(format!("{:?}", EffectSet::PURE), "{}");
        assert_eq!(
            format!("{:?}", EffectSet::from_effects([Effect::Net, Effect::Io])),
            "{io, net}"
        );
        assert_eq!(
            format!("{}", EffectSet::single(Effect::WallClock)),
            "{wall-clock}"
        );
    }

    #[test]
    fn migration_allowed_set_subset_check() {
        // M01 iter E: a state-migration thunk must be ⊆ {alloc, mutation}.
        let migration_allowed = EffectSet::from_effects([Effect::Alloc, Effect::Mutation]);
        assert!(EffectSet::from_effects([Effect::Alloc]).is_subset(migration_allowed));
        assert!(EffectSet::single(Effect::Mutation).is_subset(migration_allowed));
        // wall-clock in a migration is rejected.
        assert!(!EffectSet::single(Effect::WallClock).is_subset(migration_allowed));
    }
}
