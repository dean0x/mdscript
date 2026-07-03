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
//! 2. Compute *protected* byte ranges (frontmatter + code fence regions, derived
//!    from consecutive token offsets) and the set of *directive line* start
//!    offsets.
//! 3. Rewrite the source in a single left-to-right, line-oriented pass, applying
//!    only rules that are provably output-preserving (R1-R4 below).
//! 4. Run a runtime safety gate (`assert_equivalent`) that re-compiles both the
//!    original and formatted source and hard-errors (`MdsError::FormatterInvariant`)
//!    on any divergence, rather than silently returning a wrong result.
//!
//! # Ruleset (v1)
//!
//! - **R1 (CRLF/CR removal)** — applied to the WHOLE file, including protected
//!   regions. This is the one transform allowed to cross a protected boundary:
//!   `clean_output` strips every `\r` from the final compiled string regardless
//!   of where it came from, frontmatter lexing already filters `\r` out of its
//!   captured content, and directive matching trims `\r` before comparison — so
//!   no downstream consumer of a `\r` byte can ever observe it. Removing it from
//!   the source up front cannot change compiled output.
//! - **R2 (exactly one final newline)** — empty or whitespace-only input becomes
//!   `""`, matching `clean_output`'s own leading/trailing-trim behavior (verified
//!   against the live compiler: `clean_output("   \n") == ""`).
//! - **R3 (blank-line run capping)** — mirrors `clean_output`'s own newline-run
//!   cap *exactly* (verified empirically, not just inferred from its doc
//!   comment): a run of `N` consecutive raw `\n` characters keeps only
//!   `min(N, 2)` of them, and leading blank lines at the very start of the body
//!   (immediately after frontmatter, or at offset 0 if there is none) are
//!   elided entirely rather than merely capped — never applied inside protected
//!   regions.
//! - **R4 (directive trailing-whitespace strip)** — safe because the parser
//!   calls `dir.trim()` before matching, so trailing whitespace on a directive
//!   line is discarded pre-parse and never reaches output.
//!
//! R1 and R3 additionally exclude a THIRD region beyond frontmatter/code
//! fences: `@message` and `@define` bodies (see `raw_content_spans`). This
//! was discovered empirically, not anticipated up front — `@message` content
//! is built by `evaluate_nodes(...).trim()` with NO `clean_output` pass (see
//! `collect_single_message` in `evaluator.rs`), so a `\r` or an uncollapsed
//! blank-line run inside one reaches the compiled message JSON verbatim.
//! `@define` bodies get the same conservative treatment because a function
//! can be called from a markdown-mode site OR from within a `@message` body,
//! and the formatter cannot tell which without a full call-graph analysis.
//! Directive lines (R4) remain safe inside both, since directive text is
//! never part of compiled output in either mode.
//!
//! ## Deliberately NOT implemented: blank-line whitespace stripping
//!
//! An earlier reading of this ruleset called for stripping trailing whitespace
//! from *any* all-whitespace "blank" line. Verified against the live compiler,
//! that is unsound in the general case: `clean_output`'s per-character loop
//! treats a bare space as ordinary content — it resets the newline-run counter
//! and is pushed through verbatim — so a whitespace-only line in the MIDDLE of
//! a document (not the absolute start or end) survives compilation byte for
//! byte (`printf 'Hello\n   \nWorld\n' | mds build -` preserves the three
//! spaces). Stripping it in the formatter would silently break compile
//! equivalence, which this module treats as the non-negotiable constraint that
//! overrides any individual rule's literal description. See
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
/// // CRLF -> LF, and a run of 3 raw newlines caps to 2 (one blank line
/// // survives), exactly matching the compiler's own `clean_output` pass.
/// let formatted = mds::format_str("Hello   \r\n\r\n\r\nworld\r\n")?;
/// assert_eq!(formatted, "Hello   \n\nworld\n");
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
    let protected = protected_spans(&tokens, source);
    let raw_content = raw_content_spans(&tokens, source);
    let directives = directive_line_offsets(&tokens);
    let body_start = body_start_offset(&tokens, source);

    let formatted = rewrite(source, body_start, &protected, &raw_content, &directives);
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

