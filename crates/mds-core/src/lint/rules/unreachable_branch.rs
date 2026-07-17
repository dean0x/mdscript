//! Rule: `unreachable-branch`
//!
//! **Severity**: Error (default) | **Tier**: A (auto-fixable)
//!
//! Fires on two distinct patterns:
//!
//! ## Pattern 1: Always-true / always-false literal conditions
//!
//! `@if "x" == "x":` is always-true — later branches (`@elseif`, `@else`) are
//! unreachable. `@if "x" == "y":` is always-false — the then-body is dead code.
//!
//! Only `Condition::Eq` and `Condition::NotEq` are flagged when **both sides are
//! literal expressions** (`StringLiteral`, `NumberLiteral`, `BooleanLiteral`,
//! `NullLiteral`). Variable comparisons are never flagged — the value is not
//! statically known at analysis time.
//!
//! ## Pattern 2: Duplicate structural @elseif conditions
//!
//! A later `@elseif` condition that is structurally identical to an earlier
//! `@if` or `@elseif` condition can never be reached (the earlier arm matched
//! first), making the branch dead.
//!
//! ## F3 precondition: fixtures must pass `check` first
//!
//! The `l_u_ub0_check_gate_passes` test below asserts that each test fixture
//! source passes `mds::check_str`. If a future validator change rejects
//! constant conditions, that test fails loudly, signalling that the unreachable-
//! branch rule would silently produce zero findings on valid inputs.

use crate::ast::{Condition, IfBlock, Module, Node};
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;
use crate::lint::rules::structural_eq::{conditions_eq, exprs_eq, is_literal};

pub(crate) const RULE: &str = "unreachable-branch";

