//! CrabScheme runtime: tree-walking interpreter, environments, builtins.

pub mod active;
#[cfg(feature = "regions")]
pub mod alloc_dispatch;
pub mod builtins;
pub mod countable_memory_cycle;
pub mod env;
pub mod eval;
mod lang_reader;
#[cfg(feature = "regions")]
pub mod regions;
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
use cs_diag::{Diagnostic, FileId, SourceMap, Span};
use cs_expand::Expander;
use cs_parse::{read_all, Datum};

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
    /// Opt-in testing-toolkit libraries (expect/mock/prop/spec) loaded
    /// on demand by `(import (crab …))` this session. They are NOT
    /// auto-loaded with the always-on bundled libs because their DSL
    /// macros (`describe`/`it`/`expect`/…) use common identifiers that
    /// would shadow user/test bindings in every Runtime (PR #105: a
    /// global `describe` macro broke a `define/contract describe` test).
    /// Tracks which have loaded so each loads at most once.
    #[cfg(feature = "bundled-scheme")]
    loaded_optin_libs: std::collections::HashSet<String>,
    /// Library exports declared across the runtime's lifetime,
    /// mirrored from each per-call `Expander::libraries()` map
    /// (which only survives the call that built it). Keyed by the
    /// library name (e.g. `[sym("lang"), sym("foo")]`); value is
    /// the declared export list. Used by the `#!lang` custom-
    /// reader pipeline (issue #10) to decide whether `(lang NAME)`
    /// opted into the parse-time reader protocol via
    /// `(export reader ...)`.
    library_exports: std::collections::HashMap<Vec<cs_core::Symbol>, Vec<cs_core::Symbol>>,
    /// VM-tier persistent root env (lazily populated with pure builtins at construction).
    vm_env: Rc<cs_vm::vm::Env>,
    /// Slab of pinned values that survive intervening collects.
    /// Populated by [`Runtime::pin`]; cleared on the returned
    /// [`Pinned`] guard's drop. Cloned (Rc-shared) into the Pinned
    /// guard so drop can remove the slot without holding the
    /// Runtime.
    pinned: Rc<RefCell<HashMap<PinId, Value>>>,
    /// Monotonic counter producing fresh [`PinId`]s. Starts at 1
    /// so handle 0 can be reserved by FFI as the null
    /// [`crate::ffi::ValueRef`].
    next_pin_id: Rc<Cell<u64>>,
    /// Dynamically loaded shared libraries kept alive for the
    /// runtime's lifetime so dlopen'd host procedures' captured
    /// back-pointers stay valid.
    #[cfg(feature = "ffi-dynamic")]
    loaded_libs: Vec<libloading::Library>,
    /// Dlopen-time C-ABI context. Boxed so the runtime
    /// back-pointer (which equals `self`) stays valid even if
    /// Runtime fields are reordered. Only the dlopen path
    /// constructs this — `ffi-trait`-only embedders register
    /// their procedures via `register_host_procedure` directly.
    #[cfg(feature = "ffi-dynamic")]
    ffi_ctx: Option<Box<crate::ffi::RuntimeFfiContext>>,
    /// JIT lowerer; populated by [`Runtime::install_jit`]. None
    /// means the runtime hasn't opted into JIT (closures stay on
    /// the bytecode VM regardless of tier-up).
    /// (M10 W1: gated on the `jit` feature — WASM has no runtime
    /// native codegen.)
    #[cfg(feature = "jit")]
    pub(crate) jit_lowerer: Option<cs_jit_cranelift::Lowerer>,
    /// JIT poison flag. Set when a JIT compile panicked, or when the
    /// pre-codegen verifier rejected a structurally-malformed
    /// function (`JitError::Malformed`). Once set, `jit_tier_up_hook`
    /// short-circuits and every closure stays on the bytecode VM
    /// (correct by construction).
    ///
    /// Per-`Runtime`, not thread-local (issue #18): the post-1.0
    /// work-stealing scheduler shares worker threads across
    /// Runtimes, and a thread-local flag would let one Runtime's
    /// panic poison JIT for unrelated Runtimes stolen onto the same
    /// worker. `Rc<Cell<_>>` so `jit_tier_up_hook` can hold a handle
    /// independent of the `&mut Runtime` borrow it takes for the
    /// Lowerer. Cleared via [`Runtime::reset_jit_poison`] (issue #17).
    #[cfg(feature = "jit")]
    pub(crate) jit_poisoned: Rc<Cell<bool>>,
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
    /// L1 sandbox import-set policy (ADR 0015 issue #15).
    /// When `Some`, every `(environment ...)` call rejects import specs
    /// not in this list. Set via [`Runtime::set_sandbox_import_policy`]
    /// before calling `eval_str`. `None` means unrestricted.
    sandbox_import_policy: Option<Vec<String>>,
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

/// The immutable, shareable base of a [`Runtime`] — its builtins env (both
/// tiers), bundled libraries, symbol table, and macros — built **once** and
/// shared (by `Rc` / cheap clone) across many per-actor Runtimes via
/// [`Runtime::from_image`]. The shared-Runtime model (green-threads): a worker
/// builds one image, then each green actor gets a cheap per-actor Runtime that
/// overlays its own defines on this base instead of rebuilding the whole env
/// (~826 KiB → a small overlay). The base is never mutated after construction.
///
/// `Rc`-based, so `!Send`: an image lives on one thread (the LocalSet worker
/// that built it), shared only by actors on that worker — the same isolation
/// the per-actor Runtimes already rely on.
pub struct RuntimeImage {
    vm_env: Rc<cs_vm::vm::Env>,
    top: Rc<Frame>,
    /// The base symbol table, shared (`Rc`) by every per-actor table layered over
    /// it via [`SymbolTable::with_base`] — so builtin/library symbol ids stay
    /// consistent with the base env, and each actor pays only for its own symbols.
    syms: Rc<SymbolTable>,
    macros: std::collections::HashMap<cs_core::Symbol, cs_expand::Macro>,
    library_exports: std::collections::HashMap<Vec<cs_core::Symbol>, Vec<cs_core::Symbol>>,
    #[cfg(feature = "bundled-scheme")]
    loaded_optin_libs: std::collections::HashSet<String>,
}

impl RuntimeImage {
    /// Build the base once: a full [`Runtime::new`], keeping its builtins/bundled
    /// env (`vm_env`/`top`, `Rc`-shared so they outlive the template), symbol
    /// table, and macros. The template's per-instance state (pinned slab, JIT,
    /// command-line, …) is dropped — only the immutable base is retained.
    pub fn build() -> Self {
        let rt = Runtime::new();
        RuntimeImage {
            vm_env: rt.vm_env.clone(),
            top: rt.top.clone(),
            syms: Rc::new(rt.syms),
            macros: rt.macros.clone(),
            library_exports: rt.library_exports.clone(),
            #[cfg(feature = "bundled-scheme")]
            loaded_optin_libs: rt.loaded_optin_libs.clone(),
        }
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

/// Embedder-facing configuration for the layer-4 tracing
/// cycle collector (tracing-revival spec iter 5). Gated on
/// the `tracing-cycle-collector` feature.
#[cfg(feature = "tracing-cycle-collector")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TracingPolicy {
    /// Registry-size at which the next allocation
    /// auto-triggers a sweep. Defaults to 10_000 — embedders
    /// running short workloads with many transient cycles
    /// can raise it; tight loops with long-lived cycles can
    /// lower it.
    pub auto_trigger_threshold: usize,
}

#[cfg(feature = "tracing-cycle-collector")]
impl Default for TracingPolicy {
    fn default() -> Self {
        TracingPolicy {
            auto_trigger_threshold: 10_000,
        }
    }
}

impl Runtime {
    /// Apply a [`TracingPolicy`] to this thread's cycle-
    /// candidate registry. Today only sets the
    /// auto-trigger threshold; background-sweep wiring is
    /// future work (the registry is per-thread via
    /// `thread_local!` which doesn't compose with a foreign
    /// sweep thread without redesign — see ADR 0018's
    /// deferred-work list).
    #[cfg(feature = "tracing-cycle-collector")]
    pub fn set_tracing_policy(&mut self, policy: TracingPolicy) {
        cs_gc::cycle_registry::set_auto_trigger_threshold(policy.auto_trigger_threshold);
    }

    pub fn new() -> Self {
        let mut rt = Self::new_inner();
        rt.register_stdlib();
        rt
    }

