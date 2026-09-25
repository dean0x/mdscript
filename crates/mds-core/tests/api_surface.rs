use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

use mds::{
    CompileResult, CompiledOutput, FileSystem, FixLineSpan, LintConfig, LintDiagnostic, LintResult,
    MdsError, ModuleCache, NativeFs, Severity, Value, VirtualFs, MAX_DIAGNOSTICS, MAX_FILE_SIZE,
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
    let _ = mds::compile_str("---\nname: World\n---\nHello {{name}}!\n");
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
    let _ = mds::load_vars_file_reporting_duplicates(Path::new("nonexistent.json"));
    let _ = mds::load_vars_str_reporting_duplicates("{}");
}

/// #409: `display_native_path` is callable via the crate root with the expected
/// `Path -> Cow<Path>` signature. It is the single public entry point CLI and
/// binding display sinks use to strip a Windows verbatim prefix (`\\?\C:\…`)
/// from a path before showing it to a user; off Windows (this host) a canonical
/// path is never verbatim, so it is a documented no-op — pinned here rather than
/// under `#[cfg(windows)]`, since every host must have this function.
#[test]
fn display_native_path_function_exists() {
    let _: fn(&Path) -> Cow<'_, Path> = mds::display_native_path;
    let unchanged = Path::new("relative/path.mds");
    assert_eq!(&*mds::display_native_path(unchanged), unchanged);
}

/// #409 (Windows only): `display_native_path` actually strips a lossless
/// verbatim prefix. The pin test above only exercises the no-op case, which
/// passes trivially on every host, Windows included.
#[cfg(windows)]
#[test]
fn display_native_path_strips_verbatim_prefix_on_windows() {
    let verbatim = Path::new(r"\\?\C:\Users\example\file.mds");
    let shown = mds::display_native_path(verbatim);
    assert_eq!(shown.as_ref(), Path::new(r"C:\Users\example\file.mds"));
}

/// #265: `is_forbidden_path_char` and `escape_path_for_message` are callable via
/// the crate root with the expected signatures. Additive-only for now — nothing
/// in this crate enforces the predicate yet; enforcement is a follow-up commit.
#[test]
fn forbidden_path_char_functions_exist() {
    let _: fn(char) -> bool = mds::is_forbidden_path_char;
    let _: fn(&str) -> Cow<'_, str> = mds::escape_path_for_message;
    assert!(mds::is_forbidden_path_char('\t'));
    assert!(!mds::is_forbidden_path_char('a'));
    assert_eq!(&*mds::escape_path_for_message("a\tb"), "a\\u0009b");
}

