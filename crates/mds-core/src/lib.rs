//! # MDS — Markdown Script Compiler
//!
//! Compile composable LLM prompt templates from `.mds` files to Markdown.
//!
//! ## Quick Start
//!
//! **In-memory compilation** — compile from a string with no files involved:
//!
//! ```rust
//! let result = mds::compile_str("---\nname: World\n---\nHello {name}!\n")?;
//! assert_eq!(result.into_markdown()?, "---\nname: World\n---\nHello World!\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! **File-based compilation** — compile a template file to Markdown:
//!
//! ```rust,no_run
//! use std::path::Path;
//! let result = mds::compile(Path::new("template.mds"), None)?;
//! let md = result.into_markdown()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! **With runtime variables** — load vars from JSON and pass them in:
//!
//! ```rust,no_run
//! use std::path::Path;
//! let vars = mds::load_vars_file(Path::new("vars.json"))?;
//! let result = mds::compile(Path::new("template.mds"), Some(vars))?;
//! let md = result.into_markdown()?;
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

pub(crate) mod arity;
pub(crate) mod ast;
pub(crate) mod builtins;
pub(crate) mod error;
pub(crate) mod evaluator;
pub(crate) mod formatter;
pub(crate) mod fs;
pub(crate) mod lexer;
pub(crate) mod limits;
pub(crate) mod lint;
pub(crate) mod options;
pub(crate) mod parser;
pub(crate) mod resolver;
pub(crate) mod scope;
pub(crate) mod sourcemap;
pub(crate) mod validator;
pub(crate) mod value;

pub use formatter::{format_str, format_str_with};
pub use fs::{FileSystem, NativeFs, VirtualFs};
pub use lint::{fix, sanitize_control_chars, LintConfig, LintDiagnostic, LintResult, Severity};
pub use options::{
    format_unknown_keys_error, json_type_name, parse_json_vars, reject_unknown_json_keys, VarsError,
};
pub use resolver::ModuleCache;
pub use sourcemap::{CompileOptions, InvalidOptionsError, SourceMap};

/// A single structured message produced by a template containing `@message` blocks.
///
/// Mirrors a chat API message: `{ "role": "system", "content": "..." }`.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Obtain values from the compile API; do not construct via struct literal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Message {
    /// The role string (e.g. `"system"`, `"user"`, `"assistant"`).
    pub role: String,
    /// The rendered body text of the message.
    pub content: String,
}

impl From<evaluator::EvalMessage> for Message {
    fn from(m: evaluator::EvalMessage) -> Self {
        Message {
            role: m.role,
            content: m.content,
        }
    }
}

use std::collections::HashMap;
use std::path::Path;

pub use error::{MdsError, SerializedError, SerializedSpan};
pub use value::Value;

/// The output of a compiled MDS template — either Markdown or structured messages.
///
/// Shape is intrinsic to the template: any `@message` block → Messages, otherwise Markdown.
///
/// This enum is `#[non_exhaustive]`: new variants may be added in minor releases.
/// Exhaustive `match` patterns require a `_ => { ... }` catch-all arm in external crates.
///
/// # JSON shape
/// Serializes as adjacently-tagged: `{"kind":"markdown","value":"..."}` or
/// `{"kind":"messages","value":[...]}`. (Internal tagging cannot represent newtype
/// variants wrapping `String`/`Vec`, so `content`/`value` field is required.)
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum CompiledOutput {
    /// Plain Markdown — the template contained no `@message` blocks.
    Markdown(String),
    /// Structured messages — the template contained at least one `@message` block.
    Messages(Vec<Message>),
}

/// The result of compiling an MDS template with full dependency tracking.
///
/// Returned by every `compile*` entry point. The `output` is intrinsic to the
/// template (Markdown or Messages). The `dependencies` list is in depth-first
/// resolution order and excludes the entry module itself — it contains only
/// the files imported (transitively) by the entry.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Obtain values from the compile API; do not construct via struct literal in
/// external crates.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CompileResult {
    /// The compiled output: Markdown or structured messages, depending on the template.
    pub output: CompiledOutput,
    /// Warnings emitted during compilation (e.g. empty `@include`).
    pub warnings: Vec<String>,
    /// Normalized keys of all modules imported during compilation, in
    /// first-resolution (depth-first) order. Excludes the entry module.
    pub dependencies: Vec<String>,
    /// Source map for the compiled output.  Present only when
    /// `CompileOptions::source_map` is `true` (AC-API-02: absent, not null, when off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_map: Option<SourceMap>,
}

impl CompileResult {
    /// Extract the Markdown string, consuming `self`.
    /// Returns `Err(MdsError::ExpectedMarkdown)` when the output is `Messages`.
    pub fn into_markdown(self) -> Result<String, MdsError> {
        match self.output {
            CompiledOutput::Markdown(s) => Ok(s),
            CompiledOutput::Messages(_) => Err(MdsError::ExpectedMarkdown),
        }
    }

