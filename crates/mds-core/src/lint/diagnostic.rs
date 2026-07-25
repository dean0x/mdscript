//! Lint diagnostic types: `LintDiagnostic`, `Severity`, `LintResult`.
//!
//! `LintDiagnostic` implements `std::error::Error` and `miette::Diagnostic` so it can
//! be rendered by miette at the CLI human-render boundary. The `severity()` override
//! maps our `Severity` enum to miette's rendering tiers (Error/Warning/Advice).
//!
//! **Sanitization discipline**: `message` and `help` are sanitized at every output
//! boundary. The escaped class is C0 except `\n`/`\t`, DEL (U+007F), C1
//! (U+0080–U+009F), the Unicode bidi controls (U+200E/U+200F, U+202A–U+202E,
//! U+2066–U+2069 — Trojan Source, CVE-2021-42574), the JS line/paragraph separators
//! U+2028/U+2029, and U+FEFF; each becomes an uppercase 6-char `\uXXXX` literal.
//!
//! Two escape modes share one implementation, differing only on `\n`:
//!
//! - **HUMAN** ([`sanitize_control_chars`]) — `\n` preserved. For terminal / miette
//!   render output, where multi-line frames must stay readable.
//! - **WIRE** ([`sanitize_control_chars_wire`]) — `\n` escaped too, so a hostile
//!   message cannot forge an extra line in a line-oriented consumer of the value
//!   (log forging, YAML key injection).
//!
//! `\t` is preserved in both modes. The boundary set below is closed for the
//! **lint diagnostic** and **wire/JSON** paths; see "Known gap" after the table for
//! the one terminal path that is not yet covered.
//!
//! | Boundary | Mode | Fields |
//! |----------|------|--------|
//! | `render_diag_human` (mds-cli/src/lint.rs) | HUMAN | `message`/`help`/filename via `sanitize_control_chars`; source excerpts via `neutralize_source_for_render` (byte-length-preserving; avoids PF-014 caret desync) |
//! | `MdsError::at()` (error.rs) | HUMAN | filename; source via `neutralize_source_for_render` |
//! | `MdsError::display_sanitized()` (error.rs) | HUMAN | whole `Display` string — helper only; **not currently on any CLI path** (see "Known gap") |
//! | `emit_warnings()` (lib.rs) | HUMAN | warning strings printed to stderr |
//! | `safe_path()` (mds-cli/src/output.rs) | HUMAN | CLI status-line path display |
//! | `MdsError::serialize()` (error.rs) | WIRE | `message`, `help` — covers all three bindings' error path |
//! | `LintResult::to_canonical_json()` (this module) | WIRE | `message`, `help`, `files[].file` key |
//! | `CompileResult::to_canonical_json()` (lib.rs) | WIRE | warning strings; *distinct method from `LintResult::to_canonical_json`, not a duplicate* |
//! | Python `LintResult::new()` via `sanitize_lint_value()` | WIRE | `message`, `help`, `file` — construction-time, so typed getters read pre-sanitized data (PF-004) |
//! | `--diff` / `--check` preview output (mds-cli/src/output.rs) | HUMAN, TTY-gated | neutralized when stdout is a TTY; byte-faithful when piped, so redirected diffs stay applicable |
//!
//! **Known gap — `MdsError` message text on the CLI terminal path.** The table above
//! covers lint diagnostics and every wire boundary, but a *compile/build* error still
//! reaches the terminal with its message unsanitized. `mds build` / `mds check` render
//! errors via `eprint_error` → `format!("{report:?}")`, which is miette's own renderer;
//! post-processing that rendered frame is forbidden by PF-014, and the only helper that
//! would close the gap — [`MdsError::display_sanitized`] — has no production caller.
//! `MdsError::at()` does neutralize the *source excerpt*, so the caret frame is safe,
//! but an error message that interpolates attacker-controlled text (e.g.
//! `parser.rs`'s `invalid include alias: '{alias}'`) still emits raw control bytes to
//! stderr. Closing it requires sanitizing at `MdsError` construction rather than at
//! render time; that is a design decision with broad golden-test blast radius and is
//! deliberately NOT addressed in issue #176. Do not "fix" it by wrapping the rendered
//! frame — that is exactly PF-014.
//!
//! **Deliberate exclusions** (documented, not gaps):
//! - `LintDiagnostic::fmt` (raw `Display`) — unsanitized by design; use
//!   [`MdsError::display_sanitized`] for terminal output
//! - napi `err.detail` — populated only under the `debug-panics` Cargo feature,
//!   which CLAUDE.md forbids shipping
//!
//! Fields NOT sanitized: `rule` (fixed identifiers), `span`/`fix_edits` byte offsets
//! (raw byte accuracy required for fix pipelines and span highlighting).
//!
//! Raw byte values are preserved in the stored `LintDiagnostic` struct so that span
//! offsets and fix-edits remain byte-accurate. Neither sanitizer is called in
//! `LintDiagnostic` constructors.
//!
//! **Escaping is one-way.** The transformation is lossy and non-injective: a template
//! that literally contains the six characters `\`,`u`,`0`,`0`,`1`,`B` and one that
//! contains an actual ESC byte are indistinguishable in the output. Consumers MUST NOT
//! un-escape `\uXXXX` sequences back into bytes — that reconstitutes exactly the
//! injection this guard prevents. Round-tripping is an explicit non-goal; no
//! backslash-escaping will be added to make the mapping reversible. A consumer that
//! needs original bytes must read them from the source via the raw `span` /
//! `fix_edits` byte offsets.

