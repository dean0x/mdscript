//! Lint diagnostic types: `LintDiagnostic`, `Severity`, `LintResult`.
//!
//! `LintDiagnostic` implements `std::error::Error` and `miette::Diagnostic` so it can
//! be rendered by miette at the CLI human-render boundary. The `severity()` override
//! maps our `Severity` enum to miette's rendering tiers (Error/Warning/Advice).
//!
//! **Sanitization discipline**: `sanitize_control_chars` is applied at ALL
//! serialization and render boundaries:
//! - CLI human render (`render_diag_human` in mds-cli/src/lint.rs)
//! - `MdsError::serialize()` (error.rs) — covers all three bindings' error path
//! - `LintResult::to_canonical_json()` (this module) — covers all surfaces' lint path
//! - Python typed `LintDiagnostic` pyclass (mds-python/src/lib.rs)
//!
//! `message` and `help` in serialized/rendered output carry sanitized `\uXXXX` literals
//! for C0/DEL/C1 control characters. Raw byte values are preserved only in the stored
//! `LintDiagnostic` struct so that span offsets and fix-edits remain byte-accurate.
//! `sanitize_control_chars` is NOT called in `LintDiagnostic` constructors.

use std::fmt;

use crate::error::SerializedSpan;
use crate::limits::MAX_DIAGNOSTICS;

// ── Severity ──────────────────────────────────────────────────────────────────

/// Per-rule severity level.
///
/// `Off` silences the rule entirely (no diagnostic emitted, exit code unaffected).
/// `Info` renders as an advice note; never affects the exit code.
/// `Warn` renders as a warning; produces exit code 1 (warning-only run).
/// `Error` renders as an error; produces exit code 2.
///
/// Serialization: `"off"` / `"info"` / `"warn"` / `"error"` (closed enum — unknown
/// severity VALUE strings fail loudly via serde deserialization error, not
/// warn-and-ignore; only unknown rule NAMES get the lenient treatment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Rule is silenced — no diagnostic, no exit-code contribution.
    Off,
    /// Informational; rendered as miette Advice (ℹ). Never affects exit code.
    Info,
    /// Warning; rendered as miette Warning (⚠). Produces exit code 1 when no errors present.
    Warn,
    /// Error; rendered as miette Error (✖). Produces exit code 2.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Off => f.write_str("off"),
            Severity::Info => f.write_str("info"),
            Severity::Warn => f.write_str("warn"),
            Severity::Error => f.write_str("error"),
        }
    }
}

// ── TextEdit ──────────────────────────────────────────────────────────────────

/// A replacement edit at a byte range in the source.
///
/// Used by the fix planner for in-place replacement edits (e.g. `{x}` → `{{x}}`).
/// An empty `new_text` is equivalent to a pure deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// Inclusive start byte offset of the range to replace.
    pub start: usize,
    /// Exclusive end byte offset of the range to replace.
    pub end: usize,
    /// Replacement text (empty string for pure deletion).
    pub new_text: String,
}

// ── FixLineSpan ───────────────────────────────────────────────────────────────

/// A line-range descriptor for a single lint auto-fix removal.
///
/// Encodes which lines to remove from the raw source to apply the fix. Byte
/// offsets point into any character within the target line — the planner
/// computes exact start/end positions (via `line_start` / `extend_to_line_end`).
///
/// - `from`: byte offset within the first line to remove.
/// - `to`: byte offset within the boundary line.
/// - `to_inclusive: true` → remove through the END of the line containing `to`.
/// - `to_inclusive: false` → remove only UP TO the START of the line containing
///   `to` (i.e. the `to` line is kept; this is used for partial-block removals
///   where the closing `@end` must remain).
///
/// **Single-line helper**: use `FixLineSpan::single(offset)` to remove exactly
/// the one line that contains `offset`.
#[derive(Debug, Clone)]
pub struct FixLineSpan {
    /// Byte offset of any character in the first line to remove.
    pub from: usize,
    /// Byte offset of the boundary line (inclusive or exclusive per `to_inclusive`).
    pub to: usize,
    /// When `true`, the line containing `to` is removed along with all lines
    /// between `from` and `to`. When `false`, removal stops at the START of the
    /// line containing `to` (the `to` line itself is kept).
    pub to_inclusive: bool,
}

