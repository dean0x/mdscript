---
feature: mds-cli
name: MDS CLI
description: "Use when adding new subcommands, changing output-path resolution logic, modifying the watch architecture (single-file or directory mode), adding new compile paths, updating mds.json config handling, debugging stdout/stderr stream separation, or investigating exit codes. Keywords: mds build, mds check, mds watch, mds init, OutputFormat, messages mode, run_build, run_watch, build.rs, watch.rs, mds.json, output_dir, resolve_output_path, resolve_output_base, OutputBase, output_path_for, compile_and_write, compile_to_content, debounce, notify, ctrlc, content-dedup, last_written, dirs_to_watch, files_of_interest, exit_code, MAX_FILE_SIZE, read_build_input, BuildArgs, WatchArgs, forward_deps, affected_sources, is_partial, graph_key, process_dir_batch, process_dir_batch_incremental, process_dir_batch_vars_changed, liveness_probe_file, liveness_probe_dir, snapshot_state, state_differs, poll_interval, is_content_event, inotify Access, Linux busy-loop, external_recovery_decision, edge-triggered recovery, missing_watched_dirs, missing_external_dirs, recv_next, settle_mtime, FileCompileCtx, FileWatchState, DirWatchCtx, DirWatchState, LivenessState, compile_one_source, DirStartup, dir_watch_startup, handle_fs_event_file, handle_fs_event_dir, rebuild_file, armed_dirs, armed_external_dirs, root_armed, known_set, record_success, record_error, forget, stop_watching, clamp_poll_interval."
category: architecture
directories: [crates/mds-cli/src, crates/mds-cli/tests]
referencedFiles:
  - crates/mds-cli/src/main.rs
  - crates/mds-cli/src/build.rs
  - crates/mds-cli/src/watch.rs
  - crates/mds-cli/tests/cli_watch.rs
  - crates/mds-cli/tests/common/mod.rs
  - crates/mds-cli/Cargo.toml
created: 2026-06-09
updated: 2026-06-11
---

# MDS CLI

## Overview

`mds-cli` is the binary crate that implements the `mds` command-line tool. It has four subcommands — `build`, `check`, `watch`, and `init` — all wired through `main.rs` using clap. The crate is split into three source files: `main.rs` (CLI surface + dispatch), `build.rs` (all shared compile helpers, output-path resolution, and config), and `watch.rs` (the file-watcher loop). This split exists so `watch.rs` can reuse build helpers without duplicating logic or bypassing resource limits.

The crate calls into `mds-core` (aliased as `mds` in Cargo.toml) for all actual compilation. The CLI layer owns: input resolution, output-path computation, project config discovery, runtime-vars merging, stream routing (stdout vs file), exit-code mapping, and the watch event loop.

## System Context

- **mds build** — compiles one `.mds` file (or stdin) to Markdown or JSON messages. Output goes to a file (default: sibling `.md`) or stdout (`-o -`).
- **mds check** — validates without rendering. Always silent on success unless warnings exist; prints `OK: <path>` to stderr on success.
- **mds watch** — long-running watcher: single-file mode tracks transitive imports; directory mode tracks a reverse-dependency graph and recompiles all transitive importers of any changed file.
- **mds init** — writes a starter `.mds` template file. Rejects `..` path components in the output filename.

All status messages (banners, warnings, "Compiled to", "Recompiled", "Stopped watching.") go to **stderr**. Compiled content goes to **stdout only when output resolves to stdout** (i.e. `-o -` or stdin input with no output flags). This is a hard invariant — pipe consumers depend on it.

## Component Architecture

### build.rs — shared compile helpers

All `pub(crate)` functions consumed by both `build` and `watch`:

| Function | Purpose |
|---|---|
| `resolve_output_path` | Six-level precedence chain: `-o -` → `-o path` → stdin-default → `--out-dir` → `mds.json` → sibling `.md`. Inlined from the former `resolve_output_path_impl` — the dead `create=false` branch was removed. |
| `load_config` | Walk-up from input file to find `mds.json`; bounded by `MAX_TRAVERSAL_DEPTH`; enforces 1 MB cap on config file |
| `build_runtime_vars` | Merge `--vars` file + `--set KEY=VALUE` overrides into a single `HashMap<String, mds::Value>` |
| `read_build_input` | Read source file or stdin, enforce `MAX_FILE_SIZE` (PF-004 compliance) |
| `compile_to_content` | Compile without writing — returns `CompileOutput { content, dependencies }` |
| `compile_and_write` | Wraps `compile_to_content` + `write_output`; returns dep list for watch resync |
| `write_output` | Write to file or stdout; `announce` flag controls the "Compiled to" banner |
| `auto_detect_mds_file` | Scan cwd for exactly one `.mds` file; errors on zero or many |
| `exit_code` | Map `miette::Error` → 0/1/2/3 (see Exit Codes section) |
| `parse_cli_value` | Coerce `--set VALUE` string to typed `mds::Value` (bool/int/float/array/string) |
| `settle_mtime` | Snapshot `(mtime,size)` of a single path into `last_mtimes` at error-settle points |

Note: `resolve_output_path_no_create` was **removed** in the dir-mode refactor — dir-mode watch now uses `output_path_for(source, root, &output_base)` which is inherently pure (no dir creation). `resolve_output_path_impl` was also removed — it was the dead `create` parameter variant; `resolve_output_path` is now the direct, inlined implementation with only the `create=true` path.

