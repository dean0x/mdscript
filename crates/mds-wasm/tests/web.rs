use serde::Serialize;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

// Tests run in Node.js via `wasm-pack test --node crates/mds-wasm`
wasm_bindgen_test_configure!(run_in_node_experimental);

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Get a named property from a JS object, returning JsValue::UNDEFINED if not found.
fn get_prop(obj: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(obj, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

/// Get a string property from a JS object.
fn get_str(obj: &JsValue, key: &str) -> String {
    get_prop(obj, key)
        .as_string()
        .unwrap_or_else(|| format!("<not a string: {key}>"))
}

/// Serialize to a plain JS object (not a `Map`).
///
/// `serde_wasm_bindgen::to_value` defaults to `Map` for JSON objects.
/// Real JS callers pass plain object literals, so tests must match that.
fn to_js_object(v: &impl Serialize) -> JsValue {
    v.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .unwrap()
}

/// Build a simple JS options object from a vars record.
fn vars_opts(vars: &serde_json::Value) -> JsValue {
    to_js_object(&serde_json::json!({ "vars": vars }))
}

/// Build an options object with extra modules.
fn modules_opts(modules: &serde_json::Value) -> JsValue {
    to_js_object(&serde_json::json!({ "modules": modules }))
}

/// Build an options object with filename.
fn filename_opts(filename: &str) -> JsValue {
    to_js_object(&serde_json::json!({ "filename": filename }))
}

// ── compile tests ─────────────────────────────────────────────────────────────

#[wasm_bindgen_test]
fn compile_simple_no_options() {
    let result = mds_wasm::compile("Hello World!\n", JsValue::NULL).unwrap();
    let output = get_str(&result, "output");
    assert_eq!(output, "Hello World!\n", "unexpected output: {output}");
}

#[wasm_bindgen_test]
fn compile_undefined_options() {
    let result = mds_wasm::compile("Hello!\n", JsValue::UNDEFINED).unwrap();
    let output = get_str(&result, "output");
    assert_eq!(output, "Hello!\n");
}

#[wasm_bindgen_test]
fn compile_with_frontmatter_vars() {
    let source = "---\nname: World\n---\nHello {{name}}!\n";
    let result = mds_wasm::compile(source, JsValue::NULL).unwrap();
    let output = get_str(&result, "output");
    assert!(output.contains("Hello World!"), "got: {output}");
}

#[wasm_bindgen_test]
fn compile_with_runtime_vars() {
    let source = "Hello {{name}}!\n";
    let opts = vars_opts(&serde_json::json!({ "name": "Rust" }));
    let result = mds_wasm::compile(source, opts).unwrap();
    let output = get_str(&result, "output");
    assert_eq!(output, "Hello Rust!\n", "got: {output}");
}

#[wasm_bindgen_test]
fn compile_with_modules_import() {
    // VirtualFs normalizes "./lib.mds" from "input.mds" to "lib.mds",
    // so the module key must be "lib.mds".
    let source = "@import \"./lib.mds\"\n{{greet(\"World\")}}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {{x}}!\n@end\n"
    }));
    let result = mds_wasm::compile(source, opts).unwrap();
    let output = get_str(&result, "output");
    assert!(output.contains("Hello World!"), "got: {output}");
}

#[wasm_bindgen_test]
fn compile_has_warnings_field() {
    let result = mds_wasm::compile("Hello!\n", JsValue::NULL).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "warnings must be an array"
    );
}

#[wasm_bindgen_test]
fn compile_has_dependencies_field() {
    let result = mds_wasm::compile("Hello!\n", JsValue::NULL).unwrap();
    let deps = get_prop(&result, "dependencies");
    assert!(
        js_sys::Array::is_array(&deps),
        "dependencies must be an array"
    );
}

#[wasm_bindgen_test]
fn compile_dependencies_contains_imported_module() {
    let source = "@import \"./lib.mds\"\n{{greet(\"World\")}}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {{x}}!\n@end\n"
    }));
    let result = mds_wasm::compile(source, opts).unwrap();
    let deps_val = get_prop(&result, "dependencies");
    let deps = js_sys::Array::from(&deps_val);
    let dep_strings: Vec<String> = (0..deps.length())
        .map(|i| deps.get(i).as_string().unwrap_or_default())
        .collect();
    assert!(
        dep_strings.iter().any(|s| s.contains("lib.mds")),
        "dependencies must contain 'lib.mds'; got: {dep_strings:?}"
    );
}

#[wasm_bindgen_test]
fn compile_custom_filename() {
    let source = "Hello!\n";
    let opts = filename_opts("my-template.mds");
    let result = mds_wasm::compile(source, opts).unwrap();
    let output = get_str(&result, "output");
    assert_eq!(output, "Hello!\n");
}

#[wasm_bindgen_test]
fn compile_runtime_vars_override_frontmatter() {
    let source = "---\nname: Old\n---\nHello {{name}}!\n";
    let opts = vars_opts(&serde_json::json!({ "name": "New" }));
    let result = mds_wasm::compile(source, opts).unwrap();
    let output = get_str(&result, "output");
    assert!(output.contains("Hello New!"), "got: {output}");
}

// ── compile error tests ───────────────────────────────────────────────────────

/// Source string shared by all error-path tests.
///
/// The interpolation `{{undefined_var}}` starts at byte offset 6 (after
/// `"Hello "`). The compiler reports a span with offset=6 (first `{`) and
/// length=13 (the trimmed inner identifier `undefined_var`).
/// Tests that assert exact span values rely on these positions.
const UNDEFINED_VAR_SOURCE: &str = "Hello {{undefined_var}}!\n";

/// Compile `UNDEFINED_VAR_SOURCE` and return the resulting JS error.
fn compile_undefined_var_err() -> JsValue {
    mds_wasm::compile(UNDEFINED_VAR_SOURCE, JsValue::NULL).unwrap_err()
}

#[wasm_bindgen_test]
fn compile_undefined_variable_returns_error() {
    let err = compile_undefined_var_err();
    let msg = get_str(&err, "message");
    assert!(!msg.is_empty(), "error message should not be empty");
}

#[wasm_bindgen_test]
fn compile_error_has_code_property() {
    let err = compile_undefined_var_err();
    let code = get_str(&err, "code");
    assert!(!code.is_empty(), "error.code must be set");
    assert!(
        code.starts_with("mds::"),
        "code must start with 'mds::': {code}"
    );
}

