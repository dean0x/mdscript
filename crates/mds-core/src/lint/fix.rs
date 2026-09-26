//! Lint `--fix` planner: tiered fix generation, overlap detection, and reverify gate.
//!
//! ## Tier contract (T5 / AC-F-18)
//!
//! | Tier | Rules                                                     | Semantics |
//! |------|------------------------------------------------------------|-----------|
//! | A    | duplicate-import, duplicate-export, unreachable-branch,    | Auto-fixable (span-removal); gated by reverify |
//! |      | empty-block, legacy-interpolation                          |           |
//! | B    | unused-import, unused-function                             | Fixable only when structural-standalone (no imports/extends/partial; reverify applies) |
//! | C    | unused-variable, redundant-else, shadow-variable           | Report-only; never fixed |
//!
//! **Tier B nuance:**
//! - `unused-function` sets `fix_removals` to a whole-block [`FixLineSpan`] and is
//!   applied when `include_tier_b = true` (structural-standalone file). The reverify
//!   gate still runs; if removing the function would cause a compile error the edit
//!   is rejected.
//! - `unused-import` leaves `fix_removals: None` (partial-name removal from an import
//!   list is structurally ambiguous and unsafe). The rule is Tier B so it appears under
//!   `--fix --standalone` output, but no edit is ever emitted (report-only in practice).
//!
//! ## Terminology (spec §7.5)
//!
//! - **Structural-standalone**: a file with no `@import`, `@extends`, or use as a
//!   partial target. This property gates Tier B `--fix`. A file that triggers
//!   `unused-import` is, by definition, not structural-standalone.
//! - **Compile-clean**: a file that compiles without any runtime `--vars`. This
//!   property gates the output-equality reverify for Tier B fixes: removing an
//!   unused import or function must produce byte-identical compiled output.
//!
//! ## Block-span fixes (FixLineSpan)
//!
//! Block-spanning rules (`empty-block`, `unreachable-branch`, `unused-function`)
//! express their fix plan as a `Vec<FixLineSpan>` stored in
//! [`LintDiagnostic::fix_removals`] rather than deriving edits from the diagnostic
//! span alone. Each [`FixLineSpan`] encodes which lines to remove:
//!
//! - `to_inclusive: true`  — remove lines from `from` through (and including) `to`
//! - `to_inclusive: false` — remove lines from `from` up to (not including) `to`
//!
//! This lets a single diagnostic drive removal of the complete block plus its closing
//! `@end` without the planner needing to understand block structure.
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
//! See [`extend_to_line_end`].
//!
//! ## fix.rs is pure (no I/O)
//!
//! File I/O and atomic writes are the CLI's responsibility. `fix.rs` operates
//! entirely on in-memory byte slices. The caller owns the file read and write.
//!
//! ## Containment deduplication (AC-F-26)
//!
//! Before overlap detection, `dedup_contained_or_identical` removes any edit
//! whose byte range is fully contained within an earlier retained edit. This handles
//! the common case where two rules fire on the same block (e.g. `unreachable-branch`
//! and `empty-block` both targeting the same `@if`): the wider edit subsumes the
//! narrower one and a single correct removal is applied.
//!
//! ## Overlap detection (AC-F-19)
//!
//! After containment deduplication, any remaining pair of edits that partially
//! overlap (neither contains the other) is a genuine conflict. In that case the
//! ENTIRE batch is abandoned fail-closed — no partial write.
//!
//! ## Reverify gate (AC-F-20)
//!
//! [`apply_fixes_incremental`] first applies every non-overlapping edit
//! right-to-left in one pass and hands the fixed source to the caller's reverify
//! callback. If that batch is refused, it retries each edit on its own,
//! right-to-left, keeping the edits that pass; the retry is skipped (and the plan
//! refused) above [`FALLBACK_MAX_EDITS`] edits. A candidate is REFUSED in two ways:
//! - by the reverify callback, which returns `Err` — the CLI's does so when the
//!   candidate no longer compiles, and, when every edit in the plan is
//!   output-neutral ([`is_output_neutral`] — every fixable rule except
//!   `legacy-interpolation`, in Tier A and Tier B alike) and the original
//!   compiled, on any compiled-output delta;
//! - by the gate itself, when the callback's lint result has a finding the edit
//!   introduced: a rule the plan does not target with more findings than in the
//!   original lint result.
//!
//! ## Idempotence note (AC-F-25)
//!
//! When `LintResult::truncated` is true, applying fixes and re-running lint may
//! surface previously-suppressed diagnostics. The idempotence guarantee holds only
//! for non-truncated results.

use crate::error::MdsError;
use crate::lint::diagnostic::{
    sanitize_control_chars_wire, FixLineSpan, LintDiagnostic, LintResult, Severity,
};

// Tier classification lives in the leaf `tier` module to break the would-be
// circular dependency (fix.rs → diagnostic.rs → fix.rs). Re-export here so
// the public API surface at `mds::fix::FixTier` etc. is unchanged.
pub use super::tier::{is_fixable, is_output_neutral, rule_tier, FixTier};

// ── Fix plan ─────────────────────────────────────────────────────────────────

/// A single byte-range edit on the source string.
///
/// The edit replaces the bytes in `[start, end)` with `replacement`. When
/// `replacement` is empty, this is equivalent to a pure deletion. The
/// `end` is exclusive; the range must not exceed the source length.
///
/// **CRLF note**: `end` must be chosen to include the complete line terminator
/// (call [`extend_to_line_end`] to adjust if needed) for line-removal edits.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Construct via [`ByteEdit::deletion`] or [`ByteEdit::replacement`]; do not use a struct literal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteEdit {
    /// Inclusive start byte offset of the range to replace.
    pub start: usize,
    /// Exclusive end byte offset of the range to replace.
    pub end: usize,
    /// Rule that generated this edit (for audit/logging).
    pub rule: String,
    /// Replacement text. Empty string means pure deletion (line removal).
    pub replacement: String,
}

impl ByteEdit {
    /// Construct a `ByteEdit` that **deletes** the byte range `[start, end)`.
    ///
    /// Equivalent to a replacement with an empty string.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::fix::ByteEdit;
    /// let edit = ByteEdit::deletion(0, 6, "duplicate-import");
    /// assert_eq!(edit.replacement, "");
    /// ```
    #[must_use]
    pub fn deletion(start: usize, end: usize, rule: impl Into<String>) -> Self {
        ByteEdit {
            start,
            end,
            rule: rule.into(),
            replacement: String::new(),
        }
    }

    /// Construct a `ByteEdit` that **replaces** the byte range `[start, end)` with `text`.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::fix::ByteEdit;
    /// let edit = ByteEdit::replacement(6, 12, "legacy-interpolation", "{{name}}");
    /// assert_eq!(edit.replacement, "{{name}}");
    /// ```
    #[must_use]
    pub fn replacement(
        start: usize,
        end: usize,
        rule: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        ByteEdit {
            start,
            end,
            rule: rule.into(),
            replacement: text.into(),
        }
    }
}

/// A fix edit that was rejected by the per-edit reverify gate in [`apply_fixes_incremental`].
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Construct via [`RejectedEdit::new`]; do not use a struct literal.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct RejectedEdit {
    /// The edit that was rejected.
    pub edit: ByteEdit,
    /// Human-readable reason for rejection. Sanitized at construction — see
    /// [`FixOutcome::Rejected::reason`].
    pub reason: String,
}

impl RejectedEdit {
    /// Construct a `RejectedEdit` with an edit and a rejection reason.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    #[must_use]
    pub fn new(edit: ByteEdit, reason: impl Into<String>) -> Self {
        RejectedEdit {
            edit,
            reason: reason.into(),
        }
    }
}

/// Render a reverify failure into a single-line, display-safe rejection reason.
///
/// The single construction site for every rejection reason that embeds an
/// [`MdsError`]. `MdsError`'s `Display` is deliberately raw (see its "Display contract"
/// note): variants such as `syntax error: {message}`, `file not found: {path}` and
/// `circular import detected: {cycle}` interpolate untrusted template and filesystem
/// text verbatim. Interpolating that directly into `reason` would push raw control
/// bytes into a field whose consumers print it on a status line.
///
/// WIRE mode (not HUMAN) is correct here for the same reason it is correct for a
/// filename: the CLI prints this value as `fix rejected: {reason}` — one unframed,
/// unindented status line. A raw `\n` in the reason would let a hostile template forge
/// a second line indistinguishable from genuine status output (CWE-117). Every
/// `MdsError` `Display` variant is single-line by construction, so escaping `\n` here
/// discards nothing legitimate.
///
/// Sanitizing at construction rather than at each print site is deliberate: `reason` is
/// a public field of a public enum in a published crate, so there is no bound on the
/// number of print sites, and a per-site check is exactly the parallel-path pattern
/// that lapses (PF-004).
fn reverify_failure_reason(err: &MdsError) -> String {
    format!(
        "could not verify fix — the edited source did not re-parse cleanly ({}); \
         leaving the file unchanged",
        sanitize_control_chars_wire(&err.to_string())
    )
}

