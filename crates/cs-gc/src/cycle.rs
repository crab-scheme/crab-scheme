//! Synchronous local cycle detection for the `countable-memory`
//! representation.
//!
//! Reference counting reclaims acyclic graphs deterministically.
//! Cycles (`(set-cdr! x x)`, vector self-loops, hashtable value
//! self-loops, mutually-set! closures) would otherwise leak.
//! This module provides a small bounded-DFS detector that
//! mutation primitives invoke after operations that could close a
//! cycle.
//!
//! # Algorithm
//!
//! Starting at the freshly-mutated root, walk the transitive
//! `Gc<...>` children via the [`CycleVisit`] trait. Identity is
//! tracked by [`Gc::as_addr`] in a per-call `HashSet<usize>`. If
//! traversal revisits the root's address, a cycle exists — the
//! caller's `break_at` action runs to flip one storage edge from
//! `Strong(Value)` to `Weak(WeakValue)` so the cycle becomes
//! refcount-reclaimable (see `.spec-workflow/specs/countable-memory/
//! design.md` §"Component 5").
//!
//! A configurable per-call node-visit limit (default 10_000; set
//! per thread via [`set_limit`]) bounds traversal cost on
//! pathological graphs. On limit-hit the detector returns `None`
//! and the call site applies a conservative `Weak` downgrade.
//!
//! # Stack discipline
//!
//! Descent runs through the host stack via nested
//! [`CycleVisit::visit_children`] calls. Recursion depth is
//! bounded by the visit-count limit (the [`CycleVisitor`]
//! refuses to descend past the limit), so even adversarial deep
//! chains terminate without overflowing the host stack at the
//! default limit of 10_000.

use std::cell::{Cell, RefCell};

use rustc_hash::FxHashSet;

use crate::Gc;

/// Implemented by any type whose values can hold `Gc<...>` back-
/// edges that could close a cycle.
///
/// Per-type impls call `ctx.visit(child)` for each direct
/// `Gc<...>` child of `self`. When `visit` returns `true`, the
/// impl descends into that child via `child.visit_children(ctx)`;
/// when it returns `false`, the impl skips descent (the child is
/// already-visited, or the detector has reached its termination
/// state).
///
/// Leaf types (no `Gc<T>` children) provide an empty impl. The
/// blanket impl for [`Gc<T>`] forwards to the pointee's
/// `visit_children`, so call sites use either receiver
/// interchangeably.
pub trait CycleVisit {
    fn visit_children(&self, ctx: &mut CycleVisitor);
}

/// Per-type cycle-break dispatch for the layer-4 sweep
/// (Gap C-3). When the candidate registry identifies a value
/// that participates in a residual cycle, the sweep calls
/// `try_break_cycle` to demote one outgoing strong slot to
/// `Weak`. Returns `true` if a slot was successfully demoted;
/// `false` if the type has no safe break action.
///
/// Implementations live in `cs-core` (`impl BreakCycle for
/// Pair` is the only one shipped today; Vector / Hashtable
/// stay at the default no-op — the cycle counter still
/// fires for them, the layer-4 sweep just doesn't reclaim).
///
/// Required by every `T` registered as a cycle candidate
/// (i.e., every `T: AnyWeak`). The blanket default keeps
/// existing `CycleVisit` types compatible with `AnyWeak`
/// without code changes — cs-core only overrides for Pair.
pub trait BreakCycle {
    fn try_break_cycle(&self) -> bool {
        false
    }
}

// Blanket no-op impls for the std container types cs-core
// uses as `Gc<RefCell<...>>` payloads. Putting them here
// (where `BreakCycle` is local) satisfies the orphan rule
// — cs-core can't impl a foreign trait for a foreign type.
// Per-type cycle-break dispatch for Vector / String /
// ByteVector is a future iter; for now the cycle counter
// fires but the layer-4 sweep can't reclaim them.
impl<T: ?Sized> BreakCycle for std::cell::RefCell<T> {}

