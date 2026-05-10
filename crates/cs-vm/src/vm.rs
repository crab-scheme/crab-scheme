//! Stack-based VM that interprets [`Bytecode`].

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use cs_core::{Procedure, Symbol, SymbolTable, Value};
use cs_diag::Span;

use crate::opcode::{Bytecode, CompiledLambda, Inst};

thread_local! {
    /// Side-channel for multi-value returns within a VM tier. `values` (when
    /// passed >1 args) and `partition` write here; `call-with-values` reads.
    static VM_PENDING_VALUES: RefCell<Option<Vec<Value>>> = const { RefCell::new(None) };
    /// Side-channel for `raise` / `error`. Set by raise; read by
    /// with-exception-handler when a callee returns Err.
    static VM_PENDING_RAISE: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Side-channel for `call/cc` escape: when a continuation is invoked,
    /// it stashes (id, value) here and returns Err("__escape__"). The
    /// matching call/cc handler reads it; non-matching call/cc rethrows.
    static VM_PENDING_ESCAPE: RefCell<Option<(u64, Value)>> = const { RefCell::new(None) };
    /// Current input port (R6RS dynamic `current-input-port`). Set by
    /// `with-input-from-string`; read by `read` / `read-line` / `read-char`
    /// when called with no port arg.
    static VM_CURRENT_INPUT_PORT: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Current output port (R6RS dynamic `current-output-port`). Set by
    /// `with-output-to-string`; read by `display`/`write`/`newline` etc.
    static VM_CURRENT_OUTPUT_PORT: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Current error port (R7RS `current-error-port`). Lazily initialized
    /// to a string output port on first query.
    static VM_CURRENT_ERROR_PORT: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn take_pending_values() -> Option<Vec<Value>> {
    VM_PENDING_VALUES.with(|cell| cell.borrow_mut().take())
}

fn set_pending_values(vs: Vec<Value>) {
    VM_PENDING_VALUES.with(|cell| *cell.borrow_mut() = Some(vs));
}

/// Public hook for cs-runtime: builtins that produce multiple values
/// (e.g. `div-and-mod`) stash them here, and the VM dispatch machinery
/// drains via `take_pending_values` on `Inst::Call` return so
/// `call-with-values` sees them.
pub fn vm_set_pending_values(vs: Vec<Value>) {
    set_pending_values(vs);
}

fn take_pending_raise() -> Option<Value> {
    VM_PENDING_RAISE.with(|cell| cell.borrow_mut().take())
}

/// Public accessor for cs-runtime to drain VM_PENDING_RAISE on top-level
/// `__raised__` errors so callers can render the condition value rather
/// than the internal sentinel string.
pub fn vm_take_pending_raise() -> Option<Value> {
    take_pending_raise()
}

/// Public accessor for cs-runtime to drain VM_PENDING_ESCAPE on top-level
/// `__escape__` errors.
pub fn vm_take_pending_escape() -> Option<(u64, Value)> {
    take_pending_escape()
}

fn set_pending_raise(v: Value) {
    VM_PENDING_RAISE.with(|cell| *cell.borrow_mut() = Some(v));
}

/// External entry point for setting `pending_raise` from a `make_vm_builtin`
/// that needs to raise a condition (e.g. `exit`, `emergency-exit`).
pub fn vm_set_pending_raise(v: Value) {
    set_pending_raise(v);
}

fn take_pending_escape() -> Option<(u64, Value)> {
    VM_PENDING_ESCAPE.with(|cell| cell.borrow_mut().take())
}

fn set_pending_escape(id: u64, v: Value) {
    VM_PENDING_ESCAPE.with(|cell| *cell.borrow_mut() = Some((id, v)));
}

static VM_CONTINUATION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_continuation_id() -> u64 {
    VM_CONTINUATION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn current_input_port() -> Option<Value> {
    VM_CURRENT_INPUT_PORT.with(|cell| cell.borrow().clone())
}

fn current_output_port() -> Option<Value> {
    VM_CURRENT_OUTPUT_PORT.with(|cell| cell.borrow().clone())
}

/// Public accessor for cs-runtime to read the current VM input port from
/// inside a registered VmBuiltin/VmBuiltinSyms callback.
pub fn vm_current_input_port_value() -> Option<Value> {
    current_input_port()
}

/// Public accessor for cs-runtime to read the current VM output port.
pub fn vm_current_output_port_value() -> Option<Value> {
    current_output_port()
}

/// Public accessor for cs-runtime to read or lazily-init the VM error
/// port. R7RS `(current-error-port)` returns a port that user code can
/// write error output to; defaults to a string output port.
pub fn vm_current_error_port_value() -> Value {
    VM_CURRENT_ERROR_PORT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(Value::Port(cs_core::Port::string_output()));
        }
        slot.clone().unwrap()
    })
}

/// Function-pointer hook for `eval`: cs-runtime installs this before driving
/// the VM. The hook takes the value to eval and the live symbol table, and
/// returns the evaluated value. It typically reads cs-vm thread-locals like
/// `vm_eval_root_env` to find the env in which to run the sub-program.
pub type VmEvalHook = fn(&Value, &mut SymbolTable) -> Result<Value, String>;

/// Function-pointer hook fired once per `VmClosure` whose tier
/// counter crosses the threshold. The runtime installs this to
/// trigger JIT compilation of the closure's lambda. Receives the
/// closure that just crossed plus the arg slice from the
/// triggering call — useful for type-feedback signature inference
/// at JIT-compile time. The hook does whatever it likes — queue
/// compilation, log diagnostics, etc. — and returns. The
/// closure's tier counter is reset internally before the hook
/// fires so the hook isn't re-invoked on the very next call.
pub type VmTierUpHook = fn(closure: &VmClosure, args: &[Value]);

thread_local! {
    static VM_EVAL_HOOK: RefCell<Option<VmEvalHook>> = const { RefCell::new(None) };
    static VM_EVAL_ROOT_ENV: RefCell<Option<Rc<Env>>> = const { RefCell::new(None) };
    /// Optional tier-up hook fired by the closure-call dispatch when
    /// `VmClosure::tier.bump()` returns true.
    static VM_TIER_UP_HOOK: RefCell<Option<VmTierUpHook>> = const { RefCell::new(None) };
    /// Diagnostic counter: incremented each time a deopt event is
    /// recorded via [`record_deopt`]. Tests reset this to 0 with
    /// [`reset_deopt_count`].
    static VM_DEOPT_COUNT: Cell<u64> = const { Cell::new(0) };
    /// Diagnostic counter: incremented each time a tier-up hook
    /// fires. Tests use this to assert threshold-crossing behavior
    /// without installing a real hook.
    static VM_TIER_UP_COUNT: Cell<u64> = const { Cell::new(0) };
    /// Diagnostic counter: incremented each time a JIT-compiled
    /// closure dispatches through its native pointer rather than
    /// the bytecode body. Tests use this to assert the JIT actually
    /// ran (vs just being installed).
    static VM_JIT_CALL_COUNT: Cell<u64> = const { Cell::new(0) };
}

thread_local! {
    /// Pointer to the current closure's captured `Env`, set by
    /// `try_dispatch_jit` before calling into JITted code and
    /// cleared on return. The JIT calls `vm_env_lookup_fixnum`
    /// (below) which reads from this. Per-thread because the
    /// runtime is single-threaded.
    static JIT_CALLER_ENV: Cell<*const Env> = const { Cell::new(std::ptr::null()) };
}

/// RAII helper: set `JIT_CALLER_ENV` for the duration of a JIT
/// call, restore on drop.
struct JitEnvGuard {
    prev: *const Env,
}

impl JitEnvGuard {
    fn install(env: &Rc<Env>) -> Self {
        let prev = JIT_CALLER_ENV.with(|c| c.get());
        JIT_CALLER_ENV.with(|c| c.set(Rc::as_ptr(env)));
        Self { prev }
    }
}

impl Drop for JitEnvGuard {
    fn drop(&mut self) {
        JIT_CALLER_ENV.with(|c| c.set(self.prev));
    }
}

/// Helper called by JIT-compiled code to write a Fixnum back to a
/// free variable's binding. Walks the env chain via `set_existing`;
/// if no binding is found, defines at the root. Mirrors the
/// `Inst::SetVar` handler in `run_dispatch`.
///
/// # Safety
///
/// Same contract as `vm_env_lookup_fixnum` — `JIT_CALLER_ENV` must
/// be set by the runtime dispatch site.
#[no_mangle]
pub extern "C" fn vm_env_set_fixnum(sym: i64, value: i64) {
    let env_ptr = JIT_CALLER_ENV.with(|c| c.get());
    if env_ptr.is_null() {
        panic!("vm_env_set_fixnum: JIT_CALLER_ENV is null");
    }
    // SAFETY: as in vm_env_lookup_fixnum.
    let env = unsafe { &*env_ptr };
    let sym = Symbol(sym as u32);
    let v = Value::Number(cs_core::Number::Fixnum(value));
    if !env.set_existing(sym, v.clone()) {
        // No existing binding — define at root. Walk parent
        // chain holding Rc clones so each step keeps the parent
        // alive while we examine the next.
        let mut root: Rc<Env> = unsafe {
            // Rebuild an Rc from the raw pointer by cloning. The
            // closure that owns the Env is still alive (held by
            // the JIT-dispatching closure value) so the strong
            // count is at least 1; clone bumps it.
            let raw_rc = Rc::from_raw(env_ptr);
            let cloned = raw_rc.clone();
            // Don't drop the Rc we synthesized from the raw —
            // it would decrement the original count incorrectly.
            std::mem::forget(raw_rc);
            cloned
        };
        while let Some(p) = root.parent.clone() {
            root = p;
        }
        root.define(sym, v);
    }
}

/// Helper called by JIT-compiled code to look up a free variable's
/// fixnum value in the closure's captured env. The env pointer is
/// pulled from `JIT_CALLER_ENV` (set by `try_dispatch_jit`).
///
/// # Safety
///
/// The thread-local must be set to a valid Env pointer for the
/// duration of any JIT call that lowers `Inst::EnvLookup`. The
/// caller (the runtime's dispatch site) is responsible for that.
///
/// Returns the i64 value of the bound Fixnum. Panics on:
/// - Unbound symbol.
/// - Bound value not a Fixnum (TODO: deopt instead).
///
/// `extern "C"` so Cranelift can call it via a function pointer.
#[no_mangle]
pub extern "C" fn vm_env_lookup_fixnum(sym: i64) -> i64 {
    let env_ptr = JIT_CALLER_ENV.with(|c| c.get());
    if env_ptr.is_null() {
        panic!("vm_env_lookup_fixnum: JIT_CALLER_ENV is null");
    }
    // SAFETY: caller (try_dispatch_jit) guarantees env_ptr points
    // to a live Rc'd Env for the duration of the JIT call.
    let env = unsafe { &*env_ptr };
    let sym = Symbol(sym as u32);
    match env.get(sym) {
        Some(Value::Number(cs_core::Number::Fixnum(n))) => n,
        Some(other) => panic!(
            "vm_env_lookup_fixnum: symbol {:?} bound to non-Fixnum ({})",
            sym,
            other.type_name()
        ),
        None => panic!("vm_env_lookup_fixnum: unbound symbol {:?}", sym),
    }
}

// ====================================================================
// JIT heap-pointer ABI helpers (ADR 0011 D-2 / D-3 / D-5).
//
// Per ADR 0011, JIT'd bodies that need to construct or access heap-
// allocated values (Pair, Vector, Procedure, ...) call extern "C"
// runtime helpers via Cranelift. The helpers internally use cs-core's
// Value enum; the i64 ABI carriers are tagged per-slot via the
// `JIT_RT_*` constants in this file.
//
// Common encoding:
//   - Immediate types: Fixnum (i64 directly), Boolean (0/1),
//     Character (codepoint), Flonum (f64::to_bits).
//   - Heap-pointer types: tagged-pointer-style with the i64
//     carrying `Box::into_raw(Box<Value>)` for `JIT_RT_ANY`, or
//     the relevant `Rc::into_raw` / `Gc::into_raw` cast for the
//     specific-pointer tags.
//
// For now (iter AR) the helpers route everything through `Box<Value>`
// (the Any tag) — that's the simplest correctness-first path and
// matches D-3's polymorphic-call-site fallback. Specific-pointer
// tags get added as their lowering iters land (cons → Pair, vector →
// Vector, etc.).

/// Decode a `(i64, tag)` pair into a `Value`. Caller-owned: returns
/// a fresh `Value` that the caller drops on its own schedule.
///
/// For heap-pointer tags the i64 is consumed: `JIT_RT_ANY` calls
/// `Box::from_raw` (taking ownership of the box), which means each
/// i64 must only be decoded once. For other heap tags the contract
/// is the same — the caller hands ownership to the helper.
///
/// # Safety
///
/// Heap-pointer tags require the i64 to be a live, owned pointer of
/// the matching shape. Decoding mismatched tags / pointers is UB.
unsafe fn i64_to_value(i: i64, tag: u8) -> Value {
    match tag {
        JIT_RT_FIXNUM => Value::Number(cs_core::Number::Fixnum(i)),
        JIT_RT_BOOLEAN => Value::Boolean(i != 0),
        JIT_RT_CHARACTER => Value::Character(char::from_u32(i as u32).unwrap_or('\u{FFFD}')),
        JIT_RT_FLONUM => Value::Number(cs_core::Number::Flonum(f64::from_bits(i as u64))),
        JIT_RT_NULL => Value::Null,
        JIT_RT_SYMBOL => Value::Symbol(cs_core::Symbol(i as u32)),
        JIT_RT_ANY => {
            // SAFETY: caller transferred ownership of the Box<Value>
            // when it produced the i64 via `value_to_any_i64`.
            let boxed: Box<Value> = unsafe { Box::from_raw(i as *mut Value) };
            *boxed
        }
        _ => panic!(
            "i64_to_value: tag {} not yet decodable (deferred to a follow-up iter)",
            tag
        ),
    }
}

/// Encode a `Value` into a `(i64, tag)` pair carried as a single
/// i64 word with the tag stored externally — typically as Any-tagged
/// `Box::into_raw`. Caller is responsible for the matching decode.
fn value_to_any_i64(v: Value) -> i64 {
    Box::into_raw(Box::new(v)) as i64
}

/// `(cons car cdr)` — heap-allocate a fresh pair. Operands are i64
/// carriers tagged per the wider ABI; the helper decodes both into
/// `Value`s, allocates a `Pair`, and returns an `Any`-tagged i64
/// pointing at a `Box<Value::Pair(gc)>`.
///
/// `extern "C"` so Cranelift can import it via `JITBuilder::symbol`.
///
/// # Safety
///
/// Each input i64 must be a live, owned value of its declared tag.
/// `JIT_RT_ANY` inputs are consumed (Box::from_raw); pass each i64
/// only once. Caller (the JIT dispatcher) owns the ABI contract.
#[no_mangle]
pub unsafe extern "C" fn vm_alloc_pair(car: i64, car_tag: u8, cdr: i64, cdr_tag: u8) -> i64 {
    let car_v = unsafe { i64_to_value(car, car_tag) };
    let cdr_v = unsafe { i64_to_value(cdr, cdr_tag) };
    value_to_any_i64(Value::Pair(cs_core::Pair::new(car_v, cdr_v)))
}

/// `(car pair)` — return the pair's car, Any-tagged. Pre-decodes the
/// Any-tagged input box and re-Anys the inner car.
///
/// # Safety
///
/// Same contract as `vm_alloc_pair` — `pair` must be a live,
/// owned i64 from `value_to_any_i64`.
#[no_mangle]
pub unsafe extern "C" fn vm_pair_car(pair: i64) -> i64 {
    let v = unsafe { i64_to_value(pair, JIT_RT_ANY) };
    match v {
        Value::Pair(p) => value_to_any_i64(p.car.borrow().clone()),
        other => panic!("vm_pair_car: not a pair ({})", other.type_name()),
    }
}

/// `(cdr pair)` — Any-tagged cdr. See `vm_pair_car` for the safety
/// contract.
#[no_mangle]
pub unsafe extern "C" fn vm_pair_cdr(pair: i64) -> i64 {
    let v = unsafe { i64_to_value(pair, JIT_RT_ANY) };
    match v {
        Value::Pair(p) => value_to_any_i64(p.cdr.borrow().clone()),
        other => panic!("vm_pair_cdr: not a pair ({})", other.type_name()),
    }
}

/// `(pair? v)` — type predicate. Consumes the Any-tagged box on
/// the way out and returns 1 if the inner Value is a Pair, 0
/// otherwise. Consume-on-use keeps the lifetime model linear:
/// each Any-tagged i64 must be used exactly once.
#[no_mangle]
pub unsafe extern "C" fn vm_pair_p(r: i64) -> i64 {
    let boxed: Box<Value> = unsafe { Box::from_raw(r as *mut Value) };
    matches!(*boxed, Value::Pair(_)) as i64
}

/// `(null? v)` — type predicate. Consume-on-use, like `vm_pair_p`.
#[no_mangle]
pub unsafe extern "C" fn vm_null_p(r: i64) -> i64 {
    let boxed: Box<Value> = unsafe { Box::from_raw(r as *mut Value) };
    matches!(*boxed, Value::Null) as i64
}

/// Peek-clone an Any-tagged box: produce a fresh `Box<Value>` whose
/// inner value is `(*r).clone()`, return its raw pointer as the new
/// i64. The original box at `r` is left intact (the JIT body still
/// owns it). Used by `Inst::AnyClone` to support multi-use of an
/// Any operand.
#[no_mangle]
pub unsafe extern "C" fn vm_value_clone(r: i64) -> i64 {
    let v = unsafe { &*(r as *const Value) };
    value_to_any_i64(v.clone())
}

/// Drop an Any-tagged box (Box::from_raw + drop). Used by
/// `Inst::AnyDrop` at every return path to release Any-typed params
/// that the body never otherwise consumed.
#[no_mangle]
pub unsafe extern "C" fn vm_value_drop(r: i64) {
    drop(unsafe { Box::from_raw(r as *mut Value) });
}

/// Box a typed i64 into an Any-tagged `Box<Value>` and return the
/// raw pointer as the new i64. `tag` is a JIT_RT_* value (passed as
/// i64 because Cranelift doesn't have a direct u8 ABI type; the
/// helper truncates). Used by `Inst::BoxTyped` to widen a
/// Fixnum/Boolean/Character/Flonum value into the Any lane on
/// control-flow joins or returns.
#[no_mangle]
pub unsafe extern "C" fn vm_box_typed(i: i64, tag: i64) -> i64 {
    let v = unsafe { i64_to_value(i, tag as u8) };
    value_to_any_i64(v)
}

/// Consume an Any-tagged box and extract its inner Fixnum as a
/// raw i64. Panics if the boxed Value isn't a Fixnum — the
/// caller's responsibility to ensure the type-feedback signature
/// filtered out non-Fixnum-valued bodies upstream. Used by
/// `Inst::AnyToFix` to feed an Any operand into a Fixnum-only op
/// like `Add` or `Lt`.
#[no_mangle]
pub unsafe extern "C" fn vm_unbox_fixnum(r: i64) -> i64 {
    let boxed: Box<Value> = unsafe { Box::from_raw(r as *mut Value) };
    match *boxed {
        Value::Number(cs_core::Number::Fixnum(n)) => n,
        ref other => panic!("vm_unbox_fixnum: not a fixnum ({})", other.type_name()),
    }
}

