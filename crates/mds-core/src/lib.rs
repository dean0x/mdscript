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
pub(crate) mod error;
pub(crate) mod evaluator;
pub(crate) mod fs;
pub(crate) mod lexer;
pub(crate) mod limits;
pub(crate) mod options;
pub(crate) mod parser;
pub(crate) mod resolver;
pub(crate) mod scope;
pub(crate) mod validator;
pub(crate) mod value;

pub use fs::{FileSystem, NativeFs, VirtualFs};
pub use options::{
    format_unknown_keys_error, json_type_name, parse_json_vars, reject_unknown_json_keys, VarsError,
};
pub use resolver::ModuleCache;

use std::collections::HashMap;
use std::path::Path;

pub use error::{MdsError, SerializedError, SerializedSpan};
pub use value::Value;

/// The result of compiling an MDS template with full dependency tracking.
///
/// Returned by `compile_with_deps`, `compile_str_with_deps`, and
/// `compile_virtual_with_deps`. The `dependencies` list is in depth-first
/// resolution order and excludes the entry module itself — it contains only
/// the files imported (transitively) by the entry.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CompileOutput {
    /// The rendered Markdown output.
    pub output: String,
    /// Warnings emitted during compilation (e.g. empty `@include`).
    pub warnings: Vec<String>,
    /// Normalized keys of all modules imported during compilation, in
    /// first-resolution (depth-first) order. Excludes the entry module.
    pub dependencies: Vec<String>,
}

/// Maximum file size accepted for compilation (10 MB).
///
/// This is the single source of truth shared by the file resolver and the
/// stdin reader in the CLI binary.
pub const MAX_FILE_SIZE: u64 = limits::MAX_FILE_SIZE;

/// Maximum directory traversal depth for upward directory walks.
///
/// Shared between `find_project_root` in the resolver and `load_config` in the
/// CLI binary — eliminating the duplicate definition.
pub const MAX_TRAVERSAL_DEPTH: usize = limits::MAX_TRAVERSAL_DEPTH;

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
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    cache.resolve_path(path_str, &vars, &mut warnings)?;
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

/// Resolve an optional base directory to a `String`, falling back to cwd.
///
/// Fails with an explicit error when the path contains non-UTF-8 bytes rather
/// than silently corrupting the string via `display()`.
///
/// This is one of two UTF-8 boundary enforcement points; the other is
/// [`path_to_str`], which handles the entry-point `path` argument.
fn resolve_base_dir(base_dir: Option<&Path>) -> Result<String, MdsError> {
    match base_dir {
        Some(d) => d
            .to_str()
            .ok_or_else(|| MdsError::io("base_dir path is not valid UTF-8"))
            .map(str::to_owned),
        None => std::env::current_dir()
            .map_err(|e| MdsError::io(format!("cannot determine current directory: {e}")))
            .and_then(|p| {
                p.to_str()
                    .ok_or_else(|| MdsError::io("current directory path is not valid UTF-8"))
                    .map(str::to_owned)
            }),
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

/// Convert a `Path` to `&str`, returning an explicit error for non-UTF-8 paths.
///
/// Used at the public API boundary before passing the path string to the resolver,
/// which expects `&str` rather than `&Path`. This is one of two UTF-8 boundary
/// enforcement points; the other is [`resolve_base_dir`], which handles the
/// optional `base_dir` argument.
fn path_to_str(path: &Path) -> Result<&str, MdsError> {
    path.to_str()
        .ok_or_else(|| MdsError::io("path is not valid UTF-8"))
}

/// Print warnings to stderr. Each warning is printed on its own line.
fn emit_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("{w}");
    }
}

/// Build the final output string from a resolved module.
///
/// Cleans the prompt body and prepends YAML frontmatter when present.
fn build_output(resolved: &resolver::ResolvedModule) -> String {
    let body = resolved
        .prompt_body
        .as_deref()
        .map(clean_output)
        .unwrap_or_default();
    prepend_frontmatter(resolved.raw_frontmatter.as_deref(), body)
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
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let resolved = cache.resolve_path(path_str, &vars, &mut warnings)?;
    Ok((build_output(&resolved), warnings))
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
    Ok((build_output(&resolved), warnings))
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
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    cache.resolve_path(path_str, &vars, &mut warnings)?;
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

/// Compile a module from an in-memory virtual filesystem.
///
/// `modules` is a map of key → source content. `entry` is the key of the
/// module to compile (e.g. `"main.mds"`).
///
/// This is the virtual-filesystem counterpart of [`compile`], suitable for
/// WASM environments and testing where OS filesystem access is unavailable.
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
///
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "---\nname: World\n---\nHello {name}!\n".to_string());
///
/// let output = mds::compile_virtual(modules, "main.mds", None)?;
/// assert_eq!(output, "---\nname: World\n---\nHello World!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled Markdown output should be used"]
pub fn compile_virtual(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    let (output, warnings) = compile_virtual_collecting_warnings(modules, entry, runtime_vars)?;
    emit_warnings(&warnings);
    Ok(output)
}

