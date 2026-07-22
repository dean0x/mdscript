//! Lint rule: `legacy-interpolation` — detects and migrates MDS v0.x single-brace
//! interpolation syntax (`{x}`) to the new double-brace syntax (`{{x}}`).
//!
//! ## Detection strategy
//!
//! TOKEN-based, not AST-based. Fenced code blocks become `Node::Text` in the AST
//! so AST scanning would false-positive on code examples. Instead, we scan
//! `Token::Text` tokens, which structurally skip fences/directives/frontmatter.
//!
//! ## What we detect
//!
//! 1. `\{` / `\}` remnants — the old escape sequence; fix = delete the backslash.
//! 2. `${...}` patterns — skip to first `}` on same scan; `$` alone is plain text.
//! 3. Single `{expr}` where `expr` passes the OLD grammar (identifier / call / dot-path):
//!    fix = replace `{` → `{{` and `}` → `}}`.
//! 4. `@message {expr}:` directive (single-brace dynamic role) → fix `{{expr}}`.

use crate::error::SerializedSpan;
use crate::lexer::Token;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::TextEdit;

const RULE: &str = "legacy-interpolation";

/// Check for legacy single-brace interpolation syntax in the token stream.
pub(crate) fn check(
    tokens: &[Token],
    source: &str,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    let severity = match config.severity_for(RULE).copied() {
        Some(Severity::Off) => return,
        Some(s) => s,
        None => Severity::Warn, // default
    };

    for token in tokens {
        match token {
            Token::Text(text, base_offset) => {
                check_text(text, *base_offset, source, filename, severity, builder);
            }
            Token::Directive(line, offset) => {
                check_directive(line, *offset, source, filename, severity, builder);
            }
            _ => {}
        }
    }
}

/// Scan a Token::Text for legacy interpolation patterns.
fn check_text(
    text: &str,
    base: usize,
    _source: &str,
    filename: &str,
    severity: Severity,
    builder: &mut LintResultBuilder,
) {
    let chars: Vec<char> = text.chars().collect();
    // Build byte offsets for accurate source positions
    let byte_offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let ch = chars[i];

        // Pattern: `\{` or `\}` — old escape remnants
        if ch == '\\' && i + 1 < n && (chars[i + 1] == '{' || chars[i + 1] == '}') {
            let local_off = byte_offsets[i];
            let src_off = base + local_off;
            let diag = LintDiagnostic {
                rule: RULE.to_string(),
                severity,
                message: format!(
                    "legacy escape `\\{}` — single-brace escapes are no longer needed; remove the backslash",
                    chars[i + 1]
                ),
                help: Some("Run `mds lint --fix` to remove the backslash automatically.".to_string()),
                span: Some(SerializedSpan {
                    offset: src_off,
                    length: 2,
                    line: None,
                    column: None,
                }),
                file: Some(filename.to_string()),
                fix_removals: None,
                fix_edits: Some(vec![TextEdit {
                    start: src_off,
                    end: src_off + 1, // remove just the backslash
                    new_text: String::new(),
                }]),
            };
            if !builder.push(diag) {
                return;
            }
            i += 2;
            continue;
        }

        // Pattern: `${...}` — skip to first `}`; this is not MDS interpolation
        if ch == '$' && i + 1 < n && chars[i + 1] == '{' {
            // Skip to first `}`
            let mut j = i + 2;
            while j < n && chars[j] != '}' {
                j += 1;
            }
            // Past the `}` (or end of text if no close found)
            i = if j < n { j + 1 } else { j };
            continue;
        }

        // Pattern: single `{expr}` — only if expr passes old grammar
        if ch == '{' {
            // Find close via string-aware scan
            if let Some((inner, close_idx)) = find_legacy_close(&chars, i + 1) {
                // Validate inner expression under old grammar
                if is_legacy_expr(&inner) {
                    let local_open = byte_offsets[i];
                    let local_close = byte_offsets[close_idx]; // position of `}`
                    let src_open = base + local_open;
                    let src_close = base + local_close;
                    let span_len = local_close - local_open + 1; // includes `}`

                    let diag = LintDiagnostic {
                        rule: RULE.to_string(),
                        severity,
                        message: format!(
                            "legacy single-brace interpolation `{{{inner}}}` — use `{{{{{inner}}}}}` (double braces)"
                        ),
                        help: Some("Run `mds lint --fix` to migrate to `{{x}}` syntax automatically.".to_string()),
                        span: Some(SerializedSpan {
                            offset: src_open,
                            length: span_len,
                            line: None,
                            column: None,
                        }),
                        file: Some(filename.to_string()),
                        fix_removals: None,
                        fix_edits: Some(vec![
                            TextEdit {
                                start: src_open,
                                end: src_open + 1, // replace `{` with `{{`
                                new_text: "{{".to_string(),
                            },
                            TextEdit {
                                start: src_close,
                                end: src_close + 1, // replace `}` with `}}`
                                new_text: "}}".to_string(),
                            },
                        ]),
                    };
                    if !builder.push(diag) {
                        return;
                    }
                    i = close_idx + 1;
                    continue;
                }
            }
        }

        i += 1;
    }
}

