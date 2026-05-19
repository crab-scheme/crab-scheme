//! Loadable Rust module plugins.
//!
//! A cdylib plugin exports a single `cs_web_register` symbol that
//! fills a [`RouteSink`] with the routes the module wants to
//! contribute. The host calls [`Module::load`] (which `dlopen`s
//! the file), then [`Module::register_into`] (which invokes the
//! entry point against a freshly-built sink).
//!
//! Example cdylib:
//!
//! ```ignore
//! // Cargo.toml: crate-type = ["cdylib"]
//! use cs_web::{handler::service_fn, ok, Method, Request, RouteSink};
//!
//! #[no_mangle]
//! pub fn cs_web_register(sink: &mut RouteSink) {
//!     sink.get("/hello", service_fn(|_: Request| async {
//!         ok("hi from the plugin")
//!     }));
//! }
//! ```
//!
//! ## ABI stability constraint
//!
//! The plugin and host MUST be built with the same Rust
//! toolchain + the same `cs-web` version (ideally as siblings in
//! the same workspace). The entry-point signature uses Rust
//! references (`&mut RouteSink`), so layout drift would silently
//! corrupt the sink. There is no version handshake yet — that's
//! a follow-up that needs an `extern "C"` shim entry point with a
//! version u32 + size_of::<RouteSink>() check.
//!
//! ## Lifetime
//!
//! The loaded library is held alive for as long as the [`Module`]
//! handle exists. Dropping the handle `dlclose`s the library —
//! any Service still pointing into it (the routes drained into a
//! Router) will dangle. Hold the [`Module`] for at least as long
//! as the routes it produced live.

use std::path::Path;

use libloading::{Library, Symbol};

use crate::{router::RouteSink, WebError};

/// Symbol name the cdylib must export.
pub const ENTRY_POINT: &[u8] = b"cs_web_register";

/// A loaded cdylib plugin. Owns the [`Library`] handle so the
/// `.dylib` stays mapped for the lifetime of any routes it
/// produced.
pub struct Module {
    #[allow(dead_code)] // held for liveness — the symbol calls into this lib
    library: Library,
    register: unsafe extern "Rust" fn(&mut RouteSink),
}

impl Module {
    /// Open `path` and look up the registration entry point.
    /// Returns [`WebError::Module`] if the file can't be opened or
    /// the symbol is missing.
    ///
    /// Safety: the caller asserts that `path` was built by a
    /// trusted toolchain — see the ABI stability note in the
    /// module docs. Wrapped in `unsafe` because libloading's
    /// `Library::new` is unsafe (running constructor code in the
    /// loaded library) and the symbol lookup needs an unsafe
    /// transmute to a typed fn pointer.
    pub unsafe fn load(path: impl AsRef<Path>) -> Result<Self, WebError> {
        let path = path.as_ref();
        let library = Library::new(path)
            .map_err(|e| WebError::Module(format!("open {}: {e}", path.display())))?;
        let symbol: Symbol<unsafe extern "Rust" fn(&mut RouteSink)> = library
            .get(ENTRY_POINT)
            .map_err(|e| WebError::Module(format!("lookup cs_web_register: {e}")))?;
        // Detach the symbol's lifetime: the Library itself outlives
        // it because we keep it in `Module`.
        let register = *symbol.into_raw();
        Ok(Self { library, register })
    }

    /// Call the plugin's entry point against `sink`, accumulating
    /// the routes it contributes.
    pub fn register_into(&self, sink: &mut RouteSink) {
        // Safety: we vouched for the toolchain match at load.
        unsafe { (self.register)(sink) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{handler::service_fn, ok, Method, Request};
    use bytes::Bytes;

    // We can't easily build a real cdylib from a unit test, so we
    // exercise the post-load shape: prove that a function with
    // the entry-point signature can fill a sink, and that the
    // sink composes into a Router cleanly. The dlopen path is
    // exercised in the e2e tests using a fixture cdylib.
    extern "Rust" fn fake_module(sink: &mut RouteSink) {
        sink.get("/plugin", service_fn(|_| async { ok("from plugin") }));
        sink.add(
            Method::POST,
            "/plugin/echo",
            service_fn(|req: Request| async move {
                ok(format!("echo:{}", String::from_utf8_lossy(req.body())))
            }),
        );
    }

    #[test]
    fn entry_point_signature_compiles() {
        // The `register` field is `unsafe extern "Rust" fn` — if
        // this type-checks, a real cdylib exporting a function
        // with the same shape will round-trip through `Module`.
        let f: unsafe extern "Rust" fn(&mut RouteSink) = fake_module;
        let _ = f; // silence dead_assigns
    }

    #[tokio::test]
    async fn sink_filled_by_module_shape_dispatches() {
        let mut sink = RouteSink::new();
        fake_module(&mut sink);
        assert_eq!(sink.len(), 2);

        let svc = crate::Router::new().add_sink(sink).into_service();
        let req = http::Request::builder()
            .method(Method::GET)
            .uri("/plugin")
            .body(Bytes::new())
            .unwrap();
        let resp = svc.call(req).await;
        assert_eq!(resp.status(), http::StatusCode::OK);
        assert_eq!(resp.body(), &Bytes::from_static(b"from plugin"));
    }
}
