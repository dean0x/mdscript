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
//! # Key invariants
//!
//! - All content output → stdout ONLY when output resolves to stdout.
//! - All status / warnings / errors → stderr (pipe-safe).
//! - `--quiet` suppresses status + warnings but NOT compile errors.
//! - Exit 0 on clean Ctrl+C; non-zero only on startup failure.
//! - Compile errors during watching never terminate the watcher.
//! - All loops have fixed upper bounds (ADR-021 / reliability.md).
//! - All `.mds` reads go through `compile_to_content` / `read_build_input` (PF-004).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use miette::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::build::{
    auto_detect_mds_file, build_runtime_vars, compile_and_write, compile_to_content, load_config,
    resolve_output_path, write_output, MdsConfig, OutputFormat,
};

// ── Public args struct ────────────────────────────────────────────────────────

pub(crate) struct WatchArgs {
    pub(crate) input: Option<PathBuf>,
    pub(crate) output: Option<String>,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) vars: Option<PathBuf>,
    pub(crate) set_vars: Vec<(String, String)>,
    pub(crate) format: OutputFormat,
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

// ── Output base for directory mode ────────────────────────────────────────────

/// Describes where directory-mode output files are written.
///
/// `Dir(base)` mirrors the source subtree under `base`:
///   `source.strip_prefix(root)` → `base/rel/stem.md`
/// `NextToSource` places the `.md` next to the source file.
#[derive(Debug, Clone)]
pub(crate) enum OutputBase {
    Dir(PathBuf),
    NextToSource,
}

/// Compute the `OutputBase` for directory mode.
///
/// Precedence (mirrors `resolve_output_path` for file mode):
/// 1. `--out-dir` → `Dir(abs_out_dir)`
/// 2. `mds.json build.output_dir` → `Dir(config_dir.join(output_dir))`
///    — rejects `..` components at startup with a hard error.
/// 3. Default → `NextToSource`
pub(crate) fn resolve_output_base(
    abs_out_dir: Option<&Path>,
    config: &Option<(MdsConfig, PathBuf)>,
) -> Result<OutputBase> {
    if let Some(d) = abs_out_dir {
        return Ok(OutputBase::Dir(d.to_path_buf()));
    }
    if let Some((cfg, config_dir)) = config {
        if let Some(ref output_dir) = cfg.build.output_dir {
            let traversal = Path::new(output_dir)
                .components()
                .any(|c| c == std::path::Component::ParentDir);
            if traversal {
                return Err(miette::miette!(
                    "mds.json output_dir '{}' must not contain '..' components",
                    output_dir
                ));
            }
            return Ok(OutputBase::Dir(config_dir.join(output_dir)));
        }
    }
    Ok(OutputBase::NextToSource)
}

/// Compute the mirrored output path for a source file in directory mode.
///
/// Infallible — no directory creation.
///
/// - `Dir(base)`: mirrors `source` relative to `root` under `base`.
///   If `strip_prefix` fails (source not under root after canonicalization),
///   falls back to `base/stem.md` — **never** joins an absolute path that
///   could escape the output directory (AC-M7 path-escape guard).
/// - `NextToSource`: `source.with_extension("md")`.
pub(crate) fn output_path_for(source: &Path, root: &Path, base: &OutputBase) -> PathBuf {
    match base {
        OutputBase::Dir(d) => {
            // strip_prefix gives the relative path from root to source.
            // If source is outside root (canonicalization edge case), fall
            // back to just the filename to stay contained in the out-dir.
            let rel = match source.strip_prefix(root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => {
                    // Path-escape guard (AC-M7): use filename only.
                    let stem = source.file_stem().unwrap_or(source.as_os_str());
                    let mut name = std::ffi::OsString::from(stem);
                    name.push(".md");
                    return d.join(name);
                }
            };
            // Replace the extension on the relative path.
            let stem = rel.file_stem().unwrap_or(rel.as_os_str()).to_os_string();
            let mut name = stem;
            name.push(".md");
            let out = d.join(rel.parent().unwrap_or(Path::new(""))).join(name);
            // AC-M7 containment invariant: the output path must remain inside the out-dir.
            // Enforced at runtime (not only in debug builds) so the path-escape boundary
            // is guarded in production. If `strip_prefix` produced a relative path that
            // somehow contains `..` or an absolute component, fall back to the flat
            // `d/<stem>.md` form which is guaranteed to be inside `d` (reliability.md / #5).
            if out.starts_with(d) {
                out
            } else {
                debug_assert!(
                    false,
                    "output_path_for: AC-M7 violated — output {out:?} escaped out-dir {d:?}"
                );
                let stem = source
                    .file_stem()
                    .unwrap_or(source.as_os_str())
                    .to_os_string();
                let mut flat_name = stem;
                flat_name.push(".md");
                d.join(flat_name)
            }
        }
        OutputBase::NextToSource => source.with_extension("md"),
    }
}

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
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                set.insert(parent.to_path_buf());
            } else {
                // Relative path with no directory component: watch "."
                set.insert(PathBuf::from("."));
            }
        }
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