/// #326: `VarsLoad` fields are readable from an external crate. `#[non_exhaustive]`
/// forbids a struct literal, so the type is only obtainable through the load API.
#[test]
fn vars_load_fields_are_readable() {
    let loaded = mds::load_vars_str_reporting_duplicates(r#"{"x": 1, "x": 2}"#)
        .expect("should load duplicate-key vars");
    let _: &HashMap<String, Value> = &loaded.vars;
    let _: &Vec<String> = &loaded.duplicate_keys;
    let _: usize = loaded.duplicate_keys_omitted;
    assert_eq!(loaded.duplicate_keys, vec!["x".to_string()]);
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
        signature_note: String::new(),
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

/// Pin `MdsError::source_name()` — the neutral domain accessor for the embedded
/// `NamedSource` name (ADR-010).
#[test]
fn mds_error_source_name_accessor() {
    use std::sync::Arc;

    // Errors with an embedded source carry its name.
    let with_src = MdsError::Syntax {
        message: "test".to_string(),
        span: None,
        src: Some(Arc::new(miette::NamedSource::new(
            "myfile.mds",
            "content".to_string(),
        ))),
    };
    assert_eq!(with_src.source_name(), Some("myfile.mds"));

    // Errors without an embedded source return None.
    let without_src = MdsError::Io {
        message: "test".to_string(),
    };
    assert_eq!(without_src.source_name(), None);
}

/// Pin `MdsError::is_string_source()` — the compile-time-coupled sentinel predicate
/// (ADR-010).  This predicate keeps the `pub(crate)` sentinel comparison inside
/// `mds-core`; downstream crates (e.g. `mds-cli`) use it instead of hardcoding the
/// literal `"<source>"`, which would have no compile-time link to the definition.
///
/// Positive control: an error whose embedded source name is the internal sentinel
/// `"<source>"` must return `true`.
/// Negative controls: a non-sentinel source name and a source-less error must return
/// `false`.  (PF-013: absence alone proves nothing; positive control is mandatory.)
#[test]
fn mds_error_is_string_source() {
    use std::sync::Arc;

    // Positive control: the internal sentinel value "<source>" — as set by
    // `resolve_source_intrinsic`.
    let sentinel_err = MdsError::Syntax {
        message: "test".to_string(),
        span: None,
        src: Some(Arc::new(miette::NamedSource::new(
            "<source>",
            "content".to_string(),
        ))),
    };
    assert!(
        sentinel_err.is_string_source(),
        "is_string_source() must return true for the internal sentinel"
    );

    // Negative control: a real file path must not match.
    let file_err = MdsError::Syntax {
        message: "test".to_string(),
        span: None,
        src: Some(Arc::new(miette::NamedSource::new(
            "real_file.mds",
            "content".to_string(),
        ))),
    };
    assert!(
        !file_err.is_string_source(),
        "is_string_source() must return false for a real file path"
    );

    // Negative control: an error without an embedded source.
    let no_src_err = MdsError::Io {
        message: "test".to_string(),
    };
    assert!(
        !no_src_err.is_string_source(),
        "is_string_source() must return false when there is no embedded source"
    );
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

// ── FileSystem required-method set + resolver entry validation (#155) ─────────

/// A custom backend implementing EXACTLY the required `FileSystem` methods.
///
/// This impl is the pin: a new required method fails to compile here (E0046), and
/// so does removing one of these five (E0407). `resolve_entry` is an identity
/// function that performs NO validation of its own and counts its calls, so the
/// tests below can prove the resolver validates entry paths before a custom
/// backend is ever reached.
struct IdentityFs {
    inner: VirtualFs,
    resolve_entry_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl IdentityFs {
    fn new(
        modules: HashMap<String, String>,
    ) -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fs = Self {
            inner: VirtualFs::new(modules),
            resolve_entry_calls: std::sync::Arc::clone(&calls),
        };
        (fs, calls)
    }
}

impl FileSystem for IdentityFs {
    fn resolve_entry(&self, path: &str) -> Result<String, MdsError> {
        self.resolve_entry_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(path.to_string())
    }
    fn normalize_in_dir(&self, dir: &str, relative: &str) -> Result<String, MdsError> {
        self.inner.normalize_in_dir(dir, relative)
    }
    fn parent_dir(&self, key: &str) -> String {
        self.inner.parent_dir(key)
    }
    fn read(&self, key: &str) -> Result<String, MdsError> {
        self.inner.read(key)
    }
    fn is_markdown(&self, key: &str) -> bool {
        self.inner.is_markdown(key)
    }
}

fn diagnostic_code(err: &MdsError) -> Option<String> {
    miette::Diagnostic::code(err).map(|c| c.to_string())
}

#[test]
fn filesystem_trait_required_methods_pin() {
    let modules = HashMap::from([("main.mds".to_string(), "Hello!\n".to_string())]);
    let (fs, calls) = IdentityFs::new(modules);
    let fs: Box<dyn FileSystem> = Box::new(fs);
    let mut cache = ModuleCache::with_fs(fs);
    let output = cache
        .resolve_path_intrinsic("main.mds", &HashMap::new(), &mut vec![])
        .expect("a backend with only the required methods must resolve an entry");
    assert!(
        matches!(&output, CompiledOutput::Markdown(s) if s == "Hello!\n"),
        "unexpected output: {output:?}"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the entry must be resolved through FileSystem::resolve_entry"
    );
}

/// AC-155-5: entry validation runs in the resolver, so a custom backend whose
/// `resolve_entry` validates nothing still never sees an empty or NUL entry path.
#[test]
fn custom_backend_entry_validation_runs_before_backend() {
    for bad in ["", "a\0b.mds"] {
        let (fs, calls) = IdentityFs::new(HashMap::new());
        let mut cache = ModuleCache::with_fs(Box::new(fs));
        let err = cache
            .resolve_path(bad, &HashMap::new(), &mut vec![])
            .unwrap_err();
        assert_eq!(
            diagnostic_code(&err).as_deref(),
            Some("mds::io"),
            "entry {bad:?}: expected mds::io, got {err:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "entry {bad:?}: the backend must not be called"
        );
    }

    // Control: a valid entry path does reach the backend.
    let modules = HashMap::from([("main.mds".to_string(), "Hi\n".to_string())]);
    let (fs, calls) = IdentityFs::new(modules);
    let mut cache = ModuleCache::with_fs(Box::new(fs));
    cache
        .resolve_path("main.mds", &HashMap::new(), &mut vec![])
        .expect("control: a valid entry resolves");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// AC-155-1: `canonicalize`, `set_root` and `normalize` are not `FileSystem`
/// methods — not even DEFAULTED ones, which `IdentityFs` cannot pin (it only stops
/// compiling when the REQUIRED set changes).
///
/// Method lookup on a `dyn FileSystem` prefers the trait's own methods over any
/// other trait's, so each probe answers `"probe"` only while `FileSystem` has no
/// method of that name; if one came back, its call below would stop compiling or
/// stop answering `"probe"`.
trait TraitMethodProbe {
    fn canonicalize(&self, _path: &str) -> &'static str {
        "probe"
    }
    fn set_root(&self, _base: &str) -> &'static str {
        "probe"
    }
    fn normalize(&self, _base: &str, _relative: &str) -> &'static str {
        "probe"
    }
    // Never called while the trait has `anchor_base_dir` — its method shadows
    // this one. If the trait lost it, this probe would be called and the
    // `expect` would go unfulfilled: a warning, fatal under `-D warnings`.
    #[expect(
        dead_code,
        reason = "shadowed by FileSystem::anchor_base_dir (positive control)"
    )]
    fn anchor_base_dir(&self, _dir: &str) -> &'static str {
        "probe"
    }
}
impl<T: FileSystem + ?Sized> TraitMethodProbe for T {}

#[test]
fn filesystem_trait_removed_methods_pin() {
    let fs: &dyn FileSystem = &VirtualFs::new(HashMap::new());
    assert_eq!(fs.canonicalize("x"), "probe");
    assert_eq!(fs.set_root("x"), "probe");
    assert_eq!(fs.normalize("", "x"), "probe");
    // Positive control: a method the trait DOES have shadows its probe — this
    // binding only compiles because the trait's `anchor_base_dir` was chosen.
    let anchored: Result<String, MdsError> = fs.anchor_base_dir("x");
    assert_eq!(anchored.unwrap(), "x");
}

