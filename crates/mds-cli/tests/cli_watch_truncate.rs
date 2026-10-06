//! `mds watch` never publishes the empty intermediate of a truncating save (#380).
//!
//! An editor that saves by truncate-then-write leaves the source empty for a moment.
//! A watcher that compiles in that moment publishes an empty output and announces a
//! rebuild of content nobody wrote. The watcher instead holds any rebuild while a
//! watched file — the entry, an imported partial, the `--vars` file, any source of a
//! watched directory — has gone from non-empty to empty, and compiles whatever is there
//! at a FIXED deadline from the first empty observation, so a file that really was
//! emptied is still published, and a stream of events cannot postpone that forever.
//!
//! Every hold here is `File::create` — `O_TRUNC` with the handle kept open — and the
//! output is polled DURING the hold, so an empty output that existed only briefly is
//! still seen.
//!
//! The claims are cadence-independent. The watcher's first empty observation cannot
//! precede the truncation, so an empty output read before [`EMPTY_HOLD_DEADLINE`] has
//! passed since the truncation is a violation whatever the scheduler did. The stricter
//! claims — never empty, exactly one rebuild — hold only when the hold measurably ended
//! before that deadline; a test thread descheduled past it leaves the watcher entitled
//! to publish, and the test then says so and checks the deadline claim alone.
//!
//! Counts and absences are read behind an ordered anchor (`common::ORDER_MARKER_SOURCE`)
//! with `finish_text`, never from a snapshot of a live pipe.

mod common;
use common::{
    count_occurrences, mds_bin, spawn_watch_ready, wait_for_tap, write_atomic, ChildGuard,
    StderrTap, ORDER_MARKER_LINE, ORDER_MARKER_SOURCE,
};

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────────────

/// How long a test holds a watched file truncated before it writes the content — a
/// slow editor save. Deliberately half of [`EMPTY_HOLD_DEADLINE`], so a hold that ends
/// on time ends well inside it.
const TRUNCATE_HOLD: Duration = Duration::from_millis(500);

/// The fixed deadline, from its first empty observation, after which the watcher
/// compiles a watched file that is still empty. The product's own constant of the
/// same name carries this value.
const EMPTY_HOLD_DEADLINE: Duration = Duration::from_secs(1);

/// Upper bound on an inotify-delivered effect — the same failure bound, not a
/// synchroniser, as `TIMEOUT` in `cli_watch.rs`.
const TIMEOUT: Duration = Duration::from_secs(2);

/// How often the output file is read while a hold lasts.
const OUTPUT_POLL: Duration = Duration::from_millis(5);

/// How often the repeated-truncation test truncates the already-empty entry, and how
/// many times: every 100ms for five seconds.
const REPEAT_EVERY: Duration = Duration::from_millis(100);
const REPEAT_ROUNDS: u32 = 50;

/// At most this many distinct output states are kept for a failure message.
const MAX_STATES: usize = 32;

/// The one line `mds watch` prints when it publishes an empty output over a non-empty
/// one — suppressed by `--quiet`, like every other watch status line. The output's
/// label follows it after a space: `<this text> <output>`.
const EMPTY_OUTPUT_NOTICE: &str = "Wrote an empty output:";

// ── Harness ─────────────────────────────────────────────────────────────────

/// Spawn a watcher, block until it reports readiness, and wrap it in a `ChildGuard`.
fn spawn_ready(cmd: &mut Command) -> (ChildGuard, StderrTap) {
    let (child, tap, stdout_tap) = spawn_watch_ready(cmd);
    assert!(stdout_tap.is_none(), "these tests never pipe stdout");
    (ChildGuard(child), tap)
}

/// What the test read from one output file, in order.
#[derive(Default)]
struct OutputLog {
    /// Distinct successive contents with the instant each was FIRST read (at most
    /// [`MAX_STATES`]).
    states: Vec<(String, Instant)>,
    /// When the output was first read empty.
    first_empty: Option<Instant>,
}

impl OutputLog {
    /// Read `path` once and record what it holds. A missing file records nothing.
    fn record(&mut self, path: &Path) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let now = Instant::now();
        if content.is_empty() && self.first_empty.is_none() {
            self.first_empty = Some(now);
        }
        let changed = self.states.last().is_none_or(|(last, _)| *last != content);
        if changed && self.states.len() < MAX_STATES {
            self.states.push((content.clone(), now));
        }
        Some(content)
    }

    /// When a state holding `needle` was first read.
    fn first_holding(&self, needle: &str) -> Option<Instant> {
        self.states
            .iter()
            .find(|(content, _)| content.contains(needle))
            .map(|&(_, at)| at)
    }

    /// The contents read, in order, for a failure message.
    fn contents(&self) -> Vec<&str> {
        self.states.iter().map(|(c, _)| c.as_str()).collect()
    }

    /// Keep reading `path` for `span`.
    fn poll_for(&mut self, path: &Path, span: Duration) {
        let until = Instant::now() + span;
        // Bounded by `span`: at most span / OUTPUT_POLL iterations.
        while Instant::now() < until {
            self.record(path);
            std::thread::sleep(OUTPUT_POLL);
        }
    }

    /// Keep reading `path` until it holds `needle`. PANICS after `timeout`, naming the
    /// caller and every state read.
    #[track_caller]
    fn wait_for(&mut self, path: &Path, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        // Bounded by `timeout`: at most timeout / OUTPUT_POLL iterations.
        loop {
            if self.record(path).is_some_and(|c| c.contains(needle)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: {} never held {needle:?} within {timeout:?}; states read: {:?}",
                std::panic::Location::caller(),
                path.display(),
                self.contents()
            );
            std::thread::sleep(OUTPUT_POLL);
        }
    }
}

