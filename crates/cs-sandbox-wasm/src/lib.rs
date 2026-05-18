//! cs-sandbox-wasm — WASM-instance sandbox for CrabScheme.
//!
//! Implements ADR 0015 L2: a real capability-based isolation
//! boundary built on wasmtime + WASI. The host crabscheme spawns
//! a wasmtime Instance of the no-default-features
//! `crabscheme.wasm` binary, constrains its WASI capabilities,
//! and communicates via a stdin/stdout text protocol.
//!
//! ## Iter 1 scope (this version)
//!
//! - Complete type surface: [`SandboxConfig`], [`SandboxInstance`],
//!   [`SandboxError`], the three named presets ([`SandboxConfig::hygiene`],
//!   [`SandboxConfig::plugin`], [`SandboxConfig::adversarial`])
//! - Wasmtime 36.x LTS integration smoke: an in-process WASM
//!   Engine + Module + Linker dance proving the embedding works
//! - Resource limits: memory (via `ResourceLimiter`), CPU (fuel
//!   or epoch interruption), per-call wall-clock timeout
//!
//! ## NOT in iter 1 (tracked as iter 1.5 follow-up)
//!
//! - Loading the actual `crabscheme.wasm` binary. Requires CI
//!   provisioning of the wasm32-wasip1 build. The
//!   [`SandboxInstance::eval`] surface is shipped here returning
//!   a clear "not yet integrated" error; iter 1.5 wires the
//!   real protocol against the prebuilt binary.
//! - The stdin/stdout text protocol decode side. The framing
//!   format is documented at module top; the parser lands in
//!   iter 1.5 alongside the binary integration.
//!
//! ## Why the split
//!
//! Iter 1 proves the *Rust* side of the integration — that
//! wasmtime 36.x's APIs work as the ADR's design assumes, that
//! the resource-limit story holds together, that the
//! SandboxConfig presets compile and round-trip. Once that's
//! locked in, iter 1.5 wires the crabscheme.wasm binary without
//! any further design churn. The split lets iter 1 ship without
//! a hard CI dependency on the WASM build.
//!
//! ## Wire protocol (decode side lands in iter 1.5)
//!
//! Newline-framed text on stdin/stdout:
//!
//! ```text
//! > EVAL <length>
//! > <s-expression source for length bytes>
//! < OK <length>
//! < <s-expression of result>
//! ```
//!
//! Or on error:
//! ```text
//! < ERR <kind> <length>
//! < <s-expression of condition record>
//! ```
//!
//! `<kind>` is one of `raised`, `fuel`, `memory`, `capability`,
//! `internal`.

use std::path::PathBuf;
use std::time::Duration;

mod runtime;

pub use runtime::{verify_wasmtime_integration, SandboxRuntime};

// ---- SandboxConfig ----

/// Configuration for a single [`SandboxInstance`]. Construct via
/// one of the named presets and override specific fields.
///
/// The presets match the three threat models the user resolved
/// in the ADR's Q1 interview: `hygiene` (friendly code,
/// state-preserving), `plugin` (third-party untrusted but
/// long-running), `adversarial` (per-eval-fresh, strict caps).
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Max linear memory the guest can allocate (bytes).
    pub memory_limit: usize,

    /// Wasmtime fuel — roughly 1 unit per executed Wasm
    /// instruction. `None` = unlimited (don't use for
    /// adversarial code). Deterministic; same program hits the
    /// same trap point regardless of host load.
    pub fuel: Option<u64>,

    /// Cheaper alternative to fuel: epoch interruption. Host
    /// ticks an epoch counter at this interval; guest traps when
    /// its store's epoch deadline expires. Non-deterministic but
    /// much lower per-instruction overhead. `None` = disabled.
    /// Mutually exclusive with `fuel` in the default presets —
    /// pick one.
    pub epoch_tick_interval: Option<Duration>,

    /// Paths the guest can read/write. Empty = no filesystem.
    pub allow_paths: Vec<PathBuf>,

    /// Whether to grant network capabilities (wasi-sockets, WASI
    /// 0.2). Default false for all presets.
    pub allow_network: bool,

    /// Wall-clock timeout for a single sandbox-eval call.
    /// Independent of fuel — covers I/O stalls.
    pub wall_clock_timeout: Duration,

    /// Initial library import-spec the guest's eval will see
    /// inside the L1.1/L1.2 `(environment ...)` machinery.
    /// Default: `("(rnrs base)")`.
    pub imports: Vec<String>,

    /// Whether to reuse the same wasmtime Instance across
    /// multiple sandbox-eval calls on this SandboxInstance.
    /// - true: REPL-like; bindings/state persist across calls;
    ///   faster per-call. Recommended for friendly threat
    ///   models.
    /// - false: each eval spawns a fresh wasmtime Instance;
    ///   no cross-call state. Slower per-call but strongest
    ///   isolation. Recommended for adversarial use cases.
    pub reuse_instance: bool,

    /// Filesystem path to the `crabscheme.wasm` binary. Iter 1
    /// stub: this field exists in the API surface but
    /// `SandboxInstance::new` doesn't yet require it. Iter 1.5
    /// makes this mandatory when the eval-protocol path lands.
    pub binary_path: Option<PathBuf>,
}

