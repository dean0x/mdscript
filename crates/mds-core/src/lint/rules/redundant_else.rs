//! Rule: `redundant-else`
//!
//! **Severity**: Warn (default) | **Tier**: C (never auto-fixed)
//!
//! When an `@else` body is structurally identical to the `@if` then-body, the
//! conditional is pointless — both branches produce the same output regardless of
//! the condition. This is almost always a copy-paste mistake.
//!
//! Structural identity ignores all `offset` and `len` fields (position-independent).
//! `f64` literals are compared via `to_bits()` (NaN-safe, parser rejects NaN anyway).
//!
//! ## Tier C: report-only
//!
//! This rule is Tier C because "fixing" it by removing the `@else` changes the
//! conditional structure in a way that may not be what the author intended — they
//! might want to keep the @else and fix the content instead. The diagnostic message
//! guides the author; no auto-fix is generated.

use crate::ast::{Module, Node};
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;
use crate::lint::rules::structural_eq::nodes_eq;

pub(crate) const RULE: &str = "redundant-else";

/// Check the module for @if/@else pairs where both branches are structurally identical.
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
                // Check if @else body is structurally identical to then-body.
                // Only meaningful with NO intervening @elseif branches: with an @elseif
                // present, then==else does NOT make the conditional redundant (the elseif
                // arm still produces different output), so firing here would be a false
                // positive with a factually wrong "same output regardless" message.
                if let Some(else_body) = &b.else_body {
                    if b.elseif_branches.is_empty()
                        && nodes_eq(&b.then_body, else_body)
                        && !builder.push(LintDiagnostic {
                            rule: RULE.to_string(),
                            severity: *severity,
                            message: "The @else body is identical to the @if body — \
                                      the conditional produces the same output regardless \
                                      of the condition."
                                .to_string(),
                            help: Some(
                                "Remove the @else branch or make its content different \
                                 from the @if body."
                                    .to_string(),
                            ),
                            span: Some(SerializedSpan {
                                offset: b.offset,
                                length: "@if".len(),
                                line: None,
                                column: None,
                            }),
                            file: Some(filename.to_string()),
                            fix_removals: None,
                            fix_edits: None,
                        })
                    {
                        return;
                    }
                }

                // Recurse into all branches.
                check_nodes(&b.then_body, filename, severity, builder);
                for branch in &b.elseif_branches {
                    check_nodes(&branch.body, filename, severity, builder);
                }
                if let Some(else_body) = &b.else_body {
                    check_nodes(else_body, filename, severity, builder);
                }
            }
            Node::For(b) => check_nodes(&b.body, filename, severity, builder),
            Node::Define(b) => check_nodes(&b.body, filename, severity, builder),
            Node::Message(b) => check_nodes(&b.body, filename, severity, builder),
            Node::Block(b) => check_nodes(&b.body, filename, severity, builder),
            Node::Text(_)
            | Node::Interpolation(_)
            | Node::EscapedBrace { .. }
            | Node::Import(_)
            | Node::Export(_)
            | Node::Include(_) => {}
        }
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

    /// L-U-RE1: identical then/else bodies fire.
    #[test]
    fn identical_then_else_fires() {
        let src = "@if x:\nhello\n@else:\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for identical then/else; got: {:?}",
            diags
        );
    }

    /// Different content does not fire.
    #[test]
    fn different_then_else_does_not_fire() {
        let src = "@if x:\nhello\n@else:\nworld\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for different then/else; got: {:?}",
            diags
        );
    }

    /// No @else body: should not fire.
    #[test]
    fn no_else_does_not_fire() {
        let src = "@if x:\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire when no @else"
        );
    }

    /// With an intervening @elseif, identical then/else is NOT redundant — the elseif
    /// arm still produces different output, so the rule must not fire (false positive).
    #[test]
    fn identical_then_else_with_elseif_does_not_fire() {
        let src = "@if x:\nhello\n@elseif y:\nworld\n@else:\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "must not fire when an @elseif branch differs; got: {:?}",
            diags
        );
    }

    /// Structural identity ignores offsets — same text at different source positions.
    #[test]
    fn structural_eq_ignores_offsets() {
        // The `hello\n` appears at different byte offsets in then vs else.
        // Structural equality must still consider them identical.
        let src = "@if x:\nhello\n@else:\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "offset difference must not affect structural equality"
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@if x:\nhello\n@else:\nhello\n@end\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        let config = LintConfig {
            rules: [(RULE.to_string(), Severity::Off)].into_iter().collect(),
        };
        check(&module, &ctx, "test.mds", &config, &mut builder);
        assert!(builder.build(false).diagnostics.is_empty());
    }
}
