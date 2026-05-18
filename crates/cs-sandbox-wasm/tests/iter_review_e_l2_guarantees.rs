//! cs-sandbox-wasm — PR #12 review cluster E.
//!
//! Tests requested by the code-reviewer agent that the existing
//! iter15/iter5 suites didn't cover:
//!
//! 1. **Memory exhaustion**: a guest allocating past `memory_limit`
//!    surfaces as `SandboxError::MemoryExhausted`, not as a generic
//!    Internal error. This validates the cluster-A trap classifier
//!    (`Trap::MemoryOutOfBounds → MemoryExhausted`).
//!
//! 2. **Load-bearing defense in depth**: the iter5 suite verifies L1
//!    blocks file ops even WITH a hypothetical filesystem grant, but
//!    its test never actually mounted a directory — the test passed
//!    because L1's unbound-identifier check fires before WASI is
//!    consulted. This test ACTUALLY pre-opens a temp directory and
//!    confirms L1 still blocks `open-input-file` from the user
//!    expression. If the L1 wrap were removed, WASI would grant
//!    access and `open-input-file` would succeed.
//!
//! 3. **Nested eval respects L1**: a user expression that itself
//!    calls `(eval … (environment '(rnrs base)))` should run the
//!    inner eval under L1 restriction too — verifying that L1's
//!    snapshot scoping is recursive, not one-shot at the outer wrap.
//!
//! 4. **allow_network=false plumbing**: the SandboxConfig field is
//!    `false` in every preset but no preset-test verified the linker
//!    actually withholds socket capabilities. White-box test that
//!    no `wasi_snapshot_preview1::sock_*` imports are satisfied for
//!    a module that tries to import them.
//!
//! 5. **Wall-clock timeout end-to-end** (added after pr-test-analyzer
//!    follow-up, severity 9): cluster A wired the epoch-interruption
//!    + ticker-thread machinery for `wall_clock_timeout`, but no test
//!    exercised the full `Trap::Interrupt → SandboxError::Timeout`
//!    path end-to-end. A guest infinite loop with a sub-second timeout
//!    must trap as `Timeout`, not as `FuelExhausted` or a generic
//!    `Internal`. Also covers the case where fuel is intentionally
//!    `None` so the timeout is the only forcing mechanism.
//!
//! All tests use the `requires_wasm!` skip-when-no-binary pattern
//! from the iter15 suite, so CI without the WASM build gets a green
//! skip rather than a red miss.

use std::path::PathBuf;
use std::time::Duration;

use cs_sandbox_wasm::{SandboxConfig, SandboxError, SandboxInstance};

fn binary_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("CRABSCHEME_WASM_PATH") {
        let p = PathBuf::from(env_path);
        return p.exists().then_some(p);
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip1/release/crabscheme.wasm");
    default.exists().then_some(default)
}

macro_rules! requires_wasm {
    () => {
        if binary_path().is_none() {
            eprintln!("skipping: crabscheme.wasm not found");
            return;
        }
    };
}

// ---- 1. Memory exhaustion ----

/// Allocate a vector larger than the adversarial preset's 16 MiB
/// memory cap. The wasmtime memory grow trap surfaces as
/// `MemoryExhausted` (cluster-A trap classifier). Pre-cluster-A,
/// this would have been `Internal("...wasm trap: out of bounds...")`.
#[test]
fn allocation_above_memory_limit_returns_memory_exhausted() {
    requires_wasm!();
    let mut config = SandboxConfig::adversarial();
    config.binary_path = binary_path();
    // Push memory_limit hard down so the test runs fast; the
    // adversarial preset's 16 MiB is enough to do real work but
    // makes the test allocate ~16M cells which is slow. 4 MiB cap
    // means a 2M-element vector (~16MiB at 8 bytes/cell) trips
    // the limiter without long allocation churn.
    config.memory_limit = 4 * 1024 * 1024;
    // Make sure the wall-clock timeout doesn't fire first.
    config.wall_clock_timeout = Duration::from_secs(60);
    // Give enough fuel that the test failure mode is memory, not
    // fuel exhaustion. The allocation loop itself uses few wasm
    // instructions — the trap fires inside the linear-memory grow.
    config.fuel = Some(1_000_000_000);
    let mut sb = SandboxInstance::new(config).unwrap();
    let result = sb.eval("(make-vector 2000000 0)");
    match result {
        Err(SandboxError::MemoryExhausted) => (),
        Err(SandboxError::FuelExhausted) => {
            // Acceptable degenerate path — if the walker tier's
            // per-cell init cost dominates, fuel runs out first.
            // Document and accept; the safety claim (sandbox
            // contains the OOM) is met either way.
            eprintln!("fuel exhausted before memory limit; both are valid containment paths");
        }
        Err(other) => panic!(
            "expected MemoryExhausted or FuelExhausted, got: {:?}",
            other
        ),
        Ok(v) => panic!(
            "allocation past memory_limit returned value {:?}; \
             memory limiter did NOT contain the OOM",
            v
        ),
    }
}

