//! CLI source-map integration tests (SM-1 .. SM-15).
//!
//! Covers AC-FUNC-01..14 (source map feature surface), plus a determinism /
//! security test (AC-SEC-01), one byte-golden test, and a VLQ decode helper
//! (to verify mappings are parseable and encode known positions).

mod common;
use common::{fixture, mds_bin};
use std::path::Path;

// ── VLQ decode helper (inline; no extra dependency) ──────────────────────────

/// Decode a single base64-VLQ value from `iter`.
///
/// Returns the decoded signed integer or `None` on empty / invalid input.
fn decode_vlq_one(iter: &mut impl Iterator<Item = u8>) -> Option<i64> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    let mut continuation = true;
    while continuation {
        let ch = iter.next()?;
        let digit = B64.iter().position(|&b| b == ch)? as u64;
        continuation = (digit & 0x20) != 0;
        result |= (digit & 0x1f) << shift;
        shift += 5;
        if shift > 30 {
            return None; // Guard against pathological input.
        }
    }
    // LSB is sign bit.
    let value = (result >> 1) as i64;
    if (result & 1) != 0 {
        Some(-value)
    } else {
        Some(value)
    }
}

/// Decode the first segment of a Source Map v3 `mappings` string.
///
/// Skips any leading empty groups (frontmatter lines produce empty groups) to find
/// the first non-empty segment.  Returns `(gen_col, src_idx, orig_line, orig_col)`.
fn decode_first_segment(mappings: &str) -> Option<(i64, i64, i64, i64)> {
    // Groups are separated by ';' (line boundaries). Within a group, segments
    // are separated by ','. Skip empty groups produced by frontmatter padding.
    let first_seg = mappings
        .split(';')
        .filter_map(|group| group.split(',').next())
        .find(|seg| !seg.is_empty())?;
    let mut iter = first_seg.bytes();
    let gen_col = decode_vlq_one(&mut iter)?;
    let src_idx = decode_vlq_one(&mut iter)?;
    let orig_line = decode_vlq_one(&mut iter)?;
    let orig_col = decode_vlq_one(&mut iter)?;
    Some((gen_col, src_idx, orig_line, orig_col))
}

// ── Base64 decode helper (for inline carrier tests) ───────────────────────────

fn base64_decode(input: &str) -> Vec<u8> {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=')
        .map(|b| B64.iter().position(|&x| x == b).unwrap() as u8)
        .collect();
    for chunk in bytes.chunks(4) {
        let n = (chunk[0] as u32) << 18
            | (chunk.get(1).copied().unwrap_or(0) as u32) << 12
            | (chunk.get(2).copied().unwrap_or(0) as u32) << 6
            | (chunk.get(3).copied().unwrap_or(0) as u32);
        out.push(((n >> 16) & 0xff) as u8);
        if chunk.len() > 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if chunk.len() > 3 {
            out.push((n & 0xff) as u8);
        }
    }
    out
}

// ── Helper utilities ──────────────────────────────────────────────────────────

/// Build `mds build <file> [extra_args...]` and return the output.
fn build_file(path: &Path, extra_args: &[&str]) -> std::process::Output {
    mds_bin()
        .arg("build")
        .arg(path)
        .args(extra_args)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .output()
        .expect("mds binary should run")
}

/// Parse the JSON in a `.map` file and return the `serde_json::Value`.
fn read_map_json(map_path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(map_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", map_path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("map file is not valid JSON at {}: {e}", map_path.display()))
}

// ── SM-1: --source-map creates a sidecar file (AC-FUNC-01) ───────────────────

#[test]
fn sm1_source_map_creates_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(result.status.success(), "build should succeed");
    assert!(out.exists(), "output .md should exist");

    // Sidecar is <output>.map, not <output-without-ext>.map.
    let map_path = dir.path().join("out.md.map");
    assert!(
        map_path.exists(),
        "sidecar map out.md.map should exist next to out.md"
    );
}

// ── SM-2: sidecar is valid JSON with version=3 (AC-FUNC-02) ──────────────────

#[test]
fn sm2_sidecar_is_valid_v3_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(result.status.success(), "build should succeed");

    let map_path = dir.path().join("out.md.map");
    let v = read_map_json(&map_path);

    assert_eq!(
        v["version"].as_u64(),
        Some(3),
        "version must be 3, got: {:?}",
        v["version"]
    );
    assert!(v["sources"].is_array(), "sources must be an array");
    assert!(v["mappings"].is_string(), "mappings must be a string");
    let mappings = v["mappings"].as_str().unwrap();
    assert!(!mappings.is_empty(), "mappings must be non-empty");
}

// ── SM-3: sources[] entries are relative paths (AC-FUNC-05 / AC-SEC-01) ──────