/// Compute the protected byte ranges: the union of Frontmatter* and Code*
/// region ranges.
///
/// Each token's region runs from its own start offset to the next token's
/// start offset (or `src.len()` for the last token) — the lexer already ran
/// the fence/frontmatter state machine, so this reuses its offsets rather than
/// re-detecting fences.
fn protected_spans(tokens: &[Token], src: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let is_protected = matches!(
            t,
            Token::FrontmatterFence(_)
                | Token::FrontmatterContent(_, _)
                | Token::CodeFence(_, _)
                | Token::CodeContent(_, _)
        );
        if !is_protected {
            continue;
        }
        let start = token_offset(t);
        let end = tokens.get(i + 1).map(token_offset).unwrap_or(src.len());
        if end > start {
            spans.push(start..end);
        }
    }
    spans
}

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
/// any, is ordinary body content — it participates in the same leading/middle
/// blank-line handling as everything else).
fn body_start_offset(tokens: &[Token], src: &str) -> usize {
    let has_frontmatter = matches!(tokens.first(), Some(Token::FrontmatterFence(0)));
    if !has_frontmatter {
        return 0;
    }
    tokens.get(3).map(token_offset).unwrap_or(src.len())
}

/// Compute byte ranges within which content must be copied byte-for-byte:
/// neither R1's `\r` removal nor R3's blank-line-run capping may apply. These
/// are the bodies of `@message` and `@define` blocks.
///
/// VERIFIED against the live evaluator (not inferred from the doc comment on
/// `clean_output`): `@message` content is produced by
/// `evaluate_nodes(&block.body, ...)?.trim()` — see `collect_single_message`
/// in `evaluator.rs` — with NO `clean_output` pass. Unlike markdown-mode
/// output, this means a `\r` or an uncollapsed blank-line run inside a
/// message body reaches the compiled JSON verbatim. Confirmed empirically:
/// `@message user:\r\nHi\r\nthere\r\n@end\r\n` compiles to message content
/// `"Hi\r\nthere"` (the `\r` survives), and `@message user:\nHi\n\n\n\nthere\n@end\n`
/// compiles to `"Hi\n\n\n\nthere"` (all 4 raw newlines survive uncapped,
/// unlike markdown mode's `"Hi\n\nthere"`).
///
/// `@define` bodies get the same conservative treatment: a defined function
/// can be called from a markdown-mode site (where the surrounding
/// `clean_output` pass makes pre-collapsing harmless) or from within a
/// `@message` body (where it does not) — the formatter can't tell which
/// without a full call-graph analysis, so every `@define` body is treated as
/// raw. `@if`/`@for`/`@block` don't themselves bypass `clean_output`; they
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
    // EOF, rather than letting R1/R3 touch content that never had a chance
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

// ── Rewrite (R1-R4) ──────────────────────────────────────────────────────────

