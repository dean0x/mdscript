//! Rule: `unused-variable`
//!
//! **Severity**: Warn (default) | **Tier**: C (never auto-fixed)
//!
//! A frontmatter variable key that is never referenced in the template body
//! contributes nothing to the compiled output. It may be a leftover from
//! template refactoring or an accidental typo.
//!
//! ## What counts as a "use"
//!
//! A frontmatter key is "used" when it appears as any of:
//! - `Expr::Var(key)` — `{key}` interpolation
//! - `Expr::MemberAccess { object: key, .. }` — `{key.field}`
//! - `Arg::Var(key)` — `func(key)` argument
//! - `Arg::MemberAccess { object: key, .. }` — `func(key.field)` argument
//!
//! This covers ALL recursive bodies including `@if` conditions, `@for` iterables,
//! and nested `@define`/`@message` bodies.
//!
//! ## Reserved skip-set
//!
//! The following keys are NEVER flagged: `imports`, `type`, `extends`, `prompt`.
//! These are either structural keys consumed by the resolver/compiler or
//! automatically injected by merge imports.
//!
//! ## Tier C: report-only
//!
//! Frontmatter keys may affect YAML output (when compiled with `---` pass-through)
//! and removing them could change non-body behaviour. Tier C means the user must
//! remove the variable manually — no auto-fix is generated.
//!
//! ## Suppression on partials/@extends
//!
//! Unused-* rules are suppressed when `ctx.is_partial_or_extends` is true:
//! a partial library file's variables may be consumed by its importers; an
//! `@extends` child template's variables may satisfy its parent's expectations.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;

pub(crate) const RULE: &str = "unused-variable";

/// Check the module for unused frontmatter variables.
pub(crate) fn check(
    _module: &Module,
    ctx: &AnalysisContext,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    let severity = resolve_severity(config);
    if severity == Severity::Off {
        return;
    }

    // Suppressed on partials / @extends children.
    if ctx.is_partial_or_extends {
        return;
    }

    for fv in &ctx.frontmatter_vars {
        // A key is "used" if it appears in any body expression as a variable or member object.
        let is_used = ctx.used_vars.contains(&fv.name);

        if !is_used
            && !builder.push(LintDiagnostic {
                rule: RULE.to_string(),
                severity: severity.clone(),
                message: format!(
                    "Variable '{}' is defined in frontmatter but never referenced in the body.",
                    fv.name
                ),
                help: Some(
                    "Remove the frontmatter key or reference it in the template body.".to_string(),
                ),
                span: fv.approx_offset.map(|off| SerializedSpan {
                    offset: off,
                    length: fv.name.len(),
                    line: None,
                    column: None,
                }),
                file: Some(filename.to_string()),
            })
        {
            return;
        }
    }
}

fn resolve_severity(config: &LintConfig) -> Severity {
    config.severity_for(RULE).cloned().unwrap_or(Severity::Warn)
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

    fn lint_src_partial(src: &str) -> Vec<LintDiagnostic> {
        let tokens = tokenize(src, "_partial.mds").unwrap();
        let module = parse_with_ctx(&tokens, "_partial.mds", src).unwrap();
        let ctx = collect_facts(&module, true, src).unwrap(); // is_partial=true
        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "_partial.mds",
            &LintConfig::default(),
            &mut builder,
        );
        builder.build(false).diagnostics
    }

    /// L-U-UV1: Unused FM key fires.
    #[test]
    fn unused_fm_key_fires() {
        let src = "---\nname: World\n---\nHello!\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for unused FM key 'name'; got: {:?}",
            diags
        );
    }

    /// L-U-UV2: Used FM key does not fire.
    #[test]
    fn used_fm_key_does_not_fire() {
        let src = "---\nname: World\n---\nHello {name}!\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire when 'name' is used; got: {:?}",
            diags
        );
    }

    /// Reserved keys (imports, type, extends, prompt) are never flagged.
    #[test]
    fn reserved_keys_not_flagged() {
        // `type` and `imports` are reserved and excluded from frontmatter_vars.
        let src = "---\ntype: mds\nimports: []\nprompt: hi\n---\nHello!\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "reserved keys must not be flagged; got: {:?}",
            diags
        );
    }

    /// FM var used in @if condition is not flagged.
    #[test]
    fn fm_var_used_in_condition_not_flagged() {
        let src = "---\nrole: admin\n---\n@if role == \"admin\":\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "FM var used in condition should not be flagged; got: {:?}",
            diags
        );
    }

    /// FM var used as @for iterable is not flagged.
    #[test]
    fn fm_var_used_as_for_iterable_not_flagged() {
        let src = "---\nitems: []\n---\n@for item in items:\n{item}\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "FM var used as @for iterable should not be flagged; got: {:?}",
            diags
        );
    }

    /// Partial file suppresses unused-variable.
    #[test]
    fn partial_suppresses_unused_variable() {
        let src = "---\nname: World\n---\nHello!\n";
        let diags = lint_src_partial(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "partial file should suppress unused-variable; got: {:?}",
            diags
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "---\nname: World\n---\nHello!\n";
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