    /// A cheap per-actor Runtime that **shares** `image`'s immutable base
    /// (builtins env + bundled libs + macros) instead of rebuilding it. The VM
    /// env is a per-actor [`Env::child_define_root`](cs_vm::vm::Env::child_define_root)
    /// overlay: the body's top-level `(define …)`s land in the overlay (isolated
    /// from the shared base and from peer actors — Wall 1's define-boundary),
    /// while lookups fall through to the base for builtins/libraries. `top` is
    /// shared read-only (a green VM-tier body never defines into the walker env);
    /// `syms`/`macros` are cheap clones (interned `Rc<str>` shared) so each actor
    /// can intern/define freely with builtin ids consistent with the base.
    ///
    /// This is the shared-Runtime memory lever (green-threads): N actors cost one
    /// base + N small overlays, not N × full `Runtime::new()`.
    pub fn from_image(image: &RuntimeImage) -> Self {
        let vm_env = cs_vm::vm::Env::child_define_root(image.vm_env.clone());
        Self {
            // Layer a tiny per-actor extension over the shared base table (an Rc
            // bump, not a full copy) — the syms RSS lever.
            syms: SymbolTable::with_base(image.syms.clone()),
            sources: SourceMap::new(),
            top: image.top.clone(),
            macros: image.macros.clone(),
            #[cfg(feature = "bundled-scheme")]
            loaded_optin_libs: image.loaded_optin_libs.clone(),
            library_exports: image.library_exports.clone(),
            vm_env,
            pinned: Rc::new(RefCell::new(HashMap::new())),
            next_pin_id: Rc::new(Cell::new(1)),
            #[cfg(feature = "ffi-dynamic")]
            loaded_libs: Vec::new(),
            #[cfg(feature = "ffi-dynamic")]
            ffi_ctx: None,
            #[cfg(feature = "jit")]
            jit_lowerer: None,
            #[cfg(feature = "jit")]
            jit_poisoned: Rc::new(Cell::new(false)),
            command_line: None,
            typer_hints_by_lambda_id: RefCell::new(std::collections::HashMap::new()),
            sandbox_import_policy: None,
        }
    }

