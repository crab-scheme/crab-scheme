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
        // R7RS division aliases (single-value forms).
        ("truncate-quotient", b_truncate_quotient),
        ("truncate-remainder", b_truncate_remainder),
        ("floor-quotient", b_floor_quotient),
        ("floor-remainder", b_floor_remainder),
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
        // R6RS (rnrs arithmetic fixnums) — typed fixnum-only ops; raise on
        // non-fixnum or out-of-range results (no implicit promotion to Big).
        ("fx+", b_fx_add),
        ("fx-", b_fx_sub),
        ("fx*", b_fx_mul),
        ("fxdiv", b_fx_div),
        ("fxmod", b_fx_mod),
        ("fxdiv0", b_fx_div0),
        ("fxmod0", b_fx_mod0),
        ("fx=?", b_fx_eq),
        ("fx<?", b_fx_lt),
        ("fx>?", b_fx_gt),
        ("fx<=?", b_fx_le),
        ("fx>=?", b_fx_ge),
        ("fxzero?", b_fx_zero),
        ("fxpositive?", b_fx_positive),
        ("fxnegative?", b_fx_negative),
        ("fxodd?", b_fx_odd),
        ("fxeven?", b_fx_even),
        ("fxmax", b_fx_max),
        ("fxmin", b_fx_min),
        ("fxnot", b_fx_not),
        ("fxand", b_fx_and),
        ("fxior", b_fx_ior),
        ("fxxor", b_fx_xor),
        ("fxarithmetic-shift", b_fx_arith_shift),
        ("fxarithmetic-shift-left", b_fx_arith_shift_left),
        ("fxarithmetic-shift-right", b_fx_arith_shift_right),
        // R6RS (rnrs arithmetic flonums) — typed flonum-only ops; raise on
        // non-flonum operands. Pure IEEE-754 (no exact contagion).
        ("fl+", b_fl_add),
        ("fl-", b_fl_sub),
        ("fl*", b_fl_mul),
        ("fl/", b_fl_div),
        ("fl=?", b_fl_eq),
        ("fl<?", b_fl_lt),
        ("fl>?", b_fl_gt),
        ("fl<=?", b_fl_le),
        ("fl>=?", b_fl_ge),
        ("flzero?", b_fl_zero),
        ("flpositive?", b_fl_positive),
        ("flnegative?", b_fl_negative),
        ("flmax", b_fl_max),
        ("flmin", b_fl_min),
        ("flabs", b_fl_abs),
        ("flfloor", b_fl_floor),
        ("flceiling", b_fl_ceiling),
        ("fltruncate", b_fl_truncate),
        ("flround", b_fl_round),
        ("flsqrt", b_fl_sqrt),
        ("flexp", b_fl_exp),
        ("fllog", b_fl_log),
        ("flsin", b_fl_sin),
        ("flcos", b_fl_cos),
        ("fltan", b_fl_tan),
        ("flnan?", b_fl_nan),
        ("flfinite?", b_fl_finite),
        ("flinfinite?", b_fl_infinite),
        ("flinteger?", b_fl_integer),
        ("fleven?", b_fl_even),
        ("flodd?", b_fl_odd),
        ("fixnum->flonum", b_fixnum_to_flonum),
        // type predicates
        ("number?", b_number_p),
        ("integer?", b_integer_p),
        ("fixnum?", b_fixnum_p),
        ("flonum?", b_flonum_p),
        ("rational?", b_rational_p),
        ("boolean?", b_boolean_p),
        ("pair?", b_pair_p),
        ("null?", b_null_p),
        ("list?", b_list_p),
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
        ("string-ci=?", b_string_ci_eq),
        ("string-ci<?", b_string_ci_lt),
        ("string-ci<=?", b_string_ci_le),
        ("string-ci>?", b_string_ci_gt),
        ("string-ci>=?", b_string_ci_ge),
        ("char-ci=?", b_char_ci_eq),
        ("char-ci<?", b_char_ci_lt),
        ("char-ci<=?", b_char_ci_le),
        ("char-ci>?", b_char_ci_gt),
        ("char-ci>=?", b_char_ci_ge),
        ("string-ref", b_string_ref),
        ("string-set!", b_string_set_bang),
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
        ("char-foldcase", b_char_foldcase),
        ("char-titlecase", b_char_titlecase),
        ("digit-value", b_digit_value),
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
        ("exact->inexact", b_inexact),
        ("inexact->exact", b_exact),
        ("exact?", b_exact_p),
        ("inexact?", b_inexact_p),
        ("exact-integer?", b_exact_integer_p),
        ("exact-nonnegative-integer?", b_exact_nonneg_int_p),
        ("exact-rational?", b_exact_rational_p),
        ("nan?", b_nan_p),
        ("finite?", b_finite_p),
        ("infinite?", b_infinite_p),
        ("numerator", b_numerator),
        ("denominator", b_denominator),
        // string conversions
        ("make-string", b_make_string),
        ("substring", b_substring),
        ("string-copy", b_string_copy),
        ("number->string", b_number_to_string),
        ("string->number", b_string_to_number),
        // vectors
        ("make-vector", b_make_vector),
        ("vector", b_vector),
        ("string", b_string_ctor),
        ("vector-length", b_vector_length),
        ("vector-ref", b_vector_ref),
        ("vector-set!", b_vector_set),
        ("vector-fill!", b_vector_fill),
        ("string-fill!", b_string_fill),
        ("vector->list", b_vector_to_list),
        ("list->vector", b_list_to_vector),
        // assoc lists
        ("assv", b_assv),
        ("assq", b_assq),
        // member family
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
        ("string-index-right", b_string_index_right),
        ("string-contains-right", b_string_contains_right),
        ("string-replace", b_string_replace),
        ("string-replace-all", b_string_replace_all),
        ("string-count", b_string_count),
        // R7RS time / process / environment.
        ("current-second", b_current_second),
        ("current-jiffy", b_current_jiffy),
        ("jiffies-per-second", b_jiffies_per_second),
        ("get-environment-variable", b_get_environment_variable),
        ("get-environment-variables", b_get_environment_variables),
        ("command-line", b_command_line),
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
        ("file-error?", b_file_error_p),
        ("read-error?", b_read_error_p),
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
        ("vector-append", b_vector_append),
        ("subvector", b_subvector),
        ("make-list", b_make_list),
        ("list-copy", b_list_copy),
        ("list-set!", b_list_set_bang),
        ("boolean=?", b_boolean_eq),
        ("symbol=?", b_symbol_eq),
        ("bytevector-copy!", b_bytevector_copy_bang),
        ("string-copy!", b_string_copy_bang),
        // bytevectors
        ("make-bytevector", b_make_bytevector),
        ("bytevector", b_bytevector),
        ("bytevector?", b_bytevector_p),
        ("bytevector-length", b_bytevector_length),
        ("bytevector-u8-ref", b_bytevector_u8_ref),
        ("bytevector-u8-set!", b_bytevector_u8_set),
        ("bytevector-s8-ref", b_bytevector_s8_ref),
        ("bytevector-s8-set!", b_bytevector_s8_set),
        // R6RS (rnrs bytevectors): native-endian variants are pure
        // (no symbol arg). Explicit-endianness variants need ctx for
        // symbol inspection — see higher_order_builtins below.
        ("bytevector-u16-native-ref", b_bytevector_u16_native_ref),
        ("bytevector-u16-native-set!", b_bytevector_u16_native_set),
        ("bytevector-s16-native-ref", b_bytevector_s16_native_ref),
        ("bytevector-s16-native-set!", b_bytevector_s16_native_set),
        ("bytevector-u32-native-ref", b_bytevector_u32_native_ref),
        ("bytevector-u32-native-set!", b_bytevector_u32_native_set),
        ("bytevector-s32-native-ref", b_bytevector_s32_native_ref),
        ("bytevector-s32-native-set!", b_bytevector_s32_native_set),
        ("bytevector-u64-native-ref", b_bytevector_u64_native_ref),
        ("bytevector-u64-native-set!", b_bytevector_u64_native_set),
        ("bytevector-s64-native-ref", b_bytevector_s64_native_ref),
        ("bytevector-s64-native-set!", b_bytevector_s64_native_set),
        (
            "bytevector-ieee-single-native-ref",
            b_bytevector_ieee_single_native_ref,
        ),
        (
            "bytevector-ieee-single-native-set!",
            b_bytevector_ieee_single_native_set,
        ),
        (
            "bytevector-ieee-double-native-ref",
            b_bytevector_ieee_double_native_ref,
        ),
        (
            "bytevector-ieee-double-native-set!",
            b_bytevector_ieee_double_native_set,
        ),
        ("bytevector-copy", b_bytevector_copy),
        ("bytevector->u8-list", b_bytevector_to_u8_list),
        ("u8-list->bytevector", b_u8_list_to_bytevector),
        ("bytevector->list", b_bytevector_to_list_r7rs),
        ("list->bytevector", b_u8_list_to_bytevector),
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
        // read-char / peek-char / read-string are HIGHER (in higher_order_builtins
        // below) so they can default to current-input-port when called with
        // no port arg per R7RS.
        // R7RS aliases for the R6RS open-{string,bytevector}-input-port forms.
        ("open-input-string", b_open_string_input_port),
        ("open-input-bytevector", b_open_bytevector_input_port),
        ("open-output-string", b_open_string_output_port),
        ("open-output-bytevector", b_open_bytevector_output_port),
        ("get-output-bytevector", b_get_bytevector_output_port),
        // char-ready? / read-u8 / peek-u8 / u8-ready? / read-bytevector are
        // HIGHER (in higher_order_builtins below) so they can default to
        // current-input-port when called with no port arg per R7RS.
        ("get-line", b_get_line),
        ("port?", b_port_p),
        ("input-port?", b_input_port_p),
        ("output-port?", b_output_port_p),
        ("close-input-port", b_close_input_port),
        ("close-output-port", b_close_output_port),
        ("flush-output-port", b_flush_output_port),
        ("input-port-open?", b_input_port_open_p),
        ("output-port-open?", b_output_port_open_p),
        // write-char / write-string are HIGHER (in higher_order_builtins
        // below) so they can default to current-output-port when called
        // with no port arg per R7RS.
        // write-u8 / write-bytevector are HIGHER (default current-output-port).
        // promises
        ("promise?", b_promise_p),
        ("make-promise", b_make_promise),
        ("__make-pending-promise", b_make_pending_promise),
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
        ("hashtable-keys", b_hashtable_keys),
        ("hashtable-values", b_hashtable_values),
        ("hashtable-clear!", b_hashtable_clear),
        ("hashtable-copy", b_hashtable_copy),
        ("hashtable-mutable?", b_hashtable_mutable_p),
        ("hashtable-hash-function", b_hashtable_hash_function),
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
        ("exit", b_exit),
        ("emergency-exit", b_emergency_exit),
        ("symbol->string", b_symbol_to_string_ho),
        ("string->symbol", b_string_to_symbol_ho),
        ("hashtable-update!", b_hashtable_update_ho),
        ("hashtable-walk", b_hashtable_walk),
        ("hashtable-entries", b_hashtable_entries),
        ("hashtable-set!", b_hashtable_set),
        ("hashtable-ref", b_hashtable_ref),
        ("hashtable-contains?", b_hashtable_contains),
        ("hashtable-delete!", b_hashtable_delete),
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
        ("call-with-port", b_call_with_port),
        ("call-with-input-string", b_call_with_input_string),
        ("call-with-output-string", b_call_with_output_string),
        ("with-output-to-file", b_with_output_to_file),
        ("with-input-from-file", b_with_input_from_file),
        ("current-input-port", b_current_input_port),
        ("current-output-port", b_current_output_port),
        ("current-error-port", b_current_error_port),
        ("gensym", b_gensym),
        ("eval", b_eval),
        ("environment", b_environment),
        ("interaction-environment", b_interaction_environment),
        ("div-and-mod", b_div_and_mod),
        ("div0-and-mod0", b_div0_and_mod0),
        ("exact-integer-sqrt", b_exact_integer_sqrt),
        ("assoc", b_assoc),
        ("member", b_member),
        ("truncate/", b_truncate_div),
        ("floor/", b_floor_div),
        ("features", b_features),
        // vector higher-order
        ("vector-map", b_vector_map),
        ("vector-for-each", b_vector_for_each),
        // port-aware read
        ("read", b_read),
        ("read-char", b_read_char_ho),
        ("peek-char", b_peek_char_ho),
        ("read-string", b_read_string_ho),
        ("write-char", b_write_char_ho),
        ("write-string", b_write_string_ho),
        ("char-ready?", b_char_ready_p_ho),
        ("read-u8", b_read_u8_ho),
        ("peek-u8", b_peek_u8_ho),
        ("u8-ready?", b_u8_ready_p_ho),
        ("read-bytevector", b_read_bytevector_ho),
        ("write-u8", b_write_u8_ho),
        ("write-bytevector", b_write_bytevector_ho),
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

type SymsEntry = (
    &'static str,
    fn(&[Value], &mut SymbolTable) -> Result<Value, String>,
);

/// Builtins that need read-write access to the symbol table but not the
/// full EvalCtx (no application of user procedures, no port I/O). Both
/// the walker (`install_into`) and the VM tier register from this list.
pub fn syms_builtins() -> Vec<SymsEntry> {
    vec![
        // R6RS bytevector typed accessors with explicit endianness — need
        // to inspect a Symbol arg ('big | 'little) via the symbol table.
        ("bytevector-u16-ref", b_bytevector_u16_ref),
        ("bytevector-u16-set!", b_bytevector_u16_set),
        ("bytevector-s16-ref", b_bytevector_s16_ref),
        ("bytevector-s16-set!", b_bytevector_s16_set),
        ("bytevector-u32-ref", b_bytevector_u32_ref),
        ("bytevector-u32-set!", b_bytevector_u32_set),
        ("bytevector-s32-ref", b_bytevector_s32_ref),
        ("bytevector-s32-set!", b_bytevector_s32_set),
        ("bytevector-u64-ref", b_bytevector_u64_ref),
        ("bytevector-u64-set!", b_bytevector_u64_set),
        ("bytevector-s64-ref", b_bytevector_s64_ref),
        ("bytevector-s64-set!", b_bytevector_s64_set),
        ("bytevector-ieee-single-ref", b_bytevector_ieee_single_ref),
        ("bytevector-ieee-single-set!", b_bytevector_ieee_single_set),
        ("bytevector-ieee-double-ref", b_bytevector_ieee_double_ref),
        ("bytevector-ieee-double-set!", b_bytevector_ieee_double_set),
        ("native-endianness", b_native_endianness),
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
    for (name, f) in syms_builtins() {
        let sym = syms.intern(name);
        env.define(sym, crate::proc::make_builtin_syms(name, f));
    }
    // hashtable-equivalence-function returns a *tier-specific* procedure
    // (a walker Builtin here, a VmBuiltin on the VM tier in lib.rs), so
    // it can't share the pure_builtins registration loop.
    let heqf_sym = syms.intern("hashtable-equivalence-function");
    env.define(
        heqf_sym,
        make_builtin_pure(
            "hashtable-equivalence-function",
            b_hashtable_equivalence_function,
        ),
    );
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

fn as_integer_num(name: &str, v: &Value) -> Result<Number, String> {
    match v {
        Value::Number(n) if n.is_integer() => Ok(n.clone()),
        Value::Number(_) => Err(format!("{}: expected integer, got non-integer", name)),
        other => Err(type_err(name, "integer", other)),
    }
}

fn b_quotient(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("quotient", "2", args.len()));
    }
    let a = as_integer_num("quotient", &args[0])?;
    let b = as_integer_num("quotient", &args[1])?;
    a.quotient(&b)
        .map(Value::Number)
        .map_err(|_| "quotient: division by zero".into())
}

