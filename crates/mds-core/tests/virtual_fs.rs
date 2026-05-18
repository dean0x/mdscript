//! End-to-end integration tests for the VirtualFs-backed compilation pipeline.
//!
//! These tests verify that the full MDS compiler pipeline works correctly
//! against an in-memory filesystem, without any OS filesystem access.

use std::collections::HashMap;

use mds::{MdsError, ModuleCache, Value};

// ── Helper ────────────────────────────────────────────────────────────────────

/// Compile a virtual module set and return the output string.
///
/// Creates a `ModuleCache::virtual_fs` from `modules`, resolves `entry` by key,
/// and returns the rendered output (prompt body with frontmatter prepended).
fn compile_vfs(modules: HashMap<String, String>, entry: &str) -> Result<String, MdsError> {
    mds::compile_virtual(modules, entry, None)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn single_file_compile() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hello World!"), "got: {output}");
}

#[test]
fn two_file_import_merge() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\"\n{greet(\"Alice\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hello Alice!"), "got: {output}");
}

#[test]
fn three_file_chain() {
    let mut modules = HashMap::new();
    modules.insert(
        "c.mds".to_string(),
        "@define shout(x):\n{x}!!!\n@end\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        "@import \"./c.mds\"\n@define greet(x):\n{shout(x)}\n@end\n".to_string(),
    );
    modules.insert(
        "a.mds".to_string(),
        "@import \"./b.mds\"\n{greet(\"World\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "a.mds").expect("should compile");
    assert!(output.contains("World!!!"), "got: {output}");
}

#[test]
fn circular_import_error() {
    let mut modules = HashMap::new();
    modules.insert(
        "a.mds".to_string(),
        "@import \"./b.mds\"\nHello!\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        "@import \"./a.mds\"\nWorld!\n".to_string(),
    );
    let err = compile_vfs(modules, "a.mds").expect_err("should fail with circular import");
    assert!(
        matches!(err, MdsError::CircularImport { .. }),
        "expected CircularImport, got {err:?}"
    );
}

#[test]
fn deep_chain_exceeds_max_depth() {
    // Build a chain of 65 files: main → 1 → 2 → ... → 64 → end
    // This exceeds MAX_IMPORT_DEPTH (64), so resolution should fail.
    let mut modules = HashMap::new();
    // The end of the chain (file 64) has no imports.
    modules.insert(
        "file64.mds".to_string(),
        "@define base():\nbase\n@end\n".to_string(),
    );
    // Build intermediate files 63 → 1.
    for i in (1..64).rev() {
        let content = format!("@import \"./file{}.mds\"\n", i + 1);
        modules.insert(format!("file{i}.mds"), content);
    }
    // The entry file imports file1.
    modules.insert(
        "main.mds".to_string(),
        "@import \"./file1.mds\"\nHello\n".to_string(),
    );

    let err = compile_vfs(modules, "main.mds").expect_err("should fail: import depth exceeded");
    let msg = err.to_string();
    assert!(
        msg.contains("import depth") || msg.contains("maximum"),
        "expected depth error, got: {msg}"
    );
}

#[test]
fn selective_import() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n@define farewell(x):\nBye {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import { greet } from \"./lib.mds\"\n{greet(\"Bob\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hi Bob!"), "got: {output}");
}

#[test]
fn selective_import_excludes_non_imported() {
    // selective_import only imports greet from lib.mds; farewell should not be
    // available in main.mds. Without this negative assertion the positive test
    // passes even if selective import filtering is a no-op.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n@define farewell(x):\nBye {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import { greet } from \"./lib.mds\"\n{farewell(\"Bob\")}\n".to_string(),
    );
    let err =
        compile_vfs(modules, "main.mds").expect_err("calling a non-imported function should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("farewell")
            || msg.contains("not defined")
            || msg.contains("not found")
            || msg.contains("undefined")
            || msg.contains("unknown"),
        "expected an error mentioning the missing symbol, got: {msg}"
    );
}

#[test]
fn export_visibility_exported_function_accessible() {
    // greet is exported — it must be callable from the importer.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define internal():\nhidden\n@end\n@define greet(x):\nHi {x}!\n@end\n@export greet\n"
            .to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\"\n{greet(\"Carol\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hi Carol!"), "got: {output}");
}

