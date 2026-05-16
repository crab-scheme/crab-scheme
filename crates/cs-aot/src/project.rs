//! Whole-program AOT pipeline glue.
//!
//! Where [`emit`](crate::emit) and [`emit_with`](crate::emit_with)
//! produce a single function as a Rust source `String`, this module
//! takes a slice of [`Function`]s + an entry-function pick + a
//! target directory, and writes a complete cargo project that
//! builds to a standalone binary.
//!
//! The pipeline is intentionally minimal — just enough to validate
//! that AOT can hit static-binary status for self-recursive numeric
//! kernels. Closures, globals, cross-function calls beyond
//! `CallSelf`, and runtime support for non-numeric values are out
//! of scope for iter 3 and will land alongside iter 4's bench
//! integration + the post-1.0 broader RIR coverage.
//!
//! ## What the emitted project looks like
//!
//! ```text
//! <out_dir>/
//!   Cargo.toml      — declares the package, optionally cs-vm dep
//!   src/main.rs     — all funcs + a `fn main()` shim
//! ```
//!
//! The `main()` shim parses CLI args as decimal `i64`s, encodes them
//! according to the chosen [`EmitMode`], calls the entry function,
//! decodes the result, and prints it. This lets the user invoke the
//! built binary with `./factorial 10` and see `3628800`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cs_rir::Function;

use crate::{
    emit_with_resolver, nb_helpers_source, sanitize_ident_for_project, AotError, EmitMode,
    LambdaResolver,
};

/// Errors specific to project emission (separate from per-function
/// emit errors; those bubble through `Aot`).
#[derive(Debug)]
pub enum ProjectError {
    /// Filesystem error while writing the project skeleton.
    Io(io::Error),
    /// `entry_fn_name` didn't match any function in the input slice.
    EntryNotFound(String),
    /// The emitter rejected one of the functions; the variant carries
    /// (function name, underlying AotError).
    Emit(String, AotError),
    /// Caller passed an empty function slice.
    NoFunctions,
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectError::Io(e) => write!(f, "cs-aot project: I/O error: {e}"),
            ProjectError::EntryNotFound(n) => {
                write!(f, "cs-aot project: entry function `{n}` not in funcs slice")
            }
            ProjectError::Emit(n, e) => {
                write!(f, "cs-aot project: emit error in function `{n}`: {e}")
            }
            ProjectError::NoFunctions => write!(f, "cs-aot project: funcs slice is empty"),
        }
    }
}

impl std::error::Error for ProjectError {}

impl From<io::Error> for ProjectError {
    fn from(e: io::Error) -> Self {
        ProjectError::Io(e)
    }
}

/// How the emitted Cargo.toml refers to cs-vm.
///
/// RC3 Phase 1 iter 1.5: introduced ahead of the cs-vm crates.io
/// publish (iter 1.3). Path-based emission stays the default for
/// dev-tree usage; the Version variant flips on once cs-vm is
/// published, eliminating the "release-installed crabscheme can't
/// AOT because cs-vm isn't at the resolved path" gap documented
/// in `docs/milestones/aot-hardening-plan.md` Phase 1.
#[derive(Debug, Clone)]
pub enum CsVmDep {
    /// `cs-vm = { path = "<absolute path>" }`. Resolves to the
    /// in-workspace cs-vm at build time. Required pre-Phase-1.3;
    /// dev-tree builds always use this.
    Path(PathBuf),
    /// `cs-vm = "<version>"`. Resolves via crates.io. Use once
    /// cs-vm is published. The string is passed verbatim to cargo
    /// — supports caret (`"0.1"`), exact (`"=0.1.2"`), or any
    /// other valid cargo version requirement spec.
    Version(String),
}

impl CsVmDep {
    /// Render as the right-hand-side of `cs-vm = ` in TOML form.
    /// Returns the value sans the `cs-vm = ` prefix so the caller
    /// can compose it into the deps table cleanly.
    pub(crate) fn to_toml(&self) -> String {
        match self {
            CsVmDep::Path(p) => format!("{{ path = \"{}\" }}", p.display()),
            CsVmDep::Version(v) => format!("\"{v}\""),
        }
    }
}