/// Rewrite `source` into its formatted form.
///
/// Single left-to-right pass over the original source (no per-line substring
/// re-scans of already-visited bytes, so this stays linear). Idempotent by
/// construction: R3's cap can't create a new collapsible run, trimming an
/// already-trimmed directive line is a no-op, and every rule acts on a
/// disjoint line classification (raw-content / protected / directive / blank
/// / content, checked in that priority order — see `rewrite_body`).
fn rewrite(
    source: &str,
    body_start: usize,
    protected: &[Range<usize>],
    raw_content: &[Range<usize>],
    directives: &BTreeSet<usize>,
) -> String {
    if source.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(source.len() + 1);

    // The leading frontmatter span (if any) is copied verbatim, mod \r — it is
    // invisible to `clean_output`'s leading/trailing-trim and newline-run
    // capping, which only ever see the body.
    if body_start > 0 {
        push_stripped_cr(&mut out, &source[..body_start]);
    }

    let body = rewrite_body(source, body_start, protected, raw_content, directives);
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
/// 1. **Directive** — R4 (trailing-whitespace strip) applies unconditionally;
///    directive text is never part of any compiled output, in markdown OR
///    messages mode, so trimming it is always safe.
/// 2. **Raw content** (`@message`/`@define` bodies) — copied byte-for-byte,
///    including any nested code-fence content: this content bypasses
///    `clean_output` entirely (see `raw_content_spans`), so neither R1 nor R3
///    may touch it.
/// 3. **Protected** (frontmatter never reaches here; code-fence lines do) —
///    R1 (\r strip) applies, R3 does not.
/// 4. **Ordinary content** — R1 and R3 (leading-blank elision + blank-run
///    capping) both apply.
fn rewrite_body(
    source: &str,
    body_start: usize,
    protected: &[Range<usize>],
    raw_content: &[Range<usize>],
    directives: &BTreeSet<usize>,
) -> String {
    let end = source.len();
    let mut out = String::with_capacity(end.saturating_sub(body_start));

    let mut pos = body_start;
    let mut leading_mode = true;
    // Mirrors clean_output's own `newline_count`: sits at 1 immediately after
    // any non-blank line (that line's own terminator counts as the first `\n`
    // of a potential run), then increments per subsequent blank line.
    let mut newline_run: usize = 0;
    let mut pi: usize = 0;
    let mut ri: usize = 0;

    // Skip past any protected/raw spans that end at or before body_start (the
    // frontmatter's own spans, when present).
    while pi < protected.len() && protected[pi].end <= body_start {
        pi += 1;
    }
    while ri < raw_content.len() && raw_content[ri].end <= body_start {
        ri += 1;
    }

    while pos < end {
        let line_start = pos;
        let line_end = source[pos..].find('\n').map(|rel| pos + rel).unwrap_or(end);
        let had_newline = line_end < end;
        let next_pos = if had_newline { line_end + 1 } else { line_end };

        while pi < protected.len() && protected[pi].end <= line_start {
            pi += 1;
        }
        let is_protected = pi < protected.len()
            && protected[pi].start <= line_start
            && line_start < protected[pi].end;

        while ri < raw_content.len() && raw_content[ri].end <= line_start {
            ri += 1;
        }
        let is_raw_content = ri < raw_content.len()
            && raw_content[ri].start <= line_start
            && line_start < raw_content[ri].end;

        let raw_line = &source[line_start..line_end];

        if directives.contains(&line_start) {
            // Priority 1: directive lines are never part of any compiled
            // output (markdown or messages mode), so R4's trailing-whitespace
            // strip is always safe, even inside a raw-content span.
            let no_cr = strip_cr(raw_line);
            out.push_str(no_cr.trim_end());
            if had_newline {
                out.push('\n');
            }
            leading_mode = false;
            newline_run = 1;
        } else if is_raw_content {
            // Priority 2: @message/@define body content bypasses
            // clean_output entirely (see raw_content_spans) -- copy exactly,
            // not even R1's \r removal.
            out.push_str(raw_line);
            if had_newline {
                out.push('\n');
            }
            leading_mode = false;
            newline_run = 1;
        } else if is_protected {
            // Priority 3: frontmatter (never reaches here) / code-fence
            // content -- R1 applies, R3 does not.
            push_stripped_cr(&mut out, raw_line);
            if had_newline {
                out.push('\n');
            }
            leading_mode = false;
            newline_run = 1;
        } else {
            let no_cr = strip_cr(raw_line);
            if no_cr.is_empty() {
                // Truly blank line (zero-width -- nothing between the newlines,
                // not merely whitespace; see module docs for why a
                // whitespace-only line is NOT treated as blank here).
                if leading_mode {
                    // Elide entirely: no content, no newline.
                } else {
                    newline_run += 1;
                    if newline_run <= 2 && had_newline {
                        out.push('\n');
                    }
                    // newline_run > 2: drop this blank line's newline (R3 cap).
                }
            } else {
                out.push_str(&no_cr);
                if had_newline {
                    out.push('\n');
                }
                leading_mode = false;
                newline_run = 1;
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
fn structural_equivalent(source: &str, formatted: &str, raw_content: &[Range<usize>]) -> bool {
    let (Ok(src_tokens), Ok(fmt_tokens)) =
        (lexer::tokenize(source, ""), lexer::tokenize(formatted, ""))
    else {
        return false;
    };

    if src_tokens.len() != fmt_tokens.len() {
        return false;
    }

    src_tokens
        .iter()
        .zip(fmt_tokens.iter())
        .all(|(a, b)| match (a, b) {
            (Token::Text(ta, oa), Token::Text(tb, _)) => {
                if raw_content.iter().any(|r| r.contains(oa)) {
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
                if raw_content.iter().any(|r| r.contains(oa)) {
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
    fn protected_spans_covers_frontmatter_and_code_fence() {
        let src = "---\nname: x\n---\n```\ncode\n```\nAfter\n";
        let tokens = lexer::tokenize(src, "").unwrap();
        let spans = protected_spans(&tokens, src);
        // Every byte of the frontmatter block and the code fence block should
        // be covered by some protected span; "After\n" should not be.
        let covers = |offset: usize| spans.iter().any(|r| r.contains(&offset));
        assert!(covers(0), "frontmatter fence start should be protected");
        assert!(
            covers(src.find("name").unwrap()),
            "frontmatter content should be protected"
        );
        assert!(
            covers(src.find("```").unwrap()),
            "code fence should be protected"
        );
        assert!(
            covers(src.find("code").unwrap()),
            "code content should be protected"
        );
        assert!(
            !covers(src.find("After").unwrap()),
            "body text after the fence should not be protected"
        );
    }

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
        // rather than leaving it to R1/R3, even though the parser will
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
}
