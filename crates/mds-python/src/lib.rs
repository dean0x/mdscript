//! Native Python bindings for the MDS compiler via PyO3.
//!
//! Exposes ten functions to Python as the native extension module
//! `mdscript._mdscript` (re-exported by the pure-Python `mdscript` package):
//! [`compile`], [`compile_file`], [`compile_virtual`], [`check`], [`check_file`],
//! [`check_virtual`], [`scan_imports`], [`lint`], [`lint_file`], and
//! [`lint_virtual`].
//!
//! ## Design mirror
//!
//! The four string/file functions mirror `crates/mds-napi` for error, panic,
//! resource-limit, vars, and options handling; the three virtual/scan functions
//! mirror `crates/mds-wasm` (the virtual filesystem model). Compile output funnels
//! through [`mds::CompileResult::to_canonical_json`] and lint output through
//! [`mds::LintResult::to_canonical_json`], so the wire shape is byte-identical to
//! the Node.js and WASM bindings by construction.
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
use pyo3::types::{PyDict, PyType};
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

    /// The Source Map v3 document as a plain `dict`, or `None` when not generated.
    ///
    /// Present only when `source_map=True` was passed to the compile function AND the
    /// result is Markdown (messages-mode degrades to `None` per AC-FUNC-07).
    /// The wire key in `to_dict()` / `to_json()` is `"sourceMap"` (camelCase).
    #[getter]
    fn source_map<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match self.value.get("sourceMap") {
            Some(sm) => value_to_py(py, sm).map(Some),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        let sm_part = if self.value.get("sourceMap").is_some() {
            ", source_map=<present>"
        } else {
            ""
        };
        match self.kind().as_str() {
            "messages" => format!(
                "CompileResult(kind='messages', messages=<{} item(s)>, warnings={:?}, dependencies={:?}{sm_part})",
                self.value
                    .get("messages")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len),
                self.warnings(),
                self.dependencies(),
            ),
            _ => format!(
                "CompileResult(kind='markdown', output={:?}, warnings={:?}, dependencies={:?}{sm_part})",
                self.output().unwrap_or_default(),
                self.warnings(),
                self.dependencies(),
            ),
        }
    }

    /// Return the canonical discriminated-union `dict` (the inactive payload key is
    /// absent), with `"sourceMap"` always present as `None` when no map was generated.
    ///
    /// ## `to_dict()` vs `to_json()` asymmetry
    ///
    /// `to_dict()` always includes the `"sourceMap"` key, using Python `None` when no
    /// source map was requested. This is the Python-idiomatic style (attribute always
    /// exists; check `result.source_map is None` rather than `"sourceMap" in d`).
    ///
    /// `to_json()` omits the key when no map was generated — this is the canonical
    /// wire format shared with the Node.js and WASM bindings. If you need byte-level
    /// parity with those surfaces, use `to_json()` (or `json.loads(r.to_json())`),
    /// not `to_dict()`.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // Clone and inject "sourceMap": None when the key is absent so the
        // Python-level dict always has the key (Python-idiomatic always-present style).
        let mut value = self.value.clone();
        if let serde_json::Value::Object(ref mut map) = value {
            map.entry("sourceMap").or_insert(serde_json::Value::Null);
        }
        value_to_py(py, &value)
    }

    /// Return the canonical result as a JSON string.
    ///
    /// The `"sourceMap"` key is **omitted** when no map was generated — this is the
    /// wire format shared with the Node.js and WASM bindings. See `to_dict()` for
    /// the always-present Python-idiomatic variant.
    fn to_json(&self) -> String {
        self.value.to_string()
    }

    /// Reconstruct on unpickle via `CompileResult(canonical_dict)`.
    ///
    /// Uses the raw backing `value` (not `to_dict()`), so the null-sourceMap
    /// injection from `to_dict()` is NOT included. This preserves round-trip
    /// equality: original.value == restored.value.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyType>, (Bound<'py, PyAny>,))> {
        Ok((
            py.get_type::<CompileResult>(),
            (value_to_py(py, &self.value)?,),
        ))
    }
}