### watch.rs — file watcher (architecture)

The watch loop uses `notify 8` (non-recursive for single-file, recursive for directories) + `ctrlc 3.5`. Events and Ctrl+C are both sent over a single `mpsc::Sender<Msg>` where `Msg` is either `Msg::Fs(notify::Result<Event>)` or `Msg::Interrupt`. This design lets the main loop handle both interrupt and FS events in one receive call.

**`recv_next(rx, tick: Option<Duration>)`** — shared helper used by both loops. Returns:
- `Ok(Some(msg))` — a message arrived
- `Ok(None)` — idle tick (only when `tick` is `Some`)
- `Err(Disconnected)` — channel disconnected; caller should break

**`clamp_poll_interval(poll_interval: u64) -> Option<Duration>`** — extracted pure helper (AC-C contract):
- `0` → `None` (disables liveness probe; blocking receive, no tick)
- nonzero → `Some(Duration::from_millis(poll_interval.max(50)))` — enforces 50 ms floor
Unit-tested independently: `clamp_poll_interval_zero_disables_probe`, `clamp_poll_interval_one_clamped_to_50ms`, `clamp_poll_interval_exactly_50_unchanged`, `clamp_poll_interval_above_floor_unchanged`, `clamp_poll_interval_75ms_unchanged`.

**`is_content_event(kind: &notify::EventKind) -> bool`** — filters `EventKind::Access(_)` events. On Linux, inotify emits `IN_ACCESS`/`IN_OPEN`/`IN_CLOSE_NOWRITE` events whenever a file is merely **read** (not written). The compile step reads `.mds` source files, which causes inotify to emit Access events for those same files. Without this filter the watcher ingests those events, recompiles, reads again, emits more Access events, and enters a busy-loop (~3000 recompiles/second). macOS FSEvents does not report reads, so this was a Linux-only bug. Both the event path and `drain_debounce` call `is_content_event` to drop Access variants before any path check.

**`stop_watching(quiet: bool)`** — emits "Stopped watching." to stderr (unless quiet). Called at every Ctrl+C exit point in both watch loops.

### Single-file mode structs and extracted functions

**`FileCompileCtx`** — groups compile-time constants for single-file mode, resolved once at startup:
```rust
struct FileCompileCtx {
    entry: PathBuf,
    vars_path: Option<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    format: OutputFormat,
    output_path: Option<PathBuf>,
    output_key: String,   // display string for output path, or "<stdout>"
    quiet: bool,
}
```
Eliminates the 6-7 individual constant arguments previously threaded through `rebuild_file` / `liveness_probe_file`, removing `#[allow(clippy::too_many_arguments)]` suppressions.

**`FileWatchState`** — groups mutable loop state for single-file mode, updated on every rebuild or liveness tick:
```rust
struct FileWatchState {
    watched_dirs: BTreeSet<PathBuf>,
    armed_dirs: BTreeSet<PathBuf>,  // subset of watched_dirs successfully armed with the OS
    foi: HashSet<PathBuf>,          // files of interest (entry + deps + vars)
    last_mtimes: HashMap<PathBuf, (Option<SystemTime>, Option<u64>)>,
    last_written: HashMap<String, String>,  // content-dedup keyed by output_key
    entry_was_missing: bool,
    first_tick: bool,
    missing_watched_dirs: BTreeSet<PathBuf>,
}
```
`armed_dirs` tracks dirs actually registered with the OS watcher so `liveness_probe_file` can skip the `watcher.watch()` syscall for dirs already known-good — steady-state idle cost becomes O(missing_dirs) ≈ O(0) rather than O(watched_dirs) (ADR-021).

**`FileEventAction`** enum — outcome from `handle_fs_event_file`:
- `Skip` — Access event or irrelevant path
- `Stop` — Ctrl+C received
- `Rebuild` — triggers a rebuild after debounce window

**`handle_fs_event_file(msg, foi, rx, debounce_ms, clear) -> FileEventAction`** — classifies a `Msg` for single-file mode. Drops Access events, checks relevance against `foi`, drains debounce, clears terminal if requested.

**`rebuild_file(ctx, watcher, state)`** — the single canonical compile→dedup→resync→write→settle sequence for single-file mode. Called from both the idle-tick and FS-event paths. Preserves ADR-016 (fresh dep recompute) and PF-004 (all reads through `compile_to_content`). Updates `armed_dirs` to mirror `watched_dirs` after successful resync.

**`liveness_probe_file(ctx, watcher, state) -> bool`** — idle-tick liveness probe for single-file mode. Only calls `watcher.watch()` for dirs that were missing last tick or not yet armed (O(missing_dirs) idle cost). Removes vanished dirs from `armed_dirs` using already-computed exists flags to avoid a second stat per dir. Uses `external_recovery_decision` for edge-triggered recovery. Returns `true` when a rebuild is needed.

**Single-file watch flow** (`run_watch_file`):
1. Load config + resolve output path once at startup.
2. Perform initial compile via `compile_and_write` (announces "Compiled to").
3. Register `notify` watchers on all **parent directories** (not file inodes — survives atomic-rename saves).
4. Record baseline content in `last_written` after watcher registration to suppress macOS synthetic FSEvents.
5. Pre-seed `last_mtimes` (mtime+size snapshot) for liveness probe state.
6. Pre-seed `missing_watched_dirs`: the set of desired watch dirs that don't exist yet at startup, so their first appearance is treated as a recovery edge rather than a per-tick walk.
7. Initialize `armed_dirs = watched_dirs.clone()` so startup-registered dirs are known-good.
8. Main loop: `recv_next` → `handle_fs_event_file` (Skip/Stop/Rebuild) or liveness tick → `liveness_probe_file` → `rebuild_file` if needed.

