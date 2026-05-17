//! Annotation table — side-channel attaching `TypeAnn` data to
//! `cs_ir::CoreExpr` nodes via their source `Span`.
//!
//! Rationale: `CoreExpr` is consumed by ~50 construction sites
//! across cs-expand, cs-vm, cs-runtime, and tests. Adding
//! optional annotation fields directly to the enum variants
//! would require touching every constructor (37 Lambda + 12
//! Letrec sites at iter 1.3 count) with `param_types: vec![]`
//! noise even for purely-untyped code.
//!
//! The side-table approach keeps cs-ir's API stable. Iter 1.4
//! will have cs-expand populate this table during expansion;
//! iter 2.x (the bidirectional checker) reads it to drive
//! Lambda / Letrec / Ref type-checking.
//!
//! Keying: `cs_diag::Span` uniquely identifies a syntactic
//! construct in the post-expansion AST (the expander preserves
//! source spans through macro expansion). Two different
//! Lambdas at different source locations get different Spans;
//! macro-expanded Lambdas get the span of their use site, not
//! the macro definition.

use std::collections::HashMap;

use cs_core::Symbol;
use cs_diag::Span;

use crate::types::Type;

/// Type annotations for a single `Lambda` form.
///
/// `param_types[i]` is the annotation on the i-th positional
/// param (None if unannotated). `param_types.len()` may equal
/// `params.fixed.len()`, or be shorter if the user only
/// annotated some params and stopped. Out-of-bound indices
/// are unannotated by default.
///
/// `return_type` is the optional `: RetType` after the param
/// list, e.g. `(define (f [x : Fixnum]) : Fixnum ...)`.
///
/// `rest_type` covers the variadic tail: `(define (sum . xs)
/// : Fixnum ...)` where xs's type is recorded separately.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LambdaAnnotation {
    pub param_types: Vec<Option<Type>>,
    pub return_type: Option<Type>,
    pub rest_type: Option<Type>,
}

impl LambdaAnnotation {
    /// True iff at least one slot is annotated. Used by the
    /// checker to skip lambdas with no user-supplied types
    /// (their bodies are treated as untyped).
    pub fn is_annotated(&self) -> bool {
        self.return_type.is_some()
            || self.rest_type.is_some()
            || self.param_types.iter().any(|p| p.is_some())
    }

    /// Annotated type for param at index `i`, or `None` if
    /// the user didn't write one.
    pub fn param(&self, i: usize) -> Option<&Type> {
        self.param_types.get(i).and_then(|opt| opt.as_ref())
    }
}

/// Type annotations on a `Letrec` form's bindings.
/// `binding_types[i]` parallels `bindings[i]`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LetrecAnnotation {
    pub binding_types: Vec<Option<Type>>,
}

/// A `(: NAME TYPE)` ascription form at top-level scope.
/// Attaches a type to the next top-level `define NAME ...`.
/// The expander records these in order of appearance; the
/// checker walks the top-level Begin and matches ascriptions
/// to definitions by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopLevelAnnotation {
    pub name: Symbol,
    pub type_ann: Type,
    pub ascription_span: Span,
}

/// A `(define-type ALIAS TYPE)` form at top-level scope.
/// Aliases are resolved at check-time by name lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeAlias {
    pub name: Symbol,
    pub target: Type,
    pub define_span: Span,
}

/// Full annotation table produced by the expander for a
/// program. Passed alongside `CoreExpr` into the checker.
#[derive(Clone, Debug, Default)]
pub struct AnnotationTable {
    /// Span of the Lambda form → its annotations.
    pub lambdas: HashMap<Span, LambdaAnnotation>,
    /// Span of the Letrec form → its annotations.
    pub letrecs: HashMap<Span, LetrecAnnotation>,
    /// Top-level `(: name type)` ascriptions in order of
    /// appearance.
    pub top_level: Vec<TopLevelAnnotation>,
    /// Top-level `(define-type alias type)` declarations.
    pub aliases: Vec<TypeAlias>,
}

impl AnnotationTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// True iff the program contains at least one type
    /// annotation. The checker entry point short-circuits
    /// to a no-op when this is false — preserving zero
    /// overhead for purely untyped code.
    pub fn is_empty(&self) -> bool {
        self.lambdas.is_empty()
            && self.letrecs.is_empty()
            && self.top_level.is_empty()
            && self.aliases.is_empty()
    }

    /// Lookup annotation for a Lambda by its span.
    pub fn lambda(&self, span: Span) -> Option<&LambdaAnnotation> {
        self.lambdas.get(&span)
    }

    /// Lookup annotation for a Letrec by its span.
    pub fn letrec(&self, span: Span) -> Option<&LetrecAnnotation> {
        self.letrecs.get(&span)
    }

    /// Resolve a type alias by name. None if the name isn't
    /// a declared alias.
    pub fn resolve_alias(&self, name: Symbol) -> Option<&Type> {
        self.aliases
            .iter()
            .find(|a| a.name == name)
            .map(|a| &a.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_diag::FileId;

    fn span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 10,
        }
    }

    #[test]
    fn empty_table_is_empty() {
        let t = AnnotationTable::new();
        assert!(t.is_empty());
    }

    #[test]
    fn lambda_lookup_by_span() {
        let mut t = AnnotationTable::new();
        let s = span();
        let mut la = LambdaAnnotation::default();
        la.param_types.push(Some(Type::Fixnum));
        la.return_type = Some(Type::Fixnum);
        t.lambdas.insert(s, la.clone());
        assert!(!t.is_empty());
        assert_eq!(t.lambda(s), Some(&la));
        assert_eq!(t.lambda(s).unwrap().param(0), Some(&Type::Fixnum));
    }

    #[test]
    fn lambda_annotation_is_annotated_predicate() {
        let mut la = LambdaAnnotation::default();
        assert!(!la.is_annotated());
        la.param_types.push(None);
        assert!(!la.is_annotated());
        la.return_type = Some(Type::Fixnum);
        assert!(la.is_annotated());
    }

    #[test]
    fn alias_resolution() {
        let mut t = AnnotationTable::new();
        let name = Symbol(42);
        t.aliases.push(TypeAlias {
            name,
            target: Type::union(vec![Type::Fixnum, Type::Flonum]),
            define_span: span(),
        });
        assert_eq!(
            t.resolve_alias(name),
            Some(&Type::Union(vec![Type::Fixnum, Type::Flonum]))
        );
        assert_eq!(t.resolve_alias(Symbol(99)), None);
    }
}
