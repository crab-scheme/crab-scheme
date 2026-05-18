//! Wasmtime + WASI integration internals for cs-sandbox-wasm.
//!
//! Iter 1 shipped the type surface + wasmtime smoke (inline WAT
//! module). Iter 1.5 adds the actual crabscheme.wasm load + WASI
//! ctx + stdin/stdout text-protocol exchange.
//!
//! ## Protocol shape (iter 1.5 implementation)
//!
//! crabscheme.wasm accepts `--eval <expr>` via argv and prints
//! just the result value to stdout (or a diagnostic to stderr on
//! error). To eval `(+ 1 2 3)`:
//!
//! 1. Build a fresh `WasiCtx` with:
//!    - argv = `["crabscheme", "--eval", "(+ 1 2 3)"]`
//!    - stdin: empty `MemoryInputPipe`
//!    - stdout: `MemoryOutputPipe` (host reads after run)
//!    - stderr: `MemoryOutputPipe`
//!    - preopened dirs from `config.allow_paths`
//! 2. Instantiate the cached `crabscheme.wasm` Module against
//!    the linker; call `_start`.
//! 3. Drain stdout; trim trailing newline; that's the result.
//! 4. If the guest exited non-zero or `_start` trapped, read
//!    stderr for the diagnostic and wrap in `SandboxError`.
//!
//! Iter 6 evaluates whether to migrate to a component-model
//! interface; for now the argv + stdout text protocol mirrors
//! what `wasmtime run crabscheme.wasm --eval EXPR` does
//! interactively.

use std::path::PathBuf;

use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

use crate::{SandboxConfig, SandboxError};

/// Wasmtime engine + cached Module + per-config setup.
/// Long-lived; a per-eval Store is built on each
/// `eval_via_protocol` call.
pub struct SandboxRuntime {
    engine: Engine,
    /// Cached compiled module from `config.binary_path`. `None`
    /// when the config doesn't specify a binary (iter 1 path:
    /// type surface only).
    module: Option<Module>,
    memory_limit: usize,
    fuel: Option<u64>,
    allow_paths: Vec<PathBuf>,
}