    /// Extract the messages vector, consuming `self`.
    /// Returns `Err(MdsError::ExpectedMessages)` when the output is `Markdown`.
    pub fn into_messages(self) -> Result<Vec<Message>, MdsError> {
        match self.output {
            CompiledOutput::Messages(v) => Ok(v),
            CompiledOutput::Markdown(_) => Err(MdsError::ExpectedMessages),
        }
    }

    /// Build the canonical discriminated-union JS object from this result.
    ///
    /// Produces the `{ kind, <active-payload>, warnings, dependencies }` shape
    /// shared by the NAPI and WASM bindings — a single authoritative implementation
    /// so both wire formats are guaranteed byte-identical.
    ///
    /// Shape:
    /// - Markdown: `{ kind: "markdown", output: <string>, warnings: string[], dependencies: string[] }`
    /// - Messages: `{ kind: "messages", messages: [{role,content},...], warnings: string[], dependencies: string[] }`
    ///
    /// The **inactive payload field is ABSENT** — a markdown result has no `messages`
    /// key; a messages result has no `output` key. Explicit field-by-field construction
    /// via `serde_json::json!()` prevents serde derive from injecting unwanted keys.
    pub fn to_canonical_json(self) -> serde_json::Value {
        let warnings: serde_json::Value = self
            .warnings
            .into_iter()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>()
            .into();
        let dependencies: serde_json::Value = self
            .dependencies
            .into_iter()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>()
            .into();
        // Source map: serialize if present; absent (not null) when off (AC-API-02).
        let source_map_val: Option<serde_json::Value> = self
            .source_map
            .map(|sm| serde_json::to_value(sm).unwrap_or(serde_json::Value::Null));

        match self.output {
            CompiledOutput::Markdown(text) => {
                let mut obj = serde_json::json!({
                    "kind": "markdown",
                    "output": text,
                    "warnings": warnings,
                    "dependencies": dependencies,
                });
                if let Some(sm) = source_map_val {
                    obj["sourceMap"] = sm;
                }
                obj
            }
            CompiledOutput::Messages(msgs) => {
                let messages: serde_json::Value = msgs
                    .into_iter()
                    .map(|m| {
                        serde_json::json!({
                            "role": m.role,
                            "content": m.content,
                        })
                    })
                    .collect::<Vec<_>>()
                    .into();
                let mut obj = serde_json::json!({
                    "kind": "messages",
                    "messages": messages,
                    "warnings": warnings,
                    "dependencies": dependencies,
                });
                if let Some(sm) = source_map_val {
                    obj["sourceMap"] = sm;
                }
                obj
            }
        }
    }
}

// ── Test-only Markdown-extracting shims ───────────────────────────────────────
//
// The public `compile_*` entry points return a `CompileResult` whose `output` is
// intrinsic (Markdown or Messages). The many markdown-mode unit tests in the
// internal `#[cfg(test)]` modules (parser_tests, resolver_tests, …) predate the
// intrinsic API and assert on a plain `String`. These thin shims preserve the old
// `Result<String, MdsError>` shape — so a markdown template still surfaces compile
// errors via `?`/`unwrap_err`, while `into_markdown()` extracts the rendered string.
#[cfg(test)]
pub(crate) fn compile_str_md(source: &str) -> Result<String, MdsError> {
    compile_str(source).and_then(CompileResult::into_markdown)
}

#[cfg(test)]
pub(crate) fn compile_str_with_md(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    compile_str_with(source, base_dir, runtime_vars).and_then(CompileResult::into_markdown)
}

