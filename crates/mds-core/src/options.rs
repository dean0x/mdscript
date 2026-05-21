//! Shared options-parsing utilities for WASM and napi binding layers.
//!
//! Both binding layers accept a user-supplied `vars` object and need to:
//! 1. Determine the runtime type-name of an arbitrary JSON value.
//! 2. Validate and convert a JSON vars object into a `HashMap<String, Value>`.
//! 3. Reject unknown option keys with a uniform error message.
//!
//! Centralising these three functions here eliminates identical copies that
//! previously lived in `mds-wasm/src/lib.rs` and `mds-napi/src/lib.rs`.

use std::collections::HashMap;

use crate::error::MdsError;
use crate::value::Value;

// ── json_type_name ────────────────────────────────────────────────────────────

/// Return a human-readable type name for a JSON value, for use in diagnostics.
///
/// # Examples
///
/// ```
/// use mds::json_type_name;
/// use serde_json::json;
///
/// assert_eq!(json_type_name(&json!(null)),   "null");
/// assert_eq!(json_type_name(&json!(true)),   "boolean");
/// assert_eq!(json_type_name(&json!(42)),     "number");
/// assert_eq!(json_type_name(&json!("hi")),   "string");
/// assert_eq!(json_type_name(&json!([])),     "array");
/// assert_eq!(json_type_name(&json!({})),     "object");
/// ```
#[must_use]
pub fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ── VarsError ─────────────────────────────────────────────────────────────────

/// Errors that can occur when parsing the `vars` option.
#[derive(Debug)]
pub enum VarsError {
    /// The `vars` value was not a JSON object (e.g. it was an array or string).
    ///
    /// Contains a human-readable description of the actual type.
    InvalidType(String),

    /// A value inside the `vars` object could not be converted to an MDS `Value`.
    Conversion(MdsError),
}

impl std::fmt::Display for VarsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VarsError::InvalidType(msg) => write!(f, "{msg}"),
            VarsError::Conversion(e) => write!(f, "vars conversion error: {e}"),
        }
    }
}

impl std::error::Error for VarsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VarsError::InvalidType(_) => None,
            VarsError::Conversion(e) => Some(e),
        }
    }
}

// ── parse_json_vars ───────────────────────────────────────────────────────────

/// Parse a JSON `vars` value into a `HashMap<String, Value>`.
///
/// Accepts a `serde_json::Value` that should be a JSON object whose entries
/// are converted to MDS [`Value`]s. Returns `VarsError::InvalidType` if the
/// value is not a plain object (e.g. if it is an array or a string), and
/// `VarsError::Conversion` if any entry cannot be converted.
///
/// Pre-sizes the output map with [`HashMap::with_capacity`] to avoid
/// incremental rehashing for typical-sized vars objects.
///
/// # Examples
///
/// ```
/// use mds::{parse_json_vars, VarsError};
/// use serde_json::json;
///
/// // Valid object
/// let vars = parse_json_vars(json!({ "name": "World" })).unwrap();
/// assert_eq!(vars.len(), 1);
///
/// // Array rejected
/// let err = parse_json_vars(json!(["a", "b"])).unwrap_err();
/// assert!(matches!(err, VarsError::InvalidType(_)));
/// ```
pub fn parse_json_vars(vars_value: serde_json::Value) -> Result<HashMap<String, Value>, VarsError> {
    let serde_json::Value::Object(map) = vars_value else {
        return Err(VarsError::InvalidType(format!(
            "options.vars must be a plain object, got {}",
            json_type_name(&vars_value)
        )));
    };

    let mut result = HashMap::with_capacity(map.len());
    for (key, val) in map {
        let mds_val = Value::from_json(val).map_err(VarsError::Conversion)?;
        result.insert(key, mds_val);
    }
    Ok(result)
}

// ── reject_unknown_json_keys ──────────────────────────────────────────────────