use std::borrow::Cow;
use std::fmt;
use std::fmt::Write as _;

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
/// miette. The `severity()` override maps `Severity::Info` → Advice, `Warn` → Warning,
/// `Error` → Error; `Off` diagnostics are never constructed (the lint engine filters
/// them before collecting).
///
/// **CLI render**: always use `mds_cli::output::eprint_error` to render diagnostics on
/// a TTY — never call `eprintln!("{report:?}")` on a raw `miette::Report`. Writing the
/// rendered frame directly bypasses input-level sanitization and can inject C0/C1
/// control bytes from hostile source content into the terminal (CWE-150 / PF-014).
/// Input fields (`message`, `help`, source excerpts) are sanitized before the Report
/// is constructed; post-rendering the frame must not be re-sanitized.
///
/// **JSON**: use `LintResult::to_canonical_json()` — never construct JSON manually.
/// **Sanitization**: see the module-level "Sanitization discipline" note — `message`
/// and `help` are sanitized at every output boundary; constructors keep raw bytes so
/// span offsets and `fix_edits` stay byte-accurate.
pub struct LintDiagnostic {
    /// Short rule identifier, e.g. `"unused-variable"`. Becomes the miette code
    /// `mds::lint::<rule>`.
    pub rule: String,
    /// Effective severity of this finding (never `Off` — `Off` diagnostics are not
    /// collected).
    pub severity: Severity,
    /// Human-readable finding description. Raw — do not sanitize in the constructor
    /// (sanitized at output boundaries — see module docs).
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
    #[must_use]
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

            // Sanitize at the serialization boundary (issue #176 / CWE-150) in WIRE
            // mode: message and help carry sanitized \uXXXX literals for control,
            // bidi, separator and BOM characters — and for `\n`, so a hostile message
            // cannot forge an extra line in a line-oriented consumer of this JSON.
            // Spans and fix_edits reference raw byte offsets into the source — left
            // untouched so fix pipelines and span highlighting stay accurate.
            let d = serde_json::json!({
                "rule": diag.rule,
                "severity": diag.severity.to_string(),
                "message": sanitize_control_chars_wire(&diag.message),
                "help": diag.help.as_deref().map(sanitize_control_chars_wire),
                "fixable": (diag.fix_removals.is_some() || diag.fix_edits.is_some()) && super::tier::is_fixable(&diag.rule, self.is_standalone),
                "span": span_json,
                "fix_edits": fix_edits_json,
            });

