---
feature: mds-cli
name: MDS CLI
description: "Use when adding new subcommands, changing output-path resolution logic, modifying the watch architecture (single-file or directory mode), adding new compile paths, updating mds.json config handling, debugging stdout/stderr stream separation, or investigating exit codes. Keywords: mds build, mds check, mds watch, mds init, OutputFormat, messages mode, run_build, run_watch, build.rs, watch.rs, mds.json, output_dir, resolve_output_path, resolve_output_base, OutputBase, output_path_for, compile_and_write, compile_to_content, debounce, notify, ctrlc, content-dedup, last_written, dirs_to_watch, files_of_interest, exit_code, MAX_FILE_SIZE, read_build_input, BuildArgs, WatchArgs, forward_deps, affected_sources, is_partial, graph_key, process_dir_batch, liveness_probe, snapshot_state, state_differs, rearm, poll_interval, is_content_event, inotify Access, Linux busy-loop, external_recovery_decision, edge-triggered recovery, missing_watched_dirs, missing_external_dirs, recv_next, settle_mtime."
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
updated: 2026-06-10
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
| `resolve_output_path` | Six-level precedence chain: `-o -` → `-o path` → stdin-default → `--out-dir` → `mds.json` → sibling `.md` |
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

Note: `resolve_output_path_no_create` was **removed** in the dir-mode refactor — dir-mode watch now uses `output_path_for(source, root, &output_base)` which is inherently pure (no dir creation).

### watch.rs — file watcher

The watch loop uses `notify 8` (non-recursive for single-file, recursive for directories) + `ctrlc 3.5`. Events and Ctrl+C are both sent over a single `mpsc::Sender<Msg>` where `Msg` is either `Msg::Fs(notify::Result<Event>)` or `Msg::Interrupt`. This design lets the main loop handle both interrupt and FS events in one receive call.

**`recv_next(rx, tick: Option<Duration>)`** — shared helper used by both loops. Returns:
- `Ok(Some(msg))` — a message arrived
- `Ok(None)` — idle tick (only when `tick` is `Some`)
- `Err(Disconnected)` — channel disconnected; caller should break

**`is_content_event(kind: &notify::EventKind) -> bool`** — filters `EventKind::Access(_)` events. On Linux, inotify emits `IN_ACCESS`/`IN_OPEN`/`IN_CLOSE_NOWRITE` events whenever a file is merely **read** (not written). The compile step reads `.mds` source files, which causes inotify to emit Access events for those same files. Without this filter the watcher ingests those events, recompiles, reads again, emits more Access events, and enters a busy-loop (~3000 recompiles/second). macOS FSEvents does not report reads, so this was a Linux-only bug. Both the event path and `drain_debounce` call `is_content_event` to drop Access variants before any path check.

**Single-file mode** (`run_watch_file`):
1. Load config + resolve output path once at startup.
2. Perform initial compile via `compile_and_write` (announces "Compiled to").
3. Register `notify` watchers on all **parent directories** (not file inodes — survives atomic-rename saves).
4. Record baseline content in `last_written` after watcher registration to suppress macOS synthetic FSEvents.
5. Pre-seed `last_mtimes` (mtime+size snapshot) for liveness probe state.
6. Pre-seed `missing_watched_dirs`: the set of desired watch dirs that don't exist yet at startup, so their first appearance is treated as a recovery edge rather than a per-tick walk.
7. Main loop: on each `Msg::Fs` event, check `is_content_event` first, then `event_is_relevant` against `files_of_interest`; if relevant, drain debounce window, then call `compile_to_content`, compare with `last_written`, write only if changed. On idle tick: run liveness probe.
8. After each successful rebuild, recompute `dirs_to_watch` and `files_of_interest` from **fresh** dependency output (ADR-016: never trust a stale dep set). Update `last_mtimes`.

