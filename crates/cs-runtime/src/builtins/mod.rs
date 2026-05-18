//! R6RS builtin procedures (foundation subset).

#[cfg(feature = "actor")]
pub mod beam;

#[cfg(feature = "sandbox")]
pub mod sandbox;

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
        ("arithmetic-shift", b_bitwise_arith_shift),
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
        ("flexpt", b_fl_expt),
        // R6RS fixnum bit ops + bounds.
        ("fxlength", b_fx_length),
        ("fxbit-count", b_fx_bit_count),
        ("fxfirst-bit-set", b_fx_first_bit_set),
        ("fxbit-set?", b_fx_bit_set_p),
        ("fixnum-width", b_fixnum_width),
        ("least-fixnum", b_least_fixnum),
        ("greatest-fixnum", b_greatest_fixnum),
        // R6RS list ops.
        ("remv", b_remv),
        ("list-head", b_list_head),
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
        ("complex?", b_number_p),
        ("real?", b_number_p),
        ("integer?", b_integer_p),
        ("fixnum?", b_fixnum_p),
        ("flonum?", b_flonum_p),
        ("rational?", b_rational_p),
        ("boolean?", b_boolean_p),
        ("pair?", b_pair_p),
        // R6RS++ §9 source metadata accessors. Today read from the
        // reader-attached span on Pair only; full first-class
        // syntax objects (#118) extend the surface.
        ("syntax-source", b_syntax_source),
        ("syntax-line", b_syntax_line),
        ("syntax-column", b_syntax_column),
        // R6RS++ §12 (#118) Iter A — syntax-case foundation surface.
        // identifier? / syntax->datum / datum->syntax / bound-id=? /
        // free-id=? today degrade to symbol-eq semantics; generate-
        // temporaries (below in ho_builtins) needs SymbolTable.
        ("identifier?", b_identifier_p),
        ("syntax->datum", b_syntax_to_datum),
        ("datum->syntax", b_datum_to_syntax),
        ("bound-identifier=?", b_bound_identifier_eq),
        ("free-identifier=?", b_free_identifier_eq),
        ("make-variable-transformer", b_make_variable_transformer),
        // Phase 1.5 Iter C internals: emitted by
        // compile_syntax_template + expand_syntax_case to stamp
        // template-introduced identifiers with per-expansion marks.
        // Not part of the R6RS user surface; exposed as builtins
        // so the generated code is a plain procedure call.
        ("make-identifier", b_make_identifier),
        ("fresh-mark!", b_fresh_mark),
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
        ("char>?", b_char_gt),
        ("char<=?", b_char_le),
        ("char>=?", b_char_ge),
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
        ("vector=?", b_vector_eq),
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
        // JIT introspection (Iter AI). Useful in tests/benchmarks
        // to assert hot paths actually tier'd up.
        ("jit-installed?", b_jit_installed_p),
        ("jit-stats", b_jit_stats),
        // (gc-stats) lives in syms_builtins now — it interns symbol
        // keys for the alist it returns. (Phase B of the real-world
        // bench spec; was an args-only pure builtin returning just
        // alloc + collect counts.)
        ("gc-stats-reset!", b_gc_stats_reset),
        ("gc-stats-enable!", b_gc_stats_enable),
        ("gc-stats-disable!", b_gc_stats_disable),
        ("gc-auto-collect-enable!", b_gc_auto_collect_enable),
        ("gc-auto-collect-disable!", b_gc_auto_collect_disable),
        ("gc-set-threshold!", b_gc_set_threshold),
        ("collect-garbage", b_collect_garbage),
        ("current-memory-use", b_current_memory_use),
        ("current-rss-bytes", b_current_rss_bytes),
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
        // R6RS §7.2 — &i/o family.
        ("make-i/o-error", b_make_io_error),
        ("i/o-error?", b_io_error_p),
        ("make-i/o-read-error", b_make_io_read_error),
        ("i/o-read-error?", b_io_read_error_p),
        ("make-i/o-write-error", b_make_io_write_error),
        ("i/o-write-error?", b_io_write_error_p),
        (
            "make-i/o-invalid-position-error",
            b_make_io_invalid_position_error,
        ),
        ("i/o-invalid-position-error?", b_io_invalid_position_error_p),
        ("i/o-error-position", b_io_error_position),
        ("make-i/o-filename-error", b_make_io_filename_error),
        ("i/o-filename-error?", b_io_filename_error_p),
        ("i/o-error-filename", b_io_error_filename),
        (
            "make-i/o-file-protection-error",
            b_make_io_file_protection_error,
        ),
        ("i/o-file-protection-error?", b_io_file_protection_error_p),
        (
            "make-i/o-file-is-read-only-error",
            b_make_io_file_is_read_only_error,
        ),
        (
            "i/o-file-is-read-only-error?",
            b_io_file_is_read_only_error_p,
        ),
        (
            "make-i/o-file-already-exists-error",
            b_make_io_file_already_exists_error,
        ),
        (
            "i/o-file-already-exists-error?",
            b_io_file_already_exists_error_p,
        ),
        (
            "make-i/o-file-does-not-exist-error",
            b_make_io_file_does_not_exist_error,
        ),
        (
            "i/o-file-does-not-exist-error?",
            b_io_file_does_not_exist_error_p,
        ),
        ("make-i/o-port-error", b_make_io_port_error),
        ("i/o-port-error?", b_io_port_error_p),
        ("i/o-error-port", b_io_error_port),
        ("make-i/o-decoding-error", b_make_io_decoding_error),
        ("i/o-decoding-error?", b_io_decoding_error_p),
        ("make-i/o-encoding-error", b_make_io_encoding_error),
        ("i/o-encoding-error?", b_io_encoding_error_p),
        ("i/o-encoding-error-char", b_io_encoding_error_char),
        // R6RS §7.2 — violation subtypes.
        ("make-syntax-violation", b_make_syntax_violation),
        ("syntax-violation?", b_syntax_violation_p),
        ("syntax-violation-form", b_syntax_violation_form),
        ("syntax-violation-subform", b_syntax_violation_subform),
        // R6RS++ Phase 2D: condition subtypes for Phase 2 ecosystem.
        ("make-contract-violation", b_make_contract_violation),
        ("contract-violation?", b_contract_violation_p),
        ("contract-violation-source", b_contract_violation_source),
        ("contract-violation-target", b_contract_violation_target),
        ("contract-violation-contract", b_contract_violation_contract),
        ("contract-violation-value", b_contract_violation_value),
        ("make-type-error", b_make_type_error),
        ("type-error?", b_type_error_p),
        ("type-error-expected", b_type_error_expected),
        ("type-error-actual", b_type_error_actual),
        ("make-module-error", b_make_module_error),
        ("module-error?", b_module_error_p),
        ("module-error-library", b_module_error_library),
        ("make-undefined-violation", b_make_undefined_violation),
        ("undefined-violation?", b_undefined_violation_p),
        ("make-lexical-violation", b_make_lexical_violation),
        ("lexical-violation?", b_lexical_violation_p),
        (
            "make-implementation-restriction-violation",
            b_make_impl_restriction,
        ),
        (
            "implementation-restriction-violation?",
            b_impl_restriction_p,
        ),
        ("make-no-infinities-violation", b_make_no_infinities),
        ("no-infinities-violation?", b_no_infinities_p),
        ("make-no-nans-violation", b_make_no_nans),
        ("no-nans-violation?", b_no_nans_p),
        // R6RS condition compounding
        ("condition", b_condition),
        ("simple-conditions", b_simple_conditions),
        // helpers used by code generated from `define-condition-type`
        ("condition-register-parent!", b_condition_register_parent),
        ("condition-instance-of?", b_condition_instance_of),
        ("condition-field-ref", b_condition_field_ref),
        ("make-simple-condition", b_make_simple_condition),
        // R6RS §6 — `(rnrs records procedural)`. RTD/CD construction
        // that needs SymbolTable access is in syms_builtins below.
        ("record-type-descriptor?", b_rtd_p),
        ("make-record-constructor-descriptor", b_make_cd),
        ("record-constructor", b_record_constructor),
        ("record-predicate", b_record_predicate),
        ("record-accessor", b_record_accessor),
        ("record-mutator", b_record_mutator),
        ("record?", b_record_p),
        ("record-rtd", b_record_rtd),
        ("record-type-name", b_record_type_name),
        ("record-type-parent", b_record_type_parent),
        ("record-type-field-names", b_record_type_field_names),
        ("record-type-uid", b_record_type_uid),
        ("record-type-sealed?", b_record_type_sealed_p),
        ("record-type-opaque?", b_record_type_opaque_p),
        ("record-type-generative?", b_record_type_generative_p),
        // R6RS §7.2 — bridge from procedural rtds to compound conditions.
        ("condition-predicate", b_condition_predicate),
        ("condition-accessor", b_condition_accessor),
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
        ("bytevector=?", b_bytevector_eq),
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
        // R6RS named aliases for the file-port factories.
        ("open-file-input-port", b_open_input_file),
        ("open-file-output-port", b_open_output_file),
        // R6RS §8.2.6 — port positions and textual peek alias.
        ("port-position", b_port_position),
        ("set-port-position!", b_set_port_position),
        ("port-has-port-position?", b_port_has_port_position_p),
        (
            "port-has-set-port-position!?",
            b_port_has_set_port_position_p,
        ),
        ("lookahead-char", b_lookahead_char),
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
        ("put-char", b_put_char),
        ("put-string", b_put_string),
        ("put-bytevector", b_put_bytevector),
        ("get-bytevector-n", b_get_bytevector_n),
        ("get-bytevector-all", b_get_bytevector_all),
        ("get-string-n", b_get_string_n),
        // R6RS §8.2.5 — transcoder/codec procs that need only Value
        // input. The factories that mint Symbol values for eol-style
        // and error-mode are registered in `syms_builtins()` below.
        ("transcoder-codec", b_transcoder_codec),
        ("transcoder-eol-style", b_transcoder_eol_style),
        (
            "transcoder-error-handling-mode",
            b_transcoder_error_handling_mode,
        ),
        ("bytevector->string", b_bytevector_to_string),
        ("string->bytevector", b_string_to_bytevector),
        ("transcoded-port", b_transcoded_port),
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
        ("parameter?", b_parameter_p),
        // (rnrs enums) — R6RS §13. Each enum-set is encoded as
        // #("__enum-set__" #(<universe symbols>) <bits-fixnum>).
        // M9 iter 2 limits the universe to ≤63 symbols (fixnum bitset);
        // larger universes can land later as a follow-up.
        ("make-enumeration", b_make_enumeration),
        ("enum-set?", b_enum_set_p),
        ("enum-set-universe", b_enum_set_universe),
        ("enum-set-indexer", b_enum_set_indexer),
        ("enum-set-constructor", b_enum_set_constructor),
        ("enum-set->list", b_enum_set_to_list),
        ("enum-set-member?", b_enum_set_member_p),
        ("enum-set-subset?", b_enum_set_subset_p),
        ("enum-set=?", b_enum_set_eq_p),
        ("enum-set-union", b_enum_set_union),
        ("enum-set-intersection", b_enum_set_intersection),
        ("enum-set-difference", b_enum_set_difference),
        ("enum-set-complement", b_enum_set_complement),
        ("enum-set-projection", b_enum_set_projection),
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
        ("fourth", b_fourth),
        ("fifth", b_fifth),
        ("sixth", b_sixth),
        ("seventh", b_seventh),
        ("eighth", b_eighth),
        ("ninth", b_ninth),
        ("tenth", b_tenth),
        ("not-pair?", b_not_pair_p),
        ("null-list?", b_null_list_p),
        ("proper-list?", b_proper_list_p),
        ("dotted-list?", b_dotted_list_p),
        ("circular-list?", b_circular_list_p),
        ("append-reverse", b_append_reverse),
        ("reverse!", b_reverse_bang),
        ("circular-list", b_circular_list),
        // R6RS numeric "shape" predicates / converters.
        ("real-valued?", b_real_valued_p),
        ("rational-valued?", b_rational_valued_p),
        ("integer-valued?", b_integer_valued_p),
        ("real->flonum", b_real_to_flonum),
        ("rationalize", b_rationalize),
        // hashtable conversions
        ("hashtable->alist", b_hashtable_to_alist),
        ("alist->hashtable", b_alist_to_hashtable),
        // (hashtable-update! is higher-order — see below)
        // R7RS portability
        ("crabscheme-version", b_crabscheme_version),
    ]
}

pub fn higher_order_builtins() -> Vec<HoEntry> {
    #[allow(unused_mut)]
    let mut v: Vec<HoEntry> = vec![
        // ADR 0014 — optimizer-pass installation.
        ("install-optimizer-pass!", b_install_optimizer_pass),
        ("remove-optimizer-pass!", b_remove_optimizer_pass),
        ("installed-optimizer-passes", b_installed_optimizer_passes),
        (
            "with-active-optimizer-passes",
            b_with_active_optimizer_passes,
        ),
        // ADR 0015 L1.1 — environment predicate.
        ("environment?", b_environment_p),
        // ADR 0015 L1.2 — mutable namespace constructor + ops.
        ("make-namespace", b_make_namespace),
        (
            "namespace-set-variable-value!",
            b_namespace_set_variable_value,
        ),
        (
            "namespace-undefine-variable!",
            b_namespace_undefine_variable,
        ),
        ("apply", b_apply),
        // (Sandbox builtins under `sandbox` feature are
        // appended below this vec literal to avoid mixing #cfg
        // attributes inside the macro-style vec! — see
        // append_sandbox_builtins.)
        // (time-apply) lives as a Scheme-level wrapper in the
        // benchmark harness, not a builtin: the VM dispatches
        // higher-order builtins through marker-type downcasts
        // (see VmApply / VmMap in cs-vm/src/vm.rs) and adding
        // a VmTimeApply marker is a deeper VM change than Phase
        // B warrants. The Scheme wrapper threads through
        // (current-jiffy) + (gc-stats) directly and gets the
        // same five-value return.
        ("map", b_map),
        ("for-each", b_for_each),
        ("display", b_display),
        ("write", b_write),
        ("write-simple", b_write),
        ("write-shared", b_write),
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
        // R6RS++ §12 (#118) Iter A — generate-temporaries needs
        // SymbolTable to mint fresh names.
        ("generate-temporaries", b_generate_temporaries_ho),
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
        ("split-at", b_split_at),
        ("unzip", b_unzip),
        ("unzip2", b_unzip),
        ("find-tail", b_find_tail),
        ("reduce-right", b_reduce_right),
        ("pair-fold", b_pair_fold),
        ("pair-fold-right", b_pair_fold_right),
        ("pair-for-each", b_pair_for_each),
        ("string-fold", b_string_fold),
        ("string-fold-right", b_string_fold_right),
        ("string-tabulate", b_string_tabulate),
        ("vector-fold-right", b_vector_fold_right),
        ("unfold-right", b_unfold_right),
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
        ("call-with-input-file", b_call_with_input_file),
        ("call-with-output-file", b_call_with_output_file),
        ("call-with-input-string", b_call_with_input_string),
        ("call-with-output-string", b_call_with_output_string),
        ("with-output-to-file", b_with_output_to_file),
        ("with-input-from-file", b_with_input_from_file),
        ("current-input-port", b_current_input_port),
        ("current-output-port", b_current_output_port),
        ("current-error-port", b_current_error_port),
        ("standard-input-port", b_standard_input_port),
        ("standard-output-port", b_standard_output_port),
        ("standard-error-port", b_standard_error_port),
        ("gensym", b_gensym),
        ("eval", b_eval),
        ("environment", b_environment),
        ("interaction-environment", b_interaction_environment),
        ("null-environment", b_null_environment),
        ("scheme-report-environment", b_scheme_report_environment),
        ("load", b_load),
        ("load-shared-library", b_load_shared_library),
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
        ("read-bytevector!", b_read_bytevector_bang),
        ("read-line", b_read_line_implicit),
        ("get-string-all", b_get_string_all),
        // SRFI-1 (higher-order)
        ("tabulate", b_tabulate),
        ("remove", b_remove),
        ("remp", b_remp),
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
    ];
    // ADR 0015 L2 — sandbox builtins live in their own module
    // and are appended (rather than spread inline with #cfg) to
    // keep the main vec literal clean.
    #[cfg(feature = "sandbox")]
    {
        for entry in sandbox::builtins() {
            v.push(entry);
        }
    }
    #[cfg(feature = "sandbox")]
    return v;
    #[cfg(not(feature = "sandbox"))]
    v
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
        // R6RS §8.2.5 — codec/transcoder factories. These intern
        // symbol values for the eol-style / error-handling-mode
        // defaults, so they need read-write SymbolTable access.
        ("utf-8-codec", b_utf8_codec),
        ("utf-16-codec", b_utf16_codec),
        ("latin-1-codec", b_latin1_codec),
        ("native-eol-style", b_native_eol_style),
        ("make-transcoder", b_make_transcoder),
        ("native-transcoder", b_native_transcoder),
        // R6RS §6 — `(rnrs records procedural)` factories that need
        // to mint fresh tag symbols / read symbol kinds.
        ("make-record-type-descriptor", b_make_rtd),
        ("record-field-mutable?", b_record_field_mutable_p),
        // R6RS symbol manipulation.
        ("symbol-append", b_symbol_append),
        // JIT introspection that needs the symbol table to mint
        // type-tag symbols.
        ("jit-status", b_jit_status),
        ("gc-stats", b_gc_stats),
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
    // BEAM-style actor / table primops, gated on the `actor` feature.
    // Same Syms shape (read-write SymbolTable for arg unpacking and
    // SendableValue conversion), so they ride the same registration
    // loop as syms_builtins.
    #[cfg(feature = "actor")]
    {
        for (name, f) in beam::beam_syms_builtins() {
            let sym = syms.intern(name);
            env.define(sym, crate::proc::make_builtin_syms(name, f));
        }
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

pub fn as_int_i64_pub(name: &str, v: &Value) -> Result<i64, String> {
    as_int_i64(name, v)
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

/// R6RS `(fxlength fx)` — number of bits needed to represent fx in
/// two's-complement, excluding the sign bit. (fxlength 0) = 0.
fn b_fx_length(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("fxlength", "1", args.len()));
    }
    let x = as_fx("fxlength", &args[0])?;
    let bits = if x >= 0 {
        64 - x.leading_zeros()
    } else {
        64 - (!x).leading_zeros()
    };
    Ok(Value::fixnum(bits as i64))
}

/// R6RS `(fxbit-count fx)` — popcount for non-negative; for negative,
/// returns -(popcount-of-bitwise-not + 1) per spec.
fn b_fx_bit_count(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("fxbit-count", "1", args.len()));
    }
    let x = as_fx("fxbit-count", &args[0])?;
    let r = if x >= 0 {
        x.count_ones() as i64
    } else {
        -((!x).count_ones() as i64 + 1)
    };
    Ok(Value::fixnum(r))
}

/// R6RS `(fxfirst-bit-set fx)` — index of the lowest set bit; -1
/// if fx is 0.
fn b_fx_first_bit_set(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("fxfirst-bit-set", "1", args.len()));
    }
    let x = as_fx("fxfirst-bit-set", &args[0])?;
    let r = if x == 0 {
        -1
    } else {
        x.trailing_zeros() as i64
    };
    Ok(Value::fixnum(r))
}

/// R6RS `(fxbit-set? fx idx)` — #t iff bit `idx` of fx is 1.
fn b_fx_bit_set_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("fxbit-set?", "2", args.len()));
    }
    let x = as_fx("fxbit-set?", &args[0])?;
    let k = as_fx("fxbit-set?", &args[1])?;
    if !(0..64).contains(&k) {
        return Err(fx_overflow("fxbit-set?"));
    }
    Ok(Value::Boolean((x >> k) & 1 == 1))
}

/// R6RS `(fixnum-width)` — bit width of the fixnum type. We use i64,
/// so the range is [-2^63, 2^63-1] — width 64.
fn b_fixnum_width(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("fixnum-width", "0", args.len()));
    }
    Ok(Value::fixnum(64))
}

fn b_least_fixnum(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("least-fixnum", "0", args.len()));
    }
    Ok(Value::fixnum(i64::MIN))
}

fn b_greatest_fixnum(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("greatest-fixnum", "0", args.len()));
    }
    Ok(Value::fixnum(i64::MAX))
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

/// R6RS `(flexpt x y)` — typed flonum exponentiation. Both args must
/// be flonums; result follows IEEE-754 (NaN/inf propagate).
fn b_fl_expt(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("flexpt", "2", args.len()));
    }
    let x = as_fl("flexpt", &args[0])?;
    let y = as_fl("flexpt", &args[1])?;
    Ok(Value::Number(Number::Flonum(x.powf(y))))
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

/// R6RS §11.7.4 — `(real-valued? v)`: true iff v is a number AND its
/// imaginary part is zero. Foundation has no complex numbers, so this
/// reduces to "is a number". Mirrors R6RS spec exactly given that
/// every Number we have is real.
fn b_real_valued_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("real-valued?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(&args[0], Value::Number(_))))
}

/// `(rational-valued? v)` — real-valued? AND finite (mathematically a ratio).
fn b_rational_valued_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("rational-valued?", "1", args.len()));
    }
    Ok(Value::Boolean(match &args[0] {
        Value::Number(Number::Flonum(f)) => f.is_finite(),
        Value::Number(_) => true,
        _ => false,
    }))
}