### Directory mode structs and extracted functions

**`DirWatchCtx`** — groups compile-time context for directory mode:
```rust
struct DirWatchCtx {
    root: PathBuf,
    vars_path: Option<PathBuf>,
    static_set_vars: Vec<(String, String)>,
    output_base: OutputBase,
    exclude_prefix: Option<PathBuf>,
    vars_dir_extra: Option<PathBuf>,  // vars dir if outside root
    clear: bool,
    debounce_ms: u64,
    quiet: bool,
}
```

**`DirWatchState`** — mutable state for the directory-mode watch loop:
```rust
struct DirWatchState {
    forward_deps: HashMap<PathBuf, Vec<PathBuf>>,  // canonical source → canonical transitive deps
    errored: HashSet<PathBuf>,
    known_files: BTreeSet<PathBuf>,
    last_written: HashMap<PathBuf, String>,
    external_dep_dirs: BTreeSet<PathBuf>,
    last_mtimes: HashMap<PathBuf, (Option<SystemTime>, Option<u64>)>,
}
```
Methods:
- `record_success(src, dep_paths, root, out, content)` — updates `forward_deps`, `errored`, `known_files`, `last_written`, `external_dep_dirs`
- `record_error(src)` — inserts into `errored`, clears `forward_deps` entry, calls `settle_mtime`
- `known_set() -> HashSet<PathBuf>` — returns `known_files` as a `HashSet` for use with `snapshot_state`
- `forget(src, out)` — removes all state for a deleted source and its output

**`LivenessState`** — state for the dir-mode liveness probe:
```rust
struct LivenessState {
    first_tick: bool,
    root_was_missing: bool,
    root_armed: bool,           // whether OS watcher is currently armed for root
    missing_external_dirs: BTreeSet<PathBuf>,
    armed_external_dirs: BTreeSet<PathBuf>,  // external dirs currently held by OS watcher
}
```
`root_armed` mirrors the `armed_dirs` discipline from file mode: `watcher.watch(root, Recursive)` is only called when necessary (first tick, vanish→reappear, or previous arm failed), never on healthy steady-state ticks. This prevents the O(subtree) re-WalkDir / FSEvents stream teardown on every idle tick (ADR-021).

`armed_external_dirs` tracks which external dep dirs the OS watcher actually holds. When `process_dir_batch_incremental` prunes `external_dep_dirs` after a batch (cross-root `@import` removed), `liveness_probe_dir` detects the difference and calls `watcher.unwatch()` on dropped dirs. Without this, OS watches leak for the process lifetime, eventually approaching `fs.inotify.max_user_watches`.

**`DirStartup`** — return value from `dir_watch_startup`:
```rust
struct DirStartup {
    watcher: RecommendedWatcher,
    rx: mpsc::Receiver<Msg>,
    state: DirWatchState,
    liveness: LivenessState,
    ctx: DirWatchCtx,
}
```

**`dir_watch_startup(...) -> Result<DirStartup>`** — extracted one-time startup function for directory mode. Loads config, compiles all sources at startup, sets up watcher + Ctrl+C handler, records dedup baseline, seeds mtime snapshot, and builds context structs. Seeds `liveness.armed_external_dirs` with any external dep dirs that existed at startup (so first tick skips re-arm for healthy dirs). Seeds `liveness.missing_external_dirs` with dirs that don't yet exist (so their first appearance is an edge, not a per-tick walk).

**`compile_one_source(src, root, output_base, runtime_vars, quiet, state) -> ()`** — shared kernel for both the `vars_changed` full-recompile loop and the per-affected-source incremental loop in `process_dir_batch_incremental`. Handles compile→dedup→write→error-settle sequence. For partials: refreshes graph edges but skips `write_output`.

**`DirEventOutcome`** enum — outcome from `handle_fs_event_dir`:
- `Skip` — Access event, no `.mds` paths, no vars change
- `Stop` — Ctrl+C received
- `Done` — batch processed

**`handle_fs_event_dir(msg, ctx, rx, state) -> DirEventOutcome`** — processes a `Msg` for directory mode. Drops Access events, drains debounce, filters non-`.mds` paths and paths inside the out-dir, checks vars change, calls `process_dir_batch`.

**`liveness_probe_dir(ctx, watcher, liveness, state)`** — dir-mode liveness probe. Re-arms root (Recursive, gated by `root_armed`) + external dirs (gated by `armed_external_dirs`) + vars dir. Unwatches pruned external dirs via `armed_external_dirs` diff. Uses `external_recovery_decision` for edge-triggered external dir recovery. `missing_external_dirs` is pruned to only dirs still in `external_dep_dirs` before the check.