#[wasm_bindgen_test]
fn compile_error_is_js_error() {
    // Verify the thrown value is an instanceof Error by checking it has a message property.
    let err = compile_undefined_var_err();
    let msg = get_prop(&err, "message");
    assert!(
        msg.as_string().is_some(),
        "error.message must be a string, got: {msg:?}"
    );
}

#[wasm_bindgen_test]
fn compile_error_has_span_with_offset_and_length() {
    // UndefinedVariable is emitted with a source span pointing at the variable reference.
    // In UNDEFINED_VAR_SOURCE ("Hello {undefined_var}!\n"):
    //   - The interpolation "{undefined_var}" starts at byte offset 6 (after "Hello ").
    //   - The compiler emits a span with offset=6 and length=13, covering the
    //     opening brace plus the identifier name ("undefined_var" is 13 bytes).
    //     The closing "}" is not included in the span length.
    let err = compile_undefined_var_err();
    let span = get_prop(&err, "span");
    assert!(
        !span.is_undefined() && !span.is_null(),
        "error.span must be present for an UndefinedVariable error"
    );
    let offset = get_prop(&span, "offset")
        .as_f64()
        .expect("span.offset must be a number") as usize;
    let length = get_prop(&span, "length")
        .as_f64()
        .expect("span.length must be a number") as usize;
    // Assert exact byte positions so regressions in span calculation are caught.
    assert_eq!(
        offset, 6,
        "span.offset must be 6 (start of '{{{{undefined_var}}}}' in source)"
    );
    assert_eq!(
        length, 13,
        "span.length must be 13 (byte length of 'undefined_var' identifier)"
    );
}

#[wasm_bindgen_test]
fn compile_error_span_has_line_and_column() {
    // When a source span is present and src is available, line and column are resolved.
    let err = compile_undefined_var_err();
    let span = get_prop(&err, "span");
    assert!(!span.is_undefined(), "span must be present");
    let line = get_prop(&span, "line")
        .as_f64()
        .expect("span.line must be a number when source is available") as usize;
    let column = get_prop(&span, "column")
        .as_f64()
        .expect("span.column must be a number when source is available");
    // Line and column are 1-indexed.
    assert_eq!(line, 1, "span.line should be 1 for single-line source");
    assert!(column >= 1.0, "span.column must be >= 1");
}

#[wasm_bindgen_test]
fn compile_error_has_help_for_undefined_variable() {
    // UndefinedVariable carries a static help hint from the diagnostic attribute.
    let err = compile_undefined_var_err();
    let code = get_str(&err, "code");
    assert_eq!(
        code, "mds::undefined_var",
        "expected undefined_var error: {code}"
    );
    let help = get_prop(&err, "help")
        .as_string()
        .expect("error.help must be a string for UndefinedVariable");
    assert!(!help.is_empty(), "error.help must not be empty");
}

#[wasm_bindgen_test]
fn compile_source_too_large_returns_resource_limit() {
    // MAX_SOURCE_SIZE mirrors mds::MAX_FILE_SIZE (10 MiB). A source one byte
    // over the limit must be rejected before compilation begins.
    let big = "x".repeat(mds::MAX_FILE_SIZE as usize + 1);
    let err = mds_wasm::compile(&big, JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::resource_limit", "got: {code}");
}

#[wasm_bindgen_test]
fn check_source_too_large_returns_resource_limit() {
    // Same guard is enforced on the check() path.
    let big = "x".repeat(mds::MAX_FILE_SIZE as usize + 1);
    let err = mds_wasm::check(&big, JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::resource_limit", "got: {code}");
}

// ── check tests ───────────────────────────────────────────────────────────────

#[wasm_bindgen_test]
fn check_valid_template() {
    let result = mds_wasm::check("Hello!\n", JsValue::NULL).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "warnings must be an array"
    );
}

#[wasm_bindgen_test]
fn check_with_frontmatter_vars() {
    let source = "---\nname: World\n---\nHello {{name}}!\n";
    let result = mds_wasm::check(source, JsValue::NULL).unwrap();
    let warnings_arr = js_sys::Array::from(&get_prop(&result, "warnings"));
    assert_eq!(warnings_arr.length(), 0, "should have no warnings");
}

#[wasm_bindgen_test]
fn check_invalid_template_returns_error() {
    let err = mds_wasm::check(UNDEFINED_VAR_SOURCE, JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert!(!code.is_empty(), "error.code must be set");
}

#[wasm_bindgen_test]
fn check_error_has_code_property() {
    let err = mds_wasm::check(UNDEFINED_VAR_SOURCE, JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert!(
        code.starts_with("mds::"),
        "code must start with 'mds::': {code}"
    );
}

#[wasm_bindgen_test]
fn check_with_modules_import() {
    // check() exercises check_virtual_collecting_warnings, a different code path
    // from compile_virtual_with_deps; module resolution must work through it too.
    let source = "@import \"./lib.mds\"\n{{greet(\"World\")}}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {{x}}!\n@end\n"
    }));
    let result = mds_wasm::check(source, opts).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "check() with modules must return a warnings array"
    );
}

#[wasm_bindgen_test]
fn check_with_runtime_vars() {
    // Verify the vars option flows through the check() path correctly.
    let source = "Hello {{name}}!\n";
    let opts = vars_opts(&serde_json::json!({ "name": "Rust" }));
    let result = mds_wasm::check(source, opts).unwrap();
    let warnings_arr = js_sys::Array::from(&get_prop(&result, "warnings"));
    assert_eq!(
        warnings_arr.length(),
        0,
        "check() with valid vars should produce no warnings"
    );
}

// ── options validation tests ──────────────────────────────────────────────────

#[wasm_bindgen_test]
fn compile_empty_filename_returns_error() {
    let opts = filename_opts("");
    let err = mds_wasm::compile("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
}

#[wasm_bindgen_test]
fn compile_filename_collision_returns_error() {
    // modules already contains "input.mds" — same as default filename
    let opts_val = serde_json::json!({
        "modules": {
            "input.mds": "Some other module\n"
        }
    });
    let opts = to_js_object(&opts_val);
    let err = mds_wasm::compile("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::filename_collision", "got: {code}");
}

#[wasm_bindgen_test]
fn compile_invalid_vars_type_returns_error() {
    // vars must be an object, not a string
    let opts_val = serde_json::json!({ "vars": "not an object" });
    let opts = to_js_object(&opts_val);
    let err = mds_wasm::compile("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
}

#[wasm_bindgen_test]
fn check_null_options() {
    let result = mds_wasm::check("Hello!\n", JsValue::NULL).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "warnings must be an array"
    );
}

#[wasm_bindgen_test]
fn check_undefined_options() {
    let result = mds_wasm::check("Hello!\n", JsValue::UNDEFINED).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "warnings must be an array"
    );
}

