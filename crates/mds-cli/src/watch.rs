//! Watch subcommand — file watcher with auto-recompile on save (issue #57).
//!
//! # Design overview
//!
//! Two modes share a single watch loop:
//!
//! - **Single-file mode**: watches the entry file and all its transitive imports.
//!   On each rebuild the dependency set is recomputed from fresh compilation output
//!   (the freshness rule under "Key invariants": never trust a stale dep set).
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
//! The tick is scheduled against an absolute deadline (`TickSchedule`, driven by
//! `TickClock`), so a stream of filesystem events cannot postpone the backstop
//! indefinitely (#319).
//!
//! # Coalescing
//!
//! `--debounce` is a **quiet period**: the first relevant event opens a window and
//! every further content event restarts it, so a save burst longer than the window is
//! still one rebuild. The window is itself bounded — by an absolute cap of
//! `max(10 x window, 1s)` measured from the first event, and by 10 000 drained
//! messages — because the loop does not consult the idle tick while a window is open,
//! so an unbounded window would starve the content backstop as well as the rebuild
//! (#379).
//!
//! # Empty files
//!
//! An editor that saves by truncating and then writing leaves the file empty for a
//! moment, at any `--debounce`. A rebuild that finds a watched file — the entry, a
//! dependency, the `--vars` file, any source of a watched directory — empty where the
//! baseline saw bytes is held back (`EmptyHold`): nothing is compiled, written or printed
//! until a rebuild finds it written, or until a deadline one second after the first
//! rebuild that found it emptied, when the files are compiled as they are. The deadline
//! never moves, and the loop's driver waits for it like the tick, so neither a stream of
//! events nor `--poll-interval 0` can postpone it (#380).
//!
//! # Key invariants
//!
//! - All content output → stdout ONLY when output resolves to stdout.
//! - All status / warnings / errors → stderr (pipe-safe).
//! - `--quiet` suppresses status + warnings but NOT compile errors.
//! - Exit 0 on clean Ctrl+C; non-zero only on startup failure.
//! - Streams (#157): with `-o -`, stdout's reader going away ends the session with
//!   `Stopped watching (stdout closed).` and verdict 0, since nothing can receive its
//!   output any more; a closed stderr only loses the status lines. Once live
//!   ([`live::run_session`]), a rebuild's output failure is reported where it can be and never
//!   changes the exit code; a session that ends at startup exits as a batch run does.
//!   Every status line goes through the CLI's stderr writer, which never panics.
//! - Compile errors during watching never terminate the watcher.
//! - All loops have fixed upper bounds (reconcile rule / reliability.md): the idle tick
//!   against an absolute deadline, the debounce window against an absolute cap
//!   (window <= cap) and a message bound (<= 10 000 per window), and a rebuild held
//!   while a watched file is empty against an absolute deadline (#380).
//! - All `.mds` reads go through `compile_to_content` (PF-004).
//! - **Freshness rule** (design decision of 2026-06; kept in git history as legacy
//!   decision 016 in `88ddbcc~1:.devflow/decisions/decisions.md`): the dependency set
//!   and the `--vars` file are re-derived from fresh compile output / from disk on
//!   every rebuild — never served from a cached snapshot.
//! - **Reconcile rule** (legacy decision 021, same file): the idle tick only re-arms
//!   watches cheaply; a full directory rescan happens only on watch loss/recovery, so
//!   idle cost is O(1) in tree size.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::ControlFlow;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use miette::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use mds::MdsError;

use crate::build::{
    admit_output, auto_detect_mds_file, build_runtime_vars, compile_inputs, compile_to_content,
    resolve_dir_as_created, resolve_output_path_for_kind, source_inputs, write_output,
    CompileOutput, EntryPaths, OutputKind, ProjectConfig, RuntimeVarArgs,
};
use crate::output::{
    collect_mds_files, eprint_error, eprint_warning, is_partial, is_within_default_excluded_dir,
    notify_cause, output_path_for, resolve_output_base, safe_inline, safe_path, stdout_failure,
    write_stdout, OutputBase, Panicked, RootPaths, StdoutOutcome, WriteTarget,
};
use crate::write::{
    remove_proven, write_over_own, DirIdentity, Durability, Inputs, NotCreated, NotRemoved,
    Parents, Removal,
};

// ── Public args struct ────────────────────────────────────────────────────────

pub(crate) struct WatchArgs {
    pub(crate) input: Option<PathBuf>,
    /// `-o/--output` as given; `reject_forbidden_output_flags` turns it into text.
    pub(crate) output: Option<std::ffi::OsString>,
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
/// to cover `entry`, all `deps` (graph keys, [`graph_keys`]), and an optional
/// `vars_file`.
///
/// Watching parent directories rather than file inodes is necessary because editors
/// perform atomic save via rename: a file-inode watch is silently orphaned after the
/// swap, but a directory watch survives.
pub(crate) fn dirs_to_watch(
    entry: &Path,
    deps: &[PathBuf],
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
        push_parent(dep, &mut dirs);
    }

    if let Some(vf) = vars_file {
        push_parent(vf, &mut dirs);
    }

    dirs
}

/// Build the set of paths that are "of interest" for a single-file watch:
/// the entry itself, all dependency paths (graph keys), and the vars file if given.
pub(crate) fn files_of_interest(
    entry: &Path,
    deps: &[PathBuf],
    vars_file: Option<&Path>,
) -> HashSet<PathBuf> {
    let mut set = HashSet::new();
    set.insert(entry.to_path_buf());
    for dep in deps {
        set.insert(dep.clone());
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

/// The graph keys ([`graph_key`]) of the dependencies a compile reported, in the
/// canonical form notify reports event paths in — which on Windows keeps the `\\?\`
/// prefix the compiler's own list drops (#409).
///
/// Each stays a path (#390): the text of a path is lossy for a name that is not UTF-8,
/// so two dependencies whose names differ only there would share one key.
pub(crate) fn graph_keys<P: AsRef<Path>>(paths: &[P]) -> Vec<PathBuf> {
    paths.iter().map(|p| graph_key(p.as_ref())).collect()
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
/// Edge-triggered (reconcile rule / AC-P1): a missing external dir forces a reconcile
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
/// it later; the vars file is reloaded on every rebuild — freshness rule — so a duplicate
/// key introduced after startup is caught on the next rebuild, #326).
///
/// A path carrying a forbidden path character (#265) is refused first, before the
/// `exists` probe — so before its directory is ever watched — with the message
/// `check_symlink` gives an existing one (`mds::io`).
///
/// Only the symlink refusal (`ImportError`) is reworded for the `--vars` flag, as every
/// command words it ([`crate::build::vars_file_error`], #157); every other `check_symlink`
/// error — a forbidden character in the resolved path, or a file removed since the
/// `exists` probe — keeps its own message and code.
pub(crate) fn canonicalize_vars_path(vars: Option<PathBuf>) -> Result<Option<PathBuf>, MdsError> {
    if let Some(p) = &vars {
        crate::output::reject_forbidden_output_path("path", p.as_os_str())?;
    }
    match vars {
        Some(p) if p.exists() => mds::NativeFs::check_symlink(&p)
            .map(Some)
            .map_err(|e| crate::build::vars_file_error(&p, e)),
        other => Ok(other),
    }
}

/// Write the ANSI clear-screen sequence to stderr if stderr is a TTY.
///
/// Uses `\x1b[2J\x1b[3J\x1b[H` (erase screen + scrollback + home).
pub(crate) fn clear_terminal() {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        crate::output::ewrite!("\x1b[2J\x1b[3J\x1b[H");
    }
}

/// Update the watcher to reflect a new set of directories.
///
/// Unwatch directories no longer needed, watch newly required ones; a directory that
/// cannot be watched is named through [`shown_watched_dir`] with `root` and `vars`.
/// Returns the updated set of currently-watched directories.
pub(crate) fn resync_watches(
    watcher: &mut RecommendedWatcher,
    current_dirs: &BTreeSet<PathBuf>,
    new_dirs: &BTreeSet<PathBuf>,
    root: RootPaths<'_>,
    vars: Option<RootPaths<'_>>,
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
                safe_path(&shown_watched_dir(dir, root, vars)),
                safe_inline(notify_cause(&e))
            ));
        } else {
            result.insert(dir.clone());
        }
    }
    result
}

// ── Small shared helpers ──────────────────────────────────────────────────────

/// Why a watch session stops, which its last status line names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopReason {
    /// Ctrl+C — or the event channel closing, which ends the loop the same way.
    Interrupted,
    /// `-o -` and stdout's reader is gone: nothing can receive the output any more
    /// (#157).
    StdoutClosed,
}

/// Emit the session's last status line to stderr (unless quiet): `Stopped watching.`, or
/// `Stopped watching (stdout closed).` when `-o -` lost its reader.
///
/// Called at every point where a watch session ends without an error: Ctrl+C in the watch
/// loop both modes share, and a gone stdout reader in file mode, at startup or on a
/// rebuild. The verdict is 0 — the exit code of a live session ([`live::run_session`]); a
/// session that stops at startup exits as a batch run with that verdict does (#157).
fn stop_watching(quiet: bool, why: StopReason) {
    if quiet {
        return;
    }
    match why {
        StopReason::Interrupted => crate::output::ewriteln!("Stopped watching."),
        StopReason::StdoutClosed => crate::output::ewriteln!("Stopped watching (stdout closed)."),
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
/// written marker. The temporary file is created new ([`create_ready_marker`]), so a
/// symlink planted at `<marker>.tmp` never redirects the write (#390). Failures are
/// ignored: this is a test affordance, and a watcher that cannot create the file must
/// still watch.
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
    if create_ready_marker(&tmp).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Create `tmp` new, holding [`READY_MARKER`]: never through an entry already at it,
/// which a write that opens the path would follow if it were a symlink (#390). An entry
/// there — a leftover, or a planted link — is removed (the entry itself, never what a link
/// points to) and the file created once more; another entry in between ends the attempt.
///
/// A raw write, not `atomic_write_file`'s, by design (#227): the rename that follows it
/// is the atomic step. Allow-listed in tests/write_funnel.rs.
fn create_ready_marker(tmp: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(tmp)
    };
    let mut file = match create() {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(tmp)?;
            create()?
        }
        created => created?,
    };
    file.write_all(READY_MARKER.as_bytes())
}

// ── Empty-file hold (#380) ────────────────────────────────────────────────────

/// How long, at most, a rebuild is held while a watched file is empty (#380), counted
/// from the rebuild that first found it emptied and never extended.
///
/// An editor that saves by truncating the file and then writing it leaves it empty in
/// between, and a rebuild in that moment would publish an empty output nobody wrote. A
/// file still empty this long after is taken to be empty on purpose, and is compiled as it
/// is. One second is far past any editor's truncate-to-write gap and short enough that a
/// file really emptied is published promptly. It is its own constant, not the debounce
/// cap's floor: the two answer different questions and may change apart.
const EMPTY_HOLD_DEADLINE: Duration = Duration::from_secs(1);

/// Whether a watched file went from non-empty to empty: the baseline saw bytes in it
/// (`before`), and it has none `now` (#380). A file the baseline never saw, saw missing or
/// saw empty — one empty from the start, or one compiled since it was emptied — has not,
/// and neither has a file that is gone.
fn went_empty(before: Option<&FileStamp>, now: &FileStamp) -> bool {
    matches!((before.and_then(|stamp| stamp.1), now.1), (Some(had), Some(0)) if had > 0)
}

/// `path`'s `(mtime, size)` now; `(None, None)` when it cannot be read.
fn stamp_now(path: &Path) -> FileStamp {
    match std::fs::metadata(path) {
        Ok(m) => (m.modified().ok(), Some(m.len())),
        Err(_) => (None, None),
    }
}

/// Whether any of `paths` went from non-empty to empty since `baseline` was taken
/// ([`went_empty`]): one `stat` per path, stopping at the first that did.
fn any_went_empty<'a>(paths: impl IntoIterator<Item = &'a PathBuf>, baseline: &StampMap) -> bool {
    paths
        .into_iter()
        .any(|path| went_empty(baseline.get(path), &stamp_now(path)))
}

/// Which of `paths` went from non-empty to empty since `baseline` was taken
/// ([`went_empty`]): one `stat` per path.
fn emptied_paths<'a>(
    paths: impl IntoIterator<Item = &'a PathBuf>,
    baseline: &StampMap,
) -> BTreeSet<PathBuf> {
    paths
        .into_iter()
        .filter(|path| went_empty(baseline.get(*path), &stamp_now(path)))
        .cloned()
        .collect()
}

/// The baseline a batch leaves while a rebuild is held (#380): `fresh`, except that a file
/// gone empty since `before` keeps the stamp that saw its bytes, so the next look finds it
/// emptied still. A file `before` never saw takes its fresh stamp.
fn baseline_keeping_emptied(before: &StampMap, mut fresh: StampMap) -> StampMap {
    for (path, now) in &mut fresh {
        if let Some(old) = before.get(path).filter(|old| went_empty(Some(*old), now)) {
            *now = *old;
        }
    }
    fresh
}

/// The instant a rebuild decides its hold at: `now`, or `due` — the deadline of the hold
/// that runs it — if `now` is earlier, so that deadline ends the hold whatever the clock
/// does between the wake and the rebuild (#380).
fn not_before(now: Instant, due: Option<Instant>) -> Instant {
    due.map_or(now, |due| now.max(due))
}

/// A rebuild held while a watched file is empty, and the **absolute** deadline at which it
/// runs anyway (#380).
///
/// The deadline is set by the rebuild that first finds a watched file emptied, and never
/// moves: a later event, another truncation of the same file and an idle tick each find
/// the hold running and leave it as it is. A deadline that moved with them would let a
/// stream of events postpone the rebuild forever — the starvation #319 removed from the
/// idle tick. The hold ends at the first rebuild that finds no watched file emptied, the
/// content having been written, or at the deadline, when the files are compiled as they
/// are. Either rebuild takes the baseline again, so a file that stays empty is not
/// emptied any more, and a later truncation starts a hold of its own.
///
/// A file the rebuild's compile read can also be found emptied after the rebuild looked —
/// a truncation that began between the look and the read ([`Self::on_late_empty`]). That
/// rebuild is held as one the look found is, unless the deadline ended the hold in it: a
/// rebuild the deadline runs compiles the files as they are, and nothing found after its
/// look holds it again.
///
/// The hold never reads the clock: every instant is an argument. The rebuild that looks
/// at the files passes the instant it looked — never earlier than the deadline it is run
/// for ([`not_before`]); the watch loop's driver, [`TickClock`], waits for
/// [`Self::deadline`] and wakes the session there — under `--poll-interval 0` too, where
/// no tick would.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum EmptyHold {
    /// No rebuild is held.
    #[default]
    Off,
    /// Rebuilds are held until this instant.
    Until(Instant),
    /// The deadline ended the hold in the rebuild now running, which compiles the files as
    /// they are; no rebuild is held. The next rebuild's look decides afresh.
    Expired,
}

/// What a rebuild does, as [`EmptyHold::on_rebuild`] decides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldVerdict {
    /// Compile: no watched file is emptied, or the hold's deadline has come.
    Compile,
    /// Hold the rebuild back: a watched file is emptied and the deadline is still ahead.
    Hold,
}

impl EmptyHold {
    /// A rebuild at `now` found a watched file emptied (`emptied`), or found none.
    fn on_rebuild(&mut self, emptied: bool, now: Instant) -> HoldVerdict {
        match (*self, emptied) {
            (_, false) => {
                *self = Self::Off;
                HoldVerdict::Compile
            }
            (Self::Off | Self::Expired, true) => {
                *self = Self::Until(now + EMPTY_HOLD_DEADLINE);
                HoldVerdict::Hold
            }
            (Self::Until(until), true) if now < until => HoldVerdict::Hold,
            (Self::Until(_), true) => {
                *self = Self::Expired;
                HoldVerdict::Compile
            }
        }
    }

    /// A rebuild that compiled found, at `now`, a file its compile read emptied since the
    /// rebuild looked. It is held as [`Self::on_rebuild`] holds one the look found — the
    /// deadline set by the first such finding and never moved — unless the deadline ended
    /// the hold in this rebuild, which compiles the files as they are.
    fn on_late_empty(&mut self, now: Instant) -> HoldVerdict {
        match *self {
            Self::Expired => HoldVerdict::Compile,
            Self::Off | Self::Until(_) => self.on_rebuild(true, now),
        }
    }

    /// When the held rebuild runs anyway, if a rebuild is held.
    fn deadline(&self) -> Option<Instant> {
        match *self {
            Self::Off | Self::Expired => None,
            Self::Until(until) => Some(until),
        }
    }
}

// ── Idle tick ─────────────────────────────────────────────────────────────────

/// The idle tick's schedule, holding an **absolute** deadline (#319).
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
/// tick comes due on schedule no matter how loaded the channel is. A tick that is
/// already due is reported before any message is taken ([`TickPoll::Due`]), so a
/// channel that is never empty cannot postpone it either.
///
/// Re-arming to `now + interval` at the moment a tick is *observed* bounds it from
/// the other side. The probe — and any recompile it triggers — runs between two
/// polls, so a probe that overruns its own interval simply arms the next deadline
/// from when it finished. It can never accumulate overdue ticks and fire them
/// back-to-back: at most one tick per interval, under every load.
///
/// The schedule never reads the clock: every instant is an argument. [`TickClock`]
/// is the driver that reads it and waits on the channel.
#[derive(Debug, Clone, Copy)]
enum TickSchedule {
    /// `--poll-interval 0`: the probe is off, and no tick ever comes due.
    Off,
    /// One tick per `interval`; the next comes due at `next`.
    Every { interval: Duration, next: Instant },
}

/// What a [`TickSchedule`] says at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TickPoll {
    /// No tick will ever come due: wait for a message for as long as it takes.
    Never,
    /// The tick is due: run it before taking another message. The schedule has
    /// already re-armed one interval from the instant it was asked at.
    Due,
    /// Not due yet: wait at most this long for a message.
    Wait(Duration),
}

impl TickSchedule {
    /// The schedule of a session that starts at `now`: its first tick comes due one
    /// interval later.
    fn start(interval: Option<Duration>, now: Instant) -> Self {
        match interval {
            None => Self::Off,
            Some(interval) => Self::Every {
                interval,
                next: now + interval,
            },
        }
    }

    /// Whether the tick is due at `now`. A due tick re-arms the schedule
    /// ([`Self::rearm`]).
    fn poll(&mut self, now: Instant) -> TickPoll {
        let Self::Every { next, .. } = *self else {
            return TickPoll::Never;
        };
        if now >= next {
            self.rearm(now);
            TickPoll::Due
        } else {
            TickPoll::Wait(next - now)
        }
    }

    /// A tick was observed at `now`: the next comes due one interval after it — never
    /// one interval after the deadline it may have missed.
    fn rearm(&mut self, now: Instant) {
        if let Self::Every { interval, next } = self {
            *next = now + *interval;
        }
    }
}

/// What the watch loop's two deadlines — a held rebuild's ([`EmptyHold`]) and the idle
/// tick's ([`TickSchedule`]) — say at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakePoll {
    /// The held rebuild's deadline has come: run it before taking another message, and
    /// before a tick due at the same instant.
    HoldDue,
    /// The tick is due: run it before taking another message. The schedule has already
    /// re-armed.
    TickDue,
    /// Nothing is due: wait at most `wait` for a message. The wait runs out at the held
    /// rebuild's deadline when `hold`, else at the tick's.
    Wait { wait: Duration, hold: bool },
    /// No deadline at all — no rebuild held, and `--poll-interval 0`: wait for a message
    /// for as long as it takes.
    Never,
}

/// The watch loop's next wake at `now`: the held rebuild's deadline `hold`, if a rebuild
/// is held, and the idle tick's `schedule`, whichever comes first (#380).
///
/// The hold is served first when both are due. Its deadline is the one a user waits on —
/// an emptied file published within a second — and the rebuild it runs takes the baseline
/// again, so a tick due at the same instant then finds nothing to do but re-arm watches.
/// A tick left due is served on the very next poll, so neither postpones the other by
/// more than one wake, and a channel that is never empty postpones neither.
fn poll_wake(schedule: &mut TickSchedule, hold: Option<Instant>, now: Instant) -> WakePoll {
    if hold.is_some_and(|until| now >= until) {
        return WakePoll::HoldDue;
    }
    let until_hold = hold.map(|until| until - now);
    match (schedule.poll(now), until_hold) {
        (TickPoll::Due, _) => WakePoll::TickDue,
        (TickPoll::Never, None) => WakePoll::Never,
        (TickPoll::Never, Some(wait)) => WakePoll::Wait { wait, hold: true },
        (TickPoll::Wait(tick), Some(wait)) if wait <= tick => WakePoll::Wait { wait, hold: true },
        (TickPoll::Wait(wait), _) => WakePoll::Wait { wait, hold: false },
    }
}

/// What wakes the watch loop ([`TickClock::recv_next`]).
enum Wake {
    /// A message arrived: a filesystem event, or Ctrl+C.
    Message(Msg),
    /// The idle tick came due ([`TickSchedule`]).
    Tick,
    /// The held rebuild's deadline came ([`EmptyHold`], #380).
    HoldDue,
}

/// Drives a [`TickSchedule`] and a held rebuild's deadline on the real clock and the watch
/// channel ([`poll_wake`]): the only place the watch loop reads `Instant::now()` or waits.
struct TickClock {
    schedule: TickSchedule,
}

impl TickClock {
    fn new(interval: Option<Duration>) -> Self {
        Self {
            schedule: TickSchedule::start(interval, Instant::now()),
        }
    }

    /// Wait for the next wake: a message from the watch channel, the idle tick, or `hold`
    /// — the deadline of a rebuild held while a watched file is empty (#380). A held
    /// rebuild's deadline is waited for whatever the poll interval: under
    /// `--poll-interval 0`, with no tick to wake the loop, nothing else would.
    ///
    /// `Err` means the channel disconnected; the caller stops.
    fn recv_next(
        &mut self,
        rx: &mpsc::Receiver<Msg>,
        hold: Option<Instant>,
    ) -> std::result::Result<Wake, mpsc::RecvError> {
        match poll_wake(&mut self.schedule, hold, Instant::now()) {
            WakePoll::HoldDue => Ok(Wake::HoldDue),
            WakePoll::TickDue => Ok(Wake::Tick),
            WakePoll::Never => rx.recv().map(Wake::Message),
            WakePoll::Wait { wait, hold } => match rx.recv_timeout(wait) {
                Ok(msg) => Ok(Wake::Message(msg)),
                Err(mpsc::RecvTimeoutError::Timeout) if hold => Ok(Wake::HoldDue),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.schedule.rearm(Instant::now());
                    Ok(Wake::Tick)
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(mpsc::RecvError),
            },
        }
    }
}

// ── Debounce loop ─────────────────────────────────────────────────────────────

/// Largest accepted `--debounce` window; larger values are clamped to it.
///
/// `Instant::now() + Duration::from_millis(u64::MAX)` does not overflow on the
/// i64-second monotonic clocks of macOS and Linux: the deadline lands roughly 585
/// million years out, so an unclamped `--debounce 18446744073709551615` watches
/// forever and silently never rebuilds (observed). 60s is orders of magnitude past
/// any editor save burst.
const MAX_DEBOUNCE_MS: u64 = 60_000;

/// Absolute cap on one debounce window, as a multiple of the window.
const DEBOUNCE_CAP_FACTOR: u32 = 10;

/// Floor under the absolute cap.
///
/// Matches the default `--poll-interval`: while a window is open the loop never
/// reaches `TickClock::recv_next`, so this floor is also the bound on how late the
/// idle-tick backstop can run under a continuous event stream.
const DEBOUNCE_CAP_FLOOR: Duration = Duration::from_millis(1_000);

/// Upper bound on the messages one window will drain.
///
/// The cap bounds the window's DURATION; this bounds its work and its memory. A
/// sender faster than the drain would otherwise grow `paths` without limit inside a
/// single window. Messages left in the channel are not lost: the caller's next
/// event opens a new window and drains them.
const MAX_DEBOUNCE_MESSAGES: usize = 10_000;

/// Why a debounce window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebounceEnd {
    /// `--debounce 0`: coalescing is off; no window was ever opened.
    Disabled,
    /// The window closed on its own ([`DebounceWindow::classify`]).
    Closed(WindowEnd),
    /// Ctrl+C. The caller must stop, not rebuild.
    Interrupted,
    /// The watcher's sender was dropped.
    Disconnected,
}

/// Why an open [`DebounceWindow`] closed on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowEnd {
    /// A full window passed with no further content event: the intended exit.
    Quiet,
    /// The absolute cap elapsed while events were still arriving.
    Cap,
    /// `MAX_DEBOUNCE_MESSAGES` messages were drained in this window.
    MessageLimit,
}

/// Result of one debounce window.
struct DebounceOutcome {
    /// Every path seen in a content event during the window.
    paths: BTreeSet<PathBuf>,
    /// Why the window ended.
    end: DebounceEnd,
}

impl DebounceOutcome {
    fn interrupted(&self) -> bool {
        self.end == DebounceEnd::Interrupted
    }
}

/// Convert a raw `--debounce` value (milliseconds) into a quiet-period window.
///
/// - `0` -> `None`: coalescing disabled, every event rebuilds immediately.
/// - nonzero -> `Some(min(value, MAX_DEBOUNCE_MS))`.
///
/// Extracted so the clamp contract is verifiable without the watch loop, exactly as
/// [`clamp_poll_interval`] is.
fn clamp_debounce(debounce_ms: u64) -> Option<Duration> {
    if debounce_ms == 0 {
        None
    } else {
        Some(Duration::from_millis(debounce_ms.min(MAX_DEBOUNCE_MS)))
    }
}

/// Absolute bound on one debounce window: `max(10 x window, 1s)`.
///
/// `window * DEBOUNCE_CAP_FACTOR` cannot overflow `Duration`: [`clamp_debounce`] caps
/// the window at 60s, so the product is at most 600s.
fn debounce_cap(window: Duration) -> Duration {
    (window * DEBOUNCE_CAP_FACTOR).max(DEBOUNCE_CAP_FLOOR)
}

/// One open debounce window: a quiet period with an absolute cap and a message limit
/// (see [`drain_debounce`] for why each rule exists).
///
/// It never reads the clock: every instant is an argument. [`drain_debounce`] is the
/// driver that reads it and waits on the channel.
struct DebounceWindow {
    /// The quiet period every content event restarts.
    window: Duration,
    /// `start + debounce_cap(window)`: no event extends the window past it.
    cap_end: Instant,
    /// When the window closes unless a content event extends it; never past `cap_end`.
    deadline: Instant,
    /// Messages drained so far, of every kind.
    messages: usize,
    /// Every path seen in a content event.
    paths: BTreeSet<PathBuf>,
}

impl DebounceWindow {
    /// A window opened at `start`: it closes one window later unless a content event
    /// extends it, and never after `start + debounce_cap(window)`.
    fn open(window: Duration, start: Instant) -> Self {
        Self {
            window,
            cap_end: start + debounce_cap(window),
            deadline: start + window,
            messages: 0,
            paths: BTreeSet::new(),
        }
    }

    /// A filesystem event drained at `now`. It counts toward the message limit; a
    /// content event also contributes its paths and extends the window
    /// ([`Self::on_content`]). An `Access` event does neither (inotify
    /// IN_ACCESS/IN_OPEN/IN_CLOSE_NOWRITE — reads must not trigger recompiles; see
    /// [`is_content_event`]).
    fn on_event(&mut self, event: notify::Event, now: Instant) {
        self.messages += 1;
        if !is_content_event(&event.kind) {
            return;
        }
        self.paths.extend(event.paths);
        self.on_content(now);
    }

    /// A watch error was drained: it counts toward the message limit and extends
    /// nothing.
    fn on_watch_error(&mut self) {
        self.messages += 1;
    }

    /// A content event at `now`: the window now closes one window after it, but never
    /// after the cap end.
    fn on_content(&mut self, now: Instant) {
        self.deadline = (now + self.window).min(self.cap_end);
    }

    /// When the window closes unless a content event extends it.
    fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Whether the window has closed by `now`, and why. `None`: it is still open, so
    /// wait for a message until [`Self::deadline`].
    fn classify(&self, now: Instant) -> Option<WindowEnd> {
        // The bound, enforced in release too: a window may be extended by further
        // events, never past `start + cap`. Pure arithmetic: a descheduled runner
        // cannot trip it, only a defect can. Asserting on MEASURED elapsed time
        // instead would panic a shipped watcher whenever `recv_timeout` overshoots.
        assert!(
            self.deadline <= self.cap_end,
            "debounce deadline escaped its cap: a file written to continuously would \
             postpone its own rebuild (and the idle-tick backstop behind it) for as \
             long as the writing lasts"
        );
        if self.messages >= MAX_DEBOUNCE_MESSAGES {
            Some(WindowEnd::MessageLimit)
        } else if now < self.deadline {
            None
        } else if self.deadline == self.cap_end {
            Some(WindowEnd::Cap)
        } else {
            Some(WindowEnd::Quiet)
        }
    }
}

/// Coalesce a burst of filesystem events into one rebuild.
///
/// # Quiet period, not a fixed window
///
/// The first relevant event opens a window of `debounce_ms`; every further **content**
/// event restarts it. A window that expired at a fixed offset from the FIRST event
/// split any burst longer than `debounce_ms` across two or three windows and rebuilt
/// once per window, each compile seeing a different intermediate state of the file:
/// visible as three `Recompiled` lines from one ten-write burst on a loaded CI runner.
/// The size of the burst a user can produce is not a property `debounce_ms` can
/// predict; the size of the GAP between saves is.
///
/// # Why the cap is mandatory
///
/// An extendable window with no bound is unbounded: a file written to continuously
/// postpones its own rebuild for as long as the writing lasts. Worse, the idle-tick
/// liveness probe is not consulted while a window is open ([`TickClock::recv_next`] is
/// only reached between batches), so an endless stream would starve the content
/// backstop through a door the absolute tick deadline does not cover. The cap
/// (`max(10 x window, 1s)`) bounds both: the rebuild, and the probe behind it.
///
/// # What does NOT extend
///
/// `Access` events (inotify reads; see [`is_content_event`]) and watch errors. The
/// compile reads its own sources, so an extending `Access` event would let the watcher
/// hold its own window open.
///
/// Relevance is deliberately NOT filtered here. An editor's atomic save writes a temp
/// file and renames it; that temp path is in no watch set, and ending the window on it
/// would split the very burst this exists to coalesce. Relevance decides whether to
/// rebuild ([`event_is_relevant`] in file mode, the `.mds`/root filter in dir mode);
/// this decides when.
fn drain_debounce(rx: &mpsc::Receiver<Msg>, debounce_ms: u64) -> DebounceOutcome {
    let Some(window) = clamp_debounce(debounce_ms) else {
        // `--debounce 0`: no coalescing. The channel is left untouched, so the next
        // event is delivered to the loop as its own batch.
        return DebounceOutcome {
            paths: BTreeSet::new(),
            end: DebounceEnd::Disabled,
        };
    };

    // The window decides; this loop only reads the clock and waits on the channel.
    // Bounded by the window: it closes at its cap end or its message limit.
    let mut open = DebounceWindow::open(window, Instant::now());
    let end = loop {
        let now = Instant::now();
        if let Some(closed) = open.classify(now) {
            break DebounceEnd::Closed(closed);
        }
        match rx.recv_timeout(open.deadline() - now) {
            Ok(Msg::Fs(Ok(event))) => open.on_event(event, Instant::now()),
            Ok(Msg::Fs(Err(e))) => {
                open.on_watch_error();
                eprint_warning(&format!(
                    "warning: watch error during debounce: {}",
                    safe_inline(notify_cause(&e))
                ));
            }
            Ok(Msg::Interrupt) => break DebounceEnd::Interrupted,
            // The deadline is the single decision point: re-loop and let the window
            // classify the exit.
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break DebounceEnd::Disconnected,
        }
    };

    DebounceOutcome {
        paths: open.paths,
        end,
    }
}

// ── Poll-interval clamp (reconcile rule) ─────────────────────────────────────────────

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

// ── Working directory ─────────────────────────────────────────────────────────

/// The working directory `mds watch` started in, by its canonical path.
///
/// Every path typed relative — the entry, a directory argument such as `.`, `--vars`, a
/// file-mode `-o` — is resolved against the process's working directory on every
/// rebuild. Once that directory is deleted, the process is left in it, and none of them
/// resolves again, even after a directory is recreated at the same path, as a
/// `git checkout` that removes and restores it does (#417).
/// [`restore_if_recreated`](Self::restore_if_recreated) moves the process back into it.
struct WorkingDir {
    /// `None` when the working directory did not resolve at startup: nothing is restored.
    canonical: Option<PathBuf>,
}

impl WorkingDir {
    /// Record the working directory at startup, before any rebuild reads a path typed
    /// relative to it.
    fn record() -> Self {
        WorkingDir {
            canonical: std::env::current_dir()
                .and_then(|dir| dir.canonicalize())
                .ok(),
        }
    }

    /// Called first by every rebuild, before it reads anything. When the working
    /// directory has been deleted and a directory exists again at its recorded canonical
    /// path, reached through no symlink (canonical compared with canonical, #408), it
    /// becomes the working directory again. A working directory that still exists is
    /// never changed. Otherwise nothing changes, and a path typed relative that does not
    /// resolve is reported as typed by the read that needs it, as any missing file is.
    ///
    /// A directory swapped in between the check and the move is the check-then-open
    /// window every path-based read has; the compile checks that the path it compiles
    /// still leads to the file being watched either way
    /// ([`WatchedPath::ensure_unmoved`]).
    fn restore_if_recreated(&self) {
        let Some(recorded) = &self.canonical else {
            return;
        };
        if std::env::current_dir().is_ok() {
            return;
        }
        if recorded.canonicalize().is_ok_and(|now| now == *recorded) {
            // A failure leaves the process where it was; see above.
            let _ = std::env::set_current_dir(recorded);
        }
    }
}

// ── The out-dir during a session (#160) ───────────────────────────────────────

