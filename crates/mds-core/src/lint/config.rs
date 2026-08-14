//! Lint configuration: `LintConfig` with per-rule severity overrides.
//!
//! `LintConfig` is public (lives in mds-core) — the CLI converts the `mds.json`
//! `lint.rules` section into it, and all `lint_*` entry points accept a `&LintConfig`.
//!
//! **Unknown rule NAMEs** emit a warning and lint continues — the unknown rule simply
//! has no effect. This is deliberate forward-compatibility: severities are a closed set,
//! but rule names grow every release; hard-failing an unknown name would break a config
//! naming a newer rule when run with an older binary.
//! **Unknown severity VALUES** fail loudly via serde deserialization error (closed enum).

use std::collections::HashMap;

use super::diagnostic::Severity;
use super::rules;

/// All known lint rule names, sorted lexicographically.
///
/// AD-224-2: assembled from each rule module's own `RULE` const (the single
/// source of truth for the string). The omission risk — a new module whose `RULE`
/// is never listed — is closed by the bidirectional tier table in `tier.rs`.
///
/// PF-015: this list is accurate as of this release. Future releases may add
/// entries; code that gates on this list should be prepared for it to grow.
pub const KNOWN_LINT_RULES: &[&str] = rules::ALL_RULE_NAMES;

/// The set of rule names in a `rules` map that are not registered with the lint engine.
///
/// AD-224-1: this is NOT an error. Under the 2026-08-12 ruling, an unknown rule name
/// warns and lint continues — the rule simply has no effect. See [`find_unknown_rule_names`].
///
/// The names are sorted lexicographically. This type is `#[non_exhaustive]`:
/// use the [`UnknownRuleNames::names`] accessor, not a struct literal.
///
/// This type is `#[non_exhaustive]` per ADR-010. It is constructible only through
/// the library — external crates MUST NOT build it via a struct literal.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct UnknownRuleNames {
    /// Sorted, non-empty list of unknown rule names.
    names: Vec<String>,
}

impl UnknownRuleNames {
    fn new(mut names: Vec<String>) -> Self {
        names.sort();
        UnknownRuleNames { names }
    }

    /// The unknown rule names, sorted lexicographically.
    ///
    /// Always non-empty — `UnknownRuleNames` is only constructed when at least
    /// one unknown name is present.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// Detect rule names in `rules` that are not registered in [`KNOWN_LINT_RULES`].
///
/// Returns `None` when every name in `rules` is known. Returns `Some(UnknownRuleNames)`
/// when at least one unknown name is found; names inside are sorted lexicographically.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use mds::{Severity, find_unknown_rule_names, KNOWN_LINT_RULES};
///
/// // All-known map → no unknowns.
/// let known = HashMap::from([("unused-variable".to_string(), Severity::Off)]);
/// assert!(find_unknown_rule_names(&known).is_none());
///
/// // Map with an unknown name → Some with that name.
/// let mixed = HashMap::from([
///     ("unused-variable".to_string(), Severity::Off),
///     ("no-such-rule".to_string(), Severity::Warn),
/// ]);
/// let u = find_unknown_rule_names(&mixed).expect("should have unknown");
/// assert_eq!(u.names(), &["no-such-rule".to_string()]);
/// assert_eq!(KNOWN_LINT_RULES.len(), 10);
/// ```
pub fn find_unknown_rule_names(
    rules: &HashMap<String, Severity>,
) -> Option<UnknownRuleNames> {
    let unknowns: Vec<String> = rules
        .keys()
        .filter(|k| !KNOWN_LINT_RULES.contains(&k.as_str()))
        .cloned()
        .collect();
    if unknowns.is_empty() {
        None
    } else {
        Some(UnknownRuleNames::new(unknowns))
    }
}

/// Format the warning message for one or more unknown lint rule names.
///
/// AD-224-4: both the offending-name list and the recognised-rules list are
/// sorted lexicographically in the output, ensuring deterministic output across
/// runs and surfaces regardless of HashMap iteration order. The structural
/// precondition (at least one unknown name) is guaranteed by the `UnknownRuleNames`
/// type — only constructible with a non-empty list.
///
/// **CLI note:** the CLI applies `safe_inline` to each name BEFORE passing it here,
/// so the output of this function contains WIRE-escaped control bytes. The bindings
/// pass raw names (JSON encoding handles escaping for their output channel).
///
/// Produces one of:
/// - `"unknown lint rule 'NAME'; recognised rules are: ...; ignoring"`
/// - `"unknown lint rules: 'A', 'B'; recognised rules are: ...; ignoring"`
#[must_use]
pub fn format_unknown_rule_names_warning(names: &[String]) -> String {
    // Structural precondition: names must be non-empty.
    // UnknownRuleNames guarantees this, but callers passing &[String] directly
    // should ensure the same.
    assert!(!names.is_empty(), "format_unknown_rule_names_warning called with empty names");
    let recognised = KNOWN_LINT_RULES.join(", ");
    if names.len() == 1 {
        format!(
            "unknown lint rule '{}'; recognised rules are: {}; ignoring",
            names[0], recognised
        )
    } else {
        let quoted: Vec<String> = names.iter().map(|n| format!("'{n}'")).collect();
        format!(
            "unknown lint rules: {}; recognised rules are: {}; ignoring",
            quoted.join(", "),
            recognised
        )
    }
}

/// Per-rule severity override configuration.
///
/// Loaded from the `lint.rules` section of `mds.json`:
/// ```json
/// { "lint": { "rules": { "unused-variable": "off", "shadow-variable": "warn" } } }
/// ```
///
/// Absent rules default to the engine's built-in severity (defined per rule in the
/// rule catalog). Unknown rule names in the map produce a warning on every surface
/// (the rule simply has no effect, and lint continues). This is deliberate
/// forward-compatibility: a config naming a rule from a newer mds version warns
/// but does not break when run with an older binary.
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
    /// Unknown rule names in the map produce a warning via [`find_unknown_rule_names`]
    /// on every API surface; they do not cause this constructor to fail. Call
    /// [`find_unknown_rule_names`] before or after construction if you need to inspect
    /// or surface those names.
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