#[cfg(test)]
pub(crate) fn compile_virtual_md(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<String, MdsError> {
    compile_virtual(modules, entry, runtime_vars).and_then(CompileResult::into_markdown)
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

/// Maximum number of lint diagnostics collected per file before truncation.
///
/// When the `lint_*` entry points accumulate this many diagnostics for a single
/// file, collection stops and `LintResult::truncated` is set to `true`. Re-run
/// after resolving visible findings to surface the rest.
///
/// Pin guard: L-API-5 / AC-API-10.
pub const MAX_DIAGNOSTICS: usize = limits::MAX_DIAGNOSTICS;

/// Compile an MDS file, returning a [`CompileResult`].
///
/// The output shape is intrinsic to the template: a template containing any
/// `@message` block produces [`CompiledOutput::Messages`], otherwise
/// [`CompiledOutput::Markdown`]. Use [`CompileResult::into_markdown`] /
/// [`CompileResult::into_messages`] to extract the expected variant.
///
/// Warnings (e.g. empty `@include`) are printed to stderr. Pass `runtime_vars`
/// to override or supply variables that aren't defined in frontmatter.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let result = mds::compile(Path::new("template.mds"), None)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output should be used"]
pub fn compile(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let result = compile_collecting_warnings(path, runtime_vars)?;
    emit_warnings(&result.warnings);
    Ok(result)
}

/// Compile MDS source code from a string, returning a [`CompileResult`].
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Examples
///
/// ```rust
/// let result = mds::compile_str("---\ngreeting: Hi\n---\n{greeting} there!\n")?;
/// assert_eq!(result.into_markdown()?, "---\ngreeting: Hi\n---\nHi there!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output should be used"]
pub fn compile_str(source: &str) -> Result<CompileResult, MdsError> {
    compile_str_with(source, None, None)
}

/// Compile MDS source code from a string with options, returning a [`CompileResult`].
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
/// let result = mds::compile_str_with(
///     "---\nlang: unknown\n---\nI love {lang}!\n",
///     Some(Path::new("templates/")),
///     Some(vars),
/// )?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output should be used"]
pub fn compile_str_with(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let result = compile_str_collecting_warnings(source, base_dir, runtime_vars)?;
    emit_warnings(&result.warnings);
    Ok(result)
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
    cache.resolve_path_intrinsic(path_str, &vars, &mut warnings)?;
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
    cache.resolve_source_intrinsic(source, &dir, &vars, &mut warnings)?;
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

/// Compile an MDS file and return a [`CompileResult`] (output, warnings, dependencies).
///
/// Unlike [`compile`], this function does not print warnings to stderr. The caller
/// is responsible for deciding whether to display them (e.g. based on a quiet flag).
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let result = mds::compile_collecting_warnings(Path::new("template.mds"), None)?;
/// for w in &result.warnings { eprintln!("warning: {w}"); }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_collecting_warnings(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let path = path.as_ref();
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let output = cache.resolve_path_intrinsic(path_str, &vars, &mut warnings)?;
    // The intrinsic resolver does NOT insert the entry module into the cache (only
    // imported sub-modules are inserted), so `dependencies()` normally already
    // excludes the entry. Filter the canonical entry key anyway to also drop it in
    // the edge case where a transitive import re-imports the entry. check_symlink
    // mirrors the normalize("", path) the resolver already performed — no extra I/O.
    let canonical_entry = NativeFs::check_symlink(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path_str.to_owned());
    let dependencies = cache
        .dependencies()
        .into_iter()
        .filter(|k| k != &canonical_entry)
        .collect();
    Ok(CompileResult {
        output,
        warnings,
        dependencies,
        source_map: None,
    })
}

/// Compile MDS source from a string and return a [`CompileResult`].
///
/// Unlike [`compile_str_with`], this function does not print warnings to stderr. The caller
/// is responsible for deciding whether to display them (e.g. based on a quiet flag).
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_str_collecting_warnings(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let dir = resolve_base_dir(base_dir)?;
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let output = cache.resolve_source_intrinsic(source, &dir, &vars, &mut warnings)?;
    // resolve_source_intrinsic does not insert the inline source into the modules cache,
    // so cache.dependencies() contains only imported files — no entry-key filtering needed.
    let dependencies = cache.dependencies();
    Ok(CompileResult {
        output,
        warnings,
        dependencies,
        source_map: None,
    })
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
    // Dispatch on output shape so a messages template's mixed-content check runs
    // during validation; the CompiledOutput itself is discarded.
    cache.resolve_path_intrinsic(path_str, &vars, &mut warnings)?;
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
    cache.resolve_source_intrinsic(source, &dir, &vars, &mut warnings)?;
    Ok(((), warnings))
}

/// Output-side reserved frontmatter keys: stripped from raw YAML before re-emitting frontmatter.
///
/// `extends` is intentionally absent — it is a directive consumed during template inheritance,
/// never emitted as output FM. Contrast with `RESERVED_MERGE_KEYS` in `resolver/frontmatter.rs`
/// which includes `extends` (the merge gate sees it as a directive to consume, not a value key).
///
/// SYNC POINT: when this constant changes, also audit `RESERVED_MERGE_KEYS` in
/// `resolver/frontmatter.rs` — the two constants serve different purposes and are intentionally
/// not identical.
const RESERVED_OUTPUT_KEYS: &[&str] = &["type", "imports"];

/// Remove reserved keys (`type: mds` and `imports:` blocks) from raw frontmatter content.
///
/// Returns `Some(remaining)` if any non-whitespace content survives after filtering,
/// or `None` if the frontmatter would be empty (nothing worth emitting).
///
/// Strips the keys named in `RESERVED_OUTPUT_KEYS`:
/// - Top-level `type: mds` lines (all three YAML quoting styles; `type` with any other value
///   is not a reserved directive and is left in place).
/// - Top-level `imports:` key and its continuation lines (indented child lines).
fn strip_reserved_keys(raw: &str) -> Option<String> {
    // Verify this function's bespoke stripping logic covers the keys listed in
    // RESERVED_OUTPUT_KEYS. Zero cost in release; surfaces drift in debug/test builds.
    debug_assert!(
        RESERVED_OUTPUT_KEYS.contains(&"type") && RESERVED_OUTPUT_KEYS.contains(&"imports"),
        "RESERVED_OUTPUT_KEYS must include 'type' and 'imports' — update strip_reserved_keys if it changes"
    );
    let mut filtered = String::with_capacity(raw.len());
    let mut in_imports_block = false;

    for line in raw.lines() {
        // Blank lines are not YAML keys — preserve them outside an imports block
        // and skip them (without resetting state) inside one.
        if line.is_empty() {
            if !in_imports_block {
                filtered.push('\n');
            }
            continue;
        }

        // Only inspect top-level (no leading whitespace) keys.
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');

        if is_top_level {
            // Reset imports block tracking whenever we see a new top-level key.
            if line.starts_with("imports:") {
                in_imports_block = true;
                // Drop this line (the `imports:` key itself).
                continue;
            }
            in_imports_block = false;

            // Strip the `type: mds` line (plain, double-quoted, and single-quoted).
            let is_type_mds = line.strip_prefix("type:").is_some_and(|v| {
                let v = v.trim();
                v == "mds" || v == "\"mds\"" || v == "'mds'"
            });
            if is_type_mds {
                continue;
            }
        } else if in_imports_block {
            // Continuation lines of the `imports:` block — drop them too.
            continue;
        }

        filtered.push_str(line);
        filtered.push('\n');
    }

    let trimmed = filtered.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("{trimmed}\n"))
    }
}