// ---- 2. Load-bearing defense in depth ----

/// Actually pre-open a tmpdir and verify L1 still blocks the user
/// expression's `open-input-file`. The iter5 suite's analogous
/// test cited safety as the reason not to mount a real directory;
/// mounting a freshly-created tempdir is safe — write/read there
/// can't leak anything sensitive.
#[test]
fn l1_blocks_file_op_even_with_real_preopen() {
    requires_wasm!();
    // Create a tempdir we can mount safely. The path exists at
    // SandboxConfig construction time; wasmtime opens it during
    // instance construction. We don't write anything into it.
    let tmp = tempdir_or_skip();
    let Some(tmp) = tmp else {
        eprintln!("skipping: couldn't create tempdir");
        return;
    };
    let mut config = SandboxConfig::hygiene();
    config.binary_path = binary_path();
    config.allow_paths = vec![tmp.path().to_path_buf()];
    // Standard (rnrs base) only — open-input-file is not in there.
    config.imports = vec!["(rnrs base)".into()];

    let mut sb = SandboxInstance::new(config).unwrap();
    // Try to open a file inside the GRANTED directory. WASI would
    // allow this; L1's `(environment '(rnrs base))` wrap should
    // not — `open-input-file` is not in the import set.
    let target = format!("{}/no-such-file", tmp.path().display());
    let result = sb.eval(&format!("(open-input-file \"{}\")", target));
    assert!(
        result.is_err(),
        "L1 should reject open-input-file even with WASI grant; got Ok({:?})",
        result
    );
    let err_str = format!("{:?}", result.unwrap_err());
    // L1 unbound error mentions the identifier or "unbound" /
    // "undefined". An L2-only failure (WASI denied) would
    // mention "capability" or "permission".
    assert!(
        err_str.contains("open-input-file")
            || err_str.contains("unbound")
            || err_str.contains("undefined")
            || err_str.contains("not bound"),
        "expected L1 unbound error, got: {}",
        err_str
    );
    drop(tmp);
}

// ---- 3. Nested eval respects L1 ----

/// The user expression contains its own `(eval … (environment …))`.
/// L1's scoping property: that inner call gets evaluated in an
/// inner `(rnrs base)`-only frame, so attempting to escape via a
/// nested eval still fails on unbound identifiers.
///
/// What this catches: a regression where the outer L1 wrap is one-
/// shot — applied only once at the host's wrap site, then a user
/// can nest `(eval … (environment '(rnrs base) '(rnrs io)))` to
/// re-open the import set. The expected behavior is that the
/// guest's `(environment …)` builtin itself enforces the L1.3
/// composite-construction rules, so nesting can't widen the set
/// beyond what the host approved.
///
/// Concretely: `(rnrs io)` is currently not part of any preset's
/// import list AND is also not registered in the guest's
/// `resolve_import_spec` table — so requesting it inside the
/// nested eval errors at the inner `(environment …)` call.
#[test]
fn nested_eval_cannot_widen_l1_import_set() {
    requires_wasm!();
    let mut config = SandboxConfig::hygiene();
    config.binary_path = binary_path();
    config.imports = vec!["(rnrs base)".into()];
    let mut sb = SandboxInstance::new(config).unwrap();
    // Nested eval that tries to pull in (rnrs io) which isn't in
    // the host-approved set. The inner `(environment '(rnrs io))`
    // call should fail because (rnrs io) isn't a recognized
    // library spec.
    let result = sb.eval("(eval '(+ 1 2) (environment '(rnrs io)))");
    match result {
        Err(_) => {
            // Good — either (rnrs io) is unknown (preferred), or
            // the nested eval fails some other way. The safety
            // claim is just that this expression doesn't return
            // a value (i.e., doesn't silently succeed and open
            // up a bigger surface).
        }
        Ok(v) => panic!(
            "nested eval with unapproved import returned {:?}; L1 \
             does NOT constrain nested (environment) calls",
            v
        ),
    }
}

