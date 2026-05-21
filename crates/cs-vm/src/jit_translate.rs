//! Bytecode → RIR translator (M6 iter 5).
//!
//! Translates a [`crate::opcode::CompiledLambda`] body into a
//! [`cs_rir::Function`] that the JIT backend can lower. Supports a
//! narrow subset for now — enough for fib / fact / ack / nqueens-
//! style lambdas (pure-fixnum, control flow, single-recursion).
//!
//! Out of scope this iter:
//! - Closures / non-param `LoadVar` (env access)
//! - General `Call` (only self-recursive `Call N` is allowed; the
//!   self-name is supplied by the caller and matched against
//!   `LoadVar(self_name) ... Call N` patterns)
//! - `set!`, `define-local`, multiple values, raise, etc.
//! - Joins beyond two return arms (we only translate "branch then
//!   each arm Returns" shapes; explicit Jump-to-join lands later)
//!
//! The translator simulates the VM stack as a `Vec<RirValue>` and
//! emits SSA per push. Block boundaries are: bytecode offset 0,
//! every JumpIfFalse/Jump target, and every offset just after a
//! Jump or Return.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use cs_core::{Symbol, Value};
use cs_rir::inline::{analyze_for_inline, splice_single_block, SpliceRequest};
use cs_rir::{Block, BlockId, Const, Function, Inst as RirInst, Term, Type, Value as RirValue};

use crate::opcode::{CompiledLambda, Inst};
use crate::vm::{Env, VmClosure};

