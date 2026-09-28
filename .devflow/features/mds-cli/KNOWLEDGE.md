---
feature: mds-cli
name: MDS CLI (mds-cli)
description: "Use when adding new subcommands, changing output-path resolution logic, modifying the watch architecture, adding new compile paths, updating mds.json config handling, debugging stdout/stderr stream separation, investigating exit codes, adding directory-mode build/check support, working on stale-output cleanup, or on the CLI's input refusals (forbidden path characters, directory-argument validation, or the entry-overwrite refusal). Keywords: mds build, mds check, mds watch, mds init, OutputKind, run_build, run_watch, build.rs, output.rs, watch.rs, input.rs, mds.json, output_dir, resolve_output_base, OutputBase, output_path_for, compile_and_write, compile_to_content, intrinsic extension, run_build_directory, run_check_directory, is_partial, collect_mds_files, probe_and_remove_stale, canonicalize_out_dir, output_base_no_ext, continue-on-error, subtree mirror, symlink guard, 10 MiB cap, load_config, reject_forbidden_output_path, reject_forbidden_resolved_output_path, resolve_directory_argument, ensure_existing_mds_file, forbidden path characters, #265, #413, #417, #425, #428, read_canonical_source, read_stdin, read_at_most, safe_path, safe_file_display, escape_path_for_message, display_native_path, #409, mds init filename, admit_output, refuse_output_over_entry, file_identity, EntryPaths, WatchedPath, Watched, WorkingDir, CompileWriteOutcome, has_sidecar_head, verify_then_delete_map."
category: component-patterns
directories: ["crates/mds-cli/"]
referencedFiles:
  - crates/mds-cli/src/main.rs
  - crates/mds-cli/src/input.rs
  - crates/mds-cli/src/build.rs
  - crates/mds-cli/src/output.rs
  - crates/mds-cli/src/watch.rs
  - crates/mds-cli/tests/cli_build.rs
  - crates/mds-cli/tests/dir_build.rs
  - crates/mds-cli/tests/forbidden_paths.rs
  - crates/mds-cli/tests/intrinsic_output.rs
  - crates/mds-cli/tests/cli_watch.rs
  - crates/mds-cli/Cargo.toml
created: 2026-06-26
updated: 2026-09-28
---

# MDS CLI (mds-cli)

## Overview

`crates/mds-cli/` implements the `mds` binary with six subcommands: `build`, `check`, `fmt`, `lint`, `watch`, and `init`. The CLI delegates all compilation to `mds-core`; its job is input resolution, output routing, config loading, and process lifecycle. The output extension is derived from the compiled result's kind — there is no `--format` flag. Markdown templates produce `.md` files; messages templates produce `.json` files.

The CLI supports both single-file and directory modes for `build`, `check`, `fmt`, `lint`, and `watch`. Directory mode recursively compiles/processes all non-partial `.mds` files under a root, mirrors the subtree into an optional `--out-dir`, and (for `build`/`check`) continues on error with a final summary.

