---
feature: mds-compiler
name: MDS Compiler
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, or modifying CLI output behavior (file output, project config). Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, export, include, define, for, if, closure, lexical scope, prompt export, nested function calls, arg parsing, warnings, quiet mode, stdin, auto-detect, compile_file, reexport, EvalContext, CapturedScope, IndexSet, Arc, exit_code, mds.json, output_dir, out_dir, default output, file output, MdsConfig, BuildConfig, load_config, resolve_output_path, derive_output_filename, non_exhaustive, pub(crate), run_build, run_check, run_init, MAX_TRAVERSAL_DEPTH, MAX_NESTING_DEPTH."
category: architecture
directories: [src/, tests/]
referencedFiles:
  - src/lib.rs
  - src/ast.rs
  - src/lexer.rs
  - src/parser.rs
  - src/validator.rs
  - src/resolver.rs
  - src/evaluator.rs
  - src/scope.rs
  - src/value.rs
  - src/error.rs
  - src/main.rs
created: 2026-05-12
updated: 2026-05-15
---

# MDS Compiler

## Overview

MDS (Markdown Script) is a Rust compiler that transforms `.mds` files — Markdown with `@directives` and `{var}` interpolation — into plain Markdown. The primary use case is composable LLM prompt templates: authors write templates with variables, conditionals, loops, and reusable function fragments, then compile them to a final prompt string.

The compilation pipeline is strictly sequential: **lexer → parser → validator → resolver → evaluator → render**. Each layer has a single responsibility and communicates through typed interfaces rather than shared mutable state. The `resolver` is the orchestrator — it drives all other stages and manages the module cache used for imports.

## System Context

The binary is a CLI tool (`mds build`, `mds check`, `mds init`) backed by a library crate. The library exposes these public functions:

| Function | Purpose |
|---|---|
| `compile(path, runtime_vars)` | Compile a file to Markdown, printing warnings to stderr |
| `compile_file(path: &str)` | Convenience wrapper: calls `compile(Path::new(path), None)` — no runtime vars |
| `compile_str(source)` | Compile from string, no options |
| `compile_str_with(source, base_dir, runtime_vars)` | Compile from string with options |
| `compile_collecting_warnings(path, runtime_vars)` | Compile and return `(String, Vec<String>)` — caller controls warning output |
| `compile_str_collecting_warnings(source, base_dir, runtime_vars)` | String variant of the above |
| `check(path, runtime_vars)` | Validate a file without rendering |
| `check_str(source)` | Validate from string, no options |
| `check_str_with(source, base_dir, runtime_vars)` | Validate from string with options |
| `check_collecting_warnings(path, runtime_vars)` | Validate and return `((), Vec<String>)` — caller controls warning output |
| `check_str_collecting_warnings(source, base_dir, runtime_vars)` | String variant of the above |
| `load_vars_file(path)` | Load runtime vars from a JSON file |

`compile_file` is the simplest entry point for embedding MDS in tools that already have a path as `&str`. It does not accept runtime vars; use `compile` directly when runtime overrides are needed.

All public `compile*` and `check*` functions carry `#[must_use = "..."]` attributes. The Rust compiler will warn if a caller discards the return value — discarding compiled output is almost certainly a bug. When adding new public API functions, include `#[must_use]`.

All compile/check functions funnel through `ModuleCache::resolve` / `ModuleCache::resolve_source`, which is the single entry point to the full pipeline. The CLI and any programmatic callers share exactly the same compilation behavior.

**Warning collection pattern**: Warnings (e.g. empty `@include`) are passed as a `&mut Vec<String>` through the full pipeline — `process_module` → `evaluate` → `evaluate_nodes` → `evaluate_include`. Nothing in the evaluator or resolver calls `eprintln!` directly. The public `compile*` variants print warnings by calling `emit_warnings(&warnings)` on the collected `Vec`. The `compile_collecting_warnings` variants return warnings without printing — this is what the CLI build command uses so it can gate output on the `--quiet` flag.

The CLI `build` and `check` commands both accept `-` as the input path to read from stdin, resolved against the current working directory for import paths. When the `input` argument is omitted entirely, both commands call `auto_detect_mds_file()` to scan the CWD for a single `.mds` file. If zero or multiple `.mds` files are found, a diagnostic error with hints is returned.

External dependencies are minimal: `clap` for CLI parsing, `serde_json` and `serde_yaml` for frontmatter and runtime vars, `miette`/`thiserror` for rich diagnostic errors, `indexmap` for the cycle-detection `IndexSet`, `tempfile` in tests.

## Component Architecture

### Token Model (`src/lexer.rs`)

The lexer converts raw source text into a flat `Vec<Token>` via the public `tokenize(source, file)` function. Internally, this creates a `Lexer<'a>` struct and calls `.run()`. The `Lexer` struct encapsulates all mutable scanning state (`pos`, `tokens`, `code_fence_backticks`) and the pre-computed `chars: Vec<char>` and `byte_offsets: Vec<usize>` arrays. The monolithic loop has been decomposed into focused `scan_*` methods: `scan_frontmatter`, `scan_code_fence`, `scan_code_content`, `scan_directive`, `scan_escape`, `scan_interpolation`, `scan_text`. The public API (`tokenize`) is unchanged.

Token variants cover the complete surface syntax:

- `Text(String, usize)` — raw passthrough text with byte offset
- `Interpolation(String, usize)` — inner content of `{...}` without braces
- `EscapedBrace(usize)` — `\{` → literal `{` at evaluation time
- `Directive(String, usize)` — full line starting with `@`
- `FrontmatterFence(usize)` / `FrontmatterContent(String, usize)` — YAML block
- `CodeFence(String, usize)` / `CodeContent(String, usize)` — fenced code blocks

Code blocks are tokenized as opaque `CodeContent` — no interpolation or directive parsing occurs inside triple-backtick regions. This is enforced at the lexer level; the rest of the pipeline never needs to check for this case.

### AST (`src/ast.rs`)

The `Module` struct holds an optional `Frontmatter` and a `Vec<Node>`. `Node` is an enum with variants for every construct: `Text(TextNode)`, `Interpolation`, `EscapedBrace`, `If`, `For`, `Define`, `Import`, `Export`, `Include`.

`TextNode` is a struct (`{ text: String }`) with no offset field — offsets are not tracked for raw text. `EscapedBrace` is a unit variant with no fields. Expressions inside `{...}` are further typed as `Expr::Var`, `Expr::Call`, or `Expr::QualifiedCall`.

**`Arg` enum** has three variants — this is the complete set:

| Variant | Meaning |
|---|---|
| `Arg::StringLiteral(String)` | Quoted string literal, e.g. `"hello"` |
| `Arg::Var(String)` | Variable reference, e.g. `name` |
| `Arg::Call { name, args: Vec<Arg> }` | Nested function call, e.g. `inner("arg")` |

`Arg::Call` enables arbitrary nesting: `{outer(inner("arg"))}` parses as `Expr::Call { args: [Arg::Call { ... }] }`. Depth is bounded by `MAX_NESTING_DEPTH = 256` in the parser.

All non-text AST nodes carry a byte `offset` into the original source. This is threaded through to `MdsError` variants to produce precise source-span diagnostics via `miette`.

### Scope (`src/scope.rs`)

`Scope` is a stack of `Frame` structs (innermost last). Each frame holds:
- `vars: HashMap<String, Value>` — variable bindings
- `functions: HashMap<String, Arc<FunctionDef>>` — `@define` functions stored as `Arc` for O(1) clone
- `namespaces: HashMap<String, NamespaceScope>` — aliased imports (`@import "x" as ns`)

