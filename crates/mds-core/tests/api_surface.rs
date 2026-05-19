use std::collections::HashMap;
use std::path::Path;

use mds::{
    CompileOutput, FileSystem, MdsError, ModuleCache, NativeFs, Value, VirtualFs, MAX_FILE_SIZE,
    MAX_TRAVERSAL_DEPTH,
};

#[test]
fn public_functions_exist() {
    let _ = mds::compile_str("---\nname: World\n---\nHello {name}!\n");
    let _ = mds::compile_str_with("Hello!\n", None, None);
    let _ = mds::compile_str_collecting_warnings("Hello!\n", None, None);
    let _ = mds::compile(Path::new("nonexistent.mds"), None);
    let _ = mds::compile_collecting_warnings(Path::new("nonexistent.mds"), None);
    let _ = mds::compile_file("nonexistent.mds");
    let _ = mds::compile_virtual(HashMap::new(), "main.mds", None);
    let _ = mds::compile_virtual_collecting_warnings(HashMap::new(), "main.mds", None);
    let _ = mds::check_str("Hello!\n");
    let _ = mds::check_str_with("Hello!\n", None, None);
    let _ = mds::check_str_collecting_warnings("Hello!\n", None, None);
    let _ = mds::check(Path::new("nonexistent.mds"), None);
    let _ = mds::check_collecting_warnings(Path::new("nonexistent.mds"), None);
    let _ = mds::check_virtual(HashMap::new(), "main.mds", None);
    let _ = mds::check_virtual_collecting_warnings(HashMap::new(), "main.mds", None);
    let _ = mds::load_vars_file(Path::new("nonexistent.json"));
    let _ = mds::load_vars_str("{}");
}

#[test]
fn value_variants_exist() {
    let _ = Value::String("hello".to_string());
    let _ = Value::Number(42.0);
    let _ = Value::Boolean(true);
    let _ = Value::Array(vec![]);
    let _ = Value::Object(HashMap::new());
    let _ = Value::Null;
}

