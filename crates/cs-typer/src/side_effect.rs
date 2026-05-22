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

use std::collections::HashMap;
use std::fmt;

use cs_core::{Symbol, SymbolTable};
use cs_ir::CoreExpr;

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

/// The observable side effects of a primitive procedure, by its R6RS /
/// stdlib name. Unknown names are [`EffectSet::PURE`] — the inferencer
/// can't reason about an unclassified callee and stays permissive (matching
/// [`crate::effect::primitive_effect`]); the check pass relies on declared
/// effects for user functions, not on this table being exhaustive.
///
/// Allocation is intentionally *not* reported here — it's a benign effect
/// tracked by the separate [`crate::effect::AllocEffect`] analysis, not an
/// observable side effect relevant to effect declarations, determinism, or
/// capabilities. [`Effect::Agent`] / [`Effect::Audit`] have no primitives
/// yet (they arrive with M09 / M10).
pub fn primitive_side_effects(name: &str) -> EffectSet {
    let one = EffectSet::single;
    match name {
        // Network.
        "http-get" | "http-post" | "http-put" | "http-delete" | "http-head" | "http-patch"
        | "http-request" | "tcp-connect" | "tcp-listen" | "tcp-accept" | "tcp-send"
        | "tcp-recv" | "udp-open" | "udp-send" | "udp-recv" | "dns-resolve" | "dns-lookup"
        | "websocket-connect" | "websocket-send" | "websocket-recv" | "socket-send"
        | "socket-recv" => one(Effect::Net),

        // Filesystem / port I/O.
        "read-file"
        | "write-file"
        | "append-file"
        | "delete-file"
        | "file-exists?"
        | "rename-file"
        | "open-input-file"
        | "open-output-file"
        | "call-with-input-file"
        | "call-with-output-file"
        | "with-input-from-file"
        | "with-output-to-file"
        | "read"
        | "read-line"
        | "read-char"
        | "peek-char"
        | "read-string"
        | "read-u8"
        | "write"
        | "write-char"
        | "write-string"
        | "write-u8"
        | "display"
        | "newline"
        | "flush-output-port"
        | "close-port"
        | "close-input-port"
        | "close-output-port"
        | "directory-list"
        | "make-directory"
        | "current-directory" => one(Effect::Io),

        // Wall clock — non-deterministic.
        "current-time" | "current-jiffy" | "current-second" | "current-date" | "system-time"
        | "time-monotonic" | "time-utc" => one(Effect::WallClock),

        // Randomness — non-deterministic.
        "random"
        | "random-real"
        | "random-integer"
        | "random-fixnum"
        | "make-random-state"
        | "random-source-randomize!" => one(Effect::Random),

        // Mutation of existing state.
        "set-car!" | "set-cdr!" | "vector-set!" | "vector-fill!" | "vector-copy!"
        | "string-set!" | "string-fill!" | "bytevector-u8-set!" | "bytevector-set!"
        | "bytevector-copy!" | "hashtable-set!" | "hashtable-delete!" | "hashtable-update!"
        | "hashtable-clear!" | "set!" => one(Effect::Mutation),

        // Non-local control transfer.
        "raise"
        | "raise-continuable"
        | "error"
        | "assert"
        | "assertion-violation"
        | "error-with-irritants" => one(Effect::Panic),

        _ => EffectSet::PURE,
    }
}