/// Per-Store data: the WASI ctx + the resource limiter.
struct StoreData {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

impl StoreData {
    fn limiter(state: &mut Self) -> &mut dyn wasmtime::ResourceLimiter {
        &mut state.limits
    }
}

impl SandboxRuntime {
    /// Build a new runtime per `config`. Compiles
    /// `config.binary_path`'s Module lazily; if the path isn't
    /// set (iter 1 type-only path) the engine is built but no
    /// module is cached. eval_via_protocol then errors with a
    /// clear "no binary path configured" message.
    pub fn new(config: &SandboxConfig) -> Result<Self, SandboxError> {
        let mut wasm_config = Config::new();
        wasm_config.consume_fuel(config.fuel.is_some());
        if config.epoch_tick_interval.is_some() {
            wasm_config.epoch_interruption(true);
        }
        let engine = Engine::new(&wasm_config)
            .map_err(|e| SandboxError::Internal(format!("wasmtime Engine::new failed: {}", e)))?;
        // Pre-compile the binary if one is configured. Iter 1
        // tests pass binary_path=None; iter 1.5 tests set it.
        let module = match &config.binary_path {
            Some(path) => Some(Module::from_file(&engine, path).map_err(|e| {
                SandboxError::Internal(format!("Module::from_file({:?}) failed: {}", path, e))
            })?),
            None => None,
        };
        Ok(Self {
            engine,
            module,
            memory_limit: config.memory_limit,
            fuel: config.fuel,
            allow_paths: config.allow_paths.clone(),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn store_limits(&self) -> StoreLimits {
        StoreLimitsBuilder::new()
            .memory_size(self.memory_limit)
            .build()
    }

    pub fn fuel(&self) -> Option<u64> {
        self.fuel
    }

    /// Run a single `--eval <expr>` cycle through the cached
    /// crabscheme.wasm module. Returns the captured stdout text
    /// (trimmed). Iter 1.5 entry point.
    pub fn eval_via_protocol(&self, expr_source: &str) -> Result<String, SandboxError> {
        let module = self.module.as_ref().ok_or_else(|| {
            SandboxError::Internal(
                "SandboxConfig.binary_path not set — set it to the path of \
                 crabscheme.wasm built via `cargo build --release --target \
                 wasm32-wasip1 --no-default-features --bin crabscheme`"
                    .into(),
            )
        })?;

        // Build WASI ctx with --eval EXPR argv and captured stdio.
        let stdout = MemoryOutputPipe::new(1024 * 1024); // 1 MiB cap
        let stderr = MemoryOutputPipe::new(1024 * 1024);
        let stdin = MemoryInputPipe::new(Vec::new());
        let mut wasi_builder = WasiCtxBuilder::new();
        wasi_builder
            .stdin(stdin)
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .arg("crabscheme")
            .arg("--eval")
            .arg(expr_source);
        for path in &self.allow_paths {
            // Map each allow_path to itself in the guest's
            // filesystem namespace. Full DirPerms + FilePerms
            // mirror `wasmtime run --dir=PATH`; finer-grained
            // grant policies can be exposed via SandboxConfig
            // in a future iter.
            let path_str = path.to_string_lossy().to_string();
            wasi_builder
                .preopened_dir(
                    path,
                    &path_str,
                    wasmtime_wasi::DirPerms::all(),
                    wasmtime_wasi::FilePerms::all(),
                )
                .map_err(|e| {
                    SandboxError::CapabilityDenied(format!("preopen {:?}: {}", path, e))
                })?;
        }
        let wasi = wasi_builder.build_p1();

        let data = StoreData {
            wasi,
            limits: self.store_limits(),
        };
        let mut store = Store::new(&self.engine, data);
        store.limiter(StoreData::limiter);
        if let Some(fuel) = self.fuel {
            store
                .set_fuel(fuel)
                .map_err(|e| SandboxError::Internal(format!("set_fuel: {}", e)))?;
        }

        // Linker carrying WASI preview1 imports.
        let mut linker: Linker<StoreData> = Linker::new(&self.engine);
        preview1::add_to_linker_sync(&mut linker, |d: &mut StoreData| &mut d.wasi)
            .map_err(|e| SandboxError::Internal(format!("preview1 linker: {}", e)))?;

        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| SandboxError::Internal(format!("instantiate: {}", e)))?;
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| SandboxError::Internal(format!("get _start: {}", e)))?;

        let call_result = start.call(&mut store, ());
        // Drain stdout regardless of trap so we can return
        // partial output if the guest printed before failing.
        let out = stdout.contents();
        let err = stderr.contents();

        if let Err(trap) = call_result {
            let trap_str = format!("{}", trap);
            // Distinguish fuel/memory exhaustion from generic
            // trap by inspecting the message — wasmtime's traps
            // carry distinct text for each.
            if trap_str.contains("all fuel consumed") {
                return Err(SandboxError::FuelExhausted);
            }
            if trap_str.contains("memory") && trap_str.contains("limit") {
                return Err(SandboxError::MemoryExhausted);
            }
            // WASI exit(0) is normal — caught as a special trap
            // type. Anything else is a real error.
            if let Some(exit) = trap.downcast_ref::<wasmtime_wasi::I32Exit>() {
                if exit.0 == 0 {
                    return Ok(parse_stdout_into_result(&out));
                } else {
                    let err_text = String::from_utf8_lossy(&err).to_string();
                    return Err(SandboxError::GuestRaised(format!(
                        "guest exit({}): {}",
                        exit.0, err_text
                    )));
                }
            }
            return Err(SandboxError::Internal(trap_str));
        }
        Ok(parse_stdout_into_result(&out))
    }
}

/// Convert the captured stdout bytes into the result string.
/// `--eval <expr>` prints just the value; trim trailing newline.
fn parse_stdout_into_result(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim_end_matches('\n').to_string()
}

/// Iter 1 smoke: compile + instantiate a trivial WAT module
/// against the runtime's engine, call its `add` export with two
/// i32 args, return the sum. Pre-existing from iter 1; retained
/// so the iter-1 smoke tests still pass.
pub fn verify_wasmtime_integration(
    runtime: &SandboxRuntime,
    a: i32,
    b: i32,
) -> Result<i32, SandboxError> {
    const WAT: &str = r#"
        (module
          (func (export "add") (param i32 i32) (result i32)
            local.get 0
            local.get 1
            i32.add))
    "#;
    let module = Module::new(runtime.engine(), WAT.as_bytes())
        .map_err(|e| SandboxError::Internal(format!("Module::new failed: {}", e)))?;
    let mut store = Store::new(runtime.engine(), ());
    if let Some(fuel) = runtime.fuel() {
        store
            .set_fuel(fuel)
            .map_err(|e| SandboxError::Internal(format!("Store::set_fuel failed: {}", e)))?;
    }
    let instance = wasmtime::Instance::new(&mut store, &module, &[])
        .map_err(|e| SandboxError::Internal(format!("Instance::new failed: {}", e)))?;
    let add = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "add")
        .map_err(|e| SandboxError::Internal(format!("get_typed_func failed: {}", e)))?;
    add.call(&mut store, (a, b))
        .map_err(|e| SandboxError::Internal(format!("call failed: {}", e)))
}
