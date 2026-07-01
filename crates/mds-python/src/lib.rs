//! Native Python bindings for the MDS compiler via PyO3.
//!
//! Exposes seven functions to Python as the native extension module
//! `mdscript._mdscript` (re-exported by the pure-Python `mdscript` package):
//! [`compile`], [`compile_file`], [`compile_virtual`], [`check`], [`check_file`],
//! [`check_virtual`], and [`scan_imports`].
//!
//! ## Design mirror
//!
//! The four string/file functions mirror `crates/mds-napi` for error, panic,
//! resource-limit, vars, and options handling; the three virtual/scan functions
//! mirror `crates/mds-wasm` (the virtual filesystem model). All compilation output
//! funnels through the single shared serializer [`mds::CompileResult::to_canonical_json`],
//! so the wire shape is byte-identical to the Node.js and WASM bindings by construction.
//!
//! ## Canonical result object
//!
//! [`compile`], [`compile_file`], and [`compile_virtual`] return a [`CompileResult`]
//! whose `.to_dict()` is the discriminated union:
//!
//! - Markdown: `{ "kind": "markdown", "output": str, "warnings": [str], "dependencies": [str] }`
//! - Messages: `{ "kind": "messages", "messages": [{role,content}], "warnings": [str], "dependencies": [str] }`
//!
//! The **inactive payload field is absent** — a markdown result has no `messages`
//! key; a messages result has no `output` key. The typed getters (`.output`,
//! `.messages`) return `None` on the inactive variant.
//!
//! ## Error codes
//!
//! Every failure raises [`MdsError`] (a native, catchable `mdscript.MdsError`) with a
//! `.code`. Codes originating in `mds-core` (e.g. `"mds::syntax"`) are defined by
//! [`mds::MdsError`]. Three codes are **binding-only** — synthesised here:
//!
//! | Code                   | Meaning                                        |
//! |------------------------|------------------------------------------------|
//! | `mds::internal`        | Unexpected panic caught at the Python boundary  |
//! | `mds::invalid_options` | Malformed / type-incorrect `vars` or `modules`  |
//! | `mds::resource_limit`  | Input exceeds an enforced size / count limit    |
//!
//! ## Concurrency
//!
//! Each call releases the GIL around the (stateless) core compile via
//! `Python::detach`, with `catch_unwind` trapping panics inside the
//! GIL-released region. Result classes are `#[pyclass(frozen)]` and the module is
//! declared `gil_used = false`, so the extension is free-threading ready.

#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use mds::{json_type_name, parse_json_vars, Value, VarsError};
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyType;
use pythonize::{depythonize, pythonize};

// ── Resource limits ───────────────────────────────────────────────────────────

/// Maximum source string size accepted at the Python boundary (10 MiB).
///
/// Mirrors `mds::MAX_FILE_SIZE`. String inputs bypass the file layer's own size
/// check, so the limit is re-enforced here for every string input, `scan_imports`
/// included.
const MAX_SOURCE_SIZE: usize = mds::MAX_FILE_SIZE as usize;

/// Maximum number of entries in a `compile_virtual` / `check_virtual` `modules` map.
///
/// Mirrors the WASM binding. 256 modules is well above any realistic template graph.
const MAX_MODULE_COUNT: usize = 256;

/// Maximum aggregate byte size of all module values combined (same ceiling as a
/// single source input).
const MAX_MODULES_AGGREGATE_SIZE: usize = MAX_SOURCE_SIZE;

// ── Native exception ───────────────────────────────────────────────────────────

create_exception!(
    _mdscript,
    MdsError,
    PyException,
    "Raised for every MDS compilation failure.\n\n\
     Carries structured attributes: `code` (str), `message` (str), `help` (str | None),\n\
     and `span` (Span | None). `str(err) == err.message`."
);

// ── Result / value classes ─────────────────────────────────────────────────────

/// A source span attached to an [`MdsError`].
///
/// `offset`/`length` are byte offsets into the source; `line` is 1-indexed and
/// `column` is the 1-indexed character (Unicode scalar) position, or `None` when
/// the core could not resolve them. All values are Python `int`s — no truncation.
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq, Eq)]
pub struct Span {
    #[pyo3(get)]
    offset: usize,
    #[pyo3(get)]
    length: usize,
    #[pyo3(get)]
    line: Option<usize>,
    #[pyo3(get)]
    column: Option<usize>,
}

