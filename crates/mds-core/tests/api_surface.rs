use std::collections::HashMap;
use std::path::Path;

use mds::{
    CompileResult, CompiledOutput, FileSystem, LintConfig, LintDiagnostic, LintResult, MdsError,
    ModuleCache, NativeFs, Severity, Value, VirtualFs, MAX_DIAGNOSTICS, MAX_FILE_SIZE,
    MAX_TRAVERSAL_DEPTH,
};

#[test]
fn public_functions_exist() {
    let _: fn(&str) -> Result<String, MdsError> = mds::format_str;
    let _: fn(&str, Option<&Path>) -> Result<String, MdsError> = mds::format_str_with;
    let _: fn(&str, Option<&Path>, &str) -> Result<String, MdsError> = mds::format_str_named;
    let _ = mds::format_str("Hello!\n");
    let _ = mds::format_str_with("Hello!\n", None);
    let _ = mds::format_str_named("Hello!\n", None, "<test>");
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
        expected_min: 1,
        expected_max: 1,
        got: 2,
        span: None,
        src: None,
    };
    let _ = MdsError::BuiltinError {
        message: "type error".to_string(),
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
    let _ = MdsError::FormatterInvariant {
        message: "test".to_string(),
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
        | MdsError::ExportError { .. }
        | MdsError::BuiltinError { .. }
        | MdsError::FormatterInvariant { .. } => {}
        _ => {}
    }
}

#[test]
fn formatter_invariant_has_diagnostic_code() {
    let err = MdsError::FormatterInvariant {
        message: "test detail".to_string(),
    };
    let code = miette::Diagnostic::code(&err)
        .map(|c| c.to_string())
        .unwrap_or_default();
    assert_eq!(code, "mds::formatter_invariant");
    assert!(format!("{err}").contains("test detail"));
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
    // Compile-time check that compile_str matches the fn(&str) -> Result<CompileResult, MdsError> shape.
    let _: fn(&str) -> Result<CompileResult, MdsError> = |s| mds::compile_str(s);
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

// ── CompileResult / CompiledOutput / dependency graph API ─────────────────────

#[test]
fn compile_result_type_importable() {
    // CompileResult is produced by the compile API (it is #[non_exhaustive] — not externally
    // constructible). Its public fields are readable and it implements Debug + Clone + PartialEq.
    let co = mds::compile_str("Hello!\n").expect("should compile");
    let _ = &co.output;
    let _ = &co.warnings;
    let _ = &co.dependencies;
    let _ = &co.source_map;
    let cloned = co.clone();
    assert_eq!(co, cloned);
    let _ = format!("{co:?}");
}

#[test]
fn compiled_output_type_importable() {
    // CompiledOutput is produced by the compile API.  Both CompiledOutput and Message are
    // #[non_exhaustive]; callers obtain them from compile results, not from struct/enum literals.
    // The type implements Debug + Clone + PartialEq and Message fields are publicly readable.
    let md = mds::compile_str("hi\n").expect("compile").output;
    let msgs = mds::compile_str("@message user:\nhi\n@end\n")
        .expect("compile")
        .output;
    assert_eq!(md.clone(), md);
    assert_ne!(md, msgs);
    let _ = format!("{md:?} {msgs:?}");
    // Message fields are readable via if-let (match on #[non_exhaustive] enum needs `_` arm).
    if let CompiledOutput::Messages(v) = &msgs {
        let _ = &v[0].role;
        let _ = &v[0].content;
    }
}

#[test]
fn compile_result_to_json() {
    // CompileResult must serialize to JSON with "output", "warnings", "dependencies" keys,
    // and the output is the adjacently-tagged CompiledOutput shape.
    // Obtain a real CompileResult with a dependency via the compile API.
    let modules = HashMap::from([
        (
            "dep.mds".to_string(),
            "@define greet(x):\nHello {x}!\n@end\n".to_string(),
        ),
        (
            "main.mds".to_string(),
            "@import \"./dep.mds\"\n{greet(\"World\")}\n".to_string(),
        ),
    ]);
    let co = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("should compile");
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
    assert!(
        json.contains("\"kind\""),
        "missing CompiledOutput kind: {json}"
    );
    assert!(
        json.contains("\"markdown\""),
        "missing markdown kind: {json}"
    );
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
    assert_eq!(result.dependencies, Vec::<String>::new());
    assert_eq!(
        result.into_markdown().unwrap(),
        "---\nname: World\n---\nHello World!\n"
    );
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
    assert_eq!(result.dependencies, Vec::<String>::new());
    assert_eq!(
        result.into_markdown().unwrap(),
        "---\nname: World\n---\nHello World!\n"
    );
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
    // Same input → same Markdown output as compile_virtual.
    let modules = HashMap::from([(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    )]);
    let baseline = mds::compile_virtual(modules.clone(), "main.mds", None)
        .expect("baseline")
        .into_markdown()
        .unwrap();
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None)
        .expect("with deps")
        .into_markdown()
        .unwrap();
    assert_eq!(result, baseline);
}

