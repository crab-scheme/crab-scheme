//! cs-pkg — package manifest, lockfile, and library resolver
//! for the R6RS++ package system described in
//! `docs/research/r6rs_extensions_spec.md` §10.
//!
//! Manifest, lockfile, and resolver are independently testable
//! today. The piece that's NOT here yet: integration with
//! cs-expand's import path, so `(import (pkg http server))`
//! actually consults the resolver. That wiring is a follow-up;
//! the surfaces below stay stable so the integration is local.
//!
//! ## v1 scope
//!
//! - Manifest format: a single `(package …)` s-expression
//!   parsed by cs-parse.
//! - Lockfile format: a single `(lock …)` s-expression with
//!   resolved versions + content hashes.
//! - Resolver: given `(pkg <name> <module-path>)`, returns the
//!   filesystem path to the library source under a vendored
//!   tree.
//! - Version constraints: prefix-match (`>=`, `^`, `~`, exact)
//!   over a strict `MAJOR.MINOR.PATCH` semver.
//!
//! Deferred: remote fetch, registry, dependency-solver beyond
//! direct deps, lockfile signing. Per spec §10 these are v2.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use cs_core::SymbolTable;
use cs_diag::SourceMap;
use cs_parse::{read_all, Datum};
use thiserror::Error;

// ============================================================
// Semver
// ============================================================

/// Strict `MAJOR.MINOR.PATCH` version. Pre-release and build
/// metadata are not supported in v1 — the spec calls for "boring
/// + reproducible" over Cargo-level expressiveness.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn parse(s: &str) -> Result<Version, PkgError> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return Err(PkgError::BadVersion(s.into()));
        }
        let parse = |p: &str| p.parse::<u32>().map_err(|_| PkgError::BadVersion(s.into()));
        Ok(Version {
            major: parse(parts[0])?,
            minor: parse(parts[1])?,
            patch: parse(parts[2])?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Version constraint as written in a manifest's `dependencies`
/// section. Modeled after Cargo's subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionReq {
    /// `=1.2.3` — must match exactly.
    Exact(Version),
    /// `>=1.2.3` — any version at or above.
    AtLeast(Version),
    /// `^1.2.3` — major-compatible. `^1.2.3` permits `1.x.y` for
    /// any `x >= 2` (or `2.y` for `y >= 3`), but not `2.0.0`.
    Caret(Version),
    /// `~1.2.3` — minor-compatible. `~1.2.3` permits `1.2.y` for
    /// any `y >= 3`, but not `1.3.0`.
    Tilde(Version),
}

impl VersionReq {
    pub fn parse(s: &str) -> Result<VersionReq, PkgError> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix(">=") {
            Ok(VersionReq::AtLeast(Version::parse(rest.trim())?))
        } else if let Some(rest) = s.strip_prefix('=') {
            Ok(VersionReq::Exact(Version::parse(rest.trim())?))
        } else if let Some(rest) = s.strip_prefix('^') {
            Ok(VersionReq::Caret(Version::parse(rest.trim())?))
        } else if let Some(rest) = s.strip_prefix('~') {
            Ok(VersionReq::Tilde(Version::parse(rest.trim())?))
        } else {
            // Bare version is treated as caret. Matches Cargo's
            // bare default and keeps the manifest concise.
            Ok(VersionReq::Caret(Version::parse(s)?))
        }
    }

    pub fn matches(&self, v: Version) -> bool {
        match self {
            VersionReq::Exact(req) => v == *req,
            VersionReq::AtLeast(req) => v >= *req,
            VersionReq::Caret(req) => {
                // Same major (≥1.0.0); otherwise minor-locked.
                if req.major >= 1 {
                    v.major == req.major && v >= *req
                } else if req.minor >= 1 {
                    v.major == 0 && v.minor == req.minor && v >= *req
                } else {
                    v == *req
                }
            }
            VersionReq::Tilde(req) => {
                v.major == req.major && v.minor == req.minor && v.patch >= req.patch
            }
        }
    }
}

