//! CrabScheme Rust FFI surface.
//!
//! Entry-point crate for the M5b milestone (Rust FFI). Defines the
//! `HostProcedure` trait that user Rust code implements to expose
//! procedures to Scheme, the `FromValue` / `IntoValue` marshaling
//! traits, and the uniform `FfiError` type.
//!
//! Two intended consumers:
//!
//! - **Application authors** embedding CrabScheme as a scripting
//!   layer: depend on `cs-ffi` plus `cs-runtime`, register host
//!   procedures via `Runtime::register_host_procedure`.
//!
//! - **Plugin authors** building shared libraries that
//!   `(load-shared-library "path")` will load at runtime: depend
//!   on `cs-ffi` alone, expose a `crabscheme_register` symbol.
//!   (Iter 6.)
//!
//! See `.spec-workflow/specs/ffi/{requirements,design}.md` and
//! `docs/adr/0008-ffi-design.md`.

#![deny(unsafe_code)]

pub mod error;
pub mod host;
pub mod marshal;

pub use error::FfiError;
pub use host::{HostProcedure, UntypedProc};
pub use marshal::{FromValue, IntoValue};

#[cfg(test)]
mod tests {
    use super::*;
    use cs_core::Value;

    #[test]
    fn re_exports_compile() {
        // Smoke test: every public symbol is reachable from the
        // crate root.
        let _ = UntypedProc::new("noop", |_args: &[Value]| Ok(Value::Unspecified));
    }
}
