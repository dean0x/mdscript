use std::sync::Arc;

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

use crate::lint::{named_source_for_render, sanitize_control_chars, sanitize_control_chars_wire};

// ── Serializable error types ──────────────────────────────────────────────────

/// A serializable representation of a source span.
///
/// `offset` and `length` are in bytes from the start of the source string,
/// matching `miette::SourceSpan`. `line` is 1-indexed; `column` is the
/// 1-indexed character position (Unicode scalar values) from the start of the
/// line — NOT a byte offset and NOT UTF-16 code units.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Construct via [`SerializedSpan::new`] and the optional `with_line` /
/// `with_column` builders; do not construct via struct literal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SerializedSpan {
    pub offset: usize,
    pub length: usize,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

impl SerializedSpan {
    /// Construct a `SerializedSpan` with byte `offset` and `length`; line and
    /// column default to `None`.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::SerializedSpan;
    /// let span = SerializedSpan::new(10, 5);
    /// assert_eq!(span.offset, 10);
    /// assert_eq!(span.length, 5);
    /// assert!(span.line.is_none());
    /// assert!(span.column.is_none());
    /// ```
    #[must_use]
    pub fn new(offset: usize, length: usize) -> Self {
        SerializedSpan {
            offset,
            length,
            line: None,
            column: None,
        }
    }

    /// Set the 1-indexed line number.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::SerializedSpan;
    /// let span = SerializedSpan::new(10, 5).with_line(3);
    /// assert_eq!(span.line, Some(3));
    /// ```
    #[must_use]
    pub fn with_line(mut self, line: usize) -> Self {
        self.line = Some(line);
        self
    }

    /// Set the 1-indexed character column.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::SerializedSpan;
    /// let span = SerializedSpan::new(10, 5).with_column(7);
    /// assert_eq!(span.column, Some(7));
    /// ```
    #[must_use]
    pub fn with_column(mut self, column: usize) -> Self {
        self.column = Some(column);
        self
    }
}

/// A serializable, `serde`-friendly representation of an [`MdsError`].
///
/// Suitable for embedding in JSON API responses or structured log output.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Obtain values via [`MdsError::serialize`]; do not construct via struct literal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SerializedError {
    pub code: String,
    pub message: String,
    pub help: Option<String>,
    pub span: Option<SerializedSpan>,
}

/// Compute the 1-indexed line and column (character-based) for a byte offset in source.
///
/// Returns `None` if `offset` exceeds `source.len()` OR if `offset` does not fall
/// on a UTF-8 character boundary. A foreign or stale offset (e.g. one computed
/// against a different source string — as can occur when a base-template span is
/// reported against a child source in `@extends` validation) will yield `None`
/// rather than panicking with "byte index N is not a char boundary".
///
/// Both line and column are 1-indexed: the very first character is (1, 1).
///
/// Column counts Unicode scalar values (characters) from the start of the current
/// line, not bytes. This matches the convention used by editors and language
/// servers that report character-based positions.
fn compute_line_column(source: &str, offset: usize) -> Option<(usize, usize)> {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return None;
    }
    let mut line = 1usize;
    let mut col = 1usize;
    for ch in source[..offset].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Some((line, col))
}

/// Format an arity range for display in error messages.
///
/// - `min == max == 1` → "expected 1 argument"
/// - `min == max` → "expected N arguments"
/// - `min != max` → "expected M-N arguments"
fn format_arity(min: usize, max: usize) -> String {
    if min == max {
        if min == 1 {
            "expected 1 argument".to_string()
        } else {
            format!("expected {min} arguments")
        }
    } else {
        format!("expected {min}-{max} arguments")
    }
}

