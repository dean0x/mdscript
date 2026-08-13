//! Rule: `unused-import`
//!
//! **Severity**: Warn (default) | **Tier**: B (report-only in practice — no
//! `fix_removals` wired; a file with imports is never structural-standalone)
//!
//! An import that is never used in the module body wastes the resolver's work
//! (and in partial-eval contexts, the loading of an external file).
//!
//! ## Per-form semantics (Appendix A)
//!
//! ### Alias import (`@import "path" as alias`)
//!
//! Used when `alias` appears as:
//! - `Expr::QualifiedCall { namespace: alias, .. }` — `{alias.func(...)}`
//! - `IncludeDirective { alias }` — `@include alias`
//!
//! ### Selective import (`@import { name1, name2 } from "path"`)
//!
//! Each name is checked individually (better UX). A name is used when it appears as:
//! - `Expr::Call { name, .. }` — `{name(...)}`
//! - `Arg::Call { name, .. }` — `func(name(...))`
//! - `Arg::Var(name)` — `func(name)` (passing a function ref as argument)
//! - `Expr::Var(name)` — `{name}` (using the imported name as a variable)
//!
//! ### Merge import (`@import "path"`)
//!
//! **Always treated as used** (conservative). A merge import injects all the
//! imported module's exports plus the `prompt` variable into scope — tracking
//! which injected symbols are actually used would require cross-file analysis,
//! which is out of scope for v1.
//!
//! ### Re-export exemption
//!
//! A selective import name that appears in a `@export name` or
//! `@export name from "path"` directive is considered "used" even without a
//! call-site reference (the module re-exports it).
//!
//! ### Frontmatter `imports:` key
//!
//! The frontmatter `imports:` YAML key is not tracked as AST import nodes —
//! it is handled by the resolver outside the AST. This lint rule does not flag
//! frontmatter imports in v1 (document in rule doc comment).
//!
//! ## Suppression on partials/@extends
//!
//! Suppressed when `ctx.is_partial_or_extends` is true.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::{AnalysisContext, ExportKind, ImportKind};

pub(crate) const RULE: &str = "unused-import";

