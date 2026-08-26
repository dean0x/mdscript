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

pub(crate) const RULE: &str = "legacy-interpolation";

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
                check_text(text, *base_offset, filename, severity, builder);
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
                        fix_edits: Some(vec![TextEdit {
                            start: src_open,
                            end: src_close + 1, // replace entire `{expr}` span atomically
                            new_text: format!("{{{{{}}}}}", inner), // `{expr}` → `{{expr}}`
                        }]),
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
                        fix_edits: Some(vec![TextEdit {
                            start: src_open,
                            end: src_close + 1, // replace entire `{expr}` span atomically
                            new_text: format!("{{{{{}}}}}", inner), // `{expr}` → `{{expr}}`
                        }]),
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
    if s.contains('.') && s.split('.').all(is_valid_identifier) {
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
        check(
            &tokens,
            src,
            "test.mds",
            &LintConfig::default(),
            &mut builder,
        );
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
        assert!(
            diags.is_empty(),
            "JSON-like brace content must not be flagged"
        );
    }

    #[test]
    fn detects_old_backslash_escape() {
        let diags = check_src(r"Text \{name}");
        assert!(!diags.is_empty(), "legacy \\{{ escape should be flagged");
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
    fn detects_message_dynamic_role_directive() {
        // `@message {role}:` (single-brace dynamic role) → flagged with atomic fix.
        let diags = check_src("@message {role}:\nhi\n@end\n");
        assert_eq!(
            diags.len(),
            1,
            "single-brace @message role should be flagged"
        );
        assert_eq!(diags[0].rule, RULE);
        let edits = diags[0].fix_edits.as_ref().unwrap();
        assert_eq!(edits.len(), 1, "single atomic edit for @message role");
        // `{role}` starts at byte 9 in "@message {role}:" and spans 6 bytes.
        assert_eq!(edits[0].start, 9, "fix starts at `{{` of the role");
        assert_eq!(edits[0].end, 15, "fix ends exclusive after `}}`");
        assert_eq!(
            edits[0].new_text, "{{role}}",
            "role migrated to double brace"
        );
    }

    #[test]
    fn does_not_flag_double_brace_message_role() {
        // `@message {{role}}:` is already new syntax — must not be flagged.
        let diags = check_src("@message {{role}}:\nhi\n@end\n");
        assert!(
            diags.is_empty(),
            "double-brace @message role must not be flagged"
        );
    }

    #[test]
    fn does_not_flag_bareword_message_role() {
        // `@message system:` bare-word role — no braces, nothing to migrate.
        let diags = check_src("@message system:\nhi\n@end\n");
        assert!(
            diags.is_empty(),
            "bare-word @message role must not be flagged"
        );
    }

    #[test]
    fn multibyte_prefix_offsets_are_byte_accurate() {
        // Non-ASCII prefix (`é` is 2 UTF-8 bytes) must not skew the byte offsets of
        // the fix edit — risk area: multi-byte offset correctness.
        // "café {name}!": c0 a1 f2 é3-4 space5 {6 n7 a8 m9 e10 }11 !12
        let src = "café {name}!";
        let diags = check_src(src);
        assert_eq!(
            diags.len(),
            1,
            "one legacy interpolation after a multibyte prefix"
        );
        let edits = diags[0].fix_edits.as_ref().unwrap();
        assert_eq!(
            edits[0].start, 6,
            "`{{` sits at byte 6 after `café ` (é is 2 bytes)"
        );
        assert_eq!(edits[0].end, 12, "exclusive end after `}}` at byte 12");
        assert_eq!(edits[0].new_text, "{{name}}");
        // The edit range must land on char boundaries in the original source.
        assert!(src.is_char_boundary(edits[0].start));
        assert!(src.is_char_boundary(edits[0].end));
        // Applying the edit yields the correctly-migrated string.
        let mut applied = src.to_string();
        applied.replace_range(edits[0].start..edits[0].end, &edits[0].new_text);
        assert_eq!(applied, "café {{name}}!");
    }

    #[test]
    fn fix_edits_single_atomic_replacement() {
        // Regression: must emit ONE atomic TextEdit, not split open/close.
        // Split edits allow per-edit fallback to accept only the close-brace half,
        // producing `{expr}}` garbage that compounds on repeated `lint --fix` runs.
        let src = "Hello {name}!";
        let diags = check_src(src);
        let edits = diags[0].fix_edits.as_ref().unwrap();
        assert_eq!(edits.len(), 1, "single atomic edit — no split open/close");
        // `{name}` starts at byte 6, ends exclusive at byte 12
        assert_eq!(edits[0].start, 6, "start at opening `{{`");
        assert_eq!(edits[0].end, 12, "end exclusive after closing `}}`");
        assert_eq!(
            edits[0].new_text, "{{name}}",
            "full span replaced atomically"
        );
    }

    /// Rule=off suppresses all findings (AC-F23: suppressible via config).
    #[test]
    fn rule_off_suppresses() {
        // Source with multiple legacy-interpolation patterns: single-brace var,
        // dot-path, and old backslash-escape — all must be suppressed by Off.
        let src = "Hello {name}! Value: {user.id}. Old escape: \\{x}.";
        let tokens = tokenize(src, "test.mds").unwrap();
        let mut builder = LintResultBuilder::new();
        let config = LintConfig {
            rules: [(RULE.to_string(), Severity::Off)].into_iter().collect(),
        };
        check(&tokens, src, "test.mds", &config, &mut builder);
        assert!(
            builder.build(true).diagnostics.is_empty(),
            "rule=off must suppress all legacy-interpolation findings"
        );
    }
}