Lookup always walks from innermost to outermost frame, enabling block-scoped shadowing. `push()`/`pop()` are called around `@for` iterations and function calls. `pop()` returns `Result<(), MdsError>` — it returns an error if called when only the global scope frame remains, surfacing mismatched push/pop as a compiler-bug diagnostic rather than a panic. All callers use `scope.pop()?`.

**`CapturedScope` struct** bundles the three closure capture fields that were previously separate fields on `FunctionDef`:

```rust
// CapturedScope replaces three separate captured_* fields on FunctionDef.
// functions is owned (not Arc) to avoid reference cycles between captured functions.
pub struct CapturedScope {
    pub namespaces: HashMap<String, NamespaceScope>,
    pub functions: HashMap<String, FunctionDef>,  // owned, not Arc
    pub vars: HashMap<String, Value>,
}

pub struct FunctionDef {
    pub params: Vec<String>,
    pub body: Vec<Node>,
    pub captured: CapturedScope,  // single field, not captured_namespaces/captured_functions/captured_vars
}
```

Key consequence: code that previously wrote `func.captured_namespaces = ...` now writes `func.captured.namespaces = ...`.

`FunctionDef::from(&DefineBlock)` creates a `FunctionDef` with `captured: CapturedScope::default()` (all empty maps). The resolver fills `captured.namespaces`, `captured.functions`, and `captured.vars` after construction. Never assume captures are populated immediately after `FunctionDef::from`.

**`Arc` in scope and modules**: `Frame::functions` and `NamespaceScope::functions` store `Arc<FunctionDef>`. `Scope::set_function` takes `Arc<FunctionDef>`; `get_function` returns `Option<&Arc<FunctionDef>>`. `CapturedScope::functions` stores owned `FunctionDef` (not `Arc`) to break potential reference cycles.

Helper methods `get_all_namespaces()`, `get_all_functions()`, `get_all_vars()` snapshot the current scope for closure capture at definition time. All three iterate **outer frame to inner frame**, so when the same key appears in multiple frames, the inner (more recently defined) value wins. `get_all_functions()` returns `HashMap<String, Arc<FunctionDef>>` — callers that need owned captures must convert via `.map(|(k, v)| (k, (*v).clone()))`. All three delegate to the private `collect_all<T: Clone>(get: impl Fn(&Frame) -> &HashMap<String, T>)` method — to add a new `get_all_X()` for a new `Frame` field, add the field to `Frame` and call `self.collect_all(|f| &f.x)`.

### Value System (`src/value.rs`)

The `Value` enum has five variants: `String`, `Number(f64)`, `Boolean`, `Array(Vec<Value>)`, `Null`. Objects/maps are explicitly unsupported in v0.1. Truthiness rules match JavaScript-like semantics: `0`, `""`, `[]`, `null`, `false`, and `NaN` are falsy; everything else is truthy.

**`#[non_exhaustive]`**: The `Value` enum is marked `#[non_exhaustive]`. This means external crates cannot exhaustively match on it without a wildcard arm. It is intentional: it allows adding new value variants without a semver break. Within the crate all `match` arms remain exhaustive; you do not need `_` inside the library.

**`pub(crate)` converters**: Both `Value::from_yaml` and `Value::from_json` are `pub(crate)` — they are intentionally not part of the public API. External consumers receive `Value` via frontmatter parsing and runtime var injection, not by constructing it from raw YAML/JSON. This prevents library users from relying on internal parsing behavior.

`Value::Display` renders numbers as integers when the fractional part is zero, guarding against i64 overflow for very large floats. Arrays display as comma-separated values. `Null` displays as empty string.

Both `from_yaml` and `from_json` converters exist, with identical rejection of object/map types. `from_yaml` also handles `serde_yml::Value::Tagged` by unwrapping the tag and recursing.

The `Value` enum implements `From` for common Rust types: `&str`, `String`, `f64`, `i64`, `i32`, `bool`, and `Vec<T: Into<Value>>`. Use these conversions in test setup and programmatic API code rather than constructing enum variants directly.

### Parser (`src/parser.rs`)

The parser converts a token stream to a `Module` AST. Key hardening:

- `pub(crate) const MAX_NESTING_DEPTH: usize = 256` — `pub(crate)` (not private) so `src/validator.rs` can import it for `validate_var_args`'s depth guard; enforced via a `depth` counter on the parser struct; shared between two independent limits: (1) `@if`/`@for`/`@define` block nesting via `enter_block()`, and (2) nested function call argument depth via `parse_args_inner`
- `enter_block()` — extracted helper that increments `self.depth` and returns `Err` if the limit is exceeded; called at the start of `parse_if_block`, `parse_for_block`, and `parse_define_block`, with matching `self.depth -= 1` on exit
- `is_valid_identifier(s)` — all directive names (function names, loop vars, aliases, export names) are validated: must start with ASCII letter or `_`, contain only ASCII alphanumeric or `_`
- Duplicate `@define` parameter names are rejected at parse time
- `@else` without colon gives a targeted error message ("use '@else:' with trailing colon")

**Argument parsing internals**: `parse_args` and `parse_single_arg` are thin public wrappers that delegate to `parse_args_inner(s, depth)` and `parse_single_arg_inner(s, depth)`. The `_inner` variants carry the recursion depth. When a `parse_single_arg_inner` encounters `name(...)` syntax, it produces `Arg::Call` and recurses via `parse_args_inner(inner, depth + 1)`.

`parse_args_inner` tracks open parentheses (`paren_depth`) so that commas inside nested calls are not treated as argument separators at the outer level. Quote-escaped commas inside string arguments are similarly skipped.

Note: `parse_single_arg` (without `_inner` suffix) exists only under `#[cfg(test)]` as a test shim. In production code only `parse_single_arg_inner(s, 0)` is called directly (or via `parse_args`).

### Validator (`src/validator.rs`)

Validates the AST against the current scope **before** evaluation. Catches: undefined variables in `{interpolation}` and `@if` conditions, undefined iterables in `@for`, undefined namespaces in `@include`, undefined functions and arity mismatches in calls, and undefined variable arguments to functions.

**`validate()` signature**: `pub fn validate(nodes: &[Node], scope: &mut Scope, file: &str, source: &str) -> Result<(), MdsError>`. The scope parameter is `&mut Scope` — the validator uses `scope.push()` / `scope.pop()` directly for `@for` and `@define` body recursion instead of cloning. This is more efficient and avoids a clone-per-block overhead.

**`@for` body validation**: The validator calls `scope.push()`, injects the loop variable as `Value::Null`, recurses via `validate()`, then calls `scope.pop()`. The `pop()` result is discarded with `let _ = scope.pop()` because we just pushed — it cannot fail.

**`@define` body validation**: The validator calls `scope.push()`, injects all params as `Value::Array(vec![])`, recurses via `validate()`, then calls `scope.pop()`. Using an empty array — rather than `Null` — allows `@for item in param:` inside the define body to pass the array type check at validation time. The actual runtime type of arguments is enforced by the evaluator at call time.

**`@for` iterable type check**: The validator checks that the iterable is a `Value::Array` at validation time, using `type_error_at` to attach a source span. This is an early-exit check: non-array iterables fail at validate time with a precise source location, not at evaluation time.

**`validate_var_args`** is extended to cover all three `Arg` variants:
- `Arg::StringLiteral` — no validation needed
- `Arg::Var` — variable existence check against scope
- `Arg::Call { name, args }` — function existence check, arity check against `func.params.len()`, then recursion into `inner_args` via `validate_var_args`

This means nested calls like `{outer(inner("arg"))}` are fully validated: both `outer` and `inner` must exist with correct arity before evaluation. For `Arg::Call` arity errors, the span length is `name.len()` (not the full expression length). `validate_var_args` accepts a `depth: usize` parameter that limits recursive validation depth — analogous to `resolve_args`'s depth guard in the evaluator; the parser already enforces `MAX_NESTING_DEPTH = 256` on arg nesting so this acts as a safety belt.

