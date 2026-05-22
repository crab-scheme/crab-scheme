//! Embeds the workspace's crate sources into the `crabscheme` binary so
//! `crabscheme aot --build` works from a **release-installed** binary
//! (no dev source tree on disk) — AOT level 1.
//!
//! Gated on the `bundled-aot-sources` feature (off by default). When off,
//! `BUNDLED_SOURCES` is empty and there's no `include_bytes!` churn on the
//! dev loop; a from-source `cargo install` resolves cs-vm via its on-disk
//! path instead. The release workflow turns the feature on so the shipped
//! tarball's binary is self-contained.
//!
//! Output: `$OUT_DIR/bundled_sources.rs`, a
//! `pub static BUNDLED_SOURCES: &[(&str, &[u8])]` of
//! `(relative-path, include_bytes!(absolute-path))` for the root manifest
//! files + everything under `crates/` (target dirs excluded).

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_BUNDLED_AOT_SOURCES");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("bundled_sources.rs");

    // Feature off → emit an empty table and skip the walk entirely.
    if std::env::var_os("CARGO_FEATURE_BUNDLED_AOT_SOURCES").is_none() {
        fs::write(
            &out,
            "pub static BUNDLED_SOURCES: &[(&str, &[u8])] = &[];\n",
        )
        .unwrap();
        return;
    }

    // Workspace root = crates/cs-cli/../.. .
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ws_root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf();

    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for top in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] {
        let p = ws_root.join(top);
        if p.exists() {
            entries.push((top.to_string(), p));
        }
    }
    collect_dir(&ws_root.join("crates"), &ws_root, &mut entries);
    entries.sort();

    let mut src = String::from("pub static BUNDLED_SOURCES: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &entries {
        // rerun if any embedded file changes (keeps the embed in sync when
        // the feature is on, e.g. release builds).
        println!("cargo:rerun-if-changed={}", abs.display());
        src.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?})),\n",
            abs.display().to_string()
        ));
    }
    src.push_str("];\n");
    fs::write(&out, src).unwrap();
}

/// Recursively collect files under `dir`, recording each as
/// `(path-relative-to-`base`, absolute-path)`. Skips `target/` dirs and
/// hidden entries.
fn collect_dir(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            collect_dir(&path, base, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push((rel.to_string_lossy().replace('\\', "/"), path.clone()));
        }
    }
}
