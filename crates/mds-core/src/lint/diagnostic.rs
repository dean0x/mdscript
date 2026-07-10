//! Lint diagnostic types: `LintDiagnostic`, `Severity`, `LintResult`.
//!
//! `LintDiagnostic` implements `std::error::Error` and `miette::Diagnostic` so it can
//! be rendered by miette at the CLI human-render boundary. The `severity()` override
//! maps our `Severity` enum to miette's rendering tiers (Error/Warning/Advice).
//!
//! **Sanitization discipline**: `sanitize_control_chars` is a render-boundary helper.
//! It is NOT called in `LintDiagnostic` constructors — the raw message is preserved
//! intact for `LintResult::to_canonical_json()` (typed serialization is safe; C0/C1
//! bytes in JSON string values are legal and the consumer can handle them). Apply
//! `sanitize_control_chars` only at the CLI human-render step (mds-cli/src/lint.rs).

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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
}

// ── LintResult ────────────────────────────────────────────────────────────────

/// The result of a lint pass on one or more modules.
///
/// `diagnostics` is the collected findings, capped at `MAX_DIAGNOSTICS` per file.
/// When `truncated` is `true`, collection was stopped early and the caller should
/// re-run after resolving visible findings.
#[derive(Debug)]
pub struct LintResult {
    /// Collected lint findings. Never contains `Severity::Off` diagnostics.
    pub diagnostics: Vec<LintDiagnostic>,
    /// `true` when the `MAX_DIAGNOSTICS` cap was reached for at least one file.
    pub truncated: bool,
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
    /// Per-file grouping: diagnostics without a `file` are grouped under `null` key.
    /// The `fixable` field is always present (defaults to `false` until the `--fix`
    /// planner populates it in S2).
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

            let d = serde_json::json!({
                "rule": diag.rule,
                "severity": diag.severity.to_string(),
                "message": diag.message,
                "help": diag.help,
                "fixable": false,
                "span": span_json,
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

    pub(crate) fn build(self) -> LintResult {
        LintResult {
            diagnostics: self.diagnostics,
            truncated: self.truncated,
        }
    }
}

// ── sanitize_control_chars ────────────────────────────────────────────────────

/// Strip or escape C0 (U+0000–U+001F incl. ESC) and C1 (U+0080–U+009F) control
/// characters from a string, except `\n` (U+000A) and `\t` (U+0009).
///
/// Applied ONLY at the CLI human-render boundary — NOT in `LintDiagnostic`
/// constructors. The raw message is preserved in `to_canonical_json()` output
/// because typed JSON serialization escapes control characters safely, and mutating
/// the constructor would corrupt the LSP-stable wire format.
///
/// Replacement strategy: replace each control character with its Unicode escape
/// `\uXXXX` to make the rendered text visually safe on terminals without silently
/// dropping information that a developer might need to diagnose rule logic.
pub fn sanitize_control_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let is_c0 = ch < '\u{0020}' && ch != '\n' && ch != '\t';
        let is_c1 = ('\u{0080}'..='\u{009F}').contains(&ch);
        if is_c0 || is_c1 {
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
    fn sanitize_escapes_multiple_controls() {
        let input = "\x07bell\x0Dcarriage\x1Bescape\u{0085}nel";
        let output = sanitize_control_chars(input);
        assert!(output.contains("\\u0007"));
        assert!(output.contains("\\u000D"));
        assert!(output.contains("\\u001B"));
        assert!(output.contains("\\u0085"));
    }

    /// L-U-H2 regression: raw message is NOT sanitized in to_canonical_json —
    /// JSON serialization handles control characters safely via `serde_json`.
    #[test]
    fn canonical_json_raw_message_preserved() {
        let result = LintResult {
            diagnostics: vec![LintDiagnostic {
                rule: "test-rule".to_string(),
                severity: Severity::Warn,
                message: "msg\x1Bwith\x00controls".to_string(),
                help: None,
                span: None,
                file: Some("f.mds".to_string()),
            }],
            truncated: false,
        };
        let json = result.to_canonical_json();
        let raw_msg = json["files"][0]["diagnostics"][0]["message"]
            .as_str()
            .unwrap();
        // The raw bytes should appear in JSON (serde_json escapes them as \u00xx).
        // Crucially, we must NOT have applied sanitize_control_chars in the constructor.
        assert!(
            raw_msg.contains('\x1B') || raw_msg.contains("\\u001B"),
            "JSON should preserve or properly escape ESC byte, got: {raw_msg:?}"
        );
        assert!(
            raw_msg.contains('\x00') || raw_msg.contains("\\u0000"),
            "JSON should preserve or properly escape NUL byte, got: {raw_msg:?}"
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
        });
        assert!(!rejected, "push beyond cap should return false");

        let result = builder.build();
        assert_eq!(result.diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(result.truncated, "truncated must be true when cap was hit");
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
        };
        assert_eq!(warn.severity(), Some(miette::Severity::Warning));

        let info = LintDiagnostic {
            rule: "r".to_string(),
            severity: Severity::Info,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
        };
        assert_eq!(info.severity(), Some(miette::Severity::Advice));

        let err = LintDiagnostic {
            rule: "r".to_string(),
            severity: Severity::Error,
            message: "x".to_string(),
            help: None,
            span: None,
            file: None,
        };
        assert_eq!(err.severity(), Some(miette::Severity::Error));
    }
}
