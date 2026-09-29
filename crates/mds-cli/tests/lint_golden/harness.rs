//! Shared machinery for the `mds lint` characterization goldens (#309).
//!
//! A *cell* is one `mds lint` invocation: an input mode, an output format, `--quiet`
//! off or on, a fix mode and an outcome fixture. Each cell is run in a fresh temporary
//! directory holding the fixture, with a cleared environment, piped streams and
//! relative arguments. The run is recorded as its exit code, exact stdout, exact
//! stderr and the fixture source file's bytes afterwards, then compared with the
//! golden the cell maps to.
//!
//! The goldens pin what `mds lint` prints today, including output that is known to be
//! wrong: a later commit that changes lint output regenerates them and names the
//! changed cell ids in its message.
//!
//! Normalization is applied to the recorded streams only, and consists of exactly:
//! 1. every spelling of the cell's temporary directory (as created, canonical, with and
//!    without a `/private` prefix, with and without a Windows verbatim prefix, with
//!    doubled or forward-slash separators) is replaced by `$TMP`, and the number of
//!    replacements is part of the golden;
//! 2. an atomic-write temp file name `.mds-tmp-` + six ASCII alphanumerics + `.tmp` is
//!    replaced by `.mds-tmp-RANDOM.tmp`, and that count is part of the golden too;
//! 3. on Windows only, path separators are rewritten to `/` (a doubled, escaped
//!    backslash first, then a single one).
//!
//! Nothing is sorted, trimmed or line-ending-rewritten.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

// ── Storage bounds ───────────────────────────────────────────────────────────

/// A stream longer than this many bytes is stored as a [`Digest`], not inline.
pub const DIGEST_THRESHOLD: usize = 16 * 1024;
/// A digest keeps at most this many leading lines ...
pub const DIGEST_HEAD_LINES: usize = 40;
/// ... truncated to at most this many bytes (a single long JSON line would otherwise
/// be stored whole).
pub const DIGEST_HEAD_MAX_BYTES: usize = 4096;
/// A digest keeps at most this many trailing lines ...
pub const DIGEST_TAIL_LINES: usize = 20;
/// ... truncated to their last this-many bytes.
pub const DIGEST_TAIL_MAX_BYTES: usize = 2048;
/// Upper bound on the total bytes of golden text (inline streams, digest heads and
/// tails, changed file bytes).
pub const GOLDEN_DATA_CAP: usize = 1024 * 1024;

/// How long one `mds lint` child may run before the cell fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(180);
/// Poll interval while waiting for a child (the wait is bounded by `CHILD_DEADLINE`).
const CHILD_POLL: Duration = Duration::from_millis(10);

/// The environment variable that switches the generator on (see `print_goldens_module`).
pub const PRINT_ENV: &str = "MDS_GOLDEN_PRINT";

// ── Golden representation ────────────────────────────────────────────────────

/// A recorded stream, inline or (above [`DIGEST_THRESHOLD`]) as a digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Text(&'static str),
    Digest(Digest),
}

/// Length, line count, FNV-1a-64 hash and the first/last lines of a long stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digest {
    pub len: usize,
    pub lines: usize,
    pub fnv: u64,
    pub head: &'static str,
    pub tail: &'static str,
}

/// The fixture source file after the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAfter {
    /// Byte-identical to the fixture.
    Unchanged,
    /// Rewritten; the new bytes.
    Changed(Stream),
    /// No longer present.
    Missing,
}

/// One deduplicated golden: everything a cell records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Golden {
    pub exit: i32,
    pub stdout: Stream,
    pub stderr: Stream,
    pub file: FileAfter,
    /// Temporary-directory spellings replaced by `$TMP` (the path-leak count).
    pub tmp_paths: u32,
    /// Atomic-write temp file names replaced by `.mds-tmp-RANDOM.tmp`.
    pub tmp_names: u32,
}

// ── The matrix ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// `mds lint … -`, fixture piped on stdin, cwd = fixture dir (so `mds.json` applies).
    Stdin,
    /// `mds lint … x.mds` (relative), cwd = fixture dir.
    File,
}

impl Input {
    pub const ALL: [Input; 2] = [Input::Stdin, Input::File];

    pub fn name(self) -> &'static str {
        match self {
            Input::Stdin => "stdin",
            Input::File => "file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Json,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Human, Format::Json];

    pub fn name(self) -> &'static str {
        match self {
            Format::Human => "human",
            Format::Json => "json",
        }
    }

