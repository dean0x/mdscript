//! Native Node.js bindings for the MDS compiler via napi-rs.
//!
//! Exposes [`compile`], [`compile_file`], [`check`], and [`check_file`] to
//! Node.js as native add-ons. All compilation runs against the real OS
//! filesystem — no virtual FS layer.
//!
//! ## Error codes
//!
//! Errors thrown at the napi boundary carry a `code` property (set via
//! N-API `napi_create_error` with an explicit code string). Codes that
//! originate inside `mds-core` (e.g. `"mds::syntax"`) are defined by
//! [`mds::MdsError`]. The following codes are **napi-only** — they are
//! synthesised here and do not exist in the core crate:
//!
//! | Code                   | Meaning                                      |
//! |------------------------|----------------------------------------------|
//! | `mds::internal`        | Unexpected panic caught at the napi boundary |
//! | `mds::invalid_options` | Malformed or type-incorrect options object   |
//! | `mds::resource_limit`  | Input exceeds an enforced size limit         |
//!
//! ## Usage (JavaScript)
//!
//! ```js
//! const { compile, compileFile, check, checkFile } = require('./index');
//!
//! const result = compile('Hello {name}!\n', { vars: { name: 'World' } });
//! console.log(result.output); // "Hello World!\n"
//!
//! const fileResult = compileFile('/path/to/template.mds');
//! console.log(fileResult.output);
//! ```

#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::ffi::CString;
use std::panic::catch_unwind;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::ptr;

use mds::Value;
use napi::bindgen_prelude::*;
use napi::sys;
use napi::Env;
use napi_derive::napi;

// ── Resource limits ───────────────────────────────────────────────────────────

/// Maximum source string size accepted at the napi boundary (10 MiB).
///
/// Mirrors `mds::MAX_FILE_SIZE`. The napi boundary bypasses the file layer
/// when the caller passes a string, so the limit must be re-enforced here.
const MAX_SOURCE_SIZE: usize = mds::MAX_FILE_SIZE as usize;

// ── Return types ──────────────────────────────────────────────────────────────

/// Result returned by `compile` and `compileFile`.
#[napi(object)]
pub struct CompileResult {
    /// The rendered Markdown output.
    pub output: String,
    /// Warnings emitted during compilation (e.g. empty `@include`).
    pub warnings: Vec<String>,
    /// Absolute paths of all files imported during compilation, in
    /// depth-first resolution order. Excludes the entry file itself.
    pub dependencies: Vec<String>,
}

/// Result returned by `check` and `checkFile`.
#[napi(object)]
pub struct CheckResult {
    /// Warnings emitted during validation (e.g. empty `@include`).
    pub warnings: Vec<String>,
}

// ── Low-level error helpers ───────────────────────────────────────────────────

/// Create a JS Error with a custom code and message using raw N-API.
///
/// `napi_create_error(env, code, message, &mut err)` creates a standard JS
/// Error whose `.code` property is set to `code`. This is the canonical N-API
/// mechanism for structured errors.
///
/// Returns the raw `napi_value` of the created error object, or null on failure.
unsafe fn raw_create_error(
    env: sys::napi_env,
    code: &str,
    message: &str,
) -> sys::napi_value {
    let mut code_val: sys::napi_value = ptr::null_mut();
    let mut msg_val: sys::napi_value = ptr::null_mut();
    let mut err_val: sys::napi_value = ptr::null_mut();

    let _ = sys::napi_create_string_utf8(
        env,
        code.as_ptr().cast(),
        code.len() as isize,
        &mut code_val,
    );
    let _ = sys::napi_create_string_utf8(
        env,
        message.as_ptr().cast(),
        message.len() as isize,
        &mut msg_val,
    );
    let _ = sys::napi_create_error(env, code_val, msg_val, &mut err_val);

    err_val
}

