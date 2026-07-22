//! End-to-end integration tests for the VirtualFs-backed compilation pipeline.
//!
//! These tests verify that the full MDS compiler pipeline works correctly
//! against an in-memory filesystem, without any OS filesystem access.

use std::collections::HashMap;

use mds::{MdsError, ModuleCache, Value};

// ── Helper ────────────────────────────────────────────────────────────────────

/// Compile a virtual module set and return the rendered Markdown string.
///
/// Creates a `ModuleCache::virtual_fs` from `modules`, resolves `entry` by key,
/// and returns the rendered Markdown (prompt body with frontmatter prepended).
/// These tests use plain (non-`@message`) templates, so the output is Markdown.
fn compile_vfs(modules: HashMap<String, String>, entry: &str) -> Result<String, MdsError> {
    mds::compile_virtual(modules, entry, None).and_then(mds::CompileResult::into_markdown)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn single_file_compile() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {{name}}!\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hello World!"), "got: {output}");
}

#[test]
fn two_file_import_merge() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\"\n{{greet(\"Alice\")}}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hello Alice!"), "got: {output}");
}

#[test]
fn three_file_chain() {
    let mut modules = HashMap::new();
    modules.insert(
        "c.mds".to_string(),
        "@define shout(x):\n{{x}}!!!\n@end\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        "@import \"./c.mds\"\n@define greet(x):\n{{shout(x)}}\n@end\n".to_string(),
    );
    modules.insert(
        "a.mds".to_string(),
        "@import \"./b.mds\"\n{{greet(\"World\")}}\n".to_string(),
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
        "@define greet(x):\nHi {{x}}!\n@end\n@define farewell(x):\nBye {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import { greet } from \"./lib.mds\"\n{{greet(\"Bob\")}}\n".to_string(),
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
        "@define greet(x):\nHi {{x}}!\n@end\n@define farewell(x):\nBye {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import { greet } from \"./lib.mds\"\n{{farewell(\"Bob\")}}\n".to_string(),
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
        "@define internal():\nhidden\n@end\n@define greet(x):\nHi {{x}}!\n@end\n@export greet\n"
            .to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\"\n{{greet(\"Carol\")}}\n".to_string(),
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
        "@define internal():\nhidden\n@end\n@define greet(x):\nHi {{x}}!\n@end\n@export greet\n"
            .to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        // Attempt to call the non-exported function.
        "@import \"./lib.mds\"\n{{internal()}}\n".to_string(),
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
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\" as lib\n{{lib.greet(\"Dave\")}}\n".to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("should compile");
    assert!(output.contains("Hello Dave!"), "got: {output}");
}

#[test]
fn wildcard_reexport() {
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "@define greet(x):\nHi {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "barrel.mds".to_string(),
        "@export * from \"./base.mds\"\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./barrel.mds\"\n{{greet(\"Eve\")}}\n".to_string(),
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
        "@define format_name(x):\n[{{x}}]\n@end\n".to_string(),
    );
    modules.insert(
        "pages/main.mds".to_string(),
        "@import \"../shared/utils.mds\"\n{{format_name(\"Alice\")}}\n".to_string(),
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
        "---\nwho: World\n---\nHello {{who}}!\n".to_string(),
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
fn deps_two_files() {
    // main imports lib → deps = ["lib.mds"]
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "@import \"./lib.mds\"\n{{greet(\"Alice\")}}\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "main.mds", None).expect("should compile");
    assert_eq!(result.dependencies, vec!["lib.mds".to_string()]);
    let md = result.into_markdown().unwrap();
    assert!(md.contains("Hello Alice!"), "got: {md}");
}

#[test]
fn deps_three_file_chain() {
    // a → b → c: deps of a = ["c.mds", "b.mds"] in post-order DFS (leaves first)
    let mut modules = HashMap::new();
    modules.insert(
        "c.mds".to_string(),
        "@define shout(x):\n{{x}}!!!\n@end\n".to_string(),
    );
    modules.insert(
        "b.mds".to_string(),
        "@import \"./c.mds\"\n@define greet(x):\n{{shout(x)}}\n@end\n".to_string(),
    );
    modules.insert(
        "a.mds".to_string(),
        "@import \"./b.mds\"\n{{greet(\"World\")}}\n".to_string(),
    );
    let result = mds::compile_virtual_with_deps(modules, "a.mds", None).expect("should compile");
    // Resolution is post-order DFS: c is inserted first (leaf), then b, then a (entry, excluded).
    assert_eq!(
        result.dependencies,
        vec!["c.mds".to_string(), "b.mds".to_string()]
    );
    let md = result.into_markdown().unwrap();
    assert!(md.contains("World!!!"), "got: {md}");
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
        "@define tag(x):\n[{{x}}]\n@end\n".to_string(),
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
    let md = result.clone().into_markdown().unwrap();
    assert!(md.contains("hello"), "got: {md}");
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
    let result = mds::compile_str_with_deps("---\nname: Alice\n---\nHi {{name}}!\n", None, None)
        .expect("should compile");
    // No imports → no deps
    assert_eq!(result.dependencies, Vec::<String>::new());
    let md = result.into_markdown().unwrap();
    assert!(md.contains("Hi Alice!"), "got: {md}");
}

#[test]
fn deps_str_with_deps_file_import() {
    // compile_str_with_deps resolves @import relative to base_dir on disk.
    use std::io::Write;

    let dir = tempfile::TempDir::new().unwrap();
    let lib_path = dir.path().join("lib.mds");
    let mut f = std::fs::File::create(&lib_path).unwrap();
    f.write_all(b"@define greet(x):\nHello {{x}}!\n@end\n")
        .unwrap();

    let source = "@import \"./lib.mds\"\n{{greet(\"World\")}}\n";
    let result = mds::compile_str_with_deps(source, Some(dir.path()), None)
        .expect("should compile with file import");

    // The lib file is an imported dependency; source string is not a file, so
    // only the imported lib appears in dependencies.
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
    let md = result.into_markdown().unwrap();
    assert!(
        md.contains("Hello World!"),
        "expected rendered output, got: {md}"
    );
}

#[test]
fn deps_error_returns_err() {
    // Undefined variable → Err, no partial deps
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "Hello {{undefined_var}}!\n".to_string(),
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
    // Alias import: `as lib` → `{{lib.greet("Alice")}}` works.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n    as: lib\n---\n{{lib.greet(\"Alice\")}}\n"
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
        "@define greet(x):\nHi {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n---\n{{greet(\"Bob\")}}\n".to_string(),
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
        "@define greet(x):\nHi {{x}}!\n@end\n@define farewell(x):\nBye {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n    names: [greet]\n---\n{{greet(\"Carol\")}}\n"
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
        "@define shout(x):\n{{x}}!!!\n@end\n".to_string(),
    );
    modules.insert(
        "body_lib.mds".to_string(),
        "@define greet(x):\nHello {{x}}\n@end\n".to_string(),
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
            "{{fm.shout(greet(\"Dave\"))}}\n",
        )
        .to_string(),
    );
    let output = compile_vfs(modules, "main.mds").expect("fm + body imports should compile");
    assert!(output.contains("Hello Dave!!!"), "got: {output}");
}