#[test]
fn export_visibility_non_exported_function_inaccessible() {
    // internal is NOT exported — calling it from an importer must fail.
    // This verifies that @export has a real filtering effect: without it
    // the test would pass trivially even if @export were a no-op.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define internal():\nhidden\n@end\n@define greet(x):\nHi {x}!\n@end\n@export greet\n"
            .to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        // Attempt to call the non-exported function.
        "@import \"./lib.mds\"\n{internal()}\n".to_string(),
    );
    let err =
        compile_vfs(modules, "main.mds").expect_err("calling a non-exported function should fail");
    // The error must indicate the symbol is unknown/not found, not a compile
    // success that silently produces wrong output.
    let msg = err.to_string();
    assert!(
        msg.contains("internal")
            || msg.contains("not defined")
            || msg.contains("not found")
            || msg.contains("undefined")
            || msg.contains("unknown"),
        "expected an error mentioning the missing symbol, got: {msg}"
    );
}

#[test]
fn namespace_import() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\" as lib\n{lib.greet(\"Dave\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hello Dave!"), "got: {output}");
}

#[test]
fn wildcard_reexport() {
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "barrel.mds".to_string(),
        "@export * from \"./base.mds\"\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./barrel.mds\"\n{greet(\"Eve\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hi Eve!"), "got: {output}");
}

#[test]
fn module_not_found() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "@import \"./missing.mds\"\nHello!\n".to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("should fail: file not found");
    assert!(
        matches!(
            err,
            MdsError::FileNotFound { .. } | MdsError::ImportError { .. }
        ),
        "expected FileNotFound or ImportError, got {err:?}"
    );
}