    fn args(self) -> &'static [&'static str] {
        match self {
            Format::Human => &[],
            Format::Json => &["--format", "json"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    Loud,
    Quiet,
}

impl Quiet {
    pub const ALL: [Quiet; 2] = [Quiet::Loud, Quiet::Quiet];

    pub fn name(self) -> &'static str {
        match self {
            Quiet::Loud => "loud",
            Quiet::Quiet => "quiet",
        }
    }

    fn args(self) -> &'static [&'static str] {
        match self {
            Quiet::Loud => &[],
            Quiet::Quiet => &["--quiet"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixMode {
    Report,
    Fix,
    FixCheck,
    FixDiff,
    FixCheckDiff,
}

impl FixMode {
    pub const ALL: [FixMode; 5] = [
        FixMode::Report,
        FixMode::Fix,
        FixMode::FixCheck,
        FixMode::FixDiff,
        FixMode::FixCheckDiff,
    ];

    pub fn name(self) -> &'static str {
        match self {
            FixMode::Report => "report",
            FixMode::Fix => "fix",
            FixMode::FixCheck => "fix-check",
            FixMode::FixDiff => "fix-diff",
            FixMode::FixCheckDiff => "fix-check-diff",
        }
    }

    fn args(self) -> &'static [&'static str] {
        match self {
            FixMode::Report => &[],
            FixMode::Fix => &["--fix"],
            FixMode::FixCheck => &["--fix", "--check"],
            FixMode::FixDiff => &["--fix", "--diff"],
            FixMode::FixCheckDiff => &["--fix", "--check", "--diff"],
        }
    }
}

/// The outcome fixtures. Sources are ASCII and contain no backslash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixture {
    /// No findings.
    Clean,
    /// One warning (an unused frontmatter key).
    Warn,
    /// The warning raised to an error by `mds.json`.
    Error,
    /// One fixable error: an always-false `@if` whose removal leaves a clean file.
    Fixed,
    /// The fixable dead `@if` followed by an unfixable warning located AFTER it.
    Partial,
    /// A fix the reverify gate rejects (removing the empty `@define` orphans `@export`).
    Rejected,
    /// One more unused frontmatter key than the diagnostic cap.
    Cap,
    /// One more fixable empty `@if` block than the diagnostic cap.
    CapFixable,
    /// An unterminated `@if` (the analysis itself fails).
    AnalysisFail,
    /// One byte over the 10 MiB input limit.
    Limit,
    /// A clean source next to a malformed `mds.json`.
    ConfigFail,
    /// The fixable source in a read-only (0o555) directory. Unix only.
    WriteFail,
    /// A partial module (`_p.mds`) with a fixable dead `@if` and a frontmatter key that
    /// only a partial may leave unused.
    PartialModule,
}

impl Fixture {
    pub const ALL: [Fixture; 13] = [
        Fixture::Clean,
        Fixture::Warn,
        Fixture::Error,
        Fixture::Fixed,
        Fixture::Partial,
        Fixture::Rejected,
        Fixture::Cap,
        Fixture::CapFixable,
        Fixture::AnalysisFail,
        Fixture::Limit,
        Fixture::ConfigFail,
        Fixture::WriteFail,
        Fixture::PartialModule,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Fixture::Clean => "clean",
            Fixture::Warn => "warn",
            Fixture::Error => "error",
            Fixture::Fixed => "fixed",
            Fixture::Partial => "partial",
            Fixture::Rejected => "rejected",
            Fixture::Cap => "cap",
            Fixture::CapFixable => "cap-fixable",
            Fixture::AnalysisFail => "analysis-fail",
            Fixture::Limit => "limit",
            Fixture::ConfigFail => "config-fail",
            Fixture::WriteFail => "write-fail",
            Fixture::PartialModule => "partial-module",
        }
    }

    /// The source file's name inside the fixture directory.
    pub fn file_name(self) -> &'static str {
        match self {
            Fixture::PartialModule => "_p.mds",
            _ => "x.mds",
        }
    }

    /// The source text (generated fixtures are built once per process).
    pub fn source(self) -> &'static str {
        match self {
            Fixture::Clean | Fixture::ConfigFail => CLEAN_SOURCE,
            Fixture::Warn | Fixture::Error => WARN_SOURCE,
            Fixture::Fixed | Fixture::WriteFail => FIXED_SOURCE,
            Fixture::Partial => PARTIAL_SOURCE,
            Fixture::Rejected => REJECTED_SOURCE,
            Fixture::Cap => cap_source(),
            Fixture::CapFixable => cap_fixable_source(),
            Fixture::AnalysisFail => ANALYSIS_FAIL_SOURCE,
            Fixture::Limit => limit_source(),
            Fixture::PartialModule => PARTIAL_MODULE_SOURCE,
        }
    }

    /// The `mds.json` written next to the source, if any.
    pub fn config(self) -> Option<&'static str> {
        match self {
            Fixture::Error => Some(ERROR_CONFIG),
            Fixture::ConfigFail => Some(BROKEN_CONFIG),
            _ => None,
        }
    }

    /// Cells for this fixture exist on Unix only (Windows' read-only attribute does not
    /// stop file creation inside a directory).
    pub fn unix_only(self) -> bool {
        self == Fixture::WriteFail
    }
}

pub const CLEAN_SOURCE: &str = "---\ngreeting: Hello\n---\n\n{{greeting}}, world!\n";
pub const WARN_SOURCE: &str =
    "---\ngreeting: Hello\nunused_key: never referenced in the body\n---\n\n{{greeting}}, world!\n";
