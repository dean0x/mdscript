use std::collections::HashMap;

#[test]
fn dot_notation_object_access_works() {
    // {obj.key} now works as object field access (not an error).
    let source = "---\ndata:\n  name: Alice\n---\n{data.name}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("Alice\n"), "got: {result}");
}

#[test]
fn object_single_level_access() {
    let source = "---\nconfig:\n  key: val\n---\n{config.key}\n";
    let result = mds::compile_str(source).unwrap();
    assert_eq!(
        result, "---\nconfig:\n  key: val\n---\nval\n",
        "got: {result}"
    );
}

#[test]
fn object_multi_level_access() {
    let source = "---\na:\n  b:\n    c: deep\n---\n{a.b.c}\n";
    let result = mds::compile_str(source).unwrap();
    assert_eq!(
        result, "---\na:\n  b:\n    c: deep\n---\ndeep\n",
        "got: {result}"
    );
}

#[test]
fn object_direct_interpolation_error() {
    let source = "---\nobj:\n  key: val\n---\n{obj}\n";
    let result = mds::compile_str(source);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("cannot interpolate object"), "got: {err}");
}

#[test]
fn object_field_not_found() {
    let source = "---\nobj:\n  key: val\n---\n{obj.missing}\n";
    let result = mds::compile_str(source);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("not found") && err.contains("missing"),
        "got: {err}"
    );
}

#[test]
fn object_access_on_non_object() {
    let source = "---\nname: Alice\n---\n{name.key}\n";
    let result = mds::compile_str(source);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("cannot access field") && err.contains("string"),
        "got: {err}"
    );
}

#[test]
fn if_dot_path_truthy() {
    let source = "---\nconfig:\n  debug: true\n---\n@if config.debug:\nDEBUG ON\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("DEBUG ON"), "got: {result}");
}

#[test]
fn if_dot_path_falsy() {
    let source =
        "---\nconfig:\n  debug: false\n---\n@if config.debug:\nDEBUG ON\n@else:\nDEBUG OFF\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("DEBUG OFF"), "got: {result}");
}

#[test]
fn for_dot_path_iterable() {
    let source = "---\nconfig:\n  items:\n    - a\n    - b\n---\n@for item in config.items:\n- {item}\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("- a") && result.contains("- b"),
        "got: {result}"
    );
}

#[test]
fn for_key_value_object() {
    let source =
        "---\nobj:\n  alpha: 1\n  beta: 2\n---\n@for key, value in obj:\n{key}={value}\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("alpha=1") && result.contains("beta=2"),
        "got: {result}"
    );
    // Verify alphabetical order
    let alpha_pos = result.find("alpha=1").unwrap();
    let beta_pos = result.find("beta=2").unwrap();
    assert!(
        alpha_pos < beta_pos,
        "keys should be in sorted order, got: {result}"
    );
}

#[test]
fn for_single_var_object_error() {
    let source = "---\nobj:\n  key: val\n---\n@for item in obj:\n{item}\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("key, value") || err.contains("key,value") || err.contains("object"),
        "error should direct to key-value syntax, got: {err}"
    );
}

#[test]
fn for_key_value_non_object_error() {
    let source = "---\nitems:\n  - a\n  - b\n---\n@for k, v in items:\n{k}\n@end\n";
    let result = mds::compile_str(source);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("object") || err.contains("array"),
        "error should mention type, got: {err}"
    );
}

#[test]
fn func_arg_dot_path() {
    let source = "---\nconfig:\n  name: Alice\n---\n@define greet(who):\nHello {who}!\n@end\n{greet(config.name)}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("Hello Alice!"), "got: {result}");
}

#[test]
fn objects_inside_arrays() {
    let source = "---\nitems:\n  - name: Alice\n  - name: Bob\n---\n@for item in items:\n{item.name}\n@end\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("Alice") && result.contains("Bob"),
        "got: {result}"
    );
}

#[test]
fn empty_object_is_falsy() {
    let source = "@if obj:\nTRUTHY\n@else:\nFALSY\n@end\n";
    let mut vars = HashMap::new();
    vars.insert("obj".to_string(), mds::Value::Object(HashMap::new()));
    let result = mds::compile_str_with(source, None, Some(vars)).unwrap();
    assert!(
        result.contains("FALSY"),
        "empty object should be falsy, got: {result}"
    );
}

#[test]
fn namespace_and_object_coexist() {
    // Verify that {obj.key} (MemberAccess) works alongside the existing codebase features.
    let source = "---\nobj:\n  key: val\n---\n{obj.key}\n";
    let result = mds::compile_str(source).unwrap();
    assert_eq!(result, "---\nobj:\n  key: val\n---\nval\n", "got: {result}");
}

