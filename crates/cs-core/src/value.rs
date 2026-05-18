//! The universal Scheme value type.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::number::Number;
use crate::symbol::{Symbol, SymbolTable};

/// A pair (cons cell). Mutable per R6RS via `set-car!` / `set-cdr!`.
///
/// Pairs that originate from the reader carry their source-text span
/// in `source`, populated by `Datum::to_value`. Pairs created at run
/// time via `(cons …)` leave `source` as `None`. This is the
/// foundation that R6RS++ §9's `(syntax-source …)` accessors read.
#[derive(Debug)]
pub struct Pair {
    pub car: RefCell<Value>,
    pub cdr: RefCell<Value>,
    /// Source-text origin if this pair came from the reader. `Cell`
    /// rather than plain field so the post-construction setter
    /// `set_source` doesn't need `&mut Pair`.
    pub source: std::cell::Cell<Option<cs_diag::Span>>,
}

impl Pair {
    pub fn new(car: Value, cdr: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Pair {
            car: RefCell::new(car),
            cdr: RefCell::new(cdr),
            source: std::cell::Cell::new(None),
        })
    }

    /// Construct a pair tagged with its reader-produced source span.
    pub fn with_source(car: Value, cdr: Value, span: cs_diag::Span) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Pair {
            car: RefCell::new(car),
            cdr: RefCell::new(cdr),
            source: std::cell::Cell::new(Some(span)),
        })
    }

    pub fn source_span(&self) -> Option<cs_diag::Span> {
        self.source.get()
    }
}

impl cs_gc::Trace for Pair {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        self.car.borrow().trace(marker);
        self.cdr.borrow().trace(marker);
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
    /// User-supplied (hash, equiv) procedures stored on the Hashtable.
    /// The procedures live in `Hashtable::custom`.
    Custom,
}

/// User-supplied hash and equivalence procedures attached to a hashtable
/// created with the 2-arg form of `(make-hashtable hash equiv)`. The
/// runtime calls these via the standard procedure-application path on
/// every set!/ref/contains?/delete!.
#[derive(Debug)]
pub struct CustomHashFns {
    pub hash: Value,
    pub equiv: Value,
}

/// R6RS hashtable.
#[derive(Debug)]
pub struct Hashtable {
    pub items: RefCell<Vec<(Value, Value)>>,
    pub eq_kind: HtEqKind,
    /// Populated only when `eq_kind == Custom`.
    pub custom: Option<CustomHashFns>,
}

impl Hashtable {
    pub fn new(eq_kind: HtEqKind) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Hashtable {
            items: RefCell::new(Vec::new()),
            eq_kind,
            custom: None,
        })
    }

    /// Construct a hashtable with user-supplied hash + equiv procedures.
    /// `eq_kind` is set to `Custom`; the runtime is responsible for
    /// dispatching the stored procs on every key comparison.
    pub fn new_custom(hash: Value, equiv: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Hashtable {
            items: RefCell::new(Vec::new()),
            eq_kind: HtEqKind::Custom,
            custom: Some(CustomHashFns { hash, equiv }),
        })
    }
}

impl cs_gc::Trace for Hashtable {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        for (k, v) in self.items.borrow().iter() {
            k.trace(marker);
            v.trace(marker);
        }
        if let Some(c) = &self.custom {
            c.hash.trace(marker);
            c.equiv.trace(marker);
        }
    }
}

/// A port: foundation supports string-, bytevector-, and file-backed
/// ports. File output ports buffer in memory and flush on `close-port`.
/// File input is currently slurped as a string-input-port at open time
/// (see `b_open_input_file` in cs-runtime); a streaming file-input
/// variant lands in a later milestone.
#[derive(Debug)]
pub enum Port {
    StringInput(RefCell<StringInputState>),
    StringOutput(RefCell<String>),
    ByteVectorInput(RefCell<ByteVectorInputState>),
    ByteVectorOutput(RefCell<Vec<u8>>),
    /// File output port. `buf` accumulates writes; `close-port` writes
    /// the buffer to `path`. `closed` flips true on close so subsequent
    /// writes are rejected.
    FileOutput(RefCell<FileOutputState>),
}

#[derive(Debug)]
pub struct FileOutputState {
    pub path: String,
    pub buf: Vec<u8>,
    pub closed: bool,
}

