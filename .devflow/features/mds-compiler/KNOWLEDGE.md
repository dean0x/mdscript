---
feature: mds-compiler
name: MDS Compiler Core (mds-core)
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, working with @message blocks, the intrinsic output format, CompiledOutput, CompileResult, mixed-content errors, the FileSystem trait / path security (entry and import resolution, base-directory anchoring, forbidden path characters, Windows verbatim paths), or @extends region-by-region evaluation and its budgets. Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, include, define, for, if, message, @message, CompiledOutput, CompileResult, into_markdown, into_messages, intrinsic, mixed_content, MixedContent, has_message_block, process_module_intrinsic, collect_messages_strict, evaluate_messages_intrinsic, TextNode.offset, FileSystem, NativeFs, VirtualFs, resolve_entry, normalize_in_dir, anchor_base_dir, parent_dir, source_root, validate_entry_path, validate_relative_import, validate_import_path, import_path_violation, resolve_entry_key, anchor_base, check_segment_count, MAX_PATH_SEGMENTS, check_symlink, check_symlink_named, canonical_dir, init_root, resolve_base_dir, is_forbidden_path_char, escape_path_for_message, reject_forbidden_in_path, with_fs, display_native_path, native_dependencies, simplify_verbatim, verbatim.rs, EvalBudget, evaluate_seeded, evaluate_with_map_seeded, evaluate_messages_seeded, evaluate_regions_with_map, evaluate_message_regions, process_module_extends, validate_extends_components, spliced_regions, Origin."
category: domain-knowledge
directories: ["crates/mds-core/"]
referencedFiles:
  - crates/mds-core/src/lib.rs
  - crates/mds-core/src/evaluator.rs
  - crates/mds-core/src/resolver.rs
  - crates/mds-core/src/error.rs
  - crates/mds-core/src/ast.rs
  - crates/mds-core/src/resolver/inheritance.rs
  - crates/mds-core/src/resolver/frontmatter.rs
  - crates/mds-core/src/limits.rs
  - crates/mds-core/src/scope.rs
  - crates/mds-core/src/value.rs
  - crates/mds-core/src/fs.rs
  - crates/mds-core/src/verbatim.rs
  - crates/mds-core/src/lint/diagnostic.rs
  - crates/mds-core/tests/forbidden_path_chars.rs
created: 2026-06-26
updated: 2026-09-25
---

# MDS Compiler Core (mds-core)

## Overview

`mds-core` is the Rust library crate at `crates/mds-core/`. Every other layer (CLI, NAPI bindings, WASM bindings) calls into it. It compiles `.mds` template files to either Markdown or structured chat-message arrays — the distinction is **intrinsic** to the template: the presence of any `@message` block causes the compiler to produce `CompiledOutput::Messages`; templates without `@message` blocks always produce `CompiledOutput::Markdown`. Callers do not specify a format at call time.

The pipeline is: lexer → parser → validator → resolver (imports, inheritance, frontmatter) → evaluator → output wrapping. All error codes carry the `mds::` prefix (e.g. `mds::mixed_content`, `mds::syntax`, `mds::undefined_variable`).

## Business Context

The intrinsic output model replaced a previous `--format messages` flag and a separate `compile_messages_*` API family. Those symbols are deleted. The key invariant: a template is either a "markdown template" or a "messages template" — it cannot be both. Top-level text content intermixed with `@message` blocks is a hard error (`mds::mixed_content`), not a warning.

## Core Business Rules

### Intrinsic Output Rule

The compiled output kind is determined by `process_module_intrinsic` in `resolver.rs`. It calls `has_message_block` on the resolved AST; if any `@message` node exists at the entry module level the output is Messages, otherwise Markdown.

- `CompiledOutput::Markdown(String)` — no `@message` blocks in template
- `CompiledOutput::Messages(Vec<Message>)` — at least one `@message` block

Mixed content (top-level text/interpolation AND `@message` blocks in the same module) is rejected with `MdsError::MixedContent`.

### CompileResult — the only public output type

Every `compile*` entry point returns `Result<CompileResult, MdsError>`:

```rust
pub struct CompileResult {
    pub output: CompiledOutput,   // Markdown(String) or Messages(Vec<Message>)
    pub warnings: Vec<String>,
    pub dependencies: Vec<String>, // depth-first, excludes entry module itself
}

impl CompileResult {
    pub fn into_markdown(self) -> Result<String, MdsError>  // Err: ExpectedMarkdown
    pub fn into_messages(self) -> Result<Vec<Message>, MdsError> // Err: ExpectedMessages
}
```

`dependencies` is in first-resolution (depth-first) order and excludes the entry module itself.

### Public Entry Points (all return `Result<CompileResult, MdsError>`)

```rust
mds::compile(path, runtime_vars)
mds::compile_str(source)
mds::compile_str_with(source, base_dir, runtime_vars)
mds::compile_file(path)            // path as &str
mds::compile_collecting_warnings(path, runtime_vars)
mds::compile_str_collecting_warnings(source, base_dir, runtime_vars)
mds::compile_virtual(modules, entry, runtime_vars)
mds::compile_virtual_collecting_warnings(modules, entry, runtime_vars)
mds::compile_with_deps(path, runtime_vars)
mds::compile_str_with_deps(source, base_dir, runtime_vars)
mds::compile_virtual_with_deps(modules, entry, runtime_vars)
```

Check functions — signature unchanged externally, now route through intrinsic dispatch internally (rejects mixed content):

```rust
mds::check(path, runtime_vars) -> Result<(), MdsError>
mds::check_str(source) -> Result<(), MdsError>
mds::check_str_with(source, base_dir, runtime_vars)
mds::check_collecting_warnings(path, runtime_vars) -> Result<((), Vec<String>), MdsError>
mds::check_str_collecting_warnings(source, base_dir, runtime_vars)
mds::check_virtual(modules, entry, runtime_vars)
mds::check_virtual_collecting_warnings(modules, entry, runtime_vars)
```

### Consuming CompileResult

Two patterns for obtaining the payload:

```rust
// Pattern 1: match on output (non-consuming if you need warnings/deps too)
match result.output {
    CompiledOutput::Markdown(s) => { /* use s */ }
    CompiledOutput::Messages(msgs) => { /* use msgs: Vec<Message> */ }
}

// Pattern 2: typed helpers (consume result — warnings/deps are discarded)
let s: String = result.into_markdown()?;       // Err if Messages
let msgs: Vec<Message> = result.into_messages()?; // Err if Markdown
```

### MdsError Variants Added by Intrinsic Refactor

```rust
// code: mds::mixed_content
// help: "move all text and interpolations inside @message blocks…"
MixedContent { span: Option<SourceSpan>, src: Option<Arc<NamedSource<String>>> }

// code: mds::expected_markdown   (into_markdown called on Messages result)
ExpectedMarkdown

// code: mds::expected_messages   (into_messages called on Markdown result)
ExpectedMessages
```

Constructors (private to the crate):
- `MdsError::mixed_content()` — span-less version (used by evaluator in most cases)
- `MdsError::mixed_content_at(file, source, offset, len)` — span version (defined; not yet used by code paths in this PR)

### AST Change: TextNode.offset

```rust
pub struct TextNode { pub text: String, pub offset: usize }
```

The `offset` field is a byte offset from the start of the source. It exists for future diagnostic spans on mixed-content errors. All TextNode constructions use `offset: 0` unless the construction site has a real offset from lexer/parser output.

### Internal Intrinsic Dispatch (resolver.rs)

`process_module_intrinsic` is the internal resolver method called by all `compile*` functions. It:
1. Calls `has_message_block` on the module AST
2. Dispatches to `evaluate_messages_intrinsic` (messages path) or the existing markdown evaluator (markdown path)
3. `evaluate_messages_intrinsic` — strict; errors on orphan top-level Text/Interpolation; `EscapedBrace` is inert; `@include` in message context emits a warning

### Deleted Public Symbols

These are removed and must not be referenced anywhere:
- `CompileMessagesOutput` struct
- `CompileOutput` struct
- `compile_messages_str`, `compile_messages_str_with_deps`, `compile_messages_virtual`, `compile_messages_virtual_with_deps`, `compile_messages_file`, `compile_messages_file_with_deps`
- `ModuleCache::resolve_path_messages`, `resolve_key_messages`, `resolve_source_messages`

`evaluate_messages` (evaluator.rs) — was public; is now private/unused dead code from the deleted messages path.

### Internal ModuleCache Methods Added

