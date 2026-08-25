//! Watch subcommand — file watcher with auto-recompile on save (issue #57).
//!
//! # Design overview
//!
//! Two modes share a single watch loop:
//!
//! - **Single-file mode**: watches the entry file and all its transitive imports.
//!   On each rebuild the dependency set is recomputed from fresh compilation output
//!   (ADR-016: never trust a stale dep set).
//!
//! - **Directory mode**: recursive watch on the root dir; tracks a reverse-dependency
//!   graph so editing a shared partial recompiles all transitive importers.
//!   `_`-prefixed files are partials: tracked in the graph but never emitted to their
//!   own `.md` output (DD2). Cross-root dependencies are watched NonRecursively (DD3).
//!   Output mirrors the source subtree under `--out-dir` / `mds.json output_dir` (Fix 2).
//!
//! # Change detection
//!
//! Both modes run two detectors, and neither alone is sufficient:
//!
//! 1. **OS watches** (inotify / FSEvents) — the primary path. Armed before the first
//!    read of anything they cover, so an edit during startup is queued, not dropped.
//! 2. **The idle-tick content backstop** — a `(mtime, size)` diff over every tracked
//!    source *and dependency*, run once per `--poll-interval` by `liveness_probe_*`.
//!    It exists for changes no OS event can announce: a cross-root dependency's
//!    directory is unknowable until the compile that reads it returns, and a watch
//!    descriptor destroyed by `rmdir` never announces its own replacement (#321).
//!
//! The tick is scheduled against an absolute deadline (`TickClock`), so a stream of
//! filesystem events cannot postpone the backstop indefinitely (#319).
//!
//! # Key invariants
//!
//! - All content output → stdout ONLY when output resolves to stdout.
//! - All status / warnings / errors → stderr (pipe-safe).
//! - `--quiet` suppresses status + warnings but NOT compile errors.
//! - Exit 0 on clean Ctrl+C; non-zero only on startup failure.
//! - Compile errors during watching never terminate the watcher.
//! - All loops have fixed upper bounds (ADR-021 / reliability.md).
//! - All `.mds` reads go through `compile_to_content` (PF-004).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use miette::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use mds::MdsError;

use crate::build::{
    auto_detect_mds_file, build_runtime_vars, compile_and_write, compile_to_content, load_config,
    resolve_output_path_for_kind, write_output, OutputKind, RuntimeVarArgs,
};
use crate::output::{
    canonicalize_out_dir, collect_mds_files, eprint_error, eprint_warning, is_partial,
    is_within_default_excluded_dir, output_base_no_ext, output_path_for, probe_and_remove_stale,
    resolve_output_base, safe_inline, safe_path, OutputBase,
};

// ── Public args struct ────────────────────────────────────────────────────────

pub(crate) struct WatchArgs {
    pub(crate) input: Option<PathBuf>,
    pub(crate) output: Option<String>,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) set_string_vars: Vec<(String, String)>,
    pub(crate) clear: bool,
    pub(crate) debounce: u64,
    pub(crate) quiet: bool,
    pub(crate) poll_interval: u64,
}

// ── Internal message types ────────────────────────────────────────────────────

enum Msg {
    Fs(notify::Result<Event>),
    Interrupt,
}

// ── OutputBase re-exported from output.rs (moved for shared use) ─────────────
//
// `OutputBase`, `resolve_output_base`, `output_path_for`, `collect_mds_files`,
// and `is_partial` are now defined in `output.rs` and imported above.
// The doc comments there describe the contracts; no duplication needed here.

// ── Pure helpers (unit-tested below) ─────────────────────────────────────────

/// Compute the set of parent directories that need to be watched (non-recursively)
/// to cover `entry`, all `deps`, and an optional `vars_file`.
///
/// Watching parent directories rather than file inodes is necessary because editors
/// perform atomic save via rename: a file-inode watch is silently orphaned after the
/// swap, but a directory watch survives.
pub(crate) fn dirs_to_watch(
    entry: &Path,
    deps: &[String],
    vars_file: Option<&Path>,
) -> BTreeSet<PathBuf> {
    let mut dirs = BTreeSet::new();

    let push_parent = |path: &Path, set: &mut BTreeSet<PathBuf>| {
        // Route through mds::effective_parent so that bare filenames — where
        // Path::parent() returns Some("") rather than None — are handled by the
        // single canonical implementation rather than an inline re-implementation.
        // Avoids PF-006: one owner, one place to maintain or regress.
        set.insert(mds::effective_parent(path).to_path_buf());
    };

    push_parent(entry, &mut dirs);

    for dep in deps {
        push_parent(Path::new(dep), &mut dirs);
    }

    if let Some(vf) = vars_file {
        push_parent(vf, &mut dirs);
    }

    dirs
}

/// Build the set of paths that are "of interest" for a single-file watch:
/// the entry itself, all dependency paths, and the vars file if given.
pub(crate) fn files_of_interest(
    entry: &Path,
    deps: &[String],
    vars_file: Option<&Path>,
) -> HashSet<PathBuf> {
    let mut set = HashSet::new();
    set.insert(entry.to_path_buf());
    for dep in deps {
        set.insert(PathBuf::from(dep));
    }
    if let Some(vf) = vars_file {
        set.insert(vf.to_path_buf());
    }
    set
}

/// Return `true` for filesystem event kinds that represent **content changes**.
///
/// `EventKind::Access(_)` covers inotify `IN_ACCESS`, `IN_OPEN`, and
/// `IN_CLOSE_NOWRITE` — events emitted when a file is merely *read*, not
/// written.  On Linux the compile step reads `.mds` source files, which causes
/// inotify to emit Access events for those same files.  Without this filter the
/// watcher ingests those events, re-compiles, reads again, emits more Access
/// events, and enters a busy-loop (thousands of recompiles per second).
///
/// macOS FSEvents does not report reads, so this bug was invisible locally and
/// only manifested in CI on `ubuntu-latest`.
///
/// Kept conservative: `Modify`, `Create`, `Remove`, `Any`, `Other` all return
/// `true`.  `Access(Close(AccessMode::Write))` is technically a write-close but
/// those paths also produce a `Modify` event on Linux, so excluding all Access
/// variants is safe and simpler.
pub(crate) fn is_content_event(kind: &notify::EventKind) -> bool {
    !matches!(kind, notify::EventKind::Access(_))
}

/// Return `true` when an fs event is relevant to the current watch set.
///
/// Matches by canonical path. Falls back to (file-name + parent) comparison
/// for just-renamed files whose canonical path may differ transiently.
/// Also tries canonicalizing the event path to handle /tmp → /private/tmp
/// symlink differences on macOS.
pub(crate) fn event_is_relevant(event: &Event, watched: &HashSet<PathBuf>) -> bool {
    for path in &event.paths {
        if watched.contains(path) {
            return true;
        }
        // Try resolving symlinks in the event path (macOS /tmp → /private/tmp).
        if let Ok(canonical) = path.canonicalize() {
            if watched.contains(&canonical) {
                return true;
            }
        }
        // Fallback: check by (parent, file_name) in case the path is a relative
        // or non-canonical form of a watched file.
        let name = path.file_name();
        let parent = path.parent();
        if let (Some(n), Some(p)) = (name, parent) {
            if watched
                .iter()
                .any(|w| w.file_name() == Some(n) && w.parent() == Some(p))
            {
                return true;
            }
            // Also try canonical parent.
            if let Ok(cp) = p.canonicalize() {
                if watched
                    .iter()
                    .any(|w| w.file_name() == Some(n) && w.parent() == Some(cp.as_path()))
                {
                    return true;
                }
            }
        }
    }
    false
}

// collect_mds_files and is_partial are now in output.rs (imported above).

/// Canonicalize a graph key: exists → `p.canonicalize()`; missing → canonicalize parent + rejoin.
///
/// Used to normalize event paths before graph lookups so macOS `/tmp`→`/private/tmp`
/// and other symlink-resolved differences are handled consistently.
pub(crate) fn graph_key(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    // File doesn't exist (just deleted): canonicalize effective parent + rejoin filename.
    // mds::effective_parent maps Some("") (bare filename, e.g. "hello.mds") to
    // Path::new(".") so that "".canonicalize() never runs — avoids PF-006 in the
    // graph-key lookup-miss path: without this guard a bare-named file that is
    // deleted cannot be matched against the absolute-path keys stored in forward_deps.
    let parent = mds::effective_parent(p);
    if let Ok(cp) = parent.canonicalize() {
        if let Some(name) = p.file_name() {
            return cp.join(name);
        }
    }
    p.to_path_buf()
}

/// Compute the transitive set of sources affected by `seeds`.
///
/// Builds an inverted importer map from the start-of-batch `forward_deps` snapshot
/// then walks DFS with a visited set (cycle-safe, terminates).
/// Returns `seeds ∪ all transitive importers`.
///
/// Pure function — only reads `forward_deps`, does not mutate it.
pub(crate) fn affected_sources(
    forward_deps: &HashMap<PathBuf, Vec<PathBuf>>,
    seeds: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    // Build inverted map: dep → Vec<importer>
    let mut importers: HashMap<&PathBuf, Vec<&PathBuf>> = HashMap::new();
    for (src, deps) in forward_deps {
        for dep in deps {
            importers.entry(dep).or_default().push(src);
        }
    }

    let mut visited: HashSet<&PathBuf> = HashSet::new();
    let mut result: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<&PathBuf> = Vec::new();

    // Seed the stack with the initial changed files.
    for seed in seeds {
        if visited.insert(seed) {
            result.push(seed.clone());
            stack.push(seed);
        }
    }

    // DFS: find all importers transitively.
    while let Some(node) = stack.pop() {
        if let Some(imps) = importers.get(node) {
            for imp in imps {
                if visited.insert(imp) {
                    result.push((*imp).clone());
                    stack.push(imp);
                }
            }
        }
    }

    result
}

/// A single path's content fingerprint: `(mtime, size)`.
///
/// Each field is `None` when the file does not exist or its metadata is unreadable —
/// absence is a valid state to track, and is what lets a deletion register as a change.
pub(crate) type FileStamp = (Option<std::time::SystemTime>, Option<u64>);

/// A `(mtime, size)` baseline keyed by path, as produced by [`snapshot_state`].
pub(crate) type StampMap = HashMap<PathBuf, FileStamp>;

/// Snapshot `(mtime, size)` for a set of paths (liveness probe state).
///
/// Returns `None` for the mtime or size field when the file doesn't exist or
/// the metadata call fails — absence is a valid state to track.
pub(crate) fn snapshot_state(paths: &HashSet<PathBuf>) -> StampMap {
    let mut map = HashMap::new();
    for p in paths {
        match std::fs::metadata(p) {
            Ok(m) => {
                let mtime = m.modified().ok();
                let size = Some(m.len());
                map.insert(p.clone(), (mtime, size));
            }
            Err(_) => {
                map.insert(p.clone(), (None, None));
            }
        }
    }
    map
}

/// Record `path`'s current `(mtime, size)` in `snapshot`, keeping any entry already
/// there.
///
/// The keep-existing rule is the point: baselines are merged oldest-wins, because only
/// a baseline taken before a read can prove the read saw the current content.
pub(crate) fn baseline_path(path: &Path, snapshot: &mut StampMap) {
    snapshot
        .entry(path.to_path_buf())
        .or_insert_with(|| match std::fs::metadata(path) {
            Ok(m) => (m.modified().ok(), Some(m.len())),
            Err(_) => (None, None),
        });
}

/// Return `true` if the current `(mtime, size)` of `path` differs from its entry in
/// `prev`.
///
/// A path with no entry in `prev` counts as differing: the baseline has never seen it,
/// so the watcher cannot claim its content is accounted for.
pub(crate) fn path_state_differs(path: &Path, prev: &StampMap) -> bool {
    let current = match std::fs::metadata(path) {
        Ok(m) => (m.modified().ok(), Some(m.len())),
        Err(_) => (None, None),
    };
    !matches!(prev.get(path), Some(old) if *old == current)
}

/// Return `true` if the current `(mtime, size)` of any path in `paths` differs
/// from its entry in `prev`.
pub(crate) fn state_differs(paths: &HashSet<PathBuf>, prev: &StampMap) -> bool {
    paths.iter().any(|p| path_state_differs(p, prev))
}

/// Decide whether a missing/recovered external dep dir should trigger a full
/// reconcile, and compute the new "missing" set for the next tick.
///
/// Edge-triggered (ADR-021 / AC-P1): a missing external dir forces a reconcile
/// only when it *reappears* (was in `prev_missing`, now exists). A dir that stays
/// missing across ticks does NOT trigger a walk — otherwise a permanently-deleted
/// cross-root dep dir would cause an O(tree) rescan on every idle tick.
///
/// `statuses` is one `(dir, exists, rearm_ok)` per current external dep dir, where
/// `rearm_ok` is the result of attempting to re-arm an existing dir (ignored when
/// `exists` is false).
///
/// Returns `(recovery_needed, now_missing)`.
pub(crate) fn external_recovery_decision(
    prev_missing: &BTreeSet<PathBuf>,
    statuses: &[(PathBuf, bool, bool)],
) -> (bool, BTreeSet<PathBuf>) {
    let mut now_missing = BTreeSet::new();
    let mut recovery = false;
    for (dir, exists, rearm_ok) in statuses {
        if *exists {
            if !*rearm_ok {
                // Re-arming an existing dir failed: genuine watch loss.
                recovery = true;
            } else if prev_missing.contains(dir) {
                // Was missing last tick, now exists and re-armed: recovery edge.
                recovery = true;
            }
        } else {
            now_missing.insert(dir.clone());
        }
    }
    (recovery, now_missing)
}

/// Canonicalize an optional vars path so it matches the canonical paths in notify
/// events (e.g. resolves `/tmp` → `/private/tmp` on macOS).
///
/// Rejects a symlinked vars file at startup (build parity — PF-004).
/// Falls back to the raw path when the file does not yet exist (the user may create
/// it later; the per-rebuild `load_vars_file` will catch it then).
pub(crate) fn canonicalize_vars_path(vars: Option<PathBuf>) -> Result<Option<PathBuf>, MdsError> {
    match vars {
        Some(p) if p.exists() => {
            mds::NativeFs::check_symlink(&p)
                .map(Some)
                .map_err(|_| MdsError::Io {
                    message: format!("--vars file must not be a symlink: {}", p.display()),
                })
        }
        other => Ok(other),
    }
}

/// Write the ANSI clear-screen sequence to stderr if stderr is a TTY.
///
/// Uses `\x1b[2J\x1b[3J\x1b[H` (erase screen + scrollback + home).
pub(crate) fn clear_terminal() {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        eprint!("\x1b[2J\x1b[3J\x1b[H");
    }
}

/// Update the watcher to reflect a new set of directories.
///
/// Unwatch directories no longer needed, watch newly required ones.
/// Returns the updated set of currently-watched directories.
pub(crate) fn resync_watches(
    watcher: &mut RecommendedWatcher,
    current_dirs: &BTreeSet<PathBuf>,
    new_dirs: &BTreeSet<PathBuf>,
) -> BTreeSet<PathBuf> {
    let mut result = current_dirs.clone();
    // Unwatch removed directories.
    for dir in current_dirs.difference(new_dirs) {
        // Errors here are non-fatal (dir may have been deleted).
        let _ = watcher.unwatch(dir);
        result.remove(dir);
    }
    // Watch new directories.
    for dir in new_dirs.difference(current_dirs) {
        if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            eprint_warning(&format!(
                "warning: failed to watch {}: {}",
                safe_path(dir),
                safe_inline(&e)
            ));
        } else {
            result.insert(dir.clone());
        }
    }
    result
}

// ── Small shared helpers ──────────────────────────────────────────────────────

