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
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;

use mds::{format_unknown_keys_error, parse_json_vars, Value, VarsError};
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

/// A single structured message returned by `compileMessages`.
///
/// Mirrors a chat API message: `{ role: string, content: string }`.
#[napi(object)]
pub struct Message {
    /// The role string (e.g. `"system"`, `"user"`, `"assistant"`).
    pub role: String,
    /// The rendered body text of the message (trimmed).
    pub content: String,
}

/// Result returned by `compileMessages`.
#[napi(object)]
pub struct CompileMessagesResult {
    /// Structured messages produced from `@message` blocks.
    pub messages: Vec<Message>,
    /// Warnings emitted during compilation (e.g. orphan text outside `@message`).
    pub warnings: Vec<String>,
    /// Absolute paths of all files imported during compilation, in
    /// depth-first resolution order. Excludes the entry file itself.
    pub dependencies: Vec<String>,
}

// ── Low-level error helpers ───────────────────────────────────────────────────

/// Create a JS Error with a custom code and message using raw N-API.
///
/// `napi_create_error(env, code, message, &mut err)` creates a standard JS
/// Error whose `.code` property is set to `code`. This is the canonical N-API
/// mechanism for structured errors.
///
/// Returns the raw `napi_value` of the created error object, or null on failure.
///
/// # Safety
///
/// `env` must be a valid `napi_env` obtained from an active napi callback frame.
/// The function must be called from within a valid napi callback scope.
unsafe fn raw_create_error(env: sys::napi_env, code: &str, message: &str) -> sys::napi_value {
    let mut code_val: sys::napi_value = ptr::null_mut();
    let mut msg_val: sys::napi_value = ptr::null_mut();
    let mut err_val: sys::napi_value = ptr::null_mut();

    if sys::napi_create_string_utf8(
        env,
        code.as_ptr().cast(),
        code.len() as isize,
        &mut code_val,
    ) != sys::Status::napi_ok
    {
        return ptr::null_mut();
    }

    if sys::napi_create_string_utf8(
        env,
        message.as_ptr().cast(),
        message.len() as isize,
        &mut msg_val,
    ) != sys::Status::napi_ok
    {
        return ptr::null_mut();
    }

    if sys::napi_create_error(env, code_val, msg_val, &mut err_val) != sys::Status::napi_ok {
        return ptr::null_mut();
    }

    err_val
}

/// Set a string property on a raw JS object using raw N-API.
///
/// # Safety
///
/// `env` must be a valid `napi_env` obtained from an active napi callback frame.
/// `obj` must be a valid `napi_value` representing a JS object in the current scope.
unsafe fn raw_set_string_prop(env: sys::napi_env, obj: sys::napi_value, key: &str, value: &str) {
    let Ok(ckey) = CString::new(key) else { return };
    let mut val: sys::napi_value = ptr::null_mut();
    let ok =
        sys::napi_create_string_utf8(env, value.as_ptr().cast(), value.len() as isize, &mut val);
    if ok == sys::Status::napi_ok {
        let _ = sys::napi_set_named_property(env, obj, ckey.as_ptr(), val);
    }
}

/// Set a uint32 property on a raw JS object using raw N-API.
///
/// # Safety
///
/// `env` must be a valid `napi_env` obtained from an active napi callback frame.
/// `obj` must be a valid `napi_value` representing a JS object in the current scope.
unsafe fn raw_set_uint32_prop(env: sys::napi_env, obj: sys::napi_value, key: &str, value: u32) {
    let Ok(ckey) = CString::new(key) else { return };
    let mut val: sys::napi_value = ptr::null_mut();
    let ok = sys::napi_create_uint32(env, value, &mut val);
    if ok == sys::Status::napi_ok {
        let _ = sys::napi_set_named_property(env, obj, ckey.as_ptr(), val);
    }
}

// ── Error conversion helpers ──────────────────────────────────────────────────