pub const ERROR_CONFIG: &str = "{\"lint\":{\"rules\":{\"unused-variable\":\"error\"}}}\n";
pub const BROKEN_CONFIG: &str = "{";
pub const FIXED_SOURCE: &str = "@if \"x\" == \"y\":\nhidden\n@end\nHello\n";
pub const PARTIAL_SOURCE: &str = "---\nflag: true\n---\n@if \"x\" == \"y\":\nhidden\n@end\n\
     @if flag:\nsame\n@else:\nsame\n@end\n";
/// The same text as `FIX_REJECTED_SOURCE` in `cli_lint.rs`.
pub const REJECTED_SOURCE: &str = "@define empty_fn():\n\n@end\n\n@export empty_fn\n";
pub const ANALYSIS_FAIL_SOURCE: &str = "---\nflag: true\n---\n@if flag:\nhello\n";
pub const PARTIAL_MODULE_SOURCE: &str = "---\npartial_var: value\n---\n\
     @if \"x\" == \"y\":\nhidden\n@end\nThis is a partial file.\n";

/// Findings the cap fixtures produce: one more than the 1,000-diagnostic cap.
pub const CAP_FINDINGS: usize = 1001;
/// Bytes in the limit fixture: one more than the 10 MiB input limit.
pub const LIMIT_BYTES: usize = 10 * 1024 * 1024 + 1;

/// 1,001 unused frontmatter keys (an unfixable warning each).
pub fn cap_source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let mut s = String::from("---\n");
        for i in 0..CAP_FINDINGS {
            let _ = writeln!(s, "v{i}: 1");
        }
        s.push_str("---\nHello\n");
        s
    })
}

/// 1,001 empty `@if` blocks (a fixable `empty-block` warning each). Built from `@if`,
/// not `@define`: sources with many `@define` blocks are kept out of this matrix.
pub fn cap_fixable_source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| {
        let mut s = String::from("---\nflag: true\n---\n");
        for _ in 0..CAP_FINDINGS {
            s.push_str("@if flag:\n@end\n");
        }
        s
    })
}

/// 10 MiB + 1 bytes of plain text, generated once per process.
pub fn limit_source() -> &'static str {
    static SOURCE: OnceLock<String> = OnceLock::new();
    SOURCE.get_or_init(|| "a".repeat(LIMIT_BYTES))
}

/// One cell of the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub input: Input,
    pub format: Format,
    pub quiet: Quiet,
    pub fix: FixMode,
    pub fixture: Fixture,
}

impl Cell {
    /// `input/format/quiet/fix-mode/fixture`, e.g. `file/json/loud/fix-check/partial`.
    pub fn id(&self) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.input.name(),
            self.format.name(),
            self.quiet.name(),
            self.fix.name(),
            self.fixture.name()
        )
    }

    /// Whether the cell runs on this platform.
    pub fn active(&self) -> bool {
        cfg!(unix) || !self.fixture.unix_only()
    }

    fn args(&self) -> Vec<&'static str> {
        let mut args = vec!["lint"];
        args.extend_from_slice(self.format.args());
        args.extend_from_slice(self.quiet.args());
        args.extend_from_slice(self.fix.args());
        args.push(match self.input {
            Input::Stdin => "-",
            Input::File => self.fixture.file_name(),
        });
        args
    }
}

/// The cells of one (input, fixture) group, in table order.
pub fn group_cells(input: Input, fixture: Fixture) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(20);
    for format in Format::ALL {
        for quiet in Quiet::ALL {
            for fix in FixMode::ALL {
                cells.push(Cell {
                    input,
                    format,
                    quiet,
                    fix,
                    fixture,
                });
            }
        }
    }
    cells
}

/// Every stdin and single-file cell, on every platform, in table order.
pub fn all_cells() -> Vec<Cell> {
    let mut cells = Vec::with_capacity(Input::ALL.len() * Fixture::ALL.len() * 20);
    for input in Input::ALL {
        for fixture in Fixture::ALL {
            cells.extend(group_cells(input, fixture));
        }
    }
    cells
}

/// The cells that run on this platform.
pub fn active_cells() -> Vec<Cell> {
    all_cells().into_iter().filter(Cell::active).collect()
}

// ── Running a cell ───────────────────────────────────────────────────────────

/// What one run recorded, after normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
    pub file: ObservedFile,
    pub tmp_paths: u32,
    pub tmp_names: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedFile {
    Unchanged,
    Changed(String),
    Missing,
}

/// Why a cell produced no observation.
#[derive(Debug)]
pub enum Skip {
    /// The read-only directory does not stop file creation (euid 0, or a filesystem
    /// that ignores mode bits), so the write-failure precondition cannot be built.
    ReadOnlyDirIsWritable,
}

