//! WebAssembly bindings for the MDS compiler.
//!
//! Exposes [`compile`] and [`check`] to JavaScript via `wasm-bindgen`.
//! All compilation runs against an in-memory virtual filesystem — no
//! OS file access occurs inside the WASM boundary.
//!
//! ## Error codes
//!
//! Errors thrown at the WASM boundary carry a `code` property. Codes that
//! originate inside `mds-core` (e.g. `"mds::syntax"`) are defined by
//! [`mds::MdsError`]. The following codes are **WASM-only** — they are
//! synthesised here and do not exist in the core crate:
//!
//! | Code                      | Meaning                                          |
//! |---------------------------|--------------------------------------------------|
//! | `mds::internal`           | Unexpected panic caught at the WASM boundary     |
//! | `mds::invalid_options`    | Malformed or type-incorrect options object       |
//! | `mds::resource_limit`     | Input exceeds an enforced size limit             |
//! | `mds::filename_collision` | `options.modules` key collides with `filename`   |
//!
//! ## Usage (JavaScript)
//!
//! ```js
//! import init, { compile, check } from 'mds-wasm';
//!
//! await init();
//!
//! const result = compile('Hello {name}!\n', {
//!   vars: { name: 'World' },
//!   filename: 'input.mds',
//! });
//! console.log(result.output); // "Hello World!\n"
//!
//! check('Hello {name}!\n', { vars: { name: 'World' } });
//! ```

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;

use js_sys::Reflect;
use mds::{format_unknown_keys_error, json_type_name, parse_json_vars, Value, VarsError};
use serde::Serialize;
use wasm_bindgen::prelude::*;

// ── Resource limits ───────────────────────────────────────────────────────────

/// Maximum source string size accepted at the WASM boundary (10 MiB).
///
/// Mirrors `mds::MAX_FILE_SIZE`. The WASM boundary bypasses the file layer,
/// so the limit must be re-enforced here to prevent memory exhaustion.
const MAX_SOURCE_SIZE: usize = mds::MAX_FILE_SIZE as usize;

/// Maximum number of module entries in `options.modules`.
///
/// Prevents a caller from exhausting WASM linear memory by passing thousands
/// of small modules. 256 modules is well above any realistic template graph.
const MAX_MODULE_COUNT: usize = 256;

/// Maximum aggregate byte size of all module values combined (same as source limit).
const MAX_MODULES_AGGREGATE_SIZE: usize = MAX_SOURCE_SIZE;

// ── Defaults ─────────────────────────────────────────────────────────────────

/// Default filename used when the caller does not supply `options.filename`.
const DEFAULT_FILENAME: &str = "input.mds";

// ── JS interop primitives ─────────────────────────────────────────────────────

/// Set a property on a JS object, asserting success in debug builds.
///
/// `Reflect::set` only fails on non-extensible or frozen objects; we never
/// pass those, so failure is a programming error. The debug assertion catches
/// it during development without adding overhead in release.
fn set_prop(target: &JsValue, key: &str, value: &JsValue) {
    let ok = Reflect::set(target, &JsValue::from_str(key), value).unwrap_or(false);
    debug_assert!(ok, "Reflect::set failed for key {key:?}");
}

/// Build a JS `Error` with a `code` property.
///
/// Every error thrown at the WASM boundary carries `code` so callers can
/// branch programmatically (e.g. `if (err.code === "mds::syntax") …`).
fn js_error(message: &str, code: &str) -> JsValue {
    let err = js_sys::Error::new(message);
    set_prop(&err, "code", &JsValue::from_str(code));
    err.into()
}

/// Shorthand for a `js_error` with `code = "mds::invalid_options"`.
fn options_error(message: &str) -> JsValue {
    js_error(message, "mds::invalid_options")
}

// ── Error conversion helpers ──────────────────────────────────────────────────

/// Convert an [`mds::MdsError`] into a JS `Error` with structured metadata.
///
/// The returned object is a `js_sys::Error` with additional properties:
/// - `code`: diagnostic code string (e.g. `"mds::syntax"`)
/// - `help`: optional hint string (may be undefined)
/// - `span`: optional `{ offset, length, line, column }` object (may be undefined)
fn mds_error_to_js(err: mds::MdsError) -> JsValue {
    let serialized = err.serialize();

    let js_err = js_sys::Error::new(&serialized.message);
    set_prop(&js_err, "code", &JsValue::from_str(&serialized.code));

    if let Some(help) = &serialized.help {
        set_prop(&js_err, "help", &JsValue::from_str(help));
    }

    if let Some(span) = &serialized.span {
        let span_obj = span_to_js(span);
        set_prop(&js_err, "span", &span_obj);
    }

    js_err.into()
}