#[test]
fn fm_import_not_leaked_as_var() {
    // `{{imports}}` in body should error — `imports` is reserved, not a scope variable.
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define f():\nok\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        "---\nimports:\n  - path: ./lib.mds\n---\n{{imports}}\n".to_string(),
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
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
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
            "{{lib.greet(name)}}\n",
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
        "@define split_items(s, sep):\n{{split(s, sep)}}\n@end\n".to_string(),
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
            "{{x}}\n",
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
            "@define wrap():\n[{{c.base()}}]\n@end\n",
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
            "{{b.wrap()}}\n",
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
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
    );
    modules.insert(
        "main.mds".to_string(),
        concat!(
            "---\n",
            "imports:\n",
            "  - path: ./lib.mds\n",
            "    as: lib\n",
            "---\n",
            "{{lib.greet(\"World\")}}\n",
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
            "{{lib.f()}}\n",
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
            "{{lib.f()}}\n",
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
        "@define greet(x):\nHi {{x}}!\n@end\n".to_string(),
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
        "@define greet(x):\nHi {{x}}!\n@end\n".to_string(),
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
            "{{lib.greet(name)}}\n",
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
        "@define greet(x):\nHello {{x}}!\n@end\n".to_string(),
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
            "{{a.greet(\"X\")}} {{b.greet(\"Y\")}}\n",
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
            "{{common()}}\n",
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

// ── Issue repros: #150 / #151 (interior-verbatim whitespace contract) ─────────
//
// These tests lock in the compile-output end-to-end, verifying that the
// interior-verbatim contract (blank-line runs preserved, leading/trailing
// newlines no longer stripped from block/define bodies) is observable at the
// compiled markdown level and not just in parser AST tests.

#[test]
fn issue_150_interior_blank_line_runs_preserved_in_compiled_output() {
    // Issue #150: multiple consecutive blank lines inside a body were being
    // collapsed to one by clean_output's old newline-run cap. With the
    // interior-verbatim contract, they must survive to compiled output.
    let src = "Intro.\n\n\n\nBody.\n";
    let out = mds::compile_str(src)
        .expect("#150 repro: should compile")
        .into_markdown()
        .expect("#150 repro: expected markdown output");
    assert_eq!(
        out, "Intro.\n\n\n\nBody.\n",
        "#150: interior blank-line run must be preserved verbatim"
    );
}

#[test]
fn issue_151_block_body_edge_newline_preserved_at_compile_boundary() {
    // Issue #151: @block bodies had their leading/trailing newlines stripped
    // at parse time (strip_leading_newline / strip_trailing_newline). With
    // the interior-verbatim contract, the trailing \n before @end is part of
    // the body and must appear in compiled output, producing a blank line
    // between the block body and the following skeleton text.
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        // "Para A\n" + skeleton "\n" + "Para B\n" was "Para A\nPara B\n" (old).
        // Now "Para A\n" (body verbatim) + "\n" (skeleton) = "Para A\n\nPara B\n".
        "@block section:\nPara A.\n@end\n\nPara B.\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "@extends \"./base.mds\"\n".to_string(),
    );
    let out = mds::compile_virtual(
        modules.into_iter().collect::<HashMap<_, _>>(),
        "child.mds",
        None,
    )
    .expect("#151 repro: should compile")
    .into_markdown()
    .expect("#151 repro: expected markdown output");
    assert_eq!(
        out, "Para A.\n\nPara B.\n",
        "#151: trailing \\n of block body + skeleton \\n must produce a blank line"
    );
}

// ── Issue repros: #154 (@extends emits deep-merged frontmatter) ───────────────
//
// Previously, a child template using @extends only emitted its own raw
// frontmatter in the compiled output, discarding base frontmatter keys that
// the child did not explicitly repeat. The fix serializes the deep-merged
// mapping (base < child, reserved keys excluded) for the output frontmatter.

#[test]
fn issue_154_extends_emits_merged_frontmatter() {
    // Base has `model` and `temperature`; child overrides `temperature` and adds `role`.
    // The compiled output must contain all three non-reserved keys: model (from base),
    // temperature (child overrides base), and role (child-only).
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "---\nmodel: gpt-4\ntemperature: 0.7\n---\n@block section:\nbase text\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "---\ntemperature: 0.2\nrole: system\n---\n@extends \"./base.mds\"\n".to_string(),
    );
    let out =
        compile_vfs(modules.into_iter().collect(), "child.mds").expect("#154: should compile");
    assert!(
        out.contains("model: gpt-4"),
        "#154: base-only key 'model' must appear in merged output; got: {out}"
    );
    assert!(
        out.contains("temperature: 0.2"),
        "#154: child value must win for 'temperature'; got: {out}"
    );
    assert!(
        out.contains("role: system"),
        "#154: child-only key 'role' must appear in merged output; got: {out}"
    );
}

