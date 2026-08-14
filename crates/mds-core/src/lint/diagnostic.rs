//! Lint diagnostic types: `LintDiagnostic`, `Severity`, `LintResult`.
//!
//! `LintDiagnostic` implements `std::error::Error` and `miette::Diagnostic` so it can
//! be rendered by miette at the CLI human-render boundary. The `severity()` override
//! maps our `Severity` enum to miette's rendering tiers (Error/Warning/Advice).
//!
//! **Sanitization discipline**: `message` and `help` are sanitized at every output
//! boundary. The escaped class — stated exactly as spec §7.5 states it — is C0
//! (U+0000–U+001F) **including `\n`**, with `\t` (U+0009) as the sole exemption; DEL
//! (U+007F); C1 (U+0080–U+009F); the complete Unicode `Bidi_Control=Yes` set — all twelve
//! of U+061C, U+200E/U+200F, U+202A–U+202E, U+2066–U+2069 (Trojan Source,
//! CVE-2021-42574) — the JS line/paragraph separators U+2028/U+2029, and U+FEFF;
//! each becomes an uppercase 6-char `\uXXXX` literal.
//!
//! `\n` is *in* the class. Whether a given boundary escapes it is the HUMAN/WIRE mode
//! choice below, not a property of the class. Describing the class as "C0 except
//! `\n`/`\t`" folds the mode into the class definition and makes the two documents
//! disagree about what the class contains; they must not.
//!
//! Two escape modes share one implementation, differing only on `\n`:
//!
//! - **HUMAN** ([`sanitize_control_chars`]) — `\n` preserved. For terminal / miette
//!   render output, where multi-line frames must stay readable.
//! - **WIRE** ([`sanitize_control_chars_wire`]) — `\n` escaped too, so a hostile
//!   message cannot forge an extra line in a line-oriented consumer of the value
//!   (log forging, YAML key injection).
//!
//! `\t` is preserved in both modes.
//!
//! **Mode is chosen per FIELD, not per surface** — the governing rule, re-ratified
//! 2026-07-26 and normative in spec §7.5:
//!
//! > On the **diagnostic** surfaces — the `"version": 1` JSON wire, CLI status and
//! > warning lines, and `[file:line:col]` frame headers — untrusted **identifiers,
//! > filenames and error causes are WIRE**, human terminal output included. **Prose** —
//! > a diagnostic message body or help body — stays **HUMAN** on terminal surfaces so
//! > multi-line frames keep rendering.
//!
//! The rule governs diagnostic output. Two categories of output are named carve-outs and
//! are not escaped at all — the command's **product** (compiled template output) and
//! **functional path references** (source-map `file`/`sources`/`sourcesContent`,
//! `CompileResult.dependencies`). Both are in "Scope of the table" below.
//!
//! The discriminator is whether the value is ever legitimately multi-line. A filename, an
//! `mds.json` rule name, a `--format` argument and an `io::Error` cause are each rendered
//! on exactly one line — a status line, or a `[file:line:col]` frame header — so a raw
//! `\n` in one only lets it forge a standalone line byte-identical in form to genuine
//! output (CWE-117); POSIX permits a newline inside a filename and the user never types
//! it. A diagnostic body genuinely is multi-line, so escaping its newlines would break
//! the frame.
//!
//! This rule supersedes the earlier "wire mode at exactly these four boundaries"
//! enumeration. Enumerations of boundaries went stale twice under review; a per-field
//! rule makes each new site decidable without re-deriving the list.
//!
//! ## Boundary table
//!
//! This table is an audit list of the sanitizing boundaries, not a proof of closure.
//! Read it together with "Scope of the table" below, which names the one category of
//! CLI output that is deliberately outside it.
//!
//! | Boundary | Mode | Fields |
//! |----------|------|--------|
//! | `eprint_error` (mds-cli/src/output.rs) | HUMAN | `message`, `help`, `LabeledSpan` text, and the whole auxiliary diagnostic graph (`source` cause chain, `related`, `diagnostic_source`) of **every** report rendered to stderr — see "CLI terminal path" below |
//! | `eprint_warning` (mds-cli/src/output.rs) | **prose HUMAN, interpolated identifiers/paths WIRE** | the warning body is escaped HUMAN by the helper; each value interpolated into it must additionally be WIRE-escaped by the caller (`safe_path` for paths, `safe_inline` for identifiers / config values / `io::Error` causes). HUMAN alone is not sufficient — it preserves `\n`, which is the CWE-117 forgery vector |
//! | `safe_inline()` (mds-cli/src/output.rs) | WIRE | any single-line untrusted value interpolated into a status, warning or error line: `mds.json` rule names and config paths, `--format` arguments, fix-rejection reasons, `io::Error` causes |
//! | `tests/print_discipline.rs` (mds-cli) | *enforcement, not a boundary* | fails CI if any print macro under `crates/mds-cli/src/**` interpolates a value that is not passed through one of the escape helpers, and applies the same rule to `format!`s nested inside `eprint_warning` calls. Exceptions live in an explicit allowlist with per-entry justifications |
//! | `emit_warnings()` (lib.rs) | HUMAN | warning strings printed to stderr on the non-collecting paths. The identifiers its producers interpolate — `resolver.rs`'s imported-module filename, `evaluator.rs`'s `@include` alias — are WIRE-escaped at construction, per the per-field rule |
//! | `named_source_for_render()` (this module) | **per field** | the single `NamedSource` builder used by `MdsError::at()` (error.rs), `check_equivalence` (formatter.rs) and `render_diag_human` (mds-cli/src/lint.rs): filename WIRE, source via `neutralize_source_for_render` (byte-length-preserving; avoids PF-014 caret desync) |
//! | `render_diag_human` (mds-cli/src/lint.rs) | HUMAN | `message`/`help` (its filename and source go through `named_source_for_render`) |
//! | `safe_path()` / `safe_file_display()` (mds-cli/src/output.rs) | WIRE | CLI status-line path display (`Clean:`, `Fixed:`, `Would fix:`, `Compiled to`, …) |
//! | `fix::FixOutcome::Rejected.reason` (fix.rs) | WIRE | construction-time: the `MdsError` `Display` embedded in a reverify-failure reason, so the value is display-safe for every consumer of the published `mds::fix` API (PF-004) |
//! | `MdsError::serialize()` (error.rs) | WIRE | `message`, `help` — covers all three bindings' error path |
//! | `LintResult::to_canonical_json()` (this module) | WIRE | `message`, `help`, `files[].file` key |
//! | `CompileResult::to_canonical_json()` (lib.rs) | WIRE | warning strings; *distinct method from `LintResult::to_canonical_json`, not a duplicate* |
//! | Python `LintResult::new()` via `sanitize_lint_value()` | WIRE | `message`, `help`, `file` — construction-time, so typed getters read pre-sanitized data (PF-004) |
//! | `--diff` preview output (mds-cli/src/output.rs) | neutralized, TTY-gated | source excerpts neutralized when stdout is a TTY; byte-faithful when piped, so redirected diffs stay applicable. **`--check` alone emits no preview text** — only status lines, which are unconditionally sanitized via `safe_path`. |
//!
//! **Scope of the table.** It covers every path that carries *untrusted text* —
//! diagnostic prose, warnings, filenames, identifiers, and rejection reasons.
//!
//! `watch.rs`'s lifecycle status lines (`Watching {}`, `Removed {} (source deleted)`,
//! `warning: could not remove {}: {e}`) were previously carved out here as a
//! pre-existing gap. **That carve-out is gone**: they now route through `safe_path` /
//! `safe_inline` / `eprint_warning` like every other CLI print, because leaving them out
//! would have meant an allowlist entry in `print_discipline.rs` — a deliberate hole in
//! the guard rather than a documented one.
//!
//! Three categories remain outside the table, deliberately and without a coverage claim:
//!
//! - **Untrusted values interpolated into a diagnostic MESSAGE BODY**, at either of its
//!   two construction sites:
//!   - `miette::miette!(…)` in the CLI, which interpolates `mds.json` values and
//!     filesystem paths; and
//!   - **`MdsError` message bodies in this crate**, which interpolate paths and
//!     `io::Error` causes (`fs.rs`'s `cannot read {normalized}: {e}` and `invalid UTF-8
//!     in {normalized}: {e}`) and template identifiers (`parser_helpers.rs`'s `invalid
//!     import alias: '{alias}'`).
//!
//!   Both are rendered through `eprint_error`, which escapes message, help and label text
//!   in HUMAN mode before miette sees them, so no raw control byte reaches stderr — but a
//!   `\n` in an interpolated path or identifier survives *inside the rendered frame*.
//!   Frame content is indented and `│`-prefixed by the renderer rather than emitted as a
//!   bare status line, and that prefix survives `strip()`, so forged frame content cannot
//!   masquerade as genuine status output the way an unescaped filename in a `Clean: …`
//!   line could. It is a weaker surface than the status lines above — a known residual,
//!   not a closed one, disclosed here and in spec §7.5 ("Residual: paths and identifiers
//!   inside a message body").
//!
//!   Note the asymmetry this preserves: a path in a **diagnostic** `file` field — a
//!   status line, a `[file:line:col]` header, the JSON `file` key — *is* WIRE-escaped on
//!   every surface that renders one. The residual is specifically a path or identifier
//!   that has been interpolated into prose, where the per-field rule makes the
//!   surrounding body HUMAN.
//! - **Compiled template output** (`mds build -o -`, `mds lint --fix -`). That is the
//!   command's product, not a diagnostic; escaping it would corrupt every redirect.
//! - **Functional path references**: the source-map `file`, `sources` and
//!   `sourcesContent` fields — in the `mds build --source-map` sidecar and in the
//!   `sourceMap` object embedded in [`crate::CompileResult::to_canonical_json`] — and the
//!   `dependencies` array of that same method. These are emitted **verbatim**: a
//!   filename containing a control byte, a `\n` or a bidi control reaches the consumer
//!   unmodified, and JSON string encoding is not escaping (a decoded `"\n"` is a real
//!   newline again). Devtools, bundlers and IDEs resolve `file` / `sources` against the
//!   filesystem and the bundler plugins watch `dependencies`, so rewriting a path to a
//!   `\uXXXX` literal would point at a path that does not exist — breaking resolution to
//!   defend against a pathological filename. **Consumers MUST treat these paths as
//!   untrusted and escape them for whatever destination they render them to.** Specified
//!   in spec §7.5 ("Carve-out: functional path references"). The CLI does not depend on
//!   this contract for its own output: `Compiled to …` and `Source map written to …`
//!   print through `safe_path` and so carry the WIRE-escaped form.
//!
//! **CLI terminal path.** `mds build` / `check` / `fmt` / `lint` / `watch` all render
//! errors through the single `eprint_error` choke-point, which wraps the `miette::Report`
//! in a sanitizing view *before* miette renders it. This covers both CLI error families:
//! `MdsError` (whose messages interpolate template text, e.g. `parser.rs`'s
//! `invalid include alias: '{alias}'`) and CLI-authored `miette::miette!()` reports
//! (which interpolate `mds.json` values and filesystem paths, and do **not** downcast to
//! `MdsError`). Because it wraps at the `Report` level, an error type added later inherits
//! the guarantee without touching the boundary — the PF-004 failure mode of a check that
//! holds on one path and silently lapses on a sibling path cannot recur here.
//!
//! Sanitizing the renderer's *inputs* is mandatory; sanitizing its *output* is forbidden.
//! Running an escaper over an already-rendered miette frame escapes miette's own ANSI SGR
//! codes into literal noise on any colour-capable TTY and desynchronises caret alignment,
//! on entirely benign input — and CI cannot catch it, because the CLI tests pin
//! `NO_COLOR=1` and pipe stderr. That is PF-014, and an earlier round of #176 shipped and
//! reverted exactly that defect. The colour path is pinned instead by in-process unit
//! tests that select a theme explicitly (`output.rs`, `sanitize_report_*`).
//!
//! **Deliberate exclusions** (documented, not gaps):
//! - `LintDiagnostic::fmt` and `MdsError`'s derived `Display` (raw) — unsanitized by
//!   design so machine-readable pipelines see exact bytes. No in-tree path renders
//!   either one to a terminal *unescaped*: diagnostics go through `eprint_error`, and
//!   the one place that embeds an `MdsError` `Display` into another user-visible string
//!   — `fix::FixOutcome::Rejected.reason` — escapes it at construction (see the table).
//!   Downstream Rust consumers of the published crate that print an `MdsError`
//!   themselves should use [`MdsError::display_sanitized`], which applies this module's
//!   HUMAN mode to the `Display` string. That helper is a **consumer-facing API, not a
//!   CLI boundary** — the CLI's own guarantee comes from `eprint_error`.
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
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Construct via [`TextEdit::new`]; do not use a struct literal.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// Inclusive start byte offset of the range to replace.
    pub start: usize,
    /// Exclusive end byte offset of the range to replace.
    pub end: usize,
    /// Replacement text (empty string for pure deletion).
    pub new_text: String,
}

