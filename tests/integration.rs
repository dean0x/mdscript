use std::collections::HashMap;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn simple_variable_interpolation() {
    let result = mds::compile(&fixture("simple.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("You have 3 items."));
}

#[test]
fn conditional_truthy() {
    let result = mds::compile(&fixture("conditional.mds"), None).unwrap();
    assert!(result.contains("Thanks for being premium!"));
    assert!(!result.contains("Upgrade for premium features."));
}

#[test]
fn conditional_falsy() {
    let result = mds::compile(&fixture("conditional_false.mds"), None).unwrap();
    assert!(!result.contains("Thanks for being premium!"));
    assert!(result.contains("Upgrade for premium features."));
}

#[test]
fn loop_over_array() {
    let result = mds::compile(&fixture("loop.mds"), None).unwrap();
    assert!(result.contains("- apple"));
    assert!(result.contains("- banana"));
    assert!(result.contains("- cherry"));
}

#[test]
fn function_definition_and_call() {
    let result = mds::compile(&fixture("function.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Hello Bob!"));
}

#[test]
fn escaped_braces() {
    let result = mds::compile(&fixture("escaped.mds"), None).unwrap();
    assert!(result.contains("{name}"));
}

#[test]
fn code_block_passthrough() {
    let result = mds::compile(&fixture("code_block.mds"), None).unwrap();
    // Inside code block: no interpolation should occur
    assert!(result.contains("{not_a_var}"));
    assert!(result.contains("{world}"));
    // Outside code block: interpolation should work
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Goodbye Alice!"));
}

#[test]
fn import_alias() {
    let result = mds::compile(&fixture("import_alias.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Goodbye Alice!"));
}

#[test]
fn import_merge() {
    let result = mds::compile(&fixture("import_merge.mds"), None).unwrap();
    assert!(result.contains("Hello Bob!"));
    assert!(result.contains("Goodbye Bob!"));
}

#[test]
fn import_selective() {
    let result = mds::compile(&fixture("import_selective.mds"), None).unwrap();
    assert!(result.contains("Hello Charlie!"));
}

#[test]
fn include_directive() {
    let result = mds::compile(&fixture("include_test.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("Thank you for using our service."));
}

#[test]
fn reexport() {
    let result = mds::compile(&fixture("reexport_consumer.mds"), None).unwrap();
    assert!(result.contains("Hello Dave!"));
}

#[test]
fn wildcard_reexport_barrel() {
    let result = mds::compile(&fixture("barrel_consumer.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("- search"));
    assert!(result.contains("- code"));
    assert!(result.contains("- browse"));
}

#[test]
fn circular_import_error() {
    let result = mds::compile(&fixture("circular_a.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("circular import"),
        "expected circular import error, got: {err}"
    );
    assert!(
        err.contains('\u{2192}'),
        "expected cycle chain with → arrow, got: {err}"
    );
}

#[test]
fn undefined_variable_error() {
    let result = mds::compile(&fixture("undefined_var.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("username"));
}

#[test]
fn arity_mismatch_error() {
    let result = mds::compile(&fixture("arity_error.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("arity") || err.contains("expected 1"));
}

#[test]
fn runtime_vars_override() {
    let mut vars = HashMap::new();
    vars.insert(
        "name".to_string(),
        mds::value::Value::String("Override".to_string()),
    );
    let result = mds::compile(&fixture("simple.mds"), Some(vars)).unwrap();
    assert!(result.contains("Hello Override!"));
}

#[test]
fn complete_example_welcome() {
    let result = mds::compile(&fixture("welcome.mds"), None).unwrap();
    assert!(result.contains("Hello Alice!"));
    assert!(result.contains("- apple"));
    assert!(result.contains("- banana"));
    assert!(result.contains("Thanks for being premium!"));
    assert!(!result.contains("Upgrade for premium features."));
    assert!(result.contains("Thank you for using our service."));
}

#[test]
fn file_not_found_error() {
    let result = mds::compile(&PathBuf::from("nonexistent.mds"), None);
    assert!(result.is_err());
}

#[test]
fn not_mds_file_error() {
    // Try to compile a .md file without type: mds
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("spec.md");
    let result = mds::compile(&path, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("not an MDS file") || err.contains("not_mds"));
}

#[test]
fn vars_file_loading() {
    // Create a temporary vars file
    let vars_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("test_vars.json");
    std::fs::write(&vars_path, r#"{"name": "FromJSON", "count": 99}"#).unwrap();

    let vars = mds::load_vars_file(&vars_path).unwrap();
    assert_eq!(
        vars.get("name"),
        Some(&mds::value::Value::String("FromJSON".to_string()))
    );
    assert_eq!(vars.get("count"), Some(&mds::value::Value::Number(99.0)));

    // Clean up
    let _ = std::fs::remove_file(&vars_path);
}

#[test]
fn check_valid_file() {
    let result = mds::check(&fixture("simple.mds"), None);
    assert!(result.is_ok());
}

#[test]
fn check_invalid_file() {
    let result = mds::check(&fixture("undefined_var.mds"), None);
    assert!(result.is_err());
}

#[test]
fn function_calls_function() {
    let result = mds::compile(&fixture("fn_calls_fn.mds"), None).unwrap();
    assert!(result.contains("[Alice]"));
}

#[test]
fn recursion_detected() {
    let result = mds::compile(&fixture("recursive.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("recursion"));
}

#[test]
fn nested_conditionals() {
    let result = mds::compile(&fixture("nested_if.mds"), None).unwrap();
    assert!(result.contains("outer true"));
    assert!(result.contains("inner false"));
    assert!(!result.contains("inner true"));
    assert!(!result.contains("outer false"));
}

#[test]
fn absolute_import_path_rejected() {
    let result = mds::compile(&fixture("absolute_import.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("relative") || err.contains("import"));
}

#[test]
fn unicode_content() {
    let result = mds::compile(&fixture("unicode.mds"), None).unwrap();
    assert!(result.contains("Greetings Rene!"));
    assert!(result.contains("Hello"));
    // Code block content should not be interpolated
    assert!(result.contains("{not_interpolated}"));
    assert!(result.contains("Farewell Rene!"));
}

#[test]
fn for_iterate_non_array_error() {
    // Attempting to iterate over a non-array should produce a type error
    let mut vars = HashMap::new();
    vars.insert(
        "items".to_string(),
        mds::value::Value::String("not_an_array".to_string()),
    );
    let result = mds::compile(&fixture("loop.mds"), Some(vars));
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("expected array") || err.contains("type error") || err.contains("string"));
}

#[test]
fn empty_array_loop() {
    // Iterating over an empty array should produce no output for the loop body
    let mut vars = HashMap::new();
    vars.insert("items".to_string(), mds::value::Value::Array(vec![]));
    let result = mds::compile(&fixture("loop.mds"), Some(vars)).unwrap();
    assert!(!result.contains("- apple"));
    assert!(!result.contains("- banana"));
}

#[test]
fn compile_str_simple() {
    let source = "---\nname: World\n---\nHello {name}!\n";
    let result = mds::compile_str(source, None, None).unwrap();
    assert!(result.contains("Hello World!"));
}

#[test]
fn compile_str_no_frontmatter() {
    let result = mds::compile_str("Just plain text.", None, None).unwrap();
    assert!(result.contains("Just plain text."));
}

#[test]
fn undefined_function_error() {
    let source = "{nonexistent(\"arg\")}\n";
    let result = mds::compile_str(source, None, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined function") || err.contains("nonexistent"),
        "expected undefined function error, got: {err}"
    );
}

#[test]
fn undefined_namespace_in_qualified_call() {
    let source = "{missing_ns.greet(\"Alice\")}\n";
    let result = mds::compile_str(source, None, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined") || err.contains("missing_ns"),
        "expected undefined namespace error, got: {err}"
    );
}

#[test]
fn name_collision_on_merge_import() {
    let result = mds::compile(&fixture("collision_consumer.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("name collision") || err.contains("greet"),
        "expected name collision error, got: {err}"
    );
}

#[test]
fn merge_import_variable_collision_errors() {
    let result = mds::compile(&fixture("var_collision_consumer.mds"), None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("name collision") || err.contains("role"),
        "expected name collision error for variable, got: {err}"
    );
}

#[test]
fn type_key_available_in_mds_files() {
    let result = mds::compile(&fixture("type_variable.mds"), None).unwrap();
    assert!(
        result.contains("assistant"),
        "expected 'type' variable to be available in .mds files, got: {result}"
    );
}

#[test]
fn undefined_function_error_message_says_function() {
    let source = "{nonexistent_fn(\"arg\")}\n";
    let result = mds::compile_str(source, None, None);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined function") || err.contains("nonexistent_fn"),
        "expected 'undefined function' error, not 'undefined variable', got: {err}"
    );
    assert!(
        !err.contains("undefined variable"),
        "error should say 'function', not 'variable', got: {err}"
    );
}

#[test]
fn for_body_undefined_var_errors_at_validate_time() {
    let result = mds::compile(&fixture("for_body_undef.mds"), None);
    assert!(
        result.is_err(),
        "expected error for undefined var in @for body"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("undefined") || err.contains("undefined_var_in_body"),
        "expected undefined variable error in for body, got: {err}"
    );
}
