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
//! | B    | unused-import, unused-function                    | Fixable only when standalone |
//! | C    | unused-variable, redundant-else, shadow-variable  | Never fixed |

/// Fix tier for a lint rule.
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
        "duplicate-import" | "duplicate-export" | "unreachable-branch" | "empty-block" => {
            FixTier::A
        }
        "unused-import" | "unused-function" => FixTier::B,
        _ => FixTier::C, // unused-variable, redundant-else, shadow-variable, unknown
    }
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