impl FixLineSpan {
    /// Remove exactly the one line that contains `offset`.
    ///
    /// Equivalent to `FixLineSpan { from: offset, to: offset, to_inclusive: true }`.
    pub fn single(offset: usize) -> Self {
        FixLineSpan {
            from: offset,
            to: offset,
            to_inclusive: true,
        }
    }
}

// ── LintDiagnostic ────────────────────────────────────────────────────────────

/// A single lint finding.
///
/// Implements `std::error::Error + miette::Diagnostic` so it can be rendered by
/// miette at the CLI boundary: `eprintln!("{:?}", miette::Report::from(diag))`.
/// The `severity()` override maps `Severity::Info` → Advice, `Warn` → Warning,
/// `Error` → Error; `Off` diagnostics are never constructed (the lint engine filters
/// them before collecting).
///
/// Attach a named source for miette span rendering:
/// ```rust,no_run
/// // At the CLI render boundary:
/// // let diag = diag.with_source(Arc::new(miette::NamedSource::new(filename, src)));
/// ```
///
/// **JSON**: use `LintResult::to_canonical_json()` — never construct JSON manually.
/// **Sanitization**: apply `sanitize_control_chars` at the CLI render boundary only.
pub struct LintDiagnostic {
    /// Short rule identifier, e.g. `"unused-variable"`. Becomes the miette code
    /// `mds::lint::<rule>`.
    pub rule: String,
    /// Effective severity of this finding (never `Off` — `Off` diagnostics are not
    /// collected).
    pub severity: Severity,
    /// Human-readable finding description. Raw — do not sanitize in the constructor.
    pub message: String,
    /// Optional fix hint shown below the message.
    pub help: Option<String>,
    /// Source span for miette label rendering and JSON `span` field.
    pub span: Option<SerializedSpan>,
    /// Source file path (for per-file JSON grouping and miette NamedSource).
    pub file: Option<String>,
    /// Concrete fix plan for this diagnostic: `Some(spans)` when an auto-fix is
    /// available for this specific instance; `None` when the rule does not support
    /// fixing this case (e.g. partially-reducible blocks, report-only rules).
    ///
    /// Used by the fix planner (`plan_fixes_with_options`) instead of the legacy
    /// span-based heuristic. The `fixable` field in the JSON wire format is
    /// `fix_removals.is_some() && tier::is_fixable(rule, is_standalone)`.
    pub fix_removals: Option<Vec<FixLineSpan>>,
    /// In-place replacement edits for this diagnostic (alternative to `fix_removals`).
    ///
    /// Used for rules that need to replace text rather than remove whole lines
    /// (e.g. `legacy-interpolation` replaces `{x}` with `{{x}}`). `None` for
    /// rules that use `fix_removals` or have no fix.
    pub fix_edits: Option<Vec<TextEdit>>,
}

impl fmt::Debug for LintDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LintDiagnostic")
            .field("rule", &self.rule)
            .field("severity", &self.severity)
            .field("message", &self.message)
            .field("help", &self.help)
            .field("span", &self.span)
            .field("file", &self.file)
            .field("fix_removals", &self.fix_removals.as_ref().map(|v| v.len()))
            .field("fix_edits", &self.fix_edits.as_ref().map(|v| v.len()))
            .finish()
    }
}

impl fmt::Display for LintDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.rule, self.message)
    }
}

impl std::error::Error for LintDiagnostic {}