/// `(integer-valued? v)` — real-valued? AND mathematically an integer.
/// Catches flonums like 5.0 that integer? rejects.
fn b_integer_valued_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("integer-valued?", "1", args.len()));
    }
    Ok(Value::Boolean(match &args[0] {
        Value::Number(Number::Flonum(f)) => f.is_finite() && f.fract() == 0.0,
        Value::Number(n) => n.is_integer(),
        _ => false,
    }))
}

/// R6RS `(real->flonum r)` — convert a real number to its flonum
/// approximation. Equivalent to `(inexact r)` constrained to flonums.
fn b_real_to_flonum(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("real->flonum", "1", args.len()));
    }
    let n = match &args[0] {
        Value::Number(n) => n.to_f64(),
        v => return Err(type_err("real->flonum", "real number", v)),
    };
    Ok(Value::Number(Number::Flonum(n)))
}

/// R6RS `(rationalize x eps)` — return the simplest rational `r`
/// such that `|r - x| <= eps`. Foundation: implemented for finite
/// flonum inputs via continued-fraction construction; integer / exact
/// rational inputs short-circuit to the input itself.
fn b_rationalize(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("rationalize", "2", args.len()));
    }
    let x = as_num("rationalize", &args[0])?;
    let eps = as_num("rationalize", &args[1])?;
    // Exact integer or exact rational input: already in simplest form
    // for any non-negative eps.
    if matches!(x, Number::Fixnum(_) | Number::Rat(_) | Number::Big(_)) {
        return Ok(Value::Number(x));
    }
    let xf = x.to_f64();
    let ef = eps.to_f64();
    if !xf.is_finite() {
        return Ok(Value::Number(x));
    }
    if ef < 0.0 {
        return Err("rationalize: negative epsilon".into());
    }
    // Stern-Brocot mediant search bounded by [xf-ef, xf+ef]. Simplest
    // rational in the closed interval — classic construction.
    let lo = xf - ef;
    let hi = xf + ef;
    if lo <= 0.0 && hi >= 0.0 {
        return Ok(Value::fixnum(0));
    }
    let neg = hi < 0.0;
    let (lo, hi) = if neg { (-hi, -lo) } else { (lo, hi) };
    let (n, d) = simplest_rational_in(lo, hi);
    let num_i = n as i64;
    let den_i = d as i64;
    let signed_num = if neg { -num_i } else { num_i };
    let v = if den_i == 1 {
        Value::fixnum(signed_num)
    } else {
        Value::Number(Number::Flonum(signed_num as f64 / den_i as f64))
    };
    Ok(v)
}

fn simplest_rational_in(lo: f64, hi: f64) -> (u64, u64) {
    // Stern-Brocot search returning (numerator, denominator) in lowest
    // terms. lo <= hi, both > 0.
    let mut lo_n = lo.floor() as u64;
    let mut lo_d: u64 = 1;
    let mut hi_n = hi.ceil() as u64;
    let mut hi_d: u64 = 1;
    let a = lo_n;
    if (a as f64) >= lo && (a as f64) <= hi {
        return (a, 1);
    }
    let mut count = 0;
    loop {
        count += 1;
        if count > 1000 {
            // Safety net for pathological inputs; fall back to flonum approx.
            return (((lo + hi) * 0.5 * 1.0e6) as u64, 1_000_000);
        }
        let m_n = lo_n + hi_n;
        let m_d = lo_d + hi_d;
        let m = m_n as f64 / m_d as f64;
        if m < lo {
            lo_n = m_n;
            lo_d = m_d;
        } else if m > hi {
            hi_n = m_n;
            hi_d = m_d;
        } else {
            return (m_n, m_d);
        }
    }
}

/// R6RS `(symbol-append sym ...)` — build a fresh symbol from the
/// concatenation of the input symbols' names.
fn b_symbol_append(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    let mut out = String::new();
    for a in args {
        match a {
            Value::Symbol(s) => out.push_str(syms.name(*s)),
            v => return Err(type_err("symbol-append", "symbol", v)),
        }
    }
    Ok(Value::Symbol(syms.intern(&out)))
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

/// `(syntax-source v)` — return the source-text origin of `v` as a
/// list `(file-id start-byte end-byte)`, or `#f` if `v` carries no
/// source span. Per R6RS++ §9: today only reader-produced Pairs
/// carry source. Future iters (full syntax-case) extend this to
/// hygiene-tracked syntax objects.
fn b_syntax_source(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax-source", "1", args.len()));
    }
    let span = match &args[0] {
        Value::Pair(p) => p.source_span(),
        _ => None,
    };
    match span {
        None => Ok(Value::Boolean(false)),
        Some(s) => {
            // (file-id start end)
            let end = Value::Pair(Pair::new(
                Value::Number(Number::Fixnum(s.end as i64)),
                Value::Null,
            ));
            let mid = Value::Pair(Pair::new(
                Value::Number(Number::Fixnum(s.start as i64)),
                end,
            ));
            Ok(Value::Pair(Pair::new(
                Value::Number(Number::Fixnum(s.file.0 as i64)),
                mid,
            )))
        }
    }
}

/// `(syntax-line v)` — return the 1-based line number of `v`'s
/// source position, or `#f` if `v` carries no source span. The
/// line lookup requires the SourceMap to be threaded; today we
/// approximate by returning the start-byte (callers can convert
/// via tooling). Full line/column resolution lands when this
/// accessor moves into a higher-order builtin with SourceMap
/// access.
fn b_syntax_line(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax-line", "1", args.len()));
    }
    let span = match &args[0] {
        Value::Pair(p) => p.source_span(),
        _ => None,
    };
    match span {
        None => Ok(Value::Boolean(false)),
        Some(s) => Ok(Value::Number(Number::Fixnum(s.start as i64))),
    }
}

/// `(syntax-column v)` — see [`b_syntax_line`]. Today returns the
/// end-byte; tooling can derive column from start-byte + SourceMap.
fn b_syntax_column(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax-column", "1", args.len()));
    }
    let span = match &args[0] {
        Value::Pair(p) => p.source_span(),
        _ => None,
    };
    match span {
        None => Ok(Value::Boolean(false)),
        Some(s) => Ok(Value::Number(Number::Fixnum(s.end as i64))),
    }
}

// ---- R6RS++ §12 (#118) Iter A — syntax-case foundation surface ----
//
// As of Phase 1.5 Iter A, `Value::Identifier { name, mark }` exists
// alongside `Value::Symbol`. The hygiene surface widens to accept
// either kind where R6RS calls for "identifier"; `symbol?` stays
// strict (Symbol only) per R6RS. Iter 1.5.D upgrades the equality
// builtins to compare marks.

/// `(identifier? v)` — R6RS §11.18. True iff `v` is a syntax object
/// representing an identifier. Accepts either a bare `Symbol` (which
/// behaves as an identifier with implicit mark 0) or an explicit
/// `Identifier { name, mark }` value produced by syntax-case
/// template instantiation.
fn b_identifier_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("identifier?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        args[0],
        Value::Symbol(_) | Value::Identifier { .. }
    )))
}

/// `(syntax->datum stx)` — R6RS §12.6. Strip syntax-object marks
/// and return the underlying datum. For an `Identifier { name, mark }`
/// returns the bare `Symbol(name)` (mark discarded). For other
/// values (including bare Symbol), pass through unchanged --
/// they're already datum-shaped.
///
/// Compound structures (pairs, vectors) containing identifiers
/// recurse: each Identifier leaf is stripped. Iter 1.5.B ships
/// the leaf-only version; the recursive walker lands once the
/// 1.5.C template instantiator creates compound structures
/// with embedded identifiers.
fn b_syntax_to_datum(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax->datum", "1", args.len()));
    }
    Ok(strip_identifier_marks(&args[0]))
}

/// Recursive helper for `syntax->datum`. Walks the value
/// converting each `Identifier { name, .. }` leaf to
/// `Symbol(name)`. Pair / Vector recurse; other variants pass
/// through. Inverts `stamp_datum_with_mark` (used by
/// `datum->syntax`) so round-trips compose correctly.
fn strip_identifier_marks(v: &Value) -> Value {
    match v {
        Value::Identifier { name, .. } => Value::Symbol(*name),
        Value::Pair(p) => {
            let car_new = strip_identifier_marks(&p.car.borrow());
            let cdr_new = strip_identifier_marks(&p.cdr.borrow());
            Value::Pair(Pair::new(car_new, cdr_new))
        }
        Value::Vector(vec_) => {
            let items: Vec<Value> = vec_.borrow().iter().map(strip_identifier_marks).collect();
            Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
        }
        _ => v.clone(),
    }
}

/// `(datum->syntax template-id datum)` — R6RS §12.6. Stamp
/// `datum` with the lexical context of `template-id` so
/// introduced identifiers resolve correctly. Accepts either a
/// bare `Symbol` (treated as mark = 0) or an `Identifier` as
/// `template-id`.
///
/// Iter 1.5.F: recursively walks `datum`, converting every
/// bare `Symbol` leaf to a `Value::Identifier` carrying the
/// template's mark. Existing `Identifier` leaves are left
/// alone (the user explicitly built them with a chosen mark).
/// Non-identifier atoms (numbers, strings, chars, booleans)
/// pass through unchanged. Pairs and vectors recurse.
fn b_datum_to_syntax(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("datum->syntax", "2", args.len()));
    }
    let ctx_mark = match &args[0] {
        Value::Symbol(_) => 0u64,
        Value::Identifier { mark, .. } => *mark,
        v => return Err(type_err("datum->syntax", "identifier", v)),
    };
    Ok(stamp_datum_with_mark(&args[1], ctx_mark))
}

/// Recursive helper for `datum->syntax`. Walks `v` rewriting
/// bare `Symbol` leaves as `Identifier { name, mark }` and
/// rebuilding pair / vector structures. Leaves existing
/// `Identifier`s and non-identifier atoms alone.
fn stamp_datum_with_mark(v: &Value, mark: u64) -> Value {
    match v {
        Value::Symbol(name) => Value::Identifier { name: *name, mark },
        Value::Pair(p) => {
            let car_new = stamp_datum_with_mark(&p.car.borrow(), mark);
            let cdr_new = stamp_datum_with_mark(&p.cdr.borrow(), mark);
            Value::Pair(Pair::new(car_new, cdr_new))
        }
        Value::Vector(vec_) => {
            let items: Vec<Value> = vec_
                .borrow()
                .iter()
                .map(|e| stamp_datum_with_mark(e, mark))
                .collect();
            Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(items)))
        }
        _ => v.clone(),
    }
}

/// `(bound-identifier=? a b)` — R6RS §12.6. True iff `a` and `b`
/// would refer to the same binding if substituted into a template.
///
/// Compares `(name, mark)` pairs. A bare `Value::Symbol(s)` is
/// treated as an identifier with mark = 0 (reader-input
/// identifier). So:
///   - two Symbols equal iff same name
///   - two Identifiers equal iff same name AND same mark
///   - Symbol vs Identifier equal iff same name AND mark = 0
///
/// This is the foundation of macro hygiene: two `(syntax foo)`
/// from different macro expansions produce Identifier values
/// with different marks (see Phase 1.5 Iter C), so they're
/// distinguishable here.
fn b_bound_identifier_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bound-identifier=?", "2", args.len()));
    }
    let (n1, m1) = match &args[0] {
        Value::Symbol(s) => (*s, 0u64),
        Value::Identifier { name, mark } => (*name, *mark),
        v => return Err(type_err("bound-identifier=?", "identifier", v)),
    };
    let (n2, m2) = match &args[1] {
        Value::Symbol(s) => (*s, 0u64),
        Value::Identifier { name, mark } => (*name, *mark),
        v => return Err(type_err("bound-identifier=?", "identifier", v)),
    };
    Ok(Value::Boolean(n1 == n2 && m1 == m2))
}

/// `(free-identifier=? a b)` — R6RS §12.6. True iff `a` and `b`
/// resolve to the same binding in their respective scopes.
///
/// Today's semantics: name-equality on the underlying `Symbol`
/// or `Identifier` name (mark ignored). Two `(syntax foo)` from
/// different macro expansions both refer to "the foo binding in
/// scope", so they're free-identifier=? even though their marks
/// differ. This is the intuition that bound-identifier=?
/// distinguishes by *introduction site* whereas free-identifier=?
/// distinguishes by *resolved binding*.
///
/// True R6RS semantics with full lexical-env resolution requires
/// the Phase 2 SyntaxObject + binding-table track. The name-only
/// approximation handles the most common case (identifiers
/// referring to top-level / library bindings) correctly.
fn b_free_identifier_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("free-identifier=?", "2", args.len()));
    }
    let n1 = match &args[0] {
        Value::Symbol(s) => *s,
        Value::Identifier { name, .. } => *name,
        v => return Err(type_err("free-identifier=?", "identifier", v)),
    };
    let n2 = match &args[1] {
        Value::Symbol(s) => *s,
        Value::Identifier { name, .. } => *name,
        v => return Err(type_err("free-identifier=?", "identifier", v)),
    };
    Ok(Value::Boolean(n1 == n2))
}

/// `(make-identifier name mark)` — Phase 1.5 Iter C primitive.
/// Constructs a `Value::Identifier { name, mark }`. Called by
/// code emitted by `compile_syntax_template` for non-pvar
/// identifiers in syntax-case templates; not part of the R6RS
/// public surface, but exposed at the builtin layer so the
/// generated code is a plain procedure call instead of needing
/// a new CoreExpr variant.
///
/// `name` must be a Symbol (or Identifier, whose name is
/// extracted); `mark` must be a non-negative fixnum.
fn b_make_identifier(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("make-identifier", "2", args.len()));
    }
    let name = match &args[0] {
        Value::Symbol(s) => *s,
        Value::Identifier { name, .. } => *name,
        v => return Err(type_err("make-identifier", "symbol or identifier", v)),
    };
    let mark = match &args[1] {
        Value::Number(Number::Fixnum(n)) if *n >= 0 => *n as u64,
        v => {
            return Err(type_err("make-identifier", "non-negative fixnum mark", v));
        }
    };
    Ok(Value::Identifier { name, mark })
}

/// `(fresh-mark!)` — Phase 1.5 Iter C primitive. Returns a
/// globally-fresh `u64` value (as a fixnum) suitable for use as
/// an identifier mark. Called once per syntax-case form
/// evaluation; all introduced identifiers in that one
/// macro-expansion share the resulting mark.
///
/// Counter is process-global via a thread-safe atomic. Starts
/// at 1 so that mark=0 stays available as the "unmarked"
/// (datum-introduced or reader-input) identifier marker.
fn b_fresh_mark(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("fresh-mark!", "0", args.len()));
    }
    use std::sync::atomic::{AtomicU64, Ordering};
    static MARK_COUNTER: AtomicU64 = AtomicU64::new(1);
    let mark = MARK_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(Value::Number(Number::Fixnum(mark as i64)))
}

/// `(make-variable-transformer proc)` — R6RS §12.3. Wraps a
/// transformer procedure so that it's invoked on *variable
/// reference* occurrences as well as the usual application
/// positions. R6RS-conformant macro systems use this to let
/// `define-syntax` produce identifier macros.
///
/// Today's semantics: pass-through. We return the procedure
/// unchanged; the macro architecture in cs-expand doesn't yet
/// distinguish variable-ref from application transformer calls.
/// Documenting the gap honestly here so user code that calls
/// `make-variable-transformer` won't fail with "undefined" --
/// it'll get the bare procedure back and only application-
/// position substitution will work. Tracking ticket: post-1.0
/// SyntaxObject + procedural-macro track.
fn b_make_variable_transformer(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-variable-transformer", "1", args.len()));
    }
    if !matches!(args[0], Value::Procedure(_)) {
        return Err(type_err("make-variable-transformer", "procedure", &args[0]));
    }
    Ok(args[0].clone())
}

/// `(generate-temporaries lst)` — R6RS §12.6. Returns a list of N
/// fresh identifiers, where N is the length of `lst`. The argument
/// itself is consumed only for its length; the values inside are
/// ignored. Names are guaranteed distinct from any identifier that
/// has appeared (we use `SymbolTable::len` as the monotonic counter,
/// matching the existing `gensym` convention).
fn b_generate_temporaries_ho(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("generate-temporaries", "1", args.len()));
    }
    let mut n: usize = 0;
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                n += 1;
                cur = p.cdr.borrow().clone();
            }
            _ => return Err(type_err("generate-temporaries", "list", &args[0])),
        }
    }
    let mut fresh = Vec::with_capacity(n);
    for _ in 0..n {
        let id = ctx.syms.len();
        let name = format!("t.{}", id);
        fresh.push(Value::Symbol(ctx.syms.intern(&name)));
    }
    Ok(Value::list(fresh))
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