/// Set a string property on a raw JS object using raw N-API.
unsafe fn raw_set_string_prop(env: sys::napi_env, obj: sys::napi_value, key: &str, value: &str) {
    let Ok(ckey) = CString::new(key) else { return };
    let mut val: sys::napi_value = ptr::null_mut();
    let ok = sys::napi_create_string_utf8(
        env,
        value.as_ptr().cast(),
        value.len() as isize,
        &mut val,
    );
    if ok == sys::Status::napi_ok {
        let _ = sys::napi_set_named_property(env, obj, ckey.as_ptr(), val);
    }
}

/// Set a uint32 property on a raw JS object using raw N-API.
unsafe fn raw_set_uint32_prop(env: sys::napi_env, obj: sys::napi_value, key: &str, value: u32) {
    let Ok(ckey) = CString::new(key) else { return };
    let mut val: sys::napi_value = ptr::null_mut();
    let ok = sys::napi_create_uint32(env, value, &mut val);
    if ok == sys::Status::napi_ok {
        let _ = sys::napi_set_named_property(env, obj, ckey.as_ptr(), val);
    }
}

// ── Error conversion helpers ──────────────────────────────────────────────────

/// Convert an [`mds::MdsError`] into a thrown JS exception with structured metadata.
///
/// Creates a JS Error via `napi_create_error` (which sets `.code`), then attaches
/// optional `.help` and `.span` properties. Finally calls `napi_throw` to make the
/// exception pending.
///
/// Returns `napi::Error::new(Status::PendingException, "")` to signal napi-rs
/// that a JS exception is already pending — it must not create another one.
fn throw_mds_error(env: &Env, err: mds::MdsError) -> napi::Error {
    let serialized = err.serialize();
    let raw_env = env.raw();

    unsafe {
        let err_obj = raw_create_error(raw_env, &serialized.code, &serialized.message);
        if !err_obj.is_null() {
            if let Some(help) = &serialized.help {
                raw_set_string_prop(raw_env, err_obj, "help", help);
            }

            if let Some(span) = &serialized.span {
                let mut span_obj: sys::napi_value = ptr::null_mut();
                if sys::napi_create_object(raw_env, &mut span_obj) == sys::Status::napi_ok {
                    raw_set_uint32_prop(raw_env, span_obj, "offset", span.offset as u32);
                    raw_set_uint32_prop(raw_env, span_obj, "length", span.length as u32);
                    if let Some(line) = span.line {
                        raw_set_uint32_prop(raw_env, span_obj, "line", line as u32);
                    }
                    if let Some(column) = span.column {
                        raw_set_uint32_prop(raw_env, span_obj, "column", column as u32);
                    }
                    if let Ok(ckey) = CString::new("span") {
                        let _ = sys::napi_set_named_property(raw_env, err_obj, ckey.as_ptr(), span_obj);
                    }
                }
            }

            let _ = sys::napi_throw(raw_env, err_obj);
        } else {
            // Fallback: use throw_error (no extra properties but always works).
            let _ = env.throw_error(&serialized.message, Some(&serialized.code));
        }
    }

    napi::Error::new(Status::PendingException, "")
}

/// Create a `mds::invalid_options` JS exception and return `PendingException`.
fn throw_options_error(env: &Env, msg: &str) -> napi::Error {
    throw_coded_error(env, msg, "mds::invalid_options")
}

/// Create a `mds::resource_limit` JS exception and return `PendingException`.
fn throw_resource_limit(env: &Env, msg: &str) -> napi::Error {
    throw_coded_error(env, msg, "mds::resource_limit")
}

/// Create a coded JS Error, throw it, and return `PendingException`.
fn throw_coded_error(env: &Env, msg: &str, code: &str) -> napi::Error {
    let raw_env = env.raw();
    unsafe {
        let err_obj = raw_create_error(raw_env, code, msg);
        if !err_obj.is_null() {
            let _ = sys::napi_throw(raw_env, err_obj);
        } else {
            let _ = env.throw_error(msg, Some(code));
        }
    }
    napi::Error::new(Status::PendingException, "")
}

// ── Panic guard ───────────────────────────────────────────────────────────────

