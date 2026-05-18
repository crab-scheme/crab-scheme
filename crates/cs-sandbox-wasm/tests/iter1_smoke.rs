//! cs-sandbox-wasm iter 1 — surface + wasmtime smoke tests.
//!
//! No crabscheme.wasm binary involved; iter 1's job is to prove
//! the Rust-side integration shape works. Iter 1.5 adds the
//! protocol-decoder + binary integration tests.

use std::time::Duration;

use cs_sandbox_wasm::{
    verify_wasmtime_integration, SandboxConfig, SandboxError, SandboxInstance, SandboxRuntime,
};

// ---- Preset defaults ----

#[test]
fn hygiene_preset_validates() {
    let c = SandboxConfig::hygiene();
    c.validate().unwrap();
    assert!(c.reuse_instance);
    assert_eq!(c.fuel, None);
    assert_eq!(c.wall_clock_timeout, Duration::from_secs(300));
    assert!(!c.allow_network);
    assert!(c.allow_paths.is_empty());
}

#[test]
fn plugin_preset_validates() {
    let c = SandboxConfig::plugin();
    c.validate().unwrap();
    assert!(c.reuse_instance);
    assert_eq!(c.fuel, Some(100_000_000));
    assert_eq!(c.wall_clock_timeout, Duration::from_secs(30));
}

#[test]
fn adversarial_preset_validates() {
    let c = SandboxConfig::adversarial();
    c.validate().unwrap();
    assert!(!c.reuse_instance);
    assert_eq!(c.fuel, Some(10_000_000));
    assert_eq!(c.wall_clock_timeout, Duration::from_secs(5));
}

// ---- Validation negative cases ----

#[test]
fn zero_memory_limit_fails_validation() {
    let mut c = SandboxConfig::hygiene();
    c.memory_limit = 0;
    let err = c.validate().unwrap_err();
    assert!(matches!(err, SandboxError::Internal(_)));
    assert!(format!("{}", err).contains("memory_limit"));
}

#[test]
fn fuel_and_epoch_together_fails_validation() {
    let mut c = SandboxConfig::hygiene();
    c.fuel = Some(1000);
    c.epoch_tick_interval = Some(Duration::from_millis(10));
    let err = c.validate().unwrap_err();
    assert!(format!("{}", err).contains("mutually exclusive"));
}

#[test]
fn zero_wall_clock_fails_validation() {
    let mut c = SandboxConfig::hygiene();
    c.wall_clock_timeout = Duration::ZERO;
    assert!(c.validate().is_err());
}

#[test]
fn empty_imports_fails_validation() {
    let mut c = SandboxConfig::hygiene();
    c.imports.clear();
    let err = c.validate().unwrap_err();
    assert!(format!("{}", err).contains("imports"));
}

// ---- SandboxInstance construction ----

#[test]
fn sandbox_instance_constructs_with_each_preset() {
    for c in [
        SandboxConfig::hygiene(),
        SandboxConfig::plugin(),
        SandboxConfig::adversarial(),
    ] {
        SandboxInstance::new(c).expect("each preset constructs");
    }
}

#[test]
fn sandbox_instance_construction_propagates_validation_error() {
    let mut c = SandboxConfig::hygiene();
    c.memory_limit = 0;
    assert!(SandboxInstance::new(c).is_err());
}

#[test]
fn sandbox_instance_exposes_config_readonly() {
    let mut sb = SandboxInstance::new(SandboxConfig::plugin()).unwrap();
    assert_eq!(sb.config().fuel, Some(100_000_000));
    // Reset is currently a no-op-ish rebuild; verify it doesn't
    // crash and config survives.
    sb.reset().unwrap();
    assert_eq!(sb.config().fuel, Some(100_000_000));
}

// ---- Eval surface ----
//
// Iter 1 stubbed eval; iter 1.5 wired the real protocol. Iter 1
// configs leave binary_path=None, so eval now reports the
// missing-path Internal error rather than NotImplementedYet.

#[test]
fn eval_without_binary_returns_internal_path_error() {
    let mut sb = SandboxInstance::new(SandboxConfig::adversarial()).unwrap();
    let err = sb.eval("(+ 1 2)").unwrap_err();
    match err {
        SandboxError::Internal(msg) => {
            assert!(
                msg.contains("binary_path") || msg.contains("not set"),
                "expected missing-binary-path message, got: {}",
                msg
            );
        }
        other => panic!("expected Internal(missing-binary-path), got {:?}", other),
    }
}

// ---- Wasmtime integration smoke (no WASI, no crabscheme.wasm) ----

#[test]
fn wasmtime_integration_compiles_and_runs_inline_wat() {
    let runtime = SandboxRuntime::new(&SandboxConfig::hygiene()).unwrap();
    let sum = verify_wasmtime_integration(&runtime, 7, 35).unwrap();
    assert_eq!(sum, 42);
}

#[test]
fn wasmtime_integration_honors_fuel_path() {
    // The trivial `add` consumes very little fuel; this test
    // just proves the fuel-API code path doesn't panic when
    // configured. Real fuel-exhaustion testing lands in iter
    // 1.5 with a body that actually loops.
    let runtime = SandboxRuntime::new(&SandboxConfig::plugin()).unwrap();
    let sum = verify_wasmtime_integration(&runtime, 100, 200).unwrap();
    assert_eq!(sum, 300);
}

#[test]
fn wasmtime_integration_runs_with_adversarial_preset_fuel() {
    let runtime = SandboxRuntime::new(&SandboxConfig::adversarial()).unwrap();
    // 10M fuel is plenty for an add.
    let sum = verify_wasmtime_integration(&runtime, -5, 9).unwrap();
    assert_eq!(sum, 4);
}

// ---- Error display ----

#[test]
fn sandbox_error_display_includes_variant_info() {
    assert!(format!("{}", SandboxError::FuelExhausted).contains("fuel"));
    assert!(format!("{}", SandboxError::Timeout).contains("timeout"));
    assert!(format!("{}", SandboxError::MemoryExhausted).contains("memory"));
    assert!(
        format!("{}", SandboxError::CapabilityDenied("/etc/passwd".into())).contains("/etc/passwd")
    );
    assert!(format!("{}", SandboxError::ProtocolError("framing".into())).contains("framing"));
    assert!(format!("{}", SandboxError::Internal("oops".into())).contains("oops"));
    assert!(format!("{}", SandboxError::GuestRaised("cond".into())).contains("cond"));
    assert!(format!("{}", SandboxError::NotImplementedYet("foo")).contains("not yet implemented"));
}
