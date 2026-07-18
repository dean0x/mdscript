//! Source Map v3 generation for MDS compiled output.
//!
//! Provides the [`SourceMap`] type (SMv3 shape) and the pure encoding
//! machinery that later phases thread through the evaluator:
//!
//! - [`vlq_encode`] — hand-rolled Base64-VLQ encoding (zero external deps,
//!   protecting the 800 KiB WASM budget).
//! - [`LineTable`] — byte-offset → `(line, utf16_col)` resolver with an
//!   ASCII fast-path.
//! - [`encode_mappings`] — delta-encodes a sorted list of point mappings
//!   into the SMv3 `mappings` string.
//! - [`SourceMap::from_points`] — ties all three together and assembles
//!   the final struct.
//!
//! # Security invariant
//!
//! The Base64 alphabet used by [`vlq_encode`] is the standard SMv3 alphabet
//! (`A-Z a-z 0-9 + /`). It deliberately excludes `-`, `<`, and `>` — a
//! property that later phases rely on when embedding source maps inside
//! HTML comments (`<!--# sourceMappingURL=... -->`). Do **not** change the
//! alphabet.

use std::sync::Arc;

use serde::Serialize;

// ---------------------------------------------------------------------------
// Origin — source-file provenance for function bodies (S7)
// ---------------------------------------------------------------------------

/// The display name and source bytes that a set of AST node offsets index into.
///
/// Carried by [`FunctionDef`](crate::scope::FunctionDef) (as `origin`) so that
/// source-map recording during function-body evaluation (S8) can attribute output
/// segments to the **defining** file, not the call site.
///
/// Placed here (rather than `resolver.rs`) so [`crate::scope`] can import it
/// without creating a scope → resolver cycle.
///
/// `Clone` = two refcount bumps (`O(1)`).
///
/// # Debug output
///
/// The manual `Debug` impl prints `file` + `source.len()` bytes — **never** the
/// raw source text.  This aligns with the `debug-panics` no-leak rule (source
/// bytes must not appear in panic messages or debug output).
#[derive(Clone)]
pub(crate) struct Origin {
    /// Display name of the file (shown in error messages / source labels).
    pub(crate) file: Arc<str>,
    /// Raw source bytes; AST node offsets are relative to this string.
    pub(crate) source: Arc<str>,
}

impl std::fmt::Debug for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Origin")
            .field("file", &self.file)
            .field("source_len", &self.source.len())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// String-source canonical map label
// ---------------------------------------------------------------------------

/// Canonical `sources[]` label for in-memory (string-source) compilations.
///
/// All paths that produce a [`MapBuilder`] for string-source input converge
/// on [`MapBuilder::new`] or [`MapBuilder::source_index`].  Both choke-points
/// apply [`map_source_label`] so the diagnostic sentinel `"<source>"` can
/// never appear in `sources[]`.
///
/// All binding surfaces (WASM, napi, Python, CLI) that handle string-source
/// compiles must import this constant rather than redeclaring the literal, so
/// cross-surface `sources[0]` parity (PF-007 / AC-API-06) is a compile-time
/// fact rather than a comment-coordinated manual sync.
pub const STRING_SOURCE_MAP_LABEL: &str = "input.mds";

/// Map a raw source file label to its canonical source-map label.
///
/// The diagnostic/cycle-detection sentinel `"<source>"` is remapped to
/// [`STRING_SOURCE_MAP_LABEL`] so that the `sources[]` array in produced
/// source maps is identical across native, WASM, napi, and Python surfaces
/// (fixes the PF-007 cross-surface divergence).
///
/// Applied at BOTH choke-points where new labels enter a [`MapBuilder`]:
/// - [`MapBuilder::new`] (the seed label at index 0), and
/// - [`MapBuilder::source_index`] (before the dedup compare, so `"<source>"`
///   and `"input.mds"` can never coexist as two distinct `sources[]` entries
///   even if S8 or spliced-region paths pass the sentinel separately).
///
/// The literal `"<source>"` is used here rather than `SOURCE_LABEL` from
/// `resolver.rs` to avoid a cross-module dependency.  If the sentinel ever
/// changes, update this function first.
#[inline]
pub(crate) fn map_source_label(name: &str) -> &str {
    if name == "<source>" {
        STRING_SOURCE_MAP_LABEL
    } else {
        name
    }
}

// ---------------------------------------------------------------------------
// Public type
// ---------------------------------------------------------------------------

/// A Source Map v3 document.
///
/// Shape follows the [Source Map v3 / ECMAScript 426] specification.
/// Construct via [`SourceMap::from_points`]; serialize to JSON with
/// [`SourceMap::to_json`].
///
/// Field order matches the SMv3 convention (version → file → sources →
/// sourcesContent → names → mappings); `serde` emits struct fields in
/// declaration order.
///
/// [Source Map v3 / ECMAScript 426]: https://tc39.es/ecma426/
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceMap {
    /// Spec version — always `3`.
    pub version: u8,
    /// Optional name of the generated file (e.g. the compiled `.md` path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Original source file names, parallel to the source index used in
    /// mappings fields.
    pub sources: Vec<String>,
    /// Original source file contents, parallel to `sources`. Embedded inline
    /// so consumers do not need to fetch source files separately.
    #[serde(rename = "sourcesContent", skip_serializing_if = "Option::is_none")]
    pub sources_content: Option<Vec<String>>,
    /// Symbol names referenced by mappings — always `[]` until a later phase
    /// adds symbol-level mapping.
    pub names: Vec<String>,
    /// Base64-VLQ encoded mappings string (SMv3 format).
    pub mappings: String,
}

impl SourceMap {
    /// Build a [`SourceMap`] from raw output byte-offset points.
    ///
    /// Each element of `raw_points` is `(out_byte_offset, src_index,
    /// src_line, src_col)` where:
    ///
    /// - `out_byte_offset` — absolute byte position in `body` (the compiled
    ///   output string) where the mapping starts.
    /// - `src_index` — 0-based index into `sources`.
    /// - `src_line`, `src_col` — 0-based line and column in the source file.
    ///
    /// Points whose `out_byte_offset` does not land on a UTF-8 char boundary
    /// are silently dropped (graceful degradation — never panics).
    ///
    /// This function builds a [`LineTable`] over `body`, resolves each
    /// byte offset to a `(line, utf16_col)` pair, then calls
    /// [`encode_mappings`] on the resolved points.
    pub fn from_points(
        body: &str,
        sources: Vec<String>,
        sources_content: Option<Vec<String>>,
        file: Option<String>,
        raw_points: impl IntoIterator<Item = (usize, u32, u32, u32)>,
    ) -> Self {
        let table = LineTable::new(body);

        let resolved: Vec<(u32, u32, u32, u32, u32)> = raw_points
            .into_iter()
            .filter_map(|(out_byte, src_idx, src_line, src_col)| {
                let (out_line, out_col) = table.resolve(out_byte)?;
                Some((out_line, out_col, src_idx, src_line, src_col))
            })
            .collect();

        let mappings = encode_mappings(resolved);

        SourceMap {
            version: 3,
            file,
            sources,
            sources_content,
            names: vec![],
            mappings,
        }
    }

    /// Serialize this source map to a JSON string.
    ///
    /// Infallible: the struct shape is always JSON-serializable.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("SourceMap is always JSON-serializable")
    }
}

// ---------------------------------------------------------------------------
// LineTable — byte offset → (0-based line, 0-based UTF-16 column)
// ---------------------------------------------------------------------------

/// Precomputed line-start table for resolving absolute byte offsets in a
/// body string to `(line, utf16_col)` pairs.
///
/// Construction is O(n) in body length. Resolution is O(log L) for ASCII
/// lines (binary search only) and O(chars in line) for non-ASCII lines.
///
/// # ASCII fast-path
///
/// For a line containing only ASCII bytes, the UTF-16 column equals the byte
/// offset within the line (no per-char scan required). Per-line ASCII-ness is
/// precomputed at construction time so a large multibyte line referenced by
/// many points does not blow up to O(points × line_len).
pub(crate) struct LineTable<'a> {
    body: &'a str,
    /// Byte offset of the first byte of each line (0-indexed lines).
    line_starts: Vec<usize>,
    /// Per-line ASCII flag, precomputed at construction.
    line_is_ascii: Vec<bool>,
}

impl<'a> LineTable<'a> {
    /// Build a `LineTable` over `body`.
    ///
    /// Splits on `\n`. Defensive against CRLF: `\r` is included in line
    /// content if present; real callers supply CR-stripped bodies per the
    /// interior-verbatim contract (ADR-002), so this branch is rarely taken.
    pub(crate) fn new(body: &'a str) -> Self {
        let bytes = body.as_bytes();
        let mut line_starts: Vec<usize> = vec![0];

        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }

        let line_count = line_starts.len();
        let mut line_is_ascii: Vec<bool> = Vec::with_capacity(line_count);

        for li in 0..line_count {
            let start = line_starts[li];
            // Exclude the trailing '\n' (if any) from the ASCII scan.
            let end = if li + 1 < line_count {
                // line_starts[li+1] points past the '\n', so subtract 1.
                line_starts[li + 1].saturating_sub(1)
            } else {
                body.len()
            };
            let all_ascii = bytes[start..end].iter().all(|b| b.is_ascii());
            line_is_ascii.push(all_ascii);
        }

