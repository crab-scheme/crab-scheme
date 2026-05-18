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
use std::time::Duration;

use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
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
    /// L1-inside-L2 defense in depth: the guest's expression is
    /// wrapped in `(eval (read) (environment IMPORTS...))` so the
    /// L1 namespace restriction layer fires even when the host
    /// accidentally grants too-much WASI capability. Snapshot
    /// taken at runtime construction; (reset-sandbox!) picks up
    /// any config changes.
    imports: Vec<String>,
    /// Wall-clock cancel deadline per eval. Enforced via
    /// wasmtime epoch interruption + a one-shot ticker thread
    /// per call. Validated > 0 at SandboxConfig::validate.
    wall_clock_timeout: Duration,
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
        // Epoch interruption is the mechanism for BOTH the
        // optional `epoch_tick_interval` CPU bound AND the
        // mandatory wall-clock timeout enforcement. Always-on;
        // the per-call ticker is what differs.
        wasm_config.epoch_interruption(true);
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
            imports: config.imports.clone(),
            wall_clock_timeout: config.wall_clock_timeout,
        })
    }

    /// Wasmtime Engine for internal crate use (smoke helper +
    /// per-call Store construction). Not part of the public API
    /// — embedders should construct `SandboxInstance` instead of
    /// poking at the engine directly. `pub(crate)` keeps tests
    /// honest about the supported surface.
    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Build a fresh per-Store resource limiter from the runtime's
    /// configured memory cap. Internal — see [`engine`].
    pub(crate) fn store_limits(&self) -> StoreLimits {
        StoreLimitsBuilder::new()
            .memory_size(self.memory_limit)
            .build()
    }

    /// Configured per-call wasm fuel, if any. Internal — see
    /// [`engine`].
    pub(crate) fn fuel(&self) -> Option<u64> {
        self.fuel
    }

    /// Run a single eval cycle through the cached crabscheme.wasm
    /// module. Returns the captured stdout text (trimmed).
    ///
    /// Protocol (iter 1.5 + iter-5 wrap + post-review hardening):
    /// - L1 wrap is `(eval (with-input-from-string "USER" read)
    ///   (environment IMPORTS...))`. The user expression is
    ///   embedded as a properly-escaped string literal inside the
    ///   wrapper, then parsed by the guest's `read` against a
    ///   fresh string port. This eliminates the string-paste
    ///   escape risk from earlier iters: a malformed user
    ///   expression fails the guest's READ rather than rewriting
    ///   the wrapper. (Earlier iters used naked `'USER` paste
    ///   which a crafted `1) (display 'pwned` could break.)
    /// - Wall-clock timeout is enforced via wasmtime epoch
    ///   interruption + a per-call ticker thread; on expiry the
    ///   guest traps with `Trap::Interrupt` which we map to
    ///   `SandboxError::Timeout`.
    /// - Trap classification uses `wasmtime::Trap` discriminants
    ///   (`OutOfFuel`, `MemoryOutOfBounds`, `Interrupt`) rather
    ///   than string matching on the error message.
    pub fn eval_via_protocol(&self, expr_source: &str) -> Result<String, SandboxError> {
        if expr_source.is_empty() {
            return Err(SandboxError::ProtocolError(
                "expr_source is empty; nothing to read".into(),
            ));
        }
        let module = self.module.as_ref().ok_or_else(|| {
            SandboxError::Internal(
                "SandboxConfig.binary_path not set — set it to the path of \
                 crabscheme.wasm built via `cargo build --release --target \
                 wasm32-wasip1 --no-default-features --bin crabscheme`"
                    .into(),
            )
        })?;

        // Defense-in-depth wrap — see fn doc above. Host owns
        // the entire wrapper text in argv; the user expression is
        // embedded as a properly-escaped Scheme string literal.
        let wrapper = build_l1_wrapper_argv(expr_source, &self.imports);

        let stdin = MemoryInputPipe::new(Vec::<u8>::new());
        let stdout = MemoryOutputPipe::new(1024 * 1024); // 1 MiB cap
        let stderr = MemoryOutputPipe::new(1024 * 1024);

        let mut wasi_builder = WasiCtxBuilder::new();
        wasi_builder
            .stdin(stdin)
            .stdout(stdout.clone())
            .stderr(stderr.clone())
            .arg("crabscheme")
            .arg("--eval")
            .arg(&wrapper);
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
        // Wall-clock enforcement: arm an epoch deadline of
        // current+1 and spawn a ticker thread that bumps the
        // engine's epoch after `wall_clock_timeout`. On expiry
        // the guest traps with `Trap::Interrupt`. The ticker
        // continues sleeping if the call finishes first; that's
        // wasted but harmless. A cancellable ticker is a
        // future optimization.
        store.set_epoch_deadline(1);
        let timeout = self.wall_clock_timeout;
        let engine_for_ticker = self.engine.clone();
        let ticker_handle = std::thread::spawn(move || {
            std::thread::sleep(timeout);
            engine_for_ticker.increment_epoch();
        });

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
        // Drain stdio regardless of trap so we can return partial
        // output if the guest printed before failing.
        let out = stdout.contents();
        let err = stderr.contents();

        // Don't wait on the ticker; it'll either fire harmlessly
        // or already fired (in which case join is instant). join
        // discards any panic from the ticker thread.
        let _ = ticker_handle;

        if let Err(e) = call_result {
            // Discriminate by trap kind (API-stable) before
            // falling back to the I32Exit / generic-error paths.
            if let Some(trap) = e.downcast_ref::<Trap>() {
                match *trap {
                    Trap::OutOfFuel => return Err(SandboxError::FuelExhausted),
                    Trap::MemoryOutOfBounds => return Err(SandboxError::MemoryExhausted),
                    Trap::Interrupt => return Err(SandboxError::Timeout),
                    _ => { /* fall through to generic Internal */ }
                }
            }
            // WASI exit(0) is normal — return captured stdout.
            // Non-zero exit means the guest raised; surface the
            // stderr text as the diagnostic.
            if let Some(exit) = e.downcast_ref::<wasmtime_wasi::I32Exit>() {
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
            return Err(SandboxError::Internal(format!("{}", e)));
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

/// Post-review iter 1.5 hardening — build the L1 wrapper that
/// runs entirely from host-controlled text. The user expression
/// is embedded as a Scheme STRING LITERAL (properly escaped) and
/// parsed at guest runtime via `with-input-from-string` + `read`.
/// The wrapper grammar is host-owned; the user text appears only
/// as data inside string-literal quotes, so no closing paren or
/// quote in the user input can break out of the wrap.
///
/// `imports` is a list of import-spec strings — each one is a
/// Scheme datum like `"(rnrs base)"` that gets pasted verbatim
/// (these are host-controlled, not user input). The default
/// `["(rnrs base)"]` produces:
///
/// ```text
/// (eval (with-input-from-string "<escaped user>" read)
///       (environment '(rnrs base)))
/// ```
fn build_l1_wrapper_argv(user_expr: &str, imports: &[String]) -> String {
    let escaped = escape_scheme_string(user_expr);
    let mut s = String::with_capacity(escaped.len() + 96 + imports.len() * 32);
    s.push_str("(eval (with-input-from-string \"");
    s.push_str(&escaped);
    s.push_str("\" read) (environment");
    for spec in imports {
        s.push_str(" '");
        s.push_str(spec);
    }
    s.push_str("))");
    s
}

/// Escape a string for embedding inside a Scheme string literal.
/// R6RS string-literal grammar requires `\` and `"` to be
/// escaped; we also escape control characters defensively so
/// raw newlines / tabs in the user expression don't surprise
/// the guest's reader.
fn escape_scheme_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
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
    // Engine has epoch_interruption(true) always-on for the
    // wall-clock path; a store without an explicit deadline
    // traps on the first instruction. Push the deadline out
    // of the way for the smoke (no host-side ticker armed).
    store.set_epoch_deadline(u64::MAX);
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