/// Scan Token::Directive for `@message {expr}:` pattern.
fn check_directive(
    line: &str,
    offset: usize,
    source: &str,
    filename: &str,
    severity: Severity,
    builder: &mut LintResultBuilder,
) {
    // Only @message with single-brace dynamic role
    let trimmed = line.trim();
    let Some(rest) = trimmed.strip_prefix("@message ") else {
        return;
    };
    let rest = rest.trim();
    // Strip trailing colon
    let Some(role_part) = rest.strip_suffix(':') else {
        return;
    };
    let role_part = role_part.trim();

    // Check for single-brace `{expr}` (not double-brace `{{expr}}`)
    if role_part.starts_with('{')
        && !role_part.starts_with("{{")
        && role_part.ends_with('}')
        && !role_part.ends_with("}}")
        && role_part.len() >= 3
    {
        let inner = &role_part[1..role_part.len() - 1];
        if is_legacy_expr(inner) {
            // Find the byte offset of role_part in the source
            if let Some(src_line) = source.get(offset..) {
                if let Some(brace_pos) = src_line.find(role_part) {
                    let src_open = offset + brace_pos;
                    let src_close = src_open + role_part.len() - 1;

                    let diag = LintDiagnostic {
                        rule: RULE.to_string(),
                        severity,
                        message: format!(
                            "legacy single-brace `@message` role `{role_part}` — use `{{{{{inner}}}}}` (double braces)"
                        ),
                        help: Some("Run `mds lint --fix` to update to `{{expr}}` syntax.".to_string()),
                        span: Some(SerializedSpan {
                            offset: src_open,
                            length: role_part.len(),
                            line: None,
                            column: None,
                        }),
                        file: Some(filename.to_string()),
                        fix_removals: None,
                        fix_edits: Some(vec![
                            TextEdit {
                                start: src_open,
                                end: src_open + 1,
                                new_text: "{{".to_string(),
                            },
                            TextEdit {
                                start: src_close,
                                end: src_close + 1,
                                new_text: "}}".to_string(),
                            },
                        ]),
                    };
                    builder.push(diag);
                }
            }
        }
    }
}

/// Find the closing `}` for a legacy `{...}` interpolation starting at `start` in chars.
///
/// String-aware scan matching the old grammar. Returns `(inner_text, close_index)` or None.
fn find_legacy_close(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut i = start;
    let mut content = String::new();
    let mut in_string = false;
    let mut string_char = '"';
    let mut escaped = false;
    let n = chars.len();

    while i < n {
        let c = chars[i];

        if escaped {
            content.push(c);
            escaped = false;
            i += 1;
            continue;
        }

        if in_string {
            match c {
                '\\' => {
                    escaped = true;
                    content.push(c);
                }
                q if q == string_char => {
                    content.push(c);
                    in_string = false;
                }
                _ => content.push(c),
            }
            i += 1;
            continue;
        }

        if c == '}' {
            return Some((content, i));
        }

        if c == '"' || c == '\'' {
            in_string = true;
            string_char = c;
            content.push(c);
            i += 1;
            continue;
        }

        content.push(c);
        i += 1;
    }

    None // unclosed — not a complete legacy interpolation
}

/// Check if `s` is a valid legacy interpolation expression (identifier, dot-path, or call).
fn is_legacy_expr(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() {
        return false;
    }
    // Simple identifier
    if is_valid_identifier(s) {
        return true;
    }
    // Dot path: a.b.c
    if s.split('.').all(is_valid_identifier) && s.contains('.') {
        return true;
    }
    // Function call: name(...)
    if let Some(paren_pos) = s.find('(') {
        let name = s[..paren_pos].trim();
        if is_valid_identifier(name) && s.ends_with(')') {
            return true;
        }
    }
    // Qualified call: obj.method(...)
    if let Some(dot_pos) = s.find('.') {
        let before_dot = &s[..dot_pos];
        let after_dot = &s[dot_pos + 1..];
        if is_valid_identifier(before_dot) {
            if let Some(paren_pos) = after_dot.find('(') {
                let method = after_dot[..paren_pos].trim();
                if is_valid_identifier(method) && after_dot.ends_with(')') {
                    return true;
                }
            }
        }
    }
    false
}

fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::lint::config::LintConfig;

    fn check_src(src: &str) -> Vec<LintDiagnostic> {
        let tokens = tokenize(src, "test.mds").unwrap();
        let mut builder = LintResultBuilder::new();
        check(&tokens, src, "test.mds", &LintConfig::default(), &mut builder);
        builder.build(true).diagnostics
    }

    #[test]
    fn detects_single_brace_identifier() {
        let diags = check_src("Hello {name}!");
        assert_eq!(diags.len(), 1, "should detect one legacy interpolation");
        assert_eq!(diags[0].rule, RULE);
        assert!(diags[0].fix_edits.is_some(), "should have fix_edits");
    }

    #[test]
    fn does_not_flag_double_brace() {
        let diags = check_src("Hello {{name}}!");
        assert!(diags.is_empty(), "double-brace must not be flagged");
    }

    #[test]
    fn does_not_flag_json_like_braces() {
        // `{"key": 1}` — not a valid legacy expr (contains quotes/spaces/colon)
        let diags = check_src(r#"JSON: {"key": 1}"#);
        assert!(diags.is_empty(), "JSON-like brace content must not be flagged");
    }

    #[test]
    fn detects_old_backslash_escape() {
        let diags = check_src(r"Text \{name}");
        assert!(
            !diags.is_empty(),
            "legacy \\{{ escape should be flagged"
        );
        assert!(diags[0].fix_edits.is_some());
    }

    #[test]
    fn detects_dot_path() {
        let diags = check_src("Value: {user.name}");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].rule, RULE);
    }

    #[test]
    fn detects_function_call() {
        let diags = check_src("Hello {greet()}");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn no_flag_inside_code_fence() {
        let src = "```\n{name}\n```\n";
        let diags = check_src(src);
        assert!(diags.is_empty(), "inside code fence must not be flagged");
    }

    #[test]
    fn fix_edits_replace_open_and_close() {
        let diags = check_src("Hello {name}!");
        let edits = diags[0].fix_edits.as_ref().unwrap();
        assert_eq!(edits.len(), 2, "two edits: open and close brace");
        assert_eq!(edits[0].new_text, "{{");
        assert_eq!(edits[1].new_text, "}}");
    }
}
