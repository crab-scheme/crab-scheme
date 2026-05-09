//! R6RS builtin procedures (foundation subset).

use cs_core::{
    eq, Hashtable, HtEqKind, Number, Pair, Port, Promise, PromiseState, SymbolTable, Value,
    WriteMode,
};
use num_traits::ToPrimitive;

use crate::eval::{apply_procedure, EvalCtx};
use crate::proc::{make_builtin_higher, make_builtin_pure, BuiltinFn, PureBuiltinFn};

type PureEntry = (&'static str, PureBuiltinFn);
type HoEntry = (
    &'static str,
    fn(&[Value], &mut EvalCtx) -> Result<Value, String>,
);

pub fn pure_builtins() -> Vec<PureEntry> {
    vec![
        // arithmetic
        ("+", b_add),
        ("-", b_sub),
        ("*", b_mul),
        ("/", b_div),
        ("=", b_num_eq),
        ("<", b_lt),
        (">", b_gt),
        ("<=", b_le),
        (">=", b_ge),
        ("zero?", b_zero),
        ("positive?", b_positive),
        ("negative?", b_negative),
        ("abs", b_abs),
        ("min", b_min),
        ("max", b_max),
        ("modulo", b_modulo),
        ("quotient", b_quotient),
        ("remainder", b_remainder),
        ("div", b_div_op),
        ("mod", b_mod_op),
        ("div0", b_div0_op),
        ("mod0", b_mod0_op),
        ("expt", b_expt),
        ("gcd", b_gcd),
        ("lcm", b_lcm),
        ("floor", b_floor),
        ("ceiling", b_ceiling),
        ("truncate", b_truncate),
        ("round", b_round),
        ("even?", b_even_p),
        ("odd?", b_odd_p),
        ("square", b_square),
        ("exact-integer-sqrt", b_exact_integer_sqrt),
        // transcendental
        ("sqrt", b_sqrt),
        ("exp", b_exp),
        ("log", b_log),
        ("sin", b_sin),
        ("cos", b_cos),
        ("tan", b_tan),
        ("asin", b_asin),
        ("acos", b_acos),
        ("atan", b_atan),
        // bitwise (R6RS arithmetic bitwise)
        ("bitwise-and", b_bitwise_and),
        ("bitwise-or", b_bitwise_or),
        ("bitwise-xor", b_bitwise_xor),
        ("bitwise-not", b_bitwise_not),
        ("bitwise-arithmetic-shift", b_bitwise_arith_shift),
        ("bitwise-arithmetic-shift-left", b_bitwise_arith_shift_left),
        (
            "bitwise-arithmetic-shift-right",
            b_bitwise_arith_shift_right,
        ),
        ("bitwise-bit-count", b_bitwise_bit_count),
        ("bitwise-length", b_bitwise_length),
        ("bitwise-bit-set?", b_bitwise_bit_set_p),
        // type predicates
        ("number?", b_number_p),
        ("integer?", b_integer_p),
        ("boolean?", b_boolean_p),
        ("pair?", b_pair_p),
        ("null?", b_null_p),
        ("symbol?", b_symbol_p),
        ("string?", b_string_p),
        ("procedure?", b_procedure_p),
        ("char?", b_char_p),
        ("vector?", b_vector_p),
        // pairs / lists
        ("cons", b_cons),
        ("car", b_car),
        ("cdr", b_cdr),
        // cXXr compositional accessors, depths 2..4
        ("caar", b_caar),
        ("cadr", b_cadr),
        ("cdar", b_cdar),
        ("cddr", b_cddr),
        ("caaar", b_caaar),
        ("caadr", b_caadr),
        ("cadar", b_cadar),
        ("caddr", b_caddr),
        ("cdaar", b_cdaar),
        ("cdadr", b_cdadr),
        ("cddar", b_cddar),
        ("cdddr", b_cdddr),
        ("caaaar", b_caaaar),
        ("caaadr", b_caaadr),
        ("caadar", b_caadar),
        ("caaddr", b_caaddr),
        ("cadaar", b_cadaar),
        ("cadadr", b_cadadr),
        ("caddar", b_caddar),
        ("cadddr", b_cadddr),
        ("cdaaar", b_cdaaar),
        ("cdaadr", b_cdaadr),
        ("cdadar", b_cdadar),
        ("cdaddr", b_cdaddr),
        ("cddaar", b_cddaar),
        ("cddadr", b_cddadr),
        ("cdddar", b_cdddar),
        ("cddddr", b_cddddr),
        ("set-car!", b_set_car),
        ("set-cdr!", b_set_cdr),
        ("list", b_list),
        ("length", b_length),
        ("reverse", b_reverse),
        ("append", b_append),
        ("list-tail", b_list_tail),
        ("list-ref", b_list_ref),
        // equality
        ("eq?", b_eq),
        ("eqv?", b_eqv),
        ("equal?", b_equal),
        // logical
        ("not", b_not),
        // strings
        ("string-length", b_string_length),
        ("string=?", b_string_eq),
        ("string-ref", b_string_ref),
        ("string->list", b_string_to_list),
        ("list->string", b_list_to_string),
        ("string-append", b_string_append),
        // characters
        ("char=?", b_char_eq),
        ("char<?", b_char_lt),
        ("char->integer", b_char_to_integer),
        ("integer->char", b_integer_to_char),
        ("char-upcase", b_char_upcase),
        ("char-downcase", b_char_downcase),
        ("char-alphabetic?", b_char_alphabetic),
        ("char-numeric?", b_char_numeric),
        ("char-whitespace?", b_char_whitespace),
        ("char-upper-case?", b_char_upper_case),
        ("char-lower-case?", b_char_lower_case),
        // eof
        ("eof-object?", b_eof_object_p),
        ("eof-object", b_eof_object),
        // (symbol->string and string->symbol are higher-order — see below)
        // numbers
        ("exact", b_exact),
        ("inexact", b_inexact),
        ("exact?", b_exact_p),
        ("inexact?", b_inexact_p),
        // string conversions
        ("make-string", b_make_string),
        ("substring", b_substring),
        ("string-copy", b_string_copy),
        ("number->string", b_number_to_string),
        ("string->number", b_string_to_number),
        // vectors
        ("make-vector", b_make_vector),
        ("vector", b_vector),
        ("vector-length", b_vector_length),
        ("vector-ref", b_vector_ref),
        ("vector-set!", b_vector_set),
        ("vector-fill!", b_vector_fill),
        ("vector->list", b_vector_to_list),
        ("list->vector", b_list_to_vector),
        // assoc lists
        ("assoc", b_assoc),
        ("assv", b_assv),
        ("assq", b_assq),
        // member family
        ("member", b_member),
        ("memv", b_memv),
        ("memq", b_memq),
        // strings (case)
        ("string-upcase", b_string_upcase),
        ("string-downcase", b_string_downcase),
        ("string-foldcase", b_string_foldcase),
        ("string-titlecase", b_string_titlecase),
        ("string-prefix?", b_string_prefix_p),
        ("string-suffix?", b_string_suffix_p),
        ("string-take", b_string_take),
        ("string-drop", b_string_drop),
        ("string-take-right", b_string_take_right),
        ("string-drop-right", b_string_drop_right),
        ("string-pad", b_string_pad),
        ("string-pad-right", b_string_pad_right),
        ("string<?", b_string_lt),
        ("string<=?", b_string_le),
        ("string>?", b_string_gt),
        ("string>=?", b_string_ge),
        ("string-trim", b_string_trim),
        ("string-trim-left", b_string_trim_left),
        ("string-trim-right", b_string_trim_right),
        ("string-contains", b_string_contains),
        ("string-index", b_string_index),
        ("string-split", b_string_split),
        ("string-join", b_string_join),
        ("string->vector", b_string_to_vector),
        ("vector->string", b_vector_to_string),
        ("string-reverse", b_string_reverse),
        // condition predicates
        ("condition?", b_condition_p),
        ("error-object?", b_error_object_p),
        ("error-object-message", b_error_object_message),
        ("error-object-irritants", b_error_object_irritants),
        ("assertion-violation?", b_assertion_violation_p),
        // R6RS standard condition types — constructors
        ("make-message-condition", b_make_message_condition),
        ("make-irritants-condition", b_make_irritants_condition),
        ("make-warning", b_make_warning),
        ("make-serious-condition", b_make_serious_condition),
        ("make-error", b_make_error),
        ("make-violation", b_make_violation),
        ("make-assertion-violation", b_make_assertion_violation),
        (
            "make-non-continuable-violation",
            b_make_non_continuable_violation,
        ),
        ("make-who-condition", b_make_who_condition),
        // R6RS standard condition types — predicates
        ("message-condition?", b_message_condition_p),
        ("irritants-condition?", b_irritants_condition_p),
        ("warning?", b_warning_p),
        ("serious-condition?", b_serious_condition_p),
        ("error?", b_error_p),
        ("violation?", b_violation_p),
        ("non-continuable-violation?", b_non_continuable_violation_p),
        ("who-condition?", b_who_condition_p),
        // R6RS standard condition types — accessors
        ("condition-message", b_condition_message),
        ("condition-irritants", b_condition_irritants),
        ("condition-who", b_condition_who),
        // R6RS condition compounding
        ("condition", b_condition),
        ("simple-conditions", b_simple_conditions),
        // helpers used by code generated from `define-condition-type`
        ("condition-register-parent!", b_condition_register_parent),
        ("condition-instance-of?", b_condition_instance_of),
        ("condition-field-ref", b_condition_field_ref),
        ("make-simple-condition", b_make_simple_condition),
        // copy variants
        ("vector-copy", b_vector_copy),
        ("vector-copy!", b_vector_copy_bang),
        ("bytevector-copy!", b_bytevector_copy_bang),
        ("string-copy!", b_string_copy_bang),
        // bytevectors
        ("make-bytevector", b_make_bytevector),
        ("bytevector", b_bytevector),
        ("bytevector?", b_bytevector_p),
        ("bytevector-length", b_bytevector_length),
        ("bytevector-u8-ref", b_bytevector_u8_ref),
        ("bytevector-u8-set!", b_bytevector_u8_set),
        ("bytevector-copy", b_bytevector_copy),
        ("bytevector->u8-list", b_bytevector_to_u8_list),
        ("u8-list->bytevector", b_u8_list_to_bytevector),
        ("bytevector-append", b_bytevector_append),
        ("bytevector-fill!", b_bytevector_fill),
        ("string->utf8", b_string_to_utf8),
        ("utf8->string", b_utf8_to_string),
        // file ports (R6RS file I/O)
        ("file-exists?", b_file_exists_p),
        ("delete-file", b_delete_file),
        ("open-input-file", b_open_input_file),
        ("open-output-file", b_open_output_file),
        ("close-port", b_close_port),
        ("port-eof?", b_port_eof_p),
        // ports
        ("open-string-input-port", b_open_string_input_port),
        ("open-string-output-port", b_open_string_output_port),
        ("open-bytevector-input-port", b_open_bytevector_input_port),
        ("open-bytevector-output-port", b_open_bytevector_output_port),
        ("get-bytevector-output-port", b_get_bytevector_output_port),
        ("get-u8", b_get_u8),
        ("lookahead-u8", b_lookahead_u8),
        ("put-u8", b_put_u8),
        ("get-bytevector-n", b_get_bytevector_n),
        ("binary-port?", b_binary_port_p),
        ("textual-port?", b_textual_port_p),
        ("get-output-string", b_get_output_string),
        ("read-char", b_read_char),
        ("peek-char", b_peek_char),
        ("get-line", b_get_line),
        ("port?", b_port_p),
        ("input-port?", b_input_port_p),
        ("output-port?", b_output_port_p),
        ("write-char", b_write_char),
        ("write-string", b_write_string),
        // promises
        ("promise?", b_promise_p),
        ("make-promise", b_make_promise),
        // simple list ops (no procedure callback)
        ("iota", b_iota),
        ("last", b_last),
        ("last-pair", b_last_pair),
        ("take", b_take),
        ("drop", b_drop),
        ("zip", b_zip),
        // hashtables
        ("make-eq-hashtable", b_make_eq_hashtable),
        ("make-eqv-hashtable", b_make_eqv_hashtable),
        ("make-hashtable", b_make_hashtable),
        ("string-hash", b_string_hash),
        ("symbol-hash", b_symbol_hash_pure),
        ("equal-hash", b_equal_hash),
        ("hashtable?", b_hashtable_p),
        ("hashtable-size", b_hashtable_size),
        ("hashtable-set!", b_hashtable_set),
        ("hashtable-ref", b_hashtable_ref),
        ("hashtable-contains?", b_hashtable_contains),
        ("hashtable-delete!", b_hashtable_delete),
        ("hashtable-keys", b_hashtable_keys),
        ("hashtable-values", b_hashtable_values),
        ("hashtable-clear!", b_hashtable_clear),
        ("make-parameter", b_make_parameter),
        // SRFI-1 list ops (pure)
        ("delete", b_delete),
        ("delete-duplicates", b_delete_duplicates),
        ("concatenate", b_concatenate),
        ("cons*", b_cons_star),
        ("list*", b_cons_star),
        ("alist-copy", b_alist_copy),
        ("first", b_first),
        ("second", b_second),
        ("third", b_third),
        // hashtable conversions
        ("hashtable->alist", b_hashtable_to_alist),
        ("alist->hashtable", b_alist_to_hashtable),
        // (hashtable-update! is higher-order — see below)
        // R7RS portability
        ("crabscheme-version", b_crabscheme_version),
    ]
}

pub fn higher_order_builtins() -> Vec<HoEntry> {
    vec![
        ("apply", b_apply),
        ("map", b_map),
        ("for-each", b_for_each),
        ("display", b_display),
        ("write", b_write),
        ("newline", b_newline),
        ("raise", b_raise),
        ("display-condition", b_display_condition),
        // raise-continuable: in proper R6RS, the current exception handler
        // is invoked synchronously and its return value becomes the value
        // of the call to raise-continuable. Our `raise` already routes the
        // handler's return through `with-exception-handler`, so for the
        // foundation milestone we expose raise-continuable as an alias.
        // True per-handler continuable semantics (with the previous
        // handler being current during the called handler's body) is a
        // future iteration once a handler stack lands.
        ("raise-continuable", b_raise),
        ("error", b_error_ho),
        ("assertion-violation", b_assertion_violation),
        ("with-exception-handler", b_with_exception_handler),
        ("symbol->string", b_symbol_to_string_ho),
        ("string->symbol", b_string_to_symbol_ho),
        ("hashtable-update!", b_hashtable_update_ho),
        ("hashtable-walk", b_hashtable_walk),
        ("values", b_values),
        ("call-with-values", b_call_with_values),
        ("call/cc", b_call_cc),
        ("call-with-current-continuation", b_call_cc),
        // SRFI-1 higher-order list ops
        ("filter", b_filter),
        ("take-while", b_take_while),
        ("drop-while", b_drop_while),
        ("span", b_span),
        ("break", b_break),
        ("list-index", b_list_index),
        ("filter-map", b_filter_map),
        ("append-map", b_append_map),
        ("list-tabulate", b_list_tabulate),
        ("fold-left", b_fold_left),
        ("fold-right", b_fold_right),
        ("reduce", b_reduce),
        ("find", b_find),
        ("count", b_count),
        ("any", b_any),
        ("every", b_every),
        ("for-all", b_every),
        ("exists", b_any),
        ("partition", b_partition),
        ("force", b_force),
        ("dynamic-wind", b_dynamic_wind),
        ("with-input-from-string", b_with_input_from_string),
        ("with-output-to-string", b_with_output_to_string),
        ("with-output-to-file", b_with_output_to_file),
        ("with-input-from-file", b_with_input_from_file),
        ("current-input-port", b_current_input_port),
        ("current-output-port", b_current_output_port),
        ("gensym", b_gensym),
        ("eval", b_eval),
        ("environment", b_environment),
        ("interaction-environment", b_interaction_environment),
        ("div-and-mod", b_div_and_mod),
        ("div0-and-mod0", b_div0_and_mod0),
        ("features", b_features),
        // vector higher-order
        ("vector-map", b_vector_map),
        ("vector-for-each", b_vector_for_each),
        // port-aware read
        ("read", b_read),
        ("read-line", b_read_line_implicit),
        ("get-string-all", b_get_string_all),
        // SRFI-1 (higher-order)
        ("tabulate", b_tabulate),
        ("remove", b_remove),
        ("string-map", b_string_map),
        ("string-for-each", b_string_for_each),
        ("vector-filter", b_vector_filter),
        ("vector-fold", b_vector_fold),
        // sorting (R6RS)
        ("list-sort", b_list_sort),
        ("vector-sort", b_vector_sort),
        ("vector-sort!", b_vector_sort_bang),
        // SRFI-1 extras
        ("unfold", b_unfold),
        ("zip-with", b_zip_with),
        // hashtable HO
        ("hashtable-fold", b_hashtable_fold),
        ("hashtable-for-each", b_hashtable_for_each),
    ]
}

pub fn install_into(env: &crate::env::Frame, syms: &mut SymbolTable) {
    // Reset the thread-local condition registry so this Runtime starts from
    // a clean standard hierarchy. User-defined condition types from earlier
    // Runtimes on the same thread won't leak into this one.
    init_condition_registry();
    for (name, f) in pure_builtins() {
        let sym = syms.intern(name);
        env.define(sym, make_builtin_pure(name, f));
    }
    for (name, f) in higher_order_builtins() {
        let sym = syms.intern(name);
        env.define(sym, make_builtin_higher(name, f));
    }
    // Global record-type ancestor registry. The expander emits
    // (hashtable-set! __record-parents__ '<my-tag> '(<parent-tag> ...))
    // calls at every (define-record-type ... (parent ...) ...) site, and
    // record predicates consult it so a `point?` test against a `cpoint`
    // instance succeeds. See the cs-expand `expand_define_record_type`.
    let registry_sym = syms.intern(RECORD_PARENTS_REGISTRY);
    env.define(registry_sym, Value::Hashtable(Hashtable::new(HtEqKind::Eq)));
    let _ = BuiltinFn::Pure;
}

/// Name of the global hashtable that maps a record-type's leaf tag symbol
/// to the list of its ancestor tag symbols (immediate parent first, root
/// last). See `expand_define_record_type` for how it's populated.
pub const RECORD_PARENTS_REGISTRY: &str = "__record-parents__";

fn arity_err(name: &str, expected: &str, got: usize) -> String {
    format!("{}: expected {} arguments, got {}", name, expected, got)
}

fn type_err(name: &str, expected: &str, got: &Value) -> String {
    // Include a short display of the offending value where it can be
    // rendered without a SymbolTable handle. Symbols print as their
    // internal handle via Display, which is unhelpful — leave them out.
    // Cap the rendered length so giant values don't blow up the message.
    let extra = match got {
        Value::String(_) | Value::Number(_) | Value::Boolean(_) | Value::Character(_) => {
            let display = format!("{}", got);
            let cap = 60;
            let trimmed: String = if display.chars().count() > cap {
                let head: String = display.chars().take(cap - 1).collect();
                format!("{}…", head)
            } else {
                display
            };
            format!(" {}", trimmed)
        }
        _ => String::new(),
    };
    // Stash the offending value so the dispatcher can attach it as an
    // &irritants simple when this string Err is converted into a raised
    // condition. Drained by `builtin_err_to_eval` / VM equivalent.
    cs_core::stash_builtin_err_irritant(got.clone());
    format!(
        "{}: expected {}, got {}{}",
        name,
        expected,
        got.type_name(),
        extra
    )
}

fn as_num(name: &str, v: &Value) -> Result<Number, String> {
    match v {
        Value::Number(n) => Ok(n.clone()),
        _ => Err(type_err(name, "number", v)),
    }
}

fn as_int_i64(name: &str, v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(Number::Fixnum(n)) => Ok(*n),
        Value::Number(Number::Big(b)) => b
            .to_i64()
            .ok_or_else(|| format!("{}: integer out of range for i64", name)),
        _ => Err(type_err(name, "integer", v)),
    }
}

fn b_add(args: &[Value]) -> Result<Value, String> {
    let mut acc = Number::Fixnum(0);
    for a in args {
        acc = acc.add(&as_num("+", a)?);
    }
    Ok(Value::Number(acc))
}

fn b_sub(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("-", "at least 1", 0));
    }
    if args.len() == 1 {
        let n = as_num("-", &args[0])?;
        return Ok(Value::Number(n.neg()));
    }
    let mut acc = as_num("-", &args[0])?;
    for a in &args[1..] {
        acc = acc.sub(&as_num("-", a)?);
    }
    Ok(Value::Number(acc))
}