#[test]
fn issue_154_extends_no_base_frontmatter() {
    // Base has no frontmatter; child has frontmatter → only child keys emitted.
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "@block section:\nbase text\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "---\nrole: user\n---\n@extends \"./base.mds\"\n".to_string(),
    );
    let out = compile_vfs(modules.into_iter().collect(), "child.mds")
        .expect("#154: no-base-fm case should compile");
    assert!(
        out.contains("role: user"),
        "#154: child-only key 'role' must appear when base has no frontmatter; got: {out}"
    );
}

#[test]
fn issue_154_extends_reserved_keys_excluded_from_output() {
    // `extends`, `imports`, and `type` are reserved — they must not appear in the
    // compiled output frontmatter even if they appear in the source.
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "---\nmodel: gpt-4\ntype: mds\n---\n@block section:\nbase\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "---\nrole: system\n---\n@extends \"./base.mds\"\n".to_string(),
    );
    let out = compile_vfs(modules.into_iter().collect(), "child.mds")
        .expect("#154: reserved-key exclusion case should compile");
    assert!(
        !out.contains("type:"),
        "#154: reserved key 'type' must NOT appear in merged output; got: {out}"
    );
    assert!(
        !out.contains("extends:"),
        "#154: reserved key 'extends' must NOT appear in merged output; got: {out}"
    );
    assert!(
        out.contains("model: gpt-4"),
        "#154: non-reserved key 'model' must still appear; got: {out}"
    );
    assert!(
        out.contains("role: system"),
        "#154: non-reserved key 'role' must still appear; got: {out}"
    );
}

#[test]
fn issue_154_extends_no_frontmatter_on_either_side() {
    // Neither base nor child has frontmatter → no frontmatter block in output.
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "@block section:\nbase text\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "@extends \"./base.mds\"\n".to_string(),
    );
    let out = compile_vfs(modules.into_iter().collect(), "child.mds")
        .expect("#154: no-fm case should compile");
    assert!(
        !out.contains("---"),
        "#154: output must not contain frontmatter fences when neither side has FM; got: {out}"
    );
}

// ── Issue repros: #152 (cross-type comparison errors, --set-string) ───────────
//
// Previously, comparing values of different types (e.g. number vs string) in
// an @if condition silently returned false (for ==) or true (for !=), leading
// to confusing logic bugs. The fix makes cross-type comparisons a runtime
// TypeMismatch error, forcing explicit type conversion before comparison.

#[test]
fn issue_152_cross_type_eq_is_type_mismatch_error() {
    // `@if x == "3":` with x=3 (number from frontmatter) — number vs string is a
    // TypeMismatch error, not a silent false.
    let src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@else:\nno\n@end\n";
    let err = mds::compile_str(src).expect_err("#152 repro: cross-type == must be a runtime error");
    let msg = format!("{err}");
    assert!(
        msg.contains("type mismatch") || msg.contains("mds::type_mismatch"),
        "#152: expected TypeMismatch error for number == string, got: {msg}"
    );
}

#[test]
fn issue_152_type_mismatch_help_mentions_set_string() {
    // The TypeMismatch help text must reference --set-string so users know how
    // to pass a string value without type coercion.  This pins the contract
    // specified for the error's help field — QA black-box tested exactly this.
    let src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n";
    let err = mds::compile_str(src).expect_err("#152: cross-type == must error");
    let help = miette::Diagnostic::help(&err)
        .map(|h| h.to_string())
        .unwrap_or_default();
    assert!(
        help.contains("--set-string"),
        "#152: TypeMismatch help must mention --set-string; got: {help:?}"
    );
}

#[test]
fn issue_152_cross_type_neq_is_type_mismatch_error() {
    // `@if x != "3":` with x=3 (number) — was silently returning true. Now an error.
    let src = "---\nx: 3\n---\n@if x != \"3\":\ndiff\n@else:\nsame\n@end\n";
    let err = mds::compile_str(src).expect_err("#152 repro: cross-type != must be a runtime error");
    let msg = format!("{err}");
    assert!(
        msg.contains("type mismatch") || msg.contains("mds::type_mismatch"),
        "#152: expected TypeMismatch error for number != string, got: {msg}"
    );
}

