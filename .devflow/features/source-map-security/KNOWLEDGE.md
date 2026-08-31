---
feature: source-map-security
name: Source Map Security and Path Containment
description: "Use when working with Source Map v3 generation, sources[] path relativization, the relativize_source choke-point, FileSystem::source_root(), CompileOptions.source_map_base, cross-surface source-map parity tests, or the Windows verbatim UNC path fix. Keywords: source map, sources[], relativize_source, source_map_base, source_root, path containment, basename fallback, PF-005, ADR-005, SEC-3, Windows verbatim UNC, path_to_unified, compute_source_map_base, apply_source_map_file_label, CF-SM2, V-SM1, differential test, two-level anchoring, map-relative, root-relative."
category: domain-knowledge
directories:
  - crates/mds-core/src
  - crates/mds-cli/src
  - packages/mds/src
created: 2026-07-19
updated: 2026-08-31
---

# Source Map Security and Path Containment

## Overview

Source Map v3 generation (`ADR-005`) is opt-in via `CompileOptions::source_map`. When enabled, `sources[]` entries must never expose absolute paths, machine-layout information, or paths outside the project root — failing to enforce this is a disclosure vulnerability (`PF-005`). The entire enforcement is funneled through a single choke-point function, `relativize_source` in `crates/mds-core/src/source_path.rs`, applied unconditionally at BOTH `finalize` sites in `resolver.rs`.

The security model is **project-root containment, not "no `../`"**. A source file at `/proj/src/a.mds` with the map in `/proj/build/` legitimately produces `../src/a.mds` — that path is inside the project root and is correct SMv3 output. The guard fires only when the resolved path escapes above the root, in which case the entry collapses to a bare basename.

## Business Context

The guard closes three disclosure classes: (1) paths above the project root leaking system-level directories; (2) absolute paths embedding machine layout; (3) Windows verbatim extended-length paths (`\\?\C:\...`) surviving as-is into published source maps. All invariants are enforced unconditionally at runtime — never as `debug_assert!` (which is compiled out in release builds; see `PF-005`).

## Core Business Rules

### The Containment Rule

The decision tree, checked in this order:

1. **Sentinel**: `source` starts with `<` and ends with `>` (e.g. `<stdin>`) → return verbatim. These are diagnostic labels, not filesystem paths.
2. **Separator unification first**: replace `\` with `/` before any other check — closes the backslash-on-Unix bypass where `..\..\Users\alice\secret.mds` would otherwise pass every subsequent `/`-based check.
3. **Verbatim-prefix strip**: remove `//?/UNC/` or `//?/` — closes Windows `canonicalize()` producing verbatim paths that misalign component lists (see "Windows UNC gotcha" below).
4. **Classify absolute** (leading `/` or drive-qualified `C:\`, `C:/`, `C:`).
5. **Lexical normalization into components** — resolves `.`, `..` — closes `./../../` and interior-dot-dot bypasses.
6. **`root = None` branch** (VirtualFs / WASM): no containment concept. Absolute/drive-qualified keys → basename; relative keys whose first component is `..` → basename; anything else passes through unified.
7. **Resolve against `base` (or `root`)** if not absolute.
8. **Containment check** (component-wise): source must be a descendant of `root`. If not → basename.
9. **Emit relative to `b`** where `b = base` if base is inside root, else `b = root`. Round-trip re-check before returning; on failure → basename.
10. **Basename fallback** uses the last non-`..` component of the NORMALIZED component list — never `Path::file_name()` on the raw string, which on Unix returns the whole string for backslash paths like `..\..\secret.mds`.

The discriminating test pair in `source_path.rs` that both must pass simultaneously:
- `core_rule_map_relative`: `/proj/src/a.mds`, base `/proj/build`, root `/proj` → `"../src/a.mds"` (inside root, map-relative)
- `core_rule_source_outside_root`: `/proj/src/a.mds`, base `/proj/build`, root `/proj/build` → `"a.mds"` (outside that root, fallback)

A prior fix (`a7ef84f`) got this backwards (treating any `../` as an escape) and was reverted. The correct rule is containment relative to root, not the absence of `..`.

### Two-Level Anchoring (CLI vs. Bindings)

`sources[]` paths are anchored to different locations depending on the surface:

| Surface | `source_map_base` | Anchoring | Example |
|---------|-------------------|-----------|---------|
| CLI (`mds build -o build/out.md`) | `Some(build/)` — output file's parent directory | Map-relative (`../src/x.mds`) | SMv3 spec §3 |
| CLI stdin / `-o -` | `Some(cwd())` | Map-relative against CWD | |
| napi, Python, WASM | `None` | Root-relative (`src/x.mds`) | |

When `base = None`, `relativize_source` anchors against root directly. This means CLI writing a sidecar into `build/` legitimately yields `../src/x.mds` while bindings yield `src/x.mds` — that is two correct values for two different anchors, not divergence.

### `FileSystem::source_root()` — defaulted trait method

```rust
// crates/mds-core/src/fs.rs
// Default returns None — safe for VirtualFs / WASM.
// NativeFs overrides: returns the path from init_root (walk-up from entry-point dir).
fn source_root(&self) -> Option<String> {
    None
}
```

`VirtualFs` inherits the default (`None`). `NativeFs` overrides it to return the project root established by the walk-up from the entry-point directory's `normalize()` call.

**Critical gotcha**: because `source_root()` is a *defaulted* method returning `None`, any external `FileSystem` implementor that forgets to override it silently lands on the `root = None` branch (step 6 above). That branch still enforces "never absolute, never drive-qualified" but skips the containment check. `resolver.rs` has a defense-in-depth guard for this:

```rust
// resolver.rs — at both finalize sites, before calling relativize_source:
// Defense-in-depth: if root was not established yet, establish it now.
// No-op for VirtualFs (source_root() always None, ctx.base_dir empty).
if self.fs.source_root().is_none() && !ctx.base_dir.is_empty() {
    let _ = self.fs.set_root(ctx.base_dir);
}
```

## Technical Implementation Patterns

### `source_path.rs` — the single choke-point

`pub fn relativize_source(source: &str, base: Option<&Path>, root: Option<&Path>) -> String` in `crates/mds-core/src/source_path.rs` is the ONLY place where absolute source paths are converted to relative map entries. Re-exported from `crates/mds-core/src/lib.rs`. Called exactly twice — once per finalize site in `resolver.rs` — both calls unconditional.

**Do not add a third call site.** The `PF-004` single-choke-point principle is what makes the security guarantee structural rather than incidental. Any new code path that emits `sources[]` entries must flow through this function.

### `CompileOptions.source_map_base`

```rust
// crates/mds-core/src/sourcemap.rs
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    pub source_map: bool,
    pub include_sources_content: bool,
    pub source_map_base: Option<std::path::PathBuf>,  // new field (PR #196)
}
```

`CompileOptions` does NOT carry `#[non_exhaustive]` (deliberately). That attribute would forbid struct-literal and `..Default::default()` construction from other crates, breaking all three bindings. Callers outside this crate that construct `CompileOptions` by field must use `..Default::default()` to stay forward-compatible.

CLI sets `source_map_base = Some(compute_source_map_base(...))`. Bindings leave it `None`.

### `compute_source_map_base` in `crates/mds-cli/src/build.rs`

A pure pre-compile directory oracle. It mirrors the output-directory rules of `resolve_output_path_for_kind` without creating directories:

```
-o - or stdin → Some(cwd())
-o <file>     → Some(abs(effective_parent(Path::new(output_file))))
--out-dir <d> → Some(abs(dir))
mds.json output_dir → Some(abs(config_dir.join(output_dir)))
default       → Some(abs(effective_parent(input)))
```

**Must use `compute_output_dir_path_for_kind`, NOT `prepare_output_dir_for_kind`**: `prepare_output_dir_for_kind` calls `create_dir_all` and would leave an empty output directory behind if the compile fails. Directory-mode `opts` is built per-file so each file gets its own base.

### `apply_source_map_file_label` in `crates/mds-cli/src/build.rs`

The CLI's only post-`relativize_source` source-map work (renamed from `relativize_source_map_fields`). Two jobs only:
1. `sm.file = output_basename` — the SMv3 `file` field names the generated artifact; core always emits `file: None`.
2. `<stdin>` relabel: maps `STRING_SOURCE_MAP_LABEL` (`"input.mds"`) → `"<stdin>"` for stdin builds. This is a pure label swap — no path logic.

The old `relativize_source_path` and `relative_path` CLI helpers are deleted. Everything path-related now happens inside `relativize_source`.

### Cross-Surface Parity Tests

Two differential tests enforce that surfaces agree with each other, not just with their own golden:

- **`V-SM1`** (`packages/mds/__test__/source-map.spec.mjs`): WASM `compile()` with explicit filename + empty modules produces the same `sources[]` as native napi. Virtual keys are relative by construction — guards that `relativize_source`'s `root = None` branch does not accidentally mutate them.
- **`CF-SM2`** (same file): napi, WASM, CLI, and Python all produce identical `sources[]` for a nested `@import` fixture. **Hard-fails in CI** (`process.env.CI`) if any surface is unavailable — a silently-skipped parity test reads as green and is worse than no test.

In CI the JS job must:
- Build `mds-cli` (Rust binary) before running the JS tests.
- `pip install ./crates/mds-python` to supply the Python surface.
- Expose `MDS_CLI_BIN` and `MDS_PYTHON_BIN` env vars so the test's surface-discovery logic finds them.


### R3 Display-Path Architecture (`display_path_for`)

Source-map `sources[]` interning uses the canonical key (absolute path), but **diagnostic display** must never expose absolute paths (CWE-209). `display_path_for` in `source_path.rs` bridges the two:

```rust
// crates/mds-core/src/source_path.rs
// Wraps relativize_source with base=None: produces root-relative display paths.
// Returns the key verbatim for VirtualFs (no root) and <sentinel> paths.
pub(crate) fn display_path_for(fs: &dyn FileSystem, key: &str) -> String {
    let root_str = fs.source_root();
    let root = root_str.as_deref().map(std::path::Path::new);
    relativize_source(key, None, root)
}
```

**`Origin` struct split** (`crates/mds-core/src/sourcemap.rs`): each loaded module carries two identity fields:
- `file: Arc<str>` — canonical key (absolute path for NativeFs, virtual key or `<source>` sentinel for VirtualFs). Used exclusively for source-map interning in `MapBuilder::source_index`. Must NEVER reach a user-visible surface.
- `display: Arc<str>` — root-relative display path, populated at construction time via `display_path_for(fs, key)`. Used for error messages, `NamedSource` names, and diagnostic `file` labels (R3 / CWE-209 / PF-013).

The `Origin.display` field is populated eagerly at module-load time so no absolute path can "escape" to a display surface later, even via a code path that doesn't explicitly call `display_path_for`.

**`MapBuilder` parallel `display_names`**: `MapBuilder` carries two parallel vecs:
- `sources: Vec<String>` — canonical keys; emitted verbatim into SMv3 `sources[]` (byte-identical to ADR-005 contract)
- `display_names: Vec<String>` — root-relative display paths; used only for diagnostics, never emitted into `sources[]`

Both `MapBuilder::new(source_name, display_name, source_content)` and `MapBuilder::source_index(file, display, content)` are 3-argument; callers must supply both the canonical key and the display path. A `debug_assert_eq!` enforces strict length parity between the two vecs after every insertion.

The `sources[]` bytes emitted into produced source maps are **byte-identical** to what they were before R3 — only the diagnostic display path changes. ADR-005 is preserved.

**CLI `read_source_file` anchors display roots**: In `crates/mds-cli/src/lint.rs`, `read_source_file` calls `fs.set_root(effective_parent(&canonical))` before `fs.read()`. This anchors the project-root walk-up for the `NativeFs` instance used in single-file lint mode, so even error messages from the raw read path show root-relative display paths instead of the basename fallback.

### TS Mirror: `packages/bundler-utils/src/project-root.ts`

The bundler-utils package mirrors the Rust display-path logic in TypeScript for two concerns: finding the project root and normalising dependency paths emitted to bundler metadata.

**`findProjectRoot(start)`**: Walks up from `start` to find `.git` or `.mdsroot` markers (same marker list and bounded traversal as `NativeFs::find_project_root`). Result is cached per start-directory. ARCHITECTURE EXCEPTION: uses synchronous `existsSync` — bounded and cached, same trade-off as `module-scanner.ts`.