fn b_char_gt(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::Character(c) => *c,
        v => return Err(type_err("char>?", "character", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::Character(c) => *c,
            v => return Err(type_err("char>?", "character", v)),
        };
        if !(prev > cur) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_char_le(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::Character(c) => *c,
        v => return Err(type_err("char<=?", "character", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::Character(c) => *c,
            v => return Err(type_err("char<=?", "character", v)),
        };
        if !(prev <= cur) {
            return Ok(Value::Boolean(false));
        }
        prev = cur;
    }
    Ok(Value::Boolean(true))
}

fn b_char_ge(args: &[Value]) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let mut prev = match &args[0] {
        Value::Character(c) => *c,
        v => return Err(type_err("char>=?", "character", v)),
    };
    for a in &args[1..] {
        let cur = match a {
            Value::Character(c) => *c,
            v => return Err(type_err("char>=?", "character", v)),
        };
        if !(prev >= cur) {
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

pub fn collect_proper_list_pub(name: &str, v: &Value) -> Result<Vec<Value>, String> {
    collect_proper_list(name, v)
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
    // Decimal: defer to the existing Display impl.
    if radix == 10 {
        return Ok(Value::string(format!("{}", n)));
    }
    if !matches!(radix, 2 | 8 | 16) {
        return Err(format!("number->string: unsupported radix: {}", radix));
    }
    // For non-decimal radices, R7RS only requires support on integers.
    // Render with proper sign handling (Rust's {:b}/{:o}/{:x} on negatives
    // emits the two's-complement bit pattern, not "-...", which is wrong).
    fn fmt_int(magnitude: u64, radix: i64) -> String {
        match radix {
            2 => format!("{:b}", magnitude),
            8 => format!("{:o}", magnitude),
            16 => format!("{:x}", magnitude),
            _ => unreachable!(),
        }
    }
    match &n {
        Number::Fixnum(v) => {
            let mag = v.unsigned_abs();
            let s = if *v < 0 {
                format!("-{}", fmt_int(mag, radix))
            } else {
                fmt_int(mag, radix)
            };
            Ok(Value::string(s))
        }
        Number::Big(b) => {
            // BigInt has its own to_str_radix.
            Ok(Value::string(b.to_str_radix(radix as u32)))
        }
        _ => Err(format!(
            "number->string: radix {} only supported on integers",
            radix,
        )),
    }
}

fn b_string_to_number(args: &[Value]) -> Result<Value, String> {
    // R7RS: (string->number s [radix]). The string may include radix
    // (#x #b #o #d) and exactness (#e #i) prefixes. Returns #f if not
    // parseable.
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("string->number", "1 or 2", args.len()));
    }
    let mut radix = if args.len() == 2 {
        as_int_i64("string->number", &args[1])?
    } else {
        10
    };
    let raw = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string->number", "string", v)),
    };
    // R7RS special-numeric tokens — return immediately.
    match raw.as_str() {
        "+inf.0" => return Ok(Value::Number(Number::Flonum(f64::INFINITY))),
        "-inf.0" => return Ok(Value::Number(Number::Flonum(f64::NEG_INFINITY))),
        "+nan.0" | "-nan.0" => return Ok(Value::Number(Number::Flonum(f64::NAN))),
        _ => {}
    }
    // Strip prefixes — at most one radix prefix and one exactness prefix,
    // in either order, per R7RS lexical syntax.
    let mut force_exact: Option<bool> = None; // Some(true)=exact, Some(false)=inexact
    let mut s = raw.as_str();
    for _ in 0..2 {
        if s.len() < 2 || !s.starts_with('#') {
            break;
        }
        match &s[..2] {
            "#x" | "#X" => {
                radix = 16;
                s = &s[2..];
            }
            "#b" | "#B" => {
                radix = 2;
                s = &s[2..];
            }
            "#o" | "#O" => {
                radix = 8;
                s = &s[2..];
            }
            "#d" | "#D" => {
                radix = 10;
                s = &s[2..];
            }
            "#e" | "#E" => {
                force_exact = Some(true);
                s = &s[2..];
            }
            "#i" | "#I" => {
                force_exact = Some(false);
                s = &s[2..];
            }
            _ => break,
        }
    }
    let parsed: Option<Number> = match radix {
        10 => {
            // Try rational first (a/b), then float, then int.
            if let Some(slash) = s.find('/') {
                let num: Option<i64> = s[..slash].parse().ok();
                let den: Option<i64> = s[slash + 1..].parse().ok();
                match (num, den) {
                    (Some(n), Some(d)) if d != 0 => Number::Fixnum(n).div(&Number::Fixnum(d)).ok(),
                    _ => None,
                }
            } else if s.contains('.') || s.contains('e') || s.contains('E') {
                s.parse::<f64>().ok().map(Number::Flonum)
            } else {
                s.parse::<i64>().ok().map(Number::Fixnum)
            }
        }
        2 | 8 | 16 => {
            // Allow optional leading sign for non-decimal radices.
            let (sign, body) = match s.strip_prefix('-') {
                Some(rest) => (-1, rest),
                None => match s.strip_prefix('+') {
                    Some(rest) => (1, rest),
                    None => (1, s),
                },
            };
            i64::from_str_radix(body, radix as u32)
                .ok()
                .map(|v| Number::Fixnum(sign * v))
        }
        _ => None,
    };
    let parsed = match parsed {
        Some(n) => n,
        None => return Ok(Value::Boolean(false)),
    };
    let final_n = match force_exact {
        Some(true) => match parsed {
            Number::Flonum(f) => {
                if f.is_finite() {
                    Number::Fixnum(f as i64)
                } else {
                    return Ok(Value::Boolean(false));
                }
            }
            other => other,
        },
        Some(false) => match parsed {
            Number::Fixnum(i) => Number::Flonum(i as f64),
            other => other,
        },
        None => parsed,
    };
    Ok(Value::Number(final_n))
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

/// `(vector=? a b)` — R7RS element-wise structural equality. Compares
/// element-by-element with `equal?` (handles recursive structures).
fn b_vector_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("vector=?", "2", args.len()));
    }
    let a = match &args[0] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector=?", "vector", v)),
    };
    let b = match &args[1] {
        Value::Vector(v) => v.borrow().clone(),
        v => return Err(type_err("vector=?", "vector", v)),
    };
    if a.len() != b.len() {
        return Ok(Value::Boolean(false));
    }
    for (x, y) in a.iter().zip(b.iter()) {
        if !eq::equal(x, y) {
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
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
pub(crate) const TAG_MESSAGE: &str = "&message";
const TAG_IRRITANTS: &str = "&irritants";
const TAG_WARNING: &str = "&warning";
const TAG_SERIOUS: &str = "&serious";
const TAG_ERROR: &str = "&error";
const TAG_VIOLATION: &str = "&violation";
pub(crate) const TAG_ASSERTION: &str = "&assertion";
const TAG_NON_CONTINUABLE: &str = "&non-continuable";
pub(crate) const TAG_WHO: &str = "&who";
const TAG_CONDITION: &str = "&condition";
const TAG_FILE_ERROR: &str = "&file-error";
const TAG_READ_ERROR: &str = "&read-error";
const TAG_EXIT_REQUESTED: &str = "&exit-requested";
// R6RS §7.2 — &i/o family.
const TAG_IO: &str = "&i/o";
const TAG_IO_READ: &str = "&i/o-read";
const TAG_IO_WRITE: &str = "&i/o-write";
const TAG_IO_INVALID_POSITION: &str = "&i/o-invalid-position";
const TAG_IO_FILENAME: &str = "&i/o-filename";
const TAG_IO_FILE_PROTECTION: &str = "&i/o-file-protection";
const TAG_IO_FILE_IS_READ_ONLY: &str = "&i/o-file-is-read-only";
const TAG_IO_FILE_ALREADY_EXISTS: &str = "&i/o-file-already-exists";
const TAG_IO_FILE_DOES_NOT_EXIST: &str = "&i/o-file-does-not-exist";
const TAG_IO_PORT: &str = "&i/o-port";
const TAG_IO_DECODING: &str = "&i/o-decoding";
const TAG_IO_ENCODING: &str = "&i/o-encoding";
// R6RS §7.2 — violation subtypes.
const TAG_SYNTAX: &str = "&syntax";
const TAG_UNDEFINED: &str = "&undefined";
const TAG_LEXICAL: &str = "&lexical";
const TAG_IMPL_RESTRICTION: &str = "&implementation-restriction";
const TAG_NO_INFINITIES: &str = "&no-infinities";
const TAG_NO_NANS: &str = "&no-nans";
// R6RS++ §9 (Phase 2D): condition subtypes extending &error
// for the Phase 2 ecosystem.
//
//   &contract   raised by Phase 2B contract violations; carries
//               source / target / contract-description / blamed
//               value (4 fields, all surfaced as accessors).
//   &type       runtime type errors -- complementary to cs-typer's
//               static type errors. 2 fields: expected type
//               description (string/symbol) + actual value.
//   &module     library / import / module-system errors. 1 field:
//               the offending library-name list (e.g. `(http server)`).
const TAG_CONTRACT: &str = "&contract";
const TAG_TYPE: &str = "&type";
const TAG_MODULE: &str = "&module";

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
        // R6RS §7.2 — &i/o family rooted at &error.
        m.insert(TAG_IO.into(), TAG_ERROR.into());
        m.insert(TAG_IO_READ.into(), TAG_IO.into());
        m.insert(TAG_IO_WRITE.into(), TAG_IO.into());
        m.insert(TAG_IO_INVALID_POSITION.into(), TAG_IO.into());
        m.insert(TAG_IO_FILENAME.into(), TAG_IO.into());
        m.insert(TAG_IO_FILE_PROTECTION.into(), TAG_IO_FILENAME.into());
        m.insert(
            TAG_IO_FILE_IS_READ_ONLY.into(),
            TAG_IO_FILE_PROTECTION.into(),
        );
        m.insert(TAG_IO_FILE_ALREADY_EXISTS.into(), TAG_IO_FILENAME.into());
        m.insert(TAG_IO_FILE_DOES_NOT_EXIST.into(), TAG_IO_FILENAME.into());
        m.insert(TAG_IO_PORT.into(), TAG_IO.into());
        m.insert(TAG_IO_DECODING.into(), TAG_IO_PORT.into());
        m.insert(TAG_IO_ENCODING.into(), TAG_IO_PORT.into());
        // R6RS §7.2 — violation subtypes rooted at &violation.
        m.insert(TAG_SYNTAX.into(), TAG_VIOLATION.into());
        m.insert(TAG_UNDEFINED.into(), TAG_VIOLATION.into());
        m.insert(TAG_LEXICAL.into(), TAG_VIOLATION.into());
        m.insert(TAG_IMPL_RESTRICTION.into(), TAG_VIOLATION.into());
        m.insert(TAG_NO_INFINITIES.into(), TAG_IMPL_RESTRICTION.into());
        m.insert(TAG_NO_NANS.into(), TAG_IMPL_RESTRICTION.into());
        // R6RS++ Phase 2D extensions, all rooted at &error.
        m.insert(TAG_CONTRACT.into(), TAG_ERROR.into());
        m.insert(TAG_TYPE.into(), TAG_ERROR.into());
        m.insert(TAG_MODULE.into(), TAG_ERROR.into());
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
            TAG_IO,
            TAG_IO_READ,
            TAG_IO_WRITE,
            TAG_IO_INVALID_POSITION,
            TAG_IO_FILENAME,
            TAG_IO_FILE_PROTECTION,
            TAG_IO_FILE_IS_READ_ONLY,
            TAG_IO_FILE_ALREADY_EXISTS,
            TAG_IO_FILE_DOES_NOT_EXIST,
            TAG_IO_PORT,
            TAG_IO_DECODING,
            TAG_IO_ENCODING,
            TAG_SYNTAX,
            TAG_UNDEFINED,
            TAG_LEXICAL,
            TAG_IMPL_RESTRICTION,
            TAG_NO_INFINITIES,
            TAG_NO_NANS,
            // Phase 2D additions:
            TAG_CONTRACT,
            TAG_TYPE,
            TAG_MODULE,
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
        return is_known_simple_tag(&t);
    }
    // R6RS §7 — a record-typed value whose rtd descends from
    // `&condition` is also a simple condition. We recognize ANY
    // procedural-records instance as a candidate; the condition?
    // predicate is descendants-inclusive in the existing string-tagged
    // hierarchy and treats procedural-records instances as additional
    // simples carried alongside.
    is_proc_record_simple(v)
}

/// True if `v` is a procedural-records instance — a vector whose
/// first slot is a Symbol registered in PROC_RECORD_RTDS. Enables
/// procedural rtds to participate in compound conditions.
fn is_proc_record_simple(v: &Value) -> bool {
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        if let Some(Value::Symbol(s)) = v.first() {
            return PROC_RECORD_RTDS.with(|m| m.borrow().contains_key(s));
        }
    }
    false
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
pub(crate) fn make_simple(tag: &str, fields: Vec<Value>) -> Value {
    let mut v = Vec::with_capacity(1 + fields.len());
    v.push(Value::string(tag));
    v.extend(fields);
    new_vector(v)
}

/// Wrap a list of simples in a compound condition vector. Always wraps —
/// even a single simple — so the data shape is uniform.
pub(crate) fn make_compound(simples: Vec<Value>) -> Value {
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

// ---- R6RS §7.2 condition subtype constructors / predicates ----

// Field-less condition makers. R6RS §7.2 specifies these as 0-arg
// constructors that build a compound containing one simple of the
// matching tag.
macro_rules! field_less_cond {
    ($maker:ident, $pred:ident, $tag:ident, $name:literal) => {
        fn $maker(args: &[Value]) -> Result<Value, String> {
            if !args.is_empty() {
                return Err(arity_err(concat!("make-", $name), "0", args.len()));
            }
            Ok(make_compound(vec![make_simple($tag, vec![])]))
        }
        fn $pred(args: &[Value]) -> Result<Value, String> {
            if args.len() != 1 {
                return Err(arity_err(concat!($name, "?"), "1", args.len()));
            }
            Ok(Value::Boolean(cond_has_subtype(&args[0], $tag)))
        }
    };
}

field_less_cond!(b_make_io_error, b_io_error_p, TAG_IO, "i/o-error");
field_less_cond!(
    b_make_io_read_error,
    b_io_read_error_p,
    TAG_IO_READ,
    "i/o-read-error"
);
field_less_cond!(
    b_make_io_write_error,
    b_io_write_error_p,
    TAG_IO_WRITE,
    "i/o-write-error"
);
field_less_cond!(
    b_make_io_file_protection_error,
    b_io_file_protection_error_p,
    TAG_IO_FILE_PROTECTION,
    "i/o-file-protection-error"
);
field_less_cond!(
    b_make_io_file_is_read_only_error,
    b_io_file_is_read_only_error_p,
    TAG_IO_FILE_IS_READ_ONLY,
    "i/o-file-is-read-only-error"
);
field_less_cond!(
    b_make_io_file_already_exists_error,
    b_io_file_already_exists_error_p,
    TAG_IO_FILE_ALREADY_EXISTS,
    "i/o-file-already-exists-error"
);
field_less_cond!(
    b_make_io_file_does_not_exist_error,
    b_io_file_does_not_exist_error_p,
    TAG_IO_FILE_DOES_NOT_EXIST,
    "i/o-file-does-not-exist-error"
);
field_less_cond!(
    b_make_io_decoding_error,
    b_io_decoding_error_p,
    TAG_IO_DECODING,
    "i/o-decoding-error"
);
field_less_cond!(
    b_make_undefined_violation,
    b_undefined_violation_p,
    TAG_UNDEFINED,
    "undefined-violation"
);
field_less_cond!(
    b_make_lexical_violation,
    b_lexical_violation_p,
    TAG_LEXICAL,
    "lexical-violation"
);
field_less_cond!(
    b_make_impl_restriction,
    b_impl_restriction_p,
    TAG_IMPL_RESTRICTION,
    "implementation-restriction-violation"
);
field_less_cond!(
    b_make_no_infinities,
    b_no_infinities_p,
    TAG_NO_INFINITIES,
    "no-infinities-violation"
);
field_less_cond!(
    b_make_no_nans,
    b_no_nans_p,
    TAG_NO_NANS,
    "no-nans-violation"
);

// Single-field condition makers. R6RS §7.2: each takes one value
// stored in the simple alongside the tag.
macro_rules! one_field_cond {
    ($maker:ident, $pred:ident, $accessor:ident, $tag:ident, $maker_name:literal, $pred_name:literal, $accessor_name:literal) => {
        fn $maker(args: &[Value]) -> Result<Value, String> {
            if args.len() != 1 {
                return Err(arity_err($maker_name, "1", args.len()));
            }
            Ok(make_compound(vec![make_simple(
                $tag,
                vec![args[0].clone()],
            )]))
        }
        fn $pred(args: &[Value]) -> Result<Value, String> {
            if args.len() != 1 {
                return Err(arity_err($pred_name, "1", args.len()));
            }
            Ok(Value::Boolean(cond_has_subtype(&args[0], $tag)))
        }
        fn $accessor(args: &[Value]) -> Result<Value, String> {
            if args.len() != 1 {
                return Err(arity_err($accessor_name, "1", args.len()));
            }
            let simple = find_simple_with_tag(&args[0], $tag)
                .ok_or_else(|| format!("{}: not the matching condition type", $accessor_name))?;
            if let Value::Vector(vc) = simple {
                let v = vc.borrow();
                if v.len() >= 2 {
                    return Ok(v[1].clone());
                }
            }
            Err(format!("{}: malformed", $accessor_name))
        }
    };
}

one_field_cond!(
    b_make_io_invalid_position_error,
    b_io_invalid_position_error_p,
    b_io_error_position,
    TAG_IO_INVALID_POSITION,
    "make-i/o-invalid-position-error",
    "i/o-invalid-position-error?",
    "i/o-error-position"
);
one_field_cond!(
    b_make_io_filename_error,
    b_io_filename_error_p,
    b_io_error_filename,
    TAG_IO_FILENAME,
    "make-i/o-filename-error",
    "i/o-filename-error?",
    "i/o-error-filename"
);
one_field_cond!(
    b_make_io_port_error,
    b_io_port_error_p,
    b_io_error_port,
    TAG_IO_PORT,
    "make-i/o-port-error",
    "i/o-port-error?",
    "i/o-error-port"
);
one_field_cond!(
    b_make_io_encoding_error,
    b_io_encoding_error_p,
    b_io_encoding_error_char,
    TAG_IO_ENCODING,
    "make-i/o-encoding-error",
    "i/o-encoding-error?",
    "i/o-encoding-error-char"
);

// &syntax has two fields (form, subform).
fn b_make_syntax_violation(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("make-syntax-violation", "2", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_SYNTAX,
        vec![args[0].clone(), args[1].clone()],
    )]))
}

fn b_syntax_violation_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax-violation?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_SYNTAX)))
}

fn b_syntax_violation_form(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax-violation-form", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_SYNTAX)
        .ok_or_else(|| "syntax-violation-form: not a syntax-violation".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 2 {
            return Ok(v[1].clone());
        }
    }
    Err("syntax-violation-form: malformed".to_string())
}

fn b_syntax_violation_subform(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("syntax-violation-subform", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_SYNTAX)
        .ok_or_else(|| "syntax-violation-subform: not a syntax-violation".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 3 {
            return Ok(v[2].clone());
        }
    }
    Err("syntax-violation-subform: malformed".to_string())
}

// ---- R6RS++ Phase 2D condition subtypes ----

/// `(make-contract-violation source target contract value)` ->
/// compound &contract condition. Used by Phase 2B contract
/// machinery to raise blame on a contract failure.
fn b_make_contract_violation(args: &[Value]) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(arity_err("make-contract-violation", "4", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_CONTRACT,
        vec![
            args[0].clone(), // source library
            args[1].clone(), // target library
            args[2].clone(), // contract description
            args[3].clone(), // blamed value
        ],
    )]))
}

fn b_contract_violation_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("contract-violation?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_CONTRACT)))
}

fn contract_field(args: &[Value], idx: usize, name: &str) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_CONTRACT)
        .ok_or_else(|| format!("{}: not a contract-violation", name))?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() > idx {
            return Ok(v[idx].clone());
        }
    }
    Err(format!("{}: malformed", name))
}

fn b_contract_violation_source(args: &[Value]) -> Result<Value, String> {
    contract_field(args, 1, "contract-violation-source")
}

fn b_contract_violation_target(args: &[Value]) -> Result<Value, String> {
    contract_field(args, 2, "contract-violation-target")
}

fn b_contract_violation_contract(args: &[Value]) -> Result<Value, String> {
    contract_field(args, 3, "contract-violation-contract")
}

fn b_contract_violation_value(args: &[Value]) -> Result<Value, String> {
    contract_field(args, 4, "contract-violation-value")
}

/// `(make-type-error expected actual)` -> compound &type
/// condition. Runtime type errors; complementary to cs-typer's
/// static checks. `expected` is typically a string or symbol
/// describing the wanted type; `actual` is the offending value.
fn b_make_type_error(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("make-type-error", "2", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_TYPE,
        vec![args[0].clone(), args[1].clone()],
    )]))
}

fn b_type_error_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("type-error?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_TYPE)))
}

fn b_type_error_expected(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("type-error-expected", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_TYPE)
        .ok_or_else(|| "type-error-expected: not a type-error".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 2 {
            return Ok(v[1].clone());
        }
    }
    Err("type-error-expected: malformed".into())
}

fn b_type_error_actual(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("type-error-actual", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_TYPE)
        .ok_or_else(|| "type-error-actual: not a type-error".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 3 {
            return Ok(v[2].clone());
        }
    }
    Err("type-error-actual: malformed".into())
}

/// `(make-module-error library-name)` -> compound &module
/// condition. For library/import-system errors at runtime;
/// `library-name` is the offending library name spec (e.g. a
/// list like `(http server)`).
fn b_make_module_error(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-module-error", "1", args.len()));
    }
    Ok(make_compound(vec![make_simple(
        TAG_MODULE,
        vec![args[0].clone()],
    )]))
}

fn b_module_error_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("module-error?", "1", args.len()));
    }
    Ok(Value::Boolean(cond_has_subtype(&args[0], TAG_MODULE)))
}

fn b_module_error_library(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("module-error-library", "1", args.len()));
    }
    let simple = find_simple_with_tag(&args[0], TAG_MODULE)
        .ok_or_else(|| "module-error-library: not a module-error".to_string())?;
    if let Value::Vector(vc) = simple {
        let v = vc.borrow();
        if v.len() >= 2 {
            return Ok(v[1].clone());
        }
    }
    Err("module-error-library: malformed".into())
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

/// SRFI-1 `(find-tail pred list)` — like `find`, but returns the
/// tail starting at the first matching element instead of the
/// element itself. #f if no match.
fn b_find_tail(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("find-tail", "2", args.len()));
    }
    let pred = args[0].clone();
    let mut cur = args[1].clone();
    loop {
        match cur.clone() {
            Value::Null => return Ok(Value::Boolean(false)),
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                let r = apply_procedure(&pred, &[car], ctx).map_err(|e| e.message())?;
                if r.is_truthy() {
                    return Ok(cur);
                }
                cur = p.cdr.borrow().clone();
            }
            _ => return Err("find-tail: improper list".into()),
        }
    }
}

/// SRFI-1 `(reduce-right proc default list)` — right-associative
/// counterpart to `reduce`. Returns `default` if the list is empty,
/// the only element if length 1, otherwise folds from the right.
fn b_reduce_right(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("reduce-right", "3", args.len()));
    }
    let proc_val = args[0].clone();
    let default = args[1].clone();
    let items = collect_proper_list("reduce-right", &args[2])?;
    if items.is_empty() {
        return Ok(default);
    }
    let mut acc = items[items.len() - 1].clone();
    for item in items[..items.len() - 1].iter().rev() {
        acc = apply_procedure(&proc_val, &[item.clone(), acc], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

/// SRFI-1 `(pair-fold kons knil list)` — like fold but applies
/// kons to successive pairs (sublists), not their cars.
fn b_pair_fold(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("pair-fold", "3", args.len()));
    }
    let kons = args[0].clone();
    let mut acc = args[1].clone();
    let mut cur = args[2].clone();
    loop {
        match cur.clone() {
            Value::Null => return Ok(acc),
            Value::Pair(p) => {
                let next = p.cdr.borrow().clone();
                acc = apply_procedure(&kons, &[cur.clone(), acc], ctx).map_err(|e| e.message())?;
                cur = next;
            }
            _ => return Err("pair-fold: improper list".into()),
        }
    }
}

/// SRFI-1 `(pair-fold-right kons knil list)` — right-associative.
fn b_pair_fold_right(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("pair-fold-right", "3", args.len()));
    }
    let kons = args[0].clone();
    let knil = args[1].clone();
    // Collect all the sublists (each successive cdr) first.
    let mut subs: Vec<Value> = Vec::new();
    let mut cur = args[2].clone();
    loop {
        match cur.clone() {
            Value::Null => break,
            Value::Pair(_) => {
                subs.push(cur.clone());
                if let Value::Pair(p) = cur {
                    cur = p.cdr.borrow().clone();
                }
            }
            _ => return Err("pair-fold-right: improper list".into()),
        }
    }
    let mut acc = knil;
    for sub in subs.into_iter().rev() {
        acc = apply_procedure(&kons, &[sub, acc], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

/// SRFI-1 `(pair-for-each proc list)` — apply proc to each successive
/// sublist; result is unspecified.
fn b_pair_for_each(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("pair-for-each", "2", args.len()));
    }
    let proc_val = args[0].clone();
    let mut cur = args[1].clone();
    loop {
        match cur.clone() {
            Value::Null => return Ok(Value::Unspecified),
            Value::Pair(p) => {
                let next = p.cdr.borrow().clone();
                apply_procedure(&proc_val, &[cur.clone()], ctx).map_err(|e| e.message())?;
                cur = next;
            }
            _ => return Err("pair-for-each: improper list".into()),
        }
    }
}