impl SandboxConfig {
    /// Hygiene-only L2 wrapper. Friendly code; cross-call state
    /// preserved (REPL feel). reuse_instance=true, fuel=None,
    /// 5min wall-clock, allow_paths empty, allow_network=false.
    pub fn hygiene() -> Self {
        Self {
            memory_limit: 256 * 1024 * 1024, // 256 MiB
            fuel: None,
            epoch_tick_interval: None,
            allow_paths: Vec::new(),
            allow_network: false,
            wall_clock_timeout: Duration::from_secs(300),
            imports: vec!["(rnrs base)".to_string()],
            reuse_instance: true,
            binary_path: None,
        }
    }

    /// Plugin / extension use case. Untrusted-by-default but
    /// long-running. reuse_instance=true, fuel=Some(100M),
    /// 30s wall-clock, allow_paths empty, allow_network=false.
    pub fn plugin() -> Self {
        Self {
            memory_limit: 64 * 1024 * 1024, // 64 MiB
            fuel: Some(100_000_000),
            epoch_tick_interval: None,
            allow_paths: Vec::new(),
            allow_network: false,
            wall_clock_timeout: Duration::from_secs(30),
            imports: vec!["(rnrs base)".to_string()],
            reuse_instance: true,
            binary_path: None,
        }
    }

    /// Adversarial / per-eval-fresh use case (code playground,
    /// untrusted user submission). reuse_instance=false,
    /// fuel=Some(10M), 5s wall-clock, allow_paths empty,
    /// allow_network=false.
    pub fn adversarial() -> Self {
        Self {
            memory_limit: 16 * 1024 * 1024, // 16 MiB
            fuel: Some(10_000_000),
            epoch_tick_interval: None,
            allow_paths: Vec::new(),
            allow_network: false,
            wall_clock_timeout: Duration::from_secs(5),
            imports: vec!["(rnrs base)".to_string()],
            reuse_instance: false,
            binary_path: None,
        }
    }

    /// Validate field combinations that wasmtime won't reject
    /// until later in the build, surfacing them upfront with
    /// clearer attribution.
    ///
    /// Returns the first detected misconfiguration; in iter 1.5
    /// this gets called by `SandboxInstance::new` before any
    /// wasmtime work. Iter 1 makes it pub so tests can verify
    /// the preset configs validate.
    pub fn validate(&self) -> Result<(), SandboxError> {
        if self.memory_limit == 0 {
            return Err(SandboxError::Internal(
                "SandboxConfig.memory_limit must be > 0".into(),
            ));
        }
        if self.fuel.is_some() && self.epoch_tick_interval.is_some() {
            return Err(SandboxError::Internal(
                "SandboxConfig: fuel and epoch_tick_interval are mutually \
                 exclusive; pick one CPU bound"
                    .into(),
            ));
        }
        if self.wall_clock_timeout.is_zero() {
            return Err(SandboxError::Internal(
                "SandboxConfig.wall_clock_timeout must be > 0".into(),
            ));
        }
        if self.imports.is_empty() {
            return Err(SandboxError::Internal(
                "SandboxConfig.imports must list at least one import-spec; \
                 use [\"(rnrs base)\"] for the standard default"
                    .into(),
            ));
        }
        Ok(())
    }
}

// ---- SandboxError ----