/// A custom backend that does not override `anchor_base_dir` gets the identity:
/// the base directory of a string compile is used unchanged as a key-space
/// directory, and imports resolve from it.
#[test]
fn anchor_base_dir_default_is_identity_for_custom_backends() {
    let modules = HashMap::from([(
        "virtual/dir/lib.mds".to_string(),
        "@define hi():\nHi from lib\n@end\n".to_string(),
    )]);
    let (fs, _calls) = IdentityFs::new(modules);
    // Fully qualified: `TraitMethodProbe` above also names `anchor_base_dir`.
    assert_eq!(
        FileSystem::anchor_base_dir(&fs, "virtual/dir").unwrap(),
        "virtual/dir"
    );

    let mut cache = ModuleCache::with_fs(Box::new(fs));
    let output = cache
        .resolve_source_intrinsic(
            "@import \"./lib.mds\" as lib\n{{lib.hi()}}\n",
            "virtual/dir",
            &HashMap::new(),
            &mut vec![],
        )
        .expect("imports resolve from the unchanged base directory");
    assert!(
        matches!(&output, CompiledOutput::Markdown(s) if s.contains("Hi from lib")),
        "unexpected output: {output:?}"
    );
}

/// A NUL byte in an entry path is caller input, not an `@import` string: it
/// reports `mds::io` through the public compile API and on the virtual backend.
#[test]
fn nul_in_entry_path_is_io_error() {
    let err = mds::compile("./\0evil.mds", None).unwrap_err();
    assert_eq!(
        diagnostic_code(&err).as_deref(),
        Some("mds::io"),
        "compile: got {err:?}"
    );

    let mut cache = ModuleCache::virtual_fs(HashMap::new());
    let err = cache
        .resolve_path("a\0b.mds", &HashMap::new(), &mut vec![])
        .unwrap_err();
    assert_eq!(
        diagnostic_code(&err).as_deref(),
        Some("mds::io"),
        "virtual: got {err:?}"
    );

    // Control: a NUL byte in an @import string stays mds::import.
    let modules = HashMap::from([(
        "main.mds".to_string(),
        "@import \"./a\0b.mds\"\n".to_string(),
    )]);
    let err = mds::compile_virtual(modules, "main.mds", None).unwrap_err();
    assert_eq!(
        diagnostic_code(&err).as_deref(),
        Some("mds::import"),
        "import control: got {err:?}"
    );
}

/// AC-155-3: every virtual entry API validates the entry key before resolving it.
/// The module map DOES contain the bad key, so an unvalidated path would read and
/// compile it; the refusal can only come from entry validation.
#[test]
fn virtual_entry_apis_validate_the_entry_key() {
    type EntryApi = fn(HashMap<String, String>, &str) -> Result<(), MdsError>;
    let apis: [(&str, EntryApi); 6] = [
        ("ModuleCache::resolve_virtual_intrinsic", |m, e| {
            ModuleCache::virtual_fs(m)
                .resolve_virtual_intrinsic(e, &HashMap::new(), &mut vec![])
                .map(drop)
        }),
        ("ModuleCache::resolve_virtual_intrinsic_opts", |m, e| {
            ModuleCache::virtual_fs(m)
                .resolve_virtual_intrinsic_opts(
                    e,
                    &HashMap::new(),
                    &mds::CompileOptions::default(),
                    &mut vec![],
                )
                .map(drop)
        }),
        ("ModuleCache::resolve_key", |m, e| {
            ModuleCache::virtual_fs(m)
                .resolve_key(e, &HashMap::new(), &mut vec![])
                .map(drop)
        }),
        ("compile_virtual", |m, e| {
            mds::compile_virtual(m, e, None).map(drop)
        }),
        ("check_virtual", |m, e| mds::check_virtual(m, e, None)),
        ("lint_virtual", |m, e| {
            mds::lint_virtual(m, e, None, &LintConfig::default()).map(drop)
        }),
    ];
    for (name, api) in apis {
        for bad in ["", "a\0b.mds"] {
            let modules = HashMap::from([(bad.to_string(), "Hello!\n".to_string())]);
            let err = api(modules, bad).unwrap_err();
            assert_eq!(
                diagnostic_code(&err).as_deref(),
                Some("mds::io"),
                "{name} with entry {bad:?}: expected mds::io, got {err:?}"
            );
        }
        // Control: a valid key in the same position resolves.
        let modules = HashMap::from([("main.mds".to_string(), "Hello!\n".to_string())]);
        api(modules, "main.mds").unwrap_or_else(|e| panic!("{name} control: {e:?}"));
    }
}

#[test]
fn module_cache_new_still_works() {
    let _cache = ModuleCache::new();
}

// ── CompileResult / CompiledOutput / dependency graph API ─────────────────────

/// A project whose `main.mds` imports `lib.mds`; returns the guard, the entry
/// path and the canonical path of `lib.mds`.
fn project_with_one_import() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join(".mdsroot"), "").unwrap();
    std::fs::write(
        dir.path().join("main.mds"),
        "@import \"./lib.mds\" as lib\n{{lib.hi()}}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("lib.mds"), "@define hi():\nHi\n@end\n").unwrap();
    let lib = std::fs::canonicalize(dir.path().join("lib.mds")).unwrap();
    let main = dir.path().join("main.mds");
    (dir, main, lib)
}