/// A single lint finding within a [`LintFileReport`].
///
/// Maps to the canonical wire-format diagnostic object
/// `{ rule, severity, message, help?, fixable, span? }`.
/// `help` is `None` when the rule emits no hint; `span` is `None` for rules
/// that do not attach a source offset. Both fields are always set as Python
/// attributes (never missing), so callers need not use `getattr` defaults.
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq, Eq)]
pub struct LintDiagnostic {
    #[pyo3(get)]
    rule: String,
    #[pyo3(get)]
    severity: String,
    #[pyo3(get)]
    message: String,
    #[pyo3(get)]
    help: Option<String>,
    #[pyo3(get)]
    fixable: bool,
    #[pyo3(get)]
    span: Option<Span>,
    // `fix_edits` is not exposed via `#[pyo3(get)]` because `Vec<serde_json::Value>`
    // does not implement `IntoPy`; a custom `#[getter]` is used instead.
    fix_edits: Option<Vec<serde_json::Value>>,
}

/// The `(type, args)` shape returned by [`LintDiagnostic::__reduce__`].
///
/// `fix_edits` is round-tripped as a JSON string (`Option<String>`) because
/// `serde_json::Value` is not directly pickle-able; the `#[new]` constructor
/// accepts and parses this string back into `Vec<serde_json::Value>`.
type LintDiagnosticReduce<'py> = (
    Bound<'py, PyType>,
    (
        String,
        String,
        String,
        Option<String>,
        bool,
        Option<Py<Span>>,
        Option<String>,
    ),
);

#[pymethods]
impl LintDiagnostic {
    #[new]
    #[pyo3(signature = (rule, severity, message, help=None, fixable=false, span=None, fix_edits_json=None))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        rule: String,
        severity: String,
        message: String,
        help: Option<String>,
        fixable: bool,
        // `Span` has `skip_from_py_object` so it cannot appear directly in
        // a `#[new]` signature; accept `Py<Span>` (which implements
        // `FromPyObject`) and clone the inner value via a GIL borrow.
        span: Option<Py<Span>>,
        // `fix_edits` is passed as a JSON string during unpickling so that the
        // args tuple stays fully pickle-able; `None` means no edits available.
        fix_edits_json: Option<String>,
    ) -> PyResult<Self> {
        let span = span.map(|py_span| py_span.borrow(py).clone());
        let fix_edits = fix_edits_json
            .as_deref()
            .map(serde_json::from_str::<Vec<serde_json::Value>>)
            .transpose()
            .map_err(|e| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "fix_edits_json is not valid JSON: {e}"
                ))
            })?;
        Ok(LintDiagnostic {
            rule,
            severity,
            message,
            help,
            fixable,
            span,
            fix_edits,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "LintDiagnostic(rule={:?}, severity={:?}, message={:?}, fixable={})",
            self.rule, self.severity, self.message, self.fixable,
        )
    }

    /// Return the diagnostic as a plain `dict`.
    ///
    /// `help` is `None` (Python) when the rule emits no hint; `span` is `None` (Python)
    /// when the rule emits no span — matching the canonical wire format produced by
    /// `to_canonical_json()` and all other surfaces.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.as_json())
    }

    /// Return the diagnostic as a canonical JSON string.
    fn to_json(&self) -> String {
        self.as_json().to_string()
    }

    /// Reconstruct on unpickle via `LintDiagnostic(rule, severity, message, help, fixable, span, fix_edits_json)`.
    fn __reduce__<'py>(&self, py: Python<'py>) -> PyResult<LintDiagnosticReduce<'py>> {
        let span_py = self
            .span
            .as_ref()
            .map(|s| Py::new(py, s.clone()))
            .transpose()?;
        let fix_edits_str = self
            .fix_edits
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok());
        Ok((
            py.get_type::<LintDiagnostic>(),
            (
                self.rule.clone(),
                self.severity.clone(),
                self.message.clone(),
                self.help.clone(),
                self.fixable,
                span_py,
                fix_edits_str,
            ),
        ))
    }

    /// The byte-range replacement edits the fix engine would apply, or `None`.
    ///
    /// Each edit is a dict with keys `start`, `end` (byte offsets into the
    /// source), and `new_text` (replacement string). Returns `None` when no
    /// edits are associated with this diagnostic.
    #[getter]
    fn fix_edits<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match &self.fix_edits {
            Some(edits) => value_to_py(py, &serde_json::Value::Array(edits.clone())),
            None => Ok(py.None().into_bound(py)),
        }
    }
}