/// A fresh fixture directory.
///
/// On Unix it is created under `/tmp`, not the platform temp dir: some messages print
/// the directory's absolute path, and the diagnostic renderer wraps a long path at
/// `/` boundaries, so a long, machine-specific temp root (macOS's
/// `/private/var/folders/…/T`) would move the line breaks from machine to machine. A
/// path under `/tmp` (`/private/tmp` on macOS) is short enough never to wrap.
pub fn fixture_dir() -> tempfile::TempDir {
    let dir = if cfg!(unix) {
        tempfile::Builder::new().tempdir_in("/tmp")
    } else {
        tempfile::tempdir()
    };
    dir.expect("create fixture tempdir")
}

/// Run one cell in a fresh fixture directory.
pub fn run_cell(cell: &Cell) -> Result<Observed, Skip> {
    let dir = fixture_dir();
    let source_path = dir.path().join(cell.fixture.file_name());
    std::fs::write(&source_path, cell.fixture.source()).expect("write fixture source");
    if let Some(config) = cell.fixture.config() {
        std::fs::write(dir.path().join("mds.json"), config).expect("write fixture mds.json");
    }
    let read_only = ReadOnlyDir::apply(cell.fixture, dir.path())?;

    let stdin = match cell.input {
        Input::Stdin => Some(cell.fixture.source().as_bytes()),
        Input::File => None,
    };
    let (exit, stdout, stderr) = run_mds(&cell.id(), dir.path(), &cell.args(), stdin);

    drop(read_only);
    let file = match std::fs::read(&source_path) {
        Ok(bytes) if bytes == cell.fixture.source().as_bytes() => ObservedFile::Unchanged,
        Ok(bytes) => ObservedFile::Changed(
            String::from_utf8(bytes)
                .unwrap_or_else(|_| panic!("{}: rewritten source is not UTF-8", cell.id())),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ObservedFile::Missing,
        Err(e) => panic!("{}: cannot read the source back: {e}", cell.id()),
    };

    let normalizer = Normalizer::for_dir(dir.path());
    let (stdout, out_paths, out_names) = normalizer.apply(&utf8(cell, "stdout", stdout));
    let (stderr, err_paths, err_names) = normalizer.apply(&utf8(cell, "stderr", stderr));
    Ok(Observed {
        exit,
        stdout,
        stderr,
        file,
        tmp_paths: out_paths + err_paths,
        tmp_names: out_names + err_names,
    })
}

fn utf8(cell: &Cell, label: &str, bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|_| panic!("{}: {label} is not UTF-8", cell.id()))
}

/// The write-failure fixture's directory is made read-only for the run and writable
/// again on drop, so a failing assertion never leaves an undeletable tempdir.
struct ReadOnlyDir {
    #[cfg(unix)]
    dir: Option<PathBuf>,
}

impl ReadOnlyDir {
    #[cfg(unix)]
    fn apply(fixture: Fixture, dir: &Path) -> Result<Self, Skip> {
        use std::os::unix::fs::PermissionsExt as _;
        if fixture != Fixture::WriteFail {
            return Ok(ReadOnlyDir { dir: None });
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))
            .expect("make the fixture dir read-only");
        let guard = ReadOnlyDir {
            dir: Some(dir.to_path_buf()),
        };
        // Precondition probe: creating a file must now fail.
        let probe = dir.join("write-probe");
        if std::fs::File::create(&probe).is_ok() {
            let _ = std::fs::remove_file(&probe);
            return Err(Skip::ReadOnlyDirIsWritable);
        }
        Ok(guard)
    }

    #[cfg(not(unix))]
    fn apply(fixture: Fixture, _dir: &Path) -> Result<Self, Skip> {
        assert!(
            !fixture.unix_only(),
            "unix-only fixture {} reached a non-unix run",
            fixture.name()
        );
        Ok(ReadOnlyDir {})
    }
}

impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(dir) = self.dir.take() {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// `mds lint --format json <file_name>` on `source` alone in a fresh directory, run
/// like a cell; returns (exit code, stdout). Used by the fixture markers.
pub fn lint_json_report(file_name: &str, source: &'static str) -> (i32, String) {
    let dir = fixture_dir();
    std::fs::write(dir.path().join(file_name), source).expect("write source");
    let id = format!("lint --format json {file_name}");
    let (exit, stdout, _) = run_mds(
        &id,
        dir.path(),
        &["lint", "--format", "json", file_name],
        None,
    );
    (exit, String::from_utf8(stdout).expect("UTF-8 stdout"))
}

/// Spawn `mds <args>` in `dir` with a cleared environment and piped streams, feeding
/// `stdin` when given; return (exit code, stdout, stderr). The wait is bounded by
/// `CHILD_DEADLINE`; `id` names the run in failure messages.
fn run_mds(
    id: &str,
    dir: &Path,
    args: &[&str],
    stdin: Option<&'static [u8]>,
) -> (i32, Vec<u8>, Vec<u8>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mds"));
    cmd.env_clear();
    let allowlist: &[&str] = if cfg!(windows) {
        &["PATH", "SystemRoot", "TEMP"]
    } else {
        &["PATH"]
    };
    for key in allowlist {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env("NO_COLOR", "1");
    cmd.current_dir(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("{id}: spawn mds: {e}"));
    let writer = match (child.stdin.take(), stdin) {
        (Some(mut pipe), Some(bytes)) => Some(std::thread::spawn(move || {
            // The child may exit before reading everything (an oversized input is
            // refused early); a broken pipe here is expected, not a failure.
            let _ = pipe.write_all(bytes);
        })),
        _ => None,
    };
    let mut out = child.stdout.take().expect("piped stdout");
    let mut err = child.stderr.take().expect("piped stderr");
    let out_reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = out.read_to_end(&mut v);
        v
    });
    let err_reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = err.read_to_end(&mut v);
        v
    });

    let deadline = Instant::now() + CHILD_DEADLINE;
    let max_polls = CHILD_DEADLINE.as_millis() / CHILD_POLL.as_millis() + 1;
    let mut status = None;
    for _ in 0..max_polls {
        if let Some(s) = child
            .try_wait()
            .unwrap_or_else(|e| panic!("{id}: wait for mds: {e}"))
        {
            status = Some(s);
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(CHILD_POLL);
    }
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "{id}: mds did not exit within {}s",
            CHILD_DEADLINE.as_secs()
        );
    };
    if let Some(writer) = writer {
        writer.join().expect("stdin writer thread");
    }
    let stdout = out_reader.join().expect("stdout reader thread");
    let stderr = err_reader.join().expect("stderr reader thread");
    let exit = status
        .code()
        .unwrap_or_else(|| panic!("{id}: mds ended without an exit code: {status}"));
    (exit, stdout, stderr)
}

