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
                        Some(vec![FixLineSpan {
                            from: b.offset,
                            to: b.end_offset,
                            to_inclusive: true,
                        }]),
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
                        Some(vec![FixLineSpan {
                            from: b.offset,
                            to: b.end_offset,
                            to_inclusive: true,
                        }]),
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
    // Case ③: no other branches → safe to remove the whole @if block.
    // Case ④: other branches present → leave them intact; fix is unsafe.
    let then_fix = if b.elseif_branches.is_empty() && b.else_body.is_none() {
        Some(vec![FixLineSpan {
            from: b.offset,
            to: b.end_offset,
            to_inclusive: true,
        }])
    } else {
        None // ④ partial-block removal is unsafe
    };
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
            then_fix,
        ),
        builder,
    ) {
        return;
    }

    // Check @elseif branches.
    // Case ⑥: last branch with no @else → remove from elseif up to (not incl.) @end.
    // Case ⑦: not the last branch, or @else follows → fix is unsafe.
    for (i, branch) in b.elseif_branches.iter().enumerate() {
        let is_last = i == b.elseif_branches.len() - 1;
        let elseif_fix = if is_last && b.else_body.is_none() {
            // Case ⑥: terminal empty @elseif with no @else — remove up to @end (exclusive).
            Some(vec![FixLineSpan {
                from: branch.offset,
                to: b.end_offset,
                to_inclusive: false,
            }])
        } else {
            None // ⑦ non-terminal or followed by @else — unsafe
        };
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
                elseif_fix,
            ),
            builder,
        ) {
            return;
        }
    }

    // Check @else body.
    // Case ⑤: always safe to remove the @else clause (remove from @else: up to @end, exclusive).
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
                // Case ⑤: remove from @else: up to (not including) @end.
                Some(vec![FixLineSpan {
                    from: else_off,
                    to: b.end_offset,
                    to_inclusive: false,
                }]),
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
        fix_edits: None,
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

    // ── A3 case matrix: fix_removals descriptor for each coverage case ─────────
    //
    // Each test pins the fix_removals shape (Some/None, span count, to_inclusive)
    // for every case handled by check_nodes / check_if_block. The cases follow
    // the numbering in check_if_block's inline comments (①–⑧).

    /// A3-①: empty @for → fix_removals = Some([whole-block, to_inclusive = true]).
    #[test]
    fn a3_case1_empty_for_fix_removals_is_whole_block_inclusive() {
        let diags = lint_src("@for x in items:\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@for"))
            .expect("expected empty @for diagnostic");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case ①: empty @for must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case ①: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            spans[0].to_inclusive,
            "case ①: whole-block removal must be inclusive (to_inclusive = true); got: {spans:?}"
        );
    }

    /// A3-②: empty @define → fix_removals = Some([whole-block, to_inclusive = true]).
    #[test]
    fn a3_case2_empty_define_fix_removals_is_whole_block_inclusive() {
        let diags = lint_src("@define greet():\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@define"))
            .expect("expected empty @define diagnostic");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case ②: empty @define must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case ②: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            spans[0].to_inclusive,
            "case ②: whole-block removal must be inclusive; got: {spans:?}"
        );
    }

    /// A3-③: empty @if with no other branches → fix_removals = Some([whole-block, to_inclusive = true]).
    #[test]
    fn a3_case3_empty_if_no_branches_fix_removals_is_whole_block_inclusive() {
        let diags = lint_src("@if x:\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@if"))
            .expect("expected empty @if (no branches) diagnostic");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case ③: empty @if (no branches) must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case ③: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            spans[0].to_inclusive,
            "case ③: whole-block removal must be inclusive; got: {spans:?}"
        );
    }

    /// A3-④: empty @if then-body when other branches present → fix_removals = None.
    ///
    /// Partial-block removal is unsafe when @else or @elseif branches exist;
    /// the fix planner must refuse rather than silently drop live branches.
    #[test]
    fn a3_case4_empty_then_body_with_other_branches_fix_removals_is_none() {
        // @if body is empty; @else has content — partial removal would drop @else.
        let diags = lint_src("@if x:\n@else:\nhello\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@if"))
            .expect("expected empty @if then-body diagnostic");
        assert!(
            d.fix_removals.is_none(),
            "case ④: empty @if with other branches must have fix_removals = None \
             (unsafe to auto-fix); got: {:?}",
            d.fix_removals
        );
    }

    /// A3-⑤: empty @else → fix_removals = Some([exclusive span, to_inclusive = false]).
    ///
    /// Removes from the @else: directive up to (not including) the @end line,
    /// leaving the @end intact so the @if structure remains valid.
    #[test]
    fn a3_case5_empty_else_fix_removals_is_exclusive() {
        // @if then-body has content; @else body is empty.
        let diags = lint_src("@if x:\nhello\n@else:\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@else"))
            .expect("expected empty @else diagnostic");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case ⑤: empty @else must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case ⑤: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            !spans[0].to_inclusive,
            "case ⑤: @else removal must be exclusive (to_inclusive = false); got: {spans:?}"
        );
    }

    /// A3-⑥: terminal empty @elseif (no @else follows) → fix_removals = Some([exclusive span]).
    ///
    /// Removes from the @elseif directive up to (not including) @end, leaving
    /// the @end intact. "Terminal" = this is the last @elseif and there is no @else.
    #[test]
    fn a3_case6_terminal_empty_elseif_fix_removals_is_exclusive() {
        // @if has content; @elseif body is empty; no @else.
        let diags = lint_src("@if x:\nhello\n@elseif y:\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@elseif"))
            .expect("expected terminal empty @elseif diagnostic");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case ⑥: terminal empty @elseif must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case ⑥: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            !spans[0].to_inclusive,
            "case ⑥: terminal @elseif removal must be exclusive (to_inclusive = false); \
             got: {spans:?}"
        );
    }

    /// A3-⑦: non-terminal empty @elseif → fix_removals = None.
    ///
    /// When another @elseif or @else follows, removing just this branch would
    /// create an invalid structure; the planner refuses (unsafe to auto-fix).
    #[test]
    fn a3_case7_nonterminal_empty_elseif_fix_removals_is_none() {
        // @if has content; first @elseif body is empty but NOT terminal (another @elseif follows).
        let diags = lint_src("@if x:\nhello\n@elseif y:\n@elseif z:\nworld\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@elseif"))
            .expect("expected non-terminal empty @elseif diagnostic");
        assert!(
            d.fix_removals.is_none(),
            "case ⑦: non-terminal empty @elseif must have fix_removals = None \
             (unsafe to auto-fix); got: {:?}",
            d.fix_removals
        );
    }

    /// A3-⑧: empty @message → fix_removals = None (not auto-fixable by design).
    ///
    /// Empty @message may be intentional (priming turn). The rule fires but no
    /// auto-fix is offered; the user must manually remove or populate it.
    #[test]
    fn a3_case8_empty_message_fix_removals_is_none() {
        let diags = lint_src("@message user:\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("@message"))
            .expect("expected empty @message diagnostic");
        assert!(
            d.fix_removals.is_none(),
            "case ⑧: empty @message must have fix_removals = None \
             (not auto-fixable by design); got: {:?}",
            d.fix_removals
        );
    }
}
