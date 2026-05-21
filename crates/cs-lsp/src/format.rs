//! Formatting (Phase 5 iters 5.3/5.4).
//!
//! A depth-based reindenter: each line's leading whitespace is set from
//! the open-paren nesting at its start (2 spaces per level), with
//! leading close-parens dedented to line up with their opener. It only
//! rewrites leading/trailing whitespace — tokens, comments, and string
//! contents are preserved verbatim (an AST re-emit would drop comments).
//! Idempotent. Not a full Lisp pretty-printer (no arg-alignment or
//! line re-wrapping); that's a later refinement.

const INDENT: usize = 2;

/// Reformat `src` with canonical depth-based indentation.
pub fn format(src: &str) -> String {
    let mut out = String::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let lines: Vec<&str> = src.split('\n').collect();
    let n = lines.len();

    for (i, &line) in lines.iter().enumerate() {
        if in_string {
            // Inside a multi-line string: never touch the line (its
            // whitespace is string content).
            out.push_str(line);
        } else {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let closes = count_leading_closes(trimmed);
                let level = (depth - closes).max(0) as usize;
                for _ in 0..level * INDENT {
                    out.push(' ');
                }
                out.push_str(trimmed);
            }
            // empty/whitespace-only line → emit a bare blank line
        }
        let (delta, next_in_string) = scan_line(line, in_string);
        depth = (depth + delta).max(0);
        in_string = next_in_string;
        if i + 1 < n {
            out.push('\n');
        }
    }
    out
}

/// Count leading `)`/`]` on a trimmed line (they dedent it).
fn count_leading_closes(s: &str) -> i32 {
    s.chars().take_while(|&c| c == ')' || c == ']').count() as i32
}

/// Net paren delta of one line plus the string state at its end.
/// Skips parens inside strings, line comments (`;`), and char literals
/// (`#\(`). `#|…|#` block comments are not tracked (rare).
fn scan_line(line: &str, mut in_string: bool) -> (i32, bool) {
    let mut delta = 0i32;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '\\' => {
                    chars.next();
                } // escaped char in string
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            ';' => break, // line comment to EOL
            '#' if chars.peek() == Some(&'\\') => {
                chars.next(); // backslash
                chars.next(); // the literal char (e.g. the '(' of #\()
            }
            '(' | '[' => delta += 1,
            ')' | ']' => delta -= 1,
            _ => {}
        }
    }
    (delta, in_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindents_messy_nesting() {
        let messy = "(define (f x)\n(+ x\n1))";
        assert_eq!(format(messy), "(define (f x)\n  (+ x\n    1))");
    }

    #[test]
    fn leading_close_dedents() {
        let messy = "(let ((x 1))\nx\n)";
        assert_eq!(format(messy), "(let ((x 1))\n  x\n)");
    }

    #[test]
    fn is_idempotent() {
        let messy = "(a\n(b\n(c)))\n(d)";
        let once = format(messy);
        assert_eq!(format(&once), once, "format not idempotent");
    }

    #[test]
    fn preserves_comments_and_trailing_newline() {
        let src = "(f x) ; a comment\n";
        assert_eq!(format(src), "(f x) ; a comment\n");
    }

    #[test]
    fn does_not_count_parens_in_strings() {
        // The ")" inside the string must not change indentation.
        let src = "(display \")(\")\n(g)";
        assert_eq!(format(src), "(display \")(\")\n(g)");
    }

    #[test]
    fn strips_trailing_whitespace_and_overindent() {
        let src = "(a   \n      (b))";
        assert_eq!(format(src), "(a\n  (b))");
    }
}