/// Recursively collect all `.mds` files under `root`, bounded by `max_depth`.
///
/// Symlinked directories are skipped to avoid cycles.
/// When `exclude_prefix` is `Some(p)`, any path that starts with `p` is skipped
/// (used to exclude the out-dir when it is inside the watched root).
pub(crate) fn collect_mds_files(
    root: &Path,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
) -> Vec<PathBuf> {
    let mut results = Vec::new();
    collect_mds_files_inner(root, 0, max_depth, exclude_prefix, &mut results);
    results
}

fn collect_mds_files_inner(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    exclude_prefix: Option<&Path>,
    results: &mut Vec<PathBuf>,
) {
    if depth > max_depth {
        eprintln!(
            "warning: directory depth limit ({max_depth}) reached at {}; \
             deeper files will not be watched",
            dir.display()
        );
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();

        // Skip the output directory when it is nested inside the root.
        if let Some(excl) = exclude_prefix {
            if path.starts_with(excl) {
                continue;
            }
        }

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            // Symlinked dirs skipped to prevent cycles.
            continue;
        }
        if file_type.is_dir() {
            collect_mds_files_inner(&path, depth + 1, max_depth, exclude_prefix, results);
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds") {
            results.push(path);
        }
    }
}

/// Return `true` if `path`'s file name starts with `_` (partial convention, DD2).
pub(crate) fn is_partial(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.starts_with('_'))
        .unwrap_or(false)
}