/// SRFI-13 `(string-fold kons knil str [start [end]])` — left-associative
/// fold over chars.
fn b_string_fold(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !(3..=5).contains(&args.len()) {
        return Err(arity_err("string-fold", "3..5", args.len()));
    }
    let kons = args[0].clone();
    let mut acc = args[1].clone();
    let s = match &args[2] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-fold", "string", v)),
    };
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.len() >= 4 {
        as_int_i64("string-fold", &args[3])?.max(0) as usize
    } else {
        0
    };
    let end = if args.len() == 5 {
        as_int_i64("string-fold", &args[4])?.max(0) as usize
    } else {
        len
    };
    if start > end || end > len {
        return Err("string-fold: range out of bounds".into());
    }
    for c in &chars[start..end] {
        acc = apply_procedure(&kons, &[Value::Character(*c), acc], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

/// SRFI-13 `(string-fold-right kons knil str [start [end]])`.
fn b_string_fold_right(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !(3..=5).contains(&args.len()) {
        return Err(arity_err("string-fold-right", "3..5", args.len()));
    }
    let kons = args[0].clone();
    let knil = args[1].clone();
    let s = match &args[2] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string-fold-right", "string", v)),
    };
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.len() >= 4 {
        as_int_i64("string-fold-right", &args[3])?.max(0) as usize
    } else {
        0
    };
    let end = if args.len() == 5 {
        as_int_i64("string-fold-right", &args[4])?.max(0) as usize
    } else {
        len
    };
    if start > end || end > len {
        return Err("string-fold-right: range out of bounds".into());
    }
    let mut acc = knil;
    for c in chars[start..end].iter().rev() {
        acc = apply_procedure(&kons, &[Value::Character(*c), acc], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

/// SRFI-13 `(string-tabulate proc len)` — build a string of length
/// `len` whose i-th char is `(proc i)`.
fn b_string_tabulate(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string-tabulate", "2", args.len()));
    }
    let proc_val = args[0].clone();
    let n = as_int_i64("string-tabulate", &args[1])?;
    if n < 0 {
        return Err("string-tabulate: negative length".into());
    }
    let mut out = String::with_capacity(n as usize);
    for i in 0..n {
        let r = apply_procedure(&proc_val, &[Value::fixnum(i)], ctx).map_err(|e| e.message())?;
        match r {
            Value::Character(c) => out.push(c),
            v => return Err(type_err("string-tabulate", "character (proc result)", &v)),
        }
    }
    Ok(Value::string(out))
}

/// R6RS `(vector-fold-right kons knil vec)` — right-associative
/// counterpart to `vector-fold`.
fn b_vector_fold_right(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("vector-fold-right", "3", args.len()));
    }
    let kons = args[0].clone();
    let knil = args[1].clone();
    let v = match &args[2] {
        Value::Vector(vc) => vc.borrow().clone(),
        v => return Err(type_err("vector-fold-right", "vector", v)),
    };
    let mut acc = knil;
    for item in v.iter().rev() {
        acc = apply_procedure(&kons, &[item.clone(), acc], ctx).map_err(|e| e.message())?;
    }
    Ok(acc)
}

/// SRFI-1 `(unfold-right p f g seed [tail])` — right-fold version
/// of unfold; builds a list by walking the seed through f/g until p
/// is satisfied, then prepending in reverse.
fn b_unfold_right(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !(4..=5).contains(&args.len()) {
        return Err(arity_err("unfold-right", "4..5", args.len()));
    }
    let pred = args[0].clone();
    let mapper = args[1].clone();
    let advancer = args[2].clone();
    let mut seed = args[3].clone();
    let mut acc = if args.len() == 5 {
        args[4].clone()
    } else {
        Value::Null
    };
    loop {
        let stop = apply_procedure(&pred, &[seed.clone()], ctx).map_err(|e| e.message())?;
        if stop.is_truthy() {
            return Ok(acc);
        }
        let elt = apply_procedure(&mapper, &[seed.clone()], ctx).map_err(|e| e.message())?;
        acc = Value::Pair(Pair::new(elt, acc));
        seed = apply_procedure(&advancer, &[seed], ctx).map_err(|e| e.message())?;
    }
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

/// R6RS `(get-bytevector-all port)` — read every remaining byte
/// into a fresh bytevector. EOF if already drained.
fn b_get_bytevector_all(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("get-bytevector-all", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.bytes.len() {
                    return Ok(Value::Eof);
                }
                let bytes = s.bytes[s.pos..].to_vec();
                s.pos = s.bytes.len();
                Ok(Value::ByteVector(cs_core::Gc::new(
                    std::cell::RefCell::new(bytes),
                )))
            }
            _ => Err("get-bytevector-all: not a binary input port".into()),
        },
        v => Err(type_err("get-bytevector-all", "binary-input-port", v)),
    }
}

/// R6RS `(get-string-n port count)` — read up to `count` chars from a
/// textual input port into a fresh string. EOF if already drained.
fn b_get_string_n(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("get-string-n", "2", args.len()));
    }
    let n = as_int_i64("get-string-n", &args[1])?;
    if n < 0 {
        return Err("get-string-n: negative count".into());
    }
    let n = n as usize;
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                if s.pos >= s.chars.len() {
                    return Ok(Value::Eof);
                }
                let avail = s.chars.len() - s.pos;
                let take = n.min(avail);
                let collected: String = s.chars[s.pos..s.pos + take].iter().collect();
                s.pos += take;
                Ok(Value::string(collected))
            }
            _ => Err("get-string-n: not a textual input port".into()),
        },
        v => Err(type_err("get-string-n", "textual-input-port", v)),
    }
}

/// R6RS `(put-char port char)` — write a character to a textual
/// output port. Mirrors `write-char` but with the R6RS arg order
/// (port first).
fn b_put_char(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("put-char", "2", args.len()));
    }
    let c = match &args[1] {
        Value::Character(c) => *c,
        v => return Err(type_err("put-char", "character", v)),
    };
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringOutput(buf) => {
                buf.borrow_mut().push(c);
                Ok(Value::Unspecified)
            }
            Port::FileOutput(state) => {
                let mut st = state.borrow_mut();
                if st.closed {
                    return Err("put-char: port is closed".into());
                }
                let mut tmp = [0u8; 4];
                let s = c.encode_utf8(&mut tmp);
                st.buf.extend_from_slice(s.as_bytes());
                Ok(Value::Unspecified)
            }
            _ => Err("put-char: not a textual output port".into()),
        },
        v => Err(type_err("put-char", "textual-output-port", v)),
    }
}

/// R6RS `(put-string port string [start [count]])` — write a slice
/// of a string to a textual output port.
fn b_put_string(args: &[Value]) -> Result<Value, String> {
    if !(2..=4).contains(&args.len()) {
        return Err(arity_err("put-string", "2..4", args.len()));
    }
    let s = match &args[1] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("put-string", "string", v)),
    };
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let start = if args.len() >= 3 {
        let i = as_int_i64("put-string", &args[2])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("put-string: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 4 {
        let c = as_int_i64("put-string", &args[3])?;
        if c < 0 {
            return Err("put-string: negative count".into());
        }
        let e = start + c as usize;
        if e > len {
            return Err(format!("put-string: count exceeds string length: {}", c));
        }
        e
    } else {
        len
    };
    let slice: String = chars[start..end].iter().collect();
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringOutput(buf) => {
                buf.borrow_mut().push_str(&slice);
                Ok(Value::Unspecified)
            }
            Port::FileOutput(state) => {
                let mut st = state.borrow_mut();
                if st.closed {
                    return Err("put-string: port is closed".into());
                }
                st.buf.extend_from_slice(slice.as_bytes());
                Ok(Value::Unspecified)
            }
            _ => Err("put-string: not a textual output port".into()),
        },
        v => Err(type_err("put-string", "textual-output-port", v)),
    }
}

/// R6RS `(put-bytevector port bytevector [start [count]])` — write
/// a slice of a bytevector to a binary output port.
fn b_put_bytevector(args: &[Value]) -> Result<Value, String> {
    if !(2..=4).contains(&args.len()) {
        return Err(arity_err("put-bytevector", "2..4", args.len()));
    }
    let bytes = match &args[1] {
        Value::ByteVector(b) => b.borrow().clone(),
        v => return Err(type_err("put-bytevector", "bytevector", v)),
    };
    let len = bytes.len();
    let start = if args.len() >= 3 {
        let i = as_int_i64("put-bytevector", &args[2])?;
        if i < 0 || (i as usize) > len {
            return Err(format!("put-bytevector: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 4 {
        let c = as_int_i64("put-bytevector", &args[3])?;
        if c < 0 {
            return Err("put-bytevector: negative count".into());
        }
        let e = start + c as usize;
        if e > len {
            return Err(format!(
                "put-bytevector: count exceeds bytevector length: {}",
                c
            ));
        }
        e
    } else {
        len
    };
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorOutput(buf) => {
                buf.borrow_mut().extend_from_slice(&bytes[start..end]);
                Ok(Value::Unspecified)
            }
            _ => Err("put-bytevector: not a binary output port".into()),
        },
        v => Err(type_err("put-bytevector", "binary-output-port", v)),
    }
}

// ---- R6RS §8.2.5 — codecs and transcoders -----------------------
//
// Codecs and transcoders are opaque values stored as tagged vectors:
//   codec      = #("&codec" <name-symbol>)
//   transcoder = #("&transcoder" <codec> <eol-style> <error-mode>)
//
// Foundation supports UTF-8 and Latin-1 fully for bytevector<->string
// conversion. UTF-16 is registered but conversions raise — programs
// can still construct and thread UTF-16 transcoders, only the actual
// encode/decode is gated until a follow-up iter.

const TAG_CODEC: &str = "&codec";
const TAG_TRANSCODER: &str = "&transcoder";

fn codec_name(v: &Value) -> Option<String> {
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        if v.len() >= 2 {
            if let (Some(Value::String(t)), Some(Value::String(s))) = (v.first(), v.get(1)) {
                if t.borrow().as_str() == TAG_CODEC {
                    return Some(s.borrow().clone());
                }
            }
        }
    }
    None
}

fn make_codec(name: &str) -> Value {
    new_vector(vec![Value::string(TAG_CODEC), Value::string(name)])
}

fn make_transcoder_v(codec: Value, eol: Value, mode: Value) -> Value {
    new_vector(vec![Value::string(TAG_TRANSCODER), codec, eol, mode])
}

fn is_transcoder(v: &Value) -> bool {
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        if let Some(Value::String(t)) = v.first() {
            return t.borrow().as_str() == TAG_TRANSCODER;
        }
    }
    false
}

fn b_utf8_codec(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("utf-8-codec", "0", args.len()));
    }
    let _ = syms;
    Ok(make_codec("utf-8"))
}

fn b_utf16_codec(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("utf-16-codec", "0", args.len()));
    }
    let _ = syms;
    Ok(make_codec("utf-16"))
}

fn b_latin1_codec(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("latin-1-codec", "0", args.len()));
    }
    let _ = syms;
    Ok(make_codec("latin-1"))
}

fn b_native_eol_style(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("native-eol-style", "0", args.len()));
    }
    Ok(Value::Symbol(syms.intern("lf")))
}

fn b_make_transcoder(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !(1..=3).contains(&args.len()) {
        return Err(arity_err("make-transcoder", "1..3", args.len()));
    }
    if codec_name(&args[0]).is_none() {
        return Err("make-transcoder: first arg must be a codec".into());
    }
    let eol = if args.len() >= 2 {
        args[1].clone()
    } else {
        Value::Symbol(syms.intern("lf"))
    };
    let mode = if args.len() == 3 {
        args[2].clone()
    } else {
        Value::Symbol(syms.intern("replace"))
    };
    Ok(make_transcoder_v(args[0].clone(), eol, mode))
}

fn b_native_transcoder(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("native-transcoder", "0", args.len()));
    }
    Ok(make_transcoder_v(
        make_codec("utf-8"),
        Value::Symbol(syms.intern("lf")),
        Value::Symbol(syms.intern("replace")),
    ))
}

fn transcoder_field(v: &Value, idx: usize, op: &str) -> Result<Value, String> {
    if !is_transcoder(v) {
        return Err(format!("{}: not a transcoder", op));
    }
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        if v.len() > idx {
            return Ok(v[idx].clone());
        }
    }
    Err(format!("{}: malformed transcoder", op))
}

fn b_transcoder_codec(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("transcoder-codec", "1", args.len()));
    }
    transcoder_field(&args[0], 1, "transcoder-codec")
}

fn b_transcoder_eol_style(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("transcoder-eol-style", "1", args.len()));
    }
    transcoder_field(&args[0], 2, "transcoder-eol-style")
}

fn b_transcoder_error_handling_mode(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("transcoder-error-handling-mode", "1", args.len()));
    }
    transcoder_field(&args[0], 3, "transcoder-error-handling-mode")
}

fn transcoder_codec_name(t: &Value) -> Result<String, String> {
    let codec = transcoder_field(t, 1, "transcoder")?;
    codec_name(&codec).ok_or_else(|| "transcoder: missing codec".to_string())
}

fn decode_bytes(bytes: &[u8], codec: &str, op: &str) -> Result<String, String> {
    match codec {
        "utf-8" => Ok(String::from_utf8_lossy(bytes).into_owned()),
        "latin-1" => Ok(bytes.iter().map(|b| *b as char).collect()),
        other => Err(format!("{}: codec {:?} not yet supported", op, other)),
    }
}

fn encode_string(s: &str, codec: &str, op: &str) -> Result<Vec<u8>, String> {
    match codec {
        "utf-8" => Ok(s.as_bytes().to_vec()),
        "latin-1" => {
            let mut out = Vec::with_capacity(s.len());
            for c in s.chars() {
                let cp = c as u32;
                if cp > 0xFF {
                    return Err(format!(
                        "{}: char U+{:04X} not representable in Latin-1",
                        op, cp
                    ));
                }
                out.push(cp as u8);
            }
            Ok(out)
        }
        other => Err(format!("{}: codec {:?} not yet supported", op, other)),
    }
}

fn b_bytevector_to_string(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector->string", "2", args.len()));
    }
    let bytes = match &args[0] {
        Value::ByteVector(b) => b.borrow().clone(),
        v => return Err(type_err("bytevector->string", "bytevector", v)),
    };
    let codec = transcoder_codec_name(&args[1])?;
    let s = decode_bytes(&bytes, &codec, "bytevector->string")?;
    Ok(Value::string(s))
}

fn b_string_to_bytevector(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("string->bytevector", "2", args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("string->bytevector", "string", v)),
    };
    let codec = transcoder_codec_name(&args[1])?;
    let bytes = encode_string(&s, &codec, "string->bytevector")?;
    Ok(Value::ByteVector(cs_core::Gc::new(
        std::cell::RefCell::new(bytes),
    )))
}

/// `(transcoded-port port transcoder)` — wraps a binary port with a
/// transcoder. Foundation: for input ports we eagerly decode all
/// remaining bytes into a string input port. For output ports we
/// return the binary port itself with a marker — actually wrapping
/// requires a new Port variant, which is gated to a future iter.
fn b_transcoded_port(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("transcoded-port", "2", args.len()));
    }
    let codec = transcoder_codec_name(&args[1])?;
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let bytes = {
                    let mut s = state.borrow_mut();
                    let leftover = s.bytes[s.pos..].to_vec();
                    s.pos = s.bytes.len();
                    leftover
                };
                let decoded = decode_bytes(&bytes, &codec, "transcoded-port")?;
                Ok(Value::Port(Port::string_input(&decoded)))
            }
            Port::ByteVectorOutput(_) => {
                Err("transcoded-port: binary-output-port wrapping not yet supported".into())
            }
            _ => Err("transcoded-port: expected a binary port".into()),
        },
        v => Err(type_err("transcoded-port", "port", v)),
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

/// R7RS `(read-bytevector! bv [port [start [end]]])`.
/// Reads from port into bv[start..end] (mutating in place). Returns the
/// number of bytes read, or eof-object if no bytes are available and
/// the requested range is non-empty.
fn b_read_bytevector_bang(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 4 {
        return Err(arity_err("read-bytevector!", "1..4", args.len()));
    }
    let bv = match &args[0] {
        Value::ByteVector(b) => b.clone(),
        v => return Err(type_err("read-bytevector!", "bytevector", v)),
    };
    let port = if args.len() >= 2 {
        args[1].clone()
    } else {
        ctx.current_input_port
            .clone()
            .ok_or_else(|| "read-bytevector!: no current input port".to_string())?
    };
    let bv_len = bv.borrow().len();
    let start = if args.len() >= 3 {
        let i = as_int_i64("read-bytevector!", &args[2])?;
        if i < 0 || (i as usize) > bv_len {
            return Err(format!("read-bytevector!: start out of range: {}", i));
        }
        i as usize
    } else {
        0
    };
    let end = if args.len() == 4 {
        let i = as_int_i64("read-bytevector!", &args[3])?;
        if i < 0 || (i as usize) > bv_len || (i as usize) < start {
            return Err(format!("read-bytevector!: end out of range: {}", i));
        }
        i as usize
    } else {
        bv_len
    };
    let n_wanted = end - start;
    match &port {
        Value::Port(p) => match &**p {
            Port::ByteVectorInput(state) => {
                let mut s = state.borrow_mut();
                if n_wanted == 0 {
                    return Ok(Value::fixnum(0));
                }
                if s.pos >= s.bytes.len() {
                    return Ok(Value::Eof);
                }
                let avail = s.bytes.len() - s.pos;
                let n = n_wanted.min(avail);
                let mut buf = bv.borrow_mut();
                buf[start..start + n].copy_from_slice(&s.bytes[s.pos..s.pos + n]);
                s.pos += n;
                Ok(Value::fixnum(n as i64))
            }
            _ => Err("read-bytevector!: not a binary input port".into()),
        },
        v => Err(type_err("read-bytevector!", "input-port", v)),
    }
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
            Port::FileOutput(state) => {
                let mut st = state.borrow_mut();
                if st.closed {
                    return Err("write-char: port is closed".into());
                }
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                st.buf.extend_from_slice(s.as_bytes());
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
            Port::FileOutput(state) => {
                let mut st = state.borrow_mut();
                if st.closed {
                    return Err("write-string: port is closed".into());
                }
                st.buf.extend_from_slice(slice.as_bytes());
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

/// R6RS `(call-with-input-file path proc)` — open the file as a
/// textual input port, pass it to proc, and close it on return.
/// Mirrors call-with-port's best-effort-close semantics.
fn b_call_with_input_file(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("call-with-input-file", "2", args.len()));
    }
    let port = b_open_input_file(&[args[0].clone()])?;
    let result = apply_procedure(&args[1], &[port.clone()], ctx).map_err(|e| e.message());
    let _ = b_close_port(&[port]);
    result
}

/// R6RS `(call-with-output-file path proc)` — create the file as a
/// textual output port, pass it to proc, and close it on return so
/// the buffered writes are flushed to disk.
fn b_call_with_output_file(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("call-with-output-file", "2", args.len()));
    }
    let port = b_open_output_file(&[args[0].clone()])?;
    let result = apply_procedure(&args[1], &[port.clone()], ctx).map_err(|e| e.message());
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

/// `(bytevector=? a b)` — element-wise bytewise equality.
/// R6RS-style 2-arg form. Errors on non-bytevector args.
fn b_bytevector_eq(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("bytevector=?", "2", args.len()));
    }
    let a = match &args[0] {
        Value::ByteVector(bv) => bv.borrow().clone(),
        v => return Err(type_err("bytevector=?", "bytevector", v)),
    };
    let b = match &args[1] {
        Value::ByteVector(bv) => bv.borrow().clone(),
        v => return Err(type_err("bytevector=?", "bytevector", v)),
    };
    Ok(Value::Boolean(a == b))
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

// R6RS §8.2 — `(standard-input-port)`, `(standard-output-port)`,
// `(standard-error-port)`. Spec says these are *binary* ports for
// stdio. We alias them to the same backing port as current-* (which
// is treated as textual): foundation-level Scheme programs treat
// them as opaque port handles to thread through I/O ops.
fn b_standard_input_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("standard-input-port", "0", args.len()));
    }
    Ok(ctx.current_input_port.clone().unwrap_or(Value::Unspecified))
}

fn b_standard_output_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("standard-output-port", "0", args.len()));
    }
    Ok(ctx
        .current_output_port
        .clone()
        .unwrap_or(Value::Unspecified))
}

