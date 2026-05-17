//! Type environment — hierarchical `Symbol → Type` map with
//! scope push/pop for `Lambda` / `Letrec` bodies.
//!
//! The environment is a stack of frames. `Lambda` pushes a frame
//! with its params; the body checks against the extended env;
//! exit pops. `Letrec` pushes a frame with its bindings before
//! the values are checked (so recursive references see their
//! own type at the type-of-binding declaration).
//!
//! Top-level bindings live in frame 0, which never pops.
//! `define`s from `extract_annotations`'s top-level
//! ascriptions seed frame 0; primops (iter 2.2) are baked into a
//! separate primops table that the env consults on lookup miss.

use std::collections::HashMap;

use cs_core::Symbol;

use crate::types::Type;

/// A single lexical scope's bindings.
///
/// `Frame` is `Default + Clone` so callers can spin them up
/// cheaply; the env is a `Vec<Frame>` where the LAST element
/// is the innermost scope.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub bindings: HashMap<Symbol, Type>,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, name: Symbol, ty: Type) {
        self.bindings.insert(name, ty);
    }

    pub fn get(&self, name: Symbol) -> Option<&Type> {
        self.bindings.get(&name)
    }
}

/// The type environment. A stack of `Frame`s; lookup walks
/// from innermost (top of stack) to outermost (frame 0).
#[derive(Clone, Debug)]
pub struct TypeEnv {
    frames: Vec<Frame>,
}

impl Default for TypeEnv {
    fn default() -> Self {
        Self {
            frames: vec![Frame::new()],
        }
    }
}

impl TypeEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new scope onto the stack. Returns a guard-style
    /// `usize` that [`pop_to`] can use to restore. We use this
    /// pattern instead of RAII because the checker walks the
    /// AST in a recursive fn that doesn't outlive scope-
    /// introducing nodes anyway.
    pub fn push(&mut self) -> usize {
        let mark = self.frames.len();
        self.frames.push(Frame::new());
        mark
    }

    /// Pop frames back to `mark` (returned by an earlier `push`).
    /// Useful in error-recovery paths.
    pub fn pop_to(&mut self, mark: usize) {
        self.frames.truncate(mark);
    }

    /// Convenience: push, mutate, pop in one call. Returns
    /// whatever the closure returns.
    pub fn with_scope<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let mark = self.push();
        let out = f(self);
        self.pop_to(mark);
        out
    }

    /// Insert a binding in the innermost scope.
    ///
    /// Panics if there are no frames (impossible via the
    /// public API — `new()` seeds the top-level frame).
    pub fn define(&mut self, name: Symbol, ty: Type) {
        self.frames
            .last_mut()
            .expect("TypeEnv always has at least one frame")
            .insert(name, ty);
    }

    /// Insert at the TOP-LEVEL frame (index 0). Used to seed
    /// ascription-extracted top-level types regardless of
    /// nesting depth at the call site.
    pub fn define_top_level(&mut self, name: Symbol, ty: Type) {
        self.frames[0].insert(name, ty);
    }

    /// Look up a name. Walks frames from innermost to outermost.
    /// Returns `None` if the name is unbound at every scope.
    pub fn lookup(&self, name: Symbol) -> Option<&Type> {
        for frame in self.frames.iter().rev() {
            if let Some(t) = frame.get(name) {
                return Some(t);
            }
        }
        None
    }

    /// True if the name is bound in any scope.
    pub fn contains(&self, name: Symbol) -> bool {
        self.lookup(name).is_some()
    }

    /// Current scope depth (1 = only top-level).
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(n: u32) -> Symbol {
        Symbol(n)
    }

    #[test]
    fn fresh_env_has_top_level_only() {
        let env = TypeEnv::new();
        assert_eq!(env.depth(), 1);
        assert!(env.lookup(sym(0)).is_none());
    }

    #[test]
    fn define_then_lookup() {
        let mut env = TypeEnv::new();
        env.define(sym(7), Type::Fixnum);
        assert_eq!(env.lookup(sym(7)), Some(&Type::Fixnum));
        assert!(env.lookup(sym(8)).is_none());
    }

    #[test]
    fn inner_shadows_outer() {
        let mut env = TypeEnv::new();
        env.define(sym(7), Type::Fixnum);
        env.with_scope(|env| {
            env.define(sym(7), Type::Flonum);
            assert_eq!(env.lookup(sym(7)), Some(&Type::Flonum));
        });
        // Pops restore the outer binding.
        assert_eq!(env.lookup(sym(7)), Some(&Type::Fixnum));
    }

    #[test]
    fn inner_inherits_outer() {
        let mut env = TypeEnv::new();
        env.define(sym(7), Type::Fixnum);
        env.with_scope(|env| {
            env.define(sym(8), Type::Boolean);
            assert_eq!(env.lookup(sym(7)), Some(&Type::Fixnum));
            assert_eq!(env.lookup(sym(8)), Some(&Type::Boolean));
        });
        assert!(env.lookup(sym(8)).is_none(), "inner binding popped");
    }

    #[test]
    fn define_top_level_skips_inner() {
        let mut env = TypeEnv::new();
        env.with_scope(|env| {
            env.define_top_level(sym(99), Type::Any);
            // Visible from inside.
            assert_eq!(env.lookup(sym(99)), Some(&Type::Any));
        });
        // Still visible after popping the inner scope.
        assert_eq!(env.lookup(sym(99)), Some(&Type::Any));
    }

    #[test]
    fn manual_push_pop_with_marks() {
        let mut env = TypeEnv::new();
        let m = env.push();
        env.define(sym(1), Type::Fixnum);
        env.push();
        env.define(sym(2), Type::Flonum);
        assert_eq!(env.depth(), 3);
        env.pop_to(m);
        // Both inner bindings should be gone; only the
        // outermost frame remains.
        assert_eq!(env.depth(), 1);
        assert!(env.lookup(sym(1)).is_none());
        assert!(env.lookup(sym(2)).is_none());
    }
}