/// Compile a module from an in-memory virtual filesystem and return the output
/// along with any collected warnings.
///
/// Unlike [`compile_virtual`], this function does not print warnings to stderr.
/// The caller is responsible for deciding whether to display them (e.g. based
/// on a quiet flag).
///
/// This is the virtual-filesystem counterpart of [`compile_collecting_warnings`].
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
///
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "---\nname: World\n---\nHello {name}!\n".to_string());
///
/// let (output, warnings) = mds::compile_virtual_collecting_warnings(modules, "main.mds", None)?;
/// assert_eq!(output, "---\nname: World\n---\nHello World!\n");
/// assert!(warnings.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled Markdown output and warnings should be used"]
pub fn compile_virtual_collecting_warnings(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(String, Vec<String>), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    let resolved = cache.resolve_key(entry, &vars, &mut warnings)?;
    Ok((build_output(&resolved), warnings))
}

/// Compile an MDS file and return a [`CompileOutput`] with dependency tracking.
///
/// Like [`compile_collecting_warnings`] but also returns the list of imported
/// modules (direct and transitive) in depth-first resolution order. The entry
/// file itself is excluded from `dependencies`.
///
/// Warnings are not printed to stderr; they are returned in `CompileOutput::warnings`.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
///
/// let result = mds::compile_with_deps(Path::new("template.mds"), None)?;
/// println!("{}", result.output);
/// for dep in &result.dependencies { println!("dep: {dep}"); }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_with_deps(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileOutput, MdsError> {
    let path = path.as_ref();
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let resolved = cache.resolve_path(path_str, &vars, &mut warnings)?;
    let output = build_output(&resolved);
    // Post-order DFS guarantees the entry module is last in the cache.
    // Filter by value rather than position for explicitness.
    let deps = cache.dependencies();
    let entry_key = deps.last().cloned();
    let dependencies = deps
        .into_iter()
        .filter(|k| Some(k) != entry_key.as_ref())
        .collect();
    Ok(CompileOutput {
        output,
        warnings,
        dependencies,
    })
}

/// Compile MDS source code from a string and return a [`CompileOutput`] with
/// dependency tracking.
///
/// Like [`compile_str_collecting_warnings`] but also returns the list of imported
/// modules in depth-first resolution order. Because the source is not a file,
/// there is no entry key to exclude — all resolved imports appear in `dependencies`.
///
/// Warnings are not printed to stderr; they are returned in `CompileOutput::warnings`.
///
/// # Examples
///
/// ```rust
/// let result = mds::compile_str_with_deps(
///     "---\ngreeting: Hi\n---\n{greeting} there!\n",
///     None,
///     None,
/// )?;
/// assert_eq!(result.output, "---\ngreeting: Hi\n---\nHi there!\n");
/// assert!(result.dependencies.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_str_with_deps(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileOutput, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let dir = resolve_base_dir(base_dir)?;
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let resolved = cache.resolve_source(source, &dir, &vars, &mut warnings)?;
    let output = build_output(&resolved);
    // resolve_source does not insert the inline source into the modules cache,
    // so cache.dependencies() contains only imported files — no filtering needed.
    let dependencies = cache.dependencies();
    Ok(CompileOutput {
        output,
        warnings,
        dependencies,
    })
}

/// Compile a module from an in-memory virtual filesystem and return a
/// [`CompileOutput`] with dependency tracking.
///
/// Like [`compile_virtual_collecting_warnings`] but also returns the list of
/// imported modules in depth-first resolution order. The entry module itself is
/// excluded from `dependencies`.
///
/// Warnings are not printed to stderr; they are returned in `CompileOutput::warnings`.
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
///
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "---\nname: World\n---\nHello {name}!\n".to_string());
///
/// let result = mds::compile_virtual_with_deps(modules, "main.mds", None)?;
/// assert_eq!(result.output, "---\nname: World\n---\nHello World!\n");
/// assert!(result.dependencies.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_virtual_with_deps(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileOutput, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    let resolved = cache.resolve_key(entry, &vars, &mut warnings)?;
    let output = build_output(&resolved);
    let dependencies = cache
        .dependencies()
        .into_iter()
        .filter(|k| k != entry)
        .collect();
    Ok(CompileOutput {
        output,
        warnings,
        dependencies,
    })
}

