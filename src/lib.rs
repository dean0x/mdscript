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
use std::path::{Path, PathBuf};

use error::MdsError;
use resolver::ModuleCache;

/// Compile an MDS file to a final Markdown string.
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Arguments
/// * `path` — Path to the .mds file
/// * `runtime_vars` — Optional runtime variable overrides (from --vars JSON)
#[must_use = "the compiled Markdown output should be used"]
pub fn compile(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    let (output, warnings) = compile_collecting_warnings(path, runtime_vars)?;
    emit_warnings(&warnings);
    Ok(output)
}

/// Compile MDS source code from a string.
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Arguments
/// * `source` — MDS source code
#[must_use = "the compiled Markdown output should be used"]
pub fn compile_str(source: &str) -> Result<String, MdsError> {
    compile_str_with(source, None, None)
}

/// Compile MDS source code from a string with options.
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Arguments
/// * `source` — MDS source code
/// * `base_dir` — Base directory for resolving imports (defaults to current dir)
/// * `runtime_vars` — Optional runtime variable overrides
#[must_use = "the compiled Markdown output should be used"]
pub fn compile_str_with(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    let (output, warnings) = compile_str_collecting_warnings(source, base_dir, runtime_vars)?;
    emit_warnings(&warnings);
    Ok(output)
}

/// Check (validate) an MDS file without rendering output.
/// Returns Ok(()) if the file is valid, or an error describing the problem.
#[must_use = "the validation result should be checked"]
pub fn check(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(), MdsError> {
    let path = path.as_ref();
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    cache.resolve(path, &vars, &mut warnings)?;
    emit_warnings(&warnings);
    Ok(())
}

/// Check (validate) MDS source from a string without rendering output.
#[must_use = "the validation result should be checked"]
pub fn check_str(source: &str) -> Result<(), MdsError> {
    check_str_with(source, None, None)
}

/// Resolve an optional base directory to a `PathBuf`, falling back to cwd.
fn resolve_base_dir(base_dir: Option<&Path>) -> Result<PathBuf, MdsError> {
    match base_dir {
        Some(d) => Ok(d.to_path_buf()),
        None => std::env::current_dir().map_err(|e| MdsError::Io {
            message: format!("cannot determine current directory: {e}"),
        }),
    }
}

/// Check (validate) MDS source from a string with options.
#[must_use = "the validation result should be checked"]
pub fn check_str_with(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let dir = resolve_base_dir(base_dir)?;
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    cache.resolve_source(source, &dir, &vars, &mut warnings)?;
    emit_warnings(&warnings);
    Ok(())
}

/// Print warnings to stderr. Each warning is printed on its own line.
fn emit_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("{w}");
    }
}

/// Compile an MDS file and return the output along with any collected warnings.
///
/// Unlike [`compile`], this function does not print warnings to stderr. The caller
/// is responsible for deciding whether to display them (e.g. based on a quiet flag).
#[must_use = "the compiled Markdown output and warnings should be used"]
pub fn compile_collecting_warnings(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(String, Vec<String>), MdsError> {
    let path = path.as_ref();
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let resolved = cache.resolve(path, &vars, &mut warnings)?;
    let output = resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default();
    Ok((output, warnings))
}

/// Compile MDS source from a string and return the output along with any collected warnings.
///
/// Unlike [`compile_str_with`], this function does not print warnings to stderr. The caller
/// is responsible for deciding whether to display them (e.g. based on a quiet flag).
#[must_use = "the compiled Markdown output and warnings should be used"]
pub fn compile_str_collecting_warnings(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(String, Vec<String>), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let dir = resolve_base_dir(base_dir)?;
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let resolved = cache.resolve_source(source, &dir, &vars, &mut warnings)?;
    let output = resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default();
    Ok((output, warnings))
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

/// Convenience wrapper around [`compile`] for callers who have a path as `&str`.
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
#[must_use = "the compiled Markdown output should be used"]
pub fn compile_file(path: &str) -> Result<String, MdsError> {
    compile(Path::new(path), None)
}

/// Load runtime variables from a JSON file.
#[must_use = "the loaded variables should be used"]
pub fn load_vars_file(path: &Path) -> Result<HashMap<String, Value>, MdsError> {
    let content = std::fs::read_to_string(path).map_err(|e| MdsError::Io {
        message: format!("cannot read vars file {}: {e}", path.display()),
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| MdsError::JsonError {
            message: e.to_string(),
        })?;

    let serde_json::Value::Object(map) = json else {
        return Err(MdsError::JsonError {
            message: "vars file must contain a JSON object".to_string(),
        });
    };

    map.into_iter()
        .map(|(key, val)| Value::from_json(val).map(|v| (key, v)))
        .collect()
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
