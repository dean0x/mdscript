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
    assert!(js_sys::Array::is_array(&warnings), "warnings must be an array");
}

#[wasm_bindgen_test]
fn compile_has_dependencies_field() {
    let result = mds_wasm::compile("Hello!\n", JsValue::NULL).unwrap();
    let deps = get_prop(&result, "dependencies");
    assert!(js_sys::Array::is_array(&deps), "dependencies must be an array");
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

#[wasm_bindgen_test]
fn compile_undefined_variable_returns_error() {
    let err = mds_wasm::compile("Hello {undefined_var}!\n", JsValue::NULL).unwrap_err();
    let msg = get_str(&err, "message");
    assert!(!msg.is_empty(), "error message should not be empty");
}

#[wasm_bindgen_test]
fn compile_error_has_code_property() {
    let err = mds_wasm::compile("Hello {undefined_var}!\n", JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert!(!code.is_empty(), "error.code must be set");
    assert!(code.starts_with("mds::"), "code must start with 'mds::': {code}");
}

#[wasm_bindgen_test]
fn compile_error_is_js_error() {
    // Verify the thrown value is an instanceof Error by checking it has a message property
    let err = mds_wasm::compile("{undefined}\n", JsValue::NULL).unwrap_err();
    let msg = get_prop(&err, "message");
    assert!(
        msg.as_string().is_some(),
        "error.message must be a string, got: {msg:?}"
    );
}

// ── check tests ───────────────────────────────────────────────────────────────

#[wasm_bindgen_test]
fn check_valid_template() {
    let result = mds_wasm::check("Hello!\n", JsValue::NULL).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(js_sys::Array::is_array(&warnings), "warnings must be an array");
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
    let err = mds_wasm::check("Hello {undefined_var}!\n", JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert!(!code.is_empty(), "error.code must be set");
}

#[wasm_bindgen_test]
fn check_error_has_code_property() {
    let err = mds_wasm::check("{undefined}\n", JsValue::NULL).unwrap_err();
    let code = get_str(&err, "code");
    assert!(code.starts_with("mds::"), "code must start with 'mds::': {code}");
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
    assert!(js_sys::Array::is_array(&warnings));
}

#[wasm_bindgen_test]
fn check_undefined_options() {
    let result = mds_wasm::check("Hello!\n", JsValue::UNDEFINED).unwrap();
    let warnings = get_prop(&result, "warnings");
    assert!(js_sys::Array::is_array(&warnings));
}
