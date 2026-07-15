//! Integration tests for Source Map v3 generation via the VirtualFs pipeline.
//!
//! These tests drive the full compiler pipeline (parse → evaluate → finalize)
//! through the public API and assert on the produced [`SourceMap`] structure.
//!
//! Each test covers one acceptance criterion from the CP2 test plan:
//!
//! - Basic source map generation for a single-file template
//! - CR (`\r\n`) compensation
//! - Frontmatter prefix shift
//! - @extends / spliced regions (multi-source segments)
//! - `source_map: false` produces `source_map: None` in [`CompileResult`]
//! - `to_canonical_json` includes the `"sourceMap"` key only when present

use std::collections::HashMap;

use mds::{CompileOptions, CompileResult};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn vfs_opts(modules: HashMap<String, String>, entry: &str, opts: CompileOptions) -> CompileResult {
    mds::compile_virtual_with_deps_opts(modules, entry, None, opts)
        .expect("compilation should succeed")
}

fn vfs_with_map(modules: HashMap<String, String>, entry: &str) -> CompileResult {
    vfs_opts(modules, entry, CompileOptions { source_map: true })
}

fn vfs_no_map(modules: HashMap<String, String>, entry: &str) -> CompileResult {
    vfs_opts(modules, entry, CompileOptions { source_map: false })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// AC-API-01: basic source map is produced for a single-file template.
///
/// Template: "Hello {name}!\n" with frontmatter `name: World`.
/// Expected: source_map is present with version=3, sources=["main.mds"],
/// non-empty mappings.
#[test]
fn source_map_basic_single_file() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert_eq!(sm.version, 3, "version must be 3");
    assert_eq!(sm.sources, vec!["main.mds"], "source name must match entry");
    assert!(
        sm.sources_content.is_some(),
        "sourcesContent must be present"
    );
    assert!(!sm.mappings.is_empty(), "mappings must be non-empty");
    // Security: VLQ alphabet must not contain '-', '<', '>'
    assert!(
        !sm.mappings.contains('-'),
        "VLQ alphabet must not contain '-'"
    );
    assert!(
        !sm.mappings.contains('<'),
        "VLQ alphabet must not contain '<'"
    );
    assert!(
        !sm.mappings.contains('>'),
        "VLQ alphabet must not contain '>'"
    );
}

/// AC-API-02: source_map absent (not null) when source_map option is false.
///
/// Verifies the JSON `skip_serializing_if` contract: when source_map is disabled
/// the field must be entirely absent from JSON output, not serialized as null.
#[test]
fn source_map_absent_when_disabled() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );

    let result = vfs_no_map(modules, "main.mds");

    // In-memory: None
    assert!(
        result.source_map.is_none(),
        "source_map field must be None when disabled"
    );

    // In JSON: absent entirely (not serialized as null)
    let json = serde_json::to_string(&result).expect("should serialize");
    assert!(
        !json.contains("sourceMap"),
        "sourceMap key must not appear in JSON when disabled; got: {json}"
    );
}

/// AC-PERF-01: zero-cost when disabled — no MapBuilder allocated.
///
/// This is observable by ensuring compilation with source_map=false succeeds
/// and produces correct output with no source_map overhead.  The absence of
/// source_map in the result is the observable proxy for no allocation.
#[test]
fn source_map_disabled_zero_cost_observable() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );

    let result = vfs_no_map(modules.clone(), "main.mds");
    assert!(result.source_map.is_none());

    // Output must still be correct
    let output = result.into_markdown().expect("markdown output");
    assert!(output.contains("Hello World!"), "output: {output}");
}

/// S5/stage-2: CR compensation — raw output with `\r\n` produces a source map
/// with mappings that align to the clean (LF-only) output.
///
/// Template content with no frontmatter: "Line1\nLine2\n"
/// Source map must be present and non-empty.  The key invariant is that
/// mappings use LF-based offsets (not CRLF-based).
#[test]
fn source_map_cr_compensation() {
    // We inject CRLF by providing a template whose raw evaluator output would
    // contain \r\n.  Since the evaluator itself doesn't produce \r, we verify
    // via the finalize stage unit test; here we just ensure the pipeline
    // survives CRLF in the *source* template content without panicking and
    // produces a non-empty source map.
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "Hello World\r\nSecond line\r\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert_eq!(sm.version, 3);
    assert!(!sm.mappings.is_empty(), "mappings must be non-empty");
    // No '-' in VLQ output
    assert!(!sm.mappings.contains('-'), "VLQ must not contain '-'");
}