/// The `(type, args)` shape returned by [`Span::__reduce__`] for pickling.
type SpanReduce<'py> = (
    Bound<'py, PyType>,
    (usize, usize, Option<usize>, Option<usize>),
);

#[pymethods]
impl Span {
    #[new]
    #[pyo3(signature = (offset, length, line=None, column=None))]
    fn new(offset: usize, length: usize, line: Option<usize>, column: Option<usize>) -> Self {
        Span {
            offset,
            length,
            line,
            column,
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "Span(offset={}, length={}, line={}, column={})",
            self.offset,
            self.length,
            opt_repr(self.line),
            opt_repr(self.column),
        )
    }

    /// Return the span as a plain `dict` (`offset`, `length`, `line`, `column`).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.as_json())
    }

    /// Return the span as a canonical JSON string.
    fn to_json(&self) -> String {
        self.as_json().to_string()
    }

    /// Reconstruct on unpickle via `Span(offset, length, line, column)`.
    fn __reduce__<'py>(&self, py: Python<'py>) -> SpanReduce<'py> {
        (
            py.get_type::<Span>(),
            (self.offset, self.length, self.line, self.column),
        )
    }
}

impl Span {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "offset": self.offset,
            "length": self.length,
            "line": self.line,
            "column": self.column,
        })
    }
}

/// A single chat message produced by a `@message`-bearing template.
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    #[pyo3(get)]
    role: String,
    #[pyo3(get)]
    content: String,
}

#[pymethods]
impl Message {
    #[new]
    fn new(role: String, content: String) -> Self {
        Message { role, content }
    }

    fn __repr__(&self) -> String {
        format!("Message(role={:?}, content={:?})", self.role, self.content)
    }

    /// Return the message as a plain `dict` (`role`, `content`).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.as_json())
    }

    /// Return the message as a canonical JSON string.
    fn to_json(&self) -> String {
        self.as_json().to_string()
    }

    /// Reconstruct on unpickle via `Message(role, content)`.
    fn __reduce__<'py>(&self, py: Python<'py>) -> (Bound<'py, PyType>, (String, String)) {
        (
            py.get_type::<Message>(),
            (self.role.clone(), self.content.clone()),
        )
    }
}

impl Message {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({ "role": self.role, "content": self.content })
    }
}

/// The result of [`check`], [`check_file`], or [`check_virtual`].
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq, Eq)]
pub struct CheckResult {
    #[pyo3(get)]
    warnings: Vec<String>,
}

#[pymethods]
impl CheckResult {
    #[new]
    fn new(warnings: Vec<String>) -> Self {
        CheckResult { warnings }
    }

    fn __repr__(&self) -> String {
        format!("CheckResult(warnings={:?})", self.warnings)
    }

    /// Return the result as a plain `dict` (`{ "warnings": [...] }`).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.as_json())
    }

    /// Return the result as a canonical JSON string.
    fn to_json(&self) -> String {
        self.as_json().to_string()
    }

    /// Reconstruct on unpickle via `CheckResult(warnings)`.
    fn __reduce__<'py>(&self, py: Python<'py>) -> (Bound<'py, PyType>, (Vec<String>,)) {
        (py.get_type::<CheckResult>(), (self.warnings.clone(),))
    }
}

impl CheckResult {
    fn as_json(&self) -> serde_json::Value {
        serde_json::json!({ "warnings": self.warnings })
    }
}

/// The result of [`compile`], [`compile_file`], or [`compile_virtual`].
///
/// Retains the canonical `to_canonical_json()` value as its single backing store;
/// every typed getter and `to_dict()`/`to_json()` reads from it, so they can never
/// diverge. `__eq__` is wire equality; the object is intentionally unhashable.
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq)]
pub struct CompileResult {
    /// The canonical discriminated-union value — the single source of truth.
    value: serde_json::Value,
}