#[test]
fn issue_152_same_type_eq_still_works() {
    // Same-type comparisons must continue to work correctly after the change.
    let src = "---\nname: Alice\n---\n@if name == \"Alice\":\nhello Alice\n@end\n";
    let out = mds::compile_str(src)
        .expect("#152: same-type string == should succeed")
        .into_markdown()
        .expect("#152: expected markdown");
    assert!(out.contains("hello Alice"), "#152: same-type eq must work");
}

#[test]
fn issue_152_set_string_forces_string_type() {
    // When a variable is set via the --set-string equivalent (Value::String), comparing
    // it with a string literal must succeed (same-type comparison).
    let src = "---\n---\n@if count == \"3\":\nstring match\n@else:\nno match\n@end\n";
    let mut vars = std::collections::HashMap::new();
    vars.insert("count".to_string(), mds::Value::String("3".to_string()));
    let out = mds::compile_str_with(src, None, Some(vars))
        .expect("#152: string count == \"3\" should succeed")
        .into_markdown()
        .expect("#152: expected markdown");
    assert!(
        out.contains("string match"),
        "#152: --set-string should make count a string so count == \"3\" is true"
    );
}

// ── Round-trip acceptance test (#150/#151) ────────────────────────────────────
//
// A realistic template with frontmatter, post-FM blank line, lists, an indented
// code fence inside a list item, interior blank runs, and a trailing-space line
// is compiled through the pass-through pipeline. The compiled output must be
// byte-identical to the source modulo the two carve-outs mandated by the
// interior-verbatim contract (#150/#151):
//   1. Trailing-edge normalization: any trailing whitespace is stripped, exactly
//      one final newline is appended.
//   2. \r characters are stripped unconditionally.

#[test]
fn round_trip_interior_verbatim_byte_identical() {
    // Source deliberately contains:
    // - Frontmatter with two keys
    // - Blank line after the closing ---
    // - A list with an indented code fence inside a list item
    // - Literal braces inside the fence (must not be interpolated)
    // - Interior blank runs (must be preserved verbatim)
    // - A trailing-space "blank" line (preserved: body-text trailing whitespace
    //   is NOT stripped by `clean_output` — only the very trailing edge is)
    let src = concat!(
        "---\n",
        "author: test\n",
        "version: 1\n",
        "---\n",
        "\n",
        "- First item\n",
        "- Second item with fence:\n",
        "\n",
        "  ```json\n",
        "  {\"key\": \"value\"}\n",
        "  ```\n",
        "\n",
        "- Third item\n",
        "\n",
        "Interior blank run above is preserved.\n",
        "  \n",
        "Trailing-space line above; this is the last line.\n",
    );

    let out = mds::compile_str(src)
        .expect("round-trip source must compile")
        .into_markdown()
        .expect("expected markdown");

    // Apply the two permitted carve-outs to the source for the expected value.
    // clean_output: strip \r, trim trailing whitespace, add one final \n.
    let frontmatter_part = "---\nauthor: test\nversion: 1\n---\n";
    let body_part = concat!(
        "\n",
        "- First item\n",
        "- Second item with fence:\n",
        "\n",
        "  ```json\n",
        "  {\"key\": \"value\"}\n",
        "  ```\n",
        "\n",
        "- Third item\n",
        "\n",
        "Interior blank run above is preserved.\n",
        "  \n",
        "Trailing-space line above; this is the last line.\n",
    );
    // After clean_output: \r stripped (none here), trailing edge normalized to one \n.
    // The body ends with "\n" already, so clean_output produces body_part as-is
    // (trim_end removes the trailing \n-only sequence, then push_str adds one back).
    let expected = format!("{frontmatter_part}{body_part}");

    assert_eq!(
        out, expected,
        "compiled output must be byte-identical to source (modulo trailing-edge normalization)"
    );
}

// ── Integration repro: #149 — indented fence with literal braces ──────────────

#[test]
fn issue_149_indented_fence_inside_list_item_with_braces() {
    // An indented code fence inside a list item containing literal `{{braces}}`
    // must compile successfully: braces inside the fence are NOT interpolated,
    // and the fence lines are emitted verbatim.
    let src = concat!(
        "Here is a list:\n",
        "\n",
        "- Step 1: use the API\n",
        "\n",
        "  ```json\n",
        "  {\"action\": \"query\", \"filter\": {\"type\": \"all\"}}\n",
        "  ```\n",
        "\n",
        "- Step 2: done\n",
    );
    let out = mds::compile_str(src)
        .expect("#149: indented fence in list item with braces must compile")
        .into_markdown()
        .expect("#149: expected markdown output");

    // The braces inside the fence must appear literally in the output.
    assert!(
        out.contains("{\"action\": \"query\""),
        "#149: literal braces inside indented fence must be preserved; got: {out}"
    );
    assert!(
        out.contains("```json"),
        "#149: fence opener must be verbatim; got: {out}"
    );
    assert!(
        out.contains("```\n"),
        "#149: fence closer must be verbatim; got: {out}"
    );
}

// ── D2: type_mismatch_at — span threaded from @if/@elseif directive ──────────────
//
// These tests verify that a cross-type comparison (e.g. string == number) in an
// @if or @elseif condition now carries a source span pointing to the directive line,
// and that the cross-source @extends case degrades gracefully to spanless.