// ── Normalization ────────────────────────────────────────────────────────────

pub const TMP_TOKEN: &str = "$TMP";
const TMP_NAME_PREFIX: &str = ".mds-tmp-";
const TMP_NAME_SUFFIX: &str = ".tmp";
const TMP_NAME_RANDOM_LEN: usize = 6;
const TMP_NAME_REPLACEMENT: &str = ".mds-tmp-RANDOM.tmp";

/// Rewrites the spellings of one temporary directory.
pub struct Normalizer {
    /// Longest first, so a longer spelling is replaced before any spelling it contains.
    spellings: Vec<String>,
}

impl Normalizer {
    pub fn for_dir(dir: &Path) -> Self {
        let mut bases: Vec<PathBuf> = vec![dir.to_path_buf()];
        if let Ok(canonical) = std::fs::canonicalize(dir) {
            bases.push(canonical);
        }
        Normalizer::from_spellings(bases.iter().filter_map(|p| p.to_str()))
    }

    /// Build from raw directory spellings; every derived variant is added.
    pub fn from_spellings<'a>(raw: impl IntoIterator<Item = &'a str>) -> Self {
        let mut forms: Vec<String> = Vec::new();
        for base in raw {
            let mut variants = vec![base.to_string()];
            if let Some(rest) = base.strip_prefix(r"\\?\UNC\") {
                variants.push(format!(r"\\{rest}"));
            } else if let Some(rest) = base.strip_prefix(r"\\?\") {
                variants.push(rest.to_string());
            }
            if let Some(rest) = base.strip_prefix("/private/") {
                variants.push(format!("/{rest}"));
            } else if base.starts_with('/') {
                variants.push(format!("/private{base}"));
            }
            for v in variants {
                forms.push(v.replace('\\', "\\\\"));
                forms.push(v.replace('\\', "/"));
                forms.push(v);
            }
        }
        forms.retain(|f| !f.is_empty());
        forms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        forms.dedup();
        Normalizer { spellings: forms }
    }

    /// Normalize one stream; returns (text, `$TMP` replacements, temp-name replacements).
    pub fn apply(&self, text: &str) -> (String, u32, u32) {
        let mut s = text.to_string();
        let mut paths = 0u32;
        for spelling in &self.spellings {
            let n = s.matches(spelling.as_str()).count();
            if n > 0 {
                paths += u32::try_from(n).expect("replacement count fits u32");
                s = s.replace(spelling.as_str(), TMP_TOKEN);
            }
        }
        let (s, names) = replace_tmp_names(&s);
        let s = if cfg!(windows) {
            s.replace("\\\\", "/").replace('\\', "/")
        } else {
            s
        };
        (s, paths, names)
    }
}

/// `.mds-tmp-` + exactly six ASCII alphanumerics + `.tmp` → `.mds-tmp-RANDOM.tmp`.
pub fn replace_tmp_names(text: &str) -> (String, u32) {
    let mut out = String::with_capacity(text.len());
    let mut count = 0u32;
    let mut rest = text;
    // Bounded: every iteration consumes at least one byte of `rest`.
    for _ in 0..=text.len() {
        let Some(at) = rest.find(TMP_NAME_PREFIX) else {
            break;
        };
        let after = &rest[at + TMP_NAME_PREFIX.len()..];
        let random = after.as_bytes().get(..TMP_NAME_RANDOM_LEN);
        let matches = random.is_some_and(|r| r.iter().all(u8::is_ascii_alphanumeric))
            && after[TMP_NAME_RANDOM_LEN..].starts_with(TMP_NAME_SUFFIX);
        if matches {
            out.push_str(&rest[..at]);
            out.push_str(TMP_NAME_REPLACEMENT);
            rest = &after[TMP_NAME_RANDOM_LEN + TMP_NAME_SUFFIX.len()..];
            count += 1;
        } else {
            let keep = at + TMP_NAME_PREFIX.len();
            out.push_str(&rest[..keep]);
            rest = &rest[keep..];
        }
    }
    out.push_str(rest);
    (out, count)
}