**Directory mode** (`run_watch_dir`):
1. Load config once; compute `OutputBase` (`Dir(base)` for `--out-dir`/`mds.json`, `NextToSource` by default). Reject `..` in `mds.json output_dir` at startup.
2. Compile all `.mds` files under root with `collect_mds_files` (depth-bounded at 64, excludes out-dir subtree when it is inside root). Build `forward_deps`, `errored`, `known_files`, `external_dep_dirs`, `last_mtimes` during startup.
3. Register recursive watcher on root; NonRecursive watchers on external dep dirs + optional vars dir.
4. Record content-dedup baseline after watcher registration.
5. On events: drop `Access` events (`is_content_event`); canonicalize changed paths; accept `.mds` paths under root OR in external dep dirs. If vars file changed, recompile ALL files (full deps refresh + prune). Otherwise, call `process_dir_batch`.
6. `process_dir_batch`: compute `affected = affected_sources(forward_deps_snapshot, seeds)`, compile each affected source; refresh graph edges; partials (DD2) refresh edges but don't emit output; deletions remove output + graph entries.
7. Liveness probe (idle tick): re-arm root (Recursive) + external dirs + vars dir. On recovery (root reappeared, re-arm failed, first tick): run `collect_mds_files` diff → `process_dir_batch` for appeared/removed.

### Liveness Probe and Edge-Triggered Recovery

The liveness probe uses **edge-triggered recovery** in both single-file and directory modes (ADR-021 / AC-P1):

**File mode** (`missing_watched_dirs: BTreeSet<PathBuf>`):
- Desired watch dirs are evaluated per-tick using `external_recovery_decision(&missing_watched_dirs, &dir_statuses)`.
- `external_recovery_decision` returns `(recovery_needed, now_missing)`.
- Recovery fires ONLY when: (a) first tick, (b) a previously-missing dir reappears (vanish→reappear edge), or (c) an existing dir fails to re-arm (genuine watch loss).
- A dir that STAYS missing across ticks does NOT trigger recovery — avoids per-tick error spam when the entry's parent dir is permanently absent.
- `entry_was_missing && entry_now_exists` is a separate edge trigger for the entry file itself.

**Directory mode** (`LivenessState.missing_external_dirs: BTreeSet<PathBuf>`):
- Same `external_recovery_decision` function used for external dep dirs.
- Root recovery: `(root_now_exists && !root_ok)` — an existing root whose re-arm FAILED (genuine watch loss). A merely-missing root is handled by the `root_was_missing && root_now_exists` vanish→reappear edge. **NOT** `!root_ok` alone, which would fire on every tick while root stays missing.
- `liveness.root_was_missing = !root_now_exists` is updated each tick to track the transition.

### Dependency models

- **Single-file mode**: **forward deps** — recompute deps from each `compile_to_content` output; set of watched dirs and `files_of_interest` updated on each rebuild. Stale dep sets are never reused (ADR-016).
- **Directory mode**: **reverse-dep graph** — `forward_deps: HashMap<PathBuf, Vec<PathBuf>>` (canonical source → canonical transitive deps). On a change event, `affected_sources(forward_deps, seeds)` does DFS with a visited set (cycle-safe) to find all transitive importers. The graph is refreshed from fresh compilation output after each successful compile.

### Partials (DD2)

A `.mds` file whose name starts with `_` is a **partial**: it is tracked in the dependency graph and triggers rebuilds of its importers on edit, but it never emits its own `.md` output file. `is_partial(path)` tests the `_` prefix. Partials are graph nodes — they have entries in `forward_deps` and `known_files` — but the compile path skips `write_output` for them.

### Cross-root imports (DD3)

If a source file imports a `.mds` file outside the watched root, the parent directory of that external file is added to `external_dep_dirs` and watched NonRecursive. An event for an external `.mds` path is accepted as a seed into `affected_sources`. External files are **never** compiled to their own output (only in-root importers are emitted). External dep dirs are re-armed by the liveness probe.

### Output-path resolution

**File mode / `mds build`** — six-level chain in `resolve_output_path_impl` (unchanged):
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
- `Dir(d)`: `rel = source.strip_prefix(root)`; `d.join(rel).with_extension("md")`. If strip_prefix fails (source outside root — canonicalization edge case), falls back to `d.join(stem.md)` — **never joins an absolute path** (path-escape guard, AC-M7).
- `NextToSource`: `source.with_extension("md")`.

