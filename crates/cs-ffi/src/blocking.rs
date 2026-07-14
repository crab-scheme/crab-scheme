//! Generic cooperative-blocking-op infrastructure (cs-845.3).
//!
//! Several stdlib modules (`cs-stdlib-fs`, `cs-stdlib-process`, …) run
//! genuinely blocking `std` operations — file I/O, subprocess wait — that,
//! on a green actor's shared `LocalSet` worker, would freeze every
//! co-tenant actor for the duration. `cs-runtime`'s coroutine driver
//! (`crates/cs-runtime/src/builtins/beam.rs`) can run these on a
//! `tokio::task::spawn_blocking` thread instead and resume the suspended
//! coroutine with the result — but this crate is beneath the actor layer
//! (same inverted-dependency shape as `cs_stdlib_time::install_cooperative_sleep`
//! and `cs_stdlib_net::install_async_recv`/`install_async_send`), so it
//! can't call into `cs-runtime` directly.
//!
//! This module gives every such stdlib crate one shared, type-erased hook
//! shape instead of each reinventing the boxing/downcasting. Each crate
//! still owns its own `OnceLock<BlockingHook>` + `install_cooperative_blocking`
//! function (statics don't cross crates), but the erasure + downcast logic
//! lives here once.

use std::any::Any;

/// A blocking closure ready to hand to the coroutine driver: runs on a
/// `spawn_blocking` thread, so must be `Send`. Its result is type-erased
/// (`Box<dyn Any + Send>`) because the hook is stored as a plain `fn`
/// pointer — it can't be generic — so it can't know the concrete `T`.
pub type BlockingOp = Box<dyn FnOnce() -> Result<Box<dyn Any + Send>, String> + Send>;

/// Installed by `cs-runtime` (when the actor layer is present) to let a
/// coroutine driver service a blocking op by suspending the caller's green
/// actor and running the op on a `spawn_blocking` thread instead of
/// blocking the shared worker. Always returns `Some`: with no active
/// coroutine driver on this thread, the hook itself just runs `op()`
/// inline (identical to the caller doing it directly).
pub type BlockingHook = fn(BlockingOp) -> Option<Result<Box<dyn Any + Send>, String>>;

/// Run `op`, cooperatively parking the calling green actor if `hook` is
/// installed and a coroutine driver is active on this thread; otherwise
/// (no hook installed, e.g. the `actor` feature is off, or no driver
/// active) `op` runs as a plain blocking call.
///
/// # Panics
///
/// Panics if `hook` resumes with a value of the wrong concrete type — a
/// driver bug, not a reachable runtime condition (each call site's `T`
/// matches the closure it boxed).
pub fn run_blocking<T: Send + 'static>(
    hook: Option<BlockingHook>,
    op: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    match hook {
        Some(h) => {
            let boxed: BlockingOp =
                Box::new(move || op().map(|v| Box::new(v) as Box<dyn Any + Send>));
            match h(boxed) {
                Some(Ok(v)) => Ok(*v
                    .downcast::<T>()
                    .unwrap_or_else(|_| panic!("run_blocking: hook resumed with the wrong type"))),
                Some(Err(e)) => Err(e),
                None => Err("run_blocking: internal error (hook returned no result)".into()),
            }
        }
        None => op(),
    }
}
