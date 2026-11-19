---
feature: mds-compiler
name: MDS Compiler Core (mds-core)
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, working with @message blocks, the intrinsic output format, CompiledOutput, CompileResult, mixed-content errors, the FileSystem trait / path security (entry and import resolution, base-directory anchoring, forbidden path characters, Windows verbatim paths, bounded reads, module identity in error messages, virtual-filesystem module aliases), the output-size cap on evaluator buffers and replace(), lint severity parsing, or @extends region-by-region evaluation, its budgets and its source-map fragment sharing. Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, include, define, for, if, message, @message, CompiledOutput, CompileResult, into_markdown, into_messages, intrinsic, mixed_content, MixedContent, has_message_block, process_module_intrinsic, collect_messages_strict, evaluate_messages_intrinsic, TextNode.offset, FileSystem, NativeFs, VirtualFs, resolve_entry, normalize_in_dir, anchor_base_dir, parent_dir, source_root, validate_entry_path, validate_relative_import, validate_import_path, import_path_violation, resolve_entry_key, anchor_base, check_segment_count, MAX_PATH_SEGMENTS, check_symlink, check_symlink_named, check_directory, canonical_dir, init_root, resolve_base_dir, compile_source_in_dir, key_of, is_forbidden_path_char, escape_path_for_message, reject_forbidden_in_path, reject_forbidden_path, with_fs, display_native_path, native_dependencies, simplify_verbatim, verbatim.rs, EvalBudget, evaluate_seeded, evaluate_with_map_seeded, evaluate_messages_seeded, evaluate_regions_with_map, evaluate_message_regions, process_module_extends, validate_extends_components, spliced_regions, Origin, SourceTable, MapBuilder, FragmentMap, splice_fragment, into_fragment, fragment_map_from, prompt_body_and_map, is_visible_export, check_module_bytes, check_module_type, ModuleRef, ModuleKey, NotMdsFile, read_at_most, read_verified_capped, read_opened_capped, Capped, module_text, VarsFile, EntryPin, with_entry_pin, compile_native_fs, check_native_fs, lint_native_fs, scan_import_records, ImportRecord, ImportKind, ResolveAs, VirtualFs::with_aliases, ModuleAliasError, MAX_MODULE_ALIASES, push_capped, replace_result_len, replace_pieces, Severity, FromStr, ParseSeverityError, SeveritySpellings, parse_rule_severities."
category: domain-knowledge
directories: ["crates/mds-core/"]
referencedFiles:
  - crates/mds-core/src/lib.rs
  - crates/mds-core/src/evaluator.rs
  - crates/mds-core/src/evaluator/budget.rs
  - crates/mds-core/src/resolver.rs
  - crates/mds-core/src/error.rs
  - crates/mds-core/src/ast.rs
  - crates/mds-core/src/resolver/inheritance.rs
  - crates/mds-core/src/resolver/frontmatter.rs
  - crates/mds-core/src/limits.rs
  - crates/mds-core/src/scope.rs
  - crates/mds-core/src/value.rs
  - crates/mds-core/src/fs.rs
  - crates/mds-core/src/formatter.rs
  - crates/mds-core/src/verbatim.rs
  - crates/mds-core/src/options.rs
  - crates/mds-core/src/builtins.rs
  - crates/mds-core/src/sourcemap.rs
  - crates/mds-core/src/lint/config.rs
  - crates/mds-core/src/lint/diagnostic.rs
  - crates/mds-core/tests/forbidden_path_chars.rs
  - crates/mds-core/tests/api_surface.rs
  - crates/mds-core/tests/output_cap_funnel.rs
  - crates/mds-core/tests/memory_bound.rs
  - crates/mds-core/tests/read_bounds.rs
created: 2026-06-26
updated: 2026-11-19
---

# MDS Compiler Core (mds-core)

## Rules

