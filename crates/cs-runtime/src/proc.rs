//! Concrete procedure types: builtins and closures.

use std::any::Any;
use std::rc::Rc;

use cs_core::{Procedure, Symbol, SymbolTable, Value};
use cs_ir::{CoreExpr, Params};

use crate::env::Frame;
use crate::eval::EvalCtx;

pub type PureBuiltinFn = fn(&[Value]) -> Result<Value, String>;
pub type HoBuiltinFn = fn(&[Value], &mut EvalCtx) -> Result<Value, String>;
pub type SymsBuiltinFn = fn(&[Value], &mut SymbolTable) -> Result<Value, String>;

#[derive(Clone, Copy)]
pub enum BuiltinFn {
    Pure(PureBuiltinFn),
    Higher(HoBuiltinFn),
    Syms(SymsBuiltinFn),
}

#[derive(Clone, Copy)]
pub struct Builtin {
    pub name: &'static str,
    pub f: BuiltinFn,
}

impl std::fmt::Debug for Builtin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Builtin({})", self.name)
    }
}

impl Procedure for Builtin {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some(self.name)
    }
}

impl cs_core::Trace for Builtin {
    fn trace(&self, _marker: &mut cs_core::Marker) {
        // Leaf — Builtin holds only a fn pointer and a static name.
    }
}

#[derive(Debug)]
pub struct Closure {
    pub params: Params,
    pub body: Rc<CoreExpr>,
    pub env: Rc<Frame>,
    pub name: Option<Symbol>,
    pub display_name: Option<String>,
}

impl Procedure for Closure {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        self.display_name.as_deref()
    }
}

impl cs_core::Trace for Closure {
    fn trace(&self, marker: &mut cs_core::Marker) {
        // Trace the captured environment chain. The body is a shared
        // immutable IR pointer (`Rc<CoreExpr>`) carrying only Symbols
        // and span info — no Values to trace.
        self.env.trace(marker);
    }
}

// Parameter now lives in cs-core so both VM and tree-walker dispatch the
// same type. Re-export for backward compat.
pub use cs_core::{make_parameter, Parameter};

/// Escape-only first-class continuation produced by `call/cc`. Calling it
/// with one argument unwinds the stack to the originating `call/cc` and
/// returns that argument. Calls to a continuation outside its dynamic
/// extent are not supported (the matching call/cc has already returned).
#[derive(Debug)]
pub struct Continuation {
    pub id: u64,
}

impl Procedure for Continuation {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("continuation")
    }
}

impl cs_core::Trace for Continuation {
    fn trace(&self, _marker: &mut cs_core::Marker) {
        // Leaf — escape continuations carry only a u64 id.
    }
}

pub fn make_continuation(id: u64) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(Continuation { id });
    Value::Procedure(p)
}

pub fn make_builtin_pure(name: &'static str, f: PureBuiltinFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(Builtin {
        name,
        f: BuiltinFn::Pure(f),
    });
    Value::Procedure(p)
}

pub fn make_builtin_higher(name: &'static str, f: HoBuiltinFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(Builtin {
        name,
        f: BuiltinFn::Higher(f),
    });
    Value::Procedure(p)
}

pub fn make_builtin_syms(name: &'static str, f: SymsBuiltinFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(Builtin {
        name,
        f: BuiltinFn::Syms(f),
    });
    Value::Procedure(p)
}

pub fn make_closure(
    params: Params,
    body: Rc<CoreExpr>,
    env: Rc<Frame>,
    name: Option<Symbol>,
    syms: &SymbolTable,
) -> Value {
    let display_name = name.map(|s| syms.name(s).to_string());
    let p: Rc<dyn Procedure> = Rc::new(Closure {
        params,
        body,
        env,
        name,
        display_name,
    });
    Value::Procedure(p)
}
