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

    /// Build a MUTABLE root frame pre-populated with `bindings`.
    /// Used by `(make-namespace ...)` (ADR 0015 L1.2). set! is
    /// allowed; the frame itself can be mutated. Note: per-eval
    /// frame; explicit `namespace-set-variable-value!` is the
    /// primary write path back to the namespace storage.
    pub fn mutable_root(bindings: HashMap<Symbol, Value>) -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(bindings),
            parent: None,
            immutable: false,
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
