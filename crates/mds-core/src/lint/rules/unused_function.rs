//! Rule: `unused-function`
//!
//! **Severity**: Warn (default) | **Tier**: B (recompile-diff-proven)
//!
//! A `@define` function that is neither exported nor called within the module
//! is dead code. The function definition contributes nothing to the compiled
//! output and should be removed or exported.
//!
//! ## Firing condition
//!
//! Fires ONLY when:
//! 1. `ctx.has_explicit_exports` is `true` — when there are NO explicit exports,
//!    every `@define` function is implicitly exported (default-public semantics).
//!    Flagging unexported functions in that case would be a false positive.
//! 2. The function is NOT exported (not in any `@export name` or `@export name from`).
//! 3. The function is NOT called anywhere in the module body (not in `used_calls`).
//!
//! ## Self-recursion
//!
//! A function that calls itself (`{myFn(myFn())}`) adds `myFn` to `used_calls`,
//! so it is treated as "called" and not flagged. This matches Appendix A: self-
//! recursion is treated as used. (Note: true mutual recursion would require a
//! second traverse — not needed in v1, and infinite recursion is a runtime error.)
//!
//! ## Suppression on partials/@extends
//!
//! Suppressed when `ctx.is_partial_or_extends` is true.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{FixLineSpan, LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::{AnalysisContext, ExportKind};

pub(crate) const RULE: &str = "unused-function";

/// Check the module for unexported, uncalled @define functions.
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

    // Only fires when there are explicit exports (default-public modules export everything).
    if !ctx.has_explicit_exports {
        return;
    }

    // Build set of explicitly exported function names.
    let exported_names: std::collections::HashSet<String> = ctx
        .exports
        .iter()
        .filter(|e| matches!(e.kind, ExportKind::Named | ExportKind::ReExport))
        .filter_map(|e| e.name.clone())
        .collect();

    for def in &ctx.defines {
        let is_exported = exported_names.contains(&def.name);
        let is_called = ctx.used_calls.contains(&def.name);

        if !is_exported && !is_called && !builder.push(LintDiagnostic {
            rule: RULE.to_string(),
            severity,
            message: format!(
                "Function '{}' is defined but never exported or called.",
                def.name
            ),
            help: Some(
                "Export the function with @export or call it somewhere, or remove the definition."
                    .to_string(),
            ),
            span: Some(SerializedSpan {
                offset: def.offset,
                length: "@define".len() + 1 + def.name.len(),
                line: None,
                column: None,
            }),
            file: Some(filename.to_string()),
            fix_removals: Some(vec![FixLineSpan::single(def.offset)]),
        }) {
            return;
        }
    }
}

fn resolve_severity(config: &LintConfig) -> Severity {
    config.severity_for(RULE).copied().unwrap_or(Severity::Warn)
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

    /// L-U-UF1: Unexported, uncalled function fires (when has_explicit_exports).
    #[test]
    fn unexported_uncalled_fires_when_module_has_exports() {
        // has_explicit_exports=true (there's an @export for `greet`), but `format_name` is unused.
        let src =
            "@define greet():\nhello\n@end\n@define format_name(x):\n{x}!\n@end\n@export greet\n";
        let diags = lint_src(src);
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("format_name")),
            "should fire for unexported/uncalled format_name; got: {:?}",
            diags
        );
    }

    /// No exports → default-public → rule does NOT fire.
    #[test]
    fn no_exports_does_not_fire() {
        let src = "@define greet():\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "no exports → default-public, should not fire; got: {:?}",
            diags
        );
    }

    /// Exported function does not fire.
    #[test]
    fn exported_function_does_not_fire() {
        let src = "@define greet():\nhello\n@end\n@export greet\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "exported function should not fire; got: {:?}",
            diags
        );
    }

    /// Called function does not fire.
    #[test]
    fn called_function_does_not_fire() {
        let src =
            "@define greet():\nhello\n@end\n@define helper():\n@end\n@export greet\n{greet()}\n";
        // `greet` is exported AND called; `helper` is not exported but also not called.
        let diags = lint_src(src);
        // `helper` is the only one that should fire (it's unused).
        // `greet` should not fire.
        assert!(
            !diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("greet")),
            "called+exported function greet should not fire; got: {:?}",
            diags
        );
    }

    /// Self-recursive function is treated as "called" — does not fire.
    #[test]
    fn self_recursive_function_not_flagged() {
        // `count` calls itself → in used_calls → treated as called.
        let src =
            "@define count(n):\n{count(n)}\n@end\n@export greet\n@define greet():\nhello\n@end\n";
        let diags = lint_src(src);
        assert!(
            !diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("count")),
            "self-recursive function should not be flagged; got: {:?}",
            diags
        );
    }

    /// Partial file suppresses unused-function.
    #[test]
    fn partial_suppresses_unused_function() {
        let src = "@define greet():\nhello\n@end\n@export other\n@define other():\nhello\n@end\n";
        let tokens = tokenize(src, "_partial.mds").unwrap();
        let module = parse_with_ctx(&tokens, "_partial.mds", src).unwrap();
        let ctx = collect_facts(&module, true, src).unwrap();
        let mut builder = LintResultBuilder::new();
        check(
            &module,
            &ctx,
            "_partial.mds",
            &LintConfig::default(),
            &mut builder,
        );
        assert!(
            builder.build(false).diagnostics.is_empty(),
            "partial should suppress unused-function"
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@define greet():\nhello\n@end\n@define dead():\n@end\n@export greet\n";
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