fn b_remainder(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("remainder", "2", args.len()));
    }
    let a = as_integer_num("remainder", &args[0])?;
    let b = as_integer_num("remainder", &args[1])?;
    a.remainder(&b)
        .map(Value::Number)
        .map_err(|_| "remainder: division by zero".into())
}

fn b_modulo(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("modulo", "2", args.len()));
    }
    let a = as_integer_num("modulo", &args[0])?;
    let b = as_integer_num("modulo", &args[1])?;
    a.modulo(&b)
        .map(Value::Number)
        .map_err(|_| "modulo: division by zero".into())
}

// ---- R6RS Euclidean division (div / mod / div-and-mod / div0 / mod0) ----
//
// `(div x y)` returns nd such that 0 ≤ x − y·nd < |y|. So `mod` is always
// non-negative regardless of the signs of x and y. `div0`/`mod0` use
// centered division: mod0 is in [−|y|/2, |y|/2). `*-and-*` versions
// return both via `(values d m)`.

/// Public so `Runtime::new` can plumb VM-tier shims that need to
/// compute Euclidean div/mod and stash the (d, m) pair via the VM's
/// pending-values channel. Returns `(d, m)` as Numbers so bignum
/// operands flow through cleanly.
pub fn div_and_mod_num(x: &Value, y: &Value) -> Result<(Value, Value), String> {
    let xi = as_integer_num("div-and-mod", x)?;
    let yi = as_integer_num("div-and-mod", y)?;
    let d = xi
        .euclid_div(&yi)
        .map_err(|_| "div-and-mod: division by zero".to_string())?;
    let m = xi
        .euclid_mod(&yi)
        .map_err(|_| "div-and-mod: division by zero".to_string())?;
    Ok((Value::Number(d), Value::Number(m)))
}

pub fn div0_and_mod0_num(x: &Value, y: &Value) -> Result<(Value, Value), String> {
    let xi = as_integer_num("div0-and-mod0", x)?;
    let yi = as_integer_num("div0-and-mod0", y)?;
    let d = xi
        .euclid_div0(&yi)
        .map_err(|_| "div0-and-mod0: division by zero".to_string())?;
    let m = xi
        .euclid_mod0(&yi)
        .map_err(|_| "div0-and-mod0: division by zero".to_string())?;
    Ok((Value::Number(d), Value::Number(m)))
}

fn b_div_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div", "2", args.len()));
    }
    let x = as_integer_num("div", &args[0])?;
    let y = as_integer_num("div", &args[1])?;
    x.euclid_div(&y)
        .map(Value::Number)
        .map_err(|_| "div: division by zero".into())
}

fn b_mod_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("mod", "2", args.len()));
    }
    let x = as_integer_num("mod", &args[0])?;
    let y = as_integer_num("mod", &args[1])?;
    x.euclid_mod(&y)
        .map(Value::Number)
        .map_err(|_| "mod: division by zero".into())
}

fn b_div0_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div0", "2", args.len()));
    }
    let x = as_integer_num("div0", &args[0])?;
    let y = as_integer_num("div0", &args[1])?;
    x.euclid_div0(&y)
        .map(Value::Number)
        .map_err(|_| "div0: division by zero".into())
}

fn b_mod0_op(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("mod0", "2", args.len()));
    }
    let x = as_integer_num("mod0", &args[0])?;
    let y = as_integer_num("mod0", &args[1])?;
    x.euclid_mod0(&y)
        .map(Value::Number)
        .map_err(|_| "mod0: division by zero".into())
}

fn b_div_and_mod(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div-and-mod", "2", args.len()));
    }
    let x = as_integer_num("div-and-mod", &args[0])?;
    let y = as_integer_num("div-and-mod", &args[1])?;
    let d = x
        .euclid_div(&y)
        .map_err(|_| "div-and-mod: division by zero".to_string())?;
    let m = x
        .euclid_mod(&y)
        .map_err(|_| "div-and-mod: division by zero".to_string())?;
    ctx.pending_values = Some(vec![Value::Number(d), Value::Number(m)]);
    Ok(Value::Unspecified)
}

fn b_div0_and_mod0(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("div0-and-mod0", "2", args.len()));
    }
    let (d, m) = div0_and_mod0_num(&args[0], &args[1])?;
    ctx.pending_values = Some(vec![d, m]);
    Ok(Value::Unspecified)
}

// =====================================================================
// R7RS division aliases. R7RS standardizes both truncated (R5RS) and
// floored division families, plus combined `truncate/` and `floor/`
// that return both quotient and remainder via multiple values.

fn b_truncate_quotient(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("truncate-quotient", "2", args.len()));
    }
    let x = as_integer_num("truncate-quotient", &args[0])?;
    let y = as_integer_num("truncate-quotient", &args[1])?;
    x.quotient(&y)
        .map(Value::Number)
        .map_err(|_| "truncate-quotient: division by zero".into())
}

fn b_truncate_remainder(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("truncate-remainder", "2", args.len()));
    }
    let x = as_integer_num("truncate-remainder", &args[0])?;
    let y = as_integer_num("truncate-remainder", &args[1])?;
    x.remainder(&y)
        .map(Value::Number)
        .map_err(|_| "truncate-remainder: division by zero".into())
}

fn b_floor_quotient(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("floor-quotient", "2", args.len()));
    }
    let x = as_integer_num("floor-quotient", &args[0])?;
    let y = as_integer_num("floor-quotient", &args[1])?;
    x.floor_quotient(&y)
        .map(Value::Number)
        .map_err(|_| "floor-quotient: division by zero".into())
}

fn b_floor_remainder(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("floor-remainder", "2", args.len()));
    }
    let x = as_integer_num("floor-remainder", &args[0])?;
    let y = as_integer_num("floor-remainder", &args[1])?;
    x.modulo(&y)
        .map(Value::Number)
        .map_err(|_| "floor-remainder: division by zero".into())
}

fn b_truncate_div(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("truncate/", "2", args.len()));
    }
    let x = as_integer_num("truncate/", &args[0])?;
    let y = as_integer_num("truncate/", &args[1])?;
    let q = x
        .quotient(&y)
        .map_err(|_| "truncate/: division by zero".to_string())?;
    let r = x
        .remainder(&y)
        .map_err(|_| "truncate/: division by zero".to_string())?;
    ctx.pending_values = Some(vec![Value::Number(q), Value::Number(r)]);
    Ok(Value::Unspecified)
}

fn b_floor_div(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("floor/", "2", args.len()));
    }
    let x = as_integer_num("floor/", &args[0])?;
    let y = as_integer_num("floor/", &args[1])?;
    let q = x
        .floor_quotient(&y)
        .map_err(|_| "floor/: division by zero".to_string())?;
    let r = x
        .modulo(&y)
        .map_err(|_| "floor/: division by zero".to_string())?;
    ctx.pending_values = Some(vec![Value::Number(q), Value::Number(r)]);
    Ok(Value::Unspecified)
}

/// Public for VM-tier shim that mirrors div_and_mod_num.
pub fn truncate_div_num(x: &Value, y: &Value) -> Result<(Value, Value), String> {
    let xi = as_integer_num("truncate/", x)?;
    let yi = as_integer_num("truncate/", y)?;
    let q = xi
        .quotient(&yi)
        .map_err(|_| "truncate/: division by zero".to_string())?;
    let r = xi
        .remainder(&yi)
        .map_err(|_| "truncate/: division by zero".to_string())?;
    Ok((Value::Number(q), Value::Number(r)))
}

pub fn floor_div_num(x: &Value, y: &Value) -> Result<(Value, Value), String> {
    let xi = as_integer_num("floor/", x)?;
    let yi = as_integer_num("floor/", y)?;
    let q = xi
        .floor_quotient(&yi)
        .map_err(|_| "floor/: division by zero".to_string())?;
    let r = xi
        .modulo(&yi)
        .map_err(|_| "floor/: division by zero".to_string())?;
    Ok((Value::Number(q), Value::Number(r)))
}

// =====================================================================
// R6RS (rnrs arithmetic fixnums) — typed fixnum ops.
// Differ from generic + - * etc.: operands MUST be fixnums (not bignums,
// not flonums), and overflow raises &implementation-restriction rather
// than promoting to BigInt. We model the error as a string for now,
// which surfaces as a generic error condition.

fn as_fx(name: &str, v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(Number::Fixnum(n)) => Ok(*n),
        _ => Err(type_err(name, "fixnum", v)),
    }
}

fn fx_overflow(name: &str) -> String {
    format!("{}: fixnum overflow", name)
}

fn b_fx_add(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fx+", "2", args.len()));
    }
    let a = as_fx("fx+", &args[0])?;
    let b = as_fx("fx+", &args[1])?;
    a.checked_add(b)
        .map(Value::fixnum)
        .ok_or_else(|| fx_overflow("fx+"))
}

fn b_fx_sub(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("fx-", "1 or 2", args.len()));
    }
    if args.len() == 1 {
        let a = as_fx("fx-", &args[0])?;
        return a
            .checked_neg()
            .map(Value::fixnum)
            .ok_or_else(|| fx_overflow("fx-"));
    }
    let a = as_fx("fx-", &args[0])?;
    let b = as_fx("fx-", &args[1])?;
    a.checked_sub(b)
        .map(Value::fixnum)
        .ok_or_else(|| fx_overflow("fx-"))
}

fn b_fx_mul(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fx*", "2", args.len()));
    }
    let a = as_fx("fx*", &args[0])?;
    let b = as_fx("fx*", &args[1])?;
    a.checked_mul(b)
        .map(Value::fixnum)
        .ok_or_else(|| fx_overflow("fx*"))
}

fn b_fx_div(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxdiv", "2", args.len()));
    }
    let a = as_fx("fxdiv", &args[0])?;
    let b = as_fx("fxdiv", &args[1])?;
    if b == 0 {
        return Err("fxdiv: division by zero".into());
    }
    let xn = Number::Fixnum(a);
    let yn = Number::Fixnum(b);
    let r = xn
        .euclid_div(&yn)
        .map_err(|_| "fxdiv: division by zero".to_string())?;
    match r {
        Number::Fixnum(v) => Ok(Value::fixnum(v)),
        _ => Err(fx_overflow("fxdiv")),
    }
}

fn b_fx_mod(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxmod", "2", args.len()));
    }
    let a = as_fx("fxmod", &args[0])?;
    let b = as_fx("fxmod", &args[1])?;
    if b == 0 {
        return Err("fxmod: division by zero".into());
    }
    let xn = Number::Fixnum(a);
    let yn = Number::Fixnum(b);
    let r = xn
        .euclid_mod(&yn)
        .map_err(|_| "fxmod: division by zero".to_string())?;
    match r {
        Number::Fixnum(v) => Ok(Value::fixnum(v)),
        _ => Err(fx_overflow("fxmod")),
    }
}

fn b_fx_div0(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxdiv0", "2", args.len()));
    }
    let a = as_fx("fxdiv0", &args[0])?;
    let b = as_fx("fxdiv0", &args[1])?;
    if b == 0 {
        return Err("fxdiv0: division by zero".into());
    }
    let xn = Number::Fixnum(a);
    let yn = Number::Fixnum(b);
    let r = xn
        .euclid_div0(&yn)
        .map_err(|_| "fxdiv0: division by zero".to_string())?;
    match r {
        Number::Fixnum(v) => Ok(Value::fixnum(v)),
        _ => Err(fx_overflow("fxdiv0")),
    }
}

fn b_fx_mod0(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxmod0", "2", args.len()));
    }
    let a = as_fx("fxmod0", &args[0])?;
    let b = as_fx("fxmod0", &args[1])?;
    if b == 0 {
        return Err("fxmod0: division by zero".into());
    }
    let xn = Number::Fixnum(a);
    let yn = Number::Fixnum(b);
    let r = xn
        .euclid_mod0(&yn)
        .map_err(|_| "fxmod0: division by zero".to_string())?;
    match r {
        Number::Fixnum(v) => Ok(Value::fixnum(v)),
        _ => Err(fx_overflow("fxmod0")),
    }
}

fn fx_chain_pred(
    name: &str,
    args: &[Value],
    cmp: impl Fn(i64, i64) -> bool,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err(name, "at least 2", args.len()));
    }
    let mut prev = as_fx(name, &args[0])?;
    for a in &args[1..] {
        let cur = as_fx(name, a)?;
        if !cmp(prev, cur) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_fx_eq(args: &[Value]) -> Result<Value, String> {
    fx_chain_pred("fx=?", args, |a, b| a == b)
}
fn b_fx_lt(args: &[Value]) -> Result<Value, String> {
    fx_chain_pred("fx<?", args, |a, b| a < b)
}
fn b_fx_gt(args: &[Value]) -> Result<Value, String> {
    fx_chain_pred("fx>?", args, |a, b| a > b)
}
fn b_fx_le(args: &[Value]) -> Result<Value, String> {
    fx_chain_pred("fx<=?", args, |a, b| a <= b)
}
fn b_fx_ge(args: &[Value]) -> Result<Value, String> {
    fx_chain_pred("fx>=?", args, |a, b| a >= b)
}

fn fx_pred1(name: &str, args: &[Value], pred: impl Fn(i64) -> bool) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    Ok(Value::Boolean(pred(as_fx(name, &args[0])?)))
}

fn b_fx_zero(args: &[Value]) -> Result<Value, String> {
    fx_pred1("fxzero?", args, |x| x == 0)
}
fn b_fx_positive(args: &[Value]) -> Result<Value, String> {
    fx_pred1("fxpositive?", args, |x| x > 0)
}
fn b_fx_negative(args: &[Value]) -> Result<Value, String> {
    fx_pred1("fxnegative?", args, |x| x < 0)
}
fn b_fx_odd(args: &[Value]) -> Result<Value, String> {
    fx_pred1("fxodd?", args, |x| x.rem_euclid(2) != 0)
}
fn b_fx_even(args: &[Value]) -> Result<Value, String> {
    fx_pred1("fxeven?", args, |x| x.rem_euclid(2) == 0)
}

fn b_fx_max(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("fxmax", "at least 1", 0));
    }
    let mut acc = as_fx("fxmax", &args[0])?;
    for a in &args[1..] {
        let v = as_fx("fxmax", a)?;
        if v > acc {
            acc = v;
        }
    }
    Ok(Value::fixnum(acc))
}

fn b_fx_min(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("fxmin", "at least 1", 0));
    }
    let mut acc = as_fx("fxmin", &args[0])?;
    for a in &args[1..] {
        let v = as_fx("fxmin", a)?;
        if v < acc {
            acc = v;
        }
    }
    Ok(Value::fixnum(acc))
}

fn b_fx_not(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("fxnot", "1", args.len()));
    }
    Ok(Value::fixnum(!as_fx("fxnot", &args[0])?))
}

