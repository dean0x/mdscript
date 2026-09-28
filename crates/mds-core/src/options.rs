//! Shared options-parsing and wire-format utilities for WASM and napi binding layers.
//!
//! Both binding layers accept a user-supplied `vars` object and need to:
//! 1. Determine the runtime type-name of an arbitrary JSON value.
//! 2. Validate and convert a JSON vars object into a `HashMap<String, Value>`.
//! 3. Reject unknown option keys with a uniform error message.
//! 4. Inject `lint_warnings` into the canonical-JSON result for unknown rule names.
//! 5. Parse a `rules` map into per-rule severities — the Python binding too.
//!
//! Centralising these functions here eliminates identical copies that previously
//! lived in `mds-wasm/src/lib.rs` and `mds-napi/src/lib.rs`, and ensures the
//! D8 wire contract (`lint_warnings` key name, shape, absent-when-empty semantics)
//! has a single authoritative definition.

use std::collections::HashMap;

use crate::error::MdsError;
use crate::lint::diagnostic::SeveritySpellings;
use crate::lint::{sanitize_control_chars_wire, Severity};
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
#[non_exhaustive]
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

// ── format_unknown_keys_error ─────────────────────────────────────────────────

/// Build the "unknown option key(s)" error message from a list of offending keys.
///
/// Shared by [`reject_unknown_json_keys`] in this module and by the NAPI/WASM
/// binding crates, which collect unknown keys from their own runtime types before
/// delegating message construction here.
///
/// # Format
///
/// - Single unknown key:
///   `unknown option key "foo"; recognised keys are: basePath, vars`
/// - Multiple unknown keys:
///   `unknown option keys: "foo", "bar"; recognised keys are: basePath, vars`
///
/// Each unknown key is the caller's text, so it is WIRE-escaped with
/// [`sanitize_control_chars_wire`] as the message is built (#418): control characters,
/// DEL and the bidi/format hazards become escape text, and TAB stays raw. A clean key
/// shows unchanged. `@mdscript/mds`'s own key check (`assertKnownKeys`) escapes
/// identically, so the two messages stay byte-identical for any key that is valid
/// Unicode: a lone surrogate reaches napi as U+FFFD, and `assertKnownKeys` shows it
/// as is.
///
/// # Panics
///
/// Panics (in debug builds) if `unknowns` is empty — callers must only call
/// this when at least one unknown key was found.
#[must_use]
pub fn format_unknown_keys_error(unknowns: &[&str], known: &[&str]) -> String {
    debug_assert!(!unknowns.is_empty(), "called with empty unknowns list");
    let recognised = known.join(", ");
    if let [only] = unknowns {
        format!(
            "unknown option key \"{}\"; recognised keys are: {}",
            sanitize_control_chars_wire(only),
            recognised
        )
    } else {
        let listed: Vec<String> = unknowns
            .iter()
            .map(|k| format!("\"{}\"", sanitize_control_chars_wire(k)))
            .collect();
        format!(
            "unknown option keys: {}; recognised keys are: {}",
            listed.join(", "),
            recognised
        )
    }
}

// ── reject_unknown_json_keys ──────────────────────────────────────────────────

/// Reject any key in `map` that is not in the `known` list.
///
/// Collects **all** unknown keys before returning so that the error message
/// names every offending key at once, not just the first one encountered.
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

    Err(format_unknown_keys_error(&unknowns, known))
}

// ── parse_rule_severities ─────────────────────────────────────────────────────

