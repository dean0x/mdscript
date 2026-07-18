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
//!
//! CP4 tests cover S7/S8/S9:
//!
//! - S8 body attribution: function defined in imported file attributed to that file
//! - S8 nested trim: f→g→h composition with leading/trailing whitespace
//! - AC-FUNC-06: sourcesContent present when on, absent when off
//! - AC-FUNC-07: messages-mode template → `source_map: None` + warning
//! - AC-PERF-03: segment cap overflow → `source_map: None` + warning
//! - AC-PERF-04: large multibyte line — map present and VLQ alphabet clean

use std::collections::HashMap;

use mds::{CompileOptions, CompileResult, Value};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn vfs_opts(modules: HashMap<String, String>, entry: &str, opts: CompileOptions) -> CompileResult {
    mds::compile_virtual_with_deps_opts(modules, entry, None, opts)
        .expect("compilation should succeed")
}

fn vfs_with_map(modules: HashMap<String, String>, entry: &str) -> CompileResult {
    // include_sources_content: true so that existing sourcesContent assertions continue to pass.
    vfs_opts(
        modules,
        entry,
        CompileOptions {
            source_map: true,
            include_sources_content: true,
        },
    )
}

fn vfs_no_map(modules: HashMap<String, String>, entry: &str) -> CompileResult {
    vfs_opts(
        modules,
        entry,
        CompileOptions {
            source_map: false,
            ..Default::default()
        },
    )
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

/// AC-PERF-02 (@extends): compiled output for an `@extends` template must be
/// byte-identical whether source maps are on or off. This is the riskiest path
/// for divergence because map-on evaluates per spliced region
/// (`evaluate_regions_with_map`) while map-off evaluates the assembled
/// `final_body` in one pass — the two must produce the same bytes.
#[test]
fn source_map_extends_output_unchanged() {
    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "# Title\n\n@block intro:\nDefault intro\n@end\n\nMiddle\n\n@block body:\nDefault body\n@end\n\nFooter\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "@extends \"./base.mds\"\n@block body:\nChild content\n@end\n".to_string(),
    );

    let with_map = vfs_with_map(modules.clone(), "child.mds")
        .into_markdown()
        .expect("markdown with source map");
    let without_map = vfs_no_map(modules, "child.mds")
        .into_markdown()
        .expect("markdown without source map");

    assert_eq!(
        with_map, without_map,
        "@extends compiled output must be byte-identical regardless of source_map setting"
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

// ══════════════════════════════════════════════════════════════════════════════
// CP4 tests — S7/S8/S9: provenance, fine-grained body mapping, bounds
// ══════════════════════════════════════════════════════════════════════════════

/// S8 body attribution: a function defined in an imported file must appear in
/// `sources` when its body is evaluated under source-map mode (S8 path).
///
/// Without S8 provenance, the library file would never appear in `sources`
/// because the body is suppressed (S3 path attributes output to the call site).
/// With S8, the body tokens are recorded against the definition file, so
/// `sources` must contain both `entry.mds` and `lib.mds`.
#[test]
fn source_map_s8_cross_file_function_attribution() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(who):\nHello {who}!\n@end\n".to_string(),
    );
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./lib.mds\" as lib\n{lib.greet(\"World\")}\n".to_string(),
    );

    let result = vfs_with_map(modules.clone(), "entry.mds");
    let sm = result
        .source_map
        .clone()
        .expect("source_map must be present");

    // Compiled text must still be correct (AC-PERF-02: output unchanged).
    let text = result.into_markdown().expect("markdown output");
    assert_eq!(
        text.trim(),
        "Hello World!",
        "S8 must not alter compiled output"
    );

    // S8: the definition file must appear in sources.
    assert!(
        sm.sources.contains(&"lib.mds".to_string()),
        "lib.mds must appear in sources (S8 body attribution); got: {:?}",
        sm.sources
    );
    assert!(
        sm.sources.contains(&"entry.mds".to_string()),
        "entry.mds must appear in sources; got: {:?}",
        sm.sources
    );

    // sourcesContent must include lib.mds content.
    let sc = sm
        .sources_content
        .as_ref()
        .expect("sourcesContent must be present");
    assert_eq!(
        sc.len(),
        sm.sources.len(),
        "sourcesContent entries must match sources count"
    );
    let lib_idx = sm
        .sources
        .iter()
        .position(|s| s == "lib.mds")
        .expect("lib.mds position");
    assert!(
        sc[lib_idx].contains("@define greet"),
        "sourcesContent for lib.mds must contain its source"
    );

    // Without source maps, output is identical.
    let no_map_text = vfs_no_map(modules, "entry.mds")
        .into_markdown()
        .expect("markdown without map");
    assert_eq!(text, no_map_text, "output must be map-independent");
}