fn b_mul(args: &[Value]) -> Result<Value, String> {
    let mut acc = Number::Fixnum(1);
    for a in args {
        acc = acc.mul(&as_num("*", a)?);
    }
    Ok(Value::Number(acc))
}

fn b_div(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("/", "at least 1", 0));
    }
    if args.len() == 1 {
        let one = Number::Fixnum(1);
        let n = as_num("/", &args[0])?;
        return one
            .div(&n)
            .map(Value::Number)
            .map_err(|_| "division by zero".into());
    }
    let mut acc = as_num("/", &args[0])?;
    for a in &args[1..] {
        let n = as_num("/", a)?;
        acc = acc.div(&n).map_err(|_| "division by zero".to_string())?;
    }
    Ok(Value::Number(acc))
}

fn b_num_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let first = as_num("=", &args[0])?;
    for a in &args[1..] {
        if !first.eq_value(&as_num("=", a)?) {
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
}

fn cmp_chain(
    name: &str,
    args: &[Value],
    pred: fn(std::cmp::Ordering) -> bool,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = as_num(name, &args[0])?;
    for a in &args[1..] {
        let cur = as_num(name, a)?;
        if !pred(prev.cmp(&cur)) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_lt(args: &[Value]) -> Result<Value, String> {
    cmp_chain("<", args, |o| o == std::cmp::Ordering::Less)
}

fn b_gt(args: &[Value]) -> Result<Value, String> {
    cmp_chain(">", args, |o| o == std::cmp::Ordering::Greater)
}

fn b_le(args: &[Value]) -> Result<Value, String> {
    cmp_chain("<=", args, |o| o != std::cmp::Ordering::Greater)
}

fn b_ge(args: &[Value]) -> Result<Value, String> {
    cmp_chain(">=", args, |o| o != std::cmp::Ordering::Less)
}

fn b_zero(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("zero?", "1", args.len()));
    }
    Ok(Value::Boolean(as_num("zero?", &args[0])?.is_zero()))
}

fn b_positive(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("positive?", "1", args.len()));
    }
    let n = as_num("positive?", &args[0])?;
    Ok(Value::Boolean(
        n.cmp(&Number::Fixnum(0)) == std::cmp::Ordering::Greater,
    ))
}

fn b_negative(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("negative?", "1", args.len()));
    }
    let n = as_num("negative?", &args[0])?;
    Ok(Value::Boolean(
        n.cmp(&Number::Fixnum(0)) == std::cmp::Ordering::Less,
    ))
}

fn b_abs(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("abs", "1", args.len()));
    }
    Ok(Value::Number(as_num("abs", &args[0])?.abs()))
}

fn b_min(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("min", "at least 1", 0));
    }
    let mut acc = as_num("min", &args[0])?;
    for a in &args[1..] {
        let cur = as_num("min", a)?;
        if cur.cmp(&acc) == std::cmp::Ordering::Less {
            acc = cur;
        }
    }
    Ok(Value::Number(acc))
}

fn b_max(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("max", "at least 1", 0));
    }
    let mut acc = as_num("max", &args[0])?;
    for a in &args[1..] {
        let cur = as_num("max", a)?;
        if cur.cmp(&acc) == std::cmp::Ordering::Greater {
            acc = cur;
        }
    }
    Ok(Value::Number(acc))
}

fn b_quotient(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("quotient", "2", args.len()));
    }
    let a = as_int_i64("quotient", &args[0])?;
    let b = as_int_i64("quotient", &args[1])?;
    if b == 0 {
        return Err("quotient: division by zero".into());
    }
    Ok(Value::fixnum(a.wrapping_div(b)))
}

fn b_remainder(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("remainder", "2", args.len()));
    }
    let a = as_int_i64("remainder", &args[0])?;
    let b = as_int_i64("remainder", &args[1])?;
    if b == 0 {
        return Err("remainder: division by zero".into());
    }
    Ok(Value::fixnum(a.wrapping_rem(b)))
}

fn b_modulo(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("modulo", "2", args.len()));
    }
    let a = as_int_i64("modulo", &args[0])?;
    let b = as_int_i64("modulo", &args[1])?;
    if b == 0 {
        return Err("modulo: division by zero".into());
    }
    let r = a.wrapping_rem(b);
    let m = if (r > 0 && b < 0) || (r < 0 && b > 0) {
        r + b
    } else {
        r
    };
    Ok(Value::fixnum(m))
}

// ---- R6RS Euclidean division (div / mod / div-and-mod / div0 / mod0) ----
//
// `(div x y)` returns nd such that 0 ≤ x − y·nd < |y|. So `mod` is always
// non-negative regardless of the signs of x and y. `div0`/`mod0` use
// centered division: mod0 is in [−|y|/2, |y|/2). `*-and-*` versions
// return both via `(values d m)`.

/// Public so `Runtime::new` can plumb VM-tier shims that need to
/// compute Euclidean div/mod and stash the (d, m) pair via the VM's
/// pending-values channel.
pub fn div_and_mod_i64(x: &Value, y: &Value) -> Result<(i64, i64), String> {
    let xi = as_int_i64("div-and-mod", x)?;
    let yi = as_int_i64("div-and-mod", y)?;
    let d = r6rs_div_i64(xi, yi)?;
    let m = xi - d * yi;
    Ok((d, m))
}

pub fn div0_and_mod0_i64(x: &Value, y: &Value) -> Result<(i64, i64), String> {
    let xi = as_int_i64("div0-and-mod0", x)?;
    let yi = as_int_i64("div0-and-mod0", y)?;
    let d = r6rs_div0_i64(xi, yi)?;
    let m = xi - d * yi;
    Ok((d, m))
}

fn r6rs_div_i64(x: i64, y: i64) -> Result<i64, String> {
    if y == 0 {
        return Err("division by zero".into());
    }
    let q = x.wrapping_div(y);
    let r = x.wrapping_sub(q.wrapping_mul(y));
    let q = if r < 0 {
        if y > 0 {
            q - 1
        } else {
            q + 1
        }
    } else {
        q
    };
    Ok(q)
}

fn r6rs_mod_i64(x: i64, y: i64) -> Result<i64, String> {
    let q = r6rs_div_i64(x, y)?;
    Ok(x - q * y)
}

fn r6rs_div0_i64(x: i64, y: i64) -> Result<i64, String> {
    let d = r6rs_div_i64(x, y)?;
    let m = x - d * y;
    // Centered: shift by 1 if m exceeds |y|/2 (ties broken downward to
    // match the half-open interval [-|y|/2, |y|/2)).
    let abs_y = y.unsigned_abs() as i128;
    let twice_m = (m as i128) * 2;
    let shift = if twice_m > abs_y as i128 {
        1
    } else if twice_m == abs_y as i128 && abs_y % 2 == 0 {
        1
    } else {
        0
    };
    if shift == 1 {
        if y > 0 {
            Ok(d + 1)
        } else {
            Ok(d - 1)
        }
    } else {
        Ok(d)
    }
}

fn r6rs_mod0_i64(x: i64, y: i64) -> Result<i64, String> {
    let d0 = r6rs_div0_i64(x, y)?;
    Ok(x - d0 * y)
}

fn b_div_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div", "2", args.len()));
    }
    let x = as_int_i64("div", &args[0])?;
    let y = as_int_i64("div", &args[1])?;
    Ok(Value::fixnum(
        r6rs_div_i64(x, y).map_err(|m| format!("div: {}", m))?,
    ))
}

fn b_mod_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("mod", "2", args.len()));
    }
    let x = as_int_i64("mod", &args[0])?;
    let y = as_int_i64("mod", &args[1])?;
    Ok(Value::fixnum(
        r6rs_mod_i64(x, y).map_err(|m| format!("mod: {}", m))?,
    ))
}

fn b_div0_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div0", "2", args.len()));
    }
    let x = as_int_i64("div0", &args[0])?;
    let y = as_int_i64("div0", &args[1])?;
    Ok(Value::fixnum(
        r6rs_div0_i64(x, y).map_err(|m| format!("div0: {}", m))?,
    ))
}

fn b_mod0_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("mod0", "2", args.len()));
    }
    let x = as_int_i64("mod0", &args[0])?;
    let y = as_int_i64("mod0", &args[1])?;
    Ok(Value::fixnum(
        r6rs_mod0_i64(x, y).map_err(|m| format!("mod0: {}", m))?,
    ))
}

fn b_div_and_mod(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div-and-mod", "2", args.len()));
    }
    let x = as_int_i64("div-and-mod", &args[0])?;
    let y = as_int_i64("div-and-mod", &args[1])?;
    let d = r6rs_div_i64(x, y).map_err(|m| format!("div-and-mod: {}", m))?;
    let m = x - d * y;
    ctx.pending_values = Some(vec![Value::fixnum(d), Value::fixnum(m)]);
    Ok(Value::Unspecified)
}

fn b_div0_and_mod0(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div0-and-mod0", "2", args.len()));
    }
    let x = as_int_i64("div0-and-mod0", &args[0])?;
    let y = as_int_i64("div0-and-mod0", &args[1])?;
    let d = r6rs_div0_i64(x, y).map_err(|m| format!("div0-and-mod0: {}", m))?;
    let m = x - d * y;
    ctx.pending_values = Some(vec![Value::fixnum(d), Value::fixnum(m)]);
    Ok(Value::Unspecified)
}

fn b_expt(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("expt", "2", args.len()));
    }
    let base = as_num("expt", &args[0])?;
    let exp = as_num("expt", &args[1])?;
    // Integer power case via repeated multiplication if exp is non-negative integer.
    if let (Number::Fixnum(_), Number::Fixnum(e)) = (&base, &exp) {
        if *e >= 0 && *e < 64 {
            let mut acc = Number::Fixnum(1);
            let mut i = 0;
            while i < *e {
                acc = acc.mul(&base);
                i += 1;
            }
            return Ok(Value::Number(acc));
        }
    }
    // Fallback: floating point.
    let r = base.to_f64().powf(exp.to_f64());
    Ok(Value::flonum(r))
}

fn b_number_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("number?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Number(_))))
}

fn b_integer_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("integer?", "1", args.len()));
    }
    Ok(Value::Boolean(match &args[0] {
        Value::Number(n) => n.is_integer(),
        _ => false,
    }))
}

fn b_boolean_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("boolean?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Boolean(_))))
}

fn b_pair_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("pair?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Pair(_))))
}

fn b_null_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("null?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Null)))
}

fn b_symbol_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("symbol?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Symbol(_))))
}

fn b_string_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::String(_))))
}

fn b_procedure_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("procedure?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Procedure(_))))
}

fn b_char_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Character(_))))
}

fn b_vector_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("vector?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Vector(_))))
}

fn b_cons(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("cons", "2", args.len()));
    }
    Ok(Value::Pair(Pair::new(args[0].clone(), args[1].clone())))
}

fn b_car(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("car", "1", args.len()));
    }
    match &args[0] {
        Value::Pair(p) => Ok(p.car.borrow().clone()),
        v => Err(type_err("car", "pair", v)),
    }
}

fn b_cdr(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("cdr", "1", args.len()));
    }
    match &args[0] {
        Value::Pair(p) => Ok(p.cdr.borrow().clone()),
        v => Err(type_err("cdr", "pair", v)),
    }
}

// ---- R6RS compositional pair accessors (cXXr depth 2..4) ----
//
// Each of these is a fixed sequence of car/cdr applied right-to-left.
// `(cadr xs)` means `(car (cdr xs))`. The dispatcher walks the pattern
// once per call. Spelled out via macro so each public name resolves
// directly to a `fn(&[Value]) -> Result<Value, String>` for registration.

fn cxr_apply(name: &str, ops: &str, mut v: Value) -> Result<Value, String> {
    // Right-to-left: caXr means apply X first, then car at the end.
    for c in ops.chars().rev() {
        v = match (c, &v) {
            ('a', Value::Pair(p)) => p.car.borrow().clone(),
            ('d', Value::Pair(p)) => p.cdr.borrow().clone(),
            (_, other) => return Err(type_err(name, "pair", other)),
        };
    }
    Ok(v)
}

macro_rules! cxr_fn {
    ($fname:ident, $sname:expr, $ops:expr) => {
        fn $fname(args: &[Value]) -> Result<Value, String> {
            if args.len() != 1 {
                return Err(arity_err($sname, "1", args.len()));
            }
            cxr_apply($sname, $ops, args[0].clone())
        }
    };
}

// depth 2 (4)
cxr_fn!(b_caar, "caar", "aa");
cxr_fn!(b_cadr, "cadr", "ad");
cxr_fn!(b_cdar, "cdar", "da");
cxr_fn!(b_cddr, "cddr", "dd");
// depth 3 (8)
cxr_fn!(b_caaar, "caaar", "aaa");
cxr_fn!(b_caadr, "caadr", "aad");
cxr_fn!(b_cadar, "cadar", "ada");
cxr_fn!(b_caddr, "caddr", "add");
cxr_fn!(b_cdaar, "cdaar", "daa");
cxr_fn!(b_cdadr, "cdadr", "dad");
cxr_fn!(b_cddar, "cddar", "dda");
cxr_fn!(b_cdddr, "cdddr", "ddd");
// depth 4 (16)
cxr_fn!(b_caaaar, "caaaar", "aaaa");
cxr_fn!(b_caaadr, "caaadr", "aaad");
cxr_fn!(b_caadar, "caadar", "aada");
cxr_fn!(b_caaddr, "caaddr", "aadd");
cxr_fn!(b_cadaar, "cadaar", "adaa");
cxr_fn!(b_cadadr, "cadadr", "adad");
cxr_fn!(b_caddar, "caddar", "adda");
cxr_fn!(b_cadddr, "cadddr", "addd");
cxr_fn!(b_cdaaar, "cdaaar", "daaa");
cxr_fn!(b_cdaadr, "cdaadr", "daad");
cxr_fn!(b_cdadar, "cdadar", "dada");
cxr_fn!(b_cdaddr, "cdaddr", "dadd");
cxr_fn!(b_cddaar, "cddaar", "ddaa");
cxr_fn!(b_cddadr, "cddadr", "ddad");
cxr_fn!(b_cdddar, "cdddar", "ddda");
cxr_fn!(b_cddddr, "cddddr", "dddd");

fn b_set_car(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("set-car!", "2", args.len()));
    }
    match &args[0] {
        Value::Pair(p) => {
            *p.car.borrow_mut() = args[1].clone();
            Ok(Value::Unspecified)
        }
        v => Err(type_err("set-car!", "pair", v)),
    }
}

fn b_set_cdr(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("set-cdr!", "2", args.len()));
    }
    match &args[0] {
        Value::Pair(p) => {
            *p.cdr.borrow_mut() = args[1].clone();
            Ok(Value::Unspecified)
        }
        v => Err(type_err("set-cdr!", "pair", v)),
    }
}

fn b_list(args: &[Value]) -> Result<Value, String> {
    Ok(Value::list(args.iter().cloned()))
}

fn b_length(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("length", "1", args.len()));
    }
    let mut n: i64 = 0;
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Null => return Ok(Value::fixnum(n)),
            Value::Pair(p) => {
                n += 1;
                cur = p.cdr.borrow().clone();
            }
            v => return Err(type_err("length", "proper list", &v)),
        }
    }
}

fn b_reverse(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("reverse", "1", args.len()));
    }
    let mut acc = Value::Null;
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Null => return Ok(acc),
            Value::Pair(p) => {
                acc = Value::Pair(Pair::new(p.car.borrow().clone(), acc));
                cur = p.cdr.borrow().clone();
            }
            v => return Err(type_err("reverse", "proper list", &v)),
        }
    }
}

fn b_append(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    if args.len() == 1 {
        return Ok(args[0].clone());
    }
    let mut items: Vec<Value> = Vec::new();
    for a in &args[..args.len() - 1] {
        let mut cur = a.clone();
        loop {
            match cur {
                Value::Null => break,
                Value::Pair(p) => {
                    items.push(p.car.borrow().clone());
                    cur = p.cdr.borrow().clone();
                }
                v => return Err(type_err("append", "proper list", &v)),
            }
        }
    }
    let mut acc = args[args.len() - 1].clone();
    while let Some(item) = items.pop() {
        acc = Value::Pair(Pair::new(item, acc));
    }
    Ok(acc)
}

fn b_list_tail(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("list-tail", "2", args.len()));
    }
    let n = as_int_i64("list-tail", &args[1])?;
    if n < 0 {
        return Err("list-tail: negative index".into());
    }
    let mut cur = args[0].clone();
    let mut i: i64 = 0;
    while i < n {
        match cur {
            Value::Pair(p) => {
                cur = p.cdr.borrow().clone();
                i += 1;
            }
            _ => return Err("list-tail: index out of range".into()),
        }
    }
    Ok(cur)
}

fn b_list_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("list-ref", "2", args.len()));
    }
    let tail = b_list_tail(args)?;
    match tail {
        Value::Pair(p) => Ok(p.car.borrow().clone()),
        _ => Err("list-ref: index out of range".into()),
    }
}

fn b_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("eq?", "2", args.len()));
    }
    Ok(Value::Boolean(eq::eq(&args[0], &args[1])))
}

fn b_eqv(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("eqv?", "2", args.len()));
    }
    Ok(Value::Boolean(eq::eqv(&args[0], &args[1])))
}

fn b_equal(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("equal?", "2", args.len()));
    }
    Ok(Value::Boolean(eq::equal(&args[0], &args[1])))
}

fn b_not(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("not", "1", args.len()));
    }
    Ok(Value::Boolean(!args[0].is_truthy()))
}

fn b_string_length(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-length", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::fixnum(s.borrow().chars().count() as i64)),
        v => Err(type_err("string-length", "string", v)),
    }
}

fn b_string_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let s0 = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string=?", "string", v)),
    };
    for a in &args[1..] {
        match a {
            Value::String(s) => {
                if *s.borrow() != s0 {
                    return Ok(Value::Boolean(false));
                }
            }
            v => return Err(type_err("string=?", "string", v)),
        }
    }
    Ok(Value::Boolean(true))
}

fn b_string_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-ref", "2", args.len()));
    }
    let i = as_int_i64("string-ref", &args[1])?;
    if i < 0 {
        return Err("string-ref: negative index".into());
    }
    match &args[0] {
        Value::String(s) => {
            let s = s.borrow();
            s.chars()
                .nth(i as usize)
                .map(Value::Character)
                .ok_or_else(|| "string-ref: index out of range".into())
        }
        v => Err(type_err("string-ref", "string", v)),
    }
}

fn b_string_to_list(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string->list", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::list(s.borrow().chars().map(Value::Character))),
        v => Err(type_err("string->list", "string", v)),
    }
}

fn b_list_to_string(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("list->string", "1", args.len()));
    }
    let mut s = String::new();
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Null => return Ok(Value::string(s)),
            Value::Pair(p) => {
                let head = p.car.borrow().clone();
                match head {
                    Value::Character(c) => s.push(c),
                    other => return Err(type_err("list->string", "character", &other)),
                }
                cur = p.cdr.borrow().clone();
            }
            v => return Err(type_err("list->string", "list of characters", &v)),
        }
    }
}

fn b_string_append(args: &[Value]) -> Result<Value, String> {
    let mut s = String::new();
    for a in args {
        match a {
            Value::String(part) => s.push_str(&part.borrow()),
            v => return Err(type_err("string-append", "string", v)),
        }
    }
    Ok(Value::string(s))
}

fn b_char_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let first = match &args[0] {
        Value::Character(c) => *c,
        v => return Err(type_err("char=?", "character", v)),
    };
    for a in &args[1..] {
        match a {
            Value::Character(c) => {
                if *c != first {
                    return Ok(Value::Boolean(false));
                }
            }
            v => return Err(type_err("char=?", "character", v)),
        }
    }
    Ok(Value::Boolean(true))
}

fn b_char_lt(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::Character(c) => *c,
        v => return Err(type_err("char<?", "character", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::Character(c) => *c,
            v => return Err(type_err("char<?", "character", v)),
        };
        if !(prev < cur) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_char_to_integer(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char->integer", "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => Ok(Value::fixnum(*c as i64)),
        v => Err(type_err("char->integer", "character", v)),
    }
}

fn b_integer_to_char(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("integer->char", "1", args.len()));
    }
    let n = as_int_i64("integer->char", &args[0])?;
    if !(0..=0x10FFFF).contains(&n) {
        return Err("integer->char: codepoint out of range".into());
    }
    char::from_u32(n as u32)
        .map(Value::Character)
        .ok_or_else(|| "integer->char: not a Unicode scalar".into())
}

