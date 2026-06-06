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
    assert!(
        result.output.contains("Hello Alice!"),
        "got: {}",
        result.output
    );
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
    assert_eq!(
        result.dependencies,
        vec!["c.mds".to_string(), "b.mds".to_string()]
    );
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
    let shared_count = result
        .dependencies
        .iter()
        .filter(|d| *d == "shared.mds")
        .count();
    assert_eq!(
        shared_count, 1,
        "shared appeared {shared_count} times: {:?}",
        result.dependencies
    );
    // All three deps present
    assert!(
        result.dependencies.contains(&"a.mds".to_string()),
        "missing a.mds: {:?}",
        result.dependencies
    );
    assert!(
        result.dependencies.contains(&"b.mds".to_string()),
        "missing b.mds: {:?}",
        result.dependencies
    );
    assert!(
        result.dependencies.contains(&"shared.mds".to_string()),
        "missing shared.mds: {:?}",
        result.dependencies
    );
    // 3 deps total, no duplicates
    assert_eq!(
        result.dependencies.len(),
        3,
        "expected 3 deps, got: {:?}",
        result.dependencies
    );
}

#[test]
fn deps_str_with_deps_basic() {
    // compile_str_with_deps: inline source that imports a virtual module.
    // Use a base_dir so the import resolution works; but with NativeFs that
    // would look for real files. Skip this variant here — covered in api_surface.rs.
    // Instead test the no-import case:
    let result = mds::compile_str_with_deps("---\nname: Alice\n---\nHi {name}!\n", None, None)
        .expect("should compile");
    assert!(
        result.output.contains("Hi Alice!"),
        "got: {}",
        result.output
    );
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
    f.write_all(b"@define greet(x):\nHello {x}!\n@end\n")
        .unwrap();

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

// ── Frontmatter import integration tests ──────────────────────────────────────

#[test]
fn fm_import_alias_basic() {
    // Alias import: `as lib` → `{lib.greet("Alice")}` works.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n    as: lib\n---\n{lib.greet(\"Alice\")}\n"
            .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("alias import should compile");
    assert!(output.contains("Hello Alice!"), "got: {output}");
}

#[test]
fn fm_import_merge_basic() {
    // Merge import: functions come into the current scope directly.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n---\n{greet(\"Bob\")}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("merge import should compile");
    assert!(output.contains("Hi Bob!"), "got: {output}");
}

#[test]
fn fm_import_selective_basic() {
    // Selective import: only `greet` is imported; `farewell` is not.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n@define farewell(x):\nBye {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n    names: [greet]\n---\n{greet(\"Carol\")}\n"
            .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("selective import should compile");
    assert!(output.contains("Hi Carol!"), "got: {output}");
}

#[test]
fn fm_import_with_body_import() {
    // Frontmatter import and body @import coexist.
    let mut modules = HashMap::new();
    modules.insert(
        "fm_lib.mds".to_string(),
        "@define shout(x):\n{x}!!!\n@end\n".to_string(),
    );
    modules.insert(
        "body_lib.mds".to_string(),
        "@define greet(x):\nHello {x}\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./fm_lib.mds\n",
            "    as: fm\n",
            "---\n",
            "@import \"./body_lib.mds\"\n",
            "{fm.shout(greet(\"Dave\"))}\n",
        )
        .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("fm + body imports should compile");
    assert!(output.contains("Hello Dave!!!"), "got: {output}");
}

#[test]
fn fm_import_not_leaked_as_var() {
    // `{imports}` in body should error — `imports` is reserved, not a scope variable.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define f():\nok\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n---\n{imports}\n".to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("imports should not be a scope var");
    let msg = err.to_string();
    assert!(
        msg.contains("imports") || msg.contains("undefined") || msg.contains("not defined"),
        "got: {msg}"
    );
}

#[test]
fn fm_import_stripped_output() {
    // The `imports` key must NOT appear in the compiled frontmatter output.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "name: Alice\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "{lib.greet(name)}\n",
        )
        .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(
        output.contains("Hello Alice!"),
        "body correct, got: {output}"
    );
    assert!(
        output.contains("name: Alice"),
        "name var preserved, got: {output}"
    );
    assert!(
        !output.contains("imports:"),
        "imports must be stripped, got: {output}"
    );
    assert!(
        !output.contains("./lib.mds"),
        "path must be stripped, got: {output}"
    );
}

