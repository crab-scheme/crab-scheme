//! `cs_rir::Function` → Cranelift IR lowering.
//!
//! Iter 2 scope: enough to JIT a Fixnum-typed pure-arithmetic
//! function. Specifically:
//!
//! - `LoadConst(Fixnum)` and `LoadConst(Boolean)` and `LoadConst(Null)`
//! - `Param`
//! - `Move`
//! - `Add`, `Sub`, `Mul`, `Lt`, `Eq` over i64
//! - `Term::Return`
//!
//! Out of scope this iter (planned for iter 3+):
//! - `Branch` / `Jump` / blocks beyond entry
//! - `Call` / closures / env access
//! - `DeoptCheck` (we currently trust the IR's type tags)
//! - Flonum / Boolean arithmetic (we lower booleans via i64
//!   representation 0/1 which is fine for `Lt`/`Eq` results but we
//!   don't yet specialize on `Type::Flonum`).
//!
//! Calling convention: every JITted function is exposed as
//! `extern "C" fn(i64, i64, ...) -> i64`. The runtime is responsible
//! for unboxing Scheme `Value::Number(Fixnum)` to i64 before the
//! call and re-boxing the i64 result. Iter 3+ extends this to
//! richer ABIs.

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
        let builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        let module = JITModule::new(builder);
        let ctx = module.make_context();
        Ok(Self {
            module,
            ctx,
            func_ctx: FunctionBuilderContext::new(),
            next_id: 0,
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
    /// Iter 2 only supports a single-block function. Multi-block
    /// (Branch / Jump terminators) lands in iter 3.
    pub fn compile_pure_fixnum(&mut self, rir: &RirFunction) -> Result<*const u8, JitError> {
        // Validate scope.
        if rir.blocks.len() != 1 {
            return Err(JitError::Unsupported(format!(
                "iter 2 lowerer accepts single-block functions only; got {}",
                rir.blocks.len()
            )));
        }
        let block = &rir.blocks[0];

        // Build a Cranelift signature. Every param is i64; return is i64.
        let mut sig = Signature::new(CallConv::SystemV);
        for _ in &rir.params {
            sig.params.push(AbiParam::new(I64));
        }
        sig.returns.push(AbiParam::new(I64));

        let func_name = UserFuncName::user(0, self.fresh_id() as u32);
        let mut clif = ClifFunction::with_name_signature(func_name, sig.clone());

        // Map RIR Value -> Cranelift Value.
        let mut value_map: HashMap<RirValue, cranelift_codegen::ir::Value> = HashMap::new();

        {
            let mut builder = FunctionBuilder::new(&mut clif, &mut self.func_ctx);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            builder.seal_block(entry);

            // Map RIR params to Cranelift block params.
            let entry_params = builder.block_params(entry).to_vec();
            if entry_params.len() != rir.params.len() {
                return Err(JitError::Codegen(format!(
                    "param count mismatch: rir={} clif={}",
                    rir.params.len(),
                    entry_params.len()
                )));
            }
            for ((rir_v, _ty), clif_v) in rir.params.iter().zip(entry_params.iter()) {
                value_map.insert(*rir_v, *clif_v);
            }

            // Lower instructions.
            for inst in &block.insts {
                lower_inst(&mut builder, &mut value_map, inst)?;
            }

            // Lower terminator.
            match &block.terminator {
                Term::Return(v) => {
                    let cv = value_map.get(v).copied().ok_or_else(|| {
                        JitError::Codegen(format!("undefined return value {:?}", v))
                    })?;
                    builder.ins().return_(&[cv]);
                }
                Term::Jump(_, _) | Term::Branch(_, _, _) => {
                    return Err(JitError::Unsupported(
                        "Branch/Jump terminators land in iter 3".into(),
                    ));
                }
            }

            builder.finalize();
        }

        // Hand the populated function to the JIT module.
        let func_id = self
            .module
            .declare_function(&rir.name, Linkage::Local, &sig)
            .map_err(|e| JitError::Codegen(format!("declare_function {}: {e}", rir.name)))?;
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

fn lower_inst(
    b: &mut FunctionBuilder,
    map: &mut HashMap<RirValue, cranelift_codegen::ir::Value>,
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
        Inst::Call(_, _, _) | Inst::DeoptCheck(_, _) => {
            return Err(JitError::Unsupported(format!(
                "{:?} not in iter-2 scope",
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
    fn unsupported_branch_terminator_rejected() {
        let mut f = RirFunction::new("with_branch");
        f.entry = cs_rir::BlockId(0);
        f.blocks.push(Block {
            id: cs_rir::BlockId(0),
            params: vec![],
            insts: vec![Inst::LoadConst(cs_rir::Value(0), Const::Boolean(true))],
            terminator: Term::Branch(cs_rir::Value(0), cs_rir::BlockId(1), cs_rir::BlockId(2)),
        });
        let mut lowerer = Lowerer::new().unwrap();
        match lowerer.compile_pure_fixnum(&f) {
            Err(JitError::Unsupported(msg)) => assert!(msg.contains("Branch")),
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }
}