fn fx_fold_bits(
    name: &str,
    args: &[Value],
    ident: i64,
    op: impl Fn(i64, i64) -> i64,
) -> Result<Value, String> {
    let mut acc = ident;
    for a in args {
        acc = op(acc, as_fx(name, a)?);
    }
    Ok(Value::fixnum(acc))
}

fn b_fx_and(args: &[Value]) -> Result<Value, String> {
    fx_fold_bits("fxand", args, -1, |a, b| a & b)
}
fn b_fx_ior(args: &[Value]) -> Result<Value, String> {
    fx_fold_bits("fxior", args, 0, |a, b| a | b)
}
fn b_fx_xor(args: &[Value]) -> Result<Value, String> {
    fx_fold_bits("fxxor", args, 0, |a, b| a ^ b)
}

fn b_fx_arith_shift(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxarithmetic-shift", "2", args.len()));
    }
    let x = as_fx("fxarithmetic-shift", &args[0])?;
    let k = as_fx("fxarithmetic-shift", &args[1])?;
    if k >= 64 || k <= -64 {
        return Err(fx_overflow("fxarithmetic-shift"));
    }
    let r = if k >= 0 { x << k } else { x >> (-k) };
    Ok(Value::fixnum(r))
}

fn b_fx_arith_shift_left(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxarithmetic-shift-left", "2", args.len()));
    }
    let x = as_fx("fxarithmetic-shift-left", &args[0])?;
    let k = as_fx("fxarithmetic-shift-left", &args[1])?;
    if !(0..64).contains(&k) {
        return Err(fx_overflow("fxarithmetic-shift-left"));
    }
    Ok(Value::fixnum(x << k))
}

fn b_fx_arith_shift_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxarithmetic-shift-right", "2", args.len()));
    }
    let x = as_fx("fxarithmetic-shift-right", &args[0])?;
    let k = as_fx("fxarithmetic-shift-right", &args[1])?;
    if !(0..64).contains(&k) {
        return Err(fx_overflow("fxarithmetic-shift-right"));
    }
    Ok(Value::fixnum(x >> k))
}

// =====================================================================
// R6RS (rnrs arithmetic flonums) — typed flonum ops.
// Operands MUST be flonums (no fixnum/big/rational); pure IEEE-754
// semantics (no exact contagion, NaN/inf propagate naturally).

fn as_fl(name: &str, v: &Value) -> Result<f64, String> {
    match v {
        Value::Number(Number::Flonum(f)) => Ok(*f),
        _ => Err(type_err(name, "flonum", v)),
    }
}

fn b_fl_add(args: &[Value]) -> Result<Value, String> {
    let mut acc = 0.0f64;
    for a in args {
        acc += as_fl("fl+", a)?;
    }
    Ok(Value::Number(Number::Flonum(acc)))
}

fn b_fl_sub(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("fl-", "at least 1", 0));
    }
    if args.len() == 1 {
        return Ok(Value::Number(Number::Flonum(-as_fl("fl-", &args[0])?)));
    }
    let mut acc = as_fl("fl-", &args[0])?;
    for a in &args[1..] {
        acc -= as_fl("fl-", a)?;
    }
    Ok(Value::Number(Number::Flonum(acc)))
}

fn b_fl_mul(args: &[Value]) -> Result<Value, String> {
    let mut acc = 1.0f64;
    for a in args {
        acc *= as_fl("fl*", a)?;
    }
    Ok(Value::Number(Number::Flonum(acc)))
}

fn b_fl_div(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("fl/", "at least 1", 0));
    }
    if args.len() == 1 {
        return Ok(Value::Number(Number::Flonum(1.0 / as_fl("fl/", &args[0])?)));
    }
    let mut acc = as_fl("fl/", &args[0])?;
    for a in &args[1..] {
        acc /= as_fl("fl/", a)?;
    }
    Ok(Value::Number(Number::Flonum(acc)))
}

fn fl_chain_pred(
    name: &str,
    args: &[Value],
    cmp: impl Fn(f64, f64) -> bool,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err(name, "at least 2", args.len()));
    }
    let mut prev = as_fl(name, &args[0])?;
    for a in &args[1..] {
        let cur = as_fl(name, a)?;
        if !cmp(prev, cur) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_fl_eq(args: &[Value]) -> Result<Value, String> {
    fl_chain_pred("fl=?", args, |a, b| a == b)
}
fn b_fl_lt(args: &[Value]) -> Result<Value, String> {
    fl_chain_pred("fl<?", args, |a, b| a < b)
}
fn b_fl_gt(args: &[Value]) -> Result<Value, String> {
    fl_chain_pred("fl>?", args, |a, b| a > b)
}
fn b_fl_le(args: &[Value]) -> Result<Value, String> {
    fl_chain_pred("fl<=?", args, |a, b| a <= b)
}
fn b_fl_ge(args: &[Value]) -> Result<Value, String> {
    fl_chain_pred("fl>=?", args, |a, b| a >= b)
}

fn fl_pred1(name: &str, args: &[Value], pred: impl Fn(f64) -> bool) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    Ok(Value::Boolean(pred(as_fl(name, &args[0])?)))
}

fn b_fl_zero(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flzero?", args, |x| x == 0.0)
}
fn b_fl_positive(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flpositive?", args, |x| x > 0.0)
}
fn b_fl_negative(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flnegative?", args, |x| x < 0.0)
}
fn b_fl_nan(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flnan?", args, f64::is_nan)
}
fn b_fl_finite(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flfinite?", args, f64::is_finite)
}
fn b_fl_infinite(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flinfinite?", args, f64::is_infinite)
}
fn b_fl_integer(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flinteger?", args, |x| x.is_finite() && x.fract() == 0.0)
}
fn b_fl_even(args: &[Value]) -> Result<Value, String> {
    fl_pred1("fleven?", args, |x| {
        x.is_finite() && x.fract() == 0.0 && (x as i64).rem_euclid(2) == 0
    })
}
fn b_fl_odd(args: &[Value]) -> Result<Value, String> {
    fl_pred1("flodd?", args, |x| {
        x.is_finite() && x.fract() == 0.0 && (x as i64).rem_euclid(2) != 0
    })
}

fn b_fl_max(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("flmax", "at least 1", 0));
    }
    let mut acc = as_fl("flmax", &args[0])?;
    for a in &args[1..] {
        let v = as_fl("flmax", a)?;
        acc = acc.max(v);
    }
    Ok(Value::Number(Number::Flonum(acc)))
}

fn b_fl_min(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("flmin", "at least 1", 0));
    }
    let mut acc = as_fl("flmin", &args[0])?;
    for a in &args[1..] {
        let v = as_fl("flmin", a)?;
        acc = acc.min(v);
    }
    Ok(Value::Number(Number::Flonum(acc)))
}

fn fl_unary(name: &str, args: &[Value], op: impl Fn(f64) -> f64) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    Ok(Value::Number(Number::Flonum(op(as_fl(name, &args[0])?))))
}

fn b_fl_abs(args: &[Value]) -> Result<Value, String> {
    fl_unary("flabs", args, f64::abs)
}
fn b_fl_floor(args: &[Value]) -> Result<Value, String> {
    fl_unary("flfloor", args, f64::floor)
}
fn b_fl_ceiling(args: &[Value]) -> Result<Value, String> {
    fl_unary("flceiling", args, f64::ceil)
}
fn b_fl_truncate(args: &[Value]) -> Result<Value, String> {
    fl_unary("fltruncate", args, f64::trunc)
}
fn b_fl_round(args: &[Value]) -> Result<Value, String> {
    // R6RS round: banker's rounding (half to even) — match the generic
    // round builtin's semantics.
    fl_unary("flround", args, |x| {
        let r = x.round();
        // f64::round is half-away-from-zero; convert to banker's.
        if (x - x.trunc()).abs() == 0.5 {
            let t = x.trunc();
            if (t as i64).rem_euclid(2) == 0 {
                t
            } else {
                r
            }
        } else {
            r
        }
    })
}
fn b_fl_sqrt(args: &[Value]) -> Result<Value, String> {
    fl_unary("flsqrt", args, f64::sqrt)
}
fn b_fl_exp(args: &[Value]) -> Result<Value, String> {
    fl_unary("flexp", args, f64::exp)
}
fn b_fl_log(args: &[Value]) -> Result<Value, String> {
    if args.len() == 1 {
        return fl_unary("fllog", args, f64::ln);
    }
    if args.len() == 2 {
        let x = as_fl("fllog", &args[0])?;
        let base = as_fl("fllog", &args[1])?;
        return Ok(Value::Number(Number::Flonum(x.log(base))));
    }
    Err(arity_err("fllog", "1 or 2", args.len()))
}
fn b_fl_sin(args: &[Value]) -> Result<Value, String> {
    fl_unary("flsin", args, f64::sin)
}
fn b_fl_cos(args: &[Value]) -> Result<Value, String> {
    fl_unary("flcos", args, f64::cos)
}
fn b_fl_tan(args: &[Value]) -> Result<Value, String> {
    fl_unary("fltan", args, f64::tan)
}

fn b_fixnum_to_flonum(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("fixnum->flonum", "1", args.len()));
    }
    let n = as_fx("fixnum->flonum", &args[0])?;
    Ok(Value::Number(Number::Flonum(n as f64)))
}

fn b_expt(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("expt", "2", args.len()));
    }
    let base = as_num("expt", &args[0])?;
    let exp = as_num("expt", &args[1])?;
    // Exact integer base + non-negative integer exponent: stay exact.
    // Number::mul promotes Fixnum overflow to Big, so the loop stays
    // correct beyond i64. Cap the exponent at 1<<20 to avoid runaway
    // memory use on pathological inputs (`(expt 2 (expt 2 30))`).
    if base.is_integer() && exp.is_integer() {
        if let Number::Fixnum(e) = exp {
            if e >= 0 && e <= (1 << 20) {
                // Repeated-squaring keeps the loop log(e) instead of
                // linear in e — important since `expt` on big exponents
                // is the canonical path for building bignums.
                let mut acc = Number::Fixnum(1);
                let mut b = base.clone();
                let mut k = e;
                while k > 0 {
                    if k & 1 == 1 {
                        acc = acc.mul(&b);
                    }
                    k >>= 1;
                    if k > 0 {
                        b = b.mul(&b);
                    }
                }
                return Ok(Value::Number(acc));
            }
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

/// `(fixnum? v)` — true iff v is an exact integer that fits in i64.
/// R6RS-style. Bignums and rationals/flonums all return #f.
fn b_fixnum_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("fixnum?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        &args[0],
        Value::Number(Number::Fixnum(_))
    )))
}

/// `(flonum? v)` — true iff v is an inexact real (Number::Flonum). R6RS.
fn b_flonum_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("flonum?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        &args[0],
        Value::Number(Number::Flonum(_))
    )))
}

/// `(rational? v)` — true for any number except non-finite flonums.
/// All exact integers and exact rationals qualify; finite flonums are
/// also rational per R6RS (every finite real is exactly representable
/// as a ratio in principle).
fn b_rational_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("rational?", "1", args.len()));
    }
    Ok(Value::Boolean(match &args[0] {
        Value::Number(Number::Flonum(f)) => f.is_finite(),
        Value::Number(_) => true, // Fixnum / Big / Rational are all rational
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

/// `list?` (R7RS): true iff the argument is a proper list (terminates
/// in '()). Walks the spine; returns #f on any improper tail and on
/// any infinite cycle (detected via Floyd's tortoise/hare).
fn b_list_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("list?", "1", args.len()));
    }
    fn step(v: &Value) -> Option<Value> {
        match v {
            Value::Pair(p) => Some(p.cdr.borrow().clone()),
            _ => None,
        }
    }
    let mut slow = args[0].clone();
    let mut fast = args[0].clone();
    loop {
        match &fast {
            Value::Null => return Ok(Value::Boolean(true)),
            Value::Pair(_) => {}
            _ => return Ok(Value::Boolean(false)),
        }
        let f1 = step(&fast).unwrap();
        match &f1 {
            Value::Null => return Ok(Value::Boolean(true)),
            Value::Pair(_) => {}
            _ => return Ok(Value::Boolean(false)),
        }
        let f2 = step(&f1).unwrap();
        let s1 = step(&slow).unwrap();
        if let (Value::Pair(s), Value::Pair(f)) = (&s1, &f2) {
            if cs_core::Gc::ptr_eq(s, f) {
                return Ok(Value::Boolean(false));
            }
        }
        slow = s1;
        fast = f2;
    }
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

/// R7RS `(list-set! list k obj)` — destructively replace the element at
/// index k. Walks k cdrs and uses set-car! on the resulting pair.
fn b_list_set_bang(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("list-set!", "3", args.len()));
    }
    let tail = b_list_tail(&args[..2])?;
    match tail {
        Value::Pair(p) => {
            *p.car.borrow_mut() = args[2].clone();
            Ok(Value::Unspecified)
        }
        _ => Err("list-set!: index out of range".into()),
    }
}

/// R7RS `(boolean=? bool1 bool2 bool3 ...)` — true iff all booleans are
/// the same. Requires at least two args; all must be booleans.
fn b_boolean_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("boolean=?", "2 or more", args.len()));
    }
    let first = match &args[0] {
        Value::Boolean(b) => *b,
        v => return Err(type_err("boolean=?", "boolean", v)),
    };
    for v in &args[1..] {
        match v {
            Value::Boolean(b) => {
                if *b != first {
                    return Ok(Value::Boolean(false));
                }
            }
            other => return Err(type_err("boolean=?", "boolean", other)),
        }
    }
    Ok(Value::Boolean(true))
}

/// R7RS `(symbol=? sym1 sym2 sym3 ...)` — true iff all symbols are the
/// same (uses Symbol identity, which equals interned-name equality).
fn b_symbol_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(arity_err("symbol=?", "2 or more", args.len()));
    }
    let first = match &args[0] {
        Value::Symbol(s) => *s,
        v => return Err(type_err("symbol=?", "symbol", v)),
    };
    for v in &args[1..] {
        match v {
            Value::Symbol(s) => {
                if *s != first {
                    return Ok(Value::Boolean(false));
                }
            }
            other => return Err(type_err("symbol=?", "symbol", other)),
        }
    }
    Ok(Value::Boolean(true))
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

/// Case-insensitive helper: lowercase a single char. Matches char-foldcase
/// behavior for the foundation milestone (Unicode-aware via Rust's
/// to_lowercase iterator).
fn ci_char(c: char) -> String {
    c.to_lowercase().collect()
}

fn ci_string(s: &str) -> String {
    s.chars().flat_map(|c| c.to_lowercase()).collect()
}