/// Check the module for unreachable branches.
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
    config
        .severity_for(RULE)
        .copied()
        .unwrap_or(Severity::Error)
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
                check_if_block(b, filename, severity, builder);
                // Recurse into bodies.
                check_nodes(&b.then_body, filename, severity, builder);
                for (_, body) in &b.elseif_branches {
                    check_nodes(body, filename, severity, builder);
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

fn check_if_block(
    b: &IfBlock,
    filename: &str,
    severity: &Severity,
    builder: &mut LintResultBuilder,
) {
    // Pattern 1: check the primary @if condition.
    match classify_condition(&b.condition) {
        ConditionClass::AlwaysTrue => {
            // Always-true primary condition → LATER branches (@elseif/@else) are unreachable.
            // Appendix A: "always-true → LATER branches unreachable."
            // If there are no later branches, nothing is unreachable — do not flag (M2 FP fix).
            let has_later_branches = !b.elseif_branches.is_empty() || b.else_body.is_some();
            if has_later_branches
                && !builder.push(make_diag(
                    *severity,
                    filename,
                    "@if condition is always true — @elseif/@else branches are unreachable"
                        .to_string(),
                    Some(
                        "Replace the constant condition with a variable or remove later branches."
                            .to_string(),
                    ),
                    b.offset,
                    "@if".len(),
                ))
            {
                return;
            }
        }
        ConditionClass::AlwaysFalse => {
            // Always-false primary condition → then-body is dead code, regardless of later branches.
            if !builder.push(make_diag(
                *severity,
                filename,
                "@if condition is always false — the then-body is dead code".to_string(),
                Some(
                    "Replace the constant condition with a variable or remove the dead branch."
                        .to_string(),
                ),
                b.offset,
                "@if".len(),
            )) {
                return;
            }
        }
        ConditionClass::Unknown => {}
    }

    // Pattern 2: Duplicate @elseif conditions.
    // Collect all seen conditions in order; flag a branch if its condition equals any prior one.
    let mut seen_conditions: Vec<&Condition> = vec![&b.condition];

    for (cond, _body) in &b.elseif_branches {
        // Check if this @elseif condition duplicates any prior condition.
        let is_duplicate = seen_conditions
            .iter()
            .any(|prior| conditions_eq(prior, cond));

        if is_duplicate {
            // Emit ONE finding for the duplicate. Skip the always-true/false check below —
            // the duplicate detection already identifies this dead code (M4 dedup).
            if !builder.push(make_diag(
                *severity,
                filename,
                "@elseif condition is structurally identical to an earlier branch — \
                 this branch can never be reached."
                    .to_string(),
                Some("Remove the duplicate @elseif branch or change its condition.".to_string()),
                b.offset,
                "@elseif".len(),
            )) {
                return;
            }
        } else {
            // Not a duplicate — check if this @elseif is always-true or always-false.
            match classify_condition(cond) {
                ConditionClass::AlwaysTrue => {
                    if !builder.push(make_diag(
                        *severity,
                        filename,
                        "@elseif condition is always true".to_string(),
                        Some("Replace the constant condition with a variable.".to_string()),
                        b.offset,
                        "@elseif".len(),
                    )) {
                        return;
                    }
                }
                ConditionClass::AlwaysFalse => {
                    if !builder.push(make_diag(
                        *severity,
                        filename,
                        "@elseif condition is always false — this branch is dead code".to_string(),
                        Some(
                            "Replace the constant condition with a variable or remove the dead branch."
                                .to_string(),
                        ),
                        b.offset,
                        "@elseif".len(),
                    )) {
                        return;
                    }
                }
                ConditionClass::Unknown => {}
            }
        }

        seen_conditions.push(cond);
    }
}

enum ConditionClass {
    AlwaysTrue,
    AlwaysFalse,
    Unknown,
}

/// Classify a condition as always-true, always-false, or statically unknown.
///
/// Only `Condition::Eq` and `Condition::NotEq` with BOTH sides being literals are
/// flaggable. Variable comparisons return `Unknown`.
fn classify_condition(cond: &Condition) -> ConditionClass {
    match cond {
        Condition::Eq(lhs, rhs) if is_literal(lhs) && is_literal(rhs) => {
            if exprs_eq(lhs, rhs) {
                ConditionClass::AlwaysTrue
            } else {
                ConditionClass::AlwaysFalse
            }
        }
        Condition::NotEq(lhs, rhs) if is_literal(lhs) && is_literal(rhs) => {
            if exprs_eq(lhs, rhs) {
                ConditionClass::AlwaysFalse
            } else {
                ConditionClass::AlwaysTrue
            }
        }
        _ => ConditionClass::Unknown,
    }
}

fn make_diag(
    severity: Severity,
    filename: &str,
    message: String,
    help: Option<String>,
    offset: usize,
    length: usize,
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

    // ── L-U-UB0: F3 precondition — fixtures must pass check_str first ─────────

    /// L-U-UB0: All unreachable-branch fixture sources must pass mds::check_str.
    ///
    /// If a future validator change rejects constant conditions, this test fails
    /// loudly — signalling that the unreachable-branch rule would be dead on those inputs.
    #[test]
    fn l_u_ub0_check_gate_passes_for_all_fixtures() {
        let fixtures = [
            // Always-true: literal == same-literal
            "@if \"x\" == \"x\":\nhello\n@end\n",
            // Always-false: different literals
            "@if \"x\" == \"y\":\nhello\n@end\n",
            // Literal != literal (NotEq, always-true)
            "@if \"a\" != \"b\":\nhello\n@end\n",
            // Duplicate @elseif condition — uses variable x, must define it in frontmatter
            "---\nx: hello\n---\n@if x == \"a\":\nfoo\n@elseif x == \"a\":\nbar\n@end\n",
            // Number literals
            "@if 1 == 1:\nhello\n@end\n",
            // Bool literals
            "@if true == true:\nhello\n@end\n",
        ];
        for src in &fixtures {
            let result = crate::check_str(src);
            assert!(
                result.is_ok(),
                "F3 precondition: fixture must pass check_str before unreachable-branch tests\n\
                 fixture: {src:?}\n\
                 error: {:?}",
                result.unwrap_err()
            );
        }
    }

    /// L-U-UB1: Always-true condition with later branches fires.
    ///
    /// M2: always-true @if with @elseif/@else → later branches unreachable → fires.
    #[test]
    fn always_true_literal_eq_fires_when_later_branches_present() {
        // @else branch makes the later-branch unreachable.
        let src = "@if \"x\" == \"x\":\nhello\n@else:\nworld\n@end\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for always-true literal condition with @else; got: {:?}",
            diags
        );
    }

    /// M2 FP fix: always-true @if with NO later branches must NOT fire.
    ///
    /// Appendix A: "always-true → LATER branches unreachable."
    /// With no @elseif or @else, there is nothing unreachable.
    #[test]
    fn always_true_no_later_branches_does_not_fire() {
        let diags = lint_src("@if \"yes\" == \"yes\":\nbody\n@end\n");
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "M2: must NOT fire when always-true @if has no @elseif/@else (nothing is unreachable); \
             got: {:?}",
            diags
        );
    }

    /// Always-false condition fires (then-body is dead code, regardless of later branches).
    #[test]
    fn always_false_literal_eq_fires() {
        let diags = lint_src("@if \"x\" == \"y\":\nhello\n@end\n");
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for always-false literal condition; got: {:?}",
            diags
        );
    }

    /// NotEq always-true: "a" != "b" is always-true → fires only when later branches exist.
    #[test]
    fn always_true_literal_neq_fires_when_later_branches_present() {
        let src = "@if \"a\" != \"b\":\nhello\n@elseif x == \"c\":\nworld\n@end\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for always-true != condition with @elseif; got: {:?}",
            diags
        );
    }

    /// Variable comparison never fires.
    #[test]
    fn variable_comparison_does_not_fire() {
        let diags = lint_src("@if role == \"admin\":\nhello\n@end\n");
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should NOT fire for variable comparison; got: {:?}",
            diags
        );
    }

    /// Duplicate @elseif condition fires.
    #[test]
    fn duplicate_elseif_condition_fires() {
        let src = "@if x == \"a\":\nfoo\n@elseif x == \"a\":\nbar\n@end\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for duplicate @elseif condition; got: {:?}",
            diags
        );
    }

    /// Non-duplicate @elseif condition does not fire.
    #[test]
    fn distinct_elseif_condition_does_not_fire() {
        let src = "@if x == \"a\":\nfoo\n@elseif x == \"b\":\nbar\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should NOT fire for distinct @elseif conditions; got: {:?}",
            diags
        );
    }

    /// Number literal always-true: 1 == 1 fires when later branches exist.
    #[test]
    fn number_literal_always_true_fires_with_later_branches() {
        let src = "@if 1 == 1:\nhello\n@else:\nworld\n@end\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for 1 == 1 with @else; got: {:?}",
            diags
        );
    }

    /// M4: always-true @if + always-true duplicate @elseif → at most 2 findings (not 3).
    ///
    /// Before M4: 3 findings (Pattern 1 + Pattern 2 duplicate + Pattern 2 always-true).
    /// After M4:  2 findings (Pattern 1 + Pattern 2 duplicate only; always-true skipped as redundant).
    #[test]
    fn triple_report_dedup_yields_at_most_two_findings() {
        // @if "a"=="a" (always-true, has @elseif) + @elseif "a"=="a" (duplicate + always-true).
        let src = "@if \"a\" == \"a\":\nfoo\n@elseif \"a\" == \"a\":\nbar\n@end\n";
        let diags = lint_src(src);
        let count = diags.iter().filter(|d| d.rule == RULE).count();
        assert_eq!(
            count, 2,
            "M4: duplicate always-true @elseif must yield exactly 2 findings \
             (not 3 — always-true and duplicate are the same dead code); got {count}: {:?}",
            diags
        );
    }

    /// Rule=error is the default; rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@if \"x\" == \"x\":\nhello\n@end\n";
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