/// A plan of fix edits for a single file's source.
///
/// Obtain via [`plan_fixes`] or [`plan_fixes_with_options`], then pass to
/// [`apply_fixes_incremental`]. External crates that need an empty plan can
/// use `FixPlan::default()`; its fields are `pub`, so they remain directly
/// readable and writable.
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases;
/// do not use a struct literal in external crates.
#[non_exhaustive]
#[derive(Debug, Default)]
pub struct FixPlan {
    /// Sorted (start ASC, end DESC), deduplicated, non-overlapping byte edits
    /// to apply right-to-left. Populated by [`plan_fixes`] / [`plan_fixes_with_options`].
    pub edits: Vec<ByteEdit>,
    /// `true` when the batch was cleared because a genuine partial overlap survived
    /// containment deduplication (see module-level "Overlap detection" note).
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
///
/// **Warning:** a bare `_ => {}` arm silently swallows
/// [`FixOutcome::PartiallyFixed`] (new in v0.4.0) without any compiler signal.
/// Match `PartiallyFixed` explicitly if partial results matter.
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
    ///
    /// **New in v0.4.0.** A `_ => {}` wildcard arm silently discards partial results —
    /// there is no compiler signal when the arm matches. Match this variant explicitly
    /// if partial results matter.
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
        /// The original (unchanged) source. Raw — this is the file's bytes, not prose.
        source: String,
        /// Human-readable reason for rejection.
        ///
        /// **Display-safe by construction.** Every untrusted fragment interpolated into
        /// this string (currently only an [`MdsError`]'s raw `Display`) is escaped with
        /// WIRE-mode [`sanitize_control_chars_wire`][crate::sanitize_control_chars_wire]
        /// before it is stored, so the value is always single-line and free of C0/DEL/C1
        /// control bytes and bidi controls. Callers may print it directly on a status
        /// line without further escaping. See `reverify_failure_reason`.
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

        plan.edits.extend(diag_to_edits(diag, source));
    }

    // Sort by start position ascending, then by end position descending.
    // The secondary end-DESC key ensures that among edits sharing the same start byte,
    // the widest (most encompassing) edit comes first — a precondition for
    // `dedup_contained_or_identical` to work correctly in a single linear pass.
    plan.edits
        .sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));

    // Containment and identical-range deduplication: drop any edit whose byte range is
    // fully covered by an earlier retained edit (see `dedup_contained_or_identical`).
    // This resolves false-overlap rejections that occur when two rules each emit spans
    // for the same region (e.g. unreachable-branch + empty-block on the same @if block).
    dedup_contained_or_identical(&mut plan.edits);

    // Overlap detection: reject the entire batch if any pair partially overlaps.
    // Containment has already been resolved above; any remaining overlap is a genuine
    // partial overlap that cannot be applied safely.
    if has_overlapping_edits(&plan.edits) {
        plan.overlap_rejected = true;
        plan.edits.clear();
    }

    plan
}

/// Find the byte offset of the start of the line containing `offset`.
///
/// Uses `str::get(..offset)` — returns `None` for out-of-range or
/// non-char-boundary offsets (fail-closed per ADR-001; no panic).
fn line_start(source: &str, offset: usize) -> Option<usize> {
    let prefix = source.get(..offset)?;
    Some(prefix.rfind('\n').map(|p| p + 1).unwrap_or(0))
}

/// Convert a `FixLineSpan` to a `ByteEdit`, or `None` when the span is invalid.
///
/// - `to_inclusive: true`  → remove `[line_start(from) .. extend_to_line_end(to))`
/// - `to_inclusive: false` → remove `[line_start(from) .. line_start(to))`
///
/// Fail-closed per ADR-001: any span that resolves to a zero-length or
/// out-of-range byte range produces `None` (edit silently skipped).
fn fix_line_span_to_edit(span: &FixLineSpan, source: &str, rule: &str) -> Option<ByteEdit> {
    let start = line_start(source, span.from)?;
    let end = if span.to_inclusive {
        extend_to_line_end(source, span.to)
    } else {
        line_start(source, span.to)?
    };

    if start >= end || end > source.len() {
        return None;
    }

    Some(ByteEdit {
        start,
        end,
        rule: rule.to_string(),
        replacement: String::new(), // line-removal: empty replacement
    })
}