#[pymethods]
impl CompileResult {
    /// Reconstruct from a canonical mapping (used by unpickling).
    #[new]
    fn new(canonical: &Bound<'_, PyAny>) -> PyResult<Self> {
        let value: serde_json::Value = depythonize(canonical).map_err(|e| {
            options_error(canonical.py(), &format!("invalid CompileResult state: {e}"))
        })?;
        Ok(CompileResult { value })
    }

    /// `"markdown"` or `"messages"` — the intrinsic output shape of the template.
    #[getter]
    fn kind(&self) -> String {
        self.value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    /// The rendered Markdown string, or `None` when the result is `messages`.
    #[getter]
    fn output(&self) -> Option<String> {
        self.value
            .get("output")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    /// The structured messages, or `None` when the result is `markdown`.
    #[getter]
    fn messages(&self) -> Option<Vec<Message>> {
        self.value
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|m| Message {
                        role: json_str(m, "role"),
                        content: json_str(m, "content"),
                    })
                    .collect()
            })
    }

    /// Warnings emitted during compilation (never printed to stderr).
    #[getter]
    fn warnings(&self) -> Vec<String> {
        json_str_array(&self.value, "warnings")
    }

    /// Imported module keys in depth-first resolution order (entry excluded).
    #[getter]
    fn dependencies(&self) -> Vec<String> {
        json_str_array(&self.value, "dependencies")
    }

    fn __repr__(&self) -> String {
        match self.kind().as_str() {
            "messages" => format!(
                "CompileResult(kind='messages', messages=<{} item(s)>, warnings={:?}, dependencies={:?})",
                self.value
                    .get("messages")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len),
                self.warnings(),
                self.dependencies(),
            ),
            _ => format!(
                "CompileResult(kind='markdown', output={:?}, warnings={:?}, dependencies={:?})",
                self.output().unwrap_or_default(),
                self.warnings(),
                self.dependencies(),
            ),
        }
    }

    /// Return the canonical discriminated-union `dict` (the inactive payload key is
    /// absent). Byte-identical to the Node.js and WASM bindings' wire output.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.value)
    }

    /// Return the canonical result as a JSON string.
    fn to_json(&self) -> String {
        self.value.to_string()
    }

    /// Reconstruct on unpickle via `CompileResult(canonical_dict)`.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyType>, (Bound<'py, PyAny>,))> {
        Ok((py.get_type::<CompileResult>(), (self.to_dict(py)?,)))
    }
}

// ── Error / value conversion helpers ───────────────────────────────────────────

/// Convert an [`mds::MdsError`] into a raised [`MdsError`] with typed attributes.
///
/// Reads `serialize()` once and attaches `code`/`message`/`help`/`span`. `help` and
/// `span` are always set (to `None` when absent) so the attributes always exist.
/// The raised exception's message equals `serialized.message`, so `str(e) == e.message`.
fn mds_err_to_py(py: Python<'_>, err: &mds::MdsError) -> PyErr {
    let s = err.serialize();
    let pyerr = MdsError::new_err(s.message.clone());
    let inst = pyerr.value(py);
    let _ = inst.setattr("code", &s.code);
    let _ = inst.setattr("message", &s.message);
    match &s.help {
        Some(h) => drop(inst.setattr("help", h)),
        None => drop(inst.setattr("help", py.None())),
    }
    match &s.span {
        Some(sp) => {
            let span = Span {
                offset: sp.offset,
                length: sp.length,
                line: sp.line,
                column: sp.column,
            };
            match Py::new(py, span) {
                Ok(obj) => drop(inst.setattr("span", obj)),
                Err(_) => drop(inst.setattr("span", py.None())),
            }
        }
        None => drop(inst.setattr("span", py.None())),
    }
    pyerr
}

/// Build an [`MdsError`] carrying a synthesised (binding-only) `code`.
fn coded_error(py: Python<'_>, code: &str, message: &str) -> PyErr {
    let pyerr = MdsError::new_err(message.to_owned());
    let inst = pyerr.value(py);
    let _ = inst.setattr("code", code);
    let _ = inst.setattr("message", message);
    let _ = inst.setattr("help", py.None());
    let _ = inst.setattr("span", py.None());
    pyerr
}

/// `mds::invalid_options` — malformed / type-incorrect options.
fn options_error(py: Python<'_>, message: &str) -> PyErr {
    coded_error(py, "mds::invalid_options", message)
}