**Directory mode flow** (`run_watch_dir`):
1. `dir_watch_startup` — loads config once; computes `OutputBase`; rejects `..` in `mds.json output_dir` at startup.
2. Compile all `.mds` files under root with `collect_mds_files` (depth-bounded at `MAX_COLLECT_DEPTH = 64`, excludes out-dir subtree when it is inside root). Build `forward_deps`, `errored`, `known_files`, `external_dep_dirs`, `last_mtimes` during startup.
3. Register recursive watcher on root; NonRecursive watchers on external dep dirs + optional vars dir.
4. Record content-dedup baseline after watcher registration.
5. On events: drop `Access` events (`is_content_event`); canonicalize changed paths; accept `.mds` paths under root OR in external dep dirs. If vars file changed, call `process_dir_batch_vars_changed`. Otherwise, call `process_dir_batch_incremental`.
6. Liveness probe (idle tick): re-arm root (gated) + external dirs (gated) + vars dir. On recovery (root reappeared, re-arm failed, first tick): run `collect_mds_files` diff → `process_dir_batch` for appeared/removed.

### Liveness Probe and Edge-Triggered Recovery

The liveness probe uses **edge-triggered recovery** in both single-file and directory modes (ADR-021 / AC-P1):

**File mode** (`missing_watched_dirs: BTreeSet<PathBuf>` in `FileWatchState`):
- Desired watch dirs are evaluated per-tick using `external_recovery_decision(&missing_watched_dirs, &dir_statuses)`.
- `external_recovery_decision` returns `(recovery_needed, now_missing)`.
- Recovery fires ONLY when: (a) first tick, (b) a previously-missing dir reappears (vanish→reappear edge), or (c) an existing dir fails to re-arm (genuine watch loss).
- A dir that STAYS missing across ticks does NOT trigger recovery — avoids per-tick error spam when the entry's parent dir is permanently absent.
- `entry_was_missing && entry_now_exists` is a separate edge trigger for the entry file itself.
- `armed_dirs` optimization: `watcher.watch()` is only called for dirs that were missing last tick or not yet armed — already-armed dirs are skipped to achieve O(missing_dirs) idle cost.
- Vanished dirs are pruned from `armed_dirs` using already-computed exists flags (no second stat per tick).

**Directory mode** (`LivenessState.missing_external_dirs: BTreeSet<PathBuf>`):
- Same `external_recovery_decision` function used for external dep dirs.
- Root recovery: `(root_now_exists && !root_ok)` — an existing root whose re-arm FAILED (genuine watch loss). A merely-missing root is handled by the `root_was_missing && root_now_exists` vanish→reappear edge. **NOT** `!root_ok` alone, which would fire on every tick while root stays missing.
- `liveness.root_was_missing = !root_now_exists` is updated each tick to track the transition.
- `root_armed` is updated: `true` on successful re-arm, `false` when root disappears. Prevents `watcher.watch(root, Recursive)` on every healthy tick (OS re-WalkDir cost).
- `missing_external_dirs` is pruned each tick to only dirs still in `state.external_dep_dirs` (prevents accumulation of stale entries after a cross-root import is removed).
- `armed_external_dirs` is explicitly diffed against `state.external_dep_dirs` each tick to `unwatch()` pruned dirs (prevents OS watch leaks).

### process_dir_batch split

`process_dir_batch` is a thin dispatcher:
- If `vars_changed`: calls `process_dir_batch_vars_changed` — full recompile of all known files, prunes stale entries, handles deletions in the same batch.
- Otherwise: calls `process_dir_batch_incremental` — incremental compile using `affected_sources` DFS.

**`process_dir_batch_incremental`** (steps):
1. Partition changed paths into `existing` / `deleted`.
2. Seeds = existing ∪ deleted ∪ (errored ∩ real-change batch).
3. Affected = seeds ∪ transitive importers (uses start-of-batch `forward_deps` snapshot via `affected_sources`).
4. Compile each affected source that exists and is in-root or a known external dep. Uses `compile_one_source` for in-root sources.
5. Deletions: remove outputs, call `state.forget(del_src, out)`.
6. Prune `external_dep_dirs` to only dirs still referenced by live `forward_deps` (prevents monotonic growth from removed cross-root imports).

**Ghost entry pruning**: if a source appears in `affected` but is not in `deleted` yet doesn't exist (issue #7 — delete event never delivered), it is proactively removed from `errored`, `forward_deps`, and `known_files` via `state.forget()`.

### Dependency models

- **Single-file mode**: **forward deps** — recompute deps from each `compile_to_content` output; set of watched dirs and `files_of_interest` updated on each rebuild. Stale dep sets are never reused (ADR-016).
- **Directory mode**: **reverse-dep graph** — `forward_deps: HashMap<PathBuf, Vec<PathBuf>>` (canonical source → canonical transitive deps). On a change event, `affected_sources(forward_deps, seeds)` does DFS with a visited set (cycle-safe) to find all transitive importers. The graph is refreshed from fresh compilation output after each successful compile.

### Partials (DD2)

A `.mds` file whose name starts with `_` is a **partial**: it is tracked in the dependency graph and triggers rebuilds of its importers on edit, but it never emits its own `.md` output file. `is_partial(path)` tests the `_` prefix. Partials are graph nodes — they have entries in `forward_deps` and `known_files` — but `compile_one_source` skips `write_output` for them (uses `record_success(src, dep_paths, root, None, None)`).

### Cross-root imports (DD3)

If a source file imports a `.mds` file outside the watched root, the parent directory of that external file is added to `external_dep_dirs` and watched NonRecursive. An event for an external `.mds` path is accepted as a seed into `affected_sources`. External files are **never** compiled to their own output (only in-root importers are emitted). External dep dirs are re-armed by the liveness probe (gated by `armed_external_dirs`). `process_dir_batch_incremental` recomputes `external_dep_dirs` from live `forward_deps` after each batch to avoid monotonic growth from removed imports. `liveness_probe_dir` explicitly unwatches pruned dirs.

