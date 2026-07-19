//! Rule: `empty-block`
//!
//! **Severity**: Warn (default) | **Tier**: A (auto-fixable)
//!
//! A directive block whose body is empty or contains only whitespace is almost
//! certainly a mistake. Empty blocks produce no output and may indicate a forgotten
//! body, stale template scaffolding, or accidental body erasure.
//!
//! ## Coverage
//!
//! Fires on: `@if`, `@elseif`, `@else`, `@for`, `@define`, `@message`.
//! NEVER fires on: `@block` — empty block bodies are the documented default
//! placeholder pattern (`@block tools:` / `@end` = "inherit parent default").
//!
//! ## Whitespace-only bodies (F2)
//!
//! The lexer emits `Token::Text` for a whitespace-only line between a directive
//! and `@end`. The parser produces a `Node::Text` with whitespace-only `.text`.
//! Confirmed at the parse level in the `f2_whitespace_body_is_text_node` test.
//!
//! ## @message note
//!
//! An empty `@message user:` body may be intentional for priming turns
//! (e.g. an empty assistant placeholder), so this warning is suppressible via
//! `mds.json` `"lint": { "rules": { "empty-block": "off" } }`. The @block
//! exemption rationale is documented above.

use crate::ast::{IfBlock, Module, Node};
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{FixLineSpan, LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;

pub(crate) const RULE: &str = "empty-block";

/// Check the module for empty or whitespace-only block bodies.
pub(crate) fn check(
    module: &Module,
    _ctx: &AnalysisContext,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    let severity = resolve_severity(config);
    if severity == Severity::Off {
        return;
    }

    check_nodes(&module.body, filename, &severity, builder);
}

fn resolve_severity(config: &LintConfig) -> Severity {
    config.severity_for(RULE).copied().unwrap_or(Severity::Warn)
}

/// Recursively check a node list for empty bodies.
///
/// Recursion depth is pre-bounded by the parser's `enter_block` guard
/// (MAX_NESTING_DEPTH=64), so no local depth counter is needed here.
fn check_nodes(
    nodes: &[Node],
    filename: &str,
    severity: &Severity,
    builder: &mut LintResultBuilder,
) {
    for node in nodes {
        match node {
            Node::If(b) => {
                check_if_block(b, filename, severity, builder);
            }
            Node::For(b) => {
                if flag_if_empty(
                    &b.body,
                    filename,
                    severity,
                    make_diag(
                        *severity,
                        filename,
                        "@for body is empty.".to_string(),
                        Some("Add content inside the @for block or remove it.".to_string()),
                        b.offset,
                        "@for".len(),
                        Some(vec![FixLineSpan::single(b.offset)]),
                    ),
                    builder,
                ) {
                    return;
                }
            }
            Node::Define(b) => {
                if flag_if_empty(
                    &b.body,
                    filename,
                    severity,
                    make_diag(
                        *severity,
                        filename,
                        format!("@define '{}' body is empty.", b.name),
                        Some("Add a body to the function or remove the definition.".to_string()),
                        b.offset,
                        "@define".len() + 1 + b.name.len(),
                        Some(vec![FixLineSpan::single(b.offset)]),
                    ),
                    builder,
                ) {
                    return;
                }
            }
            Node::Message(b) => {
                if flag_if_empty(
                    &b.body,
                    filename,
                    severity,
                    make_diag(
                        *severity,
                        filename,
                        "@message body is empty.".to_string(),
                        Some(
                            "Add content to the message block or remove it. \
                             Empty @message is allowed for priming but often accidental."
                                .to_string(),
                        ),
                        b.offset,
                        "@message".len(),
                        None, // @message: fix_removals = None (not auto-fixable by design)
                    ),
                    builder,
                ) {
                    return;
                }
            }
            // @block: intentional placeholder pattern — NEVER flagged.
            Node::Block(b) => {
                check_nodes(&b.body, filename, severity, builder);
            }
            // Leaf nodes.
            Node::Text(_)
            | Node::Interpolation(_)
            | Node::EscapedBrace { .. }
            | Node::Import(_)
            | Node::Export(_)
            | Node::Include(_) => {}
        }
    }
}

fn check_if_block(
    b: &IfBlock,
    filename: &str,
    severity: &Severity,
    builder: &mut LintResultBuilder,
) {
    // Check then-body.
    if flag_if_empty(
        &b.then_body,
        filename,
        severity,
        make_diag(
            *severity,
            filename,
            "@if then-body is empty.".to_string(),
            Some("Add content inside the @if block or remove it.".to_string()),
            b.offset,
            "@if".len(),
            Some(vec![FixLineSpan::single(b.offset)]),
        ),
        builder,
    ) {
        return;
    }

    // Check @elseif branches.
    for branch in &b.elseif_branches {
        if flag_if_empty(
            &branch.body,
            filename,
            severity,
            make_diag(
                *severity,
                filename,
                "@elseif body is empty.".to_string(),
                Some("Add content inside the @elseif block or remove it.".to_string()),
                branch.offset,
                "@elseif".len(),
                Some(vec![FixLineSpan::single(branch.offset)]),
            ),
            builder,
        ) {
            return;
        }
    }

    // Check @else body.
    if let Some(else_body) = &b.else_body {
        let else_off = b.else_offset.unwrap_or(b.offset);
        // Last check in this function — no further work follows regardless of return.
        flag_if_empty(
            else_body,
            filename,
            severity,
            make_diag(
                *severity,
                filename,
                "@else body is empty.".to_string(),
                Some("Add content inside the @else block or remove it.".to_string()),
                else_off,
                "@else".len(),
                Some(vec![FixLineSpan::single(else_off)]),
            ),
            builder,
        );
    }
}

/// If `body` is empty or whitespace-only, push `diag` and return `true`
/// (diagnostic limit reached — caller should stop processing). Otherwise recurse
/// via `check_nodes` and return `false`.
fn flag_if_empty(
    body: &[Node],
    filename: &str,
    severity: &Severity,
    diag: LintDiagnostic,
    builder: &mut LintResultBuilder,
) -> bool {
    if is_empty_or_whitespace(body) {
        !builder.push(diag)
    } else {
        check_nodes(body, filename, severity, builder);
        false
    }
}

/// A body is "empty" if it contains no nodes, OR all nodes are whitespace-only Text.
///
/// **F2 verified**: the parser emits `Node::Text(TextNode { text: "   \n", ... })`
/// for whitespace-only lines between a directive and `@end`, confirmed by the
/// `f2_whitespace_body_is_text_node` test below.
fn is_empty_or_whitespace(body: &[Node]) -> bool {
    body.is_empty()
        || body.iter().all(|node| {
            if let Node::Text(t) = node {
                t.text.chars().all(char::is_whitespace)
            } else {
                false
            }
        })
}

fn make_diag(
    severity: Severity,
    filename: &str,
    message: String,
    help: Option<String>,
    offset: usize,
    length: usize,
    fix_removals: Option<Vec<FixLineSpan>>,
) -> LintDiagnostic {
    LintDiagnostic {
        rule: RULE.to_string(),
        severity,
        message,
        help,
        span: Some(SerializedSpan {
            offset,
            length,
            line: None,
            column: None,
        }),
        file: Some(filename.to_string()),
        fix_removals,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::lint::facts::collect_facts;
    use crate::parser::parse_with_ctx;

    fn lint_src(src: &str) -> Vec<LintDiagnostic> {
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "test.mds",
            &LintConfig::default(),
            &mut builder,
        );
        builder.build(false).diagnostics
    }

    /// F2: parse-level assertion confirming whitespace-only bodies produce a Text node.
    ///
    /// This test is the RED gate for the whitespace-only predicate. If the parser
    /// changes to strip or omit whitespace Text nodes in directive bodies, this test
    /// fails and the `is_empty_or_whitespace` predicate needs adjustment.
    #[test]
    fn f2_whitespace_body_is_text_node() {
        let src = "@if x:\n   \n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let Node::If(block) = &module.body[0] else {
            panic!("expected If block");
        };
        assert_eq!(
            block.then_body.len(),
            1,
            "whitespace-only body should have exactly one Text node, not be empty"
        );
        let Node::Text(t) = &block.then_body[0] else {
            panic!(
                "expected Text node in whitespace-only body, got: {:?}",
                block.then_body[0]
            );
        };
        assert!(
            t.text.chars().all(char::is_whitespace),
            "body Text node should be all whitespace, got: {:?}",
            t.text
        );
    }

    /// L-U-EB1: @if with completely empty body fires.
    #[test]
    fn if_empty_body_fires() {
        let diags = lint_src("@if x:\n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for empty @if body; got: {:?}",
            diags
        );
    }

    /// L-U-EB2: @if with whitespace-only body fires.
    #[test]
    fn if_whitespace_only_body_fires() {
        let diags = lint_src("@if x:\n   \n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for whitespace-only @if body; got: {:?}",
            diags
        );
    }

    /// @if with content does NOT fire.
    #[test]
    fn if_with_content_does_not_fire() {
        let diags = lint_src("@if x:\nhello\n@end\n");
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire when @if body has content"
        );
    }

    /// @for with empty body fires.
    #[test]
    fn for_empty_body_fires() {
        let diags = lint_src("@for x in items:\n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for empty @for body; got: {:?}",
            diags
        );
    }

    /// @define with empty body fires.
    #[test]
    fn define_empty_body_fires() {
        let diags = lint_src("@define greet():\n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for empty @define body; got: {:?}",
            diags
        );
    }

    /// @message with empty body fires.
    #[test]
    fn message_empty_body_fires() {
        let diags = lint_src("@message user:\n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for empty @message body; got: {:?}",
            diags
        );
    }

    /// @block with empty body does NOT fire (intentional placeholder pattern).
    #[test]
    fn block_empty_body_does_not_fire() {
        let diags = lint_src("@block tools:\n@end\n");
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "@block exemption: should NOT fire for empty @block body; got: {:?}",
            diags
        );
    }

    /// TEST-7: @elseif with empty body fires.
    ///
    /// The `@elseif` path was the one uncovered branch among the six directives
    /// checked by `check_if_block` and `check_nodes`. This test locks in that
    /// coverage: when the then-body of @if has content but the @elseif body is
    /// empty, exactly the @elseif finding must fire.
    #[test]
    fn elseif_empty_body_fires() {
        // @if then-body has content ("hello") — only @elseif body is empty.
        let diags = lint_src("@if x:\nhello\n@elseif y:\n@end\n");
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("@elseif")),
            "should fire for empty @elseif body; got: {:?}",
            diags
        );
    }

    /// @else with empty body fires.
    #[test]
    fn else_empty_body_fires() {
        let diags = lint_src("@if x:\nhello\n@else:\n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for empty @else body; got: {:?}",
            diags
        );
    }

    /// The @elseif diagnostic span is anchored at the @elseif line, not the @if line.
    ///
    /// The span must point at the `@elseif` directive itself, via `ElseifBranch.offset`.
    ///
    /// Source layout (ASCII, all bytes):
    ///   "@if x:\nhello\n@elseif y:\n@end\n"
    ///    ^0       ^7     ^13
    /// @elseif is at byte offset 13.
    #[test]
    fn elseif_empty_body_span_at_elseif_offset() {
        // @if then-body has content; only @elseif body is empty.
        let src = "@if x:\nhello\n@elseif y:\n@end\n";
        let diags = lint_src(src);
        let elseif_diag = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@elseif"))
            .expect("expected an @elseif empty-body diagnostic");
        let span = elseif_diag
            .span
            .as_ref()
            .expect("diagnostic must carry a span");
        assert_eq!(
            span.offset, 13,
            "@elseif diagnostic span must be at the @elseif directive (byte 13), \
             not at the @if opener (byte 0); got offset {}",
            span.offset
        );
    }

    /// The @else diagnostic span uses else_offset when present.
    ///
    /// Source layout:
    ///   "@if x:\nhello\n@else:\n@end\n"
    ///    ^0       ^7     ^13
    /// @else is at byte offset 13.
    #[test]
    fn else_empty_body_span_at_else_offset() {
        let src = "@if x:\nhello\n@else:\n@end\n";
        let diags = lint_src(src);
        let else_diag = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@else"))
            .expect("expected an @else empty-body diagnostic");
        let span = else_diag
            .span
            .as_ref()
            .expect("diagnostic must carry a span");
        assert_eq!(
            span.offset, 13,
            "@else diagnostic span must be at the @else directive (byte 13), \
             not at the @if opener (byte 0); got offset {}",
            span.offset
        );
    }

    /// Turning off the rule via config produces no diagnostics.
    #[test]
    fn rule_off_suppresses_all() {
        let src = "@if x:\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        let config = LintConfig {
            rules: [("empty-block".to_string(), Severity::Off)]
                .into_iter()
                .collect(),
        };
        check(&module, &ctx, "test.mds", &config, &mut builder);
        let result = builder.build(false);
        assert!(
            result.diagnostics.is_empty(),
            "rule=off should produce no diagnostics"
        );
    }
}
