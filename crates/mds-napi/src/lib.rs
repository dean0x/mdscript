//! Native Node.js bindings for the MDS compiler via napi-rs.
//!
//! Exposes [`compile`], [`compile_file`], [`check`], and [`check_file`] to
//! Node.js as native add-ons. All compilation runs against the real OS
//! filesystem — no virtual FS layer.
//!
//! ## Canonical result object
//!
//! `compile` and `compileFile` return a discriminated union object:
//!
//! - Markdown: `{ kind: "markdown", output: <string>, warnings: string[], dependencies: string[] }`
//! - Messages: `{ kind: "messages", messages: [{role,content},...], warnings: string[], dependencies: string[] }`
//!
//! The **inactive payload field is ABSENT** — a markdown result has no `messages` key;
//! a messages result has no `output` key. The object is constructed field-by-field using
//! `serde_json::Value` so napi-rs's struct-derive path cannot inject an unwanted field.
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
//! console.log(result.kind);   // "markdown"
//! console.log(result.output); // "Hello World!\n"
//!
//! const msgResult = compile('@message user:\nHi\n@end\n');
//! console.log(msgResult.kind);            // "messages"
//! console.log(msgResult.messages[0].role); // "user"
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

/// Maximum number of module entries accepted by `lintVirtual`.
///
/// Mirrors the WASM and Python bindings. 256 modules is well above any realistic
/// template graph; the cap prevents a caller from exhausting memory with thousands
/// of small virtual modules.
const MAX_MODULE_COUNT: usize = 256;

/// Maximum aggregate byte size of all `lintVirtual` module values combined
/// (same ceiling as a single source input).
const MAX_MODULES_AGGREGATE_SIZE: usize = MAX_SOURCE_SIZE;

// ── Return types ──────────────────────────────────────────────────────────────

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

/// Extract and validate a boolean option from an options object.
///
/// Returns `default_val` when the key is absent, undefined, or null.
/// Errors on non-boolean types with a descriptive message.
fn extract_bool_direct(
    env: &Env,
    obj: &Object,
    key: &str,
    default_val: bool,
) -> napi::Result<bool> {
    if !obj.has_named_property(key)? {
        return Ok(default_val);
    }
    let val: Unknown = obj.get_named_property_unchecked(key)?;
    let vt = val.get_type()?;
    match vt {
        ValueType::Undefined | ValueType::Null => Ok(default_val),
        ValueType::Boolean => {
            // SAFETY: we checked get_type() == Boolean above before casting.
            let b: bool = unsafe { val.cast()? };
            Ok(b)
        }
        other => Err(throw_options_error(
            env,
            &format!(
                "options.{key} must be a boolean, got {}",
                napi_type_name(other)
            ),
        )),
    }
}

/// Extract and validate the `sourceMap` and `sourcesContent` options.
///
/// Delegates the cross-field constraint check to [`mds::CompileOptions::validate`] —
/// the single enforcement point (avoids PF-004/PF-005).
/// Returns a [`mds::CompileOptions`] with both fields populated.
fn extract_compile_options_direct(env: &Env, obj: &Object) -> napi::Result<mds::CompileOptions> {
    let source_map = extract_bool_direct(env, obj, "sourceMap", false)?;
    let include_sources_content = extract_bool_direct(env, obj, "sourcesContent", false)?;
    let opts = mds::CompileOptions {
        source_map,
        include_sources_content,
    };
    opts.validate().map_err(|_| {
        throw_options_error(
            env,
            "option \"sourcesContent\" requires \"sourceMap\" to be true",
        )
    })?;
    Ok(opts)
}

/// Parse options for `compile` and `check` (source-string variants).
///
/// Valid keys: `basePath`, `vars`, `sourceMap`, `sourcesContent`.
/// Returns `(base_path, vars, compile_opts)`.
type CompileOpts = (
    Option<PathBuf>,
    Option<HashMap<String, Value>>,
    mds::CompileOptions,
);

fn parse_compile_opts(env: &Env, opts: Option<Object>) -> napi::Result<CompileOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, None, mds::CompileOptions::default()));
    };

    reject_unknown_napi_keys(
        env,
        &opts_obj,
        &["basePath", "vars", "sourceMap", "sourcesContent"],
    )?;
    let base_path = extract_base_path_direct(env, &opts_obj)?;
    let vars = extract_vars_direct(env, &opts_obj)?;
    let compile_opts = extract_compile_options_direct(env, &opts_obj)?;

    Ok((base_path, vars, compile_opts))
}