/// Serialise a [`mds::SerializedSpan`] into a plain JS object.
///
/// Always sets `offset` and `length`; sets `line` and `column` only when
/// the compiler was able to resolve them from the source text.
fn span_to_js(span: &mds::SerializedSpan) -> js_sys::Object {
    let obj = js_sys::Object::new();
    set_prop(&obj, "offset", &JsValue::from_f64(span.offset as f64));
    set_prop(&obj, "length", &JsValue::from_f64(span.length as f64));
    if let Some(line) = span.line {
        set_prop(&obj, "line", &JsValue::from_f64(line as f64));
    }
    if let Some(column) = span.column {
        set_prop(&obj, "column", &JsValue::from_f64(column as f64));
    }
    obj
}

/// Wrap a fallible closure in `catch_unwind` to prevent panics from aborting
/// the WASM module. Panics are converted to JS `Error` values with
/// `code = "mds::internal"`.
///
/// `AssertUnwindSafe` is required because the closure captures data that is
/// not `UnwindSafe` by default (e.g. `String`, `HashMap`). Callers ensure
/// this is safe by cloning all captured data before calling `catch_panic`.
///
/// The public error message is deliberately generic to avoid leaking internal
/// paths or assertion details. The raw panic payload is attached as `detail`
/// for debugging purposes only — callers should not rely on its format.
fn catch_panic<F, T>(f: F) -> Result<T, JsValue>
where
    F: std::panic::UnwindSafe + FnOnce() -> Result<T, JsValue>,
{
    std::panic::catch_unwind(f).unwrap_or_else(|payload| {
        let err = js_error("internal compiler error", "mds::internal");

        // The raw panic payload may contain absolute filesystem paths from Rust
        // source locations or assertion messages. Only expose it when the
        // `debug-panics` feature is enabled so that production WASM builds
        // never leak internal paths to untrusted JS callers.
        #[cfg(feature = "debug-panics")]
        {
            let detail = if let Some(s) = payload.downcast_ref::<&str>() {
                JsValue::from_str(s)
            } else if let Some(s) = payload.downcast_ref::<String>() {
                JsValue::from_str(s)
            } else {
                JsValue::from_str("unknown panic payload")
            };
            set_prop(&err, "detail", &detail);
        }

        // Suppress unused-variable warning in non-debug builds.
        #[cfg(not(feature = "debug-panics"))]
        let _ = payload;

        Err(err)
    })
}

// ── Options parsing ───────────────────────────────────────────────────────────

/// Parsed options extracted from the JS options object.
struct ParsedOptions {
    filename: String,
    extra_modules: HashMap<String, String>,
    vars: Option<HashMap<String, Value>>,
}

impl Default for ParsedOptions {
    fn default() -> Self {
        ParsedOptions {
            filename: DEFAULT_FILENAME.to_string(),
            extra_modules: HashMap::new(),
            vars: None,
        }
    }
}

/// Reject any JS object key that is not in the `known` list.
///
/// Iterates `js_sys::Object::keys(obj)` and collects all unknowns before
/// returning, so the error message names every offending key at once.
fn reject_unknown_wasm_keys(obj: &js_sys::Object, known: &[&str]) -> Result<(), JsValue> {
    let key_array = js_sys::Object::keys(obj);
    // Collect owned strings first; as_string() returns Option<String>.
    let unknown_owned: Vec<String> = (0..key_array.length())
        .filter_map(|i| key_array.get(i).as_string())
        .filter(|k| !known.contains(&k.as_str()))
        .collect();
    if unknown_owned.is_empty() {
        return Ok(());
    }
    let unknowns: Vec<&str> = unknown_owned.iter().map(String::as_str).collect();
    Err(options_error(&format_unknown_keys_error(&unknowns, known)))
}

