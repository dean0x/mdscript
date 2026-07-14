//! Rule: `duplicate-import`
//!
//! **Severity**: Error (default) | **Tier**: A (auto-fixable)
//!
//! Flags two or more `@import` directives that resolve to the same path after
//! lexical normalization (T4: interior segment-collapse — resolving `.` and
//! interior `..` textually without filesystem operations).
//!
//! Same path imported in different FORMS (alias vs merge vs selective) = flag.
//! Same path in the same form with the same alias = flag.
//!
//! ## Symlink/case limitation (documented v1)
//!
//! Symlinks and case-insensitive filesystems may cause `./a.mds` and
//! `./A.mds` to resolve to the same file without this lint detecting it.
//! This is a known v1 limitation — the normalization is purely textual.

use crate::ast::Module;
use crate::error::SerializedSpan;
use crate::lint::config::LintConfig;
use crate::lint::diagnostic::{LintDiagnostic, LintResultBuilder, Severity};
use crate::lint::facts::AnalysisContext;
use crate::lint::tier::first_occurrence;

pub(crate) const RULE: &str = "duplicate-import";

/// Check the module for duplicate import paths.
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

    // Group imports by normalized path and check for duplicates.
    // O(n) pass: build map from normalized path → first-seen offset.
    // O(n) second pass: flag any that have been seen before.
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for imp in &ctx.imports {
        let norm = normalize_import_path(&imp.path);
        if !first_occurrence(&mut seen, norm, imp.offset)
            && !builder.push(LintDiagnostic {
                rule: RULE.to_string(),
                severity,
                message: format!(
                    "Duplicate import: '{}' is imported more than once.",
                    imp.path
                ),
                help: Some(
                    "Remove the duplicate import. If different forms are needed (alias vs \
                     merge), consolidate into one import directive."
                        .to_string(),
                ),
                span: Some(SerializedSpan {
                    offset: imp.offset,
                    length: "@import".len(),
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
    config
        .severity_for(RULE)
        .copied()
        .unwrap_or(Severity::Error)
}

/// Lexical path normalization: collapse `.` segments and interior `..` segments.
///
/// Never performs filesystem operations. Documented v1 limitation: symlinks and
/// case-insensitive filesystems may cause false negatives.
///
/// ## Algorithm (D1)
///
/// Split on `/`, then:
/// - `.` segment → skip (same-directory marker).
/// - `..` segment → pop the last non-`..` segment (if any); otherwise retain `..`.
/// - Any other segment → push.
///
/// Examples:
/// - `"./utils.mds"` → `"utils.mds"` (leading `./` collapsed to nothing)
/// - `"../lib/utils.mds"` → `"../lib/utils.mds"` (leading `..` preserved)
/// - `"a/./b.mds"` → `"a/b.mds"` (interior `.` collapsed)
/// - `"a/../b.mds"` → `"b.mds"` (interior `..` collapsed)
pub(crate) fn normalize_import_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();

    for part in path.split('/') {
        match part {
            "." => {} // skip: current-directory marker
            ".." => {
                // Pop the last non-`..` segment, or preserve `..` if at the root.
                match segments.last() {
                    Some(&"..") | None => {
                        segments.push("..");
                    }
                    Some(_) => {
                        segments.pop();
                    }
                }
            }
            other => {
                segments.push(other);
            }
        }
    }

    segments.join("/")
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

    // ── L-U-H1: normalize_import_path unit tests ──────────────────────────────

    #[test]
    fn normalize_leading_dot_slash() {
        assert_eq!(normalize_import_path("./utils.mds"), "utils.mds");
    }

    #[test]
    fn normalize_no_prefix() {
        assert_eq!(normalize_import_path("utils.mds"), "utils.mds");
    }

    #[test]
    fn normalize_interior_dot() {
        assert_eq!(normalize_import_path("a/./b.mds"), "a/b.mds");
    }

    #[test]
    fn normalize_interior_dotdot() {
        assert_eq!(normalize_import_path("a/../b.mds"), "b.mds");
    }

    #[test]
    fn normalize_leading_dotdot_preserved() {
        assert_eq!(
            normalize_import_path("../lib/utils.mds"),
            "../lib/utils.mds"
        );
    }

    #[test]
    fn normalize_multiple_leading_dotdot() {
        assert_eq!(
            normalize_import_path("../../lib/utils.mds"),
            "../../lib/utils.mds"
        );
    }

    #[test]
    fn normalize_complex_path() {
        // `a/b/../c/./d.mds` → remove `../`, keep `c`, collapse `.` → `a/c/d.mds`
        assert_eq!(normalize_import_path("a/b/../c/./d.mds"), "a/c/d.mds");
    }

    // ── L-U-DI1: duplicate-import rule tests ─────────────────────────────────

    /// L-U-DI1: Same path twice fires.
    #[test]
    fn same_path_twice_fires() {
        let src = "@import \"./utils.mds\" as u1\n@import \"./utils.mds\" as u2\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire for same path twice; got: {:?}",
            diags
        );
    }

    /// Same path with `./` prefix and without = duplicate (after normalization).
    #[test]
    fn dot_slash_vs_bare_is_duplicate() {
        let src = "@import \"./utils.mds\" as u1\n@import \"utils.mds\" as u2\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire: ./utils.mds == utils.mds after normalization; got: {:?}",
            diags
        );
    }

    /// Different paths do not fire.
    #[test]
    fn different_paths_do_not_fire() {
        let src = "@import \"./utils.mds\" as u1\n@import \"./other.mds\" as u2\n";
        let diags = lint_src(src);
        assert!(
            !diags.iter().any(|d| d.rule == RULE),
            "should not fire for different paths; got: {:?}",
            diags
        );
    }

    /// Same path in different forms (alias vs merge) fires.
    #[test]
    fn different_forms_same_path_fires() {
        let src = "@import \"./utils.mds\" as u\n@import \"./utils.mds\"\n";
        let diags = lint_src(src);
        assert!(
            diags.iter().any(|d| d.rule == RULE),
            "should fire: alias+merge of same path; got: {:?}",
            diags
        );
    }

    /// Rule=off suppresses.
    #[test]
    fn rule_off_suppresses() {
        let src = "@import \"./utils.mds\" as u1\n@import \"./utils.mds\" as u2\n";
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