/// Prepend YAML frontmatter fences to a compiled body.
///
/// If `raw` is `None`, or after stripping reserved keys the frontmatter is empty,
/// the body is returned unchanged.
pub(crate) fn prepend_frontmatter(raw: Option<&str>, body: String) -> String {
    let Some(raw) = raw else {
        return body;
    };
    let Some(cleaned) = strip_reserved_keys(raw) else {
        return body;
    };
    format!("---\n{cleaned}---\n{body}")
}

/// Normalise output whitespace using the "interior-verbatim with trailing-edge
/// normalisation" contract (#150/#151):
///
/// - Strip every `\r` character (Windows line endings — `\r\n` becomes `\n`).
/// - Trim all trailing whitespace from the very end of the string.
/// - If the result is non-empty, ensure exactly one final `\n`.
/// - Whitespace-only / empty input returns `""`.
///
/// Interior blank-line runs, leading blank lines, and mid-document whitespace-only
/// lines all pass through **verbatim** — only the trailing edge is normalised.
///
/// **Formatter dependency**: `mds fmt` (`formatter.rs`) mirrors these exact semantics.
/// `@message` and `@define` body content bypasses this function entirely (evaluated
/// via `.trim()` only — see `collect_single_message` and `invoke_function` in
/// `evaluator.rs`); the formatter marks those as raw-content regions. See
/// `raw_content_spans` in `formatter.rs` for the authoritative region boundary.
pub(crate) fn clean_output(s: &str) -> String {
    // Strip \r unconditionally (Windows line endings).
    let no_cr: std::borrow::Cow<str> = if s.as_bytes().contains(&b'\r') {
        std::borrow::Cow::Owned(s.chars().filter(|&c| c != '\r').collect())
    } else {
        std::borrow::Cow::Borrowed(s)
    };

    // Trailing-edge normalisation: trim all trailing whitespace, then add one \n.
    let trimmed = no_cr.trim_end();
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
/// let result = mds::compile_virtual(modules, "main.mds", None)?;
/// assert_eq!(result.into_markdown()?, "---\nname: World\n---\nHello World!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output should be used"]
pub fn compile_virtual(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let result = compile_virtual_collecting_warnings(modules, entry, runtime_vars)?;
    emit_warnings(&result.warnings);
    Ok(result)
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
/// let result = mds::compile_virtual_collecting_warnings(modules, "main.mds", None)?;
/// assert_eq!(result.into_markdown()?, "---\nname: World\n---\nHello World!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_virtual_collecting_warnings(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    let output = cache.resolve_virtual_intrinsic(entry, &vars, &mut warnings)?;
    let dependencies = cache
        .dependencies()
        .into_iter()
        .filter(|k| k != entry)
        .collect();
    Ok(CompileResult {
        output,
        warnings,
        dependencies,
        source_map: None,
    })
}

/// Compile an MDS file and return a [`CompileResult`] with dependency tracking.
///
/// Like [`compile_collecting_warnings`] (and now identical to it): the result
/// includes the list of imported modules (direct and transitive) in depth-first
/// resolution order, with the entry file excluded from `dependencies`.
///
/// Warnings are not printed to stderr; they are returned in `CompileResult::warnings`.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
///
/// let result = mds::compile_with_deps(Path::new("template.mds"), None)?;
/// for dep in &result.dependencies { println!("dep: {dep}"); }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_with_deps(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    compile_collecting_warnings(path, runtime_vars)
}

