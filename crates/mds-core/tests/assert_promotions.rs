//! Guard (#220): the invariants promoted out of `debug_assert!` stay promoted, their
//! messages stay free of user data, and the two span sites that were changed to degrade
//! stay degraded.
//!
//! The behavioural half of #220 is pinned by unit tests (release-profile runs prove the
//! asserts fire and the degradations do not panic). This file pins the *shape* the
//! behaviour depends on and that a behavioural test cannot see:
//!
//! * an unconditional `assert!`/`assert_eq!`, never a `debug_` form — a demotion would
//!   still pass every debug-profile test in CI, which is exactly how these invariants
//!   came to be debug-only in the first place;
//! * a message with no `{` in it — a promoted assert is a release-build panic whose text
//!   reaches the user's terminal through the CLI's default panic handler, so it must
//!   never interpolate a block name, an offset, or any source text;
//! * a justification comment naming the issue within twelve lines above the assert, so
//!   the next reader learns why this one is release-critical when the surrounding
//!   assertions are not.
//!
//! The scan is a fixed per-site table, not a repo-wide sweep: every OTHER `debug_assert!`
//! in the workspace is deliberately debug-only, so a blanket rule would be wrong.

use std::path::Path;

/// A promoted invariant: which file, which anchor, which assertion macro after that
/// anchor (1-based), the macro form it must have, and its exact message.
struct Promoted {
    file: &'static str,
    anchor: &'static str,
    nth: usize,
    macro_form: &'static str,
    message: &'static str,
}

/// This table is the single source of truth for the promoted message wording; the
/// `#[should_panic(expected = ...)]` strings in the unit tests are substrings of it.
const PROMOTED: &[Promoted] = &[
    Promoted {
        file: "resolver/inheritance.rs",
        anchor: "pub(super) fn spliced_regions<'a>(",
        nth: 1,
        macro_form: "assert!",
        message: "skeleton @block has no effective_blocks entry: the override map was not built \
                  for this skeleton, so a child's @block override would be silently dropped from \
                  the compiled output",
    },
    Promoted {
        file: "lint/diagnostic.rs",
        anchor: "pub fn neutralize_source_for_render(s: &str) -> Cow<'_, str> {",
        nth: 1,
        macro_form: "assert_eq!",
        message: "neutralize_source_for_render changed the byte length of the source: every span \
                  offset and caret column after the first substitution would be shifted",
    },
    Promoted {
        file: "lint/diagnostic.rs",
        anchor: "pub fn new(start: usize, end: usize, new_text: impl Into<String>) -> Self {",
        nth: 1,
        macro_form: "assert!",
        message: "TextEdit::new: start is greater than end; a reversed byte range is not an edit \
                  and the fix applier would skip it in silence",
    },
    Promoted {
        file: "lint/diagnostic.rs",
        anchor: "pub fn range_inclusive(from: usize, to: usize) -> Self {",
        nth: 1,
        macro_form: "assert!",
        message: "FixLineSpan::range_inclusive: from is greater than to; a reversed line range \
                  cannot be turned into a removal and the planner would skip it in silence",
    },
    Promoted {
        file: "lint/diagnostic.rs",
        anchor: "pub fn range_exclusive(from: usize, to: usize) -> Self {",
        nth: 1,
        macro_form: "assert!",
        message: "FixLineSpan::range_exclusive: from is greater than to; a reversed line range \
                  cannot be turned into a removal and the planner would skip it in silence",
    },
    // The three source-map cursor checks live in one function, in this order: Text,
    // EscapedBrace, Interpolation. Addressing them by position also pins that ordering.
    Promoted {
        file: "evaluator.rs",
        anchor: "fn evaluate_nodes(",
        nth: 1,
        macro_form: "assert_eq!",
        message: "source-map cursor desynchronised from the output length at a Text node: every \
                  following segment would map to the wrong output offset",
    },
    Promoted {
        file: "evaluator.rs",
        anchor: "fn evaluate_nodes(",
        nth: 2,
        macro_form: "assert_eq!",
        message: "source-map cursor desynchronised from the output length at an EscapedBrace \
                  node: every following segment would map to the wrong output offset",
    },
    Promoted {
        file: "evaluator.rs",
        anchor: "fn evaluate_nodes(",
        nth: 3,
        macro_form: "assert_eq!",
        message: "source-map cursor desynchronised from the output length at an Interpolation \
                  node: every following segment would map to the wrong output offset",
    },
];

/// A span site that must DEGRADE rather than assert: it routes the line length through
/// the shared helper and no longer slices the source itself.
struct Degraded {
    file: &'static str,
    anchor: &'static str,
    needle: &'static str,
}

const DEGRADED: &[Degraded] = &[
    Degraded {
        file: "resolver.rs",
        anchor: "fn attach_import_span(",
        needle: "line_len_at(",
    },
    Degraded {
        file: "resolver/inheritance.rs",
        anchor: "pub(super) fn check_child_only_blocks(",
        needle: "super::line_len_at(",
    },
];

/// Forms the degraded sites must NOT regain: a boundary assert that is compiled out in
/// release, or the raw slice that panics one line below it.
const DEGRADED_FORBIDDEN: &[&str] = &["debug_assert", "[offset..]"];