/// Read the per-thread JIT-dispatch count. Test/diagnostics only.
pub fn jit_call_count() -> u64 {
    VM_JIT_CALL_COUNT.with(|c| c.get())
}

/// Reset the per-thread JIT-dispatch count.
pub fn reset_jit_call_count() {
    VM_JIT_CALL_COUNT.with(|c| c.set(0));
}

/// Increment the JIT-dispatch counter. Called by the closure-call
/// dispatch each time it routes through native code.
fn bump_jit_call_count() {
    VM_JIT_CALL_COUNT.with(|c| c.set(c.get() + 1));
}

/// Try to dispatch a JIT-compiled closure call. Returns
/// `Some(result)` if the closure has a JIT pointer installed and
/// every arg is a Fixnum; otherwise `None` (caller falls back to
/// bytecode dispatch).
///
/// Iter-6 ABI: `extern "C" fn(i64, ..., i64) -> i64`. Args are
/// unboxed Fixnums; the result is wrapped as `Value::Number(Fixnum)`.
fn try_dispatch_jit(closure: &VmClosure, args: &[Value]) -> Option<Value> {
    let ptr = closure.jit_ptr();
    if ptr.is_null() {
        return None;
    }
    if closure.jit_arity() as usize != args.len() {
        return None;
    }
    let mut argv = [0i64; 6];
    if args.len() > argv.len() {
        return None;
    }
    let param_types = closure.jit_param_types();
    for (i, a) in args.iter().enumerate() {
        let expected = ((param_types >> (i * 4)) & 0xF) as u8;
        match (expected, a) {
            (JIT_RT_FIXNUM, Value::Number(cs_core::Number::Fixnum(n))) => argv[i] = *n,
            (JIT_RT_FLONUM, Value::Number(cs_core::Number::Flonum(f))) => {
                argv[i] = f.to_bits() as i64
            }
            (JIT_RT_BOOLEAN, Value::Boolean(b)) => argv[i] = if *b { 1 } else { 0 },
            (JIT_RT_CHARACTER, Value::Character(c)) => argv[i] = *c as u32 as i64,
            // Any-tagged param: clone the Value into a fresh Box and
            // pass its raw pointer as the i64. The JIT body owns the
            // box and is expected to consume it linearly (via car /
            // cdr / pair? / null? / return). Iter AU only fires this
            // path when param_hints recorded the arg as a non-
            // immediate Value (Pair, Vector, ...).
            (JIT_RT_ANY, v) => argv[i] = value_to_any_i64(v.clone()),
            _ => {
                // Type-guard miss: the JIT body's signature doesn't
                // match this call's arg shapes. Bump the per-closure
                // deopt counter; if it crosses the recompile
                // threshold, drop the JIT pointer so the next
                // call's tier-up hook recompiles with fresh type
                // feedback. (Iter AH — feedback-driven recompile.)
                let n = closure.bump_jit_deopt();
                if n >= JIT_DEOPT_RECOMPILE_THRESHOLD {
                    closure.clear_jit_for_recompile();
                }
                return None;
            }
        }
    }
    bump_jit_call_count();
    closure.bump_jit_call_count_self();
    // Install the closure's env in the JIT thread-local so any
    // Inst::EnvLookup the body emits can read free vars. The
    // guard restores the previous value (or null) on drop, even
    // on panic from inside the JIT body.
    let _env_guard = JitEnvGuard::install(&closure.env);
    let r: i64 = match args.len() {
        0 => {
            let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };
            f()
        }
        1 => {
            let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
            f(argv[0])
        }
        2 => {
            let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(ptr) };
            f(argv[0], argv[1])
        }
        3 => {
            let f: extern "C" fn(i64, i64, i64) -> i64 = unsafe { std::mem::transmute(ptr) };
            f(argv[0], argv[1], argv[2])
        }
        4 => {
            let f: extern "C" fn(i64, i64, i64, i64) -> i64 = unsafe { std::mem::transmute(ptr) };
            f(argv[0], argv[1], argv[2], argv[3])
        }
        _ => return None,
    };
    Some(decode_jit_return(closure.jit_return_type(), r))
}

/// Wrap a raw i64 from a JIT'd body into the matching `Value` form
/// based on the closure's stored return-type tag. Boolean uses 0/1
/// from Lt/Eq; Character carries the codepoint in the low 32 bits;
/// Flonum reads the i64 as the bit pattern of an f64. Any reads
/// the i64 as `Box::into_raw(Box<Value>)` and reconstitutes the
/// owned Value (dropping the Box on the way out).
fn decode_jit_return(rt: u8, r: i64) -> Value {
    match rt {
        JIT_RT_BOOLEAN => Value::Boolean(r != 0),
        JIT_RT_CHARACTER => {
            // Truncate to u32; `char::from_u32` rejects surrogates and
            // out-of-range codepoints. If the JIT body produced an
            // invalid codepoint we fall back to U+FFFD rather than
            // panicking — this lines up with `decode_bytes` in the
            // codec layer.
            Value::Character(char::from_u32(r as u32).unwrap_or('\u{FFFD}'))
        }
        JIT_RT_FLONUM => {
            let f = f64::from_bits(r as u64);
            Value::Number(cs_core::Number::Flonum(f))
        }
        JIT_RT_NULL => Value::Null,
        JIT_RT_SYMBOL => Value::Symbol(cs_core::Symbol(r as u32)),
        JIT_RT_ANY => {
            // SAFETY: the JIT body produced this i64 via
            // `value_to_any_i64` (Box::into_raw). We own the Box now
            // and reconstitute it, copying out the inner Value before
            // the Box drops.
            let boxed: Box<Value> = unsafe { Box::from_raw(r as *mut Value) };
            *boxed
        }
        _ => Value::Number(cs_core::Number::Fixnum(r)),
    }
}

/// Install the `eval` hook for the current thread. Returns the previous hook
/// so callers can restore it after the VM run completes.
pub fn install_eval_hook(hook: Option<VmEvalHook>) -> Option<VmEvalHook> {
    VM_EVAL_HOOK.with(|cell| {
        let prev = *cell.borrow();
        *cell.borrow_mut() = hook;
        prev
    })
}

/// Install the root env that the eval hook should use as the parent env when
/// running an evaluated sub-program. Returns the previous value for restore.
pub fn install_eval_root_env(env: Option<Rc<Env>>) -> Option<Rc<Env>> {
    VM_EVAL_ROOT_ENV.with(|cell| {
        cell.borrow_mut().take().or_else(|| {
            // Use only when current is None; replacement done below.
            None
        })
    });
    VM_EVAL_ROOT_ENV.with(|cell| {
        let prev = cell.borrow_mut().take();
        *cell.borrow_mut() = env;
        prev
    })
}

/// Read the eval root env (used by the hook to compile-and-run sub-programs
/// against the live runtime's VM environment).
pub fn vm_eval_root_env() -> Option<Rc<Env>> {
    VM_EVAL_ROOT_ENV.with(|cell| cell.borrow().clone())
}

/// Install the tier-up hook for the current thread. Returns the
/// previous hook so callers can restore it after the VM run
/// completes. Pass `None` to clear.
pub fn install_tier_up_hook(hook: Option<VmTierUpHook>) -> Option<VmTierUpHook> {
    VM_TIER_UP_HOOK.with(|cell| {
        let prev = *cell.borrow();
        *cell.borrow_mut() = hook;
        prev
    })
}

/// Fire the tier-up hook for the given closure if one is installed.
/// Independent of whether the threshold actually crossed — callers
/// should only invoke this after [`cs_jit::Tier::bump`] returned
/// true. Safe to call when no hook is installed.
fn fire_tier_up_hook(closure: &VmClosure, args: &[Value]) {
    VM_TIER_UP_COUNT.with(|c| c.set(c.get() + 1));
    let hook = VM_TIER_UP_HOOK.with(|cell| *cell.borrow());
    if let Some(f) = hook {
        f(closure, args);
    }
}

/// Read the per-thread tier-up event count. Test/diagnostics only.
pub fn tier_up_count() -> u64 {
    VM_TIER_UP_COUNT.with(|c| c.get())
}

/// Reset the per-thread tier-up event count. Test/diagnostics only.
pub fn reset_tier_up_count() {
    VM_TIER_UP_COUNT.with(|c| c.set(0));
}

/// Record a deopt event from JIT-compiled code falling back to the
/// VM. The runtime's deopt trampoline calls this before returning
/// to the interpreter. Bumps `VM_DEOPT_COUNT` and (per the
/// `cs_jit::Tier` contract) bumps the supplied tier's deopt tally;
/// the procedure may end up blacklisted if the budget is exhausted.
///
/// Iter 3 ships only the bookkeeping side; the trampoline itself
/// (saving JIT register state, restoring VM state) lands in iter
/// 4+ once the JIT actually executes code.
pub fn record_deopt(tier: &cs_jit::Tier) -> bool {
    VM_DEOPT_COUNT.with(|c| c.set(c.get() + 1));
    tier.record_deopt()
}

/// Read the per-thread deopt count. Test/diagnostics only.
pub fn deopt_count() -> u64 {
    VM_DEOPT_COUNT.with(|c| c.get())
}

/// Reset the per-thread deopt count. Test/diagnostics only.
pub fn reset_deopt_count() {
    VM_DEOPT_COUNT.with(|c| c.set(0));
}

fn run_eval_hook(v: &Value, syms: &mut SymbolTable) -> Result<Value, VmError> {
    let hook = VM_EVAL_HOOK.with(|cell| *cell.borrow());
    match hook {
        Some(f) => f(v, syms).map_err(VmError::new),
        None => Err(VmError::new("eval: no hook installed")),
    }
}

fn swap_input_port(new: Option<Value>) -> Option<Value> {
    VM_CURRENT_INPUT_PORT.with(|cell| {
        let prev = cell.borrow_mut().take();
        *cell.borrow_mut() = new;
        prev
    })
}

fn swap_output_port(new: Option<Value>) -> Option<Value> {
    VM_CURRENT_OUTPUT_PORT.with(|cell| {
        let prev = cell.borrow_mut().take();
        *cell.borrow_mut() = new;
        prev
    })
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub message: String,
    pub span: Span,
    /// Caller-call-site spans, innermost first. Populated by the VM dispatch
    /// loop when an error bubbles out: each entry is the span of a Call /
    /// TailCall instruction in an outer frame, in stack order.
    pub backtrace: Vec<Span>,
}

impl VmError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            span: Span::DUMMY,
            backtrace: Vec::new(),
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        if self.span.is_dummy() {
            self.span = span;
        }
        self
    }
}

/// VM closure: a compiled lambda + the env at the point of construction.
///
/// Each closure carries a [`cs_jit::Tier`] counter that bumps on
/// every call. When the counter crosses the per-runtime tier-up
/// threshold, the optional `VmTierUpHook` (installed by
/// [`install_tier_up_hook`]) fires. The hook is the JIT compiler's
/// trigger; if it's `None`, the counter still bumps but nothing
/// happens.
///
/// On successful JIT compilation, the hook calls
/// [`VmClosure::set_jit_ptr`] to install the native function
/// pointer + arity. The closure-call dispatch checks `jit_ptr`
/// before falling back to bytecode.
#[derive(Debug)]
pub struct VmClosure {
    pub lambda_idx: usize,
    pub env: Rc<Env>,
    pub bc: Rc<Bytecode>,
    /// Per-closure tier counter. Owned, not shared — each freshly
    /// allocated closure starts at 0. (M6 iter 3.)
    pub tier: cs_jit::Tier,
    /// Native function pointer once JIT-compiled, else null.
    /// Lazy: filled by the tier-up hook (M6 iter 6).
    jit_ptr: Cell<*const u8>,
    /// Arity at which `jit_ptr` was compiled. Caller checks this
    /// before transmuting.
    jit_arity: Cell<u32>,
    /// Symbol the closure was first bound to (via Define / Set),
    /// if any. The bytecode→RIR translator uses this to detect
    /// `LoadVar(self_name)` patterns inside the body and emit
    /// `Inst::CallSelf` so recursive functions JIT. (M6 iter 7.)
    self_name: Cell<Option<Symbol>>,
    /// Logical return type of the JIT'd body, encoded for the
    /// dispatcher. 0 = Fixnum (default; back-compat with iter-6),
    /// 1 = Boolean. Stored as u8 because `cs_rir::Type` lives in a
    /// crate cs-vm doesn't depend on at this layer.
    jit_return_type: Cell<u8>,
    /// Per-param JIT-expected types, packed 4 bits per param (low
    /// nibble = arg 0). 0xF = unset/unused. Default all-Fixnum
    /// matches iter-W behavior. The dispatcher checks each arg
    /// against the matching nibble before transmuting to the
    /// `extern "C"` function pointer.
    jit_param_types: Cell<u32>,
    /// Per-closure type-guard miss counter. Incremented by
    /// `try_dispatch_jit` whenever an arg fails the stored
    /// signature. When the count exceeds [`JIT_DEOPT_RECOMPILE_THRESHOLD`]
    /// the dispatch site fires the tier-up hook again with the
    /// current args; the hook clears `jit_ptr`, recompiles with
    /// fresh signature, and the next call retries against the
    /// new layout. (Item 12 of the JIT roadmap — feedback-driven
    /// recompile.)
    jit_deopt_count: Cell<u32>,
    /// Per-closure JIT call counter. Bumped each time
    /// `try_dispatch_jit` successfully runs the native body.
    /// Exposed via the `jit-status` builtin so tests/benchmarks
    /// can pin down which specific closures are tier'd up vs
    /// just having a JIT pointer that nobody dispatches through.
    jit_call_count: Cell<u64>,
}

/// How many type-guard misses a closure tolerates before the
/// dispatch site re-fires the tier-up hook for recompilation.
/// Set conservatively — a single mistyped warming call shouldn't
/// trigger a wholesale recompile, but a closure that's
/// consistently called with different types should adapt.
pub const JIT_DEOPT_RECOMPILE_THRESHOLD: u32 = 256;

/// Encodings for [`VmClosure::jit_return_type`] and the per-nibble
/// slots in `jit_param_types`. Kept as plain `u8` so storage stays
/// Copy without pulling cs-rir into cs-vm.
///
/// Tags 0..3 are the M6 Phase 2 immediate types — fully wired through
/// the dispatcher and Cranelift lowering. Tags 4..14 are the heap-
/// pointer types reserved by ADR 0011 D-1 for the next milestone;
/// they only have constant entries today, no dispatcher decode yet
/// (try_dispatch_jit rejects any non-immediate slot until those
/// iters land). Tag 15 = Any per ADR 0011 D-3.
pub const JIT_RT_FIXNUM: u8 = 0;
pub const JIT_RT_BOOLEAN: u8 = 1;
pub const JIT_RT_CHARACTER: u8 = 2;
pub const JIT_RT_FLONUM: u8 = 3;
/// Heap-pointer Pair (`Rc<Pair>::into_raw`).
pub const JIT_RT_PAIR: u8 = 4;
/// Heap-pointer Vector (`Gc<RefCell<Vec<Value>>>::into_raw`).
pub const JIT_RT_VECTOR: u8 = 5;
/// Heap-pointer String.
pub const JIT_RT_STRING: u8 = 6;
/// Heap-pointer ByteVector.
pub const JIT_RT_BYTEVECTOR: u8 = 7;
/// Heap-pointer Procedure (`Rc<dyn Procedure>::into_raw`).
pub const JIT_RT_PROCEDURE: u8 = 8;
/// `Symbol(u32)` zero-extended.
pub const JIT_RT_SYMBOL: u8 = 9;
/// Heap-pointer BigInt.
pub const JIT_RT_BIGINT: u8 = 10;
/// Heap-pointer Rational.
pub const JIT_RT_RATIONAL: u8 = 11;
/// Heap-pointer Hashtable.
pub const JIT_RT_HASHTABLE: u8 = 12;
/// Heap-pointer Port.
pub const JIT_RT_PORT: u8 = 13;
/// `Value::Null` (the `'()` singleton) — immediate-shaped: the i64
/// payload is ignored on decode (always 0). Lets the JIT carry an
/// empty-list value through the i64 ABI without any heap allocation.
pub const JIT_RT_NULL: u8 = 14;
/// Polymorphic slot — i64 carries `Box::into_raw(Box<Value>)`. Per
/// ADR 0011 D-3, used at megamorphic call sites.
pub const JIT_RT_ANY: u8 = 15;

/// Default `jit_param_types` value: every nibble = JIT_RT_FIXNUM (0).
pub const JIT_PARAM_TYPES_ALL_FIXNUM: u32 = 0;

impl VmClosure {
    /// Install a native function pointer compiled by the JIT, with
    /// the matching parameter count. Called by the tier-up hook on
    /// successful compilation. After this, the closure-call dispatch
    /// will route through native code when arg types match.
    pub fn set_jit_ptr(&self, ptr: *const u8, arity: u32) {
        self.jit_ptr.set(ptr);
        self.jit_arity.set(arity);
        // Default — callers that know the JIT'd body returns Boolean
        // should call `set_jit_return_type` after this.
        self.jit_return_type.set(JIT_RT_FIXNUM);
    }

    /// Tell the dispatcher how to decode the i64 the JIT'd body
    /// returns. Defaults to Fixnum; predicate procedures should set
    /// this to Boolean before the closure is dispatched.
    pub fn set_jit_return_type(&self, rt: u8) {
        self.jit_return_type.set(rt);
    }

    pub fn jit_return_type(&self) -> u8 {
        self.jit_return_type.get()
    }

    /// Bake per-param type tags from a slice (low nibble = arg 0).
    /// Caller is responsible for limiting `tags` to the same arity
    /// the JIT'd body was compiled with — extra tags get truncated
    /// at the 8-arg / 32-bit boundary.
    pub fn set_jit_param_types(&self, tags: &[u8]) {
        let mut packed: u32 = 0;
        for (i, t) in tags.iter().take(8).enumerate() {
            packed |= ((*t as u32) & 0xF) << (i * 4);
        }
        self.jit_param_types.set(packed);
    }

    pub fn jit_param_types(&self) -> u32 {
        self.jit_param_types.get()
    }

    /// Bump the deopt counter; returns the new value. Called from
    /// the dispatch path each time `try_dispatch_jit` rejects on a
    /// type-guard mismatch.
    pub fn bump_jit_deopt(&self) -> u32 {
        let n = self.jit_deopt_count.get().saturating_add(1);
        self.jit_deopt_count.set(n);
        n
    }

    pub fn jit_deopt_count(&self) -> u32 {
        self.jit_deopt_count.get()
    }

    pub fn jit_call_count(&self) -> u64 {
        self.jit_call_count.get()
    }

    pub(crate) fn bump_jit_call_count_self(&self) {
        let n = self.jit_call_count.get().saturating_add(1);
        self.jit_call_count.set(n);
    }

    /// Clear the JIT pointer + deopt counter so the next call's
    /// tier-up hook recompiles with fresh type feedback. The
    /// closure stays alive; only the cached native function
    /// pointer is dropped. Tier counter is primed to threshold-1
    /// via `Tier::reset_for_recompile` so the very next call
    /// re-fires the hook.
    pub fn clear_jit_for_recompile(&self) {
        self.jit_ptr.set(std::ptr::null());
        self.jit_deopt_count.set(0);
        self.tier.reset_for_recompile();
    }

