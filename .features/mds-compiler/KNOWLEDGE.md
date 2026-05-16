---
feature: mds-compiler
name: MDS Compiler
description: "Use when working on the MDS compilation pipeline, adding directives, modifying scope/variable handling, extending the module system, debugging output rendering, or modifying CLI output behavior (file output, project config). Keywords: lexer, parser, evaluator, resolver, validator, scope, frontmatter, interpolation, directive, import, export, include, define, for, if, closure, lexical scope, prompt export, nested function calls, arg parsing, warnings, quiet mode, stdin, auto-detect, compile_file, reexport, EvalContext, CapturedScope, IndexSet, Arc, exit_code, mds.json, output_dir, out_dir, default output, file output, MdsConfig, BuildConfig, load_config, resolve_output_path, derive_output_filename, non_exhaustive, pub(crate), run_build, run_check, run_init, MAX_TRAVERSAL_DEPTH, MAX_NESTING_DEPTH, object, map, Value::Object, dot notation, member access, MemberAccess, key-value iteration, resolve_dot_path, dot path, config.field, raw_frontmatter, strip_type_mds, prepend_frontmatter, frontmatter preservation."
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
updated: 2026-05-16
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

`TextNode` is a struct (`{ text: String }`) with no offset field — offsets are not tracked for raw text. `EscapedBrace` is a unit variant with no fields.

**`Expr` enum** has four variants representing the forms inside `{...}`:

| Variant | Syntax | Notes |
|---|---|---|
| `Expr::Var(String)` | `{name}` | Simple variable lookup |
| `Expr::Call { name, args }` | `{greet("x")}` | Local function call |
| `Expr::QualifiedCall { namespace, name, args }` | `{ns.greet("x")}` | Namespace-prefixed call (requires args) |
| `Expr::MemberAccess { object, fields }` | `{config.key}` or `{a.b.c}` | Object field access; no call syntax |

`Expr::MemberAccess` is produced when a dot appears before any `(` and there are no parentheses. `Expr::QualifiedCall` is produced when a dot appears before `(` with arguments following. Direct object interpolation (`{obj}` where obj is `Value::Object`) is a runtime error — users must access a specific field.

**`Arg` enum** has four variants — this is the complete set:

| Variant | Meaning |
|---|---|
| `Arg::StringLiteral(String)` | Quoted string literal, e.g. `"hello"` |
| `Arg::Var(String)` | Variable reference, e.g. `name` |
| `Arg::Call { name, args: Vec<Arg> }` | Nested function call, e.g. `inner("arg")` |
| `Arg::MemberAccess { object, fields: Vec<String> }` | Object field access as argument, e.g. `greet(config.name)` |

`Arg::Call` enables arbitrary nesting: `{outer(inner("arg"))}` parses as `Expr::Call { args: [Arg::Call { ... }] }`. Depth is bounded by `MAX_NESTING_DEPTH = 256` in the parser.

`Arg::MemberAccess` is produced when a function argument contains dots without parentheses: `{greet(config.name)}` parses as `Expr::Call { args: [Arg::MemberAccess { object: "config", fields: ["name"] }] }`. Field existence is validated at runtime; the validator only checks that the root object variable exists in scope.

**`IfBlock.condition`** is `Vec<String>` — a dot-separated path. `@if flag:` → `vec!["flag"]`; `@if config.debug:` → `vec!["config", "debug"]`. The evaluator resolves the full path via `resolve_dot_path`.

**`ForBlock.iterable`** is also `Vec<String>` — a dot-separated path. **`ForBlock.key_var: Option<String>`** is set for key-value iteration (`@for key, value in obj:`). When `key_var` is `Some`, the evaluator iterates over a `Value::Object` rather than an array.

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

The `Value` enum has six variants: `String`, `Number(f64)`, `Boolean`, `Array(Vec<Value>)`, `Object(HashMap<String, Value>)`, `Null`. Truthiness rules match JavaScript-like semantics: `0`, `""`, `[]`, `{}`, `null`, `false`, and `NaN` are falsy; everything else is truthy.

**`Value::Object`**: YAML mappings and JSON objects are converted to `Value::Object(HashMap<String, Value>)` by `from_yaml` and `from_json`. Key behaviors:
- Empty objects are falsy; non-empty objects are truthy
- Non-string YAML keys are **rejected with `MdsError::yaml_error`** — a diagnostic directs users to use a quoted string key. This replaces the previous silent-skip behavior; callers now get a clear error rather than confusing "field not found" failures downstream
- `Display` renders as alphabetically-sorted `key: val1, key2: val2` pairs
- Objects cannot be directly interpolated — `{obj}` where obj is an `Object` produces a runtime `TypeError` directing users to access a specific field via `{obj.field}`
- `type_name()` returns `"object"`
- `From<HashMap<String, Value>>` is implemented for use in test setup and API code

**`#[non_exhaustive]`**: The `Value` enum is marked `#[non_exhaustive]`. This means external crates cannot exhaustively match on it without a wildcard arm. Within the crate all `match` arms remain exhaustive; you do not need `_` inside the library.

**`pub(crate)` converters**: Both `Value::from_yaml` and `Value::from_json` are `pub(crate)` — they are intentionally not part of the public API. External consumers receive `Value` via frontmatter parsing and runtime var injection, not by constructing it from raw YAML/JSON.

`Value::Display` renders numbers as integers when the fractional part is zero, guarding against i64 overflow for very large floats. Arrays display as comma-separated values. Objects display as sorted `key: val` pairs. `Null` displays as empty string.

Both converters enforce `MAX_VALUE_DEPTH = 64` to reject YAML/JSON nested deeper than 64 levels (applies to both sequences/arrays and mappings/objects).

The `Value` enum implements `From` for common Rust types: `&str`, `String`, `f64`, `i64`, `i32`, `bool`, `Vec<T: Into<Value>>`, and `HashMap<String, Value>`. Use these conversions in test setup and programmatic API code rather than constructing enum variants directly.

### Parser (`src/parser.rs`)

The parser converts a token stream to a `Module` AST. Key hardening:

- `pub(crate) const MAX_NESTING_DEPTH: usize = 256` — `pub(crate)` (not private) so `src/validator.rs` can import it for `validate_var_args`'s depth guard; enforced via a `depth` counter on the parser struct; shared between two independent limits: (1) `@if`/`@for`/`@define` block nesting via `enter_block()`, and (2) nested function call argument depth via `parse_args_inner`
- `enter_block()` — extracted helper that increments `self.depth` and returns `Err` if the limit is exceeded; called at the start of `parse_if_block`, `parse_for_block`, and `parse_define_block`, with matching `self.depth -= 1` on exit
- `is_valid_identifier(s)` — all directive names (function names, loop vars, aliases, export names) are validated: must start with ASCII letter or `_`, contain only ASCII alphanumeric or `_`
- Duplicate `@define` parameter names are rejected at parse time
- `@else` without colon gives a targeted error message ("use '@else:' with trailing colon")

**Dot-path parsing**: `@if` conditions and `@for` iterables are both parsed as `Vec<String>` by splitting on `.`. Each segment is validated with `is_valid_identifier`. Parser invariant: the resulting `Vec` is always non-empty (the validator uses `.first().ok_or_else(...)` on these paths rather than index access, returning `MdsError::syntax` in release builds instead of panicking). `@if config.debug:` → `condition: vec!["config", "debug"]`. `@for item in data.list:` → `iterable: vec!["data", "list"]`.

**Key-value `@for` parsing**: `@for key, value in obj:` is parsed by looking for a comma in the `var_part` (before `in`). Both the key and value identifiers are validated with `is_valid_identifier`. The parser produces `ForBlock { key_var: Some("key"), var: "value", iterable: vec!["obj"], ... }`.

**Interpolation disambiguation** — the parser inspects raw content to choose among `Expr::Var`, `Expr::Call`, `Expr::QualifiedCall`, and `Expr::MemberAccess`:
1. Dot before any `(` → either `QualifiedCall` or `MemberAccess`
2. Dot before `(` with args → `QualifiedCall`
3. Dot with no `(` anywhere → `MemberAccess`; all dot-path segments validated with `is_valid_identifier`
4. No dot before `(` → `Var` (no parens) or `Call` (with parens)

**Argument parsing internals**: `parse_args` and `parse_single_arg` are thin public wrappers that delegate to `parse_args_inner(s, depth)` and `parse_single_arg_inner(s, depth)`. The `_inner` variants carry the recursion depth. When `parse_single_arg_inner` encounters `name(...)` syntax, it produces `Arg::Call`. When it encounters `name.field` syntax (a dot with no following `(`), it produces `Arg::MemberAccess`.

`parse_args_inner` tracks open parentheses (`paren_depth`) so that commas inside nested calls are not treated as argument separators at the outer level. Quote-escaped commas inside string arguments are similarly skipped.

Note: `parse_single_arg` (without `_inner` suffix) exists only under `#[cfg(test)]` as a test shim.

### Validator (`src/validator.rs`)

Validates the AST against the current scope **before** evaluation. Catches: undefined variables in `{interpolation}` and `@if` conditions, undefined iterables in `@for`, undefined namespaces in `@include`, undefined functions and arity mismatches in calls, and undefined variable arguments to functions.

**`validate()` signature**: `pub fn validate(nodes: &[Node], scope: &mut Scope, file: &str, source: &str) -> Result<(), MdsError>`. The scope parameter is `&mut Scope` — the validator uses `scope.push()` / `scope.pop()` directly for `@for` and `@define` body recursion instead of cloning.

**`@for` body validation with key-value support**: The validator checks the iterable's root variable exists. Static type enforcement is conditional: if `key_var` is `None` AND the iterable is a simple identifier (single element, no dot path), the validator enforces the iterable is `Value::Array`. When iterating an object with a single var, it rejects with a hint to use `@for key, value in obj:` syntax. When `key_var` is `Some`, the validator injects both `key_var` and `var` as `Value::Null` in the pushed scope. Dot-path iterables skip static type checks because field types cannot be resolved statically.

**`@define` body validation**: The validator calls `scope.push()`, injects all params as `Value::Array(vec![])`, recurses via `validate()`, then calls `scope.pop()`. Using an empty array — rather than `Null` — allows `@for item in param:` inside the define body to pass the array type check at validation time.

**`validate_var_args`** covers all four `Arg` variants:
- `Arg::StringLiteral` — no validation needed
- `Arg::Var` — variable existence check against scope
- `Arg::Call { name, args }` — function existence check, arity check against `func.params.len()`, then recursion into `inner_args`
- `Arg::MemberAccess { object, .. }` — root variable existence check against scope; field resolution deferred to runtime

`validate_var_args` accepts a `depth: usize` parameter that limits recursive validation depth. The parser already enforces `MAX_NESTING_DEPTH = 256` on arg nesting so this acts as a safety belt.

The `arity_at` constructor provides source-span-aware arity errors from the validator.

### Resolver (`src/resolver.rs`)

The resolver is the orchestrator. `ModuleCache` drives the full pipeline for each file and caches `Arc<ResolvedModule>` by canonical path, preventing repeated work and providing cycle detection.

**Project root detection**: `find_project_root` walks up from the entry file's directory looking for `.git` or `.mdsroot` markers. The found root is stored in `ModuleCache::root_dir` on first resolve. All subsequently resolved paths must `starts_with(root_dir)` — this is the path traversal boundary. The walk is bounded by `MAX_TRAVERSAL_DEPTH = 256`.

**Security guards** — split across focused private methods:

`canonicalize_and_check` (always):
1. `check_symlink(path)` — detects symlinks by comparing `canonical_parent.join(filename)` vs `full_canonicalize()`; if they differ, returns `ImportError`
2. `root_dir` initialization — set on first resolve by calling `find_project_root` from the entry file's directory
3. `check_import_depth(&self)` — rejects chains deeper than `MAX_IMPORT_DEPTH = 64`; checked via `resolving.len()`
4. `check_path_traversal(&self, canonical)` — resolved canonical path must `starts_with(root_dir)`

`read_validated_file` (cache misses only):
5. Reads bytes first, then checks size against `MAX_FILE_SIZE = 10MB` — reading first avoids a TOCTOU race

**Import helpers** — each `ImportDirective` variant dispatches to a dedicated private method:
- `resolve_alias_import` — calls `validate_import_path`, resolves, calls `scope.set_namespace`
- `resolve_merge_import` — brings all exports + `prompt` body into scope; frontmatter vars not imported
- `resolve_selective_import` — imports only named exports; `prompt` binds as a variable via `scope.set_var`