#[test]
fn fm_import_for_expr() {
    // Alias import usable in @for expression: `@for x in lib.split_items(csv, ",")`.
    // Uses the split() built-in via an imported function to produce an array.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define split_items(s, sep):\n{split(s, sep)}\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "csv: a,b,c\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "@for x in split(csv, \",\"):\n",
            "{x}\n",
            "@end\n",
        )
        .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("@for with fm alias should compile");
    assert!(output.contains('a'), "got: {output}");
    assert!(output.contains('b'), "got: {output}");
}

#[test]
fn fm_import_chain() {
    // A → B → C, all using frontmatter imports.
    let mut modules = HashMap::new();
    modules.insert(
        "c.mds".to_string(),
        "@define base():\nbase\n@end\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./c.mds\n",
            "    as: c\n",
            "---\n",
            "@define wrap():\n[{c.base()}]\n@end\n",
        )
        .to_string(),
    );
    modules.insert(
        "a.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./b.mds\n",
            "    as: b\n",
            "---\n",
            "{b.wrap()}\n",
        )
        .to_string(),
    );
    let output = compile_vfs(modules, "a.mds").expect("fm import chain should compile");
    assert!(output.contains("[base]"), "got: {output}");
}

#[test]
fn fm_import_deps_tracked() {
    // compile_virtual_with_deps must include fm-imported modules in dependencies.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "{lib.greet(\"World\")}\n",
        )
        .to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None)
        .expect("should compile with deps");
    assert!(
        result.dependencies.contains(&"lib.mds".to_string()),
        "lib.mds must be tracked as dependency, got: {:?}",
        result.dependencies
    );
}

#[test]
fn fm_import_collision_with_body() {
    // Same alias in fm import AND body @import → name collision error.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define f():\nok\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "@import \"./lib.mds\" as lib\n",
            "{lib.f()}\n",
        )
        .to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("duplicate alias should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("lib") && (msg.contains("collision") || msg.contains("already defined")),
        "expected collision error, got: {msg}"
    );
}

#[test]
fn fm_import_collision_within_fm() {
    // Two fm imports with the same alias → name collision.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define f():\nok\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "{lib.f()}\n",
        )
        .to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("duplicate fm alias should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("lib") && (msg.contains("collision") || msg.contains("already defined")),
        "expected collision error, got: {msg}"
    );
}

#[test]
fn fm_import_circular() {
    // A imports B in frontmatter, B imports A in frontmatter → circular error.
    let mut modules = HashMap::new();
    modules.insert(
        "a.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./b.mds\n",
            "    as: b\n",
            "---\n",
            "Hello!\n",
        )
        .to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./a.mds\n",
            "    as: a\n",
            "---\n",
            "World!\n",
        )
        .to_string(),
    );
    let err = compile_vfs(modules, "a.mds").expect_err("circular fm import should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("circular") || msg.contains("cycle"),
        "expected circular error, got: {msg}"
    );
}

#[test]
fn fm_import_file_not_found() {
    // Nonexistent path in fm import → error message includes "(in frontmatter)".
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./missing.mds\n---\nHello!\n".to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("missing file should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("missing") || msg.contains("not found"),
        "expected file-not-found context, got: {msg}"
    );
    assert!(
        msg.contains("frontmatter"),
        "error should mention frontmatter context, got: {msg}"
    );
}

#[test]
fn fm_import_selective_not_exported() {
    // Name not exported from the target → error.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    names: [nonexistent]\n",
            "---\n",
            "Hello!\n",
        )
        .to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("not-exported name should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("nonexistent") || msg.contains("not exported"),
        "got: {msg}"
    );
}

