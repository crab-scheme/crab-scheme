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

/// An interner that can layer over a shared immutable **base** table.
///
/// A plain [`SymbolTable::new`] is flat. [`SymbolTable::with_base`] builds one
/// *over* an `Rc`-shared base: symbol ids `0..base.len()` resolve in the base
/// (read-only); newly interned symbols go into this table at ids `≥ base.len()`.
/// This lets many per-actor tables share one large base (all the builtin +
/// bundled-library symbols) while each holds only the handful its own body
/// interns — the shared-Runtime syms lever. `intern(&mut self, …)` is unchanged,
/// so no caller has to know about the base. `Clone` is cheap for a layered table
/// (the base is an `Rc` bump; only this table's own small maps are copied).
#[derive(Clone, Default)]
pub struct SymbolTable {
    /// Shared immutable base; `None` for a flat table. Ids below `base_offset`
    /// belong to (and resolve through) this base.
    base: Option<Rc<SymbolTable>>,
    /// First id this table owns (== `base.len()`, or 0 with no base).
    base_offset: u32,
    /// This table's own entries (the extension over `base`).
    by_name: HashMap<Rc<str>, Symbol>,
    by_id: Vec<Rc<str>>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// A table layered over a shared immutable `base`. Ids `0..base.len()`
    /// resolve in the base; symbols interned here get ids `≥ base.len()`. The
    /// base must be treated as immutable for the lifetime of this table (it is,
    /// in the shared-Runtime model: the per-worker base is built once and never
    /// mutated after the per-actor tables layer over it).
    pub fn with_base(base: Rc<SymbolTable>) -> Self {
        let base_offset = base.len() as u32;
        Self {
            base: Some(base),
            base_offset,
            by_name: HashMap::new(),
            by_id: Vec::new(),
        }
    }

    /// Read-only name→symbol lookup across this table and its base chain.
    fn lookup_name(&self, name: &str) -> Option<Symbol> {
        if let Some(s) = self.by_name.get(name) {
            return Some(*s);
        }
        self.base.as_ref().and_then(|b| b.lookup_name(name))
    }

    pub fn intern(&mut self, name: &str) -> Symbol {
        if let Some(s) = self.lookup_name(name) {
            return s;
        }
        let rc: Rc<str> = Rc::from(name);
        let sym = Symbol(self.base_offset + self.by_id.len() as u32);
        self.by_id.push(rc.clone());
        self.by_name.insert(rc, sym);
        sym
    }

    pub fn name(&self, sym: Symbol) -> &str {
        if sym.0 < self.base_offset {
            // Owned by the base chain.
            return self
                .base
                .as_ref()
                .expect("base_offset > 0 implies a base")
                .name(sym);
        }
        &self.by_id[(sym.0 - self.base_offset) as usize]
    }

    pub fn len(&self) -> usize {
        self.base_offset as usize + self.by_id.len()
    }

    /// True if `sym` resolves through the shared `base` chain (id below
    /// this table's `base_offset`) rather than this table's own extension.
    ///
    /// ## Why base ids are cross-table-stable
    ///
    /// Every per-actor table on a worker is built with [`with_base`] over the
    /// *same* `Rc<SymbolTable>` base image (the worker's shared builtin +
    /// bundled-library symbols). A base id `k < base_offset` is simply an index
    /// into that one shared base, so it names the identical symbol in any table
    /// layered over it — no re-interning required. Extension ids (`≥
    /// base_offset`) are private to the table that minted them: the same id
    /// means different (or no) symbols in a sibling table. So base ids may be
    /// copied verbatim between two such tables, extension ids may not. Used by
    /// cs-runtime's same-worker actor fast-send path (cs-845.1), which copies a
    /// message's symbols by id only when every one of them `is_base`.
    pub fn is_base(&self, sym: Symbol) -> bool {
        sym.0 < self.base_offset
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
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

    #[test]
    fn with_base_shares_ids_and_layers_extensions() {
        let mut base = SymbolTable::new();
        let car = base.intern("car"); // id 0
        let cdr = base.intern("cdr"); // id 1
        assert_eq!(base.len(), 2);
        let base = Rc::new(base);

        let mut a = SymbolTable::with_base(base.clone());
        // A builtin already in the base interns to the BASE's id — so base-env
        // lookups (keyed by base ids) resolve from a layered table.
        assert_eq!(a.intern("car"), car);
        assert_eq!(a.intern("cdr"), cdr);
        // A new symbol gets an id past the base, and is idempotent.
        let foo = a.intern("foo");
        assert_eq!(foo, Symbol(2));
        assert_eq!(a.intern("foo"), foo);
        // name() chains: base ids resolve in the base, own ids locally.
        assert_eq!(a.name(car), "car");
        assert_eq!(a.name(foo), "foo");
        assert_eq!(a.len(), 3);

        // A second table over the same base is independent (its own ids start at
        // the same offset) but still resolves base ids.
        let mut b = SymbolTable::with_base(base.clone());
        assert_eq!(b.intern("bar"), Symbol(2)); // same offset, distinct table
        assert_eq!(b.intern("car"), car); // base still resolves
        assert_eq!(b.name(car), "car");
    }
}