fn b_exact(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("exact", "1", args.len()));
    }
    match as_num("exact", &args[0])? {
        n if n.is_exact() => Ok(Value::Number(n)),
        Number::Flonum(f) => {
            if f.fract() == 0.0 && f.is_finite() && (f as i64 as f64) == f {
                Ok(Value::fixnum(f as i64))
            } else {
                Err("exact: cannot represent non-integral flonum exactly (rational coercion not yet supported)".into())
            }
        }
        _ => unreachable!(),
    }
}

fn b_inexact(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("inexact", "1", args.len()));
    }
    let n = as_num("inexact", &args[0])?;
    Ok(Value::flonum(n.to_f64()))
}

fn b_exact_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("exact?", "1", args.len()));
    }
    match &args[0] {
        Value::Number(n) => Ok(Value::Boolean(n.is_exact())),
        v => Err(type_err("exact?", "number", v)),
    }
}

fn b_inexact_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("inexact?", "1", args.len()));
    }
    match &args[0] {
        Value::Number(n) => Ok(Value::Boolean(!n.is_exact())),
        v => Err(type_err("inexact?", "number", v)),
    }
}

/// `(features)` — R7RS portability. Returns a list of feature symbols
/// matching the cond-expand identifiers the expander recognizes.
fn b_features(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("features", "0", args.len()));
    }
    let feats = ["crabscheme", "r6rs-subset", "r7rs-subset", "exact-closed"];
    let syms_list: Vec<Value> = feats
        .iter()
        .map(|n| Value::Symbol(ctx.syms.intern(n)))
        .collect();
    Ok(Value::list(syms_list))
}

/// `(crabscheme-version)` — non-portable, returns the implementation's
/// own version string. Useful for compatibility shims that only need to
/// know they're running on CrabScheme.
fn b_crabscheme_version(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("crabscheme-version", "0", args.len()));
    }
    Ok(Value::string(env!("CARGO_PKG_VERSION")))
}

fn b_newline(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("newline", "0 or 1", args.len()));
    }
    write_output("\n", args.first().cloned(), ctx)
}

// ---- higher-order builtins ----

fn b_apply(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("apply", "at least 2", args.len()));
    }
    let proc_val = &args[0];
    let last = &args[args.len() - 1];
    let mut all: Vec<Value> = args[1..args.len() - 1].to_vec();
    let mut cur = last.clone();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                all.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            v => return Err(type_err("apply", "proper list (last arg)", &v)),
        }
    }
    apply_procedure(proc_val, &all, ctx).map_err(|e| e.message())
}

fn b_map(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("map", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("map", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        let r = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
        out.push(r);
    }
    Ok(Value::list(out))
}

fn b_for_each(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("for-each", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("for-each", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
    }
    Ok(Value::Unspecified)
}

fn collect_proper_list(name: &str, v: &Value) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            other => return Err(type_err(name, "proper list", &other)),
        }
    }
}

fn b_display(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("display", "1 or 2", args.len()));
    }
    let s = args[0].format_with(ctx.syms, WriteMode::Display);
    write_output(&s, args.get(1).cloned(), ctx)
}

fn b_write(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("write", "1 or 2", args.len()));
    }
    let s = args[0].format_with(ctx.syms, WriteMode::Write);
    write_output(&s, args.get(1).cloned(), ctx)
}

fn write_output(s: &str, explicit_port: Option<Value>, ctx: &mut EvalCtx) -> Result<Value, String> {
    let target = explicit_port.or_else(|| ctx.current_output_port.clone());
    match target {
        Some(Value::Port(p)) => match &*p {
            Port::StringOutput(buf) => {
                buf.borrow_mut().push_str(s);
                Ok(Value::Unspecified)
            }
            Port::FileOutput(state) => {
                let mut st = state.borrow_mut();
                if st.closed {
                    return Err("write/display: port is closed".into());
                }
                st.buf.extend_from_slice(s.as_bytes());
                Ok(Value::Unspecified)
            }
            _ => Err("write/display: not an output port".into()),
        },
        Some(v) => Err(type_err("write/display", "output port", &v)),
        None => {
            print!("{}", s);
            Ok(Value::Unspecified)
        }
    }
}

// ---- string conversions ----

fn b_make_string(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("make-string", "1 or 2", args.len()));
    }
    let n = as_int_i64("make-string", &args[0])?;
    if n < 0 {
        return Err("make-string: negative length".into());
    }
    let fill = if args.len() == 2 {
        match &args[1] {
            Value::Character(c) => *c,
            v => return Err(type_err("make-string", "character", v)),
        }
    } else {
        ' '
    };
    let s: String = std::iter::repeat(fill).take(n as usize).collect();
    Ok(Value::string(s))
}

fn b_substring(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("substring", "3", args.len()));
    }
    let start = as_int_i64("substring", &args[1])?;
    let end = as_int_i64("substring", &args[2])?;
    if start < 0 || end < start {
        return Err("substring: invalid bounds".into());
    }
    match &args[0] {
        Value::String(s) => {
            let s = s.borrow();
            let chars: Vec<char> = s.chars().collect();
            if (end as usize) > chars.len() {
                return Err("substring: end out of range".into());
            }
            let sub: String = chars[start as usize..end as usize].iter().collect();
            Ok(Value::string(sub))
        }
        v => Err(type_err("substring", "string", v)),
    }
}

fn b_string_copy(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-copy", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().clone())),
        v => Err(type_err("string-copy", "string", v)),
    }
}

fn b_number_to_string(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("number->string", "1 or 2", args.len()));
    }
    let n = as_num("number->string", &args[0])?;
    let radix = if args.len() == 2 {
        as_int_i64("number->string", &args[1])?
    } else {
        10
    };
    match (radix, &n) {
        (10, _) => Ok(Value::string(format!("{}", n))),
        (2, Number::Fixnum(v)) => Ok(Value::string(format!("{:b}", v))),
        (8, Number::Fixnum(v)) => Ok(Value::string(format!("{:o}", v))),
        (16, Number::Fixnum(v)) => Ok(Value::string(format!("{:x}", v))),
        _ => Err("number->string: unsupported radix or number type".into()),
    }
}

fn b_string_to_number(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("string->number", "1 or 2", args.len()));
    }
    let radix = if args.len() == 2 {
        as_int_i64("string->number", &args[1])?
    } else {
        10
    };
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string->number", "string", v)),
    };
    let parsed = match radix {
        10 => {
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s.parse::<f64>().ok().map(Number::Flonum)
            } else {
                s.parse::<i64>().ok().map(Number::Fixnum)
            }
        }
        2 | 8 | 16 => i64::from_str_radix(&s, radix as u32)
            .ok()
            .map(Number::Fixnum),
        _ => None,
    };
    Ok(parsed.map(Value::Number).unwrap_or(Value::Boolean(false)))
}

// ---- vectors ----

fn b_make_vector(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("make-vector", "1 or 2", args.len()));
    }
    let n = as_int_i64("make-vector", &args[0])?;
    if n < 0 {
        return Err("make-vector: negative length".into());
    }
    let fill = if args.len() == 2 {
        args[1].clone()
    } else {
        Value::Unspecified
    };
    let v: Vec<Value> = std::iter::repeat(fill).take(n as usize).collect();
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(v))))
}

fn b_vector(args: &[Value]) -> Result<Value, String> {
    let v: Vec<Value> = args.to_vec();
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(v))))
}

fn b_vector_length(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("vector-length", "1", args.len()));
    }
    match &args[0] {
        Value::Vector(v) => Ok(Value::fixnum(v.borrow().len() as i64)),
        v => Err(type_err("vector-length", "vector", v)),
    }
}

fn b_vector_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("vector-ref", "2", args.len()));
    }
    let i = as_int_i64("vector-ref", &args[1])?;
    if i < 0 {
        return Err("vector-ref: negative index".into());
    }
    match &args[0] {
        Value::Vector(v) => v
            .borrow()
            .get(i as usize)
            .cloned()
            .ok_or_else(|| "vector-ref: index out of range".into()),
        v => Err(type_err("vector-ref", "vector", v)),
    }
}

fn b_vector_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("vector-set!", "3", args.len()));
    }
    let i = as_int_i64("vector-set!", &args[1])?;
    if i < 0 {
        return Err("vector-set!: negative index".into());
    }
    match &args[0] {
        Value::Vector(v) => {
            let mut v = v.borrow_mut();
            if (i as usize) >= v.len() {
                return Err("vector-set!: index out of range".into());
            }
            v[i as usize] = args[2].clone();
            Ok(Value::Unspecified)
        }
        v => Err(type_err("vector-set!", "vector", v)),
    }
}

fn b_vector_fill(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("vector-fill!", "2", args.len()));
    }
    match &args[0] {
        Value::Vector(v) => {
            let mut v = v.borrow_mut();
            for slot in v.iter_mut() {
                *slot = args[1].clone();
            }
            Ok(Value::Unspecified)
        }
        v => Err(type_err("vector-fill!", "vector", v)),
    }
}

fn b_vector_to_list(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("vector->list", "1", args.len()));
    }
    match &args[0] {
        Value::Vector(v) => Ok(Value::list(v.borrow().iter().cloned())),
        v => Err(type_err("vector->list", "vector", v)),
    }
}

fn b_list_to_vector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("list->vector", "1", args.len()));
    }
    let items = collect_proper_list("list->vector", &args[0])?;
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(
        items,
    ))))
}

// ---- assoc / member family ----

fn b_assoc(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("assoc", "2", args.len()));
    }
    assoc_search("assoc", &args[0], &args[1], eq::equal)
}

fn b_assv(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("assv", "2", args.len()));
    }
    assoc_search("assv", &args[0], &args[1], eq::eqv)
}

fn b_assq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("assq", "2", args.len()));
    }
    assoc_search("assq", &args[0], &args[1], eq::eq)
}

fn assoc_search(
    name: &str,
    key: &Value,
    list: &Value,
    pred: fn(&Value, &Value) -> bool,
) -> Result<Value, String> {
    let mut cur = list.clone();
    loop {
        match cur {
            Value::Null => return Ok(Value::Boolean(false)),
            Value::Pair(p) => {
                let head = p.car.borrow().clone();
                match &head {
                    Value::Pair(pair) => {
                        if pred(&pair.car.borrow(), key) {
                            return Ok(head.clone());
                        }
                    }
                    _ => return Err(type_err(name, "list of pairs", &head)),
                }
                cur = p.cdr.borrow().clone();
            }
            other => return Err(type_err(name, "proper list", &other)),
        }
    }
}

fn b_member(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("member", "2", args.len()));
    }
    member_search("member", &args[0], &args[1], eq::equal)
}

fn b_memv(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("memv", "2", args.len()));
    }
    member_search("memv", &args[0], &args[1], eq::eqv)
}

fn b_memq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("memq", "2", args.len()));
    }
    member_search("memq", &args[0], &args[1], eq::eq)
}

fn member_search(
    name: &str,
    obj: &Value,
    list: &Value,
    pred: fn(&Value, &Value) -> bool,
) -> Result<Value, String> {
    let mut cur = list.clone();
    loop {
        match cur {
            Value::Null => return Ok(Value::Boolean(false)),
            Value::Pair(p) => {
                if pred(&p.car.borrow(), obj) {
                    return Ok(Value::Pair(p));
                }
                cur = p.cdr.borrow().clone();
            }
            other => return Err(type_err(name, "proper list", &other)),
        }
    }
}

// ---- string case + ordering ----

fn b_string_upcase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-upcase", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().to_uppercase())),
        v => Err(type_err("string-upcase", "string", v)),
    }
}

fn b_string_downcase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-downcase", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().to_lowercase())),
        v => Err(type_err("string-downcase", "string", v)),
    }
}

/// `(string-foldcase s)` — R6RS case-folding for case-insensitive
/// comparison. For ASCII this matches `string-downcase`; full Unicode
/// folding (e.g. ß → ss) is not yet implemented.
fn b_string_foldcase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-foldcase", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().to_lowercase())),
        v => Err(type_err("string-foldcase", "string", v)),
    }
}

/// `(string-titlecase s)` — uppercase the first character of every
/// run of word characters, lowercase the rest.
fn b_string_titlecase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-titlecase", "1", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-titlecase", "string", v)),
    };
    let mut out = String::with_capacity(s.len());
    let mut prev_alphabetic = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if !prev_alphabetic {
                for u in c.to_uppercase() {
                    out.push(u);
                }
            } else {
                for u in c.to_lowercase() {
                    out.push(u);
                }
            }
            prev_alphabetic = true;
        } else {
            out.push(c);
            prev_alphabetic = false;
        }
    }
    Ok(Value::string(out))
}

/// `(string-prefix? prefix s)` — true iff `s` starts with `prefix`.
fn b_string_prefix_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-prefix?", "2", args.len()));
    }
    let pre = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-prefix?", "string", v)),
    };
    let s = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-prefix?", "string", v)),
    };
    Ok(Value::Boolean(s.starts_with(&pre)))
}

/// `(string-suffix? suffix s)` — true iff `s` ends with `suffix`.
fn b_string_suffix_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-suffix?", "2", args.len()));
    }
    let suf = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-suffix?", "string", v)),
    };
    let s = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-suffix?", "string", v)),
    };
    Ok(Value::Boolean(s.ends_with(&suf)))
}

/// `(string-take s n)` — first `n` characters of `s` as a fresh string.
fn b_string_take(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-take", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-take", "string", v)),
    };
    let n = as_int_i64("string-take", &args[1])?;
    if n < 0 {
        return Err("string-take: negative count".into());
    }
    let out: String = s.chars().take(n as usize).collect();
    Ok(Value::string(out))
}

/// `(string-drop s n)` — drop the first `n` characters; return the rest.
fn b_string_drop(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-drop", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-drop", "string", v)),
    };
    let n = as_int_i64("string-drop", &args[1])?;
    if n < 0 {
        return Err("string-drop: negative count".into());
    }
    let out: String = s.chars().skip(n as usize).collect();
    Ok(Value::string(out))
}

/// `(string-take-right s n)` — last `n` characters of `s`.
fn b_string_take_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-take-right", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-take-right", "string", v)),
    };
    let n = as_int_i64("string-take-right", &args[1])?;
    if n < 0 {
        return Err("string-take-right: negative count".into());
    }
    let total: usize = s.chars().count();
    let n = (n as usize).min(total);
    let out: String = s.chars().skip(total - n).collect();
    Ok(Value::string(out))
}

/// `(string-drop-right s n)` — drop the last `n` characters.
fn b_string_drop_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-drop-right", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-drop-right", "string", v)),
    };
    let n = as_int_i64("string-drop-right", &args[1])?;
    if n < 0 {
        return Err("string-drop-right: negative count".into());
    }
    let total: usize = s.chars().count();
    let keep = total.saturating_sub(n as usize);
    let out: String = s.chars().take(keep).collect();
    Ok(Value::string(out))
}

/// `(string-pad s width [char])` — left-pad `s` with `char` (default
/// space) so the result has exactly `width` characters. Truncates from
/// the LEFT when `s` is longer than `width`, matching SRFI-13.
fn b_string_pad(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 || args.len() > 3 {
        return Err(arity_err("string-pad", "2 or 3", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-pad", "string", v)),
    };
    let width = as_int_i64("string-pad", &args[1])?;
    if width < 0 {
        return Err("string-pad: negative width".into());
    }
    let pad_char = if args.len() == 3 {
        match &args[2] {
            Value::Character(c) => *c,
            v => return Err(type_err("string-pad", "character", v)),
        }
    } else {
        ' '
    };
    let total = s.chars().count();
    let width = width as usize;
    if total >= width {
        // Truncate from left (SRFI-13 padding semantics keep right side).
        let drop = total - width;
        Ok(Value::string(s.chars().skip(drop).collect::<String>()))
    } else {
        let pad: String = std::iter::repeat(pad_char).take(width - total).collect();
        Ok(Value::string(format!("{}{}", pad, s)))
    }
}

/// `(string-pad-right s width [char])` — right-pad with `char`. Truncates
/// from the RIGHT when `s` is longer than `width`.
fn b_string_pad_right(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 || args.len() > 3 {
        return Err(arity_err("string-pad-right", "2 or 3", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-pad-right", "string", v)),
    };
    let width = as_int_i64("string-pad-right", &args[1])?;
    if width < 0 {
        return Err("string-pad-right: negative width".into());
    }
    let pad_char = if args.len() == 3 {
        match &args[2] {
            Value::Character(c) => *c,
            v => return Err(type_err("string-pad-right", "character", v)),
        }
    } else {
        ' '
    };
    let total = s.chars().count();
    let width = width as usize;
    if total >= width {
        Ok(Value::string(s.chars().take(width).collect::<String>()))
    } else {
        let pad: String = std::iter::repeat(pad_char).take(width - total).collect();
        Ok(Value::string(format!("{}{}", s, pad)))
    }
}

fn string_chain(
    name: &str,
    args: &[Value],
    pred: fn(std::cmp::Ordering) -> bool,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err(name, "string", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::String(s) => s.borrow().clone(),
            v => return Err(type_err(name, "string", v)),
        };
        if !pred(prev.as_str().cmp(cur.as_str())) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_string_lt(args: &[Value]) -> Result<Value, String> {
    string_chain("string<?", args, |o| o == std::cmp::Ordering::Less)
}

fn b_string_le(args: &[Value]) -> Result<Value, String> {
    string_chain("string<=?", args, |o| o != std::cmp::Ordering::Greater)
}

fn b_string_gt(args: &[Value]) -> Result<Value, String> {
    string_chain("string>?", args, |o| o == std::cmp::Ordering::Greater)
}

fn b_string_ge(args: &[Value]) -> Result<Value, String> {
    string_chain("string>=?", args, |o| o != std::cmp::Ordering::Less)
}

// ---- R6RS conditions ----
//
// Representation: every condition is a Vector tagged at slot 0 with a string.
// A *compound* condition is `#("&compound-condition" simple1 simple2 ...)`,
// where each `simple` is itself a vector `#("&<type>" field0 field1 ...)`.
// A "simple" condition produced by a constructor like `make-message-condition`
// is wrapped in a one-element compound for uniformity, so `condition?` is
// always a single check and `simple-conditions` is always a slice.
//
// The standard hierarchy is wired up by `init_condition_registry` at
// runtime startup. User-defined condition types (`define-condition-type`)
// register themselves by calling `condition-register-parent!` in their
// expansion, so user types and standard types share one registry.

const COND_COMPOUND_TAG: &str = "&compound-condition";
const TAG_MESSAGE: &str = "&message";
const TAG_IRRITANTS: &str = "&irritants";
const TAG_WARNING: &str = "&warning";
const TAG_SERIOUS: &str = "&serious";
const TAG_ERROR: &str = "&error";
const TAG_VIOLATION: &str = "&violation";
const TAG_ASSERTION: &str = "&assertion";
const TAG_NON_CONTINUABLE: &str = "&non-continuable";
const TAG_WHO: &str = "&who";
const TAG_CONDITION: &str = "&condition";

thread_local! {
    /// Map from condition tag → its parent tag. Walked by predicates to
    /// decide R6RS subtype relationships. Pre-populated with the standard
    /// hierarchy at every `Runtime::new` (idempotent), and extended by
    /// user-defined types via `(condition-register-parent! tag parent)`.
    ///
    /// `&condition` (the root) is intentionally absent from the map so
    /// the chain walker terminates there.
    static COND_PARENTS: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Set of registered "simple condition" tag strings (i.e. types that
    /// appear in the parent registry, plus `&condition` itself).
    /// `condition?` uses this to decide whether an arbitrary vector with a
    /// `&...` tag is actually a condition or just a vector that happens to
    /// start with the same character.
    static COND_KNOWN_TAGS: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Populate the registry with the standard R6RS condition hierarchy.
/// Called from `Runtime::new` so each runtime starts from a clean state
/// (idempotent — calling twice has no observable effect).
pub fn init_condition_registry() {
    COND_PARENTS.with(|reg| {
        let mut m = reg.borrow_mut();
        m.clear();
        m.insert(TAG_WARNING.into(), TAG_CONDITION.into());
        m.insert(TAG_SERIOUS.into(), TAG_CONDITION.into());
        m.insert(TAG_MESSAGE.into(), TAG_CONDITION.into());
        m.insert(TAG_IRRITANTS.into(), TAG_CONDITION.into());
        m.insert(TAG_WHO.into(), TAG_CONDITION.into());
        m.insert(TAG_ERROR.into(), TAG_SERIOUS.into());
        m.insert(TAG_VIOLATION.into(), TAG_SERIOUS.into());
        m.insert(TAG_ASSERTION.into(), TAG_VIOLATION.into());
        m.insert(TAG_NON_CONTINUABLE.into(), TAG_VIOLATION.into());
    });
    COND_KNOWN_TAGS.with(|reg| {
        let mut s = reg.borrow_mut();
        s.clear();
        for t in [
            TAG_CONDITION,
            TAG_WARNING,
            TAG_SERIOUS,
            TAG_MESSAGE,
            TAG_IRRITANTS,
            TAG_WHO,
            TAG_ERROR,
            TAG_VIOLATION,
            TAG_ASSERTION,
            TAG_NON_CONTINUABLE,
        ] {
            s.insert(t.into());
        }
    });
}

/// True if `child` is `ancestor` or has `ancestor` somewhere in its parent
/// chain. Walks the registry; terminates at `&condition` (the root) or at
/// an unregistered tag.
fn is_descendant_of(child: &str, ancestor: &str) -> bool {
    if child == ancestor {
        return true;
    }
    COND_PARENTS.with(|reg| {
        let map = reg.borrow();
        let mut cur = child.to_string();
        loop {
            match map.get(&cur) {
                Some(p) => {
                    if p == ancestor {
                        return true;
                    }
                    cur = p.clone();
                }
                None => return false,
            }
        }
    })
}

fn is_known_simple_tag(s: &str) -> bool {
    COND_KNOWN_TAGS.with(|reg| reg.borrow().contains(s))
}

fn vec_first_tag(v: &Value) -> Option<String> {
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        if let Some(Value::String(s)) = v.first() {
            return Some(s.borrow().clone());
        }
    }
    None
}

fn is_compound_cond(v: &Value) -> bool {
    matches!(vec_first_tag(v).as_deref(), Some(COND_COMPOUND_TAG))
}

fn is_simple_cond(v: &Value) -> bool {
    if let Some(t) = vec_first_tag(v) {
        is_known_simple_tag(&t)
    } else {
        false
    }
}

fn is_any_cond(v: &Value) -> bool {
    is_compound_cond(v) || is_simple_cond(v)
}

/// Walk the simples of `cond`. For a compound, yields each element after
/// slot 0. For a bare simple, yields itself once.
fn for_each_simple(cond: &Value, mut f: impl FnMut(&Value)) {
    if is_compound_cond(cond) {
        if let Value::Vector(vc) = cond {
            let v = vc.borrow();
            for slot in v.iter().skip(1) {
                f(slot);
            }
        }
    } else if is_simple_cond(cond) {
        f(cond);
    }
}

/// True if `cond` contains any simple whose type is `parent` or a
/// descendant of it. Walks the runtime registry, so user-defined condition
/// types registered via `define-condition-type` are matched alongside the
/// standard hierarchy.
fn cond_has_subtype(cond: &Value, parent: &str) -> bool {
    let mut found = false;
    for_each_simple(cond, |s| {
        if let Some(t) = vec_first_tag(s) {
            if is_descendant_of(&t, parent) {
                found = true;
            }
        }
    });
    found
}

fn find_simple_with_tag(cond: &Value, tag: &str) -> Option<Value> {
    let mut found: Option<Value> = None;
    for_each_simple(cond, |s| {
        if found.is_none() {
            if let Some(t) = vec_first_tag(s) {
                if t == tag {
                    found = Some(s.clone());
                }
            }
        }
    });
    found
}

/// Build a simple condition: `#("&<tag>" field0 field1 ...)`.
fn make_simple(tag: &str, fields: Vec<Value>) -> Value {
    let mut v = Vec::with_capacity(1 + fields.len());
    v.push(Value::string(tag));
    v.extend(fields);
    new_vector(v)
}

/// Wrap a list of simples in a compound condition vector. Always wraps —
/// even a single simple — so the data shape is uniform.
fn make_compound(simples: Vec<Value>) -> Value {
    let mut v = Vec::with_capacity(1 + simples.len());
    v.push(Value::string(COND_COMPOUND_TAG));
    v.extend(simples);
    new_vector(v)
}

fn new_vector(items: Vec<Value>) -> Value {
    Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(items)))
}

/// Append all simples of `cond` to `out`. Used by `condition` to flatten
/// a list of conditions into one compound.
fn flatten_simples(cond: &Value, out: &mut Vec<Value>) {
    if is_compound_cond(cond) {
        if let Value::Vector(vc) = cond {
            let v = vc.borrow();
            for slot in v.iter().skip(1) {
                out.push(slot.clone());
            }
        }
    } else if is_simple_cond(cond) {
        out.push(cond.clone());
    }
}

fn b_condition_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("condition?", "1", args.len()));
    }
    Ok(Value::Boolean(is_any_cond(&args[0])))
}

// ---- standard simple-condition constructors ----

fn b_make_message_condition(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-message-condition", "1", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_MESSAGE,
        vec![args[0].clone()],
    )]))
}