/// Options controlling the emitted cargo project.
#[derive(Debug, Clone)]
pub struct ProjectOptions {
    /// ABI mode for emitted functions. See [`EmitMode`].
    pub mode: EmitMode,
    /// Package name written into the emitted `Cargo.toml`. Becomes
    /// the binary name as well.
    pub package_name: String,
    /// Name of the entry function (must match one of the input
    /// `Function`s). The emitted `main()` calls it with parsed CLI
    /// args. The entry's arity determines how many CLI args main
    /// requires.
    pub entry_fn_name: String,
    /// Where the emitted Cargo.toml gets cs-vm from. See
    /// [`CsVmDep`]. When `None`, falls back to the legacy
    /// [`cs_vm_path`](Self::cs_vm_path) field for backward
    /// compatibility with rc2-era callers; new code should prefer
    /// this field over `cs_vm_path`.
    ///
    /// Required for Nb mode (the emitted main.rs references
    /// `cs_vm::vm::NanboxValue` to encode args + decode the
    /// result). Ignored in RawI64 mode.
    pub cs_vm_dep: Option<CsVmDep>,
    /// **Deprecated** in favor of [`cs_vm_dep`](Self::cs_vm_dep).
    /// Kept for backward compatibility with rc2 callers that
    /// constructed `ProjectOptions` literally. When `cs_vm_dep`
    /// is `None` and this is `Some(path)`, behaves identically to
    /// `cs_vm_dep = Some(CsVmDep::Path(path))`.
    pub cs_vm_path: Option<PathBuf>,
    /// RC3 Phase 6 iter 6.3: when true, the emitted binary takes
    /// `<fn-name> <args...>` and dispatches to one of the input
    /// `Function`s by name. When false (default), the binary
    /// calls the single `entry_fn_name` directly with its CLI
    /// args. Multi-procedure mode is the natural fit for users
    /// who want to AOT a whole utility library; single-entry mode
    /// is what RC2's CLI shipped with.
    pub multi_procedure: bool,
}

impl ProjectOptions {
    /// Resolve the effective cs-vm dependency: prefer `cs_vm_dep`
    /// if set, fall back to wrapping `cs_vm_path` as
    /// `CsVmDep::Path`.
    pub(crate) fn effective_cs_vm_dep(&self) -> Option<CsVmDep> {
        self.cs_vm_dep
            .clone()
            .or_else(|| self.cs_vm_path.clone().map(CsVmDep::Path))
    }
}

/// Result of a successful project emission. Caller passes
/// [`built_binary_path`](Self::built_binary_path) to `cargo build`
/// or runs the binary directly.
#[derive(Debug, Clone)]
pub struct EmittedProject {
    /// The directory the project was written to.
    pub project_dir: PathBuf,
    /// Path the `cargo build --release` will produce. Caller is
    /// responsible for actually invoking cargo.
    pub built_binary_path: PathBuf,
}

/// Write a complete cargo project to `out_dir`. The directory is
/// created (along with `src/`) if missing; existing files are
/// overwritten.
///
/// Returns paths the caller needs to drive cargo + invoke the
/// resulting binary.
pub fn emit_project(
    funcs: &[Function],
    out_dir: &Path,
    opts: &ProjectOptions,
) -> Result<EmittedProject, ProjectError> {
    if funcs.is_empty() {
        return Err(ProjectError::NoFunctions);
    }
    // Resolve the entry function up-front so we know its arity.
    let entry = funcs
        .iter()
        .find(|f| f.name == opts.entry_fn_name)
        .ok_or_else(|| ProjectError::EntryNotFound(opts.entry_fn_name.clone()))?;

    fs::create_dir_all(out_dir.join("src"))?;

    fs::write(out_dir.join("Cargo.toml"), render_cargo_toml(opts))?;
    fs::write(
        out_dir.join("src/main.rs"),
        render_main_rs(funcs, entry, opts)?,
    )?;

    Ok(EmittedProject {
        project_dir: out_dir.to_path_buf(),
        built_binary_path: out_dir
            .join("target")
            .join("release")
            .join(&opts.package_name),
    })
}

