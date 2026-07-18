//! `mds fmt` engine — an opinionated, safety-gated auto-formatter for MDS source.
//!
//! # The load-bearing constraint
//!
//! MDS is a *content language*: the formatter MUST produce byte-identical compile
//! output (`compile(fmt(src)).output == compile(src).output`) and MUST be
//! idempotent (`fmt(fmt(src)) == fmt(src)`). The token stream is lossy (the lexer
//! trims interpolation inner whitespace, discards the newline after a directive
//! line / code fence, and filters frontmatter `\r`) so source can never be
//! reconstructed from tokens/AST alone without inventing or dropping whitespace
//! the author actually wrote. This module therefore rewrites the ORIGINAL SOURCE
//! STRING directly, line by line, copying any byte without a proven-safe rule
//! verbatim — it never reconstructs from tokens.
//!
//! # Approach
//!
//! 1. Tokenize the source (surfaces syntax errors; reuses the lexer's fence /
//!    frontmatter state machine rather than re-implementing it).
//! 2. Compute *raw-content* byte ranges (`@message`/`@define` bodies) and the
//!    set of *directive line* start offsets.
//! 3. Rewrite the source in a single left-to-right, line-oriented pass, applying
//!    only rules that are provably output-preserving (R1, R2, R4 below).
//! 4. Run a runtime safety gate (`assert_equivalent`) that re-compiles both the
//!    original and formatted source and hard-errors (`MdsError::FormatterInvariant`)
//!    on any divergence, rather than silently returning a wrong result.
//!
//! # Ruleset (v2 — interior-verbatim)
//!
//! - **R1 (CRLF/CR removal)** — applied everywhere *except* `@message`/`@define`
//!   bodies (`raw_content_spans`). `clean_output` strips every `\r` from the
//!   final compiled string; frontmatter lexing and directive matching also strip
//!   `\r` before comparison. Only raw-content bodies bypass `clean_output` and
//!   therefore must be left byte-exact.
//! - **R2 (exactly one final newline)** — the whole body is trimmed to its last
//!   non-whitespace byte, then a single `\n` is appended, matching
//!   `clean_output`'s own trailing-edge normalisation.
//! - **R4 (directive trailing-whitespace strip)** — safe because the parser
//!   calls `dir.trim()` before matching, so trailing whitespace on a directive
//!   line is discarded pre-parse and never reaches output. Applied even inside
//!   `@message`/`@define` bodies since directive text is never part of compiled
//!   output in either mode.
//!
//! R3 (blank-line run capping) was present in v1 but **removed** in v2 (#150,
//! #151). `clean_output` now passes interior content through verbatim — it only
//! normalises the trailing edge — so capping blank-line runs in the formatter
//! would *change* compiled output and violate the load-bearing constraint.
//! Code-fence regions no longer need a special protection class: they receive
//! the same treatment as ordinary body content (R1 only), because without R3
//! there is nothing to gate on those boundaries.
//!
//! `@message`/`@define` bodies (see `raw_content_spans`) are a THIRD region
//! that is copied completely verbatim — not even R1's `\r` removal is applied.
//! This was discovered empirically: `@message` content is built by
//! `evaluate_nodes(...).trim()` with NO `clean_output` pass (see
//! `collect_single_message` in `evaluator.rs`), so a `\r` inside one reaches
//! the compiled message JSON verbatim. `@define` bodies get the same
//! conservative treatment because a function can be called from a markdown-mode
//! site OR from within a `@message` body.
//!
//! ## Deliberately NOT implemented: blank-line whitespace stripping
//!
//! Stripping trailing whitespace from all-whitespace "blank" lines would break
//! compile equivalence: `clean_output` passes a whitespace-only line through
//! verbatim (the space is not a `\n`, so it resets no run counter). See
//! `r4_whitespace_only_line_in_middle_of_document_is_preserved_verbatim` in
//! `tests/fmt.rs` for the regression test that locks this in.
//!
//! # Deferred to a future version (NOT implemented here)
//!
//! Interpolation `{ x }` -> `{x}` trimming, frontmatter key sorting, `@import`
//! grouping/reordering, blank-line insertion around directive blocks, body
//! hard-break normalization, directive internal spacing.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::Path;

use crate::error::MdsError;
use crate::lexer::{self, Token};