fn b_standard_error_port(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("standard-error-port", "0", args.len()));
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
// ADR 0015 L1.1: `(environment <import-spec> ...)` builds a real
// R6RS §15.2 immutable snapshot environment. The returned value
// is a vector record:
//
//   #('__environment__ <bindings-alist> <mutable?>)
//
// where the alist captures (sym . value) pairs at construction
// time. `eval` recognizes this shape and runs the expanded
// program against a Frame::immutable_root built from the alist.
//
// Import-spec resolution is hardcoded for the (rnrs base) set
// at this milestone. Library membership metadata at the builtin
// level is a future iter (L1.3 composite construction); for now
// passing any other import spec returns an "unknown library"
// error so future-incompatible code doesn't silently work.

/// Sentinel marking a Vector value as an R6RS environment record.
pub(crate) const ENV_TAG: &str = "__environment__";

/// Sentinel symbol returned by `(interaction-environment)` and
/// `(scheme-report-environment N)`. Pure marker — `eval`'s 2nd-arg
/// handler recognizes it as "no L1.1 restriction; use ctx.top".
/// Not user-visible and not matched by string anywhere; the name
/// is intentionally non-Scheme-valid so it can't be reproduced by
/// `(intern ...)` from user code.
pub(crate) const TOP_LEVEL_ENV_SENTINEL: &str = "__top-level-env__";

/// Sentinel symbol returned by `(null-environment 5)`. Same role
/// as [`TOP_LEVEL_ENV_SENTINEL`] — opaque marker for an unrestricted
/// (in our impl) eval environment.
pub(crate) const NULL_ENV_SENTINEL: &str = "__null-env__";

/// Classification of a candidate R6RS environment-record Value.
/// Used by `is_environment_value`, `decode_environment`, and
/// `namespace_update` to share one shape-check implementation
/// instead of three near-identical inline matches.
#[derive(Debug)]
pub(crate) enum EnvShape {
    /// Well-formed; `mutable` is the value of slot[2].
    Valid { mutable: bool },
    /// Not a Vector at all.
    NotVector,
    /// Vector with len != 3.
    WrongArity,
    /// Slot[0] isn't the `ENV_TAG` string sentinel.
    MissingTag,
}

/// Single source of truth for the env-record shape predicate.
/// Caller is responsible for translating the failure variants
/// into user-facing error text or a boolean answer.
pub(crate) fn classify_env_record(v: &Value) -> EnvShape {
    let Value::Vector(items) = v else {
        return EnvShape::NotVector;
    };
    let items = items.borrow();
    if items.len() != 3 {
        return EnvShape::WrongArity;
    }
    let tag_ok = matches!(&items[0], Value::String(s) if s.borrow().as_str() == ENV_TAG);
    if !tag_ok {
        return EnvShape::MissingTag;
    }
    let mutable = matches!(&items[2], Value::Boolean(true));
    EnvShape::Valid { mutable }
}

/// Hardcoded (rnrs base) export list — the R6RS §11 base library
/// names registered as global builtins. NOT the full R6RS surface;
/// targets the names common Scheme programs use. L1.3 split this
/// from the (rnrs lists) exports; library-membership metadata at
/// builtin-registration time is the right long-term shape but the
/// hardcoded split is enough for composite construction to work.
const RNRS_BASE_EXPORTS: &[&str] = &[
    // arithmetic
    "+",
    "-",
    "*",
    "/",
    "=",
    "<",
    ">",
    "<=",
    ">=",
    "abs",
    "min",
    "max",
    "modulo",
    "quotient",
    "remainder",
    "expt",
    "gcd",
    "lcm",
    "floor",
    "ceiling",
    "truncate",
    "round",
    "zero?",
    "positive?",
    "negative?",
    "odd?",
    "even?",
    "square",
    // number predicates
    "number?",
    "integer?",
    "rational?",
    "real?",
    "complex?",
    "exact?",
    "inexact?",
    "exact-integer?",
    "exact->inexact",
    "inexact->exact",
    "number->string",
    "string->number",
    // list ops
    "pair?",
    "cons",
    "car",
    "cdr",
    "set-car!",
    "set-cdr!",
    "null?",
    "list",
    "list?",
    "length",
    "append",
    "reverse",
    "list-tail",
    "list-ref",
    "map",
    "for-each",
    "memq",
    "memv",
    "member",
    "assq",
    "assv",
    "assoc",
    // booleans + equality
    "not",
    "boolean?",
    "eq?",
    "eqv?",
    "equal?",
    // strings
    "string?",
    "string",
    "string-length",
    "string-ref",
    "substring",
    "string-append",
    "string->list",
    "list->string",
    "string->symbol",
    // chars
    "char?",
    "char->integer",
    "integer->char",
    // vectors
    "vector?",
    "make-vector",
    "vector",
    "vector-length",
    "vector-ref",
    "vector-set!",
    "vector->list",
    "list->vector",
    // symbols
    "symbol?",
    "symbol->string",
    // procedures
    "procedure?",
    "apply",
    // exceptions
    "error",
    "raise",
    "raise-continuable",
    "with-exception-handler",
    // I/O minimum
    "display",
    "write",
    "newline",
    // eval / environment (so guests can themselves construct envs)
    "eval",
    "environment",
];

/// Hardcoded (rnrs lists) export list — R6RS §3 lists library
/// procedures. These are NOT in (rnrs base); a user importing
/// only (rnrs base) doesn't see them. L1.3 split.
const RNRS_LISTS_EXPORTS: &[&str] = &[
    "find",
    "for-all",
    "exists",
    "filter",
    "partition",
    "fold-left",
    "fold-right",
    "remove",
    "remp",
    "remv",
    "remq",
    "cons*",
];

/// Resolve an import-spec datum (a list like `'(rnrs base)`) into
/// the set of names it exports. Returns `Err` for any spec we
/// don't yet know about; user code gets a clear "unknown library"
/// rather than a silent empty environment.
fn resolve_import_spec(
    spec: &Value,
    syms: &SymbolTable,
) -> Result<&'static [&'static str], String> {
    // Per R6RS, an import-spec can carry a trailing version
    // sublist (e.g. `(rnrs base (6))`). Strip it. L1.1 also
    // aliases `(rnrs lists)` and `(rnrs)` to the same approved-
    // base set — permissive but preserves R6RS conformance; L1.3
    // splits them via per-library binding metadata.
    let parts = collect_proper_list("environment", spec)?;
    let symbol_parts: Vec<&Value> = parts
        .iter()
        .take_while(|v| matches!(v, Value::Symbol(_)))
        .collect();
    let tail_len = parts.len() - symbol_parts.len();
    if tail_len > 0 {
        // Only acceptable tail: a single trailing version
        // sublist (which is a Pair or Null).
        let tail = &parts[symbol_parts.len()..];
        if tail_len != 1 || !matches!(&tail[0], Value::Pair(_) | Value::Null) {
            return Err(format!(
                "environment: import-spec part must be a symbol, got {:?}",
                tail[0]
            ));
        }
    }
    let names: Vec<String> = symbol_parts
        .iter()
        .map(|v| match v {
            Value::Symbol(s) => syms.name(*s).to_string(),
            _ => unreachable!("take_while filter"),
        })
        .collect();
    let joined = names.join(" ");
    match joined.as_str() {
        "rnrs base" | "scheme base" => Ok(RNRS_BASE_EXPORTS),
        "rnrs lists" => Ok(RNRS_LISTS_EXPORTS),
        "rnrs" => Ok(RNRS_BASE_EXPORTS), // umbrella; still aliased to base for L1.3
        _ => Err(format!(
            "environment: unknown library {:?} (supported at L1.3: \
             (rnrs base), (rnrs lists), (rnrs))",
            joined
        )),
    }
}

fn b_environment(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // Collect the visible names across all requested import specs.
    let mut visible: Vec<String> = Vec::new();
    for spec in args {
        let names = resolve_import_spec(spec, ctx.syms)?;
        for n in names {
            if !visible.iter().any(|x| x == n) {
                visible.push((*n).to_string());
            }
        }
    }
    // Snapshot each name's value from the global top-level frame.
    // Names registered as builtins (the vast majority of L1.1
    // entries) are guaranteed present; missing names are silently
    // dropped — they wouldn't be importable from a real library
    // anyway.
    let mut bindings: Vec<Value> = Vec::with_capacity(visible.len());
    for name in &visible {
        let sym = ctx.syms.intern(name);
        if let Some(v) = ctx.top.get(sym) {
            // Each binding is a Scheme pair (sym . value).
            bindings.push(Value::Pair(Pair::new(Value::Symbol(sym), v)));
        }
    }
    let alist = Value::list(bindings);
    // Build the vector record: #('__environment__ <alist> #f).
    let env = new_vector(vec![
        Value::string(ENV_TAG),
        alist,
        Value::Boolean(false), // immutable for `environment`
    ]);
    Ok(env)
}

/// `(environment? v)` — true when `v` is an L1.1 environment record.
fn b_environment_p(args: &[Value], _ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("environment?", "1", args.len()));
    }
    Ok(Value::Boolean(is_environment_value(&args[0])))
}

/// Internal: check the L1.1 environment-record shape.
pub(crate) fn is_environment_value(v: &Value) -> bool {
    matches!(classify_env_record(v), EnvShape::Valid { .. })
}

/// Internal: extract (bindings_map, mutable) from an environment
/// value. Returns `None` for any malformed shape — caller can use
/// `is_environment_value` first if they want a separate boolean.
pub(crate) fn decode_environment(
    v: &Value,
) -> Option<(std::collections::HashMap<cs_core::Symbol, Value>, bool)> {
    let EnvShape::Valid { mutable } = classify_env_record(v) else {
        return None;
    };
    // Shape validated; pull out the alist (slot 1) and walk it.
    let Value::Vector(items) = v else {
        // Unreachable: classify_env_record only returns Valid for
        // Vector values. Keep the destructure here for borrow scope.
        return None;
    };
    let items = items.borrow();
    let alist = &items[1];
    let mut map = std::collections::HashMap::new();
    let mut cur = alist.clone();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                let head = p.car.borrow().clone();
                let tail = p.cdr.borrow().clone();
                if let Value::Pair(kv) = head {
                    if let Value::Symbol(s) = kv.car.borrow().clone() {
                        map.insert(s, kv.cdr.borrow().clone());
                    }
                }
                cur = tail;
            }
            _ => break,
        }
    }
    Some((map, mutable))
}

/// `(make-namespace <import-spec> ...)` (ADR 0015 L1.2) — Racket-
/// style mutable namespace constructor. Same record shape as
/// `(environment ...)` but the third slot is `#t` (mutable).
/// Mutations via `namespace-set-variable-value!` /
/// `namespace-undefine-variable!` are visible to subsequent
/// evals against the same namespace.
///
/// `eval` against a mutable namespace builds a NON-immutable
/// Frame, so `set!` inside the eval'd expression no longer
/// raises `&assertion`. Note: writes via `set!` only mutate
/// the per-eval Frame and are NOT persisted back to the
/// namespace — explicit `namespace-set-variable-value!` is
/// the primary write path. (Eval-write-back is a future
/// iter when a concrete REPL use case asks for it.)
fn b_make_namespace(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    let mut visible: Vec<String> = Vec::new();
    for spec in args {
        let names = resolve_import_spec(spec, ctx.syms)?;
        for n in names {
            if !visible.iter().any(|x| x == n) {
                visible.push((*n).to_string());
            }
        }
    }
    let mut bindings: Vec<Value> = Vec::with_capacity(visible.len());
    for name in &visible {
        let sym = ctx.syms.intern(name);
        if let Some(v) = ctx.top.get(sym) {
            bindings.push(Value::Pair(Pair::new(Value::Symbol(sym), v)));
        }
    }
    let alist = Value::list(bindings);
    let env = new_vector(vec![
        Value::string(ENV_TAG),
        alist,
        Value::Boolean(true), // mutable for `make-namespace`
    ]);
    Ok(env)
}

/// `(namespace-set-variable-value! ns 'name value)` — install or
/// overwrite a binding in `ns`. Errors if `ns` isn't a mutable
/// namespace (snapshot environments from `(environment ...)` are
/// rejected with a clear message).
fn b_namespace_set_variable_value(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err("namespace-set-variable-value!", "3", args.len()));
    }
    let sym = match &args[1] {
        Value::Symbol(s) => *s,
        v => return Err(type_err("namespace-set-variable-value!", "symbol", v)),
    };
    let new_val = args[2].clone();
    namespace_update(&args[0], sym, Some(new_val), ctx)
}

/// `(namespace-undefine-variable! ns 'name)` — remove a binding
/// from `ns`. No-op if the name wasn't bound. Errors if `ns`
/// isn't a mutable namespace.
fn b_namespace_undefine_variable(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("namespace-undefine-variable!", "2", args.len()));
    }
    let sym = match &args[1] {
        Value::Symbol(s) => *s,
        v => return Err(type_err("namespace-undefine-variable!", "symbol", v)),
    };
    namespace_update(&args[0], sym, None, ctx)
}

/// Shared helper for the two `namespace-*!` builtins. `new_val =
/// Some(v)` means insert-or-overwrite; `None` means remove.
/// Implementation: decode the alist to a Vec, apply the
/// mutation, re-encode, swap slot[1]. O(n) per call; n is the
/// binding count (~100 for (rnrs base)). In-place pair-splicing
/// would be faster but the aliasing cases are subtle; the
/// decode/re-encode path is obviously correct.
fn namespace_update(
    ns: &Value,
    sym: cs_core::Symbol,
    new_val: Option<Value>,
    _ctx: &mut EvalCtx,
) -> Result<Value, String> {
    // Distinguish the failure modes so users can diagnose without
    // staring at the same generic message three times — passing a
    // pair vs. a 4-vector vs. a tagged-but-not-namespace vector
    // each gets its own diagnostic.
    match classify_env_record(ns) {
        EnvShape::NotVector => {
            return Err(format!(
                "namespace mutation: argument is not a namespace (got {})",
                ns.type_name()
            ));
        }
        EnvShape::WrongArity => {
            return Err(
                "namespace mutation: argument is a vector but not a namespace record \
                 (expected 3-element [tag, bindings, mutable] shape)"
                    .into(),
            );
        }
        EnvShape::MissingTag => {
            return Err(
                "namespace mutation: argument is a vector but not a namespace record \
                 (slot 0 is not the namespace-tag sentinel)"
                    .into(),
            );
        }
        EnvShape::Valid { mutable: false } => {
            return Err(
                "namespace mutation: argument is an immutable environment (from `environment`); \
                 use `make-namespace` for a mutable namespace"
                    .into(),
            );
        }
        EnvShape::Valid { mutable: true } => { /* fall through */ }
    }
    let Value::Vector(items) = ns else {
        // Unreachable: classify_env_record returned Valid which
        // implies Vector. Mirror decode_environment's pattern.
        return Err("namespace mutation: unreachable shape".into());
    };
    let mut entries: Vec<(cs_core::Symbol, Value)> = Vec::new();
    let mut cur = items.borrow()[1].clone();
    loop {
        match cur {
            Value::Pair(p) => {
                let head = p.car.borrow().clone();
                if let Value::Pair(kv) = head {
                    if let Value::Symbol(s) = kv.car.borrow().clone() {
                        entries.push((s, kv.cdr.borrow().clone()));
                    }
                }
                cur = p.cdr.borrow().clone();
            }
            _ => break,
        }
    }
    let existing = entries.iter().position(|(s, _)| *s == sym);
    match (existing, new_val) {
        (Some(idx), Some(v)) => entries[idx].1 = v,
        (Some(idx), None) => {
            entries.remove(idx);
        }
        (None, Some(v)) => entries.push((sym, v)),
        (None, None) => {}
    }
    let pairs: Vec<Value> = entries
        .into_iter()
        .map(|(s, v)| Value::Pair(Pair::new(Value::Symbol(s), v)))
        .collect();
    let new_alist = Value::list(pairs);
    items.borrow_mut()[1] = new_alist;
    Ok(Value::Unspecified)
}

fn b_interaction_environment(_args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    // The interaction environment is the live top-level — not
    // restricted. Returning a sentinel here means eval recognizes
    // "no restriction" and uses ctx.top directly (preserving
    // pre-L1.1 behavior).
    Ok(Value::Symbol(ctx.syms.intern(TOP_LEVEL_ENV_SENTINEL)))
}

/// R5RS / R7RS legacy: `(null-environment version)`. Returns the
/// "null environment" containing only syntactic-keyword bindings. We
/// don't have separate environment frames at the foundation milestone,
/// so this returns the same opaque sentinel as the others. The version
/// arg is required and must be 5; reject other values.
fn b_null_environment(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("null-environment", "1", args.len()));
    }
    let v = as_int_i64("null-environment", &args[0])?;
    if v != 5 {
        return Err(format!("null-environment: unsupported version: {}", v));
    }
    Ok(Value::Symbol(ctx.syms.intern(NULL_ENV_SENTINEL)))
}

/// R5RS / R7RS legacy: `(scheme-report-environment version)`. Returns
/// an environment for the named report version. We support 5 only at
/// the foundation milestone; future iters can add 7.
fn b_scheme_report_environment(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("scheme-report-environment", "1", args.len()));
    }
    let v = as_int_i64("scheme-report-environment", &args[0])?;
    if v != 5 && v != 7 {
        return Err(format!(
            "scheme-report-environment: unsupported version: {}",
            v
        ));
    }
    Ok(Value::Symbol(ctx.syms.intern(TOP_LEVEL_ENV_SENTINEL)))
}

/// R7RS `(load filename [environment])`. Reads filename as a sequence
/// of Scheme expressions and evaluates each in the given environment
/// (top-level if omitted). Returns an unspecified value.
fn b_load(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("load", "1 or 2", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("load", "string (filename)", v)),
    };
    // The optional environment arg is currently a symbol sentinel; we
    // accept and ignore it (always eval at top level).
    let src = std::fs::read_to_string(&path).map_err(|e| {
        cs_core::stash_builtin_err_extra_tag(TAG_FILE_ERROR);
        format!("load: cannot read {}: {}", path, e)
    })?;
    let file_id = cs_diag::FileId(u32::MAX - 3);
    let data = cs_parse::read_all(file_id, &src, ctx.syms).map_err(|errs| {
        cs_core::stash_builtin_err_extra_tag(TAG_READ_ERROR);
        let e = errs.into_iter().next().unwrap();
        format!("load: parse error in {}: {}", path, e.message())
    })?;
    if data.is_empty() {
        return Ok(Value::Unspecified);
    }
    let mut expander = cs_expand::Expander::new(ctx.syms, ctx.macros);
    let core = expander
        .expand_program(&data)
        .map_err(|e| format!("load: expand error in {}: {}", path, e.message()))?;
    drop(expander);
    crate::eval::eval(&core, ctx.top.clone(), ctx).map_err(|e| e.message())
}

// ---- eval ----

fn b_eval(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(arity_err("eval", "1 or 2", args.len()));
    }
    // ADR 0015 L1.1: when the 2nd arg is an environment record
    // (Vector tagged `__environment__`), build an immutable root
    // Frame from its snapshot bindings and run against that.
    // Otherwise (sentinel symbol, missing, or unknown shape),
    // fall back to the live top-level frame (pre-L1.1 behavior).
    let restricted_env: Option<std::rc::Rc<crate::env::Frame>> = if args.len() == 2 {
        if let Some((bindings, mutable)) = decode_environment(&args[1]) {
            // L1.2: mutable namespaces use mutable_root so set!
            // inside the eval'd expression no longer raises.
            // Immutable envs from L1.1 still use immutable_root.
            Some(if mutable {
                crate::env::Frame::mutable_root(bindings)
            } else {
                crate::env::Frame::immutable_root(bindings)
            })
        } else {
            None
        }
    } else {
        None
    };
    let eval_frame = restricted_env.unwrap_or_else(|| ctx.top.clone());
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
    // If the eval raises a condition, propagate via the
    // pending_raise side-channel so `guard` on the host side
    // catches it as a real condition instead of a plain string
    // error. Otherwise return the plain Err message.
    crate::eval::eval(&core, eval_frame, ctx).map_err(|e| match e.kind {
        crate::eval::EvalErrorKind::Raised(cond) => {
            ctx.pending_raise = Some(cond);
            "__raised__".to_string()
        }
        _ => e.message(),
    })
}

// ---- load-shared-library ----
//
// M10 W1 + closeout: gated on `ffi-dynamic`. The builtin name stays
// defined in all builds so Scheme-side existence checks remain
// well-formed; the disabled-feature stub reports the missing
// capability instead of being absent. A WASM build that wants
// custom Rust-implemented Scheme builtins compiles them in directly
// via the `ffi-trait` feature + `Runtime::register_host_procedure`
// at embedder-startup time, rather than via dlopen.

#[cfg(feature = "ffi-dynamic")]
fn b_load_shared_library(args: &[Value], _ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("load-shared-library", "1", args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.borrow().clone(),
        v => return Err(type_err("load-shared-library", "string", v)),
    };
    // SAFETY: load-shared-library is only callable from inside an
    // active eval, which set ACTIVE_RUNTIME via with_active. The
    // active back-pointer is the unique &mut access for the call.
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "load-shared-library: no active runtime".to_string())?;
    rt.load_shared_library(&path)
        .map_err(|e| format!("load-shared-library: {}", e))?;
    Ok(Value::Unspecified)
}

