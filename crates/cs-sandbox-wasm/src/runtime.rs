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
        // Epoch interruption is wired ONLY for the mandatory
        // wall-clock timeout enforcement. The `epoch_tick_interval`
        // field on SandboxConfig is currently a stub — held for
        // forward compatibility but no separate CPU-bound ticker
        // is spawned. Always-on so the per-call wall-clock ticker
        // can bump it; every Store must call `set_epoch_deadline`
        // (the inline-WAT smoke pushes its deadline to u64::MAX).
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
        // current+1 and spawn a *cancellable* ticker that bumps the
        // engine's epoch after `wall_clock_timeout`. On expiry the
        // guest traps with `Trap::Interrupt`.
        //
        // The ticker blocks on a cancellation channel rather than a
        // bare `sleep` (issue #14). When the call finishes the eval
        // drops `cancel_tx`, so `recv_timeout` returns
        // `Disconnected` and the ticker exits at once *without*
        // bumping the epoch. That fixes two bugs in the old
        // detached-sleep ticker: (1) threads accumulating one per
        // eval for the whole timeout window, and (2) a stale ticker
        // bumping the shared engine's epoch into a later eval and
        // tripping a spurious `Trap::Interrupt` → `Timeout`.
        store.set_epoch_deadline(1);
        let timeout = self.wall_clock_timeout;
        let engine_for_ticker = self.engine.clone();
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        let ticker_handle = std::thread::spawn(move || {
            // `Timeout` => the wall-clock budget genuinely elapsed;
            // bump the epoch. `Disconnected` => the eval finished
            // and dropped the sender; exit, epoch untouched.
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = cancel_rx.recv_timeout(timeout)
            {
                engine_for_ticker.increment_epoch();
            }
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

        // Cancel + reap the ticker. Dropping `cancel_tx` wakes the
        // ticker's `recv_timeout` with `Disconnected` so it exits
        // at once (without bumping the epoch); `join` then returns
        // immediately and no detached thread is left behind.
        drop(cancel_tx);
        let _ = ticker_handle.join();

        if let Err(e) = call_result {
            // Discriminate by trap kind (API-stable) before
            // falling back to the I32Exit / generic-error paths.
            if let Some(trap) = e.downcast_ref::<Trap>() {
                match *trap {
                    Trap::OutOfFuel => return Err(SandboxError::FuelExhausted),
                    Trap::MemoryOutOfBounds => return Err(SandboxError::MemoryExhausted),
                    Trap::Interrupt => return Err(SandboxError::Timeout),
                    _ => { /* fall through to abort/Rust-OOM heuristic + generic Internal */ }
                }
            }
            // Rust-allocator OOM path: when wasmtime's StoreLimits
            // memory_limiter rejects a grow, the guest's Rust
            // allocator returns NULL, hits `handle_alloc_error`
            // and calls `abort()` — which compiles to wasm
            // `unreachable` and surfaces as `Trap::Unreachable
            // CodeReached`, NOT `Trap::MemoryOutOfBounds`. The
            // proximate cause is still the memory cap, so
            // classify it as `MemoryExhausted` to give callers
            // the actionable error.
            //
            // NOTE: this is intentionally backtrace-string-matching,
            // which violates the cluster-A principle that trap
            // classification should use API-stable Trap discriminants.
            // The exception is justified: wasmtime exposes no
            // distinct error kind for "memory limiter denied the
            // grow", so the only signal is the symbol names in the
            // guest's panic backtrace. Revisit if wasmtime ever
            // adds `Trap::MemoryLimiterDenied` or equivalent. The
            // false-positive risk is acceptable: these identifiers
            // appear in compiled-guest symbol names, not in arbitrary
            // user text.
            let msg = format!("{}", e);
            if msg.contains("rust_oom")
                || msg.contains("handle_alloc_error")
                || msg.contains("rust_alloc_error_handler")
            {
                return Err(SandboxError::MemoryExhausted);
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

#[cfg(test)]
mod tests {
    //! Unit tests for the L1-wrap support functions added in
    //! cluster A's MEDIUM-2 security fix. The end-to-end coverage
    //! from `tests/iter15_protocol.rs` etc. exercises these via
    //! actual guest evals; these unit tests assert the underlying
    //! string-building invariants directly so a regression that
    //! re-introduces the escape gap fails at the lowest layer
    //! rather than only via a guest exploit attempt.
    use super::*;

    // ---- escape_scheme_string ----

    #[test]
    fn escape_passes_normal_chars_through() {
        assert_eq!(escape_scheme_string("(+ 1 2 3)"), "(+ 1 2 3)");
        assert_eq!(escape_scheme_string(""), "");
        assert_eq!(escape_scheme_string("hello world"), "hello world");
    }

    #[test]
    fn escape_doubles_backslash() {
        // Backslash MUST be escaped first; otherwise the subsequent
        // quote-escape would itself be undone by an injected `\"`.
        assert_eq!(escape_scheme_string("\\"), "\\\\");
        assert_eq!(escape_scheme_string("a\\b"), "a\\\\b");
        assert_eq!(escape_scheme_string("\\\\"), "\\\\\\\\");
    }

    #[test]
    fn escape_doubles_quote() {
        assert_eq!(escape_scheme_string("\""), "\\\"");
        assert_eq!(escape_scheme_string("a\"b"), "a\\\"b");
    }

    #[test]
    fn escape_converts_control_chars() {
        assert_eq!(escape_scheme_string("\n"), "\\n");
        assert_eq!(escape_scheme_string("\r"), "\\r");
        assert_eq!(escape_scheme_string("\t"), "\\t");
        assert_eq!(escape_scheme_string("line1\nline2"), "line1\\nline2");
    }

    #[test]
    fn escape_blocks_wrapper_break_attempts() {
        // MEDIUM-2 exploit attempts from the original review: each
        // input contains characters that would close the wrapper's
        // string literal early IF the escape were missing. Verify
        // the output keeps the literal closed by doubling the
        // injection chars.
        //
        // Attempt 1: bare quote tries to terminate the string mid-
        // wrap. With escape: the quote becomes \" inside the
        // literal, so the literal stays open.
        let input1 = "foo\"; (display 'pwned)";
        let out1 = escape_scheme_string(input1);
        assert!(!out1.contains(r#"""#) || out1.contains(r#"\""#));
        // Check that every `"` in the output is preceded by `\`.
        let chars: Vec<char> = out1.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '"' {
                assert!(
                    i > 0 && chars[i - 1] == '\\',
                    "unescaped quote at {}: {:?}",
                    i,
                    out1
                );
            }
        }

        // Attempt 2: backslash followed by quote — naive escaper
        // that escapes only `"` first would let the `\` consume the
        // escape backslash. Order matters: `\` is escaped first.
        let input2 = "\\\"";
        assert_eq!(escape_scheme_string(input2), "\\\\\\\"");

        // Attempt 3: raw newline mid-expression. The R6RS reader
        // accepts raw control bytes in string literals as data, so
        // this isn't strictly a wrapper-break attempt, but we
        // defensively escape it so the guest's reader never sees a
        // surprise terminator from the host's serialization.
        let input3 = "foo\nbar";
        assert_eq!(escape_scheme_string(input3), "foo\\nbar");

        // Attempt 4: line continuation (backslash + newline).
        // Backslash-first ordering means the `\` doubles, then the
        // newline converts to `\n`. The resulting `\\\n` is data,
        // not a continuation.
        let input4 = "foo\\\nbar";
        assert_eq!(escape_scheme_string(input4), "foo\\\\\\nbar");
    }

    #[test]
    fn escape_is_idempotent_against_double_application() {
        // Defensive: running escape twice on the same input
        // shouldn't introduce wrap breaks. The output of one pass
        // is just text — a second pass treats it as ordinary
        // string data.
        let original = "embed \\ and \" and \n";
        let once = escape_scheme_string(original);
        let twice = escape_scheme_string(&once);
        // Twice-escaped output must still have all quotes preceded
        // by backslash and all backslashes doubled.
        let chars: Vec<char> = twice.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if *c == '"' {
                assert!(
                    i > 0 && chars[i - 1] == '\\',
                    "twice-escaped unescaped quote"
                );
            }
        }
    }

    // ---- build_l1_wrapper_argv ----

    #[test]
    fn wrapper_default_imports_uses_rnrs_base() {
        let out = build_l1_wrapper_argv("(+ 1 2)", &["(rnrs base)".to_string()]);
        assert_eq!(
            out,
            r#"(eval (with-input-from-string "(+ 1 2)" read) (environment '(rnrs base)))"#
        );
    }

    #[test]
    fn wrapper_multiple_imports_each_quoted() {
        let out = build_l1_wrapper_argv(
            "x",
            &["(rnrs base)".to_string(), "(rnrs lists)".to_string()],
        );
        assert_eq!(
            out,
            r#"(eval (with-input-from-string "x" read) (environment '(rnrs base) '(rnrs lists)))"#
        );
    }

    /// Verify the wrap is structurally well-formed for the given
    /// user input + imports: outer structure is the host-owned
    /// prefix + suffix, and every `"` inside the literal region is
    /// preceded by an odd number of `\` (i.e., escape-canceled).
    /// Panics with a debug-friendly message on violation. Used by
    /// the exploit-attempt tests below.
    fn assert_wrap_well_formed(out: &str, expected_imports: &[&str]) {
        const PREFIX: &str = r#"(eval (with-input-from-string ""#;
        let suffix_imports: String = expected_imports
            .iter()
            .map(|s| format!(" '{}", s))
            .collect();
        let suffix = format!("\" read) (environment{}))", suffix_imports);
        assert!(
            out.starts_with(PREFIX),
            "wrap prefix broken: {:?}",
            &out[..PREFIX.len().min(out.len())]
        );
        assert!(out.ends_with(&suffix), "wrap suffix broken: {:?}", out);
        // The literal region is between the opening `"` (last char
        // of PREFIX) and the closing `"` (first char of suffix).
        let literal_start = PREFIX.len();
        let literal_end = out.len() - suffix.len() + 1; // include the closing `"`? no
        let literal = &out[literal_start..out.len() - suffix.len()];
        // Walk the literal. Every `"` must be preceded by an odd
        // number of `\`. Track the run-length of `\` as we go.
        let chars: Vec<char> = literal.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '"' {
                // Count consecutive `\` preceding this position.
                let mut backslashes = 0usize;
                let mut j = i;
                while j > 0 && chars[j - 1] == '\\' {
                    backslashes += 1;
                    j -= 1;
                }
                assert!(
                    backslashes % 2 == 1,
                    "unescaped `\"` at offset {} in literal {:?} (full wrap: {:?})",
                    i,
                    literal,
                    out
                );
            }
            i += 1;
        }
        let _ = literal_end; // silence unused, kept for documentation
    }

    #[test]
    fn wrapper_escapes_user_expr_inside_string_literal() {
        // The whole point of cluster A's MEDIUM-2 fix: user
        // expression is embedded as a string literal that the
        // guest's `read` parses at runtime. Crafted user input
        // can't break out of the literal.
        let out = build_l1_wrapper_argv("1) (display 'pwned", &["(rnrs base)".to_string()]);
        assert_wrap_well_formed(&out, &["(rnrs base)"]);
        // Sanity: the wrap ends with `))` (close `eval` + close
        // `environment`).
        assert!(out.ends_with("))"));
    }

    #[test]
    fn wrapper_rejects_wrap_escape_via_backslash_quote() {
        // Sneakier: backslash followed by quote. If the escaper
        // ordered quote-first then backslash-first, the resulting
        // wrap would have an unescaped `\"` sequence terminating
        // the literal early. Verify the wrap's literal region
        // keeps every `"` escape-canceled (odd backslash prefix).
        let out = build_l1_wrapper_argv("\\\"", &["(rnrs base)".to_string()]);
        assert_wrap_well_formed(&out, &["(rnrs base)"]);
    }

    #[test]
    fn wrapper_rejects_naked_quote_followed_by_payload() {
        // Direct paste of the MEDIUM-2 original repro from the
        // security-auditor agent: a naked `"` mid-expr would close
        // the literal early in any wrap that pasted the user expr
        // directly. With escape, it can't.
        let out = build_l1_wrapper_argv(r#"foo"; (display 'pwned"#, &["(rnrs base)".to_string()]);
        assert_wrap_well_formed(&out, &["(rnrs base)"]);
    }

    #[test]
    fn wrapper_rejects_control_char_smuggling() {
        // Raw newlines in the user expr don't terminate a Scheme
        // string literal (the reader accepts them as data), so
        // this isn't a wrap-break, but the escape converts to `\n`
        // anyway for defensive consistency. Verify the wrap region
        // still has only escaped `"` chars.
        let out = build_l1_wrapper_argv("foo\nbar\n", &["(rnrs base)".to_string()]);
        assert_wrap_well_formed(&out, &["(rnrs base)"]);
    }

    #[test]
    fn wrapper_empty_user_expr_still_well_formed() {
        // eval_via_protocol rejects empty input upfront via
        // ProtocolError, but build_l1_wrapper_argv itself must
        // still produce a well-formed wrap (defensive — the
        // function is callable from other paths in the future).
        let out = build_l1_wrapper_argv("", &["(rnrs base)".to_string()]);
        assert_eq!(
            out,
            r#"(eval (with-input-from-string "" read) (environment '(rnrs base)))"#
        );
    }
}
