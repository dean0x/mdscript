use std::fmt;

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
            Value::Number(n) => *n != 0.0,
            Value::Array(a) => !a.is_empty(),
        }
    }

    /// Convert a serde_yaml::Value into our Value enum.
    pub fn from_yaml(yaml: serde_yaml::Value) -> Result<Value, String> {
        match yaml {
            serde_yaml::Value::Null => Ok(Value::Null),
            serde_yaml::Value::Bool(b) => Ok(Value::Boolean(b)),
            serde_yaml::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Number(i as f64))
                } else if let Some(f) = n.as_f64() {
                    Ok(Value::Number(f))
                } else {
                    Err(format!("unsupported number: {n:?}"))
                }
            }
            serde_yaml::Value::String(s) => Ok(Value::String(s)),
            serde_yaml::Value::Sequence(seq) => {
                let items: Result<Vec<Value>, String> =
                    seq.into_iter().map(Value::from_yaml).collect();
                Ok(Value::Array(items?))
            }
            serde_yaml::Value::Mapping(_) => {
                Err("object/map types are not supported in MDS v0.1".to_string())
            }
            serde_yaml::Value::Tagged(t) => Value::from_yaml(t.value),
        }
    }

    /// Convert a serde_json::Value into our Value enum.
    pub fn from_json(json: serde_json::Value) -> Result<Value, String> {
        match json {
            serde_json::Value::Null => Ok(Value::Null),
            serde_json::Value::Bool(b) => Ok(Value::Boolean(b)),
            serde_json::Value::Number(n) => {
                if let Some(f) = n.as_f64() {
                    Ok(Value::Number(f))
                } else {
                    Err(format!("unsupported number: {n:?}"))
                }
            }
            serde_json::Value::String(s) => Ok(Value::String(s)),
            serde_json::Value::Array(arr) => {
                let items: Result<Vec<Value>, String> =
                    arr.into_iter().map(Value::from_json).collect();
                Ok(Value::Array(items?))
            }
            serde_json::Value::Object(_) => {
                Err("object/map types are not supported in MDS v0.1".to_string())
            }
        }
    }

    /// Try to interpret this value as an array.
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Return a human-readable type name for error messages.
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

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) => write!(f, "{s}"),
            Value::Number(n) => {
                if *n == (*n as i64) as f64 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Value::Boolean(b) => write!(f, "{b}"),
            Value::Array(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
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
    fn display() {
        assert_eq!(Value::String("hello".into()).to_string(), "hello");
        assert_eq!(Value::Number(42.0).to_string(), "42");
        assert_eq!(Value::Number(3.14).to_string(), "3.14");
        assert_eq!(Value::Boolean(true).to_string(), "true");
        assert_eq!(Value::Null.to_string(), "");
    }
}