/// Build the `(span, src)` pair shared by all `_at` constructors.
///
/// Defense-in-depth guard: if `offset` or `offset + len` is out of bounds for
/// `source`, or if either boundary is not a UTF-8 character boundary, `src` is
/// set to `None` so miette never tries to read outside the source string (which
/// would produce a raw `OutOfBounds` render). The span offset/length are still
/// preserved in the `Some(SourceSpan)` so that `serialize()` can emit them and
/// callers that only check `span.is_some()` are unaffected.
///
/// In debug/test builds a `debug_assert!` fires for the cross-source case (where
/// `source` is non-empty — deliberate empty-source unit test shorthands are excluded)
/// so mismatches surface loudly during development.
fn at(
    file: &str,
    source: &str,
    offset: usize,
    len: usize,
) -> (Option<SourceSpan>, Option<Arc<miette::NamedSource<String>>>) {
    let end = offset.saturating_add(len);
    let in_bounds =
        end <= source.len() && source.is_char_boundary(offset) && source.is_char_boundary(end);

    // Fire only when source is non-empty: an empty source is a deliberate simplification
    // used in some unit tests (passing "" when the exact source text isn't needed). The
    // real cross-source bug always involves a non-empty base source paired with a child
    // context — that case must be caught loudly in debug/test builds.
    debug_assert!(
        in_bounds || source.is_empty(),
        "MdsError::at(): cross-source offset mismatch — offset {offset}+len {len} is out of \
         bounds or not a char boundary for source of length {} (file: {file}). This means an \
         AST node's offset (relative to its own file) was paired with a different source string. \
         Check span construction at the @extends validation site.",
        source.len()
    );

    if !in_bounds {
        // Out-of-bounds or non-char-boundary: keep the span's offset/length so
        // serialize() can still emit numeric values, but drop the source so miette
        // never tries to highlight outside the source string (avoids OutOfBounds).
        return (Some(SourceSpan::new(offset.into(), len)), None);
    }

    // `named_source_for_render` applies the per-half sanitization: byte-length-preserving
    // neutralization for the span-indexed source (PF-014), WIRE-mode \uXXXX escaping for
    // the single-line filename (a newline-bearing filename must not forge a line).
    (
        Some(SourceSpan::new(offset.into(), len)),
        Some(Arc::new(named_source_for_render(file, source))),
    )
}

