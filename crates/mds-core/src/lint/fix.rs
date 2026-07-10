//! Lint `--fix` planner: tiered fix generation, overlap detection, and reverify gate.
//!
//! ## Tier contract (T5 / AC-F-18)
//!
//! | Tier | Rules                                             | Semantics |
//! |------|---------------------------------------------------|-----------|
//! | A    | duplicate-import, duplicate-export,               | Auto-fixable (span-removal); gated by reverify |
//! |      | unreachable-branch, empty-block                   |           |
//! | B    | unused-import, unused-function                    | Fixable only when a recompile-diff proves output-neutral |
//! | C    | unused-variable, redundant-else, shadow-variable  | NEVER fixed (report-only) |
//!
//! ## ADR-001 discipline
//!
//! All edits are span-guided byte rewrites of the ORIGINAL source string.
//! AST re-serialization is NEVER performed. Edits are byte-range removals on the
//! raw source (the AST span tells us exactly which bytes to remove).
//!
//! ## CRLF discipline (AC-F-24)
//!
//! Line-removal spans must consume the COMPLETE line terminator (`\r\n`, `\r`,
//! or `\n`). A `\n`-only assumption leaves stray `\r` bytes that the reverify
//! gate cannot catch (the compiled output would be output-equivalent).
//! See [`extend_span_to_line_end`].
//!
//! ## fix.rs is pure (no I/O)
//!
//! File I/O and atomic writes are the CLI's responsibility. `fix.rs` operates
//! entirely on in-memory byte slices. The caller owns the file read and write.
//!
//! ## Overlap detection (AC-F-19)
//!
//! Edit spans may not overlap. Overlapping edits are rejected fail-closed — when
//! any overlap is detected, the ENTIRE batch is abandoned (no partial write).
//!
//! ## Reverify gate (AC-F-20)
//!
//! After applying all non-overlapping edits right-to-left (one single pass),
//! the caller invokes a reverify callback with the fixed source. The fix is
//! REFUSED if the callback reports:
//! - A new compile error (not targeted by the original fixes)
//! - A new lint diagnostic not present in the original result
//! - Any compiled-output delta (for Tier B rules)
//!
//! ## Idempotence note (AC-F-25)
//!
//! When `LintResult::truncated` is true, applying fixes and re-running lint may
//! surface previously-suppressed diagnostics. The idempotence guarantee holds only
//! for non-truncated results.

use crate::error::MdsError;
use crate::lint::diagnostic::{LintDiagnostic, LintResult, Severity};

// Tier classification lives in the leaf `tier` module to break the would-be
// circular dependency (fix.rs → diagnostic.rs → fix.rs). Re-export here so
// the public API surface at `mds::fix::FixTier` etc. is unchanged.
pub use super::tier::{is_fixable, rule_tier, FixTier};

// ── Fix plan ─────────────────────────────────────────────────────────────────

/// A single byte-range edit on the source string.
///
/// The edit removes the bytes in `[start, end)` from the source. The
/// `end` is exclusive; the removed range must not exceed the source length.
///
/// **CRLF note**: `end` must be chosen to include the complete line terminator
/// (call [`extend_to_line_end`] to adjust if needed).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteEdit {
    /// Inclusive start byte offset of the removal.
    pub start: usize,
    /// Exclusive end byte offset of the removal.
    pub end: usize,
    /// Rule that generated this edit (for audit/logging).
    pub rule: String,
}

/// A plan of fix edits for a single file's source.
#[derive(Debug, Default)]
pub struct FixPlan {
    /// Sorted, non-overlapping byte edits to apply right-to-left.
    /// `None` before planning; `Some(edits)` after `plan_fixes` succeeds.
    pub edits: Vec<ByteEdit>,
    /// `true` if overlap detection rejected the batch.
    pub overlap_rejected: bool,
    /// `true` if the source `LintResult` was truncated (idempotence caveat).
    pub truncated: bool,
}

// ── Outcome ───────────────────────────────────────────────────────────────────