// ── Public entry points return CompileResult ──────────────────────────────────

#[test]
fn compile_virtual_returns_compile_result() {
    // compile_virtual returns Result<CompileResult, MdsError>.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result: Result<CompileResult, MdsError> = mds::compile_virtual(modules, "main.mds", None);
    assert!(result.is_ok());
}

#[test]
fn compile_str_returns_compile_result() {
    // compile_str returns Result<CompileResult, MdsError>.
    let result: Result<CompileResult, MdsError> = mds::compile_str("Hello!\n");
    assert!(result.is_ok());
}

#[test]
fn compile_virtual_exists() {
    // compile_virtual is callable with a trivial module.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result = mds::compile_virtual(modules, "main.mds", None);
    assert!(result.is_ok(), "compile_virtual should succeed: {result:?}");
    assert_eq!(result.unwrap().into_markdown().unwrap(), "Hello!\n");
}

#[test]
fn compile_virtual_collecting_warnings_direct() {
    // Direct call to compile_virtual_collecting_warnings: assert on both the
    // output and the warnings vector.
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
    let result = result.unwrap();
    assert!(
        result.warnings.is_empty(),
        "expected no warnings, got: {:?}",
        result.warnings
    );
    assert!(
        result.into_markdown().unwrap().contains("Hello World!"),
        "expected rendered output"
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

    // The imported lib must appear in deps.
    assert_eq!(
        result.dependencies.len(),
        1,
        "expected 1 dep, got: {:?}",
        result.dependencies
    );
    let dep = result.dependencies[0].clone();
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
    let md = result.into_markdown().unwrap();
    assert!(
        md.contains("Hello World!"),
        "expected rendered output, got: {md}"
    );
}

/// Test that compiler-emitted warnings surface in `CompileResult::warnings`.
///
/// The evaluator emits a warning when `@include` is used against a module that
/// has no body text (only macro definitions). This test verifies that the warning
/// makes it into `result.warnings` rather than being silently dropped or sent to
/// stderr.
#[test]
fn compile_result_warnings_emitted_for_empty_include() {
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
    assert!(
        result.into_markdown().unwrap().contains("Hello World!"),
        "expected rendered output"
    );
}

/// Verify that `compile_str_with` resolves `@import` paths relative to the
/// supplied `base_dir`. The reported `dependencies` must list the real imported
/// path (ending with "lib.mds") with no spurious entries. Regression test for
/// directory-anchored import resolution (PF-003 / #133, #146).
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
    let compiled = result.unwrap();

    // dependencies() must list exactly one entry: the real imported lib.mds path.
    assert_eq!(
        compiled.dependencies.len(),
        1,
        "expected exactly one dependency, got: {:?}",
        compiled.dependencies
    );
    assert!(
        compiled.dependencies[0].ends_with("lib.mds"),
        "expected dependency to resolve to the real lib.mds path, got: {:?}",
        compiled.dependencies
    );
    // Self-documenting: the '<source>' sentinel must never leak into the dependency list
    // now that directory-anchored resolution (#146) replaced the synthetic key entirely.
    assert!(
        !compiled.dependencies.iter().any(|d| d.contains("<source>")),
        "dependency list must not contain the '<source>' sentinel, got: {:?}",
        compiled.dependencies
    );

    let output = compiled.into_markdown().unwrap();
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
    let output = mds::compile_virtual(modules, "main.mds", Some(vars))
        .unwrap()
        .into_markdown()
        .unwrap();
    assert_eq!(output, "Hello Test!\n");
}