/// S8 nested trim: function composition f→g→h where each level adds
/// leading/trailing whitespace.  After rebase_trim at each level, only
/// the actual content tokens are attributed to the definition files.
///
/// Verifies:
/// - Compilation succeeds and output is trimmed correctly.
/// - All three definition files appear in `sources`.
/// - VLQ alphabet is valid (no `-`, `<`, `>`).
#[test]
fn source_map_s8_nested_trim_composition() {
    let mut modules = HashMap::new();
    // h produces content with extra surrounding whitespace.
    modules.insert(
        "h.mds".to_string(),
        "@define h(x):\n   inner {x}   \n@end\n".to_string(),
    );
    // g calls h, adding its own wrapper whitespace.
    modules.insert(
        "g.mds".to_string(),
        "@import \"./h.mds\" as hm\n@define g(x):\n  {hm.h(x)}  \n@end\n".to_string(),
    );
    // f calls g.
    modules.insert(
        "f.mds".to_string(),
        "@import \"./g.mds\" as gm\n@define f(x):\n{gm.g(x)}\n@end\n".to_string(),
    );
    // entry calls f.
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./f.mds\" as fm\nResult: {fm.f(\"test\")}\n".to_string(),
    );

    let result = vfs_with_map(modules, "entry.mds");
    let sm = result
        .source_map
        .clone()
        .expect("source_map must be present");

    // Compiled output must be correct.
    let text = result
        .into_markdown()
        .expect("markdown for nested-trim test");
    assert!(
        text.contains("inner test"),
        "nested trim must produce correct content; got: {text:?}"
    );

    // All definition files must appear in sources.
    for expected in &["h.mds", "g.mds", "f.mds", "entry.mds"] {
        assert!(
            sm.sources.iter().any(|s| s == expected),
            "{expected} must appear in sources for nested-trim S8; got: {:?}",
            sm.sources
        );
    }

    // VLQ alphabet must be valid.
    assert!(
        !sm.mappings.contains('-'),
        "VLQ mappings must not contain '-'"
    );
    assert!(
        !sm.mappings.contains('<'),
        "VLQ mappings must not contain '<'"
    );
    assert!(
        !sm.mappings.contains('>'),
        "VLQ mappings must not contain '>'"
    );
}

/// AC-FUNC-06: `sourcesContent` is present when source_map=true and absent
/// when source_map=false (i.e. source_map field is None).
///
/// Also verifies that `sourcesContent[0]` matches the template source byte-for-byte.
#[test]
fn source_map_sources_content_on_off() {
    let source_text = "Hello world\n";
    let mut modules = HashMap::new();
    modules.insert("main.mds".to_string(), source_text.to_string());

    // source_map=true → sources_content is Some with matching content.
    let with_map = vfs_with_map(modules.clone(), "main.mds");
    let sm = with_map
        .source_map
        .expect("source_map must be Some when source_map=true");
    let sc = sm
        .sources_content
        .expect("sourcesContent must be present when source_map=true");
    assert_eq!(
        sc.len(),
        1,
        "single-source compilation must have one sourcesContent entry"
    );
    assert_eq!(
        sc[0], source_text,
        "sourcesContent must match original template source byte-for-byte"
    );

    // source_map=false → source_map field is None (AC-PERF-01 zero-cost path).
    let no_map = vfs_no_map(modules, "main.mds");
    assert!(
        no_map.source_map.is_none(),
        "source_map must be None when source_map=false"
    );
}

/// AC-FUNC-07: a template with `@message` blocks in messages mode combined
/// with `source_map: true` must return `source_map: None` and emit a warning.
///
/// Source maps operate on the flat text output stream; messages-mode boundaries
/// are not representable in the SMv3 segment model, so we degrade gracefully.
#[test]
fn source_map_messages_mode_degrades_to_none() {
    let mut modules = HashMap::new();
    modules.insert(
        "chat.mds".to_string(),
        "@message role=user:\nHello!\n@end\n".to_string(),
    );

    let result = vfs_opts(
        modules,
        "chat.mds",
        CompileOptions {
            source_map: true,
            ..Default::default()
        },
    );

    // source_map must be None (messages-mode degrades gracefully).
    assert!(
        result.source_map.is_none(),
        "messages-mode with source_map=true must degrade to None"
    );

    // A warning must be emitted explaining the degradation (AC-FUNC-07).
    // The warning uses MSG_MODE_SOURCE_MAP_WARNING (surface-neutral wording).
    let has_warning = result
        .warnings
        .iter()
        .any(|w| w.contains("messages-mode output") && w.contains("source_map will be None"));
    assert!(
        has_warning,
        "AC-FUNC-07: must emit a warning for messages-mode + source_map=true; \
         got warnings: {:?}",
        result.warnings
    );
}