fn b_make_irritants_condition(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-irritants-condition", "1", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_IRRITANTS,
        vec![args[0].clone()],
    )]))
}

fn b_make_warning(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("make-warning", "0", args.len()));
    }
    Ok(make_compound(vec![make_simple(TAG_WARNING, vec![])]))
}

fn b_make_serious_condition(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("make-serious-condition", "0", args.len()));
    }
    Ok(make_compound(vec![make_simple(TAG_SERIOUS, vec![])]))
}

fn b_make_error(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("make-error", "0", args.len()));
    }
    Ok(make_compound(vec![make_simple(TAG_ERROR, vec![])]))
}

fn b_make_violation(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("make-violation", "0", args.len()));
    }
    Ok(make_compound(vec![make_simple(TAG_VIOLATION, vec![])]))
}

fn b_make_assertion_violation(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("make-assertion-violation", "0", args.len()));
    }
    Ok(make_compound(vec![make_simple(TAG_ASSERTION, vec![])]))
}

fn b_make_non_continuable_violation(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("make-non-continuable-violation", "0", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_NON_CONTINUABLE,
        vec![],
    )]))
}

fn b_make_who_condition(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-who-condition", "1", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_WHO,
        vec![args[0].clone()],
    )]))
}

// ---- standard predicates (descendants-inclusive) ----

fn b_message_condition_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("message-condition?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_MESSAGE)))
}

fn b_irritants_condition_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("irritants-condition?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_IRRITANTS)))
}

fn b_warning_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("warning?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_WARNING)))
}

fn b_serious_condition_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("serious-condition?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_SERIOUS)))
}

fn b_error_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("error?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_ERROR)))
}

fn b_violation_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("violation?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_VIOLATION)))
}

fn b_non_continuable_violation_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("non-continuable-violation?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(
        &args[0],
        TAG_NON_CONTINUABLE,
    )))
}

fn b_who_condition_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("who-condition?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_WHO)))
}

// ---- standard accessors ----

fn b_condition_message(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("condition-message", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_MESSAGE)
        .ok_or_else(|| "condition-message: not a message condition".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 2 {
            return Ok(v[1].clone());
        }
    }
    Err("condition-message: malformed".to_string())
}

fn b_condition_irritants(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("condition-irritants", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_IRRITANTS)
        .ok_or_else(|| "condition-irritants: not an irritants condition".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 2 {
            return Ok(v[1].clone());
        }
    }
    Err("condition-irritants: malformed".to_string())
}

fn b_condition_who(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("condition-who", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_WHO)
        .ok_or_else(|| "condition-who: not a who-condition".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 2 {
            return Ok(v[1].clone());
        }
    }
    Err("condition-who: malformed".to_string())
}

// ---- compound builders ----

fn b_condition(args: &[Value]) -> Result<Value, String> {
    let mut simples = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if !is_any_cond(a) {
            return Err(format!(
                "condition: arg {} is not a condition ({})",
                i + 1,
                a.type_name()
            ));
        }
        flatten_simples(a, &mut simples);
    }
    Ok(make_compound(simples))
}

fn b_simple_conditions(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("simple-conditions", "1", args.len()));
    }
    if !is_any_cond(&args[0]) {
        return Err(type_err("simple-conditions", "condition", &args[0]));
    }
    let mut out = Vec::new();
    flatten_simples(&args[0], &mut out);
    Ok(Value::list(out))
}

/// Render a condition into the same human-friendly string format used by
/// the top-level uncaught-condition path. Returns the rendered string —
/// callers print it via the appropriate port.
pub fn render_condition(c: &Value, syms: &SymbolTable) -> String {
    let simples: Vec<Value> = if is_compound_cond(c) {
        if let Value::Vector(vc) = c {
            vc.borrow().iter().skip(1).cloned().collect()
        } else {
            Vec::new()
        }
    } else if is_simple_cond(c) {
        vec![c.clone()]
    } else {
        return format!("non-condition: {}", c.format_with(syms, WriteMode::Write));
    };

    let mut msg: Option<String> = None;
    let mut irritants: Vec<Value> = Vec::new();
    let mut who: Option<Value> = None;
    let mut is_assertion = false;
    let mut other_tags: Vec<String> = Vec::new();
    for simple in &simples {
        if let Some(tag) = vec_first_tag(simple) {
            let fields: Vec<Value> = if let Value::Vector(vc) = simple {
                vc.borrow().iter().skip(1).cloned().collect()
            } else {
                Vec::new()
            };
            match tag.as_str() {
                "&message" => {
                    if let Some(Value::String(s)) = fields.first() {
                        msg = Some(s.borrow().clone());
                    }
                }
                "&irritants" => {
                    if let Some(list) = fields.first() {
                        let mut cur = list.clone();
                        loop {
                            match cur {
                                Value::Null => break,
                                Value::Pair(p) => {
                                    irritants.push(p.car.borrow().clone());
                                    cur = p.cdr.borrow().clone();
                                }
                                other => {
                                    irritants.push(other);
                                    break;
                                }
                            }
                        }
                    }
                }
                "&who" => {
                    who = fields.into_iter().next();
                }
                "&error" | "&serious" | "&violation" => {}
                "&assertion" => {
                    is_assertion = true;
                }
                other => other_tags.push(format!("[{}]", other)),
            }
        }
    }
    let prefix = if is_assertion {
        "assertion-violation"
    } else {
        "error"
    };
    let mut out = String::from(prefix);
    if let Some(w) = &who {
        if !matches!(w, Value::Boolean(false)) {
            out.push_str(" in ");
            out.push_str(&w.format_with(syms, WriteMode::Display));
        }
    }
    out.push(':');
    if let Some(m) = msg {
        out.push(' ');
        out.push_str(&m);
    }
    if !irritants.is_empty() {
        let irritant_strs: Vec<String> = irritants
            .iter()
            .map(|i| i.format_with(syms, WriteMode::Write))
            .collect();
        out.push_str(&format!(" ({})", irritant_strs.join(" ")));
    }
    if !other_tags.is_empty() {
        out.push(' ');
        out.push_str(&other_tags.join(" "));
    }
    out
}

/// `(display-condition <cond> [<port>])` — write a textual representation
/// of a condition to the given port (or the current output port when
/// omitted). Newline included so successive calls render cleanly.
fn b_display_condition(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("display-condition", "1 or 2", args.len()));
    }
    if !is_any_cond(&args[0]) {
        return Err(type_err("display-condition", "condition", &args[0]));
    }
    let mut s = render_condition(&args[0], ctx.syms);
    s.push('\n');
    write_output(&s, args.get(1).cloned(), ctx)
}

// ---- helpers for define-condition-type-generated code ----
//
// `define-condition-type` desugars to a `condition-register-parent!` call
// at runtime startup plus three lambda-bound bindings (constructor,
// predicate, accessors) that consume the next two helpers. Splitting the
// type-walking and field-fetching into builtin primitives keeps the
// generated code small and avoids re-implementing the registry walk in
// macro-expanded scheme.

fn b_condition_register_parent(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("condition-register-parent!", "2", args.len()));
    }
    let child = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("condition-register-parent!", "string", other)),
    };
    let parent = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("condition-register-parent!", "string", other)),
    };
    COND_PARENTS.with(|reg| reg.borrow_mut().insert(child.clone(), parent));
    COND_KNOWN_TAGS.with(|reg| reg.borrow_mut().insert(child));
    Ok(Value::Unspecified)
}

fn b_condition_instance_of(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("condition-instance-of?", "2", args.len()));
    }
    let tag = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("condition-instance-of?", "string", other)),
    };
    Ok(Value::Boolean(cond_has_subtype(&args[0], &tag)))
}

/// Find the first simple in `cond` whose tag is `child` or a descendant of
/// `child`, then return slot `field-index + 1` of that simple (slot 0 is
/// the tag). Used by accessors generated for `define-condition-type`.
fn b_condition_field_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("condition-field-ref", "3", args.len()));
    }
    let tag = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("condition-field-ref", "string", other)),
    };
    let idx = as_int_i64("condition-field-ref", &args[2])? as usize;
    let mut found: Option<Value> = None;
    for_each_simple(&args[0], |s| {
        if found.is_none() {
            if let Some(t) = vec_first_tag(s) {
                if is_descendant_of(&t, &tag) {
                    found = Some(s.clone());
                }
            }
        }
    });
    let simple =
        found.ok_or_else(|| format!("condition-field-ref: condition has no '{}' simple", tag))?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if let Some(slot) = v.get(idx + 1) {
            return Ok(slot.clone());
        }
    }
    Err(format!(
        "condition-field-ref: simple '{}' has no field {}",
        tag, idx
    ))
}

/// Build a simple condition value from a string tag and field values, then
/// wrap it in a one-slot compound. The expansion of `define-condition-type`
/// emits a call to this for each constructor.
fn b_make_simple_condition(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("make-simple-condition: needs at least a tag".into());
    }
    let tag = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("make-simple-condition", "string", other)),
    };
    let fields: Vec<Value> = args[1..].to_vec();
    Ok(make_compound(vec![make_simple(&tag, fields)]))
}

// ---- raise / error / with-exception-handler ----

fn b_raise(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("raise", "1", args.len()));
    }
    ctx.pending_raise = Some(args[0].clone());
    Err("__raised__".to_string())
}

fn b_error_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() {
        return Err("error: needs at least 1 argument".into());
    }
    // R6RS `(error who message irritant ...)` vs R7RS `(error message
    // irritant ...)`. We accept both: when args[0] is a symbol, `#f`, or a
    // string with at least one more arg AND args[1] is a string, treat
    // args[0] as who. Otherwise treat args[0] as the message.
    //
    // The string-vs-string disambiguation is the only ambiguous case; we
    // resolve it as "if there are ≥2 args and args[1] is also a string,
    // assume R6RS-style — args[0] is who". This matches what most R6RS
    // implementations do and keeps backward compat with single-string
    // R7RS calls like `(error "boom")`.
    let (who, msg_idx) = match &args[0] {
        Value::Symbol(_) | Value::Boolean(false) => (Some(args[0].clone()), 1),
        Value::String(_) if args.len() >= 2 && matches!(&args[1], Value::String(_)) => {
            (Some(args[0].clone()), 1)
        }
        _ => (None, 0),
    };
    let msg = if msg_idx < args.len() {
        match &args[msg_idx] {
            Value::String(s) => s.borrow().clone(),
            other => format!("{}", other),
        }
    } else {
        // R7RS allows `(error <who-symbol>)` with no message — fall back
        // to a generic placeholder.
        "error".to_string()
    };
    let irritants: Vec<Value> = if msg_idx + 1 <= args.len() {
        args[(msg_idx + 1).min(args.len())..].to_vec()
    } else {
        Vec::new()
    };
    let condition = make_error_condition(who, msg, irritants);
    ctx.pending_raise = Some(condition);
    Err("__raised__".to_string())
}

/// R6RS `(assertion-violation who message irritant ...)`. Always parses
/// `who` as the first arg (R6RS spec — and there's no ambiguity since the
/// caller is expected to pass a symbol/string/#f). Raises a compound
/// containing `&assertion`, `&who`, `&message`, and (if any) `&irritants`.
fn b_assertion_violation(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("assertion-violation: needs at least <who> and <message>".into());
    }
    let who = args[0].clone();
    let msg = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        other => format!("{}", other),
    };
    let irritants: Vec<Value> = args[2..].to_vec();
    let mut simples = vec![
        make_simple(TAG_ASSERTION, vec![]),
        make_simple(TAG_WHO, vec![who]),
        make_simple(TAG_MESSAGE, vec![Value::string(msg)]),
    ];
    if !irritants.is_empty() {
        simples.push(make_simple(TAG_IRRITANTS, vec![Value::list(irritants)]));
    }
    ctx.pending_raise = Some(make_compound(simples));
    Err("__raised__".to_string())
}

/// Helper used by `error` and the VM-tier error path. Builds a compound
/// condition with `&error`, optional `&who`, `&message`, and (if non-empty)
/// `&irritants`. Centralized so both tiers produce the same shape.
pub fn make_error_condition(who: Option<Value>, msg: String, irritants: Vec<Value>) -> Value {
    let mut simples = vec![make_simple(TAG_ERROR, vec![])];
    if let Some(w) = who {
        simples.push(make_simple(TAG_WHO, vec![w]));
    }
    simples.push(make_simple(TAG_MESSAGE, vec![Value::string(msg)]));
    if !irritants.is_empty() {
        simples.push(make_simple(TAG_IRRITANTS, vec![Value::list(irritants)]));
    }
    make_compound(simples)
}

fn b_with_exception_handler(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("with-exception-handler", "2", args.len()));
    }
    let handler = args[0].clone();
    let thunk = args[1].clone();
    let prev = ctx.pending_raise.take();
    let res = apply_procedure(&thunk, &[], ctx);
    match res {
        Ok(v) => {
            ctx.pending_raise = prev;
            Ok(v)
        }
        Err(e) => match e.kind {
            crate::eval::EvalErrorKind::Raised(cond) => {
                ctx.pending_raise = prev;
                // If the handler itself raises, repropagate as a raise so an
                // outer with-exception-handler can catch it.
                match apply_procedure(&handler, &[cond], ctx) {
                    Ok(v) => Ok(v),
                    Err(e2) => match e2.kind {
                        crate::eval::EvalErrorKind::Raised(c2) => {
                            ctx.pending_raise = Some(c2);
                            Err("__raised__".to_string())
                        }
                        crate::eval::EvalErrorKind::Escape(eid, v) => {
                            ctx.pending_escape = Some((eid, v));
                            Err("__escape__".to_string())
                        }
                        crate::eval::EvalErrorKind::Message(m) => Err(m),
                    },
                }
            }
            crate::eval::EvalErrorKind::Escape(eid, v) => {
                ctx.pending_raise = prev;
                ctx.pending_escape = Some((eid, v));
                Err("__escape__".to_string())
            }
            crate::eval::EvalErrorKind::Message(m) => {
                ctx.pending_raise = prev;
                Err(m)
            }
        },
    }
}

// ---- numeric extensions ----

fn b_gcd(args: &[Value]) -> Result<Value, String> {
    let mut acc: i64 = 0;
    for a in args {
        let n = as_int_i64("gcd", a)?.abs();
        acc = num_gcd(acc, n);
    }
    Ok(Value::fixnum(acc))
}

fn b_lcm(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::fixnum(1));
    }
    let mut acc: i64 = 1;
    for a in args {
        let n = as_int_i64("lcm", a)?.abs();
        if n == 0 {
            return Ok(Value::fixnum(0));
        }
        let g = num_gcd(acc, n);
        acc = (acc / g).saturating_mul(n);
    }
    Ok(Value::fixnum(acc))
}

fn num_gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a.abs(), b.abs());
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

fn b_floor(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("floor", "1", args.len()));
    }
    match as_num("floor", &args[0])? {
        Number::Fixnum(_) | Number::Big(_) => Ok(args[0].clone()),
        Number::Flonum(f) => Ok(Value::flonum(f.floor())),
        Number::Rat(_) => {
            let f = as_num("floor", &args[0])?.to_f64().floor();
            // exact->inexact->exact for now; full exact handling lands later.
            Ok(Value::flonum(f))
        }
    }
}

fn b_ceiling(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("ceiling", "1", args.len()));
    }
    match as_num("ceiling", &args[0])? {
        Number::Fixnum(_) | Number::Big(_) => Ok(args[0].clone()),
        Number::Flonum(f) => Ok(Value::flonum(f.ceil())),
        Number::Rat(_) => {
            let f = as_num("ceiling", &args[0])?.to_f64().ceil();
            Ok(Value::flonum(f))
        }
    }
}

