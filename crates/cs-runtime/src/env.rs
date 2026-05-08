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
