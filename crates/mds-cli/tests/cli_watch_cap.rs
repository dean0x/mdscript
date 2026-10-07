//! The debounce cap rebuilds a source that is never left alone (#379, #397).
//!
//! `mds watch --debounce` closes a window one quiet period after the last event, so a
//! writer that never pauses would postpone its own rebuild for as long as it writes;
//! the cap ends every window at most `max(10 x window, 1s)` after it opened. The cap's
//! value, and its exact end under an endless stream, are proved on synthetic instants by
//! the unit tests beside `DebounceWindow` in `src/watch.rs`. This binary holds a real
//! watcher, filesystem and scheduler to the end-to-end claims no runner cadence can
//! falsify:
//!
//! 1. A rebuild is published while the writes go on. The writer reads the output before
//!    each write, so a rebuilt output read before a write shows one. A slow runner
//!    cannot fail the claim — a pause can close a window early, never hold one open — but
//!    it can satisfy it without the cap: a window closes quietly once the writer pauses
//!    for half a window, the watcher seeing each write up to half a window late. So the
//!    claim is proven only when no measured gap of the writes up to the one the rebuilt
//!    output was read before reached half a window. Otherwise a quiet close may have
//!    published it, and the run's flag says the claim went unjudged.
//! 2. The stream's final state is published once the stream ends.
//! 3. The rebuilds number no more than the stream's MEASURED cadence allows
//!    (`common::most_debounce_windows`) — a bound a window that never extends, or a cap
//!    that ends windows early, exceeds. It is judged only when the writer never paused
//!    for half a window: a longer pause may have let a window close quietly, and a
//!    runner that descheduled the writer may have descheduled the watcher too, which the
//!    writer cannot measure. Such a run is inconclusive about the bound: it passes with
//!    the bound logged and not judged. No run fails on the runner's speed, and none
//!    skips.
//!
//! Every run reports which it was in one line (`common::record_run_flag`); the watch
//! soak counts them. The test has a binary of its own because `cargo test` runs one
//! test binary at a time, so no sibling test competes with its writer for the runner.

mod common;
use common::{
    append_line, count_occurrences, mds_bin, most_debounce_windows, poll_tap_until,
    record_run_flag, run_flag_line, spawn_watch_ready, write_atomic, ChildGuard, RunFlag,
    StderrTap, WriteCadence, ORDER_MARKER_LINE, ORDER_MARKER_SOURCE,
};

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ── Constants ───────────────────────────────────────────────────────────────

/// The quiet period: `--debounce 200`.
const WINDOW: Duration = Duration::from_millis(200);

/// The window's absolute cap, `max(10 x WINDOW, 1s)`.
const CAP: Duration = Duration::from_secs(2);

/// How long the writer keeps changing the source: three caps, so a working cap ends
/// windows two or three times before the stream does.
const WRITE_FOR: Duration = Duration::from_secs(6);

/// The writer's sleep after each write: a fortieth of the window, so a runner anywhere
/// near this cadence leaves no pause a window could close in.
const WRITE_PAUSE: Duration = Duration::from_millis(5);

/// The most writes: `WRITE_FOR / WRITE_PAUSE`, so the clock ends the stream first
/// wherever a sleep lasts as long as asked. The bound also keeps the stream's events — a
/// handful per write — under the 10 000 at which the product closes any window, so with
/// the cap removed that limit cannot stand in for it.
const MAX_WRITES: u32 = 1_200;

/// One window more than the measured cadence allows, for what the writer cannot
/// measure: a window closed at the product's message limit, or a watcher descheduled
/// for half a window while the writer was not.
const ALLOWANCE: usize = 1;

/// Upper bound on an inotify-delivered effect: the failure bound `TIMEOUT` of
/// `cli_watch.rs`, not a synchroniser.
const TIMEOUT: Duration = Duration::from_secs(2);

/// How often a wait for the output reads it.
const OUTPUT_POLL: Duration = Duration::from_millis(20);

// ── Harness ─────────────────────────────────────────────────────────────────

/// Spawn a watcher, block until it reports readiness, and wrap it in a `ChildGuard`.
fn spawn_ready(cmd: &mut Command) -> (ChildGuard, StderrTap) {
    let (child, tap, stdout_tap) = spawn_watch_ready(cmd);
    assert!(stdout_tap.is_none(), "this test never pipes stdout");
    (ChildGuard(child), tap)
}

/// Version `version` of the source.
fn source(version: u32) -> String {
    format!("---\nname: v{version}\n---\nHot {{{{name}}}}!\n")
}

/// What version `version` of the source compiles to: `mds` copies the frontmatter
/// through and interpolates the body.
fn output(version: u32) -> String {
    format!("---\nname: v{version}\n---\nHot v{version}!\n")
}