            by_file.entry(key).or_default().push(d);
        }

        // Sanitize the file key at the serialization boundary (issue #176 / CWE-150),
        // WIRE mode: POSIX filenames may legally contain C0/DEL/C1 bytes, bidi
        // controls, and even newlines.  All surfaces that call to_canonical_json()
        // inherit this fix without further changes.
        let files: Vec<serde_json::Value> = by_file
            .into_iter()
            .map(|(file, diagnostics)| {
                serde_json::json!({
                    "file": sanitize_control_chars_wire(&file),
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

/// Which escape class a sanitizer call applies.
///
/// Both variants escape the same hostile character class (see [`is_control_char`]);
/// they differ only in how they treat `\n` (U+000A). `\t` (U+0009) is preserved by
/// both — a tab cannot forge a line and cannot reposition a cursor destructively.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EscapeMode {
    /// Terminal / miette render output. `\n` is preserved so multi-line diagnostic
    /// frames stay readable.
    Human,
    /// Machine-readable output (JSON wire, binding error objects). `\n` is escaped
    /// as well, so a hostile message cannot forge an extra line in a line-oriented
    /// consumer of the string value (log forging, YAML key injection).
    Wire,
}

/// Escape hostile characters, preserving `\n` (HUMAN mode).
///
/// The escaped class is C0 (U+0000–U+001F incl. ESC) except `\n`/`\t`, DEL (U+007F),
/// C1 (U+0080–U+009F), the Unicode bidi controls (U+200E/U+200F, U+202A–U+202E,
/// U+2066–U+2069), the line/paragraph separators U+2028/U+2029, and U+FEFF.
///
/// Use this at **render** boundaries (anything bound for a terminal or a miette
/// frame). Use [`sanitize_control_chars_wire`] at **wire** boundaries (JSON, binding
/// error objects), where an embedded newline is itself an injection vector.
///
/// Returns a borrowed view of the input when no escaping is needed (zero
/// allocation for the overwhelmingly-common clean case). Allocates only when
/// a hostile character is actually present, reserving exact capacity.
///
/// Applied at every output boundary — see the module-level "Sanitization discipline"
/// note for the authoritative list. NOT called in `LintDiagnostic` constructors so that
/// span offsets and fix-edit byte ranges remain accurate against the raw source. Raw
/// bytes in the stored struct; sanitized literals in all output.
///
/// Replacement strategy: replace each hostile character with its Unicode escape
/// `\uXXXX` (uppercase hex, 4 digits, literal backslash) to make the rendered
/// text visually safe on terminals without silently dropping information that a
/// developer might need to diagnose rule logic. DEL (U+007F) is included because
/// some terminals interpret it as a backspace, which can corrupt human-readable
/// output. The bidi controls are included because they can visually reorder a
/// diagnostic line (Trojan Source, CVE-2021-42574). The function is idempotent —
/// calling it twice on already-sanitized text is a no-op.
///
/// # Escaping is one-way
///
/// This transformation is **lossy and non-injective**: a template that literally
/// contains the six characters `\`,`u`,`0`,`0`,`1`,`B` and an actual ESC byte both
/// serialize to the same `\u001B` output. Consumers **MUST NOT** un-escape `\uXXXX`
/// sequences back into bytes — doing so re-creates exactly the injection this guard
/// exists to prevent. The escape is for display only; when a consumer needs the
/// original bytes it must read the source through `span`/`fix_edits` byte offsets,
/// which are deliberately left raw. No backslash-escaping (`\` → `\\`) will be added
/// to make the mapping reversible; round-tripping is an explicit non-goal.
///
/// # Examples
///
/// ```
/// use mds::sanitize_control_chars;
///
/// // ESC (U+001B) is escaped to the 6-char uppercase literal.
/// assert_eq!(&*sanitize_control_chars("\x1B[33m"), "\\u001B[33m");
///
/// // \n and \t are preserved in HUMAN mode.
/// assert_eq!(&*sanitize_control_chars("hello\nworld"), "hello\nworld");
///
/// // DEL (U+007F) is escaped.
/// assert_eq!(&*sanitize_control_chars("a\x7Fb"), "a\\u007Fb");
///
/// // C1 control NEL (U+0085) is escaped.
/// assert_eq!(&*sanitize_control_chars("a\u{0085}b"), "a\\u0085b");
///
/// // Bidi override (U+202E RLO — Trojan Source) is escaped.
/// assert_eq!(&*sanitize_control_chars("a\u{202E}b"), "a\\u202Eb");
///
/// // JS line separator (U+2028) and BOM (U+FEFF) are escaped.
/// assert_eq!(&*sanitize_control_chars("a\u{2028}b"), "a\\u2028b");
/// assert_eq!(&*sanitize_control_chars("a\u{FEFF}b"), "a\\uFEFFb");
///
/// // Clean input is borrowed — zero allocation.
/// let s = "normal text";
/// let cow = sanitize_control_chars(s);
/// assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
///
/// // Idempotent: a second call on already-sanitized output is a no-op.
/// let once = sanitize_control_chars("a\x1Bb");
/// let twice = sanitize_control_chars(&once);
/// assert_eq!(once, twice);
/// ```
#[must_use]
pub fn sanitize_control_chars(s: &str) -> Cow<'_, str> {
    sanitize_with(s, EscapeMode::Human)
}

/// Escape hostile characters **including `\n`** (WIRE mode).
///
/// Identical to [`sanitize_control_chars`] except that `\n` (U+000A) is also escaped
/// to its 6-character literal. `\t` is still preserved.
///
/// Use this at machine-readable boundaries — `MdsError::serialize()`,
/// `LintResult::to_canonical_json()`, `CompileResult::to_canonical_json()` warnings,
/// and the Python typed-surface construction path. A raw newline inside a JSON string
/// value is legal JSON, but once a consumer prints or line-splits that value a hostile
/// message can forge an entire extra diagnostic line (log forging / YAML key
/// injection). Escaping it makes the value single-line by construction.
///
/// The one-way-escaping contract in [`sanitize_control_chars`] applies verbatim here.
///
/// # Examples
///
/// ```
/// use mds::{sanitize_control_chars, sanitize_control_chars_wire};
///
/// // WIRE escapes the newline; HUMAN keeps it.
/// assert_eq!(&*sanitize_control_chars_wire("a\nb"), "a\\u000Ab");
/// assert_eq!(&*sanitize_control_chars("a\nb"), "a\nb");
///
/// // \t is preserved in both modes.
/// assert_eq!(&*sanitize_control_chars_wire("a\tb"), "a\tb");
///
/// // Everything else escapes identically in both modes.
/// assert_eq!(&*sanitize_control_chars_wire("a\u{202E}b"), "a\\u202Eb");
///
/// // Clean input is borrowed — zero allocation.
/// assert!(matches!(
///     sanitize_control_chars_wire("normal text"),
///     std::borrow::Cow::Borrowed(_)
/// ));
/// ```
#[must_use]
pub fn sanitize_control_chars_wire(s: &str) -> Cow<'_, str> {
    sanitize_with(s, EscapeMode::Wire)
}

/// Single escape implementation shared by both public entry points.
///
/// Kept as one function on purpose: a second, forked escape map would be a PF-004
/// parallel path — the two would drift and one boundary would silently stop
/// enforcing what the other does.
fn sanitize_with(s: &str, mode: EscapeMode) -> Cow<'_, str> {
    // Byte-level fast path: scan for any byte that can start an escaped character.
    // - C0 (U+0000–U+001F) and DEL (U+007F) are single bytes: b < 0x20 or b == 0x7F.
    // - C1 (U+0080–U+009F) in UTF-8 is encoded as 0xC2 0x80–0xC2 0x9F.
    // - U+200E/U+200F, U+2028/U+2029, U+202A–U+202E and U+2066–U+2069 all start
    //   with 0xE2; U+FEFF starts with 0xEF.
    // The scan is a deliberate over-approximation (0xC2/0xE2/0xEF also lead many
    // benign codepoints); false positives only cost a trip through the char loop
    // below, which leaves non-hostile characters unchanged.
    let needs_work = s
        .bytes()
        .any(|b| b < 0x20 || b == 0x7F || b == 0xC2 || b == 0xE2 || b == 0xEF);
    if !needs_work {
        return Cow::Borrowed(s);
    }

    // Count the escapes so we can reserve exactly: each escaped char takes 6 output
    // bytes (\uXXXX) instead of 1–3 input bytes, a net growth of at most 5 bytes.
    let n_escaped = s.chars().filter(|&ch| escapes_in(ch, mode)).count();
    let mut out = String::with_capacity(s.len() + 5 * n_escaped);

    // Bulk-copy clean runs; replace each hostile char with its \uXXXX literal.
    let mut bulk_start = 0;
    for (i, ch) in s.char_indices() {
        if escapes_in(ch, mode) {
            out.push_str(&s[bulk_start..i]);
            // \uXXXX: literal backslash + u + 4 uppercase hex digits. Every escaped
            // codepoint is in the BMP, so 4 digits is always exact.
            write!(out, "\\u{:04X}", ch as u32).expect("writing to a String is infallible");
            bulk_start = i + ch.len_utf8();
        }
    }
    // Flush the final clean segment.
    out.push_str(&s[bulk_start..]);
    Cow::Owned(out)
}

