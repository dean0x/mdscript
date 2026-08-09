//! Lint configuration: `LintConfig` with per-rule severity overrides.
//!
//! `LintConfig` is public (lives in mds-core) — the CLI converts the `mds.json`
//! `lint.rules` section into it, and all `lint_*` entry points accept a `&LintConfig`.
//!
//! **Unknown rule NAMEs** are preserved in the map (warn-and-ignore at the CLI layer).
//! **Unknown severity VALUES** fail loudly via serde deserialization error (closed enum).

use std::collections::HashMap;

use super::diagnostic::Severity;

/// Per-rule severity override configuration.
///
/// Loaded from the `lint.rules` section of `mds.json`:
/// ```json
/// { "lint": { "rules": { "unused-variable": "off", "shadow-variable": "warn" } } }
/// ```
///
/// Absent rules default to the engine's built-in severity (defined per rule in the
/// rule catalog). Unknown rule names in the map are preserved and may emit a
/// warn-at-CLI-layer diagnostic (so forward-compat: a new rule name from a newer
/// version of mds does not break older configs).
///
/// Unknown severity *values* (e.g. `"verbose"`) cause a hard parse error (`exit 2`)
/// because the closed enum has no sensible fallback.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Use `LintConfig::default()` for a config with all rules at engine defaults, or
/// [`LintConfig::from_rules`] to supply per-rule overrides; do not construct via
/// struct literal.
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct LintConfig {
    /// Per-rule severity overrides. Key = rule name (e.g. `"unused-variable"`),
    /// value = desired severity. Empty map = all rules at built-in defaults.
    pub rules: HashMap<String, Severity>,
}

impl LintConfig {
    /// Construct a `LintConfig` with the given per-rule severity overrides.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// Per Rust API guidelines (C-CTOR): constructors are named `new`, `from_*`,
    /// or `with_*` only when taking `self`. This function does not take `self`,
    /// so it is named `from_rules`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use mds::{LintConfig, Severity};
    /// let config = LintConfig::from_rules(HashMap::from([
    ///     ("unused-variable".to_string(), Severity::Off),
    /// ]));
    /// assert_eq!(config.severity_for("unused-variable"), Some(&Severity::Off));
    /// ```
    #[must_use]
    pub fn from_rules(rules: HashMap<String, Severity>) -> Self {
        LintConfig { rules }
    }

    /// Look up the configured severity for a rule name.
    ///
    /// Returns `None` when the rule has no explicit override — callers should fall
    /// back to the rule's built-in default severity.
    pub fn severity_for(&self, rule: &str) -> Option<&Severity> {
        self.rules.get(rule)
    }
}