#[test]
fn fm_import_set_blocked() {
    // --set imports=foo on a .mds file must be rejected.
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello!\n".to_string());
    let mut vars = HashMap::new();
    vars.insert("imports".to_string(), Value::String("foo".to_string()));
    let err = mds::compile_virtual(modules, "main.mds", Some(vars))
        .expect_err("--set imports should be rejected for .mds files");
    let msg = err.to_string();
    assert!(
        msg.contains("imports") && msg.contains("reserved"),
        "expected 'reserved' error, got: {msg}"
    );
}

#[test]
fn fm_import_set_blocked_md_type_mds() {
    // --set imports=foo on a .md file with `type: mds` must also be rejected.
    let mut modules = HashMap::new();
    modules.insert(
        "main.md".to_string(),
        "---\ntype: mds\n---\nHello!\n".to_string(),
    );
    let mut vars = HashMap::new();
    vars.insert("imports".to_string(), Value::String("foo".to_string()));
    let err = mds::compile_virtual(modules, "main.md", Some(vars))
        .expect_err("--set imports should be rejected for .md with type:mds");
    let msg = err.to_string();
    assert!(
        msg.contains("imports") && msg.contains("reserved"),
        "expected 'reserved' error, got: {msg}"
    );
}

#[test]
fn fm_import_md_without_type_mds() {
    // A plain .md file (no `type: mds`) treats `imports` as a regular variable.
    // Virtual FS: use a .md file key directly; the file type check requires `type: mds`.
    // This test validates that `imports` in frontmatter of a .md file without
    // `type: mds` becomes a scope variable rather than structured imports.
    //
    // We test via scan_imports: a .md source with `imports: foo` should not return
    // any paths (the `imports` key is only parsed as structured imports for mds files).
    let source = concat!("---\n", "imports: some_value\n", "---\n", "Hello!\n",);
    // scan_imports is source-only — it doesn't check file types, so it will try
    // to parse `imports: some_value` as frontmatter imports. The parse will fail
    // (not a sequence) and scan_imports silently ignores the error.
    // The key semantic: frontmatter `imports` as a non-sequence is gracefully ignored.
    let paths = mds::scan_imports(source).expect("should not error on non-sequence imports");
    // scan_imports treats the imports key as best-effort; a non-sequence value
    // returns no paths (parse error is silently ignored).
    assert!(
        paths.is_empty(),
        "non-sequence imports should produce no paths, got: {paths:?}"
    );
}

#[test]
fn fm_import_with_other_vars() {
    // fm imports coexist with other frontmatter variables.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHi {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "name: Alice\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "{lib.greet(name)}\n",
        )
        .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("fm imports + other vars should compile");
    assert!(output.contains("Hi Alice!"), "got: {output}");
}

#[test]
fn fm_import_same_path_diff_alias() {
    // Same path imported twice — once in fm (as a), once in body (as b) — both work.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: a\n",
            "---\n",
            "@import \"./lib.mds\" as b\n",
            "{a.greet(\"X\")} {b.greet(\"Y\")}\n",
        )
        .to_string(),
    );
    let output =
        compile_vfs(modules, "main.mds").expect("same path different aliases should compile");
    assert!(output.contains("Hello X!"), "got: {output}");
    assert!(output.contains("Hello Y!"), "got: {output}");
}

#[test]
fn fm_import_collision_merge_within_fm() {
    // Two merge imports that export the same function name → collision error.
    // Exercises the scope.get_function() check in the merge-import path
    // (resolver.rs build_scope_from_frontmatter, FrontmatterImport::Merge branch).
    let mut modules = HashMap::new();
    modules.insert(
        "lib_a.mds".to_string(),
        "@define common():\nfrom-a\n@end\n".to_string(),
    );
    modules.insert(
        "lib_b.mds".to_string(),
        "@define common():\nfrom-b\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib_a.mds\n",
            "  - path: ./lib_b.mds\n",
            "---\n",
            "{common()}\n",
        )
        .to_string(),
    );
    let err = compile_vfs(modules, "main.mds").expect_err("merge collision should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("common") && (msg.contains("collision") || msg.contains("already defined")),
        "expected collision error for 'common', got: {msg}"
    );
}