fn render_cargo_toml(opts: &ProjectOptions) -> String {
    let mut s = String::new();
    s.push_str("[package]\n");
    s.push_str(&format!("name = \"{}\"\n", opts.package_name));
    s.push_str("version = \"0.0.1\"\n");
    s.push_str("edition = \"2021\"\n\n");

    s.push_str("[dependencies]\n");
    if opts.mode == EmitMode::Nb {
        // Nb mode requires cs-vm in scope for the runtime helpers +
        // NanboxValue encode/decode in the main shim. The dep can be
        // either a path (in-workspace dev usage) or a crates.io
        // version (RC3 Phase 1 iter 1.5 — once iter 1.3 publishes
        // cs-vm). `effective_cs_vm_dep` prefers the new `cs_vm_dep`
        // field, falling back to the rc2-era `cs_vm_path`.
        let dep = opts.effective_cs_vm_dep().expect(
            "ProjectOptions::cs_vm_dep (or legacy cs_vm_path) must be set when mode == Nb \
             (caller should resolve cs-vm's location/version before emitting)",
        );
        s.push_str(&format!("cs-vm = {}\n", dep.to_toml()));
    }
    s.push('\n');

    s.push_str(&format!(
        "[[bin]]\nname = \"{}\"\npath = \"src/main.rs\"\n\n",
        opts.package_name
    ));

    // RC3 Phase 5 iter 5.6 — flip on LTO + codegen-units=1 in the
    // emitted release profile. The workspace default keeps these
    // off for dev-loop predictability, but AOT-emitted projects
    // are one-off binaries that benefit from cross-crate inlining
    // (cs-vm's `vm_value_*_nb` runtime helpers in particular can
    // get inlined into the emitted source under thin LTO when the
    // call site's tag-check predicate is monomorphic). Adds 10-30%
    // to cold-cache build time; eliminates a comparable fraction
    // of runtime overhead on the hot path.
    s.push_str(
        "[profile.release]\n\
         opt-level = 3\n\
         lto = \"thin\"\n\
         codegen-units = 1\n\
         \n",
    );

    // Empty `[workspace]` table: opt this project OUT of any
    // enclosing cargo workspace. Without it, AOT'ing into a
    // directory that lives below an existing workspace's root
    // (e.g. emitting into `target/aot-comparison/<name>/` inside
    // crabscheme's own checkout) fails with "current package
    // believes it's in a workspace when it's not". Standalone
    // emission with no enclosing workspace is unaffected — an
    // empty `[workspace]` table on a single-package manifest is a
    // no-op there.
    s.push_str("[workspace]\n");

    s
}

fn render_main_rs(
    funcs: &[Function],
    entry: &Function,
    opts: &ProjectOptions,
) -> Result<String, ProjectError> {
    let mut src = String::new();
    src.push_str(
        "//! AOT-emitted by cs-aot iter 3.\n\
         //!\n\
         //! Do not edit by hand — re-run cs-aot::project::emit_project to refresh.\n",
    );
    src.push_str("#![allow(unused, unused_unsafe)]\n\n");

    // Nb mode: prepend the inline NB fast-path helpers once at the
    // top of the translation unit. Each emitted function's
    // arith/cmp ops are calls into these helpers (nb_add_inline,
    // etc.) — see `nb_helpers_source` for the contract.
    if opts.mode == EmitMode::Nb {
        src.push_str(nb_helpers_source());
    }

    // RC3 iter 2.2 Step 3: build a resolver from the funcs slice's
    // `lambda_index` fields. MakeClosure(_, idx) in any emitted
    // Function looks up `idx` here to find the dispatch wrapper
    // name + arity. Functions without `lambda_index` set don't
    // contribute (their MakeClosure references would fail with
    // UnsupportedInst — same as before this iter).
    let resolver = LambdaResolver::from_funcs(funcs);

    // Emit every Function in declaration order. Self-recursion (via
    // `CallSelf`) works because each function refers to itself by
    // its sanitized name — Rust resolves the recursive reference at
    // the module scope.
    for f in funcs {
        let body = emit_with_resolver(opts.mode, f, &resolver)
            .map_err(|e| ProjectError::Emit(f.name.clone(), e))?;
        src.push_str(&body);
        src.push('\n');

        // RC3 iter 2.2 Step 2: per-Function dispatch wrapper. A
        // uniform-arity `extern "C" fn(*const i64, usize) -> i64`
        // that vm_alloc_aot_procedure can wrap. The wrapper unpacks
        // the args slice + calls the actual typed fn. Only emitted
        // in Nb mode (RawI64 doesn't interop with cs-vm's Procedure
        // table) and only when the Function has a non-zero arity
        // that the wrapper can statically validate.
        if opts.mode == EmitMode::Nb {
            write_aot_dispatch_wrapper(&mut src, f);
        }
    }

    // Main shim. Two shapes:
    //   single-entry (default): call `entry_fn_name` directly
    //   multi-procedure (iter 6.3): dispatch on args[1] (fn name)
    let aot_version = env!("CARGO_PKG_VERSION");

    if opts.multi_procedure {
        write_multi_procedure_main(&mut src, funcs, opts.mode, aot_version)?;
    } else {
        write_single_entry_main(&mut src, entry, opts.mode, aot_version);
    }

    Ok(src)
}