**Cycle detection** uses `IndexSet<PathBuf>` (`indexmap` crate) — provides O(1) membership test plus insertion-ordered iteration. `shift_remove` preserves insertion order when unmarking.

**`process_module` decomposition**: split into focused helpers:
- `build_scope_from_frontmatter(frontmatter, is_md, runtime_vars)` — parses YAML, populates scope, applies runtime var overrides; skips `type` key for `.md` files
- `collect_definitions_and_imports(body, scope, ctx, warnings)` — walks AST dispatching to `collect_define`, `collect_export`, `resolve_import`; returns `CollectedDefs`
- `validate_exports(explicit_exports, functions)` — checks every named export refers to a defined function or `"prompt"`
- `canonicalize_and_check(path)` — all security checks WITHOUT reading file; called on every resolve including cache hits
- `read_validated_file(canonical)` — reads bytes then checks size; called only on cache misses
- `attach_import_span(err, path, file_str, source, offset)` — re-annotates `FileNotFound` and `CircularImport` errors to point to the `@import` directive in the parent file

`process_module` itself is now a ~25-line orchestrator that calls these helpers in sequence.

**`ModuleCtx` struct** bundles the borrowed per-module context (`file_str`, `source`, `base_dir`, `runtime_vars`), reducing parameter lists.

**`CollectedDefs` struct**: Fields: `functions: HashMap<String, Arc<FunctionDef>>`, `has_explicit_exports: bool`, `explicit_exports: HashSet<String>`.

**`Arc<ResolvedModule>`**: `ModuleCache::modules` stores `Arc<ResolvedModule>`. Both `resolve()` and `resolve_source()` return `Arc<ResolvedModule>`. Cache hits clone the `Arc` (O(1)).

**`ResolvedModule`** fields (all `pub(crate)` — access via methods, not direct field access):
- `functions: HashMap<String, Arc<FunctionDef>>` — all `@define`d functions (including re-exports)
- `prompt_body: Option<String>` — rendered body text, or None if empty
- `raw_frontmatter: Option<String>` — **raw YAML text** between the `---` fences (excluding the fences); captured from `module.frontmatter.as_ref().map(|fm| fm.raw.clone())` during `process_module`; used by `lib.rs` to reconstruct the frontmatter block in compiled output
- `has_explicit_exports: bool` — true once any `@export` appears
- `explicit_exports: HashSet<String>` — the explicitly listed export names

**`ResolvedModule` methods**:
- `get_export(name)` → `Option<Arc<FunctionDef>>` — respects export visibility
- `get_all_exports()` → `Vec<(String, Arc<FunctionDef>)>` — all exported (name, Arc) pairs, filtered by explicit exports
- `get_prompt_value()` — returns `prompt_body` as `Value::String` if it is an available export; `None` otherwise
- `to_namespace()` — converts to `NamespaceScope`; respects export visibility for both `functions` and `prompt_body`

**`prompt` as an export**: Any module with a non-empty body implicitly exports it as `prompt`, unless the module has explicit exports and `"prompt"` is not listed. Importers can bring in the body text via `@import { prompt } from "./module.mds"` or merge import.

**Export validation**: After collecting all `@export` directives, the resolver checks every named export either refers to a defined function or is the string `"prompt"`. For re-exports (`@export name from "path"`), the source module is resolved first and `get_export(name)` is called.

**Import semantics**:
- **Alias** (`@import "path" as ns`): resolved module becomes a `NamespaceScope` under `ns`; functions accessed as `{ns.fn(arg)}`
- **Merge** (`@import "path"`): all exported functions brought into scope; frontmatter variables from the imported module are NOT brought in (only functions and `prompt` body)
- **Selective** (`@import { fn } from "path"`): only named exports brought in; `prompt` is handled specially (bound as a variable, not a function)

**Re-export semantics** (`@export name from "path"`, `@export * from "path"`): The source module is resolved and its exports are added to the current module's `functions` map. They are NOT added to the current file's runtime scope. If a named re-export target does not exist in the source module's exports, the error is raised at the re-export site.

**Closure capture**: When a `@define` node is processed, the resolver calls `FunctionDef::from(def)` (which creates empty captures), then fills `func.captured.namespaces`, `func.captured.functions`, and `func.captured.vars` from the current scope state. `captured.functions` is populated by converting `Arc<FunctionDef>` → owned `FunctionDef` (via `(*v).clone()`) to avoid reference cycles.

### Evaluator (`src/evaluator.rs`)

The evaluator walks the AST and produces the final rendered string. Its public entry point is `evaluate(nodes, scope, warnings)` — the `warnings: &mut Vec<String>` parameter is threaded through all internal helpers including `evaluate_include`. Nothing in the evaluator calls `eprintln!` directly.

**`EvalContext` struct** bundles three mutable state fields:

```rust
pub(crate) struct EvalContext<'a> {
    call_stack: Vec<String>,          // recursion detection (Vec, not HashSet)
    total_iterations: usize,          // cumulative @for iterations
    warnings: &'a mut Vec<String>,    // non-fatal diagnostics
}
```

**`resolve_dot_path(root: &str, fields: &[String], scope: &Scope) -> Result<Value, MdsError>`**: Private function that walks a dot-separated path. `root` is the name of the top-level scope variable; `fields` is the remaining path segments after the dot. Looks up `root` in `scope`, then traverses `Value::Object` fields for each element of `fields`. Returns a `MdsError::undefined_var` if the root is missing, or `MdsError::syntax` if a field is missing or an intermediate value is not an object.

`resolve_dot_path` is the single implementation shared across four use sites:
- `evaluate_expr(Expr::MemberAccess)` — `{config.key}` interpolation
- `evaluate_if` — `@if config.debug:` condition resolution
- `evaluate_for` — `@for item in data.list:` iterable resolution
- `resolve_args(Arg::MemberAccess)` — `greet(config.name)` argument resolution

**Object interpolation guard**: `Expr::Var` and `Expr::MemberAccess` both check for `Value::Object` and return a `MdsError::syntax` with a hint to access a specific field.