fn string_ci_chain(
    name: &str,
    args: &[Value],
    pred: impl Fn(std::cmp::Ordering) -> bool,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::String(s) => ci_string(&s.borrow()),
        v => return Err(type_err(name, "string", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::String(s) => ci_string(&s.borrow()),
            v => return Err(type_err(name, "string", v)),
        };
        if !pred(prev.as_str().cmp(cur.as_str())) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_string_ci_eq(args: &[Value]) -> Result<Value, String> {
    string_ci_chain("string-ci=?", args, |o| o == std::cmp::Ordering::Equal)
}
fn b_string_ci_lt(args: &[Value]) -> Result<Value, String> {
    string_ci_chain("string-ci<?", args, |o| o == std::cmp::Ordering::Less)
}
fn b_string_ci_le(args: &[Value]) -> Result<Value, String> {
    string_ci_chain("string-ci<=?", args, |o| o != std::cmp::Ordering::Greater)
}
fn b_string_ci_gt(args: &[Value]) -> Result<Value, String> {
    string_ci_chain("string-ci>?", args, |o| o == std::cmp::Ordering::Greater)
}
fn b_string_ci_ge(args: &[Value]) -> Result<Value, String> {
    string_ci_chain("string-ci>=?", args, |o| o != std::cmp::Ordering::Less)
}

fn char_ci_chain(
    name: &str,
    args: &[Value],
    pred: impl Fn(std::cmp::Ordering) -> bool,
) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::Character(c) => ci_char(*c),
        v => return Err(type_err(name, "character", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::Character(c) => ci_char(*c),
            v => return Err(type_err(name, "character", v)),
        };
        if !pred(prev.as_str().cmp(cur.as_str())) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_char_ci_eq(args: &[Value]) -> Result<Value, String> {
    char_ci_chain("char-ci=?", args, |o| o == std::cmp::Ordering::Equal)
}
fn b_char_ci_lt(args: &[Value]) -> Result<Value, String> {
    char_ci_chain("char-ci<?", args, |o| o == std::cmp::Ordering::Less)
}
fn b_char_ci_le(args: &[Value]) -> Result<Value, String> {
    char_ci_chain("char-ci<=?", args, |o| o != std::cmp::Ordering::Greater)
}
fn b_char_ci_gt(args: &[Value]) -> Result<Value, String> {
    char_ci_chain("char-ci>?", args, |o| o == std::cmp::Ordering::Greater)
}
fn b_char_ci_ge(args: &[Value]) -> Result<Value, String> {
    char_ci_chain("char-ci>=?", args, |o| o != std::cmp::Ordering::Less)
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

/// R7RS `(string-set! str k char)` — destructively replace the kth char.
/// Indices are character (not byte) positions.
fn b_string_set_bang(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("string-set!", "3", args.len()));
    }
    let i = as_int_i64("string-set!", &args[1])?;
    if i < 0 {
        return Err("string-set!: negative index".into());
    }
    let new_ch = match &args[2] {
        Value::Character(c) => *c,
        v => return Err(type_err("string-set!", "character", v)),
    };
    match &args[0] {
        Value::String(s) => {
            let mut chars: Vec<char> = s.borrow().chars().collect();
            if (i as usize) >= chars.len() {
                return Err("string-set!: index out of range".into());
            }
            chars[i as usize] = new_ch;
            *s.borrow_mut() = chars.into_iter().collect();
            Ok(Value::Unspecified)
        }
        v => Err(type_err("string-set!", "string", v)),
    }
}

fn b_string_to_list(args: &[Value]) -> Result<Value, String> {
    // R7RS: (string->list s [start [end]]). Indices are character (not byte)
    // positions so multibyte UTF-8 strings work correctly.
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("string->list", "1..3", args.len()));
    }
    let chars: Vec<char> = match &args[0] {
        Value::String(s) => s.borrow().chars().collect(),
        v => return Err(type_err("string->list", "string", v)),
    };
    let len = chars.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("string->list", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("string->list: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("string->list", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("string->list: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    Ok(Value::list(
        chars[start..end].iter().copied().map(Value::Character),
    ))
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
    let n = as_num("exact", &args[0])?;
    n.to_exact()
        .map(Value::Number)
        .ok_or_else(|| "exact: non-finite flonum has no exact representation".into())
}

fn b_numerator(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("numerator", "1", args.len()));
    }
    let n = as_num("numerator", &args[0])?;
    n.numerator()
        .map(Value::Number)
        .ok_or_else(|| "numerator: non-finite flonum has no numerator".into())
}

fn b_denominator(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("denominator", "1", args.len()));
    }
    let n = as_num("denominator", &args[0])?;
    n.denominator()
        .map(Value::Number)
        .ok_or_else(|| "denominator: non-finite flonum has no denominator".into())
}

fn b_nan_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("nan?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        &args[0],
        Value::Number(Number::Flonum(f)) if f.is_nan()
    )))
}

fn b_finite_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("finite?", "1", args.len()));
    }
    let r = match &args[0] {
        Value::Number(Number::Flonum(f)) => f.is_finite(),
        Value::Number(_) => true,
        v => return Err(type_err("finite?", "number", v)),
    };
    Ok(Value::Boolean(r))
}

fn b_infinite_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("infinite?", "1", args.len()));
    }
    let r = match &args[0] {
        Value::Number(Number::Flonum(f)) => f.is_infinite(),
        Value::Number(_) => false,
        v => return Err(type_err("infinite?", "number", v)),
    };
    Ok(Value::Boolean(r))
}

fn b_exact_integer_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("exact-integer?", "1", args.len()));
    }
    let r = matches!(
        &args[0],
        Value::Number(Number::Fixnum(_)) | Value::Number(Number::Big(_))
    ) || matches!(&args[0], Value::Number(Number::Rat(r)) if r.is_integer());
    Ok(Value::Boolean(r))
}

fn b_exact_nonneg_int_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("exact-nonnegative-integer?", "1", args.len()));
    }
    use num_traits::Signed;
    let r = match &args[0] {
        Value::Number(Number::Fixnum(v)) => *v >= 0,
        Value::Number(Number::Big(b)) => !b.is_negative(),
        Value::Number(Number::Rat(r)) => r.is_integer() && !r.numer().is_negative(),
        _ => false,
    };
    Ok(Value::Boolean(r))
}

fn b_exact_rational_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("exact-rational?", "1", args.len()));
    }
    let r = matches!(
        &args[0],
        Value::Number(Number::Fixnum(_))
            | Value::Number(Number::Big(_))
            | Value::Number(Number::Rat(_))
    );
    Ok(Value::Boolean(r))
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
    // R7RS: (string-copy s [start [end]]). Indices are character (not byte)
    // positions so multibyte UTF-8 strings work correctly.
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("string-copy", "1..3", args.len()));
    }
    let chars: Vec<char> = match &args[0] {
        Value::String(s) => s.borrow().chars().collect(),
        v => return Err(type_err("string-copy", "string", v)),
    };
    let len = chars.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("string-copy", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("string-copy: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("string-copy", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("string-copy: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    Ok(Value::string(chars[start..end].iter().collect::<String>()))
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
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(v))))
}

fn b_vector(args: &[Value]) -> Result<Value, String> {
    let v: Vec<Value> = args.to_vec();
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(v))))
}

/// R7RS `(string char ...)` — return a string composed of the given chars.
/// Variadic, including 0 args (empty string). Errors if any arg isn't a
/// character.
fn b_string_ctor(args: &[Value]) -> Result<Value, String> {
    let mut s = String::new();
    for a in args {
        match a {
            Value::Character(c) => s.push(*c),
            v => return Err(type_err("string", "character", v)),
        }
    }
    Ok(Value::string(s))
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
    // R7RS: (vector-fill! v fill [start [end]])
    if args.len() < 2 || args.len() > 4 {
        return Err(arity_err("vector-fill!", "2..4", args.len()));
    }
    match &args[0] {
        Value::Vector(v) => {
            let mut v = v.borrow_mut();
            let len = v.len();
            let start = if args.len() >= 3 {
                let i = as_int_i64("vector-fill!", &args[2])?;
                if i < 0 || (i as usize) > len {
                    return Err(format!("vector-fill!: start out of range: {}", i));
                }
                i as usize
            } else {
                0
            };
            let end = if args.len() == 4 {
                let i = as_int_i64("vector-fill!", &args[3])?;
                if i < 0 || (i as usize) > len || (i as usize) < start {
                    return Err(format!("vector-fill!: end out of range: {}", i));
                }
                i as usize
            } else {
                len
            };
            for slot in &mut v[start..end] {
                *slot = args[1].clone();
            }
            Ok(Value::Unspecified)
        }
        v => Err(type_err("vector-fill!", "vector", v)),
    }
}

fn b_vector_to_list(args: &[Value]) -> Result<Value, String> {
    // R7RS: (vector->list v [start [end]]).
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("vector->list", "1..3", args.len()));
    }
    let items: Vec<Value> = match &args[0] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector->list", "vector", v)),
    };
    let len = items.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("vector->list", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("vector->list: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("vector->list", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("vector->list: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    Ok(Value::list(items[start..end].iter().cloned()))
}

fn b_list_to_vector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("list->vector", "1", args.len()));
    }
    let items = collect_proper_list("list->vector", &args[0])?;
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
        items,
    ))))
}

// ---- assoc / member family ----

fn b_assoc(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // R7RS: (assoc obj alist [compare]); the 2-arg form uses equal?,
    // the 3-arg form applies the supplied comparison procedure.
    // (R6RS only specifies the 2-arg form.) `obj` is args[0]; the
    // alist is args[1].
    match args.len() {
        2 => assoc_search("assoc", &args[0], &args[1], eq::equal),
        3 => {
            let cmp = args[2].clone();
            assoc_search_with("assoc", &args[0], &args[1], ctx, &cmp)
        }
        n => Err(arity_err("assoc", "2 or 3", n)),
    }
}

/// Like `assoc_search` but applies a user-supplied comparison
/// procedure on each step. Errors propagate as builtin errors.
fn assoc_search_with(
    name: &str,
    key: &Value,
    list: &Value,
    ctx: &mut EvalCtx,
    cmp: &Value,
) -> Result<Value, String> {
    let mut cur = list.clone();
    loop {
        match cur {
            Value::Null => return Ok(Value::Boolean(false)),
            Value::Pair(p) => {
                let head = p.car.borrow().clone();
                match &head {
                    Value::Pair(pair) => {
                        let car = pair.car.borrow().clone();
                        let r = apply_procedure(cmp, &[car, key.clone()], ctx)
                            .map_err(|d| format!("{:?}", d))?;
                        if r.is_truthy() {
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

fn b_member(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // R7RS: (member obj list [compare]). 2-arg uses equal?; 3-arg
    // uses the supplied comparison procedure.
    match args.len() {
        2 => member_search("member", &args[0], &args[1], eq::equal),
        3 => {
            let cmp = args[2].clone();
            member_search_with("member", &args[0], &args[1], ctx, &cmp)
        }
        n => Err(arity_err("member", "2 or 3", n)),
    }
}

fn member_search_with(
    name: &str,
    obj: &Value,
    list: &Value,
    ctx: &mut EvalCtx,
    cmp: &Value,
) -> Result<Value, String> {
    let mut cur = list.clone();
    loop {
        match cur {
            Value::Null => return Ok(Value::Boolean(false)),
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                let r = apply_procedure(cmp, &[car, obj.clone()], ctx)
                    .map_err(|d| format!("{:?}", d))?;
                if r.is_truthy() {
                    return Ok(Value::Pair(p.clone()));
                }
                cur = p.cdr.borrow().clone();
            }
            other => return Err(type_err(name, "proper list", &other)),
        }
    }
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
const TAG_FILE_ERROR: &str = "&file-error";
const TAG_READ_ERROR: &str = "&read-error";
const TAG_EXIT_REQUESTED: &str = "&exit-requested";

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
        m.insert(TAG_FILE_ERROR.into(), TAG_ERROR.into());
        m.insert(TAG_READ_ERROR.into(), TAG_ERROR.into());
        m.insert(TAG_EXIT_REQUESTED.into(), TAG_CONDITION.into());
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
            TAG_FILE_ERROR,
            TAG_READ_ERROR,
            TAG_EXIT_REQUESTED,
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

/// Append an empty-fielded simple of the given tag to an existing compound
/// condition and return the updated value. Used to mark a condition with
/// `&file-error` / `&read-error` after the base condition has been built.
pub fn add_simple_to_compound(cond: Value, tag: &str) -> Value {
    if !is_compound_cond(&cond) {
        return cond;
    }
    if let Value::Vector(vc) = &cond {
        let mut items = vc.borrow().clone();
        items.push(make_simple(tag, vec![]));
        return new_vector(items);
    }
    cond
}

fn new_vector(items: Vec<Value>) -> Value {
    Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
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

/// R7RS `(exit)` and `(exit obj)` — terminate the program normally,
/// communicating obj as the exit value. We implement this as raising a
/// catchable `&exit-requested` condition; the value is stored as a single
/// field on the simple. The CLI's top-level recognizes this tag and maps
/// it to a process exit code; embedded use can catch it via
/// with-exception-handler.
fn b_exit(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("exit", "0 or 1", args.len()));
    }
    let val = args.first().cloned().unwrap_or(Value::Boolean(true));
    let cond = make_compound(vec![
        make_simple(TAG_EXIT_REQUESTED, vec![val]),
        // Empty &message simple so error-object-message returns "" instead
        // of erroring out — keeps catchers that introspect via
        // error-object-message uniform.
        make_simple(TAG_MESSAGE, vec![Value::string("")]),
    ]);
    ctx.pending_raise = Some(cond);
    Err("__raised__".to_string())
}

/// R7RS `(emergency-exit)` and `(emergency-exit obj)` — terminate without
/// running any pending dynamic-wind after-procedures. Same condition shape
/// as `exit`, with an extra `&emergency` marker so a top-level catcher can
/// distinguish the two and skip cleanup.
fn b_emergency_exit(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("emergency-exit", "0 or 1", args.len()));
    }
    let val = args.first().cloned().unwrap_or(Value::Boolean(true));
    let cond = make_compound(vec![
        make_simple(TAG_EXIT_REQUESTED, vec![val]),
        make_simple("&emergency", vec![]),
        make_simple(TAG_MESSAGE, vec![Value::string("")]),
    ]);
    ctx.pending_raise = Some(cond);
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

/// `(char-foldcase c)` — case-folding for case-insensitive comparison.
/// For ASCII this matches `char-downcase`; full Unicode folding (e.g.
/// ß → ss, which produces multiple chars) is not yet implemented.
fn b_char_foldcase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char-foldcase", "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => {
            let folded = c.to_lowercase().next().unwrap_or(*c);
            Ok(Value::Character(folded))
        }
        v => Err(type_err("char-foldcase", "character", v)),
    }
}

/// `(char-titlecase c)` — uppercase the character. Title-case differs
/// from upper-case for a few Unicode chars (e.g. ǳ vs Ǳ vs ǲ); for
/// now we approximate with uppercase.
fn b_char_titlecase(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char-titlecase", "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => {
            let up = c.to_uppercase().next().unwrap_or(*c);
            Ok(Value::Character(up))
        }
        v => Err(type_err("char-titlecase", "character", v)),
    }
}

/// `(digit-value c)` — for a numeric digit character, returns the
/// integer it represents; for non-digits returns #f. R6RS spec.
fn b_digit_value(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("digit-value", "1", args.len()));
    }
    match &args[0] {
        Value::Character(c) => match c.to_digit(10) {
            Some(d) => Ok(Value::fixnum(d as i64)),
            None => Ok(Value::Boolean(false)),
        },
        v => Err(type_err("digit-value", "character", v)),
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
    // R6RS `(make-hashtable)` defaults to `equal?`. Two-arg form
    // `(make-hashtable hash equiv)` stores user procedures and routes
    // through them on every key comparison; the storage is still a
    // linear-search Vec, so the supplied hash is held for inspection
    // (returned by `hashtable-hash-function`) but doesn't speed up
    // lookup at this milestone.
    match args.len() {
        0 => Ok(Value::Hashtable(Hashtable::new(HtEqKind::Equal))),
        2 => {
            let hash = args[0].clone();
            let equiv = args[1].clone();
            if !matches!(hash, Value::Procedure(_)) {
                return Err(type_err("make-hashtable", "procedure", &hash));
            }
            if !matches!(equiv, Value::Procedure(_)) {
                return Err(type_err("make-hashtable", "procedure", &equiv));
            }
            Ok(Value::Hashtable(Hashtable::new_custom(hash, equiv)))
        }
        n => Err(arity_err("make-hashtable", "0 or 2", n)),
    }
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
        HtEqKind::Custom => unreachable!("custom-equiv hashtables route through ht_eq_ctx"),
    }
}

