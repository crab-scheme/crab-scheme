//! `cs_rir::Function` → Cranelift IR lowering.
//!
//! Iter 4b scope: pure-Fixnum function bodies with control flow
//! and self-recursion.
//!
//! - `LoadConst(Fixnum / Boolean / Null / Unspecified)`
//! - `Param`
//! - `Move`
//! - `Add`, `Sub`, `Mul`, `Lt`, `Eq` over i64
//! - `Term::Return`, `Term::Jump` (with block params),
//!   `Term::Branch`
//! - Multi-block functions (each RIR `BlockId` becomes a Cranelift
//!   `Block`; entry block carries the function-arg params, others
//!   carry IR-specified params).
//! - `Inst::CallSelf(dst, args)` — recursive self-call routed
//!   through `module.declare_func_in_func`. Lets fib / fact / any
//!   single-recursive function JIT end-to-end.
//!
//! Out of scope (planned for later iters):
//! - General `Call` (procedure-value callee with feedback)
//! - Closures / env access
//! - `DeoptCheck` (we currently trust the IR's type tags)
//! - Flonum / Boolean arithmetic (we lower booleans via i64
//!   representation 0/1 which is fine for `Lt`/`Eq` results but we
//!   don't yet specialize on `Type::Flonum`).
//!
//! Calling convention: every JITted function is exposed as
//! `extern "C" fn(i64, i64, ...) -> i64`. The runtime is responsible
//! for unboxing Scheme `Value::Number(Fixnum)` to i64 before the
//! call and re-boxing the i64 result. Richer ABIs (closure
//! self-reference, non-fixnum args) come once `Call` lands.

use std::collections::HashMap;