/// Why a sandbox operation failed.
///
/// Failure modes map to the wire protocol's `<kind>` field — the
/// iter 1.5 protocol decoder produces these variants directly
/// from a guest response.
#[derive(Debug)]
pub enum SandboxError {
    /// The guest's eval raised a condition. The String carries
    /// the condition's printed representation (full Value
    /// round-trip lands in iter 1.5 when the protocol decoder
    /// runs).
    GuestRaised(String),
    /// Fuel exhausted before the eval completed.
    FuelExhausted,
    /// Wall-clock timeout exceeded.
    Timeout,
    /// Memory limit exceeded.
    MemoryExhausted,
    /// Guest tried to access a path/capability not granted.
    CapabilityDenied(String),
    /// Serialization mismatch — shouldn't happen with a
    /// versioned protocol but caught defensively.
    ProtocolError(String),
    /// Wasmtime/runtime internal failure (corrupted binary,
    /// linker mismatch, malformed config, etc.).
    Internal(String),
    /// Iter 1: the eval surface is stubbed pending iter 1.5
    /// crabscheme.wasm wiring. Calls to
    /// [`SandboxInstance::eval`] return this until iter 1.5.
    NotImplementedYet(&'static str),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::GuestRaised(s) => write!(f, "guest raised: {}", s),
            SandboxError::FuelExhausted => write!(f, "fuel exhausted"),
            SandboxError::Timeout => write!(f, "wall-clock timeout"),
            SandboxError::MemoryExhausted => write!(f, "memory exhausted"),
            SandboxError::CapabilityDenied(s) => write!(f, "capability denied: {}", s),
            SandboxError::ProtocolError(s) => write!(f, "protocol error: {}", s),
            SandboxError::Internal(s) => write!(f, "internal: {}", s),
            SandboxError::NotImplementedYet(s) => {
                write!(f, "not yet implemented in cs-sandbox-wasm iter 1: {}", s)
            }
        }
    }
}

impl std::error::Error for SandboxError {}

// ---- SandboxInstance ----

/// A live sandbox. Holds the wasmtime Engine + Module + (in iter
/// 1.5) a long-lived Store/Instance pair when reuse_instance is
/// true.
///
/// Iter 1: holds a constructed [`SandboxRuntime`] proving the
/// wasmtime side works. Iter 1.5 adds a crabscheme.wasm-derived
/// Instance + the stdin/stdout protocol plumbing.
pub struct SandboxInstance {
    config: SandboxConfig,
    runtime: SandboxRuntime,
}

impl SandboxInstance {
    /// Construct a new sandbox. Validates the config, builds the
    /// wasmtime Engine + Linker per the resource limits, and (in
    /// iter 1.5) loads the crabscheme.wasm binary.
    pub fn new(config: SandboxConfig) -> Result<Self, SandboxError> {
        config.validate()?;
        let runtime = SandboxRuntime::new(&config)?;
        Ok(Self { config, runtime })
    }

    /// Evaluate a Scheme expression inside the sandbox. Returns
    /// the printed result string from the guest's stdout.
    ///
    /// Iter 1.5 wired: instantiates the cached crabscheme.wasm
    /// module against a fresh WasiCtx whose argv is
    /// `["crabscheme", "--eval", expr_source]`; runs `_start`;
    /// captures stdout. Resource limits (fuel, memory) are
    /// enforced by wasmtime; guest trap kinds map to specific
    /// `SandboxError` variants.
    ///
    /// Requires `config.binary_path` to be set. Iter 1 callers
    /// without a binary still get the surface but receive a
    /// clear "no binary path" error here.
    pub fn eval(&mut self, expr_source: &str) -> Result<String, SandboxError> {
        self.runtime.eval_via_protocol(expr_source)
    }

    /// Return the active config (read-only view).
    pub fn config(&self) -> &SandboxConfig {
        &self.config
    }

    /// Reset the sandbox. For reuse_instance=true configs this
    /// rebuilds the inner Store/Instance; for false it's a
    /// no-op (every eval already spawns fresh).
    ///
    /// Iter 1 stub: re-validates the config; iter 1.5 rebuilds
    /// the wasmtime state.
    pub fn reset(&mut self) -> Result<(), SandboxError> {
        self.runtime = SandboxRuntime::new(&self.config)?;
        Ok(())
    }
}