/// What a write finds where the out-dir was ([`OutDirAnchor::check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutDirNow {
    /// The directory the session last saw there.
    Unchanged,
    /// No directory, or a different one at the same path — deleted, or another put in its
    /// place: the write creates or uses it, and no output the session wrote is in it.
    New,
    /// The path the user named now leads to a different directory: the write is refused.
    Elsewhere,
}

/// The out-dir a `mds watch` session writes below, as it was when the session started
/// (#160): `--out-dir`, or `mds.json`'s `build.output_dir` below the directory `mds.json`
/// is in.
///
/// Every write below it first resolves the path the user named again — `--out-dir` as
/// typed, or the directory `mds.json` was reached by — as the write would create it, and
/// compares the result with the one resolved when the session started, canonical with
/// canonical (#408). A different directory — a symlink on the path retargeted, the out-dir
/// replaced by a link, or the working directory a relative one is typed against moved —
/// refuses the write: the session writes only where it started, and following the path
/// elsewhere is for a restart to decide. The same
/// path is written below whatever directory is there: a deleted out-dir is created again
/// by the write, a directory put in its place is used, and either becomes the one the
/// next write compares; none of the outputs the session wrote is in it, so the content
/// dedup is cleared and none is skipped as written already. Nothing is held open between
/// writes.
///
/// The check is by path, and so is the write's open of its anchor, so a link swapped onto
/// the path between the two would lead the open elsewhere: the check also finds the
/// directory the write is anchored at — `resolved`, or the nearest directory above it
/// that is there when it is missing — and the write is made only if the anchor it opens
/// is that directory ([`below_checked_out_dir`]).
struct OutDirAnchor {
    /// The path the user named: `--out-dir` as typed, or the directory `mds.json` was
    /// reached by.
    typed: PathBuf,
    /// Where `typed` led when the session started, as a write would create it: the
    /// directory a write below the out-dir is anchored at.
    resolved: PathBuf,
    /// The directory outputs are written below: `resolved`, or `build.output_dir` below it.
    out_dir: PathBuf,
    /// The directory the session last saw at `out_dir`; `None` while there was none.
    identity: Option<DirIdentity>,
    /// Where the last check found the directory the next write is anchored at; `None`
    /// before the first check and after one that refused the write.
    checked: Option<CheckedAnchor>,
}

/// The directory a write below the out-dir is anchored at, as [`OutDirAnchor::check`]
/// found it: `resolved` itself, or — when it is missing, and the write is to create it —
/// the nearest directory above it that is there, `missing` levels up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckedAnchor {
    missing: usize,
    identity: DirIdentity,
}

impl CheckedAnchor {
    /// `dir`, or the nearest directory above it that is there; `None` when none is.
    fn find(dir: &Path) -> Option<Self> {
        dir.ancestors().enumerate().find_map(|(missing, at)| {
            DirIdentity::of(at).map(|identity| Self { missing, identity })
        })
    }
}

impl OutDirAnchor {
    /// The out-dir a session writes below, recorded once its startup writes are made:
    /// `--out-dir`, else `build.output_dir`. `None` for neither — an output beside its
    /// source is below the watched entry's or root's directory, which every compile
    /// checks ([`WatchedPath::ensure_unmoved`]) — and for a working directory that does
    /// not resolve.
    fn record(out_dir: Option<&Path>, config: Option<&ProjectConfig>) -> Option<Self> {
        let (typed, below) = match (out_dir, config) {
            (Some(typed), _) => (typed.to_path_buf(), None),
            (None, Some(project)) => (
                project.shown_dir.clone(),
                Some(project.config.build.output_dir.as_deref()?),
            ),
            (None, None) => return None,
        };
        let resolved = resolve_dir_as_created(&typed)?;
        let out_dir = below.map_or_else(|| resolved.clone(), |below| resolved.join(below));
        let identity = DirIdentity::of(&out_dir);
        Some(Self {
            typed,
            resolved,
            out_dir,
            identity,
            checked: None,
        })
    }

    /// What the out-dir is now ([`OutDirNow`]). A directory that is not the one last seen
    /// becomes the one the next check compares.
    ///
    /// The directory the write is anchored at is found first and the path the user named
    /// resolved after: a link swapped in before then changes where that path leads, and
    /// one swapped in after it leads the write's open to another directory than the one
    /// found here, which the write refuses.
    fn check(&mut self) -> OutDirNow {
        self.checked = CheckedAnchor::find(&self.resolved);
        if self.checked.is_none()
            || resolve_dir_as_created(&self.typed).as_ref() != Some(&self.resolved)
        {
            self.checked = None;
            return OutDirNow::Elsewhere;
        }
        let now = DirIdentity::of(&self.out_dir);
        if now.is_some() && now == self.identity {
            OutDirNow::Unchanged
        } else {
            self.identity = now;
            OutDirNow::New
        }
    }

    /// A write below the out-dir succeeded: the directory it created, if it found none,
    /// is the one the next check compares.
    fn written(&mut self) {
        if self.identity.is_none() {
            self.identity = DirIdentity::of(&self.out_dir);
        }
    }
}

/// The out-dir as it is now (#160), for a session with none treated as
/// [`OutDirNow::Unchanged`]: one the typed path leads elsewhere from refuses the write
/// below; a new one holds nothing the session wrote, so `last_written` is cleared.
fn check_out_dir<K, V>(
    anchor: Option<&mut OutDirAnchor>,
    last_written: &mut HashMap<K, V>,
) -> OutDirNow {
    let now = anchor.map_or(OutDirNow::Unchanged, OutDirAnchor::check);
    if now == OutDirNow::New {
        last_written.clear();
    }
    now
}

/// `target`, a write below the out-dir after a check that admitted it, made below the
/// directory that check found and only if the anchor the write opens is that directory
/// (#160): a link swapped onto the path in between is refused, not followed. A session
/// with no out-dir writes `target` as it is.
fn below_checked_out_dir(anchor: Option<&OutDirAnchor>, target: &WriteTarget) -> WriteTarget {
    match anchor.and_then(|anchor| Some((anchor, anchor.checked?))) {
        Some((anchor, CheckedAnchor { missing, identity })) => {
            target.below_checked_anchor(&anchor.resolved, missing, identity)
        }
        None => target.clone(),
    }
}

/// Why a session retires one of its outputs (#160), which decides how a removal is told.
#[derive(Clone, Copy)]
enum Retirement {
    /// Its source was deleted: a removal prints `Removed <output> (source deleted)`.
    SourceDeleted,
    /// Its source now compiles to the other kind, whose output was just written: a
    /// removal is silent, as cleaning up after a change of kind always was.
    KindChanged,
}

/// What a directory-mode session wrote at one of its output paths, and for which source
/// (#160): what a later write of the same may skip, the bytes a removal or a replace asks
/// the file to still hold, and whose output the file is — beside their sources `a.b.mds`
/// and `a.mds` both name theirs `a.md` or `a.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WrittenOutput {
    /// The source the output was written for.
    source: PathBuf,
    /// The bytes written.
    content: String,
}

/// What a session's record says of the file at one of its output paths, for the source a
/// removal or a write after a change of kind is made for (#160).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Record<'a> {
    /// No record: the session never wrote there, not since the out-dir was made, or the
    /// file it wrote is gone.
    Unwritten,
    /// The session last wrote there for another source.
    OtherSource,
    /// The session last wrote these bytes there for this source.
    Own(&'a str),
}

impl<'a> Record<'a> {
    /// The record of a file-mode session, whose one source is the entry: what it last
    /// wrote there, if anything.
    fn of_entry(written: Option<&'a String>) -> Self {
        written.map_or(Self::Unwritten, |content| Self::Own(content))
    }
}

/// Retire `out`, an output its source no longer has (#160). `record` is what this
/// session's record says of the file there for that source, and `now` what the out-dir
/// check made before this found.
///
/// The file is removed only if this session wrote it for that source and it still holds
/// exactly those bytes, and only as every deletion below the out-dir is made: below the
/// directory that check found ([`below_checked_out_dir`]) — one the typed path leads
/// elsewhere from refuses the removal — through no symlink and only as a regular file
/// ([`remove_proven`]). A file this session did not write, one it wrote for another
/// source, and one changed since it was written are kept, with one notice saying which
/// (none under `--quiet`). A name nothing has is not mentioned; a symlink there, live or
/// dangling, is something there, and the session's own is refused with a warning.
///
/// Returns whether the file is gone — removed, or not there — so that the record of it
/// can go too; a file kept, or one whose removal failed, keeps its record.
#[must_use]
fn retire_output(
    out: &WriteTarget,
    anchor: Option<&OutDirAnchor>,
    now: OutDirNow,
    record: Record<'_>,
    why: Retirement,
    quiet: bool,
) -> bool {
    // Looked at without following a symlink: a link at the output, live or dangling, is
    // something there, and refused below as one.
    if std::fs::symlink_metadata(&out.path).is_err() {
        return true;
    }
    let written = match record {
        Record::Own(written) => written,
        Record::Unwritten => {
            if !quiet {
                crate::output::ewriteln!(
                    "Kept {}: not written by this session",
                    safe_path(&out.shown)
                );
            }
            return false;
        }
        Record::OtherSource => {
            if !quiet {
                crate::output::ewriteln!(
                    "Kept {}: not written by this source",
                    safe_path(&out.shown)
                );
            }
            return false;
        }
    };
    let removal = match now {
        OutDirNow::Elsewhere => Err(NotRemoved::out_dir_moved()),
        OutDirNow::Unchanged | OutDirNow::New => {
            remove_proven(&below_checked_out_dir(anchor, out), |file| {
                holds_exactly(file, written)
            })
        }
    };
    match removal {
        Ok(Removal::Removed) => {
            if let (Retirement::SourceDeleted, false) = (why, quiet) {
                crate::output::ewriteln!("Removed {} (source deleted)", safe_path(&out.shown));
            }
            true
        }
        Ok(Removal::Kept) => {
            if !quiet {
                crate::output::ewriteln!(
                    "Kept {}: changed since it was written",
                    safe_path(&out.shown)
                );
            }
            false
        }
        Ok(Removal::Missing) => true,
        Err(not_removed) => {
            match why {
                Retirement::SourceDeleted => eprint_warning(&format!(
                    "warning: could not remove {}: {}",
                    safe_path(&out.shown),
                    safe_inline(not_removed.cause())
                )),
                Retirement::KindChanged => eprint_warning(&format!(
                    "warning: could not remove stale output {}: {}",
                    safe_path(&out.shown),
                    safe_inline(not_removed.cause())
                )),
            }
            false
        }
    }
}

/// Write `content` to `out`, the output of a source's new kind after a change of kind
/// (#160) — `record`, what this session's record says of the file there for that source
/// — only where nothing is, or over the file this session wrote there for that source
/// while it still holds exactly those bytes ([`write_over_own`]), as an output is
/// written: below the directory the caller's out-dir check found, with the directories it
/// goes in created. Anything else there — a file this session did not write, one it wrote
/// for another source, one changed since, a symlink, a directory — is kept, with one
/// notice saying which (none under `--quiet`, and none when `told`: the rebuild of the
/// same source just before kept the same content), and nothing is written: `Ok(false)`,
/// so a later save tries again. The output of the old kind is the caller's to retire, and
/// only once the new one is written.
fn write_after_change_of_kind(
    out: &WriteTarget,
    record: Record<'_>,
    content: &str,
    told: bool,
    quiet: bool,
) -> std::result::Result<bool, MdsError> {
    let own = match record {
        Record::Own(written) => Some(written),
        Record::Unwritten | Record::OtherSource => None,
    };
    match write_over_own(out, own, content, Durability::RenameOnly, Parents::Create) {
        Ok(()) => Ok(true),
        Err(NotCreated::Exists) => {
            if !told && !quiet {
                match record {
                    Record::Own(_) => crate::output::ewriteln!(
                        "Kept {}: changed since it was written; not overwritten",
                        safe_path(&out.shown)
                    ),
                    Record::OtherSource => crate::output::ewriteln!(
                        "Kept {}: not written by this source; not overwritten",
                        safe_path(&out.shown)
                    ),
                    Record::Unwritten => crate::output::ewriteln!(
                        "Kept {}: not written by this session; not overwritten",
                        safe_path(&out.shown)
                    ),
                }
            }
            Ok(false)
        }
        Err(NotCreated::Failed(e)) => Err(e),
    }
}

/// Whether `file` holds exactly `written`: read no further than one byte past it.
fn holds_exactly(file: &mut std::fs::File, written: &str) -> std::io::Result<bool> {
    let len = written.len() as u64;
    Ok(mds::read_at_most(file, len.saturating_add(1), len)? == written.as_bytes())
}

// ── Watched paths ─────────────────────────────────────────────────────────────

/// What a [`WatchedPath`] is: it decides how the typed form is resolved and how a
/// refusal names it.
#[derive(Clone, Copy)]
enum Watched {
    /// File mode's entry.
    Entry,
    /// Directory mode's root.
    Root,
    /// A source below directory mode's root, by its walked path.
    Source,
}

impl Watched {
    /// The refusal once `typed`, a path of this kind, leads somewhere other than the
    /// watched file or directory: `mds::io`, naming the path as typed, escaped — never by
    /// its canonical absolute path (#417, #413).
    fn moved(self, typed: &Path) -> MdsError {
        let (watched, different) = match self {
            Watched::Entry => ("entry", "file"),
            Watched::Root => ("directory", "directory"),
            Watched::Source => ("file", "file"),
        };
        MdsError::Io {
            message: format!(
                "watched {watched} now resolves to a different {different}: \"{}\"; \
                 restart mds watch to follow it",
                mds::escape_path_for_message(&typed.to_string_lossy())
            ),
        }
    }
}

/// A path `mds watch` holds in two forms (#417, #413): file mode's entry, directory
/// mode's root, or a source below that root — one type, checked by one rule.
///
/// `typed` is the path as the user reaches it: as typed, or for a source the root as
/// typed joined with the source's path below it, the form `mds build <dir>` walks. Every
/// compile goes through it, so an error names the file that way, never by its canonical
/// absolute path (#417, #265), `mds.json` is looked up from it at startup (#413), and
/// every status line names the path, and an output resolved beside or below it, that
/// way (#390).
/// `canonical` is the form notify reports event paths under: every identity check —
/// watched directories, files of interest, graph keys, output paths, baselines — uses
/// it. The two are never compared as text (#408):
/// [`ensure_unmoved`](Self::ensure_unmoved) resolves `typed` again and compares the
/// result with `canonical`, canonical with canonical.
struct WatchedPath {
    typed: PathBuf,
    canonical: PathBuf,
    what: Watched,
}

impl WatchedPath {
    /// The entry in the form [`admit_output`] takes, typed first, so the two forms
    /// cannot be handed over swapped.
    fn paths(&self) -> EntryPaths<'_> {
        EntryPaths {
            typed: &self.typed,
            canonical: &self.canonical,
        }
    }

    /// Refuse to compile once `typed` leads somewhere other than `canonical` — the one
    /// rule for the entry, the root and each source ([`Watched::moved`]).
    ///
    /// Every compile resolves `typed` afresh, while notify keeps watching `canonical`.
    /// Once a symlink on the typed path is retargeted — `link/..` is the directory above
    /// the link's target — or a directory on it is replaced, a symlink included, the two
    /// name different files: the compile would read one while the watched directories,
    /// graph keys, output path and change detection follow the other. So the directory
    /// `typed` now leads into must be the one `canonical` names: the root's own canonical
    /// form, and the canonical parent of the entry or a source — the directories, not
    /// the files, so a file name the volume respells (its case, on a case-insensitive
    /// volume) is not a move (#408). A typed path that no longer resolves — deleted,
    /// before it is recreated — and a file whose final component is a symlink are left
    /// to the compile, which reports the first as typed and refuses the second. A
    /// retarget racing the compile itself is the check-then-open window every
    /// path-based read has.
    fn ensure_unmoved(&self) -> Result<(), MdsError> {
        let moved = match self.what {
            Watched::Root => self
                .typed
                .canonicalize()
                .is_ok_and(|now| now != self.canonical),
            Watched::Entry | Watched::Source => mds::NativeFs::check_symlink(&self.typed)
                .is_ok_and(|now| now.parent() != self.canonical.parent()),
        };
        if moved {
            Err(self.what.moved(&self.typed))
        } else {
            Ok(())
        }
    }

    /// Compile the entry, or a source, by its typed path once
    /// [`ensure_unmoved`](Self::ensure_unmoved) has confirmed that it still leads to the
    /// file being watched. `mds watch` writes no source maps, so the compile takes the
    /// default options. A panic in the compile is caught: the session goes on (#389).
    fn compile(
        &self,
        runtime_vars: Option<HashMap<String, mds::Value>>,
        quiet: bool,
    ) -> std::result::Result<CompileOutput, CompileFailure> {
        self.ensure_unmoved().map_err(miette::Error::from)?;
        let compiled = crate::output::catch_compile(
            &self.typed,
            AssertUnwindSafe(|| {
                compile_to_content(
                    &self.typed,
                    runtime_vars,
                    quiet,
                    mds::CompileOptions::default(),
                )
            }),
        );
        compiled
            .map_err(|Panicked| CompileFailure::Panicked)?
            .map_err(CompileFailure::from)
    }

    /// The root in the two forms an output below it is resolved and named by: walked
    /// canonical, named as typed (#390).
    fn root_paths(&self) -> RootPaths<'_> {
        RootPaths {
            typed: &self.typed,
            walked: &self.canonical,
        }
    }

    /// The directory of the entry in the same two forms: the canonical directory notify
    /// watches, named by the entry's directory as typed (#390).
    fn dir_paths(&self) -> RootPaths<'_> {
        RootPaths {
            typed: mds::effective_parent(&self.typed),
            walked: mds::effective_parent(&self.canonical),
        }
    }

    /// The path `src` (a walked or graph-key path under the root's `canonical`) is
    /// compiled by: the form its output is named below the root by, too.
    ///
    /// A source outside the root — an out-of-root dependency (DD3) — has no walked
    /// form and is compiled by its canonical path.
    fn walked(&self, src: &Path) -> PathBuf {
        self.root_paths().shown_below(src)
    }

    /// Compile `src` below the root — every directory-mode compile goes through here.
    ///
    /// A source under the root is compiled as a [`Watched::Source`] by its
    /// [walked](Self::walked) path, keyed by `src`, after the root itself: a moved root
    /// is refused naming the root, and a directory moved below it naming the source. The
    /// startup walk skips symlinked directories, but a rebuild compiles the sources it
    /// already knows, and the idle tick's content check follows a link — so a directory
    /// below the root replaced by a symlink after startup is caught at the source. `src`
    /// is canonical: a key the walk of the canonical root produced, or a graph key. An
    /// out-of-root dependency is compiled by its canonical path, with no walked form to
    /// check.
    fn compile_source(
        &self,
        src: &Path,
        runtime_vars: Option<HashMap<String, mds::Value>>,
        quiet: bool,
    ) -> std::result::Result<CompileOutput, CompileFailure> {
        if !src.starts_with(&self.canonical) {
            let compiled = crate::output::catch_compile(
                src,
                AssertUnwindSafe(|| {
                    compile_to_content(src, runtime_vars, quiet, mds::CompileOptions::default())
                }),
            );
            return compiled
                .map_err(|Panicked| CompileFailure::Panicked)?
                .map_err(CompileFailure::from);
        }
        self.ensure_unmoved().map_err(miette::Error::from)?;
        WatchedPath {
            typed: self.walked(src),
            canonical: src.to_path_buf(),
            what: Watched::Source,
        }
        .compile(runtime_vars, quiet)
    }
}

/// Why a watch compile gave no output. Either way the file counts as failed, and the
/// session keeps watching.
#[derive(Debug)]
enum CompileFailure {
    /// The compile's error, or the refusal of a path that no longer leads to the watched
    /// file — still to be reported.
    Error(miette::Report),
    /// The compile panicked. The panic hook has reported it, and the session exits 101
    /// when it stops (#389).
    Panicked,
}

impl CompileFailure {
    /// The error to report: none for a panic, which the panic hook reported.
    fn unreported(self) -> Option<miette::Report> {
        match self {
            Self::Error(e) => Some(e),
            Self::Panicked => None,
        }
    }
}

impl From<miette::Report> for CompileFailure {
    fn from(e: miette::Report) -> Self {
        Self::Error(e)
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

    // #265, #390: refuse a hostile output location before anything is read or compiled.
    let output =
        crate::build::reject_forbidden_output_flags(output.as_deref(), out_dir.as_deref())?;

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

    // Clamp poll_interval: 0 = disable; nonzero ≥ 50ms floor (reconcile rule).
    let tick_opt: Option<Duration> = clamp_poll_interval(poll_interval);

    let session_args = SessionArgs {
        out_dir,
        vars,
        set_vars,
        set_string_vars,
        clear,
        debounce_ms: debounce,
        quiet,
        tick: tick_opt,
    };

    if is_dir {
        // #413: the one directory-argument check every directory-mode subcommand makes
        // (a symlink, the filesystem root, a forbidden character — all `mds::io`). It
        // takes `.`, `..` and `sub/..`, which `check_symlink` cannot, and returns the
        // canonical form notify reports event paths under.
        let canonical = crate::input::resolve_directory_argument(&resolved_input)
            .map_err(miette::Error::from)?;
        run_watch_dir(
            WatchedPath {
                typed: resolved_input,
                canonical,
                what: Watched::Root,
            },
            session_args,
        )
    } else {
        // Reject a symlinked entry (build parity — PF-004); plain canonicalize would
        // silently follow it. check_symlink returns the canonical path for
        // non-symlinks, preserving FSEvents path-matching.
        let canonical =
            mds::NativeFs::check_symlink(&resolved_input).map_err(miette::Error::from)?;
        run_watch_file(
            WatchedPath {
                typed: resolved_input,
                canonical,
                what: Watched::Entry,
            },
            output,
            session_args,
        )
    }
}

/// What a watch session runs with besides what it watches, as `mds watch`'s options give
/// it (#256).
struct SessionArgs {
    /// `--out-dir`.
    out_dir: Option<PathBuf>,
    /// `--vars`, as typed.
    vars: Option<PathBuf>,
    /// `--set`'s values.
    set_vars: Vec<(String, String)>,
    /// `--set-string`'s values.
    set_string_vars: Vec<(String, String)>,
    /// `--clear`.
    clear: bool,
    /// `--debounce`, in milliseconds.
    debounce_ms: u64,
    /// `--quiet`.
    quiet: bool,
    /// The idle tick's interval; `None` when `--poll-interval 0` turned it off.
    tick: Option<Duration>,
}

// ── Live session ──────────────────────────────────────────────────────────────

/// The live half of a watch session (#256): going live, the watch loop, and the session's
/// last status line. A mode's startup hands [`live::run_session`] the channel its watcher
/// sends on and a [`live::Session`] — what a tick and a message do — once every watch it
/// can arm is armed and every baseline taken. Going live is reached only through
/// [`live::run_session`], which runs the loop straight after it.
mod live {
    use std::ops::ControlFlow;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::{emit_ready_marker, stop_watching, Msg, StopReason, TickClock, Wake};

    /// What a live watch session does when the loop wakes it: each says whether the
    /// session goes on or stops, and why.
    pub(super) trait Session {
        /// Whether the session runs under `--quiet`, which leaves out its last status line.
        fn is_quiet(&self) -> bool;

        /// When the rebuild the session holds while a watched file is empty runs anyway, if
        /// it holds one (#380): the loop wakes it there ([`Self::on_hold_due`]).
        fn hold_deadline(&self) -> Option<Instant>;

        /// The idle tick came due ([`TickClock`]): the liveness probe (reconcile rule).
        fn on_tick(&mut self) -> ControlFlow<StopReason>;

        /// The held rebuild's deadline came (#380): rebuild, compiling the files as they
        /// are. The rebuild ends the hold — the loop wakes for a deadline that has come at
        /// once, so one left standing would never let it wait.
        fn on_hold_due(&mut self) -> ControlFlow<StopReason>;

        /// A message arrived — a filesystem event, or Ctrl+C. `rx` is the channel it came
        /// on, which a debounce window drains.
        fn on_message(&mut self, msg: Msg, rx: &mpsc::Receiver<Msg>) -> ControlFlow<StopReason>;
    }

    /// Go live ([`go_live`]), watch until `session` stops, then print its last status line
    /// ([`stop_watching`]). `tx` is the sender the watcher was armed with, kept until the
    /// loop has ended.
    pub(super) fn run_session(
        tx: mpsc::Sender<Msg>,
        rx: mpsc::Receiver<Msg>,
        tick: Option<Duration>,
        mut session: impl Session,
    ) {
        go_live(&tx);
        let why = watch_loop(&rx, tick, &mut session);
        stop_watching(session.is_quiet(), why);
    }

    /// The session is live: Ctrl+C is wired, every watched directory is armed and every
    /// baseline captured.
    ///
    /// From here on a rebuild's output failure is reported as it happens and never changes
    /// how the session exits: the exit funnel applies its watch-session rule (#157). Only
    /// then is the readiness marker written, so a test that sees the marker sees a live
    /// session.
    fn go_live(tx: &mpsc::Sender<Msg>) {
        // ── Ctrl+C: install LAST, immediately before the loop that can service it ──
        //
        // Installing a handler converts SIGINT from "terminate now" into "enqueue
        // `Msg::Interrupt`", and that message is only ever read by the watch loop. So
        // every instruction between `set_handler` and the loop is a stretch of startup
        // during which Ctrl+C does nothing at all — the process keeps compiling and keeps
        // writing output, then exits 0 as if the user had never pressed it. Repeat
        // presses do not help; only SIGKILL does. The cost scales with the size of the
        // startup compile, so the handler is installed once startup is done, with nothing
        // but the loop after it. Nothing before it needs the handler: arming the watcher
        // only needs the sender, which is cloned here just as well.
        let tx_ctrlc = tx.clone();
        let _ = ctrlc::set_handler(move || {
            crate::output::panic_in_handler("ctrlc");
            let _ = tx_ctrlc.send(Msg::Interrupt);
        });

        // Every dir is armed and every baseline captured — the watch is now live.
        crate::output::note_watch_session_live();
        emit_ready_marker();
    }

    /// The watch loop: one event batch, one idle tick, or one held rebuild's deadline at a
    /// time. It is bounded: it ends on Ctrl+C, on the channel closing, or when a wake ends
    /// the session (stdout's reader gone), and returns why.
    fn watch_loop(
        rx: &mpsc::Receiver<Msg>,
        tick: Option<Duration>,
        session: &mut impl Session,
    ) -> StopReason {
        let mut clock = TickClock::new(tick);
        loop {
            let next = match clock.recv_next(rx, session.hold_deadline()) {
                Err(mpsc::RecvError) => break StopReason::Interrupted,
                Ok(Wake::Tick) => session.on_tick(),
                Ok(Wake::HoldDue) => session.on_hold_due(),
                Ok(Wake::Message(msg)) => session.on_message(msg, rx),
            };
            if let ControlFlow::Break(why) = next {
                break why;
            }
        }
    }
}

// ── Single-file watch ─────────────────────────────────────────────────────────

/// What [`compile_and_write`] returns on success, `(output_path, deps, content)`:
/// - `output_path`: the resolved output, written and shown (None for stdout).
/// - `deps`: transitive dependency paths, as graph keys ([`graph_keys`]).
/// - `content`: the compiled string (issue 3 — reused by the watch baseline block
///   so startup does not compile twice).
type WrittenEntry = (Option<WriteTarget>, Vec<PathBuf>, String);

/// Outcome of [`compile_and_write`]'s compile-and-write attempt, for an output route every
/// rebuild can use.
enum CompileWriteOutcome {
    /// Compiled, routed and written.
    Written(WrittenEntry),
    /// The compile failed, so the output's kind is unknown — and with it the route an
    /// output of that kind takes. `mds watch` keeps watching. `Some` is the failure to
    /// report; `None` a compile that panicked, which the panic hook reported (#389).
    CompileFailed(Option<miette::Report>),
    /// Compiled and routed, but not written. The dependencies the compile reported stay
    /// the session's (#257): every rebuild is triggered by them. `mds watch` keeps
    /// watching. `failure`: `Some` to report, naming the route of the compiled kind;
    /// `None` a repeat of a stdout failure reported already ([`OutputWrite::Failed`]).
    WriteFailed {
        /// Transitive dependency paths, as graph keys ([`graph_keys`]).
        deps: Vec<PathBuf>,
        failure: Option<miette::Report>,
    },
    /// `-o -` and stdout's reader is gone: `mds watch` stops (#157).
    StdoutClosed,
}

/// What one write of a watch session's output did (#157).
#[derive(Debug)]
#[must_use]
enum OutputWrite {
    /// Written in full.
    Written,
    /// Not written. `Some` is the failure to report: an output file that could not be
    /// written, or a new stdout failure (`mds::io`, naming stdout) — the session's first,
    /// or the first since a stdout write last landed. `None` is a repeat of that stdout
    /// failure, which is not reported again. Either way the content-dedup map must not
    /// record the content, so the next rebuild writes again even when its output has not
    /// changed.
    Failed(Option<miette::Report>),
    /// `-o -` and stdout's reader is gone: the session ends.
    StdoutClosed,
}

impl OutputWrite {
    /// The session's reading of one `-o -` write.
    ///
    /// Not [`StdoutOutcome::into_batch_result`]: a batch run takes a closed pipe and a
    /// repeated failure for success and finishes, but a session must stop on the first
    /// and must not record the second as written.
    fn from_stdout(outcome: StdoutOutcome) -> Self {
        match outcome {
            StdoutOutcome::Written => Self::Written,
            StdoutOutcome::Closed => Self::StdoutClosed,
            StdoutOutcome::Failed(e) => Self::Failed(Some(miette::Report::new(stdout_failure(&e)))),
            StdoutOutcome::FailedAgain => Self::Failed(None),
        }
    }
}

/// The path a session's output is written to; `None` for stdout.
fn written_path(output: &Option<WriteTarget>) -> Option<&Path> {
    output.as_ref().map(|target| target.path.as_path())
}

/// The session's output as a status line names it: the file by its shown form (#390), or
/// `<stdout>`.
fn shown_output(output: &Option<WriteTarget>) -> &Path {
    output
        .as_ref()
        .map_or(Path::new("<stdout>"), |target| target.shown.as_path())
}

/// Whether writing `after` empties an output: what the session last wrote there for the
/// same source (`before`) had bytes, and `after` has none (#380). An output the session
/// never wrote there, or wrote empty, is not emptied by an empty write.
fn empties_output(before: Option<&str>, after: &str) -> bool {
    after.is_empty() && before.is_some_and(|before| !before.is_empty())
}

/// Announce a rebuild's output, `shown` as its `Recompiled` line names it, when the write
/// `emptied` it ([`empties_output`], #380) — as a held rebuild whose source stayed empty
/// past its deadline does. Nothing under `--quiet`, as for every watch status line.
fn announce_emptied_output(shown: &Path, emptied: bool, quiet: bool) {
    if emptied && !quiet {
        crate::output::ewriteln!("Wrote an empty output: {}", safe_path(shown));
    }
}

/// Write `content` where the session writes: the output file, through [`write_output`]
/// (`announce` prints its `Compiled to` line) and never over one of `inputs`, the files
/// its compile read (#425), or stdout for `-o -` (`output_path` is `None`).
fn write_session_output(
    output_path: Option<&WriteTarget>,
    content: &str,
    inputs: &Inputs,
    quiet: bool,
    announce: bool,
) -> OutputWrite {
    match output_path {
        Some(target) => match write_output(Some(target), content, inputs, quiet, announce) {
            Ok(()) => OutputWrite::Written,
            Err(e) => OutputWrite::Failed(Some(e)),
        },
        None => OutputWrite::from_stdout(write_stdout(content.as_bytes())),
    }
}

/// Compile `entry` ([`WatchedPath::compile`]), derive the output path from the compiled
/// kind and the entry's two forms — written beside `entry.canonical`, named beside
/// `entry.typed` (#390) — and write: file mode's startup compile.
///
/// Returns a [`CompileWriteOutcome`]. `CompileFailed` and `WriteFailed` are failures
/// `mds watch` reports and keeps watching through — a failed write still carries the
/// dependencies its compile reported (#257); `StdoutClosed` ends the
/// session before it goes live, with exit 0 (#157). The `Err` this function
/// itself returns is an output route no rebuild can use, which ends `mds watch` at
/// startup (exit 2), since every rebuild writes where startup resolved: a route that
/// fails to resolve — `mds.json` `build.output_dir` with a `..` component, refused with
/// the error `mds build` gives for it — or an output that is the entry file itself
/// ([`admit_output`], #425).
///
/// The output path is derived AFTER compiling (compile-then-route) so the kind
/// (and thus extension: `.json` for messages, `.md` for markdown) is known before
/// the path is constructed. This is the single-file intrinsic extension path.
///
/// If `-o <path>` is given explicitly, that path is used verbatim, and once
/// [`admit_output`] has admitted it an ext-mismatch warning is emitted when the
/// extension contradicts the kind (AC-FUNC-11). If `-o -`, content is written to stdout.
/// No source map is written: `mds watch` emits none. The output is never written over a
/// file the compile read — the entry, a module it imported, or one of `reads`, the
/// `--vars` file and the `mds.json` in force (#425); such a write fails as any other.
///
/// # PF-004 compliance
/// All file reads go through `compile_to_content` → `mds::compile_with_deps_opts`
/// (which uses the resolver that enforces MAX_FILE_SIZE). There is no bare
/// `std::fs::read_to_string` path here.
fn compile_and_write(
    entry: &WatchedPath,
    output: &Option<String>,
    out_dir: &Option<PathBuf>,
    config: &Option<ProjectConfig>,
    reads: &[PathBuf],
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<CompileWriteOutcome> {
    let compiled = match entry.compile(runtime_vars, quiet) {
        Ok(compiled) => compiled,
        Err(failure) => return Ok(CompileWriteOutcome::CompileFailed(failure.unreported())),
    };
    let output_path =
        resolve_output_path_for_kind(Some(entry.paths()), output, out_dir, config, compiled.kind)?;
    admit_output(
        written_path(&output_path),
        entry.paths(),
        output,
        compiled.kind,
        quiet,
    )
    .map_err(miette::Error::from)?;
    let inputs = compile_inputs(Some(&entry.canonical), &compiled.dependencies, reads);
    Ok(
        match write_session_output(
            output_path.as_ref(),
            &compiled.content,
            &inputs,
            quiet,
            true,
        ) {
            OutputWrite::Written => CompileWriteOutcome::Written((
                output_path,
                graph_keys(&compiled.dependencies),
                compiled.content,
            )),
            OutputWrite::Failed(failure) => CompileWriteOutcome::WriteFailed {
                deps: graph_keys(&compiled.dependencies),
                failure,
            },
            OutputWrite::StdoutClosed => CompileWriteOutcome::StdoutClosed,
        },
    )
}

/// Compile-time context for single-file watch mode.
///
/// Holds the parameters that are resolved once at startup and passed to every
/// rebuild — replaces the 6-7 individual constant args on `rebuild_file` and
/// `liveness_probe_file`, removing the `#[allow(clippy::too_many_arguments)]`
/// suppressions (issue #6 / zero-warnings policy).
struct FileCompileCtx {
    /// The watched entry, as typed and canonical ([`Watched::Entry`]).
    entry: WatchedPath,
    /// The working directory at startup, restored first by every rebuild.
    working_dir: WorkingDir,
    /// Canonicalized `--vars` path — matches notify's canonicalized event paths;
    /// used for `dirs_to_watch`/`files_of_interest` (never for display, #326).
    vars_path: Option<PathBuf>,
    /// The `--vars` path exactly as the user typed it, uncanonicalized (#326, D4).
    /// Used for `RuntimeVarArgs.vars` so the vars-file duplicate-key warning displays
    /// (and reads) the as-typed path rather than its canonical form.
    vars_path_typed: Option<PathBuf>,
    /// The files every compile reads besides the entry's own — the `--vars` file and the
    /// `mds.json` in force — which no output is written over (#425).
    reads: Vec<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    static_set_string_vars: Vec<(String, String)>,
    quiet: bool,
}

/// What file mode's content-dedup map is keyed by: stdout, or the output file by the
/// path it is written to (#390).
///
/// A path, never its text: the text is lossy for a name that is not UTF-8, so two outputs
/// whose names differ only there would share one key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum OutputKey {
    Stdout,
    File(PathBuf),
}

impl OutputKey {
    /// The key of the session's output: `None` is stdout (`-o -`).
    fn of(output: Option<&WriteTarget>) -> Self {
        match output {
            Some(target) => Self::File(target.path.clone()),
            None => Self::Stdout,
        }
    }
}

/// Where file mode writes its output, resolved at startup (#257). A route that fails to
/// resolve never gets here: it ends `mds watch` at startup.
#[derive(Debug, PartialEq, Eq)]
enum OutputRoute {
    /// The path an explicit `-o` names: every output is written there, whatever its
    /// kind. `None` is stdout (`-o -`).
    Named(Option<WriteTarget>),
    /// No `-o`: the route of each kind, resolved at startup, and every output takes its
    /// own kind's — after a startup compile that failed, and when an edit changes the
    /// kind (#257, #160).
    ByKind {
        markdown: Option<WriteTarget>,
        messages: Option<WriteTarget>,
    },
}

impl OutputRoute {
    /// The route an output of `kind` takes: the named one whatever the kind, or else
    /// that kind's.
    fn of(&self, kind: OutputKind) -> &Option<WriteTarget> {
        match self {
            Self::Named(route) => route,
            Self::ByKind { markdown, messages } => match kind {
                OutputKind::Markdown => markdown,
                OutputKind::Messages => messages,
            },
        }
    }