### Output-path resolution

**File mode / `mds build`** — six-level chain in `resolve_output_path` (directly, after dead `create=false` branch removal):
```
1. -o -            → None (stdout)
2. -o <path>       → Some(path)  [wins over mds.json config]
3. stdin + no flags → None (stdout)
4. --out-dir <dir>  → Some(<dir>/<stem>.md)
5. mds.json         → Some(<config_dir>/<output_dir>/<stem>.md)
6. default          → Some(<source_dir>/<stem>.md)
```

**Directory mode** — `OutputBase` enum computed once at startup by `resolve_output_base`:
```
enum OutputBase { Dir(PathBuf), NextToSource }

Precedence:
1. --out-dir  → Dir(abs_out_dir)
2. mds.json build.output_dir  → Dir(config_dir.join(output_dir))   [rejects '..' at startup]
3. default    → NextToSource
```

`output_path_for(source, root, base)` — infallible, no dir creation:
- `Dir(d)`: `rel = source.strip_prefix(root)`; `d.join(rel).with_extension("md")`. If strip_prefix fails (source outside root — canonicalization edge case), falls back to `d.join(stem.md)` — **never joins an absolute path** (path-escape guard, AC-M7). A `debug_assert!` enforces the containment invariant in debug builds.
- `NextToSource`: `source.with_extension("md")` (uses Rust standard library method directly).

Output dirs are created on write by `write_output` (which calls `create_dir_all` on the parent).

`mds.json` is found by walking up from the input file. Its `build.output_dir` field is rejected if it contains `..` components (path traversal guard). `resolve_output_path_no_create` was **removed** — dir-mode deletion now uses `output_path_for` which is inherently pure (no dir creation).

## Self-Healing Watcher (ADR-021)

The outer loop uses `recv_next(rx, tick)` where `tick = Some(Duration)` when `poll_interval > 0` (default 1000ms; nonzero values clamped to ≥50ms via `clamp_poll_interval`). On each idle `Timeout` tick, the liveness probe runs:

1. **Re-arm (gated)**: File mode: `watcher.watch(path, mode)` only for paths not yet in `armed_dirs` or missing last tick. Dir mode: `watcher.watch(root, Recursive)` only when `need_root_rearm` (`first_tick || (root_was_missing && root_now_exists) || !root_armed`); external dirs only when not in `armed_external_dirs` or previously missing.
2. **Unwatch pruned dirs (dir mode)**: External dirs removed from `external_dep_dirs` by a batch are detected by diffing `armed_external_dirs` vs `external_dep_dirs` and unwatched explicitly.
3. **Recovery gate (edge-triggered)**: full reconcile runs only when `external_recovery_decision` returns `recovery_needed = true`. A dir that STAYS missing does NOT trigger recovery — only vanish→reappear or re-arm failure of an existing dir does.
4. **Single-file mode**: `state_differs` check over `files_of_interest` using `(mtime, size)` snapshots. Triggers rebuild if any file changed or recovery applies.
5. **Dir mode recovery**: `collect_mds_files` diff vs `known_files` → `process_dir_batch` for appeared/removed. Replaces `last_mtimes` from fresh snapshot.
6. **Pre-loop seeding**: `last_mtimes` and `missing_watched_dirs`/`missing_external_dirs` initialized before the loop, so the first tick detects no change and emits zero `Recompiled` lines (AC-W4).
7. **Error-settle** (`settle_mtime`): on compile error, the `(mtime,size)` snapshot is updated for the failed source so the tick gate doesn't re-fire on unchanged files. `errored` sources are retried only when a real change event arrives, not on each tick.

`poll_interval = 0` → `tick = None` (blocking `rx.recv()`, no timeout arm, no liveness probe).

## Component Interactions

**Compile pipeline boundary**: `mds-cli` never calls `mds::compile` directly with bare file contents that bypass the resource-limit checks. All compile paths flow through either:
- `mds::compile_with_deps(path, ...)` — used for Markdown mode (enforces `MAX_FILE_SIZE` internally through the resolver)
- `read_build_input(path)` → `mds::compile_messages_str_with_deps(source, base_dir, ...)` — used for Messages mode

**PF-004 compliance**: both `compile_to_content` and `read_build_input` carry explicit doc comments marking them as the PF-004 enforcement points. The partial/reverse-dep/reconcile paths all go through `compile_to_content`. There is no bare `std::fs::read_to_string` of any `.mds` file.

**Dep tracking**: `compile_and_write` and `compile_to_content` return `dependencies: Vec<String>` (absolute paths). Single-file mode uses this to update `dirs_to_watch` and `files_of_interest` on every rebuild. Dir-mode inserts dep paths into `forward_deps` and `external_dep_dirs` on every successful compile (ADR-016).

## Exit Codes

`exit_code()` in `build.rs` maps `miette::Error` to:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Logical/syntax error (undefined var, arity mismatch, recursion, generic miette errors) |
| 2 | I/O / filesystem error (`MdsError::Io`, `FileNotFound`, `NotMdsFile`) |
| 3 | Resource limit exceeded (`MdsError::ResourceLimit`) |

Only `MdsError` values wrapped via `.map_err(miette::Error::from)` downcast correctly. Errors created via `miette::miette!()` macro always produce exit code 1. Clap parse errors (e.g., invalid `--poll-interval`) exit 2 via clap's default.