/// Context-aware equality dispatch for hashtable lookups. Built-in
/// kinds (Eq/Eqv/Equal) short-circuit to the host comparator; the
/// Custom kind applies the user-supplied equiv procedure via the
/// walker's apply_procedure.
fn ht_eq_ctx(
    h: &Hashtable,
    key_a: &Value,
    key_b: &Value,
    ctx: &mut EvalCtx,
) -> Result<bool, String> {
    if h.eq_kind != HtEqKind::Custom {
        return Ok(ht_eq(h.eq_kind, key_a, key_b));
    }
    let equiv = h
        .custom
        .as_ref()
        .map(|c| c.equiv.clone())
        .ok_or_else(|| "hashtable: custom kind without procs".to_string())?;
    let r = apply_procedure(&equiv, &[key_a.clone(), key_b.clone()], ctx)
        .map_err(|d| format!("{:?}", d))?;
    Ok(r.is_truthy())
}

fn as_ht<'a>(name: &str, v: &'a Value) -> Result<&'a cs_core::Gc<Hashtable>, String> {
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

fn b_hashtable_set(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("hashtable-set!", "3", args.len()));
    }
    let h = as_ht("hashtable-set!", &args[0])?.clone();
    if h.eq_kind == HtEqKind::Custom {
        // Linear search applying the user's equiv proc each step.
        let len = h.items.borrow().len();
        for i in 0..len {
            let k = h.items.borrow()[i].0.clone();
            if ht_eq_ctx(&h, &k, &args[1], ctx)? {
                h.items.borrow_mut()[i].1 = args[2].clone();
                return Ok(Value::Unspecified);
            }
        }
        h.items
            .borrow_mut()
            .push((args[1].clone(), args[2].clone()));
        return Ok(Value::Unspecified);
    }
    let kind = h.eq_kind;
    let mut items = h.items.borrow_mut();
    if let Some(slot) = items.iter_mut().find(|(k, _)| ht_eq(kind, k, &args[1])) {
        slot.1 = args[2].clone();
    } else {
        items.push((args[1].clone(), args[2].clone()));
    }
    Ok(Value::Unspecified)
}

fn b_hashtable_ref(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("hashtable-ref", "3", args.len()));
    }
    let h = as_ht("hashtable-ref", &args[0])?.clone();
    if h.eq_kind == HtEqKind::Custom {
        let len = h.items.borrow().len();
        for i in 0..len {
            let k = h.items.borrow()[i].0.clone();
            if ht_eq_ctx(&h, &k, &args[1], ctx)? {
                return Ok(h.items.borrow()[i].1.clone());
            }
        }
        return Ok(args[2].clone());
    }
    let kind = h.eq_kind;
    let items = h.items.borrow();
    if let Some((_, v)) = items.iter().find(|(k, _)| ht_eq(kind, k, &args[1])) {
        Ok(v.clone())
    } else {
        Ok(args[2].clone())
    }
}

fn b_hashtable_contains(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("hashtable-contains?", "2", args.len()));
    }
    let h = as_ht("hashtable-contains?", &args[0])?.clone();
    if h.eq_kind == HtEqKind::Custom {
        let len = h.items.borrow().len();
        for i in 0..len {
            let k = h.items.borrow()[i].0.clone();
            if ht_eq_ctx(&h, &k, &args[1], ctx)? {
                return Ok(Value::Boolean(true));
            }
        }
        return Ok(Value::Boolean(false));
    }
    let kind = h.eq_kind;
    let items = h.items.borrow();
    Ok(Value::Boolean(
        items.iter().any(|(k, _)| ht_eq(kind, k, &args[1])),
    ))
}

fn b_hashtable_delete(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("hashtable-delete!", "2", args.len()));
    }
    let h = as_ht("hashtable-delete!", &args[0])?.clone();
    if h.eq_kind == HtEqKind::Custom {
        let len = h.items.borrow().len();
        for i in 0..len {
            let k = h.items.borrow()[i].0.clone();
            if ht_eq_ctx(&h, &k, &args[1], ctx)? {
                h.items.borrow_mut().swap_remove(i);
                return Ok(Value::Unspecified);
            }
        }
        return Ok(Value::Unspecified);
    }
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
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(v))))
}

fn b_hashtable_values(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-values", "1", args.len()));
    }
    let h = as_ht("hashtable-values", &args[0])?;
    let items = h.items.borrow();
    let v: Vec<Value> = items.iter().map(|(_, v)| v.clone()).collect();
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(v))))
}

/// `hashtable-copy` (R6RS) — return a fresh hashtable with the same
/// equivalence function and a snapshot of the entries. Optional second
/// arg `mutable?` is accepted for compat but ignored (we don't track
/// the immutability bit yet — copies are always mutable).
fn b_hashtable_copy(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("hashtable-copy", "1 or 2", args.len()));
    }
    let h = as_ht("hashtable-copy", &args[0])?;
    let items = h.items.borrow().clone();
    let new_ht = match h.eq_kind {
        HtEqKind::Custom => {
            let c = h.custom.as_ref().expect("custom kind has procs");
            Hashtable::new_custom(c.hash.clone(), c.equiv.clone())
        }
        kind => Hashtable::new(kind),
    };
    *new_ht.items.borrow_mut() = items;
    Ok(Value::Hashtable(new_ht))
}

/// `hashtable-mutable?` (R6RS). All hashtables we hand out are mutable.
fn b_hashtable_mutable_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-mutable?", "1", args.len()));
    }
    let _ = as_ht("hashtable-mutable?", &args[0])?;
    Ok(Value::Boolean(true))
}

/// `hashtable-equivalence-function` (R6RS) — returns the equivalence
/// function used by the hashtable: `eq?`, `eqv?`, or `equal?`. We hand
/// out a fresh builtin procedure value pointing at the same fn impl,
/// so identity-comparison against the global binding may differ but
/// applying the result behaves correctly.
pub fn b_hashtable_equivalence_function(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-equivalence-function", "1", args.len()));
    }
    let h = as_ht("hashtable-equivalence-function", &args[0])?;
    Ok(match h.eq_kind {
        HtEqKind::Eq => make_builtin_pure("eq?", b_eq),
        HtEqKind::Eqv => make_builtin_pure("eqv?", b_eqv),
        HtEqKind::Equal => make_builtin_pure("equal?", b_equal),
        HtEqKind::Custom => h
            .custom
            .as_ref()
            .expect("custom kind has procs")
            .equiv
            .clone(),
    })
}

/// VM-tier `hashtable-equivalence-function`: returns a VM-shape builtin
/// for built-in kinds (Walker returns a Builtin). For Custom tables we
/// hand back the user-supplied procedure verbatim — it's already
/// callable on whichever tier created it.
pub fn vm_hashtable_equivalence_function(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("hashtable-equivalence-function: 1 arg".into());
    }
    let h = as_ht("hashtable-equivalence-function", &args[0])?;
    Ok(match h.eq_kind {
        HtEqKind::Eq => cs_vm::vm::make_vm_builtin("eq?", b_eq),
        HtEqKind::Eqv => cs_vm::vm::make_vm_builtin("eqv?", b_eqv),
        HtEqKind::Equal => cs_vm::vm::make_vm_builtin("equal?", b_equal),
        HtEqKind::Custom => h
            .custom
            .as_ref()
            .expect("custom kind has procs")
            .equiv
            .clone(),
    })
}

/// `hashtable-hash-function` (R6RS) — returns the hash function for
/// custom-hash tables, or #f for built-in eq/eqv/equal hashtables.
fn b_hashtable_hash_function(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-hash-function", "1", args.len()));
    }
    let h = as_ht("hashtable-hash-function", &args[0])?;
    Ok(match h.eq_kind {
        HtEqKind::Custom => h
            .custom
            .as_ref()
            .expect("custom kind has procs")
            .hash
            .clone(),
        _ => Value::Boolean(false),
    })
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
/// `hashtable-entries` (R6RS) — returns two values via the multi-value
/// channel: vector of keys, vector of values. Both vectors share the
/// same indexing (entries[i] = (keys[i], values[i])).
fn b_hashtable_entries(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("hashtable-entries", "1", args.len()));
    }
    let h = as_ht("hashtable-entries", &args[0])?;
    let items = h.items.borrow();
    let keys: Vec<Value> = items.iter().map(|(k, _)| k.clone()).collect();
    let vals: Vec<Value> = items.iter().map(|(_, v)| v.clone()).collect();
    let kv = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(keys)));
    let vv = Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(vals)));
    ctx.pending_values = Some(vec![kv, vv]);
    Ok(Value::Unspecified)
}

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
                Ok(Value::ByteVector(cs_core::Gc::new(
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
                Ok(Value::ByteVector(cs_core::Gc::new(
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

/// R7RS `(read-char [port])`. Defaults to current-input-port.
fn b_read_char_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("read-char", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "read-char: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_read_char(&[port])
}

/// R7RS `(peek-char [port])`. Defaults to current-input-port.
fn b_peek_char_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("peek-char", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "peek-char: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_peek_char(&[port])
}

/// R7RS `(read-string k [port])`. k is required; port defaults to
/// current-input-port.
fn b_read_string_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("read-string", "1 or 2", args.len()));
    }
    let port = if args.len() == 1 {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "read-string: no current input port".to_string())?
    } else {
        args[1].clone()
    };
    b_read_string(&[args[0].clone(), port])
}

/// R7RS `(char-ready? [port])`.
fn b_char_ready_p_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("char-ready?", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "char-ready?: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_char_ready_p(&[port])
}

/// R7RS `(read-u8 [port])`.
fn b_read_u8_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("read-u8", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "read-u8: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_read_u8(&[port])
}

/// R7RS `(peek-u8 [port])`.
fn b_peek_u8_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("peek-u8", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "peek-u8: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_peek_u8(&[port])
}

/// R7RS `(u8-ready? [port])`.
fn b_u8_ready_p_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("u8-ready?", "0 or 1", args.len()));
    }
    let port = if args.is_empty() {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "u8-ready?: no current input port".to_string())?
    } else {
        args[0].clone()
    };
    b_u8_ready_p(&[port])
}

/// R7RS `(read-bytevector k [port])`. k required, port optional.
fn b_read_bytevector_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("read-bytevector", "1 or 2", args.len()));
    }
    let port = if args.len() == 1 {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "read-bytevector: no current input port".to_string())?
    } else {
        args[1].clone()
    };
    b_read_bytevector(&[args[0].clone(), port])
}

/// R7RS `(write-u8 byte [port])`.
fn b_write_u8_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("write-u8", "1 or 2", args.len()));
    }
    let port = if args.len() == 1 {
        ctx.current_output_port
            .clone()
            .ok_or_else(|| "write-u8: no current output port".to_string())?
    } else {
        args[1].clone()
    };
    b_write_u8(&[args[0].clone(), port])
}

/// R7RS `(write-bytevector bv [port [start [end]]])`.
fn b_write_bytevector_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 4 {
        return Err(arity_err("write-bytevector", "1..4", args.len()));
    }
    let mut full = Vec::with_capacity(4);
    full.push(args[0].clone());
    if args.len() == 1 {
        let port = ctx
            .current_output_port
            .clone()
            .ok_or_else(|| "write-bytevector: no current output port".to_string())?;
        full.push(port);
    } else {
        full.push(args[1].clone());
    }
    if args.len() >= 3 {
        full.push(args[2].clone());
    }
    if args.len() == 4 {
        full.push(args[3].clone());
    }
    b_write_bytevector(&full)
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

/// `(read-string k port)` — R7RS. Read up to k chars from `port` and
/// return them as a string. If 0 chars remain at EOF, returns the
/// EOF object. If fewer than k are available, returns whatever's
/// available; subsequent reads would return EOF.
fn b_read_string(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("read-string", "2", args.len()));
    }
    let k = as_int_i64("read-string", &args[0])?;
    if k < 0 {
        return Err("read-string: negative count".into());
    }
    let k = k as usize;
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.chars.len() {
                    return Ok(Value::Eof);
                }
                let end = std::cmp::min(s.pos + k, s.chars.len());
                let collected: String = s.chars[s.pos..end].iter().collect();
                s.pos = end;
                Ok(Value::string(collected))
            }
            _ => Err("read-string: not a textual input port".into()),
        },
        v => Err(type_err("read-string", "input-port", v)),
    }
}

/// `(char-ready? port)` — R7RS. True if a `read-char` would return
/// without blocking. For string ports we always know the answer:
/// true if chars remain, true at EOF (read-char returns EOF without
/// blocking), so always true for our backing.
fn b_char_ready_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("char-ready?", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(_) => Ok(Value::Boolean(true)),
            _ => Err("char-ready?: not a textual input port".into()),
        },
        v => Err(type_err("char-ready?", "input-port", v)),
    }
}

/// `(read-u8 port)` — R7RS. Read one byte from a binary input port
/// or return EOF.
fn b_read_u8(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("read-u8", "1", args.len()));
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
            _ => Err("read-u8: not a binary input port".into()),
        },
        v => Err(type_err("read-u8", "input-port", v)),
    }
}

/// `(peek-u8 port)` — R7RS. Like `read-u8` but without consuming.
fn b_peek_u8(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("peek-u8", "1", args.len()));
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
            _ => Err("peek-u8: not a binary input port".into()),
        },
        v => Err(type_err("peek-u8", "input-port", v)),
    }
}

/// `(u8-ready? port)` — R7RS binary counterpart of char-ready?.
fn b_u8_ready_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("u8-ready?", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(_) => Ok(Value::Boolean(true)),
            _ => Err("u8-ready?: not a binary input port".into()),
        },
        v => Err(type_err("u8-ready?", "input-port", v)),
    }
}

/// `(read-bytevector k port)` — R7RS. Read up to k bytes; returns a
/// fresh bytevector (possibly shorter than k) or EOF when no bytes
/// remain.
fn b_read_bytevector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("read-bytevector", "2", args.len()));
    }
    let k = as_int_i64("read-bytevector", &args[0])?;
    if k < 0 {
        return Err("read-bytevector: negative count".into());
    }
    let k = k as usize;
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.bytes.len() {
                    return Ok(Value::Eof);
                }
                let end = std::cmp::min(s.pos + k, s.bytes.len());
                let bytes: Vec<u8> = s.bytes[s.pos..end].to_vec();
                s.pos = end;
                Ok(Value::ByteVector(cs_core::Gc::new(
                    std::cell::RefCell::new(bytes),
                )))
            }
            _ => Err("read-bytevector: not a binary input port".into()),
        },
        v => Err(type_err("read-bytevector", "input-port", v)),
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

