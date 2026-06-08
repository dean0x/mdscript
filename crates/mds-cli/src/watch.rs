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
//! - **Directory mode**: recursive watch on the root dir; compiles each changed
//!   `.mds` file independently. No reverse-dependency tracking — editing a shared
//!   partial refreshes that partial's own output, not the callers.
//!
//! # Key invariants
//!
//! - All content output → stdout ONLY when output resolves to stdout.
//! - All status / warnings / errors → stderr (pipe-safe).
//! - `--quiet` suppresses status + warnings but NOT compile errors.
//! - Exit 0 on clean Ctrl+C; non-zero only on startup failure.
//! - Compile errors during watching never terminate the watcher.
//! - All loops have fixed upper bounds (ADR-016 / no unbounded while-true).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use miette::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

use crate::build::{
    auto_detect_mds_file, build_runtime_vars, compile_and_write, load_config, resolve_output_path,
    resolve_output_path_no_create, OutputFormat,
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
}

// ── Internal message types ────────────────────────────────────────────────────

enum Msg {
    Fs(notify::Result<Event>),
    Interrupt,
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
pub(crate) fn collect_mds_files(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut results = Vec::new();
    collect_mds_files_inner(root, 0, max_depth, &mut results);
    results
}

fn collect_mds_files_inner(dir: &Path, depth: usize, max_depth: usize, results: &mut Vec<PathBuf>) {
    if depth > max_depth {
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            // Symlinked dirs skipped to prevent cycles.
            continue;
        }
        if file_type.is_dir() {
            collect_mds_files_inner(&path, depth + 1, max_depth, results);
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("mds") {
            results.push(path);
        }
    }
}

/// Compute the output path for a source file in directory mode WITHOUT creating directories.
///
/// If `out_dir` is set, the output is `<out_dir>/<stem>.md`.
/// If `config` has an `output_dir`, the output is `<config_dir>/<output_dir>/<stem>.md`.
/// Otherwise, the output is `<source_parent>/<stem>.md` (next to source).
pub(crate) fn output_path_for(
    source: &Path,
    out_dir: Option<&Path>,
    config: &Option<(crate::build::MdsConfig, PathBuf)>,
) -> PathBuf {
    // No explicit -o flag in directory mode; pass None for output.
    let out_dir_pb = out_dir.map(|d| d.to_path_buf());
    match resolve_output_path_no_create(&Some(source.to_path_buf()), &None, &out_dir_pb, config) {
        Ok(Some(path)) => path,
        // Fallback: next-to-source (should not normally reach here since source is a real path)
        _ => {
            let stem = source.file_stem().unwrap_or(source.as_os_str());
            let mut name = std::ffi::OsString::from(stem);
            name.push(".md");
            source.parent().unwrap_or(Path::new(".")).join(name)
        }
    }
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
    // Unwatch removed directories.
    for dir in current_dirs.difference(new_dirs) {
        // Errors here are non-fatal (dir may have been deleted).
        let _ = watcher.unwatch(dir);
    }
    // Watch new directories.
    let mut result = current_dirs.clone();
    for dir in new_dirs.difference(current_dirs) {
        if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
            eprintln!("warning: failed to watch {}: {e}", dir.display());
        } else {
            result.insert(dir.clone());
        }
    }
    // Remove unwatched dirs from result.
    for dir in current_dirs.difference(new_dirs) {
        result.remove(dir);
    }
    result
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
                for p in event.paths {
                    paths.insert(p);
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

    if is_dir {
        run_watch_dir(
            canonical_input,
            out_dir,
            vars,
            set_vars,
            clear,
            debounce,
            quiet,
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
        )
    }
}

// ── Single-file watch ─────────────────────────────────────────────────────────

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
    let initial_deps =
        match compile_and_write(&entry, output_path.clone(), runtime_vars, &format, quiet) {
            Ok(deps) => deps,
            Err(e) => {
                // Initial compile error: print and continue watching (entry dir still watched).
                eprintln!("{e:?}");
                vec![]
            }
        };

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

    let mut current_deps = initial_deps;
    let mut foi = files_of_interest(&entry, &current_deps, vars_path.as_deref());

