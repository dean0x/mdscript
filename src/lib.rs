pub mod ast;
pub mod error;
pub mod evaluator;
pub mod lexer;
pub mod parser;
pub mod resolver;
pub mod scope;
pub mod validator;
pub mod value;

use std::collections::HashMap;
use std::path::Path;

use error::MdsError;
use resolver::ModuleCache;
use value::Value;

/// Compile an MDS file to a final Markdown string.
///
/// # Arguments
/// * `path` — Path to the .mds file
/// * `runtime_vars` — Optional runtime variable overrides (from --vars JSON)
pub fn compile(path: &Path, runtime_vars: Option<HashMap<String, Value>>) -> Result<String, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let resolved = cache.resolve(path, &vars)?;

    match resolved.prompt_body {
        Some(body) => Ok(clean_output(&body)),
        None => Ok(String::new()),
    }
}

/// Check (validate) an MDS file without rendering output.
/// Returns Ok(()) if the file is valid, or an error describing the problem.
pub fn check(path: &Path, runtime_vars: Option<HashMap<String, Value>>) -> Result<(), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let _resolved = cache.resolve(path, &vars)?;
    Ok(())
}

/// Clean up output whitespace: collapse 3+ consecutive newlines to 2 (one blank line),
/// and trim leading/trailing blank lines.
fn clean_output(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut newline_count = 0;

    // Trim leading whitespace/newlines
    let s = s.trim_start_matches('\n').trim_start_matches("\r\n");

    for ch in s.chars() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count <= 2 {
                result.push(ch);
            }
        } else if ch == '\r' {
            // Skip \r, handled with \n
        } else {
            newline_count = 0;
            result.push(ch);
        }
    }

    // Trim trailing whitespace but keep one final newline
    let trimmed = result.trim_end();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = trimmed.to_string();
    out.push('\n');
    out
}

/// Load runtime variables from a JSON file.
pub fn load_vars_file(path: &Path) -> Result<HashMap<String, Value>, MdsError> {
    let content = std::fs::read_to_string(path).map_err(|e| MdsError::Io {
        message: format!("cannot read vars file {}: {e}", path.display()),
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| MdsError::JsonError {
            message: e.to_string(),
        })?;

    let mut vars = HashMap::new();
    if let serde_json::Value::Object(map) = json {
        for (key, val) in map {
            let value = Value::from_json(val).map_err(|e| MdsError::JsonError { message: e })?;
            vars.insert(key, value);
        }
    } else {
        return Err(MdsError::JsonError {
            message: "vars file must contain a JSON object".to_string(),
        });
    }

    Ok(vars)
}