/// A watched file held truncated: `File::create` emptied it and the handle stays open.
struct Hold {
    file: File,
    started: Instant,
}

/// When a hold began, and when its content was written.
#[derive(Clone, Copy)]
struct HoldTiming {
    started: Instant,
    written: Instant,
}

impl Hold {
    /// Truncate `path` and keep it open. `started` is taken BEFORE the truncation, so
    /// every observation of the empty file is at or after it.
    fn start(path: &Path) -> Hold {
        let started = Instant::now();
        let file = File::create(path)
            .unwrap_or_else(|e| panic!("cannot truncate {}: {e}", path.display()));
        Hold { file, started }
    }

    /// Write `content` through the held handle, then close it.
    fn write_and_close(mut self, content: &str) -> HoldTiming {
        self.file
            .write_all(content.as_bytes())
            .expect("write through the held handle");
        let written = Instant::now();
        drop(self.file);
        HoldTiming {
            started: self.started,
            written,
        }
    }
}

impl HoldTiming {
    /// Whether the content was written before the watcher's deadline could fire: its
    /// first empty observation is at or after `started`, so its deadline is too.
    fn ended_before_deadline(&self) -> bool {
        self.written.duration_since(self.started) < EMPTY_HOLD_DEADLINE
    }

    fn held(&self) -> Duration {
        self.written.duration_since(self.started)
    }
}

/// The deadline claim, which holds under ANY scheduling: an empty output read before
/// [`EMPTY_HOLD_DEADLINE`] had passed since the truncation was published by a watcher
/// that did not hold.
#[track_caller]
fn assert_no_empty_before_the_deadline(log: &OutputLog, started: Instant, what: &str) {
    if let Some(empty_at) = log.first_empty {
        let after = empty_at.duration_since(started);
        assert!(
            after >= EMPTY_HOLD_DEADLINE,
            "{what}: an empty output was published {after:?} after the truncation, before \
             the {EMPTY_HOLD_DEADLINE:?} deadline could have passed; states read: {:?}",
            log.contents()
        );
    }
}

/// Whether the stricter claims apply to this hold. When they do not, the test says so
/// on stderr — where a failing run shows it — and checks the deadline claim only.
fn strict_claims_apply(timing: &HoldTiming, what: &str) -> bool {
    let strict = timing.ended_before_deadline();
    if !strict {
        eprintln!(
            "{what}: the hold lasted {:?}, past the {EMPTY_HOLD_DEADLINE:?} deadline (the \
             test thread was descheduled); only the deadline claim is checked",
            timing.held()
        );
    }
    strict
}

/// Write the order marker to `src` and return everything the watcher wrote to stderr
/// through its diagnostic — every earlier rebuild's lines included.
#[track_caller]
fn stderr_through_the_marker(src: &Path, tap: StderrTap, child: &mut ChildGuard) -> String {
    write_atomic(src, ORDER_MARKER_SOURCE);
    wait_for_tap(&tap, ORDER_MARKER_LINE, TIMEOUT);
    tap.finish_text(child)
}

/// Everything stderr held before the order marker's first diagnostic began.
///
/// Only the start of the marker is a boundary: one edit can reach the watcher as more
/// than one event, and a failed compile is reported every time, so the marker's
/// diagnostic may repeat after it.
fn before_the_marker(stderr: &str) -> &str {
    let at = stderr
        .find(ORDER_MARKER_LINE)
        .expect("the order marker's diagnostic is on stderr");
    // A diagnostic opens with its code line (`mds::undefined_var`) above the message.
    let head = &stderr[..at];
    head.rfind("mds::").map_or(head, |code| &head[..code])
}