## stdout / stderr Stream Contract

This is the most important operational invariant for pipe consumers:

- **stdout**: compiled content ONLY (when `-o -` or stdin with no output flags). No status, no warnings, no error messages.
- **stderr**: everything else — banners, warnings, "Compiled to", "Recompiled", "Stopped watching.", compile errors, "OK:" for check, ANSI clear sequences. The reverse-dep and reconcile paths also write exclusively to stderr.
- **`--quiet` (`-q`)**: suppresses banners, warnings, and "Compiled to"/"Recompiled" status lines. Does NOT suppress compile errors (errors always appear on stderr regardless of quiet).
- **`--clear`**: emits `\x1b[2J\x1b[3J\x1b[H` to stderr before each rebuild BUT ONLY when `std::io::stderr().is_terminal()` is true. On piped stderr (CI, scripts) it is a complete no-op.

## Debounce Architecture

Debounce is hand-rolled (notify-debouncer-full deliberately not used). The `drain_debounce` function:
- Takes a `debounce_ms` parameter (default 100, `--debounce 0` for immediate rebuilds).
- Computes a `deadline = Instant::now() + Duration::from_millis(debounce_ms)`.
- Loops calling `rx.recv_timeout(remaining)` until deadline or disconnect.
- **Drops Access events** (`is_content_event` check) — same filter as the main event path, so the Linux inotify busy-loop cannot restart through the debounce window.
- Returns `(BTreeSet<PathBuf>, interrupted)`.
- The outer loop is bounded by `recv_timeout` semantics — there is no unbounded while-true.

`--debounce` (burst coalescing) and `--poll-interval` (liveness-probe cadence) are **orthogonal** — debounce applies after the first event arrives; poll-interval is the idle tick between events.

`--debounce 0` is used in integration tests for determinism (no wait for debounce window).

## mds.json Project Config

`load_config(start: &Path) → Result<Option<(MdsConfig, PathBuf)>>`:
- Walks upward from the input file's directory, checking for `mds.json` at each level.
- Bounded by `MAX_TRAVERSAL_DEPTH` (imported from `mds-core`).
- Enforces a 1 MB cap on the config file itself.
- Returns `(config, config_dir)` where `config_dir` is the directory containing `mds.json` (used to resolve relative `output_dir` values).
- `output_dir` in `mds.json` is the only currently supported field.

`mds.json` example:
```json
{ "build": { "output_dir": "dist" } }
```

In **file/build mode**: `mds build src/prompt.mds` writes `dist/prompt.md` relative to the `mds.json` location.
In **directory watch mode**: `mds watch src/ --out-dir` (or via `mds.json`) mirrors the subtree, so `src/a/b/prompt.mds` → `dist/a/b/prompt.md`.

`..` in `output_dir` is rejected:
- File/build mode: rejected inside `resolve_output_path`.
- Dir watch mode: rejected at startup inside `resolve_output_base`.

## Anti-Patterns

- **Bare `std::fs::read_to_string` + direct `mds::compile_str`** — bypasses the `MAX_FILE_SIZE` cap (PF-004). All reads must go through `read_build_input` or `mds::compile_with_deps`. This applies to ALL paths including partials, reconcile, and cross-root files.

- **Trusting stale dependency sets in the watch loop** — the dep list from the PREVIOUS rebuild must never be reused as-is for the next cycle. Always recompute from `compile_to_content` output (ADR-016). Using stale deps causes phantom watches on deleted imports or missed watches on newly added imports.

- **Writing compile output to stdout during the watch loop** — only the initial compile (`compile_and_write`) is allowed to write to stdout; subsequent rebuilds compare content and only call `write_output` if changed, with `announce=false` to suppress the duplicate "Compiled to" line. Removing the content-dedup check causes duplicate writes that corrupt downstream pipe consumers.

- **Calling `watcher.watch` recursively for single-file mode** — the watcher must use `RecursiveMode::NonRecursive` for each parent directory, not recursive on the entry's root. Recursive mode on a shared project root would generate massive event noise from unrelated files.

- **Adding a new compile path that uses `resolve_output_path_no_create`** — this function was removed. Dir-mode watch now uses `output_path_for(source, root, &output_base)` which is inherently pure (no dir creation). Dir creation happens in `write_output` via `create_dir_all`. Similarly, do not look for `resolve_output_path_impl` — it was removed when the dead `create=false` branch was eliminated.

- **Using `--format messages` in directory watch mode** — rejected at startup. Multiple `.mds` files cannot map to a single JSON document. Always validate directory-mode constraints before entering the watch loop.

- **Per-tick full-tree walk** — O(tree) cost on every tick. The liveness probe is gated: cheap re-arm + stat only; full `collect_mds_files` only on recovery/first-tick (ADR-021 / DD1). File mode uses `armed_dirs` to skip `watcher.watch()` for already-armed dirs. Dir mode uses `root_armed` and `armed_external_dirs` to skip `watcher.watch()` for the root and external dirs when healthy.

- **Not filtering Access events from the event path** — inotify on Linux emits Access events when `.mds` files are read during compilation. Without `is_content_event` filtering BOTH in the main event path and in `drain_debounce`, the watcher enters a busy-loop (thousands of recompiles/second on Linux).

