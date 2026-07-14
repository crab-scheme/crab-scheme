//! cs-aot-rt — the AOT **level-3** runtime archive.
//!
//! Built as a `staticlib` (`libcs_aot_rt.a`) and bundled per-target in the
//! release tarball. The L3 object backend (cranelift-object) emits a `.o`
//! that references cs-vm's `vm_*_nb` helpers plus the C-ABI boundary shims
//! below; the system `cc` links the `.o` + a generated C `main` against
//! this archive to produce a native binary — **no Rust toolchain required
//! at AOT time** (the L1 cargo+rustc path is used when a toolchain IS
//! present).
//!
//! See `docs/user/aot.md` (AOT levels) and the cs-jit-cranelift object
//! backend.
//!
//! **Symbol retention.** The emitted `.o` references cs-vm's `vm_*_nb`
//! helpers by name; they must survive into the archive even though nothing
//! *in this crate* calls them. Empirically (workspace `lto = "thin"`,
//! `codegen-units = 1`) cs-vm compiles to a single object with all its
//! `#[no_mangle]` (external-linkage) symbols intact, so the consuming `cc`
//! resolves every reference — no explicit `#[used]` force-link table is
//! needed. The `archive_resolves_all_jit_symbols` design note in
//! `docs/user/aot.md` records the proof.

// cs-vnf.2 — mimalloc as the global allocator for the AOT-built binary
// that links this archive. Set here (not in the generated C `main`) so
// every allocation cs-vm/cs-runtime make inside the archive uses it too.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// NB-encode a host `i64` as a Fixnum `NanboxValue` carrier. The generated
/// C `main` calls this to turn parsed argv integers into the NB ABI the
/// emitted entry function consumes (keeps the nan-box encoding in Rust so
/// the C glue stays ABI-agnostic).
#[no_mangle]
pub extern "C" fn cs_aot_nb_fixnum(n: i64) -> i64 {
    cs_vm::vm::NanboxValue::fixnum(n).into_raw()
}

/// Print an NB result carrier as a Scheme value (Write mode), mirroring the
/// L1 main shim: fixnum / flonum fast-path, everything else through
/// `cs_runtime::aot_format_result`. Called by the generated C `main`.
#[no_mangle]
pub extern "C" fn cs_aot_print_result(nb: i64) {
    let v = cs_vm::vm::NanboxValue(nb);
    if let Some(n) = v.as_fixnum() {
        println!("{n}");
    } else if v.is_flonum() {
        println!("{}", f64::from_bits(nb as u64));
    } else {
        println!("{}", cs_runtime::aot_format_result(nb));
    }
}

/// Generic by-name builtin dispatch for the L3 object backend — parity with
/// L1's `cs_runtime::aot_call_builtin`. `name` is a UTF-8 byte buffer
/// (ptr+len); `args` are NB carriers (ptr+len). Returns an NB carrier the
/// caller owns.
///
/// # Safety
/// `name_ptr` must point to `name_len` valid bytes and `args_ptr` to
/// `n_args` valid `i64`s, for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn cs_aot_call_builtin(
    name_ptr: *const u8,
    name_len: usize,
    args_ptr: *const i64,
    n_args: usize,
) -> i64 {
    let name = std::str::from_utf8(std::slice::from_raw_parts(name_ptr, name_len)).unwrap_or("");
    let args = std::slice::from_raw_parts(args_ptr, n_args);
    cs_runtime::aot_call_builtin(name, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nb_fixnum_round_trips() {
        let nb = cs_aot_nb_fixnum(42);
        assert_eq!(cs_vm::vm::NanboxValue(nb).as_fixnum(), Some(42));
    }

    #[test]
    fn call_builtin_dispatches_add() {
        // Mirrors cs_runtime::aot_call_builtin's own dispatch test, but
        // through the C-ABI boundary shim the L3 object backend uses.
        let args = [cs_aot_nb_fixnum(40), cs_aot_nb_fixnum(2)];
        let name = b"+";
        let r =
            unsafe { cs_aot_call_builtin(name.as_ptr(), name.len(), args.as_ptr(), args.len()) };
        assert_eq!(cs_vm::vm::NanboxValue(r).as_fixnum(), Some(42));
    }
}