/// Get a property from a JS object via Reflect, returning `JsValue::UNDEFINED` on failure.
fn get_prop_js(obj: &js_sys::Object, key: &str) -> JsValue {
    Reflect::get(obj, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

/// Extract and validate the `filename` field from the options object.
fn extract_filename(obj: &js_sys::Object) -> Result<String, JsValue> {
    let val = get_prop_js(obj, "filename");
    if val.is_undefined() || val.is_null() {
        return Ok(DEFAULT_FILENAME.to_string());
    }
    match val.as_string() {
        Some(s) if s.trim().is_empty() => {
            Err(options_error("options.filename must be a non-empty string"))
        }
        Some(s) => Ok(s),
        None => Err(options_error(&format!(
            "options.filename must be a string, got {}",
            js_type_name(&val)
        ))),
    }
}

/// Return a human-readable JS value type name for error diagnostics.
fn js_type_name(v: &JsValue) -> &'static str {
    if v.is_null() {
        "null"
    } else if v.is_undefined() {
        "undefined"
    } else if v.as_bool().is_some() {
        "boolean"
    } else if v.as_f64().is_some() {
        "number"
    } else if v.as_string().is_some() {
        "string"
    } else if js_sys::Array::is_array(v) {
        "array"
    } else {
        "object"
    }
}

/// Extract and validate the `modules` field from the options object.
///
/// Deserializes only the modules sub-value via serde_wasm_bindgen for
/// structured access. Enforces module count and size limits.
fn extract_modules(obj: &js_sys::Object) -> Result<HashMap<String, String>, JsValue> {
    let val = get_prop_js(obj, "modules");
    if val.is_undefined() || val.is_null() {
        return Ok(HashMap::new());
    }
    // Deserialize only the modules sub-object.
    let modules_json: serde_json::Value = serde_wasm_bindgen::from_value(val)
        .map_err(|e| options_error(&format!("invalid options.modules: {e}")))?;
    // Reuse the existing parse_modules logic on the deserialized sub-map.
    let serde_json::Value::Object(mods_map) = modules_json else {
        return Err(options_error(&format!(
            "options.modules must be a plain object, got {}",
            json_type_name(&modules_json)
        )));
    };
    parse_modules_from_map(mods_map)
}

/// Parse a modules map (after deserialization) into HashMap<String, String>.
///
/// Extracted from the original `parse_modules` to allow reuse by `extract_modules`.
fn parse_modules_from_map(
    mods: serde_json::Map<String, serde_json::Value>,
) -> Result<HashMap<String, String>, JsValue> {
    if mods.len() > MAX_MODULE_COUNT {
        return Err(js_error(
            &format!(
                "options.modules exceeds maximum module count of {} ({} provided)",
                MAX_MODULE_COUNT,
                mods.len()
            ),
            "mds::resource_limit",
        ));
    }

    let mut result = HashMap::with_capacity(mods.len());
    let mut aggregate_size: usize = 0;

    for (key, val) in mods {
        match val {
            serde_json::Value::String(s) => {
                if s.len() > MAX_SOURCE_SIZE {
                    return Err(js_error(
                        &format!(
                            "options.modules[\"{key}\"] exceeds maximum size of {} bytes ({} bytes provided)",
                            MAX_SOURCE_SIZE,
                            s.len()
                        ),
                        "mds::resource_limit",
                    ));
                }
                aggregate_size = aggregate_size.saturating_add(s.len());
                if aggregate_size > MAX_MODULES_AGGREGATE_SIZE {
                    return Err(js_error(
                        &format!(
                            "options.modules aggregate size exceeds maximum of {} bytes",
                            MAX_MODULES_AGGREGATE_SIZE
                        ),
                        "mds::resource_limit",
                    ));
                }
                result.insert(key, s);
            }
            other => {
                return Err(options_error(&format!(
                    "options.modules[\"{key}\"] must be a string, got {}",
                    json_type_name(&other)
                )));
            }
        }
    }
    Ok(result)
}

/// Extract and validate the `vars` field from the options object.
///
/// Deserializes only the vars sub-value via serde_wasm_bindgen, then
/// delegates to the shared `parse_json_vars` from mds-core.
fn extract_vars(obj: &js_sys::Object) -> Result<Option<HashMap<String, Value>>, JsValue> {
    let val = get_prop_js(obj, "vars");
    if val.is_undefined() || val.is_null() {
        return Ok(None);
    }
    // Deserialize only the vars sub-value.
    let vars_json: serde_json::Value = serde_wasm_bindgen::from_value(val)
        .map_err(|e| options_error(&format!("invalid options.vars: {e}")))?;
    parse_json_vars(vars_json).map(Some).map_err(|e| match e {
        VarsError::InvalidType(msg) => options_error(&msg),
        VarsError::Conversion(mds_err) => mds_error_to_js(mds_err),
    })
}

/// Parse the JS options argument into structured Rust data.
///
/// - `options` may be `null` or `undefined` — all fields default.
/// - `filename`: string key for the source in the virtual FS; default `"input.mds"`.
/// - `modules`: `Record<string, string>` of additional virtual files.
/// - `vars`: `Record<string, any>` of runtime variable overrides.
fn parse_options(options: JsValue) -> Result<ParsedOptions, JsValue> {
    // null / undefined → all defaults
    if options.is_null() || options.is_undefined() {
        return Ok(ParsedOptions::default());
    }

    // Reject non-objects (including arrays).
    if !options.is_object() || js_sys::Array::is_array(&options) {
        return Err(options_error("options must be a plain object"));
    }

    // SAFETY: we verified options.is_object() above.
    let obj: js_sys::Object = options.unchecked_into();

    reject_unknown_wasm_keys(&obj, &["filename", "modules", "vars"])?;

    let filename = extract_filename(&obj)?;
    let extra_modules = extract_modules(&obj)?;
    let vars = extract_vars(&obj)?;

    Ok(ParsedOptions {
        filename,
        extra_modules,
        vars,
    })
}

/// Build the virtual filesystem module map.
///
/// Inserts `source` under `filename`, then merges `extra_modules`. Returns
/// an error if `extra_modules` already contains `filename` (collision).
fn build_modules(
    source: String,
    filename: &str,
    extra_modules: HashMap<String, String>,
) -> Result<HashMap<String, String>, JsValue> {
    if extra_modules.contains_key(filename) {
        return Err(js_error(
            &format!(
                "options.modules already contains key \"{filename}\"; this would shadow the source — use a different filename"
            ),
            "mds::filename_collision",
        ));
    }

    let mut modules = extra_modules;
    modules.insert(filename.to_string(), source);
    Ok(modules)
}

/// Guard against oversized source inputs before entering the compilation path.
fn check_source_size(source: &str) -> Result<(), JsValue> {
    if source.len() > MAX_SOURCE_SIZE {
        return Err(js_error(
            &format!(
                "source exceeds maximum size of {} bytes ({} bytes provided)",
                MAX_SOURCE_SIZE,
                source.len()
            ),
            "mds::resource_limit",
        ));
    }
    Ok(())
}

// ── Output types ──────────────────────────────────────────────────────────────

/// Serializable output for the `check` function.
#[derive(Serialize)]
struct CheckOutput {
    warnings: Vec<String>,
}

/// Serialize a value to JS using the JSON-compatible serializer.
///
/// This ensures maps/structs become plain JS objects (not `Map` instances),
/// matching the behavior JavaScript callers expect from a JSON-like API.
fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| js_error(&format!("failed to serialize result: {e}"), "mds::internal"))
}