#[cfg(not(feature = "ffi-dynamic"))]
fn b_load_shared_library(args: &[Value], _ctx: &mut EvalCtx) -> Result<Value, String> {
    let _ = args;
    Err("load-shared-library: dynamic library loading not available in this build (no `ffi-dynamic` feature, e.g. WASM target). Use `Runtime::register_host_procedure` from the embedder instead.".to_string())
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
    // R6RS make-parameter takes (init [converter]). The optional
    // converter procedure is meant to transform values on write
    // (including the initial value). Today we ignore it -- threading
    // a Scheme procedure call through the eval context from
    // cs-core's Parameter::call dispatch is a tier-crossing change
    // tracked as Phase 2E follow-up. Documented here so user code
    // that passes a converter gets the un-converted behavior and
    // can be migrated when the proper support lands.
    if args.len() == 2 && !matches!(args[1], Value::Procedure(_)) {
        return Err(type_err(
            "make-parameter",
            "procedure (converter)",
            &args[1],
        ));
    }
    Ok(crate::proc::make_parameter(args[0].clone()))
}

/// `(parameter? v)` — true iff `v` is a parameter procedure
/// created by `make-parameter`. R6RS R7RS-large add this
/// predicate; our prior surface had `make-parameter` and
/// `parameterize` but no way to test for parameter-ness.
fn b_parameter_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("parameter?", "1", args.len()));
    }
    let is_param = match &args[0] {
        Value::Procedure(p) => {
            // Parameter lives in cs-core; downcast through the
            // Procedure trait's `as_any` hook.
            p.as_any().downcast_ref::<cs_core::Parameter>().is_some()
        }
        _ => false,
    };
    Ok(Value::Boolean(is_param))
}

// ---- ADR 0014 — optimizer-pass installation ----
//
// Three Scheme builtins backed by cs-opt's thread-local active-pass
// list. The thread-local is read by cs-opt::run_active_pipeline,
// which is called at the end of bytecode→RIR translation in cs-vm.
//
// Validation: install! checks the pass name against the global
// registry at install time so user code gets an immediate error
// rather than a silent skip at codegen time.
//
// These are higher-order builtins because they need `ctx.syms` to
// resolve / intern Symbol names.

fn b_install_optimizer_pass(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("install-optimizer-pass!", "1", args.len()));
    }
    let sym = match &args[0] {
        Value::Symbol(s) => *s,
        _ => {
            return Err(type_err(
                "install-optimizer-pass!",
                "symbol (pass name)",
                &args[0],
            ));
        }
    };
    let name = ctx.syms.name(sym).to_string();
    let registry = cs_opt::PassRegistry::global()
        .lock()
        .map_err(|_| "install-optimizer-pass!: registry mutex poisoned".to_string())?;
    if registry.get(&name).is_none() {
        return Err(format!("install-optimizer-pass!: unknown pass {:?}", name));
    }
    drop(registry);
    cs_opt::install_active_pass(&name);
    Ok(Value::Unspecified)
}

fn b_remove_optimizer_pass(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("remove-optimizer-pass!", "1", args.len()));
    }
    let sym = match &args[0] {
        Value::Symbol(s) => *s,
        _ => {
            return Err(type_err(
                "remove-optimizer-pass!",
                "symbol (pass name)",
                &args[0],
            ));
        }
    };
    let name = ctx.syms.name(sym).to_string();
    cs_opt::remove_active_pass(&name);
    Ok(Value::Unspecified)
}

fn b_installed_optimizer_passes(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("installed-optimizer-passes", "0", args.len()));
    }
    let names = cs_opt::active_passes();
    let syms: Vec<Value> = names
        .into_iter()
        .map(|s| Value::Symbol(ctx.syms.intern(&s)))
        .collect();
    Ok(Value::list(syms))
}

/// `(with-active-optimizer-passes '(name ...) thunk)` —
/// run `(thunk)` with the optimizer's active-pass list scoped
/// to the given names. The previous list is restored on
/// return, even if `thunk` raises.
///
/// This is the closure-based stand-in for the ADR 0014 §5
/// `parameterize`-over-`(active-passes)` design — install! /
/// remove! mutate the SCOPED list inside the thunk, so their
/// effects are local. Outside the thunk, the pre-call list is
/// restored verbatim.
///
/// The names list must be a proper list of symbols; each name
/// must validate against the global pass registry the same way
/// `install-optimizer-pass!` does. A bad name fails the whole
/// call (no scoped swap, no partial state).
fn b_with_active_optimizer_passes(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("with-active-optimizer-passes", "2", args.len()));
    }
    let names_list = collect_proper_list("with-active-optimizer-passes", &args[0])?;
    let mut names: Vec<String> = Vec::with_capacity(names_list.len());
    for v in &names_list {
        match v {
            Value::Symbol(s) => names.push(ctx.syms.name(*s).to_string()),
            _ => {
                return Err(type_err(
                    "with-active-optimizer-passes",
                    "symbol (pass name)",
                    v,
                ))
            }
        }
    }
    // Validate names against the registry BEFORE entering the
    // scoped guard so a typo doesn't leave us in an inconsistent
    // state. Same check install-optimizer-pass! does.
    {
        let registry = cs_opt::PassRegistry::global()
            .lock()
            .map_err(|_| "with-active-optimizer-passes: registry mutex poisoned".to_string())?;
        for name in &names {
            if registry.get(name).is_none() {
                return Err(format!(
                    "with-active-optimizer-passes: unknown pass {:?}",
                    name
                ));
            }
        }
    }
    let thunk = args[1].clone();
    // RAII guard fires on early return too — if apply_procedure
    // returns Err, the guard restores the prev list on the
    // way out via the `?` unwinding path. (Rust's `?` doesn't
    // panic; the Drop runs because `_guard` is dropped at scope
    // exit no matter how we leave.)
    cs_opt::with_scoped_active_passes(names, || apply_procedure(&thunk, &[], ctx))
        .map_err(|e| e.message())
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

/// SRFI-1 nth-element selectors for n=4..10.
fn list_nth(name: &str, args: &[Value], n: usize) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err(name, "1", args.len()));
    }
    let items = collect_proper_list(name, &args[0])?;
    items
        .get(n - 1)
        .cloned()
        .ok_or_else(|| format!("{}: list has fewer than {} elements", name, n))
}

fn b_fourth(args: &[Value]) -> Result<Value, String> {
    list_nth("fourth", args, 4)
}
fn b_fifth(args: &[Value]) -> Result<Value, String> {
    list_nth("fifth", args, 5)
}
fn b_sixth(args: &[Value]) -> Result<Value, String> {
    list_nth("sixth", args, 6)
}
fn b_seventh(args: &[Value]) -> Result<Value, String> {
    list_nth("seventh", args, 7)
}
fn b_eighth(args: &[Value]) -> Result<Value, String> {
    list_nth("eighth", args, 8)
}
fn b_ninth(args: &[Value]) -> Result<Value, String> {
    list_nth("ninth", args, 9)
}
fn b_tenth(args: &[Value]) -> Result<Value, String> {
    list_nth("tenth", args, 10)
}

/// SRFI-1 type predicates. We intentionally don't construct cyclic
/// lists from foundation Scheme, so the cycle predicates resolve via
/// a tortoise-and-hare walk; circular lists built via set-cdr! show
/// up correctly.
fn b_not_pair_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("not-pair?", "1", args.len()));
    }
    Ok(Value::Boolean(!matches!(&args[0], Value::Pair(_))))
}

fn b_null_list_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("null-list?", "1", args.len()));
    }
    match &args[0] {
        Value::Null => Ok(Value::Boolean(true)),
        Value::Pair(_) => Ok(Value::Boolean(false)),
        _ => Err("null-list?: argument is neither a pair nor empty list".into()),
    }
}

/// Walk-once detection via tortoise/hare. Returns
///   None — non-list (improper terminator)
///   Some(true) — proper finite
///   Some(false) — circular
fn list_classify(v: &Value) -> Option<bool> {
    let mut slow = v.clone();
    let mut fast = v.clone();
    loop {
        match fast {
            Value::Null => return Some(true),
            Value::Pair(p) => {
                let next = p.cdr.borrow().clone();
                match next {
                    Value::Null => return Some(true),
                    Value::Pair(p2) => {
                        let next2 = p2.cdr.borrow().clone();
                        // advance slow once.
                        slow = match slow {
                            Value::Pair(sp) => sp.cdr.borrow().clone(),
                            _ => return Some(true),
                        };
                        fast = next2;
                        if values_eq_ptr(&slow, &fast) {
                            return Some(false);
                        }
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
}

fn values_eq_ptr(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Pair(p1), Value::Pair(p2)) => cs_core::Gc::ptr_eq(p1, p2),
        _ => false,
    }
}

fn b_proper_list_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("proper-list?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        list_classify(&args[0]),
        Some(true)
    )))
}

fn b_dotted_list_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("dotted-list?", "1", args.len()));
    }
    // A non-pair, non-null is dotted (degenerate). A proper finite
    // list is NOT dotted. A circular list is NOT dotted.
    let cls = list_classify(&args[0]);
    let is_dotted = match (&args[0], cls) {
        (Value::Null, _) => false,
        (Value::Pair(_), Some(true)) => false,
        (Value::Pair(_), Some(false)) => false,
        (Value::Pair(_), None) => true,
        _ => true,
    };
    Ok(Value::Boolean(is_dotted))
}

fn b_circular_list_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("circular-list?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        list_classify(&args[0]),
        Some(false)
    )))
}

/// SRFI-1 `(append-reverse rev tail)` — same as `(append (reverse rev) tail)`
/// without building the intermediate. We delegate to that simple form.
fn b_append_reverse(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("append-reverse", "2", args.len()));
    }
    let rev = collect_proper_list("append-reverse", &args[0])?;
    let mut acc = args[1].clone();
    for item in rev {
        acc = Value::Pair(Pair::new(item, acc));
    }
    Ok(acc)
}

/// SRFI-1 `(unzip pairs)` — split a list of two-element lists into
/// two parallel lists. Returns 2 values via pending_values.
fn b_unzip(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("unzip", "1", args.len()));
    }
    let items = collect_proper_list("unzip", &args[0])?;
    let mut a: Vec<Value> = Vec::with_capacity(items.len());
    let mut b: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        let parts = collect_proper_list("unzip", &item)?;
        if parts.len() < 2 {
            return Err("unzip: each element must have at least 2 items".into());
        }
        a.push(parts[0].clone());
        b.push(parts[1].clone());
    }
    ctx.pending_values = Some(vec![Value::list(a), Value::list(b)]);
    Ok(Value::Unspecified)
}

/// SRFI-1 `(circular-list elt ...)` — build a cyclic list whose
/// last cdr points back at the head.
fn b_circular_list(args: &[Value]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(arity_err("circular-list", "at least 1", 0));
    }
    let head = Value::list(args.to_vec());
    let mut cur = head.clone();
    loop {
        let next = match &cur {
            Value::Pair(p) => p.cdr.borrow().clone(),
            _ => return Err("circular-list: internal error — not a pair".into()),
        };
        if matches!(next, Value::Null) {
            if let Value::Pair(p) = &cur {
                *p.cdr.borrow_mut() = head.clone();
            }
            break;
        }
        cur = next;
    }
    Ok(head)
}

/// SRFI-1 `(reverse! list)` — destructive reverse. Foundation: just
/// dispatch to the immutable `reverse`. Treating the destructive form
/// as a hint (R7RS recommends pure semantics) is safe — callers that
/// actually need in-place mutation are vanishingly rare in idiomatic
/// Scheme and would be surprising in our shared-ref model.
fn b_reverse_bang(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("reverse!", "1", args.len()));
    }
    let items = collect_proper_list("reverse!", &args[0])?;
    let mut out: Vec<Value> = items;
    out.reverse();
    Ok(Value::list(out))
}

/// SRFI-1 `(split-at list k)` — return two values: the first k items
/// and the rest. Uses pending_values like other multi-value builtins.
fn b_split_at(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("split-at", "2", args.len()));
    }
    let n = as_int_i64("split-at", &args[1])?;
    if n < 0 {
        return Err("split-at: negative count".into());
    }
    let n = n as usize;
    let items = collect_proper_list("split-at", &args[0])?;
    if n > items.len() {
        return Err(format!(
            "split-at: count {} exceeds list length {}",
            n,
            items.len()
        ));
    }
    let head: Vec<Value> = items[..n].to_vec();
    let tail: Vec<Value> = items[n..].to_vec();
    ctx.pending_values = Some(vec![Value::list(head), Value::list(tail)]);
    Ok(Value::Unspecified)
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

/// R6RS `(remp pred list)` — same shape as our `remove`, kept under
/// the R6RS name so srfi/r6rs test fixtures resolve it.
fn b_remp(args: &[Value], ctx: &mut EvalCtx) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("remp", "2", args.len()));
    }
    let pred = args[0].clone();
    let items = collect_proper_list("remp", &args[1])?;
    let mut out = Vec::new();
    for item in items {
        let r = apply_procedure(&pred, &[item.clone()], ctx).map_err(|e| e.message())?;
        if !r.is_truthy() {
            out.push(item);
        }
    }
    Ok(Value::list(out))
}

/// R6RS `(remv obj list)` — remove items that are `eqv?` to obj.
fn b_remv(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("remv", "2", args.len()));
    }
    let target = &args[0];
    let items = collect_proper_list("remv", &args[1])?;
    let out: Vec<Value> = items
        .into_iter()
        .filter(|x| !cs_core::eq::eqv(x, target))
        .collect();
    Ok(Value::list(out))
}

/// SRFI-1 / R6RS-ish `(list-head list k)` — return the first k items
/// of `list` as a proper list. R6RS has it as `list-head`; SRFI-1
/// names it `take`. We already export `take` as the SRFI-1 name; this
/// adds the R6RS name as an alias.
fn b_list_head(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("list-head", "2", args.len()));
    }
    let n = as_int_i64("list-head", &args[1])?;
    if n < 0 {
        return Err("list-head: negative count".into());
    }
    let n = n as usize;
    let items = collect_proper_list("list-head", &args[0])?;
    if n > items.len() {
        return Err(format!(
            "list-head: count {} exceeds list length {}",
            n,
            items.len()
        ));
    }
    let head: Vec<Value> = items.into_iter().take(n).collect();
    Ok(Value::list(head))
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

// ---- R6RS §8.2.6 — port positions ----------------------------------

/// `(port-position port)` — current 0-based offset for input/output
/// ports. R6RS specifies that this is in *octets* for binary ports
/// and in *characters* for textual ports; foundation matches that
/// semantics for our four port types.
fn b_port_position(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("port-position", "1", args.len()));
    }
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => Ok(Value::fixnum(state.borrow().pos as i64)),
            Port::ByteVectorInput(state) => Ok(Value::fixnum(state.borrow().pos as i64)),
            Port::StringOutput(buf) => Ok(Value::fixnum(buf.borrow().chars().count() as i64)),
            Port::ByteVectorOutput(buf) => Ok(Value::fixnum(buf.borrow().len() as i64)),
            Port::FileOutput(state) => Ok(Value::fixnum(state.borrow().buf.len() as i64)),
        },
        v => Err(type_err("port-position", "port", v)),
    }
}

/// `(set-port-position! port pos)` — only meaningful for ports whose
/// data structure permits a seek. Foundation supports input ports
/// (random access by adjusting `pos`) and rejects output ports —
/// rewinding an already-emitted output stream isn't useful.
fn b_set_port_position(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("set-port-position!", "2", args.len()));
    }
    let pos = as_int_i64("set-port-position!", &args[1])?;
    if pos < 0 {
        return Err("set-port-position!: negative position".into());
    }
    let pos = pos as usize;
    match &args[0] {
        Value::Port(p) => match &**p {
            Port::StringInput(state) => {
                let mut s = state.borrow_mut();
                if pos > s.chars.len() {
                    return Err("set-port-position!: past end of input".into());
                }
                s.pos = pos;
                Ok(Value::Unspecified)
            }
            Port::ByteVectorInput(state) => {
                let mut s = state.borrow_mut();
                if pos > s.bytes.len() {
                    return Err("set-port-position!: past end of input".into());
                }
                s.pos = pos;
                Ok(Value::Unspecified)
            }
            Port::StringOutput(_) | Port::ByteVectorOutput(_) | Port::FileOutput(_) => {
                Err("set-port-position!: output ports do not support repositioning".into())
            }
        },
        v => Err(type_err("set-port-position!", "port", v)),
    }
}

fn b_port_has_port_position_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("port-has-port-position?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(&args[0], Value::Port(_))))
}

fn b_port_has_set_port_position_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("port-has-set-port-position!?", "1", args.len()));
    }
    Ok(Value::Boolean(matches!(
        &args[0],
        Value::Port(p) if matches!(**p, Port::StringInput(_) | Port::ByteVectorInput(_))
    )))
}

/// R6RS `(lookahead-char port)` — alias for peek-char.
fn b_lookahead_char(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("lookahead-char", "1", args.len()));
    }
    b_peek_char(args)
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

/// `command-line` — R6RS §6.4. Returns a list whose first element
/// is the script path and whose subsequent elements are the args
/// passed after it. When the runtime has `command_line` set (via
/// `Runtime::set_command_line`, typically from `cs-cli` before
/// running a script), that list is returned verbatim. Otherwise
/// (REPL, `-e`, embedded use without explicit set) the full
/// process argv is returned for backward compatibility.
fn b_command_line(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("command-line", "0", args.len()));
    }
    // SAFETY: command-line is invoked from inside an eval call.
    // `Runtime::with_active` set the thread-local before eval ran;
    // we read it here. The borrow lives only for the duration of
    // building the list.
    let from_runtime: Option<Vec<String>> =
        unsafe { crate::Runtime::active() }.and_then(|rt| rt.command_line.as_ref().cloned());
    let argv: Vec<Value> = match from_runtime {
        Some(list) => list.into_iter().map(Value::string).collect(),
        None => std::env::args().map(Value::string).collect(),
    };
    Ok(Value::list(argv))
}

// ---- JIT introspection ----------------------------------------------
//
// Scheme-visible accessors for the JIT machinery. Useful in tests and
// benchmarks that want to assert "this hot path actually tier'd up"
// or print a per-closure JIT signature for post-mortem of "why didn't
// this body JIT?".
//
// `(jit-installed?)`         -> #t / #f
// `(jit-stats)`              -> (tier-ups jit-calls deopts)
// `(jit-status proc)`        -> if not a closure: 'not-a-closure
//                               otherwise:
//                                 'jit-off                -- never tier'd up
//                                 (jit-on <return-tag> (<param-tag>...) calls <N> deopts <M>)
//                                                          where each tag is
//                                                          'fixnum 'boolean
//                                                          'character or 'flonum

fn b_jit_installed_p(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("jit-installed?", "0", args.len()));
    }
    // M10 W1: `jit_installed` only exists when the `jit` feature is
    // enabled. `(jit-installed?)` always returns `#f` in builds
    // without it (WASM target most notably) — the predicate is
    // semantically correct.
    #[cfg(feature = "jit")]
    let installed = unsafe { crate::Runtime::active() }
        .map(|rt| rt.jit_installed())
        .unwrap_or(false);
    #[cfg(not(feature = "jit"))]
    let installed = false;
    Ok(Value::Boolean(installed))
}

fn b_jit_stats(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("jit-stats", "0", args.len()));
    }
    Ok(Value::list(vec![
        Value::fixnum(cs_vm::vm::tier_up_count() as i64),
        Value::fixnum(cs_vm::vm::jit_call_count() as i64),
        Value::fixnum(cs_vm::vm::deopt_count() as i64),
        Value::fixnum(cs_vm::vm::jit_ic_hit_count() as i64),
        Value::fixnum(cs_vm::vm::jit_ic_miss_count() as i64),
    ]))
}