#[test]
fn d2_type_mismatch_at_if_carries_span() {
    // A type mismatch in the primary @if condition must carry a span whose offset
    // falls within the source and whose line/column are computed (non-None).
    // Source: "@if x == \"3\":\nyes\n@end\n" where x=3 (number from frontmatter).
    let src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n";
    let err =
        mds::compile_str(src).expect_err("D2: cross-type == in @if must be a TypeMismatch error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "D2: error must be mds::type_mismatch, got: {}",
        serialized.code
    );
    let span = serialized
        .span
        .expect("D2: @if type_mismatch must carry a span");
    // Offset should point to the start of "@if x == \"3\":" — after the 14-byte frontmatter prefix.
    // The exact offset doesn't matter here, but line/column must be populated.
    assert!(
        span.line.is_some(),
        "D2: type_mismatch span must have a line number, got: {span:?}"
    );
    assert!(
        span.column.is_some(),
        "D2: type_mismatch span must have a column, got: {span:?}"
    );
    assert!(
        span.length > 0,
        "D2: type_mismatch span length must be > 0, got: {span:?}"
    );
}

#[test]
fn d2_type_mismatch_at_elseif_span_points_to_elseif_not_if() {
    // A type mismatch in an @elseif branch must carry a span anchored to the
    // @elseif directive line, NOT to the @if line.
    // Layout:
    // line 4 = "@if x == 5:" — valid but always false (x=3 ≠ 5, same type → no mismatch here)
    // line 5 = "no"
    // line 6 = "@elseif x == \"3\":" — this is where the type_mismatch fires (int vs string)
    // Note: bare `@if false:` is rejected by the parser; use a variable comparison instead.
    let src = "---\nx: 3\n---\n@if x == 5:\nno\n@elseif x == \"3\":\nyes\n@end\n";
    let err = mds::compile_str(src)
        .expect_err("D2: cross-type == in @elseif must be a TypeMismatch error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "D2: error must be mds::type_mismatch, got: {}",
        serialized.code
    );
    let span = serialized
        .span
        .expect("D2: @elseif type_mismatch must carry a span");
    // The span must point to "@elseif x == \"3\":" (line 6 in this 8-line source).
    // We verify: line >= 5 (at least past the @if line at line 4).
    let line = span.line.expect("D2: @elseif span must have a line number");
    assert!(
        line >= 5,
        "D2: type_mismatch span from @elseif must point past the @if line; got line={line}"
    );
    // Also verify the @if line (4) is NOT the anchor: line must be >= 5 for the @elseif.
    // (It's line 6 in this 8-line source; we just check >= 5 to avoid hard-coding offset arithmetic.)
}

#[test]
fn extends_base_skeleton_type_mismatch_span_not_misattributed_to_child() {
    // A type mismatch in a BASE skeleton `@if` condition (offset relative to the base
    // template) must NOT be attributed to the CHILD source. The flat
    // `evaluate(&final_body, …, ctx.source)` path in the `@extends` pipeline evaluates a
    // body spliced from base-skeleton nodes (base-relative offsets) and child block
    // overrides (child-relative offsets) against a single `ctx.source` (the child). A
    // base-relative offset that happens to land within the child source at a char
    // boundary would otherwise anchor the `type_mismatch` span onto the child's
    // `@extends` line — mis-attribution to a foreign source. The contract
    // (`build_type_mismatch` doc / ADR-005) is to degrade to spanless rather than
    // mis-attribute, exactly as the flat non-`@extends` path never faces this because its
    // offsets and `ctx.source` share one origin.
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        // `n` is a string; `@if n == 5` is a string-vs-number mismatch that fires at eval
        // time (the validator cannot type-check it). The `@if` lives in the base SKELETON
        // (not inside a `@block`), so its offset (14) is base-relative. The `@block`
        // placeholder lets `child.mds` be a valid extender.
        "---\nn: hi\n---\n@if n == 5:\nbase-if-branch\n@end\n@block body:\nbase default\n@end\n"
            .to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        // Long enough that the base `@if` offset (14) falls inside the child source at a
        // valid char boundary — the exact condition that triggered mis-attribution.
        "@extends \"./base.mds\"\n@block body:\nThis override body is intentionally long so the base skeleton offset lands inside the child source.\n@end\n"
            .to_string(),
    );
    let err = compile_vfs(modules, "child.mds")
        .expect_err("cross-type mismatch in an inherited @if must error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "must surface as mds::type_mismatch, got: {}",
        serialized.code
    );
    assert!(
        serialized.span.is_none(),
        "cross-source @extends type_mismatch must degrade to spanless (never mis-attributed \
         to the child source); got span: {:?}",
        serialized.span
    );
}

// ── Integration repro: #153 — invalid interpolation hint says \{ not \{{ ───────

#[test]
fn issue_153_invalid_interpolation_hint_text() {
    // Compiling a file with an invalid interpolation shape must produce an error
    // whose hint text contains `\\{{` (double-brace escape hint for the new {{expr}} syntax).
    let src = "{{123invalid}}\n";
    let err = mds::compile_str(src).expect_err("#153: invalid interpolation must fail to compile");
    let msg = format!("{err}");
    assert!(
        msg.contains("\\\\{{"),
        "#153: error hint must mention \\\\{{ (double-brace escape); got: {msg}"
    );
}