- **KB-AP-1** Opening a vars file that was looked up as a regular file with a blocking `File::open` hangs when the path was swapped to a named pipe; use `ReadRule::open` (non-blocking, then a handle type check) and keep the FIFO-lookup case blocking. Details: Filesystem Boundary, Gotchas.
- **KB-INV-1** A not-MDS `@extends` target error carries the importing file's name and the directive's span, at the entry and at every intermediate base; never the entry's name or a canonical key. Details: `@extends` Evaluation.

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
mds::compile_virtual_fs(fs: VirtualFs, entry, runtime_vars, opts)
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
mds::check_virtual_fs(fs: VirtualFs, entry, runtime_vars) -> Result<Vec<String>, MdsError>  // Ok(warnings)
mds::check_virtual_collecting_warnings(modules, entry, runtime_vars)
```

`compile_virtual_fs(fs, entry, runtime_vars, opts: CompileOptions)`, `check_virtual_fs(fs, entry, runtime_vars)` and `lint_virtual_fs(fs, entry, runtime_vars, config: &LintConfig)` accept a caller-built `VirtualFs` (so a caller can seed it with `with_aliases`, see below); `compile_virtual`/`check_virtual`/`lint_virtual` are unchanged wrappers that build a plain `VirtualFs::new(modules)` and delegate to the `_fs` sibling.

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
- `mds::fix::apply_fixes` (removed in v0.5.0 Wave 1.5, #304; use `mds::fix::apply_fixes_incremental`, its `Fn`-closure incremental sibling — see mds-lint KB)

`evaluate_messages` (evaluator.rs) — was public; is now private/unused dead code from the deleted messages path.

### Internal ModuleCache Methods Added

```rust
pub fn resolve_path_intrinsic(&mut self, path, vars, warnings) -> Result<CompiledOutput, MdsError>
pub(crate) fn resolve_path_intrinsic_keyed(&mut self, path, vars, warnings) -> Result<(CompiledOutput, String), MdsError>
pub fn resolve_source_intrinsic(&mut self, source, base_dir, vars, warnings) -> Result<CompiledOutput, MdsError>
pub fn resolve_key_intrinsic(&mut self, key, vars, warnings) -> Result<CompiledOutput, MdsError>
```

`resolve_path_intrinsic_source` returns the compiled output and exact entry text from the check gate. `resolve_path_intrinsic` discards the text result. `mds::lint` delegates to `lint_native_fs`, which applies rules to that same entry text without another read.

## Filesystem Boundary (v0.5.0 Wave 1 + 1.5: #155, #265, #371, #408, #409, #413, #414, #415, #417, #428)

Normative rules, codes and pinning tests: `spec.md` §4.6 "Filesystem constraints" and its "Forbidden-character enforcement" table. This section is the implementer's map.

### `FileSystem` trait (`fs.rs`) — BREAKING in v0.5.0 (#155)

- **Required**: `resolve_entry(path) -> Result<String>` (an entry path → its key; `NativeFs` returns the canonical absolute path, refuses a symlinked final component and anchors the project root on first call; `VirtualFs` returns the key UNCHANGED — no `.`/`..` collapsing, it must match a module-map key), `normalize_in_dir(dir, relative)` (imports; `dir` comes from `parent_dir(importer_key)`, `""` = key-space root), `parent_dir`, `read`, `is_markdown`.
- **Defaulted**: `anchor_base_dir(dir)` — identity by default (nothing anchored; right for an in-memory key-space). `NativeFs` = `canonical_dir` + `init_root`: canonicalizes, refuses a symlinked final component, anchors the project root FIRST-WRITER-WINS (a later call never moves it). `source_root()` — `None` by default.
- **Removed**: `normalize` (its `base == ""` branch is `resolve_entry`; the non-empty branch had no production caller), `canonicalize` and `set_root` (merged into `anchor_base_dir`; `set_root` anchored with no symlink check). Pinned by `filesystem_trait_required_methods_pin` (`IdentityFs`) and `filesystem_trait_removed_methods_pin` (method-probe trait) in `tests/api_surface.rs`.
- Callers: `ModuleCache::resolve_entry_key` (validate, then `fs.resolve_entry`) serves `resolve_path*`, `resolve_key` and `resolve_virtual_intrinsic*`; `ModuleCache::anchor_base` (forbidden-char check, then `fs.anchor_base_dir`) serves `resolve_source*`. The CLI's `read_canonical_source` (lint.rs, shared by `mds fmt`) PROPAGATES an anchor failure (`mds::io`, exit 2) — it used to be `let _ =` (PF-004). The two source-map finalize sites call `ModuleCache::anchor_root_for_source_map` (c615ace), a defense-in-depth root anchor when `source_root()` is `None` that is BEST-EFFORT BY DESIGN, not an unfinished `?`: only a custom backend with no `source_root` of its own reaches it (NativeFs always has a root by then; the default anchor cannot fail), after it accepted the entry, so a refusal must not fail the compile — `source_root()` stays `None` and absolute sources degrade to basenames. Pinned by `source_map_root_safety_net_never_fails_a_compile` (resolver_tests.rs); switching either site to `?` fails it.
- `resolve_key` on `NativeFs` now validates, refuses a symlinked key and anchors the root, so the imports of a `resolve_key` entry are contained (before, no root was ever set). Pinned by `native_resolve_key_*` in `fs.rs`.

### Guards and where they run

- `validate_entry_path` (empty → NUL → forbidden char; all `mds::io`, path escaped) runs in the resolver for every backend AND again inside both built-in `resolve_entry`s, so a direct trait call is covered. NUL-in-entry was `mds::import` before v0.5.0.
- `validate_import_path` / `import_path_violation` (resolver; relative form → NUL → forbidden char, `mds::import`) runs before `normalize_in_dir` for every backend; `validate_relative_import` repeats empty/NUL/forbidden inside both built-in `normalize_in_dir`s. The frontmatter `imports:` parser uses `import_path_violation` so it reports the real reason (`imports[<n>]: invalid path "<p>": contains forbidden character U+XXXX`).
- `check_segment_count` (256, `mds::resource_limit`) on both backends' `resolve_entry` and on `NativeFs::normalize_in_dir`'s `relative`; `VirtualFs` counts segments after resolving against `dir` (`resolve_relative_segments`). `NativeFs` never enforced the documented cap before #155.
- `NativeFs::check_symlink_named`: canonicalize the PARENT, join the name as written, refuse when `symlink_metadata` (not followed) says symlink — on Windows every name-surrogate reparse point, so junctions too — then canonicalize and refuse a result whose parent is not the canonical parent (swap race), then `reject_forbidden_in_path` over the WHOLE canonical path. The file type decides, never a canonical-vs-joined string compare: that compare produced false "symlinks are not allowed" errors for case-mismatched names on case-insensitive volumes (#408). A module is keyed by its on-disk spelling.
- `NativeFs::canonical_dir` (#371): a filesystem root (`has_root() && parent().is_none()`) is canonicalized directly + `is_dir()`; everything else goes through `check_symlink_named`. It must never call `effective_parent` on a root — that maps to `"."` and silently re-anchors at the cwd. `check_directory` (#413, below) reuses this same root special-case.
- `NativeFs::normalize_in_dir_impl` refuses an import whose LAST component is `..` (`../`, `./sub/..`) as not found, on every OS (#414): such a path names a directory, never a file. Decided on the relative path AS WRITTEN before the `Path::join` — on Windows joining a `..`-suffixed relative onto a verbatim (`\\?\`) canonical `dir` would otherwise collapse the `..` lexically and silently name the directory itself. Previously Windows opened the directory the `..` resolved to and reported the OS's own I/O error (`cannot read .: Access is denied.`).
- `fs::key_of(canonical, shown)` (dc1135d, a P0 fix): every path `NativeFs` resolves — `resolve_entry`, `normalize_in_dir`, `anchor_base_dir` — is now keyed by the canonical path's EXACT UTF-8 string form; a canonical path that is not valid UTF-8 is refused (`mds::io`, `resolved path is not valid UTF-8: "<shown>"`) instead of being converted with `canonical.display().to_string()` (a LOSSY conversion that replaces each invalid byte sequence with U+FFFD and so names a DIFFERENT, valid-UTF-8 path — the "twin"). Before this fix every check (symlink, root containment, forbidden-character) ran against the REAL non-UTF-8 path, but the key stored and later `read()` opened the TWIN — reachable through a symlink into a directory whose name is not valid UTF-8 (Linux permits this; APFS/HFS+ do not, so the on-disk half of the regression test is Linux-CI-only). `key_of` is called and can fail BEFORE the project root is anchored in `resolve_entry` and `anchor_base_dir`, so a refused path never anchors it.
- String compiles (`compile_str_with`/`check_str_with`/`lint_str_with`, bindings' `basePath`) pre-resolve the base dir in `lib.rs::resolve_base_dir` (typed-form forbidden check → `std::fs::canonicalize` → canonical-form forbidden check), so a symlinked base dir is FOLLOWED there; the base-dir symlink refusal applies only at `ModuleCache::resolve_source*` / trait level (spec F2 narrowing; `symlinked_base_dir_refused_by_resolve_source_followed_by_string_api`). `resolve_base_dir` is `pub(crate)`, not private, specifically so `formatter.rs`'s safety gate can call it too (1cbe97f): `assert_equivalent` now resolves the base dir ONCE, up front — exactly as `check_str_with` does — via the new `compile_source_in_dir(source, dir, vars)` helper (the post-resolution half of `compile_str_collecting_warnings`, split out for this reuse), and propagates a resolution failure (a forbidden path character, or a directory that does not exist) as that `mds::io` error. Before this fix a refused/unresolvable base dir made `assert_equivalent`'s first compile `Err`, which its `Err(_) =>` arm reads as "does not compile standalone" and silently falls back to the weaker structural-equivalence check — so `mds fmt -` in a hostile-named working directory exited 0 where `mds check -` exited 2.

### Forbidden path characters (#265)

`mds::is_forbidden_path_char` (`lint/diagnostic.rs`) = `is_control_char` (78: C0 minus LF/TAB, DEL, C1, U+061C, U+200E/F, U+202A–E, U+2066–9, U+2028/9, U+FEFF) + LF + TAB = exactly 80 (`forbidden_path_char_class_is_exactly_80`). `mds::escape_path_for_message` = WIRE sanitize + TAB escaped (WIRE alone leaves TAB raw); its output carries none of the 80 (`escape_path_for_message_leaves_no_forbidden_char`). Messages are `<what> contains forbidden character U+XXXX: "<shown, escaped>"` where `<what>` is `import path` / `entry path` / `base directory` / `resolved path` / `path` (`check_symlink`) — built by `fs::forbidden_char_message`; `shown` is always what the caller typed, never the absolute resolved path. NUL keeps its own `contains null byte` message (checked first). The residual is a custom `with_fs` backend's OWN paths (keys it rewrites, links it follows): the trait's Security Contract requires it to apply `is_forbidden_path_char`. A project located under a hostile-named directory now fails entirely, by design. All three bindings' own options-parsing errors — a non-string or oversized `modules[...]` entry (napi, WASM, Python) and the WASM `filename_collision` error — now escape the caller-supplied module KEY through `mds::escape_path_for_message` too (a122cac), in the same `modules["<key>"]` form on all three (napi and WASM used to show it raw; Python showed a Rust `{key:?}` debug string). A per-construction-site census run at design time before #413/#414/#417 landed a code found five more sinks the same census pattern applies to (PF-033's third instance) — when adding a new path-carrying error, grep for every message-construction site rather than trusting one surface's test suite.

### Module bytes, bounded reads, and module identity (#417, #428)

- `mds::check_module_bytes(bytes: Vec<u8>, display: &str) -> Result<String, MdsError>` applies the shared size and UTF-8 checks. A leading BOM stays intact.
- `NativeFs::read` passes its verified capped result through `module_text` to `check_module_bytes`. The WASM pre-scanner calls the exported `preflightModule` wrapper.
- `mds::read_at_most(reader, limit, size_hint)` returns bytes through EOF or `limit`, whichever comes first. Capacity stays within `limit`; 64 consecutive Interrupted reads end the attempt.
- `read_opened_capped` uses the opened handle's size. It returns `Capped::TooLarge(size)` unread when that size exceeds the cap; otherwise it reads at most `cap + 1` bytes.
- `NativeFs` stores resolve records in `Mutex<HashMap<String, FileId>>`. Entry and import resolution record the identity from the final component's no-follow type lookup.
- `NativeFs::read` requires a record from the same instance. An absent record or poisoned record lock returns `mds::io`: `cannot read <display>: it was not resolved first`.
- `read_verified_capped` checks the path's type before open, then compares the opened handle before any byte read. Module sources must be regular files.
- Unix compares device and inode with the no-follow record. Windows compares `GetFinalPathNameByHandleW` with the recorded canonical path; other native targets refuse reads.
- A mismatch returns `mds::io`: `cannot read <display>: the file changed while it was opened`. Both fixed messages use the escaped root-relative display.
- `VarsFile::resolve` retains the canonical path, lookup identity and lookup type. `load_vars_file_reporting_duplicates` reads it through the same verified capped reader.
- Vars regular files receive the opened-file comparison. A FIFO at lookup and open skips the identity comparison, with the same 10 MiB cap.
- On Unix, a vars lookup that found a regular file opens through `ReadRule::open` with `O_NONBLOCK` (`rustix::fs::OFlags::NONBLOCK` via `OpenOptionsExt::custom_flags`), so a swap to a named pipe cannot wait for a writer. The opened handle is checked before any byte is read, and a non-regular handle returns the changed-file error. `check_before` also requires a regular file for a regular-file lookup, so a swap seen before open returns `mds::io` at once. A lookup that found a FIFO keeps the plain blocking open. Pinned by `vars_snapshot_refuses_a_regular_file_replaced_by_a_pipe_without_a_writer` and `vars_open_refuses_a_regular_file_replaced_by_a_pipe_after_metadata` (both fail on a 2 s timeout if the open blocks). `rustix` is a Unix-only `mds-core` dependency (workspace version) for this flag.
- Other vars types and type mismatches return the fixed changed-file error. Process substitution remains valid on macOS; Linux refuses `/dev/fd` links.
- Limits: imported-module reads can still select a same-named file from another directory. Windows accepts a replacement regular file at the same path.
- Some overlay or network filesystems report different lookup and handle identities and refuse reads. A source open can still wait indefinitely for a FIFO.
- A watch vars path can still read a same-named file from another directory.

### Native entry pins

- `EntryPin` is `#[non_exhaustive]`, with private state. `EntryPin::key` requires the canonical key; `with_walked` adds device and inode from walk metadata on Unix.
- `EntryPin::parent_dir` requires only the canonical parent. Watch uses this form so a save by rename or case-only name change can pass.
- `NativeFs::with_entry_pin` applies the pin only at entry resolution, before root setup. Imports use their own resolve records, not the entry pin.
- `compile_native_fs(fs, entry, vars, opts)`, `check_native_fs(fs, entry, vars)` and `lint_native_fs(fs, entry, vars, config)` accept a caller-built `NativeFs`.
- The path entry APIs delegate to these functions with `NativeFs::new()`. `CompileOptions` has no pin field; `ModuleCache::native()` keeps its signature.
- `lint_native_fs` applies its rules to the check gate's exact entry text. It does not re-read the entry.
- A walked file must match its canonical key and, on Unix, the walked identity. A watch entry must match its canonical parent.
- These pins do not freeze file content. A directory command can refuse a save by rename; a later run can accept it.