// Leaf-primitive impls so test code and toy embedders can
// use `Gc<i64>` / `Gc<String>` / etc. without needing a
// per-type BreakCycle impl. Cycles can't form through these
// types (no Gc back-edges) so the default no-op is correct.
impl BreakCycle for i64 {}
impl BreakCycle for u64 {}
impl BreakCycle for i32 {}
impl BreakCycle for u32 {}
impl BreakCycle for f64 {}
impl BreakCycle for bool {}
impl BreakCycle for char {}
impl BreakCycle for String {}
impl BreakCycle for &'static str {}

/// Opaque cycle witness. Returned by [`cycle_check`] when a cycle
/// is found; the contained address is the root that closed the
/// cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CyclePath {
    /// Address of the root slot whose subgraph contains the
    /// cycle.
    pub root: usize,
}

/// Context threaded through a [`cycle_check`] traversal. Tracks
/// visited addresses, termination state, and the per-call visit
/// budget.
///
/// Per-type [`CycleVisit`] impls construct the visitor only
/// indirectly through [`cycle_check`] / [`check_and_break`]; they
/// interact with it via [`CycleVisitor::visit`] (consulted before
/// descending into a child) and [`CycleVisitor::done`] (consulted
/// before processing further siblings).
pub struct CycleVisitor {
    visited: FxHashSet<usize>,
    found: bool,
    over_limit: bool,
    root_addr: usize,
    limit: usize,
    /// Iter 7.1.x.z — flips to true when a `CycleVisit` impl
    /// uses [`root_addr`] to identify a back-edge and
    /// successfully demotes its outgoing slot to a `Weak`
    /// tombstone. Used by the runtime's break-action callback
    /// to skip a redundant root-level demote attempt.
    broken: bool,
}

impl CycleVisitor {
    /// Consult the visitor about a child slot. Returns `true`
    /// when the caller should descend into `child` via
    /// `child.visit_children(self)`; `false` when the caller
    /// should skip descent (already visited, cycle reached, or
    /// limit hit).
    ///
    /// Detects a cycle when `child`'s slot address equals the
    /// detector's root address — at which point `done()` will
    /// also start returning `true` and the rest of the traversal
    /// unwinds cheaply.
    pub fn visit<T>(&mut self, child: &Gc<T>) -> bool
    where
        T: CycleVisit + 'static,
    {
        if self.found || self.over_limit {
            return false;
        }
        let addr = Gc::as_addr(child);
        if addr == self.root_addr {
            self.found = true;
            return false;
        }
        if !self.visited.insert(addr) {
            // Shared DAG node we've already enumerated; skip
            // re-descent. Sibling traversal continues.
            return false;
        }
        if self.visited.len() > self.limit {
            self.over_limit = true;
            return false;
        }
        true
    }

    /// `true` once the detector has either found the cycle or
    /// exceeded its visit budget. [`CycleVisit`] impls should
    /// `return` early when this flips so deep recursions unwind
    /// without further work.
    pub fn done(&self) -> bool {
        self.found || self.over_limit
    }

    /// Iter 7.1.x.z — the address of the root the detector
    /// was started on. [`CycleVisit`] impls use this to
    /// identify back-edges: when a child's address equals
    /// `root_addr`, the calling node IS the back-edge source
    /// and is in a position to demote its outgoing slot
    /// (`car` or `cdr`) to a `Weak` tombstone.
    pub fn root_addr(&self) -> usize {
        self.root_addr
    }

    /// Iter 7.1.x.z — signal that a `CycleVisit` impl
    /// successfully demoted a cycle edge to `Weak`. The
    /// runtime's `check_and_break` invocation reads this via
    /// [`is_broken`] to decide whether the explicit root-level
    /// break is still needed.
    pub fn mark_broken(&mut self) {
        self.broken = true;
    }

    /// Iter 7.1.x.z — `true` if any in-walk demote attempt
    /// succeeded.
    pub fn is_broken(&self) -> bool {
        self.broken
    }