    // ── Watch loop ────────────────────────────────────────────────────────────
    // The outer loop processes one event batch at a time and is bounded:
    // it terminates on Interrupt or when all senders are dropped.
    while let Ok(first) = rx.recv() {
        let interrupted = match first {
            Msg::Interrupt => true,
            Msg::Fs(Err(e)) => {
                eprintln!("warning: watch error: {e}");
                false
            }
            Msg::Fs(Ok(ref event)) => {
                if !event_is_relevant(event, &foi) {
                    continue; // Not relevant — skip debounce entirely.
                }
                false
            }
        };

        if interrupted {
            if !quiet {
                eprintln!("Stopped watching.");
            }
            return Ok(());
        }

        // Drain the debounce window.
        let (_extra_paths, interrupted2) = drain_debounce(&rx, debounce_ms);
        if interrupted2 {
            if !quiet {
                eprintln!("Stopped watching.");
            }
            return Ok(());
        }

        // Clear terminal if requested (only when stderr is a TTY).
        if clear {
            clear_terminal();
        }

        // Rebuild.
        let t0 = Instant::now();
        let runtime_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;
        match compile_and_write(&entry, output_path.clone(), runtime_vars, &format, quiet) {
            Ok(new_deps) => {
                let elapsed = t0.elapsed().as_millis();
                // ADR-016: recompute dep set from fresh compilation output, never trust stale set.
                let new_dirs = dirs_to_watch(&entry, &new_deps, vars_path.as_deref());
                watched_dirs = resync_watches(&mut watcher, &watched_dirs, &new_dirs);
                foi = files_of_interest(&entry, &new_deps, vars_path.as_deref());
                current_deps = new_deps;
                let out_display = output_path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<stdout>".to_string());
                if !quiet {
                    eprintln!(
                        "Recompiled {} ({} deps) in {}ms",
                        out_display,
                        current_deps.len(),
                        elapsed
                    );
                }
            }
            Err(e) => {
                // Compile error: print and keep watching with the current dep set.
                eprintln!("{e:?}");
            }
        }
    }

    if !quiet {
        eprintln!("Stopped watching.");
    }
    Ok(())
}

// ── Directory watch ───────────────────────────────────────────────────────────

const MAX_COLLECT_DEPTH: usize = 64;

