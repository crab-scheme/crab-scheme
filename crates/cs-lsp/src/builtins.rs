//! Builtin / special-form docstrings for hover (Phase 2 iter 2.5).
//!
//! Maps a name to a one-line R6RS-style signature + description, shown
//! as hover markup. Not exhaustive — the common procedures and special
//! forms an editing session hits most. Extend freely; auto-generation
//! from cs-runtime doc comments is a later refinement.

/// Hover doc for `name`, or `None` if it isn't a known builtin/form.
pub fn builtin_doc(name: &str) -> Option<&'static str> {
    let doc = match name {
        // ---- pairs / lists ----
        "cons" => "(cons obj1 obj2) — a new pair with car obj1, cdr obj2.",
        "car" => "(car pair) — the first element of pair.",
        "cdr" => "(cdr pair) — the rest of pair.",
        "set-car!" => "(set-car! pair obj) — store obj as pair's car.",
        "set-cdr!" => "(set-cdr! pair obj) — store obj as pair's cdr.",
        "pair?" => "(pair? obj) — #t if obj is a pair.",
        "null?" => "(null? obj) — #t if obj is the empty list.",
        "list" => "(list obj …) — a newly allocated list of its arguments.",
        "list?" => "(list? obj) — #t if obj is a proper list.",
        "length" => "(length list) — the number of elements in list.",
        "append" => "(append list …) — concatenate the lists.",
        "reverse" => "(reverse list) — list with elements in reverse order.",
        "list-ref" => "(list-ref list k) — the kth element (0-based) of list.",
        "list-tail" => "(list-tail list k) — list after dropping k elements.",
        "assoc" => "(assoc obj alist) — first pair in alist whose car equal? obj, else #f.",
        "assq" => "(assq obj alist) — like assoc using eq?.",
        "member" => "(member obj list) — sublist starting at the first equal? obj, else #f.",
        "memq" => "(memq obj list) — like member using eq?.",
        "map" => "(map proc list1 list2 …) — apply proc elementwise, collecting results.",
        "for-each" => "(for-each proc list1 …) — apply proc elementwise for effect.",
        "apply" => "(apply proc arg … list) — call proc with the args plus list's elements.",
        // ---- numbers ----
        "+" => "(+ z …) — sum of the arguments (0 if none).",
        "-" => "(- z1 z2 …) — difference; (- z) negates.",
        "*" => "(* z …) — product of the arguments (1 if none).",
        "/" => "(/ z1 z2 …) — quotient; (/ z) is the reciprocal.",
        "=" => "(= z1 z2 …) — #t if all arguments are numerically equal.",
        "<" => "(< x1 x2 …) — #t if the arguments are monotonically increasing.",
        ">" => "(> x1 x2 …) — #t if monotonically decreasing.",
        "<=" => "(<= x1 x2 …) — #t if monotonically non-decreasing.",
        ">=" => "(>= x1 x2 …) — #t if monotonically non-increasing.",
        "abs" => "(abs x) — absolute value of x.",
        "min" => "(min x1 x2 …) — the smallest argument.",
        "max" => "(max x1 x2 …) — the largest argument.",
        "quotient" => "(quotient n1 n2) — integer quotient, truncated toward zero.",
        "remainder" => "(remainder n1 n2) — remainder with the sign of n1.",
        "modulo" => "(modulo n1 n2) — remainder with the sign of n2.",
        "expt" => "(expt z1 z2) — z1 raised to the power z2.",
        "number?" => "(number? obj) — #t if obj is a number.",
        "zero?" => "(zero? z) — #t if z is zero.",
        "even?" => "(even? n) — #t if n is even.",
        "odd?" => "(odd? n) — #t if n is odd.",
        // ---- predicates / equality ----
        "not" => "(not obj) — #t if obj is #f, else #f.",
        "eq?" => "(eq? obj1 obj2) — identity comparison (fast, pointer-ish).",
        "eqv?" => "(eqv? obj1 obj2) — like eq? but compares numbers/chars by value.",
        "equal?" => "(equal? obj1 obj2) — deep structural equality.",
        "boolean?" => "(boolean? obj) — #t if obj is #t or #f.",
        "symbol?" => "(symbol? obj) — #t if obj is a symbol.",
        "string?" => "(string? obj) — #t if obj is a string.",
        "procedure?" => "(procedure? obj) — #t if obj is callable.",
        // ---- vectors / strings ----
        "vector" => "(vector obj …) — a newly allocated vector of its arguments.",
        "make-vector" => "(make-vector k [fill]) — a vector of k elements.",
        "vector-ref" => "(vector-ref vec k) — the kth element of vec.",
        "vector-set!" => "(vector-set! vec k obj) — store obj at index k.",
        "vector-length" => "(vector-length vec) — number of elements in vec.",
        "string-length" => "(string-length s) — number of characters in s.",
        "string-ref" => "(string-ref s k) — the kth character of s.",
        "string-append" => "(string-append s …) — concatenate the strings.",
        "substring" => "(substring s start end) — the substring s[start, end).",
        // ---- I/O ----
        "display" => "(display obj [port]) — write obj in human-readable form.",
        "write" => "(write obj [port]) — write obj in machine-readable (re-readable) form.",
        "newline" => "(newline [port]) — write a newline.",
        // ---- special forms ----
        "define" => {
            "(define name value) / (define (f args…) body…) — bind a top-level or internal name."
        }
        "lambda" => "(lambda (args…) body…) — an anonymous procedure.",
        "let" => "(let ((name val)…) body…) — bind names locally.",
        "let*" => "(let* ((name val)…) body…) — sequential local bindings.",
        "letrec" => "(letrec ((name val)…) body…) — mutually recursive local bindings.",
        "if" => "(if test then [else]) — conditional.",
        "cond" => "(cond (test body…)… [(else body…)]) — multi-way conditional.",
        "case" => "(case key ((datum…) body…)… [(else body…)]) — dispatch on eqv?.",
        "when" => "(when test body…) — evaluate body if test is true.",
        "unless" => "(unless test body…) — evaluate body if test is false.",
        "begin" => "(begin expr…) — evaluate in sequence, return the last.",
        "quote" => "(quote datum) / 'datum — the datum, unevaluated.",
        "set!" => "(set! name value) — assign to an existing binding.",
        _ => return None,
    };
    Some(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_builtins_have_docs() {
        assert!(builtin_doc("cons").unwrap().contains("pair"));
        assert!(builtin_doc("map").unwrap().contains("proc"));
        assert!(builtin_doc("lambda").unwrap().contains("procedure"));
    }

    #[test]
    fn unknown_name_has_no_doc() {
        assert!(builtin_doc("my-helper").is_none());
    }
}
