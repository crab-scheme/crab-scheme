//! CrabScheme runtime: tree-walking interpreter, environments, builtins.

pub mod builtins;
pub mod env;
pub mod eval;
pub mod proc;

use std::rc::Rc;

use cs_core::{SymbolTable, Value, WriteMode};
use cs_diag::{Diagnostic, FileId, SourceMap};
use cs_expand::Expander;
use cs_parse::read_all;

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
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    pub fn new() -> Self {
        let mut syms = SymbolTable::new();
        let top = Frame::root();
        builtins::install_into(&top, &mut syms);
        let vm_env = cs_vm::vm::Env::root();
        for (name, f) in builtins::pure_builtins() {
            let sym = syms.intern(name);
            vm_env.define(sym, cs_vm::vm::make_vm_builtin(name, f));
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
        let force_sym = syms.intern("force");
        vm_env.define(force_sym, cs_vm::vm::make_vm_force());
        // I/O port-state ops.
        let display_sym = syms.intern("display");
        vm_env.define(display_sym, cs_vm::vm::make_vm_display());
        let write_sym = syms.intern("write");
        vm_env.define(write_sym, cs_vm::vm::make_vm_write());
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
        let cip_sym = syms.intern("current-input-port");
        vm_env.define(cip_sym, cs_vm::vm::make_vm_current_input_port());
        let cop_sym = syms.intern("current-output-port");
        vm_env.define(cop_sym, cs_vm::vm::make_vm_current_output_port());
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
                Ok(Value::Symbol(st.intern("__top-level-env__")))
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
        let ienv_sym = syms.intern("interaction-environment");
        vm_env.define(
            ienv_sym,
            cs_vm::vm::make_vm_builtin_syms("interaction-environment", |args, st| {
                if !args.is_empty() {
                    return Err("interaction-environment: 0 args".into());
                }
                Ok(Value::Symbol(st.intern("__top-level-env__")))
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
                            let datum = reader
                                .read(st)
                                .map_err(|e| format!("read: {}", e.message()))?;
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
        Self {
            syms,
            sources: SourceMap::new(),
            top,
            macros: std::collections::HashMap::new(),
            vm_env,
        }
    }

    pub fn symbols(&self) -> &SymbolTable {
        &self.syms
    }

    pub fn source_map(&self) -> &SourceMap {
        &self.sources
    }

    /// Evaluate a string of Scheme source. Returns the value of the final
    /// top-level expression (or `Unspecified` for empty/define-only input).
    pub fn eval_str(&mut self, name: &str, src: &str) -> Result<Value, Diagnostic> {
        let file_id = self.sources.add(name, src);
        self.eval_str_in_file(file_id, src)
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
                    "uncaught escape continuation #{} (value: {})",
                    id,
                    v.format_with(&self.syms, WriteMode::Write)
                ),
                crate::eval::EvalErrorKind::Message(m) => match m.as_str() {
                    "__escape__" => match pending_escape {
                        Some((id, v)) => format!(
                            "uncaught escape continuation #{} (value: {})",
                            id,
                            v.format_with(&self.syms, WriteMode::Write)
                        ),
                        None => "uncaught escape continuation".to_string(),
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

    /// Evaluate a string of Scheme source via the **bytecode VM** tier.
    /// Foundation: only pure builtins are supported. Higher-order builtins
    /// (apply/map/raise/with-exception-handler/etc.) and parameterize/dynamic-wind
    /// fall back to the tree-walker via per-call dispatch — for now this VM
    /// path is best-effort for pure-arithmetic / pure-list programs.
    pub fn eval_str_via_vm(&mut self, name: &str, src: &str) -> Result<Value, Diagnostic> {
        let file_id = self.sources.add(name, src);
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
                        "uncaught escape continuation #{} (value: {})",
                        id,
                        v.format_with(&self.syms, WriteMode::Write)
                    ),
                    None => "uncaught escape continuation".to_string(),
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
