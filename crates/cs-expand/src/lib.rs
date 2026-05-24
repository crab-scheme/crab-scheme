//! Core form recognizer.
//!
//! Foundation milestone: this is NOT a hygienic macro expander yet. It
//! recognizes the core forms required by R6RS §11 and lowers them into
//! [`CoreExpr`]. `let`/`let*`/`and`/`or`/`cond`/`when`/`unless` are
//! desugared. `define-syntax` and `syntax-rules` are deferred to M3.

use std::rc::Rc;

use cs_core::{Symbol, SymbolTable, Value};
use cs_diag::Span;
use cs_ir::{CoreExpr, Params};
use cs_parse::Datum;

mod syntax_parse;

#[derive(Clone, Debug)]
pub enum ExpandError {
    UnknownForm { name: String, span: Span },
    BadSyntax { what: String, span: Span },
    EmptyApplication { span: Span },
}

impl ExpandError {
    pub fn span(&self) -> Span {
        match self {
            ExpandError::UnknownForm { span, .. }
            | ExpandError::BadSyntax { span, .. }
            | ExpandError::EmptyApplication { span } => *span,
        }
    }

    pub fn message(&self) -> String {
        match self {
            ExpandError::UnknownForm { name, .. } => format!("unknown form '{}'", name),
            ExpandError::BadSyntax { what, .. } => format!("bad syntax: {}", what),
            ExpandError::EmptyApplication { .. } => "empty application '()'".into(),
        }
    }
}

/// Callback the expander invokes for each `(include "path")` form. Returns
/// `Some((file_id, source))` if the file was found, or `None` to signal a
/// missing-file error. The host (cs-runtime / CLI) supplies this so the
/// expander avoids a direct std::fs dependency and the SourceMap stays
/// owned by a single layer.
pub type IncludeResolver<'a> = dyn FnMut(&str) -> Option<(cs_diag::FileId, String)> + 'a;

/// Callback the expander invokes when an `(import …)` form references a
/// library that hasn't been declared in the current session. The
/// resolver is given the library name spec (e.g., `(rnrs base)` or
/// `(pkg http server)`) as a slice of interned symbols plus read-only
/// access to the SymbolTable for printing, and returns
/// `Some((file_id, source))` if it can locate the library file. The
/// expander then parses + expands that source, expecting it to contain
/// a matching `(library …)` declaration.
///
/// Architectural call: this is the integration seam for cs-pkg,
/// `LIBRARY_PATH`-style search, or any other library-discovery
/// mechanism. cs-expand stays agnostic — it never opens a file or
/// names a package format.
pub type LibraryResolver<'a> =
    dyn FnMut(&[Symbol], &SymbolTable) -> Option<(cs_diag::FileId, String)> + 'a;

/// Cache key for a loaded library: (name-segments, source-hash).
/// Names are stored as `Vec<String>` (printable segment names)
/// rather than `Vec<Symbol>` so the cache stays valid across
/// SymbolTable boundaries -- Symbol IDs are per-table and
/// shouldn't leak into long-lived state. The source-hash is a
/// 64-bit content hash — if the resolved source's hash matches
/// a cached entry, the parse + expand work is skipped and the
/// cached CoreExpr is reused.
pub type LibraryCacheKey = (Vec<String>, u64);

/// A cached library expansion + its direct-import dependency
/// closure. Phase 2F: when A imports B, A's cache entry records
/// `(B's-name-segments, B's-source-hash-at-cache-time)` so the
/// expander can re-resolve B on cache hit and detect upstream
/// changes even if A's own source is unchanged.
///
/// Transitive deps fall out naturally: invalidating B (because
/// some C in B's deps changed) re-expands B, which gets a new
/// hash, which differs from what A cached for its B-dep, which
/// invalidates A.
///
/// Dep names are stored as `Vec<String>` (printable segment
/// names) instead of `Vec<Symbol>` so the cache stays valid
/// across SymbolTable boundaries. Symbol IDs are per-table;
/// reusing them in a different session would index into the
/// wrong slot. The validator re-interns the strings against
/// the current session's SymbolTable before resolving.
#[derive(Clone, Debug)]
pub struct LibraryCacheEntry {
    pub core_expr: CoreExpr,
    /// Each entry: `(dep-library-name-segments, dep-source-hash-at-cache-time)`.
    pub deps: Vec<(Vec<String>, u64)>,
}

/// Cache for the expanded form of cross-file libraries. The
/// expander consults this in [`Expander::try_load_library`]
/// after the resolver returns source but before parsing +
/// expansion. Callers can plug in any backend (in-memory map,
/// on-disk file-system cache, content-addressed store, …); the
/// default in-process backend is [`HashMapLibraryCache`].
pub trait LibraryCache {
    fn get(&self, key: &LibraryCacheKey) -> Option<LibraryCacheEntry>;
    fn put(&mut self, key: LibraryCacheKey, value: LibraryCacheEntry);
}

/// Simple in-memory `LibraryCache` keyed by name+hash. Reset
/// each Expander session unless the caller persists it across
/// sessions.
#[derive(Default)]
pub struct HashMapLibraryCache {
    entries: std::collections::HashMap<LibraryCacheKey, LibraryCacheEntry>,
}

impl HashMapLibraryCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl LibraryCache for HashMapLibraryCache {
    fn get(&self, key: &LibraryCacheKey) -> Option<LibraryCacheEntry> {
        self.entries.get(key).cloned()
    }
    fn put(&mut self, key: LibraryCacheKey, value: LibraryCacheEntry) {
        self.entries.insert(key, value);
    }
}

/// 64-bit content hash for library source. Stable for the
/// process; not stable across rebuilds (uses Rust's
/// `DefaultHasher`). For an on-disk persistent cache, swap to a
/// proper content-addressed hash (sha256, blake3, etc.) at the
/// trait impl layer.
fn hash_library_source(src: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    h.finish()
}

pub struct Expander<'a> {
    pub syms: &'a mut SymbolTable,
    pub macros: &'a mut std::collections::HashMap<Symbol, Macro>,
    /// Cached symbol IDs for the keywords we match on.
    keywords: Keywords,
    /// Counter for synthetic symbols used inside macro expansion. Per-Expander
    /// (so each `eval_str` call resets, but macros across calls work because
    /// the macros table is in the Runtime).
    gensym_counter: u32,
    /// Hook for `(include "path")` forms.
    include_resolver: Option<&'a mut IncludeResolver<'a>>,
    /// Hook for `(import (name …))` forms whose library hasn't been
    /// declared in this session. See [`LibraryResolver`].
    library_resolver: Option<&'a mut LibraryResolver<'a>>,
    /// Optional content-hash cache for cross-file library loads.
    /// When set, the expander consults it in `try_load_library`
    /// before re-parsing + re-expanding identical sources. See
    /// [`LibraryCache`].
    library_cache: Option<&'a mut dyn LibraryCache>,
    /// Per-Expander record-type registry. Populated each time
    /// `define-record-type` expands; consulted when a child names a
    /// `(parent <type-name>)` so we can resolve its tag chain and inherited
    /// field count. This is *expansion-time* state, separate from the
    /// runtime `__record-parents__` hashtable that powers predicate checks.
    record_types: std::collections::HashMap<Symbol, RecordTypeInfo>,
    /// Per-Expander condition-type registry, populated by
    /// `define-condition-type`. Tracks the total field count of each user
    /// condition type so that subtype expansions can compute the offset
    /// of their own fields (inherited fields come first in the vector).
    /// Standard types like `&error` are pre-registered with field_count=0
    /// when first referenced as a parent.
    condition_types: std::collections::HashMap<Symbol, ConditionTypeInfo>,
    /// Per-Expander library registry, populated by `(library ...)`.
    /// Keyed by the library name (a symbol list, version stripped). The
    /// stored info captures the export name list so future namespace
    /// filtering can validate that imported names are actually exported.
    /// Today the body still splices into the importing context — the
    /// registry adds validation and a structural foothold for the
    /// per-library scope frames that land in the next pre-M5 step.
    libraries: std::collections::HashMap<Vec<Symbol>, LibraryInfo>,
    /// Stack of currently-bound syntax-case pattern variables, used
    /// by `(syntax X)` template instantiation. Each entry is
    /// `(name, depth)` -- depth is 0 for ordinary scalar pvars,
    /// 1 for pvars bound under one ellipsis (`...`), 2 for two
    /// nesting levels, etc. Iter C2 introduced depth-1 pvars
    /// (single-pvar ellipsis); Iter C3 adds depth-1 for
    /// compound-pattern ellipsis.
    ///
    /// Each call to `expand_syntax_case` / `expand_with_syntax`
    /// pushes its clause-local pvars before expanding the clause
    /// body and pops them after. Nested syntax-binding forms
    /// inherit the outer pvars by reading the full stack at
    /// expansion time, so `(syntax X)` resolves correctly when X
    /// is bound at any surrounding scope.
    syntax_pvars: Vec<(Symbol, u32)>,
    /// Stack of in-progress library loads' direct-dep
    /// collectors. Phase 2F: when `try_load_library(A)` runs and
    /// recursively triggers `try_load_library(B)` for one of A's
    /// imports, B's `(name, source-hash)` is appended to the top
    /// of this stack so it gets attached to A's cache entry on
    /// completion. Empty when no library is being loaded.
    library_dep_stack: Vec<Vec<(Vec<String>, u64)>>,
    /// Phase 2A.2 user-defined syntax class registry. Each
    /// `(define-syntax-class name predicate)` form binds
    /// `name -> predicate-symbol`. The `define-syntax-parser`
    /// dispatcher consults this map (after the four built-in
    /// classes id/expr/number/string) when resolving `name:class`
    /// annotations in user macro patterns. Cleared per Expander
    /// session; user classes don't persist across sessions
    /// unless registered before the session begins.
    syntax_classes: std::collections::HashMap<Symbol, Symbol>,
    /// Stack of mark-expression Datums, one per enclosing
    /// syntax-case form being expanded. The top-of-stack entry
    /// is consumed by `compile_syntax_template` (Phase 1.5
    /// Iter C) to stamp non-pvar identifiers in templates with
    /// the right per-expansion mark. Standalone `(syntax T)`
    /// outside any syntax-case body (empty stack) gets a
    /// literal 0 (the "unmarked" identifier).
    syntax_mark_exprs: Vec<Datum>,
}

/// Compile-time info recorded for each `(library ...)` declaration.
#[derive(Debug, Clone)]
pub struct LibraryInfo {
    /// The list of names exported by the library (as written in the
    /// `(export ...)` clause). Used to validate `import (only ...)`
    /// shapes against what the library actually provides.
    pub exports: Vec<Symbol>,
}

/// Compile-time info about a `define-record-type`, retained so subtypes can
/// chain off it. `field_count` is the *total* slot count beyond the tag —
/// i.e. inherited + own fields — so the next subtype's accessor offsets
/// follow naturally.
#[derive(Clone, Debug)]
pub struct RecordTypeInfo {
    pub tag: Symbol,
    /// Tag symbols of every ancestor, immediate parent first, root last.
    /// Empty for a root record type.
    pub ancestors: Vec<Symbol>,
    /// Number of fields stored in instance vectors at slot index ≥ 1
    /// (inclusive of inherited fields).
    pub field_count: usize,
}

/// Compile-time info about a `define-condition-type`. Each user-defined
/// condition type stores its inherited+own fields together in a single
/// simple vector (slot 0 = tag, slots 1.. = fields with parent fields
/// first), matching R6RS default-protocol semantics: a constructor takes
/// every ancestor's fields followed by the type's own.
#[derive(Clone, Debug)]
struct ConditionTypeInfo {
    /// Total number of fields stored in this condition's simple vector
    /// (slot 0 is the tag, slots 1..=field_count are fields).
    field_count: usize,
}

/// Field count of a standard R6RS condition type by tag string. Used when
/// a `define-condition-type` parents on a standard type that hasn't been
/// explicitly tracked in `condition_types`. Mirrors the runtime simple
/// constructors in `cs_runtime`: `&message`, `&irritants`, `&who` each
/// carry one field; everything else carries none.
fn standard_condition_field_count(tag: &str) -> usize {
    match tag {
        "&message" | "&irritants" | "&who" => 1,
        _ => 0,
    }
}

/// A user-defined macro, parsed from `(syntax-rules ...)` or
/// `(define-syntax-parser ...)`.
#[derive(Clone, Debug)]
pub struct Macro {
    pub literals: Vec<Symbol>,
    pub rules: Vec<(Datum, Datum)>,
    /// Name (for diagnostics).
    pub name: Symbol,
    /// When true the rules use `syntax-parse` combinators (`~or`,
    /// `~optional`, `~once`) and are matched by the backtracking
    /// matcher in [`syntax_parse`] instead of the deterministic
    /// `syntax-rules` matcher. Set only by `define-syntax-parser`
    /// when a clause actually uses a combinator (R6RS++ Phase 2A.3,
    /// issue #31); plain syntax-rules macros leave it `false` and
    /// keep the original fast path unchanged.
    pub parser: bool,
}

#[derive(Clone, Copy)]
struct Keywords {
    quote: Symbol,
    lambda: Symbol,
    if_: Symbol,
    set_bang: Symbol,
    begin: Symbol,
    define: Symbol,
    let_: Symbol,
    let_star: Symbol,
    letrec: Symbol,
    letrec_star: Symbol,
    and: Symbol,
    or: Symbol,
    when: Symbol,
    unless: Symbol,
    cond: Symbol,
    case: Symbol,
    do_: Symbol,
    guard: Symbol,
    else_: Symbol,
    arrow: Symbol,
    define_record_type: Symbol,
    define_condition_type: Symbol,
    define_values: Symbol,
    library: Symbol,
    define_library: Symbol,
    import: Symbol,
    export: Symbol,
    fields: Symbol,
    parent: Symbol,
    immutable: Symbol,
    mutable: Symbol,
    define_syntax: Symbol,
    define_syntax_parser: Symbol,
    define_syntax_class: Symbol,
    let_syntax: Symbol,
    letrec_syntax: Symbol,
    syntax_rules: Symbol,
    syntax_case: Symbol,
    syntax_: Symbol,
    with_syntax: Symbol,
    quasisyntax: Symbol,
    unsyntax: Symbol,
    unsyntax_splicing: Symbol,
    syntax_error: Symbol,
    ellipsis: Symbol,
    underscore: Symbol,
    delay: Symbol,
    delay_force: Symbol,
    let_values: Symbol,
    let_star_values: Symbol,
    parameterize: Symbol,
    quasiquote: Symbol,
    unquote: Symbol,
    unquote_splicing: Symbol,
    assert_: Symbol,
    case_lambda: Symbol,
    cond_expand: Symbol,
    include: Symbol,
    endianness: Symbol,
    submodule: Symbol,
    // R6RS++ Phase 2A.3 syntax-parse combinators (issue #31). Recognized
    // only inside `define-syntax-parser` patterns; ordinary identifiers
    // everywhere else.
    tilde_or: Symbol,
    tilde_optional: Symbol,
    tilde_once: Symbol,
    kw_defaults: Symbol,
}

impl Keywords {
    fn intern(syms: &mut SymbolTable) -> Self {
        Self {
            quote: syms.intern("quote"),
            lambda: syms.intern("lambda"),
            if_: syms.intern("if"),
            set_bang: syms.intern("set!"),
            begin: syms.intern("begin"),
            define: syms.intern("define"),
            let_: syms.intern("let"),
            let_star: syms.intern("let*"),
            letrec: syms.intern("letrec"),
            letrec_star: syms.intern("letrec*"),
            and: syms.intern("and"),
            or: syms.intern("or"),
            when: syms.intern("when"),
            unless: syms.intern("unless"),
            cond: syms.intern("cond"),
            case: syms.intern("case"),
            do_: syms.intern("do"),
            guard: syms.intern("guard"),
            else_: syms.intern("else"),
            arrow: syms.intern("=>"),
            define_record_type: syms.intern("define-record-type"),
            define_condition_type: syms.intern("define-condition-type"),
            define_values: syms.intern("define-values"),
            library: syms.intern("library"),
            define_library: syms.intern("define-library"),
            import: syms.intern("import"),
            export: syms.intern("export"),
            fields: syms.intern("fields"),
            parent: syms.intern("parent"),
            immutable: syms.intern("immutable"),
            mutable: syms.intern("mutable"),
            define_syntax: syms.intern("define-syntax"),
            define_syntax_parser: syms.intern("define-syntax-parser"),
            define_syntax_class: syms.intern("define-syntax-class"),
            let_syntax: syms.intern("let-syntax"),
            letrec_syntax: syms.intern("letrec-syntax"),
            syntax_rules: syms.intern("syntax-rules"),
            syntax_case: syms.intern("syntax-case"),
            syntax_: syms.intern("syntax"),
            with_syntax: syms.intern("with-syntax"),
            quasisyntax: syms.intern("quasisyntax"),
            unsyntax: syms.intern("unsyntax"),
            unsyntax_splicing: syms.intern("unsyntax-splicing"),
            syntax_error: syms.intern("syntax-error"),
            ellipsis: syms.intern("..."),
            underscore: syms.intern("_"),
            delay: syms.intern("delay"),
            delay_force: syms.intern("delay-force"),
            let_values: syms.intern("let-values"),
            let_star_values: syms.intern("let*-values"),
            parameterize: syms.intern("parameterize"),
            quasiquote: syms.intern("quasiquote"),
            unquote: syms.intern("unquote"),
            unquote_splicing: syms.intern("unquote-splicing"),
            assert_: syms.intern("assert"),
            case_lambda: syms.intern("case-lambda"),
            cond_expand: syms.intern("cond-expand"),
            include: syms.intern("include"),
            endianness: syms.intern("endianness"),
            submodule: syms.intern("submodule"),
            tilde_or: syms.intern("~or"),
            tilde_optional: syms.intern("~optional"),
            tilde_once: syms.intern("~once"),
            kw_defaults: syms.intern("#:defaults"),
        }
    }
}

impl<'a> Expander<'a> {
    pub fn new(
        syms: &'a mut SymbolTable,
        macros: &'a mut std::collections::HashMap<Symbol, Macro>,
    ) -> Self {
        let keywords = Keywords::intern(syms);
        Self {
            syms,
            macros,
            keywords,
            gensym_counter: 0,
            include_resolver: None,
            library_resolver: None,
            library_cache: None,
            record_types: std::collections::HashMap::new(),
            condition_types: std::collections::HashMap::new(),
            syntax_pvars: Vec::new(),
            library_dep_stack: Vec::new(),
            syntax_classes: std::collections::HashMap::new(),
            syntax_mark_exprs: Vec::new(),
            libraries: std::collections::HashMap::new(),
        }
    }

    /// Install an `include` resolver. Calls to `(include "path")` will
    /// invoke this callback with the literal path string from the form.
    pub fn with_include_resolver(mut self, resolver: &'a mut IncludeResolver<'a>) -> Self {
        self.include_resolver = Some(resolver);
        self
    }

    /// Install a library resolver. When `(import (name …))` references
    /// a library not declared in this session, the expander calls
    /// this with the symbol-list name; the resolver returns the
    /// library file's source (or `None` to leave the import as a
    /// no-op rename collector, matching the legacy behavior).
    pub fn with_library_resolver(mut self, resolver: &'a mut LibraryResolver<'a>) -> Self {
        self.library_resolver = Some(resolver);
        self
    }

    /// Install a library cache. When set, the expander consults
    /// the cache by `(name, source-hash)` before parsing +
    /// expanding library source returned from the resolver. On a
    /// hit the cached CoreExpr is reused; on a miss the expander
    /// stores the freshly-expanded form keyed by the same hash.
    pub fn with_library_cache(mut self, cache: &'a mut dyn LibraryCache) -> Self {
        self.library_cache = Some(cache);
        self
    }

    /// Expand a top-level program (sequence of definitions and expressions)
    /// into a single `Begin` whose body lifts top-level defines into runtime
    /// bindings. We model defines as `set!` on a pre-installed binding for
    /// foundation simplicity (the runtime auto-creates them).
    pub fn expand_program(&mut self, data: &[Datum]) -> Result<CoreExpr, ExpandError> {
        if data.is_empty() {
            return Ok(CoreExpr::Const {
                value: Value::Unspecified,
                span: Span::DUMMY,
            });
        }
        let mut exprs: Vec<CoreExpr> = Vec::with_capacity(data.len());
        for d in data {
            exprs.push(self.expand_top(d)?);
        }
        let span = data
            .first()
            .map(Datum::span)
            .unwrap_or(Span::DUMMY)
            .merge(data.last().map(Datum::span).unwrap_or(Span::DUMMY));
        Ok(CoreExpr::Begin { exprs, span })
    }

    fn expand_top(&mut self, d: &Datum) -> Result<CoreExpr, ExpandError> {
        // Recognize top-level `define`, `define-record-type`, `define-syntax`,
        // `include`, and the R6RS module prologue forms `library` / `import`.
        if let Some((head, tail)) = list_head(d) {
            if let Datum::Symbol(s, _) = &*head {
                if *s == self.keywords.define {
                    return self.expand_define(&tail, d.span());
                }
                if *s == self.keywords.define_record_type {
                    return self.expand_define_record_type(&tail, d.span());
                }
                if *s == self.keywords.define_condition_type {
                    return self.expand_define_condition_type(&tail, d.span());
                }
                if *s == self.keywords.define_values {
                    return self.expand_define_values(&tail, d.span());
                }
                if *s == self.keywords.define_syntax {
                    return self.expand_define_syntax(&tail, d.span());
                }
                if *s == self.keywords.define_syntax_parser {
                    return self.expand_define_syntax_parser(&tail, d.span());
                }
                if *s == self.keywords.define_syntax_class {
                    return self.expand_define_syntax_class(&tail, d.span());
                }
                if *s == self.keywords.include {
                    return self.expand_include(&tail, d.span());
                }
                if *s == self.keywords.library {
                    return self.expand_library(&tail, d.span());
                }
                if *s == self.keywords.define_library {
                    return self.expand_define_library(&tail, d.span());
                }
                if *s == self.keywords.import {
                    return self.expand_import(&tail, d.span());
                }
                // Top-level `begin` splices its children at the
                // top level so that a `(begin (define ...) ...)`
                // produced by a macro is treated the same as
                // writing those defines directly. This matches
                // R7RS top-level begin semantics.
                if *s == self.keywords.begin {
                    let mut exprs = Vec::with_capacity(tail.len());
                    for child in &tail {
                        exprs.push(self.expand_top(child)?);
                    }
                    return Ok(CoreExpr::Begin {
                        exprs,
                        span: d.span(),
                    });
                }
                // Top-level macro use: expand once, then route the
                // expansion back through expand_top so a macro that
                // yields a `(define ...)` (or any other top-level-
                // only form) is recognized as such. Without this,
                // the expansion falls into expand_pair which only
                // handles expression-position forms.
                if self.macros.contains_key(s) {
                    let expanded = self.try_expand_macro(*s, d)?;
                    return self.expand_top(&expanded);
                }
            }
        }
        self.expand(d)
    }

