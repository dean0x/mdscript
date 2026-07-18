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

/// A fix edit that was rejected by the per-edit reverify gate in [`apply_fixes_incremental`].
#[derive(Debug, Clone)]
pub struct RejectedEdit {
    /// The edit that was rejected.
    pub edit: ByteEdit,
    /// Human-readable reason for rejection.
    pub reason: String,
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
///
/// Marked `#[non_exhaustive]` so that adding new variants in a future release
/// does not constitute a semver-breaking change for downstream crates that match
/// on this enum. External callers must include a `_ => {}` wildcard arm.
#[derive(Debug)]
#[non_exhaustive]
pub enum FixOutcome {
    /// All edits applied successfully; the fixed source is returned.
    Fixed {
        /// The fixed source string.
        source: String,
        /// Residual diagnostics after applying fixes (from reverify).
        residual: LintResult,
    },
    /// Some edits applied, some individually rejected by the per-edit reverify gate.
    ///
    /// Returned only by [`apply_fixes_incremental`] when the full batch is refused but at
    /// least one individual edit passes the reverify gate.  The `source` field holds the
    /// partially-fixed text; `residual` carries the residual diagnostics (from the last
    /// successful per-edit reverify); `rejected` lists every edit that was turned down.
    PartiallyFixed {
        /// The partially-fixed source (accepted edits applied, rejected edits untouched).
        source: String,
        /// Residual diagnostics from the last successful per-edit reverify pass.
        residual: LintResult,
        /// Edits that were individually rejected by the reverify gate.
        rejected: Vec<RejectedEdit>,
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
#[must_use = "a dropped FixPlan silently discards planned fix edits"]
pub fn plan_fixes(lint_result: &LintResult, source: &str) -> FixPlan {
    plan_fixes_with_options(lint_result, source, false)
}

/// Build a `FixPlan` with control over Tier B inclusion.
///
/// `include_tier_b`: when `true`, Tier B edits are included (use only for
/// standalone files where a recompile-diff can be obtained).
#[must_use = "a dropped FixPlan silently discards planned fix edits"]
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
    // `str::get(..offset)` returns None for out-of-range or non-char-boundary
    // offsets — fail-closed per ADR-001 rather than panicking on a bad span.
    let prefix = source.get(..offset)?;
    let line_start = prefix.rfind('\n').map(|p| p + 1).unwrap_or(0);

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
    // Clamp: if pos is past the end of source, start scanning from source.len().
    // This satisfies the documented contract ("if pos is past the end of source,
    // returns source.len()") — applies ADR-001 fail-closed semantics.
    let mut i = pos.min(bytes.len());
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

// ── Shared reverify helpers ───────────────────────────────────────────────────

/// Count non-targeted diagnostics per rule (used for regression detection in the reverify gate).
///
/// Returns a `HashMap<&str, usize>` mapping rule name → occurrence count for every diagnostic
/// whose rule is NOT in `targeted`.  Used to build the pre-fix baseline and to count post-fix
/// residuals so the two can be compared (AC-F-23).
///
/// Keys borrow from `diags` — no allocation per entry (issue #68 regression fix).
fn count_untargeted_per_rule<'a>(
    diags: &'a [LintDiagnostic],
    targeted: &std::collections::HashSet<String>,
) -> std::collections::HashMap<&'a str, usize> {
    let mut counts = std::collections::HashMap::new();
    for d in diags {
        if !targeted.contains(d.rule.as_str()) {
            *counts.entry(d.rule.as_str()).or_insert(0) += 1;
        }
    }
    counts
}

/// Return the sorted list of rule names whose count increased vs `baseline` (regressions).
///
/// A rule is regressed when its count in `residual_counts` is strictly greater than its count in
/// `baseline`.  Pre-existing untargeted findings (same or lower count) are allowed through (AC-F-23).
fn regressed_rules(
    residual_counts: &std::collections::HashMap<&str, usize>,
    baseline: &std::collections::HashMap<&str, usize>,
) -> Vec<String> {
    let mut regressed = Vec::new();
    for (&rule, &count) in residual_counts {
        if count > baseline.get(rule).copied().unwrap_or(0) {
            regressed.push(rule.to_string());
        }
    }
    regressed.sort_unstable();
    regressed
}

// ── Application ───────────────────────────────────────────────────────────────

/// Apply a `FixPlan` to a source string, returning the fixed source.
///
/// Edits are applied **right-to-left** (highest start offset first) in a
/// single pass, so earlier edits' offsets remain valid after later edits are
/// applied.
///
/// # `_unchecked` suffix — ADR-001
///
/// This function bypasses the ADR-001 reverify gate (compile-equivalence
/// check) — it applies edits without recompiling or verifying that the fixed
/// source produces identical compiled output. Production code that writes back
/// to disk **must** use [`apply_fixes`] instead, which gates on the reverify
/// callback before returning `FixOutcome::Fixed`. `apply_plan_unchecked` is
/// provided for the `--fix --diff` / `--fix --check` diff-preview path (which
/// computes the delta without writing it) and for unit tests. Calling it on a
/// write path without a subsequent reverify is an anti-pattern — the reverify
/// gate is the only guard against a fix that accidentally changes compiled
/// semantics.
///
/// The caller must pass `plan` with `overlap_rejected == false`; if true,
/// calling this function is a logic error (use [`apply_fixes`] which checks
/// this).
///
/// # Panics
///
/// Does not panic — invalid spans produce no change (the edit is skipped with
/// a `debug_assert` violation in debug builds).
pub fn apply_plan_unchecked(source: &str, plan: &FixPlan) -> String {
    debug_assert!(
        !plan.overlap_rejected,
        "apply_plan_unchecked called on a rejected (overlapping) plan"
    );

    if plan.edits.is_empty() {
        return source.to_string();
    }

    let mut result = source.as_bytes().to_vec();

    // `plan.edits` must be sorted ascending by start offset (guaranteed by
    // plan_fixes_with_options which calls `edits.sort()` before returning).
    // Iterate right-to-left with `.rev()` — no clone or re-sort needed.
    //
    // Unconditional assert (not debug_assert) — avoids PF-005: the sortedness
    // precondition for right-to-left accumulation is release-critical; a
    // debug_assert! would be compiled out in release builds, allowing unsorted
    // edits to silently corrupt the source. The `_unchecked` callers in
    // `apply_fixes` and `apply_fixes_incremental` perform their own fail-closed
    // guards before reaching here; this assert is defense-in-depth for direct
    // external callers who bypass those guards.
    assert!(
        plan.edits.windows(2).all(|w| w[0].start <= w[1].start),
        "apply_plan_unchecked: edits must be sorted ascending by start offset (avoids PF-005)"
    );
    for edit in plan.edits.iter().rev() {
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
/// - The `reverify` callback returns `Err`. The CLI reverify path checks three
///   conditions inside this closure (AC-F-20): (1) recompile-success — the fixed
///   source must still compile; (2) no-new-untargeted-diagnostics — the residual
///   must not introduce new findings beyond the targeted rules; (3) output
///   byte-equality — when the original source is standalone-compilable, compiled
///   output of the fixed source must be byte-identical to the original (enforced by
///   the caller returning `Err` on delta). All real auto-fixes are output-neutral
///   by design; any delta signals a bug in the fix logic and must be refused.
/// - The residual contains MORE diagnostics of an untargeted rule than `original`
///   did (i.e. the edit introduced a new, non-fixed problem).
///
/// Returns `FixOutcome::Fixed`, `FixOutcome::Rejected`, or `FixOutcome::NothingToFix`.
#[must_use = "a dropped FixOutcome silently discards the fix result"]
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

    // Sortedness guard (avoids PF-005): edits must be sorted ascending by start offset for the
    // right-to-left application in apply_plan_unchecked to be correct. A debug_assert!-only guard
    // is compiled out in release builds, where unsorted edits cause silent source corruption.
    // This unconditional check returns Rejected before apply_plan_unchecked is reached.
    if plan.edits.windows(2).any(|w| w[0].start > w[1].start) {
        return FixOutcome::Rejected {
            source: source.to_string(),
            reason: "Fix edits are not sorted ascending by start offset; refusing to apply \
                     to prevent source corruption (avoids PF-005)."
                .to_string(),
        };
    }

    let fixed_source = apply_plan_unchecked(source, &plan);

    // Build the set of rules targeted by this fix batch.
    let targeted_rules: std::collections::HashSet<String> =
        plan.edits.iter().map(|e| e.rule.clone()).collect();

    // Baseline: per-rule count of NON-targeted diagnostics that were already present
    // before the fix. A pre-existing untargeted finding must not trip the gate — only
    // an untargeted rule whose count INCREASES is a regression the edit introduced.
    let baseline = count_untargeted_per_rule(&original.diagnostics, &targeted_rules);

    // Reverify: run the lint engine on the fixed source.
    match reverify(&fixed_source) {
        Err(err) => FixOutcome::Rejected {
            source: source.to_string(),
            reason: format!("Reverify failed: fixed source does not compile: {err}"),
        },
        Ok(residual) => {
            // Count untargeted diagnostics in the residual, per rule.
            let residual_counts = count_untargeted_per_rule(&residual.diagnostics, &targeted_rules);

            // A regression is an untargeted rule whose count grew vs. the original —
            // i.e. a NEW problem the edit introduced (pre-existing findings survive
            // untouched and are allowed through, per AC-F-23).
            let regressed = regressed_rules(&residual_counts, &baseline);

            if !regressed.is_empty() {
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

/// Maximum number of edits for which the per-edit fallback path is attempted when the
/// batch reverify fails.
///
/// Each per-edit reverify call incurs ~3 full module resolves + 2 dependency disk sweeps
/// (every `lint_str_with` builds a fresh `ModuleCache::new()`). For plans exceeding this
/// cap, `apply_fixes_incremental` returns `FixOutcome::Rejected` fail-closed instead of
/// attempting up to `plan.edits.len()` additional reverify calls.
///
/// The batch attempt (1 call) is always made regardless of plan size — only the fallback
/// is capped. Applies PF-004 (resource cap must hold on all paths, not just the primary one).
pub const FALLBACK_MAX_EDITS: usize = 50;

/// Apply a `FixPlan` with a bounded per-edit fallback.
///
/// Attempts the full edit batch first (one reverify call). If the batch is rejected by the
/// reverify gate, falls back to right-to-left per-edit retry: each edit is tested individually
/// against the running (partially-fixed) source. Accepted edits accumulate; rejected edits are
/// collected in [`RejectedEdit`] entries.
///
/// **Reverify call bound:** ≤ `plan.edits.len() + 1` total calls across both strategies
/// (1 batch attempt + at most `edits.len()` individual retries), subject to [`FALLBACK_MAX_EDITS`].
/// When `plan.edits.len() > FALLBACK_MAX_EDITS` and the batch fails, the function returns
/// `Rejected` immediately without attempting per-edit retries.
///
/// **Right-to-left accumulation:** Edits are sorted ascending by offset (guaranteed by
/// [`plan_fixes`]/[`plan_fixes_with_options`]). Per-edit retry processes them highest-offset-first
/// (`iter().rev()`). Each accepted high-offset edit shortens the source at a higher byte position,
/// leaving lower-offset bytes untouched — so subsequent lower-offset edits remain positionally valid.
///
/// Returns:
/// - [`FixOutcome::Fixed`] — all edits accepted (full batch or all-per-edit passes).
/// - [`FixOutcome::PartiallyFixed`] — at least one edit accepted and at least one rejected.
/// - [`FixOutcome::Rejected`] — overlap detected, unsorted edits, cap exceeded, or ALL
///   per-edit retries refused.  The `reason` field includes the actual reverify failure
///   messages (applies ADR-004).
/// - [`FixOutcome::NothingToFix`] — empty plan with no overlap.
///
/// Unlike [`apply_fixes`] which requires `F: FnOnce`, this function requires `F: Fn` because
/// `reverify` may be called up to `plan.edits.len() + 1` times.
#[must_use = "a dropped FixOutcome silently discards the fix result"]
pub fn apply_fixes_incremental<F>(
    source: &str,
    plan: FixPlan,
    original: &LintResult,
    reverify: F,
) -> FixOutcome
where
    F: Fn(&str) -> Result<LintResult, MdsError>,
{
    // Overlap is detected statically in plan_fixes; edits are cleared when overlap is found.
    // Per-edit retry cannot rescue an overlap batch — refuse it fail-closed.
    if plan.overlap_rejected {
        return FixOutcome::Rejected {
            source: source.to_string(),
            reason: "Overlapping fix spans detected — batch rejected to avoid data corruption."
                .to_string(),
        };
    }

    if plan.edits.is_empty() {
        return FixOutcome::NothingToFix;
    }

    // Sortedness guard (avoids PF-005): edits must be sorted ascending by start offset for the
    // right-to-left application to be correct. A debug_assert!-only guard would be compiled out
    // in release builds, where unsorted edits silently corrupt the source that is written to disk.
    // This unconditional check returns Rejected before apply_plan_unchecked is reached.
    if plan.edits.windows(2).any(|w| w[0].start > w[1].start) {
        return FixOutcome::Rejected {
            source: source.to_string(),
            reason: "Fix edits are not sorted ascending by start offset; refusing to apply \
                     to prevent source corruption (avoids PF-005)."
                .to_string(),
        };
    }

    let targeted_rules: std::collections::HashSet<String> =
        plan.edits.iter().map(|e| e.rule.clone()).collect();
    let baseline = count_untargeted_per_rule(&original.diagnostics, &targeted_rules);

    // ── Batch attempt (one reverify call) ─────────────────────────────────────
    // Saves per-edit calls for the common case where all edits are compatible.
    let batch_source = apply_plan_unchecked(source, &plan);
    match reverify(&batch_source) {
        Ok(residual) => {
            let residual_counts = count_untargeted_per_rule(&residual.diagnostics, &targeted_rules);
            let regressed = regressed_rules(&residual_counts, &baseline);
            if regressed.is_empty() {
                return FixOutcome::Fixed {
                    source: batch_source,
                    residual,
                };
            }
            // Batch introduced regressions; fall through to per-edit retry.
        }
        Err(_) => {
            // Batch failed reverify; fall through to per-edit retry.
        }
    }

    // ── Resource cap (PF-004) ─────────────────────────────────────────────────
    // The per-edit fallback calls reverify up to plan.edits.len() more times. Each call
    // is ~3 module resolves + 2 disk sweeps (fresh ModuleCache per call). For large plans
    // in directory mode this is prohibitively expensive — cap fail-closed.
    if plan.edits.len() > FALLBACK_MAX_EDITS {
        return FixOutcome::Rejected {
            source: source.to_string(),
            reason: format!(
                "Fix plan has {} edits; per-edit fallback cap is {} — batch was rejected by \
                 the reverify gate. Re-run --fix after manually reducing the issue count \
                 (avoids PF-004).",
                plan.edits.len(),
                FALLBACK_MAX_EDITS
            ),
        };
    }

    // ── Per-edit fallback (≤ edits.len() more reverify calls) ─────────────────
    // Process right-to-left: previously accepted high-offset changes do not
    // invalidate the byte positions of lower-offset edits processed next.
    let mut running_source = source.to_string();
    let mut last_residual: Option<LintResult> = None;
    let mut rejected: Vec<RejectedEdit> = Vec::new();
    let mut accepted_count: usize = 0;

    for edit in plan.edits.iter().rev() {
        let single_plan = FixPlan {
            edits: vec![edit.clone()],
            overlap_rejected: false,
            truncated: false,
        };
        let test_source = apply_plan_unchecked(&running_source, &single_plan);

        let reverify_result = reverify(&test_source);
        let reject_reason: Option<String> = match &reverify_result {
            Err(err) => Some(format!(
                "Reverify failed: fixed source does not compile: {err}"
            )),
            Ok(residual) => {
                // Use the full targeted_rules set (identical to the baseline) so the
                // comparison is symmetric: other targeted-rule diagnostics that are
                // still present (not yet fixed) must not be counted as regressions.
                // Using a single-rule targeted set would cause false positives when
                // two rules share the same baseline (e.g. empty-block + duplicate-export
                // both targeted → applying dup-export fix alone leaves empty-block in
                // residual, which would incorrectly look like a new regression against
                // a baseline that excluded it).
                let residual_counts =
                    count_untargeted_per_rule(&residual.diagnostics, &targeted_rules);
                let regressed = regressed_rules(&residual_counts, &baseline);
                if !regressed.is_empty() {
                    Some(format!(
                        "Reverify produced new untargeted diagnostics: {regressed:?}. \
                         Edit reverted."
                    ))
                } else {
                    None
                }
            }
        };

        if let Some(reason) = reject_reason {
            rejected.push(RejectedEdit {
                edit: edit.clone(),
                reason,
            });
        } else {
            running_source = test_source;
            if let Ok(residual) = reverify_result {
                last_residual = Some(residual);
            }
            accepted_count += 1;
        }
    }

    if accepted_count == 0 {
        // Surface the real per-edit rejection reasons (applies ADR-004): the three-tier
        // safety gate is only as useful as its refusal reporting. Include actual reverify
        // failure messages so callers can diagnose why every edit was refused.
        let reason = if rejected.is_empty() {
            // Defensive: should not reach here with an empty rejected vec when
            // accepted_count==0 and the plan was non-empty, but fail-safe.
            "All fix edits were rejected by the per-edit reverify gate.".to_string()
        } else if rejected.len() == 1 {
            rejected[0].reason.clone()
        } else {
            let reasons = rejected
                .iter()
                .map(|r| r.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            format!("All {} fix edits rejected: {}", rejected.len(), reasons)
        };
        return FixOutcome::Rejected {
            source: source.to_string(),
            reason,
        };
    }

    // invariant: accepted_count > 0 → at least one Ok(residual) was stored above,
    // because reject_reason is None only when reverify_result is Ok(_). Fail closed
    // rather than panic in case the invariant is ever violated.
    let residual = match last_residual {
        Some(r) => r,
        None => {
            return FixOutcome::Rejected {
                source: source.to_string(),
                reason: "internal: reverify residual missing despite accepted_count > 0; \
                         fix aborted to preserve correctness"
                    .to_string(),
            };
        }
    };

    if rejected.is_empty() {
        FixOutcome::Fixed {
            source: running_source,
            residual,
        }
    } else {
        FixOutcome::PartiallyFixed {
            source: running_source,
            residual,
            rejected,
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

    /// I-09 regression: `extend_to_line_end` with `pos` past source end must return
    /// `source.len()`, not `pos`.
    ///
    /// Doc contract: "If pos is past the end of source, returns source.len()."
    /// Without `pos.min(bytes.len())`, `i` starts at `pos` and the while-loop body
    /// never executes, so the function would return `pos` (out-of-range). The clamp
    /// fixes this (applies ADR-001 fail-closed semantics).
    #[test]
    fn extend_to_line_end_past_end_returns_source_len() {
        let source = "hello\n"; // 6 bytes
                                // pos well past end
        assert_eq!(
            extend_to_line_end(source, source.len() + 10),
            source.len(),
            "pos past end must return source.len(), not pos"
        );
        // pos == source.len() (exactly at the end boundary) must also return source.len()
        assert_eq!(
            extend_to_line_end(source, source.len()),
            source.len(),
            "pos == source.len() must return source.len()"
        );
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

        let fixed = apply_plan_unchecked(source, &plan);

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

    /// AC-F-19: two diagnostics that map to the same line produce identical
    /// ByteEdits (`{ start: 0, end: 23 }`). The overlap detector fires because
    /// `a.end (23) > b.start (0)`. The whole batch is rejected — edits cleared,
    /// `apply_fixes` returns `FixOutcome::Rejected`.
    #[test]
    fn l_fix_ovl1_overlapping_edits_rejected() {
        let source =
            "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@import \"./a.mds\" as c\n";
        // Both diag1 (offset 0) and diag2 (offset 2) are on the same line → same
        // computed line span → overlap detected.
        let diag1 = make_diag("duplicate-import", 0, "@import".len());
        let diag2 = make_diag("duplicate-import", 2, "@import".len());

        let result = make_result(vec![diag1, diag2]);
        let plan = plan_fixes(&result, source);

        // Unconditional: overlap must be detected for same-line edits (AC-F-19).
        assert!(
            plan.overlap_rejected,
            "two edits on the same line must trigger overlap detection"
        );
        assert!(
            plan.edits.is_empty(),
            "rejected plan must have empty edits; got: {:?}",
            plan.edits
        );

        // apply_fixes on a rejected plan must return FixOutcome::Rejected
        // (the reverify closure must never be called in this path).
        let outcome = apply_fixes(source, plan, &result, |_| {
            panic!("reverify must not be called when the batch is overlap-rejected")
        });
        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "apply_fixes on an overlap-rejected plan must return Rejected; got: {outcome:?}"
        );
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

    // ── REL-1: slice-panic guard for non-char-boundary / out-of-range offsets ─

    /// REL-1 regression: a diagnostic with a non-char-boundary offset must NOT
    /// cause a panic. `diag_to_edit` returns `None` (edit skipped).
    ///
    /// `"é"` encodes to 2 bytes (U+00E9 → 0xC3 0xA9). Offset 1 splits the
    /// character — `source[..1]` panics on the pre-fix code;
    /// `source.get(..1)` returns `None` with the fix (applies ADR-001).
    #[test]
    fn rel1_non_char_boundary_offset_does_not_panic() {
        let source = "é\n"; // 3 bytes: 0xC3 0xA9 0x0A
                            // Offset 1 is inside the multibyte 'é' — NOT a char boundary.
        let diag = make_diag("duplicate-import", 1, 1);
        let result = make_result(vec![diag]);

        // Must not panic — edit is skipped (None from diag_to_edit).
        let plan = plan_fixes(&result, source);
        assert!(
            plan.edits.is_empty(),
            "non-char-boundary offset must produce no edit; got: {:?}",
            plan.edits
        );
        assert!(
            !plan.overlap_rejected,
            "no overlap rejection expected when there are no valid edits"
        );
    }

    /// REL-1 regression: a diagnostic with an out-of-range offset must NOT
    /// cause a panic. `diag_to_edit` returns `None` (edit skipped).
    #[test]
    fn rel1_out_of_range_offset_does_not_panic() {
        let source = "hello\n"; // 6 bytes
                                // Offset 100 is beyond the source length.
        let diag = make_diag("duplicate-import", 100, 1);
        let result = make_result(vec![diag]);

        let plan = plan_fixes(&result, source);
        assert!(
            plan.edits.is_empty(),
            "out-of-range offset must produce no edit; got: {:?}",
            plan.edits
        );
        assert!(
            !plan.overlap_rejected,
            "no overlap rejection expected when there are no valid edits"
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

    /// A single non-overlapping `duplicate-import` on line 2 must plan, apply,
    /// pass the (stubbed) reverify, and return `FixOutcome::Fixed`.
    ///
    /// The source has the second `@import` starting at byte 23
    /// (`"@import \"./a.mds\" as a\n"` = 23 bytes), which is a valid char
    /// boundary, so `diag_to_edit` succeeds and the plan is non-empty.
    #[test]
    fn reverify_success_returns_fixed() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\nHello!\n";
        let diag = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![diag]);
        let plan = plan_fixes(&result, source);

        // Preconditions (non-vacuous): the plan MUST have edits.
        assert!(
            !plan.edits.is_empty(),
            "duplicate-import at byte 23 must produce an edit (assertion must not be vacuous)"
        );
        assert!(
            !plan.overlap_rejected,
            "single non-overlapping edit must not be rejected"
        );

        // Reverify callback that succeeds with an empty residual.
        let outcome = apply_fixes(source, plan, &result, |_fixed| {
            Ok(LintResult {
                diagnostics: vec![],
                truncated: false,
                is_standalone: true,
            })
        });

        assert!(
            matches!(outcome, FixOutcome::Fixed { .. }),
            "successful reverify must return Fixed outcome; got: {outcome:?}"
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

    // ── L-FIX-REV1: AC-F-20 output-delta gate ────────────────────────────────

    /// I-13: End-to-end Tier B coverage — `unused-function` on a standalone file.
    ///
    /// This test closes the coverage gap identified in I-13: no test previously
    /// exercised `plan_fixes_with_options(result, source, include_tier_b=true)` through
    /// `apply_fixes` on a REAL Tier B diagnostic with a real reverify closure.
    ///
    /// **Why the refusal path**: `diag_to_edit` removes only the `@define dead():`
    /// opening line, leaving the body (`World!\n`) and `@end` orphaned. The reverify
    /// parse fails (`MdsError::Syntax`) and `apply_fixes` returns `Rejected`. This is
    /// the correct fail-closed behavior documented in the KNOWLEDGE.md "block-spanning
    /// fixes are always refused" gotcha — and exercises the reverify/output-neutrality
    /// gate for Tier B explicitly.
    ///
    /// **Why unused-function, not unused-import**: a standalone file has no `@import`
    /// by definition (`is_standalone = !is_partial_or_extends && imports.is_empty()`),
    /// so `unused-import` cannot fire on a standalone file. `unused-function` fires
    /// when `has_explicit_exports && !exported && !called` — achieved here with an
    /// explicit `@export greet` plus an unexported, uncalled `@define dead():`.
    #[test]
    fn tier_b_unused_function_standalone_apply_is_refused() {
        // Standalone source: no @import, no @extends, has explicit @export.
        // `dead` is unexported and uncalled → fires unused-function (Tier B).
        let source =
            "@define greet():\nHello!\n@end\n@define dead():\nWorld!\n@end\n@export greet\n";

        // Step 1: obtain a real lint result via the public API (not a stub).
        let lint_result = crate::lint_str(source).expect("source should lint without error");
        assert!(
            lint_result
                .diagnostics
                .iter()
                .any(|d| d.rule == "unused-function"),
            "unused-function must fire for `dead`; diagnostics: {:?}",
            lint_result.diagnostics
        );

        // Step 2: plan with Tier B included.
        let plan = plan_fixes_with_options(&lint_result, source, /* include_tier_b= */ true);
        assert!(
            !plan.overlap_rejected,
            "no overlap expected on a single Tier B edit"
        );
        assert!(
            !plan.edits.is_empty(),
            "plan must be non-empty for Tier B unused-function; edits: {:?}",
            plan.edits
        );
        assert!(
            plan.edits.iter().any(|e| e.rule == "unused-function"),
            "edit must target the unused-function rule; edits: {:?}",
            plan.edits
        );

        // Step 3: apply with a real reverify closure.
        // Removing only `@define dead():\n` leaves `World!\n@end` orphaned —
        // the reverify parse fails (Syntax error) → apply_fixes returns Rejected.
        let outcome = apply_fixes(source, plan, &lint_result, crate::lint_str);

        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "Tier B unused-function fix must be Rejected (block-span refusal: orphaned @end); \
             got: {outcome:?}"
        );
    }

    /// L-FIX-REV1: A reverify closure that detects an output delta MUST cause
    /// `apply_fixes` to return `FixOutcome::Rejected`.
    ///
    /// White-box test: we inject a synthetic ByteEdit that removes non-dead content
    /// ("World" from "Hello World!\n"), then pass a reverify closure that compares
    /// compiled outputs. The delta (fixed → "Hello !\n") must cause rejection.
    ///
    /// This verifies the mechanism the CLI relies on: when the reverify closure
    /// returns `Err` due to an output delta, the entire fix batch is refused.
    #[test]
    fn l_fix_rev1_output_delta_causes_rejection() {
        let source = "Hello World!\n";
        // An empty LintResult — no real diagnostics needed for this mechanism test.
        let original_result = LintResult {
            diagnostics: vec![],
            truncated: false,
            is_standalone: true,
        };
        // Synthetic ByteEdit removes " World" (bytes 5-12) — this is NOT a real lint
        // fix; it simulates a hypothetical broken fix that changes compiled output.
        let plan = FixPlan {
            edits: vec![ByteEdit {
                start: 5,
                end: 12,
                rule: "duplicate-import".to_string(),
            }],
            overlap_rejected: false,
            truncated: false,
        };

        // Capture original compiled output as the baseline.
        let original_output = crate::compile_str(source)
            .expect("source should compile cleanly")
            .output;

        let outcome = apply_fixes(source, plan, &original_result, move |fixed| {
            // Simulate the CLI output-delta gate (AC-F-20):
            // lint first, then compare compiled outputs.
            let residual = crate::lint_str(fixed)?;
            let fixed_output = crate::compile_str(fixed)
                .expect("fixed source should still compile")
                .output;
            if fixed_output != original_output {
                return Err(crate::error::MdsError::Io {
                    message: "lint --fix would change compiled output; batch refused".to_string(),
                });
            }
            Ok(residual)
        });

        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "apply_fixes must return Rejected when reverify detects an output delta; got: {outcome:?}"
        );
    }

    // ── apply_fixes_incremental ────────────────────────────────────────────────

    /// INC-1: Empty plan with no overlap → NothingToFix (zero reverify calls).
    #[test]
    fn incremental_nothing_to_fix() {
        let source = "Hello!\n";
        let original = make_result(vec![]);
        let plan = FixPlan {
            edits: vec![],
            overlap_rejected: false,
            truncated: false,
        };
        let outcome = apply_fixes_incremental(source, plan, &original, |_| {
            unreachable!("no calls expected")
        });
        assert!(
            matches!(outcome, FixOutcome::NothingToFix),
            "empty plan must return NothingToFix; got: {outcome:?}"
        );
    }

    /// INC-2: overlap_rejected = true → Rejected immediately, no reverify calls.
    #[test]
    fn incremental_overlap_immediate_reject() {
        let source = "Hello!\n";
        let original = make_result(vec![]);
        let plan = FixPlan {
            edits: vec![],
            overlap_rejected: true,
            truncated: false,
        };
        let outcome = apply_fixes_incremental(source, plan, &original, |_| {
            unreachable!("no calls expected")
        });
        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "overlap must return Rejected; got: {outcome:?}"
        );
    }

    /// INC-3: Batch reverify passes → Fixed in exactly 1 reverify call (no per-edit loop).
    #[test]
    fn incremental_batch_success_single_call() {
        // Two removable lines at known offsets.
        let source = "LineA\nLineB\nKeep!\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "LineA".len()),
            make_diag("duplicate-import", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert!(!plan.overlap_rejected);
        assert_eq!(plan.edits.len(), 2, "both edits must be planned");

        let call_count = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            call_count.set(call_count.get() + 1);
            Ok(make_result(vec![]))
        });

        assert!(
            matches!(outcome, FixOutcome::Fixed { .. }),
            "batch success must return Fixed; got: {outcome:?}"
        );
        assert_eq!(
            call_count.get(),
            1,
            "batch success must use exactly 1 reverify call; got: {}",
            call_count.get()
        );
    }

    /// INC-4: Batch fails, per-edit retry: one edit accepted, one rejected → PartiallyFixed.
    ///
    /// Source: "LineA\nLineB\n" (12 bytes).
    /// edit[0] removes LineA (0..6), edit[1] removes LineB (6..12).
    /// Reverify rejects empty strings → batch ("") fails.
    /// Per-edit right-to-left: edit[1] first → "LineA\n" (passes); edit[0] → "" (fails).
    /// Expected: PartiallyFixed { source: "LineA\n", rejected: [edit[0]] }.
    #[test]
    fn incremental_partial_batch_fail_per_edit_fallback() {
        let source = "LineA\nLineB\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "LineA".len()),
            make_diag("duplicate-import", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert!(!plan.overlap_rejected);
        assert_eq!(plan.edits.len(), 2);

        // Reverify: reject empty results (simulates "can't compile an empty file").
        let call_count = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |fixed| {
            call_count.set(call_count.get() + 1);
            if fixed.trim().is_empty() {
                Err(crate::error::MdsError::Io {
                    message: "empty source rejected".to_string(),
                })
            } else {
                Ok(make_result(vec![]))
            }
        });

        match &outcome {
            FixOutcome::PartiallyFixed {
                source: fixed_src,
                rejected,
                ..
            } => {
                assert_eq!(
                    fixed_src, "LineA\n",
                    "accepted edit (LineB removal) should yield 'LineA\\n'; got: {fixed_src:?}"
                );
                assert_eq!(rejected.len(), 1, "exactly one edit should be rejected");
                assert_eq!(
                    rejected[0].edit.rule, "duplicate-import",
                    "rejected edit must be the LineA removal"
                );
            }
            other => panic!("expected PartiallyFixed; got: {other:?}"),
        }

        // Call count: 1 (batch) + 2 (per-edit for 2 edits) = 3 ≤ edits.len()+1+1
        // (batch fails = 1 call; per-edit = 2 calls; total = 3 = 2+1 = edits.len()+1)
        assert_eq!(
            call_count.get(),
            3,
            "batch(1) + per-edit(2) = 3 calls for 2 edits; got: {}",
            call_count.get()
        );
    }

    /// INC-5: Batch fails, all per-edit retries fail → Rejected.
    #[test]
    fn incremental_all_rejected() {
        let source = "LineA\nLineB\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "LineA".len()),
            make_diag("duplicate-import", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(plan.edits.len(), 2);

        let call_count = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            call_count.set(call_count.get() + 1);
            Err(crate::error::MdsError::Io {
                message: "always-fail".to_string(),
            })
        });

        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "all-rejected must return Rejected; got: {outcome:?}"
        );
        // 1 (batch) + 2 (per-edit) = 3 = edits.len()+1
        assert_eq!(
            call_count.get(),
            3,
            "call count must be edits.len()+1 = 3; got: {}",
            call_count.get()
        );
    }

    /// INC-6: Call count bound — N edits → ≤ N+1 total reverify calls.
    #[test]
    fn incremental_call_count_bounded() {
        // Three-edit source: "A\nB\nC\n" (each line 2 bytes including \n).
        let source = "A\nB\nC\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, 1),
            make_diag("duplicate-import", 2, 1),
            make_diag("duplicate-import", 4, 1),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(plan.edits.len(), 3, "all three edits must be planned");

        let call_count = std::cell::Cell::new(0usize);
        // Batch always fails; per-edit always passes → all accepted.
        let first_call = std::cell::Cell::new(true);
        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            call_count.set(call_count.get() + 1);
            if first_call.get() {
                first_call.set(false);
                Err(crate::error::MdsError::Io {
                    message: "batch-fail".to_string(),
                })
            } else {
                Ok(make_result(vec![]))
            }
        });

        // Batch fails (1 call) + 3 per-edit (3 calls) = 4 = edits.len()+1.
        assert!(
            call_count.get() <= 3 + 1,
            "call count must be ≤ edits.len()+1 = 4; got: {}",
            call_count.get()
        );
        // All per-edit passed → Fixed.
        assert!(
            matches!(outcome, FixOutcome::Fixed { .. }),
            "all edits accepted → Fixed; got: {outcome:?}"
        );
    }

    /// INC-7: Right-to-left accumulation is correct — applying edits right-to-left
    /// preserves lower-offset edit validity after higher-offset edits are accepted.
    #[test]
    fn incremental_right_to_left_accumulation() {
        // Source: "AAAA\nBBBB\nKeep!\n"
        // edit[0]: remove line 0 (AAAA\n, bytes 0..5)
        // edit[1]: remove line 1 (BBBB\n, bytes 5..10)
        // Batch fails; both pass individually.
        // Expected fixed source: "Keep!\n" (both lines removed, right-to-left order maintained).
        let source = "AAAA\nBBBB\nKeep!\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "AAAA".len()),
            make_diag("duplicate-import", "AAAA\n".len(), "BBBB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(plan.edits.len(), 2);

        let first_call = std::cell::Cell::new(true);
        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            if first_call.get() {
                first_call.set(false);
                Err(crate::error::MdsError::Io {
                    message: "batch-fail".to_string(),
                })
            } else {
                Ok(make_result(vec![]))
            }
        });

        match &outcome {
            FixOutcome::Fixed {
                source: fixed_src, ..
            } => {
                assert_eq!(
                    fixed_src, "Keep!\n",
                    "both edits accepted right-to-left must yield 'Keep!\\n'; got: {fixed_src:?}"
                );
            }
            other => panic!("expected Fixed; got: {other:?}"),
        }
    }

    // ── PF-005 regression: sortedness guard ──────────────────────────────────

    /// PF-005 regression: `apply_fixes_incremental` with unsorted edits must return
    /// `FixOutcome::Rejected`, NOT silently corrupt the source.
    ///
    /// **Why this test is critical:** In a RELEASE build (`--release`), the pre-fix code's
    /// `debug_assert!` in `apply_plan_unchecked` is compiled out.  `apply_fixes_incremental`
    /// had no sortedness check at all, so passing unsorted edits would silently apply them
    /// in the wrong order and WRITE the corrupted source TO DISK with no diagnostic.
    ///
    /// After the fix, an unconditional guard in `apply_fixes_incremental` catches unsorted
    /// edits before `apply_plan_unchecked` is reached, returning `Rejected` in both debug
    /// and release builds.
    ///
    /// This test would PANIC against the pre-fix code in debug mode (the `debug_assert!`
    /// in `apply_plan_unchecked` fires) and would produce a WRONG outcome (source silently
    /// corrupted, then reverify might return `Fixed`) in a release build.  After the fix,
    /// it returns `Rejected` in all build modes.
    #[test]
    fn pf005_unsorted_edits_rejected_in_incremental() {
        let source = "LineA\nLineB\n";
        let original = make_result(vec![]);
        // Manually construct a plan with DESCENDING offsets — this violates the ascending-sort
        // invariant required for correct right-to-left application.
        // edit[0]: LineB at offset 6 (higher) listed FIRST — wrong order.
        // edit[1]: LineA at offset 0 (lower) listed SECOND — wrong order.
        let plan = FixPlan {
            edits: vec![
                ByteEdit {
                    start: 6,
                    end: 12,
                    rule: "duplicate-import".to_string(),
                },
                ByteEdit {
                    start: 0,
                    end: 6,
                    rule: "duplicate-import".to_string(),
                },
            ],
            overlap_rejected: false,
            truncated: false,
        };
        let outcome = apply_fixes_incremental(source, plan, &original, |_| Ok(make_result(vec![])));
        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "unsorted edits must be rejected, not silently applied; got: {outcome:?}"
        );
    }

    /// PF-005 regression: `apply_fixes` with unsorted edits must return
    /// `FixOutcome::Rejected`, NOT silently corrupt the source.
    #[test]
    fn pf005_unsorted_edits_rejected_in_apply_fixes() {
        let source = "LineA\nLineB\n";
        let original = make_result(vec![]);
        let plan = FixPlan {
            edits: vec![
                ByteEdit {
                    start: 6,
                    end: 12,
                    rule: "duplicate-import".to_string(),
                },
                ByteEdit {
                    start: 0,
                    end: 6,
                    rule: "duplicate-import".to_string(),
                },
            ],
            overlap_rejected: false,
            truncated: false,
        };
        let outcome = apply_fixes(source, plan, &original, |_| {
            unreachable!("reverify must not be called when edits are unsorted")
        });
        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "unsorted edits must be rejected in apply_fixes; got: {outcome:?}"
        );
    }

    // ── INC-8: FALLBACK_MAX_EDITS cap (PF-004) ──────────────────────────────

    /// INC-8: A plan with more than `FALLBACK_MAX_EDITS` edits returns `Rejected`
    /// fail-closed when the batch fails, using only 1 reverify call (batch only).
    /// This prevents the O(N×resolves) per-edit fallback on large plans (avoids PF-004).
    #[test]
    fn fallback_max_edits_cap_rejects_large_plan() {
        // Build a plan with exactly FALLBACK_MAX_EDITS + 1 edits (one over the cap).
        let lines: Vec<String> = (0..=FALLBACK_MAX_EDITS)
            .map(|i| format!("L{i}\n"))
            .collect();
        let source = lines.concat();
        let mut offset = 0usize;
        let mut diags = Vec::new();
        for line in &lines {
            diags.push(make_diag("duplicate-import", offset, 1));
            offset += line.len();
        }
        let original = make_result(diags);
        let plan = plan_fixes_with_options(&original, &source, false);
        assert_eq!(
            plan.edits.len(),
            FALLBACK_MAX_EDITS + 1,
            "plan must have FALLBACK_MAX_EDITS+1 edits for this test to be valid"
        );

        // Batch always fails (simulates block-spanning Tier A edit defeating the batch).
        let call_count = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(&source, plan, &original, |_fixed| {
            call_count.set(call_count.get() + 1);
            Err(crate::error::MdsError::Io {
                message: "batch-fail".to_string(),
            })
        });

        // Must be Rejected (cap exceeded) — no per-edit retries.
        assert!(
            matches!(outcome, FixOutcome::Rejected { .. }),
            "plan exceeding FALLBACK_MAX_EDITS must be Rejected fail-closed; got: {outcome:?}"
        );
        // Only 1 reverify call (batch attempt), not N+1.
        assert_eq!(
            call_count.get(),
            1,
            "only the batch reverify call must be made when cap is exceeded; got: {}",
            call_count.get()
        );
    }

    // ── #7 regression: real rejection reasons surfaced (ADR-004) ────────────

    /// #7 regression: when all per-edit retries are rejected, the `reason` in
    /// `FixOutcome::Rejected` must include the actual reverify failure messages,
    /// not the former fixed string "All fix edits were rejected…".
    ///
    /// Applies ADR-004: a three-tier safety gate is only as useful as its refusal
    /// reporting — surfacing the real rejection reason is diagnostic infrastructure
    /// for the whole `--fix` feature.
    #[test]
    fn rejected_reason_includes_per_edit_failure_details() {
        let source = "LineA\n";
        let original = make_result(vec![make_diag("duplicate-import", 0, "LineA".len())]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(
            plan.edits.len(),
            1,
            "must have exactly 1 edit for this test"
        );

        // Reverify always fails with a distinctive message.
        let outcome = apply_fixes_incremental(source, plan, &original, |_| {
            Err(crate::error::MdsError::Io {
                message: "distinctive-rejection-message".to_string(),
            })
        });

        match &outcome {
            FixOutcome::Rejected { reason, .. } => {
                assert!(
                    reason.contains("distinctive-rejection-message"),
                    "rejection reason must include the actual per-edit failure message \
                     (applies ADR-004); got reason: {reason:?}"
                );
            }
            other => panic!("expected Rejected; got: {other:?}"),
        }
    }
}