    /// The route an output of `kind` no longer takes once it is written: the other
    /// kind's — whose output a change of kind leaves behind (#160) — and none for a
    /// named route, which every kind takes.
    fn other_than(&self, kind: OutputKind) -> Option<&WriteTarget> {
        match self {
            Self::Named(_) => None,
            Self::ByKind { markdown, messages } => match kind {
                OutputKind::Markdown => messages.as_ref(),
                OutputKind::Messages => markdown.as_ref(),
            },
        }
    }
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
    /// ≈ O(0) rather than O(watched_dirs) (reconcile rule / issue #1).
    armed_dirs: BTreeSet<PathBuf>,
    /// Set of paths relevant to the current build (entry + deps + vars).
    foi: HashSet<PathBuf>,
    /// Snapshot of `(mtime, size)` used by the liveness probe (reconcile rule), and by a
    /// rebuild to tell a file of interest emptied since (#380).
    last_mtimes: StampMap,
    /// The rebuild held while a file of interest is empty (#380).
    hold: EmptyHold,
    /// What this session last wrote, by where it wrote it: what a later write may skip as
    /// unchanged, and the bytes a removal or a write after a change of kind asks the file
    /// to still hold (#160). An entry goes with its file, so a file kept — changed, or a
    /// removal that failed — stays the session's.
    last_written: HashMap<OutputKey, String>,
    /// Where this session last wrote its output, if it has: a rebuild whose route is
    /// another — a change of kind — writes only where nothing is, or over its own file
    /// ([`write_after_change_of_kind`], #160).
    written_to: Option<OutputKey>,
    /// What the last rebuild kept from being written after a change of kind, the file
    /// there not being the session's (#160): the next rebuild of the same — another event
    /// of the same save — tries again but tells it no more. Any other rebuild clears it.
    kept: Option<String>,
    /// Where every rebuild writes ([`OutputRoute::of`]), and the output a change of kind
    /// leaves behind ([`OutputRoute::other_than`]).
    output: OutputRoute,
    /// The out-dir the output is written below, checked before every write; `None` for
    /// `-o` and for an output beside the entry.
    out_dir: Option<OutDirAnchor>,
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

/// Run the idle-tick liveness probe for single-file mode (reconcile rule).
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
    // 1. Re-arm watches for dirs that need attention (reconcile rule idle-O(1) fix).
    //    A dir "needs attention" if it was previously missing OR not yet armed.
    //    Already-armed, currently-present dirs are not touched — steady-state idle
    //    cost becomes O(missing_dirs) ≈ O(0), not O(watched_dirs).
    let desired_dirs: BTreeSet<PathBuf> =
        dirs_to_watch(&ctx.entry.canonical, &[], ctx.vars_path.as_deref())
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
    // Edge-triggered recovery (reconcile rule): mirrors external_recovery_decision used in
    // dir mode — a dir that STAYS missing must not trigger recovery every tick.
    let (dirs_recovery, now_missing_dirs) =
        external_recovery_decision(&state.missing_watched_dirs, &dir_statuses);
    state.missing_watched_dirs = now_missing_dirs;

    // 2. Determine if we need a full reconcile:
    //    (a) first tick, (b) edge-triggered dir recovery,
    //    (c) entry was missing and now exists (vanish→reappear edge).
    let entry_now_exists = ctx.entry.canonical.exists();
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
            eprint_warning(&format!(
                "warning: watch error: {}",
                safe_inline(notify_cause(&e))
            ));
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
    // The drained paths are discarded: file mode has already decided relevance above
    // and rebuilds its single entry regardless of which path moved.
    if drain_debounce(rx, debounce_ms).interrupted() {
        return FileEventAction::Stop;
    }

    // Clear terminal if requested (only when stderr is a TTY).
    if clear {
        clear_terminal();
    }

    FileEventAction::Rebuild
}

/// Compile `entry`, resync watches, compare with last-written content, and write
/// if changed.  Called from the idle-tick, the held-rebuild and the FS-event arms of file
/// mode's live session (`file_startup::FileSession`'s `on_tick`, `on_hold_due` and
/// `on_message`) — the single canonical implementation of the
/// hold→compile→resync→route→dedup→write→settle sequence for single-file mode.
///
/// `ctx` holds compile-time constants; `state` holds all mutable loop state;
/// `watcher` is passed separately (non-Clone, distinct lifecycle role).
///
/// Returns `Break` when the session must stop: `-o -` found stdout's reader gone
/// (#157). Every other outcome, failures included, keeps watching.
///
/// # Holding while a file is empty (#380)
///
/// Before anything is read, a file of interest — the entry, a dependency, the `--vars`
/// file — found empty where the baseline saw bytes holds the rebuild back: nothing is
/// compiled, written or printed, and the baseline is left as it was ([`Settle::Defer`]),
/// so the next event or tick finds the file again. The hold ends at the first rebuild that
/// finds none emptied, or at its deadline ([`EmptyHold`]), when the files are compiled as
/// they are; an output published empty over the non-empty one before it is announced
/// ([`announce_emptied_output`]). A file the compile read found emptied after that look —
/// a truncation that began between the two — holds the rebuild the same way, unless the
/// deadline ended the hold in it ([`EmptyHold::on_late_empty`]). `due` is the hold's
/// deadline when that deadline runs this rebuild, which then ends the hold
/// ([`not_before`]); `None` for an event or a tick.
///
/// # Invariants preserved
/// - Freshness rule: `foi` and `watched_dirs` always recomputed from fresh dep output, by
///   every compile that succeeds — its output refused or not written included (#257).
/// - PF-004: all reads go through `compile_to_content`.
/// - Error-settle: every failure — the vars file, the compile, the output route or its
///   #425 refusal, the write — goes through [`settle`], except a repeated stdout failure,
///   which was reported already. A compile that panicked is settled the same way; the
///   panic hook was its report (#389).
/// - `last_written` records only content that was written, so a rebuild after a failed
///   write writes again even when its output has not changed (#157).
/// - A recreated working directory is restored before anything is read
///   ([`WorkingDir::restore_if_recreated`]).
fn rebuild_file(
    ctx: &FileCompileCtx,
    watcher: &mut RecommendedWatcher,
    state: &mut FileWatchState,
    due: Option<Instant>,
) -> ControlFlow<StopReason> {
    ctx.working_dir.restore_if_recreated();
    // #380: a file of interest emptied holds the rebuild back — before `kept` is taken,
    // since a rebuild held is no rebuild.
    let emptied = any_went_empty(&state.foi, &state.last_mtimes);
    let verdict = state
        .hold
        .on_rebuild(emptied, not_before(Instant::now(), due));
    debug_assert!(
        due.is_none() || verdict == HoldVerdict::Compile,
        "a held rebuild's deadline ends its hold"
    );
    if verdict == HoldVerdict::Hold {
        settle(SettleInto::File(state), None, Settle::Defer);
        return ControlFlow::Continue(());
    }
    // What the rebuild before this one kept from being written (#160): told again unless
    // this rebuild keeps the same once more, as another event of the same save does.
    let kept_before = state.kept.take();

    // Soft-error: vars file may be temporarily absent (AC-W7 / AC-C5).
    // Print the error, settle mtime to avoid re-fire, and keep watching.
    //
    // The vars-file duplicate-key warnings (#326) are NOT emitted here
    // unconditionally: `rebuild_file` is called both from a genuine fs-event
    // rebuild AND from the liveness probe's unconditional-on-first-tick
    // self-heal recompile (a documented "worst case: one redundant compile"
    // that normally dedups to no write, see the comment above this function).
    // Emitting here would double-report the same duplicate once per session on
    // every startup. Instead the resolved vars are held and the warning is
    // emitted below, gated on `content_changed` — the same signal that gates
    // the "Recompiled" line — so the vars-file duplicate is re-reported exactly
    // once per OBSERVABLE rebuild (tests I16, I18, I20).
    let mut resolved = match build_runtime_vars(RuntimeVarArgs {
        vars: ctx.vars_path_typed.clone(),
        set_vars: ctx.static_set_vars.clone(),
        set_string_vars: ctx.static_set_string_vars.clone(),
    }) {
        Ok(v) => v,
        Err(e) => {
            settle(SettleInto::File(state), Some(e), Settle::Rebaseline);
            return ControlFlow::Continue(());
        }
    };
    // Move the map out instead of cloning it: `compile_to_content` takes
    // `runtime_vars` by value, and the emitter below only ever reads
    // `resolved.vars_file` / `duplicate_vars_file_keys` /
    // `duplicate_vars_file_keys_omitted` — none of which need `.vars`.
    let runtime_vars = resolved.vars.take();

    let t0 = Instant::now();
    let entry = &ctx.entry;
    let compiled = match entry.compile(runtime_vars, ctx.quiet) {
        Ok(compiled) => compiled,
        Err(failure) => {
            settle(
                SettleInto::File(state),
                failure.unreported(),
                Settle::MarkErrored(&entry.canonical),
            );
            return ControlFlow::Continue(());
        }
    };

    let deps = graph_keys(&compiled.dependencies);
    let foi = files_of_interest(&entry.canonical, &deps, ctx.vars_path.as_deref());
    // #380: a file the compile read, emptied since the look above — a truncation that began
    // between the two — holds the rebuild back as the look would have, with nothing
    // recorded, unless the deadline ended the hold in this rebuild.
    if any_went_empty(&foi, &state.last_mtimes)
        && state.hold.on_late_empty(Instant::now()) == HoldVerdict::Hold
    {
        state.kept = kept_before;
        settle(SettleInto::File(state), None, Settle::Defer);
        return ControlFlow::Continue(());
    }

    // Freshness rule: always recompute dep set from fresh output — before the output's
    // route is admitted, so the files a compile read are watched even when its output is
    // refused, a dependency in a directory no earlier compile reported included (#257).
    let new_dirs = dirs_to_watch(&entry.canonical, &deps, ctx.vars_path.as_deref());
    state.watched_dirs = resync_watches(
        watcher,
        &state.watched_dirs,
        &new_dirs,
        entry.dir_paths(),
        vars_dir_paths(ctx.vars_path.as_deref(), ctx.vars_path_typed.as_deref()),
    );
    // Keep armed_dirs in sync: all dirs in watched_dirs are successfully armed;
    // dirs removed by resync_watches are no longer in watched_dirs.
    state.armed_dirs = state.watched_dirs.clone();
    state.foi = foi;
    // Update mtime snapshot after a compile (even if content unchanged).
    state.last_mtimes = snapshot_state(&state.foi);

    // The compiled kind's route, re-decided by every rebuild: one whose kind changed is
    // written to that kind's output (#257) — only where nothing is, or over the file this
    // session wrote there (#160).
    let output_path = state.output.of(compiled.kind).clone();
    // #425: a rebuild never writes over the entry — reachable after a failed startup
    // compile, which refuses no route, and after a change of kind: the route of the
    // compiled kind, or an explicit `-o`, can be the entry. A refused route writes
    // nothing, and is reported and settled as a failed compile is; the next rebuild
    // routes again. No `-o` extension warning (`&None`): startup printed it for the path
    // an explicit `-o` names, which every rebuild reuses.
    if let Err(refused) = admit_output(
        written_path(&output_path),
        entry.paths(),
        &None,
        compiled.kind,
        ctx.quiet,
    ) {
        settle(
            SettleInto::File(state),
            Some(miette::Report::from(refused)),
            Settle::MarkErrored(&entry.canonical),
        );
        return ControlFlow::Continue(());
    }

    // The content-dedup key: where the output is written.
    let output_key = OutputKey::of(output_path.as_ref());

    let out_dir = check_out_dir(state.out_dir.as_mut(), &mut state.last_written);

    // A change of kind (#160): the route is not the one this session last wrote to, so
    // the file there is written over only if it is the session's own.
    let kind_changed = state
        .written_to
        .as_ref()
        .is_some_and(|key| *key != output_key);
    // Content-based dedup: skip write + summary line when unchanged — never after a change
    // of kind, whose write is decided by the file there whatever the record of it holds.
    let content_changed = kind_changed
        || state
            .last_written
            .get(&output_key)
            .is_none_or(|prev| *prev != compiled.content);

    // #326: re-report the vars-file duplicate-key warnings exactly when an
    // observable rebuild happens (same gate as the "Recompiled" line below),
    // not on the liveness probe's redundant no-op recompile.
    if content_changed {
        crate::build::emit_duplicate_vars_file_warnings(&resolved, ctx.quiet);
    }

    if !content_changed {
        return ControlFlow::Continue(());
    }
    let written = match (out_dir, output_path.as_ref()) {
        (OutDirNow::Elsewhere, Some(target)) => OutputWrite::Failed(Some(miette::Report::new(
            crate::write::out_dir_moved(target),
        ))),
        (_, Some(target)) if kind_changed => match write_after_change_of_kind(
            &below_checked_out_dir(state.out_dir.as_ref(), target),
            Record::of_entry(state.last_written.get(&output_key)),
            &compiled.content,
            kept_before.as_ref() == Some(&compiled.content),
            ctx.quiet,
        ) {
            Ok(true) => OutputWrite::Written,
            // Kept: nothing is written and nothing retired, and `last_written` is left as
            // it was, so a later save tries again.
            Ok(false) => {
                state.kept = Some(compiled.content);
                return ControlFlow::Continue(());
            }
            Err(e) => OutputWrite::Failed(Some(miette::Report::new(e))),
        },
        (_, target) => write_session_output(
            target
                .map(|target| below_checked_out_dir(state.out_dir.as_ref(), target))
                .as_ref(),
            &compiled.content,
            &compile_inputs(
                Some(&ctx.entry.canonical),
                &compiled.dependencies,
                &ctx.reads,
            ),
            ctx.quiet,
            false,
        ),
    };
    match written {
        OutputWrite::Written => {
            let elapsed = t0.elapsed().as_millis();
            let dep_count = deps.len();
            if !ctx.quiet {
                crate::output::ewriteln!(
                    "Recompiled {} ({} deps) in {}ms",
                    safe_path(shown_output(&output_path)),
                    dep_count,
                    elapsed
                );
            }
            announce_emptied_output(
                shown_output(&output_path),
                empties_output(
                    state.last_written.get(&output_key).map(String::as_str),
                    &compiled.content,
                ),
                ctx.quiet,
            );
            state
                .last_written
                .insert(output_key.clone(), compiled.content);
            state.written_to = Some(output_key);
            if let Some(anchor) = &mut state.out_dir {
                anchor.written();
            }
            // A change of kind (#160): the output this session last wrote, the other
            // kind's, is retired — removed only if the session wrote it and it holds what
            // was written, else kept with a notice. The record of it goes only with the
            // file, so one kept stays the session's, changed or restored.
            if let Some(stale) = state
                .output
                .other_than(compiled.kind)
                .filter(|_| kind_changed)
                .cloned()
            {
                let stale_key = OutputKey::of(Some(&stale));
                let gone = retire_output(
                    &stale,
                    state.out_dir.as_ref(),
                    out_dir,
                    Record::of_entry(state.last_written.get(&stale_key)),
                    Retirement::KindChanged,
                    ctx.quiet,
                );
                if gone {
                    state.last_written.remove(&stale_key);
                }
            }
        }
        // Not written: `last_written` keeps what was last written, so the next rebuild
        // writes again even when its output has not changed.
        OutputWrite::Failed(Some(e)) => settle(
            SettleInto::File(state),
            Some(e),
            Settle::MarkErrored(&ctx.entry.canonical),
        ),
        // A repeat of a stdout failure reported already (#157): neither reported nor
        // settled again.
        OutputWrite::Failed(None) => {}
        OutputWrite::StdoutClosed => return ControlFlow::Break(StopReason::StdoutClosed),
    }
    ControlFlow::Continue(())
}

// ── Settling a failure ────────────────────────────────────────────────────────

/// How `mds watch` settles a rebuild-time failure it keeps watching through (#257):
/// reading the vars file, a compile — a panic included (#389) — the output route, a
/// write; and a rebuild held back while a watched file is empty (#380), which is no
/// failure. The site picks the action and hands it to [`settle`] with the state to apply
/// it to ([`SettleInto`]). A startup failure settles through [`settle_startup_error`]
/// instead: there is no baseline yet to take again, so [`Settle::Rebaseline`] has nothing
/// to apply there, and no startup compile is ever held. The repeat of a stdout failure
/// reported already (#157) settles nothing, through neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settle<'a> {
    /// Take the `(mtime, size)` baseline again, so the idle tick does not fire again on
    /// files that have not changed since.
    Rebaseline,
    /// The source failed, and a later change must compile it again even when the source
    /// itself has not changed. Directory mode records it as errored — re-seeded into every
    /// batch that carries a real change — keeping the dependency set its last successful
    /// compile recorded (#321); the batch takes the baseline once, at its end. File mode
    /// compiles its one source again on every change anyway, so there it takes the
    /// baseline again, as [`Settle::Rebaseline`] does.
    MarkErrored(&'a Path),
    /// The rebuild was held back, a watched file being empty (#380): nothing ran, so there
    /// is nothing to report and nothing to record, and the baseline is left as it was —
    /// the next event or tick then finds the emptied file again, and the rebuild is held
    /// again or run.
    Defer,
}

/// The rebuild state a [`Settle`] is applied to.
enum SettleInto<'a> {
    /// A file-mode rebuild.
    File(&'a mut FileWatchState),
    /// A directory-mode rebuild.
    Dir(&'a mut DirWatchState),
}

/// Settle a rebuild-time failure `mds watch` keeps watching through (#257): report
/// `failure`, then apply `how` to `into`. `failure` is `None` when there is nothing to
/// report — a compile that panicked, which the panic hook reported (#389) — so a panic
/// settles exactly as an error at the same site does, reported once.
fn settle(into: SettleInto<'_>, failure: Option<miette::Report>, how: Settle<'_>) {
    settle_reporting(into, failure, how, eprint_error);
}

/// [`settle`], with `report` doing the reporting: the session's error renderer there, a
/// recorder in a test.
fn settle_reporting(
    into: SettleInto<'_>,
    failure: Option<miette::Report>,
    how: Settle<'_>,
    report: impl FnOnce(miette::Report),
) {
    if let Some(e) = failure {
        report(e);
    }
    match (into, how) {
        (SettleInto::File(state), Settle::Rebaseline | Settle::MarkErrored(_)) => {
            state.last_mtimes = snapshot_state(&state.foi);
        }
        (SettleInto::Dir(state), Settle::Rebaseline) => {
            state.last_mtimes = snapshot_state(&state.watched_set());
        }
        (SettleInto::Dir(state), Settle::MarkErrored(src)) => {
            state.record_error(src);
        }
        (SettleInto::File(_) | SettleInto::Dir(_), Settle::Defer) => {}
    }
}

/// The startup state a failure (#257) settles into: file mode's startup records nothing —
/// the session's first baseline is taken once the startup compile is done, the one
/// [`Settle::Rebaseline`] asks for in a rebuild; directory mode's startup records the
/// source as errored, keeping the dependency set its compile reported (empty for a source
/// new to the graph).
enum StartupInto<'a> {
    /// File mode's startup.
    File,
    /// Directory mode's startup.
    Dir(&'a mut DirWatchState),
}

/// Settle a startup failure `mds watch` keeps watching through (#257): report `failure`,
/// then mark `src` errored in directory mode. `failure` is `None` for a compile that
/// panicked, which the panic hook reported (#389). There is no [`Settle::Rebaseline`] at
/// startup — the baseline is still to come — so `StartupInto` takes no `how`: a startup
/// failure only ever means the source is errored.
fn settle_startup_error(into: StartupInto<'_>, failure: Option<miette::Report>, src: &Path) {
    settle_startup_error_reporting(into, failure, src, eprint_error);
}

/// [`settle_startup_error`], with `report` doing the reporting: the session's error
/// renderer there, a recorder in a test.
fn settle_startup_error_reporting(
    into: StartupInto<'_>,
    failure: Option<miette::Report>,
    src: &Path,
    report: impl FnOnce(miette::Report),
) {
    if let Some(e) = failure {
        report(e);
    }
    if let StartupInto::Dir(state) = into {
        state.record_error(src);
    }
}

/// A directory `mds watch` watches, as a message names it (#390). `root` is the directory
/// argument in directory mode, the entry's directory in file mode ([`RootPaths`]: the
/// canonical form watched, the form typed): it is named as typed, and a directory below
/// it — a dependency's — below it as typed. `vars` is the directory armed for the `--vars`
/// file, named by that file's directory as typed. Any other directory — a dependency's
/// outside both — has no typed form and is named by the path the compile reported.
fn shown_watched_dir(dir: &Path, root: RootPaths<'_>, vars: Option<RootPaths<'_>>) -> PathBuf {
    match vars {
        Some(vars) if dir == vars.walked && dir != root.walked => vars.typed.to_path_buf(),
        _ => root.typed_below(dir).unwrap_or_else(|| dir.to_path_buf()),
    }
}

/// The directory of file mode's `--vars` file in the two forms [`shown_watched_dir`]
/// takes: the canonical directory [`dirs_to_watch`] arms, and the file's directory as
/// typed.
fn vars_dir_paths<'a>(
    canonical: Option<&'a Path>,
    typed: Option<&'a Path>,
) -> Option<RootPaths<'a>> {
    canonical.zip(typed).map(|(canonical, typed)| RootPaths {
        typed: mds::effective_parent(typed),
        walked: mds::effective_parent(canonical),
    })
}

/// In directory mode, the `--vars` file's directory armed on its own, outside the root, in
/// the two forms a message names it by ([`shown_watched_dir`]): the canonical directory
/// armed, and the file's directory as typed.
fn extra_vars_dir<'a>(
    vars_dir_extra: Option<&'a Path>,
    vars_path_typed: Option<&'a Path>,
) -> Option<RootPaths<'a>> {
    vars_dir_extra
        .zip(vars_path_typed)
        .map(|(walked, typed)| RootPaths {
            typed: mds::effective_parent(typed),
            walked,
        })
}

/// Single-file watch: `entry.typed` is the path as typed — the entry is compiled by it
/// (#417), `mds.json` is looked up from it at startup (#413), and every status line
/// names the entry and its output by it (#390), so they name the files as the user
/// reaches them; `entry.canonical` is its canonical form, which everything else uses.
///
/// Startup runs in phases whose order the types enforce (#256, [`file_startup`]):
/// [`file_startup::arm_pre_read`] arms the watches and takes the baseline before anything
/// is read, [`file_startup::startup_compile`] compiles and writes once,
/// [`file_startup::arm_deps_and_seed`] arms the dependencies' directories and seeds what
/// every rebuild reads, and [`file_startup::Seeded::go_live`] goes live and watches until
/// the session stops.
fn run_watch_file(entry: WatchedPath, output: Option<String>, args: SessionArgs) -> Result<()> {
    let quiet = args.quiet;
    let armed = file_startup::arm_pre_read(entry, output, args)?;
    let compiled = match file_startup::startup_compile(armed)? {
        ControlFlow::Continue(compiled) => compiled,
        // stdout's reader is gone before the session went live: it stops here, and its
        // verdict is 0 — a closed pipe never changes the exit code (#157). The armed
        // watches it hands back are dropped after the stop line.
        ControlFlow::Break((why, _armed)) => {
            stop_watching(quiet, why);
            return Ok(());
        }
    };
    file_startup::arm_deps_and_seed(compiled)?.go_live();
    Ok(())
}