/// Check (validate) a module from an in-memory virtual filesystem without rendering output.
///
/// `modules` is a map of key → source content. `entry` is the key of the
/// module to check (e.g. `"main.mds"`).
///
/// Returns `Ok(())` if the module is valid, or an error describing the problem.
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// This is the virtual-filesystem counterpart of [`check`], suitable for
/// WASM environments and testing where OS filesystem access is unavailable.
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
///
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "---\nname: World\n---\nHello {name}!\n".to_string());
///
/// mds::check_virtual(modules, "main.mds", None)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "errors should be handled"]
pub fn check_virtual(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<(), MdsError> {
    let ((), warnings) = check_virtual_collecting_warnings(modules, entry, runtime_vars)?;
    emit_warnings(&warnings);
    Ok(())
}

/// Check (validate) a module from an in-memory virtual filesystem and return any
/// collected warnings without rendering output.
///
/// Unlike [`check_virtual`], this function does not print warnings to stderr.
/// The caller is responsible for deciding whether to display them (e.g. based
/// on a quiet flag).
///
/// This is the virtual-filesystem counterpart of [`check_collecting_warnings`].
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
///
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "---\nname: World\n---\nHello {name}!\n".to_string());
///
/// let ((), warnings) = mds::check_virtual_collecting_warnings(modules, "main.mds", None)?;
/// assert!(warnings.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "warnings should be used"]
pub fn check_virtual_collecting_warnings(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<((), Vec<String>), MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    cache.resolve_key(entry, &vars, &mut warnings)?;
    Ok(((), warnings))
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

/// Extract all import and re-export paths from an MDS source string.
///
/// Parses the source and walks the AST, collecting the `path` field from every
/// import and re-export directive:
/// - `@import "path" as alias` → path
/// - `@import "path"` (merge) → path
/// - `@import { names } from "path"` → path
/// - `@export name from "path"` → path
/// - `@export * from "path"` → path
/// - `@export name` (named, no path) → skipped
///
/// Duplicate paths are deduplicated while preserving insertion order.
/// Returns an error if the source has a syntax error.
///
/// # Examples
///
/// ```rust
/// let paths = mds::scan_imports("@import \"./foo.mds\" as foo\n@import \"./bar.mds\"\n")?;
/// assert_eq!(paths, vec!["./foo.mds".to_string(), "./bar.mds".to_string()]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the extracted import paths should be used"]
pub fn scan_imports(source: &str) -> Result<Vec<String>, MdsError> {
    use indexmap::IndexSet;

    let tokens = lexer::tokenize(source, "")?;
    let module = parser::parse_with_ctx(&tokens, "", source)?;

    let mut paths: IndexSet<String> = IndexSet::new();

    for node in &module.body {
        match node {
            ast::Node::Import(
                ast::ImportDirective::Alias { path, .. }
                | ast::ImportDirective::Merge { path, .. }
                | ast::ImportDirective::Selective { path, .. },
            ) => {
                paths.insert(path.clone());
            }
            ast::Node::Export(
                ast::ExportDirective::ReExport { path, .. }
                | ast::ExportDirective::Wildcard { path },
            ) => {
                paths.insert(path.clone());
            }
            _ => {}
        }
    }

    Ok(paths.into_iter().collect())
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
    let path_str = path_to_str(path)?;
    // Read bytes first, then check size (same TOCTOU-safe pattern as resolver.rs).
    let bytes = std::fs::read(path)
        .map_err(|e| MdsError::io(format!("cannot read vars file {path_str}: {e}")))?;
    if bytes.len() as u64 > MAX_FILE_SIZE {
        return Err(MdsError::resource_limit(format!(
            "vars file exceeds maximum size of {} bytes: {path_str}",
            MAX_FILE_SIZE,
        )));
    }
    let content = String::from_utf8(bytes)
        .map_err(|e| MdsError::io(format!("invalid UTF-8 in vars file {path_str}: {e}")))?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| MdsError::json_error(e.to_string()))?;

    let serde_json::Value::Object(map) = json else {
        return Err(MdsError::json_error("vars file must contain a JSON object"));
    };

    map.into_iter()
        .map(|(key, val)| Value::from_json(val).map(|v| (key, v)))
        .collect()
}