/// While a rebuild is held nothing is printed: before the order marker, stderr holds
/// no diagnostic, only startup lines and exactly `rebuilds` `Recompiled` lines.
#[track_caller]
fn assert_holding_printed_nothing(stderr: &str, rebuilds: usize, what: &str) {
    let before = before_the_marker(stderr);
    assert_eq!(
        count_occurrences(before, "mds::"),
        0,
        "{what}: no diagnostic may be printed while the file is held empty; \
         stderr:\n{stderr}"
    );
    let lines: Vec<&str> = before.lines().filter(|l| !l.trim().is_empty()).collect();
    let unexpected: Vec<&&str> = lines
        .iter()
        .filter(|l| {
            !(l.starts_with("Watching ")
                || l.starts_with("Compiled to ")
                || l.starts_with("Recompiled "))
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "{what}: holding a rebuild prints nothing; unexpected lines {unexpected:?}; \
         stderr:\n{stderr}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("Recompiled "))
            .count(),
        rebuilds,
        "{what}: exactly {rebuilds} rebuild(s) for the content finally written; \
         stderr:\n{stderr}"
    );
}

// ── The entry held truncated ────────────────────────────────────────────────

/// Hold the entry truncated for [`TRUNCATE_HOLD`], then write: the output never goes
/// empty, and exactly one `Recompiled` line is printed, for the final content.
fn held_truncate_of_the_entry(extra: &[&str], what: &str) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "version 1\n").unwrap();
    let out = dir.path().join("t.md");

    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&src)
            .args(extra)
            .stdout(Stdio::null()),
    );
    let mut log = OutputLog::default();
    log.wait_for(&out, "version 1", TIMEOUT);

    let hold = Hold::start(&src);
    log.poll_for(&out, TRUNCATE_HOLD);
    let timing = hold.write_and_close("version 2\n");
    log.wait_for(&out, "version 2", TIMEOUT);
    // A little longer: an empty output published late would still be seen.
    log.poll_for(&out, TRUNCATE_HOLD);

    let stderr = stderr_through_the_marker(&src, tap, &mut child);
    assert_no_empty_before_the_deadline(&log, timing.started, what);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "version 2\n",
        "{what}: the output holds the content finally written"
    );
    if strict_claims_apply(&timing, what) {
        assert!(
            log.first_empty.is_none(),
            "{what}: the output must never be empty while the entry is held truncated; \
             states read: {:?}; stderr:\n{stderr}",
            log.contents()
        );
        assert_holding_printed_nothing(&stderr, 1, what);
    }
}

#[test]
fn held_truncate_of_the_entry_at_debounce_0_never_publishes_an_empty_output() {
    held_truncate_of_the_entry(&["--debounce", "0"], "--debounce 0");
}

#[test]
fn held_truncate_of_the_entry_at_the_default_debounce_never_publishes_an_empty_output() {
    held_truncate_of_the_entry(&[], "default debounce");
}

#[test]
fn held_truncate_of_the_entry_with_poll_interval_0_never_publishes_an_empty_output() {
    held_truncate_of_the_entry(
        &["--debounce", "0", "--poll-interval", "0"],
        "--debounce 0 --poll-interval 0",
    );
}

// ── A dependency held truncated ─────────────────────────────────────────────

/// A dependency of a source to hold truncated.
struct DependencyCase<'a> {
    what: &'a str,
    /// The path `mds watch` is given: the source, or the directory it is in.
    watched: &'a Path,
    /// The source that reads the dependency; the order marker is written to it.
    source: &'a Path,
    /// The source's output.
    output: &'a Path,
    /// The file held truncated: a dependency of the source, or the source itself.
    dependency: &'a Path,
    /// What is written through the held handle.
    rewrite: &'a str,
    /// Extra `mds watch` arguments.
    extra: &'a [&'a str],
    /// A needle the output holds before the hold, and one it holds after the rewrite.
    before: &'a str,
    after: &'a str,
}

/// Hold a dependency of a source truncated, then write it: no compile runs while it is
/// empty — no diagnostic, no `Recompiled`, the output untouched — and one rebuild
/// follows the write.
fn held_truncate_of_a_dependency(case: &DependencyCase<'_>) {
    let what = case.what;
    let out = case.output;
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(case.watched)
            .args(case.extra)
            .args(["--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let mut log = OutputLog::default();
    log.wait_for(out, case.before, TIMEOUT);

    let hold = Hold::start(case.dependency);
    log.poll_for(out, TRUNCATE_HOLD);
    let timing = hold.write_and_close(case.rewrite);
    log.wait_for(out, case.after, TIMEOUT);

    let stderr = stderr_through_the_marker(case.source, tap, &mut child);
    assert_no_empty_before_the_deadline(&log, timing.started, what);
    if strict_claims_apply(&timing, what) {
        assert_eq!(
            log.states.len(),
            2,
            "{what}: the output changes once, from the old content straight to the new; \
             states read: {:?}",
            log.contents()
        );
        assert_holding_printed_nothing(&stderr, 1, what);
    }
}

#[test]
fn held_truncate_of_an_imported_partial_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let partial = dir.path().join("_p.mds");
    std::fs::write(
        &partial,
        "@define val():\nPartial one\n@end\n\n@export val\n",
    )
    .unwrap();
    let entry = dir.path().join("t.mds");
    std::fs::write(&entry, "@import \"./_p.mds\" as p\n{{p.val()}}\n").unwrap();

    held_truncate_of_a_dependency(&DependencyCase {
        what: "an imported partial",
        watched: &entry,
        source: &entry,
        output: &dir.path().join("t.md"),
        dependency: &partial,
        rewrite: "@define val():\nPartial two\n@end\n\n@export val\n",
        extra: &[],
        before: "Partial one",
        after: "Partial two",
    });
}

#[test]
fn held_truncate_of_the_vars_file_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "one"}"#).unwrap();
    let entry = dir.path().join("t.mds");
    std::fs::write(&entry, "Vars {{v}}\n").unwrap();

    held_truncate_of_a_dependency(&DependencyCase {
        what: "the vars file",
        watched: &entry,
        source: &entry,
        output: &dir.path().join("t.md"),
        dependency: &vars,
        rewrite: r#"{"v": "two"}"#,
        extra: &["--vars", vars.to_str().unwrap()],
        before: "Vars one",
        after: "Vars two",
    });
}

