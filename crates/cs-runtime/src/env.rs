//! Lexical environments.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cs_core::{Symbol, Value};

/// A frame of bindings.
///
/// `immutable` is set by `Frame::immutable_root` (ADR 0015 L1.1):
/// when true, `set!` against a binding in this frame raises
/// `&assertion` via the eval-side check rather than mutating.
/// Default false.
#[derive(Debug)]
pub struct Frame {
    bindings: RefCell<HashMap<Symbol, Value>>,
    parent: Option<Rc<Frame>>,
    immutable: bool,
}

impl Frame {
    pub fn root() -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(HashMap::new()),
            parent: None,
            immutable: false,
        })
    }

    pub fn child(parent: Rc<Frame>) -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(HashMap::new()),
            parent: Some(parent),
            immutable: false,
        })
    }

    /// Build an immutable root frame pre-populated with `bindings`.
    /// Used by `(environment ...)` to construct an R6RS-strict
    /// snapshot environment. set! against any name in this frame
    /// raises &assertion at the eval-site check; the frame
    /// itself never mutates.
    pub fn immutable_root(bindings: HashMap<Symbol, Value>) -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(bindings),
            parent: None,
            immutable: true,
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

    /// Whether this frame was built immutable (snapshot env).
    pub fn is_immutable(&self) -> bool {
        self.immutable
    }

    /// Walk the frame chain looking for an IMMUTABLE frame that
    /// already contains `name`. Returns true if found — used by
    /// the eval Set handler to raise &assertion before falling
    /// through to the normal mutation path.
    ///
    /// Pure walk: doesn't take the bindings RefCell mutably.
    pub fn is_immutable_definition(&self, name: Symbol) -> bool {
        let mut cur: &Frame = self;
        loop {
            if cur.immutable && cur.bindings.borrow().contains_key(&name) {
                return true;
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }
}

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