// ── Digests ──────────────────────────────────────────────────────────────────

/// FNV-1a, 64-bit.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// An owned digest of an observed stream (the comparison twin of [`Digest`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDigest {
    pub len: usize,
    pub lines: usize,
    pub fnv: u64,
    pub head: String,
    pub tail: String,
}

pub fn digest_of(text: &str) -> OwnedDigest {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let head_len: usize = lines.iter().take(DIGEST_HEAD_LINES).map(|l| l.len()).sum();
    let head_end = floor_boundary(text, head_len.min(DIGEST_HEAD_MAX_BYTES));
    let tail_len: usize = lines
        .iter()
        .rev()
        .take(DIGEST_TAIL_LINES)
        .map(|l| l.len())
        .sum();
    let tail_start = ceil_boundary(text, text.len() - tail_len.min(DIGEST_TAIL_MAX_BYTES));
    OwnedDigest {
        len: text.len(),
        lines: lines.len(),
        fnv: fnv1a64(text.as_bytes()),
        head: text[..head_end].to_string(),
        tail: text[tail_start..].to_string(),
    }
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    // Bounded: a UTF-8 character is at most four bytes.
    for _ in 0..4 {
        if s.is_char_boundary(i) {
            break;
        }
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    for _ in 0..4 {
        if s.is_char_boundary(i) {
            break;
        }
        i += 1;
    }
    i
}

// ── Comparison ───────────────────────────────────────────────────────────────

/// Lines of context printed on each side of the first difference.
const DIFF_CONTEXT: usize = 3;
/// Characters of one line printed in a failure message.
const DIFF_LINE_CHARS: usize = 160;

/// Compare an observation with its golden. The error names the cell and the part that
/// differs, with lengths and the first differing line ±3 lines — never a whole stream.
pub fn compare(cell_id: &str, golden: &Golden, obs: &Observed) -> Result<(), String> {
    let mut problems = Vec::new();
    if golden.exit != obs.exit {
        problems.push(format!(
            "exit: expected {}, actual {}",
            golden.exit, obs.exit
        ));
    }
    if golden.tmp_paths != obs.tmp_paths {
        problems.push(format!(
            "$TMP replacements: expected {}, actual {}",
            golden.tmp_paths, obs.tmp_paths
        ));
    }
    if golden.tmp_names != obs.tmp_names {
        problems.push(format!(
            "temp-name replacements: expected {}, actual {}",
            golden.tmp_names, obs.tmp_names
        ));
    }
    if let Err(e) = compare_stream("stdout", &golden.stdout, &obs.stdout) {
        problems.push(e);
    }
    if let Err(e) = compare_stream("stderr", &golden.stderr, &obs.stderr) {
        problems.push(e);
    }
    match (&golden.file, &obs.file) {
        (FileAfter::Unchanged, ObservedFile::Unchanged)
        | (FileAfter::Missing, ObservedFile::Missing) => {}
        (FileAfter::Changed(expected), ObservedFile::Changed(actual)) => {
            if let Err(e) = compare_stream("file", expected, actual) {
                problems.push(e);
            }
        }
        (expected, actual) => problems.push(format!(
            "file: expected {}, actual {}",
            file_kind(expected),
            observed_file_kind(actual)
        )),
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("cell {cell_id}:\n  {}", problems.join("\n  ")))
    }
}

fn file_kind(f: &FileAfter) -> String {
    match f {
        FileAfter::Unchanged => "unchanged".to_string(),
        FileAfter::Missing => "missing".to_string(),
        FileAfter::Changed(Stream::Text(t)) => format!("changed ({} bytes)", t.len()),
        FileAfter::Changed(Stream::Digest(d)) => format!("changed ({} bytes)", d.len),
    }
}

fn observed_file_kind(f: &ObservedFile) -> String {
    match f {
        ObservedFile::Unchanged => "unchanged".to_string(),
        ObservedFile::Missing => "missing".to_string(),
        ObservedFile::Changed(t) => format!("changed ({} bytes)", t.len()),
    }
}