/// In directory mode the `--vars` file, which no source of the directory is, holds the
/// batch back while it is empty as a source would: no compile, no error, no rebuild until
/// it is written (#380).
#[test]
fn dir_mode_held_truncate_of_the_vars_file_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "one"}"#).unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let source = root.join("t.mds");
    std::fs::write(&source, "Vars {{v}}\n").unwrap();
    let out = dir.path().join("out");

    held_truncate_of_a_dependency(&DependencyCase {
        what: "directory mode, the vars file",
        watched: &root,
        source: &source,
        output: &out.join("t.md"),
        dependency: &vars,
        rewrite: r#"{"v": "two"}"#,
        extra: &[
            "--out-dir",
            out.to_str().unwrap(),
            "--vars",
            vars.to_str().unwrap(),
        ],
        before: "Vars one",
        after: "Vars two",
    });
}

// ── A dependency truncated after its rebuild looked ─────────────────────────

/// Hold a dependency of a source truncated while a rebuild is under way — after it looked
/// and found every file full, before it read any — then write it. The debug build's pause
/// between a rebuild's look and its reads (`MDS_TEST_PAUSE_AFTER_LOOK`) makes the window
/// certain; outside a test, a save that lands while an earlier event's rebuild runs opens
/// it. The rebuild reads the file empty and fails — the `--vars` file cannot be loaded, or
/// the import no longer defines what the source uses — and is held as a rebuild whose look
/// found the file empty is: no diagnostic, no `Recompiled`, the output untouched, and one
/// rebuild follows the write.
#[cfg(debug_assertions)]
fn held_truncate_after_the_look(case: &DependencyCase<'_>) {
    held_truncate_in_a_pause(case, case.source, "MDS_TEST_PAUSE_AFTER_LOOK");
}

/// Save `saved` as it is, truncate the dependency while the rebuild that save starts is
/// stopped at the debug build's `pause` — the variable of the pause after a rebuild's look,
/// or of the one after a directory batch's split — then write it: no diagnostic, no
/// `Recompiled`, the output untouched, and one rebuild follows the write.
#[cfg(debug_assertions)]
fn held_truncate_in_a_pause(case: &DependencyCase<'_>, saved: &Path, pause: &str) {
    let what = case.what;
    let out = case.output;
    // Outside every watched directory, so the pause's files raise no event.
    let gate = tempfile::tempdir().unwrap();
    let (go, paused) = (gate.path().join("go"), gate.path().join("go.paused"));
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(case.watched)
            .args(case.extra)
            .args(["--debounce", "0", "--poll-interval", "0"])
            .env(pause, &go)
            .stdout(Stdio::null()),
    );
    let mut log = OutputLog::default();
    log.wait_for(out, case.before, TIMEOUT);

    // The file saved as it is: its rebuild looks, finds every file full, and pauses.
    let content = std::fs::read_to_string(saved).unwrap();
    write_atomic(saved, &content);
    let paused_by = Instant::now() + TIMEOUT;
    // Bounded by TIMEOUT: at most TIMEOUT / OUTPUT_POLL iterations.
    while !paused.exists() {
        assert!(
            Instant::now() < paused_by,
            "{what}: setup: no rebuild paused after its look; stderr: {}",
            tap.text()
        );
        std::thread::sleep(OUTPUT_POLL);
    }
    let hold = Hold::start(case.dependency);
    std::fs::write(&go, "").unwrap();
    log.poll_for(out, TRUNCATE_HOLD);
    let timing = hold.write_and_close(case.rewrite);
    log.wait_for(out, case.after, TIMEOUT);

    let stderr = stderr_through_the_marker(case.source, tap, &mut child);
    assert_no_empty_before_the_deadline(&log, timing.started, what);
    if strict_claims_apply(&timing, what) {
        assert_eq!(
            log.states.len(),
            2,
            "{what}: the output changes once, from the old content straight to the new; \
             states read: {:?}",
            log.contents()
        );
        assert_holding_printed_nothing(&stderr, 1, what);
    }
}

