//! WebAssembly bindings for the MDS compiler.
//!
//! Exposes [`compile`] and [`check`] to JavaScript via `wasm-bindgen`.
//! All compilation runs against an in-memory virtual filesystem — no
//! OS file access occurs inside the WASM boundary.
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
use mds::Value;
use serde::Serialize;
use wasm_bindgen::prelude::*;

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

    // Set code — always present.
    let _ = Reflect::set(&js_err, &JsValue::from_str("code"), &JsValue::from_str(&serialized.code));

    // Set help — only when present.
    if let Some(help) = &serialized.help {
        let _ = Reflect::set(&js_err, &JsValue::from_str("help"), &JsValue::from_str(help));
    }

    // Set span — only when present.
    if let Some(span) = &serialized.span {
        let span_obj = js_sys::Object::new();
        let _ = Reflect::set(
            &span_obj,
            &JsValue::from_str("offset"),
            &JsValue::from_f64(span.offset as f64),
        );
        let _ = Reflect::set(
            &span_obj,
            &JsValue::from_str("length"),
            &JsValue::from_f64(span.length as f64),
        );
        if let Some(line) = span.line {
            let _ = Reflect::set(
                &span_obj,
                &JsValue::from_str("line"),
                &JsValue::from_f64(line as f64),
            );
        }
        if let Some(column) = span.column {
            let _ = Reflect::set(
                &span_obj,
                &JsValue::from_str("column"),
                &JsValue::from_f64(column as f64),
            );
        }
        let _ = Reflect::set(&js_err, &JsValue::from_str("span"), &span_obj);
    }

    js_err.into()
}

/// Wrap a fallible closure in `catch_unwind` to prevent panics from aborting
/// the WASM module. Panics are converted to JS `Error` values with
/// `code = "mds::internal"`.
///
/// `AssertUnwindSafe` is required because the closure captures data that is
/// not `UnwindSafe` by default (e.g. `String`, `HashMap`). Callers ensure
/// this is safe by cloning all captured data before calling `catch_panic`.
fn catch_panic<F, T>(f: F) -> Result<T, JsValue>
where
    F: std::panic::UnwindSafe + FnOnce() -> Result<T, JsValue>,
{
    std::panic::catch_unwind(f).unwrap_or_else(|payload| {
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            format!("internal compiler panic: {s}")
        } else if let Some(s) = payload.downcast_ref::<String>() {
            format!("internal compiler panic: {s}")
        } else {
            "internal compiler panic: unknown internal error".to_string()
        };

        let js_err = js_sys::Error::new(&msg);
        let _ = Reflect::set(
            &js_err,
            &JsValue::from_str("code"),
            &JsValue::from_str("mds::internal"),
        );
        Err(js_err.into())
    })
}

// ── Options parsing ───────────────────────────────────────────────────────────