#[wasm_bindgen_test]
fn check_empty_filename_returns_error() {
    // Verifies that the shared options-validation path is exercised via check().
    let opts = filename_opts("");
    let err = mds_wasm::check("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options");
}

#[wasm_bindgen_test]
fn compile_unknown_option_key_returns_error() {
    // A typo like `varss` must be caught rather than silently ignored.
    let opts_val = serde_json::json!({ "varss": { "name": "World" } });
    let opts = to_js_object(&opts_val);
    let err = mds_wasm::compile("Hello {{name}}!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
    let message = get_str(&err, "message");
    assert!(
        message.contains("varss"),
        "error message should name the unknown key, got: {message}"
    );
}

#[wasm_bindgen_test]
fn check_unknown_option_key_returns_error() {
    // Verifies the same unknown-key guard is exercised via check().
    let opts_val = serde_json::json!({ "moduless": {} });
    let opts = to_js_object(&opts_val);
    let err = mds_wasm::check("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
}

// ── check() options-key strictness (B5/F9) ───────────────────────────────────
//
// check() only accepts filename/modules/vars. sourceMap and sourcesContent are
// compile-only options; passing them must be rejected with mds::invalid_options.

#[wasm_bindgen_test]
fn check_rejects_source_map_option() {
    // B5: check() must reject sourceMap — it is not a valid check() option.
    let opts = to_js_object(&serde_json::json!({ "sourceMap": true }));
    let err = mds_wasm::check("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
    let msg = get_str(&err, "message");
    assert!(
        msg.contains("sourceMap"),
        "error message must name the unknown key, got: {msg}"
    );
}

#[wasm_bindgen_test]
fn check_rejects_sources_content_option() {
    // B5: check() must reject sourcesContent — it is not a valid check() option.
    let opts = to_js_object(&serde_json::json!({ "sourcesContent": true }));
    let err = mds_wasm::check("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
    let msg = get_str(&err, "message");
    assert!(
        msg.contains("sourcesContent"),
        "error message must name the unknown key, got: {msg}"
    );
}

#[wasm_bindgen_test]
fn check_still_accepts_filename_option() {
    // check() must continue to accept the standard filename option.
    let opts = filename_opts("my-check.mds");
    let result = mds_wasm::check("Hello!\n", opts).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "check() with filename must still succeed"
    );
}

#[wasm_bindgen_test]
fn check_still_accepts_vars_option() {
    // check() must continue to accept the vars option.
    let opts = vars_opts(&serde_json::json!({ "name": "World" }));
    let result = mds_wasm::check("Hello {{name}}!\n", opts).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "check() with vars must still succeed"
    );
}

#[wasm_bindgen_test]
fn check_still_accepts_modules_option() {
    // check() must continue to accept the modules option.
    let source = "@import \"./lib.mds\"\n{{greet(\"World\")}}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {{x}}!\n@end\n"
    }));
    let result = mds_wasm::check(source, opts).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "check() with modules must still succeed"
    );
}

// ── scan_imports tests ────────────────────────────────────────────────────────

/// Helper: get the JS array length.
fn js_array_len(val: &JsValue) -> u32 {
    js_sys::Array::from(val).length()
}

/// Helper: get string element at index from a JS array.
fn js_array_str(val: &JsValue, idx: u32) -> String {
    js_sys::Array::from(val)
        .get(idx)
        .as_string()
        .unwrap_or_default()
}

#[wasm_bindgen_test]
fn scan_imports_returns_array_for_source_with_imports() {
    let source = "@import \"./foo.mds\"\n@import \"./bar.mds\"\n";
    let result = mds_wasm::scan_imports(source).unwrap();
    assert_eq!(js_array_len(&result), 2);
    assert_eq!(js_array_str(&result, 0), "./foo.mds");
    assert_eq!(js_array_str(&result, 1), "./bar.mds");
}

#[wasm_bindgen_test]
fn scan_imports_returns_empty_array_for_importless_source() {
    let result = mds_wasm::scan_imports("Hello World!\n").unwrap();
    assert_eq!(js_array_len(&result), 0);
}

#[wasm_bindgen_test]
fn scan_imports_returns_error_for_malformed_source() {
    // Unclosed double-brace interpolation — should produce a syntax error.
    let err = mds_wasm::scan_imports("Hello {{name\n").unwrap_err();
    let code = get_str(&err, "code");
    assert!(
        !code.is_empty(),
        "error should have a code, got empty string"
    );
}

#[wasm_bindgen_test]
fn scan_imports_returns_error_for_oversized_source() {
    // Build a source that exceeds MAX_SOURCE_SIZE (10 MiB).
    let oversized = "x".repeat(10 * 1024 * 1024 + 1);
    let err = mds_wasm::scan_imports(&oversized).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::resource_limit");
}

#[wasm_bindgen_test]
fn scan_imports_handles_all_directive_forms() {
    let source = concat!(
        "@import \"./a.mds\" as a\n",
        "@import { foo } from \"./b.mds\"\n",
        "@import \"./c.mds\"\n",
        "@export bar from \"./d.mds\"\n",
        "@export * from \"./e.mds\"\n",
        "@export localFn\n",
    );
    let result = mds_wasm::scan_imports(source).unwrap();
    assert_eq!(js_array_len(&result), 5);
    assert_eq!(js_array_str(&result, 0), "./a.mds");
    assert_eq!(js_array_str(&result, 1), "./b.mds");
    assert_eq!(js_array_str(&result, 2), "./c.mds");
    assert_eq!(js_array_str(&result, 3), "./d.mds");
    assert_eq!(js_array_str(&result, 4), "./e.mds");
}

// ── Template inheritance tests (@extends / @block) ───────────────────────────

/// Build a modules option for inheritance tests.
///
/// `source` (child_src) is passed as the first argument to `compile` and is
/// registered under `"filename"` by the binding. `modules` must therefore
/// contain ONLY the *other* files (the base template), not the entry file —
/// otherwise the binding rejects it with `mds::filename_collision`.
fn inheritance_modules_opts(child_src: &str, base_src: &str) -> JsValue {
    let _ = child_src; // entry content is passed as `source`, not in modules
    to_js_object(&serde_json::json!({
        "modules": {
            "base.mds": base_src,
        },
        "filename": "child.mds"
    }))
}

