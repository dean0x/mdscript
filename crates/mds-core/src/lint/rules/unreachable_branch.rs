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
use crate::lint::diagnostic::{FixLineSpan, LintDiagnostic, LintResultBuilder, Severity};
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
            if has_later_branches {
                // Case A: remove the @if directive line + the unreachable later branches
                // through @end. Result: then-body unwrapped in parent scope.
                let first_later_offset = b
                    .elseif_branches
                    .first()
                    .map(|br| br.offset)
                    .or(b.else_offset)
                    .unwrap_or(b.end_offset);
                let fix = Some(vec![
                    FixLineSpan::single(b.offset),
                    FixLineSpan {
                        from: first_later_offset,
                        to: b.end_offset,
                        to_inclusive: true,
                    },
                ]);
                if !builder.push(make_diag(
                    *severity,
                    filename,
                    "@if condition is always true — @elseif/@else branches are unreachable."
                        .to_string(),
                    Some(
                        "Replace the constant condition with a variable or remove later branches."
                            .to_string(),
                    ),
                    b.offset,
                    "@if".len(),
                    fix,
                )) {
                    return;
                }
            }
        }
        ConditionClass::AlwaysFalse => {
            // Always-false primary condition → then-body is dead code, regardless of later branches.
            let has_elseif = !b.elseif_branches.is_empty();
            let has_else = b.else_body.is_some();
            let fix = if !has_elseif && !has_else {
                // Case B: no other branches → remove the whole block.
                Some(vec![FixLineSpan {
                    from: b.offset,
                    to: b.end_offset,
                    to_inclusive: true,
                }])
            } else if !has_elseif && has_else {
                // Case C: only @else → remove @if..@else: (inclusive) + the @end line.
                // Result: else-body unwrapped in parent scope.
                let else_off = b.else_offset.unwrap_or(b.end_offset);
                Some(vec![
                    FixLineSpan {
                        from: b.offset,
                        to: else_off,
                        to_inclusive: true,
                    },
                    FixLineSpan::single(b.end_offset),
                ])
            } else {
                // Case D: @elseif branches present — too complex to auto-fix safely.
                None
            };
            if !builder.push(make_diag(
                *severity,
                filename,
                "@if condition is always false — the then-body is dead code.".to_string(),
                Some(
                    "Replace the constant condition with a variable or remove the dead branch."
                        .to_string(),
                ),
                b.offset,
                "@if".len(),
                fix,
            )) {
                return;
            }
        }
        ConditionClass::Unknown => {}
    }

    // Pattern 2: Duplicate @elseif conditions.
    // Collect all seen conditions in order; flag a branch if its condition equals any prior one.
    let mut seen_conditions: Vec<&Condition> = vec![&b.condition];

    for (i, branch) in b.elseif_branches.iter().enumerate() {
        // The removal boundary for this branch is the start of the NEXT branch.
        // If this is the last @elseif, the boundary is @else: (if present) or @end.
        let next_boundary = b
            .elseif_branches
            .get(i + 1)
            .map(|next| next.offset)
            .or(b.else_offset)
            .unwrap_or(b.end_offset);

        let cond = &branch.condition;
        // Check if this @elseif condition duplicates any prior condition.
        let is_duplicate = seen_conditions
            .iter()
            .any(|prior| conditions_eq(prior, cond));

        if is_duplicate {
            // Case G: remove this duplicate @elseif branch up to the next boundary
            // (exclusive) — keeps whatever follows intact.
            let fix = Some(vec![FixLineSpan {
                from: branch.offset,
                to: next_boundary,
                to_inclusive: false,
            }]);
            // Emit ONE finding for the duplicate. Skip the always-true/false check below —
            // the duplicate detection already identifies this dead code (M4 dedup).
            if !builder.push(make_diag(
                *severity,
                filename,
                "@elseif condition is structurally identical to an earlier branch — \
                 this branch can never be reached."
                    .to_string(),
                Some("Remove the duplicate @elseif branch or change its condition.".to_string()),
                branch.offset,
                "@elseif".len(),
                fix,
            )) {
                return;
            }
        } else {
            // Not a duplicate — check if this @elseif is always-true or always-false.
            match classify_condition(cond) {
                ConditionClass::AlwaysTrue => {
                    // Case F: always-true @elseif — later branches are unreachable.
                    // Unwrapping the body safely requires complex restructuring; leave as None.
                    if !builder.push(make_diag(
                        *severity,
                        filename,
                        "@elseif condition is always true.".to_string(),
                        Some("Replace the constant condition with a variable.".to_string()),
                        branch.offset,
                        "@elseif".len(),
                        None, // Case F: restructuring needed — not auto-fixable
                    )) {
                        return;
                    }
                }
                ConditionClass::AlwaysFalse => {
                    // Case E: remove this dead @elseif up to the next boundary (exclusive).
                    let fix = Some(vec![FixLineSpan {
                        from: branch.offset,
                        to: next_boundary,
                        to_inclusive: false,
                    }]);
                    if !builder.push(make_diag(
                        *severity,
                        filename,
                        "@elseif condition is always false — this branch is dead code.".to_string(),
                        Some(
                            "Replace the constant condition with a variable or remove the dead branch."
                                .to_string(),
                        ),
                        branch.offset,
                        "@elseif".len(),
                        fix,
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

    /// Duplicate @elseif diagnostic is anchored at the @elseif line, not the @if line.
    ///
    /// Source layout (all ASCII):
    ///   "@if x == \"a\":\nfoo\n@elseif x == \"a\":\nbar\n@end\n"
    ///    ^0              ^14    ^18
    ///
    /// The duplicate-@elseif diagnostic must have span.offset == 18 (start of @elseif),
    /// not 0 (start of @if).
    #[test]
    fn duplicate_elseif_diagnostic_anchored_at_elseif() {
        // Need x in scope for check_str to pass.
        let src = "---\nx: hello\n---\n@if x == \"a\":\nfoo\n@elseif x == \"a\":\nbar\n@end\n";
        // "---\nx: hello\n---\n" = 17 bytes; @if starts at 17.
        // "@if x == \"a\":\n" = 14 bytes, so @elseif starts at 17+14+3 (foo\n) = 34
        // Let's compute: "---\nx: hello\n---\n" has chars:
        //   '-','-','-','\n','x',':',' ','h','e','l','l','o','\n','-','-','-','\n' = 17 bytes
        // "@if x == \"a\":\n" = '@','i','f',' ','x',' ','=','=',' ','"','a','"',':','\n' = 14 bytes → starts at 17, ends at 30
        // "foo\n" = 4 bytes → starts at 31, ends at 34
        // "@elseif x == \"a\":\n" starts at 35
        let diags = lint_src(src);
        let dup_diag = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("structurally identical"))
            .expect("expected a duplicate-@elseif diagnostic");
        let span = dup_diag
            .span
            .as_ref()
            .expect("diagnostic must carry a span");
        // Verify the span is NOT at the @if opener (byte 17).
        assert_ne!(
            span.offset, 17,
            "duplicate @elseif diagnostic must NOT be anchored at @if opener (byte 17)"
        );
        // Verify the span IS at or after the @elseif directive.
        // The @elseif "x == \"a\":" starts at offset 35 in this source.
        assert_eq!(
            span.offset, 35,
            "duplicate @elseif diagnostic must be at the @elseif directive (byte 35); \
             got offset {}",
            span.offset
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

    // ── A3 case matrix: fix_removals descriptor for each case A–G ─────────────
    //
    // Each test pins the fix_removals shape (Some/None, span count, to_inclusive)
    // for every removal case defined in check_if_block. The letter labels (A–G)
    // follow the case labels in the source comments.

    /// A3-Case A: always-true @if with later branches → fix_removals = Some(two spans).
    ///
    /// Span 1: FixLineSpan::single(b.offset)                          — remove @if directive line.
    /// Span 2: FixLineSpan { from: first_later_offset, …, inclusive } — remove later branches + @end.
    #[test]
    fn a3_case_a_always_true_with_later_branches_fix_removals_is_two_span() {
        // always-true @if "x"=="x" with @else branch.
        let diags = lint_src("@if \"x\" == \"x\":\nhello\n@else:\nworld\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("always true"))
            .expect("expected always-true @if diagnostic (case A)");
        let spans = d.fix_removals.as_ref().expect(
            "case A: always-true @if with later branches must have fix_removals = Some(...)",
        );
        assert_eq!(
            spans.len(),
            2,
            "case A: expected exactly two removal spans (directive line + later-branches+@end); \
             got: {spans:?}"
        );
        // Span 1 is a single-line removal (FixLineSpan::single → to_inclusive = true, from == to).
        assert!(
            spans[0].to_inclusive && spans[0].from == spans[0].to,
            "case A: span 1 must be a single-line inclusive removal \
             (from == to, to_inclusive = true); got: {:?}",
            spans[0]
        );
        // Span 2 covers through @end (to_inclusive = true).
        assert!(
            spans[1].to_inclusive,
            "case A: span 2 (later branches + @end) must be inclusive; got: {:?}",
            spans[1]
        );
    }

    /// A3-Case B: always-false @if, no other branches → fix_removals = Some([whole-block, inclusive]).
    #[test]
    fn a3_case_b_always_false_no_branches_fix_removals_is_whole_block_inclusive() {
        let diags = lint_src("@if \"x\" == \"y\":\nhello\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("always false"))
            .expect("expected always-false @if diagnostic (case B)");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case B: always-false @if (no branches) must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case B: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            spans[0].to_inclusive,
            "case B: whole-block removal must be inclusive; got: {:?}",
            spans[0]
        );
    }

    /// A3-Case C: always-false @if with only @else → fix_removals = Some(two spans).
    ///
    /// Span 1: FixLineSpan { from: @if, to: @else, inclusive } — remove @if..@else: inclusive.
    /// Span 2: FixLineSpan::single(@end)                       — remove @end line.
    /// Result: else-body is unwrapped in the parent scope.
    #[test]
    fn a3_case_c_always_false_with_only_else_fix_removals_is_two_span() {
        let diags = lint_src("@if \"x\" == \"y\":\nhello\n@else:\nworld\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("always false"))
            .expect("expected always-false @if with @else diagnostic (case C)");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case C: always-false @if with @else must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            2,
            "case C: expected exactly two removal spans (@if+@else directive, @end line); \
             got: {spans:?}"
        );
        // Span 1 includes the @else: line (inclusive).
        assert!(
            spans[0].to_inclusive,
            "case C: span 1 (@if..@else: inclusive) must be inclusive; got: {:?}",
            spans[0]
        );
        // Span 2 is a single-line removal of @end (from == to, to_inclusive = true).
        assert!(
            spans[1].to_inclusive && spans[1].from == spans[1].to,
            "case C: span 2 must be a single-line inclusive removal of @end \
             (from == to, to_inclusive = true); got: {:?}",
            spans[1]
        );
    }

    /// A3-Case D: always-false @if with @elseif present → fix_removals = None.
    ///
    /// Auto-fix is too complex when @elseif branches exist (the planner would
    /// need to decide which branch to hoist into the parent scope).
    #[test]
    fn a3_case_d_always_false_with_elseif_fix_removals_is_none() {
        // @elseif uses a variable condition (not always-true/false) to avoid
        // triggering additional unreachable-branch findings that would complicate lookup.
        let diags = lint_src("@if \"x\" == \"y\":\nhello\n@elseif z == \"a\":\nworld\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("always false"))
            .expect("expected always-false @if with @elseif diagnostic (case D)");
        assert!(
            d.fix_removals.is_none(),
            "case D: always-false @if with @elseif must have fix_removals = None \
             (too complex to auto-fix safely); got: {:?}",
            d.fix_removals
        );
    }

    /// A3-Case E: always-false @elseif → fix_removals = Some([exclusive span]).
    ///
    /// Removes from the @elseif directive up to (not including) the next boundary
    /// (@else, next @elseif, or @end) — the boundary line is kept intact.
    #[test]
    fn a3_case_e_always_false_elseif_fix_removals_is_exclusive() {
        // @if condition uses a variable (Unknown) so only the @elseif fires.
        let diags = lint_src("@if z == \"a\":\nhello\n@elseif \"x\" == \"y\":\nworld\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("always false"))
            .expect("expected always-false @elseif diagnostic (case E)");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case E: always-false @elseif must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case E: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            !spans[0].to_inclusive,
            "case E: @elseif removal must be exclusive (to_inclusive = false); got: {:?}",
            spans[0]
        );
    }

    /// A3-Case F: always-true @elseif → fix_removals = None.
    ///
    /// Unwrapping the body would require complex restructuring; the planner
    /// refuses to auto-fix (report-only for this case).
    #[test]
    fn a3_case_f_always_true_elseif_fix_removals_is_none() {
        // @if condition uses a variable (Unknown); @elseif "x"=="x": is always-true.
        let diags = lint_src("@if z == \"a\":\nhello\n@elseif \"x\" == \"x\":\nworld\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("always true"))
            .expect("expected always-true @elseif diagnostic (case F)");
        assert!(
            d.fix_removals.is_none(),
            "case F: always-true @elseif must have fix_removals = None \
             (restructuring required, not auto-fixable); got: {:?}",
            d.fix_removals
        );
    }

    /// A3-Case G: duplicate @elseif condition → fix_removals = Some([exclusive span]).
    ///
    /// Removes from the duplicate @elseif directive up to (not including) the next
    /// boundary, keeping whatever follows intact.
    #[test]
    fn a3_case_g_duplicate_elseif_fix_removals_is_exclusive() {
        // @elseif x == "a" duplicates the @if condition → dead code.
        let diags = lint_src("@if x == \"a\":\nfoo\n@elseif x == \"a\":\nbar\n@end\n");
        let d = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("structurally identical"))
            .expect("expected duplicate @elseif diagnostic (case G)");
        let spans = d
            .fix_removals
            .as_ref()
            .expect("case G: duplicate @elseif must have fix_removals = Some(...)");
        assert_eq!(
            spans.len(),
            1,
            "case G: expected exactly one removal span; got: {spans:?}"
        );
        assert!(
            !spans[0].to_inclusive,
            "case G: duplicate @elseif removal must be exclusive (to_inclusive = false); \
             got: {:?}",
            spans[0]
        );
    }
}