### Module identity in errors

- Module identity in errors, `mds::ModuleRef<'a> { key, typed }` (#417): built ONLY via the two-step `ModuleRef::keyed(key).typed(typed)` (`ModuleKey` is the `#[must_use]` intermediate) so a key and a caller-typed path can never be swapped by position. `key` is what every check/cache/read uses (on `NativeFs`, the canonical on-disk-cased path); `typed` only ever names the module in a message, escaped with `escape_path_for_message`. `mds::check_module_type(module: ModuleRef, source: &str)` judges the extension on `module.key` (so `Doc.MDS` typed for `doc.mds` still passes, #408) but on refusal names `module.typed` in `MdsError::NotMdsFile { path }` — which used to hold the resolved key (an absolute path on `NativeFs`, a host-path leak, CWE-209) and now holds only the escaped typed text. `check_module_type` is the resolver's not-an-MDS-file check, run after a module is read and before it is parsed; `@mdscript/mds`'s WASM pre-scanner runs the same check through `preflightModule` in the same place, so a non-MDS file is refused identically on both backends before any import-like line in it is followed.

### Import records and virtual module aliases (#414)

- `mds::scan_import_records(source, resolve_as: ResolveAs) -> Result<Vec<ImportRecord>, MdsError>` is `scan_imports`' superset: the same paths, but each an `ImportRecord { path, kind: ImportKind, frontmatter_index: Option<usize>, span: Option<SerializedSpan> }` (`#[non_exhaustive]`, obtained only from the function) carrying the CONTEXT the resolver attaches when resolving that path fails — an `@extends`/`@import` miss gets `mds::file_not_found` with `span` on the directive's whole line; a frontmatter miss gets `(in frontmatter imports[<i>])` via `frontmatter_index`; `@export … from` gets neither. `ImportKind` (`Extends`/`Frontmatter`/`Import`/`ExportFrom`, `.name()` → `extends`/`frontmatter`/`import`/`export-from`) and `ResolveAs` (`Standalone`/`Base`) are both `#[non_exhaustive]`. `ResolveAs::Standalone` lists the `@extends` base FIRST (`scan_imports`' existing order, now implemented as `scan_import_records(source, ResolveAs::Standalone)`); `ResolveAs::Base` lists it LAST — mirroring how the resolver's skeleton pass actually resolves a module reached as another's base (its own imports before its own `@extends`). `@mdscript/mds`'s WASM pre-scanner uses this to report an import failure it raises before its engine runs in the native resolver's own terms.
- `mds::VirtualFs::with_aliases(aliases: HashMap<String, String>) -> Result<Self, ModuleAliasError>` lets an import resolving to key `alias` reach the module keyed `aliases[alias]` instead — applied ONLY inside `normalize_in_dir` (the alias lookup happens after `resolve_relative_segments`); an ENTRY key is always taken as given, never redirected. Bounds are checked before any alias is validated: `MAX_MODULE_ALIASES` (65,536) aliases, `MAX_MODULE_ALIASES_SIZE` (10 MiB) total bytes of keys + targets. Then every alias key AND every module key it targets is checked as a key an import can resolve to (`module_key_violation`: non-empty, no forbidden character, ≤256 segments, no empty/`.`/`..` segment) — case-folding is lossy, so both the alias spelling and its target are validated independently — and no alias may itself be a module key (`alias_violation`). `ModuleAliasError` (`#[non_exhaustive]`): `TooMany{count}` / `TooLarge` (→ `mds::resource_limit`) checked first, then `Refused{alias, reason}` (→ `mds::io`, `module alias "<alias>": <reason>`) for the first offending alias in KEY ORDER — both fields already escaped. `@mdscript/mds`'s WASM backend uses this to key every spelling that reaches a module (a case variant, a path through a symlinked directory) to the ONE key the native backend would use, matching its module-identity model (#408).
- `compile_virtual_fs`/`check_virtual_fs`/`lint_virtual_fs` take a caller-built `VirtualFs` (so a caller seeds `with_aliases` before compiling); `compile_virtual_with_deps_opts`, `check_virtual_collecting_warnings` and `lint_virtual` are unchanged — each still builds a plain `VirtualFs::new(modules)` and calls its `_fs` sibling. Note `check_virtual_fs` takes no `opts`/`config` parameter at all (just `fs, entry, runtime_vars`), unlike its `compile_`/`lint_` siblings.

### Directory-argument check and forbidden-path reuse outside mds-core (#413)

- `mds::NativeFs::check_directory(path: &Path) -> Result<PathBuf, MdsError>` is the ONE directory-argument check now shared by the CLI's `build`/`check`/`fmt`/`lint`/`watch` (previously each had its own ad hoc logic). Accepts a path WITH a final name (`src`, `src/`, `src/.` — judged by `check_symlink_named`'s file-type rule, so a trailing `/` or `/.` never makes it follow a link) or WITHOUT one (`.`, `..`, `sub/..` — canonicalized as the OS resolves it, naming no link). Either way the canonical result must carry no forbidden path character and must be a directory (`not_a_directory`, `cannot resolve path <path>: not a directory`); every message names `path` as passed, never its canonical form. Reuses `canonical_dir`'s filesystem-root special case (#371).
- `mds::reject_forbidden_path(what: &str, path: &Path, typed: &str) -> Result<(), MdsError>` is a general-purpose forbidden-character check callers can run on a path mds-core never itself resolves — the CLI uses it for output locations (`-o`, `--out-dir`, `mds.json` `build.output_dir`) — so a refusal outside mds-core is worded identically to one inside it: `<what> contains forbidden character U+XXXX: "<typed>"`. `reject_forbidden_in_path` (crate-private) is `reject_forbidden_path("resolved path", ...)`, used wherever mds-core itself checks a canonical path.

### Windows verbatim paths (#409)

`verbatim::simplify_verbatim` (compiled on Windows only; string logic tested on every host) rewrites `\\?\C:\…` → `C:\…` and `\\?\UNC\server\share\…` → `\\server\share\…` only when lossless (≤ MAX_PATH, no reserved device name, no trailing dot/space, no `.`/`..` component). Two users: `native_dependencies` (the `CompileResult.dependencies` boundary of native compiles) and the public `mds::display_native_path` (a no-op off Windows; the CLI's `safe_path` routes every displayed path through it). Keys inside the resolver stay verbatim — containment compares canonical paths.

## Output cap per buffer, one memory budget per compile (#415, #420)

- Every String output accumulator appends through `evaluator::push_capped(out: &mut String, add: &str, mem: &mut MemBudget) -> Result<(), MdsError>` instead of a raw `push_str`. It checks `out.len() + add.len() <= MAX_OUTPUT_SIZE` BEFORE appending, so a `@for` loop — top-level, nested, inside `@define`/`@block`/a text-mode `@message`, in an `@extends` region, or in an imported module — stops on the PASS whose output would cross the cap, instead of building the whole loop body first (native surfaces used to allocate memory proportional to the whole oversized loop; WASM, once linear memory ran out, trapped with an uncoded `RuntimeError: unreachable`). Growth stays geometric (doubling, like `push_str`) but `reserve_exact`'s target is clamped to `MAX_OUTPUT_SIZE`, so capacity never balloons to ~2x the cap the way plain doubling would. The cap check runs first; then the growth is charged to the compile memory budget before `reserve_exact`, and any allocator excess after it, so a buffer's charge equals its `capacity()`.
- `builtin_replace` (the `replace()` template function, `builtins.rs`) computes the result's EXACT length first, via `replace_result_len`, which walks the same match-and-splice logic the build uses through one shared `replace_pieces` visitor callback (so length-counting and building can never disagree), and only allocates + builds when that length is `<= MAX_OUTPUT_SIZE`. A call that would exceed it (a one-character search string replaced by a long string, over many matches) now fails with `replace() output exceeds maximum size of <cap> bytes` WITHOUT ever building the oversized result; a fitting result is allocated once, at exactly its final length.
- **The output cap is per buffer; the memory budget is per compile** (PF-053). One evaluation has one output buffer: `@if`, `@for`, `@block`, `@include` and `@extends` regions write into the buffer of the evaluation that holds them. Only a `@define` call body, a messages-mode `@message` content and each module evaluation have their own buffer, and each of those has its own `MAX_OUTPUT_SIZE` (50 MiB) cap. The whole compile has one memory budget, `MAX_COMPILE_MEMORY` = 4 x `MAX_OUTPUT_SIZE` (209,715,200 B), kept in `MemBudget` (`evaluator/budget.rs`; `charge`, `mark`/`release_to`, `transfer`). It counts output buffers by capacity, the temporaries that evaluation makes (call results, built-in results, loop and call frames, the read module text, parsed frontmatter), and the parts that the `ModuleCache` keeps for each cached module (`retained`). A shared part is charged once, by the module that makes it. A charge comes before its allocation, except at a few named charge-after sites with a bounded overshoot. A charge past the budget fails with `mds::resource_limit`, message `compile memory exceeds maximum of 209715200 bytes`, no span (`MdsError::compile_memory_exceeded`, the only constructor). The budget resets at each root resolution, but `retained` never resets: a reused `ModuleCache` keeps counting its cached modules and eventually refuses calls. Outside the budget: the syntax tree, tokens, caller variables, the line table, the YAML event buffer and binding output conversion.
- Each frontmatter parse in a compile has a byte allowance (`Allowance::in_compile`): `MAX_OUTPUT_SIZE`, or the rest of the compile budget when that is smaller. Each YAML node spends `NODE_YAML` (192) bytes plus its string and key bytes, before the node is built. Past 50 MiB the parse fails with `frontmatter YAML expands beyond maximum of 52428800 bytes` (`MdsError::frontmatter_expansion_exceeded`); past a smaller rest it fails with the compile memory message. Lint and the import scans parse with the plain 50 MiB allowance. Each module parses its frontmatter once.
- At root completion, `finish_root` checks that no temporary is still charged and that no counter underflowed. A failed check returns `mds::internal`, message `internal compiler error` (`MdsError::Internal`), instead of the result. `retain_module` makes the same checks and skips the cache insert when one fails, so a later cache hit cannot serve that module. Every surface maps that code (the bindings list it next to their own panic case).
- Neither output-cap message TEXT changed — the fix moved WHEN the check runs, not its wording — so a regression test must assert the resource the fix bounds (iterations reached, peak bytes under a counting allocator; `tests/memory_bound.rs` allows the budget plus an 8 MiB slack), never the message alone. `tests/output_cap_funnel.rs` is a mechanised SOURCE-SCAN gate (not a runtime test). `output_cap_is_funnelled_through_one_helper_and_one_constructor` scans the bodies of the three accumulators — `evaluate_nodes` (one `push_capped` call) and `evaluate_for` (none) in `evaluator.rs`, `evaluate_regions_with_map` (none) in `resolver.rs` — and fails on a raw append there (`push_str`, `.push(`, `+=`, `write!`, …). It also checks, across all of `src/` (not `*_tests.rs`, `#[cfg(test)]` items or comments), that each of the three messages (output size, compile memory, frontmatter allowance) is built only in its `error.rs` constructor. `builtins.rs` is covered only through that message anchor: its `replace()` and `builtin_output_too_large` messages carry the output-size words mid-literal, and the gate needs at least two such unanchored hits outside `error.rs`. The pre-size-then-allocate shape of `replace()` is not scanned; runtime tests pin it. `budget_rules_apply_to_each_source_file` bans the four cfg keys that can select a wasm32 target (`target_arch`, `target_family`, `target_os`, `target_pointer_width`; plain `cfg(unix)` and `cfg(windows)` stay allowed) in the listed core files — `evaluator.rs`, `resolver.rs`, `builtins.rs`, `sourcemap.rs`, `lib.rs`, `scope.rs`, `value.rs`, `resolver/frontmatter.rs`, `resolver/inheritance.rs` and `evaluator/budget.rs` — and bans panics, `unwrap`/`expect`, asserts and `RefCell` in `evaluator/budget.rs`; `budget_gates_reject_planted_text` is its planted control, with one planted line per banned cfg key.

## Severity has one parser, shared by every surface (#175, #418)

- `Severity` (`lint/diagnostic.rs`) hand-implements `serde::Deserialize` by deserializing to `String` first and parsing it with `FromStr` — so `mds.json`'s `lint.rules` values and every binding's `rules` map accept exactly the same four spellings, with NO case folding, trimming or escape decoding (`"Warn"` and `" warn"` are both rejected; napi/WASM used to decode escapes before comparing, so spelling the leading letter as a JSON unicode escape (U+0077, followed by "arn") configured `warn`). `Severity::ALL = [Off, Info, Warn, Error]` is what both `FromStr` and the spelling-list `Display` helper (`SeveritySpellings`) iterate; `as_str()`/`Display` print the one canonical spelling. `ParseSeverityError` (`#[non_exhaustive]`, carries no copy of the rejected input, so its own message is always safe to print raw) is the sole error `str::parse::<Severity>()` returns: `unknown severity; expected "off", "info", "warn", or "error"`.
- `mds::parse_rule_severities(rules: serde_json::Map<String, Value>, field: &str) -> Result<HashMap<String, Severity>, String>` (`options.rs`) is the shared reader for a whole `rules` map: napi, WASM and Python each convert their caller's `rules` option to a `serde_json::Map` and call this once, so all three read a severity — and word an error about it — identically; only the wrapping error TYPE differs per binding. Errors name the OFFENDING RULE (map order) and WIRE-escape both the rule name and the bad value with `sanitize_control_chars_wire` before interpolating (a rule name is caller-controlled and JSON `\uXXXX` decodes to real control bytes — CWE-150/CWE-117 if left raw): `<field>["<name>"] must be a severity string, got <type>` for a non-string value, `<field>["<name>"]: unknown severity "<value>"; expected "off", "info", "warn", or "error"` otherwise. `field` is the caller's own name for the map (`"options.rules"` on napi/WASM, `"rules"` on Python, `"lint.rules"` for the CLI's `mds.json` reader) — shown verbatim, never escaped.
- Both `parse_rule_severities` and `Severity`'s `Display`/`Deserialize` are `#[inline]` specifically so each binding compiles its own copy at its own optimization level: mds-wasm builds for SIZE, mds-core at opt-level 3, and inlining these measured several KB smaller in the WASM binary than calling into an opt-level-3 copy compiled once in mds-core. Do not remove the attribute as dead-looking cruft.

## `@define` captures share values and scope entries

- Frame zero looks up variables in this order: its own map, the runtime layer, then the frontmatter layer. Each public module resolution call creates one runtime layer for its modules and captures. A capture reuses its own variable map snapshot until the write generation changes. Each slot shares its root value.
- A define captures its scope before it enters that scope. A capture freezes pending function and namespace entries into one new link only when those maps have writes. The chain reuses earlier links. Lookup starts with the newest link. The live frame keeps flat maps for direct lookup and name checks.
- A scope can have at most 256 capture links. The resolver counts local `@define` declarations, not re-exports. The count includes declarations inside `@if`, `@elseif`, `@else`, `@for`, `@block`, `@message` and `@define` bodies. It returns `mds::resource_limit` for declaration 257 before it checks the name or makes a capture.
- A call frame points to the callee. Lookup checks parameters first, then the definition-site capture, then the caller's frames. It does not copy the captured entries.
- Function and namespace lookup probe each distinct capture link at most once per lookup. A link already checked for this name also marks its older links as checked.
- A variable read shares a root `Arc<Value>`. A dot path or loop variable adds checked key or index steps to a slot. Each step uses one level of the root value. The path has no custom `Drop` because its depth cannot exceed the root value's depth.
- The compiler releases the capture graph with a worklist, not recursive calls. A shared node contributes its outgoing edges only when its last owner releases it.
- A resolved module shares its prompt text and export-filtered namespace across imports. The compile memory budget charges the parts that a cached module keeps (prompt body, frontmatter text and tree, captured scope layers, source-map fragments) once, when the module is cached; an import that shares them adds no charge. `get_prompt_value` still returns an owned value, and production code calls neither it nor `to_namespace` (`internal_prompt_producers_use_shared_accessors` in `resolver_tests.rs` scans for them). `@include` still copies its text into the output through `push_capped`, so the copy counts against the 50 MiB cap of the receiving buffer and against the compile budget.

## `@extends` Evaluation (#114, #115, #412, #416)

- Every `@extends` path — markdown with source maps on (`evaluate_regions_with_map` with a `MapBuilder`), maps off (same function, `None` builder), an extending module reached through `@import` or `@include` (both go through the same `process_module_extends`), and messages mode (`evaluate_message_regions`) — evaluates the spliced regions (`spliced_regions(skeleton, effective_blocks, skeleton_origin)`) ONE REGION AT A TIME against the region's own `Origin` (`origin.display` + `origin.source`), so an error spans the file it is written in (base or child). Never pass `origin.file` to a diagnostic: it is the canonical key, absolute on `NativeFs`. `validate_extends_components` validates per region with `origin.display` too.
- **Fixed (#412), previously a known gap**: an extending module reached through `@include` now contributes a `FragmentMap` too, under the SAME gate a standalone module's evaluation uses — `prompt_body_and_map`/`fragment_map_from` are shared between `process_module` and `process_module_extends`, so `process_module_extends` seeds a `MapBuilder` (`seed = (source_map_mode && is_visible_export(..., "prompt")).then_some(&skeleton_origin)`) whenever source maps are on and `prompt` is export-visible, exactly as it would for a plain module. An `@include` of such a module now maps each byte it contributes back to the root base, intermediate, or child file that wrote it; that text used to carry no mappings at all (a direct `@extends` compile of the same file was already mapped — only the `@include`d path was blind). `fragment_map_from` still degrades to `None` when the segment cap was hit mid-build (a partial map would misattribute) or the body is empty (PF-034 — an `@include` of it adds no text).
- `MapBuilder`'s `SourceTable::intern` shares each distinct source's `Arc<str>` key/display/source text (three refcount bumps, no text COPY) rather than copying a region's file text on every splice (#416): a source-mapped `@extends` chain used to copy a region's source text once PER SPLICED REGION (every top-level node of the root base is its own region), so cost grew with the SQUARE of the base's size — a 262,144-node base took 68s with maps on vs 0.6s with them off in a debug build; now ~1.2s, byte-identical maps. `MapBuilder::splice_fragment` caches each `Arc<FragmentMap>`'s local→global source-index remap keyed by the fragment's raw `Arc` pointer, so K `@include`s of the same module (in a loop, or from several sites) register its sources once and only rebase segments on every later splice. A `FragmentMap` segment naming no fragment source is a compiler bug: `debug_assert!` stops on it in debug builds; release silently drops the segment (`rebased` returns `None`) rather than misattribute it — the same fail-safe `expand_per_line` already uses for an unresolvable segment. A file first registered through an `@include` splice now keeps the ROOT-RELATIVE display path it was loaded with, rather than the absolute canonical key the builder used to substitute as the display name (a CWE-209 leak no released version could reach — an included module's map never carried a file with its own `@extends` region until #412 made that possible).
- `ModuleCache` resets `EvalBudget` at the start of each compile. The function call count applies across all evaluated modules and regions. The loop and message-byte counts reset before each module evaluation, not before each region. All three seeded evaluator entry points use this budget. Messages mode shares one message vector across regions. In markdown mode the regions write into one buffer, so `push_capped` checks the cumulative output size at each append, and the `MemBudget` in `EvalBudget` charges that buffer's capacity to the compile memory budget. The retained total of cached modules does not reset with `EvalBudget`.
- `splice_skeleton` and `ExtendsComponents::final_body` are deleted; `has_message_block` is decided across the regions.
- A not-MDS `@extends` target keeps the importing file's name and the directive's span at an intermediate base too, not only at the entry: the error names the file that wrote the directive (`sub/c.mds`), not the entry. The intrinsic compile and the cached `resolve_path` give the same error. Pinned by `not_mds_intermediate_extends_keeps_the_base_importer_span` (`resolver_tests.rs`), which also compiles the matching MDS chain as a control.

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

`MdsError::NotMdsFile { path }` (#417) holds display text only — the entry path or virtual entry key the caller passed, or the `@import`/`@export … from`/frontmatter/`@extends` string as written, already escaped with `escape_path_for_message`. It never holds the resolved key (an absolute path on `NativeFs`). Code that used the old field to LOCATE the file must resolve the path itself; whether a file is an MDS file is still judged on the resolved file's content and on-disk extension (`check_module_type`).

## Anti-Patterns

- **Calling `compile_messages_*` functions** — deleted; use `compile*` and match on `.output`
- **Checking `OutputFormat` enum** — deleted; the format is intrinsic
- **Constructing TextNode without `offset`** — must include `offset: 0` (or real offset); omitting causes a compile error since the struct field is public
- **Using `evaluate_messages` directly** — private; use `process_module_intrinsic` via the resolver
- **Calling `resolve_path_messages`** — deleted; use `resolve_path_intrinsic`
- **Calling `mds::fix::apply_fixes`** — deleted (#304); use `apply_fixes_incremental`
- **Building a `ModuleRef` with a struct literal or pairing `key`/`typed` by position** — always go through `ModuleRef::keyed(key).typed(typed)`; the type is `#[non_exhaustive]` and the two-step builder exists precisely so the compiler catches a swapped pair
- **Appending to an evaluator output buffer with raw `push_str`** — bypasses the per-append `MAX_OUTPUT_SIZE` check; always go through `push_capped`, and for an amplifying built-in, compute the result's length first the way `builtin_replace` does
- **Treating `VirtualFs::with_aliases` as applying to entry resolution** — it only affects `normalize_in_dir` (imports); an entry key is always taken as given, unaliased

## Gotchas

- `check_*` functions now run full intrinsic dispatch including `has_message_block`. They will return `MixedContent` errors on templates that would have passed the old check (which used the markdown path only). This is a breaking behavior change.
- `dependencies` in `CompileResult` excludes the entry module. Downstream callers must not add the entry path manually.
- The `collect_messages_strict` internal name appears in some internal call chains; externally the only symbol exposed is `evaluate_messages_intrinsic`.
- The adjacently-tagged serde shape (`kind`/`value`) is for Rust-to-Rust serialization only. The NAPI wire format uses `output`/`messages` (not `value`) as the payload key — built explicitly in `build_canonical_result`.
- The output-size cap (`MAX_OUTPUT_SIZE`) bounds each output BUFFER, not a whole compile (PF-053). `@if`, `@for`, `@block`, `@include` and `@extends` regions share the buffer of their evaluation; a `@define` call body, a messages-mode `@message` content and each module evaluation have their own buffer and their own 50 MiB cap. The whole-compile bound is the compile memory budget (`MAX_COMPILE_MEMORY`, 200 MiB, #420), and it bounds only the parts it counts (output buffers by capacity, evaluation temporaries, the parts that the `ModuleCache` keeps) plus the bounded charge-after overshoots. A peak-memory test for one compile asserts the budget plus a slack (`tests/memory_bound.rs`: 8 MiB), never the 50 MiB cap.
- `ModuleAliasError`'s bound checks run in a fixed order — `TooMany` before `TooLarge` before per-alias `Refused` — so a map that is simultaneously too large in count AND holds a bad alias always reports the count error first, never the alias.
- `scan_import_records`'s `ResolveAs::Base` ordering (imports before the module's own `@extends`) does NOT match `scan_imports`'/`ResolveAs::Standalone`'s ordering (`@extends` first); pick the variant that matches how the module is actually being reached, not always `Standalone`.
- A vars path swapped from a regular file to a FIFO after lookup is refused at once only because the regular-file open is non-blocking; do not replace `ReadRule::open` with `File::open` for that case, and do not add `O_NONBLOCK` to the FIFO-lookup or module opens (a FIFO vars input stays supported, and a source open can still wait for a FIFO).
- `Severity`'s `FromStr` is intentionally strict (no case-fold, no trim): a `mds.json` or binding-option value that a human typed with different casing (`"Warn"`) is a hard parse error, not silently accepted — this is a deliberate breaking change from napi/WASM's prior escape-decoding behavior, not an oversight.

## Key Files

- `crates/mds-core/src/lib.rs` — all public entry points; `CompileResult`, `CompiledOutput`, `Message` types, `scan_import_records`, `ImportRecord`/`ImportKind`/`ResolveAs`, `load_vars_file_reporting_duplicates`
- `crates/mds-core/src/error.rs` — `MdsError` variants including `MixedContent`, `ExpectedMarkdown`, `ExpectedMessages`, `NotMdsFile`; `LineColumns` (pub(crate), line/column lookup shared with `scan_import_records`'s span attachment)
- `crates/mds-core/src/resolver.rs` — `process_module_intrinsic`, `has_message_block`, `resolve_*_intrinsic*`, `process_module_extends`, `ModuleRef`/`ModuleKey`, `check_module_type`, `fragment_map_from`, `prompt_body_and_map`, `is_visible_export`
- `crates/mds-core/src/evaluator.rs` — `evaluate_messages_intrinsic`, `EvalMessage`, `EvalBudget`, `push_capped`
- `crates/mds-core/src/builtins.rs` — `builtin_replace`, `replace_result_len`, `replace_pieces` (output-cap pre-sizing, #415)
- `crates/mds-core/src/ast.rs` — `TextNode` with `offset: usize` field
- `crates/mds-core/src/fs.rs` — `FileSystem` trait (Security Contract rustdoc), `NativeFs`, `VirtualFs` (+ `with_aliases`, `ModuleAliasError`), `EntryPin`, `VarsFile`, `check_module_bytes`, `read_at_most`/`read_verified_capped` (+ `ReadRule::open`, non-blocking vars open on Unix), `check_directory`, `reject_forbidden_path`, shared path guards
- `crates/mds-core/src/options.rs` — `parse_rule_severities`, `json_type_name`, `format_unknown_keys_error`
- `crates/mds-core/src/sourcemap.rs` — `SourceTable`, `MapBuilder`, `FragmentMap`, `Origin`
- `crates/mds-core/src/lint/diagnostic.rs` — `Severity`, `ParseSeverityError`, `SeveritySpellings`, `is_forbidden_path_char`, `escape_path_for_message`
- `crates/mds-core/src/verbatim.rs` — `simplify_verbatim` (Windows verbatim → conventional, lossless only)
- `crates/mds-core/tests/forbidden_path_chars.rs` — #265 import/entry/base-dir/custom-backend/symlinked-hostile-directory cases
- `crates/mds-core/tests/api_surface.rs` — `FileSystem` trait-shape pins, `check_module_bytes`/`check_module_type`/`scan_import_records`/`VirtualFs` alias/`check_directory`/`read_at_most` existence pins
- `crates/mds-core/src/evaluator/budget.rs` — `MemBudget`, `Part`, `value_bytes`/`display_len`: the compile memory budget (#420)
- `crates/mds-core/tests/output_cap_funnel.rs` — mechanised source-scan gate: the three accumulators in `evaluator.rs` and `resolver.rs` append only through `push_capped`; the output-size, compile-memory and frontmatter-allowance messages are built only in their `error.rs` constructors; the listed core files (`resolver/inheritance.rs` included) use none of the four cfg keys that can select a wasm32 target (`target_arch`, `target_family`, `target_os`, `target_pointer_width`), and the budget module has no panic or `RefCell` (#415, #420)
- `crates/mds-core/tests/memory_bound.rs` — counting-allocator peak tests for compile shapes; the limit is the compile budget plus an 8 MiB slack (#420)
- `crates/mds-core/tests/read_bounds.rs` — the ONE `#[test]` in this binary (a process-wide counting global allocator cannot share the process with a second test): pins peak heap growth for a module read at roughly `MAX_FILE_SIZE + 1` byte plus a fixed 256 KiB slack for paths/display/error overhead — never the whole oversized file (#428)

## Related

- Feature: mds-cli — consumes `CompileResult` and `CompiledOutput`; derives output extension from `OutputKind::from(&compiled.output)`; its own directory-argument and output-path refusals now call `NativeFs::check_directory`/`reject_forbidden_path` (#413) and its `lint.rules` reader calls `parse_rule_severities` (#175/#418)
- Feature: mds-napi — builds the canonical discriminated-union wire object from `CompileResult` via `build_canonical_result`; its `rules` option now reads through `parse_rule_severities`
- Feature: mds-js — TypeScript `CompileResult = MarkdownResult | MessagesResult` union mirrors this Rust type; its WASM-backend file pre-scanner mirrors `check_module_bytes`/`check_module_type`/`scan_import_records`/`VirtualFs::with_aliases` via `preflightModule`/`scanImportRecords`/`moduleAliases` (#414)
- Feature: mds-lint — `Severity`, `parse_rule_severities` and `apply_fixes_incremental` live here; consult it for the fix-tier and reverify-gate model
- Feature: source-map-security — `relativize_source`/`source_root` path-relativization choke point; this KB's `SourceTable`/`MapBuilder`/`FragmentMap` section (#416) covers the source-SHARING model that choke point's callers rely on
- PF-004: an alternate code path silently bypassing a resource/security check — cited above for the pre-#155 `let _ =` anchor-failure swallow and for why `EvalBudget` is one-per-chain, not one-per-region
- PF-033: a host-path leak fixed on one surface stays live on its siblings — cited above for `NotMdsFile`'s pre-#417 shape and the design-time census that found #413/#414/#417's remaining sinks before they shipped
- PF-053: a resource bound constrains only the axis it counts, at the layer where it runs — cited above for why the output cap is per-buffer, not per-compile, and why capacity (not just length) must be bounded