/// Parsed options extracted from the JS options object.
struct ParsedOptions {
    filename: String,
    extra_modules: HashMap<String, String>,
    vars: Option<HashMap<String, Value>>,
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
        return Ok(ParsedOptions {
            filename: "input.mds".to_string(),
            extra_modules: HashMap::new(),
            vars: None,
        });
    }

    // Deserialize options object → serde_json::Value for structured access.
    let opts_json: serde_json::Value = serde_wasm_bindgen::from_value(options).map_err(|e| {
        let js_err = js_sys::Error::new(&format!("invalid options: {e}"));
        let _ = Reflect::set(
            &js_err,
            &JsValue::from_str("code"),
            &JsValue::from_str("mds::invalid_options"),
        );
        JsValue::from(js_err)
    })?;

    let serde_json::Value::Object(map) = &opts_json else {
        let js_err = js_sys::Error::new("options must be a plain object");
        let _ = Reflect::set(
            &js_err,
            &JsValue::from_str("code"),
            &JsValue::from_str("mds::invalid_options"),
        );
        return Err(js_err.into());
    };

    // Extract filename (string, default "input.mds").
    let filename = match map.get("filename") {
        Some(serde_json::Value::String(s)) => s.clone(),
        None => "input.mds".to_string(),
        Some(other) => {
            let js_err = js_sys::Error::new(&format!(
                "options.filename must be a string, got {}",
                json_type_name(other)
            ));
            let _ = Reflect::set(
                &js_err,
                &JsValue::from_str("code"),
                &JsValue::from_str("mds::invalid_options"),
            );
            return Err(js_err.into());
        }
    };

    // Validate filename is non-empty.
    if filename.trim().is_empty() {
        let js_err = js_sys::Error::new("options.filename must be a non-empty string");
        let _ = Reflect::set(
            &js_err,
            &JsValue::from_str("code"),
            &JsValue::from_str("mds::invalid_options"),
        );
        return Err(js_err.into());
    }

    // Extract modules (Record<string, string>, default empty).
    let extra_modules = match map.get("modules") {
        Some(serde_json::Value::Object(mods)) => {
            let mut result = HashMap::with_capacity(mods.len());
            for (key, val) in mods {
                match val {
                    serde_json::Value::String(s) => {
                        result.insert(key.clone(), s.clone());
                    }
                    other => {
                        let js_err = js_sys::Error::new(&format!(
                            "options.modules[\"{key}\"] must be a string, got {}",
                            json_type_name(other)
                        ));
                        let _ = Reflect::set(
                            &js_err,
                            &JsValue::from_str("code"),
                            &JsValue::from_str("mds::invalid_options"),
                        );
                        return Err(js_err.into());
                    }
                }
            }
            result
        }
        None => HashMap::new(),
        Some(other) => {
            let js_err = js_sys::Error::new(&format!(
                "options.modules must be a plain object, got {}",
                json_type_name(other)
            ));
            let _ = Reflect::set(
                &js_err,
                &JsValue::from_str("code"),
                &JsValue::from_str("mds::invalid_options"),
            );
            return Err(js_err.into());
        }
    };

    // Extract vars (Record<string, any>, default None).
    let vars = match map.get("vars") {
        Some(serde_json::Value::Object(vars_map)) => {
            let mut result = HashMap::with_capacity(vars_map.len());
            for (key, val) in vars_map {
                let mds_val = Value::from_json(val.clone()).map_err(mds_error_to_js)?;
                result.insert(key.clone(), mds_val);
            }
            Some(result)
        }
        None => None,
        Some(other) => {
            let js_err = js_sys::Error::new(&format!(
                "options.vars must be a plain object, got {}",
                json_type_name(other)
            ));
            let _ = Reflect::set(
                &js_err,
                &JsValue::from_str("code"),
                &JsValue::from_str("mds::invalid_options"),
            );
            return Err(js_err.into());
        }
    };

    Ok(ParsedOptions { filename, extra_modules, vars })
}

/// Return a human-readable JSON value type name for error diagnostics.
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
        let js_err = js_sys::Error::new(&format!(
            "options.modules already contains key \"{filename}\"; this would shadow the source — use a different filename"
        ));
        let _ = Reflect::set(
            &js_err,
            &JsValue::from_str("code"),
            &JsValue::from_str("mds::filename_collision"),
        );
        return Err(js_err.into());
    }

    let mut modules = extra_modules;
    modules.insert(filename.to_string(), source);
    Ok(modules)
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
        .map_err(|e| {
            let js_err = js_sys::Error::new(&format!("failed to serialize result: {e}"));
            JsValue::from(js_err)
        })
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
    // source must be cloned into an owned String so the closure is UnwindSafe.
    let source = source.to_string();

    catch_panic(AssertUnwindSafe(move || {
        let opts = parse_options(options)?;
        let modules = build_modules(source, &opts.filename, opts.extra_modules)?;
        let result =
            mds::compile_virtual_with_deps(modules, &opts.filename, opts.vars)
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
    // source must be cloned into an owned String so the closure is UnwindSafe.
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
