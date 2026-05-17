//! Lexical environments.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cs_core::{Symbol, Value};

/// A frame of bindings.
#[derive(Debug)]
pub struct Frame {
    bindings: RefCell<HashMap<Symbol, Value>>,
    parent: Option<Rc<Frame>>,
}

impl Frame {
    pub fn root() -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(HashMap::new()),
            parent: None,
        })
    }

    pub fn child(parent: Rc<Frame>) -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(HashMap::new()),
            parent: Some(parent),
        })
    }

    pub fn get(&self, name: Symbol) -> Option<Value> {
        if let Some(v) = self.bindings.borrow().get(&name) {
            return Some(v.clone());
        }
        if let Some(parent) = &self.parent {
            return parent.get(name);
        }
        None
    }

    pub fn set_existing(&self, name: Symbol, value: Value) -> bool {
        if self.bindings.borrow().contains_key(&name) {
            self.bindings.borrow_mut().insert(name, value);
            return true;
        }
        if let Some(parent) = &self.parent {
            return parent.set_existing(name, value);
        }
        false
    }

    /// Insert in the topmost frame.
    pub fn define(&self, name: Symbol, value: Value) {
        self.bindings.borrow_mut().insert(name, value);
    }
}

#[cfg(not(feature = "countable-memory"))]
impl cs_core::Trace for Frame {
    fn trace(&self, marker: &mut cs_core::Marker) {
        for (_, val) in self.bindings.borrow().iter() {
            val.trace(marker);
        }
        if let Some(p) = &self.parent {
            p.trace(marker);
        }
    }
}

#[cfg(feature = "countable-memory")]
impl cs_gc::cycle::CycleVisit for Frame {
    fn visit_children(&self, ctx: &mut cs_gc::cycle::CycleVisitor) {
        // Frame is Rc<Self>; the address-based dedup keeps the
        // detector from re-entering the same frame when a
        // closure binding points back through its capturing env.
        // (E.g. `(define (f) f)` — bindings has f → Closure(env)
        // where env is this same Frame.)
        let addr = self as *const Self as usize;
        if !ctx.visit_addr(addr) {
            return;
        }
        // Walk THIS frame's bindings only. The parent chain is
        // not traversed: in mutation-driven cycle detection the
        // relevant question is "does the freshly mutated cell
        // reach back to itself through user data?", not "does
        // it reach back through the stdlib root env?".
        // Parent-chain traversal would recurse through hundreds
        // of defining frames (each holding builtins and prior
        // user definitions) and blow the host stack on deeply
        // nested test environments.
        //
        // Iter 8's planned Weak<Frame> refactor would close
        // this with a structural guarantee; the iter-11 safe
        // behavior is identity-dedup + skip-parent.
        for (_, val) in self.bindings.borrow().iter() {
            if ctx.done() {
                return;
            }
            val.visit_children(ctx);
        }
    }
}