/// Infer the observable [`EffectSet`] of `expr`, bottom-up over the
/// `cs_ir::CoreExpr`. Mirrors the allocation inferencer in
/// [`crate::effect::infer_effect`] but for the side-effect axis.
///
/// `syms` resolves a [`Symbol`] callee to its name so it can be looked up
/// in `declared` (user functions with `#:effects`) or
/// [`primitive_side_effects`]. `declared` is the effect environment the
/// check pass (iter D) builds from all top-level `#:effects` annotations;
/// pass an empty map for standalone inference.
///
/// # Scope (iter B)
///
/// - **A `Lambda` expression is pure**: constructing a closure performs no
///   observable effect. Its *latent* effect (what running the body does)
///   surfaces when the closure is applied — either directly
///   (`App` of a `Lambda`) or via a `declared` lookup when it's bound to a
///   name and called by name.
/// - **Unknown callees are permissive** (`PURE`): a computed callee
///   (`((car fns) …)`) or an unclassified primitive contributes no effect.
///   Full soundness needs an inter-procedural fixpoint, deferred per the
///   M01 brief; the check pass leans on `declared` for user code.
pub fn infer_side_effects(
    expr: &CoreExpr,
    syms: &SymbolTable,
    declared: &HashMap<Symbol, EffectSet>,
) -> EffectSet {
    match expr {
        CoreExpr::Const { .. } | CoreExpr::Ref { .. } => EffectSet::PURE,

        // `set!` mutates; plus whatever the RHS does.
        CoreExpr::Set { value, .. } => {
            EffectSet::single(Effect::Mutation).union(infer_side_effects(value, syms, declared))
        }

        // Constructing a closure is pure; its latent effect surfaces at the
        // call site (see App / the check pass's lambda-body handling).
        CoreExpr::Lambda { .. } => EffectSet::PURE,

        CoreExpr::If {
            cond, then, alt, ..
        } => infer_side_effects(cond, syms, declared)
            .union(infer_side_effects(then, syms, declared))
            .union(infer_side_effects(alt, syms, declared)),

        CoreExpr::Begin { exprs, .. } => exprs.iter().fold(EffectSet::PURE, |acc, e| {
            acc.union(infer_side_effects(e, syms, declared))
        }),

        CoreExpr::Letrec { bindings, body, .. } => {
            let mut acc = infer_side_effects(body, syms, declared);
            for (_, rhs) in bindings {
                acc = acc.union(infer_side_effects(rhs, syms, declared));
            }
            acc
        }

        CoreExpr::App { func, args, .. } => {
            // Evaluating the callee + args.
            let mut acc = infer_side_effects(func, syms, declared);
            for arg in args {
                acc = acc.union(infer_side_effects(arg, syms, declared));
            }
            // Plus the callee's latent effect (what calling it does).
            acc = acc.union(callee_latent_effect(func, syms, declared));
            acc
        }
    }
}

/// The latent effect of applying `func`: a named callee resolves through
/// `declared` (user `#:effects`) then [`primitive_side_effects`]; an
/// immediately-invoked lambda contributes its body's effect; any other
/// (computed) callee is permissive `PURE`.
fn callee_latent_effect(
    func: &CoreExpr,
    syms: &SymbolTable,
    declared: &HashMap<Symbol, EffectSet>,
) -> EffectSet {
    match func {
        CoreExpr::Ref { name, .. } => declared
            .get(name)
            .copied()
            .unwrap_or_else(|| primitive_side_effects(syms.name(*name))),
        CoreExpr::Lambda { body, .. } => infer_side_effects(body, syms, declared),
        _ => EffectSet::PURE,
    }
}

