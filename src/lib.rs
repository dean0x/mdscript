//! # MDS — Markdown Script Compiler
//!
//! Compile composable LLM prompt templates from `.mds` files to Markdown.
//!
//! ## Quick Start
//!
//! **In-memory compilation** — compile from a string with no files involved:
//!
//! ```rust
//! let output = mds::compile_str("---\nname: World\n---\nHello {name}!\n")?;
//! assert_eq!(output, "---\nname: World\n---\nHello World!\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! **File-based compilation** — compile a template file to Markdown:
//!
//! ```rust,no_run
//! use std::path::Path;
//! let md = mds::compile(Path::new("template.mds"), None)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! **With runtime variables** — load vars from JSON and pass them in:
//!
//! ```rust,no_run
//! use std::path::Path;
//! let vars = mds::load_vars_file(Path::new("vars.json"))?;
//! let md = mds::compile(Path::new("template.mds"), Some(vars))?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! **Validation only** — check a template without rendering output:
//!
//! ```rust,no_run
//! use std::path::Path;
//! mds::check(Path::new("template.mds"), None)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub(crate) mod ast;
pub mod error;
pub(crate) mod evaluator;
pub(crate) mod lexer;
pub(crate) mod limits;
pub(crate) mod parser;
pub(crate) mod resolver;
pub(crate) mod scope;
pub(crate) mod validator;
pub mod value;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use resolver::ModuleCache;

pub use error::MdsError;
pub use value::Value;

/// Maximum file size accepted for compilation (10 MB).
///
/// This is the single source of truth shared by the file resolver and the
/// stdin reader in the CLI binary.
pub const MAX_FILE_SIZE: u64 = resolver::MAX_FILE_SIZE;

/// Maximum directory traversal depth for upward directory walks.
///
/// Shared between `find_project_root` in the resolver and `load_config` in the
/// CLI binary — eliminating the duplicate definition.
pub const MAX_TRAVERSAL_DEPTH: usize = resolver::MAX_TRAVERSAL_DEPTH;

/// Compile an MDS file to a final Markdown string.
///
/// Warnings (e.g. empty `@include`) are printed to stderr. Pass `runtime_vars`
/// to override or supply variables that aren't defined in frontmatter.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let md = mds::compile(Path::new("template.mds"), None)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
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
/// # Examples
///
/// ```rust
/// let output = mds::compile_str("---\ngreeting: Hi\n---\n{greeting} there!\n")?;
/// assert_eq!(output, "---\ngreeting: Hi\n---\nHi there!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled Markdown output should be used"]
pub fn compile_str(source: &str) -> Result<String, MdsError> {
    compile_str_with(source, None, None)
}

/// Compile MDS source code from a string with options.
///
/// Warnings (e.g. empty `@include`) are printed to stderr. `base_dir` sets the
/// root for resolving `@import` paths; defaults to the current directory.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// use std::collections::HashMap;
///
/// let vars = HashMap::from([("lang".to_string(), mds::Value::String("Rust".to_string()))]);
/// let md = mds::compile_str_with(
///     "---\nlang: unknown\n---\nI love {lang}!\n",
///     Some(Path::new("templates/")),
///     Some(vars),
/// )?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
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
///
/// Returns `Ok(())` if the file is valid, or an error describing the problem.
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// mds::check(Path::new("template.mds"), None)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "errors should be handled"]
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
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Examples
///
/// ```rust
/// // Valid template — no error
/// mds::check_str("---\nname: Test\n---\nHello {name}!\n")?;
///
/// // Invalid template — undefined variable
/// assert!(mds::check_str("Hello {name}!\n").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "errors should be handled"]
pub fn check_str(source: &str) -> Result<(), MdsError> {
    check_str_with(source, None, None)
}

