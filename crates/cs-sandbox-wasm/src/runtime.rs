//! Wasmtime integration internals for cs-sandbox-wasm.
//!
//! Iter 1 scope: prove the wasmtime 36.x APIs work as the ADR
//! 0015 design assumes, without requiring the actual
//! crabscheme.wasm binary.
//!
//! `SandboxRuntime` builds an `Engine` + `Linker` per a
//! SandboxConfig, configuring resource limits (memory via
//! ResourceLimiter, CPU via fuel or epoch interruption). It
//! exposes a `verify_wasmtime_integration` helper that the
//! tests use to smoke-test the Rust-side integration: compile a
//! trivial inline WAT module, instantiate it, call an exported
//! function, return the result. No WASI, no crabscheme.wasm
//! — just enough to prove the Engine/Module/Linker/Store/Instance
//! dance works.
//!
//! Iter 1.5 will extend `SandboxRuntime` to load
//! crabscheme.wasm, set up the WASI context, and implement the
//! stdin/stdout text protocol.

use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::{SandboxConfig, SandboxError};

/// Wasmtime engine + per-config setup. Long-lived; an
/// `Instance` is created from this per-eval (or once if
/// reuse_instance).
pub struct SandboxRuntime {
    engine: Engine,
    memory_limit: usize,
    fuel: Option<u64>,
}

impl SandboxRuntime {
    /// Build a new runtime per `config`. Iter 1 doesn't load any
    /// module here — `verify_wasmtime_integration` does that
    /// per-test; iter 1.5 loads crabscheme.wasm at this point.
    pub fn new(config: &SandboxConfig) -> Result<Self, SandboxError> {
        let mut wasm_config = Config::new();
        wasm_config.consume_fuel(config.fuel.is_some());
        if config.epoch_tick_interval.is_some() {
            wasm_config.epoch_interruption(true);
        }
        // Cranelift's "speed" optimization level is the default
        // and the right choice for sandbox workloads where we
        // amortize compile time over many eval calls (reuse
        // instance) or care about per-eval throughput rather
        // than startup latency.
        let engine = Engine::new(&wasm_config)
            .map_err(|e| SandboxError::Internal(format!("wasmtime Engine::new failed: {}", e)))?;
        Ok(Self {
            engine,
            memory_limit: config.memory_limit,
            fuel: config.fuel,
        })
    }

    /// Borrow the underlying engine. Iter 1.5 uses this to
    /// compile the crabscheme.wasm Module.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Build a `StoreLimits` honoring this runtime's memory
    /// limit. Iter 1.5 attaches this via `Store::limiter`.
    pub fn store_limits(&self) -> StoreLimits {
        StoreLimitsBuilder::new()
            .memory_size(self.memory_limit)
            .build()
    }

    /// The fuel budget per `Store::set_fuel` call (when fuel is
    /// enabled).
    pub fn fuel(&self) -> Option<u64> {
        self.fuel
    }
}

/// Smoke test: compile + instantiate a trivial WAT module
/// against the runtime's engine, call its `add` export with two
/// i32 args, return the sum. Used by iter-1 tests to prove the
/// wasmtime integration works end-to-end without needing the
/// crabscheme.wasm binary.
///
/// Returns `Err(SandboxError::Internal(...))` on any wasmtime-
/// side failure; `Ok(sum)` on success.
pub fn verify_wasmtime_integration(
    runtime: &SandboxRuntime,
    a: i32,
    b: i32,
) -> Result<i32, SandboxError> {
    // Inline WAT: a module with one exported function that adds
    // its two i32 parameters. No imports, no WASI, no memory.
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
    // Honor the fuel budget if configured (proves the fuel API
    // path even though `add` consumes negligible fuel).
    if let Some(fuel) = runtime.fuel() {
        store
            .set_fuel(fuel)
            .map_err(|e| SandboxError::Internal(format!("Store::set_fuel failed: {}", e)))?;
    }
    let instance = Instance::new(&mut store, &module, &[])
        .map_err(|e| SandboxError::Internal(format!("Instance::new failed: {}", e)))?;
    let add = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "add")
        .map_err(|e| SandboxError::Internal(format!("get_typed_func failed: {}", e)))?;
    add.call(&mut store, (a, b))
        .map_err(|e| SandboxError::Internal(format!("call failed: {}", e)))
}
