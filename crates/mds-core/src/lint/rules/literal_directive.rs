//! Report conditional lines that stay literal in text or in unmarked code fences.

use crate::error::SerializedSpan;
use crate::lexer::Token;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};

pub(crate) const RULE: &str = "literal-directive";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Conditional {
    If,
    Branch,
    End,
}

/// Match the directive shape, not the condition expression.
fn conditional(line: &str) -> Option<Conditional> {
    let line = line
        .trim_start_matches([' ', '\t'])
        .trim_end_matches([' ', '\t', '\r']);
    match line {
        "@else:" => Some(Conditional::Branch),
        "@end" => Some(Conditional::End),
        _ => {
            let head = line.strip_suffix(':')?;
            for (prefix, kind) in [("@if ", Conditional::If), ("@elseif ", Conditional::Branch)] {
                if head
                    .strip_prefix(prefix)
                    .is_some_and(|s| !s.trim().is_empty())
                {
                    return Some(kind);
                }
            }
            None
        }
    }
}

/// Text tokens can start or end within a line. Only use whole source lines.
fn complete_lines<'a>(
    text: &'a str,
    base: usize,
    source: &str,
) -> impl Iterator<Item = (usize, &'a str)> {
    let first_at_start = base == 0 || source.get(base - 1..base) == Some("\n");
    let source_len = source.len();
    let mut offset = base;
    text.split_inclusive('\n').filter_map(move |line| {
        let start = offset;
        offset += line.len();
        if (first_at_start || start != base) && (line.ends_with('\n') || offset == source_len) {
            Some((start, line.trim_end_matches(['\r', '\n'])))
        } else {
            None
        }
    })
}

fn is_unmarked_example_candidate(opener: &str, offset: usize, source: &str) -> bool {
    // A marked opener stops before the marker or its separator. An unmarked
    // opener stops at a line break, since the fence has content and a closer.
    if !source
        .get(offset + opener.len()..)
        .is_some_and(|rest| rest.starts_with(['\r', '\n']))
    {
        return false;
    }
    let fence = opener.trim_start_matches([' ', '\t', '>']);
    let Some(delimiter) = fence.chars().next() else {
        return false;
    };
    let info = fence.trim_start_matches(delimiter);
    info.split([' ', '\t']).find(|word| !word.is_empty()) != Some("mds")
}