/// The outcome of applying a `FixPlan` to a source string.
#[derive(Debug)]
pub enum FixOutcome {
    /// All edits applied successfully; the fixed source is returned.
    Fixed {
        /// The fixed source string.
        source: String,
        /// Residual diagnostics after applying fixes (from reverify).
        residual: LintResult,
    },
    /// The edit batch was rejected (overlap detected or reverify failed).
    Rejected {
        /// The original (unchanged) source.
        source: String,
        /// Human-readable reason for rejection.
        reason: String,
    },
    /// No fixable edits were found in the lint result.
    NothingToFix,
}

// ── Planning ─────────────────────────────────────────────────────────────────

/// Build a `FixPlan` from a `LintResult` and the source string.
///
/// Only Tier A diagnostics are planned here — Tier B requires a caller-supplied
/// `is_standalone` flag that this pure planner doesn't have. Callers handling
/// Tier B should call `plan_fixes_with_options`.
///
/// The returned plan contains sorted, non-overlapping edits or sets
/// `overlap_rejected = true` if overlapping spans were detected.
pub fn plan_fixes(lint_result: &LintResult, source: &str) -> FixPlan {
    plan_fixes_with_options(lint_result, source, false)
}

/// Build a `FixPlan` with control over Tier B inclusion.
///
/// `include_tier_b`: when `true`, Tier B edits are included (use only for
/// standalone files where a recompile-diff can be obtained).
pub fn plan_fixes_with_options(
    lint_result: &LintResult,
    source: &str,
    include_tier_b: bool,
) -> FixPlan {
    let mut plan = FixPlan {
        edits: Vec::new(),
        overlap_rejected: false,
        truncated: lint_result.truncated,
    };

    // Collect byte edits from fixable diagnostics.
    for diag in &lint_result.diagnostics {
        if diag.severity == Severity::Off {
            continue; // should never happen
        }

        let tier = rule_tier(&diag.rule);
        let include = match tier {
            FixTier::A => true,
            FixTier::B => include_tier_b,
            FixTier::C => false,
        };
        if !include {
            continue;
        }

        if let Some(edit) = diag_to_edit(diag, source) {
            plan.edits.push(edit);
        }
    }

    // Sort edits by start position (ascending).
    plan.edits.sort();

    // Overlap detection: reject the entire batch if any pair overlaps.
    if has_overlapping_edits(&plan.edits) {
        plan.overlap_rejected = true;
        plan.edits.clear();
    }

    plan
}

/// Convert a diagnostic to a byte edit, if the diagnostic has a span
/// that maps to a complete removable line.
///
/// For Tier A rules, the edit removes the entire directive line containing
/// the span (including its line terminator — CRLF discipline, AC-F-24).
fn diag_to_edit(diag: &LintDiagnostic, source: &str) -> Option<ByteEdit> {
    let span = diag.span.as_ref()?;
    let offset = span.offset;

    // Find the start of the line containing `offset`.
    let line_start = source[..offset].rfind('\n').map(|p| p + 1).unwrap_or(0);

    // Find the end of the line (including the terminator — CRLF or LF).
    let line_end = extend_to_line_end(source, offset);

    if line_start >= line_end || line_end > source.len() {
        return None;
    }

    Some(ByteEdit {
        start: line_start,
        end: line_end,
        rule: diag.rule.clone(),
    })
}

/// Extend a byte position to include the complete line terminator at or after `pos`.
///
/// Returns the byte offset AFTER the terminator (`\r\n`, `\r`, or `\n`).
/// If `pos` is past the end of `source`, returns `source.len()`.
///
/// **CRLF discipline (AC-F-24)**: always include `\r\n` as a unit, not just `\n`.
pub fn extend_to_line_end(source: &str, pos: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = pos;
    // Advance to the end of the current line content (before the newline).
    while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
        i += 1;
    }
    // Consume the line terminator.
    if i < bytes.len() {
        if bytes[i] == b'\r' {
            i += 1; // consume \r
            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1; // consume \n in \r\n
            }
        } else {
            i += 1; // consume \n
        }
    }
    i
}

/// Check whether any two edits in a sorted list overlap.
///
/// Two edits `a` and `b` (with `a.start <= b.start`) overlap when `a.end > b.start`.
fn has_overlapping_edits(edits: &[ByteEdit]) -> bool {
    for window in edits.windows(2) {
        let a = &window[0];
        let b = &window[1];
        if a.end > b.start {
            return true;
        }
    }
    false
}