impl miette::Diagnostic for LintDiagnostic {
    /// Dynamic code `mds::lint::<rule>` — stable across runs, usable by tooling.
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(format!("mds::lint::{}", self.rule)))
    }

    /// Map our Severity to miette's rendering tier.
    fn severity(&self) -> Option<miette::Severity> {
        match self.severity {
            Severity::Off => None, // should never happen; Off diagnostics are filtered
            Severity::Info => Some(miette::Severity::Advice),
            Severity::Warn => Some(miette::Severity::Warning),
            Severity::Error => Some(miette::Severity::Error),
        }
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help
            .as_deref()
            .map(|h| -> Box<dyn fmt::Display + 'a> { Box::new(h) })
    }

    /// Return the finding's span as a single labeled span for miette source rendering.
    ///
    /// The label text is the diagnostic message so the caret points directly at the
    /// offending byte range with a one-line summary. Source code is NOT embedded in
    /// `LintDiagnostic` itself — attach it at the CLI render boundary via
    /// `miette::Report::from(diag).with_source_code(NamedSource::new(file, src))`.
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let span = self.span.as_ref()?;
        let labeled = miette::LabeledSpan::at(
            miette::SourceSpan::new(span.offset.into(), span.length),
            self.message.as_str(),
        );
        Some(Box::new(std::iter::once(labeled)))
    }
}

// ── LintResult ────────────────────────────────────────────────────────────────

/// The result of a lint pass on one or more modules.
///
/// `diagnostics` is the collected findings, capped at `MAX_DIAGNOSTICS` per file.
/// When `truncated` is `true`, collection was stopped early and the caller should
/// re-run after resolving visible findings.
///
/// `is_standalone` is `true` when the entry module has no `@import` or `@extends`
/// directives — used to determine whether Tier B fixes (unused-import, unused-function)
/// are safe to apply (they change compiled output for non-standalone files because the
/// importer's compiled output depends on what the imported module exports).
#[derive(Debug)]
pub struct LintResult {
    /// Collected lint findings. Never contains `Severity::Off` diagnostics.
    pub diagnostics: Vec<LintDiagnostic>,
    /// `true` when the `MAX_DIAGNOSTICS` cap was reached for at least one file.
    pub truncated: bool,
    /// `true` when the entry has no @import or @extends (Tier B fixes are safe).
    pub is_standalone: bool,
}

impl LintResult {
    /// Produce the canonical, LSP-stable JSON wire format.
    ///
    /// Schema:
    /// ```json
    /// {
    ///   "version": 1,
    ///   "files": [
    ///     {
    ///       "file": "<path>",
    ///       "diagnostics": [
    ///         {
    ///           "rule": "unused-variable",
    ///           "severity": "warn",
    ///           "message": "...",
    ///           "help": "...",
    ///           "fixable": false,
    ///           "span": { "offset": 0, "length": 5, "line": 1, "column": 1 }
    ///         }
    ///       ]
    ///     }
    ///   ],
    ///   "truncated": false
    /// }
    /// ```
    ///
    /// Per-file grouping: diagnostics without a `file` are grouped under the string
    /// key `"<unknown>"` (a defensive fallback; in practice every rule sets `file:
    /// Some(..)`).
    /// The `fixable` field reflects tier semantics: `true` for Tier A rules and for
    /// Tier B rules when the file is standalone, `false` otherwise.
    ///
    /// **NEVER** build this JSON via `format!()` — use `serde_json::json!()` so
    /// control characters in message/help are serialized safely.
    pub fn to_canonical_json(&self) -> serde_json::Value {
        use std::collections::BTreeMap;

        // Group diagnostics by file, preserving insertion order within each group.
        // BTreeMap gives deterministic (sorted) file ordering in the output.
        let mut by_file: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();

        for diag in &self.diagnostics {
            let key = diag.file.clone().unwrap_or_else(|| "<unknown>".to_string());
            let span_json = diag.span.as_ref().map(|s| {
                let mut obj = serde_json::json!({
                    "offset": s.offset,
                    "length": s.length,
                });
                if let Some(line) = s.line {
                    obj["line"] = serde_json::Value::from(line);
                }
                if let Some(col) = s.column {
                    obj["column"] = serde_json::Value::from(col);
                }
                obj
            });

            let fix_edits_json = diag.fix_edits.as_ref().map(|edits| {
                edits
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "start": e.start,
                            "end": e.end,
                            "new_text": e.new_text,
                        })
                    })
                    .collect::<Vec<_>>()
            });

            // Sanitize at the serialization boundary (issue #176 / CWE-150):
            // message and help carry sanitized \uXXXX literals for C0/DEL/C1 bytes.
            // Spans and fix_edits reference raw byte offsets into the source — left
            // untouched so fix pipelines and span highlighting stay accurate.
            let d = serde_json::json!({
                "rule": diag.rule,
                "severity": diag.severity.to_string(),
                "message": sanitize_control_chars(&diag.message),
                "help": diag.help.as_deref().map(sanitize_control_chars),
                "fixable": (diag.fix_removals.is_some() || diag.fix_edits.is_some()) && super::tier::is_fixable(&diag.rule, self.is_standalone),
                "span": span_json,
                "fix_edits": fix_edits_json,
            });

            by_file.entry(key).or_default().push(d);
        }

        let files: Vec<serde_json::Value> = by_file
            .into_iter()
            .map(|(file, diagnostics)| {
                serde_json::json!({
                    "file": file,
                    "diagnostics": diagnostics,
                })
            })
            .collect();

        serde_json::json!({
            "version": 1,
            "files": files,
            "truncated": self.truncated,
        })
    }
}

