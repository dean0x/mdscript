---
feature: mds-compiler
name: MDS Compiler
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, modifying CLI output behavior, using the virtual filesystem / dependency tracking API, working with @message blocks, messages output mode, the compile_messages API family, or template inheritance (@extends/@block). Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, export, include, define, for, if, elseif, negation, equality, Condition, CondValue, And, Or, logical operators, Param, default arguments, And, Or, logical operators, ArityMismatch, BuiltinError, call_function, required_param_count, condvalue_to_value, MAX_LOGICAL_OPERANDS, message, @message, messages mode, compile_messages, compile_messages_str, compile_messages_virtual, CompileMessagesOutput, Message, evaluate_messages, collect_messages, EvalMessage, OutputFormat, --format messages, injection safety, bare-word role, dynamic role, inside_message, total_message_bytes, MAX_MESSAGE_COUNT, MAX_MESSAGES_TOTAL_SIZE, MAX_ARRAY_ELEMENTS, scan_imports, load_vars_file, load_vars_str, check_virtual, compile_file, read_build_input, compile_to_content, compile_and_write, watch, extends, block, skeleton, ExtendsDirective, BlockNode, effective_skeleton, effective_blocks, frontmatter_values, process_module_skeleton, resolve_by_key_skeleton, resolve_extends_components, ExtendsComponents, splice_skeleton, spliced_regions, validate_extends_components, deep_merge_yaml, build_scope_from_merged_mapping, apply_block_overrides, check_child_only_blocks, MAX_BLOCKS_PER_MODULE, MAX_FRONTMATTER_MERGE_DEPTH, mds::extends, template inheritance, diamond inheritance, seed_effective_blocks, pf004_messages_mode_extends_validates_final_body_parity, utf8_boundary, compute_line_column, extends_error_at, p5b_deep_chain, Origin, EffectiveBlock, skeleton_origin, region-aware validation, ride-along, spliced_regions, ADR-022, diagnostic attribution, cross-source offset, OutOfBounds, SerializedSpan, character-based column."
category: architecture
directories: [crates/mds-core/src/, crates/mds-cli/src/, crates/mds-cli/tests/]
referencedFiles:
  - crates/mds-core/src/lib.rs
  - crates/mds-core/src/fs.rs
  - crates/mds-core/src/ast.rs
  - crates/mds-core/src/lexer.rs
  - crates/mds-core/src/parser.rs
  - crates/mds-core/src/parser_helpers.rs
  - crates/mds-core/src/validator.rs
  - crates/mds-core/src/resolver.rs
  - crates/mds-core/src/evaluator.rs
  - crates/mds-core/src/scope.rs
  - crates/mds-core/src/value.rs
  - crates/mds-core/src/error.rs
  - crates/mds-core/src/limits.rs
  - crates/mds-core/src/builtins.rs
  - crates/mds-cli/src/main.rs
  - crates/mds-cli/src/build.rs
  - crates/mds-cli/src/watch.rs
  - crates/mds-core/tests/api_surface.rs
  - crates/mds-core/tests/messages.rs
  - crates/mds-cli/tests/format_messages.rs
  - crates/mds-cli/tests/inheritance.rs
created: 2026-05-12
updated: 2026-06-14
---

# MDS Compiler

## Overview

MDS (Markdown Script) is a Rust compiler that transforms `.mds` files — Markdown with `@directives` and `{var}` interpolation — into plain Markdown. The primary use case is composable LLM prompt templates: authors write templates with variables, conditionals, loops, and reusable function fragments, then compile them to a final prompt string.

The compilation pipeline is strictly sequential: **lexer → parser → validator → resolver → evaluator → render**. Each layer has a single responsibility and communicates through typed interfaces rather than shared mutable state. The `resolver` is the orchestrator — it drives all other stages and manages the module cache used for imports.