**Key-value `@for` iteration**: When `block.key_var` is `Some`, the evaluator matches the iterable against `Value::Object`, sorts keys alphabetically for deterministic output, and iterates over `(key, val)` pairs. Each iteration pushes a scope frame with `key_var → Value::String(key)` and `var → val`. When `key_var` is `None` and the iterable resolves to an object, the evaluator returns an error with a hint to use key-value syntax.

`call_stack` is `Vec<String>` (not `HashSet<String>`). Recursion detection uses `ctx.call_stack.iter().any(|s| s == call_key)` — O(n) scan at MAX_CALL_DEPTH=128, acceptable. The LIFO property is verified with a **structured error return** (not `assert!`) — a mismatch produces `MdsError::syntax`; `prefer_first_error` ensures the render error wins over the LIFO error in double-fault scenarios.

Five resource limits guard against runaway compilation:
- `MAX_CALL_DEPTH = 128` — prevents stack overflow from deeply nested function calls
- `MAX_LOOP_ITERATIONS = 100,000` — enforced per `@for` loop (applies to both arrays and key-value object iteration)
- `MAX_TOTAL_ITERATIONS = 1,000,000` — cumulative across all loops; tracked via `ctx.total_iterations`
- `MAX_OUTPUT_SIZE = 50 MB` — checked after each node renders
- `MAX_WARNINGS = 1,000` — once the warnings vec reaches this size, `evaluate_include` silently skips further pushes

All limits return `MdsError::ResourceLimit` (no source span). If you add a warning-emitting path or a new iterable node, pass `ctx` through so `total_iterations` and `warnings` are respected.

**`Node::Define` in the evaluator**: The evaluator's `Node::Define` arm is a deliberate no-op — all function registration happens in the resolver's pre-evaluation AST walk.

`invoke_function` restores the function's captured closure scope from `func.captured` before binding parameters, so params shadow captured vars correctly. The double-fault error-preservation `prefer_first_error` helper is used after each scope pop: render errors win over LIFO/pop failures.

**`resolve_args` signature**: `resolve_args(args: &[Arg], scope: &mut Scope, ctx: &mut EvalContext, depth: usize) -> Result<Vec<Value>, MdsError>`. The `Arg::Call` arm wraps the result in `Value::String` — nested call results are always strings. The `Arg::MemberAccess` arm calls `resolve_dot_path` and returns the raw `Value` — preserving the actual type of the field.

`@include alias` looks up the aliased module's `prompt_body` from the namespace and injects it inline. If the included module has no body text, `evaluate_include` pushes a warning to `ctx.warnings` and returns an empty string.

`@import` and `@export` nodes are no-ops in the evaluator (handled entirely by the resolver).

### Frontmatter Preservation (`src/lib.rs`)

Compiled output preserves the source file's YAML frontmatter. The pipeline steps for this are in `lib.rs`:

1. `ResolvedModule::raw_frontmatter: Option<String>` — captured during `process_module` from `module.frontmatter.raw`
2. `strip_type_mds(raw: &str) -> Option<String>` — filters out lines matching `type: mds` (a compiler-internal key, not user data); returns `None` if all remaining content is whitespace
3. `prepend_frontmatter(raw: Option<&str>, body: String) -> String` — if raw is `Some` and non-empty after stripping, prepends `---\n{cleaned_yaml}---\n` to the body; otherwise returns body unchanged

Both `compile_collecting_warnings` and `compile_str_collecting_warnings` call `prepend_frontmatter(resolved.raw_frontmatter.as_deref(), body)` as the final step before returning. This means output from a source with only `type: mds` in frontmatter (and nothing else useful) will have no frontmatter block in the output.

### Error System (`src/error.rs`)

**`#[non_exhaustive]`**: `MdsError` is marked `#[non_exhaustive]`. External crates cannot exhaustively match on it without a wildcard arm, allowing new variants to be added without a semver break. Within the crate, all match arms are exhaustive and do not need `_`.

**`pub(crate)` constructors**: All `MdsError` constructor methods are `pub(crate)`. They are not part of the public API — error construction is internal to the compiler.

**`_at` constructors**: Every major `MdsError` variant has a corresponding `_at` constructor that accepts `(file: &str, source: &str, offset: usize, len: usize)` and populates the `span` and `src` fields for miette rich diagnostics. Always prefer `_at` variants inside the validator and evaluator where source offsets are available from the AST nodes.

### CLI (`src/main.rs`)

The CLI logic has been extracted from `run()` into three dedicated functions:

- `run_build(input, output, out_dir, vars, set_vars, quiet)` — handles the complete build flow: auto-detect input, reject directory input, load config, resolve output path, compile, write output or print to stdout
- `run_check(input, vars, set_vars, quiet)` — handles the complete check flow
- `run_init(filename, force, quiet)` — creates a starter `.mds` file; rejects `..` components in the filename before writing

**Named constant — single source of truth**: `MAX_TRAVERSAL_DEPTH: usize = 256` is defined once in `src/resolver.rs` as `pub(crate)`, re-exported from `src/lib.rs` as `pub const MAX_TRAVERSAL_DEPTH`, and imported in `src/main.rs` via `use mds::MAX_TRAVERSAL_DEPTH`.

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
  → lib::clean_output()    → trimmed body string
  → lib::prepend_frontmatter()  → final output with optional YAML frontmatter header
