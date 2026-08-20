//! Lint configuration: `LintConfig` with per-rule severity overrides.
//!
//! `LintConfig` is public (lives in mds-core) — the CLI converts the `mds.json`
//! `lint.rules` section into it, and all `lint_*` entry points accept a `&LintConfig`.
//!
//! **Unknown rule NAMEs** do not cause construction failures — the rule simply has no
//! effect and lint continues. The preferred construction path for surfaces that must
//! surface unknowns is [`LintConfig::from_rules_checked`]: it returns the config and
//! an unknowns report in one atomic call, making it structurally impossible to miss
//! the detection step. [`find_unknown_rule_names`] remains available for callers that
//! perform detection separately. mds-core itself emits no warning. The CLI's
//! `mds lint` warns on stderr; napi/WASM/Python return `lint_warnings`; `mds build`,
//! `check`, `fmt`, and `watch` do not warn — an accepted asymmetry, not an oversight.
//! This is deliberate forward-compatibility: severities are a closed set, but rule
//! names grow every release; hard-failing an unknown name would break a config naming
//! a newer rule when run with an older binary.
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
/// The names are sorted lexicographically. This type is `#[non_exhaustive]` per ADR-010:
/// use the [`UnknownRuleNames::names`] accessor, not a struct literal. External crates
/// MUST NOT construct it directly.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct UnknownRuleNames {
    /// Sorted, non-empty list of unknown rule names.
    names: Vec<String>,
}