/// The latent effect of a definition's value — the effects performed when
/// the binding is *used*. For a `(lambda …)` value that's the body's
/// effect (the function runs when called); for any other value it's the
/// effect of evaluating it. This is what the check pass compares against a
/// `#:effects` declaration.
pub fn definition_body_effects(
    value: &CoreExpr,
    syms: &SymbolTable,
    declared: &HashMap<Symbol, EffectSet>,
) -> EffectSet {
    match value {
        CoreExpr::Lambda { body, .. } => infer_side_effects(body, syms, declared),
        other => infer_side_effects(other, syms, declared),
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

    // === iter B: inferencer + builtin table ===

    use cs_core::Value;
    use cs_diag::Span;
    use cs_ir::Params;
    use std::rc::Rc;

    fn cnst(v: i64) -> CoreExpr {
        CoreExpr::Const {
            value: Value::fixnum(v),
            span: Span::DUMMY,
        }
    }
    fn rf(sym: Symbol) -> CoreExpr {
        CoreExpr::Ref {
            name: sym,
            span: Span::DUMMY,
        }
    }
    fn app(func: CoreExpr, args: Vec<CoreExpr>) -> CoreExpr {
        CoreExpr::App {
            func: Rc::new(func),
            args,
            span: Span::DUMMY,
        }
    }
    fn lam(params: Vec<Symbol>, body: CoreExpr) -> CoreExpr {
        CoreExpr::Lambda {
            params: Params::fixed(params),
            body: Rc::new(body),
            span: Span::DUMMY,
        }
    }
    fn set(name: Symbol, value: CoreExpr) -> CoreExpr {
        CoreExpr::Set {
            name,
            value: Rc::new(value),
            span: Span::DUMMY,
        }
    }
    fn no_decls() -> HashMap<Symbol, EffectSet> {
        HashMap::new()
    }

    #[test]
    fn primitive_table_classifies_each_axis() {
        assert_eq!(
            primitive_side_effects("http-get"),
            EffectSet::single(Effect::Net)
        );
        assert_eq!(
            primitive_side_effects("read-file"),
            EffectSet::single(Effect::Io)
        );
        assert_eq!(
            primitive_side_effects("current-time"),
            EffectSet::single(Effect::WallClock)
        );
        assert_eq!(
            primitive_side_effects("random"),
            EffectSet::single(Effect::Random)
        );
        assert_eq!(
            primitive_side_effects("vector-set!"),
            EffectSet::single(Effect::Mutation)
        );
        assert_eq!(
            primitive_side_effects("raise"),
            EffectSet::single(Effect::Panic)
        );
        // Pure ops and benign allocation are not observable side effects.
        assert!(primitive_side_effects("+").is_pure());
        assert!(primitive_side_effects("cons").is_pure());
        assert!(primitive_side_effects("car").is_pure());
        assert!(primitive_side_effects("totally-unknown").is_pure());
    }

    #[test]
    fn pure_expression_has_empty_effect_set() {
        let mut syms = SymbolTable::new();
        let plus = syms.intern("+");
        let e = app(rf(plus), vec![cnst(1), cnst(2)]);
        assert!(infer_side_effects(&e, &syms, &no_decls()).is_pure());
    }

    #[test]
    fn read_file_call_has_io() {
        let mut syms = SymbolTable::new();
        let rdf = syms.intern("read-file");
        let p = syms.intern("p");
        let e = app(rf(rdf), vec![rf(p)]);
        assert_eq!(
            infer_side_effects(&e, &syms, &no_decls()),
            EffectSet::single(Effect::Io)
        );
    }

    #[test]
    fn call_to_declared_binding_uses_its_effects() {
        let mut syms = SymbolTable::new();
        let g = syms.intern("g");
        let mut decls = no_decls();
        decls.insert(g, EffectSet::from_effects([Effect::Net, Effect::Audit]));
        let e = app(rf(g), vec![]);
        assert_eq!(
            infer_side_effects(&e, &syms, &decls),
            EffectSet::from_effects([Effect::Net, Effect::Audit])
        );
    }

    #[test]
    fn set_node_is_mutation() {
        let mut syms = SymbolTable::new();
        let x = syms.intern("x");
        let e = set(x, cnst(1));
        assert_eq!(
            infer_side_effects(&e, &syms, &no_decls()),
            EffectSet::single(Effect::Mutation)
        );
    }

    #[test]
    fn if_unions_branch_effects() {
        let mut syms = SymbolTable::new();
        let hget = syms.intern("http-get");
        let rdf = syms.intern("read-file");
        let (c, u, p) = (syms.intern("c"), syms.intern("u"), syms.intern("p"));
        let e = CoreExpr::If {
            cond: Rc::new(rf(c)),
            then: Rc::new(app(rf(hget), vec![rf(u)])),
            alt: Rc::new(app(rf(rdf), vec![rf(p)])),
            span: Span::DUMMY,
        };
        assert_eq!(
            infer_side_effects(&e, &syms, &no_decls()),
            EffectSet::from_effects([Effect::Net, Effect::Io])
        );
    }

    #[test]
    fn lambda_is_pure_latent_effect_surfaces_at_call() {
        let mut syms = SymbolTable::new();
        let hget = syms.intern("http-get");
        let u = syms.intern("u");
        // Defining the closure is pure.
        let l = lam(vec![u], app(rf(hget), vec![rf(u)]));
        assert!(
            infer_side_effects(&l, &syms, &no_decls()).is_pure(),
            "constructing a closure performs no observable effect"
        );
        // Immediately invoking it surfaces the body's {net}.
        let called = app(lam(vec![u], app(rf(hget), vec![rf(u)])), vec![cnst(0)]);
        assert_eq!(
            infer_side_effects(&called, &syms, &no_decls()),
            EffectSet::single(Effect::Net)
        );
        // definition_body_effects gives the latent {net} the #:effects
        // check (iter D) compares against the declaration.
        let l2 = lam(vec![u], app(rf(hget), vec![rf(u)]));
        assert_eq!(
            definition_body_effects(&l2, &syms, &no_decls()),
            EffectSet::single(Effect::Net)
        );
    }

    #[test]
    fn begin_unions_subexpr_effects() {
        let mut syms = SymbolTable::new();
        let rnd = syms.intern("random");
        let disp = syms.intern("display");
        let e = CoreExpr::Begin {
            exprs: vec![app(rf(rnd), vec![]), app(rf(disp), vec![cnst(1)])],
            span: Span::DUMMY,
        };
        assert_eq!(
            infer_side_effects(&e, &syms, &no_decls()),
            EffectSet::from_effects([Effect::Random, Effect::Io])
        );
    }
}