fn b_truncate(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("truncate", "1", args.len()));
    }
    match as_num("truncate", &args[0])? {
        Number::Fixnum(_) | Number::Big(_) => Ok(args[0].clone()),
        Number::Flonum(f) => Ok(Value::flonum(f.trunc())),
        Number::Rat(_) => {
            let f = as_num("truncate", &args[0])?.to_f64().trunc();
            Ok(Value::flonum(f))
        }
    }
}

fn b_round(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("round", "1", args.len()));
    }
    match as_num("round", &args[0])? {
        Number::Fixnum(_) | Number::Big(_) => Ok(args[0].clone()),
        Number::Flonum(f) => {
            // R6RS round-half-to-even (banker's rounding) for flonums.
            let r = f.round();
            // f64::round rounds away from zero, but R6RS wants round-half-to-even.
            // Apply correction when fractional part is exactly 0.5.
            let r = if (f - f.floor() - 0.5).abs() < f64::EPSILON {
                let floor = f.floor();
                if (floor as i64) % 2 == 0 {
                    floor
                } else {
                    floor + 1.0
                }
            } else {
                r
            };
            Ok(Value::flonum(r))
        }
        Number::Rat(_) => Ok(Value::flonum(as_num("round", &args[0])?.to_f64().round())),
    }
}

fn b_even_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("even?", "1", args.len()));
    }
    let n = as_int_i64("even?", &args[0])?;
    Ok(Value::Boolean(n % 2 == 0))
}

fn b_odd_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("odd?", "1", args.len()));
    }
    let n = as_int_i64("odd?", &args[0])?;
    Ok(Value::Boolean(n % 2 != 0))
}

fn b_square(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("square", "1", args.len()));
    }
    let n = as_num("square", &args[0])?;
    Ok(Value::Number(n.mul(&n)))
}

// ---- character extensions ----

fn b_char_upcase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char-upcase", "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => {
            let up = c.to_uppercase().next().unwrap_or(*c);
            Ok(Value::Character(up))
        }
        v => Err(type_err("char-upcase", "character", v)),
    }
}

fn b_char_downcase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char-downcase", "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => {
            let down = c.to_lowercase().next().unwrap_or(*c);
            Ok(Value::Character(down))
        }
        v => Err(type_err("char-downcase", "character", v)),
    }
}

fn char_pred(name: &str, args: &[Value], pred: fn(char) -> bool) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => Ok(Value::Boolean(pred(*c))),
        v => Err(type_err(name, "character", v)),
    }
}

fn b_char_alphabetic(args: &[Value]) -> Result<Value, String> {
    char_pred("char-alphabetic?", args, |c| c.is_alphabetic())
}

fn b_char_numeric(args: &[Value]) -> Result<Value, String> {
    char_pred("char-numeric?", args, |c| c.is_numeric())
}

fn b_char_whitespace(args: &[Value]) -> Result<Value, String> {
    char_pred("char-whitespace?", args, |c| c.is_whitespace())
}

fn b_char_upper_case(args: &[Value]) -> Result<Value, String> {
    char_pred("char-upper-case?", args, |c| c.is_uppercase())
}

fn b_char_lower_case(args: &[Value]) -> Result<Value, String> {
    char_pred("char-lower-case?", args, |c| c.is_lowercase())
}

// ---- eof ----

fn b_eof_object_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("eof-object?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Eof)))
}

fn b_eof_object(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("eof-object", "0", args.len()));
    }
    Ok(Value::Eof)
}

// ---- symbol <-> string (higher-order, need SymbolTable) ----

fn b_symbol_to_string_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("symbol->string", "1", args.len()));
    }
    match &args[0] {
        Value::Symbol(s) => Ok(Value::string(ctx.syms.name(*s).to_string())),
        v => Err(type_err("symbol->string", "symbol", v)),
    }
}

fn b_string_to_symbol_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string->symbol", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => {
            let sym = ctx.syms.intern(&s.borrow());
            Ok(Value::Symbol(sym))
        }
        v => Err(type_err("string->symbol", "string", v)),
    }
}

// ---- hashtables ----

fn b_make_eq_hashtable(_args: &[Value]) -> Result<Value, String> {
    Ok(Value::Hashtable(Hashtable::new(HtEqKind::Eq)))
}

fn b_make_eqv_hashtable(_args: &[Value]) -> Result<Value, String> {
    Ok(Value::Hashtable(Hashtable::new(HtEqKind::Eqv)))
}

fn b_make_hashtable(args: &[Value]) -> Result<Value, String> {
    // R6RS make-hashtable takes (hash-fn equiv-fn). We don't support custom
    // hash/equiv yet; treat as `equal?`-based.
    if args.len() > 2 {
        return Err(arity_err("make-hashtable", "0..2", args.len()));
    }
    Ok(Value::Hashtable(Hashtable::new(HtEqKind::Equal)))
}

// ---- R6RS standard hash functions ----
//
// These each return a small integer derived from their argument. The
// hash quality only needs to be good enough for hashtable bucket
// distribution — collisions are resolved via the equiv? function.
// Programs that pass them to `(make-hashtable hash equiv)` resolve;
// our `make-hashtable` itself ignores the user-provided functions and
// uses the built-in equal? table for now.

fn fnv1a_hash(bytes: &[u8]) -> i64 {
    // 64-bit FNV-1a — small, good enough for general hashing, no deps.
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // Truncate to a positive fixnum-friendly range.
    (h as i64).wrapping_abs()
}

fn b_string_hash(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-hash", "1", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("string-hash", "string", other)),
    };
    Ok(Value::fixnum(fnv1a_hash(s.as_bytes())))
}

fn b_symbol_hash_pure(args: &[Value]) -> Result<Value, String> {
    // Symbols compare by id internally; hashing the id is uniform.
    if args.len() != 1 {
        return Err(arity_err("symbol-hash", "1", args.len()));
    }
    match &args[0] {
        Value::Symbol(s) => {
            // The Symbol struct wraps a u32; hash that as 4 bytes.
            let id = s.0 as u32;
            Ok(Value::fixnum(fnv1a_hash(&id.to_le_bytes())))
        }
        other => Err(type_err("symbol-hash", "symbol", other)),
    }
}

/// Recursive hash for `equal?` semantics: walks pairs/vectors, includes
/// strings/symbols/numbers/booleans/chars. Cycles are not handled —
/// hashing a cyclic structure overflows the call stack, same constraint
/// as `equal?` itself.
fn equal_hash_rec(v: &Value, acc: &mut u64) {
    let mix = |h: &mut u64, x: u64| {
        *h ^= x;
        *h = h.wrapping_mul(0x100000001b3);
    };
    match v {
        Value::Null => mix(acc, 0x01),
        Value::Boolean(b) => mix(acc, 0x10 | (*b as u64)),
        Value::Number(n) => mix(acc, fnv1a_hash(format!("{}", n).as_bytes()) as u64),
        Value::Character(c) => mix(acc, 0x20 | (*c as u64)),
        Value::String(s) => {
            mix(acc, 0x30);
            for b in s.borrow().as_bytes() {
                mix(acc, *b as u64);
            }
        }
        Value::Symbol(s) => {
            mix(acc, 0x40);
            mix(acc, s.0 as u64);
        }
        Value::Pair(p) => {
            mix(acc, 0x50);
            equal_hash_rec(&p.car.borrow(), acc);
            equal_hash_rec(&p.cdr.borrow(), acc);
        }
        Value::Vector(vc) => {
            mix(acc, 0x60);
            for slot in vc.borrow().iter() {
                equal_hash_rec(slot, acc);
            }
        }
        _ => mix(acc, 0xff),
    }
}

fn b_equal_hash(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("equal-hash", "1", args.len()));
    }
    let mut h: u64 = 0xcbf29ce484222325;
    equal_hash_rec(&args[0], &mut h);
    Ok(Value::fixnum((h as i64).wrapping_abs()))
}

fn b_hashtable_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Hashtable(_))))
}

fn ht_eq(kind: HtEqKind, a: &Value, b: &Value) -> bool {
    match kind {
        HtEqKind::Eq => eq::eq(a, b),
        HtEqKind::Eqv => eq::eqv(a, b),
        HtEqKind::Equal => eq::equal(a, b),
    }
}

fn as_ht<'a>(name: &str, v: &'a Value) -> Result<&'a std::rc::Rc<Hashtable>, String> {
    match v {
        Value::Hashtable(h) => Ok(h),
        other => Err(type_err(name, "hashtable", other)),
    }
}

fn b_hashtable_size(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-size", "1", args.len()));
    }
    let h = as_ht("hashtable-size", &args[0])?;
    Ok(Value::fixnum(h.items.borrow().len() as i64))
}

fn b_hashtable_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("hashtable-set!", "3", args.len()));
    }
    let h = as_ht("hashtable-set!", &args[0])?;
    let kind = h.eq_kind;
    let mut items = h.items.borrow_mut();
    if let Some(slot) = items.iter_mut().find(|(k, _)| ht_eq(kind, k, &args[1])) {
        slot.1 = args[2].clone();
    } else {
        items.push((args[1].clone(), args[2].clone()));
    }
    Ok(Value::Unspecified)
}

fn b_hashtable_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("hashtable-ref", "3", args.len()));
    }
    let h = as_ht("hashtable-ref", &args[0])?;
    let kind = h.eq_kind;
    let items = h.items.borrow();
    if let Some((_, v)) = items.iter().find(|(k, _)| ht_eq(kind, k, &args[1])) {
        Ok(v.clone())
    } else {
        Ok(args[2].clone())
    }
}

fn b_hashtable_contains(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("hashtable-contains?", "2", args.len()));
    }
    let h = as_ht("hashtable-contains?", &args[0])?;
    let kind = h.eq_kind;
    let items = h.items.borrow();
    Ok(Value::Boolean(
        items.iter().any(|(k, _)| ht_eq(kind, k, &args[1])),
    ))
}

fn b_hashtable_delete(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("hashtable-delete!", "2", args.len()));
    }
    let h = as_ht("hashtable-delete!", &args[0])?;
    let kind = h.eq_kind;
    let mut items = h.items.borrow_mut();
    if let Some(idx) = items.iter().position(|(k, _)| ht_eq(kind, k, &args[1])) {
        items.swap_remove(idx);
    }
    Ok(Value::Unspecified)
}

fn b_hashtable_keys(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-keys", "1", args.len()));
    }
    let h = as_ht("hashtable-keys", &args[0])?;
    let items = h.items.borrow();
    let v: Vec<Value> = items.iter().map(|(k, _)| k.clone()).collect();
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(v))))
}

fn b_hashtable_values(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-values", "1", args.len()));
    }
    let h = as_ht("hashtable-values", &args[0])?;
    let items = h.items.borrow();
    let v: Vec<Value> = items.iter().map(|(_, v)| v.clone()).collect();
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(v))))
}

fn b_hashtable_clear(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("hashtable-clear!", "1 or 2", args.len()));
    }
    let h = as_ht("hashtable-clear!", &args[0])?;
    h.items.borrow_mut().clear();
    Ok(Value::Unspecified)
}

/// `(hashtable-update! ht key proc default)` — replaces ht[key] with
/// (proc (hashtable-ref ht key default)).
fn b_hashtable_update_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("hashtable-update!", "4", args.len()));
    }
    let h = as_ht("hashtable-update!", &args[0])?;
    let kind = h.eq_kind;
    let current = {
        let items = h.items.borrow();
        items
            .iter()
            .find(|(k, _)| ht_eq(kind, k, &args[1]))
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| args[3].clone())
    };
    let new_val = apply_procedure(&args[2], &[current], ctx).map_err(|e| e.message())?;
    let mut items = h.items.borrow_mut();
    if let Some(slot) = items.iter_mut().find(|(k, _)| ht_eq(kind, k, &args[1])) {
        slot.1 = new_val;
    } else {
        items.push((args[1].clone(), new_val));
    }
    Ok(Value::Unspecified)
}

/// `(hashtable-walk ht proc)` — calls (proc key value) for each entry.
fn b_hashtable_walk(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("hashtable-walk", "2", args.len()));
    }
    let h = as_ht("hashtable-walk", &args[0])?;
    let proc_val = args[1].clone();
    let entries: Vec<(Value, Value)> = h.items.borrow().clone();
    for (k, v) in entries {
        apply_procedure(&proc_val, &[k, v], ctx).map_err(|e| e.message())?;
    }
    Ok(Value::Unspecified)
}

/// `(values v1 v2 ...)` — multi-value return via side channel.
/// With 0 args, returns the unspecified value (after stashing zero values).
/// With 1 arg, returns that arg directly (no multi-value semantics needed).
/// With 2+ args, stashes them in ctx and returns Unspecified.
fn b_values(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() == 1 {
        return Ok(args[0].clone());
    }
    ctx.pending_values = Some(args.to_vec());
    Ok(Value::Unspecified)
}

// Monotonic counter for call/cc continuation ids. Each call/cc invocation
// gets a unique id so unwinding only catches its own escape and rethrows
// others.
static CONTINUATION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// `(call/cc proc)` / `(call-with-current-continuation proc)` — escape-only
/// continuation. Allocates a fresh id, builds a Continuation procedure,
/// calls proc with it. If proc returns normally, that's the result.
/// If proc (or anything it calls) invokes the continuation with `v`,
/// EvalError::Escape unwinds the stack to here and `v` is the result.
fn b_call_cc(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("call/cc", "1", args.len()));
    }
    let id = CONTINUATION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let k = crate::proc::make_continuation(id);
    let prev_escape = ctx.pending_escape.take();
    match apply_procedure(&args[0], &[k], ctx) {
        Ok(v) => {
            ctx.pending_escape = prev_escape;
            Ok(v)
        }
        Err(e) => {
            // Continuation invocations can arrive via two paths:
            // (a) directly as EvalErrorKind::Escape (when invoked inside
            //     simple eval of an App)
            // (b) via the pending_escape side-channel + a Message error
            //     (when the invocation was buried inside a higher builtin
            //      whose `.map_err(|e| e.message())` collapsed the kind)
            // Check both.
            let escape = match &e.kind {
                crate::eval::EvalErrorKind::Escape(eid, v) => Some((*eid, v.clone())),
                _ => ctx.pending_escape.take(),
            };
            if let Some((eid, v)) = escape {
                if eid == id {
                    ctx.pending_escape = prev_escape;
                    return Ok(v);
                }
                ctx.pending_escape = Some((eid, v));
                return Err("__escape__".to_string());
            }
            ctx.pending_escape = prev_escape;
            match e.kind {
                crate::eval::EvalErrorKind::Raised(cond) => {
                    ctx.pending_raise = Some(cond);
                    Err("__raised__".to_string())
                }
                crate::eval::EvalErrorKind::Message(m) => Err(m),
                crate::eval::EvalErrorKind::Escape(_, _) => unreachable!(),
            }
        }
    }
}

/// `(call-with-values producer consumer)` — calls producer with no args,
/// then applies consumer to the values it returned (single or multi).
fn b_call_with_values(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("call-with-values", "2", args.len()));
    }
    let producer = args[0].clone();
    let consumer = args[1].clone();
    let prev = ctx.pending_values.take();
    let result = apply_procedure(&producer, &[], ctx).map_err(|e| e.message())?;
    let values = if let Some(vs) = ctx.pending_values.take() {
        vs
    } else {
        vec![result]
    };
    ctx.pending_values = prev;
    apply_procedure(&consumer, &values, ctx).map_err(|e| e.message())
}

// ---- SRFI-1 / R6RS list-extras (pure) ----

fn b_iota(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("iota", "1..3", args.len()));
    }
    let count = as_int_i64("iota", &args[0])?;
    if count < 0 {
        return Err("iota: negative count".into());
    }
    let start = if args.len() >= 2 {
        as_int_i64("iota", &args[1])?
    } else {
        0
    };
    let step = if args.len() == 3 {
        as_int_i64("iota", &args[2])?
    } else {
        1
    };
    let mut items = Vec::with_capacity(count as usize);
    for i in 0..count {
        items.push(Value::fixnum(start + i * step));
    }
    Ok(Value::list(items))
}

fn b_last(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("last", "1", args.len()));
    }
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Pair(p) => {
                let cdr = p.cdr.borrow().clone();
                if matches!(cdr, Value::Null) {
                    return Ok(p.car.borrow().clone());
                }
                cur = cdr;
            }
            Value::Null => return Err("last: empty list".into()),
            v => return Err(type_err("last", "proper list", &v)),
        }
    }
}

fn b_last_pair(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("last-pair", "1", args.len()));
    }
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Pair(p) => {
                let cdr = p.cdr.borrow().clone();
                if !matches!(cdr, Value::Pair(_)) {
                    return Ok(Value::Pair(p));
                }
                cur = cdr;
            }
            v => return Err(type_err("last-pair", "non-empty list", &v)),
        }
    }
}

fn b_take(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("take", "2", args.len()));
    }
    let n = as_int_i64("take", &args[1])?;
    if n < 0 {
        return Err("take: negative count".into());
    }
    let mut taken = Vec::with_capacity(n as usize);
    let mut cur = args[0].clone();
    let mut i = 0i64;
    while i < n {
        match cur {
            Value::Pair(p) => {
                taken.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
                i += 1;
            }
            _ => return Err("take: list shorter than n".into()),
        }
    }
    Ok(Value::list(taken))
}

fn b_drop(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("drop", "2", args.len()));
    }
    let n = as_int_i64("drop", &args[1])?;
    if n < 0 {
        return Err("drop: negative count".into());
    }
    let mut cur = args[0].clone();
    let mut i = 0i64;
    while i < n {
        match cur {
            Value::Pair(p) => {
                cur = p.cdr.borrow().clone();
                i += 1;
            }
            _ => return Err("drop: list shorter than n".into()),
        }
    }
    Ok(cur)
}