    /// `(library (<name> ...) (export ...) (import ...) <body> ...)`
    /// Foundation: we don't have a real library system yet, so the form
    /// is recognized purely so R6RS programs that wrap their code in a
    /// library declaration can run. The library's body is spliced in
    /// place as a `begin`. Imports and exports are accepted but ignored
    /// — every binding is global at this milestone.
    fn expand_library(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 3 {
            return Err(ExpandError::BadSyntax {
                what: "library needs (name) (export ...) (import ...) body...".into(),
                span,
            });
        }
        // items[0] = library name spec. R6RS allows `(<id> ...)` and
        // `(<id> ... (<version-id> ...))` — we accept either, strip the
        // version, and require the leading name parts to be symbols.
        let name_parts = collect_proper_list_strict(&items[0]).ok_or(ExpandError::BadSyntax {
            what: "library: name spec must be a list".into(),
            span: items[0].span(),
        })?;
        if name_parts.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "library: name must have at least one identifier".into(),
                span: items[0].span(),
            });
        }
        let name_syms = self.parse_library_name(&name_parts, items[0].span())?;

        // items[1] = (export <id> ...).
        let export_parts = collect_proper_list_strict(&items[1]).ok_or(ExpandError::BadSyntax {
            what: "library: missing (export ...) clause".into(),
            span: items[1].span(),
        })?;
        if !matches!(export_parts.first(), Some(Datum::Symbol(s, _)) if *s == self.keywords.export)
        {
            return Err(ExpandError::BadSyntax {
                what: "library: second clause must be (export ...)".into(),
                span: items[1].span(),
            });
        }
        let mut exports: Vec<Symbol> = Vec::with_capacity(export_parts.len() - 1);
        for e in &export_parts[1..] {
            match e {
                Datum::Symbol(s, _) => exports.push(*s),
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "export: each name must be an identifier".into(),
                        span: e.span(),
                    })
                }
            }
        }

        // items[2] = (import <import-spec> ...). Reuse expand_import for
        // validation + rename-effect synthesis.
        let import_parts = collect_proper_list_strict(&items[2]).ok_or(ExpandError::BadSyntax {
            what: "library: missing (import ...) clause".into(),
            span: items[2].span(),
        })?;
        if !matches!(import_parts.first(), Some(Datum::Symbol(s, _)) if *s == self.keywords.import)
        {
            return Err(ExpandError::BadSyntax {
                what: "library: third clause must be (import ...)".into(),
                span: items[2].span(),
            });
        }
        let import_expr = self.expand_import(&import_parts[1..], items[2].span())?;

        // Reject duplicate library declarations within one expander pass.
        if self.libraries.contains_key(&name_syms) {
            let printed = name_syms
                .iter()
                .map(|s| self.syms.name(*s))
                .collect::<Vec<_>>()
                .join(" ");
            return Err(ExpandError::BadSyntax {
                what: format!("library ({}) is already declared", printed),
                span: items[0].span(),
            });
        }
        self.libraries.insert(
            name_syms.clone(),
            LibraryInfo {
                exports: exports.clone(),
            },
        );

        // Splice the body in as if it were top-level forms — full
        // namespace isolation requires per-library scope frames, which
        // is the next pre-M5 step. The export list is now validated
        // and tracked, so future scope work has the manifest to filter
        // against.
        //
        // Phase 3B: `(submodule NAME body...)` clauses inside the
        // body are lifted into sibling library declarations named
        // `(parent... NAME)` and expanded after the parent body so
        // they can see the parent's defines (the global-namespace
        // milestone means parent bindings are visible).
        let body = &items[3..];
        let mut exprs: Vec<CoreExpr> = Vec::with_capacity(body.len() + 1);
        // Run the import effects first so library bodies have access
        // to renamed bindings before their `define`s run.
        exprs.push(import_expr);
        let mut deferred_submodules: Vec<CoreExpr> = Vec::new();
        for d in body {
            if let Some((head, sub_tail)) = list_head(d) {
                if let Datum::Symbol(s, _) = &*head {
                    if *s == self.keywords.submodule {
                        let lifted = self.lift_submodule(&name_syms, &sub_tail, d.span())?;
                        deferred_submodules.push(lifted);
                        continue;
                    }
                }
            }
            exprs.push(self.expand_top(d)?);
        }
        exprs.extend(deferred_submodules);
        Ok(CoreExpr::Begin { exprs, span })
    }

    /// Phase 3B: lift a `(submodule NAME body...)` into a sibling
    /// library declaration named `(parent... NAME)` and expand it.
    /// Submodules can optionally provide leading `(export ...)`
    /// and/or `(import ...)` clauses; missing clauses default to
    /// empty `(export)` / `(import)` respectively. Body expressions
    /// see the parent's bindings because the library system is
    /// still global at this milestone.
    fn lift_submodule(
        &mut self,
        parent_name: &[Symbol],
        tail: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if tail.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "submodule needs a name".into(),
                span,
            });
        }
        let sub_name_sym = match &tail[0] {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "submodule name must be a single identifier".into(),
                    span: tail[0].span(),
                })
            }
        };
        // Build the sibling library name list as datum form
        // ((parent...) sub_name).
        let mut full_name_parts: Vec<Datum> = parent_name
            .iter()
            .map(|s| Datum::Symbol(*s, span))
            .collect();
        full_name_parts.push(Datum::Symbol(sub_name_sym, span));
        let name_datum = Self::datum_list(full_name_parts, span);

        // Walk the rest of the submodule's body, extracting any
        // leading `(export ...)` and `(import ...)` clauses, then
        // the body expressions.
        let mut export_clause: Option<Datum> = None;
        let mut import_clause: Option<Datum> = None;
        let mut body_items: Vec<Datum> = Vec::new();
        for d in &tail[1..] {
            if let Some((head, _)) = list_head(d) {
                if let Datum::Symbol(s, _) = &*head {
                    if *s == self.keywords.export && export_clause.is_none() {
                        export_clause = Some(d.clone());
                        continue;
                    }
                    if *s == self.keywords.import && import_clause.is_none() {
                        import_clause = Some(d.clone());
                        continue;
                    }
                }
            }
            body_items.push(d.clone());
        }
        let export_kw = self.keywords.export;
        let import_kw = self.keywords.import;
        let export_datum = export_clause
            .unwrap_or_else(|| Self::datum_list(vec![Datum::Symbol(export_kw, span)], span));
        let import_datum = import_clause
            .unwrap_or_else(|| Self::datum_list(vec![Datum::Symbol(import_kw, span)], span));

        // Synthesize (library (parent... sub) (export ...) (import ...) body...).
        let mut library_parts: Vec<Datum> = vec![
            Datum::Symbol(self.keywords.library, span),
            name_datum,
            export_datum,
            import_datum,
        ];
        library_parts.extend(body_items);
        let library_form = Self::datum_list(library_parts, span);
        self.expand_top(&library_form)
    }

    /// Parse a library name datum list into the canonical symbol list,
    /// dropping any trailing R6RS version sublist.
    fn parse_library_name(
        &mut self,
        parts: &[Datum],
        span: Span,
    ) -> Result<Vec<Symbol>, ExpandError> {
        let mut out: Vec<Symbol> = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            match p {
                Datum::Symbol(s, _) => out.push(*s),
                Datum::Pair(_, _, _) | Datum::Null(_) if i == parts.len() - 1 => {
                    // Trailing version list — accepted but stripped. R6RS
                    // versions are integers, but we don't enforce that
                    // here yet.
                }
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "library name parts must be identifiers \
                               (with an optional trailing version list)"
                            .into(),
                        span: p.span(),
                    })
                }
            }
        }
        if out.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "library name must have at least one identifier".into(),
                span,
            });
        }
        Ok(out)
    }

    /// Public read-only view into the library registry. Lets callers
    /// (the runtime in particular) inspect which libraries an expander
    /// pass declared, e.g. for diagnostic listings or future namespace
    /// filtering enforcement.
    pub fn libraries(&self) -> &std::collections::HashMap<Vec<Symbol>, LibraryInfo> {
        &self.libraries
    }

    /// R7RS `(define-library <name> <library-decl>...)`.
    ///
    /// Recognized library-decl shapes:
    ///   `(export <id>...)`
    ///   `(import <import-spec>...)`
    ///   `(begin <body-expr>...)`
    ///   `(include "path"...)`           — same semantics as top-level include
    ///   `(include-ci "path"...)`        — accepted, treated as include (we
    ///                                    don't case-fold)
    ///   `(cond-expand <clause>...)`     — accepted; not yet evaluated against
    ///                                    R7RS feature set at library time
    ///   `(include-library-declarations ...)` — accepted; ignored for now
    ///
    /// As with `library`, the body forms are spliced as a `begin` into
    /// the importing context; full namespace isolation is M9 work.
    fn expand_define_library(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "define-library: missing name spec".into(),
                span,
            });
        }
        // items[0] = library name spec, same shape as `library`.
        let name_parts = collect_proper_list_strict(&items[0]).ok_or(ExpandError::BadSyntax {
            what: "define-library: name spec must be a list".into(),
            span: items[0].span(),
        })?;
        let name_syms = self.parse_library_name(&name_parts, items[0].span())?;

        let mut exports: Vec<Symbol> = Vec::new();
        let mut import_exprs: Vec<CoreExpr> = Vec::new();
        let mut body_exprs: Vec<CoreExpr> = Vec::new();

        // Cache the clause-head keywords once — the head check is symbol
        // identity, so interning happens on the first hit.
        let export_kw = self.syms.intern("export");
        let begin_kw = self.syms.intern("begin");
        let include_kw = self.syms.intern("include");
        let include_ci_kw = self.syms.intern("include-ci");
        let cond_expand_kw = self.syms.intern("cond-expand");
        let incl_lib_decls_kw = self.syms.intern("include-library-declarations");

        for clause in &items[1..] {
            let parts = collect_proper_list_strict(clause).ok_or(ExpandError::BadSyntax {
                what: "define-library: clause must be a list".into(),
                span: clause.span(),
            })?;
            let head = match parts.first() {
                Some(Datum::Symbol(s, _)) => *s,
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "define-library: clause head must be a keyword".into(),
                        span: clause.span(),
                    })
                }
            };
            if head == export_kw {
                for e in &parts[1..] {
                    match e {
                        Datum::Symbol(s, _) => exports.push(*s),
                        _ => {
                            return Err(ExpandError::BadSyntax {
                                what: "define-library export: each name must be an identifier"
                                    .into(),
                                span: e.span(),
                            })
                        }
                    }
                }
            } else if head == self.keywords.import {
                import_exprs.push(self.expand_import(&parts[1..], clause.span())?);
            } else if head == begin_kw {
                for d in &parts[1..] {
                    body_exprs.push(self.expand_top(d)?);
                }
            } else if head == include_kw || head == include_ci_kw {
                // Reuse the existing include resolver path.
                body_exprs.push(self.expand_include(&parts[1..], clause.span())?);
            } else if head == cond_expand_kw {
                // Inside define-library, cond-expand selects feature-
                // matching body clauses. The clause body is at
                // top-level (defines, define-record-type, etc. are
                // all valid), so we walk clauses ourselves with
                // `expand_top` rather than dispatching to
                // expand_cond_expand which uses expand_body.
                let mut taken = false;
                for cclause in &parts[1..] {
                    let cparts =
                        collect_proper_list_strict(cclause).ok_or(ExpandError::BadSyntax {
                            what: "cond-expand clause must be a list".into(),
                            span: cclause.span(),
                        })?;
                    if cparts.is_empty() {
                        return Err(ExpandError::BadSyntax {
                            what: "cond-expand: empty clause".into(),
                            span: cclause.span(),
                        });
                    }
                    if self.cond_expand_match(&cparts[0]) {
                        for d in &cparts[1..] {
                            // R7RS spells the matched-clause body as a
                            // single `(begin <forms>...)` so the body
                            // can carry multiple top-level forms. We
                            // splice such begins; otherwise the form
                            // is treated as a single top-level form.
                            if let Some((head, tail)) = list_head(d) {
                                if let Datum::Symbol(bh, _) = &*head {
                                    if *bh == self.keywords.begin {
                                        for inner in &tail {
                                            body_exprs.push(self.expand_top(inner)?);
                                        }
                                        continue;
                                    }
                                }
                            }
                            body_exprs.push(self.expand_top(d)?);
                        }
                        taken = true;
                        break;
                    }
                }
                if !taken {
                    return Err(ExpandError::BadSyntax {
                        what: "cond-expand: no matching clause".into(),
                        span: clause.span(),
                    });
                }
            } else if head == incl_lib_decls_kw {
                // M9: accepted but not yet evaluated. Library-decls
                // inclusion requires a more substantial loader.
            } else {
                return Err(ExpandError::BadSyntax {
                    what: format!("define-library: unknown clause '{}'", self.syms.name(head)),
                    span: clause.span(),
                });
            }
        }

        // Reject duplicate library declarations within one expander pass.
        if self.libraries.contains_key(&name_syms) {
            let printed = name_syms
                .iter()
                .map(|s| self.syms.name(*s))
                .collect::<Vec<_>>()
                .join(" ");
            return Err(ExpandError::BadSyntax {
                what: format!("library ({}) is already declared", printed),
                span: items[0].span(),
            });
        }
        self.libraries.insert(
            name_syms.clone(),
            LibraryInfo {
                exports: exports.clone(),
            },
        );

        // Splice imports first, then body. Same model as `expand_library`.
        let mut exprs: Vec<CoreExpr> = Vec::with_capacity(import_exprs.len() + body_exprs.len());
        exprs.extend(import_exprs);
        exprs.extend(body_exprs);
        Ok(CoreExpr::Begin { exprs, span })
    }

    /// `(import <import-spec> ...)` at top level.
    ///
    /// R6RS import-spec shapes recognized here:
    ///   `<library-ref>`                     bare reference, no filter
    ///   `(only <import-spec> id ...)`       restrict to listed ids
    ///   `(except <import-spec> id ...)`     all but the listed ids
    ///   `(prefix <import-spec> <prefix>)`   add a prefix to every id
    ///   `(rename <import-spec> (old new) ...)`  rename selected bindings
    ///
    /// At this milestone we don't track per-library export manifests, so
    /// `only`, `except`, and `prefix` are accepted syntactically but
    /// don't restrict the global namespace (the listed names remain
    /// directly accessible). `rename` does have effect: each
    /// `(<old> <new>)` pair synthesizes a `(define <new> <old>)` so the
    /// renamed binding becomes available alongside the original.
    ///
    /// When library namespace isolation lands the same parser runs but
    /// `only`/`except`/`prefix` start enforcing what's importable and the
    /// library scope filters out non-imported names.
    fn expand_import(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        let mut renames: Vec<(Symbol, Symbol)> = Vec::new();
        let mut loaded_bodies: Vec<CoreExpr> = Vec::new();
        for spec in items {
            self.collect_import_renames(spec, &mut renames, &mut loaded_bodies)?;
        }
        if renames.is_empty() && loaded_bodies.is_empty() {
            return Ok(CoreExpr::Const {
                value: Value::Unspecified,
                span,
            });
        }
        // Splice the loaded library bodies FIRST so their top-level
        // defines run before any subsequent renames refer to them.
        let mut exprs: Vec<CoreExpr> = loaded_bodies;
        // Synthesize `(define <new> <old>)` for each rename. The
        // expander lowers top-level define to CoreExpr::Set, which the
        // runtime treats as a top-level binding (auto-defined on first
        // assignment), so the renamed name becomes a fresh global.
        for (old, new) in renames {
            exprs.push(CoreExpr::Set {
                name: new,
                value: Rc::new(CoreExpr::Ref { name: old, span }),
                span,
            });
        }
        Ok(CoreExpr::Begin { exprs, span })
    }

    /// Try to load a library that hasn't been declared in this
    /// session. Returns the expanded library body's CoreExpr (a
    /// Begin that includes the library's defines) if a resolver
    /// was installed and produced a source. Returns `Ok(None)` if
    /// no resolver was installed — the bare reference behaves as
    /// before (no-op).
    fn try_load_library(
        &mut self,
        name: &[Symbol],
        span: Span,
    ) -> Result<Option<CoreExpr>, ExpandError> {
        // Already loaded in this session — nothing to do.
        if self.libraries.contains_key(name) {
            return Ok(None);
        }
        let (file_id, src) = match &mut self.library_resolver {
            Some(r) => match r(name, self.syms) {
                Some(loaded) => loaded,
                None => return Ok(None),
            },
            None => return Ok(None),
        };
        let source_hash = hash_library_source(&src);

        // Record this library as a direct dep of the parent
        // load (if any). Phase 2F dep-closure tracking: store the
        // observed (name-as-strings, source_hash) on the parent's
        // collection. We convert Symbols to Strings so cached
        // entries stay valid across SymbolTable boundaries.
        if let Some(parent_deps) = self.library_dep_stack.last_mut() {
            let name_strs: Vec<String> = name
                .iter()
                .map(|s| self.syms.name(*s).to_string())
                .collect();
            parent_deps.push((name_strs, source_hash));
        }

        // Cache key uses printable name strings for cross-session
        // SymbolTable safety; see LibraryCacheKey doc.
        let name_strs: Vec<String> = name
            .iter()
            .map(|s| self.syms.name(*s).to_string())
            .collect();
        let cache_key: LibraryCacheKey = (name_strs, source_hash);

        // Content-hash cache lookup. A hit additionally validates
        // each cached dep by re-resolving + re-hashing it; if any
        // upstream lib has drifted we fall through to a fresh
        // expansion.
        let cache_hit_valid: Option<LibraryCacheEntry> = {
            let cached = self
                .library_cache
                .as_deref()
                .and_then(|c| c.get(&cache_key));
            match cached {
                Some(entry) => {
                    if self.cached_deps_still_valid(&entry.deps) {
                        Some(entry)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        if let Some(entry) = cache_hit_valid {
            // Replay the expansion to populate self.libraries
            // via the (library …) declaration's side effect.
            // Then return the cached body.
            let data = cs_parse::read_all(file_id, &src, self.syms)
                .map_err(|errs| parse_err(errs, name, self.syms, span))?;
            for d in &data {
                self.expand_top(d)?;
            }
            return Ok(Some(entry.core_expr));
        }

        // Cache miss (or invalidated): full parse + expand,
        // tracking THIS library's direct deps.
        self.library_dep_stack.push(Vec::new());
        let data = cs_parse::read_all(file_id, &src, self.syms)
            .map_err(|errs| parse_err(errs, name, self.syms, span))?;
        // The loaded file should declare a `(library …)` form. Expand
        // each top-level datum so the (library …) registration
        // populates self.libraries and the body defines splice into
        // the importer's scope.
        let mut exprs: Vec<CoreExpr> = Vec::with_capacity(data.len());
        for d in &data {
            exprs.push(self.expand_top(d)?);
        }
        let collected_deps = self.library_dep_stack.pop().expect("pushed above");
        if !self.libraries.contains_key(name) {
            return Err(ExpandError::BadSyntax {
                what: format!(
                    "library file did not declare library {}",
                    format_library_name(name, self.syms)
                ),
                span,
            });
        }
        let result = CoreExpr::Begin { exprs, span };
        if let Some(cache) = self.library_cache.as_deref_mut() {
            cache.put(
                cache_key,
                LibraryCacheEntry {
                    core_expr: result.clone(),
                    deps: collected_deps,
                },
            );
        }
        Ok(Some(result))
    }

    /// Phase 2F: re-resolve each cached dep and compare its
    /// current source hash to the cached value. Returns true if
    /// every dep still matches, false if any has drifted (which
    /// means the cached entry is stale).
    ///
    /// If the resolver returns None for a dep (gone), treat as
    /// drift (invalidate). If no resolver is installed, treat
    /// as valid (we couldn't have loaded the dep anyway).
    fn cached_deps_still_valid(&mut self, deps: &[(Vec<String>, u64)]) -> bool {
        if self.library_resolver.is_none() {
            return true;
        }
        // Re-intern each dep's printable name into the current
        // SymbolTable, then call the resolver with the fresh
        // Symbol IDs. We do the interning first (with the
        // SymbolTable borrowed mutably) and only afterwards
        // borrow the resolver, so the two mutable borrows don't
        // overlap.
        let mut resolved: Vec<(Vec<Symbol>, u64)> = Vec::with_capacity(deps.len());
        for (dep_name_strs, cached_hash) in deps {
            let name_syms: Vec<Symbol> =
                dep_name_strs.iter().map(|s| self.syms.intern(s)).collect();
            resolved.push((name_syms, *cached_hash));
        }
        let resolver = self.library_resolver.as_deref_mut().expect("checked above");
        for (name_syms, cached_hash) in &resolved {
            match resolver(name_syms, self.syms) {
                Some((_, src)) => {
                    if hash_library_source(&src) != *cached_hash {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }

    /// Walk an import-spec shape and accumulate any `rename` pairs into
    /// `out`. Other modifier shapes (`only`/`except`/`prefix`) are
    /// validated for syntactic well-formedness but contribute nothing —
    /// they become enforceable when we have per-library scopes.
    ///
    /// Bare library references — `(rnrs base)`, `(pkg http server)`,
    /// etc. — additionally try to load the library file via
    /// [`Self::try_load_library`] when a [`LibraryResolver`] is
    /// installed and the library hasn't been declared in this
    /// session. The expanded library body is pushed into
    /// `loaded_bodies` so the caller can splice it into the
    /// importer's compilation BEFORE the rename defines fire.
    fn collect_import_renames(
        &mut self,
        spec: &Datum,
        out: &mut Vec<(Symbol, Symbol)>,
        loaded_bodies: &mut Vec<CoreExpr>,
    ) -> Result<(), ExpandError> {
        let parts = collect_proper_list_strict(spec).ok_or(ExpandError::BadSyntax {
            what: "import spec must be a list".into(),
            span: spec.span(),
        })?;
        if parts.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "import spec must not be empty".into(),
                span: spec.span(),
            });
        }
        // Modifier dispatch: head must be a symbol to match a modifier.
        if let Datum::Symbol(head, _) = &parts[0] {
            let only_sym = self.syms.intern("only");
            let except_sym = self.syms.intern("except");
            let prefix_sym = self.syms.intern("prefix");
            let rename_sym = self.syms.intern("rename");
            if *head == only_sym || *head == except_sym {
                if parts.len() < 2 {
                    return Err(ExpandError::BadSyntax {
                        what: format!(
                            "{} import-spec needs inner spec and at least one id",
                            self.syms.name(*head)
                        ),
                        span: spec.span(),
                    });
                }
                self.collect_import_renames(&parts[1], out, loaded_bodies)?;
                for id in &parts[2..] {
                    if !matches!(id, Datum::Symbol(_, _)) {
                        return Err(ExpandError::BadSyntax {
                            what: "only/except expects identifier names".into(),
                            span: id.span(),
                        });
                    }
                }
                return Ok(());
            }
            if *head == prefix_sym {
                if parts.len() != 3 {
                    return Err(ExpandError::BadSyntax {
                        what: "prefix needs (prefix <import-spec> <id>)".into(),
                        span: spec.span(),
                    });
                }
                self.collect_import_renames(&parts[1], out, loaded_bodies)?;
                if !matches!(&parts[2], Datum::Symbol(_, _)) {
                    return Err(ExpandError::BadSyntax {
                        what: "prefix: third element must be an identifier".into(),
                        span: parts[2].span(),
                    });
                }
                return Ok(());
            }
            if *head == rename_sym {
                if parts.len() < 2 {
                    return Err(ExpandError::BadSyntax {
                        what: "rename needs (rename <import-spec> (old new) ...)".into(),
                        span: spec.span(),
                    });
                }
                self.collect_import_renames(&parts[1], out, loaded_bodies)?;
                for pair in &parts[2..] {
                    let pair_items =
                        collect_proper_list_strict(pair).ok_or(ExpandError::BadSyntax {
                            what: "rename pair must be (old new)".into(),
                            span: pair.span(),
                        })?;
                    if pair_items.len() != 2 {
                        return Err(ExpandError::BadSyntax {
                            what: "rename pair must have exactly two ids".into(),
                            span: pair.span(),
                        });
                    }
                    let old = match &pair_items[0] {
                        Datum::Symbol(s, _) => *s,
                        _ => {
                            return Err(ExpandError::BadSyntax {
                                what: "rename: old name must be an identifier".into(),
                                span: pair_items[0].span(),
                            })
                        }
                    };
                    let new = match &pair_items[1] {
                        Datum::Symbol(s, _) => *s,
                        _ => {
                            return Err(ExpandError::BadSyntax {
                                what: "rename: new name must be an identifier".into(),
                                span: pair_items[1].span(),
                            })
                        }
                    };
                    out.push((old, new));
                }
                return Ok(());
            }
        }
        // Bare library reference. Try to load the library via the
        // installed LibraryResolver if it hasn't been declared in
        // this session. Loading splices the library body's top-level
        // forms (defines, macros, etc.) into the importer's scope —
        // matching the legacy "library body splices into importer"
        // semantics. No rename effect at this layer; per-library
        // export filtering is its own milestone.
        let name = self.parse_library_name(&parts, spec.span())?;
        if let Some(body) = self.try_load_library(&name, spec.span())? {
            loaded_bodies.push(body);
        }
        Ok(())
    }

    /// `(include "path1" "path2" ...)` — at expand time, read each named
    /// file via the installed `IncludeResolver` callback, parse it to
    /// datums, and return the concatenation as a `(begin ...)` so any
    /// top-level forms in the included files participate in the program.
    fn expand_include(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "include needs at least one path".into(),
                span,
            });
        }
        // Eagerly parse every path argument up front so the dispatch is
        // borrow-clean before we touch the resolver / syms.
        let paths: Vec<(String, Span)> = items
            .iter()
            .map(|d| match d {
                Datum::String(s, sp) => Ok(((**s).clone(), *sp)),
                other => Err(ExpandError::BadSyntax {
                    what: "include path must be a string literal".into(),
                    span: other.span(),
                }),
            })
            .collect::<Result<_, _>>()?;
        let mut all_data: Vec<Datum> = Vec::new();
        for (path, p_span) in paths {
            let (file_id, src) = match &mut self.include_resolver {
                Some(resolver) => resolver(&path).ok_or_else(|| ExpandError::BadSyntax {
                    what: format!("include: cannot read {}", path),
                    span: p_span,
                })?,
                None => {
                    return Err(ExpandError::BadSyntax {
                        what: format!("include: no resolver installed (needed for {})", path),
                        span: p_span,
                    });
                }
            };
            // Parse the included source with a fresh reader, attributing
            // datum spans to file_id supplied by the resolver.
            let included = cs_parse::read_all(file_id, &src, self.syms).map_err(|errs| {
                let e = errs.into_iter().next().unwrap();
                ExpandError::BadSyntax {
                    what: format!("include: parse error in {}: {}", path, e.message()),
                    span: p_span,
                }
            })?;
            all_data.extend(included);
        }
        // Expand each included datum as a top-level form so its defines
        // / define-record-type / define-syntax / nested includes work
        // correctly.
        let mut exprs: Vec<CoreExpr> = Vec::with_capacity(all_data.len());
        for d in &all_data {
            exprs.push(self.expand_top(d)?);
        }
        Ok(CoreExpr::Begin { exprs, span })
    }

    /// `(let-syntax ((name spec) ...) body ...)` and
    /// `(letrec-syntax ((name spec) ...) body ...)` — local macros.
    /// Foundation simplification: both behave the same since define-syntax
    /// doesn't allow recursive references in the spec body itself anyway
    /// (the syntax-rules form is just data).
    fn expand_let_syntax(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "let-syntax needs ((name spec)...) body...".into(),
                span,
            });
        }
        let bindings_datum = &items[0];
        let body_datums = &items[1..];

        let bindings = match bindings_datum {
            Datum::Null(_) => Vec::new(),
            _ => collect_proper_list_strict(bindings_datum).ok_or(ExpandError::BadSyntax {
                what: "let-syntax: bindings must be a proper list".into(),
                span: bindings_datum.span(),
            })?,
        };

        // Save existing macros to restore after.
        let mut shadowed: Vec<(cs_core::Symbol, Option<Macro>)> = Vec::new();
        let mut new_names: Vec<cs_core::Symbol> = Vec::new();

        for b in &bindings {
            let parts = collect_proper_list_strict(b).ok_or(ExpandError::BadSyntax {
                what: "let-syntax binding must be (name spec)".into(),
                span: b.span(),
            })?;
            if parts.len() != 2 {
                return Err(ExpandError::BadSyntax {
                    what: "let-syntax binding must be (name spec)".into(),
                    span: b.span(),
                });
            }
            let name = match &parts[0] {
                Datum::Symbol(s, _) => *s,
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "let-syntax: name must be a symbol".into(),
                        span: parts[0].span(),
                    });
                }
            };
            let prev = self.macros.remove(&name);
            shadowed.push((name, prev));
            new_names.push(name);
            // Re-use expand_define_syntax logic by feeding it (name spec).
            let _ = self.expand_define_syntax(&parts, b.span())?;
        }

        // Expand the body in this temporary macro scope.
        let body_expr = if body_datums.len() == 1 {
            self.expand(&body_datums[0])?
        } else {
            let mut exprs = Vec::with_capacity(body_datums.len());
            for d in body_datums {
                exprs.push(self.expand(d)?);
            }
            CoreExpr::Begin { exprs, span }
        };

        // Restore prior macro bindings.
        for name in new_names {
            self.macros.remove(&name);
        }
        for (name, prev) in shadowed {
            if let Some(m) = prev {
                self.macros.insert(name, m);
            }
        }

        Ok(body_expr)
    }

    /// `(define-syntax name (syntax-rules (literals...) (pattern template) ...))`
    /// R6RS++ Phase 2A.2: `(define-syntax-class name predicate)`.
    /// Binds `name` as a user-defined syntax class. After
    /// definition, patterns like `pat:name` inside
    /// `define-syntax-parser` clauses consult the registered
    /// predicate to constrain matched values.
    ///
    /// Simple predicate form only; Racket's compound class
    /// definitions (`(pattern ... #:when ...)`) defer to a later
    /// iter.
    ///
    /// Lowers to a no-op CoreExpr (Unspecified). The registry
    /// update is the side effect.
    fn expand_define_syntax_class(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() != 2 {
            return Err(ExpandError::BadSyntax {
                what: "define-syntax-class: (define-syntax-class name predicate)".into(),
                span,
            });
        }
        let name = match &items[0] {
            Datum::Symbol(s, _) => *s,
            other => {
                return Err(ExpandError::BadSyntax {
                    what: "define-syntax-class: name must be a symbol".into(),
                    span: other.span(),
                });
            }
        };
        let pred = match &items[1] {
            Datum::Symbol(s, _) => *s,
            other => {
                return Err(ExpandError::BadSyntax {
                    what: "define-syntax-class: predicate must be a bare symbol naming a procedure"
                        .into(),
                    span: other.span(),
                });
            }
        };
        self.syntax_classes.insert(name, pred);
        Ok(CoreExpr::Const {
            value: Value::Unspecified,
            span,
        })
    }

    /// R6RS++ Phase 2A.1: `(define-syntax-parser name clause ...)`.
    /// Each clause is `[(_ pat ...) body ...]` where pattern items
    /// may be annotated `id:class` to constrain the matched value
    /// to a specific syntax class.
    ///
    /// Implementation strategy: desugar to `(define-syntax name
    /// (syntax-rules () (<stripped-pat> <class-checked-body>)
    /// ...))` where `:class` annotations are stripped from the
    /// pattern (leaving just the pvar name) and each annotated
    /// pvar gains a runtime predicate check at the top of the
    /// template body. If the check fails, an error is raised.
    ///
    /// Supported classes (resolved via builtin predicates):
    ///   id       -> identifier?
    ///   expr     -> (always #t)
    ///   number   -> number?
    ///   string   -> string?
    ///
    /// Limitation: the class check fires at RUNTIME of the
    /// expanded code, not at expand time. Phase 2A.4 lifts this
    /// to expand-time pinpointing.
    fn expand_define_syntax_parser(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "define-syntax-parser: (define-syntax-parser name clause ...)".into(),
                span,
            });
        }
        let name_datum = items[0].clone();
        let clause_datums = &items[1..];
        // Build the syntax-rules form: (syntax-rules () <translated-clauses>...)
        // For each clause: walk the pattern, strip ":class" annotations,
        // wrap body in class-check ifs.
        let mut translated_clauses: Vec<Datum> = Vec::with_capacity(clause_datums.len());
        // Parallel (stripped-pattern, checked-body) rules for the
        // combinator path (Phase 2A.3): if any clause uses `~or` /
        // `~optional` / `~once`, these become a `parser`-flagged Macro
        // matched by the backtracking `syntax_parse` matcher instead of
        // being desugared to `syntax-rules` (which can't express them).
        let mut parser_rules: Vec<(Datum, Datum)> = Vec::with_capacity(clause_datums.len());
        for clause in clause_datums {
            let parts = collect_proper_list_strict(clause).ok_or(ExpandError::BadSyntax {
                what: "define-syntax-parser clause must be (pattern body ...)".into(),
                span: clause.span(),
            })?;
            if parts.len() < 2 {
                return Err(ExpandError::BadSyntax {
                    what: "define-syntax-parser clause needs pattern + body".into(),
                    span: clause.span(),
                });
            }
            let pattern = &parts[0];
            let body_datums = &parts[1..];
            // Walk the pattern, collecting class checks.
            // class_checks: each entry is (pvar-sym, class-name-str)
            let mut class_checks: Vec<(Symbol, String)> = Vec::new();
            let stripped_pattern = strip_class_annotations(pattern, &mut class_checks, self.syms);
            // Wrap body in a (begin) and prepend class-check ifs from
            // OUTSIDE in (deepest check innermost so first-failing
            // check fires its error first).
            let body_begin = if body_datums.len() == 1 {
                body_datums[0].clone()
            } else {
                let mut all = vec![Datum::Symbol(self.keywords.begin, clause.span())];
                all.extend(body_datums.iter().cloned());
                mk_list(all, clause.span())
            };
            // Build the class-checked body bottom-up.
            let mut checked_body = body_begin;
            for (pvar, class_name) in class_checks.iter().rev() {
                let pred = match class_name.as_str() {
                    "id" => Some(self.syms.intern("identifier?")),
                    "number" => Some(self.syms.intern("number?")),
                    "string" => Some(self.syms.intern("string?")),
                    "expr" => None, // matches anything
                    other => {
                        // Phase 2A.2: consult user-defined
                        // syntax-class registry. Intern the
                        // class-name symbol and look up.
                        let name_sym = self.syms.intern(other);
                        match self.syntax_classes.get(&name_sym) {
                            Some(p) => Some(*p),
                            None => {
                                return Err(ExpandError::BadSyntax {
                                    what: format!(
                                        "define-syntax-parser: unknown syntax class `{}` (built-in: id, expr, number, string; user classes registered via define-syntax-class)",
                                        class_name
                                    ),
                                    span: clause.span(),
                                });
                            }
                        }
                    }
                };
                if let Some(pred_sym) = pred {
                    // For :id, the matched pvar is a literal
                    // identifier -- evaluating it as a variable
                    // ref would crash with undefined. Wrap in
                    // (quote pvar) so the predicate sees the
                    // symbol itself. For value-classes (:number,
                    // :string), the bare pvar is fine: the
                    // matched expression evaluates to a value
                    // and the predicate checks it.
                    let pvar_for_check = if class_name == "id" {
                        mk_list(
                            vec![
                                Datum::Symbol(self.keywords.quote, clause.span()),
                                Datum::Symbol(*pvar, clause.span()),
                            ],
                            clause.span(),
                        )
                    } else {
                        Datum::Symbol(*pvar, clause.span())
                    };
                    // (if (pred pvar-for-check) checked_body
                    //   (error 'name "expected <class>" pvar-for-check))
                    let pred_call = mk_list(
                        vec![
                            Datum::Symbol(pred_sym, clause.span()),
                            pvar_for_check.clone(),
                        ],
                        clause.span(),
                    );
                    let macro_name_sym = match &name_datum {
                        Datum::Symbol(s, _) => *s,
                        _ => self.syms.intern("define-syntax-parser"),
                    };
                    let error_call = mk_list(
                        vec![
                            Datum::Symbol(self.syms.intern("error"), clause.span()),
                            mk_list(
                                vec![
                                    Datum::Symbol(self.keywords.quote, clause.span()),
                                    Datum::Symbol(macro_name_sym, clause.span()),
                                ],
                                clause.span(),
                            ),
                            Datum::String(
                                Rc::new(format!(
                                    "expected {} for `{}`",
                                    class_name,
                                    self.syms.name(*pvar)
                                )),
                                clause.span(),
                            ),
                            pvar_for_check,
                        ],
                        clause.span(),
                    );
                    checked_body = mk_list(
                        vec![
                            Datum::Symbol(self.keywords.if_, clause.span()),
                            pred_call,
                            checked_body,
                            error_call,
                        ],
                        clause.span(),
                    );
                }
            }
            parser_rules.push((stripped_pattern.clone(), checked_body.clone()));
            translated_clauses.push(mk_list(vec![stripped_pattern, checked_body], clause.span()));
        }
        // Combinator path: if any clause uses `~or` / `~optional` /
        // `~once`, register a backtracking parser-macro directly and
        // skip the syntax-rules desugar (which can't express them).
        let parse_syms = self.parse_syms();
        if parser_rules
            .iter()
            .any(|(p, _)| syntax_parse::pattern_uses_combinators(p, &parse_syms))
        {
            let name = match &name_datum {
                Datum::Symbol(s, _) => *s,
                other => {
                    return Err(ExpandError::BadSyntax {
                        what: "define-syntax-parser: name must be a symbol".into(),
                        span: other.span(),
                    });
                }
            };
            self.macros.insert(
                name,
                Macro {
                    literals: Vec::new(),
                    rules: parser_rules,
                    name,
                    parser: true,
                },
            );
            return Ok(CoreExpr::Const {
                value: Value::Unspecified,
                span,
            });
        }
        // Build (define-syntax <name> (syntax-rules () <clauses>...))
        let mut sr_items = vec![
            Datum::Symbol(self.keywords.syntax_rules, span),
            Datum::Null(span), // empty literals list
        ];
        sr_items.extend(translated_clauses);
        let syntax_rules_form = mk_list(sr_items, span);
        let define_syntax_form = mk_list(
            vec![
                Datum::Symbol(self.keywords.define_syntax, span),
                name_datum,
                syntax_rules_form,
            ],
            span,
        );
        // define-syntax is a top-level form, not an expression --
        // it lives in expand_top's dispatch table. Route there
        // directly instead of via self.expand (which goes to
        // expand_pair and wouldn't recognize the form).
        self.expand_top(&define_syntax_form)
    }

    fn expand_define_syntax(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() != 2 {
            return Err(ExpandError::BadSyntax {
                what: "define-syntax: (define-syntax name spec)".into(),
                span,
            });
        }
        let name = match &items[0] {
            Datum::Symbol(s, _) => *s,
            other => {
                return Err(ExpandError::BadSyntax {
                    what: "define-syntax: name must be a symbol".into(),
                    span: other.span(),
                });
            }
        };
        // Parse (syntax-rules (literals) (pattern template) ...).
        let (sr_head, sr_tail) = collect_list(&items[1]).ok_or(ExpandError::BadSyntax {
            what: "define-syntax: spec must be a syntax-rules form".into(),
            span,
        })?;
        match &*sr_head {
            Datum::Symbol(s, _) if *s == self.keywords.syntax_rules => {}
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "define-syntax: only syntax-rules supported (foundation)".into(),
                    span,
                });
            }
        }
        if sr_tail.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "syntax-rules: missing literals + rules".into(),
                span,
            });
        }
        // First element: literals list.
        let literals: Vec<cs_core::Symbol> = match &sr_tail[0] {
            Datum::Null(_) => Vec::new(),
            Datum::Pair(_, _, _) => {
                let lits =
                    collect_proper_list_strict(&sr_tail[0]).ok_or(ExpandError::BadSyntax {
                        what: "syntax-rules: literals must be a list".into(),
                        span,
                    })?;
                let mut out = Vec::with_capacity(lits.len());
                for l in lits {
                    match l {
                        Datum::Symbol(s, _) => out.push(s),
                        other => {
                            return Err(ExpandError::BadSyntax {
                                what: "syntax-rules: literal must be a symbol".into(),
                                span: other.span(),
                            });
                        }
                    }
                }
                out
            }
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "syntax-rules: literals must be a list".into(),
                    span,
                });
            }
        };
        // Remaining: rules.
        let mut rules = Vec::new();
        for rule in &sr_tail[1..] {
            let rparts = collect_proper_list_strict(rule).ok_or(ExpandError::BadSyntax {
                what: "syntax-rules: rule must be (pattern template)".into(),
                span: rule.span(),
            })?;
            if rparts.len() != 2 {
                return Err(ExpandError::BadSyntax {
                    what: "syntax-rules: rule must be (pattern template)".into(),
                    span: rule.span(),
                });
            }
            rules.push((rparts[0].clone(), rparts[1].clone()));
        }
        let m = Macro {
            literals,
            rules,
            name,
            parser: false,
        };
        self.macros.insert(name, m);
        Ok(CoreExpr::Const {
            value: Value::Unspecified,
            span,
        })
    }

    pub fn expand(&mut self, d: &Datum) -> Result<CoreExpr, ExpandError> {
        match d {
            Datum::Boolean(b, span) => Ok(CoreExpr::Const {
                value: Value::Boolean(*b),
                span: *span,
            }),
            Datum::Number(n, span) => Ok(CoreExpr::Const {
                value: Value::Number(n.clone()),
                span: *span,
            }),
            Datum::Character(c, span) => Ok(CoreExpr::Const {
                value: Value::Character(*c),
                span: *span,
            }),
            Datum::String(s, span) => Ok(CoreExpr::Const {
                value: Value::string((**s).clone()),
                span: *span,
            }),
            Datum::Symbol(name, span) => Ok(CoreExpr::Ref {
                name: *name,
                span: *span,
            }),
            Datum::Null(span) => Err(ExpandError::EmptyApplication { span: *span }),
            Datum::Pair(_, _, _) => self.expand_pair(d),
            Datum::Vector(_, span) => Ok(CoreExpr::Const {
                value: d.to_value(),
                span: *span,
            }),
            Datum::ByteVector(_, span) => Ok(CoreExpr::Const {
                value: d.to_value(),
                span: *span,
            }),
        }
    }

    fn expand_pair(&mut self, d: &Datum) -> Result<CoreExpr, ExpandError> {
        let (head, tail_items) = collect_list(d).ok_or_else(|| ExpandError::BadSyntax {
            what: "improper list as expression".into(),
            span: d.span(),
        })?;
        let span = d.span();
        // User-defined macro check first (so macros can shadow special forms).
        if let Datum::Symbol(macro_name, _) = &*head {
            if self.macros.contains_key(macro_name) {
                let expanded = self.try_expand_macro(*macro_name, d)?;
                return self.expand(&expanded);
            }
        }
        if let Datum::Symbol(s, _) = &*head {
            let s = *s;
            if s == self.keywords.quote {
                return self.expand_quote(&tail_items, span);
            }
            if s == self.keywords.if_ {
                return self.expand_if(&tail_items, span);
            }
            if s == self.keywords.set_bang {
                return self.expand_set(&tail_items, span);
            }
            if s == self.keywords.begin {
                return self.expand_begin(&tail_items, span);
            }
            if s == self.keywords.lambda {
                return self.expand_lambda(&tail_items, span);
            }
            if s == self.keywords.let_ {
                return self.expand_let(&tail_items, span);
            }
            if s == self.keywords.let_star {
                return self.expand_let_star(&tail_items, span);
            }
            if s == self.keywords.letrec || s == self.keywords.letrec_star {
                return self.expand_letrec(&tail_items, span);
            }
            if s == self.keywords.and {
                return self.expand_and(&tail_items, span);
            }
            if s == self.keywords.or {
                return self.expand_or(&tail_items, span);
            }
            if s == self.keywords.when {
                return self.expand_when(&tail_items, span, false);
            }
            if s == self.keywords.unless {
                return self.expand_when(&tail_items, span, true);
            }
            if s == self.keywords.cond {
                return self.expand_cond(&tail_items, span);
            }
            if s == self.keywords.case {
                return self.expand_case(&tail_items, span);
            }
            if s == self.keywords.do_ {
                return self.expand_do(&tail_items, span);
            }
            if s == self.keywords.guard {
                return self.expand_guard(&tail_items, span);
            }
            if s == self.keywords.assert_ {
                return self.expand_assert(&tail_items, span);
            }
            if s == self.keywords.endianness {
                return self.expand_endianness(&tail_items, span);
            }
            if s == self.keywords.case_lambda {
                return self.expand_case_lambda(&tail_items, span);
            }
            if s == self.keywords.cond_expand {
                return self.expand_cond_expand(&tail_items, span);
            }
            if s == self.keywords.delay {
                return self.expand_delay(&tail_items, span);
            }
            if s == self.keywords.syntax_error {
                // R7RS (syntax-error message irritant ...) — expansion
                // fails immediately with the supplied message. Useful
                // for syntax-rules templates that need to reject a
                // malformed input pattern.
                return self.expand_syntax_error(&tail_items, span);
            }
            if s == self.keywords.syntax_case {
                return self.expand_syntax_case(&tail_items, span);
            }
            if s == self.keywords.syntax_ {
                return self.expand_syntax_form(&tail_items, span);
            }
            if s == self.keywords.with_syntax {
                return self.expand_with_syntax(&tail_items, span);
            }
            if s == self.keywords.quasisyntax {
                if tail_items.len() != 1 {
                    return Err(ExpandError::BadSyntax {
                        what: "quasisyntax takes exactly 1 argument".into(),
                        span,
                    });
                }
                return self.expand_quasisyntax(&tail_items[0], 1, span);
            }
            if s == self.keywords.unsyntax || s == self.keywords.unsyntax_splicing {
                return Err(ExpandError::BadSyntax {
                    what: "unsyntax / unsyntax-splicing only valid inside quasisyntax".into(),
                    span,
                });
            }
            if s == self.keywords.delay_force {
                // R7RS delay-force: same expansion as delay (a thunk-wrapping
                // promise). Force is the half that distinguishes them — it
                // iterates when the thunk returns another promise, achieving
                // proper iterative tail calls in lazy code.
                return self.expand_delay(&tail_items, span);
            }
            if s == self.keywords.let_values {
                return self.expand_let_values(&tail_items, span, false);
            }
            if s == self.keywords.let_star_values {
                return self.expand_let_values(&tail_items, span, true);
            }
            if s == self.keywords.parameterize {
                return self.expand_parameterize(&tail_items, span);
            }
            if s == self.keywords.quasiquote {
                if tail_items.len() != 1 {
                    return Err(ExpandError::BadSyntax {
                        what: "quasiquote takes exactly 1 argument".into(),
                        span,
                    });
                }
                return self.expand_quasiquote(&tail_items[0], 1, span);
            }
            if s == self.keywords.let_syntax || s == self.keywords.letrec_syntax {
                return self.expand_let_syntax(&tail_items, span);
            }
            if s == self.keywords.define {
                // Internal defines that escape to here are nested-position
                // ones (not in body head) — those aren't allowed by R6RS.
                return Err(ExpandError::BadSyntax {
                    what: "define not allowed in expression position".into(),
                    span,
                });
            }
        }
        // Application: expand head and arguments.
        let func = self.expand(&head)?;
        let mut args = Vec::with_capacity(tail_items.len());
        for d in &tail_items {
            args.push(self.expand(d)?);
        }
        Ok(CoreExpr::App {
            func: Rc::new(func),
            args,
            span,
        })
    }

    fn expand_quote(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 1 {
            return Err(ExpandError::BadSyntax {
                what: "quote takes exactly 1 argument".into(),
                span,
            });
        }
        Ok(CoreExpr::Const {
            value: items[0].to_value(),
            span,
        })
    }

    fn expand_if(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 2 && items.len() != 3 {
            return Err(ExpandError::BadSyntax {
                what: "if takes 2 or 3 arguments".into(),
                span,
            });
        }
        let cond = self.expand(&items[0])?;
        let then = self.expand(&items[1])?;
        let alt = if items.len() == 3 {
            self.expand(&items[2])?
        } else {
            CoreExpr::Const {
                value: Value::Unspecified,
                span,
            }
        };
        Ok(CoreExpr::If {
            cond: Rc::new(cond),
            then: Rc::new(then),
            alt: Rc::new(alt),
            span,
        })
    }

    fn expand_set(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 2 {
            return Err(ExpandError::BadSyntax {
                what: "set! takes exactly 2 arguments".into(),
                span,
            });
        }
        let name = match &items[0] {
            Datum::Symbol(s, _) => *s,
            other => {
                return Err(ExpandError::BadSyntax {
                    what: "set! target must be a symbol".into(),
                    span: other.span(),
                });
            }
        };
        let value = self.expand(&items[1])?;
        Ok(CoreExpr::Set {
            name,
            value: Rc::new(value),
            span,
        })
    }

    fn expand_begin(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Ok(CoreExpr::Const {
                value: Value::Unspecified,
                span,
            });
        }
        let mut exprs = Vec::with_capacity(items.len());
        for d in items {
            exprs.push(self.expand(d)?);
        }
        Ok(CoreExpr::Begin { exprs, span })
    }

    fn expand_lambda(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "lambda needs params and body".into(),
                span,
            });
        }
        let params = parse_lambda_params(&items[0])?;
        let body = self.expand_body(&items[1..], span)?;
        Ok(CoreExpr::Lambda {
            params,
            body: Rc::new(body),
            span,
        })
    }

    fn expand_body(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "empty body".into(),
                span,
            });
        }
        // Collect leading `define` forms and lift them into a letrec* per
        // R6RS §11.4.6 internal-definition semantics.
        let mut defs: Vec<(cs_core::Symbol, CoreExpr)> = Vec::new();
        let mut idx = 0usize;
        while idx < items.len() {
            let it = &items[idx];
            if let Some((head, rest)) = collect_list(it) {
                if let Datum::Symbol(s, _) = &*head {
                    if *s == self.keywords.define {
                        // Parse the define and capture (name, value-expr).
                        let (name, value_expr) = self.parse_internal_define(&rest, it.span())?;
                        defs.push((name, value_expr));
                        idx += 1;
                        continue;
                    }
                }
            }
            break;
        }
        let body_items = &items[idx..];
        if body_items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "body must contain at least one expression after defines".into(),
                span,
            });
        }
        let body_expr = if body_items.len() == 1 {
            self.expand(&body_items[0])?
        } else {
            let mut exprs = Vec::with_capacity(body_items.len());
            for d in body_items {
                exprs.push(self.expand(d)?);
            }
            CoreExpr::Begin { exprs, span }
        };
        if defs.is_empty() {
            return Ok(body_expr);
        }
        Ok(CoreExpr::Letrec {
            bindings: defs,
            body: Rc::new(body_expr),
            span,
        })
    }

    /// Parse `(define x v)` or `(define (f args ...) body ...)` for internal
    /// define lifting. Returns (name, value expression).
    fn parse_internal_define(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<(cs_core::Symbol, CoreExpr), ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "define needs name and value".into(),
                span,
            });
        }
        match &items[0] {
            Datum::Symbol(name, _) => {
                if items.len() != 2 {
                    return Err(ExpandError::BadSyntax {
                        what: "(define name value)".into(),
                        span,
                    });
                }
                let value = self.expand(&items[1])?;
                Ok((*name, value))
            }
            Datum::Pair(_, _, _) => {
                let (head, rest) = collect_list(&items[0]).ok_or(ExpandError::BadSyntax {
                    what: "define: bad function form".into(),
                    span,
                })?;
                let name = match &*head {
                    Datum::Symbol(s, _) => *s,
                    _ => {
                        return Err(ExpandError::BadSyntax {
                            what: "define: function name must be symbol".into(),
                            span,
                        });
                    }
                };
                let params = build_params_from_datums(&rest)?;
                let body = self.expand_body(&items[1..], span)?;
                let lam = CoreExpr::Lambda {
                    params,
                    body: Rc::new(body),
                    span,
                };
                Ok((name, lam))
            }
            other => Err(ExpandError::BadSyntax {
                what: "define: invalid target".into(),
                span: other.span(),
            }),
        }
    }

    fn expand_define(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "define needs name and value".into(),
                span,
            });
        }
        match &items[0] {
            Datum::Symbol(name, _) => {
                if items.len() != 2 {
                    return Err(ExpandError::BadSyntax {
                        what: "(define name value)".into(),
                        span,
                    });
                }
                let value = self.expand(&items[1])?;
                Ok(CoreExpr::Set {
                    name: *name,
                    value: Rc::new(value),
                    span,
                })
            }
            // (define (name args...) body...) sugar
            Datum::Pair(_, _, _) => {
                let (head, rest) = collect_list(&items[0]).ok_or(ExpandError::BadSyntax {
                    what: "define: bad function form".into(),
                    span,
                })?;
                let name = match &*head {
                    Datum::Symbol(s, _) => *s,
                    _ => {
                        return Err(ExpandError::BadSyntax {
                            what: "define: function name must be symbol".into(),
                            span,
                        });
                    }
                };
                let params = build_params_from_datums(&rest)?;
                let body = self.expand_body(&items[1..], span)?;
                let lam = CoreExpr::Lambda {
                    params,
                    body: Rc::new(body),
                    span,
                };
                Ok(CoreExpr::Set {
                    name,
                    value: Rc::new(lam),
                    span,
                })
            }
            other => Err(ExpandError::BadSyntax {
                what: "define: invalid target".into(),
                span: other.span(),
            }),
        }
    }

    /// `(define-values <formals> <expression>)` — R6RS multiple-value
    /// binding at top level. `<formals>` matches lambda formals: a fixed
    /// list, a single rest symbol, or a fixed list with a dotted rest tail.
    /// Desugars to declarations that initialize each name to Unspecified
    /// followed by a `call-with-values` that mutates them in place.
    fn expand_define_values(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() != 2 {
            return Err(ExpandError::BadSyntax {
                what: "define-values needs <formals> and <expression>".into(),
                span,
            });
        }
        let params = build_params_from_datums_loose(&items[0])?;
        let expr = self.expand(&items[1])?;

        // Synthesize lambda parameter names so we can shadow user names
        // inside the call-with-values consumer (the consumer just forwards
        // each into the right top-level binding via `set!`).
        let mut consumer_fixed: Vec<Symbol> = Vec::with_capacity(params.fixed.len());
        for (i, _) in params.fixed.iter().enumerate() {
            consumer_fixed.push(self.syms.intern(&format!("__dv-arg-{}__", i)));
        }
        let consumer_rest: Option<Symbol> = params.rest.map(|_| self.syms.intern("__dv-rest__"));

        let mut out: Vec<CoreExpr> = Vec::new();
        // 1. Pre-declare each name with Unspecified so subsequent code can
        // see the binding even before the call-with-values runs.
        for name in &params.fixed {
            out.push(CoreExpr::Set {
                name: *name,
                value: Rc::new(CoreExpr::Const {
                    value: Value::Unspecified,
                    span,
                }),
                span,
            });
        }
        if let Some(rest_name) = params.rest {
            out.push(CoreExpr::Set {
                name: rest_name,
                value: Rc::new(CoreExpr::Const {
                    value: Value::Unspecified,
                    span,
                }),
                span,
            });
        }
        // 2. Build the consumer body: a sequence of `(set! <user-name> <synth-arg>)`.
        let mut sets: Vec<CoreExpr> = Vec::with_capacity(params.fixed.len() + 1);
        for (user_name, synth) in params.fixed.iter().zip(consumer_fixed.iter()) {
            sets.push(CoreExpr::Set {
                name: *user_name,
                value: Rc::new(CoreExpr::Ref { name: *synth, span }),
                span,
            });
        }
        if let (Some(user_rest), Some(synth_rest)) = (params.rest, consumer_rest) {
            sets.push(CoreExpr::Set {
                name: user_rest,
                value: Rc::new(CoreExpr::Ref {
                    name: synth_rest,
                    span,
                }),
                span,
            });
        }
        let consumer_body = if sets.len() == 1 {
            sets.pop().unwrap()
        } else if sets.is_empty() {
            CoreExpr::Const {
                value: Value::Unspecified,
                span,
            }
        } else {
            CoreExpr::Begin { exprs: sets, span }
        };
        let consumer = CoreExpr::Lambda {
            params: Params {
                fixed: consumer_fixed,
                rest: consumer_rest,
            },
            body: Rc::new(consumer_body),
            span,
        };
        let producer = CoreExpr::Lambda {
            params: Params::fixed(Vec::new()),
            body: Rc::new(expr),
            span,
        };
        let cwv_sym = self.syms.intern("call-with-values");
        out.push(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cwv_sym,
                span,
            }),
            args: vec![producer, consumer],
            span,
        });

        Ok(CoreExpr::Begin { exprs: out, span })
    }

    fn expand_let(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        // Two shapes:
        //   (let ((name expr) ...) body...) -> ((lambda (name ...) body...) expr ...)
        //   (let LOOP ((name expr) ...) body...) -> named let, expands to:
        //     (letrec ((LOOP (lambda (name ...) body...))) (LOOP expr ...))
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "let needs bindings and body".into(),
                span,
            });
        }
        // Named let: first element is a symbol (the loop name), and there's
        // at least 2 more (bindings + body).
        if let Datum::Symbol(loop_name, _) = &items[0] {
            if items.len() < 3 {
                return Err(ExpandError::BadSyntax {
                    what: "named let needs name, bindings, and body".into(),
                    span,
                });
            }
            let bindings = parse_bindings(&items[1])?;
            let mut names = Vec::with_capacity(bindings.len());
            let mut value_exprs = Vec::with_capacity(bindings.len());
            for (n, e) in bindings {
                names.push(n);
                let ex = self.expand(&e)?;
                value_exprs.push(ex);
            }
            let body = self.expand_body(&items[2..], span)?;
            let lam = CoreExpr::Lambda {
                params: Params::fixed(names),
                body: Rc::new(body),
                span,
            };
            // (letrec ((LOOP lam)) (LOOP exprs...))
            let call = CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: *loop_name,
                    span,
                }),
                args: value_exprs,
                span,
            };
            return Ok(CoreExpr::Letrec {
                bindings: vec![(*loop_name, lam)],
                body: Rc::new(call),
                span,
            });
        }
        // Plain let.
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "let needs bindings and body".into(),
                span,
            });
        }
        let bindings = parse_bindings(&items[0])?;
        let mut names = Vec::with_capacity(bindings.len());
        let mut exprs = Vec::with_capacity(bindings.len());
        for (n, e) in bindings {
            names.push(n);
            exprs.push(e);
        }
        let body = self.expand_body(&items[1..], span)?;
        let mut arg_exprs = Vec::with_capacity(exprs.len());
        for d in &exprs {
            arg_exprs.push(self.expand(d)?);
        }
        let lam = CoreExpr::Lambda {
            params: Params::fixed(names),
            body: Rc::new(body),
            span,
        };
        Ok(CoreExpr::App {
            func: Rc::new(lam),
            args: arg_exprs,
            span,
        })
    }

    fn expand_let_star(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        // (let* ((x e1) (y e2)) body) -> (let ((x e1)) (let ((y e2)) body))
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "let* needs bindings and body".into(),
                span,
            });
        }
        let bindings = parse_bindings(&items[0])?;
        let body_items = &items[1..];
        let body = self.expand_body(body_items, span)?;
        let mut acc = body;
        for (name, expr) in bindings.into_iter().rev() {
            let value = self.expand(&expr)?;
            let lam = CoreExpr::Lambda {
                params: Params::fixed(vec![name]),
                body: Rc::new(acc),
                span,
            };
            acc = CoreExpr::App {
                func: Rc::new(lam),
                args: vec![value],
                span,
            };
        }
        Ok(acc)
    }

    fn expand_letrec(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "letrec needs bindings and body".into(),
                span,
            });
        }
        let bindings = parse_bindings(&items[0])?;
        let mut bs = Vec::with_capacity(bindings.len());
        for (n, e) in bindings {
            let ex = self.expand(&e)?;
            bs.push((n, ex));
        }
        let body = self.expand_body(&items[1..], span)?;
        Ok(CoreExpr::Letrec {
            bindings: bs,
            body: Rc::new(body),
            span,
        })
    }

    fn expand_and(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        // (and) -> #t; (and e) -> e; (and e ...) -> (if e (and ...) #f)
        if items.is_empty() {
            return Ok(CoreExpr::Const {
                value: Value::Boolean(true),
                span,
            });
        }
        if items.len() == 1 {
            return self.expand(&items[0]);
        }
        let head = self.expand(&items[0])?;
        let rest = self.expand_and(&items[1..], span)?;
        Ok(CoreExpr::If {
            cond: Rc::new(head),
            then: Rc::new(rest),
            alt: Rc::new(CoreExpr::Const {
                value: Value::Boolean(false),
                span,
            }),
            span,
        })
    }

    fn expand_or(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Ok(CoreExpr::Const {
                value: Value::Boolean(false),
                span,
            });
        }
        if items.len() == 1 {
            return self.expand(&items[0]);
        }
        // (or e ...) -> (let ((t e)) (if t t (or ...))) — proper R6RS preserves value of e.
        // For simplicity in foundation we use (if e e (or ...)) which double-evals; acceptable for now.
        let head = self.expand(&items[0])?;
        let rest = self.expand_or(&items[1..], span)?;
        Ok(CoreExpr::If {
            cond: Rc::new(head.clone()),
            then: Rc::new(head),
            alt: Rc::new(rest),
            span,
        })
    }

    fn expand_when(
        &mut self,
        items: &[Datum],
        span: Span,
        invert: bool,
    ) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "when/unless needs a condition".into(),
                span,
            });
        }
        let cond = self.expand(&items[0])?;
        let body = self.expand_body(&items[1..], span)?;
        let unspec = CoreExpr::Const {
            value: Value::Unspecified,
            span,
        };
        let (then, alt) = if invert {
            (unspec, body)
        } else {
            (body, unspec)
        };
        Ok(CoreExpr::If {
            cond: Rc::new(cond),
            then: Rc::new(then),
            alt: Rc::new(alt),
            span,
        })
    }

    fn expand_cond(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        // (cond <clause>...) where each clause is one of:
        //   (else body...)             — fallback, must be last
        //   (test => consumer)         — call (consumer test-value) when truthy
        //   (test)                     — yield test-value when truthy
        //   (test body...)             — evaluate body when truthy
        let mut acc = CoreExpr::Const {
            value: Value::Unspecified,
            span,
        };
        for clause in items.iter().rev() {
            acc = self.expand_clause_with_alt(clause, acc, span, "cond")?;
        }
        Ok(acc)
    }

    /// Expand one cond/guard clause given the alternative branch (next
    /// clause's expansion) — handles `else`, `=>`, single-test, and
    /// multi-body shapes uniformly so `cond` and `guard` share the logic.
    fn expand_clause_with_alt(
        &mut self,
        clause: &Datum,
        alt: CoreExpr,
        _span: Span,
        form_name: &str,
    ) -> Result<CoreExpr, ExpandError> {
        let parts = collect_proper_list_strict(clause).ok_or(ExpandError::BadSyntax {
            what: format!("{} clause must be a proper list", form_name),
            span: clause.span(),
        })?;
        if parts.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: format!("{} clause is empty", form_name),
                span: clause.span(),
            });
        }
        let head = &parts[0];
        // else clause: collapses to its body, with alt discarded.
        if let Datum::Symbol(s, _) = head {
            if *s == self.keywords.else_ {
                return self.expand_body(&parts[1..], clause.span());
            }
        }
        let test_expr = self.expand(head)?;
        // (test => consumer)
        if parts.len() == 3 {
            if let Datum::Symbol(s, _) = &parts[1] {
                if *s == self.keywords.arrow {
                    let consumer_expr = self.expand(&parts[2])?;
                    let tmp_sym = self.syms.intern("__cond-arrow-tmp__");
                    // ((lambda (t) (if t (consumer t) <alt>)) <test>)
                    let lam_body = CoreExpr::If {
                        cond: Rc::new(CoreExpr::Ref {
                            name: tmp_sym,
                            span: clause.span(),
                        }),
                        then: Rc::new(CoreExpr::App {
                            func: Rc::new(consumer_expr),
                            args: vec![CoreExpr::Ref {
                                name: tmp_sym,
                                span: clause.span(),
                            }],
                            span: clause.span(),
                        }),
                        alt: Rc::new(alt),
                        span: clause.span(),
                    };
                    return Ok(CoreExpr::App {
                        func: Rc::new(CoreExpr::Lambda {
                            params: Params::fixed(vec![tmp_sym]),
                            body: Rc::new(lam_body),
                            span: clause.span(),
                        }),
                        args: vec![test_expr],
                        span: clause.span(),
                    });
                }
            }
        }
        // (test) — yield test value when truthy
        if parts.len() == 1 {
            let tmp_sym = self.syms.intern("__cond-test-tmp__");
            let lam_body = CoreExpr::If {
                cond: Rc::new(CoreExpr::Ref {
                    name: tmp_sym,
                    span: clause.span(),
                }),
                then: Rc::new(CoreExpr::Ref {
                    name: tmp_sym,
                    span: clause.span(),
                }),
                alt: Rc::new(alt),
                span: clause.span(),
            };
            return Ok(CoreExpr::App {
                func: Rc::new(CoreExpr::Lambda {
                    params: Params::fixed(vec![tmp_sym]),
                    body: Rc::new(lam_body),
                    span: clause.span(),
                }),
                args: vec![test_expr],
                span: clause.span(),
            });
        }
        // (test body...) — evaluate body when truthy
        let body = self.expand_body(&parts[1..], clause.span())?;
        Ok(CoreExpr::If {
            cond: Rc::new(test_expr),
            then: Rc::new(body),
            alt: Rc::new(alt),
            span: clause.span(),
        })
    }

    /// `(case key (datum-list body ...) ... (else body ...))`
    /// Desugars to a let-bound key + cond + memv chain.
    /// We model it as: `(let ((__case-key__ key)) (cond ((memv __case-key__ '(d ...)) body ...) ... (else body ...)))`
    /// without a real `let` we use a lambda-application directly.
    fn expand_case(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "case needs a key expression".into(),
                span,
            });
        }
        let key_expr = self.expand(&items[0])?;
        // Generate a unique-ish key name. Symbol re-interning is fine since it's
        // a synthetic name unlikely to collide.
        let key_sym = self.syms.intern("__case-key__");

        let mut acc = CoreExpr::Const {
            value: Value::Unspecified,
            span,
        };
        for clause in items[1..].iter().rev() {
            let (head, body_items) = collect_list(clause).ok_or(ExpandError::BadSyntax {
                what: "case clause must be a list".into(),
                span: clause.span(),
            })?;
            // R7RS `=>` arrow form: when the body starts with `=>`,
            // the second body element is a procedure that receives
            // the key as its argument. Shape: ((d ...) => proc) or
            // (else => proc).
            let arrow_form = body_items
                .first()
                .and_then(|d| match d {
                    Datum::Symbol(s, _) if *s == self.keywords.arrow => Some(()),
                    _ => None,
                })
                .is_some();
            let body = if arrow_form {
                if body_items.len() != 2 {
                    return Err(ExpandError::BadSyntax {
                        what: "case `=>` clause needs exactly one expression after =>".into(),
                        span: clause.span(),
                    });
                }
                let proc_expr = self.expand(&body_items[1])?;
                CoreExpr::App {
                    func: Rc::new(proc_expr),
                    args: vec![CoreExpr::Ref {
                        name: key_sym,
                        span: clause.span(),
                    }],
                    span: clause.span(),
                }
            } else if body_items.is_empty() {
                CoreExpr::Const {
                    value: Value::Unspecified,
                    span: clause.span(),
                }
            } else {
                self.expand_body(&body_items, clause.span())?
            };
            // else clause
            if let Datum::Symbol(s, _) = &*head {
                if *s == self.keywords.else_ {
                    acc = body;
                    continue;
                }
            }
            // Otherwise: head must be a list of datums.
            let datums = collect_proper_list_strict(&head).ok_or(ExpandError::BadSyntax {
                what: "case clause datums must be a list".into(),
                span: clause.span(),
            })?;
            let datum_list = Datum::Null(clause.span());
            let mut acc_d = datum_list;
            for d in datums.into_iter().rev() {
                acc_d = Datum::Pair(Rc::new(d.clone()), Rc::new(acc_d.clone()), clause.span());
            }
            // Build the test: `(memv __case-key__ '(d1 d2 ...))`
            let test = CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: self.syms.intern("memv"),
                    span: clause.span(),
                }),
                args: vec![
                    CoreExpr::Ref {
                        name: key_sym,
                        span: clause.span(),
                    },
                    CoreExpr::Const {
                        value: acc_d.to_value(),
                        span: clause.span(),
                    },
                ],
                span: clause.span(),
            };
            acc = CoreExpr::If {
                cond: Rc::new(test),
                then: Rc::new(body),
                alt: Rc::new(acc),
                span: clause.span(),
            };
        }
        // Wrap in a single-binding letrec (acts like let).
        Ok(CoreExpr::Letrec {
            bindings: vec![(key_sym, key_expr)],
            body: Rc::new(acc),
            span,
        })
    }

    // ---- R6RS++ §12 (#118) Iter B: syntax-case form ----
    //
    // `(syntax-case <expr> (<literal>...) (<pattern> <template>) ...)`
    //
    // Iter B implements the runtime form: matches `<expr>` against
    // each pattern in turn; on first match, evaluates the
    // corresponding template with pattern variables in scope.
    // The pattern grammar matches the syntax-rules subset (no
    // ellipsis -- Iter C):
    //   _                 wildcard, no binding
    //   <literal-sym>     matches `eq?` to <literal-sym> (must
    //                     appear in the literals list)
    //   <pvar>            binds the matched value
    //   <number|string|bool|char>  matches `equal?` to itself
    //   ()                matches null
    //   (<p> . <p>)       matches a pair
    //   (<p1> <p2> ...)   matches a proper list of that length
    //
    // Template handling: scan the template body for `(syntax T)`
    // forms and replace them with code that re-constructs T using
    // the bound pattern variables. Outside `(syntax ...)` the body
    // is ordinary Scheme code that can reference pvars by name.
    //
    // Desugars to:
    //   (let ((__sc-key__ <expr>))
    //     (cond [<test1> (let (<pvars1>) <body1>)]
    //           [<test2> (let (<pvars2>) <body2>)]
    //           ...
    //           [else (error 'syntax-case "no matching pattern" __sc-key__)]))
    //
    // The desugared datum is fed back through `self.expand` so the
    // existing `let`/`cond`/`error` handlers do the rest.
    fn expand_syntax_case(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "syntax-case needs <expr> (<literal>...) <clause>...".into(),
                span,
            });
        }
        let scrut = items[0].clone();
        let literals = collect_proper_list_strict(&items[1]).ok_or(ExpandError::BadSyntax {
            what: "syntax-case literals must be a proper list".into(),
            span: items[1].span(),
        })?;
        let mut literal_syms: Vec<Symbol> = Vec::with_capacity(literals.len());
        for l in &literals {
            match l {
                Datum::Symbol(s, _) => literal_syms.push(*s),
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "syntax-case literal must be a symbol".into(),
                        span: l.span(),
                    });
                }
            }
        }

        let key_sym = self.syms.intern("__sc-key__");
        let key_ref = Datum::Symbol(key_sym, span);

        // For each clause, build a `(test, body-with-pvar-lets)`
        // pair as Datums. Pvars are pushed onto `syntax_pvars`
        // around the body conversion so that any `(syntax X)`
        // inside the body (or nested syntax-binding forms) sees
        // them at expansion time.
        let mut cond_clauses_dat: Vec<Datum> = Vec::with_capacity(items.len() - 2 + 1);

        for clause_d in &items[2..] {
            let clause_parts =
                collect_proper_list_strict(clause_d).ok_or(ExpandError::BadSyntax {
                    what: "syntax-case clause must be (pattern template)".into(),
                    span: clause_d.span(),
                })?;
            // Allow 2-element `(pattern template)` and 3-element
            // `(pattern fender template)`. R6RS doesn't define a
            // 4+ shape.
            if clause_parts.len() != 2 && clause_parts.len() != 3 {
                return Err(ExpandError::BadSyntax {
                    what:
                        "syntax-case clause must be (pattern template) or (pattern fender template)"
                            .into(),
                    span: clause_d.span(),
                });
            }
            let pat = &clause_parts[0];
            let (fender_opt, tmpl_body) = if clause_parts.len() == 3 {
                (Some(clause_parts[1].clone()), &clause_parts[2])
            } else {
                (None, &clause_parts[1])
            };

            let mut pvars: Vec<(Symbol, u32, Datum)> = Vec::new();
            let test = compile_sc_pattern(
                pat,
                key_ref.clone(),
                &literal_syms,
                &mut pvars,
                self.syms,
                &self.keywords,
            )?;

            // Build the inner body datum: optionally wrap in
            // (if <fender> <body> __sc-try-next__), and wrap in a
            // `let` that binds pvars to their extractors. The
            // `__sc-try-next__` placeholder is replaced with a
            // 0-arity thunk call in the CoreExpr-building loop;
            // for the datum representation we use a literal
            // marker symbol to keep the per-clause shape uniform.
            let inner_body = if let Some(fender_dat) = &fender_opt {
                // (if <fender> <body> (__sc-try-next__))
                let try_next_call = mk_list(
                    vec![Datum::Symbol(
                        self.syms.intern("__sc-try-next__"),
                        clause_d.span(),
                    )],
                    clause_d.span(),
                );
                mk_list(
                    vec![
                        Datum::Symbol(self.keywords.if_, clause_d.span()),
                        fender_dat.clone(),
                        tmpl_body.clone(),
                        try_next_call,
                    ],
                    clause_d.span(),
                )
            } else {
                tmpl_body.clone()
            };
            let body_with_lets = if pvars.is_empty() {
                inner_body
            } else {
                let mut binding_list = Datum::Null(clause_d.span());
                for (pname, _depth, ext) in pvars.iter().rev() {
                    let bind_pair = mk_list(
                        vec![Datum::Symbol(*pname, clause_d.span()), ext.clone()],
                        clause_d.span(),
                    );
                    binding_list =
                        Datum::Pair(Rc::new(bind_pair), Rc::new(binding_list), clause_d.span());
                }
                mk_list(
                    vec![
                        Datum::Symbol(self.keywords.let_, clause_d.span()),
                        binding_list,
                        inner_body,
                    ],
                    clause_d.span(),
                )
            };

            // Tag fender presence into the clause datum so the
            // CoreExpr-building loop knows whether to bind a thunk.
            // 2-element clause -> (test body)
            // 3-element clause -> (test body 'has-fender)
            let cond_clause = if fender_opt.is_some() {
                mk_list(
                    vec![
                        test,
                        body_with_lets,
                        Datum::Symbol(self.syms.intern("__has-fender__"), clause_d.span()),
                    ],
                    clause_d.span(),
                )
            } else {
                mk_list(vec![test, body_with_lets], clause_d.span())
            };
            let _ = &pvars;
            cond_clauses_dat.push(cond_clause);
        }

        // Else clause: raise a syntax-case mismatch error.
        let else_kw = Datum::Symbol(self.keywords.else_, span);
        let error_call = mk_list(
            vec![
                Datum::Symbol(self.syms.intern("error"), span),
                mk_list(
                    vec![
                        Datum::Symbol(self.keywords.quote, span),
                        Datum::Symbol(self.syms.intern("syntax-case"), span),
                    ],
                    span,
                ),
                Datum::String(Rc::new("no matching pattern".to_string()), span),
                key_ref.clone(),
            ],
            span,
        );
        cond_clauses_dat.push(mk_list(vec![else_kw, error_call], span));

        // (cond clause1 ... else-clause). We can't simply call
        // self.expand on the whole assembled form because we need
        // to push per-clause pvars around the matching clause's
        // body expansion. Build a CoreExpr by hand instead.
        //
        // Each clause has `(test body)` shape. We extract per-
        // clause pvars by re-walking the original patterns; the
        // already-built Datum tree wraps body in `(let ((pv ext)
        // ...) body)`, but we still need to push pvars onto
        // `syntax_pvars` so nested `(syntax X)` sees them.
        // Phase 1.5 Iter C: each syntax-case form gets one
        // fresh mark, shared by every `(syntax T)` evaluation
        // inside any clause body during this form's runtime
        // invocation. Bound as `__sc-mark-N__` via the outer
        // Letrec; pushed onto `syntax_mark_exprs` so
        // `expand_syntax_form` can pick it up when compiling
        // template-introduced identifiers.
        self.gensym_counter += 1;
        let mark_sym = self
            .syms
            .intern(&format!("__sc-mark-{}__", self.gensym_counter));
        let mark_ref = Datum::Symbol(mark_sym, span);
        self.syntax_mark_exprs.push(mark_ref);

        let key_expr = self.expand(&scrut)?;
        // Walk clauses in reverse so the final accumulator is the
        // else fallthrough (raises a no-match error).
        let last = cond_clauses_dat.pop().expect("else clause was just pushed");
        let last_parts = collect_proper_list_strict(&last).expect("else clause is a list");
        let mut acc = self.expand(&last_parts[1])?;

        let try_next_sym = self.syms.intern("__sc-try-next__");
        for (clause_i, clause_dat) in cond_clauses_dat.iter().enumerate().rev() {
            let parts = collect_proper_list_strict(clause_dat).expect("clause is a list");
            let test_dat = &parts[0];
            let body_dat = &parts[1];
            // A 3rd element (the `__has-fender__` marker) flags a
            // fender clause. Body already encodes
            // `(if <fender> <body> __sc-try-next__)`; we just
            // need to bind __sc-try-next__ to a 0-arity thunk
            // whose body is the current acc (next-clause
            // expression), so both the test-failure path and the
            // fender-failure path call into the same shared
            // continuation without duplicating the CoreExpr tree.
            let has_fender = parts.len() >= 3;

            // Determine this clause's pvars from the original
            // pattern. (We already compiled it once and discarded
            // the names; recompile to recover them. The cost is
            // negligible -- pattern compilation is a small linear
            // walk.)
            let orig_clause_parts =
                collect_proper_list_strict(&items[2 + clause_i]).expect("orig clause");
            let mut pvars_pairs: Vec<(Symbol, u32, Datum)> = Vec::new();
            let _ = compile_sc_pattern(
                &orig_clause_parts[0],
                key_ref.clone(),
                &literal_syms,
                &mut pvars_pairs,
                self.syms,
                &self.keywords,
            )?;
            let pvar_entries: Vec<(Symbol, u32)> =
                pvars_pairs.into_iter().map(|(s, d, _)| (s, d)).collect();

            // Push, expand body, pop. The body datum already
            // wraps in a `let` that binds the pvars as Scheme
            // vars; pushing the pvar names onto `syntax_pvars`
            // ensures any `(syntax X)` inside (or in nested
            // syntax-binding forms) treats X as a pvar.
            let saved = self.syntax_pvars.len();
            self.syntax_pvars.extend(pvar_entries.iter().copied());
            let body_expr = self.expand(body_dat)?;
            self.syntax_pvars.truncate(saved);

            let test_expr = self.expand(test_dat)?;

            if has_fender {
                // Wrap so that __sc-try-next__ -> (next-clause).
                // The clause body has already been expanded
                // referencing __sc-try-next__ as a variable; we
                // wire it through a Letrec binding to a 0-arity
                // thunk whose body is `acc` (the previous
                // cond's accumulated cascade).
                let next_thunk = CoreExpr::Lambda {
                    params: Params::fixed(vec![]),
                    body: Rc::new(acc.clone()),
                    span,
                };
                let next_call = CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: try_next_sym,
                        span,
                    }),
                    args: vec![],
                    span,
                };
                let inner_if = CoreExpr::If {
                    cond: Rc::new(test_expr),
                    then: Rc::new(body_expr),
                    alt: Rc::new(next_call),
                    span,
                };
                acc = CoreExpr::Letrec {
                    bindings: vec![(try_next_sym, next_thunk)],
                    body: Rc::new(inner_if),
                    span,
                };
            } else {
                acc = CoreExpr::If {
                    cond: Rc::new(test_expr),
                    then: Rc::new(body_expr),
                    alt: Rc::new(acc),
                    span,
                };
            }
        }

        // Done with this syntax-case form's mark scope.
        self.syntax_mark_exprs.pop();

        // Outer Letrec binds both the scrutinee key AND the
        // freshly-generated mark for the form. The mark binding
        // calls `(fresh-mark!)` at form-eval time so every
        // invocation of this syntax-case gets a distinct mark
        // (the mechanism that makes two calls of the same
        // macro-defining-syntax distinguishable under
        // bound-identifier=?).
        let fresh_mark_call = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: self.syms.intern("fresh-mark!"),
                span,
            }),
            args: vec![],
            span,
        };
        Ok(CoreExpr::Letrec {
            bindings: vec![(key_sym, key_expr), (mark_sym, fresh_mark_call)],
            body: Rc::new(acc),
            span,
        })
    }

    /// Standalone `(syntax T)` outside of a syntax-case body. Iter
    /// B treats this as `(quote T)` -- there are no pvars in scope
    /// to substitute. Iter C extends this when used inside macro
    /// transformers (with-syntax / quasisyntax) where pvars do
    /// exist.
    fn expand_syntax_form(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 1 {
            return Err(ExpandError::BadSyntax {
                what: "syntax takes exactly 1 argument".into(),
                span,
            });
        }
        // Consult `self.syntax_pvars` for currently-bound pattern
        // variables. `compile_syntax_template` turns pvar symbols
        // into variable references and everything else (literal
        // identifiers, atoms, sub-lists) into self-quoting /
        // cons-constructed values. Standalone `(syntax T)` outside
        // any syntax-binding scope therefore lowers to roughly
        // `(quote T)` -- but inside a syntax-case or with-syntax
        // body it transparently substitutes the bound pvars.
        let pvars_snapshot: Vec<(Symbol, u32)> = self.syntax_pvars.clone();
        // Top-of-stack mark expression for this `(syntax T)` form:
        // when inside a syntax-case body, that's the body's
        // `__sc-mark-N__` variable reference; standalone use
        // gets the literal 0 (unmarked identifier).
        let mark_expr = self
            .syntax_mark_exprs
            .last()
            .cloned()
            .unwrap_or_else(|| Datum::Number(cs_core::Number::Fixnum(0), span));
        let compiled = compile_syntax_template(
            &items[0],
            &pvars_snapshot,
            self.syms,
            &self.keywords,
            &mark_expr,
        );
        self.expand(&compiled)
    }

    // ---- R6RS++ §12 (#118) Iter C: with-syntax + quasisyntax ----

    /// `(with-syntax ((pat val) ...) body ...)`
    ///
    /// Pattern-binds each `val` against the corresponding `pat`
    /// (using the same pattern grammar as syntax-case) and
    /// evaluates `body ...` with the pvars in scope. Desugars to
    /// a nest of single-clause `syntax-case` forms; the final
    /// innermost body becomes `(let () body ...)` so `body` can be
    /// a sequence with internal defines.
    fn expand_with_syntax(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "with-syntax needs ((pat val) ...) body ...".into(),
                span,
            });
        }
        let bindings_datum = &items[0];
        let body_datums = &items[1..];

        let bindings =
            collect_proper_list_strict(bindings_datum).ok_or(ExpandError::BadSyntax {
                what: "with-syntax bindings must be a proper list".into(),
                span: bindings_datum.span(),
            })?;
        // Sanity-check each binding shape up front.
        let mut pat_val: Vec<(Datum, Datum)> = Vec::with_capacity(bindings.len());
        for b in &bindings {
            let parts = collect_proper_list_strict(b).ok_or(ExpandError::BadSyntax {
                what: "with-syntax binding must be (pattern value)".into(),
                span: b.span(),
            })?;
            if parts.len() != 2 {
                return Err(ExpandError::BadSyntax {
                    what: "with-syntax binding must be (pattern value)".into(),
                    span: b.span(),
                });
            }
            pat_val.push((parts[0].clone(), parts[1].clone()));
        }

        // Innermost form: (let () body ...) so multi-form bodies
        // sequence cleanly and support internal defines.
        let mut inner: Datum = {
            let mut all = vec![Datum::Symbol(self.keywords.let_, span), Datum::Null(span)];
            for d in body_datums {
                all.push(d.clone());
            }
            mk_list(all, span)
        };

        // Build the nested syntax-case bottom-up: innermost form
        // is `inner`; each outer layer wraps it with another
        // single-clause syntax-case binding the next pvar.
        for (pat, val) in pat_val.into_iter().rev() {
            let clause = mk_list(vec![pat, inner.clone()], span);
            inner = mk_list(
                vec![
                    Datum::Symbol(self.keywords.syntax_case, span),
                    val,
                    Datum::Null(span),
                    clause,
                ],
                span,
            );
        }
        self.expand(&inner)
    }

    /// `(quasisyntax T)` -- like `quasiquote` but `#,e` / `#,@e`
    /// (`unsyntax e` / `unsyntax-splicing e`) interpolate
    /// evaluated expressions. With today's syntax-object-as-datum
    /// model the semantics match `quasiquote` exactly, so we
    /// rewrite the template by swapping the qs/us/uss head
    /// symbols for the corresponding quasiquote/unquote/
    /// unquote-splicing symbols and delegate. Future iters
    /// distinguish syntax-object marking; the rewrite hook
    /// stays the same.
    fn expand_quasisyntax(
        &mut self,
        template: &Datum,
        depth: u32,
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        let rewritten = rewrite_qs_to_qq(template, &self.keywords);
        self.expand_quasiquote(&rewritten, depth, span)
    }

    /// `(do ((var init step) ...) (test result ...) body ...)`
    /// Desugars to a named let:
    ///   (letrec ((__do-loop__ (lambda (var ...)
    ///     (if test
    ///         (begin result ...)
    ///         (begin body ... (__do-loop__ step ...))))))
    ///     (__do-loop__ init ...))
    fn expand_do(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "do needs (bindings) (test result...) body...".into(),
                span,
            });
        }
        let bindings_datum = &items[0];
        let test_datum = &items[1];
        let body_datums = &items[2..];

        let bindings = match bindings_datum {
            Datum::Null(_) => Vec::new(),
            Datum::Pair(_, _, _) => {
                collect_proper_list_strict(bindings_datum).ok_or(ExpandError::BadSyntax {
                    what: "do: bindings must be a proper list".into(),
                    span: bindings_datum.span(),
                })?
            }
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "do: bindings must be a list".into(),
                    span: bindings_datum.span(),
                });
            }
        };
        let mut vars: Vec<cs_core::Symbol> = Vec::with_capacity(bindings.len());
        let mut inits: Vec<CoreExpr> = Vec::with_capacity(bindings.len());
        let mut steps: Vec<CoreExpr> = Vec::with_capacity(bindings.len());
        for b in &bindings {
            let parts = collect_proper_list_strict(b).ok_or(ExpandError::BadSyntax {
                what: "do binding must be (var init [step])".into(),
                span: b.span(),
            })?;
            if parts.len() < 2 || parts.len() > 3 {
                return Err(ExpandError::BadSyntax {
                    what: "do binding must be (var init [step])".into(),
                    span: b.span(),
                });
            }
            let var = match &parts[0] {
                Datum::Symbol(s, _) => *s,
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "do: binding name must be a symbol".into(),
                        span: parts[0].span(),
                    });
                }
            };
            vars.push(var);
            inits.push(self.expand(&parts[1])?);
            let step_expr = if parts.len() == 3 {
                self.expand(&parts[2])?
            } else {
                CoreExpr::Ref {
                    name: var,
                    span: b.span(),
                }
            };
            steps.push(step_expr);
        }

        // Test/result clause
        let test_parts = collect_proper_list_strict(test_datum).ok_or(ExpandError::BadSyntax {
            what: "do: (test result...) clause".into(),
            span: test_datum.span(),
        })?;
        if test_parts.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "do: empty test clause".into(),
                span: test_datum.span(),
            });
        }
        let test_expr = self.expand(&test_parts[0])?;
        let result_expr = if test_parts.len() <= 1 {
            CoreExpr::Const {
                value: Value::Unspecified,
                span: test_datum.span(),
            }
        } else if test_parts.len() == 2 {
            self.expand(&test_parts[1])?
        } else {
            let mut exprs = Vec::with_capacity(test_parts.len() - 1);
            for d in &test_parts[1..] {
                exprs.push(self.expand(d)?);
            }
            CoreExpr::Begin {
                exprs,
                span: test_datum.span(),
            }
        };

        // Body
        let mut body_exprs: Vec<CoreExpr> = Vec::with_capacity(body_datums.len());
        for d in body_datums {
            body_exprs.push(self.expand(d)?);
        }

        let loop_sym = self.syms.intern("__do-loop__");
        // Build the recursive call: (__do-loop__ step1 step2 ...)
        let recursive_call = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: loop_sym,
                span,
            }),
            args: steps.clone(),
            span,
        };
        // Build the alt branch: (begin body ... (__do-loop__ step ...))
        let alt_body = if body_exprs.is_empty() {
            recursive_call
        } else {
            let mut all = body_exprs.clone();
            all.push(recursive_call);
            CoreExpr::Begin { exprs: all, span }
        };
        // Build the if
        let lam_body = CoreExpr::If {
            cond: Rc::new(test_expr),
            then: Rc::new(result_expr),
            alt: Rc::new(alt_body),
            span,
        };
        // Build the lambda (var ...) lam_body
        let loop_lambda = CoreExpr::Lambda {
            params: Params::fixed(vars.clone()),
            body: Rc::new(lam_body),
            span,
        };
        // Build the letrec ((__do-loop__ loop_lambda)) (__do-loop__ init ...)
        let initial_call = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: loop_sym,
                span,
            }),
            args: inits,
            span,
        };
        Ok(CoreExpr::Letrec {
            bindings: vec![(loop_sym, loop_lambda)],
            body: Rc::new(initial_call),
            span,
        })
    }

    /// `(guard (var clause ...) body ...)`
    /// Desugars to:
    ///   (with-exception-handler
    ///     (lambda (var) (cond clause ...))   ; if no else, re-raise at end
    ///     (lambda () body ...))
    fn expand_guard(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "guard needs (var clauses...) body...".into(),
                span,
            });
        }
        let head = &items[0];
        let body_datums = &items[1..];

        let head_parts = collect_proper_list_strict(head).ok_or(ExpandError::BadSyntax {
            what: "guard: (var clauses...) clause".into(),
            span: head.span(),
        })?;
        if head_parts.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "guard: missing condition variable".into(),
                span: head.span(),
            });
        }
        let cond_var = match &head_parts[0] {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "guard: variable must be a symbol".into(),
                    span: head_parts[0].span(),
                });
            }
        };
        let cond_clauses = &head_parts[1..];

        // Detect whether an `else` clause exists.
        let has_else = cond_clauses.iter().any(|c| {
            if let Some((head, _)) = collect_list(c) {
                if let Datum::Symbol(s, _) = &*head {
                    return *s == self.keywords.else_;
                }
            }
            false
        });

        // Build the cond — if no else, append a catchall that re-raises.
        let raise_sym = self.syms.intern("raise");
        let mut acc = if has_else {
            CoreExpr::Const {
                value: Value::Unspecified,
                span,
            }
        } else {
            // (raise cond_var)
            CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: raise_sym,
                    span,
                }),
                args: vec![CoreExpr::Ref {
                    name: cond_var,
                    span,
                }],
                span,
            }
        };
        for clause in cond_clauses.iter().rev() {
            acc = self.expand_clause_with_alt(clause, acc, span, "guard")?;
        }
        let handler_lambda = CoreExpr::Lambda {
            params: Params::fixed(vec![cond_var]),
            body: Rc::new(acc),
            span,
        };

        // Build (lambda () body ...)
        let body_expr = if body_datums.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "guard: empty body".into(),
                span,
            });
        } else if body_datums.len() == 1 {
            self.expand(&body_datums[0])?
        } else {
            let mut exprs = Vec::with_capacity(body_datums.len());
            for d in body_datums {
                exprs.push(self.expand(d)?);
            }
            CoreExpr::Begin { exprs, span }
        };
        let thunk_lambda = CoreExpr::Lambda {
            params: Params::fixed(Vec::new()),
            body: Rc::new(body_expr),
            span,
        };

        let weh = self.syms.intern("with-exception-handler");
        Ok(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref { name: weh, span }),
            args: vec![handler_lambda, thunk_lambda],
            span,
        })
    }

    /// `(let-values (((var ...) expr) ...) body ...)` desugars to nested
    /// `call-with-values` invocations.
    /// `(let*-values ...)` is the same but each binding sees the previous.
    fn expand_let_values(
        &mut self,
        items: &[Datum],
        span: Span,
        sequential: bool,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "let-values needs bindings + body".into(),
                span,
            });
        }
        let bindings_datum = &items[0];
        let body_datums = &items[1..];

        // Parse bindings: each is ((var ...) expr)
        let bindings = match bindings_datum {
            Datum::Null(_) => Vec::new(),
            _ => collect_proper_list_strict(bindings_datum).ok_or(ExpandError::BadSyntax {
                what: "let-values bindings must be a proper list".into(),
                span: bindings_datum.span(),
            })?,
        };

        struct Binding {
            vars: Vec<cs_core::Symbol>,
            expr: Datum,
        }
        let mut parsed: Vec<Binding> = Vec::new();
        for b in &bindings {
            let parts = collect_proper_list_strict(b).ok_or(ExpandError::BadSyntax {
                what: "let-values binding must be ((var ...) expr)".into(),
                span: b.span(),
            })?;
            if parts.len() != 2 {
                return Err(ExpandError::BadSyntax {
                    what: "let-values binding must be ((var ...) expr)".into(),
                    span: b.span(),
                });
            }
            let var_list = match &parts[0] {
                Datum::Null(_) => Vec::new(),
                Datum::Pair(_, _, _) => {
                    collect_proper_list_strict(&parts[0]).ok_or(ExpandError::BadSyntax {
                        what: "let-values: variable list must be proper".into(),
                        span: parts[0].span(),
                    })?
                }
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "let-values: variable list must be a list".into(),
                        span: parts[0].span(),
                    });
                }
            };
            let mut vars = Vec::with_capacity(var_list.len());
            for v in var_list {
                match v {
                    Datum::Symbol(s, _) => vars.push(s),
                    other => {
                        return Err(ExpandError::BadSyntax {
                            what: "let-values: variable must be a symbol".into(),
                            span: other.span(),
                        });
                    }
                }
            }
            parsed.push(Binding {
                vars,
                expr: parts[1].clone(),
            });
        }

        // Build the body
        let body_expr = if body_datums.len() == 1 {
            self.expand(&body_datums[0])?
        } else {
            let mut exprs = Vec::with_capacity(body_datums.len());
            for d in body_datums {
                exprs.push(self.expand(d)?);
            }
            CoreExpr::Begin { exprs, span }
        };

        // For let-values: nest call-with-values from outermost to innermost
        // such that all bindings come from the same scope simultaneously.
        // We achieve that by nesting; for let* the same but each sees prior.
        // Both produce the same shape since Scheme nesting already provides
        // sequential scoping; the difference is whether bindings can see
        // each other's expr (let* lets later expr refer to earlier var).
        // For our simplified expander, the shapes are identical — what
        // differs is just where the body's enclosing scope starts.
        let _ = sequential;

        let cwv = self.syms.intern("call-with-values");
        let mut acc = body_expr;
        for binding in parsed.into_iter().rev() {
            // (call-with-values (lambda () expr) (lambda (vars...) acc))
            let producer_expr = self.expand(&binding.expr)?;
            let producer_lambda = CoreExpr::Lambda {
                params: Params::fixed(Vec::new()),
                body: Rc::new(producer_expr),
                span,
            };
            let consumer_lambda = CoreExpr::Lambda {
                params: Params::fixed(binding.vars),
                body: Rc::new(acc),
                span,
            };
            acc = CoreExpr::App {
                func: Rc::new(CoreExpr::Ref { name: cwv, span }),
                args: vec![producer_lambda, consumer_lambda],
                span,
            };
        }
        Ok(acc)
    }

    /// Expand `(quasiquote template)` at the given nesting depth.
    /// At depth 1, `(unquote x)` evaluates `x`; `(unquote-splicing x)` splices.
    /// Nested quasiquotes increase depth; nested unquotes decrease it.
    fn expand_quasiquote(
        &mut self,
        template: &Datum,
        depth: u32,
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        // Base case at depth 0 shouldn't happen; reaching depth 0 means the
        // unquote already consumed it.
        debug_assert!(depth >= 1);
        match template {
            // Atoms quote to themselves.
            Datum::Boolean(_, _)
            | Datum::Number(_, _)
            | Datum::Character(_, _)
            | Datum::String(_, _)
            | Datum::Symbol(_, _)
            | Datum::Null(_) => Ok(CoreExpr::Const {
                value: template.to_value(),
                span: template.span(),
            }),
            Datum::Pair(_, _, pair_span) => {
                // Recognize (unquote x) and (unquote-splicing x) and (quasiquote x).
                if let Some((head, tail)) = collect_list(template) {
                    if let Datum::Symbol(s, _) = &*head {
                        if *s == self.keywords.unquote && tail.len() == 1 {
                            if depth == 1 {
                                return self.expand(&tail[0]);
                            }
                            // Deeper: reconstruct as `(list 'unquote ...)`.
                            let unquote_sym = self.keywords.unquote;
                            let inner = self.expand_quasiquote(&tail[0], depth - 1, span)?;
                            return Ok(self.qq_list_call(
                                vec![
                                    CoreExpr::Const {
                                        value: Value::Symbol(unquote_sym),
                                        span,
                                    },
                                    inner,
                                ],
                                span,
                            ));
                        }
                        if *s == self.keywords.quasiquote && tail.len() == 1 {
                            let qq_sym = self.keywords.quasiquote;
                            let inner = self.expand_quasiquote(&tail[0], depth + 1, span)?;
                            return Ok(self.qq_list_call(
                                vec![
                                    CoreExpr::Const {
                                        value: Value::Symbol(qq_sym),
                                        span,
                                    },
                                    inner,
                                ],
                                span,
                            ));
                        }
                    }
                }
                // Otherwise: walk the list and build (cons elem rest)
                // honoring unquote-splicing for spliced positions.
                self.qq_walk_pair(template, depth, *pair_span)
            }
            Datum::Vector(items, vspan) => {
                // Build (list->vector (... walked list ...))
                let mut acc = CoreExpr::Const {
                    value: Value::Null,
                    span: *vspan,
                };
                for item in items.iter().rev() {
                    acc = self.qq_combine_one(item, depth, acc, *vspan)?;
                }
                let l2v = self.syms.intern("list->vector");
                Ok(CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: l2v,
                        span: *vspan,
                    }),
                    args: vec![acc],
                    span: *vspan,
                })
            }
            Datum::ByteVector(_, vspan) => Ok(CoreExpr::Const {
                value: template.to_value(),
                span: *vspan,
            }),
        }
    }

    /// Walk a (possibly improper) list-shaped Datum, building a chain of
    /// cons/append calls at runtime.
    fn qq_walk_pair(
        &mut self,
        template: &Datum,
        depth: u32,
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        // Walk linearly. Track the tail so we can handle dotted pairs.
        let mut elements: Vec<Datum> = Vec::new();
        let mut tail: Datum = template.clone();
        loop {
            match tail.clone() {
                Datum::Pair(car, cdr, _) => {
                    elements.push((*car).clone());
                    tail = (*cdr).clone();
                }
                Datum::Null(_) => break,
                _ => break, // dotted tail: 'tail' is the cdr of the improper list
            }
        }

        // Build from right-to-left.
        let mut acc = match tail {
            Datum::Null(s) => CoreExpr::Const {
                value: Value::Null,
                span: s,
            },
            other => self.expand_quasiquote(&other, depth, span)?,
        };
        for elem in elements.into_iter().rev() {
            acc = self.qq_combine_one(&elem, depth, acc, span)?;
        }
        Ok(acc)
    }

    /// Given a single template element and an "accumulator" expression
    /// (representing the tail), produce a new expression with the element
    /// prepended (cons or append for unquote-splicing).
    fn qq_combine_one(
        &mut self,
        elem: &Datum,
        depth: u32,
        rest: CoreExpr,
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        // Detect `(unquote-splicing x)` at depth 1 → use append.
        if depth == 1 {
            if let Some((head, tail)) = collect_list(elem) {
                if let Datum::Symbol(s, _) = &*head {
                    if *s == self.keywords.unquote_splicing && tail.len() == 1 {
                        let spliced = self.expand(&tail[0])?;
                        let append_sym = self.syms.intern("append");
                        return Ok(CoreExpr::App {
                            func: Rc::new(CoreExpr::Ref {
                                name: append_sym,
                                span,
                            }),
                            args: vec![spliced, rest],
                            span,
                        });
                    }
                }
            }
        }
        // Default: cons element onto rest.
        let elem_expr = self.expand_quasiquote(elem, depth, span)?;
        let cons_sym = self.syms.intern("cons");
        Ok(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cons_sym,
                span,
            }),
            args: vec![elem_expr, rest],
            span,
        })
    }

    fn qq_list_call(&mut self, args: Vec<CoreExpr>, span: Span) -> CoreExpr {
        let list_sym = self.syms.intern("list");
        CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: list_sym,
                span,
            }),
            args,
            span,
        }
    }

    /// `(parameterize ((p1 v1) (p2 v2) ...) body ...)` desugars to:
    ///   (let ((old1 (p1)) (old2 (p2)) ... (new1 v1) (new2 v2) ...)
    ///     (dynamic-wind
    ///       (lambda () (p1 new1) (p2 new2) ...)
    ///       (lambda () body ...)
    ///       (lambda () (p1 old1) (p2 old2) ...)))
    fn expand_parameterize(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "parameterize needs ((param value)...) body...".into(),
                span,
            });
        }
        let bindings_datum = &items[0];
        let body_datums = &items[1..];

        let bindings = match bindings_datum {
            Datum::Null(_) => Vec::new(),
            _ => collect_proper_list_strict(bindings_datum).ok_or(ExpandError::BadSyntax {
                what: "parameterize bindings must be a proper list".into(),
                span: bindings_datum.span(),
            })?,
        };

        struct Binding {
            param_expr: CoreExpr,
            new_expr: CoreExpr,
            param_var: cs_core::Symbol,
            new_var: cs_core::Symbol,
            old_var: cs_core::Symbol,
        }
        let mut parsed: Vec<Binding> = Vec::new();
        for (i, b) in bindings.iter().enumerate() {
            let parts = collect_proper_list_strict(b).ok_or(ExpandError::BadSyntax {
                what: "parameterize binding must be (param value)".into(),
                span: b.span(),
            })?;
            if parts.len() != 2 {
                return Err(ExpandError::BadSyntax {
                    what: "parameterize binding must be (param value)".into(),
                    span: b.span(),
                });
            }
            let param_expr = self.expand(&parts[0])?;
            let new_expr = self.expand(&parts[1])?;
            let param_var = self.syms.intern(&format!("__param-{}__", i));
            let new_var = self.syms.intern(&format!("__new-{}__", i));
            let old_var = self.syms.intern(&format!("__old-{}__", i));
            parsed.push(Binding {
                param_expr,
                new_expr,
                param_var,
                new_var,
                old_var,
            });
        }

        // Body
        let body_expr = if body_datums.len() == 1 {
            self.expand(&body_datums[0])?
        } else {
            let mut exprs = Vec::with_capacity(body_datums.len());
            for d in body_datums {
                exprs.push(self.expand(d)?);
            }
            CoreExpr::Begin { exprs, span }
        };

        // Build (lambda () (p1 new1) (p2 new2) ... ) — set new values
        let mut before_calls: Vec<CoreExpr> = Vec::new();
        for b in &parsed {
            before_calls.push(CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: b.param_var,
                    span,
                }),
                args: vec![CoreExpr::Ref {
                    name: b.new_var,
                    span,
                }],
                span,
            });
        }
        let before_body = if before_calls.len() == 1 {
            before_calls.pop().unwrap()
        } else {
            CoreExpr::Begin {
                exprs: before_calls,
                span,
            }
        };
        let before_lambda = CoreExpr::Lambda {
            params: Params::fixed(Vec::new()),
            body: Rc::new(before_body),
            span,
        };

        // (lambda () (p1 old1) (p2 old2) ...) — restore
        let mut after_calls: Vec<CoreExpr> = Vec::new();
        for b in &parsed {
            after_calls.push(CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: b.param_var,
                    span,
                }),
                args: vec![CoreExpr::Ref {
                    name: b.old_var,
                    span,
                }],
                span,
            });
        }
        let after_body = if after_calls.is_empty() {
            CoreExpr::Const {
                value: Value::Unspecified,
                span,
            }
        } else if after_calls.len() == 1 {
            after_calls.pop().unwrap()
        } else {
            CoreExpr::Begin {
                exprs: after_calls,
                span,
            }
        };
        let after_lambda = CoreExpr::Lambda {
            params: Params::fixed(Vec::new()),
            body: Rc::new(after_body),
            span,
        };

        // Body wrapped in a thunk
        let thunk_lambda = CoreExpr::Lambda {
            params: Params::fixed(Vec::new()),
            body: Rc::new(body_expr),
            span,
        };

        // (dynamic-wind before thunk after)
        let dw_sym = self.syms.intern("dynamic-wind");
        let dw_call = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref { name: dw_sym, span }),
            args: vec![before_lambda, thunk_lambda, after_lambda],
            span,
        };

        // Outer letrec binds: param-expr → param_var, new-expr → new_var, (param) → old_var
        let mut bindings_out: Vec<(cs_core::Symbol, CoreExpr)> = Vec::new();
        for b in &parsed {
            bindings_out.push((b.param_var, b.param_expr.clone()));
            bindings_out.push((b.new_var, b.new_expr.clone()));
            // old_var = (param)
            bindings_out.push((
                b.old_var,
                CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: b.param_var,
                        span,
                    }),
                    args: vec![],
                    span,
                },
            ));
        }
        Ok(CoreExpr::Letrec {
            bindings: bindings_out,
            body: Rc::new(dw_call),
            span,
        })
    }

    /// `(delay expr)` desugars to `(make-promise (lambda () expr))`.
    /// Builtin features advertised by `cond-expand`.
    fn supported_features(&self) -> &'static [&'static str] {
        &["crabscheme", "r6rs-subset", "r7rs-subset", "exact-closed"]
    }

    /// Evaluate a `cond-expand` feature requirement at expansion time.
    fn cond_expand_match(&self, req: &Datum) -> bool {
        match req {
            Datum::Symbol(s, _) => {
                let name = self.syms.name(*s);
                if name == "else" {
                    return true;
                }
                self.supported_features().contains(&name)
            }
            Datum::Pair(_, _, _) => {
                let parts = match collect_proper_list_strict(req) {
                    Some(p) => p,
                    None => return false,
                };
                if parts.is_empty() {
                    return false;
                }
                let head = match &parts[0] {
                    Datum::Symbol(s, _) => *s,
                    _ => return false,
                };
                let head_name = self.syms.name(head);
                match head_name {
                    "and" => parts[1..].iter().all(|r| self.cond_expand_match(r)),
                    "or" => parts[1..].iter().any(|r| self.cond_expand_match(r)),
                    "not" => parts.len() == 2 && !self.cond_expand_match(&parts[1]),
                    "library" => {
                        // R7RS: (library <name>) tests whether <name> is a
                        // currently-known library. We consult the registry
                        // populated as `define-library` / `library` forms
                        // are expanded, plus a small set of bundled R7RS
                        // names that map to no-op stubs.
                        if parts.len() != 2 {
                            return false;
                        }
                        let name_parts = match collect_proper_list_strict(&parts[1]) {
                            Some(p) => p,
                            None => return false,
                        };
                        let name_syms: Vec<Symbol> = match name_parts
                            .iter()
                            .map(|d| match d {
                                Datum::Symbol(s, _) => Some(*s),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>()
                        {
                            Some(v) => v,
                            None => return false,
                        };
                        if self.libraries.contains_key(&name_syms) {
                            return true;
                        }
                        // R7RS bundled libraries: (scheme base) is always
                        // provided since we install its bindings at the
                        // top-level. Match common stdlib names so user code
                        // using (cond-expand ((library (scheme base)) ...))
                        // doesn't take the false branch.
                        let names: Vec<&str> =
                            name_syms.iter().map(|s| self.syms.name(*s)).collect();
                        matches!(
                            names.as_slice(),
                            ["scheme", "base"]
                                | ["scheme", "char"]
                                | ["scheme", "complex"]
                                | ["scheme", "cxr"]
                                | ["scheme", "eval"]
                                | ["scheme", "file"]
                                | ["scheme", "inexact"]
                                | ["scheme", "lazy"]
                                | ["scheme", "load"]
                                | ["scheme", "process-context"]
                                | ["scheme", "read"]
                                | ["scheme", "repl"]
                                | ["scheme", "time"]
                                | ["scheme", "write"]
                                | ["scheme", "r5rs"]
                        )
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// `(cond-expand (<req> <body>...) (<req> <body>...) ... (else <body>...))`
    /// Picks the first clause whose feature requirement is satisfied and
    /// inlines its body as a (begin ...). Always selects exactly one
    /// clause; if none match and there's no else, raises a syntax error.
    fn expand_cond_expand(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        for clause in items {
            let parts = collect_proper_list_strict(clause).ok_or(ExpandError::BadSyntax {
                what: "cond-expand clause must be a list".into(),
                span: clause.span(),
            })?;
            if parts.is_empty() {
                return Err(ExpandError::BadSyntax {
                    what: "cond-expand clause needs a feature requirement".into(),
                    span: clause.span(),
                });
            }
            let req = &parts[0];
            if self.cond_expand_match(req) {
                let body = &parts[1..];
                if body.is_empty() {
                    return Ok(CoreExpr::Const {
                        value: Value::Unspecified,
                        span: clause.span(),
                    });
                }
                return self.expand_body(body, clause.span());
            }
        }
        Err(ExpandError::BadSyntax {
            what: "cond-expand: no matching clause".into(),
            span,
        })
    }

    /// `(case-lambda (formals1 body1) (formals2 body2) ...)` — arity-
    /// dispatched procedure. Lowered to a single rest-arg lambda that
    /// inspects (length args) and re-applies the matching clause.
    /// R6RS: each formals can be a fixed-arity list or a rest-arg pattern.
    fn expand_case_lambda(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "case-lambda needs at least one clause".into(),
                span,
            });
        }
        // The dispatch arg ("args") binds to the rest-list.
        let args_sym = self.syms.intern("__case-lambda-args__");
        let length_sym = self.syms.intern("length");
        let apply_sym = self.syms.intern("apply");
        let eq_sym = self.syms.intern("=");
        let ge_sym = self.syms.intern(">=");
        let error_sym = self.syms.intern("error");

        let mut acc = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: error_sym,
                span,
            }),
            args: vec![CoreExpr::Const {
                value: Value::string("case-lambda: no matching arity"),
                span,
            }],
            span,
        };
        for clause in items.iter().rev() {
            let parts = collect_proper_list_strict(clause).ok_or(ExpandError::BadSyntax {
                what: "case-lambda clause must be (formals body...)".into(),
                span: clause.span(),
            })?;
            if parts.is_empty() {
                return Err(ExpandError::BadSyntax {
                    what: "case-lambda clause needs formals + body".into(),
                    span: clause.span(),
                });
            }
            let formals = &parts[0];
            let body_items = &parts[1..];
            // Parse the formals into (fixed-names, rest-name, has-rest).
            let (params, has_rest) = parse_case_lambda_formals(formals)?;
            // Build the inner lambda for this clause.
            let body = self.expand_body(body_items, clause.span())?;
            let inner_lam = CoreExpr::Lambda {
                params: params.clone(),
                body: Rc::new(body),
                span: clause.span(),
            };
            // Build the dispatch test based on arity.
            let n_fixed = params.fixed.len() as i64;
            let arity_test = if has_rest {
                // (>= (length args) n_fixed)
                CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: ge_sym,
                        span: clause.span(),
                    }),
                    args: vec![
                        CoreExpr::App {
                            func: Rc::new(CoreExpr::Ref {
                                name: length_sym,
                                span: clause.span(),
                            }),
                            args: vec![CoreExpr::Ref {
                                name: args_sym,
                                span: clause.span(),
                            }],
                            span: clause.span(),
                        },
                        CoreExpr::Const {
                            value: Value::fixnum(n_fixed),
                            span: clause.span(),
                        },
                    ],
                    span: clause.span(),
                }
            } else {
                // (= (length args) n_fixed)
                CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: eq_sym,
                        span: clause.span(),
                    }),
                    args: vec![
                        CoreExpr::App {
                            func: Rc::new(CoreExpr::Ref {
                                name: length_sym,
                                span: clause.span(),
                            }),
                            args: vec![CoreExpr::Ref {
                                name: args_sym,
                                span: clause.span(),
                            }],
                            span: clause.span(),
                        },
                        CoreExpr::Const {
                            value: Value::fixnum(n_fixed),
                            span: clause.span(),
                        },
                    ],
                    span: clause.span(),
                }
            };
            // (apply <inner-lam> args)
            let apply_call = CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: apply_sym,
                    span: clause.span(),
                }),
                args: vec![
                    inner_lam,
                    CoreExpr::Ref {
                        name: args_sym,
                        span: clause.span(),
                    },
                ],
                span: clause.span(),
            };
            acc = CoreExpr::If {
                cond: Rc::new(arity_test),
                then: Rc::new(apply_call),
                alt: Rc::new(acc),
                span: clause.span(),
            };
        }
        // Outer lambda with rest-arg.
        Ok(CoreExpr::Lambda {
            params: Params {
                fixed: Vec::new(),
                rest: Some(args_sym),
            },
            body: Rc::new(acc),
            span,
        })
    }

    /// `(assert <expr>)` — evaluates `<expr>`; if truthy, yields unspecified;
    /// otherwise raises an `&assertion` condition (per R6RS). The condition
    /// also carries a `&who` of `'assert` and a `&message` containing the
    /// source form of the failed expression so handlers can identify it.
    fn expand_assert(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 1 {
            return Err(ExpandError::BadSyntax {
                what: "assert needs exactly one expression".into(),
                span,
            });
        }
        // Render the offending datum BEFORE expanding so symbols print by
        // their original name (the expander may rename them via hygiene).
        let datum_src = items[0].format_with(self.syms);
        let test = self.expand(&items[0])?;
        let err_msg = format!("assertion failed: {}", datum_src);
        let assert_who_sym = self.syms.intern("assert");
        let av_sym = self.syms.intern("assertion-violation");
        let error_call = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref { name: av_sym, span }),
            args: vec![
                CoreExpr::Const {
                    value: Value::Symbol(assert_who_sym),
                    span,
                },
                CoreExpr::Const {
                    value: Value::string(err_msg),
                    span,
                },
            ],
            span,
        };
        Ok(CoreExpr::If {
            cond: Rc::new(test),
            then: Rc::new(CoreExpr::Const {
                value: Value::Unspecified,
                span,
            }),
            alt: Rc::new(error_call),
            span,
        })
    }

    /// R6RS `(endianness <symbol>)` macro. Expands to a quoted symbol; only
    /// the literal identifiers `big` and `little` are accepted (anything
    /// else is a syntax error). The result is consumed by the typed
    /// bytevector accessors in (rnrs bytevectors).
    fn expand_endianness(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 1 {
            return Err(ExpandError::BadSyntax {
                what: "endianness expects exactly one identifier".into(),
                span,
            });
        }
        match &items[0] {
            Datum::Symbol(s, _) => {
                let name = self.syms.name(*s);
                if name == "big" || name == "little" {
                    Ok(CoreExpr::Const {
                        value: Value::Symbol(*s),
                        span,
                    })
                } else {
                    Err(ExpandError::BadSyntax {
                        what: format!("endianness: unknown endianness '{}", name),
                        span,
                    })
                }
            }
            _ => Err(ExpandError::BadSyntax {
                what: "endianness: expected an identifier (big or little)".into(),
                span,
            }),
        }
    }

    fn expand_syntax_error(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "syntax-error needs at least a message".into(),
                span,
            });
        }
        let message: String = match &items[0] {
            Datum::String(s, _) => s.as_ref().clone(),
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "syntax-error: first arg must be a string literal".into(),
                    span,
                });
            }
        };
        // Build a "<message> [<irritant1> <irritant2> ...]" string for the
        // diagnostic. Irritants render via Datum's Display impl.
        let mut full = message;
        if items.len() > 1 {
            full.push(':');
            for ir in &items[1..] {
                full.push(' ');
                full.push_str(&format!("{:?}", ir));
            }
        }
        Err(ExpandError::BadSyntax { what: full, span })
    }

    fn expand_delay(&mut self, items: &[Datum], span: Span) -> Result<CoreExpr, ExpandError> {
        if items.len() != 1 {
            return Err(ExpandError::BadSyntax {
                what: "delay needs exactly one expression".into(),
                span,
            });
        }
        let body = self.expand(&items[0])?;
        let thunk = CoreExpr::Lambda {
            params: Params::fixed(Vec::new()),
            body: Rc::new(body),
            span,
        };
        // Use the internal Pending-wrapping constructor so we don't clash
        // with R7RS make-promise (which takes a value, not a thunk).
        let make_pending = self.syms.intern("__make-pending-promise");
        Ok(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: make_pending,
                span,
            }),
            args: vec![thunk],
            span,
        })
    }

    /// Build a proper-list `Datum::Pair` chain from a Vec of items,
    /// terminated by `Datum::Null(span)`. Used by the R7RS DRT
    /// transformation to synthesize R6RS-shaped clauses.
    fn datum_list(items: Vec<Datum>, span: Span) -> Datum {
        let mut acc = Datum::Null(span);
        for item in items.into_iter().rev() {
            acc = Datum::Pair(Rc::new(item), Rc::new(acc), span);
        }
        acc
    }

    /// R7RS shape — transforms to R6RS shape and falls through to
    /// the main expander. R7RS:
    ///   `(define-record-type Foo (make-foo x y) foo?
    ///      (x foo-x) (y foo-y set-foo-y!))`
    /// becomes R6RS:
    ///   `(define-record-type (Foo make-foo foo?)
    ///      (fields (immutable x foo-x) (mutable y foo-y set-foo-y!)))`
    ///
    /// Field ordering follows the constructor's argument order, not
    /// the source order of the field-spec clauses (so the vector-
    /// slot offsets match what the constructor builds). Field-specs
    /// for fields the constructor doesn't mention are appended at
    /// the end — those fields land at later vector slots and the
    /// constructor takes only the constructor-argument fields.
    /// (Phase-1 limitation: such "hidden" fields are vector-set! to
    /// the last constructor arg's value; programs that need real
    /// "uninitialized" behavior should use a wrapper.)
    fn expand_define_record_type_r7rs(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        // items[0] = type-name (bare symbol; checked by caller)
        // items[1] = (ctor-name field...)
        // items[2] = predicate (bare symbol)
        // items[3..] = field-spec : (field-name accessor) | (field-name accessor mutator)
        if items.len() < 3 {
            return Err(ExpandError::BadSyntax {
                what: "R7RS define-record-type needs name, constructor spec, and predicate".into(),
                span,
            });
        }
        let type_name = match &items[0] {
            Datum::Symbol(s, sp) => (*s, *sp),
            _ => unreachable!("caller verified bare-symbol name"),
        };
        let ctor_parts = collect_proper_list_strict(&items[1]).ok_or(ExpandError::BadSyntax {
            what: "R7RS define-record-type: constructor spec must be a list".into(),
            span: items[1].span(),
        })?;
        if ctor_parts.is_empty() {
            return Err(ExpandError::BadSyntax {
                what: "R7RS define-record-type: empty constructor spec".into(),
                span: items[1].span(),
            });
        }
        let ctor_name = match &ctor_parts[0] {
            Datum::Symbol(s, sp) => (*s, *sp),
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "constructor name must be a symbol".into(),
                    span: ctor_parts[0].span(),
                })
            }
        };
        let mut ctor_fields: Vec<(Symbol, Span)> = Vec::with_capacity(ctor_parts.len() - 1);
        for f in &ctor_parts[1..] {
            match f {
                Datum::Symbol(s, sp) => ctor_fields.push((*s, *sp)),
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "constructor field name must be a symbol".into(),
                        span: f.span(),
                    })
                }
            }
        }
        let pred_name = match &items[2] {
            Datum::Symbol(s, sp) => (*s, *sp),
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "R7RS define-record-type: predicate must be a symbol".into(),
                    span: items[2].span(),
                })
            }
        };

        // Collect field-spec clauses into a map.
        struct FldSpec {
            accessor: Datum,
            mutator: Option<Datum>,
            span: Span,
        }
        let mut field_map: std::collections::HashMap<Symbol, FldSpec> =
            std::collections::HashMap::new();
        let mut field_order: Vec<Symbol> = Vec::new();
        for fs in &items[3..] {
            let parts = collect_proper_list_strict(fs).ok_or(ExpandError::BadSyntax {
                what: "field-spec must be a list".into(),
                span: fs.span(),
            })?;
            if parts.len() < 2 || parts.len() > 3 {
                return Err(ExpandError::BadSyntax {
                    what: "field-spec must be (field-name accessor) or \
                           (field-name accessor mutator)"
                        .into(),
                    span: fs.span(),
                });
            }
            let fname = match &parts[0] {
                Datum::Symbol(s, _) => *s,
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "field-spec name must be a symbol".into(),
                        span: parts[0].span(),
                    })
                }
            };
            if !matches!(parts[1], Datum::Symbol(_, _)) {
                return Err(ExpandError::BadSyntax {
                    what: "field-spec accessor must be a symbol".into(),
                    span: parts[1].span(),
                });
            }
            if parts.len() == 3 && !matches!(parts[2], Datum::Symbol(_, _)) {
                return Err(ExpandError::BadSyntax {
                    what: "field-spec mutator must be a symbol".into(),
                    span: parts[2].span(),
                });
            }
            if field_map.contains_key(&fname) {
                return Err(ExpandError::BadSyntax {
                    what: "duplicate field-spec".into(),
                    span: fs.span(),
                });
            }
            field_map.insert(
                fname,
                FldSpec {
                    accessor: parts[1].clone(),
                    mutator: parts.get(2).cloned(),
                    span: fs.span(),
                },
            );
            field_order.push(fname);
        }
        // Verify every constructor field has a matching field-spec.
        for (cf, sp) in &ctor_fields {
            if !field_map.contains_key(cf) {
                return Err(ExpandError::BadSyntax {
                    what: format!(
                        "R7RS define-record-type: constructor field '{}' has no matching \
                         field-spec",
                        self.syms.name(*cf)
                    ),
                    span: *sp,
                });
            }
        }

        // Synthesize R6RS-shaped Datum:
        //   (define-record-type (<name> <ctor> <pred>)
        //     (fields <field-decl>...))
        //
        // Field-decl shape per R6RS:
        //   (immutable <fname> <accessor>)
        //   (mutable <fname> <accessor> <mutator>)
        //
        // Order fields by the constructor's argument order (so vector
        // slots match), then append any field-specs the constructor
        // didn't mention.
        let mut ordered_fields: Vec<Symbol> = ctor_fields.iter().map(|(s, _)| *s).collect();
        for f in &field_order {
            if !ordered_fields.contains(f) {
                ordered_fields.push(*f);
            }
        }
        let immutable_kw = self.keywords.immutable;
        let mutable_kw = self.keywords.mutable;
        let mut field_decls: Vec<Datum> = Vec::with_capacity(ordered_fields.len());
        for fname in &ordered_fields {
            let spec = field_map
                .get(fname)
                .expect("field map has all entries we listed");
            let mut parts: Vec<Datum> = Vec::new();
            let head = if spec.mutator.is_some() {
                mutable_kw
            } else {
                immutable_kw
            };
            parts.push(Datum::Symbol(head, spec.span));
            parts.push(Datum::Symbol(*fname, spec.span));
            parts.push(spec.accessor.clone());
            if let Some(m) = &spec.mutator {
                parts.push(m.clone());
            }
            field_decls.push(Self::datum_list(parts, spec.span));
        }
        let fields_clause = {
            let mut parts: Vec<Datum> = Vec::with_capacity(1 + field_decls.len());
            parts.push(Datum::Symbol(self.keywords.fields, span));
            parts.extend(field_decls);
            Self::datum_list(parts, span)
        };
        let name_spec = Self::datum_list(
            vec![
                Datum::Symbol(type_name.0, type_name.1),
                Datum::Symbol(ctor_name.0, ctor_name.1),
                Datum::Symbol(pred_name.0, pred_name.1),
            ],
            span,
        );

        let r6rs_items = vec![name_spec, fields_clause];
        self.expand_define_record_type(&r6rs_items, span)
    }

    /// `(define-record-type name-spec ...)` — R6RS or R7RS.
    ///
    /// **R6RS shape:**
    ///   `(define-record-type <name>|(<name> <ctor> <pred>) (parent <p>)? (fields ...)?)`
    /// Desugars to vector-backed records:
    /// - Constructor: `(make-NAME f1 f2 ...)` returns `#(<tag> f1 f2 ...)`
    /// - Predicate:   `(NAME? v)` checks vector? + length + tag
    /// - Accessor:    `(NAME-FIELD r)` returns `(vector-ref r <i>)`
    /// - Mutator:     `(NAME-FIELD-set! r v)` invokes `vector-set!`
    ///
    /// **R7RS shape:**
    ///   `(define-record-type <name> (<ctor> <field>...) <pred> (<field> <accessor>)
    ///    | (<field> <accessor> <mutator>) ...)`
    ///
    /// Detection: a bare-symbol `<name>` followed by a list whose head
    /// is NOT `parent`/`fields` is R7RS. The R7RS branch dispatches to
    /// `expand_define_record_type_r7rs` and reuses the same vector-
    /// tagged record builder once it has gathered the field list.
    fn expand_define_record_type(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() < 2 {
            return Err(ExpandError::BadSyntax {
                what: "define-record-type needs name-spec and fields-spec".into(),
                span,
            });
        }
        // Detect R7RS shape: name is a bare symbol AND items[1] is a
        // list whose head is neither `parent` nor `fields`.
        if let Datum::Symbol(_, _) = &items[0] {
            if let Some(parts) = collect_proper_list_strict(&items[1]) {
                let head_is_r6rs_clause = match parts.first() {
                    Some(Datum::Symbol(s, _)) => {
                        *s == self.keywords.parent || *s == self.keywords.fields
                    }
                    _ => false,
                };
                if !head_is_r6rs_clause {
                    return self.expand_define_record_type_r7rs(items, span);
                }
            }
        }
        // Parse name-spec: either a bare symbol or (name constructor predicate)
        let (type_name, ctor_name, pred_name) = match &items[0] {
            Datum::Symbol(s, _) => {
                let name = self.syms.name(*s).to_string();
                let ctor = self.syms.intern(&format!("make-{}", name));
                let pred = self.syms.intern(&format!("{}?", name));
                (*s, ctor, pred)
            }
            Datum::Pair(_, _, _) => {
                let parts =
                    collect_proper_list_strict(&items[0]).ok_or(ExpandError::BadSyntax {
                        what: "define-record-type: bad name spec".into(),
                        span,
                    })?;
                if parts.len() != 3 {
                    return Err(ExpandError::BadSyntax {
                        what: "(name constructor predicate) form requires 3 elements".into(),
                        span,
                    });
                }
                let n = match &parts[0] {
                    Datum::Symbol(s, _) => *s,
                    _ => {
                        return Err(ExpandError::BadSyntax {
                            what: "type name must be a symbol".into(),
                            span,
                        });
                    }
                };
                let c = match &parts[1] {
                    Datum::Symbol(s, _) => *s,
                    _ => {
                        return Err(ExpandError::BadSyntax {
                            what: "constructor name must be a symbol".into(),
                            span,
                        });
                    }
                };
                let p = match &parts[2] {
                    Datum::Symbol(s, _) => *s,
                    _ => {
                        return Err(ExpandError::BadSyntax {
                            what: "predicate name must be a symbol".into(),
                            span,
                        });
                    }
                };
                (n, c, p)
            }
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "define-record-type: invalid name-spec".into(),
                    span,
                });
            }
        };

        // Parse the remaining clauses: at most one `(parent <type-name>)` and
        // at most one `(fields field-decl ...)`. Either may be omitted —
        // (parent ...) is optional, and (fields ...) is optional too if the
        // record has no own fields beyond what it inherits.
        let mut parent_type_name: Option<Symbol> = None;
        let mut fields_clause: Option<Vec<Datum>> = None;
        for clause in &items[1..] {
            let parts = collect_proper_list_strict(clause).ok_or(ExpandError::BadSyntax {
                what: "define-record-type clause must be a list".into(),
                span: clause.span(),
            })?;
            if parts.is_empty() {
                return Err(ExpandError::BadSyntax {
                    what: "empty clause".into(),
                    span: clause.span(),
                });
            }
            match &parts[0] {
                Datum::Symbol(s, _) if *s == self.keywords.fields => {
                    if fields_clause.is_some() {
                        return Err(ExpandError::BadSyntax {
                            what: "duplicate (fields ...) clause".into(),
                            span: clause.span(),
                        });
                    }
                    fields_clause = Some(parts);
                }
                Datum::Symbol(s, _) if *s == self.keywords.parent => {
                    if parts.len() != 2 {
                        return Err(ExpandError::BadSyntax {
                            what: "(parent <type-name>) needs exactly one argument".into(),
                            span: clause.span(),
                        });
                    }
                    let name = match &parts[1] {
                        Datum::Symbol(p, _) => *p,
                        _ => {
                            return Err(ExpandError::BadSyntax {
                                what: "parent name must be a symbol".into(),
                                span: clause.span(),
                            });
                        }
                    };
                    if parent_type_name.is_some() {
                        return Err(ExpandError::BadSyntax {
                            what: "duplicate (parent ...) clause".into(),
                            span: clause.span(),
                        });
                    }
                    parent_type_name = Some(name);
                }
                Datum::Symbol(s, _) => {
                    return Err(ExpandError::BadSyntax {
                        what: format!(
                            "define-record-type: unknown clause '{}'",
                            self.syms.name(*s)
                        ),
                        span: clause.span(),
                    });
                }
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "clause must start with a symbol".into(),
                        span: clause.span(),
                    });
                }
            }
        }
        // Resolve parent info up front: we need its tag (for ancestor list)
        // and its field count (for offsetting our own accessors).
        let (parent_info, inherited_field_count): (Option<RecordTypeInfo>, usize) =
            if let Some(p) = parent_type_name {
                let info =
                    self.record_types
                        .get(&p)
                        .cloned()
                        .ok_or_else(|| ExpandError::BadSyntax {
                            what: format!(
                                "parent type '{}' is not a known define-record-type",
                                self.syms.name(p)
                            ),
                            span,
                        })?;
                let fc = info.field_count;
                (Some(info), fc)
            } else {
                (None, 0)
            };
        // Empty fields_clause means no own fields (just inherits). Synthesize
        // a parts list with only the (fields ...) head so the existing parser
        // below sees an empty field list. Same for explicit (fields).
        let fields_block_parts: Vec<Datum> =
            fields_clause.unwrap_or_else(|| vec![Datum::Symbol(self.keywords.fields, span)]);

        // Each field-decl is either:
        //   field-name → immutable, accessor = NAME-FIELD
        //   (immutable field-name accessor)
        //   (mutable field-name accessor mutator)
        struct FieldDecl {
            name: cs_core::Symbol,
            accessor: cs_core::Symbol,
            mutator: Option<cs_core::Symbol>,
        }
        let type_name_str = self.syms.name(type_name).to_string();
        let mut fields: Vec<FieldDecl> = Vec::new();
        for f in &fields_block_parts[1..] {
            match f {
                Datum::Symbol(s, _) => {
                    let fname = self.syms.name(*s).to_string();
                    let accessor = self.syms.intern(&format!("{}-{}", type_name_str, fname));
                    fields.push(FieldDecl {
                        name: *s,
                        accessor,
                        mutator: None,
                    });
                }
                Datum::Pair(_, _, _) => {
                    let parts = collect_proper_list_strict(f).ok_or(ExpandError::BadSyntax {
                        what: "field decl must be a list".into(),
                        span: f.span(),
                    })?;
                    if parts.is_empty() {
                        return Err(ExpandError::BadSyntax {
                            what: "empty field decl".into(),
                            span: f.span(),
                        });
                    }
                    let kind = match &parts[0] {
                        Datum::Symbol(s, _) => *s,
                        _ => {
                            return Err(ExpandError::BadSyntax {
                                what: "field-decl: expected immutable/mutable keyword".into(),
                                span: f.span(),
                            });
                        }
                    };
                    if kind == self.keywords.immutable {
                        if parts.len() != 3 {
                            return Err(ExpandError::BadSyntax {
                                what: "(immutable field accessor) needs 3 elements".into(),
                                span: f.span(),
                            });
                        }
                        let name = match &parts[1] {
                            Datum::Symbol(s, _) => *s,
                            _ => {
                                return Err(ExpandError::BadSyntax {
                                    what: "field name must be symbol".into(),
                                    span: f.span(),
                                });
                            }
                        };
                        let accessor = match &parts[2] {
                            Datum::Symbol(s, _) => *s,
                            _ => {
                                return Err(ExpandError::BadSyntax {
                                    what: "accessor name must be symbol".into(),
                                    span: f.span(),
                                });
                            }
                        };
                        fields.push(FieldDecl {
                            name,
                            accessor,
                            mutator: None,
                        });
                    } else if kind == self.keywords.mutable {
                        // Accept three shapes:
                        //   (mutable FIELD)
                        //     → accessor NAME-FIELD, mutator set-NAME-FIELD!
                        //   (mutable FIELD ACCESSOR MUTATOR)
                        //     → fully explicit
                        // The two-element shorthand is what
                        // define-record-mutable in lib/record/record.scm
                        // relies on; syntax-rules can't synthesize the
                        // accessor/mutator names itself.
                        if parts.len() != 2 && parts.len() != 4 {
                            return Err(ExpandError::BadSyntax {
                                what: "(mutable field) or (mutable field accessor mutator)".into(),
                                span: f.span(),
                            });
                        }
                        let name = match &parts[1] {
                            Datum::Symbol(s, _) => *s,
                            _ => {
                                return Err(ExpandError::BadSyntax {
                                    what: "field name must be symbol".into(),
                                    span: f.span(),
                                });
                            }
                        };
                        let (accessor, mutator) = if parts.len() == 2 {
                            let fname = self.syms.name(name).to_string();
                            (
                                self.syms.intern(&format!("{}-{}", type_name_str, fname)),
                                self.syms
                                    .intern(&format!("set-{}-{}!", type_name_str, fname)),
                            )
                        } else {
                            let acc = match &parts[2] {
                                Datum::Symbol(s, _) => *s,
                                _ => {
                                    return Err(ExpandError::BadSyntax {
                                        what: "accessor name must be symbol".into(),
                                        span: f.span(),
                                    });
                                }
                            };
                            let mut_ = match &parts[3] {
                                Datum::Symbol(s, _) => *s,
                                _ => {
                                    return Err(ExpandError::BadSyntax {
                                        what: "mutator name must be symbol".into(),
                                        span: f.span(),
                                    });
                                }
                            };
                            (acc, mut_)
                        };
                        fields.push(FieldDecl {
                            name,
                            accessor,
                            mutator: Some(mutator),
                        });
                    } else {
                        return Err(ExpandError::BadSyntax {
                            what: "field-decl: kind must be 'immutable' or 'mutable'".into(),
                            span: f.span(),
                        });
                    }
                }
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "field-decl must be symbol or list".into(),
                        span: f.span(),
                    });
                }
            }
        }

        // Build the tag value: a fresh symbol unique to this type by appending
        // a marker. Two define-record-types of the same name will collide; that
        // matches our foundation simplification (R6RS uses gensym uids).
        let tag_sym = self.syms.intern(&format!("__rec-tag-{}__", type_name_str));
        let tag_const = CoreExpr::Const {
            value: Value::Symbol(tag_sym),
            span,
        };

        // Helper refs
        let vector_sym = self.syms.intern("vector");
        let vector_p_sym = self.syms.intern("vector?");
        let vector_length_sym = self.syms.intern("vector-length");
        let vector_ref_sym = self.syms.intern("vector-ref");
        let vector_set_sym = self.syms.intern("vector-set!");
        let eq_p_sym = self.syms.intern("eq?");
        let ge_sym = self.syms.intern(">=");
        let and_chain = |exprs: Vec<CoreExpr>| -> CoreExpr {
            // Build a chain of `if` for `and`
            let mut acc = CoreExpr::Const {
                value: Value::Boolean(true),
                span,
            };
            for e in exprs.into_iter().rev() {
                acc = CoreExpr::If {
                    cond: Rc::new(e),
                    then: Rc::new(acc),
                    alt: Rc::new(CoreExpr::Const {
                        value: Value::Boolean(false),
                        span,
                    }),
                    span,
                };
            }
            acc
        };

        let mut out: Vec<CoreExpr> = Vec::new();

        // Build the ancestor tag chain at expansion time. Immediate parent
        // first, root last. Empty for a root record type.
        let ancestors: Vec<cs_core::Symbol> = match &parent_info {
            Some(p) => {
                let mut a = Vec::with_capacity(1 + p.ancestors.len());
                a.push(p.tag);
                a.extend(p.ancestors.iter().copied());
                a
            }
            None => Vec::new(),
        };

        // Synthesize parent field-param symbols. R6RS lets the parent's
        // constructor be customized via record protocols; we use the simple
        // default — parent fields first, then own fields, in source order.
        // The names here are gensym-fresh so they can't collide with user
        // identifiers in the generated lambda body.
        let parent_field_params: Vec<cs_core::Symbol> = (0..inherited_field_count)
            .map(|i| {
                self.syms
                    .intern(&format!("__rec-pf-{}-{}__", type_name_str, i))
            })
            .collect();

        // 1. Constructor: (lambda (pf1 ... pfN f1 ... fM) (vector tag pf1 ... f1 ...))
        let mut ctor_params: Vec<cs_core::Symbol> =
            Vec::with_capacity(parent_field_params.len() + fields.len());
        ctor_params.extend(parent_field_params.iter().copied());
        ctor_params.extend(fields.iter().map(|f| f.name));
        let mut vector_call_args: Vec<CoreExpr> = vec![tag_const.clone()];
        for p in &parent_field_params {
            vector_call_args.push(CoreExpr::Ref { name: *p, span });
        }
        for f in &fields {
            vector_call_args.push(CoreExpr::Ref { name: f.name, span });
        }
        let ctor_body = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: vector_sym,
                span,
            }),
            args: vector_call_args,
            span,
        };
        let ctor_lambda = CoreExpr::Lambda {
            params: Params::fixed(ctor_params),
            body: Rc::new(ctor_body),
            span,
        };
        out.push(CoreExpr::Set {
            name: ctor_name,
            value: Rc::new(ctor_lambda),
            span,
        });

        // 2. Predicate. Generated form:
        //   (lambda (obj)
        //     (and (vector? obj) (>= (vector-length obj) 1)
        //          (let ((t (vector-ref obj 0)))
        //            (or (eq? t '<my-tag>)
        //                (memq '<my-tag>
        //                      (hashtable-ref __record-parents__ t '()))))))
        // The OR-with-memq accepts descendant tags. We don't need to know
        // the descendants at expansion time — children register themselves
        // in the registry as they're defined, so a parent's predicate
        // automatically picks up new subtypes.
        let obj_sym = self.syms.intern("__rec-obj__");
        let obj_ref = CoreExpr::Ref {
            name: obj_sym,
            span,
        };
        let registry_sym = self.syms.intern("__record-parents__");
        let hashtable_ref_sym = self.syms.intern("hashtable-ref");
        let memq_sym = self.syms.intern("memq");
        let t_sym = self.syms.intern("__rec-t__");
        let t_ref = CoreExpr::Ref { name: t_sym, span };
        let direct_eq = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: eq_p_sym,
                span,
            }),
            args: vec![t_ref.clone(), tag_const.clone()],
            span,
        };
        let registry_lookup = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: hashtable_ref_sym,
                span,
            }),
            args: vec![
                CoreExpr::Ref {
                    name: registry_sym,
                    span,
                },
                t_ref.clone(),
                CoreExpr::Const {
                    value: Value::Null,
                    span,
                },
            ],
            span,
        };
        let memq_check = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: memq_sym,
                span,
            }),
            args: vec![tag_const.clone(), registry_lookup],
            span,
        };
        let or_check = CoreExpr::If {
            cond: Rc::new(direct_eq),
            then: Rc::new(CoreExpr::Const {
                value: Value::Boolean(true),
                span,
            }),
            alt: Rc::new(memq_check),
            span,
        };
        let tag_check_with_let = CoreExpr::Letrec {
            bindings: vec![(
                t_sym,
                CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: vector_ref_sym,
                        span,
                    }),
                    args: vec![
                        obj_ref.clone(),
                        CoreExpr::Const {
                            value: Value::fixnum(0),
                            span,
                        },
                    ],
                    span,
                },
            )],
            body: Rc::new(or_check),
            span,
        };
        let pred_body = and_chain(vec![
            CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: vector_p_sym,
                    span,
                }),
                args: vec![obj_ref.clone()],
                span,
            },
            CoreExpr::App {
                func: Rc::new(CoreExpr::Ref { name: ge_sym, span }),
                args: vec![
                    CoreExpr::App {
                        func: Rc::new(CoreExpr::Ref {
                            name: vector_length_sym,
                            span,
                        }),
                        args: vec![obj_ref.clone()],
                        span,
                    },
                    CoreExpr::Const {
                        value: Value::fixnum(1),
                        span,
                    },
                ],
                span,
            },
            tag_check_with_let,
        ]);
        let pred_lambda = CoreExpr::Lambda {
            params: Params::fixed(vec![obj_sym]),
            body: Rc::new(pred_body),
            span,
        };
        out.push(CoreExpr::Set {
            name: pred_name,
            value: Rc::new(pred_lambda),
            span,
        });

        // 3. Accessors and mutators (one per *own* field). Inherited fields
        // already have accessors emitted by the parent's expansion that read
        // slots `1..=inherited_field_count`, and our instance vectors keep
        // those same slots for parent fields, so the parent's accessors work
        // unchanged on us. Our own fields start at slot `1 + inherited`.
        let rec_sym = self.syms.intern("__rec-rec__");
        let val_sym = self.syms.intern("__rec-val__");
        for (i, field) in fields.iter().enumerate() {
            let idx = (1 + inherited_field_count + i) as i64;
            // Accessor: (lambda (rec) (vector-ref rec idx))
            let accessor_lambda = CoreExpr::Lambda {
                params: Params::fixed(vec![rec_sym]),
                body: Rc::new(CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: vector_ref_sym,
                        span,
                    }),
                    args: vec![
                        CoreExpr::Ref {
                            name: rec_sym,
                            span,
                        },
                        CoreExpr::Const {
                            value: Value::fixnum(idx),
                            span,
                        },
                    ],
                    span,
                }),
                span,
            };
            out.push(CoreExpr::Set {
                name: field.accessor,
                value: Rc::new(accessor_lambda),
                span,
            });
            // Mutator (if mutable): (lambda (rec val) (vector-set! rec idx val))
            if let Some(mutator) = field.mutator {
                let mutator_lambda = CoreExpr::Lambda {
                    params: Params::fixed(vec![rec_sym, val_sym]),
                    body: Rc::new(CoreExpr::App {
                        func: Rc::new(CoreExpr::Ref {
                            name: vector_set_sym,
                            span,
                        }),
                        args: vec![
                            CoreExpr::Ref {
                                name: rec_sym,
                                span,
                            },
                            CoreExpr::Const {
                                value: Value::fixnum(idx),
                                span,
                            },
                            CoreExpr::Ref {
                                name: val_sym,
                                span,
                            },
                        ],
                        span,
                    }),
                    span,
                };
                out.push(CoreExpr::Set {
                    name: mutator,
                    value: Rc::new(mutator_lambda),
                    span,
                });
            }
        }

        // 4. Register our ancestor list at runtime, so any parent's
        // predicate (or any future ancestor's predicate) can match us:
        //   (hashtable-set! __record-parents__ '<my-tag> '(<ancestors>))
        // Skipped when there are no ancestors AND the type is final-by-shape
        // — wait, we don't know finality, so always emit. The cost is one
        // hashtable insert at definition time, paid once.
        let hashtable_set_sym = self.syms.intern("hashtable-set!");
        let ancestors_value = Value::list(
            ancestors
                .iter()
                .map(|a| Value::Symbol(*a))
                .collect::<Vec<_>>(),
        );
        out.push(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: hashtable_set_sym,
                span,
            }),
            args: vec![
                CoreExpr::Ref {
                    name: registry_sym,
                    span,
                },
                tag_const.clone(),
                CoreExpr::Const {
                    value: ancestors_value,
                    span,
                },
            ],
            span,
        });

        // Track in expander state so subsequent (parent <type-name>) clauses
        // can resolve us.
        self.record_types.insert(
            type_name,
            RecordTypeInfo {
                tag: tag_sym,
                ancestors: ancestors.clone(),
                field_count: inherited_field_count + fields.len(),
            },
        );

        Ok(CoreExpr::Begin { exprs: out, span })
    }

    /// `(define-condition-type <type-name> <parent-name> <ctor> <pred>
    ///    (<field> <accessor>) ...)`
    ///
    /// Desugars to one runtime registration call (so the parent chain is
    /// visible to predicate lookups) plus three lambda-bound bindings:
    /// constructor, predicate, and one accessor per field. We use the
    /// symbol name as the runtime tag string; standard types like
    /// `&error` already match this convention, so a user can extend any
    /// standard or previously-defined user type as a parent.
    ///
    /// The expansion does NOT use `define-record-type` because conditions
    /// have flat (compound-of-simples) semantics — `condition?` and the
    /// predicate-walking is registry-based, not record-based.
    fn expand_define_condition_type(
        &mut self,
        items: &[Datum],
        span: Span,
    ) -> Result<CoreExpr, ExpandError> {
        if items.len() < 4 {
            return Err(ExpandError::BadSyntax {
                what: "define-condition-type needs <type> <parent> <ctor> <pred> [(field accessor) ...]"
                    .into(),
                span,
            });
        }
        let type_sym = match &items[0] {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "define-condition-type: type name must be a symbol".into(),
                    span: items[0].span(),
                });
            }
        };
        let parent_sym = match &items[1] {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "define-condition-type: parent must be a symbol".into(),
                    span: items[1].span(),
                });
            }
        };
        let ctor_sym = match &items[2] {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "define-condition-type: constructor name must be a symbol".into(),
                    span: items[2].span(),
                });
            }
        };
        let pred_sym = match &items[3] {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "define-condition-type: predicate name must be a symbol".into(),
                    span: items[3].span(),
                });
            }
        };
        // Each remaining item is `(<field-name> <accessor-name>)`. Field
        // count is the total (positional) — we don't yet support inherited
        // fields appearing as parameters; the constructor here only takes
        // own fields, matching the existing simple-condition shape where
        // each simple holds only its own type's fields.
        let mut field_accessors: Vec<Symbol> = Vec::new();
        for spec in &items[4..] {
            let parts = collect_proper_list_strict(spec).ok_or(ExpandError::BadSyntax {
                what: "define-condition-type field-spec must be a list".into(),
                span: spec.span(),
            })?;
            if parts.len() != 2 {
                return Err(ExpandError::BadSyntax {
                    what: "field-spec must be (<field-name> <accessor-name>)".into(),
                    span: spec.span(),
                });
            }
            // Field name itself is unused at expansion time — it only names
            // the slot for documentation. The accessor symbol is what we
            // actually bind.
            let _field_name = match &parts[0] {
                Datum::Symbol(s, _) => *s,
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "field name must be a symbol".into(),
                        span: spec.span(),
                    });
                }
            };
            let accessor = match &parts[1] {
                Datum::Symbol(s, _) => *s,
                _ => {
                    return Err(ExpandError::BadSyntax {
                        what: "accessor name must be a symbol".into(),
                        span: spec.span(),
                    });
                }
            };
            field_accessors.push(accessor);
        }
        let type_tag = self.syms.name(type_sym).to_string();
        let parent_tag = self.syms.name(parent_sym).to_string();

        // Resolve the parent's known field count. Standard types like
        // `&error` carry no fields; `&message`, `&irritants`, and `&who`
        // each carry one. User parents come from `condition_types`.
        let inherited_field_count = if let Some(info) = self.condition_types.get(&parent_sym) {
            info.field_count
        } else {
            standard_condition_field_count(&parent_tag)
        };
        let own_field_count = field_accessors.len();
        let total_field_count = inherited_field_count + own_field_count;

        // Helper symbols.
        let cond_register_sym = self.syms.intern("condition-register-parent!");
        let cond_instance_of_sym = self.syms.intern("condition-instance-of?");
        let cond_field_ref_sym = self.syms.intern("condition-field-ref");
        let make_simple_sym = self.syms.intern("make-simple-condition");
        let cond_obj_sym = self.syms.intern("__cond-obj__");

        let tag_const = CoreExpr::Const {
            value: Value::string(type_tag.clone()),
            span,
        };
        let parent_const = CoreExpr::Const {
            value: Value::string(parent_tag),
            span,
        };

        let mut out: Vec<CoreExpr> = Vec::new();

        // 1. Register parent at runtime.
        out.push(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cond_register_sym,
                span,
            }),
            args: vec![tag_const.clone(), parent_const],
            span,
        });

        // 2. Constructor takes inherited-then-own fields and stores them
        // all in a single simple `#(<my-tag> if0 ... own0 ...)`. Parent
        // accessors that look up by their tag find this simple as a
        // descendant and read the inherited slots, which sit at the same
        // offsets they would in a parent-tagged simple.
        let mut ctor_params: Vec<Symbol> = Vec::with_capacity(total_field_count);
        for i in 0..inherited_field_count {
            ctor_params.push(self.syms.intern(&format!("__cond-pf-{}-{}__", type_tag, i)));
        }
        for i in 0..own_field_count {
            ctor_params.push(self.syms.intern(&format!("__cond-of-{}-{}__", type_tag, i)));
        }
        let mut ctor_call_args: Vec<CoreExpr> = vec![tag_const.clone()];
        for p in &ctor_params {
            ctor_call_args.push(CoreExpr::Ref { name: *p, span });
        }
        let ctor_body = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: make_simple_sym,
                span,
            }),
            args: ctor_call_args,
            span,
        };
        out.push(CoreExpr::Set {
            name: ctor_sym,
            value: Rc::new(CoreExpr::Lambda {
                params: Params::fixed(ctor_params),
                body: Rc::new(ctor_body),
                span,
            }),
            span,
        });

        // 3. Predicate: (lambda (c) (condition-instance-of? c <tag>)).
        let pred_body = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: cond_instance_of_sym,
                span,
            }),
            args: vec![
                CoreExpr::Ref {
                    name: cond_obj_sym,
                    span,
                },
                tag_const.clone(),
            ],
            span,
        };
        out.push(CoreExpr::Set {
            name: pred_sym,
            value: Rc::new(CoreExpr::Lambda {
                params: Params::fixed(vec![cond_obj_sym]),
                body: Rc::new(pred_body),
                span,
            }),
            span,
        });

        // 4. Accessors: (lambda (c) (condition-field-ref c <tag> <i>)).
        for (i, accessor) in field_accessors.iter().enumerate() {
            let body = CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: cond_field_ref_sym,
                    span,
                }),
                args: vec![
                    CoreExpr::Ref {
                        name: cond_obj_sym,
                        span,
                    },
                    tag_const.clone(),
                    CoreExpr::Const {
                        value: Value::fixnum((inherited_field_count + i) as i64),
                        span,
                    },
                ],
                span,
            };
            out.push(CoreExpr::Set {
                name: *accessor,
                value: Rc::new(CoreExpr::Lambda {
                    params: Params::fixed(vec![cond_obj_sym]),
                    body: Rc::new(body),
                    span,
                }),
                span,
            });
        }

        // Register so future subtypes can resolve our inherited field count.
        self.condition_types.insert(
            type_sym,
            ConditionTypeInfo {
                field_count: total_field_count,
            },
        );

        Ok(CoreExpr::Begin { exprs: out, span })
    }

    // ---- macro expansion (M3 first cut: non-hygienic syntax-rules) ----

    /// Build the combinator-symbol bundle the `syntax_parse` matcher
    /// needs, from the already-interned `Keywords`.
    fn parse_syms(&self) -> syntax_parse::ParseSyms {
        syntax_parse::ParseSyms {
            ellipsis: self.keywords.ellipsis,
            underscore: self.keywords.underscore,
            tilde_or: self.keywords.tilde_or,
            tilde_optional: self.keywords.tilde_optional,
            tilde_once: self.keywords.tilde_once,
            kw_defaults: self.keywords.kw_defaults,
        }
    }

    fn try_expand_macro(
        &mut self,
        name: cs_core::Symbol,
        input: &Datum,
    ) -> Result<Datum, ExpandError> {
        let macro_def = self
            .macros
            .get(&name)
            .cloned()
            .ok_or_else(|| ExpandError::BadSyntax {
                what: "macro lookup failed".into(),
                span: input.span(),
            })?;
        let parse_syms = self.parse_syms();
        // Furthest-position failure across all clauses, used to build
        // a pinpointed error when no clause matches (R6RS++ Phase 2A.4,
        // issue #33).
        let mut best: Option<syntax_parse::MatchError> = None;
        let keep_best = |best: &mut Option<syntax_parse::MatchError>,
                         me: syntax_parse::MatchError| {
            if best.as_ref().is_none_or(|b| me.span.end >= b.span.end) {
                *best = Some(me);
            }
        };
        for (pattern, template) in &macro_def.rules {
            let mut bindings: std::collections::HashMap<cs_core::Symbol, MatchBinding> =
                std::collections::HashMap::new();
            // Combinator-using parsers (`~or`/`~optional`/`~once`) need the
            // backtracking matcher; plain syntax-rules macros keep the fast
            // deterministic path. (R6RS++ Phase 2A.3, issue #31.)
            if macro_def.parser {
                match syntax_parse::match_parse_clause(
                    pattern,
                    input,
                    &macro_def.literals,
                    &parse_syms,
                    self.syms,
                    &mut bindings,
                ) {
                    Ok(()) => return self.instantiate_template(template, &bindings),
                    Err(me) => keep_best(&mut best, me),
                }
            } else if match_pattern(
                pattern,
                input,
                &macro_def.literals,
                self.keywords.ellipsis,
                self.keywords.underscore,
                true,
                &mut bindings,
            ) {
                return self.instantiate_template(template, &bindings);
            } else if let Some(me) = diagnose_sr(
                pattern,
                input,
                &macro_def.literals,
                self.keywords.ellipsis,
                self.keywords.underscore,
                true,
                self.syms,
            ) {
                keep_best(&mut best, me);
            }
        }
        let macro_name = self.syms.name(name).to_string();
        let err = match best {
            Some(me) => ExpandError::BadSyntax {
                what: format!("`{}`: {}", macro_name, me.reason),
                span: me.span,
            },
            None => ExpandError::BadSyntax {
                what: format!("no matching rule for macro '{}'", macro_name),
                span: input.span(),
            },
        };
        Err(err)
    }

    fn instantiate_template(
        &mut self,
        template: &Datum,
        bindings: &std::collections::HashMap<cs_core::Symbol, MatchBinding>,
    ) -> Result<Datum, ExpandError> {
        let raw = instantiate(
            template,
            bindings,
            self.keywords.ellipsis,
            &mut self.gensym_counter,
            self.syms,
        )?;
        // Hygiene post-pass: rename marked binders, then strip markers.
        let renames = std::collections::HashMap::new();
        let hygiened = hygiene_pass(&raw, &renames, &mut self.gensym_counter, self.syms);
        Ok(hygiened)
    }
}