// ============================================================
// Manifest
// ============================================================

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageManifest {
    pub name: String,
    pub version: Version,
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub req: VersionReq,
}

impl PackageManifest {
    /// Parse a manifest from source text. The text is expected to
    /// contain a single `(package …)` form; surrounding
    /// whitespace and comments are tolerated.
    pub fn parse(name: &str, src: &str) -> Result<PackageManifest, PkgError> {
        let mut syms = SymbolTable::new();
        let mut sources = SourceMap::new();
        let file_id = sources.add(name, src);
        let data = read_all(file_id, src, &mut syms).map_err(|errs| {
            let msg = errs
                .into_iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<_>>()
                .join("; ");
            PkgError::ParseFailed(msg)
        })?;
        if data.len() != 1 {
            return Err(PkgError::BadShape(format!(
                "expected exactly one (package …) form, got {}",
                data.len()
            )));
        }
        manifest_from_datum(&data[0], &syms)
    }
}

fn manifest_from_datum(d: &Datum, syms: &SymbolTable) -> Result<PackageManifest, PkgError> {
    let items =
        expect_list(d).ok_or_else(|| PkgError::BadShape("manifest: expected list".into()))?;
    let head = items
        .first()
        .ok_or_else(|| PkgError::BadShape("manifest: empty list".into()))?;
    if symbol_name(head.as_ref(), syms).as_deref() != Some("package") {
        return Err(PkgError::BadShape("manifest: expected (package …)".into()));
    }

    let mut name: Option<String> = None;
    let mut version: Option<Version> = None;
    let mut dependencies: Vec<Dependency> = Vec::new();

    for entry in &items[1..] {
        let pair = expect_list(entry.as_ref())
            .ok_or_else(|| PkgError::BadShape("manifest entry must be a list".into()))?;
        let key = pair
            .first()
            .and_then(|d| symbol_name(d.as_ref(), syms))
            .ok_or_else(|| PkgError::BadShape("manifest entry needs a symbol key".into()))?;
        match key.as_str() {
            "name" => {
                let s = pair
                    .get(1)
                    .and_then(|d| string_value(d.as_ref()))
                    .ok_or_else(|| PkgError::BadShape("(name …) must be a string".into()))?;
                name = Some(s);
            }
            "version" => {
                let s = pair
                    .get(1)
                    .and_then(|d| string_value(d.as_ref()))
                    .ok_or_else(|| PkgError::BadShape("(version …) must be a string".into()))?;
                version = Some(Version::parse(&s)?);
            }
            "dependencies" => {
                for dep in &pair[1..] {
                    dependencies.push(dep_from_datum(dep.as_ref(), syms)?);
                }
            }
            other => {
                return Err(PkgError::BadShape(format!(
                    "unknown manifest entry: {}",
                    other
                )))
            }
        }
    }

    Ok(PackageManifest {
        name: name.ok_or_else(|| PkgError::MissingField("name".into()))?,
        version: version.ok_or_else(|| PkgError::MissingField("version".into()))?,
        dependencies,
    })
}

fn dep_from_datum(d: &Datum, syms: &SymbolTable) -> Result<Dependency, PkgError> {
    let items =
        expect_list(d).ok_or_else(|| PkgError::BadShape("dependency must be a list".into()))?;
    if items.len() != 2 {
        return Err(PkgError::BadShape(
            "dependency must be (name version-constraint)".into(),
        ));
    }
    let name = symbol_name(items[0].as_ref(), syms)
        .ok_or_else(|| PkgError::BadShape("dependency name must be a symbol".into()))?;
    let req_str = string_value(items[1].as_ref())
        .ok_or_else(|| PkgError::BadShape("dependency version must be a string".into()))?;
    let req = VersionReq::parse(&req_str)?;
    Ok(Dependency { name, req })
}

