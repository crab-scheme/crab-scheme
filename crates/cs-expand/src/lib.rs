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
    /// Per-Expander record-type registry. Populated each time
    /// `define-record-type` expands; consulted when a child names a
    /// `(parent <type-name>)` so we can resolve its tag chain and inherited
    /// field count. This is *expansion-time* state, separate from the
    /// runtime `__record-parents__` hashtable that powers predicate checks.
    record_types: std::collections::HashMap<Symbol, RecordTypeInfo>,
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

/// A user-defined macro, parsed from `(syntax-rules ...)`.
#[derive(Clone, Debug)]
pub struct Macro {
    pub literals: Vec<Symbol>,
    pub rules: Vec<(Datum, Datum)>,
    /// Name (for diagnostics).
    pub name: Symbol,
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
    define_record_type: Symbol,
    fields: Symbol,
    parent: Symbol,
    immutable: Symbol,
    mutable: Symbol,
    define_syntax: Symbol,
    let_syntax: Symbol,
    letrec_syntax: Symbol,
    syntax_rules: Symbol,
    ellipsis: Symbol,
    underscore: Symbol,
    delay: Symbol,
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
            define_record_type: syms.intern("define-record-type"),
            fields: syms.intern("fields"),
            parent: syms.intern("parent"),
            immutable: syms.intern("immutable"),
            mutable: syms.intern("mutable"),
            define_syntax: syms.intern("define-syntax"),
            let_syntax: syms.intern("let-syntax"),
            letrec_syntax: syms.intern("letrec-syntax"),
            syntax_rules: syms.intern("syntax-rules"),
            ellipsis: syms.intern("..."),
            underscore: syms.intern("_"),
            delay: syms.intern("delay"),
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
            record_types: std::collections::HashMap::new(),
        }
    }

    /// Install an `include` resolver. Calls to `(include "path")` will
    /// invoke this callback with the literal path string from the form.
    pub fn with_include_resolver(mut self, resolver: &'a mut IncludeResolver<'a>) -> Self {
        self.include_resolver = Some(resolver);
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
        // and `include`.
        if let Some((head, tail)) = list_head(d) {
            if let Datum::Symbol(s, _) = &*head {
                if *s == self.keywords.define {
                    return self.expand_define(&tail, d.span());
                }
                if *s == self.keywords.define_record_type {
                    return self.expand_define_record_type(&tail, d.span());
                }
                if *s == self.keywords.define_syntax {
                    return self.expand_define_syntax(&tail, d.span());
                }
                if *s == self.keywords.include {
                    return self.expand_include(&tail, d.span());
                }
            }
        }
        self.expand(d)
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
            if s == self.keywords.case_lambda {
                return self.expand_case_lambda(&tail_items, span);
            }
            if s == self.keywords.cond_expand {
                return self.expand_cond_expand(&tail_items, span);
            }
            if s == self.keywords.delay {
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
        // (cond (test body...) ... (else body...))
        let mut acc = CoreExpr::Const {
            value: Value::Unspecified,
            span,
        };
        for clause in items.iter().rev() {
            let (head, body_items) = collect_list(clause).ok_or(ExpandError::BadSyntax {
                what: "cond clause must be a list".into(),
                span: clause.span(),
            })?;
            let body = if body_items.is_empty() {
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
            let test = self.expand(&head)?;
            acc = CoreExpr::If {
                cond: Rc::new(test),
                then: Rc::new(body),
                alt: Rc::new(acc),
                span: clause.span(),
            };
        }
        Ok(acc)
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
            let body = if body_items.is_empty() {
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
            let (head, body_items) = collect_list(clause).ok_or(ExpandError::BadSyntax {
                what: "guard clause must be a list".into(),
                span: clause.span(),
            })?;
            let body = if body_items.is_empty() {
                CoreExpr::Const {
                    value: Value::Unspecified,
                    span: clause.span(),
                }
            } else {
                self.expand_body(&body_items, clause.span())?
            };
            if let Datum::Symbol(s, _) = &*head {
                if *s == self.keywords.else_ {
                    acc = body;
                    continue;
                }
            }
            let test = self.expand(&head)?;
            acc = CoreExpr::If {
                cond: Rc::new(test),
                then: Rc::new(body),
                alt: Rc::new(acc),
                span: clause.span(),
            };
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
                        // We don't have a library system — every (library ...)
                        // requirement is false.
                        false
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
    /// otherwise raises an error containing the source form of the failed
    /// expression. R6RS spec calls for an `&assertion-violation` condition;
    /// we use the existing string-tagged condition shape since the test
    /// surface checks via predicates only.
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
        let error_call = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: self.syms.intern("error"),
                span,
            }),
            args: vec![CoreExpr::Const {
                value: Value::string(err_msg),
                span,
            }],
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
        let make_promise = self.syms.intern("make-promise");
        Ok(CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: make_promise,
                span,
            }),
            args: vec![thunk],
            span,
        })
    }

    /// `(define-record-type name-spec (fields field-spec ...))`
    /// Desugars to vector-backed records:
    /// - Constructor: `(make-NAME f1 f2 ...)` returns `#(<tag> f1 f2 ...)`
    /// - Predicate:   `(NAME? v)` checks vector? + length + tag
    /// - Accessor:    `(NAME-FIELD r)` returns `(vector-ref r <i>)`
    /// - Mutator:     `(NAME-FIELD-set! r v)` invokes `vector-set!`
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
                        if parts.len() != 4 {
                            return Err(ExpandError::BadSyntax {
                                what: "(mutable field accessor mutator) needs 4 elements".into(),
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
                        let mutator = match &parts[3] {
                            Datum::Symbol(s, _) => *s,
                            _ => {
                                return Err(ExpandError::BadSyntax {
                                    what: "mutator name must be symbol".into(),
                                    span: f.span(),
                                });
                            }
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

    // ---- macro expansion (M3 first cut: non-hygienic syntax-rules) ----

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
        for (pattern, template) in &macro_def.rules {
            let mut bindings: std::collections::HashMap<cs_core::Symbol, MatchBinding> =
                std::collections::HashMap::new();
            if match_pattern(
                pattern,
                input,
                &macro_def.literals,
                self.keywords.ellipsis,
                self.keywords.underscore,
                true,
                &mut bindings,
            ) {
                return self.instantiate_template(template, &bindings);
            }
        }
        Err(ExpandError::BadSyntax {
            what: format!("no matching rule for macro '{}'", self.syms.name(name)),
            span: input.span(),
        })
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
            // (let ((name val) ...) body ...)
            if items.len() < 2 {
                return rebuild_list(items.to_vec(), span);
            }
            let bindings_datum = &items[1];
            let body_datums = &items[2..];
            // Process bindings: for each (name val), rename name if marked.
            let bindings_items = collect_proper_list_strict(bindings_datum).unwrap_or_default();
            let mut new_bindings: Vec<Datum> = Vec::with_capacity(bindings_items.len());
            for b in &bindings_items {
                let parts = collect_proper_list_strict(b).unwrap_or_default();
                if parts.len() != 2 {
                    new_bindings.push(hygiene_pass(b, parent_renames, counter, syms));
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
                // val is evaluated in OUTER scope (parent_renames), not local.
                let new_val = hygiene_pass(val_datum, parent_renames, counter, syms);
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
enum MatchBinding {
    Single(Datum),
    Repeat(Vec<Datum>),
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
    match (pattern, input) {
        // Wildcards
        (Datum::Symbol(s, _), _) if *s == underscore_sym => true,
        // Literal keyword: must be the same symbol
        (Datum::Symbol(s, _), Datum::Symbol(t, _)) if literals.contains(s) => *s == *t,
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
            if let Some(items) = collect_proper_list_strict(pattern) {
                for it in items {
                    collect_pattern_vars_into(&it, literals, ellipsis_sym, underscore_sym, out);
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
                }
            } else {
                // Template-introduced (literal) symbol: mark it so the hygiene
                // post-pass can identify it as macro-introduced.
                let marked = mark_template_symbol(*s, syms);
                Ok(Datum::Symbol(marked, *span))
            }
        }
        Datum::Pair(_, _, span) => {
            let items = collect_proper_list_strict(template).ok_or(ExpandError::BadSyntax {
                what: "template must be proper list".into(),
                span: *span,
            })?;
            // Process items, expanding ellipses.
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
            Ok(rebuild_list(out, *span))
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
fn collect_proper_list_strict(d: &Datum) -> Option<Vec<Datum>> {
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
}