use cranelift_codegen::ir::{
    types::I64, AbiParam, Function as ClifFunction, InstBuilder, Signature, UserFuncName,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

use cs_jit::JitError;
#[cfg(test)]
use cs_rir::Block;
use cs_rir::{Const, Function as RirFunction, Inst, Term, Value as RirValue};

/// Owns a Cranelift `JITModule` and emits one native function per
/// `compile_pure_fixnum` call.
///
/// One module per backend instance is fine for iter 2 (we never
/// re-define the same function); iter 3 may want a per-module
/// finalize pattern for tier-up.
pub struct Lowerer {
    module: JITModule,
    ctx: Context,
    func_ctx: FunctionBuilderContext,
    next_id: u64,
    /// FuncId of the imported `vm_env_lookup_fixnum` helper.
    /// `Inst::EnvLookup` lowers to a Cranelift call against this.
    env_lookup_func: cranelift_module::FuncId,
}

impl Lowerer {
    /// Build a fresh lowerer, using the host ISA.
    pub fn new() -> Result<Self, JitError> {
        let mut flag_builder = settings::builder();
        // Use the default optimization level. Iter 2 doesn't need
        // tuning; iter 6+ (the perf iter) revisits.
        flag_builder
            .set("use_colocated_libcalls", "false")
            .map_err(|e| JitError::Codegen(format!("flag use_colocated_libcalls: {e}")))?;
        flag_builder
            .set("is_pic", "false")
            .map_err(|e| JitError::Codegen(format!("flag is_pic: {e}")))?;
        let isa_builder = cranelift_native::builder()
            .map_err(|e| JitError::Codegen(format!("native isa: {e}")))?;
        let isa = isa_builder
            .finish(settings::Flags::new(flag_builder))
            .map_err(|e| JitError::Codegen(format!("isa finish: {e}")))?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        // Register runtime helpers that JITted code calls. The
        // address comes from cs-vm's `extern "C"` function; the
        // linker resolves the symbol across workspace crates via
        // `#[no_mangle]`.
        builder.symbol(
            "vm_env_lookup_fixnum",
            cs_vm::vm::vm_env_lookup_fixnum as *const u8,
        );
        let mut module = JITModule::new(builder);

        // Import vm_env_lookup_fixnum as a Cranelift function so
        // the lowerer can call it. Signature: extern "C" fn(i64) -> i64.
        let mut env_lookup_sig = module.make_signature();
        env_lookup_sig.params.push(AbiParam::new(I64));
        env_lookup_sig.returns.push(AbiParam::new(I64));
        let env_lookup_func = module
            .declare_function(
                "vm_env_lookup_fixnum",
                cranelift_module::Linkage::Import,
                &env_lookup_sig,
            )
            .map_err(|e| JitError::Codegen(format!("declare_function env_lookup: {e}")))?;

        let ctx = module.make_context();
        Ok(Self {
            module,
            ctx,
            func_ctx: FunctionBuilderContext::new(),
            next_id: 0,
            env_lookup_func,
        })
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Compile a pure-fixnum RIR `Function` into a callable
    /// `extern "C" fn(i64, ..., i64) -> i64`. Returns the function
    /// pointer; the lowerer retains ownership of the underlying
    /// memory mapping.
    ///
    /// Iter 4 supports multi-block functions with `Branch` and
    /// `Jump` terminators. Block parameters carry per-edge values.
    /// `Call` and `DeoptCheck` are still Unsupported (iter 4b+).
    pub fn compile_pure_fixnum(&mut self, rir: &RirFunction) -> Result<*const u8, JitError> {
        if rir.blocks.is_empty() {
            return Err(JitError::Codegen("function has no blocks".into()));
        }

        // Build a Cranelift signature. Every param is i64; return is i64.
        let mut sig = Signature::new(CallConv::SystemV);
        for _ in &rir.params {
            sig.params.push(AbiParam::new(I64));
        }
        sig.returns.push(AbiParam::new(I64));

        let func_name = UserFuncName::user(0, self.fresh_id() as u32);
        let mut clif = ClifFunction::with_name_signature(func_name, sig.clone());

        // Declare the function up front so we can import a self-
        // FuncRef inside the body for `CallSelf` lowering.
        let func_id = self
            .module
            .declare_function(&rir.name, Linkage::Local, &sig)
            .map_err(|e| JitError::Codegen(format!("declare_function {}: {e}", rir.name)))?;

        // Map RIR Value -> Cranelift Value.
        let mut value_map: HashMap<RirValue, cranelift_codegen::ir::Value> = HashMap::new();

        {
            let mut builder = FunctionBuilder::new(&mut clif, &mut self.func_ctx);

            // Import a self-FuncRef for CallSelf to dispatch through.
            let self_fnref = self.module.declare_func_in_func(func_id, builder.func);
            // Import the env-lookup helper into this function.
            let env_lookup_fnref = self
                .module
                .declare_func_in_func(self.env_lookup_func, builder.func);

            // Create a Cranelift block for every RIR block, with the
            // matching block params.
            let mut block_map: HashMap<cs_rir::BlockId, cranelift_codegen::ir::Block> =
                HashMap::with_capacity(rir.blocks.len());
            for rir_block in &rir.blocks {
                let cb = builder.create_block();
                if rir_block.id == rir.entry {
                    // Entry block carries the function args via
                    // append_block_params_for_function_params; skip
                    // adding RIR-specified params (entry blocks
                    // typically have no extra params anyway).
                    builder.append_block_params_for_function_params(cb);
                } else {
                    for _ in &rir_block.params {
                        builder.append_block_param(cb, I64);
                    }
                }
                block_map.insert(rir_block.id, cb);
            }

            // Switch to the entry block and bind the function-arg
            // params to RIR Value entries.
            let entry_clif = *block_map
                .get(&rir.entry)
                .ok_or_else(|| JitError::Codegen("entry block not in block map".into()))?;
            builder.switch_to_block(entry_clif);
            let entry_params = builder.block_params(entry_clif).to_vec();
            if entry_params.len() != rir.params.len() {
                return Err(JitError::Codegen(format!(
                    "entry param count mismatch: rir={} clif={}",
                    rir.params.len(),
                    entry_params.len()
                )));
            }
            for ((rir_v, _ty), clif_v) in rir.params.iter().zip(entry_params.iter()) {
                value_map.insert(*rir_v, *clif_v);
            }

            // Lower each block's contents.
            for rir_block in &rir.blocks {
                let cb = *block_map.get(&rir_block.id).unwrap();
                builder.switch_to_block(cb);

                // Bind any non-entry RIR block params to their
                // Cranelift block params.
                if rir_block.id != rir.entry {
                    let bps = builder.block_params(cb).to_vec();
                    for ((rir_v, _ty), clif_v) in rir_block.params.iter().zip(bps.iter()) {
                        value_map.insert(*rir_v, *clif_v);
                    }
                }

                for inst in &rir_block.insts {
                    lower_inst(
                        &mut builder,
                        &mut value_map,
                        self_fnref,
                        env_lookup_fnref,
                        inst,
                    )?;
                }

                lower_terminator(&mut builder, &block_map, &value_map, &rir_block.terminator)?;
            }

            // All branches emitted; seal everything.
            builder.seal_all_blocks();
            builder.finalize();
        }

        self.ctx.func = clif;
        self.module
            .define_function(func_id, &mut self.ctx)
            .map_err(|e| JitError::Codegen(format!("define_function {}: {e}", rir.name)))?;
        self.module.clear_context(&mut self.ctx);
        self.module
            .finalize_definitions()
            .map_err(|e| JitError::Codegen(format!("finalize_definitions: {e}")))?;
        Ok(self.module.get_finalized_function(func_id))
    }

    /// Drain references to internal state. Used by tests that want
    /// to ensure module isolation between calls.
    #[doc(hidden)]
    pub fn module(&self) -> &JITModule {
        &self.module
    }
}

fn lower_terminator(
    b: &mut FunctionBuilder,
    block_map: &HashMap<cs_rir::BlockId, cranelift_codegen::ir::Block>,
    map: &HashMap<RirValue, cranelift_codegen::ir::Value>,
    term: &Term,
) -> Result<(), JitError> {
    match term {
        Term::Return(v) => {
            let cv = lookup(map, *v)?;
            b.ins().return_(&[cv]);
        }
        Term::Jump(target, args) => {
            let tb = *block_map
                .get(target)
                .ok_or_else(|| JitError::Codegen(format!("unknown jump target {:?}", target)))?;
            let cargs: Vec<cranelift_codegen::ir::BlockArg> = args
                .iter()
                .map(|a| lookup(map, *a).map(cranelift_codegen::ir::BlockArg::Value))
                .collect::<Result<_, _>>()?;
            b.ins().jump(tb, &cargs);
        }
        Term::Branch(cond, then_b, else_b) => {
            let cv = lookup(map, *cond)?;
            let tb = *block_map
                .get(then_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown then target {:?}", then_b)))?;
            let eb = *block_map
                .get(else_b)
                .ok_or_else(|| JitError::Codegen(format!("unknown else target {:?}", else_b)))?;
            b.ins().brif(cv, tb, &[], eb, &[]);
        }
    }
    Ok(())
}

fn lower_inst(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    self_fnref: cranelift_codegen::ir::FuncRef,
    env_lookup_fnref: cranelift_codegen::ir::FuncRef,
    inst: &Inst,
) -> Result<(), JitError> {
    match inst {
        Inst::LoadConst(dst, c) => {
            let v = match c {
                Const::Fixnum(n) => b.ins().iconst(I64, *n),
                Const::Boolean(true) => b.ins().iconst(I64, 1),
                Const::Boolean(false) => b.ins().iconst(I64, 0),
                Const::Null => b.ins().iconst(I64, 0),
                Const::Unspecified => b.ins().iconst(I64, 0),
                other => {
                    return Err(JitError::Unsupported(format!(
                        "LoadConst {:?} not in iter-2 scope",
                        other
                    )));
                }
            };
            map.insert(*dst, v);
        }
        Inst::Add(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().iadd(l, r))?,
        Inst::Sub(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().isub(l, r))?,
        Inst::Mul(dst, lhs, rhs) => binop(b, map, *dst, *lhs, *rhs, |b, l, r| b.ins().imul(l, r))?,
        Inst::Lt(dst, lhs, rhs) => {
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            // icmp returns an i8 0/1; widen to i64 to match the
            // calling convention.
            let cmp = b.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                l,
                r,
            );
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::Eq(dst, lhs, rhs) => {
            let l = lookup(map, *lhs)?;
            let r = lookup(map, *rhs)?;
            let cmp = b
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, l, r);
            let widened = b.ins().uextend(I64, cmp);
            map.insert(*dst, widened);
        }
        Inst::Move(dst, src) => {
            let v = lookup(map, *src)?;
            map.insert(*dst, v);
        }
        Inst::Param(_, _) => {
            // Param entries are populated from the entry block's
            // appended params before lower_inst runs.
            return Err(JitError::Codegen(
                "Inst::Param appears in block body — must be entry-only".into(),
            ));
        }
        Inst::CallSelf(dst, args) => {
            let cargs: Vec<cranelift_codegen::ir::Value> = args
                .iter()
                .map(|a| lookup(map, *a))
                .collect::<Result<_, _>>()?;
            let inst_ref = b.ins().call(self_fnref, &cargs);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "CallSelf expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::EnvLookup(dst, sym) => {
            // Pass the symbol id as i64; the helper reads
            // JIT_CALLER_ENV from a thread-local that the runtime
            // dispatch site set before the JIT call.
            let sym_v = b.ins().iconst(I64, *sym as i64);
            let inst_ref = b.ins().call(env_lookup_fnref, &[sym_v]);
            let results = b.inst_results(inst_ref);
            if results.len() != 1 {
                return Err(JitError::Codegen(format!(
                    "EnvLookup expected 1 result, got {}",
                    results.len()
                )));
            }
            map.insert(*dst, results[0]);
        }
        Inst::Call(_, _, _) | Inst::DeoptCheck(_, _) => {
            return Err(JitError::Unsupported(format!(
                "{:?} not in iter-4b scope",
                inst
            )));
        }
    }
    Ok(())
}

fn binop(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
    dst: RirValue,
    lhs: RirValue,
    rhs: RirValue,
    op: impl FnOnce(
        &mut FunctionBuilder,
        cranelift_codegen::ir::Value,
        cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value,
) -> Result<(), JitError> {
    let l = lookup(map, lhs)?;
    let r = lookup(map, rhs)?;
    let v = op(b, l, r);
    map.insert(dst, v);
    Ok(())
}

fn lookup(
    map: &HashMap<RirValue, cranelift_codegen::ir::Value>,
    v: RirValue,
) -> Result<cranelift_codegen::ir::Value, JitError> {
    map.get(&v)
        .copied()
        .ok_or_else(|| JitError::Codegen(format!("undefined RIR value {:?}", v)))
}

#[cfg(any(test, feature = "test-helpers"))]
#[doc(hidden)]
pub mod testing {
    use super::*;

    /// Build a minimal `f(a, b) = a + b` RIR Function for tests.
    pub fn add_two_fixnums() -> RirFunction {
        let mut f = RirFunction::new("add_two");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(1), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![cs_rir::Inst::Add(
                cs_rir::Value(2),
                cs_rir::Value(0),
                cs_rir::Value(1),
            )],
            terminator: cs_rir::Term::Return(cs_rir::Value(2)),
        });
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::transmute;

    #[test]
    fn lower_add_two_fixnums_runs_natively() {
        let mut lowerer = Lowerer::new().expect("Lowerer::new");
        let f = testing::add_two_fixnums();
        let ptr = lowerer
            .compile_pure_fixnum(&f)
            .expect("compile_pure_fixnum");
        // SAFETY: ptr is the address of a finalized native
        // function with the (i64, i64) -> i64 signature we declared.
        let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(2, 3), 5);
        assert_eq!(func(-7, 7), 0);
        assert_eq!(func(i64::MAX - 1, 1), i64::MAX);
    }

    #[test]
    fn lower_const_plus_param_returns_const_when_arg_zero() {
        // f(x) = x + 100
        let mut f = RirFunction::new("addconst");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(100)),
                Inst::Add(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Return(cs_rir::Value(2)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(0), 100);
        assert_eq!(func(42), 142);
    }

    #[test]
    fn lower_lt_returns_one_or_zero() {
        // f(x, y) = if x < y then 1 else 0  (encoded as Lt + Return)
        let mut f = RirFunction::new("lt");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(1), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![Inst::Lt(
                cs_rir::Value(2),
                cs_rir::Value(0),
                cs_rir::Value(1),
            )],
            terminator: Term::Return(cs_rir::Value(2)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64, i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(1, 2), 1);
        assert_eq!(func(2, 1), 0);
        assert_eq!(func(5, 5), 0);
    }

    #[test]
    fn lower_mul_and_sub_chain() {
        // f(a, b, c) = (a + b) * (a - c)
        let mut f = RirFunction::new("mulsub");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(1), cs_rir::Type::Fixnum));
        f.params.push((cs_rir::Value(2), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::Add(cs_rir::Value(3), cs_rir::Value(0), cs_rir::Value(1)),
                Inst::Sub(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(2)),
                Inst::Mul(cs_rir::Value(5), cs_rir::Value(3), cs_rir::Value(4)),
            ],
            terminator: Term::Return(cs_rir::Value(5)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64, i64, i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(5, 3, 1), (5 + 3) * (5 - 1)); // 8 * 4 = 32
        assert_eq!(func(0, 0, 0), 0);
    }

    #[test]
    fn lower_branch_two_arm_returns() {
        // f(x) = if x < 10 then x else x*2
        // entry:  cond = x < 10; brif cond, then, else
        // then:   return x
        // else:   t = x*2; return t
        let mut f = RirFunction::new("clamp");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(10)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(cs_rir::Value(2), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(cs_rir::Value(0)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(3), Const::Fixnum(2)),
                Inst::Mul(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
            ],
            terminator: Term::Return(cs_rir::Value(4)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(5), 5);
        assert_eq!(func(9), 9);
        assert_eq!(func(10), 20);
        assert_eq!(func(100), 200);
    }

    #[test]
    fn lower_jump_with_block_param() {
        // f(x) = let join_arg = (if x < 0 then -x else x) in join_arg + 1
        // entry: cond = x < 0; brif cond, neg, pos
        // neg:   t = 0 - x; jump join(t)
        // pos:   jump join(x)
        // join(p): r = p + 1; return r
        let mut f = RirFunction::new("absplus1");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(0)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(cs_rir::Value(2), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(3), Const::Fixnum(0)),
                Inst::Sub(cs_rir::Value(4), cs_rir::Value(3), cs_rir::Value(0)),
            ],
            terminator: Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(4)]),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![],
            terminator: Term::Jump(cs_rir::BlockId(3), vec![cs_rir::Value(0)]),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(3),
            params: vec![(cs_rir::Value(5), cs_rir::Type::Fixnum)],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(6), Const::Fixnum(1)),
                Inst::Add(cs_rir::Value(7), cs_rir::Value(5), cs_rir::Value(6)),
            ],
            terminator: Term::Return(cs_rir::Value(7)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(5), 6);
        assert_eq!(func(-5), 6);
        assert_eq!(func(0), 1);
        assert_eq!(func(-100), 101);
    }

    #[test]
    fn lower_self_recursive_fib() {
        // fib(n) = if n < 2 then n else fib(n-1) + fib(n-2)
        //
        // entry: cond = n < 2; brif cond, base, rec
        // base:  return n
        // rec:   one = 1; n_minus_1 = n - one
        //        a = call_self(n_minus_1)
        //        two = 2; n_minus_2 = n - two
        //        b = call_self(n_minus_2)
        //        s = a + b; return s
        let mut f = RirFunction::new("fib");
        f.params.push((cs_rir::Value(0), cs_rir::Type::Fixnum));
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(1), Const::Fixnum(2)),
                Inst::Lt(cs_rir::Value(2), cs_rir::Value(0), cs_rir::Value(1)),
            ],
            terminator: Term::Branch(cs_rir::Value(2), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(1),
            params: vec![],
            insts: vec![],
            terminator: Term::Return(cs_rir::Value(0)),
        });
        f.blocks.push(Block {
            id: cs_rir::BlockId(2),
            params: vec![],
            insts: vec![
                Inst::LoadConst(cs_rir::Value(3), Const::Fixnum(1)),
                Inst::Sub(cs_rir::Value(4), cs_rir::Value(0), cs_rir::Value(3)),
                Inst::CallSelf(cs_rir::Value(5), vec![cs_rir::Value(4)]),
                Inst::LoadConst(cs_rir::Value(6), Const::Fixnum(2)),
                Inst::Sub(cs_rir::Value(7), cs_rir::Value(0), cs_rir::Value(6)),
                Inst::CallSelf(cs_rir::Value(8), vec![cs_rir::Value(7)]),
                Inst::Add(cs_rir::Value(9), cs_rir::Value(5), cs_rir::Value(8)),
            ],
            terminator: Term::Return(cs_rir::Value(9)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        let ptr = lowerer.compile_pure_fixnum(&f).unwrap();
        let func: extern "C" fn(i64) -> i64 = unsafe { transmute(ptr) };
        assert_eq!(func(0), 0);
        assert_eq!(func(1), 1);
        assert_eq!(func(2), 1);
        assert_eq!(func(5), 5);
        assert_eq!(func(10), 55);
        assert_eq!(func(20), 6765);
    }

    #[test]
    fn empty_function_rejected() {
        let f = RirFunction::new("empty");
        let mut lowerer = Lowerer::new().unwrap();
        match lowerer.compile_pure_fixnum(&f) {
            Err(JitError::Codegen(msg)) => assert!(msg.contains("no blocks")),
            other => panic!("expected Codegen error, got {:?}", other),
        }
    }
}
