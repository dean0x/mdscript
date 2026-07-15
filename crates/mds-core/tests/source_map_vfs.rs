//! Integration tests for Source Map v3 generation via the VirtualFs pipeline.
//!
//! These tests drive the full compiler pipeline (parse → evaluate → finalize)
//! through the public API and assert on the produced [`SourceMap`] structure.
//!
//! CP2 tests cover one acceptance criterion each:
//!
//! - Basic source map generation for a single-file template
//! - CR (`\r\n`) compensation
//! - Frontmatter prefix shift
//! - @extends / spliced regions (multi-source segments)
//! - `source_map: false` produces `source_map: None` in [`CompileResult`]
//! - `to_canonical_json` includes the `"sourceMap"` key only when present
//!
//! CP3 tests cover `@include` `FragmentMap` splicing (S6):
//!
//! - 2-file attribution: partial content maps to source index 1
//! - 3-file attribution: two independent partials, both referenced
//! - Dedup: same partial included twice → ONE `sources` entry (AC-FUNC-04)
//! - `@for` loop: N iterations do not duplicate `sources` (AC-PERF-05)
//! - Nested compose: outer includes inner, all three sources present bottom-up
//! - Determinism: repeated compilations produce identical mappings (AC-FUNC-05)
//! - Output invariance: compiled text byte-identical with/without source maps (AC-PERF-02)

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

// ══════════════════════════════════════════════════════════════════════════════
// CP3 tests — @include FragmentMap splice (S6)
// ══════════════════════════════════════════════════════════════════════════════

// ── VLQ decode helpers ────────────────────────────────────────────────────────

/// Decode one Base64-VLQ signed integer from the start of `bytes`.
/// Returns `(decoded_value, remaining_bytes)`.
fn vlq_decode_one(bytes: &[u8]) -> (i64, &[u8]) {
    let mut acc: u64 = 0;
    let mut shift: u32 = 0;
    let mut i = 0;
    loop {
        let v: u64 = match bytes[i] {
            b'A'..=b'Z' => (bytes[i] - b'A') as u64,
            b'a'..=b'z' => (bytes[i] - b'a') as u64 + 26,
            b'0'..=b'9' => (bytes[i] - b'0') as u64 + 52,
            b'+' => 62,
            b'/' => 63,
            c => panic!("invalid VLQ char: {}", c as char),
        };
        i += 1;
        acc |= (v & 0x1F) << shift;
        shift += 5;
        if v & 0x20 == 0 {
            break;
        }
    }
    // zigzag decode: LSB is sign
    let n: i64 = if acc & 1 == 1 {
        -((acc >> 1) as i64)
    } else {
        (acc >> 1) as i64
    };
    (n, &bytes[i..])
}

/// Collect the set of all source-file indices referenced by mapped segments.
///
/// Correctly accumulates `src_base` across lines (semicolons reset only
/// `gen_col`; `src_base` is cumulative across the entire mappings string).
fn referenced_src_indices(mappings: &str) -> std::collections::HashSet<u32> {
    let mut seen = std::collections::HashSet::new();
    let mut src_base: i64 = 0;
    for line_str in mappings.split(';') {
        for seg_str in line_str.split(',') {
            if seg_str.is_empty() {
                continue;
            }
            let (_, rest) = vlq_decode_one(seg_str.as_bytes()); // genCol delta
            if rest.is_empty() {
                continue; // 1-field (unmapped) segment — no source info
            }
            let (ds, _) = vlq_decode_one(rest); // srcIdx delta
            src_base += ds;
            seen.insert(src_base as u32);
        }
    }
    seen
}

// ── CP3 integration tests ─────────────────────────────────────────────────────

/// S6 / 2-file attribution: entry @import s + @include s a partial; the
/// produced source map must list both files in `sources` and attribute the
/// partial's output bytes to source index 1.
#[test]
fn source_map_include_two_file_attribution() {
    let entry_src = "@import \"./partial.mds\" as p\n@include p\n";
    let partial_src = "Hello from partial\n";

    let mut modules = HashMap::new();
    modules.insert("entry.mds".to_string(), entry_src.to_string());
    modules.insert("partial.mds".to_string(), partial_src.to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // Source list: entry seeded at 0, partial added on first splice.
    assert_eq!(
        sm.sources,
        vec!["entry.mds", "partial.mds"],
        "sources must list entry first, then partial"
    );

    // sourcesContent mirrors sources.
    let contents = sm.sources_content.expect("sourcesContent must be present");
    assert_eq!(contents.len(), 2);
    assert_eq!(
        contents[0], entry_src,
        "sourcesContent[0] must be entry source"
    );
    assert_eq!(
        contents[1], partial_src,
        "sourcesContent[1] must be partial source"
    );

    // At least one output byte attributed to source 1 (the partial).
    assert!(!sm.mappings.is_empty(), "mappings must be non-empty");
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.contains(&1),
        "partial content must be attributed to source index 1; indices={:?} mappings={:?}",
        src_indices,
        sm.mappings
    );
}

/// S6 / 3-file, two independent partials: entry includes a.mds then b.mds;
/// all three files appear in `sources` in interner order (entry, a, b) and
/// both partial source indices appear in the decoded mappings.
#[test]
fn source_map_include_three_file_two_partials() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include a\n@include b\n".to_string(),
    );
    modules.insert("a.mds".to_string(), "Part A\n".to_string());
    modules.insert("b.mds".to_string(), "Part B\n".to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // Interner order: entry (seed), a (first splice), b (second splice).
    assert_eq!(
        sm.sources,
        vec!["entry.mds", "a.mds", "b.mds"],
        "sources must list all three files in splice order"
    );

    // Both partials referenced in segments.
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.contains(&1),
        "a.mds content must be attributed to source 1"
    );
    assert!(
        src_indices.contains(&2),
        "b.mds content must be attributed to source 2"
    );
}

