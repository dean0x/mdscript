use std::collections::HashMap;
use std::path::Path;

use mds::{FileSystem, MdsError, ModuleCache, NativeFs, Value, VirtualFs, MAX_FILE_SIZE, MAX_TRAVERSAL_DEPTH};

#[test]
fn public_functions_exist() {
    let _ = mds::compile_str("---\nname: World\n---\nHello {name}!\n");
    let _ = mds::compile_str_with("Hello!\n", None, None);
    let _ = mds::compile_str_collecting_warnings("Hello!\n", None, None);
    let _ = mds::compile(Path::new("nonexistent.mds"), None);
    let _ = mds::compile_collecting_warnings(Path::new("nonexistent.mds"), None);
    let _ = mds::compile_file("nonexistent.mds");
    let _ = mds::check_str("Hello!\n");
    let _ = mds::check_str_with("Hello!\n", None, None);
    let _ = mds::check_str_collecting_warnings("Hello!\n", None, None);
    let _ = mds::check(Path::new("nonexistent.mds"), None);
    let _ = mds::check_collecting_warnings(Path::new("nonexistent.mds"), None);
    let _ = mds::load_vars_file(Path::new("nonexistent.json"));
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
    assert!(MAX_TRAVERSAL_DEPTH > 0);
    assert!(MAX_TRAVERSAL_DEPTH <= 1000);
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
    use mds::MdsError;
    use mds::MAX_FILE_SIZE;
    use mds::MAX_TRAVERSAL_DEPTH;

    let _: fn(&str) -> Result<String, MdsError> = |s| mds::compile_str(s);
    assert!(MAX_FILE_SIZE > 0);
    assert!(MAX_TRAVERSAL_DEPTH > 0);
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

#[test]
fn compile_virtual_exists() {
    // compile_virtual is callable with a trivial module.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let result = mds::compile_virtual(modules, "main.mds", None);
    assert!(result.is_ok(), "compile_virtual should succeed: {result:?}");
    assert_eq!(result.unwrap(), "Hello!\n");
}