/// Forms whose first sub-form contains binder names. Each entry maps to
/// the index of the binder list within the form's tail items (after the
/// keyword), and a kind describing how to extract binder names.
fn binder_form_kind(head_name: &str) -> Option<BinderFormKind> {
    match head_name {
        "let" | "let*" | "letrec" | "letrec*" => Some(BinderFormKind::LetLike),
        "lambda" => Some(BinderFormKind::Lambda),
        "do" => Some(BinderFormKind::Do),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum BinderFormKind {
    /// `(let ((name val) ...) body ...)` and friends.
    LetLike,
    /// `(lambda (name ...) body ...)` or `(lambda name body ...)`.
    Lambda,
    /// `(do ((name init step) ...) (test result ...) body ...)`.
    Do,
}

/// Walk the Datum, renaming marked binder identifiers to gensyms (and their
/// in-scope references), then stripping markers from all symbols.
fn hygiene_pass(
    d: &Datum,
    renames: &std::collections::HashMap<cs_core::Symbol, cs_core::Symbol>,
    counter: &mut u32,
    syms: &mut SymbolTable,
) -> Datum {
    match d {
        Datum::Symbol(s, span) => {
            if let Some(replacement) = renames.get(s) {
                Datum::Symbol(*replacement, *span)
            } else if is_template_marked(*s, syms) {
                let unmarked = unmark_template_symbol(*s, syms);
                Datum::Symbol(unmarked, *span)
            } else {
                d.clone()
            }
        }
        Datum::Pair(_, _, span) => {
            let items = match collect_proper_list_strict(d) {
                Some(v) => v,
                None => return d.clone(),
            };
            // Detect binder form by head.
            if let Some(head) = items.first() {
                if let Datum::Symbol(s, _) = head {
                    let head_name_unmarked = if is_template_marked(*s, syms) {
                        let nm = syms.name(*s).to_string();
                        nm.strip_prefix(TEMPLATE_MARKER).unwrap().to_string()
                    } else {
                        syms.name(*s).to_string()
                    };
                    if let Some(kind) = binder_form_kind(&head_name_unmarked) {
                        return rename_binder_form(&items, kind, renames, counter, syms, *span);
                    }
                }
            }
            // Default: recurse on all children.
            let new_items: Vec<Datum> = items
                .iter()
                .map(|c| hygiene_pass(c, renames, counter, syms))
                .collect();
            rebuild_list(new_items, *span)
        }
        Datum::Vector(items, span) => {
            let new_items: Vec<Datum> = items
                .iter()
                .map(|c| hygiene_pass(c, renames, counter, syms))
                .collect();
            Datum::Vector(new_items, *span)
        }
        _ => d.clone(),
    }
}

fn fresh_gensym(
    orig: cs_core::Symbol,
    counter: &mut u32,
    syms: &mut SymbolTable,
) -> cs_core::Symbol {
    *counter += 1;
    let base = {
        let n = syms.name(orig);
        // Strip any leading marker.
        if let Some(stripped) = n.strip_prefix(TEMPLATE_MARKER) {
            stripped.to_string()
        } else {
            n.to_string()
        }
    };
    syms.intern(&format!("{}__G{}", base, counter))
}

fn rename_binder_form(
    items: &[Datum],
    kind: BinderFormKind,
    parent_renames: &std::collections::HashMap<cs_core::Symbol, cs_core::Symbol>,
    counter: &mut u32,
    syms: &mut SymbolTable,
    span: Span,
) -> Datum {
    // Build per-form rename map starting from parent's.
    let mut local_renames = parent_renames.clone();
    let mut new_items: Vec<Datum> = Vec::with_capacity(items.len());
    // The head: hygiene pass on it (just strips marker).
    new_items.push(hygiene_pass(&items[0], parent_renames, counter, syms));

    match kind {
        BinderFormKind::LetLike => {
            // Two shapes for `let`:
            //   (let ((name val) ...) body ...)         -- plain
            //   (let LOOP ((name val) ...) body ...)    -- named (R7RS)
            // letrec / letrec* / let* don't have a named form.
            if items.len() < 2 {
                return rebuild_list(items.to_vec(), span);
            }
            // Detect named-let: head is `let` (after stripping
            // marker) AND items[1] is a bare symbol (the loop
            // name) AND items.len() >= 3.
            let head_unmarked = match &items[0] {
                Datum::Symbol(s, _) => {
                    let n = syms.name(*s).to_string();
                    if let Some(stripped) = n.strip_prefix(TEMPLATE_MARKER) {
                        stripped.to_string()
                    } else {
                        n
                    }
                }
                _ => String::new(),
            };
            let is_named_let = head_unmarked == "let"
                && items.len() >= 3
                && matches!(&items[1], Datum::Symbol(_, _));

            let (loop_name_idx, bindings_idx, body_start_idx) = if is_named_let {
                (Some(1usize), 2usize, 3usize)
            } else {
                (None, 1usize, 2usize)
            };

            // For named-let, the loop name is a binder visible
            // throughout the body. Rename it (if marked) BEFORE
            // processing bindings so any recursive references
            // inside the body / step exprs see the fresh name.
            if let Some(lni) = loop_name_idx {
                let loop_datum = &items[lni];
                let new_loop_datum = match loop_datum {
                    Datum::Symbol(s, ns) if is_template_marked(*s, syms) => {
                        let fresh = fresh_gensym(*s, counter, syms);
                        local_renames.insert(*s, fresh);
                        Datum::Symbol(fresh, *ns)
                    }
                    other => hygiene_pass(other, parent_renames, counter, syms),
                };
                // Stash for re-insertion in order: we'll push
                // [head, loop_name, bindings, body...] below.
                new_items.push(new_loop_datum);
            }

            let bindings_datum = &items[bindings_idx];
            let body_datums = &items[body_start_idx..];

            // Process bindings: for each (name val), rename name if marked.
            // Val evals in the outer scope (parent_renames + loop_name
            // rename if named-let), not the binding-local scope.
            let bindings_items = collect_proper_list_strict(bindings_datum).unwrap_or_default();
            let mut new_bindings: Vec<Datum> = Vec::with_capacity(bindings_items.len());
            // Snapshot of renames available for val expressions:
            // includes the loop_name (for named-let) but NOT the
            // about-to-be-introduced binding names.
            let val_renames = local_renames.clone();
            for b in &bindings_items {
                let parts = collect_proper_list_strict(b).unwrap_or_default();
                if parts.len() != 2 {
                    new_bindings.push(hygiene_pass(b, &val_renames, counter, syms));
                    continue;
                }
                let name_datum = &parts[0];
                let val_datum = &parts[1];
                let new_name_datum = match name_datum {
                    Datum::Symbol(s, ns) if is_template_marked(*s, syms) => {
                        let fresh = fresh_gensym(*s, counter, syms);
                        local_renames.insert(*s, fresh);
                        Datum::Symbol(fresh, *ns)
                    }
                    other => hygiene_pass(other, parent_renames, counter, syms),
                };
                let new_val = hygiene_pass(val_datum, &val_renames, counter, syms);
                new_bindings.push(rebuild_list(vec![new_name_datum, new_val], b.span()));
            }
            new_items.push(rebuild_list(new_bindings, bindings_datum.span()));
            for body in body_datums {
                new_items.push(hygiene_pass(body, &local_renames, counter, syms));
            }
        }
        BinderFormKind::Lambda => {
            // (lambda params body ...) — params is either a symbol, a list, or a dotted list.
            if items.len() < 2 {
                return rebuild_list(items.to_vec(), span);
            }
            let params_datum = &items[1];
            let body_datums = &items[2..];
            let new_params_datum = rename_lambda_params(
                params_datum,
                &mut local_renames,
                parent_renames,
                counter,
                syms,
            );
            new_items.push(new_params_datum);
            for body in body_datums {
                new_items.push(hygiene_pass(body, &local_renames, counter, syms));
            }
        }
        BinderFormKind::Do => {
            // (do ((name init step) ...) (test result ...) body ...)
            if items.len() < 3 {
                return rebuild_list(items.to_vec(), span);
            }
            let bindings_datum = &items[1];
            let test_datum = &items[2];
            let body_datums = &items[3..];
            let bindings_items = collect_proper_list_strict(bindings_datum).unwrap_or_default();
            let mut new_bindings: Vec<Datum> = Vec::with_capacity(bindings_items.len());
            for b in &bindings_items {
                let parts = collect_proper_list_strict(b).unwrap_or_default();
                if parts.len() < 2 || parts.len() > 3 {
                    new_bindings.push(hygiene_pass(b, parent_renames, counter, syms));
                    continue;
                }
                let name_datum = &parts[0];
                let init_datum = &parts[1];
                // init: eval'd in outer scope
                let new_init = hygiene_pass(init_datum, parent_renames, counter, syms);
                let new_name_datum = match name_datum {
                    Datum::Symbol(s, ns) if is_template_marked(*s, syms) => {
                        let fresh = fresh_gensym(*s, counter, syms);
                        local_renames.insert(*s, fresh);
                        Datum::Symbol(fresh, *ns)
                    }
                    other => hygiene_pass(other, parent_renames, counter, syms),
                };
                // step: in inner scope
                let mut new_parts = vec![new_name_datum, new_init];
                if parts.len() == 3 {
                    let new_step = hygiene_pass(&parts[2], &local_renames, counter, syms);
                    new_parts.push(new_step);
                }
                new_bindings.push(rebuild_list(new_parts, b.span()));
            }
            new_items.push(rebuild_list(new_bindings, bindings_datum.span()));
            // test/result clause: in inner scope
            new_items.push(hygiene_pass(test_datum, &local_renames, counter, syms));
            for body in body_datums {
                new_items.push(hygiene_pass(body, &local_renames, counter, syms));
            }
        }
    }
    rebuild_list(new_items, span)
}

fn rename_lambda_params(
    params: &Datum,
    local_renames: &mut std::collections::HashMap<cs_core::Symbol, cs_core::Symbol>,
    parent_renames: &std::collections::HashMap<cs_core::Symbol, cs_core::Symbol>,
    counter: &mut u32,
    syms: &mut SymbolTable,
) -> Datum {
    match params {
        Datum::Symbol(s, span) => {
            if is_template_marked(*s, syms) {
                let fresh = fresh_gensym(*s, counter, syms);
                local_renames.insert(*s, fresh);
                Datum::Symbol(fresh, *span)
            } else {
                hygiene_pass(params, parent_renames, counter, syms)
            }
        }
        Datum::Pair(_, _, span) => {
            // Walk: each car may be a symbol (binder), tail may be a symbol (rest binder)
            let mut new_items: Vec<Datum> = Vec::new();
            let mut cur = params.clone();
            let mut tail_rename: Option<Datum> = None;
            loop {
                match cur {
                    Datum::Pair(car, cdr, _) => {
                        match &*car {
                            Datum::Symbol(s, ns) if is_template_marked(*s, syms) => {
                                let fresh = fresh_gensym(*s, counter, syms);
                                local_renames.insert(*s, fresh);
                                new_items.push(Datum::Symbol(fresh, *ns));
                            }
                            other => {
                                new_items.push(hygiene_pass(other, parent_renames, counter, syms));
                            }
                        }
                        cur = (*cdr).clone();
                    }
                    Datum::Null(_) => break,
                    Datum::Symbol(s, ns) if is_template_marked(s, syms) => {
                        let fresh = fresh_gensym(s, counter, syms);
                        local_renames.insert(s, fresh);
                        tail_rename = Some(Datum::Symbol(fresh, ns));
                        break;
                    }
                    other => {
                        tail_rename = Some(hygiene_pass(&other, parent_renames, counter, syms));
                        break;
                    }
                }
            }
            // Rebuild: items + optional dotted tail
            let mut acc = tail_rename.unwrap_or(Datum::Null(*span));
            for item in new_items.into_iter().rev() {
                let s = item.span().merge(acc.span());
                acc = Datum::Pair(Rc::new(item), Rc::new(acc), s);
            }
            acc
        }
        Datum::Null(_) => params.clone(),
        _ => params.clone(),
    }
}

/// A match binding: either a single Datum or a list of repetitions
/// (for ellipsis patterns).
#[derive(Clone, Debug)]
pub(crate) enum MatchBinding {
    Single(Datum),
    Repeat(Vec<Datum>),
    /// A pattern variable that an `~optional` / `~or` head pattern
    /// (syntax-parse, Phase 2A.3) could have bound but didn't,
    /// because the optional sub-pattern was absent / a different
    /// alternative was taken. Referencing it in a template is an
    /// error unless the `~optional` supplied `#:defaults` (which
    /// turns it into a `Single`). Tracked explicitly rather than
    /// left unbound so `instantiate` reports a clear diagnostic
    /// instead of silently treating the name as a literal.
    Absent,
}

/// Match `input` against `pattern`. Returns `true` if match succeeded.
/// `is_outer` controls whether the first symbol is `_` (the macro name).
fn match_pattern(
    pattern: &Datum,
    input: &Datum,
    literals: &[cs_core::Symbol],
    ellipsis_sym: cs_core::Symbol,
    underscore_sym: cs_core::Symbol,
    is_outer: bool,
    bindings: &mut std::collections::HashMap<cs_core::Symbol, MatchBinding>,
) -> bool {
    // The outer macro-name placeholder slot matches anything —
    // it's the macro keyword being invoked, not a real pattern.
    if is_outer {
        if let Datum::Symbol(_, _) = pattern {
            return true;
        }
    }
    match (pattern, input) {
        // Literal keyword: must be the same symbol — checked before
        // the `_` wildcard so a macro that explicitly puts `_` in
        // its literals list (overriding the R7RS wildcard default)
        // gets literal-match semantics. Otherwise `_` in the input
        // would slip past as a wildcard and the user's catch-all
        // identifier rule would never fire.
        (Datum::Symbol(s, _), Datum::Symbol(t, _)) if literals.contains(s) => *s == *t,
        (Datum::Symbol(s, _), _) if literals.contains(s) => false,
        // Wildcards (only when not declared as a literal above)
        (Datum::Symbol(s, _), _) if *s == underscore_sym => true,
        // Pattern variable: bind it (unless it's the outer macro name)
        (Datum::Symbol(s, _), _) => {
            if is_outer {
                // The very first symbol of the outer pattern is the macro name;
                // skip binding it.
                return true;
            }
            bindings.insert(*s, MatchBinding::Single(input.clone()));
            true
        }
        // Atoms: must equal exactly
        (Datum::Boolean(p, _), Datum::Boolean(i, _)) => p == i,
        (Datum::Number(_, _), Datum::Number(_, _)) => {
            // For now: strict equality via to_value().
            cs_core::eq::equal(&pattern.to_value(), &input.to_value())
        }
        (Datum::Character(p, _), Datum::Character(i, _)) => p == i,
        (Datum::String(p, _), Datum::String(i, _)) => **p == **i,
        (Datum::Null(_), Datum::Null(_)) => true,
        // List patterns
        (Datum::Pair(_, _, _), _) => match_list_pattern(
            pattern,
            input,
            literals,
            ellipsis_sym,
            underscore_sym,
            is_outer,
            bindings,
        ),
        _ => false,
    }
}

fn match_list_pattern(
    pattern: &Datum,
    input: &Datum,
    literals: &[cs_core::Symbol],
    ellipsis_sym: cs_core::Symbol,
    underscore_sym: cs_core::Symbol,
    is_outer: bool,
    bindings: &mut std::collections::HashMap<cs_core::Symbol, MatchBinding>,
) -> bool {
    // Detect dotted patterns: (p1 p2 . tail-pat) matches a list of
    // any length ≥ #spine, binding tail-pat to the remainder.
    // Ellipsis-in-spine + dotted-tail combinations are not supported
    // here; either-or, not both. The ellipsis branch below stays on
    // the proper-list code path.
    if let Some((pspine, ptail)) = collect_pair_chain(pattern) {
        if !matches!(ptail, Datum::Null(_)) {
            return match_dotted_list_pattern(
                &pspine,
                &ptail,
                input,
                literals,
                ellipsis_sym,
                underscore_sym,
                is_outer,
                bindings,
            );
        }
    }
    let pattern_items = match collect_proper_list_strict(pattern) {
        Some(v) => v,
        None => return false,
    };
    let input_items = match collect_proper_list_strict(input) {
        Some(v) => v,
        None => return false,
    };
    // Detect ellipsis position. Pattern shape: (... a b c X ... ) where X ... means
    // "X repeats". We support ellipsis at the END only.
    // Find ellipsis index.
    let ellipsis_idx = pattern_items
        .iter()
        .position(|d| matches!(d, Datum::Symbol(s, _) if *s == ellipsis_sym));
    if let Some(eidx) = ellipsis_idx {
        // The pattern is: prefix..., elem_at(eidx-1), ellipsis, then we ignore trailing.
        if eidx == 0 {
            return false; // ill-formed
        }
        let fixed = &pattern_items[..eidx - 1];
        let repeat_pattern = &pattern_items[eidx - 1];
        let trailing = &pattern_items[eidx + 1..];
        if input_items.len() < fixed.len() + trailing.len() {
            return false;
        }
        // Match fixed prefix
        for (p, i) in fixed.iter().zip(input_items.iter()) {
            let outer =
                is_outer && std::ptr::eq(p as *const Datum, &pattern_items[0] as *const Datum);
            if !match_pattern(
                p,
                i,
                literals,
                ellipsis_sym,
                underscore_sym,
                outer,
                bindings,
            ) {
                return false;
            }
        }
        // Repeating section:
        let repeat_count = input_items.len() - fixed.len() - trailing.len();
        let repeat_inputs = &input_items[fixed.len()..fixed.len() + repeat_count];
        // Collect bindings for vars in repeat_pattern across all repetitions.
        let repeat_vars =
            collect_pattern_vars(repeat_pattern, literals, ellipsis_sym, underscore_sym);
        let mut repeat_bindings: std::collections::HashMap<cs_core::Symbol, Vec<Datum>> =
            std::collections::HashMap::new();
        for rv in &repeat_vars {
            repeat_bindings.insert(*rv, Vec::new());
        }
        for ri in repeat_inputs {
            let mut sub_bindings = std::collections::HashMap::new();
            if !match_pattern(
                repeat_pattern,
                ri,
                literals,
                ellipsis_sym,
                underscore_sym,
                false,
                &mut sub_bindings,
            ) {
                return false;
            }
            for rv in &repeat_vars {
                if let Some(MatchBinding::Single(d)) = sub_bindings.get(rv) {
                    repeat_bindings.get_mut(rv).unwrap().push(d.clone());
                }
            }
        }
        for (k, v) in repeat_bindings {
            bindings.insert(k, MatchBinding::Repeat(v));
        }
        // Match trailing
        let tail_inputs = &input_items[fixed.len() + repeat_count..];
        for (p, i) in trailing.iter().zip(tail_inputs.iter()) {
            if !match_pattern(
                p,
                i,
                literals,
                ellipsis_sym,
                underscore_sym,
                false,
                bindings,
            ) {
                return false;
            }
        }
        return true;
    }
    // No ellipsis: must match length exactly.
    if pattern_items.len() != input_items.len() {
        return false;
    }
    for (i, (p, inp)) in pattern_items.iter().zip(input_items.iter()).enumerate() {
        let outer = is_outer && i == 0;
        if !match_pattern(
            p,
            inp,
            literals,
            ellipsis_sym,
            underscore_sym,
            outer,
            bindings,
        ) {
            return false;
        }
    }
    true
}

/// Match `(p1 p2 ... pn . tail-pat)` against an input list.
///
/// Semantics: spine elements match positionally; the tail
/// pattern binds the remainder of the input (which may itself be
/// a list or any atom).
fn match_dotted_list_pattern(
    pspine: &[Datum],
    ptail: &Datum,
    input: &Datum,
    literals: &[cs_core::Symbol],
    ellipsis_sym: cs_core::Symbol,
    underscore_sym: cs_core::Symbol,
    is_outer: bool,
    bindings: &mut std::collections::HashMap<cs_core::Symbol, MatchBinding>,
) -> bool {
    let (ispine, itail) = match collect_pair_chain(input) {
        Some(p) => p,
        None => return false,
    };
    if ispine.len() < pspine.len() {
        return false;
    }
    // Spine match.
    for (i, (p, inp)) in pspine.iter().zip(ispine.iter()).enumerate() {
        let outer = is_outer && i == 0;
        if !match_pattern(
            p,
            inp,
            literals,
            ellipsis_sym,
            underscore_sym,
            outer,
            bindings,
        ) {
            return false;
        }
    }
    // Tail: bundle the remaining input spine + input tail back into
    // a single Datum and recurse on the tail pattern.
    let remaining = rebuild_list_with_tail(ispine[pspine.len()..].to_vec(), itail, ptail.span());
    match_pattern(
        ptail,
        &remaining,
        literals,
        ellipsis_sym,
        underscore_sym,
        false,
        bindings,
    )
}

/// Best-effort pinpoint of why a (combinator-free) syntax-rules pattern
/// failed to match `input`, for `define-syntax-parser` / `syntax-rules`
/// error messages (R6RS++ Phase 2A.4, issue #33). A deterministic
/// mirror of [`match_list_pattern`] for the common cases — top-level
/// arity and nested-shape mismatches — returning the furthest-reaching
/// divergence (by source position). Patterns it can't reason about
/// precisely (dotted tails, ellipsis) yield `None`, leaving the caller
/// to emit its generic message. Runs only on the error path.
fn diagnose_sr(
    pattern: &Datum,
    input: &Datum,
    literals: &[Symbol],
    ellipsis: Symbol,
    underscore: Symbol,
    is_outer: bool,
    syms: &SymbolTable,
) -> Option<syntax_parse::MatchError> {
    let (pitems, ptail) = collect_pair_chain(pattern)?;
    if !matches!(ptail, Datum::Null(_)) {
        return None; // dotted pattern tail — out of scope
    }
    let iitems = collect_proper_list_strict(input)?;
    if pitems
        .iter()
        .any(|d| matches!(d, Datum::Symbol(s, _) if *s == ellipsis))
    {
        return None; // ellipsis pattern — leave to the generic message
    }
    // Arity: surplus or missing arguments.
    if iitems.len() > pitems.len() {
        let extra = &iitems[pitems.len()];
        return Some(syntax_parse::MatchError {
            span: extra.span(),
            reason: "unexpected extra form".to_string(),
        });
    }
    if iitems.len() < pitems.len() {
        return Some(syntax_parse::MatchError {
            span: input.span(),
            reason: format!(
                "too few forms: this clause expects {} but got {}",
                pitems.len().saturating_sub(1),
                iitems.len().saturating_sub(1)
            ),
        });
    }
    // Same length: report the furthest element-level divergence.
    let mut best: Option<syntax_parse::MatchError> = None;
    for (idx, (p, i)) in pitems.iter().zip(iitems.iter()).enumerate() {
        if is_outer && idx == 0 {
            continue; // macro-keyword slot
        }
        if let Some(me) = diagnose_one_sr(p, i, literals, ellipsis, underscore, syms) {
            if best.as_ref().is_none_or(|b| me.span.end >= b.span.end) {
                best = Some(me);
            }
        }
    }
    best
}

/// Element-level companion to [`diagnose_sr`].
fn diagnose_one_sr(
    pat: &Datum,
    inp: &Datum,
    literals: &[Symbol],
    ellipsis: Symbol,
    underscore: Symbol,
    syms: &SymbolTable,
) -> Option<syntax_parse::MatchError> {
    match pat {
        Datum::Symbol(s, _) if *s == underscore => None,
        Datum::Symbol(s, _) if literals.contains(s) => {
            if matches!(inp, Datum::Symbol(t, _) if t == s) {
                None
            } else {
                Some(syntax_parse::MatchError {
                    span: inp.span(),
                    reason: format!("expected `{}`", syms.name(*s)),
                })
            }
        }
        Datum::Symbol(_, _) => None, // pattern variable: matches anything
        Datum::Null(_) => {
            if matches!(inp, Datum::Null(_)) {
                None
            } else {
                Some(syntax_parse::MatchError {
                    span: inp.span(),
                    reason: "expected `()`".to_string(),
                })
            }
        }
        Datum::Pair(_, _, _) => {
            if collect_proper_list_strict(inp).is_none() {
                Some(syntax_parse::MatchError {
                    span: inp.span(),
                    reason: "expected a list".to_string(),
                })
            } else {
                diagnose_sr(pat, inp, literals, ellipsis, underscore, false, syms)
            }
        }
        _ => {
            // Self-evaluating literal datum.
            if cs_core::eq::equal(&pat.to_value(), &inp.to_value()) {
                None
            } else {
                Some(syntax_parse::MatchError {
                    span: inp.span(),
                    reason: "expected a specific literal".to_string(),
                })
            }
        }
    }
}

fn collect_pattern_vars(
    pattern: &Datum,
    literals: &[cs_core::Symbol],
    ellipsis_sym: cs_core::Symbol,
    underscore_sym: cs_core::Symbol,
) -> Vec<cs_core::Symbol> {
    let mut out = Vec::new();
    collect_pattern_vars_into(pattern, literals, ellipsis_sym, underscore_sym, &mut out);
    out
}

fn collect_pattern_vars_into(
    pattern: &Datum,
    literals: &[cs_core::Symbol],
    ellipsis_sym: cs_core::Symbol,
    underscore_sym: cs_core::Symbol,
    out: &mut Vec<cs_core::Symbol>,
) {
    match pattern {
        Datum::Symbol(s, _) => {
            if *s == ellipsis_sym || *s == underscore_sym || literals.contains(s) {
                return;
            }
            if !out.contains(s) {
                out.push(*s);
            }
        }
        Datum::Pair(_, _, _) => {
            if let Some((spine, tail)) = collect_pair_chain(pattern) {
                for it in spine {
                    collect_pattern_vars_into(&it, literals, ellipsis_sym, underscore_sym, out);
                }
                if !matches!(tail, Datum::Null(_)) {
                    collect_pattern_vars_into(&tail, literals, ellipsis_sym, underscore_sym, out);
                }
            }
        }
        _ => {}
    }
}

/// Prefix used to mark template-introduced symbols during macro expansion.
/// Stripped in the hygiene post-pass; binders carrying this marker get
/// gensym-renamed to prevent capture of user-site identifiers.
const TEMPLATE_MARKER: char = '\u{E000}';

fn is_template_marked(s: cs_core::Symbol, syms: &SymbolTable) -> bool {
    syms.name(s).starts_with(TEMPLATE_MARKER)
}

fn unmark_template_symbol(s: cs_core::Symbol, syms: &mut SymbolTable) -> cs_core::Symbol {
    let name = syms.name(s).to_string();
    if let Some(stripped) = name.strip_prefix(TEMPLATE_MARKER) {
        syms.intern(stripped)
    } else {
        s
    }
}

fn mark_template_symbol(s: cs_core::Symbol, syms: &mut SymbolTable) -> cs_core::Symbol {
    let name = syms.name(s).to_string();
    if name.starts_with(TEMPLATE_MARKER) {
        s
    } else {
        let marked = format!("{}{}", TEMPLATE_MARKER, name);
        syms.intern(&marked)
    }
}

fn instantiate(
    template: &Datum,
    bindings: &std::collections::HashMap<cs_core::Symbol, MatchBinding>,
    ellipsis_sym: cs_core::Symbol,
    _gensym_counter: &mut u32,
    syms: &mut SymbolTable,
) -> Result<Datum, ExpandError> {
    match template {
        Datum::Symbol(s, span) => {
            if let Some(b) = bindings.get(s) {
                match b {
                    MatchBinding::Single(d) => Ok(d.clone()),
                    MatchBinding::Repeat(_) => {
                        // Bare repeat-var without ellipsis → use its first or error.
                        Err(ExpandError::BadSyntax {
                            what: "ellipsis variable used outside ellipsis context".into(),
                            span: *span,
                        })
                    }
                    MatchBinding::Absent => Err(ExpandError::BadSyntax {
                        what: format!(
                            "pattern variable `{}` is absent here (its ~optional/~or alternative did not match); supply #:defaults or guard its use",
                            syms.name(*s)
                        ),
                        span: *span,
                    }),
                }
            } else {
                // Template-introduced (literal) symbol: mark it so the hygiene
                // post-pass can identify it as macro-introduced.
                let marked = mark_template_symbol(*s, syms);
                Ok(Datum::Symbol(marked, *span))
            }
        }
        Datum::Pair(_, _, span) => {
            // Templates may be proper OR dotted: support both. The
            // ellipsis expansion logic operates over the spine; a
            // non-Null tail is instantiated separately and reattached
            // via rebuild_list_with_tail.
            let (items, tail_template) =
                collect_pair_chain(template).ok_or_else(|| ExpandError::BadSyntax {
                    what: "template must be a pair".into(),
                    span: *span,
                })?;
            // Process spine, expanding ellipses.
            let mut out: Vec<Datum> = Vec::new();
            let mut i = 0;
            while i < items.len() {
                let cur = &items[i];
                let next_is_ellipsis = items.get(i + 1).map_or(
                    false,
                    |d| matches!(d, Datum::Symbol(s, _) if *s == ellipsis_sym),
                );
                if next_is_ellipsis {
                    // Find ellipsis vars in `cur` and expand.
                    let repeat_vars = collect_template_repeat_vars(cur, bindings);
                    if repeat_vars.is_empty() {
                        return Err(ExpandError::BadSyntax {
                            what: "ellipsis without ellipsis variable".into(),
                            span: *span,
                        });
                    }
                    // All repeat vars must have same count.
                    let n = match bindings.get(&repeat_vars[0]) {
                        Some(MatchBinding::Repeat(vs)) => vs.len(),
                        _ => 0,
                    };
                    for k in 0..n {
                        // Build a sub-bindings by replacing each repeat var with its k'th item.
                        let mut sub_bindings = bindings.clone();
                        for rv in &repeat_vars {
                            if let Some(MatchBinding::Repeat(vs)) = bindings.get(rv) {
                                if let Some(v) = vs.get(k) {
                                    sub_bindings.insert(*rv, MatchBinding::Single(v.clone()));
                                }
                            }
                        }
                        let inst =
                            instantiate(cur, &sub_bindings, ellipsis_sym, _gensym_counter, syms)?;
                        out.push(inst);
                    }
                    i += 2; // skip the cur and the `...`
                } else {
                    out.push(instantiate(
                        cur,
                        bindings,
                        ellipsis_sym,
                        _gensym_counter,
                        syms,
                    )?);
                    i += 1;
                }
            }
            let tail = match &tail_template {
                Datum::Null(_) => tail_template,
                other => instantiate(other, bindings, ellipsis_sym, _gensym_counter, syms)?,
            };
            Ok(rebuild_list_with_tail(out, tail, *span))
        }
        _ => Ok(template.clone()),
    }
}

fn collect_template_repeat_vars(
    template: &Datum,
    bindings: &std::collections::HashMap<cs_core::Symbol, MatchBinding>,
) -> Vec<cs_core::Symbol> {
    let mut out = Vec::new();
    collect_template_repeat_vars_into(template, bindings, &mut out);
    out
}

fn collect_template_repeat_vars_into(
    template: &Datum,
    bindings: &std::collections::HashMap<cs_core::Symbol, MatchBinding>,
    out: &mut Vec<cs_core::Symbol>,
) {
    match template {
        Datum::Symbol(s, _) => {
            if let Some(MatchBinding::Repeat(_)) = bindings.get(s) {
                if !out.contains(s) {
                    out.push(*s);
                }
            }
        }
        Datum::Pair(_, _, _) => {
            if let Some(items) = collect_proper_list_strict(template) {
                for it in items {
                    collect_template_repeat_vars_into(&it, bindings, out);
                }
            }
        }
        _ => {}
    }
}

fn rebuild_list(items: Vec<Datum>, span: Span) -> Datum {
    let mut acc = Datum::Null(span);
    for item in items.into_iter().rev() {
        let s = item.span().merge(acc.span());
        acc = Datum::Pair(Rc::new(item), Rc::new(acc), s);
    }
    acc
}

/// Collect a proper list of Datums from a Datum::Pair chain. Returns None
/// for improper lists.
pub(crate) fn collect_proper_list_strict(d: &Datum) -> Option<Vec<Datum>> {
    let mut out = Vec::new();
    let mut cur = d.clone();
    loop {
        match cur {
            Datum::Null(_) => return Some(out),
            Datum::Pair(car, cdr, _) => {
                out.push((*car).clone());
                cur = (*cdr).clone();
            }
            _ => return None,
        }
    }
}

/// Render a library name spec as `(seg seg seg)` for diagnostics.
fn format_library_name(name: &[Symbol], syms: &SymbolTable) -> String {
    let segs: Vec<&str> = name.iter().map(|s| syms.name(*s)).collect();
    format!("({})", segs.join(" "))
}

/// Build a parse-error ExpandError for cross-file library loads.
/// Factored out so the cache-hit and cache-miss paths share the
/// same diagnostic shape.
fn parse_err(
    errs: Vec<cs_parse::ReaderError>,
    name: &[Symbol],
    syms: &SymbolTable,
    span: Span,
) -> ExpandError {
    let e = errs.into_iter().next().unwrap();
    ExpandError::BadSyntax {
        what: format!(
            "library {}: parse error: {}",
            format_library_name(name, syms),
            e.message()
        ),
        span,
    }
}

/// Walks a Pair chain returning the spine of car-elements and the
/// cdr of the last pair. For a proper list `(a b c)` the tail is
/// `Datum::Null`. For an improper list `(a b . c)` the tail is the
/// atom `c`. Returns None if `d` is not a Pair at all.
pub(crate) fn collect_pair_chain(d: &Datum) -> Option<(Vec<Datum>, Datum)> {
    let mut out = Vec::new();
    let mut cur = d.clone();
    loop {
        match cur {
            Datum::Pair(car, cdr, _) => {
                out.push((*car).clone());
                cur = (*cdr).clone();
            }
            other => {
                if out.is_empty() {
                    return None;
                }
                return Some((out, other));
            }
        }
    }
}

/// Rebuild a list from a spine + optional dotted tail. With
/// `tail = Datum::Null(_)` this produces a proper list; with any
/// other tail it produces a dotted-pair chain ending in `tail`.
fn rebuild_list_with_tail(items: Vec<Datum>, tail: Datum, _span: Span) -> Datum {
    let mut acc = tail;
    for item in items.into_iter().rev() {
        let s = item.span().merge(acc.span());
        acc = Datum::Pair(Rc::new(item), Rc::new(acc), s);
    }
    acc
}

/// Build a proper-list Datum from a vector of items at `span`.
/// Used by the syntax-case desugarer to assemble synthesized
/// forms before handing them back through `Expander::expand`.
fn mk_list(items: Vec<Datum>, span: Span) -> Datum {
    let mut acc = Datum::Null(span);
    for item in items.into_iter().rev() {
        acc = Datum::Pair(Rc::new(item), Rc::new(acc), span);
    }
    acc
}

/// Phase 2A.1 helper: walk a `define-syntax-parser` pattern,
/// strip `:class` annotations from each symbol, and record the
/// stripped symbol + class-name pairs into `class_checks` so
/// the caller can wrap the template body in class-predicate
/// `if`s.
///
/// A pattern symbol like `name:class` is split into:
///   - stripped symbol `name`  (binds the pvar in the syntax-rules pattern)
///   - class name `"class"`    (looked up against the built-in predicate map)
///
/// Plain symbols without a colon pass through untouched and
/// aren't recorded. Compound datums (pairs, vectors) recurse;
/// non-symbol atoms pass through.
///
/// Edge cases:
///   * `_:class` and leading/trailing `:` patterns are left
///     untouched (no annotation extracted)
///   * Multiple colons: split at the first `:` only
fn strip_class_annotations(
    pat: &Datum,
    class_checks: &mut Vec<(Symbol, String)>,
    syms: &mut SymbolTable,
) -> Datum {
    match pat {
        Datum::Symbol(s, span) => {
            let name = syms.name(*s).to_string();
            // Keyword identifiers (`#:foo`, including the `~optional`
            // `#:defaults` marker and `#:kw` pattern literals) keep
            // their internal colon — never treat it as a `:class`
            // annotation. (Phase 2A.3, issue #31.)
            if name.starts_with('#') {
                return pat.clone();
            }
            // Find the first colon NOT at position 0 (so `:class`
            // alone or `_` alone passes through).
            if let Some(colon_idx) = name.find(':').filter(|&i| i > 0 && i < name.len() - 1) {
                let (pvar_name, _) = name.split_at(colon_idx);
                let class_name = &name[colon_idx + 1..];
                let pvar_sym = syms.intern(pvar_name);
                class_checks.push((pvar_sym, class_name.to_string()));
                Datum::Symbol(pvar_sym, *span)
            } else {
                pat.clone()
            }
        }
        Datum::Pair(car, cdr, span) => {
            let new_car = strip_class_annotations(car, class_checks, syms);
            let new_cdr = strip_class_annotations(cdr, class_checks, syms);
            Datum::Pair(Rc::new(new_car), Rc::new(new_cdr), *span)
        }
        Datum::Vector(items, span) => {
            let new_items: Vec<Datum> = items
                .iter()
                .map(|i| strip_class_annotations(i, class_checks, syms))
                .collect();
            Datum::Vector(new_items, *span)
        }
        _ => pat.clone(),
    }
}

/// Compile a syntax-case pattern into a boolean-valued test
/// `Datum` over the key expression `key`, while collecting any
/// pattern-variable bindings (each `(pvar, extractor-expr)`)
/// into `pvars_out`.
///
/// Symbols in `literals` match by name (compiled to `eq?`).
/// Other symbols become pattern variables that bind the matched
/// sub-value. `_` is a wildcard (no binding, no constraint).
///
/// Returns the test-datum; the caller composes it into the
/// outer `cond` clause and emits a `let` over `pvars_out` to
/// bind the variables in the template body.
fn compile_sc_pattern(
    pat: &Datum,
    key: Datum,
    literals: &[Symbol],
    pvars_out: &mut Vec<(Symbol, u32, Datum)>,
    syms: &mut SymbolTable,
    kw: &Keywords,
) -> Result<Datum, ExpandError> {
    let span = pat.span();
    let mk_sym =
        |name: &str, syms: &mut SymbolTable| -> Datum { Datum::Symbol(syms.intern(name), span) };
    match pat {
        Datum::Symbol(s, _) if *s == kw.underscore => Ok(Datum::Boolean(true, span)),
        Datum::Symbol(s, _) if literals.contains(s) => {
            let eq_call = mk_list(
                vec![
                    mk_sym("eq?", syms),
                    key,
                    mk_list(
                        vec![Datum::Symbol(kw.quote, span), Datum::Symbol(*s, span)],
                        span,
                    ),
                ],
                span,
            );
            Ok(eq_call)
        }
        Datum::Symbol(s, _) => {
            // Pattern variable at depth 0: binds `s` to `key`.
            pvars_out.push((*s, 0, key));
            Ok(Datum::Boolean(true, span))
        }
        Datum::Null(_) => Ok(mk_list(vec![mk_sym("null?", syms), key], span)),
        Datum::Boolean(_, _)
        | Datum::Number(_, _)
        | Datum::Character(_, _)
        | Datum::String(_, _) => {
            let lit_quote = mk_list(vec![Datum::Symbol(kw.quote, span), pat.clone()], span);
            Ok(mk_list(vec![mk_sym("equal?", syms), key, lit_quote], span))
        }
        Datum::Pair(_, _, _) => {
            // Detect a trailing `(prefix... sub ...)` form. `sub` may
            // be either:
            //   * a bare symbol pvar -- Iter C2's minimal case
            //   * a proper list of bare-symbol pvars (Iter C3
            //     compound sub-pattern)
            // Anything more complex (nested ellipsis, dotted sub,
            // literals inside sub) is rejected with a pointer.
            if let Some(items) = collect_proper_list_strict(pat) {
                let n = items.len();
                if n >= 2 {
                    if let Datum::Symbol(last, _) = &items[n - 1] {
                        if *last == kw.ellipsis {
                            let sub = &items[n - 2];
                            // Walk the prefix (items[0..n-2]).
                            let mut tests: Vec<Datum> = Vec::new();
                            let mut walking_key = key.clone();
                            for prefix_item in &items[..n - 2] {
                                let pair_test =
                                    mk_list(vec![mk_sym("pair?", syms), walking_key.clone()], span);
                                let car_key =
                                    mk_list(vec![mk_sym("car", syms), walking_key.clone()], span);
                                let inner_test = compile_sc_pattern(
                                    prefix_item,
                                    car_key,
                                    literals,
                                    pvars_out,
                                    syms,
                                    kw,
                                )?;
                                tests.push(pair_test);
                                tests.push(inner_test);
                                walking_key = mk_list(vec![mk_sym("cdr", syms), walking_key], span);
                            }

                            // Bare-pvar sub: Iter C2 case.
                            if let Datum::Symbol(s, _) = sub {
                                if *s != kw.underscore && !literals.contains(s) {
                                    pvars_out.push((*s, 1, walking_key.clone()));
                                    let list_test =
                                        mk_list(vec![mk_sym("list?", syms), walking_key], span);
                                    tests.push(list_test);
                                    let mut all = vec![Datum::Symbol(kw.and, span)];
                                    all.extend(tests);
                                    return Ok(mk_list(all, span));
                                }
                            }

                            // Nested ellipsis detection (Iter C6/C7).
                            // If sub itself has the shape `(inner-pat ...)`
                            // — a proper-list of length 2 ending in `...`
                            // — that's a nested ellipsis section.
                            // General handling: recursively compile sub
                            // as a standalone ellipsis pattern against a
                            // synthetic `__sc-inner-elem__` key. The
                            // resulting per-element test/extractors get
                            // wrapped in `every` / `map` to lift them to
                            // the outer ellipsis. Each inner pvar's
                            // depth bumps by 1 (e.g. depth-1 -> depth-2).
                            //
                            // This subsumes Iter C6's bare-pvar
                            // special-case and handles Iter C7
                            // compound/prefixed inner forms uniformly:
                            //   ((p …) …)          -> p at depth 2
                            //   ((kw p …) …)       -> p at depth 2
                            //   (((a b) …) …)      -> a,b at depth 2
                            //   (((a b) c …) …)    -> a,b at depth 2, c at depth 2
                            if let Some(sub_items) = collect_proper_list_strict(sub) {
                                if sub_items.len() >= 2 {
                                    if let Datum::Symbol(inner_ellipsis, _) =
                                        &sub_items[sub_items.len() - 1]
                                    {
                                        if *inner_ellipsis == kw.ellipsis {
                                            let inner_elem_sym = syms.intern("__sc-inner-elem__");
                                            let inner_key = Datum::Symbol(inner_elem_sym, span);
                                            let mut inner_pvars: Vec<(Symbol, u32, Datum)> =
                                                Vec::new();
                                            let inner_test = compile_sc_pattern(
                                                sub,
                                                inner_key,
                                                literals,
                                                &mut inner_pvars,
                                                syms,
                                                kw,
                                            )?;

                                            // Outer shape: list? walking-key
                                            // + (every (lambda (e) <inner-test>) walking-key)
                                            let inner_test_lambda = mk_list(
                                                vec![
                                                    Datum::Symbol(kw.lambda, span),
                                                    mk_list(
                                                        vec![Datum::Symbol(inner_elem_sym, span)],
                                                        span,
                                                    ),
                                                    inner_test,
                                                ],
                                                span,
                                            );
                                            let list_test = mk_list(
                                                vec![mk_sym("list?", syms), walking_key.clone()],
                                                span,
                                            );
                                            let every_test = mk_list(
                                                vec![
                                                    mk_sym("every", syms),
                                                    inner_test_lambda,
                                                    walking_key.clone(),
                                                ],
                                                span,
                                            );
                                            tests.push(list_test);
                                            tests.push(every_test);

                                            // For each inner pvar (name, inner-depth,
                                            // inner-extractor), bump depth by 1 and
                                            // wrap extractor in
                                            // `(map (lambda (__sc-inner-elem__) <extr>) walking-key)`.
                                            for (pv, inner_depth, inner_extractor) in inner_pvars {
                                                let extractor_lambda = mk_list(
                                                    vec![
                                                        Datum::Symbol(kw.lambda, span),
                                                        mk_list(
                                                            vec![Datum::Symbol(
                                                                inner_elem_sym,
                                                                span,
                                                            )],
                                                            span,
                                                        ),
                                                        inner_extractor,
                                                    ],
                                                    span,
                                                );
                                                let map_call = mk_list(
                                                    vec![
                                                        mk_sym("map", syms),
                                                        extractor_lambda,
                                                        walking_key.clone(),
                                                    ],
                                                    span,
                                                );
                                                pvars_out.push((pv, inner_depth + 1, map_call));
                                            }

                                            let mut all = vec![Datum::Symbol(kw.and, span)];
                                            all.extend(tests);
                                            return Ok(mk_list(all, span));
                                        }
                                    }
                                }
                            }

                            // Compound sub: walk recursively to
                            // collect structural constraints + pvar
                            // accessors. Handles arbitrarily nested
                            // compound sub-patterns (Iter C5);
                            // rejects nested ellipsis with a clear
                            // pointer to a future iter.
                            let elem_sym = syms.intern("__sc-elem__");
                            let mut sub_constraints: Vec<Datum> = Vec::new();
                            let mut sub_pvars: Vec<(Symbol, Datum)> = Vec::new();
                            if walk_sub_pattern(
                                sub,
                                Datum::Symbol(elem_sym, span),
                                literals,
                                kw,
                                &mut sub_constraints,
                                &mut sub_pvars,
                                syms,
                            )
                            .is_err()
                            {
                                return Err(ExpandError::BadSyntax {
                                    what: "syntax-case ellipsis: sub-pattern contains nested ellipsis or vector pattern -- lands in a future iter".into(),
                                    span,
                                });
                            }

                            // Build the shape lambda: (lambda (e) (and <c1> <c2> ...))
                            // If there are zero constraints (the sub is
                            // a single bare pvar), the lambda just returns #t.
                            let mut shape_body_parts: Vec<Datum> =
                                vec![Datum::Symbol(kw.and, span)];
                            shape_body_parts.extend(sub_constraints);
                            let shape_body = mk_list(shape_body_parts, span);
                            let shape_lambda = mk_list(
                                vec![
                                    Datum::Symbol(kw.lambda, span),
                                    mk_list(vec![Datum::Symbol(elem_sym, span)], span),
                                    shape_body,
                                ],
                                span,
                            );

                            let list_test =
                                mk_list(vec![mk_sym("list?", syms), walking_key.clone()], span);
                            let every_test = mk_list(
                                vec![mk_sym("every", syms), shape_lambda, walking_key.clone()],
                                span,
                            );
                            tests.push(list_test);
                            tests.push(every_test);

                            // Bind each pvar's depth-1 list via
                            // `(map (lambda (e) <accessor>) walking-key)`.
                            for (pv, accessor) in sub_pvars {
                                let extractor_lambda = mk_list(
                                    vec![
                                        Datum::Symbol(kw.lambda, span),
                                        mk_list(vec![Datum::Symbol(elem_sym, span)], span),
                                        accessor,
                                    ],
                                    span,
                                );
                                let map_call = mk_list(
                                    vec![
                                        mk_sym("map", syms),
                                        extractor_lambda,
                                        walking_key.clone(),
                                    ],
                                    span,
                                );
                                pvars_out.push((pv, 1, map_call));
                            }

                            let mut all = vec![Datum::Symbol(kw.and, span)];
                            all.extend(tests);
                            return Ok(mk_list(all, span));
                        }
                    }
                }
            }
            // (p1 . p2) or fixed-arity list: pair-spine traversal.
            let car_pat = match pat {
                Datum::Pair(c, _, _) => (**c).clone(),
                _ => unreachable!(),
            };
            let cdr_pat = match pat {
                Datum::Pair(_, c, _) => (**c).clone(),
                _ => unreachable!(),
            };
            let pair_test = mk_list(vec![mk_sym("pair?", syms), key.clone()], span);
            let car_key = mk_list(vec![mk_sym("car", syms), key.clone()], span);
            let cdr_key = mk_list(vec![mk_sym("cdr", syms), key], span);
            let car_test = compile_sc_pattern(&car_pat, car_key, literals, pvars_out, syms, kw)?;
            let cdr_test = compile_sc_pattern(&cdr_pat, cdr_key, literals, pvars_out, syms, kw)?;
            Ok(mk_list(
                vec![Datum::Symbol(kw.and, span), pair_test, car_test, cdr_test],
                span,
            ))
        }
        Datum::Vector(_, _) | Datum::ByteVector(_, _) => Err(ExpandError::BadSyntax {
            what: "syntax-case vector patterns land in a future iter".into(),
            span,
        }),
    }
}

/// Rewrite a `quasisyntax` template so the existing
/// `expand_quasiquote` engine can process it: swap
/// `quasisyntax` / `unsyntax` / `unsyntax-splicing` head symbols
/// for `quasiquote` / `unquote` / `unquote-splicing` everywhere
/// in the template. Only head positions of pair forms are
/// rewritten; the rest of the structure is preserved verbatim.
fn rewrite_qs_to_qq(d: &Datum, kw: &Keywords) -> Datum {
    match d {
        Datum::Pair(head, tail, span) => {
            // If head is a recognized qs-keyword symbol, swap it.
            let new_head = match &**head {
                Datum::Symbol(s, hsp) if *s == kw.quasisyntax => Datum::Symbol(kw.quasiquote, *hsp),
                Datum::Symbol(s, hsp) if *s == kw.unsyntax => Datum::Symbol(kw.unquote, *hsp),
                Datum::Symbol(s, hsp) if *s == kw.unsyntax_splicing => {
                    Datum::Symbol(kw.unquote_splicing, *hsp)
                }
                other => rewrite_qs_to_qq(other, kw),
            };
            let new_tail = rewrite_qs_to_qq(tail, kw);
            Datum::Pair(Rc::new(new_head), Rc::new(new_tail), *span)
        }
        Datum::Vector(items, span) => {
            let new_items: Vec<Datum> = items.iter().map(|i| rewrite_qs_to_qq(i, kw)).collect();
            Datum::Vector(new_items, *span)
        }
        _ => d.clone(),
    }
}

/// Recursive walker for a compound sub-pattern under ellipsis.
/// Builds (a) a list of structural constraint expressions, each
/// applied to a free variable representing one outer-list
/// element, and (b) a list of `(pvar, accessor-expr)` bindings
/// where the accessor extracts the pvar's value from that
/// element.
///
/// Supports arbitrarily nested compound sub-patterns:
/// `((a (b c)) …)`, `((a (b . c)) …)`, `((kw (a b)) …)` with
/// literal kw, `((a _) …)` with wildcard, mixed nesting.
///
/// Nested ellipsis (`((p …) …)`) is NOT supported -- detected
/// at the outer Pair walk and signaled via an Err; the caller
/// surfaces it with a "future iter" pointer.
fn walk_sub_pattern(
    sub: &Datum,
    accessor: Datum,
    literals: &[Symbol],
    kw: &Keywords,
    constraints: &mut Vec<Datum>,
    pvars: &mut Vec<(Symbol, Datum)>,
    syms: &mut SymbolTable,
) -> Result<(), ()> {
    let span = sub.span();
    let mk_sym =
        |name: &str, syms: &mut SymbolTable| -> Datum { Datum::Symbol(syms.intern(name), span) };
    match sub {
        Datum::Symbol(s, _) if *s == kw.underscore => Ok(()),
        Datum::Symbol(s, _) if *s == kw.ellipsis => Err(()),
        Datum::Symbol(s, _) if literals.contains(s) => {
            let quoted = mk_list(
                vec![Datum::Symbol(kw.quote, span), Datum::Symbol(*s, span)],
                span,
            );
            constraints.push(mk_list(vec![mk_sym("eq?", syms), accessor, quoted], span));
            Ok(())
        }
        Datum::Symbol(s, _) => {
            pvars.push((*s, accessor));
            Ok(())
        }
        Datum::Null(_) => {
            constraints.push(mk_list(vec![mk_sym("null?", syms), accessor], span));
            Ok(())
        }
        Datum::Boolean(_, _)
        | Datum::Number(_, _)
        | Datum::Character(_, _)
        | Datum::String(_, _) => {
            let quoted = mk_list(vec![Datum::Symbol(kw.quote, span), sub.clone()], span);
            constraints.push(mk_list(
                vec![mk_sym("equal?", syms), accessor, quoted],
                span,
            ));
            Ok(())
        }
        Datum::Pair(_, _, _) => {
            // Reject nested ellipsis: any spine position with the
            // `...` symbol means an inner `(p …)` form, which would
            // need a per-element matcher loop.
            if let Some(items) = collect_proper_list_strict(sub) {
                if items.len() >= 2 {
                    if let Datum::Symbol(last, _) = &items[items.len() - 1] {
                        if *last == kw.ellipsis {
                            return Err(());
                        }
                    }
                }
            }
            // Otherwise: descend into car + cdr with adjusted accessors.
            constraints.push(mk_list(vec![mk_sym("pair?", syms), accessor.clone()], span));
            let car_acc = mk_list(vec![mk_sym("car", syms), accessor.clone()], span);
            let cdr_acc = mk_list(vec![mk_sym("cdr", syms), accessor], span);
            let (car_d, cdr_d) = match sub {
                Datum::Pair(a, b, _) => ((**a).clone(), (**b).clone()),
                _ => unreachable!(),
            };
            walk_sub_pattern(&car_d, car_acc, literals, kw, constraints, pvars, syms)?;
            walk_sub_pattern(&cdr_d, cdr_acc, literals, kw, constraints, pvars, syms)?;
            Ok(())
        }
        Datum::Vector(_, _) | Datum::ByteVector(_, _) => Err(()),
    }
}

/// Walk a template datum and collect every symbol that's a pvar
/// at depth >= 1 in `pvars`. Used by the zip-map case of
/// `compile_syntax_template` to figure out which lists to map
/// over in parallel. Skips into `(quote X)` (literal datum).
fn collect_depth1_pvars(t: &Datum, pvars: &[(Symbol, u32)], out: &mut Vec<Symbol>) {
    match t {
        Datum::Symbol(s, _) => {
            if pvars.iter().any(|(p, d)| *p == *s && *d >= 1) && !out.contains(s) {
                out.push(*s);
            }
        }
        Datum::Pair(head, tail, _) => {
            // Skip into (quote X) -- never substitutes.
            if let Datum::Symbol(_, _) = &**head {
                // Don't recognize quote keyword here without
                // access to Keywords; the worst that happens is
                // we collect a pvar reference that won't trip
                // (quote forms quoting a pvar name aren't a
                // meaningful template construct).
            }
            collect_depth1_pvars(head, pvars, out);
            collect_depth1_pvars(tail, pvars, out);
        }
        Datum::Vector(items, _) => {
            for i in items {
                collect_depth1_pvars(i, pvars, out);
            }
        }
        _ => {}
    }
}

/// Compile a `(syntax T)` template into an expression that, when
/// evaluated, reproduces T with pattern-variable substitutions.
/// `pvars` carries `(name, depth)`: depth 0 is a scalar pvar that
/// can be referenced anywhere; depth >= 1 must appear inside a
/// matching number of `...` template positions or it's an error
/// (today we just emit the variable reference and let the runtime
/// trip; Iter C3+ formalizes this).
fn compile_syntax_template(
    t: &Datum,
    pvars: &[(Symbol, u32)],
    syms: &mut SymbolTable,
    kw: &Keywords,
    mark_expr: &Datum,
) -> Datum {
    let span = t.span();
    let is_pvar = |s: Symbol| pvars.iter().any(|(p, _)| *p == s);
    match t {
        Datum::Symbol(s, _) => {
            if is_pvar(*s) {
                Datum::Symbol(*s, span)
            } else {
                // Phase 1.5 Iter C: template-introduced
                // identifiers wrap as a hygienic Identifier
                // carrying the surrounding (syntax-case ...)
                // form's mark, so two macro call sites of the
                // same template produce distinguishable values.
                // For standalone `(syntax T)` outside any
                // syntax-case body, `mark_expr` evaluates to 0
                // (unmarked) -- see the caller in
                // expand_syntax_form.
                mk_list(
                    vec![
                        Datum::Symbol(syms.intern("make-identifier"), span),
                        mk_list(
                            vec![Datum::Symbol(kw.quote, span), Datum::Symbol(*s, span)],
                            span,
                        ),
                        mark_expr.clone(),
                    ],
                    span,
                )
            }
        }
        Datum::Null(_) => mk_list(vec![Datum::Symbol(kw.quote, span), t.clone()], span),
        Datum::Pair(_, _, _) => {
            // Detect a trailing `(prefix... sub ...)` template
            // (matches the shape of the corresponding ellipsis
            // pattern). Two cases for `sub`:
            //   * a bare pvar at depth >= 1 -- splice its list
            //     (Iter C2 minimal case)
            //   * a compound template whose pvars are all
            //     depth >= 1 -- zip-map (Iter C3)
            if let Some(items) = collect_proper_list_strict(t) {
                let n = items.len();
                if n >= 2 {
                    if let Datum::Symbol(last, _) = &items[n - 1] {
                        if *last == kw.ellipsis {
                            let sub = &items[n - 2];

                            // Case 1: sub is a single pvar at depth>=1.
                            if let Datum::Symbol(s, _) = sub {
                                if is_pvar(*s) {
                                    // Splice: (cons 'p1 (cons 'p2 ... (cons 'pK sub)))
                                    let mut acc: Datum = Datum::Symbol(*s, span);
                                    for prefix in items[..n - 2].iter().rev() {
                                        let car_expr = compile_syntax_template(
                                            prefix, pvars, syms, kw, mark_expr,
                                        );
                                        acc = mk_list(
                                            vec![
                                                Datum::Symbol(syms.intern("cons"), span),
                                                car_expr,
                                                acc,
                                            ],
                                            span,
                                        );
                                    }
                                    return acc;
                                }
                            }

                            // Case 2: sub is a compound. Collect all
                            // depth>=1 pvars referenced inside sub.
                            // For each, the map call needs that pvar's
                            // depth-1 list as one of its inputs. The
                            // inner template re-binds them at depth 0
                            // (since the lambda's params are scalars).
                            let mut depth1_pvars: Vec<Symbol> = Vec::new();
                            collect_depth1_pvars(sub, pvars, &mut depth1_pvars);
                            if depth1_pvars.is_empty() {
                                // No depth>=1 pvars inside sub means
                                // there's nothing to zip over -- this
                                // would loop forever / produce empty.
                                // Reject explicitly.
                                return mk_list(
                                    vec![
                                        Datum::Symbol(syms.intern("error"), span),
                                        mk_list(
                                            vec![
                                                Datum::Symbol(kw.quote, span),
                                                Datum::Symbol(
                                                    syms.intern("syntax-template"),
                                                    span,
                                                ),
                                            ],
                                            span,
                                        ),
                                        Datum::String(
                                            Rc::new(
                                                "no depth>=1 pattern variable inside `...` template"
                                                    .to_string(),
                                            ),
                                            span,
                                        ),
                                    ],
                                    span,
                                );
                            }
                            // Inner template: each ellipsis layer
                            // drops one level of depth for the pvars
                            // referenced inside. A depth-1 pvar
                            // becomes depth-0 (scalar in the lambda
                            // body); a depth-2 pvar becomes depth-1
                            // (still needs another nested ellipsis to
                            // fully unwrap). Other pvars pass through
                            // unchanged.
                            let inner_pvars: Vec<(Symbol, u32)> = pvars
                                .iter()
                                .map(|(p, d)| {
                                    if depth1_pvars.contains(p) && *d > 0 {
                                        (*p, *d - 1)
                                    } else {
                                        (*p, *d)
                                    }
                                })
                                .collect();
                            let inner_expr =
                                compile_syntax_template(sub, &inner_pvars, syms, kw, mark_expr);
                            // Build `(map (lambda (p1 p2 ... pK) <inner>) p1-list ... pK-list)`.
                            let lambda_params = mk_list(
                                depth1_pvars
                                    .iter()
                                    .map(|p| Datum::Symbol(*p, span))
                                    .collect(),
                                span,
                            );
                            let lambda_form = mk_list(
                                vec![Datum::Symbol(kw.lambda, span), lambda_params, inner_expr],
                                span,
                            );
                            let mut map_call =
                                vec![Datum::Symbol(syms.intern("map"), span), lambda_form];
                            for p in &depth1_pvars {
                                map_call.push(Datum::Symbol(*p, span));
                            }
                            let mapped = mk_list(map_call, span);

                            // Splice mapped result into prefix:
                            // (cons 'p1 (cons 'p2 ... (cons 'pK mapped)))
                            let mut acc = mapped;
                            for prefix in items[..n - 2].iter().rev() {
                                let car_expr =
                                    compile_syntax_template(prefix, pvars, syms, kw, mark_expr);
                                acc = mk_list(
                                    vec![Datum::Symbol(syms.intern("cons"), span), car_expr, acc],
                                    span,
                                );
                            }
                            return acc;
                        }
                    }
                }
            }
            // Compose `(cons <T car> <T cdr>)` recursively.
            let (car, cdr) = match t {
                Datum::Pair(a, b, _) => ((**a).clone(), (**b).clone()),
                _ => unreachable!(),
            };
            let car_expr = compile_syntax_template(&car, pvars, syms, kw, mark_expr);
            let cdr_expr = compile_syntax_template(&cdr, pvars, syms, kw, mark_expr);
            mk_list(
                vec![Datum::Symbol(syms.intern("cons"), span), car_expr, cdr_expr],
                span,
            )
        }
        // Self-quoting atoms (numbers, strings, chars, bools)
        // already evaluate to themselves; emit unchanged.
        _ => t.clone(),
    }
}

/// Returns `(head, tail_items)` if `d` is a proper list of length ≥ 1.
fn collect_list(d: &Datum) -> Option<(Rc<Datum>, Vec<Datum>)> {
    let (head, mut cur) = match d {
        Datum::Pair(car, cdr, _) => (car.clone(), cdr.clone()),
        _ => return None,
    };
    let mut tail = Vec::new();
    loop {
        match &*cur {
            Datum::Null(_) => return Some((head, tail)),
            Datum::Pair(car, cdr, _) => {
                tail.push((**car).clone());
                cur = cdr.clone();
            }
            _ => return None,
        }
    }
}

fn list_head(d: &Datum) -> Option<(Rc<Datum>, Vec<Datum>)> {
    collect_list(d)
}

fn parse_bindings(d: &Datum) -> Result<Vec<(Symbol, Datum)>, ExpandError> {
    let (first, rest) = match d {
        Datum::Null(_) => return Ok(Vec::new()),
        Datum::Pair(_, _, _) => collect_list(d).ok_or(ExpandError::BadSyntax {
            what: "bindings must be a proper list".into(),
            span: d.span(),
        })?,
        _ => {
            return Err(ExpandError::BadSyntax {
                what: "bindings must be a list".into(),
                span: d.span(),
            });
        }
    };
    let mut all: Vec<Datum> = std::iter::once((*first).clone()).chain(rest).collect();
    // If d started as null, all is empty already
    if matches!(d, Datum::Null(_)) {
        all = Vec::new();
    }
    let mut out = Vec::with_capacity(all.len());
    for b in &all {
        let (n, vs) = collect_list(b).ok_or(ExpandError::BadSyntax {
            what: "binding must be (name expr)".into(),
            span: b.span(),
        })?;
        if vs.len() != 1 {
            return Err(ExpandError::BadSyntax {
                what: "binding must be (name expr)".into(),
                span: b.span(),
            });
        }
        let name = match &*n {
            Datum::Symbol(s, _) => *s,
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "binding name must be symbol".into(),
                    span: n.span(),
                });
            }
        };
        out.push((name, vs.into_iter().next().unwrap()));
    }
    Ok(out)
}

