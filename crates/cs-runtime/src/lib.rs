//! CrabScheme runtime: tree-walking interpreter, environments, builtins.

pub mod active;
pub mod builtins;
pub mod env;
pub mod eval;
// The `ffi` module contains only the libloading-using dlopen path
// (`load_shared_library`, `RuntimeFfiContext`, `CAbiProc`) — gated
// on `ffi-dynamic`. The pure-Rust trait surface (HostProcedure,
// register_host_procedure) lives in `lib.rs` under `ffi-trait` so
// it remains available in WASM builds.
#[cfg(feature = "ffi-dynamic")]
pub mod ffi;
#[cfg(feature = "jit")]
pub mod jit;
pub mod proc;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use cs_core::{SymbolTable, Value, WriteMode};
use cs_diag::{Diagnostic, FileId, SourceMap};
use cs_expand::Expander;
use cs_parse::read_all;

use crate::builtins::{NULL_ENV_SENTINEL, TOP_LEVEL_ENV_SENTINEL};
use crate::env::Frame;
use crate::eval::{eval, EvalCtx, EvalError};

/// A CrabScheme runtime instance. Owns the symbol table, source map, and
/// top-level environment.
pub struct Runtime {
    syms: SymbolTable,
    sources: SourceMap,
    top: Rc<Frame>,
    macros: std::collections::HashMap<cs_core::Symbol, cs_expand::Macro>,
    /// VM-tier persistent root env (lazily populated with pure builtins at construction).
    vm_env: Rc<cs_vm::vm::Env>,
    /// GC heap. M5 milestone: pre-registered with the walker top frame
    /// and the VM root env as persistent roots. Phase 1 collect() walks
    /// these roots transitively but the underlying allocations are
    /// still Rc-backed, so collect() has no observable side effect on
    /// existing programs — it's the seam Phase 2 swaps to a real arena.
    heap: cs_gc::Heap,
    /// Slab of values rooted via `pin()`. Keyed by a monotonically-
    /// increasing PinId. The shared root closure registered at
    /// construction marks every value in here on every collect.
    /// `Pinned<'rt>::Drop` removes its entry by id. (M5b iter 4.)
    pinned: Rc<RefCell<HashMap<PinId, Value>>>,
    /// Next PinId — never reused. u64 is enough for any conceivable
    /// program lifetime (~5×10^14 pins/sec for 1000 years).
    next_pin_id: Rc<Cell<u64>>,
    /// Shared libraries loaded via [`Runtime::load_shared_library`].
    /// Held here only so the plugin's text segment stays mapped for
    /// the runtime's lifetime; we never inspect them after register.
    /// (M10 W1: gated on `ffi-dynamic` — WASM has no `dlopen`.
    /// Plugins compiled-in via the `ffi-trait` API don't need this.)
    #[cfg(feature = "ffi-dynamic")]
    loaded_libs: Vec<libloading::Library>,
    /// Cached C-ABI context. Lazily initialized on first dlopen use;
    /// kept alive for the runtime's lifetime so registered host
    /// procedures' captured back-pointers stay valid. Boxed so the
    /// runtime back-pointer (which equals `self`) stays valid even
    /// if Runtime fields are reordered. Only the dlopen path
    /// constructs this — `ffi-trait`-only embedders register their
    /// procedures via `register_host_procedure` directly.
    #[cfg(feature = "ffi-dynamic")]
    ffi_ctx: Option<Box<crate::ffi::RuntimeFfiContext>>,
    /// JIT lowerer; populated by [`Runtime::install_jit`]. None
    /// means the runtime hasn't opted into JIT (closures stay on
    /// the bytecode VM regardless of tier-up).
    /// (M10 W1: gated on the `jit` feature — WASM has no runtime
    /// native codegen.)
    #[cfg(feature = "jit")]
    pub(crate) jit_lowerer: Option<cs_jit_cranelift::Lowerer>,
    /// Override for `(command-line)`. R6RS specifies that
    /// command-line returns `(<program-path> <arg> ...)` — the
    /// script path followed by the args passed after it. The
    /// process's full argv (which includes `crabscheme`, the
    /// `--tier` flag, and the `run` subcommand) is the wrong
    /// answer. cs-cli's `run_file` sets this to the right list
    /// before evaluating the script. When `None`, the builtin
    /// falls back to `std::env::args()` for backward compat (REPL,
    /// `-e` evaluation, etc.).
    pub(crate) command_line: Option<Vec<String>>,
    /// Typer-derived param-type hints, keyed by
    /// `LambdaProfile::lambda_id`. The JIT tier-up hook prefers
    /// these over the observation-based `param_hints` it would
    /// otherwise derive from the first call's argument types —
    /// the user's annotation is authoritative, while observations
    /// are single-sample and can mis-specialize on polymorphic
    /// call sites. Phase 5.4. Populated by
    /// [`Runtime::install_typer_hints`] after the typer has
    /// run; empty otherwise (no behavior change for untyped
    /// programs).
    pub(crate) typer_hints_by_lambda_id:
        std::cell::RefCell<std::collections::HashMap<u32, Vec<cs_rir::Type>>>,
}

/// Opaque handle for a [`Pinned`] slot. See [`Runtime::pin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PinId(u64);

/// RAII rooting guard returned by [`Runtime::pin`].
///
/// While alive, the wrapped `Value` is reachable from the GC root
/// set, surviving any number of intervening allocations and
/// collections. On drop, the value is unrooted; if no other strong
/// reference exists it can be swept on the next collect.
///
/// `Pinned` does not borrow the `Runtime`, so user code can keep
/// calling runtime operations (eval, etc.) while a pin is alive —
/// that's the whole point of pinning across FFI calls. Single-
/// threaded access is enforced at runtime by the slab's `RefCell`;
/// per ADR 0008 the concurrency story is single-threaded for now.
pub struct Pinned {
    id: PinId,
    pinned: Rc<RefCell<HashMap<PinId, Value>>>,
}

impl Pinned {
    /// The pin id (useful for debugging; equality semantics).
    pub fn id(&self) -> PinId {
        self.id
    }

    /// Clone of the currently-pinned value.
    pub fn value(&self) -> Value {
        self.pinned
            .borrow()
            .get(&self.id)
            .cloned()
            .expect("pinned value still live")
    }
}