/// `mds::resource_limit` — input exceeds an enforced size / count limit.
fn resource_limit_error(py: Python<'_>, message: &str) -> PyErr {
    coded_error(py, "mds::resource_limit", message)
}

/// `mds::internal` — an unexpected panic was caught at the boundary.
///
/// The public message is deliberately generic; the raw panic payload is attached as
/// `detail` only under the (never-shipped-enabled) `debug-panics` feature, since it
/// can contain absolute filesystem paths.
fn internal_error(py: Python<'_>, detail: Option<String>) -> PyErr {
    let pyerr = coded_error(py, "mds::internal", "internal compiler error");
    if let Some(d) = detail {
        let _ = pyerr.value(py).setattr("detail", d);
    }
    pyerr
}

/// Serialize a `serde_json::Value` to a Python object (dict/list/str/…).
fn value_to_py<'py>(py: Python<'py>, value: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    pythonize(py, value).map_err(|e| {
        coded_error(
            py,
            "mds::internal",
            &format!("failed to serialize result: {e}"),
        )
    })
}

/// Read a string field from a JSON object, defaulting to empty.
fn json_str(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Read an array-of-strings field from a JSON object, defaulting to empty.
fn json_str_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Render an `Option<usize>` as its Python repr (`None` or the number).
fn opt_repr(v: Option<usize>) -> String {
    v.map_or_else(|| "None".to_owned(), |n| n.to_string())
}

// ── Panic guard (GIL released around the core) ──────────────────────────────────

/// The result of running a core call under `detach` + `catch_unwind`.
///
/// Deliberately Ungil (no `Py`/`Python` values) so it can cross the
/// `detach` boundary; the panic payload is reduced to an `Option<String>`
/// inside the closure rather than escaping as a `Box<dyn Any>`.
enum Outcome<T> {
    Ok(T),
    Mds(mds::MdsError),
    Panic(Option<String>),
}

/// Run a fallible core closure, trapping panics. Call this **inside**
/// `Python::detach` so the GIL is released for the duration.
fn guard<T>(f: impl FnOnce() -> Result<T, mds::MdsError>) -> Outcome<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Outcome::Ok(v),
        Ok(Err(e)) => Outcome::Mds(e),
        Err(payload) => Outcome::Panic(panic_detail(&*payload)),
    }
}

/// Map an [`Outcome`] back to a `PyResult` after the GIL is re-acquired.
fn map_outcome<T>(py: Python<'_>, outcome: Outcome<T>) -> PyResult<T> {
    match outcome {
        Outcome::Ok(v) => Ok(v),
        Outcome::Mds(e) => Err(mds_err_to_py(py, &e)),
        Outcome::Panic(detail) => Err(internal_error(py, detail)),
    }
}

/// Extract a human-readable panic detail — only under `debug-panics`.
fn panic_detail(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    #[cfg(feature = "debug-panics")]
    {
        if let Some(s) = payload.downcast_ref::<&str>() {
            Some((*s).to_owned())
        } else if let Some(s) = payload.downcast_ref::<String>() {
            Some(s.clone())
        } else {
            Some("unknown panic payload".to_owned())
        }
    }
    #[cfg(not(feature = "debug-panics"))]
    {
        let _ = payload;
        None
    }
}

// ── Boundary guards / options parsing ───────────────────────────────────────────

/// Reject oversized source strings before compilation.
fn check_source_size(py: Python<'_>, source: &str) -> PyResult<()> {
    if source.len() > MAX_SOURCE_SIZE {
        return Err(resource_limit_error(
            py,
            &format!(
                "source exceeds maximum size of {MAX_SOURCE_SIZE} bytes ({} bytes provided)",
                source.len()
            ),
        ));
    }
    Ok(())
}

/// Convert the optional `vars` argument into runtime variables.
///
/// `None`/absent → no vars. A non-mapping value (array, string, number, …) →
/// `mds::invalid_options`. Conversion runs while the GIL is held, before the core
/// call releases it (Decision 4).
fn extract_vars(
    py: Python<'_>,
    vars: Option<&Bound<'_, PyAny>>,
) -> PyResult<Option<HashMap<String, Value>>> {
    let Some(obj) = vars else {
        return Ok(None);
    };
    if obj.is_none() {
        return Ok(None);
    }
    let json: serde_json::Value =
        depythonize(obj).map_err(|e| options_error(py, &format!("invalid vars: {e}")))?;
    parse_json_vars(json).map(Some).map_err(|e| match e {
        VarsError::InvalidType(msg) => options_error(py, &msg),
        VarsError::Conversion(mds_err) => mds_err_to_py(py, &mds_err),
    })
}