/// Reject any key in `map` that is not in the `known` list.
///
/// Collects **all** unknown keys before returning so that the error message
/// names every offending key at once, not just the first one encountered.
///
/// # Error format
///
/// - Single unknown key:
///   `unknown option key "foo"; recognised keys are: basePath, vars`
/// - Multiple unknown keys:
///   `unknown option keys: "foo", "bar"; recognised keys are: basePath, vars`
///
/// Returns `Ok(())` when every key in `map` appears in `known`.
///
/// # Examples
///
/// ```
/// use mds::reject_unknown_json_keys;
/// use serde_json::{json, Map};
///
/// let map: Map<String, serde_json::Value> = serde_json::from_str(r#"{"basePath": "."}"#).unwrap();
/// assert!(reject_unknown_json_keys(&map, &["basePath", "vars"]).is_ok());
///
/// let bad: Map<String, serde_json::Value> = serde_json::from_str(r#"{"typo": "x"}"#).unwrap();
/// assert!(reject_unknown_json_keys(&bad, &["basePath", "vars"]).is_err());
/// ```
pub fn reject_unknown_json_keys(
    map: &serde_json::Map<String, serde_json::Value>,
    known: &[&str],
) -> Result<(), String> {
    let unknowns: Vec<&str> = map
        .keys()
        .filter(|k| !known.contains(&k.as_str()))
        .map(String::as_str)
        .collect();

    if unknowns.is_empty() {
        return Ok(());
    }

    let recognised = known.join(", ");

    let msg = if unknowns.len() == 1 {
        format!(
            "unknown option key \"{}\"; recognised keys are: {}",
            unknowns[0], recognised
        )
    } else {
        let listed: Vec<String> = unknowns.iter().map(|k| format!("\"{k}\"")).collect();
        format!(
            "unknown option keys: {}; recognised keys are: {}",
            listed.join(", "),
            recognised
        )
    };

    Err(msg)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── json_type_name ────────────────────────────────────────────────────────

    #[test]
    fn test_json_type_name_all_variants() {
        assert_eq!(json_type_name(&json!(null)), "null");
        assert_eq!(json_type_name(&json!(true)), "boolean");
        assert_eq!(json_type_name(&json!(false)), "boolean");
        assert_eq!(json_type_name(&json!(42)), "number");
        assert_eq!(json_type_name(&json!(3.14)), "number");
        assert_eq!(json_type_name(&json!("hello")), "string");
        assert_eq!(json_type_name(&json!([])), "array");
        assert_eq!(json_type_name(&json!([1, 2])), "array");
        assert_eq!(json_type_name(&json!({})), "object");
        assert_eq!(json_type_name(&json!({"a": 1})), "object");
    }

    // ── parse_json_vars ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_json_vars_valid_object() {
        let result = parse_json_vars(json!({ "name": "World", "count": 42 }));
        let vars = result.expect("valid object should succeed");
        assert_eq!(vars.len(), 2);
        assert!(matches!(vars["name"], Value::String(ref s) if s == "World"));
        assert!(matches!(vars["count"], Value::Number(n) if (n - 42.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_parse_json_vars_empty_object() {
        let result = parse_json_vars(json!({}));
        let vars = result.expect("empty object should succeed");
        assert!(vars.is_empty());
    }

    #[test]
    fn test_parse_json_vars_nested_values() {
        let result = parse_json_vars(json!({
            "flag": true,
            "items": [1, 2, 3],
            "inner": { "x": "y" }
        }));
        let vars = result.expect("nested values should succeed");
        assert_eq!(vars.len(), 3);
        assert!(matches!(vars["flag"], Value::Boolean(true)));
        assert!(matches!(vars["items"], Value::Array(_)));
        assert!(matches!(vars["inner"], Value::Object(_)));
    }

    #[test]
    fn test_parse_json_vars_invalid_string() {
        let err = parse_json_vars(json!("not an object")).unwrap_err();
        assert!(
            matches!(&err, VarsError::InvalidType(msg) if msg.contains("string")),
            "expected InvalidType with 'string' mention, got: {err}"
        );
    }

    #[test]
    fn test_parse_json_vars_invalid_array() {
        let err = parse_json_vars(json!(["a", "b"])).unwrap_err();
        assert!(
            matches!(&err, VarsError::InvalidType(msg) if msg.contains("array")),
            "expected InvalidType with 'array' mention, got: {err}"
        );
    }

    #[test]
    fn test_parse_json_vars_invalid_null() {
        let err = parse_json_vars(json!(null)).unwrap_err();
        assert!(
            matches!(&err, VarsError::InvalidType(msg) if msg.contains("null")),
            "expected InvalidType with 'null' mention, got: {err}"
        );
    }

    #[test]
    fn test_parse_json_vars_conversion_error() {
        // Build a JSON value nested beyond MAX_VALUE_DEPTH (64).
        // Construct 66 levels deep: {"a": {"a": {"a": ... }}}
        let mut deep = serde_json::json!("leaf");
        for _ in 0..66 {
            deep = serde_json::json!({ "a": deep });
        }
        // Wrap in a single-key object so parse_json_vars sees an Object
        let vars_val = json!({ "v": deep });
        let err = parse_json_vars(vars_val).unwrap_err();
        assert!(
            matches!(err, VarsError::Conversion(_)),
            "expected Conversion error for deeply nested value, got: {err}"
        );
    }

    // ── reject_unknown_json_keys ──────────────────────────────────────────────

    #[test]
    fn test_reject_unknown_empty_map() {
        let map: serde_json::Map<String, serde_json::Value> = Default::default();
        assert!(reject_unknown_json_keys(&map, &["basePath", "vars"]).is_ok());
    }

    #[test]
    fn test_reject_unknown_all_known() {
        let map: serde_json::Map<_, _> =
            serde_json::from_str(r#"{"basePath": ".", "vars": {}}"#).unwrap();
        assert!(reject_unknown_json_keys(&map, &["basePath", "vars"]).is_ok());
    }

    #[test]
    fn test_reject_unknown_single_key() {
        let map: serde_json::Map<_, _> =
            serde_json::from_str(r#"{"typo": "x"}"#).unwrap();
        let err = reject_unknown_json_keys(&map, &["basePath", "vars"]).unwrap_err();
        assert!(
            err.contains("unknown option key"),
            "single-key message should say 'unknown option key': {err}"
        );
        assert!(err.contains("\"typo\""), "should name the unknown key: {err}");
        assert!(
            err.contains("basePath") && err.contains("vars"),
            "should list recognised keys: {err}"
        );
        // Plural form must NOT appear for a single key
        assert!(
            !err.contains("keys:"),
            "should use singular form for one key: {err}"
        );
    }

    #[test]
    fn test_reject_unknown_multiple_keys() {
        let map: serde_json::Map<_, _> =
            serde_json::from_str(r#"{"foo": 1, "bar": 2}"#).unwrap();
        let err = reject_unknown_json_keys(&map, &["basePath", "vars"]).unwrap_err();
        assert!(
            err.contains("unknown option keys:"),
            "multiple-key message should say 'unknown option keys:': {err}"
        );
        assert!(err.contains("\"foo\""), "should name 'foo': {err}");
        assert!(err.contains("\"bar\""), "should name 'bar': {err}");
    }

    // ── VarsError Display / source ────────────────────────────────────────────

    #[test]
    fn test_vars_error_display() {
        let invalid_type = VarsError::InvalidType("bad type".to_string());
        assert_eq!(format!("{invalid_type}"), "bad type");

        let mds_err = MdsError::json_error("depth exceeded");
        let conversion = VarsError::Conversion(mds_err);
        let display = format!("{conversion}");
        assert!(
            display.contains("vars conversion error"),
            "Conversion display should prefix message: {display}"
        );
    }

    #[test]
    fn test_vars_error_source() {
        use std::error::Error;

        let invalid_type = VarsError::InvalidType("bad".to_string());
        assert!(
            invalid_type.source().is_none(),
            "InvalidType has no source"
        );

        let mds_err = MdsError::json_error("depth exceeded");
        let conversion = VarsError::Conversion(mds_err);
        assert!(
            conversion.source().is_some(),
            "Conversion should have a source"
        );
    }
}