Two cross-cutting refusal systems now live at the front of every code path, both keyed on comparing **canonical forms**, never raw text (#408-style comparisons):
- **#413** — one shared directory-argument resolver (`input::resolve_directory_argument`) used by all five directory-mode subcommands, replacing five ad hoc per-subcommand checks.
- **#425** — one shared entry-overwrite refusal (`build::admit_output` / `refuse_output_over_entry` / `file_identity`) used by every route that could write a compiled entry's output over the entry file itself, in both `build` and `watch`.

## Core Responsibilities

- Subcommand dispatch: `main.rs` parses args (clap) and calls `run_build` (build.rs), `run_check` (defined in `main.rs`, not build.rs), `run_watch` (watch.rs); `run_init` (main.rs) handles `mds init`
- `input.rs`: the one directory-argument resolver shared by `build`/`check`/`fmt`/`lint`/`watch` (#413)
- `build.rs`: single-file and directory build logic, output-path resolution, the entry-overwrite refusal (#425), shared helpers (`compile_to_content`, `load_config`, `ensure_existing_mds_file`)
- `output.rs`: directory-mode shared machinery (`OutputBase`, `output_path_for`, `collect_mds_files`, `is_partial`, `probe_and_remove_stale`, `canonicalize_out_dir`), forbidden-path-character refusals, atomic writes, display/escape helpers
- `watch.rs`: file/directory watch loop; `WatchedPath`/`Watched` own the typed-vs-canonical duality for every watched path; `compile_and_write`/`CompileWriteOutcome` (moved here from build.rs) own the single-file startup route
- Does NOT: implement compiler logic, manage modules, or handle imports

## Standard Structure

### input.rs — `resolve_directory_argument` (#413)

The one directory-argument check `build`, `check`, `fmt`, `lint`, and `watch` (directory mode) all make, replacing what used to be four separate `symlink_metadata` checks (which a trailing `link/` walks straight past) plus `watch`'s own `NativeFs::check_symlink` call (which cannot take a path with no final name, so `mds watch .` used to fail with `file not found: .`).

```rust
// In input.rs
pub(crate) fn resolve_directory_argument(typed: &Path) -> Result<PathBuf, MdsError> {
    let canonical = mds::NativeFs::check_directory(typed).map_err(|e| match e {
        MdsError::ImportError { .. } => refusal(typed, "must not be a symlink"),
        other => other,
    })?;
    if is_filesystem_root(&canonical) {
        return Err(refusal(typed, "must not be the filesystem root"));
    }
    Ok(canonical)
}
```

Behavior, all `mds::io`, exit 2, naming the directory **as typed**, escaped:
- `mds::NativeFs::check_directory` takes `typed` with or without a final path component (`src`, `src/`, `.`, `..`, `link/.`, `sub/..`) and returns the canonical form. It refuses a forbidden path character (#265) as typed or in the canonical form with one shared message.
- A symlinked **final component** is refused as `directory argument must not be a symlink: "<typed>"`. `link/..` names no link (it is the directory above the link's target on Unix), so it is **accepted**.
- The filesystem root, however spelled (`/`, `/..`, `.` in a cwd of `/`), is refused as `directory argument must not be the filesystem root: "<typed>"` — a root stays usable as a project root or cwd (#371); only *walking* one is refused.

`mds watch .` (and `./`, `..`, `sub/..`) now works — this was a `main`-only bug (not a #265 regression) fixed alongside #413; the old KB's "cannot start" gotcha no longer applies.

### build.rs — the #425 entry-overwrite refusal

A `.md` entry that declares `type: mds` compiles to Markdown, and its **default output name is its own name** (stem + `.md`). Before #425, `mds build page.md` (or `-o`/`--out-dir`/`mds.json build.output_dir` naming the entry another way) silently replaced the source with its compiled form, and the next build then failed because the compiled text no longer declares `type: mds`.

```rust
// In build.rs — the entry in the two forms admit_output takes, always
// (typed, canonical) order so the two cannot be passed swapped.
pub(crate) struct EntryPaths<'a> {
    pub(crate) typed: &'a Path,     // names the entry in every refusal
    pub(crate) canonical: &'a Path, // compared with the output, canonical-with-canonical
}

// Admit output_path as the destination of the compiled entry: refuse it when it
// IS the entry (refuse_output_over_entry), then warn on -o extension mismatch.
pub(crate) fn admit_output(
    output_path: Option<&Path>,
    entry: EntryPaths<'_>,
    output_arg: &Option<String>,
    kind: OutputKind,
    quiet: bool,
) -> Result<(), MdsError> {
    refuse_output_over_entry(output_path, entry)?;
    warn_output_extension_mismatch(output_arg, kind, quiet);
    Ok(())
}
```

`refuse_output_over_entry` compares `output` and `entry.canonical` through `file_identity`, **canonical with canonical, never text** — so `-o newdir/../page.md` (a directory that does not exist yet, walked back out of with `..`), a case variant on a case-insensitive volume, or a symlinked directory leading back to the entry are all caught. `file_identity(path)`:

1. Resolves `path`'s directory the way `write_output`'s `create_dir_all` will leave it — `resolve_dir_as_created`, which canonicalizes an existing directory (following symlinks) and, for a directory that does not exist yet, walks it component-by-component the way the write would create it (a `..` after a not-yet-created component pops back to its already-resolved parent, never above the root).
2. Looks the file's **name** up in that resolved directory — so on a case-insensitive volume, `PAGE.md` and `page.md` canonicalize to the same file (the "case-variant lookup" the C18 note refers to), whatever directory-form reached it (`newdir/../PAGE.md` included).
3. A symlinked or not-yet-existing name is kept as-is (the symlink itself is the directory entry a write would replace, not its target).

Every route to a compiled entry's output goes through `admit_output`: `mds build` file mode (`EntryPaths { typed: &input, canonical: &input }` — build only ever holds the typed path, and `file_identity` canonicalizes it itself), and every `mds watch` file-mode route (startup compile, the startup Markdown fallback after a failed compile, and every rebuild). **Directory mode is unaffected** — it only compiles `.mds` files, whose outputs are `.md`/`.json`, never the same name as the source.

### watch.rs — `WatchedPath`/`Watched` (typed-vs-canonical duality, #417/#413)

Replaces the older `WatchRoot`/`WatchEntry`/`compile_entry` split with one type used for the file-mode entry, the directory-mode root, and every source below that root:

```rust
enum Watched { Entry, Root, Source }   // decides how a refusal names the path

struct WatchedPath {
    typed: PathBuf,     // as the user reaches it; every compile and mds.json lookup uses this
    canonical: PathBuf, // what notify reports events under; every identity check uses this
    what: Watched,
}
```

- `ensure_unmoved()` re-resolves `typed` before every compile and compares its **parent** with `canonical`'s parent (or, for the root, the whole canonical form) — never text-compares the two paths (#408). A retarget (a symlink on the typed path swapped, or a directory replaced) is refused as `watched {entry|directory|file} now resolves to a different {file|directory}: "<typed>"; restart mds watch to follow it`. This is non-fatal for a directory-mode source: the compile errors and `mds watch` keeps running (`state.errored` marks it, retried on the next real change).
- `compile()` calls `ensure_unmoved()` first, then compiles by `typed` (so errors name the file the user reaches, never its canonical absolute path).
- `walked(src)` turns a canonical path below the root into its typed form (`root.typed.join(src.strip_prefix(&root.canonical))`) — an out-of-root dependency has no walked form and keeps its canonical path.
- `compile_source(src, ...)` is every directory-mode compile's entry point: an out-of-root path compiles directly (no `WatchedPath` wrapping — DD3); an in-root path re-checks the root itself is unmoved, then builds a `Watched::Source` `WatchedPath` and calls `compile()`.

### watch.rs — `compile_and_write`/`CompileWriteOutcome` (moved from build.rs)

```rust
enum CompileWriteOutcome {
    Written(WrittenEntry),      // (Option<PathBuf>, deps, content) — routed and written
    Failed(miette::Report),     // compile or write failure; mds watch reports and keeps watching
}

fn compile_and_write(
    entry: &WatchedPath, output: &Option<String>, out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>, runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<CompileWriteOutcome>
```

Compile-then-route: compile first (kind is unknown until then), derive the output path from `kind`, call `admit_output` (the #425 refusal), then write. The **function's own `Err`** (not `CompileWriteOutcome::Failed`) is an output route no rebuild can use — a route that fails to resolve (`mds.json build.output_dir` with `..`) or an output that is the entry itself — and it is fatal at startup (exit 2, nothing written, no stdout fallback). A *compile* or *write* failure is `Failed`, reported and non-fatal: `mds watch` keeps watching through it.

`FileCompileCtx.output_path` is resolved **once at startup** and reused by every rebuild (`None` means stdout, `-o -`, the only way a rebuild writes to stdout). There is no more per-rebuild re-derivation (`FileCompileCtx::rebuild_output_path` — which used to `.unwrap_or(None)` a route failure into a silent stdout fallback — is gone): `rebuild_file` calls `admit_output` again on every rebuild (the entry-overwrite check must be re-run since a route resolved once could later point at the entry after a failed startup compile left the Markdown default in place), but a **route** failure can no longer happen post-startup because the route itself never changes after startup.

### watch.rs — `WorkingDir` (recreated-cwd restore)

```rust
struct WorkingDir { canonical: Option<PathBuf> }
impl WorkingDir {
    fn record() -> Self { /* snapshot cwd at startup */ }
    fn restore_if_recreated(&self) { /* called FIRST by every rebuild */ }
}
```

When the working directory `mds watch` started in is deleted and recreated at the same canonical path (e.g. a `git checkout` that removes and restores it), `restore_if_recreated` `set_current_dir`s back into it before the rebuild reads anything typed relative to it (a relative `--vars`, `-o`, or the directory argument). Comparison is canonical-with-canonical; a directory swapped in between the check and the move is the same check-then-open window every path-based read has, left to the compile's own `ensure_unmoved`/existence checks.

### Directory-mode retarget and route refusals

- **Per-source retarget** (directory mode): `compile_source` → `ensure_unmoved` returning `Err` is caught by `compile_one_source`'s `Err` arm — `eprint_error` + `state.record_error(src)` — non-fatal, that one source is skipped until it changes again.
- **Startup output-route refusal, no stdout fallback**: `dir_watch_startup` calls `resolve_output_base(...)?` before the walk or the first compile — an unresolvable route (`mds.json build.output_dir` with `..`) ends `mds watch <dir>` at startup, exit 2, nothing compiled. Directory mode's output base is resolved once and never re-derived, so (unlike the pre-#425 file-mode bug) there was never a stdout-fallback path to close here.

### Resource limits and bounded reads

- `MAX_FILE_SIZE` (10 MiB): enforced by `mds-core` for file inputs; enforced in `read_stdin`/`read_stdin_from` for stdin via `mds::read_at_most(reader, MAX_FILE_SIZE + 1, 0)` — reads at most one byte past the cap and never grows its buffer further (#428), so an oversized or still-growing stdin stream is never held in memory whole.
- `MAX_CONFIG_SIZE` (1 MiB): `load_config` opens `mds.json`, reads its `metadata().len()` up front, and if that already exceeds the cap refuses before reading any bytes; otherwise it reads via `mds::read_at_most(&mut file, MAX_CONFIG_SIZE + 1, size)` — same one-byte-over, no-regrow contract (#428).
- `has_sidecar_head`/`verify_then_delete_map` (stale **source-map** sidecar cleanup, not to be confused with `probe_and_remove_stale`'s wrong-extension-sibling cleanup): before deleting an old `.map` file, reads only its first N bytes (the exact length of `{"version":3,"file":<name>,`) via `mds::read_at_most(reader, len, len)` and compares — never opens or reads the whole file, and never opens a non-regular file at all (a FIFO with no writer would block).
- `MAX_TRAVERSAL_DEPTH`: `mds.json` upward walk cap (from `mds-core`)
- `MAX_DEPTH = 64`: hardcoded local cap in `run_build_directory` for directory recursion

## Dependency Patterns

The CLI uses `mds::compile_with_deps`/`compile_str_with_deps` (never bare `std::fs::read_to_string`). All file reads go through `mds-core`'s resolver, which enforces `MAX_FILE_SIZE` and the symlink guard.

## Error Handling

Exit codes:
- 0: success
- 1: compile/logic error, or `fail_count > 0` in directory mode (`std::process::exit(1)`) whatever the failing file's error
- 2: I/O or filesystem error (`MdsError::Io`, `FileNotFound`, `NotMdsFile`) — including (#265) an `-o`/`--out-dir`/`mds.json build.output_dir`/`mds init <filename>` value carrying a forbidden path character (typed or resolved form), a `build.output_dir` with a `..` component, a directory argument that is a symlink or the filesystem root (#413), and the #425 entry-overwrite refusal
- 3: resource limit exceeded (`MdsError::ResourceLimit`)

`MdsError::NotMdsFile { path }` now holds the path **as the caller typed it**, escaped (#417) — it used to hold the resolved canonical path. `run_build_directory` calls `std::process::exit(1)` directly (not via the `Result` chain) when `fail_count > 0`, matching other CLI exit points in `build.rs`.

## Anti-Patterns

- **Using `--format` flag** — deleted; does not exist in the CLI anymore. `intrinsic_output.rs` asserts this.
- **Calling `run_build_messages`/`run_build_markdown`/`read_build_input`/`reject_directory_input`** — all deleted.
- **Using 3-arg `output_path_for`** — the canonical signature is 4-arg with `ext: &str`. Never hardcode an extension.
- **Using `-o` with a directory input** — rejected; use `--out-dir`.
- **Re-implementing a per-subcommand symlink/root check on a directory argument** — always call `input::resolve_directory_argument` instead; a hand-rolled `symlink_metadata` check walks straight past a trailing `link/` and does not know the filesystem-root rule.
- **Text-comparing a typed path with a canonical one** — every identity check in this crate (`WatchedPath::ensure_unmoved`, `refuse_output_over_entry`/`file_identity`) canonicalizes both sides first (#408); a raw string/PartialEq comparison of `typed` against `canonical` is always wrong on a case-insensitive volume or through a symlink.
- **Adding a rebuild-side re-derivation of the output route** — `FileCompileCtx::rebuild_output_path` was deleted because its `.unwrap_or(None)` swallowed a route failure into a silent stdout fallback (#425); the route is resolved once at startup and reused.

## Gotchas

- `probe_and_remove_stale` is fire-and-forget (soft-warn on failure, never propagates). A stale sibling is an annoyance, not a correctness bug.
- `canonicalize_out_dir` resolves relative paths against `current_dir` and then canonicalizes; must run BEFORE `resolve_output_base` so later `starts_with` checks are reliable.
- Watch mode derives the extension from `compiled.kind.extension()` after each compile. On deletion (kind unknown) it probes both `.md` and `.json`.
- The `CompileOutput` struct in `build.rs` (content + kind + deps) is a local CLI struct — not the same as `mds::CompiledOutput` (the Rust enum from mds-core).
- `mds.json build.output_dir` rejects `..` components (`mds::io`, exit 2) in both single-file and directory mode, checked in both `load_config` (forbidden characters) and `resolve_output_path_for_kind`/`resolve_output_base` (traversal).
- Forbidden path characters (#265) are refused UP FRONT: `-o`/`--output`/`--out-dir` via `build::reject_forbidden_output_flags`, `mds.json build.output_dir` inside `load_config`, `mds init <filename>` in `main.rs`, and a single-file directory-mode argument via `input::resolve_directory_argument` — all via `output::reject_forbidden_output_path`, a thin CLI wrapper over `mds::reject_forbidden_path` (the scan and message now live in mds-core, not duplicated in the CLI). `-o`/`--out-dir`/`build.output_dir` additionally each run `output::reject_forbidden_resolved_output_path`, which canonicalizes the deepest EXISTING ancestor of the value (`resolve_existing_prefix`) and scans the whole resolved form — a symlink, or a hostile-named working directory above a relative value, is refused before anything is created. The message always names the value **as typed**, never the absolute resolved path.
- `build::ensure_existing_mds_file` — the single-file argument check that runs the typed forbidden-character scan, then `try_exists()`, then the `.mds`-extension check — is called ONLY by `fmt.rs` and `lint.rs`. It does **not** serve `build`/`check`/`watch`'s single-file argument: those rely on `mds-core`'s own resolver checks inside `compile_with_deps`/`compile_str_with_deps` (which raise `FileNotFound`/`NotMdsFile` themselves), and on `input::resolve_directory_argument` in directory mode. (Superseding the old KB's claim that `ensure_existing_mds_file` served all four subcommands.)
- `mds check` never loads `mds.json` (a malformed or hostile config does not fail it). `fmt` loads it only in directory mode.
- `watch::canonicalize_vars_path` runs the typed forbidden-path-character check on `--vars` FIRST, before the `exists()` probe — refused whether or not the vars file exists, and before its directory is ever watched.
- `build::load_config` names `mds.json` in every error by the path `start` REACHES it by (`./mds.json`, `sub/../mds.json`, one `..` per upward step) — never the canonical absolute path — since the config loads before the input is validated and the canonical form could leak a forbidden character from a hostile-named ancestor directory.
- `build::read_stdin` returns only the source string (`Result<String>`), no longer `(String, PathBuf)` — callers pass `None` as the base directory (core anchors `None` at cwd); `lint`'s stdin path passes `Path::new(".")` explicitly instead. A refusal of the working directory shows `"."`, never the absolute cwd.
- Status-line and error-message paths go through `output::safe_path`/`safe_file_display`, which apply `mds::display_native_path` (strips a lossless Windows `\\?\` prefix, #409) then `mds::escape_path_for_message` (WIRE plus `\t`). `output::safe_inline` (general single-line-value escaper) stays WIRE-only, no TAB addition.
- Debounce is a quiet period, not a fixed window (#379, `watch.rs`): the first relevant content event opens a `--debounce` window and every further content event restarts it; bounded by `debounce_cap = max(10 × window, 1s)`, `--debounce` clamped to `MAX_DEBOUNCE_MS = 60_000`, plus `MAX_DEBOUNCE_MESSAGES = 10_000`.
- Every write funnels through `atomic_write_file` (`output.rs`, #227): temp-file + rename, refusing a symlink at the target. `Durability::Fsync` for source rewrites (`fmt`, `lint --fix`); `Durability::RenameOnly` for reproducible derived artifacts (`build`/`watch`/`init`, #386). `tests/write_funnel.rs` is a lexical guard against a new raw `fs::write`/`File::create` site. An empty directory, or one whose only `.mds` files are partials, is a hard failure (not silent success) for `build`/`check`/`fmt`/`lint` (#204, #387).
- `mds build`'s single-file path and `mds watch` file mode both hold only the TYPED path as their entry — `file_identity` (inside `admit_output`) canonicalizes it itself when comparing with the output. Directory mode never needs this check: sources are `.mds`, outputs are `.md`/`.json`, so they can never collide.

## Key Files

- `crates/mds-cli/src/main.rs` — clap argument parsing; `run_check`/`run_check_directory`/`run_init` defined here; dispatches to `build::run_build` and `watch::run_watch`; `mod input`, `mod output`
- `crates/mds-cli/src/input.rs` — `resolve_directory_argument` (#413), the one directory-argument resolver for `build`/`check`/`fmt`/`lint`/`watch`
- `crates/mds-cli/src/build.rs` — `OutputKind`, `compile_to_content`, `run_build`, `run_build_directory`, `load_config`, `ensure_existing_mds_file` (fmt/lint only), `read_stdin`/`read_stdin_from` (bounded, source-only), `EntryPaths`/`file_identity`/`resolve_dir_as_created`/`refuse_output_over_entry`/`admit_output` (#425), `has_sidecar_head`/`verify_then_delete_map` (bounded stale-sourcemap cleanup), all output-path helpers for single-file mode
- `crates/mds-cli/src/output.rs` — `OutputBase`, `resolve_output_base`, `output_path_for`, `collect_mds_files`, `is_partial`, `probe_and_remove_stale`, `canonicalize_out_dir`, `output_base_no_ext`, `reject_forbidden_output_path`/`reject_forbidden_resolved_output_path` (thin wrappers over `mds::reject_forbidden_path`), `resolve_existing_prefix`, `safe_path`/`safe_file_display`/`safe_inline`, `atomic_write_file`
- `crates/mds-cli/src/watch.rs` — `WatchedPath`/`Watched` (typed-vs-canonical duality), `WorkingDir`, `compile_and_write`/`CompileWriteOutcome`, `FileCompileCtx`, `DirWatchState` (`forget`/`forget_graph`), `compile_one_source`, `dir_watch_startup`, `rebuild_dir_batch`/`process_dir_batch_incremental`
- `crates/mds-cli/tests/forbidden_paths.rs` — #265 walker matrix (build/check/fmt/lint/watch), single-file-argument refusals, output-location (typed and resolved) and `mds init` refusals
- `crates/mds-cli/tests/dir_build.rs` — directory-mode integration tests
- `crates/mds-cli/tests/cli_watch.rs` — watch integration tests, including the #425 startup-route-refusal tests (`watch_refuses_at_startup_an_output_route_that_fails`, `watch_startup_route_refusal_controls`)
- `crates/mds-cli/tests/intrinsic_output.rs` — tests asserting `--format` is rejected

## Related

- Feature: mds-compiler — provides `mds::CompileResult`, `mds::CompiledOutput`, `mds::compile_with_deps`, `mds::NativeFs::check_directory`, `mds::reject_forbidden_path`, `mds::read_at_most`
- Feature: mds-napi — parallel consumer of `CompileResult`; uses the same `kind` discriminant
- Feature: bundler-plugins — parallel consumer of the kind-based branch for bundler emitted modules