// ── Public WASM exports ───────────────────────────────────────────────────────

/// Compile an MDS template source string and return a structured result object.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `options`: optional configuration object with the following optional fields:
///   - `filename` (string, default `"input.mds"`): the entry module key.
///   - `modules` (`Record<string, string>`): additional virtual modules for import resolution.
///   - `vars` (`Record<string, any>`): runtime variable overrides.
///
/// ## Returns
///
/// On success, a JS object `{ output: string, warnings: string[], dependencies: string[] }`.
///
/// On failure, throws a JS `Error` with additional properties:
/// - `code`: diagnostic code (e.g. `"mds::syntax"`)
/// - `help`: optional hint (may be absent)
/// - `span`: optional `{ offset, length, line?, column? }` (may be absent)
///
/// ## Example (JavaScript)
///
/// ```js
/// const result = compile('Hello {name}!\n', { vars: { name: 'World' } });
/// console.log(result.output); // "Hello World!\n"
/// ```
#[wasm_bindgen]
pub fn compile(source: &str, options: JsValue) -> Result<JsValue, JsValue> {
    check_source_size(source)?;

    // Owned String required so the closure satisfies UnwindSafe.
    let source = source.to_string();

    catch_panic(AssertUnwindSafe(move || {
        let opts = parse_options(options)?;
        let modules = build_modules(source, &opts.filename, opts.extra_modules)?;
        let result = mds::compile_virtual_with_deps(modules, &opts.filename, opts.vars)
            .map_err(mds_error_to_js)?;

        to_js(&result)
    }))
}

