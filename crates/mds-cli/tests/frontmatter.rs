mod common;
use common::fixture;
use std::collections::HashMap;

#[test]
fn frontmatter_preserved_in_str_output() {
    let result = mds::compile_str("---\nname: World\n---\nHello {name}!\n").unwrap();
    assert_eq!(result, "---\nname: World\n---\nHello World!\n");
}

#[test]
fn frontmatter_type_mds_stripped() {
    let source = "---\ntype: mds\nname: Alice\n---\nHello {name}!\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("---\nname: Alice\n---\n"),
        "type: mds should be stripped, got: {result}"
    );
    assert!(
        !result.contains("type: mds"),
        "type: mds should not be in output, got: {result}"
    );
}

#[test]
fn frontmatter_type_mds_only_no_fences() {
    let source = "---\ntype: mds\n---\nHello!\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        !result.contains("---"),
        "no frontmatter fences when only type: mds, got: {result}"
    );
    assert_eq!(result, "Hello!\n");
}

#[test]
fn frontmatter_empty_no_fences() {
    // Empty frontmatter (---\n---) — the parser returns None for empty frontmatter,
    // so raw_frontmatter is None and no fences are emitted.
    let result = mds::compile_str("---\n---\nHello!\n").unwrap();
    assert!(
        !result.starts_with("---"),
        "empty frontmatter should not produce fences, got: {result}"
    );
}

#[test]
fn frontmatter_runtime_override_doesnt_alter_output() {
    // The frontmatter in the output reflects original values; the body uses overridden values.
    let source = "---\nname: Alice\n---\nHello {name}!\n";
    let mut vars = HashMap::new();
    vars.insert("name".to_string(), mds::Value::String("Bob".to_string()));
    let result = mds::compile_str_with(source, None, Some(vars)).unwrap();
    assert!(
        result.contains("name: Alice"),
        "output frontmatter should show original value, got: {result}"
    );
    assert!(
        result.contains("Hello Bob!"),
        "body should use overridden value, got: {result}"
    );
}

#[test]
fn frontmatter_only_no_body() {
    // Frontmatter-only file (no body text after ---) emits frontmatter fences.
    let source = "---\nkey: val\n---\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("---\nkey: val\n---"),
        "frontmatter-only file should preserve frontmatter, got: {result}"
    );
}

#[test]
fn frontmatter_with_objects_preserved() {
    let source = "---\nconfig:\n  theme: dark\n  debug: true\n---\n{config.theme}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(
        result.contains("config:"),
        "nested YAML should be preserved in frontmatter, got: {result}"
    );
    assert!(result.contains("theme: dark"), "got: {result}");
    assert!(
        result.contains("dark\n"),
        "body should resolve to 'dark', got: {result}"
    );
}

#[test]
fn frontmatter_imported_module_not_emitted() {
    // Only the root module's frontmatter appears in output.
    // Imported modules' frontmatter is captured but never emitted.
    let dir = tempfile::tempdir().unwrap();
    let lib_path = dir.path().join("lib.mds");
    let main_path = dir.path().join("main.mds");
    std::fs::write(
        &lib_path,
        "---\nlibkey: libval\n---\n@define greet(n):\nHi {n}!\n@end\n",
    )
    .unwrap();
    std::fs::write(
        &main_path,
        "---\nmainkey: mainval\n---\n@import \"./lib.mds\"\n{greet(\"World\")}\n",
    )
    .unwrap();
    let result = mds::compile(&main_path, None).unwrap();
    assert!(
        result.contains("mainkey: mainval"),
        "root frontmatter should appear in output, got: {result}"
    );
    assert!(
        !result.contains("libkey"),
        "imported module frontmatter should not appear in output, got: {result}"
    );
}

#[test]
fn strip_type_mds_only_strips_top_level_key() {
    // Regression: strip_type_mds used line.trim() before matching, which caused
    // indented 'type: mds' lines inside nested YAML objects to be incorrectly removed.
    // Only the top-level (no leading whitespace) 'type: mds' directive should be stripped.
    let source = "---\ntype: mds\nconfig:\n  type: mds\n  theme: dark\n---\n{config.theme}\n";
    let result = mds::compile_str(source).unwrap();
    // The top-level `type: mds` must not appear as an unindented frontmatter key.
    // Check that no line in the output equals "type: mds" (i.e. no leading whitespace).
    let has_top_level_type_mds = result.lines().any(|line| line == "type: mds");
    assert!(
        !has_top_level_type_mds,
        "top-level 'type: mds' should be stripped from output, got: {result}"
    );
    // The indented `  type: mds` under config: must be preserved (was the bug).
    assert!(
        result.contains("  type: mds"),
        "indented 'type: mds' inside nested YAML object should be preserved, got: {result}"
    );
    assert!(
        result.contains("theme: dark"),
        "sibling key in nested object should be preserved, got: {result}"
    );
    assert!(
        result.contains("dark\n"),
        "body should resolve config.theme to 'dark', got: {result}"
    );
}

#[test]
fn frontmatter_type_only_compiles() {
    // A .md file with only `type: mds` in frontmatter (no other variables) should compile.
    let result = mds::compile(fixture("frontmatter_type_only.md"), None).unwrap();
    assert!(
        result.contains("Hello from type-only frontmatter!"),
        "frontmatter with only type:mds should compile, got: {result}"
    );
}

#[test]
fn empty_frontmatter() {
    let result = mds::compile(fixture("empty_frontmatter.mds"), None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "empty frontmatter file should compile, got: {result}"
    );
}

#[test]
fn no_frontmatter_with_directives() {
    let result = mds::compile(fixture("no_frontmatter_with_define.mds"), None).unwrap();
    assert!(
        result.contains("Hello World!"),
        "file with @define but no frontmatter should compile, got: {result}"
    );
}

#[test]
fn type_key_available_in_mds_files() {
    let result = mds::compile(fixture("type_variable.mds"), None).unwrap();
    assert!(
        result.contains("assistant"),
        "expected 'type' variable to be available in .mds files, got: {result}"
    );
}

#[test]
fn yaml_map_type_works() {
    // YAML map values are now supported as objects with dot-notation access.
    let source = "---\nconfig:\n  key: value\n---\n{config.key}\n";
    let result = mds::compile_str(source).unwrap();
    assert!(result.contains("value\n"), "got: {result}");
}