#[wasm_bindgen_test]
fn compile_extends_text_mode_skeleton_and_override() {
    // F1 (WASM): child overrides instructions + tools, inherits output_format default.
    // Output must contain the overridden blocks and the base skeleton text.
    let base_src = concat!(
        "---\nrole: general\n---\n",
        "You are a {{role}} assistant.\n",
        "@block instructions:\nAnalyze data carefully.\n@end\n",
        "@block tools:\n@end\n",
        "@block output_format:\nRespond in plain text.\n@end\n",
    );
    let child_src = concat!(
        "---\nrole: data analysis\n---\n",
        "@extends \"./base.mds\"\n",
        "@block instructions:\nPerform statistical analysis.\n@end\n",
        "@block tools:\nYou have access to: Python, R\n@end\n",
    );
    let opts = inheritance_modules_opts(child_src, base_src);
    let result = mds_wasm::compile(child_src, opts).unwrap();

    let output = get_str(&result, "output");
    assert!(
        output.contains("You are a data analysis assistant."),
        "WASM F1: base skeleton with child role should render; got: {output}"
    );
    assert!(
        output.contains("Perform statistical analysis."),
        "WASM F1: overridden instructions block should render; got: {output}"
    );
    assert!(
        output.contains("You have access to: Python, R"),
        "WASM F1: overridden tools block should render; got: {output}"
    );
    assert!(
        output.contains("Respond in plain text."),
        "WASM F1: base default output_format block should render; got: {output}"
    );
}

#[wasm_bindgen_test]
fn compile_extends_dependencies_contains_base() {
    // A4 (WASM): compile() returns the canonical { kind, output|messages, warnings, dependencies }.
    // Markdown results must have { kind:"markdown", output, warnings, dependencies }.
    // The base must appear in the dependencies list.
    let base_src = "@block body:\nBase default.\n@end\n";
    let child_src = "@extends \"./base.mds\"\n";
    let opts = inheritance_modules_opts(child_src, base_src);
    let result = mds_wasm::compile(child_src, opts).unwrap();

    // A4: shape check — kind is "markdown", output is a string.
    let kind = get_str(&result, "kind");
    assert_eq!(
        kind, "markdown",
        "WASM A4: kind must be 'markdown' for text output"
    );
    let output = get_prop(&result, "output");
    assert!(
        output.as_string().is_some(),
        "WASM A4: output must be a string"
    );
    // messages field must be absent for markdown results.
    let messages_field = get_prop(&result, "messages");
    assert!(
        messages_field.is_undefined(),
        "WASM A4: messages field must be absent for markdown result"
    );
    let warnings = get_prop(&result, "warnings");
    assert!(
        js_sys::Array::is_array(&warnings),
        "WASM A4: warnings must be an array"
    );
    let deps_val = get_prop(&result, "dependencies");
    assert!(
        js_sys::Array::is_array(&deps_val),
        "WASM A4: dependencies must be an array"
    );

    // Base must be in dependencies.
    let deps = js_sys::Array::from(&deps_val);
    let dep_strings: Vec<String> = (0..deps.length())
        .map(|i| deps.get(i).as_string().unwrap_or_default())
        .collect();
    assert!(
        dep_strings.iter().any(|s| s.contains("base.mds")),
        "WASM A4: dependencies must contain base.mds; got: {dep_strings:?}"
    );
}

#[wasm_bindgen_test]
fn compile_extends_messages_mode() {
    // F9 (WASM): child overrides a block containing a @message; compile() returns
    // the canonical { kind: "messages", messages, warnings, dependencies } shape.
    let base_src = concat!(
        "---\nrole: assistant\n---\n",
        "@block system_msg:\n@message system:\nYou are a {{role}}.\n@end\n@end\n",
        "@block user_msg:\n@message user:\nHello!\n@end\n@end\n",
    );
    let child_src = concat!(
        "---\nrole: researcher\n---\n",
        "@extends \"./base.mds\"\n",
        "@block user_msg:\n@message user:\nSummarize findings.\n@end\n@end\n",
    );
    let opts = inheritance_modules_opts(child_src, base_src);

    // compile() dispatches intrinsically: @message content → kind:"messages".
    let result = mds_wasm::compile(child_src, opts).unwrap();

    let kind = get_str(&result, "kind");
    assert_eq!(kind, "messages", "WASM F9: kind must be 'messages'");

    // output field must be absent for messages results.
    let output_field = get_prop(&result, "output");
    assert!(
        output_field.is_undefined(),
        "WASM F9: output field must be absent for messages result"
    );

    let messages_val = get_prop(&result, "messages");
    assert!(
        js_sys::Array::is_array(&messages_val),
        "WASM F9: messages must be an array"
    );
    let messages = js_sys::Array::from(&messages_val);
    assert_eq!(
        messages.length(),
        2,
        "WASM F9: expected 2 messages (system + user); got: {}",
        messages.length()
    );
    let system_msg = messages.get(0);
    let role = get_str(&system_msg, "role");
    assert_eq!(
        role, "system",
        "WASM F9: first message role should be system"
    );
    let content = get_str(&system_msg, "content");
    assert!(
        content.contains("researcher"),
        "WASM F9: system message should use child's role; got: {content}"
    );
    let user_msg = messages.get(1);
    let user_content = get_str(&user_msg, "content");
    assert!(
        user_content.contains("Summarize findings."),
        "WASM F9: user message should use overridden block; got: {user_content}"
    );
}

