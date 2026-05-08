//! Core IR for CrabScheme.
//!
//! `CoreExpr` is the post-expansion AST consumed by every execution tier.
//! Foundation milestone has only the tree-walker as a consumer; later tiers
//! (bytecode VM, JIT) will share this type.

use std::rc::Rc;

use cs_core::{Symbol, Value};
use cs_diag::Span;

#[derive(Clone, Debug)]
pub enum CoreExpr {
    Const {
        value: Value,
        span: Span,
    },
    Ref {
        name: Symbol,
        span: Span,
    },
    Set {
        name: Symbol,
        value: Rc<CoreExpr>,
        span: Span,
    },
    Lambda {
        params: Params,
        body: Rc<CoreExpr>,
        span: Span,
    },
    App {
        func: Rc<CoreExpr>,
        args: Vec<CoreExpr>,
        span: Span,
    },
    If {
        cond: Rc<CoreExpr>,
        then: Rc<CoreExpr>,
        alt: Rc<CoreExpr>,
        span: Span,
    },
    Begin {
        exprs: Vec<CoreExpr>,
        span: Span,
    },
    /// `letrec*` semantics: bindings see each other, evaluated in order.
    Letrec {
        bindings: Vec<(Symbol, CoreExpr)>,
        body: Rc<CoreExpr>,
        span: Span,
    },
}

#[derive(Clone, Debug)]
pub struct Params {
    pub fixed: Vec<Symbol>,
    pub rest: Option<Symbol>,
}

impl Params {
    pub fn fixed(names: Vec<Symbol>) -> Self {
        Self {
            fixed: names,
            rest: None,
        }
    }

    pub fn variadic(names: Vec<Symbol>, rest: Symbol) -> Self {
        Self {
            fixed: names,
            rest: Some(rest),
        }
    }

    pub fn min_arity(&self) -> usize {
        self.fixed.len()
    }

    pub fn accepts_arity(&self, n: usize) -> bool {
        if self.rest.is_some() {
            n >= self.fixed.len()
        } else {
            n == self.fixed.len()
        }
    }
}

impl CoreExpr {
    pub fn span(&self) -> Span {
        match self {
            CoreExpr::Const { span, .. }
            | CoreExpr::Ref { span, .. }
            | CoreExpr::Set { span, .. }
            | CoreExpr::Lambda { span, .. }
            | CoreExpr::App { span, .. }
            | CoreExpr::If { span, .. }
            | CoreExpr::Begin { span, .. }
            | CoreExpr::Letrec { span, .. } => *span,
        }
    }
}