// ============================================================
// Lockfile
// ============================================================

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lockfile {
    pub entries: Vec<LockEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockEntry {
    pub name: String,
    pub version: Version,
    /// Content hash (sha256 hex by convention; opaque string for
    /// the lockfile machinery — verification is a separate layer).
    pub hash: String,
}

impl Lockfile {
    pub fn parse(name: &str, src: &str) -> Result<Lockfile, PkgError> {
        let mut syms = SymbolTable::new();
        let mut sources = SourceMap::new();
        let file_id = sources.add(name, src);
        let data = read_all(file_id, src, &mut syms)
            .map_err(|errs| PkgError::ParseFailed(format!("{:?}", errs)))?;
        if data.len() != 1 {
            return Err(PkgError::BadShape(format!(
                "expected exactly one (lock …) form, got {}",
                data.len()
            )));
        }
        lock_from_datum(&data[0], &syms)
    }

    /// Serialize back to canonical text. Round-trips with `parse`.
    pub fn to_string(&self) -> String {
        let mut out = String::from("(lock");
        for e in &self.entries {
            out.push_str(&format!(
                "\n  (pkg {} \"{}\" \"{}\")",
                e.name, e.version, e.hash
            ));
        }
        out.push(')');
        out
    }
}

fn lock_from_datum(d: &Datum, syms: &SymbolTable) -> Result<Lockfile, PkgError> {
    let items =
        expect_list(d).ok_or_else(|| PkgError::BadShape("lockfile: expected list".into()))?;
    let head = items
        .first()
        .ok_or_else(|| PkgError::BadShape("lockfile: empty".into()))?;
    if symbol_name(head.as_ref(), syms).as_deref() != Some("lock") {
        return Err(PkgError::BadShape("lockfile: expected (lock …)".into()));
    }
    let mut entries = Vec::with_capacity(items.len().saturating_sub(1));
    for entry in &items[1..] {
        let pair = expect_list(entry.as_ref())
            .ok_or_else(|| PkgError::BadShape("lock entry must be a list".into()))?;
        if pair.len() != 4 {
            return Err(PkgError::BadShape(
                "lock entry must be (pkg name version hash)".into(),
            ));
        }
        if symbol_name(pair[0].as_ref(), syms).as_deref() != Some("pkg") {
            return Err(PkgError::BadShape(
                "lock entry must start with 'pkg'".into(),
            ));
        }
        let name = symbol_name(pair[1].as_ref(), syms)
            .ok_or_else(|| PkgError::BadShape("lock entry name must be a symbol".into()))?;
        let version =
            Version::parse(&string_value(pair[2].as_ref()).ok_or_else(|| {
                PkgError::BadShape("lock entry version must be a string".into())
            })?)?;
        let hash = string_value(pair[3].as_ref())
            .ok_or_else(|| PkgError::BadShape("lock entry hash must be a string".into()))?;
        entries.push(LockEntry {
            name,
            version,
            hash,
        });
    }
    Ok(Lockfile { entries })
}

// ============================================================
// Resolver
// ============================================================

/// Maps `(pkg <name> <module-path>)` import requests to a
/// filesystem path. The vendored layout is one
/// `<vendor_root>/<name>-<version>/` directory per locked
/// package, with module sources as `<dotted>/<path>.scm` inside.
pub struct Resolver {
    vendor_root: PathBuf,
    /// (package-name → resolved version)
    versions: std::collections::HashMap<String, Version>,
}

impl Resolver {
    pub fn new(vendor_root: impl Into<PathBuf>) -> Self {
        Self {
            vendor_root: vendor_root.into(),
            versions: Default::default(),
        }
    }

    /// Populate the version map from a lockfile.
    pub fn from_lockfile(vendor_root: impl Into<PathBuf>, lock: &Lockfile) -> Self {
        let mut r = Self::new(vendor_root);
        for entry in &lock.entries {
            r.versions.insert(entry.name.clone(), entry.version);
        }
        r
    }

    /// Add or override a single package's resolved version.
    pub fn set_version(&mut self, name: impl Into<String>, version: Version) {
        self.versions.insert(name.into(), version);
    }

    /// Resolve `(pkg <name> <module-path>)`. The `module_path` is
    /// the rest of the import list joined with `/`; e.g.,
    /// `(pkg http server router)` becomes `server/router.scm`.
    pub fn resolve(&self, name: &str, module_path: &[&str]) -> Result<PathBuf, PkgError> {
        let version = self
            .versions
            .get(name)
            .copied()
            .ok_or_else(|| PkgError::UnknownPackage(name.into()))?;
        let mut p = self.vendor_root.join(format!("{}-{}", name, version));
        for (i, seg) in module_path.iter().enumerate() {
            if i + 1 == module_path.len() {
                p.push(format!("{}.scm", seg));
            } else {
                p.push(seg);
            }
        }
        Ok(p)
    }

    pub fn vendor_root(&self) -> &Path {
        &self.vendor_root
    }

    /// Bridge from a Scheme import-spec datum to a filesystem path.
    /// Recognises `(pkg <name> <module-segment> ...)` shape and
    /// delegates to [`resolve`]. Returns `None` for any other shape
    /// so callers can fall through to their own library-loading
    /// logic.
    ///
    /// This is the integration seam between cs-pkg and the
    /// expander's `IncludeResolver`. The expander itself stays
    /// agnostic of cs-pkg; callers (cs-cli, REPL) install an
    /// `IncludeResolver` that invokes this method on a synthesized
    /// pkg-prefixed path string, or — once cs-expand grows real
    /// library-loading — calls this directly on the import-spec
    /// datum.
    pub fn resolve_import_spec(
        &self,
        spec: &Datum,
        syms: &SymbolTable,
    ) -> Result<Option<PathBuf>, PkgError> {
        let items = match expect_list(spec) {
            Some(v) => v,
            None => return Ok(None),
        };
        if items.is_empty() {
            return Ok(None);
        }
        let head = symbol_name(items[0].as_ref(), syms);
        if head.as_deref() != Some("pkg") {
            return Ok(None);
        }
        if items.len() < 3 {
            return Err(PkgError::BadShape(
                "(pkg <name> <module-segment> ...) needs a name and at least one segment".into(),
            ));
        }
        let name = symbol_name(items[1].as_ref(), syms)
            .ok_or_else(|| PkgError::BadShape("(pkg …) package name must be a symbol".into()))?;
        let segs: Vec<String> = items[2..]
            .iter()
            .map(|d| {
                symbol_name(d.as_ref(), syms).ok_or_else(|| {
                    PkgError::BadShape("(pkg …) module segments must be symbols".into())
                })
            })
            .collect::<Result<_, _>>()?;
        let segs_ref: Vec<&str> = segs.iter().map(|s| s.as_str()).collect();
        self.resolve(&name, &segs_ref).map(Some)
    }
}

// ============================================================
// Errors
// ============================================================

#[derive(Debug, Error)]
pub enum PkgError {
    #[error("bad version: {0:?}")]
    BadVersion(String),
    #[error("parse failed: {0}")]
    ParseFailed(String),
    #[error("bad shape: {0}")]
    BadShape(String),
    #[error("missing field: {0}")]
    MissingField(String),
    #[error("unknown package: {0}")]
    UnknownPackage(String),
}

// ============================================================
// Datum helpers
// ============================================================

fn expect_list(d: &Datum) -> Option<Vec<Rc<Datum>>> {
    let mut out = Vec::new();
    let mut cur = d.clone();
    loop {
        match cur {
            Datum::Null(_) => return Some(out),
            Datum::Pair(car, cdr, _) => {
                out.push(car);
                cur = (*cdr).clone();
            }
            _ => return None,
        }
    }
}

fn symbol_name(d: &Datum, syms: &SymbolTable) -> Option<String> {
    if let Datum::Symbol(s, _) = d {
        Some(syms.name(*s).to_string())
    } else {
        None
    }
}

fn string_value(d: &Datum) -> Option<String> {
    if let Datum::String(s, _) = d {
        Some((**s).clone())
    } else {
        None
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Semver ----

    #[test]
    fn version_parse_and_display() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!(
            v,
            Version {
                major: 1,
                minor: 2,
                patch: 3
            }
        );
        assert_eq!(format!("{}", v), "1.2.3");
    }

    #[test]
    fn version_rejects_garbage() {
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("1.2.x").is_err());
    }

    #[test]
    fn req_exact() {
        let r = VersionReq::parse("=1.2.3").unwrap();
        assert!(r.matches(Version::parse("1.2.3").unwrap()));
        assert!(!r.matches(Version::parse("1.2.4").unwrap()));
    }

    #[test]
    fn req_at_least() {
        let r = VersionReq::parse(">=1.2.3").unwrap();
        assert!(r.matches(Version::parse("1.2.3").unwrap()));
        assert!(r.matches(Version::parse("2.0.0").unwrap()));
        assert!(!r.matches(Version::parse("1.2.2").unwrap()));
    }

    #[test]
    fn req_caret_pre_v1_pins_minor() {
        // ^0.5.0 permits 0.5.x for x ≥ 0, NOT 0.6.0.
        let r = VersionReq::parse("^0.5.0").unwrap();
        assert!(r.matches(Version::parse("0.5.0").unwrap()));
        assert!(r.matches(Version::parse("0.5.4").unwrap()));
        assert!(!r.matches(Version::parse("0.6.0").unwrap()));
        assert!(!r.matches(Version::parse("0.4.9").unwrap()));
    }

    #[test]
    fn req_caret_post_v1_pins_major() {
        let r = VersionReq::parse("^1.2.0").unwrap();
        assert!(r.matches(Version::parse("1.2.0").unwrap()));
        assert!(r.matches(Version::parse("1.9.9").unwrap()));
        assert!(!r.matches(Version::parse("2.0.0").unwrap()));
        assert!(!r.matches(Version::parse("1.1.9").unwrap()));
    }

    #[test]
    fn req_tilde_pins_minor() {
        let r = VersionReq::parse("~1.2.0").unwrap();
        assert!(r.matches(Version::parse("1.2.0").unwrap()));
        assert!(r.matches(Version::parse("1.2.9").unwrap()));
        assert!(!r.matches(Version::parse("1.3.0").unwrap()));
    }

    #[test]
    fn req_bare_is_caret() {
        let r = VersionReq::parse("1.2.0").unwrap();
        assert_eq!(r, VersionReq::Caret(Version::parse("1.2.0").unwrap()));
    }

    // ---- Manifest ----

    #[test]
    fn manifest_minimal() {
        let m = PackageManifest::parse(
            "test",
            r#"(package
                 (name "http")
                 (version "1.2.0"))"#,
        )
        .unwrap();
        assert_eq!(m.name, "http");
        assert_eq!(m.version, Version::parse("1.2.0").unwrap());
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn manifest_with_dependencies() {
        let m = PackageManifest::parse(
            "test",
            r#"(package
                 (name "http")
                 (version "1.2.0")
                 (dependencies
                   (json ">=1.0.0")
                   (match "^0.5.0")))"#,
        )
        .unwrap();
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(m.dependencies[0].name, "json");
        assert!(matches!(m.dependencies[0].req, VersionReq::AtLeast(_)));
        assert_eq!(m.dependencies[1].name, "match");
        assert!(matches!(m.dependencies[1].req, VersionReq::Caret(_)));
    }

    #[test]
    fn manifest_missing_name_errors() {
        let err = PackageManifest::parse("test", r#"(package (version "1.0.0"))"#)
            .expect_err("missing name should fail");
        assert!(matches!(err, PkgError::MissingField(s) if s == "name"));
    }

    #[test]
    fn manifest_bad_version_errors() {
        let err =
            PackageManifest::parse("test", r#"(package (name "x") (version "not-a-version"))"#)
                .expect_err("bad version should fail");
        assert!(matches!(err, PkgError::BadVersion(_)));
    }

    // ---- Lockfile ----

    #[test]
    fn lock_round_trip() {
        let src = r#"(lock
  (pkg http "1.2.0" "abc123")
  (pkg json "1.0.5" "def456"))"#;
        let lock = Lockfile::parse("Lock", src).unwrap();
        assert_eq!(lock.entries.len(), 2);
        assert_eq!(lock.entries[0].name, "http");
        assert_eq!(lock.entries[0].version, Version::parse("1.2.0").unwrap());
        assert_eq!(lock.entries[0].hash, "abc123");

        // Round-trip
        let serialized = lock.to_string();
        let reparsed = Lockfile::parse("Lock", &serialized).unwrap();
        assert_eq!(lock, reparsed);
    }

    #[test]
    fn lock_empty() {
        let lock = Lockfile::parse("Lock", "(lock)").unwrap();
        assert!(lock.entries.is_empty());
    }

    // ---- Resolver ----

    #[test]
    fn resolver_maps_pkg_to_path() {
        let lock = Lockfile::parse(
            "Lock",
            r#"(lock (pkg http "1.2.0" "h") (pkg json "0.5.0" "j"))"#,
        )
        .unwrap();
        let r = Resolver::from_lockfile("/tmp/vendor", &lock);
        let p = r.resolve("http", &["server"]).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/vendor/http-1.2.0/server.scm"));

        let p2 = r.resolve("http", &["server", "router"]).unwrap();
        assert_eq!(
            p2,
            PathBuf::from("/tmp/vendor/http-1.2.0/server/router.scm")
        );
    }

    #[test]
    fn resolver_unknown_package_errors() {
        let r = Resolver::new("/tmp/vendor");
        let err = r.resolve("missing", &["foo"]).unwrap_err();
        assert!(matches!(err, PkgError::UnknownPackage(n) if n == "missing"));
    }

    #[test]
    fn resolver_manual_set_version() {
        let mut r = Resolver::new("/tmp/vendor");
        r.set_version("explicit", Version::parse("3.0.0").unwrap());
        let p = r.resolve("explicit", &["lib"]).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/vendor/explicit-3.0.0/lib.scm"));
    }

    // ---- Import-spec bridge ----

    fn read_one(src: &str) -> (Datum, SymbolTable) {
        let mut syms = SymbolTable::new();
        let mut sm = SourceMap::new();
        let id = sm.add("t", src);
        let data = read_all(id, src, &mut syms).unwrap();
        (data.into_iter().next().unwrap(), syms)
    }

    #[test]
    fn resolve_import_spec_pkg_form() {
        let mut r = Resolver::new("/tmp/v");
        r.set_version("http", Version::parse("1.0.0").unwrap());
        let (spec, syms) = read_one("(pkg http server)");
        let p = r.resolve_import_spec(&spec, &syms).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/v/http-1.0.0/server.scm"));
    }

    #[test]
    fn resolve_import_spec_pkg_form_multi_segment() {
        let mut r = Resolver::new("/tmp/v");
        r.set_version("http", Version::parse("1.0.0").unwrap());
        let (spec, syms) = read_one("(pkg http server router)");
        let p = r.resolve_import_spec(&spec, &syms).unwrap().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/v/http-1.0.0/server/router.scm"));
    }

    #[test]
    fn resolve_import_spec_non_pkg_returns_none() {
        let r = Resolver::new("/tmp/v");
        let (spec, syms) = read_one("(rnrs base)");
        assert_eq!(r.resolve_import_spec(&spec, &syms).unwrap(), None);
    }

    #[test]
    fn resolve_import_spec_pkg_form_errors_on_unknown_package() {
        let r = Resolver::new("/tmp/v");
        let (spec, syms) = read_one("(pkg unknown lib)");
        let err = r.resolve_import_spec(&spec, &syms).unwrap_err();
        assert!(matches!(err, PkgError::UnknownPackage(_)));
    }
}