    /// Iter 7.1.x.z — explicitly mark the cycle as found.
    /// Used by `CycleVisit` impls that demote a back-edge
    /// BEFORE descending into the child: the demote replaces
    /// the slot with `Unspecified` so the normal `ctx.visit`
    /// path can no longer set `found` via that child. This
    /// method ensures the cycle is still reported.
    pub fn set_found(&mut self) {
        self.found = true;
    }

    /// Register a non-`Gc<T>` heap address (e.g. `Rc<T>` for
    /// `Frame` / `Closure`) and report whether the caller should
    /// descend into it. The dedup machinery is the same as
    /// [`visit`] but without the typed `Gc<T>` requirement, so
    /// arbitrary Rust-`Rc`-backed nodes (the walker's
    /// `Rc<Frame>` chain, the VM's `Rc<Env>` chain, dyn-`Procedure`
    /// closures) can be deduped against the visited set.
    ///
    /// Returns `true` to descend (first visit), `false` to skip
    /// (already visited, cycle target reached, or limit hit).
    pub fn visit_addr(&mut self, addr: usize) -> bool {
        if self.found || self.over_limit {
            return false;
        }
        if addr == self.root_addr {
            self.found = true;
            return false;
        }
        if !self.visited.insert(addr) {
            return false;
        }
        if self.visited.len() > self.limit {
            self.over_limit = true;
            return false;
        }
        true
    }
}

thread_local! {
    /// Per-call node-visit limit for [`cycle_check`]. Default
    /// 10_000. Set per-thread via [`set_limit`]; [`get_limit`]
    /// reports the current value.
    static LIMIT: Cell<usize> = const { Cell::new(10_000) };
}

/// Set the per-call node-visit limit for [`cycle_check`] on the
/// current thread. The default is 10_000.
pub fn set_limit(n: usize) {
    LIMIT.with(|c| c.set(n));
}

/// Current per-call node-visit limit on this thread.
pub fn get_limit() -> usize {
    LIMIT.with(|c| c.get())
}

thread_local! {
    /// Single reused `visited` set per thread, so back-to-back
    /// mutation-triggered checks (e.g. tail-building a list via
    /// repeated `set-cdr!`) don't allocate a fresh hash table on
    /// every call — the old cost that made list construction via
    /// `set-cdr!` O(n) per mutation. Cleared (not dropped) between
    /// uses via [`take_visited_set`] / [`return_visited_set`].
    static VISITED_POOL: RefCell<Option<FxHashSet<usize>>> =
        RefCell::new(Some(FxHashSet::default()));
}

/// Borrow the thread's reused visited-set, or allocate a fresh one
/// if it's already checked out (a nested `cycle_check` call from
/// inside a `visit_children`/`break_at` callback). The nested case
/// just forfeits reuse for that inner call — it never double-
/// borrows the `RefCell`.
fn take_visited_set() -> FxHashSet<usize> {
    VISITED_POOL
        .with(|p| p.borrow_mut().take())
        .unwrap_or_default()
}

/// Return a visited-set to the pool for the next call, clearing it
/// in place rather than dropping the backing allocation.
fn return_visited_set(mut set: FxHashSet<usize>) {
    set.clear();
    VISITED_POOL.with(|p| *p.borrow_mut() = Some(set));
}

/// Check whether the heap subgraph reachable from `root` contains
/// a cycle returning to `root` itself.
///
/// Returns `Some(CyclePath { root })` on positive detection.
/// Returns `None` both for genuinely acyclic subgraphs and for
/// subgraphs that exceed the per-call visit limit; call sites
/// that need to distinguish should consult [`get_limit`] and apply
/// a conservative downgrade on limit-hit.
pub fn cycle_check<T>(root: &Gc<T>) -> Option<CyclePath>
where
    T: CycleVisit + 'static,
{
    let mut ctx = CycleVisitor {
        visited: take_visited_set(),
        found: false,
        over_limit: false,
        root_addr: Gc::as_addr(root),
        limit: get_limit(),
        broken: false,
    };
    // Root's own slot is implicitly visited — encountering it
    // again during descent is the cycle signal.
    root.visit_children(&mut ctx);
    let result = if ctx.found {
        Some(CyclePath {
            root: ctx.root_addr,
        })
    } else {
        None
    };
    return_visited_set(ctx.visited);
    result
}

