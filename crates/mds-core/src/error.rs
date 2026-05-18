use std::sync::Arc;

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

// ── Serializable error types ──────────────────────────────────────────────────

/// A serializable representation of a source span.
///
/// Offsets and lengths are in bytes from the start of the source string,
/// matching `miette::SourceSpan`. Line and column are 1-indexed byte offsets
/// from the start of the respective line (NOT UTF-16 code units).
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

/// Compute the 1-indexed line and column (byte-based) for a byte offset in source.
///
/// Returns `None` if `offset` exceeds `source.len()`. Both line and column are
/// 1-indexed: the very first byte is (1, 1).
///
/// Column counts bytes from the start of the current line (NOT UTF-16 code units
/// and NOT Unicode scalar values). This matches the convention used by most
/// command-line tools and language servers when operating in byte mode.
fn compute_line_column(source: &str, offset: usize) -> Option<(usize, usize)> {
    if offset > source.len() {
        return None;
    }
    let mut line = 1usize;
    let mut col = 1usize;
    for byte in source[..offset].bytes() {
        if byte == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    Some((line, col))
}

/// Build the `(span, src)` pair shared by all `_at` constructors.
fn at(
    file: &str,
    source: &str,
    offset: usize,
    len: usize,
) -> (Option<SourceSpan>, Option<Arc<miette::NamedSource<String>>>) {
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

    #[error("arity mismatch for '{name}': expected {expected} {}, got {got}", if *expected == 1 { "argument" } else { "arguments" })]
    #[diagnostic(code(mds::arity))]
    ArityMismatch {
        name: String,
        expected: usize,
        got: usize,
        #[label("wrong number of arguments")]
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

    pub(crate) fn arity(name: impl Into<String>, expected: usize, got: usize) -> Self {
        MdsError::ArityMismatch {
            name: name.into(),
            expected,
            got,
            span: None,
            src: None,
        }
    }

    pub(crate) fn arity_at(
        name: impl Into<String>,
        expected: usize,
        got: usize,
        file: &str,
        source: &str,
        offset: usize,
        len: usize,
    ) -> Self {
        let (span, src) = at(file, source, offset, len);
        MdsError::ArityMismatch {
            name: name.into(),
            expected,
            got,
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

    pub(crate) fn not_mds_file(path: impl Into<String>) -> Self {
        MdsError::NotMdsFile { path: path.into() }
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
            | MdsError::ExportError { span, src, .. } => {
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
                    SerializedSpan { offset, length, line, column }
                })
            }
            MdsError::NotMdsFile { .. }
            | MdsError::Io { .. }
            | MdsError::ResourceLimit { .. }
            | MdsError::YamlError { .. }
            | MdsError::JsonError { .. } => None,
        };

        SerializedError { code, message, help, span: serialized_span }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── compute_line_column ───────────────────────────────────────────────────

    #[test]
    fn line_col_first_byte() {
        // Offset 0 in any source is (1, 1).
        assert_eq!(compute_line_column("hello", 0), Some((1, 1)));
    }

    #[test]
    fn line_col_mid_line() {
        // "hello world", offset 6 → 'w' → line 1, column 7.
        assert_eq!(compute_line_column("hello world", 6), Some((1, 7)));
    }

    #[test]
    fn line_col_second_line() {
        // "line1\nline2" — '\n' at offset 5, 'l' of "line2" at offset 6.
        assert_eq!(compute_line_column("line1\nline2", 6), Some((2, 1)));
    }

    #[test]
    fn line_col_third_line() {
        // "a\nb\nc" — 'a'=0, '\n'=1, 'b'=2, '\n'=3, 'c'=4 → offset 4 = (3,1).
        assert_eq!(compute_line_column("a\nb\nc", 4), Some((3, 1)));
    }

    #[test]
    fn line_col_multibyte_utf8() {
        // "café\nworld": 'c'=0,'a'=1,'f'=2,'é'=3+4(2 bytes),'\n'=5,'w'=6
        // offset 6 → 'w' on line 2, col 1.
        let src = "café\nworld";
        assert_eq!(src.as_bytes()[6], b'w');
        assert_eq!(compute_line_column(src, 6), Some((2, 1)));
    }

    #[test]
    fn line_col_at_newline() {
        // "ab\ncd" — '\n' is at offset 2, on line 1. col = 3 (after 'a','b').
        assert_eq!(compute_line_column("ab\ncd", 2), Some((1, 3)));
    }

    #[test]
    fn line_col_out_of_bounds() {
        // Offset beyond source length must return None.
        assert_eq!(compute_line_column("short", 100), None);
    }

    #[test]
    fn line_col_empty_source() {
        // Empty source, offset 0 is valid (at the very start).
        assert_eq!(compute_line_column("", 0), Some((1, 1)));
    }

    // ── MdsError::serialize — per-variant tests ───────────────────────────────

    #[test]
    fn serialize_syntax_with_span() {
        let e = MdsError::syntax_at("unexpected token", "file.mds", "hello world", 0, 5);
        let s = e.serialize();
        assert_eq!(s.code, "mds::syntax");
        assert_eq!(s.help, None);
        let span = s.span.expect("span should be Some");
        assert_eq!(span.offset, 0);
        assert_eq!(span.length, 5);
        assert_eq!(span.line, Some(1));
        assert_eq!(span.column, Some(1));
    }

    #[test]
    fn serialize_syntax_without_span() {
        let e = MdsError::syntax("unexpected token");
        let s = e.serialize();
        assert_eq!(s.code, "mds::syntax");
        assert_eq!(s.help, None);
        assert_eq!(s.span, None);
    }

    #[test]
    fn serialize_undefined_var_with_span() {
        // "{{ x }}" — 'x' is at offset 3, length 1.
        let e = MdsError::undefined_var_at("x", "f.mds", "{{ x }}", 3, 1);
        let s = e.serialize();
        assert_eq!(s.code, "mds::undefined_var");
        let help = s.help.expect("UndefinedVariable should have help text");
        assert!(
            help.contains("define"),
            "help should mention 'define', got: {help}"
        );
        let span = s.span.expect("span should be Some");
        assert_eq!(span.offset, 3);
        assert_eq!(span.length, 1);
        assert_eq!(span.line, Some(1));
        assert_eq!(span.column, Some(4)); // bytes 0,1,2 → col 4
    }

    #[test]
    fn serialize_arity_code() {
        let e = MdsError::arity_at("greet", 1, 3, "f.mds", "source text", 0, 6);
        let s = e.serialize();
        assert_eq!(s.code, "mds::arity");
        assert!(s.span.is_some(), "ArityMismatch with span should produce Some(span)");
    }

    #[test]
    fn serialize_type_error_with_help() {
        let e = MdsError::type_error_at("string", "f.mds", "source", 0, 6);
        let s = e.serialize();
        assert_eq!(s.code, "mds::type_error");
        assert!(
            s.help.is_some(),
            "TypeError should have help text"
        );
        assert!(s.span.is_some());
    }

    #[test]
    fn serialize_circular_import() {
        let e = MdsError::circular_import_at("a->b->a", "f.mds", "source", 0, 1);
        let s = e.serialize();
        assert_eq!(s.code, "mds::circular_import");
        assert!(
            s.help.is_some(),
            "CircularImport should have help text"
        );
        assert!(s.span.is_some());
    }

    #[test]
    fn serialize_file_not_found() {
        let e = MdsError::file_not_found_at("missing.mds", "f.mds", "source", 0, 1);
        let s = e.serialize();
        assert_eq!(s.code, "mds::file_not_found");
        assert!(
            s.help.is_some(),
            "FileNotFound should have help text"
        );
        assert!(s.span.is_some());
    }

    #[test]
    fn serialize_recursion() {
        let e = MdsError::recursion_at("fib", "f.mds", "source text", 0, 3);
        let s = e.serialize();
        assert_eq!(s.code, "mds::recursion");
        assert!(
            s.help.is_some(),
            "Recursion should have help text"
        );
        assert!(s.span.is_some());
    }

    #[test]
    fn serialize_not_mds_no_span() {
        let e = MdsError::not_mds_file("readme.txt");
        let s = e.serialize();
        assert_eq!(s.code, "mds::not_mds");
        assert!(
            s.help.is_some(),
            "NotMdsFile should have help text"
        );
        assert_eq!(s.span, None);
    }

    #[test]
    fn serialize_io_no_span() {
        let e = MdsError::io("permission denied");
        let s = e.serialize();
        assert_eq!(s.code, "mds::io");
        assert_eq!(s.help, None);
        assert_eq!(s.span, None);
    }

    #[test]
    fn serialize_resource_limit_no_span() {
        let e = MdsError::resource_limit("too many iterations");
        let s = e.serialize();
        assert_eq!(s.code, "mds::resource_limit");
        assert_eq!(s.help, None);
        assert_eq!(s.span, None);
    }

    #[test]
    fn serialize_yaml_error_no_span() {
        let e = MdsError::yaml_error("unexpected indent");
        let s = e.serialize();
        assert_eq!(s.code, "mds::yaml");
        assert_eq!(s.help, None);
        assert_eq!(s.span, None);
    }

    #[test]
    fn serialize_json_error_no_span() {
        let e = MdsError::json_error("trailing comma");
        let s = e.serialize();
        assert_eq!(s.code, "mds::json");
        assert_eq!(s.help, None);
        assert_eq!(s.span, None);
    }

    // ── JSON serialization tests ──────────────────────────────────────────────

    #[test]
    fn serialized_error_to_json_with_span() {
        let e = MdsError::syntax_at("bad token", "file.mds", "hello world", 6, 5);
        let s = e.serialize();
        let json = serde_json::to_string(&s).expect("serialization should succeed");
        // Verify JSON structure contains expected keys.
        assert!(json.contains("\"code\""), "JSON should contain 'code' key");
        assert!(json.contains("\"message\""), "JSON should contain 'message' key");
        assert!(json.contains("\"span\""), "JSON should contain 'span' key");
        assert!(json.contains("\"offset\""), "JSON should contain 'offset' key");
        assert!(json.contains("\"length\""), "JSON should contain 'length' key");
        assert!(json.contains("\"line\""), "JSON should contain 'line' key");
        assert!(json.contains("\"column\""), "JSON should contain 'column' key");
        // Verify values are correct.
        let v: serde_json::Value =
            serde_json::from_str(&json).expect("JSON should parse back");
        assert_eq!(v["code"], "mds::syntax");
        assert_eq!(v["span"]["offset"], 6);
        assert_eq!(v["span"]["length"], 5);
        assert_eq!(v["span"]["line"], 1);
        assert_eq!(v["span"]["column"], 7); // offset 6 in "hello world" → col 7
    }

    #[test]
    fn serialized_error_to_json_null_span() {
        let e = MdsError::io("disk full");
        let s = e.serialize();
        let json = serde_json::to_string(&s).expect("serialization should succeed");
        let v: serde_json::Value =
            serde_json::from_str(&json).expect("JSON should parse back");
        assert!(v["span"].is_null(), "span should be null in JSON when None");
    }

    // ── Display output ────────────────────────────────────────────────────────

    #[test]
    fn syntax_display_contains_message() {
        let e = MdsError::syntax("unexpected token '}'");
        assert!(e.to_string().contains("unexpected token '}'"));
    }

    #[test]
    fn undefined_var_display_contains_name() {
        let e = MdsError::undefined_var("my_var");
        assert!(e.to_string().contains("my_var"));
    }

    #[test]
    fn undefined_fn_display_contains_name() {
        let e = MdsError::undefined_fn("my_fn");
        assert!(e.to_string().contains("my_fn"));
    }

    #[test]
    fn arity_display_contains_name_and_counts() {
        let e = MdsError::arity("greet", 1, 3);
        let msg = e.to_string();
        assert!(msg.contains("greet"));
        assert!(msg.contains('1'));
        assert!(msg.contains('3'));
    }

    #[test]
    fn arity_display_singular_argument() {
        let e = MdsError::arity("f", 1, 0);
        assert!(
            e.to_string().contains("argument"),
            "should say 'argument' not 'arguments' for 1"
        );
    }

    #[test]
    fn arity_display_plural_arguments() {
        let e = MdsError::arity("f", 2, 0);
        assert!(
            e.to_string().contains("arguments"),
            "should say 'arguments' for 2"
        );
    }

    #[test]
    fn type_error_display_contains_got() {
        let e = MdsError::type_error("string");
        assert!(e.to_string().contains("string"));
    }

    #[test]
    fn circular_import_display_contains_cycle() {
        let e = MdsError::circular_import("a -> b -> a");
        assert!(e.to_string().contains("a -> b -> a"));
    }

    #[test]
    fn file_not_found_display_contains_path() {
        let e = MdsError::file_not_found("foo/bar.mds");
        assert!(e.to_string().contains("foo/bar.mds"));
    }

    #[test]
    fn recursion_display_contains_name() {
        let e = MdsError::recursion("fib");
        assert!(e.to_string().contains("fib"));
    }

    #[test]
    fn io_display_contains_message() {
        let e = MdsError::io("permission denied");
        assert!(e.to_string().contains("permission denied"));
    }

    #[test]
    fn yaml_error_display_contains_message() {
        let e = MdsError::yaml_error("unexpected indent");
        assert!(e.to_string().contains("unexpected indent"));
    }

    #[test]
    fn json_error_display_contains_message() {
        let e = MdsError::json_error("trailing comma");
        assert!(e.to_string().contains("trailing comma"));
    }

    #[test]
    fn not_mds_file_display_contains_path() {
        let e = MdsError::not_mds_file("readme.txt");
        assert!(e.to_string().contains("readme.txt"));
    }

    #[test]
    fn resource_limit_display_contains_message() {
        let e = MdsError::resource_limit("too many iterations");
        assert!(e.to_string().contains("too many iterations"));
    }

    // ── Span propagation via _at constructors ─────────────────────────────────

    #[test]
    fn syntax_at_populates_span_and_src() {
        let e = MdsError::syntax_at("bad token", "file.mds", "hello world", 0, 5);
        match e {
            MdsError::Syntax { span, src, .. } => {
                assert!(span.is_some(), "span should be populated");
                assert!(src.is_some(), "src should be populated");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn undefined_var_at_populates_span() {
        let e = MdsError::undefined_var_at("x", "f.mds", "{{ x }}", 3, 1);
        match e {
            MdsError::UndefinedVariable { span, .. } => {
                assert!(span.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn type_error_at_populates_span() {
        let e = MdsError::type_error_at("string", "f.mds", "source", 0, 6);
        match e {
            MdsError::TypeError { span, .. } => {
                assert!(span.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn recursion_at_populates_span() {
        let e = MdsError::recursion_at("fib", "f.mds", "source", 0, 3);
        match e {
            MdsError::Recursion { span, .. } => {
                assert!(span.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn circular_import_at_populates_span() {
        let e = MdsError::circular_import_at("a->b->a", "f.mds", "source", 0, 1);
        match e {
            MdsError::CircularImport { span, .. } => {
                assert!(span.is_some());
            }
            _ => panic!("wrong variant"),
        }
    }

    // ── No-span constructors leave span as None ───────────────────────────────

    #[test]
    fn syntax_without_at_has_no_span() {
        let e = MdsError::syntax("msg");
        match e {
            MdsError::Syntax { span, src, .. } => {
                assert!(span.is_none());
                assert!(src.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    // ── I3: serialize() tests for UndefinedFunction, ImportError, NameCollision ─

    #[test]
    fn serialize_undefined_fn_with_span() {
        // "{{ greet() }}" — 'g' of "greet" is at offset 3, length 5.
        let e = MdsError::undefined_fn_at("greet", "f.mds", "{{ greet() }}", 3, 5);
        let s = e.serialize();
        assert_eq!(s.code, "mds::undefined_fn");
        let help = s.help.expect("UndefinedFunction should have help text");
        assert!(
            help.contains("define"),
            "help should mention 'define', got: {help}"
        );
        let span = s.span.expect("span should be Some");
        assert_eq!(span.offset, 3);
        assert_eq!(span.length, 5);
        assert_eq!(span.line, Some(1));
        assert_eq!(span.column, Some(4)); // bytes 0,1,2 → col 4
    }

    #[test]
    fn serialize_import_error_with_span() {
        // "import x" — 'x' at offset 7, length 1.
        let e = MdsError::import_error_at("could not resolve", "f.mds", "import x", 7, 1);
        let s = e.serialize();
        assert_eq!(s.code, "mds::import");
        assert_eq!(s.help, None, "ImportError has no help text");
        let span = s.span.expect("span should be Some");
        assert_eq!(span.offset, 7);
        assert_eq!(span.length, 1);
        assert_eq!(span.line, Some(1));
        assert_eq!(span.column, Some(8)); // bytes 0..6 → col 8
    }

    #[test]
    fn serialize_name_collision_with_span() {
        // "foo" redefined at offset 0, length 3.
        let e = MdsError::name_collision_at("foo", "f.mds", "foo = 1", 0, 3);
        let s = e.serialize();
        assert_eq!(s.code, "mds::name_collision");
        assert_eq!(s.help, None, "NameCollision has no help text");
        let span = s.span.expect("span should be Some");
        assert_eq!(span.offset, 0);
        assert_eq!(span.length, 3);
        assert_eq!(span.line, Some(1));
        assert_eq!(span.column, Some(1));
    }

    // ── I4: serialize() test for ExportError ──────────────────────────────────

    #[test]
    fn serialize_export_error_with_span() {
        // Export statement at offset 0, length 6.
        let e = MdsError::export_error_at("invalid export target", "f.mds", "export foo", 0, 6);
        let s = e.serialize();
        assert_eq!(s.code, "mds::export");
        assert_eq!(s.help, None, "ExportError has no help text");
        let span = s.span.expect("span should be Some");
        assert_eq!(span.offset, 0);
        assert_eq!(span.length, 6);
        assert_eq!(span.line, Some(1));
        assert_eq!(span.column, Some(1));
    }

    // ── I5: span=Some but src=None produces offset/length but not line/column ──

    #[test]
    fn serialize_span_some_src_none_omits_line_column() {
        // Construct Syntax directly with span set but src intentionally None,
        // matching the documented behavior in serialize()'s doc comment.
        let e = MdsError::Syntax {
            message: "bad token".to_string(),
            span: Some(miette::SourceSpan::new(10.into(), 3)),
            src: None,
        };
        let s = e.serialize();
        assert_eq!(s.code, "mds::syntax");
        let span = s.span.expect("span should be Some when SourceSpan is set");
        assert_eq!(span.offset, 10);
        assert_eq!(span.length, 3);
        // Without src there is no source text to compute line/column from.
        assert_eq!(span.line, None, "line should be None when src is None");
        assert_eq!(span.column, None, "column should be None when src is None");
    }

    // ── I6: compute_line_column at offset == source.len() (boundary) ──────────

    #[test]
    fn line_col_at_end_of_source() {
        // "abc" has len 3. Offset 3 is one past the last byte — still valid
        // (offset == len is the exclusive-end sentinel, not out-of-bounds).
        assert_eq!(compute_line_column("abc", 3), Some((1, 4)));
    }
}