/// Convert a diagnostic's `fix_removals` and `fix_edits` into zero or more `ByteEdit`s.
///
/// Returns an empty `Vec` when both `fix_removals` and `fix_edits` are `None`
/// (no-fix case), or when every span resolves to an invalid range (fail-closed per ADR-001).
fn diag_to_edits(diag: &LintDiagnostic, source: &str) -> Vec<ByteEdit> {
    let mut edits = Vec::new();

    // fix_removals path (line removals)
    if let Some(removals) = &diag.fix_removals {
        edits.extend(
            removals
                .iter()
                .filter_map(|span| fix_line_span_to_edit(span, source, &diag.rule)),
        );
    }

    // fix_edits path (replacement edits)
    if let Some(text_edits) = &diag.fix_edits {
        for edit in text_edits {
            // Validate bounds: fail-closed per ADR-001 (silently skip invalid ranges)
            if edit.start <= edit.end
                && edit.end <= source.len()
                && source.is_char_boundary(edit.start)
                && source.is_char_boundary(edit.end)
            {
                edits.push(ByteEdit {
                    start: edit.start,
                    end: edit.end,
                    rule: diag.rule.clone(),
                    replacement: edit.new_text.clone(),
                });
            }
        }
    }

    edits
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

/// Remove edits that are fully contained within (or byte-identical to) an earlier retained edit.
///
/// **Precondition**: `edits` must be sorted by `(start ASC, end DESC)`.  Under that ordering,
/// among edits sharing the same start byte the widest (largest `end`) appears first, so the
/// linear scan correctly identifies all contained edits without look-ahead.
///
/// The containment check uses a single `max_end` tracker: because starts are non-decreasing,
/// an edit is contained in some prior retained edit iff its `end ≤ max_end`.
///
/// ## What this resolves
///
/// Two different rules can legitimately fire on the same AST node and each emit a
/// `FixLineSpan` that covers some or all of the same byte range. For example,
/// `unreachable-branch` (case A) emits a span covering the dead `@else:..@end` region,
/// while `empty-block` emits a shorter span covering just the empty `@else:` clause — the
/// shorter span is fully contained in the longer one.  Without deduplication, both edits
/// reach the overlap detector and trigger `overlap_rejected`; with deduplication the shorter
/// edit is dropped and the longer (dominant) edit is applied, which is correct.
///
/// Note: partial overlaps (where neither range contains the other) are NOT resolved here;
/// they are intentionally left for the overlap detector to catch and reject fail-closed.
fn dedup_contained_or_identical(edits: &mut Vec<ByteEdit>) {
    if edits.len() < 2 {
        return;
    }
    let mut write = 0;
    let mut max_end = edits[0].end;
    for read in 1..edits.len() {
        if edits[read].end > max_end {
            // This edit extends beyond every previously retained edit — keep it.
            write += 1;
            max_end = edits[read].end;
            edits.swap(write, read);
        }
        // else: edits[read].end ≤ max_end → fully contained within a retained edit — skip.
    }
    edits.truncate(write + 1);
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
/// # `_unchecked` suffix — ADR-004
///
/// This function bypasses the ADR-004 reverify gate — it applies edits without
/// recompiling or verifying that the fixed source produces identical compiled
/// output. Production code that writes back to disk **must** use
/// [`apply_fixes_incremental`] instead, which gates on the reverify callback
/// before returning `FixOutcome::Fixed` or `FixOutcome::PartiallyFixed`.
/// `apply_plan_unchecked` is provided for the `--fix --diff` / `--fix --check`
/// diff-preview path (which computes the delta without writing it) and for unit
/// tests. Calling it on a write path without a subsequent reverify is an
/// anti-pattern — the reverify gate is the only guard against a fix that
/// accidentally changes compiled semantics.
///
/// The caller must pass `plan` with `overlap_rejected == false`; if true,
/// calling this function is a logic error (use [`apply_fixes_incremental`] which
/// checks this).
///
/// # Panics
///
/// - In every build, when `plan.edits` is not sorted ascending by `start` — an
///   unconditional `assert!`, because applying unsorted edits right-to-left would
///   corrupt the source.
/// - In every build, when an edit within bounds has a `start` or `end` that is not
///   on a UTF-8 character boundary — `String::replace_range` panics (in debug builds
///   a `debug_assert!` fails first, naming the offset).
/// - In debug builds only, when `plan.overlap_rejected` is true, or when an edit is
///   out of bounds (`end` past the source length, or `start > end`). A release build
///   does not check the flag and applies the plan's edits as given, and it skips an
///   out-of-bounds edit, leaving the source unchanged there.
///
/// [`plan_fixes`]/[`plan_fixes_with_options`] sort their edits, so a plan they
/// return never trips the sortedness `assert!`; one they reject for an overlap has
/// its edits cleared, but still trips the debug-build `overlap_rejected` check.
pub fn apply_plan_unchecked(source: &str, plan: &FixPlan) -> String {
    debug_assert!(
        !plan.overlap_rejected,
        "apply_plan_unchecked called on a rejected (overlapping) plan"
    );

    if plan.edits.is_empty() {
        return source.to_string();
    }

    // `plan.edits` must be sorted ascending by start offset (guaranteed by
    // plan_fixes_with_options which calls `edits.sort()` before returning).
    // Iterate right-to-left with `.rev()` — no clone or re-sort needed.
    //
    // Unconditional assert (not debug_assert) — avoids PF-005: the sortedness
    // precondition for right-to-left accumulation is release-critical; a
    // debug_assert! would be compiled out in release builds, allowing unsorted
    // edits to silently corrupt the source. `apply_fixes_incremental` performs its
    // own fail-closed guard before reaching here; this assert is defense-in-depth
    // for direct external callers who bypass that guard.
    assert!(
        plan.edits.windows(2).all(|w| w[0].start <= w[1].start),
        "apply_plan_unchecked: edits must be sorted ascending by start offset (avoids PF-005)"
    );

    // Apply right-to-left: earlier byte offsets remain valid as higher-offset
    // edits are applied first.
    let mut result = source.to_string();
    for edit in plan.edits.iter().rev() {
        splice_edit(&mut result, edit);
    }

    result
}

/// Apply one edit to `buf` in place: the loop body of [`apply_plan_unchecked`], shared
/// with the per-edit fallback of [`apply_fixes_incremental`] so both paths treat an
/// out-of-bounds edit identically (skipped, with a `debug_assert` in debug builds).
///
/// `replace_range` handles pure deletions (empty replacement) and text replacements
/// uniformly. `buf` is valid UTF-8 and edits must operate on char boundaries;
/// `replace_range` on char-boundary offsets with a valid-UTF-8 replacement preserves UTF-8.
fn splice_edit(buf: &mut String, edit: &ByteEdit) {
    let (start, end) = (edit.start, edit.end);
    if end > buf.len() || start > end {
        debug_assert!(
            false,
            "fix edit out of bounds: start={start} end={end} len={}",
            buf.len()
        );
        return;
    }
    debug_assert!(
        buf.is_char_boundary(start),
        "fix edit start={start} is not a char boundary"
    );
    debug_assert!(
        buf.is_char_boundary(end),
        "fix edit end={end} is not a char boundary"
    );
    buf.replace_range(start..end, &edit.replacement);
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
/// # Reverify contract
///
/// `reverify` is called with a candidate source and returns its lint result (`Ok`, possibly
/// empty) or `Err` when the candidate fails the caller's check — the CLI refuses on a compile
/// failure and, for output-neutral rules, on any compiled-output delta (AC-F-20). It is `F: Fn`
/// because it may be called up to `plan.edits.len() + 1` times.
///
/// `original` is the lint result the plan was built from; it is the baseline of findings that
/// already existed before any fix. A pre-existing finding (e.g. a Tier C `unused-variable` beside
/// a fixable `duplicate-import`) may survive into the residual without refusing the fix
/// (AC-F-23). A candidate is refused only when some rule the plan does not target has MORE
/// findings in its residual than in `original` — a new problem the edit introduced.
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
    if let Some(refused) = precheck(source, &plan) {
        return refused;
    }
    let gate = RegressionGate::new(&plan.edits, original);

    // ── Batch attempt (one reverify call) ─────────────────────────────────────
    // A refused batch falls through to the per-edit retry, its verdict unrendered.
    let batch_source = apply_plan_unchecked(source, &plan);
    if let Verdict::Accepted(residual) = gate.verify(&reverify, &batch_source) {
        return FixOutcome::Fixed {
            source: batch_source,
            residual,
        };
    }

    // ── Per-edit fallback (≤ edits.len() more reverify calls, capped — PF-004) ──
    if plan.edits.len() > FALLBACK_MAX_EDITS {
        return fallback_cap_rejected(source, plan.edits.len());
    }
    apply_per_edit(source, plan.edits, &gate, &reverify)
}

// ── apply_fixes_incremental helpers ───────────────────────────────────────────

/// The structural fail-closed checks that run before any edit is applied or reverified.
///
/// Returns the outcome to report when the plan cannot be applied at all, or `None` to
/// proceed. The ORDER matters:
/// 1. Overlap first — `plan_fixes` clears the edits when it detects an overlap, so testing
///    emptiness first would report an overlapping batch as `NothingToFix`. Per-edit retry
///    cannot rescue an overlap batch, so it is refused fail-closed.
/// 2. Empty plan — nothing to do.
/// 3. Sortedness (avoids PF-005) — edits must be sorted ascending by start offset for the
///    right-to-left application to be correct. A `debug_assert!`-only guard would be
///    compiled out in release builds, where unsorted edits silently corrupt the source that
///    is written to disk. This unconditional check refuses them before
///    `apply_plan_unchecked` (whose own `assert!` would panic) is reached.
fn precheck(source: &str, plan: &FixPlan) -> Option<FixOutcome> {
    if plan.overlap_rejected {
        return Some(FixOutcome::Rejected {
            source: source.to_string(),
            reason: "Overlapping fix spans detected — batch rejected to avoid data corruption."
                .to_string(),
        });
    }
    if plan.edits.is_empty() {
        return Some(FixOutcome::NothingToFix);
    }
    if plan.edits.windows(2).any(|w| w[0].start > w[1].start) {
        return Some(FixOutcome::Rejected {
            source: source.to_string(),
            reason: "Fix edits are not sorted ascending by start offset; refusing to apply \
                     to prevent source corruption (avoids PF-005)."
                .to_string(),
        });
    }
    None
}

/// The ADR-004 regression gate for one plan, built once and applied to every candidate
/// source — the batch and each per-edit retry alike.
///
/// Every candidate is judged against the FULL set of rules the plan targets, never just the
/// rule of the edit being retried. The comparison must be symmetric with the baseline: while
/// one edit is retried, the other targeted rules' diagnostics are still present (not yet
/// fixed) and must not count as regressions. A single-rule set would produce exactly that
/// false positive (e.g. `empty-block` + `duplicate-export` both targeted → retrying the
/// `duplicate-export` edit alone leaves `empty-block` in the residual, which would look like a
/// new finding against a baseline that excluded it).
struct RegressionGate<'a> {
    /// Rules targeted by at least one edit of the plan.
    targeted: std::collections::HashSet<String>,
    /// Per-rule count of the untargeted findings that existed before any fix (AC-F-23).
    baseline: std::collections::HashMap<&'a str, usize>,
}

impl<'a> RegressionGate<'a> {
    fn new(edits: &[ByteEdit], original: &'a LintResult) -> Self {
        let targeted = edits.iter().map(|e| e.rule.clone()).collect();
        let baseline = count_untargeted_per_rule(&original.diagnostics, &targeted);
        RegressionGate { targeted, baseline }
    }

    /// Reverify `candidate` and judge its residual against the baseline.
    fn verify<F>(&self, reverify: &F, candidate: &str) -> Verdict
    where
        F: Fn(&str) -> Result<LintResult, MdsError>,
    {
        match reverify(candidate) {
            Err(err) => Verdict::Reverify(err),
            Ok(residual) => {
                let counts = count_untargeted_per_rule(&residual.diagnostics, &self.targeted);
                let regressed = regressed_rules(&counts, &self.baseline);
                if regressed.is_empty() {
                    Verdict::Accepted(residual)
                } else {
                    Verdict::Regressed(regressed)
                }
            }
        }
    }
}

/// The verdict of one reverify call on a candidate source. A refusal is kept unrendered so a
/// discarded verdict (the refused batch attempt) costs no formatting.
enum Verdict {
    /// The candidate passed; carries its residual lint result.
    Accepted(LintResult),
    /// The reverify callback refused the candidate.
    Reverify(MdsError),
    /// The candidate introduced new findings of these untargeted rules (sorted).
    Regressed(Vec<String>),
}

impl Verdict {
    /// The one place a verdict becomes rejection text. A reverify error goes through
    /// `reverify_failure_reason`, which WIRE-escapes its untrusted `Display` (ADR-008).
    fn into_edit_result(self) -> Result<LintResult, String> {
        match self {
            Verdict::Accepted(residual) => Ok(residual),
            Verdict::Reverify(err) => Err(reverify_failure_reason(&err)),
            Verdict::Regressed(rules) => Err(format!(
                "Reverify produced new untargeted diagnostics: {rules:?}. Edit reverted."
            )),
        }
    }
}

/// Apply `edit` to a copy of `source`: one allocation per candidate, no per-edit plan.
fn apply_one(source: &str, edit: &ByteEdit) -> String {
    let mut candidate = source.to_string();
    splice_edit(&mut candidate, edit);
    candidate
}

/// The per-edit fallback: retry each edit on its own, right-to-left (`.rev()`), against the
/// running (partially fixed) source, keeping the edits the gate accepts.
///
/// Precondition (from [`precheck`]): `edits` is non-empty and sorted ascending by start, so
/// every accepted high-offset edit leaves the byte positions of the lower-offset edits still
/// to come valid. No re-sort. A stored residual is the evidence that an edit was accepted,
/// and the reported residual is the one from the LAST accepted edit.
fn apply_per_edit<F>(
    source: &str,
    edits: Vec<ByteEdit>,
    gate: &RegressionGate<'_>,
    reverify: &F,
) -> FixOutcome
where
    F: Fn(&str) -> Result<LintResult, MdsError>,
{
    let mut running_source = source.to_string();
    let mut last_residual: Option<LintResult> = None;
    let mut rejected: Vec<RejectedEdit> = Vec::new();

    for edit in edits.into_iter().rev() {
        let candidate = apply_one(&running_source, &edit);
        match gate.verify(reverify, &candidate).into_edit_result() {
            Ok(residual) => {
                running_source = candidate;
                last_residual = Some(residual);
            }
            Err(reason) => rejected.push(RejectedEdit::new(edit, reason)),
        }
    }

    match last_residual {
        None => FixOutcome::Rejected {
            source: source.to_string(),
            reason: summarize_rejections(&rejected),
        },
        Some(residual) if rejected.is_empty() => FixOutcome::Fixed {
            source: running_source,
            residual,
        },
        Some(residual) => FixOutcome::PartiallyFixed {
            source: running_source,
            residual,
            rejected,
        },
    }
}

/// The `reason` of a plan whose every per-edit retry was refused (applies ADR-004): the
/// three-tier safety gate is only as useful as its refusal reporting, so the real per-edit
/// reasons are surfaced. One rejection is reported verbatim; several are joined behind an
/// `All {n} fix edits rejected: ` count prefix.
fn summarize_rejections(rejected: &[RejectedEdit]) -> String {
    match rejected {
        [only] => only.reason.clone(),
        all => {
            let reasons = all
                .iter()
                .map(|r| r.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            format!("All {} fix edits rejected: {}", all.len(), reasons)
        }
    }
}

/// Resource cap (PF-004): the per-edit fallback would call reverify up to `edit_count` more
/// times, each ~3 module resolves + 2 disk sweeps (fresh `ModuleCache` per call). For plans
/// above [`FALLBACK_MAX_EDITS`] that is prohibitively expensive, so a refused batch is
/// refused as a whole, fail-closed.
fn fallback_cap_rejected(source: &str, edit_count: usize) -> FixOutcome {
    FixOutcome::Rejected {
        source: source.to_string(),
        reason: format!(
            "Fix plan has {edit_count} edits; per-edit fallback cap is {FALLBACK_MAX_EDITS} — \
             batch was rejected by the reverify gate. Re-run --fix after manually reducing the \
             issue count (avoids PF-004)."
        ),
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
            // fix_removals drives the planner; FixLineSpan::single(offset) covers one line.
            fix_removals: Some(vec![FixLineSpan::single(offset)]),
            fix_edits: None,
        }
    }

    fn make_result(diags: Vec<LintDiagnostic>) -> LintResult {
        LintResult {
            diagnostics: diags,
            truncated: false,
            is_standalone: false,
        }
    }

    // Exact rejection-reason contracts of `apply_fixes_incremental`, written out
    // literally so a drift in the production text fails a test.
    const OVERLAP_REASON: &str =
        "Overlapping fix spans detected \u{2014} batch rejected to avoid data corruption.";
    const SORTEDNESS_REASON: &str = "Fix edits are not sorted ascending by start offset; \
         refusing to apply to prevent source corruption (avoids PF-005).";
    const A5_PREFIX: &str =
        "could not verify fix \u{2014} the edited source did not re-parse cleanly (";
    const A5_SUFFIX: &str = "); leaving the file unchanged";
    const NEW_EMPTY_BLOCK_REASON: &str =
        "Reverify produced new untargeted diagnostics: [\"empty-block\"]. Edit reverted.";

    /// A diagnostic whose message records the source the reverify closure was given,
    /// so a test can tell which reverify call produced a returned residual.
    fn residual_for(rule: &str, candidate: &str) -> LintResult {
        let mut diag = make_diag(rule, 0, 1);
        diag.message = candidate.to_string();
        make_result(vec![diag])
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

    // ── L-FIX-OVL1: Overlap detection and containment dedup ─────────────────

    /// AC-F-19 / A4: two diagnostics that map to the SAME line produce identical
    /// ByteEdits. `dedup_contained_or_identical` removes the duplicate, leaving a
    /// single edit. No overlap is detected and the fix proceeds.
    ///
    /// This is the correct behaviour: removing the same bytes twice is redundant,
    /// not conflicting. The dedup resolves it safely so the fix succeeds.
    #[test]
    fn l_fix_ovl1_identical_same_line_edits_are_deduped_not_rejected() {
        let source =
            "@import \"./a.mds\" as a\n@import \"./b.mds\" as b\n@import \"./a.mds\" as c\n";
        // Both diag1 (offset 0) and diag2 (offset 2) are on the same line → same
        // computed ByteEdit { start: 0, end: 23 }. dedup removes the duplicate.
        let diag1 = make_diag("duplicate-import", 0, "@import".len());
        let diag2 = make_diag("duplicate-import", 2, "@import".len());

        let result = make_result(vec![diag1, diag2]);
        let plan = plan_fixes(&result, source);

        // After containment/identical dedup: one edit remains, no overlap.
        assert!(
            !plan.overlap_rejected,
            "identical same-line edits must be deduped, not rejected; edits: {:?}",
            plan.edits
        );
        assert_eq!(
            plan.edits.len(),
            1,
            "exactly one edit must survive dedup of identical ranges; edits: {:?}",
            plan.edits
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

    // ── A4: containment / identical-range deduplication ──────────────────────

    /// A4-DEDUP-1: Two edits with identical byte ranges are reduced to one.
    /// Previously this caused overlap_rejected = true.
    #[test]
    fn a4_identical_ranges_are_deduped_to_one() {
        // Two diagnostics that produce the exact same line removal: offset 0 on
        // "@import\n". After dedup, only one edit remains → no overlap.
        let source = "@import \"./a.mds\" as a\n";
        let diag_a = LintDiagnostic {
            rule: "duplicate-import".to_string(),
            severity: Severity::Error,
            message: "dup".to_string(),
            help: None,
            span: Some(crate::error::SerializedSpan {
                offset: 0,
                length: 1,
                line: None,
                column: None,
            }),
            file: None,
            fix_removals: Some(vec![FixLineSpan::single(0)]),
            fix_edits: None,
        };
        let diag_b = LintDiagnostic {
            rule: "duplicate-export".to_string(),
            severity: Severity::Error,
            message: "dup".to_string(),
            help: None,
            span: Some(crate::error::SerializedSpan {
                offset: 0,
                length: 1,
                line: None,
                column: None,
            }),
            file: None,
            fix_removals: Some(vec![FixLineSpan::single(0)]),
            fix_edits: None,
        };
        let result = LintResult {
            diagnostics: vec![diag_a, diag_b],
            truncated: false,
            is_standalone: false,
        };
        let plan = plan_fixes(&result, source);
        assert!(
            !plan.overlap_rejected,
            "identical-range edits must be deduped, not rejected; edits: {:?}",
            plan.edits
        );
        assert_eq!(
            plan.edits.len(),
            1,
            "exactly one edit must survive dedup; edits: {:?}",
            plan.edits
        );
    }

    /// A4-DEDUP-2: An edit fully contained within a wider edit is dropped;
    /// the wider edit is applied and no overlap is detected.
    #[test]
    fn a4_contained_edit_is_dropped_wider_edit_kept() {
        // Source: "@if x:\nhello\n@else:\n\n@end\n"
        // (unreachable-branch case A: always-true @if with empty @else)
        // Wide edit: covers @else:\n\n@end\n  (the whole @else..@end block)
        // Narrow edit: covers @else:\n\n      (just @else: and empty body)
        // The narrow edit is contained within the wide edit.
        let source = "@if x:\nhello\n@else:\n\n@end\n";
        // @if line: 7 bytes (0..7)
        // "hello\n": 6 bytes (7..13)
        // "@else:": 6 + \n = 7 bytes (13..20)
        // "\n": 1 byte (20..21)
        // "@end\n": 5 bytes (21..26)
        let wide_edit = LintDiagnostic {
            rule: "unreachable-branch".to_string(),
            severity: Severity::Error,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
            // FixLineSpan { from: 13, to: 21, to_inclusive: true }
            // → start = line_start(13) = 13, end = extend_to_line_end(21) = 26
            fix_removals: Some(vec![FixLineSpan {
                from: 13, // @else: offset
                to: 21,   // @end offset
                to_inclusive: true,
            }]),
            fix_edits: None,
        };
        let narrow_edit = LintDiagnostic {
            rule: "empty-block".to_string(),
            severity: Severity::Warn,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
            // FixLineSpan { from: 13, to: 21, to_inclusive: false }
            // → start = line_start(13) = 13, end = line_start(21) = 21
            fix_removals: Some(vec![FixLineSpan {
                from: 13,
                to: 21,
                to_inclusive: false,
            }]),
            fix_edits: None,
        };
        let result = LintResult {
            diagnostics: vec![wide_edit, narrow_edit],
            truncated: false,
            is_standalone: false,
        };
        let plan = plan_fixes(&result, source);
        assert!(
            !plan.overlap_rejected,
            "contained edit must be deduped, not rejected; edits: {:?}",
            plan.edits
        );
        assert_eq!(
            plan.edits.len(),
            1,
            "exactly one (wide) edit must survive dedup; edits: {:?}",
            plan.edits
        );
        // The surviving edit must be the WIDE one (starts at 13, ends at 26).
        assert_eq!(
            plan.edits[0].start, 13,
            "surviving edit must start at @else:"
        );
        assert_eq!(
            plan.edits[0].end, 26,
            "surviving edit must end after @end newline"
        );
    }

    /// A4-DEDUP-3: A genuine partial overlap (neither range contains the other)
    /// drives the real planner path — `dedup_contained_or_identical` retains both
    /// edits because neither is contained, and `has_overlapping_edits` fires —
    /// producing `overlap_rejected = true`, edits cleared, and `FixOutcome::Rejected`
    /// from `apply_fixes_incremental` with the exact overlap reason and zero reverify calls.
    ///
    /// Math: source = "line0\nline1\nline2\n"
    ///   line0 → bytes [0,  6)
    ///   line1 → bytes [6,  12)
    ///   line2 → bytes [12, 18)
    ///
    /// Edit A: `FixLineSpan { from: 0, to: 6, to_inclusive: true }`
    ///   start = `line_start(0)` = 0
    ///   end   = `extend_to_line_end(6)` = 12  (scans through "line1\n")
    ///   → ByteEdit [0, 12)
    ///
    /// Edit B: `FixLineSpan { from: 6, to: 12, to_inclusive: true }`
    ///   start = `line_start(6)` = 6
    ///   end   = `extend_to_line_end(12)` = 18 (scans through "line2\n")
    ///   → ByteEdit [6, 18)
    ///
    /// After sort (start ASC, end DESC): A first (start=0), then B (start=6).
    /// `dedup_contained_or_identical`: B.end=18 > max_end(12) → B is not contained,
    /// both edits are retained.
    /// `has_overlapping_edits`: A.end=12 > B.start=6 → partial overlap → rejected.
    #[test]
    fn a4_partial_overlap_still_rejected_after_dedup() {
        let source = "line0\nline1\nline2\n";

        // Edit A covers bytes [0, 12): spans line0 (start) through line1 (end inclusive).
        let diag_a = LintDiagnostic {
            rule: "duplicate-import".to_string(),
            severity: Severity::Error,
            message: "a".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: Some(vec![FixLineSpan {
                from: 0, // offset inside line0
                to: 6,   // offset inside line1; extend_to_line_end(6) = 12
                to_inclusive: true,
            }]),
            fix_edits: None,
        };
        // Edit B covers bytes [6, 18): spans line1 (start) through line2 (end inclusive).
        // B.start=6 < A.end=12, B.end=18 > A.end=12 → genuine partial overlap.
        let diag_b = LintDiagnostic {
            rule: "empty-block".to_string(),
            severity: Severity::Warn,
            message: "b".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: Some(vec![FixLineSpan {
                from: 6, // offset inside line1
                to: 12,  // offset inside line2; extend_to_line_end(12) = 18
                to_inclusive: true,
            }]),
            fix_edits: None,
        };
        let result = LintResult {
            diagnostics: vec![diag_a, diag_b],
            truncated: false,
            is_standalone: false,
        };

        // Drive through the real planner: dedup cannot collapse this (B extends beyond A),
        // so has_overlapping_edits fires → overlap_rejected = true, edits cleared.
        let plan = plan_fixes(&result, source);
        assert!(
            plan.overlap_rejected,
            "partial overlap must drive overlap_rejected=true via the real planner; \
             edits after dedup: {:?}",
            plan.edits
        );
        assert!(
            plan.edits.is_empty(),
            "overlap_rejected must clear all edits; got: {:?}",
            plan.edits
        );

        // The overlap is refused fail-closed (ADR-004) before any reverify call.
        let outcome = apply_fixes_incremental(source, plan, &result, |_| {
            unreachable!("reverify must not be called for an overlap-rejected plan")
        });
        match outcome {
            FixOutcome::Rejected {
                source: unchanged,
                reason,
            } => {
                assert_eq!(reason, OVERLAP_REASON);
                assert_eq!(
                    unchanged, source,
                    "a rejected plan returns the source unchanged"
                );
            }
            other => panic!("overlap_rejected plan must surface Rejected; got: {other:?}"),
        }
    }

    // ── Block-span fix tests (FixLineSpan → ByteEdit) ────────────────────────

    /// fix_line_span_to_edit: single-line span (`to_inclusive: true`, from == to)
    /// must produce a ByteEdit that removes the whole line including its '\n'.
    #[test]
    fn block_span_single_line_to_inclusive() {
        // source: "line0\nline1\nline2\n"
        //          0     6     12    18
        let source = "line0\nline1\nline2\n";
        let span = FixLineSpan::single(6); // any offset on "line1"
        let edit = fix_line_span_to_edit(&span, source, "test-rule")
            .expect("valid single-line span should produce an edit");
        assert_eq!(edit.start, 6, "start must be BOL of line1");
        assert_eq!(edit.end, 12, "end must include the trailing \\n of line1");
        assert_eq!(&source[edit.start..edit.end], "line1\n");
    }

    /// fix_line_span_to_edit: multi-line `to_inclusive: true` span must cover
    /// both boundary lines and the lines in between, including the trailing newline
    /// of the `to` line.
    #[test]
    fn block_span_multi_line_to_inclusive() {
        // "open\nbody\nend\n" — remove lines 0..=2 (the whole thing)
        //  0    5    10   14
        let source = "open\nbody\nend\n";
        let span = FixLineSpan {
            from: 0, // first char of "open"
            to: 10,  // first char of "end"
            to_inclusive: true,
        };
        let edit = fix_line_span_to_edit(&span, source, "empty-block")
            .expect("valid multi-line span should produce an edit");
        assert_eq!(edit.start, 0);
        assert_eq!(edit.end, source.len(), "should consume the entire source");
    }

    /// fix_line_span_to_edit: `to_inclusive: false` must remove up to the START
    /// of the `to` line — the `to` line itself is preserved.
    #[test]
    fn block_span_to_exclusive_stops_at_to_line_start() {
        // "header\nbody\nkeep\n"
        //  0      7    12    17
        let source = "header\nbody\nkeep\n";
        let span = FixLineSpan {
            from: 0,
            to: 12, // start of "keep"
            to_inclusive: false,
        };
        let edit = fix_line_span_to_edit(&span, source, "unreachable-branch")
            .expect("valid span should produce an edit");
        assert_eq!(edit.start, 0);
        assert_eq!(
            edit.end, 12,
            "should stop at start of 'keep' line, not consume it"
        );
        assert_eq!(
            &source[edit.end..],
            "keep\n",
            "keep line must be intact after edit"
        );
    }

    /// fix_removals: None produces no edits (report-only diagnostic path).
    #[test]
    fn block_span_none_fix_removals_emits_no_edits() {
        use crate::error::SerializedSpan;
        let diag = LintDiagnostic {
            rule: "unused-import".to_string(),
            severity: Severity::Error,
            message: "unused".to_string(),
            help: None,
            span: Some(SerializedSpan {
                offset: 0,
                length: 5,
                line: None,
                column: None,
            }),
            file: Some("test.mds".to_string()),
            fix_removals: None, // Tier B unused-import: no edit emitted
            fix_edits: None,
        };
        let result = make_result(vec![diag]);
        // plan_fixes_with_options(include_tier_b=true) still produces no edit because
        // fix_removals is None.
        let plan = plan_fixes_with_options(&result, "hello\n", true);
        assert!(
            plan.edits.is_empty(),
            "fix_removals: None must emit no ByteEdit; got: {:?}",
            plan.edits
        );
    }

    /// A diagnostic with two FixLineSpans (e.g. unreachable-branch case A: remove
    /// opening `@if` line + trailing `@else..@end` region) must produce two
    /// independent ByteEdits, both applied to remove the correct byte ranges.
    #[test]
    fn block_span_two_disjoint_spans_produce_two_edits() {
        use crate::error::SerializedSpan;
        // "line0\n@if:\nline2\n@else:\n@end\n"
        //  0     6    11    18     25   30
        let source = "line0\n@if:\nline2\n@else:\n@end\n";
        //             0     6  10  17   23   29
        // Byte offsets:
        //   "line0\n"  → [0, 6)
        //   "@if:\n"   → [6, 11)
        //   "line2\n"  → [11, 17)
        //   "@else:\n" → [17, 24)
        //   "@end\n"   → [24, 29)
        let diag = LintDiagnostic {
            rule: "unreachable-branch".to_string(),
            severity: Severity::Error,
            message: "always-true".to_string(),
            help: None,
            span: Some(SerializedSpan {
                offset: 6,
                length: 4,
                line: None,
                column: None,
            }),
            file: Some("test.mds".to_string()),
            fix_removals: Some(vec![
                // Span 1: remove the opening "@if:" line only
                FixLineSpan::single(6),
                // Span 2: remove from "@else:" through "@end" (inclusive)
                FixLineSpan {
                    from: 17,
                    to: 24,
                    to_inclusive: true,
                },
            ]),
            fix_edits: None,
        };
        let result = make_result(vec![diag]);
        let plan = plan_fixes(&result, source);
        assert!(
            !plan.overlap_rejected,
            "disjoint spans must not trigger overlap rejection"
        );
        assert_eq!(plan.edits.len(), 2, "two disjoint spans → two edits");
        // Edits are sorted by start ASC; first edit = span 1 (BOL of @if: = 6..11)
        assert_eq!(plan.edits[0].start, 6);
        assert_eq!(plan.edits[0].end, 11);
        // Second edit = span 2 (BOL of @else: through end of @end\n = 17..29)
        assert_eq!(plan.edits[1].start, 17);
        assert_eq!(plan.edits[1].end, 29);
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

    /// A5: re-parse failure flows through the A5 path and produces the exact stable
    /// rejection message (`reverify_failure_reason`).
    ///
    /// Pins the stable prefix and suffix around the embedded error so the human-
    /// readable message cannot drift without a test failure:
    ///   "could not verify fix — the edited source did not re-parse cleanly ({err}); leaving the file unchanged"
    ///
    /// A single rejected edit's reason is reported verbatim (no "All 1 …" wrapper), and
    /// the embedded `{err}` is exactly the reverify error's `Display`, confirming that the
    /// real error was propagated rather than a blank placeholder. Reverify runs twice:
    /// the batch attempt, then the one per-edit retry.
    #[test]
    fn l_fix_rev1_a5_rejection_message_pins_stable_prefix_and_suffix() {
        const ERR: &str = "simulated compile failure after fix";
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\n";
        let diag = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![diag]);
        let plan = plan_fixes(&result, source);
        assert_eq!(plan.edits.len(), 1, "non-vacuity: exactly one edit planned");

        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &result, |_fixed| {
            calls.set(calls.get() + 1);
            Err(MdsError::syntax(ERR))
        });

        let reason = match outcome {
            FixOutcome::Rejected { reason, .. } => reason,
            other => panic!("expected Rejected, got: {other:?}"),
        };

        assert!(
            reason.starts_with(A5_PREFIX),
            "A5 reason must start with the stable prefix;\n  reason: {reason:?}\n  prefix: {A5_PREFIX:?}"
        );
        assert!(
            reason.ends_with(A5_SUFFIX),
            "A5 reason must end with the stable suffix;\n  reason: {reason:?}\n  suffix: {A5_SUFFIX:?}"
        );
        let embedded = &reason[A5_PREFIX.len()..reason.len() - A5_SUFFIX.len()];
        assert_eq!(
            embedded,
            MdsError::syntax(ERR).to_string(),
            "A5 reason must embed the reverify error's Display between the stable \
             prefix and suffix; got: {reason:?}"
        );
        assert!(embedded.contains(ERR), "non-vacuity: {embedded:?}");
        assert_eq!(calls.get(), 2, "batch(1) + per-edit(1) reverify calls");
    }

    /// A5 with two edits: when every per-edit retry is rejected, the reasons are joined
    /// behind an `All {n} fix edits rejected: ` count prefix, separated by `"; "`.
    /// Reverify runs three times: the batch attempt, then one retry per edit.
    #[test]
    fn l_fix_rev1_a5_two_edit_rejection_joins_with_count_prefix() {
        const ERR: &str = "simulated compile failure after fix";
        let source = "LineA\nLineB\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "LineA".len()),
            make_diag("duplicate-import", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes(&original, source);
        assert_eq!(plan.edits.len(), 2, "non-vacuity: two edits planned");

        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            calls.set(calls.get() + 1);
            Err(MdsError::syntax(ERR))
        });

        let one = format!("{A5_PREFIX}{}{A5_SUFFIX}", MdsError::syntax(ERR));
        match outcome {
            FixOutcome::Rejected {
                source: unchanged,
                reason,
            } => {
                assert_eq!(reason, format!("All 2 fix edits rejected: {one}; {one}"));
                assert_eq!(
                    unchanged, source,
                    "a rejected plan returns the source unchanged"
                );
            }
            other => panic!("expected Rejected, got: {other:?}"),
        }
        assert_eq!(calls.get(), 3, "batch(1) + per-edit(2) reverify calls");
    }

    // ── T-REASON: rejection reasons are display-safe by construction (#176) ──────
    //
    // `MdsError`'s `Display` is deliberately raw (see its "Display contract" note).
    // Interpolating it into `reason` pushed unescaped control bytes into a field the
    // CLI prints as an unframed status line: `fix rejected: {reason}`.

    /// Build the hostile `MdsError` used by the T-REASON tests.
    ///
    /// `MdsError::Syntax` renders as `syntax error: {message}`, so the message text —
    /// which in production comes from template source — lands verbatim in `Display`.
    /// The vector carries one member of each escape sub-class: a C0 byte (ESC), a
    /// 3-byte bidi control, the 2-byte bidi control that #176 added, and a newline.
    fn hostile_reverify_error() -> MdsError {
        MdsError::syntax(format!(
            "unexpected token{}[2J at{}line{}mark{}Clean: real.mds",
            '\u{1b}', '\u{202E}', '\u{061C}', '\n'
        ))
    }

    /// T-REASON-2 [security-11 / CWE-117 / PF-004 / PF-013 / #176]: the reverify failure
    /// reason that `apply_fixes_incremental` reports for a rejected edit escapes the
    /// embedded `MdsError` Display — the reason is built by the `reverify_failure_reason`
    /// choke-point, so the guarantee cannot hold on one path and lapse on a sibling.
    #[test]
    fn incremental_rejection_reason_escapes_embedded_error_display() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\n";
        let diag = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![diag]);
        let plan = plan_fixes(&result, source);
        assert!(
            !plan.edits.is_empty(),
            "non-vacuity: the plan must have edits or the fallback never runs"
        );

        let outcome =
            apply_fixes_incremental(
                source,
                plan,
                &result,
                |_fixed| Err(hostile_reverify_error()),
            );
        let reason = match outcome {
            FixOutcome::Rejected { reason, .. } => reason,
            other => panic!("expected Rejected, got: {other:?}"),
        };

        assert_reason_is_display_safe(&reason);
    }

    /// T-REASON-3 [CWE-117 / PF-013 / #176]: when several edits are rejected, the joined
    /// `All {n} fix edits rejected: …` reason escapes EVERY embedded error, not only the
    /// first — each hostile codepoint appears escaped once per rejected edit, and the
    /// joined reason is still a single line.
    #[test]
    fn incremental_multi_rejection_reason_escapes_each_error() {
        let source = "LineA\nLineB\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "LineA".len()),
            make_diag("duplicate-import", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes(&original, source);
        assert_eq!(plan.edits.len(), 2, "non-vacuity: two edits planned");

        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            Err(hostile_reverify_error())
        });
        let reason = match outcome {
            FixOutcome::Rejected { reason, .. } => reason,
            other => panic!("expected Rejected, got: {other:?}"),
        };

        assert!(
            reason.starts_with("All 2 fix edits rejected: "),
            "multi-rejection reason must carry the count prefix; got: {reason:?}"
        );
        assert_reason_is_display_safe(&reason);
        for cp in [0x1B_u32, 0x202E, 0x061C, 0x0A] {
            let escaped = format!("\\u{cp:04X}");
            assert_eq!(
                reason.matches(escaped.as_str()).count(),
                2,
                "{escaped} must appear once per rejected edit; got: {reason:?}"
            );
        }
    }

    /// Shared assertions for T-REASON-2/3: negative, positive, and non-vacuity.
    fn assert_reason_is_display_safe(reason: &str) {
        // Non-vacuity: the real error text actually reached the reason, so the escape
        // assertions cannot pass by the error being dropped instead of sanitized.
        assert!(
            reason.contains("syntax error") && reason.contains("unexpected token"),
            "non-vacuity: the embedded error text must be present; got: {reason:?}"
        );
        // Negative: no raw hostile codepoint survives.
        for raw in ['\u{1b}', '\u{202E}', '\u{061C}', '\n'] {
            assert!(
                !reason.contains(raw),
                "raw U+{:04X} must not appear in a rejection reason; got: {reason:?}",
                raw as u32
            );
        }
        // Positive: each is present in its escaped form. The literals are UPPERCASE
        // while the hex in the source vector is lowercase, which proves the byte really
        // decoded and was really escaped rather than passing through as literal text.
        for escaped in ["\\u001B", "\\u202E", "\\u061C", "\\u000A"] {
            assert!(
                reason.contains(escaped),
                "{escaped} must appear in the rejection reason; got: {reason:?}"
            );
        }
        // The reason is printed as one unframed status line: it must be single-line, or
        // a hostile template can forge output indistinguishable from genuine status.
        assert_eq!(
            reason.lines().count(),
            1,
            "a rejection reason must be single-line; got: {reason:?}"
        );
    }

    /// AC-F-23 regression guard: a pre-existing untargeted diagnostic (e.g. a Tier C
    /// `unused-variable` that coexists with a fixable `duplicate-import`) survives the
    /// reverify but must NOT cause the fix to be refused — residual findings are
    /// expected to remain and determine the exit code. The batch attempt accepts it,
    /// so reverify runs exactly once.
    #[test]
    fn reverify_preexisting_untargeted_survives_and_fix_applies() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\nHello!\n";
        let dup = make_diag("duplicate-import", 23, "@import".len()); // Tier A → targeted
        let unused = make_diag("unused-variable", 0, 1); // Tier C → untargeted, pre-existing
        let result = make_result(vec![dup, unused]);
        let plan = plan_fixes(&result, source);
        assert!(!plan.overlap_rejected && !plan.edits.is_empty());

        // The untargeted unused-variable is still present after the fix — same count.
        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &result, |_fixed| {
            calls.set(calls.get() + 1);
            Ok(make_result(vec![make_diag("unused-variable", 0, 1)]))
        });
        match outcome {
            FixOutcome::Fixed { residual, .. } => assert!(
                residual
                    .diagnostics
                    .iter()
                    .any(|d| d.rule == "unused-variable"),
                "the pre-existing finding must survive into the residual; got: {residual:?}"
            ),
            other => panic!(
                "a surviving pre-existing untargeted diagnostic must not refuse the fix; \
                 got: {other:?}"
            ),
        }
        assert_eq!(calls.get(), 1, "the batch attempt alone accepts the fix");
    }

    /// A genuinely NEW untargeted diagnostic introduced by the edit IS a regression
    /// and must refuse the fix. The batch attempt is refused, the one per-edit retry
    /// is refused for the same reason, and that single reason is reported verbatim.
    #[test]
    fn reverify_new_untargeted_diagnostic_is_rejected() {
        let source = "@import \"./a.mds\" as a\n@import \"./a.mds\" as b\nHello!\n";
        let dup = make_diag("duplicate-import", 23, "@import".len());
        let result = make_result(vec![dup]); // no untargeted diagnostics in the baseline
        let plan = plan_fixes(&result, source);
        assert!(!plan.overlap_rejected && !plan.edits.is_empty());

        // Reverify surfaces an empty-block diagnostic that was NOT present before.
        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &result, |_fixed| {
            calls.set(calls.get() + 1);
            Ok(make_result(vec![make_diag("empty-block", 0, 1)]))
        });
        match outcome {
            FixOutcome::Rejected {
                source: unchanged,
                reason,
            } => {
                assert_eq!(reason, NEW_EMPTY_BLOCK_REASON);
                assert_eq!(
                    unchanged, source,
                    "a rejected plan returns the source unchanged"
                );
            }
            other => panic!("a new untargeted diagnostic must refuse the fix; got: {other:?}"),
        }
        assert_eq!(calls.get(), 2, "batch(1) + per-edit(1) reverify calls");
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
    /// `apply_fixes_incremental` on a REAL Tier B diagnostic with a real reverify closure.
    ///
    /// **Why the Fixed path**: `fix_removals` now carries a `FixLineSpan` covering the
    /// whole `@define dead():..@end` block (from `def.offset` to `def.end_offset`,
    /// inclusive). The reverify parses the fixed source cleanly, confirms no output
    /// delta (the dead function was never called so removal is output-neutral), and
    /// the batch attempt returns `Fixed` after one reverify call.
    ///
    /// **Why unused-function, not unused-import**: a standalone file has no `@import`
    /// by definition (`is_standalone = !is_partial_or_extends && imports.is_empty()`),
    /// so `unused-import` cannot fire on a standalone file. `unused-function` fires
    /// when `has_explicit_exports && !exported && !called` — achieved here with an
    /// explicit `@export greet` plus an unexported, uncalled `@define dead():`.
    #[test]
    fn tier_b_unused_function_standalone_apply_succeeds() {
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
        // The block-span fix_removals removes the whole `@define dead():\nWorld!\n@end\n`
        // block. The reverify parses the fixed source and confirms output-neutrality
        // (dead() was never called, so output is unchanged) → Fixed.
        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &lint_result, |fixed| {
            calls.set(calls.get() + 1);
            crate::lint_str(fixed)
        });

        assert!(
            matches!(outcome, FixOutcome::Fixed { .. }),
            "Tier B unused-function fix must succeed with block-span fix_removals; \
             got: {outcome:?}"
        );
        if let FixOutcome::Fixed { source: fixed, .. } = outcome {
            assert!(
                !fixed.contains("@define dead():"),
                "fixed source must not contain the removed @define dead(); got: {fixed:?}"
            );
            assert!(
                fixed.contains("@define greet():"),
                "fixed source must retain the exported @define greet(); got: {fixed:?}"
            );
        }
        assert_eq!(calls.get(), 1, "the batch attempt alone accepts the fix");
    }

    /// L-FIX-REV1: A reverify closure that detects an output delta MUST cause
    /// `apply_fixes_incremental` to return `FixOutcome::Rejected`.
    ///
    /// White-box test: we inject a synthetic ByteEdit that removes non-dead content
    /// ("World" from "Hello World!\n"), then pass a reverify closure that compares
    /// compiled outputs. The delta (fixed → "Hello !\n") must cause rejection.
    ///
    /// This verifies the mechanism the CLI relies on: when the reverify closure
    /// returns `Err` due to an output delta, the batch is refused, the per-edit retry
    /// is refused the same way, and the reason carries the A5 prefix plus the delta.
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
                replacement: String::new(),
            }],
            overlap_rejected: false,
            truncated: false,
        };

        // Capture original compiled output as the baseline.
        let original_output = crate::compile_str(source)
            .expect("source should compile cleanly")
            .output;

        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original_result, |fixed| {
            calls.set(calls.get() + 1);
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

        let reason = match outcome {
            FixOutcome::Rejected { reason, .. } => reason,
            other => panic!(
                "apply_fixes_incremental must return Rejected when reverify detects an \
                 output delta; got: {other:?}"
            ),
        };
        assert!(
            reason.starts_with(A5_PREFIX),
            "an output-delta refusal is reported through the A5 reason; got: {reason:?}"
        );
        assert!(
            reason.contains("would change compiled output"),
            "the reason must carry the output-delta error; got: {reason:?}"
        );
        assert_eq!(calls.get(), 2, "batch(1) + per-edit(1) reverify calls");
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
    ///
    /// The plan's edits are empty (as `plan_fixes` leaves them after an overlap), so
    /// this also pins the check ORDER: overlap is tested before emptiness, or an
    /// overlapping batch would be reported as `NothingToFix`.
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
        match outcome {
            FixOutcome::Rejected { reason, .. } => assert_eq!(reason, OVERLAP_REASON),
            other => panic!("overlap must return Rejected; got: {other:?}"),
        }
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

    /// INC-6: Call count bound — N edits → exactly N+1 total reverify calls when the
    /// batch fails and every per-edit retry passes.
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
        assert_eq!(
            call_count.get(),
            3 + 1,
            "call count must be edits.len()+1 = 4; got: {}",
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

    /// A batch of more than `FALLBACK_MAX_EDITS` edits that passes reverify is `Fixed`
    /// after the one batch call: the cap bounds only the per-edit fallback.
    #[test]
    fn incremental_batch_success_above_cap_is_fixed() {
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
            "one edit over the cap"
        );

        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(&source, plan, &original, |_fixed| {
            calls.set(calls.get() + 1);
            Ok(make_result(vec![]))
        });

        match outcome {
            FixOutcome::Fixed { source: fixed, .. } => {
                assert_eq!(fixed, "", "every line was removed by the batch")
            }
            other => panic!("a passing batch above the cap must be Fixed; got: {other:?}"),
        }
        assert_eq!(calls.get(), 1, "the batch attempt alone accepts the fix");
    }

    /// Every per-edit retry is judged against the FULL targeted set of the plan, not
    /// the one rule of the edit being retried. Here two targeted rules each keep a
    /// diagnostic in every per-edit residual (the other edit is not applied yet); a
    /// gate built from the single retried rule would count the other rule's diagnostic
    /// as a new untargeted finding and refuse both edits.
    #[test]
    fn incremental_per_edit_uses_full_targeted_set() {
        let source = "LineA\nLineB\nKeep!\n";
        let original = make_result(vec![
            make_diag("duplicate-export", 0, "LineA".len()),
            make_diag("empty-block", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(plan.edits.len(), 2, "non-vacuity: one edit per rule");

        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |_fixed| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                return Err(MdsError::syntax("batch-fail"));
            }
            Ok(make_result(vec![
                make_diag("duplicate-export", 0, 1),
                make_diag("empty-block", 0, 1),
            ]))
        });

        match outcome {
            FixOutcome::Fixed { source: fixed, .. } => assert_eq!(fixed, "Keep!\n"),
            other => panic!("both per-edit retries must be accepted; got: {other:?}"),
        }
        assert_eq!(calls.get(), 3, "batch(1) + per-edit(2) reverify calls");
    }

    /// When the per-edit fallback accepts several edits, the reported residual is the
    /// one from the LAST accepted reverify call — the fully fixed source.
    #[test]
    fn incremental_residual_is_from_last_accepted_edit() {
        let source = "AAAA\nBBBB\nKeep!\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "AAAA".len()),
            make_diag("duplicate-import", "AAAA\n".len(), "BBBB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(plan.edits.len(), 2);

        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |fixed| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                return Err(MdsError::syntax("batch-fail"));
            }
            Ok(residual_for("duplicate-import", fixed))
        });

        match outcome {
            FixOutcome::Fixed {
                source: fixed,
                residual,
            } => {
                assert_eq!(fixed, "Keep!\n");
                assert_eq!(
                    residual.diagnostics[0].message, "Keep!\n",
                    "the residual must come from the last accepted candidate"
                );
            }
            other => panic!("expected Fixed; got: {other:?}"),
        }
        assert_eq!(calls.get(), 3, "batch(1) + per-edit(2) reverify calls");
    }

    /// A per-edit retry whose reverify succeeds but introduces a new untargeted
    /// diagnostic is rejected, and its residual is NOT reported: the outcome keeps the
    /// residual of the earlier accepted edit.
    #[test]
    fn incremental_regressed_last_call_keeps_prior_residual() {
        let source = "LineA\nLineB\n";
        let original = make_result(vec![
            make_diag("duplicate-import", 0, "LineA".len()),
            make_diag("duplicate-import", "LineA\n".len(), "LineB".len()),
        ]);
        let plan = plan_fixes_with_options(&original, source, false);
        assert_eq!(plan.edits.len(), 2);

        // Call 1: batch fails. Call 2: LineB removal (right-to-left first) is accepted.
        // Call 3: LineA removal reverifies Ok but surfaces a new empty-block finding.
        let calls = std::cell::Cell::new(0usize);
        let outcome = apply_fixes_incremental(source, plan, &original, |fixed| {
            calls.set(calls.get() + 1);
            match calls.get() {
                1 => Err(MdsError::syntax("batch-fail")),
                2 => Ok(residual_for("duplicate-import", fixed)),
                _ => Ok(residual_for("empty-block", fixed)),
            }
        });

        match outcome {
            FixOutcome::PartiallyFixed {
                source: fixed,
                residual,
                rejected,
            } => {
                assert_eq!(fixed, "LineA\n");
                assert_eq!(residual.diagnostics.len(), 1);
                assert_eq!(residual.diagnostics[0].rule, "duplicate-import");
                assert_eq!(
                    residual.diagnostics[0].message, "LineA\n",
                    "the residual must come from the accepted call, not the regressed one"
                );
                assert_eq!(rejected.len(), 1);
                assert_eq!((rejected[0].edit.start, rejected[0].edit.end), (0, 6));
                assert_eq!(rejected[0].reason, NEW_EMPTY_BLOCK_REASON);
            }
            other => panic!("expected PartiallyFixed; got: {other:?}"),
        }
        assert_eq!(calls.get(), 3, "batch(1) + per-edit(2) reverify calls");
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
    /// it returns `Rejected` in all build modes, with the exact sortedness reason and
    /// before any reverify call.
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
                    replacement: String::new(),
                },
                ByteEdit {
                    start: 0,
                    end: 6,
                    rule: "duplicate-import".to_string(),
                    replacement: String::new(),
                },
            ],
            overlap_rejected: false,
            truncated: false,
        };
        let outcome = apply_fixes_incremental(source, plan, &original, |_| {
            unreachable!("reverify must not be called when edits are unsorted")
        });
        match outcome {
            FixOutcome::Rejected {
                source: unchanged,
                reason,
            } => {
                assert_eq!(reason, SORTEDNESS_REASON);
                assert_eq!(
                    unchanged, source,
                    "a rejected plan returns the source unchanged"
                );
            }
            other => {
                panic!("unsorted edits must be rejected, not silently applied; got: {other:?}")
            }
        }
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
        match outcome {
            FixOutcome::Rejected {
                source: unchanged,
                reason,
            } => {
                assert_eq!(
                    reason,
                    format!(
                        "Fix plan has {} edits; per-edit fallback cap is {} \u{2014} batch was \
                         rejected by the reverify gate. Re-run --fix after manually reducing \
                         the issue count (avoids PF-004).",
                        FALLBACK_MAX_EDITS + 1,
                        FALLBACK_MAX_EDITS
                    )
                );
                assert_eq!(
                    unchanged, source,
                    "a rejected plan returns the source unchanged"
                );
            }
            other => panic!(
                "plan exceeding FALLBACK_MAX_EDITS must be Rejected fail-closed; got: {other:?}"
            ),
        }
        // Only 1 reverify call (batch attempt), not N+1.
        assert_eq!(
            call_count.get(),
            1,
            "only the batch reverify call must be made when cap is exceeded; got: {}",
            call_count.get()
        );
    }

    /// T-FE-FIX [ADR-008 / Option-B]: the functional `--fix` path reads raw bytes
    /// directly from `LintDiagnostic.fix_edits` and must never sanitize them.
    ///
    /// Option B sanitizes only the JSON display wire (`to_canonical_json`).  The
    /// `diag_to_edits` → `ByteEdit.replacement` chain must carry the original bytes
    /// unchanged so that `apply_plan_unchecked` writes them back to disk verbatim.
    ///
    /// This is a regression guard: it must stay GREEN before AND after the
    /// `to_canonical_json` change (only the JSON serializer is modified; `diag_to_edits`
    /// is untouched).
    #[test]
    fn fix_path_preserves_raw_bytes_in_new_text() {
        // Build `new_text` with hazardous bytes programmatically (PF-018).
        let esc = '\u{001B}';
        let rlo = '\u{202E}';
        let nl = '\n';
        let hostile = format!("{{{{{esc}name{rlo}{nl}}}}}");

        let source = format!("{{{esc}name{rlo}{nl}}}");
        let edit_start = 0;
        let edit_end = source.len();

        // Ensure the byte range is char-boundary valid so diag_to_edits doesn't drop it.
        assert!(source.is_char_boundary(edit_start));
        assert!(source.is_char_boundary(edit_end));

        let diag = LintDiagnostic {
            rule: "legacy-interpolation".to_string(),
            severity: crate::lint::diagnostic::Severity::Warn,
            message: "test".to_string(),
            help: None,
            span: None,
            file: Some("t.mds".to_string()),
            fix_removals: None,
            fix_edits: Some(vec![crate::lint::diagnostic::TextEdit {
                start: edit_start,
                end: edit_end,
                new_text: hostile.clone(),
            }]),
        };

        let edits = diag_to_edits(&diag, &source);
        assert_eq!(edits.len(), 1, "non-vacuity: one edit must be produced");

        // The replacement must carry the raw bytes — same as stored in new_text.
        assert_eq!(
            edits[0].replacement, hostile,
            "diag_to_edits must clone new_text raw into ByteEdit.replacement; \
             sanitizing here would corrupt the file written by apply_plan_unchecked"
        );

        // Confirm the raw hazardous chars ARE in the replacement (non-vacuity).
        assert!(
            edits[0].replacement.contains(esc),
            "raw ESC must survive in ByteEdit.replacement (fix path, not JSON wire)"
        );
        assert!(
            edits[0].replacement.contains(rlo),
            "raw RLO must survive in ByteEdit.replacement (fix path, not JSON wire)"
        );
        assert!(
            edits[0].replacement.contains(nl),
            "raw newline must survive in ByteEdit.replacement (fix path, not JSON wire)"
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
