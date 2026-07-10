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
#[derive(Debug, Default, Clone)]
pub struct LintConfig {
    /// Per-rule severity overrides. Key = rule name (e.g. `"unused-variable"`),
    /// value = desired severity. Empty map = all rules at built-in defaults.
    pub rules: HashMap<String, Severity>,
}

impl LintConfig {
    /// Look up the configured severity for a rule name.
    ///
    /// Returns `None` when the rule has no explicit override — callers should fall
    /// back to the rule's built-in default severity.
    pub fn severity_for(&self, rule: &str) -> Option<&Severity> {
        self.rules.get(rule)
    }
}