impl LintDiagnostic {
    /// Canonical JSON value for this diagnostic (keys in wire-format alphabetical order).
    ///
    /// `help`, `span`, and `fix_edits` are always emitted — as JSON `null` when `None` —
    /// to match `to_canonical_json()` exactly and satisfy the cross-surface byte-parity
    /// invariant (PF-007).
    fn as_json(&self) -> serde_json::Value {
        // Keys are inserted in alphabetical order (serde_json::Map preserves insertion order)
        // to produce a stable, predictable dict layout for callers that iterate keys.
        let mut map = serde_json::Map::new();
        // Emit fix_edits unconditionally (null when None) to match to_canonical_json exactly.
        let fix_edits_val = self
            .fix_edits
            .as_ref()
            .map(|v| serde_json::Value::Array(v.clone()))
            .unwrap_or(serde_json::Value::Null);
        map.insert("fix_edits".to_string(), fix_edits_val);
        map.insert("fixable".to_string(), serde_json::Value::Bool(self.fixable));
        // Emit help unconditionally (null when None) to match to_canonical_json exactly.
        map.insert(
            "help".to_string(),
            self.help
                .clone()
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
        );
        map.insert(
            "message".to_string(),
            serde_json::Value::String(self.message.clone()),
        );
        map.insert(
            "rule".to_string(),
            serde_json::Value::String(self.rule.clone()),
        );
        map.insert(
            "severity".to_string(),
            serde_json::Value::String(self.severity.clone()),
        );
        // Emit span unconditionally (null when None) to match to_canonical_json exactly.
        let span_val = self.span.as_ref().map(|sp| {
            // Span keys in alphabetical order: column (conditional), length,
            // line (conditional), offset.  line and column are omitted (key absent,
            // not null) when not set, matching to_canonical_json (diagnostic.rs:780-785).
            let mut span_map = serde_json::Map::new();
            if let Some(col) = sp.column {
                span_map.insert(
                    "column".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(col)),
                );
            }
            span_map.insert(
                "length".to_string(),
                serde_json::Value::Number(serde_json::Number::from(sp.length)),
            );
            if let Some(line) = sp.line {
                span_map.insert(
                    "line".to_string(),
                    serde_json::Value::Number(serde_json::Number::from(line)),
                );
            }
            span_map.insert(
                "offset".to_string(),
                serde_json::Value::Number(serde_json::Number::from(sp.offset)),
            );
            serde_json::Value::Object(span_map)
        });
        map.insert(
            "span".to_string(),
            span_val.unwrap_or(serde_json::Value::Null),
        );
        serde_json::Value::Object(map)
    }
}

/// The `(type, args)` shape returned by [`LintFileReport::__reduce__`].
type LintFileReportReduce<'py> = (Bound<'py, PyType>, (String, Vec<Py<LintDiagnostic>>));

/// A per-file findings group from [`LintResult::files`].
///
/// Contains the file path (`file`) and a typed list of findings (`diagnostics`).
/// Each diagnostic is a [`LintDiagnostic`] instance with fully-typed attributes.
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq, Eq)]
pub struct LintFileReport {
    #[pyo3(get)]
    file: String,
    #[pyo3(get)]
    diagnostics: Vec<LintDiagnostic>,
}

#[pymethods]
impl LintFileReport {
    #[new]
    fn new(py: Python<'_>, file: String, diagnostics: Bound<'_, PyAny>) -> PyResult<Self> {
        let diags: Vec<LintDiagnostic> = diagnostics
            .try_iter()
            .map_err(|e| options_error(py, &format!("diagnostics must be iterable: {e}")))?
            .map(|item| {
                let item = item?;
                let d = item.cast::<LintDiagnostic>().map_err(|e| {
                    options_error(
                        py,
                        &format!("diagnostics must be a list of LintDiagnostic: {e}"),
                    )
                })?;
                Ok(d.get().clone())
            })
            .collect::<PyResult<_>>()?;
        Ok(LintFileReport {
            file,
            diagnostics: diags,
        })
    }

    fn __repr__(&self) -> String {
        format!(
            "LintFileReport(file={:?}, diagnostics=<{} item(s)>)",
            self.file,
            self.diagnostics.len(),
        )
    }

    /// Return the file report as a plain `dict` (matching the canonical wire shape).
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.as_json())
    }

    /// Return the file report as a canonical JSON string.
    fn to_json(&self) -> String {
        self.as_json().to_string()
    }

    /// Reconstruct on unpickle via `LintFileReport(file, diagnostics_list)`.
    fn __reduce__<'py>(&self, py: Python<'py>) -> PyResult<LintFileReportReduce<'py>> {
        let diags: Vec<Py<LintDiagnostic>> = self
            .diagnostics
            .iter()
            .map(|d| Py::new(py, d.clone()))
            .collect::<PyResult<_>>()?;
        Ok((py.get_type::<LintFileReport>(), (self.file.clone(), diags)))
    }
}