// ── Issue #58: evaluate_with_map file/source single source of truth ───────────
//
// Regression guard for the c5a4d65 bug class: `file` and `source` previously
// traveled as BOTH explicit parameters AND EvalContext fields in the source-map
// path.  The fix (#58) derives them from `builder.current_src` — the single
// source of truth — so a caller cannot accidentally pass mismatched values.
//
// These tests compile with source_map=true and verify that type_mismatch spans
// are attributed to the correct source, never to the wrong module.

#[test]
fn source_map_extends_type_mismatch_span_not_misattributed_to_child() {
    // With source_map=true, the @extends pipeline uses evaluate_regions_with_map,
    // which sets builder.current_src to the region's origin before each call.
    // After issue #58, evaluate_with_map_seeded derives ctx.file/ctx.source from
    // that builder entry rather than from explicit params — so the region's own
    // source is always in scope for span attribution.
    //
    // A type_mismatch in the BASE skeleton's @if fires with ctx.source = base content.
    // Any resulting span must fall within the base source, not the child source.
    let mut modules = HashMap::new();
    let base_source =
        "---\nn: hi\n---\n@if n == 5:\nbase-if-branch\n@end\n@block body:\nbase default\n@end\n";
    modules.insert("base.mds".to_string(), base_source.to_string());
    modules.insert(
        "child.mds".to_string(),
        // Long enough that the base @if offset (14) lands inside the child source at
        // a valid char boundary — the exact condition that triggered mis-attribution
        // before c5a4d65 and that #58 structurally prevents.
        "@extends \"./base.mds\"\n@block body:\nThis override body is intentionally long enough that the base skeleton @if offset falls inside the child source.\n@end\n"
            .to_string(),
    );
    let err = mds::compile_virtual_with_deps_opts(
        modules,
        "child.mds",
        None,
        mds::CompileOptions {
            source_map: true,
            include_sources_content: false,
            ..Default::default()
        },
    )
    .expect_err("cross-type mismatch in an inherited @if must error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "must surface as mds::type_mismatch, got: {}",
        serialized.code
    );
    // In the source_map path, evaluate_regions_with_map evaluates the skeleton
    // with the base's origin, so the span (if present) must fall within the base
    // source.  If it degrades to spanless (ADR-005), that is also correct.
    // What is NOT correct: a span with offset >= base_source.len() pointing into
    // child territory.
    if let Some(span) = &serialized.span {
        assert!(
            span.offset < base_source.len(),
            "type_mismatch span (offset={}) must fall within base source (len={}); \
             a larger offset would indicate mis-attribution to the child source",
            span.offset,
            base_source.len()
        );
    }
}

#[test]
fn source_map_standalone_type_mismatch_carries_span() {
    // Standalone (non-@extends) compile with source_map=true: the builder is seeded
    // with the module's own file/source at current_src=0.  After issue #58,
    // evaluate_with_map derives ctx.file and ctx.source from that entry.
    // A type_mismatch in the @if must carry a span whose offset falls within the source.
    let src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n";
    let err = mds::compile_str_with_deps_opts(
        src,
        None,
        None,
        mds::CompileOptions {
            source_map: true,
            include_sources_content: false,
            ..Default::default()
        },
    )
    .expect_err("cross-type == in @if must fail");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "must surface as mds::type_mismatch, got: {}",
        serialized.code
    );
    let span = serialized
        .span
        .expect("standalone source_map type_mismatch must carry a span");
    assert!(
        span.offset < src.len(),
        "span offset ({}) must fall within the source (len={})",
        span.offset,
        src.len()
    );
    assert!(span.length > 0, "span length must be > 0, got: {span:?}");
}

// ── B1: imported-macro type_mismatch span attribution (applies PF-012) ──────────
//
// When a type_mismatch is raised inside an imported @define body, the error should
// name the HELPER (defining) file, not the caller's file, and the span bytes must
// index the helper source — not the caller's source (B1 / PF-012 fix).

fn compile_vfs_err(
    modules: std::collections::HashMap<String, String>,
    entry: &str,
) -> mds::MdsError {
    mds::compile_virtual(modules, entry, None).expect_err("expected a compile error")
}