Output dirs are created on write by `write_output` (which calls `create_dir_all` on the parent).

`mds.json` is found by walking up from the input file. Its `build.output_dir` field is rejected if it contains `..` components (path traversal guard). `resolve_output_path_no_create` was **removed** — dir-mode deletion now uses `output_path_for` which is inherently pure (no dir creation).

## Self-Healing Watcher (ADR-021)

The outer loop uses `recv_next(rx, tick)` where `tick = Some(Duration)` when `poll_interval > 0` (default 1000ms; nonzero values clamped to ≥50ms). On each idle `Timeout` tick, the liveness probe runs:

1. **Re-arm**: idempotent `watcher.watch(path, mode)` on all desired paths. Collect per-path `(dir, exists, rearm_ok)` statuses.
2. **Recovery gate (edge-triggered)**: full reconcile runs only when `external_recovery_decision` returns `recovery_needed = true`. A dir that STAYS missing does NOT trigger recovery — only vanish→reappear or re-arm failure of an existing dir does.
3. **Single-file mode**: `state_differs` check over `files_of_interest` using `(mtime, size)` snapshots. Triggers rebuild if any file changed or recovery applies.
4. **Dir mode recovery**: `collect_mds_files` diff vs `known_files` → `process_dir_batch` for appeared/removed. Replaces `last_mtimes` from fresh snapshot.
5. **Pre-loop seeding**: `last_mtimes` and `missing_watched_dirs`/`missing_external_dirs` initialized before the loop, so the first tick detects no change and emits zero `Recompiled` lines (AC-W4).
6. **Error-settle** (`settle_mtime`): on compile error, the `(mtime,size)` snapshot is updated for the failed source so the tick gate doesn't re-fire on unchanged files. `errored` sources are retried only when a real change event arrives, not on each tick.

`poll_interval = 0` → blocking `rx.recv()`, no timeout arm, no liveness probe (native-only mode).

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
- File/build mode: rejected inside `resolve_output_path_impl`.
- Dir watch mode: rejected at startup inside `resolve_output_base`.

## Anti-Patterns

- **Bare `std::fs::read_to_string` + direct `mds::compile_str`** — bypasses the `MAX_FILE_SIZE` cap (PF-004). All reads must go through `read_build_input` or `mds::compile_with_deps`. This applies to ALL paths including partials, reconcile, and cross-root files.

- **Trusting stale dependency sets in the watch loop** — the dep list from the PREVIOUS rebuild must never be reused as-is for the next cycle. Always recompute from `compile_to_content` output (ADR-016). Using stale deps causes phantom watches on deleted imports or missed watches on newly added imports.

- **Writing compile output to stdout during the watch loop** — only the initial compile (`compile_and_write`) is allowed to write to stdout; subsequent rebuilds compare content and only call `write_output` if changed, with `announce=false` to suppress the duplicate "Compiled to" line. Removing the content-dedup check causes duplicate writes that corrupt downstream pipe consumers. The reverse-dep and reconcile paths must never write to stdout.

- **Calling `watcher.watch` recursively for single-file mode** — the watcher must use `RecursiveMode::NonRecursive` for each parent directory, not recursive on the entry's root. Recursive mode on a shared project root would generate massive event noise from unrelated files.

- **Adding a new compile path that uses `resolve_output_path_no_create`** — this function was removed. Dir-mode watch now uses `output_path_for(source, root, &output_base)` which is inherently pure (no dir creation). Dir creation happens in `write_output` via `create_dir_all`.

- **Using `--format messages` in directory watch mode** — rejected at startup. Multiple `.mds` files cannot map to a single JSON document. Always validate directory-mode constraints before entering the watch loop.

- **Per-tick full-tree walk** — O(tree) cost on every tick. The liveness probe is gated: cheap re-arm + stat only; full `collect_mds_files` only on recovery/first-tick (ADR-021 / DD1).

- **Not filtering Access events from the event path** — inotify on Linux emits Access events when `.mds` files are read during compilation. Without `is_content_event` filtering BOTH in the main event path and in `drain_debounce`, the watcher enters a busy-loop (thousands of recompiles/second on Linux).