The `arity_at` constructor provides source-span-aware arity errors from the validator, in addition to the existing `undefined_var_at`, `undefined_fn_at`, `name_collision_at`, etc.

### Resolver (`src/resolver.rs`)

The resolver is the orchestrator. `ModuleCache` drives the full pipeline for each file and caches `Arc<ResolvedModule>` by canonical path, preventing repeated work and providing cycle detection.

**Project root detection**: `find_project_root` walks up from the entry file's directory looking for `.git` or `.mdsroot` markers. The found root is stored in `ModuleCache::root_dir` on first resolve. All subsequently resolved paths must `starts_with(root_dir)` — this is the path traversal boundary. The walk is bounded by `MAX_TRAVERSAL_DEPTH = 256` (named constant, same value as main.rs).

**Security guards** — split across focused private methods (extracted from the old monolithic `validate_and_read_file`):

`canonicalize_and_check` (always):
1. `check_symlink(path)` — free method; detects symlinks by comparing `canonical_parent.join(filename)` vs `full_canonicalize()`; if they differ, returns `ImportError`; called first before any other check
2. `root_dir` initialization — set on first resolve by calling `find_project_root` from the entry file's directory
3. `check_import_depth(&self)` — rejects chains deeper than `MAX_IMPORT_DEPTH = 64`; checked via `resolving.len()`
4. `check_path_traversal(&self, canonical)` — resolved canonical path must `starts_with(root_dir)`

`read_validated_file` (cache misses only):
5. Reads bytes first, then checks size against `MAX_FILE_SIZE = 10MB` — reading first avoids a TOCTOU race between a metadata call and the actual read

**Import helpers** — each `ImportDirective` variant dispatches to a dedicated private method:
- `resolve_alias_import(path, alias, offset, scope, ctx, warnings)` — calls `validate_import_path`, resolves, calls `scope.set_namespace`
- `resolve_merge_import(path, offset, scope, ctx, warnings)` — brings all exports + `prompt` body into scope; frontmatter vars not imported
- `resolve_selective_import(names, path, offset, scope, ctx, warnings)` — imports only named exports; `prompt` binds as a variable via `scope.set_var`, others as functions via `scope.set_function`

**Cycle detection** uses `IndexSet<PathBuf>` (`indexmap` crate) — this single data structure replaces the previous `HashSet<PathBuf> + Vec<PathBuf>` pair (`resolving` + `resolving_stack`). `IndexSet` provides O(1) membership test (like `HashSet`) plus insertion-ordered iteration (like `Vec`), so it serves both purposes. `shift_remove` preserves insertion order of remaining elements when unmarking.

`build_cycle_string` reconstructs the cycle path by finding the repeated path in the `IndexSet`'s ordered sequence using `.position(...).unwrap_or(0)`. The `.unwrap_or(0)` is a safe fallback. A private `path_display_name` helper extracts the filename portion of each path for display in cycle strings.

**`process_module` decomposition**: The previous monolithic `process_module` function has been split into focused helpers:
- `build_scope_from_frontmatter(frontmatter, is_md, runtime_vars)` — free function; parses YAML, populates scope, applies runtime var overrides; skips `type` key for `.md` files
- `collect_definitions_and_imports(body, scope, ctx, warnings)` — `ModuleCache` method; walks AST dispatching to three sub-helpers: `collect_define` (free fn, per-`@define` node — captures closure scope), `collect_export` (ModuleCache method, per-`@export` node — handles named/re-export/wildcard), and `resolve_import` (ModuleCache method, per-`@import` node — calls `resolve()` recursively and merges results into scope); returns `CollectedDefs`
- `validate_exports(explicit_exports, functions)` — free function; checks every named export refers to a defined function or `"prompt"`
- `canonicalize_and_check(path)` — `ModuleCache` method; performs all security checks WITHOUT reading the file: delegates to `check_symlink`, `check_import_depth`, `check_path_traversal`; called on every resolve including cache hits, so cache hits pay only the cost of two `canonicalize` syscalls and no I/O
- `read_validated_file(canonical)` — free function taking `&Path`; reads the file as bytes then checks size against `MAX_FILE_SIZE`; called only on cache misses
- `attach_import_span(err, path, file_str, source, offset)` — free function; re-annotates `FileNotFound` and `CircularImport` errors from recursive `resolve()` calls to point to the `@import` directive in the parent file

`process_module` itself is now a ~25-line orchestrator that calls these helpers in sequence.

**`ModuleCtx` struct** bundles the borrowed per-module context threaded through AST walk helpers (`file_str`, `source`, `base_dir`, `runtime_vars`), reducing parameter lists.

**`CollectedDefs` struct**: A named struct (not a type alias) returned by `collect_definitions_and_imports`. Fields: `functions: HashMap<String, Arc<FunctionDef>>`, `has_explicit_exports: bool`, `explicit_exports: HashSet<String>`. Destructured with named field syntax in `process_module` (`let CollectedDefs { functions, has_explicit_exports, explicit_exports } = ...`).

**`Arc<ResolvedModule>`**: `ModuleCache::modules` stores `Arc<ResolvedModule>`. Both `resolve()` and `resolve_source()` return `Arc<ResolvedModule>`. Cache hits clone the `Arc` (O(1)); misses wrap the new `ResolvedModule` in `Arc::new(resolved)` then insert and return a clone.

**`ResolvedModule`** fields:
- `functions: HashMap<String, Arc<FunctionDef>>` — all `@define`d functions (including re-exports); stored as `Arc` for O(1) clone
- `prompt_body: Option<String>` — rendered body text, or None if empty
- `has_explicit_exports: bool` — true once any `@export` appears
- `explicit_exports: HashSet<String>` — the explicitly listed export names

**`ResolvedModule` methods**:
- `get_export(name)` → `Option<Arc<FunctionDef>>` — returns `None` if the module has explicit exports and `name` is not in the list; otherwise clones `Arc` from `functions`
- `get_all_exports()` → `Vec<(String, Arc<FunctionDef>)>` — returns all exported (name, Arc) pairs, filtered by `explicit_exports` when present
- `get_prompt_value()` — returns `prompt_body` as `Value::String` if it is an available export; `None` otherwise
- `to_namespace()` — converts to `NamespaceScope`; respects export visibility for both `functions` and `prompt_body`

**`prompt` as an export**: Any module with a non-empty body implicitly exports it as `prompt`, unless the module has explicit exports and `"prompt"` is not listed. Importers can bring in the body text:
- `@import { prompt } from "./module.mds"` → binds body text to `prompt` variable
- Merge import of a module with a body → `prompt` variable brought into scope

**Export validation**: After collecting all `@export` directives, the resolver checks every named export either refers to a defined function or is the string `"prompt"`. Exporting an unknown name is a compile error. For re-exports (`@export name from "path"`), the source module is resolved first and `get_export(name)` is called — if `None`, an export error is returned immediately.

**Import semantics**:
- **Alias** (`@import "path" as ns`): resolved module becomes a `NamespaceScope` under `ns`; functions accessed as `{ns.fn(arg)}`
- **Merge** (`@import "path"`): all exported functions brought into scope; frontmatter variables from the imported module are NOT brought in (only functions and `prompt` body)
- **Selective** (`@import { fn } from "path"`): only named exports brought in; `prompt` is handled specially (bound as a variable, not a function)

**Re-export semantics** (`@export name from "path"`, `@export * from "path"`): The source module is resolved and its exports are added to the current module's `functions` map. They are NOT added to the current file's runtime scope — they are only available to modules that import the current module. If a named re-export target does not exist in the source module's exports, the error is raised at the re-export site (not at the consumer), with a message of the form `"cannot re-export '{name}': not exported from \"{path}\""`.