#[test]
fn promoted_asserts_are_unconditional_prose_and_justified() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut anchors_found = 0usize;

    for site in PROMOTED {
        let path = src_dir.join(site.file);
        let raw = read(&path);
        let anchor_line = unique_anchor_line(&raw, site.anchor, &path);
        anchors_found += 1;

        let macros = assertion_macros_after(&raw, anchor_line);
        let (macro_line, form, col) = *macros.get(site.nth - 1).unwrap_or_else(|| {
            panic!(
                "{}: expected at least {} assertion macro(s) after `{}`, found {}",
                site.file,
                site.nth,
                site.anchor,
                macros.len()
            )
        });

        assert_eq!(
            form,
            site.macro_form,
            "{}:{}: assertion #{} after `{}` must be an unconditional `{}` — a `debug_` \
             form is compiled out in release, which is the defect #220 removed",
            site.file,
            macro_line + 1,
            site.nth,
            site.anchor,
            site.macro_form
        );

        let args = macro_arguments(&raw, macro_line, col, site.file);
        let literals = string_literals(&args);
        assert_eq!(
            literals.len(),
            1,
            "{}:{}: expected exactly one string literal in the assertion (its message); \
             found {}: {:?}",
            site.file,
            macro_line + 1,
            literals.len(),
            literals
        );
        let message = &literals[0];
        assert_eq!(
            message,
            site.message,
            "{}:{}: the promoted assertion message must match the table verbatim — the \
             table is the wording the `#[should_panic(expected = ...)]` tests pin",
            site.file,
            macro_line + 1
        );
        assert!(
            !message.contains('{'),
            "{}:{}: a promoted assertion message must not interpolate anything — its text \
             is printed to the terminal by the CLI's default panic handler; got: {message}",
            site.file,
            macro_line + 1
        );

        assert!(
            has_justification(&raw, macro_line),
            "{}:{}: no comment naming #220 within the 12 lines above the promoted \
             assertion — a release-critical assert must say why it is one",
            site.file,
            macro_line + 1
        );
    }

    for site in DEGRADED {
        let path = src_dir.join(site.file);
        let masked = mask_comments_and_strings(&read(&path));
        let anchor_line = unique_anchor_line(&masked, site.anchor, &path);
        anchors_found += 1;

        let body = function_body(&masked, anchor_line);
        assert!(
            body.contains(site.needle),
            "{}: `{}` must compute its span length through `{}` so a bad offset degrades \
             to a zero-length span instead of slicing",
            site.file,
            site.anchor,
            site.needle
        );
        for forbidden in DEGRADED_FORBIDDEN {
            assert!(
                !body.contains(forbidden),
                "{}: `{}` must not contain `{}` — that is the debug-only-guard-plus-slice \
                 shape #220 replaced",
                site.file,
                site.anchor,
                forbidden
            );
        }
    }

    // Non-vacuity: a scanner that silently matched nothing would pass every assertion
    // above by never entering a loop body.
    assert_eq!(
        anchors_found,
        PROMOTED.len() + DEGRADED.len(),
        "non-vacuity: every table anchor must have been located"
    );
}

// ── Source scanning helpers ──────────────────────────────────────────────────

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// 0-based index of the line containing `anchor`, asserting the anchor is unique.
fn unique_anchor_line(src: &str, anchor: &str, path: &Path) -> usize {
    let hits: Vec<usize> = src
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(anchor))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "{}: anchor `{anchor}` must occur exactly once; found {} occurrence(s) at lines {:?}",
        path.display(),
        hits.len(),
        hits.iter().map(|i| i + 1).collect::<Vec<_>>()
    );
    hits[0]
}

/// Every assertion macro at or after `start_line`, as `(line, form, byte column)`.
///
/// Comment-only lines are skipped so a rustdoc or justification comment naming a macro
/// is never mistaken for the macro itself.
fn assertion_macros_after(src: &str, start_line: usize) -> Vec<(usize, &'static str, usize)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate().skip(start_line) {
        if line.trim_start().starts_with("//") {
            continue;
        }
        if let Some((form, col)) = first_assertion_macro(line) {
            out.push((i, form, col));
        }
    }
    out
}

/// The first assertion macro token in `line`, as `(form, byte column of the macro name)`.
fn first_assertion_macro(line: &str) -> Option<(&'static str, usize)> {
    let mut from = 0usize;
    // Bounded: each iteration advances `from` past the match it just examined.
    while let Some(rel) = line[from..].find("assert") {
        let at = from + rel;
        let rest = &line[at..];
        let form = if rest.starts_with("assert_eq!(") {
            Some("assert_eq!")
        } else if rest.starts_with("assert!(") {
            Some("assert!")
        } else {
            None
        };
        if let Some(form) = form {
            let debug = line[..at].ends_with("debug_");
            let start = if debug { at - "debug_".len() } else { at };
            let full = match (debug, form) {
                (true, "assert_eq!") => "debug_assert_eq!",
                (true, _) => "debug_assert!",
                (false, f) => f,
            };
            return Some((full, start));
        }
        from = at + "assert".len();
    }
    None
}