#[wasm_bindgen_test]
fn compile_extends_error_code_is_mds_extends() {
    // E1 (WASM): stray @extends (not first directive) → error.code must be mds::extends.
    let source = "Some text.\n@extends \"./base.mds\"\n";
    let err = mds_wasm::compile(source, JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(
        code, "mds::extends",
        "WASM E1: stray @extends must have code mds::extends; got: {code}"
    );
}

#[wasm_bindgen_test]
fn compile_extends_undefined_var_in_base_default_carries_real_span() {
    // C4 (WASM): an undefined variable referenced in a base template's default
    // block must produce code=mds::undefined_var with a real span (line/column
    // are numbers, not undefined) when the child does not override that block.
    // Regression: before the source-attribution fix (7d4310f), line/column were
    // absent (miette reported OutOfBounds for the cross-source offset).
    let base_src = "@block greeting:\nHello {{customer_name}}, welcome.\n@end\n";
    let child_src = "@extends \"./base.mds\"\n";
    let opts = inheritance_modules_opts(child_src, base_src);

    let err = mds_wasm::compile(child_src, opts).unwrap_err();

    let code = get_str(&err, "code");
    assert_eq!(
        code, "mds::undefined_var",
        "WASM C4: expected mds::undefined_var for undefined var in base default block; got: {code}"
    );

    let span = get_prop(&err, "span");
    assert!(
        !span.is_undefined() && !span.is_null(),
        "WASM C4: err.span must be present for inherited undefined_var"
    );
    let _line = get_prop(&span, "line")
        .as_f64()
        .expect("WASM C4: span.line must be a number, not undefined");
    let _column = get_prop(&span, "column")
        .as_f64()
        .expect("WASM C4: span.column must be a number, not undefined");
}

// ── T-15: ESC-injection hardening — WASM surface (issue #176 / CWE-150) ──────
//
// Two sub-tests:
//  F5: error path — @include alias with U+001B mid-token; err.message must
//      carry the sanitized \uXXXX literal and contain no raw control bytes.
//  F6: lint path — frontmatter key with U+001B; first diagnostic message clean.

/// Assert that a string contains no raw C0 (excl. \t \n), DEL, C1, bidi control,
/// line/paragraph separator, or BOM codepoint.
fn assert_no_control_chars(s: &str, label: &str) {
    for (i, ch) in s.char_indices() {
        let code = ch as u32;
        let is_c0 = code < 0x20 && code != 0x09 && code != 0x0A;
        let is_del = code == 0x7F;
        let is_c1 = (0x80..=0x9F).contains(&code);
        // Bidi controls (Trojan Source, CVE-2021-42574), JS line/paragraph
        // separators, and the invisible BOM.
        let is_format_hazard = matches!(ch,
            '\u{200E}' | '\u{200F}'
            | '\u{2028}' | '\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
        );
        assert!(
            !is_c0 && !is_del && !is_c1 && !is_format_hazard,
            "raw hostile char U+{code:04X} at byte {i} must not appear in {label}; got: {s:?}"
        );
    }
}

#[wasm_bindgen_test]
fn wasm_control_chars_in_error_message_are_escaped() {
    // T-15 / F5 [AC-F3]: error-path sanitization for WASM surface.
    // @include with a raw ESC byte (U+001B) mid-alias is rejected by the parser.
    // After Change #1, serialize() sanitizes so err.message contains no raw
    // control bytes and the sanitized \u001B literal is visible.
    let esc = '\u{001B}';
    let source = format!("@include fo{esc}o\n");
    let err = mds_wasm::compile(&source, JsValue::NULL).unwrap_err();
    let msg = get_str(&err, "message");
    assert!(
        !msg.is_empty(),
        "T-15/F5: err.message must not be empty for an ESC-in-alias error"
    );
    assert_no_control_chars(&msg, "err.message (T-15/F5)");
    assert!(
        msg.contains("\\u001B"),
        "T-15/F5: sanitized \\u001B literal must appear in err.message; got: {msg:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_virtual_esc_in_module_name_sanitizes_duplicate_import_message() {
    // T-15 / F6 [AC-F4]: lint-path sanitization via lintVirtual — WASM surface.
    // Use a module whose NAME contains a raw ESC byte (U+001B), imported twice so
    // duplicate-import fires and embeds the raw path in its message.
    // After sanitization: message must contain no raw control bytes and must carry
    // the sanitized \u001B literal (positive evidence). Mirrors Python E12 pattern.
    // Verifies:
    //   (1) No raw C0/DEL/C1 bytes in any diagnostic message.
    //   (2) Sanitized \u001B literal IS present (positive evidence, non-vacuous).
    //   (3) Result shape: version 1, duplicate-import rule present.
    let esc = '\u{001B}';
    let module_name = format!("fo{esc}o.mds");
    let main_src = format!("@import \"./{module_name}\"\n@import \"./{module_name}\"\n");

    // Build the modules JS object with js_sys::Reflect so the key preserves the raw
    // ESC byte as a JS string character (U+001B in UTF-16).
    let modules_obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str(&module_name),
        &JsValue::from_str("hi\n"),
    )
    .unwrap();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str("main.mds"),
        &JsValue::from_str(&main_src),
    )
    .unwrap();

    let result = mds_wasm::lint_virtual(modules_obj.into(), "main.mds", JsValue::NULL)
        .expect("T-15/F6: lintVirtual must succeed with ESC in module name");

    // (3) Result shape: version 1.
    let version = get_prop(&result, "version")
        .as_f64()
        .expect("T-15/F6: result.version must be a number") as u32;
    assert_eq!(version, 1, "T-15/F6: result.version must be 1");

    let files = get_prop(&result, "files");
    let files_arr = js_sys::Array::from(&files);
    assert!(
        files_arr.length() > 0,
        "T-15/F6: expected at least one file entry with diagnostics"
    );

    let mut all_messages: Vec<String> = Vec::new();
    for i in 0..files_arr.length() {
        let file_entry = files_arr.get(i);
        let diags = get_prop(&file_entry, "diagnostics");
        let diags_arr = js_sys::Array::from(&diags);
        for j in 0..diags_arr.length() {
            let diag = diags_arr.get(j);
            let msg = get_str(&diag, "message");
            // (1) No raw control bytes in any diagnostic message.
            assert_no_control_chars(
                &msg,
                &format!("T-15/F6: files[{i}].diagnostics[{j}].message"),
            );
            all_messages.push(msg);
        }
    }

    assert!(
        !all_messages.is_empty(),
        "T-15/F6: expected at least one diagnostic (duplicate-import should fire)"
    );

    // (2) At least one message contains the sanitized \u001B literal (positive evidence).
    let has_sanitized = all_messages.iter().any(|m| m.contains("\\u001B"));
    assert!(
        has_sanitized,
        "T-15/F6: expected sanitized \\u001B in at least one message; got: {all_messages:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_del_in_error_message_is_escaped() {
    // T-15/F5-DEL: DEL (U+007F) in @include alias — same error-path pattern as F5
    // with a different control character. serde_json does not escape DEL by default,
    // making this a load-bearing second vector. Verifies DEL is sanitized to \u007F.
    let del = '\u{007F}';
    let source = format!("@include fo{del}o\n");
    let err = mds_wasm::compile(&source, JsValue::NULL).unwrap_err();
    let msg = get_str(&err, "message");
    assert!(
        !msg.is_empty(),
        "T-15/F5-DEL: err.message must not be empty for a DEL-in-alias error"
    );
    assert_no_control_chars(&msg, "err.message (T-15/F5-DEL)");
    assert!(
        msg.contains("\\u007F"),
        "T-15/F5-DEL: sanitized \\u007F literal must appear in err.message; got: {msg:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_virtual_nel_in_module_name_sanitizes_message() {
    // T-15/F6-C1: U+0085 (NEL/C1) in lintVirtual module name — same lint-path pattern
    // as F6 with a C1 control character. NEL passes serde_yaml_ng (unlike ESC/DEL),
    // making it a reachable C1 vector. Verifies the sanitized U+0085 literal appears.
    let nel = '\u{0085}';
    let module_name = format!("fo{nel}o.mds");
    let main_src = format!("@import \"./{module_name}\"\n@import \"./{module_name}\"\n");

    let modules_obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str(&module_name),
        &JsValue::from_str("hi\n"),
    )
    .unwrap();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str("main.mds"),
        &JsValue::from_str(&main_src),
    )
    .unwrap();

    let result = mds_wasm::lint_virtual(modules_obj.into(), "main.mds", JsValue::NULL)
        .expect("T-15/F6-C1: lintVirtual must succeed with NEL in module name");

    let version = get_prop(&result, "version")
        .as_f64()
        .expect("T-15/F6-C1: result.version must be a number") as u32;
    assert_eq!(version, 1, "T-15/F6-C1: result.version must be 1");

    let files = get_prop(&result, "files");
    let files_arr = js_sys::Array::from(&files);
    assert!(
        files_arr.length() > 0,
        "T-15/F6-C1: expected at least one file entry with diagnostics"
    );

    let mut all_messages: Vec<String> = Vec::new();
    for i in 0..files_arr.length() {
        let file_entry = files_arr.get(i);
        let diags = get_prop(&file_entry, "diagnostics");
        let diags_arr = js_sys::Array::from(&diags);
        for j in 0..diags_arr.length() {
            let diag = diags_arr.get(j);
            let msg = get_str(&diag, "message");
            assert_no_control_chars(
                &msg,
                &format!("T-15/F6-C1: files[{i}].diagnostics[{j}].message"),
            );
            all_messages.push(msg);
        }
    }

    assert!(
        !all_messages.is_empty(),
        "T-15/F6-C1: expected at least one diagnostic"
    );

    let has_sanitized_nel = all_messages.iter().any(|m| m.contains("\\u0085"));
    assert!(
        has_sanitized_nel,
        "T-15/F6-C1: expected sanitized \\u0085 in at least one message; got: {all_messages:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_virtual_bidi_override_in_module_name_is_escaped() {
    // T-15/F6-BIDI: U+202E RIGHT-TO-LEFT OVERRIDE in a lintVirtual module name.
    // U+202E is outside C0/DEL/C1, so it used to reach the wire untouched and
    // reverse the display order of the rest of the line in any bidi-aware renderer
    // (Trojan Source, CVE-2021-42574). "fo<RLO>gnp.mds" renders as "fopng.mds".
    // Verifies:
    //   (1) No raw hostile codepoint in any diagnostic message or file key.
    //   (2) The escaped \\u202E literal IS present (positive evidence, non-vacuous).
    //   (3) Result shape: version 1, duplicate-import rule present.
    let rlo = '\u{202E}';
    let module_name = format!("fo{rlo}gnp.mds");
    let main_src = format!("@import \"./{module_name}\"\n@import \"./{module_name}\"\n");

    let modules_obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str(&module_name),
        &JsValue::from_str("hi\n"),
    )
    .unwrap();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str("main.mds"),
        &JsValue::from_str(&main_src),
    )
    .unwrap();

    let result = mds_wasm::lint_virtual(modules_obj.into(), "main.mds", JsValue::NULL)
        .expect("T-15/F6-BIDI: lintVirtual must succeed with RLO in module name");

    let version = get_prop(&result, "version")
        .as_f64()
        .expect("T-15/F6-BIDI: result.version must be a number") as u32;
    assert_eq!(version, 1, "T-15/F6-BIDI: result.version must be 1");

    let files = get_prop(&result, "files");
    let files_arr = js_sys::Array::from(&files);
    assert!(
        files_arr.length() > 0,
        "T-15/F6-BIDI: expected at least one file entry with diagnostics"
    );

    let mut all_messages: Vec<String> = Vec::new();
    let mut all_rules: Vec<String> = Vec::new();
    for i in 0..files_arr.length() {
        let file_entry = files_arr.get(i);
        // Cheap invariant check only — NOT coverage of the `file`-key escape.
        // The hostile RLO is in the *imported* module's name, but this key is the
        // *entry* filename ("main.mds"), so no hostile byte ever reaches it and this
        // assertion cannot fail via this vector (PF-013: it would pass even if the
        // `file`-key sanitizer were deleted). Real coverage of the `file` key lives in
        // mds-core `to_canonical_json_escapes_bidi_override`, which constructs a
        // diagnostic with `file: Some("ma\u{202E}in.mds")` directly.
        assert_no_control_chars(
            &get_str(&file_entry, "file"),
            &format!("T-15/F6-BIDI: files[{i}].file"),
        );
        let diags = get_prop(&file_entry, "diagnostics");
        let diags_arr = js_sys::Array::from(&diags);
        for j in 0..diags_arr.length() {
            let diag = diags_arr.get(j);
            let msg = get_str(&diag, "message");
            assert_no_control_chars(
                &msg,
                &format!("T-15/F6-BIDI: files[{i}].diagnostics[{j}].message"),
            );
            all_messages.push(msg);
            all_rules.push(get_str(&diag, "rule"));
        }
    }

    assert!(
        !all_messages.is_empty(),
        "T-15/F6-BIDI: expected at least one diagnostic"
    );
    assert!(
        all_rules.iter().any(|r| r == "duplicate-import"),
        "T-15/F6-BIDI: expected duplicate-import; got rules: {all_rules:?}"
    );

    let has_escaped_rlo = all_messages.iter().any(|m| m.contains("\\u202E"));
    assert!(
        has_escaped_rlo,
        "T-15/F6-BIDI: expected escaped \\u202E in at least one message; got: {all_messages:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_virtual_newline_in_frontmatter_key_is_escaped_on_the_wire() {
    // T-15/F6-NL [PF-007]: cross-surface parity for the WIRE-mode `\n` escape.
    //
    // `lint_virtual` returns `LintResult::to_canonical_json()`, which sanitizes in
    // WIRE mode — so a raw newline inside a diagnostic message becomes the literal
    // six-character escape (backslash-u-0-0-0-A). Without this test the WASM surface
    // is the only one of the five with no assertion pinning that behaviour, which is
    // exactly the per-surface blind spot PF-007 describes: each surface's own golden
    // passes while the surfaces silently diverge from one another.
    //
    // Log/YAML-key forging: a raw newline in a message lets an attacker forge what
    // reads as a second, independent finding in any line-oriented consumer.
    //
    // Reachability: a newline inside an `@import "..."` path is rejected by the
    // lexer, so that route is vacuous. A YAML *double-quoted* frontmatter key is
    // not — serde_yaml_ng decodes the `\n` escape into a real newline, and
    // unused-variable embeds the decoded key verbatim in its message.
    //
    // Mirrors napi E-15 and universal-JS U-E14 exactly (same vector, same
    // assertions) so the three surfaces are differentially comparable.
    let source = "---\n\"a\\nerror[mds::forged]: FAKE\\nb\": 1\n---\nHello\n";

    let modules_obj = js_sys::Object::new();
    js_sys::Reflect::set(
        &modules_obj,
        &JsValue::from_str("main.mds"),
        &JsValue::from_str(source),
    )
    .unwrap();

    let result = mds_wasm::lint_virtual(modules_obj.into(), "main.mds", JsValue::NULL)
        .expect("T-15/F6-NL: lintVirtual must succeed with a newline in a frontmatter key");

    let version = get_prop(&result, "version")
        .as_f64()
        .expect("T-15/F6-NL: result.version must be a number") as u32;
    assert_eq!(version, 1, "T-15/F6-NL: result.version must be 1");

    let files_arr = js_sys::Array::from(&get_prop(&result, "files"));
    let mut all_messages: Vec<String> = Vec::new();
    let mut all_rules: Vec<String> = Vec::new();
    for i in 0..files_arr.length() {
        let file_entry = files_arr.get(i);
        let diags_arr = js_sys::Array::from(&get_prop(&file_entry, "diagnostics"));
        for j in 0..diags_arr.length() {
            let diag = diags_arr.get(j);
            all_messages.push(get_str(&diag, "message"));
            all_rules.push(get_str(&diag, "rule"));
        }
    }

    // Non-vacuity guard: the vector must actually have reached the guarded path.
    assert!(
        all_rules.iter().any(|r| r == "unused-variable"),
        "T-15/F6-NL: expected unused-variable; got rules: {all_rules:?}"
    );

    // Negative: no raw newline may survive into a wire message.
    for msg in &all_messages {
        assert!(
            !msg.contains('\n'),
            "T-15/F6-NL: raw newline must not survive into the wire message; got: {msg:?}"
        );
    }

    // Positive (PF-013): the escaped form must be present.
    assert!(
        all_messages.iter().any(|m| m.contains("\\u000A")),
        "T-15/F6-NL: expected escaped \\u000A in at least one message; got: {all_messages:?}"
    );

    // Escaped, not stripped — the payload text itself is preserved verbatim.
    assert!(
        all_messages
            .iter()
            .any(|m| m.contains("error[mds::forged]")),
        "T-15/F6-NL: message body must be preserved verbatim; got: {all_messages:?}"
    );
}