**Closure capture**: When a `@define` node is processed, the resolver calls `FunctionDef::from(def)` (which creates empty captures), then immediately fills `func.captured.namespaces`, `func.captured.functions`, and `func.captured.vars` from the current scope state. `captured.functions` is populated by converting `Arc<FunctionDef>` → owned `FunctionDef` (via `(*v).clone()`) to avoid reference cycles.

### Evaluator (`src/evaluator.rs`)

The evaluator walks the AST and produces the final rendered string. Its public entry point is `evaluate(nodes, scope, warnings)` — the `warnings: &mut Vec<String>` parameter is threaded through all internal helpers including `evaluate_include`. Nothing in the evaluator calls `eprintln!` directly; all diagnostics go into the warnings vec.

**`EvalContext` struct** bundles three mutable state fields that were previously threaded individually through every function signature:

```rust
pub(crate) struct EvalContext<'a> {
    call_stack: Vec<String>,          // recursion detection (was HashSet<String>)
    total_iterations: usize,          // cumulative @for iterations
    warnings: &'a mut Vec<String>,    // non-fatal diagnostics
}
```

`evaluate()` allocates an `EvalContext` and passes `&mut ctx` to `evaluate_nodes`. All internal functions that previously took `call_stack`, `total_iterations`, and `warnings` as separate parameters now take `ctx: &mut EvalContext`. The `warnings` field in `EvalContext` is the same `&mut Vec<String>` passed by the public `evaluate()` entry point — not a copy.

`call_stack` is now `Vec<String>` (not `HashSet<String>`). Recursion detection uses `ctx.call_stack.iter().any(|s| s == call_key)` — O(n) scan at MAX_CALL_DEPTH=128, which is acceptable. The LIFO property is verified with `assert!` (not `debug_assert!`) after each call returns — this assertion is safety-critical and runs in release mode; a mismatched pop would silently corrupt recursion state, so the cost (negligible at MAX_CALL_DEPTH = 128) is justified.

Five resource limits guard against runaway compilation:
- `MAX_CALL_DEPTH = 128` — prevents stack overflow from deeply nested function calls
- `MAX_LOOP_ITERATIONS = 100,000` — enforced per `@for` loop; raising this by one over the limit triggers a `ResourceLimit` error at evaluation time
- `MAX_TOTAL_ITERATIONS = 1,000,000` — cumulative across all loops in one compilation pass; nested loops that individually fit within the per-loop limit can still be rejected here; tracked via `ctx.total_iterations`
- `MAX_OUTPUT_SIZE = 50 MB` — checked after each node renders; returning `ResourceLimit` the moment the buffer exceeds this size rather than at the end
- `MAX_WARNINGS = 1,000` — once the warnings vec reaches this size, `evaluate_include` silently skips further pushes; prevents unbounded vec growth from templates with many empty `@include` directives

All limits return `MdsError::ResourceLimit` (no source span). If you add a warning-emitting path or a new iterable node, pass `ctx` through so `total_iterations` and `warnings` are respected.

**`Node::Define` in the evaluator**: The evaluator's `Node::Define` arm is a deliberate no-op — the implementation contains only `// Handled by resolver with full lexical capture`. All function registration (including closure capture into `captured.namespaces`, `captured.functions`, `captured.vars`) happens in the resolver's pre-evaluation AST walk. The evaluator skips `@define` nodes entirely, relying on the scope the resolver built. The resolver's pre-evaluation pass is therefore load-bearing for all function calls, including cross-module ones.

`invoke_function` restores the function's captured closure scope from `func.captured` before binding parameters, so params shadow captured vars correctly. It accesses `func.captured.namespaces`, `func.captured.functions` (wrapped in `Arc::new(f.clone())` for scope insertion), and `func.captured.vars`. After evaluation the pushed frame is popped using the double-fault error-preservation pattern, extracted into the private `prefer_first_error(first: Result<T, MdsError>, second: Result<(), MdsError>) -> Result<T, MdsError>` helper: on a render error, the render error wins; on render success + pop error, the pop error is returned; on both success, the rendered string is returned. This matches the same pattern used in `evaluate_for`.

**`resolve_args` signature**: `resolve_args(args: &[Arg], scope: &mut Scope, ctx: &mut EvalContext, depth: usize) -> Result<Vec<Value>, MdsError>`. The `ctx` parameter carries `call_stack` and `warnings` so `Arg::Call` can invoke `call_function` during argument resolution. `depth` tracks argument expression nesting and is checked against `MAX_CALL_DEPTH` to prevent unbounded recursion through argument nesting alone.

The `Arg::Call` arm in `resolve_args` recursively calls `resolve_args` for inner args, then `call_function` for the nested invocation, wrapping the `String` result in `Value::String`. This means the result of a nested call is always a `String` value regardless of what the inner function produces.

`@include alias` looks up the aliased module's `prompt_body` from the namespace and injects it inline. If the included module has no body text, `evaluate_include` pushes a warning message to `ctx.warnings` (not `eprintln!`) and returns an empty string.

`@import` and `@export` nodes are no-ops in the evaluator (handled entirely by the resolver).

### Error System (`src/error.rs`)

**`#[non_exhaustive]`**: `MdsError` is marked `#[non_exhaustive]`. External crates cannot exhaustively match on it without a wildcard arm, allowing new variants to be added without a semver break. Within the crate, all match arms are exhaustive and do not need `_`.

**`pub(crate)` constructors**: All `MdsError` constructor methods (`syntax`, `syntax_at`, `undefined_var`, `undefined_var_at`, etc.) are `pub(crate)`. They are not part of the public API — error construction is internal to the compiler. Library users receive `MdsError` values only via the `Result` return from public API functions. External code may inspect the error via `Display`, `Debug`, or `Diagnostic` (miette), but cannot construct variants directly.

**`_at` constructors**: Every major `MdsError` variant has a corresponding `_at` constructor that accepts `(file: &str, source: &str, offset: usize, len: usize)` and populates the `span` and `src` fields for miette rich diagnostics. Always prefer `_at` variants inside the validator and evaluator where source offsets are available from the AST nodes.

### CLI (`src/main.rs`)

The CLI logic has been extracted from `run()` into three dedicated functions:

- `run_build(input, output, out_dir, vars, set_vars, quiet)` — handles the complete build flow: auto-detect input, reject directory input, load config, resolve output path, compile, write output or print to stdout
- `run_check(input, vars, set_vars, quiet)` — handles the complete check flow: auto-detect input, reject directory input, compile (no output), print warnings and OK status
- `run_init(filename, force, quiet)` — creates a starter `.mds` file; rejects `..` components in the filename before writing

`run()` in `main.rs` dispatches to one of these three functions. This decomposition means each function has a single responsibility and is independently testable.

**Named constant — single source of truth**: `MAX_TRAVERSAL_DEPTH: usize = 256` is defined once in `src/resolver.rs` as `pub(crate)`, re-exported from `src/lib.rs` as `pub const MAX_TRAVERSAL_DEPTH`, and imported in `src/main.rs` via `use mds::MAX_TRAVERSAL_DEPTH`. There is no second definition in `main.rs`. Both the `load_config` upward walk and the `find_project_root` upward walk share this single value via the public library re-export.

**Safety fix in `auto_detect_mds_file`**: The multiple-files branch uses `names.first().map(|s| s.as_str()).unwrap_or("<file>.mds")` instead of `names[0]` to avoid a panic on an empty sorted list.

**`load_config` TOCTOU fix**: `load_config` reads the file bytes first via `fs::read`, then checks `bytes.len() as u64 > MAX_CONFIG_SIZE`. This avoids the TOCTOU race that a `metadata().len()` check before `read()` would introduce.

## Component Interactions

The data flow is:

```
source text
  → lexer::tokenize()  → Vec<Token>
  → parser::parse()    → Module (AST)
  → resolver: scope built from frontmatter + runtime_vars  (build_scope_from_frontmatter)
  → resolver: imports resolved recursively (ModuleCache)   (collect_definitions_and_imports)
    → closure scope captured into FunctionDef.captured.*
  → resolver: export names validated                       (validate_exports)
  → validator::validate()  (uses scope snapshot, &mut Scope)
  → evaluator::evaluate(&mut warnings) via EvalContext     → String (raw prompt body)
  → lib::clean_output()    → final Markdown string
```

**Warning propagation**: the `warnings: &mut Vec<String>` vector is allocated in the public API function and passed all the way through `ModuleCache::resolve` → `process_module` → `evaluate` → inside evaluator as `ctx.warnings`. After the pipeline completes, the calling code decides whether to print them (via `emit_warnings`) or return them to the caller (via `compile_collecting_warnings`).

Runtime variables override frontmatter: in `build_scope_from_frontmatter`, frontmatter vars are loaded first, then runtime vars overwrite any key that appears in both. This means `--vars` JSON and `--set KEY=VAL` always win over template defaults.

The `ModuleCache` is created per top-level compile call (not shared across calls). Each entry in `modules` is an `Arc<ResolvedModule>` — cache hits clone the Arc (O(1)) rather than cloning the full struct.

## Integration Patterns

### Adding a New Directive

1. Add a new variant to `Node` in `src/ast.rs` (and any needed sub-structs)
2. Lex: directives are already captured as `Token::Directive` — no lexer change required unless new syntax (e.g., new brace-form)
3. Parse: add a branch in `Parser::parse_directive()` matching the `@name` prefix; validate identifier names with `is_valid_identifier()`
4. Validate: add a match arm in `validate_node()` — validate what the resolver can't catch
5. Resolve: if the directive requires file I/O (import-like), handle it in `collect_definitions_and_imports`; if it only builds scope, handle it in `build_scope_from_frontmatter` or a new helper
6. Evaluate: add a match arm in `evaluate_nodes()` — if the directive can emit warnings, accept and forward via `ctx.warnings`; `Import`/`Export` stay as no-ops there
7. Add integration test fixture in `tests/fixtures/` and a test in `tests/integration.rs`

### Adding a New Arg Variant

If you add a fourth `Arg` variant, update all three sites that match on `Arg`:
1. `parse_single_arg_inner` in `src/parser.rs` — construct the new variant
2. `resolve_args` in `src/evaluator.rs` — evaluate to a `Value`
3. `validate_var_args` in `src/validator.rs` — pre-evaluation validity check

Failing to update any one of these produces an incomplete `match` compilation error, which is intentional — `Arg` has no wildcard arm.

### Warning-Emitting Code

Any code that needs to emit a non-fatal diagnostic must accept `warnings: &mut Vec<String>` and push to it. Never call `eprintln!` from evaluator, resolver, or library code. The CLI controls whether to print warnings based on the `--quiet` flag.

Inside the evaluator, warnings are accessed via `ctx.warnings`. When writing new evaluator helpers, accept `ctx: &mut EvalContext` rather than separate parameters for `call_stack`, `total_iterations`, and `warnings`.