/// File mode's startup, in phases whose order the types enforce (#256).
///
/// Each phase takes the token only the phase before it makes, and every token's fields are
/// private to this module, so nothing outside it can make one: an
/// [`Armed`](file_startup::Armed) comes only from [`file_startup::arm_pre_read`], a
/// [`Compiled`](file_startup::Compiled) only from [`file_startup::startup_compile`] given an
/// `Armed`, a [`Seeded`](file_startup::Seeded) only from [`file_startup::arm_deps_and_seed`]
/// given a `Compiled`, and the session goes live only from a `Seeded`
/// ([`file_startup::Seeded::go_live`]). That is the order the session's change detection
/// rests on: the directories of the entry and the `--vars` file armed, and their
/// `(mtime, size)` baseline taken, before either is read; a dependency's baseline taken and
/// its directory armed once the compile reports it; Ctrl+C wired last, with nothing but the
/// loop after it. The compile takes the `Armed` and carries it on inside the `Compiled`, so
/// one `Armed` is compiled once, and a `Compiled` is seeded with the very watches it was
/// compiled under.
mod file_startup {
    use std::collections::{BTreeSet, HashMap};
    use std::ops::ControlFlow;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use miette::Result;
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};

    use crate::build::{
        admit_output, build_runtime_vars, emit_duplicate_var_warnings, load_config,
        resolve_output_path_for_kind, run_reads, OutputKind, ProjectConfig, RuntimeVarArgs,
    };
    use crate::output::{notify_cause, safe_inline, safe_path, WriteTarget};

    use super::{
        baseline_path, canonicalize_vars_path, compile_and_write, dirs_to_watch, files_of_interest,
        handle_fs_event_file, live, liveness_probe_file, rebuild_file, settle_startup_error,
        shown_watched_dir, snapshot_state, startup_race_probe, vars_dir_paths, written_path,
        CompileWriteOutcome, EmptyHold, FileCompileCtx, FileEventAction, FileWatchState, Msg,
        OutDirAnchor, OutputKey, OutputRoute, SessionArgs, StampMap, StartupInto, StopReason,
        WatchedPath, WorkingDir,
    };

    /// The session's first watches armed and its first baseline taken, before anything is
    /// read: made only by [`arm_pre_read`].
    pub(super) struct Armed {
        /// The watched entry, as typed and canonical ([`super::Watched::Entry`]).
        entry: WatchedPath,
        /// `-o` as text; `None` without it.
        output: Option<String>,
        out_dir: Option<PathBuf>,
        /// The working directory at startup, which every rebuild restores first.
        working_dir: WorkingDir,
        /// The `--vars` file, canonical: what notify names its events by (#326).
        vars_path: Option<PathBuf>,
        /// The `--vars` file as typed, which it is read and named by (#326).
        vars_path_typed: Option<PathBuf>,
        static_set_vars: Vec<(String, String)>,
        static_set_string_vars: Vec<(String, String)>,
        quiet: bool,
        clear: bool,
        debounce_ms: u64,
        tick: Option<Duration>,
        /// The channel the watcher sends on — and Ctrl+C, once the session is live.
        tx: mpsc::Sender<Msg>,
        rx: mpsc::Receiver<Msg>,
        watcher: RecommendedWatcher,
        /// The directories armed so far: the entry's and the `--vars` file's that exist
        /// and could be armed.
        watched_dirs: BTreeSet<PathBuf>,
        /// The entry's and the `--vars` file's `(mtime, size)`, taken before either is read.
        pre_mtimes: StampMap,
        /// Whether the entry was missing before its first read.
        entry_was_missing: bool,
    }

    /// The startup compile done, and its output written when it could be: made only by
    /// [`startup_compile`].
    pub(super) struct Compiled {
        /// The watches and baseline the compile ran under, handed on to
        /// [`arm_deps_and_seed`] with what it produced.
        armed: Armed,
        /// The `mds.json` in force, looked up from the entry as typed.
        config: Option<ProjectConfig>,
        /// The files every compile reads besides the entry's own (#425).
        reads: Vec<PathBuf>,
        /// Where every output is written ([`OutputRoute`]).
        output_route: OutputRoute,
        /// What the startup wrote and where, if it wrote.
        initial_written: Option<(Option<WriteTarget>, String)>,
        /// The dependencies the startup compile reported, as graph keys.
        initial_deps: Vec<PathBuf>,
    }

    /// Every directory armed and every baseline taken, with what every rebuild reads: made
    /// only by [`arm_deps_and_seed`].
    pub(super) struct Seeded {
        /// The sender the watcher was armed with, which Ctrl+C is wired to.
        tx: mpsc::Sender<Msg>,
        rx: mpsc::Receiver<Msg>,
        tick: Option<Duration>,
        session: FileSession,
    }

    /// A file-mode session once live ([`live::Session`]): what every rebuild reads, the
    /// watcher, the loop state, and how messages are coalesced.
    struct FileSession {
        ctx: FileCompileCtx,
        watcher: RecommendedWatcher,
        state: FileWatchState,
        debounce_ms: u64,
        clear: bool,
    }

    /// Record the working directory, check the `--vars` file and say what is watched; then
    /// arm the directories of the entry and the `--vars` file and take their baseline,
    /// before either is read.
    pub(super) fn arm_pre_read(
        entry: WatchedPath,
        output: Option<String>,
        args: SessionArgs,
    ) -> Result<Armed> {
        // Build runtime vars from the set_vars statics (vars file is reloaded each rebuild).
        let SessionArgs {
            out_dir,
            vars,
            set_vars: static_set_vars,
            set_string_vars: static_set_string_vars,
            clear,
            debounce_ms,
            quiet,
            tick,
        } = args;
        let working_dir = WorkingDir::record();
        // #326: keep the --vars argument as the user typed it, separately from the
        // canonicalized form below. `vars_path` (canonical) is used for everything that
        // must match notify's canonicalized event paths (dirs_to_watch, files_of_interest,
        // event matching); `vars_path_typed` is used only for `RuntimeVarArgs.vars`, so
        // the vars-file duplicate-key warning ("{path} = the --vars arg as typed")
        // displays and reads through the same path the user gave — reading a valid,
        // possibly symlinked path is fine either way, only the DISPLAYED text differs.
        let vars_path_typed = vars.clone();
        // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on
        // macOS). Also rejects a symlinked vars file at startup (build parity).
        let vars_path = canonicalize_vars_path(vars).map_err(miette::Error::from)?;

        if !quiet {
            crate::output::ewriteln!("Watching {}", safe_path(&entry.typed));
        }

        // ── Arm before publish (startup race) ─────────────────────────────────
        //
        // GUARANTEED for the entry and the vars file: the directory watch is armed and
        // the `(mtime, size)` baseline captured strictly BEFORE either is first read.
        // Both are knowable from the command line, so both happen here, ahead of
        // `build_runtime_vars` (reads vars) and `compile_and_write` (reads the entry),
        // which [`startup_compile`] runs given the [`Armed`] this returns.
        //
        // NOT guaranteed for dependencies. A dep only becomes known when the compile
        // reports it, so a dep whose directory is not the entry's or the vars file's is
        // armed — and has its baseline taken — only *after* the compile has already read
        // it (see the post-compile arming loop and the baseline merge in
        // [`arm_deps_and_seed`]). An edit to such a dep inside that window is still
        // invisible to both detectors. Deps that happen to sit in an already-armed
        // directory are covered by the OS watch from the start; cross-directory deps are
        // the residual, and are what `MDS_TEST_READY` exists to let the integration suite
        // synchronise past.
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
        //   3. `last_written` content-dedup is seeded by [`arm_deps_and_seed`], before
        //      the event loop begins. Queued events are only *processed* inside the
        //      loop, so any event that survives guards 1 and 2 recompiles to identical
        //      content and is suppressed without a write or a status line.
        // Worst case is therefore one redundant compile that dedups to no write.
        let (tx, rx) = mpsc::channel::<Msg>();
        let tx_fs = tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                crate::output::panic_in_handler("notify");
                let _ = tx_fs.send(Msg::Fs(res));
            },
            notify::Config::default(),
        )
        .map_err(|e| {
            miette::miette!(
                "failed to initialize file watcher: {}",
                safe_inline(notify_cause(&e))
            )
        })?;

        // Arm the directories that are knowable before any read: the entry's parent
        // and the vars file's parent. Dependency dirs are unknown until the compile
        // reports them and are armed immediately afterwards.
        //
        // Best-effort here — a dir that is missing or fails to arm is re-attempted by
        // the post-compile loop in [`arm_deps_and_seed`], which owns the hard-error
        // contract for the full dir set. Splitting it this way keeps startup failure
        // messages identical to the pre-reorder behaviour.
        let mut watched_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        for dir in dirs_to_watch(&entry.canonical, &[], vars_path.as_deref()) {
            if dir.exists() && watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                watched_dirs.insert(dir);
            }
        }

        // Capture the entry/vars baseline BEFORE the first read of either. Both
        // `build_runtime_vars` (reads the vars file) and `compile_and_write` (reads
        // the entry) come after this point, so an edit landing during startup leaves
        // this snapshot strictly older than the file — and the liveness probe sees it.
        let pre_mtimes = snapshot_state(&files_of_interest(
            &entry.canonical,
            &[],
            vars_path.as_deref(),
        ));
        let entry_was_missing = !entry.canonical.exists();

        Ok(Armed {
            entry,
            output,
            out_dir,
            working_dir,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            quiet,
            clear,
            debounce_ms,
            tick,
            tx,
            rx,
            watcher,
            watched_dirs,
            pre_mtimes,
            entry_was_missing,
        })
    }

    /// The startup compile, once [`arm_pre_read`] has armed what it reads, and its write.
    /// A compile or write error is reported, and watching continues. `Break` when stdout's
    /// reader is gone: the session stops, and the `Armed` comes back with the reason so
    /// its watches are dropped where they were before, after the stop line.
    pub(super) fn startup_compile(
        armed: Armed,
    ) -> Result<ControlFlow<(StopReason, Armed), Compiled>> {
        let Armed {
            entry,
            output,
            out_dir,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            quiet,
            ..
        } = &armed;
        let quiet = *quiet;

        // Initial compile: compile first, derive output path from kind (compile-then-route).
        // For explicit -o / --out-dir the path is determined by the flag.
        // For the default case (no explicit flag), the path depends on the output kind,
        // which is only known after compilation — so we compile first, then derive.
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: vars_path_typed.clone(),
            set_vars: static_set_vars.clone(),
            set_string_vars: static_set_string_vars.clone(),
        })?;
        emit_duplicate_var_warnings(&resolved, quiet);
        let runtime_vars = resolved.vars;

        // Load project config (for output_dir) — used if no explicit -o / --out-dir. By
        // the typed path, so a config error names `mds.json` as the input reaches it
        // (`./mds.json`), never by its canonical absolute path (#413); the config
        // directory it returns is canonical either way.
        let config = load_config(&entry.typed)?;

        // Initial compile: returns (output_path, deps, content).
        // content is captured here so the baseline block in [`arm_deps_and_seed`] can
        // reuse it without recompiling (issue 3 — avoids a redundant second compile at
        // startup). The outer `?` is an output route no rebuild can use — one that fails
        // to resolve, or the entry file itself (#425): refused at startup, exit 2, before
        // anything is written. A compile or write error is reported, and watching
        // continues.
        // The files every compile reads besides the entry's own: the `--vars` file and the
        // `mds.json` in force, which no output is written over (#425).
        let reads = run_reads(vars_path.as_deref(), config.as_ref());
        let startup =
            compile_and_write(entry, output, out_dir, &config, &reads, runtime_vars, quiet)?;
        // The route each output takes: an explicit `-o` names one whatever the kind;
        // without it, the route of each kind is resolved now and every output takes its
        // own kind's (#257, #160) — after a startup compile that failed, whose kind is
        // unknown, and when an edit changes the kind — so a `.json` output is never
        // written to `.md`. A route that fails to resolve is refused here, exit 2: no
        // rebuild could write anywhere else. Every rebuild refuses and reports a route
        // that is the entry (#425).
        let route_of = |kind| {
            resolve_output_path_for_kind(Some(entry.paths()), output, out_dir, &config, kind)
        };
        // What the startup wrote and where, if it wrote, and the dependencies its compile
        // reported.
        let (initial_written, initial_deps) = match startup {
            CompileWriteOutcome::Written((output_path, deps, content)) => {
                (Some((output_path, content)), deps)
            }
            // stdout's reader is gone before the session went live: it stops, and its
            // verdict is 0 — a closed pipe never changes the exit code (#157).
            CompileWriteOutcome::StdoutClosed => {
                return Ok(ControlFlow::Break((StopReason::StdoutClosed, armed)));
            }
            // Compiled and routed, but not written: report it and keep watching, with the
            // dependencies the compile reported — an edit to one rebuilds (#257). Nothing
            // was written, so the dedup map stays empty and the next rebuild writes even
            // when its output has not changed.
            CompileWriteOutcome::WriteFailed { deps, failure } => {
                settle_startup_error(StartupInto::File, failure, &entry.canonical);
                (None, deps)
            }
            CompileWriteOutcome::CompileFailed(e) => {
                // Initial compile error: print and continue watching (entry dir still
                // watched). Nothing is written now.
                settle_startup_error(StartupInto::File, e, &entry.canonical);
                if output.is_some() {
                    // An explicit `-o` names the route whatever the kind. Its refusal is
                    // dropped here: admitting it only decides whether the `-o` extension
                    // warning, which announces a write, is printed — never for an output
                    // that is the entry.
                    let _ = admit_output(
                        written_path(&route_of(OutputKind::Markdown)?),
                        entry.paths(),
                        output,
                        OutputKind::Markdown,
                        quiet,
                    );
                }
                (None, vec![])
            }
        };
        let output_route = if output.is_some() {
            OutputRoute::Named(route_of(OutputKind::Markdown)?)
        } else {
            OutputRoute::ByKind {
                markdown: route_of(OutputKind::Markdown)?,
                messages: route_of(OutputKind::Messages)?,
            }
        };

        Ok(ControlFlow::Continue(Compiled {
            armed,
            config,
            reads,
            output_route,
            initial_written,
            initial_deps,
        }))
    }

    /// Take the baseline of the dependencies the startup compile reported and arm their
    /// directories — one that cannot be armed ends the session at startup — then seed what
    /// every rebuild reads: the content-dedup map, the files of interest and the merged
    /// baseline.
    pub(super) fn arm_deps_and_seed(compiled: Compiled) -> Result<Seeded> {
        let Compiled {
            armed,
            config,
            reads,
            output_route,
            initial_written,
            initial_deps,
        } = compiled;
        let Armed {
            entry,
            output,
            out_dir,
            working_dir,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            quiet,
            clear,
            debounce_ms,
            tick,
            tx,
            rx,
            mut watcher,
            mut watched_dirs,
            mut pre_mtimes,
            entry_was_missing,
        } = armed;

        // Baseline the dependencies the compile just reported, before anything else runs.
        //
        // The entry and vars baselines precede their own reads ([`arm_pre_read`]); a
        // dependency's cannot, because the compile is what discovers the dependency
        // exists. Taking it here rather than with the post-compile snapshot below shrinks
        // the window in which an edit to a dependency is invisible to the baseline from
        // "the rest of startup" to the gap between the compile returning and this loop.
        // `baseline_path` keeps the older of any two entries.
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
            baseline_path(dep, &mut pre_mtimes);
        }

        // The startup output is now published — the positive-control injection point.
        startup_race_probe();

        // Arm the dependency directories the compile just reported. Dirs already armed by
        // [`arm_pre_read`] are skipped; anything still unarmed — including a pre-arm
        // attempt that failed — is a hard startup error, as it was before the reorder.
        let init_dirs = dirs_to_watch(&entry.canonical, &initial_deps, vars_path.as_deref());
        let unarmed: Vec<PathBuf> = init_dirs.difference(&watched_dirs).cloned().collect();
        for dir in unarmed {
            match watcher.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched_dirs.insert(dir);
                }
                Err(e) => {
                    let vars = vars_dir_paths(vars_path.as_deref(), vars_path_typed.as_deref());
                    return Err(miette::miette!(
                        "failed to watch directory {}: {}\n\
                         hint: on Linux you may need to increase fs.inotify.max_user_watches",
                        safe_path(&shown_watched_dir(&dir, entry.dir_paths(), vars)),
                        safe_inline(notify_cause(&e))
                    ));
                }
            }
        }

        // Record the dedup baseline. The event loop has not started, so nothing can
        // consult this map before it is populated (guard 3 in [`arm_pre_read`]).
        // Reuse the content the startup wrote (issue 3 — no second compile needed), an
        // empty one included: what the startup wrote is the session's, as a rebuild's is
        // (#160). When the initial compile or write failed nothing was written:
        // last_written stays empty, so the next successful rebuild writes, and is no
        // change of kind.
        let mut last_written: HashMap<OutputKey, String> = HashMap::new();
        let written_to = initial_written.map(|(written, content)| {
            let key = OutputKey::of(written.as_ref());
            last_written.insert(key.clone(), content);
            key
        });

        let foi = files_of_interest(&entry.canonical, &initial_deps, vars_path.as_deref());

        // Build pre-loop FileWatchState (mtime snapshot + edge-trigger seeds).
        let missing_watched_dirs: BTreeSet<PathBuf> = {
            let desired = dirs_to_watch(&entry.canonical, &[], vars_path.as_deref())
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
        // still passes. `entry.canonical` is inserted verbatim by `files_of_interest`, so
        // this lookup hits. The previous assertion here compared the key sets of
        // `files_of_interest(entry, &[], vars)` and `files_of_interest(entry, &deps, vars)`
        // — a subset relation those two calls guarantee by construction, so it could
        // never fail and guarded nothing.
        let entry_pre = pre_mtimes.get(&entry.canonical).copied();
        last_mtimes.extend(pre_mtimes);
        debug_assert_eq!(
            last_mtimes.get(&entry.canonical).copied(),
            entry_pre,
            "baseline merge inverted: the entry's pre-compile (mtime, size) must survive \
             the merge with the post-compile snapshot, or an edit made during startup can \
             never register as a difference"
        );

        let state = FileWatchState {
            // armed_dirs mirrors watched_dirs at startup: all dirs that were successfully
            // registered in the loop above are considered armed (reconcile rule idle-O(1)
            // fix).
            armed_dirs: watched_dirs.clone(),
            watched_dirs,
            foi,
            last_mtimes,
            hold: EmptyHold::Off,
            last_written,
            written_to,
            kept: None,
            output: output_route,
            // After the startup write, so the directory it made is the one the first
            // rebuild compares; `-o` names its own path, which no out-dir is below.
            out_dir: if output.is_some() {
                None
            } else {
                OutDirAnchor::record(out_dir.as_deref(), config.as_ref())
            },
            entry_was_missing,
            first_tick: true,
            missing_watched_dirs,
        };

        // What every rebuild and liveness tick reads, fixed for the session.
        let ctx = FileCompileCtx {
            entry,
            working_dir,
            vars_path,
            vars_path_typed,
            reads,
            static_set_vars,
            static_set_string_vars,
            quiet,
        };

        Ok(Seeded {
            tx,
            rx,
            tick,
            session: FileSession {
                ctx,
                watcher,
                state,
                debounce_ms,
                clear,
            },
        })
    }

    impl Seeded {
        /// Every watch armed and every baseline taken: go live — Ctrl+C wired last — and
        /// watch until the session stops ([`live::run_session`]).
        pub(super) fn go_live(self) {
            let Seeded {
                tx,
                rx,
                tick,
                session,
            } = self;
            live::run_session(tx, rx, tick, session);
        }
    }

    impl live::Session for FileSession {
        fn is_quiet(&self) -> bool {
            self.ctx.quiet
        }

        fn hold_deadline(&self) -> Option<Instant> {
            self.state.hold.deadline()
        }

        fn on_tick(&mut self) -> ControlFlow<StopReason> {
            // Idle tick — run liveness probe (reconcile rule).
            if liveness_probe_file(&self.ctx, &mut self.watcher, &mut self.state) {
                rebuild_file(&self.ctx, &mut self.watcher, &mut self.state, None)
            } else {
                ControlFlow::Continue(())
            }
        }

        fn on_hold_due(&mut self) -> ControlFlow<StopReason> {
            // The rebuild decides the hold first, at an instant no earlier than its
            // deadline, so it ends the hold whatever the files hold or the clock does
            // (#380).
            let due = self.state.hold.deadline();
            rebuild_file(&self.ctx, &mut self.watcher, &mut self.state, due)
        }

        fn on_message(&mut self, msg: Msg, rx: &mpsc::Receiver<Msg>) -> ControlFlow<StopReason> {
            match handle_fs_event_file(msg, &self.state.foi, rx, self.debounce_ms, self.clear) {
                FileEventAction::Skip => ControlFlow::Continue(()),
                FileEventAction::Stop => ControlFlow::Break(StopReason::Interrupted),
                FileEventAction::Rebuild => {
                    rebuild_file(&self.ctx, &mut self.watcher, &mut self.state, None)
                }
            }
        }
    }
}

// ── Directory watch ───────────────────────────────────────────────────────────

const MAX_COLLECT_DEPTH: usize = 64;

/// Mutable state for the directory-mode watch loop.
struct DirWatchState {
    /// Forward dependency map: canonical source → its canonical (transitive) deps.
    /// Dep values are graph keys already: every compile's list goes through
    /// [`graph_keys`]; do not re-canonicalize.
    forward_deps: HashMap<PathBuf, Vec<PathBuf>>,
    /// Sources whose last compile attempt failed. Re-seeded into every batch that
    /// carries a real change, so a fix to whatever broke them is picked up.
    errored: HashSet<PathBuf>,
    /// Last-seen collected `.mds` set for reconcile/rename detection.
    known_files: BTreeSet<PathBuf>,
    /// By the path each output is written to (`WriteTarget.path`): what this session last
    /// wrote there and for which source — what a later write for that source may skip as
    /// unchanged, the proof a removal or a write after a change of kind asks for, and
    /// whose output the file is (#160). An entry goes when its file is removed, another
    /// source's write replaces it, or its source is forgotten; a file kept stays its
    /// source's.
    last_written: HashMap<PathBuf, WrittenOutput>,
    /// The output each source last had written this session, by source: where a deleted
    /// source's outputs are looked for (#160). A source with no entry — a partial, a
    /// dependency outside the root, one never written — has none to remove.
    outputs: HashMap<PathBuf, WriteTarget>,
    /// By source: what its last rebuild kept from being written after a change of kind,
    /// the file there not being the session's (#160) — the next rebuild of the same tries
    /// again but tells it no more. Any other rebuild of the source clears it, as does
    /// forgetting the source.
    kept: HashMap<PathBuf, String>,
    /// The out-dir every output is written below, checked before each write; `None` when
    /// outputs go beside their sources.
    out_dir: Option<OutDirAnchor>,
    /// The files every compile reads besides its source's own — the `--vars` file and the
    /// `mds.json` in force — which no output is written over (#425); each write adds the
    /// `mds.json` nearest its source ([`source_inputs`]).
    reads: Vec<PathBuf>,
    /// Parent dirs of dependencies located outside the watched root.
    /// Watched NonRecursive; re-armed by liveness probe.
    external_dep_dirs: BTreeSet<PathBuf>,
    /// The `--vars` file, canonical, if one is given: outside [`Self::tracked_set`], but
    /// in [`Self::watched_set`], since a rebuild is held while it is empty as while a
    /// source is (#380).
    vars_file: Option<PathBuf>,
    /// `(mtime, size)` baseline over [`DirWatchState::watched_set`] — sources,
    /// dependencies and the `--vars` file. Read by the idle tick's content backstop over
    /// the tracked set, and by a batch to tell a watched file emptied since (#380);
    /// re-written at the end of every batch, so the tick reports only what the batch did
    /// not already handle (#321).
    last_mtimes: StampMap,
    /// The rebuild held while a watched file is empty (#380).
    hold: EmptyHold,
    /// What the batches held back carried, rebuilt with the batch that ends the hold
    /// (#380).
    held: HeldBatch,
}

/// What the batches held back while a watched file is empty carried (#380): every path
/// they named, and whether one changed the `--vars` file. A directory-mode hold holds the
/// whole batch back — a source that imports the emptied file, or reads the emptied vars
/// file, would compile against nothing — and the batch that ends the hold rebuilds what
/// they named with its own, so no change made during the hold waits for a later one.
#[derive(Debug, Default, PartialEq, Eq)]
struct HeldBatch {
    /// The paths of the batches held back.
    paths: BTreeSet<PathBuf>,
    /// Whether one of them changed the `--vars` file.
    vars_changed: bool,
}

impl HeldBatch {
    /// Hold `paths` back, with `vars_changed`, beside the batches held before them.
    fn hold<'a>(&mut self, paths: impl IntoIterator<Item = &'a PathBuf>, vars_changed: bool) {
        self.paths.extend(paths.into_iter().cloned());
        self.vars_changed |= vars_changed;
    }

    /// The batch that ends the hold: `batch` and every batch held back, with whether any
    /// of them changed the vars file. Nothing stays held.
    fn release(
        &mut self,
        batch: &BTreeSet<PathBuf>,
        vars_changed: bool,
    ) -> (BTreeSet<PathBuf>, bool) {
        let held = std::mem::take(self);
        let mut paths = held.paths;
        paths.extend(batch.iter().cloned());
        (paths, vars_changed || held.vars_changed)
    }
}

/// `batch` and `vars_changed` with the watched files a look found `emptied` joined to them
/// (#380): the `--vars` file as a change to it, every other file as a path of the batch —
/// whether or not its own event came — so the batch that ends a hold rebuilds it.
fn join_emptied(
    batch: &BTreeSet<PathBuf>,
    vars_changed: bool,
    emptied: &BTreeSet<PathBuf>,
    vars_file: Option<&Path>,
) -> (BTreeSet<PathBuf>, bool) {
    let (vars, paths): (Vec<&PathBuf>, Vec<&PathBuf>) = emptied
        .iter()
        .partition(|path| Some(path.as_path()) == vars_file);
    let mut joined = batch.clone();
    joined.extend(paths.into_iter().cloned());
    (joined, vars_changed || !vars.is_empty())
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
        out: Option<&WriteTarget>,
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
        if let (Some(out), Some(content)) = (out, content) {
            self.wrote(src, out, content);
        }
    }

    /// `content` was written to `out`, the output of `src`: what a later write for `src`
    /// may skip as unchanged and a removal asks the file to still hold, recorded as
    /// `src`'s — whichever source the session wrote there for before — and where the
    /// source's outputs are once it is deleted (#160).
    fn wrote(&mut self, src: &Path, out: &WriteTarget, content: String) {
        let written = WrittenOutput {
            source: src.to_path_buf(),
            content,
        };
        self.last_written.insert(out.path.clone(), written);
        self.outputs.insert(src.to_path_buf(), out.clone());
    }

    /// What the session's record says of the file at `path` for `src` (#160).
    fn record(&self, path: &Path, src: &Path) -> Record<'_> {
        match self.last_written.get(path) {
            None => Record::Unwritten,
            Some(written) if written.source == src => Record::Own(&written.content),
            Some(_) => Record::OtherSource,
        }
    }

    /// Retire `out`, an output `src` no longer has ([`retire_output`]), and drop the
    /// record of it once the file is gone, if the record was `src`'s (#160).
    fn retire(
        &mut self,
        src: &Path,
        out: &WriteTarget,
        now: OutDirNow,
        why: Retirement,
        quiet: bool,
    ) {
        let record = self.record(&out.path, src);
        let own = matches!(record, Record::Own(_));
        if retire_output(out, self.out_dir.as_ref(), now, record, why, quiet) && own {
            self.last_written.remove(&out.path);
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
    /// the `last_written` dedup then suppresses. The freshness rule is about the
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

    /// Every path whose emptying holds a batch back (#380), and the domain of the
    /// `last_mtimes` baseline: [`Self::tracked_set`] and the `--vars` file, which every
    /// compile reads. The idle tick's content backstop diffs the tracked set alone — a
    /// change to the vars file reaches a batch through its event.
    fn watched_set(&self) -> HashSet<PathBuf> {
        let mut watched = self.tracked_set();
        watched.extend(self.vars_file.iter().cloned());
        watched
    }

    /// Remove every GRAPH record of `src` — its forward edges, its error flag and its
    /// known-files membership.
    fn forget_graph(&mut self, src: &Path) {
        self.forward_deps.remove(src);
        self.errored.remove(src);
        self.known_files.remove(src);
    }

    /// Remove all state for `src`, a source that is gone: its graph records, the record of
    /// where its output is, and the records of what the session wrote for it, so nothing
    /// written for it is this session's to remove any more (#160) — a file another source
    /// was written to since keeps that source's record. The outputs are the ones recorded
    /// when they were written, never ones guessed from the source's path: a dependency
    /// outside the root has none (#217). A source's records are only ever at its outputs
    /// of the two kinds.
    fn forget(&mut self, src: &Path) {
        if let Some(out) = self.outputs.remove(src) {
            for kind in [OutputKind::Markdown, OutputKind::Messages] {
                let path = out.path.with_extension(kind.extension());
                if self
                    .last_written
                    .get(&path)
                    .is_some_and(|written| written.source == src)
                {
                    self.last_written.remove(&path);
                }
            }
        }
        self.kept.remove(src);
        self.forget_graph(src);
    }

    /// Retire the outputs of `src`, a deleted source, and forget it (#160): the output it
    /// was last written to and the other kind's beside it, each removed only if this
    /// session wrote it for `src` and it is unchanged, or else kept ([`retire_output`]) —
    /// one the session last wrote for another source is that source's. A source that is
    /// there again — unlinked and created anew within the batch, as an editor's save, a
    /// branch checkout or `git stash` does — keeps its output and its state: the event that
    /// created it rebuilds it.
    fn retire_deleted(&mut self, src: &Path, quiet: bool) {
        if src.exists() {
            return;
        }
        if let Some(out) = self.outputs.get(src).cloned() {
            let now = check_out_dir(self.out_dir.as_mut(), &mut self.last_written);
            for kind in [OutputKind::Markdown, OutputKind::Messages] {
                let candidate = out.sibling(|path| path.with_extension(kind.extension()));
                self.retire(src, &candidate, now, Retirement::SourceDeleted, quiet);
            }
        }
        self.forget(src);
    }
}

/// State for the dir-mode liveness probe (reconcile rule).
struct LivenessState {
    /// Set to true on the very first tick so we do a reconcile after startup.
    first_tick: bool,
    /// Tracks whether the root existed on the previous tick.
    root_was_missing: bool,
    /// Whether the OS watcher was successfully armed for the root on the last tick.
    ///
    /// Mirrors the `armed_dirs` discipline from file mode: skip `watcher.watch(root, …)`
    /// on healthy ticks so the OS-level re-WalkDir / FSEvents stream teardown does not
    /// happen every idle tick — O(1) idle cost regardless of subtree size (reconcile rule).
    root_armed: bool,
    /// External dep dirs that were missing on the previous tick.
    ///
    /// Recovery is **edge-triggered**: a missing external dir triggers a full
    /// reconcile only when it *reappears* (vanish→reappear), never while it stays
    /// missing. A permanently-missing external dir must NOT force an O(tree) walk
    /// on every idle tick (reconcile rule / AC-P1).
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
/// A partial refreshes the graph and writes no output of its own; any other source's
/// output is written when its content changed. A compile that succeeds records the
/// dependencies it reported even when its write fails (#257). A compile that fails
/// because `src` is gone since the batch found it there retires it as a deleted source
/// ([`DirWatchState::retire_deleted`], #160).
///
/// # Invariants preserved
/// - Freshness rule: dep set recomputed from fresh `compile_to_content` output.
/// - PF-004: all reads go through `compile_to_content`.
///
/// Does **not** touch `state.last_mtimes`: the content backstop's baseline is settled
/// once per batch by `process_dir_batch`, over the whole tracked set (#321).
///
/// Compile success/failure is already signalled via `state.errored`; the caller uses
/// that set for error tracking.
///
/// Returns `true` when this call produced an observable, content-changed rebuild
/// (a real write, not a partial/unchanged/errored compile) — used by
/// `process_dir_batch`'s callers to gate the `#326` vars-file duplicate-key
/// warning on an OBSERVABLE rebuild rather than every internal recompute (the
/// same content-based signal `rebuild_file` uses in single-file mode).
fn compile_one_source(
    src: &Path,
    watch_root: &WatchedPath,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) -> bool {
    let root = watch_root.canonical.as_path();
    let t0 = Instant::now();
    // What the rebuild of `src` before this one kept from being written (#160): told again
    // unless this rebuild keeps the same once more, as another event of the same save does.
    let kept_before = state.kept.remove(src);
    // A debug build's test pause (#160): the batch found `src` there, and has not read it.
    pause_after_batch_split();
    let failure = match watch_root.compile_source(src, runtime_vars.clone(), quiet) {
        Ok(compiled) => {
            let dep_paths = graph_keys(&compiled.dependencies);

            // Partials (DD2): refresh graph edges but do NOT write output.
            if is_partial(src) {
                state.record_success(src, dep_paths, root, None, None);
                return false;
            }

            // Derive the output path from the compiled kind (intrinsic extension).
            // AC-FUNC-23: a @message template writes .json; a plain template writes .md.
            //
            // Invariant: `src` is strictly below `root`, so this never takes the
            // out-of-root flatten arm and never emits its report (#217). Every path that
            // reaches here passed `is_in_root` in `process_dir_batch_incremental` or the
            // equivalent gate in `process_dir_batch_vars_changed`; out-of-root deps take
            // the dep-refresh-only branch above and never call this function.
            let ext = compiled.kind.extension();
            let out = output_path_for(src, watch_root.root_paths(), output_base, ext);

            let out_dir = check_out_dir(state.out_dir.as_mut(), &mut state.last_written);

            // A change of kind (#160): the output this session last wrote for `src` is
            // the other kind's, so the file at `out` is written over only if it is the
            // session's own, written for `src`.
            let previous = state
                .outputs
                .get(src)
                .filter(|last| last.path != out.path)
                .cloned();
            // Content-based dedup: skip the write when the session last wrote the same
            // there for `src` — never after a change of kind, whose write is decided by the
            // file there whatever the record of it holds.
            let content_changed = previous.is_some()
                || state.record(&out.path, src) != Record::Own(&compiled.content);
            // Whether the write empties the output the session wrote there for `src`,
            // decided before the write replaces the record (#380).
            let emptied = match state.record(&out.path, src) {
                Record::Own(before) => empties_output(Some(before), &compiled.content),
                Record::Unwritten | Record::OtherSource => false,
            };

            if content_changed {
                // #380: a file the compile read, emptied since the batch looked — a
                // truncation that began between the two — holds `src` back as the look
                // would have, unless the deadline ended the hold in this rebuild: nothing
                // written or recorded, `src` kept to be rebuilt with the batch that ends
                // the hold.
                let source = src.to_path_buf();
                let reads = std::iter::once(&source)
                    .chain(&dep_paths)
                    .chain(state.vars_file.as_ref());
                if any_went_empty(reads, &state.last_mtimes)
                    && state.hold.on_late_empty(Instant::now()) == HoldVerdict::Hold
                {
                    if let Some(kept) = kept_before {
                        state.kept.insert(source.clone(), kept);
                    }
                    state.held.hold([&source], false);
                    return false;
                }
                let written = match out_dir {
                    OutDirNow::Elsewhere => {
                        Err(miette::Report::new(crate::write::out_dir_moved(&out)))
                    }
                    OutDirNow::Unchanged | OutDirNow::New if previous.is_some() => {
                        write_after_change_of_kind(
                            &below_checked_out_dir(state.out_dir.as_ref(), &out),
                            state.record(&out.path, src),
                            &compiled.content,
                            kept_before.as_ref() == Some(&compiled.content),
                            quiet,
                        )
                        .map_err(miette::Report::new)
                    }
                    OutDirNow::Unchanged | OutDirNow::New => write_output(
                        Some(&below_checked_out_dir(state.out_dir.as_ref(), &out)),
                        &compiled.content,
                        &source_inputs(src, &compiled.dependencies, &state.reads),
                        quiet,
                        false,
                    )
                    .map(|()| true),
                };
                match written {
                    // Kept: nothing is written and nothing retired, and the record of the
                    // old kind's output stays, so a later save tries again.
                    Ok(false) => {
                        state.kept.insert(src.to_path_buf(), compiled.content);
                        state.record_success(src, dep_paths, root, None, None);
                        return false;
                    }
                    Ok(true) => {
                        if let Some(anchor) = &mut state.out_dir {
                            anchor.written();
                        }
                        let elapsed = t0.elapsed().as_millis();
                        let dep_count = compiled.dependencies.len();
                        if !quiet {
                            crate::output::ewriteln!(
                                "Recompiled {} ({} deps) in {}ms",
                                safe_path(&out.shown),
                                dep_count,
                                elapsed
                            );
                        }
                        announce_emptied_output(&out.shown, emptied, quiet);
                        // A change of kind (#160): the output the session last wrote for
                        // `src`, the other kind's, is retired — removed only if the
                        // session's record says it wrote it for `src` and it still holds
                        // what was written, else kept with a notice: one the session wrote
                        // for another source (beside its sources `a.b.mds` and `a.mds` both
                        // name theirs `a.md`), or one in an out-dir made since, is not
                        // `src`'s. The removal, below the directory the write's check
                        // found, triggers no rebuild: it is no `.mds` file. A rebuild's
                        // failure never changes how the session exits, so one to remove
                        // it stays a warning (#157).
                        if let Some(previous) = &previous {
                            state.retire(src, previous, out_dir, Retirement::KindChanged, quiet);
                        }

                        state.record_success(
                            src,
                            dep_paths,
                            root,
                            Some(&out),
                            Some(compiled.content),
                        );
                        return true;
                    }
                    Err(e) => {
                        // The compile succeeded: the dependencies it reported are the
                        // source's now, as at startup, so an edit to one of them rebuilds
                        // it — one outside the root included (#257). Nothing is recorded
                        // as written, and the settle below marks the source errored.
                        state.record_success(src, dep_paths, root, None, None);
                        Some(e)
                    }
                }
            } else {
                // Content unchanged — still refresh graph edges + known_files.
                state.record_success(src, dep_paths, root, None, None);
                return false;
            }
        }
        // The source went after the batch found it there (#160): whatever the compile
        // said of a file that is no longer there — `file not found`, or a read that
        // failed — it is a deleted source, retired by the same rule, never a compile
        // error. A source still there, or one whose presence cannot be told, fails as
        // any other compile does.
        Err(CompileFailure::Error(_)) if matches!(src.try_exists(), Ok(false)) => {
            state.retire_deleted(src, quiet);
            return false;
        }
        Err(failure) => failure.unreported(),
    };
    // The compile failed, or writing its output did: settled alike.
    settle(SettleInto::Dir(state), failure, Settle::MarkErrored(src));
    false
}

/// Compile-time context for directory-mode watch, parallel to `FileCompileCtx`.
///
/// Groups the parameters resolved once at startup and threaded into every
/// liveness-probe and event-handler call — removes `#[allow(clippy::too_many_arguments)]`
/// from the extracted helper functions (issue #6 / zero-warnings policy).
struct DirWatchCtx {
    /// The watched directory, as typed and canonical ([`Watched::Root`]).
    root: WatchedPath,
    /// The working directory at startup, restored first by every rebuild.
    working_dir: WorkingDir,
    /// Canonicalized `--vars` path — matches notify's canonicalized event paths;
    /// used for matching/watching (never for display, #326).
    vars_path: Option<PathBuf>,
    /// The `--vars` path exactly as the user typed it, uncanonicalized (#326, D4).
    /// Used for `RuntimeVarArgs.vars` so the vars-file duplicate-key warning displays
    /// (and reads) the as-typed path rather than its canonical form.
    vars_path_typed: Option<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    static_set_string_vars: Vec<(String, String)>,
    output_base: OutputBase,
    exclude_prefix: Option<PathBuf>,
    vars_dir_extra: Option<PathBuf>,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
}

/// Arm each of `dirs` — directories of dependencies outside the root — that `armed` does
/// not hold yet, with `watch`, and add it to `armed` once its watch is in place (#257).
/// One whose watch fails is reported, named through [`shown_watched_dir`] with `root` and
/// `vars`, and is left out of `armed`, so the next rebuild and the liveness tick arm it
/// again: `armed` holds only directories the watcher holds.
fn arm_external_dep_dirs<'a>(
    dirs: impl IntoIterator<Item = &'a PathBuf>,
    armed: &mut BTreeSet<PathBuf>,
    mut watch: impl FnMut(&Path) -> notify::Result<()>,
    root: RootPaths<'_>,
    vars: Option<RootPaths<'_>>,
) {
    for dir in dirs {
        if armed.contains(dir) {
            continue;
        }
        match watch(dir) {
            Ok(()) => {
                armed.insert(dir.clone());
            }
            Err(e) => eprint_warning(&format!(
                "warning: failed to watch external dep dir {}: {}",
                safe_path(&shown_watched_dir(dir, root, vars)),
                safe_inline(notify_cause(&e))
            )),
        }
    }
}

/// Once a rebuild has run, arm the directories of the dependencies outside the root that
/// are not armed — one its compile reported first, and one whose earlier watch failed — so
/// an edit in one rebuilds at once, not only at an idle tick, which `--poll-interval 0`
/// never runs (#257). A directory that does not exist is left to the liveness tick, which
/// arms it when it reappears.
fn arm_external_dirs_after_rebuild(
    ctx: &DirWatchCtx,
    watcher: &mut RecommendedWatcher,
    liveness: &mut LivenessState,
    state: &DirWatchState,
) {
    arm_external_dep_dirs(
        state.external_dep_dirs.iter().filter(|dir| dir.exists()),
        &mut liveness.armed_external_dirs,
        |dir| watcher.watch(dir, RecursiveMode::NonRecursive),
        ctx.root.root_paths(),
        extra_vars_dir(
            ctx.vars_dir_extra.as_deref(),
            ctx.vars_path_typed.as_deref(),
        ),
    );
}

/// Run the idle-tick liveness probe for directory mode (reconcile rule, DD1).
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
    // 1. Re-arm root as Recursive (gated — reconcile rule / issue #1 idle O(1) fix).
    //
    // Skip the `watcher.watch()` syscall on healthy ticks when root is already armed:
    // on Linux `notify` re-WalkDirs the entire subtree + calls `inotify_add_watch` per
    // subdirectory on every `watch()` call regardless of mode; on macOS it tears down
    // and recreates the FSEvents stream.  Only re-arm when:
    //   (a) first_tick — not yet armed
    //   (b) root was missing last tick but now exists (vanish→reappear edge)
    //   (c) root_armed is false — a previous arm attempt failed; retry
    let root_now_exists = ctx.root.canonical.exists();
    let need_root_rearm = liveness.first_tick
        || (liveness.root_was_missing && root_now_exists)
        || !liveness.root_armed;
    let root_ok = if root_now_exists && need_root_rearm {
        let ok = watcher
            .watch(&ctx.root.canonical, RecursiveMode::Recursive)
            .is_ok();
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

    // 2. Recovery trigger (reconcile rule):
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
        let current: BTreeSet<PathBuf> = collect_mds_files(
            &ctx.root.canonical,
            MAX_COLLECT_DEPTH,
            ctx.exclude_prefix.as_deref(),
        )
        .into_iter()
        .map(|p| graph_key(&p))
        .collect();
        batch.extend(current.difference(&state.known_files).cloned());
        batch.extend(state.known_files.difference(&current).cloned());
        state.known_files = current;
    }

    if !batch.is_empty() {
        // The event handler's rebuild path, so a recompile driven purely by this
        // content-backstop/full-reconcile tick (no FS event ever delivered, e.g. after a
        // root delete+recreate) re-reports the vars-file duplicate keys under the same
        // content-changed gate, and one logical edit observed by both paths still warns
        // once — tests I17 and I19.
        rebuild_dir_batch(ctx, &batch, false /* vars_changed */, state, None);
        arm_external_dirs_after_rebuild(ctx, watcher, liveness, state);
    }
    // No baseline refresh here: `process_dir_batch` re-baselines `last_mtimes` over the
    // post-batch watched set, and an empty batch means nothing appeared, was removed, or
    // changed — so the existing baseline is by definition still accurate.
}