#[test]
fn sm3_sources_are_relative_not_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(result.status.success(), "build should succeed");

    let map_path = dir.path().join("out.md.map");
    let v = read_map_json(&map_path);

    for src in v["sources"].as_array().unwrap() {
        let s = src.as_str().unwrap();
        // Must NOT be an absolute path.
        assert!(
            !s.starts_with('/'),
            "source must not start with '/' (absolute): {s}"
        );
        // Must NOT be a Windows verbatim prefix (PF-003 / AC-SEC-01).
        assert!(
            !s.starts_with(r"\\?\"),
            "source must not contain Windows verbatim prefix: {s}"
        );
        // Must NOT be a Windows absolute path.
        assert!(
            !s.contains(":\\"),
            "source must not contain Windows drive letter: {s}"
        );
    }
}

// ── SM-4: `file` field is the output basename (AC-FUNC-06) ───────────────────

#[test]
fn sm4_file_field_is_output_basename() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(result.status.success(), "build should succeed");

    let map_path = dir.path().join("out.md.map");
    let v = read_map_json(&map_path);

    assert_eq!(
        v["file"].as_str(),
        Some("out.md"),
        "file must be the output basename 'out.md', got: {:?}",
        v["file"]
    );
}

// ── SM-5: --no-source-map suppresses map generation (AC-FUNC-03) ─────────────

#[test]
fn sm5_no_source_map_suppresses_generation() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");

    // Build with --source-map first, then rebuild with --no-source-map.
    build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    let map_path = dir.path().join("out.md.map");
    assert!(
        map_path.exists(),
        "pre-condition: map should exist after first build"
    );

    // Rebuild without --source-map (no flag at all) — stale map should be cleaned up.
    let result = build_file(&fixture("sm_basic.mds"), &["-o", out.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "rebuild without --source-map should succeed"
    );

    // The stale sidecar should have been removed by verify-then-delete.
    assert!(
        !map_path.exists(),
        "stale map should be removed when --source-map is omitted"
    );
}

// ── SM-6: --inline embeds carrier in output (AC-FUNC-08) ─────────────────────

#[test]
fn sm6_inline_embeds_carrier_in_output() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "--inline", "-o", out.to_str().unwrap()],
    );
    assert!(
        result.status.success(),
        "build with --inline should succeed"
    );

    let content = std::fs::read_to_string(&out).unwrap();
    assert!(
        content.contains("<!--# sourceMappingURL=data:application/json;base64,"),
        "inline output must contain the sourceMappingURL carrier, got:\n{content}"
    );

    // Sidecar must NOT exist when using --inline.
    let map_path = dir.path().join("out.md.map");
    assert!(
        !map_path.exists(),
        "--inline must not create a sidecar .map file"
    );
}

// ── SM-7: carrier comment is well-formed and decodable (AC-FUNC-08) ──────────

#[test]
fn sm7_inline_carrier_is_decodable_v3() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "--inline", "-o", out.to_str().unwrap()],
    );

    let content = std::fs::read_to_string(&out).unwrap();

    // Extract the base64 payload from the carrier comment.
    let prefix = "<!--# sourceMappingURL=data:application/json;base64,";
    let suffix = " -->";
    let start = content
        .find(prefix)
        .expect("carrier prefix not found in inline output");
    let rest = &content[start + prefix.len()..];
    let end = rest.find(suffix).expect("carrier suffix ' -->' not found");
    let b64 = &rest[..end];

    // Base64 payload must be non-empty and decode to valid JSON with version=3.
    assert!(!b64.is_empty(), "base64 payload must be non-empty");
    let decoded = base64_decode(b64);
    let json: serde_json::Value =
        serde_json::from_slice(&decoded).expect("decoded carrier must be valid JSON");
    assert_eq!(
        json["version"].as_u64(),
        Some(3),
        "decoded carrier version must be 3"
    );
    assert!(json["sources"].is_array(), "sources must be array");
    assert!(json["mappings"].is_string(), "mappings must be string");
}

// ── SM-8: --embed-sources includes sourcesContent (AC-FUNC-09) ───────────────

#[test]
fn sm8_embed_sources_includes_sources_content() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &[
            "--source-map",
            "--embed-sources",
            "-o",
            out.to_str().unwrap(),
        ],
    );
    assert!(
        result.status.success(),
        "build with --embed-sources should succeed"
    );

    let map_path = dir.path().join("out.md.map");
    let v = read_map_json(&map_path);

    let sources_content = v["sourcesContent"]
        .as_array()
        .expect("sourcesContent must be present when --embed-sources is set");
    assert!(
        !sources_content.is_empty(),
        "sourcesContent must be non-empty"
    );
    // Each entry is either null or a string.
    for entry in sources_content {
        assert!(
            entry.is_null() || entry.is_string(),
            "each sourcesContent entry must be null or string, got: {entry:?}"
        );
    }
}