/// RC3 iter 2.2 Step 2 — emit a uniform-arity dispatch wrapper
/// for an AOT'd Function. The wrapper has signature
/// `extern "C" fn(*const i64, usize) -> i64` (matching cs-vm's
/// `AotDispatchFn` type), unpacks the args slice, and calls the
/// typed AOT fn.
///
/// `cs_vm::vm::vm_alloc_aot_procedure(<name>_aot_dispatch as usize,
/// arity)` then wraps it as a Procedure value usable from Scheme
/// code (via MakeClosure + general Call, iter 2.3+).
fn write_aot_dispatch_wrapper(src: &mut String, f: &Function) {
    let fn_name = sanitize_ident_for_project(&f.name);
    let arity = f.params.len();

    let n_captures = f.captures.len();
    src.push_str(&format!(
        "/// RC3 iter 2.2 + 2.4: dispatch wrapper for `{fn_name}` (arity {arity}, captures {n_captures}).\n\
         /// Called via cs_vm::vm::vm_call_aot_procedure when the\n\
         /// procedure value is invoked from Scheme code. Signature\n\
         /// matches cs_vm::vm::AotDispatchFn (captures + args).\n"
    ));
    let captures_binding = if n_captures == 0 {
        "_captures"
    } else {
        "captures"
    };
    src.push_str(&format!(
        "#[no_mangle]\npub unsafe extern \"C\" fn {fn_name}_aot_dispatch(\
         {captures_binding}: *const i64, _n_captures: usize, \
         args: *const i64, n_args: usize) -> i64 {{\n"
    ));
    src.push_str(&format!(
        "    debug_assert_eq!(_n_captures, {n_captures}, \"{fn_name}_aot_dispatch: n_captures\");\n"
    ));
    src.push_str(&format!(
        "    debug_assert_eq!(n_args, {arity}, \"{fn_name}_aot_dispatch: arity\");\n"
    ));

    // Unpack captures (RC3 iter 2.4) + args + invoke. Captures
    // come first to match the typed fn's signature
    // (`fn(__cap<sym0>, __cap<sym1>, ..., v_param0, ...)`).
    let mut all_loads: Vec<String> = Vec::with_capacity(n_captures + arity);
    for i in 0..n_captures {
        all_loads.push(format!("*captures.add({i})"));
    }
    for i in 0..arity {
        all_loads.push(format!("*args.add({i})"));
    }
    src.push_str(&format!("    {fn_name}({})\n}}\n\n", all_loads.join(", ")));
}