/// R7RS `(write-char char [port])` — port defaults to current-output-port.
fn b_write_char_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("write-char", "1 or 2", args.len()));
    }
    let port = if args.len() == 1 {
        ctx.current_output_port
            .clone()
            .ok_or_else(|| "write-char: no current output port".to_string())?
    } else {
        args[1].clone()
    };
    b_write_char(&[args[0].clone(), port])
}

/// R7RS `(write-string string [port [start [end]]])` — port defaults to
/// current-output-port. Slice indices are forwarded to the Pure impl.
fn b_write_string_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 4 {
        return Err(arity_err("write-string", "1..4", args.len()));
    }
    let mut full = Vec::with_capacity(4);
    full.push(args[0].clone());
    if args.len() == 1 {
        let port = ctx
            .current_output_port
            .clone()
            .ok_or_else(|| "write-string: no current output port".to_string())?;
        full.push(port);
    } else {
        full.push(args[1].clone());
    }
    if args.len() >= 3 {
        full.push(args[2].clone());
    }
    if args.len() == 4 {
        full.push(args[3].clone());
    }
    b_write_string(&full)
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
    // R7RS: (write-string string port [start [end]])
    if args.len() < 2 || args.len() > 4 {
        return Err(arity_err("write-string", "2..4", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("write-string", "string", v)),
    };
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.len() >= 3 {
        match &args[2] {
            Value::Number(n) => match n.to_f64() as i64 {
                i if i >= 0 && (i as usize) <= len => i as usize,
                _ => return Err(format!("write-string: start out of range: {}", n.to_f64())),
            },
            v => return Err(type_err("write-string", "exact integer start", v)),
        }
    } else {
        0
    };
    let end = if args.len() == 4 {
        match &args[3] {
            Value::Number(n) => match n.to_f64() as i64 {
                i if i >= 0 && (i as usize) <= len && (i as usize) >= start => i as usize,
                _ => return Err(format!("write-string: end out of range: {}", n.to_f64())),
            },
            v => return Err(type_err("write-string", "exact integer end", v)),
        }
    } else {
        len
    };
    let slice: String = chars[start..end].iter().collect();
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::StringOutput(buf) => {
                buf.borrow_mut().push_str(&slice);
                Ok(Value::Unspecified)
            }
            _ => Err("write-string: not an output port".into()),
        },
        v => Err(type_err("write-string", "output-port", v)),
    }
}

fn b_write_u8(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("write-u8", "2", args.len()));
    }
    let byte = match &args[0] {
        Value::Number(n) => match n.to_f64() as i64 {
            i if (0..=255).contains(&i) => i as u8,
            _ => return Err(format!("write-u8: byte out of range: {}", n.to_f64())),
        },
        v => return Err(type_err("write-u8", "byte (0..255)", v)),
    };
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::ByteVectorOutput(buf) => {
                buf.borrow_mut().push(byte);
                Ok(Value::Unspecified)
            }
            _ => Err("write-u8: not a binary output port".into()),
        },
        v => Err(type_err("write-u8", "output-port", v)),
    }
}

fn b_write_bytevector(args: &[Value]) -> Result<Value, String> {
    // R7RS: (write-bytevector bv port [start [end]])
    if args.len() < 2 || args.len() > 4 {
        return Err(arity_err("write-bytevector", "2..4", args.len()));
    }
    let bytes = match &args[0] {
        Value::ByteVector(b) => b.borrow().clone(),
        v => return Err(type_err("write-bytevector", "bytevector", v)),
    };
    let len = bytes.len();
    let start = if args.len() >= 3 {
        match &args[2] {
            Value::Number(n) => match n.to_f64() as i64 {
                i if i >= 0 && (i as usize) <= len => i as usize,
                _ => {
                    return Err(format!(
                        "write-bytevector: start out of range: {}",
                        n.to_f64()
                    ))
                }
            },
            v => return Err(type_err("write-bytevector", "exact integer start", v)),
        }
    } else {
        0
    };
    let end = if args.len() == 4 {
        match &args[3] {
            Value::Number(n) => match n.to_f64() as i64 {
                i if i >= 0 && (i as usize) <= len && (i as usize) >= start => i as usize,
                _ => {
                    return Err(format!(
                        "write-bytevector: end out of range: {}",
                        n.to_f64()
                    ))
                }
            },
            v => return Err(type_err("write-bytevector", "exact integer end", v)),
        }
    } else {
        len
    };
    match &args[1] {
        Value::Port(p) => match &**p {
            Port::ByteVectorOutput(buf) => {
                buf.borrow_mut().extend_from_slice(&bytes[start..end]);
                Ok(Value::Unspecified)
            }
            _ => Err("write-bytevector: not a binary output port".into()),
        },
        v => Err(type_err("write-bytevector", "output-port", v)),
    }
}

// ---- promises ----

fn b_promise_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("promise?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(args[0], Value::Promise(_))))
}

/// R7RS `(make-promise obj)` — returns a promise already in the forced
/// state holding `obj`. Forcing it returns `obj` immediately.
fn b_make_promise(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-promise", "1", args.len()));
    }
    let p = Promise::pending(args[0].clone());
    *p.state.borrow_mut() = PromiseState::Forced(args[0].clone());
    Ok(Value::Promise(p))
}

/// Internal: wraps a thunk as a Pending promise. Used by the expansion of
/// `delay` and `delay-force`. Not part of R7RS — distinct from
/// `make-promise` which takes a value, not a thunk.
fn b_make_pending_promise(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("__make-pending-promise", "1", args.len()));
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

/// R7RS `(call-with-port port proc)` — passes the port to proc and closes
/// the port when proc returns (or unwinds). Returns proc's value.
fn b_call_with_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("call-with-port", "2", args.len()));
    }
    if !matches!(&args[0], Value::Port(_)) {
        return Err(type_err("call-with-port", "port", &args[0]));
    }
    let port = args[0].clone();
    let result = apply_procedure(&args[1], &[port.clone()], ctx).map_err(|e| e.message());
    // Best-effort close — ignore close errors so they don't mask the
    // proc's return / error.
    let _ = b_close_port(&[port]);
    result
}

/// R7RS `(call-with-input-string str proc)` — convenience wrapper:
/// open a string input port, hand it to proc, close it on return.
fn b_call_with_input_string(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("call-with-input-string", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("call-with-input-string", "string", v)),
    };
    let port = Value::Port(Port::string_input(&s));
    let result = apply_procedure(&args[1], &[port.clone()], ctx).map_err(|e| e.message());
    let _ = b_close_port(&[port]);
    result
}

/// R7RS `(call-with-output-string proc)` — open a string output port,
/// pass it to proc, then return the accumulated string.
fn b_call_with_output_string(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("call-with-output-string", "1", args.len()));
    }
    let port = Port::string_output();
    let port_val = Value::Port(port.clone());
    let result = apply_procedure(&args[0], &[port_val], ctx).map_err(|e| e.message());
    result?;
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
    // Iterative force loop. R7RS `delay-force` requires that a promise
    // whose thunk returns another promise can be unwound without growing
    // the stack — otherwise (define-rec (loop p) (delay-force (loop ...)))
    // would crash. We don't grow the Rust stack here; we walk until we
    // hit a Forced state or a non-promise value.
    //
    // The original (outer) promise is the one we eventually memoize. Any
    // intermediate lazy promises encountered along the way leak — that's
    // OK for the foundation; the spec-recommended state-aliasing
    // optimization can land in a later iter.
    let original = args[0].clone();
    let mut cur = original.clone();
    loop {
        match cur {
            Value::Promise(p) => {
                // Already forced? Memoize on the original and return.
                {
                    let state = p.state.borrow();
                    if let PromiseState::Forced(v) = &*state {
                        let v = v.clone();
                        if let Value::Promise(orig) = &original {
                            if !std::ptr::eq(&**orig as *const _, &*p as *const _) {
                                *orig.state.borrow_mut() = PromiseState::Forced(v.clone());
                            }
                        }
                        return Ok(v);
                    }
                }
                // Pending: run the thunk.
                let thunk = match &*p.state.borrow() {
                    PromiseState::Pending(t) => t.clone(),
                    PromiseState::Forced(_) => unreachable!(),
                };
                let v = apply_procedure(&thunk, &[], ctx).map_err(|e| e.message())?;
                if matches!(v, Value::Promise(_)) {
                    // Thunk returned another promise — iterate. The
                    // intermediate promise `p` stays pending; the loop
                    // continues with the new promise as `cur`.
                    cur = v;
                    continue;
                }
                // Non-promise value: memoize on the original and return.
                if let Value::Promise(orig) = &original {
                    *orig.state.borrow_mut() = PromiseState::Forced(v.clone());
                }
                return Ok(v);
            }
            // Non-promise input: R6RS-style passthrough.
            v => return Ok(v),
        }
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
    Ok(Value::ByteVector(cs_core::Gc::new(
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
    Ok(Value::ByteVector(cs_core::Gc::new(
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
    Ok(Value::ByteVector(cs_core::Gc::new(
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
    Ok(Value::ByteVector(cs_core::Gc::new(
        std::cell::RefCell::new(out),
    )))
}

/// `(bytevector-fill! bv byte)` — write `byte` to every slot of `bv`.
fn b_bytevector_fill(args: &[Value]) -> Result<Value, String> {
    // R7RS: (bytevector-fill! bv fill [start [end]]).
    if args.len() < 2 || args.len() > 4 {
        return Err(arity_err("bytevector-fill!", "2..4", args.len()));
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
    let len = v.len();
    let start = if args.len() >= 3 {
        let i = as_int_i64("bytevector-fill!", &args[2])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("bytevector-fill!: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 4 {
        let i = as_int_i64("bytevector-fill!", &args[3])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("bytevector-fill!: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    for slot in &mut v[start..end] {
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

// =====================================================================
// R6RS (rnrs bytevectors) — multi-byte signed/unsigned and IEEE-754
// accessors. Endianness comes in as a Scheme symbol ('big or 'little).
// All ops bounds-check (offset + width) against the bytevector length.

#[derive(Copy, Clone)]
enum Endian {
    Big,
    Little,
}

fn parse_endian(name: &str, v: &Value, syms: &SymbolTable) -> Result<Endian, String> {
    match v {
        Value::Symbol(s) => match syms.name(*s) {
            "big" => Ok(Endian::Big),
            "little" => Ok(Endian::Little),
            _ => Err(format!("{}: endianness must be 'big or 'little", name)),
        },
        _ => Err(type_err(name, "endianness symbol", v)),
    }
}

fn native_endian() -> Endian {
    if cfg!(target_endian = "big") {
        Endian::Big
    } else {
        Endian::Little
    }
}

fn bv_read_bytes<const N: usize>(name: &str, args: &[Value]) -> Result<(usize, [u8; N]), String> {
    let i = as_int_i64(name, &args[1])?;
    if i < 0 {
        return Err(format!("{}: negative index", name));
    }
    let i = i as usize;
    match &args[0] {
        Value::ByteVector(bv) => {
            let bv = bv.borrow();
            if i + N > bv.len() {
                return Err(format!("{}: index out of range", name));
            }
            let mut buf = [0u8; N];
            buf.copy_from_slice(&bv[i..i + N]);
            Ok((i, buf))
        }
        v => Err(type_err(name, "bytevector", v)),
    }
}

fn bv_write_bytes<const N: usize>(
    name: &str,
    args: &[Value],
    bytes: [u8; N],
) -> Result<Value, String> {
    let i = as_int_i64(name, &args[1])?;
    if i < 0 {
        return Err(format!("{}: negative index", name));
    }
    let i = i as usize;
    match &args[0] {
        Value::ByteVector(bv) => {
            let mut bv = bv.borrow_mut();
            if i + N > bv.len() {
                return Err(format!("{}: index out of range", name));
            }
            bv[i..i + N].copy_from_slice(&bytes);
            Ok(Value::Unspecified)
        }
        v => Err(type_err(name, "bytevector", v)),
    }
}

fn b_bytevector_s8_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector-s8-ref", "2", args.len()));
    }
    let (_, buf) = bv_read_bytes::<1>("bytevector-s8-ref", args)?;
    Ok(Value::fixnum(buf[0] as i8 as i64))
}

fn b_bytevector_s8_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-s8-set!", "3", args.len()));
    }
    let v = as_int_i64("bytevector-s8-set!", &args[2])?;
    if !(-128..=127).contains(&v) {
        return Err("bytevector-s8-set!: value out of s8 range".into());
    }
    bv_write_bytes::<1>("bytevector-s8-set!", args, [v as i8 as u8])
}

// Macro to generate {u,s}{16,32,64} ref / set / native variants. The
// per-width logic is identical except for the int width and signed
// extension on read; using a macro keeps the surface area small.
macro_rules! bv_int_ops {
    (
        $width:expr, $bytes:expr,
        $name_uref:literal, $fn_uref:ident,
        $name_uset:literal, $fn_uset:ident,
        $name_sref:literal, $fn_sref:ident,
        $name_sset:literal, $fn_sset:ident,
        $name_nuref:literal, $fn_nuref:ident,
        $name_nuset:literal, $fn_nuset:ident,
        $name_nsref:literal, $fn_nsref:ident,
        $name_nsset:literal, $fn_nsset:ident,
        $u_ty:ty, $s_ty:ty
    ) => {
        fn $fn_uref(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
            if args.len() != 3 {
                return Err(arity_err($name_uref, "3", args.len()));
            }
            let endian = parse_endian($name_uref, &args[2], syms)?;
            let (_, buf) = bv_read_bytes::<$bytes>($name_uref, args)?;
            let v = match endian {
                Endian::Big => <$u_ty>::from_be_bytes(buf),
                Endian::Little => <$u_ty>::from_le_bytes(buf),
            };
            Ok(Value::Number(Number::from_i64(v as i64)))
        }

        fn $fn_uset(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
            if args.len() != 4 {
                return Err(arity_err($name_uset, "4", args.len()));
            }
            let endian = parse_endian($name_uset, &args[3], syms)?;
            let v = as_int_i64($name_uset, &args[2])?;
            let max: i128 = (<$u_ty>::MAX as u128) as i128;
            if (v as i128) < 0 || (v as i128) > max {
                return Err(format!(
                    "{}: value out of {}-bit unsigned range",
                    $name_uset, $width
                ));
            }
            let bytes = match endian {
                Endian::Big => (v as $u_ty).to_be_bytes(),
                Endian::Little => (v as $u_ty).to_le_bytes(),
            };
            bv_write_bytes::<$bytes>($name_uset, args, bytes)
        }

        fn $fn_sref(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
            if args.len() != 3 {
                return Err(arity_err($name_sref, "3", args.len()));
            }
            let endian = parse_endian($name_sref, &args[2], syms)?;
            let (_, buf) = bv_read_bytes::<$bytes>($name_sref, args)?;
            let v = match endian {
                Endian::Big => <$s_ty>::from_be_bytes(buf),
                Endian::Little => <$s_ty>::from_le_bytes(buf),
            };
            Ok(Value::Number(Number::from_i64(v as i64)))
        }

        fn $fn_sset(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
            if args.len() != 4 {
                return Err(arity_err($name_sset, "4", args.len()));
            }
            let endian = parse_endian($name_sset, &args[3], syms)?;
            let v = as_int_i64($name_sset, &args[2])?;
            let lo: i128 = <$s_ty>::MIN as i128;
            let hi: i128 = <$s_ty>::MAX as i128;
            if (v as i128) < lo || (v as i128) > hi {
                return Err(format!(
                    "{}: value out of {}-bit signed range",
                    $name_sset, $width
                ));
            }
            let bytes = match endian {
                Endian::Big => (v as $s_ty).to_be_bytes(),
                Endian::Little => (v as $s_ty).to_le_bytes(),
            };
            bv_write_bytes::<$bytes>($name_sset, args, bytes)
        }

        fn $fn_nuref(args: &[Value]) -> Result<Value, String> {
            if args.len() != 2 {
                return Err(arity_err($name_nuref, "2", args.len()));
            }
            let (_, buf) = bv_read_bytes::<$bytes>($name_nuref, args)?;
            let v = match native_endian() {
                Endian::Big => <$u_ty>::from_be_bytes(buf),
                Endian::Little => <$u_ty>::from_le_bytes(buf),
            };
            Ok(Value::Number(Number::from_i64(v as i64)))
        }

        fn $fn_nuset(args: &[Value]) -> Result<Value, String> {
            if args.len() != 3 {
                return Err(arity_err($name_nuset, "3", args.len()));
            }
            let v = as_int_i64($name_nuset, &args[2])?;
            let max: i128 = (<$u_ty>::MAX as u128) as i128;
            if (v as i128) < 0 || (v as i128) > max {
                return Err(format!(
                    "{}: value out of {}-bit unsigned range",
                    $name_nuset, $width
                ));
            }
            let bytes = match native_endian() {
                Endian::Big => (v as $u_ty).to_be_bytes(),
                Endian::Little => (v as $u_ty).to_le_bytes(),
            };
            bv_write_bytes::<$bytes>($name_nuset, args, bytes)
        }

        fn $fn_nsref(args: &[Value]) -> Result<Value, String> {
            if args.len() != 2 {
                return Err(arity_err($name_nsref, "2", args.len()));
            }
            let (_, buf) = bv_read_bytes::<$bytes>($name_nsref, args)?;
            let v = match native_endian() {
                Endian::Big => <$s_ty>::from_be_bytes(buf),
                Endian::Little => <$s_ty>::from_le_bytes(buf),
            };
            Ok(Value::Number(Number::from_i64(v as i64)))
        }

        fn $fn_nsset(args: &[Value]) -> Result<Value, String> {
            if args.len() != 3 {
                return Err(arity_err($name_nsset, "3", args.len()));
            }
            let v = as_int_i64($name_nsset, &args[2])?;
            let lo: i128 = <$s_ty>::MIN as i128;
            let hi: i128 = <$s_ty>::MAX as i128;
            if (v as i128) < lo || (v as i128) > hi {
                return Err(format!(
                    "{}: value out of {}-bit signed range",
                    $name_nsset, $width
                ));
            }
            let bytes = match native_endian() {
                Endian::Big => (v as $s_ty).to_be_bytes(),
                Endian::Little => (v as $s_ty).to_le_bytes(),
            };
            bv_write_bytes::<$bytes>($name_nsset, args, bytes)
        }
    };
}

bv_int_ops!(
    16,
    2,
    "bytevector-u16-ref",
    b_bytevector_u16_ref,
    "bytevector-u16-set!",
    b_bytevector_u16_set,
    "bytevector-s16-ref",
    b_bytevector_s16_ref,
    "bytevector-s16-set!",
    b_bytevector_s16_set,
    "bytevector-u16-native-ref",
    b_bytevector_u16_native_ref,
    "bytevector-u16-native-set!",
    b_bytevector_u16_native_set,
    "bytevector-s16-native-ref",
    b_bytevector_s16_native_ref,
    "bytevector-s16-native-set!",
    b_bytevector_s16_native_set,
    u16,
    i16
);

bv_int_ops!(
    32,
    4,
    "bytevector-u32-ref",
    b_bytevector_u32_ref,
    "bytevector-u32-set!",
    b_bytevector_u32_set,
    "bytevector-s32-ref",
    b_bytevector_s32_ref,
    "bytevector-s32-set!",
    b_bytevector_s32_set,
    "bytevector-u32-native-ref",
    b_bytevector_u32_native_ref,
    "bytevector-u32-native-set!",
    b_bytevector_u32_native_set,
    "bytevector-s32-native-ref",
    b_bytevector_s32_native_ref,
    "bytevector-s32-native-set!",
    b_bytevector_s32_native_set,
    u32,
    i32
);

// 64-bit unsigned can exceed i64::MAX → fall back to BigInt via parse.
fn b_bytevector_u64_ref(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-u64-ref", "3", args.len()));
    }
    let endian = parse_endian("bytevector-u64-ref", &args[2], syms)?;
    let (_, buf) = bv_read_bytes::<8>("bytevector-u64-ref", args)?;
    let v = match endian {
        Endian::Big => u64::from_be_bytes(buf),
        Endian::Little => u64::from_le_bytes(buf),
    };
    Ok(u64_to_value(v))
}

fn b_bytevector_u64_set(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("bytevector-u64-set!", "4", args.len()));
    }
    let endian = parse_endian("bytevector-u64-set!", &args[3], syms)?;
    let v = value_to_u64("bytevector-u64-set!", &args[2])?;
    let bytes = match endian {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<8>("bytevector-u64-set!", args, bytes)
}

fn b_bytevector_s64_ref(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-s64-ref", "3", args.len()));
    }
    let endian = parse_endian("bytevector-s64-ref", &args[2], syms)?;
    let (_, buf) = bv_read_bytes::<8>("bytevector-s64-ref", args)?;
    let v = match endian {
        Endian::Big => i64::from_be_bytes(buf),
        Endian::Little => i64::from_le_bytes(buf),
    };
    Ok(Value::Number(Number::from_i64(v)))
}

fn b_bytevector_s64_set(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("bytevector-s64-set!", "4", args.len()));
    }
    let endian = parse_endian("bytevector-s64-set!", &args[3], syms)?;
    let v = as_int_i64("bytevector-s64-set!", &args[2])?;
    let bytes = match endian {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<8>("bytevector-s64-set!", args, bytes)
}

fn b_bytevector_u64_native_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector-u64-native-ref", "2", args.len()));
    }
    let (_, buf) = bv_read_bytes::<8>("bytevector-u64-native-ref", args)?;
    let v = match native_endian() {
        Endian::Big => u64::from_be_bytes(buf),
        Endian::Little => u64::from_le_bytes(buf),
    };
    Ok(u64_to_value(v))
}

fn b_bytevector_u64_native_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-u64-native-set!", "3", args.len()));
    }
    let v = value_to_u64("bytevector-u64-native-set!", &args[2])?;
    let bytes = match native_endian() {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<8>("bytevector-u64-native-set!", args, bytes)
}

fn b_bytevector_s64_native_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector-s64-native-ref", "2", args.len()));
    }
    let (_, buf) = bv_read_bytes::<8>("bytevector-s64-native-ref", args)?;
    let v = match native_endian() {
        Endian::Big => i64::from_be_bytes(buf),
        Endian::Little => i64::from_le_bytes(buf),
    };
    Ok(Value::Number(Number::from_i64(v)))
}

fn b_bytevector_s64_native_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-s64-native-set!", "3", args.len()));
    }
    let v = as_int_i64("bytevector-s64-native-set!", &args[2])?;
    let bytes = match native_endian() {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<8>("bytevector-s64-native-set!", args, bytes)
}

