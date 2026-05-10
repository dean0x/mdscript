use miette::{Diagnostic, SourceSpan};
use std::path::PathBuf;
use thiserror::Error;

/// Span information for error reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub file: PathBuf,
    pub line: usize,
    pub col: usize,
    pub offset: usize,
    pub len: usize,
}

impl Span {
    pub fn new(file: PathBuf, line: usize, col: usize, offset: usize, len: usize) -> Self {
        Self {
            file,
            line,
            col,
            offset,
            len,
        }
    }
}

/// All errors produced by the MDS compiler.
#[derive(Error, Debug, Diagnostic)]
pub enum MdsError {
    #[error("syntax error: {message}")]
    #[diagnostic(code(mds::syntax))]
    Syntax {
        message: String,
        #[label("{message}")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<miette::NamedSource<String>>,
    },

    #[error("undefined variable '{name}'")]
    #[diagnostic(code(mds::undefined_var), help("define '{name}' in frontmatter or imports"))]
    UndefinedVariable {
        name: String,
        #[label("not defined")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<miette::NamedSource<String>>,
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
        src: Option<miette::NamedSource<String>>,
    },

    #[error("arity mismatch for '{name}': expected {expected} args, got {got}")]
    #[diagnostic(code(mds::arity))]
    ArityMismatch {
        name: String,
        expected: usize,
        got: usize,
        #[label("wrong number of arguments")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<miette::NamedSource<String>>,
    },

    #[error("type error: expected array for @for loop, got {got}")]
    #[diagnostic(code(mds::type_error))]
    TypeError {
        got: String,
        #[label("not an array")]
        span: Option<SourceSpan>,
        #[source_code]
        src: Option<miette::NamedSource<String>>,
    },

    #[error("circular import detected: {cycle}")]
    #[diagnostic(code(mds::circular_import))]
    CircularImport { cycle: String },

    #[error("file not found: {path}")]
    #[diagnostic(code(mds::file_not_found))]
    FileNotFound { path: String },

    #[error("import error: {message}")]
    #[diagnostic(code(mds::import))]
    ImportError { message: String },

    #[error("name collision: '{name}' is already defined")]
    #[diagnostic(code(mds::name_collision))]
    NameCollision { name: String },

    #[error("not an MDS file: {path}")]
    #[diagnostic(
        code(mds::not_mds),
        help("use .mds extension or add 'type: mds' to frontmatter")
    )]
    NotMdsFile { path: String },

    #[error("{message}")]
    #[diagnostic(code(mds::io))]
    Io { message: String },

    #[error("YAML parse error: {message}")]
    #[diagnostic(code(mds::yaml))]
    YamlError { message: String },

    #[error("JSON parse error: {message}")]
    #[diagnostic(code(mds::json))]
    JsonError { message: String },

    #[error("recursion detected in function '{name}'")]
    #[diagnostic(code(mds::recursion))]
    Recursion { name: String },

    #[error("export error: {message}")]
    #[diagnostic(code(mds::export))]
    ExportError { message: String },
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
        MdsError::Syntax {
            message: message.into(),
            span: Some(SourceSpan::new(offset.into(), len)),
            src: Some(miette::NamedSource::new(file, source.to_string())),
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
        MdsError::UndefinedVariable {
            name: name.into(),
            span: Some(SourceSpan::new(offset.into(), len)),
            src: Some(miette::NamedSource::new(file, source.to_string())),
        }
    }

    pub fn undefined_fn(name: impl Into<String>) -> Self {
        MdsError::UndefinedFunction {
            name: name.into(),
            span: None,
            src: None,
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

    pub fn type_error(got: impl Into<String>) -> Self {
        MdsError::TypeError {
            got: got.into(),
            span: None,
            src: None,
        }
    }
}
