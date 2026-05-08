//! The universal Scheme value type.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::number::Number;
use crate::symbol::{Symbol, SymbolTable};

/// A pair (cons cell). Mutable per R6RS via `set-car!` / `set-cdr!`.
#[derive(Debug)]
pub struct Pair {
    pub car: RefCell<Value>,
    pub cdr: RefCell<Value>,
}

impl Pair {
    pub fn new(car: Value, cdr: Value) -> Rc<Self> {
        Rc::new(Pair {
            car: RefCell::new(car),
            cdr: RefCell::new(cdr),
        })
    }
}

/// Hashtable equality kind. Real hashing comes later; foundation uses
/// linear search over a Vec — correctness first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtEqKind {
    /// `eq?` — pointer/identity equality.
    Eq,
    /// `eqv?` — value equality for numbers and characters, identity for heap.
    Eqv,
    /// `equal?` — structural equality.
    Equal,
}

/// R6RS hashtable.
#[derive(Debug)]
pub struct Hashtable {
    pub items: RefCell<Vec<(Value, Value)>>,
    pub eq_kind: HtEqKind,
}

impl Hashtable {
    pub fn new(eq_kind: HtEqKind) -> Rc<Self> {
        Rc::new(Hashtable {
            items: RefCell::new(Vec::new()),
            eq_kind,
        })
    }
}

/// A port: foundation supports string-input and string-output ports only.
/// File ports land in a later milestone.
#[derive(Debug)]
pub enum Port {
    StringInput(RefCell<StringInputState>),
    StringOutput(RefCell<String>),
}

#[derive(Debug)]
pub struct StringInputState {
    pub chars: Vec<char>,
    pub pos: usize,
}

impl Port {
    pub fn string_input(s: &str) -> Rc<Self> {
        Rc::new(Port::StringInput(RefCell::new(StringInputState {
            chars: s.chars().collect(),
            pos: 0,
        })))
    }

    pub fn string_output() -> Rc<Self> {
        Rc::new(Port::StringOutput(RefCell::new(String::new())))
    }

    pub fn is_input(&self) -> bool {
        matches!(self, Port::StringInput(_))
    }

    pub fn is_output(&self) -> bool {
        matches!(self, Port::StringOutput(_))
    }
}

/// A R5RS/R6RS promise. Memoized lazy value.
#[derive(Debug)]
pub struct Promise {
    pub state: RefCell<PromiseState>,
}

#[derive(Debug)]
pub enum PromiseState {
    /// Holding the un-forced thunk procedure.
    Pending(Value),
    /// Holding the memoized result.
    Forced(Value),
}

impl Promise {
    pub fn pending(thunk: Value) -> Rc<Self> {
        Rc::new(Promise {
            state: RefCell::new(PromiseState::Pending(thunk)),
        })
    }
}

/// Type-erased procedure dispatch. Concrete builtin and closure types live in
/// `cs-runtime`; eval downcasts via [`as_any`].
pub trait Procedure: fmt::Debug + 'static {
    fn as_any(&self) -> &dyn Any;
    fn name(&self) -> Option<&str> {
        None
    }
}

/// A dynamic parameter procedure (R6RS `make-parameter`). Lives in cs-core
/// so both the tree-walker and the VM can dispatch a single concrete type.
/// Calling with 0 args reads `cell`; with 1 arg writes it.
#[derive(Debug)]
pub struct Parameter {
    pub cell: RefCell<Value>,
}

impl Procedure for Parameter {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("parameter")
    }
}

pub fn make_parameter(initial: Value) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(Parameter {
        cell: RefCell::new(initial),
    });
    Value::Procedure(p)
}

/// The universal Scheme value.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Unspecified,
    Eof,
    Boolean(bool),
    Character(char),
    Number(Number),
    String(Rc<RefCell<String>>),
    Symbol(Symbol),
    Pair(Rc<Pair>),
    Vector(Rc<RefCell<Vec<Value>>>),
    ByteVector(Rc<RefCell<Vec<u8>>>),
    Procedure(Rc<dyn Procedure>),
    Hashtable(Rc<Hashtable>),
    Port(Rc<Port>),
    Promise(Rc<Promise>),
}

/// Format mode for [`Value::write_to`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteMode {
    /// R6RS `write`: read-back-able, strings quoted, characters as `#\\x`.
    Write,
    /// R6RS `display`: human-friendly, strings unquoted, raw characters.
    Display,
}

impl Value {
    pub fn fixnum(v: i64) -> Self {
        Value::Number(Number::Fixnum(v))
    }

    pub fn flonum(v: f64) -> Self {
        Value::Number(Number::Flonum(v))
    }

    pub fn string(s: impl Into<String>) -> Self {
        Value::String(Rc::new(RefCell::new(s.into())))
    }

