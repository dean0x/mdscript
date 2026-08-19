---
feature: mds-cli
name: MDS CLI (mds-cli)
description: "Use when adding new subcommands, changing output-path resolution logic, modifying the watch architecture, adding new compile paths, updating mds.json config handling, debugging stdout/stderr stream separation, investigating exit codes, adding directory-mode build/check support, or working on stale-output cleanup. Keywords: mds build, mds check, mds watch, mds init, OutputKind, run_build, run_watch, build.rs, output.rs, watch.rs, mds.json, output_dir, resolve_output_base, OutputBase, output_path_for, compile_and_write, compile_to_content, intrinsic extension, run_build_directory, run_check_directory, is_partial, collect_mds_files, probe_and_remove_stale, canonicalize_out_dir, output_base_no_ext, continue-on-error, subtree mirror, symlink guard, 10 MiB cap."
category: component-patterns
directories: ["crates/mds-cli/"]
referencedFiles:
  - crates/mds-cli/src/main.rs
  - crates/mds-cli/src/build.rs
  - crates/mds-cli/src/output.rs
  - crates/mds-cli/src/watch.rs
  - crates/mds-cli/tests/cli_build.rs
  - crates/mds-cli/tests/dir_build.rs
  - crates/mds-cli/tests/intrinsic_output.rs
  - crates/mds-cli/Cargo.toml
created: 2026-06-26
updated: 2026-06-26
---

# MDS CLI (mds-cli)

## Overview

`crates/mds-cli/` implements the `mds` binary with four subcommands: `build`, `check`, `watch`, and `init`. The CLI delegates all compilation to `mds-core`; its job is input resolution, output routing, config loading, and process lifecycle. After the intrinsic-output refactor, **the output extension is derived from the compiled result's kind** — there is no `--format` flag. Markdown templates produce `.md` files; messages templates produce `.json` files.

The CLI now supports both single-file and directory modes for `build` and `check`. Directory mode (`mds build <dir>` / `mds check <dir>`) recursively compiles all non-partial `.mds` files under the given root, mirrors the subtree into an optional `--out-dir`, and continues on error with a final summary.

## Core Responsibilities

- Subcommand dispatch: `main.rs` parses args (clap) and calls `run_build`, `run_check`, `run_watch`
- `build.rs`: single-file and directory build logic, output-path resolution, shared helpers (`compile_to_content`, `compile_and_write`)
- `output.rs`: directory-mode shared machinery (`OutputBase`, `output_path_for`, `collect_mds_files`, `is_partial`, `probe_and_remove_stale`, `canonicalize_out_dir`)
- `watch.rs`: file/directory watch loop; derives extension from `kind.extension()` after compile
- Does NOT: implement compiler logic, manage modules, or handle imports

## Standard Structure

### OutputKind — intrinsic extension derivation

```rust
// In build.rs
pub(crate) enum OutputKind { Markdown, Messages }

impl OutputKind {
    pub(crate) fn extension(self) -> &'static str { "md" | "json" }
}

impl From<&CompiledOutput> for OutputKind { ... }
```

All callers derive the extension via `OutputKind::from(&compiled.output).extension()` — never hardcode `"md"` for a possibly-messages template.

### compile_to_content — pure compile without write

```rust
pub(crate) fn compile_to_content(
    input: &Path,
    runtime_vars: Option<HashMap<String, mds::Value>>,
    quiet: bool,
) -> Result<CompileOutput>
```

Returns `CompileOutput { content: String, kind: OutputKind, dependencies: Vec<String> }`. The `content` is already serialized: markdown as-is, messages as pretty-printed JSON array with trailing newline. Used by the watch loop for content-based dedup and by `run_build_directory`.

### compile_and_write — single-file compile+route+write

```rust
pub(crate) fn compile_and_write(
    input: &Path, output: &Option<String>, out_dir: &Option<PathBuf>,
    config: &Option<(MdsConfig, PathBuf)>,
    runtime_vars: Option<HashMap<String, mds::Value>>, quiet: bool,
) -> Result<(Option<PathBuf>, Vec<String>)>
```

