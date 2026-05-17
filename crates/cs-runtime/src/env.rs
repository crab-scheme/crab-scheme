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
        for (_, val) in self.bindings.borrow().iter() {
            if ctx.done() {
                return;
            }
            val.visit_children(ctx);
        }
        if let Some(p) = &self.parent {
            if ctx.done() {
                return;
            }
            // Iter 6: parent is still strong here (Rc<Frame>); the
            // iter-8 refactor converts it to Weak<Frame> and walks
            // upward via upgrade(). For now, descend through the
            // existing strong link — cycle detection works either
            // way; structural prevention via Weak is the iter-8
            // perf+correctness layer.
            p.visit_children(ctx);
        }
    }
}