**`stripWindowsVerbatimPrefix(p)`**: Strips Windows verbatim extended-length path prefixes produced by `std::fs::canonicalize`. Mirrors `path_to_unified` in `source_path.rs`. **Deliberate divergence**: core drops the `\\?\UNC\` prefix entirely (it builds component lists), but the TS version rewrites it to `\\` so the result remains a functional absolute UNC path for the watch sink (`addWatchFile`/`addDependency` must receive absolute paths).

**`toAbsoluteDependency(root, dep)`**: Converts a compiler-reported dependency to an absolute path. The native backend emits absolute canonical paths (passed through after `stripWindowsVerbatimPrefix`); the WASM backend emits root-relative POSIX paths (resolved against `root`). `TransformResult.dependencies` carries ABSOLUTE paths — bundlers resolve relative paths against cwd, not the project root.

**`toRootRelativePosix(root, absPath)`**: Converts an absolute dependency path to a root-relative POSIX path for the emitted `metadata` literal (which ships in production bundles — absolute host paths are an information leak). Mirrors `relativize_source`: strips verbatim prefixes, unifies separators, then applies the escape/drive-qualified/`../` guards before the basename fallback. Ultimate fallback is `"source"`, matching core.

**Two wire contracts**:
1. `TransformResult.dependencies` — ABSOLUTE paths (watch input). Bundlers call `addWatchFile`/`addDependency` with these.
2. Emitted `metadata` literal — ROOT-RELATIVE POSIX paths. Never absolute, never `../`, never drive-qualified.

The Windows verbatim lesson is the same on both sides: native backend emits `\\?\D:\...` on Windows; the TS must strip verbatim prefixes BEFORE unifying separators, mirroring what `path_to_unified` does in core. The UNC rewrite divergence (functional `\\server\share` vs. dropped prefix in core) is intentional and documented at the call site.

## Anti-Patterns

- **Adding a second call site for `sources[]` relativization**: The entire security guarantee rests on the single choke-point. A second inline call bypasses the 10-step guard algorithm partially or entirely.

- **Using `Path::file_name()` on a raw source string as a basename fallback**: On Unix, `Path::file_name()` returns the entire string verbatim for paths like `..\..\Users\alice\secret.mds` (no `/` components). The correct fallback uses the last non-`..` component of the NORMALIZED component list after unification and lexical normalization.

- **Treating `root = None` as "no security needed"**: The `root = None` branch (VirtualFs / WASM) still enforces "never absolute / never drive-qualified". Omitting those guards because "there is no root" is incorrect — the invariants must hold on all branches.

- **Using `debug_assert!` to enforce path-containment invariants**: They are compiled out in release builds. All invariants in `source_path.rs` use `assert!` or runtime checks, never `debug_assert!` (avoids PF-005).

- **Using `prepare_output_dir_for_kind` inside `compute_source_map_base`**: That function creates the directory on disk. `compute_source_map_base` is a pure oracle; it must never create directories.

- **Adding `#[non_exhaustive]` to `CompileOptions`**: It would break all three binding crates' field-literal construction. Use `..Default::default()` in callers instead.

## Gotchas

**Windows verbatim UNC root** (`path_to_unified` fix): `std::fs::canonicalize` on Windows returns verbatim UNC paths (`\\?\C:\proj`). After `replace('\\', "/")` this becomes `//?/C:/proj`, and `normalize_abs` yields components `["?", "C:", "proj", ...]`. But the source path after the same treatment yields `["C:", "proj", ...]`. The prefix `"?"` causes the first-component comparison to fail → containment always fails → EVERY source map entry degrades to its basename. `path_to_unified` now strips `//?/UNC/` then `//?/` before normalizing, so root components match source components. This bug is invisible on Unix CI.

**`source_root()` returns `None` before any `normalize()` call**: `NativeFs::source_root()` returns `None` until at least one `normalize()` or explicit `set_root()` call establishes the project root. The defense-in-depth guard in `resolver.rs` catches this, but external callers that skip `normalize()` and jump straight to `compile_with_deps_opts` will land on the `root = None` branch.

**Directory-mode `opts` must be per-file**: In directory mode (`run_build_directory`), each file has a different output directory, so `source_map_base` differs per file. Constructing `opts` as loop-invariant (outside the per-file loop) would give every file the same anchor, producing incorrect relative paths for all but one file.

**`sources[]` anchored to the MAP FILE location, per SMv3**: The CLI writing a sidecar map into `build/` legitimately yields `../src/x.mds` while bindings yield `src/x.mds`. Both are correct for their respective anchors. Do not normalize these to the same value when writing cross-surface parity tests — assert them each against their own expected value within CF-SM2, comparing the set across surfaces only when anchor offsets are controlled.

