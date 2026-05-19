//! Example cs-web cdylib plugin.
//!
//! Build with `cargo build -p cs-web-mod-example`; the resulting
//! `target/debug/libcs_web_mod_example.{dylib,so,dll}` can be
//! loaded by a cs-web host via `cs_web::Module::load(path)`.

use cs_web::handler::service_fn;
use cs_web::{ok, response, Method, Request, RouteSink, StatusCode};

/// Symbol the cs-web loader expects. See [`cs_web::module`] for
/// the ABI stability constraint (same toolchain on host + plugin).
#[no_mangle]
pub fn cs_web_register(sink: &mut RouteSink) {
    sink.get(
        "/plugin/hello",
        service_fn(|_: Request| async { ok("hello from cs-web-mod-example") }),
    );
    sink.add(
        Method::POST,
        "/plugin/upper",
        service_fn(|req: Request| async move {
            let body = String::from_utf8_lossy(req.body()).to_uppercase();
            response(StatusCode::OK, body)
        }),
    );
}