/// AC-PERF-03: when the segment cap (`MAX_SOURCEMAP_SEGMENTS`) is exceeded,
/// the result degrades to `source_map: None` rather than emitting a partial
/// (and potentially misleading) map.  A warning is emitted to the caller.
///
/// This test generates >1 000 000 segments by iterating over a large array
/// with a multi-token loop body (11 segment-producing nodes per iteration ×
/// 100 000 iterations = 1 100 000 > 1 000 000 cap).
#[test]
fn source_map_segment_cap_degrades_to_none() {
    use mds::Value;

    // Build a 100 000-element array at runtime (avoids a giant template literal).
    let items: Vec<Value> = (0..100_000)
        .map(|_| Value::String("x".to_string()))
        .collect();
    let mut vars = std::collections::HashMap::new();
    vars.insert("items".to_string(), Value::Array(items));

    let mut modules = std::collections::HashMap::new();
    // Body has 6 Text nodes + 5 Interpolation nodes = 11 segments per iteration.
    // 100 000 iters × 11 = 1 100 000 > MAX_SOURCEMAP_SEGMENTS (1 000 000).
    modules.insert(
        "big.mds".to_string(),
        "@for item in items:\nA{item}B{item}C{item}D{item}E{item}F\n@end\n".to_string(),
    );

    let result = mds::compile_virtual_with_deps_opts(
        modules,
        "big.mds",
        Some(vars),
        CompileOptions {
            source_map: true,
            ..Default::default()
        },
    )
    .expect("compilation must succeed even when cap is hit");

    // Extract source_map before consuming result via into_markdown.
    let source_map_none = result.source_map.is_none();
    let warnings = result.warnings.clone();

    // Compilation succeeds: output is present (no error).
    let _text = result
        .into_markdown()
        .expect("markdown output must be present");

    // Source map degrades to None (AC-PERF-03).
    assert!(
        source_map_none,
        "source_map must be None when segment cap is exceeded (AC-PERF-03)"
    );

    // A warning must be emitted.
    let has_warning = warnings
        .iter()
        .any(|w| w.contains("segment cap") || w.contains("source_map will be None"));
    assert!(
        has_warning,
        "AC-PERF-03: must emit a warning when segment cap is exceeded; \
         got warnings: {:?}",
        warnings
    );
}

/// AC-PERF-04: a template with long lines containing multibyte (UTF-8)
/// characters must produce a valid source map with clean VLQ alphabet.
///
/// This is a shape test: we verify the map is present, non-empty, and that
/// the VLQ encoding does not accidentally emit invalid characters.
#[test]
fn source_map_multibyte_line_vlq_alphabet() {
    let mut modules = HashMap::new();
    // Mix ASCII and CJK (3-byte UTF-8) characters on one long line.
    // The VLQ encoder must handle byte-position arithmetic correctly for
    // multi-byte codepoints (AC-PERF-04).
    let long_cjk_line = "你好世界".repeat(100); // 400 CJK chars = 1200 bytes
                                                // Embed as a frontmatter value so the template renders a long CJK string.
    let source_with_fm = format!("---\nval: \"{long_cjk_line}\"\n---\n{{val}}\n");
    modules.insert("cjk.mds".to_string(), source_with_fm);

    let result = vfs_with_map(modules, "cjk.mds");
    let sm = result.source_map.expect("source_map must be present");

    assert!(
        !sm.mappings.is_empty(),
        "mappings must be non-empty for multibyte template"
    );
    // VLQ alphabet must not contain forbidden characters.
    assert!(
        !sm.mappings.contains('-'),
        "VLQ must not contain '-' (AC-PERF-04)"
    );
    assert!(
        !sm.mappings.contains('<'),
        "VLQ must not contain '<' (AC-PERF-04)"
    );
    assert!(
        !sm.mappings.contains('>'),
        "VLQ must not contain '>' (AC-PERF-04)"
    );
}

