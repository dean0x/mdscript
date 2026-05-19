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

/// Build a simple JS options object from a vars record.
fn vars_opts(vars: &serde_json::Value) -> JsValue {
    let opts = serde_json::json!({ "vars": vars });
    serde_wasm_bindgen::to_value(&opts).unwrap()
}

/// Build an options object with extra modules.
fn modules_opts(modules: &serde_json::Value) -> JsValue {
    let opts = serde_json::json!({ "modules": modules });
    serde_wasm_bindgen::to_value(&opts).unwrap()
}

/// Build an options object with filename.
fn filename_opts(filename: &str) -> JsValue {
    let opts = serde_json::json!({ "filename": filename });
    serde_wasm_bindgen::to_value(&opts).unwrap()
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
    let source = "---\nname: World\n---\nHello {name}!\n";
    let result = mds_wasm::compile(source, JsValue::NULL).unwrap();
    let output = get_str(&result, "output");
    assert!(output.contains("Hello World!"), "got: {output}");
}

#[wasm_bindgen_test]
fn compile_with_runtime_vars() {
    let source = "Hello {name}!\n";
    let opts = vars_opts(&serde_json::json!({ "name": "Rust" }));
    let result = mds_wasm::compile(source, opts).unwrap();
    let output = get_str(&result, "output");
    assert_eq!(output, "Hello Rust!\n", "got: {output}");
}

#[wasm_bindgen_test]
fn compile_with_modules_import() {
    // VirtualFs normalizes "./lib.mds" from "input.mds" to "lib.mds",
    // so the module key must be "lib.mds".
    let source = "@import \"./lib.mds\"\n{greet(\"World\")}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {x}!\n@end\n"
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
    let source = "@import \"./lib.mds\"\n{greet(\"World\")}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {x}!\n@end\n"
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
    let source = "---\nname: Old\n---\nHello {name}!\n";
    let opts = vars_opts(&serde_json::json!({ "name": "New" }));
    let result = mds_wasm::compile(source, opts).unwrap();
    let output = get_str(&result, "output");
    assert!(output.contains("Hello New!"), "got: {output}");
}

// ── compile error tests ───────────────────────────────────────────────────────

/// Source string shared by all error-path tests.
///
/// The variable reference `{undefined_var}` starts at byte offset 6 (after
/// `"Hello "`). The compiler reports a span of offset=6, length=13, covering
/// the opening brace and identifier name (`{undefined_var` without the closing
/// `}`). Tests that assert exact span values rely on these positions.
const UNDEFINED_VAR_SOURCE: &str = "Hello {undefined_var}!\n";

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
        "span.offset must be 6 (start of '{{undefined_var}}' in source)"
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
    let source = "---\nname: World\n---\nHello {name}!\n";
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
    let source = "@import \"./lib.mds\"\n{greet(\"World\")}\n";
    let opts = modules_opts(&serde_json::json!({
        "lib.mds": "@define greet(x):\nHello {x}!\n@end\n"
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
    let source = "Hello {name}!\n";
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
    let opts = serde_wasm_bindgen::to_value(&opts_val).unwrap();
    let err = mds_wasm::compile("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::filename_collision", "got: {code}");
}

#[wasm_bindgen_test]
fn compile_invalid_vars_type_returns_error() {
    // vars must be an object, not a string
    let opts_val = serde_json::json!({ "vars": "not an object" });
    let opts = serde_wasm_bindgen::to_value(&opts_val).unwrap();
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
    let opts = serde_wasm_bindgen::to_value(&opts_val).unwrap();
    let err = mds_wasm::compile("Hello {name}!\n", opts).unwrap_err();
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
    let opts = serde_wasm_bindgen::to_value(&opts_val).unwrap();
    let err = mds_wasm::check("Hello!\n", opts).unwrap_err();
    let code = get_str(&err, "code");
    assert_eq!(code, "mds::invalid_options", "got: {code}");
}