// ── LintResult builder ────────────────────────────────────────────────────────

/// Accumulate diagnostics with `MAX_DIAGNOSTICS` truncation enforcement.
///
/// Used internally by the facts walk and rule dispatch to build a `LintResult`
/// without needing to check the cap at each call site.
pub(crate) struct LintResultBuilder {
    diagnostics: Vec<LintDiagnostic>,
    truncated: bool,
}

impl LintResultBuilder {
    pub(crate) fn new() -> Self {
        LintResultBuilder {
            diagnostics: Vec::new(),
            truncated: false,
        }
    }

    /// Push a diagnostic unless the cap has been reached.
    ///
    /// Returns `true` when the diagnostic was accepted; `false` when the cap was
    /// hit (and `truncated` is set). The caller should stop collecting when this
    /// returns `false`.
    ///
    /// Called by rule implementations in `lint/rules/*.rs`.
    pub(crate) fn push(&mut self, diag: LintDiagnostic) -> bool {
        if self.diagnostics.len() >= MAX_DIAGNOSTICS {
            self.truncated = true;
            return false;
        }
        self.diagnostics.push(diag);
        true
    }

    pub(crate) fn build(self, is_standalone: bool) -> LintResult {
        LintResult {
            diagnostics: self.diagnostics,
            truncated: self.truncated,
            is_standalone,
        }
    }
}

// ── sanitize_control_chars ────────────────────────────────────────────────────