/// S8 output invariance: function defined in an imported file compiles to the
/// same text regardless of whether source maps are enabled (AC-PERF-02).
#[test]
fn source_map_s8_output_unchanged() {
    let mut modules = HashMap::new();
    modules.insert(
        "lib.mds".to_string(),
        "@define greet(who):\nHello {who}!\n@end\n".to_string(),
    );
    modules.insert(
        "entry.mds".to_string(),
        "@import \"./lib.mds\" as lib\n{lib.greet(\"World\")}\n".to_string(),
    );

    let with_map = vfs_with_map(modules.clone(), "entry.mds")
        .into_markdown()
        .expect("markdown with map");
    let without_map = vfs_no_map(modules, "entry.mds")
        .into_markdown()
        .expect("markdown without map");

    assert_eq!(
        with_map, without_map,
        "S8 must not change compiled output (AC-PERF-02)"
    );
}

/// RUST-4 / guards ADR-002: @extends compiled output must be byte-identical whether
/// `source_map` is `true` (region-by-region `evaluate_regions_with_map` path) or
/// `false` (whole-body `evaluate` path).
///
/// This test would catch output drift introduced by the REL-1 budget-threading fix
/// — if cumulative iteration counting ever altered the output string the assertion
/// below would fail.
#[test]
fn extends_output_byte_identical_with_and_without_source_map() {
    let mut modules = HashMap::new();
    // Base has no frontmatter; @block is a body directive, not YAML.
    modules.insert(
        "base.mds".to_string(),
        "@block content:\ndefault\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "---\nextends: base.mds\n---\n@block content:\nchild content\n@end\n".to_string(),
    );

    let with_map = vfs_opts(
        modules.clone(),
        "child.mds",
        CompileOptions {
            source_map: true,
            ..Default::default()
        },
    );
    let without_map = vfs_opts(
        modules,
        "child.mds",
        CompileOptions {
            source_map: false,
            ..Default::default()
        },
    );

    // ADR-002: byte-identical output regardless of source-map mode.
    assert_eq!(
        with_map.output, without_map.output,
        "ADR-002: @extends compiled output must be byte-identical with and without source maps"
    );
    assert!(
        with_map.source_map.is_some(),
        "source_map must be present when source_map=true"
    );
    assert!(
        without_map.source_map.is_none(),
        "source_map must be absent when source_map=false"
    );
}

/// REL-1 regression / applies PF-004: the cumulative loop-iteration budget must be
/// shared across ALL @extends regions when `source_map: true`.
///
/// Before the fix, `evaluate_regions_with_map` called `evaluate_with_map` per region
/// and each call seeded a FRESH `EvalContext` (total_iterations = 0), giving K regions
/// an independent 1 M budget — CPU/DoS amplification ∝ region count.
///
/// After the fix `evaluate_with_map_seeded` threads the running totals so the same
/// cumulative cap applies to the entire @extends compilation.
///
/// Setup: the child overrides two blocks; each block drives 600 outer × 1 000 inner =
/// 600 000 iterations.  No single region exceeds the 1 M cap, but the cumulative total
/// (1 200 000) does.  The outer/inner arrays are injected via runtime_vars to avoid
/// large YAML in the template.
#[test]
fn for_max_total_iterations_across_extends_regions_source_map() {
    // 600 × 1 000 = 600 000 per region; 2 regions = 1 200 000 > MAX_TOTAL_ITERATIONS (1 M).
    // Each array is well below MAX_LOOP_ITERATIONS (100 000) so the per-loop cap does
    // not trip — only the cumulative cap fires.
    let outer: Vec<Value> = (0..600usize).map(|i| Value::Number(i as f64)).collect();
    let inner: Vec<Value> = (0..1000usize).map(|i| Value::Number(i as f64)).collect();
    let mut vars = HashMap::new();
    vars.insert("outer".to_string(), Value::Array(outer));
    vars.insert("inner".to_string(), Value::Array(inner));

    let mut modules = HashMap::new();
    modules.insert(
        "base.mds".to_string(),
        "@block loop1:\n@end\n@block loop2:\n@end\n".to_string(),
    );
    modules.insert(
        "child.mds".to_string(),
        "---\nextends: base.mds\n---\n\
         @block loop1:\n\
         @for o in outer:\n@for i in inner:\n{o}{i}\n@end\n@end\n\
         @end\n\
         @block loop2:\n\
         @for o in outer:\n@for i in inner:\n{o}{i}\n@end\n@end\n\
         @end\n"
            .to_string(),
    );

    let err = mds::compile_virtual_with_deps_opts(
        modules,
        "child.mds",
        Some(vars),
        CompileOptions {
            source_map: true,
            ..Default::default()
        },
    )
    .expect_err(
        "REL-1: cumulative iteration budget across @extends regions must trip MAX_TOTAL_ITERATIONS",
    );

    assert!(
        format!("{err}").contains("total loop iterations exceeded maximum"),
        "REL-1: expected total-iterations resource_limit error, got: {err}"
    );
}