fn b_zip(args: &[Value]) -> Result<Value, String> {
    let lists: Vec<Vec<Value>> = args
        .iter()
        .map(|v| collect_proper_list("zip", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        out.push(Value::list(row));
    }
    Ok(Value::list(out))
}

// ---- SRFI-1 / R6RS list-extras (higher-order) ----

fn b_filter(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("filter", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("filter", &args[1])?;
    let mut out = Vec::new();
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

/// `(take-while pred lst)` — return the longest prefix of `lst` whose
/// elements all satisfy `pred`.
fn b_take_while(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("take-while", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("take-while", &args[1])?;
    let mut out = Vec::new();
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if !r.is_truthy() {
            break;
        }
        out.push(item);
    }
    Ok(Value::list(out))
}

/// `(drop-while pred lst)` — drop the longest prefix satisfying `pred`,
/// return the rest.
fn b_drop_while(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("drop-while", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("drop-while", &args[1])?;
    let mut idx = 0;
    while idx < items.len() {
        let r = apply_procedure(&pred, &[items[idx].clone()], ctx).map_err(|e| e.message())?;
        if !r.is_truthy() {
            break;
        }
        idx += 1;
    }
    Ok(Value::list(items[idx..].to_vec()))
}

/// `(span pred lst)` — split `lst` at the first failing predicate
/// position. Returns `(values prefix rest)` where prefix satisfies pred.
fn b_span(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("span", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("span", &args[1])?;
    let mut idx = 0;
    while idx < items.len() {
        let r = apply_procedure(&pred, &[items[idx].clone()], ctx).map_err(|e| e.message())?;
        if !r.is_truthy() {
            break;
        }
        idx += 1;
    }
    let prefix = items[..idx].to_vec();
    let rest = items[idx..].to_vec();
    ctx.pending_values = Some(vec![Value::list(prefix), Value::list(rest)]);
    Ok(Value::Unspecified)
}

/// `(break pred lst)` — span with the negated predicate. Splits at the
/// first element that satisfies pred.
fn b_break(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("break", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("break", &args[1])?;
    let mut idx = 0;
    while idx < items.len() {
        let r = apply_procedure(&pred, &[items[idx].clone()], ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            break;
        }
        idx += 1;
    }
    let prefix = items[..idx].to_vec();
    let rest = items[idx..].to_vec();
    ctx.pending_values = Some(vec![Value::list(prefix), Value::list(rest)]);
    Ok(Value::Unspecified)
}

/// `(list-index pred lst1 lst2 ...)` — return the index of the first
/// element-tuple where `(pred elt1 elt2 ...)` is truthy, or #f.
fn b_list_index(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("list-index", "at least 2", args.len()));
    }
    let pred = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("list-index", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        let r = apply_procedure(&pred, &row, ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            return Ok(Value::fixnum(i as i64));
        }
    }
    Ok(Value::Boolean(false))
}

/// `(filter-map proc lst1 lst2 ...)` — like map, but #f results are
/// dropped from the output. Idiomatic shape for "transform if matches".
fn b_filter_map(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("filter-map", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("filter-map", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        let r = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
        if !matches!(r, Value::Boolean(false)) {
            out.push(r);
        }
    }
    Ok(Value::list(out))
}

/// `(append-map proc lst1 lst2 ...)` — like map but appends each
/// list-result. Equivalent to `(apply append (map proc lst1 ...))` but
/// avoids the intermediate list.
fn b_append_map(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("append-map", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("append-map", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        let r = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
        let inner = collect_proper_list("append-map", &r)?;
        out.extend(inner);
    }
    Ok(Value::list(out))
}

/// `(list-tabulate n proc)` — build the list `((proc 0) (proc 1) ... (proc (n-1)))`.
fn b_list_tabulate(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("list-tabulate", "2", args.len()));
    }
    let n = as_int_i64("list-tabulate", &args[0])?;
    if n < 0 {
        return Err("list-tabulate: negative count".into());
    }
    let proc_val = args[1].clone();
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let r = apply_procedure(&proc_val, &[Value::fixnum(i)], ctx).map_err(|e| e.message())?;
        out.push(r);
    }
    Ok(Value::list(out))
}

fn b_fold_left(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // (fold-left proc init list1 list2 ...)
    if args.len() < 3 {
        return Err(arity_err("fold-left", "at least 3", args.len()));
    }
    let proc_val = args[0].clone();
    let mut acc = args[1].clone();
    let lists: Vec<Vec<Value>> = args[2..]
        .iter()
        .map(|v| collect_proper_list("fold-left", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    for i in 0..n {
        let mut row: Vec<Value> = vec![acc];
        for l in &lists {
            row.push(l[i].clone());
        }
        acc = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

fn b_fold_right(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 3 {
        return Err(arity_err("fold-right", "at least 3", args.len()));
    }
    let proc_val = args[0].clone();
    let init = args[1].clone();
    let lists: Vec<Vec<Value>> = args[2..]
        .iter()
        .map(|v| collect_proper_list("fold-right", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    let mut acc = init;
    for i in (0..n).rev() {
        let mut row: Vec<Value> = Vec::with_capacity(lists.len() + 1);
        for l in &lists {
            row.push(l[i].clone());
        }
        row.push(acc);
        acc = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

fn b_reduce(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // (reduce proc default list)
    if args.len() != 3 {
        return Err(arity_err("reduce", "3", args.len()));
    }
    let proc_val = args[0].clone();
    let default = args[1].clone();
    let items = collect_proper_list("reduce", &args[2])?;
    if items.is_empty() {
        return Ok(default);
    }
    let mut acc = items[0].clone();
    for item in &items[1..] {
        acc = apply_procedure(&proc_val, &[acc, item.clone()], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

fn b_find(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("find", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("find", &args[1])?;
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            return Ok(item);
        }
    }
    Ok(Value::Boolean(false))
}

fn b_count(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("count", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("count", &args[1])?;
    let mut n: i64 = 0;
    for item in items {
        let r = apply_procedure(&pred, &[item], ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            n += 1;
        }
    }
    Ok(Value::fixnum(n))
}

fn b_any(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("any", "at least 2", args.len()));
    }
    let pred = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("any", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        let r = apply_procedure(&pred, &row, ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            return Ok(r);
        }
    }
    Ok(Value::Boolean(false))
}

fn b_every(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("every", "at least 2", args.len()));
    }
    let pred = args[0].clone();
    let lists: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| collect_proper_list("every", v))
        .collect::<Result<_, _>>()?;
    let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
    if n == 0 {
        return Ok(Value::Boolean(true));
    }
    let mut last_truthy = Value::Boolean(true);
    for i in 0..n {
        let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
        let r = apply_procedure(&pred, &row, ctx).map_err(|e| e.message())?;
        if !r.is_truthy() {
            return Ok(Value::Boolean(false));
        }
        last_truthy = r;
    }
    Ok(last_truthy)
}

fn b_partition(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // (partition pred list) — returns two lists (matching, non-matching) via values
    if args.len() != 2 {
        return Err(arity_err("partition", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("partition", &args[1])?;
    let mut yes = Vec::new();
    let mut no = Vec::new();
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            yes.push(item);
        } else {
            no.push(item);
        }
    }
    // Return as multiple values
    ctx.pending_values = Some(vec![Value::list(yes), Value::list(no)]);
    Ok(Value::Unspecified)
}

// ---- ports ----

fn b_open_string_input_port(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("open-string-input-port", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::Port(Port::string_input(&s.borrow()))),
        v => Err(type_err("open-string-input-port", "string", v)),
    }
}

fn b_open_string_output_port(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("open-string-output-port", "0", args.len()));
    }
    Ok(Value::Port(Port::string_output()))
}

// ---- bytevector-backed ports (R6RS binary I/O) ----

fn b_open_bytevector_input_port(args: &[Value]) -> Result<Value, String> {
    // R6RS allows an optional transcoder; we don't have transcoders yet.
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err(
            "open-bytevector-input-port",
            "1 or 2",
            args.len(),
        ));
    }
    let bytes = match &args[0] {
        Value::ByteVector(b) => b.borrow().clone(),
        v => return Err(type_err("open-bytevector-input-port", "bytevector", v)),
    };
    Ok(Value::Port(Port::bytevector_input(bytes)))
}

fn b_open_bytevector_output_port(args: &[Value]) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err(
            "open-bytevector-output-port",
            "0 or 1",
            args.len(),
        ));
    }
    // Optional transcoder argument is ignored at the foundation milestone.
    Ok(Value::Port(Port::bytevector_output()))
}

fn b_get_bytevector_output_port(args: &[Value]) -> Result<Value, String> {
    // R6RS shape: `(get-bytevector-output-port port)` returns the
    // accumulated bytevector AND clears the buffer (the port stays open
    // and continues to be writable, starting fresh).
    if args.len() != 1 {
        return Err(arity_err("get-bytevector-output-port", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorOutput(buf) => {
                let bytes = buf.borrow().clone();
                buf.borrow_mut().clear();
                Ok(Value::ByteVector(std::rc::Rc::new(
                    std::cell::RefCell::new(bytes),
                )))
            }
            _ => Err("get-bytevector-output-port: not a bytevector output port".into()),
        },
        v => Err(type_err("get-bytevector-output-port", "output-port", v)),
    }
}

/// `(get-u8 port)` — read one byte from a binary input port. Returns the
/// EOF object at end of stream.
fn b_get_u8(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("get-u8", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos < s.bytes.len() {
                    let b = s.bytes[s.pos];
                    s.pos += 1;
                    Ok(Value::fixnum(b as i64))
                } else {
                    Ok(Value::Eof)
                }
            }
            _ => Err("get-u8: not a binary input port".into()),
        },
        v => Err(type_err("get-u8", "binary-input-port", v)),
    }
}

/// `(lookahead-u8 port)` — peek one byte without consuming it.
fn b_lookahead_u8(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("lookahead-u8", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let s = state.borrow();
                if s.pos < s.bytes.len() {
                    Ok(Value::fixnum(s.bytes[s.pos] as i64))
                } else {
                    Ok(Value::Eof)
                }
            }
            _ => Err("lookahead-u8: not a binary input port".into()),
        },
        v => Err(type_err("lookahead-u8", "binary-input-port", v)),
    }
}

/// `(put-u8 port byte)` — append one byte to a binary output port.
fn b_put_u8(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("put-u8", "2", args.len()));
    }
    let byte = as_int_i64("put-u8", &args[1])?;
    if !(0..=255).contains(&byte) {
        return Err("put-u8: byte out of u8 range".into());
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorOutput(buf) => {
                buf.borrow_mut().push(byte as u8);
                Ok(Value::Unspecified)
            }
            _ => Err("put-u8: not a binary output port".into()),
        },
        v => Err(type_err("put-u8", "binary-output-port", v)),
    }
}

/// `(get-bytevector-n port count)` — read up to `count` bytes into a
/// fresh bytevector. Returns the EOF object when no bytes can be read
/// (i.e., already at end of stream); otherwise returns whatever was
/// available, which may be shorter than `count`.
fn b_get_bytevector_n(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("get-bytevector-n", "2", args.len()));
    }
    let n = as_int_i64("get-bytevector-n", &args[1])?;
    if n < 0 {
        return Err("get-bytevector-n: negative count".into());
    }
    let n = n as usize;
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.bytes.len() {
                    return Ok(Value::Eof);
                }
                let avail = s.bytes.len() - s.pos;
                let take = n.min(avail);
                let bytes = s.bytes[s.pos..s.pos + take].to_vec();
                s.pos += take;
                Ok(Value::ByteVector(std::rc::Rc::new(
                    std::cell::RefCell::new(bytes),
                )))
            }
            _ => Err("get-bytevector-n: not a binary input port".into()),
        },
        v => Err(type_err("get-bytevector-n", "binary-input-port", v)),
    }
}

fn b_binary_port_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("binary-port?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        &args[0],
        Value::Port(p) if p.is_binary()
    )))
}

fn b_textual_port_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("textual-port?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        &args[0],
        Value::Port(p) if p.is_textual()
    )))
}

fn b_get_output_string(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("get-output-string", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringOutput(buf) => {
                let s = buf.borrow().clone();
                buf.borrow_mut().clear();
                Ok(Value::string(s))
            }
            _ => Err("get-output-string: not a string output port".into()),
        },
        v => Err(type_err("get-output-string", "output-port", v)),
    }
}

fn b_read_char(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("read-char", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos < s.chars.len() {
                    let c = s.chars[s.pos];
                    s.pos += 1;
                    Ok(Value::Character(c))
                } else {
                    Ok(Value::Eof)
                }
            }
            _ => Err("read-char: not an input port".into()),
        },
        v => Err(type_err("read-char", "input-port", v)),
    }
}

fn b_peek_char(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("peek-char", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let s = state.borrow();
                if s.pos < s.chars.len() {
                    Ok(Value::Character(s.chars[s.pos]))
                } else {
                    Ok(Value::Eof)
                }
            }
            _ => Err("peek-char: not an input port".into()),
        },
        v => Err(type_err("peek-char", "input-port", v)),
    }
}

fn b_get_line(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("get-line", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.chars.len() {
                    return Ok(Value::Eof);
                }
                let mut line = String::new();
                while s.pos < s.chars.len() {
                    let c = s.chars[s.pos];
                    s.pos += 1;
                    if c == '\n' {
                        break;
                    }
                    line.push(c);
                }
                Ok(Value::string(line))
            }
            _ => Err("get-line: not an input port".into()),
        },
        v => Err(type_err("get-line", "input-port", v)),
    }
}

fn b_port_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("port?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Port(_))))
}

fn b_input_port_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("input-port?", "1", args.len()));
    }
    Ok(Value::Boolean(match &args[0] {
        Value::Port(p) => p.is_input(),
        _ => false,
    }))
}

fn b_output_port_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("output-port?", "1", args.len()));
    }
    Ok(Value::Boolean(match &args[0] {
        Value::Port(p) => p.is_output(),
        _ => false,
    }))
}

fn b_write_char(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("write-char", "2", args.len()));
    }
    let c = match &args[0] {
        Value::Character(c) => *c,
        v => return Err(type_err("write-char", "character", v)),
    };
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::StringOutput(buf) => {
                buf.borrow_mut().push(c);
                Ok(Value::Unspecified)
            }
            _ => Err("write-char: not an output port".into()),
        },
        v => Err(type_err("write-char", "output-port", v)),
    }
}

fn b_write_string(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("write-string", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("write-string", "string", v)),
    };
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::StringOutput(buf) => {
                buf.borrow_mut().push_str(&s);
                Ok(Value::Unspecified)
            }
            _ => Err("write-string: not an output port".into()),
        },
        v => Err(type_err("write-string", "output-port", v)),
    }
}

// ---- promises ----

fn b_promise_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("promise?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Promise(_))))
}

fn b_make_promise(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-promise", "1", args.len()));
    }
    Ok(Value::Promise(Promise::pending(args[0].clone())))
}

/// `(dynamic-wind before thunk after)` — runs `before` thunk, then `thunk`,
/// then `after` thunk. If `thunk` raises, `after` runs before the raise
/// propagates. Foundation simplification: doesn't yet handle non-local
/// re-entry through continuations (because we don't have first-class
/// continuations), only the unwind direction.
fn b_dynamic_wind(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("dynamic-wind", "3", args.len()));
    }
    let before = args[0].clone();
    let thunk = args[1].clone();
    let after = args[2].clone();
    apply_procedure(&before, &[], ctx).map_err(|e| e.message())?;
    let result = apply_procedure(&thunk, &[], ctx);
    // Always run after, even on error.
    let after_err = apply_procedure(&after, &[], ctx).err().map(|e| e.message());
    match result {
        Ok(v) => {
            if let Some(msg) = after_err {
                return Err(msg);
            }
            Ok(v)
        }
        Err(e) => match e.kind {
            crate::eval::EvalErrorKind::Raised(cond) => {
                ctx.pending_raise = Some(cond);
                Err("__raised__".to_string())
            }
            crate::eval::EvalErrorKind::Escape(eid, v) => {
                ctx.pending_escape = Some((eid, v));
                Err("__escape__".to_string())
            }
            crate::eval::EvalErrorKind::Message(m) => Err(m),
        },
    }
}

/// `(with-input-from-string str thunk)` — installs a string input port as
/// `current-input-port` for the dynamic extent of `thunk`.
fn b_with_input_from_string(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("with-input-from-string", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("with-input-from-string", "string", v)),
    };
    let port = Value::Port(Port::string_input(&s));
    let prev = ctx.current_input_port.take();
    ctx.current_input_port = Some(port);
    let result = apply_procedure(&args[1], &[], ctx).map_err(|e| e.message());
    ctx.current_input_port = prev;
    result
}

/// `(with-output-to-string thunk)` — installs a string output port, runs
/// the thunk, returns the accumulated output as a string.
fn b_with_output_to_string(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("with-output-to-string", "1", args.len()));
    }
    let port = Port::string_output();
    let port_val = Value::Port(port.clone());
    let prev = ctx.current_output_port.take();
    ctx.current_output_port = Some(port_val);
    let result = apply_procedure(&args[0], &[], ctx).map_err(|e| e.message());
    ctx.current_output_port = prev;
    result?;
    // Extract accumulated string from the port we kept a reference to.
    match &*port {
        Port::StringOutput(buf) => Ok(Value::string(buf.borrow().clone())),
        _ => unreachable!(),
    }
}

/// `(with-output-to-file path thunk)` — open `path` for output, run
/// `thunk` with `current-output-port` redirected to it, then close the
/// file (which flushes the buffer to disk). Returns the thunk's value.
/// Errors raised inside the thunk still close the file.
fn b_with_output_to_file(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("with-output-to-file", "2", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("with-output-to-file", "string", v)),
    };
    // Eager check: surface I/O errors before running the thunk.
    std::fs::write(&path, "")
        .map_err(|e| format!("with-output-to-file: cannot create {}: {}", path, e))?;
    let port = Port::file_output(path.clone());
    let port_val = Value::Port(port.clone());
    let prev = ctx.current_output_port.take();
    ctx.current_output_port = Some(port_val);
    let result = apply_procedure(&args[1], &[], ctx);
    ctx.current_output_port = prev;
    // Always flush+close the port, even if the thunk raised — programs
    // that catch the condition can rely on partial output landing on
    // disk before the propagation continues.
    if let Port::FileOutput(state) = &*port {
        let mut s = state.borrow_mut();
        if !s.closed {
            let buf = std::mem::take(&mut s.buf);
            s.closed = true;
            drop(s);
            std::fs::write(&path, &buf)
                .map_err(|e| format!("with-output-to-file: write {} failed: {}", path, e))?;
        }
    }
    result.map_err(|e| e.message())
}

/// `(with-input-from-file path thunk)` — read `path` into a string-input
/// port, run `thunk` with `current-input-port` redirected to it, then
/// restore. Returns the thunk's value. The file is read in full at
/// open time; streaming file input is a future iteration.
fn b_with_input_from_file(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("with-input-from-file", "2", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("with-input-from-file", "string", v)),
    };
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("with-input-from-file: cannot read {}: {}", path, e))?;
    let port = Port::string_input(&contents);
    let port_val = Value::Port(port);
    let prev = ctx.current_input_port.take();
    ctx.current_input_port = Some(port_val);
    let result = apply_procedure(&args[1], &[], ctx).map_err(|e| e.message());
    ctx.current_input_port = prev;
    result
}

fn b_force(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("force", "1", args.len()));
    }
    match &args[0] {
        Value::Promise(p) => {
            // Check if already forced
            {
                let state = p.state.borrow();
                if let PromiseState::Forced(v) = &*state {
                    return Ok(v.clone());
                }
            }
            // Pending: invoke thunk and memoize.
            let thunk = match &*p.state.borrow() {
                PromiseState::Pending(t) => t.clone(),
                PromiseState::Forced(_) => unreachable!(),
            };
            let v = apply_procedure(&thunk, &[], ctx).map_err(|e| e.message())?;
            *p.state.borrow_mut() = PromiseState::Forced(v.clone());
            Ok(v)
        }
        // R6RS-style: force on a non-promise just returns the value.
        v => Ok(v.clone()),
    }
}

// ---- bytevectors ----

fn b_make_bytevector(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("make-bytevector", "1 or 2", args.len()));
    }
    let n = as_int_i64("make-bytevector", &args[0])?;
    if n < 0 {
        return Err("make-bytevector: negative length".into());
    }
    let fill = if args.len() == 2 {
        let v = as_int_i64("make-bytevector", &args[1])?;
        if !(0..=255).contains(&v) {
            return Err("make-bytevector: fill out of u8 range".into());
        }
        v as u8
    } else {
        0u8
    };
    let bv: Vec<u8> = std::iter::repeat(fill).take(n as usize).collect();
    Ok(Value::ByteVector(std::rc::Rc::new(
        std::cell::RefCell::new(bv),
    )))
}

fn b_bytevector(args: &[Value]) -> Result<Value, String> {
    let mut bv = Vec::with_capacity(args.len());
    for a in args {
        let v = as_int_i64("bytevector", a)?;
        if !(0..=255).contains(&v) {
            return Err("bytevector: byte out of u8 range".into());
        }
        bv.push(v as u8);
    }
    Ok(Value::ByteVector(std::rc::Rc::new(
        std::cell::RefCell::new(bv),
    )))
}

/// `(string->utf8 string [start [end]])` — encode a string into a
/// fresh bytevector of UTF-8 bytes. Optional [start, end) is a
/// character-index range. Rust strings are already UTF-8, so the
/// encoding is a slice copy after computing byte boundaries from
/// character indices.
fn b_string_to_utf8(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("string->utf8", "1..3", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        other => return Err(type_err("string->utf8", "string", other)),
    };
    let total_chars = s.chars().count();
    let start = if args.len() >= 2 {
        as_int_i64("string->utf8", &args[1])? as usize
    } else {
        0
    };
    let end = if args.len() >= 3 {
        as_int_i64("string->utf8", &args[2])? as usize
    } else {
        total_chars
    };
    if start > end || end > total_chars {
        return Err(format!(
            "string->utf8: bad range [{}, {}) for length {}",
            start, end, total_chars
        ));
    }
    let mut byte_start = 0usize;
    let mut byte_end = 0usize;
    for (i, (offset, _)) in s.char_indices().enumerate() {
        if i == start {
            byte_start = offset;
        }
        if i == end {
            byte_end = offset;
            break;
        }
    }
    if end == total_chars {
        byte_end = s.len();
    }
    let bytes = s.as_bytes()[byte_start..byte_end].to_vec();
    Ok(Value::ByteVector(std::rc::Rc::new(
        std::cell::RefCell::new(bytes),
    )))
}

/// `(utf8->string bytevector [start [end]])` — decode a bytevector
/// from UTF-8 into a string. Invalid UTF-8 sequences raise a proper
/// condition rather than producing replacement characters.
fn b_utf8_to_string(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("utf8->string", "1..3", args.len()));
    }
    let bv = match &args[0] {
        Value::ByteVector(b) => b.borrow().clone(),
        other => return Err(type_err("utf8->string", "bytevector", other)),
    };
    let len = bv.len();
    let start = if args.len() >= 2 {
        as_int_i64("utf8->string", &args[1])? as usize
    } else {
        0
    };
    let end = if args.len() >= 3 {
        as_int_i64("utf8->string", &args[2])? as usize
    } else {
        len
    };
    if start > end || end > len {
        return Err(format!(
            "utf8->string: bad range [{}, {}) for length {}",
            start, end, len
        ));
    }
    let s = std::str::from_utf8(&bv[start..end])
        .map_err(|e| format!("utf8->string: invalid UTF-8 at byte {}", e.valid_up_to()))?;
    Ok(Value::string(s.to_string()))
}