/// Check text and code content tokens. Never inspect directive tokens.
pub(crate) fn check(
    tokens: &[Token],
    source: &str,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    let severity = config.severity_for(RULE).copied().unwrap_or(Severity::Warn);
    if severity == Severity::Off {
        return;
    }

    let mut report = |offset, line: &str| {
        builder.push(
            LintDiagnostic::new(RULE, severity, "This conditional line is literal text.")
                .with_help("Use `+mds` to evaluate fence conditionals. Use the `mds` label for literal fence examples.")
                .with_span(SerializedSpan {
                    offset,
                    length: line.len(),
                    line: None,
                    column: None,
                })
                .with_file(filename),
        )
    };

    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Text(text, base) => {
                for (offset, line) in complete_lines(text, *base, source) {
                    if line.starts_with([' ', '\t'])
                        && conditional(line).is_some()
                        && !report(offset, line)
                    {
                        return;
                    }
                }
            }
            Token::CodeFence(opener, offset) => {
                // An unmarked fence with content has one CodeContent token.
                let Some([Token::CodeContent(text, base), Token::CodeFence(..)]) =
                    tokens.get(index + 1..index + 3)
                else {
                    continue;
                };
                if !is_unmarked_example_candidate(opener, *offset, source) {
                    continue;
                }
                let candidates = || {
                    complete_lines(text, *base, source).filter_map(|(offset, line)| {
                        conditional(line).map(|kind| (offset, line, kind))
                    })
                };
                let mut seen_if = false;
                let has_pair = candidates().any(|(_, _, kind)| {
                    seen_if |= kind == Conditional::If;
                    seen_if && kind == Conditional::End
                });
                if has_pair {
                    for (offset, line, _) in candidates() {
                        if !report(offset, line) {
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::{lint_source, LintConfig, LintResult, Severity};

    const RULE_NAME: &str = "literal-directive";

    fn lint(source: &str) -> LintResult {
        lint_source(source, "test.mds", &LintConfig::default()).unwrap()
    }

    fn flagged_lines<'a>(source: &'a str, result: &LintResult) -> Vec<&'a str> {
        result
            .diagnostics
            .iter()
            .filter(|diag| diag.rule == RULE_NAME)
            .map(|diag| {
                let span = diag.span.as_ref().unwrap();
                source.get(span.offset..span.offset + span.length).unwrap()
            })
            .collect()
    }

    #[test]
    fn warns_on_exact_indented_lines_with_byte_spans() {
        let source = " \t@if x: \r\né\r\n\t@elseif y:\r\n  @else:\r\n \t@end";
        let result = lint(source);
        assert_eq!(
            flagged_lines(source, &result),
            [" \t@if x: ", "\t@elseif y:", "  @else:", " \t@end"]
        );
        assert!(result
            .diagnostics
            .iter()
            .all(|d| d.severity == Severity::Warn));
    }

    #[test]
    fn warns_on_each_exact_line_in_an_unmarked_fence() {
        let source =
            "é\r\n > ~~~~text\r\n\t@if x:\r\n    @elseif y:\r\n@else:\r\n@end\r\n > ~~~~\r\n";
        assert_eq!(
            flagged_lines(source, &lint(source)),
            ["\t@if x:", "    @elseif y:", "@else:", "@end"]
        );
    }

    #[test]
    fn skips_marked_and_mds_labelled_fences() {
        let source = concat!(
            "```text\n@if flag:\nfirst\n@end\n```\n",
            "```\tmds title=example\n@if literal:\nsecond\n@end\n```\n",
            "```text +mds \t\n@if flag:\nthird\n@end\n```\n",
            "```text mds\n@if last:\nfourth\n@end\n```\n",
        );
        assert_eq!(
            flagged_lines(source, &lint(source)),
            ["@if flag:", "@end", "@if last:", "@end"]
        );
    }

    #[test]
    fn requires_an_if_before_an_end_in_the_same_fence() {
        let source = concat!(
            "```\n@if orphan:\n```\n",
            "```\n@end\n```\n",
            "```\n@end\n@if reverse:\n```\n",
            "```\n@if paired:\n@end\n```\n",
        );
        assert_eq!(
            flagged_lines(source, &lint(source)),
            ["@if paired:", "@end"]
        );
    }

    #[test]
    fn skips_near_misses() {
        let source = concat!(
            "```scss\n@if $width {\n@else {\n@end\n```\n",
            "```razor\n@if (loggedIn) {\n@else {\n@end\n```\n",
            "  @if :\n  @elseif:\n  @else\n  @end extra\n  @endnote\n  @END\n",
            "  @if\tx:\n  @if(x):\n  @if x: extra\n  @end\n",
        );
        assert_eq!(flagged_lines(source, &lint(source)), ["  @end"]);
    }

    #[test]
    fn skips_directive_tokens_and_partial_text_lines() {
        let source = concat!(
            "@if flag:\n",
            "{{x}}  @if inline:\n",
            "\\{{  @else:\n",
            "  @else: {{x}}\n",
            "  @end {{x}}\n",
            "  @if literal:\n",
            "@end\n",
        );
        assert_eq!(flagged_lines(source, &lint(source)), ["  @if literal:"]);
    }

    #[test]
    fn warns_on_the_two_literal_example_lines() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/edge-cases/04_escaped_and_code.mds");
        let source = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            flagged_lines(&source, &lint(&source)),
            ["@if condition:", "@end"]
        );
        let labelled = source.replacen("```\n@if condition:", "```mds\n@if condition:", 1);
        assert!(flagged_lines(&labelled, &lint(&labelled)).is_empty());
    }

    #[test]
    fn off_suppresses_the_rule_and_findings_have_no_fix() {
        let source = "  @end\n";
        let (config, unknown) =
            LintConfig::from_rules_checked([(RULE_NAME.to_owned(), Severity::Off)].into());
        let default = lint(source).to_canonical_json();
        assert_eq!(default["files"][0]["diagnostics"][0]["fixable"], false);
        assert!(unknown.is_none());
        assert!(lint_source(source, "test.mds", &config)
            .unwrap()
            .diagnostics
            .is_empty());
    }

    #[test]
    fn message_and_help_do_not_use_source_text() {
        let source = format!(
            "```unique-label\n@if value == \"{}{}\":\n@end\n```\n",
            char::from(27),
            char::from_u32(0x202e).unwrap()
        );
        let result = lint(&source);
        assert_eq!(flagged_lines(&source, &result).len(), 2);
        let plain = lint("```\n@if x:\n@end\n```\n");
        assert_eq!(result.diagnostics[0].message, plain.diagnostics[0].message);
        assert_eq!(result.diagnostics[0].help, plain.diagnostics[0].help);
    }

    #[test]
    fn findings_stop_at_the_diagnostic_cap() {
        let cap = crate::limits::MAX_DIAGNOSTICS;
        let result = lint(&"  @end\n".repeat(cap + 1));
        assert_eq!(result.diagnostics.len(), cap);
        assert!(result.truncated);
    }

    #[test]
    fn compile_preserves_literal_fence_content_without_a_warning() {
        let source = "```\n@if x:\nkeep\n@end\n```\n";
        let findings = crate::lint_str(source).unwrap();
        assert_eq!(flagged_lines(source, &findings), ["@if x:", "@end"]);
        let compiled = crate::compile_str(source).unwrap();
        assert!(compiled.warnings.is_empty());
        assert_eq!(compiled.into_markdown().unwrap(), source);
    }
}