pub fn compare_stream(label: &str, expected: &Stream, actual: &str) -> Result<(), String> {
    match expected {
        Stream::Text(e) => {
            if *e == actual {
                Ok(())
            } else {
                Err(format!(
                    "{label}: expected {} bytes, actual {} bytes; {}",
                    e.len(),
                    actual.len(),
                    first_difference(e, actual)
                ))
            }
        }
        Stream::Digest(e) => {
            let a = digest_of(actual);
            if e.len == a.len
                && e.lines == a.lines
                && e.fnv == a.fnv
                && e.head == a.head
                && e.tail == a.tail
            {
                return Ok(());
            }
            let where_ = if e.head != a.head {
                format!("in the stored head: {}", first_difference(e.head, &a.head))
            } else if e.tail != a.tail {
                format!("in the stored tail: {}", first_difference(e.tail, &a.tail))
            } else {
                format!(
                    "outside the stored head and tail (fnv expected {:#018x}, actual {:#018x})",
                    e.fnv, a.fnv
                )
            };
            Err(format!(
                "{label} (digest): expected {} bytes / {} lines, actual {} bytes / {} lines; \
                 differs {where_}",
                e.len, e.lines, a.len, a.lines
            ))
        }
    }
}

/// The first differing line (1-based) with ±`DIFF_CONTEXT` lines of each side.
pub fn first_difference(expected: &str, actual: &str) -> String {
    let e: Vec<&str> = expected.split_inclusive('\n').collect();
    let a: Vec<&str> = actual.split_inclusive('\n').collect();
    let at = e
        .iter()
        .zip(a.iter())
        .position(|(x, y)| x != y)
        .unwrap_or(e.len().min(a.len()));
    let from = at.saturating_sub(DIFF_CONTEXT);
    let show = |lines: &[&str]| -> String {
        let to = (at + DIFF_CONTEXT + 1).min(lines.len());
        let mut out = String::new();
        for (i, line) in lines.iter().enumerate().take(to).skip(from) {
            let clipped: String = line.chars().take(DIFF_LINE_CHARS).collect();
            let marker = if i == at { '>' } else { ' ' };
            let _ = write!(out, "\n    {marker}{:>5} {clipped:?}", i + 1);
        }
        if out.is_empty() {
            out.push_str("\n     (no line)");
        }
        out
    };
    format!(
        "first difference at line {}\n   expected:{}\n   actual:{}",
        at + 1,
        show(&e),
        show(&a)
    )
}

// ── Checking a group of cells ────────────────────────────────────────────────

/// At most this many failing cells are printed per group.
const MAX_REPORTED_FAILURES: usize = 5;

/// Run every active cell of one (input, fixture) group against its golden.
pub fn check_group(input: Input, fixture: Fixture, goldens: &[Golden], cells: &[(&str, u16)]) {
    let mut failures = Vec::new();
    let mut total = 0usize;
    for cell in group_cells(input, fixture) {
        if !cell.active() {
            continue;
        }
        let id = cell.id();
        let golden = lookup(&id, goldens, cells);
        match run_cell(&cell) {
            Ok(obs) => {
                total += 1;
                if let Err(e) = compare(&id, golden, &obs) {
                    failures.push(e);
                }
            }
            Err(Skip::ReadOnlyDirIsWritable) => {
                eprintln!(
                    "skipping {id}: a 0o555 directory accepts new files here (running as \
                     euid 0, or the filesystem ignores mode bits), so the write-failure \
                     precondition cannot be built"
                );
            }
        }
    }
    if !failures.is_empty() {
        let shown: Vec<&str> = failures
            .iter()
            .take(MAX_REPORTED_FAILURES)
            .map(String::as_str)
            .collect();
        panic!(
            "{} of {total} cells differ from their goldens (first {}):\n{}",
            failures.len(),
            shown.len(),
            shown.join("\n")
        );
    }
}

/// The golden a cell id maps to (the table is checked separately for completeness).
pub fn lookup<'g>(id: &str, goldens: &'g [Golden], cells: &[(&str, u16)]) -> &'g Golden {
    let index = cells
        .iter()
        .find(|(cell, _)| *cell == id)
        .map(|(_, index)| usize::from(*index))
        .unwrap_or_else(|| panic!("no golden for cell {id}"));
    goldens
        .get(index)
        .unwrap_or_else(|| panic!("cell {id} maps to golden {index}, which does not exist"))
}

// ── Generator ────────────────────────────────────────────────────────────────

/// Totals reported next to a generated module.
#[derive(Debug, Default)]
pub struct GenerationSummary {
    pub cells: usize,
    pub goldens: usize,
    /// Bytes of golden text, as [`golden_data_bytes`] counts them.
    pub data_bytes: usize,
    /// `$TMP` replacements summed over every cell (the path-leak baseline).
    pub tmp_paths: u64,
    /// Temp-name replacements summed over every cell.
    pub tmp_names: u64,
}

/// Bytes a stream contributes to the golden data (inline text, or a digest's head/tail).
fn stored_len(text: &str) -> usize {
    if text.len() <= DIGEST_THRESHOLD {
        text.len()
    } else {
        let d = digest_of(text);
        d.head.len() + d.tail.len()
    }
}