This is "compile-then-route": compiles first, derives the output path from `kind`, then writes. The output path is unknown until after compilation for the intrinsic extension case.

### Output path precedence (single-file mode)

1. `-o -` → stdout
2. `-o <path>` → verbatim path (warns on extension mismatch if `kind` disagrees — AC-FUNC-11)
3. Stdin with no `-o`/`--out-dir` → stdout
4. `--out-dir <dir>` → `<dir>/<stem>.<ext>` where `<ext>` is from `kind`
5. `mds.json build.output_dir` → `<config_dir>/<output_dir>/<stem>.<ext>`
6. Default → source directory + `<stem>.<ext>`

`-o` rejected in directory mode. Use `--out-dir` for directories.

### output.rs — directory-mode shared machinery

```rust
// OutputBase describes where directory-mode output files land
pub(crate) enum OutputBase {
    Dir(PathBuf),     // subtree mirror under this dir
    NextToSource,     // next to source file
}

// Compute output base (precedence: --out-dir > mds.json > next-to-source)
pub(crate) fn resolve_output_base(abs_out_dir, config) -> Result<OutputBase>

// Canonicalize --out-dir before resolve_output_base
pub(crate) fn canonicalize_out_dir(out_dir: Option<&PathBuf>) -> Option<PathBuf>

// 4-arg canonical output path computation (always pass kind.extension() as ext)
pub(crate) fn output_path_for(source: &Path, root: &Path, base: &OutputBase, ext: &str) -> PathBuf

// Recursively collect .mds files (skips symlinks and exclude_prefix subtree)
pub(crate) fn collect_mds_files(root, max_depth, exclude_prefix) -> Vec<PathBuf>

// True if filename starts with '_' (partial convention — skipped in build)
pub(crate) fn is_partial(path: &Path) -> bool

// Remove the wrong-extension sibling after a write (fire-and-forget)
pub(crate) fn probe_and_remove_stale(base_no_ext: &Path, kind: OutputKind)

// Build the extension-free path for probe_and_remove_stale
pub(crate) fn output_base_no_ext(source, root, base) -> PathBuf
```

### Directory mode patterns

**Continue-on-error** — all non-partial files are attempted; compile errors are printed to stderr; counters track `ok_count`/`fail_count`; non-zero exit when `fail_count > 0`:

```rust
for file in &files {
    if is_partial(file) { continue; }
    match compile_to_content(file, runtime_vars.clone(), quiet) {
        Ok(compiled) => { /* write + stale cleanup */ ok_count += 1; }
        Err(e) => { eprintln!("{e:?}"); fail_count += 1; }
    }
}
eprintln!("{ok_count} built, {fail_count} failed");
if fail_count > 0 { std::process::exit(1); }
```

**Stale cleanup** — after each successful write, probe and remove the wrong-extension sibling:

```rust
let base_no_ext = output_base_no_ext(file, dir, &output_base);
probe_and_remove_stale(&base_no_ext, compiled.kind);
```

**Deletion in watch mode** — when a source `.mds` is deleted, kind is unknown; probe BOTH extensions:

```rust
let base = output_base_no_ext(&src, root, &output_base);
for ext in ["md", "json"] {
    let p = base.with_extension(ext);
    if p.exists() { let _ = fs::remove_file(&p); }
}
```

**Symlink guard** — directory root must not be a symlink (checked BEFORE `collect_mds_files`):

```rust
if input.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
    return Err(miette::miette!("directory argument must not be a symlink: {}", input.display()));
}
```

`collect_mds_files` additionally skips symlinked files AND symlinked directories inside the tree (build parity with single-file mode).

**Exclude output dir from collection** — when `out_dir` is nested inside the source root, pass it as `exclude_prefix` to `collect_mds_files`:

```rust
let exclude_prefix = match &output_base {
    OutputBase::Dir(d) if d.starts_with(dir) => Some(d.clone()),
    _ => None,
};
```

**AC-M7 path-escape guard** — `output_path_for` has a runtime containment check: if the computed output path somehow escapes `Dir(base)` (e.g. via a malformed strip_prefix result), it falls back to `base/<stem>.<ext>`. A `debug_assert!(false, ...)` fires in debug builds so tests catch regressions.