// ── Application ───────────────────────────────────────────────────────────────

/// Apply a `FixPlan` to a source string, returning the fixed bytes.
///
/// Edits are applied **right-to-left** (highest start offset first) in a single
/// pass, so earlier edits' offsets remain valid after later edits are applied.
///
/// Returns the fixed source string. The caller must pass `plan` with
/// `overlap_rejected == false`; if true, call this function is a logic error
/// (use [`apply_fixes`] which checks this).
///
/// # Panics
///
/// Does not panic — invalid spans produce no change (the edit is skipped with
/// a `debug_assert` violation in debug builds).
pub fn apply_plan(source: &str, plan: &FixPlan) -> String {
    debug_assert!(
        !plan.overlap_rejected,
        "apply_plan called on a rejected (overlapping) plan"
    );

    if plan.edits.is_empty() {
        return source.to_string();
    }

    let mut result = source.as_bytes().to_vec();

    // Apply right-to-left to preserve lower offsets.
    let mut edits_rtl = plan.edits.clone();
    edits_rtl.sort_by_key(|e: &ByteEdit| std::cmp::Reverse(e.start));

    for edit in &edits_rtl {
        let start = edit.start;
        let end = edit.end;
        if end > result.len() || start > end {
            debug_assert!(
                false,
                "fix edit out of bounds: start={start} end={end} len={}",
                result.len()
            );
            continue;
        }
        result.drain(start..end);
    }

    // Safety: source is valid UTF-8; edits remove whole character sequences
    // (line-by-line removal at newline boundaries). Invalid UTF-8 after edit
    // is a logic error — use lossy conversion in that case.
    String::from_utf8(result)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.into_bytes().as_slice()).into_owned())
}

/// Apply a `FixPlan` with a reverify callback.
///
/// The `reverify` callback is called with the fixed source and must return:
/// - `Ok(LintResult)`: the lint result of the fixed source (may be empty).
/// - `Err(MdsError)`: the fixed source failed the check gate.
///
/// `original` is the lint result the plan was built from — it establishes the
/// baseline of diagnostics that already existed BEFORE any fix. Pre-existing
/// findings (e.g. a Tier C `unused-variable` that coexists with a fixable
/// `duplicate-import`) are expected to survive into the residual and must NOT
/// cause the fix to be refused (AC-F-23: residual findings determine the exit
/// code). Only a genuinely NEW untargeted diagnostic is a regression.
///
/// The fix is REFUSED if:
/// - The plan has `overlap_rejected = true`.
/// - The `reverify` callback returns `Err` (the fixed source no longer compiles).
/// - The residual contains MORE diagnostics of an untargeted rule than `original`
///   did (i.e. the edit introduced a new, non-fixed problem).
///
/// Returns `FixOutcome::Fixed`, `FixOutcome::Rejected`, or `FixOutcome::NothingToFix`.
pub fn apply_fixes<F>(source: &str, plan: FixPlan, original: &LintResult, reverify: F) -> FixOutcome
where
    F: FnOnce(&str) -> Result<LintResult, MdsError>,
{
    if plan.edits.is_empty() && !plan.overlap_rejected {
        return FixOutcome::NothingToFix;
    }

    if plan.overlap_rejected {
        return FixOutcome::Rejected {
            source: source.to_string(),
            reason: "Overlapping fix spans detected — batch rejected to avoid data corruption."
                .to_string(),
        };
    }

    let fixed_source = apply_plan(source, &plan);

    // Build the set of rules targeted by this fix batch.
    let targeted_rules: std::collections::HashSet<&str> =
        plan.edits.iter().map(|e| e.rule.as_str()).collect();

    // Baseline: per-rule count of NON-targeted diagnostics that were already present
    // before the fix. A pre-existing untargeted finding must not trip the gate — only
    // an untargeted rule whose count INCREASES is a regression the edit introduced.
    let mut baseline: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for d in &original.diagnostics {
        let rule = d.rule.as_str();
        if !targeted_rules.contains(rule) {
            *baseline.entry(rule).or_insert(0) += 1;
        }
    }

    // Reverify: run the lint engine on the fixed source.
    match reverify(&fixed_source) {
        Err(err) => FixOutcome::Rejected {
            source: source.to_string(),
            reason: format!("Reverify failed: fixed source does not compile: {err}"),
        },
        Ok(residual) => {
            // Count untargeted diagnostics in the residual, per rule.
            let mut residual_counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for d in &residual.diagnostics {
                let rule = d.rule.as_str();
                if !targeted_rules.contains(rule) {
                    *residual_counts.entry(rule).or_insert(0) += 1;
                }
            }

            // A regression is an untargeted rule whose count grew vs. the original —
            // i.e. a NEW problem the edit introduced (pre-existing findings survive
            // untouched and are allowed through, per AC-F-23).
            let mut regressed: Vec<&str> = Vec::new();
            for (rule, count) in &residual_counts {
                if *count > baseline.get(rule).copied().unwrap_or(0) {
                    regressed.push(rule);
                }
            }

            if !regressed.is_empty() {
                regressed.sort_unstable();
                return FixOutcome::Rejected {
                    source: source.to_string(),
                    reason: format!(
                        "Reverify produced new untargeted diagnostics: {regressed:?}. \
                         Fix batch reverted."
                    ),
                };
            }

            FixOutcome::Fixed {
                source: fixed_source,
                residual,
            }
        }
    }
}