```rust
pub fn resolve_path_intrinsic(&mut self, path, vars, warnings) -> Result<CompiledOutput, MdsError>
pub fn resolve_source_intrinsic(&mut self, source, base_dir, vars, warnings) -> Result<CompiledOutput, MdsError>
pub fn resolve_key_intrinsic(&mut self, key, vars, warnings) -> Result<CompiledOutput, MdsError>
```

These are what the `compile_*` lib.rs functions call internally.

## Filesystem Boundary (v0.5.0 Wave 1: #155, #265, #371, #408, #409)

Normative rules, codes and pinning tests: `spec.md` §4.6 "Filesystem constraints" and its "Forbidden-character enforcement" table. This section is the implementer's map.

### `FileSystem` trait (`fs.rs`) — BREAKING in v0.5.0 (#155)

- **Required**: `resolve_entry(path) -> Result<String>` (an entry path → its key; `NativeFs` returns the canonical absolute path, refuses a symlinked final component and anchors the project root on first call; `VirtualFs` returns the key UNCHANGED — no `.`/`..` collapsing, it must match a module-map key), `normalize_in_dir(dir, relative)` (imports; `dir` comes from `parent_dir(importer_key)`, `""` = key-space root), `parent_dir`, `read`, `is_markdown`.
- **Defaulted**: `anchor_base_dir(dir)` — identity by default (nothing anchored; right for an in-memory key-space). `NativeFs` = `canonical_dir` + `init_root`: canonicalizes, refuses a symlinked final component, anchors the project root FIRST-WRITER-WINS (a later call never moves it). `source_root()` — `None` by default.
- **Removed**: `normalize` (its `base == ""` branch is `resolve_entry`; the non-empty branch had no production caller), `canonicalize` and `set_root` (merged into `anchor_base_dir`; `set_root` anchored with no symlink check). Pinned by `filesystem_trait_required_methods_pin` (`IdentityFs`) and `filesystem_trait_removed_methods_pin` (method-probe trait) in `tests/api_surface.rs`.
- Callers: `ModuleCache::resolve_entry_key` (validate, then `fs.resolve_entry`) serves `resolve_path*`, `resolve_key` and `resolve_virtual_intrinsic*`; `ModuleCache::anchor_base` (forbidden-char check, then `fs.anchor_base_dir`) serves `resolve_source*`. The CLI's `read_canonical_source` (lint.rs, shared by `mds fmt`) PROPAGATES an anchor failure (`mds::io`, exit 2) — it used to be `let _ =` (PF-004). The two source-map finalize sites still call `let _ = self.fs.anchor_base_dir(ctx.base_dir)` as a defense-in-depth root guard when `source_root()` is `None`.
- `resolve_key` on `NativeFs` now validates, refuses a symlinked key and anchors the root, so the imports of a `resolve_key` entry are contained (before, no root was ever set). Pinned by `native_resolve_key_*` in `fs.rs`.

### Guards and where they run