/// Parse options for `compileFile` and `checkFile` (file-path variants).
///
/// Valid keys: `vars`, `sourceMap`, `sourcesContent`. `basePath` is not accepted.
/// Returns `(vars, compile_opts)`.
type FileOpts = (Option<HashMap<String, Value>>, mds::CompileOptions);

fn parse_file_opts(env: &Env, opts: Option<Object>) -> napi::Result<FileOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, mds::CompileOptions::default()));
    };

    // basePath is not valid for file operations.
    if opts_obj.has_named_property("basePath")? {
        return Err(throw_options_error(
            env,
            "option \"basePath\" is not valid for compileFile/checkFile; \
             the base directory is derived from the file path",
        ));
    }

    reject_unknown_napi_keys(env, &opts_obj, &["vars", "sourceMap", "sourcesContent"])?;
    let vars = extract_vars_direct(env, &opts_obj)?;
    let compile_opts = extract_compile_options_direct(env, &opts_obj)?;

    Ok((vars, compile_opts))
}

/// Parse options for `check` (source-string check-only path) — no source-map keys.
///
/// Valid keys: `basePath`, `vars`.  Source-map options (`sourceMap`, `sourcesContent`)
/// are NOT accepted here — `check` does not generate maps, so they are irrelevant and
/// would silently mislead callers.  Aligns with Python's `check()` which accepts only
/// `base_path`/`vars` (ARCH-5).
type CheckOpts = (Option<PathBuf>, Option<HashMap<String, Value>>);

fn parse_check_opts(env: &Env, opts: Option<Object>) -> napi::Result<CheckOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, None));
    };
    reject_unknown_napi_keys(env, &opts_obj, &["basePath", "vars"])?;
    let base_path = extract_base_path_direct(env, &opts_obj)?;
    let vars = extract_vars_direct(env, &opts_obj)?;
    Ok((base_path, vars))
}

/// Parse options for `checkFile` (file-path check-only path) — no source-map keys.
///
/// Valid keys: `vars` only.  Source-map options and `basePath` are not accepted (ARCH-5).
type CheckFileOpts = Option<HashMap<String, Value>>;

fn parse_check_file_opts(env: &Env, opts: Option<Object>) -> napi::Result<CheckFileOpts> {
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

// ── Canonical result object builder ──────────────────────────────────────────

/// Build the canonical `{ kind, <active-payload>, warnings, dependencies }` JS object
/// from a [`mds::CompileResult`], as a `serde_json::Value`.
///
/// Delegates to [`mds::CompileResult::to_canonical_json`], which is the single
/// authoritative implementation shared with the WASM binding (AC-API-13: both
/// bindings must produce byte-identical wire output).
fn build_canonical_result(result: mds::CompileResult) -> serde_json::Value {
    result.to_canonical_json()
}

// ── Public napi exports ───────────────────────────────────────────────────────

/// Compile an MDS template source string and return a structured result.
///
/// For string-source compiles the `sources[0]` field in any generated source map is
/// `"input.mds"`.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `opts`: optional configuration object:
///   - `basePath` (string): base directory for resolving `@import` paths.
///     Defaults to the current working directory.
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///   - `sourceMap` (boolean, default `false`): generate a Source Map v3 document.
///     The result gains a `sourceMap` key when this is `true`. Silently ignored for
///     messages-mode templates (source maps are not supported for them).
///   - `sourcesContent` (boolean, default `false`): embed the original source text
///     in `sourcesContent[]`. Requires `sourceMap: true`; raises `mds::invalid_options`
///     otherwise. ⚠ Privacy: embeds the full template source in the map.
///
/// ## Returns
///
/// On success:
/// - Markdown: `{ kind: "markdown", output: string, warnings: string[], dependencies: string[], sourceMap?: object }`
/// - Messages: `{ kind: "messages", messages: [{role:string,content:string},...], warnings: string[], dependencies: string[] }`
///
/// The inactive payload field is absent from the returned object.
///
/// On failure, throws a JS `Error` with additional properties:
/// - `code`: diagnostic code (e.g. `"mds::syntax"`)
/// - `help`: optional hint string
/// - `span`: optional `{ offset, length, line?, column? }`
#[napi]
pub fn compile(env: Env, source: String, opts: Option<Object>) -> napi::Result<serde_json::Value> {
    check_source_size(&env, &source)?;

    let (base_path, vars, compile_opts) = parse_compile_opts(&env, opts)?;

    let result = run_catching(
        &env,
        AssertUnwindSafe(move || {
            mds::compile_str_with_deps_opts(&source, base_path.as_deref(), vars, compile_opts)
        }),
    )?;

    Ok(build_canonical_result(result))
}

/// Compile an MDS template file and return a structured result.
///
/// ## Arguments
///
/// - `path`: path to the `.mds` file to compile.
/// - `opts`: optional configuration object:
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///   - `sourceMap` (boolean, default `false`): generate a Source Map v3 document.
///   - `sourcesContent` (boolean, default `false`): embed original source text in the
///     map (requires `sourceMap: true`).
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
) -> napi::Result<serde_json::Value> {
    let (vars, compile_opts) = parse_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let result = run_catching(
        &env,
        AssertUnwindSafe(move || mds::compile_with_deps_opts(&path_buf, vars, compile_opts)),
    )?;

    Ok(build_canonical_result(result))
}