/// Returns `true` when `ch` must be escaped under `mode`.
#[inline]
fn escapes_in(ch: char, mode: EscapeMode) -> bool {
    is_control_char(ch) || (mode == EscapeMode::Wire && ch == '\n')
}

/// Returns `true` for codepoints escaped in **both** modes.
///
/// Also the predicate driving [`neutralize_source_for_render`], so the render path
/// and the escape path can never diverge on which characters are hostile (PF-004).
#[inline]
fn is_control_char(ch: char) -> bool {
    (ch < '\u{0020}' && ch != '\n' && ch != '\t')
        || ch == '\u{007F}'
        || ('\u{0080}'..='\u{009F}').contains(&ch)
        || is_format_hazard_char(ch)
}

/// Returns `true` for the non-C0/C1 codepoints that are still display-hazardous.
///
/// - **U+200E/U+200F, U+202A–U+202E, U+2066–U+2069** — the Unicode bidirectional
///   controls (marks, embeddings, overrides, isolates). They reorder how the rest of
///   a line renders, which is the Trojan Source attack (CVE-2021-42574): a diagnostic
///   or filename can be made to display as something entirely different from its bytes.
/// - **U+2028/U+2029** — LINE SEPARATOR / PARAGRAPH SEPARATOR. Both terminate a
///   JavaScript string literal, so an unescaped one can break out of generated JS.
/// - **U+FEFF** — BOM / ZERO WIDTH NO-BREAK SPACE. Invisible in every renderer, so it
///   can hide or split content the reader believes is contiguous.
///
/// Every codepoint here is in U+0800–U+FFFF, i.e. exactly 3 bytes in UTF-8 — which is
/// what lets [`neutralize_source_for_render`] substitute U+FFFD (also 3 bytes) without
/// breaking its byte-length invariant.
#[inline]
fn is_format_hazard_char(ch: char) -> bool {
    matches!(ch,
        '\u{200E}' | '\u{200F}'
        | '\u{2028}' | '\u{2029}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FEFF}'
    )
}