```

**Frontmatter preservation flow**: After compilation, both `compile_collecting_warnings` and `compile_str_collecting_warnings` call `prepend_frontmatter(resolved.raw_frontmatter.as_deref(), body)`. The `strip_type_mds` step removes the `type: mds` directive (compiler-internal) before the YAML block is prepended. If stripping leaves only whitespace, no frontmatter block is emitted.

**Warning propagation**: the `warnings: &mut Vec<String>` vector is allocated in the public API function and passed all the way through `ModuleCache::resolve` → `process_module` → `evaluate` → inside evaluator as `ctx.warnings`. After the pipeline completes, the calling code decides whether to print them (via `emit_warnings`) or return them to the caller (via `compile_collecting_warnings`).

Runtime variables override frontmatter: in `build_scope_from_frontmatter`, frontmatter vars are loaded first, then runtime vars overwrite any key that appears in both.

The `ModuleCache` is created per top-level compile call (not shared across calls). Each entry in `modules` is an `Arc<ResolvedModule>` — cache hits clone the Arc (O(1)).

## Integration Patterns

### Adding a New Directive

1. Add a new variant to `Node` in `src/ast.rs` (and any needed sub-structs)
2. Lex: directives are already captured as `Token::Directive` — no lexer change required unless new syntax
3. Parse: add a branch in `Parser::parse_directive()` matching the `@name` prefix; validate identifier names with `is_valid_identifier()`
4. Validate: add a match arm in `validate_node()` — validate what the resolver can't catch
5. Resolve: if the directive requires file I/O (import-like), handle it in `collect_definitions_and_imports`; if it only builds scope, handle it in `build_scope_from_frontmatter` or a new helper
6. Evaluate: add a match arm in `evaluate_nodes()` — if the directive can emit warnings, accept and forward via `ctx.warnings`; `Import`/`Export` stay as no-ops there
7. Add integration test fixture in `tests/fixtures/` and a test in `tests/integration.rs`

### Adding a New Arg Variant

If you add a fifth `Arg` variant, update all three sites that match on `Arg`:
1. `parse_single_arg_inner` in `src/parser.rs` — construct the new variant
2. `resolve_args` in `src/evaluator.rs` — evaluate to a `Value`
3. `validate_var_args` in `src/validator.rs` — pre-evaluation validity check

Failing to update any one of these produces an incomplete `match` compilation error, which is intentional — `Arg` has no wildcard arm.

### Adding a New Value Type

Add the new variant to `Value` and update all internal match sites:
- `from_yaml` and `from_json` (both `pub(crate)`) — conversion from YAML/JSON
- `Display` — string rendering
- `is_truthy` — truthiness rule
- `type_name` — name used in error messages
- `as_array` — if the new type can act as an iterable
- Consider whether `resolve_dot_path` in `evaluator.rs` should handle field traversal into the new type

Because `Value` is `#[non_exhaustive]`, external code cannot match exhaustively, but all internal match arms must be updated. When writing tests for numeric values, avoid `3.14` (clippy `approx_constant` lint) — use values like `2.5` instead.

### Object/Map Access Patterns

Object values come from frontmatter YAML mappings or runtime vars JSON objects. Access rules:

- **Interpolation**: `{config.key}` — must access a leaf field; `{config}` alone is a runtime error
- **Condition**: `@if config.debug:` — evaluates truthiness of the resolved field value
- **Loop iterable**: `@for item in config.list:` — field must resolve to an array at runtime; static type check skipped for dot paths
- **Key-value iteration**: `@for key, value in config:` — iterates over object fields sorted alphabetically
- **Function argument**: `greet(config.name)` via `Arg::MemberAccess` — passes the raw `Value` to the function

The `resolve_dot_path` function is the single implementation shared across all these cases. Errors: `MdsError::undefined_var` for missing root, `MdsError::syntax` for missing field or non-object intermediate.

### Warning-Emitting Code

Any code that needs to emit a non-fatal diagnostic must accept `warnings: &mut Vec<String>` and push to it. Never call `eprintln!` from evaluator, resolver, or library code.

Inside the evaluator, warnings are accessed via `ctx.warnings`. When writing new evaluator helpers, accept `ctx: &mut EvalContext` rather than separate parameters for `call_stack`, `total_iterations`, and `warnings`.

The two-tier API pattern in `lib.rs`:
- `compile(path, vars)` — calls `compile_collecting_warnings` then `emit_warnings`
- `compile_collecting_warnings(path, vars)` — returns `(String, Vec<String>)` — use this when the caller needs to gate warning output

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

`exit_code` downcasts via `err.downcast_ref::<MdsError>()`. Errors created with `miette::miette!()` in `main.rs` do NOT downcast to `MdsError` and fall through to exit code 1. When adding new error categories, update `exit_code` first.

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

### Project Config (`mds.json`)

`load_config(start: &Path) -> Result<Option<(MdsConfig, PathBuf)>, miette::Error>` walks up the directory tree from the input file's parent, looking for `mds.json`. The walk is bounded by `MAX_TRAVERSAL_DEPTH = 256`. On finding the file: reads bytes first (TOCTOU-safe), checks size against `MAX_CONFIG_SIZE` (1 MB limit), deserializes with `serde_json`, and returns both the config and the **directory that contains `mds.json`** so `output_dir` values can be resolved relative to it, not the input file's directory.

## Constraints

- **Import paths must be relative** — `validate_import_path` rejects non-relative paths (must start with `./` or `../`) and null bytes. Runs before any filesystem access.
- **Symlinks rejected** — `check_symlink` detects symlinks in the final path component by comparing `canonical_parent.join(raw_filename)` vs the fully-resolved path.
- **Path traversal prevention** — resolved import paths must remain within the project root.
- **MAX_IMPORT_DEPTH = 64** — prevents stack overflow from deep chains; tracked via `IndexSet::len()` on the single `resolving` field.
- **MAX_FILE_SIZE = 10MB** — checked by reading bytes first, then comparing size (TOCTOU-safe).
- **MAX_CALL_DEPTH = 128** — prevents stack overflow from deeply nested function calls; tracked via `ctx.call_stack.len()`.
- **MAX_NESTING_DEPTH = 256** — `pub(crate)` constant in `src/parser.rs`; shared between: (1) parser-level block nesting (`@if`/`@for`/`@define`) via `enter_block()`, and (2) argument-level nested call depth validation in `validate_var_args`.
- **MAX_LOOP_ITERATIONS = 100,000** — per-loop hard cap in the evaluator; applies to both array and key-value object iteration.
- **MAX_TOTAL_ITERATIONS = 1,000,000** — cumulative cap across all loops in one compilation; tracked via `ctx.total_iterations`.
- **MAX_OUTPUT_SIZE = 50 MB** — evaluator checks output buffer size after each node.
- **MAX_VALUE_DEPTH = 64** — `Value::from_yaml` / `Value::from_json` reject YAML sequences/mappings or JSON arrays/objects nested deeper than 64 levels.
- **Object field access errors at runtime** — `resolve_dot_path` cannot validate field existence at compile time for dot paths whose type is determined at runtime. Missing fields produce `MdsError::syntax` at evaluation time without a source span.
- **`.md` files require `type: mds`** in frontmatter to be compiled — `validate_file_type` enforces this.
- **Recursion is detected at evaluation time** using `ctx.call_stack` — the validator cannot catch recursive call chains because they depend on runtime scope.
- **`Arg::Call` result is always String; `Arg::MemberAccess` result is the raw Value** — functions receiving a nested call argument always receive `Value::String`. Functions receiving a member access argument receive the field's actual runtime type.
- **MAX_TRAVERSAL_DEPTH = 256** — single definition in `src/resolver.rs`, re-exported as `pub const` via `src/lib.rs`; caps upward directory walks in both `load_config` (main.rs) and `find_project_root` (resolver.rs).
- **MAX_CONFIG_SIZE = 1MB** — `mds.json` files larger than 1MB are rejected by `load_config` before parsing; TOCTOU-safe (read first, then check size).
- **Directory input rejected** — `reject_directory_input()` in main.rs returns an error immediately if the input path is a directory.
- **`mds init` filename traversal rejected** — `run_init` rejects filenames containing `..` components before writing.

