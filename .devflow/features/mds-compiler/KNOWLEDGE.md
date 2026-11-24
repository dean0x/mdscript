---
feature: mds-compiler
name: MDS Compiler Core (mds-core)
description: "Use when changing the MDS compile pipeline: directives, scope, frontmatter, reserved keys, imports, @extends, local data sources, path security, output caps. Keywords: resolver, read_data, MAX_DATA_FILES."
category: domain-knowledge
directories: ["crates/mds-core/"]
referencedFiles:
  - crates/mds-core/src/lib.rs
  - crates/mds-core/src/evaluator.rs
  - crates/mds-core/src/evaluator/budget.rs
  - crates/mds-core/src/resolver.rs
  - crates/mds-core/src/resolver
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
  - crates/mds-core/src/lexer.rs
  - crates/mds-core/src/escape_class.rs
  - crates/mds-core/src/lint/facts.rs
  - crates/mds-core/tests/forbidden_path_chars.rs
  - crates/mds-core/tests/api_surface.rs
  - crates/mds-core/tests/output_cap_funnel.rs
  - crates/mds-core/tests/memory_bound.rs
  - crates/mds-core/tests/reserved_key_form.rs
  - crates/mds-core/tests/read_bounds.rs
  - crates/mds-core/src/data
  - crates/mds-core/src/resolver/module_scope.rs
  - crates/mds-core/src/resolver/process.rs
  - crates/mds-core/src/resolver/charges.rs
  - crates/mds-core/tests/data_limits.rs
  - crates/mds-core/tests/data_paths.rs
  - crates/mds-core/tests/fixtures/data-path-vectors.json
  - crates/mds-core/tests/yaml_funnel.rs
created: 2026-06-26
updated: 2026-11-24
---

# MDS Compiler Core (mds-core)

## Rules

- **KB-AP-1** Opening a vars file that was looked up as a regular file with a blocking `File::open` hangs when the path was swapped to a named pipe; use `ReadRule::open` (non-blocking, then a handle type check) and keep the FIFO-lookup case blocking. Details: Filesystem Boundary, Gotchas.
- **KB-INV-1** A not-MDS `@extends` target error carries the importing file's name and the directive's span, at the entry and at every intermediate base; never the entry's name or a canonical key. Details: `@extends` Evaluation.
- **KB-INV-2** The lexer reads a conditional line (`@if`, `@elseif`, `@else`, `@end`) inside a fence or at an indent only through `Lexer::line_class`. It gives `Plain` unless the fence has the `+mds` marker, or (outside a fence) the file key is `true`. The conditional stack is per fence: an outer `@if` never closes or continues in a fence. Details: Fence Opt-In and Indented Directives.
- **KB-INV-3** `indented-directives` is per file and is never inherited. The lexer probe and the YAML parse must agree on it, or the compile fails. Details: Fence Opt-In and Indented Directives.
- **KB-AP-2** Add a new reserved top-level frontmatter key at only some of the reserved-key sites (the `RESERVED_OUTPUT_KEYS` table in `lib.rs`, which drives the stripper, the parse proof, the form messages and the merged-output check; `RESERVED_MERGE_KEYS` in `resolver/frontmatter.rs`; `build_scope_from_frontmatter` in `resolver/module_scope.rs`; `RESERVED` in `lint/facts.rs`; for data, `RESERVED_NAMES` in `data/decl.rs` and `is_scope_key` in `data/resolve.rs`): the key then leaks into scope, merge, output frontmatter, `unused-variable` or the data entry names. Details: Anti-Patterns.
- **KB-AP-3** Add a lexer branch that leaves `pos` unchanged: `Lexer::run` returns `mds::internal` when a step does not advance. Details: Anti-Patterns.
- **KB-AP-4** Expect a backslash before `@` in a `+mds` fence to be removed: the line stays literal and the backslash stays in the output. Details: Gotchas.
- **KB-AP-5** Expect the merged `@extends` frontmatter to go through the stripper, or `fmt` to fall back to a structural check on a reserved-key form error: the merged text is emitted as is, a reserved key in it is `mds::internal`, and `fmt` returns the form error. Details: Gotchas.
- **KB-AP-6** Change how `type: mds` is read in one place only: `.md` module detection (`is_type_mds_line`) and the output stripper (`ReservedOutputKey::matches_line`) are separate scanners and differ on a trailing comment. Details: Gotchas.
- **KB-AP-7** Move the data extension rule into `check_key` (`data/path.rs`): a compile runs `check_key` on the resolved key before `canonical_file` reports a key with no data extension as `mds::data`, so the move changes that code to `mds::import`. A direct read gets the rule from `check_data_key`, which both built-in `read_data` call. Details: Local Data Sources.
- **KB-AP-8** Count a new data limit in or after the stage that does the work it bounds: each data cap runs before its stage (entry and reference caps at declaration, before the graph; the compile entry and level caps before any read; the file cap before a new read; the byte total before conversion). `tests/data_limits.rs` pins each order. Details: Local Data Sources.
- **KB-AP-9** Check a data path only as written, or add a path rule as a unit test only: the segment and containment rules run again on the exact resolved key, and a new rule is a row in the shared vector table `tests/fixtures/data-path-vectors.json`. Details: Local Data Sources.
- **KB-AP-10** Parse data JSON or YAML outside `resolver/frontmatter.rs`: `tests/yaml_funnel.rs` permits the parse entry points only there (and `serde_json` also in `vars_json.rs`), so the node and memory caps apply to every data parse. Details: Local Data Sources.
- **KB-INV-4** Only `ModuleCache::compiling` (the compile entry points) reads data files. `ModuleCache::tooling` (check, lint, fmt) reads none and marks unreplayed entries pending; a plain `ModuleCache` reports one as `mds::undefined_var`. Details: Local Data Sources.
- **KB-INV-5** No data file is read before the whole-graph preflight passes over every module. An unreplayed URL entry gives `mds::network_denied` before any local read; a kept graph refusal blocks every read, but the first fault that the compile meets itself wins. Details: Local Data Sources.
- **KB-INV-6** A `@message` role never depends on data: `expr_reads_data` must see each expression form that can carry a data value. Details: Local Data Sources.

## Overview

`mds-core` is the Rust library crate at `crates/mds-core/`. Every other layer (CLI, NAPI bindings, WASM bindings) calls into it. It compiles `.mds` template files to either Markdown or structured chat-message arrays — the distinction is **intrinsic** to the template: the presence of any `@message` block causes the compiler to produce `CompiledOutput::Messages`; templates without `@message` blocks always produce `CompiledOutput::Markdown`. Callers do not specify a format at call time.

The pipeline is: lexer → parser → validator → resolver (imports, inheritance, frontmatter) → evaluator → output wrapping. All error codes carry the `mds::` prefix (e.g. `mds::mixed_content`, `mds::syntax`, `mds::undefined_variable`).

## Business Context

The intrinsic output model replaced a previous `--format messages` flag and a separate `compile_messages_*` API family. Those symbols are deleted. The key invariant: a template is either a "markdown template" or a "messages template" — it cannot be both. Top-level text content intermixed with `@message` blocks is a hard error (`mds::mixed_content`), not a warning.

## Core Business Rules

### Intrinsic Output Rule

The compiled output kind is determined by `process_module_intrinsic` in `resolver/process.rs`. It calls `has_message_block` on the resolved AST; if any `@message` node exists at the entry module level the output is Messages, otherwise Markdown.

- `CompiledOutput::Markdown(String)` — no `@message` blocks in template
- `CompiledOutput::Messages(Vec<Message>)` — at least one `@message` block

Mixed content (top-level text/interpolation AND `@message` blocks in the same module) is rejected with `MdsError::MixedContent`.

### CompileResult — the only public output type

Every `compile*` entry point returns `Result<CompileResult, MdsError>`:

```rust
pub struct CompileResult {
    pub output: CompiledOutput,   // Markdown(String) or Messages(Vec<Message>)
    pub warnings: Vec<String>,
    pub dependencies: Vec<String>, // modules depth-first, then data files read; excludes entry
}

impl CompileResult {
    pub fn into_markdown(self) -> Result<String, MdsError>  // Err: ExpectedMarkdown
    pub fn into_messages(self) -> Result<Vec<Message>, MdsError> // Err: ExpectedMessages
}
```