        LineTable {
            body,
            line_starts,
            line_is_ascii,
        }
    }

    /// Resolve an absolute byte offset in `body` to a 0-based
    /// `(line, utf16_col)` pair.
    ///
    /// Returns `None` when `byte_offset` is not on a UTF-8 char boundary
    /// (including offsets past the end of the string). Never panics.
    ///
    /// Columns are UTF-16 code units as required by the SMv3 specification.
    ///
    /// For a pure-ASCII line, UTF-16 column == byte column (O(1) fast-path).
    pub(crate) fn resolve(&self, byte_offset: usize) -> Option<(u32, u32)> {
        // `is_char_boundary` returns false for byte_offset > len, so this
        // single check covers the out-of-bounds case too.
        if !self.body.is_char_boundary(byte_offset) {
            return None;
        }

        // Binary search: find the last line whose start ≤ byte_offset.
        let line = self
            .line_starts
            .partition_point(|&start| start <= byte_offset)
            .saturating_sub(1);

        let line_start = self.line_starts[line];
        let col_byte = byte_offset - line_start;

        let utf16_col: u32 = if self.line_is_ascii[line] {
            // ASCII fast-path: UTF-16 column == byte column (no per-char scan).
            col_byte as u32
        } else {
            // Non-ASCII: count UTF-16 code units from line start to byte_offset.
            let line_slice = &self.body[line_start..byte_offset];
            line_slice.chars().map(|c| c.len_utf16() as u32).sum()
        };

        Some((line as u32, utf16_col))
    }
}

// ---------------------------------------------------------------------------
// VLQ encoding
// ---------------------------------------------------------------------------