impl TextEdit {
    /// Construct a `TextEdit` with all fields.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::TextEdit;
    /// let edit = TextEdit::new(6, 12, "{{name}}");
    /// assert_eq!(edit.start, 6);
    /// assert_eq!(edit.end, 12);
    /// assert_eq!(edit.new_text, "{{name}}");
    /// ```
    #[must_use]
    pub fn new(start: usize, end: usize, new_text: impl Into<String>) -> Self {
        debug_assert!(start <= end, "TextEdit::new: start ({start}) > end ({end})");
        TextEdit {
            start,
            end,
            new_text: new_text.into(),
        }
    }
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
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Construct via [`FixLineSpan::single`] (single-line),
/// [`FixLineSpan::range_inclusive`] (multi-line, inclusive), or
/// [`FixLineSpan::range_exclusive`] (multi-line, exclusive);
/// external crates must not use a struct literal.
#[non_exhaustive]
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
    /// Equivalent to `FixLineSpan::range_inclusive(offset, offset)`.
    #[must_use]
    pub fn single(offset: usize) -> Self {
        FixLineSpan {
            from: offset,
            to: offset,
            to_inclusive: true,
        }
    }

    /// Remove a range of lines **including** the line containing `to`.
    ///
    /// Both `from` and `to` are byte offsets within their respective lines.
    /// The planner translates them to exact line boundaries.
    ///
    /// # Panics (debug)
    ///
    /// Panics in debug builds when `from > to`.
    #[must_use]
    pub fn range_inclusive(from: usize, to: usize) -> Self {
        debug_assert!(
            from <= to,
            "FixLineSpan::range_inclusive: from ({from}) > to ({to})"
        );
        FixLineSpan {
            from,
            to,
            to_inclusive: true,
        }
    }