/// Emit "Stopped watching." to stderr (unless quiet).
///
/// Called at every Ctrl+C exit point in both watch loops.
fn stop_watching(quiet: bool) {
    if !quiet {
        eprintln!("Stopped watching.");
    }
}

/// Test-only delay injected right after the startup output is published.
///
/// This is the **positive control** for the arm-before-publish ordering: it widens
/// the interval between "output written" and "watch fully live" to a size no edit
/// can miss. Under a defective ordering that interval is a window in which a file
/// is covered by neither detector, and the injected delay drives the lost-edit rate
/// to ~100%. Under the correct ordering the OS watch is already armed and the mtime
/// baseline already captured before the output is written, so the same delay changes
/// nothing — which is exactly what makes it evidence that the *mechanism* is gone
/// rather than merely rarer.
///
/// Compiled out entirely unless the `startup-race-probe` feature is enabled; that
/// feature must never ship enabled.
#[cfg(feature = "startup-race-probe")]
fn startup_race_probe() {
    std::thread::sleep(Duration::from_millis(200));
}

#[cfg(not(feature = "startup-race-probe"))]
fn startup_race_probe() {}

/// Environment variable that enables the test-only readiness handshake.
///
/// Its value is the **absolute path of a file** to create once the watch is armed.
/// Test-only: `mds` never sets it itself and it adds no CLI surface.
const READY_MARKER_ENV: &str = "MDS_TEST_READY";

/// Contents written to the readiness file named by [`READY_MARKER_ENV`].
const READY_MARKER: &str = "MDS_WATCH_READY";