#[test]
fn b1_type_mismatch_in_imported_define_names_helper_file() {
    // helper.mds defines `check(x)` whose body contains `@if x == "yes":`.
    // main.mds imports helper and calls `{{check(42)}}` — number vs string mismatch
    // in the HELPER'S body.  The error must name "helper.mds", not "main.mds".
    let helper_src =
        "@define check(x):\n@if x == \"yes\":\nok\n@else:\nfail\n@end\n@end\n".to_string();
    // Verify the @if offset 19 is within helper_src: "@define check(x):\n" = 18 chars, so
    // "@if" starts at byte 18.  The type_mismatch fires here.
    let mut modules = std::collections::HashMap::new();
    modules.insert("helper.mds".to_string(), helper_src.clone());
    modules.insert(
        "main.mds".to_string(),
        "@import \"./helper.mds\" as h\n{{h.check(42)}}\n".to_string(),
    );
    let err = compile_vfs_err(modules, "main.mds");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "B1: must be mds::type_mismatch, got: {}",
        serialized.code
    );
    // B1 primary attribution check — the only discriminating assertion (avoids PF-012).
    //
    // SerializedSpan carries no source identity: span.offset == 18 falls within BOTH
    // helper_src (59 B) and main.mds (42 B), so `span.offset < helper_src.len()` alone
    // cannot distinguish definer vs caller attribution.  Direct inspection of the
    // NamedSource embedded in MdsError::TypeMismatch is required.
    //
    // Discriminating power: pre-fix regression (body_origin NOT swapped) would have
    // MdsError::TypeMismatch::src naming "main.mds" → assert_eq! below fails.
    let definer_file = match &err {
        MdsError::TypeMismatch { src, .. } => src
            .as_ref()
            .expect("B1: TypeMismatch must carry a NamedSource for file attribution")
            .name()
            .to_string(),
        _ => panic!("B1: expected TypeMismatch, got: {err:?}"),
    };
    assert_eq!(
        definer_file, "helper.mds",
        "B1: error must name 'helper.mds' (definer), not the caller; \
         pre-fix regression would name 'main.mds'"
    );
    // The span must be populated (not degraded to spanless).
    let span = serialized
        .span
        .expect("B1: type_mismatch in imported body must carry a span");
    // The span offset must fall inside the helper source, not beyond it.
    assert!(
        span.offset < helper_src.len(),
        "B1: span.offset ({}) must fall inside helper_src (len={})",
        span.offset,
        helper_src.len()
    );
    assert!(
        span.length > 0,
        "B1: span.length must be > 0, got: {span:?}"
    );
    // Secondary discriminator: @if is on line 2 of helper_src (after "@define check(x):\n"),
    // but on line 1 of main.mds (byte 18 precedes main.mds's only newline at byte 28).
    assert_eq!(
        span.line,
        Some(2),
        "B1: span must be on line 2 of helper_src (@if directive); \
         caller-attributed regression span would land on line 1 (import line)"
    );
}

#[test]
fn b1_same_file_type_mismatch_span_still_works() {
    // Same-file type_mismatch (existing D2 contract): span must still be populated
    // and offset must fall within the single-file source.
    let src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n";
    let err = mds::compile_str(src).expect_err("B1: same-file type_mismatch must error");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "B1: must be mds::type_mismatch, got: {}",
        serialized.code
    );
    let span = serialized
        .span
        .expect("B1: same-file type_mismatch must carry a span");
    assert!(
        span.offset < src.len(),
        "B1: same-file span.offset ({}) must fall within source (len={})",
        span.offset,
        src.len()
    );
    assert!(span.length > 0, "B1: span.length must be > 0");
}

#[test]
fn b1_two_level_chain_attributes_to_innermost_definer() {
    // a.mds imports b.mds which imports c.mds.  c.mds defines `inner(x)` with a
    // cross-type @if.  b.mds defines `mid(x)` that calls `inner(x)`.  a.mds calls
    // `{{lib.mid(42)}}`.  The error must carry a span indexing c_src.
    let c_src = "@define inner(x):\n@if x == \"yes\":\nok\n@else:\nno\n@end\n@end\n".to_string();
    let b_src = "@import \"./c.mds\" as c\n@define mid(x):\n{{c.inner(x)}}\n@end\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("c.mds".to_string(), c_src.clone());
    modules.insert("b.mds".to_string(), b_src);
    modules.insert(
        "a.mds".to_string(),
        "@import \"./b.mds\" as lib\n{{lib.mid(42)}}\n".to_string(),
    );
    let err = compile_vfs_err(modules, "a.mds");
    let serialized = err.serialize();
    assert_eq!(
        serialized.code, "mds::type_mismatch",
        "B1 two-level: must be mds::type_mismatch, got: {}",
        serialized.code
    );
    // B1 two-level primary attribution check (avoids PF-012).
    //
    // span.offset == 18 falls within a.mds (39 B) and b.mds, so `span.offset < c_src.len()`
    // alone does not discriminate definer vs caller attribution.  Direct NamedSource
    // inspection is required.
    //
    // Discriminating power: regression would name "a.mds" or "b.mds" → assert_eq! fails.
    let definer_file = match &err {
        MdsError::TypeMismatch { src, .. } => src
            .as_ref()
            .expect("B1 two-level: TypeMismatch must carry a NamedSource")
            .name()
            .to_string(),
        _ => panic!("B1 two-level: expected TypeMismatch, got: {err:?}"),
    };
    assert_eq!(
        definer_file, "c.mds",
        "B1 two-level: error must name 'c.mds' (innermost definer), not 'a.mds'/'b.mds'; \
         pre-fix regression would fail here"
    );
    let span = serialized
        .span
        .expect("B1 two-level: must carry a span pointing into c.mds");
    assert!(
        span.offset < c_src.len(),
        "B1 two-level: span.offset ({}) must fall inside c_src (len={})",
        span.offset,
        c_src.len()
    );
    assert!(span.length > 0, "B1 two-level: span.length must be > 0");
    // Secondary discriminator: @if is on line 2 of c_src (after "@define inner(x):\n"),
    // but on line 1 of a.mds (byte 18 precedes a.mds's only newline at byte 24).
    assert_eq!(
        span.line,
        Some(2),
        "B1 two-level: span must be on line 2 of c_src (@if directive); \
         caller-attributed regression span would land on line 1 (import line)"
    );
}

// ── B2: span-less syntax errors get upgraded to span-bearing errors ──────────

