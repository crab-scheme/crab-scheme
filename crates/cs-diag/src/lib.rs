//! Diagnostics, source maps, and span types for CrabScheme.
//!
//! This crate is dependency-free at the bottom of the dependency graph; every
//! other CrabScheme crate emits diagnostics through the types defined here.

use std::fmt;

/// Identifier for a source file (or REPL input chunk) tracked by a [`SourceMap`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FileId(pub u32);

impl FileId {
    /// Sentinel for items with no real source.
    pub const DUMMY: FileId = FileId(u32::MAX);
}

/// A half-open byte range `[start, end)` inside a [`FileId`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const DUMMY: Span = Span {
        file: FileId::DUMMY,
        start: 0,
        end: 0,
    };

    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        Self { file, start, end }
    }

    pub fn merge(self, other: Span) -> Span {
        // Dummy on either side: prefer the non-dummy.
        if self.is_dummy() {
            return other;
        }
        if other.is_dummy() {
            return self;
        }
        // Cross-file merge is legitimate during macro expansion:
        // the template lives at its definition site (file A) but
        // substituted args come from the use site (file B). The
        // earlier strict `debug_assert_eq` caused a panic in
        // `cs_expand::rebuild_list` when a macro defined in one
        // eval_str unit was invoked from another. Fall back to
        // `self` (the span being extended) — diagnostics still
        // point at a meaningful location for the macro definition
        // site, and the expansion succeeds.
        if self.file != other.file {
            return self;
        }
        Span {
            file: self.file,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn is_dummy(self) -> bool {
        self.file == FileId::DUMMY
    }
}

/// Maps [`FileId`]s to source contents and provides line/column lookup.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

struct SourceFile {
    name: String,
    contents: String,
    /// Byte offsets of the first character of each line (line 0 starts at 0).
    line_starts: Vec<u32>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: impl Into<String>, contents: impl Into<String>) -> FileId {
        let contents = contents.into();
        let mut line_starts = vec![0u32];
        for (i, b) in contents.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile {
            name: name.into(),
            contents,
            line_starts,
        });
        id
    }

    pub fn name(&self, file: FileId) -> &str {
        if file == FileId::DUMMY {
            return "<unknown>";
        }
        &self.files[file.0 as usize].name
    }

    pub fn contents(&self, file: FileId) -> &str {
        if file == FileId::DUMMY {
            return "";
        }
        &self.files[file.0 as usize].contents
    }

    /// Returns 1-based (line, column).
    pub fn line_col(&self, span: Span) -> (u32, u32) {
        if span.is_dummy() {
            return (0, 0);
        }
        let f = &self.files[span.file.0 as usize];
        let line_idx = match f.line_starts.binary_search(&span.start) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = f.line_starts[line_idx];
        let col = span.start - line_start;
        (line_idx as u32 + 1, col + 1)
    }

    pub fn snippet(&self, span: Span) -> &str {
        if span.is_dummy() {
            return "";
        }
        let f = &self.files[span.file.0 as usize];
        &f.contents[span.start as usize..span.end as usize]
    }
}

/// Severity for a [`Diagnostic`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// A structured diagnostic. Renderable to plain text via [`render`].
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<&'static str>,
    pub message: String,
    pub primary: Span,
    pub labels: Vec<(Span, String)>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, primary: Span) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            message: message.into(),
            primary,
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }

    pub fn with_label(mut self, span: Span, msg: impl Into<String>) -> Self {
        self.labels.push((span, msg.into()));
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Render a diagnostic to plain text using the provided source map.
pub fn render(diag: &Diagnostic, sm: &SourceMap) -> String {
    render_with(diag, sm, false)
}

/// Like [`render`] but with an explicit color toggle. Set `color = true`
/// to wrap severity, file location, and caret in ANSI escape codes.
pub fn render_with(diag: &Diagnostic, sm: &SourceMap, color: bool) -> String {
    // ANSI sequences. When `color` is false, all of these expand to "".
    let reset = if color { "\x1b[0m" } else { "" };
    let bold = if color { "\x1b[1m" } else { "" };
    let red = if color { "\x1b[31m" } else { "" };
    let yellow = if color { "\x1b[33m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };
    let blue = if color { "\x1b[34m" } else { "" };

    let (sev_label, sev_color) = match diag.severity {
        Severity::Error => ("error", red),
        Severity::Warning => ("warning", yellow),
        Severity::Note => ("note", cyan),
    };

    let mut out = String::new();
    let header = if let Some(code) = diag.code {
        format!(
            "{sev_label}[{code}]: {msg}",
            sev_label = sev_label,
            code = code,
            msg = diag.message
        )
    } else {
        format!(
            "{sev_label}: {msg}",
            sev_label = sev_label,
            msg = diag.message
        )
    };
    out.push_str(&format!("{}{}{}{}\n", bold, sev_color, header, reset));

    if !diag.primary.is_dummy() {
        let (line, col) = sm.line_col(diag.primary);
        let name = sm.name(diag.primary.file);
        out.push_str(&format!(
            " {bold}--> {blue}{name}:{line}:{col}{reset}\n",
            bold = bold,
            blue = blue,
            reset = reset,
            name = name,
            line = line,
            col = col,
        ));

        let f_contents = sm.contents(diag.primary.file);
        if let Some(line_text) = f_contents.lines().nth((line as usize).saturating_sub(1)) {
            out.push_str("  | \n");
            out.push_str(&format!("{:>3} | {}\n", line, line_text));
            let span_len = (diag.primary.end - diag.primary.start).max(1) as usize;
            let caret_str: String = "^".repeat(span_len);
            let pad: String = " ".repeat(col as usize - 1);
            out.push_str(&format!(
                "  | {}{}{}{}{}\n",
                pad, bold, sev_color, caret_str, reset
            ));
        }
    }

    for note in &diag.notes {
        out.push_str(&format!(
            "  = {bold}note{reset}: {note}\n",
            bold = bold,
            reset = reset,
            note = note
        ));
    }
    out
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.severity, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_merge() {
        let f = FileId(0);
        let a = Span::new(f, 5, 10);
        let b = Span::new(f, 8, 15);
        let m = a.merge(b);
        assert_eq!(m.start, 5);
        assert_eq!(m.end, 15);
    }

    #[test]
    fn line_col_basic() {
        let mut sm = SourceMap::new();
        let id = sm.add("test", "abc\ndef\nghi");
        // 'a'
        assert_eq!(sm.line_col(Span::new(id, 0, 1)), (1, 1));
        // 'd'
        assert_eq!(sm.line_col(Span::new(id, 4, 5)), (2, 1));
        // 'i'
        assert_eq!(sm.line_col(Span::new(id, 10, 11)), (3, 3));
    }

    #[test]
    fn render_simple_error() {
        let mut sm = SourceMap::new();
        let id = sm.add("foo.scm", "(+ 1 \"two\")");
        let diag = Diagnostic::error("expected number, got string", Span::new(id, 5, 10))
            .with_code("E0042")
            .with_note("expected by '+'");
        let out = render(&diag, &sm);
        assert!(out.contains("error[E0042]"));
        assert!(out.contains("foo.scm:1:6"));
        assert!(out.contains("note: expected by '+'"));
    }
}