    /// Currently-installed JIT pointer, if any. `None` until the
    /// tier-up hook fires + succeeds; persists for the closure's
    /// lifetime once set.
    pub fn jit_ptr(&self) -> *const u8 {
        self.jit_ptr.get()
    }

    /// Arity the JIT pointer was compiled for. Meaningful only when
    /// [`jit_ptr`] is non-null.
    pub fn jit_arity(&self) -> u32 {
        self.jit_arity.get()
    }

    /// Stamp the closure's `self_name` if it isn't already set.
    /// Idempotent — first definer wins so re-binding doesn't
    /// overwrite the JIT-relevant identity.
    pub fn set_self_name_once(&self, sym: Symbol) {
        if self.self_name.get().is_none() {
            self.self_name.set(Some(sym));
        }
    }

    /// Self-name, if one was stamped. Used by the JIT tier-up hook
    /// to drive `bytecode_to_rir`'s self-recursion detection.
    pub fn self_name(&self) -> Option<Symbol> {
        self.self_name.get()
    }
}

/// If `v` is a `VmClosure` with no `self_name` yet, stamp it with
/// `sym`. Used by the Define / Set call sites so the JIT can
/// recognize self-recursion in the body.
fn stamp_self_name_if_closure(v: &Value, sym: Symbol) {
    if let Value::Procedure(p) = v {
        if let Some(c) = p.as_any().downcast_ref::<VmClosure>() {
            c.set_self_name_once(sym);
        }
    }
}

impl Procedure for VmClosure {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("vm-closure")
    }
}

impl cs_gc::Trace for VmClosure {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        // Trace the captured environment chain. Bytecode is immutable
        // shared `Rc<Bytecode>` containing only Symbols and opcodes —
        // no Values to trace. Tier is leaf data (atomics + u32) —
        // nothing to trace.
        self.env.trace(marker);
    }
}

/// Hybrid binding storage: small frames (the overwhelming majority — function
/// params, letrec bindings, let bindings) live in a `Vec<(Symbol, Value)>`
/// with linear scan, which beats HashMap overhead for ≤~12 entries. Once a
/// frame grows past `SMALL_THRESHOLD` entries we promote to a HashMap so
/// the root env (~80 builtins, plus user-defined globals) stays O(1).
const SMALL_THRESHOLD: usize = 12;

#[derive(Debug)]
enum Bindings {
    Small(Vec<(Symbol, Value)>),
    Large(HashMap<Symbol, Value>),
}

impl Default for Bindings {
    fn default() -> Self {
        Bindings::Small(Vec::new())
    }
}

impl cs_gc::Trace for Bindings {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        match self {
            Bindings::Small(v) => {
                for (_, val) in v {
                    val.trace(marker);
                }
            }
            Bindings::Large(m) => {
                for (_, val) in m {
                    val.trace(marker);
                }
            }
        }
    }
}

impl cs_gc::Trace for Env {
    fn trace(&self, marker: &mut cs_gc::Marker) {
        self.bindings.borrow().trace(marker);
        if let Some(p) = &self.parent {
            p.trace(marker);
        }
    }
}

impl Bindings {
    fn get(&self, name: Symbol) -> Option<Value> {
        match self {
            Bindings::Small(v) => v
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, val)| val.clone()),
            Bindings::Large(m) => m.get(&name).cloned(),
        }
    }

    fn contains(&self, name: Symbol) -> bool {
        match self {
            Bindings::Small(v) => v.iter().any(|(k, _)| *k == name),
            Bindings::Large(m) => m.contains_key(&name),
        }
    }

    fn insert(&mut self, name: Symbol, value: Value) {
        match self {
            Bindings::Small(v) => {
                if let Some(slot) = v.iter_mut().find(|(k, _)| *k == name) {
                    slot.1 = value;
                    return;
                }
                v.push((name, value));
                // Promote to HashMap once we exceed the threshold.
                if v.len() > SMALL_THRESHOLD {
                    let drained: Vec<(Symbol, Value)> = v.drain(..).collect();
                    let mut m = HashMap::with_capacity(drained.len() * 2);
                    for (k, val) in drained {
                        m.insert(k, val);
                    }
                    *self = Bindings::Large(m);
                }
            }
            Bindings::Large(m) => {
                m.insert(name, value);
            }
        }
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (Symbol, Value)> + '_> {
        match self {
            Bindings::Small(v) => Box::new(v.iter().map(|(k, val)| (*k, val.clone()))),
            Bindings::Large(m) => Box::new(m.iter().map(|(k, v)| (*k, v.clone()))),
        }
    }
}

#[derive(Debug, Default)]
pub struct Env {
    bindings: RefCell<Bindings>,
    pub parent: Option<Rc<Env>>,
}

impl Env {
    pub fn root() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn child(parent: Rc<Self>) -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(Bindings::default()),
            parent: Some(parent),
        })
    }

    pub fn get(&self, name: Symbol) -> Option<Value> {
        if let Some(v) = self.bindings.borrow().get(name) {
            return Some(v);
        }
        if let Some(p) = &self.parent {
            return p.get(name);
        }
        None
    }

    pub fn set_existing(&self, name: Symbol, value: Value) -> bool {
        if self.bindings.borrow().contains(name) {
            self.bindings.borrow_mut().insert(name, value);
            return true;
        }
        if let Some(p) = &self.parent {
            return p.set_existing(name, value);
        }
        false
    }

    pub fn define(&self, name: Symbol, value: Value) {
        self.bindings.borrow_mut().insert(name, value);
    }

    /// Snapshot the bindings of this env (and all parents) into a flat
    /// HashMap. Used by the compiler to fold known-immutable globals to
    /// `Inst::Const`. Closer-to-root parents are overridden by closer-to-
    /// leaf children if the same symbol exists at multiple levels.
    pub fn snapshot_bindings(&self) -> HashMap<Symbol, Value> {
        let mut out = HashMap::new();
        if let Some(p) = &self.parent {
            out = p.snapshot_bindings();
        }
        for (k, v) in self.bindings.borrow().iter() {
            out.insert(k, v);
        }
        out
    }
}

#[derive(Clone, Debug)]
struct Frame {
    insts: Rc<Vec<Inst>>,
    spans: Rc<Vec<Span>>,
    ip: usize,
    env: Rc<Env>,
    /// Captured shared bytecode (so closures can resolve their lambda body).
    bc: Rc<Bytecode>,
}

/// Snapshot of the VM's frame stack and value stack, captured at
/// `call/cc` entry. Restoring it replaces the live `frames` and
/// `stack` and resumes execution at the captured top frame.
///
/// Per ADR 0010 D-1: snapshots are heap-allocated and Rc-shared so
/// capture is O(frame count) Vec-of-Rc clones rather than a deep
/// memcpy. The runtime clones the inner Vecs on **invocation** (so
/// the captured snapshot is reusable for re-invocation) — capture
/// itself is just an Rc bump on this struct.
#[derive(Debug, Clone)]
pub struct VmContSnapshot {
    frames: Rc<Vec<Frame>>,
    stack: Rc<Vec<Value>>,
}

impl VmContSnapshot {
    /// Number of captured frames. Useful for tests asserting that a
    /// snapshot was actually taken (vs an empty placeholder).
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Captured value stack length.
    pub fn stack_len(&self) -> usize {
        self.stack.len()
    }
}

pub fn run(bc: &Bytecode, top_env: Rc<Env>, syms: &mut SymbolTable) -> Result<Value, VmError> {
    run_with_entry(Rc::new(bc.clone()), None, None, top_env, syms)
}

/// Like [`run`] but accepts an already-shared `Rc<Bytecode>` (avoiding a
/// heap allocation per call) and an optional `entry_insts`/`entry_spans`
/// override for running a specific lambda body. `vm_call_sync` uses this
/// for HO bridge calls to skip constructing a sub-Bytecode per element.
pub fn run_with_entry(
    bc: Rc<Bytecode>,
    entry_insts: Option<Rc<Vec<Inst>>>,
    entry_spans: Option<Rc<Vec<Span>>>,
    top_env: Rc<Env>,
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    let insts = entry_insts.unwrap_or_else(|| bc.insts.clone());
    let spans = entry_spans.unwrap_or_else(|| bc.spans.clone());
    let mut stack: Vec<Value> = Vec::new();
    let mut frames: Vec<Frame> = vec![Frame {
        insts,
        spans,
        ip: 0,
        env: top_env,
        bc,
    }];
    let result = run_dispatch(&mut stack, &mut frames, syms);
    result.map_err(|mut e| {
        // Attach a backtrace: spans of the Call/TailCall instructions in
        // the outer frames at the point the error bubbled out. Innermost
        // first, so callers can render "in <site> --> ...".
        for frame in frames.iter().rev().skip(1) {
            let ip = frame.ip.saturating_sub(1);
            if let Some(s) = frame.spans.get(ip).copied() {
                if !s.is_dummy() {
                    e.backtrace.push(s);
                }
            }
        }
        e
    })
}

