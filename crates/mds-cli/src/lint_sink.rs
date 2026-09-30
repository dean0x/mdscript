//! Where `mds lint` shows what it found (#309).
//!
//! `lint.rs` works out what linting an input came to — a plain-data report — without printing
//! anything, and its `render` shows the report through a [`ResultSink`]: a [`HumanSink`] for
//! `--format human`, a [`JsonSink`] for `--format json`. `render` decides the order; a sink
//! decides how each part reads in its format.
//!
//! A sink writes status lines to stderr through the registered writer macro `ewriteln!`,
//! called here directly, so the print-discipline guard scans every one of them, and lint's
//! product — a JSON document, a diff, the fixed source — to stdout through `write_stdout`.
//! `tests/print_discipline.rs` (`lint_output_goes_through_the_sink`) keeps lint's writer-macro
//! calls, stdout writes and exits out of `lint.rs`.
//!
//! `--quiet` suppresses the status lines about an input, and warning- and info-severity
//! findings; each method says whether it honours it.
//!
//! A sink is not the only place a lint run prints, and `--quiet` does not reach every line
//! printed elsewhere: the directory walk's depth-limit warning (`collect_mds_files_inner`
//! in `output.rs`, printed when a tree is deeper than the walk's depth limit) takes no
//! `--quiet` parameter, so it prints under `--quiet` too — for `mds lint` as for the other
//! directory commands that share the walk, `mds build` among them.

use std::path::Path;

use mds::{MdsError, Severity};
use serde_json::Value;

use crate::lint::{DirSummary, LintSource};
use crate::output::{
    eprint_error, eprint_io_failure, relabel_stdin_error, safe_inline, safe_path, stdout_failure,
    write_stdout, StdoutOutcome, WalkResult, STDIN_DISPLAY_LABEL,
};

/// Where one `mds lint` run shows its results.
///
/// The methods whose output depends on the format are required; the status lines and the
/// stdout product, which read the same in both formats, are provided.
pub(crate) trait ResultSink {
    /// `--quiet`: the status lines about an input, and warning- and info-severity findings,
    /// are not shown.
    fn quiet(&self) -> bool;