/// Run every cell and render the data module (goldens + cell table) as Rust source.
/// Strings are written with Rust's `Debug` escaping, so every literal is a single line
/// and every control character is a braced escape.
pub fn render_data_module(title: &str, cells: &[Cell]) -> (String, GenerationSummary) {
    let mut goldens: Vec<String> = Vec::new();
    let mut table: Vec<(String, usize)> = Vec::new();
    let mut summary = GenerationSummary {
        cells: cells.len(),
        ..GenerationSummary::default()
    };
    for cell in cells {
        let obs = match run_cell(cell) {
            Ok(obs) => obs,
            Err(Skip::ReadOnlyDirIsWritable) => panic!(
                "{}: the generator needs a non-root run where a 0o555 directory refuses \
                 new files",
                cell.id()
            ),
        };
        summary.tmp_paths += u64::from(obs.tmp_paths);
        summary.tmp_names += u64::from(obs.tmp_names);
        let literal = golden_literal(&obs);
        let index = match goldens.iter().position(|g| *g == literal) {
            Some(i) => i,
            None => {
                summary.data_bytes += stored_len(&obs.stdout)
                    + stored_len(&obs.stderr)
                    + match &obs.file {
                        ObservedFile::Changed(t) => stored_len(t),
                        ObservedFile::Unchanged | ObservedFile::Missing => 0,
                    };
                goldens.push(literal);
                goldens.len() - 1
            }
        };
        table.push((cell.id(), index));
    }
    let mut out = String::new();
    let _ = writeln!(out, "//! Generated `mds lint` goldens (#309): {title}.");
    out.push_str(
        "//!\n\
         //! Do not edit by hand. Regenerate with\n\
         //! `MDS_GOLDEN_PRINT=1 cargo nextest run -p mds-cli --test lint_golden \
         golden_print_mode --no-capture`,\n\
         //! copy the module printed between the BEGIN/END markers into this file, and name\n\
         //! every cell whose golden changed in the commit message.\n\n\
         use super::harness::{Digest, FileAfter, Golden, Stream};\n\n\
         #[rustfmt::skip]\n\
         pub const GOLDENS: &[Golden] = &[\n",
    );
    for (i, g) in goldens.iter().enumerate() {
        let _ = writeln!(out, "    /* {i} */ {g},");
    }
    out.push_str("];\n\n#[rustfmt::skip]\npub const CELLS: &[(&str, u16)] = &[\n");
    for (id, index) in &table {
        let _ = writeln!(out, "    ({id:?}, {index}),");
    }
    out.push_str("];\n");
    summary.goldens = goldens.len();
    (out, summary)
}

fn golden_literal(obs: &Observed) -> String {
    let file = match &obs.file {
        ObservedFile::Unchanged => "FileAfter::Unchanged".to_string(),
        ObservedFile::Missing => "FileAfter::Missing".to_string(),
        ObservedFile::Changed(t) => format!("FileAfter::Changed({})", stream_literal(t)),
    };
    format!(
        "Golden {{ exit: {}, stdout: {}, stderr: {}, file: {file}, tmp_paths: {}, tmp_names: {} }}",
        obs.exit,
        stream_literal(&obs.stdout),
        stream_literal(&obs.stderr),
        obs.tmp_paths,
        obs.tmp_names
    )
}

fn stream_literal(text: &str) -> String {
    if text.len() <= DIGEST_THRESHOLD {
        return format!("Stream::Text({text:?})");
    }
    let d = digest_of(text);
    format!(
        "Stream::Digest(Digest {{ len: {}, lines: {}, fnv: {:#018x}, head: {:?}, tail: {:?} }})",
        d.len, d.lines, d.fnv, d.head, d.tail
    )
}

/// Print a rendered module between markers on stderr, then its summary (the generator
/// never writes files).
pub fn print_module(name: &str, module: &str, summary: &GenerationSummary) {
    eprintln!("===== BEGIN {name} =====");
    eprint!("{module}");
    eprintln!("===== END {name} =====");
    eprintln!("summary for {name}: {summary:?}");
}

// ── Self-check helpers ───────────────────────────────────────────────────────

/// Bytes of golden text: inline streams, digest heads/tails and changed file bytes.
pub fn golden_data_bytes(goldens: &[Golden]) -> usize {
    fn stream(s: &Stream) -> usize {
        match s {
            Stream::Text(t) => t.len(),
            Stream::Digest(d) => d.head.len() + d.tail.len(),
        }
    }
    goldens
        .iter()
        .map(|g| {
            stream(&g.stdout)
                + stream(&g.stderr)
                + match &g.file {
                    FileAfter::Changed(s) => stream(s),
                    FileAfter::Unchanged | FileAfter::Missing => 0,
                }
        })
        .sum()
}

/// Every stored string of a golden (inline streams, digest heads/tails, file bytes).
pub fn golden_strings(g: &Golden) -> Vec<&'static str> {
    fn stream(s: &Stream, out: &mut Vec<&'static str>) {
        match s {
            Stream::Text(t) => out.push(t),
            Stream::Digest(d) => {
                out.push(d.head);
                out.push(d.tail);
            }
        }
    }
    let mut out = Vec::new();
    stream(&g.stdout, &mut out);
    stream(&g.stderr, &mut out);
    if let FileAfter::Changed(s) = &g.file {
        stream(s, &mut out);
    }
    out
}