/// `(bytevector-append bv ...)` — concatenate any number of bytevectors
/// into a fresh one (R6RS).
fn b_bytevector_append(args: &[Value]) -> Result<Value, String> {
    let mut out: Vec<u8> = Vec::new();
    for (i, a) in args.iter().enumerate() {
        match a {
            Value::ByteVector(b) => out.extend_from_slice(&b.borrow()),
            other => {
                return Err(format!(
                    "bytevector-append: arg {} expected bytevector, got {}",
                    i + 1,
                    other.type_name()
                ));
            }
        }
    }
    Ok(Value::ByteVector(std::rc::Rc::new(
        std::cell::RefCell::new(out),
    )))
}

/// `(bytevector-fill! bv byte)` — write `byte` to every slot of `bv`.
fn b_bytevector_fill(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector-fill!", "2", args.len()));
    }
    let byte = as_int_i64("bytevector-fill!", &args[1])?;
    if !(0..=255).contains(&byte) {
        return Err("bytevector-fill!: byte out of u8 range".into());
    }
    let bv = match &args[0] {
        Value::ByteVector(b) => b.clone(),
        other => return Err(type_err("bytevector-fill!", "bytevector", other)),
    };
    let mut v = bv.borrow_mut();
    for slot in v.iter_mut() {
        *slot = byte as u8;
    }
    Ok(Value::Unspecified)
}

fn b_bytevector_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bytevector?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::ByteVector(_))))
}

fn b_bytevector_length(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bytevector-length", "1", args.len()));
    }
    match &args[0] {
        Value::ByteVector(bv) => Ok(Value::fixnum(bv.borrow().len() as i64)),
        v => Err(type_err("bytevector-length", "bytevector", v)),
    }
}

fn b_bytevector_u8_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector-u8-ref", "2", args.len()));
    }
    let i = as_int_i64("bytevector-u8-ref", &args[1])?;
    if i < 0 {
        return Err("bytevector-u8-ref: negative index".into());
    }
    match &args[0] {
        Value::ByteVector(bv) => bv
            .borrow()
            .get(i as usize)
            .map(|b| Value::fixnum(*b as i64))
            .ok_or_else(|| "bytevector-u8-ref: index out of range".into()),
        v => Err(type_err("bytevector-u8-ref", "bytevector", v)),
    }
}

fn b_bytevector_u8_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-u8-set!", "3", args.len()));
    }
    let i = as_int_i64("bytevector-u8-set!", &args[1])?;
    let val = as_int_i64("bytevector-u8-set!", &args[2])?;
    if !(0..=255).contains(&val) {
        return Err("bytevector-u8-set!: value out of u8 range".into());
    }
    if i < 0 {
        return Err("bytevector-u8-set!: negative index".into());
    }
    match &args[0] {
        Value::ByteVector(bv) => {
            let mut bv = bv.borrow_mut();
            if (i as usize) >= bv.len() {
                return Err("bytevector-u8-set!: index out of range".into());
            }
            bv[i as usize] = val as u8;
            Ok(Value::Unspecified)
        }
        v => Err(type_err("bytevector-u8-set!", "bytevector", v)),
    }
}

fn b_bytevector_copy(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bytevector-copy", "1", args.len()));
    }
    match &args[0] {
        Value::ByteVector(bv) => Ok(Value::ByteVector(std::rc::Rc::new(
            std::cell::RefCell::new(bv.borrow().clone()),
        ))),
        v => Err(type_err("bytevector-copy", "bytevector", v)),
    }
}

fn b_bytevector_to_u8_list(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bytevector->u8-list", "1", args.len()));
    }
    match &args[0] {
        Value::ByteVector(bv) => Ok(Value::list(
            bv.borrow().iter().map(|b| Value::fixnum(*b as i64)),
        )),
        v => Err(type_err("bytevector->u8-list", "bytevector", v)),
    }
}

fn b_u8_list_to_bytevector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("u8-list->bytevector", "1", args.len()));
    }
    let items = collect_proper_list("u8-list->bytevector", &args[0])?;
    let mut bv = Vec::with_capacity(items.len());
    for v in items {
        let n = as_int_i64("u8-list->bytevector", &v)?;
        if !(0..=255).contains(&n) {
            return Err("u8-list->bytevector: byte out of range".into());
        }
        bv.push(n as u8);
    }
    Ok(Value::ByteVector(std::rc::Rc::new(
        std::cell::RefCell::new(bv),
    )))
}

// ---- current-port + gensym ----

fn b_current_input_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-input-port", "0", args.len()));
    }
    Ok(ctx.current_input_port.clone().unwrap_or(Value::Unspecified))
}

fn b_current_output_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-output-port", "0", args.len()));
    }
    Ok(ctx
        .current_output_port
        .clone()
        .unwrap_or(Value::Unspecified))
}

/// `(gensym [prefix])` returns a freshly-interned symbol whose name is
/// guaranteed not to clash with any prior gensym call (foundation: keyed by
/// a counter on the symbol-table size + a random-ish suffix).
fn b_gensym(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("gensym", "0 or 1", args.len()));
    }
    let prefix = if args.len() == 1 {
        match &args[0] {
            Value::String(s) => s.borrow().clone(),
            Value::Symbol(s) => ctx.syms.name(*s).to_string(),
            v => return Err(type_err("gensym", "string or symbol", v)),
        }
    } else {
        "g".to_string()
    };
    // Use the current size of the symbol table as a counter.
    let n = ctx.syms.len();
    let name = format!("{}__{}", prefix, n);
    let sym = ctx.syms.intern(&name);
    Ok(Value::Symbol(sym))
}

// ---- bitwise (R6RS arithmetic bitwise) ----

fn b_bitwise_and(args: &[Value]) -> Result<Value, String> {
    let mut acc: i64 = -1; // all ones
    for a in args {
        acc &= as_int_i64("bitwise-and", a)?;
    }
    Ok(Value::fixnum(acc))
}

fn b_bitwise_or(args: &[Value]) -> Result<Value, String> {
    let mut acc: i64 = 0;
    for a in args {
        acc |= as_int_i64("bitwise-or", a)?;
    }
    Ok(Value::fixnum(acc))
}

fn b_bitwise_xor(args: &[Value]) -> Result<Value, String> {
    let mut acc: i64 = 0;
    for a in args {
        acc ^= as_int_i64("bitwise-xor", a)?;
    }
    Ok(Value::fixnum(acc))
}

fn b_bitwise_not(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bitwise-not", "1", args.len()));
    }
    Ok(Value::fixnum(!as_int_i64("bitwise-not", &args[0])?))
}

fn b_bitwise_arith_shift(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bitwise-arithmetic-shift", "2", args.len()));
    }
    let n = as_int_i64("bitwise-arithmetic-shift", &args[0])?;
    let count = as_int_i64("bitwise-arithmetic-shift", &args[1])?;
    let result = if count >= 0 {
        if count >= 64 {
            0
        } else {
            n.wrapping_shl(count as u32)
        }
    } else {
        let abs = (-count) as u32;
        if abs >= 64 {
            if n < 0 {
                -1
            } else {
                0
            }
        } else {
            n.wrapping_shr(abs)
        }
    };
    Ok(Value::fixnum(result))
}

fn b_bitwise_arith_shift_left(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bitwise-arithmetic-shift-left", "2", args.len()));
    }
    let n = as_int_i64("bitwise-arithmetic-shift-left", &args[0])?;
    let count = as_int_i64("bitwise-arithmetic-shift-left", &args[1])?;
    if count < 0 {
        return Err("bitwise-arithmetic-shift-left: negative count".into());
    }
    let result = if count >= 64 {
        0
    } else {
        n.wrapping_shl(count as u32)
    };
    Ok(Value::fixnum(result))
}

fn b_bitwise_arith_shift_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bitwise-arithmetic-shift-right", "2", args.len()));
    }
    let n = as_int_i64("bitwise-arithmetic-shift-right", &args[0])?;
    let count = as_int_i64("bitwise-arithmetic-shift-right", &args[1])?;
    if count < 0 {
        return Err("bitwise-arithmetic-shift-right: negative count".into());
    }
    let result = if count >= 64 {
        if n < 0 {
            -1
        } else {
            0
        }
    } else {
        n.wrapping_shr(count as u32)
    };
    Ok(Value::fixnum(result))
}

fn b_bitwise_bit_count(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bitwise-bit-count", "1", args.len()));
    }
    let n = as_int_i64("bitwise-bit-count", &args[0])?;
    // R6RS: For non-negative n, returns count of 1 bits.
    // For negative, returns -1 - (count of 1 bits in (bitwise-not n)).
    let result = if n >= 0 {
        n.count_ones() as i64
    } else {
        -1 - ((!n).count_ones() as i64)
    };
    Ok(Value::fixnum(result))
}

fn b_bitwise_length(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("bitwise-length", "1", args.len()));
    }
    let n = as_int_i64("bitwise-length", &args[0])?;
    let abs = if n < 0 { !n } else { n };
    let bits = if abs == 0 {
        0
    } else {
        64 - abs.leading_zeros() as i64
    };
    Ok(Value::fixnum(bits))
}

fn b_bitwise_bit_set_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bitwise-bit-set?", "2", args.len()));
    }
    let n = as_int_i64("bitwise-bit-set?", &args[0])?;
    let bit = as_int_i64("bitwise-bit-set?", &args[1])?;
    if bit < 0 || bit >= 64 {
        return Ok(Value::Boolean(false));
    }
    Ok(Value::Boolean((n >> bit) & 1 == 1))
}

// ---- exact-integer-sqrt ----

fn b_exact_integer_sqrt(args: &[Value]) -> Result<Value, String> {
    // Returns two values: the integer square root and the remainder.
    if args.len() != 1 {
        return Err(arity_err("exact-integer-sqrt", "1", args.len()));
    }
    let n = as_int_i64("exact-integer-sqrt", &args[0])?;
    if n < 0 {
        return Err("exact-integer-sqrt: negative argument".into());
    }
    let s = (n as f64).sqrt() as i64;
    // Adjust in case of float rounding error.
    let mut s = s;
    while s * s > n {
        s -= 1;
    }
    while (s + 1) * (s + 1) <= n {
        s += 1;
    }
    let rem = n - s * s;
    // Multi-value return — but pure builtins don't have ctx access; do it
    // differently: return a list. R6RS spec wants multi-values via values.
    // For our simplified impl we return them as a 2-element list and tell users
    // to use call-with-values via a wrapper later. (Actually return as list
    // for now; real multi-value lands when this becomes higher-order.)
    Ok(Value::list([Value::fixnum(s), Value::fixnum(rem)]))
}

// ---- environments ----
//
// Foundation: every binding is global, so all `environment` /
// `interaction-environment` calls return the same opaque sentinel.
// `eval` accepts and ignores the env argument. Real per-import
// environments will land alongside library namespace filtering.

fn b_environment(_args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // R6RS allows any number of import-specs; we accept and ignore.
    Ok(Value::Symbol(ctx.syms.intern("__top-level-env__")))
}

fn b_interaction_environment(_args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    Ok(Value::Symbol(ctx.syms.intern("__top-level-env__")))
}

// ---- eval ----

fn b_eval(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("eval", "1 or 2", args.len()));
    }
    // We ignore the 2nd argument (environment) for foundation; always use top-level.
    // Convert the Value back into a Datum-like form by serializing-then-parsing.
    let datum_str = args[0].format_with(ctx.syms, WriteMode::Write);
    // Parse datum_str into a Datum tree using a fresh file id.
    let file_id = cs_diag::FileId(u32::MAX - 1);
    let data = cs_parse::read_all(file_id, &datum_str, ctx.syms).map_err(|errs| {
        let e = errs.into_iter().next().unwrap();
        format!("eval: parse error: {}", e.message())
    })?;
    if data.is_empty() {
        return Ok(Value::Unspecified);
    }
    let mut expander = cs_expand::Expander::new(ctx.syms, ctx.macros);
    let core = expander
        .expand_program(&data)
        .map_err(|e| format!("eval: expand error: {}", e.message()))?;
    drop(expander);
    crate::eval::eval(&core, ctx.top.clone(), ctx).map_err(|e| e.message())
}

// ---- error-object accessors ----
// `error-object?` is R7RS-flavored — it succeeds on any condition that
// `error?` / `assertion-violation?` would, since both produce conditions
// containing an `&error` simple. The message/irritants accessors decode the
// `&message` / `&irritants` simples in the compound condition.

fn b_error_object_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("error-object?", "1", args.len()));
    }
    Ok(Value::Boolean(is_any_cond(&args[0])))
}

fn b_error_object_message(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("error-object-message", "1", args.len()));
    }
    if !is_any_cond(&args[0]) {
        return Err(type_err(
            "error-object-message",
            "error condition",
            &args[0],
        ));
    }
    // Empty string when the condition has no &message simple — keeps R7RS
    // callers from blowing up on a `(error "no message")` shape we may have
    // received from elsewhere.
    if let Some(simple) = find_simple_with_tag(&args[0], TAG_MESSAGE) {
        if let Value::Vector(vc) = simple {
            let v = vc.borrow();
            if v.len() >= 2 {
                return Ok(v[1].clone());
            }
        }
    }
    Ok(Value::string(""))
}

fn b_error_object_irritants(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("error-object-irritants", "1", args.len()));
    }
    if !is_any_cond(&args[0]) {
        return Err(type_err(
            "error-object-irritants",
            "error condition",
            &args[0],
        ));
    }
    if let Some(simple) = find_simple_with_tag(&args[0], TAG_IRRITANTS) {
        if let Value::Vector(vc) = simple {
            let v = vc.borrow();
            if v.len() >= 2 {
                return Ok(v[1].clone());
            }
        }
    }
    Ok(Value::Null)
}

// ---- make-parameter ----

fn b_make_parameter(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("make-parameter", "1 or 2", args.len()));
    }
    // R6RS make-parameter takes (init [converter]); we ignore converter for now.
    Ok(crate::proc::make_parameter(args[0].clone()))
}

// ---- SRFI-1 extras ----

fn b_delete(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("delete", "2", args.len()));
    }
    let target = &args[0];
    let items = collect_proper_list("delete", &args[1])?;
    let kept: Vec<Value> = items
        .into_iter()
        .filter(|v| !eq::equal(v, target))
        .collect();
    Ok(Value::list(kept))
}

fn b_delete_duplicates(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("delete-duplicates", "1", args.len()));
    }
    let items = collect_proper_list("delete-duplicates", &args[0])?;
    let mut seen: Vec<Value> = Vec::new();
    for v in items {
        if !seen.iter().any(|s| eq::equal(s, &v)) {
            seen.push(v);
        }
    }
    Ok(Value::list(seen))
}

fn b_concatenate(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("concatenate", "1", args.len()));
    }
    let lists = collect_proper_list("concatenate", &args[0])?;
    let mut all = Vec::new();
    for l in lists {
        let inner = collect_proper_list("concatenate", &l)?;
        all.extend(inner);
    }
    Ok(Value::list(all))
}

/// `(cons* x1 x2 ... xn lst)` — like `list*`. Builds
/// `(cons x1 (cons x2 (... (cons xn lst))))`. With one arg, returns it
/// unchanged. With zero args, errors (R6RS spec — needs at least one).
fn b_cons_star(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("cons*", "at least 1", 0));
    }
    if args.len() == 1 {
        return Ok(args[0].clone());
    }
    // Build right-to-left: start with the last arg as the tail.
    let mut acc = args[args.len() - 1].clone();
    for v in args[..args.len() - 1].iter().rev() {
        acc = Value::Pair(cs_core::Pair::new(v.clone(), acc));
    }
    Ok(acc)
}

/// `(alist-copy alist)` — deep-copies the spine and the cons cells of
/// each entry, but leaves the keys/values themselves shared. Useful
/// when callers need to mutate without affecting the original.
fn b_alist_copy(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("alist-copy", "1", args.len()));
    }
    let entries = collect_proper_list("alist-copy", &args[0])?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        match &e {
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                let cdr = p.cdr.borrow().clone();
                out.push(Value::Pair(cs_core::Pair::new(car, cdr)));
            }
            _ => return Err(type_err("alist-copy", "pair", &e)),
        }
    }
    Ok(Value::list(out))
}

fn b_first(args: &[Value]) -> Result<Value, String> {
    b_car(args).map_err(|_| "first: not a non-empty list".into())
}

fn b_second(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("second", "1", args.len()));
    }
    let items = collect_proper_list("second", &args[0])?;
    items
        .get(1)
        .cloned()
        .ok_or_else(|| "second: list has fewer than 2 elements".into())
}

fn b_third(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("third", "1", args.len()));
    }
    let items = collect_proper_list("third", &args[0])?;
    items
        .get(2)
        .cloned()
        .ok_or_else(|| "third: list has fewer than 3 elements".into())
}

// ---- hashtable conversions ----

fn b_hashtable_to_alist(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable->alist", "1", args.len()));
    }
    let h = as_ht("hashtable->alist", &args[0])?;
    let items = h.items.borrow();
    let pairs: Vec<Value> = items
        .iter()
        .map(|(k, v)| Value::Pair(Pair::new(k.clone(), v.clone())))
        .collect();
    Ok(Value::list(pairs))
}

fn b_alist_to_hashtable(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("alist->hashtable", "1 or 2", args.len()));
    }
    // Optional second arg picks eq-kind: 'eq, 'eqv, 'equal. Default equal.
    let kind = if args.len() == 2 {
        match &args[1] {
            Value::Symbol(_) => HtEqKind::Equal, // any symbol — defaulting
            _ => HtEqKind::Equal,
        }
    } else {
        HtEqKind::Equal
    };
    let h = Hashtable::new(kind);
    let items = collect_proper_list("alist->hashtable", &args[0])?;
    for entry in items {
        match entry {
            Value::Pair(p) => {
                let k = p.car.borrow().clone();
                let v = p.cdr.borrow().clone();
                h.items.borrow_mut().push((k, v));
            }
            other => {
                return Err(type_err(
                    "alist->hashtable",
                    "list of (key . value) pairs",
                    &other,
                ));
            }
        }
    }
    Ok(Value::Hashtable(h))
}

// ---- SRFI-1 higher-order extras ----

fn b_tabulate(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("tabulate", "2", args.len()));
    }
    let n = as_int_i64("tabulate", &args[0])?;
    if n < 0 {
        return Err("tabulate: negative count".into());
    }
    let proc_val = args[1].clone();
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let r = apply_procedure(&proc_val, &[Value::fixnum(i)], ctx).map_err(|e| e.message())?;
        out.push(r);
    }
    Ok(Value::list(out))
}