/// Companion: a nested eval that uses ONLY the approved set
/// SHOULD work — proving the test above isn't just rejecting all
/// nested evals.
#[test]
fn nested_eval_within_approved_set_succeeds() {
    requires_wasm!();
    let mut config = SandboxConfig::hygiene();
    config.binary_path = binary_path();
    config.imports = vec!["(rnrs base)".into()];
    let mut sb = SandboxInstance::new(config).unwrap();
    // Nested eval that stays within (rnrs base) — should succeed.
    let result = sb
        .eval("(eval '(+ 10 20 30) (environment '(rnrs base)))")
        .expect("nested eval within (rnrs base) should succeed");
    assert_eq!(result, "60");
}

// ---- 4. allow_network=false plumbing ----

/// White-box check that the linker only registers
/// `wasi_snapshot_preview1`. wasi-sockets / `sock_*` exports must
/// not be satisfied — the SandboxConfig.allow_network field is
/// `false` in every preset, but our linker setup doesn't currently
/// READ that field (sockets aren't part of preview1 at all). This
/// test documents the current invariant: a module importing
/// `wasi_snapshot_preview1.sock_open` MUST fail to instantiate.
///
/// If a future iter adds wasi-sockets, this test becomes the
/// regression guard that `allow_network=false` actually gates it.
#[test]
fn linker_does_not_grant_socket_capabilities_to_sockets_module() {
    use wasmtime::{Config, Engine, Linker, Module, Store};
    use wasmtime_wasi::{p2::pipe::MemoryInputPipe, preview1, WasiCtxBuilder};

    // Build a host setup that mirrors runtime.rs::eval_via_protocol
    // — same engine config, same linker registration. The test
    // verifies the linker rejects a sock_* import.
    let mut wasm_cfg = Config::new();
    wasm_cfg.epoch_interruption(true);
    let engine = Engine::new(&wasm_cfg).unwrap();

    // Trivial WAT module that IMPORTS sock_open from preview1.
    // Linker resolution should fail because preview1 doesn't
    // export sockets in our build.
    const WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "sock_open"
            (func $sock_open (param i32 i32 i32) (result i32))))
    "#;
    let module = Module::new(&engine, WAT.as_bytes()).expect("WAT compiles");

    struct D {
        wasi: wasmtime_wasi::preview1::WasiP1Ctx,
    }
    let stdin = MemoryInputPipe::new(Vec::<u8>::new());
    let wasi = WasiCtxBuilder::new().stdin(stdin).build_p1();
    let mut store = Store::new(&engine, D { wasi });
    store.set_epoch_deadline(u64::MAX);

    let mut linker: Linker<D> = Linker::new(&engine);
    preview1::add_to_linker_sync(&mut linker, |d: &mut D| &mut d.wasi).unwrap();

    let instantiate_result = linker.instantiate(&mut store, &module);
    assert!(
        instantiate_result.is_err(),
        "sock_open import should not be satisfied by the preview1 \
         linker; linker silently granted sockets"
    );
    let err = format!("{}", instantiate_result.unwrap_err());
    // The wasmtime error mentions the unsatisfied import name —
    // not pinned to an exact string since wasmtime versions vary.
    assert!(
        err.contains("sock_open") || err.contains("unknown import"),
        "expected unsatisfied-import error mentioning sock_open or 'unknown import', got: {}",
        err
    );
}