// ── Non-UTF-8 path rejection ──────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn check_rejects_non_utf8_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // Construct a path whose bytes are not valid UTF-8.
    let invalid_utf8: &OsStr = OsStrExt::from_bytes(b"/tmp/\xFF\xFE.mds");
    let path = Path::new(invalid_utf8);

    let err = mds::check(path, None).expect_err("expected error for non-UTF-8 path");
    let msg = err.to_string();
    assert!(
        msg.contains("not valid UTF-8"),
        "error message should mention 'not valid UTF-8', got: {msg}"
    );
}

#[cfg(unix)]
#[test]
fn compile_rejects_non_utf8_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let invalid_utf8: &OsStr = OsStrExt::from_bytes(b"/tmp/\xFF\xFE.mds");
    let path = Path::new(invalid_utf8);

    let err = mds::compile(path, None).expect_err("expected error for non-UTF-8 path");
    let msg = err.to_string();
    assert!(
        msg.contains("not valid UTF-8"),
        "error message should mention 'not valid UTF-8', got: {msg}"
    );
}

#[cfg(unix)]
#[test]
fn compile_str_with_rejects_non_utf8_base_dir() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // Construct a base_dir path whose bytes are not valid UTF-8.
    let invalid_utf8: &OsStr = OsStrExt::from_bytes(b"/tmp/\xFF\xFE");
    let path = Path::new(invalid_utf8);

    let err = mds::compile_str_with("Hello!\n", Some(path), None)
        .expect_err("expected error for non-UTF-8 base_dir");
    let msg = err.to_string();
    assert!(
        msg.contains("not valid UTF-8"),
        "error message should mention 'not valid UTF-8', got: {msg}"
    );
}

// ── Issue #23: resolve_path and resolve_source accept &str, not &Path ─────────

#[test]
fn module_cache_resolve_path_accepts_str() {
    // Validates #23: resolve_path now takes &str, not &Path.
    // The test verifies the signature compiles — a file-not-found error is expected
    // since "/nonexistent.mds" does not exist on disk.
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let result = cache.resolve_path("/nonexistent.mds", &HashMap::new(), &mut warnings);
    assert!(result.is_err(), "expected error for nonexistent file");
}

#[test]
fn module_cache_resolve_source_accepts_str() {
    // Validates #23: resolve_source now takes &str for base_dir, not &Path.
    // A simple valid source with no imports should succeed with the current directory.
    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    let base_dir = std::env::current_dir()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let result = cache.resolve_source("Hello!\n", &base_dir, &HashMap::new(), &mut warnings);
    assert!(result.is_ok(), "expected ok for valid source: {result:?}");
}

// ── Intrinsic output API surface (pin public symbols) ─────────────────────────

#[test]
fn message_type_exists() {
    // Message is produced by the compile API (it is #[non_exhaustive] — not externally
    // constructible). Its public fields are readable and it implements Debug + Clone + PartialEq.
    let msgs = mds::compile_str("@message user:\nHello.\n@end\n")
        .expect("should compile")
        .into_messages()
        .expect("messages result");
    let msg = msgs.into_iter().next().expect("one message");
    assert_eq!(msg.role, "user");
    assert_eq!(msg.content, "Hello.");
    let cloned = msg.clone();
    assert_eq!(msg, cloned);
    let _ = format!("{msg:?}");
}