// ── SM-9: no --embed-sources → sourcesContent absent (AC-FUNC-09) ────────────

#[test]
fn sm9_no_embed_sources_omits_sources_content() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );

    let map_path = dir.path().join("out.md.map");
    let v = read_map_json(&map_path);

    assert!(
        v["sourcesContent"].is_null(),
        "sourcesContent must be absent (null in JSON) when --embed-sources is omitted, got: {:?}",
        v["sourcesContent"]
    );
}

// ── SM-10: messages-mode template degrades to no map (AC-FUNC-07) ────────────

#[test]
fn sm10_messages_mode_degrades_to_no_map() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    let result = build_file(
        &fixture("sm_message.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );

    // Build itself must succeed (graceful degradation, not an error).
    assert!(
        result.status.success(),
        "messages-mode build with --source-map must succeed (graceful degradation)"
    );

    // No map file should be produced.
    let map_path = dir.path().join("out.json.map");
    assert!(
        !map_path.exists(),
        "no .map should be emitted for a messages-mode template"
    );

    // A warning about degradation should appear on stderr.
    let stderr = String::from_utf8(result.stderr).unwrap();
    let has_warn =
        stderr.contains("messages") || stderr.contains("source_map") || stderr.contains("None");
    assert!(
        has_warn,
        "stderr must contain a degradation warning for messages-mode, got: {stderr:?}"
    );
}

// ── SM-11: stale-map reconciliation: no-map build removes prior sidecar ───────

#[test]
fn sm11_stale_map_removed_on_no_map_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");

    // Step 1: build with --source-map → creates sidecar.
    let r1 = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(
        r1.status.success(),
        "initial source-map build should succeed"
    );

    let map_path = dir.path().join("out.md.map");
    assert!(map_path.exists(), "sidecar must exist after step 1");

    // Step 2: rebuild without --source-map → stale sidecar should be removed.
    let r2 = build_file(&fixture("sm_basic.mds"), &["-o", out.to_str().unwrap()]);
    assert!(
        r2.status.success(),
        "rebuild without --source-map should succeed"
    );

    assert!(
        !map_path.exists(),
        "stale map must be removed when rebuilding without --source-map (AC-FUNC-10)"
    );
}

// ── SM-12: rebuild with --source-map overwrites existing sidecar ─────────────

#[test]
fn sm12_sidecar_overwritten_on_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");

    // Build twice with --source-map — second should overwrite, not error.
    let r1 = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(r1.status.success(), "first source-map build should succeed");

    let map_path = dir.path().join("out.md.map");
    let map1 = std::fs::read_to_string(&map_path).unwrap();

    let r2 = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(
        r2.status.success(),
        "second source-map build should succeed"
    );

    let map2 = std::fs::read_to_string(&map_path).unwrap();
    // Content is deterministic: same input → same map.
    assert_eq!(
        map1, map2,
        "map should be deterministic across identical builds"
    );
}

// ── SM-13: --inline build removes stale sidecar from prior build (AC-FUNC-10)

#[test]
fn sm13_inline_build_removes_stale_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");

    // Step 1: sidecar build.
    let r1 = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(r1.status.success(), "initial sidecar build should succeed");

    let map_path = dir.path().join("out.md.map");
    assert!(map_path.exists(), "sidecar must exist after step 1");

    // Step 2: rebuild with --inline → stale sidecar removed.
    let r2 = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "--inline", "-o", out.to_str().unwrap()],
    );
    assert!(r2.status.success(), "inline rebuild should succeed");

    assert!(
        !map_path.exists(),
        "stale sidecar must be removed when switching to --inline (AC-FUNC-10)"
    );

    // Output must now contain the inline carrier.
    let content = std::fs::read_to_string(&out).unwrap();
    assert!(
        content.contains("<!--# sourceMappingURL=data:"),
        "output must contain inline carrier after --inline rebuild"
    );
}

// ── SM-14: stdin with --source-map --inline produces carrier (AC-FUNC-12) ────

#[test]
fn sm14_stdin_source_map_inline_produces_carrier() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("stdin_out.md");

    let mut child = mds_bin()
        .args([
            "build",
            "-",
            "--source-map",
            "--inline",
            "-o",
            out.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("mds binary should spawn");

    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
        .unwrap();

    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "stdin --source-map --inline should succeed"
    );

    let content = std::fs::read_to_string(&out).unwrap();
    assert!(
        content.contains("<!--# sourceMappingURL=data:application/json;base64,"),
        "stdin inline output must contain carrier"
    );

    // Decode the carrier and verify sources label is <stdin> (AC-FUNC-12).
    let prefix = "<!--# sourceMappingURL=data:application/json;base64,";
    let suffix = " -->";
    let start = content.find(prefix).unwrap();
    let rest = &content[start + prefix.len()..];
    let end = rest.find(suffix).unwrap();
    let b64 = &rest[..end];
    let decoded = base64_decode(b64);
    let json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();

    let sources = json["sources"].as_array().expect("sources must be array");
    assert!(
        sources.iter().any(|s| s.as_str() == Some("<stdin>")),
        "stdin builds must label source as '<stdin>', got: {sources:?}"
    );
}