/// Canonicalize a graph key: exists → `p.canonicalize()`; missing → canonicalize parent + rejoin.
///
/// Used to normalize event paths before graph lookups so macOS `/tmp`→`/private/tmp`
/// and other symlink-resolved differences are handled consistently.
pub(crate) fn graph_key(p: &Path) -> PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    // File doesn't exist (just deleted): canonicalize parent + rejoin filename.
    if let Some(parent) = p.parent() {
        if let Ok(cp) = parent.canonicalize() {
            if let Some(name) = p.file_name() {
                return cp.join(name);
            }
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

/// Snapshot `(mtime, size)` for a set of paths (liveness probe state).
///
/// Returns `None` for the mtime or size field when the file doesn't exist or
/// the metadata call fails — absence is a valid state to track.
pub(crate) fn snapshot_state(
    paths: &HashSet<PathBuf>,
) -> HashMap<PathBuf, (Option<std::time::SystemTime>, Option<u64>)> {
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

/// Return `true` if the current `(mtime, size)` of any path in `paths` differs
/// from its entry in `prev`.
pub(crate) fn state_differs(
    paths: &HashSet<PathBuf>,
    prev: &HashMap<PathBuf, (Option<std::time::SystemTime>, Option<u64>)>,
) -> bool {
    for p in paths {
        let current = match std::fs::metadata(p) {
            Ok(m) => (m.modified().ok(), Some(m.len())),
            Err(_) => (None, None),
        };
        match prev.get(p) {
            Some(old) if *old == current => {}
            _ => return true,
        }
    }
    false
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
/// Falls back to the raw path when canonicalization fails (file may not exist yet).
pub(crate) fn canonicalize_vars_path(vars: Option<PathBuf>) -> Option<PathBuf> {
    vars.map(|p| {
        if p.exists() {
            p.canonicalize().unwrap_or(p)
        } else {
            p
        }
    })
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
            eprintln!("warning: failed to watch {}: {e}", dir.display());
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

/// Receive the next message from the watch channel.
///
/// Returns:
/// - `Ok(Some(msg))` — a message arrived.
/// - `Ok(None)`      — idle tick (only when `tick` is `Some`).
/// - `Err(_)`        — channel disconnected; caller should `break`.
fn recv_next(
    rx: &mpsc::Receiver<Msg>,
    tick: Option<Duration>,
) -> std::result::Result<Option<Msg>, mpsc::RecvTimeoutError> {
    match tick {
        Some(t) => rx.recv_timeout(t).map(Some).or_else(|e| match e {
            mpsc::RecvTimeoutError::Timeout => Ok(None),
            mpsc::RecvTimeoutError::Disconnected => Err(e),
        }),
        None => rx
            .recv()
            .map(Some)
            .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
    }
}

/// Snapshot the `(mtime, size)` of a single path into `last_mtimes`.
///
/// Called at every error-settle point: after a compile error or a write error,
/// the snapshot prevents the liveness tick from re-firing on the same unchanged file.
fn settle_mtime(
    path: &Path,
    last_mtimes: &mut HashMap<PathBuf, (Option<std::time::SystemTime>, Option<u64>)>,
) {
    let entry = match std::fs::metadata(path) {
        Ok(m) => (m.modified().ok(), Some(m.len())),
        Err(_) => (None, None),
    };
    last_mtimes.insert(path.to_path_buf(), entry);
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
                eprintln!("warning: watch error during debounce: {e}");
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
        format,
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
        None => auto_detect_mds_file()?,
        Some(p) => p,
    };

    let is_dir = resolved_input.is_dir();

    // Directory mode constraint checks.
    if is_dir {
        if output.is_some() {
            return Err(miette::miette!(
                "watch directory mode does not support -o/--output; \
                 use --out-dir to specify an output directory"
            ));
        }
        if format == OutputFormat::Messages {
            return Err(miette::miette!(
                "watch directory mode does not support --format messages; \
                 multiple inputs cannot map to a single JSON document"
            ));
        }
    }

    // Canonicalize the input path for stable comparisons.
    let canonical_input = resolved_input
        .canonicalize()
        .map_err(|e| miette::miette!("cannot resolve path {}: {e}", resolved_input.display()))?;

    // Clamp poll_interval: 0 = disable; nonzero ≥ 50ms floor (ADR-021).
    let tick_opt: Option<Duration> = clamp_poll_interval(poll_interval);

    if is_dir {
        run_watch_dir(
            canonical_input,
            out_dir,
            vars,
            set_vars,
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
            format,
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
struct FileCompileCtx {
    entry: PathBuf,
    vars_path: Option<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    format: OutputFormat,
    output_path: Option<PathBuf>,
    output_key: String,
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
    last_mtimes: HashMap<PathBuf, (Option<std::time::SystemTime>, Option<u64>)>,
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
            eprintln!("warning: watch error: {e}");
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
    let runtime_vars = match build_runtime_vars(ctx.vars_path.clone(), ctx.static_set_vars.clone())
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e:?}");
            state.last_mtimes = snapshot_state(&state.foi);
            return;
        }
    };

    let t0 = Instant::now();
    match compile_to_content(&ctx.entry, runtime_vars, &ctx.format, ctx.quiet) {
        Ok(compiled) => {
            // Content-based dedup: skip write + summary line when unchanged.
            let content_changed = state
                .last_written
                .get(&ctx.output_key)
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
                match write_output(ctx.output_path.clone(), &compiled.content, ctx.quiet, false) {
                    Ok(()) => {
                        let elapsed = t0.elapsed().as_millis();
                        let dep_count = compiled.dependencies.len();
                        let out_display = ctx
                            .output_path
                            .as_deref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<stdout>".to_string());
                        state
                            .last_written
                            .insert(ctx.output_key.clone(), compiled.content);
                        if !ctx.quiet {
                            eprintln!(
                                "Recompiled {} ({} deps) in {}ms",
                                out_display, dep_count, elapsed
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!("{e:?}");
                        // Error-settle: update snapshot so we don't re-fire.
                        state.last_mtimes = snapshot_state(&state.foi);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("{e:?}");
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
    format: OutputFormat,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
    tick: Option<Duration>,
) -> Result<()> {
    // Resolve output path ONCE at startup (before entering the watch loop).
    let config = load_config(&entry)?;
    let output_path = resolve_output_path(&Some(entry.clone()), &output, &out_dir, &config)?;
    // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on macOS).
    let vars_path = canonicalize_vars_path(vars);

    // Build runtime vars from the set_vars statics (vars file is reloaded each rebuild).
    let static_set_vars = set_vars;

    // Initial compile.
    let runtime_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;
    if !quiet {
        eprintln!("Watching {}", entry.display());
    }
    // Key: resolved output path string, or the sentinel "<stdout>" when output_path is None.
    let output_key: String = output_path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<stdout>".to_string());

    let initial_deps =
        match compile_and_write(&entry, output_path.clone(), runtime_vars, &format, quiet) {
            Ok(deps) => deps,
            Err(e) => {
                // Initial compile error: print and continue watching (entry dir still watched).
                eprintln!("{e:?}");
                vec![]
            }
        };

    // Set up the watcher AFTER the initial compile so we can record the baseline
    // content in last_written before any FSEvents arrive.
    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_fs = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx_fs.send(Msg::Fs(res));
        },
        notify::Config::default(),
    )
    .map_err(|e| miette::miette!("failed to initialize file watcher: {e}"))?;

    // Install Ctrl+C handler (errors here are non-fatal — we'll catch disconnect).
    let tx_ctrlc = tx.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = tx_ctrlc.send(Msg::Interrupt);
    });

    // Compute initial watch dirs and register them.
    let init_dirs = dirs_to_watch(&entry, &initial_deps, vars_path.as_deref());
    let mut watched_dirs = BTreeSet::new();
    for dir in &init_dirs {
        match watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                watched_dirs.insert(dir.clone());
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

    // Record baseline content AFTER setting up watches to suppress the first
    // synthetic FSEvent from macOS (baseline taken from the same state the watcher sees).
    let mut last_written: HashMap<String, String> = HashMap::new();
    {
        let baseline_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;
        match compile_to_content(
            &entry,
            baseline_vars,
            &format,
            true, /* quiet for baseline */
        ) {
            Ok(out) => {
                last_written.insert(output_key.clone(), out.content);
            }
            Err(_) => {
                // Baseline compile failed — leave last_written empty so next rebuild always writes.
            }
        }
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
    let last_mtimes = snapshot_state(&foi);

    let entry_was_missing = !entry.exists();
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
        format,
        output_path,
        output_key,
        quiet,
    };

    // ── Watch loop ────────────────────────────────────────────────────────────
    // The outer loop processes one event batch at a time and is bounded:
    // it terminates on Interrupt, Disconnected, or when tick probe fires.
    loop {
        match recv_next(&rx, tick) {
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
    /// Sources whose last compile attempt failed (for error-settle logic).
    errored: HashSet<PathBuf>,
    /// Last-seen collected `.mds` set for reconcile/rename detection.
    known_files: BTreeSet<PathBuf>,
    /// Content-dedup map keyed by output path.
    last_written: HashMap<PathBuf, String>,
    /// Parent dirs of dependencies located outside the watched root.
    /// Watched NonRecursive; re-armed by liveness probe.
    external_dep_dirs: BTreeSet<PathBuf>,
    /// Snapshot of (mtime, size) for known files — used by error-settle gate.
    last_mtimes: HashMap<PathBuf, (Option<std::time::SystemTime>, Option<u64>)>,
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

    /// Record a compile error for `src`.
    ///
    /// Inserts into `errored`, clears `forward_deps` entry (empty vec), and
    /// updates `last_mtimes` for error-settle (avoids re-fire on unchanged file).
    fn record_error(&mut self, src: &Path) {
        self.errored.insert(src.to_path_buf());
        self.forward_deps.insert(src.to_path_buf(), vec![]);
        settle_mtime(src, &mut self.last_mtimes);
    }

    /// Collect `known_files` as a `HashSet` for use with `snapshot_state`.
    fn known_set(&self) -> HashSet<PathBuf> {
        self.known_files.iter().cloned().collect()
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
/// 2× duplicated compile→dedup→write→error-settle block inside that function.
///
/// `write_output_file`: when `true` the compiled content is written (non-partial sources).
/// When `false` the graph is refreshed but no output file is created (used for partials
/// and external-only deps where the caller decides skip/continue).
///
/// # Invariants preserved
/// - ADR-016: dep set recomputed from fresh `compile_to_content` output.
/// - PF-004: all reads go through `compile_to_content`.
/// - Error-settle: `state.last_mtimes` updated on both write error and compile error.
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
    let out = output_path_for(src, root, output_base);
    let t0 = Instant::now();
    match compile_to_content(src, runtime_vars.clone(), &OutputFormat::Markdown, quiet) {
        Ok(compiled) => {
            let dep_paths: Vec<PathBuf> = compiled.dependencies.iter().map(PathBuf::from).collect();

            // Partials (DD2): refresh graph edges but do NOT write output.
            if is_partial(src) {
                state.record_success(src, dep_paths, root, None, None);
                settle_mtime(src, &mut state.last_mtimes);
                return;
            }

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
                                out.display(),
                                dep_count,
                                elapsed
                            );
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
                        eprintln!("{e:?}");
                        // Error-settle: update mtime so the gate won't re-fire.
                        state.record_error(src);
                    }
                }
            } else {
                // Content unchanged — still update graph + known_files + mtime baseline.
                state.record_success(src, dep_paths, root, None, None);
                settle_mtime(src, &mut state.last_mtimes);
            }
        }
        Err(e) => {
            eprintln!("{e:?}");
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

    if recovery {
        // Full reconcile: re-collect all files and diff vs known_files.
        let current_files: BTreeSet<PathBuf> =
            collect_mds_files(&ctx.root, MAX_COLLECT_DEPTH, ctx.exclude_prefix.as_deref())
                .into_iter()
                .map(|p| graph_key(&p))
                .collect();

        let appeared: BTreeSet<PathBuf> = current_files
            .difference(&state.known_files)
            .cloned()
            .collect();
        let removed: BTreeSet<PathBuf> = state
            .known_files
            .difference(&current_files)
            .cloned()
            .collect();

        if !appeared.is_empty() || !removed.is_empty() {
            // Soft-error: vars file may be temporarily absent (AC-W7 / AC-C5).
            let runtime_vars =
                match build_runtime_vars(ctx.vars_path.clone(), ctx.static_set_vars.clone()) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e:?}");
                        state.last_mtimes = snapshot_state(&state.known_set());
                        return;
                    }
                };
            let mut batch: BTreeSet<PathBuf> = appeared.clone();
            batch.extend(removed.iter().cloned());
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

        // Replace known_files with the current snapshot.
        state.known_files = current_files;
        // Refresh mtime snapshot.
        state.last_mtimes = snapshot_state(&state.known_set());
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
            eprintln!("warning: watch error: {e}");
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
    let runtime_vars = match build_runtime_vars(ctx.vars_path.clone(), ctx.static_set_vars.clone())
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e:?}");
            state.last_mtimes = snapshot_state(&state.known_set());
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
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
) -> Result<DirStartup> {
    // Load config once from the root directory.
    let config = load_config(&root)?;
    // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on macOS).
    let vars_path = canonicalize_vars_path(vars);
    let static_set_vars = set_vars;

    // Resolve the out_dir as absolute and canonicalized so that
    // the `starts_with(&root)` in-root exclusion check is reliable even when
    // `cwd` contains symlinks (root is already canonical from run_watch, security #8).
    let abs_out_dir: Option<PathBuf> = out_dir.as_ref().map(|d| {
        let abs = if d.is_absolute() {
            d.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(d)
        };
        // Best-effort: canonicalize when the path exists; fall back to absolute form when
        // the out-dir doesn't yet exist (it will be created by write_output on first write).
        abs.canonicalize().unwrap_or(abs)
    });

    // Compute the OutputBase (Fix 2 — subtree mirroring). Reject `..` at startup.
    let output_base = resolve_output_base(abs_out_dir.as_deref(), &config)?;

    // When the out-dir is inside root, exclude it from collection so the watcher
    // doesn't self-pollute (AC-M7 / edge case 6).
    let exclude_prefix: Option<PathBuf> = match &output_base {
        OutputBase::Dir(d) if d.starts_with(&root) => Some(d.clone()),
        _ => None,
    };

    if !quiet {
        eprintln!("Watching directory {}", root.display());
    }

    // Startup compile: compile all .mds files found under root.
    let all_files = collect_mds_files(&root, MAX_COLLECT_DEPTH, exclude_prefix.as_deref());
    let runtime_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;

    // Build the dependency graph and compile all files at startup.
    let mut state = DirWatchState {
        forward_deps: HashMap::new(),
        errored: HashSet::new(),
        known_files: BTreeSet::new(),
        last_written: HashMap::new(),
        external_dep_dirs: BTreeSet::new(),
        last_mtimes: HashMap::new(),
    };

    for source in &all_files {
        let key = graph_key(source);
        let out = output_path_for(&key, &root, &output_base);
        match compile_to_content(source, runtime_vars.clone(), &OutputFormat::Markdown, quiet) {
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

                state.forward_deps.insert(key.clone(), dep_paths);
                state.known_files.insert(key.clone());

                // Partials (DD2): track in graph but don't emit their own output.
                if !is_partial(source) {
                    if let Err(e) = write_output(Some(out.clone()), &compiled.content, quiet, true)
                    {
                        eprintln!("{e:?}");
                    } else {
                        state.last_written.insert(out, compiled.content);
                    }
                }
            }
            Err(e) => {
                eprintln!("{e:?}");
                state.forward_deps.insert(key.clone(), vec![]);
                state.errored.insert(key.clone());
                state.known_files.insert(key);
            }
        }
    }

    // Set up the watcher.
    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_fs = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx_fs.send(Msg::Fs(res));
        },
        notify::Config::default(),
    )
    .map_err(|e| miette::miette!("failed to initialize file watcher: {e}"))?;

    // Install Ctrl+C handler.
    let tx_ctrlc = tx.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = tx_ctrlc.send(Msg::Interrupt);
    });

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

    // Watch external dep dirs NonRecursive (DD3).
    for ext_dir in &state.external_dep_dirs {
        if let Err(e) = watcher.watch(ext_dir, RecursiveMode::NonRecursive) {
            eprintln!(
                "warning: failed to watch external dep dir {}: {e}",
                ext_dir.display()
            );
        }
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
    // Watch the vars dir if it is outside root — soft warning on failure (mirrors the
    // external-dep-dir convention and the liveness probe's best-effort re-arm semantics;
    // a transient failure must not abort the session, applies ADR-021 / consistency fix).
    if let Some(ref vd) = vars_dir_extra {
        if let Err(e) = watcher.watch(vd, RecursiveMode::NonRecursive) {
            eprintln!(
                "warning: failed to watch vars directory {}: {e}",
                vd.display()
            );
        }
    }

    // Build the dedup baseline AFTER the watcher is registered so any OS-queued
    // synthetic events arrive after the baseline is recorded and are filtered out.
    {
        let baseline_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;
        for source in &all_files {
            let key = graph_key(source);
            if is_partial(source) {
                continue; // Partials have no output path in last_written.
            }
            let out = output_path_for(&key, &root, &output_base);
            if state.last_written.contains_key(&out) {
                // Already recorded from startup compile — skip.
                continue;
            }
            match compile_to_content(
                source,
                baseline_vars.clone(),
                &OutputFormat::Markdown,
                true, /* quiet for baseline */
            ) {
                Ok(compiled) => {
                    state.last_written.insert(out, compiled.content);
                }
                Err(_) => {
                    // Baseline compile failed — leave entry absent so next rebuild always writes.
                }
            }
        }
    }

    // Pre-loop mtime snapshot for liveness probe state.
    state.last_mtimes = snapshot_state(&state.known_set());

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
        output_base,
        exclude_prefix,
        vars_dir_extra,
        clear,
        debounce_ms,
        quiet,
    };

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
    } = dir_watch_startup(root, out_dir, vars, set_vars, clear, debounce_ms, quiet)?;

    // ── Watch loop ────────────────────────────────────────────────────────────
    loop {
        match recv_next(&rx, tick) {
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
/// Uses `compile_one_source` for the shared compile→dedup→write→settle sequence.
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
        let out = output_path_for(del_src, root, output_base);
        if out.exists() {
            match std::fs::remove_file(&out) {
                Ok(()) => {
                    if !quiet {
                        eprintln!("Removed {} (source deleted)", out.display());
                    }
                }
                Err(e) => {
                    eprintln!("warning: could not remove {}: {e}", out.display());
                }
            }
        }
        // Use the canonical forget() helper so ALL state maps are cleaned up uniformly
        // (forward_deps, errored, known_files, last_written) — the previous open-coded
        // triple-remove inadvertently omitted known_files.remove (complexity.md / issue #4).
        state.forget(del_src, &out);
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
/// Uses `compile_one_source` for the shared compile→dedup→write→settle sequence.
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
                let out = output_path_for(src, root, output_base);
                state.forget(src, &out);
            }
            continue;
        }

        // External deps are graph nodes but never emit their own output (DD3).
        if !is_in_root {
            // Compile to refresh deps only; suppress output by using quiet=true.
            match compile_to_content(src, runtime_vars.clone(), &OutputFormat::Markdown, true) {
                Ok(compiled) => {
                    let dep_paths: Vec<PathBuf> =
                        compiled.dependencies.iter().map(PathBuf::from).collect();
                    state.forward_deps.insert(src.clone(), dep_paths);
                    state.errored.remove(src);
                }
                Err(e) => {
                    eprintln!("{e:?}");
                    state.errored.insert(src.clone());
                    settle_mtime(src, &mut state.last_mtimes);
                }
            }
            continue;
        }

        // In-root source: full compile→dedup→write→settle via shared helper.
        compile_one_source(src, root, output_base, runtime_vars, quiet, state);
    }

    // 5. Deletions: after importers recompiled, clean up graph + outputs.
    for del_src in &deleted {
        let out = output_path_for(del_src, root, output_base);
        if out.exists() {
            match std::fs::remove_file(&out) {
                Ok(()) => {
                    if !quiet {
                        eprintln!("Removed {} (source deleted)", out.display());
                    }
                }
                Err(e) => {
                    eprintln!("warning: could not remove {}: {e}", out.display());
                }
            }
        }
        state.forget(del_src, &out);
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
        let result = output_path_for(&source, &root, &base);
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
            output_path_for(&a, &root, &base),
            output_path_for(&b, &root, &base),
            "two files with the same stem in different subdirs must not collide"
        );
        assert_eq!(
            output_path_for(&a, &root, &base),
            PathBuf::from("/out/a/x.md")
        );
        assert_eq!(
            output_path_for(&b, &root, &base),
            PathBuf::from("/out/b/x.md")
        );
    }

    // NextToSource: default mode places .md next to source.
    #[test]
    fn output_path_for_next_to_source() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/a/b/foo.mds");
        let result = output_path_for(&source, &root, &OutputBase::NextToSource);
        assert_eq!(result, PathBuf::from("/root/a/b/foo.md"));
    }

    // Compound extension and extensionless stem.
    #[test]
    fn output_path_for_compound_extension() {
        let root = PathBuf::from("/root");
        let source = PathBuf::from("/root/foo.bar.mds");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base);
        assert_eq!(result, PathBuf::from("/out/foo.bar.md"));
    }

    // Path-escape guard (AC-M7): source outside root stays inside out-dir.
    #[test]
    fn output_path_for_source_outside_root_stays_contained() {
        let root = PathBuf::from("/root");
        // Source is completely outside root — strip_prefix will fail.
        let source = PathBuf::from("/elsewhere/a/b/foo.mds");
        let base = OutputBase::Dir(PathBuf::from("/out"));
        let result = output_path_for(&source, &root, &base);
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
                },
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
                },
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

    // T-U5 (renamed): output_path_for does NOT create directories.
    #[test]
    fn output_path_for_no_create() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let new_subdir = dir.path().join("new_out");
        assert!(!new_subdir.exists(), "precondition: subdir does not exist");

        let source = root.join("template.mds");
        let base = OutputBase::Dir(new_subdir.clone());
        let result = output_path_for(&source, &root, &base);
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
        let out = dir.path().join("entry.md");
        let deps = compile_and_write(
            &entry,
            Some(out.clone()),
            None,
            &OutputFormat::Markdown,
            true,
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