/// Parse and validate a virtual `modules` mapping (`str -> str`).
///
/// Enforces `MAX_MODULE_COUNT` and the aggregate size ceiling (both →
/// `mds::resource_limit`); non-mapping input and non-string values →
/// `mds::invalid_options`. Mirrors the WASM binding's module parsing.
fn parse_modules(py: Python<'_>, modules: &Bound<'_, PyAny>) -> PyResult<HashMap<String, String>> {
    let json: serde_json::Value =
        depythonize(modules).map_err(|e| options_error(py, &format!("invalid modules: {e}")))?;
    let serde_json::Value::Object(map) = json else {
        return Err(options_error(
            py,
            &format!(
                "modules must be a mapping of str to str, got {}",
                json_type_name(&json)
            ),
        ));
    };
    if map.len() > MAX_MODULE_COUNT {
        return Err(resource_limit_error(
            py,
            &format!(
                "modules exceeds maximum module count of {MAX_MODULE_COUNT} ({} provided)",
                map.len()
            ),
        ));
    }

    let mut result = HashMap::with_capacity(map.len());
    let mut aggregate: usize = 0;
    for (key, val) in map {
        let serde_json::Value::String(s) = val else {
            return Err(options_error(
                py,
                &format!(
                    "modules[{key:?}] must be a string, got {}",
                    json_type_name(&val)
                ),
            ));
        };
        if s.len() > MAX_SOURCE_SIZE {
            return Err(resource_limit_error(
                py,
                &format!(
                    "modules[{key:?}] exceeds maximum size of {MAX_SOURCE_SIZE} bytes ({} bytes provided)",
                    s.len()
                ),
            ));
        }
        aggregate = aggregate.saturating_add(s.len());
        if aggregate > MAX_MODULES_AGGREGATE_SIZE {
            return Err(resource_limit_error(
                py,
                &format!(
                    "modules aggregate size exceeds maximum of {MAX_MODULES_AGGREGATE_SIZE} bytes"
                ),
            ));
        }
        result.insert(key, s);
    }
    Ok(result)
}

// ── Public functions ────────────────────────────────────────────────────────────

/// Compile an MDS template source string.
///
/// `vars` is an optional mapping of runtime variable overrides; `base_path` (str or
/// `os.PathLike`) sets the base directory for resolving `@import` paths (defaults to
/// the current working directory). Both are keyword-only.
#[pyfunction]
#[pyo3(signature = (source, *, vars=None, base_path=None))]
fn compile(
    py: Python<'_>,
    source: String,
    vars: Option<Bound<'_, PyAny>>,
    base_path: Option<PathBuf>,
) -> PyResult<CompileResult> {
    check_source_size(py, &source)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let outcome = py
        .detach(|| guard(move || mds::compile_str_with_deps(&source, base_path.as_deref(), vars)));
    let result = map_outcome(py, outcome)?;
    Ok(CompileResult {
        value: result.to_canonical_json(),
    })
}

/// Compile an MDS template file (`path` is a str or `os.PathLike`).
///
/// The base directory is derived from the file's own directory, so there is no
/// `base_path` argument. `vars` is keyword-only. Dependencies are absolute paths.
#[pyfunction]
#[pyo3(signature = (path, *, vars=None))]
fn compile_file(
    py: Python<'_>,
    path: PathBuf,
    vars: Option<Bound<'_, PyAny>>,
) -> PyResult<CompileResult> {
    let vars = extract_vars(py, vars.as_ref())?;
    let outcome = py.detach(|| guard(move || mds::compile_with_deps(&path, vars)));
    let result = map_outcome(py, outcome)?;
    Ok(CompileResult {
        value: result.to_canonical_json(),
    })
}