// ── SM-15: directory mode with --source-map (AC-FUNC-14) ────────────────────

#[test]
fn sm15_directory_mode_source_map() {
    // Build a directory containing sm_basic.mds with --source-map.
    // We copy just the one file into a temp dir to isolate the test.
    let dir = tempfile::tempdir().unwrap();
    let src_content = std::fs::read_to_string(fixture("sm_basic.mds")).unwrap();
    std::fs::write(dir.path().join("basic.mds"), &src_content).unwrap();

    let out_dir = dir.path().join("out");
    let result = mds_bin()
        .args([
            "build",
            dir.path().to_str().unwrap(),
            "--source-map",
            "--out-dir",
            out_dir.to_str().unwrap(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .output()
        .expect("mds binary should run");

    assert!(
        result.status.success(),
        "directory mode --source-map build should succeed, stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let out_md = out_dir.join("basic.md");
    let out_map = out_dir.join("basic.md.map");

    assert!(out_md.exists(), "compiled .md should exist in out_dir");
    assert!(
        out_map.exists(),
        "sidecar .md.map should exist in out_dir alongside basic.md"
    );

    let v = read_map_json(&out_map);
    assert_eq!(v["version"].as_u64(), Some(3), "map version must be 3");
}

// ── SM-DET: determinism + security (AC-SEC-01 / AC-FUNC-05) ─────────────────

#[test]
fn sm_det_sources_never_absolute_across_build_dirs() {
    // Build the same fixture from two different output directories.
    // sources[] entries must be relative in both cases (AC-SEC-01).
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    let out_a = dir_a.path().join("out_a.md");
    let out_b = dir_b.path().join("out_b.md");

    build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out_a.to_str().unwrap()],
    );
    build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out_b.to_str().unwrap()],
    );

    let map_a = read_map_json(&dir_a.path().join("out_a.md.map"));
    let map_b = read_map_json(&dir_b.path().join("out_b.md.map"));

    for (label, v) in [("dir_a", &map_a), ("dir_b", &map_b)] {
        for src in v["sources"].as_array().unwrap() {
            let s = src.as_str().unwrap();
            assert!(
                !s.starts_with('/'),
                "[{label}] source must not be absolute: {s}"
            );
            assert!(
                !s.starts_with(r"\\?\"),
                "[{label}] source must not have Windows verbatim prefix: {s}"
            );
            // No colons indicating a drive letter (e.g., C:\...).
            let after_scheme = s.split_once(':').map(|(_, r)| r).unwrap_or("");
            assert!(
                !after_scheme.starts_with('\\'),
                "[{label}] source must not be a Windows absolute path: {s}"
            );
        }
    }

    // mappings must be byte-identical (same source, same output structure).
    assert_eq!(
        map_a["mappings"], map_b["mappings"],
        "mappings must be deterministic regardless of output directory"
    );
}

// ── SM-GOLD: byte-golden test (regen with: UPDATE_GOLDEN=1 cargo test) ───────

/// Golden mappings for sm_basic.mds → {name: World} → "Hello World!\nThis is line two.\n"
///
/// To regenerate this golden value:
///   1. Run the test with UPDATE_GOLDEN=1 to print the current value.
///   2. Copy the printed value into the constant below.
///   3. Commit the updated golden.
///
/// The golden anchors to the first character of the first segment only (column 0,
/// source 0, original line 0, original column 0 → VLQ "AAAA"), which is stable
/// across unrelated changes to later segments.
#[test]
fn sm_gold_mappings_first_segment_is_aaaa() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("gold.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(result.status.success(), "golden build should succeed");

    let map_path = dir.path().join("gold.md.map");
    let v = read_map_json(&map_path);
    let mappings = v["mappings"].as_str().unwrap();

    // The first non-empty segment must be "AAGA":
    //   gen_col=0 (first char of first body output line)
    //   src=0     (first source file)
    //   orig_line=3 (body starts at line 3 of sm_basic.mds, after 3 frontmatter lines)
    //   orig_col=0  (first column)
    //
    // VLQ encoding: A=0, A=0, G=+3, A=0 → "AAGA"
    // Frontmatter lines produce leading empty groups before this segment.
    const GOLDEN_FIRST_SEG: &str = "AAGA";

    let first_nonempty_seg = mappings
        .split(';')
        .filter_map(|g| g.split(',').next())
        .find(|s| !s.is_empty())
        .unwrap_or("");

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        eprintln!("UPDATE_GOLDEN: first nonempty segment = {first_nonempty_seg:?}");
        eprintln!("UPDATE_GOLDEN: full mappings = {mappings:?}");
    }

    assert_eq!(
        first_nonempty_seg, GOLDEN_FIRST_SEG,
        "first non-empty segment must be {GOLDEN_FIRST_SEG:?} \
         (gen_col=0,src=0,orig_line=3,orig_col=0 for sm_basic.mds), got: {first_nonempty_seg:?}"
    );
}