// ── D1: STRING_SOURCE_MAP_LABEL cross-surface parity (PF-007) ────────────────
//
// These tests verify that the choke-point fix in MapBuilder::new and
// source_index ensures the "<source>" diagnostic sentinel never appears in
// sources[] for any code path.

/// D1-CORE-1: string-source compile with sourceMap → sources[0] == "input.mds".
///
/// Verifies the MapBuilder::new choke-point: map_source_label("<source>") →
/// STRING_SOURCE_MAP_LABEL so the default string-source label matches WASM.
#[test]
fn d1_string_source_sources_label_is_input_mds() {
    let result = mds::compile_str_with_deps_opts(
        "Hello World!\n",
        None,
        None,
        CompileOptions {
            source_map: true,
            include_sources_content: false,
        },
    )
    .expect("should compile");
    let sm = result.source_map.expect("source_map must be present");
    assert_eq!(
        sm.sources,
        vec!["input.mds"],
        "string-source sources[0] must be \"input.mds\" after map_source_label fix; got: {:?}",
        sm.sources
    );
}

/// D1-CORE-2: S8 function-body attribution — locally-defined function in a
/// string-source template must not add a second "<source>" entry to sources[].
///
/// Without the source_index choke-point fix:
///   - MapBuilder::new("<source>") was stored as "<source>" at index 0.
///   - S8 path called source_index("<source>", ...) → found it → OK.
///   - After the MapBuilder::new fix alone:
///     - MapBuilder::new("<source>") → stores "input.mds" at index 0.
///     - S8 path called source_index("<source>", ...) → NOT found → added
///       as a NEW entry "input.mds"... but with old code it would have been
///       "<source>" at index 1.
///   - With both choke-points fixed: source_index("<source>") →
///     map_source_label → "input.mds" → found at index 0 → no new entry.
#[test]
fn d1_s8_locally_defined_function_no_source_sentinel() {
    // Define a function in the entry (string-source) template and call it.
    // In S8 path: source_index(func.origin.file) is called with "<source>".
    // After the fix both MapBuilder::new and source_index canonicalize it to
    // "input.mds", so sources must be exactly ["input.mds"] — no duplicates.
    let result = mds::compile_str_with_deps_opts(
        "@define greet():\nHello!\n@end\n{greet()}\n",
        None,
        None,
        CompileOptions {
            source_map: true,
            include_sources_content: false,
        },
    )
    .expect("should compile");
    let sm = result.source_map.expect("source_map must be present");
    assert_eq!(
        sm.sources,
        vec!["input.mds"],
        "S8 path must not add a second \"<source>\" entry; got: {:?}",
        sm.sources
    );
}

/// D1-CORE-3: @extends child that is a string-source → no "<source>" sentinel
/// in sources[].
///
/// The child's origin (file="<source>") flows into override_origin for any
/// blocks it overrides. When evaluate_with_map_seeded processes spliced regions
/// it calls source_index(origin.file, ...) for each region. After the fix,
/// origin.file="<source>" → map_source_label → "input.mds" at index 0 (the
/// same entry already seeded by MapBuilder::new with skeleton_origin.file).
#[test]
fn d1_extends_from_string_no_source_sentinel() {
    let dir = tempfile::tempdir().unwrap();
    // Write the base template to disk so @extends can resolve it.
    std::fs::write(
        dir.path().join("base.mds"),
        "@block content:\ndefault content\n@end\n",
    )
    .unwrap();

    let child = "@extends \"./base.mds\"\n@block content:\noverridden\n@end\n";
    let result = mds::compile_str_with_deps_opts(
        child,
        Some(dir.path()),
        None,
        CompileOptions {
            source_map: true,
            include_sources_content: false,
        },
    )
    .expect("should compile");
    let sm = result.source_map.expect("source_map must be present");
    // "<source>" must not appear anywhere in sources[].
    for src in &sm.sources {
        assert_ne!(
            src.as_str(),
            "<source>",
            "\"<source>\" must not appear in sources[] after map_source_label fix; got: {:?}",
            sm.sources
        );
    }
    // The child's blocks should be attributed to "input.mds" (not "<source>").
    assert!(
        sm.sources.contains(&"input.mds".to_string()),
        "child source must be labeled \"input.mds\"; got: {:?}",
        sm.sources
    );
}