#[test]
fn message_serde_field_names_pinned() {
    // CRITICAL: pin the serde field names "role" and "content" so a future Rust
    // rename cannot silently break the WASM/JS contract that depends on the JSON
    // shape `[{"role":"...", "content":"..."}]`.
    // Obtain the Message from the compile API (Message is #[non_exhaustive]).
    let msgs = mds::compile_str("@message system:\nYou are helpful.\n@end\n")
        .expect("should compile")
        .into_messages()
        .expect("messages result");
    let msg = msgs.into_iter().next().expect("one message");
    let json = serde_json::to_string(&msg).expect("Message must serialize to JSON");
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("Message JSON must be valid");
    assert_eq!(parsed["role"].as_str(), Some("system"));
    assert_eq!(parsed["content"].as_str(), Some("You are helpful."));
}

#[test]
fn into_markdown_and_into_messages_shapes() {
    // Pin the extraction method signatures on CompileResult.
    let _: fn(CompileResult) -> Result<String, MdsError> = CompileResult::into_markdown;
    let _: fn(CompileResult) -> Result<Vec<mds::Message>, MdsError> = CompileResult::into_messages;
}

#[test]
fn compile_file_messages_round_trip() {
    // A file containing @message blocks compiles (intrinsically) to Messages.
    let dir = tempfile::tempdir().unwrap();
    let entry = dir.path().join("chat.mds");
    std::fs::write(
        &entry,
        "@message system:\nYou are a helpful assistant.\n@end\n\
         @message user:\nHello!\n@end\n",
    )
    .unwrap();

    let messages = mds::compile(&entry, None)
        .expect("compile should succeed for a valid file")
        .into_messages()
        .expect("messages result");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content, "You are a helpful assistant.");
    assert_eq!(messages[1].role, "user");
    assert_eq!(messages[1].content, "Hello!");
}

#[test]
fn compile_with_deps_messages_excludes_entry_from_dependencies() {
    // compile_with_deps on a messages template excludes the entry key from deps.
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("helper.mds");
    std::fs::write(&helper, "@define greet(name):\nHello {name}!\n@end\n").unwrap();

    let entry = dir.path().join("chat.mds");
    std::fs::write(
        &entry,
        "@import { greet } from \"./helper.mds\"\n\
         @message user:\n{greet(\"World\")}\n@end\n",
    )
    .unwrap();

    let result = mds::compile_with_deps(&entry, None).expect("compile should succeed");
    let entry_key = entry.display().to_string();
    let entry_canonical = std::fs::canonicalize(&entry).unwrap().display().to_string();
    assert!(
        !result
            .dependencies
            .iter()
            .any(|d| d == &entry_key || d == &entry_canonical),
        "entry key must be excluded from dependencies; got: {:?}",
        result.dependencies
    );
    assert!(
        result.dependencies.iter().any(|d| d.contains("helper.mds")),
        "helper.mds must be in dependencies; got: {:?}",
        result.dependencies
    );
}

#[test]
#[cfg(unix)]
fn compile_rejects_symlinked_entry_for_messages_template() {
    // Symlinked entry rejection applies regardless of output shape.
    let dir = tempfile::tempdir().unwrap();
    let real_file = dir.path().join("real.mds");
    std::fs::write(&real_file, "@message system:\nYou are helpful.\n@end\n").unwrap();
    let link_file = dir.path().join("linked.mds");
    std::os::unix::fs::symlink(&real_file, &link_file).unwrap();

    let result = mds::compile(&link_file, None);
    assert!(result.is_err(), "symlinked entry must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("symlink") || err.contains("not allowed"),
        "error should mention symlink restriction, got: {err}"
    );
}

#[test]
fn compile_max_file_size_still_enforced() {
    // MAX_FILE_SIZE enforcement applies on the entry file regardless of output shape.
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.mds");
    let content = "x".repeat((MAX_FILE_SIZE + 1) as usize);
    std::fs::write(&big, &content).unwrap();

    let result = mds::compile(&big, None);
    assert!(result.is_err(), "oversized entry must be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("too large") || err.contains("maximum size") || err.contains("resource"),
        "error should mention size limit, got: {err}"
    );
}

// ── Lint API surface pins (L-API-1/2/3/4/5) ──────────────────────────────────