/// Rebuild `batch` in directory mode — the one rebuild path of the event handler, of the
/// idle tick's content backstop and of a held rebuild's deadline alike.
///
/// A recreated working directory is restored first ([`WorkingDir::restore_if_recreated`]),
/// before anything is read. A watched file — a source, a dependency or the `--vars` file
/// ([`DirWatchState::watched_set`]) — found empty where the baseline saw bytes then holds
/// the whole batch back (#380): nothing is compiled, written or printed, the baseline is
/// left as it was ([`Settle::Defer`]), and the batch is kept ([`HeldBatch`]) to be rebuilt
/// with the one that ends the hold — the first to find no watched file emptied, or the
/// first at or past its deadline ([`EmptyHold`]), which compiles the files as they are.
/// Every file the look finds emptied joins the batch, its own event lost or not
/// ([`join_emptied`]), so that batch rebuilds it. A source whose compile read a file
/// emptied after the look is held the same way, alone ([`compile_one_source`]). `due` is
/// the hold's deadline when that deadline runs this rebuild, which then ends the hold
/// ([`not_before`]); `None` for an event or a tick.
/// The vars file is then reloaded (freshness rule), and a
/// failure to read it is reported and settled: it may be temporarily absent (AC-W7 /
/// AC-C5). `--set`/`--set-string` are fixed for the session and warned once at startup —
/// discarded here (via `resolved.vars`). The vars file's duplicate keys are re-reported
/// only when the batch produced an OBSERVABLE rebuild (#326, test I17): at `--debounce 0`
/// a single edit can generate more than one raw FS event, each reaching the event handler
/// separately, and the idle tick can observe the same edit again, so the warning is
/// emitted after `process_dir_batch` reports whether anything actually changed rather
/// than unconditionally — one logical edit warns once.
fn rebuild_dir_batch(
    ctx: &DirWatchCtx,
    batch: &BTreeSet<PathBuf>,
    vars_changed: bool,
    state: &mut DirWatchState,
    due: Option<Instant>,
) {
    ctx.working_dir.restore_if_recreated();

    // #380: a watched file emptied holds the whole batch back, and joins it.
    let emptied = emptied_paths(&state.watched_set(), &state.last_mtimes);
    let verdict = state
        .hold
        .on_rebuild(!emptied.is_empty(), not_before(Instant::now(), due));
    debug_assert!(
        due.is_none() || verdict == HoldVerdict::Compile,
        "a held rebuild's deadline ends its hold"
    );
    let (batch, vars_changed) =
        join_emptied(batch, vars_changed, &emptied, state.vars_file.as_deref());
    if verdict == HoldVerdict::Hold {
        state.held.hold(&batch, vars_changed);
        settle(SettleInto::Dir(state), None, Settle::Defer);
        return;
    }
    let (batch, vars_changed) = state.held.release(&batch, vars_changed);

    let resolved = match build_runtime_vars(RuntimeVarArgs {
        vars: ctx.vars_path_typed.clone(),
        set_vars: ctx.static_set_vars.clone(),
        set_string_vars: ctx.static_set_string_vars.clone(),
    }) {
        Ok(v) => v,
        Err(e) => {
            // Re-baseline so the idle-tick content backstop does not report the same
            // change again and turn one unreadable vars file into per-tick error spam.
            settle(SettleInto::Dir(state), Some(e), Settle::Rebaseline);
            return;
        }
    };

    // `process_dir_batch` takes the map by reference, so borrow `resolved.vars`
    // directly rather than cloning it — `resolved` (and its `.vars_file`,
    // `.duplicate_vars_file_keys`, `.duplicate_vars_file_keys_omitted`) is still
    // needed below, after this borrow ends, for the warning emission.
    let any_changed = process_dir_batch(
        &batch,
        vars_changed,
        &ctx.root,
        &ctx.output_base,
        &resolved.vars,
        ctx.quiet,
        state,
    );
    if any_changed {
        crate::build::emit_duplicate_vars_file_warnings(&resolved, ctx.quiet);
    }
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
/// Collects changed paths, drains the debounce window, filters irrelevant paths, and
/// rebuilds what is left through [`rebuild_dir_batch`]. Returns `DirEventOutcome` so the
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
            eprint_warning(&format!(
                "warning: watch error: {}",
                safe_inline(notify_cause(&e))
            ));
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
    let drained = drain_debounce(rx, ctx.debounce_ms);
    if drained.interrupted() {
        return DirEventOutcome::Stop;
    }
    changed.extend(drained.paths);

    // Defense-in-depth: ignore events from inside the out-dir subtree.
    if let OutputBase::Dir {
        canonical: ref od, ..
    } = ctx.output_base
    {
        changed.retain(|p| !p.starts_with(od));
    }

    // PF-004: drop events from default-excluded subdirectories (hidden dirs and
    // node_modules/) inside the watch root.  The initial walker never seeds files
    // from those dirs, so they are not in the dep graph and processing their
    // events would cause spurious rebuilds (e.g. npm install writing to
    // node_modules/ triggers a full re-scan on every package update).
    changed.retain(|p| !is_within_default_excluded_dir(&ctx.root.canonical, p));

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
                && (p.starts_with(&ctx.root.canonical)
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

    rebuild_dir_batch(ctx, &mds_changed, vars_changed, state, None);
    DirEventOutcome::Done
}

/// Directory watch: `root.typed` is the directory as typed — `mds.json` is looked up from it
/// (#413) and every status line names it, and the directories below it, by it (#390);
/// `root.canonical` is its canonical form, which notify reports event paths under.
///
/// Startup runs in phases whose order the types enforce (#429, [`dir_startup`]):
/// [`dir_startup::arm_pre_read`] arms the root, and the `--vars` file's directory outside
/// it, before any source is read, [`dir_startup::startup_compile`] compiles each source once
/// and writes its output, [`dir_startup::arm_deps_and_seed`] arms the directories of the
/// dependencies outside the root and seeds what every rebuild reads, and
/// [`dir_startup::Seeded::go_live`] goes live and watches until the session stops.
fn run_watch_dir(root: WatchedPath, args: SessionArgs) -> Result<()> {
    let armed = dir_startup::arm_pre_read(root, args)?;
    let compiled = dir_startup::startup_compile(armed)?;
    dir_startup::arm_deps_and_seed(compiled).go_live();
    Ok(())
}

/// Directory mode's startup, in phases whose order the types enforce (#429).
///
/// Each phase takes the token only the phase before it makes, and every token's fields are
/// private to this module, so nothing outside it can make one: an
/// [`Armed`](dir_startup::Armed) comes only from [`dir_startup::arm_pre_read`], a
/// [`Compiled`](dir_startup::Compiled) only from [`dir_startup::startup_compile`] given an
/// `Armed`, a [`Seeded`](dir_startup::Seeded) only from [`dir_startup::arm_deps_and_seed`]
/// given a `Compiled`, and the session goes live only from a `Seeded`
/// ([`dir_startup::Seeded::go_live`]). That is the order the session's change detection
/// rests on: the root armed recursively, and the `--vars` file's directory outside it,
/// before the tree is walked or any source read; every source's `(mtime, size)` baseline
/// taken before the first of them is read, and a dependency's as soon as the compile that
/// reports it returns; the directories of the dependencies outside the root armed once
/// every startup output is published; Ctrl+C wired last, with nothing but the loop after
/// it. The compile takes the `Armed` and carries it on inside the `Compiled`, so one
/// `Armed` is compiled once, and a `Compiled` is seeded with the very watches it was
/// compiled under.
mod dir_startup {
    use std::collections::{BTreeSet, HashMap, HashSet};
    use std::ops::ControlFlow;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use miette::Result;
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};

    use crate::build::{
        build_runtime_vars, emit_duplicate_var_warnings, load_config, run_reads, source_inputs,
        write_output, ProjectConfig, RuntimeVarArgs,
    };
    use crate::output::{
        collect_mds_files, eprint_warning, is_partial, notify_cause, output_path_for, safe_inline,
        safe_path, OutputBase,
    };

    use super::{
        arm_external_dep_dirs, arm_external_dirs_after_rebuild, baseline_path,
        canonicalize_vars_path, extra_vars_dir, graph_key, graph_keys, handle_fs_event_dir, live,
        liveness_probe_dir, rebuild_dir_batch, resolve_output_base, settle_startup_error,
        shown_watched_dir, snapshot_state, startup_race_probe, DirEventOutcome, DirWatchCtx,
        DirWatchState, EmptyHold, FileStamp, HeldBatch, LivenessState, Msg, OutDirAnchor,
        SessionArgs, StampMap, StartupInto, StopReason, WatchedPath, WorkingDir, MAX_COLLECT_DEPTH,
    };

    /// The root armed recursively, and the `--vars` file's directory outside it, before the
    /// tree is walked or any source read: made only by [`arm_pre_read`].
    pub(super) struct Armed {
        /// The watched directory, as typed and canonical ([`super::Watched::Root`]).
        root: WatchedPath,
        /// `--out-dir`.
        out_dir: Option<PathBuf>,
        /// The working directory at startup, which every rebuild restores first.
        working_dir: WorkingDir,
        /// The `mds.json` in force, looked up from the root as typed.
        config: Option<ProjectConfig>,
        /// The `--vars` file, canonical: what notify names its events by (#326).
        vars_path: Option<PathBuf>,
        /// The `--vars` file as typed, which it is read and named by (#326).
        vars_path_typed: Option<PathBuf>,
        static_set_vars: Vec<(String, String)>,
        static_set_string_vars: Vec<(String, String)>,
        /// Where every output is written ([`OutputBase`]).
        output_base: OutputBase,
        /// The out-dir when it is inside the root, which the walk leaves out.
        exclude_prefix: Option<PathBuf>,
        /// The `--vars` file's directory when it is outside the root, armed on its own.
        vars_dir_extra: Option<PathBuf>,
        quiet: bool,
        clear: bool,
        debounce_ms: u64,
        tick: Option<Duration>,
        /// The channel the watcher sends on — and Ctrl+C, once the session is live.
        tx: mpsc::Sender<Msg>,
        rx: mpsc::Receiver<Msg>,
        watcher: RecommendedWatcher,
    }

    /// Every source compiled once, and its output written when it could be: made only by
    /// [`startup_compile`].
    pub(super) struct Compiled {
        /// The watches the compile ran under, handed on to [`arm_deps_and_seed`] with what
        /// it produced.
        armed: Armed,
        /// The dependency graph, what was written and where, and the sources errored.
        state: DirWatchState,
        /// Every source's `(mtime, size)`, taken before the first of them was read, and each
        /// dependency's, taken as the compile that reported it returned.
        pre_mtimes: StampMap,
    }

    /// Every directory armed and every baseline taken, with what every rebuild reads: made
    /// only by [`arm_deps_and_seed`].
    pub(super) struct Seeded {
        /// The sender the watcher was armed with, which Ctrl+C is wired to.
        tx: mpsc::Sender<Msg>,
        rx: mpsc::Receiver<Msg>,
        tick: Option<Duration>,
        session: DirSession,
    }

    /// A directory-mode session once live ([`live::Session`]): what every rebuild reads, the
    /// watcher, the loop state and the liveness probe's.
    struct DirSession {
        ctx: DirWatchCtx,
        watcher: RecommendedWatcher,
        state: DirWatchState,
        liveness: LivenessState,
    }

    /// Record the working directory, load `mds.json`, check the `--vars` file and the
    /// out-dir, and say what is watched; then arm the root, and the `--vars` file's
    /// directory outside it, before the tree is walked or any source read.
    pub(super) fn arm_pre_read(watch_root: WatchedPath, args: SessionArgs) -> Result<Armed> {
        let SessionArgs {
            out_dir,
            vars,
            set_vars,
            set_string_vars,
            clear,
            debounce_ms,
            quiet,
            tick,
        } = args;
        let working_dir = WorkingDir::record();
        let root = watch_root.canonical.as_path();
        // Load config once from the root directory, as typed, so a config error names
        // `mds.json` as the input reaches it (`./mds.json`, `src/../mds.json`), never by its
        // canonical absolute path (#413); the config directory it returns is canonical.
        let config = load_config(&watch_root.typed)?;
        // #326: keep the --vars argument as the user typed it (see FileCompileCtx's
        // vars_path_typed doc for why) — `vars_path` below stays canonical for matching.
        let vars_path_typed = vars.clone();
        // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on macOS).
        // Also rejects a symlinked vars file at startup (build parity).
        let vars_path = canonicalize_vars_path(vars).map_err(miette::Error::from)?;
        let static_set_vars = set_vars;
        let static_set_string_vars = set_string_vars;

        // Compute the OutputBase (Fix 2 — subtree mirroring). Reject `..` at startup. The
        // out-dir's canonical form keeps the starts_with(&root) in-root exclusion check
        // reliable even when cwd contains symlinks (root is already canonical — security #8).
        let output_base = resolve_output_base(out_dir.as_ref(), &config)?;

        // When the out-dir is inside root, exclude it from collection so the watcher
        // doesn't self-pollute (AC-M7 / edge case 6).
        let exclude_prefix: Option<PathBuf> = match &output_base {
            OutputBase::Dir { canonical: d, .. } if d.starts_with(root) => Some(d.clone()),
            _ => None,
        };

        if !quiet {
            crate::output::ewriteln!("Watching directory {}", safe_path(&watch_root.typed));
        }

        // Additionally watch the vars file's parent if it is outside root.
        let vars_dir_extra: Option<PathBuf> = vars_path.as_deref().and_then(|vf| {
            let parent = vf.parent()?;
            // Only watch if outside root to avoid redundancy.
            if !parent.starts_with(root) {
                Some(parent.to_path_buf())
            } else {
                None
            }
        });
        // That directory in the two forms a message names it by ([`shown_watched_dir`]).
        let vars_dir = extra_vars_dir(vars_dir_extra.as_deref(), vars_path_typed.as_deref());

        // ── Arm before publish (startup race) ─────────────────────────────────
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
                crate::output::panic_in_handler("notify");
                let _ = tx_fs.send(Msg::Fs(res));
            },
            notify::Config::default(),
        )
        .map_err(|e| {
            miette::miette!(
                "failed to initialize file watcher: {}",
                safe_inline(notify_cause(&e))
            )
        })?;

        // Watch the root recursively.
        watcher.watch(root, RecursiveMode::Recursive).map_err(|e| {
            miette::miette!(
                "failed to watch directory {}: {}\n\
                     hint: on Linux you may need to increase fs.inotify.max_user_watches",
                safe_path(&shown_watched_dir(root, watch_root.root_paths(), vars_dir)),
                safe_inline(notify_cause(&e))
            )
        })?;

        // Watch the vars dir if it is outside root — soft warning on failure (mirrors the
        // external-dep-dir convention and the liveness probe's best-effort re-arm
        // semantics; a transient failure must not abort the session, applies the reconcile
        // rule / consistency fix).
        if let Some(ref vd) = vars_dir_extra {
            if let Err(e) = watcher.watch(vd, RecursiveMode::NonRecursive) {
                eprint_warning(&format!(
                    "warning: failed to watch vars directory {}: {}",
                    safe_path(&shown_watched_dir(vd, watch_root.root_paths(), vars_dir)),
                    safe_inline(notify_cause(&e))
                ));
            }
        }

        Ok(Armed {
            root: watch_root,
            out_dir,
            working_dir,
            config,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            output_base,
            exclude_prefix,
            vars_dir_extra,
            quiet,
            clear,
            debounce_ms,
            tick,
            tx,
            rx,
            watcher,
        })
    }

    /// Once [`arm_pre_read`] has armed what the walk and the compiles read: walk the root,
    /// take every source's baseline before the first is read, then compile each source once
    /// and write its output. A source whose compile or write fails is reported and marked
    /// errored, and watching continues.
    pub(super) fn startup_compile(armed: Armed) -> Result<Compiled> {
        let Armed {
            root: watch_root,
            out_dir,
            config,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            output_base,
            exclude_prefix,
            ..
        } = &armed;
        let root = watch_root.canonical.as_path();
        let quiet = armed.quiet;

        // Startup compile: compile all .mds files found under root.
        let all_files = collect_mds_files(root, MAX_COLLECT_DEPTH, exclude_prefix.as_deref());
        let resolved = build_runtime_vars(RuntimeVarArgs {
            vars: vars_path_typed.clone(),
            set_vars: static_set_vars.clone(),
            set_string_vars: static_set_string_vars.clone(),
        })?;
        emit_duplicate_var_warnings(&resolved, quiet);
        let runtime_vars = resolved.vars;

        // Build the dependency graph and compile all files at startup.
        let mut state = DirWatchState {
            forward_deps: HashMap::new(),
            errored: HashSet::new(),
            known_files: BTreeSet::new(),
            last_written: HashMap::new(),
            outputs: HashMap::new(),
            kept: HashMap::new(),
            // Recorded below, once the startup writes have made the out-dir.
            out_dir: None,
            reads: run_reads(vars_path.as_deref(), config.as_ref()),
            external_dep_dirs: BTreeSet::new(),
            vars_file: vars_path.clone(),
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            held: HeldBatch::default(),
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
            match watch_root.compile_source(source, runtime_vars.clone(), quiet) {
                Ok(compiled) => {
                    let dep_paths = graph_keys(&compiled.dependencies);

                    // Track external dep dirs (DD3 — cross-root).
                    for dep in &dep_paths {
                        if let Some(parent) = dep.parent() {
                            if !parent.starts_with(root) {
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
                        //
                        // Invariant: `key` is `graph_key(source)` for a source the walker
                        // collected under the already-canonical `root`, and the walker skips
                        // symlinked files and directories — so the canonical key is still
                        // prefixed by `root` and the out-of-root flatten arm cannot fire
                        // here (#217).
                        let ext = compiled.kind.extension();
                        let out = output_path_for(&key, watch_root.root_paths(), output_base, ext);
                        // The content dedup holds only what was written: a source whose
                        // write failed is errored instead, so the next rebuild with a real
                        // change writes it even when its content has not changed (#257) —
                        // and nothing it did not write is ever its to remove (#160).
                        let inputs = source_inputs(&key, &compiled.dependencies, &state.reads);
                        if let Err(e) =
                            write_output(Some(&out), &compiled.content, &inputs, quiet, true)
                        {
                            settle_startup_error(StartupInto::Dir(&mut state), Some(e), &key);
                        } else {
                            state.wrote(&key, &out, compiled.content);
                        }
                    }
                }
                Err(failure) => {
                    // `key` is new to the graph — each source is compiled once here — so the
                    // errored source's dependency set is the empty one.
                    settle_startup_error(StartupInto::Dir(&mut state), failure.unreported(), &key);
                    state.known_files.insert(key);
                }
            }
        }
        // The directory the startup writes made, or found, is the one the first rebuild's
        // write compares (#160).
        state.out_dir = OutDirAnchor::record(out_dir.as_deref(), config.as_ref());

        Ok(Compiled {
            armed,
            state,
            pre_mtimes,
        })
    }

    /// Arm the directories of the dependencies outside the root that the startup compile
    /// reported — one that cannot be armed is a warning — then seed what every rebuild
    /// reads: the merged baseline, the liveness probe's state and the session's context.
    pub(super) fn arm_deps_and_seed(compiled: Compiled) -> Seeded {
        let Compiled {
            armed,
            mut state,
            pre_mtimes,
        } = compiled;
        let Armed {
            root: watch_root,
            out_dir: _,
            working_dir,
            config: _,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            output_base,
            exclude_prefix,
            vars_dir_extra,
            quiet,
            clear,
            debounce_ms,
            tick,
            tx,
            rx,
            mut watcher,
        } = armed;
        let root = watch_root.canonical.as_path();

        // All startup outputs are now published — the positive-control injection point.
        startup_race_probe();

        // Watch external dep dirs NonRecursive (DD3). Cross-root dependencies are only
        // discovered by the startup compile, so unlike the root they cannot be armed
        // before the first read; an edit landing in that window produces no event for
        // anyone. What closes it is the baseline [`startup_compile`] captured, which
        // predates the read — the idle tick's content backstop compares against it and
        // recompiles (#321). `MDS_WATCH_READY` still marks the instant both detectors
        // cover every path, so tests can synchronise on arming rather than on a tick.
        // Only a directory whose watch is in place is held as armed: one whose watch
        // failed is tried again by the next rebuild and the liveness tick (#257).
        let mut armed_external_dirs = BTreeSet::new();
        arm_external_dep_dirs(
            &state.external_dep_dirs,
            &mut armed_external_dirs,
            |dir| watcher.watch(dir, RecursiveMode::NonRecursive),
            watch_root.root_paths(),
            extra_vars_dir(vars_dir_extra.as_deref(), vars_path_typed.as_deref()),
        );

        // Seed the content backstop's baseline (#321), over the watched set: the `--vars`
        // file too, so a batch can tell it emptied (#380).
        //
        // The merge DIRECTION is load-bearing, exactly as in single-file mode: the
        // pre-compile pairs in `pre_mtimes` overwrite the post-compile ones, because only
        // they predate the reads whose results were published. Inverting the merge — or
        // switching to one that keeps the value already present — would silently restore
        // the lost-save bug while every test still passes.
        let mut last_mtimes = snapshot_state(&state.watched_set());
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
            // The directories whose startup watch is in place, and only those.
            armed_external_dirs,
        };

        let ctx = DirWatchCtx {
            root: watch_root,
            working_dir,
            vars_path,
            vars_path_typed,
            static_set_vars,
            static_set_string_vars,
            output_base,
            exclude_prefix,
            vars_dir_extra,
            clear,
            debounce_ms,
            quiet,
        };

        Seeded {
            tx,
            rx,
            tick,
            session: DirSession {
                ctx,
                watcher,
                state,
                liveness,
            },
        }
    }

    impl Seeded {
        /// Every watch armed and every baseline taken: go live — Ctrl+C wired last — and
        /// watch until the session stops ([`live::run_session`]).
        pub(super) fn go_live(self) {
            let Seeded {
                tx,
                rx,
                tick,
                session,
            } = self;
            live::run_session(tx, rx, tick, session);
        }
    }

    impl live::Session for DirSession {
        fn is_quiet(&self) -> bool {
            self.ctx.quiet
        }

        fn hold_deadline(&self) -> Option<Instant> {
            self.state.hold.deadline()
        }

        fn on_tick(&mut self) -> ControlFlow<StopReason> {
            // Idle tick — run liveness probe (reconcile rule, DD1).
            liveness_probe_dir(
                &self.ctx,
                &mut self.watcher,
                &mut self.liveness,
                &mut self.state,
            );
            ControlFlow::Continue(())
        }

        fn on_hold_due(&mut self) -> ControlFlow<StopReason> {
            // The batches held back, with nothing new: the rebuild decides the hold first,
            // at an instant no earlier than its deadline, so it ends the hold whatever the
            // files hold or the clock does (#380).
            let due = self.state.hold.deadline();
            rebuild_dir_batch(&self.ctx, &BTreeSet::new(), false, &mut self.state, due);
            arm_external_dirs_after_rebuild(
                &self.ctx,
                &mut self.watcher,
                &mut self.liveness,
                &self.state,
            );
            ControlFlow::Continue(())
        }

        fn on_message(&mut self, msg: Msg, rx: &mpsc::Receiver<Msg>) -> ControlFlow<StopReason> {
            match handle_fs_event_dir(msg, &self.ctx, rx, &mut self.state) {
                DirEventOutcome::Skip => ControlFlow::Continue(()),
                DirEventOutcome::Done => {
                    arm_external_dirs_after_rebuild(
                        &self.ctx,
                        &mut self.watcher,
                        &mut self.liveness,
                        &self.state,
                    );
                    ControlFlow::Continue(())
                }
                DirEventOutcome::Stop => ControlFlow::Break(StopReason::Interrupted),
            }
        }
    }
}

/// Process a batch of changed `.mds` paths in directory mode.
///
/// Thin dispatcher: delegates to `process_dir_batch_vars_changed` when every source
/// must be recompiled (vars file changed) — the known ones and those the batch names —
/// or to `process_dir_batch_incremental` for a normal seed-and-propagate pass.
///
/// Called by both the event path and the reconcile path so the same state
/// transitions apply uniformly.
///
/// Returns `true` when the batch produced at least one observable, content-changed
/// rebuild (see `compile_one_source`) — callers use this to gate the `#326`
/// vars-file duplicate-key warning on an OBSERVABLE rebuild, since a single logical
/// edit can otherwise reach this function more than once (e.g. multiple raw FS
/// events for one write at `--debounce 0`, or a liveness-probe self-heal tick
/// racing a real FS event for the same change) and would otherwise double-warn.
fn process_dir_batch(
    changed: &BTreeSet<PathBuf>,
    vars_changed: bool,
    watch_root: &WatchedPath,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) -> bool {
    let any_changed = if vars_changed {
        process_dir_batch_vars_changed(changed, watch_root, output_base, runtime_vars, quiet, state)
    } else {
        process_dir_batch_incremental(changed, watch_root, output_base, runtime_vars, quiet, state)
    };

    // Re-baseline the content backstop over the post-batch watched set (#321) — the
    // tracked set and the `--vars` file, so a file the batch compiled empty is not taken
    // for one emptied since (#380).
    //
    // This is the single settle point for `last_mtimes`, and it has to be here rather
    // than at each compile site: the batch is what the idle tick must not report again,
    // and only the batch as a whole knows which paths it covered. Doing it once, over
    // the whole set, also settles the sources a *failed* compile touched (so an
    // unchanged broken file does not re-fire every tick) and drops keys for sources the
    // batch deleted, which `snapshot_state` achieves by replacing the map outright.
    //
    // A source held back because its compile read a file emptied since the batch looked
    // leaves the hold running (#380): an emptied file then keeps the stamp that saw its
    // bytes, so the next look finds it emptied still and the hold is kept.
    let fresh = snapshot_state(&state.watched_set());
    state.last_mtimes = if state.hold.deadline().is_some() {
        baseline_keeping_emptied(&state.last_mtimes, fresh)
    } else {
        fresh
    };
    any_changed
}

/// Whether `path`, which a directory batch names, is a source of the watch below `root`: a
/// `.mds` file there, outside the directories the walk skips — what a full walk would
/// find (#380). A dependency the batch names, an in-root module that is no `.mds` file
/// included, is none.
fn names_a_source(root: &Path, path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "mds")
        && path.starts_with(root)
        && !is_within_default_excluded_dir(root, path)
}

/// Full recompile of every source triggered by a vars-file change: the known ones, and
/// those `changed` names that no walk has found ([`names_a_source`]) — one created in the
/// same batch, or in a batch held with it, which is then known as the walk's are (#380).
/// A dependency `changed` names is recompiled through its importers, which all are.
///
/// Recomputes the entire forward-deps graph, external-dep-dirs, and errored set
/// from scratch (prunes stale entries left over from deleted sources).
///
/// Also runs the same deletion cleanup that `process_dir_batch_incremental` does so
/// that a `.mds` deleted in the same debounce window as a vars edit does not orphan its
/// output `.md` or leave stale `last_written` / `forward_deps` / `errored` entries
/// (rust.md / reliability issue #3 fix). A source found gone later in the batch — before
/// its compile, during it, or after it — is retired the same way (#160).
///
/// Uses `compile_one_source` for the shared compile→dedup→write sequence.
///
/// Returns `true` when at least one source in the batch produced an observable,
/// content-changed rebuild (see `compile_one_source`).
fn process_dir_batch_vars_changed(
    changed: &BTreeSet<PathBuf>,
    watch_root: &WatchedPath,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) -> bool {
    let root = watch_root.canonical.as_path();
    let mut any_changed = false;
    let all_sources: BTreeSet<PathBuf> = state
        .known_files
        .iter()
        .chain(changed.iter().filter(|path| names_a_source(root, path)))
        .cloned()
        .collect();

    // Determine which of them no longer exist — their outputs are retired just as in the
    // incremental deletion step (step 5), by the same rule (#160).
    for del_src in all_sources.iter().filter(|p| !p.exists()) {
        state.retire_deleted(del_src, quiet);
    }

    // Snapshot the old maps, clear them so compile_one_source's record_success
    // fills fresh copies (ensures stale entries from deleted sources are pruned).
    state.forward_deps.clear();
    state.errored.clear();
    state.external_dep_dirs.clear();

    for src in &all_sources {
        // A source gone since the pass above is a deleted source too (#160).
        if !src.exists() {
            state.retire_deleted(src, quiet);
            continue;
        }
        if compile_one_source(src, watch_root, output_base, runtime_vars, quiet, state) {
            any_changed = true;
        }
    }

    // Keep the sources still there. One gone since its compile is a deleted source as
    // well, retired by the same rule rather than dropped with its outputs left (#160).
    let (present, gone): (BTreeSet<PathBuf>, BTreeSet<PathBuf>) =
        all_sources.into_iter().partition(|p| p.exists());
    for src in &gone {
        state.retire_deleted(src, quiet);
    }
    state.known_files = present;
    any_changed
}

/// Incremental recompile: compile only transitive importers of the changed seeds.
///
/// Steps:
/// 1. Partition changed paths into `existing` / `deleted`.
/// 2. Compute seeds = existing ∪ deleted ∪ (errored ∩ real-change batch).
/// 3. Compute affected = transitive importers of seeds (freshness-rule snapshot).
/// 4. Compile each affected source that exists and is not an external-only dep; one in
///    the root found gone here, or by its compile, is retired as a deleted source (#160).
/// 5. Delete outputs for removed sources.
///
/// Uses `compile_one_source` for the shared compile→dedup→write sequence.
///
/// Returns `true` when at least one affected source produced an observable,
/// content-changed rebuild (see `compile_one_source`).
fn process_dir_batch_incremental(
    changed: &BTreeSet<PathBuf>,
    watch_root: &WatchedPath,
    output_base: &OutputBase,
    runtime_vars: &Option<HashMap<String, mds::Value>>,
    quiet: bool,
    state: &mut DirWatchState,
) -> bool {
    let root = watch_root.canonical.as_path();
    let mut any_changed = false;

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
        return false;
    }

    // 3. Affected = seeds ∪ transitive importers (uses start-of-batch graph snapshot).
    let affected = affected_sources(&state.forward_deps, &seeds);

    // A debug build's test pause (#160): the batch has told its sources still there from
    // those gone, and has compiled none of them.
    pause_after_batch_split();

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
            // If `src` is in the `deleted` set, it is retired in step 5. If it is NOT —
            // deleted since this batch's partition, or seeded from `errored` while its
            // delete event never came (issue #7) — a source in the root is a deleted
            // source all the same: its outputs are retired now, by the same rule, and its
            // records go with them, so it does not stay a ghost entry (#160). An external
            // dependency has no output (#217), and is only forgotten.
            if !deleted.contains(src) {
                if is_in_root {
                    state.retire_deleted(src, quiet);
                } else {
                    state.forget(src);
                }
            }
            continue;
        }

        // External deps (out-of-root) AND excluded-in-root paths (node_modules/, .git/,
        // hidden dirs) are graph nodes but never emit their own output (DD3 pattern).
        if !is_in_root || is_excluded_in_root {
            // Compile to refresh deps only; suppress output by using quiet=true.
            match watch_root.compile_source(src, runtime_vars.clone(), true) {
                Ok(compiled) => {
                    state
                        .forward_deps
                        .insert(src.clone(), graph_keys(&compiled.dependencies));
                    state.errored.remove(src);
                }
                Err(failure) => {
                    settle(
                        SettleInto::Dir(state),
                        failure.unreported(),
                        Settle::MarkErrored(src),
                    );
                }
            }
            continue;
        }

        // In-root source: full compile→dedup→write via shared helper.
        if compile_one_source(src, watch_root, output_base, runtime_vars, quiet, state) {
            any_changed = true;
        }
    }

    // 5. Deletions: after importers recompiled, retire the outputs and forget the graph
    //    records of each deleted source (#160).
    for del_src in &deleted {
        state.retire_deleted(del_src, quiet);
    }

    // 6. Prune external_dep_dirs to only dirs still referenced by live forward_deps.
    //
    // `external_dep_dirs` is monotonically grown by `record_success` on every compile
    // (issue #2 / reliability.md): when a cross-root @import is edited away, the now-
    // unused dir stays in the set, causing the liveness probe to re-arm it on every tick
    // forever. Recompute from the current `forward_deps` after each batch so abandoned
    // external dirs are unwatched and removed (applies the reconcile rule / mirrors the prune
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
    any_changed
}