/// Build a JS span object `{ offset, length, line?, column? }` from a serialized span.
///
/// Returns the `napi_value` for the new object, or `null` if object creation fails.
///
/// # Safety
///
/// `env` must be a valid `napi_env` obtained from an active napi callback frame.
/// The caller must be within a valid napi callback scope.
unsafe fn raw_create_span_obj(env: sys::napi_env, span: &mds::SerializedSpan) -> sys::napi_value {
    let mut span_obj: sys::napi_value = ptr::null_mut();
    if sys::napi_create_object(env, &mut span_obj) != sys::Status::napi_ok {
        return ptr::null_mut();
    }
    // Use try_from to make usize→u32 truncation explicit; saturate at u32::MAX.
    raw_set_uint32_prop(
        env,
        span_obj,
        "offset",
        u32::try_from(span.offset).unwrap_or(u32::MAX),
    );
    raw_set_uint32_prop(
        env,
        span_obj,
        "length",
        u32::try_from(span.length).unwrap_or(u32::MAX),
    );
    if let Some(line) = span.line {
        raw_set_uint32_prop(
            env,
            span_obj,
            "line",
            u32::try_from(line).unwrap_or(u32::MAX),
        );
    }
    if let Some(column) = span.column {
        raw_set_uint32_prop(
            env,
            span_obj,
            "column",
            u32::try_from(column).unwrap_or(u32::MAX),
        );
    }
    span_obj
}

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

    // SAFETY: raw_env is obtained from a valid napi-rs Env that is alive for this
    // callback invocation. All napi_value handles are created and consumed within
    // the same callback scope.
    unsafe {
        let err_obj = raw_create_error(raw_env, &serialized.code, &serialized.message);
        if !err_obj.is_null() {
            if let Some(help) = &serialized.help {
                raw_set_string_prop(raw_env, err_obj, "help", help);
            }
            if let Some(span) = &serialized.span {
                let span_obj = raw_create_span_obj(raw_env, span);
                if !span_obj.is_null() {
                    if let Ok(ckey) = CString::new("span") {
                        let _ =
                            sys::napi_set_named_property(raw_env, err_obj, ckey.as_ptr(), span_obj);
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
    // SAFETY: raw_env is obtained from a valid napi-rs Env that is alive for this
    // callback invocation. All napi_value handles are created and consumed within
    // the same callback scope.
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
            #[cfg(feature = "debug-panics")]
            {
                let detail = if let Some(s) = payload.downcast_ref::<&str>() {
                    (*s).to_owned()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic payload".to_owned()
                };
                // Match mds-wasm: message is the generic string; detail is a
                // separate property on the error object so consumers can use
                // a consistent `err.detail` pattern across both binding layers.
                let raw_env = env.raw();
                // SAFETY: raw_env is obtained from a valid napi-rs Env that is alive for
                // this callback invocation. All napi_value handles are created and consumed
                // within the same callback scope.
                unsafe {
                    let err_obj =
                        raw_create_error(raw_env, "mds::internal", "internal compiler error");
                    if !err_obj.is_null() {
                        raw_set_string_prop(raw_env, err_obj, "detail", &detail);
                        let _ = sys::napi_throw(raw_env, err_obj);
                        return Err(napi::Error::new(Status::PendingException, ""));
                    }
                }
                // Fallback if object creation failed.
                Err(throw_coded_error(
                    env,
                    "internal compiler error",
                    "mds::internal",
                ))
            }
            #[cfg(not(feature = "debug-panics"))]
            {
                let _ = payload;
                Err(throw_coded_error(
                    env,
                    "internal compiler error",
                    "mds::internal",
                ))
            }
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

// ── Options parsing ───────────────────────────────────────────────────────────

/// Map a napi `ValueType` to a human-readable name for error messages.
fn napi_type_name(vt: ValueType) -> &'static str {
    match vt {
        ValueType::Undefined => "undefined",
        ValueType::Null => "null",
        ValueType::Boolean => "boolean",
        ValueType::Number => "number",
        ValueType::String => "string",
        ValueType::Symbol => "symbol",
        ValueType::Object => "object",
        ValueType::Function => "function",
        ValueType::External => "external",
        ValueType::Unknown => "unknown",
    }
}

/// Collect all unknown option keys from an Object and return an error if any exist.
///
/// Uses `get_property_names` to enumerate all keys, deserializes the resulting
/// Array as a `serde_json` array of strings, then filters out recognised keys.
/// Reports ALL unknown keys at once so users can fix multiple typos in one go.
fn reject_unknown_napi_keys(env: &Env, obj: &Object, known: &[&str]) -> napi::Result<()> {
    let names_obj: Object = obj.get_property_names()?;
    // Deserialize the property-names Array into a JSON array of strings.
    let names_json: serde_json::Value = env.from_js_value(names_obj)?;
    let keys: Vec<String> = match names_json {
        serde_json::Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => return Ok(()),
    };

    let unknowns: Vec<&str> = keys
        .iter()
        .filter(|k| !known.contains(&k.as_str()))
        .map(String::as_str)
        .collect();

    if unknowns.is_empty() {
        return Ok(());
    }

    Err(throw_options_error(
        env,
        &format_unknown_keys_error(&unknowns, known),
    ))
}

/// Extract and validate the `basePath` option using direct property access.
///
/// Returns `None` for absent, `undefined`, or `null`; errors on empty strings
/// or non-string types; returns `Some(PathBuf)` for valid non-empty strings.
fn extract_base_path_direct(env: &Env, obj: &Object) -> napi::Result<Option<PathBuf>> {
    if !obj.has_named_property("basePath")? {
        return Ok(None);
    }
    let val: Unknown = obj.get_named_property_unchecked("basePath")?;
    let vt = val.get_type()?;
    match vt {
        ValueType::Undefined | ValueType::Null => Ok(None),
        ValueType::String => {
            // SAFETY: we checked get_type() == String above before casting.
            let s: String = unsafe { val.cast()? };
            if s.is_empty() {
                Err(throw_options_error(
                    env,
                    "options.basePath must be a non-empty string",
                ))
            } else {
                Ok(Some(PathBuf::from(s)))
            }
        }
        other => Err(throw_options_error(
            env,
            &format!(
                "options.basePath must be a string, got {}",
                napi_type_name(other)
            ),
        )),
    }
}

/// Extract and validate the `vars` option using direct property access.
///
/// Returns `None` for absent, `undefined`, or `null`; delegates to the shared
/// `parse_json_vars` for object validation and conversion; errors on non-object
/// types (including arrays).
fn extract_vars_direct(env: &Env, obj: &Object) -> napi::Result<Option<HashMap<String, Value>>> {
    if !obj.has_named_property("vars")? {
        return Ok(None);
    }
    let val: Unknown = obj.get_named_property_unchecked("vars")?;
    let vt = val.get_type()?;
    match vt {
        ValueType::Undefined | ValueType::Null => Ok(None),
        ValueType::Object => {
            // Deserialize only the vars sub-value.
            let vars_json: serde_json::Value = env.from_js_value(val)?;
            // Note: ValueType::Object includes JS arrays, which serde deserializes
            // as Value::Array. The `let Value::Object(map) else` guard inside
            // parse_json_vars rejects arrays and non-objects.
            parse_json_vars(vars_json).map(Some).map_err(|e| match e {
                VarsError::InvalidType(msg) => throw_options_error(env, &msg),
                VarsError::Conversion(mds_err) => throw_mds_error(env, mds_err),
            })
        }
        other => Err(throw_options_error(
            env,
            &format!(
                "options.vars must be a plain object, got {}",
                napi_type_name(other)
            ),
        )),
    }
}

/// Parse options for `compile` and `check` (source-string variants).
///
/// Valid keys: `basePath`, `vars`.
/// Returns `(base_path, vars)`.
type CompileOpts = (Option<PathBuf>, Option<HashMap<String, Value>>);

fn parse_compile_opts(env: &Env, opts: Option<Object>) -> napi::Result<CompileOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, None));
    };

    reject_unknown_napi_keys(env, &opts_obj, &["basePath", "vars"])?;
    let base_path = extract_base_path_direct(env, &opts_obj)?;
    let vars = extract_vars_direct(env, &opts_obj)?;

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

    // basePath is not valid for file operations.
    if opts_obj.has_named_property("basePath")? {
        return Err(throw_options_error(
            env,
            "option \"basePath\" is not valid for compileFile/checkFile; \
             the base directory is derived from the file path",
        ));
    }

    reject_unknown_napi_keys(env, &opts_obj, &["vars"])?;
    let vars = extract_vars_direct(env, &opts_obj)?;

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
pub fn compile(env: Env, source: String, opts: Option<Object>) -> napi::Result<CompileResult> {
    check_source_size(&env, &source)?;

    let (base_path, vars) = parse_compile_opts(&env, opts)?;

    let result = run_catching(
        &env,
        AssertUnwindSafe(move || mds::compile_str_with_deps(&source, base_path.as_deref(), vars)),
    )?;

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
pub fn compile_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<CompileResult> {
    let vars = parse_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let result = run_catching(
        &env,
        AssertUnwindSafe(move || mds::compile_with_deps(&path_buf, vars)),
    )?;

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
pub fn check(env: Env, source: String, opts: Option<Object>) -> napi::Result<CheckResult> {
    check_source_size(&env, &source)?;

    let (base_path, vars) = parse_compile_opts(&env, opts)?;

    let ((), warnings) = run_catching(
        &env,
        AssertUnwindSafe(move || {
            mds::check_str_collecting_warnings(&source, base_path.as_deref(), vars)
        }),
    )?;

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
pub fn check_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<CheckResult> {
    let vars = parse_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let ((), warnings) = run_catching(
        &env,
        AssertUnwindSafe(move || mds::check_collecting_warnings(&path_buf, vars)),
    )?;

    Ok(CheckResult { warnings })
}

/// Compile an MDS template source string in messages mode, returning structured chat messages.
///
/// Each `@message role:` ... `@end` block becomes one entry in `messages`.
/// Orphan text outside `@message` blocks is ignored with a warning.
/// Empty messages (after trimming) are silently skipped.
///
/// Returns an error when the template contains no `@message` blocks.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `opts`: optional configuration object:
///   - `basePath` (string): base directory for resolving `@import` paths.
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///
/// ## Returns
///
/// On success, `{ messages: [{ role: string, content: string }], warnings: string[], dependencies: string[] }`.
///
/// On failure, throws a JS `Error` with additional properties:
/// - `code`: diagnostic code (e.g. `"mds::syntax"`)
/// - `help`: optional hint string
/// - `span`: optional `{ offset, length, line?, column? }`
#[napi(js_name = "compileMessages")]
pub fn compile_messages(
    env: Env,
    source: String,
    opts: Option<Object>,
) -> napi::Result<CompileMessagesResult> {
    check_source_size(&env, &source)?;

    let (base_path, vars) = parse_compile_opts(&env, opts)?;

    let result = run_catching(
        &env,
        AssertUnwindSafe(move || {
            mds::compile_messages_str_with_deps(&source, base_path.as_deref(), vars)
        }),
    )?;

    let messages = result
        .messages
        .into_iter()
        .map(|m| Message {
            role: m.role,
            content: m.content,
        })
        .collect();

    Ok(CompileMessagesResult {
        messages,
        warnings: result.warnings,
        dependencies: result.dependencies,
    })
}