/// L-API-1: lint_* function signatures mirror check_* conventions.
#[test]
#[allow(clippy::type_complexity)]
fn lint_api_signatures_exist() {
    // lint_str: simplest form, no options.
    let _: fn(&str) -> Result<LintResult, MdsError> = mds::lint_str;

    // lint_str_with: full options — base_dir, runtime_vars, config.
    let _: fn(
        &str,
        Option<&Path>,
        Option<HashMap<String, Value>>,
        &LintConfig,
    ) -> Result<LintResult, MdsError> = mds::lint_str_with;

    // lint: file-based entry point.
    let _: fn(&Path, Option<HashMap<String, Value>>, &LintConfig) -> Result<LintResult, MdsError> =
        mds::lint;

    // lint_virtual: virtual-FS entry point.
    let _: fn(
        HashMap<String, String>,
        &str,
        Option<HashMap<String, Value>>,
        &LintConfig,
    ) -> Result<LintResult, MdsError> = mds::lint_virtual;
}

/// L-API-2: pub types LintDiagnostic, Severity, LintConfig, LintResult are accessible
/// and have the expected fields/variants.
#[test]
fn lint_types_exist() {
    // Severity has four variants with lowercase serde names.
    let _off = Severity::Off;
    let _info = Severity::Info;
    let _warn = Severity::Warn;
    let _err = Severity::Error;

    // LintConfig has a `rules` field (HashMap<String, Severity>).
    let config = LintConfig {
        rules: HashMap::from([("unused-variable".to_string(), Severity::Off)]),
    };
    assert_eq!(config.rules.get("unused-variable"), Some(&Severity::Off));

    // LintDiagnostic has the expected fields.
    let diag = LintDiagnostic {
        rule: "unused-variable".to_string(),
        severity: Severity::Warn,
        message: "Variable 'name' is never used".to_string(),
        help: Some("Remove the frontmatter key or reference it in the body".to_string()),
        span: None,
        file: Some("test.mds".to_string()),
    };
    assert_eq!(diag.rule, "unused-variable");
    assert_eq!(diag.severity, Severity::Warn);

    // LintResult has diagnostics, truncated, and is_standalone fields.
    let result = LintResult {
        diagnostics: vec![diag],
        truncated: false,
        is_standalone: false,
    };
    assert_eq!(result.diagnostics.len(), 1);
    assert!(!result.truncated);
}

/// L-API-4: MdsError enum is unchanged — lint findings are LintDiagnostic, not MdsError variants.
#[test]
fn mds_error_variants_unchanged_by_lint() {
    // All pre-existing MdsError variants still exist and are exhaustively matched.
    // This is a compile-time check — if new variants were accidentally added, this match
    // would not produce an "unreachable pattern" warning (since we allow it via `_ => {}`).
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
        | MdsError::ExportError { .. }
        | MdsError::BuiltinError { .. }
        | MdsError::FormatterInvariant { .. } => {}
        _ => {}
    }
}

/// L-API-5: MAX_DIAGNOSTICS is 1_000, a publicly re-exported constant from limits.rs.
#[test]
fn max_diagnostics_pinned() {
    assert_eq!(MAX_DIAGNOSTICS, 1_000);
}

/// L-U-JSON1: LintResult::to_canonical_json produces the expected schema shape.
#[test]
fn lint_canonical_json_schema() {
    use mds::SerializedSpan;

    let result = LintResult {
        diagnostics: vec![LintDiagnostic {
            rule: "unused-variable".to_string(),
            severity: Severity::Warn,
            message: "Variable 'name' is never used".to_string(),
            help: Some("Remove the frontmatter key or reference it in the body".to_string()),
            span: Some(SerializedSpan {
                offset: 4,
                length: 4,
                line: Some(2),
                column: Some(1),
            }),
            file: Some("test.mds".to_string()),
        }],
        truncated: false,
        is_standalone: false,
    };

    let json = result.to_canonical_json();

    // Top-level shape.
    assert_eq!(json["version"], 1, "version must be 1");
    assert!(json["files"].is_array(), "files must be an array");
    assert!(
        !json["truncated"].as_bool().unwrap_or(true),
        "truncated must be false"
    );

    // Per-file grouping.
    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "one file entry for test.mds");
    let file_entry = &files[0];
    assert_eq!(file_entry["file"], "test.mds");
    assert!(file_entry["diagnostics"].is_array());

    // Per-diagnostic shape.
    let diags = file_entry["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1);
    let d = &diags[0];
    assert_eq!(d["rule"], "unused-variable");
    assert_eq!(d["severity"], "warn");
    assert!(d["message"].is_string());
    assert!(d["help"].is_string());
    assert_eq!(d["fixable"], false);

    // SerializedError-compatible span shape.
    let span = &d["span"];
    assert_eq!(span["offset"], 4);
    assert_eq!(span["length"], 4);
    assert_eq!(span["line"], 2);
    assert_eq!(span["column"], 1);
}