// ── AC-224 D8: unknown rule name warning channel on WASM ─────────────────────
//
// An unknown rule name in the `rules` option produces a `lint_warnings` array
// in the result rather than hard-failing (D8 / AC-224-1). Known rule names
// produce no `lint_warnings` field (common-case cleanliness).
//
// The deliberate asymmetry: unknown severity VALUES remain a hard error
// (`mds::invalid_options`). Severities are a closed enum; rule names grow every
// release. Both arms are pinned here so the asymmetry is asserted on the surface
// this PR touches (PF-007 — per-surface goldens prove nothing cross-surface).

#[wasm_bindgen_test]
fn wasm_lint_unknown_rule_name_returns_lint_warnings() {
    // W-WARN-1 (AC-224-1/D8): lint() with an unknown rule name returns lint_warnings.
    let opts = to_js_object(&serde_json::json!({
        "rules": { "no-such-rule-xyzzy": "warn" }
    }));
    let result = mds_wasm::lint("Hello!\n", opts).expect("W-WARN-1: lint must succeed");

    let version = get_prop(&result, "version")
        .as_f64()
        .expect("W-WARN-1: version must be a number") as u32;
    assert_eq!(version, 1, "W-WARN-1: version must be 1");

    let lint_warnings = get_prop(&result, "lint_warnings");
    assert!(
        !lint_warnings.is_undefined() && !lint_warnings.is_null(),
        "W-WARN-1: lint_warnings must be present for an unknown rule name"
    );
    let warnings_arr = js_sys::Array::from(&lint_warnings);
    assert!(
        warnings_arr.length() > 0,
        "W-WARN-1: lint_warnings must be non-empty for an unknown rule name"
    );
    let w0 = warnings_arr
        .get(0)
        .as_string()
        .expect("W-WARN-1: lint_warnings[0] must be a string");
    assert!(
        w0.contains("no-such-rule-xyzzy"),
        "W-WARN-1: lint_warnings[0] must name the unknown rule; got: {w0}"
    );
    assert!(
        w0.contains("recognised rules are") || w0.contains("recognized rules are"),
        "W-WARN-1: lint_warnings[0] must list recognised rules; got: {w0}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_known_rule_names_no_lint_warnings() {
    // W-WARN-2 (AC-224-1/D8): lint() with only known rule names has no lint_warnings.
    let opts = to_js_object(&serde_json::json!({
        "rules": { "unused-variable": "off" }
    }));
    let result = mds_wasm::lint("Hello!\n", opts).expect("W-WARN-2: lint must succeed");
    let lint_warnings = get_prop(&result, "lint_warnings");
    assert!(
        lint_warnings.is_undefined(),
        "W-WARN-2: lint_warnings must be absent for known rule names; got: {lint_warnings:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_virtual_unknown_rule_name_returns_lint_warnings() {
    // W-WARN-3 (AC-224-1/D8): lintVirtual() with an unknown rule name returns lint_warnings.
    let modules_val = to_js_object(&serde_json::json!({ "main.mds": "Hello!\n" }));
    let opts = to_js_object(&serde_json::json!({
        "rules": { "no-such-rule-xyzzy": "error" }
    }));
    let result = mds_wasm::lint_virtual(modules_val, "main.mds", opts)
        .expect("W-WARN-3: lintVirtual must succeed");

    let lint_warnings = get_prop(&result, "lint_warnings");
    assert!(
        !lint_warnings.is_undefined() && !lint_warnings.is_null(),
        "W-WARN-3: lint_warnings must be present for an unknown rule name"
    );
    let warnings_arr = js_sys::Array::from(&lint_warnings);
    assert!(
        warnings_arr.length() > 0,
        "W-WARN-3: lint_warnings must be non-empty for an unknown rule name"
    );
    let w0 = warnings_arr
        .get(0)
        .as_string()
        .expect("W-WARN-3: lint_warnings[0] must be a string");
    assert!(
        w0.contains("no-such-rule-xyzzy"),
        "W-WARN-3: lint_warnings[0] must name the unknown rule; got: {w0}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_continues_with_unknown_rule_name() {
    // W-WARN-4 (AC-224-1/D8): lint continues — files[] and truncated are present.
    let opts = to_js_object(&serde_json::json!({
        "rules": { "no-such-rule-xyzzy": "warn" }
    }));
    let result = mds_wasm::lint("Hello!\n", opts).expect("W-WARN-4: lint must succeed");

    // files[] must be present and be a genuine JS array. `js_sys::Array::from` coerces
    // almost anything, so asserting on its length would pass vacuously — check
    // `Array::is_array` on the raw value instead.
    let files = get_prop(&result, "files");
    assert!(
        js_sys::Array::is_array(&files),
        "W-WARN-4: files must be a JS array even when the rule name is unknown"
    );
    assert_eq!(
        js_sys::Array::from(&files).length(),
        0,
        "W-WARN-4: a clean source yields no file entries; the unknown rule name must not \
         add one"
    );

    // truncated must be present and false.
    let truncated = get_prop(&result, "truncated").as_bool().unwrap_or(true);
    assert!(
        !truncated,
        "W-WARN-4: truncated must be false for a clean source"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_unknown_rule_no_lint_warnings_absent_on_clean() {
    // W-WARN-2b (AC-224-1/D8): no options → no lint_warnings field at all.
    let result =
        mds_wasm::lint("Hello!\n", JsValue::NULL).expect("W-WARN-2b: lint(NULL) must succeed");
    let lint_warnings = get_prop(&result, "lint_warnings");
    assert!(
        lint_warnings.is_undefined(),
        "W-WARN-2b: lint_warnings must be absent when no rules are passed; got: {lint_warnings:?}"
    );
}

/// W-WARN-ESC (ADR-008 per-surface): a hostile rule name containing U+001B must not
/// deliver a raw control byte through `lint_warnings` to the JS consumer.
///
/// The mds-core unit test `warning_wire_escapes_hostile_rule_name` proves the
/// formatter escapes before building the string. This test proves the string that
/// actually crosses the WASM FFI boundary carries no raw control byte (negative) and
/// DOES carry the six-character ASCII sequence backslash-u-0-0-1-B (positive control,
/// PF-013/ADR-009).
///
/// PF-018: hostile bytes are built from Rust `\u{..}` escapes — never authored as
/// literal bytes in this source file.
#[wasm_bindgen_test]
fn wasm_lint_hostile_rule_name_escapes_control_bytes_in_lint_warnings() {
    // Construct the hostile rule name at runtime using Rust char escapes (PF-018).
    let hostile_rule = "\u{1b}[31mhostile-rule\u{1b}[0m".to_string();
    let opts = to_js_object(&serde_json::json!({
        "rules": { hostile_rule: "warn" }
    }));
    let result = mds_wasm::lint("Hello!\n", opts)
        .expect("W-WARN-ESC: lint must succeed with hostile rule name");

    // The warning must be present (D8 / AC-224-1) — call succeeds, lint_warnings is non-empty.
    let lint_warnings = get_prop(&result, "lint_warnings");
    assert!(
        !lint_warnings.is_undefined() && !lint_warnings.is_null(),
        "W-WARN-ESC: lint_warnings must be present for a hostile unknown rule name"
    );
    let warnings_arr = js_sys::Array::from(&lint_warnings);
    assert!(
        warnings_arr.length() > 0,
        "W-WARN-ESC: lint_warnings must be non-empty"
    );
    let w0 = warnings_arr
        .get(0)
        .as_string()
        .expect("W-WARN-ESC: lint_warnings[0] must be a string");

    // Negative: no raw control byte (C0 excl. \t \n, DEL, C1) may survive (ADR-008).
    for (i, ch) in w0.char_indices() {
        let code = ch as u32;
        let is_c0 = code < 0x20 && code != 0x09 && code != 0x0a;
        let is_del = code == 0x7f;
        let is_c1 = (0x80..=0x9f).contains(&code);
        assert!(
            !is_c0 && !is_del && !is_c1,
            "W-WARN-ESC: raw hostile char U+{code:04X} at byte {i} must not appear \
             in lint_warnings[0]; got: {w0:?}"
        );
    }

    // Positive control (PF-013/ADR-009): the sanitized literal must be present so
    // the negative above cannot pass merely because the name never reached the message.
    assert!(
        w0.contains("\\u001B"),
        "W-WARN-ESC: sanitized \\u001B literal must appear in lint_warnings[0]; got: {w0:?}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_unknown_severity_value_throws_invalid_options() {
    // W-SEVER-1 (AC-224-1 — paired throw arm, lint path): unknown severity
    // VALUES remain a hard error on the WASM surface. napi pins this in L-N-6;
    // Python in test_l5_lint_rules_unknown_severity_raises. Per PF-007 those
    // prove nothing about WASM — this test pins the throw arm here.
    //
    // Severities are a closed enum; "verbose" is not a valid severity string.
    let opts = to_js_object(&serde_json::json!({
        "rules": { "unused-variable": "verbose" }
    }));
    let err = mds_wasm::lint("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(
        code, "mds::invalid_options",
        "W-SEVER-1: unknown severity value must throw mds::invalid_options; got: {code}"
    );
}

#[wasm_bindgen_test]
fn wasm_lint_virtual_unknown_severity_value_throws_invalid_options() {
    // W-SEVER-2 (AC-224-1 — paired throw arm, lint_virtual path): mirrors
    // W-SEVER-1 for lintVirtual(). Both entry points route through
    // extract_rules(), so both paths are pinned (PF-007).
    let modules_val = to_js_object(&serde_json::json!({ "main.mds": "Hello!\n" }));
    let opts = to_js_object(&serde_json::json!({
        "rules": { "unused-variable": "verbose" }
    }));
    let err = mds_wasm::lint_virtual(modules_val, "main.mds", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(
        code, "mds::invalid_options",
        "W-SEVER-2: lint_virtual unknown severity value must throw mds::invalid_options; got: {code}"
    );
}