/// Replace control characters in source text with byte-length-preserving substitutes so
/// that miette's span byte-offsets and caret columns remain accurate (avoids PF-014).
///
/// This function is the input-sanitization companion to [`sanitize_control_chars`].
/// It MUST be applied to any source string passed to [`miette::NamedSource`] before
/// the Report is rendered.  Applying [`sanitize_control_chars`] instead would expand
/// each control char to 6 bytes (`\uXXXX`), desynchronising every span byte-offset
/// that follows the substitution point and producing misaligned carets.
///
/// It neutralizes exactly the character class [`sanitize_control_chars`] escapes —
/// the two are kept symmetric on purpose so the render path can never lag the wire
/// path on a newly-recognised hostile character (PF-004).
///
/// Substitution rules (byte-length-preserving):
/// - C0 bytes (U+0000–U+001F) except `\n`/`\t`: 1-byte → `?` (U+003F, 1 byte)
/// - DEL (U+007F): 1-byte → `?`
/// - C1 range (U+0080–U+009F, 2-byte UTF-8): 2-byte → U+00A0 NBSP (2 bytes)
/// - Bidi controls, U+2028/U+2029, U+FEFF (3-byte UTF-8): → U+FFFD (3 bytes)
///
/// Returns [`Cow::Borrowed`] when no substitution is needed (fast path).
pub fn neutralize_source_for_render(s: &str) -> Cow<'_, str> {
    let needs_neutralize = s.chars().any(is_control_char);
    if !needs_neutralize {
        return Cow::Borrowed(s);
    }
    // Allocate once; capacity is exact because every substitution preserves byte length.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        if (u < 0x20 && c != '\n' && c != '\t') || u == 0x7F {
            out.push('?'); // 1-byte C0/DEL → '?' (1 byte) — byte-length-preserving
        } else if (0x80..=0x9F).contains(&u) {
            out.push('\u{00A0}'); // 2-byte C1 → U+00A0 NBSP (2 bytes) — byte-length-preserving
        } else if is_format_hazard_char(c) {
            // 3-byte bidi/separator/BOM → U+FFFD (3 bytes) — byte-length-preserving.
            out.push('\u{FFFD}');
        } else {
            out.push(c);
        }
    }
    debug_assert_eq!(
        out.len(),
        s.len(),
        "neutralize_source_for_render must preserve byte length"
    );
    Cow::Owned(out)
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
    /// a raw ESC byte (e.g. from a hostile frontmatter key like `"a\u001Bb"`).
    ///
    /// After the fix: the JSON message AND help carry the sanitized `\uXXXX` literal
    /// (uppercase, exactly 4 digits).  Span offsets (raw byte positions) are unchanged.
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
                // Help also embeds the hostile name: removing the `map(sanitize_control_chars)`
                // call for help would leave the raw ESC byte in the output and fail below.
                help: Some(format!(
                    "Remove the key '{}' from frontmatter or reference it in the template body.",
                    hostile_name
                )),
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
        // Sanitized 6-char uppercase literal must appear (no lowercase alternative).
        assert!(
            msg.contains("\\u001B"),
            "sanitized literal \\u001B must appear in to_canonical_json message; got: {msg:?}"
        );

        let help = diag["help"].as_str().unwrap();
        // Help field must also be sanitized — pins the help-sanitize call (avoids PF-013).
        assert!(
            !help.contains('\x1B'),
            "raw ESC byte must not appear in to_canonical_json help; got: {help:?}"
        );
        assert!(
            help.contains("\\u001B"),
            "sanitized literal \\u001B must appear in to_canonical_json help; got: {help:?}"
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

    /// [testing-7]: `sanitize_control_chars` is idempotent — a second call on
    /// already-sanitized output is always a no-op.  This property is relied on by the
    /// double-pass in `render_diag_human` (field-level + whole-frame).
    #[test]
    fn sanitize_is_idempotent() {
        let cases: &[&str] = &[
            "\x1B",
            "a\x1Bb",
            "a\u{007F}b",
            "a\u{0085}b",
            "\x00\x01\x02\x1F",
            "hello world",
            "",
        ];
        for &input in cases {
            let once = sanitize_control_chars(input);
            let twice = sanitize_control_chars(&once);
            assert_eq!(
                once, twice,
                "sanitize_control_chars is not idempotent for input {input:?}"
            );
        }
    }

    // ── T-16: widened escape class — bidi / separator / BOM (issue #176) ─────
    //
    // These codepoints are outside C0/DEL/C1 but are still display-hazardous:
    //  - U+200E/U+200F and U+202A–U+202E and U+2066–U+2069 are the Unicode bidi
    //    controls behind Trojan Source (CVE-2021-42574): they can visually reorder
    //    a diagnostic so a benign-looking line renders as something else entirely.
    //  - U+2028/U+2029 terminate a JavaScript string literal, so an unescaped one
    //    inside a diagnostic message can break out of generated JS.
    //  - U+FEFF (BOM / ZWNBSP) is invisible and can hide content in any consumer.

    /// T-16a: every bidi override / isolate / mark codepoint is escaped to its
    /// uppercase 6-char `\uXXXX` literal, and the raw codepoint is gone.
    #[test]
    fn sanitize_escapes_bidi_control_chars() {
        let cases: &[(char, &str)] = &[
            ('\u{200E}', "\\u200E"), // LEFT-TO-RIGHT MARK
            ('\u{200F}', "\\u200F"), // RIGHT-TO-LEFT MARK
            ('\u{202A}', "\\u202A"), // LEFT-TO-RIGHT EMBEDDING
            ('\u{202B}', "\\u202B"), // RIGHT-TO-LEFT EMBEDDING
            ('\u{202C}', "\\u202C"), // POP DIRECTIONAL FORMATTING
            ('\u{202D}', "\\u202D"), // LEFT-TO-RIGHT OVERRIDE
            ('\u{202E}', "\\u202E"), // RIGHT-TO-LEFT OVERRIDE (Trojan Source)
            ('\u{2066}', "\\u2066"), // LEFT-TO-RIGHT ISOLATE
            ('\u{2067}', "\\u2067"), // RIGHT-TO-LEFT ISOLATE
            ('\u{2068}', "\\u2068"), // FIRST STRONG ISOLATE
            ('\u{2069}', "\\u2069"), // POP DIRECTIONAL ISOLATE
        ];
        for &(ch, expected) in cases {
            let input = format!("a{ch}b");
            let out = sanitize_control_chars(&input);
            assert!(
                !out.contains(ch),
                "raw U+{:04X} must not survive sanitization; got: {out:?}",
                ch as u32
            );
            assert_eq!(
                &*out,
                format!("a{expected}b"),
                "U+{:04X} must escape to {expected}",
                ch as u32
            );
        }
    }

    /// T-16b: U+2028 LINE SEPARATOR and U+2029 PARAGRAPH SEPARATOR are escaped.
    /// Both terminate a JS string literal, so they must never reach a consumer raw.
    #[test]
    fn sanitize_escapes_line_and_paragraph_separators() {
        assert_eq!(&*sanitize_control_chars("a\u{2028}b"), "a\\u2028b");
        assert_eq!(&*sanitize_control_chars("a\u{2029}b"), "a\\u2029b");
    }

    /// T-16c: U+FEFF (BOM / ZWNBSP) is escaped — it is invisible in every renderer.
    #[test]
    fn sanitize_escapes_bom() {
        assert_eq!(&*sanitize_control_chars("a\u{FEFF}b"), "a\\uFEFFb");
    }

    /// T-16d: `neutralize_source_for_render` must handle the widened class
    /// symmetrically (PF-004: no wire/render parallel-path gap) while keeping the
    /// byte-length invariant the T-10a anchor pins. Every new codepoint is 3-byte
    /// UTF-8, and U+FFFD is also 3 bytes.
    #[test]
    fn neutralize_source_replaces_bidi_and_separators_byte_for_byte() {
        let raw = "let x\u{202E} = 1;\u{2028}next\u{FEFF}line\u{2066}end";
        let out = neutralize_source_for_render(raw);
        assert_eq!(
            out.len(),
            raw.len(),
            "neutralize_source_for_render must preserve byte length; raw={raw:?} out={out:?}"
        );
        for ch in ['\u{202E}', '\u{2028}', '\u{FEFF}', '\u{2066}'] {
            assert!(
                !out.contains(ch),
                "raw U+{:04X} must not survive neutralization; got: {out:?}",
                ch as u32
            );
        }
        assert_eq!(
            out.matches('\u{FFFD}').count(),
            4,
            "each neutralized format char must become U+FFFD; got: {out:?}"
        );
        // Non-vacuity: surrounding source text is untouched.
        assert!(out.contains("let x"), "clean source must survive: {out:?}");
        assert!(out.contains("next"), "clean source must survive: {out:?}");
    }

    /// T-16e [PF-013]: the RLO reversal vector reaches the wire through
    /// `to_canonical_json` and comes out escaped.
    ///
    /// Vector: a `duplicate-import` style message embedding an import path that
    /// carries U+202E. Without the guard the raw RLO reaches the JSON string value
    /// and any terminal/IDE renderer reverses the rest of the line.
    ///
    /// Non-vacuity guards: the diagnostic list is non-empty, the expected rule is
    /// present, and the POSITIVE assertion requires the escaped form to be there.
    #[test]
    fn to_canonical_json_escapes_bidi_override() {
        let hostile_path = "./fo\u{202E}gnp.mds";
        let result = LintResult {
            diagnostics: vec![LintDiagnostic {
                rule: "duplicate-import".to_string(),
                severity: Severity::Error,
                message: format!("Module '{hostile_path}' is imported more than once."),
                help: Some(format!("Remove the duplicate '{hostile_path}' import.")),
                span: Some(crate::error::SerializedSpan {
                    offset: 9,
                    length: 16,
                    line: None,
                    column: None,
                }),
                file: Some("ma\u{202E}in.mds".to_string()),
                fix_removals: None,
                fix_edits: None,
            }],
            truncated: false,
            is_standalone: false,
        };

        let json = result.to_canonical_json();
        let files = json["files"].as_array().expect("files array");
        assert_eq!(files.len(), 1, "non-vacuity: exactly one file group");
        let diags = files[0]["diagnostics"].as_array().expect("diagnostics");
        assert_eq!(diags.len(), 1, "non-vacuity: exactly one diagnostic");
        assert_eq!(
            diags[0]["rule"], "duplicate-import",
            "non-vacuity: expected rule must be present"
        );

        for field in ["message", "help"] {
            let s = diags[0][field].as_str().unwrap();
            assert!(
                !s.contains('\u{202E}'),
                "raw U+202E must not appear in {field}; got: {s:?}"
            );
            assert!(
                s.contains("\\u202E"),
                "escaped \\u202E must appear in {field}; got: {s:?}"
            );
        }

        // The file group key travels the same boundary.
        let file_key = files[0]["file"].as_str().unwrap();
        assert!(
            !file_key.contains('\u{202E}'),
            "raw U+202E must not appear in the file key; got: {file_key:?}"
        );
        assert!(
            file_key.contains("\\u202E"),
            "escaped \\u202E must appear in the file key; got: {file_key:?}"
        );

        // Span offsets stay byte-accurate against the raw source.
        assert_eq!(diags[0]["span"]["offset"], 9);
        assert_eq!(diags[0]["span"]["length"], 16);
    }

    /// T-16f: U+2028 in a diagnostic message is escaped on the wire — a raw one
    /// would terminate a JS string literal in any consumer that inlines the value.
    #[test]
    fn to_canonical_json_escapes_line_separator() {
        let result = LintResult {
            diagnostics: vec![LintDiagnostic {
                rule: "unused-variable".to_string(),
                severity: Severity::Warn,
                message: "Variable 'a\u{2028}b' is never referenced.".to_string(),
                help: None,
                span: None,
                file: Some("t.mds".to_string()),
                fix_removals: None,
                fix_edits: None,
            }],
            truncated: false,
            is_standalone: false,
        };
        let json = result.to_canonical_json();
        let msg = json["files"][0]["diagnostics"][0]["message"]
            .as_str()
            .expect("non-vacuity: message must be a string");
        assert!(
            !msg.contains('\u{2028}'),
            "raw U+2028 must not appear on the wire; got: {msg:?}"
        );
        assert!(
            msg.contains("\\u2028"),
            "escaped \\u2028 must appear on the wire; got: {msg:?}"
        );
    }

    /// T-16g: wire-mode newline escaping — log/YAML-key forging guard.
    ///
    /// A diagnostic message carrying an embedded newline can forge an extra
    /// "diagnostic" line in any line-oriented consumer of the JSON string value.
    /// On the WIRE the newline (U+000A) must become its 6-char escape literal; the
    /// HUMAN render path must keep it raw so multi-line miette frames stay readable.
    #[test]
    fn to_canonical_json_escapes_newline_but_human_mode_preserves_it() {
        let forged = "a\nerror[mds::forged]: FAKE\nb";
        let result = LintResult {
            diagnostics: vec![LintDiagnostic {
                rule: "unused-variable".to_string(),
                severity: Severity::Warn,
                message: forged.to_string(),
                help: Some(forged.to_string()),
                span: None,
                file: Some("t.mds".to_string()),
                fix_removals: None,
                fix_edits: None,
            }],
            truncated: false,
            is_standalone: false,
        };
        let json = result.to_canonical_json();
        let diag = &json["files"][0]["diagnostics"][0];

        for field in ["message", "help"] {
            let s = diag[field]
                .as_str()
                .unwrap_or_else(|| panic!("non-vacuity: {field} must be a string"));
            assert!(
                !s.contains('\n'),
                "raw newline must not survive into the wire {field}; got: {s:?}"
            );
            assert!(
                s.contains("\\u000A"),
                "escaped \\u000A must appear in the wire {field}; got: {s:?}"
            );
            // Non-vacuity: the surrounding text is preserved, only the newline changed.
            assert!(
                s.contains("error[mds::forged]"),
                "message body must be preserved verbatim; got: {s:?}"
            );
        }

        // Human mode keeps the newline — this is the render-path contract.
        let human = sanitize_control_chars(forged);
        assert!(
            human.contains('\n'),
            "human mode must preserve raw newlines; got: {human:?}"
        );
        assert!(
            !human.contains("\\u000A"),
            "human mode must not escape newlines; got: {human:?}"
        );
        // \t is preserved in BOTH modes.
        assert!(
            sanitize_control_chars("a\tb").contains('\t'),
            "human mode must preserve tabs"
        );
    }

    /// T-16h: the two modes differ on `\n` and on nothing else.
    ///
    /// Guards against the two ways the split could rot: WIRE forgetting a character
    /// HUMAN escapes (or vice versa), and WIRE escaping `\t` as collateral damage.
    #[test]
    fn wire_and_human_modes_differ_only_on_newline() {
        // \n: the one intentional divergence.
        assert_eq!(&*sanitize_control_chars_wire("a\nb"), "a\\u000Ab");
        assert_eq!(&*sanitize_control_chars("a\nb"), "a\nb");
        // \t: preserved by both.
        assert_eq!(&*sanitize_control_chars_wire("a\tb"), "a\tb");
        assert_eq!(&*sanitize_control_chars("a\tb"), "a\tb");
        // Everything else: identical output from both modes.
        for probe in [
            "a\x00b",
            "a\x1Bb",
            "a\u{007F}b",
            "a\u{0085}b",
            "a\u{200E}b",
            "a\u{202E}b",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{2069}b",
            "a\u{FEFF}b",
            "plain text",
        ] {
            assert_eq!(
                sanitize_control_chars(probe),
                sanitize_control_chars_wire(probe),
                "modes must agree on {probe:?}"
            );
        }
    }

    /// T-16i: WIRE mode keeps the [`sanitize_control_chars`] properties — borrowed
    /// on clean input (zero allocation) and idempotent.
    #[test]
    fn wire_mode_is_borrowed_when_clean_and_idempotent() {
        assert!(matches!(
            sanitize_control_chars_wire("normal text"),
            Cow::Borrowed(_)
        ));
        for input in ["\x1B", "a\nb", "a\u{202E}b", "a\u{FEFF}b", "clean", ""] {
            let once = sanitize_control_chars_wire(input);
            let twice = sanitize_control_chars_wire(&once);
            assert_eq!(once, twice, "wire mode not idempotent for {input:?}");
        }
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