/// Resolve an optional base directory to a `PathBuf`, falling back to cwd.
fn resolve_base_dir(base_dir: Option<&Path>) -> Result<PathBuf, MdsError> {
    match base_dir {
        Some(d) => Ok(d.to_path_buf()),
        None => std::env::current_dir()
            .map_err(|e| MdsError::io(format!("cannot determine current directory: {e}"))),
    }
}

/// Check (validate) MDS source from a string with options.
///
/// `base_dir` sets the root for resolving `@import` paths; defaults to the
/// current directory.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// mds::check_str_with("---\nenv: dev\n---\nRunning in {env}.\n", Some(Path::new("templates/")), None)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "errors should be handled"]
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
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let (md, warnings) = mds::compile_collecting_warnings(Path::new("template.mds"), None)?;
/// for w in &warnings { eprintln!("warning: {w}"); }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
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
    let body = resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default();
    let output = prepend_frontmatter(resolved.raw_frontmatter.as_deref(), body);
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
    let body = resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default();
    let output = prepend_frontmatter(resolved.raw_frontmatter.as_deref(), body);
    Ok((output, warnings))
}

/// Check (validate) an MDS file and return any collected warnings without rendering output.
///
/// Unlike [`check`], this function does not print warnings to stderr. The caller
/// is responsible for deciding whether to display them (e.g. based on a quiet flag).
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let ((), warnings) = mds::check_collecting_warnings(Path::new("template.mds"), None)?;
/// for w in &warnings { eprintln!("warning: {w}"); }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "warnings should be used"]
pub fn check_collecting_warnings(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<((), Vec<String>), MdsError> {
    let path = path.as_ref();
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    cache.resolve(path, &vars, &mut warnings)?;
    Ok(((), warnings))
}

/// Check (validate) MDS source from a string and return any collected warnings without rendering output.
///
/// Unlike [`check_str_with`], this function does not print warnings to stderr. The caller
/// is responsible for deciding whether to display them (e.g. based on a quiet flag).
///
/// # Examples
///
/// ```rust
/// let ((), warnings) = mds::check_str_collecting_warnings(
///     "---\nname: Test\n---\nHello {name}!\n",
///     None,
///     None,
/// )?;
/// assert!(warnings.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "warnings should be used"]
pub fn check_str_collecting_warnings(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<((), Vec<String>), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let dir = resolve_base_dir(base_dir)?;
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    cache.resolve_source(source, &dir, &vars, &mut warnings)?;
    Ok(((), warnings))
}

/// Remove the `type: mds` line from raw frontmatter content.
///
/// Returns `Some(remaining)` if any non-whitespace content survives after filtering,
/// or `None` if the frontmatter would be empty (nothing worth emitting).
fn strip_type_mds(raw: &str) -> Option<String> {
    let mut filtered = String::with_capacity(raw.len());
    for line in raw.lines() {
        // Only strip the top-level (no leading whitespace) `type: mds` directive.
        // Using line.trim() here would incorrectly remove indented keys inside nested
        // YAML objects (e.g. `  type: mds` under a mapping), corrupting the output.
        //
        // All three YAML quoting styles for the value "mds" are stripped:
        //   type: mds     (plain scalar)
        //   type: "mds"   (double-quoted)
        //   type: 'mds'   (single-quoted)
        let is_type_mds = line.strip_prefix("type:").is_some_and(|v| {
            let v = v.trim();
            v == "mds" || v == "\"mds\"" || v == "'mds'"
        });
        if !is_type_mds {
            filtered.push_str(line);
            filtered.push('\n');
        }
    }
    if filtered.trim().is_empty() {
        None
    } else {
        Some(filtered)
    }
}

/// Prepend YAML frontmatter fences to a compiled body.
///
/// If `raw` is `None`, or after stripping `type: mds` the frontmatter is empty,
/// the body is returned unchanged.
fn prepend_frontmatter(raw: Option<&str>, body: String) -> String {
    let Some(raw) = raw else {
        return body;
    };
    let Some(cleaned) = strip_type_mds(raw) else {
        return body;
    };
    format!("---\n{cleaned}---\n{body}")
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
///
/// # Examples
///
/// ```rust,no_run
/// let md = mds::compile_file("template.mds")?;
/// println!("{md}");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled Markdown output should be used"]
pub fn compile_file(path: &str) -> Result<String, MdsError> {
    compile(Path::new(path), None)
}

/// Load runtime variables from a JSON file.
///
/// The file must contain a JSON object; each key becomes a variable name.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
///
/// let vars = mds::load_vars_file(Path::new("vars.json"))?;
/// let md = mds::compile(Path::new("template.mds"), Some(vars))?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the loaded variables should be used"]
pub fn load_vars_file(path: &Path) -> Result<HashMap<String, Value>, MdsError> {
    // Read bytes first, then check size (same TOCTOU-safe pattern as resolver.rs).
    let bytes = std::fs::read(path)
        .map_err(|e| MdsError::io(format!("cannot read vars file {}: {e}", path.display())))?;
    if bytes.len() as u64 > resolver::MAX_FILE_SIZE {
        return Err(MdsError::resource_limit(format!(
            "vars file exceeds maximum size of {} bytes: {}",
            resolver::MAX_FILE_SIZE,
            path.display()
        )));
    }
    let content = String::from_utf8(bytes).map_err(|e| {
        MdsError::io(format!("invalid UTF-8 in vars file {}: {e}", path.display()))
    })?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| MdsError::json_error(e.to_string()))?;

    let serde_json::Value::Object(map) = json else {
        return Err(MdsError::json_error("vars file must contain a JSON object"));
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

    #[test]
    fn clean_output_strips_carriage_returns() {
        assert_eq!(clean_output("hello\r\nworld\r\n"), "hello\nworld\n");
    }

    // ── strip_type_mds: YAML quoting variants ─────────────────────────────────

    #[test]
    fn strip_type_mds_plain_value() {
        // Baseline: unquoted `type: mds` is stripped.
        let raw = "type: mds\nname: Alice\n";
        let result = strip_type_mds(raw);
        assert_eq!(result, Some("name: Alice\n".to_string()));
    }

    #[test]
    fn strip_type_mds_double_quoted() {
        // `type: "mds"` — double-quoted YAML string — must also be stripped.
        let raw = "type: \"mds\"\nname: Alice\n";
        let result = strip_type_mds(raw);
        assert_eq!(
            result,
            Some("name: Alice\n".to_string()),
            "double-quoted type:mds should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_type_mds_single_quoted() {
        // `type: 'mds'` — single-quoted YAML string — must also be stripped.
        let raw = "type: 'mds'\nname: Alice\n";
        let result = strip_type_mds(raw);
        assert_eq!(
            result,
            Some("name: Alice\n".to_string()),
            "single-quoted type:mds should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_type_mds_no_space_after_colon() {
        // `type:mds` — no space after colon — must also be stripped.
        let raw = "type:mds\nname: Alice\n";
        let result = strip_type_mds(raw);
        assert_eq!(
            result,
            Some("name: Alice\n".to_string()),
            "no-space type:mds should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_type_mds_quoted_only_returns_none() {
        // Frontmatter with only a quoted `type: "mds"` should return None (empty after strip).
        let raw = "type: \"mds\"\n";
        let result = strip_type_mds(raw);
        assert_eq!(
            result, None,
            "frontmatter with only quoted type:mds should be None, got: {result:?}"
        );
    }

    #[test]
    fn strip_type_mds_indented_quoted_not_stripped() {
        // Indented `  type: "mds"` inside a nested mapping must NOT be stripped.
        let raw = "type: mds\nconfig:\n  type: \"mds\"\n  theme: dark\n";
        let result = strip_type_mds(raw);
        assert_eq!(
            result,
            Some("config:\n  type: \"mds\"\n  theme: dark\n".to_string()),
            "indented quoted type:mds should be preserved, got: {result:?}"
        );
    }
}