#[cfg(debug_assertions)]
#[test]
fn an_imported_partial_truncated_after_the_look_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let partial = dir.path().join("_p.mds");
    std::fs::write(
        &partial,
        "@define val():\nPartial one\n@end\n\n@export val\n",
    )
    .unwrap();
    let entry = dir.path().join("t.mds");
    std::fs::write(&entry, "@import \"./_p.mds\" as p\n{{p.val()}}\n").unwrap();

    held_truncate_after_the_look(&DependencyCase {
        what: "an imported partial, truncated after the look",
        watched: &entry,
        source: &entry,
        output: &dir.path().join("t.md"),
        dependency: &partial,
        rewrite: "@define val():\nPartial two\n@end\n\n@export val\n",
        extra: &[],
        before: "Partial one",
        after: "Partial two",
    });
}

#[cfg(debug_assertions)]
#[test]
fn the_vars_file_truncated_after_the_look_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "one"}"#).unwrap();
    let entry = dir.path().join("t.mds");
    std::fs::write(&entry, "Vars {{v}}\n").unwrap();

    held_truncate_after_the_look(&DependencyCase {
        what: "the vars file, truncated after the look",
        watched: &entry,
        source: &entry,
        output: &dir.path().join("t.md"),
        dependency: &vars,
        rewrite: r#"{"v": "two"}"#,
        extra: &["--vars", vars.to_str().unwrap()],
        before: "Vars one",
        after: "Vars two",
    });
}

#[cfg(debug_assertions)]
#[test]
fn dir_mode_the_vars_file_truncated_after_the_look_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "one"}"#).unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let source = root.join("t.mds");
    std::fs::write(&source, "Vars {{v}}\n").unwrap();
    let out = dir.path().join("out");

    held_truncate_after_the_look(&DependencyCase {
        what: "directory mode, the vars file, truncated after the look",
        watched: &root,
        source: &source,
        output: &out.join("t.md"),
        dependency: &vars,
        rewrite: r#"{"v": "two"}"#,
        extra: &[
            "--out-dir",
            out.to_str().unwrap(),
            "--vars",
            vars.to_str().unwrap(),
        ],
        before: "Vars one",
        after: "Vars two",
    });
}

#[cfg(debug_assertions)]
#[test]
fn dir_mode_an_imported_partial_truncated_after_the_look_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let partial = root.join("_p.mds");
    std::fs::write(
        &partial,
        "@define val():\nPartial one\n@end\n\n@export val\n",
    )
    .unwrap();
    let source = root.join("t.mds");
    std::fs::write(&source, "@import \"./_p.mds\" as p\n{{p.val()}}\n").unwrap();
    let out = dir.path().join("out");

    held_truncate_after_the_look(&DependencyCase {
        what: "directory mode, an imported partial, truncated after the look",
        watched: &root,
        source: &source,
        output: &out.join("t.md"),
        dependency: &partial,
        rewrite: "@define val():\nPartial two\n@end\n\n@export val\n",
        extra: &["--out-dir", out.to_str().unwrap()],
        before: "Partial one",
        after: "Partial two",
    });
}

// ── A file truncated while a directory batch runs ───────────────────────────

/// In directory mode, the `--vars` file truncated while a batch runs — after the batch read
/// it whole, before the batch ends — holds the rebuilds its own events start, as a look
/// that found it empty does: the batch compiled with what it read and ended with the file
/// empty, which it must not take for the file it read. The debug build's pause between a
/// batch's split and its compile (`MDS_TEST_PAUSE_AFTER_BATCH_SPLIT`), after the batch's
/// `--vars` load, makes the window certain; outside a test, a truncating save that lands
/// while an earlier event's batch compiles opens it.
#[cfg(debug_assertions)]
#[test]
fn dir_mode_the_vars_file_truncated_after_its_batch_read_it_triggers_nothing_until_written() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "one"}"#).unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let source = root.join("t.mds");
    std::fs::write(&source, "Vars {{v}}\n").unwrap();
    let out = dir.path().join("out");

    held_truncate_in_a_pause(
        &DependencyCase {
            what: "directory mode, the vars file, truncated after its batch read it",
            watched: &root,
            source: &source,
            output: &out.join("t.md"),
            dependency: &vars,
            rewrite: r#"{"v": "two"}"#,
            extra: &[
                "--out-dir",
                out.to_str().unwrap(),
                "--vars",
                vars.to_str().unwrap(),
            ],
            before: "Vars one",
            after: "Vars two",
        },
        &source,
        "MDS_TEST_PAUSE_AFTER_BATCH_SPLIT",
    );
}