`dependencies` lists the modules in first-resolution (depth-first) order, then the local data files that the compile read, in first-read order (`ModuleCache::dependencies`). It excludes the entry module itself. A data file that a module import also names is listed once, with the modules; a replayed entry and a URL entry add nothing.

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

### Internal Intrinsic Dispatch (`resolver/process.rs`)

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
- **Defaulted**: `anchor_base_dir(dir)` — identity by default (nothing anchored; right for an in-memory key-space). `NativeFs` = `canonical_dir` + `init_root`: canonicalizes, refuses a symlinked final component, anchors the project root FIRST-WRITER-WINS (a later call never moves it). `source_root()` — `None` by default. `read_data(key)` — the default returns `mds::io` (`this file system does not read data files`) and never falls back to `read`; both built-in backends override it (see Local Data Sources).
- **Removed**: `normalize` (its `base == ""` branch is `resolve_entry`; the non-empty branch had no production caller), `canonicalize` and `set_root` (merged into `anchor_base_dir`; `set_root` anchored with no symlink check). Pinned by `filesystem_trait_required_methods_pin` (`IdentityFs`) and `filesystem_trait_removed_methods_pin` (method-probe trait) in `tests/api_surface.rs`.
- Callers: `ModuleCache::resolve_entry_key` (validate, then `fs.resolve_entry`) serves `resolve_path*`, `resolve_key` and `resolve_virtual_intrinsic*`; `ModuleCache::anchor_base` (forbidden-char check, then `fs.anchor_base_dir`) serves `resolve_source*`. The CLI's `read_canonical_source` (lint.rs, shared by `mds fmt`) PROPAGATES an anchor failure (`mds::io`, exit 2) — it used to be `let _ =`. The two source-map finalize sites call `ModuleCache::anchor_root_for_source_map` (c615ace), a defense-in-depth root anchor when `source_root()` is `None` that is BEST-EFFORT BY DESIGN, not an unfinished `?`: only a custom backend with no `source_root` of its own reaches it (NativeFs always has a root by then; the default anchor cannot fail), after it accepted the entry, so a refusal must not fail the compile — `source_root()` stays `None` and absolute sources degrade to basenames. Pinned by `source_map_root_safety_net_never_fails_a_compile` (resolver_tests.rs); switching either site to `?` fails it.
- `resolve_key` on `NativeFs` now validates, refuses a symlinked key and anchors the root, so the imports of a `resolve_key` entry are contained (before, no root was ever set). Pinned by `native_resolve_key_*` in `fs.rs`.

### Guards and where they run

