//! Built-in functions for the MDS template language.
//!
//! Built-ins are resolved after user-defined functions, so user functions
//! can shadow built-ins with the same name.
//!
//! # Available functions
//!
//! **String:** `upper`, `lower`, `trim`, `replace`, `starts_with`, `ends_with`,
//! `contains`, `string`, `slice` (string variant)
//!
//! **Array:** `split`, `join`, `length`, `first`, `last`, `reverse`, `sort`,
//! `unique`, `slice` (array variant)
//!
//! **Conversion:** `string`, `number`

use std::collections::HashSet;

use crate::error::MdsError;
use crate::limits::MAX_OUTPUT_SIZE;
use crate::value::Value;

/// Metadata and dispatch handler for a built-in function.
///
/// Keeping metadata and handler in one struct means the `BUILTINS` registry
/// is the single source of truth — adding a new built-in requires exactly one
/// entry here. There is no separate match arm to keep in sync.
pub(crate) struct BuiltinMeta {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub handler: fn(&[Value]) -> Result<Value, MdsError>,
}

/// All built-in function definitions.
///
/// This is the single source of truth for every built-in: name, arity, and
/// dispatch handler live in one place. `get_builtin` and `call_builtin` both
/// read this array, so a new built-in needs exactly one entry here.
///
/// Linear scan over 18 elements is intentional — the array is small and
/// cache-resident. A hash map would add allocation and indirection for no
/// measurable gain at this cardinality.
static BUILTINS: &[BuiltinMeta] = &[
    // String operations
    BuiltinMeta {
        name: "upper",
        min_args: 1,
        max_args: 1,
        handler: builtin_upper,
    },
    BuiltinMeta {
        name: "lower",
        min_args: 1,
        max_args: 1,
        handler: builtin_lower,
    },
    BuiltinMeta {
        name: "trim",
        min_args: 1,
        max_args: 1,
        handler: builtin_trim,
    },
    BuiltinMeta {
        name: "replace",
        min_args: 3,
        max_args: 3,
        handler: builtin_replace,
    },
    BuiltinMeta {
        name: "split",
        min_args: 2,
        max_args: 2,
        handler: builtin_split,
    },
    BuiltinMeta {
        name: "starts_with",
        min_args: 2,
        max_args: 2,
        handler: builtin_starts_with,
    },
    BuiltinMeta {
        name: "ends_with",
        min_args: 2,
        max_args: 2,
        handler: builtin_ends_with,
    },
    BuiltinMeta {
        name: "slice",
        min_args: 2,
        max_args: 3,
        handler: builtin_slice,
    },
    BuiltinMeta {
        name: "contains",
        min_args: 2,
        max_args: 2,
        handler: builtin_contains,
    },
    // Array operations
    BuiltinMeta {
        name: "join",
        min_args: 2,
        max_args: 2,
        handler: builtin_join,
    },
    BuiltinMeta {
        name: "length",
        min_args: 1,
        max_args: 1,
        handler: builtin_length,
    },
    BuiltinMeta {
        name: "first",
        min_args: 1,
        max_args: 1,
        handler: builtin_first,
    },
    BuiltinMeta {
        name: "last",
        min_args: 1,
        max_args: 1,
        handler: builtin_last,
    },
    BuiltinMeta {
        name: "reverse",
        min_args: 1,
        max_args: 1,
        handler: builtin_reverse,
    },
    BuiltinMeta {
        name: "sort",
        min_args: 1,
        max_args: 1,
        handler: builtin_sort,
    },
    BuiltinMeta {
        name: "unique",
        min_args: 1,
        max_args: 1,
        handler: builtin_unique,
    },
    // Type conversion
    BuiltinMeta {
        name: "string",
        min_args: 1,
        max_args: 1,
        handler: builtin_string,
    },
    BuiltinMeta {
        name: "number",
        min_args: 1,
        max_args: 1,
        handler: builtin_number,
    },
];

/// Look up a built-in function by name.
///
/// Returns `Some(&BuiltinMeta)` if the function exists, `None` otherwise.
pub(crate) fn get_builtin(name: &str) -> Option<&'static BuiltinMeta> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Call a built-in function with the given resolved arguments.
///
/// Dispatches via the `BUILTINS` registry so that metadata and handler are
/// always in sync. No separate match arm is needed.
///
/// # Errors
///
/// Returns `MdsError::BuiltinError` when an argument has the wrong type, and
/// `MdsError::arity` when the wrong number of arguments is passed.
#[cfg(test)]
pub(crate) fn call_builtin(name: &str, args: &[Value]) -> Result<Value, MdsError> {
    match get_builtin(name) {
        Some(def) => (def.handler)(args),
        None => Err(MdsError::undefined_fn(name)),
    }
}