#[derive(Debug)]
pub struct StringInputState {
    pub chars: Vec<char>,
    pub pos: usize,
}

#[derive(Debug)]
pub struct ByteVectorInputState {
    pub bytes: Vec<u8>,
    pub pos: usize,
}

impl Port {
    pub fn string_input(s: &str) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::StringInput(RefCell::new(StringInputState {
            chars: s.chars().collect(),
            pos: 0,
        })))
    }

    pub fn string_output() -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::StringOutput(RefCell::new(String::new())))
    }

    pub fn bytevector_input(bytes: Vec<u8>) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::ByteVectorInput(RefCell::new(ByteVectorInputState {
            bytes,
            pos: 0,
        })))
    }

    pub fn bytevector_output() -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::ByteVectorOutput(RefCell::new(Vec::new())))
    }

    pub fn file_output(path: String) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Port::FileOutput(RefCell::new(FileOutputState {
            path,
            buf: Vec::new(),
            closed: false,
        })))
    }

    pub fn is_input(&self) -> bool {
        matches!(self, Port::StringInput(_) | Port::ByteVectorInput(_))
    }

    pub fn is_output(&self) -> bool {
        matches!(
            self,
            Port::StringOutput(_) | Port::ByteVectorOutput(_) | Port::FileOutput(_)
        )
    }

    pub fn is_textual(&self) -> bool {
        // Files can carry either, but the typical R6RS use of file output
        // is textual via `display`/`write`. Classify as textual; binary
        // file ports would be a separate variant.
        matches!(
            self,
            Port::StringInput(_) | Port::StringOutput(_) | Port::FileOutput(_)
        )
    }

    pub fn is_binary(&self) -> bool {
        matches!(self, Port::ByteVectorInput(_) | Port::ByteVectorOutput(_))
    }
}

impl cs_gc::Trace for Port {
    fn trace(&self, _marker: &mut cs_gc::Marker) {
        // Leaf: every Port variant holds either chars/bytes/Strings or a
        // file-output buffer. None contain a Value or Gc<T>, so there's
        // nothing to mark transitively.
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
    pub fn pending(thunk: Value) -> cs_gc::Gc<Self> {
        cs_gc::Gc::new(Promise {
            state: RefCell::new(PromiseState::Pending(thunk)),
        })
    }
}

impl cs_gc::Trace for Promise {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        match &*self.state.borrow() {
            PromiseState::Pending(v) | PromiseState::Forced(v) => v.trace(marker),
        }
    }
}

/// Type-erased procedure dispatch. Concrete builtin and closure types live in
/// `cs-runtime`; eval downcasts via [`as_any`].
///
/// Procedure has `cs_gc::Trace` as a supertrait so closure environments
/// (and any `Value` fields stored inside concrete procedure types)
/// participate in GC tracing. Most builtins are leaves (their `trace` is
/// empty); closures and parameters mark their captured Values.
pub trait Procedure: fmt::Debug + cs_gc::Trace + 'static {
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

impl cs_gc::Trace for Parameter {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        self.cell.borrow().trace(marker);
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
    String(crate::Gc<RefCell<String>>),
    Symbol(Symbol),
    Pair(crate::Gc<Pair>),
    Vector(crate::Gc<RefCell<Vec<Value>>>),
    ByteVector(crate::Gc<RefCell<Vec<u8>>>),
    Procedure(Rc<dyn Procedure>),
    Hashtable(crate::Gc<Hashtable>),
    Port(crate::Gc<Port>),
    Promise(crate::Gc<Promise>),
}

/// Format mode for [`Value::write_to`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteMode {
    /// R6RS `write`: read-back-able, strings quoted, characters as `#\\x`.
    Write,
    /// R6RS `display`: human-friendly, strings unquoted, raw characters.
    Display,
}