/// Check the module for unused imports.
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

    // Build re-export set: names that are re-exported (exemption for selective imports).
    // Wildcard re-exports (@export * from ...) are intentionally NOT in this exemption set:
    // a wildcard re-export operates on the imported module's own exports, not on a local
    // import binding — a file with @import + @export * re-exports from different paths and
    // there is no syntactic link between them at the name level. Adding wildcard to this
    // set would require cross-file resolution that is deliberately out of scope for v1
    // (adjudicated more-correct than Appendix A's literal wording).
    let reexport_names: std::collections::HashSet<String> = ctx
        .exports
        .iter()
        .filter(|e| matches!(e.kind, ExportKind::Named | ExportKind::ReExport))
        .filter_map(|e| e.name.clone())
        .collect();

    for imp in &ctx.imports {
        match imp.kind {
            ImportKind::Merge => {
                // Always treated as used (conservative — cross-file analysis needed).
                continue;
            }
            ImportKind::Alias => {
                let alias = imp.alias.as_deref().unwrap_or("");
                let is_used =
                    ctx.used_namespaces.contains(alias) || ctx.used_include_aliases.contains(alias);
                if !is_used
                    && !builder.push(make_diag(
                        severity,
                        filename,
                        format!(
                            "Import alias '{}' from '{}' is never used.",
                            alias, imp.path
                        ),
                        Some(
                            "Remove the @import or use the alias with @include or as \
                             a qualified call (`alias.func(...)`)."
                                .to_string(),
                        ),
                        imp.offset,
                        "@import".len(),
                    ))
                {
                    return;
                }
            }
            ImportKind::Selective => {
                // Per-name flagging: each name checked individually.
                // AD-203-1 / PF-012: anchor the span at the name, not @import.
                for (i, name) in imp.names.iter().enumerate() {
                    let is_used = ctx.used_calls.contains(name)
                        || ctx.used_vars.contains(name)
                        || reexport_names.contains(name);
                    if !is_used {
                        // Prefer the per-name offset; fall back to @import offset
                        // if name_offsets is unexpectedly short (defensive).
                        let name_offset = imp.name_offsets.get(i).copied().unwrap_or(imp.offset);
                        if !builder.push(make_diag(
                            severity,
                            filename,
                            format!(
                                "Imported name '{}' from '{}' is never used.",
                                name, imp.path
                            ),
                            Some(format!(
                                "Remove '{}' from the selective import or use it in the body.",
                                name
                            )),
                            name_offset,
                            name.len(),
                        )) {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn resolve_severity(config: &LintConfig) -> Severity {
    config.severity_for(RULE).copied().unwrap_or(Severity::Warn)
}

/// Build an unused-import diagnostic.
///
/// `offset` is the byte position of the span anchor within the source.
/// `length` is the byte length of the highlighted token.
///
/// For Alias and Merge forms the caller passes `imp.offset` /
/// `"@import".len()` so the span covers the `@import` keyword.
///
/// For Selective forms the caller passes the per-name offset from
/// `imp.name_offsets` and `name.len()` so the span covers the unused name
/// (AD-203-1 / PF-012).
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
        fix_removals: None, // unused-import is report-only (partial-name removal unsafe)
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

    /// L-U-UI1: Unused alias import fires.
    #[test]
    fn unused_alias_import_fires() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for unused alias import; got: {:?}",
            diags
        );
    }

    /// Used alias import (via qualified call) does not fire.
    #[test]
    fn used_alias_via_qualified_call_does_not_fire() {
        let src = "@import \"./lib.mds\" as lib\n{{lib.greet(\"world\")}}\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for alias used in QualifiedCall; got: {:?}",
            diags
        );
    }

    /// Used alias import (via @include) does not fire.
    #[test]
    fn used_alias_via_include_does_not_fire() {
        let src = "@import \"./lib.mds\" as lib\n@include lib\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for alias used in @include; got: {:?}",
            diags
        );
    }

    /// L-U-UI2: Unused selective import name fires per-name.
    #[test]
    fn unused_selective_import_fires() {
        let src = "@import { greet } from \"./lib.mds\"\nHello!\n";
        let diags = lint_src(src);
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("greet")),
            "should fire for unused selective import 'greet'; got: {:?}",
            diags
        );
    }

    /// Used selective import name does not fire.
    #[test]
    fn used_selective_import_does_not_fire() {
        let src = "@import { greet } from \"./lib.mds\"\n{{greet(\"world\")}}\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for used selective import; got: {:?}",
            diags
        );
    }

    /// Merge import is always treated as used (conservative).
    #[test]
    fn merge_import_always_used() {
        let src = "@import \"./lib.mds\"\nHello!\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "merge import should always be treated as used; got: {:?}",
            diags
        );
    }

    /// Re-export exemption: selective name that is re-exported is not flagged.
    #[test]
    fn selective_name_reexported_not_flagged() {
        let src = "@import { greet } from \"./lib.mds\"\n@export greet\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "re-exported selective import should not be flagged; got: {:?}",
            diags
        );
    }

    /// Partial file suppresses unused-import.
    #[test]
    fn partial_suppresses_unused_import() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
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
        assert!(
            builder.build(false).diagnostics.is_empty(),
            "partial should suppress unused-import"
        );
    }

    /// TEST-5: Wildcard re-export (`@export * from "path"`) does NOT exempt a
    /// selective import from the unused-import rule.
    ///
    /// The KB-adjudicated semantic: `@export * from "..."` operates on the imported
    /// module's own exports at a different namespace level — there is no syntactic
    /// link between the wildcard re-export and any local import binding. Only
    /// `@export name` (Named) and `@export name from "path"` (ReExport) suppress
    /// their matching local import binding.
    #[test]
    fn wildcard_reexport_does_not_exempt_unused_import() {
        // `greet` is selectively imported but never referenced in the body.
        // The wildcard re-export is from an unrelated path and carries no name
        // binding — it must NOT exempt `greet` from the unused-import finding.
        let src = "@import { greet } from \"./lib.mds\"\n@export * from \"./other.mds\"\n";
        let diags = lint_src(src);
        assert!(
            diags
                .iter()
                .any(|d| d.rule == RULE && d.message.contains("greet")),
            "wildcard re-export must NOT exempt a selective import from unused-import; \
             got: {:?}",
            diags
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
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

    // ── AC-P1-19 / AD-203-1: span anchors at the unused name ─────────────────

    /// AC-P1-19: for a single unused name in a selective import, the span offset
    /// must point at the name's first byte, not at the `@import` keyword.
    ///
    /// Source: `@import { greet } from "./lib.mds"\n`
    ///          0123456789012345...
    ///                    ^ 'greet' starts at byte 10 (after "@import { ")
    #[test]
    fn selective_span_anchors_at_name_not_at_import_keyword() {
        let src = "@import { greet } from \"./lib.mds\"\nHello!\n";
        let diags = lint_src(src);
        let diag = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("greet"))
            .expect("unused-import diagnostic for 'greet' must fire");
        let span = diag.span.as_ref().expect("span must be present");

        // "@import { " = 10 bytes before 'greet'.
        let expected_offset = "@import { ".len();
        assert_eq!(
            span.offset, expected_offset,
            "span.offset must point at the name 'greet' (byte {}), not at @import (byte 0); \
             got span.offset={}",
            expected_offset, span.offset
        );
        assert_eq!(
            span.length,
            "greet".len(),
            "span.length must equal the name length; got span.length={}",
            span.length
        );
    }

    /// AC-P1-19 (second name): in a multi-name selective import, each unused name
    /// has an independently anchored span.
    ///
    /// Source: `@import { foo, bar } from "./lib.mds"\n`
    ///          0123456789012345678...
    ///                    ^ 'foo' at 10, 'bar' at 15
    #[test]
    fn selective_multi_name_each_span_anchored_independently() {
        let src = "@import { foo, bar } from \"./lib.mds\"\nHello!\n";
        let diags = lint_src(src);

        let foo_diag = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("'foo'"))
            .expect("diagnostic for 'foo' must fire");
        let bar_diag = diags
            .iter()
            .find(|d| d.rule == RULE && d.message.contains("'bar'"))
            .expect("diagnostic for 'bar' must fire");

        let foo_span = foo_diag
            .span
            .as_ref()
            .expect("span for 'foo' must be present");
        let bar_span = bar_diag
            .span
            .as_ref()
            .expect("span for 'bar' must be present");

        // "@import { " = 10 bytes.
        assert_eq!(
            foo_span.offset,
            "@import { ".len(),
            "span for 'foo' must start at byte {}; got {}",
            "@import { ".len(),
            foo_span.offset
        );
        assert_eq!(foo_span.length, "foo".len());

        // "@import { foo, " = 15 bytes.
        assert_eq!(
            bar_span.offset,
            "@import { foo, ".len(),
            "span for 'bar' must start at byte {}; got {}",
            "@import { foo, ".len(),
            bar_span.offset
        );
        assert_eq!(bar_span.length, "bar".len());
    }

    /// Alias form still anchors at the `@import` keyword (not changed by #203).
    #[test]
    fn alias_span_anchors_at_import_keyword() {
        let src = "@import \"./lib.mds\" as lib\nHello!\n";
        let diags = lint_src(src);
        let diag = diags
            .iter()
            .find(|d| d.rule == RULE)
            .expect("unused alias import diagnostic must fire");
        let span = diag.span.as_ref().expect("span must be present");
        assert_eq!(
            span.offset, 0,
            "alias span must start at byte 0 (@import); got {}",
            span.offset
        );
        assert_eq!(
            span.length,
            "@import".len(),
            "alias span.length must equal '@import' length; got {}",
            span.length
        );
    }
}