The compiler supports two output modes: **text mode** (the default, renders to a Markdown string) and **messages mode** (compiles `@message` blocks into a structured `Vec<Message>` for LLM chat APIs). Template inheritance (`@extends`/`@block`, Issue #58) adds a third structural concern: a child template extends a base, overriding named `@block` placeholders; the resolver assembles the final body before the single validate+evaluate pass runs at the leaf. Both output modes share the full inheritance pipeline.

## System Context

**Cargo workspace**: `mds-core` (library crate, publishes as `mds`) at `crates/mds-core/`; `mds-cli` (binary crate) at `crates/mds-cli/`. The workspace root `Cargo.toml` and `Cargo.lock` are at the repo root.

The library exposes public `compile*` / `check*` / `compile_messages*` functions (all carry `#[must_use]`). Public types include: `FileSystem`, `NativeFs`, `VirtualFs`, `ModuleCache`, `Value`, `MdsError`, `SerializedError`, `SerializedSpan`, `CompileOutput`, `CompileMessagesOutput`, `Message`, and constants `MAX_FILE_SIZE` / `MAX_TRAVERSAL_DEPTH`.

**Utility functions**:
- `pub fn compile_file(path: &str) -> Result<String, MdsError>` — thin wrapper over `compile(Path::new(path), None)`.
- `pub fn scan_imports(source: &str) -> Result<Vec<String>, MdsError>` — parses the AST and returns all dependency paths in resolution order: `@extends` base path FIRST, then frontmatter `imports:` paths, then body `@import`/`@export ... from` paths. Deduplicated in insertion order. Returns a compile error on syntax error.
- `pub fn load_vars_file(path: &Path) -> Result<HashMap<String, Value>, MdsError>` — reads a JSON file as vars; enforces `MAX_FILE_SIZE`.
- `pub fn load_vars_str(json: &str) -> Result<HashMap<String, Value>, MdsError>` — parses a JSON string as vars.
- `pub fn check_virtual(modules, entry, vars) -> Result<(), MdsError>` — validates a virtual-filesystem module.
- `pub fn check_virtual_collecting_warnings(modules, entry, vars) -> Result<((), Vec<String>), MdsError>` — same but returns warnings.

All compile/check functions funnel through `ModuleCache::resolve` / `ModuleCache::resolve_source`. **Warning collection pattern**: warnings pass as `&mut Vec<String>` through the full pipeline — no `eprintln!` in evaluator or resolver.

The library module tree includes `pub(crate) mod builtins` (declared in `lib.rs`) which holds the 18 built-in functions.

## Component Architecture

### Limits Module (`crates/mds-core/src/limits.rs`)

All cross-pipeline defense-in-depth constants:

- `pub(crate) const MAX_DOT_SEGMENTS: usize = 32`
- `pub(crate) const MAX_NESTING_DEPTH: usize = 64`
- `pub(crate) const MAX_ELSEIF_BRANCHES: usize = 256`
- `pub(crate) const MAX_LOGICAL_OPERANDS: usize = 16`
- `pub(crate) const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024`
- `pub(crate) const MAX_TRAVERSAL_DEPTH: usize = 256`
- `pub(crate) const MAX_OUTPUT_SIZE: usize = 50 * 1024 * 1024`
- `pub(crate) const MAX_ARRAY_ELEMENTS: usize = 100_000`
- `pub(crate) const MAX_FRONTMATTER_IMPORTS: usize = 256`
- `pub(crate) const MAX_MESSAGE_COUNT: usize = 10_000`
- `pub(crate) const MAX_MESSAGES_TOTAL_SIZE: usize = MAX_OUTPUT_SIZE`
- `pub(crate) const MAX_BLOCKS_PER_MODULE: usize = 256` — caps `@block` declarations per module; enforced in `collect_block`
- `pub(crate) const MAX_FRONTMATTER_MERGE_DEPTH: usize = 64` — caps `deep_merge_yaml` recursion depth; exceeding returns `mds::resource_limit`

`MAX_FILE_SIZE` and `MAX_TRAVERSAL_DEPTH` are re-exported as `pub const` from `lib.rs`.

### Evaluator Constants (`crates/mds-core/src/evaluator.rs`)

- `const MAX_CALL_DEPTH: usize = 128`
- `const MAX_LOOP_ITERATIONS: usize = 100_000`
- `const MAX_TOTAL_ITERATIONS: usize = 1_000_000`
- `const MAX_WARNINGS: usize = 1_000`

### Built-in Functions (`crates/mds-core/src/builtins.rs`)

18 built-in functions in three groups. `get_builtin` / `call_builtin` are the `pub(crate)` interface. `split()` enforces `MAX_ARRAY_ELEMENTS`.

**String:** `upper`, `lower`, `trim`, `replace(str, from, to)`, `starts_with(str, prefix)`, `ends_with(str, suffix)`, `contains(str_or_array, needle)`, `slice(str_or_array, start[, end])`, `string(val)`

**Array:** `split(str, sep)`, `join(array, sep)`, `length(str_or_array)`, `first(array)`, `last(array)`, `reverse(str_or_array)`, `sort(array)`, `unique(array)`

**Type conversion:** `string(val)`, `number(val)`

### AST (`crates/mds-core/src/ast.rs`)

**Template inheritance nodes added in Issue #58**:

**`Module.extends: Option<ExtendsDirective>`** — set when the file begins with `@extends "path"`. `extends.is_some()` is the canonical child-vs-standalone discriminator — a module with `extends` cannot render standalone; a module without `extends` cannot have inherited blocks. Illegal states are unrepresentable.

**`ExtendsDirective` struct** — `path: String` (raw quoted path), `offset: usize` (byte offset of the `@extends` token, used for error spans). Modeled after `ImportDirective`.

**`Node::Block(BlockNode)`** — a named template block: `@block name:` ... `@end`. In standalone mode, the body is rendered inline (markers are invisible). In inheritance mode, the resolver splices child overrides before evaluate is called — the evaluator's `Node::Block` arm handles both cases via `evaluate_block`.

**`BlockNode` struct** — `name: String`, `body: Vec<Node>`, `offset: usize`.

**`Node::Message(MessageBlock)`** — top-level node variant for `@message` blocks (unchanged from Issue #56).

**`Condition` enum** — six variants (unchanged). Does not derive `PartialEq`.

**`Expr` enum** — unified expression type (unchanged). `parse_expr_inner` is the shared grammar.

**`required_param_count(params: &[Param]) -> usize`** lives in `ast.rs`. Both validator and evaluator import it from `crate::ast`.

### Parser (`crates/mds-core/src/parser.rs`, `parser_helpers.rs`)

**`parse_extends_if_present`** — called from `parse_module` immediately after frontmatter parsing. Peeks ahead over blank Text tokens; if the next meaningful token is `@extends`, consumes it and returns `Ok(Some(ExtendsDirective))`. A stray `@extends` later in the body is caught by `parse_directive` as an `mds::extends` error.

**`parse_block(rest, offset)`** — parses `@block name:` ... `@end`. Enforces top-level-only via `inside_block` flag and `depth` guard. Rejects nesting inside `@block`, `@if`, `@for`, `@define`, `@message` at parse time (E9 → `mds::syntax`, not `mds::extends`).

**`BlockGuard` RAII struct** — mirrors `MessageGuard`. Created immediately after `enter_block()` sets `inside_block = true`. `Drop` resets `inside_block = false` and decrements `depth`. All `?` error paths trigger Drop, keeping the invariant structural rather than manual. `debug_assert!(depth > 0)` guards against underflow.

**`Parser` struct flags**: `inside_message: bool`, `inside_block: bool` — both enforced via RAII guards.

**`parse_expr_inner`** (in `parser_helpers.rs`) — the unified expression parser used for `{...}`, `@if`/`@for` directives, and `@message` role expressions.

**`parse_message_block`** — parses `@message role:` ... `@end`. Role parsing: `{expr}` → `parse_expr_inner`; bare word → `Expr::StringLiteral`. Uses `MessageGuard` for state restore.

### Validator (`crates/mds-core/src/validator.rs`)

**`validate_block_node`** — validates a `Node::Block` arm by recursing into `block.body` with the same scope. Blocks can contain all normal directives; validation is shared between standalone and inheritance modes (the resolver has already spliced the final body before `validator::validate` is called at the leaf).

**`Node::Message` arm** — validates role expression via `validate_expr`, recurses into body.

### Evaluator (`crates/mds-core/src/evaluator.rs`)

**`Node::Block` arm in `evaluate_nodes`** — calls `evaluate_block(block, scope, ctx)`. In standalone mode, renders the default body inline (markers invisible). In inheritance mode, `splice_skeleton` has already replaced the base skeleton's `@block` placeholders with the effective body before this arm is reached — so the same arm handles both cases.

**`Node::Block` arm in `collect_messages`** — descends into the block body to surface `@message` blocks it contains, so `has_message_block` detection works for `@message` inside `@block` defaults (avoids PF-004 divergence).

**`evaluate_block(block, scope, ctx)`** — thin helper: calls `evaluate_nodes` on `block.body`.

**`EvalContext` struct** fields: `call_stack`, `total_iterations`, `total_message_bytes`, `warnings`.

**`evaluate_messages(nodes, scope, warnings)`** and **`collect_messages`** — messages-mode evaluation path (unchanged from Issue #56).

### Resolver (`crates/mds-core/src/resolver.rs`)

The resolver has two resolution paths: standalone (`process_module`) and skeleton (`process_module_skeleton` / `resolve_by_key_skeleton`).

#### Region-Aware Validation — `Origin` "Ride-Along" Architecture (ADR-022)

Previously the resolver spliced base+child into `final_body` and called `validator::validate(&final_body, ..., ctx.file_str, ctx.source)` once with the CHILD's single source. When a validation error's AST offset came from a BASE/ancestor node, it was paired with the child's `NamedSource`, causing miette to render `OutOfBounds` ("Failed to read contents for label ... : OutOfBounds") in the CLI human path. The JSON `serialize()` path degraded more gracefully (via `compute_line_column`'s OOB guard) but still reported wrong file attribution. This is the cross-source-offset class.

The fix: each spliced region carries its own source ("ride-along"):

**`struct Origin { pub(crate) file: Arc<str>, pub(crate) source: Arc<str> }`** — the display filename plus the source bytes its offsets index into. Manual `Debug` impl prints `file` + `source.len()` only — NEVER raw source bytes (no-leak rule, `debug-panics` constraint).

**`struct EffectiveBlock { pub(crate) node: Arc<BlockNode>, pub(crate) origin: Origin }`** — replaces the bare `Arc<BlockNode>` value type of `ResolvedModule.effective_blocks` and `ExtendsComponents.effective_blocks`. Now `IndexMap<String, EffectiveBlock>`. `Clone` = three refcount bumps (one `Arc<BlockNode>` + two `Arc<str>` inside `Origin`). Manual elided `Debug`.

**`ResolvedModule.skeleton_origin: Origin`** — the root-base skeleton's origin (file + source for non-block skeleton nodes). `Arc::clone`'d down the chain so deep multi-level chains have zero extra allocations.

**`ExtendsComponents.skeleton_origin: Origin`** — carries the same root skeleton origin through `resolve_extends_components` to the validate/evaluate sites.

**`ResolvedModule`'s `#[derive(Debug)]`** is replaced by a manual `Debug` that elides source bytes (avoids leaking source in panic/debug output).

**Arc allocation rule**: One `Origin` is built PER MODULE RESOLUTION (in `process_module` root path and `process_module_skeleton` root-base arm) and `Arc::clone`d into each block — NEVER `Arc::from(ctx.source)` inside a per-block loop. Load-bearing perf rule: O(blocks×source) allocation would break the `p2_wide_base_200_blocks_under_1s` guard. Tested by `p_block_sources_share_one_arc` (asserts `Arc::ptr_eq` — all blocks from one file share ONE `Arc<str>` source).

**Origin follows the winning override**: `apply_block_overrides` keeps each inherited (un-overridden) entry's existing origin on the cloned map, and stamps each override INSERT with an `Origin` built from the CURRENT (overriding) `ctx`. A base-default block keeps the root base's origin; a child override carries child offsets+source. Test: `e12_child_override_undefined_var_attributes_to_child` (F3 in unit tests).

**Root source reaches the leaf**: extending modules `Arc::clone` the ancestor's `skeleton_origin` down the chain (in `resolve_extends_components`, the `process_module_skeleton` intermediate arm, `process_module_extends` construction, and the `resolve_by_key` A1 cache-upgrade). In an A←B←C chain the leaf reads A's source bytes for skeleton non-block nodes. Test: `e12_multilevel_undefined_var_attributes_to_root_base` (F4).

#### Single Shared Skeleton Walk — `spliced_regions`

**`fn spliced_regions(skeleton, effective_blocks, skeleton_origin) -> Vec<(&[Node], &Origin)>`** — a `Node::Block` placeholder yields `(&eff_block.node.body, &eff_block.origin)`; any other skeleton node yields `(slice::from_ref(node), skeleton_origin)`. Mirrors the old missing-block `debug_assert!(false, ...)` + release fallback.

**`splice_skeleton`** is now a thin adapter: `spliced_regions(...).into_iter().flat_map(|(nodes, _)| nodes.iter().cloned()).collect()`. BOTH `splice_skeleton` (eval) and `validate_extends_components` (validate) consume this one iterator — eval and validate node sets can never drift (PF-004 lesson). This is the primary correctness mechanism against parallel-path divergence.

**`fn validate_extends_components(components: &ExtendsComponents, scope: &mut Scope) -> Result<(), MdsError>`** — walks `spliced_regions` and calls `validator::validate(nodes, scope, &origin.file, &origin.source)?` per region. BOTH the text site (`process_module_extends`) and the messages site (`process_module_messages`) call this single helper instead of validating `final_body` against `ctx.source` — preserving PF-004 parity by construction. Per-region validation equals whole-slice validation because `validator::validate` only does local scope push/pop for `@define`/`@for` and functions are pre-registered; document order (which-error-first) is preserved.

#### Template Inheritance Architecture

**`ResolvedModule` fields** (all `pub(crate)`):
- `functions: HashMap<String, Arc<FunctionDef>>`
- `prompt_body: Option<String>`
- `raw_frontmatter: Option<String>`
- `has_explicit_exports: bool`
- `explicit_exports: HashSet<String>`
- `effective_skeleton: Arc<[Node]>` — root-ancestor body, Arc-shared across all descendants. For standalone modules: own body (built once, Arc-shared). For extending modules: `Arc::clone` of the base's skeleton — never a deep-clone.
- `effective_blocks: IndexMap<String, EffectiveBlock>` — name → `EffectiveBlock { node: Arc<BlockNode>, origin: Origin }`. Seeded from own `@block` declarations (standalone) or `clone(base.effective_blocks)` + child overrides (extending). Most-derived wins; each entry carries the origin of its winning file.
- `skeleton_origin: Origin` — root-base file + source for non-block skeleton nodes. `Arc::clone`'d down the chain.
- `frontmatter_values: Option<serde_yaml_ng::Mapping>` — raw parsed YAML for this module. For intermediate bases in a chain, this is the *transitive accumulated merge* of all ancestors' FM + own FM, so a leaf descending from it gets the full chain without re-traversing.
- `is_skeleton: bool` — `true` when produced by `process_module_skeleton` (no validate/evaluate). Cache-poisoning guard: `prompt_body.is_none()` is NOT a reliable skeleton signal (standalone modules with empty body also have `None`). Always use `is_skeleton`.

**Note**: The `extends_path` field that appeared in earlier drafts of this feature was removed — it was set but never read anywhere in the workspace. Dead code removal keeps the zero-warnings gate clean.

**`Arc::from(body)` consuming move**: On both the standalone path (`process_module`) and the root-base arm of `process_module_skeleton`, `effective_skeleton` is built via `Arc::from(module.body)` (consuming the `Vec<Node>` directly), not `Arc::from(body.as_slice())`. This reuses the Vec allocation with no per-element Node clones. `seed_effective_blocks` is called before the Arc construction so the borrow ends first.

**A1 invariant `debug_assert!`**: `process_module_skeleton` has a `debug_assert!(prompt_body.is_none(), "A1 invariant: …")` pinned immediately before the `Ok(ResolvedModule { is_skeleton: true, … })` return. This catches accidental evaluate calls on the skeleton path in debug builds. The full `SkeletonEntry`/`StandaloneEntry` enum split is tracked as tech debt.

**Cache-poisoning invariant (A1)**: A file may be resolved as a skeleton base before it is compiled standalone (or vice-versa). Cache key is the same normalized file key in both cases. Rules:
- Standalone-first → skeleton reuses the full entry as-is (it has everything a base needs).
- Skeleton-first → `resolve_by_key` detects `is_skeleton` on cache hit, performs full compile, upgrades the entry in place; reuses the skeleton's `effective_skeleton` / `effective_blocks` / `skeleton_origin` Arcs so descendants keep pointer-identity.

#### Resolution Flow for Extending Modules

**`resolve_by_key_skeleton(key, runtime_vars, warnings)`** — resolves a file for use as an `@extends` base. Uses the same `ModuleCache` and `resolving` stack as `resolve_by_key`, so cycle detection (`mds::circular_import`), `MAX_IMPORT_DEPTH`, dependency tracking, and `MAX_FILE_SIZE` all apply automatically (applies PF-004). Calls `process_module_skeleton`.

**`process_module_skeleton(ctx, is_md, warnings)`** — tokenize → parse → collect (functions/blocks/frontmatter); NO validate/evaluate. Sets `is_skeleton = true`, `prompt_body = None`. For intermediate bases in a chain (B in A←B←C): recursively resolves the grandparent via `resolve_by_key_skeleton`, applies `check_child_only_blocks`, runs `apply_block_overrides`, and deep-merges FM transitively (`grandparent.frontmatter_values < own_fm_values`). `skeleton_origin` is `Arc::clone`'d from the grandparent so the root base's source bytes ride down the chain.

**`resolve_extends_components(module, ext, ctx, frontmatter_values, warnings) -> Result<ExtendsComponents>`** — the shared pipeline for steps 3a–3e, called by both `process_module_extends` (text) and `process_module_messages` (messages). Both modes go through exactly the same path, avoiding PF-004 divergence.

Steps executed by `resolve_extends_components`:
1. **3a**: validate import path, resolve base via `resolve_by_key_skeleton`.
2. **3b**: `check_child_only_blocks` — every top-level node in `module.body` must be `Node::Block` or whitespace-only `Text`. Stray content → `mds::extends`.
3. **3c**: `apply_block_overrides` — clones `base.effective_blocks` (diamond-safe: never mutates cached base), then for each `Node::Block` in `module.body`, updates the block entry with the child's override origin. Child block name not in base → `mds::extends` (unknown override, E4). Most-derived wins.
4. **3d**: Build merged scope — `deep_merge_yaml(base_fm, child_fm)` then `build_scope_from_merged_mapping`; resolve base FM imports (relative to base key, using `base.skeleton_origin.source` so any span-carrying errors attribute correctly) then child FM imports (relative to child key); merge base functions into scope; collect child body definitions.
5. **3e**: `splice_skeleton(effective_skeleton, effective_blocks, skeleton_origin)` — produces `final_body` via `spliced_regions` adapter. Linear O(S+B) pass.

**`process_module_extends(module, ext, ctx, raw_frontmatter, frontmatter_values, warnings)`** — calls `resolve_extends_components`, then calls `validate_extends_components` (region-aware validation via `spliced_regions`) + `evaluate(&final_body, ...)` (step 3f, text mode). Operates on `final_body`, NOT `module.body` — this is what makes validation of base default blocks using the merged leaf scope work (ADR-016).

**`process_module_messages`** — for the `@extends` branch, calls `resolve_extends_components`, then calls `validate_extends_components` (step 3f, messages), checks `has_message_block(&final_body)` (NOT `module.body`), then calls `evaluate_messages(&final_body, ...)`. Both text and messages mode call the same `validate_extends_components` helper, preserving PF-004 parity.

#### Scope Construction for Inheritance

**`deep_merge_yaml(base, child, depth) -> Result<Mapping>`** — deep-merges two YAML Mappings for frontmatter inheritance. Semantics:
- Both values are Mappings: recurse key-by-key.
- Otherwise: child wins (scalar over scalar, array over array, etc.).
- Arrays REPLACE WHOLESALE — no element-level merge.
- Key ORDER: base-then-child (determinism). Child-only keys appended in child order.
- Reserved keys (`imports`, `type`, `extends`) EXCLUDED from output. SYNC POINT: these three keys are in `deep_merge_yaml::RESERVED` to prevent propagation as FM variables; `strip_reserved_keys` in `lib.rs` has a different (intentionally non-identical) list — `extends` appears in `RESERVED` but NOT in `strip_reserved_keys` because it is never emitted as output YAML. Both functions carry cross-reference SYNC POINT comments to keep maintenance in sync.
- Recursion bounded by `MAX_FRONTMATTER_MERGE_DEPTH = 64`; exceeding returns `mds::resource_limit`.

Precedence: **base < child < runtime** (decision #3 / F7).

**`build_scope_from_merged_mapping(mapping, runtime_vars)`** — builds scope from a pre-merged Mapping (reserved keys already excluded by `deep_merge_yaml`). Runtime vars applied last.

#### Helper Functions

**`seed_effective_blocks(body, block_names, origin) -> IndexMap<String, EffectiveBlock>`** — extracted helper that seeds the `effective_blocks` map from a body slice, the set of known block names, and an `&Origin` (cloned into each seeded `EffectiveBlock`). Used in both `process_module` (standalone path) and `process_module_skeleton` (root-base arm). Uses a `filter_map` iterator over the body to preserve declaration order in the `IndexMap`.

**`check_child_only_blocks(body, ctx)`** — validates that every top-level node in a child body is `Node::Block` or whitespace-only `Text`. Returns `mds::extends` with span on first stray node. Has a `debug_assert!(ctx.source.is_char_boundary(offset))` guard before indexing into source.

**`apply_block_overrides(parent_blocks, body, ctx)`** — clones parent map, builds one `Origin` from `ctx` OUTSIDE the loop (perf rule), applies child overrides stamped with that origin. Returns `mds::extends` for unknown block name (child overriding a block not in parent). Inherited (un-overridden) entries keep their existing origin from the cloned map.

**`splice_skeleton(skeleton, effective_blocks, skeleton_origin) -> Vec<Node>`** — thin adapter over `spliced_regions`; flat-maps the `(nodes, _)` pairs into a `Vec<Node>`. The result is a flat `Vec<Node>` with no `Node::Block` wrappers — block markers are invisible to validate+evaluate.

**`spliced_regions(skeleton, effective_blocks, skeleton_origin) -> Vec<(&[Node], &Origin)>`** — canonical single walk used by BOTH splice and validate. See "Single Shared Skeleton Walk" above.

**`collect_block(block, defs, count, ctx)`** — registers a `@block` name in `defs.block_names`; checks for duplicate names and `@block`-vs-`@define` collisions (same namespace, decision #10); enforces `MAX_BLOCKS_PER_MODULE`.

**`CollectedDefs`** — private struct with `block_names: HashSet<String>` added alongside the existing `functions`, `has_explicit_exports`, `explicit_exports`.

**`ExtendsComponents`** — private struct returned by `resolve_extends_components`: `final_body`, `scope`, `functions`, `effective_skeleton`, `effective_blocks`, `skeleton_origin`, `has_explicit_exports`, `explicit_exports`.

#### `scan_imports` Update

`scan_imports` prepends the `@extends` base path FIRST (before frontmatter imports and body imports), matching the resolution order: `extends → fm_imports → body_imports`. This ensures dependency scanners (e.g. the watch loop) see the base as the leading dependency.

### Error System (`crates/mds-core/src/error.rs`)

**`fn at(file, source, offset, len) -> (Option<SourceSpan>, Option<Arc<NamedSource<String>>>)`** — private shared constructor called by all `*_at` error constructors. Computes `end = offset.saturating_add(len)` and `in_bounds = end <= source.len() && source.is_char_boundary(offset) && source.is_char_boundary(end)`. When `!in_bounds`, returns `(Some(SourceSpan::new(offset, len)), None)` — keeps the raw offset/length for `serialize()` but drops `src` so miette never reads outside the source string (eliminates `OutOfBounds` in ALL render paths: CLI human AND JSON). A `debug_assert!(in_bounds || source.is_empty(), "cross-source offset mismatch…")` fires in debug/test builds for the cross-source bug case; the empty-source escape prevents false-fires on unit tests that pass `""` as source.

**`compute_line_column(source: &str, offset: usize) -> Option<(usize, usize)>`** — private helper that converts a byte offset into a 1-based (line, column) pair. Boundary-safe: returns `None` for `offset > source.len()` or when `offset` is not a valid UTF-8 char boundary (guards against panic on multi-byte UTF-8). `offset == source.len()` (exclusive-end) returns `Some`. Column is **character-based** (Unicode scalar values via `source[..offset].chars()`), not byte-based — so `SerializedSpan.column` equals the visual column on multibyte lines; ASCII is unchanged. `SerializedSpan` doc was corrected to state "character position (Unicode scalar values)". Test: `compute_line_column_is_char_based` in `error_tests.rs` (CJK 3-byte chars: byte-col would be 7, char-col is 3).

**`MdsError::Extends { message, span, src }`** — code `mds::extends`. Used for:
- Child-only-blocks violations (stray top-level content in a child body).
- Unknown block override (child `@block` not declared in root base).

Constructor: `MdsError::extends_error_at(msg, file, source, offset, len)` — the ONLY constructor for `Extends` errors; the no-span variant `extends_error()` was removed (it was dead code). Span assertions in `a3_resolver_error_code_table` confirm that both E3 and E4 errors always carry source-location context (`s.span.is_some()`).

Note: `@block` nesting violations (E9) use `mds::syntax` (not `mds::extends`) — these are parse-time structural errors, not inheritance-semantic errors.

Full error code set for the inheritance subsystem: `mds::extends`, `mds::name_collision` (duplicate block name, block/define collision, duplicate FM import alias), `mds::circular_import` (cycle through `@extends` chain), `mds::syntax` (stray `@extends`, nested `@block`).

**`SerializedSpan`** — `offset` and `length` are raw bytes; `column` is the 1-indexed character position (Unicode scalar values) — NOT a byte offset and NOT UTF-16 code units. `line` and `column` are `None` when `src` is `None` (cross-source out-of-bounds case), but `offset`/`length` still reflect raw `SourceSpan` values.

### Messages-Mode Resolution

**Messages-mode resolution path** — `resolve_key_messages` and `resolve_source_messages` delegate to `process_module_messages`, which uses `resolve_extends_components` for extending modules (identical pipeline to text mode). The `has_message_block` check runs against `final_body` (not `module.body`), so `@message` blocks inside base `@block` defaults are correctly detected.

**Validate before evaluate (PF-004 parity)** — `process_module_messages` calls `validate_extends_components` (region-aware, via `spliced_regions`) before the `has_message_block` guard and before `evaluate_messages`, mirroring text-mode `process_module_extends`. Both modes share one `validate_extends_components` implementation — divergence is structurally impossible. Test: `pf004_messages_mode_span_parity_with_text_mode` (verifies both text and messages mode report the same span file/offset for a base-default undefined var).

**No-`@message`-blocks hard error** — `process_module_messages` returns `mds::syntax` if no `@message` block is found in the assembled final body. This is a compile error, not a silent fallback.

**`validate_exports` parity** — both `process_module` and `process_module_messages` call `validate_exports`; avoids PF-004.

### Frontmatter Imports

**`FrontmatterImport` enum** with three variants: `Alias { path, alias }`, `Merge { path }`, `Selective { path, names }`. Functions: `parse_frontmatter_imports_from_yaml` (from a YAML value), `parse_frontmatter_imports` (from raw YAML string, used by `scan_imports`).

**Resolution order**: frontmatter imports before body `@import` directives. Per-file resolution in inheritance: base FM imports resolve relative to the base key (using `base.skeleton_origin.source` so spans attribute correctly); child FM imports resolve relative to the child key. A duplicate alias across base and child → `mds::name_collision`.

### Public API: `compile_messages` family

Three-tier API mirroring `compile*`:

| Function | Input | Returns |
|---|---|---|
| `compile_messages_str(source)` | string | `Result<CompileMessagesOutput>` |
| `compile_messages_str_with_deps(source, base_dir, vars)` | string + options | `Result<CompileMessagesOutput>` |
| `compile_messages_virtual(modules, entry, vars)` | virtual FS | `Result<CompileMessagesOutput>` (warns to stderr) |
| `compile_messages_virtual_with_deps(modules, entry, vars)` | virtual FS | `Result<CompileMessagesOutput>` |

**`CompileMessagesOutput`**: `messages: Vec<Message>`, `warnings: Vec<String>`, `dependencies: Vec<String>`.

**`Message`**: `role: String`, `content: String`. Both types derive `serde::Serialize`.

### CLI Module Layout (`crates/mds-cli/src/`)

- **`main.rs`** — CLI entry point: `Cli` struct, `Commands` enum (Build/Check/Init/Watch), `main()`, `run()`, `run_check()`, `run_init()`.
- **`build.rs`** — all shared build logic: `OutputFormat`, `BuildArgs`, `CompileOutput` (CLI-internal), `compile_to_content`, `compile_and_write`, `run_build`/`run_build_messages`/`run_build_markdown`, `read_build_input`, `read_stdin`, `write_output`, `load_config`, `resolve_output_path`, `build_runtime_vars`, `exit_code`, `auto_detect_mds_file`, `reject_directory_input`, `parse_key_value`, `parse_cli_value`.
- **`watch.rs`** — watch subcommand: `WatchArgs`, `run_watch`, `run_watch_file`, `run_watch_dir`, `dir_watch_startup`; context structs; extracted helpers.

**Important naming distinction**: `build::CompileOutput { content: String, dependencies: Vec<String> }` (CLI-internal, pre-serialized content) ≠ `mds::CompileOutput { output: String, warnings: Vec<String>, dependencies: Vec<String> }` (core library type).

### CLI: `OutputFormat` and `--format` flag

**`OutputFormat` enum** (in `build.rs`): `Markdown` (default), `Messages`. Derives `clap::ValueEnum`. Both `build` and `watch` subcommands accept `--format`; in `watch`, `--format messages` is only valid in single-file mode.

### CLI: `compile_to_content` and `compile_and_write`

- **`compile_to_content`** — Markdown: calls `mds::compile_with_deps`; Messages: calls `read_build_input` → `mds::compile_messages_str_with_deps` → serializes. Returns `build::CompileOutput`.
- **`compile_and_write`** — calls `compile_to_content` then `write_output`. Returns transitive dep list (watch uses this to update watched files).

### CLI: `watch` subcommand

Recompiles on save using `notify` + `ctrlc`. `run_watch_dir` tracks a reverse-dependency graph; editing a shared partial recompiles all transitive importers. `_`-prefixed partials are tracked but never emit their own `.md` output. `--format messages` not supported in directory mode.

## Component Interactions

The data flow is: lexer → parser → resolver → validator → evaluator → lib::build_output.

**Template inheritance interactions**:
- `ast.rs`: `ExtendsDirective`, `BlockNode`, `Module.extends` — parsed by parser, carried into resolver
- `parser.rs`: `parse_extends_if_present` (leading only), `parse_block` with `BlockGuard`; `inside_block` flag prevents nesting
- `resolver.rs`: `resolve_by_key_skeleton` → `process_module_skeleton` builds the skeleton cache entry (with `skeleton_origin`); `resolve_extends_components` (shared text+messages pipeline) assembles `final_body`, `effective_blocks` (with `Origin` per entry), and `skeleton_origin`; `validate_extends_components` walks `spliced_regions` per-region with correct origins; `evaluate` runs ONCE at the leaf on `final_body`
- `evaluator.rs`: `Node::Block` arm in `evaluate_nodes` calls `evaluate_block` (transparent in both standalone and post-splice modes); `Node::Block` arm in `collect_messages` descends into block bodies
- `validator.rs`: `validate_block_node` recurses into block body; runs against per-region slices (via `validate_extends_components`) at the leaf (ADR-016)
- `error.rs`: `MdsError::Extends` with constructor `extends_error_at` (span-carrying; no-span `extends_error()` was removed); `fn at()` defense-in-depth guard prevents miette `OutOfBounds` across all error variants

**Cross-cutting interactions** (unchanged):
- `parser_helpers.rs`: `parse_expr_inner` is the shared grammar for interpolation, directives, and `@message` role expressions
- `builtins.rs`: `get_builtin` called from both `validator.rs` and `evaluator.rs`
- `resolver.rs`: `parse_frontmatter_imports` (pub(crate)) used by `scan_imports` in `lib.rs`

## Integration Patterns

### Adding a `@block`-Aware Feature

`@block` nodes are transparent to the evaluator after inheritance resolution. The resolver's `splice_skeleton` replaces all `Node::Block` placeholders with their effective bodies before `validate` and `evaluate` are called. You do not need to add special block handling in the evaluator for features that work on body content — they automatically see the spliced-in content.

For features that need to operate on block structure before splicing (e.g., tooling that introspects block names), read from `ResolvedModule::effective_blocks` (an `IndexMap<String, EffectiveBlock>`).

### Adding a New Directive (Updated for Inheritance)

1. Add a new variant to `Node` in `ast.rs`
2. Parse: add a branch in `Parser::parse_directive()`
3. Validate: add a match arm in `validate_node()`
4. Resolve: handle in `collect_definitions_and_imports` (file I/O) or `build_scope_from_frontmatter`
5. Evaluate (text mode): add a match arm in `evaluate_nodes()`
6. Evaluate (messages mode): add handling in `collect_messages()` if the directive can appear outside `@message` blocks
7. **New for inheritance**: if the directive could appear in a base template body (i.e., it is not top-level-only), no special action is needed — `splice_skeleton` passes non-block nodes verbatim, and `evaluate_nodes` processes the spliced body normally. If the directive is top-level-only like `@extends`, enforce this in `check_child_only_blocks`.

### Adding a New `@message`-Aware Feature

If a feature needs to work inside `@message` bodies, verify behavior in both modes:
1. **Text mode**: body rendered via `evaluate_nodes` — new directive's arm handles it automatically.
2. **Messages mode**: `collect_messages` calls `evaluate_nodes` on each `@message` body. If the directive can appear *outside* a `@message` block, add a branch in `collect_messages`.
3. **Validator**: add a `Node::YourDirective` arm in `validate_node` — shared by all modes.

### Adding a Built-in Function

1. Add `BuiltinMeta` entry to `BUILTINS` static slice in `builtins.rs`
2. Add arm in `call_builtin` match
3. Write private handler using `require_string` helpers
4. Validator and evaluator auto-recognize via `get_builtin` — no changes needed

### Adding a New Expression Form

1. Add to `Expr` enum in `ast.rs`
2. Parse in `parse_expr_inner` in `parser_helpers.rs`
3. Evaluate in `evaluate_expr` in `evaluator.rs`
4. Validate in `validate_expr` in `validator.rs`

All four sites have exhaustive matches — missing arms produce compile errors.

### Adding a Frontmatter-Processed Key

Follow the pattern used by `type: mds` and `imports`:
1. Check for the key in `build_scope_from_frontmatter` in `resolver.rs`
2. For inheritance: also exclude it in `deep_merge_yaml`'s `RESERVED` list if it should not propagate through the base < child merge.
3. Strip from output via `strip_reserved_keys` in `lib.rs`.

### Adding a New Public API Function

1. Add `#[must_use]`
2. Add the symbol to `crates/mds-core/tests/api_surface.rs`
3. For user input, enforce resource limits
4. Follow the `*_collecting_warnings` naming pattern

## Anti-Patterns

- **Calling `eprintln!` from evaluator or resolver code** — use `ctx.warnings` or `warnings: &mut Vec<String>`.
- **Calling `evaluate` before `validate`** — the evaluator trusts all references exist.
- **Creating `ModuleCache` per-module instead of per-compile** — destroys caching.
- **Using bare `MdsError::syntax(msg)` when source context is available** — prefer `syntax_at`.
- **Directly interpolating `Value::Object`** — `{obj}` is a runtime error.
- **Adding a new `Arg` variant without updating all three match sites** — parser, evaluator, validator all match exhaustively.
- **Adding a new `Condition` variant without updating `validate_condition`**.
- **Adding a new `Expr` variant without updating all four match sites** — parser, evaluator, validator, and direct Expr matches in tests.
- **Adding a new `Node` variant without updating `collect_messages`** — missing arm silently drops the node.
- **Adding a new `Node` variant without updating `collect_definitions_and_imports`** — block collection and import resolution walk this function; a missing arm means the variant is ignored during resolver setup.
- **Calling `condition.root()` or `condition.path()`** — removed in PR #76. Match on the variant directly.
- **Looking for `required_param_count` in `evaluator.rs`** — it moved to `ast.rs` in PR #76.
- **Using `values_equal(Value, CondValue)` for condition equality** — replaced by `values_equal_runtime`.
- **Calling `arity` / `arity_at` with a single `expected` value** — both now require `expected_min` and `expected_max`.
- **Processing body `@import` before frontmatter `imports`** — frontmatter imports must resolve first.
- **Using `compile_messages*` on a template without `@message` blocks** — hard compile error.
- **Nesting `@message` inside another `@message`** — rejected at parse time.
- **Using `compile_collecting_warnings` for messages mode in the CLI** — messages mode calls `compile_messages_str_with_deps` via `read_build_input`.
- **Calling `std::fs::read_to_string` directly in CLI input paths** — route through `read_build_input` or `mds::compile_with_deps`. Avoids PF-004.
- **Skipping `validate_exports` in a new compilation code path** — both `process_module` and `process_module_messages` call it; omitting means `@export <undefined>` errors are silently dropped. Avoids PF-004.
- **Manually restoring `inside_message` or `inside_block` on error paths** — use `MessageGuard` / `BlockGuard` RAII structs.
- **Looking for `OutputFormat`, `BuildArgs`, `run_build_messages` in `main.rs`** — these live in `build.rs`.
- **Confusing `build::CompileOutput` with `mds::CompileOutput`** — different structs, different purposes.
- **Using `prompt_body.is_none()` to detect skeleton entries** — standalone modules with empty body also have `None`. Use `is_skeleton` flag.
- **Mutating a cached base's `effective_blocks` directly** — always `clone()` first (diamond-inheritance correctness). `apply_block_overrides` does this correctly.
- **Calling `validate` or `evaluate` on `module.body` in `process_module_extends`** — must call on `final_body` (the spliced result), not the raw child body. ADR-016.
- **Treating `@block` nesting as an `mds::extends` error** — nested `@block` is `mds::syntax` (parse-time); `mds::extends` is reserved for inheritance-semantic violations.
- **Adding a non-reserved key to the `RESERVED` list in `deep_merge_yaml` without updating the SYNC POINT comment in `strip_reserved_keys` (lib.rs)** — the two lists have DIFFERENT purposes and are intentionally not identical; `extends` is in `RESERVED` but not in `strip_reserved_keys`. Read both SYNC POINT cross-reference comments before editing either list.
- **Expecting `scan_imports` to return `@extends` path anywhere except first position** — it is always prepended as the leading dependency.
- **Calling `compile_all_dir` in `watch.rs`** — this function was removed. Use `compile_one_source`.
- **Calling `extends_error()` (no-span variant)** — it was removed; always use `extends_error_at()`. Both E3 (stray child content) and E4 (unknown block override) carry span context.
- **Skipping `validate_extends_components` in the messages-mode `@extends` branch** — both text and messages mode must call this helper (not `validator::validate(&final_body, ..., ctx.file_str, ctx.source)`); omitting it allows undefined-variable references in base `@block` defaults to silently pass (PF-004 parallel-path bypass) AND restores the cross-source `OutOfBounds` bug.
- **Calling `validator::validate(&final_body, ..., ctx.file_str, ctx.source)` in an `@extends` code path** — use `validate_extends_components` instead. Passing `ctx.source` (child source) with a `final_body` that may contain nodes from the base means spans will index the wrong source string, causing miette `OutOfBounds` in the CLI human render (this was the original bug fixed by ADR-022).
- **Accessing the `extends_path` field on `ResolvedModule`** — it was removed (dead code, never read). Use `module.extends` on the parsed AST module if you need the path.
- **Duplicating the `seed_effective_blocks` loop** — the extracted `seed_effective_blocks(body, block_names, origin)` helper eliminates the two former duplicated seeding loops; add new call sites rather than reimplementing the logic inline.
- **Adding a second, separate walk over the skeleton for validation or eval** — both must consume `spliced_regions` so they cannot drift. A separate walk that reconstructs the node list independently can silently diverge from what the evaluator sees (PF-004 lesson).
- **Allocating `Arc::from(ctx.source)` inside a per-block loop in `apply_block_overrides` or `seed_effective_blocks`** — the `override_origin` / `origin` is built ONCE outside the loop and `Arc::clone`d per entry. O(blocks×source) allocation would break the `p2_wide_base_200_blocks_under_1s` guard. Verified by `p_block_sources_share_one_arc` (`Arc::ptr_eq` assertion).
- **Printing source bytes in `Debug` output for `Origin`, `EffectiveBlock`, or `ResolvedModule`** — the `debug-panics` no-leak rule forbids source text in panic messages or debug output. All three types have manual `Debug` impls that elide source content; never add `#[derive(Debug)]` to them.

## Gotchas

- **`Condition` does not derive `PartialEq`** — `Expr::NumberLiteral(f64)` uses IEEE 754 where `NaN != NaN`.
- **`parse_expr_inner` is the unified grammar** — a bug in it affects interpolation, `@directive` expressions, and `@message` role expressions.
- **`required_param_count` is in `ast.rs`, not `evaluator.rs`**.
- **`MAX_LOGICAL_OPERANDS = 16` is a leaf count, not a per-level count**.
- **Frontmatter `imports` is stripped from output** — does not appear in rendered Markdown.
- **Empty `names: []` in frontmatter selective import is a compile error**.
- **`CondValue` and `Expr` literal types are near-duplicates** — tech debt issue #78.
- **`call_stack` is `Vec`, not `HashSet`** — O(n) scan at `MAX_CALL_DEPTH = 128`.
- **Orphan text in messages mode is a warning, not an error**.
- **Orphan `@include` in messages mode emits a warning**.
- **`@message` body content is evaluated in text mode** — `collect_single_message` calls `evaluate_nodes`; result is trimmed before storage.
- **`MAX_MESSAGES_TOTAL_SIZE` is a cumulative cap** — applies to sum of all content lengths across all messages.
- **Injection safety**: `@message`/`@end`/`@block`/`@extends` tokenization runs on original source before variable substitution. A variable containing literal `@end` cannot break out of a block body.
- **`EvalMessage` is `pub` and lives in `evaluator.rs`** — converted to public `mds::Message` in `lib.rs`. Do not expose `EvalMessage` through the public API.
- **`MAX_ARRAY_ELEMENTS` is not exported** — `pub(crate)` in `limits.rs`.
- **`compile_and_write` returns deps, not a boolean** — the watch loop uses the returned `Vec<String>` to update `dirs_to_watch`.
- **`OutputFormat` derives `clap::ValueEnum`** — adding a variant automatically adds a valid `--format` value; ensure intentional.
- **`is_skeleton = true` means prompt_body is always None** — but `prompt_body = None` does NOT imply `is_skeleton = true`. Check `is_skeleton` explicitly.
- **`effective_skeleton` is `Arc<[Node]>`, not `Arc<Vec<Node>>`** — constructed via `Arc::from(slice)`. The Arcs are cloned (not the nodes) across the inheritance chain. Total cost of an N-level chain is O(1) for skeleton sharing.
- **`effective_blocks` is `IndexMap<String, EffectiveBlock>` (not `IndexMap<String, Arc<BlockNode>>`)** — each entry now carries `EffectiveBlock { node: Arc<BlockNode>, origin: Origin }`. `Arc::clone` on unmodified blocks is O(1); `Origin` carries two `Arc<str>` so a clone is three refcount bumps total.
- **`deep_merge_yaml` arrays replace wholesale** — `[1, 2]` in base + `[3]` in child = `[3]` in merged, not `[1, 2, 3]`. This is intentional (decision #7).
- **`@block` names share the namespace with `@define` names** — a `@block foo:` and a `@define foo()` in the same module → `mds::name_collision`.
- **A child may only override blocks declared by the root base** — intermediate bases can add overrides but cannot introduce new block names. Only the root base declares `@block` placeholders. Attempting to override a name not in the root base → `mds::extends` (E4).
- **`process_module_skeleton` does NOT call validate/evaluate** — it is collect-only. A syntax error in the base template body is deferred to the leaf's `validate_extends_components` call (ADR-016: validate at the leaf, not at intermediate bases).
- **Diamond inheritance is safe** — `apply_block_overrides` always clones before mutating, so two children of the same base each get independent `effective_blocks` maps. The `Arc<BlockNode>` body pointers are shared but bodies are never mutated after construction.
- **`@extends` must be the first directive after frontmatter** — `parse_extends_if_present` only recognizes it in the leading position (after blank lines). A stray `@extends` mid-body → `mds::extends` error from `parse_directive`.
- **`compute_line_column` is character-based and boundary-safe** — returns `None` for offsets that land mid-character or beyond `source.len()`. `offset == source.len()` (exclusive-end) returns `Some`. Column counts Unicode scalar values, not bytes — `SerializedSpan.column` reflects visual column on multibyte lines (e.g., CJK 3-byte chars: byte-col 7 vs char-col 3). This prevents panics on multi-byte sources and ensures correct column reporting in the JSON error path.
- **`MdsError::at()` defense-in-depth guard** — when an offset+len pair is out-of-bounds for the source string passed to any `*_at` constructor, `src` is set to `None` (span offset/length are preserved). Miette never renders `OutOfBounds` in any path (CLI human or JSON). A `debug_assert!` fires in debug/test builds for the non-empty-source case, surfacing cross-source offset bugs loudly during development. The empty-source escape prevents false-fires on unit tests.
- **FM-carrying deep chains are O(N^2) in the merge path** — `deep_merge_yaml` is called at each level of `process_module_skeleton`, so a 32-level chain where every level adds FM keys accumulates merge work quadratically. The current implementation is correct and passes the `p5b_deep_chain_32_levels_with_frontmatter_under_2s` guard (< 2 s on typical hardware), but fixing the O(N^2) behaviour is tracked as tech debt.
- **Test name convention** — resolver test functions use domain-descriptive names throughout: UTF-8 boundary regression tests are `utf8_boundary_*`; PF-004 parity tests are `pf004_*`. These were originally named `task1_*` and `task2_*` but were renamed for consistency with the existing `f*/e*/a*/p*` convention. Do not search for the old task-prefixed names.
- **P2 perf bound is 1s** — the `p2_wide_base_200_blocks_under_1s` test (CLI integration, `inheritance.rs`) uses a 1-second wall-clock bound, relaxed from the original 200ms to avoid CI flakiness. The guard still catches O(N²) blowup; it is intentionally generous for CI variability.

## Key Files

- `crates/mds-core/src/limits.rs` — all cross-pipeline resource limits; `MAX_BLOCKS_PER_MODULE = 256` and `MAX_FRONTMATTER_MERGE_DEPTH = 64` added for inheritance
- `crates/mds-core/src/ast.rs` — all AST types; `ExtendsDirective`, `BlockNode`, `Node::Block`, `Module.extends` added; `Node::Message(MessageBlock)`; `Condition` variants; `required_param_count`
- `crates/mds-core/src/parser.rs` — `parse_extends_if_present`; `parse_block` with `BlockGuard` RAII; `inside_block` flag; `parse_message_block` with `MessageGuard`
- `crates/mds-core/src/resolver.rs` — orchestrator; `Origin` struct (file+source ride-along, manual Debug); `EffectiveBlock` struct (node+origin); `ResolvedModule` with `skeleton_origin` field; `spliced_regions` (single shared walk); `splice_skeleton` (thin adapter); `validate_extends_components` (region-aware validation helper); `resolve_by_key_skeleton`; `process_module_skeleton`; `resolve_extends_components`; `ExtendsComponents`; `deep_merge_yaml`; `build_scope_from_merged_mapping`; `apply_block_overrides`; `check_child_only_blocks`; `seed_effective_blocks`; `ModuleCache`; `FrontmatterImport`
- `crates/mds-core/src/evaluator.rs` — `Node::Block` arm (evaluate_block); `Node::Block` arm in collect_messages; `EvalContext`; `evaluate_messages` / `collect_messages`
- `crates/mds-core/src/validator.rs` — `validate_block_node`; `Node::Block` arm; `validate_message_node`; `validate_condition`
- `crates/mds-core/src/error.rs` — `fn at()` (defense-in-depth OOB guard, used by all `*_at` constructors); `MdsError::Extends` with `extends_error_at`; `compute_line_column` (character-based column, UTF-8 boundary safe); `SerializedSpan` (offset/length in bytes, column in Unicode scalar values)
- `crates/mds-core/src/error_tests.rs` — `compute_line_column_is_char_based` (CJK char-vs-byte column test); `line_col_out_of_bounds` (OOB returns None)
- `crates/mds-core/src/builtins.rs` — 18 built-in functions; `BuiltinMeta`; `get_builtin` / `call_builtin`
- `crates/mds-core/src/lib.rs` — public API; `scan_imports` (prepends `@extends` path first); `Message`; `CompileMessagesOutput`; `compile_messages*` family
- `crates/mds-cli/src/build.rs` — `OutputFormat`; `BuildArgs`; `compile_to_content`; `compile_and_write`; `read_build_input`
- `crates/mds-cli/src/watch.rs` — watch subcommand; reverse-dep graph; `compile_one_source`
- `crates/mds-cli/tests/inheritance.rs` — CLI integration tests for template inheritance; E12 diagnostic attribution tests: `e12_base_default_undefined_var_render_points_at_base` (asserts stderr names base.mds, contains "not defined", no `OutOfBounds`/`Failed to read contents`), `e12_check_and_build_diagnostics_match` (check and build agree on error code and base file)
- `crates/mds-core/tests/messages.rs` — integration tests for `@message` / messages mode
- `crates/mds-cli/tests/format_messages.rs` — CLI integration tests for `--format messages`
- `crates/mds-core/tests/api_surface.rs` — public API regression tests

## Related

- ADR-008: bundles related language features in single PR (template inheritance shipped as Issue #58/PR #95)
- ADR-010: reuse `parse_expr_inner` across interpolation and directive parsing — `@message` role expressions follow this; `@extends`/`@block` parsing uses the same tokenizer infrastructure
- ADR-014: frontmatter imports resolve before body `@import`; per-file base-vs-child import resolution in inheritance
- ADR-016: re-validate dynamically-assembled final_body at the leaf — `process_module_extends` and `process_module_messages` call `validate_extends_components` (not `validator::validate` directly with `ctx.source`), operating on per-region slices from `spliced_regions` rather than `module.body`
- ADR-022: diagnostic source-origin rides along the data (`Origin { file, source }` on each spliced block + `skeleton_origin` on the module) via `Arc::clone`, rather than being looked up from a path→source cache map at error time. The cache-map alternative was rejected because it cannot serve in-memory `<source>` entries and would flip the displayed filename. This fixes the cross-source-offset class (where a base-template AST offset was paired with the child's `NamedSource`, causing miette `OutOfBounds` in the CLI human render path). The JSON path's `compute_line_column` OOB guard (introduced in commit `b95a0ed`) provided degraded protection before; ADR-022 makes the human-render path equally robust via the `fn at()` defense-in-depth guard.
- PF-004: base reads go ONLY through `resolve_by_key_skeleton` / `FileSystem` trait — never `std::fs` — so `MAX_FILE_SIZE`, cycle detection, and `MAX_IMPORT_DEPTH` all apply to `@extends` bases in both text and messages mode; both modes now call `validate_extends_components` (sharing one `spliced_regions` walk) preventing parallel-path divergence
- `crates/mds-core/src/resolver.rs` — canonical reference for module system, inheritance pipeline, `ResolvedModule` inheritance fields, `Origin`, `EffectiveBlock`, `ExtendsComponents`, `spliced_regions`, `splice_skeleton`, `validate_extends_components`, `deep_merge_yaml`
- `crates/mds-core/src/evaluator.rs` — canonical reference for `EvalContext`, `evaluate_expr`, `evaluate_messages`, `collect_messages`, `evaluate_block`
- `crates/mds-core/src/scope.rs` — canonical reference for `CapturedScope`, `Arc<FunctionDef>`, closure capture API
- `crates/mds-core/src/ast.rs` — canonical reference for all AST types including `BlockNode`, `ExtendsDirective`; start here for new directive forms
- `crates/mds-cli/tests/` — end-to-end tests across 12+ categorized files including `inheritance.rs`
- Tech debt: issue #77 (ScanState extraction), #78 (CondValue/Expr unification), #79 (parse_interpolation_expr delegation), #80 (parse_simple_condition complexity); O(N^2) FM merge in deep chains (untracked, documented in p5b test); `SkeletonEntry`/`StandaloneEntry` enum split to replace `is_skeleton` flag (untracked)