impl UnknownRuleNames {
    fn new(mut names: Vec<String>) -> Self {
        // `sort_unstable` rather than `sort`: the names come from `HashMap` keys and are
        // therefore distinct, so stability is unobservable — and the stable merge sort
        // monomorphised for `String` is measurably larger in the WASM binary, which runs
        // against a hard size guard (AC-224-18).
        names.sort_unstable();
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
/// assert!(!KNOWN_LINT_RULES.is_empty());
/// ```
pub fn find_unknown_rule_names(rules: &HashMap<String, Severity>) -> Option<UnknownRuleNames> {
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

/// Format the warning message for the unknown lint rule names in `unknown`.
///
/// AD-224-4: both the offending-name list and the recognised-rules list are
/// sorted lexicographically in the output, so the message is byte-identical
/// across runs and surfaces regardless of `HashMap` iteration order.
///
/// The empty-input precondition is **structural, not asserted**: the only way to
/// obtain an [`UnknownRuleNames`] is [`find_unknown_rule_names`], which returns
/// `None` rather than an empty report. This function therefore cannot panic and
/// has no failure mode (PF-005 — a `debug_assert!` here would be a no-op in
/// release, and a release `assert!` would be a panic in a library).
///
/// Each name is WIRE-escaped with [`crate::sanitize_control_chars_wire`] before it is
/// interpolated (spec §7.5 per-field rule: a rule name is a single-line
/// identifier, never prose). A rule name is an arbitrary caller-supplied map key
/// and JSON `\uXXXX` escapes decode to real control bytes, so the escape is what
/// makes the returned string safe for a consumer to render (applies ADR-008,
/// avoids PF-014 — the input is sanitized, not the rendered output). The
/// recognised-rules list needs no escaping: it is a slice of compile-time string
/// literals.
///
/// AC-224-3 (amended criterion, repo-owner ruling 2026-08-16): the criterion
/// requires a shared message body, a shared recognised-rules list, and a shared
/// sort order across all five surfaces. The CLI prefix described below is permitted
/// by the amended criterion; it is not a residual or a self-waiver.
///
/// Called by the CLI and by the bindings (napi/WASM/Python) to build their
/// warning strings. The CLI wraps the returned string with a `"warning: in
/// mds.json: "` prefix so the source-file provenance appears on stderr; the
/// bindings use the returned string as-is for the `lint_warnings` field.
/// The print-discipline guard is satisfied on the CLI surface by passing the
/// return value through `safe_inline` inside `eprint_warning`'s `format!`
/// (`safe_inline` is `sanitize_control_chars_wire` by another name; the call
/// is idempotent because names are already wire-escaped here).
///
/// Produces one of:
/// - `"unknown lint rule 'NAME'; recognised rules are: ...; ignoring"`
/// - `"unknown lint rules: 'A', 'B'; recognised rules are: ...; ignoring"`
#[must_use]
pub fn format_unknown_rule_names_warning(unknown: &UnknownRuleNames) -> String {
    // Assembled with `push_str` rather than `format!` + `join`. This function is
    // reachable from the WASM binary, which runs against a hard 850,000-byte guard
    // (AC-224-18); `[&str]::join` and `Vec<String>::join` each monomorphise into
    // kilobytes there, and nothing else in the WASM surface pulls them in. The output
    // is byte-identical to the `format!` form.
    let names = unknown.names();
    let mut out = String::with_capacity(256);
    if let [only] = names {
        out.push_str("unknown lint rule '");
        out.push_str(&super::sanitize_control_chars_wire(only));
        out.push('\'');
    } else {
        out.push_str("unknown lint rules: ");
        for (i, name) in names.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('\'');
            out.push_str(&super::sanitize_control_chars_wire(name));
            out.push('\'');
        }
    }
    out.push_str("; recognised rules are: ");
    for (i, rule) in KNOWN_LINT_RULES.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(rule);
    }
    out.push_str("; ignoring");
    out
}

/// Per-rule severity override configuration.
///
/// Loaded from the `lint.rules` section of `mds.json`:
/// ```json
/// { "lint": { "rules": { "unused-variable": "off", "shadow-variable": "warn" } } }
/// ```
///
/// Absent rules default to the engine's built-in severity (defined per rule in the
/// rule catalog). Unknown rule names in the map do not cause construction to fail
/// — the rule simply has no effect, and lint continues. Detection is opt-in via
/// [`find_unknown_rule_names`]: mds-core itself emits no warning; callers are
/// responsible for surfacing unknowns. This is deliberate forward-compatibility:
/// a config naming a rule from a newer mds version does not break when run with
/// an older binary.
///
/// Unknown severity *values* (e.g. `"verbose"`) cause a hard parse error (`exit 2`)
/// because the closed enum has no sensible fallback.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Use `LintConfig::default()` for a config with all rules at engine defaults, or
/// [`LintConfig::from_rules_checked`] (preferred) to supply per-rule overrides and
/// receive an unknowns report in a single step. Do not construct via struct literal.
#[non_exhaustive]
#[derive(Debug, Default, Clone)]
pub struct LintConfig {
    /// Per-rule severity overrides. Key = rule name (e.g. `"unused-variable"`),
    /// value = desired severity. Empty map = all rules at built-in defaults.
    pub rules: HashMap<String, Severity>,
}

impl LintConfig {
    /// Construct a `LintConfig` with the given per-rule overrides AND detect any
    /// unknown rule names in one atomic step.
    ///
    /// This is the **preferred construction path** for any surface that must warn
    /// about unknown rule names (the 2026-08-12 ruling: unknown names warn and lint
    /// continues — the silence is the only thing that changes). The return type
    /// carries the unknowns report alongside the config so the caller cannot
    /// accidentally omit the detection step.
    ///
    /// Marked `#[must_use]` — the compiler emits a warning if the return value is
    /// discarded entirely, making it structurally harder to drop the unknowns report.
    /// Callers that deliberately do not need the report should write:
    /// `let (config, _) = LintConfig::from_rules_checked(map);`
    ///
    /// The asymmetry with unknown severities is deliberate and unchanged: severity
    /// values are a closed enum and hard-fail at the serde layer; rule names grow
    /// every release, so they warn rather than fail (AD-224-1).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use mds::{LintConfig, Severity};
    ///
    /// // All-known map: unknowns report is None.
    /// let (config, unknown) = LintConfig::from_rules_checked(HashMap::from([
    ///     ("unused-variable".to_string(), Severity::Off),
    /// ]));
    /// assert!(unknown.is_none());
    /// assert_eq!(config.severity_for("unused-variable"), Some(&Severity::Off));
    ///
    /// // Map with an unknown name alongside a known one: report fires, config loads.
    /// let (config2, unknown2) = LintConfig::from_rules_checked(HashMap::from([
    ///     ("no-such-rule".to_string(), Severity::Warn),
    ///     ("unused-variable".to_string(), Severity::Error),
    /// ]));
    /// let u = unknown2.expect("should detect unknown");
    /// assert_eq!(u.names(), &["no-such-rule".to_string()]);
    /// // The config still loads — the known sibling is queryable at its configured severity.
    /// // The unknown name is stored verbatim in the map, but the lint engine never queries
    /// // it (no rule implementation matches "no-such-rule"), so it has no effect on linting.
    /// assert_eq!(config2.severity_for("unused-variable"), Some(&Severity::Error));
    /// ```
    #[must_use]
    pub fn from_rules_checked(
        rules: HashMap<String, Severity>,
    ) -> (Self, Option<UnknownRuleNames>) {
        let unknown = find_unknown_rule_names(&rules);
        (LintConfig { rules }, unknown)
    }

