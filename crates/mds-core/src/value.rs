use std::collections::HashMap;
use std::fmt;

use crate::error::MdsError;

/// Maximum nesting depth for YAML and JSON value trees.
const MAX_VALUE_DEPTH: usize = 64;

/// Return a human-readable type name for a YAML value, used in error diagnostics.
fn yaml_type_name(v: &serde_yml::Value) -> &'static str {
    match v {
        serde_yml::Value::Null => "null",
        serde_yml::Value::Bool(_) => "boolean",
        serde_yml::Value::Number(_) => "integer/float",
        serde_yml::Value::String(_) => "string",
        serde_yml::Value::Sequence(_) => "sequence",
        serde_yml::Value::Mapping(_) => "mapping",
        serde_yml::Value::Tagged(_) => "tagged",
    }
}

/// Runtime value type for MDS variables and expressions.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Number(f64),
    Boolean(bool),
    Array(Vec<Value>),
    Object(HashMap<String, Value>),
    Null,
}

impl Value {
    /// MDS truthiness rules:
    /// Falsy: false, null, "", [], {}, 0
    /// Everything else is truthy.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Boolean(b) => *b,
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(m) => !m.is_empty(),
        }
    }

    /// Convert a serde_yml::Value into our Value enum.
    pub(crate) fn from_yaml(yaml: serde_yml::Value) -> Result<Value, MdsError> {
        Self::from_yaml_inner(yaml, 0)
    }

    fn from_yaml_inner(yaml: serde_yml::Value, depth: usize) -> Result<Value, MdsError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(MdsError::yaml_error(format!(
                "value nesting exceeds maximum depth of {MAX_VALUE_DEPTH}"
            )));
        }
        match yaml {
            serde_yml::Value::Null => Ok(Value::Null),
            serde_yml::Value::Bool(b) => Ok(Value::Boolean(b)),
            serde_yml::Value::Number(n) => n
                .as_i64()
                .map(|i| i as f64)
                .or_else(|| n.as_f64())
                .map(Value::Number)
                .ok_or_else(|| MdsError::yaml_error(format!("unsupported number: {n:?}"))),
            serde_yml::Value::String(s) => Ok(Value::String(s)),
            serde_yml::Value::Sequence(seq) => seq
                .into_iter()
                .map(|v| Self::from_yaml_inner(v, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            serde_yml::Value::Mapping(mapping) => {
                let mut map = HashMap::new();
                for (k, v) in mapping {
                    // MDS only supports string keys in objects. Reject non-string keys
                    // with a clear diagnostic rather than silently discarding the entry,
                    // which would leave the user with confusing 'field not found' errors.
                    let key = match k {
                        serde_yml::Value::String(s) => s,
                        other => {
                            return Err(MdsError::yaml_error(format!(
                                "MDS only supports string keys in objects; found {} key — use a quoted string key instead",
                                yaml_type_name(&other)
                            )));
                        }
                    };
                    let value = Self::from_yaml_inner(v, depth + 1)?;
                    map.insert(key, value);
                }
                Ok(Value::Object(map))
            }
            serde_yml::Value::Tagged(t) => Self::from_yaml_inner(t.value, depth + 1),
        }
    }

    /// Convert a serde_json::Value into our Value enum.
    pub(crate) fn from_json(json: serde_json::Value) -> Result<Value, MdsError> {
        Self::from_json_inner(json, 0)
    }

    fn from_json_inner(json: serde_json::Value, depth: usize) -> Result<Value, MdsError> {
        if depth > MAX_VALUE_DEPTH {
            return Err(MdsError::json_error(format!(
                "value nesting exceeds maximum depth of {MAX_VALUE_DEPTH}"
            )));
        }
        match json {
            serde_json::Value::Null => Ok(Value::Null),
            serde_json::Value::Bool(b) => Ok(Value::Boolean(b)),
            serde_json::Value::Number(n) => n
                .as_f64()
                .map(Value::Number)
                .ok_or_else(|| MdsError::json_error(format!("unsupported number: {n:?}"))),
            serde_json::Value::String(s) => Ok(Value::String(s)),
            serde_json::Value::Array(arr) => arr
                .into_iter()
                .map(|v| Self::from_json_inner(v, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            serde_json::Value::Object(obj) => {
                let mut map = HashMap::new();
                for (key, val) in obj {
                    let value = Self::from_json_inner(val, depth + 1)?;
                    map.insert(key, value);
                }
                Ok(Value::Object(map))
            }
        }
    }

    /// Try to interpret this value as an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Return a human-readable type name for error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Number(_) => "number",
            Value::Boolean(_) => "boolean",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Null => "null",
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n as f64)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Number(n as f64)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Boolean(b)
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(v: Vec<T>) -> Self {
        Value::Array(v.into_iter().map(Into::into).collect())
    }
}

impl From<HashMap<String, Value>> for Value {
    fn from(m: HashMap<String, Value>) -> Self {
        Value::Object(m)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) => write!(f, "{s}"),
            Value::Number(n) => {
                // Display whole numbers without decimal point, but guard
                // against values outside the i64 range to avoid overflow.
                if n.fract() == 0.0
                    && n.is_finite()
                    && *n >= i64::MIN as f64
                    && *n <= i64::MAX as f64
                {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Value::Boolean(b) => write!(f, "{b}"),
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                Ok(())
            }
            Value::Object(map) => {
                // Sort keys alphabetically for deterministic output
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{key}: {}", map[*key])?;
                }
                Ok(())
            }
            Value::Null => write!(f, ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness() {
        assert!(Value::Boolean(true).is_truthy());
        assert!(!Value::Boolean(false).is_truthy());
        assert!(!Value::Null.is_truthy());
        assert!(!Value::String(String::new()).is_truthy());
        assert!(Value::String("hello".into()).is_truthy());
        assert!(!Value::Number(0.0).is_truthy());
        assert!(Value::Number(1.0).is_truthy());
        assert!(!Value::Array(vec![]).is_truthy());
        assert!(Value::Array(vec![Value::Number(1.0)]).is_truthy());
    }

    #[test]
    fn nan_is_falsy() {
        assert!(!Value::Number(f64::NAN).is_truthy(), "NaN must be falsy");
    }

    #[test]
    fn from_impls() {
        assert_eq!(Value::from("hello"), Value::String("hello".to_owned()));
        assert_eq!(
            Value::from("hello".to_owned()),
            Value::String("hello".to_owned())
        );
        assert_eq!(Value::from(2.5_f64), Value::Number(2.5));
        assert_eq!(Value::from(42_i64), Value::Number(42.0));
        assert_eq!(Value::from(7_i32), Value::Number(7.0));
        assert_eq!(Value::from(true), Value::Boolean(true));
        assert_eq!(Value::from(false), Value::Boolean(false));
        let v: Value = vec![1_i32, 2, 3].into();
        assert_eq!(
            v,
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
    }

    #[test]
    fn display() {
        assert_eq!(Value::String("hello".into()).to_string(), "hello");
        assert_eq!(Value::Number(42.0).to_string(), "42");
        assert_eq!(Value::Number(2.5).to_string(), "2.5");
        assert_eq!(Value::Boolean(true).to_string(), "true");
        assert_eq!(Value::Null.to_string(), "");
    }

    #[test]
    fn display_large_number() {
        // Numbers beyond i64 range fall through to f64 Display.
        assert_eq!(Value::Number(1e20).to_string(), "100000000000000000000");

        // NaN and infinity use f64 Display (no decimal formatting shortcut applies).
        assert_eq!(Value::Number(f64::NAN).to_string(), "NaN");
        assert_eq!(Value::Number(f64::INFINITY).to_string(), "inf");
        assert_eq!(Value::Number(f64::NEG_INFINITY).to_string(), "-inf");
    }

    // ── Security: YAML value depth limit ─────────────────────────────────────

    #[test]
    fn yaml_value_depth_limit_rejects_deeply_nested_sequence() {
        use serde_yml::Value as YamlValue;

        // Build a YAML sequence nested 65 levels deep (just past the limit of 64).
        let mut nested = YamlValue::Null;
        for _ in 0..65 {
            nested = YamlValue::Sequence(vec![nested]);
        }

        let result = Value::from_yaml(nested);
        assert!(
            result.is_err(),
            "YAML value nested 65 levels deep must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("nesting") || err.contains("depth") || err.contains("64"),
            "error should mention depth limit, got: {err}"
        );
    }

    // ── Object/Map type tests ─────────────────────────────────────────────────

    #[test]
    fn object_truthiness_empty() {
        assert!(
            !Value::Object(HashMap::new()).is_truthy(),
            "empty object should be falsy"
        );
    }

    #[test]
    fn object_truthiness_non_empty() {
        let mut m = HashMap::new();
        m.insert("key".to_string(), Value::String("val".to_string()));
        assert!(
            Value::Object(m).is_truthy(),
            "non-empty object should be truthy"
        );
    }

    #[test]
    fn from_yaml_mapping() {
        use serde_yml::Value as YamlValue;
        let mut mapping = serde_yml::Mapping::new();
        mapping.insert(
            YamlValue::String("key".to_string()),
            YamlValue::String("val".to_string()),
        );
        let yaml = YamlValue::Mapping(mapping);
        let result = Value::from_yaml(yaml).unwrap();
        if let Value::Object(map) = result {
            assert_eq!(map.get("key"), Some(&Value::String("val".to_string())));
        } else {
            panic!("expected Value::Object");
        }
    }

    /// Assert that `from_yaml` rejects a mapping whose sole key is `key_value`.
    fn assert_non_string_yaml_key_rejected(key_value: serde_yml::Value) {
        let mut mapping = serde_yml::Mapping::new();
        mapping.insert(key_value, serde_yml::Value::String("value".to_string()));
        let result = Value::from_yaml(serde_yml::Value::Mapping(mapping));
        assert!(result.is_err(), "non-string YAML key must return an error");
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("string") || err.contains("key"),
            "error should mention string keys, got: {err}"
        );
    }

    #[test]
    fn from_yaml_non_string_key_integer_returns_error() {
        assert_non_string_yaml_key_rejected(serde_yml::Value::Number(42.into()));
    }

    #[test]
    fn from_yaml_non_string_key_boolean_returns_error() {
        assert_non_string_yaml_key_rejected(serde_yml::Value::Bool(true));
    }

    #[test]
    fn from_yaml_non_string_key_null_returns_error() {
        assert_non_string_yaml_key_rejected(serde_yml::Value::Null);
    }

    #[test]
    fn from_json_object() {
        let json = serde_json::json!({"key": "val"});
        let result = Value::from_json(json).unwrap();
        if let Value::Object(map) = result {
            assert_eq!(map.get("key"), Some(&Value::String("val".to_string())));
        } else {
            panic!("expected Value::Object");
        }
    }

    #[test]
    fn yaml_nested_object_depth_limit() {
        use serde_yml::Value as YamlValue;

        // Build a YAML mapping nested 65 levels deep (just past the limit of 64).
        let mut nested = YamlValue::Null;
        for _ in 0..65 {
            let mut mapping = serde_yml::Mapping::new();
            mapping.insert(YamlValue::String("child".to_string()), nested);
            nested = YamlValue::Mapping(mapping);
        }

        let result = Value::from_yaml(nested);
        assert!(
            result.is_err(),
            "YAML object nested 65 levels deep must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("nesting") || err.contains("depth") || err.contains("64"),
            "error should mention depth limit, got: {err}"
        );
    }

    #[test]
    fn json_nested_object_depth_limit() {
        // Build a JSON object nested 65 levels deep (just past the limit of 64).
        let mut nested = serde_json::Value::Null;
        for _ in 0..65 {
            let mut obj = serde_json::Map::new();
            obj.insert("child".to_string(), nested);
            nested = serde_json::Value::Object(obj);
        }

        let result = Value::from_json(nested);
        assert!(
            result.is_err(),
            "JSON object nested 65 levels deep must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("nesting") || err.contains("depth") || err.contains("64"),
            "error should mention depth limit, got: {err}"
        );
    }

    #[test]
    fn object_display() {
        let mut m = HashMap::new();
        m.insert("key1".to_string(), Value::String("val1".to_string()));
        m.insert("key2".to_string(), Value::String("val2".to_string()));
        let s = Value::Object(m).to_string();
        assert_eq!(
            s, "key1: val1, key2: val2",
            "object display should be sorted key: val pairs"
        );
    }

    #[test]
    fn object_type_name() {
        let obj = Value::Object(HashMap::new());
        assert_eq!(obj.type_name(), "object");
    }

    #[test]
    fn from_hashmap() {
        let mut m = HashMap::new();
        m.insert("a".to_string(), Value::Number(1.0));
        let v: Value = m.clone().into();
        assert_eq!(v, Value::Object(m));
    }

    // ── Security: JSON value depth limit ─────────────────────────────────────

    #[test]
    fn json_value_depth_limit_rejects_deeply_nested_array() {
        // Build a JSON array nested 65 levels deep (just past the limit of 64).
        let mut nested = serde_json::Value::Null;
        for _ in 0..65 {
            nested = serde_json::Value::Array(vec![nested]);
        }

        let result = Value::from_json(nested);
        assert!(
            result.is_err(),
            "JSON value nested 65 levels deep must be rejected"
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("nesting") || err.contains("depth") || err.contains("64"),
            "error should mention depth limit, got: {err}"
        );
    }
}