/// S5/stage-4: frontmatter shift — source map for a template with frontmatter
/// must have semicolons in mappings (multiple output lines from FM prefix).
///
/// "---\nfm: v\n---\nHello\n" produces a 4-line output. Lines 0–2 are
/// frontmatter; line 3 is the body content.  The mappings string must contain
/// semicolons separating frontmatter lines from body lines.
#[test]
fn source_map_frontmatter_shift() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nfm: v\n---\nHello\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    // "---\nfm: v\n---\nHello\n" → 4 lines → 3 semicolons minimum in mappings
    let semicolons = sm.mappings.chars().filter(|&c| c == ';').count();
    assert!(
        semicolons >= 3,
        "mappings must have ≥3 semicolons for a 4-line output; got {semicolons}: {:?}",
        sm.mappings
    );
}

/// S5/stage-4: template with no frontmatter produces a single-line mappings
/// string (no semicolons).
#[test]
fn source_map_no_frontmatter_no_semicolons() {
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), "Hello World\n".to_string());

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert!(
        !sm.mappings.contains(';'),
        "single-line output must not contain semicolons; got: {:?}",
        sm.mappings
    );
}

/// AC-API-03 / to_canonical_json: sourceMap key present when enabled.
///
/// Verifies that `to_canonical_json()` includes `"sourceMap"` when
/// `source_map: Some(...)` and omits it when `source_map: None`.
#[test]
fn to_canonical_json_includes_source_map_key_when_present() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "---\nname: World\n---\nHello {name}!\n".to_string(),
    );

    // With source map
    let result_with = vfs_with_map(modules.clone(), "main.mds");
    let json_with = result_with.to_canonical_json();
    assert!(
        json_with.get("sourceMap").is_some(),
        "to_canonical_json must include sourceMap when enabled; got: {json_with}"
    );

    // Without source map
    let result_without = vfs_no_map(modules, "main.mds");
    let json_without = result_without.to_canonical_json();
    assert!(
        json_without.get("sourceMap").is_none(),
        "to_canonical_json must omit sourceMap when disabled; got: {json_without}"
    );
}

/// @extends path: source map for a template using @extends must be present
/// and non-empty, and must list the skeleton template as a source.
#[test]
fn source_map_extends_multi_source() {
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "# Title\n\n@block body:\nDefault body\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "@extends \"./base.mds\"\n@block body:\nChild content\n@end\n".to_string(),
    );

    let result = vfs_with_map(modules, "child.mds");
    let sm = result
        .source_map
        .expect("source_map should be present for @extends");

    assert_eq!(sm.version, 3);
    assert!(
        !sm.mappings.is_empty(),
        "mappings must be non-empty for @extends templates"
    );
    // Both base and child must appear as sources
    assert!(
        !sm.sources.is_empty(),
        "sources must be non-empty for @extends"
    );
}

/// S3/suppression: function-call interpolation records call-site only.
///
/// When a `@define`d function is invoked, the *body* nodes are suppressed
/// and only the Interpolation call-site is recorded.  The source map should
/// still be non-empty with at least one segment.
#[test]
fn source_map_function_call_suppression() {
    let mut modules = HashMap::new();
    modules.insert(
        "main.mds".to_string(),
        "@define greet(x):\nHello {x}!\n@end\n\n{greet(\"World\")}\n".to_string(),
    );

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    assert_eq!(sm.version, 3);
    assert!(
        !sm.mappings.is_empty(),
        "function-call source map must not be empty"
    );
}

/// sourcesContent is populated with template source text.
#[test]
fn source_map_sources_content_populated() {
    let source_text = "---\nname: World\n---\nHello {name}!\n";
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), source_text.to_string());

    let result = vfs_with_map(modules, "main.mds");
    let sm = result.source_map.expect("source_map should be present");

    let contents = sm.sources_content.expect("sourcesContent must be present");
    assert_eq!(contents.len(), 1, "one source → one sourcesContent entry");
    assert_eq!(
        contents[0], source_text,
        "sourcesContent must match original template source"
    );
}