    /// Look up the configured severity for a rule name.
    ///
    /// Returns `None` when the rule has no explicit override — callers should fall
    /// back to the rule's built-in default severity.
    pub fn severity_for(&self, rule: &str) -> Option<&Severity> {
        self.rules.get(rule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unknowns(names: &[&str]) -> UnknownRuleNames {
        let map: HashMap<String, Severity> = names
            .iter()
            .map(|n| ((*n).to_string(), Severity::Warn))
            .collect();
        find_unknown_rule_names(&map).expect("names must be unknown")
    }

    /// AC-224-2 / AC-224-3: singular form names the rule and every recognised rule.
    #[test]
    fn warning_singular_names_rule_and_full_registry() {
        let msg = format_unknown_rule_names_warning(&unknowns(&["no-such-rule"]));
        assert_eq!(
            msg,
            format!(
                "unknown lint rule 'no-such-rule'; recognised rules are: {}; ignoring",
                KNOWN_LINT_RULES.join(", ")
            )
        );
        for rule in KNOWN_LINT_RULES {
            assert!(msg.contains(rule), "message must list {rule}; got: {msg}");
        }
    }

    /// AC-224-3: multiple offenders are listed in sorted order, not map order.
    #[test]
    fn warning_plural_sorts_offenders() {
        // Inserted zzz-first; the output must still be aaa-first.
        let msg = format_unknown_rule_names_warning(&unknowns(&["zzz-bad", "aaa-bad", "mmm-bad"]));
        assert!(
            msg.starts_with("unknown lint rules: 'aaa-bad', 'mmm-bad', 'zzz-bad'; "),
            "offenders must be sorted lexicographically; got: {msg}"
        );
    }

    /// AC-224-3 (byte pin): plural branch output is byte-identical to the canonical
    /// `format!`/`join` equivalent. The singular branch is already pinned via
    /// `assert_eq!` in `warning_singular_names_rule_and_full_registry`; this closes
    /// the gap for the plural branch (complexity-11). The hand-rolled `push_str` join
    /// in `format_unknown_rule_names_warning` is justified by WASM binary size
    /// (AC-224-18) and must stay; this test is the accuracy contract for it.
    #[test]
    fn warning_plural_byte_identical_to_format_equivalent() {
        let msg = format_unknown_rule_names_warning(&unknowns(&["aaa-bad", "zzz-bad"]));
        assert_eq!(
            msg,
            format!(
                "unknown lint rules: 'aaa-bad', 'zzz-bad'; recognised rules are: {}; ignoring",
                KNOWN_LINT_RULES.join(", ")
            ),
            "plural branch must be byte-identical to the canonical format!/join form"
        );
    }

    /// AC-224-4 / ADR-008: a hostile rule name is WIRE-escaped by the formatter, so
    /// the string handed to a binding consumer carries no raw control byte.
    ///
    /// PF-018: the hostile characters are built from Rust `\u{..}` escapes at
    /// runtime — never authored as literal bytes in this file.
    #[test]
    fn warning_wire_escapes_hostile_rule_name() {
        let hostile = format!(
            "{}[31mEVIL{}X\nClean: totally-real.mds",
            '\u{1b}', '\u{202e}'
        );
        let msg = format_unknown_rule_names_warning(&unknowns(&[&hostile]));

        // Negative: no raw control byte survives.
        assert!(
            !msg.chars().any(|c| c.is_control()),
            "no raw control character may survive escaping; got: {msg:?}"
        );
        // Positive control (PF-013): the escaped literals are actually present, so the
        // negative assertion above cannot pass because the name never reached the message.
        for escaped in ["\\u001B", "\\u202E", "\\u000A"] {
            assert!(
                msg.contains(escaped),
                "{escaped} must appear in the escaped warning; got: {msg}"
            );
        }
        assert!(
            msg.contains("EVIL"),
            "non-vacuity: the rule name must reach the message; got: {msg}"
        );
    }
}