impl LintFileReport {
    fn as_json(&self) -> serde_json::Value {
        let diags: Vec<serde_json::Value> = self.diagnostics.iter().map(|d| d.as_json()).collect();
        serde_json::json!({
            "diagnostics": diags,
            "file": self.file,
        })
    }
}

/// The result of [`lint`], [`lint_file`], or [`lint_virtual`].
///
/// Stores the canonical `to_canonical_json()` value as its single backing store;
/// typed getters and `to_dict()`/`to_json()` read from it so they can never diverge.
/// `__eq__` is wire equality. Byte-identical to the WASM and Node.js lint surfaces.
#[pyclass(frozen, eq, skip_from_py_object, module = "mdscript")]
#[derive(Clone, PartialEq)]
pub struct LintResult {
    /// Single authoritative source of truth — the canonical lint JSON.
    value: serde_json::Value,
}

#[pymethods]
impl LintResult {
    /// Reconstruct from a canonical mapping (used by unpickling).
    ///
    /// Calls [`sanitize_lint_value`] after deserializing so the backing store is
    /// always sanitized — the same guarantee `to_canonical_json()` provides on the
    /// live lint path. This closes the parallel-path gap for the
    /// `LintResult(canonical)` constructor (PF-004).
    #[new]
    fn new(canonical: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut value: serde_json::Value = depythonize(canonical).map_err(|e| {
            options_error(canonical.py(), &format!("invalid LintResult state: {e}"))
        })?;
        sanitize_lint_value(&mut value);
        Ok(LintResult { value })
    }

