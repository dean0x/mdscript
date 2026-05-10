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
    assert!(err.contains("circular import"));
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
    vars.insert("name".to_string(), mds::value::Value::String("Override".to_string()));
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
    assert_eq!(
        vars.get("count"),
        Some(&mds::value::Value::Number(99.0))
    );

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