fn b_remove(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // (remove pred list) — like filter but keeps non-matching elements.
    if args.len() != 2 {
        return Err(arity_err("remove", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("remove", &args[1])?;
    let mut out = Vec::new();
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if !r.is_truthy() {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

// ---- transcendental functions ----

fn unary_flonum(name: &str, args: &[Value], op: fn(f64) -> f64) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    let n = as_num(name, &args[0])?;
    Ok(Value::flonum(op(n.to_f64())))
}

fn b_sqrt(args: &[Value]) -> Result<Value, String> {
    unary_flonum("sqrt", args, f64::sqrt)
}
fn b_exp(args: &[Value]) -> Result<Value, String> {
    unary_flonum("exp", args, f64::exp)
}
fn b_log(args: &[Value]) -> Result<Value, String> {
    if args.len() == 1 {
        unary_flonum("log", args, f64::ln)
    } else if args.len() == 2 {
        let n = as_num("log", &args[0])?.to_f64();
        let base = as_num("log", &args[1])?.to_f64();
        Ok(Value::flonum(n.log(base)))
    } else {
        Err(arity_err("log", "1 or 2", args.len()))
    }
}
fn b_sin(args: &[Value]) -> Result<Value, String> {
    unary_flonum("sin", args, f64::sin)
}
fn b_cos(args: &[Value]) -> Result<Value, String> {
    unary_flonum("cos", args, f64::cos)
}
fn b_tan(args: &[Value]) -> Result<Value, String> {
    unary_flonum("tan", args, f64::tan)
}
fn b_asin(args: &[Value]) -> Result<Value, String> {
    unary_flonum("asin", args, f64::asin)
}
fn b_acos(args: &[Value]) -> Result<Value, String> {
    unary_flonum("acos", args, f64::acos)
}
fn b_atan(args: &[Value]) -> Result<Value, String> {
    if args.len() == 1 {
        unary_flonum("atan", args, f64::atan)
    } else if args.len() == 2 {
        // (atan y x) — two-argument form
        let y = as_num("atan", &args[0])?.to_f64();
        let x = as_num("atan", &args[1])?.to_f64();
        Ok(Value::flonum(y.atan2(x)))
    } else {
        Err(arity_err("atan", "1 or 2", args.len()))
    }
}

// ---- I/O extras ----

fn b_read_line_implicit(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("read-line", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "read-line: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_get_line(&[port])
}

fn b_get_string_all(args: &[Value], _ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("get-string-all", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.chars.len() {
                    return Ok(Value::Eof);
                }
                let collected: String = s.chars[s.pos..].iter().collect();
                s.pos = s.chars.len();
                Ok(Value::string(collected))
            }
            _ => Err("get-string-all: not an input port".into()),
        },
        v => Err(type_err("get-string-all", "input-port", v)),
    }
}

// ---- string-map / string-for-each ----

fn b_string_map(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-map", "2", args.len()));
    }
    let proc_val = args[0].clone();
    let s = match &args[1] {
        Value::String(s) => s.borrow().chars().collect::<Vec<char>>(),
        v => return Err(type_err("string-map", "string", v)),
    };
    let mut out = String::with_capacity(s.len());
    for c in s {
        let r = apply_procedure(&proc_val, &[Value::Character(c)], ctx).map_err(|e| e.message())?;
        match r {
            Value::Character(c) => out.push(c),
            other => {
                return Err(type_err(
                    "string-map",
                    "character (from proc result)",
                    &other,
                ))
            }
        }
    }
    Ok(Value::string(out))
}

fn b_string_for_each(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-for-each", "2", args.len()));
    }
    let proc_val = args[0].clone();
    let chars: Vec<char> = match &args[1] {
        Value::String(s) => s.borrow().chars().collect(),
        v => return Err(type_err("string-for-each", "string", v)),
    };
    for c in chars {
        apply_procedure(&proc_val, &[Value::Character(c)], ctx).map_err(|e| e.message())?;
    }
    Ok(Value::Unspecified)
}

// ---- vector-filter / vector-fold ----

fn b_vector_filter(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("vector-filter", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = match &args[1] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector-filter", "vector", v)),
    };
    let mut out = Vec::new();
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if r.is_truthy() {
            out.push(item);
        }
    }
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(
        out,
    ))))
}

fn b_vector_fold(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("vector-fold", "3", args.len()));
    }
    let proc_val = args[0].clone();
    let mut acc = args[1].clone();
    let items = match &args[2] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector-fold", "vector", v)),
    };
    for item in items {
        acc = apply_procedure(&proc_val, &[acc, item], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

// ---- sorting (R6RS) ----

/// `(list-sort comparator list)` returns a new sorted list.
/// `comparator` is a 2-arg procedure: returns truthy iff a < b.
fn b_list_sort(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("list-sort", "2", args.len()));
    }
    let cmp = args[0].clone();
    let mut items = collect_proper_list("list-sort", &args[1])?;
    sort_with_predicate(&mut items, &cmp, ctx)?;
    Ok(Value::list(items))
}

fn b_vector_sort(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("vector-sort", "2", args.len()));
    }
    let cmp = args[0].clone();
    let mut items = match &args[1] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector-sort", "vector", v)),
    };
    sort_with_predicate(&mut items, &cmp, ctx)?;
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(
        items,
    ))))
}

fn b_vector_sort_bang(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("vector-sort!", "2", args.len()));
    }
    let cmp = args[0].clone();
    let v = match &args[1] {
        Value::Vector(v) => v.clone(),
        other => return Err(type_err("vector-sort!", "vector", other)),
    };
    let mut items = v.borrow_mut();
    let mut owned = std::mem::take(&mut *items);
    sort_with_predicate(&mut owned, &cmp, ctx)?;
    *items = owned;
    Ok(Value::Unspecified)
}

/// In-place sort using the user-supplied comparator. Implemented with
/// merge sort so we don't have to interleave comparator calls with the
/// borrow checker (Rust's slice::sort_by is iterative and uses Ord).
fn sort_with_predicate(
    items: &mut Vec<Value>,
    cmp: &Value,
    ctx: &mut EvalCtx,
) -> Result<(), String> {
    let n = items.len();
    if n < 2 {
        return Ok(());
    }
    // Insertion sort for foundation simplicity; O(n²) but small n suffices.
    for i in 1..n {
        let mut j = i;
        while j > 0 {
            let r = apply_procedure(cmp, &[items[j].clone(), items[j - 1].clone()], ctx)
                .map_err(|e| e.message())?;
            if !r.is_truthy() {
                break;
            }
            items.swap(j, j - 1);
            j -= 1;
        }
    }
    Ok(())
}

// ---- file ports ----

fn b_file_exists_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("file-exists?", "1", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("file-exists?", "string", v)),
    };
    Ok(Value::Boolean(std::path::Path::new(&path).exists()))
}

fn b_delete_file(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("delete-file", "1", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("delete-file", "string", v)),
    };
    std::fs::remove_file(&path).map_err(|e| format!("delete-file: {}", e))?;
    Ok(Value::Unspecified)
}

fn b_open_input_file(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("open-input-file", "1", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("open-input-file", "string", v)),
    };
    let contents = std::fs::read_to_string(&path)
        .map_err(|e| format!("open-input-file: cannot read {}: {}", path, e))?;
    Ok(Value::Port(Port::string_input(&contents)))
}

fn b_open_output_file(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("open-output-file", "1", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("open-output-file", "string", v)),
    };
    // Eagerly check that the file is openable so users see permission /
    // missing-directory errors at open time, not at close-port time.
    std::fs::write(&path, "")
        .map_err(|e| format!("open-output-file: cannot create {}: {}", path, e))?;
    Ok(Value::Port(Port::file_output(path)))
}

fn b_close_port(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("close-port", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            // File output ports flush their buffer to disk on close. The
            // `closed` flag prevents subsequent writes.
            Port::FileOutput(state) => {
                let mut s = state.borrow_mut();
                if !s.closed {
                    let path = s.path.clone();
                    let buf = std::mem::take(&mut s.buf);
                    s.closed = true;
                    drop(s);
                    std::fs::write(&path, &buf)
                        .map_err(|e| format!("close-port: write {} failed: {}", path, e))?;
                }
                Ok(Value::Unspecified)
            }
            // Other port kinds are no-op on close at this milestone — they
            // hold no OS resources.
            _ => Ok(Value::Unspecified),
        },
        v => Err(type_err("close-port", "port", v)),
    }
}

fn b_port_eof_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("port-eof?", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let s = state.borrow();
                Ok(Value::Boolean(s.pos >= s.chars.len()))
            }
            Port::ByteVectorInput(state) => {
                let s = state.borrow();
                Ok(Value::Boolean(s.pos >= s.bytes.len()))
            }
            _ => Ok(Value::Boolean(false)),
        },
        v => Err(type_err("port-eof?", "port", v)),
    }
}

// ---- assertion-violation? ----

fn b_assertion_violation_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("assertion-violation?", "1", args.len()));
    }
    // R6RS: any condition containing an `&assertion` simple (or descendant —
    // there are none in the standard hierarchy).
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_ASSERTION)))
}

// ---- copy variants ----

fn b_vector_copy(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("vector-copy", "1..3", args.len()));
    }
    let v = match &args[0] {
        Value::Vector(v) => v.borrow().clone(),
        other => return Err(type_err("vector-copy", "vector", other)),
    };
    let start = if args.len() >= 2 {
        as_int_i64("vector-copy", &args[1])? as usize
    } else {
        0
    };
    let end = if args.len() >= 3 {
        as_int_i64("vector-copy", &args[2])? as usize
    } else {
        v.len()
    };
    if start > v.len() || end > v.len() || start > end {
        return Err("vector-copy: indices out of range".into());
    }
    let copied: Vec<Value> = v[start..end].to_vec();
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(
        copied,
    ))))
}

fn b_vector_copy_bang(args: &[Value]) -> Result<Value, String> {
    // (vector-copy! dest at src [start [end]])
    if args.len() < 3 || args.len() > 5 {
        return Err(arity_err("vector-copy!", "3..5", args.len()));
    }
    let dest_at = as_int_i64("vector-copy!", &args[1])? as usize;
    let src_items = match &args[2] {
        Value::Vector(v) => v.borrow().clone(),
        other => return Err(type_err("vector-copy!", "vector (src)", other)),
    };
    let src_start = if args.len() >= 4 {
        as_int_i64("vector-copy!", &args[3])? as usize
    } else {
        0
    };
    let src_end = if args.len() == 5 {
        as_int_i64("vector-copy!", &args[4])? as usize
    } else {
        src_items.len()
    };
    if src_start > src_items.len() || src_end > src_items.len() || src_start > src_end {
        return Err("vector-copy!: src indices out of range".into());
    }
    let n = src_end - src_start;
    match &args[0] {
        Value::Vector(dest) => {
            let mut d = dest.borrow_mut();
            if dest_at + n > d.len() {
                return Err("vector-copy!: dest index out of range".into());
            }
            for i in 0..n {
                d[dest_at + i] = src_items[src_start + i].clone();
            }
        }
        other => return Err(type_err("vector-copy!", "vector (dest)", other)),
    }
    Ok(Value::Unspecified)
}

fn b_bytevector_copy_bang(args: &[Value]) -> Result<Value, String> {
    // (bytevector-copy! dest at src [start [end]])
    if args.len() < 3 || args.len() > 5 {
        return Err(arity_err("bytevector-copy!", "3..5", args.len()));
    }
    let dest_at = as_int_i64("bytevector-copy!", &args[1])? as usize;
    let src_items = match &args[2] {
        Value::ByteVector(v) => v.borrow().clone(),
        other => return Err(type_err("bytevector-copy!", "bytevector (src)", other)),
    };
    let src_start = if args.len() >= 4 {
        as_int_i64("bytevector-copy!", &args[3])? as usize
    } else {
        0
    };
    let src_end = if args.len() == 5 {
        as_int_i64("bytevector-copy!", &args[4])? as usize
    } else {
        src_items.len()
    };
    if src_start > src_items.len() || src_end > src_items.len() || src_start > src_end {
        return Err("bytevector-copy!: src indices out of range".into());
    }
    let n = src_end - src_start;
    match &args[0] {
        Value::ByteVector(dest) => {
            let mut d = dest.borrow_mut();
            if dest_at + n > d.len() {
                return Err("bytevector-copy!: dest index out of range".into());
            }
            for i in 0..n {
                d[dest_at + i] = src_items[src_start + i];
            }
        }
        other => return Err(type_err("bytevector-copy!", "bytevector (dest)", other)),
    }
    Ok(Value::Unspecified)
}

fn b_string_copy_bang(args: &[Value]) -> Result<Value, String> {
    // (string-copy! dest at src [start [end]])
    if args.len() < 3 || args.len() > 5 {
        return Err(arity_err("string-copy!", "3..5", args.len()));
    }
    let dest_at = as_int_i64("string-copy!", &args[1])? as usize;
    let src_chars: Vec<char> = match &args[2] {
        Value::String(s) => s.borrow().chars().collect(),
        other => return Err(type_err("string-copy!", "string (src)", other)),
    };
    let src_start = if args.len() >= 4 {
        as_int_i64("string-copy!", &args[3])? as usize
    } else {
        0
    };
    let src_end = if args.len() == 5 {
        as_int_i64("string-copy!", &args[4])? as usize
    } else {
        src_chars.len()
    };
    if src_start > src_chars.len() || src_end > src_chars.len() || src_start > src_end {
        return Err("string-copy!: src indices out of range".into());
    }
    match &args[0] {
        Value::String(dest) => {
            let mut d_chars: Vec<char> = dest.borrow().chars().collect();
            let n = src_end - src_start;
            if dest_at + n > d_chars.len() {
                return Err("string-copy!: dest index out of range".into());
            }
            for i in 0..n {
                d_chars[dest_at + i] = src_chars[src_start + i];
            }
            *dest.borrow_mut() = d_chars.into_iter().collect();
        }
        other => return Err(type_err("string-copy!", "string (dest)", other)),
    }
    Ok(Value::Unspecified)
}

// ---- SRFI-1 extras (HO) ----

/// `(unfold p f g seed)` builds a list. R6RS-srfi-1:
///   - p: stop predicate
///   - f: result function (mapped over each seed)
///   - g: next-seed function
///   - seed: initial seed
fn b_unfold(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("unfold", "4", args.len()));
    }
    let pred = args[0].clone();
    let map_fn = args[1].clone();
    let next_fn = args[2].clone();
    let mut seed = args[3].clone();
    let mut out = Vec::new();
    // Bound by 1M iterations to prevent runaway loops.
    for _ in 0..1_000_000 {
        let stop = apply_procedure(&pred, &[seed.clone()], ctx).map_err(|e| e.message())?;
        if stop.is_truthy() {
            return Ok(Value::list(out));
        }
        let mapped = apply_procedure(&map_fn, &[seed.clone()], ctx).map_err(|e| e.message())?;
        out.push(mapped);
        seed = apply_procedure(&next_fn, &[seed], ctx).map_err(|e| e.message())?;
    }
    Err("unfold: exceeded 1,000,000 iterations".into())
}

/// `(zip-with proc list1 list2 ...)` like R6RS `map` but returns the proc
/// results without the SRFI-1 fancy semantics.
fn b_zip_with(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("zip-with", "at least 2", args.len()));
    }
    // Same as map.
    b_map(args, ctx)
}

// ---- hashtable higher-order ----

fn b_hashtable_fold(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // (hashtable-fold proc init ht) — proc called as (proc key value acc).
    if args.len() != 3 {
        return Err(arity_err("hashtable-fold", "3", args.len()));
    }
    let proc_val = args[0].clone();
    let mut acc = args[1].clone();
    let h = as_ht("hashtable-fold", &args[2])?;
    let entries: Vec<(Value, Value)> = h.items.borrow().clone();
    for (k, v) in entries {
        acc = apply_procedure(&proc_val, &[k, v, acc], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

fn b_hashtable_for_each(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("hashtable-for-each", "2", args.len()));
    }
    let proc_val = args[0].clone();
    let h = as_ht("hashtable-for-each", &args[1])?;
    let entries: Vec<(Value, Value)> = h.items.borrow().clone();
    for (k, v) in entries {
        apply_procedure(&proc_val, &[k, v], ctx).map_err(|e| e.message())?;
    }
    Ok(Value::Unspecified)
}

// ---- string extras: trim/contains/split/join/reverse/<->vector ----

fn b_string_trim(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-trim", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().trim().to_string())),
        v => Err(type_err("string-trim", "string", v)),
    }
}

fn b_string_trim_left(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-trim-left", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().trim_start().to_string())),
        v => Err(type_err("string-trim-left", "string", v)),
    }
}

fn b_string_trim_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-trim-right", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().trim_end().to_string())),
        v => Err(type_err("string-trim-right", "string", v)),
    }
}

fn b_string_contains(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-contains", "2", args.len()));
    }
    let haystack = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-contains", "string", v)),
    };
    let needle = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-contains", "string", v)),
    };
    Ok(match haystack.find(&needle) {
        Some(byte_idx) => {
            let char_idx = haystack[..byte_idx].chars().count() as i64;
            Value::fixnum(char_idx)
        }
        None => Value::Boolean(false),
    })
}

fn b_string_index(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-index", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-index", "string", v)),
    };
    let target = match &args[1] {
        Value::Character(c) => *c,
        v => return Err(type_err("string-index", "character", v)),
    };
    Ok(match s.chars().position(|c| c == target) {
        Some(i) => Value::fixnum(i as i64),
        None => Value::Boolean(false),
    })
}

fn b_string_split(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-split", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-split", "string", v)),
    };
    let sep = match &args[1] {
        Value::String(sep) => sep.borrow().clone(),
        Value::Character(c) => c.to_string(),
        v => return Err(type_err("string-split", "string or character", v)),
    };
    let parts: Vec<Value> = if sep.is_empty() {
        s.chars().map(|c| Value::string(c.to_string())).collect()
    } else {
        s.split(&sep)
            .map(|p| Value::string(p.to_string()))
            .collect()
    };
    Ok(Value::list(parts))
}

fn b_string_join(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("string-join", "1 or 2", args.len()));
    }
    let parts = collect_proper_list("string-join", &args[0])?;
    let sep = if args.len() == 2 {
        match &args[1] {
            Value::String(s) => s.borrow().clone(),
            v => return Err(type_err("string-join", "string", v)),
        }
    } else {
        String::new()
    };
    let mut strs: Vec<String> = Vec::with_capacity(parts.len());
    for p in parts {
        match p {
            Value::String(s) => strs.push(s.borrow().clone()),
            v => return Err(type_err("string-join", "list of strings", &v)),
        }
    }
    Ok(Value::string(strs.join(&sep)))
}

fn b_string_to_vector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string->vector", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => {
            let v: Vec<Value> = s.borrow().chars().map(Value::Character).collect();
            Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(v))))
        }
        v => Err(type_err("string->vector", "string", v)),
    }
}

fn b_vector_to_string(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("vector->string", "1", args.len()));
    }
    match &args[0] {
        Value::Vector(v) => {
            let mut s = String::new();
            for item in v.borrow().iter() {
                match item {
                    Value::Character(c) => s.push(*c),
                    other => return Err(type_err("vector->string", "character", other)),
                }
            }
            Ok(Value::string(s))
        }
        v => Err(type_err("vector->string", "vector of characters", v)),
    }
}

fn b_string_reverse(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("string-reverse", "1", args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::string(s.borrow().chars().rev().collect::<String>())),
        v => Err(type_err("string-reverse", "string", v)),
    }
}

// ---- vector higher-order ----

fn b_vector_map(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("vector-map", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let vectors: Vec<std::cell::Ref<Vec<Value>>> = args[1..]
        .iter()
        .map(|v| match v {
            Value::Vector(vec) => Ok(vec.borrow()),
            other => Err(type_err("vector-map", "vector", other)),
        })
        .collect::<Result<_, _>>()?;
    let n = vectors.iter().map(|v| v.len()).min().unwrap_or(0);
    // Snapshot rows to release borrows before re-entering eval.
    let snapshots: Vec<Vec<Value>> = vectors
        .iter()
        .map(|v| v.iter().take(n).cloned().collect())
        .collect();
    drop(vectors);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row: Vec<Value> = snapshots.iter().map(|s| s[i].clone()).collect();
        let r = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
        out.push(r);
    }
    Ok(Value::Vector(std::rc::Rc::new(std::cell::RefCell::new(
        out,
    ))))
}

fn b_vector_for_each(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("vector-for-each", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let snapshots: Vec<Vec<Value>> = args[1..]
        .iter()
        .map(|v| match v {
            Value::Vector(vec) => Ok(vec.borrow().clone()),
            other => Err(type_err("vector-for-each", "vector", other)),
        })
        .collect::<Result<_, _>>()?;
    let n = snapshots.iter().map(|v| v.len()).min().unwrap_or(0);
    for i in 0..n {
        let row: Vec<Value> = snapshots.iter().map(|s| s[i].clone()).collect();
        apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
    }
    Ok(Value::Unspecified)
}

// ---- read ----

fn b_read(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("read", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port.clone()
    } else {
        Some(args[0].clone())
    };
    let port = match port {
        Some(p) => p,
        None => return Err("read: no current input port".into()),
    };
    match &port {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                let remaining: String = s.chars[s.pos..].iter().collect();
                if remaining.trim().is_empty() {
                    return Ok(Value::Eof);
                }
                let file_id = cs_diag::FileId(u32::MAX - 2);
                let mut reader = cs_parse::Reader::new(file_id, &remaining);
                let datum = reader
                    .read(ctx.syms)
                    .map_err(|e| format!("read: {}", e.message()))?;
                let consumed_bytes = match &datum {
                    // span.end is the byte offset within `remaining` where the
                    // datum finished parsing — exactly where to resume reading.
                    Some(d) => d.span().end as usize,
                    None => remaining.len(),
                };
                let consumed_chars = remaining
                    .char_indices()
                    .take_while(|(b, _)| *b < consumed_bytes)
                    .count();
                s.pos += consumed_chars;
                Ok(datum.map(|d| d.to_value()).unwrap_or(Value::Eof))
            }
            _ => Err("read: not an input port".into()),
        },
        v => Err(type_err("read", "input-port", v)),
    }
}
