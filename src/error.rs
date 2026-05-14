use std::sync::Arc;

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

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
    pub fn syntax(message: impl Into<String>) -> Self {
        MdsError::Syntax {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    pub fn syntax_at(
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

    pub fn undefined_var(name: impl Into<String>) -> Self {
        MdsError::UndefinedVariable {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub fn undefined_var_at(
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

    pub fn undefined_fn(name: impl Into<String>) -> Self {
        MdsError::UndefinedFunction {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub fn undefined_fn_at(
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

    pub fn arity(name: impl Into<String>, expected: usize, got: usize) -> Self {
        MdsError::ArityMismatch {
            name: name.into(),
            expected,
            got,
            span: None,
            src: None,
        }
    }

    pub fn arity_at(
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

    pub fn type_error(got: impl Into<String>) -> Self {
        MdsError::TypeError {
            got: got.into(),
            span: None,
            src: None,
        }
    }

    pub fn type_error_at(
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

    pub fn name_collision(name: impl Into<String>) -> Self {
        MdsError::NameCollision {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub fn name_collision_at(
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

    pub fn recursion(name: impl Into<String>) -> Self {
        MdsError::Recursion {
            name: name.into(),
            span: None,
            src: None,
        }
    }

    pub fn recursion_at(
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

    pub fn file_not_found(path: impl Into<String>) -> Self {
        MdsError::FileNotFound {
            path: path.into(),
            span: None,
            src: None,
        }
    }

    pub fn file_not_found_at(
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

    pub fn import_error(message: impl Into<String>) -> Self {
        MdsError::ImportError {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    pub fn import_error_at(
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

    pub fn circular_import(cycle: impl Into<String>) -> Self {
        MdsError::CircularImport {
            cycle: cycle.into(),
            span: None,
            src: None,
        }
    }

    pub fn circular_import_at(
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

    pub fn export_error(message: impl Into<String>) -> Self {
        MdsError::ExportError {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    pub fn export_error_at(
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

    pub fn resource_limit(message: impl Into<String>) -> Self {
        MdsError::ResourceLimit {
            message: message.into(),
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        MdsError::Io {
            message: message.into(),
        }
    }

    pub fn yaml_error(message: impl Into<String>) -> Self {
        MdsError::YamlError {
            message: message.into(),
        }
    }

    pub fn json_error(message: impl Into<String>) -> Self {
        MdsError::JsonError {
            message: message.into(),
        }
    }

    pub fn not_mds_file(path: impl Into<String>) -> Self {
        MdsError::NotMdsFile {
            path: path.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(e.to_string().contains("argument"), "should say 'argument' not 'arguments' for 1");
    }

    #[test]
    fn arity_display_plural_arguments() {
        let e = MdsError::arity("f", 2, 0);
        assert!(e.to_string().contains("arguments"), "should say 'arguments' for 2");
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

}