/// In directory mode, a source truncated while a batch that rebuilds another source runs —
/// after the batch looked, before it ends — is held when its own events come, as one a
/// look found empty is: the batch never read it and ended with it empty, which it must not
/// take for the file the session last saw. The debug build's pause after a batch's look
/// (`MDS_TEST_PAUSE_AFTER_LOOK`) makes the window certain; outside a test, a truncating
/// save that lands while an earlier event's batch rebuilds another source opens it.
#[cfg(debug_assertions)]
#[test]
fn dir_mode_a_source_truncated_while_a_batch_rebuilds_another_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let a = root.join("a.mds");
    let b = root.join("b.mds");
    std::fs::write(&a, "A one\n").unwrap();
    std::fs::write(&b, "B one\n").unwrap();
    let out = dir.path().join("out");

    held_truncate_in_a_pause(
        &DependencyCase {
            what: "directory mode, a source truncated while another's batch runs",
            watched: &root,
            source: &a,
            output: &out.join("a.md"),
            dependency: &a,
            rewrite: "A two\n",
            extra: &["--out-dir", out.to_str().unwrap()],
            before: "A one",
            after: "A two",
        },
        &b,
        "MDS_TEST_PAUSE_AFTER_LOOK",
    );
}

// ── Directory mode defers the whole batch ───────────────────────────────────

/// In directory mode a source held truncated defers every rebuild, not only its own:
/// another source edited during the hold is published only after the held one is
/// written, and neither output is ever empty.
#[test]
fn dir_mode_held_truncate_defers_the_whole_batch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let a = root.join("a.mds");
    let b = root.join("b.mds");
    std::fs::write(&a, "A one\n").unwrap();
    std::fs::write(&b, "B one\n").unwrap();
    let out = dir.path().join("out");
    let (a_out, b_out) = (out.join("a.md"), out.join("b.md"));

    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&root)
            .arg("--out-dir")
            .arg(&out)
            .args(["--debounce", "0"])
            .stdout(Stdio::null()),
    );
    let (mut a_log, mut b_log) = (OutputLog::default(), OutputLog::default());
    a_log.wait_for(&a_out, "A one", TIMEOUT);
    b_log.wait_for(&b_out, "B one", TIMEOUT);

    let hold = Hold::start(&a);
    // Into the hold, edit the other source, then keep reading both outputs. The edit is
    // always made, before a.mds is written, however late a descheduled thread comes back.
    let edit_after = TRUNCATE_HOLD / 5;
    let edit_at = hold.started + edit_after;
    let until = hold.started + TRUNCATE_HOLD;
    let mut edited_at: Option<Instant> = None;
    // Bounded by TRUNCATE_HOLD: at most TRUNCATE_HOLD / OUTPUT_POLL iterations, and one
    // more that edits when the thread came back after `until` without having edited.
    while edited_at.is_none() || Instant::now() < until {
        if edited_at.is_none() && Instant::now() >= edit_at {
            write_atomic(&b, "B two\n");
            edited_at = Some(Instant::now());
        }
        a_log.record(&a_out);
        b_log.record(&b_out);
        std::thread::sleep(OUTPUT_POLL);
    }
    let edited_at = edited_at.expect("the loop edits before it ends");
    let timing = hold.write_and_close("A two\n");
    a_log.wait_for(&a_out, "A two", TIMEOUT);
    b_log.wait_for(&b_out, "B two", TIMEOUT);

    let stderr = stderr_through_the_marker(&a, tap, &mut child);
    let what = "directory mode";
    assert_no_empty_before_the_deadline(&a_log, timing.started, what);
    // An edit made once the deadline could have passed may be published at once, as an
    // empty output then may: the strict claims need both in time.
    let edited_in_time = edited_at.duration_since(timing.started) < EMPTY_HOLD_DEADLINE;
    if strict_claims_apply(&timing, what) && edited_in_time {
        assert!(
            a_log.first_empty.is_none(),
            "{what}: a.md must never be empty while a.mds is held truncated; states: {:?}",
            a_log.contents()
        );
        let b_published = b_log
            .first_holding("B two")
            .expect("b.md reached its new content");
        assert!(
            b_published >= timing.written,
            "{what}: the other source's edit must wait for the held one — b.md held \
             `B two` {:?} before a.mds was written; stderr:\n{stderr}",
            timing.written.duration_since(b_published)
        );
        assert_holding_printed_nothing(&stderr, 2, what);
    }
}