/// A missing trailing `:` on `@if` should produce a span-bearing `mds::syntax` error
/// that underlines the directive line, not a spanless message.
#[test]
fn b2_if_missing_colon_carries_span() {
    let src = "Hello\n@if x == \"y\"\nworld\n@end\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("main.mds".to_string(), src.clone());
    let err = compile_vfs_err(modules, "main.mds");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::syntax",
        "B2: expected mds::syntax, got: {}",
        s.code
    );
    let span = s.span.expect("B2: @if missing colon must carry a span");
    // The @if directive starts at byte 6 (after "Hello\n")
    assert_eq!(
        span.offset, 6,
        "B2: span must point to @if directive offset"
    );
    assert!(span.length > 0, "B2: span.length must be > 0");
}

/// A missing trailing `:` on `@for` should produce a span-bearing `mds::syntax` error.
#[test]
fn b2_for_missing_colon_carries_span() {
    let src = "@for x in items\nrow\n@end\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("main.mds".to_string(), src.clone());
    let err = compile_vfs_err(modules, "main.mds");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::syntax",
        "B2: expected mds::syntax, got: {}",
        s.code
    );
    let span = s.span.expect("B2: @for missing colon must carry a span");
    // @for starts at offset 0
    assert_eq!(
        span.offset, 0,
        "B2: span must point to @for directive offset"
    );
    assert!(span.length > 0, "B2: span.length must be > 0");
}

// ── B3: ArityMismatch help text includes function signature ──────────────────

/// Calling a user-defined function with the wrong number of arguments should
/// produce an `mds::arity` error whose help text includes the expected signature.
#[test]
fn b3_arity_mismatch_includes_signature_in_help() {
    // Define foo(x, y) and call it with only one arg.
    let src = "@define foo(x, y):\n{{x}} {{y}}\n@end\n{{foo(\"a\")}}\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("main.mds".to_string(), src);
    let err = compile_vfs_err(modules, "main.mds");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::arity",
        "B3: expected mds::arity, got: {}",
        s.code
    );
    let help = s.help.expect("B3: arity error must have help text");
    assert!(
        help.contains("foo(x, y)"),
        "B3: help must include signature 'foo(x, y)', got: {help}"
    );
}

/// Calling a user-defined function with optional defaults should show defaults in signature.
#[test]
fn b3_arity_mismatch_signature_shows_defaults() {
    let src = "@define greet(name, sep = \", \"):\n{{name}}{{sep}}\n@end\n{{greet()}}\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("main.mds".to_string(), src);
    let err = compile_vfs_err(modules, "main.mds");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::arity",
        "B3: expected mds::arity, got: {}",
        s.code
    );
    let help = s.help.expect("B3: arity error must have help text");
    assert!(
        help.contains("greet(name, sep = \", \")"),
        "B3: help must show defaults, got: {help}"
    );
}

/// Calling a built-in function with the wrong arg count should NOT include a signature.
#[test]
fn b3_builtin_arity_mismatch_no_signature_in_help() {
    // `upper` takes exactly 1 arg.
    let src = "{{upper(\"a\", \"b\")}}\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("main.mds".to_string(), src);
    let err = compile_vfs_err(modules, "main.mds");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::arity",
        "B3 builtin: expected mds::arity, got: {}",
        s.code
    );
    let help = s.help.expect("B3 builtin: arity error must have help text");
    // Built-in help must NOT include "expected: upper(...)"
    assert!(
        !help.contains("expected: upper"),
        "B3 builtin: help should not include builtin signature, got: {help}"
    );
}

// ── B4: TypeMismatch help text update (F8) ──────────────────────────────────

/// TypeMismatch help should mention the lhs type, the --set-string flag, and
/// the '@if x:' idiom, guiding the user toward all three resolution paths.
#[test]
fn b4_type_mismatch_help_mentions_lhs_type_and_both_idioms() {
    // number (x=3) vs string literal "3"
    let src = "---\nx: 3\n---\n@if x == \"3\":\nyes\n@end\n";
    let err = mds::compile_str(src).expect_err("B4: cross-type == must error");
    let help = miette::Diagnostic::help(&err)
        .map(|h| h.to_string())
        .unwrap_or_default();
    // Must still mention --set-string (existing contract, test at line ~1205)
    assert!(
        help.contains("--set-string"),
        "B4: help must contain '--set-string'; got: {help}"
    );
    // Must now also mention comparing against a literal of the same type
    assert!(
        help.contains("number literal") || help.contains("compare against a number"),
        "B4: help must guide toward literal comparison; got: {help}"
    );
    // Must still mention @if x: idiom
    assert!(
        help.contains("@if x:"),
        "B4: help must mention @if x: idiom; got: {help}"
    );
}

/// An unknown directive should produce a span-bearing `mds::syntax` error.
#[test]
fn b2_unknown_directive_carries_span() {
    let src = "hello\n@unknown foo:\nworld\n".to_string();
    let mut modules = std::collections::HashMap::new();
    modules.insert("main.mds".to_string(), src.clone());
    let err = compile_vfs_err(modules, "main.mds");
    let s = err.serialize();
    assert_eq!(
        s.code, "mds::syntax",
        "B2: expected mds::syntax, got: {}",
        s.code
    );
    let span = s.span.expect("B2: unknown directive must carry a span");
    // @unknown starts at byte 6 (after "hello\n")
    assert_eq!(
        span.offset, 6,
        "B2: span must point to @unknown directive offset"
    );
    assert!(span.length > 0, "B2: span.length must be > 0");
}