- `validate_entry_path` (empty → NUL → forbidden char; all `mds::io`, path escaped) runs in the resolver for every backend AND again inside both built-in `resolve_entry`s, so a direct trait call is covered. NUL-in-entry was `mds::import` before v0.5.0.
- `validate_import_path` / `import_path_violation` (resolver; relative form → NUL → forbidden char, `mds::import`) runs before `normalize_in_dir` for every backend; `validate_relative_import` repeats empty/NUL/forbidden inside both built-in `normalize_in_dir`s. The frontmatter `imports:` parser uses `import_path_violation` so it reports the real reason (`imports[<n>]: invalid path "<p>": contains forbidden character U+XXXX`).
- `check_segment_count` (256, `mds::resource_limit`) on both backends' `resolve_entry` and on `NativeFs::normalize_in_dir`'s `relative`; `VirtualFs` counts segments after resolving against `dir` (`resolve_relative_segments`). `NativeFs` never enforced the documented cap before #155.
- `NativeFs::check_symlink_named`: canonicalize the PARENT, join the name as written, refuse when `symlink_metadata` (not followed) says symlink — on Windows every name-surrogate reparse point, so junctions too — then canonicalize and refuse a result whose parent is not the canonical parent (swap race), then `reject_forbidden_in_path` over the WHOLE canonical path. The file type decides, never a canonical-vs-joined string compare: that compare produced false "symlinks are not allowed" errors for case-mismatched names on case-insensitive volumes (#408). A module is keyed by its on-disk spelling.
- `NativeFs::canonical_dir` (#371): a filesystem root (`has_root() && parent().is_none()`) is canonicalized directly + `is_dir()`; everything else goes through `check_symlink_named`. It must never call `effective_parent` on a root — that maps to `"."` and silently re-anchors at the cwd.
- String compiles (`compile_str_with`/`check_str_with`/`lint_str_with`, bindings' `basePath`) pre-resolve the base dir in `lib.rs::resolve_base_dir` (typed-form forbidden check → `std::fs::canonicalize` → canonical-form forbidden check), so a symlinked base dir is FOLLOWED there; the base-dir symlink refusal applies only at `ModuleCache::resolve_source*` / trait level (spec F2 narrowing; `symlinked_base_dir_refused_by_resolve_source_followed_by_string_api`).

### Forbidden path characters (#265)

`mds::is_forbidden_path_char` (`lint/diagnostic.rs`) = `is_control_char` (78: C0 minus LF/TAB, DEL, C1, U+061C, U+200E/F, U+202A–E, U+2066–9, U+2028/9, U+FEFF) + LF + TAB = exactly 80 (`forbidden_path_char_class_is_exactly_80`). `mds::escape_path_for_message` = WIRE sanitize + TAB escaped (WIRE alone leaves TAB raw); its output carries none of the 80 (`escape_path_for_message_leaves_no_forbidden_char`). Messages are `<what> contains forbidden character U+XXXX: "<shown, escaped>"` where `<what>` is `import path` / `entry path` / `base directory` / `resolved path` / `path` (`check_symlink`) — built by `fs::forbidden_char_message`; `shown` is always what the caller typed, never the absolute resolved path. NUL keeps its own `contains null byte` message (checked first). The residual is a custom `with_fs` backend's OWN paths (keys it rewrites, links it follows): the trait's Security Contract requires it to apply `is_forbidden_path_char`. A project located under a hostile-named directory now fails entirely, by design.

### Windows verbatim paths (#409)

`verbatim::simplify_verbatim` (compiled on Windows only; string logic tested on every host) rewrites `\\?\C:\…` → `C:\…` and `\\?\UNC\server\share\…` → `\\server\share\…` only when lossless (≤ MAX_PATH, no reserved device name, no trailing dot/space, no `.`/`..` component). Two users: `native_dependencies` (the `CompileResult.dependencies` boundary of native compiles) and the public `mds::display_native_path` (a no-op off Windows; the CLI's `safe_path` routes every displayed path through it). Keys inside the resolver stay verbatim — containment compares canonical paths.

## `@extends` Evaluation (#114, #115)

- Every `@extends` path — markdown with source maps on (`evaluate_regions_with_map` with a `MapBuilder`), maps off (same function, `None` builder), an extending module reached through `@import` (`process_module_extends`), and messages mode (`evaluate_message_regions`) — evaluates the spliced regions (`spliced_regions(skeleton, effective_blocks, skeleton_origin)`) ONE REGION AT A TIME against the region's own `Origin` (`origin.display` + `origin.source`), so an error spans the file it is written in (base or child). Never pass `origin.file` to a diagnostic: it is the canonical key, absolute on `NativeFs`. `validate_extends_components` validates per region with `origin.display` too.
- `evaluator::EvalBudget { iterations, message_bytes }` (private fields, `Default`) is threaded `&mut` through `evaluate_seeded` / `evaluate_with_map_seeded` / `evaluate_messages_seeded`; one budget per module evaluation / extends chain, plus a cumulative output-size check after each region and one shared message vector in messages mode — a fresh budget per region would multiply every cap by the region count (PF-004). Pinned by `for_max_total_iterations_across_extends_regions_{source_map,maps_off,messages_mode,imported_module}`, `message_count_cumulative_across_regions`, `messages_total_bytes_cumulative_across_regions`, `output_size_cumulative_across_regions` (`tests/source_map_vfs.rs`).
- `splice_skeleton` and `ExtendsComponents::final_body` are deleted; `has_message_block` is decided across the regions.
- Known gap: an extending module reached through `@include` contributes no `FragmentMap` (`process_module_extends` sets `prompt_map: None`), so those output bytes carry no source-map segments.

## State Transitions

Template compilation follows: parse → resolve imports → evaluate → intrinsic-dispatch output wrapping. The intrinsic dispatch is a one-way gate: once `has_message_block` returns true the evaluator is `evaluate_messages_intrinsic` and any orphan text triggers `MixedContent`.

## Technical Implementation Patterns

### Test-only shims (not public API)

```rust
#[cfg(test)]
pub(crate) fn compile_str_md(source) -> Result<String, MdsError>
pub(crate) fn compile_str_with_md(source, base_dir, vars) -> Result<String, MdsError>
pub(crate) fn compile_virtual_md(modules, entry, vars) -> Result<String, MdsError>
```

These `pub(crate)` helpers exist so the large body of pre-intrinsic markdown unit tests can keep asserting on `String` without being rewritten. They compose the public `compile_*` functions with `.into_markdown()`.

### serde serialization of CompiledOutput

`CompiledOutput` uses adjacently-tagged serialization:

```rust
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum CompiledOutput { Markdown(String), Messages(Vec<Message>) }
```

JSON shape: `{"kind":"markdown","value":"..."}` or `{"kind":"messages","value":[...]}`. The NAPI layer does NOT use this derive — it builds the wire object field-by-field to control the payload key name (`output` vs `messages`, not `value`).

## Error Handling and Recovery

Mixed-content errors surface as `mds::mixed_content` from both `compile*` and `check*` paths. There is no lenient mode; the error is always fatal. The error carries a `help` string directing users to move text into `@message` blocks.

`ExpectedMarkdown` / `ExpectedMessages` are only produced by `into_markdown()` / `into_messages()` on a result whose kind doesn't match. These do not carry span or help text.

## Anti-Patterns

- **Calling `compile_messages_*` functions** — deleted; use `compile*` and match on `.output`
- **Checking `OutputFormat` enum** — deleted; the format is intrinsic
- **Constructing TextNode without `offset`** — must include `offset: 0` (or real offset); omitting causes a compile error since the struct field is public
- **Using `evaluate_messages` directly** — private; use `process_module_intrinsic` via the resolver
- **Calling `resolve_path_messages`** — deleted; use `resolve_path_intrinsic`

## Gotchas

- `check_*` functions now run full intrinsic dispatch including `has_message_block`. They will return `MixedContent` errors on templates that would have passed the old check (which used the markdown path only). This is a breaking behavior change.
- `dependencies` in `CompileResult` excludes the entry module. Downstream callers must not add the entry path manually.
- The `collect_messages_strict` internal name appears in some internal call chains; externally the only symbol exposed is `evaluate_messages_intrinsic`.
- The adjacently-tagged serde shape (`kind`/`value`) is for Rust-to-Rust serialization only. The NAPI wire format uses `output`/`messages` (not `value`) as the payload key — built explicitly in `build_canonical_result`.

## Key Files

- `crates/mds-core/src/lib.rs` — all public entry points; `CompileResult`, `CompiledOutput`, `Message` types
- `crates/mds-core/src/error.rs` — `MdsError` variants including `MixedContent`, `ExpectedMarkdown`, `ExpectedMessages`
- `crates/mds-core/src/resolver.rs` — `process_module_intrinsic`, `has_message_block`, `resolve_*_intrinsic`
- `crates/mds-core/src/evaluator.rs` — `evaluate_messages_intrinsic`, `EvalMessage`
- `crates/mds-core/src/ast.rs` — `TextNode` with `offset: usize` field
- `crates/mds-core/src/fs.rs` — `FileSystem` trait (Security Contract rustdoc), `NativeFs`, `VirtualFs`, shared path guards
- `crates/mds-core/src/verbatim.rs` — `simplify_verbatim` (Windows verbatim → conventional, lossless only)
- `crates/mds-core/tests/forbidden_path_chars.rs` — #265 import/entry/base-dir/custom-backend/symlinked-hostile-directory cases

## Related

- Feature: mds-cli — consumes `CompileResult` and `CompiledOutput`; derives output extension from `OutputKind::from(&compiled.output)`
- Feature: mds-napi — builds the canonical discriminated-union wire object from `CompileResult` via `build_canonical_result`
- Feature: mds-js — TypeScript `CompileResult = MarkdownResult | MessagesResult` union mirrors this Rust type