/// L-U-JSON1b: to_canonical_json fixable field reflects tier semantics and is_standalone flag.
#[test]
fn lint_canonical_json_fixable_semantics() {
    use mds::LintDiagnostic;

    // Tier A rule (duplicate-import) → fixable regardless of is_standalone.
    let tier_a = LintResult {
        diagnostics: vec![LintDiagnostic {
            rule: "duplicate-import".to_string(),
            severity: Severity::Error,
            message: "Duplicate import".to_string(),
            help: None,
            span: None,
            file: Some("a.mds".to_string()),
        }],
        truncated: false,
        is_standalone: false, // even non-standalone Tier A is fixable
    };
    let json = tier_a.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], true);

    // Tier B rule (unused-import) → fixable only for standalone files.
    let tier_b_non_standalone = LintResult {
        diagnostics: vec![LintDiagnostic {
            rule: "unused-import".to_string(),
            severity: Severity::Warn,
            message: "Unused import".to_string(),
            help: None,
            span: None,
            file: Some("b.mds".to_string()),
        }],
        truncated: false,
        is_standalone: false,
    };
    let json = tier_b_non_standalone.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], false);

    let tier_b_standalone = LintResult {
        diagnostics: vec![LintDiagnostic {
            rule: "unused-import".to_string(),
            severity: Severity::Warn,
            message: "Unused import".to_string(),
            help: None,
            span: None,
            file: Some("c.mds".to_string()),
        }],
        truncated: false,
        is_standalone: true,
    };
    let json = tier_b_standalone.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], true);

    // Tier C rule (unused-variable) → never fixable.
    let tier_c = LintResult {
        diagnostics: vec![LintDiagnostic {
            rule: "unused-variable".to_string(),
            severity: Severity::Warn,
            message: "Unused variable".to_string(),
            help: None,
            span: None,
            file: Some("d.mds".to_string()),
        }],
        truncated: false,
        is_standalone: true, // even standalone Tier C is not fixable
    };
    let json = tier_c.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], false);
}

/// lint_str on a trivially valid template returns an empty LintResult (no diagnostics).
#[test]
fn lint_str_trivial_source_returns_empty() {
    let result = mds::lint_str("Hello!\n").expect("lint_str should succeed for valid source");
    assert!(
        result.diagnostics.is_empty(),
        "trivial source should produce no diagnostics: {result:?}"
    );
    assert!(!result.truncated);
    // A file with no imports or @extends is standalone.
    assert!(
        result.is_standalone,
        "plain source with no imports should be standalone"
    );
}

/// lint_str_with on a source that has an @import is not standalone.
#[test]
fn lint_str_with_imports_is_not_standalone() {
    // Source with an import that doesn't exist → check gate fails, MdsError.
    // Use a virtual-fs approach via lint_virtual to test is_standalone=false.
    let mut modules = std::collections::HashMap::new();
    modules.insert("lib.mds".to_string(), "Hello!\n".to_string());
    modules.insert(
        "consumer.mds".to_string(),
        "@import \"./lib.mds\" as lib\nHi!\n".to_string(),
    );
    let config = LintConfig::default();
    let result = mds::lint_virtual(modules, "consumer.mds", None, &config).expect("should lint OK");
    // consumer.mds has @import — it is NOT standalone.
    assert!(
        !result.is_standalone,
        "file with @import should not be standalone"
    );
}