/// The actual dispatch loop, factored out so `run_with_entry` can wrap
/// its result with a frame-walking backtrace builder before returning.
fn run_dispatch(
    stack: &mut Vec<Value>,
    frames: &mut Vec<Frame>,
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    loop {
        let Some(frame) = frames.last_mut() else {
            return Err(VmError::new("vm stack underflow"));
        };
        if frame.ip >= frame.insts.len() {
            // End of frame: pop, keep top of stack as result.
            frames.pop();
            if frames.is_empty() {
                return stack
                    .pop()
                    .ok_or_else(|| VmError::new("empty stack at exit"));
            }
            continue;
        }
        // Borrow-by-reference dispatch: avoids cloning the instruction (and
        // its Value payload for Const) per VM tick. Owned data is taken only
        // in the arms that need it (Const stack-push, Call/TailCall).
        let inst_ref = &frame.insts[frame.ip];
        let inst_ip = frame.ip;
        frame.ip += 1;
        match inst_ref {
            Inst::Const(v) => stack.push(v.clone()),
            Inst::LoadVar(s) => {
                let s = *s;
                let v = frame.env.get(s).ok_or_else(|| {
                    let span = frame.spans.get(inst_ip).copied().unwrap_or(Span::DUMMY);
                    VmError::new(format!("undefined variable: {}", syms.name(s))).with_span(span)
                })?;
                stack.push(v);
            }
            Inst::SetVar(s) => {
                let s = *s;
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on Set"))?;
                stamp_self_name_if_closure(&v, s);
                if !frame.env.set_existing(s, v.clone()) {
                    let mut root = frame.env.clone();
                    while let Some(p) = root.parent.clone() {
                        root = p;
                    }
                    root.define(s, v);
                }
            }
            Inst::DefineGlobal(s) => {
                let s = *s;
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on Define"))?;
                stamp_self_name_if_closure(&v, s);
                let mut root = frame.env.clone();
                while let Some(p) = root.parent.clone() {
                    root = p;
                }
                root.define(s, v);
            }
            Inst::DefineLocal(s) => {
                let s = *s;
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on DefineLocal"))?;
                stamp_self_name_if_closure(&v, s);
                frame.env.define(s, v);
            }
            Inst::Pop => {
                stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on Pop"))?;
            }
            Inst::JumpIfFalse(target) => {
                let target = *target;
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on JumpIfFalse"))?;
                if !v.is_truthy() {
                    frame.ip = target;
                }
            }
            Inst::Jump(target) => {
                frame.ip = *target;
            }
            Inst::Call(n) | Inst::TailCall(n) => {
                let n = *n;
                let is_tail = matches!(inst_ref, Inst::TailCall(_));
                let stack_len = stack.len();
                if stack_len < n + 1 {
                    return Err(VmError::new("stack underflow on Call"));
                }
                let func_idx = stack_len - n - 1;
                let args_start = func_idx + 1;
                // FAST PATH: peek at func without popping; pass args as a
                // slice into the stack — no per-Call Vec<Value> allocation.
                // Covers closure / builtin / builtinSyms / parameter (the
                // overwhelming majority of Call sites).
                // Capture the call-site span up front so error paths can
                // attach it cheaply (one Rc deref + indexed read per Call).
                let call_span = frame.spans.get(inst_ip).copied().unwrap_or(Span::DUMMY);
                let func_proc = match &stack[func_idx] {
                    Value::Procedure(p) => p.clone(),
                    other => {
                        return Err(VmError::new(format!(
                            "call to non-procedure ({})",
                            other.type_name()
                        ))
                        .with_span(call_span));
                    }
                };
                {
                    let any = func_proc.as_any();
                    if let Some(closure) = any.downcast_ref::<VmClosure>() {
                        if closure.tier.bump() {
                            fire_tier_up_hook(closure, &stack[args_start..stack_len]);
                        }
                        // JIT fast path: if a native pointer is
                        // installed and every arg is a Fixnum, run
                        // the JIT body. Falls through to bytecode on
                        // ABI mismatch or non-Fixnum args.
                        if !closure.jit_ptr().is_null() {
                            let arg_slice = &stack[args_start..stack_len];
                            if let Some(result) = try_dispatch_jit(closure, arg_slice) {
                                stack.truncate(func_idx);
                                stack.push(result);
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack
                                            .pop()
                                            .ok_or_else(|| VmError::new("empty stack at exit"));
                                    }
                                }
                                continue;
                            }
                        }
                        let lam = &closure.bc.lambdas[closure.lambda_idx];
                        if !lambda_arity_ok(lam, n) {
                            return Err(VmError::new(format!(
                                "arity mismatch: {} expected {}{}, got {}",
                                closure.name().unwrap_or("procedure"),
                                lam.params.len(),
                                if lam.rest.is_some() { "+" } else { "" },
                                n
                            ))
                            .with_span(call_span));
                        }
                        // Fast path: lambda body is a single 2-arg primop on
                        // params/consts. Skip Env+Frame allocation; just run
                        // the primop directly on the args sitting on the stack.
                        if let Some(fp) = &lam.fast {
                            let result = apply_fast_primop(fp, &stack[args_start..stack_len], syms)
                                .map_err(|e| e.with_span(call_span))?;
                            stack.truncate(func_idx);
                            stack.push(result);
                            if is_tail {
                                // Tail-call into a fast-primop body: result is
                                // the return value of the *current* frame too,
                                // so pop the frame just like Inst::Return would.
                                frames.pop();
                                if frames.is_empty() {
                                    return stack
                                        .pop()
                                        .ok_or_else(|| VmError::new("empty stack at exit"));
                                }
                            }
                            continue;
                        }
                        let new_env = Env::child(closure.env.clone());
                        for (i, name) in lam.params.iter().enumerate() {
                            new_env.define(*name, stack[args_start + i].clone());
                        }
                        if let Some(rest_name) = lam.rest {
                            let rest = &stack[args_start + lam.params.len()..stack_len];
                            new_env.define(rest_name, Value::list(rest.iter().cloned()));
                        }
                        stack.truncate(func_idx);
                        if is_tail {
                            let last = frames.last_mut().unwrap();
                            last.insts = lam.body.clone();
                            last.spans = lam.spans.clone();
                            last.ip = 0;
                            last.env = new_env;
                            last.bc = closure.bc.clone();
                        } else {
                            frames.push(Frame {
                                insts: lam.body.clone(),
                                spans: lam.spans.clone(),
                                ip: 0,
                                env: new_env,
                                bc: closure.bc.clone(),
                            });
                        }
                        continue;
                    }
                    if let Some(b) = any.downcast_ref::<VmBuiltin>() {
                        let name = b.name;
                        let raw = (b.f)(&stack[args_start..stack_len]);
                        let r = match raw {
                            Ok(v) => v,
                            Err(e) => {
                                return Err(builtin_err_to_raised(name, &e, syms, call_span));
                            }
                        };
                        stack.truncate(func_idx);
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-builtin"));
                            }
                        }
                        continue;
                    }
                    if let Some(h) = any.downcast_ref::<VmHostBuiltin>() {
                        let name = h.name;
                        let raw = (h.f)(&stack[args_start..stack_len]);
                        let r = match raw {
                            Ok(v) => v,
                            Err(e) => {
                                return Err(builtin_err_to_raised(name, &e, syms, call_span));
                            }
                        };
                        stack.truncate(func_idx);
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-host-builtin")
                                });
                            }
                        }
                        continue;
                    }
                    if let Some(b) = any.downcast_ref::<VmBuiltinSyms>() {
                        let name = b.name;
                        let raw = (b.f)(&stack[args_start..stack_len], syms);
                        let r = match raw {
                            Ok(v) => v,
                            Err(e) => {
                                return Err(builtin_err_to_raised(name, &e, syms, call_span));
                            }
                        };
                        stack.truncate(func_idx);
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-builtin"));
                            }
                        }
                        continue;
                    }
                    if let Some(param) = any.downcast_ref::<cs_core::Parameter>() {
                        let r = if n == 0 {
                            param.cell.borrow().clone()
                        } else if n == 1 {
                            let v = stack[args_start].clone();
                            *param.cell.borrow_mut() = v;
                            Value::Unspecified
                        } else {
                            return Err(VmError::new("parameter: 0 or 1 arg"));
                        };
                        stack.truncate(func_idx);
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-parameter"));
                            }
                        }
                        continue;
                    }
                    // Continuation invocation. Two paths:
                    // 1. Snapshot present (M8 iter 3+): RESTORE the
                    //    captured frames + stack, push the new value
                    //    as the call/cc result, resume the run loop.
                    //    Re-entry lands at the captured top frame's
                    //    next instruction.
                    // 2. No snapshot: fall back to the legacy
                    //    escape-only path via pending_escape unwind.
                    if let Some(k) = any.downcast_ref::<VmContinuation>() {
                        let v = if n == 0 {
                            Value::Unspecified
                        } else {
                            stack[args_start].clone()
                        };
                        // Snapshot-restore only fires once the
                        // originating call/cc has returned (in_flight
                        // false). While call/cc is still on the
                        // stack, take the legacy escape-only path so
                        // the handler at the call/cc unwinds via
                        // pending_escape — this preserves correct
                        // tear-down of with-exception-handler /
                        // dynamic-wind frames in between.
                        if !k.in_flight.get() {
                            if let Some(snap) = &k.snapshot {
                                *frames = (*snap.frames).clone();
                                *stack = (*snap.stack).clone();
                                stack.push(v);
                                continue;
                            }
                        }
                        set_pending_escape(k.id, v);
                        return Err(VmError::new("__escape__"));
                    }
                }
                // SLOW PATH: drain into Vec<Value> and pop func for HO marker
                // dispatch. (map/fold/filter/raise/with-exception-handler/...)
                let mut args: Vec<Value> = stack.drain(args_start..).collect();
                let mut func = stack
                    .pop()
                    .ok_or_else(|| VmError::new("missing function on Call"))?;
                // SLOW PATH: HO marker dispatch (map/fold/filter/raise/...).
                // Native HO: (map proc list) — produce a list.
                if let Value::Procedure(p) = &func {
                    if p.as_any().downcast_ref::<VmMap>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("map: needs proc + list"));
                        }
                        let proc_val = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut out = Vec::with_capacity(n);
                        // Hoist the dispatch: when proc is a plain VmBuiltin,
                        // call its fn pointer directly per element instead of
                        // re-doing the match/downcast inside vm_call_sync each
                        // iteration.
                        let direct_fn: Option<VmBuiltinFn> = match &proc_val {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if lists.len() == 1 {
                            let list = &lists[0];
                            if let Some(f) = direct_fn {
                                for item in list {
                                    let r = f(std::slice::from_ref(item)).map_err(|e| {
                                        builtin_err_to_raised("map", &e, syms, call_span)
                                    })?;
                                    out.push(r);
                                }
                            } else {
                                for item in list {
                                    let r =
                                        vm_call_sync(&proc_val, std::slice::from_ref(item), syms)?;
                                    out.push(r);
                                }
                            }
                        } else {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len());
                            if let Some(f) = direct_fn {
                                for i in 0..n {
                                    row.clear();
                                    for l in &lists {
                                        row.push(l[i].clone());
                                    }
                                    let r = f(&row).map_err(|e| {
                                        builtin_err_to_raised("map", &e, syms, call_span)
                                    })?;
                                    out.push(r);
                                }
                            } else {
                                for i in 0..n {
                                    row.clear();
                                    for l in &lists {
                                        row.push(l[i].clone());
                                    }
                                    let r = vm_call_sync(&proc_val, &row, syms)?;
                                    out.push(r);
                                }
                            }
                        }
                        let result = Value::list(out);
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-map"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmForEach>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("for-each: needs proc + list"));
                        }
                        let proc_val = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        if lists.len() == 1 {
                            for item in &lists[0] {
                                vm_call_sync(&proc_val, std::slice::from_ref(item), syms)?;
                            }
                        } else {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len());
                            for i in 0..n {
                                row.clear();
                                for l in &lists {
                                    row.push(l[i].clone());
                                }
                                vm_call_sync(&proc_val, &row, syms)?;
                            }
                        }
                        stack.push(Value::Unspecified);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-for-each"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFilter>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("filter: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let mut kept = Vec::new();
                        let direct_fn: Option<VmBuiltinFn> = match &pred {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if let Some(f) = direct_fn {
                            let mut row = [Value::Unspecified];
                            for item in items {
                                row[0] = item.clone();
                                let r = f(&row).map_err(|e| {
                                    builtin_err_to_raised("filter", &e, syms, call_span)
                                })?;
                                if r.is_truthy() {
                                    kept.push(item);
                                }
                            }
                        } else {
                            for item in items {
                                let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
                                if r.is_truthy() {
                                    kept.push(item);
                                }
                            }
                        }
                        stack.push(Value::list(kept));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-filter"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFind>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("find: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let mut found = Value::Boolean(false);
                        let direct_fn: Option<VmBuiltinFn> = match &pred {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if let Some(f) = direct_fn {
                            let mut row = [Value::Unspecified];
                            for item in items {
                                row[0] = item.clone();
                                let r = f(&row).map_err(|e| {
                                    builtin_err_to_raised("find", &e, syms, call_span)
                                })?;
                                if r.is_truthy() {
                                    found = item;
                                    break;
                                }
                            }
                        } else {
                            for item in items {
                                let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
                                if r.is_truthy() {
                                    found = item;
                                    break;
                                }
                            }
                        }
                        stack.push(found);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-find"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmAny>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("any: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut result = Value::Boolean(false);
                        let direct_fn: Option<VmBuiltinFn> = match &pred {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if let Some(f) = direct_fn {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len());
                            for i in 0..n {
                                row.clear();
                                for l in &lists {
                                    row.push(l[i].clone());
                                }
                                let r = f(&row).map_err(|e| {
                                    builtin_err_to_raised("any", &e, syms, call_span)
                                })?;
                                if r.is_truthy() {
                                    result = r;
                                    break;
                                }
                            }
                        } else {
                            for i in 0..n {
                                let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                                let r = vm_call_sync(&pred, &row, syms)?;
                                if r.is_truthy() {
                                    result = r;
                                    break;
                                }
                            }
                        }
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-any"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmEvery>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("every: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut result = Value::Boolean(true);
                        let direct_fn: Option<VmBuiltinFn> = match &pred {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if let Some(f) = direct_fn {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len());
                            for i in 0..n {
                                row.clear();
                                for l in &lists {
                                    row.push(l[i].clone());
                                }
                                let r = f(&row).map_err(|e| {
                                    builtin_err_to_raised("every", &e, syms, call_span)
                                })?;
                                if !r.is_truthy() {
                                    result = Value::Boolean(false);
                                    break;
                                }
                                result = r;
                            }
                        } else {
                            for i in 0..n {
                                let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                                let r = vm_call_sync(&pred, &row, syms)?;
                                if !r.is_truthy() {
                                    result = Value::Boolean(false);
                                    break;
                                }
                                result = r;
                            }
                        }
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-every"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFoldLeft>().is_some() {
                        if args.len() < 3 {
                            return Err(VmError::new("fold-left: needs proc + init + list"));
                        }
                        let proc_val = args.remove(0);
                        let mut acc = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        // Hoist the dispatch: when proc is a known plain
                        // VmBuiltin like `+`, grab the fn pointer once and
                        // skip the per-iteration vm_call_sync match/downcast.
                        let direct_fn: Option<VmBuiltinFn> = match &proc_val {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if lists.len() == 1 {
                            // Fast path: single list. Reuse a 2-slot row buf.
                            let list = &lists[0];
                            let mut row: [Value; 2] = [Value::Unspecified, Value::Unspecified];
                            if let Some(f) = direct_fn {
                                for item in list {
                                    row[0] = acc;
                                    row[1] = item.clone();
                                    acc = f(&row).map_err(|e| {
                                        builtin_err_to_raised("fold-left", &e, syms, call_span)
                                    })?;
                                }
                            } else {
                                for item in list {
                                    row[0] = acc;
                                    row[1] = item.clone();
                                    acc = vm_call_sync(&proc_val, &row, syms)?;
                                }
                            }
                        } else {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len() + 1);
                            if let Some(f) = direct_fn {
                                for i in 0..n {
                                    row.clear();
                                    row.push(acc.clone());
                                    for l in &lists {
                                        row.push(l[i].clone());
                                    }
                                    acc = f(&row).map_err(|e| {
                                        builtin_err_to_raised("fold-left", &e, syms, call_span)
                                    })?;
                                }
                            } else {
                                for i in 0..n {
                                    row.clear();
                                    row.push(acc.clone());
                                    for l in &lists {
                                        row.push(l[i].clone());
                                    }
                                    acc = vm_call_sync(&proc_val, &row, syms)?;
                                }
                            }
                        }
                        stack.push(acc);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-fold-left"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFoldRight>().is_some() {
                        if args.len() < 3 {
                            return Err(VmError::new("fold-right: needs proc + init + list"));
                        }
                        let proc_val = args.remove(0);
                        let init = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut acc = init;
                        // Hoist the dispatch (matches fold-left). When the
                        // proc is a known plain VmBuiltin like `cons`, grab
                        // the fn pointer once and skip the per-iteration
                        // vm_call_sync match/downcast.
                        let direct_fn: Option<VmBuiltinFn> = match &proc_val {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if lists.len() == 1 {
                            let list = &lists[0];
                            let mut row: [Value; 2] = [Value::Unspecified, Value::Unspecified];
                            if let Some(f) = direct_fn {
                                for item in list.iter().take(n).rev() {
                                    row[0] = item.clone();
                                    row[1] = acc;
                                    acc = f(&row).map_err(|e| {
                                        builtin_err_to_raised("fold-right", &e, syms, call_span)
                                    })?;
                                }
                            } else {
                                for item in list.iter().take(n).rev() {
                                    row[0] = item.clone();
                                    row[1] = acc;
                                    acc = vm_call_sync(&proc_val, &row, syms)?;
                                }
                            }
                        } else {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len() + 1);
                            if let Some(f) = direct_fn {
                                for i in (0..n).rev() {
                                    row.clear();
                                    for l in &lists {
                                        row.push(l[i].clone());
                                    }
                                    row.push(acc);
                                    acc = f(&row).map_err(|e| {
                                        builtin_err_to_raised("fold-right", &e, syms, call_span)
                                    })?;
                                }
                            } else {
                                for i in (0..n).rev() {
                                    row.clear();
                                    for l in &lists {
                                        row.push(l[i].clone());
                                    }
                                    row.push(acc);
                                    acc = vm_call_sync(&proc_val, &row, syms)?;
                                }
                            }
                        }
                        stack.push(acc);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-fold-right"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmReduce>().is_some() {
                        if args.len() != 3 {
                            return Err(VmError::new("reduce: needs proc + default + list"));
                        }
                        let proc_val = args.remove(0);
                        let default = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let result = if items.is_empty() {
                            default
                        } else {
                            let mut acc = items[0].clone();
                            for item in &items[1..] {
                                acc = vm_call_sync(&proc_val, &[acc, item.clone()], syms)?;
                            }
                            acc
                        };
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-reduce"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCount>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("count: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut total: i64 = 0;
                        let direct_fn: Option<VmBuiltinFn> = match &pred {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if let Some(f) = direct_fn {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len());
                            for i in 0..n {
                                row.clear();
                                for l in &lists {
                                    row.push(l[i].clone());
                                }
                                let r = f(&row).map_err(|e| {
                                    builtin_err_to_raised("count", &e, syms, call_span)
                                })?;
                                if r.is_truthy() {
                                    total += 1;
                                }
                            }
                        } else {
                            for i in 0..n {
                                let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                                let r = vm_call_sync(&pred, &row, syms)?;
                                if r.is_truthy() {
                                    total += 1;
                                }
                            }
                        }
                        stack.push(Value::fixnum(total));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-count"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmPartition>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("partition: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let mut yes = Vec::new();
                        let mut no = Vec::new();
                        let direct_fn: Option<VmBuiltinFn> = match &pred {
                            Value::Procedure(p) => {
                                p.as_any().downcast_ref::<VmBuiltin>().map(|b| b.f)
                            }
                            _ => None,
                        };
                        if let Some(f) = direct_fn {
                            let mut row = [Value::Unspecified];
                            for item in items {
                                row[0] = item.clone();
                                let r = f(&row).map_err(|e| {
                                    builtin_err_to_raised("partition", &e, syms, call_span)
                                })?;
                                if r.is_truthy() {
                                    yes.push(item);
                                } else {
                                    no.push(item);
                                }
                            }
                        } else {
                            for item in items {
                                let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
                                if r.is_truthy() {
                                    yes.push(item);
                                } else {
                                    no.push(item);
                                }
                            }
                        }
                        set_pending_values(vec![Value::list(yes), Value::list(no)]);
                        stack.push(Value::Unspecified);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-partition"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmValues>().is_some() {
                        if args.len() == 1 {
                            stack.push(args.remove(0));
                        } else {
                            set_pending_values(args.clone());
                            stack.push(Value::Unspecified);
                        }
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-values"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCallWithValues>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("call-with-values: 2 args"));
                        }
                        let producer = args.remove(0);
                        let consumer = args.remove(0);
                        let prev = take_pending_values();
                        let prod_result = vm_call_sync(&producer, &[], syms)?;
                        let values = if let Some(vs) = take_pending_values() {
                            vs
                        } else {
                            vec![prod_result]
                        };
                        if let Some(prev) = prev {
                            set_pending_values(prev);
                        }
                        let r = vm_call_sync(&consumer, &values, syms)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-call-with-values")
                                });
                            }
                        }
                        continue;
                    }
                    // Vector / string / hashtable / sort / unfold HO ops.
                    if is_pure_ho_marker(p.as_ref()) {
                        let r = ho_apply(&func, &args, syms)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-ho"));
                            }
                        }
                        continue;
                    }
                    // `raise` / `error` / `with-exception-handler`.
                    if p.as_any().downcast_ref::<VmRaise>().is_some() {
                        if args.len() != 1 {
                            return Err(VmError::new("raise: 1 arg"));
                        }
                        set_pending_raise(args.remove(0));
                        return Err(VmError::new("__raised__"));
                    }
                    if p.as_any().downcast_ref::<VmErrorFn>().is_some() {
                        if args.is_empty() {
                            return Err(VmError::new("error: needs at least 1 arg"));
                        }
                        // Same who-detection as the walker tier: a leading
                        // symbol/#f/string-with-string-following is `who`;
                        // otherwise treat args[0] as the message.
                        let take_who = matches!(&args[0], Value::Symbol(_) | Value::Boolean(false))
                            || (matches!(&args[0], Value::String(_))
                                && args.len() >= 2
                                && matches!(&args[1], Value::String(_)));
                        let who = if take_who { Some(args.remove(0)) } else { None };
                        let msg = if !args.is_empty() {
                            match &args[0] {
                                Value::String(s) => s.borrow().clone(),
                                other => format!("{}", other),
                            }
                        } else {
                            "error".to_string()
                        };
                        let irritants: Vec<Value> = if !args.is_empty() {
                            args.drain(1..).collect()
                        } else {
                            Vec::new()
                        };
                        set_pending_raise(make_vm_error_condition(who, msg, irritants));
                        return Err(VmError::new("__raised__"));
                    }
                    if p.as_any().downcast_ref::<VmAssertionViolation>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new(
                                "assertion-violation: needs at least <who> and <message>",
                            ));
                        }
                        let who = args.remove(0);
                        let msg = match &args[0] {
                            Value::String(s) => s.borrow().clone(),
                            other => format!("{}", other),
                        };
                        let irritants: Vec<Value> = args.drain(1..).collect();
                        set_pending_raise(make_vm_assertion_violation_condition(
                            who, msg, irritants,
                        ));
                        return Err(VmError::new("__raised__"));
                    }
                    if p.as_any()
                        .downcast_ref::<VmWithExceptionHandler>()
                        .is_some()
                    {
                        if args.len() != 2 {
                            return Err(VmError::new("with-exception-handler: 2 args"));
                        }
                        let handler = args.remove(0);
                        let thunk = args.remove(0);
                        let prev = take_pending_raise();
                        let res = vm_call_sync(&thunk, &[], syms);
                        let final_val = match res {
                            Ok(v) => {
                                if let Some(prev) = prev {
                                    set_pending_raise(prev);
                                }
                                v
                            }
                            Err(e) => {
                                if e.message == "__raised__" {
                                    let cond =
                                        take_pending_raise().unwrap_or(Value::Boolean(false));
                                    if let Some(prev) = prev {
                                        set_pending_raise(prev);
                                    }
                                    // If the handler itself raises, repropagate.
                                    match vm_call_sync(&handler, &[cond], syms) {
                                        Ok(v) => v,
                                        Err(e2) => {
                                            return Err(e2);
                                        }
                                    }
                                } else {
                                    if let Some(prev) = prev {
                                        set_pending_raise(prev);
                                    }
                                    return Err(e);
                                }
                            }
                        };
                        stack.push(final_val);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-with-exception-handler")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmEval>().is_some() {
                        if args.is_empty() || args.len() > 2 {
                            return Err(VmError::new("eval: 1 or 2 args"));
                        }
                        // Ignore env arg (foundation: always top-level).
                        let v = args.remove(0);
                        let r = run_eval_hook(&v, syms)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-eval"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmDisplay>().is_some() {
                        if args.is_empty() || args.len() > 2 {
                            return Err(VmError::new("display: 1 or 2 args"));
                        }
                        let s = args[0].format_with(syms, cs_core::WriteMode::Display);
                        let explicit = if args.len() == 2 {
                            Some(args.remove(1))
                        } else {
                            None
                        };
                        let r = write_to_current_output(&s, explicit)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-display"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmWrite>().is_some() {
                        if args.is_empty() || args.len() > 2 {
                            return Err(VmError::new("write: 1 or 2 args"));
                        }
                        let s = args[0].format_with(syms, cs_core::WriteMode::Write);
                        let explicit = if args.len() == 2 {
                            Some(args.remove(1))
                        } else {
                            None
                        };
                        let r = write_to_current_output(&s, explicit)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-write"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmNewline>().is_some() {
                        if args.len() > 1 {
                            return Err(VmError::new("newline: 0 or 1 arg"));
                        }
                        let explicit = if args.len() == 1 {
                            Some(args.remove(0))
                        } else {
                            None
                        };
                        let r = write_to_current_output("\n", explicit)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-newline"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCurrentInputPort>().is_some() {
                        if !args.is_empty() {
                            return Err(VmError::new("current-input-port: 0 args"));
                        }
                        stack.push(current_input_port().unwrap_or(Value::Unspecified));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-current-input-port")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCurrentOutputPort>().is_some() {
                        if !args.is_empty() {
                            return Err(VmError::new("current-output-port: 0 args"));
                        }
                        stack.push(current_output_port().unwrap_or(Value::Unspecified));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-current-output-port")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmWithOutputToString>().is_some() {
                        if args.len() != 1 {
                            return Err(VmError::new("with-output-to-string: 1 arg"));
                        }
                        let thunk = args.remove(0);
                        let port = cs_core::Port::string_output();
                        let port_val = Value::Port(port.clone());
                        let prev = swap_output_port(Some(port_val));
                        let res = vm_call_sync(&thunk, &[], syms);
                        swap_output_port(prev);
                        res?;
                        let collected = match &*port {
                            cs_core::Port::StringOutput(buf) => buf.borrow().clone(),
                            _ => unreachable!(),
                        };
                        stack.push(Value::string(collected));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-with-output-to-string")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmWithInputFromString>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("with-input-from-string: 2 args"));
                        }
                        let s = match &args[0] {
                            Value::String(s) => s.borrow().clone(),
                            other => {
                                return Err(VmError::new(format!(
                                    "with-input-from-string: expected string, got {}",
                                    other.type_name()
                                )));
                            }
                        };
                        let thunk = args.remove(1);
                        let port = Value::Port(cs_core::Port::string_input(&s));
                        let prev = swap_input_port(Some(port));
                        let res = vm_call_sync(&thunk, &[], syms);
                        swap_input_port(prev);
                        let v = res?;
                        stack.push(v);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-with-input-from-string")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmWithOutputToFile>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("with-output-to-file: 2 args"));
                        }
                        let path = match &args[0] {
                            Value::String(s) => s.borrow().clone(),
                            other => {
                                return Err(VmError::new(format!(
                                    "with-output-to-file: expected string, got {}",
                                    other.type_name()
                                )));
                            }
                        };
                        // Eager creation surfaces I/O errors before the
                        // thunk runs.
                        std::fs::write(&path, "").map_err(|e| {
                            VmError::new(format!(
                                "with-output-to-file: cannot create {}: {}",
                                path, e
                            ))
                        })?;
                        let thunk = args.remove(1);
                        let port = cs_core::Port::file_output(path.clone());
                        let port_val = Value::Port(port.clone());
                        let prev = swap_output_port(Some(port_val));
                        let res = vm_call_sync(&thunk, &[], syms);
                        swap_output_port(prev);
                        // Always flush, even on error.
                        if let cs_core::Port::FileOutput(state) = &*port {
                            let mut s = state.borrow_mut();
                            if !s.closed {
                                let buf = std::mem::take(&mut s.buf);
                                s.closed = true;
                                drop(s);
                                std::fs::write(&path, &buf).map_err(|e| {
                                    VmError::new(format!(
                                        "with-output-to-file: write {} failed: {}",
                                        path, e
                                    ))
                                })?;
                            }
                        }
                        let v = res?;
                        stack.push(v);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-with-output-to-file")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmWithInputFromFile>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("with-input-from-file: 2 args"));
                        }
                        let path = match &args[0] {
                            Value::String(s) => s.borrow().clone(),
                            other => {
                                return Err(VmError::new(format!(
                                    "with-input-from-file: expected string, got {}",
                                    other.type_name()
                                )));
                            }
                        };
                        let contents = std::fs::read_to_string(&path).map_err(|e| {
                            VmError::new(format!(
                                "with-input-from-file: cannot read {}: {}",
                                path, e
                            ))
                        })?;
                        let thunk = args.remove(1);
                        let port = Value::Port(cs_core::Port::string_input(&contents));
                        let prev = swap_input_port(Some(port));
                        let res = vm_call_sync(&thunk, &[], syms);
                        swap_input_port(prev);
                        let v = res?;
                        stack.push(v);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-with-input-from-file")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCallCc>().is_some() {
                        if args.len() != 1 {
                            return Err(VmError::new("call/cc: 1 arg"));
                        }
                        let proc_val = args.remove(0);
                        let id = next_continuation_id();
                        // Capture frames + stack at call/cc entry. The
                        // snapshot is what the runtime restores on
                        // continuation invocation. (M8 iter 3.)
                        let snapshot = Rc::new(VmContSnapshot {
                            frames: Rc::new(frames.clone()),
                            stack: Rc::new(stack.clone()),
                        });
                        let (k, k_handle) = make_vm_continuation_with_snapshot(id, snapshot);
                        let res = vm_call_sync(&proc_val, &[k], syms);
                        // The originating call/cc has now returned
                        // (either normally or via the escape path
                        // below). Clear in_flight so any later
                        // re-invocation takes the snapshot-restore
                        // path rather than the escape path.
                        k_handle.in_flight.set(false);
                        let v = match res {
                            Ok(v) => v,
                            Err(e) => {
                                if e.message == "__escape__" {
                                    if let Some((eid, val)) = take_pending_escape() {
                                        if eid == id {
                                            val
                                        } else {
                                            // Not ours — rethrow.
                                            set_pending_escape(eid, val);
                                            return Err(e);
                                        }
                                    } else {
                                        return Err(e);
                                    }
                                } else {
                                    return Err(e);
                                }
                            }
                        };
                        stack.push(v);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-call/cc"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmDynamicWind>().is_some() {
                        if args.len() != 3 {
                            return Err(VmError::new("dynamic-wind: 3 args"));
                        }
                        let before = args.remove(0);
                        let thunk = args.remove(0);
                        let after = args.remove(0);
                        // Call before, thunk, after; even on error, after must
                        // run. Tail-position semantics get the thunk's result.
                        vm_call_sync(&before, &[], syms)?;
                        let res = vm_call_sync(&thunk, &[], syms);
                        let after_res = vm_call_sync(&after, &[], syms);
                        // Surface thunk error first; otherwise after error.
                        let v = match (res, after_res) {
                            (Ok(v), Ok(_)) => v,
                            (Err(e), _) => return Err(e),
                            (Ok(_), Err(e)) => return Err(e),
                        };
                        stack.push(v);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-dynamic-wind")
                                });
                            }
                        }
                        continue;
                    }
                }
                // Handle (apply proc a1 a2 ... rest-list)
                if let Value::Procedure(p) = &func {
                    if p.as_any().downcast_ref::<VmApply>().is_some() {
                        if args.is_empty() {
                            return Err(VmError::new("apply: needs at least 2 arguments"));
                        }
                        // Last arg is the list to spread.
                        let list_arg = args.pop().unwrap();
                        let inner_proc = args.remove(0);
                        let mut spread = Vec::new();
                        let mut cur = list_arg;
                        loop {
                            match cur {
                                Value::Null => break,
                                Value::Pair(pair) => {
                                    spread.push(pair.car.borrow().clone());
                                    cur = pair.cdr.borrow().clone();
                                }
                                other => {
                                    return Err(VmError::new(format!(
                                        "apply: last arg must be a proper list, got {}",
                                        other.type_name()
                                    )));
                                }
                            }
                        }
                        // Replace args with: prefix + spread.
                        args.extend(spread);
                        func = inner_proc;
                        // After apply rewrite: if the new func is itself a HO
                        // marker or values/cwv, handle it directly via the
                        // shared helpers (the inline arms above already ran
                        // for the original `apply` proc, not the new one).
                        if let Value::Procedure(p2) = &func {
                            let any2 = p2.as_any();
                            if any2.downcast_ref::<VmMap>().is_some()
                                || any2.downcast_ref::<VmForEach>().is_some()
                                || any2.downcast_ref::<VmFilter>().is_some()
                                || any2.downcast_ref::<VmFind>().is_some()
                                || any2.downcast_ref::<VmAny>().is_some()
                                || any2.downcast_ref::<VmEvery>().is_some()
                                || any2.downcast_ref::<VmFoldLeft>().is_some()
                                || any2.downcast_ref::<VmFoldRight>().is_some()
                                || any2.downcast_ref::<VmReduce>().is_some()
                                || any2.downcast_ref::<VmCount>().is_some()
                                || any2.downcast_ref::<VmPartition>().is_some()
                                || is_pure_ho_marker(p2.as_ref())
                            {
                                let r = ho_apply(&func, &args, syms)?;
                                stack.push(r);
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-apply-ho")
                                        });
                                    }
                                }
                                continue;
                            }
                            if any2.downcast_ref::<VmValues>().is_some() {
                                if args.len() == 1 {
                                    stack.push(args.remove(0));
                                } else {
                                    set_pending_values(args.clone());
                                    stack.push(Value::Unspecified);
                                }
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-apply-values")
                                        });
                                    }
                                }
                                continue;
                            }
                            if any2.downcast_ref::<VmCallWithValues>().is_some() {
                                if args.len() != 2 {
                                    return Err(VmError::new("call-with-values: 2 args"));
                                }
                                let producer = args.remove(0);
                                let consumer = args.remove(0);
                                let prev = take_pending_values();
                                let prod_result = vm_call_sync(&producer, &[], syms)?;
                                let values = if let Some(vs) = take_pending_values() {
                                    vs
                                } else {
                                    vec![prod_result]
                                };
                                if let Some(prev) = prev {
                                    set_pending_values(prev);
                                }
                                let r = vm_call_sync(&consumer, &values, syms)?;
                                stack.push(r);
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-apply-cwv")
                                        });
                                    }
                                }
                                continue;
                            }
                        }
                        // Fall through to closure/builtin dispatch below.
                    }
                }
                match &func {
                    Value::Procedure(p) => {
                        let any = p.as_any();
                        // Parameter: 0 args reads, 1 arg writes.
                        if let Some(param) = any.downcast_ref::<cs_core::Parameter>() {
                            let r = if args.is_empty() {
                                param.cell.borrow().clone()
                            } else if args.len() == 1 {
                                *param.cell.borrow_mut() = args.remove(0);
                                Value::Unspecified
                            } else {
                                return Err(VmError::new("parameter: 0 or 1 arg"));
                            };
                            stack.push(r);
                            if is_tail {
                                frames.pop();
                                if frames.is_empty() {
                                    return stack.pop().ok_or_else(|| {
                                        VmError::new("empty stack at tail-parameter")
                                    });
                                }
                            }
                            continue;
                        }
                        if let Some(closure) = any.downcast_ref::<VmClosure>() {
                            if closure.tier.bump() {
                                fire_tier_up_hook(closure, &args);
                            }
                            if !closure.jit_ptr().is_null() {
                                if let Some(result) = try_dispatch_jit(closure, &args) {
                                    stack.push(result);
                                    if is_tail {
                                        frames.pop();
                                        if frames.is_empty() {
                                            return stack.pop().ok_or_else(|| {
                                                VmError::new("empty stack at tail-jit")
                                            });
                                        }
                                    }
                                    continue;
                                }
                            }
                            let lam = &closure.bc.lambdas[closure.lambda_idx];
                            if !lambda_arity_ok(lam, args.len()) {
                                return Err(VmError::new("arity mismatch"));
                            }
                            if let Some(fp) = &lam.fast {
                                let result = apply_fast_primop(fp, &args, syms)?;
                                stack.push(result);
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-fastclosure")
                                        });
                                    }
                                }
                                continue;
                            }
                            let new_env = Env::child(closure.env.clone());
                            for (name, v) in lam.params.iter().zip(args.iter()) {
                                new_env.define(*name, v.clone());
                            }
                            if let Some(rest_name) = lam.rest {
                                let rest = &args[lam.params.len()..];
                                new_env.define(rest_name, Value::list(rest.iter().cloned()));
                            }
                            if is_tail {
                                // Replace current frame instead of pushing.
                                let last = frames.last_mut().unwrap();
                                last.insts = lam.body.clone();
                                last.spans = lam.spans.clone();
                                last.ip = 0;
                                last.env = new_env;
                                last.bc = closure.bc.clone();
                            } else {
                                frames.push(Frame {
                                    insts: lam.body.clone(),
                                    spans: lam.spans.clone(),
                                    ip: 0,
                                    env: new_env,
                                    bc: closure.bc.clone(),
                                });
                            }
                        } else if let Some(b) = any.downcast_ref::<VmBuiltin>() {
                            let r = (b.f)(&args)
                                .map_err(|e| VmError::new(format!("{}: {}", b.name, e)))?;
                            stack.push(r);
                            if is_tail {
                                frames.pop();
                                if frames.is_empty() {
                                    return stack.pop().ok_or_else(|| {
                                        VmError::new("empty stack at tail-builtin")
                                    });
                                }
                            }
                        } else if let Some(b) = any.downcast_ref::<VmBuiltinSyms>() {
                            let r = (b.f)(&args, syms)
                                .map_err(|e| VmError::new(format!("{}: {}", b.name, e)))?;
                            stack.push(r);
                            if is_tail {
                                frames.pop();
                                if frames.is_empty() {
                                    return stack.pop().ok_or_else(|| {
                                        VmError::new("empty stack at tail-builtin")
                                    });
                                }
                            }
                        } else {
                            return Err(VmError::new(
                                "vm: unsupported procedure type (no cross-tier bridge)",
                            ));
                        }
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "call to non-procedure ({})",
                            other.type_name()
                        )));
                    }
                }
            }
            Inst::MakeClosure(idx) => {
                let cl = VmClosure {
                    lambda_idx: *idx,
                    env: frame.env.clone(),
                    bc: frame.bc.clone(),
                    tier: cs_jit::Tier::default(),
                    jit_ptr: Cell::new(std::ptr::null()),
                    jit_arity: Cell::new(0),
                    self_name: Cell::new(None),
                    jit_return_type: Cell::new(JIT_RT_FIXNUM),
                    jit_param_types: Cell::new(JIT_PARAM_TYPES_ALL_FIXNUM),
                    jit_deopt_count: Cell::new(0),
                    jit_call_count: Cell::new(0),
                };
                let p: Rc<dyn Procedure> = Rc::new(cl);
                stack.push(Value::Procedure(p));
            }
            Inst::Return => {
                // Ends current frame; preserve top of stack as return.
                frames.pop();
                if frames.is_empty() {
                    return stack
                        .pop()
                        .ok_or_else(|| VmError::new("empty stack on Return"));
                }
            }
            // ---- 2-arg fixnum primops (specialized fast paths) ----
            // Each pops b then a; on a Fixnum/Fixnum match runs the fast
            // path; otherwise falls back to the generic Number arithmetic
            // (which handles bignum/rational/flonum + reports type errors).
            Inst::AddFx2 => {
                fixnum_binop2(stack, &mut |a: i64, b: i64| a.checked_add(b)).or_else(|args| {
                    let r = generic_arith2(args, GenericArith::Add, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::SubFx2 => {
                fixnum_binop2(stack, &mut |a: i64, b: i64| a.checked_sub(b)).or_else(|args| {
                    let r = generic_arith2(args, GenericArith::Sub, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::MulFx2 => {
                fixnum_binop2(stack, &mut |a: i64, b: i64| a.checked_mul(b)).or_else(|args| {
                    let r = generic_arith2(args, GenericArith::Mul, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::LtFx2 => {
                fixnum_cmp2(stack, &mut |a: i64, b: i64| a < b).or_else(|args| {
                    let r = generic_cmp2(args, GenericCmp::Lt, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::LeFx2 => {
                fixnum_cmp2(stack, &mut |a: i64, b: i64| a <= b).or_else(|args| {
                    let r = generic_cmp2(args, GenericCmp::Le, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::GtFx2 => {
                fixnum_cmp2(stack, &mut |a: i64, b: i64| a > b).or_else(|args| {
                    let r = generic_cmp2(args, GenericCmp::Gt, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::GeFx2 => {
                fixnum_cmp2(stack, &mut |a: i64, b: i64| a >= b).or_else(|args| {
                    let r = generic_cmp2(args, GenericCmp::Ge, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            Inst::EqFx2 => {
                fixnum_cmp2(stack, &mut |a: i64, b: i64| a == b).or_else(|args| {
                    let r = generic_cmp2(args, GenericCmp::Eq, inst_ip, &frame.spans, syms)?;
                    stack.push(r);
                    Ok::<(), VmError>(())
                })?;
            }
            // Fused compare-and-branch. Each pops 2 args; on Fixnum/Fixnum
            // match, branches to `target` iff the *negated* comparison is
            // true (i.e., the original cond was false). Slow path
            // materializes a boolean via generic_cmp2 then does a normal
            // JumpIfFalse.
            Inst::BranchOnGeFx2(target) => {
                let target = *target;
                if !fxbranch(stack, |a, b| a >= b, target, &mut frame.ip) {
                    fallback_branch(
                        stack,
                        GenericCmp::Lt,
                        target,
                        inst_ip,
                        &frame.spans,
                        syms,
                        &mut frame.ip,
                    )?;
                }
            }
            Inst::BranchOnGtFx2(target) => {
                let target = *target;
                if !fxbranch(stack, |a, b| a > b, target, &mut frame.ip) {
                    fallback_branch(
                        stack,
                        GenericCmp::Le,
                        target,
                        inst_ip,
                        &frame.spans,
                        syms,
                        &mut frame.ip,
                    )?;
                }
            }
            Inst::BranchOnLeFx2(target) => {
                let target = *target;
                if !fxbranch(stack, |a, b| a <= b, target, &mut frame.ip) {
                    fallback_branch(
                        stack,
                        GenericCmp::Gt,
                        target,
                        inst_ip,
                        &frame.spans,
                        syms,
                        &mut frame.ip,
                    )?;
                }
            }
            Inst::BranchOnLtFx2(target) => {
                let target = *target;
                if !fxbranch(stack, |a, b| a < b, target, &mut frame.ip) {
                    fallback_branch(
                        stack,
                        GenericCmp::Ge,
                        target,
                        inst_ip,
                        &frame.spans,
                        syms,
                        &mut frame.ip,
                    )?;
                }
            }
            Inst::BranchOnNeFx2(target) => {
                let target = *target;
                if !fxbranch(stack, |a, b| a != b, target, &mut frame.ip) {
                    fallback_branch(
                        stack,
                        GenericCmp::Eq,
                        target,
                        inst_ip,
                        &frame.spans,
                        syms,
                        &mut frame.ip,
                    )?;
                }
            }
        }
    }
}

/// Fast-path fused branch: pop b, pop a; on (Fixnum, Fixnum), set ip to
/// target if `op(a, b)` and return true. Returns false if either arg
/// wasn't a fixnum — caller falls back to generic_cmp2.
fn fxbranch(
    stack: &mut Vec<Value>,
    op: impl Fn(i64, i64) -> bool,
    target: usize,
    ip: &mut usize,
) -> bool {
    let b = stack.pop().expect("stack underflow on fxbranch");
    let a = stack.pop().expect("stack underflow on fxbranch");
    if let (
        Value::Number(cs_core::Number::Fixnum(av)),
        Value::Number(cs_core::Number::Fixnum(bv)),
    ) = (&a, &b)
    {
        if op(*av, *bv) {
            *ip = target;
        }
        return true;
    }
    // Non-fixnum: re-push so the slow path can recover.
    stack.push(a);
    stack.push(b);
    false
}

/// Slow-path fallback for compare+branch when args aren't both fixnums.
/// Computes the original (un-negated) comparison via generic_cmp2; if it
/// is false, branches to target. (`op` here is the *original* comparison,
/// not the negated branch trigger — matches the un-fused
/// `generic_cmp2 + JumpIfFalse` semantics.)
fn fallback_branch(
    stack: &mut Vec<Value>,
    op: GenericCmp,
    target: usize,
    inst_ip: usize,
    spans: &[Span],
    syms: &mut SymbolTable,
    ip: &mut usize,
) -> Result<(), VmError> {
    let b = stack.pop().expect("stack underflow on fallback");
    let a = stack.pop().expect("stack underflow on fallback");
    let result = generic_cmp2((a, b), op, inst_ip, spans, syms)?;
    if !result.is_truthy() {
        *ip = target;
    }
    Ok(())
}

/// Fast-path arithmetic on two fixnums. On (Fixnum, Fixnum) where the op
/// produces no overflow, pushes the result and returns Ok(()). Otherwise
/// returns Err((a, b)) with the original values for the slow path.
/// Run a fast-primop body directly on `args`, without allocating an Env or
/// Frame. Mirrors the inline AddFx2/.../EqFx2 dispatch arms: tries a fixnum
/// fast path; on miss, falls back to generic arith / cmp. Used by the call
/// sites whose lambda's body is a single primop on params/consts (very
/// common for map/fold callbacks).
fn apply_fast_primop(
    fp: &crate::opcode::FastPrimopBody,
    args: &[Value],
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    use crate::opcode::FastArg;
    let resolve = |fa: &FastArg| -> Value {
        match fa {
            FastArg::Param(i) => args[*i as usize].clone(),
            FastArg::Const(v) => v.clone(),
        }
    };
    let a = resolve(&fp.args[0]);
    let b = resolve(&fp.args[1]);
    // Fast path: both Fixnum. Mirrors the inline arms in the main dispatch.
    if let (
        Value::Number(cs_core::Number::Fixnum(av)),
        Value::Number(cs_core::Number::Fixnum(bv)),
    ) = (&a, &b)
    {
        let av = *av;
        let bv = *bv;
        match &fp.op {
            Inst::AddFx2 => {
                if let Some(r) = av.checked_add(bv) {
                    return Ok(Value::fixnum(r));
                }
            }
            Inst::SubFx2 => {
                if let Some(r) = av.checked_sub(bv) {
                    return Ok(Value::fixnum(r));
                }
            }
            Inst::MulFx2 => {
                if let Some(r) = av.checked_mul(bv) {
                    return Ok(Value::fixnum(r));
                }
            }
            Inst::LtFx2 => return Ok(Value::Boolean(av < bv)),
            Inst::LeFx2 => return Ok(Value::Boolean(av <= bv)),
            Inst::GtFx2 => return Ok(Value::Boolean(av > bv)),
            Inst::GeFx2 => return Ok(Value::Boolean(av >= bv)),
            Inst::EqFx2 => return Ok(Value::Boolean(av == bv)),
            _ => unreachable!("apply_fast_primop: non-primop op slot"),
        }
        // Fixnum overflow on Add/Sub/Mul: fall through to generic arith.
    }
    // Generic path: 1-element ad-hoc spans buffer so we can reuse the
    // existing helpers without faking a span vec.
    let spans = [fp.span];
    match &fp.op {
        Inst::AddFx2 => generic_arith2((a, b), GenericArith::Add, 0, &spans, syms),
        Inst::SubFx2 => generic_arith2((a, b), GenericArith::Sub, 0, &spans, syms),
        Inst::MulFx2 => generic_arith2((a, b), GenericArith::Mul, 0, &spans, syms),
        Inst::LtFx2 => generic_cmp2((a, b), GenericCmp::Lt, 0, &spans, syms),
        Inst::LeFx2 => generic_cmp2((a, b), GenericCmp::Le, 0, &spans, syms),
        Inst::GtFx2 => generic_cmp2((a, b), GenericCmp::Gt, 0, &spans, syms),
        Inst::GeFx2 => generic_cmp2((a, b), GenericCmp::Ge, 0, &spans, syms),
        Inst::EqFx2 => generic_cmp2((a, b), GenericCmp::Eq, 0, &spans, syms),
        _ => unreachable!("apply_fast_primop: non-primop op slot"),
    }
}

fn fixnum_binop2(
    stack: &mut Vec<Value>,
    op: &mut dyn FnMut(i64, i64) -> Option<i64>,
) -> Result<(), (Value, Value)> {
    let b = stack.pop().expect("stack underflow on fxop");
    let a = stack.pop().expect("stack underflow on fxop");
    if let (
        Value::Number(cs_core::Number::Fixnum(av)),
        Value::Number(cs_core::Number::Fixnum(bv)),
    ) = (&a, &b)
    {
        if let Some(r) = op(*av, *bv) {
            stack.push(Value::fixnum(r));
            return Ok(());
        }
    }
    Err((a, b))
}

fn fixnum_cmp2(
    stack: &mut Vec<Value>,
    op: &mut dyn FnMut(i64, i64) -> bool,
) -> Result<(), (Value, Value)> {
    let b = stack.pop().expect("stack underflow on fxcmp");
    let a = stack.pop().expect("stack underflow on fxcmp");
    if let (
        Value::Number(cs_core::Number::Fixnum(av)),
        Value::Number(cs_core::Number::Fixnum(bv)),
    ) = (&a, &b)
    {
        stack.push(Value::Boolean(op(*av, *bv)));
        return Ok(());
    }
    Err((a, b))
}

#[derive(Clone, Copy)]
enum GenericArith {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Copy)]
enum GenericCmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

fn generic_arith2(
    (a, b): (Value, Value),
    op: GenericArith,
    inst_ip: usize,
    spans: &[Span],
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    let span = spans.get(inst_ip).copied().unwrap_or(Span::DUMMY);
    let name = op_arith_name(op);
    let an = match as_number(&a, name) {
        Ok(n) => n,
        Err(m) => return Err(builtin_err_to_raised(name, &m, syms, span)),
    };
    let bn = match as_number(&b, name) {
        Ok(n) => n,
        Err(m) => return Err(builtin_err_to_raised(name, &m, syms, span)),
    };
    let r = match op {
        GenericArith::Add => an.add(&bn),
        GenericArith::Sub => an.sub(&bn),
        GenericArith::Mul => an.mul(&bn),
    };
    Ok(Value::Number(r))
}

fn generic_cmp2(
    (a, b): (Value, Value),
    op: GenericCmp,
    inst_ip: usize,
    spans: &[Span],
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    let span = spans.get(inst_ip).copied().unwrap_or(Span::DUMMY);
    let name = op_cmp_name(op);
    let an = match as_number(&a, name) {
        Ok(n) => n,
        Err(m) => return Err(builtin_err_to_raised(name, &m, syms, span)),
    };
    let bn = match as_number(&b, name) {
        Ok(n) => n,
        Err(m) => return Err(builtin_err_to_raised(name, &m, syms, span)),
    };
    let ord = an.cmp(&bn);
    let result = match op {
        GenericCmp::Lt => ord == std::cmp::Ordering::Less,
        GenericCmp::Le => ord != std::cmp::Ordering::Greater,
        GenericCmp::Gt => ord == std::cmp::Ordering::Greater,
        GenericCmp::Ge => ord != std::cmp::Ordering::Less,
        GenericCmp::Eq => an.eq_value(&bn),
    };
    Ok(Value::Boolean(result))
}

fn as_number(v: &Value, name: &str) -> Result<cs_core::Number, String> {
    match v {
        Value::Number(n) => Ok(n.clone()),
        other => {
            // Include a short display of the offending value where it can
            // render without a SymbolTable. Symbols print as their handle
            // via Display, which is unhelpful — leave them off.
            let extra = match other {
                Value::String(_) | Value::Number(_) | Value::Boolean(_) | Value::Character(_) => {
                    let display = format!("{}", other);
                    let cap = 60;
                    let trimmed: String = if display.chars().count() > cap {
                        let head: String = display.chars().take(cap - 1).collect();
                        format!("{}…", head)
                    } else {
                        display
                    };
                    format!(" {}", trimmed)
                }
                _ => String::new(),
            };
            // Stash for the dispatch loop's condition builder so the
            // raised condition carries the offending value as an
            // &irritants simple.
            cs_core::stash_builtin_err_irritant(other.clone());
            Err(format!(
                "{}: expected number, got {}{}",
                name,
                other.type_name(),
                extra
            ))
        }
    }
}

fn op_arith_name(op: GenericArith) -> &'static str {
    match op {
        GenericArith::Add => "+",
        GenericArith::Sub => "-",
        GenericArith::Mul => "*",
    }
}

fn op_cmp_name(op: GenericCmp) -> &'static str {
    match op {
        GenericCmp::Lt => "<",
        GenericCmp::Le => "<=",
        GenericCmp::Gt => ">",
        GenericCmp::Ge => ">=",
        GenericCmp::Eq => "=",
    }
}

fn lambda_arity_ok(lam: &CompiledLambda, n: usize) -> bool {
    if lam.rest.is_some() {
        n >= lam.params.len()
    } else {
        n == lam.params.len()
    }
}

/// A simple builtin-procedure type for VM consumers. The VM dispatches it
/// when a `Call` finds a procedure whose underlying type is `VmBuiltin`.
/// Embedders constructing VM environments use [`make_vm_builtin`] to install.
pub type VmBuiltinFn = fn(&[Value]) -> Result<Value, String>;

/// Builtin requiring access to the symbol table (symbol↔string, gensym,
/// display/write that resolve symbol names).
pub type VmBuiltinSymsFn = fn(&[Value], &mut SymbolTable) -> Result<Value, String>;

#[derive(Debug)]
pub struct VmBuiltin {
    pub name: &'static str,
    pub f: VmBuiltinFn,
}

impl Procedure for VmBuiltin {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some(self.name)
    }
}

#[derive(Debug)]
pub struct VmBuiltinSyms {
    pub name: &'static str,
    pub f: VmBuiltinSymsFn,
}

impl Procedure for VmBuiltinSyms {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some(self.name)
    }
}

pub fn make_vm_builtin(name: &'static str, f: VmBuiltinFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmBuiltin { name, f });
    Value::Procedure(p)
}

pub fn make_vm_builtin_syms(name: &'static str, f: VmBuiltinSymsFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmBuiltinSyms { name, f });
    Value::Procedure(p)
}

/// Boxed-closure VM builtin — for FFI host procedures whose
/// implementation captures state (an `Arc<dyn HostProcedure>`) and
/// therefore can't be a plain `fn` pointer. Handled symmetrically
/// with `VmBuiltin` in the dispatch loop.
#[allow(clippy::type_complexity)]
pub struct VmHostBuiltin {
    pub name: &'static str,
    pub f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync>,
}

impl std::fmt::Debug for VmHostBuiltin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VmHostBuiltin({})", self.name)
    }
}

impl Procedure for VmHostBuiltin {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some(self.name)
    }
}

pub fn make_vm_host_builtin(
    name: &'static str,
    f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync>,
) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmHostBuiltin { name, f });
    Value::Procedure(p)
}

/// Marker for the `apply` builtin. The VM call dispatch recognizes this
/// type and spreads the last arg (a list) before calling the inner procedure.
#[derive(Debug)]
pub struct VmApply;

impl Procedure for VmApply {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("apply")
    }
}

pub fn make_vm_apply() -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmApply);
    Value::Procedure(p)
}

/// Marker types for native HO builtins that iterate (map/for-each/filter/find).
#[derive(Debug)]
pub struct VmMap;
#[derive(Debug)]
pub struct VmForEach;
#[derive(Debug)]
pub struct VmFilter;
#[derive(Debug)]
pub struct VmFind;
#[derive(Debug)]
pub struct VmAny;
#[derive(Debug)]
pub struct VmEvery;

impl Procedure for VmMap {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("map")
    }
}
impl Procedure for VmForEach {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("for-each")
    }
}
impl Procedure for VmFilter {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("filter")
    }
}
impl Procedure for VmFind {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("find")
    }
}
impl Procedure for VmAny {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("any")
    }
}
impl Procedure for VmEvery {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("every")
    }
}

pub fn make_vm_map() -> Value {
    Value::Procedure(Rc::new(VmMap) as Rc<dyn Procedure>)
}
pub fn make_vm_for_each() -> Value {
    Value::Procedure(Rc::new(VmForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_filter() -> Value {
    Value::Procedure(Rc::new(VmFilter) as Rc<dyn Procedure>)
}
pub fn make_vm_find() -> Value {
    Value::Procedure(Rc::new(VmFind) as Rc<dyn Procedure>)
}
pub fn make_vm_any() -> Value {
    Value::Procedure(Rc::new(VmAny) as Rc<dyn Procedure>)
}
pub fn make_vm_every() -> Value {
    Value::Procedure(Rc::new(VmEvery) as Rc<dyn Procedure>)
}

/// Additional native HO marker types.
#[derive(Debug)]
pub struct VmFoldLeft;
#[derive(Debug)]
pub struct VmFoldRight;
#[derive(Debug)]
pub struct VmReduce;
#[derive(Debug)]
pub struct VmCount;
#[derive(Debug)]
pub struct VmPartition;
#[derive(Debug)]
pub struct VmValues;
#[derive(Debug)]
pub struct VmCallWithValues;

impl Procedure for VmFoldLeft {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("fold-left")
    }
}
impl Procedure for VmFoldRight {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("fold-right")
    }
}
impl Procedure for VmReduce {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("reduce")
    }
}
impl Procedure for VmCount {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("count")
    }
}
impl Procedure for VmPartition {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("partition")
    }
}
impl Procedure for VmValues {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("values")
    }
}
impl Procedure for VmCallWithValues {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("call-with-values")
    }
}

pub fn make_vm_fold_left() -> Value {
    Value::Procedure(Rc::new(VmFoldLeft) as Rc<dyn Procedure>)
}
pub fn make_vm_fold_right() -> Value {
    Value::Procedure(Rc::new(VmFoldRight) as Rc<dyn Procedure>)
}
pub fn make_vm_reduce() -> Value {
    Value::Procedure(Rc::new(VmReduce) as Rc<dyn Procedure>)
}
pub fn make_vm_count() -> Value {
    Value::Procedure(Rc::new(VmCount) as Rc<dyn Procedure>)
}
pub fn make_vm_partition() -> Value {
    Value::Procedure(Rc::new(VmPartition) as Rc<dyn Procedure>)
}
pub fn make_vm_values() -> Value {
    Value::Procedure(Rc::new(VmValues) as Rc<dyn Procedure>)
}
pub fn make_vm_call_with_values() -> Value {
    Value::Procedure(Rc::new(VmCallWithValues) as Rc<dyn Procedure>)
}

/// Vector / string / hashtable HO markers.
#[derive(Debug)]
pub struct VmVectorMap;
#[derive(Debug)]
pub struct VmVectorForEach;
#[derive(Debug)]
pub struct VmVectorFold;
#[derive(Debug)]
pub struct VmVectorFilter;
#[derive(Debug)]
pub struct VmStringMap;
#[derive(Debug)]
pub struct VmStringForEach;
#[derive(Debug)]
pub struct VmHashtableWalk;
#[derive(Debug)]
pub struct VmHashtableForEach;
#[derive(Debug)]
pub struct VmHashtableFold;
#[derive(Debug)]
pub struct VmHashtableUpdate;
#[derive(Debug)]
pub struct VmUnfold;
#[derive(Debug)]
pub struct VmListSort;
#[derive(Debug)]
pub struct VmVectorSort;
#[derive(Debug)]
pub struct VmVectorSortBang;

macro_rules! impl_proc_named {
    ($t:ty, $name:expr) => {
        impl Procedure for $t {
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn name(&self) -> Option<&str> {
                Some($name)
            }
        }
    };
}
impl_proc_named!(VmVectorMap, "vector-map");
impl_proc_named!(VmVectorForEach, "vector-for-each");
impl_proc_named!(VmVectorFold, "vector-fold");
impl_proc_named!(VmVectorFilter, "vector-filter");
impl_proc_named!(VmStringMap, "string-map");
impl_proc_named!(VmStringForEach, "string-for-each");
impl_proc_named!(VmHashtableWalk, "hashtable-walk");
impl_proc_named!(VmHashtableForEach, "hashtable-for-each");
impl_proc_named!(VmHashtableFold, "hashtable-fold");
impl_proc_named!(VmHashtableUpdate, "hashtable-update!");
impl_proc_named!(VmUnfold, "unfold");
impl_proc_named!(VmListSort, "list-sort");
impl_proc_named!(VmVectorSort, "vector-sort");
impl_proc_named!(VmVectorSortBang, "vector-sort!");

#[derive(Debug)]
pub struct VmTabulate;
#[derive(Debug)]
pub struct VmRemove;
#[derive(Debug)]
pub struct VmForce;
impl_proc_named!(VmTabulate, "tabulate");
impl_proc_named!(VmRemove, "remove");
impl_proc_named!(VmForce, "force");
pub fn make_vm_tabulate() -> Value {
    Value::Procedure(Rc::new(VmTabulate) as Rc<dyn Procedure>)
}
pub fn make_vm_remove() -> Value {
    Value::Procedure(Rc::new(VmRemove) as Rc<dyn Procedure>)
}
pub fn make_vm_force() -> Value {
    Value::Procedure(Rc::new(VmForce) as Rc<dyn Procedure>)
}

/// `eval` marker: dispatches to the installed VmEvalHook.
#[derive(Debug)]
pub struct VmEval;
impl_proc_named!(VmEval, "eval");
pub fn make_vm_eval() -> Value {
    Value::Procedure(Rc::new(VmEval) as Rc<dyn Procedure>)
}

/// I/O port-state markers.
#[derive(Debug)]
pub struct VmDisplay;
#[derive(Debug)]
pub struct VmWrite;
#[derive(Debug)]
pub struct VmNewline;
#[derive(Debug)]
pub struct VmWithOutputToString;
#[derive(Debug)]
pub struct VmWithInputFromString;
#[derive(Debug)]
pub struct VmWithOutputToFile;
#[derive(Debug)]
pub struct VmWithInputFromFile;
#[derive(Debug)]
pub struct VmCurrentInputPort;
#[derive(Debug)]
pub struct VmCurrentOutputPort;
impl_proc_named!(VmDisplay, "display");
impl_proc_named!(VmWrite, "write");
impl_proc_named!(VmNewline, "newline");
impl_proc_named!(VmWithOutputToString, "with-output-to-string");
impl_proc_named!(VmWithInputFromString, "with-input-from-string");
impl_proc_named!(VmWithOutputToFile, "with-output-to-file");
impl_proc_named!(VmWithInputFromFile, "with-input-from-file");
impl_proc_named!(VmCurrentInputPort, "current-input-port");
impl_proc_named!(VmCurrentOutputPort, "current-output-port");
pub fn make_vm_display() -> Value {
    Value::Procedure(Rc::new(VmDisplay) as Rc<dyn Procedure>)
}
pub fn make_vm_write() -> Value {
    Value::Procedure(Rc::new(VmWrite) as Rc<dyn Procedure>)
}
pub fn make_vm_newline() -> Value {
    Value::Procedure(Rc::new(VmNewline) as Rc<dyn Procedure>)
}
pub fn make_vm_with_output_to_string() -> Value {
    Value::Procedure(Rc::new(VmWithOutputToString) as Rc<dyn Procedure>)
}
pub fn make_vm_with_output_to_file() -> Value {
    Value::Procedure(Rc::new(VmWithOutputToFile) as Rc<dyn Procedure>)
}
pub fn make_vm_with_input_from_file() -> Value {
    Value::Procedure(Rc::new(VmWithInputFromFile) as Rc<dyn Procedure>)
}
pub fn make_vm_with_input_from_string() -> Value {
    Value::Procedure(Rc::new(VmWithInputFromString) as Rc<dyn Procedure>)
}
pub fn make_vm_current_input_port() -> Value {
    Value::Procedure(Rc::new(VmCurrentInputPort) as Rc<dyn Procedure>)
}
pub fn make_vm_current_output_port() -> Value {
    Value::Procedure(Rc::new(VmCurrentOutputPort) as Rc<dyn Procedure>)
}

fn write_to_current_output(s: &str, explicit_port: Option<Value>) -> Result<Value, VmError> {
    let target = explicit_port.or_else(current_output_port);
    match target {
        Some(Value::Port(p)) => match &*p {
            cs_core::Port::StringOutput(buf) => {
                buf.borrow_mut().push_str(s);
                Ok(Value::Unspecified)
            }
            cs_core::Port::FileOutput(state) => {
                let mut st = state.borrow_mut();
                if st.closed {
                    return Err(VmError::new("write/display: port is closed"));
                }
                st.buf.extend_from_slice(s.as_bytes());
                Ok(Value::Unspecified)
            }
            _ => Err(VmError::new("write/display: not an output port")),
        },
        Some(other) => Err(VmError::new(format!(
            "write/display: expected port, got {}",
            other.type_name()
        ))),
        None => {
            print!("{}", s);
            Ok(Value::Unspecified)
        }
    }
}

pub fn make_vm_vector_map() -> Value {
    Value::Procedure(Rc::new(VmVectorMap) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_for_each() -> Value {
    Value::Procedure(Rc::new(VmVectorForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_fold() -> Value {
    Value::Procedure(Rc::new(VmVectorFold) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_filter() -> Value {
    Value::Procedure(Rc::new(VmVectorFilter) as Rc<dyn Procedure>)
}
pub fn make_vm_string_map() -> Value {
    Value::Procedure(Rc::new(VmStringMap) as Rc<dyn Procedure>)
}
pub fn make_vm_string_for_each() -> Value {
    Value::Procedure(Rc::new(VmStringForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_walk() -> Value {
    Value::Procedure(Rc::new(VmHashtableWalk) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_for_each() -> Value {
    Value::Procedure(Rc::new(VmHashtableForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_fold() -> Value {
    Value::Procedure(Rc::new(VmHashtableFold) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_update() -> Value {
    Value::Procedure(Rc::new(VmHashtableUpdate) as Rc<dyn Procedure>)
}
pub fn make_vm_unfold() -> Value {
    Value::Procedure(Rc::new(VmUnfold) as Rc<dyn Procedure>)
}
pub fn make_vm_list_sort() -> Value {
    Value::Procedure(Rc::new(VmListSort) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_sort() -> Value {
    Value::Procedure(Rc::new(VmVectorSort) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_sort_bang() -> Value {
    Value::Procedure(Rc::new(VmVectorSortBang) as Rc<dyn Procedure>)
}

/// Exception support markers.
#[derive(Debug)]
pub struct VmRaise;
#[derive(Debug)]
pub struct VmErrorFn;
#[derive(Debug)]
pub struct VmAssertionViolation;
#[derive(Debug)]
pub struct VmWithExceptionHandler;
#[derive(Debug)]
pub struct VmCallCc;
#[derive(Debug)]
pub struct VmDynamicWind;

/// Escape-only continuation produced by `call/cc`. Holds the unique id
/// installed by the originating call/cc; invoking it triggers a VmError
/// with `__escape__:<id>` and stashes the value in VM_PENDING_ESCAPE.
#[derive(Debug)]
pub struct VmContinuation {
    pub id: u64,
    /// Captured frame + value-stack snapshot. `Some` when the
    /// continuation was created by an in-flight `call/cc` (M8 iter 3+);
    /// `None` for the legacy escape-only path that the runtime
    /// builds in places that don't have a snapshot at hand.
    pub snapshot: Option<Rc<VmContSnapshot>>,
    /// True while the originating `call/cc` is still on the call
    /// stack. Cleared by the call/cc handler when it returns
    /// (normal or via escape). The dispatch site uses this to
    /// distinguish:
    /// - **In-flight** (true): take the legacy escape-only path so
    ///   the handler at the call/cc unwinds via `pending_escape`
    ///   and any active `with-exception-handler` / `dynamic-wind`
    ///   frames in between get torn down correctly.
    /// - **After extent** (false): take the snapshot-restore path
    ///   so the captured context resumes as a fresh continuation
    ///   re-entry.
    pub in_flight: Cell<bool>,
}

impl_proc_named!(VmRaise, "raise");
impl_proc_named!(VmErrorFn, "error");
impl_proc_named!(VmAssertionViolation, "assertion-violation");
impl_proc_named!(VmWithExceptionHandler, "with-exception-handler");
impl_proc_named!(VmCallCc, "call/cc");
impl_proc_named!(VmDynamicWind, "dynamic-wind");
impl_proc_named!(VmContinuation, "continuation");

pub fn make_vm_raise() -> Value {
    Value::Procedure(Rc::new(VmRaise) as Rc<dyn Procedure>)
}
pub fn make_vm_error_fn() -> Value {
    Value::Procedure(Rc::new(VmErrorFn) as Rc<dyn Procedure>)
}
pub fn make_vm_assertion_violation() -> Value {
    Value::Procedure(Rc::new(VmAssertionViolation) as Rc<dyn Procedure>)
}
pub fn make_vm_with_exception_handler() -> Value {
    Value::Procedure(Rc::new(VmWithExceptionHandler) as Rc<dyn Procedure>)
}
pub fn make_vm_dynamic_wind() -> Value {
    Value::Procedure(Rc::new(VmDynamicWind) as Rc<dyn Procedure>)
}
pub fn make_vm_call_cc() -> Value {
    Value::Procedure(Rc::new(VmCallCc) as Rc<dyn Procedure>)
}
pub fn make_vm_continuation(id: u64) -> Value {
    Value::Procedure(Rc::new(VmContinuation {
        id,
        snapshot: None,
        in_flight: Cell::new(true),
    }) as Rc<dyn Procedure>)
}

/// Construct a continuation with a captured snapshot (M8 iter 3+).
/// Starts with `in_flight = true`; the call/cc handler clears the
/// flag when it returns. After clearing, dispatch routes through
/// the snapshot-restore path.
///
/// Returns the `Value::Procedure` wrapping the continuation along
/// with the `Rc<VmContinuation>` for the call site to clear
/// in_flight on completion.
pub fn make_vm_continuation_with_snapshot(
    id: u64,
    snapshot: Rc<VmContSnapshot>,
) -> (Value, Rc<VmContinuation>) {
    let k = Rc::new(VmContinuation {
        id,
        snapshot: Some(snapshot),
        in_flight: Cell::new(true),
    });
    let v = Value::Procedure(k.clone() as Rc<dyn Procedure>);
    (v, k)
}

/// Build a "condition" value matching the tree-walker's
/// `make_error_condition`: a compound containing `&error`, optionally
/// `&who`, `&message`, and (when non-empty) `&irritants`. Both tiers must
/// produce the same shape because `with-exception-handler` callbacks
/// observe the raw value.
pub fn make_vm_error_condition(who: Option<Value>, msg: String, irritants: Vec<Value>) -> Value {
    let mk = |items: Vec<Value>| -> Value {
        Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
    };
    let mut simples = vec![mk(vec![Value::string("&error")])];
    if let Some(w) = who {
        simples.push(mk(vec![Value::string("&who"), w]));
    }
    simples.push(mk(vec![Value::string("&message"), Value::string(msg)]));
    if !irritants.is_empty() {
        simples.push(mk(vec![
            Value::string("&irritants"),
            Value::list(irritants),
        ]));
    }
    let mut compound = Vec::with_capacity(1 + simples.len());
    compound.push(Value::string("&compound-condition"));
    compound.extend(simples);
    mk(compound)
}

/// Mirror of `make_error_condition` for assertion violations.
/// Produces a compound with `&assertion`, `&who`, `&message`, and
/// (when non-empty) `&irritants` — matching what `assertion-violation`
/// produces on the tree-walker tier.
pub fn make_vm_assertion_violation_condition(
    who: Value,
    msg: String,
    irritants: Vec<Value>,
) -> Value {
    let mk = |items: Vec<Value>| -> Value {
        Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
    };
    let mut simples = vec![
        mk(vec![Value::string("&assertion")]),
        mk(vec![Value::string("&who"), who]),
        mk(vec![Value::string("&message"), Value::string(msg)]),
    ];
    if !irritants.is_empty() {
        simples.push(mk(vec![
            Value::string("&irritants"),
            Value::list(irritants),
        ]));
    }
    let mut compound = Vec::with_capacity(1 + simples.len());
    compound.push(Value::string("&compound-condition"));
    compound.extend(simples);
    mk(compound)
}

/// Synchronously call a VM procedure and return its result. Used by HO native
/// builtins (map/for-each/filter) to invoke the procedure once per element.
/// For closures, this runs a sub-VM to completion on the closure body.
pub fn vm_call_sync(
    func: &Value,
    args: &[Value],
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    match func {
        Value::Procedure(p) => {
            let any = p.as_any();
            if let Some(b) = any.downcast_ref::<VmBuiltin>() {
                return (b.f)(args).map_err(|e| VmError::new(format!("{}: {}", b.name, e)));
            }
            if let Some(b) = any.downcast_ref::<VmBuiltinSyms>() {
                return (b.f)(args, syms).map_err(|e| VmError::new(format!("{}: {}", b.name, e)));
            }
            if let Some(h) = any.downcast_ref::<VmHostBuiltin>() {
                return (h.f)(args).map_err(|e| VmError::new(format!("{}: {}", h.name, e)));
            }
            if let Some(c) = any.downcast_ref::<VmClosure>() {
                if c.tier.bump() {
                    fire_tier_up_hook(c, args);
                }
                if !c.jit_ptr().is_null() {
                    if let Some(result) = try_dispatch_jit(c, args) {
                        return Ok(result);
                    }
                }
                let lam = &c.bc.lambdas[c.lambda_idx];
                if !lambda_arity_ok(lam, args.len()) {
                    return Err(VmError::new("arity mismatch"));
                }
                // Fast path: leaf primop body. Skip Env+Frame allocation
                // (often the dominant cost on per-element HO bridge calls
                // like map/fold/filter passing `(lambda (x) (* x x))`).
                if let Some(fp) = &lam.fast {
                    return apply_fast_primop(fp, args, syms);
                }
                let new_env = Env::child(c.env.clone());
                for (name, v) in lam.params.iter().zip(args.iter()) {
                    new_env.define(*name, v.clone());
                }
                if let Some(rest_name) = lam.rest {
                    let rest_args = &args[lam.params.len()..];
                    new_env.define(rest_name, Value::list(rest_args.iter().cloned()));
                }
                // Reuse the closure's existing Rc<Bytecode> with an entry-
                // insts override; avoids allocating a sub-Bytecode per HO
                // call (saves a Bytecode struct + Rc<Bytecode> heap alloc
                // per element of map/fold/filter/...).
                return run_with_entry(
                    c.bc.clone(),
                    Some(lam.body.clone()),
                    Some(lam.spans.clone()),
                    new_env,
                    syms,
                );
            }
            if any.downcast_ref::<VmApply>().is_some() {
                if args.is_empty() {
                    return Err(VmError::new("apply: 0 args"));
                }
                let inner = args[0].clone();
                let mut spread: Vec<Value> = args[1..args.len().saturating_sub(1)].to_vec();
                if args.len() >= 2 {
                    let last = args[args.len() - 1].clone();
                    let mut cur = last;
                    loop {
                        match cur {
                            Value::Null => break,
                            Value::Pair(p) => {
                                spread.push(p.car.borrow().clone());
                                cur = p.cdr.borrow().clone();
                            }
                            other => {
                                return Err(VmError::new(format!(
                                    "apply: non-list tail ({})",
                                    other.type_name()
                                )));
                            }
                        }
                    }
                }
                return vm_call_sync(&inner, &spread, syms);
            }
            if let Some(k) = any.downcast_ref::<VmContinuation>() {
                let v = if args.is_empty() {
                    Value::Unspecified
                } else {
                    args[0].clone()
                };
                set_pending_escape(k.id, v);
                return Err(VmError::new("__escape__"));
            }
            if any.downcast_ref::<VmValues>().is_some() {
                if args.len() == 1 {
                    return Ok(args[0].clone());
                }
                set_pending_values(args.to_vec());
                return Ok(Value::Unspecified);
            }
            if any.downcast_ref::<VmCallWithValues>().is_some() {
                if args.len() != 2 {
                    return Err(VmError::new("call-with-values: 2 args"));
                }
                let prev = take_pending_values();
                let prod_result = vm_call_sync(&args[0], &[], syms)?;
                let values = if let Some(vs) = take_pending_values() {
                    vs
                } else {
                    vec![prod_result]
                };
                if let Some(prev) = prev {
                    set_pending_values(prev);
                }
                return vm_call_sync(&args[1], &values, syms);
            }
            // Recursively dispatch HO markers when they're called as the
            // procedure target of vm_call_sync (e.g. (apply map proc lst)).
            if any.downcast_ref::<VmMap>().is_some()
                || any.downcast_ref::<VmForEach>().is_some()
                || any.downcast_ref::<VmFilter>().is_some()
                || any.downcast_ref::<VmFind>().is_some()
                || any.downcast_ref::<VmAny>().is_some()
                || any.downcast_ref::<VmEvery>().is_some()
                || any.downcast_ref::<VmFoldLeft>().is_some()
                || any.downcast_ref::<VmFoldRight>().is_some()
                || any.downcast_ref::<VmReduce>().is_some()
                || any.downcast_ref::<VmCount>().is_some()
                || any.downcast_ref::<VmPartition>().is_some()
                || is_pure_ho_marker(p.as_ref())
            {
                return ho_apply(func, args, syms);
            }
            Err(VmError::new("unsupported procedure type in vm_call_sync"))
        }
        _ => Err(VmError::new("not a procedure")),
    }
}

/// Dispatch a HO marker procedure (map/filter/fold/...) when invoked via
/// vm_call_sync (e.g. nested through `apply`). Mirrors the inline arms in
/// `run`'s Call dispatch but without push/pop'ing the VM stack.
fn ho_apply(func: &Value, args: &[Value], syms: &mut SymbolTable) -> Result<Value, VmError> {
    let p = match func {
        Value::Procedure(p) => p.clone(),
        _ => return Err(VmError::new("ho_apply: not a procedure")),
    };
    let any = p.as_any();
    let mut args = args.to_vec();
    if any.downcast_ref::<VmMap>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("map: needs proc + list"));
        }
        let proc_val = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            out.push(vm_call_sync(&proc_val, &row, syms)?);
        }
        return Ok(Value::list(out));
    }
    if any.downcast_ref::<VmForEach>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("for-each: needs proc + list"));
        }
        let proc_val = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmFilter>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("filter: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        let mut kept = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                kept.push(item);
            }
        }
        return Ok(Value::list(kept));
    }
    if any.downcast_ref::<VmFind>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("find: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                return Ok(item);
            }
        }
        return Ok(Value::Boolean(false));
    }
    if any.downcast_ref::<VmAny>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("any: needs pred + list"));
        }
        let pred = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            let r = vm_call_sync(&pred, &row, syms)?;
            if r.is_truthy() {
                return Ok(r);
            }
        }
        return Ok(Value::Boolean(false));
    }
    if any.downcast_ref::<VmEvery>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("every: needs pred + list"));
        }
        let pred = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut last_truthy = Value::Boolean(true);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            let r = vm_call_sync(&pred, &row, syms)?;
            if !r.is_truthy() {
                return Ok(Value::Boolean(false));
            }
            last_truthy = r;
        }
        return Ok(last_truthy);
    }
    if any.downcast_ref::<VmFoldLeft>().is_some() {
        if args.len() < 3 {
            return Err(VmError::new("fold-left: needs proc + init + list"));
        }
        let proc_val = args.remove(0);
        let mut acc = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        for i in 0..n {
            let mut row: Vec<Value> = vec![acc.clone()];
            for l in &lists {
                row.push(l[i].clone());
            }
            acc = vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmFoldRight>().is_some() {
        if args.len() < 3 {
            return Err(VmError::new("fold-right: needs proc + init + list"));
        }
        let proc_val = args.remove(0);
        let init = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut acc = init;
        for i in (0..n).rev() {
            let mut row: Vec<Value> = Vec::with_capacity(lists.len() + 1);
            for l in &lists {
                row.push(l[i].clone());
            }
            row.push(acc);
            acc = vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmReduce>().is_some() {
        if args.len() != 3 {
            return Err(VmError::new("reduce: needs proc + default + list"));
        }
        let proc_val = args.remove(0);
        let default = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        if items.is_empty() {
            return Ok(default);
        }
        let mut acc = items[0].clone();
        for item in &items[1..] {
            acc = vm_call_sync(&proc_val, &[acc, item.clone()], syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmCount>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("count: needs pred + list"));
        }
        let pred = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut total: i64 = 0;
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            let r = vm_call_sync(&pred, &row, syms)?;
            if r.is_truthy() {
                total += 1;
            }
        }
        return Ok(Value::fixnum(total));
    }
    if any.downcast_ref::<VmPartition>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("partition: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                yes.push(item);
            } else {
                no.push(item);
            }
        }
        set_pending_values(vec![Value::list(yes), Value::list(no)]);
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmVectorMap>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("vector-map: needs proc + vector"));
        }
        let proc_val = args.remove(0);
        let vectors: Vec<Vec<Value>> = args
            .iter()
            .map(|v| match v {
                Value::Vector(vec) => Ok(vec.borrow().clone()),
                other => Err(VmError::new(format!(
                    "vector-map: expected vector, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<_, _>>()?;
        let n = vectors.iter().map(|v| v.len()).min().unwrap_or(0);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let row: Vec<Value> = vectors.iter().map(|v| v[i].clone()).collect();
            out.push(vm_call_sync(&proc_val, &row, syms)?);
        }
        return Ok(Value::Vector(cs_core::Gc::new(RefCell::new(out))));
    }
    if any.downcast_ref::<VmVectorForEach>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("vector-for-each: needs proc + vector"));
        }
        let proc_val = args.remove(0);
        let vectors: Vec<Vec<Value>> = args
            .iter()
            .map(|v| match v {
                Value::Vector(vec) => Ok(vec.borrow().clone()),
                other => Err(VmError::new(format!(
                    "vector-for-each: expected vector, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<_, _>>()?;
        let n = vectors.iter().map(|v| v.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = vectors.iter().map(|v| v[i].clone()).collect();
            vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmVectorFold>().is_some() {
        if args.len() != 3 {
            return Err(VmError::new("vector-fold: needs proc + init + vector"));
        }
        let proc_val = args.remove(0);
        let mut acc = args.remove(0);
        let items = match &args[0] {
            Value::Vector(v) => v.borrow().clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-fold: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        for item in items {
            acc = vm_call_sync(&proc_val, &[acc, item], syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmVectorFilter>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("vector-filter: needs pred + vector"));
        }
        let pred = args.remove(0);
        let items = match &args[0] {
            Value::Vector(v) => v.borrow().clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-filter: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        let mut out = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                out.push(item);
            }
        }
        return Ok(Value::Vector(cs_core::Gc::new(RefCell::new(out))));
    }
    if any.downcast_ref::<VmStringMap>().is_some() {
        // R7RS multi-string: proc takes one char from each, output stops at
        // the shortest input.
        if args.len() < 2 {
            return Err(builtin_err_to_raised(
                "string-map",
                "needs proc + at least one string",
                syms,
                Span::DUMMY,
            ));
        }
        let proc_val = args.remove(0);
        let mut strings: Vec<Vec<char>> = Vec::with_capacity(args.len());
        for v in &args {
            match v {
                Value::String(s) => strings.push(s.borrow().chars().collect()),
                other => {
                    return Err(builtin_err_to_raised(
                        "string-map",
                        &format!("expected string, got {}", other.type_name()),
                        syms,
                        Span::DUMMY,
                    ));
                }
            }
        }
        let n = strings.iter().map(|s| s.len()).min().unwrap_or(0);
        let mut out = String::with_capacity(n);
        for i in 0..n {
            let row: Vec<Value> = strings.iter().map(|s| Value::Character(s[i])).collect();
            let r = vm_call_sync(&proc_val, &row, syms)?;
            match r {
                Value::Character(c) => out.push(c),
                other => {
                    return Err(builtin_err_to_raised(
                        "string-map",
                        &format!("proc must return char, got {}", other.type_name()),
                        syms,
                        Span::DUMMY,
                    ));
                }
            }
        }
        return Ok(Value::string(out));
    }
    if any.downcast_ref::<VmStringForEach>().is_some() {
        if args.len() < 2 {
            return Err(builtin_err_to_raised(
                "string-for-each",
                "needs proc + at least one string",
                syms,
                Span::DUMMY,
            ));
        }
        let proc_val = args.remove(0);
        let mut strings: Vec<Vec<char>> = Vec::with_capacity(args.len());
        for v in &args {
            match v {
                Value::String(s) => strings.push(s.borrow().chars().collect()),
                other => {
                    return Err(builtin_err_to_raised(
                        "string-for-each",
                        &format!("expected string, got {}", other.type_name()),
                        syms,
                        Span::DUMMY,
                    ));
                }
            }
        }
        let n = strings.iter().map(|s| s.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = strings.iter().map(|s| Value::Character(s[i])).collect();
            vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmHashtableWalk>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("hashtable-walk: needs ht + proc"));
        }
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-walk: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let proc_val = args.remove(1);
        let entries: Vec<(Value, Value)> = h.items.borrow().clone();
        for (k, v) in entries {
            vm_call_sync(&proc_val, &[k, v], syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmHashtableForEach>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("hashtable-for-each: needs proc + ht"));
        }
        let proc_val = args.remove(0);
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-for-each: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let entries: Vec<(Value, Value)> = h.items.borrow().clone();
        for (k, v) in entries {
            vm_call_sync(&proc_val, &[k, v], syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmHashtableFold>().is_some() {
        if args.len() != 3 {
            return Err(VmError::new("hashtable-fold: needs proc + init + ht"));
        }
        let proc_val = args.remove(0);
        let mut acc = args.remove(0);
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-fold: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let entries: Vec<(Value, Value)> = h.items.borrow().clone();
        for (k, v) in entries {
            acc = vm_call_sync(&proc_val, &[k, v, acc], syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmHashtableUpdate>().is_some() {
        if args.len() != 4 {
            return Err(VmError::new(
                "hashtable-update!: needs ht + key + proc + default",
            ));
        }
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-update!: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let kind = h.eq_kind;
        let current = {
            let items = h.items.borrow();
            items
                .iter()
                .find(|(k, _)| ht_eq_local(kind, k, &args[1]))
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| args[3].clone())
        };
        let new_val = vm_call_sync(&args[2], &[current], syms)?;
        let mut items = h.items.borrow_mut();
        if let Some(slot) = items
            .iter_mut()
            .find(|(k, _)| ht_eq_local(kind, k, &args[1]))
        {
            slot.1 = new_val;
        } else {
            items.push((args[1].clone(), new_val));
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmUnfold>().is_some() {
        if args.len() != 4 {
            return Err(VmError::new("unfold: needs pred + map + next + seed"));
        }
        let pred = args.remove(0);
        let map_fn = args.remove(0);
        let next_fn = args.remove(0);
        let mut seed = args.remove(0);
        let mut out = Vec::new();
        for _ in 0..1_000_000 {
            let stop = vm_call_sync(&pred, &[seed.clone()], syms)?;
            if stop.is_truthy() {
                return Ok(Value::list(out));
            }
            let mapped = vm_call_sync(&map_fn, &[seed.clone()], syms)?;
            out.push(mapped);
            seed = vm_call_sync(&next_fn, &[seed], syms)?;
        }
        return Err(VmError::new("unfold: exceeded 1,000,000 iterations"));
    }
    if any.downcast_ref::<VmListSort>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("list-sort: needs cmp + list"));
        }
        let cmp = args.remove(0);
        let mut items = collect_proper_list(&args[0])?;
        sort_with_predicate(&mut items, &cmp, syms)?;
        return Ok(Value::list(items));
    }
    if any.downcast_ref::<VmVectorSort>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("vector-sort: needs cmp + vector"));
        }
        let cmp = args.remove(0);
        let mut items = match &args[0] {
            Value::Vector(v) => v.borrow().clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-sort: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        sort_with_predicate(&mut items, &cmp, syms)?;
        return Ok(Value::Vector(cs_core::Gc::new(RefCell::new(items))));
    }
    if any.downcast_ref::<VmVectorSortBang>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("vector-sort!: needs cmp + vector"));
        }
        let cmp = args.remove(0);
        let vec_rc = match &args[0] {
            Value::Vector(v) => v.clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-sort!: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        let mut items = vec_rc.borrow().clone();
        sort_with_predicate(&mut items, &cmp, syms)?;
        *vec_rc.borrow_mut() = items;
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmTabulate>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("tabulate: needs n + proc"));
        }
        let n = match &args[0] {
            Value::Number(cs_core::Number::Fixnum(n)) => *n,
            other => {
                return Err(VmError::new(format!(
                    "tabulate: expected fixnum, got {}",
                    other.type_name()
                )));
            }
        };
        if n < 0 {
            return Err(VmError::new("tabulate: negative count"));
        }
        let proc_val = args.remove(1);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let r = vm_call_sync(&proc_val, &[Value::fixnum(i)], syms)?;
            out.push(r);
        }
        return Ok(Value::list(out));
    }
    if any.downcast_ref::<VmRemove>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("remove: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        let mut out = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if !r.is_truthy() {
                out.push(item);
            }
        }
        return Ok(Value::list(out));
    }
    if any.downcast_ref::<VmForce>().is_some() {
        if args.len() != 1 {
            return Err(VmError::new("force: 1 arg"));
        }
        // Iterative force: matches cs-runtime's b_force. Walks lazy
        // promise chains without growing the host stack so R7RS
        // delay-force can express iterative tail calls.
        let original = args.remove(0);
        let mut cur = original.clone();
        loop {
            match cur {
                Value::Promise(p) => {
                    {
                        let state = p.state.borrow();
                        if let cs_core::PromiseState::Forced(v) = &*state {
                            let v = v.clone();
                            if let Value::Promise(orig) = &original {
                                if !std::ptr::eq(&**orig as *const _, &*p as *const _) {
                                    *orig.state.borrow_mut() =
                                        cs_core::PromiseState::Forced(v.clone());
                                }
                            }
                            return Ok(v);
                        }
                    }
                    let thunk = match &*p.state.borrow() {
                        cs_core::PromiseState::Pending(t) => t.clone(),
                        cs_core::PromiseState::Forced(_) => unreachable!(),
                    };
                    let v = vm_call_sync(&thunk, &[], syms)?;
                    if matches!(v, Value::Promise(_)) {
                        cur = v;
                        continue;
                    }
                    if let Value::Promise(orig) = &original {
                        *orig.state.borrow_mut() = cs_core::PromiseState::Forced(v.clone());
                    }
                    return Ok(v);
                }
                other => return Ok(other),
            }
        }
    }
    Err(VmError::new("ho_apply: unrecognized HO marker"))
}

/// If the builtin error message already begins with `name:` (e.g. `+: expected
/// number, got string` from b_add), return it unchanged. Otherwise prepend
/// `name: ` so the caller knows which builtin failed. Avoids the doubled-
/// prefix `+: +: expected...` we used to produce.
fn prefix_builtin_err(name: &str, msg: &str) -> String {
    let leader = format!("{}: ", name);
    if msg.starts_with(&leader) || msg.starts_with(name) && msg[name.len()..].starts_with(':') {
        msg.to_string()
    } else {
        format!("{}: {}", name, msg)
    }
}

/// Convert a builtin error result into the `__raised__` protocol so that
/// `with-exception-handler` can catch type errors / arity mismatches /
/// etc. as proper R6RS conditions. Returns a VmError that the dispatch
/// loop will treat like an explicit `(raise ...)`.
///
/// Sentinel strings (`__raised__`, `__escape__`) pass through unchanged
/// — those are protocol markers, not real failures.
pub fn builtin_err_to_raised(name: &str, e: &str, syms: &mut SymbolTable, span: Span) -> VmError {
    // Drain unconditionally so a stale value from a prior failure can't
    // attach to an unrelated path.
    let irritants = cs_core::take_builtin_err_irritant();
    if e == "__raised__" || e == "__escape__" || e == "__stack-overflow__" {
        return VmError::new(e).with_span(span);
    }
    let prefixed = prefix_builtin_err(name, e);
    // Split on the first ": " so the part before becomes &who (interned
    // as a symbol) and the rest becomes the &message body. This matches
    // the walker tier's `builtin_err_to_eval`.
    let (who, message) = match prefixed.find(": ") {
        Some(idx) => {
            let who_sym = syms.intern(&prefixed[..idx]);
            (
                Some(Value::Symbol(who_sym)),
                prefixed[idx + 2..].to_string(),
            )
        }
        None => (None, prefixed),
    };
    let extra_tag = cs_core::take_builtin_err_extra_tag();
    let mut cond = make_vm_error_condition(who, message, irritants);
    if let Some(tag) = extra_tag {
        if let Value::Vector(vc) = &cond {
            let mut items = vc.borrow().clone();
            items.push(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
                vec![Value::string(tag)],
            ))));
            cond = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)));
        }
    }
    set_pending_raise(cond);
    VmError::new("__raised__").with_span(span)
}