/// A source created while another is held empty is compiled when the hold ends, though a
/// batch held with it changed the `--vars` file: the batch that ends the hold recompiles
/// every source, the ones created during the hold included.
#[test]
fn dir_mode_a_source_created_during_a_hold_is_compiled_with_a_vars_edit() {
    let dir = tempfile::tempdir().unwrap();
    let vars = dir.path().join("vars.json");
    std::fs::write(&vars, r#"{"v": "one"}"#).unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let a = root.join("a.mds");
    std::fs::write(&a, "A {{v}}\n").unwrap();
    let out = dir.path().join("out");
    let (a_out, new_out) = (out.join("a.md"), out.join("new.md"));

    let (_child, _tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&root)
            .arg("--out-dir")
            .arg(&out)
            .arg("--vars")
            .arg(&vars)
            // No idle tick: its first full walk would find the new source whatever the
            // batch did.
            .args(["--debounce", "0", "--poll-interval", "0"])
            .stdout(Stdio::null()),
    );
    OutputLog::default().wait_for(&a_out, "A one", TIMEOUT);

    // a.mds emptied and left so: every batch is held until the deadline, which compiles
    // the files as they are.
    std::fs::write(&a, "").unwrap();
    std::fs::write(root.join("new.mds"), "New {{v}}\n").unwrap();
    write_atomic(&vars, r#"{"v": "two"}"#);

    OutputLog::default().wait_for(&new_out, "New two", EMPTY_HOLD_DEADLINE + TIMEOUT);
}

/// A source truncated while its rebuild is under way — after the rebuild looked and found
/// it full, before its compile read it — is held as one the look found empty is: the
/// empty read is never published, and the write that follows is. The debug build's pause
/// between a directory batch's look and its compile (`MDS_TEST_PAUSE_AFTER_BATCH_SPLIT`)
/// makes the window certain.
#[cfg(debug_assertions)]
#[test]
fn dir_mode_a_source_truncated_after_its_rebuild_looked_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let a = root.join("a.mds");
    std::fs::write(&a, "A one\n").unwrap();
    let out = dir.path().join("out");
    let a_out = out.join("a.md");
    let (go, paused) = (dir.path().join("go"), dir.path().join("go.paused"));

    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&root)
            .arg("--out-dir")
            .arg(&out)
            .args(["--debounce", "0", "--poll-interval", "0"])
            .env("MDS_TEST_PAUSE_AFTER_BATCH_SPLIT", &go)
            .stdout(Stdio::null()),
    );
    let mut log = OutputLog::default();
    log.wait_for(&a_out, "A one", TIMEOUT);

    // A save: its rebuild looks, finds a.mds full, and pauses before compiling it.
    write_atomic(&a, "A two\n");
    let paused_by = Instant::now() + TIMEOUT;
    // Bounded by TIMEOUT: at most TIMEOUT / OUTPUT_POLL iterations.
    while !paused.exists() {
        assert!(
            Instant::now() < paused_by,
            "setup: the rebuild never paused; stderr: {}",
            tap.text()
        );
        std::thread::sleep(OUTPUT_POLL);
    }
    let hold = Hold::start(&a);
    std::fs::write(&go, "").unwrap();
    log.poll_for(&a_out, TRUNCATE_HOLD);
    let timing = hold.write_and_close("A three\n");
    log.wait_for(&a_out, "A three", TIMEOUT);

    let stderr = stderr_through_the_marker(&a, tap, &mut child);
    let what = "directory mode, truncated after the look";
    assert_no_empty_before_the_deadline(&log, timing.started, what);
    if strict_claims_apply(&timing, what) {
        assert!(
            log.first_empty.is_none(),
            "{what}: a.md must never be empty while a.mds is held truncated; states: {:?}",
            log.contents()
        );
        assert_holding_printed_nothing(&stderr, 1, what);
    }
}

// ── The deadline ────────────────────────────────────────────────────────────

/// Truncate the entry and close it without writing: the empty output IS published —
/// within the deadline plus the delivery bound — with one notice, which `--quiet`
/// suppresses.
fn truncate_and_close(extra: &[&str], quiet: bool, what: &str) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "version 1\n").unwrap();
    let out = dir.path().join("t.md");

    let mut cmd = mds_bin();
    cmd.arg("watch")
        .arg(&src)
        .args(["--debounce", "0"])
        .args(extra);
    if quiet {
        cmd.arg("--quiet");
    }
    let (mut child, tap) = spawn_ready(cmd.stdout(Stdio::null()));
    let mut log = OutputLog::default();
    log.wait_for(&out, "version 1", TIMEOUT);

    let started = Instant::now();
    drop(File::create(&src).expect("truncate the entry"));
    // Bounded: the deadline plus one inotify delivery.
    let bound = EMPTY_HOLD_DEADLINE + TIMEOUT;
    let until = started + bound;
    // Bounded by `bound`: at most bound / OUTPUT_POLL iterations.
    while log.first_empty.is_none() && Instant::now() < until {
        log.record(&out);
        std::thread::sleep(OUTPUT_POLL);
    }
    let empty_at = log.first_empty.unwrap_or_else(|| {
        panic!(
            "{what}: an emptied entry must be published within {bound:?}; states read: {:?}",
            log.contents()
        )
    });

    let stderr = stderr_through_the_marker(&src, tap, &mut child);
    let notices = if quiet { 0 } else { 1 };
    assert_eq!(
        count_occurrences(&stderr, EMPTY_OUTPUT_NOTICE),
        notices,
        "{what}: publishing the empty output prints {notices} notice(s) (published \
         {:?} after the truncation); stderr:\n{stderr}",
        empty_at.duration_since(started)
    );
}

