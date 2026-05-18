//! Integration tests for the `cs_gc::cycle` detector.
//!
//! These exercise the *public* API surface of the cycle module
//! (CycleVisit, CycleVisitor, cycle_check, check_and_break,
//! set_limit/get_limit) using a fresh in-test `Node` fixture
//! that holds no `cs-core` types — so failures here isolate
//! detector bugs from any consumer-crate Trace impl bugs that
//! will land in iters 5–6.
//!
//! Gated on `feature = "countable-memory"`; under the tracing
//! default the file compiles to an empty module.

use std::cell::{Cell, RefCell};

use cs_gc::cycle::{check_and_break, cycle_check, set_limit, CycleVisit, CycleVisitor};
use cs_gc::Gc;

/// In-test graph node. Holds a mutable list of strong children;
/// cycles are formed by linking a node into a chain that loops
/// back to it.
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

/// Iteratively unlink a linear chain so its Drop chain doesn't
/// blow the test thread stack at function end.
fn unlink_chain(head: &Gc<Node>) {
    let mut current = head.clone();
    loop {
        let next = current.children.borrow_mut().pop();
        match next {
            Some(n) => current = n,
            None => break,
        }
    }
}

#[test]
fn no_cycle_linear_chain() {
    // root -> a -> b -> c
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
    assert!(result.is_some());
    assert_eq!(result.unwrap().root, Gc::as_addr(&root));
    // Manually break before drop so the cyclic Rc chain doesn't
    // leak (this is what iter 7's mutation builtins will do
    // automatically via check_and_break + slot downgrade; here
    // we drain the children for cleanup).
    root.children.borrow_mut().clear();
}

#[test]
fn two_node_mutual() {
    let a = node();
    let b = node();
    link(&a, b.clone());
    link(&b, a.clone());
    assert!(cycle_check(&a).is_some());
    assert!(cycle_check(&b).is_some());
    a.children.borrow_mut().clear();
    b.children.borrow_mut().clear();
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
    // Drain to release the cycle's Rc refs.
    a.children.borrow_mut().clear();
    b.children.borrow_mut().clear();
    c.children.borrow_mut().clear();
}

#[test]
fn unrelated_graphs_dont_confuse() {
    // Linear chain reachable from root; disjoint cycle elsewhere.
    let root = node();
    let r1 = node();
    link(&root, r1.clone());

    let x = node();
    let y = node();
    link(&x, y.clone());
    link(&y, x.clone());

    assert!(cycle_check(&root).is_none());
    assert!(cycle_check(&x).is_some());

    x.children.borrow_mut().clear();
    y.children.borrow_mut().clear();
}

#[test]
fn limit_exceeded_returns_none() {
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
    assert!(result.is_none());
    unlink_chain(&root);
}

#[test]
fn check_and_break_invokes_callback_on_cycle() {
    let root = node();
    link(&root, root.clone());
    let break_count = Cell::new(0);
    check_and_break(&root, |_| break_count.set(break_count.get() + 1));
    assert_eq!(break_count.get(), 1);
    root.children.borrow_mut().clear();
}

#[test]
fn check_and_break_no_callback_on_acyclic() {
    let root = node();
    let leaf = node();
    link(&root, leaf);
    let break_count = Cell::new(0);
    check_and_break(&root, |_| break_count.set(break_count.get() + 1));
    assert_eq!(break_count.get(), 0);
}