/// Base64 alphabet for VLQ encoding (standard Source Map v3).
///
/// Excludes `-`, `<`, `>` by design — see the module-level security note.
const BASE64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append the Base64-VLQ encoding of `value` to `out`.
///
/// Implements signed integer VLQ as used by Source Map v3:
///
/// - Sign encoded in the LSB of the first 5-bit group (bit 0 = 1 → negative).
/// - Groups run from LSB to MSB.
/// - The continuation flag (bit 5, `0x20`) is set on every group except the
///   last; each 6-bit value is indexed into [`BASE64_CHARS`].
pub(crate) fn vlq_encode(value: i64, out: &mut String) {
    // Zigzag encode: positive → 2n (LSB=0), negative → 2|n|+1 (LSB=1 = sign).
    let mut vlq: u64 = if value < 0 {
        ((-value as u64) << 1) | 1
    } else {
        (value as u64) << 1
    };

    loop {
        let mut digit = (vlq & 0x1F) as u8; // take 5 payload bits
        vlq >>= 5;
        if vlq > 0 {
            digit |= 0x20; // set continuation bit
        }
        // SAFETY: digit is in 0..=63, BASE64_CHARS has exactly 64 entries.
        out.push(BASE64_CHARS[digit as usize] as char);
        if vlq == 0 {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Mappings encoder
// ---------------------------------------------------------------------------

/// Encode resolved point mappings into the SMv3 `mappings` string.
///
/// Each point is `(out_line, out_col, src_index, src_line, src_col)`, all
/// 0-based. Points are sorted by `(out_line, out_col)` before encoding.
///
/// Produces 4-field segments (no `names` field, so no 5th VLQ field):
///
/// 1. Generated column — delta from previous segment's genCol within the
///    same output line; resets to absolute (delta from 0) at each new line.
/// 2. Source index — running delta across the whole file.
/// 3. Original line — running delta across the whole file.
/// 4. Original column — running delta across the whole file.
///
/// Segments within a line are comma-separated; lines are semicolon-separated.
/// An output line with no segments contributes an empty string between two
/// semicolons.
pub(crate) fn encode_mappings(mut points: Vec<(u32, u32, u32, u32, u32)>) -> String {
    if points.is_empty() {
        return String::new();
    }

    // Sort by output position (out_line ascending, then out_col ascending).
    points.sort_by_key(|&(ol, oc, _, _, _)| (ol, oc));

    let max_line = points.iter().map(|p| p.0).max().unwrap_or(0);

    let mut out = String::new();
    // srcIdx/srcLine/srcCol are running deltas across the entire file.
    let mut prev_src_idx: i64 = 0;
    let mut prev_src_line: i64 = 0;
    let mut prev_src_col: i64 = 0;

    let mut pi = 0usize; // index into sorted `points`

    for line in 0..=max_line {
        if line > 0 {
            out.push(';');
        }
        // genCol resets to absolute (delta from 0) at each new output line.
        let mut prev_gen_col: i64 = 0;
        let mut first_seg = true;

        // Emit all segments that fall on this output line.
        while pi < points.len() && points[pi].0 == line {
            let (_out_line, out_col, src_idx, src_line, src_col) = points[pi];
            pi += 1;

            if !first_seg {
                out.push(',');
            }
            first_seg = false;

            let d_gen_col = out_col as i64 - prev_gen_col;
            let d_src_idx = src_idx as i64 - prev_src_idx;
            let d_src_line = src_line as i64 - prev_src_line;
            let d_src_col = src_col as i64 - prev_src_col;

            vlq_encode(d_gen_col, &mut out);
            vlq_encode(d_src_idx, &mut out);
            vlq_encode(d_src_line, &mut out);
            vlq_encode(d_src_col, &mut out);

            prev_gen_col = out_col as i64;
            prev_src_idx = src_idx as i64;
            prev_src_line = src_line as i64;
            prev_src_col = src_col as i64;
        }
    }

    out
}

// ---------------------------------------------------------------------------
// CompileOptions
// ---------------------------------------------------------------------------

/// Options that control optional compilation features.
///
/// Passed into the opts-bearing compile entry points
/// (`compile_with_deps_opts`, `compile_str_with_deps_opts`,
/// `compile_virtual_with_deps_opts`) and threaded through the resolver and
/// evaluator.
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Generate a [`SourceMap`] and attach it to [`crate::CompileResult::source_map`].
    ///
    /// When `false` (the default) no [`MapBuilder`] is allocated — zero overhead
    /// for callers that do not need mapping data (AC-PERF-01).
    pub source_map: bool,
    /// Include source file contents in the `sourcesContent` array.
    ///
    /// When `false` (the default) `sourcesContent` is omitted from the map, saving
    /// space for callers that do not need embedded sources (CLI default unless
    /// `--embed-sources` is passed).
    pub include_sources_content: bool,
}

/// Error returned by [`CompileOptions::validate`] when the field combination is invalid.
///
/// Each binding maps this to its own error type and message; the unit struct intentionally
/// carries no context — the per-binding wording is always determined at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidOptionsError;

impl CompileOptions {
    /// Enforce the cross-field invariant: `include_sources_content` requires `source_map`.
    ///
    /// Returns `Err(InvalidOptionsError)` when the combination is invalid.  Each binding
    /// is responsible for converting the failure into its own error type and message,
    /// preserving the existing `mds::invalid_options` code and binding-appropriate wording.
    ///
    /// avoids PF-004/PF-005: this is the single enforcement point; no binding can
    /// silently diverge on the rule even when message wording legitimately differs
    /// (napi/wasm use camelCase; Python uses snake_case + `True`).
    pub fn validate(&self) -> Result<(), InvalidOptionsError> {
        if self.include_sources_content && !self.source_map {
            Err(InvalidOptionsError)
        } else {
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// RawSegment — unfinalized segment record
// ---------------------------------------------------------------------------

/// A single raw, unfinalized source-map segment collected during evaluator
/// traversal.
///
/// Offsets are in bytes relative to the raw evaluator output and the original
/// source file; the finalization pipeline converts them to the 0-based
/// line/column deltas the SMv3 `mappings` field requires.
///
/// Fixed size: 16 bytes (4 × `u32`). The segment vector is capped at
/// [`crate::limits::MAX_SOURCEMAP_SEGMENTS`] to bound memory use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawSegment {
    /// Absolute byte offset of this segment's start in the compiled output
    /// (relative to the raw evaluator output, before `clean_output`).
    pub(crate) out: u32,
    /// 0-based source index into [`MapBuilder::sources`].
    pub(crate) src: u32,
    /// Byte offset of the corresponding token in the source file.
    pub(crate) src_off: u32,
    /// Byte length of the source token.  Used only for range validation;
    /// source maps encode start points only (no length field in VLQ).
    pub(crate) len: u32,
}

// ---------------------------------------------------------------------------
// MapBuilder — accumulates segments during evaluation
// ---------------------------------------------------------------------------

/// Accumulates [`RawSegment`] records during evaluator traversal, then
/// finalizes them into a [`SourceMap`] via [`MapBuilder::finalize`].
///
/// # Cursor invariant
///
/// `cursor` must always equal the absolute byte count of compiled output
/// emitted so far (across all `evaluate_nodes` invocations for this
/// compilation). `evaluate_nodes` updates `cursor` after every output-
/// producing node arm. A `debug_assert!` checks the invariant at each
/// leaf-node record point (when `suppress == 0`).
///
/// # Suppression
///
/// `suppress > 0` while inside a `@define` function body that does NOT have
/// a known `Origin` (S3 fallback).  Nodes inside such bodies are not
/// individually mapped; the single `Interpolation` point for the call site
/// in the parent output is already recorded.  When the function has an
/// `Origin` (S8 path) suppression is skipped and the body is recorded
/// directly with `current_src` switched to the definition file.
pub(crate) struct MapBuilder {
    /// Accumulated raw segment records.
    pub(crate) segments: Vec<RawSegment>,
    /// Current absolute byte position in the compiled output.
    pub(crate) cursor: u32,
    /// Suppression depth: >0 means we are inside a function body (S3 path).
    pub(crate) suppress: u32,
    /// Index of the source file currently being recorded (0-based into `sources`).
    pub(crate) current_src: u32,
    /// Source file names, in registration order (parallel to `sources_content`).
    pub(crate) sources: Vec<String>,
    /// Source file contents, parallel to `sources` (for `sourcesContent`).
    pub(crate) sources_content: Vec<String>,
    /// True when at least one segment was silently dropped due to the
    /// [`crate::limits::MAX_SOURCEMAP_SEGMENTS`] cap (AC-PERF-03).
    ///
    /// When true, the caller should degrade to `source_map: None` + warning
    /// rather than emitting a partial map.
    pub(crate) segments_dropped: bool,
    /// When true, [`MapBuilder::finalize`] omits `sourcesContent` from the
    /// emitted [`SourceMap`] (AC-SEC-04 ceiling degradation).
    ///
    /// Set by the caller before calling `finalize` when the total embedded
    /// source bytes exceed [`crate::limits::MAX_SOURCES_CONTENT_BYTES`].
    pub(crate) no_sources_content: bool,
}

impl MapBuilder {
    /// Create a builder seeded with a single source file.
    ///
    /// The source file at index 0 is used for all segments until
    /// [`source_index`] registers additional sources (e.g. for `@extends`
    /// base templates in CP3+).
    pub(crate) fn new(source_name: String, source_content: String) -> Self {
        // Canonicalize the label at the choke-point: "<source>" (the diagnostic
        // sentinel for string-source compiles) becomes STRING_SOURCE_MAP_LABEL.
        let canonical = map_source_label(&source_name).to_string();
        Self {
            segments: Vec::new(),
            cursor: 0,
            suppress: 0,
            current_src: 0,
            sources: vec![canonical],
            sources_content: vec![source_content],
            segments_dropped: false,
            no_sources_content: false,
        }
    }

    /// Return the index for `file`, registering it as a new source if needed.
    ///
    /// Scans linearly (sources vecs are small — typically 1-3 entries per
    /// single-file compilation).
    pub(crate) fn source_index(&mut self, file: &str, content: &str) -> u32 {
        // Apply the canonical label BEFORE the dedup compare so that "<source>"
        // and "input.mds" can never coexist as two distinct entries (e.g. when
        // S8 function-body attribution passes the sentinel after the seed is
        // already "input.mds").
        let canonical = map_source_label(file);
        if let Some(pos) = self.sources.iter().position(|s| s == canonical) {
            return pos as u32;
        }
        let idx = self.sources.len() as u32;
        self.sources.push(canonical.to_string());
        self.sources_content.push(content.to_string());
        idx
    }

    /// Total byte size of all registered `sourcesContent` strings.
    ///
    /// Called by the resolver before [`finalize`][Self::finalize] to check
    /// whether the AC-SEC-04 ceiling is exceeded.  Uses `sources_content`
    /// (the builder's internal vec) rather than the finalized struct, so the
    /// check can gate the degradation flag before any allocation.
    pub(crate) fn sources_content_bytes(&self) -> usize {
        self.sources_content.iter().map(|s| s.len()).sum()
    }

    /// Push a new segment, capping at [`crate::limits::MAX_SOURCEMAP_SEGMENTS`].
    ///
    /// Segments beyond the cap are silently dropped so compilation succeeds
    /// with a partial map rather than erroring on adversarial inputs.
    /// Sets [`Self::segments_dropped`] when the cap is first hit so callers
    /// can degrade to `source_map: None` + warning (AC-PERF-03).
    pub(crate) fn push_segment(&mut self, out: u32, src_off: u32, len: u32) {
        let src = self.current_src;
        self.push_raw(out, src, src_off, len);
    }

    /// Push a segment with an explicit source index, bypassing `current_src`.
    ///
    /// Used by the `@include` splice path (S6) to insert rebased [`FragmentMap`]
    /// segments with foreign source indices.  Subject to the same
    /// [`crate::limits::MAX_SOURCEMAP_SEGMENTS`] cap as [`push_segment`].
    /// Sets [`Self::segments_dropped`] when the cap is first hit (AC-PERF-03).
    pub(crate) fn push_fragment_segment(&mut self, out: u32, src: u32, src_off: u32, len: u32) {
        self.push_raw(out, src, src_off, len);
    }

    /// Inner push: enforces the segment cap and the debug invariant.
    fn push_raw(&mut self, out: u32, src: u32, src_off: u32, len: u32) {
        debug_assert!(
            self.segments.len() <= crate::limits::MAX_SOURCEMAP_SEGMENTS,
            "segments.len() {} exceeds cap {}; segments_dropped should be set",
            self.segments.len(),
            crate::limits::MAX_SOURCEMAP_SEGMENTS,
        );
        if self.segments.len() < crate::limits::MAX_SOURCEMAP_SEGMENTS {
            self.segments.push(RawSegment {
                out,
                src,
                src_off,
                len,
            });
        } else {
            self.segments_dropped = true;
        }
    }

    /// Finalize into a [`SourceMap`], consuming the builder.
    ///
    /// Runs the 5-stage pipeline:
    /// 1. `expand_per_line` — resolve source-side byte offsets to `(line, col)`.
    /// 2. `compensate_cr` — subtract `\r` count from output offsets.
    /// 3. `clamp_trailing_trim` — drop segments in the trailing-trimmed suffix.
    /// 4. `shift_frontmatter` — shift output offsets by frontmatter prefix length.
    /// 5. `encode_vlq` — call [`SourceMap::from_points`].
    ///
    /// # Parameters
    ///
    /// - `body_raw` — pre-`clean_output` raw evaluator output; segment `out`
    ///   values are byte offsets into this string.
    /// - `final_body` — fully finalized output (after `clean_output` and
    ///   `prepend_frontmatter`); passed to `from_points` for output-side line
    ///   resolution.
    /// - `fm_prefix_len` — byte length of the frontmatter prefix in
    ///   `final_body` (`0` when there is no frontmatter).
    /// - `file` — optional SMv3 `file` field (name of the generated file).
    pub(crate) fn finalize(
        self,
        body_raw: &str,
        final_body: &str,
        fm_prefix_len: usize,
        file: Option<String>,
    ) -> SourceMap {
        let body_clean_len = final_body.len() - fm_prefix_len;

        // Destructure to allow independent moves/borrows of each field.
        let MapBuilder {
            segments,
            sources,
            sources_content,
            no_sources_content,
            ..
        } = self;

        // Stage 1: resolve source-side byte offsets to (line, col).
        // `sources_content` is needed here for LineTable resolution even when
        // AC-SEC-04 degradation drops it from the final artifact.
        // debug_assert: the segment count must never exceed the cap (AC-PERF-03).
        debug_assert!(
            segments.len() <= crate::limits::MAX_SOURCEMAP_SEGMENTS,
            "segments.len() {} exceeds cap at finalize; segments_dropped should have been set",
            segments.len(),
        );
        let points = expand_per_line(segments, &sources_content);
        // Stage 2: adjust output offsets for \r stripping.
        let points = compensate_cr(points, body_raw);
        // Stage 3: drop segments beyond the trailing-trim boundary.
        let points = clamp_trailing_trim(points, body_clean_len);

        // AC-SEC-04: honour the ceiling flag set by the caller.
        // Only omit sourcesContent from the final artifact — resolution above
        // already used it to expand segments to (line, col) form.
        let opt_sources_content = if no_sources_content {
            None
        } else {
            Some(sources_content)
        };

        // Empty body: return a SourceMap with an empty mappings string.
        if body_clean_len == 0 {
            return SourceMap {
                version: 3,
                file,
                sources,
                sources_content: opt_sources_content,
                names: vec![],
                mappings: String::new(),
            };
        }

        // Stage 4: shift output offsets by frontmatter prefix length.
        let points = shift_frontmatter(points, fm_prefix_len);
        // Stage 5: encode into a SourceMap via from_points.
        encode_vlq(points, final_body, sources, opt_sources_content, file)
    }
}

// ---------------------------------------------------------------------------
// FragmentMap — pre-computed source attribution for an imported module
// ---------------------------------------------------------------------------

/// A pre-computed source-map fragment for an imported module's `prompt` body.
///
/// Computed once when a module is resolved in source-map mode (see
/// `process_module` in `resolver.rs`) and cached inside
/// `ResolvedModule` / [`crate::scope::NamespaceScope`] behind an `Arc` so
/// every `@include` call-site can share the same allocation.
///
/// # Local coordinate space
///
/// `sources` is a **local interner** — source indices in `segments` are
/// 0-based into THIS vector and must be remapped to the global
/// [`MapBuilder`] source indices before splicing (see `evaluate_include`
/// in `evaluator.rs`).
///
/// Segment `out` offsets are **0-based byte offsets within the module's
/// own raw evaluator output** (the `prompt_body` string before
/// `clean_output`).  During splicing the caller adds a `base` offset to
/// rebase them into the global output stream.
///
/// # Nested composition
///
/// When module A `@include`s module B, and B itself `@include`s module C,
/// B's `FragmentMap` is built by running [`evaluate_with_map`] over B's
/// body.  That evaluation calls `evaluate_include` for `@include C`, which
/// splices C's `FragmentMap` into B's local builder — so B's `FragmentMap`
/// already contains segments attributed to C's source file.  When A later
/// splices B, the three-source attribution comes along for free.
#[derive(Debug)]
pub(crate) struct FragmentMap {
    /// Local source interner: `(display_path, raw_content)` pairs.
    ///
    /// Index 0 is always the module's own file.  Additional entries appear
    /// when the module itself `@include`s nested partials.
    pub(crate) sources: Vec<(std::sync::Arc<str>, std::sync::Arc<str>)>,
    /// Raw segments local to this module's prompt body.
    pub(crate) segments: Vec<RawSegment>,
}

// ---------------------------------------------------------------------------
// rebase_trim — adjust body segments after .trim() (S8)
// ---------------------------------------------------------------------------

/// Rebase function-body segments after `.trim()` is applied to the raw body output.
///
/// Called by `invoke_function` (S8 path) immediately after the body's
/// `evaluate_nodes` returns and before the trimmed string is returned to the
/// caller.  The function:
///
/// 1. **Drops** segments whose `out` offset falls entirely within the leading
///    `[start_cursor, start_cursor + lead)` or trailing
///    `[start_cursor + untrimmed_len − trail, start_cursor + untrimmed_len)`
///    trimmed regions.
/// 2. **Shifts** every surviving segment's `out` back by `lead` so that output
///    offsets are relative to the trimmed body's start position.
/// 3. **Truncates** `segments` to the survivors.
///
/// After this call, `segments[seg_start..]` contains exactly the segments that
/// map into the trimmed body, with `out` values adjusted to the caller's output
/// coordinate space (`start_cursor` is the absolute position where the trimmed
/// body will appear in the parent output).
///
/// # Arguments
///
/// - `segments` — the full segment `Vec`; elements before `seg_start` are not
///   touched.
/// - `seg_start` — first segment index belonging to this function body.
/// - `start_cursor` — absolute output position where the body started (before
///   trimming).
/// - `lead` — byte count stripped from the front of the raw body.
/// - `untrimmed_len` — byte length of the raw (untrimmed) body output.
/// - `trail` — byte count stripped from the back of the raw body.
pub(crate) fn rebase_trim(
    segments: &mut Vec<RawSegment>,
    seg_start: usize,
    start_cursor: u32,
    lead: u32,
    untrimmed_len: u32,
    trail: u32,
) {
    if seg_start >= segments.len() {
        return; // Nothing to do.
    }

    // The kept region of the raw body output is [lead_end, trail_start).
    let lead_end: u32 = start_cursor.saturating_add(lead);
    let trail_start: u32 = start_cursor
        .saturating_add(untrimmed_len)
        .saturating_sub(trail);

    let mut write = seg_start;
    for i in seg_start..segments.len() {
        let seg = segments[i];
        // Drop segments that fall in the leading or trailing trimmed zones.
        if seg.out < lead_end || seg.out >= trail_start {
            continue;
        }
        // Shift out back by `lead` so it is relative to the trimmed start.
        segments[write] = RawSegment {
            out: seg.out - lead,
            ..seg
        };
        write += 1;
    }
    segments.truncate(write);
}

// ---------------------------------------------------------------------------
// Finalization stages (pub(crate) for unit testing)
// ---------------------------------------------------------------------------

/// Stage 1 — Resolve source-side byte offsets to `(line, col)` pairs.
///
/// For each [`RawSegment`], looks up `(src_line, src_col)` in the source file
/// at index `seg.src` by building a [`LineTable`] and calling `.resolve()` on
/// `seg.src_off`. Segments where `resolve` returns `None` (i.e. `src_off` is
/// not on a UTF-8 char boundary) are silently dropped.
///
/// Returns `Vec<(out, src_index, src_line, src_col)>` — all 0-based.
pub(crate) fn expand_per_line(
    segments: Vec<RawSegment>,
    sources_content: &[String],
) -> Vec<(u32, u32, u32, u32)> {
    // Build one LineTable per source to amortize construction cost when multiple
    // segments reference the same source file.
    let tables: Vec<LineTable<'_>> = sources_content.iter().map(|s| LineTable::new(s)).collect();

    segments
        .into_iter()
        .filter_map(|seg| {
            let table = tables.get(seg.src as usize)?;
            let (src_line, src_col) = table.resolve(seg.src_off as usize)?;
            Some((seg.out, seg.src, src_line, src_col))
        })
        .collect()
}

/// Stage 2 — Adjust output offsets for `\r` characters stripped by `clean_output`.
///
/// `clean_output` strips every `\r` from the raw evaluator output before
/// calculating the final body. Each `\r` before a segment's `out` offset shifts
/// the segment left by one byte in the cleaned output. This stage counts those
/// `\r`s and subtracts the count from `out`.
///
/// Input/output format: `(out, src_index, src_line, src_col)` — all 0-based.
pub(crate) fn compensate_cr(
    points: Vec<(u32, u32, u32, u32)>,
    body_raw: &str,
) -> Vec<(u32, u32, u32, u32)> {
    let raw_bytes = body_raw.as_bytes();

    // Fast path: no CRs anywhere in the body (the common LF-only case, ADR-002).
    // Avoids both the prefix-sum build and any per-point work.
    if !raw_bytes.contains(&b'\r') {
        return points;
    }

    // Build a prefix-sum table: `cr_prefix[i]` = number of `\r` bytes in
    // `raw_bytes[..i]`.  Length = `raw_bytes.len() + 1` so the lookup
    // `cr_prefix[up_to]` is valid for any `out` in `0..=raw_bytes.len()`.
    //
    // This is O(M) where M = body length, and reduces the per-point work to
    // an O(1) index — total cost O(M + N) vs the previous O(M × N).
    //
    // NOTE: the lookup is order-independent: each point is adjusted using only
    // its own `out` value, so unsorted or out-of-order `out` values (produced
    // by the splice/S8 paths) are handled correctly.
    let mut cr_prefix: Vec<u32> = Vec::with_capacity(raw_bytes.len() + 1);
    cr_prefix.push(0);
    for &b in raw_bytes {
        let prev = *cr_prefix.last().unwrap();
        cr_prefix.push(if b == b'\r' { prev + 1 } else { prev });
    }

    points
        .into_iter()
        .map(|(out, src, src_line, src_col)| {
            let up_to = (out as usize).min(raw_bytes.len());
            let cr_count = cr_prefix[up_to];
            (out - cr_count, src, src_line, src_col)
        })
        .collect()
}

/// Stage 3 — Drop segments that fall in the trailing-trimmed suffix.
///
/// `clean_output` trims all trailing whitespace from the body.  Any segment
/// whose `out` offset (after CR compensation) is ≥ `body_clean_len` maps into
/// the stripped suffix and must be dropped.
///
/// `body_clean_len = final_body.len() - fm_prefix_len` is computed by the caller
/// before calling this function.
///
/// Input/output format: `(out, src_index, src_line, src_col)` — all 0-based.
pub(crate) fn clamp_trailing_trim(
    points: Vec<(u32, u32, u32, u32)>,
    body_clean_len: usize,
) -> Vec<(u32, u32, u32, u32)> {
    points
        .into_iter()
        .filter(|(out, _, _, _)| (*out as usize) < body_clean_len)
        .collect()
}

/// Stage 4 — Shift output offsets by the frontmatter prefix byte length.
///
/// After `prepend_frontmatter`, the body starts at byte `fm_prefix_len` in the
/// final output string. Adding this offset to each `out` value aligns segment
/// positions with the string that [`SourceMap::from_points`] receives.
///
/// Input/output format: `(out, src_index, src_line, src_col)` — all 0-based.
pub(crate) fn shift_frontmatter(
    points: Vec<(u32, u32, u32, u32)>,
    fm_prefix_len: usize,
) -> Vec<(u32, u32, u32, u32)> {
    let shift = fm_prefix_len as u32;
    points
        .into_iter()
        .map(|(out, src, src_line, src_col)| (out + shift, src, src_line, src_col))
        .collect()
}

/// Stage 5 — Build the final [`SourceMap`] from adjusted points.
///
/// Calls [`SourceMap::from_points`] with the fully adjusted point list.
///
/// Input format: `(out_byte_in_final_body, src_index, src_line, src_col)`.
pub(crate) fn encode_vlq(
    points: Vec<(u32, u32, u32, u32)>,
    final_body: &str,
    sources: Vec<String>,
    sources_content: Option<Vec<String>>,
    file: Option<String>,
) -> SourceMap {
    SourceMap::from_points(
        final_body,
        sources,
        sources_content,
        file,
        points
            .into_iter()
            .map(|(out, src, src_line, src_col)| (out as usize, src, src_line, src_col)),
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CompileOptions::validate (ARCH-1)
    // -----------------------------------------------------------------------

    #[test]
    fn compile_options_validate_ok_defaults() {
        let opts = CompileOptions::default();
        assert!(opts.validate().is_ok(), "default options must be valid");
    }

    #[test]
    fn compile_options_validate_ok_source_map_only() {
        let opts = CompileOptions {
            source_map: true,
            include_sources_content: false,
        };
        assert!(
            opts.validate().is_ok(),
            "source_map without include_sources_content is valid"
        );
    }

    #[test]
    fn compile_options_validate_ok_both() {
        let opts = CompileOptions {
            source_map: true,
            include_sources_content: true,
        };
        assert!(
            opts.validate().is_ok(),
            "source_map=true + include_sources_content=true is valid"
        );
    }

    #[test]
    fn compile_options_validate_err_sources_content_without_source_map() {
        let opts = CompileOptions {
            source_map: false,
            include_sources_content: true,
        };
        assert!(
            opts.validate().is_err(),
            "include_sources_content=true without source_map=true must be Err"
        );
    }

    // -----------------------------------------------------------------------
    // Test-only VLQ decoder
    //
    // Decodes all VLQ fields concatenated in `s` (fields are self-delimiting
    // via the continuation bit). Used to round-trip encode → decode in tests.
    // -----------------------------------------------------------------------
    fn vlq_decode_fields(s: &str) -> Vec<i64> {
        const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

        let mut out = Vec::new();
        let mut bytes = s.bytes().peekable();

        while bytes.peek().is_some() {
            let mut accum: u64 = 0;
            let mut shift = 0u32;

            loop {
                let b = bytes.next().expect("incomplete VLQ sequence");
                let digit = B64
                    .iter()
                    .position(|&x| x == b)
                    .unwrap_or_else(|| panic!("invalid base64 byte: {b}"))
                    as u64;
                let has_cont = (digit & 0x20) != 0;
                let payload = digit & 0x1F;
                accum |= payload << shift;
                shift += 5;
                if !has_cont {
                    break;
                }
            }

            // LSB of `accum` is the sign bit (VLQ zigzag encoding).
            let sign_bit = accum & 1;
            let magnitude = (accum >> 1) as i64;
            out.push(if sign_bit == 1 { -magnitude } else { magnitude });
        }

        out
    }

    // -----------------------------------------------------------------------
    // VLQ roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn vlq_roundtrip_values() {
        let cases: &[i64] = &[
            0,
            1,
            -1,
            15,
            -15,
            16,
            -16,
            31,
            -31,
            32,
            -32,
            127,
            -127,
            128,
            -128,
            1023,
            -1023,
            1024,
            -1024,
            i32::MAX as i64,
            i32::MIN as i64,
        ];

        for &v in cases {
            let mut encoded = String::new();
            vlq_encode(v, &mut encoded);
            let decoded = vlq_decode_fields(&encoded);
            assert_eq!(
                decoded.len(),
                1,
                "value {v}: expected 1 decoded field, got {decoded:?} (encoded={encoded:?})"
            );
            assert_eq!(
                decoded[0], v,
                "value {v}: round-trip failed (encoded={encoded:?})"
            );
        }
    }

    #[test]
    fn vlq_encode_known_values() {
        // Spot-check specific known encodings from the SMv3 reference.
        let mut out = String::new();

        vlq_encode(0, &mut out);
        assert_eq!(out, "A", "0 should encode as 'A'");
        out.clear();

        vlq_encode(1, &mut out);
        assert_eq!(out, "C", "1 should encode as 'C'");
        out.clear();

        vlq_encode(-1, &mut out);
        assert_eq!(out, "D", "-1 should encode as 'D'");
        out.clear();

        // 16 requires 2 VLQ digits.
        vlq_encode(16, &mut out);
        let decoded = vlq_decode_fields(&out);
        assert_eq!(decoded, vec![16], "16 round-trip failed (encoded={out:?})");
        out.clear();

        vlq_encode(-16, &mut out);
        let decoded = vlq_decode_fields(&out);
        assert_eq!(
            decoded,
            vec![-16],
            "-16 round-trip failed (encoded={out:?})"
        );
    }

    // -----------------------------------------------------------------------
    // LineTable
    // -----------------------------------------------------------------------

    #[test]
    fn linetable_empty_string() {
        let table = LineTable::new("");
        // Offset 0 is always a valid char boundary (even on an empty string).
        assert_eq!(table.resolve(0), Some((0, 0)));
        // Offset 1 is past the end → None.
        assert_eq!(table.resolve(1), None);
    }

    #[test]
    fn linetable_single_line_no_trailing_newline() {
        let table = LineTable::new("hello");
        assert_eq!(table.resolve(0), Some((0, 0)));
        assert_eq!(table.resolve(3), Some((0, 3)));
        // Offset == len is a valid char boundary.
        assert_eq!(table.resolve(5), Some((0, 5)));
        // Offset past len is not.
        assert_eq!(table.resolve(6), None);
    }

    #[test]
    fn linetable_crlf_defensive() {
        // "ab\r\ncd": bytes  a=0 b=1 \r=2 \n=3 c=4 d=5
        // line_starts = [0, 4]
        let table = LineTable::new("ab\r\ncd");
        assert_eq!(table.resolve(0), Some((0, 0))); // 'a'
        assert_eq!(table.resolve(2), Some((0, 2))); // '\r'
        assert_eq!(table.resolve(3), Some((0, 3))); // '\n' itself (still line 0)
        assert_eq!(table.resolve(4), Some((1, 0))); // 'c' — start of line 1
        assert_eq!(table.resolve(6), Some((1, 2))); // past 'd'
    }

    #[test]
    fn linetable_multibyte_utf16_columns() {
        // 'é' = U+00E9: 2 UTF-8 bytes, 1 UTF-16 code unit.
        // '😀' = U+1F600: 4 UTF-8 bytes, 2 UTF-16 code units.
        //
        // Body: "é😀" — bytes 0-1 = 'é', bytes 2-5 = '😀'.
        let body = "é😀";
        let table = LineTable::new(body);

        // 'é' starts at byte 0 → UTF-16 col 0.
        assert_eq!(table.resolve(0), Some((0, 0)));
        // '😀' starts at byte 2 → UTF-16 col 1 (after 1 code unit for 'é').
        assert_eq!(table.resolve(2), Some((0, 1)));
        // Past '😀' at byte 6 → UTF-16 col 3 (1 + 2 = 3 code units).
        assert_eq!(table.resolve(6), Some((0, 3)));

        // Mid-char bytes → None.
        assert_eq!(table.resolve(1), None, "mid-'é' byte should be None");
        assert_eq!(table.resolve(3), None, "mid-'😀' byte should be None");
        assert_eq!(table.resolve(4), None, "mid-'😀' byte should be None");
        assert_eq!(table.resolve(5), None, "mid-'😀' byte should be None");
    }

    #[test]
    fn linetable_mid_multibyte_returns_none() {
        // Single 2-byte char: 'é'.
        let table = LineTable::new("é");
        assert_eq!(table.resolve(0), Some((0, 0)));
        assert_eq!(table.resolve(1), None); // mid-char
        assert_eq!(table.resolve(2), Some((0, 1))); // past end (valid boundary)
        assert_eq!(table.resolve(3), None); // truly past end
    }

    #[test]
    fn linetable_multi_line() {
        // "abc\ndef\nghi"
        // line 0: bytes 0-3 ("abc\n"), line 1: bytes 4-7 ("def\n"), line 2: bytes 8-10 ("ghi")
        let table = LineTable::new("abc\ndef\nghi");
        assert_eq!(table.resolve(0), Some((0, 0))); // 'a'
        assert_eq!(table.resolve(3), Some((0, 3))); // '\n' on line 0
        assert_eq!(table.resolve(4), Some((1, 0))); // 'd'
        assert_eq!(table.resolve(8), Some((2, 0))); // 'g'
        assert_eq!(table.resolve(11), Some((2, 3))); // past 'i'
    }

    // -----------------------------------------------------------------------
    // ASCII fast-path
    // -----------------------------------------------------------------------

    #[test]
    fn linetable_ascii_fastpath_column_equals_byte_offset() {
        let body = "Hello, World!";
        let table = LineTable::new(body);

        // The flag should be set.
        assert!(
            table.line_is_ascii[0],
            "pure-ASCII line must set line_is_ascii"
        );

        // For an ASCII line, UTF-16 column == byte offset.
        for col in [0usize, 1, 5, 7, 12, 13] {
            let (line, utf16_col) = table.resolve(col).expect("ASCII byte should resolve");
            assert_eq!(line, 0);
            assert_eq!(
                utf16_col, col as u32,
                "col={col}: UTF-16 column should equal byte offset on ASCII line"
            );
        }
    }

    #[test]
    fn linetable_mixed_ascii_and_nonascii_lines() {
        // Line 0 ASCII, line 1 non-ASCII.
        let body = "abc\né";
        let table = LineTable::new(body);

        assert!(table.line_is_ascii[0], "line 0 should be ASCII");
        assert!(!table.line_is_ascii[1], "line 1 should be non-ASCII");

        // ASCII line: col == byte offset.
        assert_eq!(table.resolve(0), Some((0, 0)));
        assert_eq!(table.resolve(2), Some((0, 2)));

        // Non-ASCII line: 'é' at byte 4 → col 0.
        assert_eq!(table.resolve(4), Some((1, 0)));
        // Mid-'é' → None.
        assert_eq!(table.resolve(5), None);
        // Past 'é' at byte 6 → col 1.
        assert_eq!(table.resolve(6), Some((1, 1)));
    }

    // -----------------------------------------------------------------------
    // encode_mappings
    // -----------------------------------------------------------------------

    #[test]
    fn encode_mappings_empty_points() {
        assert_eq!(encode_mappings(vec![]), "");
    }

    #[test]
    fn encode_mappings_single_segment_all_zeros() {
        let points = vec![(0u32, 0u32, 0u32, 0u32, 0u32)];
        let m = encode_mappings(points);
        // Four zero fields: "AAAA".
        assert_eq!(m, "AAAA");
        assert_eq!(vlq_decode_fields("AAAA"), vec![0, 0, 0, 0]);
    }

    #[test]
    fn encode_mappings_two_segments_on_same_line() {
        // Second segment at col 5, src col 3 (deltas from first).
        let points = vec![
            (0u32, 0u32, 0u32, 0u32, 0u32),
            (0u32, 5u32, 0u32, 0u32, 3u32),
        ];
        let m = encode_mappings(points.clone());

        let line_parts: Vec<&str> = m.split(';').collect();
        assert_eq!(
            line_parts.len(),
            1,
            "all on one output line → no semicolons"
        );

        let segs: Vec<&str> = line_parts[0].split(',').collect();
        assert_eq!(segs.len(), 2);

        let f0 = vlq_decode_fields(segs[0]);
        assert_eq!(f0, vec![0, 0, 0, 0]);

        // Second segment: d_genCol=5, d_srcIdx=0, d_srcLine=0, d_srcCol=3.
        let f1 = vlq_decode_fields(segs[1]);
        assert_eq!(f1, vec![5, 0, 0, 3]);
    }

    #[test]
    fn encode_mappings_across_two_output_lines() {
        let points = vec![
            (0u32, 0u32, 0u32, 0u32, 0u32),
            (1u32, 0u32, 0u32, 1u32, 0u32),
        ];
        let m = encode_mappings(points);
        let parts: Vec<&str> = m.split(';').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "AAAA");

        // Line 1 segment: d_genCol=0 (reset), d_srcIdx=0, d_srcLine=1, d_srcCol=0.
        let f1 = vlq_decode_fields(parts[1]);
        assert_eq!(f1, vec![0, 0, 1, 0]);
    }

    #[test]
    fn encode_mappings_empty_lines_between_segments() {
        // Points on output lines 0 and 2 — line 1 is empty.
        let points = vec![
            (0u32, 0u32, 0u32, 0u32, 0u32),
            (2u32, 0u32, 0u32, 2u32, 0u32),
        ];
        let m = encode_mappings(points);
        let parts: Vec<&str> = m.split(';').collect();
        assert_eq!(parts.len(), 3, "lines 0, 1, 2 → 3 segments");
        assert_eq!(parts[0], "AAAA");
        assert_eq!(parts[1], "", "line 1 is empty");

        // Line 2: d_genCol=0, d_srcIdx=0, d_srcLine=2, d_srcCol=0.
        let f2 = vlq_decode_fields(parts[2]);
        assert_eq!(f2, vec![0, 0, 2, 0]);
    }

    #[test]
    fn encode_mappings_sorts_unsorted_points() {
        // Given out of (col) order; encoder must sort.
        let points = vec![
            (0u32, 5u32, 0u32, 0u32, 5u32),
            (0u32, 0u32, 0u32, 0u32, 0u32),
        ];
        let m = encode_mappings(points);
        let segs: Vec<&str> = m.split(',').collect();
        assert_eq!(segs.len(), 2);

        // First segment (sorted): col 0.
        let f0 = vlq_decode_fields(segs[0]);
        assert_eq!(f0, vec![0, 0, 0, 0]);

        // Second segment: d_genCol=5, d_srcCol=5.
        let f1 = vlq_decode_fields(segs[1]);
        assert_eq!(f1, vec![5, 0, 0, 5]);
    }

    #[test]
    fn encode_mappings_roundtrip_with_deltas() {
        // Three points spread across two lines; verify full decode matches.
        let points = vec![
            (0u32, 0u32, 0u32, 0u32, 0u32),
            (0u32, 5u32, 0u32, 0u32, 3u32),
            (1u32, 0u32, 0u32, 1u32, 0u32),
        ];
        let m = encode_mappings(points.clone());

        // Decode the mappings string back into absolute points.
        let lines: Vec<&str> = m.split(';').collect();
        let mut decoded: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
        let mut prev_src_idx: i64 = 0;
        let mut prev_src_line: i64 = 0;
        let mut prev_src_col: i64 = 0;

        for (line_idx, line_str) in lines.iter().enumerate() {
            let mut prev_gen_col: i64 = 0; // resets to 0 at each new output line
            for seg in line_str.split(',').filter(|s| !s.is_empty()) {
                let fields = vlq_decode_fields(seg);
                assert_eq!(fields.len(), 4, "each segment must have 4 fields");
                prev_gen_col += fields[0];
                prev_src_idx += fields[1];
                prev_src_line += fields[2];
                prev_src_col += fields[3];
                decoded.push((
                    line_idx as u32,
                    prev_gen_col as u32,
                    prev_src_idx as u32,
                    prev_src_line as u32,
                    prev_src_col as u32,
                ));
            }
        }

        assert_eq!(decoded, points, "decoded points must match original");
    }

    // -----------------------------------------------------------------------
    // SourceMap::to_json
    // -----------------------------------------------------------------------

    #[test]
    fn to_json_basic_structure() {
        let sm = SourceMap {
            version: 3,
            file: None,
            sources: vec!["input.mds".to_string()],
            sources_content: None,
            names: vec![],
            mappings: "AAAA".to_string(),
        };
        let json = sm.to_json();

        // Required fields.
        assert!(
            json.contains(r#""version":3"#),
            "version:3 required; json={json}"
        );
        assert!(
            json.contains(r#""names":[]"#),
            r#"names:[] required; json={json}"#
        );
        assert!(
            json.contains(r#""mappings":"AAAA""#),
            r#"mappings field required; json={json}"#
        );

        // Optional fields absent when None.
        assert!(
            !json.contains(r#""file""#),
            "file should be absent when None; json={json}"
        );
        assert!(
            !json.contains("sourcesContent"),
            "sourcesContent should be absent when None; json={json}"
        );

        // Key order: version → sources → names → mappings.
        let v = json.find(r#""version""#).unwrap();
        let s = json.find(r#""sources""#).unwrap();
        let n = json.find(r#""names""#).unwrap();
        let m = json.find(r#""mappings""#).unwrap();
        assert!(v < s, "version must precede sources");
        assert!(s < n, "sources must precede names");
        assert!(n < m, "names must precede mappings");
    }

    #[test]
    fn to_json_with_optional_fields() {
        let sm = SourceMap {
            version: 3,
            file: Some("output.md".to_string()),
            sources: vec!["input.mds".to_string()],
            sources_content: Some(vec!["source content".to_string()]),
            names: vec![],
            mappings: "AAAA".to_string(),
        };
        let json = sm.to_json();

        assert!(
            json.contains(r#""file":"output.md""#),
            "file should be present; json={json}"
        );
        assert!(
            json.contains("sourcesContent"),
            "sourcesContent should be present; json={json}"
        );

        // Key order: version → file → sources → sourcesContent → names → mappings.
        let v = json.find(r#""version""#).unwrap();
        let f = json.find(r#""file""#).unwrap();
        let s = json.find(r#""sources""#).unwrap();
        let sc = json.find("sourcesContent").unwrap();
        let n = json.find(r#""names""#).unwrap();
        let m = json.find(r#""mappings""#).unwrap();
        assert!(v < f, "version before file");
        assert!(f < s, "file before sources");
        assert!(s < sc, "sources before sourcesContent");
        assert!(sc < n, "sourcesContent before names");
        assert!(n < m, "names before mappings");
    }

    // -----------------------------------------------------------------------
    // SourceMap::from_points (integration: LineTable + encode_mappings)
    // -----------------------------------------------------------------------

    #[test]
    fn from_points_empty_yields_empty_mappings() {
        let sm = SourceMap::from_points("Hello", vec!["input.mds".to_string()], None, None, vec![]);
        assert_eq!(sm.mappings, "");
        assert_eq!(sm.version, 3);
        assert_eq!(sm.names, Vec::<String>::new());
    }

    #[test]
    fn from_points_resolves_byte_offsets_across_lines() {
        // "Hello\nWorld": line 0 = "Hello\n" (bytes 0–5), line 1 = "World" (bytes 6–10).
        let body = "Hello\nWorld";
        let sm = SourceMap::from_points(
            body,
            vec!["input.mds".to_string()],
            None,
            None,
            [
                (0usize, 0u32, 0u32, 0u32), // byte 0 → line 0, col 0
                (6usize, 0u32, 1u32, 0u32), // byte 6 → line 1, col 0
            ],
        );

        let parts: Vec<&str> = sm.mappings.split(';').collect();
        assert_eq!(parts.len(), 2, "two output lines → one semicolon");

        let f0 = vlq_decode_fields(parts[0]);
        assert_eq!(f0, vec![0, 0, 0, 0], "line 0 segment");

        // Line 1: d_genCol=0 (reset), d_srcIdx=0, d_srcLine=1, d_srcCol=0.
        let f1 = vlq_decode_fields(parts[1]);
        assert_eq!(f1, vec![0, 0, 1, 0], "line 1 segment");
    }

    #[test]
    fn from_points_drops_mid_char_byte_offsets() {
        // 'é' = 2 bytes; byte offset 1 lands mid-char and must be silently dropped.
        let body = "é";
        let sm = SourceMap::from_points(
            body,
            vec!["input.mds".to_string()],
            None,
            None,
            [
                (0usize, 0u32, 0u32, 0u32), // valid
                (1usize, 0u32, 0u32, 1u32), // mid-char → dropped
            ],
        );

        // Only the valid point survives.
        let parts: Vec<&str> = sm.mappings.split(';').collect();
        assert_eq!(parts.len(), 1, "one output line");

        let segs: Vec<&str> = parts[0].split(',').filter(|s| !s.is_empty()).collect();
        assert_eq!(segs.len(), 1, "only one valid point");

        let fields = vlq_decode_fields(segs[0]);
        assert_eq!(fields, vec![0, 0, 0, 0]);
    }

    // -----------------------------------------------------------------------
    // MapBuilder
    // -----------------------------------------------------------------------

    #[test]
    fn map_builder_new_seeds_source_at_index_zero() {
        let b = MapBuilder::new("a.mds".to_string(), "source".to_string());
        assert_eq!(b.sources, vec!["a.mds"]);
        assert_eq!(b.sources_content, vec!["source"]);
        assert_eq!(b.current_src, 0);
        assert_eq!(b.cursor, 0);
        assert_eq!(b.suppress, 0);
        assert!(b.segments.is_empty());
    }

    #[test]
    fn map_builder_source_index_deduplicates() {
        let mut b = MapBuilder::new("a.mds".to_string(), "content-a".to_string());
        assert_eq!(b.source_index("a.mds", "content-a"), 0);
        assert_eq!(b.source_index("b.mds", "content-b"), 1);
        assert_eq!(b.source_index("a.mds", "content-a"), 0); // dedup
        assert_eq!(b.source_index("b.mds", "content-b"), 1); // dedup
        assert_eq!(b.sources.len(), 2);
    }

    #[test]
    fn map_builder_push_segment_caps_at_limit() {
        use crate::limits::MAX_SOURCEMAP_SEGMENTS;
        let mut b = MapBuilder::new("a.mds".to_string(), String::new());
        for i in 0..=(MAX_SOURCEMAP_SEGMENTS + 5) as u32 {
            b.push_segment(i, i, 1);
        }
        assert_eq!(b.segments.len(), MAX_SOURCEMAP_SEGMENTS, "capped at limit");
    }

    // -----------------------------------------------------------------------
    // Stage 1: expand_per_line
    // -----------------------------------------------------------------------

    #[test]
    fn expand_per_line_basic() {
        // Source: "hello\nworld\n"
        // byte 0 → line 0, col 0
        // byte 6 → line 1, col 0
        let source = "hello\nworld\n";
        let segs = vec![
            RawSegment {
                out: 0,
                src: 0,
                src_off: 0,
                len: 5,
            },
            RawSegment {
                out: 5,
                src: 0,
                src_off: 6,
                len: 5,
            },
        ];
        let pts = expand_per_line(segs, &[source.to_string()]);
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0], (0, 0, 0, 0)); // out=0, src=0, line=0, col=0
        assert_eq!(pts[1], (5, 0, 1, 0)); // out=5, src=0, line=1, col=0
    }

    #[test]
    fn expand_per_line_drops_invalid_src_off() {
        // 'é' is 2 bytes; byte 1 is not a char boundary → dropped.
        let source = "é";
        let segs = vec![
            RawSegment {
                out: 0,
                src: 0,
                src_off: 0,
                len: 2,
            }, // valid
            RawSegment {
                out: 2,
                src: 0,
                src_off: 1,
                len: 1,
            }, // mid-char → drop
        ];
        let pts = expand_per_line(segs, &[source.to_string()]);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0], (0, 0, 0, 0));
    }

    #[test]
    fn expand_per_line_multi_source() {
        let src_a = "abc";
        let src_b = "xyz";
        let segs = vec![
            RawSegment {
                out: 0,
                src: 0,
                src_off: 0,
                len: 1,
            },
            RawSegment {
                out: 1,
                src: 1,
                src_off: 0,
                len: 1,
            },
        ];
        let pts = expand_per_line(segs, &[src_a.to_string(), src_b.to_string()]);
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0], (0, 0, 0, 0));
        assert_eq!(pts[1], (1, 1, 0, 0));
    }

    // -----------------------------------------------------------------------
    // Stage 2: compensate_cr
    // -----------------------------------------------------------------------

    #[test]
    fn compensate_cr_no_cr() {
        let body_raw = "hello\nworld\n";
        let pts = vec![(5u32, 0u32, 0u32, 5u32)];
        let result = compensate_cr(pts.clone(), body_raw);
        assert_eq!(result, pts, "no \\r → offsets unchanged");
    }

    #[test]
    fn compensate_cr_one_cr_before_point() {
        // "hello\r\nworld": \r at byte 5.  Point at byte 6 (the \n's successor)
        // has 1 \r before it → adjusted to 5.
        let body_raw = "hello\r\nworld";
        let pts = vec![(6u32, 0u32, 0u32, 0u32)];
        let result = compensate_cr(pts, body_raw);
        assert_eq!(result[0].0, 5, "one \\r before offset 6 → adjusted to 5");
    }

    #[test]
    fn compensate_cr_multiple_cr() {
        // "\r\r\rfoo": 3 \r at bytes 0,1,2.  Point at byte 3 has 3 \r before it.
        let body_raw = "\r\r\rfoo";
        let pts = vec![(3u32, 0u32, 0u32, 0u32)];
        let result = compensate_cr(pts, body_raw);
        assert_eq!(result[0].0, 0, "3 \\r before offset 3 → adjusted to 0");
    }

    #[test]
    fn compensate_cr_cr_after_point() {
        // "\rhello\r": \r at byte 0, point at byte 0 → no \r before it.
        let body_raw = "\rhello\r";
        let pts = vec![(0u32, 0u32, 0u32, 0u32)];
        let result = compensate_cr(pts, body_raw);
        assert_eq!(result[0].0, 0, "\\r at or after point → no adjustment");
    }

    // -----------------------------------------------------------------------
    // Stage 3: clamp_trailing_trim
    // -----------------------------------------------------------------------

    #[test]
    fn clamp_trailing_trim_drops_past_clean_len() {
        let pts = vec![
            (0u32, 0u32, 0u32, 0u32),
            (5u32, 0u32, 0u32, 5u32),
            (10u32, 0u32, 1u32, 0u32),
        ];
        // body_clean_len = 6: points at 0 and 5 survive; 10 is dropped.
        let result = clamp_trailing_trim(pts, 6);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, 0);
        assert_eq!(result[1].0, 5);
    }

    #[test]
    fn clamp_trailing_trim_empty_body() {
        let pts = vec![(0u32, 0u32, 0u32, 0u32), (3u32, 0u32, 0u32, 3u32)];
        let result = clamp_trailing_trim(pts, 0);
        assert!(result.is_empty(), "empty body → all segments dropped");
    }

    #[test]
    fn clamp_trailing_trim_all_survive() {
        let pts = vec![(0u32, 0u32, 0u32, 0u32), (4u32, 0u32, 0u32, 4u32)];
        let result = clamp_trailing_trim(pts.clone(), 100);
        assert_eq!(result, pts, "all within clean_len → none dropped");
    }

    // -----------------------------------------------------------------------
    // Stage 4: shift_frontmatter
    // -----------------------------------------------------------------------

    #[test]
    fn shift_frontmatter_zero_prefix() {
        let pts = vec![(0u32, 0u32, 0u32, 0u32), (5u32, 0u32, 1u32, 0u32)];
        let result = shift_frontmatter(pts.clone(), 0);
        assert_eq!(result, pts, "zero prefix → no shift");
    }

    #[test]
    fn shift_frontmatter_nonzero_prefix() {
        // "---\nfm: v\n---\nbody\n" — prefix = 14 bytes.
        let pts = vec![(0u32, 0u32, 0u32, 0u32), (5u32, 0u32, 1u32, 0u32)];
        let result = shift_frontmatter(pts, 14);
        assert_eq!(result[0].0, 14);
        assert_eq!(result[1].0, 19);
    }

    // -----------------------------------------------------------------------
    // MapBuilder::finalize (end-to-end integration)
    // -----------------------------------------------------------------------

    #[test]
    fn finalize_simple_no_frontmatter_no_cr() {
        // Source template: "Hello {name}!\n"
        // After evaluation with name="World": raw = "Hello World!\n"
        // clean_output: same (no trailing whitespace change)
        // Segments: Text("Hello ") at src_off=0, len=6; Interpolation at src_off=6, len=6
        let mut b = MapBuilder::new("t.mds".to_string(), "Hello {name}!\n".to_string());
        b.push_segment(0, 0, 6); // "Hello " maps to src byte 0
        b.push_segment(6, 6, 6); // "{name}" maps to src byte 6

        let body_raw = "Hello World!\n";
        let final_body = "Hello World!\n"; // no frontmatter
        let sm = b.finalize(body_raw, final_body, 0, None);

        assert_eq!(sm.version, 3);
        assert_eq!(sm.sources, vec!["t.mds"]);
        assert!(sm.sources_content.is_some());
        // mappings must be non-empty (two segments on one line)
        assert!(!sm.mappings.is_empty(), "mappings should not be empty");
        assert!(
            !sm.mappings.contains(';'),
            "single output line → no semicolons in mappings"
        );
        assert!(
            !sm.mappings.contains(r#""-""#),
            "VLQ alphabet must not contain '-'"
        );
        // Verify 2 segments
        let segs: Vec<&str> = sm.mappings.split(',').collect();
        assert_eq!(segs.len(), 2, "two segments expected");
    }

    #[test]
    fn finalize_empty_body_yields_empty_mappings() {
        // Raw output is whitespace-only; clean_output produces "".
        let mut b = MapBuilder::new("t.mds".to_string(), "  \n  ".to_string());
        b.push_segment(0, 0, 5);

        let body_raw = "  \n  ";
        let final_body = ""; // clean_output → ""
        let sm = b.finalize(body_raw, final_body, 0, None);

        assert_eq!(sm.mappings, "", "empty body → empty mappings");
    }

    #[test]
    fn finalize_with_frontmatter_shifts_points() {
        // "---\nfm: v\n---\nHello\n" — frontmatter prefix = 14 bytes
        // Segment at out=0 in raw output → out=14 in final output after shift
        let mut b = MapBuilder::new("t.mds".to_string(), "Hello\n".to_string());
        b.push_segment(0, 0, 5);

        let body_raw = "Hello\n";
        let final_body = "---\nfm: v\n---\nHello\n"; // 14 byte prefix + 6 body
        let fm_prefix_len = 14;
        let sm = b.finalize(body_raw, final_body, fm_prefix_len, None);

        assert!(!sm.mappings.is_empty());
        // The output side point should be on line 3 (0-based), col 0
        // "---\n" = line 0, "fm: v\n" = line 1, "---\n" = line 2, "Hello\n" = line 3
        let lines: Vec<&str> = sm.mappings.split(';').collect();
        // Lines 0, 1, 2 are frontmatter (empty segments), line 3 has the segment
        assert_eq!(lines.len(), 4, "output has 4 lines (0-indexed: 0..3)");
        assert!(
            lines[3].contains('A') || !lines[3].is_empty(),
            "line 3 has segment"
        );
    }

    #[test]
    fn finalize_compensates_cr() {
        // Raw output contains \r\n line endings → clean_output strips \r.
        // "Hello\r\nWorld\r\n" → clean: "Hello\nWorld\n"
        // Segment at raw out=7 ("\r\n" takes bytes 5-6, "World" starts at 7)
        // → compensated: 7 - 1 cr before 7 = 6 (correct position in clean output)
        let mut b = MapBuilder::new("t.mds".to_string(), "src".to_string());
        b.push_segment(0, 0, 5); // "Hello" in raw output
        b.push_segment(7, 0, 5); // "World" in raw output (byte 7 after \r\n)

        let body_raw = "Hello\r\nWorld\r\n";
        let final_body = "Hello\nWorld\n"; // clean_output result
        let sm = b.finalize(body_raw, final_body, 0, None);

        assert!(!sm.mappings.is_empty());
        // Should have 2 segments across 2 output lines
        let parts: Vec<&str> = sm.mappings.split(';').collect();
        assert_eq!(parts.len(), 2, "two output lines");
    }

    // ── rebase_trim unit tests ──────────────────────────────────────────────

    fn make_segs(triples: &[(u32, u32, u32)]) -> Vec<RawSegment> {
        triples
            .iter()
            .map(|&(out, src_off, len)| RawSegment {
                out,
                src: 0,
                src_off,
                len,
            })
            .collect()
    }

    /// No-op: no segments in the window → nothing changes.
    #[test]
    fn rebase_trim_empty_window() {
        let mut segs = make_segs(&[(5, 0, 3)]);
        // seg_start beyond vec → no-op.
        rebase_trim(&mut segs, 1, 10, 2, 10, 2);
        assert_eq!(segs.len(), 1, "segment outside window must be preserved");
        assert_eq!(segs[0].out, 5);
    }

    /// Lead-only trim: drop leading whitespace segment, shift surviving ones.
    #[test]
    fn rebase_trim_lead_only() {
        // Body: "  Hello" — 2-byte lead, 0 trail, 7 total bytes.
        // start_cursor = 100
        // Segment at out=100 (in lead region) → dropped.
        // Segment at out=102 (content start) → shifted to out=100.
        let mut segs = make_segs(&[(100, 0, 2), (102, 2, 5)]);
        rebase_trim(&mut segs, 0, 100, 2, 7, 0);
        assert_eq!(segs.len(), 1, "lead segment must be dropped");
        assert_eq!(segs[0].out, 100, "surviving segment shifted by lead");
        assert_eq!(segs[0].src_off, 2, "src_off unchanged");
    }

    /// Trail-only trim: drop trailing whitespace segment, keep content.
    #[test]
    fn rebase_trim_trail_only() {
        // Body: "Hello  " — 0-byte lead, 2 trail, 7 total.
        // start_cursor = 50
        // Segment at out=50 (content) → kept, out unchanged (lead=0, no shift).
        // Segment at out=55 (in trail region) → dropped.
        let mut segs = make_segs(&[(50, 0, 5), (55, 5, 2)]);
        rebase_trim(&mut segs, 0, 50, 0, 7, 2);
        assert_eq!(segs.len(), 1, "trail segment must be dropped");
        assert_eq!(segs[0].out, 50, "no shift when lead=0");
    }

    /// Both lead and trail trimmed; middle segment survives and is shifted.
    #[test]
    fn rebase_trim_lead_and_trail() {
        // Body: "  Hi  " — 2 lead, 2 trail, 6 total.
        // start_cursor = 0
        // out=0 → in lead (< 2) → dropped.
        // out=2 → in content (2 <= out < 4) → shifted to out=0.
        // out=4 → in trail (>= 4) → dropped.
        let mut segs = make_segs(&[(0, 0, 2), (2, 2, 2), (4, 4, 2)]);
        rebase_trim(&mut segs, 0, 0, 2, 6, 2);
        assert_eq!(segs.len(), 1, "only content segment survives");
        assert_eq!(segs[0].out, 0, "shifted by lead (2)");
        assert_eq!(segs[0].src_off, 2);
    }

    /// All-whitespace body: no segments survive.
    #[test]
    fn rebase_trim_all_whitespace() {
        let mut segs = make_segs(&[(0, 0, 3), (3, 3, 2)]);
        // Body is "     " — 5-byte lead, 5-byte trail, 5 total → trail_start = 0
        rebase_trim(&mut segs, 0, 0, 5, 5, 5);
        assert_eq!(
            segs.len(),
            0,
            "all segments dropped for all-whitespace body"
        );
    }

    /// Segments before seg_start are untouched (they belong to the outer eval).
    #[test]
    fn rebase_trim_respects_seg_start() {
        // Outer segment at out=0, body segments at out=10 (lead) and out=12 (content).
        let mut segs = make_segs(&[(0, 99, 1), (10, 0, 2), (12, 2, 3)]);
        // seg_start=1 so only indices 1..3 are in the window.
        // Body start_cursor=10, lead=2, untrimmed=5, trail=0.
        rebase_trim(&mut segs, 1, 10, 2, 5, 0);
        assert_eq!(segs.len(), 2, "outer + content survive");
        assert_eq!(segs[0].out, 0, "outer segment untouched");
        assert_eq!(segs[0].src_off, 99, "outer segment src_off untouched");
        assert_eq!(segs[1].out, 10, "content segment shifted by 2 (12 - 2)");
    }

    /// `segments_dropped` is set when the cap is hit, and NOT set before.
    #[test]
    fn map_builder_segments_dropped_flag() {
        use crate::limits::MAX_SOURCEMAP_SEGMENTS;
        let mut b = MapBuilder::new("test.mds".to_string(), "source".to_string());
        // Fill to one below cap.
        for i in 0..MAX_SOURCEMAP_SEGMENTS {
            b.push_segment(i as u32, i as u32, 1);
        }
        assert!(
            !b.segments_dropped,
            "segments_dropped must be false when exactly at cap"
        );
        // One more push → triggers the drop path.
        b.push_segment(MAX_SOURCEMAP_SEGMENTS as u32, 0, 1);
        assert!(
            b.segments_dropped,
            "segments_dropped must be true after cap is exceeded"
        );
        assert_eq!(
            b.segments.len(),
            MAX_SOURCEMAP_SEGMENTS,
            "segment count must stay at cap"
        );
    }

    /// `sources_content_bytes()` returns the sum of all registered source sizes.
    #[test]
    fn map_builder_sources_content_bytes() {
        let mut b = MapBuilder::new("a.mds".to_string(), "hello".to_string()); // 5 bytes
        let _ = b.source_index("b.mds", "world!"); // 6 bytes
        assert_eq!(
            b.sources_content_bytes(),
            11,
            "sources_content_bytes must sum all source content lengths"
        );
    }
}