- **Using `!root_ok` alone as the dir-mode root recovery condition** — this triggers recovery on every idle tick while the root dir is absent (per-tick error spam). The correct condition is `root_now_exists && !root_ok` for re-arm failure of an existing root. The vanish→reappear edge is handled separately by `root_was_missing && root_now_exists`.

- **Not seeding `missing_watched_dirs` / `missing_external_dirs` before the loop** — if a watched directory is already missing at startup and recovery is not edge-triggered, every tick fires a recovery until the directory reappears. Pre-seed these sets so the first tick has the correct baseline.

- **Not pruning `external_dep_dirs` after incremental batch** — `external_dep_dirs` is monotonically grown by `record_success` on every compile. When a cross-root `@import` is removed, the now-unused external dir stays in the set and the liveness probe re-arms it on every tick forever. `process_dir_batch_incremental` recomputes `external_dep_dirs` from live `forward_deps` after each batch to prune abandoned dirs.

- **Not tracking `armed_external_dirs` in `LivenessState`** — without this, pruned external dirs can never be unwatched. OS watches leak for the process lifetime, approaching `fs.inotify.max_user_watches`. `liveness_probe_dir` diffs `armed_external_dirs` vs `external_dep_dirs` to call `watcher.unwatch()` on dropped dirs.

- **Calling compile functions in the watch loop without `compile_one_source`** — always use the shared `compile_one_source` helper for in-root sources in dir mode. It handles the compile→dedup→write→error-settle sequence uniformly. Direct calls to `compile_to_content` + manual state updates are error-prone and will diverge.

- **Forgetting ghost entry pruning in `process_dir_batch_incremental`** — a source can appear in `affected` (seeded from `errored`) but not exist and not be in `deleted` (delete event never delivered). Without `state.forget()` on such entries, they accumulate as ghost entries and waste per-batch allocation on every subsequent real-change event.

- **Using a magic upper-bound constant in bounded-count tests** — the scale-invariant two-window pattern (`count_w1 == count_w2`) is the correct approach. A hardcoded `<= N` bound is brittle under timing variations.

## Gotchas

- **Linux inotify Access events**: on Linux, inotify emits Access (`IN_ACCESS`, `IN_OPEN`, `IN_CLOSE_NOWRITE`) events whenever a file is **read** — not just written. The MDS compile step reads `.mds` source files, which triggers Access events for those same files. Without `is_content_event` filtering, the watcher ingests these, recompiles, reads again, emits more Access events, and loops at I/O speed (~3000/s). macOS FSEvents does NOT report reads, so this was a Linux-only regression invisible in local dev. `is_content_event` drops all `EventKind::Access(_)` variants conservatively.

- **macOS synthetic FSEvents**: on macOS, `notify` delivers synthetic file-modified events for every file in a newly-registered watch directory. Without the `last_written` content-dedup baseline, the watcher immediately recompiles all watched files on startup (producing spurious "Recompiled" lines and duplicate stdout writes). The baseline MUST be recorded after watcher registration and before the main loop processes any events.

- **Atomic-rename saves (editor save pattern)**: editors like vim and many others save files via rename (write to temp, rename to target). An inode-level file watch is orphaned after the rename. The fix is to watch parent directories, not file inodes. `dirs_to_watch` computes the set of unique parent directories to register.

- **macOS `/tmp` → `/private/tmp` symlink**: `notify` on macOS returns canonical paths (resolving `/tmp` to `/private/tmp`). `graph_key(p)` in dir mode canonicalizes all paths before graph lookups. The `event_is_relevant` function handles this for single-file mode. The `canonicalize_vars_path` helper canonicalizes the vars file path at startup.

- **Dir-mode `notify` event paths are not canonical** — must call `graph_key(p)` on every changed path before graph lookups and before `output_path_for`. `graph_key` handles the "just deleted" case by canonicalizing the parent + rejoining the filename.

- **Out-dir inside root self-pollutes** — when `--out-dir` / `mds.json output_dir` resolves to a path inside the watched root, `collect_mds_files` would include output `.md` files if they had a `.mds` extension, and write events would loop. This is prevented by passing `exclude_prefix = Some(out_dir)` to `collect_mds_files` and filtering events with `changed.retain(|p| !p.starts_with(od))`.

- **Output layout is BREAKING in dir mode** — `--out-dir` and `mds.json output_dir` now mirror the source subtree (`a/x.mds → out/a/x.md`). Old flat outputs (`out/x.md`) are orphaned; no auto-migration. `_`-prefixed files no longer emit their own `.md`.

- **`--format messages` is single-file only**: `--out-dir` in messages mode is silently dropped with a warning (not an error) for `mds build`. For `mds watch`, it is a hard startup error.

- **`parse_cli_value` rejects non-finite floats**: `NaN`, `Infinity`, `-Infinity` all parse as `f64` but fail `is_finite()` and fall through to `Value::String`. This is by design.

- **Linux inotify limit**: on Linux, large projects may exhaust `fs.inotify.max_user_watches`. The watcher startup code includes a hint in the error message pointing users to this system parameter.

- **`--debounce 0` in tests is not zero-latency**: even with `--debounce 0`, `drain_debounce` returns an empty set immediately (not a zero-duration window). Tests still need polling loops (`wait_for_file_contains`) because the OS delivers FS events asynchronously.

- **Compile errors during watch are non-fatal**: both single-file and directory modes print the error to stderr and continue watching. Error-settle (`settle_mtime`): the `(mtime,size)` snapshot is updated on error so the liveness probe gate doesn't re-fire on unchanged files. Errored files are retried only on a real change event, not on each tick.