/// Run a closure, catching both MDS errors and panics.
fn run_catching<F, T>(env: &Env, f: F) -> napi::Result<T>
where
    F: FnOnce() -> std::result::Result<T, mds::MdsError> + std::panic::UnwindSafe,
{
    match catch_unwind(f) {
        Ok(Ok(val)) => Ok(val),
        Ok(Err(mds_err)) => Err(throw_mds_error(env, mds_err)),
        Err(payload) => {
            #[allow(unused_variables)]
            let detail = if let Some(s) = payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic payload".to_string()
            };

            #[cfg(feature = "debug-panics")]
            let msg = format!("internal compiler error (panic): {detail}");
            #[cfg(not(feature = "debug-panics"))]
            let msg = "internal compiler error".to_string();

            Err(throw_coded_error(env, &msg, "mds::internal"))
        }
    }
}

// ── Source size guard ─────────────────────────────────────────────────────────

/// Reject oversized source strings before compilation.
fn check_source_size(env: &Env, source: &str) -> napi::Result<()> {
    if source.len() > MAX_SOURCE_SIZE {
        return Err(throw_resource_limit(
            env,
            &format!(
                "source exceeds maximum size of {} bytes ({} bytes provided)",
                MAX_SOURCE_SIZE,
                source.len()
            ),
        ));
    }
    Ok(())
}

// ── JSON type name helper ─────────────────────────────────────────────────────

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ── Options parsing ───────────────────────────────────────────────────────────

/// Extract and validate `vars` from a serde_json options map.
fn parse_vars_field(
    env: &Env,
    map: &mut serde_json::Map<String, serde_json::Value>,
) -> napi::Result<Option<HashMap<String, Value>>> {
    match map.remove("vars") {
        Some(serde_json::Value::Object(vars_map)) => {
            let mut result = HashMap::with_capacity(vars_map.len());
            for (key, val) in vars_map {
                let mds_val = Value::from_json(val)
                    .map_err(|e| throw_mds_error(env, e))?;
                result.insert(key, mds_val);
            }
            Ok(Some(result))
        }
        None => Ok(None),
        Some(other) => Err(throw_options_error(
            env,
            &format!(
                "options.vars must be a plain object, got {}",
                json_type_name(&other)
            ),
        )),
    }
}

/// Parse options for `compile` and `check` (source-string variants).
///
/// Valid keys: `basePath`, `vars`.
/// Returns `(base_path, vars)`.
fn parse_compile_opts(
    env: &Env,
    opts: Option<Object>,
) -> napi::Result<(Option<PathBuf>, Option<HashMap<String, Value>>)> {
    let Some(opts_obj) = opts else {
        return Ok((None, None));
    };

    // Use the `serde-json` feature to deserialize the options Object.
    let opts_val: serde_json::Value = env.from_js_value(opts_obj)?;

    let serde_json::Value::Object(mut map) = opts_val else {
        return Err(throw_options_error(env, "options must be a plain object"));
    };

    // Extract basePath.
    let base_path = match map.remove("basePath") {
        Some(serde_json::Value::String(s)) => {
            if s.is_empty() {
                return Err(throw_options_error(
                    env,
                    "options.basePath must be a non-empty string",
                ));
            }
            Some(PathBuf::from(s))
        }
        None => None,
        // null/undefined treated as None (omitted).
        Some(serde_json::Value::Null) => None,
        Some(other) => {
            return Err(throw_options_error(
                env,
                &format!(
                    "options.basePath must be a string, got {}",
                    json_type_name(&other)
                ),
            ))
        }
    };

    let vars = parse_vars_field(env, &mut map)?;

    // Reject unknown keys so callers catch typos early.
    if let Some(unknown_key) = map.keys().next() {
        return Err(throw_options_error(
            env,
            &format!(
                "unknown option key \"{unknown_key}\"; recognised keys are: basePath, vars"
            ),
        ));
    }

    Ok((base_path, vars))
}