/// Errors the translator can surface. `Unsupported` is the dominant
/// signal — when the runtime asks the JIT to compile a closure
/// whose bytecode contains an opcode we don't yet handle, the
/// runtime stays on the VM.
#[derive(Debug)]
pub enum TranslateError {
    /// An unsupported opcode appeared in the bytecode.
    Unsupported(String),
    /// Internal invariant violated (stack underflow, dangling
    /// branch target, etc.). These indicate a bug in either the
    /// translator or the bytecode source.
    Invalid(String),
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::Unsupported(s) => write!(f, "unsupported: {s}"),
            TranslateError::Invalid(s) => write!(f, "invalid: {s}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// Translate `lambda` into a `cs_rir::Function`.
///
/// `name` is the resulting function's name (used by the JIT module
/// for declare_function). `self_name`, if `Some`, identifies the
/// symbol that resolves to the function being translated; the
/// translator emits `Inst::CallSelf` when it sees
/// `LoadVar(self_name) ... Call N` patterns.
pub fn bytecode_to_rir(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
) -> Result<Function, TranslateError> {
    bytecode_to_rir_with_hints(lambda, name, self_name, None)
}

/// RC2 iter J — `bytecode_to_rir` plus a post-translation pass that
/// demotes non-escaping `EnvDefineLocal`/`EnvLookup*` pairs to SSA
/// aliases in the entry block. The transformation is the same one
/// the JIT already runs on inlined leaf-callees (see
/// `demote_env_to_ssa_in_first_block`'s doc) — it eliminates env
/// round-trips that arise from `(let ((x ...)) body)` desugaring
/// when `x` doesn't escape the body.
///
/// **Why expose this as a separate entry point:** the JIT only
/// applies the demote during inlining because for top-level JIT
/// bodies the env round-trip is harmless (the helpers exist
/// because JIT bodies run with `JIT_CALLER_ENV` installed). cs-aot
/// has no equivalent install API, so `EnvDefineLocal`/`EnvLookup*`
/// don't lower — programs that use `let` fail today. Demoting at
/// the top level makes `let`-using programs AOT-able without
/// adding Env-Inst support to cs-aot.
///
/// On failure (env op references a symbol not defined in this body
/// — i.e. a free variable from an enclosing closure): falls back
/// to the un-demoted RIR. cs-aot will then surface the specific
/// unhandled Inst in its `UnsupportedInst` error, same as today.
pub fn bytecode_to_rir_aot(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
) -> Result<Function, TranslateError> {
    bytecode_to_rir_aot_with_globals(lambda, name, self_name, None)
}

/// RC3 iter 2.7 — same as `bytecode_to_rir_aot` but accepts a set of
/// top-level sym IDs that are known to be AOT'd as separate
/// functions. Such syms are EXCLUDED from `func.captures` so the
/// emitter can resolve them as direct cross-procedure references via
/// the resolver's by-name-sym table rather than expecting them to be
/// passed in as closure captures. Pass `None` (or an empty set) for
/// the legacy single-procedure path.
pub fn bytecode_to_rir_aot_with_globals(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
    known_globals: Option<&std::collections::HashSet<u32>>,
) -> Result<Function, TranslateError> {
    bytecode_to_rir_aot_with_param_types(lambda, name, self_name, known_globals, None)
}

/// RC3 iter 2.15 — full AOT translator with per-param type hints.
/// cs-cli's aot --multi uses this to default LETREC-bound inner
/// lambdas' params to `Type::Any` (so they accept pair/list values
/// that the parent passes via named-let / letrec). Top-level fns
/// keep the `None` default (Fixnum) since they're called from CLI
/// with parsed Fixnum args.
pub fn bytecode_to_rir_aot_with_param_types(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
    known_globals: Option<&std::collections::HashSet<u32>>,
    param_type_hints: Option<&[Type]>,
) -> Result<Function, TranslateError> {
    let mut func = bytecode_to_rir_with_hints(lambda, name, self_name, param_type_hints)?;
    // RC3 iter 2.13 — seed self_binding_sym from self_name so
    // record_captures (which runs below) can exclude self-EnvLookups
    // from the captures list (they resolve to __self_handle instead).
    // cs-cli also sets this field after translation, but record_captures
    // needs it FIRST.
    if let Some(sym) = self_name {
        func.self_binding_sym = Some(sym.0);
    }
    // RC3 Phase 2 iter 2.5: demote runs for any function shape,
    // single- or multi-block. Pre-scan inside the helper bails
    // cleanly without mutating `func` on free-vars / duplicate
    // defines / forward cross-block lookups (the iter-2.6 atomicity
    // fix combined with iter-2.5's no-duplicate-define invariant).
    // On bail, downstream sees the original RIR + a clean
    // UnsupportedInst diagnostic from cs-aot.
    let _ = demote_env_to_ssa_all_blocks(&mut func);
    // RC3 iter 2.4 Step 1 + iter 2.7: capture analysis post-pass.
    // The demote dropped EnvLookup/EnvLookupAny for locally-defined
    // syms; any surviving env op references either (a) a free
    // variable that must be captured at MakeClosure time, or (b) a
    // top-level global known to be AOT'd separately. Record (a) in
    // first-seen order into func.captures; cs-aot resolves (b)
    // through the LambdaResolver's by_name_sym table at emit time.
    record_captures(&mut func, known_globals);
    Ok(func)
}

/// RC3 iter 2.4 Step 1 + iter 2.7 — walk a translated Function
/// looking for `EnvLookup` / `EnvLookupAny` insts referring to syms
/// NOT locally defined AND NOT in `known_globals`. Each such sym is
/// a captured free variable; we record them on `func.captures` in
/// first-seen order. Syms that ARE in `known_globals` are
/// cross-procedure references that cs-aot resolves at emit time
/// via the LambdaResolver.
///
/// Run AFTER `demote_env_to_ssa_all_blocks` so the demote-able
/// lookups (those paired with EnvDefineLocal in the same function)
/// are already gone. Surviving lookups are captures or globals
/// by definition.
fn record_captures(
    func: &mut cs_rir::Function,
    known_globals: Option<&std::collections::HashSet<u32>>,
) {
    use std::collections::HashSet;
    let mut seen: HashSet<u32> = HashSet::new();
    let mut order: Vec<u32> = Vec::new();
    // RC3 iter 2.13 — exclude the function's own self-binding-sym
    // from captures. EnvLookups of THIS sym resolve to __self_handle
    // (iter 2.12 + iter 2.13's EnvLookup arm), not a capture slot.
    // Including it as a capture would force the caller to provide a
    // value it can't have: at MakeClosure time, the closure is being
    // CREATED — its handle doesn't exist yet.
    let self_sym = func.self_binding_sym;
    for block in &func.blocks {
        for inst in &block.insts {
            if let RirInst::EnvLookup(_, sym) | RirInst::EnvLookupAny(_, sym) = inst {
                if known_globals.is_some_and(|g| g.contains(sym)) {
                    continue;
                }
                if self_sym == Some(*sym) {
                    continue;
                }
                if seen.insert(*sym) {
                    order.push(*sym);
                }
            }
        }
    }
    func.captures = order;
}

/// RC2 iter O — extends `demote_env_to_ssa_in_first_block` to
/// multi-block functions. The previous version only rewrote the
/// first block's Insts + terminator; values demoted in block 0
/// would surface as undefined references in blocks 1+ (`tak`-style
/// nested-self-call programs hit this).
///
/// Algorithm:
/// 1. Walk block 0 to collect `sym → src` from EnvDefineLocal +
///    `dst → src` alias from EnvLookupAny/EnvLookup. Same as the
///    single-block version.
/// 2. Drop the demoted Insts from block 0 + rewrite operands of
///    its remaining Insts and terminator via the alias map.
/// 3. NEW: walk blocks 1+, rewriting Value operands of every
///    `is_inline_supported`-eligible Inst and every terminator
///    via the SAME alias map. Insts outside the supported set
///    (MakeClosure, Vec*, Cons/Car/Cdr, type predicates, etc.)
///    have their operands left untouched — they panic
///    `unreachable!()` in `for_each_value_in_inst`. If a demoted
///    let-binding is referenced by such an inst in a non-block-0
///    block, the AOT emitter will still surface a clean
///    use-before-def error rather than corrupting the value.
///
/// Returns false (same contract as the original) if any EnvLookup
/// references a sym not defined locally — the free-var case
/// signals the AOT path should leave func untouched.
/// RC3 iter 2.11 — clone an Inst and rewrite its operand Values
/// through the alias map. Used by demote when we choose to KEEP an
/// Inst (rather than dropping it as an alias-target) but still want
/// operand rewrites — same machinery as the catch-all arm.
fn other_with_alias(inst: &RirInst, alias: &HashMap<RirValue, RirValue>) -> RirInst {
    use cs_rir::inline::for_each_value_in_inst;
    let mut cloned = inst.clone();
    for_each_value_in_inst(&mut cloned, |v| {
        let mut cur = *v;
        for _ in 0..=alias.len() {
            match alias.get(&cur) {
                Some(&next) if next != cur => cur = next,
                _ => break,
            }
        }
        *v = cur;
    });
    cloned
}

fn demote_env_to_ssa_all_blocks(func: &mut cs_rir::Function) -> bool {
    use cs_rir::inline::{for_each_value_in_inst, for_each_value_in_term};

    if func.blocks.is_empty() {
        return true;
    }

    // RC3 Phase 2 iter 2.5 — proper multi-block demote via the
    // pre-scan pattern from iter 2.6.
    //
    // Two-tier strictness:
    // - **Single-block functions**: same posture as iter J — pre-
    //   scan just verifies every lookup's sym is defined somewhere
    //   in block 0. Multiple defines of the same sym are fine
    //   (re-bindings; last define wins in textual order, matching
    //   the iter-J behavior the scorecard already validated).
    // - **Multi-block functions**: stricter. Pre-scan requires
    //   every looked-up sym to be defined EXACTLY ONCE across the
    //   whole function — multiple defines would need φ-merge at
    //   branch points, which iter 2.5 doesn't implement. Bails
    //   cleanly without mutating `func` on either constraint
    //   miss (free var, duplicate define, forward cross-block ref).
    let single_block = func.blocks.len() == 1;
    // RC3 iter 2.11 — track defines per-block + the lookups. A sym
    // with multiple defines all in the SAME block is a re-binding
    // (letrec/named-let emits a placeholder Unspecified-define then
    // overwrites after MakeClosure); the alias safely tracks the
    // LAST define in textual order, even in multi-block functions.
    // The strict bail only applies when defines span multiple blocks
    // (which truly needs φ-merge).
    let mut defined_blocks: HashMap<u32, std::collections::HashSet<usize>> = HashMap::new();
    let mut all_lookup_syms: Vec<u32> = Vec::new();
    for (bidx, block) in func.blocks.iter().enumerate() {
        for inst in &block.insts {
            match inst {
                RirInst::EnvDefineLocal(sym, _) => {
                    defined_blocks.entry(*sym).or_default().insert(bidx);
                }
                RirInst::EnvLookupAny(_, sym) | RirInst::EnvLookup(_, sym) => {
                    all_lookup_syms.push(*sym);
                }
                _ => {}
            }
        }
    }
    // demote_eligible: syms whose lookups we CAN safely alias to a
    // single source Value. Free vars / captures / globals NOT
    // eligible — they survive as EnvLookups for the AOT resolver to
    // handle. Multi-block + cross-block multi-define also NOT
    // eligible — needs φ-merge.
    let mut demote_eligible: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for sym in &all_lookup_syms {
        match defined_blocks.get(sym) {
            None => {} // free var — skip
            Some(blocks) if blocks.len() == 1 => {
                // All defines in one block — safe re-binding (last
                // define wins in textual order).
                demote_eligible.insert(*sym);
            }
            Some(_) if single_block => {
                demote_eligible.insert(*sym);
            }
            Some(_) => {} // defines span multiple blocks — needs φ-merge
        }
    }

    let mut sym_to_src: HashMap<u32, RirValue> = HashMap::new();
    let mut alias: HashMap<RirValue, RirValue> = HashMap::new();
    let resolve = |v: RirValue, alias: &HashMap<RirValue, RirValue>| -> RirValue {
        let mut cur = v;
        for _ in 0..=alias.len() {
            match alias.get(&cur) {
                Some(&next) if next != cur => cur = next,
                _ => return cur,
            }
        }
        cur
    };

    // Per-block rewritten buffer + alias-build walk. Done in dominator
    // order (block 0 first) so EnvLookupAny in block N can resolve
    // through the alias map built from block M's EnvDefineLocal for
    // M < N.
    let mut per_block_rewritten: Vec<Vec<RirInst>> = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut rewritten = Vec::with_capacity(block.insts.len());
        for inst in &block.insts {
            match inst {
                RirInst::EnvDefineLocal(sym, src) => {
                    if demote_eligible.contains(sym) {
                        sym_to_src.insert(*sym, resolve(*src, &alias));
                    } else {
                        // Non-eligible sym (multi-define in multi-
                        // block) — keep the define so subsequent
                        // operations against this sym still work.
                        rewritten.push(other_with_alias(inst, &alias));
                    }
                }
                RirInst::EnvLookupAny(d, sym) | RirInst::EnvLookup(d, sym) => {
                    // RC3 iter 2.11 — three-way dispatch:
                    //   1. Sym is demote-eligible AND we have a
                    //      sym_to_src entry → add to alias map, drop
                    //      this EnvLookup.
                    //   2. Sym is demote-eligible but no sym_to_src
                    //      yet (forward cross-block ref) → keep the
                    //      EnvLookup; downstream cs-aot uses its
                    //      captures / local_defs / by_name_sym
                    //      resolver. The d Value still gets defined
                    //      by the surviving EnvLookup.
                    //   3. Sym is not eligible → same as (2).
                    if demote_eligible.contains(sym) {
                        if let Some(&src) = sym_to_src.get(sym) {
                            alias.insert(*d, src);
                        } else {
                            rewritten.push(other_with_alias(inst, &alias));
                        }
                    } else {
                        rewritten.push(other_with_alias(inst, &alias));
                    }
                }
                other => {
                    // RC3 iter 2.7 — apply the alias map to operand
                    // Values in EVERY surviving inst, not just
                    // is_inline_supported ones. The former check was
                    // a leftover from when the demote pass piggybacked
                    // on the JIT inliner's eligibility predicate;
                    // CallGeneral / MakeClosure / Cons / etc. all need
                    // their operands rewritten so a callee read of a
                    // demoted let-binding resolves to the right SSA
                    // Value. Without this, a named-let lambda
                    // reference (`(let loop ...) (loop i acc)`) ends
                    // up with an unresolved Value(N) in CallGeneral's
                    // callee slot.
                    let mut cloned = other.clone();
                    for_each_value_in_inst(&mut cloned, |v| {
                        *v = resolve(*v, &alias);
                    });
                    rewritten.push(cloned);
                }
            }
        }
        per_block_rewritten.push(rewritten);
    }

    // Commit: assign the per-block rewritten Insts back + rewrite
    // each block's terminator via the now-complete alias map.
    for (block, rewritten) in func.blocks.iter_mut().zip(per_block_rewritten) {
        block.insts = rewritten;
        for_each_value_in_term(&mut block.terminator, |v| {
            *v = resolve(*v, &alias);
        });
    }

    true
}

/// Phase 6 Stage A iter 2 — maximum inline-recursion depth. Caps
/// nested leaf-callee inlining so a body that calls another body
/// that calls another body doesn't blow up code size or recurse
/// infinitely through mutually-recursive lambdas.
///
/// Depth 0 = the JIT'd body itself; depth 1+ = inlined callees.
/// Iter 2 keeps the cap conservative at 1 (caller can inline a
/// single layer of leaf callees, but inlined callees themselves
/// fall back to CallGeneral). Iter 3+ may raise this once
/// measurement shows code-size cost is contained.
pub const MAX_INLINE_DEPTH: usize = 1;

/// Like `bytecode_to_rir` but accepts an optional per-param type
/// hint. When provided, params are seeded with the given types in
/// the per-Value type table so flonum-arg bodies emit the right
/// arithmetic flavor without needing `real->flonum` conversion in
/// the body. The runtime uses this for arg-side type feedback at
/// tier-up.
pub fn bytecode_to_rir_with_hints(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
    param_type_hints: Option<&[Type]>,
) -> Result<Function, TranslateError> {
    bytecode_to_rir_full(lambda, name, self_name, param_type_hints, None, 0)
}

/// Phase 6 Stage A iter 2 — the env-aware translator entry. Same
/// as `bytecode_to_rir_with_hints` plus two new parameters:
///
/// - `caller_env`: when present, free-var callees at `CallGeneral`
///   sites can be resolved against this env and their bodies inlined
///   in place. The runtime hook supplies `Some(&closure.env)`.
/// - `inline_depth`: how many layers of leaf-callee inlining have
///   already happened. Recursive calls (when iter 2 inlines a callee)
///   pass `current_depth + 1`; the analyzer rejects further inlining
///   once it reaches `MAX_INLINE_DEPTH`.
///
/// Callers without env access (tests, the legacy `bytecode_to_rir`
/// wrapper) pass `None` + `0`; the inlining path then simply doesn't
/// trigger, and translation matches the pre-iter-2 behavior exactly.
pub fn bytecode_to_rir_full(
    lambda: &CompiledLambda,
    name: impl Into<String>,
    self_name: Option<Symbol>,
    param_type_hints: Option<&[Type]>,
    caller_env: Option<&Rc<Env>>,
    inline_depth: usize,
) -> Result<Function, TranslateError> {
    if lambda.rest.is_some() {
        return Err(TranslateError::Unsupported(
            "rest parameters not yet supported".into(),
        ));
    }
    let body = &lambda.body[..];

    // Phase 5b iter7 — pre-scan for MakeClosure in body. Retained
    // from iter5 to gate the FlonumDiv path (which would lift bodies
    // like mandelbrot's col-loop into uniform-NB JIT where their
    // let*-CallGeneral chain burns host stack). Inst::Div (the
    // helper-based slow path) doesn't need this gate — the helper
    // call has no stack-burn risk beyond a normal CallGeneral.
    let body_has_makeclosure = body.iter().any(|inst| matches!(inst, Inst::MakeClosure(_)));

    // Identify block-start offsets.
    let mut starts: BTreeSet<usize> = BTreeSet::new();
    starts.insert(0);
    for (i, inst) in body.iter().enumerate() {
        match inst {
            Inst::JumpIfFalse(t)
            | Inst::Jump(t)
            | Inst::BranchOnGeFx2(t)
            | Inst::BranchOnGtFx2(t)
            | Inst::BranchOnLeFx2(t)
            | Inst::BranchOnLtFx2(t)
            | Inst::BranchOnNeFx2(t) => {
                starts.insert(*t);
                if i + 1 < body.len() {
                    starts.insert(i + 1);
                }
            }
            Inst::Return => {
                if i + 1 < body.len() {
                    starts.insert(i + 1);
                }
            }
            _ => {}
        }
    }

    // Assign BlockIds in offset order.
    let mut offset_to_block: HashMap<usize, BlockId> = HashMap::new();
    let mut block_offsets: Vec<usize> = Vec::new();
    for (i, off) in starts.iter().enumerate() {
        offset_to_block.insert(*off, BlockId(i as u32));
        block_offsets.push(*off);
    }

    // Build the Function shell. Params are RIR Values 0..N-1.
    // When the caller supplied per-param type hints (arg-side
    // feedback), use them; else default to Fixnum.
    let mut func = Function::new(name);
    for (i, sym) in lambda.params.iter().enumerate() {
        let t = param_type_hints
            .and_then(|h| h.get(i).copied())
            .unwrap_or(Type::Fixnum);
        func.params.push((RirValue(i as u32), t));
        // RC3 iter 2.7 — record the param's sym ID so cs-aot can
        // map EnvLookup(sym) of a param to the right Value when
        // resolving an inner closure's captures.
        func.param_syms.push(sym.0);
    }
    func.entry = BlockId(0);

    // SSA value allocator. Param values reserved 0..params.len()-1.
    //
    // Phase 6 Stage A iter 2: `next_value_id` lives in a `Cell` so
    // the inlining splice path (which needs to read/bump the id
    // counter directly to pick its `SpliceRequest::value_offset`)
    // can do so without colliding with `alloc`'s mutable borrow.
    // The closure stays `Fn` instead of `FnMut`; existing call sites
    // (`alloc()`) are unchanged.
    let next_value_id: Cell<u32> = Cell::new(lambda.params.len() as u32);
    let mut alloc = || -> RirValue {
        let v = RirValue(next_value_id.get());
        next_value_id.set(v.0 + 1);
        v
    };

    // Map param symbol -> RirValue.
    let mut param_map: HashMap<Symbol, RirValue> = HashMap::new();
    for (i, sym) in lambda.params.iter().enumerate() {
        param_map.insert(*sym, RirValue(i as u32));
    }

    // Phase 5b iter8 — local lexical bindings tracked alongside the
    // runtime env. Stack of frames so LeaveScope can pop.
    let mut local_scopes: Vec<HashMap<Symbol, RirValue>> = Vec::new();

    // Per-Value type table populated as the translator emits
    // instructions. Lets arithmetic emission pick FlonumAdd vs
    // Add based on operand types, and the return-type post-pass
    // skip its own re-classification. Params default to Fixnum
    // (the i64 ABI's only legal arg type at present).
    let mut value_types: HashMap<RirValue, Type> = HashMap::new();
    for i in 0..lambda.params.len() {
        let t = param_type_hints
            .and_then(|h| h.get(i).copied())
            .unwrap_or(Type::Fixnum);
        value_types.insert(RirValue(i as u32), t);
    }

    // Per-block entry stack: the SSA values that should be on the
    // simulated stack when the block starts executing. Set by the
    // predecessor's Jump emission (the predecessor allocates fresh
    // RIR Values to serve as block params + names them in this map
    // for the target). The entry block starts empty (function args
    // are bound separately via param_map).
    let mut block_entry_stack: HashMap<BlockId, Vec<RirValue>> = HashMap::new();
    block_entry_stack.insert(BlockId(0), Vec::new());

    // Per-block declared params. Populated alongside
    // block_entry_stack so we can emit Block { params: ... } at the
    // end. Each entry's RirValues are the same SSA ids that the
    // entry-stack contains.
    let mut block_params: HashMap<BlockId, Vec<(RirValue, Type)>> = HashMap::new();
    block_params.insert(BlockId(0), Vec::new());

    // Snapshot of the Any-typed function-param RIR Values. At
    // every `Term::Return` we emit `AnyDrop` for each so the
    // dispatch-side allocation doesn't leak when the body never
    // consumed the original. Cloned uses (from `AnyClone`) own
    // separate boxes and are unaffected.
    let any_params: Vec<RirValue> = func
        .params
        .iter()
        .filter(|(_, t)| *t == Type::Any)
        .map(|(v, _)| *v)
        .collect();

    // Translate each block.
    for (i, &start) in block_offsets.iter().enumerate() {
        let block_id = BlockId(i as u32);
        let end = if i + 1 < block_offsets.len() {
            block_offsets[i + 1]
        } else {
            body.len()
        };

        // Initialize the simulated stack from the block-entry stack
        // table. If a block was never targeted by a predecessor
        // (unreachable in offset order), we default to empty — the
        // body will catch any underflow.
        let mut sim_stack: Vec<StackEntry> = block_entry_stack
            .get(&block_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(StackEntry::Value)
            .collect();
        let mut insts: Vec<RirInst> = Vec::new();
        let mut term: Option<Term> = None;

        let mut ip = start;
        while ip < end {
            let op = &body[ip];
            ip += 1;
            match op {
                Inst::Const(v) => {
                    // Procedure constants (compiler-folded builtins
                    // like `quotient`, `bitwise-and`) get pushed as
                    // BuiltinRef sentinels. The matching Call N
                    // consumes them and emits a specialized RIR op.
                    if let cs_core::Value::Procedure(p) = v {
                        if let Some(name) = p.name() {
                            // Leak the name into 'static. Builtins
                            // we lower have stable static names; the
                            // leak is one-per-distinct-builtin per
                            // process and bounded.
                            let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
                            sim_stack.push(StackEntry::BuiltinRef(leaked));
                            continue;
                        }
                    }
                    let c = value_to_const(v)?;
                    let dst = alloc();
                    let t = match c {
                        Const::Flonum(_) => Type::Flonum,
                        Const::Boolean(_) => Type::Boolean,
                        Const::Character(_) => Type::Character,
                        Const::Null => Type::Null,
                        Const::Symbol(_) => Type::Symbol,
                        _ => Type::Fixnum,
                    };
                    insts.push(RirInst::LoadConst(dst, c));
                    value_types.insert(dst, t);
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::LoadVar(sym) => {
                    // iter8b — for symbols in local_scopes (letrec-
                    // bound), emit an Any-typed env lookup directly.
                    // RirInst::EnvDefineLocal stored the value into
                    // the env; we read it back via EnvLookupAny so
                    // we always get a fresh ref (no SSA staleness).
                    let local_known = local_scopes
                        .iter()
                        .rev()
                        .any(|frame| frame.contains_key(sym));
                    if local_known {
                        let dst = alloc();
                        insts.push(RirInst::EnvLookupAny(dst, sym.0));
                        value_types.insert(dst, Type::Any);
                        sim_stack.push(StackEntry::Value(dst));
                    } else if let Some(v) = param_map.get(sym).copied() {
                        // Any-typed params live in linear-typed land:
                        // each "use" must own a fresh box. We clone
                        // on every load and drop the original at
                        // function exit. Immediate-typed params
                        // (Fixnum/Boolean/Character/Flonum) are pure
                        // i64 — share the param value directly.
                        let pt = value_types.get(&v).copied().unwrap_or(Type::Fixnum);
                        if pt == Type::Any {
                            let dst = alloc();
                            insts.push(RirInst::AnyClone(dst, v));
                            value_types.insert(dst, Type::Any);
                            sim_stack.push(StackEntry::Value(dst));
                        } else {
                            sim_stack.push(StackEntry::Value(v));
                        }
                    } else if Some(*sym) == self_name {
                        sim_stack.push(StackEntry::SelfRef);
                    } else {
                        // Free variable: emit RIR EnvLookup, which
                        // the lowerer turns into a call to the
                        // runtime helper `vm_env_lookup_fixnum`.
                        // The helper reads the closure's env from
                        // the thread-local set by `try_dispatch_jit`.
                        // Currently only Fixnum-bound free vars are
                        // supported; non-Fixnum bindings panic.
                        let dst = alloc();
                        insts.push(RirInst::EnvLookup(dst, sym.0));
                        sim_stack.push(StackEntry::Value(dst));
                    }
                }
                Inst::Pop => {
                    pop_value(&mut sim_stack)?;
                }
                Inst::MakeClosure(idx) => {
                    // ADR 0012 D-2 (iter BZ) — emit a runtime call to
                    // `vm_make_closure(lambda_idx)`. The helper reads
                    // env+bc from JIT TLS (installed by
                    // `try_dispatch_jit`) and builds a fresh
                    // `VmClosure` matching the bytecode-tier
                    // `Inst::MakeClosure`. The result is an Any-typed
                    // Gc<Value::Procedure> handle.
                    let dst = alloc();
                    insts.push(RirInst::MakeClosure(dst, *idx as u32));
                    value_types.insert(dst, Type::Any);
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::SetVar(sym) => {
                    let val = pop_value(&mut sim_stack)?;
                    if param_map.contains_key(sym) {
                        return Err(TranslateError::Unsupported(
                            "set! of a parameter (mutable params not yet supported)".into(),
                        ));
                    }
                    // Update local SSA if sym is locally bound; always
                    // emit EnvSet so any captured closure sees the
                    // new value via env lookup.
                    for frame in local_scopes.iter_mut().rev() {
                        if frame.contains_key(sym) {
                            frame.insert(*sym, val);
                            break;
                        }
                    }
                    insts.push(RirInst::EnvSet(sym.0, val));
                }
                // Phase 5b iter8 — EnterScope pushes a local frame.
                // No RIR emission: the bytecode VM's EnterScope creates
                // an env LAYER, but try_dispatch_jit_nb already
                // installs a fresh child env when jit_needs_frame_env
                // is true (which the rir.builds_closures() check sets
                // for bodies with MakeClosure). DefineLocal calls go
                // into that frame env via vm_env_define_local_nb.
                Inst::EnterScope => {
                    local_scopes.push(HashMap::new());
                }
                Inst::LeaveScope => {
                    if local_scopes.pop().is_none() {
                        return Err(TranslateError::Invalid(
                            "LeaveScope without matching EnterScope".into(),
                        ));
                    }
                }
                Inst::DefineLocal(sym) => {
                    let val = pop_value(&mut sim_stack)?;
                    if let Some(frame) = local_scopes.last_mut() {
                        frame.insert(*sym, val);
                    }
                    insts.push(RirInst::EnvDefineLocal(sym.0, val));
                }
                Inst::AddFx2 => emit_arith_binop(
                    &mut insts,
                    &mut sim_stack,
                    &mut alloc,
                    &mut value_types,
                    RirInst::Add,
                    RirInst::FlonumAdd,
                )?,
                Inst::SubFx2 => emit_arith_binop(
                    &mut insts,
                    &mut sim_stack,
                    &mut alloc,
                    &mut value_types,
                    RirInst::Sub,
                    RirInst::FlonumSub,
                )?,
                Inst::MulFx2 => emit_arith_binop(
                    &mut insts,
                    &mut sim_stack,
                    &mut alloc,
                    &mut value_types,
                    RirInst::Mul,
                    RirInst::FlonumMul,
                )?,
                Inst::LtFx2 => emit_cmp_binop(
                    &mut insts,
                    &mut sim_stack,
                    &mut alloc,
                    &mut value_types,
                    RirInst::Lt,
                    RirInst::FlonumLt,
                )?,
                Inst::EqFx2 => emit_cmp_binop(
                    &mut insts,
                    &mut sim_stack,
                    &mut alloc,
                    &mut value_types,
                    RirInst::Eq,
                    RirInst::FlonumEq,
                )?,
                Inst::GtFx2 => {
                    // a > b  →  b < a (swap operands).
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let dst = alloc();
                    let at = value_types.get(&a).copied().unwrap_or(Type::Fixnum);
                    let bt = value_types.get(&b).copied().unwrap_or(Type::Fixnum);
                    let lt_inst = if at == Type::Flonum && bt == Type::Flonum {
                        RirInst::FlonumLt(dst, b, a)
                    } else {
                        RirInst::Lt(dst, b, a)
                    };
                    insts.push(lt_inst);
                    value_types.insert(dst, Type::Boolean);
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::LeFx2 => {
                    // a <= b  →  NOT (b < a). The negation is done as
                    // (Eq lt 0) — both ends of the equality are
                    // Booleans so it's safe regardless of operand
                    // tier.
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let at = value_types.get(&a).copied().unwrap_or(Type::Fixnum);
                    let bt = value_types.get(&b).copied().unwrap_or(Type::Fixnum);
                    let lt = alloc();
                    let lt_inst = if at == Type::Flonum && bt == Type::Flonum {
                        RirInst::FlonumLt(lt, b, a)
                    } else {
                        RirInst::Lt(lt, b, a)
                    };
                    insts.push(lt_inst);
                    value_types.insert(lt, Type::Boolean);
                    let zero = alloc();
                    insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                    value_types.insert(zero, Type::Fixnum);
                    let dst = alloc();
                    insts.push(RirInst::Eq(dst, lt, zero));
                    value_types.insert(dst, Type::Boolean);
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::GeFx2 => {
                    // a >= b  →  NOT (a < b).
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let at = value_types.get(&a).copied().unwrap_or(Type::Fixnum);
                    let bt = value_types.get(&b).copied().unwrap_or(Type::Fixnum);
                    let lt = alloc();
                    let lt_inst = if at == Type::Flonum && bt == Type::Flonum {
                        RirInst::FlonumLt(lt, a, b)
                    } else {
                        RirInst::Lt(lt, a, b)
                    };
                    insts.push(lt_inst);
                    value_types.insert(lt, Type::Boolean);
                    let zero = alloc();
                    insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                    value_types.insert(zero, Type::Fixnum);
                    let dst = alloc();
                    insts.push(RirInst::Eq(dst, lt, zero));
                    value_types.insert(dst, Type::Boolean);
                    sim_stack.push(StackEntry::Value(dst));
                }
                Inst::JumpIfFalse(target) => {
                    let cond_raw = pop_value(&mut sim_stack)?;
                    // When the condition is Any-typed (a Box pointer),
                    // brif would always see the raw pointer (nonzero)
                    // and pick the truthy branch even for Boolean(false).
                    // Insert AnyTruthy to decode the box into a 0/1 i64
                    // per R6RS truthiness (only #f is falsy).
                    let cond = if value_types.get(&cond_raw).copied() == Some(Type::Any) {
                        let fresh = alloc();
                        insts.push(RirInst::AnyTruthy(fresh, cond_raw));
                        value_types.insert(fresh, Type::Boolean);
                        fresh
                    } else {
                        cond_raw
                    };
                    let target_block = lookup_block(&offset_to_block, *target, "JumpIfFalse")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "JumpIfFalse fall")?;
                    // RC3 iter 2.13 — materialize SelfRef / BuiltinRef
                    // markers before the branch so they survive across
                    // blocks as concrete Values. Without this, the
                    // merge successor sees a smaller stack than the
                    // predecessor pushed (`sim_stack_values` strips
                    // markers), and downstream `Call(n)` trips its
                    // "stack has only K entries" invariant.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        fall_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    // JumpIfFalse jumps when cond is falsy. brif: cond truthy -> first, else second.
                    // RC3 iter 2.13 — Term::Branch now carries args
                    // for the target blocks' params, same shape as
                    // Term::Jump. Both successors get the same args
                    // (the pre-branch sim_stack values).
                    let branch_args = sim_stack_values(&sim_stack);
                    term = Some(Term::Branch(cond, fall_block, target_block, branch_args));
                    break;
                }
                Inst::BranchOnGeFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = emit_typed_lt(&mut insts, &mut value_types, &mut alloc, a, b);
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnGeFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnGeFx2 fall")?;
                    // RC3 iter 2.13 — materialize SelfRef / BuiltinRef
                    // markers before the branch so they survive across
                    // blocks as concrete Values. Without this, the
                    // merge successor sees a smaller stack than the
                    // predecessor pushed (`sim_stack_values` strips
                    // markers), and downstream `Call(n)` trips its
                    // "stack has only K entries" invariant.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        fall_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    // RC3 iter 2.13 — Term::Branch now carries args
                    // for the target blocks' params, same shape as
                    // Term::Jump. Both successors get the same args
                    // (the pre-branch sim_stack values).
                    let branch_args = sim_stack_values(&sim_stack);
                    term = Some(Term::Branch(cond, fall_block, target_block, branch_args));
                    break;
                }
                Inst::BranchOnGtFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = emit_typed_lt(&mut insts, &mut value_types, &mut alloc, b, a);
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnGtFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnGtFx2 fall")?;
                    // RC3 iter 2.13 — materialize SelfRef / BuiltinRef
                    // markers before the branch so they survive across
                    // blocks as concrete Values. Without this, the
                    // merge successor sees a smaller stack than the
                    // predecessor pushed (`sim_stack_values` strips
                    // markers), and downstream `Call(n)` trips its
                    // "stack has only K entries" invariant.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        fall_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    let branch_args = sim_stack_values(&sim_stack);
                    term = Some(Term::Branch(cond, target_block, fall_block, branch_args));
                    break;
                }
                Inst::BranchOnLeFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = emit_typed_lt(&mut insts, &mut value_types, &mut alloc, b, a);
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnLeFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnLeFx2 fall")?;
                    // RC3 iter 2.13 — materialize SelfRef / BuiltinRef
                    // markers before the branch so they survive across
                    // blocks as concrete Values. Without this, the
                    // merge successor sees a smaller stack than the
                    // predecessor pushed (`sim_stack_values` strips
                    // markers), and downstream `Call(n)` trips its
                    // "stack has only K entries" invariant.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        fall_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    // RC3 iter 2.13 — Term::Branch now carries args
                    // for the target blocks' params, same shape as
                    // Term::Jump. Both successors get the same args
                    // (the pre-branch sim_stack values).
                    let branch_args = sim_stack_values(&sim_stack);
                    term = Some(Term::Branch(cond, fall_block, target_block, branch_args));
                    break;
                }
                Inst::BranchOnLtFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = emit_typed_lt(&mut insts, &mut value_types, &mut alloc, a, b);
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnLtFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnLtFx2 fall")?;
                    // RC3 iter 2.13 — materialize SelfRef / BuiltinRef
                    // markers before the branch so they survive across
                    // blocks as concrete Values. Without this, the
                    // merge successor sees a smaller stack than the
                    // predecessor pushed (`sim_stack_values` strips
                    // markers), and downstream `Call(n)` trips its
                    // "stack has only K entries" invariant.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        fall_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    let branch_args = sim_stack_values(&sim_stack);
                    term = Some(Term::Branch(cond, target_block, fall_block, branch_args));
                    break;
                }
                Inst::BranchOnNeFx2(target) => {
                    let (a, b) = pop_two_values(&mut sim_stack)?;
                    let cond = emit_typed_eq(&mut insts, &mut value_types, &mut alloc, a, b);
                    let target_block = lookup_block(&offset_to_block, *target, "BranchOnNeFx2")?;
                    let fall_block = lookup_block(&offset_to_block, ip, "BranchOnNeFx2 fall")?;
                    // RC3 iter 2.13 — materialize SelfRef / BuiltinRef
                    // markers before the branch so they survive across
                    // blocks as concrete Values. Without this, the
                    // merge successor sees a smaller stack than the
                    // predecessor pushed (`sim_stack_values` strips
                    // markers), and downstream `Call(n)` trips its
                    // "stack has only K entries" invariant.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        fall_block,
                        &sim_stack_values(&sim_stack),
                    )?;
                    // RC3 iter 2.13 — Term::Branch now carries args
                    // for the target blocks' params, same shape as
                    // Term::Jump. Both successors get the same args
                    // (the pre-branch sim_stack values).
                    let branch_args = sim_stack_values(&sim_stack);
                    term = Some(Term::Branch(cond, fall_block, target_block, branch_args));
                    break;
                }
                Inst::Jump(target) => {
                    let target_block = lookup_block(&offset_to_block, *target, "Jump")?;
                    // RC3 iter 2.13 — same marker-materialization as
                    // JumpIfFalse so the markers flow as Values.
                    materialize_markers_at_branch(
                        &mut sim_stack,
                        &mut insts,
                        &mut value_types,
                        &mut alloc,
                        self_name,
                    )?;
                    // Pull the current stack out as the jump-args.
                    // The first jump to a target seeds its block
                    // params; subsequent jumps must match the count
                    // (well-formed bytecode invariant).
                    let stack_vals: Vec<RirValue> = sim_stack
                        .iter()
                        .map(|e| match e {
                            StackEntry::Value(v) => Ok(*v),
                            StackEntry::SelfRef => Err(TranslateError::Unsupported(
                                "self-ref in Jump-arg position".into(),
                            )),
                            StackEntry::BuiltinRef(_) => Err(TranslateError::Unsupported(
                                "builtin-ref in Jump-arg position".into(),
                            )),
                        })
                        .collect::<Result<_, _>>()?;
                    seed_block_entry(
                        &mut block_entry_stack,
                        &mut block_params,
                        &mut value_types,
                        &mut alloc,
                        target_block,
                        &stack_vals,
                    )?;
                    term = Some(Term::Jump(target_block, stack_vals));
                    break;
                }
                Inst::Return => {
                    let v = pop_value(&mut sim_stack)?;
                    // Drop Any-typed params on every return path
                    // before handing control back to the dispatcher.
                    // The return value `v` is independent (cloned
                    // earlier via AnyClone or produced fresh by
                    // Cons / Car / Cdr), so dropping the original
                    // params is always safe here.
                    for &p in &any_params {
                        insts.push(RirInst::AnyDrop(p));
                    }
                    term = Some(Term::Return(v));
                    break;
                }
                Inst::Call(n) | Inst::TailCall(n) => {
                    if sim_stack.len() < n + 1 {
                        return Err(TranslateError::Invalid(format!(
                            "Call({n}): stack has only {} entries",
                            sim_stack.len()
                        )));
                    }
                    let split = sim_stack.len() - n;
                    let args_entries: Vec<StackEntry> = sim_stack.split_off(split);
                    let callee = sim_stack
                        .pop()
                        .ok_or_else(|| TranslateError::Invalid("Call: missing callee".into()))?;
                    match callee {
                        StackEntry::SelfRef => {
                            let mut args: Vec<RirValue> = Vec::with_capacity(*n);
                            for e in args_entries {
                                match e {
                                    StackEntry::Value(v) => args.push(v),
                                    StackEntry::SelfRef | StackEntry::BuiltinRef(_) => {
                                        return Err(TranslateError::Unsupported(
                                            "non-Value entry as Call arg".into(),
                                        ));
                                    }
                                }
                            }
                            let dst = alloc();
                            insts.push(RirInst::CallSelf(dst, args));
                            sim_stack.push(StackEntry::Value(dst));
                        }
                        StackEntry::BuiltinRef(name) => {
                            // Specialized lowering for known fixnum-only
                            // builtins. Unknown names fall through to
                            // Unsupported.
                            let mut args: Vec<RirValue> = Vec::with_capacity(*n);
                            for e in args_entries {
                                match e {
                                    StackEntry::Value(v) => args.push(v),
                                    _ => {
                                        return Err(TranslateError::Unsupported(
                                            "non-Value entry as builtin arg".into(),
                                        ));
                                    }
                                }
                            }
                            let dst = alloc();
                            // Single-Inst lowerings.
                            let single = match (name, args.len()) {
                                // ADR 0012 D-2 (iter ED) — R7RS division
                                // ops: truncate-quotient/remainder are
                                // aliases for quotient/remainder; floor-
                                // remainder is an alias for modulo;
                                // floor-quotient is a new FloorQuotient
                                // RIR with sdiv+adjust lowering.
                                // ADR 0012 D-2 (iter EL) — integer-only ops
                                // gated on !=Flonum to prevent silent
                                // miscompile when a Flonum operand sneaks
                                // through (e.g., from a parameter). Flonum
                                // operands fall through to the multi-Inst
                                // section (which doesn't handle them either),
                                // then to the unsupported tail → deopt to VM
                                // which gives a proper type-error.
                                ("quotient", 2) | ("truncate-quotient", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Quotient(dst, args[0], args[1]))
                                }
                                ("remainder", 2) | ("truncate-remainder", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Remainder(dst, args[0], args[1]))
                                }
                                ("modulo", 2) | ("floor-remainder", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Modulo(dst, args[0], args[1]))
                                }
                                ("floor-quotient", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::FloorQuotient(dst, args[0], args[1]))
                                }
                                ("gcd", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Gcd(dst, args[0], args[1]))
                                }
                                ("lcm", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Lcm(dst, args[0], args[1]))
                                }
                                ("expt", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Expt(dst, args[0], args[1]))
                                }
                                ("arithmetic-shift", 2) | ("bitwise-arithmetic-shift", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::ArithShift(dst, args[0], args[1]))
                                }
                                ("bitwise-and", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitAnd(dst, args[0], args[1]))
                                }
                                ("bitwise-ior", 2) | ("bitwise-or", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitOr(dst, args[0], args[1]))
                                }
                                ("bitwise-xor", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitXor(dst, args[0], args[1]))
                                }
                                ("bitwise-not", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitNot(dst, args[0]))
                                }
                                // ADR 0012 D-2 (iter FW) — fx bitwise aliases.
                                // R6RS fixnum-only; refuse Flonum and lower to
                                // the same primitives as bitwise-{and,or,xor,not}.
                                ("fxand", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitAnd(dst, args[0], args[1]))
                                }
                                ("fxior", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitOr(dst, args[0], args[1]))
                                }
                                ("fxxor", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitXor(dst, args[0], args[1]))
                                }
                                ("fxnot", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitNot(dst, args[0]))
                                }
                                // ADR 0012 D-2 (iter FW) — fx bit-inspection
                                // aliases (route to vm_bitwise_* helpers).
                                ("fxbit-count", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseBitCount(dst, args[0]))
                                }
                                ("fxlength", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseLength(dst, args[0]))
                                }
                                // ADR 0012 D-2 (iter EB) — abs/max/min are
                                // Fixnum-only on this fast path. If any
                                // operand is Flonum the multi-Inst section
                                // picks them up with FlonumAbs/Max/Min.
                                ("abs", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::AbsFixnum(dst, args[0]))
                                }
                                ("max", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::MaxFixnum(dst, args[0], args[1]))
                                }
                                ("min", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::MinFixnum(dst, args[0], args[1]))
                                }
                                // ADR 0012 D-2 (iter FN) — bitwise-bit-count / -length.
                                // Both Fixnum -> Fixnum. Gated to non-Flonum.
                                ("bitwise-bit-count", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseBitCount(dst, args[0]))
                                }
                                ("bitwise-length", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseLength(dst, args[0]))
                                }
                                // ADR 0012 D-2 (iter FV) — fx arithmetic +
                                // comparison + max/min (Fixnum-only aliases).
                                ("fx+", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Add(dst, args[0], args[1]))
                                }
                                ("fx-", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Sub(dst, args[0], args[1]))
                                }
                                ("fx*", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Mul(dst, args[0], args[1]))
                                }
                                ("fxmax", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::MaxFixnum(dst, args[0], args[1]))
                                }
                                ("fxmin", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::MinFixnum(dst, args[0], args[1]))
                                }
                                ("fx=?", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Eq(dst, args[0], args[1]))
                                }
                                ("fx<?", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Lt(dst, args[0], args[1]))
                                }
                                ("fx>?", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Lt(dst, args[1], args[0]))
                                }
                                // ADR 0012 D-2 (iter FO) — bitwise-arithmetic-shift-{left,right}.
                                ("bitwise-arithmetic-shift-left", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseArithShiftLeft(dst, args[0], args[1]))
                                }
                                ("bitwise-arithmetic-shift-right", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseArithShiftRight(dst, args[0], args[1]))
                                }
                                // ADR 0012 D-2 (iter FX) — fx shift aliases.
                                ("fxarithmetic-shift", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::ArithShift(dst, args[0], args[1]))
                                }
                                ("fxarithmetic-shift-left", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseArithShiftLeft(dst, args[0], args[1]))
                                }
                                ("fxarithmetic-shift-right", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::BitwiseArithShiftRight(dst, args[0], args[1]))
                                }
                                // ADR 0012 D-2 (iter FX) — fxfirst-bit-set.
                                ("fxfirst-bit-set", 1)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::FxFirstBitSet(dst, args[0]))
                                }
                                // ADR 0012 D-2 (iter GE) — R6RS div / mod.
                                // Both 2-arg Fixnum-only; refuse Flonum.
                                ("div", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::DivEuclid(dst, args[0], args[1]))
                                }
                                ("mod", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::ModEuclid(dst, args[0], args[1]))
                                }
                                ("fxdiv", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::DivEuclid(dst, args[0], args[1]))
                                }
                                ("fxmod", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::ModEuclid(dst, args[0], args[1]))
                                }
                                // ADR 0012 D-2 (iter HO) — R6RS div0 / mod0.
                                ("div0", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Div0(dst, args[0], args[1]))
                                }
                                ("mod0", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Mod0(dst, args[0], args[1]))
                                }
                                // ADR 0012 D-2 (iter HP) — R6RS fxdiv0 / fxmod0.
                                // Same numerics, reuse Div0 / Mod0 lowering.
                                ("fxdiv0", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Div0(dst, args[0], args[1]))
                                }
                                ("fxmod0", 2)
                                    if value_types.get(&args[0]).copied() != Some(Type::Flonum)
                                        && value_types.get(&args[1]).copied()
                                            != Some(Type::Flonum) =>
                                {
                                    Some(RirInst::Mod0(dst, args[0], args[1]))
                                }
                                _ => None,
                            };
                            if let Some(inst) = single {
                                insts.push(inst);
                                sim_stack.push(StackEntry::Value(dst));
                            } else {
                                // Multi-Inst lowerings for 1-arg fixnum
                                // predicates that the cs-vm compiler
                                // doesn't have specialized opcodes for.
                                // All produce Boolean (0 or 1 i64).
                                match (name, args.len()) {
                                    // ADR 0012 D-2 (iter EP) — Flonum-typed
                                    // zero?/positive?/negative? use FlonumEq/
                                    // FlonumLt against a 0.0 constant.
                                    ("zero?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Flonum(0.0)));
                                        value_types.insert(zero, Type::Flonum);
                                        insts.push(RirInst::FlonumEq(dst, args[0], zero));
                                    }
                                    ("positive?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Flonum(0.0)));
                                        value_types.insert(zero, Type::Flonum);
                                        insts.push(RirInst::FlonumLt(dst, zero, args[0]));
                                    }
                                    ("negative?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Flonum(0.0)));
                                        value_types.insert(zero, Type::Flonum);
                                        insts.push(RirInst::FlonumLt(dst, args[0], zero));
                                    }
                                    // Fixnum default for zero?/positive?/
                                    // negative? and odd?/even?. odd?/even?
                                    // refuse Flonum (no integer parity for
                                    // f64) → fall through to unsupported.
                                    ("zero?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Eq(dst, args[0], zero));
                                    }
                                    ("positive?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        // x > 0  →  Lt(0, x)
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Lt(dst, zero, args[0]));
                                    }
                                    ("negative?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        // x < 0  →  Lt(x, 0)
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Lt(dst, args[0], zero));
                                    }
                                    // ADR 0012 D-2 (iter FV) — fx<=? and fx>=?.
                                    // Multi-Inst because R6RS 2-arg form needs
                                    // NOT(Lt) — Lt + LoadConst(0) + Eq.
                                    ("fx<=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        // a <= b  ≡  not(b < a)
                                        let lt = alloc();
                                        insts.push(RirInst::Lt(lt, args[1], args[0]));
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                    }
                                    ("fx>=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        // a >= b  ≡  not(a < b)
                                        let lt = alloc();
                                        insts.push(RirInst::Lt(lt, args[0], args[1]));
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                    }
                                    // ADR 0012 D-2 (iter FU) — fx predicate aliases.
                                    // R6RS fixnum-specific; refuse Flonum, lower
                                    // to the same primitives as Fixnum positive?
                                    // / negative? / zero? / even? / odd?.
                                    ("fxzero?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Eq(dst, args[0], zero));
                                    }
                                    ("fxpositive?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Lt(dst, zero, args[0]));
                                    }
                                    ("fxnegative?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Lt(dst, args[0], zero));
                                    }
                                    ("fxeven?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let one = alloc();
                                        insts.push(RirInst::LoadConst(one, Const::Fixnum(1)));
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        let bit = alloc();
                                        insts.push(RirInst::BitAnd(bit, args[0], one));
                                        insts.push(RirInst::Eq(dst, bit, zero));
                                    }
                                    ("fxodd?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let one = alloc();
                                        insts.push(RirInst::LoadConst(one, Const::Fixnum(1)));
                                        let bit = alloc();
                                        insts.push(RirInst::BitAnd(bit, args[0], one));
                                        insts.push(RirInst::Eq(dst, bit, one));
                                    }
                                    ("odd?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        // x & 1 == 1  →  BitAnd then Eq with 1.
                                        let one = alloc();
                                        insts.push(RirInst::LoadConst(one, Const::Fixnum(1)));
                                        let bit = alloc();
                                        insts.push(RirInst::BitAnd(bit, args[0], one));
                                        insts.push(RirInst::Eq(dst, bit, one));
                                    }
                                    ("even?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        // x & 1 == 0
                                        let one = alloc();
                                        insts.push(RirInst::LoadConst(one, Const::Fixnum(1)));
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        let bit = alloc();
                                        insts.push(RirInst::BitAnd(bit, args[0], one));
                                        insts.push(RirInst::Eq(dst, bit, zero));
                                    }
                                    // ADR 0012 D-2 (iter EQ) — type-aware (not x).
                                    // Boolean operand: lower to the dedicated
                                    // `Inst::NotBoolean` so AOT and JIT can
                                    // emit a specialized bit-flip rather
                                    // than re-using numeric Eq (which calls
                                    // generic_cmp2 and FAILS on boolean
                                    // operands, making the previous (not x)
                                    // path return #f for every boolean — the
                                    // bug tak hit on `(not (< y x))`).
                                    ("not", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Boolean) =>
                                    {
                                        insts.push(RirInst::NotBoolean(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // Any operand: AnyTruthy returns NB
                                    // Boolean (true if truthy, false if
                                    // NB_FALSE). NotBoolean flips. Result
                                    // is NB Boolean (NB false if input
                                    // truthy, NB true if input was #f).
                                    //
                                    // RC3 iter 2.16 fix — previously this
                                    // emitted `Eq(truthy, Fixnum(0))`
                                    // which works in JIT i64-boolean mode
                                    // but fails in NB-AOT (Eq routes
                                    // through generic_cmp2 which rejects
                                    // boolean operands). binary-trees's
                                    // `(if (not (car t)) ...)` hit this.
                                    ("not", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let truthy = alloc();
                                        insts.push(RirInst::AnyTruthy(truthy, args[0]));
                                        value_types.insert(truthy, Type::Boolean);
                                        insts.push(RirInst::NotBoolean(dst, truthy));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // Other primitive types (Fixnum,
                                    // Character, Flonum, Symbol, Null): the
                                    // value is never #f, so (not x) is
                                    // always #f. Load preserved for SSA.
                                    ("not", 1) => {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(false)));
                                    }
                                    // Always-true predicates: when the
                                    // arg is a Fixnum (which it always
                                    // is in our i64 ABI), every numeric
                                    // type predicate matches. The JIT
                                    // emits Const(1) and ignores the
                                    // arg — the upstream load of `args[0]`
                                    // is preserved by SSA but unused;
                                    // Cranelift's DCE removes it.
                                    // Always-true predicates: number? / real?
                                    // are correct for both Fixnum and Flonum
                                    // (Flonum is a number and real). exact-X?
                                    // is split out below — Flonums are inexact
                                    // by definition, so all three exact-X?
                                    // predicates return #f for Flonum operand
                                    // (ADR 0012 D-2 iter EI). integer? /
                                    // rational? were split via iter EH.
                                    // ADR 0012 D-2 (iter GD) — complex? and
                                    // real-valued? are aliases of number? in the
                                    // CrabScheme tower (no complex numbers).
                                    ("number?", 1)
                                    | ("real?", 1)
                                    | ("complex?", 1)
                                    | ("real-valued?", 1) => {
                                        let _ = args[0]; // load preserved for SSA correctness
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(true)));
                                    }
                                    // ADR 0012 D-2 (iter EI) — exact-integer?
                                    // and exact-rational? for Fixnum (or
                                    // default) are #t. For Flonum, both are
                                    // #f (flonums are inexact).
                                    // (exact-real? is not a registered
                                    // runtime builtin.)
                                    ("exact-integer?", 1) | ("exact-rational?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(true)));
                                    }
                                    ("exact-integer?", 1) | ("exact-rational?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(false)));
                                    }
                                    // ADR 0012 D-2 (iter HJ) — bytevector=?.
                                    ("bytevector=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BytevectorEqP(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter HK) — vector=?.
                                    ("vector=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::VectorEqP(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter HI) — exact-nonnegative-integer?.
                                    // Flonum is always #f. Otherwise call the
                                    // helper (operand BoxTyped if not Any).
                                    ("exact-nonnegative-integer?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(false)));
                                    }
                                    ("exact-nonnegative-integer?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let arg = if t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::ExactNonNegIntP(dst, arg));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // integer? / rational? gated on
                                    // !=Flonum default to const-true (Fixnum
                                    // is always integer and always rational).
                                    // ADR 0012 D-2 (iter GF) — integer-valued?
                                    // and rational-valued? are aliases of
                                    // integer? and rational? in the CrabScheme
                                    // tower (no complex numbers; Flonum 5.0
                                    // already satisfies integer? per iter EH).
                                    ("integer?", 1)
                                    | ("rational?", 1)
                                    | ("integer-valued?", 1)
                                    | ("rational-valued?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(true)));
                                    }
                                    // ADR 0012 D-2 (iter EH) — Flonum-typed
                                    // integer? and rational?.
                                    ("integer?", 1) | ("integer-valued?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsInteger(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("rational?", 1) | ("rational-valued?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsFinite(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter EE) — exact?/inexact?
                                    // are Fixnum-vs-Flonum sensitive. Other
                                    // types (Character, Boolean, etc.) treat
                                    // exact? as #t (Fixnum default) — non-
                                    // numeric args are caller errors.
                                    ("exact?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let _ = args[0];
                                        let v = !matches!(t, Type::Flonum);
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(v)));
                                    }
                                    ("inexact?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let _ = args[0];
                                        let v = matches!(t, Type::Flonum);
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(v)));
                                    }
                                    ("nan?", 1) | ("infinite?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        // Fixnum / non-flonum: not NaN, not
                                        // infinite.
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(false)));
                                    }
                                    // ADR 0012 D-2 (iter EF) — nan?/infinite?/
                                    // finite? for Flonum via inline fcmp.
                                    ("nan?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsNan(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("infinite?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsInfinite(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("finite?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsFinite(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // finite? for Fixnum / non-flonum: always
                                    // true (per R7RS, exact numbers are
                                    // finite).
                                    ("finite?", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(true)));
                                    }
                                    // Flonum rounding when the arg is
                                    // statically Flonum-typed. Cranelift
                                    // floor/ceil/trunc/nearest do the
                                    // actual rounding on f64 bits.
                                    ("floor", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumFloor(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("ceiling", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumCeil(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("truncate", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumTrunc(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("round", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumRound(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // Identity-on-fixnum rounding ops.
                                    // (floor n), (ceiling n), etc. all
                                    // return n unchanged when n is an
                                    // integer (i.e., a Fixnum here).
                                    ("floor", 1)
                                    | ("ceiling", 1)
                                    | ("truncate", 1)
                                    | ("round", 1)
                                    | ("exact", 1)
                                    | ("inexact->exact", 1) => {
                                        insts.push(RirInst::Move(dst, args[0]));
                                    }
                                    ("square", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        // ADR 0012 D-2 (iter EK) — Flonum
                                        // square via FlonumMul.
                                        insts.push(RirInst::FlonumMul(dst, args[0], args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("square", 1) => {
                                        // (square x) → x * x for Fixnum.
                                        insts.push(RirInst::Mul(dst, args[0], args[0]));
                                    }
                                    ("cons", 2) | ("cons-in-region", 2) => {
                                        // Heap-allocate a Pair via
                                        // vm_alloc_pair. Tags come from
                                        // value_types so the helper
                                        // decodes operands correctly.
                                        // dst is Type::Any (the i64
                                        // carries Box::into_raw(Box<Value>)).
                                        //
                                        // Free-var args may be
                                        // Fixnum-defaulted EnvLookups
                                        // (no `value_types` entry). The
                                        // free var can hold *any* value
                                        // — typically a list cdr like
                                        // n-queens' `placed`. Promote
                                        // each operand's producing
                                        // `EnvLookup` to `EnvLookupAny`
                                        // before tagging, so the cons
                                        // helper sees a real Gc handle
                                        // and not a `vm_env_lookup_fixnum`
                                        // deopt placeholder. Same pattern
                                        // as the CallGeneral callee/arg
                                        // promotions. (Post-M8 Stage 0.)
                                        promote_envlookup_to_any(
                                            &mut insts,
                                            &mut value_types,
                                            args[0],
                                        );
                                        promote_envlookup_to_any(
                                            &mut insts,
                                            &mut value_types,
                                            args[1],
                                        );
                                        let car_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let cdr_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let car_tag = type_to_jit_rt_tag(car_t);
                                        let cdr_tag = type_to_jit_rt_tag(cdr_t);
                                        // Layer 3 — when the typer's
                                        // lifetime-lowering pass rewrote
                                        // a (cons …) site to
                                        // (cons-in-region …), the
                                        // bytecode encodes a call by
                                        // that name. Route it to
                                        // ConsRegion so the JIT/AOT
                                        // codegen emits vm_alloc_pair_region_gc.
                                        if name == "cons-in-region" {
                                            insts.push(RirInst::ConsRegion(
                                                dst, args[0], car_tag, args[1], cdr_tag,
                                            ));
                                        } else {
                                            insts.push(RirInst::Cons(
                                                dst, args[0], car_tag, args[1], cdr_tag,
                                            ));
                                        }
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DO) — variadic vector.
                                    // Box each non-Any arg, then emit VecBuild
                                    // which lowers to a stack-buffer + helper
                                    // call.
                                    ("vector", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::VecBuild(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DP) — variadic string.
                                    // Box each non-Any char arg, then emit
                                    // StrBuild which lowers to a stack-buffer
                                    // + helper call. The helper deopts if any
                                    // arg is not a Character.
                                    ("string", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::StrBuild(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DQ) — variadic bytevector.
                                    // Box each non-Any byte arg, then emit
                                    // BvBuild. The helper masks each Fixnum
                                    // to 8 bits and deopts on non-fixnum.
                                    ("bytevector", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::BvBuild(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DR) — variadic
                                    // string-append. Strings are always
                                    // Any-shape (Gc<Value::String>), so no
                                    // BoxTyped pass is needed; non-Any args
                                    // would be a type error anyway. The
                                    // helper deopts on non-string.
                                    ("string-append", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::StrAppend(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DS) — variadic
                                    // append. Lists / Null are Any-shape;
                                    // last arg can be any value. Box any
                                    // primitives just in case (typically a
                                    // no-op here).
                                    ("append", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::ListAppend(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DT) — variadic
                                    // vector-append. Vectors are always
                                    // Any-shape; uniform BoxTyped fallback.
                                    ("vector-append", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::VecAppend(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DU) — variadic
                                    // bytevector-append. Bytevectors are
                                    // always Any-shape.
                                    ("bytevector-append", _) => {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        insts.push(RirInst::BvAppend(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DN) — variadic list.
                                    // `(list a b c)` lowers to a right-to-left
                                    // chain of cons: cons(a, cons(b, cons(c, '()))).
                                    // The empty list case yields the Null literal.
                                    // Emit the final Cons directly into dst so
                                    // the post-pass's any_values classification
                                    // covers it (Move doesn't propagate types).
                                    ("list", _) => {
                                        if args.is_empty() {
                                            insts.push(RirInst::LoadConst(dst, Const::Null));
                                            value_types.insert(dst, Type::Null);
                                        } else {
                                            // Build innermost tail: '()
                                            let mut acc = alloc();
                                            insts.push(RirInst::LoadConst(acc, Const::Null));
                                            value_types.insert(acc, Type::Null);
                                            let mut acc_tag = type_to_jit_rt_tag(Type::Null);
                                            // Walk args right-to-left, except
                                            // the last (leftmost) which goes
                                            // directly into dst.
                                            for &arg in args[1..].iter().rev() {
                                                let arg_t = value_types
                                                    .get(&arg)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                let arg_tag = type_to_jit_rt_tag(arg_t);
                                                let next = alloc();
                                                insts.push(RirInst::Cons(
                                                    next, arg, arg_tag, acc, acc_tag,
                                                ));
                                                value_types.insert(next, Type::Any);
                                                acc = next;
                                                acc_tag = type_to_jit_rt_tag(Type::Any);
                                            }
                                            // First arg goes into dst.
                                            let first_t = value_types
                                                .get(&args[0])
                                                .copied()
                                                .unwrap_or(Type::Fixnum);
                                            let first_tag = type_to_jit_rt_tag(first_t);
                                            insts.push(RirInst::Cons(
                                                dst, args[0], first_tag, acc, acc_tag,
                                            ));
                                            value_types.insert(dst, Type::Any);
                                        }
                                    }
                                    ("car", 1) => {
                                        // Phase 5 iter4 — promote a free-var
                                        // EnvLookup arg to Any before
                                        // requiring it (e.g., `(car p)` where
                                        // `p` is captured from outer scope).
                                        let arg0_ty = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if arg0_ty != Type::Any {
                                            promote_envlookup_to_any(
                                                &mut insts,
                                                &mut value_types,
                                                args[0],
                                            );
                                        }
                                        if value_types.get(&args[0]).copied() != Some(Type::Any) {
                                            return Err(TranslateError::Unsupported(format!(
                                                "car on non-Any operand (type={:?})",
                                                arg0_ty
                                            )));
                                        }
                                        insts.push(RirInst::Car(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("cdr", 1) => {
                                        let arg0_ty = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if arg0_ty != Type::Any {
                                            promote_envlookup_to_any(
                                                &mut insts,
                                                &mut value_types,
                                                args[0],
                                            );
                                        }
                                        if value_types.get(&args[0]).copied() != Some(Type::Any) {
                                            return Err(TranslateError::Unsupported(format!(
                                                "cdr on non-Any operand (type={:?})",
                                                arg0_ty
                                            )));
                                        }
                                        insts.push(RirInst::Cdr(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DV) — composed pair
                                    // accessors (caar/cadr/.../cddddr). Lower
                                    // to a chain of Car/Cdr RIR insts read
                                    // right-to-left within the c[ad]+r name.
                                    // `(caddr x)` ≡ `(car (cdr (cdr x)))`:
                                    // emit Cdr (rightmost 'd'), then Cdr,
                                    // then Car (leftmost 'a'). Requires the
                                    // arg to be Any-typed; intermediate and
                                    // final values are Any.
                                    // ADR 0012 D-2 (iter EZ) — also handle
                                    // SRFI-1 first/second/third/fourth as
                                    // equivalent cxr names (car, cadr,
                                    // caddr, cadddr).
                                    (n, 1)
                                        if ordinal_to_cxr_dirs(n).is_some()
                                            && value_types.get(&args[0]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let dirs = ordinal_to_cxr_dirs(n).unwrap();
                                        let mut cur = args[0];
                                        let last_i = dirs.len() - 1;
                                        for (i, &is_cdr) in dirs.iter().rev().enumerate() {
                                            let next = if i == last_i { dst } else { alloc() };
                                            if is_cdr {
                                                insts.push(RirInst::Cdr(next, cur));
                                            } else {
                                                insts.push(RirInst::Car(next, cur));
                                            }
                                            value_types.insert(next, Type::Any);
                                            cur = next;
                                        }
                                    }
                                    (n, 1)
                                        if cxr_parse(n).is_some()
                                            && value_types.get(&args[0]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let dirs = cxr_parse(n).unwrap();
                                        let mut cur = args[0];
                                        let last_i = dirs.len() - 1;
                                        for (i, &is_cdr) in dirs.iter().rev().enumerate() {
                                            let next = if i == last_i { dst } else { alloc() };
                                            if is_cdr {
                                                insts.push(RirInst::Cdr(next, cur));
                                            } else {
                                                insts.push(RirInst::Car(next, cur));
                                            }
                                            value_types.insert(next, Type::Any);
                                            cur = next;
                                        }
                                    }
                                    ("pair?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        // Lower to vm_pair_p. The helper
                                        // consumes the operand box, so
                                        // the operand RIR Value must not
                                        // be reused in this body — a
                                        // future iter adds AnyClone to
                                        // support multi-use patterns.
                                        insts.push(RirInst::PairP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("null?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::NullP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter DD) — type predicates
                                    // on Any operand. The bottom-of-table
                                    // "always-false" arms still catch Fixnum-
                                    // tier operands; these gated arms only
                                    // fire for Any.
                                    ("procedure?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ProcedureP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("port?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::PortP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GC) — port-subtype predicates.
                                    // ADR 0012 D-2 (iter GP) — input-port-open? is
                                    // an alias of input-port? because the runtime
                                    // never closes input ports (they're alive until
                                    // GC). R7RS-conformant.
                                    ("input-port?", 1) | ("input-port-open?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::InputPortP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("output-port?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::OutputPortP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("binary-port?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BinaryPortP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("textual-port?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::TextualPortP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GP) — output-port-open?.
                                    ("output-port-open?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::OutputPortOpenP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GQ) — port-eof? +
                                    // port-has-set-port-position!?.
                                    // port-has-port-position? is just port?.
                                    ("port-has-port-position?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::PortP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("port-eof?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::PortEofP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("port-has-set-port-position!?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::PortHasSetPortPositionP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GR) — port-position.
                                    ("port-position", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::PortPosition(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter GD) — promise?.
                                    ("promise?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::PromiseP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GF) — hashtable?.
                                    ("hashtable?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GG) — hashtable-size /
                                    // hashtable-mutable?.
                                    ("hashtable-size", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableSize(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("hashtable-mutable?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableMutableP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter HQ) — hashtable-hash-function.
                                    ("hashtable-hash-function", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableHashFn(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GH) — hashtable-keys/values.
                                    ("hashtable-keys", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableKeys(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("hashtable-values", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableValues(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GI) — hashtable-clear! (1-arg).
                                    ("hashtable-clear!", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableClear(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HW) — hashtable-clear! 2-arg.
                                    // Second operand is an R6RS capacity hint
                                    // that CrabScheme's Vec-backed storage
                                    // ignores. Reuses HashtableClear lowering.
                                    ("hashtable-clear!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let _ = args[1];
                                        insts.push(RirInst::HashtableClear(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GJ) — equal-hash + hashtable->alist.
                                    ("equal-hash", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::EqualHash(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("hashtable->alist", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableToAlist(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GK) — file-exists? + jiffies-per-second.
                                    ("file-exists?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::FileExistsP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("jiffies-per-second", 0) => {
                                        insts.push(RirInst::LoadConst(
                                            dst,
                                            Const::Fixnum(1_000_000_000),
                                        ));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter IW) — fixnum constants.
                                    ("fixnum-width", 0) => {
                                        insts.push(RirInst::LoadConst(dst, Const::Fixnum(64)));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("least-fixnum", 0) => {
                                        insts
                                            .push(RirInst::LoadConst(dst, Const::Fixnum(i64::MIN)));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("greatest-fixnum", 0) => {
                                        insts
                                            .push(RirInst::LoadConst(dst, Const::Fixnum(i64::MAX)));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter GL) — current-second / -jiffy.
                                    ("current-second", 0) => {
                                        insts.push(RirInst::CurrentSecond(dst));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("current-jiffy", 0) => {
                                        insts.push(RirInst::CurrentJiffy(dst));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter HD) — eof-object constructor.
                                    ("eof-object", 0) => {
                                        insts.push(RirInst::EofObject(dst));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HR) — make-hashtable 0-arg.
                                    ("make-hashtable", 0) => {
                                        insts.push(RirInst::MakeHashtableEqual(dst));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HS) — make-eq/eqv-hashtable.
                                    ("make-eq-hashtable", 0) => {
                                        insts.push(RirInst::MakeHashtableEq(dst));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("make-eqv-hashtable", 0) => {
                                        insts.push(RirInst::MakeHashtableEqv(dst));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HY) — make-eq/eqv-hashtable 1-arg.
                                    // R6RS allows an optional capacity hint
                                    // (Vec storage ignores it).
                                    ("make-eq-hashtable", 1) => {
                                        let _ = args[0];
                                        insts.push(RirInst::MakeHashtableEq(dst));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("make-eqv-hashtable", 1) => {
                                        let _ = args[0];
                                        insts.push(RirInst::MakeHashtableEqv(dst));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GN) — append-reverse.
                                    ("append-reverse", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::AppendReverse(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GO) — alist-copy.
                                    ("alist-copy", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::AlistCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GS) — delete + delete-duplicates.
                                    ("delete", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Delete(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("delete-duplicates", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::DeleteDuplicates(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GU) — force (fast path).
                                    // Pending promises deopt to bytecode.
                                    ("force", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ForceForced(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GV) — hashtable-contains?.
                                    // Both operands must be Any; Custom-kind
                                    // hashtables deopt at runtime.
                                    ("hashtable-contains?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableContainsP(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GW) — hashtable-delete!.
                                    // Mutates table; result is Unspecified.
                                    ("hashtable-delete!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableDelete(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GZ) — hashtable-copy.
                                    // 1-arg form (mutable copy).
                                    ("hashtable-copy", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::HashtableCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HX) — hashtable-copy 2-arg.
                                    // Second operand is an R6RS mutability hint
                                    // that CrabScheme's mutable-only hashtables
                                    // ignore. Reuses HashtableCopy lowering.
                                    ("hashtable-copy", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let _ = args[1];
                                        insts.push(RirInst::HashtableCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GY) — hashtable-ref.
                                    // 3-arg; ht and key must be Any; default
                                    // gets BoxTyped if not Any.
                                    ("hashtable-ref", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let dt = value_types
                                            .get(&args[2])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let default_arg = if dt == Type::Any {
                                            args[2]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[2],
                                                type_to_jit_rt_tag(dt),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::HashtableRef(
                                            dst,
                                            args[0],
                                            args[1],
                                            default_arg,
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GX) — hashtable-set!.
                                    // 3-arg mutator; ht/key must be Any.
                                    // Value operand gets BoxTyped if not Any.
                                    ("hashtable-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let vt = value_types
                                            .get(&args[2])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let val_arg = if vt == Type::Any {
                                            args[2]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[2],
                                                type_to_jit_rt_tag(vt),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::HashtableSet(
                                            dst, args[0], args[1], val_arg,
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GT) — make-promise.
                                    // Accepts any operand; BoxTyped if not Any.
                                    ("make-promise", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let boxed = if t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::MakePromise(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter GI) — numerator/denominator
                                    // for Fixnum: numerator is identity, denominator
                                    // is 1.
                                    ("numerator", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        // Identity: copy via Add(0, x)? Simplest: LoadConst+Add.
                                        // Actually we can just alias by Sub(x, 0).
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Add(dst, args[0], zero));
                                    }
                                    ("denominator", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::LoadConst(dst, Const::Fixnum(1)));
                                    }
                                    ("eof-object?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::EofP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("symbol?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::SymbolP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter DE) — immediate-shape
                                    // type predicates. Each does a 3-way
                                    // dispatch on the operand's static type:
                                    // matching-type → const true; Any →
                                    // runtime helper; otherwise → const false.
                                    // These catch-all arms supersede the
                                    // always-true (fixnum?) / always-false
                                    // (char?, boolean?, flonum?) entries in
                                    // the earlier predicate tables for these
                                    // four names.
                                    ("char?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        match t {
                                            Type::Any => {
                                                insts.push(RirInst::CharP(dst, args[0]));
                                            }
                                            Type::Character => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(true),
                                                ));
                                            }
                                            _ => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(false),
                                                ));
                                            }
                                        }
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("boolean?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        match t {
                                            Type::Any => {
                                                insts.push(RirInst::BoolP(dst, args[0]));
                                            }
                                            Type::Boolean => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(true),
                                                ));
                                            }
                                            _ => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(false),
                                                ));
                                            }
                                        }
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("fixnum?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        match t {
                                            Type::Any => {
                                                insts.push(RirInst::FixnumP(dst, args[0]));
                                            }
                                            Type::Fixnum => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(true),
                                                ));
                                            }
                                            _ => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(false),
                                                ));
                                            }
                                        }
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flonum?", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        match t {
                                            Type::Any => {
                                                insts.push(RirInst::FlonumP(dst, args[0]));
                                            }
                                            Type::Flonum => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(true),
                                                ));
                                            }
                                            _ => {
                                                let _ = args[0];
                                                insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Boolean(false),
                                                ));
                                            }
                                        }
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter CA) — list ops.
                                    ("length", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Length(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("list?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ListP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter CB) — reverse.
                                    // ADR 0012 D-2 (iter EW) — reverse! is
                                    // an alias for reverse (the cs-runtime
                                    // builtin doesn't actually mutate; it
                                    // builds a fresh reversed list, same as
                                    // R7RS reverse).
                                    ("reverse", 1) | ("reverse!", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Reverse(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CC) — memq. Both
                                    // args must be Any at the helper boundary;
                                    // typed-immediate items (Symbol literals,
                                    // Fixnums) are boxed first. The list arg
                                    // is required to be Any (came from cons /
                                    // list / env-lookup-any).
                                    ("memq", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let item_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let item = if item_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(item_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::Memq(dst, item, args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CD) — assq. Mirrors
                                    // memq's BoxTyped dance on the key arg.
                                    ("assq", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let key_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let key = if key_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(key_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::Assq(dst, key, args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CG) — memv / assv,
                                    // the eqv?-flavored variants. Same
                                    // BoxTyped dance on the search key.
                                    ("memv", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let item_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let item = if item_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(item_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::Memv(dst, item, args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("assv", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let key_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let key = if key_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(key_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::Assv(dst, key, args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CM) — substring.
                                    // String arg Any, start/end Fixnum. Result
                                    // is a fresh Gc<Value::String>.
                                    ("substring", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Substring(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HV) — string-copy 2-arg slice-to-end.
                                    ("string-copy", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StrCopyFrom(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HB) — string-copy 3-arg
                                    // is identical to substring in R7RS (char-
                                    // based slicing, returns fresh string). Reuse
                                    // the substring lowering.
                                    ("string-copy", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Substring(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CK) — list-tail / list-ref.
                                    // lst Any, index Fixnum. Helpers consume
                                    // the lst handle; index is a raw i64.
                                    ("list-tail", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ListTail(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("list-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ListRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CQ) — bytevector
                                    // read ops. All gated on Any arg.
                                    ("bytevector?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("bytevector-length", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvLength(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("bytevector-u8-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU8Ref(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter FP) — bytevector-s8-ref.
                                    ("bytevector-s8-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS8Ref(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter FQ) — bytevector-u16/s16 native-ref.
                                    ("bytevector-u16-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU16NativeRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("bytevector-s16-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS16NativeRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter FR) — bytevector-u32/s32 native-ref.
                                    ("bytevector-u32-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU32NativeRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("bytevector-s32-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS32NativeRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter FS) — IEEE float native-ref.
                                    ("bytevector-ieee-single-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvIeeeSingleNativeRef(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("bytevector-ieee-double-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvIeeeDoubleNativeRef(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter FT) — bytevector-u64/s64 native-ref.
                                    ("bytevector-u64-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU64NativeRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("bytevector-s64-native-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS64NativeRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter DB) — string-copy /
                                    // vector-copy (1-arg full copy).
                                    ("string-copy", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StrCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("vector-copy", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::VecCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HT) — vector-copy 2-arg slice-to-end.
                                    ("vector-copy", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VecCopyFrom(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HA) — vector-copy 3-arg slice.
                                    // Vector must be Any; start and end must be Fixnum.
                                    ("vector-copy", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VecCopySlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HU) — bytevector-copy 2-arg slice-to-end.
                                    ("bytevector-copy", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BvCopyFrom(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HC) — bytevector-copy 3-arg slice.
                                    ("bytevector-copy", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BvCopySlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DC) — bytevector-copy.
                                    ("bytevector-copy", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DA) — string-set!.
                                    // s Any, k Fixnum, ch Character.
                                    ("string-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::StrSet(dst, args[0], args[1], args[2]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DH) — string-fill!.
                                    // s Any, ch Character. 2-arg form only.
                                    ("string-fill!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::StrFill(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IC) — string-fill! 3-arg fill-from.
                                    ("string-fill!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StrFillFrom(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HH) — string-fill! 4-arg slice.
                                    ("string-fill!", 4)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StrFillSlice(
                                            dst, args[0], args[1], args[2], args[3],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CZ) — vector-fill! /
                                    // bytevector-fill!.
                                    ("vector-fill!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        // fill arg: BoxTyped if not Any.
                                        let f_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let fill = if f_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(f_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::VecFill(dst, args[0], fill));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IB) — vector-fill! 3-arg fill-from.
                                    ("vector-fill!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        // fill arg: BoxTyped if not Any.
                                        let f_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let fill = if f_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(f_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::VecFillFrom(
                                            dst, args[0], fill, args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HG) — vector-fill! 4-arg slice.
                                    ("vector-fill!", 4)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        // fill arg: BoxTyped if not Any.
                                        let f_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let fill = if f_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(f_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::VecFillSlice(
                                            dst, args[0], fill, args[2], args[3],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-fill!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvFill(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HF) — bytevector-fill! 4-arg slice.
                                    // ADR 0012 D-2 (iter IA) — bytevector-fill! 3-arg fill-from.
                                    ("bytevector-fill!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BvFillFrom(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-fill!", 4)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BvFillSlice(
                                            dst, args[0], args[1], args[2], args[3],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CR) — bytevector write ops.
                                    ("make-bytevector", 2) => {
                                        insts.push(RirInst::BvAlloc(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HL) — make-bytevector 1-arg.
                                    // Fill defaults to 0; reuse BvAlloc with a
                                    // synthesized zero.
                                    ("make-bytevector", 1) => {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        value_types.insert(zero, Type::Fixnum);
                                        insts.push(RirInst::BvAlloc(dst, args[0], zero));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-u8-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts
                                            .push(RirInst::BvU8Set(dst, args[0], args[1], args[2]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FP) — bytevector-s8-set!.
                                    ("bytevector-s8-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts
                                            .push(RirInst::BvS8Set(dst, args[0], args[1], args[2]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FQ) — bytevector-u16/s16 native-set!.
                                    ("bytevector-u16-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU16NativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-s16-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS16NativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FR) — bytevector-u32/s32 native-set!.
                                    ("bytevector-u32-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU32NativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-s32-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS32NativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FS) — IEEE float native-set!.
                                    // Gated on Flonum value so the operand is
                                    // already an f64 bit pattern at the call site.
                                    ("bytevector-ieee-single-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::BvIeeeSingleNativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-ieee-double-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::BvIeeeDoubleNativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FT) — bytevector-u64/s64 native-set!.
                                    ("bytevector-u64-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvU64NativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-s64-native-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvS64NativeSet(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CN) — list-copy.
                                    ("list-copy", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ListCopy(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CO) — list-set!. lst
                                    // Any, n Fixnum, val gets BoxTyped if it's
                                    // a typed immediate.
                                    ("list-set!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let v_t = value_types
                                            .get(&args[2])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let val = if v_t == Type::Any {
                                            args[2]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[2],
                                                type_to_jit_rt_tag(v_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::ListSet(dst, args[0], args[1], val));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CH) — member / assoc,
                                    // the equal?-flavored variants. Same
                                    // BoxTyped dance on the search key.
                                    // Only the 2-arg form; the optional
                                    // 3-arg (user-supplied equiv proc) is
                                    // out of scope for this iter.
                                    ("member", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let item_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let item = if item_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(item_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::Member(dst, item, args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("assoc", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let key_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let key = if key_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(key_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::Assoc(dst, key, args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CE) — pair mutation.
                                    // Pair arg must be Any; value arg gets
                                    // BoxTyped if it's a typed immediate.
                                    ("set-car!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let v_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let val = if v_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(v_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::SetCar(dst, args[0], val));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("set-cdr!", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let v_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let val = if v_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(v_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::SetCdr(dst, args[0], val));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter BV) — vector ops.
                                    // make-vector requires the fill to be
                                    // Any. If the user passed a typed
                                    // immediate (e.g. Fixnum), box it via
                                    // BoxTyped first.
                                    // ADR 0012 D-2 (iter JE) — (make-vector n) 1-arg.
                                    // RC3 iter 2.18 — accept Any-typed n too.
                                    ("make-vector", 1)
                                        if matches!(
                                            value_types.get(&args[0]).copied(),
                                            Some(Type::Fixnum) | Some(Type::Any)
                                        ) =>
                                    {
                                        insts.push(RirInst::MakeVectorUnspec(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("make-vector", 2) => {
                                        let len_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let fill_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        // RC3 iter 2.18 — accept Any-typed
                                        // length too. The cs-aot VecAlloc
                                        // lowering decodes NB carriers
                                        // (Fixnum payload extraction) so
                                        // raw length works regardless of
                                        // tracked type. Previously we
                                        // bailed on non-Fixnum, blocking
                                        // spectral-norm's
                                        // (make-vector n 1.0) where n
                                        // defaults to Any (iter 2.16).
                                        if len_t != Type::Fixnum && len_t != Type::Any {
                                            return Err(TranslateError::Unsupported(format!(
                                                "make-vector: length must be Fixnum or Any (got {len_t:?})"
                                            )));
                                        }
                                        let fill = if fill_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(fill_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::VecAlloc(dst, args[0], fill));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("vector-ref", 2) => {
                                        // Phase 5 iter3 — try promoting
                                        // a free-var EnvLookup arg0 to
                                        // Any before requiring it. This
                                        // lets `(vector-ref v i)` where
                                        // `v` is a captured vector work
                                        // (previously rejected when v
                                        // defaulted to Fixnum).
                                        let arg0_ty = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if arg0_ty != Type::Any {
                                            promote_envlookup_to_any(
                                                &mut insts,
                                                &mut value_types,
                                                args[0],
                                            );
                                        }
                                        if value_types.get(&args[0]).copied() != Some(Type::Any) {
                                            return Err(TranslateError::Unsupported(format!(
                                                "vector-ref on non-Any operand (type={:?})",
                                                arg0_ty
                                            )));
                                        }
                                        insts.push(RirInst::VecRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("vector-set!", 3) => {
                                        // Phase 5 iter3 — promote arg0
                                        // from EnvLookup Fixnum default
                                        // to Any if needed.
                                        let arg0_ty = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if arg0_ty != Type::Any {
                                            promote_envlookup_to_any(
                                                &mut insts,
                                                &mut value_types,
                                                args[0],
                                            );
                                        }
                                        if value_types.get(&args[0]).copied() != Some(Type::Any) {
                                            return Err(TranslateError::Unsupported(format!(
                                                "vector-set! on non-Any operand (type={:?})",
                                                arg0_ty
                                            )));
                                        }
                                        // Box the value-to-store if it's
                                        // a typed immediate.
                                        let v_t = value_types
                                            .get(&args[2])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let val = if v_t == Type::Any {
                                            args[2]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[2],
                                                type_to_jit_rt_tag(v_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::VecSet(dst, args[0], args[1], val));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("vector-length", 1) => {
                                        let arg0_ty = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if arg0_ty != Type::Any {
                                            promote_envlookup_to_any(
                                                &mut insts,
                                                &mut value_types,
                                                args[0],
                                            );
                                        }
                                        if value_types.get(&args[0]).copied() != Some(Type::Any) {
                                            return Err(TranslateError::Unsupported(format!(
                                                "vector-length on non-Any operand (type={:?})",
                                                arg0_ty
                                            )));
                                        }
                                        insts.push(RirInst::VecLength(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("vector?", 1) => {
                                        let arg0_ty = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if arg0_ty != Type::Any {
                                            promote_envlookup_to_any(
                                                &mut insts,
                                                &mut value_types,
                                                args[0],
                                            );
                                        }
                                        if value_types.get(&args[0]).copied() != Some(Type::Any) {
                                            // For non-Any non-promoted
                                            // operands the type is statically
                                            // known and the answer is false
                                            // (except for Any, handled above).
                                            insts.push(RirInst::LoadConst(
                                                dst,
                                                Const::Boolean(false),
                                            ));
                                            value_types.insert(dst, Type::Boolean);
                                        } else {
                                            insts.push(RirInst::VecP(dst, args[0]));
                                            value_types.insert(dst, Type::Boolean);
                                        }
                                    }
                                    // ADR 0012 D-2 (iter BX) — string ops.
                                    // make-string requires the fill argument
                                    // to be Character-typed (the helper
                                    // expects a codepoint i64 in
                                    // JIT_RT_CHARACTER carrier shape).
                                    ("make-string", 2) => {
                                        let len_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let fill_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if len_t != Type::Fixnum {
                                            return Err(TranslateError::Unsupported(
                                                "make-string: length must be Fixnum-typed at JIT translate"
                                                    .into(),
                                            ));
                                        }
                                        if fill_t != Type::Character {
                                            return Err(TranslateError::Unsupported(
                                                "make-string: fill must be Character-typed at JIT translate"
                                                    .into(),
                                            ));
                                        }
                                        insts.push(RirInst::StrAlloc(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HM) — make-string 1-arg.
                                    // Fill defaults to #\space; reuse StrAlloc
                                    // with a synthesized space character.
                                    ("make-string", 1) => {
                                        let len_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        if len_t != Type::Fixnum {
                                            return Err(TranslateError::Unsupported(
                                                "make-string: length must be Fixnum-typed at JIT translate"
                                                    .into(),
                                            ));
                                        }
                                        let space = alloc();
                                        insts
                                            .push(RirInst::LoadConst(space, Const::Character(' ')));
                                        value_types.insert(space, Type::Character);
                                        insts.push(RirInst::StrAlloc(dst, args[0], space));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-ref", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        // s Any (consumed), idx Fixnum.
                                        // dst is Character — the dispatcher
                                        // decodes the i64 codepoint via
                                        // JIT_RT_CHARACTER.
                                        insts.push(RirInst::StrRef(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Character);
                                    }
                                    ("string-length", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StrLength(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("string?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StrP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // string=? mirrors the eq? Any-arg
                                    // pattern: if either side is non-Any,
                                    // box it via BoxTyped first.
                                    ("string=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            || value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let lhs_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let rhs_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let lhs = if lhs_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(lhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        let rhs = if rhs_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(rhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::StrEq(dst, lhs, rhs));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter DW) — ordered string
                                    // comparisons. Same BoxTyped-fallback
                                    // pattern as string=?.
                                    // ADR 0012 D-2 (iter DX) — string-ci
                                    // family with same dispatch shape.
                                    // ADR 0012 D-2 (iter JD) — variadic string
                                    // comparisons. Box each arg if not Any,
                                    // pairwise compare via Str* RIR + BitAnd
                                    // chain. Covers string=? plus ordered and
                                    // string-ci families.
                                    ("string=?", n)
                                    | ("string<?", n)
                                    | ("string>?", n)
                                    | ("string<=?", n)
                                    | ("string>=?", n)
                                    | ("string-ci=?", n)
                                    | ("string-ci<?", n)
                                    | ("string-ci>?", n)
                                    | ("string-ci<=?", n)
                                    | ("string-ci>=?", n)
                                        if n >= 3 =>
                                    {
                                        let boxed: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let t = value_types
                                                    .get(v)
                                                    .copied()
                                                    .unwrap_or(Type::Fixnum);
                                                if t == Type::Any {
                                                    *v
                                                } else {
                                                    let fresh = alloc();
                                                    insts.push(RirInst::BoxTyped(
                                                        fresh,
                                                        *v,
                                                        type_to_jit_rt_tag(t),
                                                    ));
                                                    value_types.insert(fresh, Type::Any);
                                                    fresh
                                                }
                                            })
                                            .collect();
                                        let make_cmp =
                                            |d: RirValue, a: RirValue, b: RirValue| match name {
                                                "string=?" => RirInst::StrEq(d, a, b),
                                                "string<?" => RirInst::StrLt(d, a, b),
                                                "string>?" => RirInst::StrGt(d, a, b),
                                                "string<=?" => RirInst::StrLe(d, a, b),
                                                "string>=?" => RirInst::StrGe(d, a, b),
                                                "string-ci=?" => RirInst::StrCiEq(d, a, b),
                                                "string-ci<?" => RirInst::StrCiLt(d, a, b),
                                                "string-ci>?" => RirInst::StrCiGt(d, a, b),
                                                "string-ci<=?" => RirInst::StrCiLe(d, a, b),
                                                "string-ci>=?" => RirInst::StrCiGe(d, a, b),
                                                _ => unreachable!(),
                                            };
                                        let first = alloc();
                                        insts.push(make_cmp(first, boxed[0], boxed[1]));
                                        value_types.insert(first, Type::Boolean);
                                        let mut acc = first;
                                        for i in 1..boxed.len() - 1 {
                                            let cmp = alloc();
                                            insts.push(make_cmp(cmp, boxed[i], boxed[i + 1]));
                                            value_types.insert(cmp, Type::Boolean);
                                            let new_acc = alloc();
                                            insts.push(RirInst::BitAnd(new_acc, acc, cmp));
                                            value_types.insert(new_acc, Type::Boolean);
                                            acc = new_acc;
                                        }
                                        insts.push(RirInst::Move(dst, acc));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("string<?", 2)
                                    | ("string>?", 2)
                                    | ("string<=?", 2)
                                    | ("string>=?", 2)
                                    | ("string-ci=?", 2)
                                    | ("string-ci<?", 2)
                                    | ("string-ci>?", 2)
                                    | ("string-ci<=?", 2)
                                    | ("string-ci>=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            || value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let lhs_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let rhs_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let lhs = if lhs_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(lhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        let rhs = if rhs_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(rhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        let inst = match name {
                                            "string<?" => RirInst::StrLt(dst, lhs, rhs),
                                            "string>?" => RirInst::StrGt(dst, lhs, rhs),
                                            "string<=?" => RirInst::StrLe(dst, lhs, rhs),
                                            "string>=?" => RirInst::StrGe(dst, lhs, rhs),
                                            "string-ci=?" => RirInst::StrCiEq(dst, lhs, rhs),
                                            "string-ci<?" => RirInst::StrCiLt(dst, lhs, rhs),
                                            "string-ci>?" => RirInst::StrCiGt(dst, lhs, rhs),
                                            "string-ci<=?" => RirInst::StrCiLe(dst, lhs, rhs),
                                            "string-ci>=?" => RirInst::StrCiGe(dst, lhs, rhs),
                                            _ => unreachable!(),
                                        };
                                        insts.push(inst);
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("integer->char", 1) => {
                                        // Same bit pattern as the Fixnum input;
                                        // the return-type post-pass will tag
                                        // dst as Character so the dispatcher
                                        // decodes the i64 codepoint into a
                                        // Value::Character on the way out.
                                        insts.push(RirInst::IntCharBitcast(dst, args[0]));
                                        // Track Character in the inline
                                        // value_types map so downstream
                                        // arms (char-alphabetic? etc.) that
                                        // gate on Type::Character can fire.
                                        value_types.insert(dst, Type::Character);
                                    }
                                    ("real->flonum", 1)
                                    | ("exact->inexact", 1)
                                    | ("inexact", 1)
                                    | ("fixnum->flonum", 1) => {
                                        // Convert the i64 Fixnum into f64
                                        // bits via Cranelift's
                                        // fcvt_from_sint+bitcast. The
                                        // return-type post-pass tags dst as
                                        // Flonum; dispatcher decodes via
                                        // f64::from_bits.
                                        insts.push(RirInst::FixToFlo(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // Flonum unary builtins. Only fire when
                                    // the operand is statically Flonum-
                                    // typed, otherwise fall through to the
                                    // unsupported tail (deopt to bytecode).
                                    ("flsqrt", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumSqrt(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter EA) — sqrt for typed
                                    // numeric args. Result is always Flonum
                                    // (R7RS: unary_flonum semantics — the
                                    // runtime promotes fixnum to flonum before
                                    // sqrt).
                                    ("sqrt", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumSqrt(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("sqrt", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Fixnum) =>
                                    {
                                        let promoted = alloc();
                                        insts.push(RirInst::FixToFlo(promoted, args[0]));
                                        value_types.insert(promoted, Type::Flonum);
                                        insts.push(RirInst::FlonumSqrt(dst, promoted));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // RC3 iter 2.18 — sqrt for Any-typed args.
                                    // cs-aot's FlonumSqrt lowering is
                                    // NB-Fixnum-aware (iter 2.17) so we can
                                    // hand the Any operand directly to
                                    // FlonumSqrt; the lowering will decode
                                    // NB Fixnum payload to f64 or use the
                                    // raw f64 bits for NB Flonum. Spectral-
                                    // norm's `(sqrt (/ vBv vv))` hits this.
                                    ("sqrt", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::FlonumSqrt(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flabs", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAbs(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter FZ) — fl trig/exp/log/
                                    // round/predicate aliases (Flonum-only).
                                    ("flsin", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumSin(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flcos", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumCos(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fltan", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumTan(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flexp", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumExp(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fllog", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumLog(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flfloor", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumFloor(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flceiling", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumCeil(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fltruncate", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumTrunc(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flround", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumRound(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flfinite?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsFinite(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flinfinite?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsInfinite(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flinteger?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsInteger(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flnan?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumIsNan(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter GB) — string-titlecase /
                                    // string-hash / symbol-hash.
                                    ("string-titlecase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringTitlecase(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-hash", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringHash(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    ("symbol-hash", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::SymbolHash(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter EB) — abs/max/min for
                                    // Flonum-typed args. max/min widen any
                                    // Fixnum operand to Flonum via FixToFlo
                                    // (numeric-tower contagion).
                                    ("abs", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAbs(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("max", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            || value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        let lhs = if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                        {
                                            args[0]
                                        } else {
                                            let p = alloc();
                                            insts.push(RirInst::FixToFlo(p, args[0]));
                                            value_types.insert(p, Type::Flonum);
                                            p
                                        };
                                        let rhs = if value_types.get(&args[1]).copied()
                                            == Some(Type::Flonum)
                                        {
                                            args[1]
                                        } else {
                                            let p = alloc();
                                            insts.push(RirInst::FixToFlo(p, args[1]));
                                            value_types.insert(p, Type::Flonum);
                                            p
                                        };
                                        insts.push(RirInst::FlonumMax(dst, lhs, rhs));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("min", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            || value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        let lhs = if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                        {
                                            args[0]
                                        } else {
                                            let p = alloc();
                                            insts.push(RirInst::FixToFlo(p, args[0]));
                                            value_types.insert(p, Type::Flonum);
                                            p
                                        };
                                        let rhs = if value_types.get(&args[1]).copied()
                                            == Some(Type::Flonum)
                                        {
                                            args[1]
                                        } else {
                                            let p = alloc();
                                            insts.push(RirInst::FixToFlo(p, args[1]));
                                            value_types.insert(p, Type::Flonum);
                                            p
                                        };
                                        insts.push(RirInst::FlonumMin(dst, lhs, rhs));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter DF) — flonum
                                    // transcendentals. Gated on Flonum operand.
                                    ("sin", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumSin(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("cos", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumCos(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("tan", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumTan(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("log", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumLog(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("exp", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumExp(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter DG) — inverse trig.
                                    ("asin", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAsin(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("acos", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAcos(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("atan", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAtan(dst, args[0]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter FM) — log 2-arg, atan 2-arg.
                                    ("log", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumLog2(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("atan", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAtan2(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter GA) — flexpt + fleven?/flodd?.
                                    ("flexpt", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumExpt(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fleven?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlEvenP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flodd?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlOddP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flmax", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumMax(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("flmin", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumMin(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    // ADR 0012 D-2 (iter FY) — fl arithmetic +
                                    // compare + predicate aliases.
                                    ("fl+", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumAdd(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fl-", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumSub(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fl*", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumMul(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fl/", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumDiv(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Flonum);
                                    }
                                    ("fl=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumEq(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("fl<?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumLt(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("fl>?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::FlonumLt(dst, args[1], args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("fl<=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        // a <= b ≡ not(b < a)
                                        let lt = alloc();
                                        insts.push(RirInst::FlonumLt(lt, args[1], args[0]));
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("fl>=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Flonum) =>
                                    {
                                        let lt = alloc();
                                        insts.push(RirInst::FlonumLt(lt, args[0], args[1]));
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flzero?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Flonum(0.0)));
                                        value_types.insert(zero, Type::Flonum);
                                        insts.push(RirInst::FlonumEq(dst, args[0], zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flpositive?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Flonum(0.0)));
                                        value_types.insert(zero, Type::Flonum);
                                        insts.push(RirInst::FlonumLt(dst, zero, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("flnegative?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Flonum) =>
                                    {
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Flonum(0.0)));
                                        value_types.insert(zero, Type::Flonum);
                                        insts.push(RirInst::FlonumLt(dst, args[0], zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char->integer", 1) => {
                                        // Inverse of integer->char: same
                                        // codepoint payload, but the result is
                                        // a Fixnum. Emit the dedicated
                                        // `CharToInt` retag (not `Move`) so the
                                        // tagged uniform-NB tier produces a
                                        // Fixnum-tagged NB carrier — a `Move`
                                        // left it Character-tagged and
                                        // miscompiled `(char->integer
                                        // (integer->char x))` → Character. The
                                        // untagged tiers still treat it as a
                                        // no-op copy. dst stays Fixnum-typed.
                                        insts.push(RirInst::CharToInt(dst, args[0]));
                                        value_types.insert(dst, Type::Fixnum);
                                    }
                                    // ADR 0012 D-2 (iter CI) — char Unicode
                                    // predicates. Gated on Character-typed
                                    // operand. Operand stays in its Fixnum-
                                    // shape codepoint lane; helper dispatches
                                    // via `char::from_u32(...).map_or(0, ...)`
                                    // so invalid codepoints simply return 0
                                    // (no deopt).
                                    // ADR 0012 D-2 (iter FO) — bitwise-bit-set?.
                                    // (Fixnum, Fixnum) -> Boolean.
                                    ("bitwise-bit-set?", 2)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::BitwiseBitSetP(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter FW) — fxbit-set? alias.
                                    ("fxbit-set?", 2)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::BitwiseBitSetP(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-alphabetic?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharAlphabeticP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-numeric?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharNumericP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-whitespace?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharWhitespaceP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter CJ) — char case ops.
                                    // Same Character-gated dispatch as CI.
                                    ("char-upcase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharUpcase(dst, args[0]));
                                        value_types.insert(dst, Type::Character);
                                    }
                                    ("char-downcase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharDowncase(dst, args[0]));
                                        value_types.insert(dst, Type::Character);
                                    }
                                    ("char-upper-case?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharUpperCaseP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-lower-case?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharLowerCaseP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter CS) — char-foldcase /
                                    // char-titlecase. Same Character-gated
                                    // shape as char-upcase / char-downcase.
                                    ("char-foldcase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharFoldcase(dst, args[0]));
                                        value_types.insert(dst, Type::Character);
                                    }
                                    ("char-titlecase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::CharTitlecase(dst, args[0]));
                                        value_types.insert(dst, Type::Character);
                                    }
                                    // ADR 0012 D-2 (iter CW) — vector->list /
                                    // list->vector. 1-arg forms.
                                    ("vector->list", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::VectorToList(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IL) — vector->list 2-arg slice-from.
                                    ("vector->list", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VectorToListSliceFrom(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IF) — vector->list 3-arg slice.
                                    ("vector->list", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VectorToListSlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("list->vector", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ListToVector(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CY) — symbol<->string.
                                    ("symbol->string", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Symbol) =>
                                    {
                                        insts.push(RirInst::SymbolToString(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string->symbol", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringToSymbol(dst, args[0]));
                                        value_types.insert(dst, Type::Symbol);
                                    }
                                    // ADR 0012 D-2 (iter CX) — string<->list.
                                    ("string->list", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringToList(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IM) — string->list 2-arg slice-from.
                                    ("string->list", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StringToListSliceFrom(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IG) — string->list 3-arg slice.
                                    ("string->list", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StringToListSlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IN) — bytevector->list 2-arg slice-from.
                                    ("bytevector->list", 2) | ("bytevector->u8-list", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BytevectorToListSliceFrom(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IH) — bytevector->list 3-arg slice.
                                    ("bytevector->list", 3) | ("bytevector->u8-list", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BytevectorToListSlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("list->string", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ListToString(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EJ) — string-reverse.
                                    ("string-reverse", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringReverse(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EU) — string-contains.
                                    ("string-contains", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringContains(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FL) — bytevector/utf8 conversion.
                                    // ADR 0012 D-2 (iter GM) — bytevector->list /
                                    // list->bytevector (1-arg) are R7RS aliases of
                                    // bytevector->u8-list / u8-list->bytevector.
                                    ("bytevector->u8-list", 1) | ("bytevector->list", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BytevectorToU8List(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("u8-list->bytevector", 1) | ("list->bytevector", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::U8ListToBytevector(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string->utf8", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringToUtf8(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("utf8->string", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Utf8ToString(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FK) — string-contains-right.
                                    ("string-contains-right", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringContainsRight(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FK) — string-index/-right.
                                    // arg[1] is Character (raw codepoint i64).
                                    ("string-index", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::StringIndex(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-index-right", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        insts
                                            .push(RirInst::StringIndexRight(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EV) — string-prefix?/suffix?.
                                    ("string-prefix?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringPrefixP(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter FE) — string-join 2-arg.
                                    ("string-join", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringJoin(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FI) — string-replace-all.
                                    ("string-replace-all", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringReplaceAll(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter HE) — string-replace
                                    // (first occurrence only).
                                    ("string-replace", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringReplaceFirst(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FH) — string trim family.
                                    ("string-trim", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringTrim(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-trim-left", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringTrimLeft(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-trim-right", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringTrimRight(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FG) — string-pad/string-pad-right 2-arg.
                                    ("string-pad", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::StringPad(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-pad-right", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::StringPadRight(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FJ) — string-take/-drop/
                                    // -take-right/-drop-right. All take (String,
                                    // Fixnum) and return a fresh String.
                                    ("string-take", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::StringTake(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-drop", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::StringDrop(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-take-right", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::StringTakeRight(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-drop-right", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::StringDropRight(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FF) — string-split 2-arg.
                                    // sep may be String (Any) or Character;
                                    // BoxTyped if Character so the helper sees
                                    // a Gc<Value> uniformly.
                                    ("string-split", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        let sep_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let sep = if sep_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(sep_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::StringSplit(dst, args[0], sep));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-suffix?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringSuffixP(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter ET) — string case
                                    // conversions: upcase / downcase / foldcase.
                                    ("string-upcase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringUpcase(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-downcase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringDowncase(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-foldcase", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringFoldcase(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EN) — (iota n) 1-arg.
                                    // n must be Fixnum-shape (not Flonum).
                                    ("iota", 1)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::IotaN(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FC) — (iota count start).
                                    ("iota", 2)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::IotaNs(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FD) — (iota count start step).
                                    ("iota", 3)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum)
                                            && value_types.get(&args[2]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts
                                            .push(RirInst::IotaNss(dst, args[0], args[1], args[2]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter ER) — vector-copy!
                                    // 3-arg form. (ES) — same shape for
                                    // bytevector-copy! and string-copy!.
                                    ("vector-copy!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::VecCopyBang(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IQ) — vector-copy! 4-arg.
                                    ("vector-copy!", 4)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VecCopyBangFrom(
                                            dst, args[0], args[1], args[2], args[3],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IT) — vector-copy! 5-arg.
                                    ("vector-copy!", 5)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[4]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VecCopyBangSlice(
                                            dst, args[0], args[1], args[2], args[3], args[4],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IR) — bytevector-copy! 4-arg.
                                    ("bytevector-copy!", 4)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BvCopyBangFrom(
                                            dst, args[0], args[1], args[2], args[3],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IU) — bytevector-copy! 5-arg.
                                    ("bytevector-copy!", 5)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[4]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::BvCopyBangSlice(
                                            dst, args[0], args[1], args[2], args[3], args[4],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IS) — string-copy! 4-arg.
                                    ("string-copy!", 4)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StrCopyBangFrom(
                                            dst, args[0], args[1], args[2], args[3],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IV) — string-copy! 5-arg.
                                    ("string-copy!", 5)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any)
                                            && value_types.get(&args[3]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[4]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StrCopyBangSlice(
                                            dst, args[0], args[1], args[2], args[3], args[4],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("bytevector-copy!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::BvCopyBang(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string-copy!", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StrCopyBang(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EO) — last-pair / last.
                                    ("last-pair", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::LastPair(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter FB) — concatenate / not-pair?.
                                    ("concatenate", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Concatenate(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("not-pair?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::NotPairP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter EY) — SRFI-1 list classifiers.
                                    ("null-list?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::NullListP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("proper-list?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::ProperListP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("dotted-list?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::DottedListP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("circular-list?", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::CircularListP(dst, args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter EX) — take / drop.
                                    // ADR 0012 D-2 (iter GC) — list-head alias.
                                    // Both R6RS list-head and SRFI-1 take fail
                                    // when n exceeds list length; we deopt to
                                    // bytecode in either case.
                                    ("take", 2) | ("list-head", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::Take(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("drop", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                != Some(Type::Flonum) =>
                                    {
                                        insts.push(RirInst::Drop(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("last", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::Last(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IK) — (make-list n) 1-arg.
                                    ("make-list", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::MakeListUnspec(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EM) — (make-list n fill).
                                    // Length must be Fixnum-typed; fill is
                                    // boxed if a typed primitive.
                                    ("make-list", 2)
                                        if value_types.get(&args[0]).copied()
                                            != Some(Type::Flonum) =>
                                    {
                                        let fill_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let fill = if fill_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(fill_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::MakeList(dst, args[0], fill));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter DY) — string<->vector
                                    // 1-arg forms.
                                    ("string->vector", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringToVector(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("vector->string", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::VectorToString(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IE) — string->vector 3-arg slice.
                                    // Strict Fixnum guards on start/end avoid
                                    // the JIT call-site cache issue that bit
                                    // iter HN.
                                    ("string->vector", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StringToVectorSlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IP) — string->vector 2-arg slice-from.
                                    ("string->vector", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StringToVectorSliceFrom(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter ID) — vector->string 3-arg slice.
                                    ("vector->string", 3)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum)
                                            && value_types.get(&args[2]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VectorToStringSlice(
                                            dst, args[0], args[1], args[2],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IO) — vector->string 2-arg slice-from.
                                    ("vector->string", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::VectorToStringSliceFrom(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter EC) — number<->string
                                    // 1-arg forms. Box typed numeric immediates
                                    // first; string->number's arg is always Any.
                                    ("number->string", 1) => {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let boxed = if t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::NumberToString(dst, boxed));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter II) — number->string 2-arg radix.
                                    ("number->string", 2)
                                        if value_types.get(&args[1]).copied()
                                            == Some(Type::Fixnum) =>
                                    {
                                        let t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let boxed = if t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::NumberToStringRadix(
                                            dst, boxed, args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    ("string->number", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any) =>
                                    {
                                        insts.push(RirInst::StringToNumber(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter IJ) — string->number 2-arg radix.
                                    ("string->number", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Fixnum) =>
                                    {
                                        insts.push(RirInst::StringToNumberRadix(
                                            dst, args[0], args[1],
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // ADR 0012 D-2 (iter CV) — digit-value.
                                    // Mixed return (Fixnum or #f) so dst is Any.
                                    ("digit-value", 1)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character) =>
                                    {
                                        insts.push(RirInst::DigitValue(dst, args[0]));
                                        value_types.insert(dst, Type::Any);
                                    }
                                    // R6RS tagged-equality on small immediates.
                                    // For Fixnum/Boolean/Character all three
                                    // live in the same i64 register, so an
                                    // `Eq` instruction (which is i64 cmp)
                                    // matches Scheme semantics. Each name
                                    // lowers to the same RIR op; the
                                    // type-guard at dispatch ensures both
                                    // args are i64-shaped before we enter.
                                    // `eq?` / `eqv?` on Any operands routes
                                    // through vm_eq_any (consume-on-use
                                    // identity check). Both operands must be
                                    // Box pointers; if one side is a typed
                                    // immediate (Fixnum / Boolean / Symbol /
                                    // ...) we wrap it first via BoxTyped.
                                    // ADR 0012 D-2 (iter EG) — extend the Any-
                                    // arg eq routing to boolean=?/char=?/
                                    // symbol=?. Closes a latent gap where
                                    // integer Eq would compare Gc pointers
                                    // instead of inner values when either side
                                    // is Any-shape.
                                    ("eq?", 2)
                                    | ("eqv?", 2)
                                    | ("boolean=?", 2)
                                    | ("char=?", 2)
                                    | ("symbol=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Any)
                                            || value_types.get(&args[1]).copied()
                                                == Some(Type::Any) =>
                                    {
                                        let lhs_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let rhs_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let lhs = if lhs_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(lhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        let rhs = if rhs_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(rhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::EqAny(dst, lhs, rhs));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter DZ) — equal? deep
                                    // structural equality. Same BoxTyped
                                    // fallback as eq?/eqv?; helper defers to
                                    // cs_core::eq::equal.
                                    ("equal?", 2) => {
                                        let lhs_t = value_types
                                            .get(&args[0])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let rhs_t = value_types
                                            .get(&args[1])
                                            .copied()
                                            .unwrap_or(Type::Fixnum);
                                        let lhs = if lhs_t == Type::Any {
                                            args[0]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[0],
                                                type_to_jit_rt_tag(lhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        let rhs = if rhs_t == Type::Any {
                                            args[1]
                                        } else {
                                            let fresh = alloc();
                                            insts.push(RirInst::BoxTyped(
                                                fresh,
                                                args[1],
                                                type_to_jit_rt_tag(rhs_t),
                                            ));
                                            value_types.insert(fresh, Type::Any);
                                            fresh
                                        };
                                        insts.push(RirInst::EqualAny(dst, lhs, rhs));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("eq?", 2)
                                    | ("eqv?", 2)
                                    | ("boolean=?", 2)
                                    | ("char=?", 2)
                                    | ("symbol=?", 2) => {
                                        insts.push(RirInst::Eq(dst, args[0], args[1]));
                                    }
                                    // ADR 0012 D-2 (iter JA) — variadic
                                    // boolean=? / char=? / symbol=? for 3+ args
                                    // where all args are the same primitive
                                    // type. Pairwise Eq + BitAnd chain.
                                    ("boolean=?", n) | ("char=?", n) | ("symbol=?", n)
                                        if n >= 3
                                            && args.iter().all(|v| {
                                                let t = value_types.get(v).copied();
                                                t == Some(Type::Boolean)
                                                    || t == Some(Type::Character)
                                                    || t == Some(Type::Symbol)
                                            })
                                            && {
                                                let first = value_types.get(&args[0]).copied();
                                                args.iter()
                                                    .all(|v| value_types.get(v).copied() == first)
                                            } =>
                                    {
                                        let first = alloc();
                                        insts.push(RirInst::Eq(first, args[0], args[1]));
                                        value_types.insert(first, Type::Boolean);
                                        let mut acc = first;
                                        for i in 1..args.len() - 1 {
                                            let cmp = alloc();
                                            insts.push(RirInst::Eq(cmp, args[i], args[i + 1]));
                                            value_types.insert(cmp, Type::Boolean);
                                            let new_acc = alloc();
                                            insts.push(RirInst::BitAnd(new_acc, acc, cmp));
                                            value_types.insert(new_acc, Type::Boolean);
                                            acc = new_acc;
                                        }
                                        insts.push(RirInst::Move(dst, acc));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter CF) — char
                                    // ordered comparisons. Character carries
                                    // a codepoint in Fixnum-shape i64 lanes,
                                    // so RirInst::Lt compares them
                                    // numerically — matching R6RS char<?
                                    // (Unicode codepoint order).
                                    ("char<?", 2) => {
                                        insts.push(RirInst::Lt(dst, args[0], args[1]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char>?", 2) => {
                                        // a > b → b < a (swap).
                                        insts.push(RirInst::Lt(dst, args[1], args[0]));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char<=?", 2) => {
                                        // a <= b → NOT (b < a). Mirrors
                                        // LeFx2's pattern: Lt(b, a) then
                                        // Eq(lt, 0).
                                        let lt = alloc();
                                        insts.push(RirInst::Lt(lt, args[1], args[0]));
                                        value_types.insert(lt, Type::Boolean);
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        value_types.insert(zero, Type::Fixnum);
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char>=?", 2) => {
                                        // a >= b → NOT (a < b).
                                        let lt = alloc();
                                        insts.push(RirInst::Lt(lt, args[0], args[1]));
                                        value_types.insert(lt, Type::Boolean);
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        value_types.insert(zero, Type::Fixnum);
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter JB) — variadic char
                                    // ordered comparisons. Pattern mirrors
                                    // variadic </>/<=/>= at line 5277:
                                    // pairwise comparison + BitAnd-chain.
                                    ("char<?", n)
                                    | ("char>?", n)
                                    | ("char<=?", n)
                                    | ("char>=?", n)
                                        if n >= 3 =>
                                    {
                                        let emit_cmp =
                                            |insts: &mut Vec<RirInst>,
                                             value_types: &mut HashMap<RirValue, Type>,
                                             alloc: &mut dyn FnMut() -> RirValue,
                                             a: RirValue,
                                             b: RirValue|
                                             -> RirValue {
                                                let d = alloc();
                                                value_types.insert(d, Type::Boolean);
                                                match name {
                                                    "char<?" => insts.push(RirInst::Lt(d, a, b)),
                                                    "char>?" => insts.push(RirInst::Lt(d, b, a)),
                                                    "char<=?" => {
                                                        let lt = alloc();
                                                        insts.push(RirInst::Lt(lt, b, a));
                                                        value_types.insert(lt, Type::Boolean);
                                                        let zero = alloc();
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Eq(d, lt, zero));
                                                    }
                                                    "char>=?" => {
                                                        let lt = alloc();
                                                        insts.push(RirInst::Lt(lt, a, b));
                                                        value_types.insert(lt, Type::Boolean);
                                                        let zero = alloc();
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Eq(d, lt, zero));
                                                    }
                                                    _ => unreachable!(),
                                                }
                                                d
                                            };
                                        let first = emit_cmp(
                                            &mut insts,
                                            &mut value_types,
                                            &mut alloc,
                                            args[0],
                                            args[1],
                                        );
                                        let mut acc = first;
                                        for i in 1..args.len() - 1 {
                                            let cmp = emit_cmp(
                                                &mut insts,
                                                &mut value_types,
                                                &mut alloc,
                                                args[i],
                                                args[i + 1],
                                            );
                                            let new_acc = alloc();
                                            insts.push(RirInst::BitAnd(new_acc, acc, cmp));
                                            value_types.insert(new_acc, Type::Boolean);
                                            acc = new_acc;
                                        }
                                        insts.push(RirInst::Move(dst, acc));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter CU) — char-ci
                                    // comparison family: case-insensitive
                                    // by foldcasing both operands first,
                                    // then reusing the base op.
                                    ("char-ci=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        let fa = alloc();
                                        let fb = alloc();
                                        insts.push(RirInst::CharFoldcase(fa, args[0]));
                                        value_types.insert(fa, Type::Character);
                                        insts.push(RirInst::CharFoldcase(fb, args[1]));
                                        value_types.insert(fb, Type::Character);
                                        insts.push(RirInst::Eq(dst, fa, fb));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-ci<?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        let fa = alloc();
                                        let fb = alloc();
                                        insts.push(RirInst::CharFoldcase(fa, args[0]));
                                        value_types.insert(fa, Type::Character);
                                        insts.push(RirInst::CharFoldcase(fb, args[1]));
                                        value_types.insert(fb, Type::Character);
                                        insts.push(RirInst::Lt(dst, fa, fb));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-ci>?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        let fa = alloc();
                                        let fb = alloc();
                                        insts.push(RirInst::CharFoldcase(fa, args[0]));
                                        value_types.insert(fa, Type::Character);
                                        insts.push(RirInst::CharFoldcase(fb, args[1]));
                                        value_types.insert(fb, Type::Character);
                                        // a > b → b < a.
                                        insts.push(RirInst::Lt(dst, fb, fa));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-ci<=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        let fa = alloc();
                                        let fb = alloc();
                                        insts.push(RirInst::CharFoldcase(fa, args[0]));
                                        value_types.insert(fa, Type::Character);
                                        insts.push(RirInst::CharFoldcase(fb, args[1]));
                                        value_types.insert(fb, Type::Character);
                                        let lt = alloc();
                                        insts.push(RirInst::Lt(lt, fb, fa));
                                        value_types.insert(lt, Type::Boolean);
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        value_types.insert(zero, Type::Fixnum);
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    ("char-ci>=?", 2)
                                        if value_types.get(&args[0]).copied()
                                            == Some(Type::Character)
                                            && value_types.get(&args[1]).copied()
                                                == Some(Type::Character) =>
                                    {
                                        let fa = alloc();
                                        let fb = alloc();
                                        insts.push(RirInst::CharFoldcase(fa, args[0]));
                                        value_types.insert(fa, Type::Character);
                                        insts.push(RirInst::CharFoldcase(fb, args[1]));
                                        value_types.insert(fb, Type::Character);
                                        let lt = alloc();
                                        insts.push(RirInst::Lt(lt, fa, fb));
                                        value_types.insert(lt, Type::Boolean);
                                        let zero = alloc();
                                        insts.push(RirInst::LoadConst(zero, Const::Fixnum(0)));
                                        value_types.insert(zero, Type::Fixnum);
                                        insts.push(RirInst::Eq(dst, lt, zero));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter JC) — variadic char-ci
                                    // ordered comparisons. Foldcase each arg
                                    // once, then pairwise compare + BitAnd
                                    // chain (mirrors JB for plain char</>).
                                    ("char-ci=?", n)
                                    | ("char-ci<?", n)
                                    | ("char-ci>?", n)
                                    | ("char-ci<=?", n)
                                    | ("char-ci>=?", n)
                                        if n >= 3
                                            && args.iter().all(|v| {
                                                value_types.get(v).copied() == Some(Type::Character)
                                            }) =>
                                    {
                                        let folded: Vec<RirValue> = args
                                            .iter()
                                            .map(|v| {
                                                let f = alloc();
                                                insts.push(RirInst::CharFoldcase(f, *v));
                                                value_types.insert(f, Type::Character);
                                                f
                                            })
                                            .collect();
                                        let emit_cmp =
                                            |insts: &mut Vec<RirInst>,
                                             value_types: &mut HashMap<RirValue, Type>,
                                             alloc: &mut dyn FnMut() -> RirValue,
                                             a: RirValue,
                                             b: RirValue|
                                             -> RirValue {
                                                let d = alloc();
                                                value_types.insert(d, Type::Boolean);
                                                match name {
                                                    "char-ci=?" => insts.push(RirInst::Eq(d, a, b)),
                                                    "char-ci<?" => insts.push(RirInst::Lt(d, a, b)),
                                                    "char-ci>?" => insts.push(RirInst::Lt(d, b, a)),
                                                    "char-ci<=?" => {
                                                        let lt = alloc();
                                                        insts.push(RirInst::Lt(lt, b, a));
                                                        value_types.insert(lt, Type::Boolean);
                                                        let zero = alloc();
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Eq(d, lt, zero));
                                                    }
                                                    "char-ci>=?" => {
                                                        let lt = alloc();
                                                        insts.push(RirInst::Lt(lt, a, b));
                                                        value_types.insert(lt, Type::Boolean);
                                                        let zero = alloc();
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Eq(d, lt, zero));
                                                    }
                                                    _ => unreachable!(),
                                                }
                                                d
                                            };
                                        let first = emit_cmp(
                                            &mut insts,
                                            &mut value_types,
                                            &mut alloc,
                                            folded[0],
                                            folded[1],
                                        );
                                        let mut acc = first;
                                        for i in 1..folded.len() - 1 {
                                            let cmp = emit_cmp(
                                                &mut insts,
                                                &mut value_types,
                                                &mut alloc,
                                                folded[i],
                                                folded[i + 1],
                                            );
                                            let new_acc = alloc();
                                            insts.push(RirInst::BitAnd(new_acc, acc, cmp));
                                            value_types.insert(new_acc, Type::Boolean);
                                            acc = new_acc;
                                        }
                                        insts.push(RirInst::Move(dst, acc));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // Always-false predicates: JIT bodies
                                    // are only entered when every arg is
                                    // a Fixnum (the type guard's
                                    // contract), so any predicate that
                                    // discriminates "is this a non-numeric
                                    // type?" reduces to Const(0).
                                    // `vector?` removed from the alternation:
                                    // the unconditional `("vector?", 1) => ...`
                                    // arm above matches first, making the
                                    // entry here dead (rustc unreachable_
                                    // patterns lint). Other predicates here
                                    // are still reachable — their earlier
                                    // arms have `if arg0 == Type::Any`
                                    // guards, so non-Any operands fall
                                    // through to this always-false fallback.
                                    ("pair?", 1)
                                    | ("null?", 1)
                                    | ("symbol?", 1)
                                    | ("string?", 1)
                                    | ("bytevector?", 1)
                                    | ("procedure?", 1)
                                    | ("port?", 1)
                                    | ("eof-object?", 1) => {
                                        let _ = args[0];
                                        insts.push(RirInst::LoadConst(dst, Const::Boolean(false)));
                                    }
                                    // Phase 5b iter7 — variadic / for
                                    // any operand types. When all
                                    // operands are statically Flonum,
                                    // emit FlonumDiv (with FixToFlo
                                    // promotion for any Fixnum stragglers
                                    // — Flonum contagion). Otherwise
                                    // emit Inst::Div, which lowers to a
                                    // direct call to `vm_value_div_nb`
                                    // (no inline fast path because
                                    // Fixnum/Fixnum can return a
                                    // Rational per R6RS exact division).
                                    //
                                    // The iter5 MakeClosure-presence
                                    // gate no longer applies: with Inst::
                                    // Div handling the slow case via a
                                    // helper, col-loop-style bodies don't
                                    // need to bail out — they get the
                                    // helper call which is identical to
                                    // what they'd do through vm_call_general.
                                    ("/", n) if n >= 2 => {
                                        let any_flonum = args.iter().any(|v| {
                                            value_types.get(v).copied() == Some(Type::Flonum)
                                        });
                                        // Apply iter5's MakeClosure
                                        // gate ONLY to the FlonumDiv
                                        // path — that path would lift
                                        // bodies that recursively call
                                        // themselves through let* into
                                        // uniform-NB and burn host
                                        // stack (mandelbrot's col-loop).
                                        // The Inst::Div path (helper
                                        // call) doesn't have the
                                        // stack-burn risk.
                                        if any_flonum && body_has_makeclosure {
                                            return Err(TranslateError::Unsupported(format!(
                                                "Call to builtin `/` (arity {}, Flonum) in body with MakeClosure",
                                                args.len()
                                            )));
                                        }
                                        if any_flonum {
                                            // Promote Fixnum operands to
                                            // Flonum, then chain FlonumDiv.
                                            let promoted: Vec<RirValue> = args
                                                .iter()
                                                .map(|v| {
                                                    let t = value_types
                                                        .get(v)
                                                        .copied()
                                                        .unwrap_or(Type::Fixnum);
                                                    if t == Type::Flonum {
                                                        *v
                                                    } else {
                                                        let p = alloc();
                                                        insts.push(RirInst::FixToFlo(p, *v));
                                                        value_types.insert(p, Type::Flonum);
                                                        p
                                                    }
                                                })
                                                .collect();
                                            let mut acc = promoted[0];
                                            for &x in &promoted[1..promoted.len() - 1] {
                                                let next = alloc();
                                                insts.push(RirInst::FlonumDiv(next, acc, x));
                                                value_types.insert(next, Type::Flonum);
                                                acc = next;
                                            }
                                            insts.push(RirInst::FlonumDiv(
                                                dst,
                                                acc,
                                                *promoted.last().unwrap(),
                                            ));
                                            value_types.insert(dst, Type::Flonum);
                                        } else {
                                            // Fixnum (or Any) operand chain.
                                            // Use Inst::Div which calls
                                            // `vm_value_div_nb` — returns
                                            // Fixnum / Rational / Bigint /
                                            // Flonum depending on values.
                                            // Result type is Any since we
                                            // can't predict.
                                            let mut acc = args[0];
                                            for &x in &args[1..args.len() - 1] {
                                                let next = alloc();
                                                insts.push(RirInst::Div(next, acc, x));
                                                value_types.insert(next, Type::Any);
                                                acc = next;
                                            }
                                            insts.push(RirInst::Div(
                                                dst,
                                                acc,
                                                *args.last().unwrap(),
                                            ));
                                            value_types.insert(dst, Type::Any);
                                        }
                                    }
                                    // Variadic +/-/*. The bytecode VM
                                    // compiler only specializes 2-arg
                                    // forms to *Fx2, so anything else
                                    // (0, 1, or 3+ args) reaches us as a
                                    // BuiltinRef + Call N. We chain the
                                    // matching binary RIR op, dispatching
                                    // to the Flonum* variants when every
                                    // operand is statically Flonum-typed.
                                    ("+", _) | ("-", _) | ("*", _) => {
                                        // Mixed-tower contagion: any
                                        // Flonum operand promotes the
                                        // whole chain to Flonum (R6RS
                                        // numeric tower). Promote each
                                        // Fixnum operand via FixToFlo.
                                        let any_flonum = args.iter().any(|v| {
                                            value_types.get(v).copied() == Some(Type::Flonum)
                                        });
                                        let result_t = if any_flonum {
                                            Type::Flonum
                                        } else {
                                            Type::Fixnum
                                        };
                                        let fx_ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "+" => RirInst::Add,
                                                "-" => RirInst::Sub,
                                                "*" => RirInst::Mul,
                                                _ => unreachable!(),
                                            };
                                        let fl_ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "+" => RirInst::FlonumAdd,
                                                "-" => RirInst::FlonumSub,
                                                "*" => RirInst::FlonumMul,
                                                _ => unreachable!(),
                                            };
                                        let ctor = if any_flonum { fl_ctor } else { fx_ctor };
                                        // Promote any Fixnum operands to
                                        // Flonum when the chain is
                                        // any-flonum. Stays Fixnum if all
                                        // operands are Fixnum.
                                        let promoted_args: Vec<RirValue> = if any_flonum {
                                            args.iter()
                                                .map(|v| {
                                                    let t = value_types
                                                        .get(v)
                                                        .copied()
                                                        .unwrap_or(Type::Fixnum);
                                                    if t == Type::Flonum {
                                                        *v
                                                    } else {
                                                        let p = alloc();
                                                        insts.push(RirInst::FixToFlo(p, *v));
                                                        value_types.insert(p, Type::Flonum);
                                                        p
                                                    }
                                                })
                                                .collect()
                                        } else {
                                            args.clone()
                                        };
                                        if promoted_args.is_empty() {
                                            // (+) = 0; (*) = 1; (-) is
                                            // an arity error in walker — bail.
                                            match name {
                                                "+" => insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Fixnum(0),
                                                )),
                                                "*" => insts.push(RirInst::LoadConst(
                                                    dst,
                                                    Const::Fixnum(1),
                                                )),
                                                _ => {
                                                    return Err(TranslateError::Unsupported(
                                                        format!("0-arg `{}` is an error", name),
                                                    ))
                                                }
                                            }
                                            value_types.insert(dst, Type::Fixnum);
                                        } else if promoted_args.len() == 1 {
                                            match name {
                                                "+" | "*" => {
                                                    // Identity.
                                                    insts
                                                        .push(RirInst::Move(dst, promoted_args[0]));
                                                    value_types.insert(dst, result_t);
                                                }
                                                "-" => {
                                                    // (- x) = 0 - x. Same
                                                    // operand-type rules.
                                                    let zero = alloc();
                                                    if any_flonum {
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Flonum(0.0),
                                                        ));
                                                        value_types.insert(zero, Type::Flonum);
                                                        insts.push(RirInst::FlonumSub(
                                                            dst,
                                                            zero,
                                                            promoted_args[0],
                                                        ));
                                                    } else {
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Sub(
                                                            dst,
                                                            zero,
                                                            promoted_args[0],
                                                        ));
                                                    }
                                                    value_types.insert(dst, result_t);
                                                }
                                                _ => unreachable!(),
                                            }
                                        } else {
                                            // 2+: chain.
                                            let mut acc = promoted_args[0];
                                            for &x in &promoted_args[1..promoted_args.len() - 1] {
                                                let next = alloc();
                                                insts.push(ctor(next, acc, x));
                                                value_types.insert(next, result_t);
                                                acc = next;
                                            }
                                            insts.push(ctor(
                                                dst,
                                                acc,
                                                *promoted_args.last().unwrap(),
                                            ));
                                            value_types.insert(dst, result_t);
                                        }
                                    }
                                    // ADR 0012 D-2 (iter DM) — variadic
                                    // comparisons (3+ args). R6RS pairwise:
                                    // (< a b c) means a<b AND b<c. Chains
                                    // pairwise Lt/Eq with BitAnd on the
                                    // Boolean (0/1) results. Fixnum-only
                                    // for now; mixed-tower deferred.
                                    ("<", n) | (">", n) | ("<=", n) | (">=", n) | ("=", n)
                                        if n >= 3 =>
                                    {
                                        let emit_cmp =
                                            |insts: &mut Vec<RirInst>,
                                             value_types: &mut HashMap<RirValue, Type>,
                                             alloc: &mut dyn FnMut() -> RirValue,
                                             a: RirValue,
                                             b: RirValue|
                                             -> RirValue {
                                                let d = alloc();
                                                value_types.insert(d, Type::Boolean);
                                                match name {
                                                    "<" => insts.push(RirInst::Lt(d, a, b)),
                                                    ">" => insts.push(RirInst::Lt(d, b, a)),
                                                    "<=" => {
                                                        let lt = alloc();
                                                        insts.push(RirInst::Lt(lt, b, a));
                                                        value_types.insert(lt, Type::Boolean);
                                                        let zero = alloc();
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Eq(d, lt, zero));
                                                    }
                                                    ">=" => {
                                                        let lt = alloc();
                                                        insts.push(RirInst::Lt(lt, a, b));
                                                        value_types.insert(lt, Type::Boolean);
                                                        let zero = alloc();
                                                        insts.push(RirInst::LoadConst(
                                                            zero,
                                                            Const::Fixnum(0),
                                                        ));
                                                        value_types.insert(zero, Type::Fixnum);
                                                        insts.push(RirInst::Eq(d, lt, zero));
                                                    }
                                                    "=" => insts.push(RirInst::Eq(d, a, b)),
                                                    _ => unreachable!(),
                                                }
                                                d
                                            };
                                        let first = emit_cmp(
                                            &mut insts,
                                            &mut value_types,
                                            &mut alloc,
                                            args[0],
                                            args[1],
                                        );
                                        let mut acc = first;
                                        for i in 1..args.len() - 1 {
                                            let cmp = emit_cmp(
                                                &mut insts,
                                                &mut value_types,
                                                &mut alloc,
                                                args[i],
                                                args[i + 1],
                                            );
                                            let new_acc = alloc();
                                            insts.push(RirInst::BitAnd(new_acc, acc, cmp));
                                            value_types.insert(new_acc, Type::Boolean);
                                            acc = new_acc;
                                        }
                                        insts.push(RirInst::Move(dst, acc));
                                        value_types.insert(dst, Type::Boolean);
                                    }
                                    // ADR 0012 D-2 (iter DJ) — variadic
                                    // bitwise ops. Fixnum-only (no Flonum
                                    // promotion). Identity element for
                                    // 0-arg: bitwise-and = -1 (all bits set),
                                    // bitwise-ior/-or = 0, bitwise-xor = 0.
                                    ("bitwise-and", _)
                                    | ("bitwise-ior", _)
                                    | ("bitwise-or", _)
                                    | ("bitwise-xor", _) => {
                                        let ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "bitwise-and" => RirInst::BitAnd,
                                                "bitwise-ior" | "bitwise-or" => RirInst::BitOr,
                                                "bitwise-xor" => RirInst::BitXor,
                                                _ => unreachable!(),
                                            };
                                        if args.is_empty() {
                                            let ident = match name {
                                                "bitwise-and" => -1i64,
                                                _ => 0i64,
                                            };
                                            insts.push(RirInst::LoadConst(
                                                dst,
                                                Const::Fixnum(ident),
                                            ));
                                            value_types.insert(dst, Type::Fixnum);
                                        } else if args.len() == 1 {
                                            insts.push(RirInst::Move(dst, args[0]));
                                            value_types.insert(dst, Type::Fixnum);
                                        } else {
                                            let mut acc = args[0];
                                            for v in &args[1..args.len() - 1] {
                                                let next = alloc();
                                                insts.push(ctor(next, acc, *v));
                                                value_types.insert(next, Type::Fixnum);
                                                acc = next;
                                            }
                                            insts.push(ctor(dst, acc, *args.last().unwrap()));
                                            value_types.insert(dst, Type::Fixnum);
                                        }
                                    }
                                    // ADR 0012 D-2 (iter JF) — variadic flmin/flmax
                                    // for 1+ args. Strict Flonum-typed args.
                                    // 1-arg case uses self-application (x.max(x))
                                    // rather than Move so the post-pass tags dst
                                    // as Flonum via the FlonumMin/FlonumMax arm —
                                    // Move is type-neutral and would leave dst
                                    // decoded as Fixnum.
                                    ("flmin", _) | ("flmax", _)
                                        if args.len() >= 1
                                            && args.iter().all(|v| {
                                                value_types.get(v).copied() == Some(Type::Flonum)
                                            }) =>
                                    {
                                        let ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "flmin" => RirInst::FlonumMin,
                                                "flmax" => RirInst::FlonumMax,
                                                _ => unreachable!(),
                                            };
                                        if args.len() == 1 {
                                            // Self-application: x.min(x) == x and
                                            // x.max(x) == x for all finite x; NaN
                                            // semantics match the underlying op.
                                            insts.push(ctor(dst, args[0], args[0]));
                                            value_types.insert(dst, Type::Flonum);
                                        } else {
                                            let mut acc = args[0];
                                            for v in &args[1..args.len() - 1] {
                                                let next = alloc();
                                                insts.push(ctor(next, acc, *v));
                                                value_types.insert(next, Type::Flonum);
                                                acc = next;
                                            }
                                            insts.push(ctor(dst, acc, *args.last().unwrap()));
                                            value_types.insert(dst, Type::Flonum);
                                        }
                                    }
                                    // ADR 0012 D-2 (iter IY) — variadic fxmin/fxmax.
                                    // Fixnum-only (Flonum operands cause deopt
                                    // via the unsupported tail). 1-arg → Move;
                                    // 3+ args → left-fold over MinFixnum/MaxFixnum.
                                    ("fxmin", _) | ("fxmax", _)
                                        if args.len() >= 1
                                            && args.iter().all(|v| {
                                                value_types.get(v).copied() != Some(Type::Flonum)
                                            }) =>
                                    {
                                        let ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "fxmin" => RirInst::MinFixnum,
                                                "fxmax" => RirInst::MaxFixnum,
                                                _ => unreachable!(),
                                            };
                                        if args.len() == 1 {
                                            insts.push(RirInst::Move(dst, args[0]));
                                            value_types.insert(dst, Type::Fixnum);
                                        } else {
                                            let mut acc = args[0];
                                            for v in &args[1..args.len() - 1] {
                                                let next = alloc();
                                                insts.push(ctor(next, acc, *v));
                                                value_types.insert(next, Type::Fixnum);
                                                acc = next;
                                            }
                                            insts.push(ctor(dst, acc, *args.last().unwrap()));
                                            value_types.insert(dst, Type::Fixnum);
                                        }
                                    }
                                    // ADR 0012 D-2 (iter DI) — variadic
                                    // min/max chain. Pattern mirrors +/-/*
                                    // above: Flonum-contagion promotion +
                                    // left-fold via MinFixnum/MaxFixnum or
                                    // FlonumMin/FlonumMax.
                                    ("min", _) | ("max", _) if args.len() >= 1 => {
                                        let any_flonum = args.iter().any(|v| {
                                            value_types.get(v).copied() == Some(Type::Flonum)
                                        });
                                        let result_t = if any_flonum {
                                            Type::Flonum
                                        } else {
                                            Type::Fixnum
                                        };
                                        let fx_ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "min" => RirInst::MinFixnum,
                                                "max" => RirInst::MaxFixnum,
                                                _ => unreachable!(),
                                            };
                                        let fl_ctor: fn(RirValue, RirValue, RirValue) -> RirInst =
                                            match name {
                                                "min" => RirInst::FlonumMin,
                                                "max" => RirInst::FlonumMax,
                                                _ => unreachable!(),
                                            };
                                        let ctor = if any_flonum { fl_ctor } else { fx_ctor };
                                        let promoted_args: Vec<RirValue> = if any_flonum {
                                            args.iter()
                                                .map(|v| {
                                                    let t = value_types
                                                        .get(v)
                                                        .copied()
                                                        .unwrap_or(Type::Fixnum);
                                                    if t == Type::Flonum {
                                                        *v
                                                    } else {
                                                        let p = alloc();
                                                        insts.push(RirInst::FixToFlo(p, *v));
                                                        value_types.insert(p, Type::Flonum);
                                                        p
                                                    }
                                                })
                                                .collect()
                                        } else {
                                            args.clone()
                                        };
                                        if promoted_args.len() == 1 {
                                            // Single arg → return as-is.
                                            insts.push(RirInst::Move(dst, promoted_args[0]));
                                            value_types.insert(dst, result_t);
                                        } else {
                                            // Left-fold: acc = ctor(acc, next).
                                            let mut acc = promoted_args[0];
                                            for v in &promoted_args[1..promoted_args.len() - 1] {
                                                let next = alloc();
                                                insts.push(ctor(next, acc, *v));
                                                value_types.insert(next, result_t);
                                                acc = next;
                                            }
                                            insts.push(ctor(
                                                dst,
                                                acc,
                                                *promoted_args.last().unwrap(),
                                            ));
                                            value_types.insert(dst, result_t);
                                        }
                                    }
                                    _ => {
                                        // No dedicated open-coding for this
                                        // builtin — emit a generic by-name
                                        // call. The AOT backend lowers it to
                                        // cs_runtime::aot_call_builtin; the
                                        // cranelift JIT's support gate
                                        // declines any function carrying it
                                        // (it has no runtime-env dispatch),
                                        // so on the JIT path the function
                                        // simply stays on the VM tier — the
                                        // same outcome as the previous
                                        // "not yet lowered" translate error.
                                        insts.push(RirInst::CallBuiltin(
                                            dst,
                                            name.to_string(),
                                            args.clone(),
                                        ));
                                        value_types.insert(dst, Type::Any);
                                    }
                                }
                                sim_stack.push(StackEntry::Value(dst));
                            }
                        }
                        StackEntry::Value(callee_v) => {
                            // Phase 6 Stage A iter 2 — leaf-callee
                            // inlining. Before falling through to the
                            // CallGeneral path below, try to resolve
                            // the callee in `caller_env` to a small,
                            // pure VmClosure and splice its body in
                            // place. Conditions for attempting:
                            //  - `caller_env` was supplied (true on
                            //    the runtime tier-up path; tests pass
                            //    None and skip inlining).
                            //  - `inline_depth < MAX_INLINE_DEPTH` —
                            //    don't recurse into inlined callees.
                            //  - The callee Value flows from an
                            //    `EnvLookup`/`EnvLookupAny` whose
                            //    symbol we can read off the producer
                            //    inst. (Other shapes — `EnvDefineLocal`
                            //    result, Call result — aren't direct
                            //    binding lookups.)
                            //
                            // `try_inline_leaf_callee` itself enforces
                            // the analyzer's eligibility gate (size,
                            // purity, single-block, single-exit, etc.).
                            let inlined_result: Option<(RirValue, Type)> = (|| {
                                if inline_depth >= MAX_INLINE_DEPTH {
                                    return None;
                                }
                                let env_ref = caller_env?;
                                let callee_sym = find_envlookup_sym(&insts, callee_v)?;
                                try_inline_leaf_callee(
                                    callee_sym,
                                    &args_entries,
                                    env_ref,
                                    inline_depth,
                                    &mut value_types,
                                    &mut insts,
                                    &next_value_id,
                                )
                            })(
                            );
                            if let Some((inlined_v, inlined_t)) = inlined_result {
                                value_types.insert(inlined_v, inlined_t);
                                sim_stack.push(StackEntry::Value(inlined_v));
                                continue;
                            }

                            // ADR 0012 D-1 (iter BU) — slow-path
                            // general Call. The bytecode invoked a
                            // procedure that the translator couldn't
                            // resolve to `self` or a known builtin
                            // (e.g. a top-level lambda binding pulled
                            // through `EnvLookup`). Emit
                            // `Inst::CallGeneral`, which lowers to a
                            // call against `vm_call_general` (the IC
                            // miss handler). All operands flow as
                            // Any-tagged `Gc<Value>` handles; if the
                            // bytecode produced a typed (Fixnum,
                            // Boolean, ...) value we box it first
                            // with `BoxTyped`, mirroring the
                            // `("eq?", 2)` Any-arg pattern above.
                            //
                            // Special case: if `callee_v` was just
                            // produced by an `EnvLookup` (the
                            // Fixnum-only free-var helper), promote
                            // that EnvLookup in-place to an
                            // `EnvLookupAny` so the helper returns a
                            // live Gc handle instead of panicking on
                            // a Procedure-bound symbol. This is the
                            // common case for `(define inner ...)
                            // (define (outer y) (inner y))`.
                            let mut callee_box = callee_v;
                            // Free-var callee may be Fixnum-defaulted
                            // (no `value_types` entry). Promote its
                            // producing `EnvLookup` in-place so the
                            // runtime returns a Gc handle for any
                            // binding — Procedure, Pair, whatever.
                            // (Helper documents the SSA-safety argument.)
                            let promoted =
                                promote_envlookup_to_any(&mut insts, &mut value_types, callee_v);
                            if !promoted {
                                let callee_t =
                                    value_types.get(&callee_v).copied().unwrap_or(Type::Fixnum);
                                if callee_t != Type::Any {
                                    let fresh = alloc();
                                    insts.push(RirInst::BoxTyped(
                                        fresh,
                                        callee_v,
                                        type_to_jit_rt_tag(callee_t),
                                    ));
                                    value_types.insert(fresh, Type::Any);
                                    callee_box = fresh;
                                }
                            }
                            // Box each arg to Any if it's not
                            // already.
                            //
                            // Free-var args get the same in-place
                            // `EnvLookup` → `EnvLookupAny` promotion
                            // the callee path uses above. Without it,
                            // a free-var-bound list / closure / etc.
                            // would default-type as `Fixnum` (no
                            // `value_types` entry for free-var loads),
                            // its `vm_env_lookup_fixnum` would deopt
                            // and return 0, then `BoxTyped(Fixnum)`
                            // would wrap that 0 into a real
                            // `Value::Number(Fixnum(0))` and feed it
                            // to the callee. Discovered when n-queens'
                            // inner `(lambda (col) ...)` started
                            // tiering up under the shared
                            // `LambdaProfile` (post-M8 Stage 0): its
                            // free var `placed` (a list) was
                            // box-as-Fixnum'd into the `safe?` call,
                            // which then `car`-ed `0` and raised.
                            let mut args: Vec<RirValue> = Vec::with_capacity(*n);
                            for e in args_entries {
                                let av = match e {
                                    StackEntry::Value(v) => v,
                                    StackEntry::SelfRef | StackEntry::BuiltinRef(_) => {
                                        return Err(TranslateError::Unsupported(
                                            "non-Value entry as CallGeneral arg".into(),
                                        ));
                                    }
                                };
                                let at = value_types.get(&av).copied().unwrap_or(Type::Fixnum);
                                let abox = if at == Type::Any {
                                    av
                                } else if promote_envlookup_to_any(&mut insts, &mut value_types, av)
                                {
                                    av
                                } else {
                                    let fresh = alloc();
                                    insts.push(RirInst::BoxTyped(
                                        fresh,
                                        av,
                                        type_to_jit_rt_tag(at),
                                    ));
                                    value_types.insert(fresh, Type::Any);
                                    fresh
                                };
                                args.push(abox);
                            }
                            let dst = alloc();
                            insts.push(RirInst::CallGeneral(dst, callee_box, args));
                            value_types.insert(dst, Type::Any);
                            sim_stack.push(StackEntry::Value(dst));
                        }
                    }
                }
                other => {
                    return Err(TranslateError::Unsupported(format!(
                        "opcode {} not yet lowered",
                        opcode_name(other)
                    )));
                }
            }
        }

        let terminator = match term {
            Some(t) => t,
            None => {
                // Implicit fall-through to the next block in offset
                // order. Pull the current stack as Jump args; seed
                // the successor's entry stack height accordingly.
                if i + 1 >= block_offsets.len() {
                    return Err(TranslateError::Invalid(format!(
                        "block at offset {} falls off function end",
                        start
                    )));
                }
                let next_id = BlockId((i + 1) as u32);
                let stack_vals: Vec<RirValue> = sim_stack
                    .iter()
                    .map(|e| match e {
                        StackEntry::Value(v) => Ok(*v),
                        StackEntry::SelfRef => Err(TranslateError::Unsupported(
                            "self-ref in fall-through stack".into(),
                        )),
                        StackEntry::BuiltinRef(_) => Err(TranslateError::Unsupported(
                            "builtin-ref in fall-through stack".into(),
                        )),
                    })
                    .collect::<Result<_, _>>()?;
                seed_block_entry(
                    &mut block_entry_stack,
                    &mut block_params,
                    &mut value_types,
                    &mut alloc,
                    next_id,
                    &stack_vals,
                )?;
                Term::Jump(next_id, stack_vals)
            }
        };

        let params = block_params.get(&block_id).cloned().unwrap_or_default();
        func.blocks.push(Block {
            id: block_id,
            params,
            insts,
            terminator,
        });
    }

    // Pre-widen: infer a preliminary return type, then propagate
    // it to every CallSelf dst's `value_types` slot. CallSelf
    // returns are "the function's own type" by definition, but
    // without explicit propagation the dst defaults to Type::Fixnum
    // and triggers spurious widening when a sibling Jump arg is
    // typed differently. The tail-call optimization in the lowerer
    // requires `CallSelf` to be the LAST inst of its block — once
    // widening inserts a BoxTyped after CallSelf, that invariant
    // breaks and sumsq-style let-loop bodies stop tail-recursing.
    let preliminary = infer_return_type(&func);
    for block in &mut func.blocks {
        for inst in &block.insts {
            if let RirInst::CallSelf(dst, _) = inst {
                value_types.insert(*dst, preliminary);
            }
        }
    }

    // Phase 4 post-keystone fix — patch operand tags on Insts that
    // were emitted with `value_types.get(...).unwrap_or(Type::Fixnum)`
    // before the CallSelf-dst propagation above ran. Without this, a
    // body like `make-tree` (which feeds two `CallSelf` results into
    // a `Cons`) emits `Cons(v_self, JIT_RT_FIXNUM, v_self,
    // JIT_RT_FIXNUM)` because `v_self`'s type isn't in `value_types`
    // yet at block-translation time. The specialized tier's lowering
    // then treats each pointer-typed payload as a raw Fixnum, and
    // the resulting Pair has Fixnum-wrapped-pointer slots — a
    // silent miscompile that produces garbage trees and, when a
    // downstream consumer (`check-tree`) recurses through those
    // slots, blows the host stack.
    //
    // The patch re-derives each Inst's operand tags from the now-
    // finalized `value_types` map. Only Inst variants that carry
    // per-operand tags are scanned (`Cons`, `BoxTyped`); everything
    // else is unaffected.
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            match inst {
                RirInst::Cons(_dst, car, car_tag, cdr, cdr_tag) => {
                    let car_t = value_types.get(car).copied().unwrap_or(Type::Fixnum);
                    let cdr_t = value_types.get(cdr).copied().unwrap_or(Type::Fixnum);
                    *car_tag = type_to_jit_rt_tag(car_t);
                    *cdr_tag = type_to_jit_rt_tag(cdr_t);
                }
                RirInst::BoxTyped(_dst, src, tag) => {
                    let t = value_types.get(src).copied().unwrap_or(Type::Fixnum);
                    *tag = type_to_jit_rt_tag(t);
                }
                _ => {}
            }
        }
    }

    // Phase 4 iter AW — control-flow-join widening. When two
    // predecessors of a block push different-typed values, widen the
    // join's params to Any and insert BoxTyped on the immediate-typed
    // predecessors. Iterates to a fixed point so widening can
    // propagate (e.g., outer ifs whose branches go through inner
    // joins). Mutates `func` in place + grows `value_types` for the
    // freshly allocated boxed values.
    widen_joins_to_any(&mut func, &mut value_types, &mut alloc);

    func.return_type = infer_return_type(&func);
    // Same idea on the return side: when a function's inferred type
    // is Any, every Return path must produce a Box pointer. Insert
    // BoxTyped on Returns whose value is still typed.
    if func.return_type == Type::Any {
        box_mixed_returns(&mut func, &mut value_types, &mut alloc);
    }
    // ADR 0014 integration point — the pluggable optimizer-pass
    // pipeline runs here, between bytecode→RIR translation and the
    // back-end consumers (cs-jit-cranelift / cs-aot). Iter 1 is a
    // documented no-op; iter 3 (Scheme `install-optimizer-pass!`)
    // replaces cs-opt's implementation to actually consult an
    // active-pass list. Calling it now means no further cs-vm
    // edits are required when the iter 3 surface lands.
    cs_opt::run_active_pipeline(&mut func);
    Ok(func)
}

/// Iterate to fixed point: for each block whose predecessors push
/// disagreeing types into the same slot, widen that block's
/// `params[i]` to `Type::Any` and insert `BoxTyped` on every
/// predecessor's Jump that passes a non-Any value. Each widening
/// can change a downstream block's argument type, so we re-loop
/// until no changes happen.
fn widen_joins_to_any(
    func: &mut cs_rir::Function,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
) {
    use std::collections::HashSet;
    loop {
        // (target_block, slot) -> set of arg types observed.
        let mut slot_types: HashMap<(BlockId, usize), HashSet<Type>> = HashMap::new();
        for block in &func.blocks {
            if let Term::Jump(target, args) = &block.terminator {
                for (i, arg) in args.iter().enumerate() {
                    let t = value_types.get(arg).copied().unwrap_or(Type::Fixnum);
                    slot_types.entry((*target, i)).or_default().insert(t);
                }
            }
        }

        let mut widened = false;
        for ((target, slot), types) in &slot_types {
            // Widen whenever the predecessors disagree on a slot's
            // type, regardless of whether Any is one of them. Without
            // this, the join block_param keeps the first predecessor's
            // type and other predecessors' i64s get reinterpreted at
            // decode time (e.g. a Null carried as Fixnum would
            // surface as `0` instead of `'()`).
            let needs_widen = types.len() > 1;
            if !needs_widen {
                continue;
            }
            let target_idx = match func.blocks.iter().position(|b| b.id == *target) {
                Some(i) => i,
                None => continue,
            };
            if func.blocks[target_idx].params[*slot].1 != Type::Any {
                let pv = func.blocks[target_idx].params[*slot].0;
                func.blocks[target_idx].params[*slot].1 = Type::Any;
                value_types.insert(pv, Type::Any);
                widened = true;
            }
        }
        if !widened {
            break;
        }
    }

    // Now insert BoxTyped on every Jump arg whose target slot is Any
    // but whose source value isn't.
    for block_idx in 0..func.blocks.len() {
        let (target_idx, mut new_args) = match &func.blocks[block_idx].terminator {
            Term::Jump(target, args) => {
                let target_idx = match func.blocks.iter().position(|b| b.id == *target) {
                    Some(i) => i,
                    None => continue,
                };
                (target_idx, args.clone())
            }
            _ => continue,
        };

        let mut box_inserts: Vec<RirInst> = Vec::new();
        for (i, arg) in new_args.iter_mut().enumerate() {
            let exp_t = func.blocks[target_idx].params[i].1;
            let arg_t = value_types.get(arg).copied().unwrap_or(Type::Fixnum);
            if exp_t == Type::Any && arg_t != Type::Any {
                let tag = type_to_jit_rt_tag(arg_t);
                let fresh = alloc();
                box_inserts.push(RirInst::BoxTyped(fresh, *arg, tag));
                value_types.insert(fresh, Type::Any);
                *arg = fresh;
            }
        }
        if !box_inserts.is_empty() {
            func.blocks[block_idx].insts.extend(box_inserts);
            if let Term::Jump(_, ref mut args) = func.blocks[block_idx].terminator {
                *args = new_args;
            }
        }
    }
}

/// For every Return whose value is still typed (not Any), insert a
/// `BoxTyped` and rewrite the terminator. Called only when the
/// function's overall return type is Any.
fn box_mixed_returns(
    func: &mut cs_rir::Function,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
) {
    for block_idx in 0..func.blocks.len() {
        let v = match &func.blocks[block_idx].terminator {
            Term::Return(v) => *v,
            _ => continue,
        };
        let vt = value_types.get(&v).copied().unwrap_or(Type::Fixnum);
        if vt == Type::Any {
            continue;
        }
        let tag = type_to_jit_rt_tag(vt);
        let fresh = alloc();
        func.blocks[block_idx]
            .insts
            .push(RirInst::BoxTyped(fresh, v, tag));
        value_types.insert(fresh, Type::Any);
        func.blocks[block_idx].terminator = Term::Return(fresh);
    }
}

/// Walk every instruction in `func` and compute a per-Value type table,
/// then inspect each block terminator and pick the type the function
/// will return at runtime. RIR values default to `Type::Fixnum`; only
/// the comparison instructions and explicit Boolean LoadConsts produce
/// Boolean. If multiple Returns disagree we conservatively fall back to
/// `Type::Fixnum` (which is the i64-passthrough decoding) — the
/// dispatcher's own type guards will catch a mismatch downstream.
/// Map an RIR `Type` to the matching `JIT_RT_*` u8 tag in cs-vm.
/// Mirrors `cs_vm::vm::JIT_RT_FIXNUM` etc. — duplicated here to
/// avoid a circular import at translate time. Heap-pointer types
/// not yet wired through Cranelift map to `JIT_RT_ANY`.
/// Map SRFI-1 ordinal accessor names (first..tenth) to the
/// equivalent Car/Cdr direction chain: car of N-1 cdrs.
/// `false` = Car, `true` = Cdr. The dispatch arm applies them
/// right-to-left, so the chain is `[Car, Cdr, Cdr, ..., Cdr]`
/// for `nth` = N-1 Cdrs then a Car. ADR 0012 D-2 (iter EZ + FA).
fn ordinal_to_cxr_dirs(name: &str) -> Option<Vec<bool>> {
    let n = match name {
        "first" => 1usize,
        "second" => 2,
        "third" => 3,
        "fourth" => 4,
        "fifth" => 5,
        "sixth" => 6,
        "seventh" => 7,
        "eighth" => 8,
        "ninth" => 9,
        "tenth" => 10,
        _ => return None,
    };
    let mut dirs = Vec::with_capacity(n);
    dirs.push(false);
    for _ in 1..n {
        dirs.push(true);
    }
    Some(dirs)
}

/// Parse a composed pair accessor name like `caar`, `caddr`,
/// `cddddr`. Returns `Some(directions)` where `false` means Car (the
/// 'a' letter) and `true` means Cdr (the 'd' letter), reading the
/// middle of the name left-to-right (outermost operation first).
/// Returns `None` for names that aren't a valid 2..=4-letter cxr
/// (also rejects `car`/`cdr` since those have specialized arms).
/// ADR 0012 D-2 (iter DV).
fn cxr_parse(name: &str) -> Option<Vec<bool>> {
    let bytes = name.as_bytes();
    if bytes.len() < 4 || bytes.len() > 6 {
        return None;
    }
    if bytes[0] != b'c' || *bytes.last().unwrap() != b'r' {
        return None;
    }
    let mid = &bytes[1..bytes.len() - 1];
    if mid.len() < 2 {
        return None;
    }
    let mut dirs = Vec::with_capacity(mid.len());
    for &b in mid {
        match b {
            b'a' => dirs.push(false),
            b'd' => dirs.push(true),
            _ => return None,
        }
    }
    Some(dirs)
}

fn type_to_jit_rt_tag(t: Type) -> u8 {
    match t {
        Type::Fixnum => 0,
        Type::Boolean => 1,
        Type::Character => 2,
        Type::Flonum => 3,
        Type::Pair => 4,
        Type::Vector => 5,
        Type::String => 6,
        Type::ByteVector => 7,
        Type::Procedure => 8,
        Type::Symbol => 9,
        Type::Null => 14,
        Type::Any => 15,
    }
}

/// If `av` was produced by a free-var [`RirInst::EnvLookup`] in
/// `insts`, rewrite that producer in-place to [`RirInst::EnvLookupAny`]
/// so the runtime returns a Gc handle for *any* value (list, closure,
/// pair, ...), not just a Fixnum. Marks `av` as [`Type::Any`] in
/// `value_types` and returns `true`. Returns `false` if `av` did not
/// come from an `EnvLookup` (e.g. it's a constant or arithmetic
/// result).
///
/// Free-var loads default-type as `Fixnum` because the translator
/// never inserts into `value_types` for them. Any downstream
/// consumer that needs the operand in an Any-shape slot (CallGeneral
/// callee/arg, `cons`'s car/cdr, …) must call this first; otherwise
/// the operand gets `BoxTyped(Fixnum)`-wrapped, the runtime
/// `vm_env_lookup_fixnum` deopts on a non-Fixnum binding, and the
/// body silently feeds garbage `Value::Number(Fixnum(0))` to the
/// consumer (which then crashes — e.g. `car` of `0` raises). This
/// promotion is SSA-safe: each `EnvLookup` result has a unique
/// producer (the bytecode->RIR translator allocates a fresh dst per
/// `LoadVar`) and a single consumer (enforced by the stack-machine
/// simulation that drives translation).
fn promote_envlookup_to_any(
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    av: RirValue,
) -> bool {
    for inst in insts.iter_mut().rev() {
        if let RirInst::EnvLookup(d, sym) = inst {
            if *d == av {
                *inst = RirInst::EnvLookupAny(*d, *sym);
                value_types.insert(av, Type::Any);
                return true;
            }
        }
    }
    false
}

/// Phase 6 Stage A iter 2 — recover the binding symbol from the
/// `EnvLookup`/`EnvLookupAny` producer that emitted `v`. Walks the
/// already-emitted RIR insts backwards; returns `Some(sym)` on
/// first match.
///
/// Returns `None` if `v` was produced by anything other than an
/// env-lookup (constant, arithmetic, call result, etc.) — those
/// shapes don't represent a top-level binding so inlining can't
/// resolve them anyway.
fn find_envlookup_sym(insts: &[RirInst], v: RirValue) -> Option<Symbol> {
    for inst in insts.iter().rev() {
        match inst {
            RirInst::EnvLookup(d, sym) | RirInst::EnvLookupAny(d, sym) if *d == v => {
                return Some(Symbol(*sym));
            }
            _ => {}
        }
    }
    None
}

/// Phase 6 Stage A iter 2 — demote `EnvDefineLocal`/`EnvLookupAny`
/// env round-trips to SSA aliases in the first block of `func`.
/// Returns `true` if the transformation succeeded, `false` if any
/// env op references a sym we didn't see defined (signal to bail
/// on inlining this callee).
///
/// Single-pass walk:
/// - `EnvDefineLocal(sym, src)` — record `sym -> src`, drop the inst.
/// - `EnvLookupAny(d, sym)` — if `sym` is recorded, rewrite into
///   `Move(d, src)`. Move is in the inline-walker's supported set
///   (an SSA copy) so analyzer + splice handle it natively.
/// - All other insts pass through unchanged.
///
/// Soundness — why aliasing is safe here:
/// - iter9's compile-time let* inlining emits `EnterScope` /
///   `DefineLocal` / body / `LeaveScope` for non-escaping bindings.
///   The translator only routes through env when the body has
///   `MakeClosure` (a closure might capture the binding); for
///   leaf-callee bodies the analyzer already rejects MakeClosure
///   (`HasMakeClosure`), so the bindings here provably don't escape.
/// - Multi-use of an SSA value with NB-inline type (Fixnum, Flonum,
///   Boolean, Character) is refcount-free in the uniform-NB lowering
///   (FlonumAdd, Add, etc. bitcast / unbox without touching counts).
///   Heap-typed sources would need incref per use; the iter 2
///   conservative posture is "pass through unchanged if the source
///   is NOT one of the inline types" — i.e. the analyzer's
///   downstream `UnsupportedInst` arms reject any inst that consumes
///   a heap-typed value with multi-use shape. iter 4+ widens this.
fn demote_env_to_ssa_in_first_block(func: &mut cs_rir::Function) -> bool {
    use cs_rir::inline::{for_each_value_in_inst, for_each_value_in_term, is_inline_supported};

    if func.blocks.is_empty() {
        return true;
    }
    let block = &mut func.blocks[0];
    // sym -> source RirValue from each EnvDefineLocal we drop.
    let mut sym_to_src: HashMap<u32, RirValue> = HashMap::new();
    // dst-of-EnvLookup -> aliased source. Subsequent insts have
    // their Value operands rewritten via this table. Substituting
    // at the operand level (rather than emitting Move insts) avoids
    // a uniform-NB tier hazard: Move is just an alias of i64 bits,
    // but in uniform-NB the bits carry an NB tag; if the source has
    // a Character / Boolean / Flonum tag and downstream consumers
    // assume Fixnum, the value decodes wrong. Substituting through
    // means downstream consumers see the SAME SSA value the
    // analyzer/translator already type-tracked correctly.
    let mut alias: HashMap<RirValue, RirValue> = HashMap::new();
    // Resolve transitively in case of alias-of-alias from nested
    // let* bindings — `(let* ((a x) (b a)) ...)` chains through.
    let resolve = |v: RirValue, alias: &HashMap<RirValue, RirValue>| -> RirValue {
        let mut cur = v;
        for _ in 0..=alias.len() {
            match alias.get(&cur) {
                Some(&next) if next != cur => cur = next,
                _ => return cur,
            }
        }
        cur
    };
    let mut rewritten: Vec<RirInst> = Vec::with_capacity(block.insts.len());
    for inst in block.insts.drain(..) {
        match inst {
            RirInst::EnvDefineLocal(sym, src) => {
                // Resolve src before recording so chains stay flat.
                sym_to_src.insert(sym, resolve(src, &alias));
                // drop the inst
            }
            RirInst::EnvLookupAny(d, sym) | RirInst::EnvLookup(d, sym) => {
                let src = match sym_to_src.get(&sym) {
                    Some(&s) => s,
                    None => {
                        // EnvLookup on a sym we didn't see defined
                        // locally — it's a free-var binding that
                        // lives in the captured env. Inlining would
                        // need env retargeting; bail.
                        return false;
                    }
                };
                // Record d -> src; don't emit any inst.
                alias.insert(d, src);
            }
            mut other => {
                // Substitute every Value operand via the alias table,
                // then push the rewritten inst into the output stream.
                //
                // Skip the walker for variants outside
                // `is_inline_supported` (MakeClosure, VecAlloc, ...)
                // — those would hit the walker's unreachable!() arm.
                // The follow-up `analyze_for_inline` rejects bodies
                // containing them anyway, so demote correctness
                // doesn't depend on rewriting their operands.
                if is_inline_supported(&other) {
                    for_each_value_in_inst(&mut other, |v| {
                        *v = resolve(*v, &alias);
                    });
                }
                rewritten.push(other);
            }
        }
    }
    block.insts = rewritten;
    // Substitute through the terminator (Return value, Jump args,
    // Branch cond). Iter 2's single-block case has only Return, but
    // doing this uniformly future-proofs for iter 3's multi-block.
    for_each_value_in_term(&mut func.blocks[0].terminator, |v| {
        *v = resolve(*v, &alias);
    });
    true
}

/// Phase 6 Stage A iter 2 — leaf-callee inlining splice driver.
///
/// Resolves `callee_sym` against `caller_env`, gets the callee's
/// VmClosure + compiled lambda, recursively translates the callee
/// body to RIR (with `inline_depth + 1` so nested calls stay as
/// `CallGeneral`), runs the eligibility analyzer, and on accept
/// splices the callee body into `insts` via
/// [`cs_rir::inline::splice_single_block`]. Returns the caller-side
/// `Value` bound to the callee's return result, along with the
/// callee's inferred return type so the caller can update its
/// `value_types` map.
///
/// On any failure (sym not bound, not a VmClosure, callee body
/// doesn't translate, multi-block, eligibility rejection, arity
/// mismatch, rest param, ...) returns `None`. The caller falls
/// through to the regular `CallGeneral` emission path.
///
/// Iter 2 restrictions:
/// - Single-block callee only (multi-block is iter 3).
/// - Callee body must pass `analyze_for_inline` (no internal calls,
///   no closures, no env mutation, no env lookups, size <= 20 insts).
/// - Caller-side EnvLookup that produced the callee value is left
///   in `insts` as dead code; Cranelift's DCE handles it.
#[allow(clippy::too_many_arguments)]
fn try_inline_leaf_callee(
    callee_sym: Symbol,
    args_entries: &[StackEntry],
    caller_env: &Rc<Env>,
    inline_depth: usize,
    value_types: &mut HashMap<RirValue, Type>,
    insts: &mut Vec<RirInst>,
    next_value_id: &Cell<u32>,
) -> Option<(RirValue, Type)> {
    // 1. Resolve symbol → VmClosure.
    let bound = caller_env.get(callee_sym)?;
    let proc_rc = match bound {
        Value::Procedure(p) => p,
        _ => return None,
    };
    let closure = proc_rc.as_any().downcast_ref::<VmClosure>()?;

    // 2. Get callee's CompiledLambda.
    let bc = closure.bc.clone();
    let lambda_idx = closure.lambda_idx;
    if lambda_idx >= bc.lambdas.len() {
        return None;
    }
    let callee_lambda = &bc.lambdas[lambda_idx];
    if callee_lambda.rest.is_some() {
        return None;
    }
    if callee_lambda.params.len() != args_entries.len() {
        return None;
    }

    // 3. Collect raw arg values + their types from args_entries.
    //    Only StackEntry::Value is acceptable; SelfRef/BuiltinRef
    //    would have been caught by the outer arm.
    let mut raw_args: Vec<(RirValue, Type)> = Vec::with_capacity(args_entries.len());
    for e in args_entries {
        let v = match e {
            StackEntry::Value(v) => *v,
            _ => return None,
        };
        let t = value_types.get(&v).copied().unwrap_or(Type::Fixnum);
        raw_args.push((v, t));
    }

    // 4. Translate callee body to RIR with arg-type hints. Recursive
    //    call passes `inline_depth + 1` to gate further inlining;
    //    `None` for `caller_env` so inlined callees don't themselves
    //    try to inline (iter 2 keeps the depth window at 1 layer).
    let hints: Vec<Type> = raw_args.iter().map(|(_, t)| *t).collect();
    let mut callee_rir = bytecode_to_rir_full(
        callee_lambda,
        format!("inlined-sym{}", callee_sym.0),
        // No self-name for the inlined body — recursive callees in
        // the callee aren't being self-recursion-detected in this
        // context (and the analyzer would reject CallSelf anyway).
        None,
        Some(&hints),
        None,
        inline_depth + 1,
    )
    .ok()?;

    // 5. Iter 2 restriction: single-block only. Multi-block needs
    //    block-id remapping (iter 3 territory).
    if callee_rir.blocks.len() != 1 {
        return None;
    }

    // 5b. Demote any env ops to SSA aliases. iter9's compile-time
    //     let* inlining emits `EnterScope` + `DefineLocal` + body +
    //     `LeaveScope`, which the translator lowers to
    //     `EnvDefineLocal`/`EnvLookupAny`. These store and read
    //     bindings through `JIT_CALLER_ENV` — which post-splice
    //     points to the CALLER's env, not the callee's, so the
    //     bindings would land in the wrong layer. For pure-leaf
    //     callees with no nested closures (the only shape we'd want
    //     to inline anyway), the env round-trip is unnecessary; the
    //     bindings can live as direct SSA aliases. The pass walks
    //     the single block, drops every `EnvDefineLocal(sym, src)`
    //     while recording `sym -> src`, and rewrites every
    //     `EnvLookupAny(d, sym)` into a `Move(d, src)`. AnyTo*
    //     conversions immediately upstream of those LoadVars stay
    //     as-is (identity in uniform-NB lowering). Aborts if any
    //     env op references a sym we didn't see defined.
    if !demote_env_to_ssa_in_first_block(&mut callee_rir) {
        return None;
    }

    // 6. Eligibility analysis. Returns Err for too-large, contains-
    //    Call, contains-MakeClosure, env-mutation, unsupported-Inst,
    //    multi-return, no-return, etc. — all of which mean the
    //    splice would either explode in size or produce wrong code.
    let md = analyze_for_inline(&callee_rir).ok()?;

    // 7. Splice. value_offset starts at the caller's current
    //    next_value_id; callee's non-param values get renumbered
    //    starting from there.
    let n_params = callee_rir.params.len() as u32;
    let base = next_value_id.get();
    let param_subst: Vec<RirValue> = raw_args.iter().map(|(v, _)| *v).collect();
    let splice = SpliceRequest::new(param_subst, base, 0);
    let result = splice_single_block(insts, &callee_rir, &md, &splice);

    // 8. Advance the caller's id counter past the highest id the
    //    splice consumed. Callee non-param values were [n_params,
    //    md.max_value]; they got remapped to [base, base + (md.max_value
    //    - n_params)]. The next free caller-side id is one past that.
    let highest_used = base.saturating_add(md.max_value.saturating_sub(n_params));
    next_value_id.set(highest_used + 1);

    // 9. Tell the caller what type the inlined result holds so its
    //    `value_types` map stays accurate for downstream consumers
    //    (BoxTyped decisions, type-feedback, …).
    Some((result, callee_rir.return_type))
}

fn infer_return_type(func: &cs_rir::Function) -> Type {
    use cs_rir::Const;
    let mut bool_values: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    let mut char_values: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    let mut flo_values: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    let mut null_values: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    let mut sym_values: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    let mut any_values: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    // Seed from the function's per-param types — when the runtime
    // hook supplied hints (arg-side feedback), parameters get the
    // observed types. Without this, a body that returns a typed
    // parameter directly (e.g. `(define (id-flo n) n)` warmed with
    // a flonum) would fall to Fixnum because the param isn't the
    // dst of any RirInst.
    for (val, ty) in &func.params {
        match ty {
            Type::Flonum => {
                flo_values.insert(*val);
            }
            Type::Boolean => {
                bool_values.insert(*val);
            }
            Type::Character => {
                char_values.insert(*val);
            }
            Type::Null => {
                null_values.insert(*val);
            }
            Type::Symbol => {
                sym_values.insert(*val);
            }
            Type::Any => {
                any_values.insert(*val);
            }
            _ => {}
        }
    }
    // Same for block params — type-propagated by `seed_block_entry`.
    // Required for Return values that came through a Branch
    // terminator (the sim_stack value is reborn as a block param
    // with its predecessor's type).
    for block in &func.blocks {
        for (val, ty) in &block.params {
            match ty {
                Type::Flonum => {
                    flo_values.insert(*val);
                }
                Type::Boolean => {
                    bool_values.insert(*val);
                }
                Type::Character => {
                    char_values.insert(*val);
                }
                Type::Null => {
                    null_values.insert(*val);
                }
                Type::Symbol => {
                    sym_values.insert(*val);
                }
                Type::Any => {
                    any_values.insert(*val);
                }
                _ => {}
            }
        }
    }
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                RirInst::Lt(dst, _, _)
                | RirInst::Eq(dst, _, _)
                | RirInst::FlonumLt(dst, _, _)
                | RirInst::FlonumEq(dst, _, _)
                | RirInst::FlonumIsNan(dst, _)
                | RirInst::FlonumIsInfinite(dst, _)
                | RirInst::FlonumIsFinite(dst, _)
                | RirInst::FlonumIsInteger(dst, _)
                | RirInst::PairP(dst, _)
                | RirInst::NullP(dst, _)
                | RirInst::EqAny(dst, _, _)
                | RirInst::EqualAny(dst, _, _)
                | RirInst::VecP(dst, _)
                | RirInst::StrP(dst, _)
                | RirInst::StrEq(dst, _, _)
                | RirInst::StrLt(dst, _, _)
                | RirInst::StrGt(dst, _, _)
                | RirInst::StrLe(dst, _, _)
                | RirInst::StrGe(dst, _, _)
                | RirInst::StrCiEq(dst, _, _)
                | RirInst::StrCiLt(dst, _, _)
                | RirInst::StrCiGt(dst, _, _)
                | RirInst::StrCiLe(dst, _, _)
                | RirInst::StrCiGe(dst, _, _)
                | RirInst::StringPrefixP(dst, _, _)
                | RirInst::StringSuffixP(dst, _, _)
                | RirInst::NullListP(dst, _)
                | RirInst::ProperListP(dst, _)
                | RirInst::DottedListP(dst, _)
                | RirInst::CircularListP(dst, _)
                | RirInst::NotPairP(dst, _)
                | RirInst::ListP(dst, _)
                | RirInst::CharAlphabeticP(dst, _)
                | RirInst::BitwiseBitSetP(dst, _, _)
                | RirInst::FlEvenP(dst, _)
                | RirInst::FlOddP(dst, _)
                | RirInst::InputPortP(dst, _)
                | RirInst::OutputPortP(dst, _)
                | RirInst::BinaryPortP(dst, _)
                | RirInst::TextualPortP(dst, _)
                | RirInst::OutputPortOpenP(dst, _)
                | RirInst::PortEofP(dst, _)
                | RirInst::PortHasSetPortPositionP(dst, _)
                | RirInst::PromiseP(dst, _)
                | RirInst::HashtableP(dst, _)
                | RirInst::HashtableMutableP(dst, _)
                | RirInst::HashtableContainsP(dst, _, _)
                | RirInst::ExactNonNegIntP(dst, _)
                | RirInst::BytevectorEqP(dst, _, _)
                | RirInst::VectorEqP(dst, _, _)
                | RirInst::FileExistsP(dst, _)
                | RirInst::CharNumericP(dst, _)
                | RirInst::CharWhitespaceP(dst, _)
                | RirInst::CharUpperCaseP(dst, _)
                | RirInst::CharLowerCaseP(dst, _)
                | RirInst::BvP(dst, _)
                | RirInst::ProcedureP(dst, _)
                | RirInst::PortP(dst, _)
                | RirInst::EofP(dst, _)
                | RirInst::SymbolP(dst, _)
                | RirInst::CharP(dst, _)
                | RirInst::BoolP(dst, _)
                | RirInst::FixnumP(dst, _)
                | RirInst::FlonumP(dst, _) => {
                    bool_values.insert(*dst);
                }
                RirInst::LoadConst(dst, Const::Boolean(_)) => {
                    bool_values.insert(*dst);
                }
                RirInst::IntCharBitcast(dst, _) => {
                    char_values.insert(*dst);
                }
                RirInst::LoadConst(dst, Const::Character(_)) => {
                    char_values.insert(*dst);
                }
                RirInst::StrRef(dst, _, _) => {
                    // string-ref returns a Fixnum-shape codepoint;
                    // the dispatcher decodes via JIT_RT_CHARACTER.
                    char_values.insert(*dst);
                }
                RirInst::CharUpcase(dst, _)
                | RirInst::CharDowncase(dst, _)
                | RirInst::CharFoldcase(dst, _)
                | RirInst::CharTitlecase(dst, _) => {
                    // char-upcase / char-downcase / char-foldcase /
                    // char-titlecase return a Character codepoint;
                    // dispatcher decodes via JIT_RT_CHARACTER.
                    char_values.insert(*dst);
                }
                RirInst::FixToFlo(dst, _)
                | RirInst::FlonumAdd(dst, _, _)
                | RirInst::FlonumSub(dst, _, _)
                | RirInst::FlonumMul(dst, _, _)
                | RirInst::FlonumDiv(dst, _, _)
                | RirInst::FlonumSqrt(dst, _)
                | RirInst::FlonumAbs(dst, _)
                | RirInst::FlonumMax(dst, _, _)
                | RirInst::FlonumMin(dst, _, _)
                | RirInst::FlonumFloor(dst, _)
                | RirInst::FlonumCeil(dst, _)
                | RirInst::FlonumTrunc(dst, _)
                | RirInst::FlonumRound(dst, _)
                | RirInst::FlonumSin(dst, _)
                | RirInst::FlonumCos(dst, _)
                | RirInst::FlonumTan(dst, _)
                | RirInst::FlonumLog(dst, _)
                | RirInst::FlonumExp(dst, _)
                | RirInst::FlonumAsin(dst, _)
                | RirInst::FlonumAcos(dst, _)
                | RirInst::FlonumAtan(dst, _)
                | RirInst::FlonumLog2(dst, _, _)
                | RirInst::FlonumAtan2(dst, _, _)
                | RirInst::FlonumExpt(dst, _, _)
                | RirInst::BvIeeeSingleNativeRef(dst, _, _)
                | RirInst::BvIeeeDoubleNativeRef(dst, _, _)
                | RirInst::CurrentSecond(dst) => {
                    flo_values.insert(*dst);
                }
                RirInst::LoadConst(dst, Const::Flonum(_)) => {
                    flo_values.insert(*dst);
                }
                RirInst::LoadConst(dst, Const::Null) => {
                    null_values.insert(*dst);
                }
                RirInst::LoadConst(dst, Const::Symbol(_)) => {
                    sym_values.insert(*dst);
                }
                RirInst::StringToSymbol(dst, _) => {
                    sym_values.insert(*dst);
                }
                RirInst::Cons(dst, _, _, _, _)
                | RirInst::Car(dst, _)
                | RirInst::Cdr(dst, _)
                | RirInst::AnyClone(dst, _)
                | RirInst::CallGeneral(dst, _, _)
                | RirInst::EnvLookupAny(dst, _)
                | RirInst::VecAlloc(dst, _, _)
                | RirInst::VecRef(dst, _, _)
                | RirInst::VecSet(dst, _, _, _)
                | RirInst::StrAlloc(dst, _, _)
                | RirInst::MakeClosure(dst, _)
                | RirInst::Reverse(dst, _)
                | RirInst::Memq(dst, _, _)
                | RirInst::Assq(dst, _, _)
                | RirInst::SetCar(dst, _, _)
                | RirInst::SetCdr(dst, _, _)
                | RirInst::Memv(dst, _, _)
                | RirInst::Assv(dst, _, _)
                | RirInst::Member(dst, _, _)
                | RirInst::Assoc(dst, _, _)
                | RirInst::ListTail(dst, _, _)
                | RirInst::ListRef(dst, _, _)
                | RirInst::Substring(dst, _, _, _)
                | RirInst::ListCopy(dst, _)
                | RirInst::ListSet(dst, _, _, _)
                | RirInst::BvAlloc(dst, _, _)
                | RirInst::BvU8Set(dst, _, _, _)
                | RirInst::BvS8Set(dst, _, _, _)
                | RirInst::BvU16NativeSet(dst, _, _, _)
                | RirInst::BvS16NativeSet(dst, _, _, _)
                | RirInst::BvU32NativeSet(dst, _, _, _)
                | RirInst::BvS32NativeSet(dst, _, _, _)
                | RirInst::BvIeeeSingleNativeSet(dst, _, _, _)
                | RirInst::BvIeeeDoubleNativeSet(dst, _, _, _)
                | RirInst::BvU64NativeSet(dst, _, _, _)
                | RirInst::BvS64NativeSet(dst, _, _, _)
                | RirInst::VecBuild(dst, _)
                | RirInst::StrBuild(dst, _)
                | RirInst::BvBuild(dst, _)
                | RirInst::StrAppend(dst, _)
                | RirInst::ListAppend(dst, _)
                | RirInst::VecAppend(dst, _)
                | RirInst::BvAppend(dst, _)
                | RirInst::VecFill(dst, _, _)
                | RirInst::BvFill(dst, _, _)
                | RirInst::StrSet(dst, _, _, _)
                | RirInst::StrFill(dst, _, _)
                | RirInst::StrCopy(dst, _)
                | RirInst::VecCopy(dst, _)
                | RirInst::BvCopy(dst, _)
                | RirInst::DigitValue(dst, _)
                | RirInst::VectorToList(dst, _)
                | RirInst::StringToVector(dst, _)
                | RirInst::VectorToString(dst, _)
                | RirInst::NumberToString(dst, _)
                | RirInst::StringToNumber(dst, _)
                | RirInst::StringReverse(dst, _)
                | RirInst::StringUpcase(dst, _)
                | RirInst::StringDowncase(dst, _)
                | RirInst::StringFoldcase(dst, _)
                | RirInst::StringContains(dst, _, _)
                | RirInst::StringContainsRight(dst, _, _)
                | RirInst::StringIndex(dst, _, _)
                | RirInst::StringIndexRight(dst, _, _)
                | RirInst::StringJoin(dst, _, _)
                | RirInst::StringSplit(dst, _, _)
                | RirInst::StringPad(dst, _, _)
                | RirInst::StringPadRight(dst, _, _)
                | RirInst::StringTrim(dst, _)
                | RirInst::StringTrimLeft(dst, _)
                | RirInst::StringTrimRight(dst, _)
                | RirInst::StringReplaceAll(dst, _, _, _)
                | RirInst::StringTake(dst, _, _)
                | RirInst::StringDrop(dst, _, _)
                | RirInst::StringTakeRight(dst, _, _)
                | RirInst::StringDropRight(dst, _, _)
                | RirInst::StringTitlecase(dst, _)
                | RirInst::HashtableKeys(dst, _)
                | RirInst::HashtableValues(dst, _)
                | RirInst::HashtableClear(dst, _)
                | RirInst::HashtableToAlist(dst, _)
                | RirInst::AppendReverse(dst, _, _)
                | RirInst::AlistCopy(dst, _)
                | RirInst::Delete(dst, _, _)
                | RirInst::DeleteDuplicates(dst, _)
                | RirInst::MakePromise(dst, _)
                | RirInst::ForceForced(dst, _)
                | RirInst::HashtableDelete(dst, _, _)
                | RirInst::HashtableSet(dst, _, _, _)
                | RirInst::HashtableRef(dst, _, _, _)
                | RirInst::HashtableCopy(dst, _)
                | RirInst::VecCopySlice(dst, _, _, _)
                | RirInst::VecCopyFrom(dst, _, _)
                | RirInst::BvCopyFrom(dst, _, _)
                | RirInst::StrCopyFrom(dst, _, _)
                | RirInst::BvFillFrom(dst, _, _, _)
                | RirInst::VecFillFrom(dst, _, _, _)
                | RirInst::StrFillFrom(dst, _, _, _)
                | RirInst::VectorToStringSlice(dst, _, _, _)
                | RirInst::StringToVectorSlice(dst, _, _, _)
                | RirInst::VectorToListSlice(dst, _, _, _)
                | RirInst::StringToListSlice(dst, _, _, _)
                | RirInst::BytevectorToListSlice(dst, _, _, _)
                | RirInst::NumberToStringRadix(dst, _, _)
                | RirInst::StringToNumberRadix(dst, _, _)
                | RirInst::MakeListUnspec(dst, _)
                | RirInst::MakeVectorUnspec(dst, _)
                | RirInst::VectorToListSliceFrom(dst, _, _)
                | RirInst::StringToListSliceFrom(dst, _, _)
                | RirInst::BytevectorToListSliceFrom(dst, _, _)
                | RirInst::VectorToStringSliceFrom(dst, _, _)
                | RirInst::StringToVectorSliceFrom(dst, _, _)
                | RirInst::VecCopyBangFrom(dst, _, _, _, _)
                | RirInst::BvCopyBangFrom(dst, _, _, _, _)
                | RirInst::StrCopyBangFrom(dst, _, _, _, _)
                | RirInst::VecCopyBangSlice(dst, _, _, _, _, _)
                | RirInst::BvCopyBangSlice(dst, _, _, _, _, _)
                | RirInst::StrCopyBangSlice(dst, _, _, _, _, _)
                | RirInst::BvCopySlice(dst, _, _, _)
                | RirInst::EofObject(dst)
                | RirInst::MakeHashtableEqual(dst)
                | RirInst::MakeHashtableEq(dst)
                | RirInst::MakeHashtableEqv(dst)
                | RirInst::StringReplaceFirst(dst, _, _, _)
                | RirInst::BvFillSlice(dst, _, _, _, _)
                | RirInst::VecFillSlice(dst, _, _, _, _)
                | RirInst::StrFillSlice(dst, _, _, _, _)
                | RirInst::MakeList(dst, _, _)
                | RirInst::IotaN(dst, _)
                | RirInst::IotaNs(dst, _, _)
                | RirInst::IotaNss(dst, _, _, _)
                | RirInst::LastPair(dst, _)
                | RirInst::Last(dst, _)
                | RirInst::Take(dst, _, _)
                | RirInst::Drop(dst, _, _)
                | RirInst::Concatenate(dst, _)
                | RirInst::VecCopyBang(dst, _, _, _)
                | RirInst::BvCopyBang(dst, _, _, _)
                | RirInst::StrCopyBang(dst, _, _, _)
                | RirInst::ListToVector(dst, _)
                | RirInst::StringToList(dst, _)
                | RirInst::ListToString(dst, _)
                | RirInst::SymbolToString(dst, _)
                | RirInst::BytevectorToU8List(dst, _)
                | RirInst::U8ListToBytevector(dst, _)
                | RirInst::StringToUtf8(dst, _)
                | RirInst::Utf8ToString(dst, _)
                | RirInst::HashtableHashFn(dst, _) => {
                    any_values.insert(*dst);
                }
                _ => {}
            }
        }
    }
    // CallSelf dsts inherit the function's own return type — that's
    // a fixed-point. Tracking which return values came from CallSelf
    // lets us defer their classification: if every other return is
    // uniform, the CallSelf path agrees by construction. If the
    // non-CallSelf returns are mixed (or empty), fall back to Fixnum.
    let mut callself_dsts: std::collections::HashSet<RirValue> = std::collections::HashSet::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let RirInst::CallSelf(dst, _) = inst {
                callself_dsts.insert(*dst);
            }
        }
    }
    let mut seen_fixnum = false;
    let mut seen_bool = false;
    let mut seen_char = false;
    let mut seen_flo = false;
    let mut seen_null = false;
    let mut seen_sym = false;
    let mut seen_any = false;
    let mut seen_callself = false;
    for block in &func.blocks {
        if let Term::Return(v) = &block.terminator {
            if callself_dsts.contains(v) {
                seen_callself = true;
            } else if any_values.contains(v) {
                seen_any = true;
            } else if flo_values.contains(v) {
                seen_flo = true;
            } else if char_values.contains(v) {
                seen_char = true;
            } else if bool_values.contains(v) {
                seen_bool = true;
            } else if null_values.contains(v) {
                seen_null = true;
            } else if sym_values.contains(v) {
                seen_sym = true;
            } else {
                seen_fixnum = true;
            }
        }
    }
    // Disjoint-tag inference: only resolve to a non-Fixnum tag when
    // every non-CallSelf return agrees. CallSelf returns inherit
    // the function's own type, so they don't constrain. Mixed returns
    // fall back to Fixnum (the conservative default — caller will
    // see wrapped numbers; the type guard at the dispatch site
    // catches misuse downstream rather than masking it as a wrong-
    // type Value).
    let _ = seen_callself; // tracked but not consumed beyond the inheritance contract
                           // Any wins on mixed: when the body has at least one Any-typed
                           // return path, the inferred type is Any, and the post-pass
                           // (`box_mixed_returns`) inserts BoxTyped on the immediate-typed
                           // return paths so the dispatcher always sees a Box pointer.
    if seen_any {
        return Type::Any;
    }
    match (
        seen_flo,
        seen_char,
        seen_bool,
        seen_null,
        seen_sym,
        seen_fixnum,
    ) {
        (true, false, false, false, false, false) => Type::Flonum,
        (false, true, false, false, false, false) => Type::Character,
        (false, false, true, false, false, false) => Type::Boolean,
        (false, false, false, true, false, false) => Type::Null,
        (false, false, false, false, true, false) => Type::Symbol,
        (false, false, false, false, false, false) => Type::Fixnum,
        // Single-immediate-type uniform return → that type. Any
        // disagreement → widen to Any so `box_mixed_returns`
        // inserts BoxTyped on the typed Return paths and the
        // dispatcher decodes via JIT_RT_ANY.
        (false, false, false, false, false, true) => Type::Fixnum,
        _ => Type::Any,
    }
}

/// One simulated stack slot. Either an already-bound RIR Value, or
/// the special `SelfRef` sentinel that `LoadVar(self_name)` pushes,
/// or a `BuiltinRef` for Const-folded builtin procedures (consumed
/// by a matching `Call N` to emit a specialized RIR op like
/// `Quotient` / `Remainder` / `BitAnd` etc.).
enum StackEntry {
    Value(RirValue),
    SelfRef,
    /// Captured at Const-of-Procedure time. The static str is the
    /// procedure's `name()`. Recognized names trigger specialized
    /// lowering at the matching Call; unrecognized names cause the
    /// translator to reject the function (Unsupported).
    BuiltinRef(&'static str),
}

fn pop_value(stack: &mut Vec<StackEntry>) -> Result<RirValue, TranslateError> {
    match stack.pop() {
        Some(StackEntry::Value(v)) => Ok(v),
        Some(StackEntry::SelfRef) => Err(TranslateError::Unsupported(
            "self-ref appears where a Value is required".into(),
        )),
        Some(StackEntry::BuiltinRef(name)) => Err(TranslateError::Unsupported(format!(
            "builtin `{name}` reference appears where a Value is required (passed to non-Call)"
        ))),
        None => Err(TranslateError::Invalid("stack underflow".into())),
    }
}

fn pop_two_values(stack: &mut Vec<StackEntry>) -> Result<(RirValue, RirValue), TranslateError> {
    let b = pop_value(stack)?;
    let a = pop_value(stack)?;
    Ok((a, b))
}

/// Short opcode name for diagnostics (Debug prints fields too,
/// which clutters error messages with closure indices etc.).
fn opcode_name(inst: &Inst) -> &'static str {
    match inst {
        Inst::Const(_) => "Const",
        Inst::LoadVar(_) => "LoadVar",
        Inst::SetVar(_) => "SetVar",
        Inst::DefineGlobal(_) => "DefineGlobal",
        Inst::DefineLocal(_) => "DefineLocal",
        Inst::Pop => "Pop",
        Inst::JumpIfFalse(_) => "JumpIfFalse",
        Inst::Jump(_) => "Jump",
        Inst::Call(_) => "Call",
        Inst::TailCall(_) => "TailCall",
        Inst::MakeClosure(_) => "MakeClosure",
        Inst::Return => "Return",
        Inst::AddFx2 => "AddFx2",
        Inst::SubFx2 => "SubFx2",
        Inst::MulFx2 => "MulFx2",
        Inst::LtFx2 => "LtFx2",
        Inst::LeFx2 => "LeFx2",
        Inst::GtFx2 => "GtFx2",
        Inst::GeFx2 => "GeFx2",
        Inst::EqFx2 => "EqFx2",
        Inst::BranchOnGeFx2(_) => "BranchOnGeFx2",
        Inst::BranchOnGtFx2(_) => "BranchOnGtFx2",
        Inst::BranchOnLeFx2(_) => "BranchOnLeFx2",
        Inst::BranchOnLtFx2(_) => "BranchOnLtFx2",
        Inst::BranchOnNeFx2(_) => "BranchOnNeFx2",
        Inst::EnterScope => "EnterScope",
        Inst::LeaveScope => "LeaveScope",
    }
}

fn lookup_block(
    map: &HashMap<usize, BlockId>,
    off: usize,
    label: &str,
) -> Result<BlockId, TranslateError> {
    map.get(&off)
        .copied()
        .ok_or_else(|| TranslateError::Invalid(format!("{label}: offset {off} not a block start")))
}

/// Seed a target block's entry stack with `count` fresh RIR Values
/// (allocated via `alloc`), or — if the target was already seeded
/// — verify that the count matches.
/// Extract just the `RirValue` slots from a sim_stack — Branch
/// terminators emit with the entire current stack as block-passing
/// args, so we collect their values for `seed_block_entry`. Non-Value
/// entries (SelfRef / BuiltinRef) shouldn't appear at terminator
/// positions; if one slips through it stays mapped as a fresh
/// throwaway value (defaults to Fixnum).
fn sim_stack_values(sim_stack: &[StackEntry]) -> Vec<RirValue> {
    sim_stack
        .iter()
        .filter_map(|e| match e {
            StackEntry::Value(v) => Some(*v),
            _ => None,
        })
        .collect()
}

/// RC3 iter 2.13 — at a branch point, any non-Value entries on the
/// stack (SelfRef, BuiltinRef) would be silently DROPPED by the
/// previous `sim_stack_values` filter, causing the merge-block
/// successor to see a smaller stack than the predecessor pushed. The
/// downstream `Call(n)` then trips its "stack has only K entries"
/// invariant.
///
/// Fix: materialize each marker as a concrete RIR Value before the
/// branch. SelfRef becomes an `EnvLookup(self_name)` (which lowers to
/// `__self_handle` via iter 2.12's resolver path). BuiltinRef is
/// rejected — the AOT pipeline can't materialize a builtin
/// procedure handle (would need runtime allocation + a name → handle
/// registry); programs that branch with a builtin on the stack stay
/// blocked and surface a clean UnsupportedInst at lower time.
fn materialize_markers_at_branch(
    sim_stack: &mut [StackEntry],
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    self_name: Option<Symbol>,
) -> Result<(), TranslateError> {
    for entry in sim_stack.iter_mut() {
        match entry {
            StackEntry::Value(_) => {}
            StackEntry::SelfRef => {
                let sym = self_name.ok_or_else(|| {
                    TranslateError::Invalid("SelfRef on stack at branch but no self_name".into())
                })?;
                let dst = alloc();
                insts.push(RirInst::EnvLookupAny(dst, sym.0));
                value_types.insert(dst, Type::Any);
                *entry = StackEntry::Value(dst);
            }
            StackEntry::BuiltinRef(name) => {
                return Err(TranslateError::Unsupported(format!(
                    "builtin-ref `{name}` on stack at branch (RC3 iter 2.13: \
                     materialization across blocks needs a builtin → AOT-handle \
                     registry that doesn't exist yet)"
                )));
            }
        }
    }
    Ok(())
}

fn seed_block_entry(
    entry_stack: &mut HashMap<BlockId, Vec<RirValue>>,
    block_params: &mut HashMap<BlockId, Vec<(RirValue, Type)>>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    target: BlockId,
    src_values: &[RirValue],
) -> Result<(), TranslateError> {
    let count = src_values.len();
    if let Some(existing) = entry_stack.get(&target) {
        if existing.len() != count {
            return Err(TranslateError::Invalid(format!(
                "block {:?} seeded with {} entries, predecessor wants {}",
                target,
                existing.len(),
                count
            )));
        }
        return Ok(());
    }
    // Allocate fresh block params + propagate their types from the
    // predecessor's stack. Without type propagation, every block
    // param defaulted to Fixnum even when the predecessor pushed a
    // Flonum — silently turning later flonum arithmetic into i64
    // ops with garbage results.
    let vals: Vec<RirValue> = (0..count).map(|_| alloc()).collect();
    let mut params: Vec<(RirValue, Type)> = Vec::with_capacity(count);
    for (new_val, src) in vals.iter().zip(src_values.iter()) {
        let t = value_types.get(src).copied().unwrap_or(Type::Fixnum);
        params.push((*new_val, t));
        value_types.insert(*new_val, t);
    }
    entry_stack.insert(target, vals);
    block_params.insert(target, params);
    Ok(())
}

/// Arithmetic binop emission with flonum/fixnum dispatch. When both
/// operands are typed Flonum (per `value_types`), emit the flonum
/// variant. When operand types are mixed (one Flonum, one Fixnum),
/// promote the Fixnum operand via FixToFlo and emit the Flonum op
/// — R6RS numeric-tower contagion: `(+ 1 1.0) ⇒ 2.0` not `2`. When
/// both Fixnum, fall back to the integer form. dst's type is
/// recorded in `value_types` so downstream ops can chain.
/// If `v` is `Type::Any`, emit `Inst::AnyToFix(fresh, v)` and
/// return the fresh Fixnum-typed RirValue. Otherwise return `v`
/// unchanged. The unbox is consume-on-use — `v` must not be
/// referenced after this call (the underlying box gets dropped).
fn unbox_any_to_fix(
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    v: RirValue,
) -> RirValue {
    if value_types.get(&v).copied() != Some(Type::Any) {
        return v;
    }
    let dst = alloc();
    insts.push(RirInst::AnyToFix(dst, v));
    value_types.insert(dst, Type::Fixnum);
    dst
}

/// If `v` is `Type::Any`, emit `Inst::AnyToFlo(fresh, v)` and
/// return the fresh Flonum-typed RirValue (the i64 carries the f64
/// bit pattern). Otherwise return `v` unchanged.
fn unbox_any_to_flo(
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    v: RirValue,
) -> RirValue {
    if value_types.get(&v).copied() != Some(Type::Any) {
        return v;
    }
    let dst = alloc();
    insts.push(RirInst::AnyToFlo(dst, v));
    value_types.insert(dst, Type::Flonum);
    dst
}

/// Choose the unbox target based on the *other* operand's type so
/// the result agrees with the surrounding op's signature. If the
/// other side is Flonum, unbox Any → Flo (else vm_unbox_fixnum
/// would panic on a runtime-Flonum operand). If the other side is
/// any non-Any type, unbox to Fixnum (today's default — assumes
/// Fixnum-shaped Any).
fn unbox_any_against(
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    v: RirValue,
    other_ty: Type,
) -> RirValue {
    if value_types.get(&v).copied() != Some(Type::Any) {
        return v;
    }
    if other_ty == Type::Flonum {
        unbox_any_to_flo(insts, value_types, alloc, v)
    } else {
        unbox_any_to_fix(insts, value_types, alloc, v)
    }
}

fn emit_arith_binop(
    insts: &mut Vec<RirInst>,
    stack: &mut Vec<StackEntry>,
    alloc: &mut impl FnMut() -> RirValue,
    value_types: &mut HashMap<RirValue, Type>,
    fixnum_ctor: fn(RirValue, RirValue, RirValue) -> RirInst,
    flonum_ctor: fn(RirValue, RirValue, RirValue) -> RirInst,
) -> Result<(), TranslateError> {
    // Peek raw types first so we can pick the right unbox target
    // when one operand is Any. (Any+Flonum needs AnyToFlo, else
    // vm_unbox_fixnum would panic on a runtime-Flonum operand.)
    let rhs_raw = pop_value(stack)?;
    let lhs_raw = pop_value(stack)?;
    let lt_raw = value_types.get(&lhs_raw).copied().unwrap_or(Type::Fixnum);
    let rt_raw = value_types.get(&rhs_raw).copied().unwrap_or(Type::Fixnum);
    let lhs = unbox_any_against(insts, value_types, alloc, lhs_raw, rt_raw);
    let rhs = unbox_any_against(insts, value_types, alloc, rhs_raw, lt_raw);
    let dst = alloc();
    let lt = value_types.get(&lhs).copied().unwrap_or(Type::Fixnum);
    let rt = value_types.get(&rhs).copied().unwrap_or(Type::Fixnum);
    let any_flonum = lt == Type::Flonum || rt == Type::Flonum;
    if any_flonum {
        let lhs_f = if lt == Type::Flonum {
            lhs
        } else {
            let promoted = alloc();
            insts.push(RirInst::FixToFlo(promoted, lhs));
            value_types.insert(promoted, Type::Flonum);
            promoted
        };
        let rhs_f = if rt == Type::Flonum {
            rhs
        } else {
            let promoted = alloc();
            insts.push(RirInst::FixToFlo(promoted, rhs));
            value_types.insert(promoted, Type::Flonum);
            promoted
        };
        insts.push(flonum_ctor(dst, lhs_f, rhs_f));
        value_types.insert(dst, Type::Flonum);
    } else {
        insts.push(fixnum_ctor(dst, lhs, rhs));
        value_types.insert(dst, Type::Fixnum);
    }
    stack.push(StackEntry::Value(dst));
    Ok(())
}

/// Emit a typed less-than instruction (Lt vs FlonumLt) and record
/// the dst as Boolean. Used by both `emit_cmp_binop` and the
/// BranchOn*Fx2 terminator translations where we need just the
/// comparison value for a brif, not a sim-stack push.
fn emit_typed_lt(
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    lhs: RirValue,
    rhs: RirValue,
) -> RirValue {
    let lt_raw = value_types.get(&lhs).copied().unwrap_or(Type::Fixnum);
    let rt_raw = value_types.get(&rhs).copied().unwrap_or(Type::Fixnum);
    let lhs = unbox_any_against(insts, value_types, alloc, lhs, rt_raw);
    let rhs = unbox_any_against(insts, value_types, alloc, rhs, lt_raw);
    let lt = value_types.get(&lhs).copied().unwrap_or(Type::Fixnum);
    let rt = value_types.get(&rhs).copied().unwrap_or(Type::Fixnum);
    let dst = alloc();
    let inst = if lt == Type::Flonum && rt == Type::Flonum {
        RirInst::FlonumLt(dst, lhs, rhs)
    } else {
        RirInst::Lt(dst, lhs, rhs)
    };
    insts.push(inst);
    value_types.insert(dst, Type::Boolean);
    dst
}

/// Counterpart to `emit_typed_lt` for equality.
fn emit_typed_eq(
    insts: &mut Vec<RirInst>,
    value_types: &mut HashMap<RirValue, Type>,
    alloc: &mut impl FnMut() -> RirValue,
    lhs: RirValue,
    rhs: RirValue,
) -> RirValue {
    let lt_raw = value_types.get(&lhs).copied().unwrap_or(Type::Fixnum);
    let rt_raw = value_types.get(&rhs).copied().unwrap_or(Type::Fixnum);
    let lhs = unbox_any_against(insts, value_types, alloc, lhs, rt_raw);
    let rhs = unbox_any_against(insts, value_types, alloc, rhs, lt_raw);
    let lt = value_types.get(&lhs).copied().unwrap_or(Type::Fixnum);
    let rt = value_types.get(&rhs).copied().unwrap_or(Type::Fixnum);
    let dst = alloc();
    let inst = if lt == Type::Flonum && rt == Type::Flonum {
        RirInst::FlonumEq(dst, lhs, rhs)
    } else {
        RirInst::Eq(dst, lhs, rhs)
    };
    insts.push(inst);
    value_types.insert(dst, Type::Boolean);
    dst
}

/// Comparison binop emission. Same shape as `emit_arith_binop` but
/// dst is always Boolean — the IEEE-754 / signed-integer comparison
/// produces a 0/1 i64 either way. Mixed-type compares promote the
/// Fixnum operand via FixToFlo so `(< 1 1.5)` runs through the
/// Flonum compare path, matching R6RS numeric-tower contagion.
fn emit_cmp_binop(
    insts: &mut Vec<RirInst>,
    stack: &mut Vec<StackEntry>,
    alloc: &mut impl FnMut() -> RirValue,
    value_types: &mut HashMap<RirValue, Type>,
    fixnum_ctor: fn(RirValue, RirValue, RirValue) -> RirInst,
    flonum_ctor: fn(RirValue, RirValue, RirValue) -> RirInst,
) -> Result<(), TranslateError> {
    let rhs = pop_value(stack)?;
    let lhs = pop_value(stack)?;
    let dst = alloc();
    let lt = value_types.get(&lhs).copied().unwrap_or(Type::Fixnum);
    let rt = value_types.get(&rhs).copied().unwrap_or(Type::Fixnum);
    let any_flonum = lt == Type::Flonum || rt == Type::Flonum;
    if any_flonum {
        let lhs_f = if lt == Type::Flonum {
            lhs
        } else {
            let promoted = alloc();
            insts.push(RirInst::FixToFlo(promoted, lhs));
            value_types.insert(promoted, Type::Flonum);
            promoted
        };
        let rhs_f = if rt == Type::Flonum {
            rhs
        } else {
            let promoted = alloc();
            insts.push(RirInst::FixToFlo(promoted, rhs));
            value_types.insert(promoted, Type::Flonum);
            promoted
        };
        insts.push(flonum_ctor(dst, lhs_f, rhs_f));
    } else {
        insts.push(fixnum_ctor(dst, lhs, rhs));
    }
    value_types.insert(dst, Type::Boolean);
    stack.push(StackEntry::Value(dst));
    Ok(())
}

fn value_to_const(v: &cs_core::Value) -> Result<Const, TranslateError> {
    use cs_core::Value;
    match v {
        Value::Number(cs_core::Number::Fixnum(n)) => Ok(Const::Fixnum(*n)),
        Value::Number(cs_core::Number::Flonum(f)) => Ok(Const::Flonum(*f)),
        Value::Boolean(b) => Ok(Const::Boolean(*b)),
        Value::Character(c) => Ok(Const::Character(*c)),
        Value::Null => Ok(Const::Null),
        Value::Unspecified => Ok(Const::Unspecified),
        Value::Symbol(s) => Ok(Const::Symbol(s.0)),
        // Inline string literal. Consumed by the AOT backend (which bakes
        // the content into emitted Rust). The cranelift JIT's support gate
        // declines any function carrying `Const::String`, so on the JIT
        // path this just routes the function to the VM tier — the same
        // outcome as the old "unsupported const" translate failure.
        Value::String(s) => Ok(Const::String(s.borrow().clone())),
        other => Err(TranslateError::Unsupported(format!(
            "Const value {:?} not in iter-5 scope",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::{CompiledLambda, FastPrimopBody, Inst};
    use cs_core::{Number, SymbolTable, Value};
    use std::rc::Rc;

    fn make_fib_lambda(syms: &mut SymbolTable) -> (CompiledLambda, Symbol) {
        let n = syms.intern("n");
        let fib = syms.intern("fib");
        // body:
        //   0: LoadVar n
        //   1: Const 2
        //   2: LtFx2
        //   3: JumpIfFalse 6
        //   4: LoadVar n
        //   5: Return
        //   6: LoadVar fib   ; self
        //   7: LoadVar n
        //   8: Const 1
        //   9: SubFx2
        //  10: Call 1
        //  11: LoadVar fib   ; self
        //  12: LoadVar n
        //  13: Const 2
        //  14: SubFx2
        //  15: Call 1
        //  16: AddFx2
        //  17: Return
        let body = vec![
            Inst::LoadVar(n),
            Inst::Const(Value::Number(Number::Fixnum(2))),
            Inst::LtFx2,
            Inst::JumpIfFalse(6),
            Inst::LoadVar(n),
            Inst::Return,
            Inst::LoadVar(fib),
            Inst::LoadVar(n),
            Inst::Const(Value::Number(Number::Fixnum(1))),
            Inst::SubFx2,
            Inst::Call(1),
            Inst::LoadVar(fib),
            Inst::LoadVar(n),
            Inst::Const(Value::Number(Number::Fixnum(2))),
            Inst::SubFx2,
            Inst::Call(1),
            Inst::AddFx2,
            Inst::Return,
        ];
        let len = body.len();
        let l = CompiledLambda {
            params: vec![n],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None as Option<FastPrimopBody>,
            profile: Default::default(),
        };
        (l, fib)
    }

    #[test]
    fn translate_fib_to_rir() {
        let mut syms = SymbolTable::new();
        let (lam, fib_sym) = make_fib_lambda(&mut syms);
        let f = bytecode_to_rir(&lam, "fib", Some(fib_sym)).unwrap();
        assert_eq!(f.name, "fib");
        assert_eq!(f.params.len(), 1);
        // Three blocks: entry, then-arm (returns n), else-arm (returns fib(n-1)+fib(n-2)).
        assert_eq!(f.blocks.len(), 3);
        // Entry ends in Branch.
        match &f.blocks[0].terminator {
            Term::Branch(_, _, _, _) => {}
            other => panic!("entry terminator: {:?}", other),
        }
        // The else arm body should contain at least 2 CallSelf insts.
        let else_arm = &f.blocks[2];
        let call_self_count = else_arm
            .insts
            .iter()
            .filter(|i| matches!(i, RirInst::CallSelf(_, _)))
            .count();
        assert_eq!(call_self_count, 2, "fib should produce 2 CallSelf insts");
    }

    #[test]
    fn translate_const_load_var_arith() {
        // f(x) = x + 1
        let mut syms = SymbolTable::new();
        let x = syms.intern("x");
        let body = vec![
            Inst::LoadVar(x),
            Inst::Const(Value::Number(Number::Fixnum(1))),
            Inst::AddFx2,
            Inst::Return,
        ];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![x],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
            profile: Default::default(),
        };
        let f = bytecode_to_rir(&lam, "addone", None).unwrap();
        assert_eq!(f.blocks.len(), 1);
        assert_eq!(f.blocks[0].insts.len(), 2);
        match &f.blocks[0].insts[1] {
            RirInst::Add(_, _, _) => {}
            other => panic!("expected Add, got {:?}", other),
        }
        match &f.blocks[0].terminator {
            Term::Return(_) => {}
            other => panic!("expected Return, got {:?}", other),
        }
    }

    #[test]
    fn loadvar_of_free_var_emits_envlookup() {
        // Free-var LoadVar now lowers to Inst::EnvLookup (M6 Phase 2
        // iter B). Translator accepts; the lowerer emits a Cranelift
        // call to vm_env_lookup_fixnum.
        let mut syms = SymbolTable::new();
        let foo = syms.intern("foo");
        let body = vec![Inst::LoadVar(foo), Inst::Return];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
            profile: Default::default(),
        };
        let f = bytecode_to_rir(&lam, "f", None).expect("free-var LoadVar should translate");
        // Look for the EnvLookup in block 0's insts.
        let has_envlookup = f.blocks[0]
            .insts
            .iter()
            .any(|i| matches!(i, RirInst::EnvLookup(_, _)));
        assert!(has_envlookup, "expected EnvLookup, got {:?}", f.blocks[0]);
    }

    #[test]
    fn general_call_lowers_to_call_general() {
        // ADR 0012 D-1 iter BU — previously `Unsupported` ("Call
        // with non-builtin non-self callee"); now lowers to
        // `Inst::CallGeneral`, which routes through the
        // `vm_call_general` slow-path helper at runtime.
        let mut syms = SymbolTable::new();
        let g = syms.intern("g");
        // (g 1) — calls non-self. The LoadVar(g) for a free var
        // is promoted to `EnvLookupAny` inline so the callee
        // arrives as an Any-tagged Gc handle for vm_call_general.
        let body = vec![
            Inst::LoadVar(g),
            Inst::Const(Value::Number(Number::Fixnum(1))),
            Inst::Call(1),
            Inst::Return,
        ];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![],
            rest: None,
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
            profile: Default::default(),
        };
        let f = bytecode_to_rir(&lam, "f", None).expect("translate succeeded");
        let has_general = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .any(|i| matches!(i, RirInst::CallGeneral(_, _, _)));
        assert!(
            has_general,
            "expected an Inst::CallGeneral in {:?}",
            f.blocks
        );
        let has_lookup_any = f
            .blocks
            .iter()
            .flat_map(|b| b.insts.iter())
            .any(|i| matches!(i, RirInst::EnvLookupAny(_, _)));
        assert!(
            has_lookup_any,
            "expected an Inst::EnvLookupAny in {:?}",
            f.blocks
        );
    }

    #[test]
    fn unsupported_rest_param_rejected() {
        let mut syms = SymbolTable::new();
        let rest = syms.intern("xs");
        let body = vec![Inst::Return];
        let len = body.len();
        let lam = CompiledLambda {
            params: vec![],
            rest: Some(rest),
            body: Rc::new(body),
            spans: Rc::new(vec![cs_diag::Span::DUMMY; len]),
            fast: None,
            profile: Default::default(),
        };
        match bytecode_to_rir(&lam, "f", None) {
            Err(TranslateError::Unsupported(msg)) => assert!(msg.contains("rest")),
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }
}