fn parse_case_lambda_formals(d: &Datum) -> Result<(Params, bool), ExpandError> {
    let params = parse_lambda_params(d)?;
    let has_rest = params.rest.is_some();
    Ok((params, has_rest))
}

fn parse_lambda_params(d: &Datum) -> Result<Params, ExpandError> {
    match d {
        Datum::Null(_) => Ok(Params::fixed(Vec::new())),
        Datum::Symbol(s, _) => Ok(Params::variadic(Vec::new(), *s)),
        Datum::Pair(_, _, _) => {
            // Walk through, allowing dotted tail.
            let mut fixed = Vec::new();
            let mut cur = d.clone();
            loop {
                match cur {
                    Datum::Pair(car, cdr, _) => {
                        match &*car {
                            Datum::Symbol(s, _) => fixed.push(*s),
                            _ => {
                                return Err(ExpandError::BadSyntax {
                                    what: "lambda param must be a symbol".into(),
                                    span: car.span(),
                                });
                            }
                        }
                        cur = (*cdr).clone();
                    }
                    Datum::Null(_) => return Ok(Params::fixed(fixed)),
                    Datum::Symbol(s, _) => return Ok(Params::variadic(fixed, s)),
                    other => {
                        return Err(ExpandError::BadSyntax {
                            what: "lambda param tail invalid".into(),
                            span: other.span(),
                        });
                    }
                }
            }
        }
        other => Err(ExpandError::BadSyntax {
            what: "lambda params must be a list or symbol".into(),
            span: other.span(),
        }),
    }
}