// ── Test-only pause between a directory batch's split and its compile (#160) ──

/// `MDS_TEST_PAUSE_AFTER_BATCH_SPLIT`: how a debug build is made to stop a directory
/// watch's rebuild batch once it has told the sources still there from the sources gone,
/// and before it compiles them — after an incremental batch's partition, and again just
/// before each source's compile — so that a test can delete a source in that window
/// (`tests/cli_watch.rs`). A release build has none of it.
#[cfg(debug_assertions)]
mod batch_pause_trigger {
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::output::WriteTarget;
    use crate::write::{atomic_write_file, Durability, Parents};

    /// The variable naming the file that ends the pause. The run writes the same name with
    /// `.paused` appended once it has stopped, for the test to wait for.
    const VARIABLE: &str = "MDS_TEST_PAUSE_AFTER_BATCH_SPLIT";

    /// How long the pause waits between two looks for the file that ends it.
    const POLL: Duration = Duration::from_millis(5);

    /// How many looks the pause makes before the batch goes on regardless: ten seconds.
    const MAX_POLLS: u32 = 2_000;

    /// Stop here when `MDS_TEST_PAUSE_AFTER_BATCH_SPLIT` names a file: say so by writing
    /// `<file>.paused`, then wait until `<file>` exists, or until [`MAX_POLLS`] looks have
    /// found none.
    pub(super) fn pause_after_batch_split() {
        let Some(go) = std::env::var_os(VARIABLE).map(PathBuf::from) else {
            return;
        };
        let mut paused = go.clone().into_os_string();
        paused.push(".paused");
        // A marker that cannot be written leaves the test waiting for it, which the test
        // reports as a batch that never paused.
        let _ = atomic_write_file(
            &WriteTarget::as_typed(PathBuf::from(paused)),
            "",
            Durability::RenameOnly,
            Parents::Existing,
        );
        for _ in 0..MAX_POLLS {
            if go.exists() {
                return;
            }
            std::thread::sleep(POLL);
        }
    }
}

#[cfg(debug_assertions)]
use batch_pause_trigger::pause_after_batch_split;

