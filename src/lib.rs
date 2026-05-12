//! # MDS — Markdown Script Compiler
//!
//! Compile composable LLM prompt templates from `.mds` files to Markdown.
//!
//! ```rust
//! // Compile from a string
//! let output = mds::compile_str("---\nname: World\n---\nHello {name}!\n").unwrap();
//! assert_eq!(output, "Hello World!\n");
//! ```

pub(crate) mod ast;
pub mod error;
pub(crate) mod evaluator;
pub(crate) mod lexer;
pub(crate) mod parser;
pub(crate) mod resolver;
pub(crate) mod scope;
pub(crate) mod validator;
pub mod value;

pub use value::Value;

use std::collections::HashMap;
use std::path::Path;

use error::MdsError;
use resolver::ModuleCache;

/// Compile an MDS file to a final Markdown string.
///
/// # Arguments
/// * `path` — Path to the .mds file
/// * `runtime_vars` — Optional runtime variable overrides (from --vars JSON)
pub fn compile(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    let path = path.as_ref();
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let resolved = cache.resolve(path, &vars)?;
    Ok(resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default())
}

/// Compile MDS source code from a string.
///
/// # Arguments
/// * `source` — MDS source code
pub fn compile_str(source: &str) -> Result<String, MdsError> {
    compile_str_with(source, None, None)
}

/// Compile MDS source code from a string with options.
///
/// # Arguments
/// * `source` — MDS source code
/// * `base_dir` — Base directory for resolving imports (defaults to current dir)
/// * `runtime_vars` — Optional runtime variable overrides
pub fn compile_str_with(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let cwd_buf;
    let dir = match base_dir {
        Some(d) => d,
        None => {
            cwd_buf = std::env::current_dir().map_err(|e| MdsError::Io {
                message: format!("cannot determine current directory: {e}"),
            })?;
            cwd_buf.as_path()
        }
    };
    let mut cache = ModuleCache::new();
    let resolved = cache.resolve_source(source, dir, &vars)?;
    Ok(resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default())
}

/// Check (validate) an MDS file without rendering output.
/// Returns Ok(()) if the file is valid, or an error describing the problem.
pub fn check(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(), MdsError> {
    let path = path.as_ref();
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    cache.resolve(path, &vars)?;
    Ok(())
}

/// Check (validate) MDS source from a string without rendering output.
pub fn check_str(source: &str) -> Result<(), MdsError> {
    check_str_with(source, None, None)
}

/// Check (validate) MDS source from a string with options.
pub fn check_str_with(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let cwd_buf;
    let dir = match base_dir {
        Some(d) => d,
        None => {
            cwd_buf = std::env::current_dir().map_err(|e| MdsError::Io {
                message: format!("cannot determine current directory: {e}"),
            })?;
            cwd_buf.as_path()
        }
    };
    let mut cache = ModuleCache::new();
    cache.resolve_source(source, dir, &vars)?;
    Ok(())
}

/// Clean up output whitespace: collapse 3+ consecutive newlines to 2 (one blank line),
/// and trim leading/trailing blank lines.
fn clean_output(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut newline_count = 0;

    // Trim leading newlines (any line ending style)
    let s = s.trim_start_matches(['\n', '\r']);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_output_collapses_excess_newlines() {
        assert_eq!(clean_output("a\n\n\n\nb"), "a\n\nb\n");
    }

    #[test]
    fn clean_output_trims_leading_newlines() {
        assert_eq!(clean_output("\n\n\nhello"), "hello\n");
    }

    #[test]
    fn clean_output_trims_trailing_whitespace() {
        assert_eq!(clean_output("hello\n\n  \n"), "hello\n");
    }

    #[test]
    fn clean_output_empty_input() {
        assert_eq!(clean_output(""), "");
        assert_eq!(clean_output("\n\n\n"), "");
    }

    #[test]
    fn clean_output_preserves_single_blank_line() {
        assert_eq!(clean_output("a\n\nb"), "a\n\nb\n");
    }
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
            let value = Value::from_json(val)?;
            vars.insert(key, value);
        }
    } else {
        return Err(MdsError::JsonError {
            message: "vars file must contain a JSON object".to_string(),
        });
    }

    Ok(vars)
}
