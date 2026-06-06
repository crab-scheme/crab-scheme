//! Per-Runtime symbol interner.

use std::collections::HashMap;
use std::rc::Rc;

/// An interned symbol identifier. Cheap to copy and compare.
///
/// Symbols are scoped to a single [`SymbolTable`] (and therefore a single
/// `Runtime`); two `Symbol`s from different tables are never equal even if
/// their backing strings match.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct Symbol(pub u32);

/// `Clone` gives a per-actor copy that shares the interned `Rc<str>` storage
/// (refcount bumps, not string copies) — the shared-Runtime model clones a
/// worker's canonical base table per actor so builtin symbol ids stay consistent
/// with the shared base env, while each actor can still intern new symbols.
#[derive(Clone, Default)]
pub struct SymbolTable {
    by_name: HashMap<Rc<str>, Symbol>,
    by_id: Vec<Rc<str>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, name: &str) -> Symbol {
        if let Some(s) = self.by_name.get(name) {
            return *s;
        }
        let rc: Rc<str> = Rc::from(name);
        let sym = Symbol(self.by_id.len() as u32);
        self.by_id.push(rc.clone());
        self.by_name.insert(rc, sym);
        sym
    }

    pub fn name(&self, sym: Symbol) -> &str {
        &self.by_id[sym.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_idempotent() {
        let mut t = SymbolTable::new();
        let a = t.intern("foo");
        let b = t.intern("foo");
        assert_eq!(a, b);
        assert_eq!(t.name(a), "foo");
    }

    #[test]
    fn distinct_symbols() {
        let mut t = SymbolTable::new();
        let a = t.intern("foo");
        let b = t.intern("bar");
        assert_ne!(a, b);
    }
}