/// Parse options for `compileFile` and `checkFile` (file-path variants).
///
/// Valid keys: `vars` only. `basePath` is not accepted.
fn parse_file_opts(
    env: &Env,
    opts: Option<Object>,
) -> napi::Result<Option<HashMap<String, Value>>> {
    let Some(opts_obj) = opts else {
        return Ok(None);
    };

    let opts_val: serde_json::Value = env.from_js_value(opts_obj)?;

    let serde_json::Value::Object(mut map) = opts_val else {
        return Err(throw_options_error(env, "options must be a plain object"));
    };

    // basePath is not valid for file operations.
    if map.contains_key("basePath") {
        return Err(throw_options_error(
            env,
            "option \"basePath\" is not valid for compileFile/checkFile; \
             the base directory is derived from the file path",
        ));
    }

    let vars = parse_vars_field(env, &mut map)?;

    // Reject all other unknown keys.
    if let Some(unknown_key) = map.keys().next() {
        return Err(throw_options_error(
            env,
            &format!(
                "unknown option key \"{unknown_key}\"; recognised keys are: vars"
            ),
        ));
    }

    Ok(vars)
}

// ── Public napi exports ───────────────────────────────────────────────────────

/// Compile an MDS template source string and return a structured result.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `opts`: optional configuration object:
///   - `basePath` (string): base directory for resolving `@import` paths.
///     Defaults to the current working directory.
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///
/// ## Returns
///
/// On success, `{ output: string, warnings: string[], dependencies: string[] }`.
///
/// On failure, throws a JS `Error` with additional properties:
/// - `code`: diagnostic code (e.g. `"mds::syntax"`)
/// - `help`: optional hint string
/// - `span`: optional `{ offset, length, line?, column? }`
#[napi]
pub fn compile(
    env: Env,
    source: String,
    opts: Option<Object>,
) -> napi::Result<CompileResult> {
    check_source_size(&env, &source)?;

    let (base_path, vars) = parse_compile_opts(&env, opts)?;

    let result = run_catching(&env, AssertUnwindSafe(move || {
        mds::compile_str_with_deps(&source, base_path.as_deref(), vars)
    }))?;

    Ok(CompileResult {
        output: result.output,
        warnings: result.warnings,
        dependencies: result.dependencies,
    })
}

/// Compile an MDS template file and return a structured result.
///
/// ## Arguments
///
/// - `path`: path to the `.mds` file to compile.
/// - `opts`: optional configuration object:
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///
/// `basePath` is not accepted — the base directory is derived from the file's
/// own directory.
///
/// ## Returns
///
/// Same shape as `compile`. Dependencies are absolute filesystem paths.
#[napi(js_name = "compileFile")]
pub fn compile_file(
    env: Env,
    path: String,
    opts: Option<Object>,
) -> napi::Result<CompileResult> {
    let vars = parse_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let result = run_catching(&env, AssertUnwindSafe(move || {
        mds::compile_with_deps(&path_buf, vars)
    }))?;

    Ok(CompileResult {
        output: result.output,
        warnings: result.warnings,
        dependencies: result.dependencies,
    })
}

/// Check (validate) an MDS template source string without rendering output.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `opts`: optional configuration object (same fields as `compile`).
///
/// ## Returns
///
/// On success, `{ warnings: string[] }`.
/// On failure, throws a JS `Error` with the same structure as `compile`.
#[napi]
pub fn check(
    env: Env,
    source: String,
    opts: Option<Object>,
) -> napi::Result<CheckResult> {
    check_source_size(&env, &source)?;

    let (base_path, vars) = parse_compile_opts(&env, opts)?;

    let ((), warnings) = run_catching(&env, AssertUnwindSafe(move || {
        mds::check_str_collecting_warnings(&source, base_path.as_deref(), vars)
    }))?;

    Ok(CheckResult { warnings })
}

/// Check (validate) an MDS template file without rendering output.
///
/// ## Arguments
///
/// - `path`: path to the `.mds` file to validate.
/// - `opts`: optional configuration object:
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///
/// ## Returns
///
/// Same shape as `check`.
#[napi(js_name = "checkFile")]
pub fn check_file(
    env: Env,
    path: String,
    opts: Option<Object>,
) -> napi::Result<CheckResult> {
    let vars = parse_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let ((), warnings) = run_catching(&env, AssertUnwindSafe(move || {
        mds::check_collecting_warnings(&path_buf, vars)
    }))?;

    Ok(CheckResult { warnings })
}