/// Parse a `rules` map — rule name → severity spelling — into the severity each rule
/// is set to.
///
/// The napi, WASM and Python bindings each convert the caller's `rules` value to a
/// JSON object and call this, so every binding reads a severity, and words its error,
/// the same way; each only wraps the message in its own error object (#418). The CLI
/// reads `mds.json`'s `lint.rules` through it too (#175). Pass the result to
/// [`crate::LintConfig::from_rules_checked`].
///
/// `field` names the map in a message as its author writes it — `"options.rules"` on
/// napi and WASM, `"rules"` for Python's keyword argument, `"lint.rules"` in
/// `mds.json`. It is the caller's own text, and is shown as given.
///
/// # Errors
///
/// The message for the first rule, in the map's key order, whose value is not one of
/// [`Severity`]'s four spellings (its `FromStr`, the one severity parser):
/// - `<field>["<name>"] must be a severity string, got <type>` for a value that is not
///   a string, `<type>` as [`json_type_name`] names it;
/// - `<field>["<name>"]: unknown severity "<value>"; expected "off", "info", "warn", or
///   "error"` for any other string.
///
/// The rule name and the value are the caller's text, so each is WIRE-escaped with
/// [`sanitize_control_chars_wire`] as the message is built: control characters, DEL
/// and the bidi/format hazards become escape text, and TAB stays raw — a rule name is
/// an identifier, not a path. A name and a value with none of those characters show
/// unchanged.
///
/// # Examples
///
/// ```
/// use mds::{parse_rule_severities, Severity};
/// use serde_json::{json, Map, Value};
///
/// let rules: Map<String, Value> = serde_json::from_value(json!({ "unused-variable": "off" }))?;
/// let severities = parse_rule_severities(rules, "options.rules").unwrap();
/// assert_eq!(severities["unused-variable"], Severity::Off);
///
/// let rules: Map<String, Value> = serde_json::from_value(json!({ "unused-variable": "Off" }))?;
/// assert_eq!(
///     parse_rule_severities(rules, "rules").unwrap_err(),
///     "rules[\"unused-variable\"]: unknown severity \"Off\"; \
///      expected \"off\", \"info\", \"warn\", or \"error\""
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
// `#[inline]` so each binding compiles it at its own optimization level: mds-wasm is
// built for size and mds-core at opt-level 3, and compiled in mds-core this function
// measured about 3.2 KB larger in the WASM binary.
#[inline]
pub fn parse_rule_severities(
    rules: serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<HashMap<String, Severity>, String> {
    let mut severities = HashMap::with_capacity(rules.len());
    for (name, value) in rules {
        let serde_json::Value::String(spelling) = &value else {
            return Err(format!(
                "{field}[\"{}\"] must be a severity string, got {}",
                sanitize_control_chars_wire(&name),
                json_type_name(&value)
            ));
        };
        let Ok(severity) = spelling.parse::<Severity>() else {
            return Err(format!(
                "{field}[\"{}\"]: unknown severity \"{}\"; expected {SeveritySpellings}",
                sanitize_control_chars_wire(&name),
                sanitize_control_chars_wire(spelling)
            ));
        };
        severities.insert(name, severity);
    }
    Ok(severities)
}

// ── attach_lint_warnings ──────────────────────────────────────────────────────

/// Inject `lint_warnings` into a canonical JSON result object when a warning is present.
///
/// D8 (AC-224-1): the napi, WASM, and Python bindings surface unknown-rule warnings
/// by adding a `lint_warnings: string[]` field to the returned JSON object. This
/// function is the single implementation of that D8 wire contract — the key name
/// `"lint_warnings"`, the array-of-one shape, and the absent-when-empty semantics
/// — so the contract cannot diverge across surfaces.
///
/// `Option<String>` rather than `Vec<String>`: there is exactly one warning message
/// today (unknown rule names are reported as a single sentence), so a vector would be
/// over-general plumbing. The JSON shape is still `string[]` — the array is built
/// here — so adding a second warning kind later is a change to this function, not to
/// the wire contract.
///
/// Deliberately kept out of `LintResult::to_canonical_json` so the CLI serializer
/// path (`--format json`) remains byte-frozen: the CLI writes the warning to stderr
/// via `eprint_warning` and never touches the JSON.
///
/// The precondition (the target value is a JSON object produced by
/// `LintResult::to_canonical_json`) is **structural**: the argument type
/// `&mut serde_json::Map<String, serde_json::Value>` is unrepresentable for non-object
/// values, so the caller must extract the map explicitly before calling. This eliminates
/// the silent-discard failure mode that would exist with a `serde_json::Value` parameter
/// (PF-005: make preconditions structural rather than asserted at runtime). Callers
/// obtain a map reference via `value.as_object_mut().expect(…)` — the `expect` is the
/// correct tool because `LintResult::to_canonical_json` is contractually guaranteed to
/// return a JSON object.
pub fn attach_lint_warnings(
    json: &mut serde_json::Map<String, serde_json::Value>,
    warning: Option<String>,
) {
    if let Some(w) = warning {
        json.insert(
            "lint_warnings".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(w)]),
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::lint::ParseSeverityError;

    // ── json_type_name ────────────────────────────────────────────────────────

    #[test]
    fn test_json_type_name_all_variants() {
        assert_eq!(json_type_name(&json!(null)), "null");
        assert_eq!(json_type_name(&json!(true)), "boolean");
        assert_eq!(json_type_name(&json!(false)), "boolean");
        assert_eq!(json_type_name(&json!(42)), "number");
        assert_eq!(json_type_name(&json!(2.72)), "number");
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
        let map: serde_json::Map<_, _> = serde_json::from_str(r#"{"typo": "x"}"#).unwrap();
        let err = reject_unknown_json_keys(&map, &["basePath", "vars"]).unwrap_err();
        assert!(
            err.contains("unknown option key"),
            "single-key message should say 'unknown option key': {err}"
        );
        assert!(
            err.contains("\"typo\""),
            "should name the unknown key: {err}"
        );
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
        let map: serde_json::Map<_, _> = serde_json::from_str(r#"{"foo": 1, "bar": 2}"#).unwrap();
        let err = reject_unknown_json_keys(&map, &["basePath", "vars"]).unwrap_err();
        assert!(
            err.contains("unknown option keys:"),
            "multiple-key message should say 'unknown option keys:': {err}"
        );
        assert!(err.contains("\"foo\""), "should name 'foo': {err}");
        assert!(err.contains("\"bar\""), "should name 'bar': {err}");
    }

    // ── #418: unknown option keys are WIRE-escaped ────────────────────────────

    /// `a`, ESC, `b`, LF, `c`, TAB, `d` — built with `char::from_u32` at runtime, never
    /// typed as live bytes (PF-018) — and how the WIRE escaper shows it.
    fn hostile_key() -> (String, String) {
        let ch = |cp: u32| char::from_u32(cp).expect("a valid scalar value");
        let esc = |cp: u32| format!("\\u{cp:04X}");
        let hostile: String = ['a', ch(0x1b), 'b', ch(0x0a), 'c', ch(0x09), 'd']
            .iter()
            .collect();
        let shown = format!("a{}b{}c{}d", esc(0x1b), esc(0x0a), ch(0x09));
        (hostile, shown)
    }

    #[test]
    fn format_unknown_keys_error_wire_escapes_keys_in_both_forms() {
        let (hostile, shown) = hostile_key();
        assert_eq!(
            format_unknown_keys_error(&[&hostile], &["basePath", "vars"]),
            format!("unknown option key \"{shown}\"; recognised keys are: basePath, vars")
        );
        assert_eq!(
            format_unknown_keys_error(&["typo", &hostile], &["vars"]),
            format!("unknown option keys: \"typo\", \"{shown}\"; recognised keys are: vars")
        );
        // Clean keys (the control): byte-identical to the unescaped form.
        assert_eq!(
            format_unknown_keys_error(&["typo"], &["basePath", "vars"]),
            "unknown option key \"typo\"; recognised keys are: basePath, vars"
        );
        assert_eq!(
            format_unknown_keys_error(&["foo", "bar"], &["vars"]),
            "unknown option keys: \"foo\", \"bar\"; recognised keys are: vars"
        );
    }

    #[test]
    fn format_unknown_keys_error_leaves_only_tab_raw() {
        // A key holding every forbidden path character: WIRE escapes all but TAB.
        let key: String = (0..=0xFFFF_u32)
            .filter_map(char::from_u32)
            .filter(|&c| crate::is_forbidden_path_char(c))
            .collect();
        assert_eq!(key.chars().count(), 80, "non-vacuity: the whole class");
        for message in [
            format_unknown_keys_error(&[&key], &["vars"]),
            format_unknown_keys_error(&[&key, &key], &["vars"]),
        ] {
            let raw: Vec<char> = message
                .chars()
                .filter(|&c| crate::is_forbidden_path_char(c))
                .collect();
            assert!(
                raw.iter().all(|&c| c == '\t') && !raw.is_empty(),
                "{message:?}"
            );
        }
    }

    #[test]
    fn reject_unknown_json_keys_wire_escapes_the_key() {
        let (hostile, shown) = hostile_key();
        let mut map = serde_json::Map::new();
        map.insert(hostile, json!(1));
        assert_eq!(
            reject_unknown_json_keys(&map, &["vars"]).unwrap_err(),
            format!("unknown option key \"{shown}\"; recognised keys are: vars")
        );
    }

    // ── #418: parse_rule_severities ───────────────────────────────────────────

    /// A `rules` map of `(name, value)` pairs.
    fn rules_of(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    /// How an unknown-severity message lists the four spellings.
    const EXPECTED: &str = "expected \"off\", \"info\", \"warn\", or \"error\"";

    #[test]
    fn parse_rule_severities_reads_each_rule_at_its_severity() {
        let rules = rules_of(&[
            ("a", json!("off")),
            ("b", json!("info")),
            ("c", json!("warn")),
            ("d", json!("error")),
        ]);
        assert_eq!(
            parse_rule_severities(rules, "options.rules"),
            Ok(HashMap::from([
                ("a".to_owned(), Severity::Off),
                ("b".to_owned(), Severity::Info),
                ("c".to_owned(), Severity::Warn),
                ("d".to_owned(), Severity::Error),
            ]))
        );
        assert_eq!(
            parse_rule_severities(serde_json::Map::new(), "rules"),
            Ok(HashMap::new())
        );
    }

    #[test]
    fn parse_rule_severities_names_the_rule_and_the_value_wire_escaped() {
        let (hostile, shown) = hostile_key();
        let cases = [
            (
                rules_of(&[(&hostile, json!(1))]),
                "options.rules",
                format!("options.rules[\"{shown}\"] must be a severity string, got number"),
            ),
            (
                rules_of(&[(&hostile, json!("Warn"))]),
                "rules",
                format!("rules[\"{shown}\"]: unknown severity \"Warn\"; {EXPECTED}"),
            ),
            (
                rules_of(&[("unused-variable", json!(hostile))]),
                "options.rules",
                format!(
                    "options.rules[\"unused-variable\"]: unknown severity \"{shown}\"; {EXPECTED}"
                ),
            ),
            // Clean controls: the whole message, nothing escaped, the field as given.
            (
                rules_of(&[("unused-variable", json!(null))]),
                "rules",
                "rules[\"unused-variable\"] must be a severity string, got null".to_owned(),
            ),
            (
                rules_of(&[("unused-variable", json!("verbose"))]),
                "options.rules",
                format!(
                    "options.rules[\"unused-variable\"]: unknown severity \"verbose\"; {EXPECTED}"
                ),
            ),
        ];
        for (rules, field, message) in cases {
            assert_eq!(parse_rule_severities(rules, field), Err(message));
        }
    }

    #[test]
    fn parse_rule_severities_lists_the_spellings_parse_severity_error_lists() {
        let message = parse_rule_severities(rules_of(&[("r", json!("x"))]), "rules").unwrap_err();
        let list = ParseSeverityError.to_string();
        let list = list
            .strip_prefix("unknown severity; ")
            .expect("ParseSeverityError's message");
        assert!(message.ends_with(&format!("; {list}")), "{message:?}");
    }

    #[test]
    fn parse_rule_severities_refuses_the_first_bad_rule_in_key_order() {
        // `b` is refused: `a` and `c` are valid, and `d`, also bad, comes after it.
        let rules = rules_of(&[
            ("d", json!("x")),
            ("c", json!("warn")),
            ("b", json!(true)),
            ("a", json!("off")),
        ]);
        assert_eq!(
            parse_rule_severities(rules, "rules"),
            Err("rules[\"b\"] must be a severity string, got boolean".to_owned())
        );
    }

    #[test]
    fn parse_rule_severities_leaves_only_tab_raw() {
        // A name and a value holding every forbidden path character: WIRE escapes all
        // but TAB.
        let text: String = (0..=0xFFFF_u32)
            .filter_map(char::from_u32)
            .filter(|&c| crate::is_forbidden_path_char(c))
            .collect();
        assert_eq!(text.chars().count(), 80, "non-vacuity: the whole class");
        for message in [
            parse_rule_severities(rules_of(&[(&text, json!(0))]), "rules").unwrap_err(),
            parse_rule_severities(rules_of(&[("r", json!(text.clone()))]), "rules").unwrap_err(),
        ] {
            let raw: Vec<char> = message
                .chars()
                .filter(|&c| crate::is_forbidden_path_char(c))
                .collect();
            assert!(
                raw.iter().all(|&c| c == '\t') && !raw.is_empty(),
                "{message:?}"
            );
        }
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
        assert!(invalid_type.source().is_none(), "InvalidType has no source");

        let mds_err = MdsError::json_error("depth exceeded");
        let conversion = VarsError::Conversion(mds_err);
        assert!(
            conversion.source().is_some(),
            "Conversion should have a source"
        );
    }

    // ── attach_lint_warnings ──────────────────────────────────────────────────

    /// D8: a present warning is injected as `lint_warnings: [string]`.
    ///
    /// PF-013 / ADR-009: both directions are tested — present warning inserts
    /// the field; absent warning leaves the object unchanged.
    #[test]
    fn attach_lint_warnings_injects_field_when_warning_present() {
        let mut json = json!({ "version": 1 });
        let obj = json.as_object_mut().expect("json! produces an object");
        attach_lint_warnings(obj, Some("unknown lint rule 'foo'; ignoring".into()));
        let arr = json["lint_warnings"]
            .as_array()
            .expect("lint_warnings must be an array");
        assert_eq!(arr.len(), 1, "exactly one element");
        assert_eq!(
            arr[0].as_str().unwrap(),
            "unknown lint rule 'foo'; ignoring"
        );
    }

    /// D8: no `lint_warnings` key is added when warning is absent (absent-when-empty semantics).
    #[test]
    fn attach_lint_warnings_leaves_object_unchanged_when_no_warning() {
        let mut json = json!({ "version": 1 });
        let obj = json.as_object_mut().expect("json! produces an object");
        attach_lint_warnings(obj, None);
        assert!(
            json.get("lint_warnings").is_none(),
            "lint_warnings must be absent when no warning; got: {json:?}"
        );
    }
}