/// All errors produced by the MDS compiler.
///
/// ## Display contract — sanitization split (CWE-150 / issue #176)
///
/// `MdsError` implements `std::fmt::Display` via `thiserror`.  The `Display`
/// output is the **raw, unsanitized** message string: it may contain C0/DEL/C1
/// control bytes from untrusted `.mds` source input.
///
/// | Method / path | Sanitized | Intended context |
/// |---|---|---|
/// | `e.to_string()` / `format!("{e}")` | **No** | Machine-readable pipelines, structured loggers |
/// | [`MdsError::display_sanitized()`] | **Yes** | Terminal output, user-facing render |
/// | [`MdsError::serialize()`]`.message` | **Yes** | JSON API / binding surfaces |
///
/// **Do not write `eprintln!("{e}")` or `e.to_string()` when the output goes
/// to a TTY or is embedded in a user-visible string without further escaping.**
/// Use [`MdsError::display_sanitized()`] instead to avoid terminal injection
/// (CWE-150).  All three published binding surfaces (napi, WASM, Python)
/// already use `serialize()` and are unaffected.
///
/// This contract governs **direct** rendering by a consumer of this crate.  The `mds`
/// CLI never renders an `MdsError` directly: it hands the `miette::Report` to
/// `eprint_error`, which escapes the message, help, and label text of every report it
/// prints.  See the boundary table in `mds-core/src/lint/diagnostic.rs` for the full set.
#[must_use]
#[non_exhaustive]
#[derive(Error, Debug, Diagnostic, Clone)]
pub enum MdsError {
    #[error("syntax error: {message}")]
    #[diagnostic(code(mds::syntax))]
    Syntax {
        message: String,
        #[label("syntax error occurred here")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("undefined variable '{name}'")]
    #[diagnostic(
        code(mds::undefined_var),
        help("define '{name}' in frontmatter or imports")
    )]
    UndefinedVariable {
        name: String,
        #[label("not defined")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("undefined function '{name}'")]
    #[diagnostic(
        code(mds::undefined_fn),
        help("define '{name}' with @define or import it")
    )]
    UndefinedFunction {
        name: String,
        #[label("not defined")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("arity mismatch for '{name}': {}, got {got}", format_arity(*expected_min, *expected_max))]
    #[diagnostic(
        code(mds::arity),
        help("check the call site — '{name}' requires a different number of arguments than were provided{signature_note}")
    )]
    ArityMismatch {
        name: String,
        expected_min: usize,
        expected_max: usize,
        got: usize,
        /// Formatted function signature for the help text (B3 — F7).
        ///
        /// When non-empty, miette appends it to the help text so the user sees
        /// the expected signature without having to look up the `@define`.
        ///
        /// Format: `"\nexpected: name(param1, param2 = default)"`.
        /// Empty string for built-in functions (their signatures live in docs).
        signature_note: String,
        #[label("wrong number of arguments")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("{message}")]
    #[diagnostic(code(mds::builtin))]
    BuiltinError {
        message: String,
        #[label("built-in function error")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("type error: expected array for @for loop, got {got}")]
    #[diagnostic(
        code(mds::type_error),
        help("@for loops require an array value; valid types are arrays (e.g. [1, 2, 3])")
    )]
    TypeError {
        got: String,
        #[label("not an array")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    /// Cross-type comparison (`string == number`, `boolean != null`, etc.).
    ///
    /// MDS refuses to silently coerce types in `==` / `!=` conditions.
    /// Pass string values with `--set-string KEY=VALUE` to keep them as
    /// strings, or use `@if x:` to test for truthiness without a comparison.
    #[error("type mismatch: cannot compare {lhs_type} with {rhs_type}")]
    #[diagnostic(
        code(mds::type_mismatch),
        help("left side is {lhs_type}, right side is {rhs_type}; compare against a {lhs_type} literal, use '@if x:' for truthiness, or pass a string with '--set-string KEY=VALUE'")
    )]
    TypeMismatch {
        lhs_type: String,
        rhs_type: String,
        #[label("cross-type comparison")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("circular import detected: {cycle}")]
    #[diagnostic(
        code(mds::circular_import),
        help("check your import graph for cycles; A imports B imports A is not allowed")
    )]
    CircularImport {
        cycle: String,
        #[label("import creates cycle here")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("file not found: {path}")]
    #[diagnostic(
        code(mds::file_not_found),
        help("check the file path and ensure the file exists")
    )]
    FileNotFound {
        path: String,
        #[label("imported here")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("import error: {message}")]
    #[diagnostic(code(mds::import))]
    ImportError {
        message: String,
        #[label("import error")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("name collision: '{name}' is already defined")]
    #[diagnostic(code(mds::name_collision))]
    NameCollision {
        name: String,
        #[label("collision here")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("not an MDS file: {path}")]
    #[diagnostic(
        code(mds::not_mds),
        help("use .mds extension or add 'type: mds' to frontmatter")
    )]
    NotMdsFile { path: String },

    #[error("{message}")]
    #[diagnostic(code(mds::io))]
    Io { message: String },

    #[error("resource limit exceeded: {message}")]
    #[diagnostic(code(mds::resource_limit))]
    ResourceLimit { message: String },

    #[error("YAML parse error: {message}")]
    #[diagnostic(code(mds::yaml))]
    YamlError { message: String },

    #[error("JSON parse error: {message}")]
    #[diagnostic(code(mds::json))]
    JsonError { message: String },

    /// Vars file exists but contains malformed JSON or a non-object top-level value.
    ///
    /// Distinct from `JsonError` (which covers other JSON sites) so the user gets
    /// the specific help text pointing to the correct fix. Exit code stays 1
    /// (content/semantic error, not a file-system error).
    #[error("invalid vars file: {message}")]
    #[diagnostic(
        code(mds::invalid_vars),
        help("vars file must be a JSON object mapping variable names to values")
    )]
    InvalidVars { message: String },

    #[error("recursion detected in function '{name}'")]
    #[diagnostic(
        code(mds::recursion),
        help("MDS does not support recursive functions; restructure using @for loops or multiple @define blocks")
    )]
    Recursion {
        name: String,
        #[label("recursive call here")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    #[error("export error: {message}")]
    #[diagnostic(code(mds::export))]
    ExportError {
        message: String,
        #[label("export error")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    /// Errors in template inheritance (`@extends` / `@block`).
    ///
    /// Used for child-only-blocks violations (3b), unknown-override (3c), and
    /// stray `@extends` directives detected at parse time.
    #[error("extends error: {message}")]
    #[diagnostic(code(mds::extends))]
    Extends {
        message: String,
        #[label("template inheritance error")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    /// Mixed content in a messages template: non-whitespace `Text` or `Interpolation`
    /// outside any `@message` block in a template that has `@message` blocks.
    #[error("mixed content: non-message content found outside @message blocks")]
    #[diagnostic(
        code(mds::mixed_content),
        help("move all text and interpolations inside @message blocks, or remove the @message blocks to use plain markdown mode")
    )]
    MixedContent {
        #[label("non-message content here")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<Arc<miette::NamedSource<String>>>,
    },

    /// Attempted to extract Markdown output from a Messages result.
    #[error("expected markdown output, but template produced messages")]
    #[diagnostic(code(mds::expected_markdown))]
    ExpectedMarkdown,

    /// Attempted to extract Messages output from a Markdown result.
    #[error("expected messages output, but template produced markdown")]
    #[diagnostic(code(mds::expected_messages))]
    ExpectedMessages,

    /// The formatter's rewritten source failed the compile-equivalence safety
    /// gate: either the formatted source failed to compile when the original
    /// did, or the two compiled to different output. This signals a formatter
    /// bug, not a problem with the input template — the CLI must not write the
    /// file when this occurs.
    #[error("formatter produced non-equivalent output: {message}")]
    #[diagnostic(
        code(mds::formatter_invariant),
        help("this indicates a bug in `mds fmt` itself; please file an issue")
    )]
    FormatterInvariant { message: String },
}