/// lint_virtual on a valid virtual FS returns an empty LintResult.
#[test]
fn lint_virtual_trivial_returns_empty() {
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let config = LintConfig::default();
    let result =
        mds::lint_virtual(modules, "main.mds", None, &config).expect("lint_virtual should succeed");
    assert!(result.diagnostics.is_empty());
}

/// lint_str on an invalid template (syntax error) returns Err(MdsError).
#[test]
fn lint_str_invalid_source_returns_err() {
    // Unclosed @if block is a syntax error — should fail the check gate.
    let result = mds::lint_str("@if x:\nhello\n");
    assert!(
        result.is_err(),
        "lint_str should return Err for invalid source"
    );
}

// ── D2: ExportDirective offset regression (L-API-3) ─────────────────────────
//
// ExportDirective is pub(crate), so we cannot name it directly in an integration
// test.  Instead we verify that parsing and resolution of all three export forms
// still succeed after the D2 offset field addition — a regression in any variant
// would surface here as a compile or runtime failure.

#[test]
fn export_directive_forms_still_resolve_after_d2() {
    // Named export: @export greet
    // ReExport: @export greet from "..."
    // Wildcard: @export * from "..."
    // All three forms must still parse and resolve without error.
    let named_src = "@define greet():\nhello\n@end\n@export greet\n";
    let reexport_src = "@export greet from \"./lib.mds\"\n";
    let wildcard_src = "@export * from \"./lib.mds\"\n";
    let lib_src = "@define greet():\nhello\n@end\n@export greet\n";

    // Named: check that a module with @export named resolves.
    let result = mds::check_str(named_src);
    assert!(
        result.is_ok(),
        "Named export form should resolve: {result:?}"
    );

    // ReExport and Wildcard require an importable lib module — use virtual FS.
    let mut modules = std::collections::HashMap::new();
    modules.insert("lib.mds".to_string(), lib_src.to_string());
    modules.insert("main_reexport.mds".to_string(), reexport_src.to_string());
    let result = mds::check_virtual(modules.clone(), "main_reexport.mds", None);
    assert!(result.is_ok(), "ReExport form should resolve: {result:?}");

    let mut modules2 = std::collections::HashMap::new();
    modules2.insert("lib.mds".to_string(), lib_src.to_string());
    modules2.insert("main_wildcard.mds".to_string(), wildcard_src.to_string());
    let result = mds::check_virtual(modules2, "main_wildcard.mds", None);
    assert!(result.is_ok(), "Wildcard form should resolve: {result:?}");
}

// ── NativeFs::check_symlink public API pin ────────────────────────────────────

#[test]
fn native_fs_check_symlink_is_public() {
    // Pin that NativeFs::check_symlink is part of the public API surface.
    // This will fail to compile if the visibility is ever narrowed back to pub(crate).
    use std::path::PathBuf;
    type CheckSymlinkFn = fn(&Path) -> Result<PathBuf, MdsError>;
    let _: CheckSymlinkFn = mds::NativeFs::check_symlink;
}

// ── CompileOptions shape (T1 wire-format parity gate) ────────────────────────

#[test]
fn compile_options_has_source_map_and_include_sources_content() {
    // T1: both fields must exist and be independently settable.
    let off = mds::CompileOptions {
        source_map: false,
        include_sources_content: false,
    };
    assert!(!off.source_map);
    assert!(!off.include_sources_content);

    let on = mds::CompileOptions {
        source_map: true,
        include_sources_content: true,
    };
    assert!(on.source_map);
    assert!(on.include_sources_content);

    // Default: both false (opts-in semantics — zero cost unless requested).
    let d = mds::CompileOptions::default();
    assert!(!d.source_map, "source_map default must be false");
    assert!(
        !d.include_sources_content,
        "include_sources_content default must be false"
    );
}

