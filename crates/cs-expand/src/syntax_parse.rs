//! Backtracking pattern matcher for `define-syntax-parser`
//! syntax-parse combinators — R6RS++ Phase 2A.3, issue #31.
//!
//! The plain `syntax-rules` matcher in the parent module is
//! deterministic: it can't express alternation (`~or`), optionality
//! (`~optional`), or cardinality-over-repetition (`~once`). This
//! module adds a small backtracking matcher that handles those, while
//! producing the **same** [`MatchBinding`] map the `syntax-rules`
//! engine produces — so the existing `instantiate` template
//! expander is reused unchanged.
//!
//! Only macros that actually use a combinator route here (the
//! `Macro::parser` flag); everything else keeps the original path.
//!
//! ## Combinator grammar (inside a `define-syntax-parser` pattern)
//!
//! - `(~or P ...)` — ordered alternation. As a single pattern
//!   element it matches the input element against each `P` in turn;
//!   the first that matches wins. Pattern variables of the
//!   alternatives not taken are bound [`MatchBinding::Absent`].
//! - `(~optional P)` / `(~optional P #:defaults ([v d] ...))` — the
//!   element may be present or absent. Absent ⇒ each var of `P` is
//!   `Absent` unless a `#:defaults` entry gives it a value.
//! - `(~once P)` — outside an ellipsis this is just a required single
//!   element (a degenerate alias for `P`). Its real power is as an
//!   **ellipsis-head** pattern (below).
//!
//! ## Ellipsis-head (EH) patterns: `EH ...`
//!
//! When the repeated unit before `...` is a combinator, each
//! repetition matches one *alternative*, and `~once` / `~optional`
//! impose cardinality constraints accumulated **across** the whole
//! ellipsis. This is what enables order-free keyword parsing:
//!
//! ```scheme
//! (define-syntax-parser my-def
//!   ((_ name (~or (~once #:a a) (~once #:b b)) ...)
//!    ...))            ; #:a and #:b each exactly once, any order
//! ```
//!
//! A `~once` / `~optional` EH-clause takes a *splice* of sub-patterns
//! (`#:a a` is two elements consumed together) and binds its vars as
//! scalars (`Single`); a plain EH-clause binds its vars as `Repeat`.
//!
//! ## Limitations
//!
//! - Clause patterns are **proper lists**; a dotted tail (`. rest`) in
//!   a combinator-using clause is not matched.
//! - One `...` per list level (same ceiling as the syntax-rules
//!   engine); pattern-variable nesting depth tops out at 1.
//! - `~or` / `~optional` alternatives consume a fixed number of
//!   elements (no `~seq` with an internal `...`).
//! - `:class` annotations (Phase 2A.1/2A.2) compose with a combinator
//!   when they constrain a single pattern variable, but **conflicting**
//!   per-alternative annotations on the *same* variable in a `~or`
//!   (e.g. `(~or n:number n:string)`) are unsupported: class checks are
//!   collected flatly and ANDed in the shared clause body, so the two
//!   constraints would contradict. Use distinct variable names per
//!   alternative, or check inside the body.

use std::cell::RefCell;
use std::collections::HashMap;

use cs_core::{Symbol, SymbolTable};
use cs_diag::Span;
use cs_parse::Datum;

use crate::{collect_pair_chain, collect_proper_list_strict, MatchBinding};

/// Pre-interned symbols the matcher recognizes as combinators /
/// structural markers. Built once per expander from `Keywords`.
pub(crate) struct ParseSyms {
    pub ellipsis: Symbol,
    pub underscore: Symbol,
    pub tilde_or: Symbol,
    pub tilde_optional: Symbol,
    pub tilde_once: Symbol,
    pub kw_defaults: Symbol,
}

/// A pinpointed match failure: which sub-form is to blame, and why
/// (R6RS++ Phase 2A.4, issue #33). `span` rides on the `ExpandError`
/// so tooling (LSP) can underline the offending form; `reason`
/// surfaces in the human-readable message.
pub(crate) struct MatchError {
    pub span: Span,
    pub reason: String,
}