impl MdsError {
    pub(crate) fn syntax(message: impl Into<String>) -> Self {
        MdsError::Syntax {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn syntax_at(
        message: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::Syntax {
            message: message.into(),
            span,
            src,
        }
    }

    pub(crate) fn undefined_var(name: impl Into<String>) -> Self {
        MdsError::UndefinedVariable {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn undefined_var_at(
        name: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::UndefinedVariable {
            name: name.into(),
            span,
            src,
        }
    }

    pub(crate) fn undefined_fn(name: impl Into<String>) -> Self {
        MdsError::UndefinedFunction {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn undefined_fn_at(
        name: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::UndefinedFunction {
            name: name.into(),
            span,
            src,
        }
    }

    pub(crate) fn arity(
        name: impl Into<String>,
        expected_min: usize,
        expected_max: usize,
        got: usize,
        signature: Option<String>,
    ) -> Self {
        let signature_note = signature
            .map(|s| format!("\nexpected: {s}"))
            .unwrap_or_default();
        MdsError::ArityMismatch {
            name: name.into(),
            expected_min,
            expected_max,
            got,
            signature_note,
            span: None,
            src: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn arity_at(
        name: impl Into<String>,
        expected_min: usize,
        expected_max: usize,
        got: usize,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
        signature: Option<String>,
    ) -> Self {
        let signature_note = signature
            .map(|s| format!("\nexpected: {s}"))
            .unwrap_or_default();
        let (span, src) = at(file, source, offset, len);
        MdsError::ArityMismatch {
            name: name.into(),
            expected_min,
            expected_max,
            got,
            signature_note,
            span,
            src,
        }
    }

    pub(crate) fn builtin_error(msg: impl Into<String>) -> Self {
        MdsError::BuiltinError {
            message: msg.into(),
            span: None,
            src: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn builtin_error_at(
        msg: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::BuiltinError {
            message: msg.into(),
            span,
            src,
        }
    }

    pub(crate) fn type_error(got: impl Into<String>) -> Self {
        MdsError::TypeError {
            got: got.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn type_error_at(
        got: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::TypeError {
            got: got.into(),
            span,
            src,
        }
    }

    pub(crate) fn type_mismatch(lhs_type: impl Into<String>, rhs_type: impl Into<String>) -> Self {
        MdsError::TypeMismatch {
            lhs_type: lhs_type.into(),
            rhs_type: rhs_type.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn type_mismatch_at(
        lhs_type: impl Into<String>,
        rhs_type: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::TypeMismatch {
            lhs_type: lhs_type.into(),
            rhs_type: rhs_type.into(),
            span,
            src,
        }
    }

    pub(crate) fn name_collision(name: impl Into<String>) -> Self {
        MdsError::NameCollision {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn name_collision_at(
        name: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::NameCollision {
            name: name.into(),
            span,
            src,
        }
    }

    pub(crate) fn recursion(name: impl Into<String>) -> Self {
        MdsError::Recursion {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn recursion_at(
        name: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::Recursion {
            name: name.into(),
            span,
            src,
        }
    }

    pub(crate) fn file_not_found(path: impl Into<String>) -> Self {
        MdsError::FileNotFound {
            path: path.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn file_not_found_at(
        path: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::FileNotFound {
            path: path.into(),
            span,
            src,
        }
    }

    pub(crate) fn import_error(message: impl Into<String>) -> Self {
        MdsError::ImportError {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn import_error_at(
        message: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::ImportError {
            message: message.into(),
            span,
            src,
        }
    }

    pub(crate) fn circular_import(cycle: impl Into<String>) -> Self {
        MdsError::CircularImport {
            cycle: cycle.into(),
            span: None,
            src: None,
        }
    }

    pub(crate) fn circular_import_at(
        cycle: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::CircularImport {
            cycle: cycle.into(),
            span,
            src,
        }
    }

    pub(crate) fn export_error(message: impl Into<String>) -> Self {
        MdsError::ExportError {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn export_error_at(
        message: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::ExportError {
            message: message.into(),
            span,
            src,
        }
    }

    pub(crate) fn resource_limit(message: impl Into<String>) -> Self {
        MdsError::ResourceLimit {
            message: message.into(),
        }
    }

    /// Upgrade a spanless `Syntax` error with a span; no-op for other variants or if
    /// the error already carries a span (B2 — spanless syntax-error upgrades at call sites
    /// that hold the directive offset but cannot thread it into the low-level helper).
    ///
    /// Passing `file/source/offset/len` that are out-of-bounds degrades gracefully (the
    /// `at()` helper drops `src` and keeps only the raw offset in `span`, so miette
    /// never renders an `OutOfBounds`).
    pub(crate) fn or_span(self, file: &str, source: &str, offset: usize, len: usize) -> Self {
        match self {
            MdsError::Syntax {
                span: None,
                src: None,
                message,
            } => Self::syntax_at(message, file, source, offset, len),
            other => other,
        }
    }

    pub(crate) fn io(message: impl Into<String>) -> Self {
        MdsError::Io {
            message: message.into(),
        }
    }

    pub(crate) fn yaml_error(message: impl Into<String>) -> Self {
        MdsError::YamlError {
            message: message.into(),
        }
    }

    pub(crate) fn json_error(message: impl Into<String>) -> Self {
        MdsError::JsonError {
            message: message.into(),
        }
    }

    pub(crate) fn invalid_vars(message: impl Into<String>) -> Self {
        MdsError::InvalidVars {
            message: message.into(),
        }
    }

    pub(crate) fn extends_error_at(
        message: impl Into<String>,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::Extends {
            message: message.into(),
            span,
            src,
        }
    }

    pub(crate) fn not_mds_file(path: impl Into<String>) -> Self {
        MdsError::NotMdsFile { path: path.into() }
    }

    /// Construct a `FormatterInvariant` error, signaling that the formatter's
    /// rewritten source failed the compile-equivalence safety gate.
    pub(crate) fn formatter_invariant(message: impl Into<String>) -> Self {
        MdsError::FormatterInvariant {
            message: message.into(),
        }
    }

    /// Construct a `MixedContent` error whose span points at the offending
    /// top-level prose / interpolation.
    ///
    /// Unlike most evaluator-path errors (which carry no span because the
    /// evaluator runs without source context — see the arity span-divergence
    /// note in `evaluator.rs`), `MixedContent` is a *structural* error about the
    /// template's shape: the offending node's byte offset is known statically
    /// from the AST, so the diagnostic underlines the orphan content (ADR-022).
    ///
    /// `offset`/`len` index into `source`; the shared [`at`] guard drops `src`
    /// (keeping raw offset/length for `serialize()`) if they fall out of bounds,
    /// so a cross-source offset can never cause a miette `OutOfBounds` render.
    pub(crate) fn mixed_content_at(file: &str, source: &str, offset: usize, len: usize) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::MixedContent { span, src }
    }

    /// Serialize this error into a [`SerializedError`] suitable for JSON output.
    ///
    /// - `code` is extracted via [`miette::Diagnostic::code`] (drift-proof).
    /// - `message` is the `Display` representation of the error.
    /// - `help` is extracted via [`miette::Diagnostic::help`] (drift-proof).
    /// - `span` is populated for variants that carry `(span, src)` fields.
    ///   If `span` is `Some` but `src` is `None`, or if the offset exceeds the
    ///   source length, `line` and `column` are `None` but `offset`/`length`
    ///   still reflect the raw `SourceSpan` values.
    pub fn serialize(&self) -> SerializedError {
        let code = Diagnostic::code(self)
            .map(|c| c.to_string())
            .unwrap_or_default();
        // WIRE mode: this value is consumed as JSON / as a binding error object, so
        // `\n` is escaped too — an embedded newline would let a hostile message forge
        // an extra line in any line-oriented consumer.  The HUMAN counterpart is
        // `display_sanitized()`, which keeps newlines raw for terminal rendering.
        let message = sanitize_control_chars_wire(&self.to_string()).into_owned();
        let help = Diagnostic::help(self)
            .map(|h| sanitize_control_chars_wire(&h.to_string()).into_owned());

        // Extract (span, src) from each span-bearing variant; no-span variants
        // use the wildcard arm and produce span: None.
        let serialized_span: Option<SerializedSpan> = match self {
            MdsError::Syntax { span, src, .. }
            | MdsError::UndefinedVariable { span, src, .. }
            | MdsError::UndefinedFunction { span, src, .. }
            | MdsError::ArityMismatch { span, src, .. }
            | MdsError::TypeError { span, src, .. }
            | MdsError::TypeMismatch { span, src, .. }
            | MdsError::CircularImport { span, src, .. }
            | MdsError::FileNotFound { span, src, .. }
            | MdsError::ImportError { span, src, .. }
            | MdsError::NameCollision { span, src, .. }
            | MdsError::Recursion { span, src, .. }
            | MdsError::ExportError { span, src, .. }
            | MdsError::BuiltinError { span, src, .. }
            | MdsError::Extends { span, src, .. }
            | MdsError::MixedContent { span, src } => {
                span.as_ref().map(|ss| {
                    let offset = ss.offset();
                    let length = ss.len();
                    let (line, column) = src
                        .as_ref()
                        .and_then(|named_src| {
                            // NamedSource<String> implements SourceCode; inner() gives &String.
                            compute_line_column(named_src.inner(), offset)
                        })
                        .map_or((None, None), |(l, c)| (Some(l), Some(c)));
                    SerializedSpan {
                        offset,
                        length,
                        line,
                        column,
                    }
                })
            }
            MdsError::NotMdsFile { .. }
            | MdsError::Io { .. }
            | MdsError::ResourceLimit { .. }
            | MdsError::YamlError { .. }
            | MdsError::JsonError { .. }
            | MdsError::InvalidVars { .. }
            | MdsError::ExpectedMarkdown
            | MdsError::ExpectedMessages
            | MdsError::FormatterInvariant { .. } => None,
        };

        SerializedError {
            code,
            message,
            help,
            span: serialized_span,
        }
    }

    /// Return a terminal-safe, sanitized version of this error's `Display` text.
    ///
    /// The escaped class is the one spec §7.5 defines: C0 (U+0000–U+001F) with `\t`
    /// (U+0009) as the sole exemption, DEL (U+007F), C1 (U+0080–U+009F), all twelve
    /// Unicode bidi controls (U+061C, U+200E/U+200F, U+202A–U+202E, U+2066–U+2069),
    /// U+2028/U+2029, and U+FEFF.  Each is replaced by its six-character `\uXXXX` escape
    /// literal.
    ///
    /// This is **HUMAN mode**, so `\n` — which *is* in the class — is preserved, keeping
    /// multi-line miette renders readable.  Whether `\n` is escaped is a property of the
    /// mode, not of the class; describing the class as "C0 except `\t` and `\n`" folds
    /// the two together and is exactly the framing spec §7.5 retired.  That mode choice
    /// is the one deliberate difference from [`MdsError::serialize`], which is a
    /// machine-readable (wire) boundary and escapes `\n` as well.  `\t` is preserved in
    /// both modes.
    ///
    /// The escaping is one-way: see [`mds::sanitize_control_chars`][crate::sanitize_control_chars]
    /// — consumers must not un-escape `\uXXXX` sequences back into bytes.
    ///
    /// Use this method — not `e.to_string()` / `eprintln!("{e}")` — whenever the
    /// string will be written to a TTY or embedded in a user-visible context
    /// without further escaping.  See the [type-level doc][MdsError] for the full
    /// Display-contract table.
    ///
    /// # Audience
    ///
    /// This is an affordance for **downstream Rust consumers** of the published crate
    /// who render an `MdsError` themselves.  It is deliberately not on the `mds` CLI's
    /// render path: the CLI hands `miette::Report`s to `eprint_error`, which sanitizes
    /// the message, help, and label text of *every* report — including CLI-authored
    /// `miette::miette!()` errors that are not `MdsError`s at all, and so could never
    /// be covered by this method.  Routing the CLI through here instead would leave
    /// that second family unescaped (PF-004).
    #[must_use]
    pub fn display_sanitized(&self) -> String {
        sanitize_control_chars(&self.to_string()).into_owned()
    }

    /// Return `true` when this error's embedded `NamedSource` carries the stdin
    /// analysis sentinel `"<source>"` — the name that `resolve_source_intrinsic`
    /// assigns to `ctx.file_str` for string-source inputs.
    ///
    /// The CLI uses this at its output boundary to decide whether to relabel the
    /// embedded source name as `"<stdin>"`.  Only errors produced by a string-source
    /// analysis carry that sentinel; errors from imported files carry the real file
    /// path and must not be relabelled (PF-012 — in-bounds-but-wrong caret class;
    /// the AD-211-5 ruling only authorised relabelling stdin's OWN source identity).
    ///
    /// Returns `false` when the error has no embedded source at all (e.g.
    /// `MdsError::Io`), because there is nothing to relabel in that case.
    #[must_use]
    pub fn source_label_is_stdin_sentinel(&self) -> bool {
        // "<source>" is resolver::SOURCE_LABEL — the value assigned to ctx.file_str
        // by resolve_source_intrinsic.  Defined as a local const so error.rs needs
        // no coupling to resolver.rs's crate-private symbol.
        const SOURCE_LABEL: &str = "<source>";
        let src = match self {
            MdsError::Syntax { src, .. }
            | MdsError::UndefinedVariable { src, .. }
            | MdsError::UndefinedFunction { src, .. }
            | MdsError::ArityMismatch { src, .. }
            | MdsError::TypeError { src, .. }
            | MdsError::TypeMismatch { src, .. }
            | MdsError::CircularImport { src, .. }
            | MdsError::FileNotFound { src, .. }
            | MdsError::ImportError { src, .. }
            | MdsError::NameCollision { src, .. }
            | MdsError::Recursion { src, .. }
            | MdsError::ExportError { src, .. }
            | MdsError::BuiltinError { src, .. }
            | MdsError::Extends { src, .. }
            | MdsError::MixedContent { src, .. } => src.as_deref(),
            MdsError::NotMdsFile { .. }
            | MdsError::Io { .. }
            | MdsError::ResourceLimit { .. }
            | MdsError::YamlError { .. }
            | MdsError::JsonError { .. }
            | MdsError::InvalidVars { .. }
            | MdsError::ExpectedMarkdown
            | MdsError::ExpectedMessages
            | MdsError::FormatterInvariant { .. } => None,
        };
        src.is_some_and(|ns| ns.name() == SOURCE_LABEL)
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