**`STRING_SOURCE_MAP_LABEL` (`"input.mds"`)**: The internal sentinel for stdin source. Core canonicalizes the source entry at `MapBuilder::new` / `source_index`, so the exact-string check in `apply_source_map_file_label` always matches. Do not change the sentinel string without updating both the core constant and all three binding surface golden tests.

**Rebuild stale binding artifacts before any cross-surface comparison**: Checked-in binding artifacts (`.node`, `.wasm`, Python wheel) can be stale. CF-SM2 tests the installed binaries. Rebuild before running cross-surface tests or you are testing an old binary.

**pytest marker filter**: Use `pytest -m "not perf"`, never `pytest -k "not perf"`. The `-k` flag matches substrings of the full test node id and silently deselects test functions whose name merely contains "perf" (avoids PF-008).

## Key Files

- `crates/mds-core/src/source_path.rs` — `relativize_source` (the single choke-point; 10-step guard algorithm; `path_to_unified` with verbatim-prefix strip; `basename_fallback` using normalized components; `core_rule_map_relative` and `core_rule_source_outside_root` discriminating test pair); `display_path_for` (R3 display-path wrapper, root-relative for NativeFs, verbatim for VirtualFs)
- `crates/mds-core/src/fs.rs:111-132` — `FileSystem::source_root()` (defaulted `None`; NativeFs override at line 522)
- `crates/mds-core/src/resolver.rs:865-884, 943-963` — two finalize sites; both call `relativize_source` unconditionally; both include the defense-in-depth `set_root` guard
- `crates/mds-core/src/sourcemap.rs` — `Origin` struct (`file: Arc<str>` canonical key; `display: Arc<str>` root-relative display path, populated via `display_path_for`); `MapBuilder` (`sources[]` + parallel `display_names[]`; 3-arg `new` and `source_index`); `CompileOptions` struct (`source_map_base: Option<PathBuf>`); `SourceMap` struct carries `#[non_exhaustive]`, `CompileOptions` does not
- `crates/mds-cli/src/build.rs` — `compute_source_map_base` (pure oracle; uses `compute_output_dir_path_for_kind`); `apply_source_map_file_label` (two-job post-processor: `sm.file` + `<stdin>` relabel)
- `crates/mds-cli/src/lint.rs:380-393` — `read_source_file`: calls `fs.set_root(effective_parent(&canonical))` before `fs.read()` to anchor display roots for single-file lint mode
- `packages/bundler-utils/src/project-root.ts` — TS mirror of R3 display-path logic: `findProjectRoot` (`.git`/`.mdsroot` walk, cached), `stripWindowsVerbatimPrefix` (mirrors `path_to_unified`; UNC divergence documented), `toAbsoluteDependency` (watch-input absolutisation), `toRootRelativePosix` (metadata literal, mirrors `relativize_source`)
- `packages/mds/__test__/source-map.spec.mjs` — V-SM1 (WASM↔native virtual parity), CF-SM2 (four-surface differential test; hard-fails in CI if any surface missing)

## Related

- **ADR-005** (Source Map v3 generation): overall architecture decision; global-cursor `MapBuilder`; two-level anchoring (map-relative CLI, root-relative bindings); `MAX_SOURCEMAP_SEGMENTS=1M`; messages-mode yields `None`; SMv3 source paths are NEVER absolute.
- **PF-004** (parallel-path enforcement): the single choke-point principle. All `sources[]` relativization must flow through `relativize_source`. No alternate path.
- **PF-005** (security invariants must be unconditional, never `debug_assert!`): every guard in `source_path.rs` is a runtime check in release builds.
- **PF-007** (per-surface goldens cannot catch cross-surface divergence): V-SM1 and CF-SM2 are the differential tests this pitfall required. CF-SM2 hard-fails in CI if any surface is unavailable.
- **PF-008** (pytest `-k` vs `-m`): use `-m "not perf"` when running Python parity tests.
- `.devflow/features/mds-lint/KNOWLEDGE.md` — lint's `atomic_write_file` (unrelated to source maps but shares `output.rs`); `display_label`/`read_source_file` anchor the same R3 display-root for lint display paths.
- `.devflow/features/mds-fmt/KNOWLEDGE.md` — fmt does not emit source maps (CLI-only), but shares `output.rs` and `effective_parent`.
- `packages/bundler-utils/src/project-root.ts` — TS mirror of the display-path and verbatim-strip logic; two wire contracts (ABSOLUTE dependencies, root-relative POSIX metadata).