/// Immutable matching context threaded through the recursion. `track`
/// uses interior mutability so the deeply-recursive (and backtracking)
/// matcher can record the *furthest* failure it sees without changing
/// every signature; the furthest-position failure is the most useful
/// diagnostic (standard parser-error heuristic).
struct Ctx<'a> {
    literals: &'a [Symbol],
    ps: &'a ParseSyms,
    syms: &'a SymbolTable,
    track: RefCell<Option<MatchError>>,
}

impl Ctx<'_> {
    /// Record a failure, keeping the one that reached furthest into the
    /// input (largest `span.end`). Ties favor the later record.
    fn record(&self, span: Span, reason: impl Into<String>) {
        let mut t = self.track.borrow_mut();
        let keep = t.as_ref().is_none_or(|best| span.end >= best.span.end);
        if keep {
            *t = Some(MatchError {
                span,
                reason: reason.into(),
            });
        }
    }
}

type Bindings = HashMap<Symbol, MatchBinding>;

/// Returns `true` if any clause in a `define-syntax-parser` uses a
/// combinator anywhere in its pattern — i.e. the macro must route
/// through this matcher rather than the `syntax-rules` desugar.
pub(crate) fn pattern_uses_combinators(pat: &Datum, ps: &ParseSyms) -> bool {
    match pat {
        Datum::Symbol(_, _) => false,
        Datum::Pair(_, _, _) => {
            if let Some((items, tail)) = collect_pair_chain(pat) {
                if let Some(Datum::Symbol(s, _)) = items.first() {
                    if *s == ps.tilde_or || *s == ps.tilde_optional || *s == ps.tilde_once {
                        return true;
                    }
                }
                items.iter().any(|i| pattern_uses_combinators(i, ps))
                    || pattern_uses_combinators(&tail, ps)
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Match a whole `define-syntax-parser` clause pattern `(_ p ...)`
/// against the macro call `input`. The leading element is the macro
/// keyword slot and matches unconditionally. On success `bindings`
/// holds every pattern variable's value.
pub(crate) fn match_parse_clause(
    pattern: &Datum,
    input: &Datum,
    literals: &[Symbol],
    ps: &ParseSyms,
    syms: &SymbolTable,
    bindings: &mut Bindings,
) -> Result<(), MatchError> {
    let fallback = |reason: &str| MatchError {
        span: input.span(),
        reason: reason.to_string(),
    };
    let pats = collect_proper_list_strict(pattern)
        .ok_or_else(|| fallback("macro clause pattern is not a proper list"))?;
    let ins = collect_proper_list_strict(input)
        .ok_or_else(|| fallback("macro call is not a proper list"))?;
    if pats.is_empty() || ins.is_empty() {
        return Err(fallback("empty macro form"));
    }
    let ctx = Ctx {
        literals,
        ps,
        syms,
        track: RefCell::new(None),
    };
    if match_seq(&pats[1..], &ins[1..], &ctx, bindings) {
        Ok(())
    } else {
        Err(ctx
            .track
            .into_inner()
            .unwrap_or_else(|| fallback("call does not match this clause")))
    }
}

// ---------------------------------------------------------------------
// Sequence matcher (handles head patterns + ellipsis with backtracking)
// ---------------------------------------------------------------------

fn match_seq(pats: &[Datum], ins: &[Datum], ctx: &Ctx, bindings: &mut Bindings) -> bool {
    let (p0, prest) = match pats.split_first() {
        None => {
            if let Some(extra) = ins.first() {
                // Pattern exhausted but input remains: a surplus argument.
                ctx.record(extra.span(), "unexpected extra form");
                return false;
            }
            return true;
        }
        Some(x) => x,
    };

    // `p0 ...` — ellipsis (plain or ellipsis-head).
    if matches!(pats.get(1), Some(d) if is_sym(d, ctx.ps.ellipsis)) {
        return match_ellipsis(p0, &pats[2..], ins, ctx, bindings);
    }

    // `(~or ALT ...)` head pattern: each ALT consumes one element.
    if let Some(alts) = combinator_args(p0, ctx.ps.tilde_or, ctx) {
        if ins.is_empty() {
            ctx.record(
                p0.span(),
                "missing form (expected one of the ~or alternatives)",
            );
            return false;
        }
        for alt in &alts {
            let snap = bindings.clone();
            if match_one(alt, &ins[0], ctx, bindings) && match_seq(prest, &ins[1..], ctx, bindings)
            {
                // Vars of the alternatives not taken are absent.
                bind_absent_others(&alts, alt, ctx, bindings);
                return true;
            }
            *bindings = snap;
        }
        return false;
    }

    // `(~optional P [#:defaults ...])` head pattern.
    if let Some(args) = combinator_args(p0, ctx.ps.tilde_optional, ctx) {
        let (sub, defaults) = parse_optional(&args, ctx);
        // Present: match P against the next element, then the rest.
        let snap = bindings.clone();
        if !ins.is_empty()
            && match_one(sub, &ins[0], ctx, bindings)
            && match_seq(prest, &ins[1..], ctx, bindings)
        {
            return true;
        }
        *bindings = snap;
        // Absent: bind P's vars to defaults / Absent, keep the input.
        bind_absent_pattern(sub, &defaults, ctx, bindings);
        return match_seq(prest, ins, ctx, bindings);
    }

    // `(~once P)` outside an ellipsis — a required single element.
    if let Some(args) = combinator_args(p0, ctx.ps.tilde_once, ctx) {
        if args.len() != 1 {
            ctx.record(
                p0.span(),
                "~once outside an ellipsis takes exactly one pattern",
            );
            return false;
        }
        if ins.is_empty() {
            ctx.record(p0.span(), "missing required form");
            return false;
        }
        return match_one(&args[0], &ins[0], ctx, bindings)
            && match_seq(prest, &ins[1..], ctx, bindings);
    }

    // Plain element: consume exactly one input.
    if ins.is_empty() {
        ctx.record(p0.span(), "missing form (expected more arguments)");
        return false;
    }
    match_one(p0, &ins[0], ctx, bindings) && match_seq(prest, &ins[1..], ctx, bindings)
}

// ---------------------------------------------------------------------
// Single-element matcher
// ---------------------------------------------------------------------

fn match_one(pat: &Datum, inp: &Datum, ctx: &Ctx, bindings: &mut Bindings) -> bool {
    match pat {
        Datum::Symbol(s, _) => {
            if *s == ctx.ps.underscore {
                return true;
            }
            if ctx.literals.contains(s) || is_keyword(*s, ctx.syms) {
                // Literal / keyword: self-match by name.
                if matches!(inp, Datum::Symbol(t, _) if t == s) {
                    return true;
                }
                ctx.record(inp.span(), format!("expected `{}`", ctx.syms.name(*s)));
                return false;
            }
            bindings.insert(*s, MatchBinding::Single(inp.clone()));
            true
        }
        Datum::Boolean(p, _) => {
            let ok = matches!(inp, Datum::Boolean(i, _) if i == p);
            if !ok {
                ctx.record(
                    inp.span(),
                    format!("expected the literal {}", if *p { "#t" } else { "#f" }),
                );
            }
            ok
        }
        Datum::Character(p, _) => {
            let ok = matches!(inp, Datum::Character(i, _) if i == p);
            if !ok {
                ctx.record(inp.span(), "expected a specific character literal");
            }
            ok
        }
        Datum::String(p, _) => {
            let ok = matches!(inp, Datum::String(i, _) if **i == **p);
            if !ok {
                ctx.record(inp.span(), "expected a specific string literal");
            }
            ok
        }
        Datum::Number(_, _) => {
            let ok = matches!(inp, Datum::Number(_, _) if cs_core::eq::equal(&pat.to_value(), &inp.to_value()));
            if !ok {
                ctx.record(inp.span(), "expected a specific number literal");
            }
            ok
        }
        Datum::Null(_) => {
            let ok = matches!(inp, Datum::Null(_));
            if !ok {
                ctx.record(inp.span(), "expected `()`");
            }
            ok
        }
        Datum::Pair(_, _, _) => {
            // Sub-list pattern: recurse with the sequence matcher so
            // nested combinators / ellipses work. Proper lists only
            // (dotted combinator sub-patterns are out of scope).
            let psub = match collect_proper_list_strict(pat) {
                Some(v) => v,
                None => return false,
            };
            let isub = match collect_proper_list_strict(inp) {
                Some(v) => v,
                None => {
                    ctx.record(inp.span(), "expected a list");
                    return false;
                }
            };
            match_seq(&psub, &isub, ctx, bindings)
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------
// Ellipsis matching: plain `P ...` and ellipsis-head `EH ...`
// ---------------------------------------------------------------------

fn match_ellipsis(
    head: &Datum,
    trailing: &[Datum],
    ins: &[Datum],
    ctx: &Ctx,
    bindings: &mut Bindings,
) -> bool {
    if is_eh_pattern(head, ctx) {
        return match_eh_ellipsis(head, trailing, ins, ctx, bindings);
    }
    // Plain `P ...`: P repeats; trailing is a fixed-length suffix.
    if ins.len() < trailing.len() {
        return false;
    }
    let rep = ins.len() - trailing.len();
    let (rep_ins, tail_ins) = ins.split_at(rep);
    let vars = pattern_vars(head, ctx);
    let mut acc: HashMap<Symbol, Vec<Datum>> = vars.iter().map(|v| (*v, Vec::new())).collect();
    for ri in rep_ins {
        let mut sub: Bindings = HashMap::new();
        if !match_one(head, ri, ctx, &mut sub) {
            return false;
        }
        for v in &vars {
            if let Some(MatchBinding::Single(d)) = sub.get(v) {
                acc.get_mut(v).unwrap().push(d.clone());
            }
        }
    }
    for (k, vs) in acc {
        bindings.insert(k, MatchBinding::Repeat(vs));
    }
    match_seq(trailing, tail_ins, ctx, bindings)
}

/// An EH clause: one alternative of an ellipsis-head pattern.
struct EhClause {
    kind: EhKind,
    /// The splice of sub-patterns consumed together per occurrence.
    subpats: Vec<Datum>,
    /// `#:defaults` for an `~optional` clause that matched zero times.
    defaults: Vec<(Symbol, Datum)>,
    /// Pattern variables this clause binds.
    vars: Vec<Symbol>,
}

#[derive(PartialEq)]
enum EhKind {
    Once,
    Optional,
    Plain,
}

fn match_eh_ellipsis(
    head: &Datum,
    trailing: &[Datum],
    ins: &[Datum],
    ctx: &Ctx,
    bindings: &mut Bindings,
) -> bool {
    let clauses = parse_eh_clauses(head, ctx);
    if ins.len() < trailing.len() {
        return false;
    }
    let eh_end = ins.len() - trailing.len();
    let (eh_ins, tail_ins) = ins.split_at(eh_end);

    // Partition eh_ins into chunks, each matched by one clause.
    let parts = match consume_eh(&clauses, eh_ins, ctx) {
        Some(p) => p,
        None => return false,
    };
    // A partition was found, so any failures recorded while exploring
    // abandoned branches are moot — clear them so a cardinality failure
    // below (the real reason) isn't outranked by a speculative one.
    *ctx.track.borrow_mut() = None;

    // Cardinality: ~once exactly 1, ~optional ≤ 1, plain unconstrained.
    let mut counts = vec![0usize; clauses.len()];
    for (i, _) in &parts {
        counts[*i] += 1;
    }
    // The whole repeated section's span, used to anchor cardinality
    // (absence / surplus) diagnostics when there is no single token to
    // blame; `head` (the EH pattern) is a reasonable fallback.
    let section_span = eh_ins
        .first()
        .map(|d| d.span())
        .unwrap_or_else(|| head.span());
    for (i, c) in clauses.iter().enumerate() {
        match c.kind {
            EhKind::Once if counts[i] == 0 => {
                ctx.record(
                    section_span,
                    format!("missing required {}", clause_label(c, ctx)),
                );
                return false;
            }
            EhKind::Once if counts[i] > 1 => {
                ctx.record(
                    section_span,
                    format!("{} may appear only once", clause_label(c, ctx)),
                );
                return false;
            }
            EhKind::Optional if counts[i] > 1 => {
                ctx.record(
                    section_span,
                    format!("{} may appear at most once", clause_label(c, ctx)),
                );
                return false;
            }
            _ => {}
        }
    }

    // Build the binding map. `~once`/`~optional` clause vars are
    // scalars (exactly one / at most one occurrence). Plain-clause vars
    // are `Repeat`s — and a var shared across several `~or` alternatives
    // accumulates across ALL of them in input order, so we collect plain
    // vars var-centrically (scanning every part) rather than per-clause.
    for (i, c) in clauses.iter().enumerate() {
        if c.kind == EhKind::Plain {
            continue;
        }
        if let Some((_, sub)) = parts.iter().find(|(j, _)| *j == i) {
            for v in &c.vars {
                if let Some(d) = sub.get(v) {
                    bindings.insert(*v, MatchBinding::Single(d.clone()));
                }
            }
        } else {
            // Optional clause that matched zero times.
            for v in &c.vars {
                let b = match c.defaults.iter().find(|(dv, _)| dv == v) {
                    Some((_, d)) => MatchBinding::Single(d.clone()),
                    None => MatchBinding::Absent,
                };
                bindings.insert(*v, b);
            }
        }
    }
    let mut repeat_vars: Vec<Symbol> = Vec::new();
    for c in clauses.iter().filter(|c| c.kind == EhKind::Plain) {
        for v in &c.vars {
            if !repeat_vars.contains(v) {
                repeat_vars.push(*v);
            }
        }
    }
    for v in repeat_vars {
        let vs: Vec<Datum> = parts
            .iter()
            .filter_map(|(_, sub)| sub.get(&v).cloned())
            .collect();
        bindings.insert(v, MatchBinding::Repeat(vs));
    }

    match_seq(trailing, tail_ins, ctx, bindings)
}

/// Greedily partition `ins` into chunks, each matched by some clause's
/// splice. Returns the per-chunk `(clause_index, scalar_bindings)` in
/// input order, or `None` if no full partition exists. Backtracks.
fn consume_eh(
    clauses: &[EhClause],
    ins: &[Datum],
    ctx: &Ctx,
) -> Option<Vec<(usize, HashMap<Symbol, Datum>)>> {
    if ins.is_empty() {
        return Some(Vec::new());
    }
    for (i, c) in clauses.iter().enumerate() {
        let w = c.subpats.len();
        if w == 0 || w > ins.len() {
            continue;
        }
        let mut sub: HashMap<Symbol, Datum> = HashMap::new();
        if match_splice(&c.subpats, &ins[..w], ctx, &mut sub) {
            if let Some(mut rest) = consume_eh(clauses, &ins[w..], ctx) {
                rest.insert(0, (i, sub));
                return Some(rest);
            }
        }
    }
    None
}

/// Match a splice of `subpats` against exactly `chunk` (same length),
/// collecting scalar bindings.
fn match_splice(
    subpats: &[Datum],
    chunk: &[Datum],
    ctx: &Ctx,
    out: &mut HashMap<Symbol, Datum>,
) -> bool {
    if subpats.len() != chunk.len() {
        return false;
    }
    for (p, i) in subpats.iter().zip(chunk.iter()) {
        let mut sub: Bindings = HashMap::new();
        if !match_one(p, i, ctx, &mut sub) {
            return false;
        }
        for (k, v) in sub {
            if let MatchBinding::Single(d) = v {
                out.insert(k, d);
            }
        }
    }
    true
}

// ---------------------------------------------------------------------
// EH-clause / ~optional parsing
// ---------------------------------------------------------------------

fn is_eh_pattern(d: &Datum, ctx: &Ctx) -> bool {
    combinator_args(d, ctx.ps.tilde_or, ctx).is_some()
        || combinator_args(d, ctx.ps.tilde_once, ctx).is_some()
        || combinator_args(d, ctx.ps.tilde_optional, ctx).is_some()
}

fn parse_eh_clauses(head: &Datum, ctx: &Ctx) -> Vec<EhClause> {
    if let Some(alts) = combinator_args(head, ctx.ps.tilde_or, ctx) {
        alts.iter().map(|a| parse_eh_clause(a, ctx)).collect()
    } else {
        vec![parse_eh_clause(head, ctx)]
    }
}

fn parse_eh_clause(c: &Datum, ctx: &Ctx) -> EhClause {
    if let Some(args) = combinator_args(c, ctx.ps.tilde_once, ctx) {
        let vars = seq_vars(&args, ctx);
        EhClause {
            kind: EhKind::Once,
            subpats: args,
            defaults: Vec::new(),
            vars,
        }
    } else if let Some(args) = combinator_args(c, ctx.ps.tilde_optional, ctx) {
        let (subpats, defaults) = parse_optional_splice(&args, ctx);
        let vars = seq_vars(&subpats, ctx);
        EhClause {
            kind: EhKind::Optional,
            subpats,
            defaults,
            vars,
        }
    } else {
        // A plain pattern alternative: one element per occurrence.
        let vars = pattern_vars(c, ctx);
        EhClause {
            kind: EhKind::Plain,
            subpats: vec![c.clone()],
            defaults: Vec::new(),
            vars,
        }
    }
}

/// A human-friendly label for an EH clause in a diagnostic — the first
/// keyword / literal in its splice (e.g. `` `#:b` ``), else "option".
fn clause_label(c: &EhClause, ctx: &Ctx) -> String {
    for p in &c.subpats {
        if let Datum::Symbol(s, _) = p {
            if is_keyword(*s, ctx.syms) || ctx.literals.contains(s) {
                return format!("`{}`", ctx.syms.name(*s));
            }
        }
    }
    "option".to_string()
}

/// Parse `~optional` args in head-pattern position: a single pattern
/// plus optional `#:defaults`.
fn parse_optional<'a>(args: &'a [Datum], ctx: &Ctx) -> (&'a Datum, Vec<(Symbol, Datum)>) {
    let pat = &args[0];
    let defaults = parse_defaults_tail(&args[1..], ctx);
    (pat, defaults)
}

/// Parse `~optional` args in EH (splice) position: the leading
/// sub-patterns form the splice, with an optional trailing
/// `#:defaults`.
fn parse_optional_splice(args: &[Datum], ctx: &Ctx) -> (Vec<Datum>, Vec<(Symbol, Datum)>) {
    // Find a `#:defaults` marker; everything before it is the splice.
    let split = args.iter().position(|d| is_sym(d, ctx.ps.kw_defaults));
    match split {
        Some(idx) => {
            let subpats = args[..idx].to_vec();
            let defaults = parse_defaults_tail(&args[idx..], ctx);
            (subpats, defaults)
        }
        None => (args.to_vec(), Vec::new()),
    }
}

/// Parse a trailing `#:defaults ([v d] ...)` from `tail`. `tail`
/// begins at the `#:defaults` keyword if present.
fn parse_defaults_tail(tail: &[Datum], ctx: &Ctx) -> Vec<(Symbol, Datum)> {
    if tail.len() < 2 || !is_sym(&tail[0], ctx.ps.kw_defaults) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(binds) = collect_proper_list_strict(&tail[1]) {
        for b in binds {
            if let Some(pair) = collect_proper_list_strict(&b) {
                if pair.len() == 2 {
                    if let Datum::Symbol(v, _) = pair[0] {
                        out.push((v, pair[1].clone()));
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------
// Absent-variable binding
// ---------------------------------------------------------------------

/// Bind every variable of `pat` to its `#:defaults` value or `Absent`.
fn bind_absent_pattern(
    pat: &Datum,
    defaults: &[(Symbol, Datum)],
    ctx: &Ctx,
    bindings: &mut Bindings,
) {
    for v in pattern_vars(pat, ctx) {
        let b = defaults
            .iter()
            .find(|(dv, _)| *dv == v)
            .map(|(_, d)| MatchBinding::Single(d.clone()))
            .unwrap_or(MatchBinding::Absent);
        bindings.insert(v, b);
    }
}

/// After an `~or` alternative is taken, bind the variables that only
/// the *other* alternatives would have bound to `Absent` (unless the
/// taken alternative already bound them).
fn bind_absent_others(alts: &[Datum], taken: &Datum, ctx: &Ctx, bindings: &mut Bindings) {
    let taken_vars = pattern_vars(taken, ctx);
    for alt in alts {
        if std::ptr::eq(alt, taken) {
            continue;
        }
        for v in pattern_vars(alt, ctx) {
            if !taken_vars.contains(&v) {
                bindings.entry(v).or_insert(MatchBinding::Absent);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Pattern-variable collection (combinator-aware)
// ---------------------------------------------------------------------

fn pattern_vars(pat: &Datum, ctx: &Ctx) -> Vec<Symbol> {
    let mut out = Vec::new();
    collect_vars(pat, ctx, &mut out);
    out
}

fn seq_vars(pats: &[Datum], ctx: &Ctx) -> Vec<Symbol> {
    let mut out = Vec::new();
    for p in pats {
        collect_vars(p, ctx, &mut out);
    }
    out
}

fn collect_vars(pat: &Datum, ctx: &Ctx, out: &mut Vec<Symbol>) {
    match pat {
        Datum::Symbol(s, _) => {
            if *s == ctx.ps.underscore
                || *s == ctx.ps.ellipsis
                || ctx.literals.contains(s)
                || is_keyword(*s, ctx.syms)
                || is_combinator_head_sym(*s, ctx)
            {
                return;
            }
            if !out.contains(s) {
                out.push(*s);
            }
        }
        Datum::Pair(_, _, _) => {
            if let Some(args) = combinator_args(pat, ctx.ps.tilde_optional, ctx) {
                // Pattern sub-forms only; the lhs of #:defaults are the
                // same vars, but never the default expressions.
                let (subpats, defaults) = parse_optional_splice(&args, ctx);
                for p in &subpats {
                    collect_vars(p, ctx, out);
                }
                for (v, _) in defaults {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
                return;
            }
            if let Some((items, tail)) = collect_pair_chain(pat) {
                for it in &items {
                    collect_vars(it, ctx, out);
                }
                if !matches!(tail, Datum::Null(_)) {
                    collect_vars(&tail, ctx, out);
                }
            }
        }
        Datum::Vector(items, _) => {
            for it in items {
                collect_vars(it, ctx, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------

fn is_sym(d: &Datum, s: Symbol) -> bool {
    matches!(d, Datum::Symbol(t, _) if *t == s)
}

fn is_keyword(s: Symbol, syms: &SymbolTable) -> bool {
    syms.name(s).starts_with("#:")
}

fn is_combinator_head_sym(s: Symbol, ctx: &Ctx) -> bool {
    s == ctx.ps.tilde_or || s == ctx.ps.tilde_optional || s == ctx.ps.tilde_once
}

/// If `d` is a list `(head arg ...)` whose head is the symbol
/// `head_sym`, return the args. Otherwise `None`.
fn combinator_args(d: &Datum, head_sym: Symbol, _ctx: &Ctx) -> Option<Vec<Datum>> {
    let items = collect_proper_list_strict(d)?;
    match items.first() {
        Some(Datum::Symbol(s, _)) if *s == head_sym => Some(items[1..].to_vec()),
        _ => None,
    }
}