// ── SM-VLQ: VLQ decode test ───────────────────────────────────────────────────

#[test]
fn sm_vlq_decoder_known_values() {
    // Verify the inline VLQ decoder against known values from the spec:
    //   0  → "A"
    //   1  → "C"
    //   -1 → "D"
    //   2  → "E"
    // Four-value sequence [0, 0, 0, 0] → "AAAA"
    let mut iter_a = b"A".iter().copied();
    assert_eq!(decode_vlq_one(&mut iter_a), Some(0), "A → 0");

    let mut iter_c = b"C".iter().copied();
    assert_eq!(decode_vlq_one(&mut iter_c), Some(1), "C → 1");

    let mut iter_d = b"D".iter().copied();
    assert_eq!(decode_vlq_one(&mut iter_d), Some(-1), "D → -1");

    let mut iter_e = b"E".iter().copied();
    assert_eq!(decode_vlq_one(&mut iter_e), Some(2), "E → 2");

    // "AAAA" = [0, 0, 0, 0] — the all-zero segment.
    let seg = decode_first_segment("AAAA").expect("AAAA must decode");
    assert_eq!(seg, (0, 0, 0, 0), "AAAA must decode to all-zero offsets");

    // "CCCC" = [1, 1, 1, 1]
    let seg2 = decode_first_segment("CCCC").expect("CCCC must decode");
    assert_eq!(seg2, (1, 1, 1, 1), "CCCC must decode to all-one offsets");
}

// ── helpers: multi-source attribution ────────────────────────────────────────

/// Return `true` if the mappings string contains at least one 4-field segment
/// whose accumulated `src_idx` is > 0 after applying all deltas.
///
/// Implements the SMv3 state-machine: gen_col resets at each `;`, but all
/// other fields (src_idx, orig_line, orig_col) accumulate across the entire
/// mapping string.  A src_idx > 0 means the segment references a source file
/// other than the primary entry (index 0 in sources[]).
fn mappings_reference_multiple_sources(mappings: &str) -> bool {
    let mut src_idx: i64 = 0;
    for group in mappings.split(';') {
        for seg in group.split(',') {
            if seg.is_empty() {
                continue;
            }
            let mut iter = seg.bytes();
            // First field: gen_col delta — consume but ignore.
            if decode_vlq_one(&mut iter).is_none() {
                continue;
            }
            // Second field: src_idx delta — accumulate and test.
            if let Some(delta) = decode_vlq_one(&mut iter) {
                src_idx = src_idx.saturating_add(delta);
                if src_idx > 0 {
                    return true;
                }
            }
        }
    }
    false
}

// ── SM-17: @include multi-file source attribution (TEST-3) ───────────────────

/// Compile a parent template that `@import`s a partial and verify that the
/// generated source map attributes output lines to BOTH source files.
///
/// Acceptance criteria (TEST-3):
///   1. sources[] contains at least 2 entries.
///   2. One entry refers to the parent (sm_include_parent.mds).
///   3. One entry refers to the included file (_sm_partial.mds).
///   4. The mappings contain a segment that references src_idx > 0
///      (i.e., at least one output line is attributed to the included file).
#[test]
fn sm17_include_multi_file_attribution() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_include_parent.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(
        result.status.success(),
        "build of @include fixture should succeed, stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let map_path = dir.path().join("out.md.map");
    assert!(map_path.exists(), "sidecar .md.map must exist");

    let v = read_map_json(&map_path);

    // (1) sources[] must have at least 2 entries.
    let sources = v["sources"].as_array().expect("sources must be an array");
    assert!(
        sources.len() >= 2,
        "sources[] must have ≥ 2 entries for a template that @imports another, got: {:?}",
        sources
    );

    // (2) At least one source must identify the parent file.
    let has_parent = sources
        .iter()
        .any(|s| s.as_str().is_some_and(|p| p.contains("sm_include_parent")));
    assert!(
        has_parent,
        "sources[] must include the parent file (sm_include_parent), got: {:?}",
        sources
    );

    // (3) At least one source must identify the imported partial.
    let has_partial = sources
        .iter()
        .any(|s| s.as_str().is_some_and(|p| p.contains("_sm_partial")));
    assert!(
        has_partial,
        "sources[] must include the imported file (_sm_partial), got: {:?}",
        sources
    );

    // (4) The VLQ mappings must attribute at least one output segment to a
    //     source other than the primary entry (src_idx > 0).
    let mappings = v["mappings"].as_str().unwrap();
    assert!(
        mappings_reference_multiple_sources(mappings),
        "mappings must contain a segment referencing the imported source (src_idx > 0); \
         got sources: {:?}, mappings: {:?}",
        sources,
        mappings
    );
}