The two-tier API pattern in `lib.rs`:
- `compile(path, vars)` — internal convenience that calls `compile_collecting_warnings` then `emit_warnings`
- `compile_collecting_warnings(path, vars)` — returns `(String, Vec<String>)` — use this when the caller needs to gate warning output (e.g., the CLI's quiet mode)

**`resolve_base_dir` helper**: Both `check_str_with` and `compile_str_collecting_warnings` use the private `resolve_base_dir(base_dir: Option<&Path>) -> Result<PathBuf, MdsError>` helper to convert an optional base directory to a concrete `PathBuf`, falling back to `std::env::current_dir()` when `None`. Any new public string-based API function that accepts an optional `base_dir` should call this helper rather than duplicating the fallback logic.

### Error Reporting Pattern

All errors are `MdsError` variants (thiserror + miette). `CircularImport`, `Recursion`, and `TypeError` variants carry `help(...)` diagnostic attributes, so `miette` renders actionable hints alongside the error message automatically.

For errors with source location, use the `pub(crate)` `_at` constructor variants:

```rust
// Use _at variants to attach a miette SourceSpan — provides file + line in error output.
// All constructors are pub(crate) — only accessible within the crate.
MdsError::syntax_at(message, file, source, offset, len)
MdsError::undefined_var_at(name, file, source, offset, len)
MdsError::undefined_fn_at(name, file, source, offset, len)
MdsError::name_collision_at(name, file, source, offset, len)
MdsError::file_not_found_at(path, file, source, offset, len)
MdsError::arity_at(name, expected, got, file, source, offset, len)
MdsError::type_error_at(got, file, source, offset, len)
MdsError::recursion_at(name, file, source, offset, len)
MdsError::import_error_at(message, file, source, offset, len)
MdsError::export_error_at(message, file, source, offset, len)
MdsError::circular_import_at(cycle, file, source, offset, len)

// Use bare variants only when source context is unavailable.
MdsError::file_not_found(path)  // etc.
```

Always prefer `_at` variants inside the validator and evaluator where source offsets are available from the AST nodes.

### CLI Exit Codes

The `exit_code(err: &miette::Error) -> i32` function in `src/main.rs` maps errors to structured exit codes:

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Logical/syntax error (undefined variable, arity mismatch, recursion, etc.) |
| 2 | I/O or filesystem error (`MdsError::Io`, `FileNotFound`, `NotMdsFile`) |
| 3 | Resource limit exceeded (`MdsError::ResourceLimit`) |

`exit_code` downcasts via `err.downcast_ref::<MdsError>()`. Errors created with `miette::miette!()` in `main.rs` do NOT downcast to `MdsError` and fall through to exit code 1. Both `Build` and `Check` commands call `process::exit(exit_code(&e))` on failure. When adding new error categories, update `exit_code` first.

### Adding a New Value Type

Currently blocked by design: `Value::from_yaml` and `Value::from_json` (both `pub(crate)`) both return `Err` for object/map types. Any new value variant must be added to both converters, to `Value::Display`, `Value::is_truthy`, `Value::type_name`, and `Value::as_array` (if relevant). Because `Value` is `#[non_exhaustive]`, external code cannot match exhaustively, but all internal match arms must be updated. When writing tests for numeric values, avoid `3.14` (clippy `approx_constant` lint) — use values like `2.5` instead.

### CLI Auto-Detection

The `auto_detect_mds_file()` function in `src/main.rs` scans the current working directory for `.mds` files. It returns:
- `Ok(path)` — exactly one `.mds` file found
- `Err(...)` with hint "run 'mds init'" — zero files found
- `Err(...)` with hint to specify a file — multiple files found (names sorted alphabetically, hint uses `names.first()` — not index access)

Both `Build` and `Check` commands use `input: Option<PathBuf>`. When `None`, they call `auto_detect_mds_file()`. `Build` additionally prints `"Building {path}"` to stderr (unless `--quiet`) when auto-detecting, so users know which file was selected.

### CLI Output Destination (`mds build`)

`mds build` writes to a file by default — it no longer prints to stdout unless explicitly requested. The output path is determined by `resolve_output_path()` using a 6-step precedence chain:

| Priority | Condition | Result |
|---|---|---|
| 1 | `-o -` | stdout (`None`) |
| 2 | `-o <path>` | that exact path |
| 3 | stdin (`-`) with no `-o` or `--out-dir` | stdout (`None`) |
| 4 | `--out-dir <dir>` | `<dir>/<stem>.md` (directory created if needed) |
| 5 | `mds.json` with `build.output_dir` | `<config_dir>/<output_dir>/<stem>.md` (dir created) |
| 6 | Default | `<source_dir>/<stem>.md` next to the source file |

`-o` and `--out-dir` are mutually exclusive (enforced by clap `conflicts_with`). The `Build` struct holds `output: Option<String>` (not `Option<PathBuf>`) so the literal string `"-"` can be detected before path resolution.

**`derive_output_filename(input: &Path) -> OsString`** extracts the file stem and appends `.md`. `foo.mds` → `foo.md`, `foo.bar.mds` → `foo.bar.md`, `README` (no extension) → `README.md`, any other extension → stem + `.md`. For stdin (`-` input), the stem would be `-` so `resolve_output_path` filters it out and uses `"output.md"` as the fallback name.

When `output_path` is `Some(path)`, the build handler creates any missing parent directories with `fs::create_dir_all` and then calls `fs::write`. On success, it prints `"Compiled to {path}"` to stderr (unless `--quiet`). When `output_path` is `None`, it calls `print!("{compiled}")` to stdout without a trailing newline.

### Project Config (`mds.json`)

`load_config(start: &Path) -> Result<Option<(MdsConfig, PathBuf)>, miette::Error>` walks up the directory tree from the input file's parent, looking for `mds.json`. The walk is bounded by `MAX_TRAVERSAL_DEPTH = 256`. On finding the file:

1. Reads bytes first via `fs::read` (TOCTOU-safe — no separate metadata call)
2. Checks `bytes.len() as u64 > MAX_CONFIG_SIZE` (1 MB limit)
3. Deserializes with `serde_json`
4. Returns both the config and the **directory that contains `mds.json`** — the config directory is stored alongside the config because `output_dir` values are resolved relative to that directory, not the input file's directory

The config structures:

```rust
// Both structs derive Default so missing keys are silently ignored.
#[derive(Debug, Default, Deserialize)]
struct MdsConfig {
    #[serde(default)]
    build: BuildConfig,
}

#[derive(Debug, Default, Deserialize)]
struct BuildConfig {
    output_dir: Option<String>,  // relative path from mds.json's directory
}
```

Key behaviors:
- `{}` (empty JSON object) is valid — all fields are `Option` or have `#[serde(default)]`; falls back to default output (file next to source)
- Invalid JSON produces a hard error immediately before compilation
- Discovery walks upward, so `mds.json` in a parent directory applies to all files in subdirectories
- `output_dir` is resolved relative to the `mds.json` location, not the source file location
- CLI flags (`-o`, `--out-dir`) always override `mds.json` (steps 1-4 in the precedence chain take priority)
- `output_dir` containing `..` components is rejected by `resolve_output_path` to prevent writing files outside the project tree

## Constraints

- **Import paths must be relative** — `validate_import_path` rejects non-relative paths (must start with `./` or `../`) and null bytes. Runs before any filesystem access.
- **Symlinks rejected** — `check_symlink` detects symlinks in the final path component by comparing `canonical_parent.join(raw_filename)` vs the fully-resolved path. If they differ, returns `ImportError` before reading the file.
- **Path traversal prevention** — resolved import paths must remain within the project root (detected via `.git`/`.mdsroot` walk-up from entry file directory).
- **MAX_IMPORT_DEPTH = 64** — prevents stack overflow from deep chains (separate from circular import detection); tracked via `IndexSet::len()` on the single `resolving` field.
- **MAX_FILE_SIZE = 10MB** — checked by reading bytes first, then comparing size (TOCTOU-safe); prevents memory exhaustion from large inputs.
- **MAX_CALL_DEPTH = 128** — prevents stack overflow from deeply nested function calls; tracked via `ctx.call_stack.len()`.
- **MAX_NESTING_DEPTH = 256** — `pub(crate)` constant in `src/parser.rs`; shared between: (1) parser-level block nesting (`@if`/`@for`/`@define`) via `enter_block()`, and (2) argument-level nested call depth validation in `validate_var_args`.
- **MAX_LOOP_ITERATIONS = 100,000** — per-loop hard cap in the evaluator.
- **MAX_TOTAL_ITERATIONS = 1,000,000** — cumulative cap across all loops in one compilation; tracked via `ctx.total_iterations`.
- **MAX_OUTPUT_SIZE = 50 MB** — evaluator checks output buffer size after each node.
- **MAX_VALUE_DEPTH = 64** — `Value::from_yaml` / `Value::from_json` reject YAML sequences or JSON arrays nested deeper than 64 levels.
- **Object types unsupported** — YAML mappings and JSON objects are rejected at the value conversion layer.
- **`.md` files require `type: mds`** in frontmatter to be compiled — `validate_file_type` enforces this.
- **Recursion is detected at evaluation time** using `ctx.call_stack` — the validator cannot catch recursive call chains because they depend on runtime scope.
- **Nested call result is always a String** — `Arg::Call` evaluation wraps the inner function's output in `Value::String`. Functions that return non-string values (e.g., future numeric functions) will still produce a string when used as a nested argument.
- **MAX_TRAVERSAL_DEPTH = 256** — single definition in `src/resolver.rs`, re-exported as `pub const` via `src/lib.rs`; caps upward directory walks in both `load_config` (main.rs) and `find_project_root` (resolver.rs); prevents unbounded traversal on unusual filesystems.
- **MAX_CONFIG_SIZE = 1MB** — `mds.json` files larger than 1MB are rejected by `load_config` before parsing; TOCTOU-safe (read first, then check size).
- **Directory input rejected** — `reject_directory_input()` in main.rs returns an error immediately if the input path is a directory (not a file or stdin); prevents confusing errors deeper in the pipeline.
- **`mds init` filename traversal rejected** — `run_init` rejects filenames containing `..` components before writing.

## Anti-Patterns

- **Calling `eprintln!` from evaluator or resolver code** — all non-fatal diagnostics must go through `ctx.warnings` (in the evaluator) or `warnings: &mut Vec<String>` (in the resolver). Direct stderr output bypasses the quiet flag and makes the warnings un-testable.
- **Calling `evaluate` before `validate`** — the evaluator trusts that all references exist; skipping validation will produce misleading errors at evaluation rather than rich span-aware diagnostics.
- **Resolving imports in the evaluator** — imports must be resolved before evaluation so scope is complete when `validate` runs. Adding import-like behavior in the evaluator breaks this order.
- **Creating `ModuleCache` per-module instead of per-compile** — the cache is the only thing preventing re-parsing the same file dozens of times. Each `compile()` / `compile_str_with()` call creates exactly one cache.
- **Using bare `MdsError::syntax(msg)` when source context is available** — always prefer `syntax_at` when you have an offset and source string. All constructors are `pub(crate)` — they can only be called within the crate.
- **Adding object/map support without updating all Value methods** — `from_yaml`, `from_json` (both `pub(crate)`), `Display`, `is_truthy`, `type_name`, and `as_array` must all be consistent.
- **Forgetting to capture closure scope in new definition-like directives** — any directive that defines a callable entity should call `scope.get_all_namespaces()`, `scope.get_all_functions()`, and `scope.get_all_vars()` at definition time so the callable works correctly when invoked from other modules. Remember that `FunctionDef::from` always produces `captured: CapturedScope::default()` — the resolver must fill the captures.
- **Adding functions to scope without also capturing current scope into the FunctionDef** — if you add a function to scope after other functions are already captured, the previously captured siblings won't see the new function.
- **Adding a new `Arg` variant without updating all three match sites** — parser (`parse_single_arg_inner`), evaluator (`resolve_args`), and validator (`validate_var_args`) all pattern-match exhaustively on `Arg`. Adding a variant without updating all three will produce a compile error, which is by design.
- **Passing separate `call_stack`/`warnings` instead of `ctx` to evaluator helpers** — all evaluator internal functions now take `ctx: &mut EvalContext`. Refactoring a helper to accept separate params breaks the invariant that all three fields move together.
- **Using `compile` instead of `compile_collecting_warnings` in CLI code** — the CLI must use the collecting variants to properly gate warning output on the `--quiet` flag. The same applies to validation: use `check_collecting_warnings` rather than `check` when the caller needs to control warning output.
- **Duplicating `base_dir` fallback logic in new string-based API functions** — always call `resolve_base_dir(base_dir)` rather than inlining the `current_dir()` fallback.
- **Calling `get_all_exports()` and expecting a `HashMap`** — `ResolvedModule::get_all_exports()` returns `Vec<(String, Arc<FunctionDef>)>`, not a `HashMap`. Callers that need map-like access must collect explicitly.
- **Injecting `Value::Null` as a placeholder for `@define` params in validation** — the validator uses `Value::Array(vec![])` so that `@for item in param:` inside a define body passes the array type check. Using `Null` would produce a spurious type error at validate time.
- **Ignoring the `Result` from `scope.pop()`** — `pop()` returns `Result<(), MdsError>` and errors when called on the global scope frame. Always use `scope.pop()?`. Exception: immediately after `scope.push()` in validator body recursion, where `let _ = scope.pop()` is safe because we just pushed.
- **Re-exporting via `to_namespace()` without export-visibility check** — `to_namespace()` was previously a bug: it included `prompt_body` unconditionally, ignoring whether `"prompt"` was in `explicit_exports`. The method now applies the same visibility rule as `get_prompt_value()`. Never bypass this method to build a `NamespaceScope` directly from `ResolvedModule` fields.
- **Accessing `func.captured_namespaces` / `func.captured_functions` / `func.captured_vars` directly** — these three separate fields no longer exist. Access via `func.captured.namespaces`, `func.captured.functions`, `func.captured.vars`.
- **Using `output: Option<PathBuf>` for the Build command's output field** — the field is `Option<String>` intentionally so the literal `"-"` can be compared before any path resolution occurs. Converting to `PathBuf` first would lose the sentinel value.
- **Resolving `mds.json`'s `output_dir` relative to the source file** — `output_dir` must be resolved relative to the directory that contains `mds.json` (the `config_dir` returned by `load_config`), not relative to the input `.mds` file's directory. These differ when `mds.json` is in a parent directory.
- **Using `metadata().len()` before `read()` for size checks** — both `load_config` and `read_validated_file` read bytes first, then check the length. Never split size check from read — that introduces a TOCTOU race.
- **Matching exhaustively on `MdsError` or `Value` in external code** — both enums are `#[non_exhaustive]`. External crates must include a wildcard arm when pattern-matching.
- **Adding run logic directly in `run()` instead of a dedicated `run_*` function** — `run()` is a pure dispatcher. New CLI commands should have a dedicated `run_*` function; `run()` just matches the command enum and calls it.

## Gotchas

- **`{namespace.varname}` dot-notation variable access is rejected** — `{ns.name}` without a call (`()`) is not a valid expression. The parser calls the private `dot_notation_error` function which produces a targeted `Syntax` error explaining the difference between dot-notation access (unsupported) and qualified function calls (`{ns.fn(arg)}`). Only `Expr::QualifiedCall` (which requires arguments) is valid for namespace access; `{ns.name}` as bare variable lookup has no support.
- **`@define` body nodes have leading/trailing newlines stripped** — the parser calls `strip_leading_newline` and `strip_trailing_newline` on `@define` bodies. If you add a new block directive, apply the same stripping unless you want those newlines in output.
- **`@for` body validation uses a Null-injected push; `@define` body validation uses an Array-injected push** — the validator uses `Value::Null` for the loop variable (type is unknown at define time) but `Value::Array(vec![])` for `@define` parameters. This asymmetry exists because `@define` params might be used as iterables (`@for item in param:`), which requires the placeholder to pass the array type check.
- **Runtime vars override frontmatter silently** — there is no warning when a runtime var shadows a frontmatter key. Intentional but can cause confusion when debugging.
- **`@export` changes all-implicit to explicit** — once any `@export` appears in a module, only explicitly listed names are exported. Adding an `@export name` to a previously-implicit-all module will break importers depending on other functions.
- **`@export prompt` is valid** — the string `"prompt"` is a special case in export validation. It does not need a corresponding `@define prompt` — it refers to the module's rendered body.
- **`to_namespace()` respects export visibility for `prompt_body`** — a bug fix: `to_namespace()` now only includes `prompt_body` in the `NamespaceScope` when `"prompt"` is an available export. Alias-imported modules with explicit exports that exclude `"prompt"` will no longer expose the body text via `@include alias`.
- **`@include` on an empty module pushes a warning and returns empty** — `evaluate_include` calls `ctx.warnings.push(...)` (not `eprintln!`) and returns `""`. Warnings are silently dropped once `ctx.warnings.len() >= MAX_WARNINGS (1,000)`.
- **Merged imports bring in `prompt` body but not frontmatter vars** — `@import "path"` (merge) brings functions and the `prompt` body text into scope, but NOT the imported module's frontmatter variables.
- **Selective import of `prompt` binds as a variable, not a function** — `@import { prompt } from "path"` sets `prompt` as a `Value::String` via `scope.set_var`, not `scope.set_function`.
- **`compile_str` takes no arguments** — the zero-argument form `compile_str(source)` is a convenience wrapper. Use `compile_str_with(source, base_dir, runtime_vars)` when you need import resolution relative to a specific directory or runtime variable overrides.
- **`compile_file` takes no runtime vars** — `compile_file(path)` calls `compile(Path::new(path), None)`. If runtime vars are needed, call `compile` directly.
- **Closure capture is eager and shallow** — `get_all_vars()` / `get_all_functions()` / `get_all_namespaces()` snapshot the scope at definition time. Functions defined after the closure capture are not visible to the captured function.
- **`get_all_functions()` returns `Arc<FunctionDef>`; captured.functions stores owned `FunctionDef`** — the resolver converts `Arc<FunctionDef>` → owned `FunctionDef` when populating captures (via `(*v).clone()`). `invoke_function` then wraps each captured function back in `Arc::new(f.clone())` when restoring closure scope. This round-trip is intentional to break reference cycles.
- **`call_stack` is `Vec`, not `HashSet`** — recursion detection in the evaluator uses `ctx.call_stack.iter().any(|s| s == call_key)` (O(n) scan). At `MAX_CALL_DEPTH = 128`, this is negligible. The Vec is also the stack that `invoke_function` pushes to and pops from; an `assert!` (not `debug_assert!`) verifies the LIFO invariant after each return — this runs in release mode because a mismatched pop would silently corrupt recursion state and allow stack overflows.
- **`IndexSet` replaces two resolver fields** — if you need to check "is this path currently being resolved?", use `self.resolving.contains(&canonical)`. If you need to reconstruct the cycle path, use `self.resolving.iter()` to get an ordered sequence. There is no separate `resolving_stack` field.
- **`compile_str` / `resolve_source` uses a virtual path `<source>`** — in-memory sources cannot be canonicalized. Repeated calls to `compile_str` re-parse every time; there is no caching for in-memory sources.
- **Project root is set on first resolve** — `root_dir` is set lazily. If `resolve_source` is called first, `root_dir` is set to the _canonicalized_ `base_dir` — if the directory cannot be resolved, `resolve_source` returns `Err` immediately.
- **Re-export errors are raised at the barrel module, not the consumer** — when `@export name from "path"` fails because `name` is not exported from the source module, the error surfaces when the barrel itself is compiled, not when the consumer imports from the barrel.
- **`--set KEY=VAL` last-write wins for duplicate keys** — when `--set name=First --set name=Second` is passed, the second value wins because runtime vars are collected into a `HashMap` and later writes overwrite earlier ones.
- **`MAX_NESTING_DEPTH` is `pub(crate)`, not `pub`** — it was elevated to `pub(crate)` so `src/validator.rs` can import it for its argument-depth guard. It was not made fully `pub` because it is an implementation detail.
- **`TextNode` has no offset** — raw text nodes (`Node::Text(TextNode)`) do not carry a byte offset. Only structured nodes have offsets for error reporting.
- **`enter_block()` must be paired with `self.depth -= 1`** — the helper only increments; callers are responsible for decrementing after the block body is parsed.
- **Selective import `from` keyword requires a whitespace separator** — `parse_import_directive` accepts `from ` (space) or `from\t` (tab) but rejects `from"path"` with no gap.
- **String literal escapes are not full Rust/JSON escapes** — `unescape_string` in the parser only recognizes `\\`, `\"`, and `\'`. A backslash followed by any other character (e.g., `\n`, `\t`) is kept verbatim as both backslash and the following character.
- **`MdsError` itself is `#[must_use]` at the type level** — in addition to `#[must_use]` on individual constructor methods, the `MdsError` enum has `#[must_use]` applied to the type declaration. This means constructing a `MdsError` value without returning or using it produces a compiler warning. This guards against accidentally constructing an error in a branch and then silently discarding it.
- **`help(...)` attributes are variant-level, not constructor-level** — `CircularImport`, `Recursion`, and `TypeError` have `#[diagnostic(help(...))]` annotations that miette renders automatically. When adding new error variants, add the `help` attribute directly on the variant, not in the constructor method.
- **Exit code 2 covers three error types** — `MdsError::Io`, `MdsError::FileNotFound`, and `MdsError::NotMdsFile` all map to exit code 2. Other `MdsError` variants map to exit code 1. Non-`MdsError` miette errors (from `miette::miette!()`) also map to exit code 1.
- **`mds build` default is file output, not stdout** — before this change the build command always wrote to stdout. Now it writes `<stem>.md` next to the source by default. Existing scripts that pipe `mds build foo.mds` and expect stdout output must be updated to add `-o -`.
- **stdin input with `--out-dir` writes `output.md`, not `-.md`** — when input is stdin (`-`) and `--out-dir` is set, the output filename is hardcoded to `"output.md"` because deriving a name from the stdin path `-` would produce `-.md`. This is handled in `resolve_output_path` by filtering out the `-` sentinel before calling `derive_output_filename`.
- **`load_config` starts from the input file's parent, not CWD** — the walk begins at the directory containing the input file. If the input file is in a subdirectory, a `mds.json` in that subdirectory is found first; one in a parent directory is also found via the walk. The resulting `output_dir` is always resolved relative to the `mds.json` location.

## Key Files

- `src/lib.rs` — public API: `compile`, `compile_file`, `compile_str`, `compile_str_with`, `compile_collecting_warnings`, `compile_str_collecting_warnings`, `check`, `check_str`, `check_str_with`, `check_collecting_warnings`, `check_str_collecting_warnings`, `load_vars_file`, `clean_output`; re-exports `MAX_FILE_SIZE` and `MAX_TRAVERSAL_DEPTH` (both sourced from `resolver.rs`); private `resolve_base_dir` helper
- `src/main.rs` — CLI entry point: `MdsConfig`/`BuildConfig` structs, `load_config` (TOCTOU-safe, bounded by `MAX_TRAVERSAL_DEPTH`), `resolve_output_path` (6-step precedence), `derive_output_filename`, `auto_detect_mds_file`, `parse_cli_value`, `build_runtime_vars`, `reject_directory_input`, `read_stdin`, `exit_code`; logic split into `run_build`/`run_check`/`run_init`
- `src/ast.rs` — all AST types including `Arg::Call` for nested function call arguments; the contract between parser and everything downstream
- `src/lexer.rs` — `Lexer<'a>` struct with `scan_*` methods; public API is `tokenize(source, file)` only
- `src/parser.rs` — converts token stream to `Module` AST; `pub(crate) MAX_NESTING_DEPTH`; `enter_block()` helper, `parse_args_inner`/`parse_single_arg_inner` depth-bounded recursion, identifier validation, duplicate param detection
- `src/resolver.rs` — orchestrator: `ModuleCache` with `Arc<ResolvedModule>` cache, `IndexSet<PathBuf>` for cycle detection; security checks split into `check_symlink`/`check_import_depth`/`check_path_traversal` (extracted helpers); import dispatch split into `resolve_alias_import`/`resolve_merge_import`/`resolve_selective_import`; `MAX_TRAVERSAL_DEPTH` named constant; `to_namespace()` respects export visibility for `prompt_body`
- `src/evaluator.rs` — AST walker; `EvalContext<'a>` bundles `call_stack: Vec<String>`, `total_iterations`, `warnings`; `resolve_args` takes `ctx: &mut EvalContext`; `evaluate_include` pushes to `ctx.warnings`
- `src/validator.rs` — pre-evaluation semantic checks; `validate()` takes `&mut Scope`, uses `push()`/`pop()` instead of cloning; `validate_var_args` recursively validates nested `Arg::Call` arguments; uses `crate::parser::MAX_NESTING_DEPTH` for depth guard
- `src/scope.rs` — `CapturedScope` struct bundling three closure capture maps; `FunctionDef.captured: CapturedScope`; `Frame::functions` and `NamespaceScope::functions` store `Arc<FunctionDef>`; `set_function` takes `Arc<FunctionDef>`; `get_function` returns `Option<&Arc<FunctionDef>>`
- `src/value.rs` — runtime value enum (`#[non_exhaustive]`); `from_yaml`/`from_json` are `pub(crate)`; YAML/JSON converters and display rules
- `src/error.rs` — `MdsError` enum (`#[non_exhaustive]`); all constructor methods are `pub(crate)`; all major variants have `_at` constructors; `ResourceLimit` variant for evaluator/value depth guards
- `tests/integration.rs` — end-to-end tests covering all features, error paths, CLI integration, and spec-compliance tests; includes resource limit tests (call depth, output size, nesting depth, warning cap), directory rejection, path traversal rejection, and `mds.json` config tests

## Related

- `src/resolver.rs` — canonical reference for the module system, import semantics, security guards, `Arc<ResolvedModule>` cache, `IndexSet` cycle detection, and `ResolvedModule` export API
- `src/evaluator.rs` — canonical reference for `EvalContext` usage, directive execution order, closure restore, call-depth guards, nested arg evaluation, and warning collection
- `src/scope.rs` — canonical reference for `CapturedScope` struct, `Arc<FunctionDef>` in frames, closure capture API (`get_all_*` methods), and shadowing semantics
- `src/ast.rs` — canonical reference for `Arg` variants; any new argument form starts here
- `src/lib.rs` — canonical reference for the two-tier warning API (`compile` vs `compile_collecting_warnings`, `check` vs `check_collecting_warnings`), `compile_file` convenience entry point, and `resolve_base_dir` helper
- `src/main.rs` — canonical reference for CLI auto-detection logic, `parse_cli_value` coercion rules, `exit_code` categorization, output destination resolution (`resolve_output_path`), project config loading (`load_config`), and run_build/run_check/run_init decomposition
- `src/error.rs` — canonical reference for `#[non_exhaustive]` on `MdsError`, `pub(crate)` constructor pattern, `help(...)` diagnostic attribute placement, and available `_at` constructors
- `src/value.rs` — canonical reference for `#[non_exhaustive]` on `Value`, `pub(crate)` converters, and the JSON/YAML parsing boundary
- `tests/integration.rs` — covers all directive combinations including nested function calls, CLI stdin/quiet mode, auto-detect, `compile_file`, error help-text, scope/export visibility rules, `--set` coercion, re-export error scenarios, default file output, `--out-dir`, `mds.json` config behavior, and all resource limit scenarios