/// Poll `path` until it holds exactly `expected` or `timeout` elapses; whether it did.
fn wait_for_output(path: &Path, expected: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    // Bounded by `timeout`: the file is read once more after the deadline passes, so a
    // state reached on the last poll still counts.
    loop {
        if std::fs::read_to_string(path).is_ok_and(|held| held == expected) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(OUTPUT_POLL);
    }
}

/// What the writer measured and saw.
struct Stream {
    cadence: WriteCadence,
    /// The first rebuilt output read before a write: that write's number, and the
    /// output's last line.
    rebuilt_before: Option<(u32, String)>,
}

/// Write versions 1, 2, … of `src` until the writes have spanned [`WRITE_FOR`] (at most
/// [`MAX_WRITES`] of them), sleeping [`WRITE_PAUSE`] after each. Until it first finds
/// one, read `out` before each write for a rebuilt output — anything but `startup`.
fn write_stream(src: &Path, out: &Path, startup: &str) -> Stream {
    let mut cadence = WriteCadence::with_capacity(MAX_WRITES as usize);
    let mut rebuilt_before = None;
    for version in 1..=MAX_WRITES {
        if cadence.span() >= WRITE_FOR {
            break;
        }
        if rebuilt_before.is_none() {
            rebuilt_before = std::fs::read_to_string(out)
                .ok()
                .filter(|held| held != startup)
                .map(|held| {
                    let line = held.lines().next_back().unwrap_or_default();
                    (version, line.to_owned())
                });
        }
        cadence.time(|| write_atomic(src, source(version)));
        std::thread::sleep(WRITE_PAUSE);
    }
    Stream {
        cadence,
        rebuilt_before,
    }
}

/// Claim 1's verdict, for the run's flag: whether it is proven, and how it was judged.
///
/// It is proven when the first rebuilt output was read before write `k` and no gap of
/// writes 1 to `k` — the last of them spans the read — reached half a window: no window
/// can have closed quietly by then, so the cap published it. Otherwise a quiet close may
/// have, and the claim went unjudged.
fn claim_1(stream: &Stream) -> (bool, String) {
    let writes = stream.cadence.writes();
    let Some((version, line)) = &stream.rebuilt_before else {
        return (
            false,
            format!("no rebuilt output read before any of the {writes} writes"),
        );
    };
    let before = stream
        .cadence
        .first(usize::try_from(*version).expect("a write number fits usize"));
    let quiet = before.pauses(WINDOW / 2);
    let read = format!("{line:?} read before write {version} of {writes}");
    if quiet == 0 {
        (
            true,
            format!(
                "claim 1 proven: {read}, no gap of {:?} or more by then ({before})",
                WINDOW / 2
            ),
        )
    } else {
        (
            false,
            format!(
                "claim 1 not judged: {read}, after {quiet} gap(s) of {:?} or more in which a \
                 window could close quietly ({before})",
                WINDOW / 2
            ),
        )
    }
}

// ── The cap ─────────────────────────────────────────────────────────────────

