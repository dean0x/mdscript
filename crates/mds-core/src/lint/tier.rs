//! Fix tier classification for lint rules.
//!
//! A leaf module imported by both `diagnostic.rs` (for the JSON `fixable` field)
//! and `fix.rs` (for fix planning). Having it here breaks what would otherwise
//! be a circular dependency: `fix.rs` imports `LintResult` from `diagnostic.rs`,
//! so `diagnostic.rs` cannot import from `fix.rs`.
//!
//! | Tier | Rules                                             | Semantics |
//! |------|---------------------------------------------------|-----------|
//! | A    | duplicate-import, duplicate-export,               | Auto-fixable; gated by reverify |
//! |      | unreachable-branch, empty-block                   |           |
//! | B    | unused-import, unused-function                    | Fixable only when structural-standalone |
//! | C    | unused-variable, redundant-else, shadow-variable  | Never fixed |
//!
//! ## Terminology (spec §7.5)
//!
//! - **Structural-standalone**: a file with no `@import`, `@extends`, or use as a
//!   partial target. This property gates Tier B `--fix`. A file that triggers
//!   `unused-import` is, by definition, not structural-standalone.
//! - **Compile-clean**: a file that compiles without any runtime `--vars`. This
//!   property gates the output-equality reverify for Tier B fixes: removing an
//!   unused import or function must produce byte-identical compiled output.

/// Fix tier for a lint rule.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixTier {
    /// Auto-fixable with a reverify gate. Diagnostic `fixable` = `true`.
    A,
    /// Fixable only when recompile proves output-neutral. `fixable` = `true`
    /// for standalone files; `fixable` = `false` for non-standalone.
    B,
    /// Never fixed. Diagnostic `fixable` = `false`.
    C,
}

/// Classify a rule into its fix tier.
pub fn rule_tier(rule: &str) -> FixTier {
    match rule {
        "duplicate-import"
        | "duplicate-export"
        | "unreachable-branch"
        | "empty-block"
        | "legacy-interpolation" => FixTier::A,
        "unused-import" | "unused-function" => FixTier::B,
        _ => FixTier::C, // unused-variable, redundant-else, shadow-variable, unknown
    }
}

/// Return `true` when applying a fix for this rule is guaranteed to produce
/// byte-identical compiled output to the original.
///
/// All current Tier A / Tier B rules are output-neutral EXCEPT
/// `legacy-interpolation`, whose fix intentionally changes compiled output:
/// it migrates `{x}` (plain text in the current engine) to `{{x}}`
/// (interpolation), so the emitted text changes from a literal brace-wrapped
/// expression to the interpolated value.  The reverify gate's output-equality
/// check must be skipped for this rule.
pub fn is_output_neutral(rule: &str) -> bool {
    rule != "legacy-interpolation"
}

/// Return `true` when this diagnostic is auto-fixable (Tier A or B standalone).
///
/// The `is_standalone` flag controls whether Tier B diagnostics are fixable.
pub fn is_fixable(rule: &str, is_standalone: bool) -> bool {
    match rule_tier(rule) {
        FixTier::A => true,
        FixTier::B => is_standalone,
        FixTier::C => false,
    }
}

/// Record `key` → `offset` as the first occurrence and return `true`; if `key`
/// was already present, leave the map unchanged and return `false`.
///
/// Shared by `duplicate-import` and `duplicate-export` for the
/// "push-first-occurrence, flag-duplicate" pattern. Extracted here because
/// `tier.rs` is the one lint leaf module with no rule-specific imports —
/// placing it here avoids duplicating an identical private fn in each rule file.
pub(crate) fn first_occurrence<K: std::hash::Hash + Eq>(
    seen: &mut std::collections::HashMap<K, usize>,
    key: K,
    offset: usize,
) -> bool {
    if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(key) {
        e.insert(offset);
        true
    } else {
        false
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// ARC-3: Every registered rule name must map to its documented FixTier.
    ///
    /// Enumerating all 10 rules explicitly means that a newly-added rule which
    /// falls silently into the `_ => FixTier::C` catch-all arm will cause this
    /// test to fail (once its expected tier is added here), preventing silent
    /// misclassification in the JSON `fixable` field and the fix planner.
    ///
    /// Tier A: duplicate-import, duplicate-export, unreachable-branch, empty-block,
    ///         legacy-interpolation
    /// Tier B: unused-import, unused-function
    /// Tier C: unused-variable, redundant-else, shadow-variable
    #[test]
    fn all_ten_rules_map_to_expected_tier() {
        // Tier A — auto-fixable with reverify gate
        assert_eq!(
            rule_tier("duplicate-import"),
            FixTier::A,
            "duplicate-import"
        );
        assert_eq!(
            rule_tier("duplicate-export"),
            FixTier::A,
            "duplicate-export"
        );
        assert_eq!(
            rule_tier("unreachable-branch"),
            FixTier::A,
            "unreachable-branch"
        );
        assert_eq!(rule_tier("empty-block"), FixTier::A, "empty-block");
        assert_eq!(
            rule_tier("legacy-interpolation"),
            FixTier::A,
            "legacy-interpolation"
        );
        // Tier B — standalone-only fixable
        assert_eq!(rule_tier("unused-import"), FixTier::B, "unused-import");
        assert_eq!(rule_tier("unused-function"), FixTier::B, "unused-function");
        // Tier C — report-only, never fixed
        assert_eq!(rule_tier("unused-variable"), FixTier::C, "unused-variable");
        assert_eq!(rule_tier("redundant-else"), FixTier::C, "redundant-else");
        assert_eq!(rule_tier("shadow-variable"), FixTier::C, "shadow-variable");
    }

    /// is_output_neutral: legacy-interpolation is NOT output-neutral; all other
    /// rules are.
    ///
    /// This property gates the reverify output-equality check in `plan_and_apply_fixes`
    /// — the check is skipped when the fix batch contains any non-output-neutral rule.
    #[test]
    fn output_neutral_classification() {
        // The sole output-changing rule:
        assert!(
            !is_output_neutral("legacy-interpolation"),
            "legacy-interpolation must NOT be output-neutral"
        );
        // All other known rules are output-neutral:
        for rule in &[
            "duplicate-import",
            "duplicate-export",
            "unreachable-branch",
            "empty-block",
            "unused-import",
            "unused-function",
            "unused-variable",
            "redundant-else",
            "shadow-variable",
        ] {
            assert!(is_output_neutral(rule), "{rule} should be output-neutral");
        }
        // Unknown rules default to output-neutral (safe fallback).
        assert!(is_output_neutral("unknown-rule"));
    }

    /// first_occurrence: first call inserts and returns true.
    #[test]
    fn first_occurrence_new_key_returns_true() {
        let mut map = std::collections::HashMap::new();
        assert!(first_occurrence(&mut map, "a".to_string(), 0));
        assert_eq!(map.get("a"), Some(&0));
    }

    /// first_occurrence: second call on same key returns false without clobbering.
    #[test]
    fn first_occurrence_duplicate_key_returns_false() {
        let mut map = std::collections::HashMap::new();
        first_occurrence(&mut map, "a".to_string(), 0);
        assert!(!first_occurrence(&mut map, "a".to_string(), 99));
        // Original offset is preserved.
        assert_eq!(map.get("a"), Some(&0));
    }
}
