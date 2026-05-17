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
fn export_visibility() {
    // greet is exported; internal is not.
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
        matches!(err, MdsError::FileNotFound { .. } | MdsError::ImportError { .. }),
        "expected FileNotFound or ImportError, got {err:?}"
    );
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
    let prompt = resolved.get_prompt_value();
    let body = prompt.as_ref().and_then(|v| {
        if let mds::Value::String(s) = v {
            Some(s.as_str())
        } else {
            None
        }
    }).unwrap_or("");
    assert!(body.contains("Hello World!"), "got body: {body:?}");
}