/// Load runtime variables from a JSON string.
///
/// The string must contain a JSON object; each key becomes a variable name.
///
/// # Examples
///
/// ```rust
/// let vars = mds::load_vars_str(r#"{"name": "World", "count": 42}"#)?;
/// let output = mds::compile_virtual(
///     std::collections::HashMap::from([
///         ("main.mds".to_string(), "Hello {name}!\n".to_string()),
///     ]),
///     "main.mds",
///     Some(vars),
/// )?;
/// assert_eq!(output, "Hello World!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the loaded variables should be used"]
pub fn load_vars_str(json: &str) -> Result<HashMap<String, Value>, MdsError> {
    if json.len() as u64 > MAX_FILE_SIZE {
        return Err(MdsError::resource_limit(format!(
            "vars string exceeds maximum size of {} bytes",
            MAX_FILE_SIZE,
        )));
    }
    let parsed: serde_json::Value =
        serde_json::from_str(json).map_err(|e| MdsError::json_error(e.to_string()))?;
    let serde_json::Value::Object(map) = parsed else {
        return Err(MdsError::json_error("vars must be a JSON object"));
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

    // ── load_vars_str: size limit ─────────────────────────────────────────────

    #[test]
    fn load_vars_str_rejects_oversized_input() {
        // Construct a string that exceeds MAX_FILE_SIZE (10 MB).
        let oversized = "x".repeat((MAX_FILE_SIZE as usize) + 1);
        let err = load_vars_str(&oversized).expect_err("expected error for oversized input");
        let msg = err.to_string();
        assert!(
            msg.contains("exceeds maximum size"),
            "error message should mention size limit, got: {msg}"
        );
    }

    #[test]
    fn load_vars_str_accepts_valid_json_within_limit() {
        let json = r#"{"name": "World", "count": 42}"#;
        let vars = load_vars_str(json).expect("valid JSON within size limit should succeed");
        assert_eq!(vars.len(), 2);
    }

    // ── scan_imports tests ────────────────────────────────────────────────────

    #[test]
    fn scan_imports_empty_source() {
        let paths = scan_imports("").expect("empty source should succeed");
        assert!(paths.is_empty(), "expected no paths, got: {paths:?}");
    }

    #[test]
    fn scan_imports_merge_import() {
        let paths = scan_imports("@import \"./foo.mds\"\n").expect("merge import should succeed");
        assert_eq!(paths, vec!["./foo.mds".to_string()]);
    }

    #[test]
    fn scan_imports_selective_import() {
        let paths = scan_imports("@import { a, b } from \"./bar.mds\"\n")
            .expect("selective import should succeed");
        assert_eq!(paths, vec!["./bar.mds".to_string()]);
    }

    #[test]
    fn scan_imports_alias_import() {
        let paths =
            scan_imports("@import \"./baz.mds\" as utils\n").expect("alias import should succeed");
        assert_eq!(paths, vec!["./baz.mds".to_string()]);
    }

    #[test]
    fn scan_imports_reexport_directive() {
        let paths =
            scan_imports("@export greet from \"./lib.mds\"\n").expect("re-export should succeed");
        assert_eq!(paths, vec!["./lib.mds".to_string()]);
    }

    #[test]
    fn scan_imports_wildcard_export() {
        let paths =
            scan_imports("@export * from \"./lib.mds\"\n").expect("wildcard export should succeed");
        assert_eq!(paths, vec!["./lib.mds".to_string()]);
    }

    #[test]
    fn scan_imports_named_export_no_path() {
        let paths =
            scan_imports("@export greeting\n").expect("named export without path should succeed");
        assert!(
            paths.is_empty(),
            "named export has no path, expected empty, got: {paths:?}"
        );
    }

    #[test]
    fn scan_imports_deduplication() {
        let source = "@import \"./foo.mds\"\n@import \"./foo.mds\"\n";
        let paths = scan_imports(source).expect("deduplication test should succeed");
        assert_eq!(
            paths,
            vec!["./foo.mds".to_string()],
            "duplicate paths should be deduplicated"
        );
    }

    #[test]
    fn scan_imports_mixed_directives_in_order() {
        let source = concat!(
            "@import \"./a.mds\" as a\n",
            "@import { foo } from \"./b.mds\"\n",
            "@export bar from \"./c.mds\"\n",
            "@export * from \"./d.mds\"\n",
            "@export localFn\n",
        );
        let paths = scan_imports(source).expect("mixed directives should succeed");
        assert_eq!(
            paths,
            vec![
                "./a.mds".to_string(),
                "./b.mds".to_string(),
                "./c.mds".to_string(),
                "./d.mds".to_string(),
            ]
        );
    }

    #[test]
    fn scan_imports_plain_text_no_imports() {
        let paths = scan_imports("Hello World!\n").expect("plain text should succeed");
        assert!(
            paths.is_empty(),
            "plain text has no imports, got: {paths:?}"
        );
    }

    #[test]
    fn scan_imports_syntax_error() {
        // Unclosed interpolation — lexer/parser should return an error.
        let result = scan_imports("Hello {name\n");
        assert!(result.is_err(), "expected error for malformed source");
    }

    #[test]
    fn scan_imports_frontmatter_with_imports() {
        let source = concat!(
            "---\n",
            "name: Alice\n",
            "---\n",
            "@import \"./lib.mds\"\n",
            "Hello {name}!\n",
        );
        let paths = scan_imports(source).expect("frontmatter + imports should succeed");
        assert_eq!(paths, vec!["./lib.mds".to_string()]);
    }
}