    /// Wire schema version — currently always `1`.
    #[getter]
    fn version(&self) -> u64 {
        self.value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1)
    }

    /// Per-file finding groups as a list of typed [`LintFileReport`] objects.
    ///
    /// Each report's `.file` is the path key; `.diagnostics` is a list of
    /// [`LintDiagnostic`] instances with fully-typed `.rule`, `.severity`,
    /// `.message`, `.help`, `.fixable`, and `.span` attributes.
    ///
    /// This getter supersedes the previous `list[dict]` return — callers that
    /// need the plain dict shape can use `result.to_dict()["files"]` or call
    /// `report.to_dict()` on individual file reports.
    #[getter]
    fn files(&self, py: Python<'_>) -> PyResult<Vec<Py<LintFileReport>>> {
        let arr = match self
            .value
            .get("files")
            .and_then(serde_json::Value::as_array)
        {
            Some(a) => a,
            None => return Ok(vec![]),
        };

        arr.iter()
            .map(|file_val| {
                // `self.value` is always sanitized: `to_canonical_json()` sanitizes at
                // the lint boundary; `sanitize_lint_value()` in `new()` covers the
                // `LintResult(canonical)` / pickle path. Plain read is safe (avoids PF-004).
                let file_key = json_str(file_val, "file");
                let diagnostics: Vec<LintDiagnostic> = file_val
                    .get("diagnostics")
                    .and_then(serde_json::Value::as_array)
                    .map(|diags| {
                        diags
                            .iter()
                            .map(|d| {
                                let span = d.get("span").and_then(|s| {
                                    Some(Span {
                                        offset: s.get("offset")?.as_u64()? as usize,
                                        length: s.get("length")?.as_u64()? as usize,
                                        // line and column are omitted (key absent, not null)
                                        // when not set; map absent / null to None.
                                        line: s
                                            .get("line")
                                            .and_then(serde_json::Value::as_u64)
                                            .map(|v| v as usize),
                                        column: s
                                            .get("column")
                                            .and_then(serde_json::Value::as_u64)
                                            .map(|v| v as usize),
                                    })
                                });
                                let fix_edits = d
                                    .get("fix_edits")
                                    .and_then(serde_json::Value::as_array)
                                    .map(|arr| arr.to_vec());
                                LintDiagnostic {
                                    rule: json_str(d, "rule"),
                                    severity: json_str(d, "severity"),
                                    // Plain reads: `self.value` is sanitized before reaching here
                                    // (see file_key comment above). No allocation on clean strings.
                                    message: json_str(d, "message"),
                                    help: d
                                        .get("help")
                                        .and_then(serde_json::Value::as_str)
                                        .map(str::to_owned),
                                    fixable: d
                                        .get("fixable")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false),
                                    span,
                                    fix_edits,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                Py::new(
                    py,
                    LintFileReport {
                        file: file_key,
                        diagnostics,
                    },
                )
            })
            .collect()
    }

    /// `True` when the per-file diagnostic cap was hit; re-run after fixing.
    #[getter]
    fn truncated(&self) -> bool {
        self.value
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    fn __repr__(&self) -> String {
        let n = self
            .value
            .get("files")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        format!(
            "LintResult(version={}, files=<{n} file(s)>, truncated={})",
            self.version(),
            self.truncated(),
        )
    }

    /// Return the canonical lint result as a plain Python `dict`.
    ///
    /// Byte-identical to the Node.js and WASM `lint()` surfaces.
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        value_to_py(py, &self.value)
    }

    /// Return the canonical lint result as a JSON string.
    fn to_json(&self) -> String {
        self.value.to_string()
    }

    /// Reconstruct on unpickle via `LintResult(canonical_dict)`.
    fn __reduce__<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyType>, (Bound<'py, PyAny>,))> {
        Ok((py.get_type::<LintResult>(), (self.to_dict(py)?,)))
    }
}

// ── Lint value sanitization ─────────────────────────────────────────────────────

/// Sanitize a string field in a JSON object in-place via
/// [`mds::sanitize_control_chars_wire`].
///
/// WIRE mode: this backing store feeds `as_json()` / `to_dict()` as well as the typed
/// getters, and must stay byte-identical to `LintResult::to_canonical_json()` on the
/// other three surfaces — CLI, napi and WASM (PF-007) — including the `\n` escape.
///
/// Uses `Cow` so clean strings (the common case) cause no allocation — only strings
/// that actually contain hostile characters are replaced. No-op when the field is
/// absent or not a string.
fn sanitize_json_str_field(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str) {
    // Clone the field value to release the immutable borrow of `obj` before the
    // subsequent `obj.insert(...)` mutable borrow.
    let s_owned = match obj.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => return,
    };
    if let std::borrow::Cow::Owned(sanitized) = mds::sanitize_control_chars_wire(&s_owned) {
        obj.insert(key.to_string(), serde_json::Value::String(sanitized));
    }
}

/// Sanitize all message, help, and file string fields in a canonical lint result value
/// in-place.
///
/// Called in [`LintResult::new`] so any data arriving through the
/// `LintResult(canonical)` / pickle path is sanitized before the typed getters or
/// `to_dict()` read from the backing store. Mirrors what
/// `LintResult::to_canonical_json()` does on the live lint path, closing the
/// parallel-path gap (PF-004).
///
/// **No re-sort (AD-202-1b):** `to_canonical_json()` preserves caller-supplied
/// order for `LintResult::new` callers; `sort_diagnostics` is the single ordering
/// choke point, called only from `LintResultBuilder::build`.  This function
/// intentionally mirrors that contract — it sanitizes but does not reorder.
fn sanitize_lint_value(value: &mut serde_json::Value) {
    let Some(files) = value.get_mut("files").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for file_val in files.iter_mut() {
        let Some(obj) = file_val.as_object_mut() else {
            continue;
        };
        sanitize_json_str_field(obj, "file");
        let Some(diags) = obj.get_mut("diagnostics").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        for d in diags.iter_mut() {
            let Some(d_obj) = d.as_object_mut() else {
                continue;
            };
            sanitize_json_str_field(d_obj, "message");
            sanitize_json_str_field(d_obj, "help");
        }
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
    // `help`/`span` may be absent; `Option<T>: IntoPyObject` maps `None` to Python
    // `None`, so a single `setattr` per attribute covers both cases.
    let _ = inst.setattr("help", s.help.as_deref());
    let span = s.span.as_ref().and_then(|sp| {
        Py::new(
            py,
            Span {
                offset: sp.offset,
                length: sp.length,
                line: sp.line,
                column: sp.column,
            },
        )
        .ok()
    });
    let _ = inst.setattr("span", span);
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

/// Release the GIL, run a fallible core closure while trapping panics, then
/// re-acquire the GIL to map any failure to a raised `PyErr`. The single entry
/// point for every public function's core call; mirrors `run_catching` in
/// `crates/mds-napi`.
fn run_catching<T: Send>(
    py: Python<'_>,
    f: impl FnOnce() -> Result<T, mds::MdsError> + Send,
) -> PyResult<T> {
    let outcome = py.detach(|| guard(f));
    map_outcome(py, outcome)
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

/// Reject an empty `base_path`.
///
/// An empty string is not the same as `None`: `None` means "let the core default
/// to the current working directory," but `Some("")` would otherwise reach the
/// core as a real (invalid) path and surface a confusing resolver error instead
/// of a precise boundary one. Mirrors `mds-napi`'s `extract_base_path_direct`
/// empty-string rejection, so both bindings raise the same `mds::invalid_options`
/// for the same input.
fn check_base_path(py: Python<'_>, base_path: &Option<PathBuf>) -> PyResult<()> {
    if matches!(base_path, Some(p) if p.as_os_str().is_empty()) {
        return Err(options_error(py, "base_path must be a non-empty string"));
    }
    Ok(())
}

/// Convert the optional `vars` argument into runtime variables.
///
/// `None`/absent → no vars. A non-mapping value (array, string, number, …) →
/// `mds::invalid_options`. Conversion runs while the GIL is held, before the core
/// call releases it.
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
        // VarsError is #[non_exhaustive]; handle future variants as conversion errors.
        _ => options_error(py, &format!("vars error: {e}")),
    })
}

/// `mds::resource_limit` for an oversized `modules` mapping.
///
/// Shared by `parse_modules`'s pre-`depythonize` `PyDict` fast-path and its
/// post-`depythonize` backstop so both independent checks raise a
/// byte-identical message.
fn module_count_error(py: Python<'_>, count: usize) -> PyErr {
    resource_limit_error(
        py,
        &format!("modules exceeds maximum module count of {MAX_MODULE_COUNT} ({count} provided)"),
    )
}

/// Parse and validate a virtual `modules` mapping (`str -> str`).
///
/// Enforces `MAX_MODULE_COUNT` and the aggregate size ceiling (both →
/// `mds::resource_limit`); non-mapping input and non-string values →
/// `mds::invalid_options`. Mirrors the WASM binding's module parsing.
fn parse_modules(py: Python<'_>, modules: &Bound<'_, PyAny>) -> PyResult<HashMap<String, String>> {
    // Cheap pre-check against the live Python object: `dict.__len__` doesn't
    // touch any key or value, so an oversized mapping is rejected *before*
    // `depythonize` below copies every key/value into Rust. Scoped to `PyDict`
    // specifically rather than the broader `PyMapping` protocol: CPython's
    // `list` also satisfies `PyMapping_Check` (it fills `mp_subscript` for
    // slicing), so downcasting to `PyMapping` would silently reinterpret an
    // oversized `list` as a "mapping" and misreport it as `mds::resource_limit`
    // instead of the `mds::invalid_options` the type check below raises for it.
    // Non-`dict` mapping-like input (rare) skips this pre-check and falls
    // through to the post-`depythonize` count check, which stays as a
    // defense-in-depth backstop for that case.
    if let Ok(dict) = modules.cast::<PyDict>() {
        if dict.len() > MAX_MODULE_COUNT {
            return Err(module_count_error(py, dict.len()));
        }
    }

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
        return Err(module_count_error(py, map.len()));
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

/// Parse and validate the `rules` keyword argument into a [`mds::LintConfig`].
///
/// `None`/absent → default config (no per-rule overrides). A non-mapping value →
/// `mds::invalid_options`. Unknown or badly typed severity values →
/// `mds::invalid_options` (accepted: `"off"`, `"info"`, `"warn"`, `"error"`).
fn extract_rules(py: Python<'_>, rules: Option<&Bound<'_, PyAny>>) -> PyResult<mds::LintConfig> {
    let Some(obj) = rules else {
        return Ok(mds::LintConfig::default());
    };
    if obj.is_none() {
        return Ok(mds::LintConfig::default());
    }
    let json: serde_json::Value =
        depythonize(obj).map_err(|e| options_error(py, &format!("invalid rules: {e}")))?;
    let serde_json::Value::Object(map) = json else {
        return Err(options_error(
            py,
            &format!(
                "rules must be a mapping of str to str, got {}",
                json_type_name(&json)
            ),
        ));
    };
    let mut rules_map = HashMap::with_capacity(map.len());
    for (key, val) in map {
        let serde_json::Value::String(s) = &val else {
            return Err(options_error(
                py,
                &format!(
                    "rules[{key:?}] must be a string, got {}",
                    json_type_name(&val)
                ),
            ));
        };
        let severity: mds::Severity = serde_json::from_value(serde_json::Value::String(s.clone()))
            .map_err(|_| {
                options_error(
                    py,
                    &format!(
                        "rules[{key:?}]: unknown severity {s:?}; \
                         expected \"off\", \"info\", \"warn\", or \"error\""
                    ),
                )
            })?;
        rules_map.insert(key, severity);
    }
    Ok(mds::LintConfig::from_rules(rules_map))
}

/// Build a [`mds::CompileOptions`] from the `source_map` and `sources_content`
/// keyword arguments, enforcing the cross-field invariant via
/// [`mds::CompileOptions::validate`] — the single enforcement point
/// (avoids PF-004/PF-005).
///
/// `sources_content=True` without `source_map=True` → `mds::invalid_options`.
fn extract_compile_options(
    py: Python<'_>,
    source_map: bool,
    sources_content: bool,
) -> PyResult<mds::CompileOptions> {
    let opts = mds::CompileOptions::default()
        .with_source_map(source_map)
        .with_include_sources_content(sources_content);
    opts.validate().map_err(|_| {
        options_error(
            py,
            "option \"sources_content\" requires \"source_map\" to be True",
        )
    })?;
    Ok(opts)
}

// ── Public functions ────────────────────────────────────────────────────────────

/// Compile an MDS template source string.
///
/// `vars` is an optional mapping of runtime variable overrides; `base_path` (str or
/// `os.PathLike`) sets the base directory for resolving `@import` paths (defaults to
/// the current working directory; an explicit empty string raises `mds::invalid_options`
/// rather than silently resolving against an empty path). `source_map=True` appends a
/// Source Map v3 document to the result; `sources_content=True` embeds original source
/// text in the map (requires `source_map=True`). All are keyword-only.
#[pyfunction]
#[pyo3(signature = (source, *, vars=None, base_path=None, source_map=false, sources_content=false))]
fn compile(
    py: Python<'_>,
    source: String,
    vars: Option<Bound<'_, PyAny>>,
    base_path: Option<PathBuf>,
    source_map: bool,
    sources_content: bool,
) -> PyResult<CompileResult> {
    check_source_size(py, &source)?;
    check_base_path(py, &base_path)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let compile_opts = extract_compile_options(py, source_map, sources_content)?;
    let result = run_catching(py, move || {
        mds::compile_str_with_deps_opts(&source, base_path.as_deref(), vars, compile_opts)
    })?;
    Ok(CompileResult {
        value: result.to_canonical_json(),
    })
}

/// Compile an MDS template file (`path` is a str or `os.PathLike`).
///
/// The base directory is derived from the file's own directory, so there is no
/// `base_path` argument. `vars`, `source_map`, and `sources_content` are keyword-only.
/// Dependencies are absolute paths.
#[pyfunction]
#[pyo3(signature = (path, *, vars=None, source_map=false, sources_content=false))]
fn compile_file(
    py: Python<'_>,
    path: PathBuf,
    vars: Option<Bound<'_, PyAny>>,
    source_map: bool,
    sources_content: bool,
) -> PyResult<CompileResult> {
    let vars = extract_vars(py, vars.as_ref())?;
    let compile_opts = extract_compile_options(py, source_map, sources_content)?;
    let result = run_catching(py, move || {
        mds::compile_with_deps_opts(&path, vars, compile_opts)
    })?;
    Ok(CompileResult {
        value: result.to_canonical_json(),
    })
}

/// Compile a module from an in-memory virtual filesystem.
///
/// `modules` maps module key → source; `entry` is the key to compile and must be a
/// key present in `modules`. `vars`, `source_map`, and `sources_content` are
/// keyword-only. No source injection occurs — all modules (entry included) are
/// supplied by the caller.
#[pyfunction]
#[pyo3(signature = (modules, entry, *, vars=None, source_map=false, sources_content=false))]
fn compile_virtual(
    py: Python<'_>,
    modules: Bound<'_, PyAny>,
    entry: String,
    vars: Option<Bound<'_, PyAny>>,
    source_map: bool,
    sources_content: bool,
) -> PyResult<CompileResult> {
    let modules = parse_modules(py, &modules)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let compile_opts = extract_compile_options(py, source_map, sources_content)?;
    let result = run_catching(py, move || {
        mds::compile_virtual_with_deps_opts(modules, &entry, vars, compile_opts)
    })?;
    Ok(CompileResult {
        value: result.to_canonical_json(),
    })
}

/// Check (validate) an MDS template source string without rendering output.
///
/// `vars` and `base_path` mirror [`compile`]. Returns a [`CheckResult`].
///
/// `source_map` and `sources_content` are **not** valid options for `check`
/// — source maps are a compile-only concept. Passing either keyword raises
/// `MdsError(code="mds::invalid_options")` immediately (mirrors the WASM and
/// `packages/mds` `CheckOptions` split, B5/F9).
#[pyfunction]
#[pyo3(signature = (source, *, vars=None, base_path=None, source_map=None, sources_content=None))]
fn check(
    py: Python<'_>,
    source: String,
    vars: Option<Bound<'_, PyAny>>,
    base_path: Option<PathBuf>,
    source_map: Option<bool>,
    sources_content: Option<bool>,
) -> PyResult<CheckResult> {
    check_source_size(py, &source)?;
    check_base_path(py, &base_path)?;
    if source_map.is_some() || sources_content.is_some() {
        return Err(options_error(
            py,
            "source maps are not available for check(); use compile() instead",
        ));
    }
    let vars = extract_vars(py, vars.as_ref())?;
    let ((), warnings) = run_catching(py, move || {
        mds::check_str_collecting_warnings(&source, base_path.as_deref(), vars)
    })?;
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
    let ((), warnings) = run_catching(py, move || mds::check_collecting_warnings(&path, vars))?;
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
    let ((), warnings) = run_catching(py, move || {
        mds::check_virtual_collecting_warnings(modules, &entry, vars)
    })?;
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
    run_catching(py, move || mds::scan_imports(&source))
}

/// Lint an MDS template source string for static analysis findings.
///
/// Runs the check gate first — on a compile error, raises [`MdsError`] with the same
/// attributes as [`compile`]. On a clean gate, applies the lint rules and returns the
/// canonical [`LintResult`].
///
/// `vars`, `base_path`, and `rules` are all keyword-only.
#[pyfunction]
#[pyo3(signature = (source, *, vars=None, base_path=None, rules=None))]
fn lint(
    py: Python<'_>,
    source: String,
    vars: Option<Bound<'_, PyAny>>,
    base_path: Option<PathBuf>,
    rules: Option<Bound<'_, PyAny>>,
) -> PyResult<LintResult> {
    check_source_size(py, &source)?;
    check_base_path(py, &base_path)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let lint_config = extract_rules(py, rules.as_ref())?;
    let result = run_catching(py, move || {
        mds::lint_str_with(&source, base_path.as_deref(), vars, &lint_config)
    })?;
    Ok(LintResult {
        value: result.to_canonical_json(),
    })
}

/// Lint an MDS template file (`path` is a str or `os.PathLike`).
///
/// The base directory is derived from the file's own directory. `vars` and `rules`
/// are keyword-only. No `base_path` argument — mirrors [`compile_file`].
#[pyfunction]
#[pyo3(signature = (path, *, vars=None, rules=None))]
fn lint_file(
    py: Python<'_>,
    path: PathBuf,
    vars: Option<Bound<'_, PyAny>>,
    rules: Option<Bound<'_, PyAny>>,
) -> PyResult<LintResult> {
    let vars = extract_vars(py, vars.as_ref())?;
    let lint_config = extract_rules(py, rules.as_ref())?;
    let result = run_catching(py, move || mds::lint(&path, vars, &lint_config))?;
    Ok(LintResult {
        value: result.to_canonical_json(),
    })
}

/// Lint a module from an in-memory virtual filesystem.
///
/// `modules` maps module key → source string; `entry` is the key to lint. `vars` and
/// `rules` are keyword-only. Mirrors [`compile_virtual`] for the virtual-FS surface.
#[pyfunction]
#[pyo3(signature = (modules, entry, *, vars=None, rules=None))]
fn lint_virtual(
    py: Python<'_>,
    modules: Bound<'_, PyAny>,
    entry: String,
    vars: Option<Bound<'_, PyAny>>,
    rules: Option<Bound<'_, PyAny>>,
) -> PyResult<LintResult> {
    let modules = parse_modules(py, &modules)?;
    let vars = extract_vars(py, vars.as_ref())?;
    let lint_config = extract_rules(py, rules.as_ref())?;
    let result = run_catching(py, move || {
        mds::lint_virtual(modules, &entry, vars, &lint_config)
    })?;
    Ok(LintResult {
        value: result.to_canonical_json(),
    })
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
    m.add_class::<LintDiagnostic>()?;
    m.add_class::<LintFileReport>()?;
    m.add_class::<LintResult>()?;
    m.add_function(wrap_pyfunction!(compile, m)?)?;
    m.add_function(wrap_pyfunction!(compile_file, m)?)?;
    m.add_function(wrap_pyfunction!(compile_virtual, m)?)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(check_file, m)?)?;
    m.add_function(wrap_pyfunction!(check_virtual, m)?)?;
    m.add_function(wrap_pyfunction!(scan_imports, m)?)?;
    m.add_function(wrap_pyfunction!(lint, m)?)?;
    m.add_function(wrap_pyfunction!(lint_file, m)?)?;
    m.add_function(wrap_pyfunction!(lint_virtual, m)?)?;
    Ok(())
}