// ── Helper ────────────────────────────────────────────────────────────────────

/// Return a type-error for a built-in that received the wrong argument type.
fn type_err(fn_name: &str, arg_pos: &str, expected: &str, got: &str) -> MdsError {
    MdsError::builtin_error(format!(
        "{fn_name}() requires a {expected} argument{}, got {got}",
        if arg_pos.is_empty() {
            String::new()
        } else {
            format!(" for {arg_pos}")
        }
    ))
}

/// Require `args[0]` to be a string, returning `MdsError::BuiltinError` otherwise.
fn require_string<'a>(args: &'a [Value], fn_name: &str) -> Result<&'a str, MdsError> {
    require_string_at(args, 0, fn_name, "")
}

/// Require the argument at `idx` to be a string.
fn require_string_at<'a>(
    args: &'a [Value],
    idx: usize,
    fn_name: &str,
    pos: &str,
) -> Result<&'a str, MdsError> {
    match &args[idx] {
        Value::String(s) => Ok(s.as_str()),
        other => Err(type_err(fn_name, pos, "string", other.type_name())),
    }
}

// ── String operations ─────────────────────────────────────────────────────────

fn builtin_upper(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string(args, "upper")?;
    Ok(Value::String(s.to_uppercase()))
}

fn builtin_lower(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string(args, "lower")?;
    Ok(Value::String(s.to_lowercase()))
}

fn builtin_trim(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string(args, "trim")?;
    Ok(Value::String(s.trim().to_string()))
}

fn builtin_replace(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string_at(args, 0, "replace", "first")?;
    let from = require_string_at(args, 1, "replace", "second")?;
    let to = require_string_at(args, 2, "replace", "third")?;
    if from.is_empty() {
        return Err(MdsError::builtin_error(
            "replace() search string must not be empty",
        ));
    }
    let result = s.replace(from, to);
    // Guard against amplification: a single-char search replaced by a long
    // string on large input can produce a result far exceeding MAX_OUTPUT_SIZE
    // before the evaluator's per-node output check fires.
    if result.len() > MAX_OUTPUT_SIZE {
        return Err(MdsError::builtin_error(format!(
            "replace() output exceeds maximum size of {} bytes",
            MAX_OUTPUT_SIZE
        )));
    }
    Ok(Value::String(result))
}

fn builtin_split(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string_at(args, 0, "split", "first")?;
    let sep = require_string_at(args, 1, "split", "second")?;
    if sep.is_empty() {
        return Err(MdsError::builtin_error(
            "split() separator must not be empty",
        ));
    }
    let parts: Vec<Value> = s.split(sep).map(|p| Value::String(p.to_string())).collect();
    Ok(Value::Array(parts))
}

fn builtin_starts_with(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string_at(args, 0, "starts_with", "first")?;
    let prefix = require_string_at(args, 1, "starts_with", "second")?;
    Ok(Value::Boolean(s.starts_with(prefix)))
}

fn builtin_ends_with(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string_at(args, 0, "ends_with", "first")?;
    let suffix = require_string_at(args, 1, "ends_with", "second")?;
    Ok(Value::Boolean(s.ends_with(suffix)))
}

fn builtin_contains(args: &[Value]) -> Result<Value, MdsError> {
    // contains works on both strings and arrays
    match &args[0] {
        Value::String(s) => {
            let needle = require_string_at(args, 1, "contains", "second")?;
            Ok(Value::Boolean(s.contains(needle)))
        }
        Value::Array(arr) => Ok(Value::Boolean(arr.contains(&args[1]))),
        other => Err(type_err(
            "contains",
            "first",
            "string or array",
            other.type_name(),
        )),
    }
}

