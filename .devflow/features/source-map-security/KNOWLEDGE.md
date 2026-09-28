---
feature: source-map-security
name: Source Map Security and Path Containment
description: "Use when working with Source Map v3 generation, sources[] path relativization, the relativize_source choke-point, FileSystem::source_root(), CompileOptions.source_map_base, the SourceTable / MapBuilder source-registration model, splicing an @include'd module's FragmentMap, source-mapping an @extends chain, cross-surface source-map parity tests, the Windows verbatim UNC path fix, or the verbatim-prefix respelling of native dependencies and CLI display paths (#409). Keywords: source map, sources[], relativize_source, source_map_base, source_root, path containment, basename fallback, PF-005, PF-013, ADR-005, SEC-3, Windows verbatim UNC, path_to_unified, compute_source_map_base, apply_source_map_file_label, SourceTable, SourceIndex, MapBuilder, register, switch_to, switch_to_index, current_origin, remap_cache, splice_fragment, rebased, FragmentMap, into_fragment, evaluate_extends_regions, fragment_map_from, is_visible_export, prompt_body_and_map, apply_map_degradation, AC-SEC-04, CF-SM1, CF-SM2, CF-SM3, V-SM1, requireNativeLeg, differential test, two-level anchoring, map-relative, root-relative, anchor_base_dir, resolve_entry, display_native_path, native_dependencies, simplify_verbatim, verbatim.rs, safe_path, #409, #416, #412."
category: domain-knowledge
directories:
  - crates/mds-core/src
  - crates/mds-cli/src
  - packages/mds/src
created: 2026-07-19
updated: 2026-09-28
---

# Source Map Security and Path Containment

## Overview

Source Map v3 generation (`ADR-005`) is opt-in via `CompileOptions::source_map`. When enabled, `sources[]` entries must never expose absolute paths, machine-layout information, or paths outside the project root — failing to enforce this is a disclosure vulnerability (`PF-005`). The entire path-relativization enforcement is funneled through a single choke-point function, `relativize_source` in `crates/mds-core/src/source_path.rs`, applied unconditionally at BOTH `finalize` sites in `resolver.rs`.

The security model is **project-root containment, not "no `../`"**. A source file at `/proj/src/a.mds` with the map in `/proj/build/` legitimately produces `../src/a.mds` — that path is inside the project root and is correct SMv3 output. The guard fires only when the resolved path escapes above the root, in which case the entry collapses to a bare basename.

A second security surface, orthogonal to path relativization, is **source-registration correctness**: which module a segment or diagnostic gets attributed to. v0.5.0 Wave 1.5 fixed two bugs here. #416 replaced a per-region full-copy source model (quadratic in the number of `@extends` regions — a DoS on adversarially large templates) with `SourceTable`, an append-only, shared-`Arc` source registry, and in the same change fixed a latent CWE-209 leak: a file first registered by an `@include` splice used to keep its absolute canonical key as its display name. #412 closed a mapping-completeness gap: an `@include` of a module that itself `@extends` a base produced no source-map segments at all for the included text, so a later diagnostic inside that text (or a bundler consuming the map) misattributed or lost provenance for real, shipped compiles.

## Business Context

The path-containment guard closes three disclosure classes: (1) paths above the project root leaking system-level directories; (2) absolute paths embedding machine layout; (3) Windows verbatim extended-length paths (`\\?\C:\...`) surviving as-is into published source maps. All invariants are enforced unconditionally at runtime — never as `debug_assert!` (which is compiled out in release builds; see `PF-005`).