- **Using `!root_ok` alone as the dir-mode root recovery condition** — this triggers recovery on every idle tick while the root dir is absent (per-tick error spam). The correct condition is `root_now_exists && !root_ok` for re-arm failure of an existing root. The vanish→reappear edge is handled separately by `root_was_missing && root_now_exists`.

- **Not seeding `missing_watched_dirs` / `missing_external_dirs` before the loop** — if a watched directory is already missing at startup and recovery is not edge-triggered, every tick fires a recovery until the directory reappears. Pre-seed these sets so the first tick has the correct baseline.

## Gotchas

- **Linux inotify Access events**: on Linux, inotify emits Access (`IN_ACCESS`, `IN_OPEN`, `IN_CLOSE_NOWRITE`) events whenever a file is **read** — not just written. The MDS compile step reads `.mds` source files, which triggers Access events for those same files. Without `is_content_event` filtering, the watcher ingests these, recompiles, reads again, emits more Access events, and loops at I/O speed (~3000/s). macOS FSEvents does NOT report reads, so this was a Linux-only regression invisible in local dev. `is_content_event` drops all `EventKind::Access(_)` variants conservatively.

- **macOS synthetic FSEvents**: on macOS, `notify` delivers synthetic file-modified events for every file in a newly-registered watch directory. Without the `last_written` content-dedup baseline, the watcher immediately recompiles all watched files on startup (producing spurious "Recompiled" lines and duplicate stdout writes). The baseline MUST be recorded after watcher registration and before the main loop processes any events. See QA-R1/R2/R3 tests.

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

## Key Files

- `crates/mds-cli/src/main.rs` — CLI surface: clap `Cli`/`Commands` structs, `run()` dispatch, `run_check`, `run_init`
- `crates/mds-cli/src/build.rs` — all shared compile helpers: output-path resolution, `mds.json` config, runtime vars, `compile_to_content`, `compile_and_write`, `settle_mtime`, exit code mapping
- `crates/mds-cli/src/watch.rs` — watch loop: `run_watch` dispatch, `run_watch_file`, `run_watch_dir`, `process_dir_batch`, `affected_sources`, `output_path_for`, `resolve_output_base`, `is_partial`, `graph_key`, `is_content_event`, `snapshot_state`, `state_differs`, `recv_next`, `external_recovery_decision`, debounce, content-dedup, dir collection
- `crates/mds-cli/tests/cli_watch.rs` — integration tests for `mds watch` (35+ test cases covering all modes, edge cases, and QA regressions including Linux busy-loop regression and bounded-errors-on-parent-dir-deleted)
- `crates/mds-cli/Cargo.toml` — `notify = "8"`, `ctrlc = "3.5"`, `miette` with `fancy` feature

## Related

- **PF-004** (Active): file reads must not bypass the 10 MiB `MAX_FILE_SIZE` cap. `read_build_input` and `mds::compile_with_deps` are the two enforcement points. Any new input path added to the CLI MUST route through one of them. The partial/reconcile/cross-root paths all go through `compile_to_content` which calls one of these.
- **ADR-016** (Active): dynamically-resolved values must be re-validated at runtime. In the watch loop, `files_of_interest`, `dirs_to_watch`, and `forward_deps` are recomputed from fresh `compile_to_content` output after every rebuild — never carried forward from the previous cycle.
- **ADR-021** (Active): liveness-gated reconcile — cheap per-tick re-arm, full directory rescan only on watch-loss/recovery. Edge-triggered: a missing dir/root triggers reconcile only on vanish→reappear, never while it stays missing. Idle cost stays O(1) regardless of tree size.
- **Project decision**: `notify 8` + `ctrlc 3.5` were selected with MSRV 1.88 (30-day version cooldown). `notify-debouncer-full` was deliberately NOT used; debounce is hand-rolled in `drain_debounce`.
- **Feature: mds-compiler** — the compiler API consumed by the CLI: `mds::compile_with_deps`, `mds::compile_messages_str_with_deps`, `mds::check_collecting_warnings`, `mds::load_vars_file`. The dependency tracking that drives watch resync comes from `compile_with_deps`'s returned `dependencies` field.