/// Format MDS source code, returning the rewritten source string.
///
/// Equivalent to [`format_str_with`] with `base_dir = None` (imports resolve
/// against the current working directory, matching [`crate::compile_str`]).
///
/// # Errors
///
/// Returns `Err` if `source` has a syntax error (never a garbled string), or if
/// the rewritten source fails the internal compile-equivalence safety gate
/// (`MdsError::FormatterInvariant` — signals a formatter bug; callers must not
/// write the file when this occurs).
///
/// # Examples
///
/// ```rust
/// // CRLF -> LF; interior blank lines are left verbatim (interior-verbatim
/// // contract, #150/#151 — R3 blank-run capping was removed in v2).
/// let formatted = mds::format_str("Hello   \r\n\r\n\r\nworld\r\n")?;
/// assert_eq!(formatted, "Hello   \n\n\nworld\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "the formatted source should be used"]
pub fn format_str(source: &str) -> Result<String, MdsError> {
    format_str_with(source, None)
}

/// Format MDS source code with an explicit `@import` base directory.
///
/// `base_dir` sets the root for resolving `@import` paths during the
/// compile-equivalence safety gate; defaults to the current directory when
/// `None`, matching [`crate::compile_str_with`].
///
/// # Errors
///
/// See [`format_str`].
#[must_use = "the formatted source should be used"]
pub fn format_str_with(source: &str, base_dir: Option<&Path>) -> Result<String, MdsError> {
    let tokens = lexer::tokenize(source, "")?;
    let raw_content = raw_content_spans(&tokens, source);
    let directives = directive_line_offsets(&tokens);
    let body_start = body_start_offset(&tokens, source);

    let formatted = rewrite(source, body_start, &raw_content, &directives);
    assert_equivalent(source, &formatted, base_dir, &raw_content)?;
    Ok(formatted)
}

// ── Token offset helper ────────────────────────────────────────────────────────

/// Extract the byte offset carried by every [`Token`] variant.
fn token_offset(t: &Token) -> usize {
    match t {
        Token::Text(_, o)
        | Token::Interpolation(_, o)
        | Token::EscapedBrace(o)
        | Token::Directive(_, o)
        | Token::FrontmatterFence(o)
        | Token::FrontmatterContent(_, o)
        | Token::CodeFence(_, o)
        | Token::CodeContent(_, o) => *o,
    }
}

// ── Region model ─────────────────────────────────────────────────────────────

/// Compute the start byte offset of every directive line.
///
/// A `Token::Directive` is only ever lexed at line-start, outside code, so its
/// own offset IS the directive line's start offset.
fn directive_line_offsets(tokens: &[Token]) -> BTreeSet<usize> {
    tokens
        .iter()
        .filter_map(|t| match t {
            Token::Directive(_, o) => Some(*o),
            _ => None,
        })
        .collect()
}

/// Compute the byte offset where the compiled "body" begins.
///
/// `clean_output` (see `lib.rs`) only ever runs on the body — the raw
/// frontmatter is stripped off before it and reattached verbatim afterward via
/// `prepend_frontmatter`. A leading frontmatter block is always exactly three
/// consecutive tokens (`FrontmatterFence(0)`, `FrontmatterContent`,
/// `FrontmatterFence`), so the body starts at the fourth token when present.
/// Returns `0` when there is no leading frontmatter (a leading code fence, if
/// any, is ordinary body content subject to R1 only).
fn body_start_offset(tokens: &[Token], src: &str) -> usize {
    let has_frontmatter = matches!(tokens.first(), Some(Token::FrontmatterFence(0)));
    if !has_frontmatter {
        return 0;
    }
    tokens.get(3).map(token_offset).unwrap_or(src.len())
}

/// Compute byte ranges within which content must be copied byte-for-byte:
/// not even R1's `\r` removal may apply. These are the bodies of `@message`
/// and `@define` blocks.
///
/// VERIFIED against the live evaluator: `@message` content is produced by
/// `evaluate_nodes(&block.body, ...)?.trim()` — see `collect_single_message`
/// in `evaluator.rs` — with NO `clean_output` pass. A `\r` inside a message
/// body reaches the compiled JSON verbatim: `@message user:\r\nHi\r\n@end\r\n`
/// compiles to `"Hi\r\n"` (the `\r` survives).
///
/// `@define` bodies get the same conservative treatment: a defined function
/// can be called from a markdown-mode site or from within a `@message` body,
/// and the formatter can't tell which without a full call-graph analysis.
/// `@if`/`@for`/`@block` don't themselves bypass `clean_output`; they
/// only inherit raw-content status by virtue of being lexically nested inside
/// a `@message`/`@define` span, which the stack below naturally captures
/// (the outer span's byte range already covers everything nested within it).
fn raw_content_spans(tokens: &[Token], src: &str) -> Vec<Range<usize>> {
    // (is_message_or_define, start_offset) per currently-open block.
    let mut stack: Vec<(bool, usize)> = Vec::new();
    let mut spans = Vec::new();
    // Defensive cap: the parser separately enforces MAX_NESTING_DEPTH (64) at
    // parse time, but this function only sees tokens (pre-parse), so it
    // cannot rely on that having been checked yet — bound the stack itself.
    const MAX_STACK: usize = 4096;

    for t in tokens {
        let Token::Directive(d, offset) = t else {
            continue;
        };
        let trimmed = d.trim();

        if trimmed == "@end" {
            if let Some((is_raw, start)) = stack.pop() {
                if is_raw {
                    spans.push(start..*offset);
                }
            }
            continue;
        }

        let is_raw_opener = trimmed.starts_with("@message ") || trimmed.starts_with("@define ");
        let is_any_opener = is_raw_opener
            || trimmed.starts_with("@if ")
            || trimmed.starts_with("@for ")
            || trimmed.starts_with("@block ");

        if is_any_opener && stack.len() < MAX_STACK {
            stack.push((is_raw_opener, *offset));
        }
    }

    // Defensive: an unclosed raw-kind block (malformed input the parser will
    // separately reject) still marks its remaining content as raw through
    // EOF, rather than letting R1 touch content that never had a chance
    // to be proven equivalent via the safety gate's real-compile path.
    for (is_raw, start) in stack {
        if is_raw {
            spans.push(start..src.len());
        }
    }

    spans.sort_by_key(|r| r.start);
    spans
}