/// Compile MDS source code from a string and return a [`CompileResult`] with
/// dependency tracking.
///
/// Like [`compile_str_collecting_warnings`] (and now identical to it). Because the
/// source is not a file, there is no entry key to exclude — all resolved imports
/// appear in `dependencies`.
///
/// Warnings are not printed to stderr; they are returned in `CompileResult::warnings`.
///
/// # Examples
///
/// ```rust
/// let result = mds::compile_str_with_deps(
///     "---\ngreeting: Hi\n---\n{greeting} there!\n",
///     None,
///     None,
/// )?;
/// assert_eq!(result.into_markdown()?, "---\ngreeting: Hi\n---\nHi there!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_str_with_deps(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    compile_str_collecting_warnings(source, base_dir, runtime_vars)
}

/// Compile a module from an in-memory virtual filesystem and return a
/// [`CompileResult`] with dependency tracking.
///
/// Like [`compile_virtual_collecting_warnings`] (and now identical to it): the
/// result includes the list of imported modules in depth-first resolution order,
/// with the entry module excluded from `dependencies`.
///
/// Warnings are not printed to stderr; they are returned in `CompileResult::warnings`.
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
/// assert_eq!(result.into_markdown()?, "---\nname: World\n---\nHello World!\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_virtual_with_deps(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
) -> Result<CompileResult, MdsError> {
    compile_virtual_collecting_warnings(modules, entry, runtime_vars)
}

// ── Opts-bearing binding seam functions ──────────────────────────────────────
//
// These are the primary entry points for CP5/CP6 (CLI, NAPI, WASM) when source
// maps are requested.  They accept a [`CompileOptions`] and return a
// [`CompileResult`] whose `source_map` field is populated when
// `opts.source_map` is `true`.

/// Compile an MDS file with optional source-map generation.
///
/// Like [`compile_with_deps`] but accepts [`CompileOptions`] for fine-grained
/// control.  When `opts.source_map` is `true`, `result.source_map` contains a
/// Source Map v3 document (AC-API-02: absent, not null, when `source_map: false`).
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let result = mds::compile_with_deps_opts(Path::new("t.mds"), None, mds::CompileOptions { source_map: true, include_sources_content: false })?;
/// if let Some(sm) = result.source_map { println!("{}", sm.to_json()); }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_with_deps_opts(
    path: impl AsRef<Path>,
    runtime_vars: Option<HashMap<String, Value>>,
    opts: CompileOptions,
) -> Result<CompileResult, MdsError> {
    let path = path.as_ref();
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let (output, source_map) =
        cache.resolve_path_intrinsic_opts(path_str, &vars, &opts, &mut warnings)?;
    let canonical_entry = NativeFs::check_symlink(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path_str.to_owned());
    let dependencies = cache
        .dependencies()
        .into_iter()
        .filter(|k| k != &canonical_entry)
        .collect();
    Ok(CompileResult {
        output,
        warnings,
        dependencies,
        source_map,
    })
}

/// Compile MDS source from a string with optional source-map generation.
///
/// Like [`compile_str_with_deps`] but accepts [`CompileOptions`].
///
/// # Examples
///
/// ```rust
/// let result = mds::compile_str_with_deps_opts(
///     "Hello!\n",
///     None,
///     None,
///     mds::CompileOptions { source_map: true, include_sources_content: false },
/// )?;
/// assert!(result.source_map.is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_str_with_deps_opts(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
    opts: CompileOptions,
) -> Result<CompileResult, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let dir = resolve_base_dir(base_dir)?;
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let (output, source_map) =
        cache.resolve_source_intrinsic_opts(source, &dir, &vars, &opts, &mut warnings)?;
    let dependencies = cache.dependencies();
    Ok(CompileResult {
        output,
        warnings,
        dependencies,
        source_map,
    })
}