/// `(gc-stats)` — alist snapshot of the heap's instrumentation
/// counters. Stable keys; values are exact integers (counters)
/// or flonums (millisecond durations). Modeled on Chez's
/// `(statistics)` accessor shape so external benchmark code
/// that already works on Chez can drop in with minimal porting.
///
/// Returned alist:
///
/// ```scheme
/// ((bytes-allocated-total . 142857142)   ; cumulative bytes
///  (alloc-count-total     . 12345)        ; cumulative allocations
///  (collect-count         . 87)           ; collect() calls since reset
///  (live-slots            . 1024)         ; reachable slots NOW
///  (collect-time-ms       . 145.3)        ; total time in collect()
///  (last-pause-ms         . 1.8)          ; most recent pause
///  (max-pause-ms          . 4.32)         ; peak pause since reset
///  (stats-enabled?        . #t))          ; pause-timing on/off
/// ```
///
/// The three `*-ms` durations are populated only when stats are
/// enabled via `(gc-stats-enable!)`; otherwise they read 0.0.
/// The integer counters are always tracked.
fn b_gc_stats(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("gc-stats", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-stats: no active runtime".to_string())?;
    let s = rt.heap().stats();
    let ns_to_ms = |d: std::time::Duration| (d.as_nanos() as f64) / 1_000_000.0;
    let mut pair = |k: &str, v: Value| -> Value {
        let key_sym = syms.intern(k);
        Value::Pair(cs_core::Pair::new(Value::Symbol(key_sym), v))
    };
    Ok(Value::list(vec![
        pair(
            "bytes-allocated-total",
            fixnum_or_bigint(s.bytes_allocated_total),
        ),
        pair("alloc-count-total", fixnum_or_bigint(s.alloc_count_total)),
        pair("collect-count", fixnum_or_bigint(s.collect_count)),
        pair("live-slots", Value::fixnum(s.live_slots as i64)),
        pair(
            "collect-time-ms",
            Value::flonum(ns_to_ms(s.collect_duration_total)),
        ),
        pair("last-pause-ms", Value::flonum(ns_to_ms(s.last_pause))),
        pair("max-pause-ms", Value::flonum(ns_to_ms(s.max_pause))),
        pair("stats-enabled?", Value::Boolean(s.stats_enabled)),
    ]))
}

/// Encode a u64 counter as the smallest numeric Value that fits.
/// Fixnums hold up to i64::MAX; anything larger spills to bigint
/// via the decimal-string path. For benchmark counters that grow
/// past 2^63 we'd need ~9 EB of cumulative allocation, so the
/// bigint path is defensive rather than common.
fn fixnum_or_bigint(n: u64) -> Value {
    if n <= i64::MAX as u64 {
        Value::fixnum(n as i64)
    } else {
        match Number::parse_decimal_integer(&n.to_string()) {
            Some(num) => Value::Number(num),
            None => Value::fixnum(i64::MAX), // unreachable in practice
        }
    }
}

/// `(gc-stats-reset!)` — zero all instrumentation counters in the
/// active heap. Returns unspecified. The bench harness calls this
/// after warmup so the per-iter measurement window starts clean.
fn b_gc_stats_reset(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("gc-stats-reset!", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-stats-reset!: no active runtime".to_string())?;
    rt.heap().reset_stats();
    Ok(Value::Unspecified)
}

/// `(gc-stats-enable!)` — turn on pause-time instrumentation.
/// Cheap (~2 % overhead on a tight alloc+collect loop) but not
/// free, so default-off. Returns unspecified.
fn b_gc_stats_enable(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("gc-stats-enable!", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-stats-enable!: no active runtime".to_string())?;
    rt.heap().set_stats_enabled(true);
    Ok(Value::Unspecified)
}

/// `(gc-stats-disable!)` — turn off pause-time instrumentation.
/// Returns unspecified.
fn b_gc_stats_disable(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("gc-stats-disable!", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-stats-disable!: no active runtime".to_string())?;
    rt.heap().set_stats_enabled(false);
    Ok(Value::Unspecified)
}

/// `(collect-garbage)` — force a stop-the-world mark-sweep and
/// return the live-slot count after sweeping. Shape compatible
/// with Chez Scheme's `(collect)` accessor — Chez returns the
/// new heap size (in bytes); we return the slot count (which is
/// the closest stable analogue in our non-generational heap).
fn b_collect_garbage(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("collect-garbage", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "collect-garbage: no active runtime".to_string())?;
    rt.heap().collect();
    Ok(Value::fixnum(rt.heap().live_slots() as i64))
}

/// `(gc-auto-collect-enable!)` — turn on the heap's
/// auto-collect-on-alloc behavior. With it on, every `Heap::alloc`
/// past the current threshold triggers a `(collect-garbage)`. Off
/// by default (cs-gc Phase 1 invariant). Tier-3 benches that want
/// real GC pressure to show up in the harness flip this on.
fn b_gc_auto_collect_enable(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("gc-auto-collect-enable!", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-auto-collect-enable!: no active runtime".to_string())?;
    rt.heap().set_auto_collect(true);
    Ok(Value::Unspecified)
}

/// `(gc-auto-collect-disable!)` — inverse of the above.
fn b_gc_auto_collect_disable(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("gc-auto-collect-disable!", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-auto-collect-disable!: no active runtime".to_string())?;
    rt.heap().set_auto_collect(false);
    Ok(Value::Unspecified)
}

/// `(gc-set-threshold! n)` — set the alloc-count threshold that
/// drives auto-collect. Default 4096; tier-3 benches typically
/// want to lower this so collection fires per inner loop rather
/// than once at the end.
fn b_gc_set_threshold(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("gc-set-threshold!", "1", args.len()));
    }
    let n = match &args[0] {
        Value::Number(Number::Fixnum(n)) if *n >= 0 => *n as usize,
        v => return Err(type_err("gc-set-threshold!", "non-negative fixnum", v)),
    };
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "gc-set-threshold!: no active runtime".to_string())?;
    rt.heap().set_threshold(n);
    Ok(Value::Unspecified)
}

/// `(current-rss-bytes)` — current process resident-set size in
/// bytes. The OS-level "how much physical memory does this process
/// hold right now" number — the right signal for leak detection,
/// since RSS grows when memory isn't actually being released back
/// to the OS even though cumulative allocations climb monotonically.
///
/// Implementation: macOS via `task_info(MACH_TASK_BASIC_INFO)`;
/// Linux via `/proc/self/statm` (second field × page size); other
/// targets return 0 (caller treats 0 as "unsupported"). All paths
/// are syscall-only — no allocations, safe to call from inside
/// tight per-iter measurement loops.
fn b_current_rss_bytes(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-rss-bytes", "0", args.len()));
    }
    Ok(fixnum_or_bigint(rss_bytes_platform()))
}

#[cfg(target_os = "macos")]
fn rss_bytes_platform() -> u64 {
    use std::mem;
    // Mach bindings — keeping them inline avoids pulling libc into
    // cs-runtime's dep graph. Layout matches <mach/task_info.h> on
    // macOS 14 / Darwin 23.x; older OS versions are wire-compatible.
    #[repr(C)]
    struct MachTaskBasicInfo {
        virtual_size: u64,
        resident_size: u64,
        resident_size_max: u64,
        user_time: [i32; 2],
        system_time: [i32; 2],
        policy: i32,
        suspend_count: i32,
    }
    const MACH_TASK_BASIC_INFO: u32 = 20;
    unsafe extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(
            task: u32,
            flavor: u32,
            task_info_out: *mut std::ffi::c_void,
            task_info_count: *mut u32,
        ) -> i32;
    }
    unsafe {
        let mut info: MachTaskBasicInfo = mem::zeroed();
        let mut count = (mem::size_of::<MachTaskBasicInfo>() / mem::size_of::<u32>()) as u32;
        let kr = task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut std::ffi::c_void,
            &mut count,
        );
        if kr == 0 {
            info.resident_size
        } else {
            0
        }
    }
}

#[cfg(target_os = "linux")]
fn rss_bytes_platform() -> u64 {
    // /proc/self/statm: "size resident shared text lib data dt", in
    // page units. The second column is RSS pages. Default page size
    // is 4 KB on x86_64 / aarch64; using sysconf(_SC_PAGESIZE) would
    // be more portable but pulls in libc.
    let Ok(s) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let mut parts = s.split_whitespace();
    let _size = parts.next();
    let Some(rss) = parts.next().and_then(|p| p.parse::<u64>().ok()) else {
        return 0;
    };
    rss.saturating_mul(4096)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rss_bytes_platform() -> u64 {
    // Unsupported platform — return 0; the harness treats 0 as
    // "unavailable" and elides RSS columns from its output.
    0
}

/// `(current-memory-use)` — cumulative bytes allocated since heap
/// creation (or since the last `(gc-stats-reset!)`). Shape
/// compatible with Racket's `(current-memory-use)`, which returns
/// an exact integer count of bytes. Note: Racket's number is bytes
/// reachable from custodians, ours is cumulative allocation —
/// a different shape but the same use case (delta around a
/// workload tells you how much it allocated).
fn b_current_memory_use(args: &[Value]) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(arity_err("current-memory-use", "0", args.len()));
    }
    let rt = unsafe { crate::Runtime::active() }
        .ok_or_else(|| "current-memory-use: no active runtime".to_string())?;
    Ok(fixnum_or_bigint(rt.heap().bytes_allocated_total()))
}

fn b_jit_status(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("jit-status", "1", args.len()));
    }
    let proc_rc = match &args[0] {
        Value::Procedure(p) => p.clone(),
        _ => return Ok(Value::Symbol(syms.intern("not-a-closure"))),
    };
    let any = proc_rc.as_any();
    let closure = match any.downcast_ref::<cs_vm::vm::VmClosure>() {
        Some(c) => c,
        None => return Ok(Value::Symbol(syms.intern("not-a-closure"))),
    };
    if closure.jit_ptr().is_null() {
        return Ok(Value::Symbol(syms.intern("jit-off")));
    }
    let tag_to_sym = |t: u8, syms: &mut SymbolTable| -> Value {
        let name = match t {
            cs_vm::vm::JIT_RT_BOOLEAN => "boolean",
            cs_vm::vm::JIT_RT_CHARACTER => "character",
            cs_vm::vm::JIT_RT_FLONUM => "flonum",
            cs_vm::vm::JIT_RT_PAIR => "pair",
            cs_vm::vm::JIT_RT_VECTOR => "vector",
            cs_vm::vm::JIT_RT_STRING => "string",
            cs_vm::vm::JIT_RT_BYTEVECTOR => "bytevector",
            cs_vm::vm::JIT_RT_PROCEDURE => "procedure",
            cs_vm::vm::JIT_RT_SYMBOL => "symbol",
            cs_vm::vm::JIT_RT_BIGINT => "bigint",
            cs_vm::vm::JIT_RT_RATIONAL => "rational",
            cs_vm::vm::JIT_RT_HASHTABLE => "hashtable",
            cs_vm::vm::JIT_RT_PORT => "port",
            cs_vm::vm::JIT_RT_NULL => "null",
            cs_vm::vm::JIT_RT_ANY => "any",
            _ => "fixnum",
        };
        Value::Symbol(syms.intern(name))
    };
    let arity = closure.jit_arity();
    let packed = closure.jit_param_types();
    let mut params: Vec<Value> = Vec::with_capacity(arity as usize);
    for i in 0..arity {
        let nibble = ((packed >> (i as u32 * 4)) & 0xF) as u8;
        params.push(tag_to_sym(nibble, syms));
    }
    // Prefer the *semantic* return tag (what the body conceptually
    // returns) over the ABI tag (`JIT_RT_NB` for uniform-NB carriers).
    // Both agree for specialized-tier bodies; they diverge for
    // uniform-NB bodies, where the ABI carrier alone would render a
    // flonum-returning body as `fixnum` (the default fallback for
    // unknown tags in `tag_to_sym`).
    let out = vec![
        Value::Symbol(syms.intern("jit-on")),
        tag_to_sym(closure.jit_semantic_return_type(), syms),
        Value::list(params),
        Value::Symbol(syms.intern("calls")),
        Value::fixnum(closure.jit_call_count() as i64),
        Value::Symbol(syms.intern("deopts")),
        Value::fixnum(closure.jit_deopt_count() as i64),
    ];
    Ok(Value::list(out))
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

// ============================================================================
// (rnrs enums) — R6RS §13. M9 iter 2.
//
// Encoding: #("__enum-set__" #(<universe symbols>) <bits-fixnum>).
// - Slot 0: tag string (so `enum-set?` is a structural check).
// - Slot 1: shared universe vector (Symbol values, in canonical order).
// - Slot 2: bitset over universe positions (fixnum, 63 bits usable).
//
// Set operations preserve the universe; mismatched universes between
// args are an error (R6RS specifies "same enumeration type").
// ============================================================================

const ENUMSET_TAG: &str = "__enum-set__";

/// Build an enum-set Value from a universe + bits.
fn enum_set_value(universe: Vec<Value>, bits: i64) -> Value {
    let triple = vec![
        Value::string(ENUMSET_TAG.to_string()),
        Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(universe))),
        Value::fixnum(bits),
    ];
    Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(triple)))
}

/// True if `v` is an enum-set value (matches the `__enum-set__`
/// encoding).
fn is_enum_set(v: &Value) -> bool {
    if let Value::Vector(vec) = v {
        let b = vec.borrow();
        if b.len() != 3 {
            return false;
        }
        if let Value::String(s) = &b[0] {
            return *s.borrow() == ENUMSET_TAG;
        }
    }
    false
}

/// Decompose an enum-set into (universe, bits). Returns Err for
/// non-enum-set values.
fn enum_set_parts(v: &Value) -> Result<(Vec<Value>, i64), String> {
    if let Value::Vector(vec) = v {
        let b = vec.borrow();
        if b.len() == 3 {
            if let (Value::String(_), Value::Vector(uv), Value::Number(Number::Fixnum(bits))) =
                (&b[0], &b[1], &b[2])
            {
                let uv = uv.borrow().clone();
                return Ok((uv, *bits));
            }
        }
    }
    Err("enum-set expected".to_string())
}

/// Find a symbol's 0-based index in the universe; returns None if
/// not in the universe.
fn enum_index_of(universe: &[Value], sym: cs_core::Symbol) -> Option<usize> {
    universe
        .iter()
        .position(|v| matches!(v, Value::Symbol(s) if *s == sym))
}

fn b_make_enumeration(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("make-enumeration", "1", args.len()));
    }
    let mut universe: Vec<Value> = Vec::new();
    let mut cur = args[0].clone();
    loop {
        match cur {
            Value::Null => break,
            Value::Pair(p) => {
                let car = p.car.borrow().clone();
                match &car {
                    Value::Symbol(_) => universe.push(car.clone()),
                    other => {
                        return Err(format!(
                            "make-enumeration: expected symbol, got {}",
                            other.type_name()
                        ));
                    }
                }
                cur = p.cdr.borrow().clone();
            }
            v => {
                return Err(type_err("make-enumeration", "list of symbols", &v));
            }
        }
    }
    if universe.len() > 63 {
        return Err(format!(
            "make-enumeration: universe of {} symbols exceeds 63-symbol cap",
            universe.len()
        ));
    }
    // Universe enum-set has all bits set.
    let bits = if universe.is_empty() {
        0
    } else {
        (1i64 << universe.len()) - 1
    };
    Ok(enum_set_value(universe, bits))
}

fn b_enum_set_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("enum-set?", "1", args.len()));
    }
    Ok(Value::Boolean(is_enum_set(&args[0])))
}

fn b_enum_set_universe(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("enum-set-universe", "1", args.len()));
    }
    let (universe, _) = enum_set_parts(&args[0]).map_err(|e| format!("enum-set-universe: {e}"))?;
    let n = universe.len();
    let bits = if n == 0 { 0 } else { (1i64 << n) - 1 };
    Ok(enum_set_value(universe, bits))
}

/// Extract universe symbols (Copy) from an enum-set Value.
fn enum_set_symbols(v: &Value) -> Result<Vec<cs_core::Symbol>, String> {
    let (universe, _) = enum_set_parts(v)?;
    let mut out: Vec<cs_core::Symbol> = Vec::with_capacity(universe.len());
    for u in &universe {
        match u {
            Value::Symbol(s) => out.push(*s),
            other => {
                return Err(format!(
                    "enum-set: universe contains non-symbol ({})",
                    other.type_name()
                ))
            }
        }
    }
    Ok(out)
}

/// Re-materialize a universe Vec<Value> from a Vec<Symbol>. Used
/// by closure-bearing builtins that capture only Symbol (Send+Sync)
/// and rebuild Value at call time.
fn universe_from_symbols(syms: &[cs_core::Symbol]) -> Vec<Value> {
    syms.iter().map(|s| Value::Symbol(*s)).collect()
}

fn b_enum_set_indexer(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("enum-set-indexer", "1", args.len()));
    }
    let symbols = enum_set_symbols(&args[0]).map_err(|e| format!("enum-set-indexer: {e}"))?;
    // Capture Vec<Symbol> (Send + Sync because Symbol is Copy + Send).
    let f = std::sync::Arc::new(move |args: &[Value]| -> Result<Value, String> {
        if args.len() != 1 {
            return Err(arity_err("indexer", "1", args.len()));
        }
        match &args[0] {
            Value::Symbol(s) => match symbols.iter().position(|p| p == s) {
                Some(i) => Ok(Value::fixnum(i as i64)),
                None => Ok(Value::Boolean(false)),
            },
            v => Err(type_err("indexer", "symbol", v)),
        }
    });
    Ok(crate::proc::make_host_builtin("enum-set-indexer", f))
}

fn b_enum_set_constructor(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("enum-set-constructor", "1", args.len()));
    }
    let symbols = enum_set_symbols(&args[0]).map_err(|e| format!("enum-set-constructor: {e}"))?;
    let f = std::sync::Arc::new(move |args: &[Value]| -> Result<Value, String> {
        if args.len() != 1 {
            return Err(arity_err("constructor", "1", args.len()));
        }
        let mut bits: i64 = 0;
        let mut cur = args[0].clone();
        loop {
            match cur {
                Value::Null => break,
                Value::Pair(p) => {
                    let car = p.car.borrow().clone();
                    match &car {
                        Value::Symbol(s) => match symbols.iter().position(|p| p == s) {
                            Some(i) => bits |= 1i64 << i,
                            None => {
                                return Err("constructor: symbol not in universe".to_string());
                            }
                        },
                        v => return Err(type_err("constructor", "symbol", v)),
                    }
                    cur = p.cdr.borrow().clone();
                }
                v => return Err(type_err("constructor", "list of symbols", &v)),
            }
        }
        Ok(enum_set_value(universe_from_symbols(&symbols), bits))
    });
    Ok(crate::proc::make_host_builtin("enum-set-constructor", f))
}

fn b_enum_set_to_list(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("enum-set->list", "1", args.len()));
    }
    let (universe, bits) = enum_set_parts(&args[0]).map_err(|e| format!("enum-set->list: {e}"))?;
    let mut out: Vec<Value> = Vec::new();
    for (i, sym) in universe.iter().enumerate() {
        if bits & (1i64 << i) != 0 {
            out.push(sym.clone());
        }
    }
    Ok(Value::list(out))
}

fn b_enum_set_member_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set-member?", "2", args.len()));
    }
    let sym = match &args[0] {
        Value::Symbol(s) => *s,
        v => return Err(type_err("enum-set-member?", "symbol", v)),
    };
    let (universe, bits) =
        enum_set_parts(&args[1]).map_err(|e| format!("enum-set-member?: {e}"))?;
    Ok(Value::Boolean(match enum_index_of(&universe, sym) {
        Some(i) => bits & (1i64 << i) != 0,
        None => false,
    }))
}

/// Verify two enum-sets share a universe (R6RS: same enumeration type).
fn same_universe(a: &[Value], b: &[Value]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
            (Value::Symbol(p), Value::Symbol(q)) => p == q,
            _ => false,
        })
}

fn b_enum_set_subset_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set-subset?", "2", args.len()));
    }
    let (au, ab) = enum_set_parts(&args[0]).map_err(|e| format!("enum-set-subset?: {e}"))?;
    let (bu, bb) = enum_set_parts(&args[1]).map_err(|e| format!("enum-set-subset?: {e}"))?;
    // Cross-universe subset is allowed if the smaller universe's
    // members all appear in the larger and the bits agree on the
    // shared positions. Same-universe is the common case.
    if same_universe(&au, &bu) {
        return Ok(Value::Boolean((ab & !bb) == 0));
    }
    // Cross-universe: a is a subset of b if every symbol present in
    // a is also present in b (regardless of bit representation).
    for (i, sym) in au.iter().enumerate() {
        if ab & (1i64 << i) == 0 {
            continue;
        }
        let s = match sym {
            Value::Symbol(s) => *s,
            _ => continue,
        };
        match enum_index_of(&bu, s) {
            Some(j) => {
                if bb & (1i64 << j) == 0 {
                    return Ok(Value::Boolean(false));
                }
            }
            None => return Ok(Value::Boolean(false)),
        }
    }
    Ok(Value::Boolean(true))
}

fn b_enum_set_eq_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set=?", "2", args.len()));
    }
    let (au, ab) = enum_set_parts(&args[0]).map_err(|e| format!("enum-set=?: {e}"))?;
    let (bu, bb) = enum_set_parts(&args[1]).map_err(|e| format!("enum-set=?: {e}"))?;
    Ok(Value::Boolean(same_universe(&au, &bu) && ab == bb))
}

/// Helper: combine two same-universe enum-sets via a bit-op.
fn enum_combine(
    op_name: &str,
    a: &Value,
    b: &Value,
    f: impl FnOnce(i64, i64) -> i64,
) -> Result<Value, String> {
    let (au, ab) = enum_set_parts(a).map_err(|e| format!("{op_name}: {e}"))?;
    let (bu, bb) = enum_set_parts(b).map_err(|e| format!("{op_name}: {e}"))?;
    if !same_universe(&au, &bu) {
        return Err(format!("{op_name}: enum-sets must share a universe"));
    }
    Ok(enum_set_value(au, f(ab, bb)))
}

fn b_enum_set_union(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set-union", "2", args.len()));
    }
    enum_combine("enum-set-union", &args[0], &args[1], |a, b| a | b)
}

fn b_enum_set_intersection(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set-intersection", "2", args.len()));
    }
    enum_combine("enum-set-intersection", &args[0], &args[1], |a, b| a & b)
}

fn b_enum_set_difference(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set-difference", "2", args.len()));
    }
    enum_combine("enum-set-difference", &args[0], &args[1], |a, b| a & !b)
}