/// The `dependencies` of every native compile entry point that reports them.
fn native_dependency_lists(main: &Path) -> Vec<(&'static str, Vec<String>)> {
    let source = std::fs::read_to_string(main).unwrap();
    let base = main.parent();
    vec![
        (
            "compile_with_deps",
            mds::compile_with_deps(main, None).unwrap().dependencies,
        ),
        (
            "compile_with_deps_opts",
            mds::compile_with_deps_opts(main, None, mds::CompileOptions::default())
                .unwrap()
                .dependencies,
        ),
        (
            "compile_str_with_deps",
            mds::compile_str_with_deps(&source, base, None)
                .unwrap()
                .dependencies,
        ),
        (
            "compile_str_with_deps_opts",
            mds::compile_str_with_deps_opts(&source, base, None, mds::CompileOptions::default())
                .unwrap()
                .dependencies,
        ),
    ]
}

/// #409: on Windows, canonicalization yields verbatim `\\?\C:\…` module keys;
/// a native compile reports each dependency in its conventional form instead,
/// still naming the same file.
#[cfg(windows)]
#[test]
fn windows_dependencies_carry_no_verbatim_prefix() {
    let (_guard, main, lib) = project_with_one_import();
    // Positive control (PF-013): the canonical key IS verbatim here, so the
    // absence assertion below can fail.
    assert!(lib.to_str().unwrap().starts_with(r"\\?\"), "{lib:?}");
    for (api, deps) in native_dependency_lists(&main) {
        let [dep] = deps.as_slice() else {
            panic!("{api}: expected one dependency, got {deps:?}");
        };
        assert!(
            !dep.starts_with(r"\\?\"),
            "{api}: a dependency must not carry the verbatim prefix: {dep}"
        );
        assert!(Path::new(dep).is_absolute(), "{api}: {dep}");
        assert_eq!(
            std::fs::canonicalize(dep).unwrap(),
            lib,
            "{api}: the dependency must name the imported file"
        );
    }
}

/// #409 control: off Windows a canonical path has no verbatim form, and each
/// dependency is exactly the canonical path of the imported file.
#[cfg(not(windows))]
#[test]
fn dependencies_are_the_canonical_paths_off_windows() {
    let (_guard, main, lib) = project_with_one_import();
    for (api, deps) in native_dependency_lists(&main) {
        assert_eq!(deps, [lib.to_str().unwrap()], "{api}");
    }
}

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
            "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
        ),
        (
            "main.mds".to_string(),
            "@import \"./dep.mds\"\n{{greet(\"World\")}}\n".to_string(),
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
    let result = mds::compile_str_with_deps("---\nname: World\n---\nHello {{name}}!\n", None, None)
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
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
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
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
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
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
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
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
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
        "Hello {{undefined_var}}!\n".to_string(),
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
    f.write_all(b"@define greet(x):\nHello {{x}}!\n@end\n")
        .unwrap();

    let entry_path = dir.path().join("main.mds");
    let mut f = std::fs::File::create(&entry_path).unwrap();
    f.write_all(b"@import \"./lib.mds\"\n{{greet(\"World\")}}\n")
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
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./defs.mds\" as defs\n@include defs\n{{defs.greet(\"World\")}}\n".to_string(),
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

// ── R2: @include warning precision ───────────────────────────────────────────

/// R2-A: @include of a module with no body text produces WARN-A ("no body text").
#[test]
fn r2_a_include_no_body_text_warns_no_body() {
    let mut modules = std::collections::HashMap::new();
    // Module has a @define but NO top-level body text → prompt_body = None.
    modules.insert(
        "fns.mds".to_string(),
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./fns.mds\" as fns\n@include fns\n".to_string(),
    );
    let result = mds::compile_virtual_collecting_warnings(modules, "main.mds", None)
        .expect("should compile");
    let warn = result.warnings.iter().find(|w| w.contains("empty output"));
    assert!(
        warn.is_some(),
        "R2-A: expected 'empty output' warning; got: {:?}",
        result.warnings
    );
    let w = warn.unwrap();
    assert!(
        w.contains("no body text"),
        "R2-A: WARN-A must say 'no body text'; got: {w}"
    );
    // Must NOT say "does not export" — that is WARN-B.
    assert!(
        !w.contains("does not export"),
        "R2-A: WARN-A must NOT mention 'does not export'; got: {w}"
    );
}

/// R2-B: @include of a module that HAS body text but whose @export list excludes
/// "prompt" produces WARN-B mentioning the exports list.  The "no body text"
/// message must be absent.
#[test]
fn r2_b_include_body_hidden_by_export_warns_export_list() {
    let mut modules = std::collections::HashMap::new();
    // Module has body text AND an explicit @export that does NOT include "prompt".
    modules.insert(
        "lib.mds".to_string(),
        "This is body text.\n@define greet(x):\nHi {{x}}!\n@end\n@export greet\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\" as lib\n@include lib\n".to_string(),
    );
    let result = mds::compile_virtual_collecting_warnings(modules, "main.mds", None)
        .expect("should compile");
    let warn = result.warnings.iter().find(|w| w.contains("empty output"));
    assert!(
        warn.is_some(),
        "R2-B: expected 'empty output' warning; got: {:?}",
        result.warnings
    );
    let w = warn.unwrap();
    // WARN-B must mention the exports list / prompt export.
    assert!(
        w.contains("does not export") || w.contains("@export list"),
        "R2-B: WARN-B must mention exports; got: {w}"
    );
    // WARN-B must NOT say "no body text" — the module has body text.
    assert!(
        !w.contains("no body text"),
        "R2-B: WARN-B must NOT say 'no body text'; got: {w}"
    );
}

/// R2-C: @include of a module that renders normally (exports prompt) produces no warning.
#[test]
fn r2_c_include_exports_prompt_no_warning() {
    let mut modules = std::collections::HashMap::new();
    // Module has body text and does NOT have an explicit @export list → prompt is exported.
    modules.insert("lib.mds".to_string(), "Prompt body here.\n".to_string());
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\" as lib\n@include lib\n".to_string(),
    );
    let result = mds::compile_virtual_collecting_warnings(modules, "main.mds", None)
        .expect("should compile");
    let include_warns: Vec<_> = result
        .warnings
        .iter()
        .filter(|w| w.contains("empty output"))
        .collect();
    assert!(
        include_warns.is_empty(),
        "R2-C: @include of a module with exported prompt must not warn; got: {include_warns:?}"
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
    f.write_all(b"@define greet(x):\nHello {{x}}!\n@end\n")
        .unwrap();

    let source = "@import \"./lib.mds\"\n{{greet(\"World\")}}\n";
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
    modules.insert("main.mds".to_string(), "Hello {{name}}!\n".to_string());
    let output = mds::compile_virtual(modules, "main.mds", Some(vars))
        .unwrap()
        .into_markdown()
        .unwrap();
    assert_eq!(output, "Hello Test!\n");
}

// ── Non-UTF-8 path rejection ──────────────────────────────────────────────────
//
// `#[cfg(unix)]` on all three: each constructs the hostile path via
// `OsStrExt::from_bytes` (arbitrary bytes), a Unix-only API; Windows paths are
// UTF-16 and have no equivalent construction from arbitrary bytes (#147).

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
    std::fs::write(&helper, "@define greet(name):\nHello {{name}}!\n@end\n").unwrap();

    let entry = dir.path().join("chat.mds");
    std::fs::write(
        &entry,
        "@import { greet } from \"./helper.mds\"\n\
         @message user:\n{{greet(\"World\")}}\n@end\n",
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

/// Creates a symlink for a test, tolerating Windows' unprivileged restriction.
///
/// Mirrors `crates/mds-core/src/fs.rs`'s unit-test helper of the same name and
/// contract (#147); duplicated rather than shared because that helper is
/// private to `fs.rs`'s own `mod tests` and this file is a separate
/// integration-test binary.
fn make_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let result = if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };

    match result {
        Ok(()) => true,
        Err(err) => {
            #[cfg(windows)]
            {
                const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
                if err.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
                    && std::env::var_os("CI").is_none()
                {
                    eprintln!(
                        "skipping: symlink creation needs Developer Mode or an elevated process on Windows"
                    );
                    return false;
                }
            }
            panic!(
                "failed to create symlink {} -> {}: {err}",
                target.display(),
                link.display()
            );
        }
    }
}

#[test]
fn compile_rejects_symlinked_entry_for_messages_template() {
    // Symlinked entry rejection applies regardless of output shape.
    let dir = tempfile::tempdir().unwrap();
    let real_file = dir.path().join("real.mds");
    std::fs::write(&real_file, "@message system:\nYou are helpful.\n@end\n").unwrap();
    let link_file = dir.path().join("linked.mds");
    if !make_symlink(&real_file, &link_file) {
        return;
    }

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

// ── #162: MAX_FILE_SIZE backstop at the string funnels ────────────────────────

fn cwd_str() -> String {
    std::env::current_dir()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

/// Every string-source funnel that runs a resolve pass rejects a source over
/// MAX_FILE_SIZE with a resource limit — the binding-layer size guards are bypassed by
/// these core entry points (PF-004), so the check must live in core.
#[track_caller]
fn assert_oversize_rejected(name: &str, r: Result<(), MdsError>) {
    assert!(
        matches!(r, Err(MdsError::ResourceLimit { .. })),
        "{name} must reject an oversize source with a resource limit, got {r:?}"
    );
    let msg = r.unwrap_err().to_string();
    assert!(
        msg.contains("too large"),
        "{name} rejection must mention the size limit, got: {msg}"
    );
}

#[test]
fn string_funnels_reject_oversize_source() {
    let over = " ".repeat((MAX_FILE_SIZE + 1) as usize);

    assert_oversize_rejected("compile_str", mds::compile_str(&over).map(|_| ()));
    assert_oversize_rejected("check_str", mds::check_str(&over));
    assert_oversize_rejected(
        "lint_str_with",
        mds::lint_str_with(&over, None, None, &LintConfig::default()).map(|_| ()),
    );
    assert_oversize_rejected(
        "compile_str_with_deps_opts",
        mds::compile_str_with_deps_opts(&over, None, None, mds::CompileOptions::default())
            .map(|_| ()),
    );

    let mut cache = ModuleCache::new();
    let mut warnings = vec![];
    assert_oversize_rejected(
        "ModuleCache::resolve_source",
        cache
            .resolve_source(&over, &cwd_str(), &HashMap::new(), &mut warnings)
            .map(|_| ()),
    );
}

/// PF-013 at-cap Ok twin: a source of EXACTLY MAX_FILE_SIZE bytes is accepted.
#[test]
fn string_funnel_accepts_source_at_cap() {
    let at = " ".repeat(MAX_FILE_SIZE as usize);
    let r = mds::check_str(&at);
    assert!(
        r.is_ok(),
        "a source at exactly MAX_FILE_SIZE must be accepted: {r:?}"
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
    let (config, _) = LintConfig::from_rules_checked(HashMap::from([(
        "unused-variable".to_string(),
        Severity::Off,
    )]));
    assert_eq!(config.rules.get("unused-variable"), Some(&Severity::Off));

    // LintDiagnostic has the expected fields.
    let diag = LintDiagnostic::new(
        "unused-variable",
        Severity::Warn,
        "Variable 'name' is never used",
    )
    .with_help("Remove the frontmatter key or reference it in the body")
    .with_file("test.mds")
    .with_span(mds::SerializedSpan::new(0, 4));
    assert_eq!(diag.rule, "unused-variable");
    assert_eq!(diag.severity, Severity::Warn);
    // Verify that the builder methods actually set their fields.
    assert_eq!(
        diag.help.as_deref(),
        Some("Remove the frontmatter key or reference it in the body"),
        "with_help must set the help field"
    );
    assert_eq!(
        diag.file.as_deref(),
        Some("test.mds"),
        "with_file must set the file field"
    );
    assert!(diag.span.is_some(), "with_span must set the span field");

    // LintResult has diagnostics, truncated, and is_standalone fields.
    let result = LintResult::new(vec![diag]);
    assert_eq!(result.diagnostics.len(), 1);
    assert!(!result.truncated);
}

/// AC-224-7 / AC-224-8: KNOWN_LINT_RULES and UnknownRuleNames honour ADR-010.
///
/// - `KNOWN_LINT_RULES` is publicly reachable from an external crate.
/// - It contains exactly the ten registered rule names.
/// - Every entry in the registry is accepted by `LintConfig::from_rules_checked` without
///   unknowns.
/// - `find_unknown_rule_names` returns `None` for all-known maps and `Some` for
///   maps containing unknown names.
/// - `UnknownRuleNames` exposes names via accessor, not a public field, and is
///   only constructible through the library.
#[test]
fn known_lint_rules_and_unknown_detection() {
    use mds::{find_unknown_rule_names, KNOWN_LINT_RULES};

    // AC-224-7: exactly 10 rules.
    assert_eq!(
        KNOWN_LINT_RULES.len(),
        10,
        "KNOWN_LINT_RULES must have exactly 10 entries"
    );

    // AC-224-7: exact sorted contents.
    let expected = [
        "duplicate-export",
        "duplicate-import",
        "empty-block",
        "legacy-interpolation",
        "redundant-else",
        "shadow-variable",
        "unreachable-branch",
        "unused-function",
        "unused-import",
        "unused-variable",
    ];
    assert_eq!(
        KNOWN_LINT_RULES, &expected,
        "KNOWN_LINT_RULES must match the expected sorted list"
    );

    // AC-224-8: every known rule is accepted by from_rules_checked without unknowns.
    let all_known: HashMap<String, Severity> = KNOWN_LINT_RULES
        .iter()
        .map(|&n| (n.to_string(), Severity::Warn))
        .collect();
    let (_config, _u) = LintConfig::from_rules_checked(all_known.clone());
    assert!(
        find_unknown_rule_names(&all_known).is_none(),
        "all-known rules map must produce no unknowns"
    );

    // AC-224-8: empty rules map produces no unknowns.
    let empty: HashMap<String, Severity> = HashMap::new();
    assert!(
        find_unknown_rule_names(&empty).is_none(),
        "empty rules map must produce no unknowns"
    );

    // AC-224-7: UnknownRuleNames is only obtainable via the library API.
    let mixed: HashMap<String, Severity> = HashMap::from([
        ("unused-variable".to_string(), Severity::Off),
        ("no-such-rule".to_string(), Severity::Warn),
        ("another-bad".to_string(), Severity::Error),
    ]);
    let unknown = find_unknown_rule_names(&mixed).expect("should detect two unknown rules");
    // Accessor returns names; struct literal construction is impossible (#[non_exhaustive]).
    let names = unknown.names();
    assert_eq!(
        names,
        &["another-bad".to_string(), "no-such-rule".to_string()],
        "names must be sorted lexicographically"
    );
    assert_eq!(names.len(), 2);

    // Positive control (PF-013 / ADR-009): find_unknown_rule_names does NOT return None
    // for a single-unknown map.
    let one_bad: HashMap<String, Severity> =
        HashMap::from([("no-such-rule".to_string(), Severity::Warn)]);
    assert!(
        find_unknown_rule_names(&one_bad).is_some(),
        "single-unknown map must produce Some"
    );
}

/// Review finding (config.rs:104) — `from_rules_checked` is the structurally-safe
/// construction path.
///
/// - An all-known map returns `(config, None)`.
/// - A map with unknowns returns `(config, Some(UnknownRuleNames))`.
/// - Both arms return a usable `LintConfig` (lint always continues).
/// - `from_rules_checked` is `#[must_use]`: the compiler warns if the caller
///   discards the return value entirely, making it structurally harder to miss
///   the detection step.
#[test]
fn from_rules_checked_structurally_returns_unknowns() {
    use mds::{LintConfig, KNOWN_LINT_RULES};

    // All-known map: config is usable, unknowns is None.
    let all_known: HashMap<String, Severity> = KNOWN_LINT_RULES
        .iter()
        .map(|&n| (n.to_string(), Severity::Warn))
        .collect();
    let (config, unknown) = LintConfig::from_rules_checked(all_known);
    assert!(
        unknown.is_none(),
        "all-known map must return None for unknowns"
    );
    assert_eq!(
        config.severity_for("unused-variable"),
        Some(&Severity::Warn),
        "config must still be usable after from_rules_checked"
    );

    // Empty map: config is usable, unknowns is None.
    let (empty_config, empty_unknown) = LintConfig::from_rules_checked(HashMap::new());
    assert!(
        empty_unknown.is_none(),
        "empty map must return None for unknowns"
    );
    assert!(
        empty_config.severity_for("unused-variable").is_none(),
        "empty config must have no overrides"
    );

    // Map with unknown names: config still loads, unknowns is Some.
    let mixed: HashMap<String, Severity> = HashMap::from([
        ("unused-variable".to_string(), Severity::Off),
        ("no-such-rule".to_string(), Severity::Warn),
        ("another-bad".to_string(), Severity::Error),
    ]);
    let (config2, unknown2) = LintConfig::from_rules_checked(mixed);
    let u = unknown2.expect("from_rules_checked must detect two unknown rules");
    // Accessor returns sorted names.
    assert_eq!(
        u.names(),
        &["another-bad".to_string(), "no-such-rule".to_string()],
        "names must be sorted lexicographically"
    );
    // The config still loads — the unknown rules have no effect but the valid one does.
    assert_eq!(
        config2.severity_for("unused-variable"),
        Some(&Severity::Off),
        "valid rule must still be present in config after detection"
    );

    // Positive control (PF-013): a single-unknown map produces Some, not None.
    let (_, one_bad_unknown) =
        LintConfig::from_rules_checked(HashMap::from([("bad-rule".to_string(), Severity::Warn)]));
    assert!(
        one_bad_unknown.is_some(),
        "single-unknown map must produce Some from from_rules_checked"
    );
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

    let result = LintResult::new(vec![LintDiagnostic::new(
        "unused-variable",
        Severity::Warn,
        "Variable 'name' is never used",
    )
    .with_help("Remove the frontmatter key or reference it in the body")
    .with_span(SerializedSpan::new(4, 4).with_line(2).with_column(1))
    .with_file("test.mds")]);

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

    // Tier A rule (duplicate-import) with fix_removals → fixable regardless of is_standalone.
    let tier_a = LintResult::new(vec![LintDiagnostic::new(
        "duplicate-import",
        Severity::Error,
        "Duplicate import",
    )
    .with_file("a.mds")
    .with_fix_removals(vec![FixLineSpan::single(0)])]); // even non-standalone Tier A is fixable
    let json = tier_a.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], true);

    // Tier B rule (unused-function) — fixable only for standalone files.
    // fix_removals: Some(...) + non-standalone → fixable: false
    let tier_b_non_standalone = LintResult::new(vec![LintDiagnostic::new(
        "unused-function",
        Severity::Warn,
        "Unused function",
    )
    .with_file("b.mds")
    .with_fix_removals(vec![FixLineSpan::single(0)])]);
    let json = tier_b_non_standalone.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], false);

    // fix_removals: Some(...) + standalone → fixable: true
    let tier_b_standalone = LintResult::new(vec![LintDiagnostic::new(
        "unused-function",
        Severity::Warn,
        "Unused function",
    )
    .with_file("c.mds")
    .with_fix_removals(vec![FixLineSpan::single(0)])])
    .standalone();
    let json = tier_b_standalone.to_canonical_json();
    assert_eq!(json["files"][0]["diagnostics"][0]["fixable"], true);

    // Tier C rule (unused-variable) → never fixable (fix_removals: None also → false).
    let tier_c = LintResult::new(vec![LintDiagnostic::new(
        "unused-variable",
        Severity::Warn,
        "Unused variable",
    )
    .with_file("d.mds")])
    .standalone(); // even standalone Tier C is not fixable
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
    let off = mds::CompileOptions::default();
    assert!(!off.source_map);
    assert!(!off.include_sources_content);

    let on = mds::CompileOptions::default()
        .with_source_map(true)
        .with_include_sources_content(true);
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
        mds::CompileOptions::default().with_source_map(true),
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
        mds::CompileOptions::default()
            .with_source_map(true)
            .with_include_sources_content(true),
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
    let edit = ByteEdit::deletion(0, 5, "duplicate-import");
    let rejected = RejectedEdit::new(edit, "simulated reverify failure");
    assert_eq!(rejected.reason, "simulated reverify failure");
    assert_eq!(rejected.edit.rule, "duplicate-import");

    // apply_fixes_incremental is callable with F: Fn — compile-time and runtime check.
    let source = "Hello!\n";
    let original = LintResult::new(vec![]);
    let plan = plan_fixes(&original, source);
    let outcome = apply_fixes_incremental(
        source,
        plan,
        &original,
        |_s| -> Result<LintResult, MdsError> { Ok(LintResult::new(vec![])) },
    );
    // Empty source with no diagnostics → NothingToFix (no reverify called).
    assert!(
        matches!(outcome, FixOutcome::NothingToFix),
        "trivial source with no diagnostics must return NothingToFix; got: {outcome:?}"
    );
}

/// F-API-3: `apply_fixes` remains reachable on the public API surface while deprecated.
/// This test pins the function signature and the empty-plan early-return path (the
/// reverify closure is never invoked when `plan.edits.is_empty()`). Remove at v0.5.0
/// with the function (AD-209-1).
///
/// AD-209-2: `#[expect(deprecated)]` was chosen over a `trybuild` compile-fail fixture
/// because: (a) trybuild only asserts that the deprecation warning fires; it does not
/// verify the function's signature or return value; (b) this test asserts the runtime
/// behavior (NothingToFix for an empty plan -- the `plan.edits.is_empty()` early-return),
/// giving a stronger pin than a compile-fail fixture alone; and
/// (c) `#[expect(deprecated)]` fires `unfulfilled_lint_expectations` when the
/// `#[deprecated]` attribute is removed from `apply_fixes`. The mutation control
/// (applies ADR-009): removing the attribute leaves the lib rlib compiling clean, so
/// both the lib-test (fix.rs `#[cfg(test)]`) and integration-test (api_surface) targets
/// are affected. A single command is insufficient: `cargo clippy --workspace --all-targets
/// -- -D warnings` emits 10 errors (all in fix.rs) and then cargo aborts compilation of
/// the lib-test target; the integration-test (`api_surface`) target is never reached in
/// that invocation. Run `cargo clippy --workspace --all-targets -- -D warnings` to verify.
/// Total: exactly 10 unfulfilled_lint_expectations, all in fix.rs — one per deprecated
/// `apply_fixes` call. All expectations are distinct; none is over-broad.
///
/// All values constructed via named constructors, never struct literals (applies ADR-010).
#[expect(
    deprecated,
    reason = "AD-209-2: F-API-3 pins the deprecated apply_fixes public API surface; see fix.rs rustdoc"
)]
#[test]
fn fix_api_apply_fixes_exists() {
    use mds::fix::{apply_fixes, plan_fixes, FixOutcome};

    // Construct via named constructors; never struct literals (applies ADR-010).
    let source = "Hello!\n";
    let original = LintResult::new(vec![]);
    let plan = plan_fixes(&original, source);
    // The closure moves out of a captured `String`, so it implements `FnOnce` but
    // NOT `Fn`/`FnMut`. That makes this a real compile-time pin on the `F: FnOnce`
    // bound: tightening `apply_fixes` to `F: Fn` (the `apply_fixes_incremental`
    // bound) would break this test's compilation rather than pass silently.
    let move_once = String::from("consumed-by-value");
    let outcome = apply_fixes(
        source,
        plan,
        &original,
        move |_s| -> Result<LintResult, MdsError> {
            drop(move_once);
            Ok(LintResult::new(vec![]))
        },
    );
    // Empty source with no diagnostics must return NothingToFix (no reverify called).
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

/// AC-P1-27 positive control (PF-013 / ADR-009): the core library still uses
/// `STRING_SOURCE_MAP_LABEL` ("input.mds") as the `file` key for string-source
/// lint results.
///
/// This test confirms that the CLI's output-boundary relabel (`set_diag_display_path`
/// in `mds-cli/src/lint.rs`) is doing real work and is not dead code — without the
/// relabel, `"input.mds"` would appear in the `files[].file` key of the CLI JSON
/// output.  The 113f472 baseline would fail AC-P1-01 on this exact property: the
/// core has always used `STRING_SOURCE_MAP_LABEL` and the PR's relabel is the only
/// thing that makes the CLI emit `"<stdin>"` instead.
#[test]
fn lint_str_uses_string_source_map_label_as_file_key() {
    // Source that triggers `duplicate-export` — no imports needed, so lint_str
    // succeeds without a filesystem base directory.
    let source = "@define greet(name):\n  Hello {{name}}!\n@end\n\n@export greet\n@export greet\n";
    let result = mds::lint_str(source).expect("lint_str must succeed for this source");
    // AC-P1-27: every diagnostic must carry STRING_SOURCE_MAP_LABEL as the file key.
    // The CLI relabels this at the output boundary; the core must not.
    let all_files: Vec<_> = result
        .diagnostics
        .iter()
        .filter_map(|d| d.file.as_deref())
        .collect();
    assert!(
        !all_files.is_empty(),
        "AC-P1-27: duplicate-export must fire and produce at least one diagnostic"
    );
    for file_key in &all_files {
        assert_eq!(
            *file_key,
            mds::STRING_SOURCE_MAP_LABEL,
            "AC-P1-27: core must use STRING_SOURCE_MAP_LABEL ('input.mds') as the \
             file key for string-source lint — got '{file_key}' instead; \
             the CLI relabel happens at the output boundary, not in core"
        );
    }
}

/// F-API-2: TextEdit is publicly nameable and LintDiagnostic::with_fix_edits wires through.
///
/// Pins:
/// - `mds::TextEdit` is a public type (was previously unnameable: pub inside pub(crate) mod).
/// - `LintDiagnostic::with_fix_edits` stores and exposes the edits.
/// - `LintResult::to_canonical_json()` emits the `fix_edits` array for a diagnostic that has them.
#[test]
fn text_edit_and_fix_edits_public_api() {
    use mds::TextEdit;

    // TextEdit is publicly constructable.
    let edit = TextEdit::new(6, 12, "{{name}}");
    assert_eq!(edit.start, 6);
    assert_eq!(edit.end, 12);
    assert_eq!(edit.new_text, "{{name}}");

    // with_fix_edits stores the edits on LintDiagnostic.
    let diag = LintDiagnostic::new(
        "legacy-interpolation",
        Severity::Warn,
        "legacy brace syntax",
    )
    .with_file("t.mds")
    .with_fix_edits(vec![edit]);
    assert!(
        diag.fix_edits.is_some(),
        "with_fix_edits must populate fix_edits"
    );
    let edits = diag.fix_edits.as_ref().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].start, 6);
    assert_eq!(edits[0].new_text, "{{name}}");

    // to_canonical_json emits fix_edits as an array (not null) for this diagnostic.
    let result = LintResult::new(vec![diag]);
    let json = result.to_canonical_json();
    let diags = &json["files"][0]["diagnostics"];
    let fix_edits = &diags[0]["fix_edits"];
    assert!(
        fix_edits.is_array(),
        "fix_edits must be a JSON array when present; got: {fix_edits:?}"
    );
    let arr = fix_edits.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["start"], 6);
    assert_eq!(arr[0]["end"], 12);
    assert_eq!(arr[0]["new_text"], "{{name}}");
}