/// Signal readiness by creating the file named by `MDS_TEST_READY`.
///
/// Called by both watch modes at the single instant where **every** file of
/// interest is covered by at least one detector: its parent directory is armed
/// with the OS watcher *and* its `(mtime, size)` baseline has been captured.
/// An edit made after this file appears is guaranteed to be observed.
///
/// This exists because no pre-existing output line is a sound readiness signal:
/// `"Watching {path}"` is printed *before* the startup compile, and
/// `"Recompiled …"` only ever appears after a successful *rebuild*. Tests that
/// keyed off either raced the tail of startup.
///
/// # Why a file and not stderr
///
/// The handshake must not perturb the streams the suite asserts on. A marker line
/// on stderr would have to bypass `--quiet` (the suite runs with `-q`), which puts
/// bytes into the exact stream two tests inspect for *emptiness* — that stderr
/// carries a compile error through `-q`, and that the initial-compile-error path
/// emits something at all. Both assertions silently become unfalsifiable the moment
/// anything else is written there unconditionally. A side channel has no such
/// coupling: stdout and stderr stay byte-for-byte what a real user would see.
///
/// Write-then-rename so a test polling for the path can never observe a partially
/// written marker. Failures are ignored: this is a test affordance, and a watcher
/// that cannot create the file must still watch.
fn emit_ready_marker() {
    let Some(raw) = std::env::var_os(READY_MARKER_ENV) else {
        return;
    };
    let path = PathBuf::from(raw);
    // Absolute paths only. A relative value would resolve against the watcher's cwd
    // — which under `cargo test` is the crate root — and litter the source tree.
    if !path.is_absolute() {
        return;
    }
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if std::fs::write(&tmp, READY_MARKER).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Idle-tick scheduler holding an **absolute** deadline (#319).
///
/// The liveness probe is the watcher's only backstop for a change that no filesystem
/// event ever announced, so how the tick is scheduled decides whether that backstop
/// is reachable at all.
///
/// Handing `recv_timeout` a fresh `--poll-interval` budget on every message made it
/// starvable: any event stream arriving faster than the interval restarted the
/// countdown before it could expire, so the tick never fired. That is not a rare
/// condition — a compile reads its own sources and inotify reports every read, an
/// editor writes scratch files beside the one being edited, a dev server or sync
/// client touches the tree continuously, and the watch suite's own 50ms output poll
/// is 20× faster than the 1000ms default interval. Under any of them a change the
/// probe was meant to recover was lost permanently rather than delayed.
///
/// Keeping the deadline as an `Instant` pins the tick to wall-clock time instead:
/// an incoming message shortens the remaining wait rather than restarting it, so the
/// tick comes due on schedule no matter how loaded the channel is.
///
/// Re-arming to `now + interval` at the moment a tick is *observed* bounds it from
/// the other side. The probe — and any recompile it triggers — runs between two
/// `recv_next` calls, so a probe that overruns its own interval simply arms the next
/// deadline from when it finished. It can never accumulate overdue ticks and fire
/// them back-to-back: at most one tick per interval, under every load.
struct TickClock {
    /// `None` when `--poll-interval 0` disabled the probe; `recv_next` then blocks.
    interval: Option<Duration>,
    /// Instant at which the next idle tick comes due. Unused while `interval` is `None`.
    next: Instant,
}

impl TickClock {
    fn new(interval: Option<Duration>) -> Self {
        Self {
            interval,
            next: Instant::now() + interval.unwrap_or(Duration::ZERO),
        }
    }

    /// Receive the next message from the watch channel.
    ///
    /// Returns:
    /// - `Ok(Some(msg))` — a message arrived before the tick came due.
    /// - `Ok(None)`      — idle tick (only when a poll interval is configured).
    /// - `Err(_)`        — channel disconnected; caller should `break`.
    fn recv_next(
        &mut self,
        rx: &mpsc::Receiver<Msg>,
    ) -> std::result::Result<Option<Msg>, mpsc::RecvTimeoutError> {
        let Some(interval) = self.interval else {
            return rx
                .recv()
                .map(Some)
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected);
        };
        let now = Instant::now();
        // Already due: fire before taking another message, so a saturated channel
        // cannot postpone the probe indefinitely.
        if now >= self.next {
            self.next = now + interval;
            return Ok(None);
        }
        match rx.recv_timeout(self.next - now) {
            Ok(msg) => Ok(Some(msg)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.next = Instant::now() + interval;
                Ok(None)
            }
            Err(e @ mpsc::RecvTimeoutError::Disconnected) => Err(e),
        }
    }
}

// ── Debounce loop ─────────────────────────────────────────────────────────────

/// Drain the channel for `debounce_ms` milliseconds, collecting all changed paths.
///
/// Returns `(paths, interrupted)`.
/// - `paths`: all file paths seen in notify events during the window.
/// - `interrupted`: true if an Interrupt message was received.
///
/// The loop is bounded: it ends when `Instant::now() >= deadline` or when
/// `interrupted` is true.
fn drain_debounce(rx: &mpsc::Receiver<Msg>, debounce_ms: u64) -> (BTreeSet<PathBuf>, bool) {
    let mut paths = BTreeSet::new();
    if debounce_ms == 0 {
        return (paths, false);
    }
    let deadline = Instant::now() + Duration::from_millis(debounce_ms);
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match rx.recv_timeout(remaining) {
            Ok(Msg::Fs(Ok(event))) => {
                // Drop Access events (inotify IN_ACCESS/IN_OPEN/IN_CLOSE_NOWRITE)
                // — reads must not trigger recompiles; see is_content_event.
                if is_content_event(&event.kind) {
                    for p in event.paths {
                        paths.insert(p);
                    }
                }
            }
            Ok(Msg::Fs(Err(e))) => {
                eprint_warning(&format!(
                    "warning: watch error during debounce: {}",
                    safe_inline(&e)
                ));
            }
            Ok(Msg::Interrupt) => return (paths, true),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    (paths, false)
}

// ── Poll-interval clamp (ADR-021) ─────────────────────────────────────────────

/// Convert a raw `--poll-interval` value (milliseconds) into a tick duration.
///
/// - `0` → `None` (blocking `recv`, no liveness probe)
/// - nonzero → `Some(max(value, 50ms))` — floor prevents a busy-spin liveness probe
///
/// Extracted so the clamp contract can be verified by unit tests independently of
/// the full watch loop.
fn clamp_poll_interval(poll_interval: u64) -> Option<Duration> {
    if poll_interval == 0 {
        None
    } else {
        Some(Duration::from_millis(poll_interval.max(50)))
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub(crate) fn run_watch(args: WatchArgs) -> Result<()> {
    let WatchArgs {
        input,
        output,
        out_dir,
        vars,
        set_vars,
        set_string_vars,
        clear,
        debounce,
        quiet,
        poll_interval,
    } = args;

    // ── Input mode dispatch ───────────────────────────────────────────────────

    // Reject stdin.
    if input.as_deref() == Some(Path::new("-")) {
        return Err(miette::miette!(
            "watch does not support stdin ('-'); use 'mds build -' instead"
        ));
    }

    // Resolve the input path (may trigger auto-detect).
    let resolved_input = match input {
        None => auto_detect_mds_file("watch")?,
        Some(p) => p,
    };

    let is_dir = resolved_input.is_dir();

    // Directory mode constraint checks.
    if is_dir && output.is_some() {
        return Err(miette::miette!(
            "watch directory mode does not support -o/--output; \
             use --out-dir to specify an output directory"
        ));
    }

    // Reject a symlinked entry/target (build parity — PF-004); plain canonicalize
    // would silently follow it. check_symlink returns the canonical path for
    // non-symlinks, preserving FSEvents path-matching.
    let canonical_input =
        mds::NativeFs::check_symlink(&resolved_input).map_err(miette::Error::from)?;

    // Clamp poll_interval: 0 = disable; nonzero ≥ 50ms floor (ADR-021).
    let tick_opt: Option<Duration> = clamp_poll_interval(poll_interval);

    if is_dir {
        run_watch_dir(
            canonical_input,
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            clear,
            debounce,
            quiet,
            tick_opt,
        )
    } else {
        run_watch_file(
            canonical_input,
            output,
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            clear,
            debounce,
            quiet,
            tick_opt,
        )
    }
}

// ── Single-file watch ─────────────────────────────────────────────────────────

/// Compile-time context for single-file watch mode.
///
/// Holds the parameters that are resolved once at startup and passed to every
/// rebuild — replaces the 6-7 individual constant args on `rebuild_file` and
/// `liveness_probe_file`, removing the `#[allow(clippy::too_many_arguments)]`
/// suppressions (issue #6 / zero-warnings policy).
///
/// `output_path` is the path resolved from the startup compile (intrinsic: kind
/// derived from the first compile result). On each rebuild the same path is reused
/// unless `-o` was specified explicitly, in which case that path is canonical.
/// For the watch single-file case the path is stable across recompiles (the template
/// kind cannot change without the template itself changing, which triggers a rebuild).
struct FileCompileCtx {
    entry: PathBuf,
    vars_path: Option<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    static_set_string_vars: Vec<(String, String)>,
    /// The `-o <path>` or `--out-dir` argument passed by the user, if any.
    /// Kept here so `rebuild_file` can reuse the same path-derivation logic for
    /// the dynamic path case (where output_path itself stays None until after compile).
    output_arg: Option<String>,
    out_dir: Option<PathBuf>,
    output_path: Option<PathBuf>,
    quiet: bool,
}

/// Mutable loop state for single-file watch mode.
///
/// Groups the per-loop variables that are updated on every rebuild or liveness tick,
/// mirroring `DirWatchState` for directory mode (eliminates the asymmetry noted in
/// the architecture review).
struct FileWatchState {
    /// Directories currently registered with `watcher`.
    watched_dirs: BTreeSet<PathBuf>,
    /// Subset of `watched_dirs` that have been successfully armed (registered with the
    /// OS watcher).  Used by `liveness_probe_file` to skip the `watcher.watch()` syscall
    /// for dirs that are already known-good — steady-state idle cost becomes O(missing_dirs)
    /// ≈ O(0) rather than O(watched_dirs) (ADR-021 / issue #1).
    armed_dirs: BTreeSet<PathBuf>,
    /// Set of paths relevant to the current build (entry + deps + vars).
    foi: HashSet<PathBuf>,
    /// Snapshot of `(mtime, size)` used by the liveness probe (ADR-021).
    last_mtimes: StampMap,
    /// Content-dedup map keyed by output-path string (or `"<stdout>"`).
    last_written: HashMap<String, String>,
    /// Whether the entry file was missing on the previous liveness tick.
    entry_was_missing: bool,
    /// True on the very first tick; forces a reconcile to close the startup race window.
    first_tick: bool,
    /// Parent dirs that were missing on the previous tick (edge-triggered recovery).
    missing_watched_dirs: BTreeSet<PathBuf>,
}

/// Outcome returned by `handle_fs_event_file` to tell the loop what to do next.
enum FileEventAction {
    /// Skip this message (Access event or irrelevant path) — go back to `recv_next`.
    Skip,
    /// Ctrl+C received — stop watching.
    Stop,
    /// Rebuild triggered.
    Rebuild,
}

/// Run the idle-tick liveness probe for single-file mode (ADR-021).
///
/// Re-arms watches for dirs that were missing or not yet armed; skips the
/// `watcher.watch()` syscall for dirs already known-good (`armed_dirs`).
/// Applies edge-triggered recovery logic, checks `(mtime, size)` of all
/// files of interest.
///
/// Returns `true` when a rebuild is needed (recovery or mtime change detected).
fn liveness_probe_file(
    ctx: &FileCompileCtx,
    watcher: &mut RecommendedWatcher,
    state: &mut FileWatchState,
) -> bool {
    // 1. Re-arm watches for dirs that need attention (ADR-021 idle-O(1) fix).
    //    A dir "needs attention" if it was previously missing OR not yet armed.
    //    Already-armed, currently-present dirs are not touched — steady-state idle
    //    cost becomes O(missing_dirs) ≈ O(0), not O(watched_dirs).
    let desired_dirs: BTreeSet<PathBuf> = dirs_to_watch(&ctx.entry, &[], ctx.vars_path.as_deref())
        .union(&state.watched_dirs)
        .cloned()
        .collect();
    let dir_statuses: Vec<(PathBuf, bool, bool)> = desired_dirs
        .iter()
        .map(|d| {
            let exists = d.exists();
            // Only pay the watcher.watch() syscall when the dir was missing last tick
            // or has not yet been armed — existing armed dirs are left alone.
            let needs_arm = !state.armed_dirs.contains(d) || state.missing_watched_dirs.contains(d);
            let rearm_ok = if exists && needs_arm {
                let ok = watcher.watch(d, RecursiveMode::NonRecursive).is_ok();
                if ok {
                    state.armed_dirs.insert(d.clone());
                }
                ok
            } else {
                // Dir is already armed and was not missing — treat as armed-ok.
                // If it disappeared, external_recovery_decision will catch the
                // vanish→reappear edge on the next tick.
                exists
            };
            (d.clone(), exists, rearm_ok)
        })
        .collect();
    // Remove vanished dirs from armed_dirs using the already-computed exists flags
    // rather than re-stating each dir (avoids a second stat per dir per tick).
    for (d, exists, _) in &dir_statuses {
        if !exists {
            state.armed_dirs.remove(d);
        }
    }
    // Edge-triggered recovery (ADR-021): mirrors external_recovery_decision used in
    // dir mode — a dir that STAYS missing must not trigger recovery every tick.
    let (dirs_recovery, now_missing_dirs) =
        external_recovery_decision(&state.missing_watched_dirs, &dir_statuses);
    state.missing_watched_dirs = now_missing_dirs;

    // 2. Determine if we need a full reconcile:
    //    (a) first tick, (b) edge-triggered dir recovery,
    //    (c) entry was missing and now exists (vanish→reappear edge).
    let entry_now_exists = ctx.entry.exists();
    let recovery =
        state.first_tick || dirs_recovery || (state.entry_was_missing && entry_now_exists);
    state.first_tick = false;
    state.entry_was_missing = !entry_now_exists;

    // 3. Cheap (mtime, size) check on files_of_interest.
    let changed = state_differs(&state.foi, &state.last_mtimes);

    recovery || changed
}

/// Classify an incoming `Msg` for single-file mode.
///
/// Returns the action the loop should take: skip irrelevant messages, stop on
/// Ctrl+C, or proceed to rebuild after draining the debounce window.
fn handle_fs_event_file(
    msg: Msg,
    foi: &HashSet<PathBuf>,
    rx: &mpsc::Receiver<Msg>,
    debounce_ms: u64,
    clear: bool,
) -> FileEventAction {
    let interrupted = match msg {
        Msg::Interrupt => true,
        Msg::Fs(Err(e)) => {
            eprint_warning(&format!("warning: watch error: {}", safe_inline(&e)));
            // Non-fatal watch error — skip but don't rebuild.
            return FileEventAction::Skip;
        }
        Msg::Fs(Ok(ref event)) => {
            // Drop Access events (inotify reads) before path check.
            if !is_content_event(&event.kind) {
                return FileEventAction::Skip;
            }
            if !event_is_relevant(event, foi) {
                return FileEventAction::Skip; // Not relevant — skip debounce entirely.
            }
            false
        }
    };

    if interrupted {
        return FileEventAction::Stop;
    }

    // Drain the debounce window.
    let (_extra_paths, interrupted2) = drain_debounce(rx, debounce_ms);
    if interrupted2 {
        return FileEventAction::Stop;
    }

    // Clear terminal if requested (only when stderr is a TTY).
    if clear {
        clear_terminal();
    }

    FileEventAction::Rebuild
}

/// Compile `entry`, compare with last-written content, resync watches, and write
/// if changed.  Called from both the idle-tick and the FS-event branch of
/// `run_watch_file` — the single canonical implementation of the
/// compile→dedup→resync→write→settle sequence for single-file mode.
///
/// `ctx` holds compile-time constants; `state` holds all mutable loop state;
/// `watcher` is passed separately (non-Clone, distinct lifecycle role).
///
/// # Invariants preserved
/// - ADR-016: `foi` and `watched_dirs` always recomputed from fresh dep output.
/// - PF-004: all reads go through `compile_to_content`.
/// - Error-settle: `last_mtimes` updated on vars error, compile error, and write error.
fn rebuild_file(
    ctx: &FileCompileCtx,
    watcher: &mut RecommendedWatcher,
    state: &mut FileWatchState,
) {
    // Soft-error: vars file may be temporarily absent (AC-W7 / AC-C5).
    // Print the error, settle mtime to avoid re-fire, and keep watching.
    let runtime_vars = match build_runtime_vars(RuntimeVarArgs {
        vars: ctx.vars_path.clone(),
        set_vars: ctx.static_set_vars.clone(),
        set_string_vars: ctx.static_set_string_vars.clone(),
    }) {
        Ok(v) => v,
        Err(e) => {
            eprint_error(e);
            state.last_mtimes = snapshot_state(&state.foi);
            return;
        }
    };

    let t0 = Instant::now();
    match compile_to_content(
        &ctx.entry,
        runtime_vars,
        ctx.quiet,
        mds::CompileOptions::default(),
    ) {
        Ok(compiled) => {
            // Derive the output path from the compiled kind (intrinsic extension).
            // If ctx.output_path is already set (startup succeeded), reuse it.
            // If ctx.output_path is None: either output is stdout (output_arg == Some("-")
            // or no flags) OR startup failed. In either case, re-derive from kind + args.
            // We load project config lazily here only if output_path is None and we need
            // to derive from mds.json — for the common "startup succeeded" path this is free.
            let output_path = if ctx.output_path.is_some() {
                ctx.output_path.clone()
            } else {
                // output_path is None: this means either stdout or startup failed.
                // Re-derive using kind. If -o - or stdin fallback, this returns None (stdout).
                let config = load_config(&ctx.entry).unwrap_or(None);
                resolve_output_path_for_kind(
                    &Some(ctx.entry.clone()),
                    &ctx.output_arg,
                    &ctx.out_dir,
                    &config,
                    compiled.kind,
                    ctx.quiet,
                )
                .unwrap_or(None)
            };

            // Build the output_key for content-dedup.
            let output_key: String = output_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<stdout>".to_string());

            // Content-based dedup: skip write + summary line when unchanged.
            let content_changed = state
                .last_written
                .get(&output_key)
                .is_none_or(|prev| *prev != compiled.content);

            // ADR-016: always recompute dep set from fresh output.
            let new_dirs =
                dirs_to_watch(&ctx.entry, &compiled.dependencies, ctx.vars_path.as_deref());
            state.watched_dirs = resync_watches(watcher, &state.watched_dirs, &new_dirs);
            // Keep armed_dirs in sync: all dirs in watched_dirs are successfully armed;
            // dirs removed by resync_watches are no longer in watched_dirs.
            state.armed_dirs = state.watched_dirs.clone();
            state.foi =
                files_of_interest(&ctx.entry, &compiled.dependencies, ctx.vars_path.as_deref());
            // Update mtime snapshot after a compile (even if content unchanged).
            state.last_mtimes = snapshot_state(&state.foi);

            if content_changed {
                match write_output(output_path.clone(), &compiled.content, ctx.quiet, false) {
                    Ok(()) => {
                        let elapsed = t0.elapsed().as_millis();
                        let dep_count = compiled.dependencies.len();
                        let out_display = output_path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<stdout>".to_string());
                        state.last_written.insert(output_key, compiled.content);
                        if !ctx.quiet {
                            eprintln!(
                                "Recompiled {} ({} deps) in {}ms",
                                safe_inline(&out_display),
                                dep_count,
                                elapsed
                            );
                        }
                    }
                    Err(e) => {
                        eprint_error(e);
                        // Error-settle: update snapshot so we don't re-fire.
                        state.last_mtimes = snapshot_state(&state.foi);
                    }
                }
            }
        }
        Err(e) => {
            eprint_error(e);
            // Error-settle: snapshot current state so the tick gate
            // won't re-fire on the same unchanged files (AC-R7/W6).
            state.last_mtimes = snapshot_state(&state.foi);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_watch_file(
    entry: PathBuf,
    output: Option<String>,
    out_dir: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    set_string_vars: Vec<(String, String)>,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
    tick: Option<Duration>,
) -> Result<()> {
    // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on macOS).
    // Also rejects a symlinked vars file at startup (build parity — PF-004).
    let vars_path = canonicalize_vars_path(vars).map_err(miette::Error::from)?;

    // Build runtime vars from the set_vars statics (vars file is reloaded each rebuild).
    let static_set_vars = set_vars;
    let static_set_string_vars = set_string_vars;

    if !quiet {
        eprintln!("Watching {}", safe_path(&entry));
    }

    // ── Arm before publish (startup race) ─────────────────────────────────────
    //
    // GUARANTEED for the entry and the vars file: the directory watch is armed and
    // the `(mtime, size)` baseline captured strictly BEFORE either is first read.
    // Both are knowable from the command line, so both happen below, ahead of
    // `build_runtime_vars` (reads vars) and `compile_and_write` (reads the entry).
    //
    // NOT guaranteed for dependencies. A dep only becomes known when the compile
    // reports it, so a dep whose directory is not the entry's or the vars file's is
    // armed — and has its baseline taken — only *after* the compile has already read
    // it (see the post-compile arming loop and the baseline merge further down). An
    // edit to such a dep inside that window is still invisible to both detectors.
    // Deps that happen to sit in an already-armed directory are covered by the OS
    // watch from the start; cross-directory deps are the residual, and are what
    // `MDS_TEST_READY` exists to let the integration suite synchronise past.
    //
    // The watcher used to be created *after* the initial compile so the dedup
    // baseline was recorded "before any FSEvents arrive". That ordering left a
    // window — output written → watcher armed → baseline snapshotted — in which an
    // edit generated no event at all: inotify was not yet armed, so there was
    // nothing to deliver it to. A user who saved during startup saw no rebuild.
    //
    // Whether that was *late* or *permanent* was decided by the liveness probe, and
    // NOT by the poisoned baseline: `liveness_probe_file` returns `recovery ||
    // changed`, and `recovery` is true on `first_tick` unconditionally — so on an
    // idle tree the first tick rebuilt and the edit was recovered regardless of what
    // the baseline held. What made it permanent is that the tick may never arrive:
    // `recv_timeout` restarts its deadline on every message, so a steady stream of
    // irrelevant events in the watched tree starves the probe indefinitely. That
    // starvation is tracked separately as #319; closing this window is what stops it
    // being reachable from a normal startup.
    //
    // Arming first means the watcher may observe the compile's own reads and the
    // startup output write. Three pre-existing guards cover that, and each is
    // still load-bearing here:
    //   1. `is_content_event` drops every `Access(_)` event, which is exactly
    //      what a source-file *read* produces on Linux (IN_OPEN / IN_ACCESS /
    //      IN_CLOSE_NOWRITE). The startup compile can no longer busy-loop itself.
    //   2. `event_is_relevant` filters to `files_of_interest` — entry, deps and
    //      the vars file. The startup output write (and the temp sibling that
    //      `atomic_write_file` renames over it) is never in that set.
    //   3. `last_written` content-dedup is seeded below, before the event loop
    //      begins. Queued events are only *processed* inside the loop, so any
    //      event that survives guards 1 and 2 recompiles to identical content
    //      and is suppressed without a write or a status line.
    // Worst case is therefore one redundant compile that dedups to no write.
    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_fs = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx_fs.send(Msg::Fs(res));
        },
        notify::Config::default(),
    )
    .map_err(|e| miette::miette!("failed to initialize file watcher: {e}"))?;

    // Arm the directories that are knowable before any read: the entry's parent
    // and the vars file's parent. Dependency dirs are unknown until the compile
    // reports them and are armed immediately afterwards.
    //
    // Best-effort here — a dir that is missing or fails to arm is re-attempted by
    // the post-compile loop below, which owns the hard-error contract for the
    // full dir set. Splitting it this way keeps startup failure messages identical
    // to the pre-reorder behaviour.
    let mut watched_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for dir in dirs_to_watch(&entry, &[], vars_path.as_deref()) {
        if dir.exists() && watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
            watched_dirs.insert(dir);
        }
    }

    // Capture the entry/vars baseline BEFORE the first read of either. Both
    // `build_runtime_vars` (reads the vars file) and `compile_and_write` (reads
    // the entry) come after this point, so an edit landing during startup leaves
    // this snapshot strictly older than the file — and the liveness probe sees it.
    let mut pre_mtimes = snapshot_state(&files_of_interest(&entry, &[], vars_path.as_deref()));
    let entry_was_missing = !entry.exists();

    // Initial compile: compile first, derive output path from kind (compile-then-route).
    // For explicit -o / --out-dir the path is determined by the flag.
    // For the default case (no explicit flag), the path depends on the output kind, which
    // is only known after compilation — so we compile first, then derive.
    let runtime_vars = build_runtime_vars(RuntimeVarArgs {
        vars: vars_path.clone(),
        set_vars: static_set_vars.clone(),
        set_string_vars: static_set_string_vars.clone(),
    })?;

    // Load project config (for output_dir) — used if no explicit -o / --out-dir.
    let config = load_config(&entry)?;

    // Initial compile: returns (output_path, deps, content).
    // content is captured here so the baseline block below can reuse it without
    // recompiling (issue 3 — avoids a redundant second compile at startup).
    let (output_path, initial_deps, initial_content) = match compile_and_write(
        &entry,
        &output,
        &out_dir,
        &config,
        runtime_vars,
        quiet,
        mds::CompileOptions::default(),
    ) {
        Ok(result) => result,
        Err(e) => {
            // Initial compile error: print and continue watching (entry dir still watched).
            eprint_error(e);
            // Fall back: resolve output path with Markdown kind as a placeholder so we
            // know where to watch. This path may not match a later successful compile if
            // the template has @message blocks, but it will correct on first successful rebuild.
            let fallback_path = resolve_output_path_for_kind(
                &Some(entry.clone()),
                &output,
                &out_dir,
                &config,
                OutputKind::Markdown,
                quiet,
            )
            .unwrap_or(None);
            (fallback_path, vec![], String::new())
        }
    };

    // Baseline the dependencies the compile just reported, before anything else runs.
    //
    // The entry and vars baselines above precede their own reads; a dependency's cannot,
    // because the compile is what discovers the dependency exists. Taking it here rather
    // than with the post-compile snapshot below shrinks the window in which an edit to a
    // dependency is invisible to the baseline from "the rest of startup" to the gap
    // between the compile returning and this loop. `baseline_path` keeps the older of
    // any two entries.
    //
    // HONEST SCOPE: this is defence in depth and has **no measured observable effect**
    // today. `liveness_probe_file` returns `recovery || changed` with `recovery`
    // including `first_tick`, so file mode's first tick rebuilds unconditionally and
    // recovers such an edit whatever the baseline says — an arm with this loop removed
    // still passed the covering test 10/10. What it buys is that `last_mtimes` means
    // what its name says, so the probe stays correct if that unconditional first-tick
    // rebuild is ever removed. Directory mode has no such fallback, which is why the
    // equivalent capture there is load-bearing and measured (#321).
    for dep in &initial_deps {
        baseline_path(Path::new(dep), &mut pre_mtimes);
    }

    // The startup output is now published — the positive-control injection point.
    startup_race_probe();

    // Key: resolved output path string, or the sentinel "<stdout>" when output_path is None.
    let output_key: String = output_path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<stdout>".to_string());

    // Arm the dependency directories the compile just reported. Dirs already armed
    // above are skipped; anything still unarmed — including a pre-arm attempt that
    // failed — is a hard startup error, as it was before the reorder.
    let init_dirs = dirs_to_watch(&entry, &initial_deps, vars_path.as_deref());
    let unarmed: Vec<PathBuf> = init_dirs.difference(&watched_dirs).cloned().collect();
    for dir in unarmed {
        match watcher.watch(&dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                watched_dirs.insert(dir);
            }
            Err(e) => {
                return Err(miette::miette!(
                    "failed to watch directory {}: {e}\n\
                     hint: on Linux you may need to increase fs.inotify.max_user_watches",
                    dir.display()
                ));
            }
        }
    }

    // Record the dedup baseline. The event loop has not started, so nothing can
    // consult this map before it is populated (guard 3 above).
    // Reuse initial_content from the startup compile (issue 3 — no second compile needed).
    let mut last_written: HashMap<String, String> = HashMap::new();
    if !initial_content.is_empty() {
        // initial_content is empty only when the initial compile failed (error path above).
        // In that case leave last_written empty so the next successful rebuild always writes.
        last_written.insert(output_key.clone(), initial_content);
    }

    let foi = files_of_interest(&entry, &initial_deps, vars_path.as_deref());

    // Build pre-loop FileWatchState (mtime snapshot + edge-trigger seeds).
    let missing_watched_dirs: BTreeSet<PathBuf> = {
        let desired = dirs_to_watch(&entry, &[], vars_path.as_deref())
            .union(&watched_dirs)
            .cloned()
            .collect::<BTreeSet<_>>();
        desired.into_iter().filter(|d| !d.exists()).collect()
    };

    // Merge the two baselines. Dependencies are only discovered by the compile, so
    // theirs is captured now; the entry/vars entries taken before the compile
    // overwrite the fresh ones because they are strictly older. That is what makes
    // an edit landing anywhere inside the startup window still register as a
    // difference on the first liveness tick.
    let mut last_mtimes = snapshot_state(&foi);
    // Witness for the assertion below. The merge DIRECTION is the load-bearing part:
    // only the pre-compile pair predates an edit that landed during startup, so
    // inverting the merge (or switching to an `or_insert`-style one that keeps the
    // value already present) silently restores the lost-save bug while every test
    // still passes. `entry` is inserted verbatim by `files_of_interest`, so this
    // lookup hits. The previous assertion here compared the key sets of
    // `files_of_interest(entry, &[], vars)` and `files_of_interest(entry, &deps, vars)`
    // — a subset relation those two calls guarantee by construction, so it could
    // never fail and guarded nothing.
    let entry_pre = pre_mtimes.get(&entry).copied();
    last_mtimes.extend(pre_mtimes);
    debug_assert_eq!(
        last_mtimes.get(&entry).copied(),
        entry_pre,
        "baseline merge inverted: the entry's pre-compile (mtime, size) must survive \
         the merge with the post-compile snapshot, or an edit made during startup can \
         never register as a difference"
    );

    let mut state = FileWatchState {
        // armed_dirs mirrors watched_dirs at startup: all dirs that were successfully
        // registered in the loop above are considered armed (ADR-021 idle-O(1) fix).
        armed_dirs: watched_dirs.clone(),
        watched_dirs,
        foi,
        last_mtimes,
        last_written,
        entry_was_missing,
        first_tick: true,
        missing_watched_dirs,
    };

    // Build compile-time context (replaces the 7 individual constant args previously
    // threaded through rebuild_file / liveness_probe_file — removes both
    // #[allow(clippy::too_many_arguments)] suppressions).
    let ctx = FileCompileCtx {
        entry,
        vars_path,
        static_set_vars,
        static_set_string_vars,
        output_arg: output,
        out_dir,
        output_path,
        quiet,
    };

    // ── Ctrl+C: install LAST, immediately before the loop that can service it ──
    //
    // Installing a handler converts SIGINT from "terminate now" into "enqueue
    // `Msg::Interrupt`", and that message is only ever read by the event loop below.
    // So every instruction between `set_handler` and the loop is a stretch of
    // startup during which Ctrl+C does nothing at all — the process keeps compiling
    // and keeps writing output, then exits 0 as if the user had never pressed it.
    // Repeat presses do not help; only SIGKILL does. The cost scales with the size
    // of the startup compile, so this must stay below it. Nothing above needs the
    // handler: arming the watcher only needs `tx`, which is cloned here just as well.
    let tx_ctrlc = tx.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = tx_ctrlc.send(Msg::Interrupt);
    });

    // Every dir is armed and every baseline captured — the watch is now live.
    emit_ready_marker();

    // ── Watch loop ────────────────────────────────────────────────────────────
    // The outer loop processes one event batch at a time and is bounded:
    // it terminates on Interrupt, Disconnected, or when tick probe fires.
    let mut clock = TickClock::new(tick);
    loop {
        match clock.recv_next(&rx) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(None) => {
                // Idle tick — run liveness probe (ADR-021).
                if liveness_probe_file(&ctx, &mut watcher, &mut state) {
                    rebuild_file(&ctx, &mut watcher, &mut state);
                }
                continue;
            }
            Ok(Some(msg)) => match handle_fs_event_file(msg, &state.foi, &rx, debounce_ms, clear) {
                FileEventAction::Skip => continue,
                FileEventAction::Stop => {
                    stop_watching(ctx.quiet);
                    return Ok(());
                }
                FileEventAction::Rebuild => rebuild_file(&ctx, &mut watcher, &mut state),
            },
            // Unreachable: recv_timeout returns Ok(None) for Timeout, not an Err.
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    stop_watching(ctx.quiet);
    Ok(())
}

// ── Directory watch ───────────────────────────────────────────────────────────

const MAX_COLLECT_DEPTH: usize = 64;

/// Mutable state for the directory-mode watch loop.
struct DirWatchState {
    /// Forward dependency map: canonical source → its canonical (transitive) deps.
    /// Dep values are already canonical from `compile_with_deps`; do not re-canonicalize.
    forward_deps: HashMap<PathBuf, Vec<PathBuf>>,
    /// Sources whose last compile attempt failed. Re-seeded into every batch that
    /// carries a real change, so a fix to whatever broke them is picked up.
    errored: HashSet<PathBuf>,
    /// Last-seen collected `.mds` set for reconcile/rename detection.
    known_files: BTreeSet<PathBuf>,
    /// Content-dedup map keyed by output path.
    last_written: HashMap<PathBuf, String>,
    /// Parent dirs of dependencies located outside the watched root.
    /// Watched NonRecursive; re-armed by liveness probe.
    external_dep_dirs: BTreeSet<PathBuf>,
    /// `(mtime, size)` baseline over [`DirWatchState::tracked_set`] — sources *and*
    /// dependencies. Read by the idle tick's content backstop and re-written at the end
    /// of every batch, so the tick reports only what the batch did not already handle
    /// (#321).
    last_mtimes: StampMap,
}

impl DirWatchState {
    /// Record a successful compile for `src` with the given dep paths and output content.
    ///
    /// Updates `forward_deps`, removes from `errored`, inserts into `known_files`,
    /// and updates `external_dep_dirs` for any deps outside `root`.
    fn record_success(
        &mut self,
        src: &Path,
        dep_paths: Vec<PathBuf>,
        root: &Path,
        out: Option<&Path>,
        content: Option<String>,
    ) {
        // Track external dep dirs (DD3 — cross-root).
        for dep in &dep_paths {
            if let Some(parent) = dep.parent() {
                if !parent.starts_with(root) {
                    self.external_dep_dirs.insert(parent.to_path_buf());
                }
            }
        }
        self.forward_deps.insert(src.to_path_buf(), dep_paths);
        self.errored.remove(src);
        self.known_files.insert(src.to_path_buf());
        if let (Some(out_path), Some(c)) = (out, content) {
            self.last_written.insert(out_path.to_path_buf(), c);
        }
    }

    /// Record a compile error for `src`, **keeping** whatever dep set the last
    /// successful compile recorded (an empty one when there has never been one).
    ///
    /// Discarding the dep set here is what made a cross-root edit unrecoverable
    /// (#321). `process_dir_batch_incremental` recomputes `external_dep_dirs` from
    /// `forward_deps` after every batch, so clearing an importer's deps on a failed
    /// compile also dropped the external directory those deps live in. The next
    /// event for that directory was then rejected by `handle_fs_event_dir` as
    /// "neither under root nor in a known external dep dir", and the liveness probe
    /// went on to `unwatch()` the directory outright. A single compile against a
    /// half-written file — an editor's `O_TRUNC` open observed before its `write`
    /// lands — was enough to blind the watcher to that dependency for the rest of
    /// the session, with the failed compile as the only trace.
    ///
    /// The retained set is used only to decide what to *watch* and what to re-seed —
    /// never as a substitute for recompiling. It therefore only ever widens what may
    /// trigger a rebuild, and the cost of a stale edge is one recompile whose output
    /// the `last_written` dedup then suppresses. ADR-016's freshness rule is about the
    /// dep set a *rebuild* records, and that still comes from fresh `compile_to_content`
    /// output on every success; a failed compile produces no fresh set to record.
    fn record_error(&mut self, src: &Path) {
        self.errored.insert(src.to_path_buf());
        self.forward_deps.entry(src.to_path_buf()).or_default();
    }

    /// Every path whose **content** the watcher must react to: all known sources
    /// plus every dependency they pull in, including cross-root ones outside the
    /// watched root.
    ///
    /// This is the domain of the idle-tick content backstop and of the `last_mtimes`
    /// baseline that feeds it (#321). `known_files` alone cannot serve: it holds
    /// exactly what `collect_mds_files(root)` returns, so a cross-root dependency is
    /// never in it, and a probe diffing only that walk can see such a file appear or
    /// vanish but never *change*.
    fn tracked_set(&self) -> HashSet<PathBuf> {
        let mut tracked: HashSet<PathBuf> = self.known_files.iter().cloned().collect();
        for deps in self.forward_deps.values() {
            tracked.extend(deps.iter().cloned());
        }
        tracked
    }

    /// Remove all state for a deleted source and its output.
    fn forget(&mut self, src: &Path, out: &Path) {
        self.last_written.remove(out);
        self.forward_deps.remove(src);
        self.errored.remove(src);
        self.known_files.remove(src);
    }
}

/// State for the dir-mode liveness probe (ADR-021).
struct LivenessState {
    /// Set to true on the very first tick so we do a reconcile after startup.
    first_tick: bool,
    /// Tracks whether the root existed on the previous tick.
    root_was_missing: bool,
    /// Whether the OS watcher was successfully armed for the root on the last tick.
    ///
    /// Mirrors the `armed_dirs` discipline from file mode: skip `watcher.watch(root, …)`
    /// on healthy ticks so the OS-level re-WalkDir / FSEvents stream teardown does not
    /// happen every idle tick — O(1) idle cost regardless of subtree size (ADR-021).
    root_armed: bool,
    /// External dep dirs that were missing on the previous tick.
    ///
    /// Recovery is **edge-triggered**: a missing external dir triggers a full
    /// reconcile only when it *reappears* (vanish→reappear), never while it stays
    /// missing. A permanently-missing external dir must NOT force an O(tree) walk
    /// on every idle tick (ADR-021 / AC-P1).
    missing_external_dirs: BTreeSet<PathBuf>,
    /// External dep dirs that are currently armed with the OS watcher.
    ///
    /// Used to call `watcher.unwatch()` when an external dir is pruned from
    /// `state.external_dep_dirs` (e.g. because a cross-root @import was edited away).
    /// Prevents inotify/FSEvents watch leaks for the process lifetime (avoids
    /// approaching `fs.inotify.max_user_watches`). Mirrors the `resync_watches`
    /// discipline from file mode.
    armed_external_dirs: BTreeSet<PathBuf>,
}

/// Compile a single in-root source file, update `state`, and optionally write output.
///
/// This is the shared kernel for both the `vars_changed` full-recompile loop and the
/// per-affected-source incremental loop in `process_dir_batch` — collapsing the
/// 2× duplicated compile→dedup→write block inside that function.
///
/// `write_output_file`: when `true` the compiled content is written (non-partial sources).
/// When `false` the graph is refreshed but no output file is created (used for partials
/// and external-only deps where the caller decides skip/continue).
///
/// # Invariants preserved
/// - ADR-016: dep set recomputed from fresh `compile_to_content` output.
/// - PF-004: all reads go through `compile_to_content`.
///
/// Does **not** touch `state.last_mtimes`: the content backstop's baseline is settled
/// once per batch by `process_dir_batch`, over the whole tracked set (#321).
///
/// Compile success/failure is already signalled via `state.errored`; the caller uses
/// that set rather than this function's return value, so the return type is `()`.
fn compile_one_source(
    src: &Path,
    root: &Path,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) {
    let t0 = Instant::now();
    match compile_to_content(
        src,
        runtime_vars.clone(),
        quiet,
        mds::CompileOptions::default(),
    ) {
        Ok(compiled) => {
            let dep_paths: Vec<PathBuf> = compiled.dependencies.iter().map(PathBuf::from).collect();

            // Partials (DD2): refresh graph edges but do NOT write output.
            if is_partial(src) {
                state.record_success(src, dep_paths, root, None, None);
                return;
            }

            // Derive the output path from the compiled kind (intrinsic extension).
            // AC-FUNC-23: a @message template writes .json; a plain template writes .md.
            let ext = compiled.kind.extension();
            let out = output_path_for(src, root, output_base, ext);

            // Content-based dedup: skip write when content unchanged.
            let content_changed = state
                .last_written
                .get(&out)
                .is_none_or(|prev| *prev != compiled.content);

            if content_changed {
                match write_output(Some(out.clone()), &compiled.content, quiet, false) {
                    Ok(()) => {
                        let elapsed = t0.elapsed().as_millis();
                        let dep_count = compiled.dependencies.len();
                        if !quiet {
                            eprintln!(
                                "Recompiled {} ({} deps) in {}ms",
                                safe_path(&out),
                                dep_count,
                                elapsed
                            );
                        }
                        // AC-FUNC-23 (stale-output cleanup on format-flip in watch mode):
                        // probe for the wrong-extension sibling and unlink it — but ONLY
                        // when the tool itself wrote that sibling this session (gate on
                        // last_written membership). This prevents clobbering a hand-authored
                        // file that happens to share the stem (e.g. notes.md kept next to
                        // notes.mds which now compiles to notes.json). Issue 1.
                        //
                        // This unlink must NOT trigger the watcher: `out` is the NEW
                        // output path we just wrote; the stale sibling has a DIFFERENT
                        // extension, so it is outside the `last_written` map and the
                        // `is_content_event` gate will drop any inotify events it causes.
                        // The watcher self-trigger guard (content-dedup / last_written)
                        // also covers the freshly written `out` — the next event for that
                        // path will find identical content and skip the write.
                        let base_no_ext = output_base_no_ext(src, root, output_base);
                        let stale_path =
                            base_no_ext.with_extension(compiled.kind.stale_extension());
                        // Remove the stale-extension sibling from last_written so the key
                        // doesn't accumulate stale entries (memory hygiene). The remove()
                        // return value tells us whether this tool wrote the stale path.
                        let tool_wrote_stale = state.last_written.remove(&stale_path).is_some();
                        if tool_wrote_stale {
                            probe_and_remove_stale(&base_no_ext, compiled.kind);
                        }

                        state.record_success(
                            src,
                            dep_paths,
                            root,
                            Some(&out),
                            Some(compiled.content),
                        );
                    }
                    Err(e) => {
                        eprint_error(e);
                        state.record_error(src);
                    }
                }
            } else {
                // Content unchanged — still refresh graph edges + known_files.
                state.record_success(src, dep_paths, root, None, None);
            }
        }
        Err(e) => {
            eprint_error(e);
            state.record_error(src);
        }
    }
}

/// Return value from `dir_watch_startup` bundling the watcher, channel, state,
/// liveness state, and context struct produced during startup.
struct DirStartup {
    watcher: RecommendedWatcher,
    rx: mpsc::Receiver<Msg>,
    state: DirWatchState,
    liveness: LivenessState,
    ctx: DirWatchCtx,
}

/// Compile-time context for directory-mode watch, parallel to `FileCompileCtx`.
///
/// Groups the parameters resolved once at startup and threaded into every
/// liveness-probe and event-handler call — removes `#[allow(clippy::too_many_arguments)]`
/// from the extracted helper functions (issue #6 / zero-warnings policy).
struct DirWatchCtx {
    root: PathBuf,
    vars_path: Option<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    static_set_string_vars: Vec<(String, String)>,
    output_base: OutputBase,
    exclude_prefix: Option<PathBuf>,
    vars_dir_extra: Option<PathBuf>,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
}

/// Run the idle-tick liveness probe for directory mode (ADR-021, DD1).
///
/// Re-arms root + external dirs + vars dir. Applies edge-triggered recovery
/// to decide whether a full reconcile (collect_mds_files diff) is needed.
/// Mutates `liveness` state for next tick.
fn liveness_probe_dir(
    ctx: &DirWatchCtx,
    watcher: &mut RecommendedWatcher,
    liveness: &mut LivenessState,
    state: &mut DirWatchState,
) {
    // 1. Re-arm root as Recursive (gated — ADR-021 / issue #1 idle O(1) fix).
    //
    // Skip the `watcher.watch()` syscall on healthy ticks when root is already armed:
    // on Linux `notify` re-WalkDirs the entire subtree + calls `inotify_add_watch` per
    // subdirectory on every `watch()` call regardless of mode; on macOS it tears down
    // and recreates the FSEvents stream.  Only re-arm when:
    //   (a) first_tick — not yet armed
    //   (b) root was missing last tick but now exists (vanish→reappear edge)
    //   (c) root_armed is false — a previous arm attempt failed; retry
    let root_now_exists = ctx.root.exists();
    let need_root_rearm = liveness.first_tick
        || (liveness.root_was_missing && root_now_exists)
        || !liveness.root_armed;
    let root_ok = if root_now_exists && need_root_rearm {
        let ok = watcher.watch(&ctx.root, RecursiveMode::Recursive).is_ok();
        liveness.root_armed = ok;
        ok
    } else if root_now_exists {
        // Already armed and still healthy — treat as ok without a syscall.
        true
    } else {
        // Root does not exist — unarmed until it reappears.
        liveness.root_armed = false;
        false
    };

    // Unwatch dirs that were pruned from external_dep_dirs by a previous batch
    // (issue #2 fix: release OS watches when cross-root @imports are edited away to
    // prevent inotify/FSEvents watch leaks approaching fs.inotify.max_user_watches).
    // `armed_external_dirs` tracks which dirs the OS watcher currently holds so we
    // can call `unwatch()` precisely on the difference.
    let dropped_external: Vec<PathBuf> = liveness
        .armed_external_dirs
        .iter()
        .filter(|d| !state.external_dep_dirs.contains(*d))
        .cloned()
        .collect();
    for d in &dropped_external {
        // Non-fatal: dir may have already been deleted.
        let _ = watcher.unwatch(d);
        liveness.armed_external_dirs.remove(d);
    }

    // Also clean up any stale entries from missing_external_dirs.
    liveness
        .missing_external_dirs
        .retain(|d| state.external_dep_dirs.contains(d));

    // Re-arm external dirs — gated like root re-arm: skip the syscall for dirs
    // that are already armed and still healthy (O(1) per healthy dir per tick).
    let ext_statuses: Vec<(PathBuf, bool, bool)> = state
        .external_dep_dirs
        .iter()
        .map(|ext_dir| {
            let exists = ext_dir.exists();
            let already_armed = liveness.armed_external_dirs.contains(ext_dir);
            let rearm_ok = if exists {
                if already_armed {
                    // Already armed and healthy — skip the syscall.
                    true
                } else {
                    let ok = watcher.watch(ext_dir, RecursiveMode::NonRecursive).is_ok();
                    if ok {
                        liveness.armed_external_dirs.insert(ext_dir.clone());
                    }
                    ok
                }
            } else {
                // Dir does not exist — ensure it is not marked as armed.
                liveness.armed_external_dirs.remove(ext_dir);
                false
            };
            (ext_dir.clone(), exists, rearm_ok)
        })
        .collect();
    let (external_recovery, now_missing_external) =
        external_recovery_decision(&liveness.missing_external_dirs, &ext_statuses);
    if let Some(ref vd) = ctx.vars_dir_extra {
        if vd.exists() {
            let _ = watcher.watch(vd, RecursiveMode::NonRecursive);
        }
    }

    // 2. Recovery trigger (ADR-021):
    //    `root_now_exists && !root_ok` = existing root whose re-arm failed (genuine watch loss).
    //    A *missing* root is handled by the `root_was_missing && root_now_exists` vanish→reappear
    //    edge and must NOT trigger recovery on every tick while absent (per-tick error spam).
    //    Note: `root_now_exists` and `root_ok` are already computed above in section 1.
    let recovery = liveness.first_tick
        || (root_now_exists && !root_ok)
        || external_recovery
        || (liveness.root_was_missing && root_now_exists);
    liveness.first_tick = false;
    liveness.root_was_missing = !root_now_exists;
    liveness.missing_external_dirs = now_missing_external;

    // 3. Content backstop (#321).
    //
    // The reconcile below diffs `collect_mds_files(root)` against `known_files`, which
    // reports only files that *appeared* or were *removed* under the root. A change to
    // a file's contents is invisible to it, and a cross-root dependency is not even in
    // that walk — so until this check existed, `last_mtimes` was written on every batch
    // in dir mode and never once read, and an edit whose event went undelivered was
    // lost for good. The events that go undelivered are not hypothetical: a
    // cross-root dependency is discovered by the compile that reads it, so its
    // directory cannot be armed until after that first read.
    //
    // Cost is one `stat` per tracked path per tick, short-circuited by nothing — the
    // full set is walked so every changed path joins the same batch. That is the same
    // price single-file mode has always paid via `state_differs` on its
    // files-of-interest, and it is O(sources + deps), not O(tree).
    let tracked = state.tracked_set();
    let mut batch: BTreeSet<PathBuf> = tracked
        .iter()
        .filter(|p| path_state_differs(p, &state.last_mtimes))
        .cloned()
        .collect();

    // 4. Full reconcile (appeared/removed), only on a recovery edge.
    //
    // `known_files` is replaced with the fresh walk *before* the batch runs, so the
    // re-baseline at the end of `process_dir_batch` already covers every file the walk
    // found — including one that appeared and then failed to compile, which
    // `record_error` deliberately does not add to `known_files`. Replacing afterwards
    // would leave such a file outside the baseline and cost one redundant compile on
    // the following tick.
    if recovery {
        let current: BTreeSet<PathBuf> =
            collect_mds_files(&ctx.root, MAX_COLLECT_DEPTH, ctx.exclude_prefix.as_deref())
                .into_iter()
                .map(|p| graph_key(&p))
                .collect();
        batch.extend(current.difference(&state.known_files).cloned());
        batch.extend(state.known_files.difference(&current).cloned());
        state.known_files = current;
    }

    if !batch.is_empty() {
        // Soft-error: vars file may be temporarily absent (AC-W7 / AC-C5).
        let runtime_vars = match build_runtime_vars(RuntimeVarArgs {
            vars: ctx.vars_path.clone(),
            set_vars: ctx.static_set_vars.clone(),
            set_string_vars: ctx.static_set_string_vars.clone(),
        }) {
            Ok(v) => v,
            Err(e) => {
                eprint_error(e);
                // Re-baseline so the next tick does not report the same change again
                // and turn one unreadable vars file into per-tick error spam.
                state.last_mtimes = snapshot_state(&state.tracked_set());
                return;
            }
        };
        process_dir_batch(
            &batch,
            false, /* vars_changed */
            &ctx.root,
            &ctx.output_base,
            &runtime_vars,
            ctx.quiet,
            state,
        );
    }
    // No baseline refresh here: `process_dir_batch` re-baselines `last_mtimes` over the
    // post-batch tracked set, and an empty batch means nothing appeared, was removed, or
    // changed — so the existing baseline is by definition still accurate.
}

/// Outcome returned by `handle_fs_event_dir` to tell the loop what to do next.
enum DirEventOutcome {
    /// Skip — nothing relevant (Access event, no .mds paths, no vars change).
    Skip,
    /// Ctrl+C received — stop watching.
    Stop,
    /// Batch computed and process_dir_batch already called by the handler.
    Done,
}

/// Process a single incoming `Msg` for directory mode.
///
/// Collects changed paths, drains the debounce window, filters irrelevant paths,
/// reloads vars, and calls `process_dir_batch`. Returns `DirEventOutcome` so the
/// caller knows whether to `continue`, `return`, or proceed.
fn handle_fs_event_dir(
    msg: Msg,
    ctx: &DirWatchCtx,
    rx: &mpsc::Receiver<Msg>,
    state: &mut DirWatchState,
) -> DirEventOutcome {
    let mut changed: BTreeSet<PathBuf> = BTreeSet::new();

    let interrupted = match msg {
        Msg::Interrupt => true,
        Msg::Fs(Err(e)) => {
            eprint_warning(&format!("warning: watch error: {}", safe_inline(&e)));
            return DirEventOutcome::Skip;
        }
        Msg::Fs(Ok(event)) => {
            // Drop Access events (inotify IN_ACCESS/IN_OPEN/IN_CLOSE_NOWRITE).
            // On Linux reading a .mds source file during compile emits Access
            // events that would re-seed the watcher in a busy-loop (~3000/s).
            if is_content_event(&event.kind) {
                for p in event.paths {
                    changed.insert(p);
                }
            }
            false
        }
    };

    if interrupted {
        return DirEventOutcome::Stop;
    }

    // Drain debounce window.
    let (extra, interrupted2) = drain_debounce(rx, ctx.debounce_ms);
    changed.extend(extra);
    if interrupted2 {
        return DirEventOutcome::Stop;
    }

    // Defense-in-depth: ignore events from inside the out-dir subtree.
    if let OutputBase::Dir(ref od) = ctx.output_base {
        changed.retain(|p| !p.starts_with(od));
    }

    // PF-004: drop events from default-excluded subdirectories (hidden dirs and
    // node_modules/) inside the watch root.  The initial walker never seeds files
    // from those dirs, so they are not in the dep graph and processing their
    // events would cause spurious rebuilds (e.g. npm install writing to
    // node_modules/ triggers a full re-scan on every package update).
    changed.retain(|p| !is_within_default_excluded_dir(&ctx.root, p));

    // Check if the vars file changed.
    let vars_changed = ctx
        .vars_path
        .as_deref()
        .map(|vf| changed.contains(vf))
        .unwrap_or(false);

    // Collect .mds paths that are either under root OR in known external dep dirs.
    let mds_changed: BTreeSet<PathBuf> = changed
        .iter()
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("mds")
                && (p.starts_with(&ctx.root)
                    || state
                        .external_dep_dirs
                        .iter()
                        .any(|d| p.parent() == Some(d.as_path())))
        })
        .map(|p| graph_key(p))
        .collect();

    if mds_changed.is_empty() && !vars_changed {
        return DirEventOutcome::Skip; // Nothing relevant changed.
    }

    if ctx.clear {
        clear_terminal();
    }

    // ADR-016: reload vars from disk on every rebuild.
    // Soft-error: vars file may be temporarily absent (AC-W7 / AC-C5).
    let runtime_vars = match build_runtime_vars(RuntimeVarArgs {
        vars: ctx.vars_path.clone(),
        set_vars: ctx.static_set_vars.clone(),
        set_string_vars: ctx.static_set_string_vars.clone(),
    }) {
        Ok(v) => v,
        Err(e) => {
            eprint_error(e);
            // Re-baseline so the idle-tick content backstop does not report the same
            // change again and turn one unreadable vars file into per-tick error spam.
            state.last_mtimes = snapshot_state(&state.tracked_set());
            return DirEventOutcome::Done;
        }
    };

    process_dir_batch(
        &mds_changed,
        vars_changed,
        &ctx.root,
        &ctx.output_base,
        &runtime_vars,
        ctx.quiet,
        state,
    );

    DirEventOutcome::Done
}