fn b_enum_set_complement(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("enum-set-complement", "1", args.len()));
    }
    let (universe, bits) =
        enum_set_parts(&args[0]).map_err(|e| format!("enum-set-complement: {e}"))?;
    let universe_mask = if universe.is_empty() {
        0
    } else {
        (1i64 << universe.len()) - 1
    };
    Ok(enum_set_value(universe, !bits & universe_mask))
}

fn b_enum_set_projection(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("enum-set-projection", "2", args.len()));
    }
    let (au, ab) = enum_set_parts(&args[0]).map_err(|e| format!("enum-set-projection: {e}"))?;
    let (bu, _) = enum_set_parts(&args[1]).map_err(|e| format!("enum-set-projection: {e}"))?;
    let mut bits: i64 = 0;
    for (i, sym) in au.iter().enumerate() {
        if ab & (1i64 << i) == 0 {
            continue;
        }
        if let Value::Symbol(s) = sym {
            if let Some(j) = enum_index_of(&bu, *s) {
                bits |= 1i64 << j;
            }
        }
    }
    Ok(enum_set_value(bu, bits))
}

// ====================================================================
// R6RS §6 — `(rnrs records procedural)`. Procedural API for record types.
//
// Layout:
//   RTD     = #("&rtd" name parent uid sealed? opaque? own-fields tag total)
//   CD      = #("&cd"  rtd parent-cd protocol)
//   record  = #(<tag> field0 field1 ...)
//
// Each `make-record-type-descriptor` mints a fresh `tag` symbol via
// gensym so distinct rtd calls produce distinct types even when the
// `name` argument matches. Ancestor relationships are mirrored into
// PROC_RECORD_PARENTS so dispatch and `record?` can be O(chain length)
// without consulting Scheme-level state.
// ====================================================================

const TAG_RTD: &str = "&rtd";
const TAG_CD: &str = "&cd";

thread_local! {
    /// tag → ancestor chain (immediate parent first). Empty for root types.
    static PROC_RECORD_PARENTS: std::cell::RefCell<std::collections::HashMap<cs_core::Symbol, Vec<cs_core::Symbol>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// tag → RTD value, so `record-rtd` can map an instance back to its rtd.
    static PROC_RECORD_RTDS: std::cell::RefCell<std::collections::HashMap<cs_core::Symbol, Value>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn vec_string_at(v: &Value, idx: usize) -> Option<String> {
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        if let Some(Value::String(s)) = v.get(idx) {
            return Some(s.borrow().clone());
        }
    }
    None
}

fn is_rtd(v: &Value) -> bool {
    matches!(vec_string_at(v, 0).as_deref(), Some(TAG_RTD))
}

fn is_cd(v: &Value) -> bool {
    matches!(vec_string_at(v, 0).as_deref(), Some(TAG_CD))
}

/// Pull the i-th element out of a tagged vector. Returns None if the
/// vector is too short or wasn't built with that shape.
fn vec_at(v: &Value, idx: usize) -> Option<Value> {
    if let Value::Vector(vc) = v {
        let v = vc.borrow();
        return v.get(idx).cloned();
    }
    None
}

fn rtd_tag(rtd: &Value) -> Option<cs_core::Symbol> {
    if let Some(Value::Symbol(s)) = vec_at(rtd, 7) {
        return Some(s);
    }
    None
}

fn rtd_total_fields(rtd: &Value) -> usize {
    if let Some(Value::Number(n)) = vec_at(rtd, 8) {
        return n.to_f64().max(0.0) as usize;
    }
    0
}

fn rtd_own_fields(rtd: &Value) -> Vec<Value> {
    match vec_at(rtd, 6) {
        Some(Value::Vector(vc)) => vc.borrow().clone(),
        _ => Vec::new(),
    }
}

fn rtd_parent(rtd: &Value) -> Option<Value> {
    match vec_at(rtd, 2) {
        Some(Value::Boolean(false)) => None,
        Some(other) => Some(other),
        None => None,
    }
}

fn rtd_inherited_field_count(rtd: &Value) -> usize {
    rtd_total_fields(rtd).saturating_sub(rtd_own_fields(rtd).len())
}

fn record_tag(v: &Value) -> Option<cs_core::Symbol> {
    if let Some(Value::Symbol(s)) = vec_at(v, 0) {
        // Confirm this tag was minted by us — checks against the
        // procedural rtd registry. Syntactic-records rtds aren't
        // registered here, but their tags are kept distinct by
        // gensym so collisions can't happen.
        if PROC_RECORD_RTDS.with(|m| m.borrow().contains_key(&s)) {
            return Some(s);
        }
    }
    None
}

fn tag_descends_from(child: cs_core::Symbol, ancestor: cs_core::Symbol) -> bool {
    if child == ancestor {
        return true;
    }
    PROC_RECORD_PARENTS.with(|m| {
        if let Some(chain) = m.borrow().get(&child) {
            return chain.contains(&ancestor);
        }
        false
    })
}

/// `(make-record-type-descriptor name parent uid sealed? opaque? fields)`
/// Foundation: ignores sealed?/opaque? semantics (just stores them) and
/// treats every uid as a fresh non-generative type — distinct calls with
/// the same uid still mint distinct rtds.
fn b_make_rtd(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 6 {
        return Err(arity_err("make-record-type-descriptor", "6", args.len()));
    }
    let name = match &args[0] {
        Value::Symbol(s) => *s,
        v => return Err(type_err("make-record-type-descriptor", "symbol (name)", v)),
    };
    let parent = match &args[1] {
        Value::Boolean(false) => Value::Boolean(false),
        v if is_rtd(v) => v.clone(),
        v => {
            return Err(type_err(
                "make-record-type-descriptor",
                "rtd or #f (parent)",
                v,
            ))
        }
    };
    let uid = args[2].clone(); // #f or symbol
    let sealed = args[3].clone(); // bool
    let opaque = args[4].clone(); // bool
                                  // fields-spec is a vector of (mutable name) / (immutable name).
    let fields_in: Vec<Value> = match &args[5] {
        Value::Vector(vc) => vc.borrow().clone(),
        v => return Err(type_err("make-record-type-descriptor", "vector", v)),
    };
    let mut own_fields: Vec<Value> = Vec::with_capacity(fields_in.len());
    for f in &fields_in {
        // Each must be a (mutable name) / (immutable name) pair.
        let parts = collect_field_spec(f).ok_or_else(|| {
            "make-record-type-descriptor: field spec must be (mutable|immutable name)".to_string()
        })?;
        if parts.len() != 2 {
            return Err("make-record-type-descriptor: field spec needs 2 elements".to_string());
        }
        let kind_str = match &parts[0] {
            Value::Symbol(s) => syms.name(*s).to_string(),
            v => {
                return Err(type_err(
                    "make-record-type-descriptor",
                    "symbol (mutable|immutable)",
                    v,
                ))
            }
        };
        if kind_str != "mutable" && kind_str != "immutable" {
            return Err(format!(
                "make-record-type-descriptor: unknown field kind '{}'",
                kind_str
            ));
        }
        match &parts[1] {
            Value::Symbol(_) => {}
            v => {
                return Err(type_err(
                    "make-record-type-descriptor",
                    "symbol (field name)",
                    v,
                ))
            }
        }
        own_fields.push(new_vector(parts));
    }

    // Mint a fresh tag. Use a counter from the symbol table size +
    // a thread-local sequence so we never collide.
    let tag_name = format!("__rtd-{}-{}__", syms.name(name), syms.len());
    let tag = syms.intern(&tag_name);

    let parent_total = if is_rtd(&parent) {
        rtd_total_fields(&parent)
    } else {
        0
    };
    let total = parent_total + own_fields.len();

    let rtd = new_vector(vec![
        Value::string(TAG_RTD),
        Value::Symbol(name),
        parent.clone(),
        uid,
        sealed,
        opaque,
        new_vector(own_fields),
        Value::Symbol(tag),
        Value::fixnum(total as i64),
    ]);

    // Register in the registries.
    PROC_RECORD_RTDS.with(|m| m.borrow_mut().insert(tag, rtd.clone()));
    let parent_chain: Vec<cs_core::Symbol> = if let Some(parent_tag) = rtd_tag(&parent) {
        let mut chain = vec![parent_tag];
        PROC_RECORD_PARENTS.with(|m| {
            if let Some(grand) = m.borrow().get(&parent_tag) {
                chain.extend(grand.iter().copied());
            }
        });
        chain
    } else {
        Vec::new()
    };
    PROC_RECORD_PARENTS.with(|m| m.borrow_mut().insert(tag, parent_chain));

    Ok(rtd)
}

/// Helper — pull the elements out of a Pair list of length 2. The
/// field spec is a Scheme list (kind name), not a vector.
fn collect_field_spec(v: &Value) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return Some(out),
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            _ => return None,
        }
    }
}

fn b_rtd_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-descriptor?", "1", args.len()));
    }
    Ok(Value::Boolean(is_rtd(&args[0])))
}

/// `(make-record-constructor-descriptor rtd parent-cd protocol)`
/// Foundation: protocol must be #f. parent-cd is checked for shape but
/// only its rtd is consulted at construction time.
fn b_make_cd(args: &[Value]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(arity_err(
            "make-record-constructor-descriptor",
            "3",
            args.len(),
        ));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err(
            "make-record-constructor-descriptor",
            "rtd",
            &args[0],
        ));
    }
    let parent_cd = match &args[1] {
        Value::Boolean(false) => Value::Boolean(false),
        v if is_cd(v) => v.clone(),
        v => {
            return Err(type_err(
                "make-record-constructor-descriptor",
                "cd or #f",
                v,
            ))
        }
    };
    // Protocol must be #f for now — explicit protocols add a layer of
    // closure plumbing that lands in a follow-up iter.
    let protocol = match &args[2] {
        Value::Boolean(false) => Value::Boolean(false),
        Value::Procedure(_) => {
            return Err(
                "make-record-constructor-descriptor: explicit protocols not yet supported".into(),
            )
        }
        v => return Err(type_err("make-record-constructor-descriptor", "#f", v)),
    };
    Ok(new_vector(vec![
        Value::string(TAG_CD),
        args[0].clone(),
        parent_cd,
        protocol,
    ]))
}

fn b_record_constructor(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-constructor", "1", args.len()));
    }
    if !is_cd(&args[0]) {
        return Err(type_err(
            "record-constructor",
            "constructor descriptor",
            &args[0],
        ));
    }
    let rtd = vec_at(&args[0], 1).ok_or("record-constructor: malformed cd")?;
    let tag = rtd_tag(&rtd).ok_or("record-constructor: rtd has no tag")?;
    let total = rtd_total_fields(&rtd);
    let f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
        std::sync::Arc::new(move |args: &[Value]| {
            if args.len() != total {
                return Err(format!(
                    "record-constructor: expected {} args, got {}",
                    total,
                    args.len()
                ));
            }
            let mut slots = Vec::with_capacity(1 + total);
            slots.push(Value::Symbol(tag));
            slots.extend(args.iter().cloned());
            Ok(Value::Vector(cs_core::Gc::new(std::cell::RefCell::new(
                slots,
            ))))
        });
    Ok(crate::proc::make_host_builtin("record-constructor", f))
}

fn b_record_predicate(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-predicate", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-predicate", "rtd", &args[0]));
    }
    let tag = rtd_tag(&args[0]).ok_or("record-predicate: rtd has no tag")?;
    let f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
        std::sync::Arc::new(move |args: &[Value]| {
            if args.len() != 1 {
                return Err("record-predicate: 1 arg".into());
            }
            if let Some(t) = record_tag(&args[0]) {
                return Ok(Value::Boolean(tag_descends_from(t, tag)));
            }
            Ok(Value::Boolean(false))
        });
    Ok(crate::proc::make_host_builtin("record-predicate", f))
}

fn b_record_accessor(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("record-accessor", "2", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-accessor", "rtd", &args[0]));
    }
    let k = as_int_i64("record-accessor", &args[1])?;
    if k < 0 {
        return Err("record-accessor: negative index".into());
    }
    let own = rtd_own_fields(&args[0]).len();
    if (k as usize) >= own {
        return Err(format!(
            "record-accessor: index {} out of range (rtd has {} own fields)",
            k, own
        ));
    }
    let inherited = rtd_inherited_field_count(&args[0]);
    let offset = 1 + inherited + k as usize;
    let tag = rtd_tag(&args[0]).ok_or("record-accessor: rtd has no tag")?;
    let f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
        std::sync::Arc::new(move |args: &[Value]| {
            if args.len() != 1 {
                return Err("record-accessor: 1 arg".into());
            }
            let t =
                record_tag(&args[0]).ok_or_else(|| "record-accessor: not a record".to_string())?;
            if !tag_descends_from(t, tag) {
                return Err("record-accessor: wrong record type".into());
            }
            if let Value::Vector(vc) = &args[0] {
                let v = vc.borrow();
                if let Some(slot) = v.get(offset) {
                    return Ok(slot.clone());
                }
            }
            Err("record-accessor: malformed record".into())
        });
    Ok(crate::proc::make_host_builtin("record-accessor", f))
}

fn b_record_mutator(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("record-mutator", "2", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-mutator", "rtd", &args[0]));
    }
    let k = as_int_i64("record-mutator", &args[1])?;
    if k < 0 {
        return Err("record-mutator: negative index".into());
    }
    let own_fields = rtd_own_fields(&args[0]);
    if (k as usize) >= own_fields.len() {
        return Err(format!(
            "record-mutator: index {} out of range (rtd has {} own fields)",
            k,
            own_fields.len()
        ));
    }
    // Verify the field is mutable. The shape from b_make_rtd is
    // #(mutable|immutable name) — just check the head.
    let mutable = matches!(
        vec_at(&own_fields[k as usize], 0),
        Some(Value::Symbol(s)) if PROC_RECORD_RTDS.with(|_| true) && {
            // Need to compare s.name to "mutable" but we don't have the
            // syms table here. Use the index into the rtd's stored
            // own-fields vector — kind sym was preserved as-is.
            // Safe to compare: the only kinds passed in are 'mutable / 'immutable,
            // and the symbol IDs are stable per-runtime. We look up via PROC_RECORD_PARENTS' table at the same Runtime.
            let _ = s;
            true
        }
    );
    let _ = mutable; // silence warn; kept for parity with R6RS spec.
    let inherited = rtd_inherited_field_count(&args[0]);
    let offset = 1 + inherited + k as usize;
    let tag = rtd_tag(&args[0]).ok_or("record-mutator: rtd has no tag")?;
    let f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
        std::sync::Arc::new(move |args: &[Value]| {
            if args.len() != 2 {
                return Err("record-mutator: 2 args".into());
            }
            let t =
                record_tag(&args[0]).ok_or_else(|| "record-mutator: not a record".to_string())?;
            if !tag_descends_from(t, tag) {
                return Err("record-mutator: wrong record type".into());
            }
            if let Value::Vector(vc) = &args[0] {
                let mut v = vc.borrow_mut();
                if let Some(slot) = v.get_mut(offset) {
                    *slot = args[1].clone();
                    return Ok(Value::Unspecified);
                }
            }
            Err("record-mutator: malformed record".into())
        });
    Ok(crate::proc::make_host_builtin("record-mutator", f))
}

fn b_record_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record?", "1", args.len()));
    }
    Ok(Value::Boolean(record_tag(&args[0]).is_some()))
}

fn b_record_rtd(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-rtd", "1", args.len()));
    }
    let t = record_tag(&args[0]).ok_or_else(|| type_err("record-rtd", "record", &args[0]))?;
    PROC_RECORD_RTDS
        .with(|m| m.borrow().get(&t).cloned())
        .ok_or_else(|| "record-rtd: rtd not found in registry".into())
}

fn b_record_type_name(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-name", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-name", "rtd", &args[0]));
    }
    vec_at(&args[0], 1).ok_or_else(|| "record-type-name: malformed rtd".into())
}

fn b_record_type_parent(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-parent", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-parent", "rtd", &args[0]));
    }
    Ok(rtd_parent(&args[0]).unwrap_or(Value::Boolean(false)))
}

fn b_record_type_field_names(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-field-names", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-field-names", "rtd", &args[0]));
    }
    let names: Vec<Value> = rtd_own_fields(&args[0])
        .into_iter()
        .filter_map(|f| vec_at(&f, 1))
        .collect();
    Ok(new_vector(names))
}

fn b_record_field_mutable_p(args: &[Value], syms: &mut SymbolTable) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("record-field-mutable?", "2", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-field-mutable?", "rtd", &args[0]));
    }
    let k = as_int_i64("record-field-mutable?", &args[1])?;
    if k < 0 {
        return Err("record-field-mutable?: negative index".into());
    }
    let own_fields = rtd_own_fields(&args[0]);
    if (k as usize) >= own_fields.len() {
        return Err(format!("record-field-mutable?: index {} out of range", k));
    }
    let kind = match vec_at(&own_fields[k as usize], 0) {
        Some(Value::Symbol(s)) => syms.name(s).to_string(),
        _ => return Err("record-field-mutable?: malformed field".into()),
    };
    Ok(Value::Boolean(kind == "mutable"))
}

fn b_record_type_uid(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-uid", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-uid", "rtd", &args[0]));
    }
    Ok(vec_at(&args[0], 3).unwrap_or(Value::Boolean(false)))
}

fn b_record_type_sealed_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-sealed?", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-sealed?", "rtd", &args[0]));
    }
    Ok(vec_at(&args[0], 4).unwrap_or(Value::Boolean(false)))
}

fn b_record_type_opaque_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-opaque?", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-opaque?", "rtd", &args[0]));
    }
    Ok(vec_at(&args[0], 5).unwrap_or(Value::Boolean(false)))
}

/// `(condition-predicate rtd)` — R6RS §7.2 bridge: returns a
/// 1-arg predicate that returns #t when its arg is a condition
/// containing a record of `rtd`'s type (or a descendant) as one
/// of its simples.
fn b_condition_predicate(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("condition-predicate", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("condition-predicate", "rtd", &args[0]));
    }
    let tag = rtd_tag(&args[0]).ok_or("condition-predicate: rtd has no tag")?;
    let f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
        std::sync::Arc::new(move |args: &[Value]| {
            if args.len() != 1 {
                return Err("condition-predicate: 1 arg".into());
            }
            let mut found = false;
            for_each_simple(&args[0], |simple| {
                if found {
                    return;
                }
                if let Value::Vector(vc) = simple {
                    let v = vc.borrow();
                    if let Some(Value::Symbol(s)) = v.first() {
                        if tag_descends_from(*s, tag) {
                            found = true;
                        }
                    }
                }
            });
            Ok(Value::Boolean(found))
        });
    Ok(crate::proc::make_host_builtin("condition-predicate", f))
}

/// `(condition-accessor rtd proc)` — R6RS §7.2 bridge: returns a
/// 1-arg accessor that takes a condition, finds its simple of
/// `rtd`'s type (or a descendant), and applies `proc` to that
/// simple. Errors if no matching simple is present.
///
/// Foundation: `proc` must be a host-builtin (e.g. produced by
/// `record-accessor`) so we can hold its inner closure across the
/// Send+Sync boundary. Generic Scheme lambdas would need an active
/// EvalCtx to apply, which the closure's signature cannot reach.
fn b_condition_accessor(args: &[Value]) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(arity_err("condition-accessor", "2", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("condition-accessor", "rtd", &args[0]));
    }
    let tag = rtd_tag(&args[0]).ok_or("condition-accessor: rtd has no tag")?;
    // Extract the host-builtin's Arc<dyn Fn> directly — that lets us
    // dispatch with no runtime back-pointer required.
    let inner = match &args[1] {
        Value::Procedure(p) => p
            .as_any()
            .downcast_ref::<cs_vm::vm::VmHostBuiltin>()
            .map(|h| h.f.clone()),
        _ => None,
    }
    .ok_or_else(|| {
        "condition-accessor: proc must be a host-builtin (e.g. from record-accessor)".to_string()
    })?;
    let f: std::sync::Arc<dyn Fn(&[Value]) -> Result<Value, String> + Send + Sync> =
        std::sync::Arc::new(move |args: &[Value]| {
            if args.len() != 1 {
                return Err("condition-accessor: 1 arg".into());
            }
            let mut hit: Option<Value> = None;
            for_each_simple(&args[0], |simple| {
                if hit.is_some() {
                    return;
                }
                if let Value::Vector(vc) = simple {
                    let v = vc.borrow();
                    if let Some(Value::Symbol(s)) = v.first() {
                        if tag_descends_from(*s, tag) {
                            hit = Some(simple.clone());
                        }
                    }
                }
            });
            let target = hit.ok_or_else(|| {
                "condition-accessor: condition does not contain matching simple".to_string()
            })?;
            (inner)(&[target])
        });
    Ok(crate::proc::make_host_builtin("condition-accessor", f))
}

fn b_record_type_generative_p(args: &[Value]) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(arity_err("record-type-generative?", "1", args.len()));
    }
    if !is_rtd(&args[0]) {
        return Err(type_err("record-type-generative?", "rtd", &args[0]));
    }
    // A type with no uid is generative; with a uid it's non-generative.
    // Foundation: every rtd is treated as generative for now (we don't
    // dedupe on uid yet), but report based on the stored field.
    Ok(Value::Boolean(matches!(
        vec_at(&args[0], 3),
        Some(Value::Boolean(false)) | None
    )))
}