- `validate_entry_path` (empty → NUL → forbidden char; all `mds::io`, path escaped) runs in the resolver for every backend AND again inside both built-in `resolve_entry`s, so a direct trait call is covered. NUL-in-entry was `mds::import` before v0.5.0.
- `validate_import_path` / `import_path_violation` (`resolver/import_paths.rs`; relative form → NUL → forbidden char, `mds::import`) runs before `normalize_in_dir` for every backend; `validate_relative_import` repeats empty/NUL/forbidden inside both built-in `normalize_in_dir`s. The frontmatter `imports:` parser uses `import_path_violation` so it reports the real reason (`imports[<n>]: invalid path "<p>": contains forbidden character U+XXXX`).
- `check_segment_count` (256, `mds::resource_limit`) on both backends' `resolve_entry` and on `NativeFs::normalize_in_dir`'s `relative`; `VirtualFs` counts segments after resolving against `dir` (`resolve_relative_segments`). `NativeFs` never enforced the documented cap before #155.
- `NativeFs::check_symlink_named`: canonicalize the PARENT, join the name as written, refuse when `symlink_metadata` (not followed) says symlink — on Windows every name-surrogate reparse point, so junctions too — then canonicalize and refuse a result whose parent is not the canonical parent (swap race), then `reject_forbidden_in_path` over the WHOLE canonical path. The file type decides, never a canonical-vs-joined string compare: that compare produced false "symlinks are not allowed" errors for case-mismatched names on case-insensitive volumes (#408). A module is keyed by its on-disk spelling.
- `NativeFs::canonical_dir` (#371): a filesystem root (`has_root() && parent().is_none()`) is canonicalized directly + `is_dir()`; everything else goes through `check_symlink_named`. It must never call `effective_parent` on a root — that maps to `"."` and silently re-anchors at the cwd. `check_directory` (#413, below) reuses this same root special-case.
- `NativeFs::normalize_in_dir_impl` refuses an import whose LAST component is `..` (`../`, `./sub/..`) as not found, on every OS (#414): such a path names a directory, never a file. Decided on the relative path AS WRITTEN before the `Path::join` — on Windows joining a `..`-suffixed relative onto a verbatim (`\\?\`) canonical `dir` would otherwise collapse the `..` lexically and silently name the directory itself. Previously Windows opened the directory the `..` resolved to and reported the OS's own I/O error (`cannot read .: Access is denied.`).
- `fs::key_of(canonical, shown)` (dc1135d, a P0 fix): every path `NativeFs` resolves — `resolve_entry`, `normalize_in_dir`, `anchor_base_dir` — is now keyed by the canonical path's EXACT UTF-8 string form; a canonical path that is not valid UTF-8 is refused (`mds::io`, `resolved path is not valid UTF-8: "<shown>"`) instead of being converted with `canonical.display().to_string()` (a LOSSY conversion that replaces each invalid byte sequence with U+FFFD and so names a DIFFERENT, valid-UTF-8 path — the "twin"). Before this fix every check (symlink, root containment, forbidden-character) ran against the REAL non-UTF-8 path, but the key stored and later `read()` opened the TWIN — reachable through a symlink into a directory whose name is not valid UTF-8 (Linux permits this; APFS/HFS+ do not, so the on-disk half of the regression test is Linux-CI-only). `key_of` is called and can fail BEFORE the project root is anchored in `resolve_entry` and `anchor_base_dir`, so a refused path never anchors it.
- String compiles (`compile_str_with`/`check_str_with`/`lint_str_with`, bindings' `basePath`) pre-resolve the base dir in `lib.rs::resolve_base_dir` (typed-form forbidden check → `std::fs::canonicalize` → canonical-form forbidden check), so a symlinked base dir is FOLLOWED there; the base-dir symlink refusal applies only at `ModuleCache::resolve_source*` / trait level (spec F2 narrowing; `symlinked_base_dir_refused_by_resolve_source_followed_by_string_api`). `resolve_base_dir` is `pub(crate)`, not private, specifically so `formatter.rs`'s safety gate can call it too (1cbe97f): `assert_equivalent` now resolves the base dir ONCE, up front — exactly as `check_str_with` does — via the new `compile_source_in_dir(source, dir, vars)` helper (the post-resolution half of `compile_str_collecting_warnings`, split out for this reuse), and propagates a resolution failure (a forbidden path character, or a directory that does not exist) as that `mds::io` error. Before this fix a refused/unresolvable base dir made `assert_equivalent`'s first compile `Err`, which its `Err(_) =>` arm reads as "does not compile standalone" and silently falls back to the weaker structural-equivalence check — so `mds fmt -` in a hostile-named working directory exited 0 where `mds check -` exited 2.

### Forbidden path characters (#265)

`mds::is_forbidden_path_char` (`lint/diagnostic.rs`) = `is_control_char` (78: C0 minus LF/TAB, DEL, C1, U+061C, U+200E/F, U+202A–E, U+2066–9, U+2028/9, U+FEFF) + LF + TAB = exactly 80 (`forbidden_path_char_class_is_exactly_80`). `mds::escape_path_for_message` = WIRE sanitize + TAB escaped (WIRE alone leaves TAB raw); its output carries none of the 80 (`escape_path_for_message_leaves_no_forbidden_char`). Messages are `<what> contains forbidden character U+XXXX: "<shown, escaped>"` where `<what>` is `import path` / `entry path` / `base directory` / `resolved path` / `path` (`check_symlink`) — built by `fs::forbidden_char_message`; `shown` is always what the caller typed, never the absolute resolved path. NUL keeps its own `contains null byte` message (checked first). The residual is a custom `with_fs` backend's OWN paths (keys it rewrites, links it follows): the trait's Security Contract requires it to apply `is_forbidden_path_char`. A project located under a hostile-named directory now fails entirely, by design. All three bindings' own options-parsing errors — a non-string or oversized `modules[...]` entry (napi, WASM, Python) and the WASM `filename_collision` error — now escape the caller-supplied module KEY through `mds::escape_path_for_message` too (a122cac), in the same `modules["<key>"]` form on all three (napi and WASM used to show it raw; Python showed a Rust `{key:?}` debug string). A per-construction-site census run at design time before #413/#414/#417 landed a code found five more sinks the same census pattern applies to — when adding a new path-carrying error, grep for every message-construction site rather than trusting one surface's test suite.

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

- `mds::scan_import_records(source, resolve_as: ResolveAs) -> Result<Vec<ImportRecord>, MdsError>` is `scan_imports`' superset: the same paths, but each an `ImportRecord { path, kind: ImportKind, frontmatter_index: Option<usize>, span: Option<SerializedSpan> }` (`#[non_exhaustive]`, obtained only from the function) carrying the CONTEXT the resolver attaches when resolving that path fails — an `@extends`/`@import` miss gets `mds::file_not_found` with `span` on the directive's whole line; a frontmatter miss gets `(in frontmatter imports[<i>])` via `frontmatter_index`; `@export … from` gets neither. `ImportKind` (`Extends`/`Frontmatter`/`Import`/`ExportFrom`/`Data`, `.name()` → `extends`/`frontmatter`/`import`/`export-from`/`data`; a `Data` record is the local file of a `data:` entry, listed after every module record with no span and no frontmatter index, and `scan_imports` drops it) and `ResolveAs` (`Standalone`/`Base`) are both `#[non_exhaustive]`. `ResolveAs::Standalone` lists the `@extends` base FIRST (`scan_imports`' existing order, now implemented as `scan_import_records(source, ResolveAs::Standalone)`); `ResolveAs::Base` lists it LAST — mirroring how the resolver's skeleton pass actually resolves a module reached as another's base (its own imports before its own `@extends`). `@mdscript/mds`'s WASM pre-scanner uses this to report an import failure it raises before its engine runs in the native resolver's own terms.
- `mds::VirtualFs::with_aliases(aliases: HashMap<String, String>) -> Result<Self, ModuleAliasError>` lets an import resolving to key `alias` reach the module keyed `aliases[alias]` instead — applied ONLY inside `normalize_in_dir` (the alias lookup happens after `resolve_relative_segments`); an ENTRY key is always taken as given, never redirected. Bounds are checked before any alias is validated: `MAX_MODULE_ALIASES` (65,536) aliases, `MAX_MODULE_ALIASES_SIZE` (10 MiB) total bytes of keys + targets. Then every alias key AND every module key it targets is checked as a key an import can resolve to (`module_key_violation`: non-empty, no forbidden character, ≤256 segments, no empty/`.`/`..` segment) — case-folding is lossy, so both the alias spelling and its target are validated independently — and no alias may itself be a module key (`alias_violation`). `ModuleAliasError` (`#[non_exhaustive]`): `TooMany{count}` / `TooLarge` (→ `mds::resource_limit`) checked first, then `Refused{alias, reason}` (→ `mds::io`, `module alias "<alias>": <reason>`) for the first offending alias in KEY ORDER — both fields already escaped. `@mdscript/mds`'s WASM backend uses this to key every spelling that reaches a module (a case variant, a path through a symlinked directory) to the ONE key the native backend would use, matching its module-identity model (#408).
- `compile_virtual_fs`/`check_virtual_fs`/`lint_virtual_fs` take a caller-built `VirtualFs` (so a caller seeds `with_aliases` before compiling); `compile_virtual_with_deps_opts`, `check_virtual_collecting_warnings` and `lint_virtual` are unchanged — each still builds a plain `VirtualFs::new(modules)` and calls its `_fs` sibling. Note `check_virtual_fs` takes no `opts`/`config` parameter at all (just `fs, entry, runtime_vars`), unlike its `compile_`/`lint_` siblings.

### Directory-argument check and forbidden-path reuse outside mds-core (#413)

- `mds::NativeFs::check_directory(path: &Path) -> Result<PathBuf, MdsError>` is the ONE directory-argument check now shared by the CLI's `build`/`check`/`fmt`/`lint`/`watch` (previously each had its own ad hoc logic). Accepts a path WITH a final name (`src`, `src/`, `src/.` — judged by `check_symlink_named`'s file-type rule, so a trailing `/` or `/.` never makes it follow a link) or WITHOUT one (`.`, `..`, `sub/..` — canonicalized as the OS resolves it, naming no link). Either way the canonical result must carry no forbidden path character and must be a directory (`not_a_directory`, `cannot resolve path <path>: not a directory`); every message names `path` as passed, never its canonical form. Reuses `canonical_dir`'s filesystem-root special case (#371).
- `mds::reject_forbidden_path(what: &str, path: &Path, typed: &str) -> Result<(), MdsError>` is a general-purpose forbidden-character check callers can run on a path mds-core never itself resolves — the CLI uses it for output locations (`-o`, `--out-dir`, `mds.json` `build.output_dir`) — so a refusal outside mds-core is worded identically to one inside it: `<what> contains forbidden character U+XXXX: "<typed>"`. `reject_forbidden_in_path` (crate-private) is `reject_forbidden_path("resolved path", ...)`, used wherever mds-core itself checks a canonical path.

### Windows verbatim paths (#409)

`verbatim::simplify_verbatim` (compiled on Windows only; string logic tested on every host) rewrites `\\?\C:\…` → `C:\…` and `\\?\UNC\server\share\…` → `\\server\share\…` only when lossless (≤ MAX_PATH, no reserved device name, no trailing dot/space, no `.`/`..` component). Two users: `native_dependencies` (the `CompileResult.dependencies` boundary of native compiles) and the public `mds::display_native_path` (a no-op off Windows; the CLI's `safe_path` routes every displayed path through it). Keys inside the resolver stay verbatim — containment compares canonical paths.

## Output cap per buffer, one memory budget per compile (#415, #420, #488)

- Every String output accumulator appends through `evaluator::push_capped(out: &mut String, add: &str, mem: &mut MemBudget) -> Result<(), MdsError>` instead of a raw `push_str`. It checks `out.len() + add.len() <= MAX_OUTPUT_SIZE` BEFORE appending, so a `@for` loop — top-level, nested, inside `@define`/`@block`/a text-mode `@message`, in an `@extends` region, or in an imported module — stops on the PASS whose output would cross the cap, instead of building the whole loop body first (native surfaces used to allocate memory proportional to the whole oversized loop; WASM, once linear memory ran out, trapped with an uncoded `RuntimeError: unreachable`). Growth stays geometric (doubling, like `push_str`) but `reserve_exact`'s target is clamped to `MAX_OUTPUT_SIZE`, so capacity never balloons to ~2x the cap the way plain doubling would. The cap check runs first; then the growth is charged to the compile memory budget before `reserve_exact`, and any allocator excess after it, so a buffer's charge equals its `capacity()`.
- `builtin_replace` (the `replace()` template function, `builtins.rs`) computes the result's EXACT length first, via `replace_result_len`, which walks the same match-and-splice logic the build uses through one shared `replace_pieces` visitor callback (so length-counting and building can never disagree), and only allocates + builds when that length is `<= MAX_OUTPUT_SIZE`. A call that would exceed it (a one-character search string replaced by a long string, over many matches) now fails with `replace() output exceeds maximum size of <cap> bytes` WITHOUT ever building the oversized result; a fitting result is allocated once, at exactly its final length.
- **The output cap is per buffer; the memory budget is per compile**. One evaluation has one output buffer: `@if`, `@for`, `@block`, `@include` and `@extends` regions write into the buffer of the evaluation that holds them. Only a `@define` call body, a messages-mode `@message` content and each module evaluation have their own buffer, and each of those has its own `MAX_OUTPUT_SIZE` (50 MiB) cap. The whole compile has one memory budget, `MAX_COMPILE_MEMORY` = 4 x `MAX_OUTPUT_SIZE` (209,715,200 B), kept in `MemBudget` (`evaluator/budget.rs`; `charge`, `mark`/`release_to`, `transfer`). It counts output buffers by capacity, the temporaries that evaluation makes (call results, built-in results, loop and call frames, the read module text, parsed frontmatter), and the parts that the `ModuleCache` keeps for each cached module (`retained`). A shared part is charged once, by the module that makes it. A charge comes before its allocation, except at a few named charge-after sites with a bounded overshoot. A charge past the budget fails with `mds::resource_limit`, message `compile memory exceeds maximum of 209715200 bytes`, no span (`MdsError::compile_memory_exceeded`, the only constructor). The budget resets at each root resolution, but `retained` never resets: a reused `ModuleCache` keeps counting its cached modules and eventually refuses calls. Since #488 it also counts a syntax tree estimate for each module and the compiler's copy of the caller variables (next two bullets). Outside the budget: the lexer stage of each source, the caller's own variable map, the line table, the YAML event buffer and binding output conversion.
- **Syntax tree estimate (#488).** In the compile and check families, `parse_charged` (`resolver/charges.rs`) runs `tokenize`, charges `tree_estimate` as `Part::Temp`, then parses, so a refused tree is never built. The tokens drop after the parse. The import scans (`scan_imports`, `scan_import_records`, and so the WASM `scanImports` / `scanImportRecords`, the Python `scan_imports` and the `@mdscript/mds` WASM file pre-scan) call `charge_scan_tree` (`resolver.rs`) between `tokenize` and `parse_with_ctx`. It charges the same `tree_estimate` as `Part::Temp` against a new `MemBudget` that holds nothing else, so a scan of a module that does not fit fails with the same compile memory error, and on WASM it is a coded error, not a trap. The estimate is `tokens x TREE_PER_TOKEN (832) + expression bytes x TREE_PER_EXPR_BYTE (184) + source bytes x TREE_PER_TEXT_BYTE (16)` (`limits.rs`), in checked arithmetic (`tree_bytes`: a count past the address space fails the charge). Expression bytes are the text of the `Token::Directive` and `Token::Interpolation` tokens. Counted trees: a cached import (`process_module`), an `@extends` base (`process_module_skeleton`), the file entry and the string entry (`process_module_intrinsic`, a temporary until the module closes). A cached module keeps the charge (`keep`, then `retained`). The skeleton upgrade keeps the charge of the skeleton entry, so its new tree stays a temporary and counts once. The K values are measured on a 64-bit host (arm64): `assert_syntax_tree_peaks` in `tests/memory_bound.rs` asserts estimate >= max(entry peak, import peak, kept bytes) for 65 shapes, and `tree_factors` reads the K values from the text of `limits.rs`, so the test and the constants can not drift. On wasm32 the 64-bit K values overcount, which fails closed. Ceilings for a module compiled alone (one compile / the sum of all sources that a reused `ModuleCache` keeps in its life): densest syntax `\{{a` 484,330 / 484,610 B, so a dense module of about 484 KB or more is refused; `{{a}}` 952,385 / 956,729 B; the densest measured expression shape (alternating fence content and directives) 616,935 / 618,527 B; prose with one `{{a}}` per 80 B 5,229,805 / 5,237,969 B; prose 10,485,760 B (the 10 MiB file cap limits first) / 12,336,188 B. A reused `ModuleCache` fills faster, because each cached module keeps its estimate until the cache is dropped. The refusal is `mds::resource_limit`, `compile memory exceeds maximum of 209715200 bytes`. Known limits: the lexer stage (`Vec<char>` and `byte_offsets`, 12 B per ASCII character, plus the tokens) runs before the charge point and is outside the budget: measured 14 B per source byte on 1 MiB of ASCII prose, and at most 56 B per source byte for the densest syntax (`LEXER_PEAK_PER_BYTE`, asserted for the lexer and for a refused module).
- **Caller-variable copy (#488).** `VarLayer` (`scope.rs`) counts its bytes while it is collected (`FromIterator`: name capacity + `value_bytes` per entry + `table_bytes`); `bytes()` returns the count, and `insert` sets it to `None`, so a changed layer fails the charge. `start_resolution` charges it once per compile, as `Part::Buffers`, at the root, so it stays out of `retained` and out of every module mark. A refused layer skips the read charge (`open_module`). The caller's own map is outside the budget. Known limits: the copy is counted, not removed (a removal from the public `ModuleCache` methods breaks the Rust API). On a root cache hit, the copy is made and freed without a charge. On a reused cache, a layer that a cached function captures lives after its compile, and only that compile counts it. `value_bytes` stops at 64 levels (`MAX_VALUE_DEPTH`), so a Rust caller `Value` nested deeper than 64 levels fails the variable charge with the compile memory error, not with a depth error; the bindings and the CLI admit at most 64 levels. A test pins this (`a_caller_value_past_the_counted_depth_is_refused_by_the_layer_charge`), and the path errors past 64 steps (`mds::syntax`, `mds::undefined_var`) stay pinned for a value put in a scope directly.
- Each frontmatter parse in a compile has a byte allowance (`Allowance::in_compile`): `MAX_OUTPUT_SIZE`, or the rest of the compile budget when that is smaller. Each YAML node spends `NODE_YAML` (192) bytes plus its string and key bytes, before the node is built. The one exception is the bytes of a `!tag`: `visit_enum` spends them after it copies the tag, and the copy is at most the source size. Past 50 MiB the parse fails with `frontmatter YAML expands beyond maximum of 52428800 bytes` (`MdsError::frontmatter_expansion_exceeded`); past a smaller rest it fails with the compile memory message. Lint and the import scans parse with the plain 50 MiB allowance. Each module parses its frontmatter once.
- At root completion, `finish_root` checks that no temporary is still charged and that no counter underflowed. A failed check returns `mds::internal`, message `internal compiler error` (`MdsError::Internal`), instead of the result. `retain_module` makes the same checks and skips the cache insert when one fails, so a later cache hit cannot serve that module. Every surface maps that code (the bindings list it next to their own panic case).
- Neither output-cap message TEXT changed — the fix moved WHEN the check runs, not its wording — so a regression test must assert the resource the fix bounds (iterations reached, peak bytes under a counting allocator; `tests/memory_bound.rs` allows the budget plus an 8 MiB slack), never the message alone. `tests/output_cap_funnel.rs` is a mechanised SOURCE-SCAN gate (not a runtime test). `output_cap_is_funnelled_through_one_helper_and_one_constructor` scans the bodies of the three accumulators — `evaluate_nodes` (one `push_capped` call) and `evaluate_for` (none) in `evaluator.rs`, `evaluate_regions_with_map` (none) in `resolver.rs` — and fails on a raw append there (`push_str`, `.push(`, `+=`, `write!`, …). It also checks, across all of `src/` (not `*_tests.rs`, `#[cfg(test)]` items or comments), that each of the three messages (output size, compile memory, frontmatter allowance) is built only in its `error.rs` constructor. `builtins.rs` is covered only through that message anchor: its `replace()` and `builtin_output_too_large` messages carry the output-size words mid-literal, and the gate needs at least two such unanchored hits outside `error.rs`. The pre-size-then-allocate shape of `replace()` is not scanned; runtime tests pin it. `budget_rules_apply_to_each_source_file` bans the text of four cfg key names (`target_arch`, `target_family`, `target_os`, `target_pointer_width`) in the listed core files — `evaluator.rs`, `resolver.rs`, `builtins.rs`, `sourcemap.rs`, `lib.rs`, `scope.rs`, `value.rs`, `limits.rs`, all `resolver/` children, and `evaluator/budget.rs`. `cfg(unix)`, `cfg(windows)`, `cfg(not(windows))` and the other target keys (`target_env`, `target_vendor`, …) stay allowed. `cfg(not(windows))` is true on wasm32, and `lib.rs` uses it for `native_dependencies`, so the gate does not prove that the listed code is the same on each target. In the code of `evaluator/budget.rs`, the gate also bans `RefCell` and these panic tokens: `panic`, `unreachable`, `todo`, `unimplemented`, `unwrap`, `expect` and the assert macros. It does not find other code that can panic, such as an index. `budget_gates_reject_planted_text` is its planted control: one planted line per banned cfg key, one for `limits.rs` through the file list, one for each of `assert`, `unreachable`, `todo` and `unimplemented`, and a control that `cfg(unix)`, `cfg(windows)` and `cfg(not(windows))` pass.

## Local Data Sources (`data:` frontmatter, #405)

Normative text: `spec.md` §4.12 "Data sources", the "Local data limits" of §4.1 and the data path row of the §4.6 filesystem table; `SECURITY.md` "Local data files". This section is the implementer's map. Code: `src/data/` (`decl.rs` declarations, `refs.rs` reference scan, `path.rs` path rules, `graph.rs` dependency graph, `resolve.rs` chain, bind and read, `convert.rs` parse), called from `resolver/process.rs` (`prepare_data`, the preflight), `resolver/module_scope.rs` (`bind_data`) and `resolver/charges.rs` (the compile counters).

### Declarations
- A module declares entries in a top-level `data:` mapping. `decl::parse_declarations` reads the bounded frontmatter tree and does no I/O. An entry name must be an identifier and not in `RESERVED_NAMES`. A value has one of three forms: a `./` or `../` string (`Source::File`); an `https://` URL string; or a request map with `url` and optional `method`, `accept`, `headers`, `body`, `cache` (`Source::Url`, which keeps only the paths of its `{{…}}` references, for the graph). A file path cannot hold `{{`. An unquoted `{{x}}` parses as a YAML map, and `check_quote` refuses it with help to quote the value.
- `scan_import_records` and lint `collect_data_entries` call the same parser and ignore its errors: the compile reports them, with the module name.
- With `@extends`, `Declarations::extend` builds a chain from child to base (`ModuleCtx::data_chain`). The nearest declaration of a name wins: a child entry replaces the whole base entry, and the base file is not read (`child_replaces_the_whole_entry_and_the_base_file_is_not_read`). Each file path resolves from the directory of the module that declares it. The frontmatter merge drops `data` at depth 0, so the chain carries the entries, not the merged frontmatter.
- A winning entry name that is also a frontmatter key or an import alias in the chain is `mds::name_collision` (`decl::check_collisions`, before the graph).

### Data paths and the two `read_data` backends
- Compile order for one file entry: `path::normalize` checks the path as written (`validate_relative_import`; the `./` or `../` prefix; no `{{`; the exact extension of `mds::is_data_path`, `.json`, `.yaml` or `.yml` byte for byte; the segment rules: no segment that starts with `.` other than `.` and `..`, no `node_modules` in any ASCII case, and on Windows a trailing dot or space does not hide one). Then `fs.normalize_in_dir` (symlink and root checks) gives the key, and `check_key` runs forbidden characters, containment below `source_root()` and the segment rules again on that exact key, relative to the root. Last, `resolve::canonical_file` maps the key's extension to a parser; a resolved key with no data extension, or a written `.json` that resolves to YAML or the reverse, is `mds::data`.
- A path refusal is `mds::import`, `data path "<shown>": <reason>`. `<shown>` is the path as written, or the root-relative display on `NativeFs`, never the absolute key.
- `check_data_key` is `check_key` plus the extension rule. `VirtualFs::read_data` (no root, so the key must be relative) and `NativeFs::read_data` (with its root) call it before the read, so a caller that gives a key directly gets the rules of a compile (`read_data_checks_a_key_that_skipped_the_data_path_rule`, `virtual_read_data_refuses_a_key_without_a_data_extension`, `native_read_data_refuses_a_resolved_key_without_a_data_extension` in `tests/data_paths.rs`). The extension rule stays out of `check_key` because of the compile order above (KB-AP-7).
- `NativeFs::read_data` uses `read_text` with `ReadRule::Data`: it reads only a key that this instance resolved, opens without a wait on a named pipe, compares the open handle with the resolve record, and reads at most `MAX_DATA_FILE_BYTES` of UTF-8. A virtual miss is `MdsError::DataFileNotFound` (code `mds::module_not_found`, `data file '<key>' is not in the virtual module map`); a native miss is `mds::file_not_found`.
- The shared vector table `tests/fixtures/data-path-vectors.json` (`portable` and `windows` rows: path, expected code, reason) drives `native_paths_follow_the_shared_portable_table` in `src/data/path_tests.rs`, and `spec.md` cites it; the Windows forms run only in the Windows CI job.
- Reach: a data path can name any `.json`, `.yaml` or `.yml` file that these rules permit below the project root (the nearest directory with `.git` or `.mdsroot`), not only files near the template, and a template can put that content in its output. `SECURITY.md` states this reach; change the two together.

### Entry-point roles
- The compile entry points (`compile*`, `compile_native_fs`, `compile_virtual_fs` and their wrappers) use `ModuleCache::compiling` (`data.compile` and `analyze_data` on). They read and bind local files. An unreplayed URL entry fails the compile with `mds::network_denied`: this release sends no network request.
- `check*`, `lint*`, `fmt` and the lint fix verify use `ModuleCache::tooling` (`analyze_data` only). They read no data file (WASM `check_and_lint_read_no_data_file`, napi `D-6`, `tooling_checks_pending_variables_without_opening_data` in `tests/data_tooling.rs`). Unreplayed entries become pending names, the warning `data pending; output was not evaluated: <entries>` is added, and module evaluation is skipped while `data.pending` is set. `tooling_output_in_dir` returns `None` then, so `fmt` and the fix verify do not compare output.
- A plain `ModuleCache` (`new`, `native`, `virtual_fs`, `with_fs`) has neither flag: an unreplayed entry is `mds::undefined_var`, and `dependencies()` lists modules only.
- A runtime variable with the entry's name replays it in every role: the graph marks it replayed, and no file or URL is used.

### Preflight, graph and first-fault order
- `prepare_data` runs once, at the root, when `analyze_data` is on. `collect_data_preflight` reads every module source into a snapshot before any module evaluates, and `check_scope` builds the graph of each scope (`FrontmatterNames::Preflight`).
- `graph::analyze`: the root of each `Source::Url` reference must name a winning entry, a frontmatter key or a runtime variable (`unknown data reference name`). `check_levels` removes one level for each pass; a pass with nothing ready is a cycle (`blocked by a data dependency cycle`); more than `MAX_DATA_LEVELS` levels is `mds::resource_limit`. A file entry has no dependencies.
- In the compile role, the frontier (unreplayed URL entries, in module first-resolution order, then declaration order) gives `MdsError::network_denied` before any local read (`url_frontier_precedes_local_reads_and_follows_module_then_declaration_order`).
- A preflight refusal does not return at once. `graph_refusal` keeps it; the compile continues from the snapshot and reports its own first fault, `bind_data` returns the refusal and reads nothing, and `pop_resolving` returns it at root close when no other fault came first (`a_later_missing_import_does_not_replace_the_first_fault_of_the_compile`, `a_refused_graph_check_still_blocks_every_data_read` in `tests/data_graph.rs`).
- `bind_data` reads each distinct canonical key once for each compile (the `data.files` memo; `one_canonical_file_is_read_once_across_three_modules`).

### The `@message` role gate
`collect_single_message` refuses a role that depends on data (`mds::data`, `@message role must not depend on a data entry`), and only checks when `scope.reaches_data()`. `expr_reads_data` and `call_reads_data` follow provenance: a variable or member access of a data binding (a replayed entry too), each call of a template function, each qualified call, and built-in arguments. A loop variable keeps the provenance of its iterable; a function or namespace from a module that reaches data sets `bound_data`. Tests: `tests/data_role.rs`.

### Limits and their stages
Each cap is counted before the stage that does the work it bounds; `tests/data_limits.rs` passes each one at the cap and refuses one more.

| Constant (`limits.rs`) | Counts | Stage and site |
|---|---|---|
| `MAX_DATA_ENTRIES_PER_MODULE` | entries of one `data:` mapping | declaration, `parse_declarations` |
| `MAX_DATA_REFS_PER_ENTRY` (public) | references in the URL, headers and body of one entry | declaration, `DataLabel::add_refs`, before the graph check (its test reference names nothing, so it fails if the graph runs first) |
| `MAX_DATA_ENTRIES_PER_COMPILE` | winning entries, summed over the checked scopes | graph build, `check_graph` and `charges::count_data_entries`, before reads |
| `MAX_DATA_LEVELS` | graph depth | `graph::check_levels`, before reads |
| `MAX_DATA_FILES` (public) | distinct files | `charges::check_data_file_count`, before each new read |
| `MAX_DATA_FILE_BYTES` | bytes of one file | the backend read and `convert` |
| `MAX_DATA_BYTES` (public; `MAX_DATA_BYTES_PER_COMPILE`) | text of all files | `charges::count_data_bytes` through `admit_bytes`, after the bounded read, before conversion |
| `MAX_DATA_NODES_PER_FILE` | parser nodes | conversion |

The compile memory budget also counts data: `read_file` reserves one `MAX_DATA_FILE_BYTES` buffer before the read, `convert` charges both trees while they are live, and the memo keeps each converted value once for each compile. A binding's virtual module map counts data keys (`mds::is_data_path`) apart from modules, against `MAX_DATA_FILES` and `MAX_DATA_BYTES` (napi `lintVirtual`, WASM, Python).

### Errors and parse funnel
- `MdsError::Data` (`mds::data`), `NetworkDenied` (`mds::network_denied`) and `DataFileNotFound` are `#[non_exhaustive]`, and each has a public named constructor: `MdsError::data(module, entry, field, reason, help)`, `MdsError::network_denied(&[(module, entry)])`, `MdsError::data_file_not_found(key)` (a custom backend uses it for a miss). `data_error_variants_have_named_constructors` in `tests/api_surface.rs` pins them. Give a project-relative module label and prose; `data_location` escapes the module and sanitizes the entry and field.
- A bad data file is `mds::yaml`, `data file "<root-relative>", entry "<name>": invalid <format> data`. JSON and YAML data parse through `parse_data_json_with` and `parse_data_yaml_with` in `resolver/frontmatter.rs`; `yaml_parse_sites_are_funnelled` and `json_parse_sites_are_funnelled` (`tests/yaml_funnel.rs`) refuse a parse entry point in another file (`vars_json.rs` is the one other JSON owner).
- Lint (`lint/facts.rs`): each entry is a `FmVarKind::DataEntry` fact that `unused-variable` reports with "data entry" text; each URL reference root counts as a use; `shadow-variable` reports a `@for` variable or `@define` parameter over an entry (`ShadowKind::DataEntry`).

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
- **Fixed (#412), previously a known gap**: an extending module reached through `@include` now contributes a `FragmentMap` too, under the SAME gate a standalone module's evaluation uses — `prompt_body_and_map`/`fragment_map_from` are shared between `process_module` and `process_module_extends`, so `process_module_extends` seeds a `MapBuilder` (`seed = (source_map_mode && is_visible_export(..., "prompt")).then_some(&skeleton_origin)`) whenever source maps are on and `prompt` is export-visible, exactly as it would for a plain module. An `@include` of such a module now maps each byte it contributes back to the root base, intermediate, or child file that wrote it; that text used to carry no mappings at all (a direct `@extends` compile of the same file was already mapped — only the `@include`d path was blind). `fragment_map_from` still degrades to `None` when the segment cap was hit mid-build (a partial map would misattribute) or the body is empty (an `@include` of it adds no text).
- `MapBuilder`'s `SourceTable::intern` shares each distinct source's `Arc<str>` key/display/source text (three refcount bumps, no text COPY) rather than copying a region's file text on every splice (#416): a source-mapped `@extends` chain used to copy a region's source text once PER SPLICED REGION (every top-level node of the root base is its own region), so cost grew with the SQUARE of the base's size — a 262,144-node base took 68s with maps on vs 0.6s with them off in a debug build; now ~1.2s, byte-identical maps. `MapBuilder::splice_fragment` caches each `Arc<FragmentMap>`'s local→global source-index remap keyed by the fragment's raw `Arc` pointer, so K `@include`s of the same module (in a loop, or from several sites) register its sources once and only rebase segments on every later splice. A `FragmentMap` segment naming no fragment source is a compiler bug: `debug_assert!` stops on it in debug builds; release silently drops the segment (`rebased` returns `None`) rather than misattribute it — the same fail-safe `expand_per_line` already uses for an unresolvable segment. A file first registered through an `@include` splice now keeps the ROOT-RELATIVE display path it was loaded with, rather than the absolute canonical key the builder used to substitute as the display name (a CWE-209 leak no released version could reach — an included module's map never carried a file with its own `@extends` region until #412 made that possible).
- `ModuleCache` resets `EvalBudget` at the start of each compile. The function call count applies across all evaluated modules and regions. The loop and message-byte counts reset before each module evaluation, not before each region. All three seeded evaluator entry points use this budget. Messages mode shares one message vector across regions. In markdown mode the regions write into one buffer, so `push_capped` checks the cumulative output size at each append, and the `MemBudget` in `EvalBudget` charges that buffer's capacity to the compile memory budget. The retained total of cached modules does not reset with `EvalBudget`.
- `splice_skeleton` and `ExtendsComponents::final_body` are deleted; `has_message_block` is decided across the regions.
- A not-MDS `@extends` target keeps the importing file's name and the directive's span at an intermediate base too, not only at the entry: the error names the file that wrote the directive (`sub/c.mds`), not the entry. The intrinsic compile and the cached `resolve_path` give the same error. Pinned by `not_mds_intermediate_extends_keeps_the_base_importer_span` (`resolver_tests.rs`), which also compiles the matching MDS chain as a control.

## Fence Opt-In and Indented Directives

Code fences are literal by default: the lexer emits `Token::CodeFence` and raw `Token::CodeContent`, and the parser adds one LF to each fence line. Two opt-ins let a conditional line act as a directive. The normative text is `spec.md` "Conditionals inside a fence" and "Indented directives".

### `+mds` fence opt-in (`lexer.rs`)

- **Marker.** `+mds` is the exact ASCII last or only word of an opener's info string; space and tab separate the words. `scan_code_fence` keeps only the source prefix before the marker and its blanks in the `CodeFence` token, so the compiled opener has no marker. An unmarked opener keeps its full line and its legacy bare-CR line split.
- **Gate.** `Lexer::line_class(pos)` returns `Plain` unless `pos` is at a line start and the active rule is on: inside a fence that is `OpenFence.conditionals` (the marker), outside a fence it is the file key. `classify_line` then skips spaces and tabs, requires `@` and an ASCII-letter word, and matches `if`, `elseif`, `else` and `end` without ASCII case. A letter, digit, `_` or `-` right after the word makes the line `Plain` (so `@endnote` is text). Any other whitespace or control character in the indent before a keyword gives `BadIndent`, a syntax error (`invalid indent for a conditional directive`).
- **Directive tokens.** An accepted line becomes a `Token::Directive` with the line-start offset and its indent. The lines between them stay `CodeContent`, so `{{x}}`, `\{{` and other directives such as `@include` stay literal. A marked inner opener inside a fence is only content: the first closer ends the outer fence (`OpenFence::closes`: same fence char, at least as many, blanks only to CR or LF).
- **Conditional stack.** Each `OpenFence` owns `open_ifs`. `on_conditional` pushes on `@if` and refuses a push at `MAX_NESTING_DEPTH` (`nesting depth exceeds maximum of …`; the limit is pinned by `accepts_64_nested_ifs_and_rejects_the_65th_in_the_lexer`). `@elseif`, `@else` and `@end` with an empty stack are a syntax error (`conditional directive needs an open if in this code fence`), at their own line. `@end` pops one. At the closer, an open `@if` is a syntax error at the innermost open `@if` (`conditional directive must end before the code fence closes`). If the fence never closes, `unclosed code fence` at the opener wins. `in_fence_terminators_cannot_use_an_outer_or_previous_if` pins the per-fence stack.
- **Balanced outer `@if`.** An `@if` ... `@end` pair outside the fence can wrap a whole fence, marked or not. Outside a fence, the line goes through `scan_directive` as before; the fence has its own empty stack, so the outer pair is not touched. Pinned by `a_balanced_outer_if_can_wrap_an_unmarked_or_opted_in_fence` (`tests/virtual_fs.rs`).
- **Total block depth.** The parser counts outer blocks and in-fence `@if` blocks together against the total depth limit of 64. `tests/virtual_fs.rs` checks both sides of the limit.
- **Backslash.** A backslash before `@` is not `@`, so `classify_line` gives `Plain`: the line stays `CodeContent` and the backslash stays in the output.
- **Opener refusals (marked openers).** A `>` in the prefix before the fence gives `conditional code fence cannot have a blockquote prefix`. A control character in the info (`escape_class::is_control_char`: C0 except TAB and LF, DEL, C1, bidi and format hazards) gives `conditional code fence info contains a control character`. The info is read up to the LF with one trailing CR removed, so a CRLF line end passes and an embedded CR is refused.
- **Reserved info words (every opener, marked or not).** `parse_opener_info` runs on the info without the exact final marker. It returns `invalid code fence info word` for a word that starts with `+` and an ASCII letter or a non-ASCII character, for a word that contains `+mds` in any ASCII case, and for `mds` (any ASCII case) right after a lone `+` word. Words such as `+5`, `+=` and `++` stay valid.
- **Progress guard.** `Lexer::run` calls `step()` and compares `pos` before and after. A step that does not advance returns `MdsError::internal()` (`mds::internal`, `internal compiler error`). Test hook: `STALL_SCAN_TEXT` in the lexer tests, pinned by `stalled_text_scanner_returns_internal_error`. `escape_class.rs` now holds `is_control_char` and the two format-hazard predicates that `lint/diagnostic.rs` used to hold.

### `indented-directives` file key

- **Probe (`lexer.rs`).** `probe_file_key` reads the CR-free frontmatter text line by line, not the YAML tree. A line that starts with `indented-directives` and then `:` or a blank is the key line. A second key line gives `mds::syntax` (`indented-directives must appear only once`). The form must be `:`, at least one blank, `true` or `false`, then only blanks or a `#` comment; any other form gives `mds::syntax` (`expected indented-directives: true or indented-directives: false`), at that line. `scan_frontmatter` stores the result in `Lexer.indented_directives` (default `false`); `line_class` reads it only outside a fence. The file key does not affect `>`-prefixed lines or unmarked fences.
- **YAML agreement (`resolver/import_paths.rs`, `parse_frontmatter_mapping`).** After the YAML parse, the function probes the same raw text and compares the probe with the YAML mapping's `indented-directives` as a bool. A probe failure or any mismatch gives `mds::yaml` with the `FILE_KEY_FORM` text. When the hit is not on the first line, `mask_file_key` replaces the value with a same-width `null`, the YAML is parsed again, and the key must read as `Null`: this proves the probed line is a real YAML key and not text inside a scalar. The proof charges `raw.len()` as a temporary and releases it (`mark`, `release_to`).
- **Reserved-key sites.** The key is reserved at these sites, with SYNC POINT comments for the related audits: the `RESERVED_OUTPUT_KEYS` table (`lib.rs`), `RESERVED_MERGE_KEYS` (`resolver/frontmatter.rs`), `build_scope_from_frontmatter` (`resolver/module_scope.rs`) and `RESERVED` (`lint/facts.rs`). `data` is reserved at all four, and at two data-only sites: `RESERVED_NAMES` (`data/decl.rs`, the names a data entry cannot take) and `is_scope_key` (`data/resolve.rs`, the frontmatter names an entry can collide with or reference; `reserved_output_keys_are_not_data_scope_names` checks it against the output table). Each site reserves keys for its own purpose, so the lists differ (`extends` is in the merge list and in `RESERVED`, not in the output table; `RESERVED` and `RESERVED_NAMES` also list `prompt`). Effects: no variable in scope, no merge at depth 0, no key in standalone output frontmatter, no `unused-variable` fact. The output table is one list of `ReservedOutputKey` entries (name, an optional only-this-value rule, and a fixed form message; `type` reserves only the value `mds`). The stripper `strip_reserved_keys`, the parse proof, the form-message check `is_frontmatter_form_message` and the merged-output check in `serialize_merged_frontmatter` all read it, and the entry order sets which form message wins. The stripper drops only unindented key lines and their indented or blank continuation lines. It keeps every other byte of the frontmatter as written, line ends included, and keeps the next field when a YAML comment ends at U+0085, U+2028 or U+2029. Pins: `indented_directives_is_stripped_only_at_top_level`, `indented_directives_never_merges_as_a_top_level_key`, `indented_directives_is_not_a_frontmatter_variable_fact`.
- **Reserved-key form proof (`prove_reserved_strip`, `lib.rs`).** `parse_frontmatter_mapping` calls it last, so it is the one parse site for every module kind (entry, import, `@extends` base) and every surface (compile, check, lint, fmt). The proof refuses with `mds::yaml` and a fixed form message when the stripped text does not parse to the original mapping minus the reserved keys. That covers a quoted key, a space before the colon, an explicit `?` key, flow-style frontmatter (`starts_with_flow_mapping`; a flow mapping that is only the first block key is allowed), a column-0 sequence under a reserved key, and a kept `#` line that is scalar text and not a comment (parsed again with those lines masked). A top-level `<<` whose mapping, or a sequence item mapping, holds a reserved key is refused first. MDS does not apply `<<`; it is ordinary metadata otherwise. The key's own message names its plain form; a change in other YAML gives the generic `RESERVED_KEY_FORM`. The proof charges its temporary text and releases it. A plain file takes a fast exit (nothing stripped, no reserved key). Pins: `tests/reserved_key_form.rs`.
- **Per file, never inherited.** Each module is tokenized on its own source, so the key of an importer, `@extends` child or `@include` parent does not reach the other module, and the merge drops it.

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
- **KB-AP-2 — Reserving a frontmatter key at fewer than all the reserved-key sites**: the sites are listed in Fence Opt-In and Indented Directives. A missing site leaks the key into scope (`build_scope_from_frontmatter`), the merge (`RESERVED_MERGE_KEYS`), standalone output (the `RESERVED_OUTPUT_KEYS` entry, which also feeds the proof and the form messages) or `unused-variable` (`RESERVED`); a missing data site lets a data entry take the key as its name (`RESERVED_NAMES`) or treats the key as a scope name (`is_scope_key`). Add the key to the output table with its form message, then audit the other lists at their SYNC POINT comments.
- **KB-AP-3 — A lexer step that does not move `pos`**: `Lexer::run` stops with `mds::internal` instead of looping. Every new `step` branch must consume at least one character or return an error.

## Gotchas

- `check_*` functions now run full intrinsic dispatch including `has_message_block`. They will return `MixedContent` errors on templates that would have passed the old check (which used the markdown path only). This is a breaking behavior change.
- `dependencies` in `CompileResult` excludes the entry module. Downstream callers must not add the entry path manually. It also lists the data files that the compile read, after the modules: a caller that treats every entry as a module (a watcher, a module pre-scan) must expect data keys there.
- The `collect_messages_strict` internal name appears in some internal call chains; externally the only symbol exposed is `evaluate_messages_intrinsic`.
- The adjacently-tagged serde shape (`kind`/`value`) is for Rust-to-Rust serialization only. The NAPI wire format uses `output`/`messages` (not `value`) as the payload key — built explicitly in `build_canonical_result`.
- The output-size cap (`MAX_OUTPUT_SIZE`) bounds each output BUFFER, not a whole compile. `@if`, `@for`, `@block`, `@include` and `@extends` regions share the buffer of their evaluation; a `@define` call body, a messages-mode `@message` content and each module evaluation have their own buffer and their own 50 MiB cap. The whole-compile bound is the compile memory budget (`MAX_COMPILE_MEMORY`, 200 MiB, #420), and it bounds only the parts it counts (output buffers by capacity, evaluation temporaries, the syntax tree estimates, the compiler's copy of the caller variables, the parts that the `ModuleCache` keeps) plus the bounded charge-after overshoots. The lexer stage runs before the tree charge and is not counted. A peak-memory test for one compile asserts the budget plus a slack (`tests/memory_bound.rs`: 8 MiB), never the 50 MiB cap.
- `ModuleAliasError`'s bound checks run in a fixed order — `TooMany` before `TooLarge` before per-alias `Refused` — so a map that is simultaneously too large in count AND holds a bad alias always reports the count error first, never the alias.
- `scan_import_records`'s `ResolveAs::Base` ordering (imports before the module's own `@extends`) does NOT match `scan_imports`'/`ResolveAs::Standalone`'s ordering (`@extends` first); pick the variant that matches how the module is actually being reached, not always `Standalone`.
- A vars path swapped from a regular file to a FIFO after lookup is refused at once only because the regular-file open is non-blocking; do not replace `ReadRule::open` with `File::open` for that case, and do not add `O_NONBLOCK` to the FIFO-lookup or module opens (a FIFO vars input stays supported, and a source open can still wait for a FIFO).
- `Severity`'s `FromStr` is intentionally strict (no case-fold, no trim): a `mds.json` or binding-option value that a human typed with different casing (`"Warn"`) is a hard parse error, not silently accepted — this is a deliberate breaking change from napi/WASM's prior escape-decoding behavior, not an oversight.
- **KB-AP-4 — Backslash before `@` in a `+mds` fence**: `\@if x:` is `Plain` for `line_class`, so it is literal content and the backslash is NOT removed from the output. To show a conditional line as plain text with no backslash, omit `+mds` from the fence.
- **KB-AP-5 — Merged `@extends` frontmatter is not stripped**: `frontmatter_prefix` takes `MarkdownFrontmatter::Standalone` raw text through the stripper, but `MarkdownFrontmatter::Merged` text (from `serialize_merged_frontmatter`) is emitted as is. The merge already drops reserved keys at depth 0, so a reserved key in the merged mapping is a compiler bug and gives a real `mds::internal` check, not a silent strip. Each base is proved at its own parse, so a bad spelling in a base fails before the merge. `fmt` returns the form error itself: `formatter.rs` lets `is_frontmatter_form_message` errors pass before the structural fallback, so a refused form is never "verified" by token comparison.
- **KB-AP-6 — Two scanners read `type: mds`**: `is_type_mds_line` (`resolver/module.rs`, decides whether a `.md` file is MDS) trims the value and accepts only `mds`, `"mds"` or `'mds'`. `ReservedOutputKey::matches_line` (the stripper) also accepts a trailing ` #` comment. Both match only unindented lines and neither parses YAML, so a spelling change in one leaves the two disagreeing. The parse proof catches only the stripper side.
- The `+mds` marker is the last blank-separated word of the info string. A longer word that holds it (`x+mds`) is not the marker and is refused as a reserved word.

## Key Files

- `crates/mds-core/src/lexer.rs` — `line_class`/`classify_line`, `OpenFence` (per-fence conditional stack), `parse_opener_info`, `probe_file_key`, `mask_file_key`, `INDENTED_DIRECTIVES`, the `run` progress guard
- `crates/mds-core/src/escape_class.rs` — `is_control_char` and the format-hazard predicates (shared by the lexer and `lint/diagnostic.rs`)
- `crates/mds-core/src/lib.rs` — all public entry points; the `RESERVED_OUTPUT_KEYS` table, `strip_reserved_keys`, `prove_reserved_strip`, `frontmatter_prefix`; `CompileResult`, `CompiledOutput`, `Message` types, `scan_import_records`, `ImportRecord`/`ImportKind`/`ResolveAs`, `load_vars_file_reporting_duplicates`
- `crates/mds-core/src/error.rs` — `MdsError` variants including `MixedContent`, `ExpectedMarkdown`, `ExpectedMessages`, `NotMdsFile`, `Data`, `NetworkDenied`, `DataFileNotFound` (with `data_location`); `LineColumns` (pub(crate), line/column lookup shared with `scan_import_records`'s span attachment)
- `crates/mds-core/src/resolver.rs` — `ModuleCache`, `resolve_*_intrinsic*`, `charge_scan_tree`, `evaluate_regions_with_map`
- `crates/mds-core/src/resolver/charges.rs` — `parse_charged`, `check_module_count`, and module budget helpers
- `crates/mds-core/src/resolver/context.rs` — `ModuleCtx`, `fragment_map_from`, and `is_visible_export`
- `crates/mds-core/src/resolver/extends.rs` — `process_module_extends` and `evaluate_extends_regions`
- `crates/mds-core/src/resolver/import_paths.rs` — `resolve_entry_key`, `validate_import_path`, and `parse_frontmatter_mapping` (the one frontmatter parse site; it runs the file-key agreement and then `prove_reserved_strip`)
- `crates/mds-core/src/resolver/imports.rs` — import dispatch and scope binding
- `crates/mds-core/src/resolver/module.rs` — `ModuleRef`, `ModuleKey`, and `check_module_type`
- `crates/mds-core/src/resolver/module_scope.rs` — `has_message_block`, `build_scope_from_frontmatter`, and definition collection
- `crates/mds-core/src/resolver/process.rs` — `process_module_intrinsic`, `process_module`, `finalize_markdown`, and `prompt_body_and_map`
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
- `crates/mds-core/tests/output_cap_funnel.rs` — mechanised source-scan gate: the three accumulators in `evaluator.rs` and `resolver.rs` append only through `push_capped`; the output-size, compile-memory and frontmatter-allowance messages are built only in their `error.rs` constructors; the listed core files (`resolver.rs`, its children, and `limits.rs` included) do not contain the four cfg key names `target_arch`, `target_family`, `target_os` and `target_pointer_width` (`cfg(unix)`, `cfg(windows)`, `cfg(not(windows))` and the other target keys stay allowed), and the budget module code has no `RefCell` and none of the listed panic tokens (`panic`, `unreachable`, `todo`, `unimplemented`, `unwrap`, `expect`, the assert macros) (#415, #420)
- `crates/mds-core/tests/memory_bound.rs` — counting-allocator peak tests for compile shapes; the limit is the compile budget plus an 8 MiB slack (#420)
- `crates/mds-core/tests/read_bounds.rs` — the ONE `#[test]` in this binary (a process-wide counting global allocator cannot share the process with a second test): pins peak heap growth for a module read at roughly `MAX_FILE_SIZE + 1` byte plus a fixed 256 KiB slack for paths/display/error overhead — never the whole oversized file (#428)
- `crates/mds-core/tests/reserved_key_form.rs` — the reserved-key form proof across module kinds, output modes and `<<` merge metadata
- `crates/mds-core/src/data/` — local data sources: `decl.rs` (`parse_declarations`, `RESERVED_NAMES`, the reference cap), `path.rs` (`normalize`, `check_key`, `check_data_key`), `graph.rs` (`analyze`, `check_levels`, the frontier), `resolve.rs` (`Declarations` chain, `with_graph`, `canonical_file`, `read_file`, `is_scope_key`), `convert.rs` (`convert`, `DataLabel`)
- `crates/mds-core/src/resolver/charges.rs`, `resolver/process.rs`, `resolver/module_scope.rs` — the data counters, `prepare_data` (preflight, `network_denied`, pending warning) and `bind_data` (reads and the file memo)
- `crates/mds-core/tests/data_*.rs` — data behavior by stage: `data_paths.rs` (both `read_data` backends), `data_limits.rs` (each cap and its order), `data_graph.rs` (first-fault order), `data_role.rs` (role gate and lint facts), `data_tooling.rs` (roles and frontier order), `data_extends.rs`, `data_output.rs`, `data_convert.rs`, `data_resolve.rs`
- `crates/mds-core/tests/fixtures/data-path-vectors.json` — the shared data path vector table (`portable`, `windows`)
- `crates/mds-core/tests/yaml_funnel.rs` — source-scan gate: YAML parse entry points only in `resolver/frontmatter.rs`, JSON ones only there and in `vars_json.rs`

## Related

- Feature: mds-cli — consumes `CompileResult` and `CompiledOutput`; derives output extension from `OutputKind::from(&compiled.output)`; its own directory-argument and output-path refusals now call `NativeFs::check_directory`/`reject_forbidden_path` (#413) and its `lint.rules` reader calls `parse_rule_severities` (#175/#418); `mds build` and `mds watch` never write an output over a data file that the run reads (`build_never_writes_over_a_data_file_it_reads` in `crates/mds-cli/tests/cli_data.rs`)
- Feature: mds-napi — builds the canonical discriminated-union wire object from `CompileResult` via `build_canonical_result`; its `rules` option now reads through `parse_rule_severities`; `lintVirtual` counts data keys apart from modules- Feature: mds-js — TypeScript `CompileResult = MarkdownResult | MessagesResult` union mirrors this Rust type; its WASM-backend file pre-scanner mirrors `check_module_bytes`/`check_module_type`/`scan_import_records`/`VirtualFs::with_aliases` via `preflightModule`/`scanImportRecords`/`moduleAliases` (#414)
- Feature: mds-lint — the `literal-directive` rule reports the conditional lines that these opt-ins leave literal; `unused-variable` and `shadow-variable` read the data entry facts; `Severity`, `parse_rule_severities` and `apply_fixes_incremental` live here; consult it for the fix-tier and reverify-gate model
- Feature: source-map-security — `relativize_source`/`source_root` path-relativization choke point; this KB's `SourceTable`/`MapBuilder`/`FragmentMap` section (#416) covers the source-SHARING model that choke point's callers rely on
- Lesson: an alternate code path silently bypassing a resource/security check — cited above for the pre-#155 `let _ =` anchor-failure swallow and for why `EvalBudget` is one-per-chain, not one-per-region
- Lesson: a host-path leak fixed on one surface stays live on its siblings — cited above for `NotMdsFile`'s pre-#417 shape and the design-time census that found #413/#414/#417's remaining sinks before they shipped
- Lesson: a resource bound constrains only the axis it counts, at the layer where it runs — cited above for why the output cap is per-buffer, not per-compile, and why capacity (not just length) must be bounded