fn builtin_slice(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        Value::String(s) => {
            // Character-based indexing: indices refer to Unicode scalar values,
            // not bytes. slice("café", 0, 4) == "café" (4 chars), not "caf".
            let char_count = s.chars().count();
            let start_idx = require_number_index(&args[1], "slice", "second")?;
            let start_idx = start_idx.min(char_count);
            if args.len() == 3 {
                let end_idx = require_number_index(&args[2], "slice", "third")?;
                let end_idx = end_idx.clamp(start_idx, char_count);
                let result: String = s
                    .chars()
                    .skip(start_idx)
                    .take(end_idx - start_idx)
                    .collect();
                Ok(Value::String(result))
            } else {
                let result: String = s.chars().skip(start_idx).collect();
                Ok(Value::String(result))
            }
        }
        Value::Array(arr) => {
            let start = require_number_index(&args[1], "slice", "second")?;
            let len = arr.len();
            let start = start.min(len);
            if args.len() == 3 {
                let end = require_number_index(&args[2], "slice", "third")?;
                let end = end.clamp(start, len);
                Ok(Value::Array(arr[start..end].to_vec()))
            } else {
                Ok(Value::Array(arr[start..].to_vec()))
            }
        }
        other => Err(type_err(
            "slice",
            "first",
            "string or array",
            other.type_name(),
        )),
    }
}

/// Parse a `Value::Number` as a `usize` for use as a slice index.
/// Clamps negative numbers to 0. Returns an error for non-number values,
/// non-finite values (NaN, infinity), or values exceeding `usize::MAX`.
fn require_number_index(val: &Value, fn_name: &str, pos: &str) -> Result<usize, MdsError> {
    match val {
        Value::Number(n) => {
            if !n.is_finite() {
                return Err(MdsError::builtin_error(format!(
                    "{fn_name}() {pos} argument must be a finite number, got {n}"
                )));
            }
            let clamped = n.max(0.0).floor();
            // Guard against overflow: usize::MAX as f64 may not round-trip exactly,
            // but any value larger than 2^53 is safely above any realistic string/array
            // length and can be clamped to usize::MAX without information loss.
            if clamped > usize::MAX as f64 {
                return Err(MdsError::builtin_error(format!(
                    "{fn_name}() {pos} argument is too large: {n}"
                )));
            }
            Ok(clamped as usize)
        }
        other => Err(type_err(fn_name, pos, "number", other.type_name())),
    }
}

// ── Array operations ──────────────────────────────────────────────────────────

fn builtin_join(args: &[Value]) -> Result<Value, MdsError> {
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Err(type_err("join", "first", "array", other.type_name())),
    };
    let sep = require_string_at(args, 1, "join", "second")?;
    // Single-pass fold: validate element types and build output string without
    // an intermediate Vec<String> allocation, halving transient memory use.
    let mut out = String::new();
    for (i, v) in arr.iter().enumerate() {
        match v {
            Value::String(s) => {
                if i > 0 {
                    out.push_str(sep);
                }
                out.push_str(s);
            }
            other => {
                return Err(MdsError::builtin_error(format!(
                    "join() requires an array of strings, but found {} in array",
                    other.type_name()
                )));
            }
        }
    }
    Ok(Value::String(out))
}

fn builtin_length(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        // Character count, not byte count — consistent with user expectations
        // for non-ASCII strings (e.g. length("café") == 4, not 5).
        Value::String(s) => Ok(Value::Number(s.chars().count() as f64)),
        Value::Array(a) => Ok(Value::Number(a.len() as f64)),
        other => Err(type_err("length", "", "string or array", other.type_name())),
    }
}

fn builtin_first(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        Value::Array(a) => Ok(a.first().cloned().unwrap_or(Value::Null)),
        other => Err(type_err("first", "", "array", other.type_name())),
    }
}

fn builtin_last(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        Value::Array(a) => Ok(a.last().cloned().unwrap_or(Value::Null)),
        other => Err(type_err("last", "", "array", other.type_name())),
    }
}

fn builtin_reverse(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        // String reversal operates on Unicode scalar values (Rust `char`), not
        // grapheme clusters. This means combining diacriticals and multi-codepoint
        // sequences such as flag emoji (e.g. "🇺🇸" = U+1F1FA U+1F1F8) will be
        // reversed incorrectly. This is a documented limitation — see spec §4.5.
        Value::String(s) => Ok(Value::String(s.chars().rev().collect())),
        Value::Array(a) => {
            let mut reversed = a.clone();
            reversed.reverse();
            Ok(Value::Array(reversed))
        }
        other => Err(type_err(
            "reverse",
            "",
            "string or array",
            other.type_name(),
        )),
    }
}

