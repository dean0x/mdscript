use std::sync::Arc;

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

// ── Serializable error types ──────────────────────────────────────────────────

/// A serializable representation of a source span.
///
/// `offset` and `length` are in bytes from the start of the source string,
/// matching `miette::SourceSpan`. `line` is 1-indexed; `column` is the
/// 1-indexed character position (Unicode scalar values) from the start of the
/// line — NOT a byte offset and NOT UTF-16 code units.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SerializedSpan {
    pub offset: usize,
    pub length: usize,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

/// A serializable, `serde`-friendly representation of an [`MdsError`].
///
/// Suitable for embedding in JSON API responses or structured log output.
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

    (
        Some(SourceSpan::new(offset.into(), len)),
        Some(Arc::new(miette::NamedSource::new(file, source.to_string()))),
    )
}

/// All errors produced by the MDS compiler.
#[must_use]
#[non_exhaustive]
#[derive(Error, Debug, Diagnostic, Clone)]
pub enum MdsError {
    #[error("syntax error: {message}")]
    #[diagnostic(code(mds::syntax))]
    Syntax {
        message: String,
        #[label("{message}")]
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
    #[diagnostic(code(mds::arity))]
    ArityMismatch {
        name: String,
        expected_min: usize,
        expected_max: usize,
        got: usize,
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
    ) -> Self {
        MdsError::ArityMismatch {
            name: name.into(),
            expected_min,
            expected_max,
            got,
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
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::ArityMismatch {
            name: name.into(),
            expected_min,
            expected_max,
            got,
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

    pub(crate) fn mixed_content() -> Self {
        MdsError::MixedContent {
            span: None,
            src: None,
        }
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
        let message = self.to_string();
        let help = Diagnostic::help(self).map(|h| h.to_string());

        // Extract (span, src) from each span-bearing variant; no-span variants
        // use the wildcard arm and produce span: None.
        let serialized_span: Option<SerializedSpan> = match self {
            MdsError::Syntax { span, src, .. }
            | MdsError::UndefinedVariable { span, src, .. }
            | MdsError::UndefinedFunction { span, src, .. }
            | MdsError::ArityMismatch { span, src, .. }
            | MdsError::TypeError { span, src, .. }
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
            | MdsError::ExpectedMarkdown
            | MdsError::ExpectedMessages => None,
        };

        SerializedError {
            code,
            message,
            help,
            span: serialized_span,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