/// Single-entry main shim (RC2 baseline; default).
fn write_single_entry_main(src: &mut String, entry: &Function, mode: EmitMode, aot_version: &str) {
    let entry_name = sanitize_ident_for_project(&entry.name);
    let n_params = entry.params.len();

    src.push_str(&format!(
        "const AOT_PROVENANCE: &str = \"compiled by crabscheme (cs-aot {aot_version}) \
         from entry `{entry_name}` (NB ABI)\";\n\n"
    ));

    src.push_str("fn main() {\n");
    src.push_str("    let args: Vec<String> = std::env::args().collect();\n");
    src.push_str(
        "    if args.iter().any(|a| a == \"--version\" || a == \"-V\") {\n\
         \x20       println!(\"{}\", AOT_PROVENANCE);\n\
         \x20       std::process::exit(0);\n\
         \x20   }\n",
    );
    src.push_str(&format!("    if args.len() < {} {{\n", n_params + 1));
    src.push_str(&format!(
        "        eprintln!(\"usage: {{}} {}\", args[0]);\n",
        (0..n_params)
            .map(|i| format!("<arg{i}>"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    src.push_str("        std::process::exit(2);\n");
    src.push_str("    }\n");

    let arg_exprs: Vec<String> = (0..n_params)
        .map(|i| match mode {
            EmitMode::RawI64 => format!("args[{}].parse::<i64>().unwrap()", i + 1),
            EmitMode::Nb => format!(
                "cs_vm::vm::NanboxValue::fixnum(args[{}].parse::<i64>().unwrap()).into_raw()",
                i + 1
            ),
        })
        .collect();

    src.push_str(&format!(
        "    let raw_result: i64 = {entry_name}({});\n",
        arg_exprs.join(", ")
    ));
    match mode {
        EmitMode::RawI64 => {
            src.push_str("    println!(\"{}\", raw_result);\n");
        }
        EmitMode::Nb => {
            src.push_str("    let nb = cs_vm::vm::NanboxValue(raw_result);\n");
            src.push_str(
                "    let decoded = nb.as_fixnum().expect(\"entry returned a non-Fixnum NB value\");\n",
            );
            src.push_str("    println!(\"{}\", decoded);\n");
        }
    }
    src.push_str("}\n");
}

/// Multi-procedure main shim (RC3 Phase 6 iter 6.3).
///
/// Generates a `main()` that takes `<fn-name> <args...>` and
/// dispatches to one of the input funcs. Each func gets a match
/// arm with per-arity validation + arg parsing + result decoding.
///
/// Provenance line lists all included entries.
fn write_multi_procedure_main(
    src: &mut String,
    funcs: &[Function],
    mode: EmitMode,
    aot_version: &str,
) -> Result<(), ProjectError> {
    // RC3 iter 2.9 — only emit CLI dispatch arms for funcs that have
    // zero captures. Letrec / named-let inner lambdas have at least
    // one capture (their own self-reference, or an outer-scope
    // binding) — the CLI can't synthesize those values, so they're
    // unreachable from `<bin> <fn> <args>`. They still emit as
    // helpers callable from other AOT'd funcs via the resolver.
    let dispatchable: Vec<(&Function, String)> = funcs
        .iter()
        .filter(|f| f.captures.is_empty())
        .map(|f| (f, sanitize_ident_for_project(&f.name)))
        .collect();
    let entry_names: Vec<String> = dispatchable.iter().map(|(_, n)| n.clone()).collect();

    src.push_str(&format!(
        "const AOT_PROVENANCE: &str = \"compiled by crabscheme (cs-aot {aot_version}) \
         from {n} entr{plural}: [{entries}] (NB ABI)\";\n\n",
        n = entry_names.len(),
        plural = if entry_names.len() == 1 { "y" } else { "ies" },
        entries = entry_names.join(", "),
    ));

    src.push_str("fn main() {\n");
    src.push_str("    let args: Vec<String> = std::env::args().collect();\n");
    src.push_str(
        "    if args.iter().any(|a| a == \"--version\" || a == \"-V\") {\n\
         \x20       println!(\"{}\", AOT_PROVENANCE);\n\
         \x20       std::process::exit(0);\n\
         \x20   }\n",
    );
    src.push_str("    if args.len() < 2 {\n");
    src.push_str(&format!(
        "        eprintln!(\"usage: {{}} <fn> <args...>\\navailable: {}\", args[0]);\n",
        entry_names.join(", "),
    ));
    src.push_str("        std::process::exit(2);\n");
    src.push_str("    }\n");
    src.push_str("    let fn_name = args[1].as_str();\n");
    src.push_str("    match fn_name {\n");

    for (func, name) in dispatchable.iter() {
        let n_params = func.params.len();
        // Use the ORIGINAL Scheme name (pre-sanitization) as the
        // dispatch key — that's what the user typed. sanitize_ident_
        // for_project may rewrite `+` → `_` etc., which the user
        // wouldn't think to type.
        let scheme_name = &func.name;
        src.push_str(&format!("        \"{scheme_name}\" => {{\n"));
        src.push_str(&format!(
            "            if args.len() != {} {{\n",
            n_params + 2
        ));
        src.push_str(&format!(
            "                eprintln!(\"usage: {{}} {scheme_name} {}\", args[0]);\n",
            (0..n_params)
                .map(|i| format!("<arg{i}>"))
                .collect::<Vec<_>>()
                .join(" "),
        ));
        src.push_str("                std::process::exit(2);\n");
        src.push_str("            }\n");

        let arg_exprs: Vec<String> = (0..n_params)
            .map(|i| match mode {
                EmitMode::RawI64 => format!("args[{}].parse::<i64>().unwrap()", i + 2),
                EmitMode::Nb => format!(
                    "cs_vm::vm::NanboxValue::fixnum(args[{}].parse::<i64>().unwrap()).into_raw()",
                    i + 2
                ),
            })
            .collect();

        src.push_str(&format!(
            "            let raw_result: i64 = {name}({});\n",
            arg_exprs.join(", ")
        ));
        match mode {
            EmitMode::RawI64 => {
                src.push_str("            println!(\"{}\", raw_result);\n");
            }
            EmitMode::Nb => {
                src.push_str("            let nb = cs_vm::vm::NanboxValue(raw_result);\n");
                src.push_str(&format!(
                    "            let decoded = nb.as_fixnum().expect(\"entry `{scheme_name}` returned a non-Fixnum NB value\");\n"
                ));
                src.push_str("            println!(\"{}\", decoded);\n");
            }
        }
        src.push_str("        }\n");
    }

    src.push_str("        other => {\n");
    src.push_str(&format!(
        "            eprintln!(\"unknown fn `{{}}`; available: {}\", other);\n",
        entry_names.join(", "),
    ));
    src.push_str("            std::process::exit(2);\n");
    src.push_str("        }\n");
    src.push_str("    }\n");
    src.push_str("}\n");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cs_vm_dep_path_emits_path_form() {
        let dep = CsVmDep::Path(PathBuf::from("/abs/path/to/cs-vm"));
        assert_eq!(dep.to_toml(), "{ path = \"/abs/path/to/cs-vm\" }");
    }

    #[test]
    fn cs_vm_dep_version_emits_version_form() {
        let dep = CsVmDep::Version("0.1".to_string());
        assert_eq!(dep.to_toml(), "\"0.1\"");
    }

    #[test]
    fn effective_cs_vm_dep_prefers_new_field() {
        // RC3 Phase 1 iter 1.5: when both fields are set, cs_vm_dep
        // wins (it's the new explicit field; cs_vm_path is the
        // backward-compat fallback).
        let opts = ProjectOptions {
            mode: EmitMode::Nb,
            package_name: "test".into(),
            entry_fn_name: "f".into(),
            cs_vm_dep: Some(CsVmDep::Version("0.2".into())),
            cs_vm_path: Some(PathBuf::from("/should/be/ignored")),
            multi_procedure: false,
        };
        let dep = opts.effective_cs_vm_dep().unwrap();
        assert!(matches!(dep, CsVmDep::Version(v) if v == "0.2"));
    }

    #[test]
    fn effective_cs_vm_dep_falls_back_to_legacy_path() {
        // The rc2-era pattern: only cs_vm_path set, cs_vm_dep None.
        // Must still work — that's the back-compat contract.
        let opts = ProjectOptions {
            mode: EmitMode::Nb,
            package_name: "test".into(),
            entry_fn_name: "f".into(),
            cs_vm_dep: None,
            cs_vm_path: Some(PathBuf::from("/legacy/path")),
            multi_procedure: false,
        };
        let dep = opts.effective_cs_vm_dep().unwrap();
        assert!(matches!(dep, CsVmDep::Path(p) if p == PathBuf::from("/legacy/path")));
    }

    #[test]
    fn effective_cs_vm_dep_none_when_neither_set() {
        // RawI64 mode use case — no cs-vm needed.
        let opts = ProjectOptions {
            mode: EmitMode::RawI64,
            package_name: "test".into(),
            entry_fn_name: "f".into(),
            cs_vm_dep: None,
            cs_vm_path: None,
            multi_procedure: false,
        };
        assert!(opts.effective_cs_vm_dep().is_none());
    }
}