fn builtin_sort(args: &[Value]) -> Result<Value, MdsError> {
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Err(type_err("sort", "", "array", other.type_name())),
    };
    if arr.is_empty() {
        return Ok(Value::Array(vec![]));
    }
    // Dispatch on the first element's type; helpers validate and sort.
    match &arr[0] {
        Value::String(_) => sort_strings(arr),
        Value::Number(_) => sort_numbers(arr),
        other => Err(MdsError::builtin_error(format!(
            "sort() requires an array of strings or numbers, got array of {}",
            other.type_name()
        ))),
    }
}

/// Sort a non-empty homogeneous array of strings alphabetically.
///
/// Validates homogeneity BEFORE cloning so a type error on a large array pays
/// no allocation cost.
fn sort_strings(arr: &[Value]) -> Result<Value, MdsError> {
    for item in arr {
        if !matches!(item, Value::String(_)) {
            return Err(MdsError::builtin_error(format!(
                "sort() requires a homogeneous array; found {} mixed with string",
                item.type_name()
            )));
        }
    }
    let mut sorted = arr.to_vec();
    sorted.sort_by(|a, b| match (a, b) {
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => unreachable!(),
    });
    Ok(Value::Array(sorted))
}

/// Sort a non-empty homogeneous array of finite numbers ascending.
///
/// Validates homogeneity and finiteness together before cloning to avoid
/// paying allocation cost on large arrays with type or NaN errors.
fn sort_numbers(arr: &[Value]) -> Result<Value, MdsError> {
    for item in arr {
        match item {
            Value::Number(n) if !n.is_finite() => {
                return Err(MdsError::builtin_error(format!(
                    "sort() cannot sort non-finite number: {n}"
                )));
            }
            Value::Number(_) => {}
            _ => {
                return Err(MdsError::builtin_error(format!(
                    "sort() requires a homogeneous array; found {} mixed with number",
                    item.type_name()
                )));
            }
        }
    }
    let mut sorted = arr.to_vec();
    sorted.sort_by(|a, b| match (a, b) {
        (Value::Number(a), Value::Number(b)) => a.total_cmp(b),
        _ => unreachable!(),
    });
    Ok(Value::Array(sorted))
}

/// Produce a type-discriminated key for use in `builtin_unique`'s seen-set.
/// The prefix ensures that values of different types never collide even if
/// their `Display` representations are identical (e.g. `Null` and `String("")`
/// both display as `""`, but get keys `"null:"` vs `"s:"`).
///
/// **Complexity note**: For scalar values (String, Number, Boolean, Null) this
/// is O(1). For `Array` and `Object` variants the key is generated via
/// `Display`, which serializes the full nested structure — O(m) where m is the
/// element count. The total cost for an array of n nested values is therefore
/// O(n*m). This is bounded in practice by `MAX_FILE_SIZE` (10 MB), so no
/// additional guard is needed here.
fn unique_key(v: &Value) -> String {
    match v {
        Value::String(s) => format!("s:{s}"),
        Value::Number(n) => format!("n:{n}"),
        Value::Boolean(b) => format!("b:{b}"),
        Value::Null => "null:".to_string(),
        // Arrays and objects are stringified with their type prefix. Two
        // structurally identical nested values will collide only if they
        // produce the same Display output, which is the correct semantic for
        // equality-based deduplication of these types.
        Value::Array(_) => format!("a:{v}"),
        Value::Object(_) => format!("o:{v}"),
    }
}

fn builtin_unique(args: &[Value]) -> Result<Value, MdsError> {
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Err(type_err("unique", "", "array", other.type_name())),
    };
    // Order-preserving deduplication in O(n) time.
    // Value does not implement Hash, so we use a type-discriminated string key
    // derived from each element's display representation.
    let mut seen: HashSet<String> = HashSet::new();
    let mut result: Vec<Value> = Vec::with_capacity(arr.len());
    for item in arr {
        if seen.insert(unique_key(item)) {
            result.push(item.clone());
        }
    }
    Ok(Value::Array(result))
}

// ── Type conversion ───────────────────────────────────────────────────────────

fn builtin_string(args: &[Value]) -> Result<Value, MdsError> {
    Ok(Value::String(args[0].to_string()))
}

