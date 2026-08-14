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

    /// AC-224-9: Every registered rule name must map to its documented FixTier,
    /// and the expected-tier table must agree with the registry in BOTH directions.
    ///
    /// AD-224-2 (amended): `ALL_RULE_NAMES` removes string duplication but not
    /// omission risk — an 11th rule module forgotten here compiles fine. The
    /// mechanical closure requires:
    /// 1. A separate `EXPECTED` table with explicit (name, tier) pairs.
    /// 2. `assert_eq!(EXPECTED.len(), ALL_RULE_NAMES.len())` — length pin.
    /// 3. Every registry entry appears in `EXPECTED` (registry → table).
    /// 4. Every `EXPECTED` entry appears in the registry (table → registry).
    ///
    /// A rule present in the registry but absent from `EXPECTED`, or vice versa,
    /// fails this test. The `_ => FixTier::C` catch-all arm is now unreachable
    /// for valid registered rules.
    #[test]
    fn registry_and_tier_table_are_bidirectionally_consistent() {
        use super::super::rules::ALL_RULE_NAMES;
        use super::super::config::KNOWN_LINT_RULES;

        /// Canonical expected-tier table: (rule_name, expected_FixTier).
        /// Must be updated whenever a rule is added, removed, or re-tiered.
        /// Pin: len must equal ALL_RULE_NAMES.len() (10 as of this release).
        const EXPECTED: &[(&str, FixTier)] = &[
            // Tier A — auto-fixable with reverify gate
            ("duplicate-export", FixTier::A),
            ("duplicate-import", FixTier::A),
            ("empty-block", FixTier::A),
            ("legacy-interpolation", FixTier::A),
            ("unreachable-branch", FixTier::A),
            // Tier B — standalone-only fixable
            ("unused-function", FixTier::B),
            ("unused-import", FixTier::B),
            // Tier C — report-only, never fixed
            ("redundant-else", FixTier::C),
            ("shadow-variable", FixTier::C),
            ("unused-variable", FixTier::C),
        ];

        // Length pin: bump this when a new rule ships.
        assert_eq!(
            EXPECTED.len(), 10,
            "EXPECTED table length must equal the number of registered rules (10)"
        );
        assert_eq!(
            ALL_RULE_NAMES.len(), 10,
            "ALL_RULE_NAMES length must equal the number of registered rules (10)"
        );
        assert_eq!(
            KNOWN_LINT_RULES.len(), 10,
            "KNOWN_LINT_RULES length must equal the number of registered rules (10)"
        );

        // Direction 1: every registry entry has a correct entry in EXPECTED.
        for &name in ALL_RULE_NAMES {
            let got = rule_tier(name);
            let entry = EXPECTED.iter().find(|(n, _)| *n == name);
            let expected_tier = entry
                .unwrap_or_else(|| panic!("rule {name:?} is in the registry but missing from EXPECTED"))
                .1
                .clone();
            assert_eq!(
                got, expected_tier,
                "rule_tier({name:?}) should be {expected_tier:?} per EXPECTED"
            );
        }

        // Direction 2: every EXPECTED entry exists in the registry.
        for &(name, ref _tier) in EXPECTED {
            assert!(
                ALL_RULE_NAMES.contains(&name),
                "rule {name:?} is in EXPECTED but missing from ALL_RULE_NAMES (registry)"
            );
        }
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