/// Check (validate) an MDS template source string without rendering output.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `opts`: optional configuration object:
///   - `basePath` (string): base directory for resolving `@import` paths.
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///
/// Source-map options (`sourceMap`, `sourcesContent`) are **not** accepted — `check`
/// does not generate output so maps are irrelevant and passing them is a hard error
/// (`mds::invalid_options`).
///
/// ## Returns
///
/// On success, `{ warnings: string[] }`.
/// On failure, throws a JS `Error` with the same structure as `compile`.
#[napi]
pub fn check(env: Env, source: String, opts: Option<Object>) -> napi::Result<CheckResult> {
    check_source_size(&env, &source)?;

    // Use the check-only parser: source-map options are not valid here (ARCH-5).
    let (base_path, vars) = parse_check_opts(&env, opts)?;

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
/// `basePath` is not accepted (base directory is derived from the file path).
/// Source-map options are not accepted — see `check`.
///
/// ## Returns
///
/// Same shape as `check`.
#[napi(js_name = "checkFile")]
pub fn check_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<CheckResult> {
    // Use the check-only parser: source-map options are not valid here (ARCH-5).
    let vars = parse_check_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let ((), warnings) = run_catching(
        &env,
        AssertUnwindSafe(move || mds::check_collecting_warnings(&path_buf, vars)),
    )?;

    Ok(CheckResult { warnings })
}

// ── Lint options parsing ──────────────────────────────────────────────────────

/// Extract and validate the `rules` option: `Record<string, string>` → `mds::LintConfig`.
///
/// Returns the default config (all rules at built-in defaults) when `rules` is absent,
/// `null`, or `undefined`. Validates each severity value against the closed enum.
fn extract_rules_direct(env: &Env, obj: &Object) -> napi::Result<mds::LintConfig> {
    if !obj.has_named_property("rules")? {
        return Ok(mds::LintConfig::default());
    }
    let val: Unknown = obj.get_named_property_unchecked("rules")?;
    let vt = val.get_type()?;
    match vt {
        ValueType::Undefined | ValueType::Null => Ok(mds::LintConfig::default()),
        ValueType::Object => {
            // Deserialize the rules sub-object; js arrays also satisfy Object so
            // we guard against that in the JSON shape check below.
            let rules_json: serde_json::Value = env.from_js_value(val)?;
            let serde_json::Value::Object(rules_map) = rules_json else {
                return Err(throw_options_error(
                    env,
                    &format!(
                        "options.rules must be a plain object, got {}",
                        mds::json_type_name(&rules_json)
                    ),
                ));
            };
            let mut rules = HashMap::new();
            for (key, val) in rules_map {
                let serde_json::Value::String(s) = &val else {
                    return Err(throw_options_error(
                        env,
                        &format!(
                            "options.rules[\"{key}\"] must be a severity string, got {}",
                            mds::json_type_name(&val)
                        ),
                    ));
                };
                // Parse severity via serde — validates against the closed enum.
                let severity: mds::Severity =
                    serde_json::from_str(&format!("\"{s}\"")).map_err(|_| {
                        throw_options_error(
                            env,
                            &format!(
                                "options.rules[\"{key}\"]: unknown severity \"{s}\"; \
                                 valid values are \"off\", \"info\", \"warn\", \"error\""
                            ),
                        )
                    })?;
                rules.insert(key, severity);
            }
            Ok(mds::LintConfig { rules })
        }
        other => Err(throw_options_error(
            env,
            &format!(
                "options.rules must be a plain object, got {}",
                napi_type_name(other)
            ),
        )),
    }
}

/// Parse options for `lint` and `lintVirtual` (source-string / virtual variants).
///
/// Valid keys: `basePath`, `vars`, `rules`.
/// Returns `(base_path, vars, lint_config)`.
type LintOpts = (
    Option<PathBuf>,
    Option<HashMap<String, Value>>,
    mds::LintConfig,
);

fn parse_lint_opts(env: &Env, opts: Option<Object>) -> napi::Result<LintOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, None, mds::LintConfig::default()));
    };

    reject_unknown_napi_keys(env, &opts_obj, &["basePath", "vars", "rules"])?;
    let base_path = extract_base_path_direct(env, &opts_obj)?;
    let vars = extract_vars_direct(env, &opts_obj)?;
    let lint_config = extract_rules_direct(env, &opts_obj)?;

    Ok((base_path, vars, lint_config))
}