fn collect_proper_list(v: &Value) -> Result<Vec<Value>, VmError> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            other => {
                return Err(VmError::new(format!(
                    "expected proper list, got {}",
                    other.type_name()
                )));
            }
        }
    }
}

/// Return true when `p` is one of the HO markers handled by `ho_apply`
/// (i.e., everything except `values` and `call-with-values`, which have
/// pending-values side-channel logic).
fn is_pure_ho_marker(p: &dyn Procedure) -> bool {
    let any = p.as_any();
    any.downcast_ref::<VmVectorMap>().is_some()
        || any.downcast_ref::<VmVectorForEach>().is_some()
        || any.downcast_ref::<VmVectorFold>().is_some()
        || any.downcast_ref::<VmVectorFilter>().is_some()
        || any.downcast_ref::<VmStringMap>().is_some()
        || any.downcast_ref::<VmStringForEach>().is_some()
        || any.downcast_ref::<VmHashtableWalk>().is_some()
        || any.downcast_ref::<VmHashtableForEach>().is_some()
        || any.downcast_ref::<VmHashtableFold>().is_some()
        || any.downcast_ref::<VmHashtableUpdate>().is_some()
        || any.downcast_ref::<VmUnfold>().is_some()
        || any.downcast_ref::<VmListSort>().is_some()
        || any.downcast_ref::<VmVectorSort>().is_some()
        || any.downcast_ref::<VmVectorSortBang>().is_some()
        || any.downcast_ref::<VmTabulate>().is_some()
        || any.downcast_ref::<VmRemove>().is_some()
        || any.downcast_ref::<VmForce>().is_some()
}

