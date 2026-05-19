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
//! A plugin's code and drop-glue live in the loaded library's text
//! segment, and the routes it produces (`ArcService`s drained into
//! a Router, frequently held by detached server tasks) reference
//! that code indefinitely. `dlclose`ing the library while any such
//! route is still alive unmaps that code — undefined behavior, and
//! a hard crash on Linux where `dlclose` actually unmaps. macOS
//! keeps dylibs resident even after `dlclose`, which makes the bug
//! easy to miss there.
//!
//! Pinning the library to the lifetime of every route it produced
//! is impractical, so [`Module::load`] keeps the library mapped
//! for the process lifetime — the handle is never `dlclose`d. This
//! matches how plugin loaders generally behave: plugins are not
//! unloaded mid-process.

use std::path::Path;

use libloading::{Library, Symbol};

use crate::{router::RouteSink, WebError};

/// Symbol name the cdylib must export.
pub const ENTRY_POINT: &[u8] = b"cs_web_register";

/// A loaded cdylib plugin. The underlying [`Library`] is leaked at
/// load time (kept mapped for the process lifetime — see the
/// "Lifetime" section above), so this handle only needs to carry
/// the entry-point pointer.
pub struct Module {
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
        let register = {
            let symbol: Symbol<unsafe extern "Rust" fn(&mut RouteSink)> = library
                .get(ENTRY_POINT)
                .map_err(|e| WebError::Module(format!("lookup cs_web_register: {e}")))?;
            // Detach the symbol's lifetime from the borrow of
            // `library` so the handle can be leaked just below.
            *symbol.into_raw()
        };
        // Keep the library mapped for the process lifetime — see
        // the "Lifetime" section in the module docs. Plugin routes
        // (and detached server tasks holding them) call into this
        // code long after `Module` itself is dropped, so `dlclose`
        // must never run.
        std::mem::forget(library);
        Ok(Self { register })
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