    /// An input's findings. `text` is the source they index, when it was read: an entry of a
    /// directory under `--format json` is not read to report its findings.
    fn findings(&mut self, input: &LintSource<'_>, findings: &mds::LintResult, text: Option<&str>);

    /// An entry of a directory that could not be linted — or, under `--format json`, read to
    /// fix it. Not `--quiet`: it is an error.
    fn failed(&mut self, input: &LintSource<'_>, error: MdsError);

    /// An input whose analysis panicked. The panic hook has printed the internal compiler
    /// error text and recorded the panic, so the run exits 101 (#389).
    fn panicked(&mut self, input: &LintSource<'_>);

    /// A `--fix` rewrite of the input's file that failed. Not `--quiet`: it is an error.
    fn write_failed(&mut self, input: &LintSource<'_>, error: MdsError);

    /// `Clean:`, for an input with no findings — a file argument's human report says so.
    fn clean(&mut self, input: &LintSource<'_>, findings: &mds::LintResult);

    /// The failure that stops a run before it lints anything, or before it lints its one
    /// input, stdin or a file argument: config load, I/O, resolution, parse.
    ///
    /// `stdin_source` is stdin's text: a human frame then shows the source as
    /// [`STDIN_DISPLAY_LABEL`] instead of the `<source>` label core gives a string source
    /// ([`relabel_stdin_error`]), for every failure of a stdin run — a config rejection
    /// or I/O failure routed here later inherits the label instead of inventing a second
    /// convention. The JSON document needs no relabel: `MdsError::serialize` emits `code` /
    /// `message` / `help` / `span`, and no `MdsError` message interpolates the source's
    /// name (`cli_lint.rs::stdin_analysis_failure_labels_source_as_stdin` pins both). A file
    /// source passes `None`: its error already names the file.
    fn analysis_failure(&mut self, error: &MdsError, stdin_source: Option<&str>);

    /// A directory run starts linting its entries.
    fn start_document(&mut self);

    /// A directory run has linted every entry; `truncated` when any entry's findings stopped
    /// at the diagnostic cap.
    fn end_document(&mut self, truncated: bool);

    /// The diagnostic-cap notice, for a capped result under `--fix`. A directory's entry names
    /// its file. `--quiet` suppresses it.
    fn cap_reached(&mut self, input: &LintSource<'_>) {
        if self.quiet() {
            return;
        }
        match *input {
            LintSource::DirEntry { path, .. } => crate::output::ewriteln!(
                "{}: diagnostic cap ({}) reached; further findings were suppressed — \
                 re-run --fix to continue",
                safe_path(path),
                mds::MAX_DIAGNOSTICS
            ),
            LintSource::Stdin | LintSource::File { .. } => crate::output::ewriteln!(
                "diagnostic cap ({}) reached; further findings were suppressed — \
                 re-run --fix to continue",
                mds::MAX_DIAGNOSTICS
            ),
        }
    }

    /// `fix rejected:` — the reverify gate refused the fix, and the findings stand. A
    /// directory's entry names its file. `--quiet` suppresses it.
    fn fix_rejected(&mut self, input: &LintSource<'_>, reason: &str) {
        if self.quiet() {
            return;
        }
        match *input {
            LintSource::DirEntry { path, .. } => {
                crate::output::ewriteln!(
                    "{}: fix rejected: {}",
                    safe_path(path),
                    safe_inline(reason)
                );
            }
            LintSource::Stdin | LintSource::File { .. } => {
                crate::output::ewriteln!("fix rejected: {}", safe_inline(reason));
            }
        }
    }

    /// `Would fix:` — `--fix --check` found something to fix. `--quiet` suppresses it.
    fn would_fix(&mut self, input: &LintSource<'_>) {
        if self.quiet() {
            return;
        }
        match *input {
            LintSource::Stdin => crate::output::ewriteln!("Would fix: {STDIN_DISPLAY_LABEL}"),
            LintSource::File { typed: path, .. } | LintSource::DirEntry { path, .. } => {
                crate::output::ewriteln!("Would fix: {}", safe_path(path));
            }
        }
    }

    /// `Fixed:` / `Partially fixed:` — `--fix` fixed the input; `partial` holds the applied
    /// and planned edit counts when not every edit applied. Stdin announces a partial fix
    /// only: the fixed source on stdout is the signal. `--quiet` suppresses it.
    fn fixed(&mut self, input: &LintSource<'_>, partial: Option<(usize, usize)>) {
        if self.quiet() {
            return;
        }
        match *input {
            LintSource::Stdin => {
                if let Some((applied_count, total_count)) = partial {
                    crate::output::ewriteln!(
                        "Partially fixed: {STDIN_DISPLAY_LABEL} ({applied_count} of {total_count} fixes applied)"
                    );
                }
            }
            LintSource::File { typed: path, .. } | LintSource::DirEntry { path, .. } => {
                match partial {
                    None => crate::output::ewriteln!("Fixed: {}", safe_path(path)),
                    Some((applied_count, total_count)) => crate::output::ewriteln!(
                        "Partially fixed: {} ({applied_count} of {total_count} fixes applied)",
                        safe_path(path)
                    ),
                }
            }
        }
    }

    /// A `--fix --diff` diff, on stdout; `true` when a failing stdout lost it (see
    /// [`emit_stdout`]).
    fn diff(&mut self, diff: &str) -> bool {
        emit_stdout(diff)
    }

    /// `mds lint --fix -`'s output, the fixed source — or the source as given — on stdout.
    fn fixed_source(&mut self, source: &str) {
        emit_stdout(source);
    }

    /// The directory summary line: `{clean} clean, {warn} with warnings, {error} with
    /// errors, {limit} resource-limited`. `--quiet` suppresses it unless a file is in the error
    /// or resource-limited bucket, which is not status but the reason for a failing exit.
    fn summary(&mut self, summary: &DirSummary) {
        let DirSummary {
            clean_count,
            warn_file_count,
            error_file_count,
            limit_file_count,
            ..
        } = *summary;
        if !self.quiet() || error_file_count > 0 || limit_file_count > 0 {
            crate::output::ewriteln!(
                "{clean_count} clean, {warn_file_count} with warnings, \
                 {error_file_count} with errors, {limit_file_count} resource-limited"
            );
        }
    }

    /// A directory with nothing to lint: every `.mds` file under default-excluded
    /// directories, or none at all (#204). Not `--quiet`: a silent run would read as a pass.
    fn nothing_to_lint(&mut self, dir: &Path, walk: &WalkResult) {
        if walk.excluded_by_default > 0 {
            crate::output::ewriteln!(
                "{} .mds file(s) found but all are under default-excluded directories \
                 (hidden dirs, node_modules); nothing was linted",
                walk.excluded_by_default
            );
        } else {
            crate::output::ewriteln!(
                "no .mds files found in {}; nothing was linted",
                safe_path(dir)
            );
        }
    }

    /// Stdin could not be read, or is over the size cap: an error on stderr, in either
    /// format.
    fn stdin_unreadable(&mut self, error: MdsError) {
        eprint_error(error.into());
    }

    /// The usage error for `--fix --format json` on stdin: a plain message on stderr, in
    /// either format.
    fn stdin_fix_json_refused(&mut self) {
        crate::output::ewriteln!(
            "error: --fix --format json with stdin input is not supported; \
             use `mds lint --fix -` for filter mode or `mds lint --format json` for JSON output"
        );
    }

    /// A setup failure — the runtime variables, the input argument — on stderr.
    fn setup_failed(&mut self, error: miette::Report) {
        eprint_error(error);
    }
}

// ── Human ─────────────────────────────────────────────────────────────────────

/// `--format human`: findings rendered on stderr, each in a frame over its source.
pub(crate) struct HumanSink {
    quiet: bool,
}

impl HumanSink {
    pub(crate) fn new(quiet: bool) -> Self {
        Self { quiet }
    }
}

impl ResultSink for HumanSink {
    fn quiet(&self) -> bool {
        self.quiet
    }

    /// Each finding in its frame, named with the input's display label. `--quiet` suppresses
    /// warning- and info-severity findings; errors always show.
    fn findings(&mut self, input: &LintSource<'_>, findings: &mds::LintResult, text: Option<&str>) {
        for diag in &findings.diagnostics {
            render_diag_human(diag, self.quiet, input.display_label(), text);
        }
    }

    fn failed(&mut self, _input: &LintSource<'_>, error: MdsError) {
        eprint_error(miette::Report::from(error));
    }

    /// Nothing: the panic hook's text is the one report of a panic.
    fn panicked(&mut self, _input: &LintSource<'_>) {}

    /// A directory's entry: `error writing <path>: <error>`. A file argument: the error.
    fn write_failed(&mut self, input: &LintSource<'_>, error: MdsError) {
        match *input {
            LintSource::DirEntry { path, .. } => crate::output::ewriteln!(
                "error writing {}: {}",
                safe_path(path),
                safe_inline(&error)
            ),
            LintSource::Stdin | LintSource::File { .. } => {
                eprint_error(miette::Report::from(error));
            }
        }
    }

    /// `Clean:` names a file argument as typed, as `Fixed:` does (#390). Stdin and a
    /// directory's entries print none; a directory's summary counts its clean files.
    /// `--quiet` suppresses it.
    fn clean(&mut self, input: &LintSource<'_>, findings: &mds::LintResult) {
        if let LintSource::File { typed, .. } = *input {
            if !self.quiet && findings.diagnostics.is_empty() {
                crate::output::ewriteln!("Clean: {}", safe_path(typed));
            }
        }
    }

    fn analysis_failure(&mut self, error: &MdsError, stdin_source: Option<&str>) {
        let report = match stdin_source {
            Some(source) => relabel_stdin_error(error, source),
            None => miette::Report::from(error.clone()),
        };
        eprint_error(report);
    }

    fn start_document(&mut self) {}

    fn end_document(&mut self, _truncated: bool) {}
}

/// Render one lint diagnostic to stderr. All user-controlled text — message, help,
/// filename, and source — is sanitized at the input boundary so miette renders from
/// safe inputs; the frame itself is not post-processed.
///
/// `--quiet` suppresses Warn and Info; Error always renders.
///
/// Sanitization strategy:
/// - message/help: HUMAN-mode `sanitize_control_chars` → \\uXXXX escapes. `\n` is
///   preserved so a multi-line diagnostic frame keeps rendering.
/// - filename + source: `mds::named_source_for_render`, the shared boundary
///   `MdsError::at()` and the formatter also use — WIRE-mode escaping for the
///   single-line filename, byte-length-preserving neutralization for the span-indexed
///   source so miette's byte-offset slices stay valid.
///
/// The rendered miette frame is NOT post-processed, so miette's own SGR colour codes
/// are never corrupted.
///
/// Every human run reads its input before it lints it, so a finding has its `text`; one
/// without it would still show, only without the source snippet and the frame header
/// that names the file.
fn render_diag_human(diag: &mds::LintDiagnostic, quiet: bool, filename: &str, text: Option<&str>) {
    if quiet && matches!(diag.severity, Severity::Info | Severity::Warn) {
        return;
    }
    // Sanitize message and help at the input boundary via the core method, which also
    // leaves out the fix edits the frame does not show.
    let report = miette::Report::from(diag.sanitized_for_render());
    let report = match text {
        Some(text) => report.with_source_code(mds::named_source_for_render(filename, text)),
        None => report,
    };
    eprint_error(report);
}

// ── JSON ──────────────────────────────────────────────────────────────────────

/// `--format json`: findings as one JSON document on stdout — each input's own, or, for a
/// directory, one document for every entry.
pub(crate) struct JsonSink {
    quiet: bool,
    /// A directory run's document, from [`ResultSink::start_document`] to
    /// [`ResultSink::end_document`]: its `files[]` entries so far.
    document: Option<Vec<Value>>,
}

impl JsonSink {
    pub(crate) fn new(quiet: bool) -> Self {
        Self {
            quiet,
            document: None,
        }
    }

    /// The entries a directory run's document holds so far.
    #[cfg(test)]
    pub(crate) fn document(&self) -> &[Value] {
        self.document.as_deref().unwrap_or_default()
    }
}

impl ResultSink for JsonSink {
    fn quiet(&self) -> bool {
        self.quiet
    }

    /// Into the directory's document during a directory run; otherwise the input's own
    /// document, on stdout.
    fn findings(
        &mut self,
        _input: &LintSource<'_>,
        findings: &mds::LintResult,
        _text: Option<&str>,
    ) {
        let files = files_of(findings);
        match self.document.as_mut() {
            Some(document) => document.extend(files),
            None => {
                emit_stdout(&json_line(&files_document(files, findings.truncated)));
            }
        }
    }

    /// The failure is the entry's one record in the directory's document.
    fn failed(&mut self, input: &LintSource<'_>, error: MdsError) {
        if let Some(document) = self.document.as_mut() {
            document.push(error_entry(input, &error));
            return;
        }
        self.analysis_failure(&error, None);
    }

    /// A directory's entry: `{"file": …, "error": …}` with the internal error
    /// ([`internal_error`]) is the file's one record in the document. A file argument: the
    /// error document `{"error": …, "version": 1}` with it, on stdout — the run's one
    /// document. Stdin never gets here: `--fix --format json` refuses it.
    fn panicked(&mut self, input: &LintSource<'_>) {
        match self.document.as_mut() {
            Some(document) => document.push(serde_json::json!({
                "file": entry_key(input),
                "error": internal_error(),
            })),
            None => {
                emit_stdout(&json_line(&serde_json::json!({
                    "version": 1,
                    "error": internal_error(),
                })));
            }
        }
    }

    /// A directory's entry: the failure, in its own words, is the file's one record in the
    /// document. A file argument: the error, on stderr.
    fn write_failed(&mut self, input: &LintSource<'_>, error: MdsError) {
        match self.document.as_mut() {
            Some(document) => document.push(error_entry(
                input,
                &MdsError::Io {
                    message: format!("{error}"),
                },
            )),
            None => eprint_error(miette::Report::from(error)),
        }
    }

    fn clean(&mut self, _input: &LintSource<'_>, _findings: &mds::LintResult) {}

    /// The error document `{"error": …, "version": 1}` on stdout.
    fn analysis_failure(&mut self, error: &MdsError, _stdin_source: Option<&str>) {
        let envelope = serde_json::json!({
            "version": 1,
            "error": error.serialize()
        });
        emit_stdout(&json_line(&envelope));
    }

    fn start_document(&mut self) {
        self.document = Some(Vec::new());
    }

    /// The directory's one document, on stdout.
    fn end_document(&mut self, truncated: bool) {
        if let Some(files) = self.document.take() {
            emit_stdout(&json_line(&files_document(files, truncated)));
        }
    }
}

/// Lint's findings document, `{"files": […], "truncated": …, "version": 1}`: one input's
/// findings, or a directory's entries.
fn files_document(files: Vec<Value>, truncated: bool) -> Value {
    serde_json::json!({
        "version": 1,
        "files": files,
        "truncated": truncated,
    })
}

/// The `files[]` entries of `findings`' canonical JSON, moved out of it rather than copied
/// (#173).
fn files_of(findings: &mds::LintResult) -> Vec<Value> {
    let mut canonical = findings.to_canonical_json();
    match canonical.get_mut("files").map(Value::take) {
        Some(Value::Array(files)) => files,
        _ => Vec::new(),
    }
}

/// A directory's JSON entry for a file that failed: `{"file": …, "error": …}`.
///
/// `to_canonical_json` escapes the `file` key of an entry with findings; this entry does
/// not pass through it, so its key is escaped here the same way. A hostile file name then
/// reads the same in both entry types, and sorts where its key says.
fn error_entry(input: &LintSource<'_>, error: &MdsError) -> Value {
    serde_json::json!({
        "file": entry_key(input),
        "error": error.serialize()
    })
}

/// The `file` key of an entry that does not pass through `to_canonical_json`, escaped as
/// that escapes the key of an entry with findings.
fn entry_key(input: &LintSource<'_>) -> String {
    mds::sanitize_control_chars_wire(input.display_label()).into_owned()
}

/// The JSON error of an input whose analysis panicked (#389): the code the bindings give a
/// caught panic, `mds::internal`, and the fixed message the panic hook prints — nothing of
/// the panic itself. `help` and `span` are `null`, as in every other JSON error.
fn internal_error() -> Value {
    serde_json::json!({
        "code": "mds::internal",
        "message": "internal compiler error",
        "help": null,
        "span": null,
    })
}

/// `document` on one line, as lint prints it. A `serde_json::Value`'s `Display` is its
/// compact serialisation, which cannot fail: every key is a string, every number finite.
fn json_line(document: &Value) -> String {
    format!("{document}\n")
}

// ── stdout ────────────────────────────────────────────────────────────────────

/// Write lint's product — a JSON document, a diff, the fixed source — to stdout, and
/// return whether a failing stdout lost it (#157).
///
/// [`write_stdout`] writes and flushes it, so a fixed source without a final `\n` is
/// never left in a buffer when the run exits. A closed stdout keeps the verdict: the
/// reader is gone, as with `mds lint --fix - | head -n1`, nothing more is written, and
/// nothing is lost (`false`). The first failure for any other reason is reported as one
/// `mds::io` error naming stdout and recorded, so the exit funnel ends the run with at
/// least 2; a repeat of it was already reported and recorded. Either failure returns
/// `true`: a directory run counts the file whose diff it lost under "with errors", as it
/// counts a file whose rewrite fails and as `mds fmt <dir>` counts it failed. Every other
/// caller goes on to its verdict, which the recorded failure already lifts.
fn emit_stdout(text: &str) -> bool {
    match write_stdout(text.as_bytes()) {
        StdoutOutcome::Written | StdoutOutcome::Closed => false,
        StdoutOutcome::Failed(e) => {
            eprint_io_failure(stdout_failure(&e));
            true
        }
        StdoutOutcome::FailedAgain => true,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{files_document, files_of, json_line};
    use mds::{LintDiagnostic, LintResult, SerializedSpan, Severity};
    use serde_json::Value;

    /// Findings in two files, one with a help text and a span and one without, so the
    /// canonical JSON carries two `files[]` entries and each optional field in both forms.
    fn findings_in_two_files() -> LintResult {
        LintResult::new(vec![
            LintDiagnostic::new("empty-block", Severity::Warn, "an empty block")
                .with_help("remove the block")
                .with_span(SerializedSpan::new(0, 7))
                .with_file("a.mds"),
            LintDiagnostic::new("unused-variable", Severity::Error, "an unused variable")
                .with_file("b.mds"),
        ])
    }

    /// The top-level keys of `document`, in the order it serializes them.
    fn keys(document: &Value) -> Vec<String> {
        document
            .as_object()
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// A single input's JSON document is built from three keys — `files[]` moved out of
    /// core's `LintResult::to_canonical_json`, `truncated`, and `version` — instead of
    /// being core's document itself (#309). It must stay core's document: the same keys,
    /// the same values, and the same bytes on stdout as the canonical JSON serialized
    /// directly. A key core adds to its document and the rebuild leaves out fails the key
    /// comparison first, naming the difference.
    ///
    /// Both values of `truncated`, with findings in two files: `truncated` is the one value
    /// the rebuild takes from outside `files[]`.
    #[test]
    fn a_files_document_is_core_s_canonical_json() {
        for truncated in [false, true] {
            let result = if truncated {
                findings_in_two_files().truncated()
            } else {
                findings_in_two_files()
            };
            let canonical = result.to_canonical_json();
            assert_eq!(
                canonical["truncated"], truncated,
                "precondition: core's document carries this arm's `truncated`"
            );
            assert_eq!(
                canonical["files"].as_array().map(Vec::len),
                Some(2),
                "precondition: core's document has an entry per file; got {canonical}"
            );

            let document = files_document(files_of(&result), result.truncated);
            assert_eq!(
                keys(&document),
                keys(&canonical),
                "truncated = {truncated}: the document's keys must be core's"
            );
            assert_eq!(document, canonical, "truncated = {truncated}");
            let canonical_line = serde_json::to_string(&canonical)
                .expect("a serde_json::Value always serializes")
                + "\n";
            assert_eq!(
                json_line(&document),
                canonical_line,
                "truncated = {truncated}: stdout gets core's document byte for byte"
            );
        }
    }
}