/// Convenience: run [`cycle_check`] on `root`; if a cycle is
/// found, invoke `break_at` once with a reference to the root so
/// the caller can flip one storage edge from `Strong` to `Weak`.
///
/// The break action runs at most once per call and only on
/// positive detection; the no-cycle hot path skips the closure.
///
/// Note: under iter 7.1.x.z, the cycle walk itself may have
/// already demoted a back-edge during descent (when a
/// `CycleVisit` impl sees that a child's address equals
/// `root_addr` and the target has sufficient external
/// anchors). [`check_and_break_walk`] exposes the
/// `already_broken` signal to callers that want to skip a
/// redundant root demote.
pub fn check_and_break<T>(root: &Gc<T>, break_at: impl FnOnce(&Gc<T>))
where
    T: CycleVisit + 'static,
{
    check_and_break_walk(root, |r, _broken| break_at(r));
}

/// Variant of [`check_and_break`] that exposes whether the
/// cycle walk's inline back-edge demote already broke the
/// cycle. The `break_at` callback receives `(root, broken)`
/// — when `broken == true`, the runtime can skip a redundant
/// root-level demote attempt.
pub fn check_and_break_walk<T>(root: &Gc<T>, break_at: impl FnOnce(&Gc<T>, bool))
where
    T: CycleVisit + 'static,
{
    let mut ctx = CycleVisitor {
        visited: take_visited_set(),
        found: false,
        over_limit: false,
        root_addr: Gc::as_addr(root),
        limit: get_limit(),
        broken: false,
    };
    root.visit_children(&mut ctx);
    let (found, broken) = (ctx.found, ctx.broken);
    return_visited_set(ctx.visited);
    if found {
        break_at(root, broken);
    }
}

// === Common leaf and container `CycleVisit` impls ===

macro_rules! cycle_visit_leaf {
    ($($t:ty),* $(,)?) => {
        $(
            impl CycleVisit for $t {
                fn visit_children(&self, _ctx: &mut CycleVisitor) {}
            }
        )*
    };
}
cycle_visit_leaf!(
    bool, char, u8, i8, u16, i16, u32, i32, u64, i64, usize, isize, f32, f64, String,
);

impl<T: CycleVisit> CycleVisit for Vec<T> {
    fn visit_children(&self, ctx: &mut CycleVisitor) {
        for item in self {
            if ctx.done() {
                return;
            }
            item.visit_children(ctx);
        }
    }
}

impl<T: CycleVisit> CycleVisit for Option<T> {
    fn visit_children(&self, ctx: &mut CycleVisitor) {
        if let Some(v) = self {
            v.visit_children(ctx);
        }
    }
}

impl<T: CycleVisit> CycleVisit for std::cell::RefCell<T> {
    fn visit_children(&self, ctx: &mut CycleVisitor) {
        self.borrow().visit_children(ctx);
    }
}