/// `Trace` impl for Value enumerates the heap-pointer variants so the
/// GC can reach every transitively-reachable allocation during a mark
/// pass.
///
/// All standard heap-data variants (Pair / Vector / String / ByteVector
/// / Hashtable / Port / Promise) are `Gc<T>`-backed and trace through.
/// `Procedure` is the one exception: it stays on `Rc<dyn Procedure>`
/// in Phase 1 because `Gc<dyn Procedure>` requires the unstable
/// `CoerceUnsized` trait. Closure environments and parameter cells
/// still participate in tracing because the concrete `Procedure` impls
/// in cs-runtime / cs-vm provide non-trivial `Trace` impls; we just
/// don't reach them through this entry — they're reachable via the
/// walker top frame and the VM root env, both of which are root closures.
/// True cycles that go *through* `Rc<dyn Procedure>` would leak in
/// Phase 1; the M5 spec's exit gate calls this out explicitly.
impl cs_gc::Trace for Value {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        match self {
            // Gc<T>-backed heap variants.
            Value::String(s) => s.trace(marker),
            Value::ByteVector(v) => v.trace(marker),
            Value::Vector(v) => v.trace(marker),
            Value::Pair(p) => p.trace(marker),
            Value::Hashtable(h) => h.trace(marker),
            Value::Port(p) => p.trace(marker),
            Value::Promise(p) => p.trace(marker),
            // Rc-backed (Phase 1 limitation, see doc above).
            Value::Procedure(_) => {}
            // Leaf variants — no heap pointers.
            Value::Null
            | Value::Unspecified
            | Value::Eof
            | Value::Boolean(_)
            | Value::Character(_)
            | Value::Symbol(_)
            | Value::Number(_) => {}
        }
    }
}

impl Value {
    pub fn fixnum(v: i64) -> Self {
        Value::Number(Number::Fixnum(v))
    }

    pub fn flonum(v: f64) -> Self {
        Value::Number(Number::Flonum(v))
    }

    pub fn string(s: impl Into<String>) -> Self {
        Value::String(crate::Gc::new(RefCell::new(s.into())))
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
        let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
        self.write_to_visited(out, syms, mode, &mut visited)
    }

    fn write_to_visited(
        &self,
        out: &mut dyn fmt::Write,
        syms: &SymbolTable,
        mode: WriteMode,
        visited: &mut std::collections::HashSet<usize>,
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
            Value::Pair(p) => write_pair(out, p, syms, mode, visited),
            Value::Vector(v) => {
                let ptr = crate::Gc::as_addr(v);
                if !visited.insert(ptr) {
                    return write!(out, "#(...)");
                }
                let res = (|| -> fmt::Result {
                    write!(out, "#(")?;
                    let inner = v.borrow();
                    for (i, item) in inner.iter().enumerate() {
                        if i > 0 {
                            write!(out, " ")?;
                        }
                        item.write_to_visited(out, syms, mode, visited)?;
                    }
                    write!(out, ")")
                })();
                visited.remove(&ptr);
                res
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
                Port::ByteVectorInput(_) => write!(out, "#<binary-input-port>"),
                Port::ByteVectorOutput(_) => write!(out, "#<binary-output-port>"),
                Port::FileOutput(s) => {
                    write!(out, "#<file-output-port {:?}>", s.borrow().path)
                }
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
    visited: &mut std::collections::HashSet<usize>,
) -> fmt::Result {
    let head_ptr = p as *const Pair as usize;
    if !visited.insert(head_ptr) {
        return write!(out, "(...)");
    }
    let result = write_pair_inner(out, p, syms, mode, visited);
    visited.remove(&head_ptr);
    result
}

fn write_pair_inner(
    out: &mut dyn fmt::Write,
    p: &Pair,
    syms: &SymbolTable,
    mode: WriteMode,
    visited: &mut std::collections::HashSet<usize>,
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
        cur_car.write_to_visited(out, syms, mode, visited)?;
        match cur_cdr {
            Value::Null => break,
            Value::Pair(next) => {
                let next_ptr = crate::Gc::as_addr(&next);
                if !visited.insert(next_ptr) {
                    write!(out, " . #<cycle>")?;
                    break;
                }
                cur_car = next.car.borrow().clone();
                cur_cdr = next.cdr.borrow().clone();
            }
            other => {
                write!(out, " . ")?;
                other.write_to_visited(out, syms, mode, visited)?;
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
                Port::ByteVectorInput(_) => write!(f, "#<binary-input-port>"),
                Port::ByteVectorOutput(_) => write!(f, "#<binary-output-port>"),
                Port::FileOutput(s) => {
                    write!(f, "#<file-output-port {:?}>", s.borrow().path)
                }
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