#[test]
fn mds_error_variants_exist() {
    let _ = MdsError::Syntax {
        message: "test".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::UndefinedVariable {
        name: "x".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::UndefinedFunction {
        name: "f".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::ArityMismatch {
        name: "f".to_string(),
        expected: 1,
        got: 2,
        span: None,
        src: None,
    };
    let _ = MdsError::TypeError {
        got: "string".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::CircularImport {
        cycle: "a → b → a".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::FileNotFound {
        path: "missing.mds".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::ImportError {
        message: "test".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::NameCollision {
        name: "x".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::NotMdsFile {
        path: "test.md".to_string(),
    };
    let _ = MdsError::Io {
        message: "test".to_string(),
    };
    let _ = MdsError::ResourceLimit {
        message: "test".to_string(),
    };
    let _ = MdsError::YamlError {
        message: "test".to_string(),
    };
    let _ = MdsError::JsonError {
        message: "test".to_string(),
    };
    let _ = MdsError::Recursion {
        name: "f".to_string(),
        span: None,
        src: None,
    };
    let _ = MdsError::ExportError {
        message: "test".to_string(),
        span: None,
        src: None,
    };

    #[allow(unreachable_patterns)]
    match (MdsError::Io {
        message: "x".to_string(),
    }) {
        MdsError::Syntax { .. }
        | MdsError::UndefinedVariable { .. }
        | MdsError::UndefinedFunction { .. }
        | MdsError::ArityMismatch { .. }
        | MdsError::TypeError { .. }
        | MdsError::CircularImport { .. }
        | MdsError::FileNotFound { .. }
        | MdsError::ImportError { .. }
        | MdsError::NameCollision { .. }
        | MdsError::NotMdsFile { .. }
        | MdsError::Io { .. }
        | MdsError::ResourceLimit { .. }
        | MdsError::YamlError { .. }
        | MdsError::JsonError { .. }
        | MdsError::Recursion { .. }
        | MdsError::ExportError { .. } => {}
        _ => {}
    }
}

#[test]
fn value_trait_impls() {
    let s = Value::from("hello");
    let s2 = Value::from("hello".to_string());
    let n = Value::from(2.72_f64);
    let i = Value::from(42_i64);
    let i32_val = Value::from(7_i32);
    let b = Value::from(true);
    let arr = Value::from(vec![Value::Null]);
    let map: HashMap<String, Value> = HashMap::new();
    let obj = Value::from(map);

    assert_eq!(s, s2);
    let _ = format!("{s}");
    let _ = format!("{n:?}");
    let _ = n.clone();
    let _ = i.clone();
    let _ = i32_val.clone();
    let _ = b.clone();
    let _ = arr.clone();
    let _ = obj.clone();
}

#[test]
fn mds_error_trait_impls() {
    let err = MdsError::Io {
        message: "test".to_string(),
    };

    let _ = format!("{err}");
    let _ = format!("{err:?}");
    let _ = err.clone();

    let std_err: &dyn std::error::Error = &err;
    let _ = std_err.to_string();

    let diagnostic: &dyn miette::Diagnostic = &err;
    let _ = diagnostic.code();
}

#[test]
fn constants_have_expected_values() {
    assert_eq!(MAX_FILE_SIZE, 10 * 1024 * 1024);
    const _: () = assert!(MAX_TRAVERSAL_DEPTH > 0);
    const _: () = assert!(MAX_TRAVERSAL_DEPTH <= 1000);
}

#[test]
fn value_methods() {
    let arr = Value::Array(vec![Value::Null]);
    assert!(arr.is_truthy());
    assert!(arr.as_array().is_some());
    assert_eq!(arr.type_name(), "array");

    let null = Value::Null;
    assert!(!null.is_truthy());
    assert!(null.as_array().is_none());
    assert_eq!(null.type_name(), "null");
}

#[test]
fn cli_import_pattern_works() {
    let _: fn(&str) -> Result<String, MdsError> = |s| mds::compile_str(s);
    const _: () = assert!(MAX_FILE_SIZE > 0);
    const _: () = assert!(MAX_TRAVERSAL_DEPTH > 0);
}

// ── New public types from Phase 2 ─────────────────────────────────────────────

#[test]
fn filesystem_trait_importable() {
    // FileSystem trait is part of the public API.
    fn _accepts_fs(_fs: &dyn FileSystem) {}
    let fs = NativeFs::new();
    _accepts_fs(&fs);
}

#[test]
fn native_fs_new_exists() {
    let _fs = NativeFs::new();
}

#[test]
fn virtual_fs_new_exists() {
    let _fs = VirtualFs::new(HashMap::new());
}

#[test]
fn module_cache_native_constructor() {
    let _cache = ModuleCache::native();
}

#[test]
fn module_cache_virtual_fs_constructor() {
    let _cache = ModuleCache::virtual_fs(HashMap::new());
}

#[test]
fn module_cache_with_fs_constructor() {
    let fs: Box<dyn FileSystem> = Box::new(NativeFs::new());
    let _cache = ModuleCache::with_fs(fs);
}

#[test]
fn module_cache_new_still_works() {
    let _cache = ModuleCache::new();
}

// ── CompileOutput / dependency graph API (Stage 2) ────────────────────────────

#[test]
fn compile_output_type_importable() {
    // CompileOutput must be constructible and implement Debug + Clone + PartialEq.
    let co = CompileOutput {
        output: "hello\n".to_string(),
        warnings: vec!["warn".to_string()],
        dependencies: vec!["lib.mds".to_string()],
    };
    let cloned = co.clone();
    assert_eq!(co, cloned);
    let _ = format!("{co:?}");
}

#[test]
fn compile_output_to_json() {
    // CompileOutput must serialize to JSON with "output", "warnings", "dependencies" keys.
    let co = CompileOutput {
        output: "hello\n".to_string(),
        warnings: vec![],
        dependencies: vec!["dep.mds".to_string()],
    };
    let json = serde_json::to_string(&co).expect("should serialize");
    assert!(json.contains("\"output\""), "missing output key: {json}");
    assert!(
        json.contains("\"warnings\""),
        "missing warnings key: {json}"
    );
    assert!(
        json.contains("\"dependencies\""),
        "missing dependencies key: {json}"
    );
    assert!(json.contains("\"dep.mds\""), "missing dep value: {json}");
}

#[test]
fn compile_with_deps_exists() {
    // compile_with_deps is callable (will error on nonexistent file, which is fine).
    let _ = mds::compile_with_deps(Path::new("nonexistent.mds"), None);
}

#[test]
fn compile_str_with_deps_exists() {
    // compile_str_with_deps compiles successfully.
    let result = mds::compile_str_with_deps("---\nname: World\n---\nHello {name}!\n", None, None)
        .expect("should compile");
    assert_eq!(result.output, "---\nname: World\n---\nHello World!\n");
    assert_eq!(result.dependencies, Vec::<String>::new());
}

#[test]
fn compile_virtual_with_deps_exists() {
    // compile_virtual_with_deps compiles successfully.
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("should compile");
    assert_eq!(result.output, "---\nname: World\n---\nHello World!\n");
    assert_eq!(result.dependencies, Vec::<String>::new());
}

#[test]
fn module_cache_dependencies_exists() {
    // ModuleCache::dependencies() is callable.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    let _ = cache
        .resolve_key("main.mds", &HashMap::new(), &mut warnings)
        .expect("should resolve");
    let deps = cache.dependencies();
    assert!(deps.contains(&"main.mds".to_string()));
}

#[test]
fn compile_with_deps_output_matches_compile() {
    // Same input → same output string as compile_virtual.
    let modules = HashMap::from([(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    )]);
    let baseline = mds::compile_virtual(modules.clone(), "main.mds", None).expect("baseline");
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("with deps");
    assert_eq!(result.output, baseline);
}

// ── Regression: existing functions unchanged ──────────────────────────────────

#[test]
fn compile_virtual_unchanged() {
    // compile_virtual still returns Result<String, MdsError>, not CompileOutput.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result: Result<String, MdsError> = mds::compile_virtual(modules, "main.mds", None);
    assert!(result.is_ok());
}

#[test]
fn compile_str_unchanged() {
    // compile_str still returns Result<String, MdsError>, not CompileOutput.
    let result: Result<String, MdsError> = mds::compile_str("Hello!\n");
    assert!(result.is_ok());
}

#[test]
fn compile_virtual_exists() {
    // compile_virtual is callable with a trivial module.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result = mds::compile_virtual(modules, "main.mds", None);
    assert!(result.is_ok(), "compile_virtual should succeed: {result:?}");
    assert_eq!(result.unwrap(), "Hello!\n");
}

#[test]
fn compile_virtual_collecting_warnings_direct() {
    // Direct call to compile_virtual_collecting_warnings: assert on both the
    // output string and the warnings vector.
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );
    let result = mds::compile_virtual_collecting_warnings(modules, "main.mds", None);
    assert!(
        result.is_ok(),
        "compile_virtual_collecting_warnings should succeed: {result:?}"
    );
    let (output, warnings) = result.unwrap();
    assert!(
        output.contains("Hello World!"),
        "expected rendered output, got: {output}"
    );
    assert!(
        warnings.is_empty(),
        "expected no warnings, got: {warnings:?}"
    );
}

#[test]
fn check_virtual_exists() {
    // check_virtual is callable with a trivial module.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result = mds::check_virtual(modules, "main.mds", None);
    assert!(result.is_ok(), "check_virtual should succeed: {result:?}");
}

#[test]
fn check_virtual_collecting_warnings_direct() {
    // Direct call to check_virtual_collecting_warnings: assert on both the
    // unit result and the warnings vector.
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );
    let result = mds::check_virtual_collecting_warnings(modules, "main.mds", None);
    assert!(
        result.is_ok(),
        "check_virtual_collecting_warnings should succeed: {result:?}"
    );
    let ((), warnings) = result.unwrap();
    assert!(
        warnings.is_empty(),
        "expected no warnings, got: {warnings:?}"
    );
}

#[test]
fn check_virtual_rejects_invalid_module() {
    // check_virtual returns an error for an invalid template.
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "Hello {undefined_var}!\n".to_string(),
    );
    let result = mds::check_virtual(modules, "main.mds", None);
    assert!(
        result.is_err(),
        "check_virtual should fail for undefined variable"
    );
}

/// Integration test for `compile_with_deps` using NativeFs with real on-disk files.
///
/// Creates two .mds files in a tempdir: an entry that imports a library.
/// Verifies that:
/// - Compilation succeeds and output is correct
/// - The imported library appears in dependencies
/// - The entry file itself is excluded from dependencies
#[test]
fn compile_with_deps_native_fs_integration() {
    use std::io::Write;

    let dir = tempfile::TempDir::new().unwrap();

    let lib_path = dir.path().join("lib.mds");
    let mut f = std::fs::File::create(&lib_path).unwrap();
    f.write_all(b"@define greet(x):\nHello {x}!\n@end\n")
        .unwrap();

    let entry_path = dir.path().join("main.mds");
    let mut f = std::fs::File::create(&entry_path).unwrap();
    f.write_all(b"@import \"./lib.mds\"\n{greet(\"World\")}\n")
        .unwrap();

    let result = mds::compile_with_deps(&entry_path, None)
        .expect("compile_with_deps should succeed with real files");

    assert!(
        result.output.contains("Hello World!"),
        "expected rendered output, got: {}",
        result.output
    );
    // The imported lib must appear in deps.
    assert_eq!(
        result.dependencies.len(),
        1,
        "expected 1 dep, got: {:?}",
        result.dependencies
    );
    let dep = &result.dependencies[0];
    assert!(
        dep.ends_with("lib.mds"),
        "expected dep ending in lib.mds, got: {dep}"
    );
    // The entry file must NOT appear in deps (entry-key exclusion by value filter).
    assert!(
        !result.dependencies.iter().any(|d| d.ends_with("main.mds")),
        "entry file must be excluded from deps, got: {:?}",
        result.dependencies
    );
}

/// Test that compiler-emitted warnings surface in `CompileOutput::warnings`.
///
/// The evaluator emits a warning when `@include` is used against a module that
/// has no body text (only macro definitions). This test verifies that the warning
/// makes it into `result.warnings` rather than being silently dropped or sent to
/// stderr.
#[test]
fn compile_output_warnings_emitted_for_empty_include() {
    // A definition-only module: has @define but no top-level body text.
    // @include of this module will produce no output, triggering the warning.
    let mut modules = std::collections::HashMap::new();
    modules.insert(
        "defs.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./defs.mds\" as defs\n@include defs\n{defs.greet(\"World\")}\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("should compile");

    assert!(
        result.output.contains("Hello World!"),
        "expected rendered output, got: {}",
        result.output
    );
    assert!(
        !result.warnings.is_empty(),
        "expected at least one warning for @include of empty module, got none"
    );
    let has_include_warning = result
        .warnings
        .iter()
        .any(|w| w.contains("@include") && w.contains("empty output"));
    assert!(
        has_include_warning,
        "expected warning about empty @include, got: {:?}",
        result.warnings
    );
}

/// Verify that `compile_str_with` resolves `@import` paths relative to the
/// supplied `base_dir`, not its parent. Regression test for the base_key
/// sentinel fix in `resolve_source`.
#[test]
fn compile_str_with_import_resolves_relative_to_base_dir() {
    use std::io::Write;

    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("lib.mds");
    let mut f = std::fs::File::create(&lib_path).unwrap();
    f.write_all(b"@define greet(x):\nHello {x}!\n@end\n")
        .unwrap();

    let source = "@import \"./lib.mds\"\n{greet(\"World\")}\n";
    let result = mds::compile_str_with(source, Some(dir.path()), None);
    assert!(
        result.is_ok(),
        "compile_str_with should succeed: {result:?}"
    );
    let output = result.unwrap();
    assert!(
        output.contains("Hello World!"),
        "expected 'Hello World!' in output, got: {output}"
    );
}

// ── WASM support: Value::from_json + load_vars_str ──────────────────────────

#[test]
fn value_from_json_null() {
    let result = Value::from_json(serde_json::Value::Null).unwrap();
    assert_eq!(result, Value::Null);
}

#[test]
fn value_from_json_string() {
    let result = Value::from_json(serde_json::json!("hello")).unwrap();
    assert_eq!(result, Value::String("hello".to_string()));
}

#[test]
fn value_from_json_number() {
    let result = Value::from_json(serde_json::json!(42)).unwrap();
    assert_eq!(result, Value::Number(42.0));
}

#[test]
fn value_from_json_boolean() {
    let result = Value::from_json(serde_json::json!(true)).unwrap();
    assert_eq!(result, Value::Boolean(true));
}

#[test]
fn value_from_json_array() {
    let result = Value::from_json(serde_json::json!([1, "two", null])).unwrap();
    assert_eq!(
        result,
        Value::Array(vec![
            Value::Number(1.0),
            Value::String("two".to_string()),
            Value::Null,
        ])
    );
}

#[test]
fn value_from_json_object() {
    let result = Value::from_json(serde_json::json!({"a": 1, "b": "c"})).unwrap();
    match result {
        Value::Object(map) => {
            assert_eq!(map.get("a"), Some(&Value::Number(1.0)));
            assert_eq!(map.get("b"), Some(&Value::String("c".to_string())));
        }
        other => panic!("expected Object, got {other:?}"),
    }
}

#[test]
fn value_from_json_depth_limit() {
    // Build 65-level nested array: [[[...[null]...]]]
    let mut val = serde_json::Value::Null;
    for _ in 0..65 {
        val = serde_json::Value::Array(vec![val]);
    }
    let err = Value::from_json(val).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("nesting exceeds maximum depth"), "got: {msg}");
}

#[test]
fn load_vars_str_valid_object() {
    let vars = mds::load_vars_str(r#"{"name": "World", "count": 42}"#).unwrap();
    assert_eq!(vars.get("name"), Some(&Value::String("World".to_string())));
    assert_eq!(vars.get("count"), Some(&Value::Number(42.0)));
}

#[test]
fn load_vars_str_nested_values() {
    let vars = mds::load_vars_str(r#"{"items": [1,2], "config": {"debug": true}}"#).unwrap();
    assert!(matches!(vars.get("items"), Some(Value::Array(_))));
    assert!(matches!(vars.get("config"), Some(Value::Object(_))));
}

#[test]
fn load_vars_str_non_object_json() {
    let err = mds::load_vars_str("[1,2,3]").unwrap_err();
    assert!(err.to_string().contains("vars must be a JSON object"));
}

#[test]
fn load_vars_str_malformed_json() {
    let err = mds::load_vars_str("not json").unwrap_err();
    assert!(err.to_string().contains("JSON"));
}

#[test]
fn load_vars_str_empty_object() {
    let vars = mds::load_vars_str("{}").unwrap();
    assert!(vars.is_empty());
}

#[test]
fn load_vars_str_feeds_compile_virtual() {
    let vars = mds::load_vars_str(r#"{"name": "Test"}"#).unwrap();
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello {name}!\n".to_string());
    let output = mds::compile_virtual(modules, "main.mds", Some(vars)).unwrap();
    assert_eq!(output, "Hello Test!\n");
}