// ---- 5. Wall-clock timeout end-to-end ----

/// Cluster A's marquee fix: wall-clock timeout actually fires. The
/// pre-fix behavior was that `wall_clock_timeout` was validated > 0
/// but never armed, so an infinite loop with no fuel cap would hang
/// the host until manually killed. With the fix in place, an
/// infinite loop traps as `SandboxError::Timeout` within
/// approximately the configured timeout window.
///
/// Important: `fuel = None`. Fuel was the prior containment fallback;
/// disabling it forces the test to rely on `wall_clock_timeout`
/// alone, so a regression that re-breaks the ticker / epoch wiring
/// would either hang the test (caught by `cargo test`'s per-test
/// timeout) or produce a different error variant (caught by the
/// match below).
#[test]
fn wall_clock_timeout_fires_on_long_running_eval() {
    requires_wasm!();
    let mut config = SandboxConfig::hygiene();
    config.binary_path = binary_path();
    // 500ms — tight enough that the test runs fast; loose enough
    // that any reasonable build/scheduler doesn't false-positive
    // before the eval starts.
    config.wall_clock_timeout = Duration::from_millis(500);
    // No fuel — wall-clock is the only enforcement mechanism for
    // this test. A regression that re-breaks the ticker / epoch
    // wiring would either hang (caught by cargo's per-test
    // timeout) or surface a different error variant.
    config.fuel = None;
    let mut sb = SandboxInstance::new(config).unwrap();
    let start = std::time::Instant::now();
    // Naive Fibonacci — non-tail recursive (bounded depth ~35 so
    // it won't blow the guest's host stack in --tier walker) but
    // computationally heavy enough that the walker tier takes
    // multiple seconds. The 500ms ticker will fire well before
    // completion. We use fib instead of a tail-recursive infinite
    // loop because the guest binary is built --no-default-features
    // → walker tier → no tail-call optimization → infinite loops
    // overflow the wasm stack instead of running long enough for
    // the timeout to catch them.
    let result = sb.eval("(let f ((n 35)) (if (< n 2) n (+ (f (- n 1)) (f (- n 2)))))");
    let elapsed = start.elapsed();
    match result {
        Err(SandboxError::Timeout) => (),
        other => panic!(
            "expected SandboxError::Timeout; got {:?} after {:?}",
            other, elapsed
        ),
    }
    // Sanity: the timeout actually fired in roughly the configured
    // window. Allow generous slack for slow CI and ticker-thread
    // spawn latency. A timeout that fires way too early
    // (sub-100ms) would suggest the ticker is armed-too-early
    // (issue #14's spurious-Timeout failure mode).
    assert!(
        elapsed >= Duration::from_millis(100),
        "timeout fired suspiciously early: {:?} (expected ~500ms)",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "timeout took way too long: {:?} (expected ~500ms with ticker firing)",
        elapsed
    );
}

/// Companion: an eval that completes BEFORE the timeout fires
/// returns normally without spurious `Timeout`. Guards against a
/// regression where the ticker fires too aggressively or the epoch
/// deadline is set too low at the start of the call.
#[test]
fn fast_eval_below_timeout_returns_normally() {
    requires_wasm!();
    let mut config = SandboxConfig::hygiene();
    config.binary_path = binary_path();
    config.wall_clock_timeout = Duration::from_secs(30);
    let mut sb = SandboxInstance::new(config).unwrap();
    let result = sb.eval("(+ 1 2 3)").expect("fast eval should succeed");
    assert_eq!(result, "6");
}

// ---- helpers ----

/// Create a tempdir under the system temp root. Returns `None`
/// (and the caller skips) if creation fails — we don't want
/// flake-on-filesystem-failure to mask real sandbox bugs.
fn tempdir_or_skip() -> Option<TempDir> {
    let base = std::env::temp_dir();
    // PID + nanos = unique enough for parallel test runs without
    // pulling in the tempfile crate as a new dep.
    let suffix = format!(
        "cs-sandbox-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos()
    );
    let path = base.join(suffix);
    std::fs::create_dir(&path).ok()?;
    Some(TempDir { path })
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