fn build_params_from_datums(items: &[Datum]) -> Result<Params, ExpandError> {
    let mut fixed = Vec::with_capacity(items.len());
    for d in items {
        match d {
            Datum::Symbol(s, _) => fixed.push(*s),
            _ => {
                return Err(ExpandError::BadSyntax {
                    what: "param must be symbol".into(),
                    span: d.span(),
                });
            }
        }
    }
    Ok(Params::fixed(fixed))
}

/// Like `build_params_from_datums` but accepts a single datum that can be
/// any of the lambda formals shapes: a proper list `(a b c)`, an improper
/// list `(a b . rest)`, a bare symbol `rest`, or `()` (empty fixed).
/// Used by `define-values` where the formals appear as one form.
fn build_params_from_datums_loose(d: &Datum) -> Result<Params, ExpandError> {
    let mut fixed: Vec<Symbol> = Vec::new();
    let mut rest: Option<Symbol> = None;
    let mut cur = d.clone();
    loop {
        match cur {
            Datum::Null(_) => break,
            Datum::Symbol(s, _) => {
                rest = Some(s);
                break;
            }
            Datum::Pair(car, cdr, _) => {
                match &*car {
                    Datum::Symbol(s, _) => fixed.push(*s),
                    other => {
                        return Err(ExpandError::BadSyntax {
                            what: "formals: param must be a symbol".into(),
                            span: other.span(),
                        });
                    }
                }
                cur = (*cdr).clone();
            }
            other => {
                return Err(ExpandError::BadSyntax {
                    what: "formals: must be a list, dotted pair, or symbol".into(),
                    span: other.span(),
                });
            }
        }
    }
    Ok(Params { fixed, rest })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cs_diag::FileId;

    fn expand_str(src: &str) -> (CoreExpr, SymbolTable) {
        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), src, &mut syms).unwrap();
        let mut exp = Expander::new(&mut syms, &mut macros);
        let e = exp.expand_program(&data).unwrap();
        // Drop expander to release borrow.
        drop(exp);
        (e, syms)
    }

    #[test]
    fn expand_const() {
        let (e, _) = expand_str("42");
        match e {
            CoreExpr::Begin { exprs, .. } => match &exprs[0] {
                CoreExpr::Const {
                    value: Value::Number(n),
                    ..
                } => {
                    assert!(matches!(n, cs_core::Number::Fixnum(42)));
                }
                _ => panic!("expected const"),
            },
            _ => panic!("expected begin"),
        }
    }

    #[test]
    fn expand_application() {
        let (_e, _) = expand_str("(+ 1 2)");
    }

    #[test]
    fn expand_if() {
        let (_e, _) = expand_str("(if #t 1 2)");
    }

    #[test]
    fn expand_lambda_and_let() {
        let (_e, _) = expand_str("((lambda (x) (+ x 1)) 41)");
        let (_e2, _) = expand_str("(let ((x 1) (y 2)) (+ x y))");
    }

    #[test]
    fn expand_define_fn_sugar() {
        let (_e, _) = expand_str("(define (square x) (* x x)) (square 5)");
    }

    fn try_expand(src: &str) -> Result<CoreExpr, ExpandError> {
        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), src, &mut syms).unwrap();
        let mut exp = Expander::new(&mut syms, &mut macros);
        let r = exp.expand_program(&data);
        drop(exp);
        r
    }

    #[test]
    fn syntax_error_fires_on_top_level() {
        let r = try_expand("(syntax-error \"boom\")");
        assert!(matches!(r, Err(ExpandError::BadSyntax { .. })));
        if let Err(ExpandError::BadSyntax { what, .. }) = r {
            assert!(what.contains("boom"), "expected 'boom' in: {}", what);
        }
    }

    #[test]
    fn syntax_error_with_irritants_includes_them() {
        let r = try_expand("(syntax-error \"bad form\" foo bar)");
        if let Err(ExpandError::BadSyntax { what, .. }) = r {
            assert!(what.contains("bad form"), "msg in: {}", what);
        } else {
            panic!("expected BadSyntax");
        }
    }

    #[test]
    fn syntax_error_in_unmatched_rule_doesnt_fire() {
        let r = try_expand(
            "(define-syntax foo (syntax-rules () \
             ((_ x) x) \
             ((_ x y) (syntax-error \"two-arg\")))) \
             (foo 1)",
        );
        assert!(r.is_ok(), "matched-branch should not trigger syntax-error");
    }

    #[test]
    fn syntax_error_in_matched_rule_fires() {
        let r = try_expand(
            "(define-syntax foo (syntax-rules () \
             ((_ x) x) \
             ((_ x y) (syntax-error \"two-arg form forbidden\")))) \
             (foo 1 2)",
        );
        if let Err(ExpandError::BadSyntax { what, .. }) = r {
            assert!(
                what.contains("two-arg form forbidden"),
                "expected msg in: {}",
                what
            );
        } else {
            panic!("expected BadSyntax, got {:?}", r);
        }
    }

    #[test]
    fn library_resolver_loads_external_library() {
        // The library declares (sample lib) and the importer pulls
        // it in. The resolver must discriminate by name — the
        // library's own (import) clause is empty here so we don't
        // recurse, but a permissive resolver would loop on any
        // nested import.
        let importer_src = r#"
            (import (sample lib))
            (greeting)
        "#;
        let library_src = r#"
            (library (sample lib)
              (export greeting)
              (import)
              (define (greeting) 'hello))
        "#;

        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let importer_data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();

        let library_src_owned = library_src.to_string();
        let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
            let printed = format_library_name(name, syms);
            if printed == "(sample lib)" {
                Some((FileId(1), library_src_owned.clone()))
            } else {
                None
            }
        });
        let mut exp = Expander::new(&mut syms, &mut macros).with_library_resolver(&mut *resolver);
        exp.expand_program(&importer_data)
            .expect("import + library load");
    }

    #[test]
    fn library_resolver_not_installed_is_noop() {
        // Without a resolver, an undeclared library import is a
        // no-op (matches legacy behavior pre-#117).
        let src = "(import (no-such lib))";
        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), src, &mut syms).unwrap();
        let mut exp = Expander::new(&mut syms, &mut macros);
        exp.expand_program(&data)
            .expect("no-resolver import is a no-op");
    }

    #[test]
    fn library_resolver_called_only_once_for_same_lib() {
        // Two imports of the same library in one Expander session
        // should only resolve+load once. Use Rc<Cell> to observe.
        use std::cell::Cell;
        use std::rc::Rc;

        let library_src = "(library (single lib) (export x) (import) (define x 1))";
        let importer_src = r#"
            (import (single lib))
            (import (single lib))
        "#;

        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();

        let calls = Rc::new(Cell::new(0u32));
        let calls_for_closure = calls.clone();
        let library_src_owned = library_src.to_string();
        let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
            let printed = format_library_name(name, syms);
            if printed == "(single lib)" {
                calls_for_closure.set(calls_for_closure.get() + 1);
                Some((FileId(1), library_src_owned.clone()))
            } else {
                None
            }
        });
        let mut exp = Expander::new(&mut syms, &mut macros).with_library_resolver(&mut *resolver);
        exp.expand_program(&data)
            .expect("two imports of one library");
        drop(exp);

        assert_eq!(calls.get(), 1, "library should be loaded exactly once");
    }

    #[test]
    fn library_file_missing_declaration_errors() {
        // Resolver returns source that does NOT declare the
        // requested library. Should error.
        let library_src = "(define oops 1)  ;; no (library …) declaration";
        let importer_src = "(import (sample lib))";

        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();

        let library_src_owned = library_src.to_string();
        let mut resolver: Box<LibraryResolver> =
            Box::new(move |_name, _syms| Some((FileId(1), library_src_owned.clone())));
        let mut exp = Expander::new(&mut syms, &mut macros).with_library_resolver(&mut *resolver);
        let err = exp
            .expand_program(&data)
            .expect_err("missing library declaration should error");
        if let ExpandError::BadSyntax { what, .. } = err {
            assert!(what.contains("did not declare library"), "got: {}", what);
        } else {
            panic!("expected BadSyntax");
        }
    }

    // ---------- Library cache (#116) ----------

    #[test]
    fn library_cache_hit_skips_reexpansion() {
        // First load populates the cache; a second Expander session
        // wired to the same cache reuses the expanded body.
        use std::cell::Cell;
        use std::rc::Rc;

        let library_src = "(library (cache lib) (export x) (import) (define x 1))";
        let importer_src = "(import (cache lib))";

        // Track resolver invocations across both sessions.
        let calls = Rc::new(Cell::new(0u32));

        // Shared cache that survives session 1 → session 2.
        let mut cache = HashMapLibraryCache::new();

        // Session 1: cold cache, resolver runs.
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
            let calls_inner = calls.clone();
            let library_src_owned = library_src.to_string();
            let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
                if format_library_name(name, syms) == "(cache lib)" {
                    calls_inner.set(calls_inner.get() + 1);
                    Some((FileId(1), library_src_owned.clone()))
                } else {
                    None
                }
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data)
                .expect("session 1: cold-cache load");
        }
        assert_eq!(calls.get(), 1, "session 1 invokes resolver");
        assert_eq!(cache.len(), 1, "cache populated after session 1");

        // Session 2: same source, same cache. Resolver still runs
        // (we need source content to compute the hash key), but the
        // expansion work is skipped.
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
            let calls_inner = calls.clone();
            let library_src_owned = library_src.to_string();
            let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
                if format_library_name(name, syms) == "(cache lib)" {
                    calls_inner.set(calls_inner.get() + 1);
                    Some((FileId(1), library_src_owned.clone()))
                } else {
                    None
                }
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data)
                .expect("session 2: warm-cache load");
        }
        assert_eq!(calls.get(), 2, "session 2 also invokes resolver");
        assert_eq!(cache.len(), 1, "cache stays at 1 entry (same key reused)");
    }

    #[test]
    fn library_cache_miss_on_source_change_repopulates() {
        // Different source under the same library name produces a
        // different hash → new cache entry, not a stale hit.
        let mut cache = HashMapLibraryCache::new();

        // Round 1: load v1 source.
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), "(import (changing lib))", &mut syms).unwrap();
            let mut resolver: Box<LibraryResolver> = Box::new(|name, syms| {
                if format_library_name(name, syms) == "(changing lib)" {
                    Some((
                        FileId(1),
                        "(library (changing lib) (export x) (import) (define x 1))".to_string(),
                    ))
                } else {
                    None
                }
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data).unwrap();
        }
        assert_eq!(cache.len(), 1);

        // Round 2: same name, NEW source (different define). The
        // cache key (name, hash) differs because hash differs, so we
        // expect the cache to grow.
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), "(import (changing lib))", &mut syms).unwrap();
            let mut resolver: Box<LibraryResolver> = Box::new(|name, syms| {
                if format_library_name(name, syms) == "(changing lib)" {
                    Some((
                        FileId(1),
                        "(library (changing lib) (export x) (import) (define x 2))".to_string(),
                    ))
                } else {
                    None
                }
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data).unwrap();
        }
        assert_eq!(cache.len(), 2, "source change adds a new cache entry");
    }

    #[test]
    fn library_cache_optional_no_install() {
        // The cache is opt-in. Without with_library_cache the
        // expander behaves exactly as it did before #116.
        let library_src = "(library (no-cache lib) (export x) (import) (define x 1))";
        let importer_src = "(import (no-cache lib))";
        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
        let library_src_owned = library_src.to_string();
        let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
            if format_library_name(name, syms) == "(no-cache lib)" {
                Some((FileId(1), library_src_owned.clone()))
            } else {
                None
            }
        });
        let mut exp = Expander::new(&mut syms, &mut macros).with_library_resolver(&mut *resolver);
        exp.expand_program(&data).expect("works without cache");
    }

    // ---------- Phase 2F: dep-closure invalidation ----------

    #[test]
    fn cache_invalidates_when_transitive_dep_changes() {
        // Library A imports B. After both are cached, simulate
        // B's source changing on disk; A's cache entry must be
        // invalidated even though A's own source is unchanged.
        use std::cell::RefCell;
        use std::rc::Rc;

        let a_src = "(library (depc a) (export x) (import (depc b)) (define x 1))";
        let b_src_v1 = "(library (depc b) (export y) (import) (define y 1))";
        let b_src_v2 = "(library (depc b) (export y) (import) (define y 2))";
        let importer_src = "(import (depc a))";

        // Resolver returns the current version from a mutable map.
        let sources: Rc<RefCell<std::collections::HashMap<String, String>>> =
            Rc::new(RefCell::new(std::collections::HashMap::from([
                ("(depc a)".into(), a_src.into()),
                ("(depc b)".into(), b_src_v1.into()),
            ])));

        let mut cache = HashMapLibraryCache::new();

        // Session 1: cold cache, load A which transitively loads B.
        let resolve_calls_b_v1: Rc<std::cell::Cell<u32>> = Rc::new(std::cell::Cell::new(0));
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
            let srcs = sources.clone();
            let calls_b = resolve_calls_b_v1.clone();
            let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
                let key = format_library_name(name, syms);
                if key == "(depc b)" {
                    calls_b.set(calls_b.get() + 1);
                }
                srcs.borrow().get(&key).map(|s| (FileId(1), s.clone()))
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data).expect("session 1 cold load");
        }
        assert_eq!(cache.len(), 2, "both A and B cached after session 1");

        // Session 2: same sources, same cache. Hit on both -- no
        // expansion needed but resolver still runs for hashing
        // and for dep validation.
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
            let srcs = sources.clone();
            let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
                let key = format_library_name(name, syms);
                srcs.borrow().get(&key).map(|s| (FileId(1), s.clone()))
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data).expect("session 2 warm cache");
        }
        assert_eq!(cache.len(), 2, "no new entries on unchanged warm cache");

        // Session 3: mutate B's source. A's cache entry must
        // detect the upstream change and re-expand. Cache should
        // contain B's new hash entry + A's new hash entry.
        sources
            .borrow_mut()
            .insert("(depc b)".into(), b_src_v2.into());
        {
            let mut syms = SymbolTable::new();
            let mut macros = std::collections::HashMap::new();
            let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
            let srcs = sources.clone();
            let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
                let key = format_library_name(name, syms);
                srcs.borrow().get(&key).map(|s| (FileId(1), s.clone()))
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data)
                .expect("session 3 after dep change");
        }
        // A's cache key (source-hash) is unchanged -- only B's
        // source changed. Without dep-closure tracking, A's entry
        // would remain valid (wrong: A's compiled body references
        // the old B). With Phase 2F, A is invalidated and
        // re-expanded with B's new content. Cache ends up with B's
        // new entry (3rd) AND A re-stored under its same key
        // (overwrite), so total entries = 3 (A, B-v1, B-v2)
        // because the cache uses (name, hash) keys so stale B-v1
        // sticks around -- the validation logic uses dep-hash
        // mismatch, not eviction.
        assert!(
            cache.len() >= 3,
            "expected B v2 entry + A re-cached, got {}",
            cache.len()
        );
    }

    #[test]
    fn cache_entry_records_direct_deps() {
        // Verify the cache entry carries the dep list with the
        // observed (name, source-hash) tuples.
        let a_src = "(library (depr a) (export x) (import (depr b)) (define x 1))";
        let b_src = "(library (depr b) (export y) (import) (define y 1))";
        let importer_src = "(import (depr a))";

        let mut syms = SymbolTable::new();
        let mut macros = std::collections::HashMap::new();
        let data = cs_parse::read_all(FileId(0), importer_src, &mut syms).unwrap();
        let mut cache = HashMapLibraryCache::new();
        {
            let a_owned = a_src.to_string();
            let b_owned = b_src.to_string();
            let mut resolver: Box<LibraryResolver> = Box::new(move |name, syms| {
                let key = format_library_name(name, syms);
                match key.as_str() {
                    "(depr a)" => Some((FileId(1), a_owned.clone())),
                    "(depr b)" => Some((FileId(2), b_owned.clone())),
                    _ => None,
                }
            });
            let mut exp = Expander::new(&mut syms, &mut macros)
                .with_library_resolver(&mut *resolver)
                .with_library_cache(&mut cache);
            exp.expand_program(&data).expect("loads");
        }

        // Recompute the cache key for A and inspect its entry.
        let a_name_strs: Vec<String> = vec!["depr".into(), "a".into()];
        let a_hash = hash_library_source(a_src);
        let entry = cache
            .get(&(a_name_strs.clone(), a_hash))
            .expect("A is cached");
        // A imported B, so A's deps should include B as a
        // string-tuple (per the cross-session-safe encoding).
        let b_name_strs: Vec<String> = vec!["depr".into(), "b".into()];
        let b_hash = hash_library_source(b_src);
        assert!(
            entry
                .deps
                .iter()
                .any(|(n, h)| n == &b_name_strs && *h == b_hash),
            "expected A's dep list to include ([depr, b], B's hash); got {:?}",
            entry.deps
        );
    }
}