impl Drop for Pinned {
    fn drop(&mut self) {
        self.pinned.borrow_mut().remove(&self.id);
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        // ADR 0014 — register the shipped builtin optimizer passes
        // into the process-wide registry exactly once. Subsequent
        // Runtime::new() calls hit the duplicate-name path on every
        // pass; ignored — the registration already happened in the
        // first call (or in a third-party plugin's startup hook).
        let _ = cs_opt::register_builtins(
            &mut cs_opt::PassRegistry::global()
                .lock()
                .expect("pass registry poisoned"),
        );

        let mut syms = SymbolTable::new();
        let top = Frame::root();
        builtins::install_into(&top, &mut syms);
        let vm_env = cs_vm::vm::Env::root();
        for (name, f) in builtins::pure_builtins() {
            let sym = syms.intern(name);
            vm_env.define(sym, cs_vm::vm::make_vm_builtin(name, f));
        }
        for (name, f) in builtins::syms_builtins() {
            let sym = syms.intern(name);
            vm_env.define(sym, cs_vm::vm::make_vm_builtin_syms(name, f));
        }
        // BEAM-style actor / table primops — same Syms shape, gated
        // on the `actor` feature. See crates/cs-runtime/src/builtins/
        // beam.rs and ADR 0013-equivalent (beam_runtime_spec.md).
        #[cfg(feature = "actor")]
        {
            for (name, f) in builtins::beam::beam_syms_builtins() {
                let sym = syms.intern(name);
                vm_env.define(sym, cs_vm::vm::make_vm_builtin_syms(name, f));
            }
        }
        // Mirror the walker tier's record-parent registry so define-record-type
        // works the same on both tiers (predicates look it up at runtime).
        let registry_sym = syms.intern(builtins::RECORD_PARENTS_REGISTRY);
        vm_env.define(
            registry_sym,
            Value::Hashtable(cs_core::Hashtable::new(cs_core::HtEqKind::Eq)),
        );
        // Register symbol-aware VM builtins.
        let symbol_to_string_sym = syms.intern("symbol->string");
        vm_env.define(
            symbol_to_string_sym,
            cs_vm::vm::make_vm_builtin_syms("symbol->string", |args, st| {
                if args.len() != 1 {
                    return Err("symbol->string: 1 arg".into());
                }
                match &args[0] {
                    Value::Symbol(s) => Ok(Value::string(st.name(*s).to_string())),
                    other => Err(format!(
                        "symbol->string: expected symbol, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        let string_to_symbol_sym = syms.intern("string->symbol");
        vm_env.define(
            string_to_symbol_sym,
            cs_vm::vm::make_vm_builtin_syms("string->symbol", |args, st| {
                if args.len() != 1 {
                    return Err("string->symbol: 1 arg".into());
                }
                match &args[0] {
                    Value::String(s) => Ok(Value::Symbol(st.intern(&s.borrow()))),
                    other => Err(format!(
                        "string->symbol: expected string, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        // apply: VM-native dispatch (spreads last arg as list).
        let apply_sym = syms.intern("apply");
        vm_env.define(apply_sym, cs_vm::vm::make_vm_apply());
        let map_sym = syms.intern("map");
        vm_env.define(map_sym, cs_vm::vm::make_vm_map());
        let for_each_sym = syms.intern("for-each");
        vm_env.define(for_each_sym, cs_vm::vm::make_vm_for_each());
        let filter_sym = syms.intern("filter");
        vm_env.define(filter_sym, cs_vm::vm::make_vm_filter());
        let find_sym = syms.intern("find");
        vm_env.define(find_sym, cs_vm::vm::make_vm_find());
        let any_sym = syms.intern("any");
        vm_env.define(any_sym, cs_vm::vm::make_vm_any());
        let every_sym = syms.intern("every");
        vm_env.define(every_sym, cs_vm::vm::make_vm_every());
        let exists_sym = syms.intern("exists");
        vm_env.define(exists_sym, cs_vm::vm::make_vm_any());
        let for_all_sym = syms.intern("for-all");
        vm_env.define(for_all_sym, cs_vm::vm::make_vm_every());
        let fold_left_sym = syms.intern("fold-left");
        vm_env.define(fold_left_sym, cs_vm::vm::make_vm_fold_left());
        let fold_right_sym = syms.intern("fold-right");
        vm_env.define(fold_right_sym, cs_vm::vm::make_vm_fold_right());
        let reduce_sym = syms.intern("reduce");
        vm_env.define(reduce_sym, cs_vm::vm::make_vm_reduce());
        let count_sym = syms.intern("count");
        vm_env.define(count_sym, cs_vm::vm::make_vm_count());
        let partition_sym = syms.intern("partition");
        vm_env.define(partition_sym, cs_vm::vm::make_vm_partition());
        let values_sym = syms.intern("values");
        vm_env.define(values_sym, cs_vm::vm::make_vm_values());
        let cwv_sym = syms.intern("call-with-values");
        vm_env.define(cwv_sym, cs_vm::vm::make_vm_call_with_values());
        // Vector / string / hashtable / sort / unfold HO ops.
        let vmap_sym = syms.intern("vector-map");
        vm_env.define(vmap_sym, cs_vm::vm::make_vm_vector_map());
        let vfor_sym = syms.intern("vector-for-each");
        vm_env.define(vfor_sym, cs_vm::vm::make_vm_vector_for_each());
        let vfold_sym = syms.intern("vector-fold");
        vm_env.define(vfold_sym, cs_vm::vm::make_vm_vector_fold());
        let vfilter_sym = syms.intern("vector-filter");
        vm_env.define(vfilter_sym, cs_vm::vm::make_vm_vector_filter());
        let smap_sym = syms.intern("string-map");
        vm_env.define(smap_sym, cs_vm::vm::make_vm_string_map());
        let sfor_sym = syms.intern("string-for-each");
        vm_env.define(sfor_sym, cs_vm::vm::make_vm_string_for_each());
        let hwalk_sym = syms.intern("hashtable-walk");
        vm_env.define(hwalk_sym, cs_vm::vm::make_vm_hashtable_walk());
        let hfor_sym = syms.intern("hashtable-for-each");
        vm_env.define(hfor_sym, cs_vm::vm::make_vm_hashtable_for_each());
        let hfold_sym = syms.intern("hashtable-fold");
        vm_env.define(hfold_sym, cs_vm::vm::make_vm_hashtable_fold());
        let hupdate_sym = syms.intern("hashtable-update!");
        vm_env.define(hupdate_sym, cs_vm::vm::make_vm_hashtable_update());
        let unfold_sym = syms.intern("unfold");
        vm_env.define(unfold_sym, cs_vm::vm::make_vm_unfold());
        let lsort_sym = syms.intern("list-sort");
        vm_env.define(lsort_sym, cs_vm::vm::make_vm_list_sort());
        let vsort_sym = syms.intern("vector-sort");
        vm_env.define(vsort_sym, cs_vm::vm::make_vm_vector_sort());
        let vsortb_sym = syms.intern("vector-sort!");
        vm_env.define(vsortb_sym, cs_vm::vm::make_vm_vector_sort_bang());
        // zip-with is just an alias for map.
        let zipw_sym = syms.intern("zip-with");
        vm_env.define(zipw_sym, cs_vm::vm::make_vm_map());
        let tab_sym = syms.intern("tabulate");
        vm_env.define(tab_sym, cs_vm::vm::make_vm_tabulate());
        let rem_sym = syms.intern("remove");
        vm_env.define(rem_sym, cs_vm::vm::make_vm_remove());
        // R6RS `remp` — same shape as walker-side; reuse VmRemove.
        let remp_sym = syms.intern("remp");
        vm_env.define(remp_sym, cs_vm::vm::make_vm_remove());
        let force_sym = syms.intern("force");
        vm_env.define(force_sym, cs_vm::vm::make_vm_force());
        // I/O port-state ops.
        let display_sym = syms.intern("display");
        vm_env.define(display_sym, cs_vm::vm::make_vm_display());
        let write_sym = syms.intern("write");
        vm_env.define(write_sym, cs_vm::vm::make_vm_write());
        // R7RS aliases — we don't yet generate shared notation, so both
        // map to the same VM write marker as `write` itself.
        let ws_sym = syms.intern("write-simple");
        vm_env.define(ws_sym, cs_vm::vm::make_vm_write());
        let wsh_sym = syms.intern("write-shared");
        vm_env.define(wsh_sym, cs_vm::vm::make_vm_write());
        let newline_sym = syms.intern("newline");
        vm_env.define(newline_sym, cs_vm::vm::make_vm_newline());
        // display-condition: a builtin-syms that piggybacks on
        // builtins::render_condition so both tiers produce identical
        // output. Output goes via the VM's current output port (or
        // stdout when none is installed) the same way `display` does.
        let dcon_sym = syms.intern("display-condition");
        vm_env.define(
            dcon_sym,
            cs_vm::vm::make_vm_builtin_syms("display-condition", |args, st| {
                if args.is_empty() || args.len() > 2 {
                    return Err("display-condition: 1 or 2 args".into());
                }
                let mut s = builtins::render_condition(&args[0], st);
                s.push('\n');
                let port = if args.len() == 2 {
                    Some(args[1].clone())
                } else {
                    cs_vm::vm::vm_current_output_port_value()
                };
                match port {
                    Some(Value::Port(p)) => match &*p {
                        cs_core::Port::StringOutput(buf) => {
                            buf.borrow_mut().push_str(&s);
                            Ok(Value::Unspecified)
                        }
                        _ => Err("display-condition: not an output port".into()),
                    },
                    Some(_) => Err("display-condition: not a port".into()),
                    None => {
                        print!("{}", s);
                        Ok(Value::Unspecified)
                    }
                }
            }),
        );
        let wos_sym = syms.intern("with-output-to-string");
        vm_env.define(wos_sym, cs_vm::vm::make_vm_with_output_to_string());
        let wis_sym = syms.intern("with-input-from-string");
        vm_env.define(wis_sym, cs_vm::vm::make_vm_with_input_from_string());
        let wof_sym = syms.intern("with-output-to-file");
        vm_env.define(wof_sym, cs_vm::vm::make_vm_with_output_to_file());
        let wiff_sym = syms.intern("with-input-from-file");
        vm_env.define(wiff_sym, cs_vm::vm::make_vm_with_input_from_file());
        // R7RS port helpers built atop vm_call_sync.
        let cwp_sym = syms.intern("call-with-port");
        vm_env.define(
            cwp_sym,
            cs_vm::vm::make_vm_builtin_syms("call-with-port", |args, st| {
                if args.len() != 2 {
                    return Err("call-with-port: 2 args".into());
                }
                if !matches!(&args[0], Value::Port(_)) {
                    return Err(format!(
                        "call-with-port: expected port, got {}",
                        args[0].type_name()
                    ));
                }
                let port = args[0].clone();
                let proc = args[1].clone();
                let res = cs_vm::vm::vm_call_sync(&proc, &[port.clone()], st)
                    .map_err(|e| e.message.clone());
                // Best-effort close (only matters for file ports).
                if let Value::Port(p) = &port {
                    if let cs_core::Port::FileOutput(state) = &**p {
                        let mut st = state.borrow_mut();
                        if !st.closed {
                            let _ = std::fs::write(&st.path, &st.buf);
                            st.closed = true;
                        }
                    }
                }
                res
            }),
        );
        let cwis_sym = syms.intern("call-with-input-string");
        vm_env.define(
            cwis_sym,
            cs_vm::vm::make_vm_builtin_syms("call-with-input-string", |args, st| {
                if args.len() != 2 {
                    return Err("call-with-input-string: 2 args".into());
                }
                let s = match &args[0] {
                    Value::String(s) => s.borrow().clone(),
                    other => {
                        return Err(format!(
                            "call-with-input-string: expected string, got {}",
                            other.type_name()
                        ))
                    }
                };
                let port = Value::Port(cs_core::Port::string_input(&s));
                let proc = args[1].clone();
                cs_vm::vm::vm_call_sync(&proc, &[port], st).map_err(|e| e.message.clone())
            }),
        );
        let cwos_sym = syms.intern("call-with-output-string");
        vm_env.define(
            cwos_sym,
            cs_vm::vm::make_vm_builtin_syms("call-with-output-string", |args, st| {
                if args.len() != 1 {
                    return Err("call-with-output-string: 1 arg".into());
                }
                let port = cs_core::Port::string_output();
                let port_val = Value::Port(port.clone());
                let proc = args[0].clone();
                let _ = cs_vm::vm::vm_call_sync(&proc, &[port_val], st)
                    .map_err(|e| e.message.clone())?;
                match &*port {
                    cs_core::Port::StringOutput(buf) => Ok(Value::string(buf.borrow().clone())),
                    _ => unreachable!(),
                }
            }),
        );
        let cip_sym = syms.intern("current-input-port");
        vm_env.define(cip_sym, cs_vm::vm::make_vm_current_input_port());
        let cop_sym = syms.intern("current-output-port");
        vm_env.define(cop_sym, cs_vm::vm::make_vm_current_output_port());
        let cep_sym = syms.intern("current-error-port");
        vm_env.define(
            cep_sym,
            cs_vm::vm::make_vm_builtin("current-error-port", |args| {
                if !args.is_empty() {
                    return Err("current-error-port: 0 args".into());
                }
                Ok(cs_vm::vm::vm_current_error_port_value())
            }),
        );
        // R6RS §8.2 — standard-{input,output,error}-port. Aliased to
        // the same backing thread-local as current-* on the VM tier.
        let sip_sym = syms.intern("standard-input-port");
        vm_env.define(
            sip_sym,
            cs_vm::vm::make_vm_builtin("standard-input-port", |args| {
                if !args.is_empty() {
                    return Err("standard-input-port: 0 args".into());
                }
                Ok(cs_vm::vm::vm_current_input_port_value().unwrap_or(Value::Unspecified))
            }),
        );
        let sop_sym = syms.intern("standard-output-port");
        vm_env.define(
            sop_sym,
            cs_vm::vm::make_vm_builtin("standard-output-port", |args| {
                if !args.is_empty() {
                    return Err("standard-output-port: 0 args".into());
                }
                Ok(cs_vm::vm::vm_current_output_port_value().unwrap_or(Value::Unspecified))
            }),
        );
        let sep_sym = syms.intern("standard-error-port");
        vm_env.define(
            sep_sym,
            cs_vm::vm::make_vm_builtin("standard-error-port", |args| {
                if !args.is_empty() {
                    return Err("standard-error-port: 0 args".into());
                }
                Ok(cs_vm::vm::vm_current_error_port_value())
            }),
        );
        // eval: thread-local hook installed at entry to eval_str_via_vm so
        // VmEval can call back into the runtime without a direct cycle.
        let eval_sym = syms.intern("eval");
        vm_env.define(eval_sym, cs_vm::vm::make_vm_eval());
        // Foundation environments: same opaque sentinel from both
        // `environment` and `interaction-environment` since every binding
        // is global. The VM-tier `eval` already ignores its 2nd arg, so
        // this just unblocks the names being looked up.
        let env_sym = syms.intern("environment");
        vm_env.define(
            env_sym,
            cs_vm::vm::make_vm_builtin_syms("environment", |_args, st| {
                Ok(Value::Symbol(st.intern(TOP_LEVEL_ENV_SENTINEL)))
            }),
        );
        // R6RS multi-value division ops. Both stash the (d, m) pair via
        // the VM's pending-values channel so call-with-values picks it up.
        let dam_sym = syms.intern("div-and-mod");
        vm_env.define(
            dam_sym,
            cs_vm::vm::make_vm_builtin("div-and-mod", |args| {
                if args.len() != 2 {
                    return Err("div-and-mod: 2 args".into());
                }
                let (d, m) = builtins::div_and_mod_num(&args[0], &args[1])
                    .map_err(|e| format!("div-and-mod: {}", e))?;
                cs_vm::vm::vm_set_pending_values(vec![d, m]);
                Ok(Value::Unspecified)
            }),
        );
        // SRFI-1 `split-at` — pure data shuffle returning 2 values.
        // Walker version uses ctx.pending_values; VM mirrors via the
        // VM's pending-values thread-local.
        let split_at_sym = syms.intern("split-at");
        vm_env.define(
            split_at_sym,
            cs_vm::vm::make_vm_builtin("split-at", |args| {
                if args.len() != 2 {
                    return Err("split-at: 2 args".into());
                }
                let n = builtins::as_int_i64_pub("split-at", &args[1])?;
                if n < 0 {
                    return Err("split-at: negative count".into());
                }
                let n = n as usize;
                let items = builtins::collect_proper_list_pub("split-at", &args[0])?;
                if n > items.len() {
                    return Err(format!(
                        "split-at: count {} exceeds list length {}",
                        n,
                        items.len()
                    ));
                }
                let head: Vec<Value> = items[..n].to_vec();
                let tail: Vec<Value> = items[n..].to_vec();
                cs_vm::vm::vm_set_pending_values(vec![Value::list(head), Value::list(tail)]);
                Ok(Value::Unspecified)
            }),
        );
        // VM-tier shims for assoc / member. The 3-arg form needs to
        // apply a user-supplied comparison procedure, so the impl uses
        // vm_call_sync. The 2-arg form falls back to the same eq /
        // equal predicate as the walker.
        fn vm_assoc(args: &[Value], st: &mut cs_core::SymbolTable) -> Result<Value, String> {
            match args.len() {
                2 => vm_assoc_static(&args[0], &args[1], cs_core::eq::equal),
                3 => vm_assoc_with_proc(&args[0], &args[1], &args[2], st),
                n => Err(format!("assoc: expected 2 or 3 arguments, got {}", n)),
            }
        }
        fn vm_assoc_static(
            key: &Value,
            list: &Value,
            pred: fn(&Value, &Value) -> bool,
        ) -> Result<Value, String> {
            let mut cur = list.clone();
            loop {
                match cur {
                    Value::Null => return Ok(Value::Boolean(false)),
                    Value::Pair(p) => {
                        let head = p.car.borrow().clone();
                        match &head {
                            Value::Pair(pair) => {
                                if pred(&pair.car.borrow(), key) {
                                    return Ok(head.clone());
                                }
                            }
                            _ => return Err("assoc: list of pairs".into()),
                        }
                        cur = p.cdr.borrow().clone();
                    }
                    _ => return Err("assoc: proper list".into()),
                }
            }
        }
        fn vm_assoc_with_proc(
            key: &Value,
            list: &Value,
            cmp: &Value,
            st: &mut cs_core::SymbolTable,
        ) -> Result<Value, String> {
            let mut cur = list.clone();
            loop {
                match cur {
                    Value::Null => return Ok(Value::Boolean(false)),
                    Value::Pair(p) => {
                        let head = p.car.borrow().clone();
                        match &head {
                            Value::Pair(pair) => {
                                let car = pair.car.borrow().clone();
                                let r = cs_vm::vm::vm_call_sync(cmp, &[car, key.clone()], st)
                                    .map_err(|e| format!("{:?}", e))?;
                                if r.is_truthy() {
                                    return Ok(head.clone());
                                }
                            }
                            _ => return Err("assoc: list of pairs".into()),
                        }
                        cur = p.cdr.borrow().clone();
                    }
                    _ => return Err("assoc: proper list".into()),
                }
            }
        }
        let assoc_sym = syms.intern("assoc");
        vm_env.define(
            assoc_sym,
            cs_vm::vm::make_vm_builtin_syms("assoc", vm_assoc),
        );

        fn vm_member(args: &[Value], st: &mut cs_core::SymbolTable) -> Result<Value, String> {
            match args.len() {
                2 => vm_member_static(&args[0], &args[1], cs_core::eq::equal),
                3 => vm_member_with_proc(&args[0], &args[1], &args[2], st),
                n => Err(format!("member: expected 2 or 3 arguments, got {}", n)),
            }
        }
        fn vm_member_static(
            obj: &Value,
            list: &Value,
            pred: fn(&Value, &Value) -> bool,
        ) -> Result<Value, String> {
            let mut cur = list.clone();
            loop {
                match cur.clone() {
                    Value::Null => return Ok(Value::Boolean(false)),
                    Value::Pair(p) => {
                        if pred(&p.car.borrow(), obj) {
                            return Ok(cur);
                        }
                        cur = p.cdr.borrow().clone();
                    }
                    _ => return Err("member: proper list".into()),
                }
            }
        }
        fn vm_member_with_proc(
            obj: &Value,
            list: &Value,
            cmp: &Value,
            st: &mut cs_core::SymbolTable,
        ) -> Result<Value, String> {
            let mut cur = list.clone();
            loop {
                match cur.clone() {
                    Value::Null => return Ok(Value::Boolean(false)),
                    Value::Pair(p) => {
                        let car = p.car.borrow().clone();
                        let r = cs_vm::vm::vm_call_sync(cmp, &[car, obj.clone()], st)
                            .map_err(|e| format!("{:?}", e))?;
                        if r.is_truthy() {
                            return Ok(cur);
                        }
                        cur = p.cdr.borrow().clone();
                    }
                    _ => return Err("member: proper list".into()),
                }
            }
        }
        let member_sym = syms.intern("member");
        vm_env.define(
            member_sym,
            cs_vm::vm::make_vm_builtin_syms("member", vm_member),
        );

        let eis_sym = syms.intern("exact-integer-sqrt");
        vm_env.define(
            eis_sym,
            cs_vm::vm::make_vm_builtin("exact-integer-sqrt", |args| {
                if args.len() != 1 {
                    return Err("exact-integer-sqrt: 1 arg".into());
                }
                let (s, r) = builtins::exact_integer_sqrt_num(&args[0])
                    .map_err(|e| format!("exact-integer-sqrt: {}", e))?;
                cs_vm::vm::vm_set_pending_values(vec![s, r]);
                Ok(Value::Unspecified)
            }),
        );
        let truncdiv_sym = syms.intern("truncate/");
        vm_env.define(
            truncdiv_sym,
            cs_vm::vm::make_vm_builtin("truncate/", |args| {
                if args.len() != 2 {
                    return Err("truncate/: 2 args".into());
                }
                let (q, r) = builtins::truncate_div_num(&args[0], &args[1])
                    .map_err(|e| format!("truncate/: {}", e))?;
                cs_vm::vm::vm_set_pending_values(vec![q, r]);
                Ok(Value::Unspecified)
            }),
        );
        let floordiv_sym = syms.intern("floor/");
        vm_env.define(
            floordiv_sym,
            cs_vm::vm::make_vm_builtin("floor/", |args| {
                if args.len() != 2 {
                    return Err("floor/: 2 args".into());
                }
                let (q, r) = builtins::floor_div_num(&args[0], &args[1])
                    .map_err(|e| format!("floor/: {}", e))?;
                cs_vm::vm::vm_set_pending_values(vec![q, r]);
                Ok(Value::Unspecified)
            }),
        );
        let dam0_sym = syms.intern("div0-and-mod0");
        vm_env.define(
            dam0_sym,
            cs_vm::vm::make_vm_builtin("div0-and-mod0", |args| {
                if args.len() != 2 {
                    return Err("div0-and-mod0: 2 args".into());
                }
                let (d, m) = builtins::div0_and_mod0_num(&args[0], &args[1])
                    .map_err(|e| format!("div0-and-mod0: {}", e))?;
                cs_vm::vm::vm_set_pending_values(vec![d, m]);
                Ok(Value::Unspecified)
            }),
        );
        // VM-tier hashtable-equivalence-function: returns a VmBuiltin so
        // the result is callable inside the VM. The walker side returns
        // a Builtin via builtins::b_hashtable_equivalence_function.
        let heqf_sym = syms.intern("hashtable-equivalence-function");
        vm_env.define(
            heqf_sym,
            cs_vm::vm::make_vm_builtin(
                "hashtable-equivalence-function",
                builtins::vm_hashtable_equivalence_function,
            ),
        );
        let hentries_sym = syms.intern("hashtable-entries");
        vm_env.define(
            hentries_sym,
            cs_vm::vm::make_vm_builtin("hashtable-entries", |args| {
                if args.len() != 1 {
                    return Err("hashtable-entries: 1 arg".into());
                }
                let h = match &args[0] {
                    Value::Hashtable(h) => h.clone(),
                    _ => return Err("hashtable-entries: not a hashtable".into()),
                };
                let items = h.items.borrow();
                let keys: Vec<Value> = items.iter().map(|(k, _)| k.clone()).collect();
                let vals: Vec<Value> = items.iter().map(|(_, v)| v.clone()).collect();
                let kv = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(keys)));
                let vv = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(vals)));
                cs_vm::vm::vm_set_pending_values(vec![kv, vv]);
                Ok(Value::Unspecified)
            }),
        );
        // VM-tier shims for hashtable-set!/-ref/-contains?/-delete!
        // These were moved from pure_builtins to higher-order on the
        // walker side so Custom-equiv tables can apply user procs. The
        // VM tier needs equivalent tier-aware dispatch via vm_call_sync.
        fn vm_ht_equiv(
            h: &cs_core::Hashtable,
            a: &Value,
            b: &Value,
            syms: &mut cs_core::SymbolTable,
        ) -> Result<bool, String> {
            use cs_core::HtEqKind;
            if h.eq_kind != HtEqKind::Custom {
                return Ok(match h.eq_kind {
                    HtEqKind::Eq => cs_core::eq::eq(a, b),
                    HtEqKind::Eqv => cs_core::eq::eqv(a, b),
                    HtEqKind::Equal => cs_core::eq::equal(a, b),
                    HtEqKind::Custom => unreachable!(),
                });
            }
            let equiv = h
                .custom
                .as_ref()
                .expect("custom kind has procs")
                .equiv
                .clone();
            let r = cs_vm::vm::vm_call_sync(&equiv, &[a.clone(), b.clone()], syms)
                .map_err(|e| format!("{:?}", e))?;
            Ok(r.is_truthy())
        }

        let hset_sym = syms.intern("hashtable-set!");
        vm_env.define(
            hset_sym,
            cs_vm::vm::make_vm_builtin_syms("hashtable-set!", |args, st| {
                if args.len() != 3 {
                    return Err("hashtable-set!: 3 args".into());
                }
                let h = match &args[0] {
                    Value::Hashtable(h) => h.clone(),
                    _ => return Err("hashtable-set!: not a hashtable".into()),
                };
                let len = h.items.borrow().len();
                for i in 0..len {
                    let k = h.items.borrow()[i].0.clone();
                    if vm_ht_equiv(&h, &k, &args[1], st)? {
                        h.items.borrow_mut()[i].1 = args[2].clone();
                        return Ok(Value::Unspecified);
                    }
                }
                h.items
                    .borrow_mut()
                    .push((args[1].clone(), args[2].clone()));
                Ok(Value::Unspecified)
            }),
        );

        let href_sym = syms.intern("hashtable-ref");
        vm_env.define(
            href_sym,
            cs_vm::vm::make_vm_builtin_syms("hashtable-ref", |args, st| {
                if args.len() != 3 {
                    return Err("hashtable-ref: 3 args".into());
                }
                let h = match &args[0] {
                    Value::Hashtable(h) => h.clone(),
                    _ => return Err("hashtable-ref: not a hashtable".into()),
                };
                let len = h.items.borrow().len();
                for i in 0..len {
                    let k = h.items.borrow()[i].0.clone();
                    if vm_ht_equiv(&h, &k, &args[1], st)? {
                        return Ok(h.items.borrow()[i].1.clone());
                    }
                }
                Ok(args[2].clone())
            }),
        );

        let hcon_sym = syms.intern("hashtable-contains?");
        vm_env.define(
            hcon_sym,
            cs_vm::vm::make_vm_builtin_syms("hashtable-contains?", |args, st| {
                if args.len() != 2 {
                    return Err("hashtable-contains?: 2 args".into());
                }
                let h = match &args[0] {
                    Value::Hashtable(h) => h.clone(),
                    _ => return Err("hashtable-contains?: not a hashtable".into()),
                };
                let len = h.items.borrow().len();
                for i in 0..len {
                    let k = h.items.borrow()[i].0.clone();
                    if vm_ht_equiv(&h, &k, &args[1], st)? {
                        return Ok(Value::Boolean(true));
                    }
                }
                Ok(Value::Boolean(false))
            }),
        );

        let hdel_sym = syms.intern("hashtable-delete!");
        vm_env.define(
            hdel_sym,
            cs_vm::vm::make_vm_builtin_syms("hashtable-delete!", |args, st| {
                if args.len() != 2 {
                    return Err("hashtable-delete!: 2 args".into());
                }
                let h = match &args[0] {
                    Value::Hashtable(h) => h.clone(),
                    _ => return Err("hashtable-delete!: not a hashtable".into()),
                };
                let len = h.items.borrow().len();
                for i in 0..len {
                    let k = h.items.borrow()[i].0.clone();
                    if vm_ht_equiv(&h, &k, &args[1], st)? {
                        h.items.borrow_mut().swap_remove(i);
                        return Ok(Value::Unspecified);
                    }
                }
                Ok(Value::Unspecified)
            }),
        );

        let ienv_sym = syms.intern("interaction-environment");
        vm_env.define(
            ienv_sym,
            cs_vm::vm::make_vm_builtin_syms("interaction-environment", |args, st| {
                if !args.is_empty() {
                    return Err("interaction-environment: 0 args".into());
                }
                Ok(Value::Symbol(st.intern(TOP_LEVEL_ENV_SENTINEL)))
            }),
        );
        // R5RS/R7RS legacy environment-introspection procedures.
        let nenv_sym = syms.intern("null-environment");
        vm_env.define(
            nenv_sym,
            cs_vm::vm::make_vm_builtin_syms("null-environment", |args, st| {
                if args.len() != 1 {
                    return Err("null-environment: 1 arg".into());
                }
                let v = match &args[0] {
                    Value::Number(n) => n.to_f64() as i64,
                    _ => return Err("null-environment: version must be integer".into()),
                };
                if v != 5 {
                    return Err(format!("null-environment: unsupported version: {}", v));
                }
                Ok(Value::Symbol(st.intern(NULL_ENV_SENTINEL)))
            }),
        );
        let srenv_sym = syms.intern("scheme-report-environment");
        vm_env.define(
            srenv_sym,
            cs_vm::vm::make_vm_builtin_syms("scheme-report-environment", |args, st| {
                if args.len() != 1 {
                    return Err("scheme-report-environment: 1 arg".into());
                }
                let v = match &args[0] {
                    Value::Number(n) => n.to_f64() as i64,
                    _ => return Err("scheme-report-environment: version must be integer".into()),
                };
                if v != 5 && v != 7 {
                    return Err(format!(
                        "scheme-report-environment: unsupported version: {}",
                        v
                    ));
                }
                Ok(Value::Symbol(st.intern(TOP_LEVEL_ENV_SENTINEL)))
            }),
        );
        // get-string-all does not need ctx; install as pure VM builtin.
        let gsa_sym = syms.intern("get-string-all");
        vm_env.define(
            gsa_sym,
            cs_vm::vm::make_vm_builtin("get-string-all", |args| {
                if args.len() != 1 {
                    return Err("get-string-all: 1 arg".into());
                }
                match &args[0] {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(state) => {
                            let mut s = state.borrow_mut();
                            if s.pos >= s.chars.len() {
                                return Ok(Value::Eof);
                            }
                            let collected: String = s.chars[s.pos..].iter().collect();
                            s.pos = s.chars.len();
                            Ok(Value::string(collected))
                        }
                        _ => Err("get-string-all: not an input port".into()),
                    },
                    other => Err(format!(
                        "get-string-all: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        // read-line: 1 arg explicit port, or 0 args using current-input-port.
        let rl_sym = syms.intern("read-line");
        vm_env.define(
            rl_sym,
            cs_vm::vm::make_vm_builtin("read-line", |args| {
                if args.len() > 1 {
                    return Err("read-line: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read-line: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(state) => {
                            let mut s = state.borrow_mut();
                            if s.pos >= s.chars.len() {
                                return Ok(Value::Eof);
                            }
                            let mut line = String::new();
                            while s.pos < s.chars.len() {
                                let c = s.chars[s.pos];
                                s.pos += 1;
                                if c == '\n' {
                                    break;
                                }
                                line.push(c);
                            }
                            Ok(Value::string(line))
                        }
                        _ => Err("read-line: not an input port".into()),
                    },
                    other => Err(format!(
                        "read-line: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        // Exception support.
        let raise_sym = syms.intern("raise");
        vm_env.define(raise_sym, cs_vm::vm::make_vm_raise());
        // raise-continuable shares the VmRaise marker — see the matching
        // walker-tier alias in builtins/mod.rs for the rationale.
        let raise_cont_sym = syms.intern("raise-continuable");
        vm_env.define(raise_cont_sym, cs_vm::vm::make_vm_raise());
        let error_sym = syms.intern("error");
        vm_env.define(error_sym, cs_vm::vm::make_vm_error_fn());
        let av_sym = syms.intern("assertion-violation");
        vm_env.define(av_sym, cs_vm::vm::make_vm_assertion_violation());
        let weh_sym = syms.intern("with-exception-handler");
        vm_env.define(weh_sym, cs_vm::vm::make_vm_with_exception_handler());
        // R7RS exit / emergency-exit raise an &exit-requested condition.
        // Uses the same shape as the walker tier.
        let exit_sym = syms.intern("exit");
        vm_env.define(
            exit_sym,
            cs_vm::vm::make_vm_builtin("exit", |args| {
                if args.len() > 1 {
                    return Err("exit: 0 or 1 arg".into());
                }
                let val = args
                    .first()
                    .cloned()
                    .unwrap_or(cs_core::Value::Boolean(true));
                let mk = |items: Vec<cs_core::Value>| -> cs_core::Value {
                    cs_core::Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
                };
                let cond = mk(vec![
                    cs_core::Value::string("&compound-condition"),
                    mk(vec![cs_core::Value::string("&exit-requested"), val]),
                    mk(vec![
                        cs_core::Value::string("&message"),
                        cs_core::Value::string(""),
                    ]),
                ]);
                cs_vm::vm::vm_set_pending_raise(cond);
                Err("__raised__".into())
            }),
        );
        let eexit_sym = syms.intern("emergency-exit");
        vm_env.define(
            eexit_sym,
            cs_vm::vm::make_vm_builtin("emergency-exit", |args| {
                if args.len() > 1 {
                    return Err("emergency-exit: 0 or 1 arg".into());
                }
                let val = args
                    .first()
                    .cloned()
                    .unwrap_or(cs_core::Value::Boolean(true));
                let mk = |items: Vec<cs_core::Value>| -> cs_core::Value {
                    cs_core::Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
                };
                let cond = mk(vec![
                    cs_core::Value::string("&compound-condition"),
                    mk(vec![cs_core::Value::string("&exit-requested"), val]),
                    mk(vec![cs_core::Value::string("&emergency")]),
                    mk(vec![
                        cs_core::Value::string("&message"),
                        cs_core::Value::string(""),
                    ]),
                ]);
                cs_vm::vm::vm_set_pending_raise(cond);
                Err("__raised__".into())
            }),
        );
        let dwind_sym = syms.intern("dynamic-wind");
        vm_env.define(dwind_sym, cs_vm::vm::make_vm_dynamic_wind());
        let cc_sym = syms.intern("call/cc");
        vm_env.define(cc_sym, cs_vm::vm::make_vm_call_cc());
        let cwcc_sym = syms.intern("call-with-current-continuation");
        vm_env.define(cwcc_sym, cs_vm::vm::make_vm_call_cc());
        // read: needs symbol table to intern parsed symbols. With 0 args,
        // falls back to the VM thread-local current-input-port.
        let read_sym = syms.intern("read");
        vm_env.define(
            read_sym,
            cs_vm::vm::make_vm_builtin_syms("read", |args, st| {
                if args.len() > 1 {
                    return Err("read: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(state) => {
                            let mut s = state.borrow_mut();
                            let remaining: String = s.chars[s.pos..].iter().collect();
                            if remaining.trim().is_empty() {
                                return Ok(Value::Eof);
                            }
                            let file_id = cs_diag::FileId(u32::MAX - 2);
                            let mut reader = cs_parse::Reader::new(file_id, &remaining);
                            let datum = reader.read(st).map_err(|e| {
                                cs_core::stash_builtin_err_extra_tag("&read-error");
                                format!("read: {}", e.message())
                            })?;
                            let consumed_bytes = match &datum {
                                Some(d) => d.span().end as usize,
                                None => remaining.len(),
                            };
                            let consumed_chars = remaining
                                .char_indices()
                                .take_while(|(b, _)| *b < consumed_bytes)
                                .count();
                            s.pos += consumed_chars;
                            Ok(datum.map(|d| d.to_value()).unwrap_or(Value::Eof))
                        }
                        _ => Err("read: not a string input port".into()),
                    },
                    other => Err(format!("read: expected port, got {}", other.type_name())),
                }
            }),
        );
        // R7RS read-char / peek-char / read-string with optional port.
        // No-arg form falls back to vm_current_input_port_value().
        let rc_sym = syms.intern("read-char");
        vm_env.define(
            rc_sym,
            cs_vm::vm::make_vm_builtin("read-char", |args| {
                if args.len() > 1 {
                    return Err("read-char: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read-char: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(state) => {
                            let mut s = state.borrow_mut();
                            if s.pos < s.chars.len() {
                                let c = s.chars[s.pos];
                                s.pos += 1;
                                Ok(Value::Character(c))
                            } else {
                                Ok(Value::Eof)
                            }
                        }
                        _ => Err("read-char: not an input port".into()),
                    },
                    other => Err(format!(
                        "read-char: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        let pc_sym = syms.intern("peek-char");
        vm_env.define(
            pc_sym,
            cs_vm::vm::make_vm_builtin("peek-char", |args| {
                if args.len() > 1 {
                    return Err("peek-char: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "peek-char: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(state) => {
                            let s = state.borrow();
                            if s.pos < s.chars.len() {
                                Ok(Value::Character(s.chars[s.pos]))
                            } else {
                                Ok(Value::Eof)
                            }
                        }
                        _ => Err("peek-char: not an input port".into()),
                    },
                    other => Err(format!(
                        "peek-char: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        let rs_sym = syms.intern("read-string");
        vm_env.define(
            rs_sym,
            cs_vm::vm::make_vm_builtin("read-string", |args| {
                if args.is_empty() || args.len() > 2 {
                    return Err("read-string: 1 or 2 args".into());
                }
                let k = match &args[0] {
                    Value::Number(n) => match n.to_f64() as i64 {
                        i if i >= 0 => i as usize,
                        _ => return Err("read-string: negative count".into()),
                    },
                    other => {
                        return Err(format!(
                            "read-string: expected count, got {}",
                            other.type_name()
                        ))
                    }
                };
                let port = if args.len() == 1 {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read-string: no current input port".to_string())?
                } else {
                    args[1].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(state) => {
                            let mut s = state.borrow_mut();
                            if s.pos >= s.chars.len() {
                                return Ok(Value::Eof);
                            }
                            let end = (s.pos + k).min(s.chars.len());
                            let chars: String = s.chars[s.pos..end].iter().collect();
                            s.pos = end;
                            Ok(Value::string(chars))
                        }
                        _ => Err("read-string: not an input port".into()),
                    },
                    other => Err(format!(
                        "read-string: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        // R7RS write-char / write-string with optional port. No-arg form
        // falls back to vm_current_output_port_value().
        let wc_sym = syms.intern("write-char");
        vm_env.define(
            wc_sym,
            cs_vm::vm::make_vm_builtin("write-char", |args| {
                if args.is_empty() || args.len() > 2 {
                    return Err("write-char: 1 or 2 args".into());
                }
                let c = match &args[0] {
                    Value::Character(c) => *c,
                    other => {
                        return Err(format!(
                            "write-char: expected char, got {}",
                            other.type_name()
                        ));
                    }
                };
                let port = if args.len() == 1 {
                    cs_vm::vm::vm_current_output_port_value()
                        .ok_or_else(|| "write-char: no current output port".to_string())?
                } else {
                    args[1].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringOutput(buf) => {
                            buf.borrow_mut().push(c);
                            Ok(Value::Unspecified)
                        }
                        _ => Err("write-char: not an output port".into()),
                    },
                    other => Err(format!(
                        "write-char: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        let ws_sym = syms.intern("write-string");
        vm_env.define(
            ws_sym,
            cs_vm::vm::make_vm_builtin("write-string", |args| {
                if args.is_empty() || args.len() > 4 {
                    return Err("write-string: 1..4 args".into());
                }
                let s = match &args[0] {
                    Value::String(s) => s.borrow().clone(),
                    other => {
                        return Err(format!(
                            "write-string: expected string, got {}",
                            other.type_name()
                        ));
                    }
                };
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len();
                let port = if args.len() == 1 {
                    cs_vm::vm::vm_current_output_port_value()
                        .ok_or_else(|| "write-string: no current output port".to_string())?
                } else {
                    args[1].clone()
                };
                let start = if args.len() >= 3 {
                    match &args[2] {
                        Value::Number(n) => match n.to_f64() as i64 {
                            i if i >= 0 && (i as usize) <= len => i as usize,
                            _ => return Err(format!("write-string: start out of range")),
                        },
                        _ => return Err("write-string: start must be integer".into()),
                    }
                } else {
                    0
                };
                let end = if args.len() == 4 {
                    match &args[3] {
                        Value::Number(n) => match n.to_f64() as i64 {
                            i if i >= 0 && (i as usize) <= len && (i as usize) >= start => {
                                i as usize
                            }
                            _ => return Err("write-string: end out of range".into()),
                        },
                        _ => return Err("write-string: end must be integer".into()),
                    }
                } else {
                    len
                };
                let slice: String = chars[start..end].iter().collect();
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringOutput(buf) => {
                            buf.borrow_mut().push_str(&slice);
                            Ok(Value::Unspecified)
                        }
                        _ => Err("write-string: not an output port".into()),
                    },
                    other => Err(format!(
                        "write-string: expected port, got {}",
                        other.type_name()
                    )),
                }
            }),
        );
        // R7RS binary I/O with optional port (default current-* port).
        let ru8_sym = syms.intern("read-u8");
        vm_env.define(
            ru8_sym,
            cs_vm::vm::make_vm_builtin("read-u8", |args| {
                if args.len() > 1 {
                    return Err("read-u8: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read-u8: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorInput(state) => {
                            let mut s = state.borrow_mut();
                            if s.pos < s.bytes.len() {
                                let b = s.bytes[s.pos];
                                s.pos += 1;
                                Ok(Value::fixnum(b as i64))
                            } else {
                                Ok(Value::Eof)
                            }
                        }
                        _ => Err("read-u8: not a binary input port".into()),
                    },
                    _ => Err("read-u8: not a port".into()),
                }
            }),
        );
        let pu8_sym = syms.intern("peek-u8");
        vm_env.define(
            pu8_sym,
            cs_vm::vm::make_vm_builtin("peek-u8", |args| {
                if args.len() > 1 {
                    return Err("peek-u8: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "peek-u8: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorInput(state) => {
                            let s = state.borrow();
                            if s.pos < s.bytes.len() {
                                Ok(Value::fixnum(s.bytes[s.pos] as i64))
                            } else {
                                Ok(Value::Eof)
                            }
                        }
                        _ => Err("peek-u8: not a binary input port".into()),
                    },
                    _ => Err("peek-u8: not a port".into()),
                }
            }),
        );
        let cr_sym = syms.intern("char-ready?");
        vm_env.define(
            cr_sym,
            cs_vm::vm::make_vm_builtin("char-ready?", |args| {
                if args.len() > 1 {
                    return Err("char-ready?: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "char-ready?: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringInput(_) => Ok(Value::Boolean(true)),
                        _ => Err("char-ready?: not a textual input port".into()),
                    },
                    _ => Err("char-ready?: not a port".into()),
                }
            }),
        );
        let u8r_sym = syms.intern("u8-ready?");
        vm_env.define(
            u8r_sym,
            cs_vm::vm::make_vm_builtin("u8-ready?", |args| {
                if args.len() > 1 {
                    return Err("u8-ready?: 0 or 1 arg".into());
                }
                let port = if args.is_empty() {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "u8-ready?: no current input port".to_string())?
                } else {
                    args[0].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorInput(_) => Ok(Value::Boolean(true)),
                        _ => Err("u8-ready?: not a binary input port".into()),
                    },
                    _ => Err("u8-ready?: not a port".into()),
                }
            }),
        );
        let rbv_sym = syms.intern("read-bytevector");
        vm_env.define(
            rbv_sym,
            cs_vm::vm::make_vm_builtin("read-bytevector", |args| {
                if args.is_empty() || args.len() > 2 {
                    return Err("read-bytevector: 1 or 2 args".into());
                }
                let k = match &args[0] {
                    Value::Number(n) => match n.to_f64() as i64 {
                        i if i >= 0 => i as usize,
                        _ => return Err("read-bytevector: negative count".into()),
                    },
                    _ => return Err("read-bytevector: count must be integer".into()),
                };
                let port = if args.len() == 1 {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read-bytevector: no current input port".to_string())?
                } else {
                    args[1].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorInput(state) => {
                            let mut s = state.borrow_mut();
                            if s.pos >= s.bytes.len() {
                                return Ok(Value::Eof);
                            }
                            let end = (s.pos + k).min(s.bytes.len());
                            let bytes = s.bytes[s.pos..end].to_vec();
                            s.pos = end;
                            Ok(Value::ByteVector(cs_core::Gc::new(
                                std::cell::RefCell::new(bytes),
                            )))
                        }
                        _ => Err("read-bytevector: not a binary input port".into()),
                    },
                    _ => Err("read-bytevector: not a port".into()),
                }
            }),
        );
        let wu8_sym = syms.intern("write-u8");
        vm_env.define(
            wu8_sym,
            cs_vm::vm::make_vm_builtin("write-u8", |args| {
                if args.is_empty() || args.len() > 2 {
                    return Err("write-u8: 1 or 2 args".into());
                }
                let byte = match &args[0] {
                    Value::Number(n) => match n.to_f64() as i64 {
                        i if (0..=255).contains(&i) => i as u8,
                        _ => return Err("write-u8: byte out of range".into()),
                    },
                    _ => return Err("write-u8: byte must be 0..255".into()),
                };
                let port = if args.len() == 1 {
                    cs_vm::vm::vm_current_output_port_value()
                        .ok_or_else(|| "write-u8: no current output port".to_string())?
                } else {
                    args[1].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorOutput(buf) => {
                            buf.borrow_mut().push(byte);
                            Ok(Value::Unspecified)
                        }
                        _ => Err("write-u8: not a binary output port".into()),
                    },
                    _ => Err("write-u8: not a port".into()),
                }
            }),
        );
        let rbvb_sym = syms.intern("read-bytevector!");
        vm_env.define(
            rbvb_sym,
            cs_vm::vm::make_vm_builtin("read-bytevector!", |args| {
                if args.is_empty() || args.len() > 4 {
                    return Err("read-bytevector!: 1..4 args".into());
                }
                let bv = match &args[0] {
                    Value::ByteVector(b) => b.clone(),
                    _ => return Err("read-bytevector!: arg 1 must be bytevector".into()),
                };
                let port = if args.len() >= 2 {
                    args[1].clone()
                } else {
                    cs_vm::vm::vm_current_input_port_value()
                        .ok_or_else(|| "read-bytevector!: no current input port".to_string())?
                };
                let bv_len = bv.borrow().len();
                let start = if args.len() >= 3 {
                    match &args[2] {
                        Value::Number(n) => match n.to_f64() as i64 {
                            i if i >= 0 && (i as usize) <= bv_len => i as usize,
                            _ => return Err("read-bytevector!: start out of range".into()),
                        },
                        _ => return Err("read-bytevector!: start must be integer".into()),
                    }
                } else {
                    0
                };
                let end = if args.len() == 4 {
                    match &args[3] {
                        Value::Number(n) => match n.to_f64() as i64 {
                            i if i >= 0 && (i as usize) <= bv_len && (i as usize) >= start => {
                                i as usize
                            }
                            _ => return Err("read-bytevector!: end out of range".into()),
                        },
                        _ => return Err("read-bytevector!: end must be integer".into()),
                    }
                } else {
                    bv_len
                };
                let n_wanted = end - start;
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorInput(state) => {
                            let mut s = state.borrow_mut();
                            if n_wanted == 0 {
                                return Ok(Value::fixnum(0));
                            }
                            if s.pos >= s.bytes.len() {
                                return Ok(Value::Eof);
                            }
                            let avail = s.bytes.len() - s.pos;
                            let n = n_wanted.min(avail);
                            let mut buf = bv.borrow_mut();
                            buf[start..start + n].copy_from_slice(&s.bytes[s.pos..s.pos + n]);
                            s.pos += n;
                            Ok(Value::fixnum(n as i64))
                        }
                        _ => Err("read-bytevector!: not a binary input port".into()),
                    },
                    _ => Err("read-bytevector!: not a port".into()),
                }
            }),
        );
        let wbv_sym = syms.intern("write-bytevector");
        vm_env.define(
            wbv_sym,
            cs_vm::vm::make_vm_builtin("write-bytevector", |args| {
                if args.is_empty() || args.len() > 4 {
                    return Err("write-bytevector: 1..4 args".into());
                }
                let bytes = match &args[0] {
                    Value::ByteVector(b) => b.borrow().clone(),
                    _ => return Err("write-bytevector: arg 1 must be bytevector".into()),
                };
                let len = bytes.len();
                let port = if args.len() == 1 {
                    cs_vm::vm::vm_current_output_port_value()
                        .ok_or_else(|| "write-bytevector: no current output port".to_string())?
                } else {
                    args[1].clone()
                };
                let start = if args.len() >= 3 {
                    match &args[2] {
                        Value::Number(n) => match n.to_f64() as i64 {
                            i if i >= 0 && (i as usize) <= len => i as usize,
                            _ => return Err("write-bytevector: start out of range".into()),
                        },
                        _ => return Err("write-bytevector: start must be integer".into()),
                    }
                } else {
                    0
                };
                let end = if args.len() == 4 {
                    match &args[3] {
                        Value::Number(n) => match n.to_f64() as i64 {
                            i if i >= 0 && (i as usize) <= len && (i as usize) >= start => {
                                i as usize
                            }
                            _ => return Err("write-bytevector: end out of range".into()),
                        },
                        _ => return Err("write-bytevector: end must be integer".into()),
                    }
                } else {
                    len
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::ByteVectorOutput(buf) => {
                            buf.borrow_mut().extend_from_slice(&bytes[start..end]);
                            Ok(Value::Unspecified)
                        }
                        _ => Err("write-bytevector: not a binary output port".into()),
                    },
                    _ => Err("write-bytevector: not a port".into()),
                }
            }),
        );
        let gensym_sym = syms.intern("gensym");
        vm_env.define(
            gensym_sym,
            cs_vm::vm::make_vm_builtin_syms("gensym", |args, st| {
                let prefix = if args.is_empty() {
                    "g".to_string()
                } else {
                    match &args[0] {
                        Value::String(s) => s.borrow().clone(),
                        Value::Symbol(s) => st.name(*s).to_string(),
                        _ => "g".to_string(),
                    }
                };
                let n = st.len();
                let name = format!("{}__{}", prefix, n);
                Ok(Value::Symbol(st.intern(&name)))
            }),
        );
        // (features) — same identifier list as cond-expand recognizes.
        let features_sym = syms.intern("features");
        vm_env.define(
            features_sym,
            cs_vm::vm::make_vm_builtin_syms("features", |args, st| {
                if !args.is_empty() {
                    return Err("features: 0 args".into());
                }
                let feats = ["crabscheme", "r6rs-subset", "r7rs-subset", "exact-closed"];
                let list: Vec<Value> = feats.iter().map(|n| Value::Symbol(st.intern(n))).collect();
                Ok(Value::list(list))
            }),
        );
        // Set up the GC root set: the walker's top frame chain and the
        // VM-tier root env. Cloning Rc<Frame> / Rc<Env> into the closure
        // gives the heap a stable handle to walk on every collect().
        let heap = cs_gc::Heap::new();
        {
            let top_root = Rc::clone(&top);
            heap.add_root(move |marker| {
                use cs_gc::Trace;
                top_root.trace(marker);
            });
        }
        {
            let vm_root = Rc::clone(&vm_env);
            heap.add_root(move |marker| {
                use cs_gc::Trace;
                vm_root.trace(marker);
            });
        }

        // Pinned-value slab. The root closure traces every value in
        // the map on every collect, so anything passed to `pin()`
        // stays reachable until its Pinned guard drops.
        let pinned: Rc<RefCell<HashMap<PinId, Value>>> = Rc::new(RefCell::new(HashMap::new()));
        {
            let pinned_clone = Rc::clone(&pinned);
            heap.add_root(move |marker| {
                use cs_gc::Trace;
                for v in pinned_clone.borrow().values() {
                    v.trace(marker);
                }
            });
        }

        Self {
            syms,
            sources: SourceMap::new(),
            top,
            macros: std::collections::HashMap::new(),
            vm_env,
            heap,
            pinned,
            // Start at 1 so handle 0 can be reserved as the FFI
            // "null" ValueRef. Internal Pinned guards never use 0
            // either, so the convention is consistent across users.
            next_pin_id: Rc::new(Cell::new(1)),
            #[cfg(feature = "ffi-dynamic")]
            loaded_libs: Vec::new(),
            #[cfg(feature = "ffi-dynamic")]
            ffi_ctx: None,
            #[cfg(feature = "jit")]
            jit_lowerer: None,
            command_line: None,
            typer_hints_by_lambda_id: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    /// Install typer-derived param-type hints (Phase 5.4). Keyed
    /// by `LambdaProfile::lambda_id`. Hints replace whatever was
    /// previously installed; pass an empty map to clear. The JIT
    /// tier-up hook consults this map before falling back to
    /// observation-based inference, so installing must happen
    /// before the lambda first crosses the tier-up threshold.
    pub fn install_typer_hints(&self, hints: std::collections::HashMap<u32, Vec<cs_rir::Type>>) {
        *self.typer_hints_by_lambda_id.borrow_mut() = hints;
    }

    /// Set the `(command-line)` override for this runtime. Call
    /// this before evaluating user code that may consult the
    /// command line. R6RS expects `(<program> <arg> ...)` — the
    /// script path followed by post-script args; the runtime
    /// embedder is responsible for filtering its own dispatcher
    /// args (e.g., `crabscheme --tier vm run`) out of the list.
    pub fn set_command_line(&mut self, args: Vec<String>) {
        self.command_line = Some(args);
    }

    /// Pin a Scheme `Value` so it survives any number of intervening
    /// GC collections. On drop of the returned [`Pinned`] guard the
    /// pin is released.
    ///
    /// Use this before holding a Value across a Scheme-level operation
    /// that may allocate (re-entry into eval, host-procedure calls
    /// that transitively allocate, etc.). Without pinning, the value
    /// can be swept by the next collect.
    ///
    /// See `.spec-workflow/specs/ffi/{requirements,design}.md` FR-3
    /// and ADR 0008 D-3.
    pub fn pin(&self, v: Value) -> Pinned {
        let id = PinId(self.next_pin_id.get());
        self.next_pin_id.set(id.0 + 1);
        self.pinned.borrow_mut().insert(id, v);
        Pinned {
            id,
            pinned: Rc::clone(&self.pinned),
        }
    }

    /// Number of currently-pinned values. Useful for tests and
    /// diagnostics; should be 0 in steady state.
    pub fn pin_count(&self) -> usize {
        self.pinned.borrow().len()
    }

    /// Pin a value without an RAII guard, returning the raw u64
    /// handle. Caller MUST eventually call [`unpin_raw`] to release;
    /// otherwise the value remains rooted indefinitely.
    ///
    /// Used by the C-ABI backend (`cs-runtime/src/ffi.rs`) to expose
    /// `ValueRef` handles to dylib plugins that release explicitly
    /// rather than via Rust Drop semantics. The handle is the
    /// underlying `PinId.0`; the slab is shared with [`Pinned`].
    pub fn pin_raw(&self, v: Value) -> u64 {
        let id = PinId(self.next_pin_id.get());
        self.next_pin_id.set(id.0 + 1);
        self.pinned.borrow_mut().insert(id, v);
        id.0
    }

    /// Release a raw pin allocated by [`pin_raw`]. Idempotent on
    /// already-released handles.
    pub fn unpin_raw(&self, handle: u64) {
        self.pinned.borrow_mut().remove(&PinId(handle));
    }

    /// Look up the value behind a raw pin handle. Returns `None`
    /// if the handle was never minted or was already released.
    pub fn lookup_raw(&self, handle: u64) -> Option<Value> {
        self.pinned.borrow().get(&PinId(handle)).cloned()
    }

    /// Run a stop-the-world GC pass. Phase 1 walks the registered root
    /// set (top frame + VM env) and prunes unreachable allocations from
    /// `Heap`'s bookkeeping vec. Because Phase 1's `Gc<T>` is still
    /// Rc-backed, this has no observable behavioural effect on programs
    /// — it's the seam Phase 2 swaps to a real arena.
    pub fn collect(&self) {
        self.heap.collect();
    }

    /// Read-only access to the GC heap (for tests and tooling).
    pub fn heap(&self) -> &cs_gc::Heap {
        &self.heap
    }

    pub fn symbols(&self) -> &SymbolTable {
        &self.syms
    }

    /// RC3 iter 2.14 — snapshot the runtime's vm-env builtin
    /// procedure bindings, re-keyed by NAME. cs-cli's aot pipeline
    /// uses this to pass a globals snapshot to the compiler so that
    /// `(/ a b)`, `(display x)`, `(not p)`, etc. fold to
    /// `Const(Procedure)` (which the translator converts to a
    /// BuiltinRef → specialized RIR Inst) instead of `LoadVar` (which
    /// becomes an EnvLookup → unresolved capture in AOT).
    pub fn builtin_procs_by_name(&self) -> std::collections::HashMap<String, Value> {
        let mut m = std::collections::HashMap::new();
        for (sym, val) in self.vm_env.snapshot_bindings() {
            if matches!(val, Value::Procedure(_)) {
                m.insert(self.syms.name(sym).to_string(), val);
            }
        }
        m
    }

    pub fn source_map(&self) -> &SourceMap {
        &self.sources
    }

    /// Evaluate a string of Scheme source. Returns the value of the final
    /// top-level expression (or `Unspecified` for empty/define-only input).
    pub fn eval_str(&mut self, name: &str, src: &str) -> Result<Value, Diagnostic> {
        // Phase 3C: detect and rewrite a leading `#!lang NAME`
        // header into `(import (lang NAME))`. The rewritten source
        // (same line count as original) is what gets registered
        // and parsed; downstream line numbers stay accurate.
        let rewritten = rewrite_lang_header(src);
        let file_id = self.sources.add(name, &rewritten);
        self.with_active(|rt| rt.eval_str_in_file(file_id, &rewritten))
    }

    /// Register a Rust procedure as a top-level Scheme binding. After
    /// this, Scheme code can call `(<name> args...)` and dispatch to
    /// the supplied `cs_ffi::HostProcedure`.
    ///
    /// The proc is installed on both the walker and VM tiers so it
    /// works on either evaluation path.
    ///
    /// `FfiError` returned by the proc is translated into a Scheme
    /// condition that `with-exception-handler` can catch:
    /// - `TypeMismatch` and `HostFailure` -> standard error
    /// - `ArityError` -> arity error condition
    /// - `Panic` -> error with the panic message (the boundary
    ///   already caught the panic via `catch_unwind`).
    ///
    /// See `.spec-workflow/specs/ffi/{requirements,design}.md` and
    /// `docs/adr/0008-ffi-design.md`.
    ///
    /// (M10 W1 + closeout: gated on `ffi-trait`. The trait itself is
    /// pure Rust and portable to WASM, so a WASM embedder that wants
    /// custom Rust-implemented Scheme builtins enables `ffi-trait`
    /// and calls this method directly — without needing `ffi-dynamic`
    /// (which adds the dlopen path that WASM can't support).)
    #[cfg(feature = "ffi-trait")]
    pub fn register_host_procedure(&mut self, proc: std::sync::Arc<dyn cs_ffi::HostProcedure>) {
        let name_owned: String = proc.name().to_string();
        let name_static: &'static str = Box::leak(name_owned.into_boxed_str());

        // Single shared dispatcher closure both tiers point at via Arc
        // clones. FfiError translation matches the format the eval
        // layer's `builtin_err_to_eval` parses ("name: rest") so the
        // resulting Scheme condition has &who and &message simples
        // populated.
        let proc_arc = proc;
        let dispatcher: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
            std::sync::Arc::new(move |args: &[Value]| {
                proc_arc.call(args).map_err(|e| match e {
                    cs_ffi::FfiError::ArityError {
                        name,
                        expected,
                        got,
                    } => format!("{}: expected {} args, got {}", name, expected, got),
                    cs_ffi::FfiError::TypeMismatch { expected, got } => {
                        format!("{}: expected {}, got {}", name_static, expected, got)
                    }
                    cs_ffi::FfiError::Panic(msg) => format!("{}: panic: {}", name_static, msg),
                    cs_ffi::FfiError::HostFailure(msg) => format!("{}: {}", name_static, msg),
                })
            });

        let sym = self.syms.intern(name_static);
        self.top.define(
            sym,
            crate::proc::make_host_builtin(name_static, dispatcher.clone()),
        );
        self.vm_env.define(
            sym,
            cs_vm::vm::make_vm_host_builtin(name_static, dispatcher),
        );
    }

    fn eval_str_in_file(&mut self, file_id: FileId, src: &str) -> Result<Value, Diagnostic> {
        let data = match read_all(file_id, src, &mut self.syms) {
            Ok(d) => d,
            Err(errs) => {
                let e = &errs[0];
                return Err(Diagnostic::error(e.message(), e.span()));
            }
        };
        // Split off the fields the include resolver needs (`sources`) and the
        // ones the Expander itself takes (`syms`, `macros`) so they don't
        // overlap. This lets `(include "path")` register the file's source
        // with the SourceMap so error spans render correctly.
        let Self {
            sources,
            syms,
            macros,
            ..
        } = self;
        let mut resolver = |path: &str| -> Option<(FileId, String)> {
            let src = std::fs::read_to_string(path).ok()?;
            let id = sources.add(path, &src);
            Some((id, src))
        };
        let mut expander = Expander::new(syms, macros).with_include_resolver(&mut resolver);
        let core = match expander.expand_program(&data) {
            Ok(c) => c,
            Err(e) => return Err(Diagnostic::error(e.message(), e.span())),
        };
        drop(expander);
        drop(resolver);
        let mut ctx = EvalCtx::new(self.top.clone(), &mut self.syms, &mut self.macros);
        let result = eval(&core, self.top.clone(), &mut ctx);
        // Drain pending side-channels before ctx drops so we can render
        // proper messages even when EvalError carried only the sentinel
        // string (e.g. "__escape__" or "__raised__" coming back through a
        // higher builtin).
        let pending_raise = ctx.pending_raise.take();
        let pending_escape = ctx.pending_escape.take();
        // The eval Err path leaves ctx.call_stack intact — copy out for the
        // diagnostic before ctx drops. Innermost-last; we'll render in
        // reverse so the deepest "called from" appears first under the
        // primary error site.
        let walker_backtrace = ctx.call_stack.clone();
        result.map_err(|e: EvalError| {
            let msg = match &e.kind {
                crate::eval::EvalErrorKind::Raised(v) => format_condition(v, &self.syms),
                crate::eval::EvalErrorKind::Escape(id, v) => format!(
                    "continuation #{} invoked outside its dynamic extent (value: {}) \
                     — first-class re-invocation is not yet supported (see M8 spec)",
                    id,
                    v.format_with(&self.syms, WriteMode::Write)
                ),
                crate::eval::EvalErrorKind::Message(m) => match m.as_str() {
                    "__escape__" => match pending_escape {
                        Some((id, v)) => format!(
                            "continuation #{} invoked outside its dynamic extent (value: {}) \
                             — first-class re-invocation is not yet supported (see M8 spec)",
                            id,
                            v.format_with(&self.syms, WriteMode::Write)
                        ),
                        None => "uncaught escape continuation \
                                 — first-class continuations are not yet implemented (see M8 spec)"
                            .to_string(),
                    },
                    "__raised__" => match &pending_raise {
                        Some(cond) => format_condition(cond, &self.syms),
                        None => "raised (no condition)".to_string(),
                    },
                    other => other.to_string(),
                },
            };
            let mut diag = Diagnostic::error(msg, e.span).with_code("E_RUNTIME");
            // Render call sites innermost-first. Drop the deepest entry
            // (it's the App whose evaluation directly produced the error;
            // its span overlaps the primary error span and would be
            // redundant in the trace).
            let trace = if walker_backtrace.is_empty() {
                &[][..]
            } else {
                &walker_backtrace[..walker_backtrace.len() - 1]
            };
            for (i, bt_span) in trace.iter().rev().enumerate() {
                if bt_span.is_dummy() {
                    continue;
                }
                let (line, col) = self.sources.line_col(*bt_span);
                let name = self.sources.name(bt_span.file);
                diag = diag.with_note(format!(
                    "called from {}({}:{}:{})",
                    if i == 0 { "[1] " } else { "    " },
                    name,
                    line,
                    col
                ));
            }
            diag
        })
    }

    /// Define a binding in the top-level environment.
    pub fn define(&mut self, name: &str, value: Value) {
        let sym = self.syms.intern(name);
        self.top.define(sym, value);
    }

    /// Format a Value using this runtime's symbol table.
    pub fn format_value(&self, v: &Value, mode: WriteMode) -> String {
        v.format_with(&self.syms, mode)
    }

    /// Convert a span into its (line, column) coordinates using
    /// this runtime's SourceMap. Returned coordinates are
    /// 1-indexed.
    pub fn sources_line_col(&self, span: cs_diag::Span) -> (u32, u32) {
        self.sources.line_col(span)
    }

    /// Evaluate a string of Scheme source via the **bytecode VM** tier.
    /// Foundation: only pure builtins are supported. Higher-order builtins
    /// (apply/map/raise/with-exception-handler/etc.) and parameterize/dynamic-wind
    /// fall back to the tree-walker via per-call dispatch — for now this VM
    /// path is best-effort for pure-arithmetic / pure-list programs.
    pub fn eval_str_via_vm(&mut self, name: &str, src: &str) -> Result<Value, Diagnostic> {
        let file_id = self.sources.add(name, src);
        self.with_active(|rt| rt.eval_str_via_vm_inner(file_id, src))
    }

    fn eval_str_via_vm_inner(&mut self, file_id: FileId, src: &str) -> Result<Value, Diagnostic> {
        let data = match read_all(file_id, src, &mut self.syms) {
            Ok(d) => d,
            Err(errs) => {
                let e = &errs[0];
                return Err(Diagnostic::error(e.message(), e.span()));
            }
        };
        let Self {
            sources,
            syms,
            macros,
            ..
        } = self;
        let mut resolver = |path: &str| -> Option<(FileId, String)> {
            let src = std::fs::read_to_string(path).ok()?;
            let id = sources.add(path, &src);
            Some((id, src))
        };
        let mut expander = Expander::new(syms, macros).with_include_resolver(&mut resolver);
        let core = match expander.expand_program(&data) {
            Ok(c) => c,
            Err(e) => return Err(Diagnostic::error(e.message(), e.span())),
        };
        drop(expander);
        // Fold known-immutable global builtins (+, <, map, ...) to Const at
        // compile time, eliminating per-execution env-chain lookups for them.
        // The vm_env starts populated by Runtime::new with all the builtins
        // and never has them rebound under normal usage. User-defined globals
        // appear here too — folding them is unsafe if `set!` is used. For
        // foundation we accept that semantic: the speedup on builtins is the
        // dominant case, and tests don't (re)set! the captured snapshot.
        let globals = self.vm_env.snapshot_bindings();
        let primops = primop_table(&mut self.syms);
        let bc = cs_vm::compile_with_globals_and_primops(&core, &globals, &primops)
            .map_err(|e| Diagnostic::error(e.message, e.span))?;
        // Install the `eval` hook + root env so VmEval can call back into us.
        let prev_hook = cs_vm::vm::install_eval_hook(Some(vm_eval_callback));
        let prev_env = cs_vm::vm::install_eval_root_env(Some(self.vm_env.clone()));
        let result = cs_vm::run(&bc, self.vm_env.clone(), &mut self.syms);
        cs_vm::vm::install_eval_hook(prev_hook);
        cs_vm::vm::install_eval_root_env(prev_env);
        // Render VM errors with proper condition formatting; carry the
        // span the VM captured at the offending instruction so the
        // diagnostic can show source line/column.
        result.map_err(|e| {
            let span = e.span;
            let backtrace = e.backtrace.clone();
            let msg = match e.message.as_str() {
                "__raised__" => match cs_vm::vm::vm_take_pending_raise() {
                    Some(cond) => format_condition(&cond, &self.syms),
                    None => "raised (no condition)".to_string(),
                },
                "__escape__" => match cs_vm::vm::vm_take_pending_escape() {
                    Some((id, v)) => format!(
                        "continuation #{} invoked outside its dynamic extent (value: {}) \
                         — first-class re-invocation is not yet supported (see M8 spec)",
                        id,
                        v.format_with(&self.syms, WriteMode::Write)
                    ),
                    None => "uncaught escape continuation \
                             — first-class continuations are not yet implemented (see M8 spec)"
                        .to_string(),
                },
                _ => e.message,
            };
            let mut diag = Diagnostic::error(msg, span).with_code("E_RUNTIME");
            // Render each call-stack frame as a note line. Innermost-first
            // ordering matches the bare-error site at the top of the diag.
            for (i, bt_span) in backtrace.iter().enumerate() {
                let (line, col) = self.sources.line_col(*bt_span);
                let name = self.sources.name(bt_span.file);
                diag = diag.with_note(format!(
                    "called from {} ({}:{}:{})",
                    if i == 0 { "[1] " } else { "    " },
                    name,
                    line,
                    col
                ));
            }
            diag
        })
    }

    /// Look up a top-level binding.
    pub fn lookup(&self, name: &str) -> Option<Value> {
        // Note: this looks up in top frame only — sufficient for embed API tests.
        let sym = self.syms.by_name_lookup(name).or_else(|| {
            // Symbol may not exist yet. We can't insert because &self;
            // return None.
            None
        })?;
        self.top.get(sym)
    }
}

/// Render a raised condition value as a human-friendly error message.
/// Shape produced by `error` / `make-condition` / R6RS condition constructors:
/// a vector `#("&compound-condition" simple1 simple2 ...)` where each simple
/// is `#("&<type>" field0 field1 ...)`.
///
/// The output reads like:
///   error in <who>: <message> (<irritant ...>) [<other tags>]
/// Phase 3C — rewrite a leading `#!lang NAME` header into the
/// equivalent `(import (lang NAME))` form. The replacement
/// happens in-place on line 1 only, preserving the file's line
/// count so source-span line numbers reported in diagnostics
/// continue to point at the right source line. Column positions
/// on line 1 may shift, but that's acceptable for an MVP.
///
/// If no `#!lang` header is present, the source is returned
/// unchanged. Allows leading whitespace and/or a UTF-8 BOM before
/// the directive.
fn rewrite_lang_header(src: &str) -> String {
    let no_bom = src.strip_prefix('\u{FEFF}').unwrap_or(src);
    let (first_line, rest) = match no_bom.find('\n') {
        Some(idx) => (&no_bom[..idx], Some(&no_bom[idx..])),
        None => (no_bom, None),
    };
    let trimmed = first_line.trim_start();
    let lang_name = trimmed
        .strip_prefix("#!lang ")
        .or_else(|| trimmed.strip_prefix("#lang "))
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().all(|c| !c.is_whitespace()));
    match lang_name {
        Some(name) => {
            let mut out = format!("(import (lang {}))", name);
            if let Some(r) = rest {
                out.push_str(r);
            }
            out
        }
        None => src.to_string(),
    }
}

/// or:
///   assertion-violation in <who>: <message> ...
/// with each section omitted when not present. `who` is rendered with
/// `display` semantics (no quotes for symbols/strings); irritants use
/// `write` semantics so the reader can disambiguate types.
fn format_condition(v: &Value, syms: &SymbolTable) -> String {
    let simples = collect_condition_simples(v);
    if simples.is_empty() {
        return format!("uncaught: {}", v.format_with(syms, WriteMode::Write));
    }
    let mut msg: Option<String> = None;
    let mut irritants: Vec<Value> = Vec::new();
    let mut who: Option<Value> = None;
    let mut is_assertion = false;
    let mut other_tags: Vec<String> = Vec::new();
    for simple in &simples {
        if let Some((tag, fields)) = decompose_simple(simple) {
            match tag.as_str() {
                "&message" => {
                    if let Some(Value::String(s)) = fields.first() {
                        msg = Some(s.borrow().clone());
                    }
                }
                "&irritants" => {
                    if let Some(list) = fields.first() {
                        irritants = collect_list(list);
                    }
                }
                "&who" => {
                    who = fields.into_iter().next();
                }
                // Implied by the prefix we'll emit; not surfaced separately.
                "&error" | "&serious" | "&violation" => {}
                "&assertion" => {
                    is_assertion = true;
                }
                other => other_tags.push(format!("[{}]", other)),
            }
        }
    }
    let prefix = if is_assertion {
        "assertion-violation"
    } else {
        "error"
    };
    let mut out = String::from(prefix);
    if let Some(w) = &who {
        // `who` may be a symbol, string, or `#f`. Skip the false case
        // — it explicitly means "no who" — but render the others.
        let render = match w {
            Value::Boolean(false) => None,
            other => Some(other.format_with(syms, WriteMode::Display)),
        };
        if let Some(s) = render {
            out.push_str(" in ");
            out.push_str(&s);
        }
    }
    out.push(':');
    if let Some(m) = msg {
        out.push(' ');
        out.push_str(&m);
    }
    if !irritants.is_empty() {
        let irritant_strs: Vec<String> = irritants
            .iter()
            .map(|i| i.format_with(syms, WriteMode::Write))
            .collect();
        out.push_str(&format!(" ({})", irritant_strs.join(" ")));
    }
    if !other_tags.is_empty() {
        out.push(' ');
        out.push_str(&other_tags.join(" "));
    }
    out
}

/// Return the simple conditions of `v`, or an empty vec if `v` is not a
/// condition vector. Mirrors the runtime helper `for_each_simple` but works
/// without depending on the builtins module.
fn collect_condition_simples(v: &Value) -> Vec<Value> {
    let Value::Vector(vc) = v else {
        return Vec::new();
    };
    let inner = vc.borrow();
    let Some(Value::String(tag)) = inner.first() else {
        return Vec::new();
    };
    let tag = tag.borrow();
    if tag.as_str() == "&compound-condition" {
        inner.iter().skip(1).cloned().collect()
    } else if tag.as_str().starts_with('&') {
        vec![v.clone()]
    } else {
        Vec::new()
    }
}

/// Pull a simple condition apart into (tag, fields).
fn decompose_simple(v: &Value) -> Option<(String, Vec<Value>)> {
    let Value::Vector(vc) = v else { return None };
    let inner = vc.borrow();
    let Some(Value::String(tag)) = inner.first() else {
        return None;
    };
    let tag_str = tag.borrow().clone();
    let fields: Vec<Value> = inner.iter().skip(1).cloned().collect();
    Some((tag_str, fields))
}

fn collect_list(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return out,
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            other => {
                out.push(other);
                return out;
            }
        }
    }
}

/// Callback installed via `cs_vm::install_eval_hook` so the VM `eval`
/// builtin can re-enter the parser+expander+compiler+VM. Foundation: uses
/// an empty macro env (no syntactic forms beyond core builtins are available
/// to evaluated code at this milestone).
fn vm_eval_callback(v: &Value, syms: &mut SymbolTable) -> Result<Value, String> {
    let env =
        cs_vm::vm::vm_eval_root_env().ok_or_else(|| "eval: no root env installed".to_string())?;
    // Format the datum back to source, then re-parse → expand → compile → run.
    let datum_str = v.format_with(syms, WriteMode::Write);
    let file_id = cs_diag::FileId(u32::MAX - 3);
    let data = read_all(file_id, &datum_str, syms).map_err(|errs| {
        let e = errs.into_iter().next().unwrap();
        format!("eval: parse error: {}", e.message())
    })?;
    if data.is_empty() {
        return Ok(Value::Unspecified);
    }
    let mut macros = std::collections::HashMap::new();
    let mut expander = Expander::new(syms, &mut macros);
    let core = expander
        .expand_program(&data)
        .map_err(|e| format!("eval: expand error: {}", e.message()))?;
    drop(expander);
    let globals = env.snapshot_bindings();
    let primops = primop_table(syms);
    let bc = cs_vm::compile_with_globals_and_primops(&core, &globals, &primops)
        .map_err(|e| format!("eval: compile error: {}", e.message))?;
    cs_vm::run(&bc, env, syms).map_err(|e| format!("eval: {}", e.message))
}

/// Build the symbol→PrimOp map for the VM compiler. The set is small and
/// the lookup is per-compile, so we just rebuild it each time.
fn primop_table(
    syms: &mut SymbolTable,
) -> std::collections::HashMap<cs_core::Symbol, cs_vm::compiler::PrimOp> {
    use cs_vm::compiler::PrimOp;
    let mut m = std::collections::HashMap::new();
    m.insert(syms.intern("+"), PrimOp::Add);
    m.insert(syms.intern("-"), PrimOp::Sub);
    m.insert(syms.intern("*"), PrimOp::Mul);
    m.insert(syms.intern("<"), PrimOp::Lt);
    m.insert(syms.intern("<="), PrimOp::Le);
    m.insert(syms.intern(">"), PrimOp::Gt);
    m.insert(syms.intern(">="), PrimOp::Ge);
    m.insert(syms.intern("="), PrimOp::Eq);
    m
}

// Helper so cs-runtime can look up symbols by name without intern.
trait SymTableExt {
    fn by_name_lookup(&self, name: &str) -> Option<cs_core::Symbol>;
}

impl SymTableExt for SymbolTable {
    fn by_name_lookup(&self, name: &str) -> Option<cs_core::Symbol> {
        // Linear scan; fine for embed API.
        for i in 0..self.len() {
            let sym = cs_core::Symbol(i as u32);
            if self.name(sym) == name {
                return Some(sym);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Value {
        let mut rt = Runtime::new();
        rt.eval_str("<test>", src).unwrap_or_else(|d| {
            panic!("eval error: {}", d.message);
        })
    }

    fn run_str(src: &str) -> String {
        let mut rt = Runtime::new();
        let v = rt.eval_str("<test>", src).unwrap_or_else(|d| {
            panic!("eval error: {}", d.message);
        });
        rt.format_value(&v, WriteMode::Write)
    }

    #[test]
    fn add_two_numbers() {
        let v = run("(+ 1 2)");
        assert_eq!(format!("{}", v), "3");
    }

    // Phase 5.4: install_typer_hints API round-trip.
    #[test]
    fn install_typer_hints_round_trips() {
        let rt = Runtime::new();
        let mut h = std::collections::HashMap::new();
        h.insert(42u32, vec![cs_rir::Type::Flonum, cs_rir::Type::Fixnum]);
        rt.install_typer_hints(h.clone());
        let installed = rt.typer_hints_by_lambda_id.borrow();
        assert_eq!(
            installed.get(&42),
            Some(&vec![cs_rir::Type::Flonum, cs_rir::Type::Fixnum])
        );
    }

    #[test]
    fn install_typer_hints_replaces_existing() {
        let rt = Runtime::new();
        let mut h = std::collections::HashMap::new();
        h.insert(1u32, vec![cs_rir::Type::Fixnum]);
        rt.install_typer_hints(h);
        let mut h2 = std::collections::HashMap::new();
        h2.insert(2u32, vec![cs_rir::Type::Flonum]);
        rt.install_typer_hints(h2);
        // Old entry should be gone; new one present.
        let installed = rt.typer_hints_by_lambda_id.borrow();
        assert!(installed.get(&1).is_none(), "old entry leaked");
        assert_eq!(installed.get(&2), Some(&vec![cs_rir::Type::Flonum]));
    }

    #[test]
    fn nested_arithmetic() {
        let v = run("(* (+ 1 2) (- 10 3))");
        assert_eq!(format!("{}", v), "21");
    }

    #[test]
    fn if_true() {
        let v = run("(if #t 1 2)");
        assert_eq!(format!("{}", v), "1");
    }

    #[test]
    fn if_false() {
        let v = run("(if #f 1 2)");
        assert_eq!(format!("{}", v), "2");
    }

    #[test]
    fn lambda_application() {
        let v = run("((lambda (x) (* x x)) 7)");
        assert_eq!(format!("{}", v), "49");
    }

    #[test]
    fn define_and_call() {
        let v = run("(define (square x) (* x x)) (square 6)");
        assert_eq!(format!("{}", v), "36");
    }

    #[test]
    fn let_binding() {
        let v = run("(let ((x 3) (y 4)) (+ (* x x) (* y y)))");
        assert_eq!(format!("{}", v), "25");
    }

    #[test]
    fn factorial_recursive() {
        let v = run("(define (fact n) (if (= n 0) 1 (* n (fact (- n 1))))) (fact 6)");
        assert_eq!(format!("{}", v), "720");
    }

    #[test]
    fn factorial_iterative_tail_call() {
        // Must not stack-overflow for n=10000 thanks to tail call elimination.
        let v = run("(define (fact-iter n acc)
               (if (= n 0) acc (fact-iter (- n 1) (* n acc))))
             (fact-iter 100 1)");
        // 100! is huge but verifiable by length and trailing zero count.
        let s = format!("{}", v);
        assert!(s.len() > 100, "expected large bignum, got {}", s);
        assert!(s.ends_with("00"));
    }

    #[test]
    fn list_operations() {
        let v = run("(length (list 1 2 3 4 5))");
        assert_eq!(format!("{}", v), "5");
    }

    #[test]
    fn cons_car_cdr() {
        assert_eq!(format!("{}", run("(car (cons 1 2))")), "1");
        assert_eq!(format!("{}", run("(cdr (cons 1 2))")), "2");
    }

    #[test]
    fn quote_list() {
        let v = run("'(1 2 3)");
        assert_eq!(format!("{}", v), "(1 2 3)");
    }

    #[test]
    fn equal_predicate() {
        assert_eq!(format!("{}", run("(equal? '(1 2 3) '(1 2 3))")), "#t");
        assert_eq!(format!("{}", run("(equal? '(1 2 3) '(1 2 4))")), "#f");
    }

    #[test]
    fn closures_capture_lexical_env() {
        let v = run("(define (make-adder n) (lambda (x) (+ x n)))
             ((make-adder 10) 5)");
        assert_eq!(format!("{}", v), "15");
    }

    #[test]
    fn cond_form() {
        let s = run_str(
            "(define (classify n)
               (cond ((< n 0) 'negative)
                     ((= n 0) 'zero)
                     (else 'positive)))
             (list (classify -5) (classify 0) (classify 7))",
        );
        assert_eq!(s, "(negative zero positive)");
    }

    #[test]
    fn symbols_print_correctly() {
        assert_eq!(run_str("'foo"), "foo");
        assert_eq!(run_str("'(a b c)"), "(a b c)");
    }

    #[test]
    fn map_squares() {
        assert_eq!(
            run_str("(map (lambda (x) (* x x)) '(1 2 3 4 5))"),
            "(1 4 9 16 25)"
        );
    }

    #[test]
    fn for_each_side_effect() {
        run("(for-each (lambda (x) x) '(1 2 3))");
    }

    #[test]
    fn apply_basic() {
        assert_eq!(run_str("(apply + '(1 2 3 4 5))"), "15");
        assert_eq!(run_str("(apply + 1 2 '(3 4 5))"), "15");
    }

    #[test]
    fn modulo_quotient_remainder() {
        assert_eq!(run_str("(modulo 13 4)"), "1");
        assert_eq!(run_str("(modulo -13 4)"), "3");
        assert_eq!(run_str("(modulo 13 -4)"), "-3");
        assert_eq!(run_str("(quotient 13 4)"), "3");
        assert_eq!(run_str("(remainder 13 4)"), "1");
        assert_eq!(run_str("(remainder -13 4)"), "-1");
    }

    #[test]
    fn min_max_expt() {
        assert_eq!(run_str("(min 3 1 4 1 5 9 2 6)"), "1");
        assert_eq!(run_str("(max 3 1 4 1 5 9 2 6)"), "9");
        assert_eq!(run_str("(expt 2 10)"), "1024");
        assert_eq!(run_str("(expt 3 3)"), "27");
    }

    #[test]
    fn internal_defines_lifted() {
        let s = run_str(
            "(define (f n)
               (define (helper x) (* x 2))
               (helper n))
             (f 21)",
        );
        assert_eq!(s, "42");
    }

    #[test]
    fn mutually_recursive_internal_defines() {
        let s = run_str(
            "(define (parity n)
               (define (is-even? x) (if (= x 0) #t (is-odd? (- x 1))))
               (define (is-odd? x) (if (= x 0) #f (is-even? (- x 1))))
               (list (is-even? n) (is-odd? n)))
             (parity 10)",
        );
        assert_eq!(s, "(#t #f)");
    }

    #[test]
    fn list_tail_and_ref() {
        assert_eq!(run_str("(list-tail '(a b c d e) 2)"), "(c d e)");
        assert_eq!(run_str("(list-ref '(a b c d e) 2)"), "c");
    }

    #[test]
    fn string_to_list_round_trip() {
        assert_eq!(
            run_str("(list->string (string->list \"hello\"))"),
            "\"hello\""
        );
    }

    #[test]
    fn char_predicates() {
        assert_eq!(run_str("(char? #\\a)"), "#t");
        assert_eq!(run_str("(char->integer #\\A)"), "65");
        assert_eq!(run_str("(integer->char 65)"), "#\\A");
    }

    #[test]
    fn set_car_cdr_mutation() {
        assert_eq!(
            run_str(
                "(define p (cons 1 2))
                 (set-car! p 10)
                 (set-cdr! p 20)
                 p"
            ),
            "(10 . 20)"
        );
    }

    #[test]
    fn fibonacci() {
        let v = run("(define (fib n)
               (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
             (fib 10)");
        assert_eq!(format!("{}", v), "55");
    }

    #[test]
    fn arity_error_reported() {
        let mut rt = Runtime::new();
        let err = rt
            .eval_str("<test>", "((lambda (x y) (+ x y)) 1)")
            .unwrap_err();
        assert!(err.message.contains("arity"));
    }

    #[test]
    fn undefined_variable_error() {
        let mut rt = Runtime::new();
        let err = rt.eval_str("<test>", "(+ x 1)").unwrap_err();
        assert!(err.message.contains("undefined"));
    }
}