/// Compile a virtual-filesystem module with optional source-map generation.
///
/// Like [`compile_virtual_with_deps`] but accepts [`CompileOptions`].
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "Hello!\n".to_string());
/// let result = mds::compile_virtual_with_deps_opts(
///     modules,
///     "main.mds",
///     None,
///     mds::CompileOptions { source_map: true, include_sources_content: false },
/// )?;
/// assert!(result.source_map.is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output, warnings, and dependencies should be used"]
pub fn compile_virtual_with_deps_opts(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
    opts: CompileOptions,
) -> Result<CompileResult, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    let (output, source_map) =
        cache.resolve_virtual_intrinsic_opts(entry, &vars, &opts, &mut warnings)?;
    let dependencies = cache
        .dependencies()
        .into_iter()
        .filter(|k| k != entry)
        .collect();
    Ok(CompileResult {
        output,
        warnings,
        dependencies,
        source_map,
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
    // Dispatch on output shape so a messages template's mixed-content check runs
    // during validation; the CompiledOutput itself is discarded.
    cache.resolve_virtual_intrinsic(entry, &vars, &mut warnings)?;
    Ok(((), warnings))
}

// ── Lint entry points ─────────────────────────────────────────────────────────

/// Lint an MDS source string with default options.
///
/// Runs the check gate (resolve+validate) first: returns `Err(MdsError)` when the
/// template does not compile. On a clean gate, applies the 9 lint rules and returns
/// a `LintResult` (empty in S1 — rules arrive in S2).
///
/// # Examples
///
/// ```rust
/// let result = mds::lint_str("Hello!\n")?;
/// assert!(result.diagnostics.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "lint findings should be used"]
pub fn lint_str(source: &str) -> Result<LintResult, MdsError> {
    lint_str_with(source, None, None, &LintConfig::default())
}

/// Lint an MDS source string with full options.
///
/// `base_dir` sets the root for resolving `@import` paths; defaults to the current
/// directory. `runtime_vars` are injected into the check gate (not into lint rules).
/// `config` carries per-rule severity overrides.
///
/// Pipeline (AC-PERF-01): check gate ONCE → lint entry source.
///
/// # Examples
///
/// ```rust
/// let result = mds::lint_str_with(
///     "Hello!\n",
///     None,
///     None,
///     &mds::LintConfig::default(),
/// )?;
/// assert!(result.diagnostics.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "lint findings should be used"]
pub fn lint_str_with(
    source: &str,
    base_dir: Option<&Path>,
    runtime_vars: Option<HashMap<String, Value>>,
    config: &LintConfig,
) -> Result<LintResult, MdsError> {
    let dir = resolve_base_dir(base_dir)?;
    let vars = runtime_vars.unwrap_or_default();
    // Step 1: check gate — resolve+validate ONCE (AC-PERF-01).
    {
        let mut cache = ModuleCache::new();
        let mut warnings = vec![];
        cache.resolve_source_intrinsic(source, &dir, &vars, &mut warnings)?;
    }
    // Step 2: lint the entry source.
    // Use the same default filename as the WASM backend so lint(source) produces
    // a byte-identical "file" key across all surfaces (AC-API-06).
    lint::lint_source(source, "input.mds", config)
}

/// Lint an MDS file.
///
/// Runs the check gate first (returns `Err` on compile failure), then applies lint
/// rules to the entry file source.
///
/// # Examples
///
/// ```rust,no_run
/// use std::path::Path;
/// let result = mds::lint(Path::new("template.mds"), None, &mds::LintConfig::default())?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "lint findings should be used"]
pub fn lint(
    path: &Path,
    runtime_vars: Option<HashMap<String, Value>>,
    config: &LintConfig,
) -> Result<LintResult, MdsError> {
    let path_str = path_to_str(path)?;
    let vars = runtime_vars.unwrap_or_default();
    // Step 1: check gate — resolve+validate ONCE (AC-PERF-01).
    {
        let mut cache = ModuleCache::new();
        let mut warnings = vec![];
        cache.resolve_path_intrinsic(path_str, &vars, &mut warnings)?;
    }
    // Read source for lint re-parse (mirrors NativeFs::read size guard).
    let bytes =
        std::fs::read(path).map_err(|e| MdsError::io(format!("cannot read {path_str}: {e}")))?;
    if bytes.len() as u64 > limits::MAX_FILE_SIZE {
        return Err(MdsError::resource_limit(format!(
            "file too large ({} bytes, max {} bytes): {path_str}",
            bytes.len(),
            limits::MAX_FILE_SIZE,
        )));
    }
    let source = String::from_utf8(bytes)
        .map_err(|e| MdsError::io(format!("invalid UTF-8 in {path_str}: {e}")))?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path_str);
    lint::lint_source(&source, filename, config)
}

/// Lint an entry module from a virtual filesystem.
///
/// Runs the check gate first (returns `Err` on compile failure), then applies lint
/// rules to the entry source.
///
/// # Examples
///
/// ```rust
/// use std::collections::HashMap;
/// let mut modules = HashMap::new();
/// modules.insert("main.mds".to_string(), "Hello!\n".to_string());
/// let result = mds::lint_virtual(modules, "main.mds", None, &mds::LintConfig::default())?;
/// assert!(result.diagnostics.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "lint findings should be used"]
pub fn lint_virtual(
    modules: HashMap<String, String>,
    entry: &str,
    runtime_vars: Option<HashMap<String, Value>>,
    config: &LintConfig,
) -> Result<LintResult, MdsError> {
    let vars = runtime_vars.unwrap_or_default();
    // Get the entry source before moving `modules` into the check gate.
    let source = modules
        .get(entry)
        .ok_or_else(|| MdsError::file_not_found(entry))?
        .clone();
    // Step 1: check gate — resolve+validate ONCE (AC-PERF-01).
    {
        let mut cache = ModuleCache::virtual_fs(modules);
        let mut warnings = vec![];
        cache.resolve_virtual_intrinsic(entry, &vars, &mut warnings)?;
    }
    // Step 2: lint the entry source.
    lint::lint_source(&source, entry, config)
}