/// A source written to without pause is still rebuilt while the writing goes on, and
/// no more often than the stream's measured cadence allows (#379, #397); see the
/// module docs for the three claims and when the third is judged.
///
/// `--poll-interval 0` turns the idle tick off, so while the stream lasts only a
/// closing window can rebuild. The loop never reaches the tick while a window is open,
/// so where the tick is on, the cap is also what keeps a stream from starving it.
#[test]
fn watch_debounce_cap_rebuilds_while_writes_never_stop() {
    const TEST: &str = "watch_debounce_cap_rebuilds_while_writes_never_stop";
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("hot.mds");
    let out = dir.path().join("hot.md");
    std::fs::write(&src, source(0)).unwrap();

    let (mut child, stderr_tap) = spawn_ready(
        mds_bin()
            .args([
                "watch",
                src.to_str().unwrap(),
                "--debounce",
                "200",
                "--poll-interval",
                "0",
            ])
            .stdout(Stdio::null()),
    );
    let startup = output(0);
    assert!(
        wait_for_output(&out, &startup, TIMEOUT),
        "the startup compile must publish version 0"
    );

    let stream = write_stream(&src, &out, &startup);
    let writes = stream.cadence.writes();
    let last = u32::try_from(writes).expect("at most MAX_WRITES writes");
    // The window holding the last write closes at most a cap after it opened.
    let published = wait_for_output(&out, &output(last), CAP + TIMEOUT);

    // The marker's diagnostic reaches stderr after every line of every earlier rebuild,
    // so the count below is exact. Its compile fails, so the output keeps what it held.
    // The wait is data, not a failure, so the claims below fail in their own order
    // whatever went wrong — a watcher that died mid-stream fails the first of them.
    write_atomic(&src, ORDER_MARKER_SOURCE);
    let anchored = poll_tap_until(&stderr_tap, TIMEOUT, |text| {
        text.contains(ORDER_MARKER_LINE)
    })
    .is_ok();
    let stderr = stderr_tap.finish_text(&mut child);
    let rebuilds = count_occurrences(&stderr, "Recompiled ");

    let pauses = stream.cadence.pauses(WINDOW / 2);
    let most = most_debounce_windows(&stream.cadence, WINDOW, CAP) + ALLOWANCE;
    let flag = if pauses == 0 {
        RunFlag::Conclusive
    } else {
        RunFlag::Inconclusive
    };
    let judged = match flag {
        RunFlag::Conclusive => format!("at most {most} allowed"),
        RunFlag::Inconclusive => format!(
            "the bound of {most} not judged: {pauses} gap(s) of {:?} or more",
            WINDOW / 2
        ),
    };
    // A run with no pause at all has none before its first rebuilt output either, so a
    // conclusive run has always proven claim 1.
    let (proven, seen) = claim_1(&stream);
    assert!(
        flag == RunFlag::Inconclusive || proven || stream.rebuilt_before.is_none(),
        "a run without a pause proves claim 1 whenever it saw a rebuild: {seen}"
    );
    record_run_flag(
        TEST,
        flag,
        &format!(
            "{rebuilds} rebuild(s), {judged}; {seen}; {}",
            stream.cadence
        ),
    );

    assert!(
        stream.rebuilt_before.is_some(),
        "a rebuild must be published while the writes go on — that is what the cap is \
         for — but every read before a write found the startup output ({seen}; {}); \
         stderr was:\n{stderr}",
        stream.cadence
    );
    assert!(
        published,
        "the stream's final state, version {last}, must be published once it ends; the \
         output holds {:?}; stderr was:\n{stderr}",
        std::fs::read_to_string(&out).ok()
    );
    assert!(
        anchored,
        "the order marker's diagnostic must reach stderr within {TIMEOUT:?}, or the count \
         below is not exact; stderr was:\n{stderr}"
    );
    // Positive control for the count: the rebuild read during the stream published an
    // earlier version than the last, so publishing the last took another.
    assert!(
        rebuilds >= 2,
        "a rebuild during the stream and the one that published its final state make \
         at least two `Recompiled` lines; got {rebuilds}; stderr was:\n{stderr}"
    );
    if flag == RunFlag::Conclusive {
        assert!(
            rebuilds <= most,
            "{} with no gap of {:?} or more allow at most {most} rebuilds under a \
             {WINDOW:?} quiet period capped at {CAP:?} — a window that never extended \
             would close about once per window; got {rebuilds}; stderr was:\n{stderr}",
            stream.cadence,
            WINDOW / 2
        );
    }
}

// ── The measuring helpers ───────────────────────────────────────────────────

/// `t0` plus `ms` milliseconds.
fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

/// `count` writes 1ms long, one every `every` ms from `from` ms after `t0`.
fn writes_every(t0: Instant, from: u64, every: u64, count: u64) -> Vec<(Instant, Instant)> {
    (0..count)
        .map(|k| {
            let began = from + k * every;
            (at(t0, began), at(t0, began + 1))
        })
        .collect()
}

/// A gap runs from the start of one write to the end of the next — the longest a
/// watcher can have waited between their events — and a pause is a gap at least the
/// threshold long.
#[test]
fn a_gap_runs_from_the_start_of_one_write_to_the_end_of_the_next() {
    let t0 = Instant::now();
    let cadence = WriteCadence::of(vec![
        (at(t0, 0), at(t0, 1)),
        (at(t0, 6), at(t0, 7)),
        (at(t0, 120), at(t0, 121)),
    ]);
    assert_eq!(cadence.writes(), 3);
    assert_eq!(cadence.span(), Duration::from_millis(121));
    assert_eq!(
        cadence.gaps(),
        [Duration::from_millis(7), Duration::from_millis(115)]
    );
    assert_eq!(
        cadence.pauses(Duration::from_millis(115)),
        1,
        "a gap of exactly the threshold is a pause"
    );
    assert_eq!(cadence.pauses(Duration::from_millis(116)), 0);
    assert_eq!(cadence.pauses(Duration::from_millis(7)), 2);
    assert_eq!(WriteCadence::of(Vec::new()).span(), Duration::ZERO);
}

