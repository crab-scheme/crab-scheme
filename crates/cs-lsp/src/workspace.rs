//! Workspace symbol search (Phase 5 iter 5.5).
//!
//! Scans every `.scm` file under the workspace root, collects its
//! top-level and nested `define`s (reusing [`crate::symbols`]), and
//! returns those whose name contains the query (case-insensitive). This
//! is the cross-file `workspace/symbol` / Cmd-T feature.

use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{DocumentSymbol, Location, Range, SymbolInformation, SymbolKind, Url};

use crate::symbols::document_symbols;

/// Defines under `root` whose name contains `query` (case-insensitive;
/// empty query returns all).
#[allow(deprecated)] // SymbolInformation + its `deprecated` field
pub fn workspace_symbols(root: &Path, query: &str) -> Vec<SymbolInformation> {
    let needle = query.to_lowercase();
    let mut out = Vec::new();
    for path in scm_files(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(uri) = Url::from_file_path(&path) else {
            continue;
        };
        let mut flat = Vec::new();
        flatten(&document_symbols(uri.as_str(), &text), &mut flat);
        for (name, kind, range) in flat {
            if needle.is_empty() || name.to_lowercase().contains(&needle) {
                out.push(SymbolInformation {
                    name,
                    kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range,
                    },
                    container_name: None,
                });
            }
        }
    }
    out
}

fn flatten(symbols: &[DocumentSymbol], out: &mut Vec<(String, SymbolKind, Range)>) {
    for s in symbols {
        out.push((s.name.clone(), s.kind, s.selection_range));
        if let Some(children) = &s.children {
            flatten(children, out);
        }
    }
}

/// All `.scm` files under `root`, skipping build/VCS/hidden dirs. Capped
/// to keep a huge tree from stalling the request.
fn scm_files(root: &Path) -> Vec<PathBuf> {
    const CAP: usize = 5000;
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= CAP {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            if path.is_dir() {
                if fname.starts_with('.')
                    || matches!(fname.as_ref(), "target" | "node_modules" | "result")
                {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("scm") {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cs-lsp-ws-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_defines_across_files() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.scm"), "(define (alpha x) x)").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/b.scm"), "(define beta 2)\n(define (gamma) 3)").unwrap();

        let all = workspace_symbols(&dir, "");
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "got: {names:?}");
        assert!(names.contains(&"beta"), "got: {names:?}");
        assert!(names.contains(&"gamma"), "got: {names:?}");

        // Query filters by substring.
        let g = workspace_symbols(&dir, "amm");
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].name, "gamma");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_build_dirs() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/gen.scm"), "(define generated 1)").unwrap();
        std::fs::write(dir.join("real.scm"), "(define kept 1)").unwrap();

        let names: Vec<String> = workspace_symbols(&dir, "")
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"kept".to_string()), "got: {names:?}");
        assert!(
            !names.contains(&"generated".to_string()),
            "target/ not skipped"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