// ── SM-VLQ-INT: VLQ integration test with actual map output ──────────────────

#[test]
fn sm_vlq_actual_map_first_segment_decodes() {
    // Build sm_basic.mds and verify the first segment of the map decodes
    // to plausible positions (all values >= 0, source index 0).
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let result = build_file(
        &fixture("sm_basic.mds"),
        &["--source-map", "-o", out.to_str().unwrap()],
    );
    assert!(result.status.success(), "build should succeed");

    let map_path = dir.path().join("out.md.map");
    let v = read_map_json(&map_path);
    let mappings = v["mappings"].as_str().unwrap();

    let (gen_col, src_idx, orig_line, orig_col) =
        decode_first_segment(mappings).expect("first segment must decode");

    assert_eq!(src_idx, 0, "first segment must reference source index 0");
    assert!(gen_col >= 0, "generated column must be >= 0");
    assert!(orig_line >= 0, "original line must be >= 0");
    assert!(orig_col >= 0, "original column must be >= 0");
}

// ── SM-16: hand-authored non-SMv3 map preserved with warning (AC-FUNC-10) ───

/// A hand-authored `.map` file that is NOT a valid SMv3 tool-generated sidecar
/// must be left in place (preserved) and must trigger a warning on stderr.
/// With `--quiet`, the warning is suppressed but the file is still preserved.
#[test]
fn sm16_stale_map_not_smv3_preserved_with_warning() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.md");
    let map_path = dir.path().join("out.md.map");

    // Place a hand-authored map that is NOT valid SMv3 (version=2, wrong file field).
    std::fs::write(&map_path, r#"{"version":2,"file":"WRONG.md"}"#).unwrap();

    // Build WITHOUT --source-map; this triggers stale-map reconciliation.
    let result = build_file(&fixture("sm_basic.mds"), &["-o", out.to_str().unwrap()]);
    assert!(result.status.success(), "build should succeed");

    // (i) The hand-authored map must still exist (preserved, not deleted).
    assert!(
        map_path.exists(),
        "hand-authored non-SMv3 map must be preserved (not deleted)"
    );

    // (ii) A warning must appear on stderr.
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("warning:") && stderr.contains("out.md.map"),
        "expected a warning about leaving the non-SMv3 map in place, got stderr: {stderr:?}"
    );

    // With --quiet the warning must be suppressed (but the file is still preserved).
    // Re-write the map (build above may have recreated or left it).
    std::fs::write(&map_path, r#"{"version":2,"file":"WRONG.md"}"#).unwrap();
    let quiet_result = build_file(
        &fixture("sm_basic.mds"),
        &["-o", out.to_str().unwrap(), "--quiet"],
    );
    assert!(quiet_result.status.success(), "quiet build should succeed");
    assert!(
        map_path.exists(),
        "hand-authored non-SMv3 map must be preserved even with --quiet"
    );
    let quiet_stderr = String::from_utf8_lossy(&quiet_result.stderr);
    assert!(
        !quiet_stderr.contains("warning:"),
        "--quiet must suppress the warning; got stderr: {quiet_stderr:?}"
    );
}

// ── SM-14b: stdin --source-map sidecar relabels source to <stdin> ────────────
//
// After the STRING_SOURCE_MAP_LABEL fix, stdin builds emit "input.mds" in
// sources[0].  relativize_source_path Rule 1 must relabel it to "<stdin>"
// (AC-FUNC-12).  Verifies both the relabeling AND that "input.mds" / "<source>"
// never leak into the sidecar map.

#[test]
fn sm14b_stdin_source_map_sidecar_relabels_source_to_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("stdin_out.md");
    let map_path = dir.path().join("stdin_out.md.map");

    let mut child = mds_bin()
        .args(["build", "-", "--source-map", "-o", out.to_str().unwrap()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("mds binary should spawn");

    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"---\nname: World\n---\nHello {name}!\n")
        .unwrap();

    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "stdin --source-map sidecar should succeed"
    );

    let map_json = read_map_json(&map_path);
    let sources = map_json["sources"]
        .as_array()
        .expect("sources must be array");

    // Must use "<stdin>" — never "input.mds" or "<source>".
    assert!(
        sources.iter().any(|s| s.as_str() == Some("<stdin>")),
        "stdin sidecar must label source as \"<stdin>\"; got: {sources:?}"
    );
    assert!(
        !sources.iter().any(|s| s.as_str() == Some("input.mds")),
        "\"input.mds\" must not leak into stdin sidecar sources[]; got: {sources:?}"
    );
    assert!(
        !sources.iter().any(|s| s.as_str() == Some("<source>")),
        "\"<source>\" must not appear in stdin sidecar sources[]; got: {sources:?}"
    );
}