#[test]
fn vars_file_with_nested_objects() {
    // Test that load_vars_file handles nested JSON objects
    let dir = tempfile::tempdir().unwrap();
    let vars_path = dir.path().join("vars.json");
    std::fs::write(&vars_path, r#"{"config": {"name": "Test"}}"#).unwrap();
    let vars = mds::load_vars_file(&vars_path).unwrap();
    assert!(matches!(vars.get("config"), Some(mds::Value::Object(_))));
}

#[test]
fn runtime_vars_object_dot_access() {
    // Runtime-supplied objects (via compile_str_with) should be accessible via dot-path.
    // This covers the runtime_vars path, distinct from frontmatter-defined objects.
    let source = "{config.host}:{config.port}\n";
    let mut inner = HashMap::new();
    inner.insert(
        "host".to_string(),
        mds::Value::String("localhost".to_string()),
    );
    inner.insert("port".to_string(), mds::Value::String("8080".to_string()));
    let mut vars = HashMap::new();
    vars.insert("config".to_string(), mds::Value::Object(inner));
    let result = mds::compile_str_with(source, None, Some(vars)).unwrap();
    // No frontmatter in source, so output is body only.
    assert_eq!(result, "localhost:8080\n", "got: {result}");
}

#[test]
fn for_key_value_dot_path_object() {
    // Key-value iteration and dot-path object access should work in combination:
    // @for key, value in config.settings should iterate the nested object.
    let source = "---\nconfig:\n  settings:\n    theme: dark\n    lang: en\n---\n@for k, v in config.settings:\n{k}={v}\n@end\n";
    let result = mds::compile_str(source).unwrap();
    // Entries appear in sorted key order (lang before theme alphabetically).
    // Frontmatter is preserved in output.
    assert_eq!(
        result,
        "---\nconfig:\n  settings:\n    theme: dark\n    lang: en\n---\nlang=en\ntheme=dark\n",
        "got: {result}"
    );
}

#[test]
fn dot_path_depth_limit_interpolation() {
    // Interpolation with >32 dot segments must be rejected at parse time.
    // Build a path with 33 segments: a.b.c.d...
    let segments: Vec<String> = (0..33).map(|i| format!("f{i}")).collect();
    let path = segments.join(".");
    let source = format!("{{{path}}}\n");
    let result = mds::compile_str(&source);
    assert!(result.is_err(), "expected error for >32 segments, got Ok");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("exceeds maximum segment count"),
        "error should mention segment count limit, got: {err}"
    );
}

#[test]
fn dot_path_depth_limit_if_condition() {
    // @if condition with >32 dot segments must be rejected at parse time.
    let segments: Vec<String> = (0..33).map(|i| format!("f{i}")).collect();
    let path = segments.join(".");
    let source = format!("@if {path}:\nyes\n@end\n");
    let result = mds::compile_str(&source);
    assert!(
        result.is_err(),
        "expected error for >32 segments in @if, got Ok"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("exceeds maximum segment count"),
        "error should mention segment count limit, got: {err}"
    );
}

#[test]
fn dot_path_depth_limit_for_iterable() {
    // @for iterable with >32 dot segments must be rejected at parse time.
    let segments: Vec<String> = (0..33).map(|i| format!("f{i}")).collect();
    let path = segments.join(".");
    let source = format!("@for item in {path}:\n{{item}}\n@end\n");
    let result = mds::compile_str(&source);
    assert!(
        result.is_err(),
        "expected error for >32 segments in @for iterable, got Ok"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("exceeds maximum segment count"),
        "error should mention segment count limit, got: {err}"
    );
}

#[test]
fn dot_path_depth_limit_exactly_at_boundary_is_allowed() {
    // A path with exactly MAX_DOT_SEGMENTS (32) segments should not trigger the limit guard.
    // Build root + 31 fields = 32 total segments.
    // We use runtime vars to supply the deeply nested value so we don't need elaborate YAML.
    // Only verify the parse/evaluate doesn't return a depth-limit error; the value
    // itself may fail as undefined, which is acceptable.
    let segments: Vec<String> = (0..32).map(|i| format!("f{i}")).collect();
    let path = segments.join(".");
    let source = format!("{{{path}}}\n");
    let result = mds::compile_str(&source);
    // Should not be a depth-limit error (may be an undefined-variable error).
    if let Err(ref err) = result {
        let err_str = format!("{err}");
        assert!(
            !err_str.contains("exceeds maximum segment count"),
            "boundary-length path (32 segments) should not trigger depth limit, got: {err_str}"
        );
    }
}

#[test]
fn error_reports_full_traversed_path_on_missing_field() {
    // When a field is missing after a successful partial traversal, the error
    // should name the *traversed* path (not just the root variable).
    // For {a.b.missing}, the error should mention "a.b" (the path traversed so far).
    let source = "---\na:\n  b:\n    c: deep\n---\n{a.b.missing}\n";
    let result = mds::compile_str(source);
    assert!(result.is_err(), "expected error for missing field");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("missing") && err.contains("a.b"),
        "error should report field 'missing' not found on 'a.b', got: {err}"
    );
}
