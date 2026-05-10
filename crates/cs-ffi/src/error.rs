//! Uniform error type for the FFI boundary.
//!
//! Every variant maps to a specific Scheme condition shape; the
//! runtime side translates `FfiError` into a catchable condition
//! before re-entering the interpreter. See ADR 0008 D-5.

/// Errors that can cross the FFI boundary.
///
/// The runtime translates each variant into a Scheme condition that
/// `with-exception-handler` / `guard` can catch:
/// - `TypeMismatch` -> condition with `&type-error` simple
/// - `ArityError`   -> standard arity error condition
/// - `Panic`        -> `&error` with the panic payload as message
/// - `HostFailure`  -> `&error` with the host-supplied message
#[derive(Debug, Clone)]
pub enum FfiError {
    /// A `FromValue` impl received a value of the wrong type. The
    /// expected type name is static (`"i64"`, `"string"`, …); the
    /// got string is the runtime `type_name` of the actual value.
    TypeMismatch { expected: &'static str, got: String },

    /// A host procedure was called with the wrong number of args.
    ArityError {
        name: String,
        expected: String,
        got: usize,
    },

    /// A Rust panic crossed the FFI boundary. `catch_unwind` at the
    /// boundary translates the panic payload into this variant; the
    /// runtime never sees an aborting panic from a host procedure.
    Panic(String),

    /// A host procedure returned an explicit error. The `String` is
    /// the user-facing message that becomes the `&message` simple
    /// of the resulting Scheme condition.
    HostFailure(String),
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {}, got {}", expected, got)
            }
            FfiError::ArityError {
                name,
                expected,
                got,
            } => {
                write!(
                    f,
                    "{}: expected {} argument(s), got {}",
                    name, expected, got
                )
            }
            FfiError::Panic(msg) => write!(f, "host panic: {}", msg),
            FfiError::HostFailure(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for FfiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_mismatch_display() {
        let e = FfiError::TypeMismatch {
            expected: "i64",
            got: "string".into(),
        };
        let s = format!("{}", e);
        assert!(s.contains("i64"));
        assert!(s.contains("string"));
    }

    #[test]
    fn arity_display_includes_name() {
        let e = FfiError::ArityError {
            name: "my-proc".into(),
            expected: "2".into(),
            got: 3,
        };
        let s = format!("{}", e);
        assert!(s.contains("my-proc"));
        assert!(s.contains("2"));
        assert!(s.contains("3"));
    }

    #[test]
    fn panic_display() {
        let e = FfiError::Panic("kaboom".into());
        assert!(format!("{}", e).contains("kaboom"));
    }

    #[test]
    fn host_failure_passes_message() {
        let e = FfiError::HostFailure("network down".into());
        assert_eq!(format!("{}", e), "network down");
    }
}