    /// Body of `Runtime::new` minus the post-construction stdlib
    /// registration step. Kept private so callers that want a
    /// minimal runtime without the `(crab …)` modules can build
    /// against `--no-default-features` and skip the stdlib hookup.
    fn new_inner() -> Self {
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

        // Default-on optimizer passes — always run during JIT compile,
        // regardless of the thread-local `active-optimizer-passes` list.
        //
        // - **`scalar-replace-cons`** (#28). Eliminates non-escaping
        //   cons allocations. Unconditionally sound — only removes
        //   pairs proven unobservable.
        //
        // - **`escape-to-region`** (#51). Promotes non-escaping conses
        //   to region allocation. The escape analysis is conservative:
        //   a cons is promoted only when every use is `car`/`cdr`/
        //   `pair?`/`null?` and it never reaches a `Return` / block-arg
        //   / call / env-store, so the rewrite is never unsafe — inside
        //   `(with-region …)` the pair bump-allocates; outside,
        //   `vm_alloc_pair_region_gc` falls back to the Rc heap so the
        //   rewrite is a no-op. Default-on per the post-1.0 perf plan
        //   (the agent A/B analysis). #51b/#51c (let-bound promotion +
        //   closure escape) remain deferred — see ADR 0034.
        //
        // Both passes are semantics-preserving and ship with unit +
        // e2e coverage. Users can still layer additional passes via
        // the `active-optimizer-passes` parameter or
        // `install-optimizer-pass!`.
        cs_opt::set_default_on_passes(&["scalar-replace-cons", "escape-to-region"]);

        // Gap B-3: wire cs-vm's region-resolver function-
        // pointer hook to cs-runtime's per-thread REGION_STACK
        // accessor. This lets `vm_alloc_pair_region_gc` (the
        // JIT/AOT entry point for region-allocated cons)
        // reach our region stack without a cs-vm → cs-runtime
        // dep cycle. Idempotent — overwrites the previous
        // resolver, which is fine since both calls return the
        // same function pointer.
        #[cfg(feature = "regions")]
        cs_vm::vm::register_region_resolver(regions::region_resolver_for_cs_vm);
        let mut syms = SymbolTable::new();
        let top = Frame::root();
        builtins::install_into(&top, &mut syms);
        let vm_env = cs_vm::vm::Env::root();
        for (name, f) in builtins::pure_builtins() {
            let sym = syms.intern(name);
            // cs-h5v: tag the small set of data-primitive builtins so
            // the VM's Call/TailCall dispatch loop can take an inline
            // fast path (skips arg-Vec materialization + the indirect
            // `f` call) instead of the generic builtin-call path. See
            // `cs_vm::vm::DataPrimOp` for why this is a VmBuiltin-level
            // tag rather than a bytecode-level opcode (the latter hid
            // these calls from an existing JIT peephole and regressed
            // tier-up).
            let value = match data_primop_for(name) {
                Some(op) => cs_vm::vm::make_vm_builtin_fast(name, f, op),
                None => cs_vm::vm::make_vm_builtin(name, f),
            };
            vm_env.define(sym, value);
        }
        for (name, f) in builtins::syms_builtins() {
            let sym = syms.intern(name);
            vm_env.define(sym, cs_vm::vm::make_vm_builtin_syms(name, f));
        }
        // Issue #48: bridge the higher-order SRFI-1/13 list/pair/string/
        // vector builtins onto the VM tier (take-while, filter-map,
        // pair-fold, …). The walker gets these via install_into; without
        // this the explicit --tier vm / --tier vm-jit env was missing them.
        builtins::install_vm_higher_order(&vm_env, &mut syms);
        // BEAM-style actor / table primops — same Syms shape, gated
        // on the `actor` feature. See crates/cs-runtime/src/builtins/
        // beam.rs and ADR 0013-equivalent (beam_runtime_spec.md).
        #[cfg(feature = "actor")]
        {
            for (name, f) in builtins::beam::beam_syms_builtins() {
                let sym = syms.intern(name);
                vm_env.define(sym, cs_vm::vm::make_vm_builtin_syms(name, f));
            }
            // Make the stdlib `(crab time)` `sleep-ms` cooperative inside an
            // activation handler by routing it through beam's coroutine yielder.
            // The hook is process-global + idempotent (OnceLock), so installing
            // it per Runtime is cheap and only the first wins. cs-runtime owns
            // this wiring because cs-stdlib-time can't depend on the actor layer
            // (same shape as cs_vm::install_yield_hook <- cs_actor::tokio_yield_hook).
            cs_stdlib_time::install_cooperative_sleep(builtins::beam::cooperative_sleep_hook);
            // Make `(crab net)` tcp-recv/tcp-send cooperative on the green path:
            // a coroutine driver parks the worker for the socket I/O instead of
            // blocking it. Same inverted-dependency wiring as the sleep hook.
            #[cfg(feature = "stdlib-net")]
            {
                cs_stdlib_net::install_async_recv(builtins::beam::cooperative_tcp_recv_hook);
                cs_stdlib_net::install_async_send(builtins::beam::cooperative_tcp_send_hook);
            }
        }
        // Cross-node transport primops (the `distrib` feature) — same Syms
        // shape, registered on the VM tier alongside the BEAM primops.
        #[cfg(feature = "distrib")]
        {
            for (name, f) in builtins::distrib::distrib_syms_builtins() {
                let sym = syms.intern(name);
                vm_env.define(sym, cs_vm::vm::make_vm_builtin_syms(name, f));
            }
        }
        // gRPC (h2c) server primops (the `grpc` feature) — same Syms
        // shape. MUST be on the VM tier because gRPC handler actors run
        // their body via `eval_str_via_vm` (the green/spawn-source path):
        // without this, `grpc-request-path` / `grpc-respond!` are
        // undefined inside the handler and the actor dies mid-call.
        #[cfg(feature = "grpc")]
        {
            for (name, f) in builtins::grpc::grpc_syms_builtins() {
                let sym = syms.intern(name);
                vm_env.define(sym, cs_vm::vm::make_vm_builtin_syms(name, f));
            }
            for (name, f) in builtins::etcdpb::etcdpb_syms_builtins() {
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
                        let _ = state.borrow_mut().close();
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
        // Gap B-2-lite: VM-tier registration for
        // `(with-region thunk)`. Mirrors the walker tier
        // registration in `higher_order_builtins`. Calls
        // the thunk inside a fresh RegionScope; allocations
        // made via `cons-in-region` / `make-vector-in-region`
        // / `make-string-in-region` inside the thunk live
        // in that region's bump arena and bulk-free on exit.
        #[cfg(feature = "regions")]
        {
            let wr_sym = syms.intern("with-region");
            vm_env.define(
                wr_sym,
                cs_vm::vm::make_vm_builtin_syms("with-region", |args, st| {
                    if args.len() != 1 {
                        return Err("with-region: 1 arg".into());
                    }
                    let region = std::rc::Rc::new(cs_gc::Region::new());
                    let _guard = regions::RegionScope::enter(std::rc::Rc::clone(&region));
                    let res = cs_vm::vm::vm_call_sync(&args[0], &[], st)
                        .map_err(|e| e.message.clone())?;
                    // Deep-clone into Rc-backed Value so
                    // parallel VM-side handles to region
                    // allocations don't dangle after region
                    // drop. See `to_rc_deep` in
                    // cs-core/src/promote.rs.
                    let safe = cs_core::promote::to_rc_deep(&res);
                    drop(res);
                    drop(_guard);
                    Ok(safe)
                }),
            );
        }
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
                Ok(cs_vm::vm::vm_current_output_port_value()
                    .unwrap_or_else(|| Value::Port(cs_core::Port::stdout())))
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
                        let head = p.car();
                        match &head {
                            Value::Pair(pair) => {
                                if pred(&pair.car(), key) {
                                    return Ok(head.clone());
                                }
                            }
                            _ => return Err("assoc: list of pairs".into()),
                        }
                        cur = p.cdr();
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
                        let head = p.car();
                        match &head {
                            Value::Pair(pair) => {
                                let car = pair.car();
                                let r = cs_vm::vm::vm_call_sync(cmp, &[car, key.clone()], st)
                                    .map_err(|e| format!("{:?}", e))?;
                                if r.is_truthy() {
                                    return Ok(head.clone());
                                }
                            }
                            _ => return Err("assoc: list of pairs".into()),
                        }
                        cur = p.cdr();
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
                        if pred(&p.car(), obj) {
                            return Ok(cur);
                        }
                        cur = p.cdr();
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
                        let car = p.car();
                        let r = cs_vm::vm::vm_call_sync(cmp, &[car, obj.clone()], st)
                            .map_err(|e| format!("{:?}", e))?;
                        if r.is_truthy() {
                            return Ok(cur);
                        }
                        cur = p.cdr();
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
                        // cs-i6p.2: this VM/actor-tier override
                        // never runs the cycle detector itself, but
                        // must still go through `set_value_at` (not
                        // a raw `items[i].1 = ...` write) so it
                        // clears any stale tombstone left on this
                        // slot by the walker-tier funnel instead of
                        // shadowing the fresh write behind it.
                        h.set_value_at(i, args[2].clone());
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
                        // cs-i6p.2: `value_at` transparently
                        // upgrades a weak value-cycle tombstone —
                        // a raw `items[i].1.clone()` read would see
                        // `Unspecified` for a slot the walker tier
                        // demoted, even though it's still live.
                        return Ok(h.value_at(i).unwrap_or(Value::Unspecified));
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
                        // cs-i6p.2: `swap_remove_item` fixes up any
                        // value tombstone that `Vec::swap_remove`'s
                        // move-last-into-`i` would otherwise
                        // misalign onto a different key's slot.
                        h.swap_remove_item(i);
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
                    nv @ (Value::Fixnum(_)
                    | Value::Flonum(_)
                    | Value::BigNumber(_)
                    | Value::Rational(_)) => {
                        let n = nv.as_number().unwrap();
                        n.to_f64() as i64
                    }
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
                    nv @ (Value::Fixnum(_)
                    | Value::Flonum(_)
                    | Value::BigNumber(_)
                    | Value::Rational(_)) => {
                        let n = nv.as_number().unwrap();
                        n.to_f64() as i64
                    }
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
        // Tail-safe continuation marks (issue #36): the VM tier reads
        // marks off its frame stack, so `current-continuation-marks`
        // is a VM-special proc (the walker uses a ctx-taking builtin).
        let ccm_sym = syms.intern("current-continuation-marks");
        vm_env.define(ccm_sym, cs_vm::vm::make_vm_current_continuation_marks());
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
                    nv @ (Value::Fixnum(_)
                    | Value::Flonum(_)
                    | Value::BigNumber(_)
                    | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        .unwrap_or_else(|| Value::Port(cs_core::Port::stdout()))
                } else {
                    args[1].clone()
                };
                match &port {
                    Value::Port(p) => match &**p {
                        cs_core::Port::StringOutput(buf) => {
                            buf.borrow_mut().push(c);
                            Ok(Value::Unspecified)
                        }
                        // #48: accept file output ports too (the VM tier's
                        // display/write already do via write_to_current_output).
                        cs_core::Port::FileOutput(state) => {
                            let mut st = state.borrow_mut();
                            let mut b = [0u8; 4];
                            st.write_bytes(c.encode_utf8(&mut b).as_bytes())
                                .map_err(|_| "write-char: port is closed".to_string())?;
                            Ok(Value::Unspecified)
                        }
                        cs_core::Port::Stdout => {
                            print!("{}", c);
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
                        .unwrap_or_else(|| Value::Port(cs_core::Port::stdout()))
                } else {
                    args[1].clone()
                };
                let start = if args.len() >= 3 {
                    match &args[2] {
                        nv @ (Value::Fixnum(_)
                        | Value::Flonum(_)
                        | Value::BigNumber(_)
                        | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        nv @ (Value::Fixnum(_)
                        | Value::Flonum(_)
                        | Value::BigNumber(_)
                        | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        // #48: accept file output ports too (the VM tier's
                        // display/write already do via write_to_current_output).
                        cs_core::Port::FileOutput(state) => {
                            let mut st = state.borrow_mut();
                            st.write_bytes(slice.as_bytes())
                                .map_err(|_| "write-string: port is closed".to_string())?;
                            Ok(Value::Unspecified)
                        }
                        cs_core::Port::Stdout => {
                            print!("{}", slice);
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
                    nv @ (Value::Fixnum(_)
                    | Value::Flonum(_)
                    | Value::BigNumber(_)
                    | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                    nv @ (Value::Fixnum(_)
                    | Value::Flonum(_)
                    | Value::BigNumber(_)
                    | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        nv @ (Value::Fixnum(_)
                        | Value::Flonum(_)
                        | Value::BigNumber(_)
                        | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        nv @ (Value::Fixnum(_)
                        | Value::Flonum(_)
                        | Value::BigNumber(_)
                        | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        nv @ (Value::Fixnum(_)
                        | Value::Flonum(_)
                        | Value::BigNumber(_)
                        | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
                        nv @ (Value::Fixnum(_)
                        | Value::Flonum(_)
                        | Value::BigNumber(_)
                        | Value::Rational(_)) => match nv.as_number().unwrap().to_f64() as i64 {
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
        // Pinned-value slab. Anything passed to `pin()` stays
        // reachable via this map's strong Value refs until its
        // Pinned guard drops and removes the entry.
        let pinned: Rc<RefCell<HashMap<PinId, Value>>> = Rc::new(RefCell::new(HashMap::new()));

        Self {
            syms,
            sources: SourceMap::new(),
            top,
            macros: std::collections::HashMap::new(),
            #[cfg(feature = "bundled-scheme")]
            loaded_optin_libs: std::collections::HashSet::new(),
            library_exports: std::collections::HashMap::new(),
            vm_env,
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
            #[cfg(feature = "jit")]
            jit_poisoned: Rc::new(Cell::new(false)),
            command_line: None,
            typer_hints_by_lambda_id: std::cell::RefCell::new(std::collections::HashMap::new()),
            sandbox_import_policy: None,
        }
    }

    /// Restrict `(environment ...)` calls to the given import specs.
    ///
    /// When set, any call to `(environment '(some library))` that names a
    /// library not in `imports` returns an error that explicitly identifies
    /// the disallowed library. Pass `None` to remove the restriction.
    ///
    /// Intended for the L1 sandbox wrapper: `SandboxRuntime` calls this
    /// before `eval_str` so the guest cannot widen the approved import set
    /// via nested `(eval ... (environment ...))` calls (ADR 0015 issue #15).
    pub fn set_sandbox_import_policy(&mut self, imports: Option<Vec<String>>) {
        self.sandbox_import_policy = imports;
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

    /// Register every compiled-in `(crab …)` stdlib module on this
    /// runtime. Each `cs-stdlib-<name>` crate exposes a `procs()`
    /// function returning `Vec<Arc<dyn HostProcedure>>`; this method
    /// iterates the enabled crates and registers each procedure.
    ///
    /// Per-module features (`stdlib-path`, `stdlib-fs`, …) toggle
    /// individual modules. When no `stdlib-X` feature is enabled
    /// the body compiles to a no-op. Called automatically from
    /// `Runtime::new`.
    pub fn register_stdlib(&mut self) {
        #[cfg(feature = "stdlib-path")]
        for p in cs_stdlib_path::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-fs")]
        for p in cs_stdlib_fs::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-os")]
        for p in cs_stdlib_os::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-process")]
        for p in cs_stdlib_process::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-string")]
        for p in cs_stdlib_string::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-format")]
        for p in cs_stdlib_format::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-regex")]
        for p in cs_stdlib_regex::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-time")]
        for p in cs_stdlib_time::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-random")]
        for p in cs_stdlib_random::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-uuid")]
        for p in cs_stdlib_uuid::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-json")]
        for p in cs_stdlib_json::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-csv")]
        for p in cs_stdlib_csv::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-toml")]
        for p in cs_stdlib_toml::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-base")]
        for p in cs_stdlib_base::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-url")]
        for p in cs_stdlib_url::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-hash")]
        for p in cs_stdlib_hash::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-compress")]
        for p in cs_stdlib_compress::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-deflate")]
        for p in cs_stdlib_deflate::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-archive")]
        for p in cs_stdlib_archive::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-log")]
        for p in cs_stdlib_log::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-metrics")]
        for p in cs_stdlib_metrics::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-net")]
        for p in cs_stdlib_net::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-http")]
        for p in cs_stdlib_http::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-websocket")]
        for p in cs_stdlib_websocket::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-collection")]
        for p in cs_stdlib_collection::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-math")]
        for p in cs_stdlib_math::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-tty")]
        for p in cs_stdlib_tty::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-signal")]
        for p in cs_stdlib_signal::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-cli")]
        for p in cs_stdlib_cli::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-crypto")]
        for p in cs_stdlib_crypto::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-sql")]
        for p in cs_stdlib_sql::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-xml")]
        for p in cs_stdlib_xml::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-binary")]
        for p in cs_stdlib_binary::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-template")]
        for p in cs_stdlib_template::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-ini")]
        for p in cs_stdlib_ini::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-yaml")]
        for p in cs_stdlib_yaml::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-tls")]
        for p in cs_stdlib_tls::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-store")]
        for p in cs_store::procs() {
            self.register_host_procedure(p);
        }
        #[cfg(feature = "stdlib-meta")]
        for p in cs_stdlib_meta::procs() {
            self.register_host_procedure(p);
        }
        // Bundled Scheme libraries — these are defined in Scheme rather
        // than Rust because they take procedure arguments (compose,
        // memoize, group-by, deftest, update-in), which a host procedure
        // cannot call back into; pprint just reuses the reader/writer.
        // Their defines are evaluated into the global environment here,
        // so `(import (crab …))` is a no-op exactly like the Rust
        // modules. Each `stdlib-<lib>` feature pulls in `bundled-scheme`.
        #[cfg(feature = "bundled-scheme")]
        self.register_bundled_libraries();
    }

    /// Evaluate the enabled bundled Scheme libraries into the global
    /// environment. `functional` loads first by convention; the others
    /// are independent.
    #[cfg(feature = "bundled-scheme")]
    fn register_bundled_libraries(&mut self) {
        #[cfg(feature = "stdlib-functional")]
        self.load_bundled_library("(crab functional)", include_str!("scheme/functional.scm"));
        #[cfg(feature = "stdlib-iter")]
        self.load_bundled_library("(crab iter)", include_str!("scheme/iter.scm"));
        #[cfg(feature = "stdlib-test")]
        self.load_bundled_library("(crab test)", include_str!("scheme/test.scm"));
        #[cfg(feature = "stdlib-pprint")]
        self.load_bundled_library("(crab pprint)", include_str!("scheme/pprint.scm"));
        #[cfg(feature = "stdlib-dict")]
        self.load_bundled_library("(crab dict)", include_str!("scheme/dict.scm"));
        #[cfg(feature = "stdlib-walk")]
        self.load_bundled_library("(crab walk)", include_str!("scheme/walk.scm"));
        #[cfg(feature = "stdlib-sync")]
        self.load_bundled_library("(crab sync)", include_str!("scheme/sync.scm"));
        // Testing toolkit (expect/mock/prop/spec) is NOT auto-loaded: its
        // DSL macros (`describe`/`it`/`expect`/…) use common identifiers
        // that would shadow user/test bindings in every Runtime (PR #105 —
        // a global `describe` macro broke a `define/contract describe`
        // test). It loads on demand instead — see `load_optin_libs_for`,
        // run when a program does `(import (crab expect|mock|prop|spec))`.
        // Scheme extension of the Rust `(crab math)` module (combinatorics
        // + numeric helpers).
        #[cfg(feature = "stdlib-math")]
        self.load_bundled_library("(crab math)", include_str!("scheme/math.scm"));
        // Synchronous actor RPC (`call`). Gated on `actor` (not a
        // stdlib-* flag) because it is built on the actor primitives
        // send/raw-receive/self, which only exist with that feature.
        #[cfg(feature = "actor")]
        self.load_bundled_library("(crab actor)", include_str!("scheme/actor.scm"));
    }

    /// Evaluate one bundled library's source into the global env. A
    /// failure means a shipped library is malformed — a build bug, not
    /// a user error — so it panics loudly rather than failing silently.
    #[cfg(feature = "bundled-scheme")]
    fn load_bundled_library(&mut self, name: &str, src: &str) {
        // Load into BOTH tiers' global envs. The walker `top` and the VM
        // `vm_env` are separate (Rust builtins are registered into both, but a
        // Scheme `eval_str` only populates the walker), so a VM-tier program
        // (e.g. a spawn-source actor body, which now runs on the VM tier)
        // cannot see a library loaded only on the walker — that surfaced as
        // `undefined variable: call` from the `(crab actor)` prelude.
        if let Err(d) = self.eval_str(name, src) {
            panic!(
                "crabscheme: bundled library {} failed to load (walker): {}",
                name, d.message
            );
        }
        if let Err(d) = self.eval_str_via_vm(name, src) {
            panic!(
                "crabscheme: bundled library {} failed to load (vm): {}",
                name, d.message
            );
        }
    }

    /// Load any opt-in testing-toolkit libraries a freshly-parsed program
    /// imports via `(import (crab expect|mock|prop|spec))`, with their
    /// dependencies, into the global env — once each. Must run BEFORE the
    /// program is expanded so the toolkit's macros are visible to it.
    ///
    /// Unlike the always-on bundled libs, these are loaded only on
    /// explicit import: their DSL macros (`describe`/`it`/`expect`/…) use
    /// common identifiers that would otherwise shadow user/test bindings
    /// in every Runtime (PR #105). The `(import …)` form itself stays an
    /// expander no-op; this pre-pass is what actually makes them available.
    #[cfg(feature = "bundled-scheme")]
    fn load_optin_libs_for(&mut self, data: &[Datum]) {
        let mut wanted: std::collections::BTreeSet<&'static str> =
            std::collections::BTreeSet::new();
        for form in data {
            let Some(items) = Self::datum_proper_list(form) else {
                continue;
            };
            let Some(head) = items.first() else { continue };
            if Self::datum_symbol_name(head, &self.syms) != Some("import") {
                continue;
            }
            for spec in &items[1..] {
                let Some(parts) = Self::datum_symbol_list(spec, &self.syms) else {
                    continue;
                };
                if parts.len() == 2 && parts[0] == "crab" {
                    match parts[1].as_str() {
                        "expect" => {
                            wanted.insert("expect");
                        }
                        "mock" => {
                            wanted.insert("mock");
                        }
                        // prop + spec report failures through expect's
                        // matcher printer, so they depend on (crab expect).
                        "prop" => {
                            wanted.insert("prop");
                            wanted.insert("expect");
                        }
                        "spec" => {
                            wanted.insert("spec");
                            wanted.insert("expect");
                        }
                        _ => {}
                    }
                }
            }
        }
        // Load in dependency order, each at most once.
        for lib in ["expect", "mock", "prop", "spec"] {
            if !wanted.contains(lib) || self.loaded_optin_libs.contains(lib) {
                continue;
            }
            let src: Option<&str> = match lib {
                #[cfg(feature = "stdlib-expect")]
                "expect" => Some(include_str!("scheme/expect.scm")),
                #[cfg(feature = "stdlib-mock")]
                "mock" => Some(include_str!("scheme/mock.scm")),
                #[cfg(feature = "stdlib-prop")]
                "prop" => Some(include_str!("scheme/prop.scm")),
                #[cfg(feature = "stdlib-spec")]
                "spec" => Some(include_str!("scheme/spec.scm")),
                _ => None,
            };
            if let Some(src) = src {
                self.loaded_optin_libs.insert(lib.to_string());
                self.load_bundled_library(&format!("(crab {lib})"), src);
            }
        }
    }

    /// Elements of a proper-list `Datum` (Pair-chain ending in Null);
    /// None for an improper list or a non-list.
    #[cfg(feature = "bundled-scheme")]
    fn datum_proper_list(d: &Datum) -> Option<Vec<&Datum>> {
        let mut out = Vec::new();
        let mut cur = d;
        loop {
            match cur {
                Datum::Null(_) => return Some(out),
                Datum::Pair(car, cdr, _) => {
                    out.push(car.as_ref());
                    cur = cdr.as_ref();
                }
                _ => return None,
            }
        }
    }

    /// The printable name of a `Datum::Symbol`, else None.
    #[cfg(feature = "bundled-scheme")]
    fn datum_symbol_name<'a>(d: &Datum, syms: &'a SymbolTable) -> Option<&'a str> {
        match d {
            Datum::Symbol(s, _) => Some(syms.name(*s)),
            _ => None,
        }
    }

    /// A proper list of symbols → their printable names; else None.
    #[cfg(feature = "bundled-scheme")]
    fn datum_symbol_list(d: &Datum, syms: &SymbolTable) -> Option<Vec<String>> {
        Self::datum_proper_list(d)?
            .iter()
            .map(|it| Self::datum_symbol_name(it, syms).map(|s| s.to_string()))
            .collect()
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

    /// No-op shim preserved for callers that still invoke it.
    /// Under countable-memory there is no heap to collect —
    /// reclamation happens deterministically at `Rc::drop`.
    /// Cycles are reclaimed by the synchronous cycle detector
    /// (layer 2) and the optional layer-4 sweep registry.
    pub fn collect(&self) {}

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
    ///
    /// A leading `#!lang NAME` (or `#lang NAME`) header triggers the
    /// custom-reader protocol (R6RS++ Phase 4, issue #10): the host
    /// loads `(lang NAME)`, and if it exports a `reader` procedure
    /// the body is routed through that proc (yielding a list of
    /// datums) rather than the host reader. When no `reader` is
    /// exported, behaviour falls back to the Phase 3C MVP — the
    /// header is rewritten in place to `(import (lang NAME))` so
    /// line numbers stay aligned for diagnostic spans.
    pub fn eval_str(&mut self, name: &str, src: &str) -> Result<Value, Diagnostic> {
        if let Some((lang_name, body)) = lang_reader::parse_lang_header(src) {
            return self.eval_with_lang_header(name, src, lang_name, body);
        }
        let file_id = self.sources.add(name, src);
        self.with_active(|rt| rt.eval_str_in_file(file_id, src))
    }

    /// Custom-reader pipeline for a source file that begins with a
    /// `#!lang NAME` header. Issue #10 — see [`eval_str`].
    ///
    /// 1. If `(lang NAME)` has been declared (its name appears in
    ///    [`Self::library_exports`]) AND that declaration listed
    ///    `reader` in its `(export ...)` clause, look up the
    ///    `reader` binding and route the body through it: call
    ///    the procedure on the body source (as a Scheme string),
    ///    convert the returned proper list to a datum sequence,
    ///    expand+eval those datums.
    /// 2. Otherwise fall back to the Phase 3C MVP — rewrite the
    ///    header line to whitespace of the same byte length (so
    ///    diagnostic spans stay aligned) and run the body through
    ///    the host reader against the runtime's current top env.
    ///
    /// The declared-export check (rather than a bare
    /// `lookup("reader")`) keeps the protocol robust against a
    /// pre-existing user-defined top-level `reader`: only a lang
    /// that explicitly opts in via `(export reader ...)` activates
    /// the parse-time hook.
    fn eval_with_lang_header(
        &mut self,
        name: &str,
        original_src: &str,
        lang_name: &str,
        body: &str,
    ) -> Result<Value, Diagnostic> {
        // Resolve the optional `base-env` (issue #70) once; it
        // applies whether or not a `reader` / `expander` is also
        // exported. An error here means the lang exported a
        // `base-env` that isn't a valid environment value —
        // surface that as a hard error rather than silently
        // falling back.
        let base_env = self.resolve_lang_base_env(lang_name)?;

        // Phase 1 — does the lang declare `(export reader ...)`
        // or `(export expander ...)`? The `library_exports`
        // mirror is populated as a side effect of
        // `eval_data_in_file` whenever a `(library ...)` form
        // runs; libraries declared earlier in the same Runtime
        // session are visible here.
        let opts_into_reader = self.lang_exports_reader(lang_name);
        let opts_into_expander = self.lang_exports_symbol(lang_name, "expander");

        // No custom-reader and no user-expander → the Phase 3C
        // MVP fast path. Pad the header line with whitespace of
        // the same byte length so the body parses with the host
        // reader at its original line / column positions. Honors
        // an exported `base-env` if present (issue #70).
        if !opts_into_reader && !opts_into_expander {
            let header_len = original_src.len() - body.len();
            let mut rewritten = String::with_capacity(original_src.len());
            rewritten.extend(std::iter::repeat_n(' ', header_len));
            rewritten.push_str(body);
            let file_id = self.sources.add(name, &rewritten);
            return self.with_active(|rt| rt.eval_str_in_env(file_id, &rewritten, base_env));
        }

        // At least one of `reader` / `expander` is declared. Run
        // the appropriate read-phase, then optionally pipe the
        // resulting datums through the lang's expander before
        // handing them to the host expander.
        let body_file = self.sources.add(name, body);
        let synth_span = Span::new(body_file, 0, 0);

        let mut datums = if opts_into_reader {
            match self
                .lookup("reader")
                .filter(|v| matches!(v, Value::Procedure(_)))
            {
                Some(reader_proc) => {
                    self.invoke_lang_reader(lang_name, body, &reader_proc, synth_span)?
                }
                None => {
                    // Declared `reader` but no binding (library
                    // failed to define it). Degrade gracefully to
                    // the host reader so the body still parses.
                    self.read_body_host(body_file, body)?
                }
            }
        } else {
            // Only `expander` is declared — host reader produces
            // the datums; the lang's expander rewrites them.
            self.read_body_host(body_file, body)?
        };

        if opts_into_expander {
            if let Some(expander_proc) = self
                .lookup("expander")
                .filter(|v| matches!(v, Value::Procedure(_)))
            {
                datums =
                    self.invoke_lang_expander(lang_name, datums, &expander_proc, synth_span)?;
            }
            // Declared `expander` but no binding → silent no-op,
            // same degradation policy as the reader case.
        }

        // Final eval of the datum stream produced by the
        // reader+expander pipeline. Honors an exported `base-env`
        // (issue #70) by routing through `eval_data_in_env`
        // rather than `eval_data_in_file`.
        self.with_active(|rt| rt.eval_data_in_env(datums, base_env))
    }

    /// Resolve the `base-env` export of `(lang NAME)`, if any.
    /// Returns:
    /// - `Ok(None)` — lang didn't export `base-env`, or didn't
    ///   declare a value for the binding.
    /// - `Ok(Some(frame))` — lang exported a valid environment
    ///   value; `frame` is the [`Frame`] to use as the body's
    ///   evaluation root (immutable or mutable per the env's
    ///   `make-namespace` vs `environment` construction).
    /// - `Err(diag)` — lang exported `base-env` but the value
    ///   isn't a valid environment record.
    fn resolve_lang_base_env(&self, lang_name: &str) -> Result<Option<Rc<Frame>>, Diagnostic> {
        if !self.lang_exports_symbol(lang_name, "base-env") {
            return Ok(None);
        }
        let Some(env_val) = self.lookup("base-env") else {
            return Ok(None);
        };
        let Some((bindings, mutable)) = crate::builtins::decode_environment(&env_val) else {
            return Err(Diagnostic::error(
                format!(
                    "`base-env` for (lang {}) must be an environment \
                     (built with `environment` or `make-namespace`)",
                    lang_name
                ),
                cs_diag::Span::DUMMY,
            ));
        };
        let frame = if mutable {
            Frame::mutable_root(bindings)
        } else {
            Frame::immutable_root(bindings)
        };
        Ok(Some(frame))
    }

    /// True if a library named `(lang NAME)` has been declared in
    /// this Runtime and its `(export ...)` clause lists `reader`.
    fn lang_exports_reader(&self, lang_name: &str) -> bool {
        self.lang_exports_symbol(lang_name, "reader")
    }

    /// Generic export-list query: did `(lang NAME)` declare
    /// `(export EXPORT_NAME ...)`? Returns false when any of the
    /// involved names haven't been interned yet (a sufficient
    /// proxy for "the lang library can't have referenced
    /// EXPORT_NAME because no one has"). Used by the reader (#10),
    /// `base-env` (#70), and expander (#71) export checks.
    fn lang_exports_symbol(&self, lang_name: &str, export_name: &str) -> bool {
        let Some(lang_sym) = self.syms.by_name_lookup("lang") else {
            return false;
        };
        let Some(name_sym) = self.syms.by_name_lookup(lang_name) else {
            return false;
        };
        let Some(target_sym) = self.syms.by_name_lookup(export_name) else {
            return false;
        };
        let key = vec![lang_sym, name_sym];
        self.library_exports
            .get(&key)
            .is_some_and(|exports| exports.contains(&target_sym))
    }

    /// Read the body source through the host reader. Used by the
    /// `#!lang` pipeline when the lang doesn't (or can't) supply
    /// its own reader. `body_file` is the [`FileId`] the body's
    /// source was registered under.
    fn read_body_host(&mut self, body_file: FileId, body: &str) -> Result<Vec<Datum>, Diagnostic> {
        read_all(body_file, body, &mut self.syms).map_err(|errs| {
            let e = &errs[0];
            Diagnostic::error(e.message(), e.span())
        })
    }

    /// Invoke the lang's `reader` procedure on `body`. Returns
    /// the converted [`Datum`] list. Diagnostics name the offending
    /// lang and the failure mode (raise vs non-datum return).
    fn invoke_lang_reader(
        &mut self,
        lang_name: &str,
        body: &str,
        reader_proc: &Value,
        synth_span: Span,
    ) -> Result<Vec<Datum>, Diagnostic> {
        let body_val = Value::string(body.to_string());
        let result_val = self.with_active(|rt| {
            // The reader itself runs against the runtime's normal
            // top env (not `base-env`) — the reader is *meta-level*
            // code authored by the lang library, so it needs access
            // to whatever utilities it imported when the library
            // was declared. `base-env` restricts the *body*, not
            // the reader. The reader was defined by a user
            // (walker-tier `Closure`) or by a host builtin —
            // `apply_procedure` handles every procedure variant
            // the runtime mints; `vm_call_sync` only dispatches
            // VM-tier shapes and would refuse walker closures.
            let mut ctx = EvalCtx::new(rt.top.clone(), &mut rt.syms, &mut rt.macros);
            ctx.sandbox_allowed_imports = rt.sandbox_import_policy.clone();
            crate::eval::apply_procedure(reader_proc, &[body_val], &mut ctx)
        });
        let result_val = result_val.map_err(|e| {
            Diagnostic::error(
                format!("reader for (lang {}) raised: {}", lang_name, e.message()),
                synth_span,
            )
        })?;
        lang_reader::value_to_datum_list(&result_val, synth_span).map_err(|msg| {
            Diagnostic::error(
                format!(
                    "reader for (lang {}) returned non-datum: {}",
                    lang_name, msg
                ),
                synth_span,
            )
        })
    }

    /// Invoke the lang's `expander` procedure on a datum sequence
    /// (issue #71). The expander is a Scheme procedure of one
    /// argument — a list of datums — that returns a list of
    /// datums. This is option (2) from the #71 issue body: the
    /// host expander still runs afterwards, so the user expander
    /// is effectively a datum→datum macro pass with the full
    /// runtime available.
    fn invoke_lang_expander(
        &mut self,
        lang_name: &str,
        datums: Vec<Datum>,
        expander_proc: &Value,
        synth_span: Span,
    ) -> Result<Vec<Datum>, Diagnostic> {
        let input_val = lang_reader::datum_list_to_value(&datums);
        let result_val = self.with_active(|rt| {
            let mut ctx = EvalCtx::new(rt.top.clone(), &mut rt.syms, &mut rt.macros);
            ctx.sandbox_allowed_imports = rt.sandbox_import_policy.clone();
            crate::eval::apply_procedure(expander_proc, &[input_val], &mut ctx)
        });
        let result_val = result_val.map_err(|e| {
            Diagnostic::error(
                format!("expander for (lang {}) raised: {}", lang_name, e.message()),
                synth_span,
            )
        })?;
        lang_reader::value_to_datum_list(&result_val, synth_span).map_err(|msg| {
            Diagnostic::error(
                format!(
                    "expander for (lang {}) returned non-datum: {}",
                    lang_name, msg
                ),
                synth_span,
            )
        })
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

    /// Variant of [`Self::eval_str_in_file`] that honors an
    /// optional `base_env` — see [`Self::eval_data_in_env`]
    /// (issue #70 — `#!lang` `base-env` export). When
    /// `base_env` is `None`, behaviour is identical to
    /// `eval_str_in_file`.
    fn eval_str_in_env(
        &mut self,
        file_id: FileId,
        src: &str,
        base_env: Option<Rc<Frame>>,
    ) -> Result<Value, Diagnostic> {
        let data = match read_all(file_id, src, &mut self.syms) {
            Ok(d) => d,
            Err(errs) => {
                let e = &errs[0];
                return Err(Diagnostic::error(e.message(), e.span()));
            }
        };
        self.eval_data_in_env(data, base_env)
    }

    fn eval_str_in_file(&mut self, file_id: FileId, src: &str) -> Result<Value, Diagnostic> {
        let data = match read_all(file_id, src, &mut self.syms) {
            Ok(d) => d,
            Err(errs) => {
                let e = &errs[0];
                return Err(Diagnostic::error(e.message(), e.span()));
            }
        };
        self.eval_data_in_file(data)
    }

    /// Expand and evaluate a pre-parsed datum sequence against the
    /// runtime's default top env. Splits the post-`read_all`
    /// portion of [`Self::eval_str_in_file`] out so the `#!lang`
    /// custom-reader path (issue #10) can hand the expander a
    /// datum list produced by a Scheme `reader` procedure instead
    /// of host-parsed source.
    fn eval_data_in_file(&mut self, data: Vec<Datum>) -> Result<Value, Diagnostic> {
        self.eval_data_in_env(data, None)
    }

    /// Expand and evaluate a pre-parsed datum sequence against an
    /// optional restricted environment. `base_env: Some(frame)`
    /// activates the `#!lang` `base-env` export (issue #70) — the
    /// body evaluates with `frame` as its root chain link instead
    /// of `self.top`. `None` keeps the default behaviour.
    fn eval_data_in_env(
        &mut self,
        data: Vec<Datum>,
        base_env: Option<Rc<Frame>>,
    ) -> Result<Value, Diagnostic> {
        // Load any opt-in testing-toolkit libs this program imports
        // (e.g. `(import (crab spec))`) before expansion, so their macros
        // are visible. No-op for programs that don't import them.
        #[cfg(feature = "bundled-scheme")]
        self.load_optin_libs_for(&data);
        // Issue #11 ext-1 + ext-2: cs-typer pre-passes.
        //
        // * `extract_annotations` strips `(: NAME T)` ascriptions and
        //   typed-define brackets from the Datum stream so the
        //   expander never sees them (and `:` doesn't fail as an
        //   unbound reference). The returned table is used as
        //   fallback by the auto-contract pass below.
        //
        // * `auto_contract_library_exports` finds typed library
        //   exports and injects a `(set! NAME (apply-contract ...))`
        //   wrap after each define so untyped callers hit a clear
        //   `&contract-violation` on misuse — without the user
        //   having to write `define/typed` explicitly.
        //
        // Both passes are cheap no-ops for untyped code.
        //
        // Typer diagnostics (e.g. `unknown type: ->*` because the
        // typer's annotation grammar is narrower than the contract
        // macro's) are dropped on the runtime path — the macro
        // expander handles every form the runtime needs to accept,
        // and `crabscheme check` is where typer feedback surfaces.
        // Without this drop, valid `define/typed` programs whose
        // type uses a contract-only constructor (e.g. `->*`) would
        // fail at eval purely because the typer can't represent it.
        let (data, annotation_table, _ann_diags) =
            cs_typer::extract_annotations(&data, &mut self.syms);
        let data = cs_typer::auto_contract_library_exports(data, &annotation_table, &mut self.syms);

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
        // Mirror this call's library declarations into the
        // Runtime so the `#!lang` custom-reader pipeline (issue
        // #10) can consult exports declared in earlier `eval_str`
        // calls. The per-call expander is dropped below; without
        // mirroring, that knowledge would vanish.
        let lib_updates: Vec<(Vec<cs_core::Symbol>, Vec<cs_core::Symbol>)> = expander
            .libraries()
            .iter()
            .map(|(k, v)| (k.clone(), v.exports.clone()))
            .collect();
        drop(expander);
        drop(resolver);
        for (name, exports) in lib_updates {
            self.library_exports.insert(name, exports);
        }
        let eval_env = base_env.unwrap_or_else(|| self.top.clone());
        let mut ctx = EvalCtx::new(eval_env.clone(), &mut self.syms, &mut self.macros);
        ctx.sandbox_allowed_imports = self.sandbox_import_policy.clone();
        let result = eval(&core, eval_env, &mut ctx);
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

    /// Like [`eval_str_via_vm`], but **caches the compiled bytecode per source**
    /// (per worker thread). The first call for a given `src` compiles it and
    /// caches the `Bytecode` + the symbol table it produced; later calls (e.g.
    /// every green actor running the *same* body) skip parse/expand/compile and
    /// re-run the cached bytecode against this runtime's `vm_env` — the body's
    /// closures share the cached code chunks, so only the closures + their
    /// bindings are per-actor.
    ///
    /// **Sound only when callers share the same base env** (the shared-Runtime
    /// model — `Runtime::from_image`): the cached bytecode's builtin ids resolve
    /// against the shared base, and adopting the cached symbol table as our base
    /// makes the body-symbol ids resolve too. The body's top-level `(define …)`s
    /// still land in *this* runtime's per-actor overlay (the define boundary), so
    /// per-actor isolation is preserved. Intended for `green_source_body`.
    pub(crate) fn eval_str_via_vm_cached(
        &mut self,
        name: &str,
        src: &str,
    ) -> Result<Value, Diagnostic> {
        // source -> (compiled bytecode, the symbol table that compile produced).
        type BodyCache = HashMap<String, (Rc<cs_vm::Bytecode>, Rc<cs_core::SymbolTable>)>;
        thread_local! {
            static BODY_CACHE: RefCell<BodyCache> = RefCell::new(HashMap::new());
        }
        if let Some((bc, cached_syms)) = BODY_CACHE.with(|c| c.borrow().get(src).cloned()) {
            // Adopt the cached body symbols so the bytecode's body-symbol ids
            // resolve; defines still land in our own overlay env.
            self.syms = cs_core::SymbolTable::with_base(cached_syms);
            return self.with_active(|rt| rt.run_bytecode(&bc));
        }
        let file_id = self.sources.add(name, src);
        // Compile against the (empty-overlay) per-actor env: the globals snapshot
        // folds only the shared base builtins, valid for every reusing actor.
        let bc = Rc::new(self.with_active(|rt| rt.compile_program_via_vm(file_id, src))?);
        BODY_CACHE.with(|c| {
            c.borrow_mut()
                .insert(src.to_string(), (bc.clone(), Rc::new(self.syms.clone())));
        });
        self.with_active(|rt| rt.run_bytecode(&bc))
    }

    fn eval_str_via_vm_inner(&mut self, file_id: FileId, src: &str) -> Result<Value, Diagnostic> {
        let bc = self.compile_program_via_vm(file_id, src)?;
        self.run_bytecode(&bc)
    }

    /// Parse + expand + compile `src` to reusable VM [`Bytecode`] **without
    /// running it** — so the bytecode can be cached and re-run against many
    /// per-actor environments (shared body compilation; [`cs_vm::run`] borrows
    /// it). Globals const-folding uses this runtime's `vm_env` snapshot, so cached
    /// reuse is sound only when the reusing runtimes share the same base env (the
    /// shared-Runtime model — the base builtins are identical and the per-actor
    /// overlay is empty at compile time).
    fn compile_program_via_vm(
        &mut self,
        file_id: FileId,
        src: &str,
    ) -> Result<cs_vm::Bytecode, Diagnostic> {
        let data = match read_all(file_id, src, &mut self.syms) {
            Ok(d) => d,
            Err(errs) => {
                let e = &errs[0];
                return Err(Diagnostic::error(e.message(), e.span()));
            }
        };
        // Opt-in testing-toolkit libs (see eval_data_in_env) — must load
        // before expansion so their macros are visible to this program.
        #[cfg(feature = "bundled-scheme")]
        self.load_optin_libs_for(&data);
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
        cs_vm::compile_with_globals_and_primops(&core, &globals, &primops)
            .map_err(|e| Diagnostic::error(e.message, e.span))
    }

    /// Run already-compiled [`Bytecode`] against this runtime's `vm_env`,
    /// rendering VM errors. Shared by [`eval_str_via_vm_inner`] and the
    /// shared-body-compilation cache (which runs the *same* cached bytecode
    /// against each actor's per-actor overlay env — the body's closures share the
    /// bytecode's code chunks, only the closures + bindings are per-actor).
    fn run_bytecode(&mut self, bc: &cs_vm::Bytecode) -> Result<Value, Diagnostic> {
        // Install the `eval` hook + root env so VmEval can call back into us.
        let prev_hook = cs_vm::vm::install_eval_hook(Some(vm_eval_callback));
        let prev_env = cs_vm::vm::install_eval_root_env(Some(self.vm_env.clone()));
        let result = cs_vm::run(bc, self.vm_env.clone(), &mut self.syms);
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

    /// Look up a top-level binding. Tier-agnostic: checks the walker `top`
    /// frame first, then the VM `vm_env`. A binding `define`d via
    /// `eval_str_via_vm` lands in `vm_env`, so a walker-only lookup would
    /// spuriously miss it (e.g. an actor body loaded on the VM tier).
    pub fn lookup(&self, name: &str) -> Option<Value> {
        let sym = self.syms.by_name_lookup(name)?;
        self.top.get(sym).or_else(|| self.vm_env.get(sym))
    }
}

/// Render a raised condition value as a human-friendly error message.
/// Shape produced by `error` / `make-condition` / R6RS condition constructors:
/// a vector `#("&compound-condition" simple1 simple2 ...)` where each simple
/// is `#("&<type>" field0 field1 ...)`.
///
/// The output reads like:
///   error in <who>: <message> (<irritant ...>) [<other tags>]
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
                out.push(p.car());
                cur = p.cdr();
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

/// Maps a `pure_builtins()` name to its [`cs_vm::vm::DataPrimOp`]
/// identity, for the VM builtin-registration loop (cs-h5v). Only the
/// exact names below get tagged; every other builtin registers as a
/// plain (untagged) `VmBuiltin`.
fn data_primop_for(name: &str) -> Option<cs_vm::vm::DataPrimOp> {
    use cs_vm::vm::DataPrimOp;
    match name {
        "car" => Some(DataPrimOp::Car),
        "cdr" => Some(DataPrimOp::Cdr),
        "cons" => Some(DataPrimOp::Cons),
        "null?" => Some(DataPrimOp::NullP),
        "pair?" => Some(DataPrimOp::PairP),
        "not" => Some(DataPrimOp::Not),
        "eq?" => Some(DataPrimOp::EqP),
        "vector-ref" => Some(DataPrimOp::VectorRef),
        "vector-set!" => Some(DataPrimOp::VectorSet),
        _ => None,
    }
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

// ---------------------------------------------------------------------
// AOT generic-builtin dispatch (feat/aot-ready).
//
// The cs-aot backend open-codes a handful of builtins (arithmetic via
// primops; cons/car/list/vector/string-const via direct cs-vm helpers).
// Everything else in the stdlib — `display`, `string-append`, `assoc`,
// `for-each`, … — is lowered to a call to `aot_call_builtin`, which
// dispatches through a real VM-tier builtin environment embedded in the
// AOT'd binary (the binary links cs-runtime for exactly this).
// ---------------------------------------------------------------------

thread_local! {
    /// Per-thread runtime backing `aot_call_builtin`, built on first use.
    static AOT_RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

impl Runtime {
    /// Resolve `name` to a builtin in the walker top-level env, call it with
    /// the borrow-decoded NB `args`, and re-encode the result as an NB
    /// carrier.
    ///
    /// Dispatch goes through the **walker** `top` env + `apply_procedure`,
    /// not `vm_env` + `vm_call_sync`: `top` (via `install_into`) holds the
    /// *complete* builtin set including the higher-order ones (`display`,
    /// `map`, …) that the VM tier registers as markers `vm_call_sync` can't
    /// invoke. `apply_procedure` handles every builtin flavor (Pure / Higher
    /// / Syms). This is the walker tier, so builtin-heavy AOT code pays
    /// walker speed — fine for breadth/correctness; numeric kernels stay on
    /// the inline-NB fast paths and never reach here.
    fn aot_dispatch_builtin(&mut self, name: &str, args: &[i64]) -> i64 {
        let sym = self.syms.intern(name);
        let Some(proc) = self.top.get(sym) else {
            eprintln!("crabscheme (aot): unbound builtin `{name}`");
            return cs_vm::vm::vm_value_to_nb(Value::Unspecified);
        };
        self.aot_dispatch_builtin_with(proc, name, args)
    }

    /// cs-7rz — same dispatch as [`Self::aot_dispatch_builtin`], but the
    /// `name` → `Value` resolution comes from `cache` instead of a fresh
    /// `SymbolTable::intern` + top-level env lookup.
    ///
    /// `cache` holds this call site's already-resolved builtin `Value`
    /// once it's been looked up on this thread. This is sound because
    /// `AOT_RUNTIME`'s top-level env is populated once at `Runtime::new()`
    /// and never subsequently mutated by AOT'd user code (it exists
    /// purely as a name→builtin resolution table, not the AOT program's
    /// own top-level scope — user-level defines are lowered to native
    /// Rust functions, not routed through this env). A given call site
    /// always resolves the same literal name, so the first resolution is
    /// valid for the process's remaining lifetime on this thread.
    fn aot_dispatch_builtin_cached(
        &mut self,
        cache: &AotBuiltinCache,
        name: &str,
        args: &[i64],
    ) -> i64 {
        let cached = cache.0.borrow().clone();
        let proc = match cached {
            Some(v) => v,
            None => {
                let sym = self.syms.intern(name);
                let Some(v) = self.top.get(sym) else {
                    eprintln!("crabscheme (aot): unbound builtin `{name}`");
                    return cs_vm::vm::vm_value_to_nb(Value::Unspecified);
                };
                *cache.0.borrow_mut() = Some(v.clone());
                v
            }
        };
        self.aot_dispatch_builtin_with(proc, name, args)
    }

    /// Shared invocation tail for [`Self::aot_dispatch_builtin`] and
    /// [`Self::aot_dispatch_builtin_cached`] once `proc` is resolved.
    ///
    /// cs-7rz — args up to `INLINE_ARGS` decode into a stack array
    /// instead of a heap `Vec`; most stdlib builtins (`not`, `car`,
    /// `+`, …) take 1-2 args, so this turns a per-call heap
    /// allocation into a per-call-site cost of zero.
    fn aot_dispatch_builtin_with(&mut self, proc: Value, name: &str, args: &[i64]) -> i64 {
        const INLINE_ARGS: usize = 4;
        let mut ctx = EvalCtx::new(self.top.clone(), &mut self.syms, &mut self.macros);
        // Borrow-decode: the AOT'd caller keeps owning its arg carriers.
        let result = if args.len() <= INLINE_ARGS {
            let mut buf: [Value; INLINE_ARGS] = [
                Value::Unspecified,
                Value::Unspecified,
                Value::Unspecified,
                Value::Unspecified,
            ];
            for (slot, &a) in buf.iter_mut().zip(args) {
                *slot = cs_vm::vm::vm_nb_borrow_to_value(a);
            }
            crate::eval::apply_procedure(&proc, &buf[..args.len()], &mut ctx)
        } else {
            let arg_vals: Vec<Value> = args
                .iter()
                .map(|&a| cs_vm::vm::vm_nb_borrow_to_value(a))
                .collect();
            crate::eval::apply_procedure(&proc, &arg_vals, &mut ctx)
        };
        match result {
            Ok(v) => cs_vm::vm::vm_value_to_nb(v),
            Err(e) => {
                eprintln!("crabscheme (aot): builtin `{name}`: {}", e.message());
                cs_vm::vm::vm_value_to_nb(Value::Unspecified)
            }
        }
    }

    /// Apply an already-resolved procedure `Value` to `Value` args via the
    /// walker tier (the same dispatch path as [`Self::aot_dispatch_builtin`]).
    /// Used by the actor activation loop (`builtins::beam::spawn-activation`)
    /// to invoke a Scheme message handler once per delivered message. A raised
    /// condition is flattened to its message string — the actor logs and
    /// terminates on error, matching `run_scheme_body`.
    #[cfg(feature = "actor")]
    pub fn apply_value(&mut self, proc: &Value, args: &[Value]) -> Result<Value, String> {
        let mut ctx = EvalCtx::new(self.top.clone(), &mut self.syms, &mut self.macros);
        crate::eval::apply_procedure(proc, args, &mut ctx).map_err(|e| e.message())
    }

    /// Decode a cross-actor [`crate::builtins::beam::SendableValue`] into a
    /// `Value` interned in this runtime's symbol table. A thin wrapper so the
    /// activation loop need not reach into private fields.
    #[cfg(feature = "actor")]
    pub fn sendable_to_value(&mut self, sv: &crate::builtins::beam::SendableValue) -> Value {
        crate::builtins::beam::from_sendable(sv, &mut self.syms)
    }
}

/// Generic-builtin dispatch entry point for AOT'd binaries.
///
/// cs-aot lowers any builtin it can't open-code to
/// `cs_runtime::aot_call_builtin("<name>", &[nb_args…])`. Resolution is by
/// **name** — sym ids differ between AOT-compile time and the AOT binary's
/// fresh runtime, so the id can't be baked in. `args` are NB carriers
/// (borrow-decoded; the caller retains ownership); the return is an NB
/// carrier the caller owns. On an unbound name / runtime error this prints
/// to stderr and returns NB `Unspecified` rather than aborting.
pub fn aot_call_builtin(name: &str, args: &[i64]) -> i64 {
    AOT_RUNTIME.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rt = slot.get_or_insert_with(Runtime::new);
        rt.aot_dispatch_builtin(name, args)
    })
}

/// cs-7rz — per-call-site cache for [`aot_call_builtin_cached`].
///
/// The AOT emitter declares one `thread_local! { static: AotBuiltinCache
/// }` per `CallBuiltin` site (mirrors the `AotIcSlot` pattern for
/// `Call`/`CallGeneral` sites, see `cs_vm::vm::AotIcSlot`). Thread-local
/// rather than a shared `static`: `AOT_RUNTIME` — and therefore the
/// `Symbol` ids and `Value` this cache holds — is itself per-thread, so a
/// cache shared across threads would mix up symbol tables.
#[derive(Default)]
pub struct AotBuiltinCache(RefCell<Option<Value>>);

impl AotBuiltinCache {
    pub const fn new() -> Self {
        Self(RefCell::new(None))
    }
}

/// cs-7rz — inline-cached variant of [`aot_call_builtin`].
///
/// Same contract, plus `cache` — a process-lifetime
/// `&'static AotBuiltinCache` owned by the call site (one per
/// `CallBuiltin` instruction in the AOT'd source, emitted as a local
/// `thread_local!`). On a hit this skips `SymbolTable::intern` and the
/// top-level env lookup entirely; see
/// [`Runtime::aot_dispatch_builtin_cached`] for the soundness argument.
pub fn aot_call_builtin_cached(cache: &AotBuiltinCache, name: &str, args: &[i64]) -> i64 {
    AOT_RUNTIME.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rt = slot.get_or_insert_with(Runtime::new);
        rt.aot_dispatch_builtin_cached(cache, name, args)
    })
}

/// Format an NB result carrier as its external (Write-mode) Scheme
/// representation — used by the AOT main shim to print a function's return
/// value. Handles every value kind (strings show quoted, lists/pairs
/// nested, symbols by name, …); the numeric shim fast-paths fixnum/flonum
/// before falling back here. Borrow-decodes (the carrier isn't consumed).
pub fn aot_format_result(nb: i64) -> String {
    AOT_RUNTIME.with(|cell| {
        let mut slot = cell.borrow_mut();
        let rt = slot.get_or_insert_with(Runtime::new);
        let v = cs_vm::vm::vm_nb_borrow_to_value(nb);
        v.format_with(&rt.syms, WriteMode::Write)
    })
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

    /// cs-i6p.2 regression: the VM-tier `hashtable-set!` /
    /// `hashtable-ref` / `hashtable-delete!` overrides (defined
    /// into `vm_env` above, distinct from the walker-tier funnel in
    /// `cs_runtime::builtins`) must go through `Hashtable`'s
    /// tombstone-aware `set_value_at` / `value_at` /
    /// `swap_remove_item` rather than raw `items[i].1` reads/writes
    /// — otherwise a value the walker tier demoted to a weak
    /// tombstone reads back `Unspecified` on this tier (actor
    /// bodies run VM-tier), a raw overwrite would leave a stale
    /// tombstone shadowing the fresh value, and a raw
    /// `Vec::swap_remove` could misalign a tombstone onto an
    /// unrelated key.
    ///
    /// A walker-tier and a VM-tier `eval_str` call don't share a
    /// top-level environment (`top` vs. `vm_env` are separate), so
    /// this builds the demoted hashtable directly in Rust — via the
    /// same `cs_core::value::Hashtable::break_value_cycle` the
    /// walker-tier `b_hashtable_set` funnel calls — and defines it
    /// straight into `vm_env` (this test lives inside the crate, so
    /// it can reach that private field) before driving it through
    /// `eval_str_via_vm`.
    #[test]
    fn vm_tier_hashtable_builtins_honor_value_tombstones() {
        use cs_core::{Hashtable, HtEqKind};

        let mut rt = Runtime::new();
        let h = Hashtable::new(HtEqKind::Eq);
        let k1 = rt.syms.intern("k1");
        let k2 = rt.syms.intern("k2");
        h.items
            .borrow_mut()
            .push((Value::Symbol(k1), Value::Hashtable(h.clone())));
        h.items
            .borrow_mut()
            .push((Value::Symbol(k2), Value::fixnum(7)));
        // Demote slot 0 (self-cycle) exactly as `b_hashtable_set`
        // would: baseline=1 leaves the `h` local binding itself as
        // the sole external anchor (h.clone() above bumped the
        // strong count to 2 total).
        assert!(
            Hashtable::break_value_cycle(&h, 0, 1),
            "setup: expected the self-cycle to demote"
        );
        let h_sym = rt.syms.intern("h");
        rt.vm_env.define(h_sym, Value::Hashtable(h.clone()));

        let vm_ref = rt
            .eval_str_via_vm("<t>", "(eq? (hashtable-ref h 'k1 #f) h)")
            .expect("vm ref");
        assert_eq!(
            format!("{vm_ref}"),
            "#t",
            "VM-tier hashtable-ref must transparently upgrade the tombstone"
        );

        rt.eval_str_via_vm("<t>", "(hashtable-set! h 'k1 99)")
            .expect("vm set");
        let after_set = rt
            .eval_str_via_vm("<t>", "(hashtable-ref h 'k1 #f)")
            .expect("vm after set");
        assert_eq!(
            format!("{after_set}"),
            "99",
            "VM-tier hashtable-set! must clear the stale tombstone, not shadow the fresh write"
        );

        rt.eval_str_via_vm("<t>", "(hashtable-delete! h 'k2)")
            .expect("vm delete");
        let k1_after_del = rt
            .eval_str_via_vm("<t>", "(hashtable-ref h 'k1 #f)")
            .expect("vm after del");
        assert_eq!(
            format!("{k1_after_del}"),
            "99",
            "deleting an unrelated key via the VM-tier override must not disturb k1's value"
        );
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

    // ---- sandbox import policy (issue #15) ----

    #[test]
    fn sandbox_policy_blocks_unlisted_library() {
        // (rnrs lists) is in resolve_import_spec's table; the only thing
        // that blocks it is the host-policy enforcement from issue #15.
        let mut rt = Runtime::new();
        rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
        let err = rt
            .eval_str("<test>", "(environment '(rnrs lists))")
            .unwrap_err();
        assert!(
            err.message.contains("rnrs lists"),
            "error should name the disallowed library; got: {}",
            err.message
        );
    }

    #[test]
    fn sandbox_policy_allows_listed_library() {
        let mut rt = Runtime::new();
        rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into(), "(rnrs lists)".into()]));
        // Both approved — should produce a valid environment value.
        rt.eval_str(
            "<test>",
            "(environment? (environment '(rnrs base) '(rnrs lists)))",
        )
        .expect("approved libraries must not be blocked");
    }

    #[test]
    fn sandbox_policy_none_is_unrestricted() {
        // No policy set — any known library resolves fine.
        let mut rt = Runtime::new();
        rt.eval_str("<test>", "(environment '(rnrs lists))")
            .expect("no policy means unrestricted");
    }

    #[test]
    fn sandbox_policy_error_names_disallowed_library() {
        let mut rt = Runtime::new();
        rt.set_sandbox_import_policy(Some(vec!["(rnrs base)".into()]));
        let err = rt
            .eval_str("<test>", "(environment '(rnrs lists))")
            .unwrap_err();
        // The error message must include the library name so the caller
        // can distinguish which spec was rejected.
        assert!(
            err.message.contains("rnrs lists"),
            "error must name rejected library; got: {}",
            err.message
        );
    }

    #[test]
    fn aot_call_builtin_dispatches_stdlib() {
        // The AOT generic-dispatch path resolves a builtin by name,
        // borrow-decodes NB args, calls it, and re-encodes the result.
        // string-length of an NB string literal → fixnum 5.
        let s = cs_vm::vm::vm_string_const_nb("hello");
        match cs_vm::vm::vm_nb_borrow_to_value(aot_call_builtin("string-length", &[s])) {
            Value::Fixnum(n) => assert_eq!(n, 5),
            other => panic!("string-length: {other:?}"),
        }
        // string-append "ab" "cd" → "abcd" (multi-arg).
        let a = cs_vm::vm::vm_string_const_nb("ab");
        let b = cs_vm::vm::vm_string_const_nb("cd");
        match cs_vm::vm::vm_nb_borrow_to_value(aot_call_builtin("string-append", &[a, b])) {
            Value::String(st) => assert_eq!(st.borrow().as_str(), "abcd"),
            other => panic!("string-append: {other:?}"),
        }
        // The original arg carriers are still valid (borrow-decode, not
        // consume): re-use `a` in another call.
        match cs_vm::vm::vm_nb_borrow_to_value(aot_call_builtin("string-length", &[a])) {
            Value::Fixnum(n) => assert_eq!(n, 2),
            other => panic!("string-length reuse: {other:?}"),
        }
        // Unbound name → no panic, returns Unspecified.
        assert!(matches!(
            cs_vm::vm::vm_nb_borrow_to_value(aot_call_builtin("nope-not-a-builtin", &[])),
            Value::Unspecified
        ));
    }
}