    /// Remove a range of lines **excluding** the line containing `to`.
    ///
    /// The line containing `to` is kept; removal stops at the start of that line.
    /// Use this when a closing token (e.g. `@end`) must remain in the source.
    ///
    /// # Panics (debug)
    ///
    /// Panics in debug builds when `from > to`.
    #[must_use]
    pub fn range_exclusive(from: usize, to: usize) -> Self {
        debug_assert!(
            from <= to,
            "FixLineSpan::range_exclusive: from ({from}) > to ({to})"
        );
        FixLineSpan {
            from,
            to,
            to_inclusive: false,
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
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Obtain values from `mds::lint_str`, `mds::lint`, and similar lint API functions, or
/// construct via [`LintDiagnostic::new`] and the `with_*` builder methods; do not
/// construct via struct literal.
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
#[non_exhaustive]
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

impl LintDiagnostic {
    /// Construct a `LintDiagnostic` with required fields; all optional fields default
    /// to `None`.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::{LintDiagnostic, Severity};
    /// let diag = LintDiagnostic::new("unused-variable", Severity::Warn, "Variable 'x' is unused");
    /// assert_eq!(diag.rule, "unused-variable");
    /// assert_eq!(diag.severity, Severity::Warn);
    /// assert!(diag.help.is_none());
    /// ```
    #[must_use]
    pub fn new(rule: impl Into<String>, severity: Severity, message: impl Into<String>) -> Self {
        LintDiagnostic {
            rule: rule.into(),
            severity,
            message: message.into(),
            help: None,
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        }
    }

    /// Set the help text for this diagnostic.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::{LintDiagnostic, Severity};
    /// let diag = LintDiagnostic::new("unused-variable", Severity::Warn, "Variable 'x' is unused")
    ///     .with_help("Remove the frontmatter key or reference it in the template body.");
    /// assert!(diag.help.is_some());
    /// ```
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Set the source span for this diagnostic.
    #[must_use]
    pub fn with_span(mut self, span: crate::SerializedSpan) -> Self {
        self.span = Some(span);
        self
    }

    /// Set the source file path for this diagnostic.
    #[must_use]
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Set line-removal fix spans for this diagnostic.
    #[must_use]
    pub fn with_fix_removals(mut self, fix_removals: Vec<FixLineSpan>) -> Self {
        self.fix_removals = Some(fix_removals);
        self
    }

    /// Set in-place replacement edits for this diagnostic.
    #[must_use]
    pub fn with_fix_edits(mut self, fix_edits: Vec<TextEdit>) -> Self {
        self.fix_edits = Some(fix_edits);
        self
    }

    /// Return a sanitized clone of this diagnostic for use at the miette render boundary.
    ///
    /// Sanitizes `message` and `help` in HUMAN mode (preserves `\n`; escapes C0/DEL/C1
    /// controls, bidi controls, and BOM — see module-level "Sanitization discipline").
    /// Preserves `rule`, `severity`, `span`, and `file` unchanged. Intentionally sets
    /// `fix_removals` and `fix_edits` to `None` — miette's `Diagnostic` impl does not
    /// read those fields, so cloning them would waste allocations (architecture-3 /
    /// rust-1).
    ///
    /// This method is the architecturally correct home for render-boundary sanitization
    /// (PF-014): the CLI should not assemble sanitized copies itself. It owns the escape
    /// logic because it lives alongside the sanitizers and the struct definition.
    ///
    /// **Behavior is byte-identical to the original CLI render path** — this is
    /// security-critical code (#176 / CWE-150). Do not alter the escape mode or field
    /// selection without a corresponding audit of all render boundaries.
    ///
    /// # Escaping is one-way
    ///
    /// The HUMAN-mode escaping applied to `message` and `help` is **lossy and
    /// non-injective** — see the `# Escaping is one-way` section on
    /// [`sanitize_control_chars`]. Callers **MUST NOT** un-escape `\uXXXX` sequences
    /// back into bytes.
    ///
    /// # Result is render-only — do not serialize to a WIRE surface
    ///
    /// The returned diagnostic has `message` and `help` sanitized in **HUMAN mode**,
    /// which preserves `\n`. Passing this value to `LintResult::to_canonical_json` or
    /// any JSON / binding serializer would double-escape already-escaped text and, more
    /// importantly, would emit raw newlines inside JSON string values — violating the
    /// WIRE contract (log forging, YAML key injection). For JSON/binding output, call
    /// `to_canonical_json()` on the *original* `LintResult` instead; it applies WIRE
    /// mode at the correct boundary.
    ///
    /// # Fix data is intentionally stripped
    ///
    /// `fix_removals` and `fix_edits` are set to `None` in the returned value. A caller
    /// that reuses the sanitized clone for the fix pipeline would silently lose every
    /// `fixable` flag and every edit plan — the `fixable` field in the JSON wire format
    /// is derived from `fix_removals`/`fix_edits`, so it would always serialize as
    /// `false`.
    #[must_use]
    pub fn sanitized_for_render(&self) -> Self {
        LintDiagnostic {
            rule: self.rule.clone(),
            severity: self.severity,
            message: sanitize_control_chars(&self.message).into_owned(),
            help: self
                .help
                .as_deref()
                .map(|h| sanitize_control_chars(h).into_owned()),
            span: self.span.clone(),
            file: self.file.clone(),
            fix_removals: None,
            fix_edits: None,
        }
    }
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
///
/// This type is `#[non_exhaustive]`: new fields may be added in minor releases.
/// Obtain values from `mds::lint_str`, `mds::lint`, and similar lint API functions,
/// or construct via [`LintResult::new`] (and chain [`.truncated()`] / [`.standalone()`]);
/// do not construct via struct literal.
#[non_exhaustive]
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
    /// Construct a `LintResult` from a diagnostic list.
    ///
    /// Defaults: `truncated = false`, `is_standalone = false`.
    /// Chain [`.truncated()`] or [`.standalone()`] to override.
    ///
    /// This is the supported construction path for external crates — struct literals
    /// are not available because this type is `#[non_exhaustive]`.
    ///
    /// # Ordering
    ///
    /// **AD-202-1b:** this constructor does NOT sort because `LintResult::new`
    /// is a public, ADR-010 construction path.  Sorting here would silently
    /// reorder an external caller's deliberately-ordered `Vec<LintDiagnostic>`,
    /// which is a semantic change to a published API.  The internal builder
    /// (`LintResultBuilder::build`) sorts after truncation, so results produced
    /// by `lint`/`lint_source` always carry the canonical offset order.
    /// A `LintResult` built via this constructor preserves caller order on all
    /// surfaces — [`to_canonical_json`] included — because `sort_diagnostics`
    /// is the single ordering choke point (AD-202-1) and it is not called here.
    ///
    /// # Examples
    ///
    /// ```
    /// use mds::LintResult;
    /// let result = LintResult::new(vec![]).standalone();
    /// assert!(result.diagnostics.is_empty());
    /// assert!(!result.truncated);
    /// assert!(result.is_standalone);
    /// ```
    #[must_use]
    pub fn new(diagnostics: Vec<LintDiagnostic>) -> Self {
        LintResult {
            diagnostics,
            truncated: false,
            is_standalone: false,
        }
    }

    /// Mark this result as truncated (the diagnostic cap was reached).
    #[must_use]
    pub fn truncated(mut self) -> Self {
        self.truncated = true;
        self
    }

    /// Mark this result as standalone (the file has no `@import` or `@extends`).
    ///
    /// Standalone status gates Tier B `--fix` eligibility.
    #[must_use]
    pub fn standalone(mut self) -> Self {
        self.is_standalone = true;
        self
    }

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
    ///           "span": { "offset": 0, "length": 5 },
    ///           "fix_edits": [{ "start": 0, "end": 7, "new_text": "" }]
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
    /// Some(..)`).  Note: `"<unknown>"` sorts before alphabetic paths in the JSON
    /// `files` array (BTreeMap, `<` precedes letters), which disagrees with the
    /// ordering of `LintResult.diagnostics` where file-less entries sort last —
    /// this is academic because no lint rule emits `file: None`.
    /// The `fixable` field reflects tier semantics: `true` for Tier A rules and for
    /// Tier B rules when the file is standalone, `false` otherwise.
    ///
    /// Absence conventions: `"help"`, `"span"`, and `"fix_edits"` are JSON `null`
    /// when the field is `None`.  Within a present `"span"` object, `"offset"` and
    /// `"length"` are always present; `"line"` and `"column"` are **omitted** (the
    /// key is absent, not `null`) when not set.  No current lint rule sets `line`
    /// or `column`, so live output contains only `"offset"` and `"length"`.  The
    /// `mds-python` binding also conditionally emits `"line"` and `"column"` when set.
    ///
    /// **AD-202-1 (ordering guarantee):** within each file group, diagnostics are
    /// ordered by ascending byte offset (`span.offset`).  Diagnostics without a span
    /// sort to the end of the group. The sort is stable so equal-offset diagnostics
    /// preserve rule-insertion order.  Within this function, file groups are ordered
    /// lexicographically by the **raw** `diag.file` value (BTreeMap / byte-wise
    /// string order), while the emitted `"file"` string is the WIRE-sanitized form
    /// of that key — the two differ only for a file name carrying a control, bidi or
    /// separator character, and only an external caller can reach that case, because
    /// every engine entry point (`lint`, `lint_str`, `lint_source`, `lint_virtual`)
    /// passes exactly one filename, so an engine-produced `LintResult` always has
    /// exactly one file group.  The CLI directory mode processes one file per
    /// `LintResult` and sorts the outer `files[]` array itself, on the **sanitized**
    /// relative display path, so there its array position always matches its emitted
    /// key order (AC-P1-10).
    /// A `LintResult` assembled by an external caller via [`LintResult::new`] is
    /// emitted in the order that caller supplied — see that constructor's
    /// `# Ordering` note.
    ///
    /// **AD-202-2 (truncation is NOT offset-ranked):** when `truncated` is `true`
    /// the retained diagnostics are the first `MAX_DIAGNOSTICS` in *rule-execution*
    /// order, re-sorted by offset afterwards.  They are **not** the
    /// `MAX_DIAGNOSTICS` smallest offsets in the file.
    ///
    /// **NEVER** build this JSON via `format!()` — use `serde_json::json!()` so
    /// control characters in message/help are serialized safely.
    #[must_use]
    pub fn to_canonical_json(&self) -> serde_json::Value {
        use std::collections::BTreeMap;

        // Group diagnostics by file, preserving insertion order within each group.
        // BTreeMap gives deterministic (sorted) file ordering in the output.
        // AD-202-1: order is established by LintResultBuilder::build (for engine
        // results) or preserved as caller-supplied (for LintResult::new callers).
        // sort_diagnostics is the single ordering choke point; no re-sort here.
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
        let mut diagnostics = self.diagnostics;
        // AD-202-2: the sort runs AFTER truncation, deliberately. `push` enforces the
        // MAX_DIAGNOSTICS cap in rule-execution order, so `build` reorders only the
        // already-retained set — it never changes WHICH diagnostics are retained.
        sort_diagnostics(&mut diagnostics);
        LintResult {
            diagnostics,
            truncated: self.truncated,
            is_standalone,
        }
    }
}

// ── Diagnostic ordering ───────────────────────────────────────────────────────

/// Sort key for one diagnostic: `(file-is-absent, file, span-is-absent, offset)`.
///
/// Every component borrows from `diag`, so the comparator allocates nothing
/// (AC-P1-22). The two leading `bool`s make "absent sorts last" explicit rather
/// than encoding it as a magic sentinel value: `false < true`, so `Some(..)`
/// always precedes `None` without relying on any particular string or integer
/// comparing greater than every real value.
fn sort_key(diag: &LintDiagnostic) -> (bool, &str, bool, usize) {
    let (file_absent, file) = match diag.file.as_deref() {
        Some(f) => (false, f),
        None => (true, ""),
    };
    let (span_absent, offset) = match diag.span.as_ref() {
        Some(s) => (false, s.offset),
        None => (true, 0),
    };
    (file_absent, file, span_absent, offset)
}

/// Sort `diagnostics` in-place by `(file, span.offset)` — ascending, stable.
///
/// **AD-202-1:** the single ordering choke point, called from
/// `LintResultBuilder::build` so every surface (CLI human, CLI JSON, napi, WASM,
/// Python) inherits the same order from `LintResult.diagnostics` itself rather
/// than each renderer sorting its own copy (avoids PF-007).
///
/// **AD-202-1 (stability is load-bearing):** the sort is stable (`sort_by`, not
/// `sort_unstable_by`) so equal-offset diagnostics preserve the insertion order
/// established by the fixed 10-call `run_rules` dispatch sequence — rule tests
/// that assert exact ordering remain deterministic.
///
/// **AD-202-2:** callers must invoke this AFTER truncation. Sorting reorders the
/// retained set only; it never changes which diagnostics were retained. The
/// result is NOT "the first `MAX_DIAGNOSTICS` by offset".
///
/// Diagnostics without a span sort to the end of their file group; diagnostics
/// without a file sort to the end of the overall list. See [`sort_key`].
fn sort_diagnostics(diagnostics: &mut [LintDiagnostic]) {
    diagnostics.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
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
/// Escapes the full class — C0 (U+0000–U+001F incl. ESC) with `\t` exempt, DEL
/// (U+007F), C1 (U+0080–U+009F), all twelve Unicode bidi controls (U+061C,
/// U+200E/U+200F, U+202A–U+202E, U+2066–U+2069), the line/paragraph separators
/// U+2028/U+2029, and U+FEFF — **except `\n`, which this mode preserves**. `\n` is in
/// the class; HUMAN mode is the choice not to escape it. See the module doc.
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
/// // So is U+061C ARABIC LETTER MARK — the one Bidi_Control codepoint outside
/// // U+200E–U+2069, and the only 2-byte member of the class.
/// assert_eq!(&*sanitize_control_chars("a\u{061C}b"), "a\\u061Cb");
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
    // - U+061C is encoded as 0xD8 0x9C.
    // - U+200E/U+200F, U+2028/U+2029, U+202A–U+202E and U+2066–U+2069 all start
    //   with 0xE2; U+FEFF starts with 0xEF.
    // The scan is a deliberate over-approximation (0xC2/0xD8/0xE2/0xEF also lead many
    // benign codepoints); false positives only cost a trip through the char loop
    // below, which leaves non-hostile characters unchanged.
    let needs_work = s
        .bytes()
        .any(|b| b < 0x20 || b == 0x7F || b == 0xC2 || b == 0xD8 || b == 0xE2 || b == 0xEF);
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
/// The class is split by **UTF-8 byte width**, not by hazard category, because
/// [`neutralize_source_for_render`] must substitute a replacement of identical byte
/// length and therefore needs a different replacement per width. Splitting the
/// predicate is what keeps that invariant checkable by reading the code rather than
/// by trusting a comment: a member added to the wrong helper is a byte-width bug the
/// `debug_assert_eq!` in `neutralize_source_for_render` catches immediately.
///
/// See [`is_two_byte_format_hazard`] and [`is_three_byte_format_hazard`] for the
/// per-width membership and the rationale for each codepoint.
#[inline]
fn is_format_hazard_char(ch: char) -> bool {
    is_two_byte_format_hazard(ch) || is_three_byte_format_hazard(ch)
}

/// Display-hazardous codepoints that occupy **2 bytes** in UTF-8 (U+0080–U+07FF).
///
/// - **U+061C** — ARABIC LETTER MARK. One of the twelve codepoints with the Unicode
///   `Bidi_Control=Yes` property, and the only one outside the U+200E–U+2069 range.
///   It reorders how the rest of a line renders exactly like its U+200E/U+200F
///   siblings (Trojan Source, CVE-2021-42574), so omitting it would leave a hole in
///   the bidi class that the other eleven members close.
///
/// Members here are neutralized to U+00A0 NBSP (also 2 bytes), the same replacement
/// the C1 range uses — **not** U+FFFD, which is 3 bytes and would break the
/// byte-length invariant.
#[inline]
fn is_two_byte_format_hazard(ch: char) -> bool {
    ch == '\u{061C}'
}

/// Display-hazardous codepoints that occupy **3 bytes** in UTF-8 (U+0800–U+FFFF).
///
/// - **U+200E/U+200F, U+202A–U+202E, U+2066–U+2069** — the remaining eleven Unicode
///   bidirectional controls (marks, embeddings, overrides, isolates). They reorder how
///   the rest of a line renders, which is the Trojan Source attack (CVE-2021-42574): a
///   diagnostic or filename can be made to display as something entirely different from
///   its bytes.
/// - **U+2028/U+2029** — LINE SEPARATOR / PARAGRAPH SEPARATOR. Both terminate a
///   JavaScript string literal, so an unescaped one can break out of generated JS.
/// - **U+FEFF** — BOM / ZERO WIDTH NO-BREAK SPACE. Invisible in every renderer, so it
///   can hide or split content the reader believes is contiguous.
///
/// Members here are neutralized to U+FFFD (also 3 bytes).
#[inline]
fn is_three_byte_format_hazard(ch: char) -> bool {
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
/// - C1 range (U+0080–U+009F) and U+061C (both 2-byte UTF-8): → U+00A0 NBSP (2 bytes)
/// - Remaining bidi controls, U+2028/U+2029, U+FEFF (3-byte UTF-8): → U+FFFD (3 bytes)
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
        } else if (0x80..=0x9F).contains(&u) || is_two_byte_format_hazard(c) {
            // 2-byte C1 / U+061C → U+00A0 NBSP (2 bytes) — byte-length-preserving.
            out.push('\u{00A0}');
        } else if is_three_byte_format_hazard(c) {
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

/// Build the [`miette::NamedSource`] for a diagnostic frame, applying the sanitization
/// each of its two halves requires.
///
/// The two halves need **different** treatments, and getting them the wrong way round
/// is a real defect in both directions — which is why every in-tree site that hands a
/// filename plus source text to miette goes through this one function instead of
/// open-coding the pair (avoids PF-004 parallel-path drift):
///
/// - **`file`** — [`sanitize_control_chars_wire`] (WIRE). A filename is prose that is
///   rendered on a single line, both in miette's `[file:line:col]` frame header and in
///   the CLI's own status lines. POSIX permits `\n` inside a filename, so HUMAN mode —
///   which preserves `\n` so multi-line diagnostic *messages* stay readable — would let
///   a file named `evil.mds\nClean: real.mds` emit an attacker-authored line that is
///   byte-identical in form to genuine status output (CWE-117 log forging). A filename
///   is never legitimately multi-line, so escaping `\n` costs nothing.
/// - **`source`** — [`neutralize_source_for_render`] (byte-length-preserving). The
///   source text is span-indexed: `sanitize_control_chars*` expands a 1–2-byte control
///   to a 6-byte `\uXXXX` literal and desynchronises every following span offset and
///   caret column (PF-014).
///
/// Both halves are sanitized *before* the `Report` is built. The rendered frame is
/// never post-processed — see the module-level "Sanitization discipline" note.
#[must_use]
pub fn named_source_for_render(file: &str, source: &str) -> miette::NamedSource<String> {
    miette::NamedSource::new(
        sanitize_control_chars_wire(file).as_ref(),
        neutralize_source_for_render(source).into_owned(),
    )
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
    //  - The twelve Unicode `Bidi_Control=Yes` codepoints — U+061C, U+200E/U+200F,
    //    U+202A–U+202E and U+2066–U+2069 — are the controls behind Trojan Source
    //    (CVE-2021-42574): they can visually reorder a diagnostic so a benign-looking
    //    line renders as something else entirely.
    //  - U+2028/U+2029 terminate a JavaScript string literal, so an unescaped one
    //    inside a diagnostic message can break out of generated JS.
    //  - U+FEFF (BOM / ZWNBSP) is invisible and can hide content in any consumer.

    /// The complete Unicode `Bidi_Control=Yes` set (12 codepoints) with the escaped
    /// literal each must produce. Shared by the escape and neutralize tests so the two
    /// paths are pinned against one list and cannot diverge on a member.
    const BIDI_CONTROLS: &[(char, &str)] = &[
        ('\u{061C}', "\\u061C"), // ARABIC LETTER MARK (2 bytes in UTF-8)
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

    /// T-16a: every bidi override / isolate / mark codepoint is escaped to its
    /// uppercase 6-char `\uXXXX` literal, and the raw codepoint is gone.
    ///
    /// Covers the whole `Bidi_Control=Yes` property, including U+061C — the only member
    /// outside U+200E–U+2069, and the one the class originally missed (#176).
    #[test]
    fn sanitize_escapes_bidi_control_chars() {
        // Non-vacuity: the table really is the complete Unicode property, not a subset
        // that happens to match whatever the implementation covers.
        assert_eq!(
            BIDI_CONTROLS.len(),
            12,
            "Unicode defines exactly 12 Bidi_Control=Yes codepoints"
        );
        for &(ch, expected) in BIDI_CONTROLS {
            for out in [
                sanitize_control_chars(&format!("a{ch}b")),
                sanitize_control_chars_wire(&format!("a{ch}b")),
            ] {
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
    }

    /// T-16a-WIDTH [#176]: `neutralize_source_for_render` preserves byte length for
    /// every bidi control — the invariant the widened class most easily breaks.
    ///
    /// U+061C is 2 bytes in UTF-8 while the other eleven are 3. Routing it through the
    /// 3-byte branch (→ U+FFFD) would grow the string by one byte per occurrence,
    /// desynchronising every following span offset. The `debug_assert_eq!` inside
    /// `neutralize_source_for_render` fires on that; this test pins it from outside so
    /// the guarantee is also checked as an observable output property.
    #[test]
    fn neutralize_preserves_byte_length_for_every_bidi_control() {
        for &(ch, _) in BIDI_CONTROLS {
            let raw = format!("let x{ch} = 1;");
            let out = neutralize_source_for_render(&raw);
            assert_eq!(
                out.len(),
                raw.len(),
                "U+{:04X} ({} bytes) must be replaced by a same-width substitute; got: {out:?}",
                ch as u32,
                ch.len_utf8()
            );
            assert!(
                !out.contains(ch),
                "raw U+{:04X} must not survive neutralization; got: {out:?}",
                ch as u32
            );
            // Positive: the width-appropriate replacement, not merely "something else".
            let expected = if ch.len_utf8() == 2 {
                '\u{00A0}'
            } else {
                '\u{FFFD}'
            };
            assert!(
                out.contains(expected),
                "U+{:04X} must neutralize to U+{:04X}; got: {out:?}",
                ch as u32,
                expected as u32
            );
            // Non-vacuity: the surrounding source is untouched.
            assert!(
                out.contains("let x") && out.contains(" = 1;"),
                "non-vacuity: surrounding source must survive; got: {out:?}"
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

    // ── T-NS: named_source_for_render — the shared filename/source boundary (#176) ──

    /// T-NS-1 [S14 / CWE-117 / PF-013]: a newline-bearing FILENAME is escaped, so it
    /// cannot forge an extra line in miette's `[file:line:col]` frame header.
    ///
    /// Vector: POSIX permits `\n` inside a filename, and the user never types the name —
    /// `mds lint .` discovers it by directory walk. HUMAN mode (which the filename used
    /// to get) preserves newlines by design, so the forged text survived verbatim.
    #[test]
    fn named_source_escapes_newline_in_filename() {
        let hostile = "evil.mds\nClean: real.mds";
        let ns = named_source_for_render(hostile, "body\n");

        // Non-vacuity: the real part of the filename is still there.
        assert!(
            ns.name().contains("evil.mds"),
            "non-vacuity: the filename must still render; got: {:?}",
            ns.name()
        );
        // Negative: no raw newline survives, so no second line can be forged.
        assert!(
            !ns.name().contains('\n'),
            "a filename must be single-line after sanitization; got: {:?}",
            ns.name()
        );
        // Positive: escaped to the uppercase 6-char literal (WIRE mode).
        assert!(
            ns.name().contains("\\u000A"),
            "the newline must be escaped to its \\u000A literal; got: {:?}",
            ns.name()
        );
    }

    /// T-NS-2 [#176]: the same helper escapes the widened hazard class in the filename,
    /// including U+061C, and still escapes ESC.
    #[test]
    fn named_source_escapes_hazard_class_in_filename() {
        let ns = named_source_for_render("a\u{1b}b\u{061C}c\u{202E}d.mds", "body\n");
        for (raw, escaped) in [
            ('\u{1b}', "\\u001B"),
            ('\u{061C}', "\\u061C"),
            ('\u{202E}', "\\u202E"),
        ] {
            assert!(
                !ns.name().contains(raw),
                "raw U+{:04X} must not survive in a filename; got: {:?}",
                raw as u32,
                ns.name()
            );
            assert!(
                ns.name().contains(escaped),
                "U+{:04X} must escape to {escaped}; got: {:?}",
                raw as u32,
                ns.name()
            );
        }
    }

    /// T-NS-3 [PF-014 / T-10a]: the SOURCE half is neutralized, never escaped — the
    /// distinction the whole helper exists to keep straight.
    ///
    /// If source text went through `sanitize_control_chars*` instead, each 1-byte
    /// control would become a 6-byte literal and every following span offset would be
    /// wrong. This asserts byte length is preserved and that no `\uXXXX` literal (the
    /// signature of the wrong function) appears.
    #[test]
    fn named_source_neutralizes_source_without_changing_byte_length() {
        let src = "let x\u{1b} = 1;\u{061C}\n";
        let ns = named_source_for_render("clean.mds", src);
        let rendered = {
            use miette::SourceCode as _;
            let contents = ns
                .read_span(&(0..src.len()).into(), 0, 0)
                .expect("span must be readable");
            String::from_utf8(contents.data().to_vec()).expect("neutralized source stays UTF-8")
        };
        assert_eq!(
            rendered.len(),
            src.len(),
            "source neutralization must preserve byte length; got: {rendered:?}"
        );
        assert!(
            !rendered.contains("\\u001B"),
            "source must be NEUTRALIZED, not escaped — a \\uXXXX literal means the \
             wrong function was used and every following span offset is now wrong; \
             got: {rendered:?}"
        );
        // Positive: the width-appropriate substitutes are present.
        assert!(
            rendered.contains('?') && rendered.contains('\u{00A0}'),
            "1-byte ESC must become '?' and 2-byte U+061C must become NBSP; got: {rendered:?}"
        );
        // Non-vacuity: surrounding source survives.
        assert!(
            rendered.contains("let x"),
            "non-vacuity: clean source must survive; got: {rendered:?}"
        );
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

    // ── sanitized_for_render ──────────────────────────────────────────────────
    //
    // Security-critical: this method is the CLI's render-boundary sanitizer.
    // It lives in mds-core and must pass these invariants even when no CLI test
    // exercises it, so `cargo test -p mds-core` stays green independently.

    /// T-SFR-1: ESC byte in `message` is escaped; `\n` is preserved (HUMAN mode).
    ///
    /// Pins the HUMAN-mode choice: `\n` in a diagnostic body is legitimate prose
    /// punctuation (multi-line miette frame) and must survive the round-trip.
    /// ESC (U+001B) is a hostile control and must be replaced by `\\u001B`.
    #[test]
    fn sanitized_for_render_escapes_esc_preserves_newline() {
        let diag = LintDiagnostic {
            rule: "unused-variable".to_string(),
            severity: Severity::Warn,
            message: "line one\x1Bline two\nline three".to_string(),
            help: Some("help\x1Btext\nwith newline".to_string()),
            span: None,
            file: None,
            fix_removals: None,
            fix_edits: None,
        };

        let rendered = diag.sanitized_for_render();

        // ESC is replaced by the \uXXXX literal.
        assert!(
            !rendered.message.contains('\x1B'),
            "raw ESC must not appear in sanitized message; got: {:?}",
            rendered.message
        );
        assert!(
            rendered.message.contains("\\u001B"),
            "\\u001B literal must appear in sanitized message; got: {:?}",
            rendered.message
        );

        // \n is preserved (HUMAN mode).
        assert!(
            rendered.message.contains('\n'),
            "newline must be preserved in HUMAN-mode message; got: {:?}",
            rendered.message
        );

        // help is also sanitized.
        let help = rendered.help.as_deref().expect("help must be Some");
        assert!(!help.contains('\x1B'), "raw ESC must not appear in help");
        assert!(
            help.contains("\\u001B"),
            "\\u001B must appear in sanitized help"
        );
        assert!(help.contains('\n'), "newline must be preserved in help");
    }

    /// T-SFR-2: `span` and `file` pass through byte-identical; fix data is stripped.
    ///
    /// Pins three behaviours together:
    /// - `span` offset/length/line/column are raw values, not altered by sanitization.
    /// - `file` is copied verbatim — including hostile control bytes (WIRE escaping of
    ///   the filename is the caller's responsibility via `named_source_for_render`).
    /// - `fix_removals` and `fix_edits` are set to `None` so the sanitized clone
    ///   cannot be mistaken for a fix-bearing diagnostic.
    ///
    /// The hostile filename `"a\u{1B}/b\u{202E}.mds"` exercises the no-sanitize
    /// guarantee: `sanitized_for_render` must copy `file` byte-for-byte regardless of
    /// what control bytes it contains.  This is intentional — WIRE-escaping happens in
    /// `named_source_for_render`, not here (PF-014).
    #[test]
    fn sanitized_for_render_span_file_unchanged_fix_nulled() {
        let span = crate::error::SerializedSpan::new(42, 7)
            .with_line(3)
            .with_column(1);
        let hostile_file = "a\u{1B}/b\u{202E}.mds".to_string();
        let diag = LintDiagnostic::new("duplicate-import", Severity::Error, "msg")
            .with_span(span.clone())
            .with_file(hostile_file.clone())
            .with_fix_removals(vec![FixLineSpan::single(42)])
            .with_fix_edits(vec![TextEdit::new(0, 3, "x")]);

        let rendered = diag.sanitized_for_render();

        // span is byte-identical, including line and column.
        let rendered_span = rendered.span.as_ref().expect("span must survive");
        assert_eq!(rendered_span.offset, 42, "span.offset must be unchanged");
        assert_eq!(rendered_span.length, 7, "span.length must be unchanged");
        assert_eq!(
            rendered_span.line,
            Some(3),
            "span.line must survive sanitization"
        );
        assert_eq!(
            rendered_span.column,
            Some(1),
            "span.column must survive sanitization"
        );

        // file is copied verbatim — hostile bytes are NOT sanitized by sanitized_for_render
        // (the caller's named_source_for_render is responsible for WIRE-escaping the filename).
        assert_eq!(
            rendered.file.as_deref(),
            Some(hostile_file.as_str()),
            "file must be byte-identical, including hostile control bytes"
        );

        // fix data is intentionally stripped.
        assert!(
            rendered.fix_removals.is_none(),
            "fix_removals must be None in the sanitized clone"
        );
        assert!(
            rendered.fix_edits.is_none(),
            "fix_edits must be None in the sanitized clone"
        );
    }

    // ── AC-P1-08 / AD-202-1: sort by byte offset ─────────────────────────────

    fn make_span_diag(file: Option<&str>, offset: Option<usize>, rule: &str) -> LintDiagnostic {
        let mut diag = LintDiagnostic::new(rule, Severity::Warn, format!("{rule} at {offset:?}"));
        if let Some(o) = offset {
            diag = diag.with_span(SerializedSpan::new(o, 1));
        }
        if let Some(f) = file {
            diag = diag.with_file(f);
        }
        diag
    }

    /// AC-P1-08: `LintResultBuilder::build` sorts diagnostics by ascending byte
    /// offset within each file.
    #[test]
    fn build_sorts_diagnostics_by_offset() {
        let mut builder = LintResultBuilder::new();
        // Insert in reverse offset order.
        builder.push(make_span_diag(Some("a.mds"), Some(20), "r1"));
        builder.push(make_span_diag(Some("a.mds"), Some(5), "r2"));
        builder.push(make_span_diag(Some("a.mds"), Some(10), "r3"));
        let result = builder.build(false);
        let offsets: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| d.span.as_ref().map(|s| s.offset))
            .collect();
        assert_eq!(
            offsets,
            vec![Some(5), Some(10), Some(20)],
            "diagnostics must be ordered by ascending byte offset; got: {:?}",
            offsets
        );
    }

    /// AC-P1-10 (multi-file): files are ordered lexicographically; within a file
    /// diagnostics are ordered by byte offset.
    #[test]
    fn build_sorts_multi_file_diagnostics() {
        let mut builder = LintResultBuilder::new();
        builder.push(make_span_diag(Some("b.mds"), Some(1), "r1"));
        builder.push(make_span_diag(Some("a.mds"), Some(10), "r2"));
        builder.push(make_span_diag(Some("a.mds"), Some(3), "r3"));
        let result = builder.build(false);
        let keys: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| (d.file.as_deref(), d.span.as_ref().map(|s| s.offset)))
            .collect();
        assert_eq!(
            keys,
            vec![
                (Some("a.mds"), Some(3)),
                (Some("a.mds"), Some(10)),
                (Some("b.mds"), Some(1)),
            ],
            "multi-file diagnostics must sort by (file, offset); got: {:?}",
            keys
        );
    }

    /// AC-P1-08 (stable ties) / AD-202-1: diagnostics with the same file and
    /// offset preserve insertion order (stable sort).
    #[test]
    fn build_stable_sort_preserves_insertion_order_for_equal_offsets() {
        let mut builder = LintResultBuilder::new();
        builder.push(make_span_diag(Some("a.mds"), Some(5), "first"));
        builder.push(make_span_diag(Some("a.mds"), Some(5), "second"));
        builder.push(make_span_diag(Some("a.mds"), Some(5), "third"));
        let result = builder.build(false);
        let rules: Vec<_> = result.diagnostics.iter().map(|d| d.rule.as_str()).collect();
        assert_eq!(
            rules,
            vec!["first", "second", "third"],
            "equal-offset diagnostics must preserve insertion order; got: {:?}",
            rules
        );
    }

    /// AC-P1-08 (no-span): diagnostics without a span sort to the end of their
    /// file group and serialize as `"span": null` on the canonical-JSON wire path.
    #[test]
    fn build_no_span_sorts_to_end() {
        let mut builder = LintResultBuilder::new();
        builder.push(make_span_diag(Some("a.mds"), None, "no-span"));
        builder.push(make_span_diag(Some("a.mds"), Some(3), "has-span"));
        let result = builder.build(false);
        assert_eq!(
            result.diagnostics[0].rule, "has-span",
            "spanned diagnostic must sort before no-span; got: {:?}",
            result.diagnostics
        );
        assert_eq!(
            result.diagnostics[1].rule, "no-span",
            "no-span diagnostic must sort to the end; got: {:?}",
            result.diagnostics
        );

        // AC-P1-08 (span:None, JSON wire): the no-span diagnostic must
        // serialize as `"span": null` on the canonical-JSON wire path — key
        // present, value null, not an absent key.
        //
        // Serde_json trap: `v["span"].is_null()` returns `true` for an
        // ABSENT key too, because `Value::index` returns `Value::Null` for
        // missing keys rather than panicking.  Always verify key presence via
        // `as_object().contains_key` first, then check the value separately.
        let json = result.to_canonical_json();
        let diags = json["files"][0]["diagnostics"]
            .as_array()
            .expect("diagnostics must be a JSON array");
        // diags[1] is the no-span entry — order verified above via .diagnostics.
        let no_span_obj = diags[1]
            .as_object()
            .expect("diagnostic must be a JSON object");
        assert!(
            no_span_obj.contains_key("span"),
            "no-span diagnostic must carry `\"span\": null` on the wire \
             (key present, value null) — not an absent key; \
             got object: {no_span_obj:?}"
        );
        assert!(
            no_span_obj.get("span").unwrap().is_null(),
            "no-span diagnostic's `\"span\"` value must be JSON null; got: {:?}",
            no_span_obj.get("span").unwrap()
        );
    }

    /// AC-P1-08 (file-less diagnostics): a diagnostic with `file: None` sorts
    /// after every diagnostic that has a file — including a file name whose first
    /// character is outside the BMP.
    ///
    /// This is a regression pin for the sentinel-string approach the sort key used
    /// to take: `unwrap_or("\u{FFFF}")` compares byte-wise, and a name starting
    /// with an astral-plane character (UTF-8 lead byte `0xF0`) sorts AFTER
    /// `U+FFFF` (lead byte `0xEF`), which silently placed the file-less
    /// diagnostic in the middle of the list instead of at the end.
    #[test]
    fn build_file_less_diagnostic_sorts_after_astral_plane_filename() {
        let mut builder = LintResultBuilder::new();
        builder.push(make_span_diag(None, Some(0), "no-file"));
        // U+1F600 GRINNING FACE — first byte 0xF0, greater than U+FFFF's 0xEF.
        builder.push(make_span_diag(
            Some("\u{1F600}.mds"),
            Some(0),
            "astral-file",
        ));
        let result = builder.build(false);
        let rules: Vec<_> = result.diagnostics.iter().map(|d| d.rule.as_str()).collect();
        assert_eq!(
            rules,
            vec!["astral-file", "no-file"],
            "a file-less diagnostic must sort last regardless of the other file \
             names present; got: {rules:?}"
        );
    }

    /// AC-P1-12 / AD-202-2: truncation selects by rule-execution order, NOT by
    /// offset. Sorting reorders the retained set; it never changes which
    /// diagnostics were retained.
    ///
    /// Diagnostics are pushed with DESCENDING offsets, so the one rejected by the
    /// cap carries the SMALLEST offset — the very diagnostic that would sort first
    /// if the cap were offset-ranked. It must still be absent.
    #[test]
    fn truncation_selects_by_insertion_order_not_by_offset() {
        let mut builder = LintResultBuilder::new();
        for i in 0..MAX_DIAGNOSTICS {
            let offset = MAX_DIAGNOSTICS - i; // descending: 1000, 999, … 1
            assert!(
                builder.push(make_span_diag(Some("a.mds"), Some(offset), "kept")),
                "diagnostic {i} should be accepted below the cap"
            );
        }
        // Offset 0 is smaller than every retained offset, and is pushed last.
        let rejected = builder.push(make_span_diag(Some("a.mds"), Some(0), "rejected"));
        assert!(!rejected, "push beyond the cap must return false");

        let result = builder.build(false);
        assert_eq!(result.diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(
            result.truncated,
            "truncated must be true when the cap was hit"
        );
        assert!(
            !result.diagnostics.iter().any(|d| d.rule == "rejected"),
            "the over-cap diagnostic must stay dropped even though its offset (0) \
             would sort first — truncation is not offset-ranked (AD-202-2)"
        );
        // The retained set is sorted ascending among itself: first offset is 1.
        let offsets: Vec<usize> = result
            .diagnostics
            .iter()
            .filter_map(|d| d.span.as_ref().map(|s| s.offset))
            .collect();
        assert_eq!(
            offsets.first().copied(),
            Some(1),
            "the retained set must be sorted ascending among itself"
        );
        assert!(
            offsets.windows(2).all(|w| w[0] <= w[1]),
            "the retained set must be in non-decreasing offset order"
        );
    }

    /// AD-202-1b regression: `LintResult::new` preserves caller-supplied order on
    /// ALL surfaces, including `to_canonical_json`.
    ///
    /// The contract is documented in `LintResult::new`'s `# Ordering` section:
    /// the constructor does NOT sort, and `sort_diagnostics` is the single ordering
    /// choke point called only from `LintResultBuilder::build`.  Adding a
    /// "defensive re-sort" inside `to_canonical_json` would silently contradict
    /// CHANGELOG.md §1, the `to_canonical_json` rustdoc, and this test.
    ///
    /// Scenario: a downstream Rust consumer builds `LintResult::new` with
    /// `diag_offset_50` first and `diag_offset_10` second (deliberate priority
    /// order).  `to_canonical_json` must emit them in the same order.
    #[test]
    fn lint_result_new_preserves_caller_order_through_to_canonical_json() {
        // Two diagnostics in a deliberately REVERSE offset order (50 before 10).
        let diag_offset_50 = make_span_diag(Some("a.mds"), Some(50), "r-50");
        let diag_offset_10 = make_span_diag(Some("a.mds"), Some(10), "r-10");

        // Build via the public constructor — does NOT sort (AD-202-1b).
        let result = LintResult::new(vec![diag_offset_50, diag_offset_10]);

        let json = result.to_canonical_json();
        let diags = json["files"][0]["diagnostics"].as_array().unwrap();

        assert_eq!(
            diags.len(),
            2,
            "expected exactly two diagnostics in the JSON output"
        );

        // Caller supplied offset-50 first; it must remain first in the JSON.
        let first_rule = diags[0]["rule"].as_str().unwrap();
        let second_rule = diags[1]["rule"].as_str().unwrap();
        assert_eq!(
            first_rule, "r-50",
            "to_canonical_json must preserve caller-supplied order for LintResult::new; \
             expected 'r-50' first (offset 50) but got: {:?}",
            first_rule
        );
        assert_eq!(
            second_rule, "r-10",
            "to_canonical_json must preserve caller-supplied order for LintResult::new; \
             expected 'r-10' second (offset 10) but got: {:?}",
            second_rule
        );

        // Spot-check the offsets to make the ordering claim self-documenting.
        assert_eq!(diags[0]["span"]["offset"], 50_u64);
        assert_eq!(diags[1]["span"]["offset"], 10_u64);
    }
}