/// Parse options for `lintFile` (file-path variant).
///
/// Valid keys: `vars`, `rules`. `basePath` is not accepted (derived from file path).
type LintFileOpts = (Option<HashMap<String, Value>>, mds::LintConfig);

fn parse_lint_file_opts(env: &Env, opts: Option<Object>) -> napi::Result<LintFileOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, mds::LintConfig::default()));
    };

    if opts_obj.has_named_property("basePath")? {
        return Err(throw_options_error(
            env,
            "option \"basePath\" is not valid for lintFile; \
             the base directory is derived from the file path",
        ));
    }

    reject_unknown_napi_keys(env, &opts_obj, &["vars", "rules"])?;
    let vars = extract_vars_direct(env, &opts_obj)?;
    let lint_config = extract_rules_direct(env, &opts_obj)?;

    Ok((vars, lint_config))
}

/// Parse options for `lintVirtual` (virtual-module variant).
///
/// Valid keys: `vars`, `rules`. `basePath` is not accepted — virtual modules
/// have no file path; `@import` paths in a virtual module are resolved
/// against the module map, not the filesystem.
fn parse_lint_virtual_opts(env: &Env, opts: Option<Object>) -> napi::Result<LintFileOpts> {
    let Some(opts_obj) = opts else {
        return Ok((None, mds::LintConfig::default()));
    };

    if opts_obj.has_named_property("basePath")? {
        return Err(throw_options_error(
            env,
            "option \"basePath\" is not valid for lintVirtual; \
             virtual modules have no file path",
        ));
    }

    reject_unknown_napi_keys(env, &opts_obj, &["vars", "rules"])?;
    let vars = extract_vars_direct(env, &opts_obj)?;
    let lint_config = extract_rules_direct(env, &opts_obj)?;

    Ok((vars, lint_config))
}

// ── Public lint exports ───────────────────────────────────────────────────────

/// Lint an MDS template source string for static analysis findings.
///
/// Runs the check gate (resolve+validate) first — on a compile error, throws a
/// JS `Error` with the same structure as `compile`. On a clean gate, applies
/// the lint rules and returns the canonical lint result object.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `opts`: optional configuration object:
///   - `basePath` (string): base directory for resolving `@import` paths.
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///   - `rules` (`Record<string, string>`): per-rule severity overrides
///     (e.g. `{ "shadow-variable": "warn", "unused-variable": "off" }`).
///
/// ## Returns
///
/// On success, the canonical lint JSON object:
/// `{ version: 1, files: [{file, diagnostics: [{rule, severity, message, help, fixable, span?},...]},...], truncated: bool }`
///
/// On failure, throws a JS `Error` with the same structure as `compile`.
#[napi]
pub fn lint(env: Env, source: String, opts: Option<Object>) -> napi::Result<serde_json::Value> {
    check_source_size(&env, &source)?;

    let (base_path, vars, lint_config) = parse_lint_opts(&env, opts)?;

    let result = run_catching(
        &env,
        AssertUnwindSafe(move || {
            mds::lint_str_with(&source, base_path.as_deref(), vars, &lint_config)
        }),
    )?;

    Ok(result.to_canonical_json())
}