## Anti-Patterns

- **Calling `eprintln!` from evaluator or resolver code** — all non-fatal diagnostics must go through `ctx.warnings` (in the evaluator) or `warnings: &mut Vec<String>` (in the resolver). Direct stderr output bypasses the quiet flag and makes the warnings un-testable.
- **Calling `evaluate` before `validate`** — the evaluator trusts that all references exist; skipping validation will produce misleading errors at evaluation rather than rich span-aware diagnostics.
- **Resolving imports in the evaluator** — imports must be resolved before evaluation so scope is complete when `validate` runs.
- **Creating `ModuleCache` per-module instead of per-compile** — the cache is the only thing preventing re-parsing the same file dozens of times.
- **Using bare `MdsError::syntax(msg)` when source context is available** — always prefer `syntax_at` when you have an offset and source string.
- **Directly interpolating a `Value::Object`** — `{obj}` where obj is an object is a runtime error; users must write `{obj.key}`. The evaluator guards against this in both `Expr::Var` and `Expr::MemberAccess`.
- **Using single-var `@for item in obj:` on an object** — fails at validate time for simple identifiers with a hint to use `@for key, value in obj:`. Dot-path iterables that resolve to an object fail at evaluation time.
- **Bypassing `prepend_frontmatter` / `strip_type_mds` for new output paths** — any new public compile function that emits output must call `prepend_frontmatter(resolved.raw_frontmatter.as_deref(), body)`. Calling `clean_output` and returning directly is a bug that drops frontmatter.
- **Expecting `Arg::MemberAccess` to always produce a String** — unlike `Arg::Call`, `Arg::MemberAccess` returns the raw `Value`. A function receiving a member access argument may receive `Value::Object`, `Value::Array`, etc.
- **Adding object/map support without updating all Value methods** — `from_yaml`, `from_json` (both `pub(crate)`), `Display`, `is_truthy`, `type_name`, and `as_array` must all be consistent.
- **Adding field traversal to non-Object values in `resolve_dot_path`** — currently only `Value::Object` supports traversal. Any extension requires updating error messages and all four `resolve_dot_path` call sites.
- **Forgetting to capture closure scope in new definition-like directives** — any directive that defines a callable entity should call `scope.get_all_namespaces()`, `scope.get_all_functions()`, and `scope.get_all_vars()` at definition time. Remember that `FunctionDef::from` always produces `captured: CapturedScope::default()` — the resolver must fill the captures.
- **Adding functions to scope without also capturing current scope into the FunctionDef** — previously captured siblings won't see the new function.
- **Adding a new `Arg` variant without updating all three match sites** — parser (`parse_single_arg_inner`), evaluator (`resolve_args`), and validator (`validate_var_args`) all pattern-match exhaustively on `Arg`.
- **Passing separate `call_stack`/`warnings` instead of `ctx` to evaluator helpers** — all evaluator internal functions now take `ctx: &mut EvalContext`.
- **Using `compile` instead of `compile_collecting_warnings` in CLI code** — the CLI must use the collecting variants to properly gate warning output on the `--quiet` flag.
- **Duplicating `base_dir` fallback logic in new string-based API functions** — always call `resolve_base_dir(base_dir)` rather than inlining the `current_dir()` fallback.
- **Calling `get_all_exports()` and expecting a `HashMap`** — `ResolvedModule::get_all_exports()` returns `Vec<(String, Arc<FunctionDef>)>`, not a `HashMap`.
- **Injecting `Value::Null` as a placeholder for `@define` params in validation** — the validator uses `Value::Array(vec![])` so `@for item in param:` inside a define body passes the array type check.
- **Ignoring the `Result` from `scope.pop()`** — `pop()` returns `Result<(), MdsError>` and errors when called on the global scope frame. Always use `scope.pop()?`. Exception: immediately after `scope.push()` in validator body recursion.
- **Re-exporting via `to_namespace()` without export-visibility check** — `to_namespace()` applies the same visibility rule as `get_prompt_value()`. Never bypass this method to build a `NamespaceScope` directly from `ResolvedModule` fields.
- **Accessing `func.captured_namespaces` / `func.captured_functions` / `func.captured_vars` directly** — these three separate fields no longer exist. Access via `func.captured.namespaces`, `func.captured.functions`, `func.captured.vars`.
- **Using `output: Option<PathBuf>` for the Build command's output field** — the field is `Option<String>` intentionally so the literal `"-"` can be compared before any path resolution occurs.
- **Resolving `mds.json`'s `output_dir` relative to the source file** — `output_dir` must be resolved relative to the directory that contains `mds.json` (the `config_dir` returned by `load_config`).
- **Using `metadata().len()` before `read()` for size checks** — both `load_config` and `read_validated_file` read bytes first, then check the length. Never split size check from read — that introduces a TOCTOU race.
- **Matching exhaustively on `MdsError` or `Value` in external code** — both enums are `#[non_exhaustive]`. External crates must include a wildcard arm when pattern-matching.
- **Adding run logic directly in `run()` instead of a dedicated `run_*` function** — `run()` is a pure dispatcher.

## Gotchas