fn u64_to_value(v: u64) -> Value {
    if v <= i64::MAX as u64 {
        Value::Number(Number::from_i64(v as i64))
    } else {
        // Need a BigInt for values > i64::MAX. Use parse_decimal_integer.
        let s = v.to_string();
        Value::Number(Number::parse_decimal_integer(&s).expect("u64 to bigint"))
    }
}

fn value_to_u64(name: &str, v: &Value) -> Result<u64, String> {
    use num_traits::ToPrimitive;
    match v {
        Value::Number(Number::Fixnum(n)) => {
            if *n < 0 {
                Err(format!("{}: value negative for u64", name))
            } else {
                Ok(*n as u64)
            }
        }
        Value::Number(Number::Big(b)) => b
            .to_u64()
            .ok_or_else(|| format!("{}: value out of u64 range", name)),
        _ => Err(type_err(name, "non-negative integer", v)),
    }
}

// IEEE-754 single (f32) and double (f64) accessors.
fn b_bytevector_ieee_single_ref(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-ieee-single-ref", "3", args.len()));
    }
    let endian = parse_endian("bytevector-ieee-single-ref", &args[2], syms)?;
    let (_, buf) = bv_read_bytes::<4>("bytevector-ieee-single-ref", args)?;
    let v = match endian {
        Endian::Big => f32::from_be_bytes(buf),
        Endian::Little => f32::from_le_bytes(buf),
    };
    Ok(Value::Number(Number::Flonum(v as f64)))
}

fn b_bytevector_ieee_single_set(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("bytevector-ieee-single-set!", "4", args.len()));
    }
    let endian = parse_endian("bytevector-ieee-single-set!", &args[3], syms)?;
    let v = as_num("bytevector-ieee-single-set!", &args[2])?.to_f64() as f32;
    let bytes = match endian {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<4>("bytevector-ieee-single-set!", args, bytes)
}

fn b_bytevector_ieee_double_ref(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("bytevector-ieee-double-ref", "3", args.len()));
    }
    let endian = parse_endian("bytevector-ieee-double-ref", &args[2], syms)?;
    let (_, buf) = bv_read_bytes::<8>("bytevector-ieee-double-ref", args)?;
    let v = match endian {
        Endian::Big => f64::from_be_bytes(buf),
        Endian::Little => f64::from_le_bytes(buf),
    };
    Ok(Value::Number(Number::Flonum(v)))
}

fn b_bytevector_ieee_double_set(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("bytevector-ieee-double-set!", "4", args.len()));
    }
    let endian = parse_endian("bytevector-ieee-double-set!", &args[3], syms)?;
    let v = as_num("bytevector-ieee-double-set!", &args[2])?.to_f64();
    let bytes = match endian {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<8>("bytevector-ieee-double-set!", args, bytes)
}

fn b_bytevector_ieee_single_native_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err(
            "bytevector-ieee-single-native-ref",
            "2",
            args.len(),
        ));
    }
    let (_, buf) = bv_read_bytes::<4>("bytevector-ieee-single-native-ref", args)?;
    let v = match native_endian() {
        Endian::Big => f32::from_be_bytes(buf),
        Endian::Little => f32::from_le_bytes(buf),
    };
    Ok(Value::Number(Number::Flonum(v as f64)))
}

fn b_bytevector_ieee_single_native_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err(
            "bytevector-ieee-single-native-set!",
            "3",
            args.len(),
        ));
    }
    let v = as_num("bytevector-ieee-single-native-set!", &args[2])?.to_f64() as f32;
    let bytes = match native_endian() {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<4>("bytevector-ieee-single-native-set!", args, bytes)
}

fn b_bytevector_ieee_double_native_ref(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err(
            "bytevector-ieee-double-native-ref",
            "2",
            args.len(),
        ));
    }
    let (_, buf) = bv_read_bytes::<8>("bytevector-ieee-double-native-ref", args)?;
    let v = match native_endian() {
        Endian::Big => f64::from_be_bytes(buf),
        Endian::Little => f64::from_le_bytes(buf),
    };
    Ok(Value::Number(Number::Flonum(v)))
}

fn b_bytevector_ieee_double_native_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err(
            "bytevector-ieee-double-native-set!",
            "3",
            args.len(),
        ));
    }
    let v = as_num("bytevector-ieee-double-native-set!", &args[2])?.to_f64();
    let bytes = match native_endian() {
        Endian::Big => v.to_be_bytes(),
        Endian::Little => v.to_le_bytes(),
    };
    bv_write_bytes::<8>("bytevector-ieee-double-native-set!", args, bytes)
}

fn b_native_endianness(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("native-endianness", "0", args.len()));
    }
    let s = match native_endian() {
        Endian::Big => syms.intern("big"),
        Endian::Little => syms.intern("little"),
    };
    Ok(Value::Symbol(s))
}