/// Lint an MDS template file for static analysis findings.
///
/// ## Arguments
///
/// - `path`: path to the `.mds` file to lint.
/// - `opts`: optional configuration object:
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///   - `rules` (`Record<string, string>`): per-rule severity overrides.
///
/// `basePath` is not accepted — the base directory is derived from the file's
/// own directory.
///
/// ## Returns
///
/// Same shape as `lint`. Dependencies in the returned JSON are absolute filesystem paths.
#[napi(js_name = "lintFile")]
pub fn lint_file(env: Env, path: String, opts: Option<Object>) -> napi::Result<serde_json::Value> {
    let (vars, lint_config) = parse_lint_file_opts(&env, opts)?;

    let path_buf = PathBuf::from(path);
    let result = run_catching(
        &env,
        AssertUnwindSafe(move || mds::lint(&path_buf, vars, &lint_config)),
    )?;

    Ok(result.to_canonical_json())
}

/// Lint a multi-module virtual filesystem for static analysis findings.
///
/// Provides an explicit virtual module map and entry point for lint, enabling
/// callers to lint templates without touching the filesystem — useful for
/// editor integrations, LSP servers, and bundler plugins.
///
/// ## Arguments
///
/// - `modules`: `Record<string, string>` mapping module key → source string.
/// - `entry`: the key within `modules` to use as the lint entry point.
/// - `opts`: optional configuration object:
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///   - `rules` (`Record<string, string>`): per-rule severity overrides.
///
/// ## Returns
///
/// Same shape as `lint`.
#[napi(js_name = "lintVirtual")]
pub fn lint_virtual(
    env: Env,
    modules: serde_json::Value,
    entry: String,
    opts: Option<Object>,
) -> napi::Result<serde_json::Value> {
    // Parse the modules map.
    let serde_json::Value::Object(mods_map) = modules else {
        return Err(throw_options_error(
            &env,
            &format!(
                "modules must be a plain object, got {}",
                mds::json_type_name(&modules)
            ),
        ));
    };

    // Convert and validate, enforcing the same module-count and size caps the WASM
    // and Python bindings apply — the virtual-FS path bypasses the file layer's own
    // size guard, so it must be re-enforced here (defense-in-depth, cross-binding parity).
    if mods_map.len() > MAX_MODULE_COUNT {
        return Err(throw_resource_limit(
            &env,
            &format!(
                "modules exceeds maximum module count of {MAX_MODULE_COUNT} ({} provided)",
                mods_map.len()
            ),
        ));
    }
    let mut mods: HashMap<String, String> = HashMap::with_capacity(mods_map.len());
    let mut aggregate: usize = 0;
    for (key, val) in mods_map {
        let serde_json::Value::String(s) = val else {
            return Err(throw_options_error(
                &env,
                &format!("modules[\"{key}\"] must be a string source"),
            ));
        };
        if s.len() > MAX_SOURCE_SIZE {
            return Err(throw_resource_limit(
                &env,
                &format!(
                    "modules[\"{key}\"] exceeds maximum size of {MAX_SOURCE_SIZE} bytes ({} bytes provided)",
                    s.len()
                ),
            ));
        }
        aggregate = aggregate.saturating_add(s.len());
        if aggregate > MAX_MODULES_AGGREGATE_SIZE {
            return Err(throw_resource_limit(
                &env,
                &format!(
                    "modules aggregate size exceeds maximum of {MAX_MODULES_AGGREGATE_SIZE} bytes"
                ),
            ));
        }
        mods.insert(key, s);
    }

    let (vars, lint_config) = parse_lint_virtual_opts(&env, opts)?;

    let result = run_catching(
        &env,
        AssertUnwindSafe(move || mds::lint_virtual(mods, &entry, vars, &lint_config)),
    )?;

    Ok(result.to_canonical_json())
}