### Resource limits

- `MAX_FILE_SIZE` (10 MiB): enforced by `mds-core` for file inputs; enforced in `read_stdin()` for stdin
- `MAX_TRAVERSAL_DEPTH`: `mds.json` upward walk cap (from `mds-core`)
- `MAX_DEPTH = 64`: hardcoded local cap in `run_build_directory` for directory recursion

## Dependency Patterns

The CLI uses `mds::compile_with_deps` and `mds::compile_str_with_deps` (never bare `std::fs::read_to_string`). All file reads go through `mds-core`'s resolver, which enforces `MAX_FILE_SIZE` and the symlink guard.

## Error Handling

Exit codes:
- 0: success
- 1: compile/logic error, or `fail_count > 0` in directory mode (`std::process::exit(1)`)
- 2: I/O or filesystem error (`MdsError::Io`, `FileNotFound`, `NotMdsFile`)
- 3: resource limit exceeded (`MdsError::ResourceLimit`)

`run_build_directory` calls `std::process::exit(1)` directly (not via the `Result` chain) when fail_count > 0, matching other CLI exit points in `build.rs`.

## Anti-Patterns

- **Using `--format` flag** — deleted; does not exist in the CLI anymore. Using it as a clap argument will produce "unknown argument". Tests in `intrinsic_output.rs` assert this.
- **Calling `run_build_messages` or `run_build_markdown`** — deleted; only `run_build` exists, which dispatches to `run_build_directory` for dir inputs.
- **Calling `read_build_input` or `reject_directory_input`** — deleted.
- **Using 3-arg `output_path_for`** — the canonical signature is 4-arg with `ext: &str`. Never revert to a hardcoded extension.
- **Using `-o` with a directory input** — rejected with an error; use `--out-dir`.

## Gotchas

- `probe_and_remove_stale` is fire-and-forget (soft-warn on failure, never propagates). A stale sibling is an annoyance, not a correctness bug — so failures are non-fatal.
- `canonicalize_out_dir` resolves relative paths against `current_dir` and then canonicalizes. It must be called BEFORE `resolve_output_base` so that `starts_with` checks inside `run_build_directory` are reliable across relative/absolute paths.
- Watch mode derives the extension from `compiled.kind.extension()` after each compile. On deletion it must probe both `.md` and `.json` since the kind is not known.
- The `CompileOutput` struct in `build.rs` is a local CLI struct (content + kind + deps) — not the same as `mds::CompiledOutput` (the Rust enum). The naming is similar but they are different types.
- `mds.json build.output_dir` rejects `..` components at parse time to prevent path traversal. This check runs in both single-file and directory mode.

## Key Files

- `crates/mds-cli/src/main.rs` — clap argument parsing; `run_build`/`run_check`/`run_watch` dispatch; `mod output`
- `crates/mds-cli/src/build.rs` — `OutputKind`, `compile_to_content`, `compile_and_write`, `run_build`, `run_build_directory`, all output-path helpers for single-file mode
- `crates/mds-cli/src/output.rs` — `OutputBase`, `resolve_output_base`, `output_path_for`, `collect_mds_files`, `is_partial`, `probe_and_remove_stale`, `canonicalize_out_dir`, `output_base_no_ext`
- `crates/mds-cli/src/watch.rs` — watch loop; uses `compiled.kind.extension()` and `probe_and_remove_stale`
- `crates/mds-cli/tests/dir_build.rs` — 14 integration tests for directory mode (T-CLI-12–21 / FUNC-16–26)
- `crates/mds-cli/tests/intrinsic_output.rs` — tests asserting `--format` is rejected

## Related

- Feature: mds-compiler — provides `mds::CompileResult`, `mds::CompiledOutput`, `mds::compile_with_deps`
- Feature: mds-napi — parallel consumer of `CompileResult`; uses the same `kind` discriminant
- Feature: bundler-plugins — parallel consumer of the kind-based branch for bundler emitted modules