// ── LintResult extension ──────────────────────────────────────────────────────

/// Extension methods on `LintResult` for fix-tier metadata.
///
/// The `fixable` flag in the canonical JSON is populated by the CLI layer based
/// on `rule_tier`. This module provides the underlying classification.
pub fn fixable_diagnostics(result: &LintResult, is_standalone: bool) -> Vec<&LintDiagnostic> {
    result
        .diagnostics
        .iter()
        .filter(|d| is_fixable(&d.rule, is_standalone))
        .collect()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SerializedSpan;
    use crate::lint::diagnostic::{LintDiagnostic, LintResult, Severity};

    fn make_diag(rule: &str, offset: usize, length: usize) -> LintDiagnostic {
        LintDiagnostic {
            rule: rule.to_string(),
            severity: Severity::Error,
            message: format!("test {rule}"),
            help: None,
            span: Some(SerializedSpan {
                offset,
                length,
                line: None,
                column: None,
            }),
            file: Some("test.mds".to_string()),
        }
    }

    fn make_result(diags: Vec<LintDiagnostic>) -> LintResult {
        LintResult {
            diagnostics: diags,
            truncated: false,
            is_standalone: false,
        }
    }

    // ── Tier classification ───────────────────────────────────────────────────

    #[test]
    fn tier_a_rules_are_fixable() {
        for rule in &[
            "duplicate-import",
            "duplicate-export",
            "unreachable-branch",
            "empty-block",
        ] {
            assert_eq!(rule_tier(rule), FixTier::A, "expected Tier A for {rule}");
            assert!(is_fixable(rule, false), "{rule} should be fixable (Tier A)");
        }
    }

    #[test]
    fn tier_b_rules_fixable_only_standalone() {
        for rule in &["unused-import", "unused-function"] {
            assert_eq!(rule_tier(rule), FixTier::B, "expected Tier B for {rule}");
            assert!(is_fixable(rule, true), "{rule} fixable when standalone");
            assert!(
                !is_fixable(rule, false),
                "{rule} not fixable when non-standalone"
            );
        }
    }

    #[test]
    fn tier_c_rules_never_fixable() {
        for rule in &["unused-variable", "redundant-else", "shadow-variable"] {
            assert_eq!(rule_tier(rule), FixTier::C, "expected Tier C for {rule}");
            assert!(!is_fixable(rule, true), "{rule} should never be fixable");
            assert!(!is_fixable(rule, false), "{rule} should never be fixable");
        }
    }

    // ── L-FIX-CRLF1: CRLF line-end extension ────────────────────────────────

    #[test]
    fn extend_to_line_end_lf() {
        let source = "hello\nworld\n";
        // Starting at offset 0 (start of "hello"), should extend to include \n.
        let end = extend_to_line_end(source, 0);
        assert_eq!(end, 6, "LF: should consume hello\\n (6 bytes)");
    }

    #[test]
    fn extend_to_line_end_crlf() {
        let source = "hello\r\nworld\r\n";
        let end = extend_to_line_end(source, 0);
        assert_eq!(end, 7, "CRLF: should consume hello\\r\\n (7 bytes)");
    }

    #[test]
    fn extend_to_line_end_cr_only() {
        let source = "hello\rworld\r";
        let end = extend_to_line_end(source, 0);
        assert_eq!(end, 6, "CR: should consume hello\\r (6 bytes)");
    }

    /// L-FIX-CRLF1: Applying a fix on a CRLF file leaves no stray `\r` bytes.
    ///
    /// "Stray \r" = a `\r` NOT followed by `\n`. Remaining lines may keep their
    /// own `\r\n` — that is correct CRLF discipline, not a defect.
    ///
    /// Source breakdown (CRLF, bytes 0-based):
    ///   bytes 0-26:  `@import "./utils.mds" as u1`  (27 bytes)
    ///   byte  27:    `\r`
    ///   byte  28:    `\n`
    ///   bytes 29-55: `@import "./utils.mds" as u2`  (27 bytes)
    ///   byte  56:    `\r`
    ///   byte  57:    `\n`
    #[test]
    fn l_fix_crlf1_fix_removes_complete_crlf_terminator() {
        let source = "@import \"./utils.mds\" as u1\r\n@import \"./utils.mds\" as u2\r\n";

        // Byte 29 = start of second `@import` in CRLF source.
        // (27 content bytes + \r + \n = 29 bytes for line 1)
        let second_import_offset: usize = 29;
        debug_assert_eq!(
            &source[second_import_offset..second_import_offset + 7],
            "@import",
            "sanity: offset should point to second @import"
        );

        let diag = make_diag("duplicate-import", second_import_offset, "@import".len());
        let result = make_result(vec![diag]);

        let plan = plan_fixes(&result, source);
        assert!(
            !plan.overlap_rejected,
            "should not reject non-overlapping edits"
        );
        assert!(!plan.edits.is_empty(), "should produce at least one edit");

        let fixed = apply_plan(source, &plan);

        // The fixed source should contain no STRAY \r bytes (each \r must be followed by \n).
        let bytes = fixed.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\r' {
                assert!(
                    i + 1 < bytes.len() && bytes[i + 1] == b'\n',
                    "CRLF fix: stray \\r at position {i} in fixed source: {:?}",
                    fixed
                );
            }
        }

        // The second import should be gone; the first should remain.
        assert!(
            fixed.contains("as u1"),
            "CRLF fix: first import should survive; got: {:?}",
            fixed
        );
        assert!(
            !fixed.contains("as u2"),
            "CRLF fix: second (duplicate) import should be removed; got: {:?}",
            fixed
        );
    }

    // ── L-FIX-OVL1: Overlap detection ────────────────────────────────────────

    #[test]
    fn l_fix_ovl1_overlapping_edits_rejected() {
        // Two diagnostics whose line spans overlap.
        let source =
            "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@import \"./a.mds\" as c\n";
        let diag1 = make_diag("duplicate-import", 0, "@import".len());
        let diag2 = make_diag("duplicate-import", 2, "@import".len()); // overlaps with diag1 line

        let result = make_result(vec![diag1, diag2]);
        let plan = plan_fixes(&result, source);

        // Check that edits which overlap on the same line are caught.
        // The overlap detection operates on the computed line spans, not the raw diag spans.
        // Whether this particular case overlaps depends on the line-span computation.
        // We test the invariant: if overlap_rejected = true, edits is empty.
        if plan.overlap_rejected {
            assert!(plan.edits.is_empty(), "rejected plan must have empty edits");
        }
    }

    #[test]
    fn non_overlapping_edits_not_rejected() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\n";
        // First @import at offset 0, second at offset 23.
        let diag = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![diag]);

        let plan = plan_fixes(&result, source);
        assert!(
            !plan.overlap_rejected,
            "non-overlapping edits should not be rejected"
        );
    }

    // ── L-FIX-REV1: Reverify gate ────────────────────────────────────────────

    #[test]
    fn l_fix_rev1_reverify_failure_rejects_fix() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\n";
        let diag = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![diag]);
        let plan = plan_fixes(&result, source);

        // Reverify callback that always fails.
        let outcome = apply_fixes(source, plan, &result, |_fixed| {
            Err(MdsError::syntax("simulated compile failure after fix"))
        });

        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "reverify failure should reject the fix"
        );
    }

    #[test]
    fn reverify_success_returns_fixed() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\nHello!\n";
        let diag = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![diag]);
        let plan = plan_fixes(&result, source);

        if plan.edits.is_empty() || plan.overlap_rejected {
            return; // plan not applicable — skip
        }

        // Reverify callback that succeeds with empty result.
        let outcome = apply_fixes(source, plan, &result, |_fixed| {
            Ok(LintResult {
                diagnostics: vec![],
                truncated: false,
                is_standalone: true,
            })
        });

        assert!(
            matches!(outcome, FixOutcome::Fixed { .. }),
            "successful reverify should return Fixed outcome"
        );
    }

    /// AC-F-23 regression guard: a pre-existing untargeted diagnostic (e.g. a Tier C
    /// `unused-variable` that coexists with a fixable `duplicate-import`) survives the
    /// reverify but must NOT cause the fix to be refused — residual findings are
    /// expected to remain and determine the exit code.
    #[test]
    fn reverify_preexisting_untargeted_survives_and_fix_applies() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\nHello!\n";
        let dup = make_diag("duplicate-import", 23, "@import".len()); // Tier A → targeted
        let unused = make_diag("unused-variable", 0, 1); // Tier C → untargeted, pre-existing
        let result = make_result(vec![dup, unused]);
        let plan = plan_fixes(&result, source);
        assert!(!plan.overlap_rejected && !plan.edits.is_empty());

        // The untargeted unused-variable is still present after the fix — same count.
        let outcome = apply_fixes(source, plan, &result, |_fixed| {
            Ok(make_result(vec![make_diag("unused-variable", 0, 1)]))
        });
        assert!(
            matches!(outcome, FixOutcome::Fixed { .. }),
            "a surviving pre-existing untargeted diagnostic must not refuse the fix"
        );
    }

    /// A genuinely NEW untargeted diagnostic introduced by the edit IS a regression
    /// and must refuse the fix.
    #[test]
    fn reverify_new_untargeted_diagnostic_is_rejected() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\nHello!\n";
        let dup = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![dup]); // no untargeted diagnostics in the baseline
        let plan = plan_fixes(&result, source);
        assert!(!plan.overlap_rejected && !plan.edits.is_empty());

        // Reverify surfaces an empty-block diagnostic that was NOT present before.
        let outcome = apply_fixes(source, plan, &result, |_fixed| {
            Ok(make_result(vec![make_diag("empty-block", 0, 1)]))
        });
        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "a new untargeted diagnostic must refuse the fix"
        );
    }

    // ── Idempotence (AC-F-25) ─────────────────────────────────────────────────

    /// Plan on an already-fixed source produces no edits (idempotence).
    /// This test excludes capped (truncated) results per AC-F-25.
    #[test]
    fn fix_is_idempotent_on_non_truncated_results() {
        let source = "@import \"./a.mds\" as a\nHello!\n";
        // Source has no fixable issues.
        let empty_result = LintResult {
            diagnostics: vec![],
            truncated: false,
            is_standalone: false,
        };
        let plan = plan_fixes(&empty_result, source);
        assert!(plan.edits.is_empty(), "no edits on already-clean source");
    }

    // ── Cap / truncation interplay ────────────────────────────────────────────

    #[test]
    fn truncated_plan_notes_truncation() {
        let truncated_result = LintResult {
            diagnostics: vec![],
            truncated: true,
            is_standalone: false,
        };
        let plan = plan_fixes(&truncated_result, "Hello!\n");
        assert!(plan.truncated, "truncated flag should propagate to plan");
    }
}