fn ht_eq_local(kind: cs_core::HtEqKind, a: &Value, b: &Value) -> bool {
    match kind {
        cs_core::HtEqKind::Eq => cs_core::eq::eq(a, b),
        cs_core::HtEqKind::Eqv => cs_core::eq::eqv(a, b),
        cs_core::HtEqKind::Equal => cs_core::eq::equal(a, b),
        cs_core::HtEqKind::Custom => {
            unreachable!("custom-equiv hashtables route through tier-aware ops")
        }
    }
}

/// Sort `items` in place using `cmp` (a 2-arg procedure returning truthy when
/// the first arg should sort before the second). Stable mergesort.
fn sort_with_predicate(
    items: &mut Vec<Value>,
    cmp: &Value,
    syms: &mut SymbolTable,
) -> Result<(), VmError> {
    let n = items.len();
    if n <= 1 {
        return Ok(());
    }
    let mut buf: Vec<Value> = items.clone();
    let mut size: usize = 1;
    while size < n {
        let mut left = 0;
        while left < n {
            let mid = (left + size).min(n);
            let right = (left + 2 * size).min(n);
            let mut i = left;
            let mut j = mid;
            let mut k = left;
            while i < mid && j < right {
                // Stable merge: take items[i] when items[i] <= items[j], i.e.
                // !(cmp(items[j], items[i])). Using strict-less-than `cmp`,
                // equal elements have cmp false in both directions; this rule
                // takes the left-hand item first, preserving original order.
                let b_lt_a = vm_call_sync(cmp, &[items[j].clone(), items[i].clone()], syms)?;
                if !b_lt_a.is_truthy() {
                    buf[k] = items[i].clone();
                    i += 1;
                } else {
                    buf[k] = items[j].clone();
                    j += 1;
                }
                k += 1;
            }
            while i < mid {
                buf[k] = items[i].clone();
                i += 1;
                k += 1;
            }
            while j < right {
                buf[k] = items[j].clone();
                j += 1;
                k += 1;
            }
            left += 2 * size;
        }
        std::mem::swap(items, &mut buf);
        size *= 2;
    }
    Ok(())
}