/// The text of the macro's argument list, including the enclosing parentheses.
fn macro_arguments(src: &str, line: usize, col: usize, file: &str) -> String {
    let line_start = line_start_offset(src, line);
    let open = src[line_start + col..]
        .find('(')
        .map(|r| line_start + col + r)
        .unwrap_or_else(|| panic!("{file}:{}: assertion macro has no `(`", line + 1));
    let close = match_paren(src, open)
        .unwrap_or_else(|| panic!("{file}:{}: assertion macro has no matching `)`", line + 1));
    src[open..=close].to_string()
}

/// Byte offset of the start of 0-based `line`.
fn line_start_offset(src: &str, line: usize) -> usize {
    let mut offset = 0usize;
    for (i, l) in src.lines().enumerate() {
        if i == line {
            return offset;
        }
        offset += l.len() + 1;
    }
    offset
}

/// Index of the `)` matching the `(` at `open`, skipping string literals, char literals
/// and line comments.
fn match_paren(src: &str, open: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    // Bounded: `i` advances by at least one on every path.
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'"' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // Char literal `'x'` / `'\n'` — bounded lookahead so a lifetime is not
                // mistaken for one.
                let mut j = i + 1;
                let mut steps = 0;
                while j < b.len() && steps < 8 {
                    if b[j] == b'\\' {
                        j += 2;
                        steps += 1;
                        continue;
                    }
                    if b[j] == b'\'' {
                        i = j;
                        break;
                    }
                    j += 1;
                    steps += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Every Rust string literal in `text`, with escapes and `\`-line-continuations resolved
/// to the value the compiler would produce.
fn string_literals(text: &str) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    // Bounded: `i` advances by at least one on every path.
    while i < c.len() {
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c[i] != '"' {
            i += 1;
            continue;
        }
        i += 1;
        let mut lit = String::new();
        while i < c.len() && c[i] != '"' {
            if c[i] != '\\' {
                lit.push(c[i]);
                i += 1;
                continue;
            }
            i += 1;
            match c.get(i) {
                // Line continuation: the newline and all following whitespace vanish.
                Some('\n') => {
                    i += 1;
                    while i < c.len() && (c[i] == ' ' || c[i] == '\t') {
                        i += 1;
                    }
                }
                Some('n') => {
                    lit.push('\n');
                    i += 1;
                }
                Some('t') => {
                    lit.push('\t');
                    i += 1;
                }
                Some(&ch) => {
                    lit.push(ch);
                    i += 1;
                }
                None => break,
            }
        }
        i += 1;
        out.push(lit);
    }
    out
}

/// Is there a line comment naming the issue within the 12 lines above `macro_line`?
fn has_justification(src: &str, macro_line: usize) -> bool {
    let lines: Vec<&str> = src.lines().collect();
    let start = macro_line.saturating_sub(12);
    lines[start..macro_line]
        .iter()
        .any(|l| l.trim_start().starts_with("//") && l.contains("#220"))
}

/// The text of a top-level function body, from its signature line to the closing `}` in
/// column zero.
fn function_body(src: &str, signature_line: usize) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let end = lines
        .iter()
        .enumerate()
        .skip(signature_line)
        .find(|(_, l)| **l == "}")
        .map_or(lines.len(), |(i, _)| i);
    lines[signature_line..end].join("\n")
}

/// Replace the CONTENT of line comments, block comments, string literals and char
/// literals with spaces, preserving overall length and newlines, so a needle that only
/// appears in prose does not count as code.
fn mask_comments_and_strings(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = vec![b' '; b.len()];
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' {
            out[i] = b'\n';
        }
    }
    let mut i = 0usize;
    // Bounded: `i` advances by at least one on every path.
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        if b[i] == b'r' && matches!(b.get(i + 1), Some(&b'"') | Some(&b'#')) && !prev_is_ident(b, i)
        {
            let mut hashes = 0usize;
            let mut j = i + 1;
            while b.get(j) == Some(&b'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&b'"') {
                j += 1;
                while j < b.len() {
                    if b[j] == b'"' && (0..hashes).all(|k| b.get(j + 1 + k) == Some(&b'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    j += 1;
                }
                i = j.min(b.len());
                continue;
            }
        }
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() {
                if b[j] == b'\\' {
                    j += 2;
                    continue;
                }
                if b[j] == b'"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            i = j.min(b.len());
            continue;
        }
        if b[i] == b'\'' {
            let mut j = i + 1;
            let mut closed = false;
            let mut steps = 0;
            while j < b.len() && steps < 8 {
                if b[j] == b'\\' {
                    j += 2;
                    steps += 1;
                    continue;
                }
                if b[j] == b'\'' {
                    closed = true;
                    j += 1;
                    break;
                }
                j += 1;
                steps += 1;
            }
            if closed {
                i = j.min(b.len());
                continue;
            }
        }
        out[i] = b[i];
        i += 1;
    }
    String::from_utf8(out).expect("masking preserves UTF-8 boundaries on ASCII delimiters")
}

fn prev_is_ident(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')
}