impl<T: CycleVisit + 'static> CycleVisit for Gc<T> {
    fn visit_children(&self, ctx: &mut CycleVisitor) {
        // Forward to the pointee's impl. The CALLER (e.g. a Pair
        // impl enumerating its car/cdr children) is responsible
        // for `ctx.visit(child)` *before* invoking this method —
        // that's how identity is registered and the cycle test
        // happens. Calling `child.visit_children(ctx)` directly
        // descends into the grandchildren without re-registering
        // the child itself.
        (**self).visit_children(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Node {
        children: RefCell<Vec<Gc<Node>>>,
    }

    impl CycleVisit for Node {
        fn visit_children(&self, ctx: &mut CycleVisitor) {
            for c in self.children.borrow().iter() {
                if ctx.done() {
                    return;
                }
                if ctx.visit(c) {
                    c.visit_children(ctx);
                }
            }
        }
    }

    fn node() -> Gc<Node> {
        Gc::new(Node {
            children: RefCell::new(Vec::new()),
        })
    }

    fn link(parent: &Gc<Node>, child: Gc<Node>) {
        parent.children.borrow_mut().push(child);
    }

    #[test]
    fn no_cycle_singleton() {
        let n = node();
        assert!(cycle_check(&n).is_none());
    }

    #[test]
    fn no_cycle_linear_chain() {
        let c = node();
        let b = node();
        link(&b, c.clone());
        let a = node();
        link(&a, b.clone());
        let root = node();
        link(&root, a.clone());
        assert!(cycle_check(&root).is_none());
    }

    #[test]
    fn direct_self_loop() {
        let root = node();
        link(&root, root.clone());
        let result = cycle_check(&root);
        assert!(result.is_some(), "expected self-loop detection");
        assert_eq!(result.unwrap().root, Gc::as_addr(&root));
    }

    #[test]
    fn two_node_mutual() {
        let a = node();
        let b = node();
        link(&a, b.clone());
        link(&b, a.clone());
        assert!(cycle_check(&a).is_some());
        assert!(cycle_check(&b).is_some());
    }

    #[test]
    fn three_node_ring() {
        let a = node();
        let b = node();
        let c = node();
        link(&a, b.clone());
        link(&b, c.clone());
        link(&c, a.clone());
        assert!(cycle_check(&a).is_some());
    }

    #[test]
    fn cycle_among_siblings_subgraph_not_back_to_root() {
        // root -> a, root -> b, a -> b, b -> a.
        // Cycle exists between a and b but not back to root.
        let root = node();
        let a = node();
        let b = node();
        link(&root, a.clone());
        link(&root, b.clone());
        link(&a, b.clone());
        link(&b, a.clone());
        // Detector asks: "does any path from root return to root?"
        // Answer: no. The a<->b cycle is internal but doesn't
        // close on root. Detector terminates via visited dedup.
        assert!(cycle_check(&root).is_none());
        // From a's perspective: a -> b -> a. Cycle reaches a.
        assert!(cycle_check(&a).is_some());
    }

    #[test]
    fn unrelated_cycle_not_seen_from_root() {
        let root = node();
        let r1 = node();
        link(&root, r1.clone());

        let x = node();
        let y = node();
        link(&x, y.clone());
        link(&y, x.clone());

        assert!(cycle_check(&root).is_none(), "root subgraph is acyclic");
        assert!(cycle_check(&x).is_some(), "x subgraph has a cycle");
    }

    #[test]
    fn limit_exceeded_returns_none() {
        // 600-node linear chain — long enough to comfortably
        // exceed the test's lowered limit of 100, short enough
        // that the host-stack drop chain at test end fits the
        // 2 MB test thread stack with margin. (Deeper chains
        // overflow not the detector but the cascading Rc::drop
        // along the singly-linked children vec.)
        const N: usize = 600;
        let mut prev = node();
        let root = prev.clone();
        for _ in 0..N {
            let next = node();
            link(&prev, next.clone());
            prev = next;
        }
        set_limit(100);
        let result = cycle_check(&root);
        set_limit(10_000);
        assert!(
            result.is_none(),
            "limit-exceeded should be reported as None"
        );
        // Tear down the chain iteratively so the Drop chain
        // doesn't recurse `N` levels deep through Rc destructors
        // and overflow the test thread stack.
        let mut current = root.clone();
        loop {
            let next = current.children.borrow_mut().pop();
            match next {
                Some(n) => current = n,
                None => break,
            }
        }
    }

    #[test]
    fn check_and_break_runs_on_cycle() {
        let root = node();
        link(&root, root.clone());
        let break_ran = Cell::new(false);
        check_and_break(&root, |_| break_ran.set(true));
        assert!(break_ran.get());
    }

    #[test]
    fn check_and_break_skips_when_acyclic() {
        let root = node();
        let break_ran = Cell::new(false);
        check_and_break(&root, |_| break_ran.set(true));
        assert!(!break_ran.get());
    }
}
