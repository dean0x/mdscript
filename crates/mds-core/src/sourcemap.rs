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

use serde::Serialize;

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
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut prev_gen_col: i64 = 0;
        let mut prev_src_idx: i64 = 0;
        let mut prev_src_line: i64 = 0;
        let mut prev_src_col: i64 = 0;

        for (line_idx, line_str) in lines.iter().enumerate() {
            prev_gen_col = 0; // reset per line
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
}
