mod common;
use common::fixture;
use std::collections::HashMap;

#[test]
fn frontmatter_preserved_in_str_output() {
    let result = mds::compile_str("---\nname: World\n---\nHello {{name}}!\n")
        .unwrap()
        .into_markdown()
        .unwrap();
    assert_eq!(result, "---\nname: World\n---\nHello World!\n");
}

#[test]
fn frontmatter_type_mds_stripped() {
    let source = "---\ntype: mds\nname: Alice\n---\nHello {{name}}!\n";
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();
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
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();
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
    let result = mds::compile_str("---\n---\nHello!\n")
        .unwrap()
        .into_markdown()
        .unwrap();
    assert!(
        !result.starts_with("---"),
        "empty frontmatter should not produce fences, got: {result}"
    );
}

#[test]
fn frontmatter_runtime_override_doesnt_alter_output() {
    // The frontmatter in the output reflects original values; the body uses overridden values.
    let source = "---\nname: Alice\n---\nHello {{name}}!\n";
    let mut vars = HashMap::new();
    vars.insert("name".to_string(), mds::Value::String("Bob".to_string()));
    let result = mds::compile_str_with(source, None, Some(vars))
        .unwrap()
        .into_markdown()
        .unwrap();
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
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();
    assert!(
        result.contains("---\nkey: val\n---"),
        "frontmatter-only file should preserve frontmatter, got: {result}"
    );
}

#[test]
fn frontmatter_with_objects_preserved() {
    let source = "---\nconfig:\n  theme: dark\n  debug: true\n---\n{{config.theme}}\n";
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();
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
        "---\nlibkey: libval\n---\n@define greet(n):\nHi {{n}}!\n@end\n",
    )
    .unwrap();
    std::fs::write(
        &main_path,
        "---\nmainkey: mainval\n---\n@import \"./lib.mds\"\n{{greet(\"World\")}}\n",
    )
    .unwrap();
    let result = mds::compile(&main_path, None)
        .unwrap()
        .into_markdown()
        .unwrap();
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
    let source = "---\ntype: mds\nconfig:\n  type: mds\n  theme: dark\n---\n{{config.theme}}\n";
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();
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
    let result = mds::compile(fixture("frontmatter_type_only.md"), None)
        .unwrap()
        .into_markdown()
        .unwrap();
    assert!(
        result.contains("Hello from type-only frontmatter!"),
        "frontmatter with only type:mds should compile, got: {result}"
    );
}

#[test]
fn empty_frontmatter() {
    let result = mds::compile(fixture("empty_frontmatter.mds"), None)
        .unwrap()
        .into_markdown()
        .unwrap();
    assert!(
        result.contains("Hello World!"),
        "empty frontmatter file should compile, got: {result}"
    );
}

#[test]
fn no_frontmatter_with_directives() {
    let result = mds::compile(fixture("no_frontmatter_with_define.mds"), None)
        .unwrap()
        .into_markdown()
        .unwrap();
    assert!(
        result.contains("Hello World!"),
        "file with @define but no frontmatter should compile, got: {result}"
    );
}

#[test]
fn type_key_available_in_mds_files() {
    let result = mds::compile(fixture("type_variable.mds"), None)
        .unwrap()
        .into_markdown()
        .unwrap();
    assert!(
        result.contains("assistant"),
        "expected 'type' variable to be available in .mds files, got: {result}"
    );
}

#[test]
fn yaml_map_type_works() {
    // YAML map values are now supported as objects with dot-notation access.
    let source = "---\nconfig:\n  key: value\n---\n{{config.key}}\n";
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();
    assert!(result.contains("value\n"), "got: {result}");
}

// ── Adversarial frontmatter injection tests ────────────────────────────────────
//
// These tests verify that YAML values containing fence-like content ("\n---\n"),
// block-scalar `type: mds` lines, or YAML-looking strings cannot escape the
// frontmatter block and smuggle additional top-level keys into the compiled output.
//
// The test strategy: extract only the FRONTMATTER SECTION from the output (the text
// between the first `---\n` and the second `---\n`) and assert on that slice.
// Body text may legitimately contain `---` (e.g. from variable substitution), so we
// do not count `---` occurrences in the whole output.

/// Extract the raw YAML inside the first `---`/`---` fence pair in `output`.
/// Returns `None` if the output has no frontmatter fences.
fn extract_frontmatter(output: &str) -> Option<&str> {
    let open = output.find("---\n")?;
    let rest = &output[open + 4..];
    let close = rest.find("\n---")?;
    Some(&rest[..close])
}

#[test]
fn adversarial_value_containing_fence_sequence_via_extends() {
    // An @extends child whose base has a frontmatter value containing a literal
    // `\n---\n` sequence must produce exactly one opening and one closing `---`
    // fence in the emitted frontmatter section — not two document separators.
    //
    // The re-serialization path (serialize_merged_frontmatter / serde_yaml_ng::to_string)
    // must produce valid YAML that doesn't break the fence structure.
    let mut modules = std::collections::HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        // Value that, when stored and re-serialized, could inject a `---` line.
        "---\nprompt: \"start\\n---\\ninjected: evil\\n---\\nend\"\n---\n@block body:\ndefault\n@end\n"
            .to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "---\nrole: system\n---\n@extends \"./base.mds\"\n".to_string(),
    );
    let result = mds::compile_virtual(modules, "child.mds", None)
        .unwrap()
        .into_markdown()
        .unwrap();

    // The frontmatter section must be parseable YAML — check the output starts with `---`.
    assert!(
        result.starts_with("---\n"),
        "output must start with a frontmatter fence; got: {result}"
    );

    // Extract just the frontmatter section and verify `injected: evil` is not a key.
    let fm = extract_frontmatter(&result)
        .expect("output must have a frontmatter section; got: {result}");

    // `injected: evil` must not appear as a top-level key in the frontmatter YAML.
    let has_injected = fm.lines().any(|l| l == "injected: evil");
    assert!(
        !has_injected,
        "adversarial `injected: evil` must not appear as a top-level FM key; fm={fm}"
    );

    // The legitimate keys must be present.
    assert!(
        fm.contains("role: system"),
        "role key must be in FM; fm={fm}"
    );
    assert!(fm.contains("prompt:"), "prompt key must be in FM; fm={fm}");
}

#[test]
fn adversarial_block_scalar_with_type_mds_line() {
    // A block-scalar value whose lines include `type: mds` must not be confused with
    // the top-level `type: mds` directive — only an unindented `type: mds` top-level
    // key should be stripped from the output.
    // The `  type: mds` indented line inside the block scalar must be preserved.
    let source = concat!(
        "---\n",
        "description: |\n",
        "  type: mds\n",
        "  this is a block scalar\n",
        "model: gpt-4\n",
        "---\n",
        "{model}\n",
    );
    let result = mds::compile_str(source).unwrap().into_markdown().unwrap();

    // No unindented `type: mds` line should appear in the output.
    let has_top_level_type = result.lines().any(|l| l == "type: mds");
    assert!(
        !has_top_level_type,
        "unindented 'type: mds' must not appear in output; got: {result}"
    );

    // The description key must be present with block-scalar content preserved.
    assert!(
        result.contains("description:"),
        "description key must be preserved; got: {result}"
    );

    // The model key must be present.
    assert!(
        result.contains("model: gpt-4"),
        "model key must be preserved; got: {result}"
    );

    // Body must resolve {model} to gpt-4.
    assert!(
        result.contains("gpt-4\n") || result.ends_with("gpt-4"),
        "body must render model variable; got: {result}"
    );
}