// Empty `Trace` impl for VM-tier procedure types that hold no Values.
// Builtins, marker types like VmApply/VmMap, and continuation handles
// (which carry only an i64 id) all carry no reachable Values inside.
// VmClosure has its own non-empty Trace impl elsewhere because it
// captures an Env; everything else listed here is a leaf.
macro_rules! trace_leaf_proc {
    ($($t:ty),* $(,)?) => {
        $(
            impl cs_gc::Trace for $t {
                fn trace(&self, _marker: &mut cs_gc::Marker) {}
            }
        )*
    };
}

trace_leaf_proc!(
    VmBuiltin,
    VmBuiltinSyms,
    VmHostBuiltin,
    VmApply,
    VmMap,
    VmForEach,
    VmFilter,
    VmFind,
    VmAny,
    VmEvery,
    VmFoldLeft,
    VmFoldRight,
    VmReduce,
    VmCount,
    VmPartition,
    VmValues,
    VmCallWithValues,
    VmVectorMap,
    VmVectorForEach,
    VmVectorFold,
    VmVectorFilter,
    VmStringMap,
    VmStringForEach,
    VmHashtableWalk,
    VmHashtableForEach,
    VmHashtableFold,
    VmHashtableUpdate,
    VmUnfold,
    VmListSort,
    VmVectorSort,
    VmVectorSortBang,
    VmTabulate,
    VmRemove,
    VmForce,
    VmEval,
    VmDisplay,
    VmWrite,
    VmNewline,
    VmWithOutputToString,
    VmWithInputFromString,
    VmWithOutputToFile,
    VmWithInputFromFile,
    VmCurrentInputPort,
    VmCurrentOutputPort,
    VmRaise,
    VmErrorFn,
    VmAssertionViolation,
    VmWithExceptionHandler,
    VmCallCc,
    VmDynamicWind,
    VmContinuation,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use cs_core::{Number, SymbolTable, Value};
    use cs_diag::Span;
    use cs_ir::{CoreExpr, Params};

    fn b_add(args: &[Value]) -> Result<Value, String> {
        let mut acc: i64 = 0;
        for a in args {
            match a {
                Value::Number(Number::Fixnum(n)) => acc += n,
                _ => return Err("expected fixnum".into()),
            }
        }
        Ok(Value::fixnum(acc))
    }

    fn b_sub(args: &[Value]) -> Result<Value, String> {
        if args.is_empty() {
            return Err("sub: 0 args".into());
        }
        let mut iter = args.iter();
        let first = match iter.next().unwrap() {
            Value::Number(Number::Fixnum(n)) => *n,
            _ => return Err("expected fixnum".into()),
        };
        let mut acc = first;
        let mut consumed_more = false;
        for a in iter {
            consumed_more = true;
            match a {
                Value::Number(Number::Fixnum(n)) => acc -= n,
                _ => return Err("expected fixnum".into()),
            }
        }
        if !consumed_more {
            acc = -acc;
        }
        Ok(Value::fixnum(acc))
    }

    fn b_mul(args: &[Value]) -> Result<Value, String> {
        let mut acc: i64 = 1;
        for a in args {
            match a {
                Value::Number(Number::Fixnum(n)) => acc *= n,
                _ => return Err("expected fixnum".into()),
            }
        }
        Ok(Value::fixnum(acc))
    }

    fn b_eq(args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("=: 2 args".into());
        }
        match (&args[0], &args[1]) {
            (Value::Number(Number::Fixnum(a)), Value::Number(Number::Fixnum(b))) => {
                Ok(Value::Boolean(a == b))
            }
            _ => Err("expected fixnums".into()),
        }
    }

    fn make_test_env(syms: &mut SymbolTable) -> Rc<Env> {
        let env = Env::root();
        env.define(syms.intern("+"), make_vm_builtin("+", b_add));
        env.define(syms.intern("-"), make_vm_builtin("-", b_sub));
        env.define(syms.intern("*"), make_vm_builtin("*", b_mul));
        env.define(syms.intern("="), make_vm_builtin("=", b_eq));
        env
    }

    #[test]
    fn vm_const() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let expr = CoreExpr::Const {
            value: Value::fixnum(42),
            span: Span::DUMMY,
        };
        let bc = compile(&expr).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(42)) => {}
            other => panic!("expected 42, got {:?}", other),
        }
    }

    #[test]
    fn vm_add() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let plus = syms.intern("+");
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: plus,
                span: Span::DUMMY,
            }),
            args: vec![
                CoreExpr::Const {
                    value: Value::fixnum(1),
                    span: Span::DUMMY,
                },
                CoreExpr::Const {
                    value: Value::fixnum(2),
                    span: Span::DUMMY,
                },
                CoreExpr::Const {
                    value: Value::fixnum(3),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let bc = compile(&expr).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(6)) => {}
            other => panic!("expected 6, got {:?}", other),
        }
    }

    #[test]
    fn vm_if_then_branch() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let expr = CoreExpr::If {
            cond: Rc::new(CoreExpr::Const {
                value: Value::Boolean(true),
                span: Span::DUMMY,
            }),
            then: Rc::new(CoreExpr::Const {
                value: Value::fixnum(1),
                span: Span::DUMMY,
            }),
            alt: Rc::new(CoreExpr::Const {
                value: Value::fixnum(2),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let bc = compile(&expr).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(1)) => {}
            other => panic!("expected 1, got {:?}", other),
        }
    }

    #[test]
    fn vm_lambda_call() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let x = syms.intern("x");
        let plus = syms.intern("+");
        // ((lambda (x) (+ x 1)) 41)
        let lam = CoreExpr::Lambda {
            params: Params::fixed(vec![x]),
            body: Rc::new(CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: plus,
                    span: Span::DUMMY,
                }),
                args: vec![
                    CoreExpr::Ref {
                        name: x,
                        span: Span::DUMMY,
                    },
                    CoreExpr::Const {
                        value: Value::fixnum(1),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let app = CoreExpr::App {
            func: Rc::new(lam),
            args: vec![CoreExpr::Const {
                value: Value::fixnum(41),
                span: Span::DUMMY,
            }],
            span: Span::DUMMY,
        };
        let bc = compile(&app).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(42)) => {}
            other => panic!("expected 42, got {:?}", other),
        }
    }

    #[test]
    fn vm_letrec_recursive() {
        // (letrec ((fact (lambda (n) (if (= n 0) 1 (* n (fact (- n 1))))))) (fact 5))
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let fact = syms.intern("fact");
        let n = syms.intern("n");
        let plus = syms.intern("+");
        let _ = plus;
        let mul = syms.intern("*");
        let sub = syms.intern("-");
        let eq = syms.intern("=");
        let body = CoreExpr::Lambda {
            params: Params::fixed(vec![n]),
            body: Rc::new(CoreExpr::If {
                cond: Rc::new(CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: eq,
                        span: Span::DUMMY,
                    }),
                    args: vec![
                        CoreExpr::Ref {
                            name: n,
                            span: Span::DUMMY,
                        },
                        CoreExpr::Const {
                            value: Value::fixnum(0),
                            span: Span::DUMMY,
                        },
                    ],
                    span: Span::DUMMY,
                }),
                then: Rc::new(CoreExpr::Const {
                    value: Value::fixnum(1),
                    span: Span::DUMMY,
                }),
                alt: Rc::new(CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: mul,
                        span: Span::DUMMY,
                    }),
                    args: vec![
                        CoreExpr::Ref {
                            name: n,
                            span: Span::DUMMY,
                        },
                        CoreExpr::App {
                            func: Rc::new(CoreExpr::Ref {
                                name: fact,
                                span: Span::DUMMY,
                            }),
                            args: vec![CoreExpr::App {
                                func: Rc::new(CoreExpr::Ref {
                                    name: sub,
                                    span: Span::DUMMY,
                                }),
                                args: vec![
                                    CoreExpr::Ref {
                                        name: n,
                                        span: Span::DUMMY,
                                    },
                                    CoreExpr::Const {
                                        value: Value::fixnum(1),
                                        span: Span::DUMMY,
                                    },
                                ],
                                span: Span::DUMMY,
                            }],
                            span: Span::DUMMY,
                        },
                    ],
                    span: Span::DUMMY,
                }),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let letrec = CoreExpr::Letrec {
            bindings: vec![(fact, body)],
            body: Rc::new(CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: fact,
                    span: Span::DUMMY,
                }),
                args: vec![CoreExpr::Const {
                    value: Value::fixnum(5),
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let bc = compile(&letrec).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(120)) => {}
            other => panic!("expected 120, got {:?}", other),
        }
    }
}