- **`{obj}` where obj is an object is a runtime error** — direct object interpolation produces `MdsError::syntax` with a hint to use `{obj.key}`. Both `Expr::Var` and `Expr::MemberAccess` guard against objects at the terminal value.
- **`{namespace.varname}` where namespace is an imported module is a distinct error** — if the root of a `MemberAccess` is an imported namespace (not a variable), the evaluator returns a targeted error: "'{ns}' is an imported module, not a variable — to call a function use {ns.func()}". This is checked before `resolve_dot_path`.
- **Dot-path type errors at runtime for `@for`** — when the iterable is a dot path (e.g., `@for item in data.list:`), the validator cannot check the field type statically. If `data.list` resolves to a non-array, `type_error` surfaces at evaluation time without a source span.
- **Key-value iteration sorts keys alphabetically** — `@for key, value in obj:` always iterates in alphabetical key order for deterministic output. YAML key order is not preserved.
- **`Arg::MemberAccess` result type is not always String** — unlike `Arg::Call` (always `Value::String`), `Arg::MemberAccess` returns the raw `Value`. A function receiving a member access argument may receive `Value::Object`, `Value::Array`, etc.
- **`raw_frontmatter` is captured for all resolved modules, not just entry** — every resolved module stores its raw frontmatter. Only the entry module's frontmatter is prepended to output; imported modules' `raw_frontmatter` is not used by the compiler.
- **`strip_type_mds` only removes `type: mds` lines** — if future MDS-internal frontmatter keys are added, `strip_type_mds` must be extended to filter them from output.
- **`@define` body nodes have leading/trailing newlines stripped** — the parser calls `strip_leading_newline` and `strip_trailing_newline` on `@define` bodies. If you add a new block directive, apply the same stripping unless you want those newlines in output.
- **`@for` body validation uses a Null-injected push; `@define` body validation uses an Array-injected push** — the validator uses `Value::Null` for the loop variable but `Value::Array(vec![])` for `@define` parameters. This asymmetry exists because `@define` params might be used as iterables.
- **Runtime vars override frontmatter silently** — there is no warning when a runtime var shadows a frontmatter key.
- **`@export` changes all-implicit to explicit** — once any `@export` appears in a module, only explicitly listed names are exported.
- **`@export prompt` is valid** — the string `"prompt"` is a special case in export validation. It does not need a corresponding `@define prompt` — it refers to the module's rendered body.
- **`to_namespace()` respects export visibility for `prompt_body`** — `to_namespace()` only includes `prompt_body` in the `NamespaceScope` when `"prompt"` is an available export.
- **`@include` on an empty module pushes a warning and returns empty** — `evaluate_include` calls `ctx.warnings.push(...)` (not `eprintln!`) and returns `""`. Warnings are silently dropped once `ctx.warnings.len() >= MAX_WARNINGS`.
- **Merged imports bring in `prompt` body but not frontmatter vars** — `@import "path"` (merge) brings functions and the `prompt` body text into scope, but NOT the imported module's frontmatter variables.
- **Selective import of `prompt` binds as a variable, not a function** — `@import { prompt } from "path"` sets `prompt` as a `Value::String` via `scope.set_var`, not `scope.set_function`.
- **`compile_str` takes no arguments** — use `compile_str_with(source, base_dir, runtime_vars)` when you need import resolution relative to a specific directory.
- **`compile_file` takes no runtime vars** — call `compile` directly if runtime vars are needed.
- **Closure capture is eager and shallow** — `get_all_vars()` / `get_all_functions()` / `get_all_namespaces()` snapshot the scope at definition time. Functions defined after the closure capture are not visible to the captured function.
- **`get_all_functions()` returns `Arc<FunctionDef>`; captured.functions stores owned `FunctionDef`** — the resolver converts `Arc<FunctionDef>` → owned `FunctionDef` when populating captures. `invoke_function` then wraps each captured function back in `Arc::new(f.clone())` when restoring closure scope. This round-trip is intentional to break reference cycles.
- **`call_stack` is `Vec`, not `HashSet`** — recursion detection uses `ctx.call_stack.iter().any(|s| s == call_key)` (O(n) scan). At `MAX_CALL_DEPTH = 128`, this is negligible. The LIFO invariant is verified with a **structured error return** (not `assert!`) — a mismatch surfaces as `MdsError::syntax`; `prefer_first_error` ensures the render error takes precedence when both fail simultaneously. Same pattern applies to the `resolving` IndexSet in the resolver.
- **`IndexSet` replaces two resolver fields** — if you need to check "is this path currently being resolved?", use `self.resolving.contains(&canonical)`. If you need to reconstruct the cycle path, use `self.resolving.iter()`. There is no separate `resolving_stack` field.
- **`compile_str` / `resolve_source` uses a virtual path `<source>`** — repeated calls to `compile_str` re-parse every time; there is no caching for in-memory sources.
- **Project root is set on first resolve** — `root_dir` is set lazily. If `resolve_source` is called first, `root_dir` is set to the canonicalized `base_dir`.
- **Re-export errors are raised at the barrel module, not the consumer** — when `@export name from "path"` fails, the error surfaces when the barrel itself is compiled.
- **`--set KEY=VAL` last-write wins for duplicate keys** — later `--set` values overwrite earlier ones (HashMap semantics).
- **`MAX_NESTING_DEPTH` is `pub(crate)`, not `pub`** — elevated to `pub(crate)` so `src/validator.rs` can import it for its argument-depth guard.
- **`TextNode` has no offset** — raw text nodes do not carry a byte offset. Only structured nodes have offsets for error reporting.
- **`enter_block()` must be paired with `self.depth -= 1`** — the helper only increments; callers are responsible for decrementing after the block body is parsed.
- **Selective import `from` keyword requires a whitespace separator** — `parse_import_directive` accepts `from ` (space) or `from\t` (tab) but rejects `from"path"` with no gap.
- **String literal escapes are not full Rust/JSON escapes** — `unescape_string` in the parser only recognizes `\\`, `\"`, and `\'`. A backslash followed by any other character (e.g., `\n`, `\t`) is kept verbatim as both backslash and the following character.
- **`MdsError` itself is `#[must_use]` at the type level** — constructing a `MdsError` value without returning or using it produces a compiler warning. This guards against accidentally constructing an error and then silently discarding it.
- **`help(...)` attributes are variant-level, not constructor-level** — `CircularImport`, `Recursion`, and `TypeError` have `#[diagnostic(help(...))]` annotations that miette renders automatically. When adding new error variants, add the `help` attribute directly on the variant, not in the constructor method.
- **Exit code 2 covers three error types** — `MdsError::Io`, `MdsError::FileNotFound`, and `MdsError::NotMdsFile` all map to exit code 2. Other `MdsError` variants map to exit code 1.
- **`mds build` default is file output, not stdout** — existing scripts that pipe `mds build foo.mds` and expect stdout output must be updated to add `-o -`.
- **stdin input with `--out-dir` writes `output.md`, not `-.md`** — when input is stdin (`-`) and `--out-dir` is set, the output filename is hardcoded to `"output.md"` because deriving a name from the stdin path `-` would produce `-.md`.
- **`load_config` starts from the input file's parent, not CWD** — the walk begins at the directory containing the input file. The resulting `output_dir` is always resolved relative to the `mds.json` location.