/// The first writes of a stream are a cadence of their own, so a claim can be judged on
/// what the writer had done by a given write.
#[test]
fn the_first_writes_are_a_cadence_of_their_own() {
    let t0 = Instant::now();
    let cadence = WriteCadence::of(vec![
        (at(t0, 0), at(t0, 1)),
        (at(t0, 6), at(t0, 7)),
        (at(t0, 120), at(t0, 121)),
    ]);
    let first_two = cadence.first(2);
    assert_eq!(first_two.writes(), 2);
    assert_eq!(first_two.gaps(), [Duration::from_millis(7)]);
    assert_eq!(
        first_two.pauses(WINDOW / 2),
        0,
        "the pause comes after the second write"
    );
    assert_eq!(cadence.first(3).pauses(WINDOW / 2), 1);
    assert_eq!(
        cadence.first(10).writes(),
        3,
        "all of them when fewer were recorded"
    );
    assert_eq!(cadence.first(0).span(), Duration::ZERO);
}

/// Claim 1 is proven only when no pause of half a window came by the write the rebuilt
/// output was read before; the gap that ends at that write spans the read.
#[test]
fn claim_1_is_proven_only_when_no_pause_came_before_the_read() {
    let t0 = Instant::now();
    // Ten writes 5ms apart, a pause, then ten more: write 11 is the first after it.
    let stream = |read_before: Option<u32>| {
        let mut writes = writes_every(t0, 0, 5, 10);
        writes.extend(writes_every(t0, 300, 5, 10));
        Stream {
            cadence: WriteCadence::of(writes),
            rebuilt_before: read_before.map(|version| (version, "Hot v1!".to_owned())),
        }
    };
    for (read_before, proven) in [(Some(5), true), (Some(10), true), (Some(11), false)] {
        let (verdict, text) = claim_1(&stream(read_before));
        assert_eq!(verdict, proven, "read before write {read_before:?}: {text}");
    }
    let (verdict, text) = claim_1(&stream(None));
    assert!(
        !verdict && text.starts_with("no rebuilt output"),
        "no rebuild read: {text}"
    );
}

/// The bound counts the windows the cap can close over the measured span, one window
/// per pause of half a window, and the window the stream ends in.
#[test]
fn the_rebuild_bound_follows_the_measured_cadence() {
    let t0 = Instant::now();
    // 6s of writes every 5ms: the cap closes at most three windows within the span and
    // the window and a half after it, and the last write's window closes once more. A
    // window that never extended would close about thirty times.
    let dense = WriteCadence::of(writes_every(t0, 0, 5, 1_200));
    assert_eq!(dense.span(), Duration::from_millis(5_996));
    assert_eq!(dense.pauses(WINDOW / 2), 0);
    assert_eq!(most_debounce_windows(&dense, WINDOW, CAP), 4);

    // The same stream with two pauses of 151ms: one more window each.
    let mut paused = writes_every(t0, 0, 5, 400);
    paused.extend(writes_every(t0, 2_145, 5, 400));
    paused.extend(writes_every(t0, 4_290, 5, 400));
    let paused = WriteCadence::of(paused);
    assert_eq!(paused.pauses(WINDOW / 2), 2);
    assert_eq!(paused.span(), Duration::from_millis(6_286));
    assert_eq!(most_debounce_windows(&paused, WINDOW, CAP), 6);

    // `cli_watch.rs`'s burst — twelve writes 30ms apart under a 250ms window capped at
    // 2.5s — fits in one window.
    let burst = WriteCadence::of(writes_every(t0, 0, 30, 12));
    let burst_window = Duration::from_millis(250);
    assert_eq!(
        most_debounce_windows(&burst, burst_window, Duration::from_millis(2_500)),
        1
    );
}

/// A flag is one line the soak can count — it holds `: conclusive: ` or
/// `: inconclusive: `, never both — and a flag log that cannot be written is an error,
/// never a silent pass.
#[test]
fn a_run_flag_is_one_line_the_soak_can_count() {
    let yes = run_flag_line("t", RunFlag::Conclusive, "4 rebuild(s)");
    let no = run_flag_line("t", RunFlag::Inconclusive, "2 gap(s)");
    assert_eq!(yes, "t: conclusive: 4 rebuild(s)");
    assert_eq!(no, "t: inconclusive: 2 gap(s)");
    assert!(
        !no.contains(": conclusive: "),
        "the soak's conclusive pattern must not match an inconclusive line"
    );

    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("flags.log");
    append_line(&log, &yes).unwrap();
    append_line(&log, &no).unwrap();
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        format!("{yes}\n{no}\n")
    );
    // Control: a directory where the log should be cannot be appended to.
    assert!(append_line(dir.path(), &yes).is_err());
}
