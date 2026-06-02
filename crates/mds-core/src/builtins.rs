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

use crate::error::MdsError;
use crate::value::Value;

/// Metadata about a built-in function.
pub(crate) struct BuiltinMeta {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: usize,
}

/// All built-in function definitions.
static BUILTINS: &[BuiltinMeta] = &[
    // String operations
    BuiltinMeta {
        name: "upper",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "lower",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "trim",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "replace",
        min_args: 3,
        max_args: 3,
    },
    BuiltinMeta {
        name: "split",
        min_args: 2,
        max_args: 2,
    },
    BuiltinMeta {
        name: "starts_with",
        min_args: 2,
        max_args: 2,
    },
    BuiltinMeta {
        name: "ends_with",
        min_args: 2,
        max_args: 2,
    },
    BuiltinMeta {
        name: "slice",
        min_args: 2,
        max_args: 3,
    },
    BuiltinMeta {
        name: "contains",
        min_args: 2,
        max_args: 2,
    },
    // Array operations
    BuiltinMeta {
        name: "join",
        min_args: 2,
        max_args: 2,
    },
    BuiltinMeta {
        name: "length",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "first",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "last",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "reverse",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "sort",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "unique",
        min_args: 1,
        max_args: 1,
    },
    // Type conversion
    BuiltinMeta {
        name: "string",
        min_args: 1,
        max_args: 1,
    },
    BuiltinMeta {
        name: "number",
        min_args: 1,
        max_args: 1,
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
/// # Errors
///
/// Returns `MdsError::BuiltinError` when an argument has the wrong type, and
/// `MdsError::arity` when the wrong number of arguments is passed.
pub(crate) fn call_builtin(name: &str, args: &[Value]) -> Result<Value, MdsError> {
    match name {
        "upper" => builtin_upper(args),
        "lower" => builtin_lower(args),
        "trim" => builtin_trim(args),
        "replace" => builtin_replace(args),
        "split" => builtin_split(args),
        "starts_with" => builtin_starts_with(args),
        "ends_with" => builtin_ends_with(args),
        "slice" => builtin_slice(args),
        "contains" => builtin_contains(args),
        "join" => builtin_join(args),
        "length" => builtin_length(args),
        "first" => builtin_first(args),
        "last" => builtin_last(args),
        "reverse" => builtin_reverse(args),
        "sort" => builtin_sort(args),
        "unique" => builtin_unique(args),
        "string" => builtin_string(args),
        "number" => builtin_number(args),
        _ => Err(MdsError::undefined_fn(name)),
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
    match &args[0] {
        Value::String(s) => Ok(s.as_str()),
        other => Err(type_err(fn_name, "", "string", other.type_name())),
    }
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
    Ok(Value::String(s.replace(from, to)))
}

fn builtin_split(args: &[Value]) -> Result<Value, MdsError> {
    let s = require_string_at(args, 0, "split", "first")?;
    let sep = require_string_at(args, 1, "split", "second")?;
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
            let start = require_number_index(&args[1], "slice", "second")?;
            let len = s.len();
            // Clamp start to bounds, then snap to nearest valid char boundary
            let start = snap_to_char_boundary(s, start.min(len));
            if args.len() == 3 {
                let end = require_number_index(&args[2], "slice", "third")?;
                let end = snap_to_char_boundary(s, end.clamp(start, len));
                Ok(Value::String(s[start..end].to_string()))
            } else {
                Ok(Value::String(s[start..].to_string()))
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
/// Clamps negative numbers to 0. Returns an error for non-number values.
fn require_number_index(val: &Value, fn_name: &str, pos: &str) -> Result<usize, MdsError> {
    match val {
        Value::Number(n) => Ok(n.max(0.0).floor() as usize),
        other => Err(type_err(fn_name, pos, "number", other.type_name())),
    }
}

/// Snap a byte index to the nearest valid UTF-8 char boundary in `s`.
///
/// If `idx` is already on a char boundary, returns it unchanged.
/// Otherwise, walks backward to find the start of the enclosing character.
/// This prevents panics when byte-based slice indices fall inside multi-byte
/// UTF-8 sequences (e.g. slicing into the middle of an emoji or accented char).
fn snap_to_char_boundary(s: &str, idx: usize) -> usize {
    if s.is_char_boundary(idx) {
        return idx;
    }
    // Walk backward at most 3 bytes (max UTF-8 continuation sequence length).
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ── Array operations ──────────────────────────────────────────────────────────

fn builtin_join(args: &[Value]) -> Result<Value, MdsError> {
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Err(type_err("join", "first", "array", other.type_name())),
    };
    let sep = require_string_at(args, 1, "join", "second")?;
    let parts: Result<Vec<String>, MdsError> = arr
        .iter()
        .map(|v| match v {
            Value::String(s) => Ok(s.clone()),
            other => Err(MdsError::builtin_error(format!(
                "join() requires an array of strings, but found {} in array",
                other.type_name()
            ))),
        })
        .collect();
    Ok(Value::String(parts?.join(sep)))
}

fn builtin_length(args: &[Value]) -> Result<Value, MdsError> {
    match &args[0] {
        Value::String(s) => Ok(Value::Number(s.len() as f64)),
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

    // Determine element type from first element, then verify homogeneity and sort.
    let mut sorted = arr.clone();
    match &sorted[0] {
        Value::String(_) => {
            for item in &sorted {
                if !matches!(item, Value::String(_)) {
                    return Err(MdsError::builtin_error(format!(
                        "sort() requires a homogeneous array; found {} mixed with string",
                        item.type_name()
                    )));
                }
            }
            sorted.sort_by(|a, b| match (a, b) {
                (Value::String(a), Value::String(b)) => a.cmp(b),
                _ => unreachable!(),
            });
        }
        Value::Number(_) => {
            for item in &sorted {
                if !matches!(item, Value::Number(_)) {
                    return Err(MdsError::builtin_error(format!(
                        "sort() requires a homogeneous array; found {} mixed with number",
                        item.type_name()
                    )));
                }
            }
            sorted.sort_by(|a, b| match (a, b) {
                (Value::Number(a), Value::Number(b)) => {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => unreachable!(),
            });
        }
        other => {
            return Err(MdsError::builtin_error(format!(
                "sort() requires an array of strings or numbers, got array of {}",
                other.type_name()
            )));
        }
    }
    Ok(Value::Array(sorted))
}

fn builtin_unique(args: &[Value]) -> Result<Value, MdsError> {
    let arr = match &args[0] {
        Value::Array(a) => a,
        other => return Err(type_err("unique", "", "array", other.type_name())),
    };
    // Order-preserving deduplication
    let mut result: Vec<Value> = Vec::new();
    for item in arr {
        if !result.contains(item) {
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
    fn split_on_comma() {
        let result = call_builtin("split", &[s("a,b,c"), s(",")]).unwrap();
        assert_eq!(result, arr(&["a", "b", "c"]));
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
    fn slice_string_multibyte_snaps_to_char_boundary() {
        // "café" is 5 bytes: c(1) a(1) f(1) é(2 bytes: 0xC3 0xA9).
        // Slicing at byte 4 falls inside the 2-byte 'é'. snap_to_char_boundary
        // must round down to byte 3 (start of 'é' is actually byte 3 in "café").
        // This must NOT panic.
        let cafe = s("café");
        let result = call_builtin(
            "slice",
            &[cafe.clone(), Value::Number(0.0), Value::Number(4.0)],
        );
        assert!(result.is_ok(), "slice into multibyte char must not panic");
        // Byte 4 snaps to byte 3 (start of 'é' = bytes 3..5), so we get "caf".
        assert_eq!(result.unwrap(), s("caf"));
    }

    #[test]
    fn slice_string_emoji_does_not_panic() {
        // "a😀b" — 😀 is 4 bytes (U+1F600). Slicing mid-emoji must not panic.
        let emoji_str = s("a😀b");
        let result = call_builtin(
            "slice",
            &[emoji_str, Value::Number(2.0), Value::Number(4.0)],
        );
        assert!(result.is_ok(), "slice mid-emoji must not panic");
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
    fn length_string() {
        let result = call_builtin("length", &[s("hello")]).unwrap();
        assert_eq!(result, Value::Number(5.0));
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
    fn unique_preserves_order_and_deduplicates() {
        let result = call_builtin("unique", &[arr(&["b", "a", "b", "c", "a"])]).unwrap();
        assert_eq!(result, arr(&["b", "a", "c"]));
    }

    #[test]
    fn unique_empty_array() {
        let result = call_builtin("unique", &[Value::Array(vec![])]).unwrap();
        assert_eq!(result, Value::Array(vec![]));
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

    // ── snap_to_char_boundary ──────────────────────────────────────────────

    #[test]
    fn snap_to_char_boundary_on_boundary() {
        assert_eq!(snap_to_char_boundary("hello", 0), 0);
        assert_eq!(snap_to_char_boundary("hello", 3), 3);
        assert_eq!(snap_to_char_boundary("hello", 5), 5);
    }

    #[test]
    fn snap_to_char_boundary_mid_multibyte() {
        // "café" = c(1) a(1) f(1) é(2 bytes at index 3..5)
        // Index 4 is inside 'é' → should snap to 3
        assert_eq!(snap_to_char_boundary("café", 4), 3);
    }

    #[test]
    fn snap_to_char_boundary_emoji() {
        // "a😀b" = a(1) 😀(4 bytes at index 1..5) b(1 byte at index 5)
        // Indices 2, 3, 4 are inside 😀 → should snap to 1
        assert_eq!(snap_to_char_boundary("a😀b", 2), 1);
        assert_eq!(snap_to_char_boundary("a😀b", 3), 1);
        assert_eq!(snap_to_char_boundary("a😀b", 4), 1);
        // Index 5 is on boundary (start of 'b')
        assert_eq!(snap_to_char_boundary("a😀b", 5), 5);
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
}