fn b_bytevector_copy(args: &[Value]) -> Result<Value, String> {
    // R7RS: (bytevector-copy bv [start [end]]).
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("bytevector-copy", "1..3", args.len()));
    }
    let bytes = match &args[0] {
        Value::ByteVector(bv) => bv.borrow().clone(),
        v => return Err(type_err("bytevector-copy", "bytevector", v)),
    };
    let len = bytes.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("bytevector-copy", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("bytevector-copy: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("bytevector-copy", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("bytevector-copy: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    Ok(Value::ByteVector(cs_core::Gc::new(
        std::cell::RefCell::new(bytes[start..end].to_vec()),
    )))
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

/// R7RS `(bytevector->list bv [start [end]])` — convert (a slice of) the
/// bytevector to a list of integers.
fn b_bytevector_to_list_r7rs(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("bytevector->list", "1..3", args.len()));
    }
    let bytes = match &args[0] {
        Value::ByteVector(bv) => bv.borrow().clone(),
        v => return Err(type_err("bytevector->list", "bytevector", v)),
    };
    let len = bytes.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("bytevector->list", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("bytevector->list: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("bytevector->list", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("bytevector->list: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    Ok(Value::list(
        bytes[start..end].iter().map(|b| Value::fixnum(*b as i64)),
    ))
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
    Ok(Value::ByteVector(cs_core::Gc::new(
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

/// R7RS `(current-error-port)` — returns a port for error output.
/// Foundation: lazily creates a string output port the first time it's
/// queried per Runtime, then returns the same port for subsequent calls.
/// User code can write to it via display/write/newline; the buffer is
/// observable via get-output-string.
fn b_current_error_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-error-port", "0", args.len()));
    }
    if ctx.current_error_port.is_none() {
        ctx.current_error_port = Some(Value::Port(Port::string_output()));
    }
    Ok(ctx.current_error_port.clone().unwrap())
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

fn b_exact_integer_sqrt(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // R6RS multi-value return: (s r) such that s² ≤ n < (s+1)², r = n - s².
    if args.len() != 1 {
        return Err(arity_err("exact-integer-sqrt", "1", args.len()));
    }
    let n = as_integer_num("exact-integer-sqrt", &args[0])?;
    let (s, r) = n
        .exact_integer_sqrt()
        .ok_or_else(|| "exact-integer-sqrt: negative or non-integer argument".to_string())?;
    ctx.pending_values = Some(vec![Value::Number(s), Value::Number(r)]);
    Ok(Value::Unspecified)
}

/// Public for VM-tier shim — mirrors div_and_mod_num.
pub fn exact_integer_sqrt_num(x: &Value) -> Result<(Value, Value), String> {
    let n = as_integer_num("exact-integer-sqrt", x)?;
    let (s, r) = n
        .exact_integer_sqrt()
        .ok_or_else(|| "exact-integer-sqrt: negative or non-integer argument".to_string())?;
    Ok((Value::Number(s), Value::Number(r)))
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

fn b_file_error_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("file-error?", "1", args.len()));
    }
    Ok(Value::Boolean(
        is_any_cond(&args[0]) && find_simple_with_tag(&args[0], TAG_FILE_ERROR).is_some(),
    ))
}

fn b_read_error_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("read-error?", "1", args.len()));
    }
    Ok(Value::Boolean(
        is_any_cond(&args[0]) && find_simple_with_tag(&args[0], TAG_READ_ERROR).is_some(),
    ))
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
    // R7RS: (string-map proc str1 str2 ...) — proc takes one char from each
    // string. Result terminates at the shortest input.
    if args.len() < 2 {
        return Err(arity_err("string-map", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let strings: Vec<Vec<char>> = args[1..]
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.borrow().chars().collect()),
            other => Err(type_err("string-map", "string", other)),
        })
        .collect::<Result<_, _>>()?;
    let n = strings.iter().map(|s| s.len()).min().unwrap_or(0);
    let mut out = String::with_capacity(n);
    for i in 0..n {
        let row: Vec<Value> = strings.iter().map(|s| Value::Character(s[i])).collect();
        let r = apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
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
    // R7RS: (string-for-each proc str1 str2 ...) — like string-map but for
    // side effects. Iteration terminates at the shortest string.
    if args.len() < 2 {
        return Err(arity_err("string-for-each", "at least 2", args.len()));
    }
    let proc_val = args[0].clone();
    let strings: Vec<Vec<char>> = args[1..]
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.borrow().chars().collect()),
            other => Err(type_err("string-for-each", "string", other)),
        })
        .collect::<Result<_, _>>()?;
    let n = strings.iter().map(|s| s.len()).min().unwrap_or(0);
    for i in 0..n {
        let row: Vec<Value> = strings.iter().map(|s| Value::Character(s[i])).collect();
        apply_procedure(&proc_val, &row, ctx).map_err(|e| e.message())?;
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
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
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
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
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
    let contents = std::fs::read_to_string(&path).map_err(|e| {
        cs_core::stash_builtin_err_extra_tag(TAG_FILE_ERROR);
        format!("open-input-file: cannot read {}: {}", path, e)
    })?;
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
    std::fs::write(&path, "").map_err(|e| {
        cs_core::stash_builtin_err_extra_tag(TAG_FILE_ERROR);
        format!("open-output-file: cannot create {}: {}", path, e)
    })?;
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

/// R7RS `(close-input-port port)` — closes an input port. Errors if the
/// port is not an input port.
fn b_close_input_port(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("close-input-port", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => {
            if !p.is_input() {
                return Err("close-input-port: not an input port".into());
            }
            // Input ports hold no OS resources at this milestone — no-op.
            Ok(Value::Unspecified)
        }
        v => Err(type_err("close-input-port", "input-port", v)),
    }
}

/// R7RS `(close-output-port port)` — closes an output port and flushes any
/// buffered data. Errors if the port is not an output port.
fn b_close_output_port(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("close-output-port", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => {
            if !p.is_output() {
                return Err("close-output-port: not an output port".into());
            }
            b_close_port(args)
        }
        v => Err(type_err("close-output-port", "output-port", v)),
    }
}

/// R7RS `(flush-output-port [port])` — flushes any buffered data without
/// closing the port. For our in-memory string/bytevector ports this is a
/// no-op; for file output ports it writes the buffer to disk.
fn b_flush_output_port(args: &[Value]) -> Result<Value, String> {
    if args.len() > 1 {
        return Err(arity_err("flush-output-port", "0 or 1", args.len()));
    }
    let port = match args.first() {
        Some(v) => v.clone(),
        None => return Ok(Value::Unspecified),
    };
    match &port {
        Value::Port(p) => match &**p {
            Port::FileOutput(state) => {
                let s = state.borrow();
                if !s.closed {
                    let path = s.path.clone();
                    let buf = s.buf.clone();
                    drop(s);
                    std::fs::write(&path, &buf)
                        .map_err(|e| format!("flush-output-port: write {} failed: {}", path, e))?;
                }
                Ok(Value::Unspecified)
            }
            // Other output ports hold no buffered OS state — no-op.
            Port::StringOutput(_) | Port::ByteVectorOutput(_) => Ok(Value::Unspecified),
            _ => Err("flush-output-port: not an output port".into()),
        },
        v => Err(type_err("flush-output-port", "output-port", v)),
    }
}

/// R7RS `(input-port-open? port)` — true iff the port is an input port and
/// is still open (not yet closed via close-port / close-input-port).
fn b_input_port_open_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("input-port-open?", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => {
            if !p.is_input() {
                return Ok(Value::Boolean(false));
            }
            // Input ports don't currently track closed state — they're
            // always open until GC. R7RS-conformant behavior.
            Ok(Value::Boolean(true))
        }
        _ => Ok(Value::Boolean(false)),
    }
}

/// R7RS `(output-port-open? port)` — true iff the port is an output port
/// and not yet closed.
fn b_output_port_open_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("output-port-open?", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => {
            if !p.is_output() {
                return Ok(Value::Boolean(false));
            }
            // FileOutput tracks closed; other output ports are always open.
            let open = match &**p {
                Port::FileOutput(state) => !state.borrow().closed,
                _ => true,
            };
            Ok(Value::Boolean(open))
        }
        _ => Ok(Value::Boolean(false)),
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
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
        copied,
    ))))
}

/// `vector-append` (R7RS) — concatenate any number of vectors into a fresh one.
fn b_vector_append(args: &[Value]) -> Result<Value, String> {
    let mut out: Vec<Value> = Vec::new();
    for (i, a) in args.iter().enumerate() {
        match a {
            Value::Vector(v) => out.extend(v.borrow().iter().cloned()),
            other => {
                return Err(format!(
                    "vector-append: argument {} is not a vector ({})",
                    i + 1,
                    other.type_name()
                ))
            }
        }
    }
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
        out,
    ))))
}

/// `subvector` — slice a vector into a fresh one. Same shape as
/// `(vector-copy v start end)` but spelled the way SRFI-43 / Chicken
/// expose it.
fn b_subvector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("subvector", "3", args.len()));
    }
    let v = match &args[0] {
        Value::Vector(v) => v.borrow().clone(),
        other => return Err(type_err("subvector", "vector", other)),
    };
    let start = as_int_i64("subvector", &args[1])? as usize;
    let end = as_int_i64("subvector", &args[2])? as usize;
    if start > v.len() || end > v.len() || start > end {
        return Err("subvector: indices out of range".into());
    }
    let copied: Vec<Value> = v[start..end].to_vec();
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
        copied,
    ))))
}

/// `make-list` (R7RS) — `(make-list k)` or `(make-list k fill)`.
fn b_make_list(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("make-list", "1 or 2", args.len()));
    }
    let n = as_int_i64("make-list", &args[0])?;
    if n < 0 {
        return Err("make-list: negative length".into());
    }
    let fill = if args.len() == 2 {
        args[1].clone()
    } else {
        Value::Unspecified
    };
    let items: Vec<Value> = (0..n).map(|_| fill.clone()).collect();
    Ok(Value::list(items))
}

/// `list-copy` (R7RS) — return a shallow copy of the spine of a list.
/// On a cyclic or improper input, copies up through the dotted tail.
fn b_list_copy(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("list-copy", "1", args.len()));
    }
    let mut elems: Vec<Value> = Vec::new();
    let mut cur = args[0].clone();
    let tail = loop {
        match cur {
            Value::Pair(p) => {
                elems.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            other => break other,
        }
    };
    let mut acc = tail;
    while let Some(e) = elems.pop() {
        acc = Value::Pair(cs_core::Pair::new(e, acc));
    }
    Ok(acc)
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

/// R7RS `(string-fill! str fill [start [end]])` — destructively replaces
/// characters in `str` with `fill`. start/end are character (not byte)
/// indices. Defaults: start=0, end=len.
fn b_string_fill(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 || args.len() > 4 {
        return Err(arity_err("string-fill!", "2..4", args.len()));
    }
    let fill = match &args[1] {
        Value::Character(c) => *c,
        v => return Err(type_err("string-fill!", "character", v)),
    };
    match &args[0] {
        Value::String(s) => {
            let mut chars: Vec<char> = s.borrow().chars().collect();
            let len = chars.len();
            let start = if args.len() >= 3 {
                let i = as_int_i64("string-fill!", &args[2])?;
                if i < 0 || (i as usize) > len {
                    return Err(format!("string-fill!: start out of range: {}", i));
                }
                i as usize
            } else {
                0
            };
            let end = if args.len() == 4 {
                let i = as_int_i64("string-fill!", &args[3])?;
                if i < 0 || (i as usize) > len || (i as usize) < start {
                    return Err(format!("string-fill!: end out of range: {}", i));
                }
                i as usize
            } else {
                len
            };
            for slot in &mut chars[start..end] {
                *slot = fill;
            }
            *s.borrow_mut() = chars.into_iter().collect();
            Ok(Value::Unspecified)
        }
        v => Err(type_err("string-fill!", "string", v)),
    }
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

/// SRFI-13 `string-index-right` — find the rightmost char (or rightmost
/// substring char index) matching the target. Returns #f on no match.
fn b_string_index_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-index-right", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-index-right", "string", v)),
    };
    let target = match &args[1] {
        Value::Character(c) => *c,
        v => return Err(type_err("string-index-right", "character", v)),
    };
    let chars: Vec<char> = s.chars().collect();
    Ok(match chars.iter().rposition(|c| *c == target) {
        Some(i) => Value::fixnum(i as i64),
        None => Value::Boolean(false),
    })
}

/// SRFI-13-flavored `string-contains-right` — find the last char-index
/// where the needle starts within the haystack, or #f.
fn b_string_contains_right(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-contains-right", "2", args.len()));
    }
    let haystack = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-contains-right", "string", v)),
    };
    let needle = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-contains-right", "string", v)),
    };
    Ok(match haystack.rfind(&needle) {
        Some(byte_idx) => {
            let char_idx = haystack[..byte_idx].chars().count() as i64;
            Value::fixnum(char_idx)
        }
        None => Value::Boolean(false),
    })
}

/// `string-replace` — replace the first occurrence of `from` with `to`.
/// Returns the original string when there's no match.
fn b_string_replace(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("string-replace", "3", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-replace", "string", v)),
    };
    let from = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-replace", "string", v)),
    };
    let to = match &args[2] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-replace", "string", v)),
    };
    if from.is_empty() {
        return Err("string-replace: empty pattern".into());
    }
    let out = match s.find(&from) {
        Some(idx) => {
            let mut result = String::with_capacity(s.len() + to.len());
            result.push_str(&s[..idx]);
            result.push_str(&to);
            result.push_str(&s[idx + from.len()..]);
            result
        }
        None => s,
    };
    Ok(Value::string(out))
}

/// `string-replace-all` — replace every occurrence of `from` with `to`.
fn b_string_replace_all(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("string-replace-all", "3", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-replace-all", "string", v)),
    };
    let from = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-replace-all", "string", v)),
    };
    let to = match &args[2] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-replace-all", "string", v)),
    };
    if from.is_empty() {
        return Err("string-replace-all: empty pattern".into());
    }
    Ok(Value::string(s.replace(&from, &to)))
}

/// `string-count` — count occurrences of a character or substring.
fn b_string_count(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-count", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-count", "string", v)),
    };
    let count = match &args[1] {
        Value::Character(c) => s.chars().filter(|x| x == c).count() as i64,
        Value::String(needle) => {
            let needle = needle.borrow();
            if needle.is_empty() {
                0
            } else {
                s.matches(needle.as_str()).count() as i64
            }
        }
        v => return Err(type_err("string-count", "character or string", v)),
    };
    Ok(Value::fixnum(count))
}

// =====================================================================
// R7RS time + process + environment builtins.

/// `current-second` — fractional seconds since the Unix epoch (R7RS).
/// Returns an inexact (flonum) per the spec; clock skews/leap-seconds
/// inherit whatever the OS clock provides.
fn b_current_second(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-second", "0", args.len()));
    }
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("current-second: clock error: {}", e))?;
    Ok(Value::flonum(dur.as_secs_f64()))
}

/// `current-jiffy` — monotonic counter as exact integer (R7RS).
/// We tick at nanosecond resolution → `jiffies-per-second` is 1e9.
fn b_current_jiffy(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-jiffy", "0", args.len()));
    }
    use std::time::Instant;
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    let elapsed = epoch.elapsed().as_nanos();
    if elapsed <= i64::MAX as u128 {
        Ok(Value::fixnum(elapsed as i64))
    } else {
        // Overflow path — once we've been running >292 years.
        Ok(Value::Number(
            Number::parse_decimal_integer(&elapsed.to_string())
                .ok_or_else(|| "current-jiffy: bigint format failure".to_string())?,
        ))
    }
}

/// `jiffies-per-second` — constant 10^9 (we tick in nanoseconds).
fn b_jiffies_per_second(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("jiffies-per-second", "0", args.len()));
    }
    Ok(Value::fixnum(1_000_000_000))
}

/// `get-environment-variable` — returns the value as a string, or #f if unset.
fn b_get_environment_variable(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("get-environment-variable", "1", args.len()));
    }
    let name = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("get-environment-variable", "string", v)),
    };
    Ok(match std::env::var(&name) {
        Ok(v) => Value::string(v),
        Err(_) => Value::Boolean(false),
    })
}

/// `get-environment-variables` — returns an alist `((name . value) ...)`.
fn b_get_environment_variables(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("get-environment-variables", "0", args.len()));
    }
    let pairs: Vec<Value> = std::env::vars()
        .map(|(k, v)| Value::Pair(cs_core::Pair::new(Value::string(k), Value::string(v))))
        .collect();
    Ok(Value::list(pairs))
}

/// `command-line` — returns process argv as a list of strings.
fn b_command_line(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("command-line", "0", args.len()));
    }
    let argv: Vec<Value> = std::env::args().map(Value::string).collect();
    Ok(Value::list(argv))
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
    // R7RS: (string->vector s [start [end]]).
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("string->vector", "1..3", args.len()));
    }
    let chars: Vec<char> = match &args[0] {
        Value::String(s) => s.borrow().chars().collect(),
        v => return Err(type_err("string->vector", "string", v)),
    };
    let len = chars.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("string->vector", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("string->vector: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("string->vector", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("string->vector: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    let v: Vec<Value> = chars[start..end]
        .iter()
        .copied()
        .map(Value::Character)
        .collect();
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(v))))
}

fn b_vector_to_string(args: &[Value]) -> Result<Value, String> {
    // R7RS: (vector->string v [start [end]]).
    if args.is_empty() || args.len() > 3 {
        return Err(arity_err("vector->string", "1..3", args.len()));
    }
    let items: Vec<Value> = match &args[0] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector->string", "vector of characters", v)),
    };
    let len = items.len();
    let start = if args.len() >= 2 {
        let i = as_int_i64("vector->string", &args[1])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("vector->string: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 3 {
        let i = as_int_i64("vector->string", &args[2])?;
        if i < 0 || (i as usize) > len || (i as usize) < start {
            return Err(format!("vector->string: end out of range: {}", i));
        }
        i as usize
    } else {
        len
    };
    let mut s = String::new();
    for item in &items[start..end] {
        match item {
            Value::Character(c) => s.push(*c),
            other => return Err(type_err("vector->string", "character", other)),
        }
    }
    Ok(Value::string(s))
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
    Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
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
                let datum = reader.read(ctx.syms).map_err(|e| {
                    cs_core::stash_builtin_err_extra_tag(TAG_READ_ERROR);
                    format!("read: {}", e.message())
                })?;
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