/// Check (validate) an MDS template source string without rendering output.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `options`: optional configuration object (same fields as [`compile`]).
///
/// ## Returns
///
/// On success, a JS object `{ warnings: string[] }`.
///
/// On failure, throws a JS `Error` with the same structure as [`compile`].
///
/// ## Example (JavaScript)
///
/// ```js
/// const result = check('---\nname: World\n---\nHello {name}!\n');
/// console.log(result.warnings); // []
/// ```
#[wasm_bindgen]
pub fn check(source: &str, options: JsValue) -> Result<JsValue, JsValue> {
    check_source_size(source)?;

    // Owned String required so the closure satisfies UnwindSafe.
    let source = source.to_string();

    catch_panic(AssertUnwindSafe(move || {
        let opts = parse_options(options)?;
        let modules = build_modules(source, &opts.filename, opts.extra_modules)?;
        let ((), warnings) =
            mds::check_virtual_collecting_warnings(modules, &opts.filename, opts.vars)
                .map_err(mds_error_to_js)?;

        to_js(&CheckOutput { warnings })
    }))
}

/// Compile an MDS template in messages mode, returning structured chat messages.
///
/// Each `@message role:` ... `@end` block in the template becomes one entry in
/// the returned `messages` array. Orphan text outside `@message` blocks is
/// ignored with a warning. Empty messages (after trimming) are skipped.
///
/// Returns an error when the template contains no `@message` blocks.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
/// - `options`: optional configuration object (same fields as [`compile`]).
///
/// ## Returns
///
/// On success, a JS object:
/// ```json
/// {
///   "messages": [{ "role": "system", "content": "..." }, ...],
///   "warnings": [],
///   "dependencies": []
/// }
/// ```
///
/// On failure, throws a JS `Error` with the same structure as [`compile`].
///
/// ## Example (JavaScript)
///
/// ```js
/// const result = compileMessages('@message system:\nYou are helpful.\n@end\n');
/// console.log(result.messages[0].role);    // "system"
/// console.log(result.messages[0].content); // "You are helpful."
/// ```
#[wasm_bindgen(js_name = "compileMessages")]
pub fn compile_messages(source: &str, options: JsValue) -> Result<JsValue, JsValue> {
    check_source_size(source)?;

    // Owned String required so the closure satisfies UnwindSafe.
    let source = source.to_string();

    catch_panic(AssertUnwindSafe(move || {
        let opts = parse_options(options)?;
        let modules = build_modules(source, &opts.filename, opts.extra_modules)?;
        let result = mds::compile_messages_virtual_with_deps(modules, &opts.filename, opts.vars)
            .map_err(mds_error_to_js)?;

        to_js(&result)
    }))
}

/// Extract all import and re-export paths from an MDS source string.
///
/// Returns an array of path strings (`string[]`) in insertion order, deduplicated.
/// Does not resolve paths to the filesystem — returns them as-is from the source.
///
/// ## Arguments
///
/// - `source`: MDS template source text.
///
/// ## Returns
///
/// On success, a JS array of strings (e.g. `["./foo.mds", "./bar.mds"]`).
///
/// On failure, throws a JS `Error` with the same structure as [`compile`].
///
/// ## Example (JavaScript)
///
/// ```js
/// const paths = scanImports('@import "./foo.mds"\n@import { bar } from "./bar.mds"\n');
/// console.log(paths); // ["./foo.mds", "./bar.mds"]
/// ```
#[wasm_bindgen(js_name = "scanImports")]
pub fn scan_imports(source: &str) -> Result<JsValue, JsValue> {
    check_source_size(source)?;

    // Owned String required so the closure satisfies UnwindSafe.
    let source = source.to_string();

    catch_panic(AssertUnwindSafe(move || {
        let paths = mds::scan_imports(&source).map_err(mds_error_to_js)?;
        serde_wasm_bindgen::to_value(&paths)
            .map_err(|e| js_error(&format!("failed to serialize result: {e}"), "mds::internal"))
    }))
}