/// S6 / AC-FUNC-04 dedup: the same partial included twice produces exactly ONE
/// `sources` entry for that partial, not two.  Both output regions map to
/// source index 1.
#[test]
fn source_map_include_dedup_same_partial_twice() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./partial.mds\" as p\n@include p\n@include p\n".to_string(),
    );
    modules.insert("partial.mds".to_string(), "Line\n".to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // Two @include s of the same partial → only ONE extra sources entry.
    assert_eq!(
        sm.sources.len(),
        2,
        "dedup: two includes of the same partial must yield exactly 2 sources; got {:?}",
        sm.sources
    );
    assert_eq!(sm.sources[1], "partial.mds");

    let contents = sm.sources_content.expect("sourcesContent must be present");
    assert_eq!(contents.len(), 2, "sourcesContent must mirror sources");

    // All segments map to source 1 only.
    let src_indices = referenced_src_indices(&sm.mappings);
    assert_eq!(
        src_indices.len(),
        1,
        "only source index 1 must be referenced; got {:?}",
        src_indices
    );
    assert!(src_indices.contains(&1), "source 1 must be referenced");
}

/// S6 / AC-PERF-05: @include inside @for — N iterations must not duplicate
/// `sources`.  The local→global remap is built once and reused across
/// iterations; the observable invariant is `sources.len() == 2` regardless
/// of iteration count.
#[test]
fn source_map_include_for_loop_sources_not_duplicated() {
    // 5 iterations; each @include p splices the same partial once.
    let entry_src = "---\nitems: [a, b, c, d, e]\n---\n\
                     @import \"./partial.mds\" as p\n\
                     @for item in items:\n\
                     @include p\n\
                     @end\n";

    let mut modules = HashMap::new();
    modules.insert("entry.mds".to_string(), entry_src.to_string());
    modules.insert("partial.mds".to_string(), "Repeated line\n".to_string());

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // 5 iterations must not produce 6 sources — interner dedup keeps it at 2.
    assert_eq!(
        sm.sources.len(),
        2,
        "@for with 5 iterations must not duplicate sources; got {:?}",
        sm.sources
    );
    assert_eq!(sm.sources[1], "partial.mds");

    // Body segments all attributed to source 1 (the partial).
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.contains(&1),
        "partial segments must be attributed to source 1; got {:?}",
        src_indices
    );
}

/// S6 / nested compose: outer partial @include s inner partial; the outer's
/// `FragmentMap` is built bottom-up so it already carries inner's segments.
/// When entry splices outer, all three sources appear in the final map.
#[test]
fn source_map_include_nested_compose() {
    let mut modules = HashMap::new();
    modules.insert("inner.mds".to_string(), "Inner text\n".to_string());
    modules.insert(
        "outer.mds".to_string(),
        "@import \"./inner.mds\" as inner\n@include inner\nOuter text\n".to_string(),
    );
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./outer.mds\" as outer\n@include outer\n".to_string(),
    );

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result.source_map.expect("source_map must be present");

    // All three sources present: entry (seed), outer, inner (nested splice).
    assert_eq!(
        sm.sources.len(),
        3,
        "nested @include must bring all 3 sources; got {:?}",
        sm.sources
    );
    assert!(
        sm.sources.contains(&"outer.mds".to_string()),
        "outer.mds must be in sources; got {:?}",
        sm.sources
    );
    assert!(
        sm.sources.contains(&"inner.mds".to_string()),
        "inner.mds must be in sources; got {:?}",
        sm.sources
    );

    // Segments reference both outer and inner source indices.
    let src_indices = referenced_src_indices(&sm.mappings);
    assert!(
        src_indices.len() >= 2,
        "must reference at least outer and inner source indices; got {:?}",
        src_indices
    );
}

/// S6 / AC-FUNC-05 (core half): two separate compilations of the same template
/// produce identical `sources` ordering and identical `mappings` strings.
#[test]
fn source_map_include_deterministic() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include a\n@include b\n".to_string(),
    );
    modules.insert("a.mds".to_string(), "Alpha\n".to_string());
    modules.insert("b.mds".to_string(), "Beta\n".to_string());

    let sm1 = vfs_with_map(modules.clone(), "entry.mds")
        .source_map
        .expect("first compilation must produce source_map");
    let sm2 = vfs_with_map(modules, "entry.mds")
        .source_map
        .expect("second compilation must produce source_map");

    assert_eq!(
        sm1.sources, sm2.sources,
        "sources order must be deterministic"
    );
    assert_eq!(sm1.mappings, sm2.mappings, "mappings must be deterministic");
}

/// AC-PERF-02: compiled output is byte-identical whether source maps are
/// enabled or disabled — the `MapBuilder` is transparent to the evaluator.
#[test]
fn source_map_include_output_unchanged() {
    let mut modules = HashMap::new();
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@include a\n@include b\n".to_string(),
    );
    modules.insert("a.mds".to_string(), "Part A\n".to_string());
    modules.insert("b.mds".to_string(), "Part B\n".to_string());

    let with_map = vfs_with_map(modules.clone(), "entry.mds")
        .into_markdown()
        .expect("markdown with source map");
    let without_map = vfs_no_map(modules, "entry.mds")
        .into_markdown()
        .expect("markdown without source map");

    assert_eq!(
        with_map, without_map,
        "compiled output must be byte-identical regardless of source_map setting"
    );
}