The source-registration fix (#416) closes a fourth class: a source-mapped compile of an `@extends` chain used to copy a region's file's source text once per region — every top-level skeleton node is its own region — so cost grew with the square of the base template's size (a 262,144-line base: 68 s with maps on vs. 0.6 s with maps off, debug build). A template large enough to trigger this is attacker-controlled input to any long-running build, dev server, or bundler plugin, so the quadratic is a DoS surface, not just a performance nit. The CWE-209 leak it fixed alongside was **latent, never released**: no shipped version had an `@include` of a module that itself owned an `@extends` region (that combination only became mappable in #412), so the absolute-path display name it would have produced never reached a diagnostic in practice.

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

`VirtualFs` inherits the default (`None`). `NativeFs` overrides it to return the project root established by the walk-up from the entry-point directory — anchored by the first `resolve_entry` or `anchor_base_dir` call (first writer wins; a later call never moves it).

**Critical gotcha**: because `source_root()` is a *defaulted* method returning `None`, any external `FileSystem` implementor that forgets to override it silently lands on the `root = None` branch (step 6 above). That branch still enforces "never absolute, never drive-qualified" but skips the containment check. `resolver.rs` has a defense-in-depth guard for this:

```rust
// resolver.rs — at both finalize sites, before calling relativize_source:
self.anchor_root_for_source_map(ctx.base_dir);

// ModuleCache::anchor_root_for_source_map — best-effort by design:
if self.fs.source_root().is_none() && !base_dir.is_empty() {
    let _ = self.fs.anchor_base_dir(base_dir);
}
```

The anchor is never propagated (c615ace): only a custom backend without a `source_root` of its own reaches it, after it accepted the entry, so a refusal must not fail the compile — it leaves `source_root()` at `None` and absolute sources degrade to basenames. `source_map_root_safety_net_never_fails_a_compile` pins it.

## Technical Implementation Patterns

### `source_path.rs` — the single choke-point

`pub fn relativize_source(source: &str, base: Option<&Path>, root: Option<&Path>) -> String` in `crates/mds-core/src/source_path.rs` is the ONLY place where absolute source paths are converted to relative map entries. Re-exported from `crates/mds-core/src/lib.rs`. Called exactly twice — once per finalize site in `resolver.rs` — both calls unconditional. Unchanged by Wave 1.5.

**Do not add a third call site.** The `PF-004` single-choke-point principle is what makes the security guarantee structural rather than incidental. Any new code path that emits `sources[]` entries must flow through this function.

### `CompileOptions.source_map_base`

```rust
// crates/mds-core/src/sourcemap.rs
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    pub source_map: bool,
    pub include_sources_content: bool,
    pub source_map_base: Option<std::path::PathBuf>,
}
```

`CompileOptions` does NOT carry `#[non_exhaustive]` (deliberately). That attribute would forbid struct-literal and `..Default::default()` construction from other crates, breaking all three bindings. Callers outside this crate that construct `CompileOptions` by field must use `..Default::default()` to stay forward-compatible.

CLI sets `source_map_base = Some(compute_source_map_base(...))`. Bindings leave it `None`.

### `compute_source_map_base` / `apply_source_map_file_label` in `crates/mds-cli/src/build.rs`

Unchanged this wave. `compute_source_map_base` is a pure pre-compile directory oracle mirroring `resolve_output_path_for_kind`'s rules without creating directories — must use `compute_output_dir_path_for_kind`, NOT `prepare_output_dir_for_kind` (the latter calls `create_dir_all` and would leave an empty output directory behind on a failed compile). `apply_source_map_file_label` does two jobs only after `relativize_source` has already run: `sm.file = output_basename`, and the `<stdin>` relabel (`STRING_SOURCE_MAP_LABEL` → `"<stdin>"`).

### Source Registration and Sharing — `SourceTable` / `MapBuilder` (#416)

Before #416, each spliced `@extends` region copied its file's full source text into the builder again; a chain with many regions from the same file paid for that copy every time — quadratic in region count. #416 replaces this with a shared, append-only registry.

**`SourceTable`** (`sourcemap.rs`) — the sources a `MapBuilder` has registered, in registration order. Fields are **private**; nothing outside `sourcemap.rs` can remove or reorder an entry:

```rust
#[derive(Default)]
pub(crate) struct SourceTable {
    origins: Vec<Origin>,
    key_index: HashMap<Arc<str>, u32>,
}
```

Its API is exactly: `intern(&mut self, &Origin) -> u32` (index of the origin's key, registering it — cloning an `Origin` bumps three `Arc` refcounts, never copies text — when the key is new), `get(&self, u32) -> Option<&Origin>`, `len(&self) -> usize`, `iter(&self) -> slice::Iter<Origin>`, and `into_origins(self) -> Vec<Origin>` (consumes the table; used by `into_fragment`). `intern` applies `map_source_label` to the incoming key before the dedup lookup so the `"<source>"`/`"input.mds"` sentinel can never register as two distinct entries; the **stored** `Origin` keeps its raw key — `finalize` applies the label again when building `sources[]`.

**`SourceIndex`** is a crate-private newtype (`pub(crate) struct SourceIndex(u32)`) that only `MapBuilder` mints, always from an index its own `SourceTable` just returned. `MapBuilder`'s public-to-the-crate surface is now:

- `register(&mut self, &Origin) -> SourceIndex` — register without switching.
- `switch_to(&mut self, &Origin) -> SourceIndex` — register (or find) and switch, returning the index it replaced (what a caller uses to restore the prior source, e.g. after an S8 function-body evaluation).
- `switch_to_index(&mut self, SourceIndex)` — switch back to a previously-handed-out index.
- `current_origin(&self) -> Option<&Origin>` — `None` only for an index another builder handed out; every index a builder hands out itself always resolves.

`sources: SourceTable` and `current_src: SourceIndex` are both **private fields** of `MapBuilder`. The only way to construct or move the current source is through these four methods, so "the current source is always a registered one" is a structural invariant, not a convention that call sites have to honor. `evaluator.rs`'s S8 function-body path (`invoke_function`) and `resolver.rs`'s `evaluate_regions_with_map` region loop both hold this pattern: snapshot with `switch_to`/`register`, evaluate, restore with `switch_to_index`.

**Remap cache** — the fragment-splice fast path for repeated `@include`:

```rust
remap_cache: HashMap<usize, (Arc<FragmentMap>, Vec<u32>)>,
```

Keyed by `Arc::as_ptr(fragment) as usize`. The entry stores the `Arc` itself alongside the local→global remap — so the pointer that keys the cache cannot be freed and reused by an unrelated fragment while the key is live (no ABA hazard), and the cached indices stay valid for the builder's whole life because `SourceTable` is append-only. This means an `@include` of one module — from a `@for` loop or from several call sites — computes its source remap exactly once per compile.

**`splice_fragment(&mut self, fragment: &Arc<FragmentMap>, base: u32)`** returns immediately when `self.segments_dropped` is already true: once the segment cap has degraded the map, nothing more is registered or pushed for any later splice. Otherwise it computes (or reuses) the remap, then rebases every fragment segment via the free function `rebased`:

```rust
/// `seg` rebased to start at output offset `base` and attributed to its source's
/// global index through `remap`, or `None` when `remap` has no entry for its
/// source: the segment is then dropped, never attributed to another file.
fn rebased(seg: &RawSegment, remap: &[u32], base: u32) -> Option<RawSegment> {
    let &src = remap.get(seg.src as usize)?;
    Some(RawSegment { out: base + seg.out, src, ..*seg })
}
```

A miss (`remap.get` returning `None`) is unreachable by construction — `into_fragment` is the *only* way to build a `FragmentMap`, and every segment it records names one of the fragment's own sources — so it is a compiler-bug canary, not a real degradation path: `splice_fragment` asserts it with `debug_assert!("FragmentMap segment names no fragment source")` right before calling `rebased`, and in release builds where the debug_assert is compiled out, `rebased`'s `None` return makes the caller `continue` (drop the segment, never misattribute it to another file) — the same "degrade, never mis-attribute" policy `expand_per_line` already applies at `finalize`. This shape (debug canary + release-safe drop, both unconditionally tested — `sourcemap::tests::rebased_drops_a_segment_whose_source_the_remap_lacks` runs in every build profile) was chosen over an entry-API `Result`-returning rewrite after measuring the alternatives in WASM: every shape that made the splice/fragment path fallible cost +6.4 to +9.7 KB, against a project-wide WASM size budget.

**`FragmentMap`** (an included module's pre-computed map) also has private fields now:

```rust
pub(crate) struct FragmentMap {
    sources: Vec<Origin>,   // local interner, in the module builder's registration order
    segments: Vec<RawSegment>,
}
```

`MapBuilder::into_fragment(self) -> FragmentMap` is the sole constructor, so "every segment names one of the fragment's own sources" became a compile-time fact rather than a convention. Its manual `Debug` impl prints counts only (`FragmentMap { sources: 1, segments: 1 }`), never a path or source text.

**`finalize`** builds the public `sources: Vec<String>` and `sourcesContent` arrays exactly once each, from `SourceTable::iter()`, rather than accumulating them incrementally alongside registration as the pre-#416 model did.

### An `@include` of an Extending Module Is Mapped (#412)

Before #412, an `@include`d module that itself declared `@extends` produced **no** `FragmentMap` at all — `process_module_extends` never built a `MapBuilder` for the chain's regions, so an importer's `@include` of such a module spliced nothing, and that text carried no source-map segments even though the same `@include` of a standalone module was fully mapped. The fix shares the region-evaluation and fragment-construction machinery between the entry `@extends` path and the imported-module `@extends` path:

- **`evaluate_extends_regions(regions, seed: Option<&Origin>, scope, warnings)`** (`resolver.rs`, right after `evaluate_regions_with_map`) is the shared entry point both `process_module_intrinsic_opts` (the compile entry point's own `@extends` chain) and `process_module_extends` (an imported extending module) now call. `seed` is `Some(&skeleton_origin)` — the chain's root base `Origin` — when source maps are wanted for this evaluation; `None` otherwise. When `Some`, it builds a fresh `MapBuilder::new(seed.clone())` and runs every region through it under **one** `EvalBudget`, exactly as a direct compile of that same chain would.
- Index 0 of the resulting `FragmentMap`/map is always the chain's root base — the file at the top of the `@extends` chain — never the leaf module that was actually `@include`d, matching what a direct compile of the leaf produces.
- **`fragment_map_from(builder, body: Option<&str>, display, warnings) -> Option<Arc<FragmentMap>>`** is the shared decision for "is there a fragment worth splicing": `None` when the segment cap already dropped a segment (with a warning naming the module), `None` when the body is empty (an `@include` of it adds no text — nothing to warn about), and `Some(Arc::new(builder.into_fragment()))` otherwise. Both `process_module` (standalone) and `process_module_extends` go through it via the shared `prompt_body_and_map(body_raw, builder, display, warnings)` helper.
- The cap-exceeded warning text is now truthful about what actually happens: `source map segment cap (1000000 segments) exceeded in imported module '<display>'; text included from it is left unmapped in the source map` — it used to say "no source map will be generated", which was false for an imported module (the *importer's* map is still produced; only the included text loses its mapping).
- **`is_visible_export(has_explicit_exports, &explicit_exports, name)`** is the one export-visibility rule (`!has_explicit_exports || explicit_exports.contains(name)`), used by both `ResolvedModule::is_exported` and the `prompt`-export gate in both module paths — replacing two independent copies of the same check.
- A string compile that itself `@extends` a base now correctly puts the chain's root base at `sources[0]`, with the compiled string's own `input.mds` label following it — not `sources[0] == "input.mds"` unconditionally as all five README examples used to claim. `source_map_vfs::extends_string_compile_names_the_base_first` pins the corrected behavior; the five READMEs (`packages/mds`, `crates/mds-napi`, `crates/mds-python`, `packages/mds-wasm`, and `spec.md`) were all corrected in the same commit.
- `sourcesContent`'s AC-SEC-04 ceiling now genuinely covers an included extending chain (previously it could only ever see the importer's own file plus non-extending imports): `crates/mds-core/tests/source_map_vfs.rs`'s `include_of_extending_chain_over_the_sources_content_ceiling_drops_sources_content` is the **first test that actually exercises this ceiling** with a real over-limit corpus (a 6-level chain, each level padded to push total embedded source bytes past `MAX_SOURCES_CONTENT_BYTES`) rather than a vacuous `!sources.is_empty()` check — see Gotchas.

### The Latent Display-Name Leak Fixed by #416 (CWE-209)

Before #416, a file first registered through an `@include` splice (rather than being the builder's own seed) was interned using its absolute canonical key as its display name — the code path that later became `source_index`/`SourceTable::intern` did not thread the display path through for a splice-registered entry. A diagnostic raised later inside that file's `@extends` region would then have printed the absolute path. **This was never reachable in any released version**: before #412, an `@include`d module that itself owned an `@extends` region produced no `FragmentMap` at all (see above), so the only way to reach the bug — a splice registering a file that owns its own `@extends` region — did not exist until #412 landed in the same wave. #416 fixes the underlying cause structurally: `SourceTable::intern` stores the *whole* `Origin` (both `file` and `display`) the first time a key is registered, whichever call site — the builder's own seed or a later splice — got there first, so a splice-registered origin always carries the root-relative `display` it was loaded with. `resolver::tests::region_first_registered_by_a_splice_names_its_display_path` and `diagnostic_in_a_file_first_registered_by_a_splice_names_it_root_relative` (on-disk, via #412's mapping fix) pin this.

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
- `file: Arc<str>` — canonical key (absolute path for NativeFs, virtual key or `<source>` sentinel for VirtualFs). Must NEVER reach a user-visible surface.
- `display: Arc<str>` — root-relative display path, populated at construction time via `display_path_for(fs, key)`. Used for error messages, `NamedSource` names, and diagnostic `file` labels (R3 / CWE-209 / PF-013).

`Origin` is `Clone`, and its doc is explicit that clone is **three** refcount bumps (`file`, `display`, `source` — corrected from an earlier "two" during Phase 2 of this wave, which is what makes `SourceTable::intern` cheap to call speculatively).

**One shared registry, not parallel Vecs.** Prior to #416, `MapBuilder` held `sources: Vec<String>` (canonical keys) and a separate `display_names: Vec<String>` in lockstep, guarded by a `debug_assert_eq!` on every insertion to keep the two Vecs' lengths in sync. That model is gone: `SourceTable` stores one `Vec<Origin>`, each entry carrying its own `file` and `display` together, so the two can no longer drift apart — there is nothing left to keep in sync, and no `debug_assert_eq!` for it. The `sources[]` bytes emitted into produced source maps remain byte-identical to the pre-R3 output; only the diagnostic display path changed.

**CLI `read_source_file` anchors display roots**: In `crates/mds-cli/src/lint.rs`, `read_source_file` → `read_canonical_source` calls `fs.anchor_base_dir(effective_parent(&canonical))?` before `fs.read()` (shared by `mds lint` and `mds fmt`). This anchors the project-root walk-up for the `NativeFs` instance used for the raw read, so even error messages from that path show root-relative display paths instead of the basename fallback. An anchor failure is propagated (`mds::io`, exit 2) since #155 — it used to be discarded. Unchanged by Wave 1.5.

### Verbatim respelling at display and dependency sinks (#409)

Unchanged by Wave 1.5. On Windows `std::fs::canonicalize` returns verbatim paths (`\\?\C:\…`, `\\?\UNC\server\share\…`). Native module keys stay verbatim inside the resolver (containment compares canonical paths), but two sinks respell them with `verbatim::simplify_verbatim`, which rewrites only when the conventional form names exactly the same file (≤ MAX_PATH; no reserved device name such as `CON`; no trailing dot or space; no `.`/`..` component) and otherwise keeps the prefix:

- **`CompileResult.dependencies`** of a native compile (`native_dependencies` in `lib.rs`) — Rust, napi, `@mdscript/mds` native backend, Python. Pinned on the Windows CI leg by `windows_dependencies_carry_no_verbatim_prefix` (`tests/api_surface.rs`) and on every host by the `verbatim.rs` unit tests.
- **Display text**: the public `mds::display_native_path` (a no-op off Windows) is the entry point for showing a path to a user; the CLI's `safe_path` choke-point routes every status-line and error-message path through it.

Source-map `sources[]` are unaffected — `relativize_source`'s `path_to_unified` already strips the prefix before building component lists.

### Cross-Surface Parity Tests

Differential tests enforce that surfaces agree with each other, not just with their own golden, all in `packages/mds/__test__/source-map.spec.mjs`:

- **`V-SM1`**: WASM `compile()` with explicit filename + empty modules produces the same `sources[]` as native napi. Virtual keys are relative by construction — guards that `relativize_source`'s `root = None` branch does not accidentally mutate them.
- **`CF-SM1`**: `compileFile` differential — native napi vs. WASM-via-`buildModulesMap`, single root-level file.
- **`CF-SM2`**: napi, WASM, CLI, and Python all produce identical `sources[]` for a nested `@import` fixture (cross-directory, not just single-file).
- **`CF-SM3`** (#412): all four surfaces produce **one whole matching source map** (not just `sources[]`) for an `@include` of a three-level `@extends` chain, `sourcesContent` on — the differential this wave added specifically to cover the #412 fix cross-surface. Anchors that every chain file is referenced by the mappings, not just listed in `sources[]`.

**`requireNativeLeg(t, label)`** is a shared helper all three `CF-SM*` tests call first: it `init()`s and checks the loaded backend is actually `'native'`. In CI (`process.env.CI`), a non-native backend throws — a differential test comparing "native vs. WASM" is meaningless if the native addon silently failed to load and both legs are secretly WASM; that would pass while comparing WASM to itself and prove nothing (`PF-013`/`PF-007` shape). Locally, a missing native addon skips the test with an explanation instead of failing the suite.

In CI the JS job must:
- Build `mds-cli` (Rust binary) before running the JS tests.
- `pip install ./crates/mds-python` to supply the Python surface.
- Expose `MDS_CLI_BIN` and `MDS_PYTHON_BIN` env vars so the test's surface-discovery logic finds them.

### TS Mirror: `packages/bundler-utils/src/project-root.ts`

Unchanged by Wave 1.5. The bundler-utils package mirrors the Rust display-path logic in TypeScript for two concerns: finding the project root and normalising dependency paths emitted to bundler metadata.

**`findProjectRoot(start)`**: Walks up from `start` to find `.git` or `.mdsroot` markers (same marker list and bounded traversal as `NativeFs::find_project_root`). Result is cached per start-directory.

**`stripWindowsVerbatimPrefix(p)`**: Mirrors `path_to_unified`. **Deliberate divergence**: core drops the `\\?\UNC\` prefix entirely (it builds component lists), but the TS version rewrites it to `\\` so the result remains a functional absolute UNC path for the watch sink (`addWatchFile`/`addDependency` must receive absolute paths).

**`toAbsoluteDependency(root, dep)`** / **`toRootRelativePosix(root, absPath)`**: two wire contracts — `TransformResult.dependencies` is ABSOLUTE (watch input, bundlers resolve relative paths against cwd); the emitted `metadata` literal is ROOT-RELATIVE POSIX (ships in production bundles — an absolute host path there is an information leak). `toRootRelativePosix` mirrors `relativize_source`'s guard order, with ultimate fallback `"source"`.

## Anti-Patterns

- **Adding a second call site for `sources[]` relativization**: The entire security guarantee rests on the single choke-point (`relativize_source`). A second inline call bypasses the 10-step guard algorithm partially or entirely.

- **Adding a second constructor for `FragmentMap`**: `MapBuilder::into_fragment` is the only way to build one. A second constructor would reopen the "does every segment name one of this fragment's own sources" question that private fields + one constructor now answer at compile time.

- **Weakening `splice_fragment`'s remap-miss canary into an entry-API `Result` rewrite**: measured at +6.4 to +9.7 KB of WASM size across every fallible-shape variant tried — an always-on internal error for an unreachable-by-construction condition is unaffordable against this crate's WASM budget. The adopted shape (debug canary + release-safe drop-not-misattribute, both tested in every profile) gets the same safety property for near-zero cost.

- **Using `Path::file_name()` on a raw source string as a basename fallback**: On Unix, `Path::file_name()` returns the entire string verbatim for paths like `..\..\Users\alice\secret.mds` (no `/` components). The correct fallback uses the last non-`..` component of the NORMALIZED component list after unification and lexical normalization.

- **Treating `root = None` as "no security needed"**: The `root = None` branch (VirtualFs / WASM) still enforces "never absolute / never drive-qualified". Omitting those guards because "there is no root" is incorrect — the invariants must hold on all branches.

- **Using `debug_assert!` to enforce path-containment invariants**: They are compiled out in release builds. All invariants in `source_path.rs` use `assert!` or runtime checks, never `debug_assert!` (avoids PF-005). The one exception, `splice_fragment`'s remap-miss canary, is explicitly NOT a security invariant — a miss there costs one dropped mapping, output text and `sources[]` are unaffected, so PF-005's "internal consistency checks acceptable to skip in release" applies, not its "security invariant" rule.

- **Using `prepare_output_dir_for_kind` inside `compute_source_map_base`**: That function creates the directory on disk. `compute_source_map_base` is a pure oracle; it must never create directories.

- **Adding `#[non_exhaustive]` to `CompileOptions`**: It would break all three binding crates' field-literal construction. Use `..Default::default()` in callers instead.

- **A cross-surface differential test that never checks WHICH backend actually ran**: comparing "native" against "WASM" when the native addon silently failed to load compares WASM against itself and always passes. Always assert the backend identity before trusting a differential result in CI (`requireNativeLeg`).

## Gotchas

**Windows verbatim UNC root** (`path_to_unified` fix): `std::fs::canonicalize` on Windows returns verbatim UNC paths (`\\?\C:\proj`). After `replace('\\', "/")` this becomes `//?/C:/proj`, and `normalize_abs` yields components `["?", "C:", "proj", ...]`. But the source path after the same treatment yields `["C:", "proj", ...]`. The prefix `"?"` causes the first-component comparison to fail → containment always fails → EVERY source map entry degrades to its basename. `path_to_unified` now strips `//?/UNC/` then `//?/` before normalizing, so root components match source components. This bug is invisible on Unix CI.

**`source_root()` returns `None` before the root is anchored**: `NativeFs::source_root()` returns `None` until a `resolve_entry` or `anchor_base_dir` call establishes the project root. The defense-in-depth guard in `resolver.rs` catches this at both finalize sites; it also returns `None` for a non-UTF-8 root (lossy strings are not anchors).

**Directory-mode `opts` must be per-file**: In directory mode (`run_build_directory`), each file has a different output directory, so `source_map_base` differs per file. Constructing `opts` as loop-invariant (outside the per-file loop) would give every file the same anchor, producing incorrect relative paths for all but one file.

**`sources[]` anchored to the MAP FILE location, per SMv3**: The CLI writing a sidecar map into `build/` legitimately yields `../src/x.mds` while bindings yield `src/x.mds`. Both are correct for their respective anchors. Do not normalize these to the same value when writing cross-surface parity tests — SM-18 (`cli_source_map.rs`) derives its expected anchor from `sources[0]` itself and requires every other entry to share it, rather than hardcoding either form.

**`splice_fragment` returns immediately once `segments_dropped` is set** — a degraded map registers no further sources and pushes no further segments for any later splice in the same compile. This is deliberate (`apply_map_degradation` discards the whole map anyway once the cap has been hit), not a bug to "fix" by continuing to register.

**The remap cache's `Arc::as_ptr` key is safe only because the cache stores the `Arc` itself.** Keying by raw pointer with nothing keeping the pointee alive would be an ABA hazard (a freed-and-reused allocation could collide with a stale key); storing `(Arc<FragmentMap>, Vec<u32>)` in the entry means the pointer that keys the cache cannot be freed and reused while the key is live.

**An extending module's "prompt not exported" gate is structurally unreachable**: `check_child_only_blocks` forbids `@export` (and `@define`) in any child template, so an extending module always exports `prompt` when it has one — there is no legal fixture that reaches the not-exported branch of `process_module_extends`'s `prompt_exported` gate through an extending module. Test coverage for "gate suppresses a not-exported map" therefore uses a **standalone** not-exported module instead; the extending-path gate is kept only for parity with the standalone path's structure, not because it is independently reachable.

**AC-SEC-04's `sourcesContent` ceiling had no real test until this wave** — the prior assertion for a multi-source `@extends` compile was the vacuous `!sources.is_empty()` (PF-013 shape: passes for any non-trivial compile, proves nothing about the specific ceiling behavior). `include_of_extending_chain_over_the_sources_content_ceiling_drops_sources_content` (`source_map_vfs.rs`) is the first test that actually drives total embedded source bytes past `MAX_SOURCES_CONTENT_BYTES` (a 6-level `@extends` chain, each level padded with ~9 MiB of unrendered text) and asserts the exact ceiling-exceeded warning plus `sourcesContent` being dropped, with a small-padding control that keeps it.

**`STRING_SOURCE_MAP_LABEL` (`"input.mds"`)**: The internal sentinel for stdin source. `SourceTable::intern` canonicalizes the source entry via `map_source_label`, so the exact-string check in `apply_source_map_file_label` always matches. Do not change the sentinel string without updating both the core constant and all three binding surface golden tests. For a string compile that `@extends` a base, this label is `sources[0]` only when there is no base — otherwise the chain's root base occupies `sources[0]` and the label follows it (#412; see above — five READMEs previously claimed it was always `sources[0]`).

**Rebuild stale binding artifacts before any cross-surface comparison**: Checked-in binding artifacts (`.node`, `.wasm`, Python wheel) can be stale. `CF-SM*` tests the installed binaries. Rebuild before running cross-surface tests or you are testing an old binary.

**pytest marker filter**: Use `pytest -m "not perf"`, never `pytest -k "not perf"`. The `-k` flag matches substrings of the full test node id and silently deselects test functions whose name merely contains "perf" (avoids PF-008).

## Key Files

- `crates/mds-core/src/source_path.rs` — `relativize_source` (the single choke-point; 10-step guard algorithm; `path_to_unified` with verbatim-prefix strip; `basename_fallback` using normalized components); `display_path_for` (R3 display-path wrapper). Unchanged this wave.
- `crates/mds-core/src/fs.rs:111-132` — `FileSystem::source_root()` (defaulted `None`; NativeFs override). Unchanged this wave.
- `crates/mds-core/src/resolver.rs` — two finalize sites in `process_module_intrinsic_opts` (grep `Step 5 — single choke-point`); `evaluate_regions_with_map` (per-region source switch, one `EvalBudget`); `evaluate_extends_regions` (#412 shared entry for both extends paths, seeds a `MapBuilder` with the chain's root `Origin`); `is_visible_export`, `fragment_map_from`, `prompt_body_and_map`, `apply_map_degradation` (free fns just above `has_message_block`); `process_module` / `process_module_extends` (both go through the shared helpers)
- `crates/mds-core/src/sourcemap.rs` — `Origin` (`file`/`display`/`source`, `Clone` = three refcount bumps); `SourceTable` (private `origins`/`key_index`; `intern`/`get`/`len`/`iter`/`into_origins`); `SourceIndex` (crate-private newtype, minted only by `MapBuilder`); `MapBuilder` (private `sources`/`current_src`; `register`/`switch_to`/`switch_to_index`/`current_origin`; `remap_cache` keyed by fragment `Arc` pointer; `splice_fragment`; `into_fragment`; `finalize`); `rebased` (free fn, remap-miss → `None`, drop-not-misattribute); `FragmentMap` (private fields, `into_fragment` is the sole constructor); `CompileOptions` (`source_map_base: Option<PathBuf>`, no `#[non_exhaustive]`)
- `crates/mds-core/src/evaluator.rs` — `evaluate_with_map_seeded` (derives `EvalContext.file`/`.source` from `builder.current_origin()`); `evaluate_include` (splices via `map.splice_fragment(fragment, base)` directly — the old `ctx.map.take()` workaround is gone); the S8 function-body path in the interpolation arm (`map.switch_to(origin)` / `map.switch_to_index(saved_src)`)
- `crates/mds-cli/src/build.rs` — `compute_source_map_base` (pure oracle); `apply_source_map_file_label` (two-job post-processor: `sm.file` + `<stdin>` relabel). Unchanged this wave.
- `crates/mds-cli/src/lint.rs` — `read_source_file` / `read_canonical_source`: anchors display roots for lint and fmt. Unchanged this wave.
- `crates/mds-core/src/verbatim.rs` — `simplify_verbatim` (lossless verbatim → conventional respelling; Windows builds only, unit-tested on every host); users: `native_dependencies` and `display_native_path` in `lib.rs`. Unchanged this wave.
- `packages/bundler-utils/src/project-root.ts` — TS mirror of R3 display-path logic. Unchanged this wave.
- `crates/mds-core/tests/source_map_vfs.rs` — `extends_many_regions_source_map_is_not_quadratic` (#416 AC-3, calibrated corpus + `recv_timeout` bound); `include_of_extending_chain_attributes_each_byte` / `include_that_adds_no_text_keeps_no_map_and_warns_the_same` / `segment_cap_in_an_included_module_leaves_its_text_unmapped` (#412 AC-4/6/7); `include_of_extending_chain_over_the_sources_content_ceiling_drops_sources_content` (AC-SEC-04, first real ceiling test, `ceiling_chain` fixture); `extends_string_compile_names_the_base_first`
- `crates/mds-cli/tests/cli_source_map.rs` — `sm18_include_of_extending_chain_sources_are_relative` (on-disk #412: a 3-level chain `@include`d from `src/`, built into `build/`, every chain file relative-and-contained, anchor derived from `sources[0]`)
- `packages/mds/__test__/source-map.spec.mjs` — `requireNativeLeg` (shared native-backend assertion for CI); V-SM1, CF-SM1, CF-SM2, CF-SM3 (four-surface differential; CF-SM3 compares the whole map, not just `sources[]`)

## Related

- **ADR-005** (Source Map v3 generation): overall architecture decision; global-cursor `MapBuilder`; two-level anchoring (map-relative CLI, root-relative bindings); `MAX_SOURCEMAP_SEGMENTS=1M`; messages-mode yields `None`; SMv3 source paths are NEVER absolute.
- **PF-004** (parallel-path enforcement): the single choke-point principle. All `sources[]` relativization must flow through `relativize_source`; the shared `evaluate_extends_regions`/`fragment_map_from`/`is_visible_export` helpers apply the same one-path principle to source-map construction and export visibility.
- **PF-005** (security invariants must be unconditional, never `debug_assert!`): every path-containment guard in `source_path.rs` is a runtime check in release builds; `splice_fragment`'s remap-miss canary is the documented exception (an internal-consistency check, not a security invariant — its release behavior is drop-not-misattribute either way).
- **PF-007** (per-surface goldens cannot catch cross-surface divergence): V-SM1, CF-SM1, CF-SM2 and CF-SM3 are the differential tests this pitfall required.
- **PF-008** (pytest `-k` vs `-m`): use `-m "not perf"` when running Python parity tests.
- **PF-013** (a test that asserts only absence, or a vacuous/misspelled needle, passes with no guard behind it): `requireNativeLeg` closes the "differential test secretly compares one backend to itself" shape of this pitfall; the AC-SEC-04 ceiling test replaced a `!sources.is_empty()` vacuous assertion with an exact one.
- `.devflow/features/mds-lint/KNOWLEDGE.md` — lint's `atomic_write_file` (unrelated to source maps but shares `output.rs`); `display_label`/`read_source_file` anchor the same R3 display-root for lint display paths.
- `.devflow/features/mds-fmt/KNOWLEDGE.md` — fmt does not emit source maps (CLI-only), but shares `output.rs` and `effective_parent`.
- `packages/bundler-utils/src/project-root.ts` — TS mirror of the display-path and verbatim-strip logic; two wire contracts (ABSOLUTE dependencies, root-relative POSIX metadata).