/// Strip or escape C0 (U+0000–U+001F incl. ESC), DEL (U+007F), and C1
/// (U+0080–U+009F) control characters from a string, except `\n` (U+000A)
/// and `\t` (U+0009).
///
/// Applied at all serialization and render boundaries (CLI human render, JSON
/// serialization, and the Python typed conversion). NOT called in `LintDiagnostic`
/// constructors so that span offsets and fix-edit byte ranges remain accurate against
/// the raw source. Raw bytes in the stored struct; sanitized literals in all output.
///
/// Replacement strategy: replace each control character with its Unicode escape
/// `\uXXXX` to make the rendered text visually safe on terminals without silently
/// dropping information that a developer might need to diagnose rule logic.
/// DEL (U+007F) is included because some terminals interpret it as a backspace,
/// which can corrupt human-readable output. The function is idempotent — calling it
/// twice on already-sanitized text is a no-op.
pub fn sanitize_control_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let is_c0 = ch < '\u{0020}' && ch != '\n' && ch != '\t';
        let is_del = ch == '\u{007F}';
        let is_c1 = ('\u{0080}'..='\u{009F}').contains(&ch);
        if is_c0 || is_del || is_c1 {
            // Replace with Unicode escape so the byte is visible but harmless.
            let _ = fmt::write(&mut out, format_args!("\\u{:04X}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── L-U-H2: sanitize_control_chars ───────────────────────────────────────

    #[test]
    fn sanitize_preserves_normal_text() {
        assert_eq!(sanitize_control_chars("hello world"), "hello world");
    }

    #[test]
    fn sanitize_preserves_newline_and_tab() {
        assert_eq!(sanitize_control_chars("hello\nworld\t!"), "hello\nworld\t!");
    }

    #[test]
    fn sanitize_escapes_nul() {
        assert_eq!(sanitize_control_chars("a\x00b"), "a\\u0000b");
    }

    #[test]
    fn sanitize_escapes_esc() {
        assert_eq!(sanitize_control_chars("a\x1Bb"), "a\\u001Bb");
    }

    #[test]
    fn sanitize_escapes_c1_range() {
        // U+0080 is the first C1 control character.
        assert_eq!(sanitize_control_chars("a\u{0080}b"), "a\\u0080b");
        // U+009F is the last C1 control character.
        assert_eq!(sanitize_control_chars("a\u{009F}b"), "a\\u009Fb");
    }

    #[test]
    fn sanitize_escapes_del() {
        // I-14: DEL (U+007F) is interpreted by some terminals — ensure it is escaped.
        assert_eq!(sanitize_control_chars("a\u{007F}b"), "a\\u007Fb");
    }

    #[test]
    fn sanitize_escapes_multiple_controls() {
        let input = "\x07bell\x0Dcarriage\x1Bescape\u{0085}nel";
        let output = sanitize_control_chars(input);
        assert!(output.contains("\\u0007"));
        assert!(output.contains("\\u000D"));
        assert!(output.contains("\\u001B"));
        assert!(output.contains("\\u0085"));
    }

    /// T-4 [AC-F4, AC-C3]: `to_canonical_json()` sanitizes control chars in diagnostic
    /// message and help. Simulates a `unused-variable` diagnostic whose message embeds
    /// a raw ESC byte (e.g. from a hostile frontmatter key like `"ab"`).
    ///
    /// After the fix: the JSON message/help must carry the sanitized `\uXXXX` literal;
    /// the raw ESC byte must not be present in the JSON string value.
    /// Span offsets (raw byte positions) must remain unchanged.
    #[test]
    fn to_canonical_json_sanitizes_diagnostic_message() {
        // Simulate an unused-variable diagnostic whose variable name contains U+001B.
        let hostile_name = "a\x1Bb";
        let result = LintResult {
            diagnostics: vec![LintDiagnostic {
                rule: "unused-variable".to_string(),
                severity: Severity::Warn,
                message: format!(
                    "Variable '{}' is defined in frontmatter but never referenced in the body.",
                    hostile_name
                ),
                help: Some(
                    "Remove the frontmatter key or reference it in the template body.".to_string(),
                ),
                span: Some(crate::error::SerializedSpan {
                    offset: 4,
                    length: 3,
                    line: None,
                    column: None,
                }),
                file: Some("test.mds".to_string()),
                fix_removals: None,
                fix_edits: None,
            }],
            truncated: false,
            is_standalone: false,
        };

        let json = result.to_canonical_json();
        let diag = &json["files"][0]["diagnostics"][0];

        // Wire format shape must be intact.
        assert_eq!(json["version"], 1, "version field must be 1");
        assert_eq!(json["truncated"], false, "truncated must be false");
        assert_eq!(json["files"][0]["file"], "test.mds");

        let msg = diag["message"].as_str().unwrap();
        // Raw ESC byte must not appear in the serialized message.
        assert!(
            !msg.contains('\x1B'),
            "raw ESC byte must not appear in to_canonical_json message; got: {msg:?}"
        );
        // Sanitized 6-char literal \\u001B must appear.
        assert!(
            msg.contains("\\u001B"),
            "sanitized literal \\u001B must appear in to_canonical_json message; got: {msg:?}"
        );

        // Span offset must be byte-accurate (not corrupted by sanitization).
        assert_eq!(
            diag["span"]["offset"], 4,
            "span offset must be unchanged after message sanitization"
        );
        assert_eq!(
            diag["span"]["length"], 3,
            "span length must be unchanged after message sanitization"
        );
    }

    // ── LintResultBuilder truncation ──────────────────────────────────────────

    #[test]
    fn builder_truncates_at_max_diagnostics() {
        let mut builder = LintResultBuilder::new();
        for i in 0..MAX_DIAGNOSTICS {
            let accepted = builder.push(LintDiagnostic {
                rule: format!("r{i}"),
                severity: Severity::Warn,
                message: format!("m{i}"),
                help: None,
                span: None,
                file: None,
                fix_removals: None,
                fix_edits: None,
            });
            assert!(accepted, "diagnostic {i} should be accepted");
        }
        // The (MAX_DIAGNOSTICS+1)-th push should be rejected.
        let rejected = builder.push(LintDiagnostic {
            rule: "overflow".to_string(),
            severity: Severity::Warn,
            message: "overflow".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        });
        assert!(!rejected, "push beyond cap should return false");

        let result = builder.build(false);
        assert_eq!(result.diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(result.truncated, "truncated must be true when cap was hit");
    }

    // ── I-25: truncated wire format ───────────────────────────────────────────

    #[test]
    fn builder_truncated_canonical_json() {
        // I-25: verify the truncated path in the serialized wire format.
        // Push MAX_DIAGNOSTICS + 1 to trigger truncation, then confirm the JSON
        // output has `"truncated": true` and the diagnostics array is capped.
        let mut builder = LintResultBuilder::new();
        for i in 0..=MAX_DIAGNOSTICS {
            builder.push(LintDiagnostic {
                rule: format!("r{i}"),
                severity: Severity::Warn,
                message: format!("m{i}"),
                help: None,
                span: None,
                file: Some("f.mds".to_string()),
                fix_removals: None,
                fix_edits: None,
            });
        }
        let result = builder.build(false);
        assert!(result.truncated, "struct truncated flag must be set");

        let json = result.to_canonical_json();

        assert_eq!(
            json["truncated"],
            serde_json::Value::Bool(true),
            "serialized truncated field must be true"
        );

        let diags = json["files"][0]["diagnostics"].as_array().unwrap();
        assert_eq!(
            diags.len(),
            MAX_DIAGNOSTICS,
            "serialized diagnostics array must be capped at MAX_DIAGNOSTICS"
        );
    }

    // ── LintDiagnostic miette::Diagnostic impl ────────────────────────────────

    #[test]
    fn diagnostic_code_is_namespaced() {
        use miette::Diagnostic;
        let diag = LintDiagnostic {
            rule: "unused-variable".to_string(),
            severity: Severity::Warn,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        };
        let code = diag.code().unwrap().to_string();
        assert_eq!(code, "mds::lint::unused-variable");
    }

    #[test]
    fn diagnostic_severity_maps_correctly() {
        use miette::Diagnostic;

        let warn = LintDiagnostic {
            rule: "r".to_string(),
            severity: Severity::Warn,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        };
        assert_eq!(warn.severity(), Some(miette::Severity::Warning));

        let info = LintDiagnostic {
            rule: "r".to_string(),
            severity: Severity::Info,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        };
        assert_eq!(info.severity(), Some(miette::Severity::Advice));

        let err = LintDiagnostic {
            rule: "r".to_string(),
            severity: Severity::Error,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        };
        assert_eq!(err.severity(), Some(miette::Severity::Error));
    }
}