#[test]
fn cross_subdirectory_import() {
    // pages/main.mds imports ../shared/utils.mds using a relative path that
    // crosses a subdirectory boundary. This exercises VirtualFs::normalize for
    // the ".." traversal case across the full compile pipeline.
    let mut modules = HashMap::new();
    modules.insert(
        "shared/utils.mds".to_string(),
        "@define format_name(x):\n[{x}]\n@end\n".to_string(),
    );
    modules.insert(
        "pages/main.mds".to_string(),
        "@import \"../shared/utils.mds\"\n{format_name(\"Alice\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "pages/main.mds").expect("should compile");
    assert!(output.contains("[Alice]"), "got: {output}");
}

#[test]
fn resolve_key_directly() {
    // Test that ModuleCache::resolve_key works for VirtualFs.
    let mut modules = HashMap::new();
    modules.insert(
        "greeting.mds".to_string(),
        "---\nwho: World\n---\nHello {who}!\n".to_string(),
    );
    let mut cache = ModuleCache::virtual_fs(modules);
    let mut warnings = vec![];
    let resolved = cache
        .resolve_key("greeting.mds", &HashMap::new(), &mut warnings)
        .expect("should resolve");
    // Use get_prompt_value() since prompt_body is pub(crate).
    let body = match resolved.get_prompt_value() {
        Some(Value::String(ref s)) => s.clone(),
        other => panic!("expected Value::String, got: {other:?}"),
    };
    assert!(body.contains("Hello World!"), "got body: {body:?}");
}

// ── Dependency tracking tests ─────────────────────────────────────────────────

#[test]
fn deps_single_file() {
    // A single entry file with no imports → deps = []
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
fn deps_two_files() {
    // main imports lib → deps = ["lib.mds"]
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\"\n{greet(\"Alice\")}\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("should compile");
    assert!(result.output.contains("Hello Alice!"), "got: {}", result.output);
    assert_eq!(result.dependencies, vec!["lib.mds".to_string()]);
}

#[test]
fn deps_three_file_chain() {
    // a → b → c: deps of a = ["c.mds", "b.mds"] in post-order DFS (leaves first)
    let mut modules = HashMap::new();
    modules.insert(
        "c.mds".to_string(),
        "@define shout(x):\n{x}!!!\n@end\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        "@import \"./c.mds\"\n@define greet(x):\n{shout(x)}\n@end\n".to_string(),
    );
    modules.insert(
        "a.mds".to_string(),
        "@import \"./b.mds\"\n{greet(\"World\")}\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "a.mds", None).expect("should compile");
    assert!(result.output.contains("World!!!"), "got: {}", result.output);
    // Resolution is post-order DFS: c is inserted first (leaf), then b, then a (entry, excluded).
    assert_eq!(result.dependencies, vec!["c.mds".to_string(), "b.mds".to_string()]);
}

#[test]
fn deps_diamond_no_duplicates() {
    // Diamond: main→a,b; a,b→shared → shared must appear exactly once.
    // Post-order DFS: when resolving main, it imports a first → a imports shared →
    // shared is inserted (leaf), then a is inserted. Then main imports b → b imports
    // shared (cache hit, not re-inserted), then b is inserted.
    // Final deps (excluding main): ["shared.mds", "a.mds", "b.mds"]
    let mut modules = HashMap::new();
    modules.insert(
        "shared.mds".to_string(),
        "@define tag(x):\n[{x}]\n@end\n".to_string(),
    );
    // a and b each import shared; their output is just text (no calls to tag)
    modules.insert(
        "a.mds".to_string(),
        "@import \"./shared.mds\"\nfrom-a\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        "@import \"./shared.mds\"\nfrom-b\n".to_string(),
    );
    // main imports a and b (which transitively pull in shared); uses a literal body
    modules.insert(
        "main.mds".to_string(),
        "@import \"./a.mds\"\n@import \"./b.mds\"\nhello\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("should compile");
    assert!(result.output.contains("hello"), "got: {}", result.output);
    // shared must appear exactly once
    let shared_count = result.dependencies.iter().filter(|d| *d == "shared.mds").count();
    assert_eq!(shared_count, 1, "shared appeared {shared_count} times: {:?}", result.dependencies);
    // All three deps present
    assert!(result.dependencies.contains(&"a.mds".to_string()), "missing a.mds: {:?}", result.dependencies);
    assert!(result.dependencies.contains(&"b.mds".to_string()), "missing b.mds: {:?}", result.dependencies);
    assert!(result.dependencies.contains(&"shared.mds".to_string()), "missing shared.mds: {:?}", result.dependencies);
    // 3 deps total, no duplicates
    assert_eq!(result.dependencies.len(), 3, "expected 3 deps, got: {:?}", result.dependencies);
}

#[test]
fn deps_str_with_deps_basic() {
    // compile_str_with_deps: inline source that imports a virtual module.
    // Use a base_dir so the import resolution works; but with NativeFs that
    // would look for real files. Skip this variant here — covered in api_surface.rs.
    // Instead test the no-import case:
    let result = mds::compile_str_with_deps(
        "---\nname: Alice\n---\nHi {name}!\n",
        None,
        None,
    ).expect("should compile");
    assert!(result.output.contains("Hi Alice!"), "got: {}", result.output);
    // No imports → no deps
    assert_eq!(result.dependencies, Vec::<String>::new());
}

#[test]
fn deps_str_with_deps_file_import() {
    // compile_str_with_deps resolves @import relative to base_dir on disk.
    use std::io::Write;

    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("lib.mds");
    let mut f = std::fs::File::create(&lib_path).unwrap();
    f.write_all(b"@define greet(x):\nHello {x}!\n@end\n").unwrap();

    let source = "@import \"./lib.mds\"\n{greet(\"World\")}\n";
    let result = mds::compile_str_with_deps(source, Some(dir.path()), None)
        .expect("should compile with file import");

    assert!(
        result.output.contains("Hello World!"),
        "expected rendered output, got: {}",
        result.output
    );
    // The lib file is an imported dependency; source string is not a file, so
    // only the imported lib appears in dependencies.
    assert_eq!(result.dependencies.len(), 1, "expected 1 dep, got: {:?}", result.dependencies);
    let dep = &result.dependencies[0];
    assert!(
        dep.ends_with("lib.mds"),
        "expected dep ending in lib.mds, got: {dep}"
    );
}

#[test]
fn deps_error_returns_err() {
    // Undefined variable → Err, no partial deps
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "Hello {undefined_var}!\n".to_string(),
    );
    let err = mds::compile_virtual_with_deps(modules, "main.mds", None)
        .expect_err("should fail with undefined variable");
    assert!(
        matches!(err, MdsError::UndefinedVariable { .. }),
        "expected UndefinedVariable, got: {err:?}"
    );
}