/// Perform all one-time startup work for directory-mode watch.
///
/// Loads config, compiles all sources at startup, sets up the watcher +
/// Ctrl+C handler, records the dedup baseline, seeds the mtime snapshot,
/// and builds the context structs needed by the event loop.
///
/// Extracted from `run_watch_dir` to separate the ~186-line setup from the
/// event loop — each half is independently readable and the startup can be
/// tested in isolation (review issue #3 / architecture.md).
#[allow(clippy::too_many_arguments)]
fn dir_watch_startup(
    root: PathBuf,
    out_dir: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    set_string_vars: Vec<(String, String)>,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
) -> Result<DirStartup> {
    // Load config once from the root directory.
    let config = load_config(&root)?;
    // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on macOS).
    // Also rejects a symlinked vars file at startup (build parity — PF-004).
    let vars_path = canonicalize_vars_path(vars).map_err(miette::Error::from)?;
    let static_set_vars = set_vars;
    let static_set_string_vars = set_string_vars;

    // Canonicalize out_dir as absolute so the starts_with(&root) in-root exclusion check
    // is reliable even when cwd contains symlinks (root is already canonical — security #8).
    let abs_out_dir = canonicalize_out_dir(out_dir.as_ref());

    // Compute the OutputBase (Fix 2 — subtree mirroring). Reject `..` at startup.
    let output_base = resolve_output_base(abs_out_dir.as_deref(), &config)?;

    // When the out-dir is inside root, exclude it from collection so the watcher
    // doesn't self-pollute (AC-M7 / edge case 6).
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir(d) if d.starts_with(&root) => Some(d.clone()),
        _ => None,
    };

    if !quiet {
        eprintln!("Watching directory {}", safe_path(&root));
    }

    // Additionally watch the vars file's parent if it is outside root.
    let vars_dir_extra: Option<PathBuf> = vars_path.as_deref().and_then(|vf| {
        let parent = vf.parent()?;
        // Only watch if outside root to avoid redundancy.
        if !parent.starts_with(&root) {
            Some(parent.to_path_buf())
        } else {
            None
        }
    });

    // ── Arm before publish (startup race) ─────────────────────────────────────
    //
    // The recursive root watch is armed BEFORE the tree is walked, before any
    // source is read, and before any output is written, so that every in-root edit
    // from this point on generates an event that is queued on `rx` and drained once
    // the event loop starts. That is the primary detector and the cheapest one.
    //
    // The idle tick's content backstop (#321) is the second detector and covers what
    // arming order cannot: a dependency whose directory is unknowable until the
    // compile that reads it returns. Ordering is still what keeps that backstop cheap
    // — arming first means the backstop almost never has to fire.
    //
    // Arming first also means the watcher observes the startup compile's own
    // reads and writes. Three pre-existing guards absorb that, and all three are
    // still in force:
    //   1. `is_content_event` drops `Access(_)` — every read the compile performs.
    //   2. `handle_fs_event_dir` keeps only paths with a `.mds` extension, so the
    //      `.md`/`.json` outputs this startup writes can never seed a rebuild.
    //      This is the guard that covers in-place output (`OutputBase::NextToSource`),
    //      where outputs land beside their sources inside the watched root.
    //   3. `last_written` content-dedup in `compile_one_source`.
    //
    // When `--out-dir` sits inside the root, arming early widens the window in
    // which the watcher sees its own outputs, so that case is covered twice:
    // `exclude_prefix` keeps the out-dir out of `collect_mds_files`, and
    // `handle_fs_event_dir` drops every event whose path is under an
    // `OutputBase::Dir` before the extension filter even runs. The out-dir is
    // created by the first write *after* the recursive watch is armed, so notify
    // adds it to the watch set — the exclusion is what keeps that harmless.
    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_fs = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx_fs.send(Msg::Fs(res));
        },
        notify::Config::default(),
    )
    .map_err(|e| miette::miette!("failed to initialize file watcher: {e}"))?;

    // Watch the root recursively.
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| {
            miette::miette!(
                "failed to watch directory {}: {e}\n\
                 hint: on Linux you may need to increase fs.inotify.max_user_watches",
                root.display()
            )
        })?;

    // Watch the vars dir if it is outside root — soft warning on failure (mirrors the
    // external-dep-dir convention and the liveness probe's best-effort re-arm semantics;
    // a transient failure must not abort the session, applies ADR-021 / consistency fix).
    if let Some(ref vd) = vars_dir_extra {
        if let Err(e) = watcher.watch(vd, RecursiveMode::NonRecursive) {
            eprint_warning(&format!(
                "warning: failed to watch vars directory {}: {}",
                safe_path(vd),
                safe_inline(&e)
            ));
        }
    }

    // Startup compile: compile all .mds files found under root.
    let all_files = collect_mds_files(&root, MAX_COLLECT_DEPTH, exclude_prefix.as_deref());
    let runtime_vars = build_runtime_vars(RuntimeVarArgs {
        vars: vars_path.clone(),
        set_vars: static_set_vars.clone(),
        set_string_vars: static_set_string_vars.clone(),
    })?;

    // Build the dependency graph and compile all files at startup.
    let mut state = DirWatchState {
        forward_deps: HashMap::new(),
        errored: HashSet::new(),
        known_files: BTreeSet::new(),
        last_written: HashMap::new(),
        external_dep_dirs: BTreeSet::new(),
        last_mtimes: HashMap::new(),
    };

    // Capture the content baseline BEFORE the first read of any source, mirroring
    // single-file mode (#321). What makes the backstop sound is that this snapshot is
    // strictly older than the reads whose results were published: an edit that lands
    // anywhere after this point therefore registers as a difference on the first idle
    // tick, even when no filesystem event announced it.
    //
    // Taking it afterwards instead would be worse than useless — it would record the
    // *post*-edit state as the baseline, so the watcher would believe an output
    // compiled from the pre-edit content was up to date, and hold that belief forever.
    //
    // Keys go through `graph_key`, exactly as `known_files` below does. `tracked_set`
    // is built from those canonical keys, so a raw key here would never match one of
    // them — every source would read as "not in the baseline", i.e. changed, and the
    // first idle tick would recompile the whole tree. `collect_mds_files` walks a root
    // that is already canonical, but a symlinked subdirectory inside it still resolves
    // to something else, and the rest of this function does not assume otherwise.
    let mut pre_mtimes = snapshot_state(
        &all_files
            .iter()
            .map(|p| graph_key(p))
            .collect::<HashSet<_>>(),
    );

    for source in &all_files {
        let key = graph_key(source);
        match compile_to_content(
            source,
            runtime_vars.clone(),
            quiet,
            mds::CompileOptions::default(),
        ) {
            Ok(compiled) => {
                // Collect dep paths (already canonical from mds-core).
                let dep_paths: Vec<PathBuf> =
                    compiled.dependencies.iter().map(PathBuf::from).collect();

                // Track external dep dirs (DD3 — cross-root).
                for dep in &dep_paths {
                    if let Some(parent) = dep.parent() {
                        if !parent.starts_with(&root) {
                            state.external_dep_dirs.insert(parent.to_path_buf());
                        }
                    }
                }

                // Baseline each dependency the instant the compile that discovered it
                // returns, not in one pass after the whole tree is done. A dependency's
                // existence is unknown until it is read, so its baseline can never
                // precede its own read — but it can precede everything else, which
                // shrinks its blind window from "the rest of startup" to the gap
                // between one read and the next statement. `baseline_path` keeps the
                // pre-compile value for any dependency that is also an in-root source:
                // the older of the two is always the safe one.
                for dep in &dep_paths {
                    baseline_path(dep, &mut pre_mtimes);
                }

                state.forward_deps.insert(key.clone(), dep_paths);
                state.known_files.insert(key.clone());

                // Partials (DD2): track in graph but don't emit their own output.
                if !is_partial(source) {
                    // Derive the output path from the compiled kind (intrinsic extension).
                    let ext = compiled.kind.extension();
                    let out = output_path_for(&key, &root, &output_base, ext);
                    if let Err(e) = write_output(Some(out.clone()), &compiled.content, quiet, true)
                    {
                        eprint_error(e);
                    } else {
                        state.last_written.insert(out, compiled.content);
                    }
                }
            }
            Err(e) => {
                eprint_error(e);
                state.forward_deps.insert(key.clone(), vec![]);
                state.errored.insert(key.clone());
                state.known_files.insert(key);
            }
        }
    }

    // All startup outputs are now published — the positive-control injection point.
    startup_race_probe();

    // Watch external dep dirs NonRecursive (DD3). Cross-root dependencies are only
    // discovered by the startup compile, so unlike the root they cannot be armed
    // before the first read; an edit landing in that window produces no event for
    // anyone. What closes it is the baseline captured above, which predates the read
    // — the idle tick's content backstop compares against it and recompiles (#321).
    // `MDS_WATCH_READY` still marks the instant both detectors cover every path, so
    // tests can synchronise on arming rather than on a tick.
    for ext_dir in &state.external_dep_dirs {
        if let Err(e) = watcher.watch(ext_dir, RecursiveMode::NonRecursive) {
            eprint_warning(&format!(
                "warning: failed to watch external dep dir {}: {}",
                safe_path(ext_dir),
                safe_inline(&e)
            ));
        }
    }

    // Build the dedup baseline for any source whose startup compile did not record
    // one (partials are skipped above; a failed write leaves no entry).
    {
        let baseline_vars = build_runtime_vars(RuntimeVarArgs {
            vars: vars_path.clone(),
            set_vars: static_set_vars.clone(),
            set_string_vars: static_set_string_vars.clone(),
        })?;
        for source in &all_files {
            let key = graph_key(source);
            if is_partial(source) {
                continue; // Partials have no output path in last_written.
            }
            match compile_to_content(
                source,
                baseline_vars.clone(),
                true, /* quiet for baseline */
                mds::CompileOptions::default(),
            ) {
                Ok(compiled) => {
                    // Derive output path from the compiled kind (intrinsic extension).
                    let ext = compiled.kind.extension();
                    let out = output_path_for(&key, &root, &output_base, ext);
                    if state.last_written.contains_key(&out) {
                        // Already recorded from startup compile — skip.
                        continue;
                    }
                    state.last_written.insert(out, compiled.content);
                }
                Err(_) => {
                    // Baseline compile failed — leave entry absent so next rebuild always writes.
                }
            }
        }
    }

    // Seed the content backstop's baseline (#321).
    //
    // The merge DIRECTION is load-bearing, exactly as in single-file mode: the
    // pre-compile pairs in `pre_mtimes` overwrite the post-compile ones, because only
    // they predate the reads whose results were published. Inverting the merge — or
    // switching to one that keeps the value already present — would silently restore
    // the lost-save bug while every test still passes.
    let mut last_mtimes = snapshot_state(&state.tracked_set());
    // Witness for the assertion below, taken before the merge consumes `pre_mtimes`.
    // Chosen by `min()` rather than by iteration order: a `HashMap` yields an arbitrary
    // first element, which would make a failure reproduce only sometimes.
    //
    // This can only fire when the two snapshots actually differ for the witness path —
    // i.e. when the file changed during startup, which is the `startup-race-probe`
    // suite's scenario and no other. It is a canary against a future refactor inverting
    // the merge, not a runtime guarantee, and it is deliberately `debug_assert`: the
    // property is a property of the code's shape, not of any input, so a release-time
    // check would guard nothing a debug run does not already catch.
    let witness: Option<(PathBuf, FileStamp)> =
        pre_mtimes.keys().min().map(|p| (p.clone(), pre_mtimes[p]));
    last_mtimes.extend(pre_mtimes);
    if let Some((path, pre)) = witness {
        debug_assert_eq!(
            last_mtimes.get(&path).copied(),
            Some(pre),
            "baseline merge inverted: a pre-compile (mtime, size) must survive the merge \
             with the post-compile snapshot, or an edit made during startup can never \
             register as a difference on the idle tick"
        );
    }
    state.last_mtimes = last_mtimes;

    // Track which external dep dirs were successfully armed during startup (lines above
    // called watcher.watch() for each; treat all existing dirs as armed, missing ones
    // as unarmed so the first tick arms them when they reappear).
    let startup_armed_external: BTreeSet<PathBuf> = state
        .external_dep_dirs
        .iter()
        .filter(|d| d.exists())
        .cloned()
        .collect();

    let liveness = LivenessState {
        first_tick: true,
        root_was_missing: !root.exists(),
        // root_armed = true when root existed at startup (watcher.watch was just called).
        // false when root was missing at startup so the first tick re-arms it on appearance.
        root_armed: root.exists(),
        // Seed with any external dep dirs that don't exist yet so their first
        // appearance is treated as a recovery edge (not a per-tick walk).
        missing_external_dirs: state
            .external_dep_dirs
            .iter()
            .filter(|d| !d.exists())
            .cloned()
            .collect(),
        armed_external_dirs: startup_armed_external,
    };

    let ctx = DirWatchCtx {
        root,
        vars_path,
        static_set_vars,
        static_set_string_vars,
        output_base,
        exclude_prefix,
        vars_dir_extra,
        clear,
        debounce_ms,
        quiet,
    };

    // ── Ctrl+C: install LAST, once the loop that can service it is about to run ──
    //
    // See the matching note in `run_watch_file`. Dir mode is the worse case: an
    // installed handler only enqueues `Msg::Interrupt`, which nothing reads until
    // `run_watch_dir`'s loop starts, and startup here makes TWO full passes over
    // every source in the tree (the compile-and-write pass above, then the
    // dedup-baseline pass). Installing before those passes means Ctrl+C during a
    // large-tree startup is swallowed for their whole duration, and the tool writes
    // the remaining outputs and exits 0. Nothing above needs the handler.
    let tx_ctrlc = tx.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = tx_ctrlc.send(Msg::Interrupt);
    });

    // Root, external dep dirs and the vars dir are all armed — the watch is live.
    emit_ready_marker();

    Ok(DirStartup {
        watcher,
        rx,
        state,
        liveness,
        ctx,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_watch_dir(
    root: PathBuf,
    out_dir: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    set_string_vars: Vec<(String, String)>,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
    tick: Option<Duration>,
) -> Result<()> {
    let DirStartup {
        mut watcher,
        rx,
        mut state,
        mut liveness,
        ctx,
    } = dir_watch_startup(
        root,
        out_dir,
        vars,
        set_vars,
        set_string_vars,
        clear,
        debounce_ms,
        quiet,
    )?;

    // ── Watch loop ────────────────────────────────────────────────────────────
    let mut clock = TickClock::new(tick);
    loop {
        match clock.recv_next(&rx) {
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(None) => {
                // Idle tick — run liveness probe (ADR-021, DD1).
                liveness_probe_dir(&ctx, &mut watcher, &mut liveness, &mut state);
                continue;
            }
            Ok(Some(msg)) => match handle_fs_event_dir(msg, &ctx, &rx, &mut state) {
                DirEventOutcome::Skip | DirEventOutcome::Done => {}
                DirEventOutcome::Stop => {
                    stop_watching(ctx.quiet);
                    return Ok(());
                }
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }

    stop_watching(ctx.quiet);
    Ok(())
}

/// Process a batch of changed `.mds` paths in directory mode.
///
/// Thin dispatcher: delegates to `process_dir_batch_vars_changed` when all
/// known files must be recompiled (vars file changed), or to
/// `process_dir_batch_incremental` for a normal seed-and-propagate pass.
///
/// Called by both the event path and the reconcile path so the same state
/// transitions apply uniformly.
fn process_dir_batch(
    changed: &BTreeSet<PathBuf>,
    vars_changed: bool,
    root: &Path,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) {
    if vars_changed {
        process_dir_batch_vars_changed(root, output_base, runtime_vars, quiet, state);
    } else {
        process_dir_batch_incremental(changed, root, output_base, runtime_vars, quiet, state);
    }

    // Re-baseline the content backstop over the post-batch tracked set (#321).
    //
    // This is the single settle point for `last_mtimes`, and it has to be here rather
    // than at each compile site: the batch is what the idle tick must not report again,
    // and only the batch as a whole knows which paths it covered. Doing it once, over
    // the whole set, also settles the sources a *failed* compile touched (so an
    // unchanged broken file does not re-fire every tick) and drops keys for sources the
    // batch deleted, which `snapshot_state` achieves by replacing the map outright.
    state.last_mtimes = snapshot_state(&state.tracked_set());
}

/// Full recompile of all known files triggered by a vars-file change.
///
/// Recomputes the entire forward-deps graph, external-dep-dirs, and errored set
/// from scratch (prunes stale entries left over from deleted sources).
///
/// Also runs the same deletion cleanup that `process_dir_batch_incremental` does so
/// that a `.mds` deleted in the same debounce window as a vars edit does not orphan its
/// output `.md` or leave stale `last_written` / `forward_deps` / `errored` entries
/// (rust.md / reliability issue #3 fix).
///
/// Uses `compile_one_source` for the shared compile→dedup→write sequence.
fn process_dir_batch_vars_changed(
    root: &Path,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) {
    let all_sources: Vec<PathBuf> = state.known_files.iter().cloned().collect();

    // Determine which known sources no longer exist — their output files must be
    // removed just as in the incremental deletion step (step 5).
    let deleted: Vec<&PathBuf> = all_sources.iter().filter(|p| !p.exists()).collect();
    for del_src in &deleted {
        // Source is gone — we don't know the extension it used. Probe both.
        let base_no_ext = output_base_no_ext(del_src, root, output_base);
        for ext in &["md", "json"] {
            let out = base_no_ext.with_extension(ext);
            if out.exists() {
                match std::fs::remove_file(&out) {
                    Ok(()) => {
                        if !quiet {
                            eprintln!("Removed {} (source deleted)", safe_path(&out));
                        }
                    }
                    Err(e) => {
                        eprint_warning(&format!(
                            "warning: could not remove {}: {}",
                            safe_path(&out),
                            safe_inline(&e)
                        ));
                    }
                }
                // Use the canonical forget() helper so ALL state maps are cleaned up uniformly
                // (forward_deps, errored, known_files, last_written).
                state.forget(del_src, &out);
            }
        }
        // Ensure the source is cleaned from state even if neither sibling existed.
        state.forward_deps.remove(*del_src);
        state.errored.remove(*del_src);
        state.known_files.remove(*del_src);
    }

    // Snapshot the old maps, clear them so compile_one_source's record_success
    // fills fresh copies (ensures stale entries from deleted sources are pruned).
    state.forward_deps.clear();
    state.errored.clear();
    state.external_dep_dirs.clear();

    for src in &all_sources {
        if src.exists() {
            compile_one_source(src, root, output_base, runtime_vars, quiet, state);
        }
    }

    // Prune known_files to currently-existing sources.
    state.known_files = all_sources.into_iter().filter(|p| p.exists()).collect();
}

/// Incremental recompile: compile only transitive importers of the changed seeds.
///
/// Steps:
/// 1. Partition changed paths into `existing` / `deleted`.
/// 2. Compute seeds = existing ∪ deleted ∪ (errored ∩ real-change batch).
/// 3. Compute affected = transitive importers of seeds (ADR-016 snapshot).
/// 4. Compile each affected source that exists and is not an external-only dep.
/// 5. Delete outputs for removed sources.
///
/// Uses `compile_one_source` for the shared compile→dedup→write sequence.
fn process_dir_batch_incremental(
    changed: &BTreeSet<PathBuf>,
    root: &Path,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) {
    // 1. Partition.
    let (existing, deleted): (BTreeSet<PathBuf>, BTreeSet<PathBuf>) =
        changed.iter().cloned().partition(|p| p.exists());

    // 2. Seeds = existing ∪ deleted ∪ errored-if-real-change.
    let has_real_change = !existing.is_empty() || !deleted.is_empty();
    let mut seeds: BTreeSet<PathBuf> = existing.union(&deleted).cloned().collect();
    if has_real_change {
        seeds.extend(state.errored.iter().cloned());
    }

    if seeds.is_empty() {
        return;
    }

    // 3. Affected = seeds ∪ transitive importers (uses start-of-batch graph snapshot).
    let affected = affected_sources(&state.forward_deps, &seeds);

    // 4. Compile each affected source that exists and is not an external-only dep.
    for src in &affected {
        // External-only deps are graph nodes but never emit output (DD3).
        let is_in_root = src.starts_with(root);
        // PF-004: paths inside default-excluded subdirs (hidden dirs, node_modules/)
        // that happen to be under root are treated as external deps — they get a quiet
        // dep-refresh compile but never emit output.  This is the same invariant as the
        // initial walker (which never recurses into those dirs), applied here on the
        // parallel event-processing path so the two paths stay consistent.
        let is_excluded_in_root = is_in_root && is_within_default_excluded_dir(root, src);
        let is_known_external = state
            .external_dep_dirs
            .iter()
            .any(|d| src.parent() == Some(d.as_path()));

        if !is_in_root && !is_known_external {
            // Not in root and not a known external dep — skip.
            continue;
        }

        if !src.exists() {
            // If `src` is in the `deleted` set, it will be cleaned up in step 5.
            // If it is NOT in `deleted` (e.g. it was seeded from `errored` but its
            // delete event was never delivered — issue #7), prune it from `errored`,
            // `forward_deps`, and `known_files` now so it doesn't accumulate as a ghost
            // entry and waste per-batch allocation on every subsequent real-change event.
            if !deleted.contains(src) {
                // Source is gone — probe both .md and .json to clean up either sibling.
                let base_no_ext = output_base_no_ext(src, root, output_base);
                for ext in &["md", "json"] {
                    let out = base_no_ext.with_extension(ext);
                    state.forget(src, &out);
                }
            }
            continue;
        }

        // External deps (out-of-root) AND excluded-in-root paths (node_modules/, .git/,
        // hidden dirs) are graph nodes but never emit their own output (DD3 pattern).
        if !is_in_root || is_excluded_in_root {
            // Compile to refresh deps only; suppress output by using quiet=true.
            match compile_to_content(
                src,
                runtime_vars.clone(),
                true,
                mds::CompileOptions::default(),
            ) {
                Ok(compiled) => {
                    let dep_paths: Vec<PathBuf> =
                        compiled.dependencies.iter().map(PathBuf::from).collect();
                    state.forward_deps.insert(src.clone(), dep_paths);
                    state.errored.remove(src);
                }
                Err(e) => {
                    eprint_error(e);
                    state.errored.insert(src.clone());
                }
            }
            continue;
        }

        // In-root source: full compile→dedup→write via shared helper.
        compile_one_source(src, root, output_base, runtime_vars, quiet, state);
    }

    // 5. Deletions: after importers recompiled, clean up graph + outputs.
    for del_src in &deleted {
        // Source is gone — we don't know the extension it used. Probe both.
        let base_no_ext = output_base_no_ext(del_src, root, output_base);
        for ext in &["md", "json"] {
            let out = base_no_ext.with_extension(ext);
            if out.exists() {
                match std::fs::remove_file(&out) {
                    Ok(()) => {
                        if !quiet {
                            eprintln!("Removed {} (source deleted)", safe_path(&out));
                        }
                    }
                    Err(e) => {
                        eprint_warning(&format!(
                            "warning: could not remove {}: {}",
                            safe_path(&out),
                            safe_inline(&e)
                        ));
                    }
                }
            }
            state.forget(del_src, &out);
        }
        // Ensure source is cleaned even if no outputs were found.
        state.forward_deps.remove(del_src);
        state.errored.remove(del_src);
        state.known_files.remove(del_src);
    }

    // 6. Prune external_dep_dirs to only dirs still referenced by live forward_deps.
    //
    // `external_dep_dirs` is monotonically grown by `record_success` on every compile
    // (issue #2 / reliability.md): when a cross-root @import is edited away, the now-
    // unused dir stays in the set, causing the liveness probe to re-arm it on every tick
    // forever. Recompute from the current `forward_deps` after each batch so abandoned
    // external dirs are unwatched and removed (applies ADR-021 / mirrors the prune
    // already done in `process_dir_batch_vars_changed`).
    let live_ext_dirs: BTreeSet<PathBuf> = state
        .forward_deps
        .values()
        .flatten()
        .filter_map(|dep| dep.parent().map(Path::to_path_buf))
        .filter(|parent| !parent.starts_with(root))
        .collect();
    // Unwatch dirs that are no longer live.
    // (watcher is not in scope here; callers call liveness_probe_dir which re-arms only
    // live dirs — stale dirs simply drop off the set and stop being visited each tick.)
    state.external_dep_dirs = live_ext_dirs;
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // T-U1: dirs_to_watch deduplicates parents.
    #[test]
    fn dirs_to_watch_deduplicates_parents() {
        let entry = PathBuf::from("/project/src/entry.mds");
        let deps = vec![
            "/project/src/a.mds".to_string(),
            "/project/src/b.mds".to_string(), // same parent as entry
            "/project/lib/c.mds".to_string(), // different parent
        ];
        let vars = PathBuf::from("/project/vars.json");
        let dirs = dirs_to_watch(&entry, &deps, Some(&vars));
        // Expect exactly 3 unique parents: /project/src, /project/lib, /project
        assert!(dirs.contains(&PathBuf::from("/project/src")));
        assert!(dirs.contains(&PathBuf::from("/project/lib")));
        assert!(dirs.contains(&PathBuf::from("/project")));
        assert_eq!(dirs.len(), 3, "should deduplicate identical parent dirs");
    }

    // T-U2: files_of_interest contains entry + deps + vars.
    #[test]
    fn files_of_interest_contains_all() {
        let entry = PathBuf::from("/a/entry.mds");
        let deps = vec!["/a/dep1.mds".to_string(), "/b/dep2.mds".to_string()];
        let vars = PathBuf::from("/c/vars.json");
        let foi = files_of_interest(&entry, &deps, Some(&vars));
        assert!(foi.contains(&PathBuf::from("/a/entry.mds")));
        assert!(foi.contains(&PathBuf::from("/a/dep1.mds")));
        assert!(foi.contains(&PathBuf::from("/b/dep2.mds")));
        assert!(foi.contains(&PathBuf::from("/c/vars.json")));
        assert_eq!(foi.len(), 4);
    }

    // T-U3a: is_content_event filters Access events, passes Modify/Create/Remove/Any/Other.
    //
    // Rationale: on Linux inotify emits Access events whenever a file is read.
    // The compile step reads .mds sources, producing Access events that would
    // re-trigger compilation in a feedback loop.  is_content_event drops all
    // Access variants and lets through every kind that represents a real change.
    #[test]
    fn is_content_event_filters_access_passes_others() {
        use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind};

        // All Access variants must return false.
        assert!(!is_content_event(&notify::EventKind::Access(
            AccessKind::Read
        )));
        assert!(!is_content_event(&notify::EventKind::Access(
            AccessKind::Open(AccessMode::Read)
        )));
        assert!(!is_content_event(&notify::EventKind::Access(
            AccessKind::Close(AccessMode::Read)
        )));
        assert!(!is_content_event(&notify::EventKind::Access(
            AccessKind::Close(AccessMode::Write)
        )));
        assert!(!is_content_event(&notify::EventKind::Access(
            AccessKind::Any
        )));
        assert!(!is_content_event(&notify::EventKind::Access(
            AccessKind::Other
        )));

        // Content-changing kinds must return true.
        assert!(is_content_event(&notify::EventKind::Modify(
            ModifyKind::Any
        )));
        assert!(is_content_event(&notify::EventKind::Modify(
            ModifyKind::Data(notify::event::DataChange::Any)
        )));
        assert!(is_content_event(&notify::EventKind::Create(
            CreateKind::File
        )));
        assert!(is_content_event(&notify::EventKind::Remove(
            RemoveKind::File
        )));
        assert!(is_content_event(&notify::EventKind::Any));
        assert!(is_content_event(&notify::EventKind::Other));
    }

    // T-U3: event_is_relevant matches tracked path, rejects sibling.
    #[test]
    fn event_is_relevant_matches_and_rejects() {
        let watched_path = PathBuf::from("/project/src/entry.mds");
        let sibling = PathBuf::from("/project/src/other.mds");
        let mut watched = HashSet::new();
        watched.insert(watched_path.clone());

        // Build a minimal Event with only the paths field set.
        let relevant_event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![watched_path.clone()],
            attrs: Default::default(),
        };
        let irrelevant_event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![sibling],
            attrs: Default::default(),
        };

        assert!(event_is_relevant(&relevant_event, &watched));
        assert!(!event_is_relevant(&irrelevant_event, &watched));
    }

    // T-U4: collect_mds_files recurses and is depth-bounded.
    #[test]
    fn collect_mds_files_recurses_and_depth_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let deep = sub.join("deep");
        std::fs::create_dir(&deep).unwrap();

        std::fs::write(dir.path().join("a.mds"), "Hello!").unwrap();
        std::fs::write(sub.join("b.mds"), "World!").unwrap();
        std::fs::write(deep.join("c.mds"), "Deep!").unwrap();
        std::fs::write(dir.path().join("ignore.txt"), "not mds").unwrap();

        // depth=64 should find all 3.
        let all = collect_mds_files(dir.path(), 64, None);
        let names: Vec<_> = all
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"a.mds"), "should find top-level a.mds");
        assert!(names.contains(&"b.mds"), "should find sub/b.mds");
        assert!(names.contains(&"c.mds"), "should find deep/c.mds");
        assert!(!names.contains(&"ignore.txt"), "should skip non-.mds files");

        // depth=0 should find only top-level files.
        let top_only = collect_mds_files(dir.path(), 0, None);
        assert_eq!(top_only.len(), 1, "depth=0 should return only root files");
    }

    // T-U4b: collect_mds_files respects exclude_prefix.
    #[test]
    fn collect_mds_files_excludes_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(dir.path().join("a.mds"), "A").unwrap();
        std::fs::write(out.join("b.mds"), "B (should be excluded)").unwrap();

        let files = collect_mds_files(dir.path(), 64, Some(&out));
        let names: Vec<_> = files
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"a.mds"), "a.mds should be included");
        assert!(
            !names.contains(&"b.mds"),
            "b.mds inside out/ should be excluded"
        );
    }

    // Fix 2 unit tests — output_path_for / resolve_output_base

    // Mirroring: subtree preserved.
    #[test]
    fn output_path_for_mirrors_subtree() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/a/b/foo.mds");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, PathBuf::from("/out/a/b/foo.md"));
    }

    // No stem collision: two files with the same stem in different subdirs.
    #[test]
    fn output_path_for_no_stem_collision() {
        let root = PathBuf::from("/root");
        let a = PathBuf::from("/root/a/x.mds");
        let b = PathBuf::from("/root/b/x.mds");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        assert_ne!(
            output_path_for(&a, &root, &base, "md"),
            output_path_for(&b, &root, &base, "md"),
            "two files with the same stem in different subdirs must not collide"
        );
        assert_eq!(
            output_path_for(&a, &root, &base, "md"),
            PathBuf::from("/out/a/x.md")
        );
        assert_eq!(
            output_path_for(&b, &root, &base, "md"),
            PathBuf::from("/out/b/x.md")
        );
    }

    // NextToSource: default mode places .md next to source.
    #[test]
    fn output_path_for_next_to_source() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/a/b/foo.mds");
        let result = output_path_for(&source, &root, &OutputBase::NextToSource, "md");
        assert_eq!(result, PathBuf::from("/root/a/b/foo.md"));
    }

    // Compound extension and extensionless stem.
    #[test]
    fn output_path_for_compound_extension() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/foo.bar.mds");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, PathBuf::from("/out/foo.bar.md"));
    }

    // Path-escape guard (AC-M7): source outside root stays inside out-dir.
    #[test]
    fn output_path_for_source_outside_root_stays_contained() {
        let root = PathBuf::from("/root");
        // Source is completely outside root — strip_prefix will fail.
        let source = PathBuf::from("/elsewhere/a/b/foo.mds");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base, "md");
        // Must be inside /out, not escape to /elsewhere.
        assert!(
            result.starts_with("/out"),
            "output must stay inside out-dir even when source is outside root; got {result:?}"
        );
        // Must not join an absolute path that escapes out-dir.
        assert_eq!(result, PathBuf::from("/out/foo.md"));
    }

    // resolve_output_base: --out-dir takes precedence.
    #[test]
    fn resolve_output_base_outdir_wins() {
        let d = PathBuf::from("/my/out");
        let result = resolve_output_base(Some(&d), &None).unwrap();
        assert!(matches!(result, OutputBase::Dir(p) if p == d));
    }

    // resolve_output_base: mds.json config used when no --out-dir.
    #[test]
    fn resolve_output_base_config_used_when_no_outdir() {
        use crate::build::{BuildConfig, MdsConfig};
        let config = Some((
            MdsConfig {
                build: BuildConfig {
                    output_dir: Some("dist".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            PathBuf::from("/project"),
        ));
        let result = resolve_output_base(None, &config).unwrap();
        assert!(
            matches!(result, OutputBase::Dir(ref p) if p == &PathBuf::from("/project/dist")),
            "expected Dir(/project/dist), got {result:?}"
        );
    }

    // resolve_output_base: `..` in output_dir rejected at startup.
    #[test]
    fn resolve_output_base_rejects_dotdot() {
        use crate::build::{BuildConfig, MdsConfig};
        let config = Some((
            MdsConfig {
                build: BuildConfig {
                    output_dir: Some("../bad".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            PathBuf::from("/project"),
        ));
        let result = resolve_output_base(None, &config);
        assert!(
            result.is_err(),
            "resolve_output_base must reject output_dir with '..' components"
        );
    }

    // resolve_output_base: default → NextToSource.
    #[test]
    fn resolve_output_base_default_next_to_source() {
        let result = resolve_output_base(None, &None).unwrap();
        assert!(matches!(result, OutputBase::NextToSource));
    }

    // is_partial: _ prefix detection.
    #[test]
    fn is_partial_detects_underscore_prefix() {
        assert!(is_partial(Path::new("/some/dir/_partial.mds")));
        assert!(!is_partial(Path::new("/some/dir/normal.mds")));
        assert!(!is_partial(Path::new("/some/dir/a_b.mds")));
    }

    // affected_sources: chain A→B→C, edit C updates A, B, C.
    #[test]
    fn affected_sources_chain() {
        let a = PathBuf::from("/root/a.mds");
        let b = PathBuf::from("/root/b.mds");
        let c = PathBuf::from("/root/c.mds");

        let mut forward_deps = HashMap::new();
        // A imports B, B imports C.
        forward_deps.insert(a.clone(), vec![b.clone()]);
        forward_deps.insert(b.clone(), vec![c.clone()]);
        forward_deps.insert(c.clone(), vec![]);

        let mut seeds = BTreeSet::new();
        seeds.insert(c.clone());

        let affected = affected_sources(&forward_deps, &seeds);
        let affected_set: HashSet<PathBuf> = affected.into_iter().collect();

        assert!(affected_set.contains(&a), "A should be affected");
        assert!(affected_set.contains(&b), "B should be affected");
        assert!(affected_set.contains(&c), "C (seed) should be in result");
    }

    // affected_sources: shared partial → multiple importers.
    #[test]
    fn affected_sources_shared_partial() {
        let partial = PathBuf::from("/root/_p.mds");
        let a = PathBuf::from("/root/a.mds");
        let b = PathBuf::from("/root/b.mds");

        let mut forward_deps = HashMap::new();
        forward_deps.insert(a.clone(), vec![partial.clone()]);
        forward_deps.insert(b.clone(), vec![partial.clone()]);
        forward_deps.insert(partial.clone(), vec![]);

        let mut seeds = BTreeSet::new();
        seeds.insert(partial.clone());

        let affected = affected_sources(&forward_deps, &seeds);
        let affected_set: HashSet<PathBuf> = affected.into_iter().collect();

        assert!(affected_set.contains(&a));
        assert!(affected_set.contains(&b));
        assert!(affected_set.contains(&partial));
    }

    // affected_sources: cycle terminates (bounded).
    #[test]
    fn affected_sources_cycle_terminates() {
        let a = PathBuf::from("/root/a.mds");
        let b = PathBuf::from("/root/b.mds");

        let mut forward_deps = HashMap::new();
        // A → B → A (cycle)
        forward_deps.insert(a.clone(), vec![b.clone()]);
        forward_deps.insert(b.clone(), vec![a.clone()]);

        let mut seeds = BTreeSet::new();
        seeds.insert(a.clone());

        // Must terminate and return both.
        let affected = affected_sources(&forward_deps, &seeds);
        let affected_set: HashSet<PathBuf> = affected.into_iter().collect();
        assert!(affected_set.contains(&a));
        assert!(affected_set.contains(&b));
    }

    // affected_sources: leaf-only (seed not in graph → just seed returned).
    #[test]
    fn affected_sources_seed_not_in_graph() {
        let forward_deps: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let lone = PathBuf::from("/root/lone.mds");
        let mut seeds = BTreeSet::new();
        seeds.insert(lone.clone());
        let affected = affected_sources(&forward_deps, &seeds);
        assert_eq!(affected, vec![lone]);
    }

    // affected_sources: dual-role node visited once (AC-R6).
    #[test]
    fn affected_sources_dual_role_visited_once() {
        // B is both an importer of C and imported by A.
        let a = PathBuf::from("/root/a.mds");
        let b = PathBuf::from("/root/b.mds");
        let c = PathBuf::from("/root/c.mds");

        let mut forward_deps = HashMap::new();
        forward_deps.insert(a.clone(), vec![b.clone()]);
        forward_deps.insert(b.clone(), vec![c.clone()]);
        forward_deps.insert(c.clone(), vec![]);

        let mut seeds = BTreeSet::new();
        seeds.insert(c.clone());

        let affected = affected_sources(&forward_deps, &seeds);
        // B should appear exactly once.
        let b_count = affected.iter().filter(|p| *p == &b).count();
        assert_eq!(b_count, 1, "dual-role node B should appear exactly once");
    }

    // external_recovery_decision: a dir that STAYS missing across ticks does NOT
    // trigger recovery (ADR-021 / AC-P1 — no per-tick full-tree walk).
    #[test]
    fn external_recovery_missing_stays_missing_no_recovery() {
        let gone = PathBuf::from("/elsewhere/shared");
        let prev_missing: BTreeSet<PathBuf> = std::iter::once(gone.clone()).collect();
        // Still missing this tick.
        let statuses = vec![(gone.clone(), false, false)];
        let (recovery, now_missing) = external_recovery_decision(&prev_missing, &statuses);
        assert!(
            !recovery,
            "a permanently-missing external dir must NOT trigger a reconcile"
        );
        assert!(
            now_missing.contains(&gone),
            "still-missing dir stays tracked"
        );
    }

    // external_recovery_decision: a previously-missing dir that REAPPEARS triggers
    // recovery (vanish→reappear edge).
    #[test]
    fn external_recovery_reappear_triggers_recovery() {
        let dir = PathBuf::from("/elsewhere/shared");
        let prev_missing: BTreeSet<PathBuf> = std::iter::once(dir.clone()).collect();
        // Now exists and re-armed OK.
        let statuses = vec![(dir.clone(), true, true)];
        let (recovery, now_missing) = external_recovery_decision(&prev_missing, &statuses);
        assert!(
            recovery,
            "a reappeared external dir must trigger a reconcile"
        );
        assert!(
            now_missing.is_empty(),
            "reappeared dir no longer tracked as missing"
        );
    }

    // external_recovery_decision: re-arming an EXISTING dir failed → genuine watch
    // loss → recovery.
    #[test]
    fn external_recovery_rearm_failure_triggers_recovery() {
        let dir = PathBuf::from("/elsewhere/shared");
        let prev_missing = BTreeSet::new();
        // Exists but re-arm failed.
        let statuses = vec![(dir.clone(), true, false)];
        let (recovery, now_missing) = external_recovery_decision(&prev_missing, &statuses);
        assert!(
            recovery,
            "a failed re-arm of an existing dir must trigger a reconcile"
        );
        assert!(now_missing.is_empty());
    }

    // external_recovery_decision: all dirs present and stable → no recovery, no walk.
    #[test]
    fn external_recovery_stable_no_recovery() {
        let a = PathBuf::from("/ext/a");
        let b = PathBuf::from("/ext/b");
        let prev_missing = BTreeSet::new();
        let statuses = vec![(a, true, true), (b, true, true)];
        let (recovery, now_missing) = external_recovery_decision(&prev_missing, &statuses);
        assert!(
            !recovery,
            "stable existing external dirs must not trigger a reconcile"
        );
        assert!(now_missing.is_empty());
    }

    // snapshot_state / state_differs.
    #[test]
    fn snapshot_and_diff_detect_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("test.mds");
        std::fs::write(&f, "v1").unwrap();

        let paths: HashSet<PathBuf> = std::iter::once(f.clone()).collect();
        let snap = snapshot_state(&paths);
        // No change yet.
        assert!(!state_differs(&paths, &snap));

        // Modify the file.
        std::fs::write(&f, "v2").unwrap();
        assert!(state_differs(&paths, &snap), "should detect content change");
    }

    // snapshot_state: disappearing file detected.
    #[test]
    fn snapshot_detects_disappearing_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("gone.mds");
        std::fs::write(&f, "initial").unwrap();

        let paths: HashSet<PathBuf> = std::iter::once(f.clone()).collect();
        let snap = snapshot_state(&paths);
        // File existed in snap.
        std::fs::remove_file(&f).unwrap();
        assert!(
            state_differs(&paths, &snap),
            "should detect deleted file as changed"
        );
    }

    // AC-C: clamp_poll_interval contract — 0 disables liveness probe; nonzero values ≥50ms
    // are passed through; values below 50ms are clamped up to the floor.
    #[test]
    fn clamp_poll_interval_zero_disables_probe() {
        assert_eq!(
            clamp_poll_interval(0),
            None,
            "poll_interval=0 must disable the liveness probe (blocking recv)"
        );
    }

    #[test]
    fn clamp_poll_interval_one_clamped_to_50ms() {
        assert_eq!(
            clamp_poll_interval(1),
            Some(Duration::from_millis(50)),
            "poll_interval=1 must be clamped to the 50ms floor"
        );
    }

    #[test]
    fn clamp_poll_interval_exactly_50_unchanged() {
        assert_eq!(
            clamp_poll_interval(50),
            Some(Duration::from_millis(50)),
            "poll_interval=50 (at the floor) must pass through unchanged"
        );
    }

    #[test]
    fn clamp_poll_interval_above_floor_unchanged() {
        assert_eq!(
            clamp_poll_interval(1000),
            Some(Duration::from_millis(1000)),
            "poll_interval=1000 (above floor) must pass through unchanged"
        );
    }

    #[test]
    fn clamp_poll_interval_75ms_unchanged() {
        assert_eq!(
            clamp_poll_interval(75),
            Some(Duration::from_millis(75)),
            "poll_interval=75 (above floor) must pass through unchanged"
        );
    }

    // ── TickClock (#319) ─────────────────────────────────────────────────────
    //
    // The probe-starvation defect. These assert the two properties that make the idle
    // tick a usable backstop rather than a best-effort one: it fires under load, and
    // it does not fire more often than its interval.

    /// A tick fires when the channel stays silent.
    #[test]
    fn tick_clock_fires_when_idle() {
        let (_tx, rx) = mpsc::channel::<Msg>();
        let mut clock = TickClock::new(Some(Duration::from_millis(50)));
        assert!(
            matches!(clock.recv_next(&rx), Ok(None)),
            "an idle channel must produce a tick"
        );
    }

    /// A tick fires even when messages arrive faster than the interval (#319).
    ///
    /// This is the regression test for the starvation bug: the previous
    /// implementation handed `recv_timeout` a fresh interval per message, so a sender
    /// running at 20× the tick rate postponed the probe forever. Fifty messages at 5ms
    /// spans 250ms — five full 50ms intervals — so a correct clock must yield at least
    /// one tick before they are exhausted.
    #[test]
    fn tick_clock_fires_under_message_flood() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let sender = std::thread::spawn(move || {
            // Bounded: exactly 50 sends, then the thread ends.
            for _ in 0..50 {
                if tx.send(Msg::Interrupt).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let mut clock = TickClock::new(Some(Duration::from_millis(50)));
        let mut ticks = 0usize;
        // Bounded: at most 200 receives regardless of what the sender does.
        for _ in 0..200 {
            match clock.recv_next(&rx) {
                Ok(None) => {
                    ticks += 1;
                    break;
                }
                Ok(Some(_)) => {}
                Err(_) => break,
            }
        }
        sender.join().expect("sender thread panicked");
        assert!(
            ticks > 0,
            "the idle tick must fire while messages are arriving 10x faster than the \
             poll interval; a starvable tick makes the content backstop unreachable"
        );
    }

    /// Two ticks are never closer together than one interval, however loaded the loop.
    ///
    /// The counterpart to the test above: making the tick non-starvable must not make
    /// it free-running. A deadline that advanced from the *previous* deadline rather
    /// than from the moment the tick was observed would fire a catch-up burst after
    /// any slow probe.
    #[test]
    fn tick_clock_rate_limits_consecutive_ticks() {
        let (_tx, rx) = mpsc::channel::<Msg>();
        let interval = Duration::from_millis(50);
        let mut clock = TickClock::new(Some(interval));
        assert!(matches!(clock.recv_next(&rx), Ok(None)), "first tick");

        // Simulate a probe that overran its own interval.
        std::thread::sleep(Duration::from_millis(120));
        let t0 = Instant::now();
        assert!(matches!(clock.recv_next(&rx), Ok(None)), "overdue tick");
        let t1 = Instant::now();
        assert!(
            matches!(clock.recv_next(&rx), Ok(None)),
            "tick after the overdue one"
        );
        assert!(
            t1.duration_since(t0) < Duration::from_millis(20),
            "an overdue tick must fire immediately, not wait another interval"
        );
        assert!(
            t1.elapsed() >= Duration::from_millis(40),
            "the tick following an overdue one must wait a full interval, not fire a \
             catch-up burst; got {:?}",
            t1.elapsed()
        );
    }

    /// `--poll-interval 0` disables the tick entirely: `recv_next` blocks for a message.
    #[test]
    fn tick_clock_without_interval_never_ticks() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut clock = TickClock::new(None);
        tx.send(Msg::Interrupt).expect("send failed");
        assert!(
            matches!(clock.recv_next(&rx), Ok(Some(Msg::Interrupt))),
            "with no poll interval the clock must deliver the message, never a tick"
        );
        drop(tx);
        assert!(
            matches!(
                clock.recv_next(&rx),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ),
            "a closed channel must report Disconnected rather than tick forever"
        );
    }

    // ── Content backstop domain (#321) ───────────────────────────────────────

    /// `tracked_set` covers cross-root dependencies, which `known_files` never can.
    ///
    /// `known_files` holds exactly what `collect_mds_files(root)` returns, so a
    /// dependency outside the root is absent from it by construction. Baselining only
    /// that set is what left `last_mtimes` write-only in directory mode.
    #[test]
    fn tracked_set_includes_cross_root_dependencies() {
        let root = PathBuf::from("/w/root");
        let importer = root.join("importer.mds");
        let external = PathBuf::from("/w/shared/_x.mds");

        let mut state = DirWatchState {
            forward_deps: HashMap::new(),
            errored: HashSet::new(),
            known_files: BTreeSet::new(),
            last_written: HashMap::new(),
            external_dep_dirs: BTreeSet::new(),
            last_mtimes: HashMap::new(),
        };
        state.known_files.insert(importer.clone());
        state
            .forward_deps
            .insert(importer.clone(), vec![external.clone()]);

        let tracked = state.tracked_set();
        assert!(
            tracked.contains(&importer),
            "in-root source must be tracked"
        );
        assert!(
            tracked.contains(&external),
            "a cross-root dependency must be tracked; it is exactly the path no \
             collect_mds_files(root) walk can report"
        );
    }

    /// A failed compile keeps the dep set the last successful one recorded (#321).
    ///
    /// Clearing it dropped the external directory the deps lived in, after which every
    /// further event for that directory was filtered out as unknown — one compile
    /// against a half-written file blinded the watcher for the session.
    #[test]
    fn record_error_preserves_last_known_deps() {
        let root = PathBuf::from("/w/root");
        let importer = root.join("importer.mds");
        let external = PathBuf::from("/w/shared/_x.mds");

        let mut state = DirWatchState {
            forward_deps: HashMap::new(),
            errored: HashSet::new(),
            known_files: BTreeSet::new(),
            last_written: HashMap::new(),
            external_dep_dirs: BTreeSet::new(),
            last_mtimes: HashMap::new(),
        };
        state.record_success(&importer, vec![external.clone()], &root, None, None);
        assert!(state.external_dep_dirs.contains(Path::new("/w/shared")));

        state.record_error(&importer);

        assert!(state.errored.contains(&importer), "error must be recorded");
        assert_eq!(
            state.forward_deps.get(&importer),
            Some(&vec![external.clone()]),
            "a failed compile must keep the last known dep set, or the external dep \
             dir is pruned and its events are filtered out from then on"
        );
        assert!(
            state.tracked_set().contains(&external),
            "the backstop must still cover a dependency whose importer is broken — \
             that is precisely when it needs to notice the dependency being fixed"
        );
    }

    /// A source that has never compiled successfully gets an empty dep set, not a panic.
    #[test]
    fn record_error_on_unknown_source_inserts_empty_deps() {
        let mut state = DirWatchState {
            forward_deps: HashMap::new(),
            errored: HashSet::new(),
            known_files: BTreeSet::new(),
            last_written: HashMap::new(),
            external_dep_dirs: BTreeSet::new(),
            last_mtimes: HashMap::new(),
        };
        let src = PathBuf::from("/w/root/broken.mds");
        state.record_error(&src);
        assert_eq!(state.forward_deps.get(&src), Some(&vec![]));
    }

    /// `baseline_path` keeps the older entry — the merge direction the backstop needs.
    #[test]
    fn baseline_path_keeps_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.mds");
        std::fs::write(&f, "one").unwrap();

        let mut snap = HashMap::new();
        baseline_path(&f, &mut snap);
        let first = snap.get(&f).copied();

        std::fs::write(&f, "a much longer second revision").unwrap();
        baseline_path(&f, &mut snap);

        assert_eq!(
            snap.get(&f).copied(),
            first,
            "baseline_path must not overwrite an existing entry: only the older pair \
             predates the read whose output was published"
        );
        assert!(
            path_state_differs(&f, &snap),
            "the retained older baseline must therefore report the file as changed"
        );
    }

    /// A path absent from the baseline counts as changed.
    #[test]
    fn path_state_differs_reports_unknown_path() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.mds");
        std::fs::write(&f, "one").unwrap();
        let snap = HashMap::new();
        assert!(
            path_state_differs(&f, &snap),
            "a path the baseline has never seen cannot be claimed as accounted for"
        );
    }

    // T-U5 (renamed): output_path_for does NOT create directories.
    #[test]
    fn output_path_for_no_create() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let new_subdir = dir.path().join("new_out");
        assert!(!new_subdir.exists(), "precondition: subdir does not exist");

        let source = root.join("template.mds");
        let base = OutputBase::Dir(new_subdir.clone());
        let result = output_path_for(&source, &root, &base, "md");
        assert_eq!(result, new_subdir.join("template.md"));
        assert!(
            !new_subdir.exists(),
            "output_path_for must not create directories"
        );
    }

    // T-U6: compile_and_write returns deps for an importing template.
    //
    // Uses @define/@export/@import/@include pattern to create a verifiable
    // transitive dependency.
    #[test]
    fn compile_and_write_returns_deps_for_importing_template() {
        let dir = tempfile::tempdir().unwrap();
        // Create a helper module that exports a function.
        let helper = dir.path().join("helper.mds");
        std::fs::write(
            &helper,
            "@define greet(name):\nHello {name}!\n@end\n\n@export greet\n",
        )
        .unwrap();
        // Create an entry that imports and includes the helper.
        let entry = dir.path().join("entry.mds");
        std::fs::write(
            &entry,
            "@import \"./helper.mds\" as h\n\n{h.greet(\"World\")}\n",
        )
        .unwrap();
        // Use -o <out> style to direct output to a specific path.
        let out = dir.path().join("entry.md");
        let out_str = out.display().to_string();
        let (_written_path, deps, _content) = compile_and_write(
            &entry,
            &Some(out_str),
            &None,
            &None,
            None,
            true,
            mds::CompileOptions::default(),
        )
        .unwrap();
        // The entry's compile output should list helper as a dependency.
        assert!(out.exists(), "output file should be created");
        assert!(
            !deps.is_empty(),
            "compile_and_write should return the imported helper as a dep"
        );
        let dep_names: Vec<_> = deps
            .iter()
            .filter_map(|d| {
                PathBuf::from(d)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
            })
            .collect();
        assert!(
            dep_names.iter().any(|n| n == "helper.mds"),
            "deps should contain helper.mds, got: {dep_names:?}"
        );
    }
}