## Key Files

- `src/lib.rs` — public API; `strip_type_mds` and `prepend_frontmatter` private helpers for frontmatter preservation; re-exports `MAX_FILE_SIZE` and `MAX_TRAVERSAL_DEPTH`; private `resolve_base_dir` helper; `compile_collecting_warnings` / `compile_str_collecting_warnings` are the canonical output assembly points
- `src/main.rs` — CLI entry point: `MdsConfig`/`BuildConfig` structs, `load_config` (TOCTOU-safe, bounded by `MAX_TRAVERSAL_DEPTH`), `resolve_output_path` (6-step precedence), `derive_output_filename`, `auto_detect_mds_file`, `parse_cli_value`, `build_runtime_vars`, `reject_directory_input`, `read_stdin`, `exit_code`; logic split into `run_build`/`run_check`/`run_init`
- `src/ast.rs` — all AST types: `Expr::MemberAccess` and `Arg::MemberAccess` for dot-notation; `IfBlock.condition: Vec<String>` and `ForBlock.iterable: Vec<String>` as dot-separated paths; `ForBlock.key_var: Option<String>` for key-value iteration; `Arg::Call` for nested function call arguments
- `src/lexer.rs` — `Lexer<'a>` struct with `scan_*` methods; public API is `tokenize(source, file)` only
- `src/parser.rs` — converts token stream to `Module` AST; `pub(crate) MAX_NESTING_DEPTH`; `enter_block()` helper; dot-path parsing for `@if`/`@for`; key-value `@for` parsing; `MemberAccess` interpolation disambiguation; `parse_args_inner`/`parse_single_arg_inner` depth-bounded recursion
- `src/resolver.rs` — orchestrator: `ModuleCache` with `Arc<ResolvedModule>` cache; `raw_frontmatter: Option<String>` field on `ResolvedModule`; `IndexSet<PathBuf>` for cycle detection; security checks; import dispatch split into dedicated helpers
- `src/evaluator.rs` — AST walker; `EvalContext<'a>` bundles `call_stack: Vec<String>`, `total_iterations`, `warnings`; `resolve_dot_path` private function for dot-path traversal; `Expr::MemberAccess` and `Arg::MemberAccess` evaluation; key-value `@for` iteration; `evaluate_include` pushes to `ctx.warnings`
- `src/validator.rs` — pre-evaluation semantic checks; `validate()` takes `&mut Scope`; `Arg::MemberAccess` validation (root only); key-value `@for` validation with `key_var` injection; static type check bypass for dot-path iterables; uses `crate::parser::MAX_NESTING_DEPTH` for depth guard
- `src/scope.rs` — `CapturedScope` struct bundling three closure capture maps; `FunctionDef.captured: CapturedScope`; `Frame::functions` and `NamespaceScope::functions` store `Arc<FunctionDef>`; `set_function` takes `Arc<FunctionDef>`; `get_function` returns `Option<&Arc<FunctionDef>>`
- `src/value.rs` — runtime value enum (`#[non_exhaustive]`); `Value::Object(HashMap<String, Value>)` variant with alphabetical-sort `Display`; `from_yaml`/`from_json` are `pub(crate)` and convert YAML mappings/JSON objects to `Value::Object`; `From<HashMap<String, Value>>` impl
- `src/error.rs` — `MdsError` enum (`#[non_exhaustive]`); all constructor methods are `pub(crate)`; all major variants have `_at` constructors; `ResourceLimit` variant for evaluator/value depth guards
- `tests/integration.rs` — end-to-end tests covering all features, error paths, CLI integration, and spec-compliance tests; includes object/map access, key-value iteration, dot-path conditions, frontmatter preservation, resource limit tests, directory rejection, path traversal rejection, and `mds.json` config tests

## Related

- `src/resolver.rs` — canonical reference for the module system, import semantics, security guards, `Arc<ResolvedModule>` cache, `raw_frontmatter` field, `IndexSet` cycle detection, and `ResolvedModule` export API
- `src/evaluator.rs` — canonical reference for `EvalContext` usage, `resolve_dot_path` implementation, directive execution order, closure restore, call-depth guards, key-value for-loop implementation, nested arg evaluation, and warning collection
- `src/scope.rs` — canonical reference for `CapturedScope` struct, `Arc<FunctionDef>` in frames, closure capture API (`get_all_*` methods), and shadowing semantics
- `src/ast.rs` — canonical reference for `Arg` variants, `Expr` variants, and dot-path representations in `IfBlock`/`ForBlock`; any new argument or expression form starts here
- `src/lib.rs` — canonical reference for the two-tier warning API, `compile_file` entry point, `resolve_base_dir` helper, `strip_type_mds`/`prepend_frontmatter` for frontmatter preservation, and `clean_output`
- `src/main.rs` — canonical reference for CLI auto-detection logic, `parse_cli_value` coercion rules, `exit_code` categorization, output destination resolution (`resolve_output_path`), project config loading (`load_config`), and run_build/run_check/run_init decomposition
- `src/error.rs` — canonical reference for `#[non_exhaustive]` on `MdsError`, `pub(crate)` constructor pattern, `help(...)` diagnostic attribute placement, and available `_at` constructors
- `src/value.rs` — canonical reference for `#[non_exhaustive]` on `Value`, `pub(crate)` converters, `Value::Object` semantics, and the JSON/YAML parsing boundary
- `tests/integration.rs` — covers all directive combinations including object access, key-value iteration, dot-path conditions, frontmatter preservation, nested function calls, CLI stdin/quiet mode, auto-detect, error help-text, scope/export visibility rules, re-export error scenarios, default file output, `--out-dir`, `mds.json` config behavior, and all resource limit scenarios