#[allow(clippy::too_many_arguments)]
fn run_watch_dir(
    root: PathBuf,
    out_dir: Option<PathBuf>,
    vars: Option<PathBuf>,
    set_vars: Vec<(String, String)>,
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
) -> Result<()> {
    // Load config once from the root directory.
    let config = load_config(&root)?;
    // Canonicalize so path matches notify event paths (resolves /tmp → /private/tmp on macOS).
    let vars_path = canonicalize_vars_path(vars);
    let static_set_vars = set_vars;

    // Resolve the out_dir as absolute if it isn't already.
    let abs_out_dir: Option<PathBuf> = out_dir.as_ref().map(|d| {
        if d.is_absolute() {
            d.clone()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(d)
        }
    });

    if !quiet {
        eprintln!("Watching directory {}", root.display());
    }

    // Startup compile: compile all .mds files found under root.
    let all_files = collect_mds_files(&root, MAX_COLLECT_DEPTH);
    let runtime_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;
    compile_all_dir(
        &all_files,
        abs_out_dir.as_deref(),
        &config,
        runtime_vars,
        quiet,
    );

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
    if let Some(ref vd) = vars_dir_extra {
        watcher
            .watch(vd, RecursiveMode::NonRecursive)
            .map_err(|e| miette::miette!("failed to watch vars directory {}: {e}", vd.display()))?;
    }

    // ── Watch loop ────────────────────────────────────────────────────────────
    while let Ok(first) = rx.recv() {
        // Collect paths from the triggering event.
        let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
        let mut interrupted = false;

        match first {
            Msg::Interrupt => {
                interrupted = true;
            }
            Msg::Fs(Err(e)) => {
                eprintln!("warning: watch error: {e}");
            }
            Msg::Fs(Ok(event)) => {
                for p in event.paths {
                    changed.insert(p);
                }
            }
        }

        if interrupted {
            if !quiet {
                eprintln!("Stopped watching.");
            }
            return Ok(());
        }

        // Drain debounce window.
        let (extra, interrupted2) = drain_debounce(&rx, debounce_ms);
        changed.extend(extra);
        if interrupted2 {
            if !quiet {
                eprintln!("Stopped watching.");
            }
            return Ok(());
        }

        // Defense-in-depth: ignore events from inside the out-dir subtree to prevent loops.
        if let Some(ref od) = abs_out_dir {
            changed.retain(|p| !p.starts_with(od));
        }

        // Check if the vars file changed.
        let vars_changed = vars_path
            .as_deref()
            .map(|vf| changed.contains(vf))
            .unwrap_or(false);

        // Collect .mds paths under root from the changed set.
        let mds_changed: Vec<PathBuf> = changed
            .iter()
            .filter(|p| {
                p.starts_with(&root) && p.extension().and_then(|e| e.to_str()) == Some("mds")
            })
            .cloned()
            .collect();

        if mds_changed.is_empty() && !vars_changed {
            continue; // Nothing relevant changed.
        }

        if clear {
            clear_terminal();
        }

        // ADR-016: reload vars from disk on every rebuild.
        let runtime_vars = build_runtime_vars(vars_path.clone(), static_set_vars.clone())?;

        if vars_changed {
            // Vars file changed: recompile ALL files.
            let all = collect_mds_files(&root, MAX_COLLECT_DEPTH);
            compile_all_dir(&all, abs_out_dir.as_deref(), &config, runtime_vars, quiet);
        } else {
            // Compile / delete individual files.
            for path in mds_changed {
                if path.exists() {
                    // File exists: compile it. Reuse vars loaded for this rebuild cycle.
                    let out = output_path_for(&path, abs_out_dir.as_deref(), &config);
                    let t0 = Instant::now();
                    match compile_and_write(
                        &path,
                        Some(out.clone()),
                        runtime_vars.clone(),
                        &OutputFormat::Markdown,
                        quiet,
                    ) {
                        Ok(deps) => {
                            let elapsed = t0.elapsed().as_millis();
                            if !quiet {
                                eprintln!(
                                    "Recompiled {} ({} deps) in {}ms",
                                    out.display(),
                                    deps.len(),
                                    elapsed
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("{e:?}");
                        }
                    }
                } else {
                    // Source file deleted: remove matching output (conservative — single file only).
                    let out = output_path_for(&path, abs_out_dir.as_deref(), &config);
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
                }
            }
        }
    }

    if !quiet {
        eprintln!("Stopped watching.");
    }
    Ok(())
}

/// Compile all provided files, printing per-file errors without propagating them
/// (directory mode startup: errors are non-fatal).
fn compile_all_dir(
    files: &[PathBuf],
    out_dir: Option<&Path>,
    config: &Option<(crate::build::MdsConfig, PathBuf)>,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) {
    for source in files {
        let out = output_path_for(source, out_dir, config);
        if let Err(e) = compile_and_write(
            source,
            Some(out),
            runtime_vars.clone(),
            &OutputFormat::Markdown,
            quiet,
        ) {
            eprintln!("{e:?}");
        }
    }
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
        let all = collect_mds_files(dir.path(), 64);
        let names: Vec<_> = all
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(&"a.mds"), "should find top-level a.mds");
        assert!(names.contains(&"b.mds"), "should find sub/b.mds");
        assert!(names.contains(&"c.mds"), "should find deep/c.mds");
        assert!(!names.contains(&"ignore.txt"), "should skip non-.mds files");

        // depth=0 should find only top-level files.
        let top_only = collect_mds_files(dir.path(), 0);
        assert_eq!(top_only.len(), 1, "depth=0 should return only root files");
    }

    // T-U5: output_path_for computes target WITHOUT creating directories.
    #[test]
    fn output_path_for_no_create() {
        let dir = tempfile::tempdir().unwrap();
        let new_subdir = dir.path().join("new_out");
        assert!(!new_subdir.exists(), "precondition: subdir does not exist");

        let source = dir.path().join("template.mds");
        let result = output_path_for(&source, Some(&new_subdir), &None);
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
