use std::fmt;

use crate::error::MdsError;

/// Runtime value type for MDS variables and expressions.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Number(f64),
    Boolean(bool),
    Array(Vec<Value>),
    Null,
}

impl Value {
    /// MDS truthiness rules:
    /// Falsy: false, null, "", [], 0
    /// Everything else is truthy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Boolean(b) => *b,
            Value::Null => false,
            Value::String(s) => !s.is_empty(),
            Value::Number(n) => *n != 0.0 && !n.is_nan(),
            Value::Array(a) => !a.is_empty(),
        }
    }

    /// Convert a serde_yaml::Value into our Value enum.
    pub fn from_yaml(yaml: serde_yaml::Value) -> Result<Value, MdsError> {
        match yaml {
            serde_yaml::Value::Null => Ok(Value::Null),
            serde_yaml::Value::Bool(b) => Ok(Value::Boolean(b)),
            serde_yaml::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Number(i as f64))
                } else if let Some(f) = n.as_f64() {
                    Ok(Value::Number(f))
                } else {
                    Err(MdsError::YamlError {
                        message: format!("unsupported number: {n:?}"),
                    })
                }
            }
            serde_yaml::Value::String(s) => Ok(Value::String(s)),
            serde_yaml::Value::Sequence(seq) => {
                let items = seq
                    .into_iter()
                    .map(Value::from_yaml)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::Array(items))
            }
            serde_yaml::Value::Mapping(_) => Err(MdsError::YamlError {
                message: "object/map types are not supported in MDS v0.1".to_string(),
            }),
            serde_yaml::Value::Tagged(t) => Value::from_yaml(t.value),
        }
    }

    /// Convert a serde_json::Value into our Value enum.
    pub fn from_json(json: serde_json::Value) -> Result<Value, MdsError> {
        match json {
            serde_json::Value::Null => Ok(Value::Null),
            serde_json::Value::Bool(b) => Ok(Value::Boolean(b)),
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    Ok(Value::Number(f))
                } else {
                    Err(MdsError::JsonError {
                        message: format!("unsupported number: {n:?}"),
                    })
                }
            }
            serde_json::Value::String(s) => Ok(Value::String(s)),
            serde_json::Value::Array(arr) => {
                let items = arr
                    .into_iter()
                    .map(Value::from_json)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::Array(items))
            }
            serde_json::Value::Object(_) => Err(MdsError::JsonError {
                message: "object/map types are not supported in MDS v0.1".to_string(),
            }),
        }
    }

    /// Try to interpret this value as an array.
    #[must_use]
    pub fn as_array(&self) -> Option<&Vec<Value>> {
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
                let parts: Vec<String> = items.iter().map(ToString::to_string).collect();
                write!(f, "{}", parts.join(", "))
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
        // Numbers beyond i64 range should not panic during display
        let large = Value::Number(1e20);
        let result = large.to_string();
        assert!(!result.is_empty());

        let huge = Value::Number(f64::MAX);
        let result = huge.to_string();
        assert!(!result.is_empty());

        // NaN and infinity should display without panic
        let nan = Value::Number(f64::NAN);
        let result = nan.to_string();
        assert!(!result.is_empty());

        let inf = Value::Number(f64::INFINITY);
        let result = inf.to_string();
        assert!(!result.is_empty());
    }
}