fn builtin_number(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        Value::Number(n) => Ok(Value::Number(*n)),
        Value::String(s) => {
            let n: f64 = s.trim().parse().map_err(|_| {
                MdsError::builtin_error(format!("number() cannot convert string '{s}' to a number"))
            })?;
            if !n.is_finite() {
                return Err(MdsError::builtin_error(format!(
                    "number() produced a non-finite value from '{s}'"
                )));
            }
            Ok(Value::Number(n))
        }
        Value::Boolean(b) => Ok(Value::Number(if *b { 1.0 } else { 0.0 })),
        Value::Null => Ok(Value::Number(0.0)),
        other => Err(MdsError::builtin_error(format!(
            "number() cannot convert {} to a number",
            other.type_name()
        ))),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper
    fn s(v: &str) -> Value {
        Value::String(v.to_string())
    }

    fn arr(items: &[&str]) -> Value {
        Value::Array(items.iter().map(|s| Value::String(s.to_string())).collect())
    }

    fn num_arr(items: &[f64]) -> Value {
        Value::Array(items.iter().map(|n| Value::Number(*n)).collect())
    }

    // ── String operations ─────────────────────────────────────────────────────

    #[test]
    fn upper_converts_to_uppercase() {
        let result = call_builtin("upper", &[s("hello")]).unwrap();
        assert_eq!(result, s("HELLO"));
    }

    #[test]
    fn upper_requires_string() {
        let err = call_builtin("upper", &[Value::Number(1.0)]).unwrap_err();
        assert!(
            err.to_string().contains("string"),
            "error should mention string type"
        );
    }

    #[test]
    fn lower_converts_to_lowercase() {
        let result = call_builtin("lower", &[s("WORLD")]).unwrap();
        assert_eq!(result, s("world"));
    }

    #[test]
    fn lower_requires_string() {
        let err = call_builtin("lower", &[Value::Boolean(true)]).unwrap_err();
        assert!(err.to_string().contains("string"));
    }

    #[test]
    fn trim_removes_whitespace() {
        let result = call_builtin("trim", &[s("  hello  ")]).unwrap();
        assert_eq!(result, s("hello"));
    }

    #[test]
    fn replace_substitutes_literal() {
        let result = call_builtin("replace", &[s("hello world"), s("world"), s("Rust")]).unwrap();
        assert_eq!(result, s("hello Rust"));
    }

    #[test]
    fn replace_no_match_returns_original() {
        let result = call_builtin("replace", &[s("hello"), s("xyz"), s("abc")]).unwrap();
        assert_eq!(result, s("hello"));
    }

    #[test]
    fn replace_empty_search_string_rejected() {
        let err = call_builtin("replace", &[s("hello"), s(""), s("x")]).unwrap_err();
        assert!(
            err.to_string().contains("search string must not be empty"),
            "expected empty-search guard, got: {err}"
        );
    }

    #[test]
    fn replace_output_size_guard_fires() {
        // "x" replaced by a 1 MB string, repeated >50 times, would exceed 50 MB.
        // We use a modest amplification that fits in memory but exceeds the limit.
        let big_replacement = "a".repeat(1024 * 1024); // 1 MB
                                                       // Input: 60 occurrences of "x" separated by spaces → output would be ~60 MB.
        let input: String = vec!["x"; 60].join(" ");
        let err = call_builtin("replace", &[s(&input), s("x"), s(&big_replacement)]).unwrap_err();
        assert!(
            err.to_string().contains("maximum size"),
            "expected output size guard, got: {err}"
        );
    }

    #[test]
    fn split_on_comma() {
        let result = call_builtin("split", &[s("a,b,c"), s(",")]).unwrap();
        assert_eq!(result, arr(&["a", "b", "c"]));
    }

    #[test]
    fn split_empty_separator_rejected() {
        let err = call_builtin("split", &[s("hello"), s("")]).unwrap_err();
        assert!(
            err.to_string().contains("separator must not be empty"),
            "expected empty-separator guard, got: {err}"
        );
    }

    #[test]
    fn starts_with_true() {
        let result = call_builtin("starts_with", &[s("hello"), s("he")]).unwrap();
        assert_eq!(result, Value::Boolean(true));
    }

    #[test]
    fn starts_with_false() {
        let result = call_builtin("starts_with", &[s("hello"), s("wo")]).unwrap();
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn ends_with_true() {
        let result = call_builtin("ends_with", &[s("hello"), s("lo")]).unwrap();
        assert_eq!(result, Value::Boolean(true));
    }

    #[test]
    fn ends_with_false() {
        let result = call_builtin("ends_with", &[s("hello"), s("he")]).unwrap();
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn contains_string_found() {
        let result = call_builtin("contains", &[s("hello world"), s("world")]).unwrap();
        assert_eq!(result, Value::Boolean(true));
    }

    #[test]
    fn contains_string_not_found() {
        let result = call_builtin("contains", &[s("hello"), s("xyz")]).unwrap();
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn contains_array_found() {
        let result = call_builtin("contains", &[arr(&["a", "b"]), s("b")]).unwrap();
        assert_eq!(result, Value::Boolean(true));
    }

    #[test]
    fn slice_string_with_end() {
        let result = call_builtin(
            "slice",
            &[s("hello"), Value::Number(1.0), Value::Number(3.0)],
        )
        .unwrap();
        assert_eq!(result, s("el"));
    }

    #[test]
    fn slice_string_without_end() {
        let result = call_builtin("slice", &[s("hello"), Value::Number(2.0)]).unwrap();
        assert_eq!(result, s("llo"));
    }

    #[test]
    fn slice_clamps_to_bounds() {
        // Start beyond end → empty
        let result = call_builtin("slice", &[s("hi"), Value::Number(100.0)]).unwrap();
        assert_eq!(result, s(""));
    }

    #[test]
    fn slice_infinity_index_rejected() {
        let err = call_builtin("slice", &[s("hello"), Value::Number(f64::INFINITY)]).unwrap_err();
        assert!(
            err.to_string().contains("finite"),
            "expected finite-number guard, got: {err}"
        );
    }

    #[test]
    fn slice_nan_index_rejected() {
        let err = call_builtin("slice", &[s("hello"), Value::Number(f64::NAN)]).unwrap_err();
        assert!(
            err.to_string().contains("finite"),
            "expected finite-number guard, got: {err}"
        );
    }

    #[test]
    fn slice_string_char_based_indexing() {
        // Character-based indexing: slice("café", 0, 4) returns all 4 chars.
        // "café" is 5 bytes but 4 Unicode scalar values.
        let result = call_builtin(
            "slice",
            &[s("café"), Value::Number(0.0), Value::Number(4.0)],
        )
        .unwrap();
        assert_eq!(
            result,
            s("café"),
            "slice should use char indices, not bytes"
        );
    }

    #[test]
    fn slice_string_emoji_char_indexing() {
        // "a😀b" has 3 chars: a(0), 😀(1), b(2).
        // slice("a😀b", 1, 2) should return the emoji character.
        let result = call_builtin(
            "slice",
            &[s("a😀b"), Value::Number(1.0), Value::Number(2.0)],
        )
        .unwrap();
        assert_eq!(result, s("😀"), "slice should address emoji as single char");
    }

    #[test]
    fn slice_array() {
        let result = call_builtin(
            "slice",
            &[
                arr(&["a", "b", "c"]),
                Value::Number(1.0),
                Value::Number(3.0),
            ],
        )
        .unwrap();
        assert_eq!(result, arr(&["b", "c"]));
    }

    // ── Array operations ──────────────────────────────────────────────────────

    #[test]
    fn join_array_with_separator() {
        let result = call_builtin("join", &[arr(&["a", "b", "c"]), s(", ")]).unwrap();
        assert_eq!(result, s("a, b, c"));
    }

    #[test]
    fn join_requires_array_of_strings() {
        let mixed = Value::Array(vec![s("a"), Value::Number(1.0)]);
        let err = call_builtin("join", &[mixed, s(",")]).unwrap_err();
        assert!(err.to_string().contains("number") || err.to_string().contains("string"));
    }

    #[test]
    fn join_empty_array_returns_empty_string() {
        let result = call_builtin("join", &[Value::Array(vec![]), s(",")]).unwrap();
        assert_eq!(result, s(""));
    }

    #[test]
    fn join_single_element_no_separator() {
        let result = call_builtin("join", &[arr(&["only"]), s(", ")]).unwrap();
        assert_eq!(result, s("only"));
    }

    #[test]
    fn length_string() {
        let result = call_builtin("length", &[s("hello")]).unwrap();
        assert_eq!(result, Value::Number(5.0));
    }

    #[test]
    fn length_string_multibyte_chars() {
        // "café" is 5 bytes but 4 Unicode scalar values.
        // length() must return 4 (character count), not 5 (byte count).
        let result = call_builtin("length", &[s("café")]).unwrap();
        assert_eq!(
            result,
            Value::Number(4.0),
            "length('café') should be 4 chars"
        );

        // Emoji: "a😀b" is 6 bytes but 3 chars.
        let result = call_builtin("length", &[s("a😀b")]).unwrap();
        assert_eq!(
            result,
            Value::Number(3.0),
            "length('a😀b') should be 3 chars"
        );
    }

    #[test]
    fn length_array() {
        let result = call_builtin("length", &[arr(&["a", "b", "c"])]).unwrap();
        assert_eq!(result, Value::Number(3.0));
    }

    #[test]
    fn length_requires_string_or_array() {
        let err = call_builtin("length", &[Value::Number(1.0)]).unwrap_err();
        assert!(err.to_string().contains("string or array"));
    }

    #[test]
    fn first_returns_first_element() {
        let result = call_builtin("first", &[arr(&["x", "y", "z"])]).unwrap();
        assert_eq!(result, s("x"));
    }

    #[test]
    fn first_empty_returns_null() {
        let result = call_builtin("first", &[Value::Array(vec![])]).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn last_returns_last_element() {
        let result = call_builtin("last", &[arr(&["x", "y", "z"])]).unwrap();
        assert_eq!(result, s("z"));
    }

    #[test]
    fn last_empty_returns_null() {
        let result = call_builtin("last", &[Value::Array(vec![])]).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn reverse_string() {
        let result = call_builtin("reverse", &[s("abc")]).unwrap();
        assert_eq!(result, s("cba"));
    }

    #[test]
    fn reverse_array() {
        let result = call_builtin("reverse", &[arr(&["a", "b", "c"])]).unwrap();
        assert_eq!(result, arr(&["c", "b", "a"]));
    }

    #[test]
    fn sort_strings_alphabetically() {
        let result = call_builtin("sort", &[arr(&["banana", "apple", "cherry"])]).unwrap();
        assert_eq!(result, arr(&["apple", "banana", "cherry"]));
    }

    #[test]
    fn sort_numbers_ascending() {
        let result = call_builtin("sort", &[num_arr(&[3.0, 1.0, 2.0])]).unwrap();
        assert_eq!(result, num_arr(&[1.0, 2.0, 3.0]));
    }

    #[test]
    fn sort_empty_array() {
        let result = call_builtin("sort", &[Value::Array(vec![])]).unwrap();
        assert_eq!(result, Value::Array(vec![]));
    }

    #[test]
    fn sort_mixed_types_rejected() {
        let mixed = Value::Array(vec![s("a"), Value::Number(1.0)]);
        let err = call_builtin("sort", &[mixed]).unwrap_err();
        assert!(err.to_string().contains("homogeneous") || err.to_string().contains("mixed"));
    }

    #[test]
    fn sort_nan_rejected() {
        let arr_with_nan = Value::Array(vec![Value::Number(1.0), Value::Number(f64::NAN)]);
        let err = call_builtin("sort", &[arr_with_nan]).unwrap_err();
        assert!(
            err.to_string().contains("non-finite"),
            "expected non-finite guard for NaN, got: {err}"
        );
    }

    #[test]
    fn sort_infinity_rejected() {
        let arr_with_inf = Value::Array(vec![Value::Number(1.0), Value::Number(f64::INFINITY)]);
        let err = call_builtin("sort", &[arr_with_inf]).unwrap_err();
        assert!(
            err.to_string().contains("non-finite"),
            "expected non-finite guard for infinity, got: {err}"
        );
    }

    #[test]
    fn unique_preserves_order_and_deduplicates() {
        let result = call_builtin("unique", &[arr(&["b", "a", "b", "c", "a"])]).unwrap();
        assert_eq!(result, arr(&["b", "a", "c"]));
    }

    #[test]
    fn unique_empty_array() {
        let result = call_builtin("unique", &[Value::Array(vec![])]).unwrap();
        assert_eq!(result, Value::Array(vec![]));
    }

    #[test]
    fn unique_no_collision_between_null_and_empty_string() {
        // unique_key must distinguish Null from String("") — both display as "".
        let mixed = Value::Array(vec![Value::Null, s(""), Value::Null, s("")]);
        let result = call_builtin("unique", &[mixed]).unwrap();
        assert_eq!(
            result,
            Value::Array(vec![Value::Null, s("")]),
            "Null and empty string must be treated as distinct values"
        );
    }

    #[test]
    fn unique_large_array_completes_in_linear_time() {
        // 10 000-element array of repeated values — O(n²) would be slow under Miri or tight loops.
        // This test is a smoke-check that the HashSet path doesn't regress.
        let items: Vec<Value> = (0..10_000).map(|i| s(&(i % 100).to_string())).collect();
        let result = call_builtin("unique", &[Value::Array(items)]).unwrap();
        if let Value::Array(deduped) = result {
            assert_eq!(deduped.len(), 100);
        } else {
            panic!("expected array result");
        }
    }

    // ── Type conversion ───────────────────────────────────────────────────────

    #[test]
    fn string_converts_number() {
        let result = call_builtin("string", &[Value::Number(42.0)]).unwrap();
        assert_eq!(result, s("42"));
    }

    #[test]
    fn string_converts_boolean() {
        let result = call_builtin("string", &[Value::Boolean(true)]).unwrap();
        assert_eq!(result, s("true"));
    }

    #[test]
    fn string_converts_null() {
        let result = call_builtin("string", &[Value::Null]).unwrap();
        assert_eq!(result, s(""));
    }

    #[test]
    fn number_converts_string() {
        let result = call_builtin("number", &[s("42")]).unwrap();
        assert_eq!(result, Value::Number(42.0));
    }

    #[test]
    fn number_converts_boolean_true() {
        let result = call_builtin("number", &[Value::Boolean(true)]).unwrap();
        assert_eq!(result, Value::Number(1.0));
    }

    #[test]
    fn number_converts_boolean_false() {
        let result = call_builtin("number", &[Value::Boolean(false)]).unwrap();
        assert_eq!(result, Value::Number(0.0));
    }

    #[test]
    fn number_converts_null() {
        let result = call_builtin("number", &[Value::Null]).unwrap();
        assert_eq!(result, Value::Number(0.0));
    }

    #[test]
    fn number_rejects_non_numeric_string() {
        let err = call_builtin("number", &[s("abc")]).unwrap_err();
        assert!(err.to_string().contains("cannot convert"));
    }

    #[test]
    fn number_idempotent() {
        let result = call_builtin("number", &[Value::Number(3.5)]).unwrap();
        assert_eq!(result, Value::Number(3.5));
    }

    // ── get_builtin lookup ────────────────────────────────────────────────────

    #[test]
    fn get_builtin_returns_meta_for_known_name() {
        let meta = get_builtin("upper").unwrap();
        assert_eq!(meta.name, "upper");
        assert_eq!(meta.min_args, 1);
        assert_eq!(meta.max_args, 1);
    }

    #[test]
    fn get_builtin_returns_none_for_unknown() {
        assert!(get_builtin("nonexistent").is_none());
    }

    // ── Missing negative / not-found tests (batch-5) ─────────────────────────

    #[test]
    fn contains_requires_string_or_array() {
        let err = call_builtin("contains", &[Value::Number(1.0), s("x")]).unwrap_err();
        assert!(
            err.to_string().contains("string or array"),
            "expected 'string or array' in error, got: {err}"
        );
    }

    #[test]
    fn contains_array_not_found() {
        let result = call_builtin("contains", &[arr(&["a", "b"]), s("z")]).unwrap();
        assert_eq!(result, Value::Boolean(false));
    }

    #[test]
    fn reverse_requires_string_or_array() {
        let err = call_builtin("reverse", &[Value::Number(42.0)]).unwrap_err();
        assert!(
            err.to_string().contains("string or array"),
            "expected 'string or array' in error, got: {err}"
        );
    }

    #[test]
    fn slice_requires_string_or_array() {
        let err = call_builtin("slice", &[Value::Number(1.0), Value::Number(0.0)]).unwrap_err();
        assert!(
            err.to_string().contains("string or array"),
            "expected 'string or array' in error, got: {err}"
        );
    }

    #[test]
    fn number_rejects_array() {
        let err = call_builtin("number", &[arr(&["a", "b"])]).unwrap_err();
        assert!(
            err.to_string().contains("cannot convert"),
            "expected 'cannot convert' in error, got: {err}"
        );
    }

    #[test]
    fn number_rejects_object() {
        let obj = Value::Object(std::collections::HashMap::new());
        let err = call_builtin("number", &[obj]).unwrap_err();
        assert!(
            err.to_string().contains("cannot convert"),
            "expected 'cannot convert' in error, got: {err}"
        );
    }
}