#[test]
fn truncate_and_close_publishes_the_empty_output_within_the_deadline_with_one_notice() {
    truncate_and_close(&[], false, "default");
}

/// No idle tick, and no event arrives after the close: only a timer the deadline owns
/// can wake the watcher to publish.
#[test]
fn truncate_and_close_with_poll_interval_0_publishes_the_empty_output_on_its_own_deadline() {
    truncate_and_close(&["--poll-interval", "0"], false, "--poll-interval 0");
}

#[test]
fn truncate_and_close_under_quiet_publishes_the_empty_output_without_a_notice() {
    truncate_and_close(&[], true, "--quiet");
}

/// Truncate the already-empty entry every [`REPEAT_EVERY`], [`REPEAT_ROUNDS`] times: the
/// deadline runs from the FIRST empty observation and no later event extends it, so the
/// empty output is published while the stream is still running. Only a publish after the
/// stream ended — or none at all — fails; when inside the stream it happens is not
/// asserted.
fn repeated_truncation(poll_interval: &str) {
    let what = format!("--poll-interval {poll_interval}");
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    std::fs::write(&src, "version 1\n").unwrap();
    let out = dir.path().join("t.md");

    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&src)
            .args(["--debounce", "0", "--poll-interval", poll_interval])
            .stdout(Stdio::null()),
    );
    let mut log = OutputLog::default();
    log.wait_for(&out, "version 1", TIMEOUT);

    let stream_started = Instant::now();
    for _ in 0..REPEAT_ROUNDS {
        drop(File::create(&src).expect("truncate the entry"));
        log.poll_for(&out, REPEAT_EVERY);
    }
    let stream_ended = Instant::now();

    if log.first_empty.is_none() {
        // Classify the failure: late, or never.
        log.poll_for(&out, EMPTY_HOLD_DEADLINE + TIMEOUT);
    }
    let stderr = stderr_through_the_marker(&src, tap, &mut child);
    match log.first_empty {
        Some(at) => assert!(
            at < stream_ended,
            "{what}: the empty output was published {:?} AFTER the stream of truncations \
             ended — later events extended the deadline; stderr:\n{stderr}",
            at.duration_since(stream_ended)
        ),
        None => panic!(
            "{what}: the empty output was never published, during the {:?} stream or after \
             it; states read: {:?}; stderr:\n{stderr}",
            stream_ended.duration_since(stream_started),
            log.contents()
        ),
    }
}

#[test]
fn repeated_truncation_publishes_the_empty_output_before_the_stream_ends_poll_0() {
    repeated_truncation("0");
}

#[test]
fn repeated_truncation_publishes_the_empty_output_before_the_stream_ends_poll_1000() {
    repeated_truncation("1000");
}

// ── Empty from the start ────────────────────────────────────────────────────

/// A source that is already empty at startup is not a truncation in progress: the
/// startup compile publishes its empty output at once — before readiness — and no
/// notice is printed, since no non-empty output was replaced. Both watch modes.
#[test]
fn a_source_empty_at_startup_compiles_to_empty_immediately() {
    // File mode.
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("t.mds");
    File::create(&src).unwrap();
    let out = dir.path().join("t.md");
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&src)
            .args(["--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert_eq!(
        std::fs::read_to_string(&out).ok().as_deref(),
        Some(""),
        "file mode: the empty output exists as soon as the watcher is ready"
    );
    let stderr = stderr_through_the_marker(&src, tap, &mut child);
    assert_eq!(
        count_occurrences(&stderr, EMPTY_OUTPUT_NOTICE),
        0,
        "file mode: no notice for a source that was empty from the start; stderr:\n{stderr}"
    );

    // Directory mode.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("src");
    std::fs::create_dir(&root).unwrap();
    let src = root.join("e.mds");
    File::create(&src).unwrap();
    let out = dir.path().join("out");
    let (mut child, tap) = spawn_ready(
        mds_bin()
            .arg("watch")
            .arg(&root)
            .arg("--out-dir")
            .arg(&out)
            .args(["--debounce", "0"])
            .stdout(Stdio::null()),
    );
    assert_eq!(
        std::fs::read_to_string(out.join("e.md")).ok().as_deref(),
        Some(""),
        "directory mode: the empty output exists as soon as the watcher is ready"
    );
    let stderr = stderr_through_the_marker(&src, tap, &mut child);
    assert_eq!(
        count_occurrences(&stderr, EMPTY_OUTPUT_NOTICE),
        0,
        "directory mode: no notice for a source that was empty from the start; \
         stderr:\n{stderr}"
    );
}