#[test]
fn include_sources_content_false_omits_sources_content() {
    // source_map: true, include_sources_content: false → map present, sourcesContent absent.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result = mds::compile_virtual_with_deps_opts(
        modules,
        "main.mds",
        None,
        mds::CompileOptions {
            source_map: true,
            include_sources_content: false,
        },
    )
    .expect("should compile");

    let sm = result.source_map.expect("source_map must be present");
    // sourcesContent must be absent when include_sources_content=false.
    assert!(
        sm.sources_content.is_none(),
        "sourcesContent must be None when include_sources_content=false; got: {:?}",
        sm.sources_content
    );
}

#[test]
fn include_sources_content_true_includes_sources_content() {
    // source_map: true, include_sources_content: true → sourcesContent present.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result = mds::compile_virtual_with_deps_opts(
        modules,
        "main.mds",
        None,
        mds::CompileOptions {
            source_map: true,
            include_sources_content: true,
        },
    )
    .expect("should compile");

    let sm = result.source_map.expect("source_map must be present");
    assert!(
        sm.sources_content.is_some(),
        "sourcesContent must be Some when include_sources_content=true"
    );
}

// ── Fix API surface pin (F-API-1) ─────────────────────────────────────────────

/// F-API-1: apply_fixes_incremental and associated types exist on the public API surface.
///
/// Pins:
/// - `mds::fix::apply_fixes_incremental` is callable with `F: Fn(&str) -> Result<LintResult, MdsError>`
/// - `mds::fix::FixOutcome::PartiallyFixed` variant is exhaustively matchable
/// - `mds::fix::RejectedEdit` struct has the expected `edit: ByteEdit` and `reason: String` fields
#[test]
fn fix_api_incremental_exists() {
    use mds::fix::{apply_fixes_incremental, plan_fixes, ByteEdit, FixOutcome, RejectedEdit};

    // FixOutcome is #[non_exhaustive]: external matches need a wildcard arm.
    // We still enumerate all known variants to pin their shapes at compile time.
    let outcome: FixOutcome = FixOutcome::NothingToFix;
    #[allow(clippy::match_single_binding)]
    #[allow(unreachable_patterns)]
    match outcome {
        FixOutcome::Fixed { .. }
        | FixOutcome::PartiallyFixed { .. }
        | FixOutcome::Rejected { .. }
        | FixOutcome::NothingToFix => {}
        _ => {} // required: FixOutcome is #[non_exhaustive]
    }

    // RejectedEdit struct has `edit` and `reason` fields.
    let edit = ByteEdit {
        start: 0,
        end: 5,
        rule: "duplicate-import".to_string(),
    };
    let rejected = RejectedEdit {
        edit,
        reason: "simulated reverify failure".to_string(),
    };
    assert_eq!(rejected.reason, "simulated reverify failure");
    assert_eq!(rejected.edit.rule, "duplicate-import");

    // apply_fixes_incremental is callable with F: Fn — compile-time and runtime check.
    let source = "Hello!\n";
    let original = LintResult {
        diagnostics: vec![],
        truncated: false,
        is_standalone: false,
    };
    let plan = plan_fixes(&original, source);
    let outcome = apply_fixes_incremental(
        source,
        plan,
        &original,
        |_s| -> Result<LintResult, MdsError> {
            Ok(LintResult {
                diagnostics: vec![],
                truncated: false,
                is_standalone: false,
            })
        },
    );
    // Empty source with no diagnostics → NothingToFix (no reverify called).
    assert!(
        matches!(outcome, FixOutcome::NothingToFix),
        "trivial source with no diagnostics must return NothingToFix; got: {outcome:?}"
    );
}

/// Regression gate (issue #9): `STRING_SOURCE_MAP_LABEL` must be reachable from
/// the public `mds` API so every surface can import it rather than redeclaring
/// the literal (avoids PF-007 per-surface re-declaration defeating cross-surface
/// byte-parity; applies ADR-005).
///
/// This test fails to COMPILE if the constant reverts to `pub(crate)`.
#[test]
fn string_source_map_label_is_in_public_api() {
    let label: &str = mds::STRING_SOURCE_MAP_LABEL;
    assert_eq!(
        label, "input.mds",
        "STRING_SOURCE_MAP_LABEL must equal \"input.mds\"; changing it requires \
         updating every surface that uses it"
    );
}