/// Convenience wrapper around [`compile`] for callers who have a path as `&str`.
///
/// Warnings (e.g. empty `@include`) are printed to stderr.
///
/// # Examples
///
/// ```rust,no_run
/// let result = mds::compile_file("template.mds")?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the compiled output should be used"]
pub fn compile_file(path: &str) -> Result<CompileResult, MdsError> {
    compile(Path::new(path), None)
}

/// Extract all import and re-export paths from an MDS source string.
///
/// Parses the source and walks the AST, collecting the `path` field from every
/// import and re-export directive:
/// - Frontmatter `imports:` key (alias, merge, and selective forms) — returned FIRST
/// - `@import "path" as alias` → path
/// - `@import "path"` (merge) → path
/// - `@import { names } from "path"` → path
/// - `@export name from "path"` → path
/// - `@export * from "path"` → path
/// - `@export name` (named, no path) → skipped
///
/// Frontmatter paths are inserted before body paths, matching resolution order.
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

    // Insert @extends base path FIRST — the base is the first dependency (spec §4.11).
    if let Some(ext) = module.extends.as_ref() {
        paths.insert(ext.path.clone());
    }

    // Insert frontmatter import paths (they resolve before body imports).
    // Best-effort: ignore parse errors here (parse errors will surface at compile time).
    if let Some(fm) = module.frontmatter.as_ref() {
        if let Ok(fm_imports) = resolver::parse_frontmatter_imports(&fm.raw) {
            for imp in &fm_imports {
                paths.insert(imp.path().to_owned());
            }
        }
    }

    // Then insert body import paths (deduplication is automatic via IndexSet).
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
                | ast::ExportDirective::Wildcard { path, .. },
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
/// let result = mds::compile(Path::new("template.mds"), Some(vars))?;
/// let md = result.into_markdown()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the loaded variables should be used"]
pub fn load_vars_file(path: &Path) -> Result<HashMap<String, Value>, MdsError> {
    let path_str = path_to_str(path)?;
    // PF-004: guard the vars-file path through the same symlink check that the
    // resolver applies to every imported file — avoids a raw read that bypasses
    // the security gate. check_symlink returns the canonical path; we use the
    // original path for error messages (it names what the caller passed).
    NativeFs::check_symlink(path).map_err(|e| match e {
        MdsError::ImportError { .. } => MdsError::import_error(format!(
            "symlinks are not allowed in vars file path: {path_str}"
        )),
        other => other,
    })?;
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
/// let result = mds::compile_virtual(
///     std::collections::HashMap::from([
///         ("main.mds".to_string(), "Hello {name}!\n".to_string()),
///     ]),
///     "main.mds",
///     Some(vars),
/// )?;
/// assert_eq!(result.into_markdown()?, "Hello World!\n");
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
    fn clean_output_preserves_interior_blank_runs() {
        // Interior blank-line runs pass through verbatim — no collapsing (#150/#151).
        assert_eq!(clean_output("a\n\n\n\nb"), "a\n\n\n\nb\n");
    }

    #[test]
    fn clean_output_preserves_leading_newlines() {
        // Leading blank lines are no longer stripped — interior-verbatim contract (#150/#151).
        assert_eq!(clean_output("\n\n\nhello"), "\n\n\nhello\n");
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

    // ── strip_reserved_keys: type: mds variants ───────────────────────────────

    #[test]
    fn strip_reserved_keys_plain_value() {
        // Baseline: unquoted `type: mds` is stripped.
        let raw = "type: mds\nname: Alice\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(result, Some("name: Alice\n".to_string()));
    }

    #[test]
    fn strip_reserved_keys_double_quoted() {
        // `type: "mds"` — double-quoted YAML string — must also be stripped.
        let raw = "type: \"mds\"\nname: Alice\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("name: Alice\n".to_string()),
            "double-quoted type:mds should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_reserved_keys_single_quoted() {
        // `type: 'mds'` — single-quoted YAML string — must also be stripped.
        let raw = "type: 'mds'\nname: Alice\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("name: Alice\n".to_string()),
            "single-quoted type:mds should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_reserved_keys_no_space_after_colon() {
        // `type:mds` — no space after colon — must also be stripped.
        let raw = "type:mds\nname: Alice\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("name: Alice\n".to_string()),
            "no-space type:mds should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_reserved_keys_quoted_only_returns_none() {
        // Frontmatter with only a quoted `type: "mds"` should return None (empty after strip).
        let raw = "type: \"mds\"\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result, None,
            "frontmatter with only quoted type:mds should be None, got: {result:?}"
        );
    }

    #[test]
    fn strip_reserved_keys_indented_quoted_not_stripped() {
        // Indented `  type: "mds"` inside a nested mapping must NOT be stripped.
        let raw = "type: mds\nconfig:\n  type: \"mds\"\n  theme: dark\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("config:\n  type: \"mds\"\n  theme: dark\n".to_string()),
            "indented quoted type:mds should be preserved, got: {result:?}"
        );
    }

    // ── strip_reserved_keys: imports block stripping ──────────────────────────

    #[test]
    fn strip_imports_block() {
        // Multi-line `imports:` block with indented entries is fully stripped.
        let raw = "name: Alice\nimports:\n  - path: ./lib.mds\n    as: lib\ngreeting: hello\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("name: Alice\ngreeting: hello\n".to_string()),
            "imports block should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_imports_and_type_returns_none_when_empty() {
        // Only `type: mds` and `imports:` — nothing left after stripping → None.
        let raw = "type: mds\nimports:\n  - path: ./lib.mds\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result, None,
            "only reserved keys, should be None, got: {result:?}"
        );
    }

    #[test]
    fn strip_imports_flow_style() {
        // Inline (flow-style) `imports:` key on a single line with no children.
        // The key line itself is dropped; the next top-level key continues normally.
        let raw = "imports: []\nname: Bob\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("name: Bob\n".to_string()),
            "flow-style imports should be stripped, got: {result:?}"
        );
    }

    #[test]
    fn strip_preserves_nested_imports_key() {
        // An indented `imports:` key inside a nested mapping must NOT be stripped.
        let raw = "name: Alice\nconfig:\n  imports: true\n  theme: dark\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("name: Alice\nconfig:\n  imports: true\n  theme: dark\n".to_string()),
            "nested imports key should be preserved, got: {result:?}"
        );
    }

    #[test]
    fn strip_imports_block_with_blank_lines() {
        // Blank lines inside an `imports:` block must not leak into the output.
        // An empty line satisfies `!starts_with(' ')` so without an explicit guard
        // it would reset `in_imports_block` and allow the trailing `greeting:` to
        // be emitted — but only after the blank-line guard was added.
        let raw = "imports:\n  - path: ./a.mds\n\n  - path: ./b.mds\ngreeting: hello\n";
        let result = strip_reserved_keys(raw);
        assert_eq!(
            result,
            Some("greeting: hello\n".to_string()),
            "blank lines inside imports block must not leak; got: {result:?}"
        );
    }

    // ── scan_imports: frontmatter imports paths ───────────────────────────────

    #[test]
    fn scan_imports_fm_alias() {
        let source = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "Hello!\n",
        );
        let paths = scan_imports(source).expect("should succeed");
        assert_eq!(paths, vec!["./lib.mds".to_string()]);
    }

    #[test]
    fn scan_imports_fm_and_body() {
        // Frontmatter paths must appear BEFORE body paths.
        let source = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./fm_lib.mds\n",
            "---\n",
            "@import \"./body_lib.mds\"\n",
            "Hello!\n",
        );
        let paths = scan_imports(source).expect("should succeed");
        assert_eq!(
            paths,
            vec!["./fm_lib.mds".to_string(), "./body_lib.mds".to_string()]
        );
    }

    #[test]
    fn scan_imports_extends_before_fm_and_body() {
        // A2: the @extends base path must appear FIRST — before frontmatter imports
        // AND before body imports — so a dependency scanner sees the base as the
        // leading dependency. Exercises the ordering branch with all three present.
        // scan_imports is a static parse-only scan (no child-only-blocks enforcement),
        // so a top-level @import alongside @extends exercises the ordering directly.
        let source = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./fm_lib.mds\n",
            "---\n",
            "@extends \"./base.mds\"\n",
            "@import \"./body_lib.mds\"\n",
            "Hello!\n",
        );
        let paths = scan_imports(source).expect("should succeed");
        assert_eq!(
            paths,
            vec![
                "./base.mds".to_string(),
                "./fm_lib.mds".to_string(),
                "./body_lib.mds".to_string(),
            ],
            "A2: extends path must precede frontmatter and body imports"
        );
    }

    #[test]
    fn scan_imports_fm_dedup() {
        // Same path in frontmatter and body → deduplicated, fm path wins (first insertion).
        let source = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "---\n",
            "@import \"./lib.mds\"\n",
            "Hello!\n",
        );
        let paths = scan_imports(source).expect("should succeed");
        assert_eq!(paths, vec!["./lib.mds".to_string()]);
    }

    #[test]
    fn scan_imports_fm_all_forms() {
        // All three frontmatter import forms return their paths.
        let source = concat!(
            "---\n",
            "imports:\n",
            "  - path: ./a.mds\n",
            "    as: a\n",
            "  - path: ./b.mds\n",
            "  - path: ./c.mds\n",
            "    names: [f]\n",
            "---\n",
            "Hello!\n",
        );
        let paths = scan_imports(source).expect("should succeed");
        assert_eq!(
            paths,
            vec![
                "./a.mds".to_string(),
                "./b.mds".to_string(),
                "./c.mds".to_string(),
            ]
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
