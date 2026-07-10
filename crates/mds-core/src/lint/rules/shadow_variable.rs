//! Rule: `shadow-variable`
//!
//! **Severity**: Info (default) | **DEFAULT OFF** (silent unless enabled via config)
//!
//! An inner-scope name that shadows an outer-scope binding. Shadowing is
//! intentionally allowed by the MDS spec (spec §6: "inner scope wins, no warning
//! by default") but may be unintentional in some codebases.
//!
//! Because shadowing is explicitly supported, this rule ships at `Info` severity
//! and is **default-off**: it fires only when enabled via `mds.json`:
//!
//! ```json
//! { "lint": { "rules": { "shadow-variable": "info" } } }
//! ```
//!
//! Or elevated to a warning:
//! ```json
//! { "lint": { "rules": { "shadow-variable": "warn" } } }
//! ```
//!
//! ## Enumerated shadow pairs (Appendix A)
//!
//! 1. `@for var/key_var` over a frontmatter key
//! 2. `@define param` over a frontmatter key
//! 3. nested `@for var` over an outer `@for var`
//! 4. `@define param` over a `@for var` in the enclosing scope
//! 5. `@for var` over an import alias
//!
//! @block cannot nest (it is not a loop/define) — excluded.
//!
//! ## Note on suppression
//!
//! This rule is NOT suppressed on partials/@extends (it's default-off anyway;
//! its default-off nature already prevents spurious CI failures on library files).

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::{AnalysisContext, ShadowKind};

pub(crate) const RULE: &str = "shadow-variable";

/// Check the module for shadow variable pairs.
///
/// **Default-off**: returns immediately unless the config explicitly enables the rule.
pub(crate) fn check(
    _module: &Module,
    ctx: &AnalysisContext,
    filename: &str,
    config: &LintConfig,
    builder: &mut LintResultBuilder,
) {
    // Default severity is Off — rule is silent unless explicitly enabled.
    let severity = resolve_severity(config);
    if severity == Severity::Off {
        return;
    }

    for pair in &ctx.shadow_pairs {
        let inner_desc = kind_desc(&pair.inner_kind);
        let outer_desc = kind_desc(&pair.outer_kind);

        if !builder.push(LintDiagnostic {
            rule: RULE.to_string(),
            severity: severity.clone(),
            message: format!(
                "Variable '{}' ({}) shadows an outer {} with the same name.",
                pair.name, inner_desc, outer_desc
            ),
            help: Some(format!(
                "Rename '{}' in the inner scope to avoid shadowing.",
                pair.name
            )),
            span: Some(SerializedSpan {
                offset: pair.offset,
                length: pair.name.len(),
                line: None,
                column: None,
            }),
            file: Some(filename.to_string()),
        }) {
            return;
        }
    }
}

/// Built-in default severity: Off (rule is default-off).
fn resolve_severity(config: &LintConfig) -> Severity {
    config.severity_for(RULE).cloned().unwrap_or(Severity::Off)
}

fn kind_desc(kind: &ShadowKind) -> &'static str {
    match kind {
        ShadowKind::FmVar => "frontmatter variable",
        ShadowKind::ImportAlias => "import alias",
        ShadowKind::ForVar => "@for loop variable",
        ShadowKind::DefineParam => "@define parameter",
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::lint::facts::collect_facts;
    use crate::parser::parse_with_ctx;

    fn lint_with_config(src: &str, config: &LintConfig) -> Vec<LintDiagnostic> {
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();
        let mut builder = LintResultBuilder::new();
        check(&module, &ctx, "test.mds", config, &mut builder);
        builder.build(false).diagnostics
    }

    fn enabled_config() -> LintConfig {
        LintConfig {
            rules: [(RULE.to_string(), Severity::Info)].into_iter().collect(),
        }
    }

    /// L-U-SV1 (default-off): Shadow rule is silent with default config.
    #[test]
    fn default_off_produces_no_diagnostics() {
        let src = "---\nname: World\n---\n@for name in items:\n{name}\n@end\n";
        let diags = lint_with_config(src, &LintConfig::default());
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "shadow-variable should be default-off; got: {:?}",
            diags
        );
    }

    /// When explicitly enabled, @for var over FM key fires.
    #[test]
    fn enabled_for_var_over_fm_key_fires() {
        let src = "---\nname: World\n---\n@for name in items:\n{name}\n@end\n";
        let diags = lint_with_config(src, &enabled_config());
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("name")),
            "should fire for @for var 'name' shadowing FM key; got: {:?}",
            diags
        );
    }

    /// @define param over FM key fires when enabled.
    #[test]
    fn enabled_define_param_over_fm_key_fires() {
        let src = "---\nuser: Alice\n---\n@define greet(user):\nhello {user}\n@end\n";
        let diags = lint_with_config(src, &enabled_config());
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("user")),
            "should fire for @define param 'user' shadowing FM key; got: {:?}",
            diags
        );
    }

    /// No shadow → no diagnostic even when enabled.
    #[test]
    fn no_shadow_no_diagnostic() {
        let src = "---\nname: World\n---\n@for item in items:\n{item}\n@end\n";
        let diags = lint_with_config(src, &enabled_config());
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "no shadow should produce no diagnostic; got: {:?}",
            diags
        );
    }

    /// Rule=info enables it; rule=off silences even if it was enabled before.
    #[test]
    fn rule_off_suppresses() {
        let src = "---\nname: World\n---\n@for name in items:\n{name}\n@end\n";
        let config = LintConfig {
            rules: [(RULE.to_string(), Severity::Off)].into_iter().collect(),
        };
        let diags = lint_with_config(src, &config);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "rule=off should suppress; got: {:?}",
            diags
        );
    }
}