/// A release build's pause between a directory batch's split and its compile: none.
#[cfg(not(debug_assertions))]
fn pause_after_batch_split() {}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// #265: a `--vars` path carrying a forbidden character is refused before the
    /// filesystem is touched — whether or not it exists — with the message
    /// `NativeFs::check_symlink` gives an existing one; a clean missing path is still
    /// kept for later creation (control).
    #[test]
    fn canonicalize_vars_path_refuses_a_forbidden_char_before_existence() {
        let dir = tempfile::tempdir().unwrap();
        for ch in ['\t', '\x1b', '\n', '\u{202E}'] {
            let hostile = dir.path().join(format!("v{ch}q")).join("x.json");
            let err = canonicalize_vars_path(Some(hostile.clone()))
                .expect_err("a hostile --vars path is refused");
            let s = err.serialize();
            assert_eq!(s.code, "mds::io", "U+{:04X}", u32::from(ch));
            assert!(
                s.message.starts_with(&format!(
                    "path contains forbidden character U+{:04X}: \"",
                    u32::from(ch)
                )),
                "got: {}",
                s.message
            );
            assert!(!s.message.contains(ch), "raw char: {:?}", s.message);
        }

        let clean = dir.path().join("missing.json");
        assert_eq!(
            canonicalize_vars_path(Some(clean.clone())).unwrap(),
            Some(clean),
            "control: a clean missing --vars path is kept"
        );
    }

    // T-U1: dirs_to_watch deduplicates parents.
    #[test]
    fn dirs_to_watch_deduplicates_parents() {
        let entry = PathBuf::from("/project/src/entry.mds");
        let deps = vec![
            PathBuf::from("/project/src/a.mds"),
            PathBuf::from("/project/src/b.mds"), // same parent as entry
            PathBuf::from("/project/lib/c.mds"), // different parent
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
        let deps = vec![PathBuf::from("/a/dep1.mds"), PathBuf::from("/b/dep2.mds")];
        let vars = PathBuf::from("/c/vars.json");
        let foi = files_of_interest(&entry, &deps, Some(&vars));
        assert!(foi.contains(&PathBuf::from("/a/entry.mds")));
        assert!(foi.contains(&PathBuf::from("/a/dep1.mds")));
        assert!(foi.contains(&PathBuf::from("/b/dep2.mds")));
        assert!(foi.contains(&PathBuf::from("/c/vars.json")));
        assert_eq!(foi.len(), 4);
    }

    /// #390: a graph key and file mode's content-dedup key are paths, never their text.
    /// Two paths whose names differ only in bytes that are not UTF-8 are one text once
    /// made lossy — the key the text mapping built, which let them share one graph node
    /// and one dedup entry — and stay two keys.
    ///
    /// The paths are built from bytes and no such file is created, so it runs on every
    /// unix: macOS's filesystem refuses such a name, but nothing here asks it for one.
    #[cfg(unix)]
    #[test]
    fn distinct_non_utf8_paths_stay_distinct_graph_and_output_keys() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join(OsStr::from_bytes(b"\xff.mds"));
        let b = dir.path().join(OsStr::from_bytes(b"\xfe.mds"));
        // Control: one text once made lossy.
        assert_ne!(a, b);
        assert_eq!(a.display().to_string(), b.display().to_string());

        let keys = graph_keys(&[a.clone(), b.clone()]);
        assert_eq!(keys.len(), 2);
        assert_ne!(keys[0], keys[1], "two dependencies, two graph keys");
        assert_eq!(keys[0].file_name(), a.file_name(), "a key keeps its bytes");

        let key = |p: &Path| OutputKey::of(Some(&WriteTarget::as_typed(p.with_extension("md"))));
        assert_ne!(key(&a), key(&b), "two outputs, two dedup keys");
        assert_ne!(key(&a), OutputKey::Stdout);
        assert_eq!(OutputKey::of(None), OutputKey::Stdout);
    }

    /// #409: watch keys a compile's dependencies by [`graph_key`] — the canonical form
    /// notify event paths are compared in — from the list the compiler reports, which
    /// `compile_to_content` passes on as it is. On Windows the key is verbatim while the
    /// reported path is not: the mismatch the mapping exists for.
    #[test]
    fn a_compile_s_dependencies_are_keyed_as_notify_reports_them() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.mds"),
            "@import \"./lib.mds\" as lib\n{{lib.hi()}}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("lib.mds"), "@define hi():\nHi\n@end\n").unwrap();
        let main = dir.path().join("main.mds");

        let compiled =
            compile_to_content(&main, None, true, mds::CompileOptions::default()).unwrap();
        let reported = mds::compile_with_deps(&main, None).unwrap().dependencies;
        assert_eq!(
            compiled.dependencies, reported,
            "the compiler's list, passed on as it is"
        );
        assert_eq!(
            graph_keys(&compiled.dependencies),
            [graph_key(&dir.path().join("lib.mds"))],
            "the dependency is keyed by the watch graph key of the imported file"
        );

        #[cfg(windows)]
        {
            assert!(graph_keys(&reported)[0]
                .to_string_lossy()
                .starts_with(r"\\?\"));
            assert!(!reported[0].starts_with(r"\\?\"));
        }
    }

    /// #390: a message about watching a directory names the entry's directory in file
    /// mode, or the directory argument, as typed, and a dependency's directory below it
    /// below it as typed; the `--vars` file's directory as that file was typed. A
    /// dependency's directory outside both has no typed form and keeps the path the
    /// compile reported. Every line that names a watched directory goes through
    /// `shown_watched_dir`: file mode's startup `failed to watch directory` and
    /// rebuild-time `failed to watch`, directory mode's `failed to watch directory`,
    /// `failed to watch vars directory` and `failed to watch external dep dir`.
    #[test]
    fn a_watched_directory_is_named_as_the_user_typed_it() {
        use std::ffi::OsStr;

        // File mode: `mds watch page.mds --vars ../v.json` in `/project`.
        let entry = WatchedPath {
            typed: PathBuf::from("page.mds"),
            canonical: PathBuf::from("/project/page.mds"),
            what: Watched::Entry,
        };
        let vars = vars_dir_paths(
            Some(Path::new("/elsewhere/v.json")),
            Some(Path::new("../v.json")),
        );
        let shown = |dir: &str| shown_watched_dir(Path::new(dir), entry.dir_paths(), vars);

        // As typed exactly — no separator added to the directory itself.
        assert_eq!(shown("/project").as_os_str(), OsStr::new("."));
        assert_eq!(shown("/elsewhere").as_os_str(), OsStr::new(".."));
        // A dependency's directory below the entry's is named below it as typed.
        assert_eq!(shown("/project/lib"), Path::new("./lib"));
        assert_eq!(shown("/project/lib/deep"), Path::new("./lib/deep"));
        // Outside the entry's directory: no typed form, below the `--vars` directory
        // included.
        assert_eq!(shown("/lib"), Path::new("/lib"));
        assert_eq!(shown("/elsewhere/sub"), Path::new("/elsewhere/sub"));
        assert_eq!(
            shown_watched_dir(Path::new("/elsewhere"), entry.dir_paths(), None),
            Path::new("/elsewhere")
        );
        // A `--vars` file beside the entry: its directory is the entry's, named as such.
        let beside = vars_dir_paths(
            Some(Path::new("/project/v.json")),
            Some(Path::new("../project/v.json")),
        );
        assert_eq!(
            shown_watched_dir(Path::new("/project"), entry.dir_paths(), beside).as_os_str(),
            OsStr::new(".")
        );

        // Directory mode: `mds watch src --vars cfg/v.json`, the `--vars` directory armed
        // as `cfg` — what a missing file's directory is, as typed.
        let root = WatchedPath {
            typed: PathBuf::from("src"),
            canonical: PathBuf::from("/project/src"),
            what: Watched::Root,
        };
        let vars = Some(RootPaths {
            typed: Path::new("cfg"),
            walked: Path::new("cfg"),
        });
        let shown = |dir: &str| shown_watched_dir(Path::new(dir), root.root_paths(), vars);
        assert_eq!(shown("/project/src").as_os_str(), OsStr::new("src"));
        assert_eq!(shown("/project/src/sub"), Path::new("src/sub"));
        assert_eq!(shown("cfg").as_os_str(), OsStr::new("cfg"));
        assert_eq!(shown("/project/shared"), Path::new("/project/shared"));
    }

    /// #257: a dependency directory outside the root is held as armed only once its
    /// watch is in place. One whose watch failed, though it exists, is tried again by the
    /// next call — the next rebuild's — and stays unarmed for the liveness tick to try; one
    /// already armed is not watched again.
    #[test]
    fn an_external_dep_dir_is_armed_only_once_its_watch_succeeds() {
        let scratch = tempfile::tempdir().unwrap();
        let (ok, refused) = (scratch.path().join("ok"), scratch.path().join("refused"));
        std::fs::create_dir(&ok).unwrap();
        std::fs::create_dir(&refused).unwrap();
        let dirs = BTreeSet::from([ok.clone(), refused.clone()]);
        let root = RootPaths {
            typed: Path::new("src"),
            walked: Path::new("/project/src"),
        };

        let mut armed = BTreeSet::new();
        let mut watched = Vec::new();
        let first = |dir: &Path| {
            watched.push(dir.to_path_buf());
            if dir == refused {
                Err(notify::Error::generic("refused"))
            } else {
                Ok(())
            }
        };
        arm_external_dep_dirs(&dirs, &mut armed, first, root, None);
        // Control: both directories exist, so existence cannot tell them apart.
        assert!(ok.is_dir() && refused.is_dir());
        assert_eq!(watched, [ok.clone(), refused.clone()]);
        assert_eq!(
            armed,
            BTreeSet::from([ok.clone()]),
            "only the directory whose watch succeeded is armed"
        );

        let mut again = Vec::new();
        let second = |dir: &Path| {
            again.push(dir.to_path_buf());
            Ok(())
        };
        arm_external_dep_dirs(&dirs, &mut armed, second, root, None);
        assert_eq!(
            again,
            [refused],
            "the refused directory is tried again, the armed one is not"
        );
        assert_eq!(armed, dirs);
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

    /// An out-dir whose two forms are the same path.
    fn dir_base(d: impl Into<PathBuf>) -> OutputBase {
        let d = d.into();
        OutputBase::Dir {
            canonical: d.clone(),
            shown: d,
            below_anchor: 0,
        }
    }

    /// A loaded `mds.json` in `/project` with the given `build.output_dir`, reached as `.`.
    fn project_config(output_dir: &str) -> Option<ProjectConfig> {
        use crate::build::{BuildConfig, MdsConfig};
        Some(ProjectConfig {
            config: MdsConfig {
                build: BuildConfig {
                    output_dir: Some(output_dir.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            dir: PathBuf::from("/project"),
            shown_dir: PathBuf::from("."),
        })
    }

    // Mirroring: subtree preserved.
    #[test]
    fn output_path_for_mirrors_subtree() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/a/b/foo.mds");
        let base = dir_base("/out");
        let result = output_path_for(&source, RootPaths::as_typed(&root), &base, "md");
        assert_eq!(result.path, PathBuf::from("/out/a/b/foo.md"));
    }

    // No stem collision: two files with the same stem in different subdirs.
    #[test]
    fn output_path_for_no_stem_collision() {
        let root = PathBuf::from("/root");
        let a = PathBuf::from("/root/a/x.mds");
        let b = PathBuf::from("/root/b/x.mds");
        let base = dir_base("/out");
        assert_ne!(
            output_path_for(&a, RootPaths::as_typed(&root), &base, "md"),
            output_path_for(&b, RootPaths::as_typed(&root), &base, "md"),
            "two files with the same stem in different subdirs must not collide"
        );
        assert_eq!(
            output_path_for(&a, RootPaths::as_typed(&root), &base, "md").path,
            PathBuf::from("/out/a/x.md")
        );
        assert_eq!(
            output_path_for(&b, RootPaths::as_typed(&root), &base, "md").path,
            PathBuf::from("/out/b/x.md")
        );
    }

    // NextToSource: default mode places .md next to source.
    #[test]
    fn output_path_for_next_to_source() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/a/b/foo.mds");
        let result = output_path_for(
            &source,
            RootPaths::as_typed(&root),
            &OutputBase::NextToSource,
            "md",
        );
        assert_eq!(result.path, PathBuf::from("/root/a/b/foo.md"));
    }

    // Compound extension and extensionless stem.
    #[test]
    fn output_path_for_compound_extension() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/foo.bar.mds");
        let base = dir_base("/out");
        let result = output_path_for(&source, RootPaths::as_typed(&root), &base, "md");
        assert_eq!(result.path, PathBuf::from("/out/foo.bar.md"));
    }

    // Path-escape guard (AC-M7): source outside root stays inside out-dir.
    #[test]
    fn output_path_for_source_outside_root_stays_contained() {
        let root = PathBuf::from("/root");
        // Source is completely outside root — strip_prefix will fail.
        let source = PathBuf::from("/elsewhere/a/b/foo.mds");
        let base = dir_base("/out");
        let result = output_path_for(&source, RootPaths::as_typed(&root), &base, "md");
        // Must be inside /out, not escape to /elsewhere.
        assert!(
            result.path.starts_with("/out"),
            "output must stay inside out-dir even when source is outside root; got {result:?}"
        );
        // Must not join an absolute path that escapes out-dir.
        assert_eq!(result.path, PathBuf::from("/out/foo.md"));
    }

    // resolve_output_base: --out-dir takes precedence over mds.json's build.output_dir.
    // The out-dir is absolute on every host (a rooted `/my/out` is not, on Windows) and
    // does not exist, so it resolves to itself and is shown as typed.
    #[test]
    fn resolve_output_base_outdir_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("out");
        let result = resolve_output_base(Some(&d), &project_config("dist")).unwrap();
        assert!(
            matches!(result, OutputBase::Dir { ref canonical, ref shown, below_anchor: 0 }
                if canonical == &d && shown == &d),
            "expected Dir({d:?}), got {result:?}"
        );
    }

    // resolve_output_base: mds.json config used when no --out-dir.
    #[test]
    fn resolve_output_base_config_used_when_no_outdir() {
        let result = resolve_output_base(None, &project_config("dist")).unwrap();
        assert!(
            matches!(result, OutputBase::Dir { ref canonical, .. }
                if canonical == &PathBuf::from("/project/dist")),
            "expected Dir(/project/dist), got {result:?}"
        );
    }

    // resolve_output_base: `..` in output_dir rejected at startup.
    #[test]
    fn resolve_output_base_rejects_dotdot() {
        let result = resolve_output_base(None, &project_config("../bad"));
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
    // trigger recovery (reconcile rule / AC-P1 — no per-tick full-tree walk).
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

    // snapshot_state / state_differs: the size leg of the (mtime, size) stamp.
    //
    // The rewrite changes the length, so the change is visible however coarse the
    // filesystem's mtime clock is. Two back-to-back writes of equal length can share
    // one mtime tick (NTFS, FAT, jiffy-granular ext4) — the mtime leg is pinned
    // deterministically by `snapshot_and_diff_detect_same_size_mtime_change`.
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
        std::fs::write(&f, "v2 — longer").unwrap();
        assert!(state_differs(&paths, &snap), "should detect content change");
    }

    // snapshot_state / state_differs: the mtime leg of the (mtime, size) stamp.
    //
    // A same-size rewrite is detectable only through mtime, so the test sets the
    // mtime explicitly instead of relying on the clock ticking between two writes.
    #[test]
    fn snapshot_and_diff_detect_same_size_mtime_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("test.mds");
        std::fs::write(&f, "v1").unwrap();

        let paths: HashSet<PathBuf> = std::iter::once(f.clone()).collect();
        let snap = snapshot_state(&paths);
        let (t0, size0) = snap[&f];
        let t0 = t0.expect("mtime must be readable on the test filesystem");

        std::fs::write(&f, "v2").unwrap();
        let set_mtime = |t: std::time::SystemTime| {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&f)
                .unwrap()
                .set_modified(t)
                .unwrap();
        };
        assert_eq!(
            std::fs::metadata(&f).unwrap().len(),
            size0.unwrap(),
            "precondition: the rewrite keeps the size, so only mtime can differ"
        );

        // Control: with the mtime restored to the snapshot's, the stamp is identical —
        // this is the coarse-clock case, and it proves the setter controls the stamp.
        set_mtime(t0);
        assert!(
            !state_differs(&paths, &snap),
            "control: same size and same mtime must compare equal"
        );

        set_mtime(t0 + Duration::from_secs(2));
        assert!(
            state_differs(&paths, &snap),
            "should detect an mtime-only change"
        );
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

    // ── Idle-tick schedule (#319, #397) ──────────────────────────────────────
    //
    // The probe-starvation defect. These assert the two properties that make the idle
    // tick a usable backstop rather than a best-effort one: it fires under load, and
    // it does not fire more often than its interval. They run on synthetic instants:
    // `Instant::now()` is read once per test as an arbitrary origin, and every other
    // instant is an exact offset from it, so no assertion depends on how fast the
    // runner is.

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Runs `driver` on a worker thread and waits at most 5s for its result, so a driver
    /// that lost its bound fails the test by name rather than hanging the test binary.
    ///
    /// The worker is deliberately not joined on the timeout path: it is stuck because
    /// its bound is gone, and joining it would bring the hang back. A worker waiting on
    /// the watch channel ends once the test unwinds and drops the channel's sender.
    fn within_5s<T: Send + 'static>(what: &str, driver: impl FnOnce() -> T + Send + 'static) -> T {
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            // The test may already have given up; the send failing is fine.
            let _ = done_tx.send(driver());
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|e| panic!("{what} did not return within 5s ({e:?})"))
    }

    /// Replays [`TickClock::recv_next`] on a channel that is never empty: every poll
    /// that is not `Due` takes a message, and handling one moves the clock on by
    /// `per_message`. Returns when each tick came due before `until`, as offsets.
    fn ticks_under_saturation(
        interval: Duration,
        per_message: Duration,
        until: Duration,
    ) -> Vec<Duration> {
        let t0 = Instant::now();
        let mut schedule = TickSchedule::start(Some(interval), t0);
        let mut now = t0;
        let mut ticks = Vec::new();
        // Bounded: one step per message or tick.
        for _ in 0..10_000 {
            if now >= t0 + until {
                return ticks;
            }
            match schedule.poll(now) {
                TickPoll::Due => ticks.push(now - t0),
                TickPoll::Wait(_) | TickPoll::Never => now += per_message,
            }
        }
        panic!("the replay did not reach {until:?} within 10 000 steps; ticks so far: {ticks:?}");
    }

    /// The tick comes due one interval after the session starts, and re-arms from the
    /// instant it fired — whether a poll found it due or the driver's wait ran out.
    #[test]
    fn tick_schedule_comes_due_one_interval_after_it_starts() {
        let t0 = Instant::now();
        let mut schedule = TickSchedule::start(Some(ms(50)), t0);
        assert_eq!(schedule.poll(t0), TickPoll::Wait(ms(50)));
        assert_eq!(schedule.poll(t0 + ms(49)), TickPoll::Wait(ms(1)));
        assert_eq!(
            schedule.poll(t0 + ms(50)),
            TickPoll::Due,
            "an idle channel must produce a tick one interval in"
        );
        assert_eq!(
            schedule.poll(t0 + ms(50)),
            TickPoll::Wait(ms(50)),
            "a due tick re-arms one interval after it fired"
        );

        // The driver's wait for a message ran out at the deadline: it re-arms from there.
        schedule.rearm(t0 + ms(100));
        assert_eq!(schedule.poll(t0 + ms(100)), TickPoll::Wait(ms(50)));
        assert_eq!(schedule.poll(t0 + ms(150)), TickPoll::Due);
    }

    /// A due tick is reported before the next message is taken, so a channel that is
    /// never empty cannot postpone it (#319).
    ///
    /// The starvation bug handed `recv_timeout` a fresh interval per message, so a
    /// sender faster than the tick rate postponed the probe forever. Here a message is
    /// always waiting: each tick still comes due on the first poll at or after its
    /// deadline — on time when the message cadence divides the interval, otherwise at
    /// most one message late — and each next one a full interval after it.
    #[test]
    fn tick_schedule_is_due_first_under_saturation() {
        assert_eq!(
            ticks_under_saturation(ms(50), ms(5), ms(210)),
            [ms(50), ms(100), ms(150), ms(200)],
            "a saturated channel must not postpone a tick past its deadline"
        );
        assert_eq!(
            ticks_under_saturation(ms(50), ms(7), ms(230)),
            [ms(56), ms(112), ms(168), ms(224)],
            "a tick is due at the first poll at or after its deadline, under saturation too"
        );
    }

    /// Two ticks are never closer together than one interval, however loaded the loop.
    ///
    /// The counterpart to the test above: making the tick non-starvable must not make
    /// it free-running. A deadline that advanced from the *previous* deadline rather
    /// than from the moment the tick was observed would fire a catch-up burst after
    /// any slow probe.
    #[test]
    fn tick_schedule_rearms_from_the_tick_it_observed() {
        let t0 = Instant::now();
        let interval = ms(50);
        let mut schedule = TickSchedule::start(Some(interval), t0);
        assert_eq!(schedule.poll(t0 + ms(50)), TickPoll::Due, "first tick");

        // A probe that overran its own interval: the next poll comes 120ms later.
        assert_eq!(
            schedule.poll(t0 + ms(170)),
            TickPoll::Due,
            "an overdue tick must fire immediately, not wait another interval"
        );
        assert_eq!(
            schedule.poll(t0 + ms(170)),
            TickPoll::Wait(interval),
            "the tick following an overdue one must wait a full interval, not fire a \
             catch-up burst"
        );
        assert_eq!(schedule.poll(t0 + ms(219)), TickPoll::Wait(ms(1)));
        assert_eq!(schedule.poll(t0 + ms(220)), TickPoll::Due);
    }

    /// `--poll-interval 0`: no tick ever comes due, however long the session runs.
    #[test]
    fn tick_schedule_without_interval_never_comes_due() {
        let t0 = Instant::now();
        let mut off = TickSchedule::start(None, t0);
        let mut on = TickSchedule::start(Some(ms(50)), t0);
        for later in [Duration::ZERO, ms(50), Duration::from_secs(3_600)] {
            assert_eq!(off.poll(t0 + later), TickPoll::Never, "at {later:?}");
        }
        assert_eq!(
            on.poll(t0 + ms(50)),
            TickPoll::Due,
            "positive control: with an interval the same instant is due"
        );
    }

    /// `--poll-interval 0` with no rebuild held: `recv_next` blocks for a message.
    #[test]
    fn tick_clock_without_interval_never_ticks() {
        let (tx, rx) = mpsc::channel::<Msg>();
        let mut clock = TickClock::new(None);
        tx.send(Msg::Interrupt).expect("send failed");
        assert!(
            matches!(
                clock.recv_next(&rx, None),
                Ok(Wake::Message(Msg::Interrupt))
            ),
            "with no poll interval the clock must deliver the message, never a tick"
        );
        drop(tx);
        assert!(
            matches!(clock.recv_next(&rx, None), Err(mpsc::RecvError)),
            "a closed channel must report Disconnected rather than tick forever"
        );
    }

    /// The driver serves a due tick before it takes a waiting message (#319). The
    /// schedule has already re-armed when it reports the tick due, so a driver that took
    /// the message first would lose that tick, and a channel that is never empty would
    /// starve the probe again.
    ///
    /// The schedule is built already due, with an interval of an hour: the second call
    /// cannot find another tick due however slow the runner is.
    #[test]
    fn tick_clock_serves_a_due_tick_before_a_waiting_message() {
        let (tx, rx) = mpsc::channel::<Msg>();
        tx.send(Msg::Interrupt).expect("send failed");
        let mut clock = TickClock {
            schedule: TickSchedule::Every {
                interval: Duration::from_secs(3_600),
                next: Instant::now(),
            },
        };

        assert!(
            matches!(clock.recv_next(&rx, None), Ok(Wake::Tick)),
            "a due tick must be served before the message waiting in the channel"
        );
        assert!(
            matches!(
                clock.recv_next(&rx, None),
                Ok(Wake::Message(Msg::Interrupt))
            ),
            "positive control: the waiting message is delivered next, not lost"
        );
        drop(tx);
    }

    /// When the driver's wait for a message runs out, it serves the tick and re-arms one
    /// interval from that instant. Without the re-arm the deadline stays in the past,
    /// and the next call reports a second tick at once.
    ///
    /// A lower bound only: `t1` is read before the clock starts, the wait cannot run out
    /// before the first deadline (`t1 + interval` at the earliest), and the re-arm adds
    /// one interval to the instant it ran out. The sender stays alive, so the driver
    /// runs under [`within_5s`]: one whose wait never ran out fails by name.
    #[test]
    fn tick_clock_rearms_when_its_wait_runs_out() {
        let interval = ms(50);
        let (tx, rx) = mpsc::channel::<Msg>();
        let t1 = Instant::now();
        let (wake, clock, _rx) = within_5s("an idle tick", move || {
            let mut clock = TickClock::new(Some(interval));
            (clock.recv_next(&rx, None), clock, rx)
        });

        assert!(
            matches!(wake, Ok(Wake::Tick)),
            "an idle channel's wait runs out at the tick"
        );
        let TickSchedule::Every { next, .. } = clock.schedule else {
            panic!("a clock started with an interval keeps it");
        };
        assert!(
            next >= t1 + interval * 2,
            "the next tick must come due one interval after the wait ran out, not at the \
             deadline that has just passed: due {:?} after the clock started, expected at \
             least {:?}",
            next - t1,
            interval * 2
        );
        drop(tx);
    }

    // ── Empty-file hold (#380) ───────────────────────────────────────────────
    //
    // A rebuild held while a watched file is empty runs at a deadline fixed by the first
    // rebuild that found the file emptied. These run on synthetic instants, as the tick
    // tests above do.

    /// A file the baseline saw with bytes and finds with none went empty; nothing else
    /// did.
    #[test]
    fn went_empty_is_a_file_with_bytes_found_with_none() {
        let stamp = |size: Option<u64>| -> FileStamp { (Some(std::time::UNIX_EPOCH), size) };
        assert!(
            went_empty(Some(&stamp(Some(5))), &stamp(Some(0))),
            "bytes, then none"
        );
        assert!(
            !went_empty(Some(&stamp(Some(0))), &stamp(Some(0))),
            "a file empty from the start, or compiled since it was emptied"
        );
        assert!(
            !went_empty(None, &stamp(Some(0))),
            "a file the baseline never saw"
        );
        assert!(
            !went_empty(Some(&stamp(None)), &stamp(Some(0))),
            "a file the baseline saw missing"
        );
        assert!(
            !went_empty(Some(&stamp(Some(5))), &stamp(None)),
            "a file that is gone"
        );
        assert!(
            !went_empty(Some(&stamp(Some(5))), &stamp(Some(3))),
            "a file that still has bytes"
        );
    }

    /// The held rebuild's deadline is set by the first rebuild that finds a watched file
    /// emptied, and no later one moves it: each finds the hold running until the deadline,
    /// and the first at it compiles and ends the hold.
    #[test]
    fn empty_hold_deadline_is_fixed_at_the_first_empty_observation() {
        let t0 = Instant::now();
        let mut hold = EmptyHold::default();
        assert_eq!(
            hold.deadline(),
            None,
            "nothing is held before a file is emptied"
        );
        assert_eq!(hold.on_rebuild(true, t0), HoldVerdict::Hold);
        assert_eq!(hold.deadline(), Some(t0 + EMPTY_HOLD_DEADLINE));
        for k in 1..10u64 {
            let now = t0 + ms(100 * k);
            assert_eq!(hold.on_rebuild(true, now), HoldVerdict::Hold, "at {now:?}");
            assert_eq!(
                hold.deadline(),
                Some(t0 + EMPTY_HOLD_DEADLINE),
                "a rebuild {}ms in must not move the deadline",
                100 * k
            );
        }
        let just_before = t0 + EMPTY_HOLD_DEADLINE - Duration::from_nanos(1);
        assert_eq!(hold.on_rebuild(true, just_before), HoldVerdict::Hold);
        assert_eq!(
            hold.on_rebuild(true, t0 + EMPTY_HOLD_DEADLINE),
            HoldVerdict::Compile,
            "at the deadline the files are compiled as they are"
        );
        assert_eq!(
            hold.deadline(),
            None,
            "the rebuild at the deadline ends the hold"
        );
    }

    /// An endless stream of rebuilds that each find the file still emptied — a truncation
    /// every 2ms, every 100ms, every 999ms — compiles at the first rebuild at or past the
    /// deadline the stream's first rebuild set: no stream moves it.
    #[test]
    fn empty_hold_compiles_an_endless_stream_at_its_first_deadline() {
        for (every, compiled_at) in [
            (ms(2), ms(1_000)),
            (ms(100), ms(1_000)),
            (ms(999), ms(1_998)),
        ] {
            let t0 = Instant::now();
            let mut hold = EmptyHold::default();
            // Endless, bounded only by the first compile (and a step cap).
            let compiled = (0u32..)
                .take(10_000)
                .map(|k| t0 + every * k)
                .find(|&now| hold.on_rebuild(true, now) == HoldVerdict::Compile);
            assert_eq!(
                compiled.map(|at| at - t0),
                Some(compiled_at),
                "a stream of rebuilds every {every:?} must compile at its first deadline"
            );
        }
    }

    /// A rebuild that finds no watched file emptied — the content written — compiles at once
    /// and ends the hold; a later truncation then sets a deadline of its own.
    #[test]
    fn empty_hold_ends_when_no_watched_file_is_emptied() {
        let t0 = Instant::now();
        let mut hold = EmptyHold::default();
        assert_eq!(
            hold.on_rebuild(false, t0),
            HoldVerdict::Compile,
            "nothing emptied, nothing held"
        );
        assert_eq!(hold.deadline(), None);
        assert_eq!(hold.on_rebuild(true, t0 + ms(10)), HoldVerdict::Hold);
        assert_eq!(
            hold.on_rebuild(false, t0 + ms(500)),
            HoldVerdict::Compile,
            "written: compiled at once, not at the deadline"
        );
        assert_eq!(hold.deadline(), None, "the write ends the hold");
        assert_eq!(hold.on_rebuild(true, t0 + ms(600)), HoldVerdict::Hold);
        assert_eq!(
            hold.deadline(),
            Some(t0 + ms(600) + EMPTY_HOLD_DEADLINE),
            "a new truncation sets its own deadline"
        );
    }

    /// What woke the loop in a [`wakes_under_saturation`] replay.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Woke {
        Hold,
        Tick,
    }

    /// Replays [`TickClock::recv_next`] with a rebuild held until `hold` (an offset) on a
    /// channel that is never empty: every poll with nothing due takes a message, and
    /// handling one moves the clock on by `per_message`. The held rebuild ends its hold
    /// when it runs, as a session's `on_hold_due` does. Returns each wake before `until`,
    /// as offsets.
    fn wakes_under_saturation(
        interval: Option<Duration>,
        hold: Duration,
        per_message: Duration,
        until: Duration,
    ) -> Vec<(Duration, Woke)> {
        let t0 = Instant::now();
        let mut schedule = TickSchedule::start(interval, t0);
        let mut held = Some(t0 + hold);
        let mut now = t0;
        let mut wakes = Vec::new();
        // Bounded: one step per message or wake.
        for _ in 0..10_000 {
            if now >= t0 + until {
                return wakes;
            }
            match poll_wake(&mut schedule, held, now) {
                WakePoll::HoldDue => {
                    wakes.push((now - t0, Woke::Hold));
                    held = None;
                }
                WakePoll::TickDue => wakes.push((now - t0, Woke::Tick)),
                WakePoll::Wait { .. } | WakePoll::Never => now += per_message,
            }
        }
        panic!("the replay did not reach {until:?} within 10 000 steps; wakes so far: {wakes:?}");
    }

    /// A held rebuild's deadline is served before the next message and before a tick due at
    /// the same instant, the tick right after it; a channel that is never empty postpones
    /// neither — under `--poll-interval 0` as well, where only the hold's own deadline can
    /// wake the loop.
    #[test]
    fn wake_schedule_serves_a_held_rebuild_first_under_saturation() {
        use Woke::{Hold, Tick};
        assert_eq!(
            wakes_under_saturation(Some(ms(50)), ms(120), ms(5), ms(210)),
            [
                (ms(50), Tick),
                (ms(100), Tick),
                (ms(120), Hold),
                (ms(150), Tick),
                (ms(200), Tick)
            ],
            "a saturated channel must postpone neither the tick nor the held rebuild"
        );
        assert_eq!(
            wakes_under_saturation(Some(ms(50)), ms(100), ms(5), ms(160)),
            [
                (ms(50), Tick),
                (ms(100), Hold),
                (ms(100), Tick),
                (ms(150), Tick)
            ],
            "a held rebuild due with a tick runs first, the tick straight after it"
        );
        assert_eq!(
            wakes_under_saturation(None, ms(1_000), ms(7), ms(2_000)),
            [(ms(1_001), Hold)],
            "with no tick the held rebuild still runs at the first poll at or after its \
             deadline"
        );
    }

    /// With no message waiting the loop waits for whichever deadline comes first — the held
    /// rebuild's even under `--poll-interval 0`, where nothing else would wake it — and
    /// blocks with no deadline only when no rebuild is held and no tick runs.
    #[test]
    fn wake_schedule_waits_for_the_earlier_deadline() {
        let t0 = Instant::now();
        let held = Some(t0 + ms(1_000));
        let mut off = TickSchedule::start(None, t0);
        assert_eq!(
            poll_wake(&mut off, held, t0),
            WakePoll::Wait {
                wait: ms(1_000),
                hold: true
            },
            "--poll-interval 0 must still wait for the held rebuild"
        );
        assert_eq!(
            poll_wake(&mut off, held, t0 + ms(999)),
            WakePoll::Wait {
                wait: ms(1),
                hold: true
            }
        );
        assert_eq!(poll_wake(&mut off, held, t0 + ms(1_000)), WakePoll::HoldDue);
        assert_eq!(
            poll_wake(&mut off, None, t0 + ms(1_000)),
            WakePoll::Never,
            "positive control: nothing held and no tick — a message, however long it takes"
        );

        let mut every = TickSchedule::start(Some(ms(50)), t0);
        assert_eq!(
            poll_wake(&mut every, Some(t0 + ms(30)), t0),
            WakePoll::Wait {
                wait: ms(30),
                hold: true
            },
            "the held rebuild's deadline comes first"
        );
        assert_eq!(
            poll_wake(&mut every, Some(t0 + ms(80)), t0),
            WakePoll::Wait {
                wait: ms(50),
                hold: false
            },
            "the tick comes first"
        );
        assert_eq!(
            poll_wake(&mut every, Some(t0 + ms(50)), t0),
            WakePoll::Wait {
                wait: ms(50),
                hold: true
            },
            "a tie waits for the held rebuild, which is served first"
        );
        assert_eq!(
            poll_wake(&mut every, None, t0),
            WakePoll::Wait {
                wait: ms(50),
                hold: false
            }
        );
    }

    /// A directory-mode hold keeps every batch it holds back, and the batch that ends it
    /// rebuilds them all with its own — a vars-file change among them makes it a full
    /// rebuild — leaving nothing held.
    #[test]
    fn held_batches_are_rebuilt_with_the_batch_that_ends_the_hold() {
        let paths =
            |names: &[&str]| -> BTreeSet<PathBuf> { names.iter().map(PathBuf::from).collect() };
        let mut held = HeldBatch::default();
        held.hold(&paths(&["/w/b.mds"]), false);
        held.hold(&paths(&["/w/a.mds", "/w/b.mds"]), true);
        assert_eq!(
            held.release(&paths(&["/w/a.mds", "/w/c.mds"]), false),
            (paths(&["/w/a.mds", "/w/b.mds", "/w/c.mds"]), true)
        );
        assert_eq!(held, HeldBatch::default(), "nothing stays held");
        assert_eq!(
            held.release(&paths(&["/w/c.mds"]), false),
            (paths(&["/w/c.mds"]), false),
            "positive control: with nothing held, a batch is its own"
        );
    }

    /// An output the session wrote with bytes, written with none, is emptied; nothing else
    /// is.
    #[test]
    fn empties_output_is_an_output_with_bytes_written_with_none() {
        assert!(empties_output(Some("Page\n"), ""), "bytes, then none");
        assert!(!empties_output(Some(""), ""), "an output already empty");
        assert!(
            !empties_output(None, ""),
            "an output the session never wrote there"
        );
        assert!(
            !empties_output(Some("Page\n"), "Other\n"),
            "an output still with bytes"
        );
    }

    // ── Debounce window domain (#379) ────────────────────────────────────────

    /// A minimal content event on `path`, shaped like the ones notify delivers.
    fn content_event(path: &str) -> notify::Event {
        notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    /// A read event — the kind `is_content_event` drops.
    fn read_event(path: &str) -> notify::Event {
        notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Read),
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        }
    }

    /// A content event on `path` as the watch channel carries it.
    fn modify_event(path: &str) -> Msg {
        Msg::Fs(Ok(content_event(path)))
    }

    /// Upper bound on the steps of one [`replay`]: the message limit, twice over.
    const REPLAY_STEPS: usize = 2 * MAX_DEBOUNCE_MESSAGES;

    /// How [`drain_debounce`] drives a window, replayed on synthetic instants.
    ///
    /// `events` arrive at their instants, in order. One that arrives before the
    /// window's deadline is drained at its own instant (at once if it was already
    /// queued); otherwise the wait for it runs out at the deadline. Returns the instant
    /// the window closed at, why, and the window as it closed. `events` may be endless:
    /// only as many are taken as the window drains.
    fn replay(
        window: Duration,
        start: Instant,
        events: impl IntoIterator<Item = (Instant, notify::Event)>,
    ) -> (Instant, WindowEnd, DebounceWindow) {
        let mut open = DebounceWindow::open(window, start);
        let mut events = events.into_iter().peekable();
        let mut now = start;
        // Bounded: one step per drained event or ran-out wait.
        for _ in 0..REPLAY_STEPS {
            if let Some(end) = open.classify(now) {
                return (now, end, open);
            }
            let deadline = open.deadline();
            match events.next_if(|(at, _)| *at < deadline) {
                Some((at, event)) => {
                    now = now.max(at);
                    open.on_event(event, now);
                }
                None => now = deadline,
            }
        }
        panic!("the window was still open after {REPLAY_STEPS} steps");
    }

    /// Every content event restarts the window: a burst longer than the window
    /// coalesces into ONE result, which closes one window after the burst's last event.
    ///
    /// A window that expired at a fixed offset from the FIRST event splits any burst
    /// longer than `debounce_ms`; each piece rebuilds separately, against a different
    /// intermediate state of the file.
    #[test]
    fn debounce_window_quiet_end_is_one_window_after_the_last_content_event() {
        let t0 = Instant::now();
        let (closed_at, end, _) = replay(ms(100), t0, std::iter::empty());
        assert_eq!(
            (closed_at, end),
            (t0 + ms(100), WindowEnd::Quiet),
            "with no further event the window closes one window after it opened"
        );

        // 40 events, 5ms apart: a 195ms burst under a 100ms window.
        let burst = (0..40u32).map(|i| (t0 + ms(5) * i, content_event(&format!("/w/f{i}.mds"))));
        let (closed_at, end, closed) = replay(ms(100), t0, burst);
        assert_eq!(
            end,
            WindowEnd::Quiet,
            "a 195ms burst under a 100ms window must end quiet, not capped"
        );
        assert_eq!(
            closed_at,
            t0 + ms(295),
            "the window closes one window after the burst's last event (195ms)"
        );
        assert_eq!(
            closed.classify(t0 + ms(294)),
            None,
            "one millisecond earlier the window is still open"
        );
        assert_eq!(
            closed.paths.len(),
            40,
            "every path in the burst must be collected into the one window; got {:?}",
            closed.paths
        );
    }

    /// The cap ends a stream that never goes quiet, exactly `debounce_cap(window)`
    /// after the window opened.
    ///
    /// Without it an extendable window is unbounded: a file written to continuously
    /// postpones its own rebuild — and the idle-tick backstop behind it — for as long
    /// as the writing lasts.
    #[test]
    fn debounce_window_cap_ends_an_endless_stream_exactly_at_start_plus_cap() {
        // (window, one content event every .., cap): every gap is shorter than the
        // window, so the window never goes quiet; the stream never ends.
        let streams = [
            (ms(50), ms(2), ms(1_000)),
            (ms(10), ms(9), ms(1_000)),
            (ms(250), ms(100), ms(2_500)),
            (ms(1_000), ms(999), ms(10_000)),
        ];
        for (window, every, cap) in streams {
            let t0 = Instant::now();
            let endless = (1u32..).map(|i| (t0 + every * i, content_event("/w/hot.mds")));
            let (closed_at, end, closed) = replay(window, t0, endless);
            assert_eq!(
                end,
                WindowEnd::Cap,
                "a {window:?} window under an event every {every:?} must end at the cap"
            );
            assert_eq!(
                closed_at,
                t0 + cap,
                "a {window:?} window must close exactly at its cap, max(10 x window, 1s)"
            );
            assert_eq!(closed.paths.len(), 1, "every event names the same path");
        }
    }

    /// `--debounce 0` opens no window and consumes nothing.
    #[test]
    fn debounce_zero_is_disabled_and_leaves_the_channel_untouched() {
        let (tx, rx) = mpsc::channel::<Msg>();
        tx.send(modify_event("/w/a.mds")).expect("send failed");

        let outcome = drain_debounce(&rx, 0);

        assert_eq!(outcome.end, DebounceEnd::Disabled);
        assert!(
            outcome.paths.is_empty(),
            "a disabled window must collect nothing"
        );
        assert!(
            matches!(rx.try_recv(), Ok(Msg::Fs(Ok(_)))),
            "the queued event must still be in the channel: with coalescing off the \
             loop delivers it as its own batch"
        );
    }

    /// Ctrl+C ends the window when it is drained, however long the window had left:
    /// a 5s window that waited itself out would end `Quiet`, not `Interrupted`.
    #[test]
    fn debounce_interrupt_returns_immediately() {
        let (tx, rx) = mpsc::channel::<Msg>();
        tx.send(modify_event("/w/a.mds")).expect("send failed");
        tx.send(Msg::Interrupt).expect("send failed");

        let outcome = drain_debounce(&rx, 5_000);
        drop(tx);

        assert_eq!(outcome.end, DebounceEnd::Interrupted);
        assert!(
            outcome.interrupted(),
            "interrupted() must agree with the end reason"
        );
        assert_eq!(
            outcome.paths.len(),
            1,
            "positive control: the window was open and draining when Ctrl+C arrived"
        );
    }

    /// Reads do not extend the window.
    ///
    /// The compile reads its own sources, so an extending `Access` event would let the
    /// watcher hold its own window open.
    #[test]
    fn debounce_window_reads_count_but_neither_extend_nor_collect() {
        let t0 = Instant::now();
        // One content event, then 300ms of reads, one every 5ms.
        let reads = std::iter::once((t0, content_event("/w/a.mds")))
            .chain((1..=60u32).map(|i| (t0 + ms(5) * i, read_event("/w/a.mds"))));
        let (closed_at, end, closed) = replay(ms(100), t0, reads);
        assert_eq!(
            (closed_at, end),
            (t0 + ms(100), WindowEnd::Quiet),
            "300ms of reads must not extend a 100ms window"
        );
        assert_eq!(
            closed.paths.len(),
            1,
            "only the one content event contributes a path; got {:?}",
            closed.paths
        );
        assert_eq!(
            closed.messages, 20,
            "the content event and the 19 reads drained before the deadline all count"
        );

        // Positive control: content events at the same instants do extend.
        let writes = (0..=60u32).map(|i| (t0 + ms(5) * i, content_event(&format!("/w/f{i}.mds"))));
        let (closed_at, end, closed) = replay(ms(100), t0, writes);
        assert_eq!((closed_at, end), (t0 + ms(400), WindowEnd::Quiet));
        assert_eq!(closed.paths.len(), 61);
    }

    /// One window drains a bounded number of messages, of every kind.
    ///
    /// The cap bounds the window's duration; this bounds its work and its memory. A
    /// sender faster than the drain would otherwise grow `paths` without limit inside
    /// a single window.
    #[test]
    fn debounce_window_closes_at_the_message_limit() {
        let t0 = Instant::now();
        let mut open = DebounceWindow::open(ms(100), t0);
        for _ in 2..MAX_DEBOUNCE_MESSAGES {
            open.on_event(content_event("/w/same.mds"), t0);
        }
        open.on_event(read_event("/w/same.mds"), t0);
        assert_eq!(
            open.classify(t0),
            None,
            "one message short of the limit, the window is still open"
        );
        open.on_watch_error();
        assert_eq!(
            open.classify(t0),
            Some(WindowEnd::MessageLimit),
            "contents, reads and watch errors all count toward the limit"
        );

        // 12 000 events already queued when the window opens.
        let queued = (0..12_000u32).map(|_| (t0, content_event("/w/same.mds")));
        let (closed_at, end, closed) = replay(ms(100), t0, queued);
        assert_eq!(
            (closed_at, end),
            (t0, WindowEnd::MessageLimit),
            "12 000 queued events must hit the message bound, not the quiet period"
        );
        assert_eq!(closed.messages, MAX_DEBOUNCE_MESSAGES);
        assert_eq!(
            closed.paths.len(),
            1,
            "all 12 000 events name the same path; got {:?}",
            closed.paths
        );
    }

    /// The driver stops at the message limit: it asks the window after every message,
    /// so a flood queued faster than the window can close ends the window at the limit,
    /// and the rest of the flood stays queued for the next one.
    ///
    /// No timing: the 60s window and its 600s cap cannot close during the test, and the
    /// sender is dropped before the drain, so a driver that did not stop at the limit
    /// would drain the whole flood and end `Disconnected` rather than wait the window
    /// out. A read and a watch error sit inside the first 10 000 messages: exactly 2 000
    /// stay queued only if the driver counts both of them.
    #[test]
    fn debounce_driver_stops_at_the_message_limit_with_the_rest_still_queued() {
        let (tx, rx) = mpsc::channel::<Msg>();
        for _ in 2..MAX_DEBOUNCE_MESSAGES {
            tx.send(modify_event("/w/same.mds")).expect("send failed");
        }
        tx.send(Msg::Fs(Ok(read_event("/w/read.mds"))))
            .expect("send failed");
        let watch_error = notify::Error::generic("a planted watch error");
        tx.send(Msg::Fs(Err(watch_error))).expect("send failed");
        for _ in 0..2_000 {
            tx.send(modify_event("/w/later.mds")).expect("send failed");
        }
        drop(tx);

        let outcome = drain_debounce(&rx, 60_000);

        assert_eq!(
            outcome.end,
            DebounceEnd::Closed(WindowEnd::MessageLimit),
            "{} queued messages must end the window at the message limit",
            MAX_DEBOUNCE_MESSAGES + 2_000
        );
        assert_eq!(
            outcome.paths,
            BTreeSet::from([PathBuf::from("/w/same.mds")]),
            "only the content events drained before the limit contribute a path"
        );
        let left: Vec<Msg> = rx.try_iter().collect();
        assert_eq!(
            left.len(),
            2_000,
            "the content events, the read and the watch error all count toward the \
             limit, and every message after it stays queued"
        );
        assert!(
            left.iter().all(|msg| matches!(
                msg,
                Msg::Fs(Ok(event)) if event.paths == [PathBuf::from("/w/later.mds")]
            )),
            "positive control: the messages still queued are the flood after the limit"
        );
    }

    /// A dropped sender ends the window at once rather than waiting it out.
    ///
    /// The sender lives as long as the watcher, so a disconnect means the watcher is
    /// gone. Sitting out the remaining window there would delay shutdown by up to the
    /// cap for no possible gain: no further event can ever arrive.
    #[test]
    fn debounce_disconnected_ends_the_window_immediately() {
        let (tx, rx) = mpsc::channel::<Msg>();
        // One event already queued, so the drain has something to collect before it
        // reaches the disconnect — the exit must not discard it.
        tx.send(modify_event("/w/a.mds")).expect("send failed");
        drop(tx);

        let outcome = drain_debounce(&rx, 5_000);

        assert_eq!(
            outcome.end,
            DebounceEnd::Disconnected,
            "a 5s window that waited itself out would end Quiet, not Disconnected"
        );
        assert_eq!(
            outcome.paths.len(),
            1,
            "messages queued before the disconnect must still be collected; got {:?}",
            outcome.paths
        );
    }

    /// The one real-clock test: both drivers read `Instant::now()` and wait on a real
    /// channel. Lower bounds only — a slow runner can make a wait longer, never shorter
    /// — and the synthetic tests above pin every exact instant. Each driver call runs
    /// under [`within_5s`], and the sender stays alive throughout, so a driver that lost
    /// its bound fails here instead of hanging.
    #[test]
    fn debounce_and_tick_drivers_run_on_the_real_clock() {
        let (tx, rx) = mpsc::channel::<Msg>();
        tx.send(modify_event("/w/a.mds")).expect("send failed");

        let t0 = Instant::now();
        let (outcome, rx) = within_5s("drain_debounce", move || (drain_debounce(&rx, 20), rx));
        assert!(
            t0.elapsed() >= ms(20),
            "a 20ms window must wait at least 20ms; got {:?}",
            t0.elapsed()
        );
        // Quiet unless the runner stalled for the better part of the 1s cap between
        // opening the window and draining the event.
        assert!(
            matches!(
                outcome.end,
                DebounceEnd::Closed(WindowEnd::Quiet | WindowEnd::Cap)
            ),
            "the window must close on its own; got {:?}",
            outcome.end
        );
        assert_eq!(outcome.paths.len(), 1, "the queued event is drained");

        let t1 = Instant::now();
        let (wake, rx) = within_5s("an idle tick", move || {
            (TickClock::new(Some(ms(50))).recv_next(&rx, None), rx)
        });
        assert!(
            matches!(wake, Ok(Wake::Tick)),
            "an idle channel must produce a tick"
        );
        assert!(
            t1.elapsed() >= ms(50),
            "the tick must not come due before its interval; got {:?}",
            t1.elapsed()
        );

        // `--poll-interval 0`: no tick, yet a held rebuild's deadline still wakes the loop
        // (#380) — and not before it. A driver that blocked for a message instead fails
        // here by name.
        let t2 = Instant::now();
        let (wake, _rx) = within_5s("a held rebuild's deadline", move || {
            (TickClock::new(None).recv_next(&rx, Some(t2 + ms(30))), rx)
        });
        assert!(
            matches!(wake, Ok(Wake::HoldDue)),
            "a held rebuild's deadline must wake an idle loop with no tick"
        );
        assert!(
            t2.elapsed() >= ms(30),
            "the held rebuild must not run before its deadline; got {:?}",
            t2.elapsed()
        );
        drop(tx);
    }

    /// The clamp contract, verifiable without the watch loop.
    #[test]
    fn clamp_debounce_contract() {
        assert_eq!(
            clamp_debounce(0),
            None,
            "0 disables coalescing; it does not mean a 0ms window"
        );
        assert_eq!(clamp_debounce(100), Some(Duration::from_millis(100)));
        assert_eq!(
            clamp_debounce(u64::MAX),
            Some(Duration::from_secs(60)),
            "an unclamped u64::MAX window does not overflow on a monotonic clock — it \
             lands ~585 million years out, so the watcher silently never rebuilds"
        );
    }

    /// The cap contract: `max(10 x window, 1s)`.
    #[test]
    fn debounce_cap_contract() {
        assert_eq!(
            debounce_cap(Duration::from_millis(10)),
            Duration::from_secs(1),
            "the floor binds for small windows"
        );
        assert_eq!(
            debounce_cap(Duration::from_millis(250)),
            Duration::from_millis(2_500)
        );
        assert_eq!(
            debounce_cap(Duration::from_millis(1_000)),
            Duration::from_secs(10)
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
            outputs: HashMap::new(),
            kept: HashMap::new(),
            out_dir: None,
            reads: Vec::new(),
            external_dep_dirs: BTreeSet::new(),
            vars_file: None,
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            held: HeldBatch::default(),
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
            outputs: HashMap::new(),
            kept: HashMap::new(),
            out_dir: None,
            reads: Vec::new(),
            external_dep_dirs: BTreeSet::new(),
            vars_file: None,
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            held: HeldBatch::default(),
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

    /// #217: pruning a ghost EXTERNAL dep must not forget an in-root source's output.
    ///
    /// A dependency outside the watched root never had an output of its own, so there is
    /// none to forget. A probe for one from its path took the out-of-root flatten arm and
    /// yielded `<out-dir>/<file name>`, which is exactly the path an IN-ROOT source with
    /// the same file name owns. The prune then dropped that source's `last_written` entry
    /// and its next rebuild rewrote identical bytes.
    ///
    /// Reachable: an importer whose cross-root `@import` target is deleted leaves the
    /// vanished dep in `errored`, and every later real-change batch re-seeds `errored`.
    #[test]
    fn ghost_external_dep_prune_keeps_in_root_last_written() {
        let root_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let shared_dir = tempfile::tempdir().unwrap();
        let root = root_dir.path().to_path_buf();
        let out = out_dir.path().to_path_buf();

        // The in-root source that carries the batch's real change.
        let other = root.join("other.mds");
        std::fs::write(&other, "Hello.\n").unwrap();

        // The in-root source whose bookkeeping is at risk. It is not in this batch, so
        // nothing recompiles it — only its `last_written` entry can change.
        let victim = root.join("x.mds");
        std::fs::write(&victim, "Victim.\n").unwrap();
        let victim_out = out.join("x.md");

        // A cross-root dependency with the same file name that no longer exists.
        let ghost = shared_dir.path().join("x.mds");
        assert!(!ghost.exists(), "the ghost dep must not exist on disk");

        let mut state = DirWatchState {
            forward_deps: HashMap::new(),
            errored: HashSet::new(),
            known_files: BTreeSet::new(),
            last_written: HashMap::new(),
            outputs: HashMap::new(),
            kept: HashMap::new(),
            out_dir: None,
            reads: Vec::new(),
            external_dep_dirs: BTreeSet::new(),
            vars_file: None,
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            held: HeldBatch::default(),
        };
        state.known_files.insert(victim.clone());
        state.last_written.insert(
            victim_out.clone(),
            WrittenOutput {
                source: victim.clone(),
                content: "Victim.\n".to_string(),
            },
        );
        state.errored.insert(ghost.clone());
        state
            .external_dep_dirs
            .insert(shared_dir.path().to_path_buf());

        let changed: BTreeSet<PathBuf> = std::iter::once(other).collect();
        process_dir_batch_incremental(
            &changed,
            &WatchedPath {
                typed: root.clone(),
                canonical: root,
                what: Watched::Root,
            },
            &dir_base(out.clone()),
            &None,
            true,
            &mut state,
        );

        assert!(
            !state.errored.contains(&ghost),
            "control: the ghost prune must actually have run — without it the assertion \
             below would pass on a batch that never reached the branch"
        );
        assert!(
            state.last_written.contains_key(&victim_out),
            "#217: forgetting a ghost external dep must not drop an in-root source's \
             last_written entry; keys: {:?}",
            state.last_written.keys().collect::<Vec<_>>()
        );
    }

    /// #380: a batch whose `--vars` file changed recompiles every known source, and also
    /// compiles the sources it names that no walk has found — one created in the same
    /// batch, or held with it — which are then known, and in the baseline. A path the
    /// batch names that is no source below the root is not compiled as one.
    #[test]
    fn a_vars_changed_batch_compiles_the_sources_created_in_it() {
        let root_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        // Canonical, as every path a batch carries is (macOS tempdirs are below a symlink).
        let canonical = |dir: &tempfile::TempDir| std::fs::canonicalize(dir.path()).unwrap();
        let (root, out) = (canonical(&root_dir), canonical(&out_dir));
        let known = root.join("known.mds");
        let created = root.join("created.mds");
        // An in-root MDS module a source could import: it compiles, but is no source.
        let not_a_source = root.join("notes.md");
        let excluded = root.join("node_modules").join("dep.mds");
        let outside = canonical(&elsewhere).join("outside.mds");
        std::fs::create_dir(root.join("node_modules")).unwrap();
        for (path, text) in [
            (&known, "Known.\n"),
            (&created, "Created.\n"),
            (&not_a_source, "---\ntype: mds\nname: notes\n---\nNotes.\n"),
            (&excluded, "Excluded.\n"),
            (&outside, "Outside.\n"),
        ] {
            std::fs::write(path, text).unwrap();
        }
        let mut state = empty_dir_state();
        state.known_files.insert(known.clone());

        let changed: BTreeSet<PathBuf> = [&created, &not_a_source, &excluded, &outside]
            .into_iter()
            .cloned()
            .collect();
        process_dir_batch(
            &changed,
            true,
            &WatchedPath {
                typed: root.clone(),
                canonical: root.clone(),
                what: Watched::Root,
            },
            &dir_base(out.clone()),
            &None,
            true,
            &mut state,
        );

        let read = |rel: &Path| std::fs::read_to_string(out.join(rel)).ok();
        assert!(
            read(Path::new("known.md")).is_some_and(|text| text.contains("Known.")),
            "positive control: a known source is recompiled; out/known.md: {:?}",
            read(Path::new("known.md"))
        );
        assert!(
            read(Path::new("created.md")).is_some_and(|text| text.contains("Created.")),
            "a source created in the batch is compiled with it; out/created.md: {:?}",
            read(Path::new("created.md"))
        );
        assert!(
            state.known_files.contains(&created) && state.last_mtimes.contains_key(&created),
            "and is known and in the baseline, so a later edit or the idle tick finds it; \
             known: {:?}",
            state.known_files
        );
        // Where each would be written had it been compiled as a source.
        for rel in [
            PathBuf::from("notes.md"),
            Path::new("node_modules").join("dep.md"),
            PathBuf::from("outside.md"),
        ] {
            assert_eq!(
                read(&rel),
                None,
                "{}: a path that is no source below the root is not compiled as one",
                rel.display()
            );
        }
        for path in [&not_a_source, &excluded, &outside] {
            assert!(
                !state.known_files.contains(path) && !state.errored.contains(path),
                "{}: neither known nor errored; known: {:?}, errored: {:?}",
                path.display(),
                state.known_files,
                state.errored
            );
        }
    }

    /// #265: a source under the root is compiled by the root as typed plus its path
    /// below the canonical root; an out-of-root dependency keeps its canonical path.
    #[test]
    fn watch_root_walked_keeps_the_typed_prefix() {
        let root = WatchedPath {
            typed: PathBuf::from("src"),
            canonical: PathBuf::from("/abs/project/src"),
            what: Watched::Root,
        };
        assert_eq!(
            root.walked(Path::new("/abs/project/src/sub/a.mds")),
            Path::new("src").join("sub").join("a.mds")
        );
        assert_eq!(
            root.walked(Path::new("/abs/shared/b.mds")),
            Path::new("/abs/shared/b.mds"),
            "an out-of-root dependency has no walked form"
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
            outputs: HashMap::new(),
            kept: HashMap::new(),
            out_dir: None,
            reads: Vec::new(),
            external_dep_dirs: BTreeSet::new(),
            vars_file: None,
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            held: HeldBatch::default(),
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
        let base = dir_base(new_subdir.clone());
        let result = output_path_for(&source, RootPaths::as_typed(&root), &base, "md");
        assert_eq!(result.path, new_subdir.join("template.md"));
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
        // The canonical form `run_watch` derives, which the compile checks the typed path
        // against (on macOS the temporary directory is not canonical).
        let watched = WatchedPath {
            canonical: mds::NativeFs::check_symlink(&entry).unwrap(),
            typed: entry,
            what: Watched::Entry,
        };
        let (_written_path, deps, _content) =
            match compile_and_write(&watched, &Some(out_str), &None, &None, &[], None, true)
                .unwrap()
            {
                CompileWriteOutcome::Written(result) => result,
                CompileWriteOutcome::CompileFailed(e) => panic!("the compile failed: {e:?}"),
                CompileWriteOutcome::WriteFailed { failure, .. } => {
                    panic!("the write failed: {failure:?}")
                }
                CompileWriteOutcome::StdoutClosed => {
                    panic!("compile_and_write writes a file here, never stdout")
                }
            };
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

    /// #257: a write that fails after the compile succeeded keeps the dependencies the
    /// compile reported, and reports the failure naming the output of the compiled kind —
    /// `.json` for a messages template. Only a compile that fails leaves the kind unknown.
    #[test]
    fn compile_and_write_tells_a_failed_write_from_a_failed_compile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("part.mds"),
            "@define who():\nWorld\n@end\n\n@export who\n",
        )
        .unwrap();
        let entry = dir.path().join("chat.mds");
        std::fs::write(
            &entry,
            "@import \"./part.mds\" as p\n@message user:\nHello {{p.who()}}\n@end\n",
        )
        .unwrap();
        // A directory at the output path: no write replaces it.
        std::fs::create_dir(dir.path().join("chat.json")).unwrap();
        let watched = WatchedPath {
            canonical: mds::NativeFs::check_symlink(&entry).unwrap(),
            typed: entry.clone(),
            what: Watched::Entry,
        };
        let name = |path: &Path| path.file_name().and_then(|n| n.to_str()).map(str::to_owned);

        match compile_and_write(&watched, &None, &None, &None, &[], None, true).unwrap() {
            CompileWriteOutcome::WriteFailed { deps, failure } => {
                let dep_names: Vec<_> = deps.iter().filter_map(|d| name(d)).collect();
                assert!(
                    dep_names.iter().any(|n| n == "part.mds"),
                    "the dependencies the compile reported: {dep_names:?}"
                );
                let failure = failure.expect("a failed write is reported").to_string();
                assert!(
                    failure.contains("chat.json"),
                    "the failure names the output: {failure}"
                );
            }
            CompileWriteOutcome::CompileFailed(e) => panic!("the compile failed: {e:?}"),
            CompileWriteOutcome::Written(_) => panic!("a directory is never written over"),
            CompileWriteOutcome::StdoutClosed => panic!("the output is a file, never stdout"),
        }
        assert!(
            !dir.path().join("chat.md").exists(),
            "nothing is written on the Markdown route"
        );

        // Control: a compile that fails.
        std::fs::write(&entry, "Hello {{name\n").unwrap();
        assert!(
            matches!(
                compile_and_write(&watched, &None, &None, &None, &[], None, true).unwrap(),
                CompileWriteOutcome::CompileFailed(Some(_))
            ),
            "a failed compile is reported as one"
        );
    }

    /// #257, #160: without `-o` every output takes the route of its own kind — after a
    /// failed startup compile, and after a change of kind — and leaves the other kind's
    /// behind; the route an explicit `-o` names is every kind's, and leaves none.
    #[test]
    fn output_route_is_each_kinds_own_unless_one_is_named() {
        let target = |name: &str| Some(WriteTarget::as_typed(PathBuf::from(name)));
        let (md, json, named) = (target("chat.md"), target("chat.json"), target("out.md"));
        let by_kind = OutputRoute::ByKind {
            markdown: md.clone(),
            messages: json.clone(),
        };
        assert_eq!(by_kind.of(OutputKind::Messages), &json);
        assert_eq!(by_kind.of(OutputKind::Markdown), &md);
        assert_eq!(by_kind.other_than(OutputKind::Messages), md.as_ref());
        assert_eq!(by_kind.other_than(OutputKind::Markdown), json.as_ref());

        let route = OutputRoute::Named(named.clone());
        for kind in [OutputKind::Markdown, OutputKind::Messages] {
            assert_eq!(route.of(kind), &named, "-o is the route of every kind");
            assert_eq!(route.other_than(kind), None, "-o leaves nothing behind");
        }
    }

    /// #417: the watched entry is compiled by the typed path while that path leads to the
    /// canonical entry's directory, and refused — naming the path as typed, never the
    /// canonical one — once it leads into another directory. A typed path that no longer
    /// resolves is left to the compile's own error, `file not found`, naming it as typed.
    #[test]
    fn watched_entry_refuses_a_typed_path_that_leads_elsewhere() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in ["a", "b"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
            std::fs::write(
                dir.path().join(name).join("page.mds"),
                format!("Hi {name}\n"),
            )
            .unwrap();
        }
        let typed = dir.path().join("a").join("page.mds");
        let compile = |canonical: &Path| {
            WatchedPath {
                typed: typed.clone(),
                canonical: canonical.to_path_buf(),
                what: Watched::Entry,
            }
            .compile(None, true)
            .map(|compiled| compiled.content)
            .map_err(|failure| {
                failure
                    .unreported()
                    .map_or_else(|| "a panic".to_string(), |e| e.to_string())
            })
        };

        let here = mds::NativeFs::check_symlink(&typed).unwrap();
        assert_eq!(compile(&here), Ok("Hi a\n".to_string()), "control");

        let elsewhere =
            mds::NativeFs::check_symlink(&dir.path().join("b").join("page.mds")).unwrap();
        let refused = compile(&elsewhere).unwrap_err();
        assert_eq!(
            refused,
            format!(
                "watched entry now resolves to a different file: \"{}\"; \
                 restart mds watch to follow it",
                typed.display()
            )
        );
        assert!(
            !refused.contains(&*elsewhere.to_string_lossy()),
            "{refused}"
        );

        std::fs::remove_file(&typed).unwrap();
        let missing = compile(&here).unwrap_err();
        assert_eq!(
            missing,
            format!("file not found: {}", typed.display()),
            "a missing entry is the compile's own error, naming the path as typed"
        );
    }

    /// #413: the root and a source below it are checked by the entry's rule — the
    /// directory the typed path leads into against the one the canonical path names —
    /// and each refusal names its own kind and the path as typed. Controls: each kind
    /// whose typed path still leads to its canonical form, and each whose typed path no
    /// longer resolves, which is left to the compile.
    #[test]
    fn watched_root_and_source_share_the_entry_rule() {
        let dir = tempfile::TempDir::new().unwrap();
        for name in ["a", "b"] {
            std::fs::create_dir(dir.path().join(name)).unwrap();
            std::fs::write(dir.path().join(name).join("page.mds"), "Hi\n").unwrap();
        }
        let canonical = |p: PathBuf| p.canonicalize().unwrap();
        let check = |typed: PathBuf, canonical: PathBuf, what: Watched| {
            WatchedPath {
                typed,
                canonical,
                what,
            }
            .ensure_unmoved()
            .map_err(|e| e.to_string())
        };
        let moved = |kind: &str, now: &str, typed: &Path| {
            Err(format!(
                "watched {kind} now resolves to a different {now}: \"{}\"; \
                 restart mds watch to follow it",
                typed.display()
            ))
        };

        let root = dir.path().join("a");
        let source = root.join("page.mds");
        let root_b = canonical(dir.path().join("b"));
        let source_b = canonical(dir.path().join("b").join("page.mds"));

        let mut mismatches = Vec::new();
        let rows = [
            (
                "root, unmoved",
                check(root.clone(), canonical(root.clone()), Watched::Root),
                Ok(()),
            ),
            (
                "root, moved",
                check(root.clone(), root_b.clone(), Watched::Root),
                moved("directory", "directory", &root),
            ),
            (
                "source, unmoved",
                check(source.clone(), canonical(source.clone()), Watched::Source),
                Ok(()),
            ),
            (
                "source, moved",
                check(source.clone(), source_b.clone(), Watched::Source),
                moved("file", "file", &source),
            ),
            (
                "root, gone",
                check(dir.path().join("gone"), root_b, Watched::Root),
                Ok(()),
            ),
            (
                "source, gone",
                check(root.join("gone.mds"), source_b, Watched::Source),
                Ok(()),
            ),
        ];
        for (label, got, want) in rows {
            if got != want {
                mismatches.push(format!("{label}: got {got:?}, want {want:?}"));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }

    /// A session reads each `-o -` outcome itself (#157): a closed pipe stops it, a new
    /// failure for another reason is reported as `mds::io` naming stdout, and a repeat of
    /// it is not written and not reported — where a batch run takes both a closed pipe
    /// and a repeat for success.
    #[test]
    fn a_session_reads_each_stdout_outcome_for_itself() {
        assert!(matches!(
            OutputWrite::from_stdout(StdoutOutcome::Written),
            OutputWrite::Written
        ));
        assert!(matches!(
            OutputWrite::from_stdout(StdoutOutcome::Closed),
            OutputWrite::StdoutClosed
        ));
        assert!(matches!(
            OutputWrite::from_stdout(StdoutOutcome::FailedAgain),
            OutputWrite::Failed(None)
        ));
        let failed = std::io::Error::new(std::io::ErrorKind::StorageFull, "no space left");
        match OutputWrite::from_stdout(StdoutOutcome::Failed(failed)) {
            OutputWrite::Failed(Some(report)) => {
                assert_eq!(
                    report.to_string(),
                    format!(
                        "cannot write to stdout: {}",
                        std::io::ErrorKind::StorageFull
                    ),
                    "the error's kind, not the text it carries (#390)"
                );
                assert_eq!(
                    report.code().map(|c| c.to_string()).as_deref(),
                    Some("mds::io")
                );
            }
            other => panic!("want Failed(Some(mds::io)); got {other:?}"),
        }
    }

    /// Directory mode's state, empty.
    fn empty_dir_state() -> DirWatchState {
        DirWatchState {
            forward_deps: HashMap::new(),
            errored: HashSet::new(),
            known_files: BTreeSet::new(),
            last_written: HashMap::new(),
            outputs: HashMap::new(),
            kept: HashMap::new(),
            out_dir: None,
            reads: Vec::new(),
            external_dep_dirs: BTreeSet::new(),
            vars_file: None,
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            held: HeldBatch::default(),
        }
    }

    /// File mode's state watching `foi`, with no baseline taken.
    fn file_state(foi: HashSet<PathBuf>) -> FileWatchState {
        FileWatchState {
            watched_dirs: BTreeSet::new(),
            armed_dirs: BTreeSet::new(),
            foi,
            last_mtimes: HashMap::new(),
            hold: EmptyHold::Off,
            last_written: HashMap::new(),
            written_to: None,
            kept: None,
            output: OutputRoute::Named(None),
            out_dir: None,
            entry_was_missing: false,
            first_tick: false,
            missing_watched_dirs: BTreeSet::new(),
        }
    }

    /// A source and its dependency on disk, for the settle tests: `(dir, source, dependency)`.
    fn source_and_dependency() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("page.mds");
        let dep = dir.path().join("_dep.mds");
        std::fs::write(&src, "Page.\n").unwrap();
        std::fs::write(&dep, "Dep.\n").unwrap();
        (dir, src, dep)
    }

    /// `Settle::Rebaseline` takes the `(mtime, size)` baseline again in a rebuild, over what
    /// the mode watches, and records nothing else. There is no startup variant to apply it
    /// to: the startup baseline is still to come, so `SettleInto` holds only the two
    /// rebuild states (#257).
    #[test]
    fn settle_rebaseline_takes_the_baseline_again_in_a_rebuild_only() {
        let (dir, src, dep) = source_and_dependency();

        // File mode: over the files of interest.
        let foi: HashSet<PathBuf> = [src.clone(), dep.clone()].into_iter().collect();
        let mut file = file_state(foi.clone());
        settle(SettleInto::File(&mut file), None, Settle::Rebaseline);
        assert_eq!(file.last_mtimes, snapshot_state(&foi));
        assert!(
            file.last_mtimes
                .get(&src)
                .is_some_and(|stamp| stamp.0.is_some()),
            "the baseline holds the source as it is on disk: {:?}",
            file.last_mtimes
        );

        // Directory mode: over the watched set — the sources, their dependencies and the
        // `--vars` file (#380).
        let vars = dir.path().join("vars.json");
        std::fs::write(&vars, r#"{"v": 1}"#).unwrap();
        let mut rebuild = empty_dir_state();
        rebuild.known_files.insert(src.clone());
        rebuild.forward_deps.insert(src.clone(), vec![dep.clone()]);
        rebuild.vars_file = Some(vars.clone());
        settle(SettleInto::Dir(&mut rebuild), None, Settle::Rebaseline);
        assert_eq!(rebuild.last_mtimes, snapshot_state(&rebuild.watched_set()));
        assert!(
            rebuild.last_mtimes.contains_key(&dep) && rebuild.last_mtimes.contains_key(&vars),
            "the dependency and the vars file are in the baseline: {:?}",
            rebuild.last_mtimes
        );
        assert!(
            !rebuild.tracked_set().contains(&vars),
            "the vars file is outside the tracked set the idle tick diffs"
        );
        assert!(rebuild.errored.is_empty(), "nothing is marked errored");
    }

    /// `Settle::Defer` — a rebuild held while a watched file is empty — reports nothing,
    /// records nothing and leaves the baseline as it was in both modes, so the next event
    /// or tick finds the emptied file again; `Settle::Rebaseline` on the same state takes
    /// the empty file in (positive control) (#380).
    #[test]
    fn settle_defer_leaves_the_baseline_and_records_nothing() {
        let (_dir, src, dep) = source_and_dependency();
        let foi: HashSet<PathBuf> = [src.clone(), dep.clone()].into_iter().collect();
        let before = snapshot_state(&foi);
        std::fs::write(&dep, "").unwrap();
        let report = |e: miette::Report| panic!("a deferral reports nothing; got {e:?}");

        // File mode.
        let mut file = file_state(foi.clone());
        file.last_mtimes = before.clone();
        settle_reporting(SettleInto::File(&mut file), None, Settle::Defer, report);
        assert_eq!(file.last_mtimes, before, "the baseline is left as it was");
        assert!(
            any_went_empty(&file.foi, &file.last_mtimes),
            "the next rebuild finds the dependency emptied again"
        );
        settle(SettleInto::File(&mut file), None, Settle::Rebaseline);
        assert!(
            !any_went_empty(&file.foi, &file.last_mtimes),
            "positive control: a rebaseline takes the empty file in"
        );

        // Directory mode.
        let mut rebuild = empty_dir_state();
        rebuild.known_files.insert(src.clone());
        rebuild.forward_deps.insert(src.clone(), vec![dep.clone()]);
        rebuild.last_mtimes = before.clone();
        settle_reporting(SettleInto::Dir(&mut rebuild), None, Settle::Defer, report);
        assert_eq!(
            rebuild.last_mtimes, before,
            "the baseline is left as it was"
        );
        assert!(rebuild.errored.is_empty(), "nothing is marked errored");
        assert!(
            any_went_empty(&rebuild.watched_set(), &rebuild.last_mtimes),
            "the next batch finds the dependency emptied again"
        );
        settle(SettleInto::Dir(&mut rebuild), None, Settle::Rebaseline);
        assert!(
            !any_went_empty(&rebuild.watched_set(), &rebuild.last_mtimes),
            "positive control: a rebaseline takes the empty file in"
        );
    }

    /// A canonical temporary directory: every path a rebuild carries is canonical, and
    /// macOS tempdirs are below a symlink.
    fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap();
        (dir, path)
    }

    /// Directory mode's rebuild context: `root` watched, outputs below `out`, no `--vars`,
    /// quiet.
    fn dir_ctx(root: &Path, out: &Path) -> DirWatchCtx {
        DirWatchCtx {
            root: WatchedPath {
                typed: root.to_path_buf(),
                canonical: root.to_path_buf(),
                what: Watched::Root,
            },
            working_dir: WorkingDir { canonical: None },
            vars_path: None,
            vars_path_typed: None,
            static_set_vars: Vec::new(),
            static_set_string_vars: Vec::new(),
            output_base: dir_base(out.to_path_buf()),
            exclude_prefix: None,
            vars_dir_extra: None,
            clear: false,
            debounce_ms: 0,
            quiet: true,
        }
    }

    /// #380: a watched file found emptied while its own event was lost joins the held
    /// batch, so the batch that ends the hold rebuilds it — its write's event lost too —
    /// rather than taking its new content into the baseline unseen.
    #[test]
    fn a_file_found_emptied_without_its_event_is_rebuilt_when_the_hold_ends() {
        let (_root_dir, root) = canonical_tempdir();
        let (_out_dir, out) = canonical_tempdir();
        let (a, x) = (root.join("a.mds"), root.join("x.mds"));
        std::fs::write(&a, "A one\n").unwrap();
        std::fs::write(&x, "X one\n").unwrap();
        let ctx = dir_ctx(&root, &out);
        let mut state = empty_dir_state();
        state.known_files.extend([a.clone(), x.clone()]);
        state.last_mtimes = snapshot_state(&state.watched_set());

        // x emptied, its event lost: a batch for a finds it so.
        std::fs::write(&x, "").unwrap();
        rebuild_dir_batch(&ctx, &BTreeSet::from([a.clone()]), false, &mut state, None);
        assert!(
            state.hold.deadline().is_some(),
            "control: the batch is held while x is empty"
        );
        // x written, that event lost as well: the next batch finds nothing emptied.
        std::fs::write(&x, "X two\n").unwrap();
        rebuild_dir_batch(&ctx, &BTreeSet::new(), false, &mut state, None);

        let read = |name: &str| std::fs::read_to_string(out.join(name)).ok();
        assert!(
            read("a.md").is_some_and(|text| text.contains("A one")),
            "positive control: the held batch is rebuilt; out/a.md: {:?}",
            read("a.md")
        );
        assert!(
            read("x.md").is_some_and(|text| text.contains("X two")),
            "the file found emptied is rebuilt with it; out/x.md: {:?}",
            read("x.md")
        );
    }

    /// #380: a file rebuild whose compile read a file of interest emptied after the
    /// rebuild first looked — a truncation that began between the two — is held, not
    /// published, and leaves the baseline as it was. Nothing pauses a file rebuild
    /// between its look and its compile, so the included module is left out of the files
    /// the look stats while the baseline still holds a stamp that saw its bytes.
    /// Positive control: a baseline that saw it empty publishes the same compile.
    #[test]
    fn a_file_rebuild_whose_compile_read_a_file_emptied_since_its_look_is_held() {
        let (_dir, root) = canonical_tempdir();
        let entry = root.join("page.mds");
        let module = root.join("_inc.mds");
        let out = root.join("page.md");
        std::fs::write(
            &entry,
            "@import \"./_inc.mds\" as inc\nPage.\n@include inc\n",
        )
        .unwrap();
        std::fs::write(&module, "").unwrap();
        let ctx = FileCompileCtx {
            entry: WatchedPath {
                typed: entry.clone(),
                canonical: entry.clone(),
                what: Watched::Entry,
            },
            working_dir: WorkingDir { canonical: None },
            vars_path: None,
            vars_path_typed: None,
            reads: Vec::new(),
            static_set_vars: Vec::new(),
            static_set_string_vars: Vec::new(),
            quiet: true,
        };
        let mut watcher =
            RecommendedWatcher::new(|_: notify::Result<Event>| {}, notify::Config::default())
                .unwrap();

        for (seen, held) in [(Some(9), true), (Some(0), false)] {
            let _ = std::fs::remove_file(&out);
            let mut state = file_state(std::iter::once(entry.clone()).collect());
            state.output = OutputRoute::Named(Some(WriteTarget::as_typed(out.clone())));
            state.last_mtimes = snapshot_state(&state.foi);
            state.last_mtimes.insert(module.clone(), (None, seen));
            let before = state.last_mtimes.clone();

            let _ = rebuild_file(&ctx, &mut watcher, &mut state, None);

            let written = std::fs::read_to_string(&out).ok();
            if held {
                assert_eq!(
                    written, None,
                    "a compile that read a file emptied since the look is held, not published"
                );
                assert!(state.hold.deadline().is_some(), "a hold of its own is set");
                assert_eq!(state.last_mtimes, before, "the baseline is left as it was");
            } else {
                assert!(
                    written
                        .as_deref()
                        .is_some_and(|text| text.contains("Page.")),
                    "positive control: a file the baseline saw empty is no file emptied; \
                     page.md: {written:?}"
                );
                assert_eq!(state.hold.deadline(), None, "nothing is held");
            }
        }
    }

    /// #380: a directory source whose compile read itself emptied since the batch looked is
    /// held alone — nothing written, the source kept to be rebuilt — and the batch that
    /// ends the hold rebuilds it. Nothing pauses a release build between a batch's look
    /// and its compile (`tests/cli_watch_truncate.rs` uses the debug build's pause), so the
    /// source is one the look does not stat — not yet known — whose baseline stamp saw
    /// bytes.
    #[test]
    fn a_directory_source_whose_compile_read_it_emptied_since_the_look_is_held() {
        let (_root_dir, root) = canonical_tempdir();
        let (_out_dir, out) = canonical_tempdir();
        let page = root.join("page.mds");
        std::fs::write(&page, "").unwrap();
        let ctx = dir_ctx(&root, &out);
        let mut state = empty_dir_state();
        state.last_mtimes.insert(page.clone(), (None, Some(9)));

        rebuild_dir_batch(
            &ctx,
            &BTreeSet::from([page.clone()]),
            false,
            &mut state,
            None,
        );
        assert_eq!(
            std::fs::read_to_string(out.join("page.md")).ok(),
            None,
            "a compile that read its source emptied since the look is held, not published"
        );
        assert!(state.hold.deadline().is_some(), "a hold of its own is set");
        assert!(state.held.paths.contains(&page), "{:?}", state.held);

        std::fs::write(&page, "Page two\n").unwrap();
        rebuild_dir_batch(&ctx, &BTreeSet::new(), false, &mut state, None);
        assert!(
            std::fs::read_to_string(out.join("page.md"))
                .is_ok_and(|text| text.contains("Page two")),
            "the batch that ends the hold rebuilds the source held"
        );
        assert_eq!(state.hold.deadline(), None, "the hold has ended");
    }

    /// #380: a file found emptied after a rebuild looked holds it as the look would have —
    /// at a deadline set by the first such finding and never moved — unless the deadline
    /// ended the hold in this rebuild, which compiles the files as they are. The rebuild
    /// after that decides afresh.
    #[test]
    fn empty_hold_holds_a_late_finding_unless_the_deadline_ended_it() {
        let t0 = Instant::now();
        let mut hold = EmptyHold::default();
        assert_eq!(hold.on_rebuild(false, t0), HoldVerdict::Compile);
        assert_eq!(
            hold.on_late_empty(t0 + ms(1)),
            HoldVerdict::Hold,
            "found after a look that found nothing"
        );
        let deadline = t0 + ms(1) + EMPTY_HOLD_DEADLINE;
        assert_eq!(hold.deadline(), Some(deadline));
        assert_eq!(hold.on_late_empty(t0 + ms(100)), HoldVerdict::Hold);
        assert_eq!(
            hold.deadline(),
            Some(deadline),
            "a later finding must not move the deadline"
        );

        // The deadline's rebuild: compiled as the files are, and never held again.
        assert_eq!(hold.on_rebuild(true, deadline), HoldVerdict::Compile);
        assert_eq!(
            hold.on_late_empty(deadline + ms(1)),
            HoldVerdict::Compile,
            "a rebuild the deadline runs compiles the files as they are"
        );
        assert_eq!(hold.deadline(), None, "and leaves nothing held");

        // The next rebuild decides afresh: a later truncation holds on a deadline of its
        // own, and a rebuild that finds nothing ends it.
        assert_eq!(hold.on_rebuild(true, deadline + ms(10)), HoldVerdict::Hold);
        assert_eq!(
            hold.deadline(),
            Some(deadline + ms(10) + EMPTY_HOLD_DEADLINE)
        );
        assert_eq!(
            hold.on_rebuild(false, deadline + ms(20)),
            HoldVerdict::Compile
        );
        assert_eq!(hold.deadline(), None);
    }

    /// #380: the rebuild a hold's deadline runs decides at an instant no earlier than that
    /// deadline, so it ends the hold even when the clock reads earlier — a clock that steps
    /// back between the wake and the rebuild. An event's or a tick's rebuild takes the
    /// clock as it reads.
    #[test]
    fn a_deadline_rebuild_ends_its_hold_whatever_the_clock_reads() {
        let deadline = Instant::now() + EMPTY_HOLD_DEADLINE;
        let behind = deadline - ms(5);
        assert_eq!(not_before(behind, None), behind);
        assert_eq!(not_before(behind, Some(deadline)), deadline);
        assert_eq!(
            not_before(deadline + ms(5), Some(deadline)),
            deadline + ms(5)
        );

        let mut held = EmptyHold::Until(deadline);
        assert_eq!(
            held.on_rebuild(true, behind),
            HoldVerdict::Hold,
            "control: a rebuild at the clock's earlier reading is held again"
        );
        assert_eq!(
            held.on_rebuild(true, not_before(behind, Some(deadline))),
            HoldVerdict::Compile,
            "the deadline's rebuild ends the hold"
        );
        assert_eq!(held.deadline(), None);
    }

    /// #380: the files a look finds emptied join the batch — the `--vars` file as a change
    /// to it, every other as a path — whether or not their events came.
    #[test]
    fn files_found_emptied_join_the_batch() {
        let path = |name: &str| PathBuf::from("/w").join(name);
        let batch: BTreeSet<PathBuf> = BTreeSet::from([path("a.mds")]);
        let vars = path("vars.json");
        let emptied: BTreeSet<PathBuf> = BTreeSet::from([path("x.mds"), vars.clone()]);

        assert_eq!(
            join_emptied(&batch, false, &emptied, Some(&vars)),
            (BTreeSet::from([path("a.mds"), path("x.mds")]), true),
            "the vars file is a change to it, not a path of the batch"
        );
        assert_eq!(
            join_emptied(&batch, false, &BTreeSet::from([path("x.mds")]), Some(&vars)),
            (BTreeSet::from([path("a.mds"), path("x.mds")]), false)
        );
        assert_eq!(
            join_emptied(&batch, true, &BTreeSet::new(), Some(&vars)),
            (batch.clone(), true),
            "nothing emptied: the batch as it came"
        );
    }

    /// #380: while a rebuild is held, a batch's baseline keeps the stamp that saw an
    /// emptied file's bytes; every other file — one with bytes, one the old baseline never
    /// saw, one it saw empty — takes its fresh stamp.
    #[test]
    fn a_held_baseline_keeps_the_stamp_that_saw_an_emptied_file_s_bytes() {
        let path = |name: &str| PathBuf::from("/w").join(name);
        let before: StampMap = [
            (path("emptied"), (None, Some(5))),
            (path("edited"), (None, Some(3))),
            (path("was_empty"), (None, Some(0))),
        ]
        .into_iter()
        .collect();
        let fresh: StampMap = [
            (path("emptied"), (None, Some(0))),
            (path("edited"), (None, Some(4))),
            (path("was_empty"), (None, Some(0))),
            (path("new"), (None, Some(0))),
        ]
        .into_iter()
        .collect();
        let kept = baseline_keeping_emptied(&before, fresh.clone());
        assert_eq!(kept.get(&path("emptied")), Some(&(None, Some(5))));
        for name in ["edited", "was_empty", "new"] {
            assert_eq!(kept.get(&path(name)), fresh.get(&path(name)), "{name}");
        }
        assert_eq!(kept.len(), fresh.len());
    }

    /// #380: the paths found emptied since a baseline: those it saw with bytes that have
    /// none now — not one still with bytes, nor one it never saw.
    #[test]
    fn emptied_paths_are_those_found_with_none_after_bytes() {
        let (_dir, root) = canonical_tempdir();
        let [emptied, kept, unseen] = ["emptied", "kept", "unseen"].map(|name| root.join(name));
        std::fs::write(&emptied, "bytes").unwrap();
        std::fs::write(&kept, "bytes").unwrap();
        let paths: HashSet<PathBuf> = [emptied.clone(), kept.clone()].into_iter().collect();
        let baseline = snapshot_state(&paths);
        std::fs::write(&emptied, "").unwrap();
        std::fs::write(&unseen, "").unwrap();

        let all = [emptied.clone(), kept, unseen];
        assert_eq!(emptied_paths(&all, &baseline), BTreeSet::from([emptied]));
    }

    /// `Settle::MarkErrored` records a directory-mode source as errored in a rebuild,
    /// keeping the dependency set its last successful compile recorded, and takes no
    /// baseline: the batch takes it at its end. File mode has no errored set: it takes
    /// the baseline again, as `Settle::Rebaseline` does. A directory-mode startup failure
    /// settles the source the same way, through [`settle_startup_error`] (#257).
    #[test]
    fn settle_mark_errored_records_the_source_in_directory_mode() {
        let (_dir, src, dep) = source_and_dependency();

        // Directory rebuild: errored, the dependency set kept, no baseline.
        let mut rebuild = empty_dir_state();
        rebuild.known_files.insert(src.clone());
        rebuild.forward_deps.insert(src.clone(), vec![dep.clone()]);
        settle(
            SettleInto::Dir(&mut rebuild),
            None,
            Settle::MarkErrored(&src),
        );
        assert!(rebuild.errored.contains(&src), "{:?}", rebuild.errored);
        assert_eq!(rebuild.forward_deps.get(&src), Some(&vec![dep.clone()]));
        assert!(rebuild.last_mtimes.is_empty(), "{:?}", rebuild.last_mtimes);

        // File mode: the baseline again.
        let foi: HashSet<PathBuf> = [src.clone(), dep.clone()].into_iter().collect();
        let mut file = file_state(foi.clone());
        settle(SettleInto::File(&mut file), None, Settle::MarkErrored(&src));
        assert_eq!(file.last_mtimes, snapshot_state(&foi));
        assert!(
            file.last_mtimes.contains_key(&src),
            "{:?}",
            file.last_mtimes
        );
    }

    /// A directory-mode startup failure settles the source as errored, same as
    /// `Settle::MarkErrored` in a rebuild: a source new to the graph gets the empty
    /// dependency set, and no baseline is taken — the startup baseline is still to come.
    /// File mode's startup settle has no state to record into (#257).
    #[test]
    fn settle_startup_error_records_the_source_in_directory_mode() {
        let (_dir, src, _dep) = source_and_dependency();

        let mut startup = empty_dir_state();
        settle_startup_error(StartupInto::Dir(&mut startup), None, &src);
        assert!(startup.errored.contains(&src), "{:?}", startup.errored);
        assert_eq!(startup.forward_deps.get(&src), Some(&vec![]));
        assert!(startup.last_mtimes.is_empty(), "{:?}", startup.last_mtimes);
    }

    /// A compile that panicked settles as its error does at the same site — the site picks
    /// the action, whatever failed — and is not reported: the panic hook reported it
    /// (#389, #257).
    #[test]
    fn a_panicked_compile_settles_as_an_error_does_and_is_not_reported() {
        let (_dir, src, dep) = source_and_dependency();
        let foi: HashSet<PathBuf> = [src.clone(), dep.clone()].into_iter().collect();
        let failures = || {
            [
                (
                    "an error",
                    CompileFailure::from(miette::miette!("broken")),
                    1,
                ),
                ("a panic", CompileFailure::Panicked, 0),
            ]
        };

        for (what, failure, reports) in failures() {
            let mut reported = Vec::new();
            let mut state = empty_dir_state();
            state.forward_deps.insert(src.clone(), vec![dep.clone()]);
            settle_reporting(
                SettleInto::Dir(&mut state),
                failure.unreported(),
                Settle::MarkErrored(&src),
                |e| reported.push(e.to_string()),
            );
            assert!(state.errored.contains(&src), "{what}: marked errored");
            assert_eq!(
                state.forward_deps.get(&src),
                Some(&vec![dep.clone()]),
                "{what}"
            );
            assert_eq!(reported.len(), reports, "{what}: reported {reported:?}");
        }

        for (what, failure, reports) in failures() {
            let mut reported = Vec::new();
            let mut state = file_state(foi.clone());
            settle_reporting(
                SettleInto::File(&mut state),
                failure.unreported(),
                Settle::MarkErrored(&src),
                |e| reported.push(e.to_string()),
            );
            assert_eq!(
                state.last_mtimes,
                snapshot_state(&foi),
                "{what}: rebaselined"
            );
            assert_eq!(reported.len(), reports, "{what}: reported {reported:?}");
        }

        // The error is reported as it is, once.
        let mut reported = Vec::new();
        settle_startup_error_reporting(
            StartupInto::File,
            CompileFailure::from(miette::miette!("broken")).unreported(),
            &src,
            |e| reported.push(e.to_string()),
        );
        assert_eq!(reported, ["broken"]);
    }

    /// An out-dir is unchanged until it goes. Deleted, it is new — no output the session
    /// wrote is there to skip — and so is the directory a write makes in its place, which
    /// the next check compares once [`OutDirAnchor::written`] has taken it. A directory
    /// put in its place is new once, then the one compared (unix: Windows tells the two
    /// apart by their creation time alone, which tunnelling can carry over).
    #[test]
    fn an_out_dir_deleted_or_replaced_at_its_path_is_new_once() {
        let base = tempfile::tempdir().unwrap();
        let out = base.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let mut anchor = OutDirAnchor::record(Some(&out), None).expect("an out-dir");
        assert_eq!(
            anchor.check(),
            OutDirNow::Unchanged,
            "the directory recorded"
        );

        std::fs::remove_dir(&out).unwrap();
        assert_eq!(anchor.check(), OutDirNow::New, "deleted");
        assert_eq!(anchor.check(), OutDirNow::New, "still none");
        // The write creates the directory again.
        std::fs::create_dir(&out).unwrap();
        anchor.written();
        assert_eq!(
            anchor.check(),
            OutDirNow::Unchanged,
            "the directory the write made"
        );

        if cfg!(unix) {
            std::fs::rename(&out, base.path().join("out.old")).unwrap();
            std::fs::create_dir(&out).unwrap();
            assert_eq!(
                anchor.check(),
                OutDirNow::New,
                "another directory in its place"
            );
            assert_eq!(
                anchor.check(),
                OutDirNow::Unchanged,
                "then the one compared"
            );
        }
    }

    /// An out-dir named through a symlink is elsewhere once the link leads to another
    /// directory — one that is there, or one a write would create there — or nowhere, and
    /// is itself again once the link leads back: a check never takes the other directory
    /// as the one it compares.
    #[cfg(unix)]
    #[test]
    fn an_out_dir_the_typed_path_leads_away_from_is_elsewhere() {
        use std::os::unix::fs::symlink;

        let base = tempfile::tempdir().unwrap();
        for name in ["a", "b"] {
            std::fs::create_dir(base.path().join(name)).unwrap();
        }
        let link = base.path().join("link");
        symlink("a", &link).unwrap();
        let mut anchor = OutDirAnchor::record(Some(&link), None).expect("an out-dir");
        // `link/out` is not there yet: the write would create it in `a`.
        let mut below = OutDirAnchor::record(Some(&link.join("out")), None).expect("an out-dir");
        assert_eq!(anchor.check(), OutDirNow::Unchanged);
        assert_eq!(below.check(), OutDirNow::New, "none yet");

        let retarget = |to: &str| {
            std::fs::remove_file(&link).unwrap();
            symlink(to, &link).unwrap();
        };
        retarget("b");
        assert_eq!(
            anchor.check(),
            OutDirNow::Elsewhere,
            "a directory that is there"
        );
        assert_eq!(anchor.checked, None, "a refused write is anchored nowhere");
        assert_eq!(
            below.check(),
            OutDirNow::Elsewhere,
            "one a write would create"
        );
        retarget("nowhere");
        assert_eq!(
            anchor.check(),
            OutDirNow::Elsewhere,
            "a link that leads nowhere"
        );

        retarget("a");
        assert_eq!(anchor.check(), OutDirNow::Unchanged, "led back");
        assert_eq!(below.check(), OutDirNow::New, "led back, still none");
    }

    /// `build.output_dir` is checked below the directory `mds.json` was reached by, the
    /// path compared; `--out-dir` takes precedence over it, as it does for the output
    /// base, and with neither, or a configuration without `build.output_dir`, there is no
    /// out-dir to check.
    #[test]
    fn a_build_output_dir_is_checked_below_the_directory_of_mds_json() {
        use crate::build::{BuildConfig, MdsConfig};

        let base = tempfile::tempdir().unwrap();
        let dir = base.path().canonicalize().unwrap();
        let config = |output_dir: Option<&str>| ProjectConfig {
            config: MdsConfig {
                build: BuildConfig {
                    output_dir: output_dir.map(str::to_owned),
                    ..Default::default()
                },
                ..Default::default()
            },
            dir: dir.clone(),
            shown_dir: base.path().to_path_buf(),
        };
        let dist = config(Some("dist"));
        std::fs::create_dir(dir.join("dist")).unwrap();
        let mut anchor = OutDirAnchor::record(None, Some(&dist)).expect("an out-dir");
        assert_eq!(
            (&anchor.typed, &anchor.out_dir),
            (&base.path().to_path_buf(), &dir.join("dist"))
        );
        assert_eq!(anchor.check(), OutDirNow::Unchanged);
        std::fs::remove_dir(dir.join("dist")).unwrap();
        assert_eq!(anchor.check(), OutDirNow::New, "deleted below it");

        let flag = base.path().join("flag");
        let chosen = OutDirAnchor::record(Some(&flag), Some(&dist)).expect("an out-dir");
        assert_eq!(chosen.out_dir, dir.join("flag"), "--out-dir first");
        assert!(OutDirAnchor::record(None, Some(&config(None))).is_none());
        assert!(OutDirAnchor::record(None, None).is_none());

        // The writes are anchored at the directory `mds.json` is in, `dist` deleted or not.
        let config_dir = Some(CheckedAnchor {
            missing: 0,
            identity: DirIdentity::of(&dir).expect("a directory"),
        });
        assert_eq!(anchor.checked, config_dir);
        std::fs::create_dir(dir.join("dist")).unwrap();
        anchor.check();
        assert_eq!(anchor.checked, config_dir);
    }

    /// A check finds the directory the next write below the out-dir is anchored at (#160):
    /// the out-dir itself, or — once it is deleted, for the write to create it again — the
    /// nearest directory above it. The write is given that directory to expect, below it
    /// the file, named as before; a session with no out-dir writes its target as it is.
    #[test]
    fn a_check_finds_the_directory_the_next_write_is_anchored_at() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().canonicalize().unwrap();
        let out = root.join("out");
        std::fs::create_dir(&out).unwrap();
        let mut anchor = OutDirAnchor::record(Some(&out), None).expect("an out-dir");
        assert_eq!(anchor.checked, None, "nothing found before a check");
        let target = WriteTarget::new(out.join("a.md"), PathBuf::from("o/a.md"));
        let written = |anchor: &OutDirAnchor| {
            let checked = below_checked_out_dir(Some(anchor), &target);
            (
                checked.path.clone(),
                checked.shown.clone(),
                checked.below_anchor(),
                checked.checked_anchor(),
            )
        };

        assert_eq!(anchor.check(), OutDirNow::Unchanged);
        assert_eq!(
            written(&anchor),
            (
                out.join("a.md"),
                PathBuf::from("o/a.md"),
                1,
                DirIdentity::of(&out)
            ),
            "the out-dir"
        );

        std::fs::remove_dir(&out).unwrap();
        assert_eq!(anchor.check(), OutDirNow::New);
        assert_eq!(
            written(&anchor),
            (
                out.join("a.md"),
                PathBuf::from("o/a.md"),
                2,
                DirIdentity::of(&root)
            ),
            "deleted: the directory above it, the out-dir below it"
        );

        assert_eq!(below_checked_out_dir(None, &target), target, "no out-dir");
    }
}