    pub fn list(items: impl IntoIterator<Item = Value>) -> Self {
        let mut v: Vec<Value> = items.into_iter().collect();
        let mut acc = Value::Null;
        while let Some(item) = v.pop() {
            acc = Value::Pair(Pair::new(item, acc));
        }
        acc
    }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Boolean(false))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Unspecified => "unspecified",
            Value::Eof => "eof",
            Value::Boolean(_) => "boolean",
            Value::Character(_) => "character",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Pair(_) => "pair",
            Value::Vector(_) => "vector",
            Value::ByteVector(_) => "bytevector",
            Value::Procedure(_) => "procedure",
            Value::Hashtable(_) => "hashtable",
            Value::Port(_) => "port",
            Value::Promise(_) => "promise",
        }
    }

    /// Write this value to `out` using `syms` to resolve symbol names.
    /// Use [`format_with`] for a string-returning convenience.
    pub fn write_to(
        &self,
        out: &mut dyn fmt::Write,
        syms: &SymbolTable,
        mode: WriteMode,
    ) -> fmt::Result {
        match self {
            Value::Null => write!(out, "()"),
            Value::Unspecified => write!(out, "#<unspecified>"),
            Value::Eof => write!(out, "#<eof>"),
            Value::Boolean(true) => write!(out, "#t"),
            Value::Boolean(false) => write!(out, "#f"),
            Value::Character(c) => match mode {
                WriteMode::Write => match c {
                    ' ' => write!(out, "#\\space"),
                    '\n' => write!(out, "#\\newline"),
                    '\t' => write!(out, "#\\tab"),
                    '\r' => write!(out, "#\\return"),
                    '\0' => write!(out, "#\\nul"),
                    c => write!(out, "#\\{}", c),
                },
                WriteMode::Display => write!(out, "{}", c),
            },
            Value::Number(n) => write!(out, "{}", n),
            Value::String(s) => match mode {
                WriteMode::Write => write!(out, "\"{}\"", escape_string(&s.borrow())),
                WriteMode::Display => write!(out, "{}", s.borrow()),
            },
            Value::Symbol(s) => write!(out, "{}", syms.name(*s)),
            Value::Pair(p) => write_pair(out, p, syms, mode),
            Value::Vector(v) => {
                write!(out, "#(")?;
                let v = v.borrow();
                for (i, item) in v.iter().enumerate() {
                    if i > 0 {
                        write!(out, " ")?;
                    }
                    item.write_to(out, syms, mode)?;
                }
                write!(out, ")")
            }
            Value::Procedure(p) => match p.name() {
                Some(n) => write!(out, "#<procedure {}>", n),
                None => write!(out, "#<procedure>"),
            },
            Value::ByteVector(bv) => {
                write!(out, "#vu8(")?;
                let bv = bv.borrow();
                for (i, b) in bv.iter().enumerate() {
                    if i > 0 {
                        write!(out, " ")?;
                    }
                    write!(out, "{}", b)?;
                }
                write!(out, ")")
            }
            Value::Hashtable(h) => write!(out, "#<hashtable size={}>", h.items.borrow().len()),
            Value::Port(p) => match &**p {
                Port::StringInput(_) => write!(out, "#<input-port>"),
                Port::StringOutput(_) => write!(out, "#<output-port>"),
            },
            Value::Promise(_) => write!(out, "#<promise>"),
        }
    }

    /// Convenience: format using a SymbolTable.
    pub fn format_with(&self, syms: &SymbolTable, mode: WriteMode) -> String {
        let mut s = String::new();
        let _ = self.write_to(&mut s, syms, mode);
        s
    }
}

fn write_pair(
    out: &mut dyn fmt::Write,
    p: &Pair,
    syms: &SymbolTable,
    mode: WriteMode,
) -> fmt::Result {
    write!(out, "(")?;
    let mut first = true;
    let mut cur_car = p.car.borrow().clone();
    let mut cur_cdr = p.cdr.borrow().clone();
    loop {
        if !first {
            write!(out, " ")?;
        }
        first = false;
        cur_car.write_to(out, syms, mode)?;
        match cur_cdr {
            Value::Null => break,
            Value::Pair(next) => {
                cur_car = next.car.borrow().clone();
                cur_cdr = next.cdr.borrow().clone();
            }
            other => {
                write!(out, " . ")?;
                other.write_to(out, syms, mode)?;
                break;
            }
        }
    }
    write!(out, ")")
}

fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Default Display: opaque renderings for symbols (`#<symbol#N>`) when no
/// SymbolTable is available. Use [`Value::format_with`] for full output.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "()"),
            Value::Unspecified => write!(f, "#<unspecified>"),
            Value::Eof => write!(f, "#<eof>"),
            Value::Boolean(true) => write!(f, "#t"),
            Value::Boolean(false) => write!(f, "#f"),
            Value::Character(c) => write!(f, "#\\{}", c),
            Value::Number(n) => write!(f, "{}", n),
            Value::String(s) => write!(f, "\"{}\"", s.borrow()),
            Value::Symbol(s) => write!(f, "#<symbol#{}>", s.0),
            Value::Pair(p) => display_pair(f, p),
            Value::Vector(v) => {
                write!(f, "#(")?;
                let v = v.borrow();
                for (i, item) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", item)?;
                }
                write!(f, ")")
            }
            Value::Procedure(p) => match p.name() {
                Some(n) => write!(f, "#<procedure {}>", n),
                None => write!(f, "#<procedure>"),
            },
            Value::ByteVector(bv) => {
                write!(f, "#vu8(")?;
                let bv = bv.borrow();
                for (i, b) in bv.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", b)?;
                }
                write!(f, ")")
            }
            Value::Hashtable(h) => write!(f, "#<hashtable size={}>", h.items.borrow().len()),
            Value::Port(p) => match &**p {
                Port::StringInput(_) => write!(f, "#<input-port>"),
                Port::StringOutput(_) => write!(f, "#<output-port>"),
            },
            Value::Promise(_) => write!(f, "#<promise>"),
        }
    }
}

fn display_pair(f: &mut fmt::Formatter<'_>, p: &Pair) -> fmt::Result {
    write!(f, "(")?;
    let mut first = true;
    let mut cur_car = p.car.borrow().clone();
    let mut cur_cdr = p.cdr.borrow().clone();
    loop {
        if !first {
            write!(f, " ")?;
        }
        first = false;
        write!(f, "{}", cur_car)?;
        match cur_cdr {
            Value::Null => break,
            Value::Pair(next) => {
                cur_car = next.car.borrow().clone();
                cur_cdr = next.cdr.borrow().clone();
            }
            other => {
                write!(f, " . {}", other)?;
                break;
            }
        }
    }
    write!(f, ")")
}