// ── \r stripping (R1) ────────────────────────────────────────────────────────

/// Append `s` to `out` with every `\r` character removed (R1). Allocation-free
/// in the common case where `s` contains no `\r`.
fn push_stripped_cr(out: &mut String, s: &str) {
    if s.as_bytes().contains(&b'\r') {
        out.extend(s.chars().filter(|&c| c != '\r'));
    } else {
        out.push_str(s);
    }
}

/// Return `s` with every `\r` character removed (R1), borrowing when possible.
fn strip_cr(s: &str) -> std::borrow::Cow<'_, str> {
    if s.as_bytes().contains(&b'\r') {
        std::borrow::Cow::Owned(s.chars().filter(|&c| c != '\r').collect())
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

// ── Rewrite (R1, R2, R4) ─────────────────────────────────────────────────────

/// Rewrite `source` into its formatted form.
///
/// Single left-to-right pass over the original source (no per-line substring
/// re-scans of already-visited bytes, so this stays linear). Idempotent by
/// construction: trimming an already-trimmed directive line is a no-op,
/// and every rule acts on a disjoint line classification (directive /
/// raw-content / ordinary, checked in that priority order — see `rewrite_body`).
fn rewrite(
    source: &str,
    body_start: usize,
    raw_content: &[Range<usize>],
    directives: &BTreeSet<usize>,
) -> String {
    if source.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(source.len() + 1);

    // The leading frontmatter span (if any) is copied verbatim, mod \r — it is
    // invisible to `clean_output`'s trailing-edge normalisation, which only ever
    // sees the body.
    if body_start > 0 {
        debug_assert!(
            source.is_char_boundary(body_start),
            "body_start ({body_start}) must be a char boundary in source"
        );
        push_stripped_cr(&mut out, &source[..body_start]);
    }

    let body = rewrite_body(source, body_start, raw_content, directives);
    let trimmed = body.trim_end();

    if trimmed.is_empty() {
        // Whole body is empty/whitespace-only -> matches clean_output(body) == "".
        if out.is_empty() {
            return String::new();
        }
        // Frontmatter-only document: ensure exactly one final newline even in
        // the edge case where the source had none (e.g. truncated right after
        // the closing `---` fence).
        if !out.ends_with('\n') {
            out.push('\n');
        }
        return out;
    }

    out.push_str(trimmed);
    out.push('\n');
    out
}

/// Rewrite the body portion of `source` (from `body_start` to the end).
///
/// Each line is classified in priority order:
/// 1. **Directive** — R4 (trailing-whitespace strip) + R1 (\r strip); directive
///    text is never part of any compiled output in any mode, so trimming it is
///    always safe, even inside a raw-content span.
/// 2. **Raw content** (`@message`/`@define` bodies) — copied byte-for-byte,
///    including any nested code-fence content: this content bypasses
///    `clean_output` entirely (see `raw_content_spans`), so not even R1's \r
///    removal may touch it.
/// 3. **Everything else** (ordinary body, code-fence lines) — R1 (\r strip)
///    only; interior content is left verbatim (interior-verbatim contract, #150).
fn rewrite_body(
    source: &str,
    body_start: usize,
    raw_content: &[Range<usize>],
    directives: &BTreeSet<usize>,
) -> String {
    debug_assert!(
        raw_content.windows(2).all(|w| w[0].end <= w[1].start),
        "raw_content spans must be sorted and non-overlapping"
    );

    let end = source.len();
    let mut out = String::with_capacity(end.saturating_sub(body_start));

    let mut pos = body_start;
    let mut ri: usize = 0;

    // Skip past any raw spans that end at or before body_start.
    while ri < raw_content.len() && raw_content[ri].end <= body_start {
        ri += 1;
    }

    while pos < end {
        debug_assert!(
            source.is_char_boundary(pos),
            "byte offset {pos} is not a char boundary in source"
        );
        let line_start = pos;
        let line_end = source[pos..].find('\n').map(|rel| pos + rel).unwrap_or(end);
        let had_newline = line_end < end;
        let next_pos = if had_newline { line_end + 1 } else { line_end };

        while ri < raw_content.len() && raw_content[ri].end <= line_start {
            ri += 1;
        }
        let is_raw_content = ri < raw_content.len()
            && raw_content[ri].start <= line_start
            && line_start < raw_content[ri].end;

        let raw_line = &source[line_start..line_end];

        if directives.contains(&line_start) {
            // Priority 1: directive lines — R4 + R1.
            let no_cr = strip_cr(raw_line);
            out.push_str(no_cr.trim_end());
            if had_newline {
                out.push('\n');
            }
        } else if is_raw_content {
            // Priority 2: @message/@define bodies — verbatim copy, not even R1.
            out.push_str(raw_line);
            if had_newline {
                out.push('\n');
            }
        } else {
            // Priority 3: ordinary body and code-fence lines — R1 only.
            push_stripped_cr(&mut out, raw_line);
            if had_newline {
                out.push('\n');
            }
        }

        pos = next_pos;
    }

    out
}

// ── Safety gate ──────────────────────────────────────────────────────────────

/// Verify that `formatted` compiles to the same output as `source`.
///
/// When `source` compiles standalone, this is an exact check: both are
/// compiled with the same `base_dir` and their [`crate::CompiledOutput`]s must
/// match. `compile_str_collecting_warnings` (not `compile_str_with`) is used
/// so the gate never duplicates warnings to stderr on every format call.
///
/// When `source` does NOT compile standalone the reason matters:
///
/// - A [`MdsError::Syntax`] means the source is malformed at the lex/parse
///   level — most notably an unclosed `@message`/`@if`/`@for`/`@define`/
///   `@block` block, which (unlike an unclosed code fence or interpolation)
///   tokenizes cleanly and only fails when the parser looks for the matching
///   `@end`. There is no well-formed program to format, so the real syntax
///   error is surfaced verbatim — matching `mds build` / `mds check` — instead
///   of silently emitting a still-broken file or a misleading
///   `FormatterInvariant` for what is ordinary author error.
/// - Any OTHER compile error (a minority case — typically an undefined runtime
///   variable or function; imports still resolve via `base_dir`) means the
///   source parsed into a well-formed token stream and only failed later during
///   name resolution / evaluation. Those templates are legitimately formattable,
///   so the gate falls back to a structural, rule-aware token comparison and
///   still succeeds on sources that only compile with runtime vars supplied at
///   render time.
fn assert_equivalent(
    source: &str,
    formatted: &str,
    base_dir: Option<&Path>,
    raw_content: &[Range<usize>],
) -> Result<(), MdsError> {
    match crate::compile_str_collecting_warnings(source, base_dir, None) {
        Ok(orig) => match crate::compile_str_collecting_warnings(formatted, base_dir, None) {
            Ok(after) if after.output == orig.output => Ok(()),
            Ok(_) => Err(MdsError::formatter_invariant(
                "formatted source compiles to different output than the original",
            )),
            Err(e) => Err(MdsError::formatter_invariant(format!(
                "formatted source failed to compile though the original succeeded: {e}"
            ))),
        },
        // A lex/parse `Syntax` error means the source is genuinely malformed
        // (e.g. an unclosed `@message`/`@if`/`@for` block, which tokenizes but
        // fails at parse time). There is nothing safe to format: surface the
        // real error rather than papering over it with the structural check.
        Err(e @ MdsError::Syntax { .. }) => Err(e),
        // Any other compile failure (undefined var/fn, unresolved import, …)
        // means the token stream is well-formed and only later analysis failed,
        // so the rule-aware structural comparison is a meaningful fallback.
        Err(_) => {
            if structural_equivalent(source, formatted, raw_content) {
                Ok(())
            } else {
                Err(MdsError::formatter_invariant(
                    "formatted source diverges structurally from the original, and the \
                     original does not compile standalone to verify equivalence directly",
                ))
            }
        }
    }
}

/// Returns `true` when `offset` falls inside any span in `raw_content`.
///
/// Requires `raw_content` to be sorted by start offset and non-overlapping
/// (as guaranteed by [`raw_content_spans`]), enabling O(log S) binary search
/// rather than an O(S) linear scan per call.
fn in_raw_content(raw_content: &[Range<usize>], offset: usize) -> bool {
    // partition_point returns the first index where r.end > offset, i.e. the
    // first span that has NOT already ended before our offset.
    let idx = raw_content.partition_point(|r| r.end <= offset);
    idx < raw_content.len() && raw_content[idx].start <= offset
}

/// Pop trailing `Text` tokens from `tokens` that contribute nothing to compiled output.
///
/// A tail `Text` token is insignificant when **all** three conditions hold:
/// 1. Its source byte offset is **outside** every span in `raw_content` — inside a
///    `@message`/`@define` body, a trailing blank line is real content that bypasses
///    `clean_output` and reaches the compiled JSON verbatim, so it must not be discarded.
/// 2. `crate::clean_output(text).is_empty()` — the token contributes nothing to compiled
///    output (it is whitespace-only: `\n`, `\r\n`, space-only lines, etc.).
/// 3. The token is at the **tail** of the stream. Interior tokens are never touched,
///    so a real formatter bug (dropped interior blank line, dropped meaningful text)
///    still trips the token-count or content-mismatch checks.
///
/// This helper is called on **both** the source and formatted token streams before the
/// length comparison in `structural_equivalent`, which prevents R2's `trim_end()`
/// deletion of a trailing blank-line `Text` token from producing a spurious token-count
/// mismatch and a false `FormatterInvariant` error (RELEASE BLOCKER 3).
fn strip_trailing_insignificant_text(tokens: &mut Vec<Token>, raw_content: &[Range<usize>]) {
    loop {
        let should_pop = match tokens.last() {
            Some(Token::Text(text, offset)) => {
                // Condition 1: offset must be outside every raw-content span.
                // Condition 2: clean_output of the text must be empty.
                !in_raw_content(raw_content, *offset) && crate::clean_output(text).is_empty()
            }
            _ => false, // Not a Text token — stop immediately.
        };
        if !should_pop {
            break;
        }
        tokens.pop();
    }
}

/// Rule-aware structural comparison used when neither `source` nor
/// `formatted` can be compiled standalone (e.g. an undefined runtime
/// variable). Re-tokenizes both and compares token-for-token: `Directive`
/// content after `.trim()`, Frontmatter*/Code* content after `\r` removal
/// (both always safe, matching R4 and R1), and everything else exactly
/// EXCEPT `Text` tokens outside a raw-content span, which are compared after
/// `crate::clean_output` (the SAME function the real compiler applies to
/// markdown-mode output). `Text` tokens whose SOURCE offset falls inside
/// `raw_content` (a `@message`/`@define` body -- see `raw_content_spans`) are
/// compared exactly instead, since that content bypasses `clean_output`
/// entirely; `rewrite` already guarantees such tokens are byte-identical, but
/// comparing them exactly here (rather than via `clean_output`, which would
/// incorrectly treat some byte differences as insignificant) keeps this
/// fallback correct in its own right rather than merely accidentally correct
/// because of that upstream guarantee.
///
/// The raw-content span lookup uses [`in_raw_content`] (binary search, O(log S))
/// rather than a linear scan, since spans are sorted by [`raw_content_spans`]
/// and token offsets from the source tokenization are monotonically increasing.
///
/// Before the length check, [`strip_trailing_insignificant_text`] removes any
/// tail `Text` tokens that are (a) outside raw-content spans and (b) contribute
/// nothing to compiled output (whitespace-only, `clean_output`-empty). This
/// prevents R2's `trim_end()` from producing a spurious count mismatch when a
/// trailing blank line is deleted from the formatted output but still exists as
/// a `Text` token in the source stream (RELEASE BLOCKER 3).
fn structural_equivalent(source: &str, formatted: &str, raw_content: &[Range<usize>]) -> bool {
    let (Ok(mut src_tokens), Ok(mut fmt_tokens)) =
        (lexer::tokenize(source, ""), lexer::tokenize(formatted, ""))
    else {
        return false;
    };

    // Recompute raw-content spans for the formatted stream: `raw_content` uses
    // SOURCE byte offsets, which are not valid when searching against the offsets
    // carried by tokens from the FORMATTED string.
    let fmt_raw_content = raw_content_spans(&fmt_tokens, formatted);

    // Drop trailing Text tokens that are outside raw-content spans and contribute
    // nothing to compiled output (R2 may have deleted such a token from the
    // formatted stream while it still exists in the source stream).
    strip_trailing_insignificant_text(&mut src_tokens, raw_content);
    strip_trailing_insignificant_text(&mut fmt_tokens, &fmt_raw_content);

    if src_tokens.len() != fmt_tokens.len() {
        return false;
    }

    src_tokens
        .iter()
        .zip(fmt_tokens.iter())
        .all(|(a, b)| match (a, b) {
            (Token::Text(ta, oa), Token::Text(tb, _)) => {
                if in_raw_content(raw_content, *oa) {
                    ta == tb
                } else {
                    crate::clean_output(ta) == crate::clean_output(tb)
                }
            }
            (Token::Interpolation(ia, _), Token::Interpolation(ib, _)) => ia == ib,
            (Token::EscapedBrace(_), Token::EscapedBrace(_)) => true,
            (Token::Directive(da, _), Token::Directive(db, _)) => da.trim() == db.trim(),
            (Token::FrontmatterFence(_), Token::FrontmatterFence(_)) => true,
            (Token::FrontmatterContent(ca, _), Token::FrontmatterContent(cb, _)) => {
                ca.replace('\r', "") == cb.replace('\r', "")
            }
            (Token::CodeFence(fa, _), Token::CodeFence(fb, _)) => fa == fb,
            (Token::CodeContent(ca, oa), Token::CodeContent(cb, _)) => {
                if in_raw_content(raw_content, *oa) {
                    ca == cb
                } else {
                    ca.replace('\r', "") == cb.replace('\r', "")
                }
            }
            _ => false,
        })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_line_offsets_finds_at_directives_only() {
        let src = "@if x:\nBody\n@end\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let offsets = directive_line_offsets(&tokens);
        assert_eq!(offsets.len(), 2, "expected @if and @end, got: {offsets:?}");
        assert!(
            offsets.contains(&0),
            "expected offset 0 for @if, got: {offsets:?}"
        );
    }

    #[test]
    fn body_start_offset_zero_without_frontmatter() {
        let src = "Hello\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        assert_eq!(body_start_offset(&tokens, src), 0);
    }

    #[test]
    fn body_start_offset_after_closing_fence_with_frontmatter() {
        let src = "---\nname: x\n---\nHello\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let start = body_start_offset(&tokens, src);
        assert_eq!(&src[start..], "Hello\n");
    }

    #[test]
    fn body_start_offset_end_of_source_when_frontmatter_only() {
        let src = "---\nname: x\n---\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        assert_eq!(body_start_offset(&tokens, src), src.len());
    }

    #[test]
    fn raw_content_spans_covers_message_body_not_surrounding_text() {
        let src = "Before\n@message user:\nHi there\n@end\nAfter\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let spans = raw_content_spans(&tokens, src);
        let covers = |offset: usize| spans.iter().any(|r| r.contains(&offset));
        assert!(
            covers(src.find("Hi there").unwrap()),
            "message body should be raw content"
        );
        assert!(
            !covers(src.find("Before").unwrap()),
            "text before @message must not be raw"
        );
        assert!(
            !covers(src.find("After").unwrap()),
            "text after @end must not be raw"
        );
    }

    #[test]
    fn raw_content_spans_covers_define_body() {
        let src = "@define greet(x):\nHello {x}\n@end\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let spans = raw_content_spans(&tokens, src);
        assert!(spans
            .iter()
            .any(|r| r.contains(&src.find("Hello").unwrap())));
    }

    #[test]
    fn raw_content_spans_excludes_standalone_if_and_block() {
        // @if and @block bodies are NOT raw content on their own -- only
        // @message/@define, and anything lexically nested inside one of them.
        let src = "@if x:\nBody\n@end\n@block b:\nStuff\n@end\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let spans = raw_content_spans(&tokens, src);
        assert!(spans.is_empty(), "expected no raw spans, got: {spans:?}");
    }

    #[test]
    fn raw_content_spans_covers_nested_if_inside_message() {
        // Content nested inside @if, which is itself nested inside @message,
        // is STILL raw -- the outer @message span covers it structurally.
        let src = "@message user:\n@if x:\nNested\n@end\n@end\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let spans = raw_content_spans(&tokens, src);
        assert!(spans
            .iter()
            .any(|r| r.contains(&src.find("Nested").unwrap())));
    }

    #[test]
    fn raw_content_spans_defensive_unclosed_message_covers_to_eof() {
        // Malformed input (missing @end) still marks the remainder as raw
        // rather than leaving it to R1, even though the parser will
        // separately reject this at compile time.
        let src = "@message user:\nHi there";
        let tokens = lexer::tokenize(src, "").unwrap();
        let spans = raw_content_spans(&tokens, src);
        assert!(spans
            .iter()
            .any(|r| r.contains(&src.find("Hi there").unwrap())));
    }

    #[test]
    fn push_stripped_cr_removes_embedded_and_trailing_cr() {
        let mut out = String::new();
        push_stripped_cr(&mut out, "a\rb\r");
        assert_eq!(out, "ab");
    }

    #[test]
    fn strip_cr_borrows_when_no_cr_present() {
        let s = "no carriage returns here";
        assert!(matches!(strip_cr(s), std::borrow::Cow::Borrowed(_)));
    }

    // ── in_raw_content ────────────────────────────────────────────────────────

    #[test]
    fn in_raw_content_empty_spans_always_false() {
        assert!(!in_raw_content(&[], 0));
        assert!(!in_raw_content(&[], 42));
    }

    #[test]
    fn in_raw_content_inside_span() {
        // span covers bytes 10..20
        let span = 10_usize..20_usize;
        let spans = std::slice::from_ref(&span);
        assert!(in_raw_content(spans, 10), "start of span");
        assert!(in_raw_content(spans, 15), "middle of span");
        assert!(!in_raw_content(spans, 20), "end (exclusive) not inside");
        assert!(!in_raw_content(spans, 9), "just before span");
    }

    #[test]
    fn in_raw_content_multiple_sorted_spans() {
        let spans = vec![5_usize..10_usize, 20_usize..30_usize];
        assert!(!in_raw_content(&spans, 4));
        assert!(in_raw_content(&spans, 5));
        assert!(in_raw_content(&spans, 9));
        assert!(!in_raw_content(&spans, 10)); // gap between spans
        assert!(!in_raw_content(&spans, 19));
        assert!(in_raw_content(&spans, 20));
        assert!(in_raw_content(&spans, 29));
        assert!(!in_raw_content(&spans, 30));
    }

    // ── structural_equivalent ─────────────────────────────────────────────────

    // ── strip_trailing_insignificant_text ─────────────────────────────────────

    #[test]
    fn strip_trailing_insignificant_text_removes_trailing_blank_text() {
        // A tail Text token whose clean_output is empty must be popped.
        let mut tokens = vec![
            Token::Directive("@if x:".to_string(), 0),
            Token::Text("\n".to_string(), 7), // meaningful interior line
            Token::Directive("@end".to_string(), 9),
            Token::Text("\n".to_string(), 14), // trailing blank — MUST be stripped
        ];
        let raw: Vec<Range<usize>> = vec![];
        strip_trailing_insignificant_text(&mut tokens, &raw);
        assert_eq!(
            tokens.len(),
            3,
            "trailing blank Text must be removed, remaining: {tokens:?}"
        );
        assert!(
            matches!(tokens.last(), Some(Token::Directive(..))),
            "last token must be the @end directive after stripping"
        );
    }

    #[test]
    fn strip_trailing_insignificant_text_keeps_meaningful_trailing_text() {
        // A tail Text token with non-empty clean_output must NOT be popped.
        let mut tokens = vec![
            Token::Directive("@if x:".to_string(), 0),
            Token::Text("Hello\n".to_string(), 7), // meaningful — clean_output = "Hello\n"
        ];
        let raw: Vec<Range<usize>> = vec![];
        strip_trailing_insignificant_text(&mut tokens, &raw);
        assert_eq!(tokens.len(), 2, "meaningful tail Text must NOT be removed");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // intentional: Vec<Range<usize>> with one element
    fn strip_trailing_insignificant_text_keeps_text_inside_raw_content() {
        // A tail Text inside a raw-content span must NOT be popped, even if
        // clean_output-empty, because @message bodies bypass clean_output entirely.
        let mut tokens = vec![
            Token::Directive("@message user:".to_string(), 0),
            Token::Text("\n".to_string(), 15), // trailing blank INSIDE message body
            Token::Directive("@end".to_string(), 16),
        ];
        // raw_content span covers offset 15 (the trailing \n inside the message body).
        let raw = [0_usize..16_usize];
        strip_trailing_insignificant_text(&mut tokens, &raw);
        // The @end directive is not a Text token, so the loop breaks immediately.
        // The Text("\n", 15) is NOT at the tail (Directive is); nothing is stripped.
        assert_eq!(
            tokens.len(),
            3,
            "nothing should be stripped when tail is Directive"
        );
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // intentional: array of one Range, not Vec<usize>
    fn strip_trailing_insignificant_text_inside_raw_content_not_stripped() {
        // Directly test: a tail Text inside a raw span must survive even if clean_output is empty.
        let mut tokens = vec![
            Token::Directive("@message user:".to_string(), 0),
            Token::Text("\n".to_string(), 15), // offset 15 is inside raw span 0..20
        ];
        let raw = [0_usize..20_usize]; // covers offset 15
        strip_trailing_insignificant_text(&mut tokens, &raw);
        assert_eq!(
            tokens.len(),
            2,
            "tail Text inside raw-content span must NOT be stripped"
        );
    }

    #[test]
    fn strip_trailing_insignificant_text_crlf_is_insignificant() {
        // Text("\r\n") — clean_output strips \r then trim_end → "" → insignificant.
        let mut tokens = vec![
            Token::Directive("@end".to_string(), 0),
            Token::Text("\r\n".to_string(), 5),
        ];
        let raw: Vec<Range<usize>> = vec![];
        strip_trailing_insignificant_text(&mut tokens, &raw);
        assert_eq!(
            tokens.len(),
            1,
            "CRLF-only trailing Text must be treated as insignificant"
        );
    }

    // ── structural_equivalent now ignores trailing insignificant Text ─────────

    #[test]
    fn structural_equivalent_no_false_positive_on_trailing_blank_after_directive() {
        // The exact RELEASE BLOCKER 3 repro:
        // source has a trailing Text("\n") that R2 deletes in formatted.
        // Before the fix, the token count mismatch caused a false FormatterInvariant.
        let source = "@if undefined_var:\nx\n@end\n\n";
        let formatted = "@if undefined_var:\nx\n@end\n";
        let raw: Vec<Range<usize>> = vec![];
        assert!(
            structural_equivalent(source, formatted, &raw),
            "trailing blank Text must not cause a false negative in structural_equivalent"
        );
    }

    #[test]
    fn structural_equivalent_still_rejects_dropped_meaningful_trailing_text() {
        // A real formatter bug: meaningful trailing text was lost.
        // structural_equivalent must still return false in this case.
        let source = "@if undefined_var:\nx\n@end\nSome trailing text\n";
        let formatted = "@if undefined_var:\nx\n@end\n"; // trailing text was dropped!
        let raw: Vec<Range<usize>> = vec![];
        assert!(
            !structural_equivalent(source, formatted, &raw),
            "dropped meaningful trailing text must still be detected as non-equivalent"
        );
    }

    #[test]
    fn structural_equivalent_still_rejects_interior_blank_removal() {
        // A real formatter bug: interior blank line was removed.
        // This must still be detected because strip only removes TAIL tokens.
        let source = "@if undefined_var:\nx\n\ny\n@end\n";
        let formatted = "@if undefined_var:\nx\ny\n@end\n"; // interior \n removed!
        let raw: Vec<Range<usize>> = vec![];
        assert!(
            !structural_equivalent(source, formatted, &raw),
            "interior blank line removal must still be detected as non-equivalent"
        );
    }

    #[test]
    fn structural_equivalent_inside_raw_span_is_byte_exact_not_clean_output() {
        // A @message body is raw content: blank-line runs are NOT normalised by
        // clean_output there. structural_equivalent must compare those tokens
        // byte-for-byte, NOT via clean_output.
        let src = "@message user:\nHi\n\n\nthere\n@end\n";
        // Same text -- must be considered equivalent.
        let same = "@message user:\nHi\n\n\nthere\n@end\n";
        // Collapsed blank lines inside the body -- must NOT be considered equivalent.
        let collapsed = "@message user:\nHi\nthere\n@end\n";

        let tokens = lexer::tokenize(src, "").unwrap();
        let raw = raw_content_spans(&tokens, src);

        assert!(
            structural_equivalent(src, same, &raw),
            "identical source should be structural_equivalent"
        );
        assert!(
            !structural_equivalent(src, collapsed, &raw),
            "collapsing blank lines inside a raw-content span must NOT be \
             considered structural_equivalent"
        );
    }

    #[test]
    fn structural_equivalent_outside_raw_span_uses_clean_output() {
        // Outside @message/@define bodies text tokens are compared via
        // clean_output (interior-verbatim since v2): identical sources are
        // equivalent, but differing blank-line counts are NOT (clean_output
        // no longer caps runs, so a 3-newline run != a 2-newline run).
        let src = "Hello\n\n\nworld\n";
        let same = "Hello\n\n\nworld\n";
        let different = "Hello\n\nworld\n";
        let raw: Vec<Range<usize>> = vec![];

        assert!(
            structural_equivalent(src, same, &raw),
            "identical source must be structural_equivalent"
        );
        assert!(
            !structural_equivalent(src, different, &raw),
            "different blank-line counts must NOT be structural_equivalent \
             (clean_output is interior-verbatim; a 3-newline run != 2-newline run)"
        );
    }
}
