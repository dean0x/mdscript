//! Rule: `duplicate-export`
//!
//! **Severity**: Error (default) | **Tier**: A (auto-fixable)
//!
//! Flags duplicate export declarations within the same module:
//!
//! - `Named`: two `@export name` directives with the same name.
//! - `ReExport`: two `@export name from "path"` with the same exported name.
//! - `Wildcard`: two `@export * from "path"` with identical paths.
//!
//! **Note**: the resolver already deduplicates duplicate Named exports via `IndexSet`
//! (making them silent no-ops at runtime). This lint rule surfaces the issue before
//! the resolver silently discards the second declaration.
//!
//! **Wildcard vs Named cross-file overlap** is the resolver's job (NameCollision
//! error) — the lint rule stays single-file, matching the module-local AST.
//!
//! ## D2 spans
//!
//! Export spans use the D2 `offset: usize` field added to all `ExportDirective`
//! variants in step 1 (commit 76616ea). The `l_u_de1_export_offsets_are_real`
//! test asserts these are real byte positions in the source.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::{AnalysisContext, ExportKind};

pub(crate) const RULE: &str = "duplicate-export";

/// Check the module for duplicate export declarations.
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

    // Check Named/ReExport: duplicate name keys.
    let mut seen_names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Check Wildcard: duplicate paths.
    let mut seen_wildcards: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for exp in &ctx.exports {
        match exp.kind {
            ExportKind::Named | ExportKind::ReExport => {
                if let Some(name) = &exp.name {
                    match seen_names.get(name).copied() {
                        None => {
                            seen_names.insert(name.clone(), exp.offset);
                        }
                        Some(_first_offset) => {
                            if !builder.push(LintDiagnostic {
                                rule: RULE.to_string(),
                                severity: severity.clone(),
                                message: format!(
                                    "Duplicate export: '{}' is exported more than once.",
                                    name
                                ),
                                help: Some("Remove the duplicate @export directive.".to_string()),
                                span: Some(SerializedSpan {
                                    offset: exp.offset,
                                    length: "@export".len(),
                                    line: None,
                                    column: None,
                                }),
                                file: Some(filename.to_string()),
                            }) {
                                return;
                            }
                        }
                    }
                }
            }
            ExportKind::Wildcard => {
                if let Some(path) = &exp.path {
                    match seen_wildcards.get(path).copied() {
                        None => {
                            seen_wildcards.insert(path.clone(), exp.offset);
                        }
                        Some(_first_offset) => {
                            if !builder.push(LintDiagnostic {
                                rule: RULE.to_string(),
                                severity: severity.clone(),
                                message: format!(
                                    "Duplicate wildcard export from '{}': already exported above.",
                                    path
                                ),
                                help: Some(
                                    "Remove the duplicate @export * from directive.".to_string(),
                                ),
                                span: Some(SerializedSpan {
                                    offset: exp.offset,
                                    length: "@export".len(),
                                    line: None,
                                    column: None,
                                }),
                                file: Some(filename.to_string()),
                            }) {
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn resolve_severity(config: &LintConfig) -> Severity {
    config
        .severity_for(RULE)
        .cloned()
        .unwrap_or(Severity::Error)
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

    /// L-U-DE1 (D2 canary): Export directive offsets must be real byte positions.
    ///
    /// This test asserts that the `offset` field on each export in `ctx.exports`
    /// corresponds to the actual byte position of `@export` in the source.
    #[test]
    fn l_u_de1_export_offsets_are_real() {
        let src = "@define greet():\nhello\n@end\n@export greet\n";
        let tokens = tokenize(src, "test.mds").unwrap();
        let module = parse_with_ctx(&tokens, "test.mds", src).unwrap();
        let ctx = collect_facts(&module, false, src).unwrap();

        assert_eq!(ctx.exports.len(), 1, "should have exactly one export");
        let exp = &ctx.exports[0];

        // The `@export greet` starts after the @define block.
        // Source layout: "@define greet():\nhello\n@end\n" = 29 bytes, then "@export greet\n"
        let expected_offset = src.find("@export").expect("@export must appear in source");
        assert_eq!(
            exp.offset, expected_offset,
            "D2 canary: export offset ({}) must match actual @export position ({})",
            exp.offset, expected_offset
        );

        // Verify the source bytes at the offset are indeed "@export".
        let at_offset = &src[exp.offset..exp.offset + "@export".len()];
        assert_eq!(at_offset, "@export", "bytes at offset must be '@export'");
    }

    /// L-U-DE1: Duplicate Named export fires.
    #[test]
    fn duplicate_named_export_fires() {
        let src = "@define greet():\nhello\n@end\n@export greet\n@export greet\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for duplicate @export greet; got: {:?}",
            diags
        );
    }

    /// Non-duplicate exports do not fire.
    #[test]
    fn distinct_exports_do_not_fire() {
        let src =
            "@define greet():\nhello\n@end\n@define bye():\ngoodbye\n@end\n@export greet\n@export bye\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for distinct exports; got: {:?}",
            diags
        );
    }

    /// Duplicate wildcard exports fires.
    #[test]
    fn duplicate_wildcard_export_fires() {
        // Note: @export * from duplicates currently parse OK (no validator check).
        // This test may need to be checked against check_str first if validator is changed.
        // For now, assert that the lint rule detects it at the AST level.
        let src = "@export * from \"./lib.mds\"\n@export * from \"./lib.mds\"\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for duplicate @export * from; got: {:?}",
            diags
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@define greet():\nhello\n@end\n@export greet\n@export greet\n";
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