- **First-tick reconcile closes the startup race window** — between `collect_mds_files` and `watcher.watch(root, Recursive)`, new files may be created. The `first_tick` recovery in the liveness probe collects files again and compiles any that appeared. Pre-loop seeding ensures the subsequent diff sees no change if nothing was actually added.

- **Edge-triggered recovery means a permanently-absent dir never recompiles** — if the entry's parent dir (file mode) or the watched root (dir mode) is deleted and never recreated, the liveness probe detects it as missing on the first tick and stays silent afterward. Recovery only fires when the dir reappears. This is intentional: per-tick error spam for a permanently-missing dir would make the tool unusable.

- **`armed_dirs` divergence from `watched_dirs`** — if `resync_watches` fails to register a new dir, that dir is in `watched_dirs` (desired) but NOT in `armed_dirs` (not actually armed). The liveness probe uses `armed_dirs` to decide when to call `watcher.watch()`, so the failed dir will be retried on the next tick.

- **`DirWatchState.known_set()` allocates** — `known_set()` allocates a `HashSet` from `known_files` on every call. At 500 files the cost is measurable but acceptable. For hot paths where you need the set multiple times per call, store the result locally (the AC-P5 test validates idle behavior at 500 files).

- **Test naming**: `watch_debounce_final_value_wins_after_rapid_edits` is the correct name for the final-value debounce test (renamed from `watch_debounce_coalesces_rapid_edits`). Coalescing is verified separately by the Recompiled-count test at line ~1195.

- **Two-window bounded-count test pattern**: error-count tests use `count_w1 == count_w2` (two consecutive observation windows must agree) instead of a magic `<= N` bound. This is scale-invariant: if the filesystem is slow on a particular CI run, both windows expand together and the test still passes.

## Key Files

- `crates/mds-cli/src/main.rs` — CLI surface: clap `Cli`/`Commands` structs, `run()` dispatch, `run_check`, `run_init`
- `crates/mds-cli/src/build.rs` — all shared compile helpers: output-path resolution, `mds.json` config, runtime vars, `compile_to_content`, `compile_and_write`, `settle_mtime`, exit code mapping
- `crates/mds-cli/src/watch.rs` — watch loop: `run_watch` dispatch, `run_watch_file`, `run_watch_dir`, `dir_watch_startup`; structs `FileCompileCtx`, `FileWatchState`, `DirWatchCtx`, `DirWatchState`, `LivenessState` (with `root_armed`, `armed_external_dirs`), `DirStartup`; extracted helpers `rebuild_file`, `liveness_probe_file`, `liveness_probe_dir`, `handle_fs_event_file`, `handle_fs_event_dir`, `compile_one_source`, `process_dir_batch`, `process_dir_batch_incremental`, `process_dir_batch_vars_changed`; pure helpers `dirs_to_watch`, `files_of_interest`, `event_is_relevant`, `collect_mds_files`, `output_path_for`, `canonicalize_vars_path`, `clear_terminal`, `resync_watches`, `drain_debounce`, `affected_sources`, `is_partial`, `graph_key`, `snapshot_state`, `state_differs`, `external_recovery_decision`, `is_content_event`, `recv_next`, `settle_mtime` (private), `stop_watching`, `clamp_poll_interval`
- `crates/mds-cli/tests/cli_watch.rs` — integration tests for `mds watch` (54 test cases covering all modes, edge cases, and QA regressions including Linux busy-loop regression, two-window bounded-error pattern, idle-500-files-no-recompile, cross-root partial rebuild, ghost-entry pruning, unwatch-pruned-dirs, and many more)
- `crates/mds-cli/Cargo.toml` — `notify = "8"`, `ctrlc = "3.5"`, `miette` with `fancy` feature

## Related

- **PF-004** (Active): file reads must not bypass the 10 MiB `MAX_FILE_SIZE` cap. `read_build_input` and `mds::compile_with_deps` are the two enforcement points. Any new input path added to the CLI MUST route through one of them. The partial/reconcile/cross-root paths all go through `compile_to_content` which calls one of these.
- **ADR-016** (Active): dynamically-resolved values must be re-validated at runtime. In the watch loop, `files_of_interest`, `dirs_to_watch`, and `forward_deps` are recomputed from fresh `compile_to_content` output after every rebuild — never carried forward from the previous cycle.
- **ADR-021** (Active): liveness-gated reconcile — cheap per-tick re-arm, full directory rescan only on watch-loss/recovery. Edge-triggered: a missing dir/root triggers reconcile only on vanish→reappear, never while it stays missing. Idle cost stays O(1) regardless of tree size. File mode uses `armed_dirs` for O(missing_dirs) idle cost; dir mode uses `root_armed` and `armed_external_dirs` for the same guarantee.
- **Project decision**: `notify 8` + `ctrlc 3.5` were selected with MSRV 1.88 (30-day version cooldown). `notify-debouncer-full` was deliberately NOT used; debounce is hand-rolled in `drain_debounce`.
- **Feature: mds-compiler** — the compiler API consumed by the CLI: `mds::compile_with_deps`, `mds::compile_messages_str_with_deps`, `mds::check_collecting_warnings`, `mds::load_vars_file`. The dependency tracking that drives watch resync comes from `compile_with_deps`'s returned `dependencies` field.