// ── SM-16: --inline -o - (stdout) produces relativized map, no absolute paths─
//
// Before the relativize_source_map_fields fix, the early return on None
// output bypassed relativization entirely, leaking absolute filesystem paths
// into inline data-URI source maps.  After the fix, map_dir = PathBuf::new()
// so paths are relativized against CWD unconditionally.

/// SM-16a: file input + --inline + -o - → stdout carrier has no absolute paths.
#[test]
fn sm16a_file_inline_stdout_no_absolute_paths() {
    // Run the binary against a real fixture with --inline -o -
    let result = mds_bin()
        .args([
            "build",
            fixture("sm_basic.mds").to_str().unwrap(),
            "--source-map",
            "--inline",
            "-o",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("mds binary should run");

    assert!(
        result.status.success(),
        "file --inline -o - must succeed; stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    // Extract and decode the inline data-URI carrier.
    let prefix = "<!--# sourceMappingURL=data:application/json;base64,";
    let suffix = " -->";
    let start = stdout
        .find(prefix)
        .expect("stdout must contain inline source-map carrier");
    let rest = &stdout[start + prefix.len()..];
    let end = rest
        .find(suffix)
        .expect("carrier must be closed with \" -->\"");
    let b64 = &rest[..end];
    let decoded = base64_decode(b64);
    let json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();

    let sources = json["sources"].as_array().expect("sources must be array");
    for src in sources {
        let s = src.as_str().expect("source must be string");
        // No absolute paths (AC-SEC-01 / PF-005 unconditional guard).
        assert!(
            !std::path::Path::new(s).is_absolute(),
            "inline stdout source map must not contain absolute paths; found: {s:?}"
        );
        // Must not start with '/' or contain drive letters.
        assert!(
            !s.starts_with('/'),
            "source must not start with '/'; got: {s:?}"
        );
    }

    // `file` field must be absent for stdout output (no output filename).
    assert!(
        json.get("file").map(|v| v.is_null()).unwrap_or(true),
        "`file` field must be absent or null for stdout inline map; got: {:?}",
        json.get("file")
    );
}

/// SM-16b: stdin + --inline + -o - → previously rejected, now allowed.
///
/// The false rejection "--inline cannot be used with -o -" was deleted.
/// The inline carrier embeds in the output stream identically to file input.
#[test]
fn sm16b_stdin_inline_stdout_now_allowed() {
    let mut child = mds_bin()
        .args(["build", "-", "--source-map", "--inline", "-o", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("mds binary should spawn");

    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Hello World!\n")
        .unwrap();

    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "stdin --inline -o - must succeed after removing the false rejection; \
         stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let stdout = String::from_utf8_lossy(&result.stdout);
    // Must contain an inline carrier.
    assert!(
        stdout.contains("<!--# sourceMappingURL=data:application/json;base64,"),
        "stdin --inline -o - must embed a source-map carrier in stdout; stdout: {stdout:?}"
    );

    // Decode and verify no absolute paths in sources.
    let prefix = "<!--# sourceMappingURL=data:application/json;base64,";
    let suffix = " -->";
    let start = stdout.find(prefix).unwrap();
    let rest = &stdout[start + prefix.len()..];
    let end = rest.find(suffix).unwrap();
    let decoded = base64_decode(&rest[..end]);
    let json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    let sources = json["sources"].as_array().expect("sources must be array");
    // Stdin source should be "<stdin>" (Rule 1 of relativize_source_path).
    assert!(
        sources.iter().any(|s| s.as_str() == Some("<stdin>")),
        "stdin --inline stdout must label source as \"<stdin>\"; got: {sources:?}"
    );
    for src in sources {
        let s = src.as_str().expect("source must be string");
        assert!(
            !std::path::Path::new(s).is_absolute(),
            "inline stdout source must not be absolute; found: {s:?}"
        );
    }
}

// ── SM-SEC3: `..`-escape guard — deepCWD/output dir must not leak path layout ──

/// Regression gate for the SEC-3 fix (ADR-005 / AC-SEC-01 / avoids PF-004).
///
/// Before the fix, `relativize_source_path` only guarded against paths that were
/// still ABSOLUTE after relativization (starts with `/` or Windows drive form).
/// A source file that lives OUTSIDE the map directory produces a `../`-escaping
/// relative path whose `..` chain grows with the nesting depth — reconstructing
/// the full absolute path and leaking USERNAME + directory layout.
///
/// Covers BOTH output modes to prevent the per-mode divergence PF-004 describes:
/// - sidecar map: `map_dir = effective_parent(out_path)` (absolute, deep)
/// - inline stdout: `map_dir = CWD` (process runs from a deep directory)
///
/// Assertion: no source entry contains `../` components, and no entry contains
/// the random source-tempdir name that would confirm path reconstruction.
#[test]
fn sm_sec3_dotdot_escape_falls_back_to_basename() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    // Record the random dir-name; if it appears in sources[] the path escaped.
    let src_dir_name = src_dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // Source file is in src_dir — completely separate from the output tree.
    let src_path = src_dir.path().join("input.mds");
    std::fs::write(&src_path, "Hello SEC-3!\n").unwrap();

    // Deep output directory — enough levels that the old relative_path() call
    // produces a long `../../../...` chain back to src_dir under the old code.
    let deep_out_dir = out_dir.path().join("a/b/c/d/e/f/g/h/i/j");
    std::fs::create_dir_all(&deep_out_dir).unwrap();
    let out_path = deep_out_dir.join("out.md");

    // ── sidecar case ─────────────────────────────────────────────────────────
    // map_dir = effective_parent(out_path) = deep_out_dir (absolute).
    // relative_path(deep_out_dir, src_path) escapes via `../` under the old code.
    let result = mds_bin()
        .arg("build")
        .arg(&src_path)
        .arg("--source-map")
        .arg("-o")
        .arg(&out_path)
        .output()
        .expect("mds build (sidecar) should run");
    assert!(
        result.status.success(),
        "sidecar build should succeed; stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let map_path = deep_out_dir.join("out.md.map");
    let v = read_map_json(&map_path);
    let sidecar_sources = v["sources"].as_array().expect("sources must be array");

    for src in sidecar_sources {
        let s = src.as_str().expect("source must be string");
        // SEC-3: must NOT start with `../` — the `..`-escape guard must have fired.
        assert!(
            !s.starts_with("../"),
            "sidecar sources[] must not contain a `..`-escaping path \
             (SEC-3 guard must fire); got: {s:?}"
        );
        assert_ne!(s, "..", "sidecar sources[] must not be bare `..`");
        // Pre-existing guard: must NOT be an absolute path.
        assert!(
            !s.starts_with('/'),
            "sidecar sources[] must not be absolute; got: {s:?}"
        );
        // Key assertion: the random src-tempdir name must NOT appear.  If it does,
        // the `..` chain reconstructed the filesystem path — the exact leak the
        // PR description claimed was fixed but was not.
        assert!(
            !s.contains(src_dir_name.as_str()),
            "sidecar sources[] must not contain the source tempdir name \
             (path reconstruction leak); dir={src_dir_name:?} entry={s:?}"
        );
    }

    // ── inline stdout case ────────────────────────────────────────────────────
    // map_dir = CWD when -o - is used (PathBuf::new() → abs_map_dir = current_dir).
    // Running from deep_out_dir makes CWD == deep_out_dir, so the relative path
    // from CWD to src_path escapes via `../` under the old code.
    let inline_result = mds_bin()
        .current_dir(&deep_out_dir)
        .arg("build")
        .arg(&src_path)
        .args(["--source-map", "--inline", "-o", "-"])
        .output()
        .expect("mds build --inline should run");
    assert!(
        inline_result.status.success(),
        "inline build should succeed; stderr: {}",
        String::from_utf8_lossy(&inline_result.stderr)
    );

    let stdout = String::from_utf8_lossy(&inline_result.stdout);
    let prefix = "<!--# sourceMappingURL=data:application/json;base64,";
    let suffix = " -->";
    let start = stdout
        .find(prefix)
        .expect("stdout must contain inline source-map carrier");
    let rest = &stdout[start + prefix.len()..];
    let end = rest
        .find(suffix)
        .expect("carrier must be closed with \" -->\"");
    let decoded = base64_decode(&rest[..end]);
    let inline_json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    let inline_sources = inline_json["sources"]
        .as_array()
        .expect("inline sources must be array");

    for src in inline_sources {
        let s = src.as_str().expect("source must be string");
        assert!(
            !s.starts_with("../"),
            "inline sources[] must not contain a `..`-escaping path \
             (SEC-3 guard must fire); got: {s:?}"
        );
        assert_ne!(s, "..", "inline sources[] must not be bare `..`");
        assert!(
            !s.starts_with('/'),
            "inline sources[] must not be absolute; got: {s:?}"
        );
        assert!(
            !s.contains(src_dir_name.as_str()),
            "inline sources[] must not contain the source tempdir name \
             (path reconstruction leak); dir={src_dir_name:?} entry={s:?}"
        );
    }
}