/// Compile a module from an in-memory virtual filesystem.
///
/// `modules` maps module key → source; `entry` is the key to compile and must be a
/// key present in `modules`. `vars` is keyword-only. No source injection occurs —
/// all modules (entry included) are supplied by the caller.
#[pyfunction]
#[pyo3(signature = (modules, entry, *, vars=None))]
fn compile_virtual(
    py: Python<'_>,
    modules: Bound<'_, PyAny>,
    entry: String,
    vars: Option<Bound<'_, PyAny>>,
) -> PyResult<CompileResult> {
    let modules = parse_modules(py, &modules)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let outcome =
        py.detach(|| guard(move || mds::compile_virtual_with_deps(modules, &entry, vars)));
    let result = map_outcome(py, outcome)?;
    Ok(CompileResult {
        value: result.to_canonical_json(),
    })
}

/// Check (validate) an MDS template source string without rendering output.
///
/// `vars` and `base_path` mirror [`compile`]. Returns a [`CheckResult`].
#[pyfunction]
#[pyo3(signature = (source, *, vars=None, base_path=None))]
fn check(
    py: Python<'_>,
    source: String,
    vars: Option<Bound<'_, PyAny>>,
    base_path: Option<PathBuf>,
) -> PyResult<CheckResult> {
    check_source_size(py, &source)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let outcome = py.detach(|| {
        guard(move || mds::check_str_collecting_warnings(&source, base_path.as_deref(), vars))
    });
    let ((), warnings) = map_outcome(py, outcome)?;
    Ok(CheckResult { warnings })
}

/// Check (validate) an MDS template file without rendering output.
#[pyfunction]
#[pyo3(signature = (path, *, vars=None))]
fn check_file(
    py: Python<'_>,
    path: PathBuf,
    vars: Option<Bound<'_, PyAny>>,
) -> PyResult<CheckResult> {
    let vars = extract_vars(py, vars.as_ref())?;
    let outcome = py.detach(|| guard(move || mds::check_collecting_warnings(&path, vars)));
    let ((), warnings) = map_outcome(py, outcome)?;
    Ok(CheckResult { warnings })
}

/// Check (validate) a module from an in-memory virtual filesystem.
#[pyfunction]
#[pyo3(signature = (modules, entry, *, vars=None))]
fn check_virtual(
    py: Python<'_>,
    modules: Bound<'_, PyAny>,
    entry: String,
    vars: Option<Bound<'_, PyAny>>,
) -> PyResult<CheckResult> {
    let modules = parse_modules(py, &modules)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let outcome =
        py.detach(|| guard(move || mds::check_virtual_collecting_warnings(modules, &entry, vars)));
    let ((), warnings) = map_outcome(py, outcome)?;
    Ok(CheckResult { warnings })
}

/// Extract all import / re-export paths from an MDS source string.
///
/// Returns a deduplicated `list[str]` in resolution order (frontmatter and
/// `@extends` paths first), or `[]` when there are none. `source` is positional-only.
#[pyfunction]
#[pyo3(signature = (source, /))]
fn scan_imports(py: Python<'_>, source: String) -> PyResult<Vec<String>> {
    check_source_size(py, &source)?;
    let outcome = py.detach(|| guard(move || mds::scan_imports(&source)));
    map_outcome(py, outcome)
}

// ── Module ──────────────────────────────────────────────────────────────────────

/// The native extension module — registered as `mdscript._mdscript`.
///
/// `gil_used = false` marks the module free-threading ready: the result classes are
/// frozen, there is no mutable global state, and the GIL is released around every
/// core call.
#[pymodule(gil_used = false)]
fn _mdscript(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("MdsError", m.py().get_type::<MdsError>())?;
    m.add_class::<Span>()?;
    m.add_class::<Message>()?;
    m.add_class::<CheckResult>()?;
    m.add_class::<CompileResult>()?;
    m.add_function(wrap_pyfunction!(compile, m)?)?;
    m.add_function(wrap_pyfunction!(compile_file, m)?)?;
    m.add_function(wrap_pyfunction!(compile_virtual, m)?)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(check_file, m)?)?;
    m.add_function(wrap_pyfunction!(check_virtual, m)?)?;
    m.add_function(wrap_pyfunction!(scan_imports, m)?)?;
    Ok(())
}
